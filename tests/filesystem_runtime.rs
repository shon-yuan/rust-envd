use connectrpc::ErrorCode;
use coorworker_envd::{exec_context, filesystem};
use exec_context::{Account, Defaults};
use filesystem::{metadata, FilesystemRuntime};
use nix::unistd::{getegid, geteuid, Group, User};
use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use tempfile::TempDir;
use xattr::FileExt;

fn fixture() -> (TempDir, FilesystemRuntime) {
    assert!(
        !geteuid().is_root(),
        "filesystem tests require an ordinary user"
    );
    let dir = tempfile::tempdir().unwrap();
    let account = Account {
        name: User::from_uid(geteuid()).unwrap().unwrap().name,
        home: dir.path().to_string_lossy().into_owned(),
        uid: geteuid().as_raw(),
        gid: getegid().as_raw(),
        groups: vec![],
    };
    let defaults = Defaults {
        user: account.name.clone(),
        workdir: Some(dir.path().join("work").to_string_lossy().into_owned()),
        user_delivered: true,
    };
    (dir, FilesystemRuntime::new(account, defaults))
}

#[test]
fn non_regular_entries_and_go_special_permission_prefixes() {
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;
    use std::os::unix::net::UnixListener;
    let (dir, rt) = fixture();
    mkfifo(&dir.path().join("pipe"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let _listener = UnixListener::bind(dir.path().join("socket")).unwrap();
    assert_eq!(
        rt.stat("pipe").unwrap().r#type.to_string(),
        "FILE_TYPE_UNSPECIFIED"
    );
    assert_eq!(rt.stat("pipe").unwrap().permissions, "prw-------");
    assert!(rt.stat("socket").unwrap().permissions.starts_with('S'));
    rt.make_dir("sticky").unwrap();
    fs::set_permissions(
        dir.path().join("sticky"),
        fs::Permissions::from_mode(0o1700),
    )
    .unwrap();
    assert_eq!(rt.stat("sticky").unwrap().permissions, "dtrwx------");
    assert_eq!(rt.stat("sticky").unwrap().mode, 0o700);
}

#[test]
fn mutation_io_errors_use_internal_without_changing_existing_files() {
    let (dir, rt) = fixture();
    fs::write(dir.path().join("file"), b"x").unwrap();
    assert_eq!(
        rt.remove("file/child").unwrap_err().code,
        ErrorCode::Internal
    );
    assert_eq!(
        rt.move_entry("file", "file/child").unwrap_err().code,
        ErrorCode::Internal
    );
    rt.make_dir("locked").unwrap();
    fs::write(dir.path().join("locked/data"), b"x").unwrap();
    fs::set_permissions(dir.path().join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    let result = rt.list_dir("locked", 1);
    fs::set_permissions(dir.path().join("locked"), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.unwrap_err().code, ErrorCode::Internal);
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"x");
}

#[test]
fn stat_returns_lstat_data_names_and_permission_string() {
    let (dir, rt) = fixture();
    let path = dir.path().join("data");
    fs::write(&path, b"hello").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let raw = fs::symlink_metadata(&path).unwrap();
    let entry = rt.stat("data").unwrap();
    assert_eq!(entry.name, "data");
    assert_eq!(entry.path, path.to_str().unwrap());
    assert_eq!(entry.size, 5);
    assert_eq!(entry.mode, 0o640);
    assert_eq!(entry.permissions, "-rw-r-----");
    assert_eq!(entry.r#type.to_string(), "FILE_TYPE_FILE");
    assert_eq!(
        entry.owner,
        User::from_uid(geteuid()).unwrap().unwrap().name
    );
    assert_eq!(
        entry.group,
        Group::from_gid(getegid()).unwrap().unwrap().name
    );
    let time = entry.modified_time;
    assert_eq!(time.seconds, raw.mtime());
    assert_eq!(time.nanos, raw.mtime_nsec() as i32);
    assert!(!entry.is_symlink);
    assert_eq!(entry.symlink_target, None);
}

#[test]
fn symlink_uses_target_type_mode_but_link_size_time_permissions() {
    let (dir, rt) = fixture();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::set_permissions(dir.path().join("target"), fs::Permissions::from_mode(0o700)).unwrap();
    symlink("target", dir.path().join("alias")).unwrap();
    let raw = fs::symlink_metadata(dir.path().join("alias")).unwrap();
    let entry = rt.stat("alias").unwrap();
    assert!(entry.is_symlink);
    assert_eq!(entry.r#type.to_string(), "FILE_TYPE_DIRECTORY");
    assert_eq!(entry.mode, 0o700);
    assert_eq!(entry.permissions, "Lrwxrwxrwx");
    assert_eq!(entry.size, 6);
    assert_eq!(entry.modified_time.seconds, raw.mtime());
    assert_eq!(
        entry.symlink_target.as_deref(),
        dir.path().join("target").to_str()
    );
    symlink("missing", dir.path().join("broken")).unwrap();
    let broken = rt.stat("broken").unwrap();
    assert!(broken.is_symlink);
    assert_eq!(broken.r#type.to_string(), "FILE_TYPE_UNSPECIFIED");
    assert_eq!(broken.mode, 0);
    assert_eq!(
        broken.symlink_target.as_deref(),
        dir.path().join("broken").to_str()
    );
}

#[test]
fn relative_home_tilde_and_empty_default_path() {
    let (dir, rt) = fixture();
    fs::create_dir(dir.path().join("work")).unwrap();
    fs::write(dir.path().join("home-file"), b"x").unwrap();
    assert_eq!(rt.stat("~/home-file").unwrap().name, "home-file");
    assert_eq!(rt.stat("home-file").unwrap().name, "home-file");
    assert_eq!(rt.stat("").unwrap().name, "work");
    assert_eq!(
        rt.stat("~other/file").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(rt.stat("absent").unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(rt.stat("bad\0path").unwrap_err().code, ErrorCode::Internal);
}

#[test]
fn mkdir_creates_and_owns_all_missing_parents_without_changing_existing() {
    let (dir, rt) = fixture();
    fs::create_dir(dir.path().join("existing")).unwrap();
    fs::set_permissions(
        dir.path().join("existing"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let entry = rt.make_dir("existing/a/b").unwrap();
    assert_eq!(entry.r#type.to_string(), "FILE_TYPE_DIRECTORY");
    for path in ["existing/a", "existing/a/b"] {
        let raw = fs::metadata(dir.path().join(path)).unwrap();
        assert_eq!(raw.uid(), geteuid().as_raw());
        assert_eq!(raw.gid(), getegid().as_raw());
        assert_eq!(raw.mode() & 0o777, 0o755);
    }
    assert_eq!(
        fs::metadata(dir.path().join("existing")).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(
        rt.make_dir("existing/a").unwrap_err().code,
        ErrorCode::AlreadyExists
    );
    fs::write(dir.path().join("file"), b"x").unwrap();
    assert_eq!(
        rt.make_dir("file").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        rt.make_dir("file/child").unwrap_err().code,
        ErrorCode::Internal
    );
}

#[test]
fn move_creates_parents_renames_replaces_and_preserves_symlink_identity() {
    let (dir, rt) = fixture();
    fs::write(dir.path().join("src"), b"payload").unwrap();
    let moved = rt.move_entry("src", "new/parent/dst").unwrap();
    assert_eq!(moved.name, "dst");
    assert_eq!(
        fs::read(dir.path().join("new/parent/dst")).unwrap(),
        b"payload"
    );
    assert!(!dir.path().join("src").exists());
    assert_eq!(
        fs::metadata(dir.path().join("new/parent")).unwrap().uid(),
        geteuid().as_raw()
    );
    fs::write(dir.path().join("replacement"), b"new").unwrap();
    rt.move_entry("replacement", "new/parent/dst").unwrap();
    symlink("new/parent/dst", dir.path().join("link")).unwrap();
    assert!(rt.move_entry("link", "moved-link").unwrap().is_symlink);
    assert_eq!(
        rt.move_entry("absent", "created-even-on-failure/dst")
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert!(dir.path().join("created-even-on-failure").is_dir());
}

#[test]
fn list_depth_zero_alias_sorting_and_child_links() {
    let (dir, rt) = fixture();
    fs::create_dir_all(dir.path().join("real/a/deep")).unwrap();
    fs::write(dir.path().join("real/z"), b"z").unwrap();
    fs::write(dir.path().join("real/a/leaf"), b"l").unwrap();
    symlink("a", dir.path().join("real/link")).unwrap();
    symlink("real", dir.path().join("alias")).unwrap();
    let shallow = rt.list_dir("alias", 0).unwrap();
    assert_eq!(
        shallow.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["a", "link", "z"]
    );
    assert!(shallow.iter().all(|e| e
        .path
        .starts_with(dir.path().join("alias").to_str().unwrap())));
    let deep = rt.list_dir("alias", 2).unwrap();
    assert_eq!(
        deep.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["a", "deep", "leaf", "link", "z"]
    );
    assert_eq!(
        rt.list_dir("real/z", 1).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        rt.list_dir("absent", 1).unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[test]
fn root_symlink_cycle_fails_precondition_but_stat_is_an_entry() {
    let (dir, rt) = fixture();
    symlink("loop", dir.path().join("loop")).unwrap();
    assert_eq!(
        rt.list_dir("loop", 1).unwrap_err().code,
        ErrorCode::FailedPrecondition
    );
    assert!(rt.stat("loop").unwrap().is_symlink);
    assert_eq!(rt.stat("loop").unwrap().mode, 0);
}

#[test]
fn root_symlink_chain_over_kernel_limit_but_under_go_limit_resolves() {
    let (dir, rt) = fixture();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/file"), b"x").unwrap();
    for i in 0..60 {
        let next = if i == 59 {
            "target".to_owned()
        } else {
            format!("link{}", i + 1)
        };
        symlink(next, dir.path().join(format!("link{i}"))).unwrap();
    }
    assert_eq!(rt.list_dir("link0", 1).unwrap()[0].name, "file");
    assert_eq!(
        rt.stat("link0").unwrap().r#type.to_string(),
        "FILE_TYPE_DIRECTORY"
    );
}

#[test]
fn remove_missing_recursive_and_symlink_does_not_remove_target() {
    let (dir, rt) = fixture();
    rt.remove("missing/child").unwrap();
    fs::create_dir_all(dir.path().join("tree/sub")).unwrap();
    fs::write(dir.path().join("tree/sub/file"), b"x").unwrap();
    symlink("tree", dir.path().join("link")).unwrap();
    rt.remove("link").unwrap();
    assert!(dir.path().join("tree/sub/file").exists());
    rt.remove("tree").unwrap();
    rt.remove("tree").unwrap();
    fs::write(dir.path().join("file"), b"x").unwrap();
    rt.remove("file").unwrap();
}

#[test]
fn metadata_replaces_namespace_preserves_unrelated_and_uses_open_fd() {
    let (dir, rt) = fixture();
    let original = dir.path().join("original");
    let file = File::create(&original).unwrap();
    file.set_xattr("user.other", b"keep").unwrap();
    metadata::write_metadata(&file, &HashMap::from([("old".into(), "one".into())])).unwrap();
    fs::rename(&original, dir.path().join("moved")).unwrap();
    File::create(&original).unwrap();
    metadata::write_metadata(&file, &HashMap::from([("new".into(), "two".into())])).unwrap();
    assert_eq!(
        rt.stat("moved")
            .unwrap()
            .metadata
            .into_iter()
            .collect::<HashMap<_, _>>(),
        HashMap::from([("new".into(), "two".into())])
    );
    assert!(rt.stat("original").unwrap().metadata.is_empty());
    assert_eq!(file.get_xattr("user.other").unwrap().unwrap(), b"keep");
    symlink("moved", dir.path().join("alias")).unwrap();
    assert_eq!(
        rt.stat("alias")
            .unwrap()
            .metadata
            .get("new")
            .map(String::as_str),
        Some("two")
    );
    metadata::write_metadata(&file, &HashMap::new()).unwrap();
    assert!(rt
        .stat("moved")
        .unwrap()
        .metadata
        .into_iter()
        .collect::<HashMap<_, _>>()
        .is_empty());
}

#[test]
fn metadata_read_skips_invalid_utf8_and_accepts_direct_unicode_values() {
    let (dir, rt) = fixture();
    let file = File::create(dir.path().join("data")).unwrap();
    file.set_xattr("user.e2b.bad", &[0xff]).unwrap();
    file.set_xattr("user.e2b.good", "雪".as_bytes()).unwrap();
    assert_eq!(
        rt.stat("data")
            .unwrap()
            .metadata
            .into_iter()
            .collect::<HashMap<_, _>>(),
        HashMap::from([("good".into(), "雪".into())])
    );
}

#[test]
fn metadata_validates_ascii_key_and_combined_name_value_budget_before_writes() {
    let (dir, _) = fixture();
    let file = File::create(dir.path().join("data")).unwrap();
    metadata::write_metadata(&file, &HashMap::from([("keep".into(), "old".into())])).unwrap();
    for invalid in [
        HashMap::from([("".into(), "x".into())]),
        HashMap::from([("k".repeat(247), "x".into())]),
        HashMap::from([("key".into(), "\n".into())]),
        HashMap::from([("key".into(), "雪".into())]),
        HashMap::from([("key".into(), "x".repeat(4096))]),
    ] {
        assert!(metadata::write_metadata(&file, &invalid).is_err());
        assert_eq!(file.get_xattr("user.e2b.keep").unwrap().unwrap(), b"old");
    }
    metadata::validate_metadata(&HashMap::from([("k".repeat(246), "~".into())])).unwrap();
    metadata::validate_metadata(&HashMap::from([("k".into(), "x".repeat(4086))])).unwrap();
}

#[test]
fn go_wraps_error_messages_with_paths_and_errno_details() {
    // Oracle dir.go/move.go at 51083a83: refusal messages carry the resolved
    // path, and ENOENT wraps the raw syscall text ("rename a b: no such file
    // or directory", "lstat p: no such file or directory").
    let (dir, rt) = fixture();
    let root = dir.path().to_string_lossy().into_owned();
    fs::write(dir.path().join("file"), b"x").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    symlink("loop", dir.path().join("loop")).unwrap();
    assert_eq!(
        rt.make_dir("sub").unwrap_err().message,
        Some(format!("directory already exists: {root}/sub"))
    );
    assert_eq!(
        rt.make_dir("file").unwrap_err().message,
        Some(format!(
            "path already exists but it is not a directory: {root}/file"
        ))
    );
    assert_eq!(
        rt.move_entry("absent", "dst").unwrap_err().message,
        Some(format!(
            "source file not found: rename {root}/absent {root}/dst: no such file or directory"
        ))
    );
    assert_eq!(
        rt.list_dir("missing", 1).unwrap_err().message,
        Some(format!(
            "path not found: lstat {root}/missing: no such file or directory"
        ))
    );
    assert_eq!(
        rt.list_dir("file", 1).unwrap_err().message,
        Some(format!("path is not a directory: {root}/file"))
    );
    // Go %q quoting: plain ASCII paths render as bare double quotes.
    assert_eq!(
        rt.list_dir("loop", 1).unwrap_err().message,
        Some(format!(
            "cyclic symlink or chain >255 links at \"{root}/loop\""
        ))
    );
}
