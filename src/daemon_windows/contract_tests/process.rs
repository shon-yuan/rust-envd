//! Process Connect contract tests.
//!
//! These run in the `windows-test` job on `windows-latest`: every assertion is
//! what an SDK sees on the wire from the shared `crate::rpc::process` adapter
//! mounted over `WindowsProcessRuntime`. They were type-checked against the
//! Windows target on a Linux host, which has no MSVC linker, so they cannot
//! run there.
use super::*;
use crate::proto::process as pb;
use crate::proto::process::process_event::{data_event::Output, EndEvent, Event};
use http_body_util::BodyExt;

type HttpClient =
    Client<hyper_util::client::legacy::connect::HttpConnector, Full<hyper::body::Bytes>>;

/// A real loopback listener serving the production [`app`] surface (the same
/// runtime construction the daemon uses) with the contract token installed.
struct ProcessHttp {
    client: HttpClient,
    address: SocketAddr,
    _running: Running,
}

impl ProcessHttp {
    async fn new() -> Self {
        let running = initialized().await;
        let client = Client::builder(TokioExecutor::new()).build_http();
        Self {
            client,
            address: running.address,
            _running: running,
        }
    }

    fn url(&self, method: &str) -> String {
        format!("http://{}/process.Process/{method}", self.address)
    }

    async fn stream(&self, method: &str, value: Value) -> Events {
        self.stream_with(method, value, &[]).await
    }

    /// A server-stream request with the contract token. Extra headers may
    /// replace the credential with `Authorization: Basic ...`.
    async fn stream_with(&self, method: &str, value: Value, headers: &[(&str, &str)]) -> Events {
        let payload = value.to_string();
        let mut frame = vec![0];
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload.as_bytes());
        let mut request = Request::post(self.url(method))
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "1")
            .header("x-access-token", TOKEN);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .client
            .request(
                request
                    .body(Full::new(hyper::body::Bytes::from(frame)))
                    .unwrap(),
            )
            .await
            .expect("stream request");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/connect+json"));
        Events {
            body: response.into_body(),
            pending: Vec::new(),
        }
    }

    /// A stream carrying the contract token plus a Basic user. The daemon's
    /// token gate applies to every route but `/health`, `/init` and `/files`,
    /// so a Basic user alone would stop at the 401 before the username check.
    async fn stream_as(&self, method: &str, value: Value, user: &str) -> Events {
        let authorization = basic(user);
        let request = Request::post(self.url(method))
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "1")
            .header("x-access-token", TOKEN)
            .header("authorization", authorization);
        let payload = value.to_string();
        let mut frame = vec![0];
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload.as_bytes());
        let response = self
            .client
            .request(
                request
                    .body(Full::new(hyper::body::Bytes::from(frame)))
                    .unwrap(),
            )
            .await
            .expect("stream request");
        assert_eq!(response.status(), StatusCode::OK);
        Events {
            body: response.into_body(),
            pending: Vec::new(),
        }
    }

    async fn unary(&self, method: &str, value: Value) -> (u16, Value) {
        self.unary_with(method, value, &[]).await
    }

    async fn unary_with(
        &self,
        method: &str,
        value: Value,
        headers: &[(&str, &str)],
    ) -> (u16, Value) {
        unary_request(&self.client, self.url(method), value, headers).await
    }

    /// A unary request the test does not wait for, used to hold a PTY writer
    /// while another RPC must still be served.
    fn background_unary(
        &self,
        method: &str,
        value: Value,
    ) -> tokio::task::JoinHandle<(u16, Value)> {
        let client = self.client.clone();
        let url = self.url(method);
        tokio::spawn(async move { unary_request(&client, url, value, &[]).await })
    }
}

async fn unary_request(
    client: &HttpClient,
    url: String,
    value: Value,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let mut request = Request::post(url)
        .header("content-type", "application/json")
        .header("connect-protocol-version", "1")
        .header("x-access-token", TOKEN);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = client
        .request(
            request
                .body(Full::new(hyper::body::Bytes::from(value.to_string())))
                .unwrap(),
        )
        .await
        .expect("unary request");
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

struct Events {
    body: hyper::body::Incoming,
    pending: Vec<u8>,
}

impl Events {
    async fn envelope(&mut self) -> (u8, Value) {
        tokio::time::timeout(Duration::from_secs(15), async {
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
                let frame = self
                    .body
                    .frame()
                    .await
                    .expect("missing Connect envelope")
                    .unwrap();
                if let Ok(data) = frame.into_data() {
                    self.pending.extend_from_slice(&data);
                }
            }
        })
        .await
        .expect("HTTP event stalled")
    }

    async fn event(&mut self) -> pb::ProcessEvent {
        let (flags, value) = self.envelope().await;
        assert_eq!(flags, 0, "unexpected Connect trailer: {value}");
        serde_json::from_value(value["event"].clone()).unwrap()
    }

    /// The terminal error frame of a server stream that failed before its
    /// first event; Connect proxy errors arrive as HTTP 200 plus this frame.
    async fn stream_error(&mut self) -> Value {
        let (flags, value) = self.envelope().await;
        assert_eq!(flags, 2);
        value["error"].clone()
    }

    async fn pid(&mut self) -> u32 {
        match self.event().await.event.unwrap() {
            Event::Start(start) => start.pid,
            event => panic!("expected Start, got {event:?}"),
        }
    }

    /// Reads until End, returning stdout, stderr and the EndEvent.
    async fn finish(&mut self) -> (Vec<u8>, Vec<u8>, EndEvent) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            match self.event().await.event.unwrap() {
                Event::Data(data) => match data.output.unwrap() {
                    Output::Stdout(bytes) => stdout.extend(bytes),
                    Output::Stderr(bytes) => stderr.extend(bytes),
                    Output::Pty(bytes) => panic!("unexpected PTY bytes: {bytes:?}"),
                },
                Event::End(end) => {
                    assert_eq!(self.envelope().await, (2, json!({})));
                    return (stdout, stderr, *end);
                }
                Event::Start(_) => panic!("duplicate Start"),
                Event::Keepalive(_) => {}
            }
        }
    }

    /// Asserts a clean exit with `code` after the stream trailer. A signal
    /// death uses `code = -1` and `exited = false`, so callers assert those
    /// fields themselves.
    async fn end(&mut self, code: i32) -> EndEvent {
        loop {
            match self.event().await.event.unwrap() {
                Event::Data(data) => match data.output.unwrap() {
                    Output::Stdout(_) | Output::Stderr(_) => {}
                    Output::Pty(bytes) => panic!("unexpected PTY bytes: {bytes:?}"),
                },
                Event::End(end) => {
                    assert_eq!(end.exit_code, code);
                    assert_eq!(self.envelope().await, (2, json!({})));
                    return *end;
                }
                Event::Start(_) => panic!("duplicate Start"),
                Event::Keepalive(_) => {}
            }
        }
    }

    /// Reads to End on a PTY child, discarding terminal output. ConPTY echoes
    /// input and prints its own prompt, so the exact byte framing between the
    /// last asserted chunk and End is not part of the contract.
    async fn end_pty(&mut self, code: i32) -> EndEvent {
        loop {
            match self.event().await.event.unwrap() {
                Event::Data(_) => {}
                Event::End(end) => {
                    assert_eq!(end.exit_code, code);
                    assert_eq!(self.envelope().await, (2, json!({})));
                    return *end;
                }
                Event::Start(_) => panic!("duplicate Start"),
                Event::Keepalive(_) => {}
            }
        }
    }
}

/// Best-effort cleanup for a child a test left running.
struct Child(u32);

impl Drop for Child {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &self.0.to_string(), "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Polls `List` until the runtime has retired `pid`.
async fn retired(http: &ProcessHttp, pid: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = http.unary("List", json!({})).await;
        assert_eq!(status, 200);
        let listed: pb::ListResponse = serde_json::from_value(body).unwrap();
        if listed.processes.iter().all(|info| info.pid != pid) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "process {pid} was not retired"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn decode_events(bytes: &[u8]) -> Vec<(u8, Value)> {
    let mut result = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let len = u32::from_be_bytes(remaining[1..5].try_into().unwrap()) as usize;
        result.push((
            remaining[0],
            serde_json::from_slice(&remaining[5..5 + len]).unwrap(),
        ));
        remaining = &remaining[5 + len..];
    }
    result
}

fn frame(value: Value) -> Vec<u8> {
    let bytes = value.to_string().into_bytes();
    let mut frame = vec![0];
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend(bytes);
    frame
}

/// Removes ConPTY's VT control sequences so console text can be matched.
fn strip_ansi(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

#[tokio::test]
async fn process_start_stream_emits_start_data_end_for_a_real_child() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {
                    "cmd": "cmd.exe",
                    "args": ["/d", "/c", "echo out & echo err 1>&2 & exit 7"],
                },
                "stdin": false,
            }),
        )
        .await;
    let pid = started.pid().await;
    assert!(pid > 0);
    let (stdout, stderr, end) = started.finish().await;
    assert!(
        String::from_utf8_lossy(&stdout).contains("out"),
        "{stdout:?}"
    );
    assert!(
        String::from_utf8_lossy(&stderr).contains("err"),
        "{stderr:?}"
    );
    assert_eq!(end.exit_code, 7);
    assert!(end.exited);
    assert_eq!(end.status, "exit status 7");
    assert_eq!(end.error.as_deref(), Some("exit status 7"));

    let (status, list) = http.unary("List", json!({})).await;
    assert_eq!(status, 200);
    let listed: pb::ListResponse = serde_json::from_value(list).unwrap();
    assert!(listed.processes.iter().all(|info| info.pid != pid));
}

#[tokio::test]
async fn process_list_echoes_config_and_windows_pid() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {
                    "cmd": "cmd.exe",
                    "args": ["/d", "/c", "ping -n 60 127.0.0.1"],
                    "envs": {"ENVD_PROCESS_LIST": "1"},
                },
                "tag": "windows-listed",
            }),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    assert!(pid > 0);

    let (status, list) = http.unary("List", json!({})).await;
    assert_eq!(status, 200);
    let listed: pb::ListResponse = serde_json::from_value(list).unwrap();
    let info = listed
        .processes
        .iter()
        .find(|info| info.pid == pid)
        .expect("started process is live");
    assert_eq!(info.tag.as_deref(), Some("windows-listed"));
    let config = info.config.as_option().expect("config echo");
    assert_eq!(config.cmd, "cmd.exe");
    assert_eq!(
        config.args.iter().map(String::as_str).collect::<Vec<_>>(),
        ["/d", "/c", "ping -n 60 127.0.0.1"]
    );
    assert_eq!(
        config.envs.get("ENVD_PROCESS_LIST").map(String::as_str),
        Some("1")
    );

    let (status, body) = http
        .unary("SendSignal", json!({"process": {"pid": pid}, "signal": 9}))
        .await;
    assert_eq!(status, 200, "{body}");
    let _ = started.end(-1).await;
    child.0 = 0;
}

#[tokio::test]
async fn process_connect_attaches_without_output_replay_and_serves_retained_exit() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {
                    "cmd": "cmd.exe",
                    "args": ["/d", "/c", "echo before & ping -n 4 127.0.0.1 >nul & echo after"],
                },
                "tag": "windows-connect",
                "stdin": false,
            }),
        )
        .await;
    let pid = started.pid().await;
    // Consume the first marker, then detach the observer without killing the
    // child.
    let mut before = Vec::new();
    loop {
        match started.event().await.event.unwrap() {
            Event::Data(data) => match data.output.unwrap() {
                Output::Stdout(bytes) => {
                    before.extend(bytes);
                    if String::from_utf8_lossy(&before).contains("before") {
                        break;
                    }
                }
                output => panic!("unexpected output: {output:?}"),
            },
            event => panic!("expected output before detaching, got {event:?}"),
        }
    }
    drop(started);

    let mut attached = http
        .stream("Connect", json!({"process": {"pid": pid}}))
        .await;
    assert_eq!(attached.pid().await, pid);
    let (stdout, _, end) = attached.finish().await;
    let text = String::from_utf8_lossy(&stdout);
    assert!(text.contains("after"), "{text}");
    assert!(!text.contains("before"), "replayed prior output: {text}");
    assert_eq!(end.exit_code, 0);

    // The retained exit is reachable by pid and by tag, without replay.
    for selector in [json!({"pid": pid}), json!({"tag": "windows-connect"})] {
        let mut retained = http.stream("Connect", json!({"process": selector})).await;
        assert_eq!(retained.pid().await, pid);
        let (_, _, retained_end) = retained.finish().await;
        assert_eq!(retained_end, end);
    }
}

#[tokio::test]
async fn process_connect_and_send_signal_selector_refusals_use_getprocess_text() {
    let http = ProcessHttp::new().await;
    let missing = json!({
        "code": "not_found",
        "message": "process with pid 4294967295 not found",
    });
    let mut connect = http
        .stream("Connect", json!({"process": {"pid": u32::MAX}}))
        .await;
    assert_eq!(connect.stream_error().await, missing);
    for (method, value) in [
        (
            "SendSignal",
            json!({"process": {"pid": u32::MAX}, "signal": 9}),
        ),
        (
            "SendInput",
            json!({"process": {"pid": u32::MAX}, "input": {"stdin": "eA=="}}),
        ),
        ("Update", json!({"process": {"pid": u32::MAX}, "pty": {}})),
        ("CloseStdin", json!({"process": {"pid": u32::MAX}})),
    ] {
        let (status, error) = http.unary(method, value).await;
        assert_eq!(status, 404, "{method}: {error}");
        assert_eq!(error, missing, "{method}");
    }
    let (status, error) = http
        .unary(
            "CloseStdin",
            json!({"process": {"tag": "envd-windows-absent-tag"}}),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(
        error,
        json!({
            "code": "not_found",
            "message": "process with tag envd-windows-absent-tag not found",
        })
    );
    // An unset selector names the concrete proto type, code unimplemented.
    for (method, value) in [
        ("Update", json!({"process": {}})),
        ("SendSignal", json!({"process": {}, "signal": 9})),
    ] {
        let (status, error) = http.unary(method, value).await;
        assert_eq!(status, 501, "{method}: {error}");
        assert_eq!(
            error,
            json!({
                "code": "unimplemented",
                "message": "invalid input type *process.ProcessSelector",
            })
        );
    }
}

#[tokio::test]
async fn process_start_cwd_and_user_errors_match_linux_codes_and_text() {
    let http = ProcessHttp::new().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let missing = path_text(&directory.path().join("missing"));

    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe", "args": ["/d", "/c", "exit 0"], "cwd": missing},
                "stdin": false,
            }),
        )
        .await;
    let error = started.stream_error().await;
    assert_eq!(error["code"], "invalid_argument");
    assert_eq!(error["message"], format!("cwd '{missing}' does not exist"));

    // An unknown Basic user is refused by the shared middleware before any
    // child exists; the handler's `invalid user` text is only reachable for a
    // *default* account that fails lookup.
    let mut started = http
        .stream_as(
            "Start",
            json!({"process": {"cmd": "cmd.exe", "args": ["/d", "/c", "exit 0"]}}),
            "envd-no-such-user",
        )
        .await;
    let error = started.stream_error().await;
    assert_eq!(error["code"], "unauthenticated");
    assert_eq!(error["message"], "invalid username: 'envd-no-such-user'");

    let (status, list) = http.unary("List", json!({})).await;
    assert_eq!(status, 200);
    let listed: pb::ListResponse = serde_json::from_value(list).unwrap();
    assert!(listed.processes.is_empty());
}

#[tokio::test]
async fn process_send_input_stdin_and_pty_channel_rules() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let http = ProcessHttp::new().await;
    // `more` reads stdin and echoes it to stdout.
    let mut started = http
        .stream(
            "Start",
            json!({"process": {"cmd": "cmd.exe", "args": ["/d", "/c", "more"]}, "stdin": true}),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    assert_eq!(
        http.unary(
            "SendInput",
            json!({"process": {"pid": pid}, "input": {"stdin": STANDARD.encode("hello\r\n")}}),
        )
        .await,
        (200, json!({}))
    );
    let (status, error) = http
        .unary(
            "SendInput",
            json!({"process": {"pid": pid}, "input": {"pty": "eA=="}}),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(error["code"], "failed_precondition");
    assert_eq!(
        error["message"],
        "tty not assigned to process — input should be written to the stdin, not the tty"
    );
    let (status, error) = http
        .unary("SendInput", json!({"process": {"pid": pid}, "input": {}}))
        .await;
    assert_eq!(status, 501);
    assert_eq!(error["code"], "unimplemented");
    assert_eq!(error["message"], "invalid input type <nil>");

    assert_eq!(
        http.unary("CloseStdin", json!({"process": {"pid": pid}}))
            .await,
        (200, json!({}))
    );
    let (status, error) = http
        .unary(
            "SendInput",
            json!({"process": {"pid": pid}, "input": {"stdin": "eA=="}}),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(error["code"], "failed_precondition");
    assert_eq!(error["message"], "stdin not enabled or closed");
    let _ = started.end(0).await;
    child.0 = 0;
}

#[tokio::test]
async fn process_stream_input_orders_selection_and_reports_go_wrappers() {
    let http = ProcessHttp::new().await;
    // A child that outlives its closed stdin: `more` exits the moment the
    // CloseStdin below lands, and the daemon then reaps it, so the pid lookup
    // could answer not_found before the input rules this test pins ever run
    // (observed once in four native runs). ping never reads stdin, so the
    // errors below come from the handler's own state; `-n 3` keeps it alive
    // for the round trips and still lets the end() below see a clean exit.
    let mut started = http
        .stream(
            "Start",
            json!({"process": {"cmd": "cmd.exe", "args": ["/d", "/c", "ping -n 3 127.0.0.1"]}, "stdin": true}),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    assert_eq!(
        http.unary("CloseStdin", json!({"process": {"pid": pid}}))
            .await,
        (200, json!({}))
    );
    for (value, expected) in [
        (
            json!({}),
            json!({"code": "unimplemented", "message": "invalid input type <nil>"}),
        ),
        (
            json!({"stdin": "eA=="}),
            json!({
                "code": "failed_precondition",
                "message": "error writing to stdin: stdin not enabled or closed",
            }),
        ),
        (
            json!({"pty": "eA=="}),
            json!({
                "code": "failed_precondition",
                "message": "error writing to tty: tty not assigned to process — input should be written to the stdin, not the tty",
            }),
        ),
    ] {
        let mut body = frame(json!({"start": {"process": {"pid": pid}}}));
        body.extend(frame(json!({"data": {"input": value}})));
        let (status, _, bytes) = send(
            http.address,
            Method::POST,
            "/process.Process/StreamInput",
            &[
                ("x-access-token", TOKEN),
                ("content-type", "application/connect+json"),
                ("connect-protocol-version", "1"),
            ],
            &body,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(decode_events(&bytes), vec![(2, json!({"error": expected}))]);
    }
    // Data before Start is refused before selection.
    let body = frame(json!({"data": {"input": {"stdin": "eA=="}}}));
    let (_, _, bytes) = send(
        http.address,
        Method::POST,
        "/process.Process/StreamInput",
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/connect+json"),
            ("connect-protocol-version", "1"),
        ],
        &body,
    )
    .await;
    assert_eq!(
        decode_events(&bytes),
        vec![(
            2,
            json!({
                "error": {
                    "code": "failed_precondition",
                    "message": "process not selected",
                }
            })
        )]
    );
    let _ = started.end(0).await;
    child.0 = 0;
}

#[tokio::test]
async fn process_close_stdin_is_idempotent_and_pty_rejection_is_wrapped() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({"process": {"cmd": "cmd.exe", "args": ["/d", "/c", "more"]}, "stdin": true}),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    for _ in 0..2 {
        assert_eq!(
            http.unary("CloseStdin", json!({"process": {"pid": pid}}))
                .await,
            (200, json!({}))
        );
    }
    let (_, _, end) = started.finish().await;
    assert_eq!(end.exit_code, 0);
    child.0 = 0;

    let mut pty = http
        .stream(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe"},
                "pty": {"size": {"cols": 80, "rows": 24}},
                "stdin": false,
            }),
        )
        .await;
    let pty_pid = pty.pid().await;
    let mut pty_child = Child(pty_pid);
    let (status, error) = http
        .unary("CloseStdin", json!({"process": {"pid": pty_pid}}))
        .await;
    assert_eq!(status, 400);
    assert_eq!(error["code"], "failed_precondition");
    assert_eq!(
        error["message"],
        "error closing stdin: cannot close stdin for PTY process — send Ctrl+D (0x04) instead"
    );
    let _ = http
        .unary(
            "SendSignal",
            json!({"process": {"pid": pty_pid}, "signal": 9}),
        )
        .await;
    let _ = pty.end_pty(-1).await;
    pty_child.0 = 0;
}

#[tokio::test]
async fn process_update_resizes_conpty_and_keeps_noop_semantics() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({"process": {"cmd": "cmd.exe", "args": ["/d", "/c", "more"]}, "stdin": true}),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    assert_eq!(
        http.unary("Update", json!({"process": {"pid": pid}})).await,
        (200, json!({}))
    );
    let (status, error) = http
        .unary("Update", json!({"process": {"pid": pid}, "pty": {}}))
        .await;
    assert_eq!(status, 500);
    assert_eq!(error["code"], "internal");
    assert_eq!(
        error["message"],
        "error resizing tty: tty not assigned to process"
    );
    assert_eq!(
        http.unary("CloseStdin", json!({"process": {"pid": pid}}))
            .await,
        (200, json!({}))
    );
    let (_, _, end) = started.finish().await;
    assert_eq!(end.exit_code, 0);
    child.0 = 0;
    let (status, error) = http
        .unary("Update", json!({"process": {"pid": pid}, "pty": {}}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(error["code"], "not_found");

    let mut pty = http
        .stream(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe"},
                "pty": {"size": {"cols": 80, "rows": 24}},
                "stdin": false,
            }),
        )
        .await;
    let pty_pid = pty.pid().await;
    let mut pty_child = Child(pty_pid);
    assert_eq!(
        http.unary(
            "Update",
            json!({"process": {"pid": pty_pid}, "pty": {"size": {"cols": 100, "rows": 40}}}),
        )
        .await,
        (200, json!({}))
    );
    // `mode con` reports the console the child actually sees, so a no-op
    // resize cannot pass this.
    assert_eq!(
        http.unary(
            "SendInput",
            json!({
                "process": {"pid": pty_pid},
                "input": {"pty": STANDARD.encode("mode con\r\n")},
            }),
        )
        .await,
        (200, json!({}))
    );
    // `mode con` is localized (a zh-CN guest prints 行/列, not Lines/Columns),
    // so the report is read structurally: its first two `<label>: <number>`
    // pairs are the line and column counts on every Windows locale.
    let console_size = |report: &str| -> Vec<u32> {
        strip_ansi(report)
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter_map(|(_, rest)| rest.split_whitespace().next())
            .filter_map(|token| token.parse().ok())
            .collect()
    };
    let report = tokio::time::timeout(Duration::from_secs(15), async {
        let mut text = String::new();
        loop {
            match pty.event().await.event.unwrap() {
                Event::Data(data) => match data.output.unwrap() {
                    Output::Pty(bytes) => {
                        text.push_str(&String::from_utf8_lossy(&bytes));
                        if console_size(&text).len() >= 2 {
                            return text;
                        }
                    }
                    output => panic!("non-PTY output on a PTY child: {output:?}"),
                },
                Event::End(end) => panic!("PTY child exited before `mode con`: {end:?}"),
                Event::Start(_) => panic!("duplicate Start"),
                Event::Keepalive(_) => {}
            }
        }
    })
    .await
    .expect("`mode con` reports the resized console");
    assert_eq!(
        console_size(&report)[..2],
        [40, 100],
        "resized console report: {report:?}"
    );
    let _ = http
        .unary(
            "SendSignal",
            json!({"process": {"pid": pty_pid}, "signal": 9}),
        )
        .await;
    let _ = pty.end_pty(-1).await;
    pty_child.0 = 0;
}

/// A PTY child that writes more than the reader channel can hold before it
/// exits must still reach `End`: the supervisor closes the pseudoconsole off
/// the executor while the pump keeps draining.
#[tokio::test]
async fn process_pty_large_output_before_close_reaches_end() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {
                    "cmd": "cmd.exe",
                    "args": [
                        "/d",
                        "/c",
                        "for /L %i in (1,1,40000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                    ],
                },
                "pty": {"size": {"cols": 200, "rows": 50}},
                "stdin": false,
            }),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    let (bytes, end) = tokio::time::timeout(Duration::from_secs(120), async {
        let mut bytes = 0usize;
        loop {
            match started.event().await.event.unwrap() {
                Event::Data(data) => match data.output.unwrap() {
                    Output::Pty(chunk) => bytes += chunk.len(),
                    output => panic!("non-PTY output on a PTY child: {output:?}"),
                },
                Event::End(end) => return (bytes, *end),
                Event::Start(_) => panic!("duplicate Start"),
                Event::Keepalive(_) => {}
            }
        }
    })
    .await
    .expect("the PTY stream terminates");
    assert!(
        bytes > 2 * 1024 * 1024,
        "expected more than 2 MiB of PTY output, saw {bytes}"
    );
    assert_eq!(end.exit_code, 0);
    child.0 = 0;
}

/// SIGTERM must be served while another PTY write holds the input path: the
/// grace window starts at acceptance and the break delivery cannot block the
/// supervisor.
#[tokio::test]
async fn process_send_signal_term_is_not_wedged_by_a_blocked_pty_writer() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe", "args": ["/d", "/c", "ping -n 60 127.0.0.1 >nul"]},
                "pty": {"size": {"cols": 80, "rows": 24}},
                "stdin": false,
            }),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    // The child does not read its console while ping runs, so this write
    // fills the input pipe and holds the writer.
    let huge = STANDARD.encode(vec![b'A'; 1024 * 1024]);
    let writer = http.background_unary(
        "SendInput",
        json!({"process": {"pid": pid}, "input": {"pty": huge}}),
    );
    // Let the write reach the blocked state.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !writer.is_finished(),
        "the 1 MiB PTY write must still be blocked behind the non-reading child"
    );
    let signal = tokio::time::timeout(
        Duration::from_secs(3),
        http.unary("SendSignal", json!({"process": {"pid": pid}, "signal": 15})),
    )
    .await
    .expect("SIGTERM must not wait for the blocked PTY writer");
    assert_eq!(signal.0, 200, "{}", signal.1);
    let end = tokio::time::timeout(Duration::from_secs(15), started.end_pty(-1))
        .await
        .expect("the grace window terminates the child");
    assert!(!end.exited);
    assert_eq!(end.status, "signal: terminated");
    // The blocked request fails once the console closes; its outcome is not
    // part of the contract.
    let _ = writer.await;
    child.0 = 0;
}

/// `connect-timeout-ms` bounds execution: at the negotiated deadline the
/// response stream is terminated and the supervisor kills and retires the
/// child.
#[tokio::test]
async fn process_execution_deadline_kills_and_retires_the_child() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream_with(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe", "args": ["/d", "/c", "ping -n 60 127.0.0.1 >nul"]},
            }),
            // The same header bounds both the execution and the request, so it
            // must stay well above a cold CreateProcessW plus the Job
            // assignments; the child (`ping -n 60`) outlives it.
            &[("connect-timeout-ms", "1000")],
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    // Declared deviation, shared with the Linux transport: the daemon bounds
    // server streams by the negotiated deadline and ends the body with
    // `deadline_exceeded` ("request deadline exceeded while streaming"). The
    // oracle's handler returns its cancelled context at the same point, which
    // connect-go renders as code `canceled` / "context canceled"; no `End`
    // event reaches the client in either implementation.
    let error = tokio::time::timeout(Duration::from_secs(10), started.stream_error())
        .await
        .expect("the execution deadline ends the stream");
    assert_eq!(error["code"], "deadline_exceeded");
    // The process deadline is armed from the same header inside the runtime,
    // so the child is killed and retired even though the subscription is
    // already gone.
    retired(&http, pid).await;
    child.0 = 0;
}

/// Starts a long child, delivers `signal`, and asserts the Linux terminal
/// synthesis for it.
async fn send_signal_once(signal: i32, expected: &str) {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {
                    "cmd": "cmd.exe",
                    "args": ["/d", "/c", "ping -n 60 127.0.0.1"],
                },
                "stdin": false,
            }),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    let (status, body) = http
        .unary(
            "SendSignal",
            json!({"process": {"pid": pid}, "signal": signal}),
        )
        .await;
    assert_eq!(status, 200, "signal {signal}: {body}");
    // SIGTERM first delivers Ctrl-Break and only hard-kills after the grace
    // window, so the terminal event needs a longer budget.
    let end = started.end(-1).await;
    assert!(!end.exited);
    assert_eq!(end.status, expected);
    assert_eq!(end.error.as_deref(), Some(expected));
    child.0 = 0;
}

#[tokio::test]
async fn process_send_signal_kill_synthesizes_the_linux_kill_end_event() {
    send_signal_once(9, "signal: killed").await;
}

#[tokio::test]
async fn process_send_signal_term_synthesizes_the_linux_term_end_event() {
    send_signal_once(15, "signal: terminated").await;
}

#[tokio::test]
async fn process_invalid_signal_values_name_the_proto_enum() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({"process": {"cmd": "cmd.exe", "args": ["/d", "/c", "ping -n 30 127.0.0.1"]}}),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    for (signal, expected) in [
        (0, "invalid signal: SIGNAL_UNSPECIFIED"),
        (99, "invalid signal: 99"),
    ] {
        let (status, error) = http
            .unary(
                "SendSignal",
                json!({"process": {"pid": pid}, "signal": signal}),
            )
            .await;
        assert_eq!(status, 501);
        assert_eq!(error["code"], "unimplemented");
        assert_eq!(error["message"], expected);
    }
    // The process was never signalled.
    let listed: pb::ListResponse =
        serde_json::from_value(http.unary("List", json!({})).await.1).unwrap();
    assert!(listed.processes.iter().any(|info| info.pid == pid));
    let _ = http
        .unary("SendSignal", json!({"process": {"pid": pid}, "signal": 9}))
        .await;
    let _ = started.end(-1).await;
    child.0 = 0;
}

#[tokio::test]
async fn process_keepalive_uses_the_request_header_interval() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream_with(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe", "args": ["/d", "/c", "ping -n 60 127.0.0.1 >nul"]},
                "stdin": false,
            }),
            &[("keepalive-ping-interval", "1")],
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    let (flags, value) = started.envelope().await;
    assert_eq!(flags, 0);
    assert_eq!(value, json!({"event": {"keepalive": {}}}));
    let _ = http
        .unary("SendSignal", json!({"process": {"pid": pid}, "signal": 9}))
        .await;
    let _ = started.end(-1).await;
    child.0 = 0;
}

#[tokio::test]
async fn process_start_pty_reports_pty_events_only_and_closes_conpty() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe"},
                "pty": {"size": {"cols": 80, "rows": 24}},
                "stdin": false,
            }),
        )
        .await;
    let pid = started.pid().await;
    let mut child = Child(pid);
    assert_eq!(
        http.unary(
            "SendInput",
            json!({"process": {"pid": pid}, "input": {"pty": STANDARD.encode("exit\r\n")}}),
        )
        .await,
        (200, json!({}))
    );
    let mut saw_data = false;
    loop {
        match started.event().await.event.unwrap() {
            Event::Data(data) => match data.output.unwrap() {
                Output::Pty(bytes) => {
                    assert!(!bytes.is_empty());
                    saw_data = true;
                }
                output => panic!("non-PTY output on a PTY child: {output:?}"),
            },
            Event::End(end) => {
                assert_eq!(end.exit_code, 0);
                break;
            }
            Event::Start(_) => panic!("duplicate Start"),
            Event::Keepalive(_) => {}
        }
    }
    assert!(saw_data, "ConPTY echoes the prompt");
    assert_eq!(started.envelope().await, (2, json!({})));
    child.0 = 0;

    let mut retained = http
        .stream("Connect", json!({"process": {"pid": pid}}))
        .await;
    assert_eq!(retained.pid().await, pid);
    let (_, _, end) = retained.finish().await;
    assert_eq!(end.exit_code, 0);
}

#[tokio::test]
async fn process_unknown_basic_user_is_unauthenticated_before_spawn() {
    let http = ProcessHttp::new().await;
    let mut started = http
        .stream_as(
            "Start",
            json!({"process": {"cmd": "cmd.exe", "args": ["/d", "/c", "exit 0"]}}),
            "envd-unknown-user",
        )
        .await;
    let error = started.stream_error().await;
    assert_eq!(error["code"], "unauthenticated");
    assert_eq!(error["message"], "invalid username: 'envd-unknown-user'");
    let listed: pb::ListResponse =
        serde_json::from_value(http.unary("List", json!({})).await.1).unwrap();
    assert!(listed.processes.is_empty());
}

#[tokio::test]
async fn process_non_current_user_without_credentials_is_failed_precondition() {
    let http = ProcessHttp::new().await;
    let own = crate::platform::windows::account::current_user_name();
    // Both accounts are built into every Windows installation.
    let other = if own.eq_ignore_ascii_case("NETWORK SERVICE") {
        "LOCAL SERVICE"
    } else {
        "NETWORK SERVICE"
    };
    let directory = tempfile::tempdir().expect("tempdir");
    let mut started = http
        .stream_as(
            "Start",
            json!({
                "process": {
                    "cmd": "cmd.exe",
                    "args": ["/d", "/c", "exit 0"],
                    "cwd": path_text(directory.path()),
                },
            }),
            other,
        )
        .await;
    let error = started.stream_error().await;
    assert_eq!(error["code"], "failed_precondition", "{error}");
    assert_eq!(
        error["message"],
        format!(
            "cannot start process as user '{other}': the daemon can only run processes as its own account"
        )
    );
    let listed: pb::ListResponse =
        serde_json::from_value(http.unary("List", json!({})).await.1).unwrap();
    assert!(listed.processes.is_empty());
}

#[tokio::test]
async fn process_job_kill_on_close_reaps_descendants() {
    let http = ProcessHttp::new().await;
    let directory = tempfile::tempdir().expect("tempdir");
    // A batch file avoids embedding quotes in the API command: `cmd /c`
    // parses its own command line and does not honour the `\"` escaping that
    // the spawn path produces for CommandLineToArgvW consumers.
    let batch = directory.path().join("descendant.bat");
    std::fs::write(
        &batch,
        concat!(
            "@echo off\r\n",
            "cd /d \"%~dp0\"\r\n",
            // The detached descendant proves it ran, then lingers without
            // reading stdin. `start` returns as soon as the process exists.
            "start /b cmd /c \"echo started>descendant-started & ping -n 3 127.0.0.1 >nul & echo alive>descendant-alive\"\r\n",
            // Wait (bounded) until the descendant is observably running, so
            // the test cannot pass because the descendant never started.
            "for /L %%i in (1,1,20) do (\r\n",
            "  if exist descendant-started goto :done\r\n",
            "  ping -n 2 127.0.0.1 >nul\r\n",
            ")\r\n",
            ":done\r\n",
            "exit /b 0\r\n",
        ),
    )
    .expect("write the descendant script");
    let started_marker = directory.path().join("descendant-started");
    let marker = directory.path().join("descendant-alive");
    let mut started = http
        .stream(
            "Start",
            json!({
                "process": {"cmd": "cmd.exe", "args": ["/d", "/c", path_text(&batch)]},
                "stdin": false,
            }),
        )
        .await;
    started.pid().await;
    // The direct child exits once the descendant is running. The descendant
    // holds the inherited stdio handles, so `End` must not wait for it: the
    // per-process Job is terminated as soon as the direct child is reaped
    // (KILL_ON_JOB_CLOSE is only the backstop).
    let (_, _, end) = tokio::time::timeout(Duration::from_secs(15), started.finish())
        .await
        .expect("a descendant holding the stdio pipes must not withhold End");
    assert_eq!(end.exit_code, 0);
    assert_eq!(end.status, "exit status 0");
    assert!(
        started_marker.exists(),
        "the descendant must have started before the direct child exited"
    );
    // ping -n 3 is roughly two seconds; the grandchild is gone long before it
    // could write the marker.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !marker.exists(),
        "the per-process Job must reap the descendant before it writes {}",
        marker.display()
    );
}
