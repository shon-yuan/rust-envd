use super::*;

#[tokio::test]
async fn stat_reports_windows_entry_fields_and_synthesized_mode() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let file = directory.path().join("hello.txt");
    std::fs::write(&file, b"hello").expect("write");
    let (status, body) = connect_unary(
        &running.app(),
        "Stat",
        json!({"path": path_text(&file)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    let entry = &body["entry"];
    assert_eq!(entry["name"], "hello.txt");
    assert_eq!(entry["path"], path_text(&file));
    assert_eq!(entry["size"], "5");
    assert_eq!(entry["mode"], 0o644);
    assert_eq!(entry["permissions"], "-rw-r--r--");
    assert_eq!(entry["type"], "FILE_TYPE_FILE");
    // connect-go marshals with the default `protojson.MarshalOptions{}`, which
    // omits unpopulated fields: a false `is_symlink` never reaches the wire.
    assert!(entry.get("isSymlink").is_none(), "{entry}");
    assert!(entry["modifiedTime"].is_string());
    assert!(!entry["owner"].as_str().unwrap_or("").is_empty());
    assert!(!entry["group"].as_str().unwrap_or("").is_empty());
}

#[tokio::test]
async fn stat_missing_path_is_not_found_with_the_linux_shaped_message() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let missing = directory.path().join("missing");
    let (status, error) = connect_unary(
        &running.app(),
        "Stat",
        json!({"path": path_text(&missing)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error["code"], "not_found");
    let message = error["message"].as_str().unwrap();
    assert!(message.starts_with("file not found: lstat "), "{message}");
    assert!(message.ends_with("no such file or directory"), "{message}");
}

#[tokio::test]
async fn stat_rejects_user_specific_home_expansion() {
    let running = initialized().await;
    let (status, error) = connect_unary(
        &running.app(),
        "Stat",
        json!({"path": "~other/file"}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(error["code"], "invalid_argument");
    assert_eq!(error["message"], "cannot expand user-specific home dir");
}

#[tokio::test]
async fn make_dir_creates_parents_and_reports_already_exists() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let (status, error) =
        connect_unary(&app, "MakeDir", json!({"path": ""}), None, Some(TOKEN)).await;
    assert_eq!(status, 409);
    assert_eq!(error["code"], "already_exists");
    assert_eq!(
        error["message"],
        format!("directory already exists: {}", path_text(directory.path()))
    );

    // Built component by component: the daemon resolves the request to a
    // native path, so the entry echoes backslashes on Windows. A mixed
    // separator literal would only test the test.
    let child = directory.path().join("new").join("parent").join("child");
    let (status, made) = connect_unary(
        &app,
        "MakeDir",
        json!({"path": path_text(&child)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(made["entry"]["type"], "FILE_TYPE_DIRECTORY");
    assert_eq!(made["entry"]["path"], path_text(&child));

    let (status, _) = connect_unary(
        &app,
        "MakeDir",
        json!({"path": path_text(&child)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 409);

    let file = directory.path().join("regular");
    std::fs::write(&file, b"x").expect("write");
    let (status, error) = connect_unary(
        &app,
        "MakeDir",
        json!({"path": path_text(&file)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        error["message"],
        format!(
            "path already exists but it is not a directory: {}",
            path_text(&file)
        )
    );
}

#[tokio::test]
async fn move_creates_destination_parents_and_maps_missing_source() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let source = directory.path().join("source.txt");
    let destination = directory.path().join("new/parent/dst.txt");
    std::fs::write(&source, b"payload").expect("write");
    let (status, moved) = connect_unary(
        &app,
        "Move",
        json!({"source": path_text(&source), "destination": path_text(&destination)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(moved["entry"]["name"], "dst.txt");
    assert_eq!(std::fs::read(&destination).expect("read"), b"payload");
    assert!(!source.exists());

    let missing = directory.path().join("absent.txt");
    let created_parent = directory.path().join("created-even-on-failure");
    let (status, error) = connect_unary(
        &app,
        "Move",
        json!({
            "source": path_text(&missing),
            "destination": path_text(&created_parent.join("dst.txt")),
        }),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(
        error["message"],
        format!(
            "source file not found: rename {} {}: no such file or directory",
            path_text(&missing),
            path_text(&created_parent.join("dst.txt"))
        )
    );
    assert!(created_parent.is_dir());

    let replacement = directory.path().join("replacement.txt");
    std::fs::write(&replacement, b"new").expect("write");
    let (status, _) = connect_unary(
        &app,
        "Move",
        json!({"source": path_text(&replacement), "destination": path_text(&destination)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(std::fs::read(&destination).expect("read"), b"new");
}

#[tokio::test]
async fn list_dir_honors_depth_alias_paths_and_sorting() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    populate_tree(directory.path());

    let (status, shallow) = connect_unary(
        &app,
        "ListDir",
        json!({"path": path_text(directory.path()), "depth": 0}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    let names: Vec<_> = shallow["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "z"]);
    assert!(shallow["entries"].as_array().unwrap().iter().all(|entry| {
        entry["path"]
            .as_str()
            .unwrap()
            .starts_with(&path_text(directory.path()))
    }));

    let (status, deep) = connect_unary(
        &app,
        "ListDir",
        json!({"path": path_text(directory.path()), "depth": 2}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    let names: Vec<_> = deep["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "b", "z"]);

    let file = directory.path().join("regular.txt");
    std::fs::write(&file, b"x").expect("write");
    let (status, error) = connect_unary(
        &app,
        "ListDir",
        json!({"path": path_text(&file), "depth": 1}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        error["message"],
        format!("path is not a directory: {}", path_text(&file))
    );

    let missing = directory.path().join("missing");
    let (status, error) = connect_unary(
        &app,
        "ListDir",
        json!({"path": path_text(&missing), "depth": 1}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(
        error["message"],
        format!(
            "path not found: lstat {}: no such file or directory",
            path_text(&missing)
        )
    );
}

#[tokio::test]
async fn remove_is_recursive_and_idempotent() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = initialized_at(directory.path()).await;
    let app = running.app();
    let missing = directory.path().join("missing");
    let (status, body) = connect_unary(
        &app,
        "Remove",
        json!({"path": path_text(&missing)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!((status, body), (200, json!({})));

    let tree = directory.path().join("tree");
    std::fs::create_dir_all(tree.join("sub")).expect("tree");
    std::fs::write(tree.join("sub/file"), b"x").expect("file");
    let (status, _) = connect_unary(
        &app,
        "Remove",
        json!({"path": path_text(&tree)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);
    assert!(!tree.exists());
    let (status, _) = connect_unary(
        &app,
        "Remove",
        json!({"path": path_text(&tree)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);

    let target = directory.path().join("junction-target");
    std::fs::create_dir(&target).expect("target");
    std::fs::write(target.join("keep"), b"keep").expect("keep");
    let link = directory.path().join("junction");
    if junction(&link, &target) {
        let (status, _) = connect_unary(
            &app,
            "Remove",
            json!({"path": path_text(&link)}),
            None,
            Some(TOKEN),
        )
        .await;
        assert_eq!(status, 200);
        assert!(!link.exists());
        assert!(target.join("keep").exists());
    }
}

#[tokio::test]
async fn filesystem_unary_calls_require_the_access_token() {
    let running = initialized().await;
    let directory = tempfile::tempdir().expect("tempdir");
    let file = directory.path().join("guarded.txt");
    std::fs::write(&file, b"x").expect("write");
    let app = running.app();
    let (status, error) =
        connect_unary(&app, "Stat", json!({"path": path_text(&file)}), None, None).await;
    assert_eq!(status, 401);
    assert_eq!(error["code"], 401);

    let (status, _) = connect_unary(
        &app,
        "Stat",
        json!({"path": path_text(&file)}),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200);

    let unknown = basic("envd-unknown-filesystem-user-4d2a");
    let (status, error) = connect_unary(
        &app,
        "Stat",
        json!({"path": path_text(&file)}),
        Some(&unknown),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(error["code"], "unauthenticated");
}
