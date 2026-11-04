//! Oracle forward.go on Windows: the loopback forwarder as an in-process TCP
//! proxy.
//!
//! Every scan refresh (forward.go:126-204) marks all tracked ports DELETE,
//! re-marks the ones still seen LISTENing FORWARD, starts a proxy for new
//! ports and stops the DELETE ones. A proxy that could not start is NOT
//! tracked (the next scan sees an unforwarded port and tries again,
//! forward.go:184-189) — but a start REFUSED because the workload spawn gate
//! is held by a freeze retries on the next scan the same way
//! (forward.go:236-248): both paths remove the entry, the difference is only
//! the log level.
//!
//! Deviations, all fixed by `docs/windows-port.md` §6 and disclosed here:
//! - the forward action is an in-process Tokio proxy instead of a `socat`
//!   child, because the Windows guest image has no socat. The observable
//!   contract is the same: `<gateway>:<port>` accepts and relays to
//!   `localhost:<port>` (v6 for a family-6 listener);
//! - the frozen-cgroup gate becomes the workload registry's spawn gate
//!   ([`WorkloadRegistry::begin_spawn`]) — the same admission/retry shape,
//!   including the 100 ms admit wait and the retryable refusal;
//! - a live upgrade carries no forwards: the proxy state is owned by this
//!   process, so the Windows handover blob's forward list stays empty and the
//!   successor's first scan re-establishes every proxy. (Linux carries the
//!   socat pids because the children outlive the execve.)
//!
//! Availability follows the Linux policy: only a guest daemon forwards.
//! A development daemon on a host reaches the oracle's own "unavailable" path
//! (forward.go:101-105) — warn, subscribe to nothing, start no tasks — so a
//! proxy bound to `169.254.0.21` never exposes host loopback services.

use std::{collections::HashMap, io, net::Ipv4Addr, net::Ipv6Addr, net::SocketAddr, sync::Arc};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::{oneshot, Mutex},
    task::JoinHandle,
};

use crate::{
    logs::Logger,
    platform::{
        port::{
            scan::Scanner,
            scan_subscriber::{ScannerFilter, ScannerSubscriber},
            ConnectionStat, PortState, DEFAULT_GATEWAY_IP,
        },
        windows::registry::WorkloadRegistry,
    },
};

/// WinSock `AF_INET` (`winsock2.h`): the value the TCP table reports for IPv4.
pub const AF_INET: u32 = 2;
/// WinSock `AF_INET6`: the value the TCP table reports for IPv6. It differs
/// from Linux (10), so the shared `family_to_ip_version` cannot be reused.
pub const AF_INET6: u32 = 23;

/// WinSock family -> IP version, the oracle's forward.go:389-398 mapping.
pub fn family_to_ip_version(family: u32) -> u32 {
    match family {
        AF_INET => 4,
        AF_INET6 => 6,
        _ => 0,
    }
}

/// The upstream the oracle's socat dials (forward.go:217):
/// `TCP<family>:localhost:<port>` — `::1` for a family-6 listener, `127.0.0.1`
/// otherwise. The proxy always LISTENs on the IPv4 gateway address, exactly
/// like socat's `TCP4-LISTEN`.
fn target_for(family: u32, port: u16) -> SocketAddr {
    match family {
        6 => SocketAddr::new(std::net::IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        _ => SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), port),
    }
}

/// One running proxy.
struct ProxyHandle {
    /// Ends the accept loop; the listener is dropped by the task.
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

/// Oracle forward.go:33-45, with a proxy handle in place of the socat handle.
struct PortToForward {
    proxy: Option<ProxyHandle>,
    /// Process ID of the process that's listening on port.
    pid: i32,
    /// family version of the ip.
    family: u32,
    state: PortState,
    port: u32,
}

/// Oracle forward.go:58-73. The ports map is a `tokio::sync::Mutex`: the scan
/// loop holds it across the whole refresh, including the awaited bind.
pub struct Forwarder {
    logger: Logger,
    /// Admission for every proxy start: a frozen workload defers new proxies
    /// until thawed (insert→start→retry contract), like the socat cgroup.
    registry: Arc<WorkloadRegistry>,
    /// Serially the start/stop actions and the map they live in.
    ports: Arc<Mutex<HashMap<String, PortToForward>>>,
    scanner_subscriber: Option<Arc<ScannerSubscriber>>,
    source_ip: [u8; 4],
}

impl Forwarder {
    /// Oracle NewForwarder (forward.go:79-85) with this environment's
    /// availability.
    pub fn new(
        logger: &Logger,
        scanner: &Arc<Scanner>,
        registry: Arc<WorkloadRegistry>,
        guest: bool,
    ) -> Self {
        Self::build(logger, scanner, registry, forwarding_available(guest))
    }

    /// Oracle newForwarder (forward.go:89-117); `unavailable` is why this
    /// build cannot forward ports, Ok where it can. An unavailable forwarder
    /// subscribes to nothing — a subscription nobody drains would park the
    /// scan loop on its first broadcast (forward.go:101-105).
    pub fn build(
        logger: &Logger,
        scanner: &Arc<Scanner>,
        registry: Arc<WorkloadRegistry>,
        unavailable: Result<(), String>,
    ) -> Self {
        let mut scanner_subscriber = None;
        match unavailable {
            Err(unavailable) => {
                logger
                    .warn()
                    .err(&unavailable)
                    .msg("port forwarding disabled");
            }
            Ok(()) => {
                // We only want to forward ports that are actively listening on
                // localhost (forward.go:106-113).
                scanner_subscriber = Some(scanner.add_subscriber(
                    "port-forwarder",
                    Some(ScannerFilter {
                        ips: vec!["127.0.0.1".into(), "localhost".into(), "::1".into()],
                        state: "LISTEN".into(),
                    }),
                ));
            }
        }
        Self {
            logger: logger.clone(),
            registry,
            ports: Arc::new(Mutex::new(HashMap::new())),
            scanner_subscriber,
            source_ip: DEFAULT_GATEWAY_IP,
        }
    }

    /// Test seam: bind a loopback address instead of the guest gateway IP, so
    /// the proxy can be driven end to end on a machine where `169.254.0.21`
    /// is not a local address.
    #[doc(hidden)]
    pub fn with_source_ip(mut self, source_ip: [u8; 4]) -> Self {
        self.source_ip = source_ip;
        self
    }

    /// Oracle Forwards (forward.go:122-124): whether StartForwarding and the
    /// scan it consumes need to run.
    pub fn forwards(&self) -> bool {
        self.scanner_subscriber.is_some()
    }

    /// Test support: the tracked map keys under the lock
    /// (TestStartForwarding_RetriesAPortSkippedWhileFrozen's `tracked()`).
    #[doc(hidden)]
    pub async fn tracked_keys(&self) -> Vec<String> {
        self.ports.lock().await.keys().cloned().collect()
    }

    /// Test support: the forwarder's own subscription, so tests can drive
    /// scans the way the oracle tests drive `f.scannerSubscriber.Messages`.
    #[doc(hidden)]
    pub fn subscriber(&self) -> Option<Arc<ScannerSubscriber>> {
        self.scanner_subscriber.clone()
    }

    /// Oracle StartForwarding (forward.go:126-204). Returns on the stop
    /// signal (Go's ctx) or when the scan channel closed (forward.go:133-141).
    pub async fn start_forwarding(&self, mut stop: oneshot::Receiver<()>) {
        let Some(subscriber) = self.scanner_subscriber.clone() else {
            self.logger
                .error()
                .msg("Cannot start forwarding because scanner subscriber is nil");
            return;
        };
        loop {
            let procs = tokio::select! {
                // Go's ctx cancellation (forward.go:214) kills every socat on
                // the way out. The proxy tasks are owned by this process, so
                // the same arm stops every one of them.
                _ = &mut stop => {
                    self.stop_all().await;
                    return;
                }
                procs = subscriber.recv() => match procs {
                    None => return,
                    Some(procs) => procs,
                },
            };
            // Serialize the whole refresh against a concurrent reader.
            let mut ports = self.ports.lock().await;
            self.refresh(&mut ports, &procs).await;
        }
    }

    /// The refresh body (forward.go:148-199), under the ports lock.
    async fn refresh(&self, ports: &mut HashMap<String, PortToForward>, procs: &[ConnectionStat]) {
        // Everything tracked goes to DELETE first (forward.go:151-155).
        for port in ports.values_mut() {
            port.state = PortState::Delete;
        }
        for seen in procs {
            let key = format!("{}-{}", seen.pid, seen.laddr.port);
            match ports.get_mut(&key) {
                // Still forwarded; keep its running proxy (forward.go:163-167).
                Some(tracked) => tracked.state = PortState::Forward,
                None => {
                    self.logger
                        .debug()
                        .str("ip", &seen.laddr.ip)
                        .int("port", seen.laddr.port as i64)
                        .int("family", family_to_ip_version(seen.family) as i64)
                        .str("state", &seen.status)
                        .msg("Detected new opened port on localhost that is not forwarded");
                    // Go inserts the entry BEFORE starting the socat
                    // (forward.go:177-189): a failed start (retry=false)
                    // STAYS tracked — the port reads as forwarded and dark
                    // until its listener reopens — while a REFUSED start
                    // (retry=true, frozen workload) is removed so the next
                    // scan sees an unforwarded port and tries again.
                    ports.insert(
                        key.clone(),
                        PortToForward {
                            proxy: None,
                            pid: seen.pid,
                            port: seen.laddr.port,
                            state: PortState::Forward,
                            family: family_to_ip_version(seen.family),
                        },
                    );
                    let mut retry = false;
                    if let Some(tracked) = ports.get_mut(&key) {
                        retry = self.start_port_forwarding(tracked).await;
                    }
                    if retry {
                        ports.remove(&key);
                    }
                }
            }
        }
        // Stop everything still marked DELETE (forward.go:193-199).
        for port in ports.values_mut() {
            if port.state == PortState::Delete {
                self.stop_port_forwarding(port).await;
            }
        }
    }

    /// Oracle startPortForwarding (forward.go:210-275). Returns retry: a
    /// start REFUSED (workload frozen) is attempted again on the next scan; a
    /// start that FAILED is not (forward.go:206-209).
    async fn start_port_forwarding(&self, port: &mut PortToForward) -> bool {
        let Ok(port_number) = u16::try_from(port.port) else {
            // The oracle formats the port into socat's argv, so an implausible
            // value fails at spawn time there; here it fails at conversion.
            self.logger
                .error()
                .int("port", port.port as i64)
                .msg("Failed to start port forwarding - port is not a TCP port");
            return false;
        };
        let bind = SocketAddr::new(
            std::net::IpAddr::V4(Ipv4Addr::from(self.source_ip)),
            port_number,
        );
        let target = target_for(port.family, port_number);
        self.logger
            .debug()
            .str("bind", bind.to_string())
            .str("target", target.to_string())
            .int("pid", port.pid as i64)
            .int("family", port.family as i64)
            .str("sourceIP", dotted(self.source_ip))
            .int("port", port.port as i64)
            .msg("About to start port forwarding");

        // A frozen workload refuses new starts; the next scan retries
        // (forward.go:235-237). The gate is released once the listener is
        // bound — the admission covers the same window as a socat reaching
        // execve.
        let admission = match self.registry.begin_spawn().await {
            Ok(admission) => admission,
            Err(error) => {
                self.logger
                    .debug()
                    .int("port", port.port as i64)
                    .err(&error)
                    .msg("Deferring port forwarding: the workload is frozen");
                return true;
            }
        };
        let bound = TcpListener::bind(bind).await;
        drop(admission);

        let listener = match bound {
            Ok(listener) => listener,
            Err(error) => {
                self.logger
                    .error()
                    .str("bind", bind.to_string())
                    .err(&error)
                    .msg("Failed to start port forwarding - failed to bind the proxy");
                return false;
            }
        };
        let (stop, stop_receiver) = oneshot::channel();
        let logger = self.logger.clone();
        let task = tokio::spawn(run_proxy(listener, target, stop_receiver, logger));
        port.proxy = Some(ProxyHandle { stop, task });
        false
    }

    /// Oracle stopPortForwarding (forward.go:277-305): clear the handle and
    /// stop the proxy. The accept loop is joined so the listener is closed
    /// before the scan returns — unlike the oracle's asynchronous socat kill,
    /// this cannot race a rebind of the same port on the next refresh.
    async fn stop_port_forwarding(&self, port: &mut PortToForward) {
        let Some(handle) = port.proxy.take() else {
            return;
        };
        let logger = self
            .logger
            .with_str("pid", port.pid.to_string())
            .with_str("family", port.family.to_string())
            .with_str("sourceIP", dotted(self.source_ip))
            .with_str("port", port.port.to_string());
        logger.debug().msg("Stopping port forwarding");
        let _ = handle.stop.send(());
        let _ = handle.task.await;
        logger.debug().msg("Stopped port forwarding");
    }

    /// The ctx-death stop of the run loop (Go's CommandContext fails every
    /// socat): every tracked proxy is stopped. Entries stay in the map, the
    /// way the oracle leaves its socat records behind on ctx cancellation.
    async fn stop_all(&self) {
        for port in self.ports.lock().await.values_mut() {
            self.stop_port_forwarding(port).await;
        }
    }
}

/// The proxy task: accept on the gateway address, dial the listener's
/// upstream, and relay both directions until either side closes.
async fn run_proxy(
    listener: TcpListener,
    target: SocketAddr,
    mut stop: oneshot::Receiver<()>,
    logger: Logger,
) {
    loop {
        tokio::select! {
            _ = &mut stop => return,
            accepted = listener.accept() => {
                let (client, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // The listener outlives transient accept errors; a
                        // closed listener ends through the stop arm.
                        logger.debug().err(&error).msg("Port forwarding proxy accept failed");
                        continue;
                    }
                };
                let logger = logger.clone();
                tokio::spawn(async move {
                    let mut upstream = match TcpStream::connect(target).await {
                        Ok(upstream) => upstream,
                        Err(error) => {
                            logger
                                .debug()
                                .str("target", target.to_string())
                                .str("peer", peer.to_string())
                                .err(&error)
                                .msg("Port forwarding proxy could not reach the listener");
                            return;
                        }
                    };
                    let mut client = client;
                    if let Err(error) =
                        tokio::io::copy_bidirectional(&mut client, &mut upstream).await
                    {
                        logger.debug().err(&error).msg("Port forwarding proxy connection failed");
                    }
                });
            }
        }
    }
}

/// Oracle forward.go:75-77: this build can forward ports. Go returns nil
/// unconditionally because it only builds for the guest image; the Rust
/// daemon also runs as a development daemon on a host, where a proxy bound to
/// the guest gateway IP would try to expose host loopback services — there
/// the oracle's own "unavailable" path disables the forwarder exactly as
/// designed (forward.go:101-105).
pub fn forwarding_available(guest: bool) -> Result<(), String> {
    if guest {
        Ok(())
    } else {
        Err(io::Error::other(
            "port forwarding forwards guest loopback ports onto the guest gateway IP; \
             it is only available to a guest daemon",
        )
        .to_string())
    }
}

fn dotted(ip: [u8; 4]) -> String {
    let [a, b, c, d] = ip;
    format!("{a}.{b}.{c}.{d}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::port::scan_subscriber::ScanBatch;
    use crate::platform::port::{Addr, ConnectionStat, ScanSource};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// A scripted scan source the tests never call (scans are driven through
    /// the subscriber directly, exactly like the Linux forwarder tests).
    struct NullSource;

    impl ScanSource for NullSource {
        fn connections(&self) -> io::Result<Vec<ConnectionStat>> {
            Ok(Vec::new())
        }
    }

    fn test_scanner() -> Arc<Scanner> {
        Arc::new(Scanner::with_source(
            &Logger::none(),
            Duration::from_secs(3600),
            Arc::new(NullSource),
        ))
    }

    fn listen_row(pid: i32, port: u16, family: u32) -> ConnectionStat {
        ConnectionStat {
            fd: 0,
            family,
            sock_type: 1,
            laddr: Addr {
                ip: "127.0.0.1".into(),
                port: u32::from(port),
            },
            raddr: Addr {
                ip: "0.0.0.0".into(),
                port: 0,
            },
            status: "LISTEN".into(),
            pid,
        }
    }

    fn batch(rows: Vec<ConnectionStat>) -> ScanBatch {
        Arc::new(rows)
    }

    /// Starts the forwarder's run loop and returns its stop channel plus task.
    fn run(forwarder: &Arc<Forwarder>) -> (oneshot::Sender<()>, JoinHandle<()>) {
        let (stop, stop_rx) = oneshot::channel();
        let forwarder = Arc::clone(forwarder);
        (
            stop,
            tokio::spawn(async move { forwarder.start_forwarding(stop_rx).await }),
        )
    }

    /// A forward address on the test loopback alias. The alias keeps the
    /// proxy address (127.0.0.2) apart from the upstream it dials (127.0.0.1)
    /// on the same port, so the guest gateway address does not have to exist
    /// on the machine running the test.
    const FORWARD_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

    fn free_forward_port() -> u16 {
        let probe = std::net::TcpListener::bind((FORWARD_IP, 0)).expect("a free loopback port");
        probe.local_addr().expect("bound address").port()
    }

    async fn accepts_connections(port: u16) -> bool {
        TcpStream::connect((FORWARD_IP, port)).await.is_ok()
    }

    /// Oracle forward.go:389-398, over WinSock's family values.
    #[test]
    fn family_to_ip_version_maps_only_winsock_inet_families() {
        assert_eq!(family_to_ip_version(AF_INET), 4);
        assert_eq!(family_to_ip_version(AF_INET6), 6);
        assert_eq!(family_to_ip_version(1), 0, "unknown family stays 0");
        assert_eq!(
            family_to_ip_version(10),
            0,
            "Linux AF_INET6 (10) must not be read as WinSock's 23"
        );
    }

    /// The oracle always LISTENs on the IPv4 gateway address
    /// (`TCP4-LISTEN`); only the dialed upstream follows the listener's
    /// family (`TCP6:localhost`), forward.go:216-217.
    #[test]
    fn the_proxy_listens_on_the_gateway_and_dials_the_listener_family() {
        assert_eq!(
            target_for(4, 8080),
            SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 8080)
        );
        assert_eq!(
            target_for(6, 8080),
            SocketAddr::new(std::net::IpAddr::V6(Ipv6Addr::LOCALHOST), 8080)
        );
    }

    /// Oracle scanner_test.go:101-133: an unavailable forwarder holds no
    /// subscription nobody drains and tells envd not to run the scan.
    #[test]
    fn an_unavailable_forwarder_subscribes_to_nothing() {
        let scanner = test_scanner();
        let registry = Arc::new(WorkloadRegistry::new());
        let forwarder = Forwarder::build(
            &Logger::none(),
            &scanner,
            Arc::clone(&registry),
            Err("no forwarding in this build".into()),
        );
        assert!(!forwarder.forwards());
        assert_eq!(scanner.subscriber_count(), 0);

        let available = Forwarder::build(&Logger::none(), &scanner, registry, Ok(()));
        assert!(available.forwards());
        assert_eq!(scanner.subscriber_count(), 1);
        assert_eq!(available.subscriber().unwrap().id(), "port-forwarder");
    }

    /// The disclosed dev-mode gate: forwarding_available is the oracle's
    /// forwardingAvailable() with the environment as its only input.
    #[test]
    fn forwarding_available_follows_the_guest_environment() {
        assert!(forwarding_available(true).is_ok());
        assert!(forwarding_available(false).is_err());
    }

    /// Oracle forward_frozen_test.go:46-95: a start refused while the
    /// workload gate is held must not be tracked; once the gate lowers, the
    /// next scan must start the proxy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_port_refused_while_frozen_is_not_tracked_and_is_retried_after_thaw() {
        let scanner = test_scanner();
        let registry = Arc::new(WorkloadRegistry::new());
        let forwarder = Arc::new(
            Forwarder::new(&Logger::none(), &scanner, Arc::clone(&registry), true)
                .with_source_ip(FORWARD_IP.octets()),
        );
        let subscriber = forwarder.subscriber().unwrap();
        let (stop, task) = run(&forwarder);

        let port = free_forward_port();
        let key = format!("4242-{port}");

        // Freeze, scan: the refusal removes the entry again. The refusal
        // itself waits out the gate's admit bound, so a short settle is
        // enough for the refresh to finish.
        registry
            .raise_and_drain(None)
            .await
            .expect("raise the gate");
        subscriber
            .signal(batch(vec![listen_row(4242, port, AF_INET)]))
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !forwarder.tracked_keys().await.contains(&key),
            "a refused start must not stay tracked (key {key})"
        );
        assert!(
            !accepts_connections(port).await,
            "a refused start must not bind the forward address"
        );

        // Thaw and scan again: the retry must start the proxy.
        registry.lower();
        subscriber
            .signal(batch(vec![listen_row(4242, port, AF_INET)]))
            .await;
        let mut tracked = false;
        for _ in 0..200 {
            if forwarder.tracked_keys().await.contains(&key) {
                tracked = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(tracked, "the retry after thaw must forward port {port}");
        let mut listening = false;
        for _ in 0..200 {
            if accepts_connections(port).await {
                listening = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(listening, "the proxy must listen on the forward address");

        // The listener goes away: the entry stays, the proxy is stopped.
        subscriber.signal(batch(Vec::new())).await;
        let mut released = false;
        for _ in 0..200 {
            if !accepts_connections(port).await {
                released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            released,
            "a port that stopped listening must stop forwarding"
        );
        assert!(
            forwarder.tracked_keys().await.contains(&key),
            "Go never removes keys; the stopped entry stays tracked"
        );

        let _ = stop.send(());
        let _ = task.await;
    }

    /// The proxy relays both directions: a connection to the forward address
    /// reaches the listener and its bytes come back — the oracle's
    /// `socat ... TCP4:localhost:<port>` relay.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_proxy_relays_to_the_local_listener() {
        let scanner = test_scanner();
        let registry = Arc::new(WorkloadRegistry::new());
        let forwarder = Arc::new(
            Forwarder::new(&Logger::none(), &scanner, registry, true)
                .with_source_ip(FORWARD_IP.octets()),
        );
        let subscriber = forwarder.subscriber().unwrap();
        let (stop, task) = run(&forwarder);

        // A port free on BOTH loopback aliases: the proxy binds 127.0.0.2:P
        // and dials 127.0.0.1:P.
        let mut chosen = None;
        for _ in 0..20 {
            let port = free_forward_port();
            if let Ok(target) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
                chosen = Some((port, target));
                break;
            }
        }
        let (port, target) = chosen.expect("a port free on both loopback aliases");
        let echo = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.expect("accept");
            let mut buffer = [0u8; 4];
            socket.read_exact(&mut buffer).await.expect("read");
            socket.write_all(&buffer).await.expect("write");
        });

        subscriber
            .signal(batch(vec![listen_row(4242, port, AF_INET)]))
            .await;
        let mut client = None;
        for _ in 0..200 {
            match TcpStream::connect((FORWARD_IP, port)).await {
                Ok(stream) => {
                    client = Some(stream);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        let mut client = client.expect("the proxy must accept a connection");
        client.write_all(b"ping").await.expect("write");
        let mut echoed = [0u8; 4];
        client.read_exact(&mut echoed).await.expect("relayed echo");
        assert_eq!(&echoed, b"ping");

        let _ = echo.await;
        let _ = stop.send(());
        let _ = task.await;
    }
}
