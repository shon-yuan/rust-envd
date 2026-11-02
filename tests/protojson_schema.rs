//! Replays the oracle's protojson schema-error corpus byte for byte.
//!
//! `scripts/tests/envd-parity/schema_vm.py --side oracle --rust-fixture PATH`
//! boots the pinned Go daemon in a disposable guest and records its raw
//! responses; `fixtures/protojson_schema.json` holds the deterministic request
//! bodies plus those bytes. Every case is rejected before a handler acts, or
//! selects a non-existent process tag, so the replay starts no process and
//! touches no user file.

use axum::{
    body::{to_bytes, Body},
    http::{HeaderValue, Request},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use coorworker_envd::{
    daemon::{app_with_execution, Daemon},
    exec_context::{Account, Defaults, Environment},
    process::ProcessRuntime,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

/// The access token installed before replay; the oracle's own token is random.
const TOKEN: &str = "schema-fixture-token";

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    path: String,
    headers: BTreeMap<String, String>,
    body_base64: String,
    status: u16,
    content_type: Option<String>,
    response_base64: String,
}

fn runtime() -> ProcessRuntime {
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    ProcessRuntime::new(
        Account {
            name: user.name,
            home: user.dir.to_string_lossy().into(),
            uid: user.uid.as_raw(),
            gid: user.gid.as_raw(),
            groups: nix::unistd::getgroups()
                .unwrap()
                .iter()
                .map(|group| group.as_raw())
                .collect(),
        },
        Defaults::default(),
        Environment::default(),
        std::env::var("PATH").ok(),
    )
    .unwrap()
}

/// Every oracle outcome — status, content type and body bytes — must match.
/// The texts come from Go's protojson decoder, so a difference here is a
/// protocol difference an SDK can observe.
#[tokio::test]
async fn schema_failures_carry_the_oracle_protojson_texts() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("fixtures/protojson_schema.json")).unwrap();
    let app = app_with_execution(Arc::new(Daemon::new()), runtime());
    let initialized = app
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(format!(r#"{{"accessToken":"{TOKEN}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(initialized.status().as_u16(), 204, "init");

    for case in &fixture.cases {
        let mut request = Request::post(&case.path).header("x-access-token", TOKEN);
        for (name, value) in &case.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let body = STANDARD.decode(&case.body_base64).unwrap();
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), case.status, "{}", case.name);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .map(HeaderValue::to_str)
                .transpose()
                .unwrap(),
            case.content_type.as_deref(),
            "{}",
            case.name
        );
        let expected = STANDARD.decode(&case.response_base64).unwrap();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.as_ref(), expected, "{}", case.name);
    }
    assert_eq!(fixture.cases.len(), 73, "the oracle corpus grew or shrank");
}
