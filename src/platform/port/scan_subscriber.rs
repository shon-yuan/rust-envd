//! Oracle scan_subscriber.go + scanfilter.go.
//!
//! The subscriber replaces Go's unbuffered `chan []net.ConnectionStat` plus
//! the `exit` escape (Signal's `select { case ch <- msg: case <-exit: }`) with
//! a rendezvous slot: a send parks until a receiver TAKES the item or the
//! channel is closed, so a subscriber that stopped consuming can never park
//! the scan loop forever (scan_subscriber.go:40-45), and a merely slow
//! receiver never loses a delivery (scanner_test.go:44-69).

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::Notify;

use super::ConnectionStat;

/// Oracle scanfilter.go:9-27. A filter with neither state nor IPs matches
/// nothing (scanfilter.go:15-18); otherwise the connection's local IP must be
/// one of `ips` AND the state must equal `state` exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannerFilter {
    pub state: String,
    pub ips: Vec<String>,
}

impl ScannerFilter {
    pub fn matches(&self, connection: &ConnectionStat) -> bool {
        if self.state.is_empty() && self.ips.is_empty() {
            return false;
        }
        self.ips.contains(&connection.laddr.ip) && self.state == connection.status
    }
}

/// One scan broadcast; `Arc` keeps the unfiltered path shared exactly like Go
/// sends the same backing slice to every subscriber (scan_subscriber.go:26-39
/// only copies when a filter matches).
pub type ScanBatch = Arc<Vec<ConnectionStat>>;

/// The rendezvous channel. Invariant: at most one item in flight; `send`
/// returns only after the item was taken or the channel closed.
struct Rendezvous {
    slot: Mutex<Option<ScanBatch>>,
    closed: AtomicBool,
    /// Signalled (permit stored) when the slot is filled or the channel closed.
    filled: Notify,
    /// Signalled (permit stored) when the slot is drained or the channel closed.
    drained: Notify,
}

impl Rendezvous {
    fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            closed: AtomicBool::new(false),
            filled: Notify::new(),
            drained: Notify::new(),
        }
    }

    /// Parks until the item is handed to a receiver or the channel is closed.
    /// Closing wins over delivery only where Go's `select` could also pick the
    /// exit arm: the caller is gone and the batch is dropped.
    async fn send(&self, item: ScanBatch) {
        // Phase 1: wait for an empty slot, deposit the item.
        let mut item = Some(item);
        loop {
            let deposited = {
                let mut slot = self.slot.lock().unwrap();
                if self.closed.load(Ordering::Acquire) {
                    return;
                }
                let empty = slot.is_none();
                if empty {
                    *slot = item.take();
                }
                empty
            };
            if deposited {
                break;
            }
            self.filled.notified().await;
        }
        self.filled.notify_one();
        // Phase 2: wait until the item is taken or the channel closed — the
        // Go unbuffered-send park (scan_subscriber.go:40-45).
        loop {
            {
                let slot = self.slot.lock().unwrap();
                if self.closed.load(Ordering::Acquire) {
                    return;
                }
                if slot.is_none() {
                    return;
                }
            }
            self.drained.notified().await;
        }
    }

    /// Takes the parked item, or `None` once the channel is closed and empty.
    async fn recv(&self) -> Option<ScanBatch> {
        loop {
            {
                let mut slot = self.slot.lock().unwrap();
                if let Some(item) = slot.take() {
                    drop(slot);
                    self.drained.notify_one();
                    return Some(item);
                }
                if self.closed.load(Ordering::Acquire) {
                    return None;
                }
            }
            self.filled.notified().await;
        }
    }

    /// Unparks every sender and receiver; later sends return immediately, as
    /// they do in Go once `exit` is closed.
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.filled.notify_one();
        self.drained.notify_one();
        self.filled.notify_waiters();
        self.drained.notify_waiters();
    }
}

/// Oracle scan_subscriber.go:10-22.
pub struct ScannerSubscriber {
    id: String,
    filter: Option<ScannerFilter>,
    messages: Arc<Rendezvous>,
}

impl ScannerSubscriber {
    pub(crate) fn new(id: impl Into<String>, filter: Option<ScannerFilter>) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            filter,
            messages: Arc::new(Rendezvous::new()),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Oracle Signal (scan_subscriber.go:26-46): filter, then park on the
    /// handoff; a closed channel drops the batch instead of parking forever.
    pub async fn signal(&self, procs: ScanBatch) {
        let message = match &self.filter {
            None => procs,
            Some(filter) => {
                let filtered: Vec<ConnectionStat> = procs
                    .iter()
                    .filter(|connection| filter.matches(connection))
                    .cloned()
                    .collect();
                Arc::new(filtered)
            }
        };
        self.messages.send(message).await;
    }

    /// Go `<-ss.Messages` with the two-value receive (forward.go:138-141):
    /// `None` when the scanner stopped and closed the channel.
    pub async fn recv(&self) -> Option<ScanBatch> {
        self.messages.recv().await
    }

    /// The scanner's exit escape for a parked Signal.
    pub(crate) fn close(&self) {
        self.messages.close();
    }

    /// Test seam: whether a value is parked in the slot right now.
    #[doc(hidden)]
    pub fn has_parked_item(&self) -> bool {
        self.messages.slot.lock().unwrap().is_some()
    }

    /// Test seam: bounded wait used to observe "still parked".
    #[doc(hidden)]
    pub async fn park_tick() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Addr, ConnectionStat};
    use super::*;

    /// `AF_INET` is 2 on both supported platforms; only the shared filter's
    /// string matching is under test here.
    const AF_INET: u32 = 2;

    fn connection(ip: &str, port: u32, status: &str) -> ConnectionStat {
        ConnectionStat {
            fd: 0,
            family: AF_INET,
            sock_type: 1,
            laddr: Addr {
                ip: ip.to_owned(),
                port,
            },
            raddr: Addr {
                ip: "0.0.0.0".into(),
                port: 0,
            },
            status: status.to_owned(),
            pid: 42,
        }
    }

    /// Oracle scanfilter behaviour, pinned from scanfilter.go:14-27.
    #[test]
    fn filter_matches_ip_and_state_exactly_and_nothing_when_empty() {
        let filter = ScannerFilter {
            state: "LISTEN".into(),
            ips: vec!["127.0.0.1".into(), "localhost".into(), "::1".into()],
        };
        assert!(filter.matches(&connection("127.0.0.1", 8080, "LISTEN")));
        assert!(filter.matches(&connection("::1", 8080, "LISTEN")));
        assert!(!filter.matches(&connection("10.0.0.5", 8080, "LISTEN")));
        assert!(!filter.matches(&connection("127.0.0.1", 8080, "ESTABLISHED")));
        // An all-empty filter matches nothing (scanfilter.go:15-18).
        let empty = ScannerFilter {
            state: String::new(),
            ips: Vec::new(),
        };
        assert!(!empty.matches(&connection("127.0.0.1", 8080, "LISTEN")));
    }

    /// Oracle scanner_test.go:49-69: the exit escape must not make delivery
    /// lossy for a receiver that is merely slow — Signal parks on the send
    /// until the receiver takes the batch.
    #[tokio::test]
    async fn signal_delivers_to_a_slow_receiver_without_loss() {
        let subscriber = ScannerSubscriber::new("test", None);
        let batch: ScanBatch = Arc::new(vec![connection("127.0.0.1", 8080, "LISTEN")]);
        let sender = subscriber.clone();
        let task = tokio::spawn(async move { sender.signal(batch).await });

        // Let Signal reach its park first: the batch is deposited (phase 1)
        // but the sender has not returned (phase 2 waits for the handoff).
        ScannerSubscriber::park_tick().await;
        assert!(subscriber.has_parked_item(), "the batch must be deposited");
        assert!(
            !task.is_finished(),
            "signal must stay parked until the receiver takes the batch"
        );

        let got = subscriber.recv().await.expect("the batch must arrive");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].laddr.port, 8080);
        task.await.unwrap();
    }

    /// Oracle scanner_test.go:16-42: with nobody receiving, a parked Signal is
    /// unparked by the close instead of blocking forever (no task leak).
    #[tokio::test]
    async fn a_parked_signal_returns_once_the_channel_closes() {
        let subscriber = ScannerSubscriber::new("test", None);
        let batch: ScanBatch = Arc::new(vec![]);
        // Park the slot directly: a full signal() would itself park in phase 2
        // and this test needs the slot filled with nobody receiving.
        *subscriber.messages.slot.lock().unwrap() = Some(batch.clone());
        let sender = subscriber.clone();
        let task = tokio::spawn(async move { sender.signal(batch).await });
        ScannerSubscriber::park_tick().await;
        assert!(!task.is_finished(), "the second signal must be parked");

        subscriber.close();

        let _ = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("a parked signal must return after close — task leaked");
    }

    /// Oracle forward_test.go:15-42's receive side: recv yields None once the
    /// channel is closed and drained (the two-value receive's `!ok`).
    #[tokio::test]
    async fn recv_returns_none_after_close() {
        let subscriber = ScannerSubscriber::new("test", None);
        subscriber.close();
        assert!(subscriber.recv().await.is_none());
    }

    /// The filtered path: only matching rows are handed over, and an empty
    /// match set is still a real (empty) batch, like Go's filtered slice.
    #[tokio::test]
    async fn signal_filters_rows_and_delivers_empty_batches_when_filtered() {
        let filter = ScannerFilter {
            state: "LISTEN".into(),
            ips: vec!["127.0.0.1".into()],
        };
        let subscriber = ScannerSubscriber::new("test", Some(filter));
        let batch: ScanBatch = Arc::new(vec![
            connection("127.0.0.1", 1, "LISTEN"),
            connection("10.0.0.5", 2, "LISTEN"),
            connection("127.0.0.1", 3, "ESTABLISHED"),
        ]);
        let sender = subscriber.clone();
        let task = tokio::spawn(async move { sender.signal(batch).await });
        let got = subscriber.recv().await.unwrap();
        task.await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].laddr.port, 1);

        let empty: ScanBatch = Arc::new(vec![connection("10.0.0.5", 4, "LISTEN")]);
        let sender = subscriber.clone();
        let task = tokio::spawn(async move { sender.signal(empty).await });
        let got = subscriber.recv().await.unwrap();
        task.await.unwrap();
        assert!(
            got.is_empty(),
            "a filtered-to-nothing batch is still delivered"
        );
    }
}
