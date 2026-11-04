use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    routing::{get, post},
    Router,
};
use coorworker_envd::{
    configuration::Configuration,
    ext::{cua::CuaExtension, Extension},
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[tokio::test]
async fn shared_external_forwards_to_a_validated_ipv6_loopback_helper() {
    let helper = Router::new()
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/mcp",
            post(|headers: HeaderMap| async move {
                if headers
                    .get("authorization")
                    .is_some_and(|value| value == "Bearer internal")
                {
                    StatusCode::ACCEPTED
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        );
    let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
        Ok(listener) => listener,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
            ) =>
        {
            eprintln!("IPv6 loopback unavailable on this host: {error}; normalization has a separate unit regression");
            return;
        }
        Err(error) => panic!("IPv6 fixture bind failed: {error}"),
    };
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, helper).await.unwrap();
    });
    let configuration = Arc::new(Mutex::new(Configuration::new()));
    configuration
        .lock()
        .await
        .access
        .set_token(Some("external".into()))
        .unwrap();
    let extension = CuaExtension::adopt_shared(address, configuration, b"internal".to_vec())
        .await
        .unwrap();
    let response = extension
        .router()
        .oneshot(
            Request::post("/mcp")
                .header("authorization", "Bearer external")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    server.abort();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}
