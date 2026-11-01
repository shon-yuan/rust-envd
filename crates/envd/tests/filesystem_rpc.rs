//! Narrow adapter checks; runtime semantics are covered by filesystem_runtime.
use std::{fs, os::unix::fs::symlink, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use coorworker_envd::daemon::{app_with_filesystem, Daemon};
use nix::unistd::{geteuid, User};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(
    app: &Router,
    method: &str,
    value: Value,
    authorization: Option<&str>,
    token: Option<&str>,
) -> (u16, Value) {
    let mut request = Request::post(format!("/filesystem.Filesystem/{method}"))
        .header("content-type", "application/json")
        .header("connect-protocol-version", "1");
    if let Some(auth) = authorization {
        request = request.header("authorization", auth);
    }
    if let Some(token) = token {
        request = request.header("x-access-token", token);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn init(app: &Router, value: Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
}

fn username() -> String {
    assert!(
        !geteuid().is_root(),
        "RPC filesystem tests require an ordinary user"
    );
    User::from_uid(geteuid()).unwrap().unwrap().name
}

#[tokio::test]
async fn five_unary_methods_return_real_entries_and_go_errors() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(Daemon::new()));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir":dir.path()}),
    )
    .await;
    let (status, made) = call(&app, "MakeDir", json!({"path":""}), None, None).await;
    assert_eq!(status, 409);
    assert_eq!(made["code"], "already_exists");
    let (status, made) = call(
        &app,
        "MakeDir",
        json!({"path":dir.path().join("parent/child")}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(made["entry"]["type"], "FILE_TYPE_DIRECTORY");
    assert_eq!(
        made["entry"]["path"],
        dir.path().join("parent/child").to_str().unwrap()
    );
    fs::write(dir.path().join("parent/child/data"), b"payload").unwrap();
    let file = fs::File::open(dir.path().join("parent/child/data")).unwrap();
    xattr::FileExt::set_xattr(&file, "user.e2b.origin", b"rpc").unwrap();
    let (status, moved) = call(&app, "Move", json!({"source":dir.path().join("parent/child/data"), "destination":dir.path().join("new/data")}), None, None).await;
    assert_eq!(status, 200);
    assert_eq!(moved["entry"]["size"], "7");
    assert_eq!(moved["entry"]["metadata"]["origin"], "rpc");
    symlink("new/data", dir.path().join("alias")).unwrap();
    let (status, stat) = call(
        &app,
        "Stat",
        json!({"path":dir.path().join("alias")}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(stat["entry"]["isSymlink"], true);
    assert_eq!(stat["entry"]["type"], "FILE_TYPE_FILE");
    assert_eq!(stat["entry"]["size"], "8");
    assert!(stat["entry"]["modifiedTime"].is_string());
    let (status, listed) = call(&app, "ListDir", json!({"path":"", "depth":0}), None, None).await;
    assert_eq!(status, 200);
    assert_eq!(listed["entries"].as_array().unwrap().len(), 3);
    let (status, removed) = call(
        &app,
        "Remove",
        json!({"path":dir.path().join("new")}),
        None,
        None,
    )
    .await;
    assert_eq!((status, removed), (200, json!({})));
    assert!(!dir.path().join("new").exists());
    assert_eq!(
        call(
            &app,
            "Remove",
            json!({"path":dir.path().join("absent")}),
            None,
            None
        )
        .await,
        (200, json!({}))
    );
    for (method, request, status, code) in [
        (
            "Stat",
            json!({"path":dir.path().join("absent")}),
            404,
            "not_found",
        ),
        ("MakeDir", json!({"path":"~other"}), 400, "invalid_argument"),
        (
            "Move",
            json!({"source":dir.path().join("absent"),"destination":dir.path().join("dst")}),
            404,
            "not_found",
        ),
        (
            "ListDir",
            json!({"path":dir.path().join("alias")}),
            404,
            "not_found",
        ),
    ] {
        let (actual, error) = call(&app, method, request, None, None).await;
        assert_eq!(actual, status, "{method}");
        assert_eq!(error["code"], code, "{method}");
    }
}

#[tokio::test]
async fn basic_user_overrides_default_password_is_ignored_and_bad_user_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(Daemon::new()));
    init(
        &app,
        json!({"defaultUser":"envd-no-such-user-75f23a", "defaultWorkdir":dir.path()}),
    )
    .await;
    let request = json!({"path":dir.path()});
    assert_eq!(call(&app, "Stat", request.clone(), None, None).await.0, 401);
    let basic = format!(
        "Basic {}",
        STANDARD.encode(format!("{}:ignored:password", username()))
    );
    assert_eq!(
        call(&app, "Stat", request.clone(), Some(&basic), None)
            .await
            .0,
        200
    );
    let unknown = format!(
        "Basic {}",
        STANDARD.encode("envd-no-such-user-75f23a:anything")
    );
    let (status, error) = call(&app, "Stat", request, Some(&unknown), None).await;
    assert_eq!(status, 401);
    assert_eq!(error["code"], "unauthenticated");
}

#[tokio::test]
async fn filesystem_routes_keep_access_token_gate() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(Daemon::new()));
    init(
        &app,
        json!({"defaultUser":username(), "accessToken":"rpc-test-token"}),
    )
    .await;
    let request = json!({"path":dir.path()});
    assert_eq!(call(&app, "Stat", request.clone(), None, None).await.0, 401);
    assert_eq!(
        call(&app, "Stat", request.clone(), None, Some("wrong"))
            .await
            .0,
        401
    );
    assert_eq!(
        call(&app, "Stat", request, None, Some("rpc-test-token"))
            .await
            .0,
        200
    );
}
