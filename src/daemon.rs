//! HTTP surface and `/init` effects. NFS volume mounts are implemented with
//! oracle parity through the [`nfs`] module; every SetData effect is live.
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{header, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde::Serialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::init_payload::InitRequest;
use crate::{
    configuration::Configuration,
    exec_context::Defaults,
    freeze_wire::{freeze_params, FreezeParams},
    http::{json_error, plain_http_error},
    memory_protection::{read_memory_protection, MemoryProtection},
};

/// Oracle store.go:64-68: the one freezer shared by /freeze, /unfreeze and
/// /init's thaw, so every freeze/unfreeze caller serializes on a single lock.
pub type Freezer = Arc<crate::cgroups::freeze::WorkloadFreezer>;

/// Go main.go:218-243 wiring in one place: the daemon and the process service
/// share the ONE workload freezer instance, so a /freeze holds the spawn
/// barrier for Start (the same Arc is also the port forwarder's,
/// main.go:229/:347). Guests pass the real cgroup freezer; a development
/// build passes None and the runtime then adopts the daemon's default no-op
/// freezer, keeping the same refusal semantics without host writes.
pub fn share_freezer(
    daemon: Daemon,
    runtime: crate::process::ProcessRuntime,
    freezer: &Option<Freezer>,
) -> (Daemon, crate::process::ProcessRuntime) {
    let daemon = match freezer {
        Some(freezer) => daemon.with_freezer(freezer.clone()),
        None => daemon,
    };
    let runtime = match freezer {
        Some(freezer) => runtime.with_freezer(freezer.clone()),
        None => runtime.with_freezer(daemon.freezer()),
    };
    (daemon, runtime)
}

mod metrics;
pub mod nfs;

impl crate::http::ConfigurationState for Daemon {
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
        std::path::PathBuf::from(crate::upgrade::DEFAULT_UPGRADE_BIN_PATH)
    }
}

impl crate::filesystem_rpc::FilesystemServiceState for Daemon {
    type Runtime = crate::platform::FilesystemRuntime;
    type Watchers = crate::platform::WatcherRegistry;

    fn new_runtime(
        &self,
        account: crate::exec_context::Account,
        defaults: Defaults,
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

impl crate::process_api::ProcessRpcState for Daemon {
    fn configuration(&self) -> &Arc<Mutex<Configuration>> {
        &self.state
    }

    fn process_logger(&self) -> &crate::logs::Logger {
        &self.process_logger
    }
}

pub struct Daemon {
    state: Arc<Mutex<Configuration>>,
    init_lock: Arc<Mutex<()>>,
    memory: MemoryProtection,
    /// Oracle store.go:64-68/:169. Defaults to a no-op-manager-backed freezer
    /// (development never WRITES the host cgroupfs; Daemon::new still READS
    /// /proc/self/cgroup and /sys/fs/cgroup to size X-Envd-Memory, degrading
    /// to Partial when they cannot be read); the guest main wires the real
    /// one.
    freezer: Freezer,
    metrics: Arc<std::sync::Mutex<std::io::Result<crate::metrics::Collector>>>,
    oom: Option<Arc<crate::oom::OomWatcher>>,
    cpus: Arc<crate::cpus::CpuManager>,
    token_hash_provider: Arc<dyn crate::mmds::TokenHashProvider>,
    metadata: Option<Arc<crate::mmds::poll::MetadataLifecycle>>,
    hyperloop: Option<Arc<Hyperloop>>,
    ca_cert_installer: Option<Arc<crate::cacerts::CaCertInstaller>>,
    now: crate::clock::NowFn,
    set_system_time: Option<crate::clock::SetSystemTimeFn>,
    last_set_time: std::sync::atomic::AtomicI64,
    watchers: crate::filesystem::watchers::WatcherRegistry,
    /// Go `a.logger` (env logger) and `a.logFlusher` (init.go). Absent
    /// flusher = Go's NoopLogFlusher.
    logger: crate::logs::Logger,
    /// Go main.go:215 `fsLogger` handed to filesystemRpc.Handle.
    filesystem_logger: crate::logs::Logger,
    /// Go main.go:243 `processLogger` handed to processRpc.Handle.
    process_logger: crate::logs::Logger,
    log_flusher: Option<Arc<crate::logs::HttpExporter>>,
    /// Oracle store.go:106-108: the live-upgrade handover outcome recorded
    /// once at startup (before serving) by a handover-booted envd; PostInit
    /// advertises it on every /init response (init.go:211-218).
    handover: std::sync::Mutex<Option<crate::upgrade::HandoverOutcome>>,
    /// Go main.go:369-420: the outgoing upgrade choreography, closed over
    /// main's services exactly as Go's doUpgrade closure is. The endpoint
    /// refuses upgrades before /init and on a foreign target; only the exec
    /// itself needs the wired services. Set post-construction: the closure
    /// needs the shared [`Daemon`], so main wires it after `Arc::new` the way
    /// Go registers the handler on the already-built services.
    upgrade_run: std::sync::Mutex<Option<crate::upgrade::UpgradeRun>>,
    /// Oracle api.mounts_handover.go: the NFS mount ledger (path -> lifecycle),
    /// written only after a VERIFIED mount (init.go:976) and exported into the
    /// live-upgrade handover blob so a still-live mount with an unchanged
    /// lifecycle is not remounted.
    mounts: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// Oracle store.go:72 nfsFailure, guarded by initLock upstream: the last
    /// mount failure and the lifecycle it failed under, so same-lifecycle
    /// retries (including `volumeMounts: []`) short-circuit to the same 400.
    nfs_failure: std::sync::Mutex<Option<nfs::NfsFailure>>,
    /// Oracle store.go:71 nfs (execNFSMounter): the mount executor seam tests
    /// replace. The guest keeps the exec default.
    nfs_mounter: Arc<dyn nfs::NfsMounter>,
    /// Oracle api/fsfreeze.go: the FIFREEZE/FITHAW freezer behind /fsfreeze
    /// and /fsthaw, and the rootfs mountpoint both ioctls target. The seam
    /// keeps route tests off real host mounts.
    fsfreeze: Arc<dyn crate::fsfreeze::FsFreezer>,
    fsfreeze_mountpoint: &'static str,
    /// Oracle api/fsfreeze.go fsFreezeLock: one freeze/thaw at a time.
    fsfreeze_lock: Arc<tokio::sync::Semaphore>,
}

/// Mirrors Go `SetupHyperloop`: a per-daemon lock serializes rewrites (last
/// writer wins), success stores the internal events address, failure only logs.
struct Hyperloop {
    state: Arc<Mutex<Configuration>>,
    hosts_path: std::path::PathBuf,
    lock: Arc<Mutex<()>>,
}
impl Hyperloop {
    async fn setup(self: Arc<Self>, address: String) {
        let _guard = self.lock.lock().await;
        let path = self.hosts_path.clone();
        let rewrite = {
            let address = address.clone();
            tokio::task::spawn_blocking(move || {
                crate::hyperloop::rewrite_hosts_file(&address, &path)
            })
            .await
        };
        match rewrite {
            Ok(Ok(())) => {
                let mut state = self.state.lock().await;
                state
                    .environment
                    .store_internal("E2B_EVENTS_ADDRESS", &format!("http://{address}"));
            }
            Ok(Err(error)) => eprintln!("failed to modify hosts file: {error}"),
            Err(error) => eprintln!("failed to modify hosts file: {error}"),
        }
    }
}
struct DaemonMetadataSink {
    state: Arc<Mutex<Configuration>>,
    files: crate::mmds::poll::MetadataFiles,
}
impl crate::mmds::poll::MetadataSink for DaemonMetadataSink {
    fn store_ids<'a>(&'a self, opts: &'a crate::mmds::MmdsOpts) -> crate::mmds::BoxFuture<'a, ()> {
        Box::pin(async {
            {
                let mut state = self.state.lock().await;
                state
                    .environment
                    .store_internal("E2B_SANDBOX_ID", &opts.instance_id);
                state
                    .environment
                    .store_internal("E2B_TEMPLATE_ID", &opts.env_id);
            }
            self.files.write(opts).await;
        })
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}
impl Daemon {
    /// Oracle store.go:135-149 SetHandoverResult: record the live-upgrade
    /// handover outcome so PostInit can advertise it. Called once at startup
    /// (before serving) when this envd booted via --resume-handover.
    pub fn set_handover_result(&self, outcome: crate::upgrade::HandoverOutcome) {
        *self.handover.lock().unwrap() = Some(outcome);
    }

    /// The recorded handover outcome, if this envd booted from one
    /// (store.go:90/:113-125).
    fn handover_outcome(&self) -> Option<crate::upgrade::HandoverOutcome> {
        *self.handover.lock().unwrap()
    }

    /// Go store.go:112-113 Initialized.
    pub async fn initialized(&self) -> bool {
        self.state.lock().await.access.initialized()
    }

    /// The access state a handover boot starts from (auth.go:59-77): the
    /// secure token cleared (api.New always starts empty) and the pre-init
    /// gate engaged — only GET /health and POST /init answer until the first
    /// /init marks the daemon initialized, closing the window in which a
    /// re-adopted — and possibly hostile — guest process could drive an
    /// unauthenticated /upgrade (upgrade.go:336-343).
    pub async fn with_handover_auth(self) -> Self {
        self.state.lock().await.access = crate::auth::Access::handover();
        self
    }

    /// The exec context a boot starts serving with. Go shares one
    /// *execcontext.Defaults between the api and process services
    /// (main.go:192-204), so the resume's applied defaults are visible to
    /// every consumer from the first request (upgrade.go:396-405); the Rust
    /// daemon owns its copy, so a handover boot installs the restored record
    /// before serving.
    pub async fn with_initial_defaults(self, defaults: Defaults) -> Self {
        self.state.lock().await.defaults = defaults;
        self
    }

    /// The daemon's watcher registry, for the resume's re-arm and the outgoing
    /// handover's ExportWatchersHold (main.go:247/:390).
    pub fn watcher_registry(&self) -> &crate::filesystem::watchers::WatcherRegistry {
        &self.watchers
    }

    /// The one shared freezer (/freeze, /unfreeze, /init's thaw, the handover
    /// hold and resume). A development daemon carries the no-op-manager
    /// default, which never writes a host cgroupfs.
    pub fn freezer(&self) -> Freezer {
        self.freezer.clone()
    }

    /// Oracle api.mounts_handover.go ExportMounts: snapshot the NFS mount
    /// ledger as typed MountEntry messages, carried across a live upgrade.
    pub fn export_mounts(&self) -> Vec<crate::proto::upgrade::MountEntry> {
        self.mounts
            .lock()
            .unwrap()
            .iter()
            .map(|(path, lifecycle)| crate::proto::upgrade::MountEntry {
                path: path.clone(),
                lifecycle_id: lifecycle.clone(),
                ..Default::default()
            })
            .collect()
    }

    /// Oracle api.mounts_handover.go ImportMounts: restore the ledger from a
    /// handover so the first post-upgrade /init knows which paths are already
    /// mounted (and for which lifecycle) before its setupNFS.
    pub fn import_mounts(&self, mounts: &[crate::proto::upgrade::MountEntry]) {
        let mut ledger = self.mounts.lock().unwrap();
        for mount in mounts {
            ledger.insert(mount.path.clone(), mount.lifecycle_id.clone());
        }
    }

    /// Oracle init.go:480-484 + :796-864: the /init setupNFS arm, run under
    /// initLock with the daemon's ledger, failure memo and mount executor.
    pub(super) async fn setup_nfs(
        &self,
        lifecycle_id: &str,
        mounts: &[nfs::VolumeMount],
    ) -> Result<(), nfs::NfsVolumeError> {
        let books = nfs::NfsBooks {
            mounted: &self.mounts,
            failure: &self.nfs_failure,
        };
        nfs::setup_nfs(&books, self.nfs_mounter.as_ref(), lifecycle_id, mounts).await
    }

    /// Wire the outgoing upgrade choreography (Go's doUpgrade closure,
    /// main.go:369-420). The endpoint's pre-checks run without it; only the
    /// exec needs the wired services.
    pub fn with_upgrade_run(mut self, run: crate::upgrade::UpgradeRun) -> Self {
        *self
            .upgrade_run
            .get_mut()
            .unwrap_or_else(|error| error.into_inner()) = Some(run);
        self
    }

    /// Wire the choreography on an already-shared daemon — the closure needs
    /// the `Arc<Daemon>` itself (mount ledger, watcher registry), so main
    /// installs it after construction, the way Go's m.Post registers the
    /// handler on built services.
    pub fn set_upgrade_run(&self, run: crate::upgrade::UpgradeRun) {
        *self
            .upgrade_run
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(run);
    }

    /// The live exec context (Go's shared *execcontext.Defaults,
    /// main.go:192-204): the record /init keeps current and the outgoing
    /// handover exports (upgrade.go:147-149).
    pub async fn current_defaults(&self) -> Defaults {
        self.state.lock().await.defaults.clone()
    }

    /// Explicit lifecycle injection; defaults/development never contact MMDS.
    pub async fn with_metadata_polling(
        mut self,
        client: Arc<crate::mmds::MmdsClient>,
        run_dir: std::path::PathBuf,
        is_firecracker: bool,
    ) -> Self {
        self.state
            .lock()
            .await
            .environment
            .store_internal("E2B_SANDBOX", if is_firecracker { "true" } else { "false" });
        let sink = Arc::new(DaemonMetadataSink {
            state: self.state.clone(),
            files: crate::mmds::poll::MetadataFiles::new(run_dir),
        });
        let lifecycle = Arc::new(crate::mmds::poll::MetadataLifecycle::new(client, sink));
        if is_firecracker {
            lifecycle.start_boot();
        }
        self.metadata = Some(lifecycle);
        self
    }
    pub async fn shutdown_metadata(&self) -> std::io::Result<()> {
        if let Some(metadata) = &self.metadata {
            metadata.shutdown().await?;
        }
        Ok(())
    }

    /// Go main.go:214/:296: the env logger used by the HTTP API, and
    /// main.go:199-210: the exporter `/freeze` flushes.
    pub fn with_logger(mut self, logger: crate::logs::Logger) -> Self {
        self.logger = logger;
        self
    }

    /// Go main.go:215: the `logger=filesystem` context every filesystem RPC
    /// line carries.
    pub fn with_filesystem_logger(mut self, logger: crate::logs::Logger) -> Self {
        self.filesystem_logger = logger;
        self
    }

    /// Go main.go:243: the `logger=process` context every process RPC line
    /// carries.
    pub fn with_process_logger(mut self, logger: crate::logs::Logger) -> Self {
        self.process_logger = logger;
        self
    }

    pub fn with_log_flusher(mut self, flusher: Arc<crate::logs::HttpExporter>) -> Self {
        self.log_flusher = Some(flusher);
        self
    }

    /// The retained MMDS options receiver (main.go:196-201): the exporter owns
    /// it so the sender is never drained before the log sender starts.
    pub fn take_metadata_options_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::mmds::MmdsOpts>> {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.take_options_receiver())
    }

    pub fn with_token_hash_provider(provider: Arc<dyn crate::mmds::TokenHashProvider>) -> Self {
        Self {
            token_hash_provider: provider,
            ..Self::new()
        }
    }

    pub fn with_guest_services(
        manager: Arc<crate::cpus::CpuManager>,
        watcher: Arc<crate::oom::OomWatcher>,
        provider: Arc<dyn crate::mmds::TokenHashProvider>,
    ) -> Self {
        Self {
            cpus: manager,
            oom: Some(watcher),
            token_hash_provider: provider,
            ..Self::new()
        }
    }

    pub fn with_cpu_manager(manager: Arc<crate::cpus::CpuManager>) -> Self {
        Self {
            cpus: manager,
            ..Self::new()
        }
    }

    /// The pinned oracle writes /etc/hosts; guests wire that path and tests
    /// inject a tempdir file. Without an injector hyperloopIP is ignored.
    pub fn with_hyperloop_hosts_path(mut self, hosts_path: std::path::PathBuf) -> Self {
        self.hyperloop = Some(Arc::new(Hyperloop {
            state: self.state.clone(),
            hosts_path,
            lock: Arc::new(Mutex::new(())),
        }));
        self
    }

    /// Go store.go:60-61/:166: the reading the /init correction measurement
    /// uses; tests advance it at the points of the handler the correction
    /// should span. Production reads the real system clock.
    pub fn with_now(mut self, now: crate::clock::NowFn) -> Self {
        self.now = now;
        self
    }

    /// Go store.go:58-59/:165: the system-clock step sink. Production wires
    /// the real clock_settime only on the guest path; without a sink the step
    /// is skipped (development daemons never touch the host clock).
    pub fn with_set_system_time(mut self, sink: crate::clock::SetSystemTimeFn) -> Self {
        self.set_system_time = Some(sink);
        self
    }

    /// Go store.go:63/:168: the CA bundle installer, whose reconcile loop the
    /// guest main spawns separately. Without an installer the CA arm is
    /// skipped (development daemons never touch the host trust stores).
    pub fn with_ca_certinstaller(
        mut self,
        installer: Arc<crate::cacerts::CaCertInstaller>,
    ) -> Self {
        self.ca_cert_installer = Some(installer);
        self
    }

    /// Go store.go:152/:169: the shared workload freezer. Only the guest main
    /// passes a freezer backed by the real cgroup2 manager; development
    /// daemons keep the no-op default so nothing touches the host cgroupfs.
    pub fn with_freezer(mut self, freezer: Freezer) -> Self {
        self.freezer = freezer;
        self
    }

    /// Oracle api/fsfreeze.go: swap the FIFREEZE/FITHAW backend. Route tests
    /// inject a scripted freezer; the guest keeps the Linux default.
    pub fn with_fsfreeze(mut self, freezer: Arc<dyn crate::fsfreeze::FsFreezer>) -> Self {
        self.fsfreeze = freezer;
        self
    }

    /// Oracle store.go:71 nfs: swap the mount executor. Route tests inject a
    /// scripted mounter that never execs; the guest keeps the exec default.
    pub fn with_nfs_mounter(mut self, mounter: Arc<dyn nfs::NfsMounter>) -> Self {
        self.nfs_mounter = mounter;
        self
    }

    pub fn with_oom_watcher(watcher: Arc<crate::oom::OomWatcher>) -> Self {
        Self {
            oom: Some(watcher),
            ..Self::new()
        }
    }

    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(Configuration::new())),
            init_lock: Arc::new(Mutex::new(())),
            watchers: crate::filesystem::watchers::WatcherRegistry::new(),
            freezer: Arc::new(crate::cgroups::freeze::WorkloadFreezer::new(Arc::new(
                crate::cgroups::noop::NoopManager,
            ))),
            metrics: Arc::new(std::sync::Mutex::new(crate::metrics::Collector::new())),
            oom: None,
            cpus: Arc::new(crate::cpus::CpuManager::noop()),
            token_hash_provider: Arc::new(crate::mmds::NoopProvider),
            metadata: None,
            hyperloop: None,
            ca_cert_installer: None,
            now: Arc::new(crate::clock::real_now_nanos),
            set_system_time: None,
            last_set_time: std::sync::atomic::AtomicI64::new(0),
            // Read-only probe of the host cgroupfs for X-Envd-Memory: both
            // paths are only ever READ here, never written; when they cannot
            // be read the protection degrades to Partial. The write path
            // belongs to the guest's cgroup2 manager alone.
            logger: crate::logs::Logger::none(),
            filesystem_logger: crate::logs::Logger::none(),
            process_logger: crate::logs::Logger::none(),
            log_flusher: None,
            handover: std::sync::Mutex::new(None),
            upgrade_run: std::sync::Mutex::new(None),
            mounts: std::sync::Mutex::new(std::collections::HashMap::new()),
            nfs_failure: std::sync::Mutex::new(None),
            nfs_mounter: Arc::new(nfs::ExecNfsMounter),
            fsfreeze: Arc::new(crate::fsfreeze::LinuxFsFreezer),
            fsfreeze_mountpoint: crate::fsfreeze::ROOTFS_MOUNTPOINT,
            fsfreeze_lock: Arc::new(tokio::sync::Semaphore::new(1)),
            memory: read_memory_protection(
                std::path::Path::new("/proc/self/cgroup"),
                std::path::Path::new("/sys/fs/cgroup"),
            ),
        }
    }
}

pub fn app(state: Arc<Daemon>) -> Router {
    app_with_extensions(state, &crate::ext::Extensions::empty())
}

/// Mounts extensions on top of the plain envd router.
///
/// The merge happens after [`crate::http::layered`], so the envd frontdoor,
/// authorization and CORS layers do not run for extension requests. An empty
/// set is a no-op and leaves the S0 router exactly as [`app`] built it.
pub fn app_with_extensions(state: Arc<Daemon>, extensions: &crate::ext::Extensions) -> Router {
    extensions.mount(crate::http::layered(http_routes(state.clone()), state))
}

pub fn app_with_filesystem(state: Arc<Daemon>) -> Router {
    app_with_filesystem_and_extensions(state, &crate::ext::Extensions::empty())
}

pub fn app_with_filesystem_and_extensions(
    state: Arc<Daemon>,
    extensions: &crate::ext::Extensions,
) -> Router {
    let rpc = crate::filesystem_rpc::router(state.clone());
    extensions.mount(crate::http::layered(
        http_routes(state.clone()).fallback_service(crate::transport::rpc_service(rpc)),
        state,
    ))
}

pub fn app_with_execution(state: Arc<Daemon>, runtime: crate::process::ProcessRuntime) -> Router {
    app_with_execution_and_extensions(state, runtime, &crate::ext::Extensions::empty())
}

pub fn app_with_execution_and_extensions(
    state: Arc<Daemon>,
    runtime: crate::process::ProcessRuntime,
    extensions: &crate::ext::Extensions,
) -> Router {
    let runtime: Arc<dyn crate::process_api::ProcessRuntimeApi> = Arc::new(runtime);
    let rpc = crate::rpc::process::router(state.clone(), runtime)
        .merge(crate::filesystem_rpc::router(state.clone()));
    extensions.mount(crate::http::layered(
        http_routes(state.clone()).fallback_service(crate::transport::rpc_service(rpc)),
        state,
    ))
}

/// The Linux route table. The shared layers and the two platform-neutral
/// handlers live in [`crate::http`]; this function only lists the operations
/// backed by Linux services.
fn http_routes(state: Arc<Daemon>) -> Router {
    // Every route uses `any` so axum's method router cannot append an `Allow`
    // header to fallback responses (including the 401 authorization returns
    // before the mux); Go's chi mux answers those methods itself, and
    // [`frontdoor::gate`] reproduces its 405/404 before a handler is reached.
    Router::new()
        .route("/health", any(crate::http::health))
        .route("/metrics", any(metrics::get_metrics))
        .route("/envs", any(crate::http::envs::<Daemon>))
        .route("/init", any(init))
        .route("/upgrade", any(crate::upgrade_http::upgrade::<Daemon>))
        .route("/freeze", any(freeze))
        .route("/unfreeze", any(unfreeze))
        .route("/collapse", any(collapse))
        .route("/fsfreeze", any(fsfreeze))
        .route("/fsthaw", any(fsthaw))
        .route("/files", any(files))
        .route(
            "/files/compose",
            any(crate::files_compose::compose::<Daemon>),
        )
        .with_state(state)
}

/// chi registers GET and POST on `/files`; the gate has already rejected every
/// other method, so the dispatcher only routes the two.
async fn files(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    match *request.method() {
        Method::GET => crate::files_http::get_files(State(daemon), request).await,
        Method::POST => crate::files_upload::upload(State(daemon), request).await,
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// Go init.go:494-643 PostFreeze. maxWaitMs is what tells the two callers
/// apart: present and positive means the structured 200 result, absent means
/// the original contract — no wait at all and a bare 204. Pause stays
/// best-effort: an incomplete freeze is reported, never failed.
async fn freeze(
    State(daemon): State<Arc<Daemon>>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    let params = match freeze_params(query.as_deref()) {
        Ok(params) => params,
        Err(error) => return plain_http_error(StatusCode::BAD_REQUEST, &error),
    };
    // Go's asymmetry is deliberate: the THAW is inevitable once started
    // (freeze.go:1208-1212, context.WithoutCancel), the FREEZE respects its
    // caller — the lock acquire is ctx-bounded (freeze.go:575), so a caller
    // that leaves while waiting sweeps nothing and, crucially, holds
    // nothing: a dead /freeze must not sit on the freeze lock polling and
    // delay the rollback thaw queued behind it (freeze.go:555-557). axum's
    // shape of a gone caller is the dropped future, so THIS acquire is the
    // handler's one droppable await. From the permit to the spawn there is no
    // await, so a drop past that point can only land on the join — and the
    // sweep is detached there, owning the permit: once the lock is held the
    // sweep and its accounting must complete (freeze.go:677-710), or a retry
    // would adopt our own walked cgroups as the guest's. Disclosed residue:
    // the sweep's settle poll is not ctx-bounded the way Go's is — a dropped
    // caller waits out its maxWait inside the task instead of returning the
    // lock early; harmless direction, the lock is held at most maxWait longer
    // than Go would hold it.
    let handler_start = tokio::time::Instant::now();
    // init.go:497: PostFreeze derives its logger from a fresh operation id, so
    // every line the handler emits — the flush Warn included — carries it.
    let operation_id = crate::logs::assign_operation_id();
    let permit = daemon.freezer.freeze_hold().await;
    let sweep = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            freeze_body(&daemon, permit, params, handler_start, operation_id).await
        })
    };
    match sweep.await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("freeze handler task failed: {error}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "freeze failed")
        }
    }
}

async fn freeze_body(
    daemon: &Daemon,
    permit: tokio::sync::OwnedSemaphorePermit,
    params: FreezeParams,
    handler_start: tokio::time::Instant,
    operation_id: String,
) -> Response {
    let report = params.max_wait_ms.is_some_and(|value| value > 0);
    let options = crate::cgroups::freeze::FreezeOptions {
        max_wait: Duration::from_millis(params.max_wait_ms.unwrap_or(0).max(0) as u64),
        mode: match params.mode.as_deref() {
            Some("hierarchy") => crate::cgroups::FreezeMode::Hierarchy,
            // Absent or unknown means legacy (init.go:523-527).
            _ => crate::cgroups::FreezeMode::Legacy,
        },
        max_cgroups: params.max_cgroups.unwrap_or(0),
    };
    let (result, error) = daemon.freezer.freeze_held(permit, options).await;
    if let Some(error) = &error {
        // Per-cgroup failures are counted in result.failed and reported in
        // the body, not raised as a 500 (init.go:549-558).
        eprintln!(
            "some cgroups failed to freeze; reporting them in the result: failed={} error={error}",
            result.failed
        );
    }
    // init.go:43/:563-567: the caller's maxWaitMs covers the whole handler, so
    // only its remainder may be spent sending logs. A legacy caller has no
    // observable budget: an already expired context makes this purge-only.
    let flush_budget = if report {
        Duration::from_secs(2).min(
            Duration::from_millis(params.max_wait_ms.unwrap_or(0).max(0) as u64)
                .saturating_sub(handler_start.elapsed()),
        )
    } else {
        Duration::ZERO
    };
    if let Some(flusher) = &daemon.log_flusher {
        if let Err(flush_error) = flusher
            .flush_and_purge(tokio::time::Instant::now() + flush_budget)
            .await
        {
            daemon
                .logger
                .warn()
                // init.go:497 puts operation_id in the handler logger's
                // context, before the Warn event's own error field.
                .str("operation_id", operation_id)
                .err(&flush_error)
                .msg("flush and purge logs before pause");
        }
    }
    // The Go handler answers 503 when the request context dies (init.go:539,
    // :581); in axum a gone client drops this future instead, so no bytes are
    // written to a dead connection either way.
    if !report {
        // init.go:576-580: the bare caller gets a bare 204 — WriteHeader and
        // nothing else. Only the structured report (:664-665) sets no-store.
        return StatusCode::NO_CONTENT.into_response();
    }
    // Observation warnings, init.go:587-625.
    if !result.all_frozen() {
        eprintln!(
            "pre-pause freeze did not stop the whole workload; snapshot may capture it running: \
             requested={} frozen={} not_frozen={} failed={} vanished={}",
            result.requested, result.frozen, result.not_frozen, result.failed, result.vanished
        );
    }
    if result.vanished > 0 {
        eprintln!(
            "cgroups vanished while the freeze was working on them; counted, not failed: \
             vanished={} requested={} frozen={} vanished_paths={:?}",
            result.vanished, result.requested, result.frozen, result.vanished_paths
        );
    }
    if result.scan_failed > 0 {
        eprintln!(
            "could not classify some cgroups as the guest's own; those will be thawed on resume: \
             scan_failed={} pre_frozen={}",
            result.scan_failed, result.pre_frozen
        );
    }
    if result.truncated {
        eprintln!(
            "freeze walk hit its bound; some cgroups were never examined: visited={} requested={}",
            result.visited, result.requested
        );
    }
    if result.unobservable > 0 {
        eprintln!(
            "cgroup freeze state is unobservable on this guest; freeze issued but unverifiable: \
             unobservable={}",
            result.unobservable
        );
    }
    let mut response = Json(result.wire()).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Go init.go:676-691 PostUnfreeze: the orchestrator's pause-failure rollback
/// and the thaw after an in-place checkpoint. The resume thaw runs inside
/// /init, before it answers. Thawing a non-frozen workload is a no-op.
async fn unfreeze(State(daemon): State<Arc<Daemon>>) -> Response {
    match daemon.freezer.unfreeze().await {
        Ok(_) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(error) => {
            eprintln!("unfreeze workload cgroups: {error}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &error)
        }
    }
}

/// Go api/collapse.go PostCollapse: compact the daemon's own anonymous heap
/// with MADV_COLLAPSE just before pause, best-effort, and report the six
/// stats. The request body is never read.
async fn collapse(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    let _ = request; // deferred Body close like Go's `defer r.Body.Close()`
    let _ = &daemon;
    let started = tokio::time::Instant::now();
    // Go passes r.Context() into CollapseSelf (api/collapse.go:26), so a
    // caller that leaves stops the madvise sweep. hyper models that by
    // dropping this handler future, and the guard below flips the flag the
    // blocking sweep polls — the CancelOnDrop's Drop is the cancellation
    // delivery.
    struct CancelOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            use std::sync::atomic::Ordering;
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _guard = CancelOnDrop(cancelled.clone());
    let outcome = {
        let cancelled = cancelled.clone();
        tokio::task::spawn_blocking(move || crate::collapse::collapse_self(&cancelled)).await
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

/// Go api/fsfreeze.go: FIFREEZE the guest rootfs so a filesystem-only pause
/// flushes to a consistent on-disk state. Serialized on one lock; freezing an
/// already-frozen filesystem is a no-op.
async fn fsfreeze(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
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
    // Go acquires fsFreezeLock with the REQUEST context for FIFREEZE: a caller
    // that leaves while contended abandons the wait (api/fsfreeze.go:26-29).
    // FITHAW acquires with context.WithoutCancel (api/fsfreeze.go:55): once
    // the thaw is requested it must land even if the client disconnects, so
    // the rollback thaw can never be dropped by request teardown. Freeze keeps
    // the request-bound acquire; thaw runs in a detached task that owns its
    // own acquire, mirroring the oracle split.
    let outcome = match op {
        FsFreezeOp::Freeze => match daemon.fsfreeze_lock.clone().acquire_owned().await {
            Ok(permit) => run_fsfreeze_ioctl(&daemon, op, permit).await,
            Err(error) => Err(std::io::Error::other(error.to_string())),
        },
        FsFreezeOp::Thaw => {
            let owner = daemon.clone();
            tokio::spawn(async move {
                match owner.fsfreeze_lock.clone().acquire_owned().await {
                    Ok(permit) => run_fsfreeze_ioctl(&owner, FsFreezeOp::Thaw, permit).await,
                    Err(error) => Err(std::io::Error::other(error.to_string())),
                }
            })
            .await
            .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())))
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
                "{} rootfs: {error}",
                match op {
                    FsFreezeOp::Freeze => "freeze",
                    FsFreezeOp::Thaw => "thaw",
                }
            );
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
    }
}

/// Runs the FIFREEZE/FITHAW ioctl on the blocking pool while holding the
/// serialized fsfreeze permit.
async fn run_fsfreeze_ioctl(
    daemon: &Daemon,
    op: FsFreezeOp,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> std::io::Result<()> {
    let freezer = daemon.fsfreeze.clone();
    let mountpoint = daemon.fsfreeze_mountpoint;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match op {
            FsFreezeOp::Freeze => freezer.freeze(mountpoint),
            FsFreezeOp::Thaw => freezer.thaw(mountpoint),
        }
    })
    .await
    .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())))
}

/// Go init.go:122-137: the resume audit header, counts without pointers and
/// without omitempty — a zero violation count is a meaningful report, not an
/// absent one.
#[derive(Debug, Serialize)]
struct FreezeAuditWire {
    visited: i64,
    frozen: i64,
    escaped: i64,
    violations: i64,
    truncated: bool,
}

async fn init(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    // Go runs a handler to completion even when its client leaves; the request
    // context is what notices (ctx.Err()), cancelling the ctx-aware waits —
    // the body read, the /init lock, the MMDS hash; init.go:275's lock acquire
    // fails outright on a dead context — so a gone client holds none of them
    // past its departure. The thaw is the exception: once registered
    // (post-auth, init.go:292-313) it runs on every return via the defer, and
    // its freeze-lock wait is context.WithoutCancel (freeze.go:379-381,
    // :1208-1212) so a dropped unfreeze can never strand the workload frozen.
    // axum models a gone client by dropping the handler future, so this
    // handler stays inline — a drop lands on the cancellable phase exactly
    // where Go's context aborts — and finalize_init_response runs the
    // registered thaw in a task of its own, detached from this future.
    let client_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Dropping THIS future is the departure: the flag flips on drop, and the
    // detached thaw reads it after the thaw, inside the freeze lock — the
    // exact point Go reads ctx.Err() (init.go:706-713).
    struct GoneFlag(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for GoneFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _guard = GoneFlag(client_gone.clone());
    let mut response = apply_init(daemon.clone(), request, client_gone).await;
    // Go init.go:211-218: a handover-booted envd advertises the outcome on
    // EVERY /init response — error and stale responses included (this runs
    // before any status-specific header logic).
    if let Some(outcome) = daemon.handover_outcome() {
        if let Ok(value) = serde_json::to_string(&outcome).and_then(|value| {
            header::HeaderValue::from_str(&value).map_err(serde::ser::Error::custom)
        }) {
            response.headers_mut().insert("x-envd-handover", value);
        }
    }
    response
        .headers_mut()
        .insert("x-envd-version", header::HeaderValue::from_static("0.10.2"));
    if let Ok(value) = serde_json::to_string(&daemon.memory)
        .and_then(|value| header::HeaderValue::from_str(&value).map_err(serde::ser::Error::custom))
    {
        response.headers_mut().insert("x-envd-memory", value);
    }
    // Go init.go:365-368: only a response whose client is still connected
    // when its headers are decided carries the count; one written into a
    // connection already dead would reset it unread. The dead connection is
    // THIS dropped future, which never reaches this take — so the count the
    // detached thaw recorded survives for the next /init to carry.
    if response.status() == StatusCode::NO_CONTENT {
        let count = daemon.freezer.take_unobserved_inits();
        if let Ok(value) = header::HeaderValue::from_str(&count.to_string()) {
            response
                .headers_mut()
                .insert("x-envd-init-unobserved", value);
        }
    }
    response
}

async fn apply_init(
    daemon: Arc<Daemon>,
    request: Request<Body>,
    client_gone: Arc<std::sync::atomic::AtomicBool>,
) -> Response {
    // Go init.go:227-235: read on entry, before the first body read and
    // before the /init lock, and compared again by the thaw below: a freeze
    // that lands at any point before that thaw, including while this handler
    // waits on either, has voided it. Under Expect: 100-continue the body
    // goes out only at the first body read, after this read.
    let freeze_generation = daemon.freezer.freeze_generation().await;
    let body = match crate::init_payload::collect_body(request.into_body()).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::BAD_REQUEST, "failed to read request body").into_response(),
    };
    // Go init.go:250: the correction below measures this handler's own delay
    // from the end of the body read, so the reading is taken now, before the
    // body is decoded.
    let body_read = (daemon.now)();
    // The body decoder is shared with the Windows daemon so both targets
    // accept and reject exactly the bodies the oracle does.
    let payload: InitRequest = match crate::init_payload::decode(&body) {
        Ok(payload) => payload,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let token = payload.access_token;
    let init_guard = daemon.init_lock.clone().lock_owned().await;
    let request_token = token.as_deref().map(|value| value.as_str());
    let matches_current = daemon
        .state
        .lock()
        .await
        .access
        .matches_current_token(request_token);
    let mmds_hash = if matches_current {
        None
    } else {
        // The provider owns route repair and retry; ordinary requests stay available.
        daemon.token_hash_provider.token_hash().await.ok()
    };
    let mut state = daemon.state.lock().await;
    if let Err(error) = state
        .access
        .validate_init(request_token, mmds_hash.as_deref())
    {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    // Go init.go:480-484: setupNFS runs INSIDE SetData below (the arm sits
    // after the CA install, the last SetData effect before it), so a mount
    // failure lands after the audit and the thaw defer, exactly like the
    // oracle's writeInitError path; the oracle /init has no 501.
    // Go init.go:301: audit the frozen set BEFORE anything thaws it, and
    // advertise the counts on the response header so the orchestrator can
    // record them fleet-wide. Advisory throughout — an observer must never be
    // able to fail the resume it is observing (init.go:1096-1101).
    let audit_header = audit_frozen_set(&daemon.freezer).await;
    // Go init.go:306-314: the thaw assembly. Only "skip" withholds it; an
    // absent field, "inline" and any value this envd does not know thaw (the
    // side on which a guest is never left frozen, init.go:695-697).
    let skip_thaw = payload.thaw.as_deref() == Some("skip");
    let thaw_header = skip_thaw.then(|| header::HeaderValue::from_static("skip"));
    // Go init.go:305-313: the thaw is registered HERE — after the audit,
    // before the correction and SetData — as a defer: every return past this
    // point thaws. Spawning the task now makes that hold for dropped futures
    // too, the axum shape of a context dead mid-SetData: detached from this
    // future, the thaw lands however the handler exits (a drop on the CA
    // install or cpu target await included), exactly what Go's defer does at
    // return. finalize below only awaits the handle, so the success path
    // still answers only after the thaw (init.go:361); the client-gone
    // decision stays where Go puts it — read after the thaw, inside the
    // freeze lock (init.go:706-713) — and the outer handler's take on 204
    // stays the count's only reader (init.go:365-368).
    //
    // Disclosed timing difference (F-N1a): the spawned task begins executing
    // at its registration, so the thaw can run CONCURRENTLY with the rest of
    // SetData (set_token/envs/defaults, the CA install, the cpu
    // spawn_blocking). Go's defer only starts at the return — strictly after
    // SetData completes (init.go:337 -> :356 -> :361). Harmless direction:
    // the thaw can only become available EARLIER than Go's, never leave the
    // workload frozen, and the generation check inside thaw_if_generation
    // keeps a freeze that arrives mid-SetData winning exactly as in Go.
    let thaw_task = if skip_thaw {
        None
    } else {
        let freezer = daemon.freezer.clone();
        let gone = client_gone.clone();
        Some(tokio::spawn(async move {
            freezer
                .thaw_if_generation(freeze_generation, move || {
                    gone.load(std::sync::atomic::Ordering::SeqCst)
                })
                .await
        }))
    };
    // Go init.go:316-326: everything since the body read -- the lock, the
    // token check, the audit -- is time the guest clock would otherwise lose.
    // The corrected timestamp replaces the request's own, the whole
    // milliseconds (truncated toward zero) are answered on
    // X-Envd-Init-Clock-Correction, and both the newest-wins compare below and
    // the step's bands apply to the corrected value. The measurement moved
    // below the audit and the thaw assembly with the freeze slice, matching
    // the oracle order (:301 -> :306 -> :316).
    let mut correction_header: Option<String> = None;
    let timestamp = payload.timestamp.map(|ts| {
        let correction = (daemon.now)() - body_read;
        let millis = correction.clamp(i64::MIN as i128, i64::MAX as i128) / 1_000_000;
        correction_header = Some((millis as i64).to_string());
        ts + correction
    });
    // Go init.go:336: update only if the corrected timestamp is newer than (or
    // equal to, utils/atomic.go SetToGreater) the last accepted one; a
    // stale/replayed /init skips SetData and the cpu target entirely but the
    // handler still answers 204 with the in-effect reports.
    let gated = match timestamp {
        None => true,
        Some(corrected) => {
            let nanos = crate::clock::unix_nanos_wrapping(corrected);
            let previous = daemon
                .last_set_time
                .fetch_max(nanos, std::sync::atomic::Ordering::Relaxed);
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
            // Go init.go:313: the deferred thaw covers the error returns.
            // The guard drops BEFORE the finalize: the thaw it awaits is
            // queued on the freeze permit, and an in-flight upgrade holds
            // that permit while wanting this state lock (doUpgrade's
            // current_defaults) — awaiting through the guard would deadlock
            // both and strand the workload frozen. The success path drops at
            // the same relative point, before its own waits.
            drop(state);
            let response = json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            return finalize_init_response(
                thaw_task,
                freeze_generation,
                thaw_header,
                audit_header,
                response,
            )
            .await;
        }
        // Go init.go:459-461: the hyperloop task is fire-and-forget and its
        // outcome never affects the response.
        if let Some(address) = payload.hyperloop_ip {
            if let Some(hyperloop) = daemon.hyperloop.clone() {
                tokio::spawn(hyperloop.setup(address));
            }
        }
        // Go init.go:462-469.
        if let Some(user) = payload.default_user.filter(|v| !v.is_empty()) {
            state.defaults.user = user;
            state.defaults.user_delivered = true;
        }
        if let Some(workdir) = payload.default_workdir.filter(|v| !v.is_empty()) {
            state.defaults.workdir = Some(workdir);
        }
        // Go init.go:474-478: the CA install is the one SetData effect that
        // can fail the /init. writeInitError maps ErrCAInstallInProgress to
        // 503 (warn "CA initialization still in progress, retrying") and
        // everything else to 400 (error "Failed to set data"); the body is
        // the bare err.Error() text and the correction header already on the
        // context survives the error response.
        if let Some(ca_bundle) = payload.ca_bundle.filter(|value| !value.is_empty()) {
            if let Some(installer) = &daemon.ca_cert_installer {
                if let Err(error) = installer.install(&ca_bundle).await {
                    let in_progress = matches!(error, crate::cacerts::CaError::InProgress);
                    let status = error.init_status();
                    let body = error.to_string();
                    if in_progress {
                        eprintln!("CA initialization still in progress, retrying: {error}");
                    } else {
                        eprintln!("Failed to set data: {error}");
                    }
                    let response = write_init_error(status, &body, correction_header);
                    // Go init.go:313: the deferred thaw covers the error
                    // returns. The guard drops BEFORE the finalize: the thaw
                    // it awaits is queued on the freeze permit, and an
                    // in-flight upgrade holds that permit while wanting this
                    // state lock (doUpgrade's current_defaults) — awaiting
                    // through the guard would deadlock both and strand the
                    // workload frozen. The success path drops at the same
                    // relative point, before its own waits.
                    drop(state);
                    return finalize_init_response(
                        thaw_task,
                        freeze_generation,
                        thaw_header,
                        audit_header,
                        response,
                    )
                    .await;
                }
            }
        }
        // Go init.go:480-484: setupNFS, the LAST SetData effect. A failure
        // leaves effects 1-7 applied and `initialized` unset, and still thaws
        // through the deferred task (finding 15). The mounts run in a task
        // DETACHED from this future — Go passes context.WithoutCancel into
        // setupNFS (init.go:805), so a client that leaves mid-mount never
        // aborts the flow; in axum the equivalent hazard is this future being
        // dropped, and the detached task is what survives it. The state guard
        // drops before the await for the same reason the CA error path drops
        // it: the thaw task and the mount task must not deadlock against an
        // in-flight upgrade's state read.
        if let Some(volume_mounts) = payload.volume_mounts {
            drop(state);
            let nfs_daemon = daemon.clone();
            let lifecycle_id = payload.lifecycle_id.clone().unwrap_or_default();
            let mounted =
                tokio::spawn(
                    async move { nfs_daemon.setup_nfs(&lifecycle_id, &volume_mounts).await },
                )
                .await;
            match mounted {
                // Go init.go:356: only a SetData that RETURNED marks the
                // daemon initialized, so the failure paths below never do.
                Ok(Ok(())) => {
                    state = daemon.state.lock().await;
                }
                Ok(Err(volume_error)) => {
                    // Go init.go:730-747 writeInitError: an nfsVolumeError
                    // anywhere in the wrap chain sets X-Envd-Failed-Volume to
                    // the raw failing path (an empty path sets an empty
                    // value); the default branch answers a bare 400 whose
                    // body is err.Error() — no JSON envelope, no forced
                    // Content-Type.
                    eprintln!("Failed to set data: failed to setup NFS volumes: {volume_error}");
                    let body = format!("failed to setup NFS volumes: {volume_error}");
                    let mut response =
                        write_init_error(StatusCode::BAD_REQUEST, &body, correction_header);
                    if let Ok(value) =
                        header::HeaderValue::from_bytes(volume_error.path().as_bytes())
                    {
                        response.headers_mut().insert("x-envd-failed-volume", value);
                    }
                    return finalize_init_response(
                        thaw_task,
                        freeze_generation,
                        thaw_header,
                        audit_header,
                        response,
                    )
                    .await;
                }
                // A panic in the mount flow has no oracle counterpart (the
                // Go process would die); answer 500 rather than hang.
                Err(join_error) => {
                    eprintln!("NFS setup task failed: {join_error}");
                    let response =
                        json_error(StatusCode::INTERNAL_SERVER_ERROR, "NFS setup task failed");
                    return finalize_init_response(
                        thaw_task,
                        freeze_generation,
                        thaw_header,
                        audit_header,
                        response,
                    )
                    .await;
                }
            }
        }
    }
    // Go init.go:356: initialized is stored only once SetData returns, so a
    // failed CA install leaves the sandbox uninitialized.
    state.access.mark_initialized();
    let mut defaults =
        json!({"user":state.defaults.user,"fallback":!state.defaults.user_delivered});
    if let Some(workdir) = &state.defaults.workdir {
        defaults["workdir"] = json!(workdir);
    }
    drop(state);
    let cpus = daemon.cpus.clone();
    // Go init.go:346-353: the cpu target sits inside the newest-wins gate.
    let target = if gated { payload.cpu_count } else { None };
    let metadata = daemon.metadata.clone();
    let cpu_report = tokio::task::spawn_blocking(move || {
        // Init serialization survives HTTP cancellation without blocking other routes.
        let rejected =
            target.and_then(|target| cpus.set_target(target).err().map(|error| error.to_string()));
        let status = cpus.status();
        let mut report = json!({
            "online": status.online,
            "possible": status.possible,
            "target": status.target,
        });
        if let Some(rejected) = rejected {
            eprintln!("ignoring cpu count: {rejected}");
            report["rejected"] = json!(rejected);
        }
        if let Some(metadata) = metadata {
            // The accepted init transaction continues even if its HTTP caller leaves.
            metadata.start_post_init();
        }
        drop(init_guard);
        (report.to_string(), defaults.to_string())
    })
    .await;
    let mut response = StatusCode::NO_CONTENT.into_response();
    match cpu_report {
        Ok((report, defaults)) => {
            match header::HeaderValue::from_str(&report) {
                Ok(value) => {
                    response.headers_mut().insert("x-envd-cpus", value);
                }
                Err(error) => eprintln!("could not encode CPU state report: {error}"),
            }
            match header::HeaderValue::from_str(&defaults) {
                Ok(value) => {
                    response.headers_mut().insert("x-envd-defaults", value);
                }
                Err(error) => eprintln!("could not encode effective defaults: {error}"),
            }
        }
        Err(error) => eprintln!("could not collect CPU state report: {error}"),
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    // Go init.go:324: the whole milliseconds ride along on every timestamped
    // /init, refused or stepped alike.
    if let Some(correction) = correction_header {
        if let Ok(value) = header::HeaderValue::from_str(&correction) {
            response
                .headers_mut()
                .insert("x-envd-init-clock-correction", value);
        }
    }
    // Go init.go:361: on the success path the thaw runs before the response
    // is decided, so a client that leaves during a slow thaw is seen as gone
    // rather than handed a count in a response it will never read. The thaw
    // task was registered right after the audit; awaiting its handle here
    // keeps that order.
    finalize_init_response(
        thaw_task,
        freeze_generation,
        thaw_header,
        audit_header,
        response,
    )
    .await
}

/// Go init.go:1096-1165 auditFrozenSet: classify the frozen set at resume and
/// report the counts on the X-Envd-Freeze-Audit header. Advisory throughout:
/// every error path degrades to "say nothing", and a manager that cannot walk
/// paths (no hierarchy to audit) reports nothing at all.
async fn audit_frozen_set(freezer: &Freezer) -> Option<header::HeaderValue> {
    let result = freezer
        .audit_frozen_set(crate::cgroups::DEFAULT_THAW_MAX_CGROUPS)
        .await?;
    let audit = match result {
        Ok(audit) => audit,
        Err(error) => {
            eprintln!("could not audit the frozen cgroup set at resume: {error}");
            return None;
        }
    };
    if !audit.applicable {
        // Nothing was frozen, so this guest was not resumed from a paused
        // snapshot that froze anything; reporting escapes here would count
        // every legitimately running cgroup as one (init.go:1123-1129).
        return None;
    }
    if audit.truncated {
        eprintln!(
            "the resume audit stopped at its bound; the rest of the hierarchy was not classified: \
             visited={}",
            audit.visited
        );
    }
    if audit.violations > 0 {
        // A frozen cgroup that the resume depends on: the allowlist did not
        // hold. The paths are joined into ONE string — envd's log pipeline
        // carries scalar fields and drops arrays (init.go:1140-1157).
        eprintln!(
            "cgroups the resume depends on were frozen; the allowlist did not hold: \
             violations={} frozen={} visited={} paths={}",
            audit.violations,
            audit.frozen,
            audit.visited,
            audit.violation_paths.join(" ")
        );
    }
    if audit.escaped > 0 {
        eprintln!(
            "cgroups were running at resume that the pre-pause sweep should have stopped: \
             escaped={} frozen={} paths={}",
            audit.escaped,
            audit.frozen,
            audit.escaped_paths.join(" ")
        );
    }
    let wire = serde_json::to_string(&FreezeAuditWire {
        visited: audit.visited,
        frozen: audit.frozen,
        escaped: audit.escaped,
        violations: audit.violations,
        truncated: audit.truncated,
    })
    .ok()?;
    header::HeaderValue::from_str(&wire).ok()
}

/// Go init.go:313/:361: the thaw runs ONCE — the success path calls it before
/// the response is decided, and the deferred install (:313) covers the error
/// returns — then the assembled headers ride the response. A thaw that finds a
/// newer generation does nothing but say so (init.go:716-720); a thaw that
/// fails logs it (init.go:723-725). The thaw was registered right after the
/// audit (the init.go:305-313 defer point) and runs in a task detached from
/// the handler future, so a dropped outer future — the axum shape of a gone
/// client, where Go's context dies — stops neither the thaw nor its
/// context.WithoutCancel lock wait (freeze.go:379-381, :1208-1212); this
/// function only awaits the handle, so the success path still answers only
/// after the thaw (init.go:361). The client-gone decision was read AFTER the
/// thaw, inside the freeze lock, from the flag the dropped outer future
/// flips — the exact point Go reads ctx.Err() (init.go:706-713). The count's
/// take stays with the outer handler, which only survives to answer when its
/// client did (init.go:365-368).
async fn finalize_init_response(
    thaw_task: Option<tokio::task::JoinHandle<crate::cgroups::freeze::ThawIfGeneration>>,
    freeze_generation: u64,
    thaw_header: Option<header::HeaderValue>,
    audit_header: Option<header::HeaderValue>,
    mut response: Response,
) -> Response {
    if let Some(thaw) = thaw_task {
        match thaw.await {
            Ok(outcome) => {
                if !outcome.matched {
                    eprintln!(
                        "a freeze came after this /init entered; not thawing the workload: \
                         entry_freeze_generation={} current_freeze_generation={}",
                        freeze_generation, outcome.current
                    );
                } else if let Some(error) = &outcome.error {
                    eprintln!("unfreeze workload cgroups: {error}");
                }
            }
            // Nobody aborts this task; a join error is a panic in the thaw.
            Err(error) => eprintln!("init thaw task failed: {error}"),
        }
    }
    let mut headers = std::mem::take(response.headers_mut());
    if let Some(value) = thaw_header {
        headers.insert("x-envd-thaw", value);
    }
    if let Some(value) = audit_header {
        headers.insert("x-envd-freeze-audit", value);
    }
    *response.headers_mut() = headers;
    response
}

/// Go init.go:730-747 writeInitError: the status classifies the error and
/// init.go:746 writes the bare err.Error() text — no JSON envelope and no
/// forced Content-Type (Go sniffs the plain text). The clock-correction
/// header was set on the echo context before the gate and survives the error
/// response.
fn write_init_error(
    status: StatusCode,
    message: &str,
    correction_header: Option<String>,
) -> Response {
    let mut response = (status, message.to_owned()).into_response();
    if let Some(correction) = correction_header {
        if let Ok(value) = header::HeaderValue::from_str(&correction) {
            response
                .headers_mut()
                .insert("x-envd-init-clock-correction", value);
        }
    }
    response
}

#[cfg(test)]
mod body_tests {
    use super::*;

    #[tokio::test]
    async fn fragmented_body_and_read_failure_use_the_protected_collector() {
        use axum::body::Bytes;
        let body = Body::from_stream(futures_util::stream::iter([
            Ok::<_, std::io::Error>(Bytes::from(br#"{"accessToken":"sensitive"#.to_vec())),
            Ok(Bytes::from(br#"-token"}"#.to_vec())),
        ]));
        let protected = crate::init_payload::collect_body(body).await.unwrap();
        assert_eq!(
            protected.as_slice(),
            br#"{"accessToken":"sensitive-token"}"#
        );
        let body = Body::from_stream(futures_util::stream::iter([
            Ok(Bytes::from(b"partial-secret".to_vec())),
            Err(std::io::Error::other("read interrupted")),
        ]));
        assert!(crate::init_payload::collect_body(body).await.is_err());
    }

    #[tokio::test]
    async fn init_body_takes_ownership_of_the_original_allocation() {
        let original = br#"{"accessToken":"sensitive-original-buffer"}"#.to_vec();
        let original_pointer = original.as_ptr();
        let protected: zeroize::Zeroizing<Vec<u8>> =
            crate::init_payload::collect_body(Body::from(original))
                .await
                .unwrap();
        assert_eq!(protected.as_ptr(), original_pointer);
        assert!(protected.windows(9).any(|bytes| bytes == b"sensitive"));
    }
}

/// F1 (review round 1): the /upgrade choreography must not be bound to the
/// connection. Go's handler goroutine always completes doUpgrade — net/http
/// never cancels a handler on a client disconnect, and the deferred
/// release+thaw runs on EVERY non-exec exit (main.go:382-385) — while the
/// orchestrator treats the dropped connection as success (main.go:426). In
/// axum a disconnect DROPS the handler future, so a run awaited inline in the
/// handler dies with the connection: the workload would stay frozen with the
/// spawn barrier up until the watchdog while the orchestrator recorded a
/// success that never exec'd.
#[cfg(test)]
mod upgrade_connection_tests {
    use super::*;

    /// The connection drops mid-upgrade — modeled deterministically by
    /// aborting the handler future once it is parked inside the run (abort is
    /// the same drop hyper performs on a dead connection) — and the handover
    /// window then passes. The choreography must still run to completion:
    /// its release+thaw tail (main.go:377-383) must fire even though no
    /// response will ever be written.
    #[tokio::test]
    async fn a_dropped_connection_leaves_the_upgrade_choreography_running() {
        let daemon = Arc::new(Daemon::new());
        daemon.state.lock().await.access.mark_initialized();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (release_tx, release_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (tail_tx, mut tail_rx) = tokio::sync::mpsc::channel::<()>(1);
        // The run closure is a shared Fn, so the one-shot receiver rides in a
        // take-once slot; the run happens exactly once in this test.
        let release_slot = Arc::new(std::sync::Mutex::new(Some(release_rx)));
        // The choreography under test: park inside the handover window (the
        // freeze hold is kept, the exports in flight), then run the tail.
        let slot_for_run = release_slot.clone();
        daemon.set_upgrade_run(Arc::new(move |_new_bin: Option<String>| {
            let started_tx = started_tx.clone();
            let mut release_rx = slot_for_run
                .lock()
                .expect("test slot")
                .take()
                .expect("the run happens once");
            let tail_tx = tail_tx.clone();
            Box::pin(async move {
                let _ = started_tx.send(()).await;
                // Mid-handover: the await point a disconnect would land on.
                let _ = release_rx.recv().await;
                // The tail (main.go:377-383): runs on every non-exec exit,
                // however the connection ended.
                let _ = tail_tx.send(()).await;
                Ok(())
            })
        }));
        let request = Request::builder().body(Body::empty()).expect("request");
        let handler = tokio::spawn(crate::upgrade_http::upgrade(State(daemon.clone()), request));
        // Rendezvous: the handler is parked inside the choreography's
        // handover window.
        started_rx
            .recv()
            .await
            .expect("the handler never reached the upgrade run");
        // The client disconnects: hyper drops the handler future.
        handler.abort();
        assert!(
            handler.await.unwrap_err().is_cancelled(),
            "the handler future is what the disconnect drops"
        );
        // The daemon's own copy of the choreography must not keep the tail
        // channels alive: the only live sender after this is the running
        // choreography itself.
        drop(daemon);
        // The handover window passes. The choreography must complete even
        // though its connection is gone. On the broken inline shape the run
        // died with the aborted handler, so this send finds the receiver
        // gone and the tail below never arrives.
        release_tx
            .send(())
            .await
            .expect("the choreography died with the connection");
        assert_eq!(
            tail_rx.recv().await,
            Some(()),
            "the upgrade choreography died with the connection: the release+thaw tail never ran",
        );
    }
}

/// F2 (review round 1): an error-returning /init must not hold the daemon
/// state lock while it awaits its permit-bound thaw. The in-flight upgrade
/// keeps the freeze hold and then takes the state lock (doUpgrade's
/// current_defaults read, main.rs:637 -> :665), and the error paths' thaw task
/// queues on that same permit — so an error path that awaits
/// [`finalize_init_response`] while still holding the state guard deadlocks
/// both sides and leaves the workload frozen with the barrier up. Go has no
/// such edge: init.go:313's defer runs at the return, after SetData (and its
/// api mutex) is done with the state.
#[cfg(test)]
mod init_error_path_tests {
    use super::*;

    /// The CA-error path under an armed freeze permit. The failing /init must
    /// release the state guard before awaiting its thaw, so the upgrade side
    /// can take the state lock it needs and the queued thaw can land once the
    /// permit is released. Every wait is a bounded timeout; no wall-clock
    /// sleeps (the runtime is time-paused, so a starved wait fires instantly
    /// and a healthy one never waits at all).
    #[tokio::test(start_paused = true)]
    async fn a_failing_init_releases_the_state_guard_before_awaiting_the_permit_bound_thaw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let daemon = Arc::new(
            Daemon::new().with_ca_certinstaller(Arc::new(
                crate::cacerts::CaCertInstaller::builder()
                    .state_path(dir.path().join("state.json"))
                    .bundle_path(dir.path().join("ca-bundle.crt"))
                    .cert_dir(dir.path().join("certs"))
                    .build(),
            )),
        );
        let request = Request::builder()
            .body(Body::from(br#"{"caBundle":"not a pem"}"#.to_vec()))
            .expect("request");
        // Deterministic staging on the current-thread runtime: hold the /init
        // lock so the spawned handler parks there — its first true park (the
        // generation read only takes the then-free permit inline) — then arm
        // the freeze permit the way an in-flight upgrade holds it, and only
        // then let the /init proceed into SetData.
        let init_gate = daemon.init_lock.clone().lock_owned().await;
        let init_daemon = daemon.clone();
        let init_task = tokio::spawn(async move {
            apply_init(
                init_daemon,
                request,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
        });
        for _ in 0..8 {
            if init_task.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        // The upgrade side mid-flight: the freeze hold is kept across the
        // defaults read (doUpgrade, main.rs:637 -> :665).
        let hold = daemon.freezer.freeze_hold().await;
        drop(init_gate);
        // The /init now runs SetData under the armed permit: it takes the
        // state lock, spawns the thaw (which queues on the permit), fails the
        // CA install and parks at the response finalize. Give it the slices
        // to get there; whether the state guard is still held at that park is
        // exactly what the starvation check below decides.
        for _ in 0..64 {
            if init_task.is_finished() || daemon.state.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
        // The upgrade side asks for the live exec context. This must not
        // starve: the failing /init has to have dropped the state guard
        // before awaiting the thaw that is queued on the permit held above.
        tokio::time::timeout(std::time::Duration::from_secs(1), daemon.current_defaults())
            .await
            .expect(
                "current_defaults starved: the failing /init holds the state lock while \
                 awaiting the permit-bound thaw — ABBA with the in-flight upgrade",
            );
        // The permit drops (the upgrade's rollback): the queued thaw must now
        // land and the /init must answer its error.
        drop(hold);
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), init_task)
            .await
            .expect("the failing /init never completed after the permit released")
            .expect("the init task panicked");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[cfg(test)]
mod freezer_sharing_tests {
    use super::*;
    use crate::cgroups::freeze::WorkloadFreezer;
    use crate::cgroups::noop::NoopManager;
    use crate::exec_context::{Account, Defaults, Environment};
    use crate::process::ProcessRuntime;

    fn test_runtime() -> ProcessRuntime {
        ProcessRuntime::new(
            Account {
                name: "envd-share-freezer-test".to_string(),
                home: "/tmp".to_string(),
                uid: 0,
                gid: 0,
                groups: Vec::new(),
            },
            Defaults::default(),
            Environment::default(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn assembling_share_freezer_gives_one_freezer_instance_to_both_services() {
        // Go main.go:243 — ONE workloadFreezer for both services. The guest
        // path: the same Arc lands in the daemon (/freeze, /init's thaw) and
        // the runtime (the spawn barrier probe). Arc::ptr_eq is the proof —
        // separate instances would let a Start slip past a live freeze
        // barrier, which is exactly what the frozen-barrier guard in
        // tests/freeze_api.rs asserts end to end.
        let shared = Arc::new(WorkloadFreezer::new(Arc::new(NoopManager)));
        let (daemon, runtime) = share_freezer(Daemon::new(), test_runtime(), &Some(shared.clone()));
        assert!(Arc::ptr_eq(&daemon.freezer(), &shared));
        assert!(Arc::ptr_eq(runtime.freezer().as_ref().unwrap(), &shared));

        // Development path: no freezer — the runtime adopts the daemon's
        // default no-op freezer so refusal semantics match without host
        // writes.
        let (daemon, runtime) = share_freezer(Daemon::new(), test_runtime(), &None);
        assert!(Arc::ptr_eq(
            runtime.freezer().as_ref().unwrap(),
            &daemon.freezer()
        ));
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use crate::freeze_wire::{go_parse_int, go_query_values};
    use std::io;

    #[test]
    fn go_parse_int_matches_strconv_text_and_bounds() {
        assert_eq!(go_parse_int("42"), Ok(42));
        assert_eq!(go_parse_int("+42"), Ok(42));
        assert_eq!(go_parse_int("-5"), Ok(-5));
        assert_eq!(go_parse_int("9223372036854775807"), Ok(i64::MAX));
        assert_eq!(go_parse_int("-9223372036854775808"), Ok(i64::MIN));
        assert_eq!(
            go_parse_int("abc"),
            Err("strconv.ParseInt: parsing \"abc\": invalid syntax".into())
        );
        assert_eq!(
            go_parse_int(""),
            Err("strconv.ParseInt: parsing \"\": invalid syntax".into())
        );
        assert_eq!(
            go_parse_int(" 1"),
            Err("strconv.ParseInt: parsing \" 1\": invalid syntax".into())
        );
        assert_eq!(
            go_parse_int("9223372036854775808"),
            Err("strconv.ParseInt: parsing \"9223372036854775808\": value out of range".into())
        );
        assert_eq!(
            go_parse_int("1".repeat(100).as_str()),
            Err(format!(
                "strconv.ParseInt: parsing \"{}\": value out of range",
                "1".repeat(100)
            ))
        );
    }

    #[test]
    fn go_query_values_drops_malformed_pairs_and_keeps_the_first_value() {
        let values = go_query_values("maxWaitMs=abc&maxWaitMs=2&%zz=1&mode=a+b&bare");
        assert_eq!(values.get("maxWaitMs").map(String::as_str), Some("abc"));
        assert_eq!(values.get("mode").map(String::as_str), Some("a b"));
        assert_eq!(values.get("bare").map(String::as_str), Some(""));
        assert!(!values.contains_key("%zz"));
        assert!(go_query_values("a=1;b=2").is_empty());
    }

    #[tokio::test]
    async fn freeze_params_render_oapi_binding_errors_and_bind_in_generated_order() {
        let error = freeze_params(Some("maxWaitMs=abc")).unwrap_err();
        assert_eq!(
            error,
            "Invalid format for parameter maxWaitMs: error binding string parameter: \
             strconv.ParseInt: parsing \"abc\": invalid syntax"
        );
        // The handler renders that text through http.Error (text/plain, newline).
        let response = plain_http_error(StatusCode::BAD_REQUEST, &error);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
            &b"Invalid format for parameter maxWaitMs: error binding string parameter: \
              strconv.ParseInt: parsing \"abc\": invalid syntax\n"[..]
        );
        // mode and maxCgroups are bound before maxWaitMs (api.gen.go:723-765).
        let error = freeze_params(Some("maxWaitMs=abc&maxCgroups=zz")).unwrap_err();
        assert_eq!(
            error,
            "Invalid format for parameter maxCgroups: error binding string parameter: \
             strconv.ParseInt: parsing \"zz\": invalid syntax"
        );
        let params = freeze_params(Some("maxWaitMs=-5&mode=hierarchy&maxCgroups=3")).unwrap();
        assert_eq!(params.max_wait_ms, Some(-5));
        assert_eq!(params.mode.as_deref(), Some("hierarchy"));
        assert_eq!(params.max_cgroups, Some(3));
        // Go's URL.Query drops the malformed pair, leaving the default.
        assert_eq!(freeze_params(Some("%zz=1")).unwrap().max_wait_ms, None);
    }

    #[test]
    fn a_nul_path_renders_go_stat_einval_text() {
        let error = io::Error::new(
            io::ErrorKind::InvalidInput,
            "file name contained an unexpected NUL byte",
        );
        assert_eq!(
            crate::errno_text::stat_text("/root/\0", &error),
            "stat /root/\0: invalid argument"
        );
        let error = io::Error::from_raw_os_error(nix::errno::Errno::ENOENT as i32);
        assert_eq!(
            crate::errno_text::stat_text("/root/x", &error),
            "stat /root/x: no such file or directory"
        );
    }
}
