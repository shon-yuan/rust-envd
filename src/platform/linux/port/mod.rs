//! The loopback port forwarder, ported from the pinned oracle
//! `internal/port` (scan.go, scan_subscriber.go, scanfilter.go, forward.go)
//! at 51083a83cc0903185d992725ea6bb33683814b29.
//!
//! The scanner periodically reads the open TCP connections (`procnet` +
//! the shared `scan`), filters them per subscriber (the shared
//! `scan_subscriber`), and the [`forward`] forwarder reconciles its socat set
//! against each scan: every LISTEN socket on 127.0.0.1 / localhost / ::1 gets
//! a `socat` bound to the guest gateway IP forwarding to itself, and every
//! port that stopped listening gets its socat process group killed.
//!
//! The scan loop, its subscriber rendezvous, the `ForwardedPort` record and
//! the gateway address are platform-neutral and live in
//! `crate::platform::port`; only the `/proc/net/tcp` source and the
//! socat/cgroup spawn edge are Linux-specific.
//!
//! The live-upgrade handover slice is landed:
//! [`crate::platform::port::ForwardedPort`] carries the wire encoding of
//! handover.proto fields 1-5 (byte-identical to Go's proto.Marshal by
//! construction), [`forward::Forwarder::export_forwards_hold`] holds the
//! forwarder lock across the execve, and main.rs wires the
//! `--resume-handover` re-adoption and the doUpgrade export call.

pub mod forward;
pub mod procnet;

pub use crate::platform::port::{
    scan, scan_subscriber, Addr, ConnectionStat, ForwardedPort, ScanSource, DEFAULT_GATEWAY_IP,
};
pub use forward::{
    family_to_ip_version, forwarding_available, ForwardUnavailable, Forwarder, ForwardsHold,
    RealSocat, SocatSpawn,
};
pub use procnet::{ProcNetSource, AF_INET, AF_INET6};
pub use scan::Scanner;
pub use scan_subscriber::{ScannerFilter, ScannerSubscriber};
