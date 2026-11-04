//! The `cua` extension: supervision and discovery for the computer-use worker.
//!
//! The worker is a separate process by construction (see
//! [`coorworker_cua`](https://github.com/shon-yuan/rust-envd)): it links the
//! desktop stack that the frozen envd daemon must not carry, and on Windows it
//! must run in the interactive session rather than in the service's session 0.
//! This extension therefore does not link Cua Driver. It spawns the worker,
//! answers liveness for it, and publishes the endpoint a gateway or ToolHost
//! should dial.
//!
//! Enabled with `COORWORKER_CUA_BIN`; the listen address defaults to
//! `127.0.0.1:3211` and can be set with `COORWORKER_CUA_LISTEN`. The
//! supervisor replaces a worker that exits (exponential backoff, reset after a
//! stable run). A worker that fails to start leaves the extension unhealthy
//! and visible, and never affects the envd surface.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::process::{Child, Command};

use super::{Extension, Health};

const DEFAULT_LISTEN: &str = "127.0.0.1:3211";
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const READY_POLL: Duration = Duration::from_millis(50);
/// Restart backoff: 0.5 s, doubling, capped at 30 s. A worker that stayed up
/// for [`STABLE_AFTER`] resets the sequence, so a crash loop backs off while a
/// rare restart returns to service quickly.
const RESTART_BASE: Duration = Duration::from_millis(500);
const RESTART_MAX: Duration = Duration::from_secs(30);
const STABLE_AFTER: Duration = Duration::from_secs(60);

/// The path the optional proxy adds under the extension prefix.
const PROXY_PATH: &str = "/mcp";
/// Tool arguments are small; a capture comes back in the response, not here.
const PROXY_REQUEST_LIMIT: usize = 1 << 20;
/// A base64 desktop capture is the large case; refuse anything past this.
const PROXY_RESPONSE_LIMIT: usize = 32 << 20;
const PROXY_TIMEOUT: Duration = Duration::from_secs(30);

/// Liveness of the supervised process. The supervisor owns the child; every
/// reader only asks this snapshot, so a restart never blocks a health check.
struct WorkerStatus {
    running: bool,
    /// Why the worker is not running, or how it stopped.
    detail: Option<String>,
    /// How many times a running worker has exited and been replaced.
    restarts: u32,
}

impl WorkerStatus {
    fn running() -> Self {
        Self {
            running: true,
            detail: None,
            restarts: 0,
        }
    }

    fn stopped(detail: impl Into<String>) -> Self {
        Self {
            running: false,
            detail: Some(detail.into()),
            restarts: 0,
        }
    }

    fn health(&self) -> Health {
        if self.running {
            return Health::healthy();
        }
        Health::unhealthy(
            self.detail
                .clone()
                .unwrap_or_else(|| "worker is not running".to_owned()),
        )
    }
}

/// The guest-facing credential. Separate from anything the worker uses: the
/// proxy is what a remote caller can reach, the worker stays on loopback.
struct Proxy {
    token: Vec<u8>,
}

struct CuaState {
    status: Mutex<WorkerStatus>,
    mcp_url: String,
    /// `None` means `/cua/mcp` is not mounted at all, not that it is open.
    proxy: Option<Proxy>,
    /// The token the worker requires (`CUA_ENV_TOKEN`), injected on forward.
    worker_token: Option<Vec<u8>>,
}

/// Supervises one `coorworker-cua` process.
pub struct CuaExtension {
    state: Arc<CuaState>,
}

impl CuaExtension {
    /// Spawns the worker and waits until its loopback listener accepts, up to
    /// five seconds. A worker that cannot start is reported through
    /// [`Extension::health`] instead of aborting the daemon.
    pub async fn spawn(binary: PathBuf, listen: SocketAddr) -> Self {
        Self::build(binary, listen, None).await
    }

    /// Spawns the worker and additionally exposes it as `POST /cua/mcp`.
    ///
    /// `proxy_token` is the guest-facing bearer token. The worker's own token,
    /// if any, comes from `CUA_ENV_TOKEN` — the same variable its child process
    /// inherits — and is injected on every forwarded request, so the caller
    /// never needs it.
    pub async fn spawn_proxied(binary: PathBuf, listen: SocketAddr, proxy_token: Vec<u8>) -> Self {
        Self::build(binary, listen, Some(proxy_token)).await
    }

    async fn build(binary: PathBuf, listen: SocketAddr, proxy_token: Option<Vec<u8>>) -> Self {
        let mcp_url = format!("http://{listen}/mcp");
        // The first launch stays synchronous (and waits for the listener) so a
        // caller that spawns the extension knows whether it came up. After
        // that the supervisor owns the process and replaces it when it exits.
        let (status, child, attempts) = match launch(&binary, listen).await {
            Ok(child) => (WorkerStatus::running(), Some(child), 0),
            Err(error) => (WorkerStatus::stopped(error), None, 1),
        };
        // An empty token would enable the route with nothing to check against;
        // treat it as "not configured" instead.
        let proxy = proxy_token
            .filter(|token| !token.is_empty())
            .map(|token| Proxy { token });
        let worker_token = std::env::var_os("CUA_ENV_TOKEN")
            .map(|value| value.to_string_lossy().into_owned().into_bytes())
            .filter(|token| !token.is_empty());
        let state = Arc::new(CuaState {
            status: Mutex::new(status),
            mcp_url,
            proxy,
            worker_token,
        });
        let status = Arc::clone(&state);
        tokio::spawn(async move {
            supervise(
                &binary,
                listen,
                status,
                child,
                attempts,
                |binary, listen| async move { launch(&binary, listen).await },
            )
            .await;
        });
        Self { state }
    }

    /// `Some` when `COORWORKER_CUA_BIN` is set. The value `None` is the parity
    /// build: no extension, no routes, no child.
    pub async fn from_env() -> Option<Self> {
        let binary = std::env::var_os("COORWORKER_CUA_BIN")?;
        let listen = match std::env::var("COORWORKER_CUA_LISTEN") {
            Ok(value) => match value.parse() {
                Ok(listen) => listen,
                Err(error) => {
                    return Some(Self::unavailable(format!(
                        "invalid COORWORKER_CUA_LISTEN {value:?}: {error}"
                    )))
                }
            },
            Err(_) => DEFAULT_LISTEN.parse().expect("default listen parses"),
        };
        let proxy_token = std::env::var_os("COORWORKER_CUA_PROXY_TOKEN")
            .map(|value| value.to_string_lossy().into_owned().into_bytes());
        Some(Self::build(PathBuf::from(binary), listen, proxy_token).await)
    }

    /// An extension that is registered but cannot start. Keeping it visible
    /// is the point: `/cua/health` says why instead of the surface vanishing.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus::stopped(reason)),
                mcp_url: format!("http://{DEFAULT_LISTEN}/mcp"),
                proxy: None,
                worker_token: None,
            }),
        }
    }

    pub fn mcp_url(&self) -> &str {
        &self.state.mcp_url
    }
}

async fn launch(binary: &Path, listen: SocketAddr) -> Result<Child, String> {
    let mut child = Command::new(binary)
        .arg("--listen")
        .arg(listen.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot spawn {}: {error}", binary.display()))?;

    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match tokio::net::TcpStream::connect(listen).await {
            Ok(_) => return Ok(child),
            Err(error) if Instant::now() >= deadline => {
                let _ = child.kill().await;
                return Err(format!("worker did not accept on {listen}: {error}"));
            }
            Err(_) => {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!("worker exited during startup with {status}"));
                }
                tokio::time::sleep(READY_POLL).await;
            }
        }
    }
}

/// Owns the worker process for the life of the daemon and replaces it when it
/// exits. Readiness is re-checked by [`launch`] on every attempt, so a restart
/// only reports healthy once the new process actually accepts on its port.
///
/// `relaunch` is a parameter so tests can drive the loop without a real
/// worker binary; production passes [`launch`].
async fn supervise<F, Fut>(
    binary: &Path,
    listen: SocketAddr,
    state: Arc<CuaState>,
    mut child: Option<Child>,
    mut attempts: u32,
    relaunch: F,
) where
    F: Fn(PathBuf, SocketAddr) -> Fut,
    Fut: std::future::Future<Output = Result<Child, String>>,
{
    loop {
        match child.take() {
            Some(mut running) => {
                let started = Instant::now();
                let exit = match running.wait().await {
                    Ok(status) => format!("worker exited with {status}"),
                    Err(error) => format!("cannot read worker status: {error}"),
                };
                update(&state.status, |status| {
                    status.running = false;
                    status.detail = Some(format!("{exit}; restarting"));
                    status.restarts = status.restarts.saturating_add(1);
                });
                // A worker that had been up for a while earns a quick retry; a
                // crash loop backs off.
                attempts = if started.elapsed() >= STABLE_AFTER {
                    1
                } else {
                    attempts.saturating_add(1)
                };
                tokio::time::sleep(backoff(attempts)).await;
            }
            None => match relaunch(binary.to_path_buf(), listen).await {
                Ok(worker) => {
                    attempts = 0;
                    update(&state.status, |status| {
                        status.running = true;
                        status.detail = None;
                    });
                    child = Some(worker);
                }
                Err(error) => {
                    attempts = attempts.saturating_add(1);
                    update(&state.status, |status| {
                        status.running = false;
                        status.detail =
                            Some(format!("cannot restart {}: {error}", binary.display()));
                    });
                    tokio::time::sleep(backoff(attempts)).await;
                }
            },
        }
    }
}

fn update(status: &Mutex<WorkerStatus>, change: impl FnOnce(&mut WorkerStatus)) {
    match status.lock() {
        Ok(mut status) => change(&mut status),
        Err(poisoned) => change(&mut poisoned.into_inner()),
    }
}

fn backoff(attempts: u32) -> Duration {
    let doublings = attempts.saturating_sub(1).min(6);
    (RESTART_BASE * (1 << doublings)).min(RESTART_MAX)
}

impl Extension for CuaExtension {
    fn name(&self) -> &str {
        "cua"
    }

    fn router(&self) -> Router {
        let mut router = Router::new()
            .route("/health", get(health))
            .route("/info", get(info));
        // Mounted only when a proxy token was configured. An enabled route
        // without a credential would put a desktop behind envd's door.
        if self.state.proxy.is_some() {
            router = router.route(
                PROXY_PATH,
                post(proxy).layer(DefaultBodyLimit::max(PROXY_REQUEST_LIMIT)),
            );
        }
        router.with_state(Arc::clone(&self.state))
    }

    fn health(&self) -> Health {
        match self.state.status.lock() {
            Ok(status) => status.health(),
            Err(_) => Health::unhealthy("worker state is poisoned"),
        }
    }
}

async fn health(State(state): State<Arc<CuaState>>) -> Response {
    let (health, restarts) = match state.status.lock() {
        Ok(status) => (status.health(), status.restarts),
        Err(_) => (Health::unhealthy("worker state is poisoned"), 0),
    };
    let status = match health.state {
        super::HealthState::Healthy => StatusCode::OK,
        super::HealthState::Degraded | super::HealthState::Unhealthy => {
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    (
        status,
        Json(json!({
            "status": match health.state {
                super::HealthState::Healthy => "healthy",
                super::HealthState::Degraded => "degraded",
                super::HealthState::Unhealthy => "unhealthy",
            },
            "detail": health.detail,
            "restarts": restarts,
            "mcp": state.mcp_url,
        })),
    )
        .into_response()
}

async fn info(State(state): State<Arc<CuaState>>) -> Response {
    // The token is never echoed: a discovery document that leaks it would
    // hand every guest process control of the desktop.
    let mut body = json!({
        "name": "cua",
        "mcp_url": state.mcp_url,
        "transport": "mcp-jsonrpc-http",
        "protocol": "legacy",
        // Whether callers must present a bearer token. This reveals only the
        // requirement, never the value, and reflects CUA_ENV_TOKEN (the
        // credential this extension supervises and injects).
        "auth": if state.worker_token.is_some() { "bearer" } else { "none" },
    });
    if state.proxy.is_some() {
        // Relative on purpose: envd does not know the address a caller reached
        // it on, so publishing an absolute URL here would guess.
        body["proxy_path"] = json!(format!("/cua{PROXY_PATH}"));
        body["proxy_auth"] = json!("bearer");
    }
    Json(body).into_response()
}

/// Streams one MCP request to the loopback worker.
///
/// This exists so a remote caller needs no extra port: the worker keeps
/// listening on loopback, and envd — the surface the deployment already
/// exposes — is the door. The caller presents the proxy token; the worker's
/// own token, if it has one, is injected here and never travels outward.
async fn proxy(State(state): State<Arc<CuaState>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(proxy) = state.proxy.as_ref() else {
        return error(StatusCode::NOT_FOUND, "cua proxy is not enabled");
    };
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if presented.is_empty() || presented.as_bytes().ct_eq(&proxy.token).unwrap_u8() != 1 {
        let mut response = error(
            StatusCode::UNAUTHORIZED,
            "missing or invalid cua proxy token",
        );
        response.headers_mut().insert(
            axum::http::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer"),
        );
        return response;
    }

    let healthy = match state.status.lock() {
        Ok(status) => status.health().state == super::HealthState::Healthy,
        Err(_) => false,
    };
    if !healthy {
        return error(StatusCode::SERVICE_UNAVAILABLE, "cua worker is not running");
    }

    let exchange = forward(&state, &headers, body);
    match tokio::time::timeout(PROXY_TIMEOUT, exchange).await {
        Ok(Ok(forwarded)) => {
            let mut response = Response::new(axum::body::Body::from(forwarded.body));
            *response.status_mut() = forwarded.status;
            if let Some(content_type) = forwarded.content_type {
                if let Ok(value) = HeaderValue::from_str(&content_type) {
                    response
                        .headers_mut()
                        .insert(axum::http::header::CONTENT_TYPE, value);
                }
            }
            response
        }
        Ok(Err(message)) => error(StatusCode::BAD_GATEWAY, message),
        Err(_) => error(
            StatusCode::GATEWAY_TIMEOUT,
            "cua worker did not answer in time",
        ),
    }
}

struct Forwarded {
    status: StatusCode,
    content_type: Option<String>,
    body: Bytes,
}

async fn forward(state: &CuaState, headers: &HeaderMap, body: Bytes) -> Result<Forwarded, String> {
    let uri = state
        .mcp_url
        .parse::<hyper::Uri>()
        .map_err(|error| format!("invalid worker endpoint: {error}"))?;
    let host = uri
        .host()
        .ok_or_else(|| "worker endpoint has no host".to_owned())?
        .to_owned();
    let port = uri.port_u16().unwrap_or(80);

    let stream = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|error| format!("cua worker unreachable: {error}"))?;
    let _ = stream.set_nodelay(true);
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|error| format!("cua worker handshake: {error}"))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let mut builder = hyper::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    // Only protocol headers travel. The caller's credential must not: it is
    // the proxy's, and the worker has no use for it.
    for name in ["mcp-protocol-version", "mcp-session-id"] {
        if let Some(value) = headers.get(name) {
            builder = builder.header(name, value);
        }
    }
    if let Some(token) = &state.worker_token {
        let mut value = b"Bearer ".to_vec();
        value.extend_from_slice(token);
        builder = builder.header("authorization", value);
    }
    let request = builder
        .body(Full::new(body))
        .map_err(|error| format!("cannot build the worker request: {error}"))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|error| format!("cua worker request failed: {error}"))?;

    let status = response.status();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let collected = response
        .into_body()
        .collect()
        .await
        .map_err(|error| format!("reading the worker response failed: {error}"))?
        .to_bytes();
    if collected.len() > PROXY_RESPONSE_LIMIT {
        return Err(format!(
            "worker response is {} bytes, past the {PROXY_RESPONSE_LIMIT}-byte limit",
            collected.len()
        ));
    }
    Ok(Forwarded {
        status,
        content_type,
        body: collected,
    })
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ext::Extensions;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn free_listen() -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        address
    }

    #[tokio::test]
    async fn a_running_worker_is_healthy_and_discoverable() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus::running()),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: None,
                worker_token: None,
            }),
        };
        assert_eq!(extension.health(), Health::healthy());
        assert_eq!(extension.mcp_url(), "http://127.0.0.1:1/mcp");

        let extensions = Extensions::new(vec![Arc::new(extension)]).unwrap();
        let router = extensions.mount(Router::new());

        let response = router
            .clone()
            .oneshot(Request::get("/cua/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["mcp"], "http://127.0.0.1:1/mcp");

        let response = router
            .oneshot(Request::get("/cua/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("token"));
    }

    #[tokio::test]
    async fn a_failed_worker_is_unhealthy_not_fatal() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus::stopped("cannot spawn /missing")),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: None,
                worker_token: None,
            }),
        };
        assert_eq!(
            extension.health(),
            Health::unhealthy("cannot spawn /missing")
        );
    }

    #[test]
    fn restart_backoff_doubles_and_caps() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(2));
        assert_eq!(backoff(6), Duration::from_secs(16));
        assert_eq!(backoff(7), Duration::from_secs(30));
        assert_eq!(backoff(1_000), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn health_reports_the_restart_count() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus {
                    running: false,
                    detail: Some("worker exited with exit status: 1; restarting".to_owned()),
                    restarts: 3,
                }),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: None,
                worker_token: None,
            }),
        };
        let router = Extensions::new(vec![Arc::new(extension)])
            .unwrap()
            .mount(Router::new());
        let response = router
            .oneshot(Request::get("/cua/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["status"], "unhealthy");
        assert_eq!(body["restarts"], 3);
        assert!(body["detail"].as_str().unwrap().contains("restarting"));
    }

    #[tokio::test]
    async fn supervisor_replaces_a_worker_that_exits() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let state = Arc::new(CuaState {
            status: Mutex::new(WorkerStatus::running()),
            mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
            proxy: None,
            worker_token: None,
        });
        let exits = Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let launches = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&launches);
        let worker = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                supervise(
                    &PathBuf::from("/fake/coorworker-cua"),
                    "127.0.0.1:1".parse().unwrap(),
                    state,
                    Some(exits),
                    0,
                    move |_, _| {
                        let counted = Arc::clone(&counted);
                        async move {
                            counted.fetch_add(1, Ordering::SeqCst);
                            Command::new("sleep")
                                .arg("30")
                                .kill_on_drop(true)
                                .spawn()
                                .map_err(|error| error.to_string())
                        }
                    },
                )
                .await;
            })
        };
        // The first child exits at once; the replacement follows the first
        // backoff step (500 ms).
        for _ in 0..60 {
            if launches.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (restarts, running) = {
            let status = state.status.lock().unwrap();
            (status.restarts, status.running)
        };
        worker.abort();
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert!(restarts >= 1, "restarts = {restarts}");
        assert!(running, "a replacement should be recorded as running");
    }

    #[tokio::test]
    async fn spawn_reports_a_missing_binary() {
        let extension = CuaExtension::spawn(
            PathBuf::from("/definitely/not/coorworker-cua"),
            free_listen(),
        )
        .await;
        assert_eq!(extension.health().state, crate::ext::HealthState::Unhealthy);
    }

    async fn fake_mcp(
        State(seen): State<Arc<Mutex<Vec<Option<String>>>>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        seen.lock().unwrap().push(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        );
        if body.as_ref() == b"bad" {
            return error(StatusCode::BAD_REQUEST, "bad json");
        }
        (
            StatusCode::OK,
            Json(json!({"jsonrpc": "2.0", "id": 1,
                        "result": {"body": String::from_utf8_lossy(&body)}})),
        )
            .into_response()
    }

    async fn fake_mcp_worker() -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/mcp", post(fake_mcp))
            .with_state(Arc::clone(&seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}/mcp"), seen)
    }

    async fn proxied(mcp_url: String, proxy: &[u8], worker_token: Option<&[u8]>) -> CuaExtension {
        CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus::running()),
                mcp_url,
                proxy: Some(Proxy {
                    token: proxy.to_vec(),
                }),
                worker_token: worker_token.map(<[u8]>::to_vec),
            }),
        }
    }

    fn mcp_request(token: Option<&str>, body: &'static str) -> Request<Body> {
        let mut builder = Request::post("/cua/mcp");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body)).unwrap()
    }

    #[tokio::test]
    async fn the_proxy_route_exists_only_with_a_token_and_checks_it() {
        // Without a configured token there is no route at all, not an open one.
        let plain = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus::running()),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: None,
                worker_token: None,
            }),
        };
        let router = Extensions::new(vec![Arc::new(plain)])
            .unwrap()
            .mount(Router::new());
        let response = router
            .oneshot(mcp_request(Some("anything"), "{}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let (url, seen) = fake_mcp_worker().await;
        let router = Extensions::new(vec![Arc::new(proxied(url, b"proxy-secret", None).await)])
            .unwrap()
            .mount(Router::new());
        let rejected = router
            .clone()
            .oneshot(mcp_request(None, "{}"))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            rejected.headers().get("www-authenticate").unwrap(),
            "Bearer"
        );
        let rejected = router
            .clone()
            .oneshot(mcp_request(Some("wrong"), "{}"))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .oneshot(Request::get("/cua/mcp").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(
            seen.lock().unwrap().is_empty(),
            "a rejected request must never reach the worker"
        );
    }

    #[tokio::test]
    async fn the_proxy_forwards_the_call_and_injects_the_worker_token() {
        let (url, seen) = fake_mcp_worker().await;
        let router = Extensions::new(vec![Arc::new(
            proxied(url, b"proxy-secret", Some(b"worker-secret")).await,
        )])
        .unwrap()
        .mount(Router::new());

        let response = router
            .clone()
            .oneshot(mcp_request(
                Some("proxy-secret"),
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("tools/list"));

        // The worker's status is passed through, not flattened to 200/502.
        let response = router
            .oneshot(mcp_request(Some("proxy-secret"), "bad"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            [
                Some("Bearer worker-secret".to_owned()),
                Some("Bearer worker-secret".to_owned())
            ],
            "the proxy must inject the worker token, never forward its own"
        );
    }

    #[tokio::test]
    async fn the_proxy_reports_an_unreachable_worker() {
        let router = Extensions::new(vec![Arc::new(
            proxied(
                format!("http://{}/mcp", free_listen()),
                b"proxy-secret",
                None,
            )
            .await,
        )])
        .unwrap()
        .mount(Router::new());
        let response = router
            .oneshot(mcp_request(Some("proxy-secret"), "{}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn the_proxy_refuses_a_dead_worker() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus::stopped("worker exited with exit status: 1")),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: Some(Proxy {
                    token: b"proxy-secret".to_vec(),
                }),
                worker_token: None,
            }),
        };
        let router = Extensions::new(vec![Arc::new(extension)])
            .unwrap()
            .mount(Router::new());
        let response = router
            .oneshot(mcp_request(Some("proxy-secret"), "{}"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn info_advertises_the_proxy_only_when_enabled() {
        let (url, _seen) = fake_mcp_worker().await;
        let router = Extensions::new(vec![Arc::new(proxied(url, b"proxy-secret", None).await)])
            .unwrap()
            .mount(Router::new());
        let response = router
            .oneshot(Request::get("/cua/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["name"], "cua");
        assert_eq!(body["auth"], "none");
        assert_eq!(body["proxy_path"], "/cua/mcp");
        assert_eq!(body["proxy_auth"], "bearer");
        assert!(
            !body.to_string().contains("proxy-secret"),
            "the discovery document must not carry the token"
        );
    }

    #[tokio::test]
    async fn info_reports_that_the_worker_requires_a_token() {
        let (url, _seen) = fake_mcp_worker().await;
        let router = Extensions::new(vec![Arc::new(
            proxied(url, b"proxy-secret", Some(b"worker-secret")).await,
        )])
        .unwrap()
        .mount(Router::new());
        let response = router
            .oneshot(Request::get("/cua/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["auth"], "bearer");
        assert!(
            !body.to_string().contains("worker-secret"),
            "the discovery document must not carry the token"
        );
    }
}
