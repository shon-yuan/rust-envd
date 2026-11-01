use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use buffa::Message;
use coorworker_envd::{
    daemon::{app_with_filesystem, Daemon},
    proto::{filesystem as pb, legacyfilesystem as old},
};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

async fn request(
    router: &axum::Router,
    method: &str,
    value: serde_json::Value,
    legacy: bool,
    streaming: bool,
) -> axum::response::Response {
    let bytes = value.to_string().into_bytes();
    let body = if streaming {
        let mut frame = vec![0];
        frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        frame.extend(bytes);
        frame
    } else {
        bytes
    };
    router
        .clone()
        .oneshot(
            Request::post(format!("/filesystem.Filesystem/{method}"))
                .header(
                    "content-type",
                    if streaming {
                        "application/connect+json"
                    } else {
                        "application/json"
                    },
                )
                .header(
                    "user-agent",
                    if legacy {
                        "connect-python"
                    } else {
                        "connect-python/1.0"
                    },
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn next_frame(body: &mut Body, pending: &mut Vec<u8>) -> (u8, serde_json::Value) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if pending.len() >= 5 {
                let len = u32::from_be_bytes(pending[1..5].try_into().unwrap()) as usize;
                if pending.len() >= 5 + len {
                    let result = (
                        pending[0],
                        serde_json::from_slice(&pending[5..5 + len]).unwrap(),
                    );
                    pending.drain(..5 + len);
                    return result;
                }
            }
            let frame = body.frame().await.unwrap().unwrap();
            if let Ok(bytes) = frame.into_data() {
                pending.extend(bytes);
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn legacy_lists_watch_events_and_stream_errors_preserve_exact_header_rules() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data"), b"data").unwrap();
    let router = app_with_filesystem(Arc::new(Daemon::new()));
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(json!({"defaultUser":user.name}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let response = request(&router, "ListDir", json!({"path":dir.path()}), true, false).await;
    assert_eq!(response.headers()["x-e2b-legacy-sdk"], "true");
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value["entries"][0].as_object().unwrap().len(), 3);
    for legacy in [true, false] {
        let response = request(
            &router,
            "WatchDir",
            json!({"path":dir.path(),"includeEntry":true}),
            legacy,
            true,
        )
        .await;
        assert_eq!(response.headers().contains_key("x-e2b-legacy-sdk"), legacy);
        let mut body = response.into_body();
        let mut pending = Vec::new();
        assert!(next_frame(&mut body, &mut pending)
            .await
            .1
            .get("start")
            .is_some());
        let name = format!("new-{legacy}");
        std::fs::write(dir.path().join(&name), b"new").unwrap();
        loop {
            let (flags, value) = next_frame(&mut body, &mut pending).await;
            assert_eq!(flags, 0);
            if value["filesystem"]["name"] == name
                && value["filesystem"]["type"] == "EVENT_TYPE_CREATE"
            {
                assert_eq!(value["filesystem"].get("entry").is_none(), legacy);
                break;
            }
        }
        drop(body);
        let response = request(
            &router,
            "WatchDir",
            json!({"path":dir.path().join("missing")}),
            legacy,
            true,
        )
        .await;
        assert_eq!(response.headers().contains_key("x-e2b-legacy-sdk"), legacy);
        let mut body = response.into_body();
        let (flags, value) = next_frame(&mut body, &mut Vec::new()).await;
        assert_eq!(flags, 2);
        assert_eq!(value["error"]["code"], "not_found");
        let missing = dir.path().join("missing");
        assert_eq!(
            value["error"]["message"],
            format!(
                "path {} not found: stat {}: no such file or directory",
                missing.display(),
                missing.display()
            )
        );
    }
}

#[tokio::test]
async fn exact_legacy_user_agent_converts_json_and_protobuf_without_extra_entry_fields() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data");
    std::fs::write(&file, b"payload").unwrap();
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let router = app_with_filesystem(Arc::new(Daemon::new()));
    let init = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(json!({"defaultUser":user.name}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(init.status(), 204);
    for (agent, legacy) in [
        ("connect-python", true),
        ("connect-python/1.0", false),
        ("Connect-Python", false),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post("/filesystem.Filesystem/Stat")
                    .header("content-type", "application/json")
                    .header("user-agent", agent)
                    .body(Body::from(json!({"path":file}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers().get("x-e2b-legacy-sdk").is_some(), legacy);
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(value["entry"].get("size").is_none(), legacy);
        if legacy {
            assert_eq!(value["entry"].as_object().unwrap().len(), 3);
        }
    }
    let response = router
        .clone()
        .oneshot(
            Request::post("/filesystem.Filesystem/Stat")
                .header("content-type", "application/proto")
                .header("user-agent", "connect-python")
                .body(Body::from(
                    pb::StatRequest {
                        path: file.to_str().unwrap().into(),
                        ..Default::default()
                    }
                    .encode_to_vec(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-e2b-legacy-sdk"], "true");
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let legacy = old::StatResponse::decode_from_slice(&bytes).unwrap();
    assert_eq!(
        serde_json::to_value(legacy).unwrap()["entry"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
    let modern = pb::StatResponse::decode_from_slice(&bytes).unwrap();
    assert_eq!(modern.entry.size, 0);
    let response = router
        .oneshot(
            Request::post("/filesystem.Filesystem/Stat")
                .header("content-type", "application/json")
                .header("user-agent", "connect-python")
                .body(Body::from(
                    json!({"path":dir.path().join("missing")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert!(!response.headers().contains_key("x-e2b-legacy-sdk"));
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let missing = dir.path().join("missing");
    assert_eq!(
        value["message"],
        format!(
            "file not found: lstat {}: no such file or directory",
            missing.display()
        )
    );
}

/// Oracle conversion.go:103-127 (conversion_test.go:245-274): watcher responses
/// convert CreateWatcher/GetWatcherEvents payloads, keep the keepalive arm,
/// and pass RemoveWatcher through while still marking the response.
#[tokio::test]
async fn legacy_watcher_payloads_convert_and_remove_passes_through() {
    let dir = tempfile::tempdir().unwrap();
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let router = app_with_filesystem(Arc::new(Daemon::new()));
    let init = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(json!({"defaultUser":user.name}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(init.status(), 204);

    let response = request(
        &router,
        "CreateWatcher",
        json!({"path":dir.path(),"includeEntry":true}),
        true,
        false,
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-e2b-legacy-sdk"], "true");
    let created: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let watcher = created["watcherId"].as_str().unwrap().to_owned();

    std::fs::write(dir.path().join("new-file"), b"data").unwrap();

    let mut events = Vec::new();
    for _ in 0..60 {
        let response = request(
            &router,
            "GetWatcherEvents",
            json!({"watcherId":watcher}),
            true,
            false,
        )
        .await;
        assert_eq!(response.headers()["x-e2b-legacy-sdk"], "true");
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        events = value["events"].as_array().cloned().unwrap_or_default();
        if !events.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!events.is_empty(), "watcher never observed the new file");
    // Legacy FilesystemEvent carries only name and type (conversion.go:120-127).
    let event = events[0].as_object().unwrap();
    assert_eq!(
        event.len(),
        2,
        "legacy event leaked modern fields: {event:?}"
    );
    assert!(event.contains_key("name"));
    assert!(event.contains_key("type"));

    // Modern watchers keep the entry that includeEntry asked for.
    let response = request(
        &router,
        "CreateWatcher",
        json!({"path":dir.path(),"includeEntry":true}),
        false,
        false,
    )
    .await;
    let created: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let mut watcher = created["watcherId"].as_str().unwrap().to_owned();
    std::fs::write(dir.path().join("modern-file"), b"data").unwrap();
    let mut modern = Vec::new();
    for _ in 0..60 {
        let response = request(
            &router,
            "GetWatcherEvents",
            json!({"watcherId":watcher}),
            false,
            false,
        )
        .await;
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        modern = value["events"].as_array().cloned().unwrap_or_default();
        if !modern.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!modern.is_empty());
    assert!(
        modern[0].as_object().unwrap().contains_key("entry"),
        "modern watcher dropped includeEntry: {:?}",
        modern[0]
    );

    for legacy in [true, false] {
        if !legacy {
            // Each watcher can only be removed once; create a fresh one.
            let response = request(
                &router,
                "CreateWatcher",
                json!({"path":dir.path()}),
                false,
                false,
            )
            .await;
            let created: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap())
                    .unwrap();
            watcher = created["watcherId"].as_str().unwrap().to_owned();
        }
        let response = request(
            &router,
            "RemoveWatcher",
            json!({"watcherId":watcher}),
            legacy,
            false,
        )
        .await;
        assert_eq!(response.headers().contains_key("x-e2b-legacy-sdk"), legacy);
        assert_eq!(response.status(), 200);
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(value, json!({}));
    }
}
