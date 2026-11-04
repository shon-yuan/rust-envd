//! The Windows daemon: the shared HTTP surface of `docs/windows-port.md`
//! served over the Windows backend.
//!
//! Scope of this slice: `/health`, `/envs`, `/init`, `GET /files`,
//! `POST /files`, `POST /files/compose`, `/metrics`,
//! `/freeze`/`/unfreeze`, `/collapse`, `/fsfreeze`/`/fsthaw`, the nine
//! Filesystem Connect procedures (Stat, MakeDir, Move, ListDir, Remove,
//! CreateWatcher, GetWatcherEvents, RemoveWatcher and WatchDir), and the eight
//! Process Connect procedures (List, Start, Connect, StreamInput, SendInput,
//! CloseStdin, Update and SendSignal), behind the same connection-level
//! checks, CORS, authorization and mux frontdoor the Linux daemon uses. The
//! endpoint and adapter code is shared (`crate::http`, `crate::files_http`,
//! `crate::files_upload`, `crate::files_compose`, `crate::filesystem_rpc`,
//! `crate::rpc`, `crate::freeze_wire`), so this module only owns the state
//! that has no Linux counterpart and the Windows `/init` effects.
//! Unknown Connect procedures reach the shared Connect router and answer its
//! 404 `unimplemented`.
//!
//! Declared deviations from the Linux daemon, all recorded in
//! `docs/windows-port.md`:
//! - the MMDS poll is replaced by DPAPI-provisioned token/ids; `/init`
//!   enforces the provisioned token's SHA-512 hash exactly like the Linux
//!   MMDS seam, and a missing provisioning file keeps the first `/init`
//!   permissive;
//! - no cgroup/CA/NFS effects: accepted and reported as no-ops; the resource
//!   effects are real Windows operations (thread suspension, working-set
//!   trimming, volume flush);
//! - `X-Envd-Memory` reports `partial: true`, since no cgroup v2 exists;
//! - the builtin default user is the account the service runs as, because
//!   Windows has no `root`;
//! - uploads create files with the service account's ACL instead of chowning
//!   them, and `user.e2b.*` metadata is not persisted (logged, as on a Linux
//!   filesystem without xattr support);
//! - filesystem values, separators, Move/Remove details and watcher batching
//!   follow the §5.2 list; Windows uses `Logger::none()`, so the adapter emits
//!   no log lines;
//! - processes run without a `/bin/sh` wrapper, a missing executable is a 400
//!   rather than exit 127, SIGTERM is best-effort Ctrl-Break/ETX/WM_CLOSE plus
//!   a grace kill, PTY bytes are ConPTY-encoded, signal deaths are synthesized
//!   `EndEvent`s, a different-user start needs credentials that are not
//!   plumbed, the per-process Job reaps descendants, and there is no
//!   live-upgrade adoption; see §4.2;
//! - `fsfreeze` is a volume flush rather than a FIFREEZE barrier (no VSS),
//!   `collapse` cannot migrate base pages into huge pages, CPU hotplug metrics
//!   stay zero, and Windows error text replaces Go errno text; see
//!   `docs/windows-port.md` for the full list.
use axum::{
    body::{to_bytes, Body},
    extract::{RawQuery, State},
    http::{header, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::value::RawValue;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use zeroize::Zeroizing;

use crate::collapse::Collapser;
use crate::configuration::Configuration;
use crate::freeze_wire::{freeze_params, FreezeMode, FreezeOptions, FreezeParams};
use crate::fsfreeze::FsFreezer;
use crate::http::{json_error, plain_http_error, ConfigurationState};
use crate::metrics::CpuStatus;
use crate::platform::windows::collapse::WindowsCollapser;
use crate::platform::windows::fsfreeze::WindowsFsFreezer;
use crate::platform::windows::metrics::WindowsCollector;
use crate::platform::windows::resource::{FreezeLease, WindowsResourceBackend};

pub struct Daemon {
    state: Arc<Mutex<Configuration>>,
    init_lock: Arc<Mutex<()>>,
    watchers: crate::platform::WatcherRegistry,
    filesystem_logger: crate::logs::Logger,
    /// Interceptor logger for the shared process Connect adapter. `none()`
    /// keeps Windows silent, exactly like `filesystem_logger`.
    process_logger: crate::logs::Logger,
    /// Control-plane token and ids (DPAPI provisioning on production).
    metadata: Arc<dyn crate::platform::MetadataBackend>,
    /// Local listening-port source for the future loopback forwarder.
    ports: Arc<dyn crate::platform::PortScanBackend>,
    /// Workload registry, spawn gate and freeze/thaw backend. The same
    /// registry and Job are shared with the process runtime built by
    /// [`app`], so a `/freeze` sweep sees the processes the Connect surface
    /// started.
    resource: Arc<WindowsResourceBackend>,
    /// The workload registry the process runtime registers into. It is the
    /// resource backend's own registry (`resource.registry()`), kept so the
    /// deployment can state the sharing explicitly.
    workload: Arc<crate::platform::windows::registry::WorkloadRegistry>,
    /// The sandbox Job Object shared with the process runtime and the
    /// resource backend. `None` when creation failed; process procedures then
    /// keep the router's 404 fallback instead of panicking.
    sandbox_job: Option<Arc<crate::platform::windows::job::JobHandle>>,
    /// The shared metric shape collected from Win32 counters. The `Result` is
    /// the collector's own construction outcome; a failed collector is
    /// rebuilt on the next request, exactly like the Linux handler.
    metrics: Arc<std::sync::Mutex<io::Result<WindowsCollector>>>,
    /// Volume-flush freezer behind `/fsfreeze`, `/fsthaw` and `/init`'s thaw.
    fsfreeze: Arc<dyn FsFreezer>,
    /// The volume root the freezer flushes, used in the Go-shaped error text.
    fsfreeze_mountpoint: String,
    /// One permit: freeze and thaw never overlap.
    fsfreeze_lock: Arc<Semaphore>,
    /// The daemon's own heap compactor behind `/collapse`.
    collapser: Arc<dyn Collapser>,
    /// Oracle store.go:106-108: the live-upgrade handover outcome recorded
    /// once at startup (before serving) by a handover-booted envd; PostInit
    /// advertises it on every /init response (init.go:211-218).
    handover: std::sync::Mutex<Option<crate::upgrade::HandoverOutcome>>,
    /// Go main.go:369-420: the outgoing upgrade choreography. The endpoint
    /// refuses upgrades before /init and on a foreign target; only the swap
    /// itself needs the wired services, so the entry point installs this
    /// after the daemon is shared.
    upgrade_run: std::sync::Mutex<Option<crate::upgrade::UpgradeRun>>,
    /// Go store.go:59: the clock reading the `/init` handler-delay correction
    /// measures against. Injectable so tests never read the host clock.
    now: crate::clock::NowFn,
    /// Go store.go:60 and :165: the `/init` clock-step sink. `None` in tests
    /// and in a development daemon, which never touches the host clock.
    set_system_time: Option<crate::clock::SetSystemTimeFn>,
    /// Go init.go:336 `lastSetTimeNs`: the newest accepted `/init` timestamp.
    /// A stale (replayed) one is a no-op that still answers 204.
    last_set_time: std::sync::atomic::AtomicI64,
}

impl ConfigurationState for Daemon {
    fn configuration(&self) -> &Arc<Mutex<Configuration>> {
        &self.state
    }
}

impl crate::upgrade_http::UpgradeState for Daemon {
    fn upgrade_run(&self) -> Option<crate::upgrade::UpgradeRun> {
        self.upgrade_run
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn upgrade_bin_path(&self) -> std::path::PathBuf {
        crate::upgrade::upgrade_bin_path()
    }
}

impl crate::process_api::ProcessRpcState for Daemon {
    fn configuration(&self) -> &Arc<Mutex<Configuration>> {
        &self.state
    }

    fn process_logger(&self) -> &crate::logs::Logger {
        &self.process_logger
    }
}

impl crate::filesystem_rpc::FilesystemServiceState for Daemon {
    type Runtime = crate::platform::FilesystemRuntime;
    type Watchers = crate::platform::WatcherRegistry;

    fn new_runtime(
        &self,
        account: crate::exec_context::Account,
        defaults: crate::exec_context::Defaults,
    ) -> Self::Runtime {
        crate::platform::FilesystemRuntime::new(account, defaults)
    }

    fn watcher_registry(&self) -> &Self::Watchers {
        &self.watchers
    }

    fn filesystem_logger(&self) -> &crate::logs::Logger {
        &self.filesystem_logger
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

impl Daemon {
    pub fn new() -> Self {
        Self::with_metadata(Arc::new(
            crate::platform::windows::metadata::WindowsMetadataBackend::new(),
        ))
    }

    /// Explicit metadata injection for tests; production uses
    /// [`Daemon::new`] and the DPAPI provisioning file.
    pub fn with_metadata(metadata: Arc<dyn crate::platform::MetadataBackend>) -> Self {
        let mut configuration = Configuration::new();
        // The Linux oracle's builtin default is `root`; the Windows account
        // database has no such name, so the service account is the fallback.
        configuration.defaults.user = crate::platform::windows::account::current_user_name();
        // A provisioning read failure must not fail service startup: with no
        // token the first /init is permissive exactly like Linux without MMDS.
        // The token itself is never logged here.
        if let Ok(Some(ids)) = metadata.metadata_ids() {
            if !ids.instance_id.is_empty() {
                configuration
                    .environment
                    .store_internal("E2B_SANDBOX_ID", &ids.instance_id);
            }
            if !ids.env_id.is_empty() {
                configuration
                    .environment
                    .store_internal("E2B_TEMPLATE_ID", &ids.env_id);
            }
            if !ids.address.is_empty() {
                configuration
                    .environment
                    .store_internal("E2B_EVENTS_ADDRESS", &format!("http://{}", ids.address));
            }
        }
        let freezer = WindowsFsFreezer::system_volume();
        let fsfreeze_mountpoint = freezer.root().to_owned();
        // The process runtime and the resource backend share one workload
        // registry and one sandbox Job Object. A failed Job creation keeps
        // the daemon usable: `/freeze` falls back to the registry-only
        // legacy mode and `app()` leaves the process procedures on the 404.
        let workload = Arc::new(crate::platform::windows::registry::WorkloadRegistry::new());
        let sandbox_job = match crate::platform::windows::job::JobHandle::create() {
            Ok(job) => Some(Arc::new(job)),
            Err(error) => {
                eprintln!(
                    "cannot create the sandbox Job Object: {error}; process procedures will \
                     answer the 404 unimplemented fallback"
                );
                None
            }
        };
        let mut resource = WindowsResourceBackend::with_registry(workload.clone());
        if let Some(job) = &sandbox_job {
            resource = resource.with_job(job.clone());
        }
        Self {
            state: Arc::new(Mutex::new(configuration)),
            init_lock: Arc::new(Mutex::new(())),
            watchers: crate::platform::WatcherRegistry::new(),
            filesystem_logger: crate::logs::Logger::none(),
            process_logger: crate::logs::Logger::none(),
            metadata,
            ports: Arc::new(crate::platform::windows::net::WindowsPortScanBackend),
            resource: Arc::new(resource),
            workload,
            sandbox_job,
            metrics: Arc::new(std::sync::Mutex::new(WindowsCollector::new())),
            fsfreeze: Arc::new(freezer),
            fsfreeze_mountpoint,
            fsfreeze_lock: Arc::new(Semaphore::new(1)),
            collapser: Arc::new(WindowsCollapser),
            handover: std::sync::Mutex::new(None),
            upgrade_run: std::sync::Mutex::new(None),
            now: Arc::new(crate::clock::real_now_nanos),
            set_system_time: None,
            last_set_time: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// Go store.go:164-166: wires the `/init` clock-step sink. The entry
    /// point installs it only for a guest daemon; a development daemon reads
    /// the clock but never writes it.
    pub fn with_set_system_time(mut self, sink: crate::clock::SetSystemTimeFn) -> Self {
        self.set_system_time = Some(sink);
        self
    }

    /// Test seam: the correction measurement's clock. Tests advance it at the
    /// points of the handler the correction should span.
    #[doc(hidden)]
    pub fn with_now(mut self, now: crate::clock::NowFn) -> Self {
        self.now = now;
        self
    }

    /// Replaces the resource backend. Tests inject a backend whose registry
    /// they hold. The process runtime built by [`app`] uses that backend's
    /// registry (`resource.registry()`), so the injected registry stays the
    /// one `/freeze` and the Connect procedures share.
    pub fn with_resource(mut self, resource: Arc<WindowsResourceBackend>) -> Self {
        self.workload = resource.registry().clone();
        self.resource = resource;
        self
    }

    /// Replaces the interceptor logger for the process Connect adapter.
    pub fn with_process_logger(mut self, logger: crate::logs::Logger) -> Self {
        self.process_logger = logger;
        self
    }

    /// Replaces the metrics collector. Tests inject a failed construction to
    /// assert the empty 500 and the recovery path.
    pub fn with_metrics(mut self, metrics: io::Result<WindowsCollector>) -> Self {
        self.metrics = Arc::new(std::sync::Mutex::new(metrics));
        self
    }

    /// Replaces the volume-flush freezer. Tests script the wire shape without
    /// requiring a writable volume.
    pub fn with_fsfreeze(mut self, fsfreeze: Arc<dyn FsFreezer>) -> Self {
        self.fsfreeze = fsfreeze;
        self
    }

    /// Replaces the heap compactor.
    pub fn with_collapser(mut self, collapser: Arc<dyn Collapser>) -> Self {
        self.collapser = collapser;
        self
    }

    /// Oracle store.go:135-149 SetHandoverResult: record the live-upgrade
    /// handover outcome so PostInit can advertise it. Called once at startup
    /// (before serving) when this envd booted via `--resume-handover`.
    pub fn set_handover_result(&self, outcome: crate::upgrade::HandoverOutcome) {
        *self
            .handover
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(outcome);
    }

    /// The recorded handover outcome, if this envd booted from one
    /// (store.go:90/:113-125).
    pub fn handover_outcome(&self) -> Option<crate::upgrade::HandoverOutcome> {
        *self
            .handover
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Go store.go:112-113 Initialized.
    pub async fn initialized(&self) -> bool {
        self.state.lock().await.access.initialized()
    }

    /// The access state a handover boot starts from (auth.go:59-77): the
    /// secure token cleared and the pre-init gate engaged, so every path
    /// except GET /health and POST /init answers 401 until the first
    /// authenticated /init.
    pub async fn with_handover_auth(self) -> Self {
        self.state.lock().await.access = crate::auth::Access::handover();
        self
    }

    /// The exec context a handover boot starts serving with, before any
    /// request can observe it.
    pub async fn with_initial_defaults(self, defaults: crate::exec_context::Defaults) -> Self {
        self.state.lock().await.defaults = defaults;
        self
    }

    /// The live exec context the outgoing handover exports.
    pub async fn current_defaults(&self) -> crate::exec_context::Defaults {
        self.state.lock().await.defaults.clone()
    }

    /// Wire the outgoing upgrade choreography on an already-shared daemon.
    pub fn set_upgrade_run(&self, run: crate::upgrade::UpgradeRun) {
        *self
            .upgrade_run
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(run);
    }

    /// Builder form of [`Daemon::set_upgrade_run`].
    pub fn with_upgrade_run(self, run: crate::upgrade::UpgradeRun) -> Self {
        *self
            .upgrade_run
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(run);
        self
    }

    /// The resource backend whose workload registry and Job Object the
    /// process runtime shares.
    pub fn resource(&self) -> &Arc<WindowsResourceBackend> {
        &self.resource
    }

    /// The port-scan backend the loopback forwarder will consume.
    pub fn port_scan(&self) -> &Arc<dyn crate::platform::PortScanBackend> {
        &self.ports
    }

    /// Builds the process runtime that [`app_with_execution`] mounts.
    ///
    /// The runtime shares the resource backend's workload registry and the
    /// daemon's sandbox Job Object. `Err` is a construction failure (no Job,
    /// unresolvable service account, or a locked configuration); the caller
    /// keeps the 404 fallback and logs the reason instead of panicking.
    pub fn process_runtime(
        &self,
    ) -> Result<Arc<crate::platform::windows::process::WindowsProcessRuntime>, String> {
        let sandbox_job = self.sandbox_job.clone().ok_or_else(|| {
            "the sandbox Job Object is unavailable, so no process can be supervised".to_owned()
        })?;
        let account = crate::platform::windows::account::lookup(
            &crate::platform::windows::account::current_user_name(),
        )
        .map_err(|error| format!("cannot resolve the service account: {error}"))?;
        // `app()` runs before the server accepts requests, so this lock is
        // uncontended in production and in the contract tests. A poisoned or
        // contended lock is a construction failure, not a panic.
        let (defaults, environment) = match self.state.try_lock() {
            Ok(configuration) => (
                configuration.defaults.clone(),
                configuration.environment.clone(),
            ),
            Err(_) => {
                return Err(
                    "the configuration mutex is locked while building the process runtime"
                        .to_owned(),
                )
            }
        };
        let runtime = crate::platform::windows::process::WindowsProcessRuntime::new(
            account,
            defaults,
            environment,
            std::env::var("PATH").ok(),
            self.workload.clone(),
            sandbox_job,
        )
        // handler.go:599-605/:697-706 lifecycle lines, the same adapter the
        // Linux daemon wires in main_linux. A `Logger::none()` here is a
        // no-op, so an unconfigured daemon stays silent.
        .with_process_logs(Arc::new(crate::logs::ProcessLogger::new(
            self.process_logger.clone(),
        )));
        Ok(Arc::new(runtime))
    }
}

/// The production surface and the contract tests share one path: every known
/// Connect procedure is served by the shared process and filesystem routers,
/// and an unknown procedure gets the Connect router's 404 `unimplemented`.
pub fn app(state: Arc<Daemon>) -> Router {
    match state.process_runtime() {
        Ok(runtime) => app_with_execution(state, runtime),
        Err(error) => {
            eprintln!(
                "process procedures stay unavailable: {error}; keeping the router's 404 \
                 unimplemented fallback"
            );
            app_with_filesystem(state)
        }
    }
}

pub fn app_with_filesystem(state: Arc<Daemon>) -> Router {
    let rpc = crate::filesystem_rpc::router(state.clone());
    crate::http::layered(
        http_routes(state.clone()).fallback_service(crate::transport::rpc_service(rpc)),
        state,
    )
}

/// Mounts the shared process Connect adapter in front of the filesystem
/// router. Both routers are merged into the one `transport::rpc_service`
/// fallback, so auth, the frontdoor, framing and status codes are the Linux
/// code path; only the runtime object behind [`ProcessRuntimeApi`] is
/// Windows-specific.
pub fn app_with_execution(
    state: Arc<Daemon>,
    runtime: Arc<dyn crate::process_api::ProcessRuntimeApi>,
) -> Router {
    let rpc = crate::rpc::process::router(state.clone(), runtime)
        .merge(crate::filesystem_rpc::router(state.clone()));
    crate::http::layered(
        http_routes(state.clone()).fallback_service(crate::transport::rpc_service(rpc)),
        state,
    )
}

fn http_routes(state: Arc<Daemon>) -> Router {
    Router::new()
        .route("/health", any(crate::http::health))
        .route("/envs", any(crate::http::envs::<Daemon>))
        .route("/init", any(init))
        .route("/files", any(files))
        .route(
            "/files/compose",
            any(crate::files_compose::compose::<Daemon>),
        )
        .route("/metrics", any(metrics))
        // Registered with `any` so axum never appends an `Allow` header; the
        // frontdoor owns the 405 matrix and auth runs before both.
        .route("/freeze", any(freeze))
        .route("/unfreeze", any(unfreeze))
        .route("/collapse", any(collapse))
        .route("/fsfreeze", any(fsfreeze))
        .route("/fsthaw", any(fsthaw))
        .route("/upgrade", any(crate::upgrade_http::upgrade::<Daemon>))
        .with_state(state)
}

/// chi registers GET and POST on `/files`; the gate has already rejected every
/// other method. The read half is `crate::files_http`, the write half
/// `crate::files_upload`, so both platforms run the same handlers.
async fn files(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    match *request.method() {
        axum::http::Method::GET => crate::files_http::get_files(State(daemon), request).await,
        axum::http::Method::POST => crate::files_upload::upload(State(daemon), request).await,
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// The `/init` fields with Windows effects, plus the Go-typed fields this
/// daemon validates but does not use. Decoding follows the oracle closely
/// enough that a body Go rejects is rejected here too; the Linux-only effects
/// are listed in the module docs.
#[derive(Default)]
struct InitRequest {
    access_token: Option<Zeroizing<String>>,
    env_vars: Option<BTreeMap<String, String>>,
    default_user: Option<String>,
    default_workdir: Option<String>,
    /// Go `PostInitJSONBodyThaw`: only `skip` withholds the resume thaw.
    thaw: Option<String>,
    /// Go api.gen.go:386 `*time.Time json:"timestamp,omitempty"`: the client's
    /// observation of the guest clock, in nanoseconds since the epoch. Feeds
    /// the correction header, the newest-wins gate and the clock step
    /// (init.go:316-336, 433-443).
    timestamp: Option<i128>,
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

/// The members whose Go types implement `json.Unmarshaler`, captured from the
/// RAW body text. Go hands `time.Time` and `SecureToken` the still-escaped
/// bytes of their value, so `"2026-10-31T01:23:45.5Z"` is a decode error
/// (probed against the pinned oracle); a `serde_json::Value` walk would see
/// `01:23:45.5` and accept it.
#[derive(Default)]
struct RawInitMembers {
    timestamp: Option<String>,
    access_token: Option<String>,
}

impl<'de> serde::Deserialize<'de> for RawInitMembers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RawVisitor;
        impl<'de> serde::de::Visitor<'de> for RawVisitor {
            type Value = RawInitMembers;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an init object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut raw = RawInitMembers::default();
                while let Some(key) = map.next_key::<String>()? {
                    match folded(&key).as_str() {
                        "TIMESTAMP" => {
                            raw.timestamp = Some(map.next_value::<&RawValue>()?.get().to_owned());
                        }
                        "ACCESSTOKEN" => {
                            raw.access_token =
                                Some(map.next_value::<&RawValue>()?.get().to_owned());
                        }
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(raw)
            }
        }
        deserializer.deserialize_map(RawVisitor)
    }
}

/// Go `SecureToken.UnmarshalJSON`: a JSON string with no escape sequences and
/// no empty token; null is the nil pointer.
fn parse_token(raw: Option<&str>) -> Result<Option<Zeroizing<String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw == "null" {
        return Ok(None);
    }
    let token = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| "invalid secure token JSON string".to_owned())?;
    if token.is_empty() {
        return Err("empty token not allowed".to_owned());
    }
    if token.contains('\\') {
        return Err("invalid secure token: unexpected escape sequence".to_owned());
    }
    Ok(Some(Zeroizing::new(token.to_owned())))
}

/// Go `time.Time.UnmarshalJSON`: a quoted RFC3339 string, parsed from its raw
/// (still-escaped) bytes; null is the nil pointer.
fn decode_timestamp(raw: Option<&str>) -> Result<Option<i128>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw == "null" {
        return Ok(None);
    }
    if raw.len() < 2 || !raw.starts_with('"') || !raw.ends_with('"') {
        return Err("invalid timestamp JSON string".to_owned());
    }
    crate::clock::rfc3339_nanos(&raw[1..raw.len() - 1])
        .map(Some)
        .ok_or_else(|| "invalid RFC3339 timestamp".to_owned())
}

/// Go api.gen.go:245-252 `VolumeMount`: two non-omitempty strings a null
/// decodes to the zero value; a wrong JSON type fails the whole-body decode.
/// The mount is not used on Windows, but Go decodes the whole body before the
/// handler runs, so a body it rejects must be a 400 here too.
fn check_volume_mounts(value: &Value) -> Result<(), String> {
    let Value::Array(items) = value else {
        return match value {
            Value::Null => Ok(()),
            _ => Err("volumeMounts must be an array".to_owned()),
        };
    };
    for item in items {
        let Value::Object(fields) = item else {
            if item.is_null() {
                continue;
            }
            return Err("volumeMounts elements must be objects".to_owned());
        };
        for (key, value) in fields {
            if matches!(folded(key).as_str(), "NFS_TARGET" | "PATH")
                && !matches!(value, Value::Null | Value::String(_))
            {
                return Err("volumeMounts field must be a string".to_owned());
            }
        }
    }
    Ok(())
}

/// Go `*string` fields: absent, null and string decode; anything else fails.
fn check_optional_string(value: &Value) -> Result<(), String> {
    match value {
        Value::Null | Value::String(_) => Ok(()),
        _ => Err("field must be a string".to_owned()),
    }
}

fn decode_init(body: &[u8]) -> Result<InitRequest, String> {
    if body.is_empty() {
        return Ok(InitRequest::default());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| "invalid JSON".to_owned())?;
    let Value::Object(map) = value else {
        // Go's decoder maps a literal null body to the zero struct.
        return match value {
            Value::Null => Ok(InitRequest::default()),
            _ => Err("init body must be an object or null".to_owned()),
        };
    };
    let raw: RawInitMembers =
        serde_json::from_slice(body).map_err(|_| "invalid JSON".to_owned())?;
    let mut init = InitRequest::default();
    for (key, value) in map {
        match folded(&key).as_str() {
            // Decoded from the raw body above; see RawInitMembers.
            "ACCESSTOKEN" | "TIMESTAMP" => {}
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
            // Go api.gen.go decodes any string; only "skip" withholds the
            // thaw, while inline/unknown values thaw (init.go:695-697).
            "THAW" => match value {
                Value::Null => init.thaw = None,
                Value::String(thaw) => init.thaw = Some(thaw),
                _ => return Err("thaw must be a string".to_owned()),
            },
            // Go-typed fields with Linux-only effects: validated the way Go
            // validates them, then ignored.
            "CABUNDLE" | "HYPERLOOPIP" | "LIFECYCLEID" => check_optional_string(&value)?,
            "CPUCOUNT" => match value {
                Value::Null => {}
                Value::Number(number) if number.as_i64().is_some() => {}
                _ => return Err("cpuCount must be an integer".to_owned()),
            },
            "VOLUMEMOUNTS" => check_volume_mounts(&value)?,
            _ => {}
        }
    }
    init.access_token = parse_token(raw.access_token.as_deref())?;
    init.timestamp = decode_timestamp(raw.timestamp.as_deref())?;
    Ok(init)
}

async fn init(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    let mut response = apply_init(daemon.clone(), request).await;
    // Go store.go:119-133 and init.go:211-218: the handover outcome rides
    // EVERY /init response (success and error), before the version and memory
    // headers, exactly as the Linux daemon emits it.
    if let Some(outcome) = daemon.handover_outcome() {
        if let Ok(value) = serde_json::to_string(&outcome).and_then(|value| {
            header::HeaderValue::from_str(&value).map_err(serde::ser::Error::custom)
        }) {
            response.headers_mut().insert("x-envd-handover", value);
        }
    }
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
    // Read on entry, before the body and the /init lock, and compared again by
    // the thaw: a freeze that lands after this point voids it, exactly like
    // the Linux generation guard. Taken under the freeze lock so an accepted
    // freeze that has not bumped the counter yet is waited out.
    let freeze_generation = daemon.resource.generation().await;
    let body = match to_bytes(request.into_body(), 32 << 20).await {
        Ok(body) => body,
        Err(_) => return (StatusCode::BAD_REQUEST, "failed to read request body").into_response(),
    };
    // Go init.go:250: the correction below measures this handler's own delay
    // from the end of the body read, so the reading is taken now, before the
    // body is decoded.
    let body_read = (daemon.now)();
    let payload = match decode_init(&body) {
        Ok(payload) => payload,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let skip_thaw = payload.thaw.as_deref() == Some("skip");
    let token = payload.access_token;
    let _guard = daemon.init_lock.lock().await;
    let request_token = token.as_deref().map(|value| value.as_str());
    // Mirror the Linux MMDS seam: a token that already matches the installed
    // one needs no lookup; otherwise the provisioned token's SHA-512 hash is
    // the out-of-band authority. The DPAPI read and hash run on a blocking
    // worker; a provisioning error degrades to "no expected hash".
    let matches_current = daemon
        .state
        .lock()
        .await
        .access
        .matches_current_token(request_token);
    let expected_hash = if matches_current {
        None
    } else {
        let metadata = daemon.metadata.clone();
        tokio::task::spawn_blocking(move || metadata.init_token_sha512())
            .await
            .ok()
            .and_then(|result| result.ok())
            .flatten()
    };
    let mut state = daemon.state.lock().await;
    if let Err(error) = state
        .access
        .validate_init(request_token, expected_hash.as_deref())
    {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    // Go init.go:305-313: the thaw is registered HERE — after the token
    // check, before the rest of SetData — as a defer: every return past this
    // point thaws. Spawning the task now makes that hold for dropped futures
    // too; finalize below only awaits the handle, so the success path still
    // answers after the thaw. A freeze arriving mid-SetData wins through the
    // generation guard.
    let thaw_task = if skip_thaw {
        None
    } else {
        let daemon = daemon.clone();
        Some(tokio::spawn(async move {
            thaw_workload(&daemon, freeze_generation).await;
        }))
    };
    // Go init.go:316-326: everything since the body read — the lock, the
    // token check, the DPAPI lookup — is time the guest clock would otherwise
    // lose. The corrected timestamp replaces the request's own, the whole
    // milliseconds (truncated toward zero) are answered on
    // `X-Envd-Init-Clock-Correction`, and both the newest-wins compare below
    // and the step's bands apply to the corrected value.
    let mut correction_header: Option<String> = None;
    let timestamp = payload.timestamp.map(|timestamp| {
        let correction = (daemon.now)() - body_read;
        let millis = correction.clamp(i64::MIN as i128, i64::MAX as i128) / 1_000_000;
        correction_header = Some((millis as i64).to_string());
        timestamp + correction
    });
    // Go init.go:336: update only if the corrected timestamp is newer than (or
    // equal to, utils/atomic.go SetToGreater) the last accepted one; a
    // stale/replayed /init skips SetData entirely but still answers 204 with
    // the in-effect reports.
    let gated = match timestamp {
        None => true,
        Some(corrected) => {
            let nanos = crate::clock::unix_nanos_wrapping(corrected);
            let previous = daemon.last_set_time.fetch_max(nanos, Ordering::Relaxed);
            previous <= nanos
        }
    };
    if gated {
        // Go init.go:433-443, first in SetData: band-check the real guest
        // clock against the corrected timestamp and step to it. A failed step
        // is logged and never fails the /init.
        if let Some(corrected) = timestamp {
            if crate::clock::should_set_system_time(crate::clock::real_now_nanos(), corrected) {
                if let Some(sink) = &daemon.set_system_time {
                    if let Err(error) = sink(crate::clock::unix_nanos_wrapping(corrected)) {
                        eprintln!("Failed to set system time: {error}");
                    }
                }
            }
        }
        if let Some(envs) = payload.env_vars {
            state.environment.replace_user(envs);
        }
        if let Err(error) = state.access.set_token(token.map(|value| value.to_string())) {
            drop(state);
            if let Some(task) = thaw_task {
                let _ = task.await;
            }
            return with_init_headers(
                json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
                skip_thaw,
                correction_header.as_deref(),
            );
        }
        if let Some(user) = payload.default_user.filter(|value| !value.is_empty()) {
            state.defaults.user = user;
            state.defaults.user_delivered = true;
        }
        if let Some(workdir) = payload.default_workdir.filter(|value| !value.is_empty()) {
            state.defaults.workdir = Some(workdir);
        }
        state.access.mark_initialized();
    }
    drop(state);
    // The success path answers only after the thaw (Go init.go:361).
    if let Some(task) = thaw_task {
        if let Err(error) = task.await {
            eprintln!("init thaw task failed: {error}");
        }
    }
    with_init_headers(
        StatusCode::NO_CONTENT.into_response(),
        skip_thaw,
        correction_header.as_deref(),
    )
}

/// The `X-Envd-Thaw: skip` acknowledgement Linux sets when the thaw was
/// withheld (init.go:306-314), plus the `X-Envd-Init-Clock-Correction`
/// milliseconds every timestamped `/init` answers (init.go:324).
fn with_init_headers(mut response: Response, skip: bool, correction: Option<&str>) -> Response {
    if skip {
        response
            .headers_mut()
            .insert("x-envd-thaw", header::HeaderValue::from_static("skip"));
    }
    if let Some(correction) = correction {
        if let Ok(value) = header::HeaderValue::from_str(correction) {
            response
                .headers_mut()
                .insert("x-envd-init-clock-correction", value);
        }
    }
    response
}

/// Go init.go:306-314: the resume thaw. The generation guard makes a freeze
/// that started after `/init` entered win, so its deliberately suspended
/// workload is not resumed underneath it. Failures are logged, never allowed
/// to fail a resume.
async fn thaw_workload(daemon: &Daemon, freeze_generation: u64) {
    match daemon.resource.thaw_if_generation(freeze_generation).await {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "a freeze came after this /init entered; not thawing the workload: \
             entry_freeze_generation={freeze_generation}"
        ),
        Err(error) => eprintln!("unfreeze workloads: {error}"),
    }
}

/// Go init.go:494-643 PostFreeze over the Windows suspend backend.
///
/// The query binding, the bare-204 vs structured-200 split, the `no-store`
/// header and the 500 join-failure envelope are the Linux wire contract; only
/// the mechanism behind `freeze_held` is different.
async fn freeze(State(daemon): State<Arc<Daemon>>, RawQuery(query): RawQuery) -> Response {
    let params = match freeze_params(query.as_deref()) {
        Ok(params) => params,
        Err(error) => return plain_http_error(StatusCode::BAD_REQUEST, &error),
    };
    // The lock acquire is the handler's one cancellable await: a caller that
    // leaves while waiting holds nothing and sweeps nothing. The sweep itself
    // runs detached once the lease is held, so its accounting completes even
    // if the connection dies.
    let lease = daemon.resource.freeze_lease().await;
    let sweep = tokio::spawn(async move { freeze_body(daemon, lease, params).await });
    match sweep.await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("freeze handler task failed: {error}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "freeze failed")
        }
    }
}

async fn freeze_body(daemon: Arc<Daemon>, lease: FreezeLease, params: FreezeParams) -> Response {
    let report = params.max_wait_ms.is_some_and(|value| value > 0);
    let options = FreezeOptions {
        max_wait: Duration::from_millis(params.max_wait_ms.unwrap_or(0).max(0) as u64),
        mode: match params.mode.as_deref() {
            Some("hierarchy") => FreezeMode::Hierarchy,
            // Absent or unknown means legacy.
            _ => FreezeMode::Legacy,
        },
        max_cgroups: params.max_cgroups.unwrap_or(0),
    };
    let (result, error) = daemon.resource.freeze_held(lease, options).await;
    if let Some(error) = &error {
        // Per-process failures are counted in the body, never raised as 500.
        eprintln!(
            "some workloads failed to freeze; reporting them in the result: failed={} error={error}",
            result.failed
        );
    }
    // The bare caller gets a bare 204: no Content-Type, no Cache-Control.
    if !report {
        return StatusCode::NO_CONTENT.into_response();
    }
    if !result.all_frozen() {
        eprintln!(
            "pre-pause freeze did not stop the whole workload; snapshot may capture it running: \
             requested={} frozen={} not_frozen={} failed={} vanished={}",
            result.requested, result.frozen, result.not_frozen, result.failed, result.vanished
        );
    }
    if result.truncated {
        eprintln!(
            "freeze walk hit its bound; some processes were never examined: visited={} requested={}",
            result.visited, result.requested
        );
    }
    let mut response = Json(result.wire()).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Go init.go:676-691 PostUnfreeze: resume exactly what the last sweep
/// suspended. Idempotent; a backend failure is the joined-error JSON 500.
async fn unfreeze(State(daemon): State<Arc<Daemon>>) -> Response {
    match daemon.resource.unfreeze().await {
        Ok(()) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(error) => {
            eprintln!("unfreeze workload: {error}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &error)
        }
    }
}

/// `GET /metrics`: the shared twelve-field JSON object from Win32 counters.
/// A collection or lock failure is the empty 500 with the same headers; the
/// failed collector is rebuilt on the next request, so a later call recovers.
async fn metrics(State(daemon): State<Arc<Daemon>>) -> Response {
    let collector = daemon.metrics.clone();
    let sample = tokio::task::spawn_blocking(move || {
        let mut state = collector
            .lock()
            .map_err(|_| io::Error::other("metrics collector lock poisoned"))?;
        if state.is_err() {
            *state = WindowsCollector::new();
        }
        match state.as_mut() {
            Ok(collector) => collector.sample(CpuStatus::default()),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    })
    .await
    .map_err(io::Error::other)
    .and_then(|sample| sample)
    .and_then(|metrics| {
        let mut bytes = serde_json::to_vec(&metrics)?;
        bytes.push(b'\n');
        Ok(bytes)
    });
    let mut response = match sample {
        Ok(bytes) => bytes.into_response(),
        Err(error) => {
            eprintln!("failed to collect metrics: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

/// Go api/collapse.go PostCollapse: compact the daemon's own heap just before
/// pause, best-effort, and report the six stats. The request body is never
/// read. A caller that leaves flips the cancellation flag the blocking sweep
/// polls, the axum shape of Go's `r.Context()`.
async fn collapse(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    let _ = request; // deferred Body close like Go's `defer r.Body.Close()`
    let started = tokio::time::Instant::now();
    struct CancelOnDrop(Arc<AtomicBool>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancelled.clone());
    let collapser = daemon.collapser.clone();
    let outcome = {
        let cancelled = cancelled.clone();
        tokio::task::spawn_blocking(move || collapser.collapse_self(&cancelled)).await
    };
    match outcome {
        Ok(Ok(stats)) => {
            let elapsed_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
            let body = serde_json::json!({
                "regions": stats.regions,
                "chunks": stats.chunks,
                "collapsed": stats.collapsed,
                "alreadyHuge": stats.already_huge,
                "skipped": stats.skipped,
                "elapsedMs": elapsed_ms,
            });
            let mut response = (StatusCode::OK, format!("{body}\n")).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            response
        }
        Ok(Err(error)) => {
            eprintln!("collapse envd heap: {error}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
        Err(error) => {
            eprintln!("collapse handler task failed: {error}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
    }
}

/// Go api/fsfreeze.go: flush the system volume so a filesystem-only pause
/// reaches a consistent on-disk state. Serialized on one lock; a repeated
/// freeze is a no-op.
async fn fsfreeze(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    // Deferred Body close like Go's `defer r.Body.Close()`: the body lives
    // until the response, keeping a keep-alive connection's framing aligned.
    let _ = request;
    fsfreeze_endpoint(daemon, FsFreezeOp::Freeze).await
}

async fn fsthaw(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    let _ = request;
    fsfreeze_endpoint(daemon, FsFreezeOp::Thaw).await
}

#[derive(Clone, Copy)]
enum FsFreezeOp {
    Freeze,
    Thaw,
}

async fn fsfreeze_endpoint(daemon: Arc<Daemon>, op: FsFreezeOp) -> Response {
    // Freeze keeps the request-bound acquire: a caller that leaves while
    // contended abandons the wait. Thaw runs in a detached task that owns its
    // acquire, so a rollback thaw lands even when the client disconnects.
    let outcome = match op {
        FsFreezeOp::Freeze => match daemon.fsfreeze_lock.clone().acquire_owned().await {
            Ok(permit) => run_fsfreeze_ioctl(&daemon, op, permit).await,
            Err(error) => Err(io::Error::other(error.to_string())),
        },
        FsFreezeOp::Thaw => {
            let owner = daemon.clone();
            tokio::spawn(async move {
                match owner.fsfreeze_lock.clone().acquire_owned().await {
                    Ok(permit) => run_fsfreeze_ioctl(&owner, FsFreezeOp::Thaw, permit).await,
                    Err(error) => Err(io::Error::other(error.to_string())),
                }
            })
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error.to_string())))
        }
    };
    match outcome {
        Ok(()) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(error) => {
            eprintln!(
                "{} system volume: {error}",
                match op {
                    FsFreezeOp::Freeze => "freeze",
                    FsFreezeOp::Thaw => "thaw",
                }
            );
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
    }
}

/// Runs the volume flush on the blocking pool while holding the serialized
/// fsfreeze permit.
async fn run_fsfreeze_ioctl(
    daemon: &Daemon,
    op: FsFreezeOp,
    permit: OwnedSemaphorePermit,
) -> io::Result<()> {
    let freezer = daemon.fsfreeze.clone();
    let mountpoint = daemon.fsfreeze_mountpoint.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match op {
            FsFreezeOp::Freeze => freezer.freeze(&mountpoint),
            FsFreezeOp::Thaw => freezer.thaw(&mountpoint),
        }
    })
    .await
    .unwrap_or_else(|error| Err(io::Error::other(error.to_string())))
}

#[cfg(test)]
#[path = "daemon_windows/contract_tests.rs"]
mod contract_tests;
