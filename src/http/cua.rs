//! Native CUA handlers sharing the daemon's admission and HTTP frontdoor.
use crate::cua::{CuaError, CuaService};
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

const REQUEST_LIMIT: usize = 1 << 20;
const RESPONSE_LIMIT: usize = 32 << 20;

pub trait CuaState: Send + Sync + 'static {
    fn cua(&self) -> &Arc<CuaService>;
}

pub(crate) fn namespace(path: &str) -> bool {
    path == "/cua" || path.starts_with("/cua/")
}

pub async fn tools<S: CuaState>(State(state): State<Arc<S>>) -> Response {
    match state.cua().tools() {
        Ok(tools) => bounded_json(&json!({"tools": tools})),
        Err(error) => service_error(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    arguments: Value,
}

pub async fn call<S: CuaState>(State(state): State<Arc<S>>, request: Request<Body>) -> Response {
    let headers = request.headers();
    let media_types: Vec<_> = headers.get_all(header::CONTENT_TYPE).iter().collect();
    if media_types.len() != 1
        || !media_types[0].to_str().is_ok_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return super::json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        );
    }
    if headers.contains_key(header::CONTENT_ENCODING) {
        return super::json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "encoded CUA bodies are unsupported",
        );
    }
    let bytes = match to_bytes(request.into_body(), REQUEST_LIMIT).await {
        Ok(bytes) => bytes,
        Err(error) => {
            use std::error::Error;
            let is_limit = error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>());
            return if is_limit {
                super::json_error(StatusCode::PAYLOAD_TOO_LARGE, "CUA request exceeds 1 MiB")
            } else {
                super::json_error(StatusCode::BAD_REQUEST, "failed to read CUA request")
            };
        }
    };
    let call: Call = match serde_json::from_slice(&bytes) {
        Ok(call) => call,
        Err(_) => return super::json_error(StatusCode::BAD_REQUEST, "invalid native CUA call"),
    };
    if call.name.is_empty() || !call.arguments.is_object() {
        return super::json_error(
            StatusCode::BAD_REQUEST,
            "expected tool name and object arguments",
        );
    }
    match state.cua().call(call.name, call.arguments).await {
        Ok(output) => bounded_json(&output),
        Err(error) => service_error(error),
    }
}

pub async fn health<S: CuaState>(State(state): State<Arc<S>>) -> Response {
    let service = state.cua();
    let mut response = bounded_json(&json!({
        "runtime_available": service.available(),
        "reason": service.reason(),
    }));
    if !service.available() {
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    }
    response
}

pub async fn metrics<S: CuaState>(State(state): State<Arc<S>>) -> Response {
    let mut response = state.cua().metrics().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

fn service_error(error: CuaError) -> Response {
    let (status, message) = match error {
        CuaError::UnknownTool => (StatusCode::NOT_FOUND, "unknown CUA tool"),
        CuaError::InvalidArguments => (StatusCode::BAD_REQUEST, "invalid CUA tool arguments"),
        CuaError::Busy => (StatusCode::TOO_MANY_REQUESTS, "CUA execution is busy"),
        CuaError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "CUA runtime is unavailable",
        ),
        CuaError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            "CUA execution deadline exceeded",
        ),
        CuaError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "CUA execution failed"),
    };
    super::json_error(status, message)
}

/// Stop serialization at the transport cap rather than allocating an unbounded
/// JSON buffer first. Native tool outputs themselves remain driver-owned.
fn bounded_json(value: &impl Serialize) -> Response {
    use std::io::{self, Write};
    struct Limited(Vec<u8>);
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > RESPONSE_LIMIT.saturating_sub(self.0.len()) {
                return Err(io::Error::other("CUA response exceeds limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Limited(Vec::new());
    if serde_json::to_writer(&mut buffer, value).is_err() {
        return super::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CUA response exceeds limit or cannot be encoded",
        );
    }
    let mut response = buffer.0.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}
