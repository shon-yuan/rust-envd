//! Adapter for the vendored registry. This module is never loaded by base builds.

use cua_driver_core::{
    protocol::{Content, ToolResult},
    tool::ToolRegistry,
};
use serde_json::{json, Value};

use super::{CallFuture, CuaError, DesktopTools, ToolDescription, ToolOutput};

pub(super) struct DesktopRegistry {
    registry: ToolRegistry,
    descriptions: Vec<ToolDescription>,
}

impl DesktopRegistry {
    pub(super) fn new() -> Result<Self, CuaError> {
        #[cfg(target_os = "linux")]
        let registry = platform_linux::register_tools();
        #[cfg(windows)]
        let registry = platform_windows::register_tools();
        Self::from_registry(registry)
    }

    fn from_registry(registry: ToolRegistry) -> Result<Self, CuaError> {
        // Use the public projection for listability, session input schema additions
        // and description hints. Output metadata is specific to vendor normalization.
        let listing = registry.tools_list();
        let entries = listing
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(CuaError::Internal)?;
        let descriptions = entries
            .iter()
            .map(description)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            registry,
            descriptions,
        })
    }
}

impl DesktopTools for DesktopRegistry {
    fn tools(&self) -> Vec<ToolDescription> {
        self.descriptions.clone()
    }

    fn invoke<'a>(&'a self, name: &'a str, args: Value) -> CallFuture<'a> {
        Box::pin(async move {
            let scope = cua_driver_core::work_scope::InvocationScope::new();
            output(scope.run(self.registry.invoke(name, args)).await)
        })
    }
}

fn description(entry: &Value) -> Result<ToolDescription, CuaError> {
    let text = |key| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(CuaError::Internal)
    };
    let hint = |key| {
        entry
            .get("annotations")
            .and_then(|value| value.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Ok(ToolDescription {
        name: text("name")?,
        description: text("description")?,
        input_schema: entry
            .get("inputSchema")
            .cloned()
            .ok_or(CuaError::Internal)?,
        read_only: hint("readOnlyHint"),
        destructive: hint("destructiveHint"),
        idempotent: hint("idempotentHint"),
        open_world: hint("openWorldHint"),
        capabilities: entry.get("capabilities").cloned(),
        risk: entry.get("risk").cloned(),
    })
}

fn output(result: ToolResult) -> ToolOutput {
    let success = !result.is_error.unwrap_or(false);
    ToolOutput {
        success,
        content: result.content.into_iter().map(content).collect(),
        data: result
            .structured_content
            .or_else(|| (!success).then(|| json!({"code":"tool_error"}))),
    }
}

fn content(content: Content) -> Value {
    let (mut value, annotations) = match content {
        Content::Text { text, annotations } => (json!({"type":"text", "text":text}), annotations),
        Content::Image {
            data,
            mime_type,
            annotations,
        } => (
            json!({"type":"image", "data":data, "mime_type":mime_type}),
            annotations,
        ),
    };
    if let Some(annotations) = annotations {
        value["annotations"] = annotations;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_preserves_advertised_schema_and_hints() {
        let schema = json!({"type":"object", "properties":{"session":{"type":"string"}}, "additionalProperties":false});
        let tool = description(&json!({
            "name":"fixture", "description":"projected hint", "inputSchema":schema,
            "outputSchema":{"type":"object"},
            "annotations":{"readOnlyHint":true,"destructiveHint":true,"idempotentHint":true,"openWorldHint":true},
            "capabilities":["fixture"], "risk":{"level":"fixture"}
        })).unwrap();
        assert_eq!(tool.input_schema, schema);
        assert!(tool.read_only && tool.destructive && tool.idempotent && tool.open_world);
        let native = serde_json::to_value(tool).unwrap();
        assert!(native.get("inputSchema").is_none());
        // Vendor output schemas describe normalized protocol results; native data
        // preserves raw diagnostics and must not advertise that stronger promise.
        assert!(native.get("output_schema").is_none());
        assert!(native.get("outputSchema").is_none());
    }

    #[test]
    fn vendor_results_use_native_content_and_failure_fields() {
        let native = output(ToolResult {
            content: vec![Content::text("refused"), Content::image_png("AA==".into())],
            is_error: Some(true),
            structured_content: Some(json!({"fixture":true})),
            ..ToolResult::default()
        });
        let native = serde_json::to_value(native).unwrap();
        assert_eq!(native["success"], false);
        assert_eq!(
            native["content"][1],
            json!({"type":"image","data":"AA==","mime_type":"image/png"})
        );
        assert_eq!(native["data"], json!({"fixture":true}));
        assert!(native.get("isError").is_none());
        assert!(native.get("structuredContent").is_none());
    }

    #[test]
    fn failed_vendor_results_without_data_get_stable_native_error_code() {
        for text in ["Hyprland IPC unavailable", "unrelated failure"] {
            let native = output(ToolResult::error(text));
            assert!(!native.success);
            assert_eq!(native.data, Some(json!({"code":"tool_error"})));
            assert_eq!(native.content, vec![json!({"type":"text","text":text})]);
        }
    }

    #[test]
    fn native_error_fallback_preserves_supplied_data_and_successful_absence() {
        for data in [json!({"code":"specific_failure","detail":7}), Value::Null] {
            let native = output(ToolResult::error("fixture").with_structured(data.clone()));
            assert_eq!(native.data, Some(data));
        }
        for is_error in [None, Some(false)] {
            let native = output(ToolResult {
                is_error,
                ..ToolResult::default()
            });
            assert!(native.success);
            assert_eq!(native.data, None);
        }
    }

    struct RegistryProbe {
        definition: cua_driver_core::tool::ToolDef,
        release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        returned: std::sync::Arc<tokio::sync::Notify>,
    }

    impl cua_driver_core::tool::Tool for RegistryProbe {
        fn def(&self) -> &cua_driver_core::tool::ToolDef {
            &self.definition
        }

        fn invoke<'a, 'call>(
            &'a self,
            _: Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + 'call>>
        where
            'a: 'call,
            Self: 'call,
        {
            Box::pin(async move {
                let release = self.release.lock().unwrap().take().unwrap();
                drop(cua_driver_core::work_scope::spawn_blocking(move || {
                    let _ = release.recv();
                }));
                self.returned.notify_one();
                ToolResult::text("isolated size")
                    .with_structured(json!({"width":800,"height":600,"scale_factor":1.0}))
            })
        }
    }

    #[tokio::test]
    async fn actual_registry_adapter_drains_detached_work_before_projecting_result() {
        let (release, receiver) = std::sync::mpsc::channel();
        let returned = std::sync::Arc::new(tokio::sync::Notify::new());
        let probe = RegistryProbe {
            definition: cua_driver_core::tool::ToolDef {
                name: "get_screen_size".into(),
                description: "in-memory fixture".into(),
                input_schema: json!({"type":"object","additionalProperties":false}),
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
            release: std::sync::Mutex::new(Some(receiver)),
            returned: returned.clone(),
        };
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(probe));
        let adapter = DesktopRegistry::from_registry(registry).unwrap();
        assert_eq!(adapter.tools()[0].name, "get_screen_size");
        let request =
            tokio::spawn(async move { adapter.invoke("get_screen_size", json!({})).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), returned.notified())
            .await
            .expect("fixture registry must dispatch without desktop access");
        assert!(
            !request.is_finished(),
            "adapter must await real worker completion"
        );
        release.send(()).unwrap();
        let result = request.await.unwrap();
        assert!(result.success, "{result:?}");
        assert_eq!(
            result.data,
            Some(json!({"width":800,"height":600,"scale_factor":1.0}))
        );
    }

    #[test]
    fn malformed_vendor_projection_fails_closed() {
        assert!(matches!(
            description(&json!({"name":"fixture"})),
            Err(CuaError::Internal)
        ));
    }
}
