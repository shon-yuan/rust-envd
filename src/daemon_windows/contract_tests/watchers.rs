use super::*;

#[tokio::test]
async fn create_watcher_returns_an_id_and_get_drains_events() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let (status, created) = create_watcher(&app, directory.path()).await;
    assert_eq!(status, 200);
    let id = created["watcherId"].as_str().unwrap().to_owned();
    assert!(id.starts_with('w'));
    std::fs::write(directory.path().join("created.txt"), b"hello").expect("write");

    let mut saw_create = false;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (status, value) = get_events(&app, &id).await;
            assert_eq!(status, 200);
            if let Some(events) = value["events"].as_array() {
                saw_create |= events.iter().any(|event| {
                    event["name"] == "created.txt"
                        && event["type"] == "EVENT_TYPE_CREATE"
                        && event["entry"]["name"] == "created.txt"
                });
            }
            if saw_create {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("create event");
    assert!(saw_create);

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (status, value) = get_events(&app, &id).await;
            assert_eq!(status, 200);
            if value == json!({}) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("drained watcher");
}

#[tokio::test]
async fn get_and_remove_watcher_validate_only_explicit_basic_users() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let (status, created) = create_watcher(&app, directory.path()).await;
    assert_eq!(status, 200);
    let id = created["watcherId"].as_str().unwrap().to_owned();

    // An unknown DEFAULT user is irrelevant to the ID operations: they
    // only validate an explicitly supplied Basic username.
    init_json(
        running.address,
        json!({"accessToken": TOKEN, "defaultUser": "envd-unknown-watcher-user-9c11"}),
    )
    .await;
    let (status, value) = get_events(&app, &id).await;
    assert_eq!((status, value), (200, json!({})));

    let unknown = basic("envd-unknown-watcher-user-9c11");
    let (status, error) = connect_unary(
        &app,
        "GetWatcherEvents",
        json!({"watcherId": id}),
        Some(&unknown),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(error["code"], "unauthenticated");

    let (status, error) = get_events(&app, "wmissing").await;
    assert_eq!(status, 404);
    assert_eq!(error["message"], "watcher with id wmissing not found");

    let (status, value) = connect_unary(
        &app,
        "RemoveWatcher",
        json!({"watcherId": id}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!((status, value), (200, json!({})));
    let (status, error) = get_events(&app, &id).await;
    assert_eq!(status, 404);
    assert_eq!(error["message"], format!("watcher with id {id} not found"));
}

#[tokio::test]
async fn create_watcher_refuses_missing_file_and_file_paths() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let missing = directory.path().join("missing");
    let (status, error) = create_watcher(&app, &missing).await;
    assert_eq!(status, 404);
    assert_eq!(error["code"], "not_found");
    let message = error["message"].as_str().unwrap();
    assert!(message.starts_with(&format!("path {} not found: stat ", path_text(&missing))));
    assert!(message.ends_with("no such file or directory"));

    let file = directory.path().join("regular.txt");
    std::fs::write(&file, b"x").expect("write");
    let (status, error) = create_watcher(&app, &file).await;
    assert_eq!(status, 400);
    assert_eq!(error["code"], "invalid_argument");
    assert_eq!(error["message"], "path is not a directory");
}

#[tokio::test]
async fn watch_dir_stream_starts_then_delivers_filesystem_events() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let mut frames = Frames::new(
        watch(
            &app,
            json!({"path": path_text(directory.path()), "recursive": true, "includeEntry": true}),
            None,
            Some(TOKEN),
            None,
        )
        .await,
    );
    assert_eq!(frames.next().await, (0, json!({"start":{}})));
    std::fs::write(directory.path().join("file.txt"), b"data").expect("write");
    let created = frames.event("file.txt", "EVENT_TYPE_CREATE").await;
    assert_eq!(created["entry"]["type"], "FILE_TYPE_FILE");
    assert_eq!(
        created["entry"]["path"],
        path_text(&directory.path().join("file.txt"))
    );
    frames.event("file.txt", "EVENT_TYPE_WRITE").await;
    drop(frames);
}

#[tokio::test]
async fn watch_dir_stream_emits_keepalive() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let mut frames = Frames::new(
        watch(
            &app,
            json!({"path": path_text(directory.path())}),
            None,
            Some(TOKEN),
            Some("1"),
        )
        .await,
    );
    assert_eq!(frames.next().await, (0, json!({"start":{}})));
    let (flags, value) = frames.next().await;
    assert_eq!(flags, 0);
    assert!(value.get("keepalive").is_some(), "{value}");
}

#[tokio::test]
async fn watch_dir_reports_rename_then_create() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    std::fs::write(directory.path().join("file.txt"), b"x").expect("write");
    let mut frames = Frames::new(
        watch(
            &app,
            json!({"path": path_text(directory.path())}),
            None,
            Some(TOKEN),
            None,
        )
        .await,
    );
    assert_eq!(frames.next().await, (0, json!({"start":{}})));
    std::fs::rename(
        directory.path().join("file.txt"),
        directory.path().join("renamed.txt"),
    )
    .expect("rename");
    let renamed = frames.event("file.txt", "EVENT_TYPE_RENAME").await;
    assert!(renamed.get("entry").is_none());
    frames.event("renamed.txt", "EVENT_TYPE_CREATE").await;
    drop(frames);
}

/// Opens the directory with a share mode of 0. This succeeds only when no
/// other handle with read access (in particular the watcher's directory
/// handle) is open; otherwise Win32 reports `ERROR_SHARING_VIOLATION`.
fn open_exclusive_directory(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

fn sharing_violation(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32)
}

#[tokio::test]
async fn remove_watcher_releases_the_directory_handle() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let watched = directory.path().join("watched");
    std::fs::create_dir(&watched).expect("watched");
    let (status, created) = create_watcher(&app, &watched).await;
    assert_eq!(status, 200);
    let id = created["watcherId"].as_str().unwrap().to_owned();

    // While the watcher is registered its directory handle is open, so an
    // exclusive open must fail with a sharing violation.
    let error = open_exclusive_directory(&watched).expect_err("watcher holds a directory handle");
    assert!(
        sharing_violation(&error),
        "expected a sharing violation, got {error:?}"
    );

    let (status, _) = connect_unary(
        &app,
        "RemoveWatcher",
        json!({"watcherId": id}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);

    // RemoveWatcher aborts the reader task; wait (bounded) until the handle is
    // really closed, then prove it by opening the directory exclusively.
    let handle = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match open_exclusive_directory(&watched) {
                Ok(handle) => break handle,
                Err(error) if sharing_violation(&error) => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(error) => panic!("exclusive open failed unexpectedly: {error:?}"),
            }
        }
    })
    .await
    .expect("directory handle released");
    drop(handle);
    assert!(watched.is_dir());
}
