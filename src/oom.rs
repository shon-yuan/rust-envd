// SPDX-License-Identifier: Apache-2.0
//! Read-only OOM log watcher adapted from E2B runtime 51083a83 internal/host/oom.go.
//! The guest main runs it as a task and the daemon reports its snapshot on
//! `/metrics` (`daemon/metrics.rs`, matching internal/api/store.go GetMetrics).

use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::Mutex,
    time::Duration,
};
use tokio::{
    io::unix::AsyncFd,
    sync::oneshot,
    time::{sleep, Instant},
};

/// The shared wire type lives in the platform-neutral metrics module; this
/// path stays for the Linux watcher and its tests.
pub use crate::metrics::OomKill;

#[derive(Default)]
struct State {
    kills: Vec<OomKill>,
    caught_up: bool,
    last_unnamed: i64,
}

#[derive(Default)]
pub struct OomWatcher {
    state: Mutex<State>,
}

impl OomWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn kills(&self) -> Option<Vec<OomKill>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.caught_up.then(|| state.kills.clone())
    }

    #[cfg(test)]
    pub(crate) fn test_snapshot(records: &[&str], caught_up: bool) -> Self {
        let watcher = Self::new();
        for record in records {
            watcher.add(record.as_bytes());
        }
        if caught_up {
            watcher.mark_caught_up();
        }
        watcher
    }

    /// Follow /dev/kmsg read-only until cancelled or the sender is dropped.
    /// Only one run task should own a watcher. Cancellation clears its snapshot.
    pub async fn run(&self, stop: oneshot::Receiver<()>) {
        self.run_with(KernelSource, stop).await;
    }

    async fn run_with<S: LogSource>(&self, mut source: S, mut stop: oneshot::Receiver<()>) {
        let stopped = StopGuard(self);
        // Poll cancellation first, including when already cancelled before start.
        tokio::select! {
            biased;
            _ = &mut stop => {},
            _ = async {
                let mut recovering = false;
                loop {
                    let failure = self.watch_round(&mut source, recovering).await;
                    if !failure.quiet {
                        eprintln!("{}: {}; retry in 30 minutes", failure.kind.message(), failure.error);
                    }
                    recovering = true;
                    sleep(Duration::from_secs(30 * 60)).await;
                }
            } => {},
        }
        drop(stopped);
    }

    async fn watch_round<S: LogSource>(&self, source: &mut S, mut recovering: bool) -> Failure {
        let mut failures = 0;
        loop {
            let mut log = match open_with_retry(source).await {
                Ok(log) => log,
                Err(error) => {
                    return Failure {
                        kind: FailureKind::Open,
                        quiet: recovering,
                        error,
                    }
                }
            };
            let start = Instant::now();
            let error = self.follow(&mut log, &mut recovering).await;
            drop(log);
            self.mark_stopped();
            if start.elapsed() >= Duration::from_secs(60) {
                failures = 0;
                recovering = false;
            }
            failures += 1;
            let error = error
                .err()
                .unwrap_or_else(|| io::Error::other("kernel log ended"));
            if failures >= 5 {
                return Failure {
                    kind: FailureKind::Read,
                    quiet: recovering,
                    error,
                };
            }
            if !recovering {
                eprintln!("Failed to read the kernel log, reopening it: {error}");
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    async fn follow<L: KernelLog>(&self, log: &mut L, recovering: &mut bool) -> io::Result<()> {
        let mut buffer = [0_u8; 8192];
        loop {
            match log.read(&mut buffer) {
                Ok(size) => {
                    self.add(&buffer[..size]);
                }
                Err(error) if error.raw_os_error() == Some(nix::libc::EAGAIN) => {
                    if *recovering {
                        *recovering = false;
                        eprintln!("Reading the kernel log again, out-of-memory kills are reported");
                    }
                    self.mark_caught_up();
                    log.wait().await?;
                }
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(nix::libc::EPIPE | nix::libc::EINTR)
                    ) => {}
                Err(error) => return Err(error),
            }
            // A perpetually readable device must still permit cancellation.
            tokio::task::yield_now().await;
        }
    }

    fn add(&self, record: &[u8]) -> bool {
        if !record
            .windows(b"Killed process ".len())
            .any(|part| part == b"Killed process ")
        {
            return false;
        }
        let text = String::from_utf8_lossy(record);
        let Some((kill, unnamed)) = parse_kill(&text) else {
            return false;
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let warning = unnamed && kill.seq > state.last_unnamed;
        if warning {
            state.last_unnamed = kill.seq;
            eprintln!("Failed to find the process name of an out-of-memory kill: {text}");
        }
        state.kills.push(kill);
        if state.kills.len() > 32 {
            state.kills.remove(0);
        }
        warning
    }

    fn mark_caught_up(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .caught_up = true;
    }

    fn mark_stopped(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.caught_up = false;
        state.kills.clear();
    }
}

struct StopGuard<'a>(&'a OomWatcher);
impl Drop for StopGuard<'_> {
    fn drop(&mut self) {
        self.0.mark_stopped();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FailureKind {
    Open,
    Read,
}
impl FailureKind {
    fn message(&self) -> &'static str {
        match self {
            Self::Open => "Failed to open the kernel log, out-of-memory kills will not be reported",
            Self::Read => {
                "Stopped reading the kernel log, out-of-memory kills will not be reported"
            }
        }
    }
}
struct Failure {
    kind: FailureKind,
    quiet: bool,
    error: io::Error,
}

trait KernelLog {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize>;
    async fn wait(&mut self) -> io::Result<()>;
}
trait LogSource {
    type Log: KernelLog;
    fn open(&mut self) -> io::Result<Self::Log>;
}

async fn open_with_retry<S: LogSource>(source: &mut S) -> io::Result<S::Log> {
    let mut last = io::Error::other("kernel log open failed");
    for attempt in 0..5 {
        if attempt > 0 {
            sleep(Duration::from_secs(1)).await;
        }
        match source.open() {
            Ok(log) => return Ok(log),
            Err(error) => last = error,
        }
    }
    Err(last)
}

struct KernelSource;
struct Kmsg {
    fd: AsyncFd<OwnedFd>,
}
impl LogSource for KernelSource {
    type Log = Kmsg;
    fn open(&mut self) -> io::Result<Kmsg> {
        // SAFETY: constant NUL-terminated pathname; flags request read-only access.
        let fd = unsafe {
            nix::libc::open(
                c"/dev/kmsg".as_ptr(),
                nix::libc::O_RDONLY | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: open returned a fresh owned descriptor, transferred exactly once.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: Kmsg keeps exclusive ownership of the opened descriptor;
        // retry/cancellation drops the registration before closing its owner.
        let fd = unsafe { AsyncFd::register(owned)? };
        Ok(Kmsg { fd })
    }
}
impl KernelLog for Kmsg {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // SAFETY: buffer is writable for its length and descriptor remains owned.
        let size = unsafe {
            nix::libc::read(
                self.fd.get_ref().as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if size < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(size as usize)
        }
    }

    async fn wait(&mut self) -> io::Result<()> {
        // Old poll returns whenever events arrive and rechecks cancellation every
        // second. AsyncFd readiness plus timeout preserves that upper bound.
        loop {
            match tokio::time::timeout(Duration::from_secs(1), self.fd.readable()).await {
                Ok(Ok(mut ready)) => {
                    ready.clear_ready();
                    return Ok(());
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => {}
            }
        }
    }
}

fn parse_kill(record: &str) -> Option<(OomKill, bool)> {
    let (prefix, message) = record.split_once(';')?;
    let mut fields = prefix.splitn(3, ',');
    let priority = fields.next()?.parse::<isize>().ok()?;
    if !(0..=7).contains(&priority) {
        return None;
    }
    let seq = fields.next()?.parse::<i64>().ok()?;
    fields.next()?;
    let message = message.split('\n').next()?;
    let victim = [
        "Out of memory: Killed process ",
        "Out of memory (oom_kill_allocating_task): Killed process ",
        "Memory cgroup out of memory: Killed process ",
    ]
    .iter()
    .find_map(|prefix| message.strip_prefix(prefix))?;
    let name = victim.split_once(" (").and_then(|(pid, name)| {
        pid.parse::<isize>().ok()?;
        name.rfind(") total-vm:").map(|end| &name[..end])
    });
    Some((
        OomKill {
            seq,
            process: name.unwrap_or("Unknown").to_owned(),
        },
        name.is_none(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: i64, process: &str) -> String {
        format!("3,{seq},0,-;Out of memory: Killed process 1 ({process}) total-vm:1kB")
    }

    #[test]
    fn parser_matches_three_prefixes_and_greedy_process_delimiter() {
        for prefix in [
            "Out of memory: ",
            "Memory cgroup out of memory: ",
            "Out of memory (oom_kill_allocating_task): ",
        ] {
            let text = format!("3,843,ignored;{prefix}Killed process +475 (a) total-vm:1) total-vm:22kB\n SUBSYSTEM=memory");
            assert_eq!(
                parse_kill(&text),
                Some((
                    OomKill {
                        seq: 843,
                        process: "a) total-vm:1".into()
                    },
                    false
                ))
            );
        }
    }

    #[test]
    fn parser_rejects_non_kernel_spoofs_and_unanchored_messages() {
        for text in [
            "12,1,0,-;Out of memory: Killed process 1 (spoof) total-vm:1",
            "8,1,0,-;Out of memory: Killed process 1 (spoof) total-vm:1",
            "-1,1,0,-;Out of memory: Killed process 1 (spoof) total-vm:1",
            " 3,1,0,-;Out of memory: Killed process 1 (spoof) total-vm:1",
            "3, 1,0,-;Out of memory: Killed process 1 (spoof) total-vm:1",
            "3,9223372036854775808,0;Out of memory: Killed process 1 (spoof) total-vm:1",
            "3,1;Out of memory: Killed process 1 (spoof) total-vm:1",
            "3,1,0;EXT4-fs: x Out of memory: Killed process 1 (spoof) total-vm:1",
            "3,1,0;oom_reaper: reaped process 1 (x)",
            "3,1,0;Killed process 1 (x) total-vm:1",
            "3,1,0;other\nOut of memory: Killed process 1 (spoof) total-vm:1",
        ] {
            assert!(parse_kill(text).is_none(), "accepted {text:?}");
        }
    }

    #[test]
    fn parser_preserves_go_signed_integer_and_empty_name_semantics() {
        for (priority, seq, pid, name) in [
            ("+3", "+2", "-1", ""),
            ("0", "-9223372036854775808", "9223372036854775807", "worker"),
            ("07", "0002", "001", "a (b)"),
        ] {
            let text =
                format!("{priority},{seq},;Out of memory: Killed process {pid} ({name}) total-vm:");
            let (kill, unnamed) = parse_kill(&text).unwrap();
            assert_eq!(kill.seq, seq.parse::<i64>().unwrap());
            assert_eq!(kill.process, name);
            assert!(!unnamed);
        }
    }

    #[test]
    fn invalid_victim_is_still_a_kill_with_unknown_name() {
        for victim in [
            "3001 [garbled]",
            " 1 (x) total-vm:1",
            "1 (x)",
            "9223372036854775808 (x) total-vm:1",
            "--1 (x) total-vm:1",
            "1 (x\n) total-vm:1",
        ] {
            let text = format!("3,11,0;Out of memory: Killed process {victim}");
            assert_eq!(
                parse_kill(&text),
                Some((
                    OomKill {
                        seq: 11,
                        process: "Unknown".into()
                    },
                    true
                ))
            );
        }
    }

    #[test]
    fn snapshots_are_nonconsuming_copies_latest32_oldest_first() {
        assert!(OomWatcher::test_snapshot(&[], false).kills().is_none());
        assert_eq!(OomWatcher::test_snapshot(&[], true).kills(), Some(vec![]));
        let watcher = OomWatcher::test_snapshot(&[&record(0, "p")], false);
        for seq in 1..40 {
            watcher.add(record(seq, "p").as_bytes());
        }
        assert!(watcher.kills().is_none());
        watcher.mark_caught_up();
        let kills = watcher.kills().unwrap();
        assert_eq!(kills.len(), 32);
        assert_eq!(kills.first().unwrap().seq, 8);
        assert_eq!(kills.last().unwrap().seq, 39);
        let mut changed = kills.clone();
        changed[0].process = "changed".into();
        assert_eq!(watcher.kills().unwrap(), kills);
        watcher.mark_stopped();
        assert!(watcher.kills().is_none());
        watcher.add(record(2, "rebuilt").as_bytes());
        watcher.mark_caught_up();
        assert_eq!(
            watcher.kills().unwrap(),
            vec![OomKill {
                seq: 2,
                process: "rebuilt".into()
            }]
        );
    }

    enum Action {
        Record(String),
        Error(i32),
        Drain,
        EndAfter(std::time::Duration),
    }

    struct FakeLog {
        actions: std::collections::VecDeque<Action>,
        watcher: std::sync::Arc<OomWatcher>,
        closed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Drop for FakeLog {
        fn drop(&mut self) {
            self.closed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl KernelLog for FakeLog {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self
                .actions
                .pop_front()
                .unwrap_or(Action::Error(nix::libc::EIO))
            {
                Action::Record(record) => {
                    assert_eq!(buf.len(), 8192);
                    buf[..record.len()].copy_from_slice(record.as_bytes());
                    Ok(record.len())
                }
                Action::Error(errno) => Err(std::io::Error::from_raw_os_error(errno)),
                Action::Drain => {
                    assert!(self.watcher.kills().is_none());
                    Err(std::io::Error::from_raw_os_error(nix::libc::EAGAIN))
                }
                action @ Action::EndAfter(_) => {
                    self.actions.push_front(action);
                    Err(std::io::Error::from_raw_os_error(nix::libc::EAGAIN))
                }
            }
        }

        async fn wait(&mut self) -> std::io::Result<()> {
            assert!(self.watcher.kills().is_some());
            if let Some(Action::EndAfter(duration)) = self.actions.front() {
                let duration = *duration;
                self.actions.pop_front();
                tokio::time::sleep(duration).await;
                return Err(std::io::Error::from_raw_os_error(nix::libc::EIO));
            }
            Ok(())
        }
    }

    struct FakeSource {
        opens: std::collections::VecDeque<std::io::Result<FakeLog>>,
        attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl LogSource for FakeSource {
        type Log = FakeLog;
        fn open(&mut self) -> std::io::Result<Self::Log> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(!self.opens.is_empty(), "unexpected extra open");
            self.opens.pop_front().unwrap()
        }
    }

    fn fake_log(
        watcher: &std::sync::Arc<OomWatcher>,
        actions: Vec<Action>,
        closed: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> FakeLog {
        FakeLog {
            actions: actions.into(),
            watcher: watcher.clone(),
            closed: closed.clone(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn follow_drains_before_publishing_and_continues_overwritten_interrupted_records() {
        let watcher = std::sync::Arc::new(OomWatcher::new());
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut log = fake_log(
            &watcher,
            vec![
                Action::Record(record(2, "p")),
                Action::Error(nix::libc::EPIPE),
                Action::Error(nix::libc::EINTR),
                Action::Record(record(9, "new")),
                Action::Drain,
                Action::Error(nix::libc::EIO),
            ],
            &closed,
        );
        let mut recovering = true;
        assert_eq!(
            watcher
                .follow(&mut log, &mut recovering)
                .await
                .unwrap_err()
                .raw_os_error(),
            Some(nix::libc::EIO)
        );
        assert!(!recovering);
        assert_eq!(
            watcher
                .kills()
                .unwrap()
                .iter()
                .map(|kill| kill.seq)
                .collect::<Vec<_>>(),
            vec![2, 9]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn open_failure_retries_five_times_at_one_second_intervals() {
        let watcher = OomWatcher::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut source = FakeSource {
            opens: (0..5)
                .map(|_| Err(std::io::Error::from_raw_os_error(nix::libc::EACCES)))
                .collect(),
            attempts: attempts.clone(),
        };
        let start = tokio::time::Instant::now();
        let failure = watcher.watch_round(&mut source, false).await;
        assert!(!failure.quiet);
        assert_eq!(failure.kind, FailureKind::Open);
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(4));
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 5);
        assert!(watcher.kills().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn read_errors_close_and_rebuild_then_give_up_after_five() {
        let watcher = std::sync::Arc::new(OomWatcher::new());
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opens = (0..5)
            .map(|seq| {
                Ok(fake_log(
                    &watcher,
                    vec![
                        Action::Record(record(seq, "rebuilt")),
                        Action::Drain,
                        Action::Error(nix::libc::EIO),
                    ],
                    &closed,
                ))
            })
            .collect();
        let mut source = FakeSource {
            opens,
            attempts: attempts.clone(),
        };
        let start = tokio::time::Instant::now();
        let failure = watcher.watch_round(&mut source, true).await;
        assert_eq!(failure.kind, FailureKind::Read);
        assert!(!failure.quiet, "catching up ends quiet recovery");
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(4));
        assert_eq!(closed.load(std::sync::atomic::Ordering::SeqCst), 5);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 5);
        assert!(watcher.kills().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_sixty_second_read_resets_consecutive_failure_count() {
        let watcher = std::sync::Arc::new(OomWatcher::new());
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opens = (0..9)
            .map(|index| {
                let actions = if index == 4 {
                    vec![Action::EndAfter(std::time::Duration::from_secs(60))]
                } else {
                    vec![Action::Error(nix::libc::EIO)]
                };
                Ok(fake_log(&watcher, actions, &closed))
            })
            .collect();
        let mut source = FakeSource {
            opens,
            attempts: attempts.clone(),
        };
        let failure = watcher.watch_round(&mut source, true).await;
        assert_eq!(failure.kind, FailureKind::Read);
        assert!(!failure.quiet);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 9);
        assert_eq!(closed.load(std::sync::atomic::Ordering::SeqCst), 9);
    }

    #[tokio::test(start_paused = true)]
    async fn gave_up_watcher_recovers_after_thirty_minutes_and_cancels_during_retry() {
        let watcher = OomWatcher::new();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = FakeSource {
            opens: (0..10)
                .map(|_| Err(std::io::Error::from_raw_os_error(nix::libc::EACCES)))
                .collect(),
            attempts: attempts.clone(),
        };
        let (cancel, stop) = oneshot::channel();
        let run = watcher.run_with(source, stop);
        tokio::pin!(run);
        for second in 0..=4 {
            tokio::select! { biased; _ = &mut run => panic!("stopped early"), _ = tokio::task::yield_now() => {} }
            assert_eq!(
                attempts.load(std::sync::atomic::Ordering::SeqCst),
                second + 1
            );
            if second < 4 {
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
            }
        }
        tokio::time::advance(std::time::Duration::from_secs(1799)).await;
        tokio::select! { biased; _ = &mut run => panic!("stopped early"), _ = tokio::task::yield_now() => {} }
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 5);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::select! { biased; _ = &mut run => panic!("stopped early"), _ = tokio::task::yield_now() => {} }
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 6);
        cancel.send(()).unwrap();
        run.await;
        assert!(watcher.kills().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_during_wait_closes_log_and_clears_published_snapshot() {
        let watcher = std::sync::Arc::new(OomWatcher::new());
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log = fake_log(
            &watcher,
            vec![
                Action::Record(record(1, "p")),
                Action::EndAfter(Duration::from_secs(3600)),
            ],
            &closed,
        );
        let source = FakeSource {
            opens: [Ok(log)].into(),
            attempts,
        };
        let (cancel, stop) = oneshot::channel();
        let run = watcher.run_with(source, stop);
        tokio::pin!(run);
        for _ in 0..3 {
            tokio::select! { biased; _ = &mut run => panic!("stopped early"), _ = tokio::task::yield_now() => {} }
        }
        assert_eq!(watcher.kills().unwrap()[0].process, "p");
        let start = Instant::now();
        cancel.send(()).unwrap();
        run.await;
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(closed.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(watcher.kills().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn descriptor_readiness_survives_poll_timeout_and_reads_a_complete_record() {
        let (reader, writer) = std::os::unix::net::UnixDatagram::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let owned: OwnedFd = reader.into();
        // SAFETY: the reader was consumed into OwnedFd; the writer owns a
        // separate peer. No code closes or replaces the registered reader.
        let mut log = Kmsg {
            fd: unsafe { AsyncFd::register(owned).unwrap() },
        };
        let mut buffer = [0; 8192];
        assert_eq!(
            log.read(&mut buffer).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        {
            let wait = log.wait();
            tokio::pin!(wait);
            tokio::select! { biased; _ = &mut wait => panic!("unexpected readiness"), _ = tokio::task::yield_now() => {} }
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::select! { biased; _ = &mut wait => panic!("timeout must keep waiting"), _ = tokio::task::yield_now() => {} }
            writer.send(record(1, "socket").as_bytes()).unwrap();
            wait.await.unwrap();
        }
        let size = log.read(&mut buffer).unwrap();
        assert_eq!(
            parse_kill(std::str::from_utf8(&buffer[..size]).unwrap())
                .unwrap()
                .0
                .process,
            "socket"
        );
    }

    #[test]
    fn unnamed_warning_only_advances_for_increasing_positive_sequence() {
        let watcher = OomWatcher::new();
        for (seq, warn) in [
            (0, false),
            (-1, false),
            (2, true),
            (2, false),
            (1, false),
            (3, true),
        ] {
            let text = format!("3,{seq},0;Out of memory: Killed process garbled");
            assert_eq!(watcher.add(text.as_bytes()), warn);
        }
        watcher.mark_stopped();
        assert!(!watcher.add(b"3,2,0;Out of memory: Killed process garbled"));
        assert!(watcher.add(b"3,4,0;Out of memory: Killed process garbled"));
    }
}
