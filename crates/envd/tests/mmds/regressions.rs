use super::*;

#[tokio::test]
async fn redirect_referer_retains_empty_path_in_url_string() {
    let (listener, url) = loopback_listener().await;
    let origin = url.trim_end_matches('/').to_owned();
    let expected = origin.clone();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        headers(&mut first).await;
        first
            .write_all(b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let (mut second, _) = listener.accept().await.unwrap();
        let received = headers(&mut second).await;
        second
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        assert!(
            received.contains(&format!("referer: {expected}\r\n")),
            "{received}"
        );
    });
    HyperTransport.request(request(origin)).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn ipv6_scoped_loopback_dials_zone_and_strips_it_from_host_header() {
    let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
        Ok(listener) => listener,
        Err(error) if matches!(error.raw_os_error(), Some(99 | 97 | 93)) => {
            eprintln!(
                "IPv6 loopback unavailable: {error}; scoped address remains covered by pure tests"
            );
            return;
        }
        Err(error) => panic!("IPv6 fixture bind failed: {error}"),
    };
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let received = headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        received
    });
    let result = HyperTransport
        .request(request(format!("http://[::1%25lo]:{port}/metadata")))
        .await;
    if result.is_err() {
        server.abort();
    }
    assert!(result.is_ok(), "{result:?}");
    let received = server.await.unwrap();
    assert!(
        received.contains(&format!("host: [::1]:{port}\r\n")),
        "{received}"
    );
}

fn gzip_member(body: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}
#[tokio::test]
async fn gzip_members_join_tokens_and_json_and_reject_a_bad_second_member() {
    for (first, second, expected) in [
        (b"tok".as_slice(), b"en".as_slice(), b"token".as_slice()),
        (
            b"{\"accessTokenHash\":".as_slice(),
            b"\"ok\"}".as_slice(),
            b"{\"accessTokenHash\":\"ok\"}".as_slice(),
        ),
    ] {
        let body = [gzip_member(first), gzip_member(second)].concat();
        assert_eq!(gzip_exchange(body).await.unwrap().body, expected);
    }
    let mut bad = gzip_member(b"other");
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(gzip_exchange([gzip_member(b"first"), bad].concat())
        .await
        .is_err());
}
async fn gzip_exchange(body: Vec<u8>) -> io::Result<HttpReply> {
    let (listener, url) = loopback_listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        headers(&mut stream).await;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
    });
    let result = HyperTransport.request(request(url)).await;
    server.await.unwrap();
    result
}
#[tokio::test]
async fn go_redirect_paths_preserve_escaped_dots_backslashes_and_reject_tabs() {
    for (location, target) in [
        ("/a/%2e%2e/b", "/a/%2e%2e/b"),
        (r"/a\b", "/a%5Cb"),
        (r"\\localhost/x", "/%5C%5Clocalhost/x"),
        ("/a/../b", "/b"),
        ("/a/%2F/b", "/a/%2F/b"),
        ("/a//b/./c/../", "/a//b/"),
        ("///a/b", "///a/b"),
        ("/a!b'c(d)*[x]", "/a!b'c(d)*[x]"),
        ("/a b", "/a%20b"),
        ("/a?x=%zz+q", "/a?x=%zz+q"),
        ("?", "/?"),
        ("#frag", "/"),
    ] {
        let (listener, url) = loopback_listener().await;
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            headers(&mut first).await;
            first
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let accepted =
                tokio::time::timeout(std::time::Duration::from_secs(1), listener.accept()).await;
            let Ok(Ok((mut second, _))) = accepted else {
                return None;
            };
            let received = headers(&mut second).await;
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
            Some(received)
        });
        let result = HyperTransport.request(request(url)).await;
        let received = server.await.unwrap();
        assert!(result.is_ok(), "{location}: {result:?}");
        assert!(
            received
                .unwrap()
                .starts_with(&format!("GET {target} HTTP/1.1\r\n")),
            "{location}"
        );
    }
    let (listener, url) = loopback_listener().await;
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        headers(&mut first).await;
        first
            .write_all(b"HTTP/1.1 302 Found\r\nLocation: /a\tb\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        if let Ok(Ok((mut second, _))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept()).await
        {
            headers(&mut second).await;
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        }
    });
    assert!(HyperTransport.request(request(url)).await.is_err());
    server.await.unwrap();
}
#[tokio::test]
async fn double_backslash_location_stays_on_original_listener_with_metadata_token() {
    let (original, url) = loopback_listener().await;
    let (other, _) = loopback_listener().await;
    let location = format!("/\\\\localhost:{}/trap", other.local_addr().unwrap().port());
    let expected = format!(
        "GET /%5C%5Clocalhost:{}/trap HTTP/1.1\r\n",
        other.local_addr().unwrap().port()
    );
    let server = tokio::spawn(async move {
        let (mut first, _) = original.accept().await.unwrap();
        headers(&mut first).await;
        first
            .write_all(
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        tokio::select! {
            result=original.accept()=> {
                let (mut second,_)=result.unwrap(); let received=headers(&mut second).await;
                second.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await.unwrap();
                assert!(received.starts_with(&expected),"{received}");
                assert!(received.to_lowercase().contains("x-metadata-token: secret\r\n"));
            }
            result=other.accept()=> {
                let (mut wrong,_)=result.unwrap(); let received=headers(&mut wrong).await;
                wrong.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await.unwrap();
                panic!("redirect changed destination: {received}");
            }
        }
    });
    let mut req = request(url);
    req.headers
        .push(("X-metadata-token".into(), b"secret".to_vec()));
    HyperTransport.request(req).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn redirect_userinfo_derives_basic_for_absent_or_empty_authorization_per_hop() {
    for auth in [None, Some(""), Some("Bearer retained")] {
        let (listener, url) = loopback_listener().await;
        let authority = url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            headers(&mut first).await;
            first.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://u%3As:p%40ss@{authority}/auth\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
            let (mut second, _) = listener.accept().await.unwrap();
            let authorized = headers(&mut second).await;
            second.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{authority}/plain\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
            let (mut third, _) = listener.accept().await.unwrap();
            let plain = headers(&mut third).await;
            third
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            (authorized, plain)
        });
        let mut req = request(url);
        if let Some(auth) = auth {
            req.headers
                .push(("Authorization".into(), auth.as_bytes().to_vec()));
        }
        HyperTransport.request(req).await.unwrap();
        let (authorized, plain) = server.await.unwrap();
        let expected = if auth == Some("Bearer retained") {
            "Bearer retained"
        } else {
            "Basic dTpzOnBAc3M="
        };
        assert!(
            authorized
                .to_lowercase()
                .contains(&format!("authorization: {}\r\n", expected.to_lowercase())),
            "{authorized}"
        );
        assert!(
            !plain.contains("Basic "),
            "derived credentials persisted: {plain}"
        );
    }
}
