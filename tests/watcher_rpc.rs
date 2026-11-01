//! HTTP watcher adapters against real temporary-directory inotify events.
use std::{fs, os::unix::fs::MetadataExt, sync::Arc, time::Duration};

use axum::{
    body::{to_bytes, Body},
    http::Request,
    response::Response,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use coorworker_envd::daemon::{app_with_filesystem, Daemon};
use http_body_util::BodyExt;
use nix::unistd::{geteuid, User};
use serde_json::{json, Value};
use tower::ServiceExt;

fn username() -> String {
    assert!(
        !geteuid().is_root(),
        "watcher RPC tests require an ordinary user"
    );
    User::from_uid(geteuid()).unwrap().unwrap().name
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
async fn unary(
    app: &Router,
    method: &str,
    value: Value,
    basic: Option<&str>,
    token: Option<&str>,
) -> (u16, Value) {
    let mut request = Request::post(format!("/filesystem.Filesystem/{method}"))
        .header("content-type", "application/json");
    if let Some(basic) = basic {
        request = request.header("authorization", basic);
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
    (
        status,
        serde_json::from_slice(&bytes).expect("Connect JSON response"),
    )
}
async fn watch(app: &Router, value: Value, basic: Option<&str>, token: Option<&str>) -> Response {
    let bytes = value.to_string().into_bytes();
    let mut frame = vec![0];
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend(bytes);
    let mut request = Request::post("/filesystem.Filesystem/WatchDir")
        .header("content-type", "application/connect+json")
        .header("connect-protocol-version", "1");
    if let Some(basic) = basic {
        request = request.header("authorization", basic);
    }
    if let Some(token) = token {
        request = request.header("x-access-token", token);
    }
    app.clone()
        .oneshot(request.body(Body::from(frame)).unwrap())
        .await
        .unwrap()
}
struct Frames {
    body: Body,
    pending: Vec<u8>,
}
impl Frames {
    fn new(response: Response) -> Self {
        assert_eq!(response.status(), 200);
        Self {
            body: response.into_body(),
            pending: Vec::new(),
        }
    }
    async fn next(&mut self) -> (u8, Value) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if self.pending.len() >= 5 {
                    let len = u32::from_be_bytes(self.pending[1..5].try_into().unwrap()) as usize;
                    if self.pending.len() >= 5 + len {
                        let flags = self.pending[0];
                        let value = serde_json::from_slice(&self.pending[5..5 + len]).unwrap();
                        self.pending.drain(..5 + len);
                        return (flags, value);
                    }
                }
                let frame = self.body.frame().await.expect("open HTTP body").unwrap();
                if let Ok(data) = frame.into_data() {
                    self.pending.extend(data);
                }
            }
        })
        .await
        .expect("bounded HTTP event")
    }
    async fn event(&mut self, name: &str, kind: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (flags, value) = self.next().await;
                assert_eq!(flags, 0);
                let event = &value["filesystem"];
                if event["name"] == name && event["type"] == kind {
                    return event.clone();
                }
            }
        })
        .await
        .unwrap()
    }
}
fn inotify_fds_for_inode(inode: u64) -> Vec<String> {
    fs::read_dir("/proc/self/fdinfo")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let data = fs::read_to_string(entry.path()).ok()?;
            data.lines()
                .any(|line| {
                    line.starts_with("inotify ")
                        && line
                            .split_whitespace()
                            .any(|field| field == format!("ino:{inode:x}"))
                })
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

#[tokio::test]
async fn watcher_unary_creation_drain_basic_and_token_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let user = username();
    let app = app_with_filesystem(Arc::new(Daemon::new()));
    init(&app,json!({"defaultUser":"envd-unknown-watcher-8a19","defaultWorkdir":dir.path(),"accessToken":"watcher-test-token"})).await;
    let basic = format!(
        "Basic {}",
        STANDARD.encode(format!("{user}:ignored-password"))
    );
    assert_eq!(
        unary(
            &app,
            "CreateWatcher",
            json!({"path":""}),
            Some(&basic),
            None
        )
        .await
        .0,
        401
    );
    let (status, missing_user) = unary(
        &app,
        "CreateWatcher",
        json!({"path":""}),
        None,
        Some("watcher-test-token"),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(missing_user["code"], "unauthenticated");
    let (status, created) = unary(
        &app,
        "CreateWatcher",
        json!({"path":"","includeEntry":true}),
        Some(&basic),
        Some("watcher-test-token"),
    )
    .await;
    assert_eq!(status, 200);
    let id = created["watcherId"].as_str().unwrap();
    assert!(id.starts_with('w'));
    // ID operations do not resolve the invalid default account or enforce creator ownership.
    assert_eq!(
        unary(
            &app,
            "GetWatcherEvents",
            json!({"watcherId":id}),
            None,
            Some("watcher-test-token")
        )
        .await,
        (200, json!({}))
    );
    fs::write(dir.path().join("event"), b"hello").unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, value) = unary(
                &app,
                "GetWatcherEvents",
                json!({"watcherId":id}),
                None,
                Some("watcher-test-token"),
            )
            .await;
            assert_eq!(status, 200);
            if let Some(events) = value["events"].as_array() {
                assert!(events.iter().all(|e| e["name"] == "event"));
                assert!(events.iter().any(|e| e["entry"]["name"] == "event"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let unknown = format!(
        "Basic {}",
        STANDARD.encode("envd-unknown-watcher-8a19:unused")
    );
    assert_eq!(
        unary(
            &app,
            "GetWatcherEvents",
            json!({"watcherId":id}),
            Some(&unknown),
            Some("watcher-test-token")
        )
        .await
        .0,
        401
    );
    assert_eq!(
        unary(
            &app,
            "RemoveWatcher",
            json!({"watcherId":id}),
            None,
            Some("watcher-test-token")
        )
        .await,
        (200, json!({}))
    );
    for method in ["GetWatcherEvents", "RemoveWatcher"] {
        let (status, error) = unary(
            &app,
            method,
            json!({"watcherId":id}),
            None,
            Some("watcher-test-token"),
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(error["code"], "not_found");
        assert_eq!(error["message"], format!("watcher with id {id} not found"));
    }
}

#[tokio::test]
async fn watch_http_stream_has_start_real_events_and_cancellation_closes_inotify() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(Daemon::new()));
    init(
        &app,
        json!({"defaultUser":username(),"defaultWorkdir":dir.path()}),
    )
    .await;
    let inode = fs::metadata(dir.path()).unwrap().ino();
    let mut frames = Frames::new(
        watch(
            &app,
            json!({"path":"","recursive":true,"includeEntry":true}),
            None,
            None,
        )
        .await,
    );
    assert_eq!(frames.next().await, (0, json!({"start":{}})));
    let fds = inotify_fds_for_inode(inode);
    assert!(!fds.is_empty(), "real root inotify registration");
    fs::write(dir.path().join("file"), b"data").unwrap();
    let event = frames.event("file", "EVENT_TYPE_CREATE").await;
    assert_eq!(event["entry"]["type"], "FILE_TYPE_FILE");
    assert_eq!(
        event["entry"]["path"],
        dir.path().join("file").to_str().unwrap()
    );
    frames.event("file", "EVENT_TYPE_WRITE").await;
    fs::rename(dir.path().join("file"), dir.path().join("renamed")).unwrap();
    assert!(frames
        .event("file", "EVENT_TYPE_RENAME")
        .await
        .get("entry")
        .is_none());
    frames.event("renamed", "EVENT_TYPE_CREATE").await;
    drop(frames);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !inotify_fds_for_inode(inode).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("canceled response must close watcher fd");
}

#[tokio::test]
async fn watcher_http_errors_are_connect_status_or_terminal_stream_error() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(Daemon::new()));
    let user = username();
    init(
        &app,
        json!({"defaultUser":user,"defaultWorkdir":dir.path()}),
    )
    .await;
    fs::write(dir.path().join("file"), b"x").unwrap();
    for (path, expected, status) in [
        (dir.path().join("missing"), "not_found", 404),
        (dir.path().join("file"), "invalid_argument", 400),
    ] {
        let (actual, error) = unary(&app, "CreateWatcher", json!({"path":path}), None, None).await;
        assert_eq!(actual, status);
        assert_eq!(error["code"], expected);
        let mut frames = Frames::new(watch(&app, json!({"path":path}), None, None).await);
        let (flags, error) = frames.next().await;
        assert_eq!(flags, 2);
        assert_eq!(error["error"]["code"], expected);
    }
    let unknown = format!("Basic {}", STANDARD.encode("envd-unknown-watcher-8a19:any"));
    let mut frames = Frames::new(watch(&app, json!({"path":""}), Some(&unknown), None).await);
    let (flags, error) = frames.next().await;
    assert_eq!(flags, 2);
    assert_eq!(error["error"]["code"], "unauthenticated");
}
