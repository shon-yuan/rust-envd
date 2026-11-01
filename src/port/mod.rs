//! The loopback port forwarder, ported from the pinned oracle
//! `internal/port` (scan.go, scan_subscriber.go, scanfilter.go, forward.go)
//! at 51083a83cc0903185d992725ea6bb33683814b29.
//!
//! The scanner periodically reads the open TCP connections (`procnet` +
//! `scan`), filters them per subscriber ([`scan_subscriber`]), and the
//! [`forward`] forwarder reconciles its socat set against each scan: every
//! LISTEN socket on 127.0.0.1 / localhost / ::1 gets a `socat` bound to the
//! guest gateway IP forwarding to itself, and every port that stopped
//! listening gets its socat process group killed.
//!
//! The live-upgrade handover slice is landed: [`forward::ForwardedPort`]
//! carries the wire encoding of handover.proto fields 1-5 (byte-identical to
//! Go's proto.Marshal by construction), [`forward::Forwarder::export_forwards_hold`]
//! holds the forwarder lock across the execve, and main.rs wires the
//! `--resume-handover` re-adoption and the doUpgrade export call.

pub mod forward;
pub mod procnet;
pub mod scan;
pub mod scan_subscriber;

pub use forward::{
    family_to_ip_version, forwarding_available, ForwardUnavailable, ForwardedPort, Forwarder,
    ForwardsHold, RealSocat, SocatSpawn,
};
pub use procnet::{ConnectionStat, ProcNetSource, ScanSource, AF_INET, AF_INET6};
pub use scan::Scanner;
pub use scan_subscriber::{ScannerFilter, ScannerSubscriber};
