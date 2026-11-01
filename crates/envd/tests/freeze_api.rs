//! Daemon-surface slice for /freeze, /unfreeze and the /init thaw state
//! machine. Oracle: internal/api/init.go:494-643 (PostFreeze), :676-691
//! (PostUnfreeze), :235/:301-324/:361-368 (the /init gates) at
//! 51083a83cc0903185d992725ea6bb33683814b29.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::cgroups::fake::FakeCgroupfs;
use coorworker_envd::cgroups::freeze::{FreezeOptions, WorkloadFreezer};
use coorworker_envd::cgroups::{CgroupError, Manager, PathManager, ProcessType};
use coorworker_envd::cpus::{ChildWrites, CpuManager, CpuSysfs};
use coorworker_envd::daemon::{app, Daemon};
use std::sync::atomic::Ordering;
use tower::ServiceExt;

async fn post(router: &axum::Router, uri: &str, body: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::post(uri)
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn freeze_without_maxwait_answers_the_bare_legacy_204() {
    // Oracle init.go:576-580: the bare (pre-maxWaitMs) caller gets a bare
    // 204 — WriteHeader only, no Cache-Control, no Content-Type. The
    // structured path at :664-665 is the one that sets no-store.
    let router = app(Arc::new(Daemon::new()));
    let response = post(&router, "/freeze", "").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response.headers().get("cache-control").is_none(),
        "the bare freeze answers with no cache header"
    );
    assert!(
        response.headers().get("content-type").is_none(),
        "a bodiless 204 carries no content type"
    );
}

#[tokio::test]
async fn freeze_with_maxwait_answers_the_structured_result() {
    let router = app(Arc::new(Daemon::new()));
    let response = post(&router, "/freeze?maxWaitMs=5", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "application/json");
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    // The no-op manager accepts every write but can observe nothing back
    // (oracle iface.go:66-75): requested counts, unobservable replaces frozen.
    assert_eq!(body["mode"], "legacy");
    assert_eq!(body["requested"], 2);
    assert_eq!(body["unobservable"], 2);
    assert_eq!(body["frozen"], 0);
    assert_eq!(body["notFrozen"], 0);
    assert_eq!(body["failed"], 0);
    // Every field the Go result carries is present: init.go fills each
    // pointer from the result, so nothing is omitted (init.go:627-643).
    for field in [
        "allowlisted",
        "preFrozen",
        "sweepMs",
        "truncated",
        "vanished",
        "visited",
        "waitMs",
    ] {
        assert!(body.get(field).is_some(), "missing {field}");
    }
}

#[tokio::test]
async fn freeze_hierarchy_on_a_manager_without_paths_falls_back_to_legacy() {
    let router = app(Arc::new(Daemon::new()));
    let response = post(&router, "/freeze?maxWaitMs=5&mode=hierarchy", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(body["mode"], "legacy", "oracle freeze.go:617-618");
}

#[tokio::test]
async fn unfreeze_answers_204_and_thawing_a_non_frozen_workload_is_a_no_op() {
    let router = app(Arc::new(Daemon::new()));
    let response = post(&router, "/unfreeze", "").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn init_thaw_skip_is_honoured_and_advertised() {
    let router = app(Arc::new(Daemon::new()));
    let response = post(&router, "/init", r#"{"thaw":"skip"}"#).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["x-envd-thaw"], "skip", "init.go:310");
    assert_eq!(
        response.headers()["x-envd-init-unobserved"],
        "0",
        "init.go:368"
    );
}

#[tokio::test]
async fn init_thaw_inline_and_unknown_values_thaw_without_a_header() {
    let router = app(Arc::new(Daemon::new()));
    for body in [r#"{"thaw":"inline"}"#, r#"{"thaw":"something-else"}"#] {
        let response = post(&router, "/init", body).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
        assert!(
            response.headers().get("x-envd-thaw").is_none(),
            "only skip sets the header (init.go:695-697): {body}"
        );
    }
}

#[tokio::test]
async fn init_volume_mounts_stays_501_with_zero_partial_application() {
    let router = app(Arc::new(Daemon::new()));
    // A NON-empty volumeMounts list is the pending-effect refusal; a null or
    // empty list is the oracle's successful no-op (init.go:480 /
    // init.go:796-874 — pinned by tests/daemon.rs).
    let response = post(&router, "/init", r#"{"volumeMounts":[{"path":"/data"}]}"#).await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    // All-or-nothing staging: no thaw header, no freeze-audit header, nothing
    // applied (coordinator constraint 1).
    assert!(response.headers().get("x-envd-thaw").is_none());
    assert!(response.headers().get("x-envd-freeze-audit").is_none());
    assert!(response.headers().get("x-envd-init-unobserved").is_none());
    // thaw alongside volumeMounts is refused whole, not partially applied.
    let response = post(
        &router,
        "/init",
        r#"{"volumeMounts":[{"path":"/data"}],"thaw":"skip"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(response.headers().get("x-envd-thaw").is_none());
}

#[tokio::test]
async fn init_reports_the_freeze_audit_header_after_a_hierarchy_sweep() {
    let dir = tempfile::TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        dir.path(),
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    std::fs::write(dir.path().join("proc"), "0::/\n").unwrap();
    std::fs::create_dir_all(dir.path().join("system.slice/envd.service")).unwrap();
    let mut opts = FreezeOptions {
        max_wait: Duration::from_millis(20),
        mode: coorworker_envd::cgroups::FreezeMode::Hierarchy,
        ..Default::default()
    };
    opts.max_cgroups = 0;
    let freezer = Arc::new(WorkloadFreezer::new(fs).with_proc_self_cgroup(dir.path().join("proc")));
    let router = app(Arc::new(Daemon::new().with_freezer(freezer.clone())));
    let (result, error) = freezer.freeze(opts).await;
    assert!(error.is_none());
    assert_eq!(result.mode, coorworker_envd::cgroups::FreezeMode::Hierarchy);
    let response = post(&router, "/init", "{}").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    // The audit header is JSON with all five fields, zero values included —
    // the counts are pointers-free and omitempty-free on purpose, because a
    // zero violation count is a meaningful report, not an absent one
    // (init.go:129-137).
    let audit = response.headers()["x-envd-freeze-audit"].to_str().unwrap();
    let value: serde_json::Value = serde_json::from_str(audit).unwrap();
    for field in ["visited", "frozen", "escaped", "violations", "truncated"] {
        assert!(value.get(field).is_some(), "missing {field} in {audit}");
    }
    assert_eq!(value["violations"], 0);
}

#[tokio::test]
async fn init_with_freeze_ahead_of_entry_keeps_the_freeze() {
    let dir = tempfile::TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        dir.path(),
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    std::fs::write(dir.path().join("proc"), "0::/\n").unwrap();
    for cgroup in ["user", "ptys"] {
        std::fs::write(
            dir.path().join(cgroup).join("cgroup.events"),
            "populated 1\nfrozen 1\n",
        )
        .unwrap();
    }
    let freezer = Arc::new(WorkloadFreezer::new(fs).with_proc_self_cgroup(dir.path().join("proc")));
    let router = app(Arc::new(Daemon::new().with_freezer(freezer.clone())));
    // The pause's freeze lands before the /init enters.
    let (result, _) = freezer
        .freeze(FreezeOptions {
            max_wait: Duration::from_millis(20),
            ..Default::default()
        })
        .await;
    assert_eq!(result.requested, 2);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("user/cgroup.freeze")).unwrap(),
        "1"
    );
    let response = post(&router, "/init", "{}").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("user/cgroup.freeze")).unwrap(),
        "0",
        "the /init thaw that matched the generation ran"
    );
    // The audit has no walker memory of a legacy sweep, and the daemon carries
    // no hierarchy audit for a legacy sweep: no audit header (init.go:1133).
    assert!(response.headers().get("x-envd-freeze-audit").is_none());
}

// --- cancellation model: Go reaches these outcomes with context.WithoutCancel;
// axum's equivalent hazard is the dropped handler future, so the handler body
// must survive its own caller being dropped (oracle freeze.go:1208-1212,
// :379-381, :677-710). ---

/// A fake two-cgroup tree whose freeze lock the test can hold, so a handler
/// can be observed parked on it when the simulated disconnect lands.
struct HeldTree {
    freezer: Arc<WorkloadFreezer>,
    dir: tempfile::TempDir,
}

impl HeldTree {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let fs = Arc::new(FakeCgroupfs::new(
            dir.path(),
            &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
        ));
        std::fs::write(dir.path().join("proc"), "0::/\n").unwrap();
        let freezer =
            Arc::new(WorkloadFreezer::new(fs).with_proc_self_cgroup(dir.path().join("proc")));
        Self { freezer, dir }
    }

    fn freeze_file(&self, cgroup: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(cgroup).join("cgroup.freeze"))
            .ok()
            .map(|value| value.trim().to_owned())
    }
}

/// Drops the connection on a parked handler: the abort drops the outer future
/// the way hyper does on a gone client; the sleep gives the runtime its
/// scheduler tick to actually drop that future before whatever it parked on
/// is released.
async fn disconnect<T>(connection: tokio::task::JoinHandle<T>) {
    connection.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Wraps the fake so the test can hold a /init INSIDE its resume audit: the
/// audit is the last stop before the thaw is registered (init.go:300-313), so
/// parking there pins the handler deterministically at its post-auth phase —
/// where Go guarantees the thaw is inevitable — before it can run ahead to
/// the thaw (or all the way to 204) while the test sets the stage. The gate
/// arms only for the resume: the pre-freeze's own hierarchy walk passes
/// through untouched.
struct AuditGate {
    inner: Arc<FakeCgroupfs>,
    gate: Arc<std::sync::atomic::AtomicBool>,
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    entered_once: std::sync::atomic::AtomicBool,
    release: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl Manager for AuditGate {
    fn get_file_descriptor(&self, process_type: ProcessType) -> Option<i32> {
        self.inner.get_file_descriptor(process_type)
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        self.inner.freeze(process_type)
    }
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        self.inner.unfreeze(process_type)
    }
    fn frozen(&self, process_type: ProcessType) -> Result<bool, CgroupError> {
        self.inner.frozen(process_type)
    }
    fn close(&self) -> Result<(), CgroupError> {
        self.inner.close()
    }
    fn as_path_manager(&self) -> Option<&dyn PathManager> {
        Some(self)
    }
}

impl PathManager for AuditGate {
    fn root(&self) -> std::path::PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<std::path::PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &std::path::Path) -> Result<Vec<std::path::PathBuf>, CgroupError> {
        // The resume audit's first children_of announces the handler and
        // holds it until the test has staged the freeze lock.
        if self.gate.load(Ordering::SeqCst) && !self.entered_once.swap(true, Ordering::SeqCst) {
            let _ = self.entered.send(());
            let _ = self.release.lock().unwrap().recv();
        }
        self.inner.children_of(path)
    }
    fn freeze_at(&self, path: &std::path::Path) -> Result<(), CgroupError> {
        self.inner.freeze_at(path)
    }
    fn unfreeze_at(&self, path: &std::path::Path) -> Result<(), CgroupError> {
        self.inner.unfreeze_at(path)
    }
    fn frozen_at(&self, path: &std::path::Path) -> Result<bool, CgroupError> {
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &std::path::Path) -> Result<bool, CgroupError> {
        self.inner.freeze_requested_at(path)
    }
}

/// Parks the handler inside its cpu target update — the spawn_blocking await
/// that sits between the thaw's registration (post-audit) and finalize — so
/// the disconnect lands deterministically AFTER registration, in the exact
/// gap Go's registered defer covers (init.go:305-313) and the one the
/// spawn-at-finalize shape left open.
struct GatedCpuSysfs {
    armed: Arc<std::sync::atomic::AtomicBool>,
    entered_once: std::sync::atomic::AtomicBool,
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl CpuSysfs for GatedCpuSysfs {
    fn possible(&self) -> std::io::Result<Vec<usize>> {
        if self.armed.load(Ordering::SeqCst) && !self.entered_once.swap(true, Ordering::SeqCst) {
            let _ = self.entered.send(());
            let _ = self.release.lock().unwrap().recv();
        }
        Ok(vec![0, 1])
    }
    fn online(&self) -> std::io::Result<Vec<usize>> {
        Ok(vec![0])
    }
    fn set_online(&self, _: usize, _: bool) -> std::io::Result<()> {
        Ok(())
    }
}

struct NoChildren;
impl ChildWrites for NoChildren {
    fn write(&self, _: &std::path::Path, _: bool) -> std::io::Result<()> {
        Ok(())
    }
    fn wait_adopted(&self) {}
}

/// Everything a /init cancellation test needs: a hierarchy-frozen tree, its
/// freezer, the router with the gated cpu manager, and the two gate pairs —
/// the audit gate (to take the freeze lock while the handler is pinned
/// post-auth) and the cpu gate (to pin the drop between the thaw's
/// registration and finalize).
struct GatedResume {
    freezer: Arc<WorkloadFreezer>,
    dir: tempfile::TempDir,
    router: axum::Router,
    audit_entered: tokio::sync::mpsc::UnboundedReceiver<()>,
    audit_release: std::sync::mpsc::Sender<()>,
    cpu_entered: tokio::sync::mpsc::UnboundedReceiver<()>,
    cpu_release: std::sync::mpsc::Sender<()>,
}

/// A frozen tree whose /init can be held inside its resume audit and then
/// pinned inside its cpu target update, with the client dropped in between:
/// init.go:305-313's registered defer meeting a gone client mid-SetData.
async fn gated_resume() -> GatedResume {
    let dir = tempfile::TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        dir.path(),
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    std::fs::write(dir.path().join("proc"), "0::/\n").unwrap();
    let (audit_entered_tx, audit_entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (audit_release_tx, audit_release_rx) = std::sync::mpsc::channel();
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let freezer = Arc::new(
        WorkloadFreezer::new(Arc::new(AuditGate {
            inner: fs,
            gate: gate.clone(),
            entered: audit_entered_tx,
            entered_once: std::sync::atomic::AtomicBool::new(false),
            release: Arc::new(std::sync::Mutex::new(audit_release_rx)),
        }))
        .with_proc_self_cgroup(dir.path().join("proc")),
    );
    let (cpu_entered_tx, cpu_entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (cpu_release_tx, cpu_release_rx) = std::sync::mpsc::channel();
    let cpu_armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cpu = Arc::new(CpuManager::new(
        Arc::new(GatedCpuSysfs {
            armed: cpu_armed.clone(),
            entered_once: std::sync::atomic::AtomicBool::new(false),
            entered: cpu_entered_tx,
            release: Arc::new(std::sync::Mutex::new(cpu_release_rx)),
        }),
        Arc::new(NoChildren),
        dir.path().join("cpu-target"),
    ));
    let router = app(Arc::new(
        Daemon::with_cpu_manager(cpu).with_freezer(freezer.clone()),
    ));
    // The workload is frozen from the pause this /init resumes. Hierarchy
    // mode, because the resume audit only walks for a sweep it has walker
    // memory of (init.go:1133) — a legacy sweep would skip the audit and the
    // handler would never reach the gate. Both gates arm AFTER the freeze so
    // the freeze's own walk passes through.
    let (result, _) = freezer
        .freeze(FreezeOptions {
            max_wait: Duration::from_millis(20),
            mode: coorworker_envd::cgroups::FreezeMode::Hierarchy,
            ..Default::default()
        })
        .await;
    assert_eq!(result.requested, 2);
    gate.store(true, Ordering::SeqCst);
    cpu_armed.store(true, Ordering::SeqCst);
    GatedResume {
        freezer,
        dir,
        router,
        audit_entered: audit_entered_rx,
        audit_release: audit_release_tx,
        cpu_entered: cpu_entered_rx,
        cpu_release: cpu_release_tx,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_thaw_survives_a_client_that_leaves_while_the_freeze_lock_is_held() {
    let mut t = gated_resume().await;
    let connection = tokio::spawn(
        t.router.clone().oneshot(
            axum::http::Request::post("/init")
                .body(axum::body::Body::from(r#"{"cpuCount":2}"#))
                .unwrap(),
        ),
    );
    // The handler reached its resume audit: past auth. Hold the freeze lock,
    // then let the audit finish: the thaw registers right after the audit
    // (the init.go:305-313 defer point) and parks on the freeze lock — the
    // wait Go wraps in context.WithoutCancel (freeze.go:379-381).
    t.audit_entered.recv().await.unwrap();
    let permit = t.freezer.hold_freeze_lock_for_tests().await;
    t.audit_release.send(()).unwrap();
    // The handler is now parked inside its cpu target update — the
    // spawn_blocking await between the thaw's registration and finalize — so
    // the drop lands deterministically after registration, not at some point
    // a slow CI happens to choose.
    t.cpu_entered.recv().await.unwrap();
    // The client leaves while the handler waits on the freeze lock's thaw and
    // the cpu update's join.
    disconnect(connection).await;
    // Release the cpu gate: the spawn_blocking closure is not cancellable, so
    // it finishes its update outside the dead handler the way a real one
    // would; leaving it parked would serialize every later /init behind it.
    t.cpu_release.send(()).unwrap();
    drop(permit);

    // The registered thaw must still land: it is detached from the dropped
    // future the way Go's deferred thaw is detached from the dead context.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Ok(value) = std::fs::read_to_string(t.dir.path().join("user/cgroup.freeze")) else {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            };
            if value.trim() == "0" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the dropped client's registered thaw never ran; the workload stays frozen");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_freeze_client_that_leaves_while_waiting_for_the_lock_sweeps_nothing() {
    let tree = HeldTree::new();
    let (fired_tx, mut fired_rx) = tokio::sync::mpsc::unbounded_channel();
    tree.freezer.set_thaw_watchdog(
        Duration::from_millis(150),
        Some(Arc::new(move |result, _| {
            let _ = fired_tx.send(result.discovered);
        })),
    );
    // Hold the lock so the /freeze parks on its acquire — the one await a
    // gone caller drops out of (freeze.go:575): a dead freeze must not sit on
    // the lock polling and delay the rollback thaw behind it
    // (freeze.go:555-557).
    let permit = tree.freezer.hold_freeze_lock_for_tests().await;
    let router = app(Arc::new(Daemon::new().with_freezer(tree.freezer.clone())));
    let connection = tokio::spawn(
        router.oneshot(
            axum::http::Request::post("/freeze?maxWaitMs=200")
                .body(axum::body::Body::empty())
                .unwrap(),
        ),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    disconnect(connection).await;
    // The parked handler future is dropped at its lock wait — the only await
    // a gone caller can drop out of (freeze.go:575). Sample the tree for any
    // flip to "1" rather than reading the settled state once: the fake
    // pre-writes "0" into every cgroup.freeze, and a wrongly-detached sweep
    // holds "1" for its whole quiescence poll (the fake never reports frozen),
    // so a sampler catches the flip even if the 150ms watchdog thawed it back
    // before the final read.
    let dir = tree.dir.path().to_path_buf();
    let sampler = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_millis(700);
        while std::time::Instant::now() < deadline {
            for cgroup in ["user", "ptys"] {
                if std::fs::read_to_string(dir.join(cgroup).join("cgroup.freeze"))
                    .map(|value| value.trim() == "1")
                    .unwrap_or(false)
                {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    });
    drop(permit);

    let swept = sampler.await.unwrap();
    assert!(
        !swept,
        "a freeze dropped while waiting for the lock must not sweep (freeze.go:575)"
    );
    // No sweep, no armed backstop, no fire.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), fired_rx.recv())
            .await
            .is_err(),
        "nothing swept, so nothing may have armed the thaw watchdog"
    );
    assert_eq!(
        tree.freeze_file("user").as_deref(),
        Some("0"),
        "a freeze dropped while waiting for the lock leaves the freeze state untouched"
    );
    assert_eq!(
        tree.freeze_file("ptys").as_deref(),
        Some("0"),
        "a freeze dropped while waiting for the lock leaves the freeze state untouched"
    );
}

/// Parks the sweep between its two static writes: the first freeze() call
/// writes user, then announces the sweep is mid-flight (the lock is held)
/// and blocks until released, so the disconnect can be pinned INSIDE the
/// sweep — the window where the accounting is still owed. Carries path
/// support because the proof this test wants — the rescue watchdog's
/// callback reporting `discovered` — only happens when the rescue thaw's
/// discovery walk can run (freeze.go:1266-1273), and that walk needs a
/// PathManager.
struct SweepGate {
    inner: Arc<FakeCgroupfs>,
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl Manager for SweepGate {
    fn get_file_descriptor(&self, process_type: ProcessType) -> Option<i32> {
        self.inner.get_file_descriptor(process_type)
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        self.inner.freeze(process_type)?;
        if self.armed.swap(false, Ordering::SeqCst) {
            let _ = self.entered.send(());
            let _ = self.release.lock().unwrap().recv();
        }
        Ok(())
    }
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        self.inner.unfreeze(process_type)
    }
    fn frozen(&self, process_type: ProcessType) -> Result<bool, CgroupError> {
        self.inner.frozen(process_type)
    }
    fn close(&self) -> Result<(), CgroupError> {
        self.inner.close()
    }
    fn as_path_manager(&self) -> Option<&dyn PathManager> {
        Some(self)
    }
}

impl PathManager for SweepGate {
    fn root(&self) -> std::path::PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<std::path::PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &std::path::Path) -> Result<Vec<std::path::PathBuf>, CgroupError> {
        self.inner.children_of(path)
    }
    fn freeze_at(&self, path: &std::path::Path) -> Result<(), CgroupError> {
        self.inner.freeze_at(path)
    }
    fn unfreeze_at(&self, path: &std::path::Path) -> Result<(), CgroupError> {
        self.inner.unfreeze_at(path)
    }
    fn frozen_at(&self, path: &std::path::Path) -> Result<bool, CgroupError> {
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &std::path::Path) -> Result<bool, CgroupError> {
        self.inner.freeze_requested_at(path)
    }
}

/// A fresh tree whose legacy sweep parks between the two static writes.
async fn gated_sweep() -> (
    Arc<WorkloadFreezer>,
    tempfile::TempDir,
    tokio::sync::mpsc::UnboundedReceiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        dir.path(),
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    std::fs::write(dir.path().join("proc"), "0::/\n").unwrap();
    let (entered_tx, entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let freezer = Arc::new(
        WorkloadFreezer::new(Arc::new(SweepGate {
            inner: fs,
            armed: std::sync::atomic::AtomicBool::new(true),
            entered: entered_tx,
            release: Arc::new(std::sync::Mutex::new(release_rx)),
        }))
        .with_proc_self_cgroup(dir.path().join("proc")),
    );
    (freezer, dir, entered_rx, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freeze_accounting_survives_a_client_that_leaves_mid_sweep() {
    let (freezer, dir, mut entered, release) = gated_sweep().await;
    // A short window so the sweep's backstop fires inside the test; the
    // callback reports whether the fire DISCOVERED an open freeze window —
    // freeze_active recorded — which a rescue thaw only reports when the
    // accounting ran (freeze.go:677-710).
    let (fired_tx, mut fired_rx) = tokio::sync::mpsc::unbounded_channel();
    freezer.set_thaw_watchdog(
        Duration::from_millis(400),
        Some(Arc::new(move |result, _| {
            let _ = fired_tx.send(result.discovered);
        })),
    );
    let router = app(Arc::new(Daemon::new().with_freezer(freezer.clone())));
    let connection = tokio::spawn(
        router.oneshot(
            axum::http::Request::post("/freeze?maxWaitMs=200")
                .body(axum::body::Body::empty())
                .unwrap(),
        ),
    );
    // The sweep is mid-flight: the lock is held, user is already written, and
    // the handler has detached from the sweep at the spawn that followed its
    // acquire. Pin the disconnect here.
    entered.recv().await.unwrap();
    disconnect(connection).await;
    release.send(()).unwrap();

    // The aborted sweep must still run to completion: its writes land
    // (freeze.go:674-710 all run after the caller is gone) ...
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Ok(value) = std::fs::read_to_string(dir.path().join("ptys/cgroup.freeze")) else {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            };
            if value.trim() == "1" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the aborted client's sweep never finished its writes");
    // ... and its backstop fires having recorded the open window, so the
    // pause after a failed freeze cannot adopt our own walked cgroups as
    // the guest's.
    let discovered = tokio::time::timeout(Duration::from_secs(5), fired_rx.recv())
        .await
        .expect("the watchdog was never armed; freeze accounting was skipped")
        .unwrap();
    assert!(
        discovered,
        "the watchdog fired but freeze_active was not recorded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_init_abandoned_mid_thaw_is_counted_unobserved_by_the_next_init() {
    let mut t = gated_resume().await;
    let connection = tokio::spawn(
        t.router.clone().oneshot(
            axum::http::Request::post("/init")
                .body(axum::body::Body::from(r#"{"cpuCount":2}"#))
                .unwrap(),
        ),
    );
    t.audit_entered.recv().await.unwrap();
    let permit = t.freezer.hold_freeze_lock_for_tests().await;
    t.audit_release.send(()).unwrap();
    // Pin the drop after the thaw's registration, inside the cpu target
    // update's spawn_blocking await.
    t.cpu_entered.recv().await.unwrap();
    disconnect(connection).await;
    // Release the cpu gate so the un-cancellable spawn_blocking update
    // finishes instead of serializing the follow-up /inits behind a parked
    // write forever.
    t.cpu_release.send(()).unwrap();
    drop(permit);

    // The abandoned resume's thaw still lands ...
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Ok(value) = std::fs::read_to_string(t.dir.path().join("user/cgroup.freeze")) else {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            };
            if value.trim() == "0" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the abandoned resume's thaw never ran");

    // ... and it counts itself unobserved after the thaw, inside the freeze
    // lock (init.go:706-713).
    let response = post(&t.router, "/init", "{}").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()["x-envd-init-unobserved"],
        "1",
        "the abandoned resume was never counted (init.go:711-713)"
    );
    // The take consumes the count (init.go:365-368).
    let response = post(&t.router, "/init", "{}").await;
    assert_eq!(response.headers()["x-envd-init-unobserved"], "0");
}

#[tokio::test]
async fn collapse_reports_all_six_stat_fields_with_no_store() {
    // Oracle api/collapse.go: a POST /collapse runs CollapseSelf over the
    // daemon's own memory and answers 200 with the six-field stats object and
    // Cache-Control: no-store; the request body is never read.
    let router = app(Arc::new(Daemon::new()));
    let response = post(&router, "/collapse", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "application/json");
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    for key in [
        "elapsedMs",
        "collapsed",
        "skipped",
        "alreadyHuge",
        "regions",
        "chunks",
    ] {
        assert!(body.get(key).is_some(), "collapse report key {key}: {body}");
    }
    // Junk bodies collapse all the same (the handler defers r.Body.Close()).
    let response = post(&router, "/collapse", "junk-not-json").await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// A scripted FsFreezer so route tests never touch the dev host's mounts.
type FreezeOp = Box<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>;

struct ScriptedFsFreezer {
    freeze: FreezeOp,
    thaw: FreezeOp,
}

impl coorworker_envd::fsfreeze::FsFreezer for ScriptedFsFreezer {
    fn freeze(&self, mountpoint: &str) -> std::io::Result<()> {
        (self.freeze)(mountpoint)
    }
    fn thaw(&self, mountpoint: &str) -> std::io::Result<()> {
        (self.thaw)(mountpoint)
    }
}

#[tokio::test]
async fn fsfreeze_refusal_reports_the_go_wrapped_error() {
    // Oracle fsfreeze.go:33-37 + error.go:9-21: the refusal is jsonError 500
    // carrying the freezer's wrapped text, with charset and nosniff.
    let freezer = ScriptedFsFreezer {
        freeze: Box::new(|_| Err(std::io::Error::other("FIFREEZE /: operation not supported"))),
        thaw: Box::new(|_| Ok(())),
    };
    let router = app(Arc::new(Daemon::new().with_fsfreeze(Arc::new(freezer))));
    let response = post(&router, "/fsfreeze", "").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    assert_eq!(
        &body[..],
        b"{\"code\":500,\"message\":\"FIFREEZE /: operation not supported\"}\n"
    );
}

#[tokio::test]
async fn fsfreeze_success_and_fsthaw_answer_bodiless_no_store_204s() {
    // Oracle fsfreeze.go:40-41/:68-70.
    let freezer = ScriptedFsFreezer {
        freeze: Box::new(|_| Ok(())),
        thaw: Box::new(|_| Ok(())),
    };
    let router = app(Arc::new(Daemon::new().with_fsfreeze(Arc::new(freezer))));
    for path in ["/fsfreeze", "/fsthaw"] {
        let response = post(&router, path, "").await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response.headers().get("content-type").is_none(),
            "{path} is bodiless"
        );
    }
}

#[tokio::test]
async fn a_frozen_barrier_refuses_start_with_one_end_stream_frame() {
    // Oracle freeze + spawn + handler.go:593: a Start issued while the
    // workload freeze holds answers HTTP 200 carrying a single EndStream
    // envelope (flags 2) with the wrapped unavailable error. This is only
    // observable when the daemon and the process service share the ONE
    // freezer instance — the wiring main.rs establishes for guests
    // (Go main.go:243 passes one workloadFreezer to both services).
    use axum::body::to_bytes;
    use coorworker_envd::cgroups::noop::NoopManager;
    use coorworker_envd::daemon::{app_with_execution, share_freezer};
    use coorworker_envd::exec_context::{Account, Defaults, Environment};
    use coorworker_envd::process::ProcessRuntime;

    let shared = Arc::new(WorkloadFreezer::new(Arc::new(NoopManager)));
    let runtime = {
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
    };
    // Assembled through the production wiring (main.rs calls the same
    // share_freezer), so this guard exercises the real one-freezer assembly
    // shape rather than a hand-rolled equivalent.
    let (daemon, runtime) = share_freezer(Daemon::new(), runtime, &Some(shared.clone()));
    let router = app_with_execution(Arc::new(daemon), runtime);

    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let init_body = serde_json::json!({
        "defaultUser": user.name,
        "defaultWorkdir": "/tmp",
    })
    .to_string();
    let init = post(&router, "/init", &init_body).await;
    assert_eq!(init.status(), StatusCode::NO_CONTENT);

    let freeze = post(&router, "/freeze", "").await;
    assert_eq!(freeze.status(), StatusCode::NO_CONTENT);

    let payload = serde_json::json!({
        "process": {"cmd": "/bin/busybox", "args": ["true"]},
        "stdin": false,
    })
    .to_string();
    let mut envelope = vec![0u8];
    envelope.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    envelope.extend_from_slice(payload.as_bytes());
    let refused = axum::http::Request::post("/process.Process/Start")
        .header("content-type", "application/connect+json")
        .header("connect-protocol-version", "1")
        .body(Body::from(envelope))
        .unwrap();
    let response = router.clone().oneshot(refused).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "refused Start stays 200");
    let data = to_bytes(response.into_body(), 8192).await.unwrap();
    assert!(data.len() >= 5, "refused Start envelope");
    let flags = data[0];
    let size = u32::from_be_bytes(data[1..5].try_into().unwrap()) as usize;
    assert_eq!(flags, 2, "exactly one EndStream frame: {data:?}");
    assert_eq!(data.len(), size + 5, "no bytes beyond the EndStream frame");
    let end: serde_json::Value = serde_json::from_slice(&data[5..]).unwrap();
    assert_eq!(
        end,
        serde_json::json!({"error": {
            "code": "unavailable",
            "message": "error starting process '/bin/busybox true': the target cgroup is frozen",
        }}),
    );

    // Leave no barrier behind on the shared instance.
    let thaw = post(&router, "/unfreeze", "").await;
    assert_eq!(thaw.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_disconnected_fsthaw_still_thaws_the_rootfs() {
    // Oracle api/fsfreeze.go:53-59: PostFsthaw acquires the freeze lock with
    // context.WithoutCancel so a vanished client cannot abandon the rollback
    // thaw; PostFsfreeze (:26) deliberately keeps the request context. The
    // axum-shaped hazard is the same cancellation: hyper drops the handler
    // future on disconnect, so a request-bound acquire loses the thaw exactly
    // when the rootfs is frozen and the caller is gone. The test pins the
    // fsthaw on the contended lock, disconnects it, then releases the freeze:
    // the thaw must still land (the scripted thaw announces itself on a
    // rendezvous channel; nothing wedges — every wait is bounded).
    use std::sync::Mutex;

    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (thaw_tx, thaw_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let thaw_tx = Arc::new(Mutex::new(Some(thaw_tx)));
    let freezer = ScriptedFsFreezer {
        freeze: Box::new(move |_| {
            // Park INSIDE the freeze so the lock stays held while the test
            // stages the contended thaw and its disconnect.
            let _ = entered_tx.send(());
            let _ = release_rx.lock().unwrap().recv();
            Ok(())
        }),
        thaw: Box::new(move |_| {
            if let Some(thaw_tx) = thaw_tx.lock().unwrap().take() {
                let _ = thaw_tx.send(());
            }
            Ok(())
        }),
    };
    let router = app(Arc::new(Daemon::new().with_fsfreeze(Arc::new(freezer))));

    let request = |path: &str| Request::post(path).body(Body::from("")).unwrap();
    let freeze_task = tokio::spawn({
        let router = router.clone();
        async move { router.oneshot(request("/fsfreeze")).await }
    });
    // Deterministic park: the scripted freeze only announces once it holds
    // the lock and is inside its own gate.
    tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
        .await
        .expect("the freeze never reached its gate")
        .expect("entered channel closed");

    let thaw_task = tokio::spawn({
        let router = router.clone();
        async move { router.oneshot(request("/fsthaw")).await }
    });
    // Let the thaw handler advance to its one droppable await — the contended
    // acquire — before the disconnect lands (the same scheduler-tick
    // tolerance the disconnect helper above allows).
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    thaw_task.abort();

    // The freeze finishes; the permit frees. Whatever survived the disconnect
    // may now thaw.
    release_tx.send(()).expect("freeze gate released");
    let freeze = tokio::time::timeout(Duration::from_secs(2), freeze_task)
        .await
        .expect("freeze task wedged")
        .expect("freeze task panicked")
        .expect("oneshot is infallible");
    assert_eq!(freeze.status(), StatusCode::NO_CONTENT);

    let landed = tokio::time::timeout(Duration::from_secs(2), thaw_rx).await;
    assert!(
        matches!(landed, Ok(Ok(()))),
        "a disconnected /fsthaw must still thaw the rootfs (oracle api/fsfreeze.go:55 WithoutCancel)"
    );
}
