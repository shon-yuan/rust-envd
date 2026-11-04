//! Exercises the reusable MCP implementation inside envd; no desktop input or capture.
#![cfg(feature = "cua-desktop")]

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use coorworker_cua::{mcp_router, Auth, StaticProvider};
use coorworker_envd::{
    configuration::Configuration,
    ext::{cua::CuaExtension, Extension},
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

fn extension(configuration: Arc<Mutex<Configuration>>) -> Router {
    let provider = Arc::new(StaticProvider::new(json!({"tools":[{
        "name":"health_report", "description":"isolated echo", "inputSchema":{"type":"object"}
    }]})));
    CuaExtension::embedded(mcp_router(provider, Auth::disabled()), configuration).router()
}

async fn configured() -> Arc<Mutex<Configuration>> {
    let configuration = Arc::new(Mutex::new(Configuration::new()));
    configuration
        .lock()
        .await
        .access
        .set_token(Some("test-credential".into()))
        .unwrap();
    configuration
}

async fn call(router: &Router, body: impl Into<Body>, token: bool) -> (StatusCode, Value) {
    let mut request = Request::post("/mcp");
    if token {
        request = request.header("authorization", "Bearer test-credential");
    }
    let response = router
        .clone()
        .oneshot(request.body(body.into()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 40 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn reused_mcp_router_supports_protocol_and_enforces_live_credential() {
    let configuration = configured().await;
    let router = extension(configuration.clone());
    for (method, params) in [
        ("initialize", json!({})),
        ("tools/list", json!({})),
        (
            "tools/call",
            json!({"name":"health_report","arguments":{"value":1}}),
        ),
    ] {
        let (status, body) = call(
            &router,
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{method}: {body}");
        assert!(body.get("result").is_some(), "{body}");
        if method == "tools/call" {
            assert_eq!(body["result"]["isError"], false, "{body}");
        }
    }
    let (status, body) = call(
        &router,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, Value::Null);
    let (status, body) = call(&router, "{", true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32700);
    assert_eq!(call(&router, "{", false).await.0, StatusCode::UNAUTHORIZED);
    configuration.lock().await.access.set_token(None).unwrap();
    assert_eq!(call(&router, "{}", true).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn request_limit_applies_after_admission_without_reaching_backend() {
    let router = extension(configured().await);
    assert_eq!(
        call(&router, "x".repeat((1 << 20) + 1), true).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        call(&router, "x".repeat((1 << 20) + 1), false).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test(start_paused = true)]
async fn embedded_backend_timeout_is_bounded_and_recorded_as_error() {
    let backend = Router::new().route(
        "/mcp",
        post(|| async { std::future::pending::<StatusCode>().await }),
    );
    let router = CuaExtension::embedded(backend, configured().await).router();
    assert_eq!(
        call(
            &router,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"blocked"}}"#,
            true
        )
        .await
        .0,
        StatusCode::GATEWAY_TIMEOUT
    );
    let response = router
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let text =
        String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap();
    assert!(text.contains("tool=\"blocked\",status=\"error\"} 1"));
}

#[tokio::test]
async fn desktop_build_registers_platform_tools_without_executing_them() {
    let provider = coorworker_cua::desktop_provider();
    let list = provider.tools_list();
    let tools = list["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    assert!(tools.iter().any(|tool| tool["name"] == "list_windows"));
}
