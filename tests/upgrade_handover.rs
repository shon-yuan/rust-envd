//! The live-upgrade/handover slice against fakes and temp dirs. Oracle:
//! packages/envd at 51083a83cc0903185d992725ea6bb33683814b29 —
//! internal/services/process/upgrade.go, internal/services/cgroups/freeze.go
//! (ResumeFrozen/Thawed/GuestFrozenPaths), spec/upgrade/handover.proto via the
//! generated `proto::upgrade` types, internal/port/forward.go (ExportForwardsHold)
//! and internal/api (mounts ledger, X-Envd-Handover). No host cgroupfs is ever
//! touched: the freezer runs on the injected fake tree, the blob lands in a
//! tempfile, and process starts use real /bin/sleep children.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use buffa::Message;
use coorworker_envd::cgroups::{
    cgroup2::settled_frozen,
    freeze::{FreezeMode, FreezeOptions, WorkloadFreezer},
    CgroupError, Manager, PathManager, ProcessType,
};
use coorworker_envd::exec_context::Defaults;
use coorworker_envd::proto::upgrade as wire;
use tempfile::TempDir;

const INIT_PROCFILE: &str = "0::/system.slice/envd.service\n";

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

fn make_cgroup(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    set_freeze_state(path, false);
}

/// The fake kernel: a freeze request settles immediately for an idle tree —
/// the write flips both the request and the frozen flag the events file
/// reports, the way real cgroupfs does once the tasks stop.
fn set_freeze_state(path: &Path, frozen: bool) {
    std::fs::write(
        path.join("cgroup.freeze"),
        if frozen { "1\n" } else { "0\n" },
    )
    .unwrap();
    std::fs::write(
        path.join("cgroup.events"),
        if frozen {
            "populated 1\nfrozen 1\n"
        } else {
            "populated 1\nfrozen 0\n"
        },
    )
    .unwrap();
}

impl Manager for FakeCgroupfs {
    fn get_file_descriptor(&self, _process_type: ProcessType) -> Option<i32> {
        None
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        set_freeze_state(path, true);
        Ok(())
    }
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        set_freeze_state(path, false);
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
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.path());
            }
        }
        Ok(out)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        set_freeze_state(path, true);
        Ok(())
    }
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        set_freeze_state(path, false);
        Ok(())
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        Ok(settled_frozen(path)?)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        Ok(coorworker_envd::cgroups::cgroup2::freeze_requested(path)?)
    }
}

fn hierarchy_freezer(fake: &FakeCgroupfs, membership: &str, dir: &TempDir) -> Arc<WorkloadFreezer> {
    let procfile = dir.path().join("proc_self_cgroup");
    std::fs::write(&procfile, membership).unwrap();
    Arc::new(
        WorkloadFreezer::new(Arc::new(FakeCgroupfs {
            root: fake.root.clone(),
            paths: fake.paths.clone(),
        }))
        .with_proc_self_cgroup(procfile),
    )
}

fn freeze_options(max_wait: Duration) -> FreezeOptions {
    FreezeOptions {
        max_wait,
        mode: FreezeMode::Hierarchy,
        max_cgroups: 0,
    }
}

/// Oracle upgrade.go:542-570 restoreTerminated via the Go service; the Rust
/// home is the process runtime's retention cache.
mod terminated {
    use super::*;
    use coorworker_envd::process::{ProcessRuntime, RuntimeError};
    use coorworker_envd::proto::process as pb;

    fn runtime() -> ProcessRuntime {
        // The current ordinary user (harness rule: tests only spawn
        // ordinary-user children) and an existing workdir, since the spawn
        // resolves the command's cwd through the exec context.
        use nix::unistd::{getegid, geteuid, getgroups};
        ProcessRuntime::new(
            coorworker_envd::exec_context::Account {
                name: "runtime-handover".into(),
                home: "/tmp".into(),
                uid: geteuid().as_raw(),
                gid: getegid().as_raw(),
                groups: getgroups()
                    .unwrap()
                    .into_iter()
                    .map(|group| group.as_raw())
                    .collect(),
            },
            Defaults {
                workdir: Some("/tmp".into()),
                ..Default::default()
            },
            coorworker_envd::exec_context::Environment::default(),
            None,
        )
        .unwrap()
    }

    /// Oracle upgrade.go:542-570: a restored retained exit is served to a
    /// Connect on its pid, and a PID that is LIVE again is skipped (a stale
    /// exit under a live PID would be served to the wrong process).
    #[tokio::test]
    async fn restore_serves_a_retained_exit_and_skips_a_live_pid() {
        let runtime = runtime();
        // A live child occupying pid P.
        let mut started = runtime
            .start(start_request("sleep-restore", Some("live-tag")), None)
            .await
            .unwrap();
        let live_pid = start_pid(&mut started.recv().await.unwrap());
        let _ = started;

        let end = coorworker_envd::proto::process::process_event::EndEvent {
            exit_code: 3,
            exited: true,
            status: "exited".into(),
            ..Default::default()
        };
        let entries = vec![retained_exit(live_pid, Some("stale"), end.clone(), 30_000)];
        runtime.restore_retained(entries).await;

        // The live pid keeps its live entry: connect returns the live stream,
        // and the stale exit must not be served for it. Observable through a
        // later export: the live pid carries no retained entry.
        let exported = runtime.export_retained().await;
        assert!(
            exported.iter().all(|entry| entry.pid != live_pid),
            "a retained entry under a live pid must be skipped: {exported:?}"
        );

        // A dead pid's exit is restored and served on Connect.
        let restored = live_pid + 1_000_000;
        runtime
            .restore_retained(vec![retained_exit(
                restored as u32,
                Some("done"),
                end.clone(),
                30_000,
            )])
            .await;
        let mut subscription = runtime
            .connect(&selector(
                coorworker_envd::proto::process::process_selector::Selector::Pid(restored as u32),
            ))
            .await
            .unwrap();
        let start = subscription.recv().await.unwrap();
        let event = subscription.recv().await.unwrap();
        assert_eq!(start_pid(&mut start.clone()), restored as u32);
        match event.event {
            Some(coorworker_envd::proto::process::process_event::Event::End(end)) => {
                assert_eq!(end.exit_code, 3);
            }
            other => panic!("expected the retained end event, got {other:?}"),
        }
    }

    /// Oracle upgrade.go:254-268: the outgoing side carries only exits whose
    /// retention TTL has not run out (rem <= 0 is skipped).
    #[tokio::test]
    async fn export_carries_only_unexpired_retained_exits_with_remaining_ttl() {
        let runtime = runtime();
        let mut ended = runtime
            .start(start_request("exit", None), None)
            .await
            .unwrap();
        let pid = start_pid(&mut ended.recv().await.unwrap());
        // Wait for the real exit to land in the retention cache.
        let mut terminal = ended;
        loop {
            match terminal.recv().await.unwrap().event {
                Some(coorworker_envd::proto::process::process_event::Event::End(_)) => break,
                Some(_) => {}
                None => panic!("stream ended before the terminal event"),
            }
        }
        let exported = runtime.export_retained().await;
        assert_eq!(exported.len(), 1, "{exported:?}");
        assert_eq!(exported[0].pid, pid);
        assert!(exported[0].remaining_ms > 0, "{:?}", exported[0]);
        assert_eq!(exported[0].end.as_ref().unwrap().exit_code, 0);
    }

    fn start_request(
        cmd: &str,
        tag: Option<&str>,
    ) -> coorworker_envd::proto::process::StartRequest {
        coorworker_envd::proto::process::StartRequest {
            process: Some(coorworker_envd::proto::process::ProcessConfig {
                cmd: "/bin/sleep".into(),
                args: if cmd == "exit" {
                    vec!["0".into()]
                } else {
                    vec!["30".into()]
                },
                ..Default::default()
            })
            .into(),
            tag: tag.map(str::to_owned),
            ..Default::default()
        }
    }

    fn start_pid(event: &mut coorworker_envd::proto::process::ProcessEvent) -> u32 {
        match &event.event {
            Some(coorworker_envd::proto::process::process_event::Event::Start(start)) => start.pid,
            other => panic!("expected the start event, got {other:?}"),
        }
    }

    fn selector(
        selector: coorworker_envd::proto::process::process_selector::Selector,
    ) -> coorworker_envd::proto::process::ProcessSelector {
        coorworker_envd::proto::process::ProcessSelector {
            selector: Some(selector),
            ..Default::default()
        }
    }

    /// The wire shape the handover carries (upgrade.go:260-266) before proto
    /// encoding: pid, tag presence, the end event and the remaining TTL.
    fn retained_exit(
        pid: u32,
        tag: Option<&str>,
        end: pb::process_event::EndEvent,
        remaining_ms: i64,
    ) -> coorworker_envd::upgrade::RetainedExit {
        coorworker_envd::upgrade::RetainedExit {
            pid,
            tag: tag.map(str::to_owned),
            end: Some(end),
            remaining_ms,
        }
    }

    // Silence the unused-import warning path for RuntimeError in this module.
    #[allow(dead_code)]
    fn _runtime_error_is_imported(_: RuntimeError) {}
}

/// Oracle freeze.go:1001-1043 (GuestFrozenPaths/SetGuestFrozenPaths),
/// 1064-1087 (ResumeFrozen) and 412-428 (Thawed/signalThawed), via
/// upgrade.go:148/:502/:412 call sites.
mod freezer_resume {
    use super::*;

    /// Oracle freeze.go:1001-1025: the record is reported as root-relative,
    /// sorted, and empty without a path-capable manager.
    #[tokio::test]
    async fn guest_frozen_paths_round_trips_relative_and_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(
            &dir,
            &[("user", ProcessType::User), ("pty", ProcessType::Pty)],
        );
        let freezer = hierarchy_freezer(&fake, INIT_PROCFILE, &dir);

        // A guest cgroup frozen before our sweep is recorded by the freeze
        // scan (freeze.go:620-637) and reported relative. `customer` itself is
        // an ordinary cgroup the sweep may freeze; only `customer/paused` is
        // the guest's own freeze.
        let _ = fake.customer("customer");
        let guest = fake.customer("customer/paused");
        set_freeze_state(&guest, true);
        let (result, error) = freezer
            .freeze(freeze_options(Duration::from_millis(500)))
            .await;
        assert!(error.is_none(), "{error:?}");
        assert!(result.all_frozen(), "{result:?}");

        let paths = freezer.guest_frozen_paths();
        assert_eq!(paths, vec!["customer/paused".to_owned()]);

        // The resume side restores the same record (freeze.go:1027-1043) and
        // the getter reports it again, so a blob round-trip is identity.
        freezer.set_guest_frozen_paths(&[]);
        // An empty list is a NO-OP, not a clear (freeze.go:1029-1031).
        assert_eq!(
            freezer.guest_frozen_paths(),
            vec!["customer/paused".to_owned()]
        );

        let other = tempfile::tempdir().unwrap();
        let freezer2 = hierarchy_freezer(&fake, INIT_PROCFILE, &other);
        freezer2.set_guest_frozen_paths(&["customer/paused".to_owned()]);
        assert_eq!(
            freezer2.guest_frozen_paths(),
            vec!["customer/paused".to_owned()]
        );
    }

    /// Oracle freeze.go:1064-1087: the inheritance makes the freeze active
    /// (no fresh guest scan adopting our frozen cgroups), raises the spawn
    /// barrier, and re-arms the watchdog.
    #[tokio::test]
    async fn resume_frozen_adopts_our_freeze_raises_the_barrier_and_rearms_the_watchdog() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(
            &dir,
            &[("user", ProcessType::User), ("pty", ProcessType::Pty)],
        );
        let freezer = hierarchy_freezer(&fake, INIT_PROCFILE, &dir);
        freezer.set_thaw_watchdog(Duration::ZERO, None);

        // Freeze like the outgoing envd did, then simulate the execve: a NEW
        // freezer over the same (still-frozen) tree inherits the state.
        let (result, error) = freezer
            .freeze(freeze_options(Duration::from_millis(500)))
            .await;
        assert!(error.is_none(), "{error:?}");
        assert!(result.all_frozen());

        let fresh_root = tempfile::tempdir().unwrap();
        let inherited = hierarchy_freezer(&fake, INIT_PROCFILE, &fresh_root);
        inherited.set_thaw_watchdog(Duration::from_millis(80), None);
        inherited.resume_frozen().await.unwrap();

        // Barrier: a workload spawn is refused outright (spawn.go:104-110,
        // the resume raise at freeze.go:1080-1086).
        let refused = inherited
            .begin_spawn(ProcessType::User, None)
            .await
            .expect_err("an inherited freeze must refuse workload spawns");
        assert!(matches!(
            refused,
            coorworker_envd::cgroups::spawn::SpawnError::WorkloadFrozen
        ));

        // Freeze-active without a rescan: the pre-freeze guest record stays
        // EMPTY (freeze.go:620-623 rescans only when no freeze is in effect)
        // even though the user cgroup reads frozen, and the later thaw CLEARS
        // it instead of preserving it as the guest's.
        assert!(inherited.guest_frozen_paths().is_empty());
        let (result, error) = inherited
            .freeze(freeze_options(Duration::from_millis(500)))
            .await;
        assert!(error.is_none(), "{error:?}");
        assert!(result.all_frozen());
        assert!(
            inherited.guest_frozen_paths().is_empty(),
            "a resume-frozen freeze must not adopt our own frozen cgroups as the guest's"
        );

        // Watchdog: with /init never arriving, the backstop thaw lands and
        // clears the inherited freeze (freeze.go:1072-1077).
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !settled_frozen(&fake.paths[&ProcessType::User]).unwrap(),
            "the re-armed watchdog must thaw the inherited freeze"
        );
    }

    /// Oracle freeze.go:412-428: Thawed returns the pending channel; the next
    /// unfreeze closes it (unfreezeLocked's unconditional signalThawed,
    /// freeze.go:1335); a fresh call then waits for the following thaw.
    #[tokio::test]
    async fn thawed_resolves_on_the_next_unfreeze_and_rearms() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeCgroupfs::new(
            &dir,
            &[("user", ProcessType::User), ("pty", ProcessType::Pty)],
        );
        let freezer = hierarchy_freezer(&fake, INIT_PROCFILE, &dir);
        let mut wait = freezer.thawed();

        let (result, error) = freezer
            .freeze(freeze_options(Duration::from_millis(500)))
            .await;
        assert!(error.is_none(), "{error:?}");
        assert!(result.all_frozen());
        freezer.unfreeze().await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), wait.changed())
            .await
            .expect("the thaw must close the pending Thawed channel")
            .unwrap();

        // A fresh waiter is NOT already resolved: it waits for the NEXT thaw.
        let mut next = freezer.thawed();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !next.has_changed().unwrap(),
            "a Thawed taken after the signal must wait for the next thaw"
        );
        freezer.unfreeze().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), next.changed())
            .await
            .expect("the second thaw must close the fresh Thawed channel")
            .unwrap();
    }
}

/// Oracle upgrade.go:49-94 (the schema gate and schemaFor), 618-672
/// (handoverDefaults/ApplyHandoverDefaults), 341-361 (read + schema gate).
mod blob {
    use super::*;

    fn state() -> wire::HandoverState {
        wire::HandoverState {
            from_ver: "0.10.2".into(),
            ..Default::default()
        }
    }

    /// Oracle upgrade.go:71-94: the stamp is the LOWEST schema that can carry
    /// the blob without loss.
    #[test]
    fn schema_for_stamps_the_lowest_lossless_version() {
        // Nothing lossy: base.
        assert_eq!(coorworker_envd::upgrade::schema_for(&state()), 1);
        // A non-empty guest-frozen record needs schema 2.
        let mut guest_frozen = state();
        guest_frozen.guest_frozen_cgroups = vec!["customer/paused".into()];
        assert_eq!(coorworker_envd::upgrade::schema_for(&guest_frozen), 2);
        // An empty record does NOT spend a refusal on it (upgrade.go:62-70).
        let mut empty_record = state();
        empty_record.guest_frozen_cgroups = Vec::new();
        assert_eq!(coorworker_envd::upgrade::schema_for(&empty_record), 1);
        // A delivered non-fallback user or a workdir needs schema 3
        // (upgrade.go:75-88).
        let mut fallback_user = state();
        fallback_user.defaults = Some(wire::HandoverDefaults {
            user: coorworker_envd::exec_context::BUILTIN_DEFAULT_USER.into(),
            ..Default::default()
        })
        .into();
        assert_eq!(coorworker_envd::upgrade::schema_for(&fallback_user), 1);
        let mut delivered = state();
        delivered.defaults = Some(wire::HandoverDefaults {
            user: "guest".into(),
            ..Default::default()
        })
        .into();
        assert_eq!(coorworker_envd::upgrade::schema_for(&delivered), 3);
        let mut workdir_only = state();
        workdir_only.defaults = Some(wire::HandoverDefaults {
            workdir: "/tmp".into(),
            has_workdir: true,
            ..Default::default()
        })
        .into();
        assert_eq!(coorworker_envd::upgrade::schema_for(&workdir_only), 3);
        // Defaults outrank guest-frozen (checked first, upgrade.go:85-93).
        let mut both = guest_frozen.clone();
        both.defaults = delivered.defaults.clone();
        assert_eq!(coorworker_envd::upgrade::schema_for(&both), 3);
    }

    /// Oracle upgrade.go:618-643: only a DELIVERED user is carried; the
    /// workdir rides independently; an empty record collapses to nil.
    #[test]
    fn handover_defaults_carries_delivered_fields_per_field() {
        let carried = coorworker_envd::upgrade::handover_defaults(&Defaults {
            user: "root".into(),
            workdir: Some("/srv".into()),
            user_delivered: false,
        });
        assert_eq!(
            carried.as_ref().map(|carried| carried.user.as_str()),
            Some(""),
            "an undelivered user is not carried"
        );
        assert_eq!(
            carried
                .as_ref()
                .map(|carried| (carried.workdir.as_str(), carried.has_workdir)),
            Some(("/srv", true))
        );
        let none = coorworker_envd::upgrade::handover_defaults(&Defaults {
            user: "root".into(),
            workdir: None,
            user_delivered: false,
        });
        assert!(none.is_none(), "an empty record collapses to absent");
        let delivered = coorworker_envd::upgrade::handover_defaults(&Defaults {
            user: "guest".into(),
            workdir: None,
            user_delivered: true,
        });
        assert_eq!(
            delivered.as_ref().map(|carried| carried.user.as_str()),
            Some("guest")
        );
    }

    /// Oracle upgrade.go:655-672: restoration is per field and only widens;
    /// the reported booleans name the fields actually applied.
    #[test]
    fn apply_handover_defaults_restores_per_field_and_reports() {
        let mut defaults = Defaults::default();
        let baseline = state();
        assert_eq!(
            coorworker_envd::upgrade::apply_handover_defaults(&baseline, &mut defaults),
            (false, false),
            "an absent record leaves the fallback"
        );
        let mut carried = state();
        carried.defaults = Some(wire::HandoverDefaults {
            user: "guest".into(),
            workdir: "/srv".into(),
            has_workdir: true,
            ..Default::default()
        })
        .into();
        assert_eq!(
            coorworker_envd::upgrade::apply_handover_defaults(&carried, &mut defaults),
            (true, true)
        );
        assert_eq!(defaults.user, "guest");
        assert!(defaults.user_delivered);
        assert_eq!(defaults.workdir.as_deref(), Some("/srv"));

        // Workdir-only: the user stays the fallback but the workdir lands.
        let mut defaults = Defaults::default();
        let mut workdir_only = state();
        workdir_only.defaults = Some(wire::HandoverDefaults {
            workdir: String::new(),
            has_workdir: true,
            ..Default::default()
        })
        .into();
        assert_eq!(
            coorworker_envd::upgrade::apply_handover_defaults(&workdir_only, &mut defaults),
            (false, true)
        );
        assert_eq!(defaults.workdir.as_deref(), Some(""));
    }

    /// Oracle upgrade.go:341-361: no file is a no-op resume; a newer schema
    /// than this binary understands is refused, verbatim.
    #[test]
    fn read_handover_treats_absent_as_noop_and_refuses_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("envd-handover.pb");
        assert!(matches!(
            coorworker_envd::upgrade::read_handover(&path),
            Ok(None)
        ));

        let mut future = state();
        future.schema = coorworker_envd::upgrade::HANDOVER_SCHEMA + 1;
        std::fs::write(&path, future.encode_to_vec()).unwrap();
        let error = coorworker_envd::upgrade::read_handover(&path).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "handover schema {} exceeds max supported {}",
                coorworker_envd::upgrade::HANDOVER_SCHEMA + 1,
                coorworker_envd::upgrade::HANDOVER_SCHEMA
            )
        );
    }

    /// The full outgoing→incoming round trip on a temp path: what the writer
    /// assembled (schema stamp included) is what the reader hands back.
    #[test]
    fn write_then_read_round_trips_the_assembled_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("envd-handover.pb");
        let mut carried = state();
        carried.guest_frozen_cgroups = vec!["customer/paused".into()];
        carried.forwards = vec![wire::ForwardedPort {
            key: "100-8080".into(),
            port: 8080,
            listener_pid: 100,
            family: 4,
            socat_pid: 4242,
            ..Default::default()
        }];
        let stamped = coorworker_envd::upgrade::write_handover(&path, &carried).unwrap();
        assert_eq!(stamped, 2, "the written stamp is schemaFor's answer");
        let read = coorworker_envd::upgrade::read_handover(&path)
            .unwrap()
            .expect("the blob exists");
        assert_eq!(read.from_ver, "0.10.2");
        assert_eq!(
            read.guest_frozen_cgroups,
            vec!["customer/paused".to_owned()]
        );
        assert_eq!(read.forwards.len(), 1);
        assert_eq!(read.forwards[0].key, "100-8080");
        assert_eq!(read.forwards[0].socat_pid, 4242);
    }

    /// Wire parity with the Go proto.Marshal layout: field numbers in order,
    /// varint shapes per handover.proto. Hand-encoded expectation for
    /// ForwardedPort{key:"100-8080", port:8080, listener_pid:100, family:4,
    /// socat_pid:4242} — the fields the ports slice staged (fields 1-5).
    #[test]
    fn forwarded_port_wire_bytes_match_handover_proto_field_numbers() {
        let forwarded: wire::ForwardedPort = serde_json::from_value(serde_json::json!({
            "key": "100-8080", "port": 8080, "listenerPid": 100,
            "family": 4, "socatPid": 4242
        }))
        .unwrap();
        let bytes = forwarded.encode_to_vec();
        // field 1 (key, len-delim): 0x0A, len 8, "100-8080"
        // field 2 (port, varint):   0x10, 0x90 0x3F (8080 = 63*128 + 16)
        // field 3 (listener, varint): 0x18, 0x64 (100)
        // field 4 (family, varint):   0x20, 0x04
        // field 5 (socat, varint):    0x28, 0x92 0x21 (4242 = 33*128 + 18)
        assert_eq!(
            bytes,
            vec![
                0x0A, 0x08, b'1', b'0', b'0', b'-', b'8', b'0', b'8', b'0', //
                0x10, 0x90, 0x3F, //
                0x18, 0x64, //
                0x20, 0x04, //
                0x28, 0x92, 0x21,
            ]
        );
        let decoded = wire::ForwardedPort::decode_from_slice(&bytes).unwrap();
        assert_eq!(decoded.key, "100-8080");
        assert_eq!(decoded.port, 8080);
        assert_eq!(decoded.listener_pid, 100);
        assert_eq!(decoded.family, 4);
        assert_eq!(decoded.socat_pid, 4242);
    }
}
