use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use coorworker_envd::daemon::{app, Daemon};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

fn query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

async fn configured(workdir: &Path, token: Option<&str>) -> Router {
    assert!(
        !nix::unistd::geteuid().is_root(),
        "download tests require an ordinary user"
    );
    let router = app(Arc::new(Daemon::new()));
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let mut init = json!({"defaultUser":user.name,"defaultWorkdir":workdir});
    if let Some(token) = token {
        init["accessToken"] = json!(token);
    }
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(init.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    router
}

async fn get(router: &Router, uri: &str, headers: &[(&str, &str)]) -> axum::response::Response {
    let mut request = Request::get(uri);
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
async fn bytes(response: axum::response::Response) -> Vec<u8> {
    to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}
async fn error(response: axum::response::Response, status: u16, message: &str) {
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let body = bytes(response).await;
    assert_eq!(body.last(), Some(&b'\n'));
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["code"], status);
    assert!(
        value["message"].as_str().unwrap().contains(message),
        "{value}"
    );
}
fn signed(path: &str, user: &str, token: &str, expiration: Option<i64>) -> String {
    let mut input = format!("{path}:read:{user}:{token}");
    if let Some(expiration) = expiration {
        input.push_str(&format!(":{expiration}"));
    }
    format!(
        "v1_{}",
        STANDARD_NO_PAD.encode(Sha256::digest(input.as_bytes()))
    )
}

#[tokio::test]
async fn identity_download_streams_bytes_headers_and_default_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("payload.txt");
    let content = b"hello \xe9\x9b\xaa\n\x00\xff";
    std::fs::write(&file, content).unwrap();
    let router = configured(&file, None).await;
    for uri in [
        "/files".to_owned(),
        format!("/files?path={}", query(file.to_str().unwrap())),
    ] {
        let response = get(&router, &uri, &[]).await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            if uri == "/files" {
                "application/octet-stream"
            } else {
                "text/plain; charset=utf-8"
            }
        );
        assert_eq!(
            response.headers()["content-disposition"],
            "inline; filename=payload.txt"
        );
        assert_eq!(response.headers()["vary"], "Accept-Encoding");
        assert_eq!(response.headers()["accept-ranges"], "bytes");
        assert_eq!(
            response.headers()["content-length"],
            content.len().to_string()
        );
        assert!(response.headers().contains_key("last-modified"));
        assert!(!response.headers().contains_key("etag"));
        assert!(!response.headers().contains_key("content-encoding"));
        assert_eq!(bytes(response).await, content);
    }
}

#[tokio::test]
async fn auth_signature_expiration_and_header_token_take_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("signed.txt");
    std::fs::write(&file, "signed bytes").unwrap();
    let path = file.to_str().unwrap();
    let token = "test-download-token-generated-for-this-fixture";
    let router = configured(dir.path(), Some(token)).await;
    let uri = format!("/files?path={}", query(path));
    error(
        get(&router, &uri, &[]).await,
        401,
        "missing signature query parameter",
    )
    .await;
    let signature = signed(path, "", token, None);
    let signed_uri = format!("{uri}&signature={}", query(&signature));
    assert_eq!(
        bytes(get(&router, &signed_uri, &[]).await).await,
        b"signed bytes"
    );
    error(
        get(&router, &signed_uri, &[("x-access-token", "wrong")]).await,
        401,
        "access token present in header but does not match",
    )
    .await;
    assert_eq!(
        bytes(
            get(
                &router,
                &format!("{uri}&signature=bad&signature_expiration=0"),
                &[("x-access-token", token)]
            )
            .await
        )
        .await,
        b"signed bytes"
    );
    error(
        get(&router, &format!("{uri}&signature=bad"), &[]).await,
        401,
        "invalid signature",
    )
    .await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for (expiration, expected) in [(now - 60, 401), (now + 60, 200)] {
        let signature = signed(path, "", token, Some(expiration));
        let response = get(
            &router,
            &format!(
                "{uri}&signature={}&signature_expiration={expiration}",
                query(&signature)
            ),
            &[],
        )
        .await;
        if expected == 401 {
            error(response, 401, "signature is already expired").await;
        } else {
            assert_eq!(bytes(response).await, b"signed bytes");
        }
    }
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap()
        .name;
    let signature = signed(path, &user, token, None);
    assert_eq!(
        bytes(
            get(
                &router,
                &format!(
                    "{uri}&username={}&signature={}",
                    query(&user),
                    query(&signature)
                ),
                &[]
            )
            .await
        )
        .await,
        b"signed bytes"
    );
    error(
        get(
            &router,
            &format!("{uri}&username=bad%3Auser&signature=bad"),
            &[],
        )
        .await,
        401,
        "a user name cannot contain a colon",
    )
    .await;
}

#[tokio::test]
async fn download_errors_and_filename_disposition_match_go() {
    let dir = tempfile::tempdir().unwrap();
    let router = configured(dir.path(), None).await;
    error(get(&router, "/files", &[]).await, 400, "is a directory").await;
    error(
        get(
            &router,
            &format!(
                "/files?path={}",
                query(dir.path().join("missing").to_str().unwrap())
            ),
            &[],
        )
        .await,
        404,
        "does not exist",
    )
    .await;
    error(
        get(&router, "/files?path=~another/file", &[]).await,
        400,
        "cannot expand user-specific home dir",
    )
    .await;
    error(
        get(&router, "/files?username=envd-user-does-not-exist", &[]).await,
        401,
        "error looking up user",
    )
    .await;
    error(
        get(&router, "/files?username=", &[]).await,
        401,
        "error looking up user",
    )
    .await;
    let response = get(&router, "/files?signature_expiration=oops", &[]).await;
    assert_eq!(response.status(), 400);
    for (filename, expected) in [
        ("simple.txt", "inline; filename=simple.txt"),
        ("my document.pdf", "inline; filename=\"my document.pdf\""),
        ("file\"name.txt", "inline; filename=\"file\\\"name.txt\""),
        ("file\\name.txt", "inline; filename=\"file\\\\name.txt\""),
        (
            "文档.pdf",
            "inline; filename*=utf-8''%E6%96%87%E6%A1%A3.pdf",
        ),
        (".env", "inline; filename=.env"),
    ] {
        let file = dir.path().join(filename);
        std::fs::write(&file, "content").unwrap();
        let response = get(
            &router,
            &format!("/files?path={}", query(file.to_str().unwrap())),
            &[],
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-disposition"], expected);
        assert_eq!(bytes(response).await, b"content");
    }
}

#[tokio::test]
async fn ranges_single_suffix_multipart_invalid_and_empty() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("range.txt");
    std::fs::write(&file, b"0123456789").unwrap();
    let router = configured(&file, None).await;
    for (range, expected, content_range) in [
        ("bytes=2-4", "234", "bytes 2-4/10"),
        ("bytes=7-", "789", "bytes 7-9/10"),
        ("bytes=-3", "789", "bytes 7-9/10"),
        ("bytes=8-999", "89", "bytes 8-9/10"),
        ("bytes=99-100,1-2", "12", "bytes 1-2/10"),
    ] {
        let response = get(
            &router,
            "/files",
            &[("range", range), ("accept-encoding", "gzip")],
        )
        .await;
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["content-range"], content_range);
        assert!(!response.headers().contains_key("content-encoding"));
        assert_eq!(bytes(response).await, expected.as_bytes());
    }
    let response = get(&router, "/files", &[("range", "bytes=0-1,8-9")]).await;
    assert_eq!(response.status(), 206);
    let ctype = response.headers()["content-type"]
        .to_str()
        .unwrap()
        .to_owned();
    let boundary = ctype
        .strip_prefix("multipart/byteranges; boundary=")
        .unwrap();
    let expected = format!("--{boundary}\r\nContent-Range: bytes 0-1/10\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n01\r\n--{boundary}\r\nContent-Range: bytes 8-9/10\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n89\r\n--{boundary}--\r\n");
    assert_eq!(
        response.headers()["content-length"],
        expected.len().to_string()
    );
    assert_eq!(bytes(response).await, expected.as_bytes());
    for range in [
        "bytes=abc",
        "items=0-1",
        "bytes=4-2",
        "bytes=99-",
        "bytes=9999999999999999999999999999-",
    ] {
        let response = get(&router, "/files", &[("range", range)]).await;
        assert_eq!(response.status(), 416, "{range}");
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert!(!response.headers().contains_key("last-modified"));
    }
    let response = get(&router, "/files", &[("range", "bytes=0-9,0-9")]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(bytes(response).await, b"0123456789");
    std::fs::write(&file, b"").unwrap();
    let response = get(&router, "/files", &[("range", "bytes=0-2")]).await;
    assert_eq!(response.status(), 200);
    assert!(bytes(response).await.is_empty());
}

#[tokio::test]
async fn conditional_order_dates_etags_and_if_range() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("conditional.txt");
    std::fs::write(&file, b"content").unwrap();
    let router = configured(&file, None).await;
    let initial = get(&router, "/files", &[]).await;
    let modified = initial.headers()["last-modified"]
        .to_str()
        .unwrap()
        .to_owned();
    for headers in [
        vec![("if-modified-since", modified.as_str())],
        vec![("if-none-match", "*")],
    ] {
        let response = get(&router, "/files", &headers).await;
        assert_eq!(response.status(), 304);
        assert!(response.headers().contains_key("last-modified"));
        assert!(!response.headers().contains_key("content-type"));
        assert!(bytes(response).await.is_empty());
    }
    for headers in [
        vec![("if-match", "\"unknown\"")],
        vec![("if-unmodified-since", "Thu, 01 Jan 1970 00:00:01 GMT")],
    ] {
        let response = get(&router, "/files", &headers).await;
        assert_eq!(response.status(), 412);
        assert!(bytes(response).await.is_empty());
    }
    let response = get(
        &router,
        "/files",
        &[
            ("if-match", "*"),
            ("if-unmodified-since", "Thu, 01 Jan 1970 00:00:01 GMT"),
        ],
    )
    .await;
    assert_eq!(response.status(), 200);
    let response = get(
        &router,
        "/files",
        &[
            ("if-none-match", "\"unknown\""),
            ("if-modified-since", &modified),
        ],
    )
    .await;
    assert_eq!(response.status(), 200);
    for (condition, status) in [
        (modified.as_str(), 206),
        ("\"unknown\"", 200),
        ("invalid-date", 200),
    ] {
        let response = get(
            &router,
            "/files",
            &[("range", "bytes=0-1"), ("if-range", condition)],
        )
        .await;
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(
            bytes(response).await,
            if status == 206 {
                b"co".as_slice()
            } else {
                b"content".as_slice()
            }
        );
    }
}

#[tokio::test]
async fn gzip_negotiation_and_identity_rejection_follow_go() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("gzip.txt");
    let content = "gzip 雪\n".repeat(10001);
    std::fs::write(&file, content.as_bytes()).unwrap();
    let router = configured(&file, None).await;
    for accept in ["gzip", "GZip;Q=1", "br,gzip;q=0.5", "*;q=1,identity;q=0"] {
        let response = get(&router, "/files", &[("accept-encoding", accept)]).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-encoding"], "gzip");
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        ); // original request path has no extension
        assert!(!response.headers().contains_key("last-modified"));
        let compressed = bytes(response).await;
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, content.as_bytes());
    }
    for accept in ["", "br", "gzip;q=0", "identity;q=1,gzip;q=0.5", "*"] {
        let response = get(&router, "/files", &[("accept-encoding", accept)]).await;
        assert_eq!(response.status(), 200);
        assert!(!response.headers().contains_key("content-encoding"));
        assert_eq!(bytes(response).await, content.as_bytes());
    }
    error(
        get(&router, "/files", &[("accept-encoding", "identity;q=0,br")]).await,
        406,
        "no acceptable encoding found",
    )
    .await;
    for conditional in ["range", "if-modified-since", "if-none-match", "if-range"] {
        error(
            get(
                &router,
                "/files",
                &[("accept-encoding", "gzip,*;q=0"), (conditional, "ignored")],
            )
            .await,
            406,
            "identity encoding not acceptable",
        )
        .await;
    }
}

#[tokio::test]
async fn unknown_extension_sniffs_identity_but_gzip_uses_octet_stream() {
    let dir = tempfile::tempdir().unwrap();
    let router = configured(dir.path(), None).await;
    for (name, data, mime) in [
        ("text", b"hello".as_slice(), "text/plain; charset=utf-8"),
        (
            "binary",
            b"\x00\x01\x02".as_slice(),
            "application/octet-stream",
        ),
        ("pdf", b"%PDF-1.5".as_slice(), "application/pdf"),
        (
            "html",
            b"<!DOCTYPE HTML><html>".as_slice(),
            "text/html; charset=utf-8",
        ),
    ] {
        let file = dir.path().join(name);
        std::fs::write(&file, data).unwrap();
        let response = get(
            &router,
            &format!("/files?path={}", query(file.to_str().unwrap())),
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], mime);
        assert_eq!(bytes(response).await, data);
    }
}

#[tokio::test]
async fn query_duplicates_and_range_numeric_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("boundary.txt");
    std::fs::write(&file, b"0123456789").unwrap();
    let router = configured(&file, None).await;
    for query in [
        "path=a&path=b",
        "username=a&username=b",
        "signature=a&signature=b",
        "signature_expiration=1&signature_expiration=2",
        "signature_expiration=",
        "signature_expiration=9223372036854775808",
    ] {
        let response = get(&router, &format!("/files?{query}"), &[]).await;
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
    }
    for (range, expected_range, expected) in [
        ("bytes=+1-+2", "bytes 1-2/10", b"12".as_slice()),
        ("bytes=-0", "bytes 10-9/10", b"".as_slice()),
        ("bytes=-00", "bytes 10-9/10", b"".as_slice()),
        ("bytes= -+00 ", "bytes 10-9/10", b"".as_slice()),
    ] {
        let response = get(&router, "/files", &[("range", range)]).await;
        assert_eq!(response.status(), 206, "{range}");
        assert_eq!(response.headers()["content-range"], expected_range);
        assert_eq!(bytes(response).await, expected);
    }
    let too_many = format!("bytes={}", vec!["0-0"; 201].join(","));
    let response = get(&router, "/files", &[("range", &too_many)]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(bytes(response).await, b"0123456789");
    for range in [
        "bytes=0-9223372036854775808",
        "bytes=-9223372036854775808",
        "bytes=0- 9223372036854775808",
    ] {
        let response = get(&router, "/files", &[("range", range)]).await;
        assert_eq!(response.status(), 416);
    }
}

#[tokio::test]
async fn empty_file_zero_suffix_and_syntax_errors_match_go() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.txt");
    std::fs::write(&file, b"").unwrap();
    let router = configured(&file, None).await;
    // Go emits the arithmetic endpoint verbatim: bytes 0--1/0 with no body.
    let response = get(&router, "/files", &[("range", "bytes=-0")]).await;
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 0--1/0");
    assert!(bytes(response).await.is_empty());
    for range in ["bytes=0- 9223372036854775808", "bytes=0-invalid"] {
        let response = get(&router, "/files", &[("range", range)]).await;
        assert_eq!(response.status(), 200, "{range}");
        assert!(bytes(response).await.is_empty());
    }
    for range in ["nonsense", "bytes=a-b", "bytes=-", "bytes=--1"] {
        let response = get(&router, "/files", &[("range", range)]).await;
        assert_eq!(response.status(), 416, "{range}");
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
    }
    std::fs::write(&file, b"data").unwrap();
    let response = get(&router, "/files", &[("range", "bytes=+")]).await;
    assert_eq!(response.status(), 416);
}
