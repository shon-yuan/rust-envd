//! Structured log production and the MMDS logs exporter.
pub mod exporter;
pub mod gojson;
pub mod logger;
pub mod process;
pub mod ratelimit;

pub use exporter::{ExporterTasks, HttpExporter};
pub use gojson::{decorate, JsonError};
pub use logger::{Level, Logger, StdoutSink};
pub use process::ProcessLogger;
pub use ratelimit::{Limiter, RateLimitedLogger};

use std::sync::atomic::{AtomicI32, Ordering};

/// Destination for one encoded log line, matching the `io.Writer` the Go
/// exporter is handed by zerolog (`main.go:203-208`). Implementations must
/// accept a line without blocking on network I/O.
pub trait LogSink: Send + Sync {
    fn write_line(&self, line: &[u8]);
}

static OPERATION_ID: AtomicI32 = AtomicI32::new(0);

/// Go `AssignOperationID` (internal/logs/interceptor.go:25-27): a process-wide
/// Int32 increment, formatted as decimal.
pub fn assign_operation_id() -> String {
    let id = OPERATION_ID.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    id.to_string()
}

/// Go `formatMethod` (interceptor.go:31-49): "POST /service.Service/Method"
/// becomes "Service method".
pub fn format_method(method: &str) -> String {
    let parts: Vec<&str> = method.split('.').collect();
    if parts.len() < 2 {
        return method.to_owned();
    }
    let split: Vec<&str> = parts[1].split('/').collect();
    if split.len() < 2 {
        return method.to_owned();
    }
    format!("{} {}", upper_first(split[0]), lower_first(split[1]))
}

fn upper_first(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => String::new(),
    }
}

fn lower_first(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_ascii_lowercase().to_string() + characters.as_str(),
        None => String::new(),
    }
}
