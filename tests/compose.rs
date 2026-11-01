//! HTTP behavior pinned to runtime 51083a83 compose.go and its Unix link tests.
#[path = "common/compose_file_limit.rs"]
mod compose_file_limit;
use axum::http::Request;
use coorworker_envd::daemon::{app, Daemon};
use http_body_util::{BodyExt, Full};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

type Client = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    Full<bytes::Bytes>,
>;
struct Http {
    client: Client,
    origin: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Http {
    fn drop(&mut self) {
        self.server.abort();
    }
}
struct Reply {
    status: u16,
    headers: axum::http::HeaderMap,
    bytes: Vec<u8>,
}
impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).expect("JSON response")
    }
    fn error(&self, status: u16, message: &str) {
        let text = String::from_utf8_lossy(&self.bytes);
        assert_eq!(self.status, status, "{text}");
        let content_type = &self.headers["content-type"];
        assert_eq!(content_type, "application/json; charset=utf-8");
        assert_eq!(self.headers["x-content-type-options"], "nosniff");
        assert_eq!(self.bytes.last(), Some(&b'\n'));
        let value = self.json();
        assert_eq!(value["code"], status);
        assert!(
            value["message"].as_str().unwrap().contains(message),
            "{value}"
        );
    }
}
impl Http {
    async fn new(workdir: &Path) -> Self {
        let is_root = nix::unistd::geteuid().is_root();
        assert!(!is_root, "ordinary-user tests only");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = app(Arc::new(Daemon::new()));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let http = Self {
            client: hyper_util::client::legacy::Client::builder(
                hyper_util::rt::TokioExecutor::new(),
            )
            .build_http(),
            origin,
            server,
        };
        let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .unwrap()
            .unwrap();
        let init = http
            .raw(
                "/init",
                json!({"defaultUser":user.name,"defaultWorkdir":workdir}).to_string(),
            )
            .await;
        assert_eq!(init.status, 204);
        http
    }
    async fn raw(&self, route: &str, body: String) -> Reply {
        self.request(
            Request::post(format!("{}{route}", self.origin))
                .header("content-type", "application/json")
                .body(Full::new(bytes::Bytes::from(body)))
                .unwrap(),
        )
        .await
    }
    async fn request(&self, request: Request<Full<bytes::Bytes>>) -> Reply {
        tokio::time::timeout(Duration::from_secs(10), async {
            let response = self.client.request(request).await.unwrap();
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let bytes = response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec();
            Reply {
                status,
                headers,
                bytes,
            }
        })
        .await
        .expect("HTTP request completed")
    }
    async fn compose(&self, sources: &[PathBuf], destination: &Path) -> Reply {
        self.raw(
            "/files/compose",
            json!({"source_paths":sources,"destination":destination}).to_string(),
        )
        .await
    }
}
fn source(dir: &Path, name: &str, data: impl AsRef<[u8]>) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, data).unwrap();
    path
}
fn no_temporary_files(dir: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        assert!(!entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".e2b-compose."));
    }
}
// Restore permissions even when a request/assertion fails, before TempDir cleanup.
struct PermissionsGuard(PathBuf, u32);
impl PermissionsGuard {
    fn set(path: &Path, mode: u32) -> Self {
        let original = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self(path.to_owned(), original)
    }
}
impl Drop for PermissionsGuard {
    fn drop(&mut self) {
        fs::set_permissions(&self.0, fs::Permissions::from_mode(self.1)).unwrap();
    }
}

#[tokio::test]
async fn ordered_binary_compose_creates_owned_parents_cleans_sources_and_downloads() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let first = source(dir.path(), "雪 part", b"a\0\xff");
    let empty = source(dir.path(), "empty", b"");
    let last = source(dir.path(), "last", "雪");
    let dest = dir.path().join("nested/deep/out.bin");
    let reply = http
        .compose(&[first.clone(), empty.clone(), last.clone()], &dest)
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.headers["content-type"], "application/json");
    assert_eq!(
        reply.json(),
        json!({"path":dest,"name":"out.bin","type":"file"})
    );
    let expected = b"a\0\xff\xe9\x9b\xaa";
    assert_eq!(fs::read(&dest).unwrap(), expected);
    for path in [first, empty, last] {
        assert!(!path.exists());
    }
    for path in [
        &dest,
        dest.parent().unwrap(),
        dest.parent().unwrap().parent().unwrap(),
    ] {
        let info = fs::metadata(path).unwrap();
        assert_eq!(info.uid(), nix::unistd::geteuid().as_raw());
        assert_eq!(info.gid(), nix::unistd::getegid().as_raw());
    }
    let encoded: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("path", dest.to_str().unwrap())
        .finish();
    let download = http
        .request(
            Request::get(format!("{}/files?{encoded}", http.origin))
                .body(Full::new(bytes::Bytes::new()))
                .unwrap(),
        )
        .await;
    assert_eq!(download.status, 200);
    assert_eq!(download.bytes, expected);
    no_temporary_files(dest.parent().unwrap());
}

#[tokio::test]
async fn json_validation_and_user_resolution_report_official_categories() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    for body in [
        "",
        "{",
        "[]",
        "{\"source_paths\":3}",
        "{\"destination\":42}",
    ] {
        http.raw("/files/compose", body.into())
            .await
            .error(400, "invalid request body:");
    }
    for body in [
        json!({}),
        json!(null),
        json!({"source_paths":null}),
        json!({"source_paths":[]}),
    ] {
        http.raw("/files/compose", body.to_string())
            .await
            .error(400, "source_paths must not be empty");
    }
    http.raw("/files/compose", json!({"source_paths":["a"]}).to_string())
        .await
        .error(400, "destination is required");
    let dest = dir.path().join("out");
    http.raw("/files/compose", json!({"source_paths":["a"],"destination":dest,"username":"envd-compose-user-that-does-not-exist"}).to_string())
        .await.error(401, "error looking up user 'envd-compose-user-that-does-not-exist'");
    http.raw(
        "/files/compose",
        json!({"source_paths":["a"],"destination":"~other/path"}).to_string(),
    )
    .await
    .error(400, "error resolving destination path:");
    http.raw(
        "/files/compose",
        json!({"source_paths":["~other/path"],"destination":dest}).to_string(),
    )
    .await
    .error(400, "error resolving source path");
    assert!(!dest.exists());
}

#[tokio::test]
async fn validates_all_sources_before_replacing_destination_or_consuming_any_source() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let good = source(dir.path(), "good", "new");
    let dest = source(dir.path(), "out", "old");
    let missing = dir.path().join("missing");
    http.compose(&[good.clone(), missing.clone()], &dest)
        .await
        .error(404, "source file not found:");
    http.compose(&[good.clone(), dir.path().to_owned()], &dest)
        .await
        .error(400, "source path is not a regular file:");
    http.compose(&[good.clone(), dest.clone()], &dest)
        .await
        .error(400, "cannot be the same as destination");
    let dotted = dir.path().join(".").join("out");
    http.compose(&[good.clone(), dotted], &dest)
        .await
        .error(400, "cannot be the same as destination");
    let hardlink = dir.path().join("hardlink");
    fs::hard_link(&dest, &hardlink).unwrap();
    http.compose(&[good.clone(), hardlink], &dest)
        .await
        .error(400, "cannot be the same as destination");
    let link = dir.path().join("link");
    symlink(&dest, &link).unwrap();
    http.compose(&[link], &dest)
        .await
        .error(400, "cannot be the same as destination");
    assert_eq!(fs::read(&dest).unwrap(), b"old");
    assert_eq!(fs::read(&good).unwrap(), b"new");
    no_temporary_files(dir.path());
}

#[tokio::test]
async fn destination_symlink_is_replaced_and_source_link_through_it_is_removed() {
    for through_dest in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let http = Http::new(dir.path()).await;
        let target = source(dir.path(), "target", "a");
        let second = source(dir.path(), "second", "b");
        let dest = dir.path().join("out");
        symlink(&target, &dest).unwrap();
        let first = if through_dest {
            let link = dir.path().join("src");
            symlink(&dest, &link).unwrap();
            link
        } else {
            target.clone()
        };
        let reply = http.compose(&[first.clone(), second.clone()], &dest).await;
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.bytes)
        );
        assert!(fs::symlink_metadata(&dest).unwrap().is_file());
        assert_eq!(fs::read(&dest).unwrap(), b"ab");
        assert!(fs::symlink_metadata(&first).is_err());
        assert!(!second.exists());
        assert_eq!(target.exists(), through_dest);
    }
}

#[tokio::test]
async fn repeated_sources_and_trailing_json_follow_single_go_decode() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "same");
    let dest = dir.path().join("out");
    let body = format!(
        "{} {{\"ignored\":true}}",
        json!({"source_paths":[src,src],"destination":dest,"unknown":true})
    );
    let reply = http.raw("/files/compose", body).await;
    assert_eq!(reply.status, 200);
    assert_eq!(fs::read(dest).unwrap(), b"samesame");
    assert!(!src.exists());
}

#[tokio::test]
async fn large_compose_keeps_all_ten_megabytes_in_source_order() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let sources: Vec<_> = (0..10u8)
        .map(|i| source(dir.path(), &format!("part{i}"), vec![i; 1024 * 1024]))
        .collect();
    let dest = dir.path().join("out");
    assert_eq!(http.compose(&sources, &dest).await.status, 200);
    let bytes = fs::read(&dest).unwrap();
    assert_eq!(bytes.len(), 10 * 1024 * 1024);
    for (i, chunk) in bytes.as_chunks::<{ 1024 * 1024 }>().0.iter().enumerate() {
        assert!(chunk.iter().all(|byte| *byte == i as u8));
    }
    assert!(sources.iter().all(|path| !path.exists()));
}

#[tokio::test]
async fn failures_preserve_existing_destination_sources_and_remove_temporary_assembly() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = source(dir.path(), "out", "old");
    {
        let _permissions = PermissionsGuard::set(dir.path(), 0o500);
        http.compose(std::slice::from_ref(&src), &dest)
            .await
            .error(500, "error creating destination file:");
    }
    {
        let _permissions = PermissionsGuard::set(&src, 0o000);
        http.compose(std::slice::from_ref(&src), &dest)
            .await
            .error(500, "error opening source file");
    }
    assert_eq!(fs::read(&dest).unwrap(), b"old");
    assert_eq!(fs::read(&src).unwrap(), b"new");
    let destination_dir = dir.path().join("destination-dir");
    fs::create_dir(&destination_dir).unwrap();
    http.compose(std::slice::from_ref(&src), &destination_dir)
        .await
        .error(500, "error finalizing compose:");
    let blocked = dir.path().join("out/sub/file");
    http.compose(std::slice::from_ref(&src), &blocked)
        .await
        .error(500, "error ensuring directories:");
    assert_eq!(fs::read(&src).unwrap(), b"new");
    assert_eq!(fs::read(&dest).unwrap(), b"old");
    no_temporary_files(dir.path());
}

#[tokio::test]
async fn concurrent_composes_replace_existing_entry_with_one_complete_result() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let dest = source(dir.path(), "out", "original");
    let original_link = dir.path().join("original-link");
    fs::hard_link(&dest, &original_link).unwrap();
    let left = source(dir.path(), "left", vec![b'a'; 1024 * 1024]);
    let right = source(dir.path(), "right", vec![b'b'; 1024 * 1024]);
    let (first, second) = tokio::join!(
        http.compose(std::slice::from_ref(&left), &dest),
        http.compose(std::slice::from_ref(&right), &dest)
    );
    assert_eq!(first.status, 200);
    assert_eq!(second.status, 200);
    let bytes = fs::read(&dest).unwrap();
    assert_eq!(bytes.len(), 1024 * 1024);
    assert!(bytes.iter().all(|byte| *byte == bytes[0]));
    assert!(matches!(bytes[0], b'a' | b'b'));
    assert_eq!(fs::read(original_link).unwrap(), b"original");
    assert!(!left.exists());
    assert!(!right.exists());
    no_temporary_files(dir.path());
}

#[tokio::test]
async fn explicit_username_null_source_and_default_workdir_match_go() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), "part", "default workdir source");
    let http = Http::new(&src).await;
    let dest = dir.path().join("out");
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let reply = http
        .raw(
            "/files/compose",
            json!({
                "source_paths":[null], "destination":dest, "username":user.name
            })
            .to_string(),
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(fs::read(dest).unwrap(), b"default workdir source");
    assert!(!src.exists());
}

#[tokio::test]
async fn duplicate_fields_keep_the_first_type_error_and_never_consume_sources() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = source(dir.path(), "out", "old");
    for fields in [
        format!("\"destination\":17,\"destination\":{}", json!(dest)),
        format!("\"destination\":17,\"DESTINATION\":{}", json!(dest)),
        format!(
            "\"source_paths\":17,\"source_paths\":[{}],\"destination\":{}",
            json!(src),
            json!(dest)
        ),
        format!(
            "\"username\":17,\"username\":null,\"destination\":{}",
            json!(dest)
        ),
    ] {
        let reply = http
            .raw(
                "/files/compose",
                format!("{{\"source_paths\":[{}],{fields}}}", json!(src)),
            )
            .await;
        reply.error(
            400,
            "json: cannot unmarshal number into Go struct field ComposeRequest.",
        );
        assert_eq!(fs::read(&src).unwrap(), b"new");
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        no_temporary_files(dir.path());
    }
}

#[tokio::test]
async fn duplicate_source_array_null_reuses_go_slice_elements() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = dir.path().join("out");
    for fields in [
        "\"source_paths\":[null]",
        "\"source_paths\":[null,\"ignored\"],\"source_paths\":[null],\"source_paths\":[null]",
    ] {
        let body = format!(
            "{{\"source_paths\":[{}],{fields},\"destination\":{},\"destination\":null}}",
            json!(src),
            json!(dest)
        );
        let reply = http.raw("/files/compose", body).await;
        assert_eq!(
            reply.status,
            200,
            "{}",
            String::from_utf8_lossy(&reply.bytes)
        );
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!src.exists());
        fs::write(&src, "new").unwrap();
    }
}

#[tokio::test]
async fn malformed_utf8_in_strings_is_replaced_per_byte_like_go() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let expected = dir.path().join("out\u{fffd}\u{fffd}");
    let prefix = format!(
        "{{\"source_paths\":[{}],\"destination\":\"{}/out",
        json!(src),
        dir.path().display()
    );
    let mut body = prefix.into_bytes();
    body.extend_from_slice(b"\xe2\x82\"}");
    let reply = http
        .request(
            Request::post(format!("{}/files/compose", http.origin))
                .header("content-type", "application/json")
                .body(Full::new(bytes::Bytes::from(body)))
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["path"], json!(expected));
    assert_eq!(fs::read(expected).unwrap(), b"new");
}

#[tokio::test]
async fn unicode_folded_keys_and_ignored_large_numbers_follow_go() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = dir.path().join("out");
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .unwrap();
    let body = format!(
        "{{\"ſource_paths\":[{}],\"deſtination\":{},\"uſername\":{},\"ignored\":1e400}}",
        json!(src),
        json!(dest),
        json!(user.name)
    );
    assert_eq!(http.raw("/files/compose", body).await.status, 200);
    assert_eq!(fs::read(&dest).unwrap(), b"new");
    assert!(!src.exists());
}

#[tokio::test]
async fn ignored_nested_json_uses_go_ten_thousand_container_limit() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    for (depth, status, message) in [
        (9999, 400, "source_paths must not be empty"),
        (
            10000,
            400,
            "invalid request body: invalid character '[' exceeded max depth",
        ),
    ] {
        let body = format!(
            "{{\"ignored\":{}0{}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let reply = http.raw("/files/compose", body).await;
        reply.error(status, message);
        assert_eq!(reply.json()["message"], message);
    }
}

#[tokio::test]
async fn malformed_json_reports_exact_go_decoder_messages() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    for (body, message) in [
        ("", "EOF"),
        (" \n\t", "EOF"),
        ("{", "unexpected EOF"),
        ("{\"destination\":", "unexpected EOF"),
        (
            "{]",
            "invalid character ']' looking for beginning of object key string",
        ),
        (
            "{\"destination\" 1}",
            "invalid character '1' after object key",
        ),
        (
            "{\"source_paths\":[1,]}",
            "invalid character ']' looking for beginning of value",
        ),
        (
            "{\"destination\":truX}",
            "invalid character 'X' in literal true (expecting 'e')",
        ),
        (
            "{\"destination\":\"\\x\"}",
            "invalid character 'x' in string escape code",
        ),
        (
            "{\"destination\":1e}",
            "invalid character '}' in exponent of numeric literal",
        ),
    ] {
        let reply = http.raw("/files/compose", body.into()).await;
        let expected = format!("invalid request body: {message}");
        reply.error(400, &expected);
        assert_eq!(reply.json()["message"], expected);
    }
}

#[tokio::test]
async fn compose_responds_after_one_value_with_chunked_sender_still_open() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = dir.path().join("out");
    let body = json!({"source_paths":[src],"destination":dest}).to_string();
    let address = http.origin.trim_start_matches("http://");
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.write_all(format!("POST /files/compose HTTP/1.1\r\nHost: {address}\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:x}\r\n{body}\r\n", body.len()).as_bytes()).await.unwrap();
    let mut response = vec![0; 8192];
    let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut response))
        .await
        .expect("one complete JSON value must respond before the final chunk")
        .unwrap();
    assert!(String::from_utf8_lossy(&response[..count]).starts_with("HTTP/1.1 200"));
    assert_eq!(fs::read(dest).unwrap(), b"new");
    assert!(!src.exists());
}

#[tokio::test]
async fn source_open_failure_has_go_path_error_wording() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = source(dir.path(), "out", "old");
    let _permissions = PermissionsGuard::set(&src, 0o000);
    let reply = http.compose(std::slice::from_ref(&src), &dest).await;
    let expected = format!(
        "error opening source file {}: open {}: permission denied",
        src.display(),
        src.display()
    );
    reply.error(500, &expected);
    assert_eq!(reply.json()["message"], expected);
    assert_eq!(fs::read(&dest).unwrap(), b"old");
    no_temporary_files(dir.path());
}

#[tokio::test]
async fn slash_destination_creates_go_dir_and_go_quotes_control_paths() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = format!("{}/newdir/", dir.path().display());
    let reply = http
        .raw(
            "/files/compose",
            json!({"source_paths":[src],"destination":dest}).to_string(),
        )
        .await;
    reply.error(500, "error finalizing compose: rename ");
    assert!(dir.path().join("newdir").is_dir());
    assert_eq!(fs::read(&src).unwrap(), b"new");
    no_temporary_files(&dir.path().join("newdir"));
    let old = source(dir.path(), "out", "old");
    let dest = format!("{}/missing/../out", dir.path().display());
    let reply = http
        .raw(
            "/files/compose",
            json!({"source_paths":[src],"destination":dest}).to_string(),
        )
        .await;
    reply.error(500, "error creating destination file:");
    assert_eq!(fs::read(&src).unwrap(), b"new");
    assert_eq!(fs::read(&old).unwrap(), b"old");
    assert!(!dir.path().join("missing").exists());
}

#[tokio::test]
async fn go_quotes_control_paths() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    for (rune, escaped) in [
        ('\u{7}', "\\a"),
        ('\u{2000}', "\\u2000"),
        ('\u{378}', "\\u0378"),
    ] {
        let path = format!("{}/bell{rune}", dir.path().display());
        let reply = http
            .raw(
                "/files/compose",
                json!({"source_paths":[path],"destination":path}).to_string(),
            )
            .await;
        reply.error(400, "cannot be the same as destination");
        assert_eq!(
            reply.json()["message"],
            format!(
                "source path \"{}/bell{escaped}\" cannot be the same as destination",
                dir.path().display()
            )
        );
    }
}

#[tokio::test]
async fn nonprintable_scanner_bytes_use_go_character_quoting() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let reply = http
        .request(
            Request::post(format!("{}/files/compose", http.origin))
                .body(Full::new(bytes::Bytes::from_static(b"{\x85}")))
                .unwrap(),
        )
        .await;
    assert_eq!(reply.json()["message"], "invalid request body: invalid character '\\u0085' looking for beginning of object key string");
}

#[test]
fn copy_limit_error_reports_temporary_destination_and_preserves_files() {
    const CHILD: &str = "ENVD_COMPOSE_FSIZE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "copy_limit_error_reports_temporary_destination_and_preserves_files",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let http = Http::new(dir.path()).await;
            let src = source(dir.path(), "part", vec![b'x'; 8192]);
            let dest = source(dir.path(), "out", "old");
            let _file_size_limit = compose_file_limit::FileSizeLimit::set(1024);
            let reply = http.compose(std::slice::from_ref(&src), &dest).await;
            reply.error(
                500,
                &format!(
                    "error composing source {}: write {}.e2b-compose.",
                    src.display(),
                    dest.display()
                ),
            );
            assert!(reply.json()["message"]
                .as_str()
                .unwrap()
                .ends_with(": copy_file_range: file too large"));
            assert_eq!(fs::read(&dest).unwrap(), b"old");
            assert_eq!(fs::metadata(&src).unwrap().len(), 8192);
            no_temporary_files(dir.path());
        });
}

#[tokio::test]
async fn success_response_uses_go_html_escaping_and_struct_field_order() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let src = source(dir.path(), "part", "new");
    let dest = dir.path().join("<out>&\u{2028}\u{2029}");
    let reply = http.compose(std::slice::from_ref(&src), &dest).await;
    assert_eq!(reply.status, 200);
    let path = serde_json::to_string(&dest).unwrap();
    let name = serde_json::to_string(dest.file_name().unwrap().to_str().unwrap()).unwrap();
    let expected = format!("{{\"name\":{name},\"path\":{path},\"type\":\"file\"}}\n")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    assert_eq!(reply.bytes, expected.as_bytes());
}

#[tokio::test]
async fn cleanup_failure_does_not_fail_successful_compose() {
    let dir = tempfile::tempdir().unwrap();
    let http = Http::new(dir.path()).await;
    let source_dir = dir.path().join("sources");
    fs::create_dir(&source_dir).unwrap();
    let src = source(&source_dir, "part", "retained source");
    let dest = dir.path().join("out");
    let _permissions = PermissionsGuard::set(&source_dir, 0o500);
    assert_eq!(
        http.compose(std::slice::from_ref(&src), &dest).await.status,
        200
    );
    assert_eq!(fs::read(dest).unwrap(), b"retained source");
    assert_eq!(fs::read(src).unwrap(), b"retained source");
    no_temporary_files(dir.path());
}
