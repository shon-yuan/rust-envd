// SPDX-License-Identifier: Apache-2.0
//! Metadata lifecycle adapted from E2B runtime
//! 51083a83cc0903185d992725ea6bb33683814b29 internal/host/mmds.go.
use super::{BoxFuture, MmdsClient, MmdsOpts};
use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};

/// Store internal IDs before attempting files. File failures are best effort.
pub trait MetadataSink: Send + Sync {
    fn store_ids<'a>(&'a self, opts: &'a MmdsOpts) -> BoxFuture<'a, ()>;
}

pub fn initialize_run_dir(root: &std::path::Path, is_firecracker: bool) {
    use std::{
        fs::{DirBuilder, OpenOptions},
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    if let Err(error) = DirBuilder::new().recursive(true).mode(0o755).create(root) {
        eprintln!("error creating E2B run directory: {error}");
    }
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o444)
            .open(root.join(".E2B_SANDBOX"))?;
        file.write_all(if is_firecracker { b"true" } else { b"false" })
    })();
    if let Err(error) = result {
        eprintln!("error writing sandbox file: {error}");
    }
}

pub struct MetadataFiles {
    root: PathBuf,
}
impl MetadataFiles {
    /// Directory creation belongs to guest startup, not polling.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub async fn write(&self, opts: &MmdsOpts) {
        let root = self.root.clone();
        let ids = [opts.instance_id.clone(), opts.env_id.clone()];
        if let Err(error) = tokio::task::spawn_blocking(move || {
            use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
            for (key, value) in ["E2B_SANDBOX_ID", "E2B_TEMPLATE_ID"].into_iter().zip(ids) {
                let result = (|| -> io::Result<()> {
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o666)
                        .open(root.join(format!(".{key}")))?;
                    file.write_all(value.as_bytes())
                })();
                if let Err(error) = result {
                    eprintln!("error writing {key} file: {error}");
                }
            }
        })
        .await
        {
            eprintln!("metadata file writer failed: {error}");
        }
    }
}

/// Mirrors Go poll termination reasons, mmds.go:147-194.
#[derive(Debug)]
pub enum PollOutcome {
    Delivered,
    SendCancelled,
    Stopped { last_error: Option<io::Error> },
    TimedOut { last_error: Option<io::Error> },
}

pub async fn poll(
    client: Arc<MmdsClient>,
    sink: Arc<dyn MetadataSink>,
    options: mpsc::Sender<MmdsOpts>,
    mut stop: watch::Receiver<bool>,
    deadline: Option<tokio::time::Instant>,
) -> PollOutcome {
    let period = Duration::from_millis(50);
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_error = None;
    async fn wait_stop(stop: &mut watch::Receiver<bool>) {
        loop {
            if *stop.borrow_and_update() {
                return;
            }
            if stop.changed().await.is_err() {
                // No sender can stop us any more; cancellation is unreachable.
                std::future::pending::<()>().await;
            }
        }
    }
    async fn wait_expired(deadline: Option<tokio::time::Instant>) {
        if let Some(deadline) = deadline {
            tokio::time::sleep_until(deadline).await;
        } else {
            std::future::pending::<()>().await;
        }
    }
    let outcome = loop {
        tokio::select! {
            biased;
            _ = wait_stop(&mut stop) => break PollOutcome::Stopped { last_error: last_error.take() },
            _ = wait_expired(deadline) => break PollOutcome::TimedOut { last_error: last_error.take() },
            _ = ticker.tick() => {
                // Each fetch races stop/deadline so a hung request cannot hide them.
                let opts = tokio::select! {
                    biased;
                    _ = wait_stop(&mut stop) => break PollOutcome::Stopped {last_error: last_error.take()},
                    _ = wait_expired(deadline) => break PollOutcome::TimedOut {last_error: last_error.take()},
                    result = client.fetch_opts() => match result {
                        Ok(opts) => opts,
                        // A failed attempt waits for the next tick, like Go.
                        Err(error) => {last_error = Some(error); continue;}
                    },
                };
                // Mirrors Go's synchronous store: fetch success must finish.
                sink.store_ids(&opts).await;
                if opts.address.is_empty() {
                    break PollOutcome::Delivered;
                }
                break tokio::select! {
                    biased;
                    _ = wait_stop(&mut stop) => PollOutcome::SendCancelled,
                    _ = wait_expired(deadline) => PollOutcome::SendCancelled,
                    result = options.send(opts) => {
                        if result.is_err() {
                            eprintln!("metadata options receiver closed before exporter integration");
                        }
                        PollOutcome::Delivered
                    }
                };
            }
        }
    };
    let suffix = |last_error: &Option<io::Error>| match last_error {
        Some(error) => format!(" (last error: {error})"),
        None => String::new(),
    };
    match &outcome {
        PollOutcome::Delivered => {}
        PollOutcome::SendCancelled => eprintln!("context cancelled while sending mmds opts"),
        PollOutcome::Stopped { last_error } => {
            eprintln!(
                "gave up polling for MMDS opts: canceled{}",
                suffix(last_error)
            );
        }
        PollOutcome::TimedOut { last_error } => {
            eprintln!(
                "gave up polling for MMDS opts: deadline exceeded{}",
                suffix(last_error)
            );
        }
    }
    outcome
}

pub struct MetadataLifecycle {
    client: Arc<MmdsClient>,
    sink: Arc<dyn MetadataSink>,
    options: mpsc::Sender<MmdsOpts>,
    // Explicitly retained for the future log exporter; no artificial draining.
    receiver: Mutex<Option<mpsc::Receiver<MmdsOpts>>>,
    stop: watch::Sender<bool>,
    tasks: Mutex<JoinSet<PollOutcome>>,
}
impl MetadataLifecycle {
    pub fn new(client: Arc<MmdsClient>, sink: Arc<dyn MetadataSink>) -> Self {
        let (options, receiver) = mpsc::channel(1);
        let (stop, _) = watch::channel(false);
        Self {
            client,
            sink,
            options,
            receiver: Mutex::new(Some(receiver)),
            stop,
            tasks: Mutex::new(JoinSet::new()),
        }
    }
    pub fn start_boot(&self) {
        self.start(None);
    }
    pub fn start_post_init(&self) {
        self.start(Some(Duration::from_secs(60)));
    }
    fn start(&self, budget: Option<Duration>) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                eprintln!("metadata poll task failed: {error}");
            }
        }
        if *self.stop.borrow() {
            return;
        }
        let client = self.client.clone();
        let sink = self.sink.clone();
        let options = self.options.clone();
        let stop = self.stop.subscribe();
        let deadline = budget.map(|budget| tokio::time::Instant::now() + budget);
        tasks.spawn(poll(client, sink, options, stop, deadline));
    }
    pub fn take_options_receiver(&self) -> Option<mpsc::Receiver<MmdsOpts>> {
        self.receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    pub async fn shutdown(&self) -> io::Result<()> {
        let mut tasks = {
            let mut guard = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.stop.send_replace(true);
            std::mem::replace(&mut *guard, JoinSet::new())
        };
        let mut first = None;
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                eprintln!("metadata poll task failed: {error}");
                if first.is_none() {
                    first = Some(io::Error::other(error));
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}
impl Drop for MetadataLifecycle {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
