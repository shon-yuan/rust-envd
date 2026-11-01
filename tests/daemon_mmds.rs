use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::{
    daemon::{app, Daemon},
    mmds::{BoxFuture, TokenHashProvider},
};
use sha2::{Digest, Sha512};
use tower::ServiceExt;

struct GatedProvider {
    gate: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
}
impl TokenHashProvider for GatedProvider {
    fn token_hash(&self) -> BoxFuture<'_, std::io::Result<String>> {
        Box::pin(async move {
            if self.gate.load(Ordering::SeqCst) {
                self.entered.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(String::new())
        })
    }
}

#[tokio::test]
async fn slow_unauthorized_mmds_does_not_block_health_or_authenticated_requests() {
    let provider = Arc::new(GatedProvider {
        gate: std::sync::atomic::AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
    });
    let router = app(Arc::new(Daemon::with_token_hash_provider(provider.clone())));
    let installed = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"installed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(installed.status(), StatusCode::NO_CONTENT);
    provider.gate.store(true, Ordering::SeqCst);
    let pending_router = router.clone();
    let pending = tokio::spawn(async move {
        pending_router
            .oneshot(
                Request::post("/init")
                    .body(Body::from(
                        r#"{"accessToken":"wrong","envVars":{"BAD":"value"}}"#,
                    ))
                    .unwrap(),
            )
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.entered.notified(),
    )
    .await
    .unwrap();
    let health = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        router
            .clone()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap()),
    )
    .await;
    let envs = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        router.clone().oneshot(
            Request::get("/envs")
                .header("x-access-token", "installed")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert_eq!(
        health
            .expect("health blocked on unauthorized lookup")
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let envs = envs
        .expect("authenticated request blocked on unauthorized lookup")
        .unwrap();
    assert_eq!(envs.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(envs.into_body(), 4096).await.unwrap()).unwrap();
    assert!(value.get("BAD").is_none());
    let recovered = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        router.oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"installed"}"#))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.status(), StatusCode::NO_CONTENT);
}

#[derive(Default)]
struct Provider {
    calls: AtomicUsize,
    hash: Mutex<Option<String>>,
}
impl Provider {
    fn set(&self, token: &str) {
        *self.hash.lock().unwrap() = Some(format!("{:x}", Sha512::digest(token.as_bytes())));
    }
}
impl TokenHashProvider for Provider {
    fn token_hash(&self) -> BoxFuture<'_, std::io::Result<String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let hash = self.hash.lock().unwrap().clone();
        Box::pin(async move { hash.ok_or_else(|| std::io::Error::other("unavailable")) })
    }
}

#[tokio::test]
async fn mmds_controls_first_setup_rotation_and_reset_but_current_token_bypasses_lookup() {
    let provider = Arc::new(Provider::default());
    provider.set("first");
    let router = app(Arc::new(Daemon::with_token_hash_provider(provider.clone())));
    for (body, expected, calls) in [
        (r#"{"cpuCount":"invalid"}"#, StatusCode::BAD_REQUEST, 0),
        (r#"{"accessToken":"wrong"}"#, StatusCode::UNAUTHORIZED, 1),
        (r#"{}"#, StatusCode::UNAUTHORIZED, 2),
        (r#"{"accessToken":"first"}"#, StatusCode::NO_CONTENT, 3),
        (
            r#"{"accessToken":"first","envVars":{"VALUE":"retained"}}"#,
            StatusCode::NO_CONTENT,
            3,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{body}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
    }
    provider.set("second");
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"second"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let old = router
        .clone()
        .oneshot(
            Request::get("/envs")
                .header("x-access-token", "first")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    provider.set("");
    let reset = router
        .clone()
        .oneshot(Request::post("/init").body(Body::from("{}")).unwrap())
        .await
        .unwrap();
    assert_eq!(reset.status(), StatusCode::NO_CONTENT);
    let envs = router
        .oneshot(Request::get("/envs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(envs.status(), StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(envs.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(value["VALUE"], "retained");
}

#[tokio::test]
async fn missing_mmds_hash_allows_first_setup_but_never_resets_an_installed_token() {
    let provider = Arc::new(Provider::default());
    let router = app(Arc::new(Daemon::with_token_hash_provider(provider.clone())));
    let installed = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"installed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(installed.status(), StatusCode::NO_CONTENT);
    for hash in [None, Some(String::new())] {
        *provider.hash.lock().unwrap() = hash;
        let rejected = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            to_bytes(rejected.into_body(), 4096).await.unwrap(),
            "access token reset not authorized"
        );
    }
}
