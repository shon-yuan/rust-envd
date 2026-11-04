//! Platform-independent shared admission; real Linux /init is covered separately.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use coorworker_envd::{
    configuration::Configuration,
    ext::{cua::CuaExtension, Extension},
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

async fn status(router: &Router, token: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn embedded_admission_tracks_the_same_configuration_on_every_platform() {
    let configuration = Arc::new(Mutex::new(Configuration::new()));
    let backend = Router::new().route("/mcp", post(|| async { StatusCode::ACCEPTED }));
    let router = CuaExtension::embedded(backend, configuration.clone()).router();
    assert_eq!(status(&router, "first").await, StatusCode::UNAUTHORIZED);
    configuration
        .lock()
        .await
        .access
        .set_token(Some("first".into()))
        .unwrap();
    assert_eq!(status(&router, "first").await, StatusCode::ACCEPTED);
    assert_eq!(status(&router, "wrong").await, StatusCode::UNAUTHORIZED);
    configuration
        .lock()
        .await
        .access
        .set_token(Some("second".into()))
        .unwrap();
    assert_eq!(status(&router, "first").await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(&router, "second").await, StatusCode::ACCEPTED);
    configuration.lock().await.access.set_token(None).unwrap();
    assert_eq!(status(&router, "second").await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn shared_helper_refuses_non_loopback_and_absent_transport_credentials() {
    let configuration = Arc::new(Mutex::new(Configuration::new()));
    for (address, token) in [
        ("0.0.0.0:3211", b"private".to_vec()),
        ("127.0.0.1:3211", vec![]),
        ("127.0.0.1:3211", b"bad\nvalue".to_vec()),
    ] {
        assert!(
            CuaExtension::adopt_shared(address.parse().unwrap(), configuration.clone(), token)
                .await
                .is_err()
        );
    }
}
