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
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{io, sync::Arc};
use tower::ServiceExt;

fn app() -> Router {
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
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
    app_with_execution(Arc::new(Daemon::new()), runtime)
}
fn frame(flags: u8, bytes: &[u8]) -> Vec<u8> {
    let mut result = vec![flags];
    result.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    result.extend_from_slice(bytes);
    result
}
async fn request(body: Body, codec: &str, gzip: bool) -> Vec<(u8, Value)> {
    let mut request = Request::post("/process.Process/StreamInput")
        .header("content-type", format!("application/connect+{codec}"))
        .header("connect-protocol-version", "1");
    if gzip {
        request = request.header("connect-content-encoding", "gzip");
    }
    let response = app().oneshot(request.body(body).unwrap()).await.unwrap();
    assert_eq!(response.status(), 200);
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let mut bytes = bytes.as_ref();
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let len = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
        let payload = if bytes[0] & 1 != 0 {
            use std::io::Read;
            let mut payload = Vec::new();
            flate2::read::MultiGzDecoder::new(&bytes[5..5 + len])
                .read_to_end(&mut payload)
                .unwrap();
            payload
        } else {
            bytes[5..5 + len].to_vec()
        };
        result.push((bytes[0] & !1, serde_json::from_slice(&payload).unwrap()));
        bytes = &bytes[5 + len..];
    }
    result
}
fn error(code: &str, detail: &str) -> Vec<(u8, Value)> {
    vec![(
        2,
        json!({"error":{"code":"unknown","message":format!("error streaming input: {code}: {detail}")}}),
    )]
}
fn unmarshal(detail: &str) -> Vec<(u8, Value)> {
    error(
        "invalid_argument",
        &format!("unmarshal message: unmarshal into *process.StreamInputRequest: proto: {detail}"),
    )
}
#[tokio::test]
async fn terminal_payload_is_validated_and_trailing_bytes_counted_across_chunks() {
    for (flags, payload, suffix, expected) in [
        (
            2,
            b"x".as_slice(),
            b"".as_slice(),
            "unmarshal end stream message: invalid character 'x' looking for beginning of value",
        ),
        (
            2,
            b"{}",
            b"x",
            "corrupt response: 1 extra bytes after end of stream",
        ),
        (4, b"{}", b"", "protocol error: invalid envelope flags 4"),
    ] {
        let mut bytes = frame(flags, payload);
        bytes.extend(suffix);
        for split in 0..=bytes.len() {
            let chunks = vec![
                Ok::<_, io::Error>(bytes[..split].to_vec()),
                Ok(bytes[split..].to_vec()),
            ];
            assert_eq!(
                request(Body::from_stream(stream::iter(chunks)), "json", false).await,
                error("internal", expected),
                "split {split}"
            );
        }
    }
    assert_eq!(
        request(Body::from(frame(2, b"{}")), "json", false).await,
        vec![(0, json!({})), (2, json!({}))]
    );
}
#[tokio::test]
async fn short_prefixes_remain_errors_and_earlier_messages_keep_precedence() {
    for count in 1..5 {
        assert_eq!(
            request(Body::from(vec![0; count]), "json", false).await,
            error(
                "invalid_argument",
                "protocol error: incomplete envelope: unexpected EOF"
            )
        );
    }
    let mut bytes = frame(0, b"{}");
    bytes.extend([0, 0]);
    assert_eq!(
        request(Body::from(bytes), "json", false).await,
        vec![(
            2,
            json!({"error":{"code":"unimplemented","message":"invalid event type <nil>"}})
        )]
    );
    let mut bytes = frame(0, br#"{"keepalive":{}}"#);
    bytes.extend([0, 0]);
    assert_eq!(
        request(Body::from(bytes), "json", false).await,
        error(
            "invalid_argument",
            "protocol error: incomplete envelope: unexpected EOF"
        )
    );
}
#[tokio::test]
async fn known_null_fields_still_reject_duplicates_and_bytes_validate_base64() {
    for (payload, detail) in [
        (
            r#"{"keepalive":null,"keepalive":{}}"#,
            "(line 1:19): duplicate field \"keepalive\"",
        ),
        (
            r#"{"data":{"input":{"stdin":"%%%"}}}"#,
            "(line 1:27): invalid value for bytes field stdin: \"%%%\"",
        ),
    ] {
        assert_eq!(
            request(Body::from(frame(0, payload.as_bytes())), "json", false).await,
            unmarshal(detail)
        );
    }
}
#[tokio::test]
async fn zero_length_json_is_default_message() {
    assert_eq!(
        request(Body::from(frame(0, b"")), "json", false).await,
        vec![(
            2,
            json!({"error":{"code":"unimplemented","message":"invalid event type <nil>"}})
        )]
    );
}
#[tokio::test]
async fn json_syntax_diagnostics_match_protojson() {
    for (payload, expected) in [
        ("{", "unexpected EOF".to_owned()),
        (
            "{\"keepalive\":{},}",
            "syntax error (line 1:17): unexpected token }".to_owned(),
        ),
        ("x", "syntax error (line 1:1): invalid value x".to_owned()),
        (
            "{\"é\":1,\"keepalive\":{},}",
            "syntax error (line 1:23): unexpected token }".to_owned(),
        ),
    ] {
        assert_eq!(
            request(Body::from(frame(0, payload.as_bytes())), "json", false).await,
            unmarshal(&expected),
            "{payload}"
        );
    }
    assert_eq!(
        request(Body::from(frame(0, &vec![b'x'; 1024])), "json", false).await,
        unmarshal(&format!(
            "syntax error (line 1:1): invalid value {}",
            "x".repeat(32)
        ))
    );
}
#[tokio::test]
async fn json_semantic_diagnostics_match_protojson() {
    for (payload, expected) in [
        (r#"{"start":{"process":{"pid":true}}}"#, "(line 1:28): invalid value for uint32 field pid: true"),
        (r#"{"start":{"process":{"pid":-1}}}"#, "(line 1:28): invalid value for uint32 field pid: -1"),
        (r#"{"keepalive":{},"start":{"process":{"pid":999999}}}"#, "(line 1:17): error parsing \"start\", oneof process.StreamInputRequest.event is already set"),
    ] {
        assert_eq!(request(Body::from(frame(0,payload.as_bytes())),"json",false).await, unmarshal(expected), "{payload}");
    }
    assert_eq!(
        request(
            Body::from(frame(0, br#"{"keepalive":{},"unknown":true}"#)),
            "json",
            false
        )
        .await,
        vec![(0, json!({})), (2, json!({}))]
    );
}
#[tokio::test]
async fn malformed_protobuf_has_go_unmarshal_context() {
    assert_eq!(
        request(Body::from(frame(0, &[0x80])), "proto", false).await,
        error(
            "invalid_argument",
            "unmarshal message: proto: cannot parse invalid wire-format data"
        )
    );
}
#[tokio::test]
async fn body_errors_distinguish_header_payload_and_terminal_drain() {
    for (prefix, code, detail) in [
        (
            vec![0, 0],
            "invalid_argument",
            "protocol error: incomplete envelope: transport broke",
        ),
        (
            vec![0, 0, 0, 0, 5, b'{'],
            "unknown",
            "read enveloped message: transport broke",
        ),
        (
            frame(2, b"{}"),
            "internal",
            "corrupt response: I/O error after end-stream message: transport broke",
        ),
    ] {
        let body = Body::from_stream(stream::iter(vec![
            Ok(prefix),
            Err(io::Error::other("transport broke")),
        ]));
        assert_eq!(request(body, "json", false).await, error(code, detail));
    }
}
#[tokio::test]
async fn messages_above_library_default_limit_follow_go_unlimited_setting() {
    let payload = format!(
        r#"{{"keepalive":{{}},"ignored":"{}"}}"#,
        "a".repeat(4 * 1024 * 1024)
    );
    assert_eq!(
        request(Body::from(frame(0, payload.as_bytes())), "json", false).await,
        vec![(0, json!({})), (2, json!({}))]
    );
}
fn gzip(payload: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap()
}
#[tokio::test]
async fn gzip_members_and_first_decode_error_are_preserved() {
    for (flags, first, expected) in [
        (
            1,
            br#"{"keepalive":{}}"#.as_slice(),
            unmarshal("syntax error (line 1:17): invalid value x"),
        ),
        (
            3,
            b"{}".as_slice(),
            error(
                "internal",
                "unmarshal end stream message: invalid character 'x' after top-level value",
            ),
        ),
    ] {
        let mut payload = gzip(first);
        payload.extend(gzip(b"x"));
        assert_eq!(
            request(Body::from(frame(flags, &payload)), "json", true).await,
            expected
        );
    }
    let chunks = vec![
        Ok(frame(1, b"not gzip!!")),
        Err(io::Error::other("later error")),
    ];
    assert_eq!(
        request(Body::from_stream(stream::iter(chunks)), "json", true).await,
        error("invalid_argument", "get decompressor: gzip: invalid header")
    );
}
#[tokio::test]
async fn quoted_uint32_and_fractional_decimal_are_validated_without_rounding() {
    for value in ["\"01\"", "\"+1\"", "1.00000000000000001"] {
        let payload = format!(r#"{{"start":{{"process":{{"pid":{value}}}}}}}"#);
        assert_eq!(
            request(Body::from(frame(0, payload.as_bytes())), "json", false).await,
            unmarshal(&format!(
                "(line 1:28): invalid value for uint32 field pid: {value}"
            ))
        );
    }
}
#[tokio::test]
async fn terminal_metadata_and_error_members_are_typed() {
    for (payload,detail) in [
        (r#"{"metadata":42}"#, "json: cannot unmarshal number into Go struct field connectEndStreamMessage.metadata of type http.Header"),
        (r#"{"metadata":{"x":"value"}}"#, "json: cannot unmarshal string into Go struct field connectEndStreamMessage.metadata of type []string"),
        (r#"{"error":42}"#, "json: cannot unmarshal number into Go struct field connectEndStreamMessage.error of type struct { Code string \"json:\\\"code\\\"\"; Message string \"json:\\\"message\\\"\"; Details []*connect.connectWireDetail \"json:\\\"details\\\"\" }"),
        (r#"{"error":{"message":42}}"#, "json: cannot unmarshal number into Go struct field connectEndStreamMessage.error.message of type string"),
        (r#"{"error":{"details":[{"value":"!"}]}}"#, "decode base64: illegal base64 data at input byte 0"),
    ] {
        assert_eq!(request(Body::from(frame(2,payload.as_bytes())),"json",false).await,
            error("internal",&format!("unmarshal end stream message: {detail}")),"{payload}");
    }
}
#[tokio::test]
async fn valid_base64_newlines_and_stalled_stream_preserve_message_processing() {
    // Invalid selector is resolved before Data in process tests; keep bytes validation
    // independently observable by choosing Data before any Start.
    let payload = br#"{"data":{"input":{"stdin":"eA\n"}}}"#;
    let actual = request(Body::from(frame(0, payload)), "json", false).await;
    assert_eq!(
        actual,
        vec![(
            2,
            json!({"error":{"code":"failed_precondition","message":"process not selected"}})
        )]
    );
    let body = Body::from_stream(
        stream::once(async { Ok::<_, io::Error>(frame(0, b"{}")) }).chain(stream::pending()),
    );
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            request(body, "json", false)
        )
        .await
        .unwrap(),
        vec![(
            2,
            json!({"error":{"code":"unimplemented","message":"invalid event type <nil>"}})
        )]
    );
}
#[tokio::test]
async fn go_base64_accepts_trailing_bits_and_unicode_escaped_newlines() {
    for payload in [
        br#"{"data":{"input":{"stdin":"eB=="}}}"#.as_slice(),
        br#"{"data":{"input":{"stdin":"eA\u000a"}}}"#.as_slice(),
    ] {
        assert_eq!(
            request(Body::from(frame(0, payload)), "json", false).await,
            vec![(
                2,
                json!({"error":{"code":"failed_precondition","message":"process not selected"}})
            )]
        );
    }
}
#[tokio::test]
async fn terminal_duplicate_members_preserve_the_first_type_error() {
    assert_eq!(request(Body::from(frame(2,br#"{"metadata":42,"metadata":{}}"#)),"json",false).await,
        error("internal","unmarshal end stream message: json: cannot unmarshal number into Go struct field connectEndStreamMessage.metadata of type http.Header"));
}
#[tokio::test]
async fn uint32_accepts_long_integral_decimal_fraction() {
    let payload = format!(
        r#"{{"start":{{"process":{{"pid":1.{}}}}}}}"#,
        "0".repeat(65)
    );
    assert_eq!(
        request(Body::from(frame(0, payload.as_bytes())), "json", false).await,
        vec![(
            2,
            json!({"error":{"code":"not_found","message":"process with pid 1 not found"}})
        )]
    );
}
#[tokio::test]
async fn empty_gzip_missing_trailer_is_a_decompression_error() {
    let compressed = [0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0xff, 0x03, 0];
    assert_eq!(
        request(Body::from(frame(1, &compressed)), "json", true).await,
        error("invalid_argument", "decompress: unexpected EOF")
    );
}
#[tokio::test]
async fn gzip_receive_errors_keep_negotiated_header_and_compressed_terminal_flag() {
    use std::io::Read;
    let compressed = [0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0xff, 0x03, 0];
    for (accept, encoded) in [
        (None, true),
        (Some("identity"), true),
        (Some("gzip"), true),
        (Some("unknown"), true),
    ] {
        let mut req = Request::post("/process.Process/StreamInput")
            .header("content-type", "application/connect+json")
            .header("connect-protocol-version", "1")
            .header("connect-content-encoding", "gzip");
        if let Some(accept) = accept {
            req = req.header("connect-accept-encoding", accept);
        }
        let response = app()
            .oneshot(req.body(Body::from(frame(1, &compressed))).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("connect-content-encoding")
                .and_then(|v| v.to_str().ok()),
            encoded.then_some("gzip"),
            "accept {accept:?}"
        );
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        assert_eq!(bytes[0], if encoded { 3 } else { 2 });
        let len = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), 5 + len);
        let payload = if encoded {
            let mut payload = Vec::new();
            flate2::read::MultiGzDecoder::new(&bytes[5..])
                .read_to_end(&mut payload)
                .unwrap();
            payload
        } else {
            bytes[5..].to_vec()
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&payload).unwrap(),
            error("invalid_argument", "decompress: unexpected EOF")[0].1
        );
    }
}
#[tokio::test]
async fn gzip_success_prefers_request_encoding_over_accept_identity() {
    use std::io::Read;
    for accept in ["identity", "unknown"] {
        let response = app()
            .oneshot(
                Request::post("/process.Process/StreamInput")
                    .header("content-type", "application/connect+json")
                    .header("connect-protocol-version", "1")
                    .header("connect-content-encoding", "gzip")
                    .header("connect-accept-encoding", accept)
                    .body(Body::from(frame(1, &gzip(br#"{"keepalive":{}}"#))))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get("connect-content-encoding").unwrap(),
            "gzip"
        );
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let mut bytes = bytes.as_ref();
        let mut flags = Vec::new();
        while !bytes.is_empty() {
            let len = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
            flags.push(bytes[0]);
            let mut decoded = Vec::new();
            flate2::read::MultiGzDecoder::new(&bytes[5..5 + len])
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&decoded).unwrap(),
                json!({})
            );
            bytes = &bytes[5 + len..];
        }
        assert_eq!(flags, vec![1, 3]);
    }
}
#[tokio::test]
async fn gzip_remains_supported_for_messages_and_terminal_frames() {
    use std::io::Write;
    let mut bytes = Vec::new();
    for (flags, payload) in [
        (1, br#"{"keepalive":{}}"#.as_slice()),
        (3, b"{}".as_slice()),
    ] {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(payload).unwrap();
        bytes.extend(frame(flags, &encoder.finish().unwrap()));
    }
    assert_eq!(
        request(Body::from(bytes), "json", true).await,
        vec![(0, json!({})), (2, json!({}))]
    );
}

/// Go encoding/json unmarshals the end-stream body into
/// connect.endStreamMessage: top-level non-objects report the Go
/// "cannot unmarshal ... into Go value of type" wording, while null is a
/// no-op and null members inside the struct are ignored.
#[tokio::test]
async fn terminal_non_object_bodies_and_null_members_use_go_unmarshal_rules() {
    for (payload, kind) in [
        ("[]", "array"),
        (r#""text""#, "string"),
        ("true", "bool"),
        ("42", "number"),
        ("-1", "number"),
        ("1.5", "number"),
    ] {
        assert_eq!(
            request(Body::from(frame(2, payload.as_bytes())), "json", false).await,
            error(
                "internal",
                &format!(
                    "unmarshal end stream message: json: cannot unmarshal {kind} into Go value of type connect.connectEndStreamMessage"
                )
            ),
            "{payload}"
        );
    }
    // A nested array where the element must be a string reports the element
    // mismatch, exactly as encoding/json does for map[string][]string.
    assert_eq!(
        request(
            Body::from(frame(2, br#"{"metadata":{"x":["a",[]]}}"#)),
            "json",
            false
        )
        .await,
        error(
            "internal",
            "unmarshal end stream message: json: cannot unmarshal array into Go struct field connectEndStreamMessage.metadata of type string"
        )
    );
    // null bodies and null members are accepted: the stream completes with
    // the empty response message and a clean end-stream frame.
    let clean_end = vec![(0, json!({})), (2, json!({}))];
    for payload in [
        b"null".as_slice(),
        br#"{"metadata":{"x":null},"error":{"code":"c","details":[null]}}"#,
    ] {
        assert_eq!(
            request(Body::from(frame(2, payload)), "json", false).await,
            clean_end,
            "{}",
            String::from_utf8_lossy(payload)
        );
    }
}
