//! Unit slice for the cgroups/freeze port, against an injected fake cgroupfs
//! tree (tempfile) driving the real path-operation code. The pinned oracle is
//! `internal/services/cgroups` at 51083a83cc0903185d992725ea6bb33683814b29;
//! every assertion below cites the oracle line it pins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use coorworker_envd::cgroups::{
    cgroup2::{
        child_cgroups, freeze_requested, frozen_from_events, set_freeze_request, settled_frozen,
        write_cgroup_prop,
    },
    freeze::{FreezeError, FreezeMode, FreezeOptions, FreezeResult, WorkloadFreezer},
    hierarchy::{allowlisted, ancestor_chain, descend_set, scan_guest_frozen, self_cgroup_path},
    noop::NoopManager,
    spawn::SpawnError,
    CgroupError, Manager, PathManager, ProcessType,
    {vanished, DEFAULT_FREEZE_MAX_CGROUPS, DEFAULT_THAW_MAX_CGROUPS},
};
use tempfile::TempDir;

const INIT_PROCFILE: &str = "0::/system.slice/envd.service\n";

/// Fake v2 manager over a real tempdir tree: the freeze machinery sees the
/// trait, the tree sees real files, and no host cgroupfs is ever touched.
struct FakeCgroupfs {
    root: PathBuf,
    paths: HashMap<ProcessType, PathBuf>,
}

impl FakeCgroupfs {
    fn new(root: &TempDir, entries: &[(&str, ProcessType)]) -> Self {
        let paths = entries
            .iter()
            .map(|(relative, process_type)| {
                let path = root.path().join(relative);
                make_cgroup(&path);
                (*process_type, path)
            })
            .collect();
        Self {
            root: root.path().to_owned(),
            paths,
        }
    }

    fn customer(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        make_cgroup(&path);
        path
    }
}

/// A real cgroupfs directory always carries its interface files; the fake
/// creates them so reads behave like the kernel's.
fn make_cgroup(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("cgroup.freeze"), "0\n").unwrap();
    std::fs::write(path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
}

impl Manager for FakeCgroupfs {
    fn get_file_descriptor(&self, _process_type: ProcessType) -> Option<i32> {
        None
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        set_freeze_request(path, true)?;
        Ok(())
    }
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        set_freeze_request(path, false)?;
        Ok(())
    }
    fn frozen(&self, process_type: ProcessType) -> Result<bool, CgroupError> {
        self.paths
            .get(&process_type)
            .map(|path| settled_frozen(path).map_err(CgroupError::from))
            .unwrap_or(Err(CgroupError::UnknownProcessType(process_type)))
    }
    fn close(&self) -> Result<(), CgroupError> {
        Ok(())
    }
    fn as_path_manager(&self) -> Option<&dyn PathManager> {
        Some(self)
    }
}

impl PathManager for FakeCgroupfs {
    fn root(&self) -> PathBuf {
        self.root.clone()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.paths.get(&process_type).cloned()
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        Ok(child_cgroups(path)?)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        set_freeze_request(path, true)?;
        Ok(())
    }
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        set_freeze_request(path, false)?;
        Ok(())
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        Ok(settled_frozen(path)?)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        Ok(freeze_requested(path)?)
    }
}

fn options(max_wait: Duration) -> FreezeOptions {
    FreezeOptions {
        max_wait,
        mode: FreezeMode::Legacy,
        max_cgroups: 0,
    }
}

/// A settled-frozen read for every written cgroup, the way a real kernel
/// settles immediately for an idle tree.
fn settle_everything(root: &TempDir) {
    let mut stack = vec![root.path().to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                std::fs::write(path.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
                stack.push(path);
            }
        }
    }
}

fn freeze_value(path: &Path) -> String {
    std::fs::read_to_string(path.join("cgroup.freeze"))
        .unwrap()
        .trim()
        .to_owned()
}

/// A freezer hermetically pointed at the fixture's membership file, so no
/// test ever reads the host's /proc/self/cgroup.
fn make_freezer(fs: Arc<FakeCgroupfs>, dir: &TempDir) -> Arc<WorkloadFreezer> {
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    Arc::new(WorkloadFreezer::new(fs).with_proc_self_cgroup(dir.path().join("proc")))
}

#[tokio::test]
async fn legacy_freeze_writes_the_static_list_and_reports_it() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs.clone(), &dir);
    let (result, error) = freezer.freeze(options(Duration::from_millis(50))).await;
    assert!(error.is_none());
    assert_eq!(result.requested, 2);
    assert_eq!(freeze_value(&dir.path().join("user")), "1");
    assert_eq!(freeze_value(&dir.path().join("ptys")), "1");
    // The socat cgroup is never swept (oracle freeze.go:20 WorkloadProcessTypes).
    assert!(!dir.path().join("socats").exists());
}

#[tokio::test]
async fn hierarchy_freeze_stops_the_complement_of_the_chain_and_the_allowlist() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[
            ("user", ProcessType::User),
            ("ptys", ProcessType::Pty),
            ("socats", ProcessType::Socat),
        ],
    ));
    // envd sits at system.slice/envd.service; the customer tree hangs off
    // system.slice next to it; the liveness allowlist holds journald.
    fs.customer("system.slice/envd.service");
    let customer = fs.customer("system.slice/customer.container");
    let allowlisted_unit = fs.customer("system.slice/systemd-journald.service");
    settle_everything(&dir);
    let proc_file = dir.path().join("proc");

    let mut opts = options(Duration::from_millis(50));
    opts.mode = FreezeMode::Hierarchy;
    let freezer = make_freezer(fs.clone(), &dir);
    let (result, error) = freezer.freeze(opts).await;
    assert!(error.is_none());
    assert_eq!(result.mode, FreezeMode::Hierarchy);
    // The chain (root, system.slice, envd.service) and the static list were
    // frozen; the customer top was frozen; journald and socats were
    // allowlisted (freeze.go:647 counts every skipped allowlist entry).
    assert_eq!(freeze_value(&customer), "1");
    assert_eq!(freeze_value(&allowlisted_unit), "0");
    assert_eq!(result.allowlisted, 2);
    assert!(result.visited >= 2, "visited={}", result.visited);
    let _ = proc_file;
}

#[tokio::test]
async fn guest_frozen_cgroups_are_counted_prefrozen_and_left_frozen_by_the_thaw() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    fs.customer("system.slice/envd.service");
    let guest_paused = fs.customer("system.slice/docker.pause");
    set_freeze_request(&guest_paused, true).unwrap();
    std::fs::write(
        guest_paused.join("cgroup.events"),
        "populated 1\nfrozen 1\n",
    )
    .unwrap();

    let mut opts = options(Duration::from_millis(50));
    opts.mode = FreezeMode::Hierarchy;
    let freezer = make_freezer(fs.clone(), &dir);
    let (result, _) = freezer.freeze(opts).await;
    assert_eq!(result.pre_frozen, 1);
    assert_eq!(freeze_value(&guest_paused), "1", "the guest's freeze stays");

    let thaw = freezer.unfreeze().await.unwrap();
    assert_eq!(thaw.preserved, 1);
    assert_eq!(freeze_value(&guest_paused), "1", "still the guest's");
    assert_eq!(freeze_value(&dir.path().join("user")), "0");
}

#[tokio::test]
async fn zero_budget_counts_every_written_cgroup_not_frozen() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    // Oracle awaitFrozen: budget <= 0 observes nothing.
    let (result, _) = freezer.freeze(options(Duration::ZERO)).await;
    assert_eq!(result.requested, 2);
    assert_eq!(result.frozen, 0);
    assert_eq!(result.not_frozen, 2);
    assert!(!result.all_frozen());
}

#[tokio::test]
async fn spawn_drain_timeout_refuses_the_freeze_with_the_workload_failed() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    // One in-flight spawn holds its weight; the drain cannot complete.
    let guard = freezer.begin_spawn(ProcessType::User, None).await.unwrap();
    let (result, error) = freezer.freeze(options(Duration::from_millis(50))).await;
    assert!(matches!(error, Some(FreezeError::SpawnDrainTimeout)));
    assert_eq!(result.failed, 2, "oracle freeze.go:608");
    assert_eq!(result.requested, 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("user/cgroup.freeze")).unwrap(),
        "0\n",
        "a refused drain freezes nothing"
    );
    drop(guard);
    // Once the fork returns the weight is back and the freeze goes through.
    let (result, error) = freezer.freeze(options(Duration::from_millis(50))).await;
    assert!(error.is_none());
    assert_eq!(result.requested, 2);
}

#[tokio::test]
async fn held_barrier_refuses_a_spawn_immediately_without_queueing() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    let (result, _) = freezer.freeze(options(Duration::ZERO)).await;
    assert!(result.requested > 0);
    let started = std::time::Instant::now();
    let error = freezer
        .begin_spawn(ProcessType::User, None)
        .await
        .unwrap_err();
    // Oracle spawn.go:188: barrier UP fails immediately, no 100ms wait.
    assert!(matches!(
        error,
        SpawnError::WorkloadFrozen | SpawnError::WorkloadFrozenPath(_)
    ));
    assert!(started.elapsed() < Duration::from_millis(50));
    // The socat spawn never takes the barrier (oracle spawn.go:66-68).
    assert!(freezer.begin_spawn(ProcessType::Socat, None).await.is_ok());
    // Thawing lowers it.
    freezer.unfreeze().await.unwrap();
    assert!(freezer.begin_spawn(ProcessType::User, None).await.is_ok());
}

#[tokio::test]
async fn probe_refuses_frozen_targets_and_ancestors_but_not_read_failures() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[
            ("system.slice/user", ProcessType::User),
            ("ptys", ProcessType::Pty),
        ],
    ));
    // The mount root carries no interface files (hierarchy.go:364-369); the
    // slice above the workload does.
    fs.customer("system.slice");
    fs.customer("system.slice/envd.service");
    let freezer = make_freezer(fs, &dir);
    let user = dir.path().join("system.slice/user");
    // A guest freeze of the target itself.
    set_freeze_request(&user, true).unwrap();
    std::fs::write(user.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
    let error = freezer
        .begin_spawn(ProcessType::User, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SpawnError::WorkloadFrozen | SpawnError::WorkloadFrozenPath(_)
    ));
    // A freeze requested on the ANCESTOR (written but not settled) also stops
    // a task arriving afterwards (oracle spawn.go:285-291).
    set_freeze_request(&user, false).unwrap();
    std::fs::write(user.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
    set_freeze_request(&dir.path().join("system.slice"), true).unwrap();
    let error = freezer
        .begin_spawn(ProcessType::User, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SpawnError::WorkloadFrozen | SpawnError::WorkloadFrozenPath(_)
    ));
    // A read failure does NOT refuse (oracle spawn.go:263-266): drop the
    // requested-freeze files and the probe passes.
    std::fs::remove_file(dir.path().join("system.slice/cgroup.freeze")).unwrap();
    assert!(freezer.begin_spawn(ProcessType::User, None).await.is_ok());
}

#[tokio::test]
async fn thaw_if_generation_refuses_a_generation_a_freeze_overtook() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    let entry_gen = freezer.freeze_generation().await;
    // A pause's freeze lands while the /init handler waits on its body.
    let (result, _) = freezer.freeze(options(Duration::ZERO)).await;
    assert_eq!(result.requested, 2);
    let outcome = freezer.thaw_if_generation(entry_gen, || false).await;
    assert!(!outcome.matched, "oracle init.go:716-720");
    assert_eq!(freeze_value(&dir.path().join("user")), "1", "freeze kept");
    let outcome = freezer.thaw_if_generation(outcome.current, || false).await;
    assert!(outcome.matched);
    assert_eq!(freeze_value(&dir.path().join("user")), "0");
}

#[tokio::test]
async fn watchdog_fires_after_the_window_and_thaws_an_unthawed_freeze() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    freezer.set_thaw_watchdog(
        Duration::from_millis(40),
        Some(Arc::new(move |result, error| {
            let _ = sender.send((result.thawed, error.is_none()));
        })),
    );
    let (result, _) = freezer.freeze(options(Duration::ZERO)).await;
    assert_eq!(result.requested, 2);
    let (thawed, clean) = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("watchdog fired")
        .unwrap();
    // The static thaw is not counted; the discovery walk finds nothing frozen
    // left to count (oracle freeze.go:1252-1330).
    assert_eq!(thawed, 0);
    assert!(clean);
    assert_eq!(freeze_value(&dir.path().join("user")), "0");
}

#[tokio::test]
async fn audit_reports_escaped_and_violated_cgroups_after_a_hierarchy_freeze() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    fs.customer("system.slice/envd.service");
    let escaped = fs.customer("system.slice/late.container");
    let allowlisted_unit = fs.customer("system.slice/systemd-journald.service");
    let mut opts = options(Duration::from_millis(50));
    opts.mode = FreezeMode::Hierarchy;
    let freezer = make_freezer(fs.clone(), &dir);
    let (result, _) = freezer.freeze(opts).await;
    assert_eq!(result.mode, FreezeMode::Hierarchy);
    // A cgroup created after the sweep: not frozen, not protected -> escaped
    // (oracle hierarchy.go:386-389). One write only freezes the subtree top,
    // so the audit reads the settled state, not the request.
    let audit = freezer.audit_frozen_set(DEFAULT_THAW_MAX_CGROUPS).await;
    let audit = audit.unwrap().unwrap();
    assert!(audit.applicable);
    assert!(audit.escaped >= 1, "escaped={}", audit.escaped);
    assert!(!audit.escaped_paths.is_empty());
    // Freeze an allowlisted cgroup behind the sweep's back: a violation, the
    // arm that catches bugs (oracle hierarchy.go:229-237).
    set_freeze_request(&allowlisted_unit, true).unwrap();
    std::fs::write(
        allowlisted_unit.join("cgroup.events"),
        "populated 1\nfrozen 1\n",
    )
    .unwrap();
    let audit = freezer.audit_frozen_set(DEFAULT_THAW_MAX_CGROUPS).await;
    let audit = audit.unwrap().unwrap();
    assert!(audit.violations >= 1, "violations={}", audit.violations);
    let _ = escaped;
}

#[tokio::test]
async fn audit_declines_after_a_legacy_sweep_and_after_a_thaw() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    let (result, _) = freezer.freeze(options(Duration::ZERO)).await;
    assert_eq!(result.mode, FreezeMode::Legacy);
    let audit = freezer.audit_frozen_set(DEFAULT_THAW_MAX_CGROUPS).await;
    let audit = audit.unwrap().unwrap();
    assert!(!audit.applicable, "oracle hierarchy.go:328-330");
    // A thaw clears the remembered sweep; a post-thaw audit declines too.
    freezer.unfreeze().await.unwrap();
    let audit = freezer.audit_frozen_set(DEFAULT_THAW_MAX_CGROUPS).await;
    let audit = audit.unwrap().unwrap();
    assert!(!audit.applicable, "oracle freeze.go:1258-1261");
}

#[test]
fn hierarchy_helpers_match_the_oracle_path_arithmetic() {
    let root = Path::new("/sys/fs/cgroup");
    let self_path = Path::new("/sys/fs/cgroup/system.slice/envd.service");
    let chain = ancestor_chain(root, self_path);
    assert_eq!(
        chain,
        vec![
            PathBuf::from("/sys/fs/cgroup"),
            PathBuf::from("/sys/fs/cgroup/system.slice"),
            PathBuf::from("/sys/fs/cgroup/system.slice/envd.service"),
        ]
    );
    // A self outside root yields just the root (oracle hierarchy.go:92-94).
    assert_eq!(
        ancestor_chain(root, Path::new("/other/place")),
        vec![root.to_path_buf()]
    );
    // The descend set reaches the ancestors of every allowlist entry.
    let descend = descend_set(root, Path::new("/sys/fs/cgroup/init.scope"));
    assert!(descend.contains(&PathBuf::from("/sys/fs/cgroup/system.slice")));
    // Entry itself is not descended into (oracle hierarchy.go:150-152).
    assert!(!descend.contains(&PathBuf::from(
        "/sys/fs/cgroup/system.slice/systemd-journald.service"
    )));
    // allowlisted matches the subtree (oracle hierarchy.go:161-167).
    assert!(allowlisted(
        root,
        Path::new("/sys/fs/cgroup/system.slice/rpcbind.socket/child")
    ));
    assert!(!allowlisted(
        root,
        Path::new("/sys/fs/cgroup/system.slice/other")
    ));
}

#[test]
fn self_cgroup_path_reads_the_zero_hierarchy_line_only() {
    let dir = TempDir::new().unwrap();
    let proc_file = dir.path().join("proc");
    std::fs::write(
        &proc_file,
        "1:name=systemd:/old\n0::/system.slice/envd.service\n",
    )
    .unwrap();
    assert_eq!(
        self_cgroup_path(&proc_file, Path::new("/sys/fs/cgroup")).unwrap(),
        PathBuf::from("/sys/fs/cgroup/system.slice/envd.service")
    );
    std::fs::write(&proc_file, "1:name=systemd:/old\n").unwrap();
    assert!(self_cgroup_path(&proc_file, Path::new("/sys/fs/cgroup")).is_err());
}

#[test]
fn scan_guest_frozen_records_only_the_guests_own_freezes() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    );
    fs.customer("system.slice/envd.service");
    let guest_frozen = fs.customer("system.slice/container");
    set_freeze_request(&guest_frozen, true).unwrap();
    // user is not on envd's chain in this layout, so a freeze of it reads as
    // the guest's own (hierarchy.go:415-419); the chain and the descend set
    // are never recorded.
    set_freeze_request(&dir.path().join("user"), true).unwrap();
    let (found, truncated, errors) =
        scan_guest_frozen(&fs, &dir.path().join("proc"), DEFAULT_THAW_MAX_CGROUPS);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!truncated);
    assert_eq!(found.len(), 2, "guest_frozen + user");
    assert!(found.contains(&guest_frozen));
    assert!(found.contains(&dir.path().join("user")));
    assert!(!found.contains(&dir.path().join("system.slice/envd.service")));
}

#[test]
fn cgroup_property_writes_never_create_and_events_parse_like_go() {
    let dir = TempDir::new().unwrap();
    // O_TRUNC without O_CREATE (oracle cgroup2.go:115-118): a missing property
    // errors instead of being silently created on a tmpfs fallback.
    let missing = dir.path().join("absent/cgroup.freeze");
    assert!(write_cgroup_prop(&missing, "1").is_err());
    assert!(!missing.exists());
    // frozenFromEvents: key/value pairs per line, absence reads not frozen
    // (oracle cgroup2.go:250-262).
    assert!(!frozen_from_events(b"populated 1\n"));
    assert!(frozen_from_events(b"populated 0\nfrozen 1\n"));
    assert!(!frozen_from_events(b"populated 0\nfrozen 0\n"));
    assert!(!frozen_from_events(b""));
}

#[test]
fn freeze_option_bounds_match_the_oracle_clamps() {
    // <=0 floors at 512 (oracle freeze.go:118-121) and any positive value is
    // clamped at the thaw cap (freeze.go:123-129).
    let mut opts = options(Duration::ZERO);
    opts.max_cgroups = 0;
    assert_eq!(opts.effective_max_cgroups(), DEFAULT_FREEZE_MAX_CGROUPS);
    opts.max_cgroups = -5;
    assert_eq!(opts.effective_max_cgroups(), DEFAULT_FREEZE_MAX_CGROUPS);
    opts.max_cgroups = 1_000_000;
    assert_eq!(opts.effective_max_cgroups(), DEFAULT_THAW_MAX_CGROUPS);
    opts.max_cgroups = 10;
    assert_eq!(opts.effective_max_cgroups(), 10);
}

/// Delegates everything to the inner manager except that every FREEZE WRITE
/// fails, the way a read-only cgroupfs would. Reads stay honest so the walks
/// still see the tree.
struct FailAllFreezes {
    inner: Arc<FakeCgroupfs>,
}

fn refused() -> CgroupError {
    CgroupError::Io(std::io::Error::other("cgroupfs is read-only"))
}

impl Manager for FailAllFreezes {
    fn get_file_descriptor(&self, process_type: ProcessType) -> Option<i32> {
        self.inner.get_file_descriptor(process_type)
    }
    fn freeze(&self, _process_type: ProcessType) -> Result<(), CgroupError> {
        Err(refused())
    }
    fn unfreeze(&self, _process_type: ProcessType) -> Result<(), CgroupError> {
        Err(refused())
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

impl PathManager for FailAllFreezes {
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        self.inner.children_of(path)
    }
    fn freeze_at(&self, _path: &Path) -> Result<(), CgroupError> {
        Err(refused())
    }
    fn unfreeze_at(&self, _path: &Path) -> Result<(), CgroupError> {
        Err(refused())
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.freeze_requested_at(path)
    }
}

/// A manager whose PTY cgroup the guest removed: writes to it fail as ENOENT,
/// everything else is honest. Oracle spawn_test.go ptysRemoved.
struct PtysRemoved {
    inner: Arc<FakeCgroupfs>,
}

fn gone() -> CgroupError {
    CgroupError::Io(std::io::Error::from_raw_os_error(2))
}

impl Manager for PtysRemoved {
    fn get_file_descriptor(&self, process_type: ProcessType) -> Option<i32> {
        self.inner.get_file_descriptor(process_type)
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        match process_type {
            ProcessType::Pty => Err(gone()),
            other => self.inner.freeze(other),
        }
    }
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        match process_type {
            ProcessType::Pty => Err(gone()),
            other => self.inner.unfreeze(other),
        }
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

impl PathManager for PtysRemoved {
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        self.inner.children_of(path)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.freeze_at(path)
    }
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.unfreeze_at(path)
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.freeze_requested_at(path)
    }
}

#[tokio::test]
async fn truncation_cannot_starve_the_static_list() {
    // Oracle hierarchy_test.go:1551: cgroups named to sort before every static
    // one exhaust the bound, and the static list is STILL frozen — it is
    // written first and unconditionally, so the bound cannot aim at it.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    fs.customer("system.slice");
    fs.customer("system.slice/envd.service");
    for index in 0..8 {
        fs.customer(&format!("system.slice/aaa{index:03}"));
    }
    let mut opts = options(Duration::ZERO);
    opts.mode = FreezeMode::Hierarchy;
    opts.max_cgroups = 4;
    let freezer = make_freezer(fs.clone(), &dir);
    let (result, _) = freezer.freeze(opts).await;
    assert!(result.truncated, "the bound must actually bite");
    assert_eq!(result.mode, FreezeMode::Hierarchy);
    assert_eq!(freeze_value(&dir.path().join("user")), "1");
    assert_eq!(freeze_value(&dir.path().join("ptys")), "1");
    // The static list is the thaw's evidence that a freeze of ours is in
    // effect; the discovering thaw proves the evidence was read.
    let thaw = freezer.unfreeze().await.unwrap();
    assert!(thaw.discovered);
}

#[tokio::test]
async fn static_cgroups_are_counted_once_across_the_pass_and_the_walk() {
    // Oracle hierarchy_test.go:1589: the walk skips what the static pass
    // already requested; a second write would succeed silently and inflate
    // Requested and the confirmation set.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[
            ("user", ProcessType::User),
            ("ptys", ProcessType::Pty),
            ("socats", ProcessType::Socat),
        ],
    ));
    fs.customer("system.slice");
    fs.customer("system.slice/envd.service");
    fs.customer("system.slice/customer");
    settle_everything(&dir);
    let mut opts = options(Duration::from_millis(500));
    opts.mode = FreezeMode::Hierarchy;
    let freezer = make_freezer(fs, &dir);
    let (result, error) = freezer.freeze(opts).await;
    assert!(error.is_none());
    // user, pty (the static pass) and customer (the walk). The fixture plants
    // no other freezable cgroup, so this is exact rather than a floor — and
    // the point of the test is that the static pass and the walk TOGETHER
    // still write the static cgroups once each (oracle hierarchy_test.go:1596).
    assert_eq!(result.requested, 3);
    assert_eq!(result.frozen, 3);
    assert_eq!(
        result.allowlisted, 1,
        "socats was skipped, not double-counted"
    );
}

#[tokio::test]
async fn a_sweep_that_wrote_nothing_does_not_close_an_open_window() {
    // Oracle hierarchy_test.go:1759: the flag also stops the NEXT sweep inside
    // the same frozen window from rescanning and adopting our own cgroups as
    // the guest's — a later sweep that writes nothing must not clear it.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    fs.customer("system.slice");
    fs.customer("system.slice/envd.service");
    let customer = fs.customer("system.slice/customer");
    let mut opts = options(Duration::ZERO);
    opts.mode = FreezeMode::Hierarchy;
    let freezer = make_freezer(fs.clone(), &dir);
    let (first, error) = freezer.freeze(opts.clone()).await;
    assert!(error.is_none());
    assert!(first.requested > 0);
    // Settle the first sweep's writes so the SECOND freezer below reads the
    // freeze out of the TREE (freezeInEffect's crash-restart evidence), not
    // out of memory it does not have: that is exactly the case the flag-vs-tree
    // design exists for.
    for cgroup in ["user", "ptys"] {
        std::fs::write(
            dir.path().join(cgroup).join("cgroup.events"),
            "populated 1\nfrozen 1\n",
        )
        .unwrap();
    }

    // A second sweep whose writes all fail: it wrote nothing, but the first
    // sweep's cgroups are still frozen, so the window is still open.
    let failing: Arc<dyn Manager> = Arc::new(FailAllFreezes { inner: fs.clone() });
    let freezer =
        Arc::new(WorkloadFreezer::new(failing).with_proc_self_cgroup(dir.path().join("proc")));
    let (second, error) = freezer.freeze(opts).await;
    assert!(
        error.is_some(),
        "the writes were refused, which is reported"
    );
    assert_eq!(second.requested, 0, "the second sweep wrote nothing");
    assert_eq!(freeze_value(&customer), "1", "the window is still open");
    // And the thaw still discovers, which is what the open window buys — swap
    // the honest manager back in first.
    let freezer = make_freezer(fs, &dir);
    let thaw = freezer.unfreeze().await.unwrap();
    assert!(thaw.discovered);
    assert_eq!(
        freeze_value(&customer),
        "0",
        "the first sweep's freeze is released"
    );
}

#[tokio::test]
async fn vanished_static_cgroup_does_not_hold_the_barrier() {
    // Oracle spawn_test.go:443: a static cgroup that no longer exists holds no
    // frozen task and takes no spawn, so it must not keep spawns refused —
    // every later thaw fails on it the same way, and counting it as still
    // frozen would refuse starts into the cgroup that DID thaw for the life
    // of the sandbox.
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = Arc::new(
        WorkloadFreezer::new(Arc::new(PtysRemoved { inner: fs.clone() }))
            .with_proc_self_cgroup(dir.path().join("proc")),
    );
    let (result, error) = freezer.freeze(options(Duration::ZERO)).await;
    assert!(error.is_some(), "the pty freeze failed");
    assert_eq!(result.requested, 1, "the user freeze opened a window");
    assert!(freezer.begin_spawn(ProcessType::User, None).await.is_err());
    assert!(
        freezer.unfreeze().await.is_err(),
        "the pty thaw fails as well"
    );
    let guard = freezer.begin_spawn(ProcessType::User, None).await.unwrap();
    drop(guard);
}

// --- vanish accounting (oracle vanish_test.go) ---

#[test]
fn vanished_matches_only_the_removal_errnos() {
    // Oracle vanish_test.go:45: exactly the two errnos a removal produces, and
    // nothing else — a wide predicate would swallow the sweep's Failed count.
    assert!(vanished(&CgroupError::Io(
        std::io::Error::from_raw_os_error(2)
    )));
    assert!(vanished(&CgroupError::Io(
        std::io::Error::from_raw_os_error(19)
    )));
    assert!(!vanished(&CgroupError::Io(
        std::io::Error::from_raw_os_error(13)
    )));
    assert!(!vanished(&CgroupError::Io(std::io::Error::other(
        "refused"
    ))));
    assert!(!vanished(&CgroupError::FrozenUnobservable));
    assert!(!vanished(&CgroupError::UnknownProcessType(
        ProcessType::System
    )));
}

/// Appends a phantom child to every ChildrenOf listing: a cgroup the walk can
/// see but that is gone from the tree before anything can be written to it —
/// the shape of a transient unit finishing mid-pause.
struct GhostChild {
    inner: Arc<FakeCgroupfs>,
    /// Make the pre-write probe report the ghost unfrozen, so the walk reaches
    /// FreezeAt and vanishes there instead (the second of the two arms,
    /// oracle vanish_test.go:533).
    probe_lies: bool,
}

impl GhostChild {
    fn ghost(&self) -> PathBuf {
        self.inner.root().join("ghost")
    }
}

impl Manager for GhostChild {
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

impl PathManager for GhostChild {
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        let mut children = self.inner.children_of(path)?;
        if path == self.inner.root() {
            children.push(self.ghost());
        }
        Ok(children)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.freeze_at(path)
    }
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.unfreeze_at(path)
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        if self.probe_lies && path == self.ghost() {
            return Ok(false);
        }
        self.inner.freeze_requested_at(path)
    }
}

async fn ghost_freeze_harness(probe_lies: bool) -> (Arc<WorkloadFreezer>, TempDir, FreezeResult) {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let manager: Arc<dyn Manager> = Arc::new(GhostChild {
        inner: fs.clone(),
        probe_lies,
    });
    let freezer =
        Arc::new(WorkloadFreezer::new(manager).with_proc_self_cgroup(dir.path().join("proc")));
    let mut opts = options(Duration::ZERO);
    opts.mode = FreezeMode::Hierarchy;
    let (result, error) = freezer.freeze(opts).await;
    assert!(error.is_none(), "{error:?}");
    (freezer, dir, result)
}

#[tokio::test]
async fn a_walk_cgroup_vanished_before_the_probe_is_not_requested() {
    // Oracle vanish_test.go:253/:533: the late-freeze probe discovers the path
    // is gone, so the write below it is skipped — and the cgroup was never
    // Requested, because the write never landed.
    let (_, dir, result) = ghost_freeze_harness(false).await;
    assert_eq!(result.requested, 2, "only the static list was written");
    assert_eq!(result.vanished, 1);
    assert!(
        result
            .vanished_paths
            .contains(&dir.path().join("ghost").display().to_string()),
        "{:?}",
        result.vanished_paths
    );
    assert_eq!(
        result.scan_failed, 0,
        "a vanished cgroup is not a scan failure"
    );
    assert!(!result.truncated);
}

#[tokio::test]
async fn a_walk_cgroup_vanished_between_the_probe_and_the_write_is_not_requested() {
    // The other arm: the probe read the ghost as present-and-unfrozen (a lie
    // here), and the write itself hits the removal. Same accounting.
    let (_, _, result) = ghost_freeze_harness(true).await;
    assert_eq!(result.requested, 2);
    assert_eq!(result.vanished, 1);
    assert_eq!(result.failed, 0);
}

#[tokio::test]
async fn a_scan_over_a_vanishing_tree_is_not_a_scan_failure() {
    // Oracle vanish_test.go:308/:333: the scan tolerates a vanish outright —
    // a cgroup that no longer exists froze nothing the thaw has to preserve,
    // and inflating ScanFailed would warn about cgroups nobody can act on.
    let (_, _, result) = ghost_freeze_harness(false).await;
    assert_eq!(result.scan_failed, 0);
}

/// Deletes a cgroup right before its first settled read, so the walk's write
/// succeeded but the confirmation poll reads a removal (vanish_test.go:227).
struct DeleteBeforeSettledRead {
    inner: Arc<FakeCgroupfs>,
    target: PathBuf,
}

impl Manager for DeleteBeforeSettledRead {
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

impl PathManager for DeleteBeforeSettledRead {
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        self.inner.children_of(path)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.freeze_at(path)
    }
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.unfreeze_at(path)
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        if path == self.target {
            std::fs::remove_dir_all(path).unwrap();
        }
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.freeze_requested_at(path)
    }
}

#[tokio::test]
async fn a_poll_vanish_is_counted_and_does_not_hold_back_all_frozen() {
    // Oracle vanish_test.go:227/:348: a cgroup the WALK wrote that goes away
    // before its settled read is counted Vanished with its path sampled —
    // dropped from the poll set, so it cannot reach NotFrozen by subtraction,
    // and AllFrozen ignores it (freeze.go:497-500).
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let victim = fs.customer("victim");
    for cgroup in ["user", "ptys"] {
        std::fs::write(
            dir.path().join(cgroup).join("cgroup.events"),
            "populated 1\nfrozen 1\n",
        )
        .unwrap();
    }
    let manager: Arc<dyn Manager> = Arc::new(DeleteBeforeSettledRead {
        inner: fs.clone(),
        target: victim.clone(),
    });
    let freezer =
        Arc::new(WorkloadFreezer::new(manager).with_proc_self_cgroup(dir.path().join("proc")));
    let mut opts = options(Duration::from_millis(500));
    opts.mode = FreezeMode::Hierarchy;
    let (result, error) = freezer.freeze(opts).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        result.requested, 3,
        "user, ptys and the victim were written"
    );
    assert_eq!(result.frozen, 2);
    assert_eq!(result.vanished, 1);
    assert_eq!(result.failed, 0);
    assert_eq!(
        result.not_frozen, 0,
        "a counted vanish cannot reach NotFrozen by subtraction"
    );
    assert!(
        result
            .vanished_paths
            .contains(&victim.display().to_string()),
        "{:?}",
        result.vanished_paths
    );
    assert!(
        result.all_frozen(),
        "AllFrozen deliberately ignores Vanished (freeze.go:497-500)"
    );
    assert!(
        !std::fs::exists(&victim).unwrap(),
        "the fixture really removed it"
    );
}

// --- adopt-late-guest-freezes (oracle hierarchy_test.go:1636, :1665) ---

/// The scan reads the target unfrozen; every later read sees the guest's
/// freeze, which lands between the scan and the sweep's write (the Go
/// freezeAfterScan fixture, keyed on read count rather than a clock).
struct FreezeAfterScan {
    inner: Arc<FakeCgroupfs>,
    target: PathBuf,
    reads: std::sync::atomic::AtomicI32,
}

impl FreezeAfterScan {
    fn freeze_target_as_the_guest(&self) {
        std::fs::write(self.target.join("cgroup.freeze"), "1\n").unwrap();
    }
}

impl Manager for FreezeAfterScan {
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

impl PathManager for FreezeAfterScan {
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        self.inner.children_of(path)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.freeze_at(path)
    }
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.unfreeze_at(path)
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        if path == self.target {
            match self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                // First read is the scan's: not yet frozen.
                0 => return Ok(false),
                // Second is the sweep's probe: the guest froze it meanwhile.
                1 => {
                    self.freeze_target_as_the_guest();
                    return Ok(true);
                }
                _ => {}
            }
        }
        self.inner.freeze_requested_at(path)
    }
}

#[tokio::test]
async fn a_guest_freeze_between_the_scan_and_the_write_is_adopted() {
    // Oracle hierarchy_test.go:1636: whatever is frozen at the probe was
    // frozen by someone else — a sweep never reads back its own writes — so
    // it is adopted into the record and left to the guest. Without the
    // adoption the thaw would clear it.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let late = fs.customer("late");
    let manager: Arc<dyn Manager> = Arc::new(FreezeAfterScan {
        inner: fs.clone(),
        target: late.clone(),
        reads: std::sync::atomic::AtomicI32::new(0),
    });
    let freezer =
        Arc::new(WorkloadFreezer::new(manager).with_proc_self_cgroup(dir.path().join("proc")));
    let mut opts = options(Duration::ZERO);
    opts.mode = FreezeMode::Hierarchy;
    let (result, error) = freezer.freeze(opts).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        result.requested, 2,
        "the late cgroup was never written by us"
    );
    assert_eq!(result.pre_frozen, 1, "reported as one we leave alone");
    assert_eq!(freeze_value(&late), "1");
    let thaw = freezer.unfreeze().await.unwrap();
    assert!(thaw.preserved >= 1, "the resume must not clear it");
    assert_eq!(freeze_value(&late), "1", "still the guest's");
    assert_eq!(
        freeze_value(&dir.path().join("user")),
        "0",
        "ours is thawed"
    );
}

#[tokio::test]
async fn a_second_sweep_inside_the_window_does_not_adopt_our_own_freeze() {
    // Oracle hierarchy_test.go:1665: the second freeze inside the same frozen
    // window takes no new scan (freezeInEffect is true), so its probe must
    // never run: there, "frozen and not in the record" describes OUR cgroups
    // exactly as well as the guest's, and adopting them would hand the
    // workload to the guest and leave every later thaw preserving it.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    fs.customer("system.slice");
    fs.customer("system.slice/envd.service");
    let customer = fs.customer("system.slice/customer");
    let mut opts = options(Duration::ZERO);
    opts.mode = FreezeMode::Hierarchy;
    let freezer = make_freezer(fs, &dir);
    let (first, error) = freezer.freeze(opts.clone()).await;
    assert!(error.is_none());
    assert_eq!(first.pre_frozen, 0, "nothing was the guest's yet");

    // Second freeze in the same window: the walk re-reads customer as frozen —
    // by the FIRST sweep — and an adoption here would record it as the guest's.
    let (second, error) = freezer.freeze(opts).await;
    assert!(error.is_none());
    assert_eq!(
        second.pre_frozen, 0,
        "our own freeze must never be adopted as the guest's"
    );
    let thaw = freezer.unfreeze().await.unwrap();
    assert_eq!(thaw.preserved, 0);
    assert_eq!(freeze_value(&customer), "0", "the workload is released");
}

// --- watchdog rescue and supersession (oracle hierarchy_test.go:1343, :652) ---

/// Every unfreeze write is refused — the shape of a cgroupfs that takes the
/// freeze but no longer takes the thaw.
struct ThawRefuser {
    inner: Arc<FakeCgroupfs>,
}

impl Manager for ThawRefuser {
    fn get_file_descriptor(&self, process_type: ProcessType) -> Option<i32> {
        self.inner.get_file_descriptor(process_type)
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        self.inner.freeze(process_type)
    }
    fn unfreeze(&self, _process_type: ProcessType) -> Result<(), CgroupError> {
        Err(refused())
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

impl PathManager for ThawRefuser {
    fn root(&self) -> PathBuf {
        self.inner.root()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.inner.path_of(process_type)
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        self.inner.children_of(path)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        self.inner.freeze_at(path)
    }
    fn unfreeze_at(&self, _path: &Path) -> Result<(), CgroupError> {
        Err(refused())
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.frozen_at(path)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        self.inner.freeze_requested_at(path)
    }
}

#[tokio::test]
async fn a_dirty_watchdog_rescue_rearms_and_fires_again() {
    // Oracle hierarchy_test.go:1343: a rescue that did not fully succeed must
    // arm the backstop again — nothing else will, and without this the
    // backstop is single-shot: one transient write error during the rescue
    // leaves a live sandbox with a stopped workload and no further retry.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let manager: Arc<dyn Manager> = Arc::new(ThawRefuser { inner: fs.clone() });
    let freezer =
        Arc::new(WorkloadFreezer::new(manager).with_proc_self_cgroup(dir.path().join("proc")));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    freezer.set_thaw_watchdog(
        Duration::from_millis(30),
        Some(Arc::new(move |result, error| {
            let _ = sender.send((result.failed, error.is_some()));
        })),
    );
    let (result, _) = freezer.freeze(options(Duration::ZERO)).await;
    assert_eq!(result.requested, 2);
    for expected_fire in 0..2 {
        let (failed, errored) = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap_or_else(|_| panic!("dirty rescue fire #{expected_fire} never came"))
            .unwrap();
        assert!(failed > 0 || errored, "fire #{expected_fire} must be dirty");
    }
    assert_eq!(
        freeze_value(&dir.path().join("user")),
        "1",
        "the refused thaws never cleared anything"
    );
}

#[tokio::test]
async fn a_superseded_watchdog_fire_is_a_no_op() {
    // Oracle hierarchy_test.go:652: a fired-but-not-yet-run watchdog from one
    // freeze must not thaw the next freeze's cgroups. The staleness check
    // happens with the freeze lock held; here the fired callback is parked
    // behind the lock while a real unfreeze disarms and bumps the generation.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    freezer.set_thaw_watchdog(
        Duration::from_millis(30),
        Some(Arc::new(move |_, _| {
            sender.send(()).unwrap();
        })),
    );
    let (result, _) = freezer.freeze(options(Duration::ZERO)).await;
    assert_eq!(result.requested, 2);
    assert_eq!(freeze_value(&dir.path().join("user")), "1");

    // Park the dispatched callback behind the freeze lock, with a real
    // unfreeze queued AHEAD of it (tokio semaphores are FIFO).
    let permit = freezer.hold_freeze_lock_for_tests().await;
    let thawing = {
        let freezer = freezer.clone();
        tokio::spawn(async move { freezer.unfreeze().await })
    };
    tokio::time::sleep(Duration::from_millis(60)).await;
    drop(permit);
    thawing.await.unwrap().unwrap();
    assert_eq!(
        freeze_value(&dir.path().join("user")),
        "0",
        "the real thaw ran"
    );
    // The parked callback then finds its generation superseded: no thaw, no
    // report — and specifically no second thaw of whatever stands frozen.
    tokio::time::timeout(Duration::from_millis(150), receiver.recv())
        .await
        .expect_err("a superseded fire must never be reported");
}

// --- unobservable (oracle freeze_test.go:136) ---

#[tokio::test]
async fn unobservable_is_neither_awaited_nor_counted_against_all_frozen() {
    // Oracle freeze_test.go:136 and FreezeResult.AllFrozen (freeze.go:552):
    // a guest with no manager never had the frozen guarantee, so its writes
    // are counted Unobservable — dropped from the poll set at once — and do
    // not hold AllFrozen back.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("proc"), INIT_PROCFILE).unwrap();
    let freezer = Arc::new(
        WorkloadFreezer::new(Arc::new(NoopManager)).with_proc_self_cgroup(dir.path().join("proc")),
    );
    let started = std::time::Instant::now();
    let (result, error) = freezer.freeze(options(Duration::from_secs(30))).await;
    assert!(error.is_none());
    assert_eq!(result.requested, 2);
    assert_eq!(result.unobservable, 2);
    assert_eq!(result.frozen, 0);
    assert_eq!(result.not_frozen, 0);
    assert_eq!(result.failed, 0);
    assert!(result.all_frozen());
    // The unobservables were dropped from the poll set at once, so the wait
    // burned none of the 30 s budget (freeze.go:1135-1138).
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "unobservable cgroups must not be awaited"
    );
}

// --- the live-upgrade hold (oracle process/upgrade.go:585-605) ---

use coorworker_envd::cgroups::freeze::FreezeHoldError;

/// The hold succeeds only over a fully-stopped workload, and the shared freeze
/// lock stays held for as long as the guard lives — a concurrent thaw (or
/// freeze) blocks until release (process/upgrade.go:586-588).
#[tokio::test]
async fn freeze_workload_hold_keeps_the_lock_over_a_stopped_workload() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    // The kernel-simulated tree settles at once, the way an idle guest's
    // cgroups do (the harness's settled_frozen reads cgroup.events).
    settle_everything(&dir);
    let hold = freezer
        .freeze_workload_hold(Duration::from_millis(200))
        .await
        .expect("an idle tree settles frozen");
    assert_eq!(freeze_value(&dir.path().join("user")), "1");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), async {
            let _permit = freezer.freeze_hold().await;
        },)
        .await
        .is_err(),
        "the hold must keep the freeze lock"
    );
    drop(hold);
    tokio::time::timeout(Duration::from_millis(200), async {
        let _permit = freezer.freeze_hold().await;
    })
    .await
    .expect("lock released when the guard drops");
}

/// Oracle process/upgrade.go:592-601: a workload that did not stop refuses the
/// handover with ErrWorkloadNotFrozen, the sweep counts riding the error for
/// the caller's warning, and the lock RELEASED so the rollback thaw cannot
/// self-deadlock (main.go:377-383).
#[tokio::test]
async fn freeze_workload_hold_refuses_a_workload_that_did_not_stop() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    // Zero budget: nothing is awaited, so every written cgroup reads back
    // not-frozen (oracle awaitFrozen: budget <= 0 returns at once).
    let error = freezer
        .freeze_workload_hold(Duration::ZERO)
        .await
        .expect_err("nothing settled frozen");
    match error {
        FreezeHoldError::NotFrozen { result, .. } => {
            assert_eq!(result.requested, 2);
            assert_eq!(result.not_frozen, 2);
        }
        other => panic!("expected the strict refusal, got {other:?}"),
    }
    tokio::time::timeout(Duration::from_millis(200), async {
        let _permit = freezer.freeze_hold().await;
    })
    .await
    .expect("lock released on refusal");
}

/// Oracle main.go:387 + process/upgrade.go:572-574: a refused drain surfaces
/// pre-wrapped the way doUpgrade displays it, freezes nothing, and releases
/// the lock.
#[tokio::test]
async fn freeze_workload_hold_wraps_the_drain_refusal_like_the_handover_abort() {
    let dir = TempDir::new().unwrap();
    let fs = Arc::new(FakeCgroupfs::new(
        &dir,
        &[("user", ProcessType::User), ("ptys", ProcessType::Pty)],
    ));
    let freezer = make_freezer(fs, &dir);
    // One in-flight spawn holds its weight; the drain cannot complete.
    let guard = freezer.begin_spawn(ProcessType::User, None).await.unwrap();
    let error = freezer
        .freeze_workload_hold(Duration::from_millis(50))
        .await
        .expect_err("the drain refuses the hold");
    match &error {
        FreezeHoldError::Freeze(FreezeError::SpawnDrainTimeout) => {}
        other => panic!("expected the drain timeout, got {other:?}"),
    }
    assert!(
        error
            .to_string()
            .starts_with("handover aborted: freeze workload: timed out"),
        "display must match doUpgrade's wrap (main.go:387): {error}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("user/cgroup.freeze")).unwrap(),
        "0\n",
        "a refused drain freezes nothing"
    );
    drop(guard);
    tokio::time::timeout(Duration::from_millis(200), async {
        let _permit = freezer.freeze_hold().await;
    })
    .await
    .expect("lock released on refusal");
}
