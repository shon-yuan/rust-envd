//! Structured log production and the production wiring around it.
//!
//! Oracle: internal/logs/{logger,interceptor}.go and the log-producing call
//! sites in internal/services/{process,filesystem} plus internal/api/init.go at
//! 51083a83cc0903185d992725ea6bb33683814b29. Exporter HTTP goes to a
//! controlled in-memory transport; no test reaches 169.254.169.254.

use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use coorworker_envd::{
    daemon::{app, app_with_filesystem, Daemon},
    exec_context::{Account, Defaults, Environment},
    logs::{logger, Logger, ProcessLogger},
    mmds::{BoxFuture, HttpReply, HttpRequest, HttpTransport, MmdsOpts},
    process::ProcessRuntime,
    proto::process as pb,
};
use nix::unistd::{getegid, geteuid, getgroups, User};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Captures every encoded line handed to the sink.
#[derive(Default)]
struct Capture {
    lines: Mutex<Vec<Vec<u8>>>,
}

impl Capture {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn logger(self: &Arc<Self>) -> Logger {
        Logger::new(vec![self.clone()])
    }

    fn lines(&self) -> Vec<Vec<u8>> {
        self.lines.lock().unwrap().clone()
    }

    fn values(&self) -> Vec<Value> {
        self.lines()
            .into_iter()
            .map(|line| serde_json::from_slice(&line).expect("one JSON line"))
            .collect()
    }
}

impl coorworker_envd::logs::LogSink for Capture {
    fn write_line(&self, line: &[u8]) {
        self.lines.lock().unwrap().push(line.to_vec());
    }
}

fn fixed_clock() -> Arc<dyn Fn() -> SystemTime + Send + Sync> {
    Arc::new(|| SystemTime::UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789))
}

fn fixed_logger(capture: &Arc<Capture>) -> Logger {
    Logger::with_clock(vec![capture.clone()], fixed_clock())
}

#[test]
fn logger_emits_zerolog_shaped_lines_with_go_json_escaping() {
    let capture = Capture::new();
    let logger = fixed_logger(&capture).with_str("logger", "envd");
    logger
        .info()
        .str("event_type", "process_start")
        .int("pid", 42)
        .str("command", "echo <hi> & bye")
        .dur_ms("request_timeout_ms", Duration::from_millis(1500))
        .msg("Process with pid 42 started");

    let lines = capture.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        std::str::from_utf8(&lines[0]).unwrap(),
        "{\"level\":\"info\",\"timestamp\":\"2023-11-14T22:13:20.123456789Z\",\
         \"logger\":\"envd\",\"event_type\":\"process_start\",\"pid\":42,\
         \"command\":\"echo \\u003chi\\u003e \\u0026 bye\",\"request_timeout_ms\":1500,\
         \"message\":\"Process with pid 42 started\"}\n",
        "zerolog order is level, context, fields, message; line ends with newline"
    );
}

#[test]
fn logger_levels_match_zerolog_names_and_whole_seconds_have_no_fraction() {
    let capture = Capture::new();
    let logger = Logger::with_clock(
        vec![capture.clone()],
        Arc::new(|| SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
    );
    logger.debug().msg("d");
    logger.info().msg("i");
    logger.warn().msg("w");
    logger.error().msg("e");
    let values = capture.values();
    assert_eq!(values[0]["level"], "debug");
    assert_eq!(values[1]["level"], "info");
    assert_eq!(values[2]["level"], "warn");
    assert_eq!(values[3]["level"], "error");
    assert_eq!(values[0]["timestamp"], "2023-11-14T22:13:20Z");
}

#[test]
fn rfc3339_nano_trims_trailing_fraction_zeros_like_go() {
    let time = SystemTime::UNIX_EPOCH + Duration::from_nanos(1_700_000_000_120_000_000);
    assert_eq!(logger::rfc3339_nano(time), "2023-11-14T22:13:20.12Z");
    let time = SystemTime::UNIX_EPOCH + Duration::from_nanos(1_700_000_000_000_000_001);
    assert_eq!(logger::rfc3339_nano(time), "2023-11-14T22:13:20.000000001Z");
}

// ---- process call site ------------------------------------------------------

fn runtime(capture: &Arc<Capture>) -> ProcessRuntime {
    assert!(
        !geteuid().is_root(),
        "process log tests require an ordinary user"
    );
    let account = Account {
        name: "log-test".into(),
        home: "/tmp".into(),
        uid: geteuid().as_raw(),
        gid: getegid().as_raw(),
        groups: getgroups()
            .unwrap()
            .into_iter()
            .map(|gid| gid.as_raw())
            .collect(),
    };
    ProcessRuntime::new(
        account,
        Defaults {
            user: "log-test".into(),
            workdir: Some("/tmp".into()),
            user_delivered: true,
        },
        Environment::default(),
        Some("/usr/bin:/bin".into()),
    )
    .unwrap()
    .with_process_logs(Arc::new(ProcessLogger::new(
        capture.logger().with_str("logger", "process"),
    )))
}

async fn drain(
    subscription: &mut coorworker_envd::process::Subscription,
) -> pb::process_event::EndEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
            .await
            .expect("process event timeout")
            .expect("process event");
        if let Some(pb::process_event::Event::End(end)) = event.event {
            return *end;
        }
    }
}

async fn wait_for_lines(capture: &Arc<Capture>, count: usize) -> Vec<Value> {
    for _ in 0..200 {
        let values = capture.values();
        if values.len() >= count {
            return values;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {count} log lines, got {}", capture.lines().len());
}

#[tokio::test]
async fn process_start_and_end_logs_carry_go_fields_and_byte_counts() {
    let capture = Capture::new();
    let runtime = runtime(&capture);
    let request: pb::StartRequest = serde_json::from_value(json!({
        "process": {
            "cmd": "sh",
            "args": ["-c", "printf hello; printf err >&2"]
        },
        "tag": "log-test"
    }))
    .unwrap();
    let mut subscription = runtime
        .start_with_operation_id(request, Some(Duration::from_millis(1500)), Some("7".into()))
        .await
        .unwrap();
    let end = drain(&mut subscription).await;
    assert_eq!(end.exit_code, 0);

    let values = wait_for_lines(&capture, 2).await;
    let start = &values[0];
    assert_eq!(start["logger"], "process");
    assert_eq!(start["operation_id"], "7");
    assert_eq!(start["event_type"], "process_start");
    assert_eq!(start["command"], "sh -c printf hello; printf err >&2");
    assert_eq!(start["request_timeout_ms"], 1500);
    assert_eq!(
        start["message"],
        format!("Process with pid {} started", start["pid"])
    );

    let end_log = &values[1];
    assert_eq!(end_log["event_type"], "process_end");
    assert_eq!(end_log["stdout_bytes"], 5);
    assert_eq!(end_log["stderr_bytes"], 3);
    assert_eq!(end_log["pty_bytes"], 0);
    assert_eq!(end_log["process_result"]["exited"], true);
    assert_eq!(end_log["process_result"]["status"], "exit status 0");
    assert!(
        end_log["process_result"].get("exit_code").is_none(),
        "protobuf omitempty drops a zero exit code"
    );
    assert_eq!(
        end_log["message"],
        format!("Process with pid {} ended", start["pid"])
    );
}

// ---- filesystem call sites --------------------------------------------------

fn username() -> String {
    assert!(
        !geteuid().is_root(),
        "RPC filesystem tests require an ordinary user"
    );
    User::from_uid(geteuid()).unwrap().unwrap().name
}

async fn init(app: &axum::Router, value: Value) {
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

async fn filesystem_call(app: &axum::Router, method: &str, value: Value) -> StatusCode {
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/filesystem.Filesystem/{method}"))
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let _ = to_bytes(response.into_body(), 65536).await;
    status
}

/// Same assembly shape as `main.rs`: one base logger (exporter sink), with the
/// env and filesystem contexts derived from it, wired through their own
/// builders (Go main.go:214-216). Reviewer instruction: filesystem lines must
/// be asserted through this production path — injecting the filesystem context
/// into `with_logger` masked a wiring gap where RPC lines carried `envd`.
fn production_daemon(capture: &Arc<Capture>) -> Daemon {
    let base = capture.logger();
    Daemon::new()
        .with_logger(base.with_str("logger", "envd"))
        .with_filesystem_logger(base.with_str("logger", "filesystem"))
}

/// Raw-text field order, because parsed JSON objects lose insertion order.
fn assert_field_order(line: &[u8], fields: &[&str]) {
    let text = std::str::from_utf8(line).unwrap();
    let mut last = 0;
    for field in fields {
        let needle = format!("\"{field}\":");
        let position = text
            .find(&needle)
            .unwrap_or_else(|| panic!("missing {field} in {text}"));
        assert!(position >= last, "field {field} out of order in {text}");
        last = position;
    }
}

#[tokio::test]
async fn filesystem_unary_logs_success_at_info_and_failure_at_error() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": dir.path()}),
    )
    .await;

    let status = filesystem_call(&app, "Stat", json!({"path": dir.path()})).await;
    assert_eq!(status, StatusCode::OK);
    let status = filesystem_call(&app, "Stat", json!({"path": dir.path().join("missing")})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let lines = wait_for_lines(&capture, 2).await;
    let raw_lines = capture.lines();
    let success = &lines[0];
    assert_eq!(success["logger"], "filesystem");
    // interceptor.go:63-64 logs `logger.Err(err)`, and zerolog's Err maps a
    // nil error to Info (log.go:373-382) — unlike the stream helpers, whose
    // getErrDebugLogEvent keeps Debug on success.
    assert_eq!(success["level"], "info");
    assert_eq!(success["method"], "POST /filesystem.Filesystem/Stat");
    assert_eq!(success["message"], "Filesystem stat");
    assert!(success["operation_id"]
        .as_str()
        .is_some_and(|id| id.parse::<i32>().is_ok()));
    assert!(success.get("error").is_none());
    assert_field_order(
        &raw_lines[0],
        &["logger", "method", "operation_id", "message"],
    );

    let failure = &lines[1];
    assert_eq!(failure["logger"], "filesystem");
    assert_eq!(failure["level"], "error");
    assert_eq!(failure["method"], "POST /filesystem.Filesystem/Stat");
    assert_eq!(failure["message"], "Filesystem stat");
    assert_eq!(failure["error_code"], 5, "connect CodeNotFound");
    assert!(failure["error"].as_str().unwrap().contains("not found"));
    // interceptor.go:63-64: logger.Err(err) appends `error` before method,
    // operation_id and error_code.
    assert_field_order(
        &raw_lines[1],
        &[
            "logger",
            "error",
            "method",
            "operation_id",
            "error_code",
            "message",
        ],
    );
}

async fn watch_frame(app: &axum::Router, path: &std::path::Path) -> axum::response::Response {
    let body = json!({"path": path, "recursive": false})
        .to_string()
        .into_bytes();
    let mut frame = vec![0u8];
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend(body);
    app.clone()
        .oneshot(
            Request::post("/filesystem.Filesystem/WatchDir")
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .body(Body::from(frame))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn watchdir_logs_the_server_stream_start_and_end() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": dir.path()}),
    )
    .await;

    let response = watch_frame(&app, dir.path()).await;
    assert_eq!(response.status(), StatusCode::OK);
    // The handler has emitted its start line by the time the response exists;
    // dropping the body ends the server stream.
    drop(response.into_body());

    let values = wait_for_lines(&capture, 2).await;
    assert_eq!(values[0]["level"], "debug");
    assert_eq!(
        values[0]["message"],
        "Filesystem watchDir (server stream start)"
    );
    assert_eq!(values[0]["logger"], "filesystem");
    assert!(values[0]["operation_id"]
        .as_str()
        .is_some_and(|id| id.parse::<i32>().is_ok()));
    assert_eq!(values[1]["logger"], "filesystem");
    assert_eq!(
        values[1]["message"],
        "Filesystem watchDir (server stream end)"
    );
}

#[tokio::test]
async fn watchdir_failure_logs_start_then_end_with_error_code() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": dir.path()}),
    )
    .await;

    // interceptor.go:100-110 emits the start line before the handler runs, so
    // a failed WatchDir still produces both lines.
    let response = watch_frame(&app, &dir.path().join("missing")).await;
    drop(response.into_body());

    let lines = wait_for_lines(&capture, 2).await;
    let raw_lines = capture.lines();
    assert_eq!(lines[0]["level"], "debug");
    assert_eq!(
        lines[0]["message"],
        "Filesystem watchDir (server stream start)"
    );
    assert_eq!(lines[0]["logger"], "filesystem");

    assert_eq!(lines[1]["level"], "error");
    assert_eq!(
        lines[1]["message"],
        "Filesystem watchDir (server stream end)"
    );
    assert!(lines[1]["error"].as_str().is_some());
    assert!(lines[1]["error_code"].is_i64());
    assert_field_order(
        &raw_lines[1],
        &[
            "logger",
            "error",
            "method",
            "operation_id",
            "error_code",
            "message",
        ],
    );
}

#[tokio::test]
async fn watchdir_client_disconnect_logs_an_error_end_line() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": dir.path()}),
    )
    .await;

    // Dropping a live server-stream body is the Rust shape of the client
    // disconnect that makes watch.go:88-89 return the bare ctx.Err(). The
    // interceptor logs that with error level and connect.CodeOf(ctx.Err()):
    // connect@v1.18.1 code.go:221-226 only unwraps *connect.Error, so the raw
    // context.Canceled logs as CodeUnknown (2), text "context canceled".
    let response = watch_frame(&app, dir.path()).await;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response.into_body());

    let values = wait_for_lines(&capture, 2).await;
    let end = &values[1];
    assert_eq!(end["level"], "error");
    assert_eq!(end["error"], "context canceled");
    assert_eq!(
        end["error_code"], 2,
        "CodeOf(context.Canceled) is CodeUnknown"
    );
    assert_eq!(end["message"], "Filesystem watchDir (server stream end)");
    assert!(end.get("response").is_none(), "interceptor.go:118-122");
}

// ---- filesystem interceptor payload fields ----------------------------------
//
// interceptor.go:72-82: the unary line always carries the request payload and,
// on success, the response payload (`response` null only when the handler
// returned a nil response). Payloads are zerolog Interface, i.e. encoding/json
// over the generated Go structs (filesystem.pb.go), with every field json-tag
// named and omitempty.

fn entry_assertions(entry: &Value, name: &str, path: &str, file_type: f64, user: &str) {
    assert_eq!(entry["name"], name);
    assert_eq!(entry["path"], path);
    assert_eq!(
        entry["type"], file_type,
        "FileType is the proto enum number (filesystem.pb.go:515)"
    );
    assert_eq!(
        entry["owner"], user,
        "owner lookup matches the entry helper (utils.go:117)"
    );
    assert!(
        entry["permissions"].as_str().is_some(),
        "permissions string is present for a real entry"
    );
    let modified = &entry["modified_time"];
    assert!(
        modified["seconds"].is_i64(),
        "modified_time is the nested timestamp struct, json-tag named (filesystem.pb.go:522, timestamp.pb.go:178)"
    );
    assert!(
        entry.get("is_symlink").is_none(),
        "false bool is omitted (filesystem.pb.go:530)"
    );
    assert!(
        entry.get("symlink_target").is_none(),
        "unset optional string is omitted (filesystem.pb.go:524)"
    );
    assert!(
        entry.get("metadata").is_none(),
        "empty map is omitted (filesystem.pb.go:528)"
    );
}

#[tokio::test]
async fn filesystem_stat_logs_go_shaped_request_and_response_payloads() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": dir.path()}),
    )
    .await;

    let path = dir.path().display().to_string();
    let status = filesystem_call(&app, "Stat", json!({"path": dir.path()})).await;
    assert_eq!(status, StatusCode::OK);

    let lines = wait_for_lines(&capture, 1).await;
    let raw_lines = capture.lines();
    let line = &lines[0];
    // Unary success is Info: interceptor.go:63-64 logger.Err(nil) is zerolog
    // Info (log.go:373-382).
    assert_eq!(line["level"], "info");
    assert_eq!(line["method"], "POST /filesystem.Filesystem/Stat");
    // interceptor.go:72-74: the request payload is present even on success.
    assert_eq!(line["request"], json!({"path": path}));
    let entry = &line["response"]["entry"];
    let name = dir.path().file_name().unwrap().to_str().unwrap();
    entry_assertions(entry, name, &path, 2.0, &username());
    // interceptor.go:76-78: the response payload follows the request on
    // success, and the message field comes last.
    assert_field_order(
        &raw_lines[0],
        &[
            "logger",
            "method",
            "operation_id",
            "request",
            "response",
            "message",
        ],
    );
}

#[tokio::test]
async fn filesystem_stat_failure_logs_request_without_response() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": dir.path()}),
    )
    .await;

    let missing = dir.path().join("missing");
    let status = filesystem_call(&app, "Stat", json!({"path": missing})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let lines = wait_for_lines(&capture, 1).await;
    let raw_lines = capture.lines();
    let line = &lines[0];
    assert_eq!(line["level"], "error");
    assert_eq!(line["error_code"], 5);
    assert_eq!(
        line["request"],
        json!({"path": missing.display().to_string()})
    );
    assert!(
        line.get("response").is_none(),
        "interceptor.go:76-82: no response field when err != nil"
    );
    assert_field_order(
        &raw_lines[0],
        &[
            "logger",
            "error",
            "method",
            "operation_id",
            "error_code",
            "request",
            "message",
        ],
    );
}

#[tokio::test]
async fn filesystem_make_dir_move_remove_log_go_shaped_payloads() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    let user = username();
    init(
        &app,
        json!({"defaultUser": user, "defaultWorkdir": dir.path()}),
    )
    .await;

    let made = dir.path().join("made");
    let status = filesystem_call(&app, "MakeDir", json!({"path": made})).await;
    assert_eq!(status, StatusCode::OK);
    let renamed = dir.path().join("renamed");
    let status = filesystem_call(
        &app,
        "Move",
        json!({"source": made, "destination": renamed}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // A file move pins the enum number for the file type (FILE_TYPE_FILE = 1).
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let moved_file = dir.path().join("moved-file");
    let status = filesystem_call(
        &app,
        "Move",
        json!({"source": file, "destination": moved_file}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let status = filesystem_call(&app, "Remove", json!({"path": moved_file})).await;
    assert_eq!(status, StatusCode::OK);

    let lines = wait_for_lines(&capture, 4).await;
    let (make, move_dir, move_file, remove) = (&lines[0], &lines[1], &lines[2], &lines[3]);
    assert_eq!(make["method"], "POST /filesystem.Filesystem/MakeDir");
    assert_eq!(make["request"], json!({"path": made.display().to_string()}));
    assert_eq!(
        make["response"]["entry"]["type"], 2,
        "the created entry is a directory"
    );
    assert_eq!(
        move_dir["request"],
        json!({
            "source": made.display().to_string(),
            "destination": renamed.display().to_string(),
        })
    );
    assert_eq!(
        move_dir["response"]["entry"]["path"],
        renamed.display().to_string()
    );
    assert_eq!(
        move_file["response"]["entry"]["type"], 1,
        "FILE_TYPE_FILE is the enum number 1"
    );
    assert_eq!(remove["method"], "POST /filesystem.Filesystem/Remove");
    assert_eq!(
        remove["request"],
        json!({"path": moved_file.display().to_string()})
    );
    assert_eq!(
        remove["response"],
        json!({}),
        "RemoveResponse has no fields (filesystem.pb.go:377)"
    );
}

#[tokio::test]
async fn filesystem_list_dir_logs_depth_and_the_empty_entries_omission() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    let user = username();
    init(
        &app,
        json!({"defaultUser": user, "defaultWorkdir": dir.path()}),
    )
    .await;

    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let status = filesystem_call(&app, "ListDir", json!({"path": dir.path()})).await;
    assert_eq!(status, StatusCode::OK);
    let status = filesystem_call(&app, "ListDir", json!({"path": dir.path(), "depth": 2})).await;
    assert_eq!(status, StatusCode::OK);
    let status = filesystem_call(&app, "ListDir", json!({"path": empty})).await;
    assert_eq!(status, StatusCode::OK);

    let lines = wait_for_lines(&capture, 3).await;
    let (default_depth, explicit_depth, empty_dir) = (&lines[0], &lines[1], &lines[2]);
    assert_eq!(
        default_depth["method"],
        "POST /filesystem.Filesystem/ListDir"
    );
    assert_eq!(
        default_depth["request"],
        json!({"path": dir.path().display().to_string()}),
        "depth 0 is omitted (filesystem.pb.go:655)"
    );
    let entries = default_depth["response"]["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(entries[0]["type"].is_i64(), "enum numbers in every entry");
    assert_eq!(
        explicit_depth["request"],
        json!({
            "path": dir.path().display().to_string(),
            "depth": 2,
        })
    );
    assert_eq!(
        empty_dir["response"],
        json!({}),
        "entries omitempty drops an empty listing (filesystem.pb.go:709)"
    );
}

#[tokio::test]
async fn filesystem_watcher_unary_methods_log_go_shaped_payloads() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    let user = username();
    init(
        &app,
        json!({"defaultUser": user, "defaultWorkdir": dir.path()}),
    )
    .await;

    let basic = format!("Basic {}", STANDARD.encode(format!("{user}:")));
    let (status, body) =
        watcher_call(&app, "CreateWatcher", json!({"path": dir.path()}), &basic).await;
    assert_eq!(status, StatusCode::OK);
    let watcher_id = body["watcherId"].as_str().unwrap().to_owned();
    let created = wait_for_lines(&capture, 1).await.remove(0);
    assert_eq!(
        created["method"],
        "POST /filesystem.Filesystem/CreateWatcher"
    );
    assert_eq!(
        created["request"],
        json!({"path": dir.path().display().to_string()}),
        "false bools are omitted (filesystem.pb.go:991-996)"
    );
    assert_eq!(
        created["response"],
        json!({"watcher_id": watcher_id}),
        "json tag is watcher_id, not watcherId (filesystem.pb.go:1064)"
    );

    // The drain starts empty: events omitempty renders {} (filesystem.pb.go:1158).
    let (status, _) = watcher_call(
        &app,
        "GetWatcherEvents",
        json!({"watcherId": watcher_id}),
        &basic,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    wait_for_lines(&capture, 2).await;
    // Then a file creation produces an event with the enum number and no entry
    // (include_entry false, filesystem.pb.go:835).
    std::fs::write(dir.path().join("created-file"), b"x").unwrap();
    let mut event_line = None;
    for _ in 0..30 {
        let before = capture.lines().len();
        let (status, body) = watcher_call(
            &app,
            "GetWatcherEvents",
            json!({"watcherId": watcher_id}),
            &basic,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if !body["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        // The line for this drain carries the events payload.
        let lines = wait_for_lines(&capture, before + 1).await;
        event_line = Some(lines.last().unwrap().clone());
        break;
    }
    let event_line = event_line.expect("a drain with events");
    let events = event_line["response"]["events"].as_array().unwrap();
    let created_event = events
        .iter()
        .find(|event| event["name"] == "created-file")
        .expect("the created-file event");
    assert_eq!(
        created_event["type"], 1,
        "EVENT_TYPE_CREATE is the enum number 1"
    );
    assert!(
        created_event.get("entry").is_none(),
        "entry omitted without include_entry (filesystem.pb.go:835)"
    );

    let before = capture.lines().len();
    let (status, _) = watcher_call(
        &app,
        "RemoveWatcher",
        json!({"watcherId": watcher_id}),
        &basic,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let lines = wait_for_lines(&capture, before + 1).await;
    let removed = lines.last().unwrap();
    assert_eq!(
        removed["method"],
        "POST /filesystem.Filesystem/RemoveWatcher"
    );
    assert_eq!(removed["request"], json!({"watcher_id": watcher_id}));
    assert_eq!(
        removed["response"],
        json!({}),
        "RemoveWatcherResponse has no fields (filesystem.pb.go:1247)"
    );

    // A failed watcher call keeps the request and drops the response.
    let before = capture.lines().len();
    let (status, _) = watcher_call(
        &app,
        "GetWatcherEvents",
        json!({"watcherId": "nope"}),
        &basic,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let lines = wait_for_lines(&capture, before + 1).await;
    let failure = lines.last().unwrap();
    assert_eq!(failure["level"], "error");
    assert_eq!(failure["request"], json!({"watcher_id": "nope"}));
    assert!(failure.get("response").is_none());
}

/// Watcher unary calls with the Basic auth the watcher endpoints require.
async fn watcher_call(
    app: &axum::Router,
    method: &str,
    value: Value,
    basic: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/filesystem.Filesystem/{method}"))
                .header("content-type", "application/json")
                .header("authorization", basic)
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).expect("Connect JSON response"),
    )
}

#[tokio::test]
async fn watchdir_start_line_carries_the_request_payload() {
    let capture = Capture::new();
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_filesystem(Arc::new(production_daemon(&capture)));
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": dir.path()}),
    )
    .await;

    let response = watch_frame(&app, dir.path()).await;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response.into_body());

    // interceptor.go:106-108: the server-stream start line carries the request
    // payload, like the process Start/Connect start lines do.
    let lines = wait_for_lines(&capture, 2).await;
    let raw_lines = capture.lines();
    assert_eq!(
        lines[0]["request"],
        json!({
            "path": dir.path().display().to_string(),
        }),
        "false recursive is omitted (filesystem.pb.go:757-762)"
    );
    assert_field_order(
        &raw_lines[0],
        &["logger", "method", "operation_id", "request", "message"],
    );
}

// ---- /freeze flush wiring ---------------------------------------------------

#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<Vec<u8>>>,
    replies: Mutex<VecDeque<u16>>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.requests.lock().unwrap().clone()
    }

    async fn wait_requests(&self, expected: usize) -> Vec<Vec<u8>> {
        for _ in 0..200 {
            let bodies = self.bodies();
            if bodies.len() >= expected {
                return bodies;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.bodies()
    }
}

impl HttpTransport for Recorder {
    fn request(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.body);
            let status = self.replies.lock().unwrap().pop_front().unwrap_or(204);
            Ok(HttpReply {
                status,
                headers: Vec::new(),
                body: Vec::new(),
            })
        })
    }
}

fn options() -> MmdsOpts {
    MmdsOpts {
        instance_id: "sandbox-id".into(),
        env_id: "template-id".into(),
        address: "http://collector.invalid".into(),
        access_token_hash: String::new(),
    }
}

async fn post(router: &axum::Router, uri: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(Request::post(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn freeze_with_a_report_budget_flushes_before_pausing() {
    let capture = Capture::new();
    let recorder = Recorder::new();
    let exporter = coorworker_envd::logs::HttpExporter::new(recorder.clone());
    exporter.store_opts(options());
    exporter
        .write(br#"{"message":"queued before pause"}"#)
        .unwrap();
    let daemon = Daemon::new()
        .with_logger(capture.logger().with_str("logger", "envd"))
        .with_log_flusher(exporter.clone());
    let router = app(Arc::new(daemon));

    let response = post(&router, "/freeze?maxWaitMs=50").await;
    assert_eq!(response.status(), StatusCode::OK);
    let bodies = recorder.wait_requests(1).await;
    assert_eq!(bodies.len(), 1);
    let payload: Value = serde_json::from_slice(&bodies[0]).unwrap();
    assert_eq!(payload["message"], "queued before pause");
    assert_eq!(payload["instanceID"], "sandbox-id");
    assert!(capture
        .values()
        .iter()
        .all(|line| line["message"] != "flush and purge logs before pause"));
}

#[tokio::test]
async fn freeze_legacy_without_a_budget_purges_and_warns_on_the_flush_error() {
    let capture = Capture::new();
    let recorder = Recorder::new();
    let exporter = coorworker_envd::logs::HttpExporter::new(recorder.clone());
    exporter.store_opts(options());
    exporter
        .write(br#"{"message":"dropped by the zero budget"}"#)
        .unwrap();
    let daemon = Daemon::new()
        .with_logger(capture.logger().with_str("logger", "envd"))
        .with_log_flusher(exporter.clone());
    let router = app(Arc::new(daemon));

    let response = post(&router, "/freeze").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(exporter.buffered(), (0, 0), "the exit purge still runs");
    assert_eq!(recorder.bodies().len(), 0, "zero budget sends nothing");
    let values = wait_for_lines(&capture, 1).await;
    assert_eq!(values[0]["level"], "warn");
    assert_eq!(values[0]["message"], "flush and purge logs before pause");
    assert!(values[0]["error"]
        .as_str()
        .unwrap()
        .contains("context deadline exceeded"));
    // init.go:497: PostFreeze derives its logger from a fresh AssignOperationID,
    // so every handler line — the flush Warn included — carries operation_id.
    assert!(values[0]["operation_id"]
        .as_str()
        .is_some_and(|id| id.parse::<i32>().is_ok()));
    assert_field_order(
        &capture.lines()[0],
        &["logger", "operation_id", "error", "message"],
    );
}

// ---- exporter sink wiring ---------------------------------------------------

#[tokio::test]
async fn the_production_logger_feeds_the_exporter_queue() {
    let recorder = Recorder::new();
    let exporter = coorworker_envd::logs::HttpExporter::new(recorder.clone());
    exporter.store_opts(options());
    let logger = Logger::with_clock(
        vec![exporter.clone()],
        Arc::new(|| SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
    );
    logger
        .info()
        .str("event_type", "process_end")
        .int("pid", 9)
        .msg("done");
    assert_eq!(exporter.buffered().0, 1);
    exporter
        .flush_and_purge(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    let bodies = recorder.bodies();
    assert_eq!(bodies.len(), 1);
    let payload: Value = serde_json::from_slice(&bodies[0]).unwrap();
    assert_eq!(payload["level"], "info");
    assert_eq!(payload["event_type"], "process_end");
    assert_eq!(payload["pid"], 9);
    assert_eq!(payload["message"], "done");
    assert_eq!(payload["instanceID"], "sandbox-id");
}
