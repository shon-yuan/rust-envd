//! The Windows daemon: the shared HTTP surface of `docs/windows-port.md`
//! served over the Windows backend.
//!
//! Scope of this slice (P1): `/health`, `/envs`, `/init` and `GET /files`,
//! behind the same connection-level checks, CORS, authorization and mux
//! frontdoor the Linux daemon uses. The endpoint code is shared
//! (`crate::http`, `crate::files_http`), so this module only owns the state
//! that has no Linux counterpart and the Windows `/init` effects.
//!
//! Declared deviations from the Linux daemon, all recorded in
//! `docs/windows-port.md`:
//! - no MMDS token hash: `/init` authorizes against the installed token only;
//! - no cgroup/freezer/CA/NFS effects: accepted and reported as no-ops;
//! - `X-Envd-Memory` reports `partial: true`, since no cgroup v2 exists;
//! - the builtin default user is the account the service runs as, because
//!   Windows has no `root`.
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::configuration::Configuration;
use crate::http::{json_error, ConfigurationState};

/// The error body of an endpoint this slice does not serve yet. It is not
/// axum's default 404: the frontdoor already matched the operation, so the
/// endpoint exists and answers with the documented gap.
fn not_yet(endpoint: &str) -> Response {
    json_error(
        StatusCode::NOT_IMPLEMENTED,
        &format!("{endpoint} is not implemented on Windows yet; see docs/windows-port.md"),
    )
}

pub struct Daemon {
    state: Arc<Mutex<Configuration>>,
    init_lock: Arc<Mutex<()>>,
}

impl ConfigurationState for Daemon {
    fn configuration(&self) -> &Arc<Mutex<Configuration>> {
        &self.state
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

impl Daemon {
    pub fn new() -> Self {
        let configuration = Configuration::new();
        let mut configuration = configuration;
        // The Linux oracle's builtin default is `root`; the Windows account
        // database has no such name, so the service account is the fallback.
        configuration.defaults.user = crate::platform::windows::account::current_user_name();
        Self {
            state: Arc::new(Mutex::new(configuration)),
            init_lock: Arc::new(Mutex::new(())),
        }
    }
}

pub fn app(state: Arc<Daemon>) -> Router {
    crate::http::layered(http_routes(state.clone()), state)
}

fn http_routes(state: Arc<Daemon>) -> Router {
    Router::new()
        .route("/health", any(crate::http::health))
        .route("/envs", any(crate::http::envs::<Daemon>))
        .route("/init", any(init))
        .route("/files", any(files))
        .route(
            "/files/compose",
            any(|| async { not_yet("POST /files/compose") }),
        )
        .route("/metrics", any(|| async { not_yet("GET /metrics") }))
        .route("/freeze", any(|| async { not_yet("POST /freeze") }))
        .route("/unfreeze", any(|| async { not_yet("POST /unfreeze") }))
        .route("/collapse", any(|| async { not_yet("POST /collapse") }))
        .route("/fsfreeze", any(|| async { not_yet("POST /fsfreeze") }))
        .route("/fsthaw", any(|| async { not_yet("POST /fsthaw") }))
        .route("/upgrade", any(|| async { not_yet("POST /upgrade") }))
        // The frontdoor only forwards the 17 known Connect procedures here,
        // so this fallback is connect-go's "unimplemented" answer for a
        // procedure whose handler this slice does not have.
        .fallback(procedure_unimplemented)
        .with_state(state)
}

/// connect-go's error body for a procedure without a handler: HTTP 404 with
/// the Connect JSON error code, no trailing newline.
async fn procedure_unimplemented(request: Request<Body>) -> Response {
    let body = serde_json::json!({
        "code": "unimplemented",
        "message": format!(
            "{} is not implemented on Windows yet; see docs/windows-port.md",
            request.uri().path()
        ),
    });
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// chi registers GET and POST on `/files`; the gate has already rejected every
/// other method. Only the read half exists in this slice.
async fn files(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    match *request.method() {
        axum::http::Method::GET => crate::files_http::get_files(State(daemon), request).await,
        axum::http::Method::POST => not_yet("POST /files"),
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// The four `/init` fields with Windows effects in this slice. Unknown and
/// Linux-only fields are accepted and ignored, as Go's decoder ignores
/// unknown keys; the Linux-only effects are listed in the module docs.
#[derive(Default)]
struct InitRequest {
    access_token: Option<Zeroizing<String>>,
    env_vars: Option<BTreeMap<String, String>>,
    default_user: Option<String>,
    default_workdir: Option<String>,
}

/// Go's encoding/json folds ASCII case; the same fold keeps `accessToken`
/// and `AccessToken` equivalent on the wire.
fn folded(key: &str) -> String {
    key.chars()
        .map(|ch| match ch {
            'ſ' => 'S',
            'K' => 'K',
            ch => ch.to_ascii_uppercase(),
        })
        .collect()
}

fn decode_init(body: &[u8]) -> Result<InitRequest, String> {
    if body.is_empty() {
        return Ok(InitRequest::default());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| "invalid JSON".to_owned())?;
    let Value::Object(map) = value else {
        return Err("init body must be an object or null".to_owned());
    };
    let mut init = InitRequest::default();
    for (key, value) in map {
        match folded(&key).as_str() {
            "ACCESSTOKEN" => {
                init.access_token = match value {
                    Value::Null => None,
                    Value::String(token) if token.is_empty() => {
                        return Err("empty token not allowed".to_owned())
                    }
                    Value::String(token) => Some(Zeroizing::new(token)),
                    _ => return Err("invalid secure token JSON string".to_owned()),
                };
            }
            "ENVVARS" => match value {
                Value::Null => init.env_vars = None,
                Value::Object(values) => {
                    let mut envs = BTreeMap::new();
                    for (name, value) in values {
                        let Value::String(value) = value else {
                            return Err("envVars values must be strings".to_owned());
                        };
                        envs.insert(name, value);
                    }
                    init.env_vars = Some(envs);
                }
                _ => return Err("envVars must be an object".to_owned()),
            },
            "DEFAULTUSER" => match value {
                Value::Null => init.default_user = None,
                Value::String(user) => init.default_user = Some(user),
                _ => return Err("defaultUser must be a string".to_owned()),
            },
            "DEFAULTWORKDIR" => match value {
                Value::Null => init.default_workdir = None,
                Value::String(workdir) => init.default_workdir = Some(workdir),
                _ => return Err("defaultWorkdir must be a string".to_owned()),
            },
            // caBundle, timestamp, hyperloopIP, cpuCount, thaw, lifecycleID
            // and volumeMounts have Linux-only effects; accepting them
            // unchanged keeps the response contract identical.
            _ => {}
        }
    }
    Ok(init)
}

async fn init(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    let mut response = apply_init(daemon, request).await;
    // Go init.go:211-218 and the X-Envd-Memory report ride EVERY /init
    // response, including the error ones.
    response
        .headers_mut()
        .insert("x-envd-version", header::HeaderValue::from_static("0.10.2"));
    response.headers_mut().insert(
        "x-envd-memory",
        header::HeaderValue::from_static(r#"{"request":0,"low":0,"floor":0,"partial":true}"#),
    );
    response
}

async fn apply_init(daemon: Arc<Daemon>, request: Request<Body>) -> Response {
    let body = match to_bytes(request.into_body(), 32 << 20).await {
        Ok(body) => body,
        Err(_) => return (StatusCode::BAD_REQUEST, "failed to read request body").into_response(),
    };
    let payload = match decode_init(&body) {
        Ok(payload) => payload,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let token = payload.access_token;
    let _guard = daemon.init_lock.lock().await;
    let mut state = daemon.state.lock().await;
    let request_token = token.as_deref().map(|value| value.as_str());
    // No MMDS route on Windows: the installed token is the only authority.
    if let Err(error) = state.access.validate_init(request_token, None) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    if let Some(envs) = payload.env_vars {
        state.environment.replace_user(envs);
    }
    if let Err(error) = state.access.set_token(token.map(|value| value.to_string())) {
        drop(state);
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    if let Some(user) = payload.default_user.filter(|value| !value.is_empty()) {
        state.defaults.user = user;
        state.defaults.user_delivered = true;
    }
    if let Some(workdir) = payload.default_workdir.filter(|value| !value.is_empty()) {
        state.defaults.workdir = Some(workdir);
    }
    state.access.mark_initialized();
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod contract_tests {
    //! Endpoint contract tests. They run on `windows-latest`, where the real
    //! Win32 account and filesystem APIs answer, over a real loopback
    //! listener: the assertions are what an SDK would see on the wire.
    use super::*;
    use axum::http::{HeaderMap, Method};
    use http_body_util::{BodyExt, Full};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const TOKEN: &str = "contract-token";

    struct Running {
        address: SocketAddr,
        _daemon: Arc<Daemon>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn start() -> Running {
        let daemon = Arc::new(Daemon::new());
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
    async fn unimplemented_rest_and_connect_operations_name_the_gap() {
        let running = initialized().await;
        let (status, _, body) = send(
            running.address,
            Method::GET,
            "/metrics",
            &[("x-access-token", TOKEN)],
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(String::from_utf8_lossy(&body).contains("docs/windows-port.md"));

        let (status, headers, body) = send(
            running.address,
            Method::POST,
            "/filesystem.Filesystem/Stat",
            &[
                ("x-access-token", TOKEN),
                ("content-type", "application/json"),
            ],
            br#"{"path":"/nonexistent","username":"runner"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        let error: serde_json::Value = serde_json::from_slice(&body).expect("connect json");
        assert_eq!(error["code"], "unimplemented");
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("docs/windows-port.md"));
    }

    #[tokio::test]
    async fn unimplemented_endpoints_answer_with_the_documented_gap() {
        let running = initialized().await;
        let (status, _, body) = send(
            running.address,
            Method::POST,
            "/files",
            &[("x-access-token", TOKEN)],
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        let error: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("docs/windows-port.md"));
    }
}
