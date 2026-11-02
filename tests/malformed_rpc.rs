//! Wire text for malformed Connect requests, byte-compared against the pinned
//! oracle corpus captured by `scripts/tests/envd-parity/malformed_vm.py`.
//!
//! The oracle's texts come from connect-go 1.18.1 plus
//! google.golang.org/protobuf's JSON decoder; the vendored connectrpc fork
//! (`vendor/connectrpc`) reproduces them. Every request here is malformed
//! before any handler action, so a daemon with its default runtime suffices.

use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use coorworker_envd::{
    daemon::{app_with_execution, Daemon},
    exec_context::{Account, Defaults, Environment},
    process::ProcessRuntime,
};
use std::sync::Arc;
use tower::ServiceExt;

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

async fn post(path: &str, content_type: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let app = app_with_execution(Arc::new(Daemon::new()), runtime());
    let response = app
        .oneshot(
            Request::post(path)
                .header("content-type", content_type)
                .header("connect-protocol-version", "1")
                .header("connect-timeout-ms", "1000")
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, bytes.to_vec())
}

/// Build the oracle's compact Connect error document, escaping the message
/// the way Go's `encoding/json` does: a replacement rune produced from
/// invalid UTF-8 becomes Go's six-character U+FFFD escape, while serde_json
/// would emit its raw UTF-8 bytes.
fn error_document(code: &str, message: &str) -> String {
    format!(
        "{{\"code\":\"{code}\",\"message\":{}}}",
        serde_json::to_string(message).unwrap()
    )
    .replace('\u{fffd}', "\\ufffd")
}

/// The oracle's `corpus.rpc` texts for a unary JSON procedure (List).
#[tokio::test]
async fn unary_json_decode_failures_carry_the_oracle_protojson_texts() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "List empty-body",
            Vec::new(),
            "unmarshal message: zero-length payload is not a valid JSON object",
        ),
        (
            "List bad-json",
            b"{".to_vec(),
            "unmarshal message: unmarshal into *process.ListRequest: proto: unexpected EOF",
        ),
        (
            "List bad-utf8",
            b"\xff\xfe\x00\x01".to_vec(),
            "unmarshal message: unmarshal into *process.ListRequest: \
             proto: syntax error (line 1:1): invalid value \u{fffd}",
        ),
        (
            "List garbage-bytes",
            b"not-json".to_vec(),
            "unmarshal message: unmarshal into *process.ListRequest: \
             proto: syntax error (line 1:1): invalid value not-json",
        ),
        (
            "List frame-bytes",
            b"\x00\x00\x00\x00\x02{}".to_vec(),
            "unmarshal message: unmarshal into *process.ListRequest: \
             proto: syntax error (line 1:1): invalid value \u{0}",
        ),
        (
            "List oversized-body",
            vec![b'A'; 256 * 1024],
            "unmarshal message: unmarshal into *process.ListRequest: \
             proto: syntax error (line 1:1): \
             invalid value AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ),
    ];
    for (case, body, message) in cases {
        let (status, response) = post("/process.Process/List", "application/json", &body).await;
        assert_eq!(status, 400, "{case}");
        assert_eq!(
            response,
            error_document("invalid_argument", message).into_bytes(),
            "{case}"
        );
    }
}

/// The oracle's `corpus.rpc` texts for the server-streaming envelope cases.
#[tokio::test]
async fn streaming_envelope_failures_carry_the_oracle_connect_texts() {
    let oversized_claim = [b"\x00\x00\x10\x00\x00".as_slice(), &[b'\x08'; 1024][..]].concat();
    let cases: Vec<(&str, Vec<u8>, &str, &str)> = vec![
        (
            "Connect empty-body",
            Vec::new(),
            "unimplemented",
            "unary request has zero messages",
        ),
        (
            "Connect truncated-frame",
            b"\x00\x00\x00\x00\x64\x08\x01".to_vec(),
            "invalid_argument",
            "protocol error: promised 100 bytes in enveloped message, got 2 bytes",
        ),
        (
            "Connect bad-flags",
            b"\x63\x00\x00\x00\x02\x08\x01".to_vec(),
            "internal",
            "protocol error: sent compressed message without compression support",
        ),
        (
            "Connect bad-wire-type",
            b"\x00\x00\x00\x00\x04\x0f\xff\xff\xff".to_vec(),
            "invalid_argument",
            "unmarshal message: unmarshal into *process.ConnectRequest: \
             proto: syntax error (line 1:1): invalid value \u{f}",
        ),
        (
            "Connect oversized-claim",
            oversized_claim,
            "invalid_argument",
            "protocol error: promised 1048576 bytes in enveloped message, got 1024 bytes",
        ),
        (
            "Connect garbage-bytes",
            b"not-a-frame".to_vec(),
            "invalid_argument",
            "protocol error: promised 1869884769 bytes in enveloped message, got 6 bytes",
        ),
    ];
    for (case, body, code, message) in cases {
        let (status, response) = post(
            "/process.Process/Connect",
            "application/connect+json",
            &body,
        )
        .await;
        assert_eq!(status, 200, "{case}");
        let payload = format!("{{\"error\":{}}}", error_document(code, message));
        let mut frame = vec![0x02, 0, 0, 0, 0];
        frame[1..5].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload.as_bytes());
        assert_eq!(response, frame, "{case}");
    }
}
