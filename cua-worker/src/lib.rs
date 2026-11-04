//! Reusable Cua Driver MCP transport and desktop provider for rust-envd.
//!
//! [`mcp_router`] exposes only `/mcp` for in-process dispatch. [`app`] adds
//! standalone worker health. The `desktop` feature (enabled by default for
//! the worker) supplies `desktop_provider`; envd opts into that stack only
//! in its `cua-desktop` distribution, preserving desktop-free base builds.
//!
//! Wire surface:
//!
//! * `GET /health` — 204 while serving.
//! * `POST /mcp` — MCP JSON-RPC 2.0 (`initialize`, `tools/list`, `tools/call`,
//!   and the discovery/skill methods the driver core implements). Legacy
//!   semantics; the driver's own loopback HTTP endpoint is legacy as well.
//! * any other path — 404.
//!
//! Authentication is a bearer token from `CUA_ENV_TOKEN` (or `--token-file`).
//! A loopback bind may run without one; a non-loopback bind refuses to start
//! without it. The token is compared in constant time and is never logged.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cua_driver_core::protocol::Request;
use cua_driver_core::server::{handle_request, ToolProvider};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

/// Bearer-token check for the worker's HTTP surface.
///
/// `Auth::disabled()` is valid for standalone workers only on a loopback bind;
/// the binary enforces that pairing. Embedded callers enforce admission before
/// dispatching to a router with authentication disabled.
#[derive(Clone, Default)]
pub struct Auth {
    token: Option<Vec<u8>>,
}

impl Auth {
    pub fn disabled() -> Self {
        Self { token: None }
    }

    pub fn bearer(token: impl Into<Vec<u8>>) -> Self {
        Self {
            token: Some(token.into()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.token.is_some()
    }

    /// Accepts `authorization: Bearer <token>` or the gateway-friendly
    /// `x-cua-env-authorization: Bearer <token>`.
    pub fn allows(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.token else {
            return true;
        };
        let Some(presented) = presented_token(headers) else {
            return false;
        };
        presented.len() == expected.len() && presented.ct_eq(expected).into()
    }
}

fn presented_token(headers: &HeaderMap) -> Option<&[u8]> {
    ["authorization", "x-cua-env-authorization"]
        .iter()
        .find_map(|name| {
            headers
                .get(*name)
                .and_then(|value| value.as_bytes().strip_prefix(b"Bearer "))
        })
}

#[derive(Clone)]
struct ServerState {
    provider: Arc<dyn ToolProvider>,
    auth: Auth,
}

/// Builds the standalone worker router, including its health endpoint.
pub fn app(provider: Arc<dyn ToolProvider>, auth: Auth) -> Router {
    mcp_router(provider, auth).route("/health", get(health))
}

/// Builds only the MCP endpoint for reuse without a listener or health route.
/// The provider is Cua Driver's [`cua_driver_core::tool::ToolRegistry`] in
/// production and a stub in tests.
pub fn mcp_router(provider: Arc<dyn ToolProvider>, auth: Auth) -> Router {
    Router::new()
        .route("/mcp", post(mcp).get(method_not_allowed))
        .with_state(Arc::new(ServerState { provider, auth }))
}

/// Registers the native desktop tools for the current Linux or Windows session.
#[cfg(feature = "desktop")]
pub fn desktop_provider() -> Arc<dyn ToolProvider> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(platform_linux::register_tools())
    }
    #[cfg(windows)]
    {
        Arc::new(platform_windows::register_tools())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        compile_error!("coorworker-cua supports Linux and Windows guests only");
    }
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn method_not_allowed() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "").into_response()
}

async fn mcp(State(state): State<Arc<ServerState>>, headers: HeaderMap, body: Bytes) -> Response {
    if !state.auth.allows(&headers) {
        return (StatusCode::UNAUTHORIZED, "").into_response();
    }

    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(rpc_error(
                    Value::Null,
                    -32700,
                    &format!("Parse error: {error}"),
                )),
            )
                .into_response();
        }
    };
    let request: Request = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(rpc_error(
                    Value::Null,
                    -32600,
                    &format!("Invalid Request: {error}"),
                )),
            )
                .into_response();
        }
    };

    let Some(id) = request.id.clone() else {
        // A notification still runs (the driver tracks initialize), but MCP
        // answers it with an empty 202 rather than a JSON-RPC response.
        let _ = handle_request(request, Value::Null, state.provider.as_ref()).await;
        return StatusCode::ACCEPTED.into_response();
    };

    let response = handle_request(request, id, state.provider.as_ref()).await;
    Json(response).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// A provider for tests and for a worker started without a desktop.
pub struct StaticProvider {
    tools: Value,
}

impl StaticProvider {
    pub fn new(tools: Value) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl ToolProvider for StaticProvider {
    fn tools_list(&self) -> Value {
        self.tools.clone()
    }

    async fn invoke_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        Ok(json!({
            "content": [{"type": "text", "text": format!("{name}:{arguments}")}],
            "isError": false
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn provider() -> Arc<dyn ToolProvider> {
        Arc::new(StaticProvider::new(json!({
            "tools": [{
                "name": "health_report",
                "description": "capture the desktop",
                "inputSchema": {"type": "object", "properties": {}}
            }]
        })))
    }

    async fn call(app: Router, headers: &[(&str, &str)], body: &str) -> (StatusCode, Value) {
        let mut request = HttpRequest::post("/mcp").header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = app
            .oneshot(request.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn health_is_204() {
        let response = app(provider(), Auth::disabled())
            .oneshot(HttpRequest::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn initialize_lists_tools_and_calls_one() {
        let app = app(provider(), Auth::disabled());
        let (status, body) = call(
            app.clone(),
            &[],
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["result"]["protocolVersion"].is_string(), "{body}");

        let (status, body) = call(
            app.clone(),
            &[],
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["tools"][0]["name"], "health_report");

        let (status, body) = call(
            app,
            &[],
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"health_report","arguments":{}}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("health_report:")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn notifications_are_accepted_without_a_body() {
        let response = app(provider(), Auth::disabled())
            .oneshot(
                HttpRequest::post("/mcp")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn malformed_bodies_are_json_rpc_errors() {
        let (status, body) = call(app(provider(), Auth::disabled()), &[], "{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn a_token_is_required_when_configured() {
        let app = app(provider(), Auth::bearer("secret"));
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

        let (status, _) = call(app.clone(), &[], body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _) = call(app.clone(), &[("authorization", "Bearer wrong")], body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _) = call(app.clone(), &[("authorization", "Bearer secret")], body).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = call(app, &[("x-cua-env-authorization", "Bearer secret")], body).await;
        assert_eq!(status, StatusCode::OK);
    }
}
