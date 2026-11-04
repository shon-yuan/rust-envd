//! Contract for reusing the worker's MCP transport without a listener or desktop.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body, Bytes},
    http::{Request, StatusCode},
    Router,
};
use coorworker_cua::{app, mcp_router, Auth, StaticProvider};
use cua_driver_core::server::ToolProvider;
use serde_json::{json, Value};
use tower::ServiceExt;

fn provider() -> Arc<dyn ToolProvider> {
    Arc::new(StaticProvider::new(json!({
        "tools": [{
            "name": "health_report",
            "description": "echo the supplied arguments without desktop access",
            "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}}
        }]
    })))
}

async fn request(router: Router, request: Request<Body>) -> (StatusCode, Bytes) {
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (status, body)
}

async fn rpc(router: Router, body: impl Into<Body>) -> (StatusCode, Value) {
    let (status, body) = request(
        router,
        Request::post("/mcp")
            .header("content-type", "application/json")
            .body(body.into())
            .unwrap(),
    )
    .await;
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn mcp_router_excludes_health_and_unrelated_routes() {
    let router = mcp_router(provider(), Auth::disabled());
    for path in ["/health", "/", "/other"] {
        let (status, _) = request(
            router.clone(),
            Request::get(path).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {path}");
    }
    let (status, body) = request(router, Request::get("/mcp").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(body.is_empty());
}

#[tokio::test]
async fn standalone_app_retains_health_and_mcp() {
    let router = app(provider(), Auth::bearer("worker-secret"));
    let (status, body) = request(
        router.clone(),
        Request::get("/health").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());

    let (status, body) = request(
        router,
        Request::post("/mcp")
            .header("content-type", "application/json")
            .header("authorization", "Bearer worker-secret")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["result"]["tools"][0]["name"], "health_report");
}

#[tokio::test]
async fn mcp_router_initializes_lists_and_calls_the_supplied_provider() {
    let router = mcp_router(provider(), Auth::disabled());
    let (status, body) = rpc(
        router.clone(),
        json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "router-contract", "version": "1"}
            }
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], "init");
    assert!(body["result"]["protocolVersion"].is_string(), "{body}");
    assert!(
        body["result"]["capabilities"]["tools"].is_object(),
        "{body}"
    );

    let (status, body) = rpc(
        router.clone(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], 2);
    assert_eq!(body["result"]["tools"], provider().tools_list()["tools"]);

    let (status, body) = rpc(
        router,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"health_report","arguments":{"message":"hello"}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], 3);
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(body["result"]["content"][0]["type"], "text");
    assert_eq!(
        body["result"]["content"][0]["text"],
        r#"health_report:{"message":"hello"}"#
    );
}

#[tokio::test]
async fn mcp_router_accepts_notifications_without_a_response_body() {
    let (status, body) = request(
        mcp_router(provider(), Auth::disabled()),
        Request::post("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(body.is_empty());
}

#[tokio::test]
async fn mcp_router_preserves_parse_and_invalid_request_errors() {
    let router = mcp_router(provider(), Auth::disabled());
    for (input, code) in [("{", -32700), (r#"{"jsonrpc":"2.0","id":1}"#, -32600)] {
        let (status, body) = rpc(router.clone(), input).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{input}");
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], Value::Null);
        assert_eq!(body["error"]["code"], code);
        assert!(body["error"]["message"].is_string(), "{body}");
    }
}

#[tokio::test]
async fn mcp_router_preserves_worker_bearer_authentication() {
    let router = mcp_router(provider(), Auth::bearer("worker-secret"));
    for header in [None, Some(("authorization", "Bearer wrong"))] {
        let mut builder = Request::post("/mcp");
        if let Some((name, value)) = header {
            builder = builder.header(name, value);
        }
        let (status, body) = request(router.clone(), builder.body(Body::from("{")).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.is_empty());
    }

    for name in ["authorization", "x-cua-env-authorization"] {
        let (status, body) = request(
            router.clone(),
            Request::post("/mcp")
                .header(name, "Bearer worker-secret")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name}");
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"]["tools"][0]["name"], "health_report");
    }
}

#[cfg(feature = "desktop")]
#[test]
fn desktop_provider_is_exported_without_constructing_a_desktop() {
    let _: fn() -> Arc<dyn ToolProvider> = coorworker_cua::desktop_provider;
}
