use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
struct BlockingTools {
    first: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl DesktopTools for BlockingTools {
    fn tools(&self) -> Vec<ToolDescription> {
        vec![ToolDescription {
            name: "probe".into(),
            input_schema: json!({"type":"object","additionalProperties":false}),
            ..ToolDescription::default()
        }]
    }

    fn invoke<'a>(&'a self, _: &'a str, _: Value) -> CallFuture<'a> {
        Box::pin(async move {
            if !self.first.swap(true, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            ToolOutput {
                success: true,
                content: vec![],
                data: None,
            }
        })
    }
}

fn fixture() -> (Arc<BlockingTools>, Arc<CuaService>) {
    let backend = Arc::new(BlockingTools::default());
    let service = Arc::new(CuaService::new(backend.clone()));
    (backend, service)
}

fn start(service: &Arc<CuaService>) -> tokio::task::JoinHandle<Result<ToolOutput, CuaError>> {
    let service = service.clone();
    tokio::spawn(async move { service.call("probe".into(), json!({})).await })
}

async fn slot_releases(service: &Arc<CuaService>) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match service.call("probe".into(), json!({})).await {
                Ok(_) => break,
                Err(CuaError::Busy) => tokio::task::yield_now().await,
                other => panic!("unexpected result: {other:?}"),
            }
        }
    })
    .await
    .expect("completed action must release its slot");
}

#[tokio::test]
async fn concurrent_call_is_busy_until_actual_completion() {
    let (backend, service) = fixture();
    let first = start(&service);
    backend.entered.notified().await;
    assert!(matches!(
        service.call("probe".into(), json!({})).await,
        Err(CuaError::Busy)
    ));
    backend.release.notify_one();
    assert!(first.await.unwrap().unwrap().success);
    assert!(
        service
            .call("probe".into(), json!({}))
            .await
            .unwrap()
            .success
    );
}

#[tokio::test(start_paused = true)]
async fn timeout_detaches_execution_and_retains_slot_until_completion() {
    let (backend, service) = fixture();
    let first = start(&service);
    backend.entered.notified().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(matches!(first.await.unwrap(), Err(CuaError::Timeout)));
    assert!(matches!(
        service.call("probe".into(), json!({})).await,
        Err(CuaError::Busy)
    ));
    backend.release.notify_one();
    slot_releases(&service).await;
}

#[tokio::test]
async fn disconnect_does_not_cancel_execution_or_release_slot() {
    let (backend, service) = fixture();
    let first = start(&service);
    backend.entered.notified().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(matches!(
        service.call("probe".into(), json!({})).await,
        Err(CuaError::Busy)
    ));
    backend.release.notify_one();
    slot_releases(&service).await;
}

#[tokio::test]
async fn invalid_and_unknown_calls_do_not_admit_execution_or_create_labels() {
    let (backend, service) = fixture();
    assert!(matches!(
        service
            .call("arbitrary-sensitive-name".into(), json!({}))
            .await,
        Err(CuaError::UnknownTool)
    ));
    assert!(matches!(
        service.call("probe".into(), json!({"extra":true})).await,
        Err(CuaError::InvalidArguments)
    ));
    assert!(matches!(
        service.call("probe".into(), Value::Null).await,
        Err(CuaError::InvalidArguments)
    ));
    assert!(!backend.first.load(Ordering::SeqCst));
    assert!(!service.metrics().contains("arbitrary-sensitive-name"));
}

struct SchemaTools {
    schema: Value,
    panic: bool,
    success: bool,
}

impl DesktopTools for SchemaTools {
    fn tools(&self) -> Vec<ToolDescription> {
        vec![ToolDescription {
            name: "schema".into(),
            input_schema: self.schema.clone(),
            ..ToolDescription::default()
        }]
    }

    fn invoke<'a>(&'a self, _: &'a str, _: Value) -> CallFuture<'a> {
        assert!(!self.panic, "isolated invocation panic");
        Box::pin(async move {
            ToolOutput {
                success: self.success,
                content: vec![],
                data: None,
            }
        })
    }
}

#[tokio::test]
async fn full_json_schema_validation_applies_before_execution() {
    let service = Arc::new(CuaService::new(Arc::new(SchemaTools {
        schema: json!({
            "$defs": {"count":{"type":"integer","minimum":1,"maximum":3}},
            "type":"object",
            "properties": {
                "count":{"$ref":"#/$defs/count"},
                "mode":{"enum":["one","two"]},
                "items":{"type":"array","items":{"type":"string"},"minItems":1,"uniqueItems":true}
            },
            "required":["count","mode","items"],
            "additionalProperties":false
        }),
        panic: false,
        success: true,
    })));
    for args in [
        json!({"count":0,"mode":"one","items":["a"]}),
        json!({"count":1.5,"mode":"one","items":["a"]}),
        json!({"count":1,"mode":"three","items":["a"]}),
        json!({"count":1,"mode":"one","items":[1]}),
        json!({"count":1,"mode":"one","items":["a","a"]}),
        json!({"count":1,"mode":"one","items":[]}),
    ] {
        assert!(matches!(
            service.call("schema".into(), args).await,
            Err(CuaError::InvalidArguments)
        ));
    }
    assert!(
        service
            .call(
                "schema".into(),
                json!({"count":2,"mode":"two","items":["a"]})
            )
            .await
            .unwrap()
            .success
    );
}

#[tokio::test]
async fn invocation_panic_is_internal_and_releases_slot() {
    let service = Arc::new(CuaService::new(Arc::new(SchemaTools {
        schema: json!({"type":"object"}),
        panic: true,
        success: true,
    })));
    for _ in 0..2 {
        assert!(matches!(
            service.call("schema".into(), json!({})).await,
            Err(CuaError::Internal)
        ));
    }
    assert!(service
        .metrics()
        .contains("tool=\"schema\",status=\"internal\"} 2"));
}

#[tokio::test]
async fn tool_failure_is_a_result_and_metrics_record_completion() {
    let service = Arc::new(CuaService::new(Arc::new(SchemaTools {
        schema: json!({"type":"object"}),
        panic: false,
        success: false,
    })));
    assert!(
        !service
            .call("schema".into(), json!({}))
            .await
            .unwrap()
            .success
    );
    assert!(service
        .metrics()
        .contains("tool=\"schema\",status=\"error\"} 1"));
    assert!(service
        .metrics()
        .contains("coorworker_envd_cua_tool_latency_seconds_count 1"));
}

#[tokio::test]
async fn unavailable_service_and_invalid_schema_fail_closed() {
    let invalid = CuaService::new(Arc::new(SchemaTools {
        schema: json!({"type":"not-a-json-schema-type"}),
        panic: false,
        success: true,
    }));
    for service in [
        invalid,
        CuaService::unavailable(""),
        CuaService::for_context(DesktopContext::Unavailable),
    ] {
        assert!(!service.available());
        assert!(!service.reason().unwrap().is_empty());
        assert!(matches!(service.tools(), Err(CuaError::Unavailable)));
        assert!(matches!(
            Arc::new(service).call("schema".into(), json!({})).await,
            Err(CuaError::Unavailable)
        ));
    }
}

#[test]
fn metric_labels_are_bounded_sanitized_and_unique() {
    let mut descriptions: Vec<_> = ["probe\"", "probe__0", "probe\n", "雪", "probe"]
        .into_iter()
        .map(|name| ToolDescription {
            name: name.into(),
            ..ToolDescription::default()
        })
        .collect();
    descriptions.extend((0..MAX_METRIC_TOOLS).map(|index| ToolDescription {
        name: format!("tool_{index}"),
        ..ToolDescription::default()
    }));
    let labels = metric_names(&descriptions);
    assert_eq!(labels.len(), MAX_METRIC_TOOLS);
    let unique: std::collections::HashSet<_> = labels.values().collect();
    assert_eq!(unique.len(), labels.len());
    assert_eq!(labels["probe"], "probe");
    assert!(labels.values().all(|label| label
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))));
}

#[test]
fn removed_environment_names_are_rejected_without_exposing_values() {
    for name in [
        "COORWORKER_CUA_MODE",
        "COORWORKER_CUA_FUTURE",
        "CUA_ENV_TOKEN",
    ] {
        let err = validate_environment_names([std::ffi::OsString::from(name)]).unwrap_err();
        assert!(err.to_string().contains(name));
        assert!(err.to_string().contains("remove"));
        assert_eq!(
            format!("{err:?}"),
            err.to_string(),
            "main's Result termination uses Debug; retain the migration instructions"
        );
    }
    assert!(validate_environment_names([std::ffi::OsString::from("X_ACCESS_TOKEN")]).is_ok());
}
