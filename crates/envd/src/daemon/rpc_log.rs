//! Interceptor-line field order shared by both RPC loggers.
//!
//! Oracle: internal/logs/interceptor.go at
//! 51083a83cc0903185d992725ea6bb33683814b29 — `getErrDebugLogEvent` selects the
//! level and puts the `error` field first (`logger.Err(err)`), then `method`,
//! `operation_id`, and `error_code` when the call failed.

use std::fmt;

use crate::logs::Logger;
use connectrpc::ConnectError;

/// The error a stream end line reports. Go handlers return either a coded
/// connect error or a bare context error (watch.go:88-89 `return ctx.Err()`);
/// `connect.CodeOf` only unwraps `*connect.Error` (connect@v1.18.1
/// code.go:221-226), so the bare context.Canceled logs as CodeUnknown — never
/// CodeCanceled.
pub(crate) enum StreamEndError {
    Connect(ConnectError),
    ContextCanceled,
}

impl StreamEndError {
    fn code(&self) -> i64 {
        match self {
            Self::Connect(error) => i64::from(error.code.grpc_code()),
            Self::ContextCanceled => 2, // connectrpc CodeUnknown
        }
    }
}

impl fmt::Display for StreamEndError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(error) => error.fmt(f),
            // context.Canceled.Error()
            Self::ContextCanceled => f.write_str("context canceled"),
        }
    }
}

/// The event prefix every interceptor line shares: level by outcome, the
/// `error` field first (interceptor.go:63-64), then `method`,
/// `operation_id`, and `error_code` on failure (interceptor.go:69-71).
/// Streams log Debug on success — `getErrDebugLogEvent`
/// (interceptor.go:166-172) drives the stream helpers' Debug level.
pub(crate) fn event<'a>(
    logger: &'a Logger,
    procedure: &str,
    operation_id: &str,
    error: Option<&StreamEndError>,
) -> crate::logs::logger::Event<'a> {
    let mut line = if error.is_some() {
        logger.error()
    } else {
        logger.debug()
    };
    if let Some(error) = error {
        line = line.err(error);
    }
    line = line
        .str("method", format!("POST {procedure}"))
        .str("operation_id", operation_id);
    if let Some(error) = error {
        line = line.int("error_code", error.code());
    }
    line
}

/// The unary interceptor's prefix. Success is INFO, not Debug: the unary
/// wrapper logs `logger.Err(err)` (interceptor.go:63-64), and zerolog's
/// `Logger.Err` maps a nil error to `Info()` (zerolog v1.35.1 log.go:373-382)
/// — the failure branch keeps the Error level with the `error` field first.
pub(crate) fn unary_event<'a>(
    logger: &'a Logger,
    procedure: &str,
    operation_id: &str,
    error: Option<&StreamEndError>,
) -> crate::logs::logger::Event<'a> {
    let mut line = if error.is_some() {
        logger.error()
    } else {
        logger.info()
    };
    if let Some(error) = error {
        line = line.err(error);
    }
    line = line
        .str("method", format!("POST {procedure}"))
        .str("operation_id", operation_id);
    if let Some(error) = error {
        line = line.int("error_code", error.code());
    }
    line
}
