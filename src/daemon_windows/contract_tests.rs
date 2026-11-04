//! Endpoint contract tests. They run on `windows-latest`, where the real
//! Win32 account and filesystem APIs answer, over a real loopback
//! listener: the assertions are what an SDK would see on the wire.
use super::*;
use crate::platform::windows::metadata::StaticMetadata;
use crate::platform::windows::registry::{WorkloadClass, WorkloadRegistry};
use crate::platform::windows::resource::WindowsResourceBackend;
use axum::body::to_bytes;
use axum::http::{HeaderMap, Method};
use base64::{engine::general_purpose::STANDARD, Engine};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::{json, Value};
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

const TOKEN: &str = "contract-token";

struct Running {
    address: SocketAddr,
    _daemon: Arc<Daemon>,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    fn app(&self) -> Router {
        app(self._daemon.clone())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start() -> Running {
    start_daemon(Arc::new(Daemon::new())).await
}

async fn start_daemon(daemon: Arc<Daemon>) -> Running {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let address = listener.local_addr().expect("address");
    let app = app(daemon.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Running {
        address,
        _daemon: daemon,
        task,
    }
}

async fn send(
    address: SocketAddr,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let client: Client<_, Full<hyper::body::Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    let uri = format!("http://{address}{path}");
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Full::new(hyper::body::Bytes::copy_from_slice(body)))
        .expect("request");
    let response = client.request(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

/// A raw HTTP/1.1 exchange, for request shapes a client library will not
/// produce (`OPTIONS *`, a failing `Expect`).
async fn raw(address: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).await.expect("connect");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    String::from_utf8_lossy(&response).into_owned()
}

async fn initialized() -> Running {
    let running = start().await;
    let body = format!(r#"{{"accessToken":"{TOKEN}"}}"#);
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        body.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    running
}

async fn init_json(address: SocketAddr, value: Value) {
    let (status, _, _) = send(
        address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        value.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

async fn initialized_at(directory: &Path) -> Running {
    let running = start().await;
    init_json(
        running.address,
        json!({"accessToken": TOKEN, "defaultWorkdir": path_text(directory)}),
    )
    .await;
    running
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn basic(user: &str) -> String {
    format!(
        "Basic {}",
        STANDARD.encode(format!("{user}:ignored-password"))
    )
}

async fn connect_unary(
    app: &Router,
    method: &str,
    value: Value,
    authorization: Option<&str>,
    token: Option<&str>,
) -> (u16, Value) {
    let mut request = Request::post(format!("/filesystem.Filesystem/{method}"))
        .header("content-type", "application/json")
        .header("connect-protocol-version", "1");
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
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
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn watch(
    app: &Router,
    value: Value,
    authorization: Option<&str>,
    token: Option<&str>,
    keepalive: Option<&str>,
) -> Response {
    let bytes = value.to_string().into_bytes();
    let mut frame = vec![0];
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend(bytes);
    let mut request = Request::post("/filesystem.Filesystem/WatchDir")
        .header("content-type", "application/connect+json")
        .header("connect-protocol-version", "1");
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    if let Some(token) = token {
        request = request.header("x-access-token", token);
    }
    if let Some(keepalive) = keepalive {
        request = request.header("keepalive-ping-interval", keepalive);
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
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
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
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
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

fn junction(link: &Path, target: &Path) -> bool {
    let command = format!(r#"mklink /J "{}" "{}""#, link.display(), target.display());
    std::process::Command::new("cmd")
        .args(["/C", &command])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn populate_tree(root: &Path) {
    std::fs::create_dir_all(root.join("a/b/c")).expect("tree");
    std::fs::create_dir_all(root.join("z")).expect("z");
}

async fn create_watcher(app: &Router, path: &Path) -> (u16, Value) {
    connect_unary(
        app,
        "CreateWatcher",
        json!({"path": path_text(path), "includeEntry": true}),
        None,
        Some(TOKEN),
    )
    .await
}

async fn get_events(app: &Router, id: &str) -> (u16, Value) {
    connect_unary(
        app,
        "GetWatcherEvents",
        json!({"watcherId": id}),
        None,
        Some(TOKEN),
    )
    .await
}

#[tokio::test]
async fn health_matches_the_store_contract() {
    let running = start().await;
    let (status, headers, body) = send(running.address, Method::GET, "/health", &[], b"").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["cache-control"], "no-store");
    // store.go:193 sets Content-Type explicitly, so the header is present
    // and empty.
    assert_eq!(headers[header::CONTENT_TYPE], "");
    assert!(body.is_empty());
}

#[tokio::test]
async fn envs_requires_the_installed_token() {
    let running = initialized().await;
    let (status, _, body) = send(running.address, Method::GET, "/envs", &[], b"").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let error: serde_json::Value = serde_json::from_slice(&body).expect("json error");
    assert_eq!(error["code"], 401);
    assert_eq!(
        error["message"],
        "unauthorized access, please provide a valid access token or method signing if supported"
    );

    let (status, headers, body) = send(
        running.address,
        Method::GET,
        "/envs",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers["cache-control"], "no-store");
    assert!(body.ends_with(b"\n"), "json.NewEncoder trailing newline");
    let envs: serde_json::Value = serde_json::from_slice(&body).expect("env json");
    assert_eq!(envs["E2B_SANDBOX"], "true");
}

#[tokio::test]
async fn init_installs_the_token_and_reports_the_linux_headers() {
    let running = start().await;
    let body = br#"{"accessToken":"contract-token","defaultUser":null}"#;
    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        body,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["x-envd-version"], "0.10.2");
    assert_eq!(
        headers["x-envd-memory"],
        r#"{"request":0,"low":0,"floor":0,"partial":true}"#
    );
}

#[tokio::test]
async fn init_rejects_a_mismatched_token_and_malformed_bodies() {
    let running = initialized().await;
    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        br#"{"accessToken":"wrong"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The version/memory headers ride error responses too.
    assert_eq!(headers["x-envd-version"], "0.10.2");
    assert!(headers.contains_key("x-envd-memory"));
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        b"not json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        br#"{"accessToken":""}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn init_accepts_the_provisioned_token_and_rejects_others() {
    let running = start_daemon(Arc::new(Daemon::with_metadata(Arc::new(
        StaticMetadata::with_values(
            Some("provisioned-token"),
            "sandbox-42",
            "template-7",
            "10.0.0.9:5000",
        ),
    ))))
    .await;

    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        br#"{"accessToken":"wrong"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, b"access token validation failed");

    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, b"access token reset not authorized");

    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        br#"{"accessToken":"provisioned-token"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["x-envd-version"], "0.10.2");
    assert!(headers.contains_key("x-envd-memory"));

    let (status, _, _) = send(running.address, Method::GET, "/envs", &[], b"").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = send(
        running.address,
        Method::GET,
        "/envs",
        &[("x-access-token", "provisioned-token")],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let envs: Value = serde_json::from_slice(&body).expect("env json");
    assert_eq!(envs["E2B_SANDBOX_ID"], "sandbox-42");
    assert_eq!(envs["E2B_TEMPLATE_ID"], "template-7");
}

#[tokio::test]
async fn init_without_provisioning_keeps_the_first_init_permissive() {
    let running = start_daemon(Arc::new(Daemon::with_metadata(Arc::new(
        StaticMetadata::with_values(None, "", "", ""),
    ))))
    .await;
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        br#"{"accessToken":"first-token"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        br#"{"accessToken":"different-token"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, b"access token validation failed");
}

#[tokio::test]
async fn envs_include_provisioned_sandbox_and_template_ids() {
    let running = start_daemon(Arc::new(Daemon::with_metadata(Arc::new(
        StaticMetadata::with_values(Some(TOKEN), "sandbox-42", "template-7", "10.0.0.9:5000"),
    ))))
    .await;
    init_json(
        running.address,
        json!({
            "accessToken": TOKEN,
            "envVars": {
                "E2B_SANDBOX_ID": "user-value",
                "E2B_TEMPLATE_ID": "user-value",
                "E2B_EVENTS_ADDRESS": "user-value",
            },
        }),
    )
    .await;
    let (status, _, body) = send(
        running.address,
        Method::GET,
        "/envs",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let envs: Value = serde_json::from_slice(&body).expect("env json");
    assert_eq!(envs["E2B_SANDBOX_ID"], "sandbox-42");
    assert_eq!(envs["E2B_TEMPLATE_ID"], "template-7");
    assert_eq!(envs["E2B_EVENTS_ADDRESS"], "http://10.0.0.9:5000");
}

#[tokio::test]
async fn get_files_serves_a_real_windows_file() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let file = directory.path().join("hello.txt");
    std::fs::write(&file, b"hello windows").expect("write");
    let query = format!(
        "/files?path={}",
        url::form_urlencoded::byte_serialize(file.to_str().expect("utf8").as_bytes())
            .collect::<String>()
    );
    let (status, headers, body) = send(
        running.address,
        Method::GET,
        &query,
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"hello windows");
    assert_eq!(headers[header::CONTENT_LENGTH], "13");
    assert_eq!(headers["accept-ranges"], "bytes");
    assert!(headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .contains("text/plain"));
}

#[tokio::test]
async fn get_files_missing_path_is_the_oracle_error_shape() {
    let running = initialized().await;
    let missing = std::env::temp_dir().join("coorworker-missing-contract-file");
    let query = format!(
        "/files?path={}",
        url::form_urlencoded::byte_serialize(missing.to_str().unwrap().as_bytes())
            .collect::<String>()
    );
    let (status, headers, body) = send(
        running.address,
        Method::GET,
        &query,
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    let error: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(error["code"], 404);
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("does not exist"));
}

#[tokio::test]
async fn frontdoor_answers_unknown_paths_and_wrong_methods_like_chi() {
    let running = initialized().await;
    let (status, _, body) = send(
        running.address,
        Method::GET,
        "/nope",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, b"404 page not found\n");

    let (status, headers, body) = send(
        running.address,
        Method::GET,
        "/init",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers[header::ALLOW], "POST");
    assert!(body.is_empty());
}

#[tokio::test]
async fn cors_preflight_is_answered_before_auth() {
    let running = start().await;
    let (status, headers, _) = send(
        running.address,
        Method::OPTIONS,
        "/files",
        &[
            ("origin", "https://example.test"),
            ("access-control-request-method", "GET"),
        ],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert!(headers[header::VARY].to_str().unwrap().contains("Origin"));
}

#[tokio::test]
async fn connection_level_checks_precede_the_mux() {
    let running = start().await;
    let star = raw(
        running.address,
        "OPTIONS * HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(star.starts_with("HTTP/1.1 200"), "{star}");
    assert!(star.contains("content-length: 0"), "{star}");

    let expect = raw(
            running.address,
            "GET /health HTTP/1.1\r\nHost: localhost\r\nExpect: something-else\r\nConnection: close\r\n\r\n",
        )
        .await;
    assert!(expect.starts_with("HTTP/1.1 417"), "{expect}");
    assert!(expect.contains("connection: close"), "{expect}");
}

#[tokio::test]
async fn unknown_connect_operations_name_the_gap() {
    // All thirteen REST operations and all seventeen Connect procedures are
    // served now. An unknown procedure is not a registered chi route, so the
    // oracle's mux falls through to `http.NotFound` — a plain-text body, not
    // the Connect service's JSON "method not found" (see src/http/frontdoor.rs).
    let running = start().await;
    let (status, headers, body) = send(
        running.address,
        Method::POST,
        "/process.Process/NotAProcedure",
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/json"),
        ],
        b"{}",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(body, b"404 page not found\n");
}

fn encoded(path: &std::path::Path) -> String {
    url::form_urlencoded::byte_serialize(path.to_str().expect("utf8").as_bytes()).collect()
}

#[tokio::test]
async fn post_files_writes_a_raw_body_for_the_installed_user() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let file = directory.path().join("nested").join("hello.txt");
    let query = format!("/files?path={}", encoded(&file));
    let (status, _, body) = send(
        running.address,
        Method::POST,
        &query,
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/octet-stream"),
        ],
        b"hello windows",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let entries: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let path = file.to_string_lossy().into_owned();
    assert_eq!(
        entries,
        serde_json::json!([{ "name": name, "path": path, "type": "file" }])
    );
    assert_eq!(std::fs::read(&file).expect("read"), b"hello windows");

    // The read half serves the bytes the write half stored.
    let (status, _, body) = send(
        running.address,
        Method::GET,
        &format!("/files?path={}", encoded(&file)),
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"hello windows");
}

#[tokio::test]
async fn post_files_multipart_uploads_each_file_part() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let first = directory.path().join("first.bin");
    let second = directory.path().join("second.bin");
    // Windows accepts `/` in absolute paths, and a `\` inside a
    // Content-Disposition quoted-string would be an escape character.
    let first_name = first.to_string_lossy().replace('\\', "/");
    let second_name = second.to_string_lossy().replace('\\', "/");
    let body = format!(
            "--bound\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{first_name}\"\r\n\r\none\r\n--bound\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{second_name}\"\r\n\r\ntwo\r\n--bound--\r\n"
        );
    let (status, _, response) = send(
        running.address,
        Method::POST,
        "/files",
        &[
            ("x-access-token", TOKEN),
            ("content-type", "multipart/form-data; boundary=bound"),
        ],
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&response)
    );
    let entries: serde_json::Value = serde_json::from_slice(&response).expect("json");
    assert_eq!(entries.as_array().unwrap().len(), 2);
    assert_eq!(std::fs::read(&first).expect("read"), b"one");
    assert_eq!(std::fs::read(&second).expect("read"), b"two");
}

#[tokio::test]
async fn post_files_answers_the_oracle_error_shapes() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");

    // A directory target is refused before anything is truncated.
    let query = format!("/files?path={}", encoded(directory.path()));
    let (status, _, body) = send(
        running.address,
        Method::POST,
        &query,
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/octet-stream"),
        ],
        b"x",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .starts_with("path is a directory: "));

    // Metadata is validated before the file is opened.
    let file = directory.path().join("metadata.txt");
    let query = format!("/files?path={}", encoded(&file));
    let (status, _, body) = send(
        running.address,
        Method::POST,
        &query,
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/octet-stream"),
            ("x-metadata-author", "bad\tvalue"),
        ],
        b"x",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        error["message"],
        "invalid metadata: metadata must be printable ASCII"
    );
    assert!(!file.exists());

    // An unsupported content type is the oracle's 400.
    let (status, _, body) = send(
        running.address,
        Method::POST,
        &query,
        &[("x-access-token", TOKEN), ("content-type", "text/plain")],
        b"x",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("unsupported content type"));
}

#[tokio::test]
async fn post_files_requires_the_write_token() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let file = directory.path().join("guarded.txt");
    let query = format!("/files?path={}", encoded(&file));
    let (status, _, body) = send(
        running.address,
        Method::POST,
        &query,
        &[("content-type", "application/octet-stream")],
        b"x",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let error: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(error["code"], 401);
    assert!(!file.exists());
}

async fn compose_request(
    address: SocketAddr,
    token: Option<&str>,
    body: &[u8],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut headers = vec![("content-type", "application/json")];
    if let Some(token) = token {
        headers.push(("x-access-token", token));
    }
    send(address, Method::POST, "/files/compose", &headers, body).await
}

async fn compose_json(
    address: SocketAddr,
    token: &str,
    value: Value,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let body = value.to_string();
    compose_request(address, Some(token), body.as_bytes()).await
}

fn compose_error(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("compose error json")
}

fn assert_compose_error(status: StatusCode, body: &[u8], code: u16, message: &str) {
    assert_eq!(
        status,
        StatusCode::from_u16(code).expect("status"),
        "{}",
        String::from_utf8_lossy(body)
    );
    let error = compose_error(body);
    assert_eq!(error["code"], code);
    assert_eq!(error["message"], message);
}

/// Names left behind by the compose temporary-file guard in `directory`.
fn compose_temporaries(directory: &Path, destination: &Path) -> Vec<std::path::PathBuf> {
    let prefix = destination
        .file_name()
        .map(|name| format!("{}.e2b-compose.", name.to_string_lossy()))
        .unwrap_or_default();
    fs::read_dir(directory)
        .expect("destination directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().starts_with(&prefix))
                .unwrap_or(false)
        })
        .collect()
}

/// A daemon whose default user is empty, so compose reaches the
/// `username not provided` validation step.
async fn daemon_with_empty_default_user() -> Running {
    let daemon = Arc::new(Daemon::new());
    daemon.state.lock().await.defaults.user.clear();
    start_daemon(daemon).await
}

#[tokio::test]
async fn compose_concatenates_sources_in_order_and_deletes_them() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let tail = directory.path().join("tail.bin");
    let nulls = directory.path().join("nulls.bin");
    let middle = directory.path().join("middle.bin");
    fs::write(&tail, b"tail").expect("tail");
    fs::write(&nulls, b"\0\xff").expect("nulls");
    fs::write(&middle, b"mid").expect("middle");
    let destination = directory.path().join("composed.bin");

    let (status, headers, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [
                path_text(&tail),
                path_text(&nulls),
                path_text(&middle),
                path_text(&nulls),
            ],
            "destination": path_text(&destination),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    let entry: Value = serde_json::from_slice(&body).expect("compose json");
    assert_eq!(entry["name"], "composed.bin");
    assert_eq!(entry["path"], path_text(&destination));
    assert_eq!(entry["type"], "file");
    assert_eq!(
        fs::read(&destination).expect("composed bytes"),
        b"tail\0\xffmid\0\xff"
    );
    for source in [&tail, &nulls, &middle] {
        let query = format!("/files?path={}", encoded(source));
        let (status, _, _) = send(
            running.address,
            Method::GET,
            &query,
            &[("x-access-token", TOKEN)],
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{}", path_text(source));
    }
}

#[tokio::test]
async fn compose_overwrites_existing_destination_and_replaces_destination_symlink() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");

    let source = directory.path().join("source.bin");
    fs::write(&source, b"composed").expect("source");
    let destination = directory.path().join("destination.bin");
    fs::write(&destination, b"kept").expect("destination");
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&source)],
            "destination": path_text(&destination),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(fs::read(&destination).expect("destination"), b"composed");
    assert!(!source.exists());

    // A destination symlink is replaced itself; its target is untouched.
    let target = directory.path().join("target.bin");
    fs::write(&target, b"untouched").expect("target");
    let link = directory.path().join("link.bin");
    if std::os::windows::fs::symlink_file(&target, &link).is_err() {
        return;
    }
    let source = directory.path().join("link-source.bin");
    fs::write(&source, b"replaced").expect("source");
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&source)],
            "destination": path_text(&link),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(fs::read(&link).expect("link path"), b"replaced");
    assert!(!fs::symlink_metadata(&link)
        .expect("link metadata")
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(&target).expect("target"), b"untouched");
}

#[tokio::test]
async fn compose_validation_order_matches_and_preserves_files() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let destination = directory.path().join("destination.bin");
    let source = directory.path().join("source.bin");
    fs::write(&destination, b"original destination").expect("destination");
    fs::write(&source, b"original source").expect("source");
    let unchanged = || {
        assert_eq!(
            fs::read(&destination).expect("destination"),
            b"original destination"
        );
        assert_eq!(fs::read(&source).expect("source"), b"original source");
    };

    // 1. Empty source list.
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({"source_paths": [], "destination": path_text(&destination)}),
    )
    .await;
    assert_compose_error(status, &body, 400, "source_paths must not be empty");
    unchanged();

    // 2. Missing destination.
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({"source_paths": [path_text(&source)]}),
    )
    .await;
    assert_compose_error(status, &body, 400, "destination is required");
    unchanged();

    // 3. No username anywhere: the daemon must have an empty default user.
    let empty = daemon_with_empty_default_user().await;
    let body = json!({
        "source_paths": [path_text(&source)],
        "destination": path_text(&destination),
    })
    .to_string();
    let (status, _, response) = compose_request(empty.address, None, body.as_bytes()).await;
    assert_compose_error(status, &response, 400, "username not provided");
    unchanged();

    // 4. Unknown account: 401 before any path work.
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&source)],
            "destination": path_text(&destination),
            "username": "root",
        }),
    )
    .await;
    assert_compose_error(
        status,
        &body,
        401,
        "error looking up user 'root': user: unknown user root",
    );
    unchanged();

    // 5. Missing source: 404 with the raw request string.
    let missing = directory.path().join("missing.bin");
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&missing)],
            "destination": path_text(&destination),
        }),
    )
    .await;
    assert_compose_error(
        status,
        &body,
        404,
        &format!("source file not found: {}", path_text(&missing)),
    );
    unchanged();

    // 6. Source equal to destination, with Go-quoted plain-string escapes.
    let unusual = directory.path().join("bell\u{2000}\u{378}");
    fs::write(&unusual, b"unusual").expect("unusual");
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&unusual)],
            "destination": path_text(&unusual),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = compose_error(&body)["message"]
        .as_str()
        .expect("message")
        .to_owned();
    assert!(message.starts_with("source path \""), "{message}");
    assert!(message.contains("\\u2000"), "{message}");
    assert!(message.contains("\\u0378"), "{message}");
    assert!(
        message.ends_with(" cannot be the same as destination"),
        "{message}"
    );
    assert_eq!(fs::read(&unusual).expect("unusual"), b"unusual");

    // 7. A directory is not a regular file.
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(directory.path())],
            "destination": path_text(&destination),
        }),
    )
    .await;
    assert_compose_error(
        status,
        &body,
        400,
        &format!(
            "source path is not a regular file: {}",
            path_text(directory.path())
        ),
    );
    unchanged();

    // 8. A hard link to the destination is the same file.
    let linked = directory.path().join("linked.bin");
    fs::hard_link(&destination, &linked).expect("hard link");
    let (status, _, body) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&linked)],
            "destination": path_text(&destination),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        compose_error(&body)["message"],
        format!(
            "source path {} cannot be the same as destination",
            json!(path_text(&linked))
        )
    );
    unchanged();
}

#[tokio::test]
async fn compose_frozen_malformed_bodies_and_trailing_bytes() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let source = directory.path().join("source.bin");
    fs::write(&source, b"new").expect("source");
    let destination = directory.path().join("destination.bin");

    // A duplicate destination with a number keeps the first type error.
    let body = format!(
        r#"{{"source_paths":[{}],"destination":{},"destination":5}}"#,
        json!(path_text(&source)),
        json!(path_text(&destination))
    );
    let (status, _, response) =
        compose_request(running.address, Some(TOKEN), body.as_bytes()).await;
    assert_compose_error(
        status,
        &response,
        400,
        "invalid request body: json: cannot unmarshal number into Go struct field ComposeRequest.destination of type string",
    );
    assert!(!destination.exists());

    // One complete JSON value is enough; trailing bytes are ignored.
    let body = format!(
        r#"{{"source_paths":[{}],"destination":{}}} trailing bytes"#,
        json!(path_text(&source)),
        json!(path_text(&destination))
    );
    let (status, _, response) =
        compose_request(running.address, Some(TOKEN), body.as_bytes()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(fs::read(&destination).expect("destination"), b"new");
    assert!(!source.exists());

    for body in [&b""[..], &b"   "[..]] {
        let (status, _, response) = compose_request(running.address, Some(TOKEN), body).await;
        assert_compose_error(status, &response, 400, "invalid request body: EOF");
    }
    let (status, _, response) = compose_request(running.address, Some(TOKEN), b"null").await;
    assert_compose_error(status, &response, 400, "source_paths must not be empty");

    // Unknown fields are skipped even when their values are invalid JSON types.
    let source = directory.path().join("source.bin");
    fs::write(&source, b"new").expect("source");
    let body = json!({
        "unknown": {"deep": [1, 2, 3]},
        "source_paths": [path_text(&source)],
        "destination": path_text(&destination),
    })
    .to_string();
    let (status, _, response) =
        compose_request(running.address, Some(TOKEN), body.as_bytes()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(fs::read(&destination).expect("destination"), b"new");
}

#[tokio::test]
async fn compose_requires_the_token_and_ignores_query_username() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let source = directory.path().join("source.bin");
    fs::write(&source, b"guarded").expect("source");
    let destination = directory.path().join("destination.bin");
    let body = json!({
        "source_paths": [path_text(&source)],
        "destination": path_text(&destination),
    })
    .to_string();

    let (status, _, response) = compose_request(running.address, None, body.as_bytes()).await;
    assert_compose_error(
        status,
        &response,
        401,
        "unauthorized access, please provide a valid access token or method signing if supported",
    );
    assert!(!destination.exists());

    let (status, _, response) =
        compose_request(running.address, Some("wrong-token"), body.as_bytes()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(compose_error(&response)["code"], 401);
    assert!(!destination.exists());

    // Compose has no username query parameter: if it did, `root` would fail
    // the account lookup instead of composing as the installed default user.
    let (status, _, response) = send(
        running.address,
        Method::POST,
        "/files/compose?username=root",
        &[
            ("content-type", "application/json"),
            ("x-access-token", TOKEN),
        ],
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(fs::read(&destination).expect("destination"), b"guarded");
}

#[tokio::test]
async fn compose_unknown_user_is_the_401_shape() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let source = directory.path().join("source.bin");
    fs::write(&source, b"keep").expect("source");
    let destination = directory.path().join("new").join("deep").join("out.bin");
    let (status, _, response) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&source)],
            "destination": path_text(&destination),
            "username": "root",
        }),
    )
    .await;
    assert_compose_error(
        status,
        &response,
        401,
        "error looking up user 'root': user: unknown user root",
    );
    assert!(!destination.exists());
    assert!(!directory.path().join("new").exists());
    assert_eq!(fs::read(&source).expect("source"), b"keep");
}

#[tokio::test]
async fn compose_temp_is_cleaned_up_after_a_source_copy_failure() {
    use std::os::windows::fs::OpenOptionsExt;

    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let source = directory.path().join("locked.bin");
    fs::write(&source, b"locked").expect("source");
    let destination = directory.path().join("destination.bin");
    // Attribute validation does not need a share-compatible handle, but the
    // handler's later open does, so the copy phase fails after validation.
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&source)
        .expect("lock source");
    let (status, _, response) = compose_json(
        running.address,
        TOKEN,
        json!({
            "source_paths": [path_text(&source)],
            "destination": path_text(&destination),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        String::from_utf8_lossy(&response)
    );
    let message = compose_error(&response)["message"]
        .as_str()
        .expect("message")
        .to_owned();
    assert!(
        message.starts_with("error opening source file "),
        "{message}"
    );
    assert!(!destination.exists());
    assert!(compose_temporaries(directory.path(), &destination).is_empty());
    drop(locked);
    assert_eq!(fs::read(&source).expect("source"), b"locked");
}

#[tokio::test]
async fn compose_is_served_and_upgrade_reaches_the_shared_handler() {
    let running = initialized().await;
    // Compose now reaches the shared handler: an empty body fails its decoder
    // instead of answering the 501 gap.
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/files/compose",
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/json"),
        ],
        b"",
    )
    .await;
    assert_ne!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("invalid request body: EOF"));

    // `POST /upgrade` now reaches the shared handler: with no wired run it
    // answers the documented 500 instead of the 501 gap.
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/upgrade",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, b"upgrade is not wired\n");
}

#[path = "contract_tests/clock.rs"]
mod clock;
#[path = "contract_tests/filesystem.rs"]
mod filesystem;
#[path = "contract_tests/process.rs"]
mod process;
#[path = "contract_tests/upgrade.rs"]
mod upgrade;
#[path = "contract_tests/watchers.rs"]
mod watchers;

/// A real child the freeze backend can suspend. Dropping it terminates the
/// process even while suspended, so a failing assertion cannot leak a frozen
/// workload.
struct WorkloadChild {
    child: Option<Child>,
}

impl WorkloadChild {
    fn spawn() -> Self {
        let child = Command::new("ping")
            .args(["-n", "120", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ping workload");
        Self { child: Some(child) }
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().expect("workload alive").id()
    }
}

impl Drop for WorkloadChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn suspend_count(pid: u32) -> u32 {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
    };
    let threads = crate::platform::windows::suspend::enumerate_threads(pid).expect("threads");
    assert!(!threads.is_empty(), "live process has a thread");
    let tid = threads[0];
    // SAFETY: scalar arguments; the handle is checked and closed.
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
    assert!(!thread.is_null(), "open thread {tid}");
    // SAFETY: valid THREAD_SUSPEND_RESUME handle.
    let previous = unsafe { SuspendThread(thread) };
    assert_ne!(previous, u32::MAX, "probe suspend");
    // SAFETY: our own probe is the only increment being removed.
    assert_ne!(unsafe { ResumeThread(thread) }, u32::MAX, "probe resume");
    // SAFETY: handle ours, unused afterwards.
    unsafe {
        CloseHandle(thread);
    }
    previous
}

#[tokio::test]
async fn freeze_without_maxwait_is_a_bare_204() {
    let registry = Arc::new(WorkloadRegistry::new());
    let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
    let child = WorkloadChild::spawn();
    std::thread::sleep(std::time::Duration::from_millis(50));
    registry.register(child.pid(), WorkloadClass::User, Some("contract".into()));
    let running = start_daemon(Arc::new(Daemon::new().with_resource(backend))).await;
    init_json(running.address, json!({"accessToken": TOKEN})).await;

    let (status, headers, body) = send(
        running.address,
        Method::POST,
        "/freeze",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty(), "a bare caller gets no body");
    assert!(
        !headers.contains_key(header::CONTENT_TYPE),
        "a bare 204 sets no Content-Type"
    );
    assert!(
        !headers.contains_key(header::CACHE_CONTROL),
        "a bare 204 sets no Cache-Control"
    );
    assert_eq!(suspend_count(child.pid()), 1, "the child is suspended");

    // A structured follow-up shows the same suspension without stacking it.
    let (status, headers, body) = send(
        running.address,
        Method::POST,
        "/freeze?maxWaitMs=500",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let value: Value = serde_json::from_slice(&body).expect("freeze result");
    for key in [
        "mode",
        "visited",
        "allowlisted",
        "truncated",
        "preFrozen",
        "requested",
        "frozen",
        "notFrozen",
        "failed",
        "vanished",
        "unobservable",
        "sweepMs",
        "waitMs",
    ] {
        assert!(value.get(key).is_some(), "missing {key}: {value}");
    }
    assert_eq!(value["mode"], "legacy");
    assert!(value["requested"].as_i64().unwrap() >= 1);
    assert_eq!(suspend_count(child.pid()), 1);

    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/unfreeze",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(!headers.contains_key(header::CONTENT_TYPE));
    assert_eq!(suspend_count(child.pid()), 0);
}

#[tokio::test]
async fn freeze_query_binding_errors_are_go_plain_400s() {
    let running = start().await;
    let (status, headers, body) = send(
        running.address,
        Method::POST,
        "/freeze?maxWaitMs=x",
        &[],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        body,
        b"Invalid format for parameter maxWaitMs: error binding string parameter: \
          strconv.ParseInt: parsing \"x\": invalid syntax\n"
            .to_vec()
    );

    // Binding order stops at maxCgroups before it reads maxWaitMs.
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/freeze?maxCgroups=x&maxWaitMs=y",
        &[],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        String::from_utf8_lossy(&body).contains("parameter maxCgroups"),
        "{}",
        String::from_utf8_lossy(&body)
    );

    // mode is not validated: an unknown value is legacy.
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/freeze?mode=nonsense&maxWaitMs=1",
        &[],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_slice(&body).expect("freeze result");
    assert_eq!(value["mode"], "legacy");
}

#[tokio::test]
async fn freeze_hierarchy_falls_back_to_legacy_without_a_job() {
    // `Daemon::new` always creates the sandbox Job, so the HTTP surface can
    // only see the job-less fallback through an injected backend.
    let backend = Arc::new(WindowsResourceBackend::with_registry(Arc::new(
        WorkloadRegistry::new(),
    )));
    let running = start_daemon(Arc::new(Daemon::new().with_resource(backend))).await;
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/freeze?mode=hierarchy&maxWaitMs=1",
        &[],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_slice(&body).expect("freeze result");
    assert_eq!(value["mode"], "legacy");
    assert_eq!(value["visited"], 0);
    assert_eq!(value["allowlisted"], 0);
    assert_eq!(value["truncated"], false);
    assert_eq!(value["unobservable"], 0);
    assert_eq!(
        value["notFrozen"].as_i64().unwrap(),
        value["requested"].as_i64().unwrap()
            - value["frozen"].as_i64().unwrap()
            - value["failed"].as_i64().unwrap()
            - value["vanished"].as_i64().unwrap()
            - value["unobservable"].as_i64().unwrap()
    );
}

#[tokio::test]
async fn freeze_requires_the_token_and_does_not_suspend_on_401() {
    let registry = Arc::new(WorkloadRegistry::new());
    let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
    let child = WorkloadChild::spawn();
    std::thread::sleep(std::time::Duration::from_millis(50));
    registry.register(child.pid(), WorkloadClass::User, None);
    let running = start_daemon(Arc::new(Daemon::new().with_resource(backend))).await;
    init_json(running.address, json!({"accessToken": TOKEN})).await;

    let (status, _, body) = send(running.address, Method::POST, "/freeze", &[], b"").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let error: Value = serde_json::from_slice(&body).expect("json error");
    assert_eq!(error["code"], 401);
    assert_eq!(suspend_count(child.pid()), 0, "auth precedes the sweep");
}

#[tokio::test]
async fn unfreeze_resumes_and_is_idempotent() {
    let registry = Arc::new(WorkloadRegistry::new());
    let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
    let child = WorkloadChild::spawn();
    std::thread::sleep(std::time::Duration::from_millis(50));
    registry.register(child.pid(), WorkloadClass::User, None);
    let running = start_daemon(Arc::new(Daemon::new().with_resource(backend))).await;
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/freeze?maxWaitMs=100",
        &[],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suspend_count(child.pid()), 1);

    for _ in 0..2 {
        let (status, headers, body) =
            send(running.address, Method::POST, "/unfreeze", &[], b"").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty());
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert!(!headers.contains_key(header::CONTENT_TYPE));
        assert_eq!(suspend_count(child.pid()), 0);
    }
}

#[tokio::test]
async fn unfreeze_on_a_backend_error_is_the_json_500() {
    let backend = WindowsResourceBackend::with_registry(Arc::new(WorkloadRegistry::new()))
        .with_unfreeze_error_for_tests("resume pid 1234: Access is denied.");
    let running = start_daemon(Arc::new(Daemon::new().with_resource(Arc::new(backend)))).await;
    let (status, headers, body) = send(running.address, Method::POST, "/unfreeze", &[], b"").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        body,
        b"{\"code\":500,\"message\":\"resume pid 1234: Access is denied.\"}\n".to_vec()
    );
}

/// A scripted volume freezer: the wire shape under test, without requiring a
/// writable system volume.
struct ScriptedFreezer {
    freeze_error: Option<&'static str>,
    thaw_error: Option<&'static str>,
}

impl ScriptedFreezer {
    fn ok() -> Self {
        Self {
            freeze_error: None,
            thaw_error: None,
        }
    }
}

impl crate::fsfreeze::FsFreezer for ScriptedFreezer {
    fn freeze(&self, _mountpoint: &str) -> std::io::Result<()> {
        match self.freeze_error {
            Some(error) => Err(std::io::Error::other(error)),
            None => Ok(()),
        }
    }

    fn thaw(&self, _mountpoint: &str) -> std::io::Result<()> {
        match self.thaw_error {
            Some(error) => Err(std::io::Error::other(error)),
            None => Ok(()),
        }
    }
}

#[tokio::test]
async fn metrics_requires_the_token_and_reports_every_field() {
    let running = initialized().await;
    let (status, headers, body) = send(running.address, Method::GET, "/metrics", &[], b"").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    let error: Value = serde_json::from_slice(&body).expect("json error");
    assert_eq!(error["code"], 401);
    assert!(body.ends_with(b"\n"));

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (status, headers, body) = send(
        running.address,
        Method::GET,
        "/metrics",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(body.ends_with(b"\n"), "json.Encoder trailing newline");
    let value: Value = serde_json::from_slice(&body).expect("metrics json");
    let object = value.as_object().expect("object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
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
            "ts",
        ]
    );
    assert!(value.get("oom_kills").is_none(), "oom_kills is unknown");
    assert!(value["cpu_count"].as_u64().unwrap() > 0);
    let percent = value["cpu_used_pct"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&percent), "{percent}");
    assert!(value["mem_total"].as_u64().unwrap() > 0);
    assert!(value["mem_used"].as_u64().unwrap() <= value["mem_total"].as_u64().unwrap());
    assert!(value["disk_total"].as_u64().unwrap() > 0);
    assert!(value["disk_used"].as_u64().unwrap() <= value["disk_total"].as_u64().unwrap());
    let ts = value["ts"].as_i64().unwrap();
    assert!(
        (before..=after).contains(&ts),
        "ts={ts} window={before}..={after}"
    );
}

#[tokio::test]
async fn metrics_cpu_target_fields_are_zero_without_a_hotplug_manager() {
    let running = initialized().await;
    let (status, _, body) = send(
        running.address,
        Method::GET,
        "/metrics",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_slice(&body).expect("metrics json");
    assert_eq!(value["cpu_possible"], 0);
    assert_eq!(value["cpu_target"], 0);
    assert_eq!(value["cpu_target_attempts"], 0);
    assert_eq!(value["cpu_write_pending_ms"], 0);
}

#[tokio::test]
async fn a_failed_metrics_sample_is_an_empty_500_with_the_same_headers() {
    // A collector whose disk sample cannot resolve still samples memory and
    // CPU first; the shared handler answers the empty 500 with `no-store`.
    let daemon =
        Daemon::new().with_metrics(WindowsCollector::with_volume_for_tests(r"\\.\NoSuchVolume"));
    let running = start_daemon(Arc::new(daemon)).await;
    init_json(running.address, json!({"accessToken": TOKEN})).await;
    let (status, headers, body) = send(
        running.address,
        Method::GET,
        "/metrics",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(body.is_empty());
}

#[tokio::test]
async fn metrics_is_get_only_and_auth_precedes_the_method() {
    let running = start().await;
    // No token installed: the frontdoor owns the method matrix.
    let (status, headers, body) = send(running.address, Method::POST, "/metrics", &[], b"").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers[header::ALLOW], "GET");
    assert!(body.is_empty());

    init_json(running.address, json!({"accessToken": TOKEN})).await;
    // Auth runs before the frontdoor: no token is 401, not 405.
    let (status, _, body) = send(running.address, Method::POST, "/metrics", &[], b"").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("json")["code"],
        401
    );
    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/metrics",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers[header::ALLOW], "GET");
}

#[tokio::test]
async fn collapse_reports_the_six_field_shape_with_no_store() {
    let running = initialized().await;
    for junk in [&b""[..], b"not json at all"] {
        let (status, headers, body) = send(
            running.address,
            Method::POST,
            "/collapse",
            &[("x-access-token", TOKEN)],
            junk,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert!(
            !headers.contains_key("x-content-type-options"),
            "the 200 shape has no nosniff"
        );
        assert!(body.ends_with(b"\n"));
        let value: Value = serde_json::from_slice(&body).expect("collapse json");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "alreadyHuge",
                "chunks",
                "collapsed",
                "elapsedMs",
                "regions",
                "skipped"
            ]
        );
        assert_eq!(value["collapsed"], 0);
        assert_eq!(value["alreadyHuge"], 0);
        assert_eq!(value["skipped"], value["chunks"]);
        assert!(value["elapsedMs"].as_i64().unwrap() >= 0);
    }

    // POST is the only method.
    let (status, headers, _) = send(
        running.address,
        Method::GET,
        "/collapse",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers[header::ALLOW], "POST");
}

#[tokio::test]
async fn collapse_requires_the_token() {
    let running = initialized().await;
    let (status, headers, body) = send(running.address, Method::POST, "/collapse", &[], b"").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    let error: Value = serde_json::from_slice(&body).expect("json error");
    assert_eq!(error["code"], 401);
}

#[tokio::test]
async fn fsfreeze_and_fsthaw_are_bodiless_no_store_204s() {
    let running = start_daemon(Arc::new(
        Daemon::new().with_fsfreeze(Arc::new(ScriptedFreezer::ok())),
    ))
    .await;
    init_json(running.address, json!({"accessToken": TOKEN})).await;
    for path in ["/fsfreeze", "/fsthaw", "/fsfreeze", "/fsthaw"] {
        let (status, headers, body) = send(
            running.address,
            Method::POST,
            path,
            &[("x-access-token", TOKEN)],
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{path}");
        assert!(body.is_empty(), "{path}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert!(!headers.contains_key(header::CONTENT_TYPE), "{path}");
    }
}

#[tokio::test]
async fn fsfreeze_on_a_volume_error_is_the_json_500() {
    let running = start_daemon(Arc::new(Daemon::new().with_fsfreeze(Arc::new(
        ScriptedFreezer {
            freeze_error: Some("FIFREEZE C:\\: Access is denied. (os error 5)"),
            thaw_error: Some("FITHAW C:\\: Access is denied. (os error 5)"),
        },
    ))))
    .await;
    for (path, message) in [
        ("/fsfreeze", "FIFREEZE C:\\: Access is denied. (os error 5)"),
        ("/fsthaw", "FITHAW C:\\: Access is denied. (os error 5)"),
    ] {
        let (status, headers, body) = send(running.address, Method::POST, path, &[], b"").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert_eq!(
            headers[header::CONTENT_TYPE],
            "application/json; charset=utf-8"
        );
        assert_eq!(headers["x-content-type-options"], "nosniff");
        let expected = format!("{}\n", json!({"code": 500, "message": message}));
        assert_eq!(String::from_utf8_lossy(&body), expected);
    }
}

#[tokio::test]
async fn fsfreeze_and_fsthaw_require_the_token() {
    let running = initialized().await;
    for path in ["/fsfreeze", "/fsthaw"] {
        let (status, _, body) = send(running.address, Method::POST, path, &[], b"").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("json")["code"],
            401
        );
    }
}

#[tokio::test]
async fn init_thaws_the_workload_unless_skip() {
    let registry = Arc::new(WorkloadRegistry::new());
    let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
    let child = WorkloadChild::spawn();
    std::thread::sleep(std::time::Duration::from_millis(50));
    registry.register(child.pid(), WorkloadClass::User, None);
    let running = start_daemon(Arc::new(Daemon::new().with_resource(backend))).await;
    init_json(running.address, json!({"accessToken": TOKEN})).await;

    // A default /init resumes the workload it froze before the resume.
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/freeze?maxWaitMs=200",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suspend_count(child.pid()), 1);
    init_json(running.address, json!({"accessToken": TOKEN})).await;
    assert_eq!(suspend_count(child.pid()), 0, "init thaws by default");

    // `thaw: "skip"` withholds it.
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/freeze?maxWaitMs=200",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suspend_count(child.pid()), 1);
    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        format!(r#"{{"accessToken":"{TOKEN}","thaw":"skip"}}"#).as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["x-envd-thaw"], "skip");
    assert_eq!(suspend_count(child.pid()), 1, "skip withholds the thaw");

    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/unfreeze",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(suspend_count(child.pid()), 0);
}
