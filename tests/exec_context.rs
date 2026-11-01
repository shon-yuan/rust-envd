use std::collections::BTreeMap;

use coorworker_envd::exec_context::{Account, Defaults, Environment};

fn account() -> Account {
    Account {
        name: "guest".into(),
        home: "/home/guest".into(),
        uid: 1000,
        gid: 1000,
        groups: vec![1000, 44],
    }
}

#[test]
fn basic_password_bytes_cannot_change_the_selected_account() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let defaults = Defaults {
        user: "envd-parity-no-such-default".into(),
        ..Defaults::default()
    };
    let mut credentials = format!("{}:", user.name).into_bytes();
    credentials.extend_from_slice(&[0xff, 0, 0xfe]);
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Basic {}", STANDARD.encode(credentials))
            .parse()
            .unwrap(),
    );
    let actual = coorworker_envd::exec_context::request_account(
        &defaults,
        &connectrpc::RequestContext::new(headers),
    )
    .unwrap();
    assert_eq!(actual.name, user.name);
    assert_eq!(actual.uid, user.uid.as_raw());
}

#[test]
fn resolves_paths_with_official_home_and_default_rules() {
    let default = Defaults::default();
    let user = account();
    for (path, expected) in [
        ("", "/home/guest"),
        ("file", "/home/guest/file"),
        ("../file", "/home/file"),
        ("~/dir/../file", "/home/guest/file"),
        ("~", "/home/guest"),
        ("~\\file", "/home/guest/\\file"),
        ("/tmp/../file", "/tmp/../file"),
    ] {
        assert_eq!(default.resolve_path(path, &user).unwrap(), expected);
    }
    assert!(default.resolve_path("~other/file", &user).is_err());
    let custom = Defaults {
        workdir: Some("workspace".into()),
        ..default
    };
    assert_eq!(
        custom.resolve_path("", &user).unwrap(),
        "/home/guest/workspace"
    );
    assert_eq!(
        custom.resolve_path("explicit", &user).unwrap(),
        "/home/guest/explicit"
    );
}

#[test]
fn empty_and_relative_homes_follow_go_join_expand_and_abs() {
    let cwd = std::env::current_dir().unwrap();
    let parent = cwd.parent().unwrap();
    let grandparent = parent.parent().unwrap();
    for (home, path, expected) in [
        ("", "", cwd.clone()),
        ("", "file", cwd.join("file")),
        ("", "../../file", grandparent.join("file")),
        ("", "~/file", "/file".into()),
        ("relative", "file", cwd.join("relative/file")),
        ("relative", "../../file", parent.join("file")),
        ("relative", "~/file", cwd.join("relative/relative/file")),
        ("../relative", "file", parent.join("relative/file")),
        ("../relative", "../../file", grandparent.join("file")),
        ("../relative", "~/file", parent.join("relative/file")),
    ] {
        let user = Account {
            home: home.into(),
            ..account()
        };
        assert_eq!(
            Defaults::default().resolve_path(path, &user).unwrap(),
            expected.to_str().unwrap(),
            "home={home:?}, path={path:?}"
        );
    }
    let user = Account {
        home: "relative".into(),
        ..account()
    };
    let defaults = Defaults {
        workdir: Some("~/file".into()),
        ..Defaults::default()
    };
    assert_eq!(
        defaults.resolve_path("", &user).unwrap(),
        cwd.join("relative/relative/file").to_str().unwrap()
    );
    assert_eq!(
        defaults.resolve_path("/tmp/../file", &user).unwrap(),
        "/tmp/../file"
    );
}

#[test]
fn preserves_explicit_empty_username_and_builtin_root() {
    let default = Defaults::default();
    assert_eq!(default.resolve_username(None).unwrap(), "root");
    assert_eq!(default.resolve_username(Some("")).unwrap(), "");
    let missing = Defaults {
        user: String::new(),
        ..default
    };
    assert!(missing.resolve_username(None).is_err());
}

#[test]
fn init_replaces_user_environment_but_preserves_internal_values() {
    let mut env = Environment::default();
    env.store_internal("E2B_SANDBOX", "true");
    env.replace_user(BTreeMap::from([
        ("OLD".into(), "old".into()),
        ("E2B_SANDBOX".into(), "false".into()),
    ]));
    assert_eq!(env.all()["E2B_SANDBOX"], "true");
    env.replace_user(BTreeMap::from([("NEW".into(), "new".into())]));
    assert!(!env.all().contains_key("OLD"));
    assert_eq!(env.all()["NEW"], "new");
    env.replace_user(BTreeMap::new());
    assert_eq!(
        env.all(),
        BTreeMap::from([("E2B_SANDBOX".into(), "true".into())])
    );
}

#[test]
fn process_environment_has_only_explicit_daemon_path_and_ordered_overrides() {
    let user = account();
    let mut env = Environment::default();
    env.store_internal("E2B_SANDBOX", "true");
    env.replace_user(BTreeMap::from([
        ("USER".into(), "default-user".into()),
        ("DEFAULT".into(), "yes".into()),
    ]));
    let request = BTreeMap::from([
        ("USER".into(), "request-user".into()),
        ("E2B_SANDBOX".into(), "override".into()),
    ]);
    let actual = env.for_process(&user, Some("/usr/bin:/bin"), &request);
    assert_eq!(
        actual,
        BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), "/home/guest".into()),
            ("USER".into(), "request-user".into()),
            ("LOGNAME".into(), "guest".into()),
            ("DEFAULT".into(), "yes".into()),
            ("E2B_SANDBOX".into(), "override".into()),
        ])
    );
    assert!(!env
        .for_process(&user, None, &BTreeMap::new())
        .contains_key("PATH"));
}
