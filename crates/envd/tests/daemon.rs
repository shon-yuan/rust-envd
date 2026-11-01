use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::daemon::{app, Daemon};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn init_matches_arbitrary_case_and_duplicate_fields_in_wire_order() {
    for body in [
        r#"{"aCcEsStOkEn":"installed","dEfAuLtUsEr":"guest","DEFAULTWORKDIR":"/tmp","EnVvArS":{"A":"1"}}"#,
        r#"{"accessToken":"discarded","ACCESSTOKEN":"installed","defaultUser":"old","DEFAULTUSER":"guest","defaultWorkdir":"/old","DEFAULTWORKDIR":"/tmp","envVars":{"OLD":"1"},"ENVVARS":{"A":"1"}}"#,
        r#"{"ACCESSTOKEN":"discarded","accessToken":"installed","accessToken":"installed","defaultUser":"guest","defaultWorkdir":"/tmp","envVars":{"A":"1"}}"#,
        r#"{"acceſſToKen":"installed","defaultUſer":"guest","defaultWorKdir":"/tmp","envVarſ":{"A":"1"}}"#,
    ] {
        let router = app(Arc::new(Daemon::new()));
        let response = router
            .clone()
            .oneshot(
                Request::post("/init")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
        let defaults: serde_json::Value =
            serde_json::from_slice(response.headers()["x-envd-defaults"].as_bytes()).unwrap();
        assert_eq!(
            defaults,
            json!({"user":"guest","workdir":"/tmp","fallback":false})
        );
        assert_eq!(
            router
                .clone()
                .oneshot(Request::get("/envs").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let response = router
            .oneshot(
                Request::get("/envs")
                    .header("x-access-token", "installed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let envs: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024).await.unwrap()).unwrap();
        assert_eq!(envs["A"], "1");
    }
}

#[tokio::test]
async fn case_variants_cannot_bypass_token_validation_or_pending_effects() {
    for (body, status) in [
        (r#"{"ACCESSTOKEN":""}"#, StatusCode::BAD_REQUEST),
        (r#"{"aCcEsStOkEn":"escaped\n"}"#, StatusCode::BAD_REQUEST),
        (
            r#"{"accessToken":"","ACCESSTOKEN":"later"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"accessToken":"escaped\n","ACCESSTOKEN":"later"}"#,
            StatusCode::BAD_REQUEST,
        ),
        // The clock slice replaced the staging 501: a null timestamp is an
        // absent *time.Time in Go api.gen.go:386, so the init proceeds
        // (tests/clock.rs pins the full timestamp contract).
        (r#"{"TiMeStAmP":null}"#, StatusCode::NO_CONTENT),
        (r#"{"cPuCoUnT":2}"#, StatusCode::NO_CONTENT),
    ] {
        let response = app(Arc::new(Daemon::new()))
            .oneshot(
                Request::post("/init")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{body}");
    }
}

#[tokio::test]
async fn empty_cabundle_is_absent_not_a_pending_effect() {
    // Go init.go:474: `data.CaBundle != nil && *data.CaBundle != ""` — an
    // empty string skips the install entirely, so the init must proceed, not
    // answer the staging 501.
    let response = app(Arc::new(Daemon::new()))
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"caBundle":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn null_and_empty_volume_mounts_are_not_a_pending_effect() {
    // Go init.go:480 gates on `data.VolumeMounts != nil` (a *[]VolumeMount:
    // JSON null decodes to a nil pointer) and init.go:796-874's setupNFS
    // mounts nothing for an empty slice — both answer a successful /init.
    // Only a NON-empty list reaches the unimplemented NFS staging refusal.
    let daemon = Arc::new(Daemon::new());
    for body in [r#"{"volumeMounts":null}"#, r#"{"volumeMounts":[]}"#] {
        let response = app(daemon.clone())
            .clone()
            .oneshot(
                Request::post("/init")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
    }
}

#[tokio::test]
async fn a_non_empty_volume_mounts_list_remains_refused_and_scalars_fail_decode() {
    // A non-empty list reaches the still-blocked NFS staging refusal (501),
    // and anything that is not an array (or null) fails the whole-body decode
    // the way a Go *[]VolumeMount would (api.gen.go), answering 400.
    for (body, status) in [
        (
            r#"{"volumeMounts":[{"path":"/data","lifecycleId":"l"}]}"#,
            StatusCode::NOT_IMPLEMENTED,
        ),
        (r#"{"volumeMounts":42}"#, StatusCode::BAD_REQUEST),
        (r#"{"volumeMounts":"[]"}"#, StatusCode::BAD_REQUEST),
    ] {
        let response = app(Arc::new(Daemon::new()))
            .oneshot(
                Request::post("/init")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{body}");
    }
}

#[tokio::test]
async fn non_string_cabundle_is_a_decode_error_not_a_pending_effect() {
    // Go api.gen.go:362 decodes caBundle as *string: any other JSON type
    // fails the whole /init as a 400 before any effect is considered.
    let response = app(Arc::new(Daemon::new()))
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"caBundle":42}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn daemon_bootstrap_sets_defaults_and_enforces_body_token() {
    let state = Arc::new(Daemon::new());
    let router = app(state);
    let health = router
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::NO_CONTENT);
    assert_eq!(health.headers()["cache-control"], "no-store");
    assert!(!health.headers().contains_key("content-type"));
    let init = json!({"accessToken":"daemon-test-token", "envVars":{"VALUE":"雪"}, "defaultUser":"root", "defaultWorkdir":"/tmp"});
    let request = || {
        Request::post("/init")
            .header("content-type", "application/json")
            .body(Body::from(init.to_string()))
            .unwrap()
    };
    let response = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["x-envd-version"], "0.10.2");
    let memory: serde_json::Value =
        serde_json::from_slice(response.headers()["x-envd-memory"].as_bytes()).unwrap();
    assert!(memory["request"].is_u64());
    assert!(memory["low"].is_u64());
    assert!(memory["floor"].is_u64());
    assert!(memory["partial"].is_boolean());
    let defaults: serde_json::Value =
        serde_json::from_slice(response.headers()["x-envd-defaults"].as_bytes()).unwrap();
    assert_eq!(
        defaults,
        json!({"user":"root","workdir":"/tmp","fallback":false})
    );
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    for body in ["{}", r#"{"accessToken":"wrong"}"#] {
        let response = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let denied = router
        .clone()
        .oneshot(Request::get("/envs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let envs = router
        .oneshot(
            Request::get("/envs")
                .header("x-access-token", "daemon-test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(envs.status(), StatusCode::OK);
    let body = to_bytes(envs.into_body(), 1024).await.unwrap();
    let envs: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envs["VALUE"], "雪");
}

#[tokio::test]
async fn empty_init_and_environment_replacement_preserve_internal_values() {
    let router = app(Arc::new(Daemon::new()));
    for body in ["", "null", "{}"] {
        let response = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let defaults: serde_json::Value =
            serde_json::from_slice(response.headers()["x-envd-defaults"].as_bytes()).unwrap();
        assert_eq!(defaults, json!({"user":"root","fallback":true}));
    }
    for body in [r#"{"envVars":{"ONE":"1"}}"#, "{}", r#"{"envVars":{}}"#] {
        assert_eq!(
            router
                .clone()
                .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    let response = router
        .oneshot(Request::get("/envs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let values: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024).await.unwrap()).unwrap();
    assert_eq!(values, json!({"E2B_SANDBOX":"true"}));
}
#[tokio::test]
async fn init_rejects_empty_escaped_token_and_malformed_json_with_version_header() {
    let router = app(Arc::new(Daemon::new()));
    for body in [
        "{",
        r#"{"accessToken":""}"#,
        r#"{"accessToken":"escaped\n"}"#,
    ] {
        let response = router
            .clone()
            .oneshot(Request::post("/init").body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-envd-version"], "0.10.2");
    }
}

/// Go main.go:427-433: the /upgrade endpoint refuses upgrades until the first
/// authenticated /init — a 409 whose body is Go's http.Error plain text —
/// because a re-exec'd envd serves before /init with its token cleared, and a
/// guest process must not drive an unauthenticated upgrade in that window.
#[tokio::test]
async fn upgrade_endpoint_conflicts_until_initialized() {
    let router = app(Arc::new(Daemon::new()));
    let response = router
        .oneshot(
            Request::post("/upgrade")
                .header("x-access-token", "ignored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"envd not initialized\n");
}

/// Go main.go:441-445: the delivered binary is always exec'd from the fixed
/// path, so a caller asking for any other target is refused with a 400 before
/// any side effect.
#[tokio::test]
async fn upgrade_endpoint_refuses_a_foreign_target() {
    let daemon = Arc::new(Daemon::new());
    let router = app(daemon);
    // The orchestrator only triggers /upgrade on an initialized envd, so the
    // 400 path is reached through a real first /init (main.go:427-433).
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"installed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = router
        .oneshot(
            Request::post("/upgrade")
                // /upgrade is NOT in Go's authExcludedPaths (auth.go:31-38): the
                // orchestrator's delivery carries the token, like any secured
                // route (auth.go:56-66).
                .header("x-access-token", "installed")
                .header("x-envd-upgrade-bin", "/bin/sh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"unsupported upgrade target\n");
}

/// Go store.go:135-149 + init.go:211-218: an envd that booted from a
/// live-upgrade handover advertises the outcome on EVERY /init response
/// through the X-Envd-Handover JSON header, error and stale responses
/// included, and an envd that did not boot from one carries no header.
#[tokio::test]
async fn init_advertises_the_handover_outcome_header() {
    let plain = app(Arc::new(Daemon::new()));
    let response = plain
        .oneshot(Request::post("/init").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(response.headers().get("x-envd-handover").is_none());

    let daemon = Arc::new(Daemon::new());
    daemon.set_handover_result(coorworker_envd::upgrade::HandoverOutcome {
        failed: true,
        procs: 2,
        procs_failed: 1,
        retained: 1,
        retained_failed: 0,
        watchers: 3,
        watchers_failed: 3,
    });
    let router = app(daemon);
    // Error responses carry it too (init.go:211-218 runs before any gate).
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers()["x-envd-handover"],
        r#"{"failed":true,"procs":2,"procs_failed":1,"retained":1,"retained_failed":0,"watchers":3,"watchers_failed":3}"#
    );
    let response = router
        .oneshot(Request::post("/init").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response.headers().get("x-envd-handover").is_some());
}
