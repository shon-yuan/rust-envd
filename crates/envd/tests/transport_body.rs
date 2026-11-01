//! Transport body and redirect-body contract for the logs exporter.
//!
//! Oracle: `defer response.Body.Close()` after `client.Do` plus
//! `redirectBehavior` in the pinned Go net/http client (301-303 switch to GET
//! and drop the body, 307/308 replay it because `bytes.Buffer` sets GetBody)
//! as used by internal/logs/exporter/exporter.go:58-81.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use coorworker_envd::mmds::{HttpRequest, HttpTransport, HyperTransport};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    _target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

struct Response {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Send headers only, then hold the connection open without the declared body.
    stall_body: bool,
}

fn reply(status: u16, reason: &'static str, body: &[u8]) -> Response {
    Response {
        status,
        reason,
        headers: Vec::new(),
        body: body.to_vec(),
        stall_body: false,
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<Seen> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = socket.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..count]);
        let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = std::str::from_utf8(&buffer[..end]).ok()?;
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next()?.split_whitespace();
        let method = request_line.next()?.to_owned();
        let target = request_line.next()?.to_owned();
        let mut headers = Vec::new();
        for line in lines {
            let (key, value) = line.split_once(':')?;
            headers.push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
        let length = headers
            .iter()
            .find(|(key, _)| key == "content-length")
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buffer[end + 4..].to_vec();
        while body.len() < length {
            let count = socket.read(&mut chunk).await.ok()?;
            if count == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..count]);
        }
        body.truncate(length);
        return Some(Seen {
            method,
            _target: target,
            headers,
            body,
        });
    }
}

async fn write_response(socket: &mut TcpStream, response: Response) {
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, response.reason);
    for (key, value) in &response.headers {
        head.push_str(&format!("{key}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    head.push_str("Connection: close\r\n\r\n");
    if socket.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    if response.stall_body {
        // The declared body never arrives; the client must not wait for it.
        tokio::time::sleep(Duration::from_secs(3)).await;
        return;
    }
    let _ = socket.write_all(&response.body).await;
}

/// Raw HTTP/1.1 server: one connection per request, recorded in arrival order.
async fn server<F>(
    handler: F,
) -> (
    SocketAddr,
    Arc<Mutex<Vec<Seen>>>,
    tokio::task::JoinHandle<()>,
)
where
    F: Fn(&Seen) -> Response + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let handler = Arc::new(handler);
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let recorded = recorded.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let Some(request) = read_request(&mut socket).await else {
                    return;
                };
                recorded.lock().unwrap().push(request.clone());
                write_response(&mut socket, handler(&request)).await;
            });
        }
    });
    (address, seen, task)
}

fn request(url: String, method: &str, body: &[u8]) -> HttpRequest {
    HttpRequest {
        method: method.to_owned(),
        url,
        headers: vec![("Content-Type".to_owned(), b"application/json".to_vec())],
        body: body.to_vec(),
    }
}

#[tokio::test]
async fn post_sends_the_request_body_with_content_type_and_length() {
    let (address, seen, task) = server(|_| reply(204, "No Content", b"")).await;
    let url = format!("http://{address}/collect");
    let body = br#"{"message":"first"}"#;
    let response = HyperTransport
        .request(request(url, "POST", body))
        .await
        .unwrap();
    task.abort();
    assert_eq!(response.status, 204);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].body, body);
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(
        seen[0].header("content-length"),
        Some(body.len().to_string().as_str())
    );
}

#[tokio::test]
async fn redirect_303_drops_the_post_body_and_becomes_get() {
    let (target, target_seen, target_task) = server(|_| reply(204, "No Content", b"")).await;
    let location = format!("http://{target}/next");
    let (origin, origin_seen, origin_task) = server(move |_| Response {
        status: 303,
        reason: "See Other",
        headers: vec![("Location".to_owned(), location.clone())],
        body: Vec::new(),
        stall_body: false,
    })
    .await;

    let body = br#"{"message":"redirected"}"#;
    let response = HyperTransport
        .request(request(format!("http://{origin}/collect"), "POST", body))
        .await
        .unwrap();
    origin_task.abort();
    target_task.abort();

    assert_eq!(response.status, 204);
    let origin_seen = origin_seen.lock().unwrap();
    assert_eq!(origin_seen.len(), 1);
    assert_eq!(origin_seen[0].method, "POST");
    assert_eq!(origin_seen[0].body, body);
    let target_seen = target_seen.lock().unwrap();
    assert_eq!(target_seen.len(), 1);
    assert_eq!(target_seen[0].method, "GET");
    assert!(target_seen[0].body.is_empty(), "303 must drop the body");
    assert_eq!(target_seen[0].header("content-type"), None);
}

#[tokio::test]
async fn redirect_308_replays_the_original_post_body() {
    let (target, target_seen, target_task) = server(|_| reply(204, "No Content", b"")).await;
    let location = format!("http://{target}/next");
    let (origin, origin_seen, origin_task) = server(move |_| Response {
        status: 308,
        reason: "Permanent Redirect",
        headers: vec![("Location".to_owned(), location.clone())],
        body: Vec::new(),
        stall_body: false,
    })
    .await;

    let body = br#"{"message":"replayed"}"#;
    let response = HyperTransport
        .request(request(format!("http://{origin}/collect"), "POST", body))
        .await
        .unwrap();
    origin_task.abort();
    target_task.abort();

    assert_eq!(response.status, 204);
    let origin_seen = origin_seen.lock().unwrap();
    assert_eq!(origin_seen.len(), 1);
    assert_eq!(origin_seen[0].body, body);
    let target_seen = target_seen.lock().unwrap();
    assert_eq!(target_seen.len(), 1);
    assert_eq!(target_seen[0].method, "POST");
    assert_eq!(target_seen[0].body, body);
}

#[tokio::test]
async fn redirect_302_also_drops_the_post_body() {
    let (target, target_seen, target_task) = server(|_| reply(204, "No Content", b"")).await;
    let location = format!("http://{target}/next");
    let (origin, _, origin_task) = server(move |_| Response {
        status: 302,
        reason: "Found",
        headers: vec![("Location".to_owned(), location.clone())],
        body: Vec::new(),
        stall_body: false,
    })
    .await;

    HyperTransport
        .request(request(
            format!("http://{origin}/collect"),
            "POST",
            br#"{"message":"drop"}"#,
        ))
        .await
        .unwrap();
    origin_task.abort();
    target_task.abort();

    let target_seen = target_seen.lock().unwrap();
    assert_eq!(target_seen[0].method, "GET");
    assert!(target_seen[0].body.is_empty());
}

#[tokio::test]
async fn headers_only_request_returns_before_the_response_body_completes() {
    let (address, _, task) = server(|_| Response {
        status: 204,
        reason: "No Content",
        headers: Vec::new(),
        body: vec![0; 128],
        stall_body: true,
    })
    .await;
    let url = format!("http://{address}/collect");
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        HyperTransport.request_headers(request(url, "POST", br#"{"message":"x"}"#)),
    )
    .await;
    task.abort();
    let response = result.expect("headers-only request waited for the body");
    assert_eq!(response.unwrap().status, 204);
}

#[test]
fn header_names_are_case_insensitive_from_the_server_side() {
    // Guard the fixture itself: the assertions above read lowercase names.
    let seen = Seen {
        method: "POST".into(),
        _target: "/".into(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: Vec::new(),
    };
    let map: HashMap<_, _> = seen.headers.iter().cloned().collect();
    assert_eq!(
        map.get("content-type").map(String::as_str),
        Some("application/json")
    );
}
