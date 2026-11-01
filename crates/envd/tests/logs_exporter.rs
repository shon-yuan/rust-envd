//! MMDS logs exporter contract.
//!
//! Oracle: internal/logs/exporter/exporter.go at
//! 51083a83cc0903185d992725ea6bb33683814b29. Every HTTP behavior below runs
//! against a controlled local socket; no test reaches 169.254.169.254.

use std::{
    collections::VecDeque,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use coorworker_envd::{
    logs::exporter::{HttpExporter, MAX_BUFFERED_BYTES, MAX_LOG_LINE_BYTES},
    mmds::{BoxFuture, HttpReply, HttpRequest, HttpTransport, HyperTransport, MmdsOpts},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, Semaphore},
    time::Instant,
};

/// Injected transport that records requests, can hold each one on a gate, and
/// answers from a scripted status queue (0 means transport failure).
struct Recorder {
    requests: Mutex<Vec<(String, Vec<u8>)>>,
    started: Mutex<Option<mpsc::UnboundedSender<()>>>,
    hold: AtomicBool,
    gate: Semaphore,
    replies: Mutex<VecDeque<u16>>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            started: Mutex::new(None),
            hold: AtomicBool::new(false),
            gate: Semaphore::new(0),
            replies: Mutex::new(VecDeque::new()),
        }
    }
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn holding() -> Arc<Self> {
        let recorder = Self::new();
        recorder.hold.store(true, Ordering::SeqCst);
        recorder
    }

    fn started(&self) -> mpsc::UnboundedReceiver<()> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self.started.lock().unwrap() = Some(sender);
        receiver
    }

    fn release(&self, permits: usize) {
        self.gate.add_permits(permits);
    }

    fn reply(&self, status: u16) {
        self.replies.lock().unwrap().push_back(status);
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, body)| body.clone())
            .collect()
    }

    fn urls(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(url, _)| url.clone())
            .collect()
    }
}

impl HttpTransport for Recorder {
    fn request(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push((request.url.clone(), request.body.clone()));
            if let Some(started) = self.started.lock().unwrap().as_ref() {
                let _ = started.send(());
            }
            if self.hold.load(Ordering::SeqCst) {
                let _ = self.gate.acquire().await;
            }
            let status = self.replies.lock().unwrap().pop_front().unwrap_or(204);
            if status == 0 {
                return Err(io::Error::other("transport failure"));
            }
            Ok(HttpReply {
                status,
                headers: Vec::new(),
                body: Vec::new(),
            })
        })
    }
}

fn options(address: &str) -> MmdsOpts {
    MmdsOpts {
        instance_id: "sandbox-id".into(),
        env_id: "template-id".into(),
        address: address.into(),
        access_token_hash: String::new(),
    }
}

fn exporter(transport: Arc<dyn HttpTransport>) -> Arc<HttpExporter> {
    HttpExporter::new(transport)
}

fn future_deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[tokio::test]
async fn write_enqueues_synchronously_and_reports_the_line_length() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    let line = br#"{"message":"queued"}"#;
    assert_eq!(exporter.write(line).unwrap(), line.len());
    assert_eq!(exporter.buffered(), (1, line.len()));
    // A completed Write is visible to a flush barrier even with no send loop.
    exporter.store_opts(options("http://collector.invalid"));
    assert!(exporter.flush_and_purge(future_deadline()).await.is_ok());
    assert_eq!(recorder.count(), 1);
    assert_eq!(exporter.buffered(), (0, 0));
}

#[tokio::test]
async fn a_line_of_exactly_192_kib_is_accepted_and_one_byte_more_is_dropped() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    exporter.store_opts(options("http://collector.invalid"));

    let at_limit = vec![b'x'; MAX_LOG_LINE_BYTES];
    assert_eq!(exporter.write(&at_limit).unwrap(), MAX_LOG_LINE_BYTES);
    assert_eq!(exporter.buffered().0, 1);

    let oversized = vec![b'y'; MAX_LOG_LINE_BYTES + 1];
    assert_eq!(exporter.write(&oversized).unwrap(), MAX_LOG_LINE_BYTES + 1);
    assert_eq!(exporter.buffered().0, 1, "the oversized line is dropped");

    let _ = exporter.flush_and_purge(future_deadline()).await;
    // The accepted line is raw bytes, not JSON, so it is dropped at decoration
    // time; the oversized line never reached the queue either.
    assert_eq!(recorder.count(), 0);
}

#[tokio::test]
async fn the_queue_drops_oldest_lines_above_eight_mebibytes() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    let line = vec![b'x'; MAX_LOG_LINE_BYTES];
    let total = MAX_BUFFERED_BYTES / MAX_LOG_LINE_BYTES + 10;
    for _ in 0..total {
        exporter.add_logs(line.clone());
    }
    let (count, bytes) = exporter.buffered();
    assert!(count < total, "the bounded queue must evict old lines");
    assert!(bytes <= MAX_BUFFERED_BYTES, "buffered bytes stay bounded");
    assert_eq!(bytes, count * MAX_LOG_LINE_BYTES);
    let _ = exporter.flush_and_purge(future_deadline()).await;
    assert_eq!(exporter.buffered(), (0, 0));
}

#[tokio::test]
async fn flush_sends_the_calling_snapshot_and_purges_later_arrivals() {
    let recorder = Recorder::holding();
    let exporter = exporter(recorder.clone());
    exporter.store_opts(options("http://collector.invalid"));
    exporter.write(br#"{"sequence":1}"#).unwrap();
    exporter.write(br#"{"sequence":2}"#).unwrap();

    let mut started = recorder.started();
    let flush = {
        let exporter = exporter.clone();
        tokio::spawn(async move { exporter.flush_and_purge(future_deadline()).await })
    };
    started.recv().await.expect("first flush request");
    // This line arrives after the snapshot; the purge must drop it.
    exporter.write(br#"{"sequence":3}"#).unwrap();
    recorder.release(8);

    flush.await.unwrap().unwrap();
    assert_eq!(recorder.count(), 2, "the snapshot holds exactly two lines");
    // exporter.go:83-84 defers purgeLogs(), so a line that arrived after the
    // snapshot is discarded rather than sent or kept.
    assert_eq!(
        exporter.buffered(),
        (0, 0),
        "the exit purge drops later arrivals"
    );
}

#[tokio::test]
async fn flush_keeps_the_first_error_and_continues_sendable_lines() {
    let recorder = Recorder::new();
    recorder.reply(0); // transport failure
    recorder.reply(204);
    recorder.reply(0);
    let exporter = exporter(recorder.clone());
    exporter.store_opts(options("http://collector.invalid"));
    exporter.write(br#"{"sequence":1}"#).unwrap();
    exporter.write(br#"{"sequence":2}"#).unwrap();
    exporter.write(br#"{"sequence":3}"#).unwrap();

    let error = exporter
        .flush_and_purge(future_deadline())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("send instance logs"));
    assert_eq!(recorder.count(), 3, "a failed line never stops the rest");
    assert_eq!(exporter.buffered(), (0, 0));
}

#[tokio::test]
async fn flush_drops_malformed_json_and_keeps_sending_valid_lines() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    exporter.store_opts(options("http://collector.invalid"));
    exporter.write(b"not json").unwrap();
    exporter.write(br#"{"sequence":2}"#).unwrap();

    let error = exporter
        .flush_and_purge(future_deadline())
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("add instance logging options to JSON"));
    assert_eq!(recorder.count(), 1, "only the valid line is sent");
}

#[tokio::test]
async fn flush_without_options_purges_and_succeeds() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    exporter.write(br#"{"sequence":1}"#).unwrap();
    assert!(exporter.flush_and_purge(future_deadline()).await.is_ok());
    assert_eq!(recorder.count(), 0);
    assert_eq!(exporter.buffered(), (0, 0));
}

#[tokio::test]
async fn an_expired_flush_deadline_purges_without_sending() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    exporter.store_opts(options("http://collector.invalid"));
    exporter.write(br#"{"sequence":1}"#).unwrap();
    let deadline = Instant::now() - Duration::from_secs(1);
    let error = exporter.flush_and_purge(deadline).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(recorder.count(), 0, "an expired context sends nothing");
    assert_eq!(exporter.buffered(), (0, 0));
}

#[tokio::test]
async fn an_empty_collector_address_skips_sending_but_purges() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    exporter.store_opts(options(""));
    exporter.write(br#"{"sequence":1}"#).unwrap();
    assert!(exporter.flush_and_purge(future_deadline()).await.is_ok());
    assert_eq!(recorder.count(), 0);
    assert_eq!(exporter.buffered(), (0, 0));
}

#[tokio::test]
async fn one_flush_batch_uses_a_single_options_snapshot() {
    let recorder = Recorder::holding();
    let exporter = exporter(recorder.clone());
    exporter.store_opts(options("http://first.invalid"));
    exporter.write(br#"{"sequence":1}"#).unwrap();
    exporter.write(br#"{"sequence":2}"#).unwrap();

    let mut started = recorder.started();
    let flush = {
        let exporter = exporter.clone();
        tokio::spawn(async move { exporter.flush_and_purge(future_deadline()).await })
    };
    started.recv().await.expect("first flush request");
    // Options replaced mid-batch must not leak into this batch.
    exporter.store_opts(options("http://second.invalid"));
    recorder.release(8);
    flush.await.unwrap().unwrap();

    assert_eq!(
        recorder.urls(),
        vec![
            "http://first.invalid".to_owned(),
            "http://first.invalid".to_owned()
        ]
    );
    // A later batch picks up the replacement.
    exporter.write(br#"{"sequence":3}"#).unwrap();
    exporter.flush_and_purge(future_deadline()).await.unwrap();
    assert_eq!(recorder.urls().last().unwrap(), "http://second.invalid");
}

#[tokio::test]
async fn background_sending_starts_only_when_options_arrive() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    let (options_tx, options_rx) = mpsc::channel(1);
    let tasks = exporter.start(options_rx).unwrap();
    let mut started = recorder.started();

    exporter.write(br#"{"sequence":1}"#).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(recorder.count(), 0, "no MMDS options means no HTTP traffic");

    options_tx
        .send(options("http://collector.invalid"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.recv())
        .await
        .expect("background send after options")
        .expect("request notification");
    assert_eq!(recorder.count(), 1);
    assert_eq!(exporter.buffered(), (0, 0));
    tasks.shutdown().await;
}

#[tokio::test]
async fn background_replaces_options_between_batches() {
    let recorder = Recorder::new();
    let exporter = exporter(recorder.clone());
    let (options_tx, options_rx) = mpsc::channel(1);
    let tasks = exporter.start(options_rx).unwrap();
    let mut started = recorder.started();

    options_tx
        .send(options("http://first.invalid"))
        .await
        .unwrap();
    exporter.write(br#"{"sequence":1}"#).unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.recv())
        .await
        .expect("first batch")
        .expect("request notification");

    options_tx
        .send(options("http://second.invalid"))
        .await
        .unwrap();
    // Give the listener a chance to observe the replacement before the next batch.
    tokio::time::sleep(Duration::from_millis(50)).await;
    exporter.write(br#"{"sequence":2}"#).unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.recv())
        .await
        .expect("second batch")
        .expect("request notification");

    assert_eq!(
        recorder.urls(),
        vec![
            "http://first.invalid".to_owned(),
            "http://second.invalid".to_owned()
        ]
    );
    tasks.shutdown().await;
}

#[tokio::test]
async fn background_does_not_duplicate_a_line_taken_by_flush() {
    let recorder = Recorder::holding();
    let exporter = exporter(recorder.clone());
    let (options_tx, options_rx) = mpsc::channel(1);
    let tasks = exporter.start(options_rx).unwrap();
    let mut started = recorder.started();

    options_tx
        .send(options("http://collector.invalid"))
        .await
        .unwrap();
    exporter.write(br#"{"sequence":1}"#).unwrap();
    started.recv().await.expect("background took line 1");

    exporter.write(br#"{"sequence":2}"#).unwrap();
    let flush = {
        let exporter = exporter.clone();
        tokio::spawn(async move { exporter.flush_and_purge(future_deadline()).await })
    };
    started.recv().await.expect("flush took line 2");
    recorder.release(8);
    flush.await.unwrap().unwrap();
    tasks.shutdown().await;

    let bodies = recorder.bodies();
    assert_eq!(bodies.len(), 2, "each line is sent exactly once");
    assert_ne!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn background_survives_transport_and_decoration_errors() {
    let recorder = Recorder::new();
    recorder.reply(0);
    recorder.reply(204);
    let exporter = exporter(recorder.clone());
    let (options_tx, options_rx) = mpsc::channel(1);
    let tasks = exporter.start(options_rx).unwrap();
    let mut started = recorder.started();
    options_tx
        .send(options("http://collector.invalid"))
        .await
        .unwrap();

    exporter.write(b"not json").unwrap();
    exporter.write(br#"{"sequence":2}"#).unwrap();
    exporter.write(br#"{"sequence":3}"#).unwrap();
    started.recv().await.expect("first send attempt");
    started.recv().await.expect("second send attempt");
    // One malformed line is dropped before any request; the transport failure
    // on the next line does not stop the following line.
    assert_eq!(recorder.count(), 2);
    tasks.shutdown().await;
}

#[tokio::test]
async fn writes_stay_bounded_and_fast_while_a_background_send_is_stalled() {
    let recorder = Recorder::holding();
    let exporter = exporter(recorder.clone());
    let (options_tx, options_rx) = mpsc::channel(1);
    let tasks = exporter.start(options_rx).unwrap();
    let mut started = recorder.started();
    options_tx
        .send(options("http://collector.invalid"))
        .await
        .unwrap();
    exporter.write(br#"{"sequence":0}"#).unwrap();
    started.recv().await.expect("stalled send");

    let line = format!("{{\"message\":\"{}\"}}", "x".repeat(4096));
    let began = std::time::Instant::now();
    for index in 0..500 {
        exporter.write(line.as_bytes()).unwrap();
        if index % 50 == 0 {
            assert!(exporter.buffered().1 <= MAX_BUFFERED_BYTES);
        }
    }
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "Write blocked on the stalled collector"
    );
    recorder.release(8);
    tasks.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_an_in_flight_background_send() {
    let recorder = Recorder::holding();
    let exporter = exporter(recorder.clone());
    let (options_tx, options_rx) = mpsc::channel(1);
    let tasks = exporter.start(options_rx).unwrap();
    let mut started = recorder.started();
    options_tx
        .send(options("http://collector.invalid"))
        .await
        .unwrap();
    exporter.write(br#"{"sequence":1}"#).unwrap();
    started.recv().await.expect("stalled send");

    tokio::time::timeout(Duration::from_secs(1), tasks.shutdown())
        .await
        .expect("shutdown waited for the stalled request");
}

// ---- real controlled socket -------------------------------------------------

struct SocketRequest {
    head: String,
    body: Vec<u8>,
}

/// One-connection HTTP server; replies after reading the request. `stall`
/// declares a body length in the reply but never writes it.
async fn one_shot_server(
    status: u16,
    reason: &'static str,
    stall: bool,
) -> (
    std::net::SocketAddr,
    Arc<Mutex<Option<SocketRequest>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(None));
    let recorded = seen.clone();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let count = socket.read(&mut chunk).await.unwrap();
            if count == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..count]);
            if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = buffer[end + 4..].to_vec();
                while body.len() < length {
                    let count = socket.read(&mut chunk).await.unwrap();
                    body.extend_from_slice(&chunk[..count]);
                }
                *recorded.lock().unwrap() = Some(SocketRequest { head, body });
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            if stall { 64 } else { 0 }
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        if stall {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
    (address, seen, task)
}

#[tokio::test]
async fn the_collector_receives_a_decorated_post_with_application_json() {
    let (address, seen, task) = one_shot_server(204, "No Content", false).await;
    let exporter = HttpExporter::new(Arc::new(HyperTransport));
    exporter.store_opts(options(&format!("http://{address}/collect")));
    exporter.write(br#"{"message":"first","code":7}"#).unwrap();
    exporter.flush_and_purge(future_deadline()).await.unwrap();
    let seen = seen.lock().unwrap().take().expect("collector request");
    task.abort();
    assert!(seen.head.starts_with("POST /collect HTTP/1.1\r\n"));
    assert!(seen
        .head
        .to_ascii_lowercase()
        .contains("content-type: application/json"));
    let payload: serde_json::Value = serde_json::from_slice(&seen.body).unwrap();
    assert_eq!(payload["message"], "first");
    assert_eq!(payload["code"], 7);
    assert_eq!(payload["instanceID"], "sandbox-id");
    assert_eq!(payload["envID"], "template-id");
}

#[tokio::test]
async fn non_2xx_collector_status_is_a_failure() {
    let (address, _, task) = one_shot_server(503, "Service Unavailable", false).await;
    let exporter = HttpExporter::new(Arc::new(HyperTransport));
    exporter.store_opts(options(&format!("http://{address}/collect")));
    exporter.write(br#"{"message":"first"}"#).unwrap();
    let error = exporter
        .flush_and_purge(future_deadline())
        .await
        .unwrap_err();
    task.abort();
    assert!(
        error.to_string().contains("collector returned 503"),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("Service Unavailable"));
    assert_eq!(exporter.buffered(), (0, 0));
}

#[tokio::test]
async fn flush_returns_after_the_collector_headers_without_reading_the_body() {
    let (address, _, task) = one_shot_server(200, "OK", true).await;
    let exporter = HttpExporter::new(Arc::new(HyperTransport));
    exporter.store_opts(options(&format!("http://{address}/collect")));
    exporter.write(br#"{"message":"first"}"#).unwrap();
    let began = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(1500),
        exporter.flush_and_purge(future_deadline()),
    )
    .await
    .expect("flush waited for the response body");
    task.abort();
    assert!(result.is_ok());
    assert!(began.elapsed() < Duration::from_secs(1));
}
