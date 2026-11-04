//! The Windows loopback port forwarder.
//!
//! The oracle's `internal/port` package (`forward.go`, `scan.go`,
//! `scan_subscriber.go`, `scanfilter.go`) at
//! 51083a83cc0903185d992725ea6bb33683814b29 forward every port listening on
//! `127.0.0.1`/`localhost`/`::1` onto the guest gateway IP `169.254.0.21` by
//! spawning `socat` children. Windows has no socat in the guest image, so the
//! same wiring is served by an in-process Tokio TCP proxy instead of a child
//! process: the scan loop, its subscriber rendezvous, the filter and the
//! `ForwardedPort` record are the shared
//! [`crate::platform::port`] modules, and only the forward action differs.
//! `docs/windows-port.md` §6 fixes that deviation.
//!
//! Everything else keeps the oracle's state machine: each scan marks every
//! tracked port DELETE, re-marks the ones still LISTENing FORWARD, starts a
//! proxy for a new port, and stops whatever is still DELETE. A start refused
//! by the workload spawn gate (the Windows analogue of the frozen socat
//! cgroup) is retried on the next scan; a start that failed is not.

pub mod forward;

pub use forward::{family_to_ip_version, Forwarder, AF_INET, AF_INET6};
