use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use coorworker_envd::daemon::{app, app_with_filesystem, Daemon};
use serde_json::json;
use tower::ServiceExt;

const MISSING: &str = "envd-parity-no-such-account";

fn authorization(username: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{username}:ignored")))
}

#[tokio::test]
async fn explicit_missing_user_is_rejected_before_all_http_handlers() {
    for (method, path, body) in [
        ("GET", "/health", ""),
        ("GET", "/envs", ""),
        ("POST", "/init", "{}"),
        ("GET", "/files?path=/not-present", ""),
        ("POST", "/files?path=/not-present", ""),
    ] {
        let response = app(Arc::new(Daemon::new()))
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", authorization(MISSING))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{method} {path}");
        assert_eq!(response.headers()["content-type"], "application/json");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"code":"unauthenticated", "message":format!("invalid username: '{MISSING}'")})
        );
    }
}

#[tokio::test]
async fn missing_user_connect_stream_error_precedes_handler_and_body_decoding() {
    let response = app_with_filesystem(Arc::new(Daemon::new()))
        .oneshot(
            Request::post("/filesystem.Filesystem/WatchDir")
                .header("authorization", authorization(MISSING))
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .body(Body::from("not-an-envelope"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    assert_eq!(body[0], 2);
    assert_eq!(
        u32::from_be_bytes(body[1..5].try_into().unwrap()) as usize,
        body.len() - 5
    );
    let end: serde_json::Value = serde_json::from_slice(&body[5..]).unwrap();
    assert_eq!(
        end["error"],
        json!({"code":"unauthenticated", "message":format!("invalid username: '{MISSING}'")})
    );
}

#[tokio::test]
async fn basic_error_writer_classifies_method_version_and_grpc_web_like_go() {
    for (method, content_type, version, status) in [
        ("POST", "application/connect+json", "2", 401),
        ("GET", "application/connect+json", "1", 401),
        ("GET", "application/grpc+proto", "1", 401),
        ("POST", "Application/Connect+Json", "1", 200),
        ("POST", "application/connect+json", "", 200),
        ("POST", "application/grpc-web+proto", "1", 200),
    ] {
        let response = app(Arc::new(Daemon::new()))
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/health")
                    .header("authorization", authorization(MISSING))
                    .header("content-type", content_type)
                    .header("connect-protocol-version", version)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            status,
            "{method} {content_type} {version}"
        );
        if content_type == "application/grpc-web+proto" {
            assert_eq!(response.headers()["grpc-status"], "16");
            assert!(to_bytes(response.into_body(), 8192)
                .await
                .unwrap()
                .is_empty());
        } else if status == 200 {
            assert_eq!(
                response.headers()["content-type"],
                "application/connect+json"
            );
            let body = to_bytes(response.into_body(), 8192).await.unwrap();
            assert_eq!(body[0], 2);
            let end: serde_json::Value = serde_json::from_slice(&body[5..]).unwrap();
            assert_eq!(end["error"]["code"], "unauthenticated");
        } else {
            assert_eq!(response.headers()["content-type"], "application/json");
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                    .unwrap();
            assert_eq!(body["code"], "unauthenticated");
        }
    }
}

#[tokio::test]
async fn malformed_basic_is_ignored_and_valid_basic_password_is_ignored() {
    for header in [
        "Basic invalid!".to_owned(),
        format!("Basic {}", STANDARD.encode(MISSING)),
        "Bearer unknown".to_owned(),
        authorization("root"),
    ] {
        let response = app(Arc::new(Daemon::new()))
            .oneshot(
                Request::get("/health")
                    .header("authorization", header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 204);
    }
}

#[tokio::test]
async fn general_access_token_rejection_precedes_basic_user_rejection() {
    let router = app(Arc::new(Daemon::new()));
    let init = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"test-only-basic-order"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(init.status(), 204);
    let response = router
        .oneshot(
            Request::get("/envs")
                .header("authorization", authorization(MISSING))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(body["code"], 401);
}
