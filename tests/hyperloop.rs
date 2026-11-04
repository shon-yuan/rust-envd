//! Hyperloop slice: /etc/hosts events alias rewrite + E2B_EVENTS_ADDRESS.
//! Contract: pinned init.go:1027-1084, init.go:459-461, init_test.go:30-66,
//! txeh v1.8.1 observable rendering (Apache-2.0, attributed in src/hyperloop.rs).

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::hyperloop::{get_ip_family, rewrite_hosts_file, IPFamily};
use std::time::Duration;
use tower::ServiceExt;

/// Poll interval for the async waits: yielding alone can spin a whole budget
/// before a blocking writer runs on a loaded host (CI exposed it).
const POLL: Duration = Duration::from_millis(1);

async fn envs(router: axum::Router, token: Option<&str>) -> serde_json::Value {
    let mut request = Request::get("/envs");
    if let Some(token) = token {
        request = request.header("X-Access-Token", token);
    }
    let response = router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}

async fn init_with(router: axum::Router, body: &str) -> StatusCode {
    router
        .oneshot(
            Request::post("/init")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn wait_env(router: &axum::Router, expected: &str) -> bool {
    for _ in 0..4000 {
        let values = envs(router.clone(), None).await;
        if values
            .get("E2B_EVENTS_ADDRESS")
            .and_then(|value| value.as_str())
            == Some(expected)
        {
            return true;
        }
        tokio::time::sleep(POLL).await;
    }
    false
}

async fn wait_for(mut probe: impl FnMut() -> bool) -> bool {
    for _ in 0..4000 {
        if probe() {
            return true;
        }
        tokio::time::sleep(POLL).await;
    }
    false
}

fn hosts(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("hosts")
}

fn seed(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(hosts(&dir), content).unwrap();
    dir
}

// --- engine: rewrite_hosts_file against an injected tempdir hosts file ---

#[test]
fn appends_normalized_line_and_preserves_unrelated_lines() {
    let dir = seed("\n# comment\n127.0.0.1        one.host\n127.0.0.2        two.host\n");
    rewrite_hosts_file("127.0.0.3", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"\n# comment\n127.0.0.1        one.host\n127.0.0.2        two.host\n127.0.0.3        events.e2b.local\n"
    );
}

#[test]
fn idempotent_same_address_and_family_writes_nothing() {
    let content = b"192.0.2.1        events.e2b.local\n".to_vec();
    let dir = seed(std::str::from_utf8(&content).unwrap());
    rewrite_hosts_file("192.0.2.1", &hosts(&dir)).unwrap();
    assert_eq!(std::fs::read(hosts(&dir)).unwrap(), content);
}

#[test]
fn same_family_replacement_removes_the_old_entry() {
    let dir = seed("1.2.3.4          events.e2b.local\n");
    rewrite_hosts_file("5.6.7.8", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"5.6.7.8          events.e2b.local\n"
    );
}

#[test]
fn different_family_entries_coexist() {
    let dir = seed("1.2.3.4          events.e2b.local\n");
    rewrite_hosts_file("::5", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"1.2.3.4          events.e2b.local\n::5              events.e2b.local\n"
    );
}

#[test]
fn localhost_new_address_keeps_existing_entry_before_appending() {
    let dir = seed("1.2.3.4          events.e2b.local\n");
    rewrite_hosts_file("127.0.0.3", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"1.2.3.4          events.e2b.local\n127.0.0.3        events.e2b.local\n"
    );
}

#[test]
fn same_address_merges_onto_the_existing_line() {
    let dir = seed("192.0.2.1        alpha.host\n1.2.3.4          events.e2b.local\n");
    rewrite_hosts_file("192.0.2.1", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"192.0.2.1        alpha.host events.e2b.local\n"
    );
}

#[test]
fn invalid_address_is_an_error_and_writes_nothing() {
    let content = "1.2.3.4          events.e2b.local\n";
    let dir = seed(content);
    let error = rewrite_hosts_file("events.e2b.local", &hosts(&dir)).unwrap_err();
    assert!(
        error.to_string().contains("failed to get ip family"),
        "{error}"
    );
    assert!(
        error.to_string().contains("failed to parse IP address"),
        "{error}"
    );
    let error = rewrite_hosts_file("", &hosts(&dir)).unwrap_err();
    assert!(
        error.to_string().contains("failed to parse IP address"),
        "{error}"
    );
    assert_eq!(std::fs::read(hosts(&dir)).unwrap(), content.as_bytes());
}

#[test]
fn missing_hosts_file_is_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = rewrite_hosts_file("127.0.0.3", &hosts(&dir)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("failed to create hosts: read hosts file"),
        "{error}"
    );
    assert!(!hosts(&dir).exists());
}

#[test]
fn comments_blank_and_unknown_lines_survive_and_address_lines_renormalize() {
    let dir =
        seed("# header\n\njustword\n127.0.0.9   spaced.host\n1.2.3.4          events.e2b.local\n");
    rewrite_hosts_file("192.0.2.7", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"# header\n\njustword\n127.0.0.9        spaced.host\n192.0.2.7        events.e2b.local\n"
    );
}

#[test]
fn get_ip_family_matches_go_netip_families() {
    assert!(matches!(get_ip_family("192.0.2.1").unwrap(), IPFamily::V4));
    assert!(matches!(get_ip_family("::5").unwrap(), IPFamily::V6));
    let error = get_ip_family("events.e2b.local").unwrap_err();
    assert!(error.to_string().contains("failed to parse IP address"));
}

#[test]
fn zone_ipv6_keeps_bytes_and_reports_ok() {
    let content = "1.2.3.4          events.e2b.local\n";
    let dir = seed(content);
    rewrite_hosts_file("fe80::1%eth0", &hosts(&dir)).unwrap();
    assert_eq!(std::fs::read(hosts(&dir)).unwrap(), content.as_bytes());
}

#[test]
fn empty_zone_address_is_an_error_and_writes_nothing() {
    let content = "127.0.0.1        localhost\n";
    let dir = seed(content);
    // Go netip.ParseAddr("fe80::1%") fails with "zone must be a non-empty
    // string" before any file IO, so the daemon only logs and stores no env.
    let error = rewrite_hosts_file("fe80::1%", &hosts(&dir)).unwrap_err();
    assert!(
        error.to_string().contains("failed to get ip family"),
        "{error}"
    );
    assert_eq!(std::fs::read(hosts(&dir)).unwrap(), content.as_bytes());
}

#[test]
fn bare_hash_comment_line_merges_and_renders_without_suffix() {
    let dir = seed("192.0.2.1        alpha.host #\n1.2.3.4          events.e2b.local\n");
    rewrite_hosts_file("192.0.2.1", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"192.0.2.1        alpha.host events.e2b.local\n"
    );
}

#[test]
fn ipv4_mapped_line_is_same_family_for_replacement() {
    let dir = seed("::ffff:192.0.2.1        events.e2b.local\n");
    rewrite_hosts_file("192.0.2.1", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"192.0.2.1        events.e2b.local\n"
    );
}

#[test]
fn crlf_and_trailing_blank_normalize_like_txeh() {
    let dir = seed("127.0.0.1        one.host\r\n\r\n");
    rewrite_hosts_file("127.0.0.2", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"127.0.0.1        one.host\n\n127.0.0.2        events.e2b.local\n"
    );
}

#[test]
fn multi_hostname_entry_rejoins_with_comment() {
    let dir = seed("192.0.2.1        alpha.host beta.host #\n1.2.3.4          events.e2b.local\n");
    rewrite_hosts_file("192.0.2.1", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"192.0.2.1        alpha.host beta.host events.e2b.local\n"
    );
}

#[test]
fn entry_with_real_comment_survives_rerender_with_suffix() {
    // txeh re-renders every parsed address line and keeps a non-empty comment
    // as " # note"; unrelated commented lines survive a rewrite untouched.
    let dir = seed("127.0.0.1        localhost\n192.0.2.1        host # note\n");
    rewrite_hosts_file("192.0.2.9", &hosts(&dir)).unwrap();
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"127.0.0.1        localhost\n192.0.2.1        host # note\n192.0.2.9        events.e2b.local\n"
    );
}

// --- daemon: SetData placement, gate, serialization, internal env ---

fn hyperloop_daemon(path: std::path::PathBuf) -> axum::Router {
    use coorworker_envd::daemon::{app, Daemon};
    app(Arc::new(Daemon::new().with_hyperloop_hosts_path(path)))
}

/// The pinned oracle reads an existing /etc/hosts; tests seed a tempdir one.
fn seed_hosts(path: &std::path::Path) {
    std::fs::write(path, "127.0.0.1        localhost\n").unwrap();
}

#[tokio::test]
async fn init_without_hyperloop_ip_touches_neither_hosts_nor_env() {
    let dir = tempfile::tempdir().unwrap();
    let router = hyperloop_daemon(hosts(&dir));
    assert_eq!(
        init_with(router.clone(), "{}").await,
        StatusCode::NO_CONTENT
    );
    assert!(!hosts(&dir).exists());
    assert!(envs(router, None).await.get("E2B_EVENTS_ADDRESS").is_none());
}

#[tokio::test]
async fn hyperloop_init_is_204_even_when_hosts_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let router = hyperloop_daemon(dir.path().join("absent/hosts"));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.5"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert!(envs(router, None).await.get("E2B_EVENTS_ADDRESS").is_none());
}

#[tokio::test]
async fn hyperloop_init_writes_entry_and_internal_env() {
    let dir = tempfile::tempdir().unwrap();
    seed_hosts(&hosts(&dir));
    let router = hyperloop_daemon(hosts(&dir));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.5"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert!(
        wait_for(|| {
            std::fs::read_to_string(hosts(&dir))
                .map(|data| data.contains("192.0.2.5        events.e2b.local"))
                .unwrap_or(false)
        })
        .await
    );
    assert!(wait_env(&router, "http://192.0.2.5").await);
}

#[tokio::test]
async fn serialized_inits_end_with_the_last_address_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    seed_hosts(&hosts(&dir));
    let router = hyperloop_daemon(hosts(&dir));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.10"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.20"}"#).await,
        StatusCode::NO_CONTENT
    );
    // The lock serializes the two rewrites, so whichever task acquires it last
    // is the last writer: exactly one events line survives and env and hosts
    // name the same address. Wait for both tasks to quiesce before asserting.
    let mut stable = 0;
    let mut last = String::new();
    for _ in 0..6000 {
        tokio::time::sleep(POLL).await;
        let values = envs(router.clone(), None).await;
        let current = values
            .get("E2B_EVENTS_ADDRESS")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let data = std::fs::read_to_string(hosts(&dir)).unwrap_or_default();
        let signature = format!("{current}|{data}");
        if signature == last {
            stable += 1;
        } else {
            stable = 0;
        }
        last = signature;
        if stable >= 300 {
            break;
        }
    }
    assert!(stable >= 300, "hyperloop tasks never quiesced: {last}");
    let (current, data) = last.split_once('|').unwrap();
    let address = current.trim_start_matches("http://");
    assert!(
        address == "192.0.2.10" || address == "192.0.2.20",
        "unexpected env {current}"
    );
    assert_eq!(data.matches("events.e2b.local").count(), 1, "{data}");
    // txeh renders "%-16s %s": one pad+separator gap of 17 - len spaces.
    let line = format!(
        "{address}{}events.e2b.local",
        " ".repeat(17 - address.len())
    );
    assert!(data.contains(&line), "{data}");
    assert_eq!(current, format!("http://{address}"));
}

#[tokio::test]
async fn user_env_vars_cannot_override_the_events_address() {
    let dir = tempfile::tempdir().unwrap();
    seed_hosts(&hosts(&dir));
    let router = hyperloop_daemon(hosts(&dir));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.5"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert!(wait_for(|| { std::fs::read(hosts(&dir)).is_ok() }).await);
    assert_eq!(
        init_with(
            router.clone(),
            r#"{"envVars":{"E2B_EVENTS_ADDRESS":"http://evil"}}"#
        )
        .await,
        StatusCode::NO_CONTENT
    );
    // The rejection is applied by the same background writer that publishes
    // the daemon's own value, so poll for the settled value instead of racing
    // it with a fixed sleep (which failed under CI load).
    let mut values = serde_json::Value::Null;
    for _ in 0..200 {
        values = envs(router.clone(), None).await;
        if values["E2B_EVENTS_ADDRESS"] == "http://192.0.2.5" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(values["E2B_EVENTS_ADDRESS"], "http://192.0.2.5");
}

#[tokio::test]
async fn repeated_identical_hyperloop_init_keeps_env_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    seed_hosts(&hosts(&dir));
    let router = hyperloop_daemon(hosts(&dir));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.5"}"#).await,
        StatusCode::NO_CONTENT
    );
    // Wait until the spawned writer actually landed the entry (not just any
    // readable file) before pinning the bytes: the read succeeds trivially
    // once seeded, so the old is_ok() check raced the writer under load.
    assert!(
        wait_for(|| std::fs::read(hosts(&dir))
            .map(|content| content.windows(9).any(|w| w == b"192.0.2.5"))
            .unwrap_or(false))
        .await
    );
    let before = std::fs::read(hosts(&dir)).unwrap();
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.5"}"#).await,
        StatusCode::NO_CONTENT
    );
    for _ in 0..2000 {
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        envs(router.clone(), None).await["E2B_EVENTS_ADDRESS"],
        "http://192.0.2.5"
    );
    assert_eq!(std::fs::read(hosts(&dir)).unwrap(), before);
}

use std::sync::Arc;
#[tokio::test]
async fn zone_ipv6_stores_env_and_keeps_bytes() {
    let dir = tempfile::tempdir().unwrap();
    seed_hosts(&hosts(&dir));
    let before = std::fs::read(hosts(&dir)).unwrap();
    let router = hyperloop_daemon(hosts(&dir));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"fe80::1%eth0"}"#).await,
        StatusCode::NO_CONTENT
    );
    assert!(wait_env(&router, "http://fe80::1%eth0").await);
    assert_eq!(std::fs::read(hosts(&dir)).unwrap(), before);
}

#[tokio::test]
async fn empty_zone_init_stores_no_env() {
    let dir = tempfile::tempdir().unwrap();
    seed_hosts(&hosts(&dir));
    let before = std::fs::read(hosts(&dir)).unwrap();
    let router = hyperloop_daemon(hosts(&dir));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"fe80::1%"}"#).await,
        StatusCode::NO_CONTENT
    );
    // Quiesce like the serialized test: Go fails the task at getIPFamily
    // ("zone must be a non-empty string") before any file IO, so neither env
    // nor a write may ever appear.
    let mut stable = 0;
    let mut last = String::new();
    for _ in 0..6000 {
        tokio::time::sleep(POLL).await;
        let values = envs(router.clone(), None).await;
        let current = values
            .get("E2B_EVENTS_ADDRESS")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let data = std::fs::read_to_string(hosts(&dir)).unwrap_or_default();
        let signature = format!("{current}|{data}");
        if signature == last {
            stable += 1;
        } else {
            stable = 0;
        }
        last = signature;
        if stable >= 300 {
            break;
        }
    }
    assert!(stable >= 300, "hyperloop task never quiesced: {last}");
    let (current, data) = last.split_once('|').unwrap();
    assert_eq!(current, "", "env must not be stored for an empty zone");
    assert_eq!(data, std::str::from_utf8(&before).unwrap());
}

#[tokio::test]
async fn daemon_without_builder_ignores_hyperloop_ip() {
    use coorworker_envd::daemon::{app, Daemon};
    let dir = tempfile::tempdir().unwrap();
    seed_hosts(&hosts(&dir));
    let router = app(Arc::new(Daemon::new()));
    assert_eq!(
        init_with(router.clone(), r#"{"hyperloopIP":"192.0.2.5"}"#).await,
        StatusCode::NO_CONTENT
    );
    for _ in 0..2000 {
        tokio::time::sleep(POLL).await;
    }
    assert!(envs(router.clone(), None)
        .await
        .get("E2B_EVENTS_ADDRESS")
        .is_none());
    assert_eq!(
        std::fs::read(hosts(&dir)).unwrap(),
        b"127.0.0.1        localhost\n"
    );
}
