//! Oracle scan.go: the periodic TCP-connection scan broadcast to subscribers.
//!
//! `scan_and_broadcast` scans first, then waits a period (scan.go:45-62); a
//! failed scan is logged and NOT broadcast — an empty result would make the
//! forwarder kill every socat (scan.go:40, scanner_linux_test.go:16-22).
//! `destroy` stops the loop and unparks any Signal parked mid-broadcast, so
//! the loop cannot leak on a subscriber that stopped consuming
//! (scanner_test.go:71-99).

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use super::{
    scan_subscriber::{ScanBatch, ScannerFilter, ScannerSubscriber},
    ScanSource,
};
use crate::logs::Logger;

/// Oracle scan.go:12-17. The subscribers live in a mutex-protected vector —
/// Go keeps them in a concurrent map (`smap`); only Insert/Items/Count are
/// used and nothing removes, so a Vec behind a mutex is the same contract.
pub struct Scanner {
    logger: Logger,
    period: Duration,
    subs: Mutex<Vec<Arc<ScannerSubscriber>>>,
    source: Arc<dyn ScanSource>,
    /// Destroyed flag; `changed()` on a fresh receiver observes the current
    /// value, so a close before the loop's first await still stops it.
    scan_exit: tokio::sync::watch::Receiver<bool>,
    scan_exit_sender: tokio::sync::watch::Sender<bool>,
}

impl Scanner {
    /// Oracle NewScanner (scan.go:23-30) with the platform's production
    /// source: `/proc/net/tcp{,6}` on Linux, the Win32 TCP table on Windows.
    pub fn new(logger: &Logger, period: Duration) -> Self {
        Self::with_source(logger, period, crate::platform::port_scan_source())
    }

    /// Test seam: an injected scan source, standing in for gopsutil's
    /// HOST_PROC redirection the oracle tests use.
    pub fn with_source(logger: &Logger, period: Duration, source: Arc<dyn ScanSource>) -> Self {
        let (scan_exit_sender, scan_exit) = tokio::sync::watch::channel(false);
        Self {
            logger: logger.clone(),
            period,
            subs: Mutex::new(Vec::new()),
            source,
            scan_exit,
            scan_exit_sender,
        }
    }

    /// Oracle AddSubscriber (scan.go:32-37).
    pub fn add_subscriber(
        &self,
        id: impl Into<String>,
        filter: Option<ScannerFilter>,
    ) -> Arc<ScannerSubscriber> {
        let subscriber = ScannerSubscriber::new(id, filter);
        self.subs.lock().unwrap().push(subscriber.clone());
        subscriber
    }

    /// Test support: Go's `smap.Count` in TestNewForwarder_SubscribesOnlyWhenForwarding.
    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().unwrap().len()
    }

    /// Oracle Destroy (scan.go:19-21): stop the loop and abort every parked
    /// Signal. Idempotent; Go's `close` would panic on a second call, which
    /// the single deferred caller never does — the Rust close is simply
    /// safe to repeat.
    pub fn destroy(&self) {
        let _ = self.scan_exit_sender.send(true);
        for subscriber in self.subs.lock().unwrap().iter() {
            subscriber.close();
        }
    }

    /// Oracle ScanAndBroadcast (scan.go:41-63): scan, broadcast, wait for the
    /// tick — the loop scans immediately on entry because the ticker is
    /// awaited after the first broadcast. Exits on destroy.
    pub async fn scan_and_broadcast(&self) {
        // Go time.NewTicker fires the first tick a full period in and the loop
        // scans before the first wait; tokio's interval fires immediately, so
        // the first (immediate) tick is consumed before the first scan to
        // keep the same phase: scan now, then every period.
        let mut ticker = tokio::time::interval(self.period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        let mut scan_exit = self.scan_exit.clone();
        loop {
            // tcp monitors both ipv4 and ipv6 connections (scan.go:46-47).
            match self.source.connections() {
                Ok(processes) => {
                    let batch: ScanBatch = Arc::new(processes);
                    // Snapshot the subscriber list: the guard must not ride
                    // across the awaited Signal (the future has to stay Send).
                    let subs = self.subs.lock().unwrap().clone();
                    for sub in subs.iter() {
                        // Pass the exit so a subscriber that stopped consuming
                        // can't park this loop (scan.go:51-55).
                        sub.signal(batch.clone()).await;
                    }
                }
                Err(error) => {
                    self.logger
                        .warn()
                        .err(&error)
                        .msg("Failed to scan open TCP ports, keeping the last known ports");
                }
            }
            tokio::select! {
                _ = scan_exit.changed() => {
                    if *scan_exit.borrow() {
                        return;
                    }
                }
                _ = ticker.tick() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::port::{Addr, ConnectionStat};

    /// `AF_INET` is 2 on both supported platforms (`AF_INET6` differs and
    /// these scan-loop tests only build IPv4 listeners).
    const AF_INET: u32 = 2;
    use std::{
        io,
        sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
        time::Duration,
    };

    fn listen(port: u32, pid: i32) -> ConnectionStat {
        ConnectionStat {
            fd: 0,
            family: AF_INET,
            sock_type: 1,
            laddr: Addr {
                ip: "127.0.0.1".into(),
                port,
            },
            raddr: Addr {
                ip: "0.0.0.0".into(),
                port: 0,
            },
            status: "LISTEN".into(),
            pid,
        }
    }

    /// A scripted source: every call runs the next closure; an empty script
    /// (or exhausted script) errors, pinning scan-failure behaviour.
    struct ScriptedSource {
        scans: Mutex<Vec<Result<Vec<ConnectionStat>, io::Error>>>,
        calls: AtomicUsize,
    }

    impl ScriptedSource {
        fn once(result: Result<Vec<ConnectionStat>, io::Error>) -> Arc<Self> {
            Arc::new(Self {
                scans: Mutex::new(vec![result]),
                calls: AtomicUsize::new(0),
            })
        }

        fn always_failing() -> Arc<Self> {
            Self::once(Err(io::Error::other("no net/tcp")))
        }
    }

    impl ScanSource for ScriptedSource {
        fn connections(&self) -> io::Result<Vec<ConnectionStat>> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.scans
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| Err(io::Error::other("no net/tcp")))
        }
    }

    /// Oracle scanner_linux_test.go:24-67: a failed scan is not broadcast;
    /// the next successful scan is.
    #[tokio::test]
    async fn a_failed_scan_is_not_broadcast_but_the_next_success_is() {
        let logger = Logger::none();
        let source = ScriptedSource::always_failing();
        let scanner = Arc::new(Scanner::with_source(
            &logger,
            Duration::from_millis(1),
            source,
        ));
        let sub = scanner.add_subscriber("test", None);

        let scan_task = tokio::spawn({
            let scanner = Arc::clone(&scanner);
            async move { scanner.scan_and_broadcast().await }
        });

        // The first (and every) scan fails: nothing may arrive.
        let nothing = tokio::time::timeout(Duration::from_millis(100), sub.recv()).await;
        assert!(
            matches!(nothing, Err(_) | Ok(None)),
            "a failed scan must not be broadcast"
        );
        scanner.destroy();
        let _ = tokio::time::timeout(Duration::from_secs(1), scan_task).await;
    }

    #[tokio::test]
    async fn a_successful_scan_reaches_the_subscriber_and_destroy_stops_the_loop() {
        let logger = Logger::none();
        let source = ScriptedSource::once(Ok(vec![listen(8080, 4242)]));
        let scanner = Arc::new(Scanner::with_source(
            &logger,
            Duration::from_millis(1),
            source,
        ));
        let sub = scanner.add_subscriber("test", None);

        let scan_task = tokio::spawn({
            let scanner = Arc::clone(&scanner);
            async move { scanner.scan_and_broadcast().await }
        });
        let got = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .expect("the successful scan must be broadcast")
            .expect("the channel must be open");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].laddr.port, 8080);

        scanner.destroy();
        tokio::time::timeout(Duration::from_secs(2), scan_task)
            .await
            .expect("scan_and_broadcast must exit after destroy — task leaked")
            .unwrap();
    }

    /// Oracle scanner_test.go:76-99: a stalled subscriber must not leak the
    /// scan loop — destroy aborts the parked broadcast send.
    #[tokio::test]
    async fn destroy_exits_even_with_a_stalled_subscriber() {
        let logger = Logger::none();
        let first = ScriptedSource::once(Ok(vec![listen(8080, 4242)]));
        let scanner = Arc::new(Scanner::with_source(
            &logger,
            Duration::from_millis(10),
            first,
        ));
        let stalled = scanner.add_subscriber("stalled-sub", None);
        // Nobody ever receives from `stalled`.

        let scan_task = tokio::spawn({
            let scanner = Arc::clone(&scanner);
            async move { scanner.scan_and_broadcast().await }
        });
        // Let the loop reach its first signal and park there.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(stalled.has_parked_item(), "the broadcast must be parked");

        scanner.destroy();

        tokio::time::timeout(Duration::from_secs(2), scan_task)
            .await
            .expect("ScanAndBroadcast did not exit after destroy — task leaked")
            .unwrap();
    }

    /// The scan phase: first scan immediately, then one per period.
    #[tokio::test(flavor = "current_thread")]
    async fn the_loop_scans_immediately_then_once_per_period() {
        // Real proc source; the port numbers are irrelevant, only the call
        // rhythm is pinned.
        let logger = Logger::none();
        let source = ScriptedSource {
            scans: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        };
        let source = Arc::new(source);
        let scanner = Arc::new(Scanner::with_source(
            &logger,
            Duration::from_millis(100),
            source.clone(),
        ));
        // A subscriber that drains immediately so the loop keeps ticking.
        let sub = scanner.add_subscriber("drain", None);
        let drain_task = tokio::spawn(async move { while sub.recv().await.is_some() {} });

        let scan_task = tokio::spawn({
            let scanner = Arc::clone(&scanner);
            async move { scanner.scan_and_broadcast().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let after_first = source.calls.load(AtomicOrdering::SeqCst);
        assert_eq!(
            after_first, 1,
            "the first scan happens immediately, before any period elapses"
        );
        tokio::time::sleep(Duration::from_millis(180)).await;
        let after_period = source.calls.load(AtomicOrdering::SeqCst);
        // Scans at t=0 (immediate), t≈100ms and t≈200ms: three by 230ms.
        assert_eq!(
            after_period, 3,
            "one scan per period after the immediate first scan"
        );
        scanner.destroy();
        let _ = tokio::time::timeout(Duration::from_secs(1), scan_task).await;
        drain_task.abort();
    }
}
