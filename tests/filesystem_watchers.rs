//! Real inotify events in ordinary-user temporary directories; no daemon/handover fixture.
#[path = "../src/platform/linux/filesystem/watchers.rs"]
mod watchers;

use coorworker_envd::{exec_context, filesystem, proto};
use exec_context::{Account, Defaults};
use nix::unistd::{getegid, geteuid};
use proto::filesystem::{self as pb, watch_dir_response};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    time::Duration,
};
use watchers::{WatchSubscription, WatcherRegistry};

fn context(dir: &std::path::Path) -> (Account, Defaults) {
    assert!(
        !geteuid().is_root(),
        "watcher tests require an ordinary user"
    );
    (
        Account {
            name: "watcher-test".into(),
            home: dir.to_str().unwrap().into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: vec![],
        },
        Defaults {
            user: "watcher-test".into(),
            workdir: Some(dir.to_str().unwrap().into()),
            user_delivered: true,
        },
    )
}
fn request(path: &str, recursive: bool, include_entry: bool) -> pb::CreateWatcherRequest {
    pb::CreateWatcherRequest {
        path: path.into(),
        recursive,
        include_entry,
        ..Default::default()
    }
}
async fn next_fs(stream: &mut WatchSubscription) -> pb::FilesystemEvent {
    loop {
        let response = tokio::time::timeout(Duration::from_secs(3), stream.recv())
            .await
            .expect("event deadline")
            .expect("stream open")
            .unwrap();
        if let Some(watch_dir_response::Event::Filesystem(event)) = response.event {
            return *event;
        }
    }
}
async fn find(
    stream: &mut WatchSubscription,
    name: &str,
    kind: pb::EventType,
) -> pb::FilesystemEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = next_fs(stream).await;
            if event.name == name && event.r#type == kind {
                return event;
            }
        }
    })
    .await
    .expect("expected kernel event")
}

async fn wait_for_registration(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let inode = fs::metadata(path).unwrap().ino();
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let found = fs::read_dir("/proc/self/fdinfo")
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    fs::read_to_string(entry.path()).is_ok_and(|text| {
                        text.lines().any(|line| {
                            line.starts_with("inotify ")
                                && line
                                    .split_whitespace()
                                    .any(|part| part == format!("ino:{inode:x}"))
                        })
                    })
                });
            if found {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn recursive_startup_unreadable_directory_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    let (user, defaults) = context(dir.path());
    let child = dir.path().join("unreadable");
    fs::create_dir(&child).unwrap();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o111)).unwrap();
    let result = WatcherRegistry::new().create_watcher(&user, &defaults, request("", true, false));
    fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(error) if error.code == connectrpc::ErrorCode::Internal));
}

#[tokio::test]
async fn recursive_dynamic_unreadable_tree_is_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    let (user, defaults) = context(dir.path());
    let registry = WatcherRegistry::new();
    let id = registry
        .create_watcher(&user, &defaults, request("", true, false))
        .unwrap()
        .watcher_id;
    let source = staging.path().join("unreadable");
    fs::create_dir(&source).unwrap();
    let destination = dir.path().join("unreadable");
    fs::rename(&source, &destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o111)).unwrap();
    let terminal = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if let Err(error) = registry.get_watcher_events(&id) {
                break error;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        terminal
            .expect("dynamic recursive failure must be terminal")
            .code,
        connectrpc::ErrorCode::Internal
    );
}

#[tokio::test]
async fn paused_stream_rearms_child_and_retains_transient_create_remove() {
    let dir = tempfile::tempdir().unwrap();
    let (user, defaults) = context(dir.path());
    let mut stream = WatcherRegistry::new()
        .watch_dir(
            &user,
            &defaults,
            pb::WatchDirRequest {
                recursive: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
    stream.recv().await.unwrap().unwrap();
    let child = dir.path().join("child");
    fs::create_dir(&child).unwrap();
    let registered = wait_for_registration(&child).await;
    if registered {
        fs::write(child.join("transient"), b"x").unwrap();
        fs::remove_file(child.join("transient")).unwrap();
    }
    assert!(
        registered,
        "reader must rearm while subscription is not being polled"
    );
    find(&mut stream, "child/transient", pb::EventType::Create).await;
    find(&mut stream, "child/transient", pb::EventType::Remove).await;
    drop(stream);
    tokio::task::yield_now().await;
    assert!(
        !wait_for_registration(&child).await,
        "cancellation must release inotify registration"
    );
}

#[tokio::test]
async fn stream_start_flags_relative_names_and_optional_entries() {
    let dir = tempfile::tempdir().unwrap();
    let (user, defaults) = context(dir.path());
    let registry = WatcherRegistry::new();
    let mut stream = registry
        .watch_dir(
            &user,
            &defaults,
            pb::WatchDirRequest {
                include_entry: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert!(matches!(
        stream.recv().await.unwrap().unwrap().event,
        Some(watch_dir_response::Event::Start(_))
    ));
    fs::write(dir.path().join("old"), b"hello").unwrap();
    let created = find(&mut stream, "old", pb::EventType::Create).await;
    assert!(created.entry.is_set());
    assert_eq!(created.entry.name, "old");
    find(&mut stream, "old", pb::EventType::Write).await;
    fs::set_permissions(dir.path().join("old"), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(find(&mut stream, "old", pb::EventType::Chmod)
        .await
        .entry
        .is_set());
    fs::rename(dir.path().join("old"), dir.path().join("new")).unwrap();
    assert!(!find(&mut stream, "old", pb::EventType::Rename)
        .await
        .entry
        .is_set());
    assert!(find(&mut stream, "new", pb::EventType::Create)
        .await
        .entry
        .is_set());
    fs::remove_file(dir.path().join("new")).unwrap();
    assert!(!find(&mut stream, "new", pb::EventType::Remove)
        .await
        .entry
        .is_set());
}

#[tokio::test]
async fn persistent_is_detached_unique_atomic_drain_and_unknown_ids_fail() {
    let dir = tempfile::tempdir().unwrap();
    let (user, defaults) = context(dir.path());
    let registry = WatcherRegistry::new();
    let id = registry
        .create_watcher(&user, &defaults, request("", false, false))
        .unwrap()
        .watcher_id;
    let other = registry
        .create_watcher(&user, &defaults, request("", false, false))
        .unwrap()
        .watcher_id;
    assert!(id.starts_with('w'));
    assert_ne!(id, other);
    assert!(registry.get_watcher_events(&id).unwrap().events.is_empty());
    // A detached streaming observer provides an event barrier without sleeps.
    let mut barrier = registry
        .watch_dir(&user, &defaults, pb::WatchDirRequest::default(), None)
        .unwrap();
    fs::write(dir.path().join("data"), b"x").unwrap();
    find(&mut barrier, "data", pb::EventType::Write).await;
    let events = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = registry.get_watcher_events(&id).unwrap().events;
            if !events.is_empty() {
                break events;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(events.iter().any(|e| e.name == "data"));
    assert!(events.iter().all(|e| !e.entry.is_set()));
    registry.remove_watcher(&id).unwrap();
    assert_eq!(
        registry.get_watcher_events(&id).unwrap_err().code,
        connectrpc::ErrorCode::NotFound
    );
    assert_eq!(
        registry.remove_watcher(&id).unwrap_err().code,
        connectrpc::ErrorCode::NotFound
    );
    registry.remove_watcher(&other).unwrap();
}

#[tokio::test]
async fn recursive_existing_new_tree_and_directory_rename_are_rearmed() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("existing/deep")).unwrap();
    let (user, defaults) = context(dir.path());
    let registry = WatcherRegistry::new();
    let mut stream = registry
        .watch_dir(
            &user,
            &defaults,
            pb::WatchDirRequest {
                recursive: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
    fs::write(dir.path().join("existing/deep/a"), b"x").unwrap();
    find(&mut stream, "existing/deep/a", pb::EventType::Create).await;
    fs::create_dir_all(dir.path().join("new/deep")).unwrap();
    fs::write(dir.path().join("new/deep/b"), b"x").unwrap();
    find(&mut stream, "new/deep", pb::EventType::Create).await;
    // Observe rearm before producing the next child event.
    fs::write(dir.path().join("new/deep/after"), b"x").unwrap();
    find(&mut stream, "new/deep/after", pb::EventType::Create).await;
    fs::rename(dir.path().join("existing"), dir.path().join("renamed")).unwrap();
    find(&mut stream, "renamed", pb::EventType::Create).await;
    fs::write(dir.path().join("renamed/deep/after"), b"x").unwrap();
    find(&mut stream, "renamed/deep/after", pb::EventType::Create).await;
}

#[tokio::test]
async fn nonrecursive_does_not_follow_children_or_symlink_aliases() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("child")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), dir.path().join("alias")).unwrap();
    let (user, defaults) = context(dir.path());
    let registry = WatcherRegistry::new();
    let id = registry
        .create_watcher(&user, &defaults, request("", false, false))
        .unwrap()
        .watcher_id;
    let mut barrier = registry
        .watch_dir(&user, &defaults, pb::WatchDirRequest::default(), None)
        .unwrap();
    fs::write(dir.path().join("child/hidden"), b"x").unwrap();
    fs::write(outside.path().join("hidden"), b"x").unwrap();
    fs::write(dir.path().join("visible"), b"x").unwrap();
    find(&mut barrier, "visible", pb::EventType::Write).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = registry.get_watcher_events(&id).unwrap().events;
            if !events.is_empty() {
                assert!(events.iter().all(|e| e.name == "visible"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn stream_keepalive_defaults_reset_and_drop_closes_task() {
    let dir = tempfile::tempdir().unwrap();
    let (user, defaults) = context(dir.path());
    let registry = WatcherRegistry::new();
    let mut stream = registry
        .watch_dir(&user, &defaults, pb::WatchDirRequest::default(), Some("2"))
        .unwrap();
    stream.recv().await.unwrap().unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        stream.recv().await.unwrap().unwrap().event,
        Some(watch_dir_response::Event::Keepalive(_))
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    fs::write(dir.path().join("reset"), b"x").unwrap();
    find(&mut stream, "reset", pb::EventType::Write).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    {
        use futures_util::FutureExt;
        assert!(
            stream.recv().now_or_never().is_none(),
            "event must reset keepalive timer"
        );
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        stream.recv().await.unwrap().unwrap().event,
        Some(watch_dir_response::Event::Keepalive(_))
    ));
    assert_eq!(watchers::keepalive_interval(None), Duration::from_secs(90));
    for invalid in ["0", "-1", "abc", "9223372037"] {
        assert_eq!(
            watchers::keepalive_interval(Some(invalid)),
            Duration::from_secs(90)
        );
    }
    assert_eq!(
        watchers::keepalive_interval(Some("+3")),
        Duration::from_secs(3)
    );
    let closed = stream.closed_signal();
    drop(stream);
    tokio::task::yield_now().await;
    assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn validates_paths_and_snapshot_is_nondestructive_exportable_state() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("file"), b"x").unwrap();
    let (user, defaults) = context(dir.path());
    let registry = WatcherRegistry::new();
    for (path, code) in [
        ("missing", connectrpc::ErrorCode::NotFound),
        ("file", connectrpc::ErrorCode::InvalidArgument),
        ("~other", connectrpc::ErrorCode::InvalidArgument),
    ] {
        assert_eq!(
            registry
                .create_watcher(&user, &defaults, request(path, false, false))
                .unwrap_err()
                .code,
            code
        );
    }
    let id = registry
        .create_watcher(&user, &defaults, request("", true, true))
        .unwrap()
        .watcher_id;
    let snapshots = registry.snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].id, id);
    assert_eq!(snapshots[0].watch_path, dir.path());
    assert!(snapshots[0].recursive && snapshots[0].include_entry);
    assert!(snapshots[0].error.is_none());
    assert!(snapshots[0].events.is_empty());
    assert!(registry.get_watcher_events(&id).unwrap().events.is_empty());
    assert!(watchers::is_network_mount_type(0x6969));
    assert!(watchers::is_network_mount_type(0x65735546));
    assert!(!watchers::is_network_mount_type(0xef53));
    assert!(watchers::is_network_mount_type(0xff534d42_u64));
    assert!(watchers::is_network_mount_type(0xfe534d42_i64));
    assert!(!watchers::is_network_mount_type(u64::MAX));
    assert!(!watchers::is_network_mount_type(-1_i64));
}
