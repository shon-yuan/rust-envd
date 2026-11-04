//! Environment-driven mode selection runs in isolated child test processes.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::{
    configuration::Configuration,
    ext::{
        cua::{CuaExtension, LaunchContext},
        Extension,
    },
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[tokio::test]
async fn explicit_modes_never_downgrade_to_legacy_or_tokenless_execution() {
    if let Ok(case) = std::env::var("CUA_FACTORY_TEST_CASE") {
        check_case(&case).await;
        return;
    }
    for case in [
        "legacy",
        "empty",
        "invalid",
        "embedded",
        "service",
        "external-missing",
        "external-invalid",
        "external-nonloopback",
        "external-down",
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "explicit_modes_never_downgrade_to_legacy_or_tokenless_execution",
                "--nocapture",
            ])
            .env_clear()
            .env("CUA_FACTORY_TEST_CASE", case)
            .env("TOKIO_WORKER_THREADS", "2");
        for name in ["PATH", "SystemRoot", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mode = match case {
            "legacy" => "legacy",
            "empty" => "",
            "invalid" => "invalid",
            "embedded" | "service" => "embedded",
            _ => "shared-external",
        };
        command.env("COORWORKER_CUA_MODE", mode);
        match case {
            "external-invalid" => {
                command.env("COORWORKER_CUA_LISTEN", "invalid");
            }
            "external-nonloopback" => {
                command
                    .env("COORWORKER_CUA_LISTEN", "0.0.0.0:3211")
                    .env("CUA_ENV_TOKEN", "internal-test");
            }
            "external-down" => {
                command
                    .env("COORWORKER_CUA_LISTEN", "127.0.0.1:0")
                    .env("CUA_ENV_TOKEN", "internal-test");
            }
            _ => {}
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

async fn check_case(case: &str) {
    let context = if case == "service" {
        LaunchContext::ExternalOnly
    } else {
        LaunchContext::DesktopSession
    };
    let extension =
        CuaExtension::from_env_with_context(Arc::new(Mutex::new(Configuration::new())), context)
            .await;
    if case == "legacy" {
        assert!(extension.is_none());
        return;
    }
    let extension = extension.unwrap();
    let expected_available = case == "embedded" && cfg!(feature = "cua-desktop");
    assert_eq!(
        extension.health().state == coorworker_envd::ext::HealthState::Healthy,
        expected_available
    );
    let router = extension.router();
    let response = router
        .clone()
        .oneshot(Request::post("/mcp").body(Body::from("{")).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router
        .oneshot(Request::get("/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let info: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(info["auth_source"], "envd");
    assert_eq!(info["auth_configured"], false);
    assert!(info.get("mcp_url").is_none());
}
