//! `POST /upgrade` contract tests on Windows.
//!
//! The handler is the shared `crate::upgrade_http::upgrade`; these tests pin
//! the wire contract an SDK/orchestrator sees on the Windows daemon, with the
//! same status codes and bodies the Linux daemon produces. They run in the
//! `windows-test` job on `windows-latest`; a Linux host can only type-check
//! them (no MSVC linker).
use super::*;

/// The handover boot answers `401 envd not initialized` for `/upgrade`, while
/// `/health` and `/init` stay reachable (auth.rs's pre-init gate).
#[tokio::test]
async fn post_upgrade_on_a_handover_boot_is_401() {
    let daemon = Arc::new(Daemon::new().with_handover_auth().await);
    let running = start_daemon(daemon).await;

    let (status, headers, body) = send(
        running.address,
        Method::POST,
        "/upgrade",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    let envelope: Value = serde_json::from_slice(&body).expect("json envelope");
    assert_eq!(envelope["code"], 401);
    assert_eq!(envelope["message"], "envd not initialized");

    // The gate leaves exactly GET /health and POST /init reachable.
    let (status, _, _) = send(running.address, Method::GET, "/health", &[], b"").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/json"),
        ],
        br#"{"accessToken":"contract-token"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// The next `/init` after a resume boot advertises the outcome on every
/// response, including an error one.
#[tokio::test]
async fn init_after_handover_boot_reports_the_handover_header() {
    let daemon = Arc::new(Daemon::new().with_handover_auth().await);
    daemon.set_handover_result(crate::upgrade::HandoverOutcome {
        failed: false,
        procs: 3,
        procs_failed: 1,
        retained: 2,
        retained_failed: 0,
        watchers: 4,
        watchers_failed: 1,
    });
    let running = start_daemon(daemon).await;

    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/json"),
        ],
        br#"{"accessToken":"contract-token"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_handover_header(&headers);

    // A malformed body is a 400 and still carries the header (Go sets it on
    // every /init response, not only the success path).
    let (status, headers, _) = send(
        running.address,
        Method::POST,
        "/init",
        &[
            ("x-access-token", TOKEN),
            ("content-type", "application/json"),
        ],
        b"not json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_handover_header(&headers);
    assert!(headers.contains_key("x-envd-version"));
    assert!(headers.contains_key("x-envd-memory"));
}

fn assert_handover_header(headers: &HeaderMap) {
    let value = headers
        .get("x-envd-handover")
        .expect("the handover outcome rides every /init response");
    let outcome: Value = serde_json::from_str(value.to_str().expect("utf8")).expect("json");
    assert_eq!(
        outcome,
        json!({
            "failed": false,
            "procs": 3,
            "procs_failed": 1,
            "retained": 2,
            "retained_failed": 0,
            "watchers": 4,
            "watchers_failed": 1,
        })
    );
}

#[tokio::test]
async fn upgrade_before_init_is_conflict() {
    let running = start().await;
    let (status, headers, body) = send(running.address, Method::POST, "/upgrade", &[], b"").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(body, b"envd not initialized\n");
}

#[tokio::test]
async fn upgrade_foreign_target_is_bad_request() {
    let running = initialized().await;
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/upgrade",
        &[
            ("x-access-token", TOKEN),
            ("x-envd-upgrade-bin", r"C:\other\envd.exe"),
        ],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, b"unsupported upgrade target\n");
}

#[tokio::test]
async fn post_upgrade_without_a_wired_run_is_500() {
    let running = initialized().await;
    let (status, headers, body) = send(
        running.address,
        Method::POST,
        "/upgrade",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(body, b"upgrade is not wired\n");
}

#[tokio::test]
async fn upgrade_run_returns_ok_is_a_500() {
    let daemon = Arc::new(Daemon::new());
    daemon.set_upgrade_run(Arc::new(|_new_bin: Option<String>| {
        Box::pin(async { Ok(()) })
    }));
    let running = start_daemon(daemon).await;
    init_json(running.address, json!({"accessToken": TOKEN})).await;
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/upgrade",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, b"upgrade returned without replacing the process\n");
}

#[tokio::test]
async fn upgrade_failure_answers_the_run_error_and_keeps_serving() {
    let daemon = Arc::new(Daemon::new());
    daemon.set_upgrade_run(Arc::new(|_new_bin: Option<String>| {
        Box::pin(async { Err("boom".to_owned()) })
    }));
    let running = start_daemon(daemon).await;
    init_json(running.address, json!({"accessToken": TOKEN})).await;
    let (status, _, body) = send(
        running.address,
        Method::POST,
        "/upgrade",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, b"boom\n");
    let (status, _, _) = send(
        running.address,
        Method::GET,
        "/health",
        &[("x-access-token", TOKEN)],
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
