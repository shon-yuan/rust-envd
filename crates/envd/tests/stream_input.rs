use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use coorworker_envd::{
    daemon::{app_with_execution, Daemon},
    exec_context::{Account, Defaults, Environment},
    process::ProcessRuntime,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn runtime() -> ProcessRuntime {
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    assert!(!user.uid.is_root());
    let runtime = ProcessRuntime::new(
        Account {
            name: user.name,
            home: user.dir.to_string_lossy().into_owned(),
            uid: user.uid.as_raw(),
            gid: user.gid.as_raw(),
            groups: nix::unistd::getgroups()
                .unwrap()
                .iter()
                .map(|gid| gid.as_raw())
                .collect(),
        },
        Defaults::default(),
        Environment::default(),
        std::env::var("PATH").ok(),
    )
    .unwrap();
    runtime
}
fn app() -> Router {
    app_with_execution(Arc::new(Daemon::new()), runtime())
}
fn frame(value: Value) -> Vec<u8> {
    let bytes = value.to_string().into_bytes();
    let mut frame = vec![0];
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend(bytes);
    frame
}
async fn input(app: &Router, bytes: Vec<u8>) -> Vec<(u8, Value)> {
    input_body(app, Body::from(bytes)).await
}
async fn input_body(app: &Router, body: Body) -> Vec<(u8, Value)> {
    let response = app
        .clone()
        .oneshot(
            Request::post("/process.Process/StreamInput")
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let mut remaining = bytes.as_ref();
    let mut result = Vec::new();
    while !remaining.is_empty() {
        let len = u32::from_be_bytes(remaining[1..5].try_into().unwrap()) as usize;
        result.push((
            remaining[0],
            serde_json::from_slice(&remaining[5..5 + len]).unwrap(),
        ));
        remaining = &remaining[5 + len..];
    }
    result
}
#[tokio::test]
async fn stream_selection_keeps_identity_when_tag_is_reused() {
    use coorworker_envd::proto::process::{self as pb, process_event::Event};
    let runtime = runtime();
    let start: pb::StartRequest = serde_json::from_value(
        json!({"process":{"cmd":"/bin/cat"},"stdin":true,"tag":"shared-stream"}),
    )
    .unwrap();
    let mut first = runtime
        .start(start.clone(), Some(std::time::Duration::from_secs(5)))
        .await
        .unwrap();
    let pid = match first.recv().await.unwrap().event.unwrap() {
        Event::Start(event) => event.pid,
        other => panic!("{other:?}"),
    };
    let app = app_with_execution(Arc::new(Daemon::new()), runtime.clone());
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(1);
    let body = Body::from_stream(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move { receiver.recv().await.map(|value| (value, receiver)) },
    ));
    let task = tokio::spawn(async move { input_body(&app, body).await });
    sender
        .send(Ok(frame(
            json!({"start":{"process":{"tag":"shared-stream"}}}),
        )))
        .await
        .unwrap();
    sender
        .send(Ok(frame(
            json!({"data":{"input":{"stdin":"U1RBUlRfQk9VTkQK"}}}),
        )))
        .await
        .unwrap();
    let mut output = Vec::new();
    while output != b"START_BOUND\n" {
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), first.recv())
            .await
            .unwrap()
            .unwrap();
        if let Some(Event::Data(data)) = event.event {
            if let Some(pb::process_event::data_event::Output::Stdout(bytes)) = data.output {
                output.extend(bytes);
            }
        }
    }
    runtime
        .close_stdin(&serde_json::from_value(json!({"pid":pid})).unwrap())
        .await
        .unwrap();
    while first.recv().await.is_some() {}
    let mut second = runtime
        .start(start, Some(std::time::Duration::from_secs(5)))
        .await
        .unwrap();
    let pid = match second.recv().await.unwrap().event.unwrap() {
        Event::Start(event) => event.pid,
        other => panic!("{other:?}"),
    };
    sender
        .send(Ok(frame(json!({"data":{"input":{"stdin":"eA=="}}}))))
        .await
        .unwrap();
    drop(sender);
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    runtime
        .close_stdin(&serde_json::from_value(json!({"pid":pid})).unwrap())
        .await
        .unwrap();
    while second.recv().await.is_some() {}
    assert_eq!(
        result,
        vec![(
            2,
            json!({"error":{"code":"failed_precondition","message":"error writing to stdin: stdin not enabled or closed"}})
        )]
    );
}

#[tokio::test]
async fn invalid_start_is_resolved_before_eof_and_following_bad_data() {
    for selector in [
        json!({"pid":999999}),
        json!({"tag":"missing-stream-tag"}),
        json!({}),
    ] {
        let expected = if selector.get("pid").is_some() {
            json!({"code":"not_found","message":"process with pid 999999 not found"})
        } else if selector.get("tag").is_some() {
            json!({"code":"not_found","message":"process with tag missing-stream-tag not found"})
        } else {
            json!({"code":"unimplemented","message":"invalid input type *process.ProcessSelector"})
        };
        for trailing in [Vec::new(), vec![0, 0, 0, 0, 1, b'x']] {
            let mut bytes = frame(json!({"start":{"process":selector}}));
            bytes.extend(trailing);
            assert_eq!(
                input(&app(), bytes).await,
                vec![(2, json!({"error":expected}))]
            );
        }
    }
}

#[tokio::test]
async fn selected_process_input_errors_match_go() {
    let runtime = runtime();
    let mut events = runtime
        .start(
            serde_json::from_value(json!({"process":{"cmd":"/bin/sh","args":["-c","/bin/cat; /bin/sleep 2"]},"stdin":true})).unwrap(),
            Some(std::time::Duration::from_secs(3)),
        )
        .await
        .unwrap();
    let pid = match events.recv().await.unwrap().event.unwrap() {
        coorworker_envd::proto::process::process_event::Event::Start(event) => event.pid,
        event => panic!("unexpected {event:?}"),
    };
    let selector = serde_json::from_value(json!({"pid":pid})).unwrap();
    runtime.close_stdin(&selector).await.unwrap();
    let app = app_with_execution(Arc::new(Daemon::new()), runtime);
    for (value, code, message) in [
        (json!({}), "unimplemented", "invalid input type <nil>"),
        (
            json!({"pty":"eA=="}),
            "failed_precondition",
            "error writing to tty: tty not assigned to process — input should be written to the stdin, not the tty",
        ),
        (
            json!({"stdin":"eA=="}),
            "failed_precondition",
            "error writing to stdin: stdin not enabled or closed",
        ),
    ] {
        let mut bytes = frame(json!({"start":{"process":{"pid":pid}}}));
        bytes.extend(frame(json!({"data":{"input":value}})));
        assert_eq!(
            input(&app, bytes).await,
            vec![(2, json!({"error":{"code":code,"message":message}}))]
        );
    }
    while events.recv().await.is_some() {}
}

#[tokio::test]
async fn empty_keepalive_and_missing_event_match_go() {
    let app = app();
    for bytes in [Vec::new(), frame(json!({"keepalive":{}}))] {
        assert_eq!(
            input(&app, bytes).await,
            vec![(0, json!({})), (2, json!({}))]
        );
    }
    assert_eq!(
        input(&app, frame(json!({}))).await,
        vec![(
            2,
            json!({"error":{"code":"unimplemented","message":"invalid event type <nil>"}})
        )]
    );
}
#[tokio::test]
async fn invalid_json_keeps_go_unknown_wrapper_and_diagnostics() {
    let message = "error streaming input: invalid_argument: unmarshal message: unmarshal into *process.StreamInputRequest: proto: syntax error (line 1:1): invalid value x";
    assert_eq!(
        input(&app(), vec![0, 0, 0, 0, 1, b'x']).await,
        vec![(2, json!({"error":{"code":"unknown","message":message}}))]
    );
}
#[tokio::test]
async fn truncated_envelope_reports_promised_and_received_bytes() {
    let message = "error streaming input: invalid_argument: protocol error: promised 5 bytes in enveloped message, got 2 bytes";
    assert_eq!(
        input(&app(), vec![0, 0, 0, 0, 5, b'{', b'}']).await,
        vec![(2, json!({"error":{"code":"unknown","message":message}}))]
    );
}
