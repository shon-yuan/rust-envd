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
//! supervisor replaces a worker that exits (exponential backoff with bounded
//! jitter, reset after a stable run) and stops relaunching after
//! `COORWORKER_CUA_MAX_RESTARTS` consecutive failures. A worker that fails to
//! start leaves the extension unhealthy and visible, and never affects the
//! envd surface.
//!
//! With `COORWORKER_CUA_EXTERNAL` the extension instead **adopts** a worker it
//! does not own: it probes `GET /health` on the same address and reports the
//! result, but never spawns, kills or restarts it. This is the Windows service
//! shape — the worker belongs to the logged-in session, envd stays in session
//! 0 — and it also fits any deployment that starts the worker itself.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
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
/// Consecutive failed launches/exits after which the supervisor stops
/// relaunching. Zero (or `COORWORKER_CUA_MAX_RESTARTS=0`) keeps the old
/// infinite retry. A stable run clears the count, so this bounds a crash loop,
/// not a worker that restarts occasionally.
const MAX_RESTART_ATTEMPTS_DEFAULT: u32 = 8;
/// Distinct tool names kept in the proxy metrics. Past this, every unknown
/// name is attributed to `other`, so a hostile body cannot grow the label set.
const TOOL_METRIC_NAMES: usize = 64;
/// How often an adopted worker is probed. The worker answers `/health`
/// without touching the desktop, so two seconds is cheap and keeps a
/// `/cua/health` caller from reading a stale state for long.
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Adopted mode: how many times a worker envd does not own was seen to be
    /// healthy again after it had been healthy and then stopped answering.
    /// Never mixed with [`WorkerStatus::restarts`].
    observed_restarts: u32,
    /// Spawn mode: the supervisor gave up after its failure budget. Recovery
    /// is manual (restart envd or fix the worker); nothing retries behind it.
    budget_exhausted: bool,
}

impl WorkerStatus {
    fn running() -> Self {
        Self {
            running: true,
            detail: None,
            restarts: 0,
            observed_restarts: 0,
            budget_exhausted: false,
        }
    }

    fn stopped(detail: impl Into<String>) -> Self {
        Self {
            running: false,
            detail: Some(detail.into()),
            restarts: 0,
            observed_restarts: 0,
            budget_exhausted: false,
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

/// Counters for calls the optional proxy forwards. Only `/cua/mcp` traffic is
/// counted: a caller that reaches the worker directly, or the ToolHost WS
/// relay, is invisible here and the docs say so.
#[derive(Default)]
struct ToolMetrics {
    counts: BTreeMap<(String, bool), u64>,
    names: BTreeSet<String>,
    latency: Duration,
}

impl ToolMetrics {
    fn record(&mut self, calls: &[String], ok: bool, elapsed: Duration) {
        self.latency += elapsed;
        for call in calls {
            let name = if self.names.contains(call) || self.names.len() < TOOL_METRIC_NAMES {
                self.names.insert(call.clone());
                call.clone()
            } else {
                "other".to_owned()
            };
            *self.counts.entry((name, ok)).or_insert(0) += 1;
        }
    }

    fn exposition(&self) -> String {
        let mut body = String::from(
            "# HELP coorworker_envd_cua_tool_calls_total Tool calls forwarded through /cua/mcp, by tool and outcome. Direct worker access and the ToolHost relay are not counted.\n\
             # TYPE coorworker_envd_cua_tool_calls_total counter\n",
        );
        for ((tool, ok), count) in &self.counts {
            body.push_str(&format!(
                "coorworker_envd_cua_tool_calls_total{{tool=\"{}\",status=\"{}\"}} {count}\n",
                metric_label(tool),
                if *ok { "ok" } else { "error" }
            ));
        }
        body.push_str(&format!(
            "# HELP coorworker_envd_cua_tool_latency_seconds_sum Cumulative time spent forwarding tool calls through /cua/mcp.\n\
             # TYPE coorworker_envd_cua_tool_latency_seconds_sum counter\n\
             coorworker_envd_cua_tool_latency_seconds_sum {:.6}\n",
            self.latency.as_secs_f64()
        ));
        body
    }
}

/// Prometheus label values escape backslash, quote and newline. Tool names come
/// from a caller's JSON, so they are untrusted text even after the name cap.
fn metric_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Tool names in a proxied JSON-RPC body. A batch is walked item by item; a
/// body that is not JSON, or a method other than `tools/call`, counts nothing.
fn proxied_tool_calls(body: &[u8]) -> Vec<String> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let items: Vec<&serde_json::Value> = match &value {
        serde_json::Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter(|item| item.get("method").and_then(|m| m.as_str()) == Some("tools/call"))
        .filter_map(|item| {
            item.get("params")
                .and_then(|params| params.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_owned)
        })
        .collect()
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
    /// Proxy-forwarded call counters. Present even when the proxy is absent so
    /// every constructor has one shape; it then stays empty.
    tool_metrics: Mutex<ToolMetrics>,
}

/// How the extension gets its worker.
enum Launch {
    /// envd owns the process and replaces it when it exits.
    Spawn(PathBuf),
    /// A worker started elsewhere is adopted; envd only probes it.
    External,
}

/// `COORWORKER_CUA_EXTERNAL` selects adoption and wins when both variables are
/// set — a mode switch, not a second wire. Spawning is refused outright when
/// `allow_spawn` is false (SCM service mode): a process started from session 0
/// has no interactive desktop to drive.
fn resolve_launch(binary: Option<PathBuf>, external: bool, allow_spawn: bool) -> Option<Launch> {
    if external {
        return Some(Launch::External);
    }
    match binary {
        Some(binary) if allow_spawn => Some(Launch::Spawn(binary)),
        _ => None,
    }
}

/// `COORWORKER_CUA_MAX_RESTARTS`: consecutive failed launches/exits before the
/// supervisor gives up. Unset uses the default; `0` restores unlimited retry.
fn parse_max_attempts(value: Option<std::ffi::OsString>) -> Result<u32, String> {
    match value {
        None => Ok(MAX_RESTART_ATTEMPTS_DEFAULT),
        Some(value) => {
            let text = value.to_string_lossy();
            let text = text.trim();
            if text.is_empty() {
                return Ok(MAX_RESTART_ATTEMPTS_DEFAULT);
            }
            text.parse::<u32>()
                .map_err(|error| format!("invalid COORWORKER_CUA_MAX_RESTARTS {text:?}: {error}"))
        }
    }
}

/// Supervises one `coorworker-cua` process, or watches one it does not own.
pub struct CuaExtension {
    state: Arc<CuaState>,
}

impl CuaExtension {
    /// Spawns the worker and waits until its loopback listener accepts, up to
    /// five seconds. A worker that cannot start is reported through
    /// [`Extension::health`] instead of aborting the daemon.
    pub async fn spawn(binary: PathBuf, listen: SocketAddr) -> Self {
        Self::build(binary, listen, None, MAX_RESTART_ATTEMPTS_DEFAULT).await
    }

    /// Spawns the worker and additionally exposes it as `POST /cua/mcp`.
    ///
    /// `proxy_token` is the guest-facing bearer token. The worker's own token,
    /// if any, comes from `CUA_ENV_TOKEN` — the same variable its child process
    /// inherits — and is injected on every forwarded request, so the caller
    /// never needs it.
    pub async fn spawn_proxied(binary: PathBuf, listen: SocketAddr, proxy_token: Vec<u8>) -> Self {
        Self::build(
            binary,
            listen,
            Some(proxy_token),
            MAX_RESTART_ATTEMPTS_DEFAULT,
        )
        .await
    }

    async fn build(
        binary: PathBuf,
        listen: SocketAddr,
        proxy_token: Option<Vec<u8>>,
        max_attempts: u32,
    ) -> Self {
        // The first launch stays synchronous (and waits for the listener) so a
        // caller that spawns the extension knows whether it came up. After
        // that the supervisor owns the process and replaces it when it exits.
        let (status, child, attempts) = match launch(&binary, listen).await {
            Ok(child) => (WorkerStatus::running(), Some(child), 0),
            Err(error) => (WorkerStatus::stopped(error), None, 1),
        };
        let state = shared_state(listen, proxy_token, status);
        let watched = Arc::clone(&state);
        tokio::spawn(async move {
            supervise(
                &binary,
                listen,
                watched,
                child,
                attempts,
                max_attempts,
                |binary, listen| async move { launch(&binary, listen).await },
            )
            .await;
        });
        Self { state }
    }

    /// Adopts a worker envd does not own. The worker is expected to be started
    /// by the deployment — on Windows, a logon task in the interactive session
    /// while envd stays in session 0. The probe runs immediately and then every
    /// [`PROBE_INTERVAL`]; `restarts` stays zero because envd never replaces
    /// this process.
    pub async fn adopt(listen: SocketAddr, proxy_token: Option<Vec<u8>>) -> Self {
        let status = match probe(listen).await {
            None => WorkerStatus::running(),
            Some(detail) => WorkerStatus::stopped(detail),
        };
        let state = shared_state(listen, proxy_token, status);
        let watched = Arc::clone(&state);
        tokio::spawn(async move { watch(listen, watched).await });
        Self { state }
    }

    /// `Some` when the worker is configured: `COORWORKER_CUA_BIN` spawns one,
    /// `COORWORKER_CUA_EXTERNAL` adopts one. The value `None` is the parity
    /// build: no extension, no routes, no child.
    pub async fn from_env() -> Option<Self> {
        Self::from_env_with(true).await
    }

    /// The service-mode entry point. A process spawned from session 0 cannot
    /// reach the interactive desktop, so only `COORWORKER_CUA_EXTERNAL` is
    /// honoured and `COORWORKER_CUA_BIN` alone yields no extension.
    pub async fn from_env_external_only() -> Option<Self> {
        Self::from_env_with(false).await
    }

    async fn from_env_with(allow_spawn: bool) -> Option<Self> {
        let external = std::env::var_os("COORWORKER_CUA_EXTERNAL").is_some_and(|value| {
            let value = value.to_string_lossy();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        });
        let binary = std::env::var_os("COORWORKER_CUA_BIN").map(PathBuf::from);
        let launch = resolve_launch(binary, external, allow_spawn)?;
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
        let max_attempts = match parse_max_attempts(std::env::var_os("COORWORKER_CUA_MAX_RESTARTS"))
        {
            Ok(value) => value,
            Err(error) => return Some(Self::unavailable(error)),
        };
        Some(match launch {
            Launch::Spawn(binary) => Self::build(binary, listen, proxy_token, max_attempts).await,
            Launch::External => Self::adopt(listen, proxy_token).await,
        })
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
                tool_metrics: Mutex::new(ToolMetrics::default()),
            }),
        }
    }

    pub fn mcp_url(&self) -> &str {
        &self.state.mcp_url
    }
}

/// The state shared by both modes. An empty proxy token means "not
/// configured": the route stays absent rather than open with nothing to check.
fn shared_state(
    listen: SocketAddr,
    proxy_token: Option<Vec<u8>>,
    status: WorkerStatus,
) -> Arc<CuaState> {
    let proxy = proxy_token
        .filter(|token| !token.is_empty())
        .map(|token| Proxy { token });
    let worker_token = std::env::var_os("CUA_ENV_TOKEN")
        .map(|value| value.to_string_lossy().into_owned().into_bytes())
        .filter(|token| !token.is_empty());
    Arc::new(CuaState {
        status: Mutex::new(status),
        mcp_url: format!("http://{listen}/mcp"),
        proxy,
        worker_token,
        tool_metrics: Mutex::new(ToolMetrics::default()),
    })
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

/// Watches an adopted worker. Only the snapshot changes; the worker's process
/// belongs to someone else, so there is nothing here to restart.
async fn watch(listen: SocketAddr, state: Arc<CuaState>) {
    // A worker that was healthy at adoption and later comes back is a restart
    // envd observed but did not perform; one that was never up is a start.
    let mut ever_healthy = match state.status.lock() {
        Ok(status) => status.running,
        Err(poisoned) => poisoned.into_inner().running,
    };
    loop {
        tokio::time::sleep(PROBE_INTERVAL).await;
        let detail = probe(listen).await;
        let healthy = detail.is_none();
        // Pass the prior history: the first healthy sighting after adoption is
        // a start, not a recovery.
        update(&state.status, |status| {
            observe(status, ever_healthy, detail)
        });
        ever_healthy |= healthy;
    }
}

/// Apply one adoption probe. A worker that was healthy before and is healthy
/// again after an unhealthy observation counts one observed restart; a worker
/// seen healthy for the first time is a start, not a restart.
fn observe(status: &mut WorkerStatus, ever_healthy: bool, detail: Option<String>) {
    match detail {
        Some(detail) => {
            status.running = false;
            status.detail = Some(detail);
        }
        None => {
            if ever_healthy && !status.running {
                status.observed_restarts = status.observed_restarts.saturating_add(1);
            }
            status.running = true;
            status.detail = None;
        }
    }
}

/// `None` when the worker answers `GET /health` with `204`; otherwise the
/// reason. An adopted worker is judged by the same contract a caller sees, so
/// a port that accepts but no longer serves is not reported healthy.
async fn probe(listen: SocketAddr) -> Option<String> {
    match tokio::time::timeout(PROBE_TIMEOUT, probe_once(listen)).await {
        Ok(Ok(())) => None,
        Ok(Err(detail)) => Some(detail),
        Err(_) => Some(format!(
            "cua worker at {listen} did not answer in {} ms",
            PROBE_TIMEOUT.as_millis()
        )),
    }
}

async fn probe_once(listen: SocketAddr) -> Result<(), String> {
    let stream = tokio::net::TcpStream::connect(listen)
        .await
        .map_err(|error| format!("cua worker unreachable at {listen}: {error}"))?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|error| format!("cua worker handshake at {listen}: {error}"))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = hyper::Request::builder()
        .method("GET")
        .uri(format!("http://{listen}/health"))
        .body(Full::new(Bytes::new()))
        .map_err(|error| format!("cannot build the worker probe: {error}"))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|error| format!("cua worker probe failed at {listen}: {error}"))?;
    if response.status() != StatusCode::NO_CONTENT {
        return Err(format!("cua worker /health answered {}", response.status()));
    }
    Ok(())
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
    max_attempts: u32,
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
                // A worker that had been up for a while earns a fresh budget
                // and a quick retry; a crash loop backs off and is bounded.
                attempts = attempts_after_exit(started.elapsed() >= STABLE_AFTER, attempts);
                let exhausted = budget_spent(attempts, max_attempts);
                update(&state.status, |status| {
                    status.running = false;
                    status.restarts = status.restarts.saturating_add(1);
                    status.budget_exhausted = exhausted;
                    status.detail = Some(if exhausted {
                        format!("restart budget exhausted after {attempts} attempts: {exit}")
                    } else {
                        format!("{exit}; restarting")
                    });
                });
                if exhausted {
                    return;
                }
                tokio::time::sleep(backoff(attempts)).await;
            }
            None => {
                if budget_spent(attempts, max_attempts) {
                    update(&state.status, |status| {
                        let reason = status
                            .detail
                            .clone()
                            .unwrap_or_else(|| "worker is not running".to_owned());
                        status.budget_exhausted = true;
                        status.detail = Some(format!(
                            "restart budget exhausted after {attempts} attempts: {reason}"
                        ));
                    });
                    return;
                }
                match relaunch(binary.to_path_buf(), listen).await {
                    Ok(worker) => {
                        // Readiness is not stability: retain failures until
                        // a worker survives STABLE_AFTER and then exits.
                        update(&state.status, |status| {
                            status.running = true;
                            status.detail = None;
                            status.budget_exhausted = false;
                        });
                        child = Some(worker);
                    }
                    Err(error) => {
                        attempts = attempts.saturating_add(1);
                        let exhausted = budget_spent(attempts, max_attempts);
                        update(&state.status, |status| {
                            status.running = false;
                            status.budget_exhausted = exhausted;
                            status.detail = Some(if exhausted {
                                format!(
                                    "restart budget exhausted after {attempts} attempts: cannot restart {}: {error}",
                                    binary.display()
                                )
                            } else {
                                format!("cannot restart {}: {error}", binary.display())
                            });
                        });
                        if exhausted {
                            return;
                        }
                        tokio::time::sleep(backoff(attempts)).await;
                    }
                }
            }
        }
    }
}

/// Consecutive failures carried into the next cycle. A stable run clears the
/// budget; an immediate exit consumes one attempt.
fn attempts_after_exit(stable: bool, attempts: u32) -> u32 {
    if stable {
        0
    } else {
        attempts.saturating_add(1)
    }
}

/// Whether the supervisor must stop. Zero means unlimited (the pre-budget
/// behaviour, opt-in through the environment).
fn budget_spent(attempts: u32, max_attempts: u32) -> bool {
    max_attempts != 0 && attempts >= max_attempts
}

fn update(status: &Mutex<WorkerStatus>, change: impl FnOnce(&mut WorkerStatus)) {
    match status.lock() {
        Ok(mut status) => change(&mut status),
        Err(poisoned) => change(&mut poisoned.into_inner()),
    }
}

fn backoff(attempts: u32) -> Duration {
    backoff_with(attempts, jitter_unit())
}

/// Bounded jitter: each step waits between half and 1.5× its nominal delay,
/// then the 30 s cap applies. `unit` is in `[0, 1]`; tests pass a fixed value
/// instead of drawing from [`jitter_unit`].
fn backoff_with(attempts: u32, unit: f64) -> Duration {
    let doublings = attempts.saturating_sub(1).min(6);
    let nominal = RESTART_BASE * (1u32 << doublings);
    nominal.mul_f64(0.5 + unit.clamp(0.0, 1.0)).min(RESTART_MAX)
}

/// xorshift64 state for restart jitter. Not cryptographic: it only spreads
/// simultaneous envd starts so they do not retry in lockstep.
static JITTER: AtomicU64 = AtomicU64::new(0);

fn jitter_unit() -> f64 {
    let mut seed = JITTER.load(Ordering::Relaxed);
    if seed == 0 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or(0);
        seed = (nanos ^ (u64::from(std::process::id()) << 32)) | 1;
    }
    let mut x = seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    JITTER.store(x, Ordering::Relaxed);
    // 53 bits is exactly the f64 mantissa, so the result is in [0, 1).
    (x >> 11) as f64 / (1u64 << 53) as f64
}

impl Extension for CuaExtension {
    fn name(&self) -> &str {
        "cua"
    }

    fn router(&self) -> Router {
        let mut router = Router::new()
            .route("/health", get(health))
            .route("/info", get(info))
            .route("/metrics", get(metrics));
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
    let (health, restarts, observed_restarts, budget_exhausted) = match state.status.lock() {
        Ok(status) => (
            status.health(),
            status.restarts,
            status.observed_restarts,
            status.budget_exhausted,
        ),
        Err(_) => (Health::unhealthy("worker state is poisoned"), 0, 0, false),
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
            "observed_restarts": observed_restarts,
            "budget_exhausted": budget_exhausted,
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

/// Prometheus text exposition for the extension. Kept on the extension's own
/// path: the S2 `/metrics` document is the oracle's JSON wire format and stays
/// byte-identical, so extension counters never enter it.
async fn metrics(State(state): State<Arc<CuaState>>) -> Response {
    let (running, restarts, observed_restarts, budget_exhausted) = match state.status.lock() {
        Ok(status) => (
            status.running,
            status.restarts,
            status.observed_restarts,
            status.budget_exhausted,
        ),
        Err(_) => (false, 0, 0, false),
    };
    let tool_metrics = match state.tool_metrics.lock() {
        Ok(metrics) => metrics.exposition(),
        Err(poisoned) => poisoned.into_inner().exposition(),
    };
    let body = format!(
        "# HELP coorworker_envd_cua_worker_up 1 when the supervised cua worker is running.\n\
         # TYPE coorworker_envd_cua_worker_up gauge\n\
         coorworker_envd_cua_worker_up {}\n\
         # HELP coorworker_envd_cua_worker_restarts_total Worker exits replaced by the supervisor since envd started. Stays 0 for an adopted worker envd does not own.\n\
         # TYPE coorworker_envd_cua_worker_restarts_total counter\n\
         coorworker_envd_cua_worker_restarts_total {restarts}\n\
         # HELP coorworker_envd_cua_worker_observed_restarts_total Healthy-to-unhealthy-to-healthy recoveries observed while adopting a worker envd does not own.\n\
         # TYPE coorworker_envd_cua_worker_observed_restarts_total counter\n\
         coorworker_envd_cua_worker_observed_restarts_total {observed_restarts}\n\
         # HELP coorworker_envd_cua_worker_budget_exhausted 1 when the supervisor stopped relaunching after its failure budget.\n\
         # TYPE coorworker_envd_cua_worker_budget_exhausted gauge\n\
         coorworker_envd_cua_worker_budget_exhausted {}\n{tool_metrics}",
        if running { 1 } else { 0 },
        if budget_exhausted { 1 } else { 0 },
    );
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        body,
    )
        .into_response()
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

    // Only `tools/call` requests are counted; discovery and protocol traffic
    // would otherwise pad the series with names that are not tools.
    let calls = proxied_tool_calls(&body);
    let started = Instant::now();
    let record = |ok: bool| {
        if calls.is_empty() {
            return;
        }
        let elapsed = started.elapsed();
        match state.tool_metrics.lock() {
            Ok(mut metrics) => metrics.record(&calls, ok, elapsed),
            Err(poisoned) => poisoned.into_inner().record(&calls, ok, elapsed),
        }
    };
    let exchange = forward(&state, &headers, body);
    match tokio::time::timeout(PROXY_TIMEOUT, exchange).await {
        Ok(Ok(forwarded)) => {
            record(forwarded_ok(&forwarded.status, &forwarded.body));
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
        Ok(Err(message)) => {
            record(false);
            error(StatusCode::BAD_GATEWAY, message)
        }
        Err(_) => {
            record(false);
            error(
                StatusCode::GATEWAY_TIMEOUT,
                "cua worker did not answer in time",
            )
        }
    }
}

/// A forwarded call counts as `ok` only when the HTTP status is 2xx and no
/// JSON-RPC item carries an error or `result.isError: true`.
fn forwarded_ok(status: &StatusCode, body: &[u8]) -> bool {
    if !status.is_success() {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let items: Vec<&serde_json::Value> = match &value {
        serde_json::Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    items.iter().all(|item| {
        item.get("error").is_none()
            && item
                .get("result")
                .and_then(|result| result.get("isError"))
                .and_then(serde_json::Value::as_bool)
                != Some(true)
    })
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

    /// A loopback address owned by this test process. Ports handed out are
    /// never returned twice: the kernel can re-issue a port the moment it is
    /// released, which made two concurrent tests race for the same stub
    /// address (`AddrInUse` on the second bind).
    fn free_listen() -> SocketAddr {
        static HANDED_OUT: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());
        for _ in 0..64 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            drop(listener);
            if HANDED_OUT.lock().unwrap().insert(address.port()) {
                return address;
            }
        }
        panic!("no unused loopback port after 64 attempts");
    }

    #[tokio::test]
    async fn a_running_worker_is_healthy_and_discoverable() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus::running()),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: None,
                worker_token: None,
                tool_metrics: Mutex::new(ToolMetrics::default()),
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
                tool_metrics: Mutex::new(ToolMetrics::default()),
            }),
        };
        assert_eq!(
            extension.health(),
            Health::unhealthy("cannot spawn /missing")
        );
    }

    #[test]
    fn restart_backoff_is_bounded_and_caps() {
        // Nominal steps double: 0.5, 0.5, 1, 2, 4, 8, 16 s below the cap.
        let nominal = [
            Duration::from_millis(500),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
        ];
        for (attempt, step) in nominal.iter().enumerate() {
            // Jitter stays within [0.5x, 1.5x] of the nominal step.
            assert_eq!(backoff_with(attempt as u32, 0.5), *step);
            assert_eq!(backoff_with(attempt as u32, 0.0), *step / 2);
            assert_eq!(backoff_with(attempt as u32, 1.0), *step * 3 / 2);
        }
        // Attempt 7's nominal step is 32 s; the 30 s cap applies after jitter.
        assert_eq!(backoff_with(7, 0.0), Duration::from_secs(16));
        assert_eq!(backoff_with(7, 0.5), Duration::from_secs(30));
        assert_eq!(backoff_with(7, 1.0), Duration::from_secs(30));
        assert_eq!(backoff_with(1_000, 0.0), Duration::from_secs(16));
        // Values outside [0,1] clamp instead of producing a negative delay.
        assert_eq!(backoff_with(1, -3.0), Duration::from_millis(250));
        assert_eq!(backoff_with(1, 9.0), Duration::from_millis(750));
    }

    #[test]
    fn the_restart_budget_counts_consecutive_failures_and_resets_after_stability() {
        assert_eq!(attempts_after_exit(false, 0), 1);
        assert_eq!(attempts_after_exit(false, 7), 8);
        assert_eq!(attempts_after_exit(true, 41), 0, "a stable run resets");
        assert!(!budget_spent(7, 8));
        assert!(budget_spent(8, 8));
        assert!(!budget_spent(1_000, 0), "0 means unlimited");
        assert!(budget_spent(1, 1));
    }

    #[test]
    fn max_restarts_environment_values_are_parsed_strictly() {
        use std::ffi::OsString;
        assert_eq!(
            parse_max_attempts(None).unwrap(),
            MAX_RESTART_ATTEMPTS_DEFAULT
        );
        assert_eq!(parse_max_attempts(Some(OsString::from(""))).unwrap(), 8);
        assert_eq!(parse_max_attempts(Some(OsString::from(" 3 "))).unwrap(), 3);
        assert_eq!(parse_max_attempts(Some(OsString::from("0"))).unwrap(), 0);
        assert!(parse_max_attempts(Some(OsString::from("many"))).is_err());
        assert!(parse_max_attempts(Some(OsString::from("-1"))).is_err());
    }

    #[test]
    fn tool_metrics_cap_names_and_attribute_overflow() {
        let mut metrics = ToolMetrics::default();
        for index in 0..TOOL_METRIC_NAMES {
            metrics.record(
                &[format!("tool_{index:02}")],
                true,
                Duration::from_millis(10),
            );
        }
        metrics.record(&["tool_00".to_owned()], false, Duration::from_millis(5));
        metrics.record(&["newcomer".to_owned()], true, Duration::from_millis(1));
        metrics.record(&["another".to_owned()], true, Duration::from_millis(1));

        assert!(metrics.names.contains("tool_00"));
        assert!(!metrics.names.contains("newcomer"));
        assert_eq!(
            metrics.counts.get(&(String::from("other"), true)),
            Some(&2),
            "unknown names collapse after the cap"
        );
        assert_eq!(
            metrics.counts.get(&(String::from("tool_00"), false)),
            Some(&1)
        );
        assert_eq!(metrics.names.len(), TOOL_METRIC_NAMES);

        let text = metrics.exposition();
        assert!(
            text.contains("coorworker_envd_cua_tool_calls_total{tool=\"tool_00\",status=\"ok\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("coorworker_envd_cua_tool_calls_total{tool=\"other\",status=\"ok\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("coorworker_envd_cua_tool_latency_seconds_sum 0.647000"),
            "{text}"
        );
    }

    #[test]
    fn proxied_tool_calls_reads_single_and_batch_requests() {
        let single = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"launch_app","arguments":{}}}"#;
        assert_eq!(proxied_tool_calls(single), vec!["launch_app".to_owned()]);
        let batch = br#"[{"jsonrpc":"2.0","id":1,"method":"initialize"},
                        {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"click"}},
                        {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"click"}}]"#;
        assert_eq!(
            proxied_tool_calls(batch),
            vec!["click".to_owned(), "click".to_owned()]
        );
        assert!(proxied_tool_calls(br#"{"method":"tools/list"}"#).is_empty());
        assert!(proxied_tool_calls(b"bad").is_empty());
        assert!(proxied_tool_calls(br#"{"method":"tools/call","params":{}}"#).is_empty());
    }

    #[test]
    fn forwarded_ok_reads_status_and_json_rpc_errors() {
        let ok = br#"{"jsonrpc":"2.0","id":1,"result":{"content":[]}}"#;
        assert!(forwarded_ok(&StatusCode::OK, ok));
        let rpc_error = br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"no"}}"#;
        assert!(!forwarded_ok(&StatusCode::OK, rpc_error));
        let tool_error = br#"{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[]}}"#;
        assert!(!forwarded_ok(&StatusCode::OK, tool_error));
        assert!(!forwarded_ok(&StatusCode::BAD_REQUEST, ok));
        assert!(!forwarded_ok(&StatusCode::OK, b"not json"));
    }

    #[test]
    fn external_mode_wins_and_service_mode_never_spawns() {
        let binary = Some(PathBuf::from("/opt/coorworker-cua"));
        assert!(matches!(
            resolve_launch(binary.clone(), true, true),
            Some(Launch::External)
        ));
        assert!(matches!(
            resolve_launch(binary.clone(), true, false),
            Some(Launch::External)
        ));
        assert!(matches!(
            resolve_launch(binary.clone(), false, true),
            Some(Launch::Spawn(_))
        ));
        assert!(resolve_launch(binary, false, false).is_none());
        assert!(resolve_launch(None, false, true).is_none());
        assert!(matches!(
            resolve_launch(None, true, false),
            Some(Launch::External)
        ));
    }

    /// A stand-in for `coorworker-cua`: answers `GET /health` with `status`.
    async fn worker_stub(listen: SocketAddr, status: StatusCode) -> tokio::task::JoinHandle<()> {
        let app = Router::new().route("/health", get(move || async move { status }));
        let listener = tokio::net::TcpListener::bind(listen).await.unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        })
    }

    #[tokio::test]
    async fn an_adopted_worker_is_probed_and_reported() {
        let listen = free_listen();
        let extension = CuaExtension::adopt(listen, None).await;
        let health = extension.health();
        assert_eq!(health.state, crate::ext::HealthState::Unhealthy);
        assert!(health.detail.contains("unreachable"), "{}", health.detail);

        // A worker that appears after adoption is picked up by the probe loop
        // and reported through the same routes a caller uses.
        let stub = worker_stub(listen, StatusCode::NO_CONTENT).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && extension.health() != Health::healthy() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(extension.health(), Health::healthy());

        let router = Extensions::new(vec![Arc::new(extension)])
            .unwrap()
            .mount(Router::new());
        let response = router
            .oneshot(Request::get("/cua/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["restarts"], 0, "envd never restarts an adopted worker");
        stub.abort();
    }

    #[tokio::test]
    async fn an_adopted_worker_that_stops_serving_is_unhealthy() {
        let listen = free_listen();
        let stub = worker_stub(listen, StatusCode::SERVICE_UNAVAILABLE).await;
        let extension = CuaExtension::adopt(listen, None).await;
        let health = extension.health();
        assert_eq!(health.state, crate::ext::HealthState::Unhealthy);
        assert!(health.detail.contains("503"), "{}", health.detail);
        stub.abort();
    }

    #[tokio::test]
    async fn health_reports_the_restart_count() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus {
                    running: false,
                    detail: Some("worker exited with exit status: 1; restarting".to_owned()),
                    restarts: 3,
                    ..WorkerStatus::running()
                }),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: None,
                worker_token: None,
                tool_metrics: Mutex::new(ToolMetrics::default()),
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
        assert_eq!(body["observed_restarts"], 0);
        assert_eq!(body["budget_exhausted"], false);
        assert!(body["detail"].as_str().unwrap().contains("restarting"));
    }

    #[tokio::test]
    async fn health_reports_an_exhausted_restart_budget() {
        let extension = CuaExtension {
            state: Arc::new(CuaState {
                status: Mutex::new(WorkerStatus {
                    running: false,
                    detail: Some("restart budget exhausted after 8 attempts: exit 1".to_owned()),
                    budget_exhausted: true,
                    ..WorkerStatus::running()
                }),
                mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                proxy: None,
                worker_token: None,
                tool_metrics: Mutex::new(ToolMetrics::default()),
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
        assert_eq!(body["budget_exhausted"], true);
        assert!(body["detail"].as_str().unwrap().contains("exhausted"));
    }

    #[tokio::test]
    async fn metrics_report_worker_state_and_restarts() {
        fn router_with(status: WorkerStatus) -> Router {
            Extensions::new(vec![Arc::new(CuaExtension {
                state: Arc::new(CuaState {
                    status: Mutex::new(status),
                    mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
                    proxy: None,
                    worker_token: None,
                    tool_metrics: Mutex::new(ToolMetrics::default()),
                }),
            })])
            .unwrap()
            .mount(Router::new())
        }
        let response = router_with(WorkerStatus {
            running: true,
            detail: None,
            restarts: 2,
            ..WorkerStatus::running()
        })
        .oneshot(Request::get("/cua/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE].to_str().unwrap(),
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.starts_with("# HELP"), "{body}");
        assert!(body.contains("coorworker_envd_cua_worker_up 1"), "{body}");
        assert!(
            body.contains("coorworker_envd_cua_worker_restarts_total 2"),
            "{body}"
        );
        assert!(
            body.contains("coorworker_envd_cua_worker_observed_restarts_total 0"),
            "{body}"
        );
        assert!(
            body.contains("coorworker_envd_cua_worker_budget_exhausted 0"),
            "{body}"
        );

        let response = router_with(WorkerStatus::stopped("worker exited"))
            .oneshot(Request::get("/cua/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("coorworker_envd_cua_worker_up 0"),
            "{body:?}"
        );
    }

    #[tokio::test]
    async fn supervisor_replaces_a_worker_that_exits() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let state = Arc::new(CuaState {
            status: Mutex::new(WorkerStatus::running()),
            mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
            proxy: None,
            worker_token: None,
            tool_metrics: Mutex::new(ToolMetrics::default()),
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
                tool_metrics: Mutex::new(ToolMetrics::default()),
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
                tool_metrics: Mutex::new(ToolMetrics::default()),
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
                tool_metrics: Mutex::new(ToolMetrics::default()),
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

    #[test]
    fn adoption_counts_recoveries_but_not_the_first_appearance() {
        let mut status = WorkerStatus::stopped("unreachable");
        // First sighting after an unhealthy adoption is a start.
        observe(&mut status, false, None);
        assert!(status.running);
        assert_eq!(status.observed_restarts, 0);

        observe(&mut status, true, Some("unreachable".to_owned()));
        assert!(!status.running);
        assert_eq!(status.observed_restarts, 0);
        observe(&mut status, true, None);
        assert!(status.running);
        assert_eq!(status.observed_restarts, 1, "one observed recovery");

        observe(&mut status, true, Some("503".to_owned()));
        observe(&mut status, false, Some("503".to_owned()));
        observe(&mut status, true, None);
        assert_eq!(status.observed_restarts, 2);
        // Adoption never fabricates a supervisor restart count.
        assert_eq!(status.restarts, 0);
    }

    #[tokio::test]
    async fn supervisor_stops_after_the_restart_budget() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let state = Arc::new(CuaState {
            status: Mutex::new(WorkerStatus::stopped("cannot spawn /missing")),
            mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
            proxy: None,
            worker_token: None,
            tool_metrics: Mutex::new(ToolMetrics::default()),
        });
        let launches = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&launches);
        let worker = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                supervise(
                    &PathBuf::from("/fake/coorworker-cua"),
                    "127.0.0.1:1".parse().unwrap(),
                    state,
                    None,
                    // The first launch already failed before supervision began.
                    1,
                    3,
                    move |_, _| {
                        let counted = Arc::clone(&counted);
                        async move {
                            counted.fetch_add(1, Ordering::SeqCst);
                            Err("still broken".to_owned())
                        }
                    },
                )
                .await;
            })
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if state.status.lock().unwrap().budget_exhausted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (exhausted, running, detail) = {
            let status = state.status.lock().unwrap();
            (
                status.budget_exhausted,
                status.running,
                status.detail.clone().unwrap_or_default(),
            )
        };
        assert!(exhausted, "supervisor must give up");
        assert!(!running);
        assert!(
            detail.contains("restart budget exhausted after 3 attempts"),
            "{detail}"
        );
        // Initial failure + two relaunches, and nothing behind them.
        assert_eq!(launches.load(Ordering::SeqCst), 2);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(launches.load(Ordering::SeqCst), 2);
        worker.abort();
    }

    #[tokio::test]
    async fn ready_workers_that_crash_still_exhaust_the_restart_budget() {
        use std::sync::atomic::AtomicU32;

        let state = shared_state(
            "127.0.0.1:1".parse().unwrap(),
            None,
            WorkerStatus::running(),
        );
        let launches = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&launches);
        let crash = || {
            Command::new("sh")
                .args(["-c", "exit 1"])
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| error.to_string())
        };
        let result = tokio::time::timeout(
            Duration::from_secs(6),
            supervise(
                Path::new("/fake/coorworker-cua"),
                "127.0.0.1:1".parse().unwrap(),
                Arc::clone(&state),
                Some(crash().unwrap()),
                0,
                3,
                move |_, _| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    // Ok models launch() having passed readiness. The process
                    // then exits before STABLE_AFTER on every relaunch.
                    std::future::ready(crash())
                },
            ),
        )
        .await;
        assert!(result.is_ok(), "ready crash loop must exhaust its budget");
        assert_eq!(launches.load(Ordering::SeqCst), 2);
        let status = state.status.lock().unwrap();
        assert!(!status.running);
        assert!(status.budget_exhausted);
        assert_eq!(status.restarts, 3);
        assert!(status
            .detail
            .as_deref()
            .unwrap()
            .contains("after 3 attempts"));
    }

    #[tokio::test]
    async fn the_proxy_counts_tool_calls_and_latency_only_for_calls() {
        let (url, _seen) = fake_mcp_worker().await;
        let router = Extensions::new(vec![Arc::new(proxied(url, b"proxy-secret", None).await)])
            .unwrap()
            .mount(Router::new());

        let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"launch_app","arguments":{}}}"#;
        let response = router
            .clone()
            .oneshot(mcp_request(Some("proxy-secret"), call))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // Discovery traffic is not a tool call.
        let discovery = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let response = router
            .clone()
            .oneshot(mcp_request(Some("proxy-secret"), discovery))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // A rejected token never reaches the worker and never counts.
        let response = router
            .clone()
            .oneshot(mcp_request(Some("wrong"), call))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = router
            .oneshot(Request::get("/cua/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains(
                "coorworker_envd_cua_tool_calls_total{tool=\"launch_app\",status=\"ok\"} 1"
            ),
            "{body}"
        );
        assert!(
            body.contains("coorworker_envd_cua_tool_latency_seconds_sum 0."),
            "{body}"
        );
        assert!(!body.contains("tools/list"), "{body}");
        assert!(!body.contains("tool=\"other\""), "{body}");
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
