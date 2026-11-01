use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::daemon::{app, Daemon};
use serde_json::{json, Value};
use tower::ServiceExt;

struct FixedCpus;

impl coorworker_envd::cpus::CpuSysfs for FixedCpus {
    fn possible(&self) -> std::io::Result<Vec<usize>> {
        Ok(vec![0, 1, 2, 3])
    }
    fn online(&self) -> std::io::Result<Vec<usize>> {
        Ok(vec![0, 1])
    }
    fn set_online(&self, _: usize, _: bool) -> std::io::Result<()> {
        panic!("HTTP tests must not write CPU attributes")
    }
}

struct NoChildren;

impl coorworker_envd::cpus::ChildWrites for NoChildren {
    fn write(&self, _: &std::path::Path, _: bool) -> std::io::Result<()> {
        panic!("HTTP tests must not spawn CPU helpers")
    }
    fn wait_adopted(&self) {}
}

#[tokio::test]
async fn real_manager_reports_targets_and_rejections_without_failing_init() {
    let directory = tempfile::tempdir().unwrap();
    let target_file = directory.path().join("target");
    let manager = Arc::new(coorworker_envd::cpus::CpuManager::new(
        Arc::new(FixedCpus),
        Arc::new(NoChildren),
        target_file.clone(),
    ));
    let router = app(Arc::new(Daemon::with_cpu_manager(manager)));
    for (body, expected) in [
        (
            r#"{"cpuCount":3}"#,
            json!({"online":2,"possible":4,"target":3}),
        ),
        (
            r#"{"cpuCount":5,"envVars":{"CPU_INIT":"applied"}}"#,
            json!({"online":2,"possible":4,"target":3,"rejected":"cpu target 5 outside 1..4"}),
        ),
        ("{}", json!({"online":2,"possible":4,"target":3})),
    ] {
        let response = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
        let report: Value =
            serde_json::from_slice(response.headers()["x-envd-cpus"].as_bytes()).unwrap();
        assert_eq!(report, expected);
        assert_eq!(std::fs::read_to_string(&target_file).unwrap(), "3");
    }
    let response = router
        .clone()
        .oneshot(Request::get("/envs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let envs: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(envs["CPU_INIT"], "applied");
    let response = router
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let metrics: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(metrics["cpu_possible"], 4);
    assert_eq!(metrics["cpu_target"], 3);
    assert_eq!(metrics["cpu_target_attempts"], 0);
    assert_eq!(metrics["cpu_write_pending_ms"], 0);
}

#[tokio::test]
async fn noop_cpu_targets_accept_go_ints_and_report_zero_state() {
    for body in [
        "",
        "null",
        "{}",
        r#"{"cpuCount":null}"#,
        r#"{"cpuCount":0}"#,
        r#"{"cpuCount":-0}"#,
        r#"{"cPuCoUnT":-1}"#,
        r#"{"cpuCount":9223372036854775807}"#,
        r#"{"cpuCount":-9223372036854775808}"#,
        r#"{"cpuCount":2,"CPUCOUNT":null}"#,
        r#"{"cpuCount":null,"CPUCOUNT":3}"#,
    ] {
        let response = app(Arc::new(Daemon::new()))
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
        let report: Value = serde_json::from_slice(
            response
                .headers()
                .get("x-envd-cpus")
                .expect(body)
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            report,
            json!({"online":0,"possible":0,"target":0}),
            "{body}"
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn invalid_cpu_json_is_rejected_before_auth_or_configuration_changes() {
    let router = app(Arc::new(Daemon::new()));
    for body in [
        r#"{"cpuCount":true}"#,
        r#"{"cpuCount":"2"}"#,
        r#"{"cpuCount":2.0}"#,
        r#"{"cpuCount":2e0}"#,
        r#"{"cpuCount":9223372036854775808}"#,
        r#"{"cpuCount":[]}"#,
        r#"{"cpuCount":{}}"#,
        r#"{"cpuCount":false,"CPUCOUNT":2}"#,
        r#"{"accessToken":"must-not-install","cpuCount":false}"#,
    ] {
        let response = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(response.headers()["x-envd-version"], "0.10.2");
        assert!(!response.headers().contains_key("x-envd-cpus"));
        assert!(to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty());
    }
    let response = router
        .oneshot(Request::get("/envs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn cpu_target_does_not_bypass_init_token_validation() {
    let router = app(Arc::new(Daemon::new()));
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"cpu-test-token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    for (body, expected) in [
        (r#"{"cpuCount":2}"#, "access token reset not authorized"),
        (
            r#"{"accessToken":"wrong","cpuCount":2}"#,
            "access token validation failed",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!response.headers().contains_key("x-envd-cpus"));
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
        assert!(!response.headers().contains_key("x-content-type-options"));
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap(),
            expected
        );
    }
}
