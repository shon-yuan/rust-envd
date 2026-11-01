use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use coorworker_envd::{
    daemon::{app_with_execution, Daemon},
    exec_context::{Account, Defaults, Environment},
    process::ProcessRuntime,
};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

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

#[tokio::test(start_paused = true)]
async fn idle_process_stream_emits_keepalive_at_requested_interval() {
    use http_body_util::BodyExt;
    let app = app_with_execution(Arc::new(Daemon::new()), runtime());
    assert!(
        !nix::unistd::geteuid().is_root(),
        "HTTP process tests require an ordinary user"
    );
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap()
        .name;
    let init = json!({"defaultUser":user,"defaultWorkdir":"/tmp"});
    app.clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(init.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let payload = json!({"process":{"cmd":"/bin/cat"},"stdin":true}).to_string();
    let mut frame = vec![0];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload.as_bytes());
    let response = app
        .clone()
        .oneshot(
            Request::post("/process.Process/Start")
                .header("content-type", "application/connect+json")
                .header("keepalive-ping-interval", "1")
                .body(Body::from(frame))
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let event: serde_json::Value = serde_json::from_slice(&first[5..]).unwrap();
    let pid = event["event"]["start"]["pid"].as_u64().unwrap();
    let next = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame()).await;
    let close = json!({"process":{"pid":pid}});
    app.oneshot(
        Request::post("/process.Process/CloseStdin")
            .header("content-type", "application/json")
            .body(Body::from(close.to_string()))
            .unwrap(),
    )
    .await
    .unwrap();
    let next = next
        .expect("idle stream must emit keepalive")
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    let event: serde_json::Value = serde_json::from_slice(&next[5..]).unwrap();
    assert_eq!(event, json!({"event":{"keepalive":{}}}));
}
#[tokio::test]
async fn invalid_process_timeout_never_creates_a_child() {
    let app = app_with_execution(Arc::new(Daemon::new()), runtime());
    let payload = json!({"process":{"cmd":"/bin/true"}}).to_string();
    let mut frame = vec![0];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload.as_bytes());
    for timeout in ["abc", "18446744073709551616"] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/process.Process/Start")
                    .header("content-type", "application/connect+json")
                    .header("connect-protocol-version", "1")
                    .header("connect-timeout-ms", timeout)
                    .body(Body::from(frame.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("invalid_argument"));
        assert!(!String::from_utf8_lossy(&body).contains("\"pid\""));
    }
}
#[tokio::test]
async fn process_rpc_executes_real_child_with_init_defaults_and_lists_retained_exit() {
    let app = app_with_execution(Arc::new(Daemon::new()), runtime());
    assert!(
        !nix::unistd::geteuid().is_root(),
        "HTTP process tests require an ordinary user"
    );
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap()
        .name;
    let init = json!({"defaultUser":user,"defaultWorkdir":"/tmp","envVars":{"RPC_VALUE":"雪"}});
    assert_eq!(
        app.clone()
            .oneshot(
                Request::post("/init")
                    .body(Body::from(init.to_string()))
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        204
    );
    let start = json!({"process":{"cmd":"/bin/sh","args":["-c","printf '%s' \"$RPC_VALUE\"; exit 7"]},"stdin":false});
    let payload = start.to_string();
    let mut framed = vec![0];
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(payload.as_bytes());
    let invalid = app
        .clone()
        .oneshot(
            Request::post("/process.Process/Start")
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .header("authorization", "Basic Om5vLXVzZXI=")
                .body(Body::from(framed.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    let invalid_body = to_bytes(invalid.into_body(), 65536).await.unwrap();
    assert!(String::from_utf8_lossy(&invalid_body).contains("unauthenticated"));
    let response = app
        .clone()
        .oneshot(
            Request::post("/process.Process/Start")
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .body(Body::from(framed))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        to_bytes(response.into_body(), 65536),
    )
    .await
    .unwrap()
    .unwrap();
    let mut offset = 0;
    let mut events = Vec::new();
    while offset < bytes.len() {
        let flags = bytes[offset];
        let len = u32::from_be_bytes(bytes[offset + 1..offset + 5].try_into().unwrap()) as usize;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes[offset + 5..offset + 5 + len]).unwrap();
        if flags == 0 {
            events.push(value);
        } else {
            assert_eq!(value, json!({}));
        }
        offset += 5 + len;
    }
    assert!(events[0]["event"]["start"]["pid"].as_u64().unwrap() > 0);
    assert!(events
        .iter()
        .any(|v| v["event"]["data"]["stdout"] == "6Zuq"));
    assert_eq!(events.last().unwrap()["event"]["end"]["exitCode"], 7);
    let list = app
        .oneshot(
            Request::post("/process.Process/List")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(list.into_body(), 65536).await.unwrap()).unwrap();
    assert!(value
        .get("processes")
        .is_none_or(|v| v.as_array().unwrap().is_empty()));
}

// Use a real loopback HTTP connection so stream cancellation and incremental
// Connect framing are exercised together with ordinary-user PTY children.
type HttpClient = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    http_body_util::Full<bytes::Bytes>,
>;
struct HttpProcess {
    client: HttpClient,
    origin: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for HttpProcess {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl HttpProcess {
    async fn new() -> Self {
        let app = app_with_execution(Arc::new(Daemon::new()), runtime());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http();
        let http = Self {
            client,
            origin,
            server,
        };
        let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .unwrap()
            .unwrap()
            .name;
        let init = http
            .request(
                "/init",
                json!({"defaultUser":user,"defaultWorkdir":"/tmp"}),
                false,
            )
            .await;
        assert_eq!(init.status(), 204);
        http_body_util::BodyExt::collect(init.into_body())
            .await
            .unwrap();
        http
    }
    async fn request(
        &self,
        path: &str,
        value: serde_json::Value,
        streaming: bool,
    ) -> hyper::Response<hyper::body::Incoming> {
        let payload = value.to_string();
        let body = if streaming {
            let mut frame = vec![0];
            frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            frame.extend_from_slice(payload.as_bytes());
            frame
        } else {
            payload.into_bytes()
        };
        let request = Request::post(format!("{}{path}", self.origin))
            .header(
                "content-type",
                if streaming {
                    "application/connect+json"
                } else {
                    "application/json"
                },
            )
            .header("connect-protocol-version", "1")
            .header("connect-timeout-ms", "4000")
            .body(http_body_util::Full::new(bytes::Bytes::from(body)))
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.client.request(request),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn unary(&self, method: &str, value: serde_json::Value) -> (u16, serde_json::Value) {
        use http_body_util::BodyExt;
        let response = self
            .request(&format!("/process.Process/{method}"), value, false)
            .await;
        let status = response.status().as_u16();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn stream(&self, method: &str, value: serde_json::Value) -> HttpEvents {
        let response = self
            .request(&format!("/process.Process/{method}"), value, true)
            .await;
        assert_eq!(response.status(), 200);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/connect+json"));
        HttpEvents {
            body: response.into_body(),
            pending: Vec::new(),
        }
    }
}
struct HttpEvents {
    body: hyper::body::Incoming,
    pending: Vec<u8>,
}
impl HttpEvents {
    async fn envelope(&mut self) -> (u8, serde_json::Value) {
        use http_body_util::BodyExt;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
    async fn event(&mut self) -> coorworker_envd::proto::process::ProcessEvent {
        let (flags, value) = self.envelope().await;
        assert_eq!(flags, 0, "unexpected Connect trailer: {value}");
        serde_json::from_value(value["event"].clone()).unwrap()
    }
    async fn pid(&mut self) -> u32 {
        match self.event().await.event.unwrap() {
            coorworker_envd::proto::process::process_event::Event::Start(start) => start.pid,
            event => panic!("expected Start, got {event:?}"),
        }
    }
    async fn pty_until(&mut self, marker: &[u8]) -> Vec<u8> {
        use coorworker_envd::proto::process::process_event::{data_event::Output, Event};
        let mut bytes = Vec::new();
        loop {
            match self.event().await.event.unwrap() {
                Event::Data(data) => match data.output.unwrap() {
                    Output::Pty(data) => bytes.extend(data),
                    output => panic!("wrong PTY output channel: {output:?}"),
                },
                event => panic!("expected PTY output, got {event:?}"),
            }
            if bytes.windows(marker.len()).any(|window| window == marker) {
                return bytes;
            }
        }
    }
    async fn end(&mut self, expected_code: i32) {
        use coorworker_envd::proto::process::process_event::Event;
        match self.event().await.event.unwrap() {
            Event::End(end) => {
                assert_eq!(end.exit_code, expected_code);
                assert!(end.exited);
            }
            event => panic!("expected End without replay, got {event:?}"),
        }
        assert_eq!(self.envelope().await, (2, json!({})));
    }
}
struct HttpChild(u32);
impl Drop for HttpChild {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(self.0 as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[tokio::test]
async fn http_pty_start_input_resize_close_and_reconnect_match_contract() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let http = HttpProcess::new().await;
    let mut start = http.stream("Start", json!({
        "process":{"cmd":"/bin/sh","args":["-c", "test -t 0 && test -t 1 && test -t 2 && : </dev/tty || exit 91; stty -echo; stty size; printf 'stderr\\n' >&2; printf ready; read line; stty size; printf 'reply:%s\\n' \"$line\"; printf wait; exec cat"]},
        "pty":{"size":{"cols":80,"rows":24}}, "stdin":false
    })).await;
    let pid = start.pid().await;
    let mut child = HttpChild(pid);
    assert_eq!(start.pty_until(b"ready").await, b"24 80\r\nstderr\r\nready");
    assert_eq!(
        http.unary("Update", json!({"process":{"pid":pid}})).await,
        (200, json!({}))
    );
    assert_eq!(
        http.unary(
            "Update",
            json!({"process":{"pid":pid},"pty":{"size":{"cols":65647,"rows":65573}}})
        )
        .await,
        (200, json!({}))
    );
    for (method, payload) in [
        ("CloseStdin", json!({"process":{"pid":pid}})),
        (
            "SendInput",
            json!({"process":{"pid":pid},"input":{"stdin":"eA=="}}),
        ),
    ] {
        let (status, error) = http.unary(method, payload).await;
        assert_eq!(status, 400);
        assert_eq!(error["code"], "failed_precondition");
        if method == "CloseStdin" {
            assert_eq!(error["message"], "error closing stdin: cannot close stdin for PTY process — send Ctrl+D (0x04) instead");
        }
    }
    drop(start);
    let mut connected = http.stream("Connect", json!({"process":{"pid":pid}})).await;
    assert_eq!(connected.pid().await, pid);
    let bytes = "literal 雪 ; $()\n";
    assert_eq!(
        http.unary(
            "SendInput",
            json!({"process":{"pid":pid},"input":{"pty":STANDARD.encode(bytes)}})
        )
        .await,
        (200, json!({}))
    );
    assert_eq!(
        connected.pty_until(b"wait").await,
        "37 111\r\nreply:literal 雪 ; $()\r\nwait".as_bytes()
    );
    assert_eq!(
        http.unary(
            "SendInput",
            json!({"process":{"pid":pid},"input":{"pty":STANDARD.encode([4])}})
        )
        .await,
        (200, json!({}))
    );
    connected.end(0).await;
    child.0 = 0;
    let mut retained = http.stream("Connect", json!({"process":{"pid":pid}})).await;
    assert_eq!(retained.pid().await, pid);
    retained.end(0).await;
    let (_, error) = http
        .unary("Update", json!({"process":{"pid":pid},"pty":{}}))
        .await;
    assert_eq!(error["code"], "not_found");
}

#[tokio::test]
async fn http_update_and_pty_input_report_official_error_categories() {
    let http = HttpProcess::new().await;
    let mut start = http
        .stream("Start", json!({"process":{"cmd":"/bin/cat"},"stdin":true}))
        .await;
    let pid = start.pid().await;
    let mut child = HttpChild(pid);
    assert_eq!(
        http.unary("Update", json!({"process":{"pid":pid}})).await,
        (200, json!({}))
    );
    let (status, error) = http
        .unary("Update", json!({"process":{"pid":pid},"pty":{}}))
        .await;
    assert_eq!(status, 500);
    assert_eq!(error["code"], "internal");
    assert_eq!(
        error["message"],
        "error resizing tty: tty not assigned to process"
    );
    let (status, error) = http
        .unary(
            "SendInput",
            json!({"process":{"pid":pid},"input":{"pty":"eA=="}}),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(error["code"], "failed_precondition");
    let (status, error) = http.unary("Update", json!({"process":{}})).await;
    assert_eq!(status, 501);
    assert_eq!(error["code"], "unimplemented");
    let (status, error) = http
        .unary("Update", json!({"process":{"pid":u32::MAX},"pty":{}}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(error["code"], "not_found");
    assert_eq!(
        http.unary("CloseStdin", json!({"process":{"pid":pid}}))
            .await,
        (200, json!({}))
    );
    start.end(0).await;
    child.0 = 0;
}

#[tokio::test]
async fn http_selector_refusals_carry_the_oracle_getprocess_text() {
    // Oracle service.go:171-200 getProcess: every selector-resolving
    // procedure names the missed pid or tag (CodeNotFound) or the unset
    // oneof's concrete type (CodeUnimplemented). StreamInput already pins
    // these texts through its selection_error mapping (stream_input.rs);
    // SendSignal, Connect, SendInput and Update resolve through the same
    // helper upstream, so their refusals must read identically.
    let http = HttpProcess::new().await;
    let missing = json!({"code": "not_found",
                         "message": "process with pid 4294967295 not found"});
    let (status, error) = http
        .unary(
            "SendSignal",
            json!({"process": {"pid": u32::MAX}, "signal": "SIGNAL_SIGKILL"}),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(error, missing);
    let mut connect = http
        .stream("Connect", json!({"process": {"pid": u32::MAX}}))
        .await;
    let (flags, end) = connect.envelope().await;
    assert_eq!(flags, 2);
    assert_eq!(end, json!({"error": missing}));
    let (status, error) = http
        .unary(
            "SendInput",
            json!({"process": {"pid": u32::MAX}, "input": {"stdin": "eA=="}}),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(error, missing);
    let (status, error) = http
        .unary("Update", json!({"process": {"pid": u32::MAX}, "pty": {}}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(error, missing);
    // CloseStdin resolves through getProcess too (input.go:90), so its misses
    // carry the same texts — including the tag form.
    let (status, error) = http
        .unary("CloseStdin", json!({"process": {"pid": u32::MAX}}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(error, missing);
    let (status, error) = http
        .unary(
            "CloseStdin",
            json!({"process": {"tag": "envd-parity-absent-tag"}}),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(
        error,
        json!({"code": "not_found",
               "message": "process with tag envd-parity-absent-tag not found"}),
    );
}
