//! Pinned POST /files behavior against ordinary-user files and real HTTP bodies.
use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use bytes::Bytes;
use coorworker_envd::{
    daemon::{app, Daemon},
    filesystem::metadata::read_metadata,
};
use flate2::{write::GzEncoder, Compression};
use nix::unistd::{getegid, geteuid, User};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    sync::Arc,
};
use tower::ServiceExt;

fn username() -> String {
    assert!(
        !geteuid().is_root(),
        "upload tests require an ordinary user"
    );
    User::from_uid(geteuid()).unwrap().unwrap().name
}
async fn setup(dir: &std::path::Path) -> Router {
    let app = app(Arc::new(Daemon::new()));
    let result = app
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(
                    json!({"defaultUser":username(),"defaultWorkdir":dir}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.status(), 204);
    app
}
async fn upload(
    app: &Router,
    query: &str,
    kind: &str,
    body: Body,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let mut req = Request::post(format!("/files{query}")).header("content-type", kind);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let bytes = to_bytes(res.into_body(), 65536).await.unwrap();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)}));
    (status, value)
}
fn gzip(value: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(value).unwrap();
    enc.finish().unwrap()
}
fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, data) in parts {
        write!(
            &mut body,
            "--boundary\r\nContent-Disposition: form-data; name=\"{name}\""
        )
        .unwrap();
        if let Some(filename) = filename {
            write!(&mut body, "; filename=\"{filename}\"").unwrap();
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--boundary--\r\n");
    body
}
fn chunks(bytes: Vec<u8>) -> Body {
    Body::from_stream(futures_util::stream::iter(
        bytes
            .chunks(3)
            .map(|v| Ok::<_, std::io::Error>(Bytes::copy_from_slice(v)))
            .collect::<Vec<_>>(),
    ))
}

#[tokio::test]
async fn raw_creates_parents_sets_ownership_and_replaces_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let file = dir.path().join("parent/child/file");
    let query = format!("?path={}", file.display());
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream; charset=binary",
        Body::from("first"),
        &[
            ("X-Metadata-Author", "first"),
            ("x-metadata-purpose", "old"),
        ],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(
        value,
        json!([{"name":"file","path":file,"type":"file","metadata":{"author":"first","purpose":"old"}}])
    );
    let info = fs::metadata(&file).unwrap();
    assert_eq!(
        (info.uid(), info.gid()),
        (geteuid().as_raw(), getegid().as_raw())
    );
    let parent = fs::metadata(file.parent().unwrap()).unwrap();
    assert_eq!(
        (parent.uid(), parent.gid()),
        (geteuid().as_raw(), getegid().as_raw())
    );
    xattr::set(&file, "user.unrelated", b"keep").unwrap();
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        Body::from("second"),
        &[
            ("x-metadata-author", "new"),
            ("x-metadata-author", "ignored"),
        ],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value[0]["metadata"], json!({"author":"new"}));
    assert_eq!(fs::read(&file).unwrap(), b"second");
    assert_eq!(read_metadata(&file).unwrap().len(), 1);
    let (status, value) =
        upload(&app, &query, "application/octet-stream", Body::empty(), &[]).await;
    assert_eq!(status, 200, "{value}");
    assert!(value[0].get("metadata").is_none());
    assert!(read_metadata(&file).unwrap().is_empty());
    assert_eq!(
        xattr::get(&file, "user.unrelated").unwrap().unwrap(),
        b"keep"
    );
}

#[tokio::test]
async fn multipart_preserves_full_names_skips_other_fields_and_rejects_aliases_before_truncate() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let a = dir.path().join("nested/a");
    let b = dir.path().join("b");
    let body = multipart(&[
        ("note", None, b"ignore"),
        ("file", Some(a.to_str().unwrap()), b"first"),
        ("file", Some(b.to_str().unwrap()), b"second"),
    ]);
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        chunks(body),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value.as_array().unwrap().len(), 2);
    assert_eq!(fs::read(&a).unwrap(), b"first");
    assert_eq!(fs::read(&b).unwrap(), b"second");
    for kind in ["same", "hard", "soft", "lexical"] {
        let alias = dir.path().join(format!("alias-{kind}"));
        let target = match kind {
            "same" => a.clone(),
            "lexical" => a.parent().unwrap().join("../nested/a"),
            "hard" => {
                fs::hard_link(&a, &alias).unwrap();
                alias
            }
            _ => {
                symlink(&a, &alias).unwrap();
                alias
            }
        };
        let body = multipart(&[
            ("file", Some(a.to_str().unwrap()), b"preserved"),
            ("file", Some(target.to_str().unwrap()), b"destroyed"),
        ]);
        let (status, value) = upload(
            &app,
            "",
            "multipart/form-data; boundary=boundary",
            Body::from(body),
            &[],
        )
        .await;
        assert_eq!(status, 400, "{kind}: {value}");
        assert_eq!(fs::read(&a).unwrap(), b"preserved");
    }
}

#[tokio::test]
async fn streaming_gzip_raw_and_multipart_support_multiple_members_and_crc_errors() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let path = dir.path().join("gzip");
    let query = format!("?path={}", path.display());
    let mut data = gzip(b"first");
    data.extend(gzip(b"second"));
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        chunks(data),
        &[("content-encoding", "GZip;q=0")],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(fs::read(&path).unwrap(), b"firstsecond");
    let dest = dir.path().join("multipart");
    let body = gzip(&multipart(&[(
        "file",
        Some(dest.to_str().unwrap()),
        b"decoded",
    )]));
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        chunks(body),
        &[("content-encoding", "gzip")],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(fs::read(dest).unwrap(), b"decoded");
    let mut corrupt = gzip(b"partial");
    let len = corrupt.len();
    corrupt[len - 8] ^= 1;
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        chunks(corrupt),
        &[("content-encoding", "gzip")],
    )
    .await;
    assert_eq!(status, 500, "{value}");
    assert_eq!(fs::read(&path).unwrap(), b"partial");
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        Body::from("bad"),
        &[("content-encoding", "gzip")],
    )
    .await;
    assert_eq!(status, 400, "{value}");
    // Go's compress/gzip texts, wrapped by getDecompressedBody verbatim: a
    // three-byte body is truncated inside the fixed header ("unexpected EOF"),
    // while a full-sized non-gzip header fails the magic check.
    assert_eq!(
        value["message"],
        "error decompressing request body: failed to create gzip reader: unexpected EOF"
    );
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        Body::from("not-gzip-bytes"),
        &[("content-encoding", "gzip")],
    )
    .await;
    assert_eq!(status, 400, "{value}");
    assert_eq!(
        value["message"],
        "error decompressing request body: failed to create gzip reader: gzip: invalid header"
    );
    assert_eq!(fs::read(&path).unwrap(), b"partial");
}

#[tokio::test]
async fn truncated_multipart_headers_answer_an_empty_sniffed_array() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    // Go's mime/multipart NextPart reports io.EOF when the stream ends inside
    // the next part's headers, so PostFiles succeeds with no entries; the
    // marshal wrote with no explicit type is sniffed as text/plain.
    let response = app
        .clone()
        .oneshot(
            Request::post("/files")
                .header("content-type", "multipart/form-data; boundary=X")
                .body(Body::from(
                    "--X\r\nContent-Disposition: form-data; name=\"a\"\r\n",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/plain; charset=utf-8"
    );
    assert_eq!(
        to_bytes(response.into_body(), 65536).await.unwrap(),
        &b"[]"[..]
    );
}

#[tokio::test]
async fn errors_do_not_succeed_or_truncate_before_validation() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let path = dir.path().join("file");
    fs::write(&path, b"old").unwrap();
    let query = format!("?path={}", path.display());
    for (q, kind, body, headers, expected) in [
        (
            "".to_owned(),
            "application/octet-stream",
            b"new".to_vec(),
            vec![],
            400,
        ),
        (query.clone(), "text/plain", b"new".to_vec(), vec![], 400),
        (
            query.clone(),
            "application/octet-stream",
            b"new".to_vec(),
            vec![("content-encoding", "br")],
            400,
        ),
        (
            query.clone(),
            "application/octet-stream",
            b"new".to_vec(),
            vec![("x-metadata-author", "bad\tvalue")],
            400,
        ),
        (
            format!("?path={}", dir.path().display()),
            "application/octet-stream",
            b"new".to_vec(),
            vec![],
            400,
        ),
        (
            format!("?path={}/child", path.display()),
            "application/octet-stream",
            b"new".to_vec(),
            vec![],
            500,
        ),
        (
            "?path=~other/file".to_owned(),
            "application/octet-stream",
            b"new".to_vec(),
            vec![],
            400,
        ),
        (
            "".to_owned(),
            "multipart/form-data",
            Vec::new(),
            vec![],
            500,
        ),
        (
            "".to_owned(),
            "multipart/form-data; boundary=boundary",
            multipart(&[("file", None, b"new")]),
            vec![],
            400,
        ),
    ] {
        let (status, value) = upload(&app, &q, kind, Body::from(body), &headers).await;
        assert_eq!(status, expected, "{q}: {value}");
        assert_eq!(fs::read(&path).unwrap(), b"old");
    }
    let (status, value) = upload(
        &app,
        &format!("{query}&username=envd-no-such-user-123"),
        "application/octet-stream",
        Body::from("new"),
        &[],
    )
    .await;
    assert_eq!(status, 401, "{value}");
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        Body::from(multipart(&[("other", None, b"ignore")])),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value, json!([]));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        Body::from("new"),
        &[],
    )
    .await;
    assert_eq!(status, 500, "{value}");
}

#[tokio::test]
async fn raw_default_path_override_and_request_body_errors_are_real() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let file = dir.path().join("target");
    let query = format!("?path={}", file.display());
    let body = multipart(&[
        ("file", Some("ignored-one"), b"first"),
        ("file", Some("ignored-two"), b"second"),
    ]);
    let (status, value) = upload(
        &app,
        &query,
        "multipart/form-data; boundary=boundary",
        Body::from(body),
        &[],
    )
    .await;
    assert_eq!(status, 400, "{value}");
    assert_eq!(fs::read(&file).unwrap(), b"first");
    let stream = futures_util::stream::iter(vec![
        Ok(Bytes::from_static(b"partial")),
        Err(std::io::Error::other("injected HTTP body failure")),
    ]);
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        Body::from_stream(stream),
        &[],
    )
    .await;
    assert_eq!(status, 500, "{value}");
    assert_eq!(fs::read(&file).unwrap(), b"partial");
}

#[tokio::test]
async fn multipart_mixed_extended_filenames_and_quoted_printable_match_next_part() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let dest = dir.path().join("extended name");
    let encoded = percent_encoding::utf8_percent_encode(
        dest.to_str().unwrap(),
        percent_encoding::NON_ALPHANUMERIC,
    )
    .to_string();
    let body=format!("--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename*=UTF-8''{encoded}\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nhello=20world=0A\r\n--boundary--\r\n");
    let (status, value) = upload(
        &app,
        "",
        "multipart/mixed; boundary=boundary",
        chunks(body.into_bytes()),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(fs::read(&dest).unwrap(), b"hello world\n");
    for (input, want) in [
        (&b"high\x80\nnext"[..], &b"high\x80\nnext"[..]),
        (&b"first=\r\nsecond=\nthird"[..], &b"firstsecondthird"[..]),
        (&b"a=3db=XYc=Z"[..], &b"a=b=XYc=Z"[..]),
        (&b"space \t\nnext\t"[..], &b"space\nnext"[..]),
    ] {
        let mut body=format!("--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n",dest.display()).into_bytes();
        body.extend_from_slice(input);
        body.extend_from_slice(b"\r\n--boundary--\r\n");
        let (status, value) = upload(
            &app,
            "",
            "multipart/form-data; boundary=boundary",
            chunks(body),
            &[],
        )
        .await;
        assert_eq!(status, 200, "{value}");
        assert_eq!(fs::read(&dest).unwrap(), want);
    }
    let mut long = vec![b'a'; 12000];
    long.extend_from_slice(b"=0Aend");
    let mut body=format!("--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n",dest.display()).into_bytes();
    body.extend_from_slice(&long);
    body.extend_from_slice(b"\r\n--boundary--\r\n");
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        chunks(body),
        &[],
    )
    .await;
    assert_eq!(status, 500, "{value}");
    assert_eq!(fs::read(&dest).unwrap(), vec![b'a'; 4096]);
}

#[tokio::test]
async fn review_disposition_attachment_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let file = dir.path().join("target");
    fs::write(&file, b"old").unwrap();
    let body=format!("--boundary\r\nContent-Disposition: attachment; name=\"file\"; filename=\"{}\"\r\n\r\nnew\r\n--boundary--\r\n",file.display());
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        chunks(body.into_bytes()),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value, json!([]));
    assert_eq!(fs::read(file).unwrap(), b"old");
}
#[tokio::test]
async fn review_disposition_parameter_names_are_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    for (name, filename) in [("NAME", "filename"), ("name", "FILENAME")] {
        let file = dir.path().join(name);
        let body=format!("--boundary\r\nContent-Disposition: form-data; {name}=\"file\"; {filename}=\"{}\"\r\n\r\nnew\r\n--boundary--\r\n",file.display());
        let (status, value) = upload(
            &app,
            "",
            "multipart/form-data; boundary=boundary",
            chunks(body.into_bytes()),
            &[],
        )
        .await;
        assert_eq!(status, 200, "{value}");
        assert_eq!(value.as_array().unwrap().len(), 1);
        assert_eq!(fs::read(file).unwrap(), b"new");
    }
}
#[tokio::test]
async fn review_quoted_printable_cr_escape_errors() {
    review_quoted_error(b"a=\rb", b"a").await;
}
#[tokio::test]
async fn review_quoted_printable_retains_prefix_before_error() {
    review_quoted_error(b"ok\x01\n", b"ok").await;
}
async fn review_quoted_error(data: &[u8], prefix: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let file = dir.path().join("quoted");
    let mut body=format!("--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n",file.display()).into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\r\n--boundary--\r\n");
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        chunks(body),
        &[],
    )
    .await;
    assert_eq!(status, 500, "{value}");
    assert_eq!(fs::read(file).unwrap(), prefix);
}
#[tokio::test]
async fn review_unfinished_part_headers_reject_before_sender_closes() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(512);
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|v| (v, receiver))
    });
    sender.send(Ok(Bytes::from_static(b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"ignored\"\r\nX-Pad: "))).await.unwrap();
    for _ in 0..352 {
        sender
            .send(Ok(Bytes::from(vec![b'a'; 32768])))
            .await
            .unwrap();
    }
    let mut task = tokio::spawn(async move {
        upload(
            &app,
            "",
            "multipart/form-data; boundary=boundary",
            Body::from_stream(stream),
            &[],
        )
        .await
    });
    // The budget is 10 MiB, and in a debug build multer rescans the growing
    // unterminated header line on every read, so this test alone costs several
    // seconds; the deadline is a hang guard, not a latency target.
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), &mut task).await;
    if result.is_err() {
        task.abort();
        drop(sender);
        panic!("unterminated headers exceeded Go's budget without early rejection");
    }
    let (status, value) = result.unwrap().unwrap();
    assert_eq!(status, 500, "{value}");
    drop(sender);
}

#[tokio::test]
async fn review_header_guard_preserves_large_file_streams_and_checks_later_parts() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let file = dir.path().join("large");
    let prefix = Bytes::from(format!(
        "--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n\r\n",
        file.display()
    ));
    let stream = futures_util::stream::iter(
        std::iter::once(Ok::<_, std::io::Error>(prefix))
            .chain((0..352).map(|_| Ok(Bytes::from(vec![b'x'; 32768]))))
            .chain(std::iter::once(Ok(Bytes::from_static(
                b"\r\n--boundary--\r\n",
            )))),
    );
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        Body::from_stream(stream),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(fs::metadata(&file).unwrap().len(), 352 * 32768);
    let mut body = multipart(&[("file", Some(file.to_str().unwrap()), b"first")]);
    body.truncate(body.len() - b"--boundary--\r\n".len());
    body.extend_from_slice(b"--boundary\r\nX-Pad: ");
    body.extend(std::iter::repeat_n(b'a', 11 << 20));
    body.extend_from_slice(b"\r\n\r\nignored\r\n--boundary--\r\n");
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        Body::from(body),
        &[],
    )
    .await;
    assert_eq!(status, 500, "{value}");
    assert_eq!(fs::read(file).unwrap(), b"first");
}

#[tokio::test]
async fn review_preamble_without_boundary_rejects_while_sender_open() {
    preamble_rejected(b"", 8192).await;
}
#[tokio::test]
async fn review_non_line_boundary_cannot_bypass_header_budget() {
    preamble_rejected(b"x--boundary\r\nX-Pad: ", 11 << 20).await;
}
async fn preamble_rejected(prefix: &'static [u8], size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(512);
    sender.send(Ok(Bytes::from_static(prefix))).await.unwrap();
    for chunk in vec![b'a'; size].chunks(32768) {
        sender
            .send(Ok(Bytes::copy_from_slice(chunk)))
            .await
            .unwrap();
    }
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|v| (v, receiver))
    });
    let mut task = tokio::spawn(async move {
        upload(
            &app,
            "",
            "multipart/form-data; boundary=boundary",
            Body::from_stream(stream),
            &[],
        )
        .await
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), &mut task).await;
    if result.is_err() {
        task.abort();
        drop(sender);
        panic!("preamble did not reject before body sender closed");
    }
    let (status, value) = result.unwrap().unwrap();
    assert_eq!(status, 500, "{value}");
    drop(sender);
}

#[tokio::test]
async fn review_preamble_short_lines_are_discarded_before_valid_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let file = dir.path().join("preamble");
    let mut body = b"x--boundary\r\n".repeat(800);
    body.extend_from_slice(&multipart(&[(
        "file",
        Some(file.to_str().unwrap()),
        b"valid",
    )]));
    let (status, value) = upload(
        &app,
        "",
        "multipart/form-data; boundary=boundary",
        chunks(body),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(fs::read(file).unwrap(), b"valid");
}

#[tokio::test]
async fn review_final_boundary_at_eof_needs_no_newline() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    for data in ["--boundary--", "--boundary-- \t"] {
        let (status, value) = upload(
            &app,
            "",
            "multipart/form-data; boundary=boundary",
            chunks(data.as_bytes().to_vec()),
            &[],
        )
        .await;
        assert_eq!(status, 200, "{value}");
        assert_eq!(value, json!([]));
    }
}
#[tokio::test]
async fn raw_is_written_before_http_body_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let file = dir.path().join("stream");
    let query = format!("?path={}", file.display());
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(1);
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|data| (data, receiver))
    });
    sender.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
    let task = tokio::spawn(async move {
        upload(
            &app,
            &query,
            "application/octet-stream",
            Body::from_stream(stream),
            &[],
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if fs::read(&file).is_ok_and(|data| data == b"first") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    sender
        .send(Ok(Bytes::from_static(b"second")))
        .await
        .unwrap();
    drop(sender);
    let (status, value) = task.await.unwrap();
    assert_eq!(status, 200, "{value}");
    assert_eq!(fs::read(&file).unwrap(), b"firstsecond");
}

#[tokio::test]
async fn write_signature_header_priority_and_method_binding() {
    use coorworker_envd::auth::{Access, ReadWrite};
    let dir = tempfile::tempdir().unwrap();
    let app = setup(dir.path()).await;
    let token = "upload-test-token";
    let result = app
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(json!({"accessToken":token}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.status(), 204);
    let mut access = Access::fresh();
    access.set_token(Some(token.to_owned())).unwrap();
    let file = dir.path().join("signed");
    let path = file.to_str().unwrap();
    let user = username();
    let signature = access
        .signature(path, ReadWrite::Write, &user, None)
        .unwrap();
    let signature =
        percent_encoding::utf8_percent_encode(&signature, percent_encoding::NON_ALPHANUMERIC);
    let query = format!("?path={path}&username={user}&signature={signature}");
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        Body::from("signed"),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{value}");
    let (status, value) = upload(
        &app,
        &query,
        "application/octet-stream",
        Body::from("bad"),
        &[("x-access-token", "wrong")],
    )
    .await;
    assert_eq!(status, 401, "{value}");
    assert_eq!(fs::read(&file).unwrap(), b"signed");
    let read = access
        .signature(path, ReadWrite::Read, &user, None)
        .unwrap();
    let read = percent_encoding::utf8_percent_encode(&read, percent_encoding::NON_ALPHANUMERIC);
    let (status, value) = upload(
        &app,
        &format!("?path={path}&username={user}&signature={read}"),
        "application/octet-stream",
        Body::from("bad"),
        &[],
    )
    .await;
    assert_eq!(status, 401, "{value}");
    let (status, value) = upload(
        &app,
        &format!("?path={path}"),
        "application/octet-stream",
        Body::from("header"),
        &[("x-access-token", token)],
    )
    .await;
    assert_eq!(status, 200, "{value}");
}
