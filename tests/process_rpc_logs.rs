//! Process RPC interceptor lines.
//!
//! Oracle: internal/logs/interceptor.go and the process service wrappers
//! (start.go:20, connect.go:16, input.go:50, service.go:162) at
//! 51083a83cc0903185d992725ea6bb33683814b29. Payload fields mirror
//! `encoding/json` over the generated Go structs
//! (internal/services/spec/process/process.pb.go): omitempty per json tag,
//! oneof wrappers under their Go field names (`Selector`, `Input`, `Stdin`),
//! enums as numbers, bytes as base64.

use std::{sync::Arc, time::Duration};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::{
    daemon::{app_with_execution, Daemon},
    exec_context::{Account, Defaults, Environment},
    logs::LogSink,
    process::ProcessRuntime,
};
use http_body_util::BodyExt;
use nix::unistd::{geteuid, User};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Captures every encoded line handed to the sink.
#[derive(Default)]
struct Capture {
    lines: std::sync::Mutex<Vec<Vec<u8>>>,
}

impl Capture {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn logger(self: &Arc<Self>) -> coorworker_envd::logs::Logger {
        coorworker_envd::logs::Logger::new(vec![self.clone()])
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

    fn with_message(&self, message: &str) -> Value {
        self.values()
            .into_iter()
            .find(|line| line["message"] == message)
            .unwrap_or_else(|| panic!("no line with message {message}: {:?}", self.values()))
    }
}

impl LogSink for Capture {
    fn write_line(&self, line: &[u8]) {
        self.lines.lock().unwrap().push(line.to_vec());
    }
}

/// The main.rs assembly shape: one base logger, per-service contexts through
/// their own builders (Go main.go:214-216, :243).
fn production_daemon(capture: &Arc<Capture>) -> Daemon {
    let base = capture.logger();
    Daemon::new()
        .with_logger(base.with_str("logger", "envd"))
        .with_filesystem_logger(base.with_str("logger", "filesystem"))
        .with_process_logger(base.with_str("logger", "process"))
}

fn runtime() -> ProcessRuntime {
    assert!(
        !nix::unistd::geteuid().is_root(),
        "HTTP process tests require an ordinary user"
    );
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    ProcessRuntime::new(
        Account {
            name: user.name,
            home: user.dir.to_string_lossy().into(),
            uid: user.uid.as_raw(),
            gid: user.gid.as_raw(),
            groups: nix::unistd::getgroups()
                .unwrap()
                .iter()
                .map(|g| g.as_raw())
                .collect(),
        },
        Defaults::default(),
        Environment::default(),
        std::env::var("PATH").ok(),
    )
    .unwrap()
}

fn username() -> String {
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

fn framed(payload: &Value) -> Vec<u8> {
    let body = payload.to_string();
    let mut frame = vec![0u8];
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body.as_bytes());
    frame
}

async fn unary(app: &axum::Router, method: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::post(format!("/process.Process/{method}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn server_stream(app: &axum::Router, method: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::post(format!("/process.Process/{method}"))
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .body(Body::from(framed(&body)))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn wait_for_lines(capture: &Arc<Capture>, count: usize) -> Vec<Value> {
    for _ in 0..300 {
        let values = capture.values();
        if values.len() >= count {
            return values;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {count} log lines, got {}", capture.lines().len());
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

fn assert_operation_id(line: &Value) {
    assert!(line["operation_id"]
        .as_str()
        .is_some_and(|id| id.parse::<i32>().is_ok()));
}

/// Spawns `/bin/cat` with stdin and returns (pid, response body) so the caller
/// controls the start stream's lifetime.
async fn start_cat(app: &axum::Router) -> (u64, axum::body::Body) {
    let response = server_stream(
        app,
        "Start",
        json!({"process":{"cmd":"/bin/cat"},"stdin":true}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let event: Value = serde_json::from_slice(&first[5..]).unwrap();
    (event["event"]["start"]["pid"].as_u64().unwrap(), body)
}

async fn kill(app: &axum::Router, pid: u64) {
    let response = unary(app, "SendSignal", json!({"process":{"pid":pid},"signal":9})).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn unary_list_logs_go_shaped_payloads() {
    let capture = Capture::new();
    let app = app_with_execution(Arc::new(production_daemon(&capture)), runtime());

    let response = unary(&app, "List", json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);

    let values = wait_for_lines(&capture, 1).await;
    let raw_lines = capture.lines();
    let line = &values[0];
    // zerolog Logger.Err(nil) is Info (log.go:373-382): the unary success line
    // is info, unlike the stream helpers' getErrDebugLogEvent Debug.
    assert_eq!(line["level"], "info");
    assert_eq!(line["logger"], "process");
    assert_eq!(line["method"], "POST /process.Process/List");
    assert_operation_id(line);
    assert_eq!(line["request"], json!({}), "ListRequest has no fields");
    assert_eq!(
        line["response"],
        json!({}),
        "ListResponse omits an empty processes field"
    );
    assert_eq!(line["message"], "Process list");
    assert!(line.get("error").is_none());
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
async fn unary_failure_logs_error_code_the_request_and_no_response() {
    let capture = Capture::new();
    let app = app_with_execution(Arc::new(production_daemon(&capture)), runtime());

    let response = unary(
        &app,
        "SendSignal",
        json!({"process":{"pid":99999999},"signal":15}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let values = wait_for_lines(&capture, 1).await;
    let raw_lines = capture.lines();
    let line = &values[0];
    assert_eq!(line["level"], "error");
    assert_eq!(line["logger"], "process");
    assert_eq!(line["method"], "POST /process.Process/SendSignal");
    assert_eq!(line["error_code"], 5, "connect CodeNotFound");
    assert!(line["error"].as_str().unwrap().contains("not found"));
    // The Go oneof wrapper structs carry no json tags, so the wrapper field
    // names `Selector`/`Pid` are Go-identifier-cased and always present
    // (process.pb.go:1304-1306), and the enum is a number.
    assert_eq!(
        line["request"],
        json!({"process":{"Selector":{"Pid":99999999}},"signal":15})
    );
    assert!(
        line.get("response").is_none(),
        "interceptor.go:78-83: no response field when err != nil"
    );
    assert_eq!(line["message"], "Process sendSignal");
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
async fn send_input_and_close_stdin_payloads_use_go_oneof_shapes() {
    let capture = Capture::new();
    let app = app_with_execution(Arc::new(production_daemon(&capture)), runtime());
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": "/tmp"}),
    )
    .await;

    let (pid, _start_body) = start_cat(&app).await;
    let response = unary(
        &app,
        "SendInput",
        json!({"process":{"pid":pid},"input":{"stdin":"aGk="}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = unary(&app, "CloseStdin", json!({"process":{"pid":pid}})).await;
    assert_eq!(response.status(), StatusCode::OK);

    wait_for_lines(&capture, 3).await;
    let raw_lines = capture.lines();

    let line = capture.with_message("Process sendInput");
    assert_eq!(line["level"], "info");
    assert_eq!(line["logger"], "process");
    assert_eq!(line["method"], "POST /process.Process/SendInput");
    assert_eq!(
        line["request"],
        json!({
            "process":{"Selector":{"Pid":pid}},
            "input":{"Input":{"Stdin":"aGk="}},
        }),
        "process.pb.go:867-869: the oneof wrapper has no json tag, so the Go \
         field names Input/Stdin are used and bytes are base64"
    );
    assert_eq!(line["response"], json!({}), "SendInputResponse is empty");

    let line = capture.with_message("Process closeStdin");
    assert_eq!(line["request"], json!({"process":{"Selector":{"Pid":pid}}}));
    assert_eq!(line["response"], json!({}));

    for raw in &raw_lines {
        let text = std::str::from_utf8(raw).unwrap();
        if text.contains("Process sendInput") {
            assert_field_order(
                raw,
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
    }

    kill(&app, pid).await;
}

#[tokio::test]
async fn start_stream_logs_the_request_and_a_null_response_at_the_end() {
    let capture = Capture::new();
    let app = app_with_execution(Arc::new(production_daemon(&capture)), runtime());
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": "/tmp"}),
    )
    .await;

    let response = server_stream(
        &app,
        "Start",
        json!({"process":{"cmd":"/bin/true"},"stdin":false}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    // Drain to natural completion: the Go handler then returns a still-live
    // context (start.go return ctx.Err() with no cause), so the end line is
    // Debug with response null.
    let mut body = response.into_body();
    while let Some(frame) = body.frame().await {
        frame.unwrap();
    }

    let values = wait_for_lines(&capture, 2).await;
    let raw_lines = capture.lines();
    let start = &values[0];
    assert_eq!(start["level"], "debug");
    assert_eq!(start["logger"], "process");
    assert_eq!(start["method"], "POST /process.Process/Start");
    assert_operation_id(start);
    assert_eq!(
        start["request"],
        json!({"process":{"cmd":"/bin/true"},"stdin":false}),
        "StartRequest json tags: args omitted when empty, stdin false is set"
    );
    assert_eq!(start["message"], "Process start (server stream start)");
    assert_field_order(
        &raw_lines[0],
        &["logger", "method", "operation_id", "request", "message"],
    );

    let end = &values[1];
    assert_eq!(end["level"], "debug");
    assert_eq!(end["response"], Value::Null, "interceptor.go:122");
    assert_eq!(end["message"], "Process start (server stream end)");
    assert!(end.get("error").is_none());
    assert_field_order(
        &raw_lines[1],
        &["logger", "method", "operation_id", "response", "message"],
    );
}

#[tokio::test]
async fn stream_input_logs_start_without_request_and_end_with_response() {
    let capture = Capture::new();
    let app = app_with_execution(Arc::new(production_daemon(&capture)), runtime());
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": "/tmp"}),
    )
    .await;

    let (pid, _start_body) = start_cat(&app).await;

    // interceptor.go:132-138: the client-stream start line carries only the
    // method and operation_id — no request payload.
    let mut body = framed(&json!({"start":{"process":{"pid":pid}}}));
    body.extend(framed(&json!({"data":{"input":{"stdin":"aGk="}}})));
    let response = app
        .clone()
        .oneshot(
            Request::post("/process.Process/StreamInput")
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let drained = to_bytes(response.into_body(), 65536).await.unwrap();
    assert!(!drained.is_empty());

    wait_for_lines(&capture, 3).await;
    let raw_lines = capture.lines();

    let start = capture.with_message("Process streamInput (client stream start)");
    assert_eq!(start["level"], "debug");
    assert_eq!(start["logger"], "process");
    assert_eq!(start["method"], "POST /process.Process/StreamInput");
    assert_operation_id(&start);
    assert!(
        start.get("request").is_none(),
        "LogClientStreamWithoutEvents logs no request payload"
    );

    let end = capture.with_message("Process streamInput (client stream end)");
    assert_eq!(end["level"], "debug");
    assert_eq!(
        end["response"],
        json!({}),
        "StreamInputResponse is empty (interceptor.go:145-150)"
    );
    assert!(end.get("error").is_none());

    for raw in &raw_lines {
        let text = std::str::from_utf8(raw).unwrap();
        if text.contains("client stream start") {
            assert_field_order(raw, &["logger", "method", "operation_id", "message"]);
        }
        if text.contains("client stream end") {
            assert_field_order(
                raw,
                &["logger", "method", "operation_id", "response", "message"],
            );
        }
    }

    kill(&app, pid).await;
}

#[tokio::test]
async fn connect_client_disconnect_logs_an_error_end_line() {
    let capture = Capture::new();
    let app = app_with_execution(Arc::new(production_daemon(&capture)), runtime());
    init(
        &app,
        json!({"defaultUser": username(), "defaultWorkdir": "/tmp"}),
    )
    .await;

    let (pid, _start_body) = start_cat(&app).await;

    let response = server_stream(&app, "Connect", json!({"process":{"pid":pid}})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(!first.is_empty());
    // Dropping a live server-stream body is the Rust shape of the client
    // disconnect that makes the Go handler return the bare ctx.Err()
    // (connect.go `case <-ctx.Done(): return ctx.Err()`).
    drop(body);

    wait_for_lines(&capture, 3).await;
    let end = capture.with_message("Process connect (server stream end)");
    assert_eq!(end["level"], "error");
    assert_eq!(end["logger"], "process");
    assert_eq!(end["error"], "context canceled");
    // connect.CodeOf(context.Canceled) is CodeUnknown: connect@v1.18.1
    // code.go:221-226 only unwraps *connect.Error, and the raw context error
    // the handler returns is never wrapped before the interceptor logs it.
    assert_eq!(end["error_code"], 2);
    assert!(end.get("response").is_none(), "interceptor.go:118-122");
    assert_eq!(end["message"], "Process connect (server stream end)");

    kill(&app, pid).await;
}
