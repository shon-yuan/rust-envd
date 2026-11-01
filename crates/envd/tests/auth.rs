use coorworker_envd::auth::{Access, AuthError, FileAccess, ReadWrite};

#[test]
fn existing_token_fast_path_never_confuses_first_setup_with_a_match() {
    let mut access = Access::fresh();
    assert!(!access.matches_current_token(None));
    assert!(!access.matches_current_token(Some("candidate")));
    assert!(access.validate_init(Some("candidate"), None).is_ok());
    access.set_token(Some("candidate".into())).unwrap();
    assert!(access.matches_current_token(Some("candidate")));
    assert!(!access.matches_current_token(None));
    assert!(!access.matches_current_token(Some("wrong")));
    access.set_token(None).unwrap();
    assert!(!access.matches_current_token(Some("candidate")));
}

#[test]
fn empty_token_rejection_preserves_the_existing_secret() {
    let mut access = Access::fresh();
    access.set_token(Some("original".into())).unwrap();
    assert_eq!(
        access.set_token(Some(String::new())),
        Err(AuthError::EmptyToken)
    );
    assert_eq!(
        access.authorize("GET", "/envs", None),
        Err(AuthError::Unauthorized)
    );
    assert!(access.authorize("GET", "/envs", Some("original")).is_ok());
    access.set_token(Some("replacement".into())).unwrap();
    assert!(access.authorize("GET", "/envs", Some("original")).is_err());
    assert!(access
        .authorize("GET", "/envs", Some("replacement"))
        .is_ok());
    access.set_token(None).unwrap();
    assert!(access.authorize("GET", "/envs", None).is_ok());
}

#[cfg(target_os = "linux")]
#[test]
fn locked_storage_is_released_and_lock_failure_preserves_the_token() {
    const CHILD: &str = "ENVD_AUTH_LOCK_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("prlimit")
            .args(["--memlock=8192:8192", "--"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "locked_storage_is_released_and_lock_failure_preserves_the_token",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    fn locked_kib() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("VmLck:"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }
    let before = locked_kib();
    let mut access = Access::fresh();
    access.set_token(Some("original".into())).unwrap();
    assert!(locked_kib() > before);
    assert_eq!(
        access.set_token(Some("x".repeat(128 * 1024))),
        Err(AuthError::TokenStorage)
    );
    assert!(access.authorize("GET", "/envs", Some("original")).is_ok());
    assert!(access.authorize("GET", "/envs", None).is_err());
    drop(access);
    assert_eq!(locked_kib(), before);
}

#[test]
fn protected_access_is_send_and_sync_and_works_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Access>();
    let mut access = Access::fresh();
    access.set_token(Some("thread-secret".into())).unwrap();
    let access = std::sync::Arc::new(access);
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let access = access.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    assert!(access
                        .authorize("GET", "/envs", Some("thread-secret"))
                        .is_ok());
                    assert!(access.signature("file", ReadWrite::Read, "", None).is_ok());
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn init_validation_matches_current_token_mmds_and_reset_policy() {
    use sha2::{Digest, Sha512};
    let token_hash = format!("{:x}", Sha512::digest(b"new-token"));
    let reset_hash = format!("{:x}", Sha512::digest(b""));
    let mut access = Access::fresh();
    for hash in [None, Some("")] {
        assert!(access.validate_init(None, hash).is_ok());
        assert!(access.validate_init(Some("new-token"), hash).is_ok());
    }
    assert!(access
        .validate_init(Some("new-token"), Some(&token_hash))
        .is_ok());
    assert!(access.validate_init(None, Some(&reset_hash)).is_ok());
    assert_eq!(
        access.validate_init(None, Some(&token_hash)),
        Err(AuthError::ResetNotAuthorized)
    );
    assert_eq!(
        access.validate_init(Some("wrong"), Some(&token_hash)),
        Err(AuthError::TokenMismatch)
    );
    assert_eq!(
        access.validate_init(Some(""), None),
        Err(AuthError::EmptyToken)
    );
    access.set_token(Some("current".into())).unwrap();
    assert!(access
        .validate_init(Some("current"), Some("wrong-hash"))
        .is_ok());
    assert!(access
        .validate_init(Some("new-token"), Some(&token_hash))
        .is_ok());
    assert!(access.validate_init(None, Some(&reset_hash)).is_ok());
    assert_eq!(
        access.validate_init(None, None),
        Err(AuthError::ResetNotAuthorized)
    );
    assert_eq!(
        access.validate_init(Some("wrong"), None),
        Err(AuthError::TokenMismatch)
    );
    assert!(access.authorize("GET", "/envs", Some("current")).is_ok());
}

#[test]
fn fresh_boot_token_and_handover_gates_follow_official_exclusions() {
    let mut access = Access::fresh();
    assert!(access.authorize("GET", "/envs", None).is_ok());
    access.set_token(Some("test-sandbox-token".into())).unwrap();
    assert!(access.authorize("GET", "/envs", None).is_err());
    assert!(access.authorize("GET", "/envs", Some("wrong")).is_err());
    assert!(access
        .authorize("GET", "/envs", Some("test-sandbox-token"))
        .is_ok());
    for (method, path) in [
        ("GET", "/health"),
        ("POST", "/init"),
        ("GET", "/files"),
        ("POST", "/files"),
    ] {
        assert!(access.authorize(method, path, Some("wrong")).is_ok());
    }
    assert!(access.authorize("POST", "/files/compose", None).is_err());
    let mut handover = Access::handover();
    assert!(handover.authorize("GET", "/health", None).is_ok());
    assert!(handover.authorize("POST", "/init", None).is_ok());
    assert!(handover.authorize("GET", "/files", None).is_err());
    handover.set_token(Some("restored".into())).unwrap();
    assert!(handover.authorize("GET", "/envs", Some("restored")).is_ok());
}

#[test]
fn signature_uses_raw_path_operation_username_token_and_optional_expiry() {
    let mut access = Access::fresh();
    access.set_token(Some("sandbox-token".into())).unwrap();
    // Fixed SHA256/base64 vectors, generated independently of the implementation.
    assert_eq!(
        access
            .signature("/tmp/a:ü", ReadWrite::Read, "", None)
            .unwrap(),
        "v1_MIkRzIVb860LFr3PJkZY7XDIUMfKf0lL4iumq+7bKVo"
    );
    assert_eq!(
        access
            .signature("relative", ReadWrite::Write, "root", Some(1700000000))
            .unwrap(),
        "v1_3jCzlGjiA/G+FR8ucUnJ5DSi/UNjQ4xP19PJqzqDb04"
    );
}

#[test]
fn file_header_token_overrides_signature_and_expiration_is_inclusive() {
    let mut access = Access::fresh();
    access.set_token(Some("sandbox-token".into())).unwrap();
    let signature = access
        .signature("relative", ReadWrite::Read, "", Some(100))
        .unwrap();
    let valid = FileAccess {
        path: "relative",
        operation: ReadWrite::Read,
        username: None,
        header_token: None,
        signature: Some(&signature),
        expiration: Some(100),
        now: 100,
    };
    assert!(access.authorize_file(&valid).is_ok());
    assert!(access
        .authorize_file(&FileAccess { now: 101, ..valid })
        .is_err());
    assert!(access
        .authorize_file(&FileAccess {
            header_token: Some("wrong"),
            ..valid
        })
        .is_err());
    assert!(access
        .authorize_file(&FileAccess {
            header_token: Some("sandbox-token"),
            signature: None,
            now: 101,
            ..valid
        })
        .is_ok());
    assert!(access
        .authorize_file(&FileAccess {
            username: Some("root"),
            ..valid
        })
        .is_err());
    assert!(access
        .authorize_file(&FileAccess {
            path: "/absolute",
            ..valid
        })
        .is_err());
    assert!(access
        .authorize_file(&FileAccess {
            operation: ReadWrite::Write,
            ..valid
        })
        .is_err());
    assert!(access
        .authorize_file(&FileAccess {
            username: Some("a:b"),
            ..valid
        })
        .is_err());
    assert!(Access::fresh().authorize_file(&valid).is_ok());
}
