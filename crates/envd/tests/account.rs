use base64::{engine::general_purpose::STANDARD, Engine};
use connectrpc::RequestContext;
use coorworker_envd::exec_context::{request_account, Defaults};

#[test]
fn resolves_basic_username_ignores_password_and_falls_back_on_malformed_header() {
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let defaults = Defaults {
        user: user.name.clone(),
        ..Default::default()
    };
    for authorization in [
        None,
        Some("Basic !!!".to_owned()),
        Some(format!(
            "Basic {}",
            STANDARD.encode(format!("{}:ignored:password", user.name))
        )),
    ] {
        let mut headers = axum::http::HeaderMap::new();
        if let Some(value) = authorization {
            headers.insert("authorization", value.parse().unwrap());
        }
        let ctx = RequestContext::new(headers);
        let account = request_account(&defaults, &ctx).unwrap();
        assert_eq!(account.uid, user.uid.as_raw());
        assert_eq!(account.gid, user.gid.as_raw());
        assert!(account.groups.contains(&user.gid.as_raw()));
    }
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Basic {}", STANDARD.encode(":ignored"))
            .parse()
            .unwrap(),
    );
    let error = request_account(&defaults, &RequestContext::new(headers)).unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::Unauthenticated);
}
