//! The decode contract of `volumeMounts` (and `lifecycleID`) on /init,
//! against the pinned oracle: Go decodes the whole body into
//! InitRequest (internal/api/init.go:250, api.gen.go:386-387) before any
//! volume work, so a wrong JSON type anywhere inside those fields fails
//! the whole request as 400 with an empty body — no executor effect at
//! all (internal/api/init.go:480-484).

use std::sync::{Arc, Mutex};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::daemon::nfs::{BoxFut, CommandOutcome, NfsMounter};
use coorworker_envd::daemon::{app, Daemon};
use tower::ServiceExt;

async fn init(router: &axum::Router, body: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// A recording mounter: the decode-failure contract is that the executor
/// is never reached, so every method just notes the call and answers a
/// default that would fail loudly if it ever ran.
#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Vec<&'static str>>>);

impl Recording {
    fn calls(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

impl NfsMounter for Recording {
    fn mount(
        &self,
        _nfs_target: &str,
        _path: &str,
        _deadline: tokio::time::Instant,
    ) -> BoxFut<'_, CommandOutcome> {
        self.0.lock().unwrap().push("mount");
        Box::pin(async {
            CommandOutcome {
                error: Some("decode test: mount must not run".to_owned()),
                deadline_error: false,
            }
        })
    }

    fn make_dir(&self, _path: &str, _deadline: tokio::time::Instant) -> BoxFut<'_, CommandOutcome> {
        self.0.lock().unwrap().push("mkdir");
        Box::pin(async {
            CommandOutcome {
                error: Some("decode test: mkdir must not run".to_owned()),
                deadline_error: false,
            }
        })
    }

    fn unmount(&self, _path: &str) -> BoxFut<'_, Result<(), String>> {
        self.0.lock().unwrap().push("unmount");
        Box::pin(async { Err("decode test: unmount must not run".to_owned()) })
    }

    fn mount_id(&self, _path: &str) -> BoxFut<'_, Result<u64, String>> {
        self.0.lock().unwrap().push("mount_id");
        Box::pin(async { Err("decode test: mount_id must not run".to_owned()) })
    }
}

fn decode_daemon(recorder: &Recording) -> axum::Router {
    app(Arc::new(
        Daemon::new().with_nfs_mounter(Arc::new(recorder.clone())),
    ))
}

/// Every wrong JSON type under volumeMounts (or in lifecycleID, decoded
/// by the same body pass) kills the whole /init as 400, empty body, no
/// X-Envd-Failed-Volume and no Content-Type, and the mount executor is
/// never touched (Go api.gen.go:386-387 + encoding/json type errors;
/// internal/api/init.go:250 fails the request before SetData).
#[tokio::test]
async fn malformed_volume_mounts_types_are_400_with_empty_body() {
    let recorder = Recording::default();
    let router = decode_daemon(&recorder);
    for body in [
        // Scalar/object/bool where *[]VolumeMount is expected.
        r#"{"volumeMounts":42}"#,
        r#"{"volumeMounts":"[]"}"#,
        r#"{"volumeMounts":true}"#,
        r#"{"volumeMounts":{}}"#,
        // Wrong types inside the slice: element, path, nfs_target.
        r#"{"volumeMounts":[42]}"#,
        r#"{"volumeMounts":[{"path":42}]}"#,
        r#"{"volumeMounts":[{"nfs_target":42,"path":"/data"}]}"#,
        // lifecycleID rides the same whole-body decode (api.gen.go:380).
        r#"{"lifecycleID":42}"#,
    ] {
        let response = init(&router, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert!(
            response.headers().get("x-envd-failed-volume").is_none(),
            "no volume has failed yet: {body}"
        );
        assert!(
            response.headers().get("content-type").is_none(),
            "the decode error body is empty bytes, so no content type: {body}"
        );
        let bytes = to_bytes(response.into_body(), 16384).await.unwrap();
        assert!(bytes.is_empty(), "expected an empty body: {body}");
    }
    assert!(
        recorder.calls().is_empty(),
        "a decode failure precedes every executor effect, saw {:?}",
        recorder.calls()
    );
}
