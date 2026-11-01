use coorworker_envd::mmds::HyperTransport;
use coorworker_envd::mmds::{
    BoxFuture, CommandOutput, CommandRunner, HttpReply, HttpRequest, HttpTransport, IptablesRoute,
    MmdsClient, MmdsOpts, RoutePin, TokenHashProvider,
};
use std::{
    collections::VecDeque,
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn headers(stream: &mut tokio::net::TcpStream) -> String {
    let mut data = Vec::new();
    while !data.ends_with(b"\r\n\r\n") {
        data.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(data).unwrap()
}
async fn loopback_listener() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    (listener, url)
}
fn request(url: String) -> HttpRequest {
    HttpRequest {
        method: "PUT".into(),
        url,
        headers: vec![("X-metadata-token-ttl-seconds".into(), b"60".to_vec())],
        body: Vec::new(),
    }
}

#[tokio::test]
async fn hyper_redirects_rewrite_methods_strip_body_headers_and_close_connections() {
    for (status, method, retains_content) in [
        (301, "GET", false),
        (302, "GET", false),
        (303, "GET", false),
        (307, "PUT", true),
        (308, "PUT", true),
    ] {
        let (listener, url) = loopback_listener().await;
        let origin = url.clone();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let first_headers = headers(&mut first).await;
            assert!(first_headers.starts_with("PUT / HTTP/1.1\r\n"));
            assert!(first_headers
                .to_lowercase()
                .contains("connection: close\r\n"));
            first
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Redirect\r\nLocation: next\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            assert_eq!(first.read(&mut [0u8; 1]).await.unwrap(), 0);
            let (mut second, _) = listener.accept().await.unwrap();
            let received = headers(&mut second).await;
            assert!(
                received.starts_with(&format!("{method} /next HTTP/1.1\r\n")),
                "{received}"
            );
            let lower = received.to_lowercase();
            assert_eq!(
                lower.contains("content-type: application/json\r\n"),
                retains_content
            );
            assert!(lower.contains("x-metadata-token-ttl-seconds: 60\r\n"));
            assert!(lower.contains(&format!("referer: {origin}\r\n")));
            second
                .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 5\r\n\r\ntoken")
                .await
                .unwrap();
        });
        let mut req = request(url);
        req.headers
            .push(("Content-Type".into(), b"application/json".to_vec()));
        let response = HyperTransport.request(req).await.unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(response.body, b"token");
        server.await.unwrap();
    }
}
#[tokio::test]
async fn hyper_follows_at_most_ten_requests_and_returns_locationless_redirect() {
    let (listener, url) = loopback_listener().await;
    let server = tokio::spawn(async move {
        for _ in 0..10 {
            let (mut stream, _) = listener.accept().await.unwrap();
            headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: /again\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        }
    });
    assert!(HyperTransport
        .request(request(url))
        .await
        .unwrap_err()
        .to_string()
        .contains("stopped after 10 redirects"));
    server.await.unwrap();
    let (listener, url) = loopback_listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 301 Found\r\nContent-Length: 4\r\n\r\nhere")
            .await
            .unwrap();
    });
    assert_eq!(
        HyperTransport.request(request(url)).await.unwrap().body,
        b"here"
    );
    server.await.unwrap();
}
#[tokio::test]
async fn canceling_hyper_body_read_closes_its_socket() {
    let (listener, url) = loopback_listener().await;
    let (ready, started) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\na")
            .await
            .unwrap();
        ready.send(()).unwrap();
        let closed = stream.read(&mut [0u8; 1]).await;
        assert!(
            matches!(closed, Ok(0))
                || closed.is_err_and(|error| error.kind() == io::ErrorKind::ConnectionReset)
        );
    });
    let client = tokio::spawn(async move { HyperTransport.request(request(url)).await });
    if tokio::time::timeout(std::time::Duration::from_secs(1), started)
        .await
        .is_err()
    {
        client.abort();
        server.abort();
        panic!("transport never connected to loopback fixture");
    }
    client.abort();
    assert!(client.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
}

struct Script {
    replies: Mutex<VecDeque<io::Result<HttpReply>>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl HttpTransport for Script {
    fn request(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async {
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
        })
    }
}
struct Pin {
    calls: AtomicUsize,
    fail: bool,
}
impl RoutePin for Pin {
    fn pin(&self) -> BoxFuture<'_, io::Result<()>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            if self.fail {
                Err(io::Error::other("injected pin failure"))
            } else {
                Ok(())
            }
        })
    }
}
fn reply(status: u16, body: &str) -> io::Result<HttpReply> {
    Ok(HttpReply {
        status,
        headers: vec![],
        body: body.as_bytes().to_vec(),
    })
}
fn script(replies: Vec<io::Result<HttpReply>>) -> Arc<Script> {
    Arc::new(Script {
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(vec![]),
    })
}
fn pin() -> Arc<Pin> {
    Arc::new(Pin {
        calls: AtomicUsize::new(0),
        fail: false,
    })
}

#[path = "mmds/regressions.rs"]
mod regressions;

#[test]
fn metadata_json_replaces_unpaired_surrogates_like_go() {
    assert_eq!(
        MmdsOpts::from_json(br#"{"instanceID":"\ud800x\udc00","accessTokenHash":"ok"}"#)
            .unwrap()
            .instance_id,
        "�x�"
    );
}

#[tokio::test]
async fn hyper_decompresses_implicit_gzip_and_retains_explicit_encoding() {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"decoded").unwrap();
    let encoded = encoder.finish().unwrap();
    let (listener, url) = loopback_listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let received = headers(&mut stream).await.to_lowercase();
        assert!(received.contains("accept-encoding: gzip\r\n"));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                    encoded.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&encoded).await.unwrap();
    });
    let outcome = HyperTransport.request(request(url)).await;
    server.await.unwrap();
    assert_eq!(outcome.unwrap().body, b"decoded");
}

#[tokio::test(start_paused = true)]
async fn request_timeout_cancels_transport_after_ten_seconds() {
    struct Pending;
    impl HttpTransport for Pending {
        fn request(&self, _: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
            Box::pin(std::future::pending())
        }
    }
    let client = MmdsClient::new(Arc::new(Pending), pin());
    let start = tokio::time::Instant::now();
    assert_eq!(
        client.fetch_opts().await.unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(start.elapsed(), std::time::Duration::from_secs(10));
}

#[tokio::test]
async fn noop_provider_is_explicit_and_empty() {
    assert_eq!(
        coorworker_envd::mmds::NoopProvider
            .token_hash()
            .await
            .unwrap(),
        ""
    );
}

#[tokio::test]
async fn metadata_failure_retries_a_fresh_token_and_preserves_raw_token_bytes() {
    let transport = script(vec![
        Ok(HttpReply {
            status: 200,
            headers: vec![],
            body: vec![0xff],
        }),
        Err(io::Error::other("get failed")),
        reply(200, "fresh"),
        reply(200, r#"{"accessTokenHash":"ok"}"#),
    ]);
    let route = pin();
    let client = MmdsClient::new(transport.clone(), route.clone());
    assert_eq!(client.token_hash().await.unwrap(), "ok");
    assert_eq!(
        transport.requests.lock().unwrap()[1].headers[0].1,
        vec![0xff]
    );
    assert_eq!(route.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn route_coalesces_concurrent_repairs_and_cancellation_releases_gate() {
    struct Gated {
        calls: AtomicUsize,
        started: tokio::sync::Notify,
    }
    impl CommandRunner for Gated {
        fn run(&self, _: Vec<String>) -> BoxFuture<'_, io::Result<CommandOutput>> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                std::future::pending().await
            })
        }
    }
    let runner = Arc::new(Gated {
        calls: AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
    });
    let route = Arc::new(IptablesRoute::new(runner.clone()));
    let first = {
        let route = route.clone();
        tokio::spawn(async move { route.pin().await })
    };
    runner.started.notified().await;
    route.pin().await.unwrap();
    assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    first.abort();
    let _ = first.await;
    let second = {
        let route = route.clone();
        tokio::spawn(async move { route.pin().await })
    };
    runner.started.notified().await;
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    second.abort();
    let _ = second.await;
}

#[tokio::test]
async fn https_redirect_validates_native_roots_and_hostname_in_isolated_process() {
    const CHILD: &str = "ENVD_MMDS_TLS_FIXTURE";
    if let Ok(root) = std::env::var(CHILD) {
        use tokio_rustls::rustls::{
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
            ServerConfig,
        };
        let cert = std::fs::read(format!("{root}/cert.der")).unwrap();
        let key = std::fs::read(format!("{root}/key.der")).unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let (tls, _) = loopback_listener().await;
        let address = tls.local_addr().unwrap();
        let (plain, url) = loopback_listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = plain.accept().await.unwrap();
            headers(&mut stream).await;
            stream.write_all(format!("HTTP/1.1 307 Redirect\r\nLocation: https://localhost:{}/secure\r\nContent-Length: 0\r\n\r\n",address.port()).as_bytes()).await.unwrap();
            let (stream, _) = tls.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            let mut data = Vec::new();
            while !data.ends_with(b"\r\n\r\n") {
                data.push(stream.read_u8().await.unwrap());
            }
            assert!(data.starts_with(b"PUT /secure HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecure")
                .await
                .unwrap();
            let (stream, _) = tls.accept().await.unwrap();
            assert!(acceptor.accept(stream).await.is_err());
        });
        assert_eq!(
            HyperTransport.request(request(url)).await.unwrap().body,
            b"secure"
        );
        // The same trusted certificate has no IP SAN: verification must reject it.
        assert!(HyperTransport
            .request(request(format!("https://{address}/")))
            .await
            .is_err());
        server.await.unwrap();
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("openssl")
            .args(args)
            .current_dir(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    };
    run(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "key.pem",
        "-out",
        "cert.pem",
        "-days",
        "1",
        "-subj",
        "/CN=localhost",
        "-addext",
        "subjectAltName=DNS:localhost",
        "-addext",
        "basicConstraints=critical,CA:FALSE",
    ]);
    run(&[
        "x509", "-in", "cert.pem", "-outform", "DER", "-out", "cert.der",
    ]);
    run(&[
        "pkcs8", "-topk8", "-nocrypt", "-in", "key.pem", "-outform", "DER", "-out", "key.der",
    ]);
    let status = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "https_redirect_validates_native_roots_and_hostname_in_isolated_process",
            "--nocapture",
        ])
        .env(CHILD, root)
        .env("SSL_CERT_FILE", root.join("cert.pem"))
        .env("SSL_CERT_DIR", root.join("absent"))
        .status()
        .await
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn redirect_sensitive_headers_stay_removed_when_returning_to_original_host() {
    let (first, url) = loopback_listener().await;
    let (second, _) = loopback_listener().await;
    let second_port = second.local_addr().unwrap().port();
    let return_url = url.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = first.accept().await.unwrap();
        assert!(headers(&mut stream)
            .await
            .to_lowercase()
            .contains("authorization: bearer secret\r\n"));
        stream.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://localhost:{second_port}/hop\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
        let (mut stream, _) = second.accept().await.unwrap();
        let received = headers(&mut stream).await.to_lowercase();
        assert!(!received.contains("authorization:"));
        assert!(!received.contains("cookie:"));
        assert!(received.contains("x-metadata-token-ttl-seconds: 60"));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 307 Redirect\r\nLocation: {return_url}\r\nContent-Length: 0\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let (mut stream, _) = first.accept().await.unwrap();
        let received = headers(&mut stream).await.to_lowercase();
        assert!(!received.contains("authorization:"));
        assert!(!received.contains("content-type:"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });
    let mut req = request(url);
    req.headers.extend([
        ("Authorization".into(), b"Bearer secret".to_vec()),
        ("Cookie".into(), b"secret=yes".to_vec()),
        ("Content-Type".into(), b"application/json".to_vec()),
    ]);
    assert_eq!(HyperTransport.request(req).await.unwrap().body, b"ok");
    server.await.unwrap();
}

#[test]
fn metadata_json_follows_go_case_duplicates_null_and_type_rules() {
    let opts = MmdsOpts::from_json(br#"{"instanceID":"old","INSTANCEid":"new","envID":"t","address":"a","accessTokenHash":"hash","instanceID":null,"extra":1}"#).unwrap();
    assert_eq!(
        opts,
        MmdsOpts {
            instance_id: "new".into(),
            env_id: "t".into(),
            address: "a".into(),
            access_token_hash: "hash".into()
        }
    );
    assert_eq!(MmdsOpts::from_json(b"null").unwrap(), MmdsOpts::default());
    for body in [
        b"[]".as_slice(),
        b"{\"instanceID\":2}",
        b"{\"accessTokenHash\":false}",
        b"{",
        b"{} {}",
    ] {
        assert!(MmdsOpts::from_json(body).is_err());
    }
}

#[tokio::test]
async fn every_lookup_acquires_a_fresh_token_and_ignores_http_status() {
    let transport = script(vec![
        reply(500, "token1"),
        reply(403, r#"{"accessTokenHash":"hash1"}"#),
        reply(200, "token2"),
        reply(200, r#"{"accessTokenHash":"hash2"}"#),
    ]);
    let route = pin();
    let client = MmdsClient::new(transport.clone(), route.clone());
    assert_eq!(client.token_hash().await.unwrap(), "hash1");
    assert_eq!(client.token_hash().await.unwrap(), "hash2");
    assert_eq!(route.calls.load(Ordering::SeqCst), 0);
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].url, "http://169.254.169.254/latest/api/token");
    assert!(requests[0]
        .headers
        .contains(&("X-metadata-token-ttl-seconds".into(), b"60".to_vec())));
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].url, "http://169.254.169.254/");
    assert!(requests[1]
        .headers
        .contains(&("X-metadata-token".into(), b"token1".to_vec())));
    assert!(requests[1]
        .headers
        .contains(&("Accept".into(), b"application/json".to_vec())));
}

#[tokio::test]
async fn lookup_failure_pins_once_and_retries_full_exchange_even_if_pin_fails() {
    let transport = script(vec![
        reply(200, ""),
        reply(200, "new"),
        reply(200, r#"{"accessTokenHash":"ok"}"#),
    ]);
    let route = Arc::new(Pin {
        calls: AtomicUsize::new(0),
        fail: true,
    });
    let client = MmdsClient::new(transport.clone(), route.clone());
    assert_eq!(client.token_hash().await.unwrap(), "ok");
    assert_eq!(route.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn empty_hash_is_success_without_route_repair_and_second_failure_is_returned() {
    let route = pin();
    let client = MmdsClient::new(
        script(vec![reply(200, "token"), reply(200, "{}")]),
        route.clone(),
    );
    assert_eq!(client.token_hash().await.unwrap(), "");
    assert_eq!(route.calls.load(Ordering::SeqCst), 0);
    let client = MmdsClient::new(script(vec![reply(200, ""), reply(200, "")]), route.clone());
    assert_eq!(
        client.token_hash().await.unwrap_err().to_string(),
        "failed to get MMDS token: mmds token is an empty string"
    );
    assert_eq!(route.calls.load(Ordering::SeqCst), 1);
}

struct Runner {
    calls: Mutex<Vec<Vec<String>>>,
    fail_insert: bool,
}
impl CommandRunner for Runner {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, io::Result<CommandOutput>> {
        let insert = args.iter().any(|arg| arg == "-I");
        self.calls.lock().unwrap().push(args);
        Box::pin(async move {
            Ok(CommandOutput {
                success: insert && !self.fail_insert,
                output: b"injected\n".to_vec(),
                error: "exit status 1".into(),
            })
        })
    }
}
#[tokio::test]
async fn route_delete_failure_is_ignored_and_insert_order_and_arguments_are_exact() {
    let runner = Arc::new(Runner {
        calls: Mutex::new(vec![]),
        fail_insert: false,
    });
    IptablesRoute::new(runner.clone()).pin().await.unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    for (index, chain) in ["PREROUTING", "OUTPUT"].into_iter().enumerate() {
        for (offset, operation) in ["-D", "-I"].into_iter().enumerate() {
            let mut expected = vec!["-w", "5", "-t", "nat", operation, chain];
            if offset == 1 {
                expected.push("1");
            }
            expected.extend([
                "-d",
                "169.254.169.254",
                "-p",
                "tcp",
                "--dport",
                "80",
                "-j",
                "RETURN",
            ]);
            assert_eq!(calls[index * 2 + offset], expected);
        }
    }
}
#[tokio::test]
async fn first_route_insert_error_stops_before_second_chain() {
    let runner = Arc::new(Runner {
        calls: Mutex::new(vec![]),
        fail_insert: true,
    });
    assert_eq!(
        IptablesRoute::new(runner.clone())
            .pin()
            .await
            .unwrap_err()
            .to_string(),
        "iptables -I nat PREROUTING: exit status 1: injected\n"
    );
    assert_eq!(runner.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn route_pin_error_carries_the_go_exec_error_text() {
    // When the iptables lookup itself fails, Go's CombinedOutput error is the
    // bare exec.Error (no output, no "exit status" prefix); the pin wraps it
    // as `iptables -I nat <chain>: <text>` (mmds_route_linux.go:39-41, 48-55).
    struct LookupMiss;
    impl CommandRunner for LookupMiss {
        fn run(&self, _: Vec<String>) -> BoxFuture<'_, io::Result<CommandOutput>> {
            Box::pin(async {
                Err(io::Error::other(
                    "exec: \"iptables\": executable file not found in $PATH",
                ))
            })
        }
    }
    assert_eq!(
        IptablesRoute::new(Arc::new(LookupMiss))
            .pin()
            .await
            .unwrap_err()
            .to_string(),
        "iptables -I nat PREROUTING: exec: \"iptables\": executable file not found in $PATH"
    );
}

struct CapturingSink(std::sync::Mutex<Vec<Vec<u8>>>);
impl coorworker_envd::logs::LogSink for CapturingSink {
    fn write_line(&self, line: &[u8]) {
        self.0.lock().unwrap().push(line.to_vec());
    }
}

fn limiter_clock() -> (
    Arc<std::sync::atomic::AtomicI64>,
    Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
) {
    use std::sync::atomic::AtomicI64;
    use std::time::{Duration, Instant};
    let millis = Arc::new(AtomicI64::new(0));
    let handle = millis.clone();
    let base = Instant::now();
    (
        millis,
        Arc::new(move || base + Duration::from_millis(handle.load(Ordering::SeqCst) as u64)),
    )
}

#[tokio::test]
async fn pin_failure_warns_once_per_window_as_a_zerolog_envd_event() {
    use coorworker_envd::logs::ratelimit::Limiter;
    use coorworker_envd::logs::Logger;
    let shared = Arc::new(CapturingSink(std::sync::Mutex::new(Vec::new())));
    let logger = Logger::new(vec![shared.clone()]).with_str("logger", "envd");
    let (millis, now) = limiter_clock();
    let limiter = Limiter::with_clock(std::time::Duration::from_secs(10), now);
    // Each failing round is a full exchange plus the post-pin retry: PUT
    // token, failing GET, then PUT token and failing GET again.
    let failing_round = || -> Vec<io::Result<HttpReply>> {
        vec![
            reply(200, "token"),
            reply(500, "x"),
            reply(200, "token"),
            reply(500, "x"),
        ]
    };
    let mut replies = Vec::new();
    for _ in 0..3 {
        replies.extend(failing_round());
    }
    let transport = script(replies);
    // A route pin failure carries the real IptablesRoute error shape
    // (`iptables -I nat <chain>: <text>`, route.rs pin()).
    struct FailingRoute;
    impl RoutePin for FailingRoute {
        fn pin(&self) -> BoxFuture<'_, io::Result<()>> {
            Box::pin(async {
                Err(io::Error::other(
                    "iptables -I nat PREROUTING: injected pin failure",
                ))
            })
        }
    }
    let client = MmdsClient::new(transport, Arc::new(FailingRoute))
        .with_pin_logger(logger)
        .with_pin_limiter(limiter);
    // First failure emits the warn with suppressed=0 (init.go:88-93).
    assert!(client.token_hash().await.is_err());
    // A failure inside the window is suppressed and counted, not emitted.
    millis.store(1_000, Ordering::SeqCst);
    assert!(client.token_hash().await.is_err());
    // Past the window the warn emits again, carrying the suppressed count.
    millis.store(10_001, Ordering::SeqCst);
    assert!(client.token_hash().await.is_err());
    let lines = shared.0.lock().unwrap();
    assert_eq!(lines.len(), 2, "expected exactly two warn lines");
    let first: serde_json::Value = serde_json::from_slice(&lines[0]).unwrap();
    assert_eq!(first["level"], "warn");
    assert_eq!(first["logger"], "envd");
    assert_eq!(
        first["error"],
        "iptables -I nat PREROUTING: injected pin failure"
    );
    assert_eq!(first["suppressed"], 0);
    assert_eq!(first["message"], "failed to pin MMDS iptables route");
    assert!(first["timestamp"].is_string());
    let second: serde_json::Value = serde_json::from_slice(&lines[1]).unwrap();
    assert_eq!(second["suppressed"], 1);
}

#[tokio::test]
async fn pin_success_and_success_after_repair_never_warn() {
    use coorworker_envd::logs::Logger;
    let shared = Arc::new(CapturingSink(std::sync::Mutex::new(Vec::new())));
    let logger = Logger::new(vec![shared.clone()]).with_str("logger", "envd");
    // A failed lookup that a successful repair answers must not warn.
    let transport = script(vec![reply(200, ""), reply(200, "token"), reply(200, "{}")]);
    let route = pin();
    let client = MmdsClient::new(transport, route).with_pin_logger(logger.clone());
    assert_eq!(client.token_hash().await.unwrap(), "");
    assert!(shared.0.lock().unwrap().is_empty());
    // A fully healthy lookup warns about nothing either.
    let transport = script(vec![
        reply(200, "token"),
        reply(200, r#"{"accessTokenHash":"h"}"#),
    ]);
    let client = MmdsClient::new(transport, pin()).with_pin_logger(logger);
    assert_eq!(client.token_hash().await.unwrap(), "h");
    assert!(shared.0.lock().unwrap().is_empty());
}
