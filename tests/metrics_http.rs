use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::daemon::{app, Daemon};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn metrics_returns_real_linux_sample_with_go_headers_and_shape() {
    let router = app(Arc::new(Daemon::new()));
    for _ in 0..2 {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let response = router
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["content-type"], "application/json");
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let metrics: Value = serde_json::from_slice(&bytes).unwrap();
        let object = metrics.as_object().unwrap();
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "cpu_count",
                "cpu_possible",
                "cpu_target",
                "cpu_target_attempts",
                "cpu_used_pct",
                "cpu_write_pending_ms",
                "disk_total",
                "disk_used",
                "mem_cache",
                "mem_total",
                "mem_used",
                "ts"
            ]
        );
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!((before..=after).contains(&metrics["ts"].as_u64().unwrap()));
        assert!(metrics["cpu_count"].as_u64().unwrap() > 0);
        assert!((0.0..=100.0).contains(&metrics["cpu_used_pct"].as_f64().unwrap()));
        for field in [
            "cpu_possible",
            "cpu_target",
            "cpu_target_attempts",
            "cpu_write_pending_ms",
        ] {
            assert_eq!(metrics[field], 0);
        }
        for (used, total) in [("mem_used", "mem_total"), ("disk_used", "disk_total")] {
            assert!(metrics[total].as_u64().unwrap() > 0);
            assert!(metrics[used].as_u64().unwrap() <= metrics[total].as_u64().unwrap());
        }
        assert!(metrics["mem_cache"].as_u64().is_some());
    }
}

#[tokio::test]
async fn metrics_uses_global_token_then_basic_authorization() {
    let router = app(Arc::new(Daemon::new()));
    let init = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"metrics-test-token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(init.status(), StatusCode::NO_CONTENT);
    for token in [None, Some("wrong")] {
        let mut request =
            Request::get("/metrics").header("authorization", "Basic X19lbnZkX21pc3NpbmdfdXNlcjp4");
        if let Some(token) = token {
            request = request.header("x-access-token", token);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        assert_eq!(
            bytes.last(),
            Some(&b'\n'),
            "token error retains Go encoder newline"
        );
        let error: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(error["code"], 401);
    }
    let response = router
        .clone()
        .oneshot(
            Request::get("/metrics")
                .header("x-access-token", "metrics-test-token")
                .header("authorization", "Basic X19lbnZkX21pc3NpbmdfdXNlcjp4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let error: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error["code"], "unauthenticated");
    assert_eq!(error["message"], "invalid username: '__envd_missing_user'");
    let response = router
        .oneshot(
            Request::get("/metrics")
                .header("x-access-token", "metrics-test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
