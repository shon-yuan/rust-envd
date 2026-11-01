//! MMDS logs exporter, mirroring internal/logs/exporter/exporter.go at
//! 51083a83cc0903185d992725ea6bb33683814b29.
//!
//! One `write` is one log line (never split on newlines). Enqueueing is
//! synchronous and bounded; HTTP happens only on the background task started by
//! the first MMDS options, or in a caller-driven `flush_and_purge`.

use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

use super::{decorate, ratelimit::RateLimitedLogger, LogSink};
use crate::mmds::{HttpRequest, HttpTransport, MmdsOpts};

/// Go `ExporterTimeout`.
pub const EXPORTER_TIMEOUT: Duration = Duration::from_secs(10);
/// Under Loki's 256 KiB max_line_size default (Go `maxLogLineBytes`).
pub const MAX_LOG_LINE_BYTES: usize = 192 << 10;
/// Go `maxBufferedBytes`.
pub const MAX_BUFFERED_BYTES: usize = 8 << 20;

#[derive(Default)]
struct Queue {
    lines: VecDeque<Vec<u8>>,
    bytes: usize,
}

pub struct HttpExporter {
    transport: Arc<dyn HttpTransport>,
    queue: Mutex<Queue>,
    opts: Mutex<Option<MmdsOpts>>,
    triggers: mpsc::Sender<()>,
    trigger_receiver: Mutex<Option<mpsc::Receiver<()>>>,
    json_error: RateLimitedLogger,
    send_error: RateLimitedLogger,
    oversized: RateLimitedLogger,
}

impl HttpExporter {
    #[allow(clippy::new_ret_no_self)] // The exporter is shared by logger and flusher.
    pub fn new(transport: Arc<dyn HttpTransport>) -> Arc<Self> {
        let (triggers, trigger_receiver) = mpsc::channel(1);
        Arc::new(Self {
            transport,
            queue: Mutex::new(Queue::default()),
            opts: Mutex::new(None),
            triggers,
            trigger_receiver: Mutex::new(Some(trigger_receiver)),
            json_error: RateLimitedLogger::new(),
            send_error: RateLimitedLogger::new(),
            oversized: RateLimitedLogger::new(),
        })
    }

    /// Go `Write`: drop oversized lines with a rate-limited diagnostic, copy
    /// and enqueue synchronously, and report the original length.
    pub fn write(&self, line: &[u8]) -> io::Result<usize> {
        if line.len() > MAX_LOG_LINE_BYTES {
            self.oversized.log(format!(
                "dropped log line exceeding {MAX_LOG_LINE_BYTES} bytes"
            ));
            return Ok(line.len());
        }
        self.add_logs(line.to_vec());
        Ok(line.len())
    }

    /// Go `addLogs`: evict oldest lines to stay under the byte bound, enqueue,
    /// and leave at most one pending trigger notification.
    pub fn add_logs(&self, line: Vec<u8>) {
        {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            while queue.bytes + line.len() > MAX_BUFFERED_BYTES && !queue.lines.is_empty() {
                if let Some(oldest) = queue.lines.pop_front() {
                    queue.bytes -= oldest.len();
                }
            }
            queue.bytes += line.len();
            queue.lines.push_back(line);
        }
        let _ = self.triggers.try_send(());
    }

    /// Snapshot of `(line count, buffered bytes)` for flush-barrier tests.
    pub fn buffered(&self) -> (usize, usize) {
        let queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        (queue.lines.len(), queue.bytes)
    }

    /// Atomically take every queued line; the sender and flush share this, so a
    /// line is only ever owned by one of them.
    fn get_all_logs(&self) -> Vec<Vec<u8>> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.bytes = 0;
        queue.lines.drain(..).collect()
    }

    /// Discard everything still buffered, including lines that arrived after a
    /// flush snapshot was taken.
    fn purge_logs(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.lines.clear();
        queue.bytes = 0;
    }

    pub fn store_opts(&self, opts: MmdsOpts) {
        *self.opts.lock().unwrap_or_else(PoisonError::into_inner) = Some(opts);
    }

    fn opts_snapshot(&self) -> Option<MmdsOpts> {
        self.opts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Go `sendInstanceLogs`: POST application/json, 10 second ceiling, no
    /// keepalive (the transport opens a fresh connection and sends
    /// `Connection: close`), non-2xx is an error. An empty address is a no-op.
    /// The response body is never read.
    pub async fn send_instance_logs(
        &self,
        logs: &[u8],
        address: &str,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        if address.is_empty() {
            return Ok(());
        }
        let request = HttpRequest {
            method: "POST".to_owned(),
            url: address.to_owned(),
            headers: vec![("Content-Type".to_owned(), b"application/json".to_vec())],
            body: logs.to_vec(),
        };
        let client_deadline = Instant::now() + EXPORTER_TIMEOUT;
        let deadline = deadline.map_or(client_deadline, |deadline| deadline.min(client_deadline));
        let reply = tokio::time::timeout_at(deadline, self.transport.request_headers(request))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "collector request timed out")
            })??;
        if !(200..300).contains(&reply.status) {
            let reason = hyper::StatusCode::from_u16(reply.status)
                .ok()
                .and_then(|status| status.canonical_reason())
                .unwrap_or_default();
            return Err(io::Error::other(if reason.is_empty() {
                format!("collector returned {}", reply.status)
            } else {
                format!("collector returned {} {}", reply.status, reason)
            }));
        }
        Ok(())
    }

    /// Go `FlushAndPurge`: send the lines buffered when the call began, keep
    /// the first error, and purge everything still buffered on every exit path.
    pub async fn flush_and_purge(&self, deadline: Instant) -> io::Result<()> {
        let logs = self.get_all_logs();
        let result = self.flush(&logs, deadline).await;
        self.purge_logs();
        result
    }

    async fn flush(&self, logs: &[Vec<u8>], deadline: Instant) -> io::Result<()> {
        let Some(opts) = self.opts_snapshot() else {
            return Ok(());
        };
        let mut first_error: Option<io::Error> = None;
        for line in logs {
            if Instant::now() >= deadline {
                first_error.get_or_insert_with(deadline_exceeded);
                break;
            }
            let decorated = match decorate(line, &opts.instance_id, &opts.env_id) {
                Ok(decorated) => decorated,
                Err(error) => {
                    first_error.get_or_insert_with(|| {
                        io::Error::other(format!("add instance logging options to JSON: {error}"))
                    });
                    continue;
                }
            };
            if let Err(error) = self
                .send_instance_logs(&decorated, &opts.address, Some(deadline))
                .await
            {
                first_error.get_or_insert_with(|| {
                    io::Error::other(format!("send instance logs: {error}"))
                });
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Consume the retained MMDS options receiver. The HTTP send loop starts
    /// only on the first options; queued logs survive until then.
    pub fn start(self: &Arc<Self>, options: mpsc::Receiver<MmdsOpts>) -> io::Result<ExporterTasks> {
        let Some(triggers) = self
            .trigger_receiver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return Err(io::Error::other("log exporter already started"));
        };
        let (stop, stop_receiver) = watch::channel(false);
        let mut tasks = JoinSet::new();
        tasks.spawn(run(
            self.clone(),
            options,
            stop_receiver.clone(),
            stop_receiver,
            triggers,
        ));
        Ok(ExporterTasks {
            stop,
            tasks: Mutex::new(tasks),
        })
    }
}

impl LogSink for HttpExporter {
    fn write_line(&self, line: &[u8]) {
        let _ = self.write(line);
    }
}

/// Owns the exporter tasks; `shutdown` signals them and joins both.
pub struct ExporterTasks {
    stop: watch::Sender<bool>,
    tasks: Mutex<JoinSet<()>>,
}

impl ExporterTasks {
    pub async fn shutdown(&self) {
        self.stop.send_replace(true);
        let mut tasks = {
            let mut guard = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };
        while tasks.join_next().await.is_some() {}
    }
}

impl Drop for ExporterTasks {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

fn deadline_exceeded() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "context deadline exceeded")
}

/// Completes when shutdown is requested or the sender is gone.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

async fn run(
    exporter: Arc<HttpExporter>,
    mut options: mpsc::Receiver<MmdsOpts>,
    mut listen_stop: watch::Receiver<bool>,
    mut send_stop: watch::Receiver<bool>,
    mut triggers: mpsc::Receiver<()>,
) {
    let started = tokio::sync::Notify::new();
    let listener = async {
        loop {
            tokio::select! {
                biased;
                () = stopped(&mut listen_stop) => break,
                received = options.recv() => match received {
                    Some(opts) => {
                        // New options atomically replace the current ones; the
                        // send loop starts exactly once.
                        exporter.store_opts(opts);
                        started.notify_one();
                    }
                    // The producer keeps the channel open for the daemon's life
                    // (main.go:197-201); a close ends the listener.
                    None => break,
                },
            }
        }
    };
    let sender = async {
        tokio::select! {
            biased;
            () = stopped(&mut send_stop) => return,
            () = started.notified() => {}
        }
        loop {
            tokio::select! {
                biased;
                () = stopped(&mut send_stop) => return,
                received = triggers.recv() => {
                    if received.is_none() {
                        return;
                    }
                }
            }
            let logs = exporter.get_all_logs();
            if logs.is_empty() {
                continue;
            }
            // One options snapshot per batch: IDs/address never change midway.
            let Some(opts) = exporter.opts_snapshot() else {
                continue;
            };
            for line in logs {
                if *send_stop.borrow() {
                    return;
                }
                let decorated = match decorate(&line, &opts.instance_id, &opts.env_id) {
                    Ok(decorated) => decorated,
                    Err(error) => {
                        exporter.json_error.log(format!(
                            "error adding instance logging options to JSON: {error}"
                        ));
                        continue;
                    }
                };
                let send = exporter.send_instance_logs(&decorated, &opts.address, None);
                tokio::select! {
                    biased;
                    () = stopped(&mut send_stop) => return,
                    result = send => {
                        if let Err(error) = result {
                            exporter
                                .send_error
                                .log(format!("error sending instance logs: {error}"));
                        }
                    }
                }
            }
        }
    };
    tokio::join!(listener, sender);
}
