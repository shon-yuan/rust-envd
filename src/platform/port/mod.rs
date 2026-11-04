//! Platform-neutral port-scan and port-forward contract types.
//!
//! gopsutil's `net.Addr`/`net.ConnectionStat` and the [`ScanSource`] seam are
//! shared by the Linux `/proc/net/tcp` scanner and the Windows
//! `GetExtendedTcpTable` backend. Only the values a platform can actually
//! produce differ; the dedup key and the forwarder's filter consume these
//! types unchanged on both platforms. The scan loop, its subscriber
//! rendezvous and the live-upgrade forward record are platform-neutral too:
//! the Linux forwarder reaches them through `crate::port`, the Windows one
//! through `crate::platform::windows::port`.

use std::io;

pub mod forwarded;
pub mod scan;
pub mod scan_subscriber;

pub use forwarded::{ForwardedPort, PortState, DEFAULT_GATEWAY_IP};

/// gopsutil `net.Addr` (the fields the port package and the dedup key use).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addr {
    pub ip: String,
    pub port: u32,
}

/// gopsutil `net.ConnectionStat`, reduced to the fields `net.Connections`
/// populates that the oracle port package reads (`Family`, `Laddr`, `Status`,
/// `Pid`; forward.go:159-191) plus the dedup-key inputs. `Uids` is dropped:
/// gopsutil fetches and then drops every read error (`conn.Uids, _ =`) and no
/// port-package code reads it (net_linux.go:467-470).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionStat {
    pub fd: u32,
    pub family: u32,
    pub sock_type: u32,
    pub laddr: Addr,
    pub raddr: Addr,
    pub status: String,
    pub pid: i32,
}

/// The scan the scanner broadcasts. The production source reads `/proc` or the
/// Win32 TCP table; tests inject a synthetic proc tree or a fully scripted
/// source.
pub trait ScanSource: Send + Sync {
    /// gopsutil `net.Connections("tcp")`: both address families or an error.
    fn connections(&self) -> io::Result<Vec<ConnectionStat>>;
}
