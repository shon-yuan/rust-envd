//! The S0 invariant: an extension surface is additive and cannot shadow the
//! frozen envd protocol surface.
//!
//! These tests run against a real daemon router, not a toy one, because the
//! property being asserted is about the interaction with [`daemon::layered`]:
//! envd's frontdoor/authorization must still own every S0 path while an
//! extension path is served without them.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use coorworker_envd::daemon::{app_with_extensions, app_with_filesystem_and_extensions, Daemon};
use coorworker_envd::ext::{Extension, Extensions};
use tower::ServiceExt;

struct Cua;

impl Extension for Cua {
    fn name(&self) -> &str {
        "cua"
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/health", get(|| async { "cua-health" }))
            // Deliberately declares an envd path name. Nesting must keep it
            // under /cua, and the root /envs must keep envd's 401.
            .route("/envs", get(|| async { "cua-envs" }))
    }
}

fn extensions() -> Extensions {
    Extensions::new(vec![Arc::new(Cua) as Arc<dyn Extension>]).unwrap()
}

async fn call(router: Router, path: &str) -> (StatusCode, String) {
    let response = router
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Installs the `/init` access token, so envd's authorization layer has
/// something to reject. Before `/init` the daemon is unauthenticated by
/// design, and 401 is not the observable behaviour.
async fn install_token(router: Router) {
    let response = router
        .oneshot(
            Request::post("/init")
                .body(Body::from(
                    r#"{"accessToken":"ext-test-token","defaultUser":"root","defaultWorkdir":"/tmp"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn extension_is_additive_and_cannot_shadow_s0() {
    let router = app_with_extensions(Arc::new(Daemon::new()), &extensions());
    install_token(router.clone()).await;

    let (status, _) = call(router.clone(), "/health").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "envd /health is untouched");

    let (status, body) = call(router.clone(), "/cua/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "cua-health");

    // No x-access-token: the envd authorization layer must still answer 401.
    let (status, _) = call(router.clone(), "/envs").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The extension's own /envs lives under its prefix and is not gated by
    // envd's authorization layer.
    let (status, body) = call(router.clone(), "/cua/envs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "cua-envs");

    let (status, _) = call(router, "/healthz").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn extension_merges_over_the_connect_fallback() {
    // `app_with_filesystem*` installs a fallback service for the Connect
    // surface; merging an extension on top must not lose it or panic.
    let router = app_with_filesystem_and_extensions(Arc::new(Daemon::new()), &extensions());
    install_token(router.clone()).await;

    let (status, body) = call(router.clone(), "/cua/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "cua-health");

    let (status, _) = call(router, "/files").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn no_extensions_keeps_the_daemon_router_identical() {
    let router = app_with_extensions(Arc::new(Daemon::new()), &Extensions::empty());
    let (status, _) = call(router.clone(), "/health").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(router, "/cua/health").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
