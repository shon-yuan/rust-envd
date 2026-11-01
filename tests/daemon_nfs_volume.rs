//! The /init volumeMounts parity slice, against the pinned oracle
//! (internal/api/init.go:480-484, :730-747, :796-1007; nfs_mount.go;
//! nfs_mount_linux.go; mounts_handover.go at
//! 51083a83cc0903185d992725ea6bb33683814b29). Every executor effect is
//! simulated by a scripted fake mounted through the daemon seam — no real
//! NFS, no mount(8) — and every retry/timeout boundary runs on the paused
//! tokio clock, so nothing here sleeps in real time.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::daemon::nfs::{
    exec_failure_text, make_dir_command, mount_command, BoxFut, CommandOutcome, NfsMounter,
};
use coorworker_envd::daemon::{app, Daemon};
use tower::ServiceExt;

/// The compile-time mount options string (oracle nfs_mount.go:33 +
/// init.go:749-770), spelled out here so the argv assertion is a literal.
const MOUNT_OPTS_ARG: &str = "fg,hard,sync,rsize=1048576,wsize=1048576,mountproto=tcp,\
mountport=2049,proto=tcp,port=2049,nfsvers=3,noacl,noac,lookupcache=none,retry=0";

async fn init(router: &axum::Router, body: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 16384)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    MakeDir(Vec<String>),
    Mount(Vec<String>),
    Unmount(String),
    MountId(String),
}

/// What the next mount() call does.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
enum MountScript {
    /// mount(8) reports success and the fake bumps the path's mount ID.
    #[default]
    Ok,
    /// mount(8) reports success but the mount ID does not change.
    SameId,
    /// mount(8) fails immediately with the given full CombinedOutput text.
    Fail(String),
    /// mount(8) hangs past its deadline and is killed.
    Hang,
    /// mount(8) succeeds AND covers another path (a later mount over an
    /// earlier one), replacing that path's mount ID too.
    Overmount(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
enum MkdirScript {
    #[default]
    Ok,
    Fail(String),
    Hang,
}

#[derive(Default)]
struct Inner {
    calls: Vec<Call>,
    mount_script: VecDeque<MountScript>,
    mkdir_script: VecDeque<MkdirScript>,
    default_mount: MountScript,
    default_mkdir: MkdirScript,
    ids: HashMap<String, u64>,
    next_id: u64,
}

#[derive(Default, Clone)]
struct FakeMounter {
    inner: Arc<Mutex<Inner>>,
}

impl FakeMounter {
    fn with_mount_script(script: Vec<MountScript>) -> Self {
        let fake = Self::default();
        fake.inner.lock().unwrap().mount_script = script.into();
        fake
    }

    fn with_mkdir_script(script: Vec<MkdirScript>) -> Self {
        let fake = Self::default();
        fake.inner.lock().unwrap().mkdir_script = script.into();
        fake
    }

    /// Every mount call fails immediately (the oracle retries these for the
    /// full 10s window), starting with the optional scripted prefix.
    fn always_fail(text: impl Into<String>) -> Self {
        let fake = Self::default();
        fake.inner.lock().unwrap().default_mount = MountScript::Fail(text.into());
        fake
    }

    /// Succeeds exactly once (the first mount), then fails every later call —
    /// a partial-success flow whose retries never rescue the failure.
    fn ok_once_then_fail(text: impl Into<String>) -> Self {
        let fake = Self::always_fail(text);
        fake.inner.lock().unwrap().mount_script = vec![MountScript::Ok].into();
        fake
    }

    /// Fails the first `n` mount calls, then succeeds — enough scripted
    /// failures to exhaust one full immediate-retry window (21 attempts).
    fn fails_n_times_then_succeeds(n: usize, text: impl Into<String>) -> Self {
        let text = text.into();
        let fake = Self::default();
        fake.inner.lock().unwrap().mount_script = (0..n)
            .map(|_| MountScript::Fail(text.clone()))
            .collect::<Vec<_>>()
            .into();
        fake
    }

    /// Every mkdir call fails immediately.
    fn mkdir_always_fail(text: impl Into<String>) -> Self {
        let fake = Self::default();
        fake.inner.lock().unwrap().default_mkdir = MkdirScript::Fail(text.into());
        fake
    }

    fn calls(&self) -> Vec<Call> {
        self.inner.lock().unwrap().calls.clone()
    }
}

impl NfsMounter for FakeMounter {
    fn mount(
        &self,
        nfs_target: &str,
        path: &str,
        deadline: tokio::time::Instant,
    ) -> BoxFut<'_, CommandOutcome> {
        self.inner
            .lock()
            .unwrap()
            .calls
            .push(Call::Mount(mount_command(nfs_target, path)));
        let inner = self.inner.clone();
        let target = nfs_target.to_owned();
        let path = path.to_owned();
        Box::pin(async move {
            let behavior = {
                let mut guard = inner.lock().unwrap();
                match guard.mount_script.pop_front() {
                    Some(behavior) => behavior,
                    None => guard.default_mount.clone(),
                }
            };
            match behavior {
                MountScript::Ok | MountScript::Overmount(_) => {
                    let id = {
                        let mut guard = inner.lock().unwrap();
                        guard.next_id += 1;
                        let id = guard.next_id;
                        guard.ids.insert(path.clone(), id);
                        id
                    };
                    if let MountScript::Overmount(covered) = behavior {
                        inner.lock().unwrap().ids.insert(covered, id);
                    }
                    CommandOutcome::ok()
                }
                // Success reported, IDs untouched: the verification must catch it.
                MountScript::SameId => CommandOutcome::ok(),
                MountScript::Fail(text) => CommandOutcome::failure(text),
                MountScript::Hang => {
                    tokio::time::sleep_until(deadline + Duration::from_millis(1)).await;
                    CommandOutcome::failure(exec_failure_text(
                        "mount",
                        &mount_command(&target, &path),
                        "signal: killed",
                        "",
                    ))
                }
            }
        })
    }

    fn make_dir(&self, path: &str, deadline: tokio::time::Instant) -> BoxFut<'_, CommandOutcome> {
        self.inner
            .lock()
            .unwrap()
            .calls
            .push(Call::MakeDir(make_dir_command(path)));
        let inner = self.inner.clone();
        let path = path.to_owned();
        Box::pin(async move {
            let behavior = {
                let mut guard = inner.lock().unwrap();
                match guard.mkdir_script.pop_front() {
                    Some(behavior) => behavior,
                    None => guard.default_mkdir.clone(),
                }
            };
            match behavior {
                MkdirScript::Ok => CommandOutcome::ok(),
                MkdirScript::Fail(text) => CommandOutcome::failure(text),
                MkdirScript::Hang => {
                    tokio::time::sleep_until(deadline + Duration::from_millis(1)).await;
                    CommandOutcome::failure(exec_failure_text(
                        "mkdir",
                        &make_dir_command(&path),
                        "signal: killed",
                        "",
                    ))
                }
            }
        })
    }

    fn unmount(&self, path: &str) -> BoxFut<'_, Result<(), String>> {
        self.inner
            .lock()
            .unwrap()
            .calls
            .push(Call::Unmount(path.to_owned()));
        Box::pin(async move { Ok(()) })
    }

    fn mount_id(&self, path: &str) -> BoxFut<'_, Result<u64, String>> {
        self.inner
            .lock()
            .unwrap()
            .calls
            .push(Call::MountId(path.to_owned()));
        // 0 is never an ASSIGNED id (assignment starts at 1), so an unprobed
        // path always differs from the post-mount probe, like a fresh kernel
        // mount ID does.
        let id = self
            .inner
            .lock()
            .unwrap()
            .ids
            .get(path)
            .copied()
            .unwrap_or(0);
        Box::pin(async move { Ok(id) })
    }
}

fn nfs_daemon(mounter: &FakeMounter) -> axum::Router {
    app(Arc::new(
        Daemon::new().with_nfs_mounter(Arc::new(mounter.clone())),
    ))
}

/// The default successful volume flow touches the executor exactly in the
/// oracle's order: stale detach, mount point, mount, the mount-ID probes.
#[tokio::test(start_paused = true)]
async fn mount_and_mkdir_argv_are_byte_exact() {
    let fake = FakeMounter::default();
    let router = nfs_daemon(&fake);
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"10.0.0.1:/vol","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let argv = vec![
        "-v".to_owned(),
        "-t".to_owned(),
        "nfs".to_owned(),
        "-o".to_owned(),
        MOUNT_OPTS_ARG.to_owned(),
        "10.0.0.1:/vol".to_owned(),
        "/data".to_owned(),
    ];
    assert_eq!(
        fake.calls(),
        vec![
            Call::Unmount("/data".to_owned()),
            Call::MakeDir(vec!["-p".to_owned(), "/data".to_owned()]),
            Call::MountId("/data".to_owned()),
            Call::Mount(argv.clone()),
            Call::MountId("/data".to_owned()),
            Call::MountId("/data".to_owned()),
        ],
        "init.go:823-828 unmount, :911 mkdir, :917 before probe, :927 mount, \
         :968 after probe, :851 hidden re-check"
    );
    // The compile-time argv, spelled out (nfs_mount.go:33).
    assert_eq!(mount_command("10.0.0.1:/vol", "/data"), argv);
    assert_eq!(
        make_dir_command("/data"),
        vec!["-p".to_owned(), "/data".to_owned()]
    );
}

/// Absent, JSON null and [] all skip or no-op the executor and answer 204
/// (init.go:480 gates on the non-nil pointer; setupNFS over an empty slice
/// touches nothing).
#[tokio::test(start_paused = true)]
async fn absent_null_and_empty_volume_mounts_answer_204_without_touching_the_executor() {
    let fake = FakeMounter::default();
    let router = nfs_daemon(&fake);
    for body in [
        r#"{}"#,
        r#"{"volumeMounts":null}"#,
        r#"{"volumeMounts":[]}"#,
    ] {
        let response = init(&router, body).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
    }
    assert!(
        fake.calls().is_empty(),
        "no volume ever reaches the executor: {:?}",
        fake.calls()
    );
}

/// A missing nfs_target is NOT rejected: the mount runs with an empty target
/// and its failure is a mount error (finding 14).
#[tokio::test(start_paused = true)]
async fn missing_nfs_target_proceeds_to_the_mount_attempt() {
    let failure = exec_failure_text(
        "mount",
        &mount_command("", "/data"),
        "exit status 1",
        "mount.nfs: connection refused",
    );
    let fake = FakeMounter::always_fail(failure.clone());
    let router = nfs_daemon(&fake);
    let response = init(&router, r#"{"volumeMounts":[{"path":"/data"}]}"#).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()["x-envd-failed-volume"], "/data");
    let body = body_text(response).await;
    assert_eq!(
        body,
        format!(
            "failed to setup NFS volumes: NFS volume at \"/data\": after 21 attempts: {failure}"
        )
    );
    let Call::Mount(argv) = &fake.calls()[3] else {
        panic!("expected a mount call, got {:?}", fake.calls());
    };
    assert_eq!(argv[5], "", "the empty target rides the argv unchanged");
}

/// An empty path fails at `mkdir -p ""` BEFORE any mount and answers 400 with
/// an EMPTY X-Envd-Failed-Volume value (init.go:730-733 sets the header to
/// the raw path even when it is empty).
#[tokio::test(start_paused = true)]
async fn empty_path_fails_at_mkdir_with_an_empty_failed_volume_header() {
    let mkdir_failure = "`mkdir -p ` failed: exit status 1: \
mkdir: cannot create directory '': No such file or directory"
        .to_owned();
    let fake = FakeMounter::with_mkdir_script(vec![MkdirScript::Fail(mkdir_failure.clone())]);
    let router = nfs_daemon(&fake);
    let response = init(
        &router,
        r#"{"volumeMounts":[{"nfs_target":"t","path":""}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers()["x-envd-failed-volume"],
        "",
        "the header is present with an empty value"
    );
    let body = body_text(response).await;
    assert_eq!(
        body,
        format!(
            "failed to setup NFS volumes: NFS volume at \"\": create the mount point: {mkdir_failure}"
        )
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Mount(_))),
        "no mount is attempted for an empty path: {:?}",
        fake.calls()
    );
}

/// Parents mount before children by RAW depth, then again by SYMLINK-RESOLVED
/// depth (init.go:807-810, :830-838). The re-sort must change the order this
/// test builds: /s/link/deep resolves to /p/deep through the /s -> /p link.
#[tokio::test(start_paused = true)]
async fn depth_ordering_and_the_symlink_resolved_re_sort() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("p/deep")).unwrap();
    std::fs::create_dir_all(dir.path().join("s")).unwrap();
    std::os::unix::fs::symlink("../p", dir.path().join("s/link")).unwrap();
    let base = dir.path().to_str().unwrap().to_owned();
    // Request order: deep(5 raw), link(4 raw), link/q(5 raw).
    let v1 = format!("{base}/s/link"); // raw 4, resolved {base}/p (3)
    let v2 = format!("{base}/p/deep/under"); // raw 5, resolved unchanged (5)
    let v3 = format!("{base}/s/link/q"); // raw 5, resolved {base}/p/q (4)
    let body = serde_json::json!({
        "volumeMounts": [
            {"nfs_target": "t2", "path": v2},
            {"nfs_target": "t1", "path": v1},
            {"nfs_target": "t3", "path": v3},
        ]
    })
    .to_string();
    let fake = FakeMounter::default();
    let router = nfs_daemon(&fake);
    let response = init(&router, &body).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let mounts: Vec<String> = fake
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Mount(argv) => Some(argv[6].clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        mounts,
        vec![v1, v3, v2],
        "the symlink-resolved re-sort orders v1 before v3 before v2; \
         a raw-depth-only sort would answer v1, v2, v3"
    );
}

/// Every stale pending mount is detached BEFORE any new mount runs
/// (init.go:820-828).
#[tokio::test(start_paused = true)]
async fn stale_mounts_detach_before_any_new_mount() {
    let fake = FakeMounter::default();
    let daemon = Arc::new(Daemon::new().with_nfs_mounter(Arc::new(fake.clone())));
    daemon.import_mounts(&[coorworker_envd::proto::upgrade::MountEntry {
        path: "/data".to_owned(),
        lifecycle_id: "old".to_owned(),
        ..Default::default()
    }]);
    let router = app(daemon.clone());
    let response = init(
        &router,
        r#"{"lifecycleID":"new","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fake.calls().first(),
        Some(&Call::Unmount("/data".to_owned())),
        "the stale mount detaches before the mkdir and the mount"
    );
    // And the ledger now names the NEW lifecycle for the live mount.
    assert_eq!(
        daemon
            .export_mounts()
            .into_iter()
            .map(|entry| (entry.path, entry.lifecycle_id))
            .collect::<Vec<_>>(),
        vec![("/data".to_owned(), "new".to_owned())]
    );
}

/// A later mount that covers an earlier path fails the EARLIER volume as
/// hidden (init.go:849-856), removes it from the ledger, and leaves the later
/// volume's entry alone.
#[tokio::test(start_paused = true)]
async fn a_later_mount_covering_an_earlier_path_fails_as_hidden() {
    let fake = FakeMounter::with_mount_script(vec![
        MountScript::Ok,
        MountScript::Overmount("/a".to_owned()),
    ]);
    let daemon = Arc::new(Daemon::new().with_nfs_mounter(Arc::new(fake.clone())));
    let router = app(daemon.clone());
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"t1","path":"/a"},{"nfs_target":"t2","path":"/b"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()["x-envd-failed-volume"], "/a");
    let body = body_text(response).await;
    assert_eq!(
        body,
        "failed to setup NFS volumes: NFS volume at \"/a\": hidden by another volume mounted over it"
    );
    let ledger: Vec<(String, String)> = daemon
        .export_mounts()
        .into_iter()
        .map(|entry| (entry.path, entry.lifecycle_id))
        .collect();
    assert_eq!(
        ledger,
        vec![("/b".to_owned(), "l".to_owned())],
        "the covered volume leaves the ledger; the covering one stays"
    );
}

/// A mount that reports success without changing the path's mount ID fails
/// (init.go:966-974) and never enters the ledger.
#[tokio::test(start_paused = true)]
async fn a_mount_that_leaves_the_mount_id_unchanged_fails() {
    let fake = FakeMounter::with_mount_script(vec![MountScript::SameId]);
    let daemon = Arc::new(Daemon::new().with_nfs_mounter(Arc::new(fake.clone())));
    let router = app(daemon.clone());
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()["x-envd-failed-volume"], "/data");
    let body = body_text(response).await;
    assert_eq!(
        body,
        "failed to setup NFS volumes: NFS volume at \"/data\": \
         mount reported success, but the path is not on a new mount"
    );
    assert!(
        daemon.export_mounts().is_empty(),
        "an unverified mount is never recorded"
    );
}

/// A mount that TIMES OUT is retried once, after a forced unmount, and the
/// final error counts both attempts (init.go:942-953, :963).
#[tokio::test(start_paused = true)]
async fn a_timed_out_mount_retries_once_after_a_forced_unmount() {
    let fake = FakeMounter::with_mount_script(vec![MountScript::Hang, MountScript::Hang]);
    let router = nfs_daemon(&fake);
    let start = tokio::time::Instant::now();
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    // Two 10s attempts inside the 20s volume budget (init.go:779-780).
    assert!(
        start.elapsed() >= Duration::from_secs(20),
        "the volume budget ran its two 10s attempts, not {:?}",
        start.elapsed()
    );
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_text(response).await;
    let killed = exec_failure_text("mount", &mount_command("t", "/data"), "signal: killed", "");
    assert_eq!(
        body,
        format!("failed to setup NFS volumes: NFS volume at \"/data\": after 2 attempts: {killed}")
    );
    assert_eq!(
        fake.calls(),
        vec![
            Call::Unmount("/data".to_owned()),
            Call::MakeDir(vec!["-p".to_owned(), "/data".to_owned()]),
            Call::MountId("/data".to_owned()),
            Call::Mount(mount_command("t", "/data")),
            Call::Unmount("/data".to_owned()),
            Call::Mount(mount_command("t", "/data")),
        ],
        "the retry runs after a forced unmount and stops at two attempts"
    );
}

/// A mount that fails immediately is retried every 500ms until the 10s retry
/// window closes — 21 attempts at t=0.0s, 0.5s, ... 10.0s (init.go:955-960).
#[tokio::test(start_paused = true)]
async fn an_immediate_failure_retries_every_500ms_within_the_10s_window() {
    let failure = exec_failure_text(
        "mount",
        &mount_command("t", "/data"),
        "exit status 1",
        "mount.nfs: connection refused",
    );
    let fake = FakeMounter::always_fail(failure.clone());
    let router = nfs_daemon(&fake);
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let attempts = fake
        .calls()
        .into_iter()
        .filter(|call| matches!(call, Call::Mount(_)))
        .count();
    assert_eq!(attempts, 21, "t=0.0s..10.0s inclusive at 500ms steps");
    let body = body_text(response).await;
    assert_eq!(
        body,
        format!(
            "failed to setup NFS volumes: NFS volume at \"/data\": after 21 attempts: {failure}"
        )
    );
}

/// A hanging MOUNT POINT creation fails the volume at "create the mount
/// point" within its own 10s, before any mount (init.go:908-915).
#[tokio::test(start_paused = true)]
async fn a_hanging_mkdir_fails_the_volume_at_create_the_mount_point() {
    let fake = FakeMounter::with_mkdir_script(vec![MkdirScript::Hang]);
    let router = nfs_daemon(&fake);
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_text(response).await;
    let killed = exec_failure_text("mkdir", &make_dir_command("/data"), "signal: killed", "");
    assert_eq!(
        body,
        format!(
            "failed to setup NFS volumes: NFS volume at \"/data\": create the mount point: {killed}"
        )
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Mount(_))),
        "a failed mount point never reaches the mount: {:?}",
        fake.calls()
    );
}

/// Volumes mounted before a later failure stay mounted AND in the ledger
/// (finding 17: partial success persists).
#[tokio::test(start_paused = true)]
async fn partial_success_keeps_earlier_mounts_and_their_ledger_entries() {
    let failure = exec_failure_text(
        "mount",
        &mount_command("t2", "/bad"),
        "exit status 1",
        "mount.nfs: connection refused",
    );
    let fake = FakeMounter::ok_once_then_fail(failure);
    let daemon = Arc::new(Daemon::new().with_nfs_mounter(Arc::new(fake)));
    let router = app(daemon.clone());
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"t1","path":"/good"},{"nfs_target":"t2","path":"/bad"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        daemon
            .export_mounts()
            .into_iter()
            .map(|entry| (entry.path, entry.lifecycle_id))
            .collect::<Vec<_>>(),
        vec![("/good".to_owned(), "l".to_owned())],
        "the volume mounted before the failure stays mounted and recorded"
    );
}

/// The memoized failure: same lifecycle short-circuits to the SAME 400 (even
/// for `volumeMounts: []`), a new lifecycle retries, and the memo survives a
/// successful other-lifecycle setup (init.go:796-800, :876-884).
#[tokio::test(start_paused = true)]
async fn memoized_failure_short_circuits_empty_lists_until_the_lifecycle_changes() {
    let failure = exec_failure_text(
        "mount",
        &mount_command("t", "/data"),
        "exit status 1",
        "mount.nfs: connection refused",
    );
    let fake = FakeMounter::fails_n_times_then_succeeds(21, failure.clone());
    let router = nfs_daemon(&fake);
    let first = init(
        &router,
        r#"{"lifecycleID":"L1","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    let expected_body = format!(
        "failed to setup NFS volumes: NFS volume at \"/data\": after 21 attempts: {failure}"
    );
    assert_eq!(body_text(first).await, expected_body);
    let calls_after_first = fake.calls().len();

    // Same lifecycle, EMPTY list: the memo answers before anything mounts.
    let replay = init(&router, r#"{"lifecycleID":"L1","volumeMounts":[]}"#).await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_text(replay).await, expected_body, "the SAME error");
    assert_eq!(
        fake.calls().len(),
        calls_after_first,
        "the memoized replay touches no executor calls"
    );

    // A new lifecycle retries for real.
    let response = init(
        &router,
        r#"{"lifecycleID":"L2","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The L1 memo was never cleared: L1 answers the same 400 again.
    let again = init(&router, r#"{"lifecycleID":"L1","volumeMounts":[]}"#).await;
    assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_text(again).await, expected_body);
}

/// The remount truth table (init.go:992-1007): same lifecycle skips, a
/// changed or cleared lifecycle remounts.
#[tokio::test(start_paused = true)]
async fn lifecycle_truth_table_same_skips_change_and_clear_remount() {
    let fake = FakeMounter::default();
    let daemon = Arc::new(Daemon::new().with_nfs_mounter(Arc::new(fake.clone())));
    let router = app(daemon.clone());
    let mounts = r#""volumeMounts":[{"nfs_target":"t","path":"/data"}]"#;
    let mounted_calls = || {
        vec![
            Call::Unmount("/data".to_owned()),
            Call::MakeDir(vec!["-p".to_owned(), "/data".to_owned()]),
            Call::MountId("/data".to_owned()),
            Call::Mount(mount_command("t", "/data")),
            Call::MountId("/data".to_owned()),
            Call::MountId("/data".to_owned()),
        ]
    };
    // mounted="" + request="abc": mount.
    let response = init(&router, &format!(r#"{{"lifecycleID":"abc",{mounts}}}"#)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(fake.calls(), mounted_calls());
    // mounted="abc" + request="abc": skip everything.
    let response = init(&router, &format!(r#"{{"lifecycleID":"abc",{mounts}}}"#)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fake.calls(),
        mounted_calls(),
        "same lifecycle: no calls at all"
    );
    // mounted="abc" + request="xyz": remount.
    let response = init(&router, &format!(r#"{{"lifecycleID":"xyz",{mounts}}}"#)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let mut remounted = mounted_calls();
    remounted.extend_from_slice(&mounted_calls());
    assert_eq!(fake.calls(), remounted);
    // mounted="xyz" + request="": the cleared lifecycle remounts too.
    let response = init(&router, &format!(r#"{{"lifecycleID":"",{mounts}}}"#)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let mut cleared = remounted;
    cleared.extend_from_slice(&mounted_calls());
    assert_eq!(fake.calls(), cleared);
    assert_eq!(
        daemon
            .export_mounts()
            .into_iter()
            .map(|entry| (entry.path, entry.lifecycle_id))
            .collect::<Vec<_>>(),
        vec![("/data".to_owned(), String::new())]
    );
}

/// A stale/replayed /init (older timestamp) skips SetData entirely: no mount
/// attempt and no failure memoization (init.go:336-341).
#[tokio::test(start_paused = true)]
async fn a_stale_timestamp_skips_setup_nfs_entirely() {
    let fake = FakeMounter::with_mount_script(vec![
        MountScript::Ok,
        // Would fail if the stale init ran setup.
        MountScript::Fail("must not run".to_owned()),
    ]);
    let router = nfs_daemon(&fake);
    let fresh = init(
        &router,
        r#"{"timestamp":"2026-01-02T00:00:00Z","lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(fresh.status(), StatusCode::NO_CONTENT);
    let calls_after_fresh = fake.calls().len();
    let stale = init(
        &router,
        r#"{"timestamp":"2026-01-01T00:00:00Z","lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(stale.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fake.calls().len(),
        calls_after_fresh,
        "the stale /init never reached the executor"
    );
}

/// The failed /init response contract (finding 15): the 400 carries
/// X-Envd-Version, X-Envd-Memory, X-Envd-Handover, X-Envd-Init-Clock-
/// Correction and X-Envd-Failed-Volume, with a plain-text body and NO
/// X-Envd-Defaults, X-Envd-Cpus, X-Envd-Init-Unobserved or Cache-Control.
#[tokio::test(start_paused = true)]
async fn failed_init_headers_follow_the_oracle_contract() {
    let failure = exec_failure_text(
        "mount",
        &mount_command("t", "/data"),
        "exit status 1",
        "mount.nfs: connection refused",
    );
    let fake = FakeMounter::always_fail(failure.clone());
    let daemon = Arc::new(Daemon::new().with_nfs_mounter(Arc::new(fake)));
    daemon.set_handover_result(coorworker_envd::upgrade::HandoverOutcome::default());
    let router = app(daemon);
    let response = init(
        &router,
        r#"{"timestamp":"2026-01-02T00:00:00Z","lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let headers = response.headers();
    assert_eq!(headers["x-envd-version"], "0.10.2");
    assert!(headers.get("x-envd-memory").is_some());
    assert!(headers.get("x-envd-handover").is_some());
    assert!(
        headers.get("x-envd-init-clock-correction").is_some(),
        "a timestamped /init answers the correction on the error too"
    );
    assert_eq!(headers["x-envd-failed-volume"], "/data");
    for absent in [
        "x-envd-defaults",
        "x-envd-cpus",
        "x-envd-init-unobserved",
        "cache-control",
        "x-envd-thaw",
        "x-envd-freeze-audit",
    ] {
        assert!(
            headers.get(absent).is_none(),
            "{absent} is set only after the success-only thaw"
        );
    }
    assert_eq!(
        headers["content-type"], "text/plain; charset=utf-8",
        "writeInitError sets no Content-Type; the sniff fills plain text"
    );
    let body = body_text(response).await;
    assert_eq!(
        body,
        format!(
            "failed to setup NFS volumes: NFS volume at \"/data\": after 21 attempts: {failure}"
        )
    );
}

/// The handover ledger: an imported entry with an UNCHANGED lifecycle is not
/// remounted, survives the round trip, and is re-exported (finding 17).
#[tokio::test(start_paused = true)]
async fn handover_import_skips_remount_for_an_unchanged_lifecycle() {
    let fake = FakeMounter::default();
    let daemon = Arc::new(Daemon::new().with_nfs_mounter(Arc::new(fake.clone())));
    daemon.import_mounts(&[coorworker_envd::proto::upgrade::MountEntry {
        path: "/data".to_owned(),
        lifecycle_id: "l".to_owned(),
        ..Default::default()
    }]);
    let router = app(daemon.clone());
    let response = init(
        &router,
        r#"{"lifecycleID":"l","volumeMounts":[{"nfs_target":"t","path":"/data"}]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        fake.calls().is_empty(),
        "a still-live mount with the same lifecycle is left in place: {:?}",
        fake.calls()
    );
    assert_eq!(
        daemon
            .export_mounts()
            .into_iter()
            .map(|entry| (entry.path, entry.lifecycle_id))
            .collect::<Vec<_>>(),
        vec![("/data".to_owned(), "l".to_owned())]
    );

    // The ledger rides the handover blob itself.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.pb");
    coorworker_envd::upgrade::write_handover(
        &path,
        &coorworker_envd::proto::upgrade::HandoverState {
            mounts: daemon.export_mounts(),
            ..Default::default()
        },
    )
    .unwrap();
    let restored = coorworker_envd::upgrade::read_handover(&path)
        .unwrap()
        .unwrap();
    assert_eq!(restored.mounts.len(), 1);
    assert_eq!(restored.mounts[0].path, "/data");
    assert_eq!(restored.mounts[0].lifecycle_id, "l");
}

/// The freeze liveness allowlist keeps the NFSv3 locking services out of the
/// paused set (oracle hierarchy.go:14-53) — this slice must not disturb it.
#[test]
fn freeze_liveness_allowlist_keeps_the_nfs_locking_services() {
    use coorworker_envd::cgroups::hierarchy::LIVENESS_ALLOWLIST;
    for entry in [
        "init.scope",
        "system.slice/rpcbind.service",
        "system.slice/rpcbind.socket",
        "system.slice/rpc-statd.service",
    ] {
        assert!(
            LIVENESS_ALLOWLIST.contains(&entry),
            "{entry} must stay allowlisted for the post-resume mount.nfs path"
        );
    }
    assert_eq!(LIVENESS_ALLOWLIST.len(), 6);
}

/// A null nfs_target/path inside an element decodes as the zero string like
/// Go (encoding/json: null is a no-op for a string field), and a null ELEMENT
/// is the zero VolumeMount — both reach the executor and both fail at the
/// real `mkdir -p ""` (finding 14).
#[tokio::test(start_paused = true)]
async fn null_fields_and_null_elements_decode_as_zero_values() {
    let mkdir_failure = "`mkdir -p ` failed: exit status 1: \
mkdir: cannot create directory '': No such file or directory"
        .to_owned();
    let fake = FakeMounter::mkdir_always_fail(mkdir_failure.clone());
    let router = nfs_daemon(&fake);
    for body in [
        r#"{"volumeMounts":[{"nfs_target":null,"path":null}]}"#,
        r#"{"volumeMounts":[null]}"#,
    ] {
        let response = init(&router, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            response.headers()["x-envd-failed-volume"],
            "",
            "the zero volume's empty path rides the header"
        );
    }
    let zero_volume_calls = vec![
        Call::Unmount(String::new()),
        Call::MakeDir(vec!["-p".to_owned(), String::new()]),
    ];
    let mut expected = zero_volume_calls.clone();
    expected.extend(zero_volume_calls);
    assert_eq!(
        fake.calls(),
        expected,
        "each zero volume fails at mkdir -p \"\" before any mount"
    );
}
