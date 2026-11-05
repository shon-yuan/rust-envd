//! Native CUA execution, independent of HTTP and desktop platform dependencies.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::FutureExt;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Semaphore;

#[cfg(all(feature = "cua-desktop", any(target_os = "linux", windows)))]
mod desktop;
#[cfg(test)]
mod tests;

const CALL_DEADLINE: Duration = Duration::from_secs(30);
const MAX_METRIC_TOOLS: usize = 1024;
const MAX_METRIC_NAME: usize = 64;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolDescription {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolOutput {
    pub success: bool,
    pub content: Vec<Value>,
    pub data: Option<Value>,
}

pub type CallFuture<'a> = Pin<Box<dyn Future<Output = ToolOutput> + Send + 'a>>;

/// In-memory test seam and native desktop execution boundary.
pub trait DesktopTools: Send + Sync + 'static {
    fn tools(&self) -> Vec<ToolDescription>;
    fn invoke<'a>(&'a self, name: &'a str, args: Value) -> CallFuture<'a>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CuaError {
    #[error("unknown CUA tool")]
    UnknownTool,
    #[error("invalid CUA tool arguments")]
    InvalidArguments,
    #[error("CUA execution is busy")]
    Busy,
    #[error("CUA runtime is unavailable")]
    Unavailable,
    #[error("CUA execution deadline exceeded; the action may still be running")]
    Timeout,
    #[error("internal CUA execution failure")]
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopContext {
    DesktopSession,
    Unavailable,
}

impl DesktopContext {
    /// Pure selection: service attachment, Session 0, and unknown sessions fail closed.
    pub fn for_windows(service_attached: bool, session: Option<u32>) -> Self {
        match (service_attached, session) {
            (false, Some(id)) if id != 0 => Self::DesktopSession,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Default)]
struct ToolMetrics {
    ok: AtomicU64,
    error: AtomicU64,
    internal: AtomicU64,
}

/// A single execution slot survives request cancellation and the HTTP deadline.
/// Descriptions and validators are cached; readiness and metrics never invoke tools.
pub struct CuaService {
    backend: Option<Arc<dyn DesktopTools>>,
    descriptions: Vec<ToolDescription>,
    validators: HashMap<String, jsonschema::Validator>,
    unavailable_reason: Option<String>,
    slot: Arc<Semaphore>,
    metric_names: HashMap<String, String>,
    tool_metrics: BTreeMap<String, ToolMetrics>,
    latency_nanos: AtomicU64,
    completed: AtomicU64,
    timeouts: AtomicU64,
}

impl CuaService {
    pub fn new(backend: Arc<dyn DesktopTools>) -> Self {
        let descriptions = backend.tools();
        let mut validators = HashMap::new();
        for description in &descriptions {
            if description.name.is_empty() || validators.contains_key(&description.name) {
                return Self::unavailable("desktop registry has invalid or duplicate tool names");
            }
            let Ok(validator) = jsonschema::validator_for(&description.input_schema) else {
                return Self::unavailable("desktop registry advertises an invalid input schema");
            };
            validators.insert(description.name.clone(), validator);
        }
        let metric_names = metric_names(&descriptions);
        let tool_metrics = metric_names
            .values()
            .map(|name| (name.clone(), ToolMetrics::default()))
            .collect();
        Self {
            backend: Some(backend),
            descriptions,
            validators,
            unavailable_reason: None,
            slot: Arc::new(Semaphore::new(1)),
            metric_names,
            tool_metrics,
            latency_nanos: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            backend: None,
            descriptions: Vec::new(),
            validators: HashMap::new(),
            unavailable_reason: Some(if reason.is_empty() {
                "desktop runtime unavailable".into()
            } else {
                reason
            }),
            slot: Arc::new(Semaphore::new(1)),
            metric_names: HashMap::new(),
            tool_metrics: BTreeMap::new(),
            latency_nanos: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
        }
    }

    pub fn for_context(context: DesktopContext) -> Self {
        if context != DesktopContext::DesktopSession {
            return Self::unavailable("CUA requires a known desktop user session");
        }
        #[cfg(all(feature = "cua-desktop", any(target_os = "linux", windows)))]
        {
            match desktop::DesktopRegistry::new() {
                Ok(registry) => Self::new(Arc::new(registry)),
                Err(_) => Self::unavailable("desktop registry initialization failed"),
            }
        }
        #[cfg(not(all(feature = "cua-desktop", any(target_os = "linux", windows))))]
        Self::unavailable("this build does not include desktop CUA support")
    }

    pub fn available(&self) -> bool {
        self.backend.is_some()
    }

    pub fn reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }

    pub fn tools(&self) -> Result<Vec<ToolDescription>, CuaError> {
        if !self.available() {
            return Err(CuaError::Unavailable);
        }
        Ok(self.descriptions.clone())
    }

    pub async fn call(self: &Arc<Self>, name: String, args: Value) -> Result<ToolOutput, CuaError> {
        let backend = self.backend.clone().ok_or(CuaError::Unavailable)?;
        let validator = self.validators.get(&name).ok_or(CuaError::UnknownTool)?;
        if !args.is_object() || !validator.is_valid(&args) {
            return Err(CuaError::InvalidArguments);
        }
        let permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| CuaError::Busy)?;
        let service = self.clone();
        // Dropping a JoinHandle detaches it. Neither an HTTP disconnect nor the
        // deadline drops the underlying invocation or its admission permit.
        let task = tokio::spawn(async move {
            let _permit = permit;
            let started = tokio::time::Instant::now();
            let result = AssertUnwindSafe(async { backend.invoke(&name, args).await })
                .catch_unwind()
                .await;
            service.record(&name, &result, started.elapsed());
            result.map_err(|_| CuaError::Internal)
        });
        match tokio::time::timeout(CALL_DEADLINE, task).await {
            Ok(result) => result.map_err(|_| CuaError::Internal)?,
            Err(_) => {
                self.timeouts.fetch_add(1, Ordering::Relaxed);
                Err(CuaError::Timeout)
            }
        }
    }

    fn record(&self, name: &str, result: &std::thread::Result<ToolOutput>, elapsed: Duration) {
        if let Some(metrics) = self
            .metric_names
            .get(name)
            .and_then(|name| self.tool_metrics.get(name))
        {
            let count = match result {
                Ok(output) if output.success => &metrics.ok,
                Ok(_) => &metrics.error,
                Err(_) => &metrics.internal,
            };
            count.fetch_add(1, Ordering::Relaxed);
        }
        let nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut current = self.latency_nanos.load(Ordering::Relaxed);
        loop {
            match self.latency_nanos.compare_exchange_weak(
                current,
                current.saturating_add(nanos),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        self.completed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn metrics(&self) -> String {
        let mut output = String::from("# TYPE coorworker_envd_cua_tool_calls_total counter\n");
        for (name, metrics) in &self.tool_metrics {
            for (status, count) in [
                ("ok", &metrics.ok),
                ("error", &metrics.error),
                ("internal", &metrics.internal),
            ] {
                let _ = writeln!(output, "coorworker_envd_cua_tool_calls_total{{tool=\"{name}\",status=\"{status}\"}} {}", count.load(Ordering::Relaxed));
            }
        }
        let _ = writeln!(
            output,
            "# TYPE coorworker_envd_cua_tool_latency_seconds summary"
        );
        let _ = writeln!(
            output,
            "coorworker_envd_cua_tool_latency_seconds_sum {}",
            self.latency_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0
        );
        let _ = writeln!(
            output,
            "coorworker_envd_cua_tool_latency_seconds_count {}",
            self.completed.load(Ordering::Relaxed)
        );
        let _ = writeln!(output, "# TYPE coorworker_envd_cua_timeouts_total counter");
        let _ = writeln!(
            output,
            "coorworker_envd_cua_timeouts_total {}",
            self.timeouts.load(Ordering::Relaxed)
        );
        let _ = writeln!(output, "# TYPE coorworker_envd_cua_ready gauge");
        let _ = writeln!(
            output,
            "coorworker_envd_cua_ready {}",
            u8::from(self.available())
        );
        output
    }
}

fn metric_names(descriptions: &[ToolDescription]) -> HashMap<String, String> {
    // Only names registered at startup can enter the fixed-size label set.
    let mut used = std::collections::HashSet::new();
    descriptions
        .iter()
        .take(MAX_METRIC_TOOLS)
        .enumerate()
        .map(|(index, tool)| {
            let sanitized: String = tool
                .name
                .chars()
                .take(MAX_METRIC_NAME)
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                        ch
                    } else {
                        '_'
                    }
                })
                .collect();
            let mut label = if sanitized == tool.name {
                sanitized
            } else {
                format!("{sanitized}_{index}")
            };
            while !used.insert(label.clone()) {
                label.push('_');
            }
            (tool.name.clone(), label)
        })
        .collect()
}

#[derive(thiserror::Error)]
#[error("removed CUA environment variable {name}: remove it and use envd initialization and X-Access-Token")]
pub struct EnvironmentError {
    name: String,
}

impl std::fmt::Debug for EnvironmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

/// Reject removed configuration even when its value is empty or non-Unicode.
/// Values, including the obsolete credential, are never included in errors.
pub fn validate_environment() -> Result<(), EnvironmentError> {
    validate_environment_names(std::env::vars_os().map(|(name, _)| name))
}

fn validate_environment_names(
    names: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(), EnvironmentError> {
    for name in names {
        let name = name.to_string_lossy();
        if name.starts_with("COORWORKER_CUA_") || name == "CUA_ENV_TOKEN" {
            return Err(EnvironmentError {
                name: name.into_owned(),
            });
        }
    }
    Ok(())
}
