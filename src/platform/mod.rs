//! Platform seams, one module tree per supported operating system.
//!
//! The Linux tree holds the parity implementation for the pinned Go oracle and
//! is called directly by the daemon. The Windows tree is the skeleton for the
//! port specified in `docs/windows-port.md`: every trait below has a Windows
//! implementation that currently answers [`std::io::ErrorKind::Unsupported`].
//!
//! The traits deliberately carry the request/response types that are already
//! platform-neutral (`crate::proto`) and `std::io::Error`, the daemon's
//! dominant error type. Linux adapters are introduced in P1, when the first
//! Windows implementations land; until then the Linux daemon calls its
//! backends directly and never dispatches through these traits.

use std::io;
use std::path::Path;
use std::time::Duration;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

/// Identifies one registered filesystem watcher.
pub type WatchId = u64;

/// Starts, enumerates, and terminates workload processes.
pub trait ProcessBackend: Send + Sync {
    fn start(
        &self,
        config: &crate::proto::process::ProcessConfig,
    ) -> io::Result<crate::proto::process::ProcessInfo>;
    fn list(&self) -> io::Result<Vec<crate::proto::process::ProcessInfo>>;
    fn terminate(&self, pid: u32) -> io::Result<()>;
}

/// Limits, freeze/thaw, and resource accounting for the sandbox as a whole.
pub trait ResourceBackend: Send + Sync {
    fn freeze(&self, max_wait: Duration) -> io::Result<()>;
    fn thaw(&self) -> io::Result<()>;
}

/// Filesystem metadata and mutation.
pub trait FsBackend: Send + Sync {
    fn stat(&self, path: &Path) -> io::Result<crate::proto::filesystem::EntryInfo>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<crate::proto::filesystem::EntryInfo>>;
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// Filesystem change notification.
pub trait WatcherBackend: Send + Sync {
    fn watch(&self, path: &Path) -> io::Result<WatchId>;
    fn unwatch(&self, id: WatchId) -> io::Result<()>;
    fn events(&self, id: WatchId) -> io::Result<Vec<crate::proto::filesystem::FilesystemEvent>>;
}

/// Local listening TCP ports, used by the forwarder.
pub trait PortScanBackend: Send + Sync {
    fn listening_tcp_ports(&self) -> io::Result<Vec<u16>>;
}

/// Sandbox clock and host-time alignment.
pub trait ClockBackend: Send + Sync {
    fn now_unix_nanos(&self) -> i128;
    fn set_system_time(&self, nanos: i64) -> Result<(), String>;
}

/// Access-token delivery outside the Linux MMDS path.
pub trait MetadataBackend: Send + Sync {
    fn init_token(&self) -> io::Result<Option<Vec<u8>>>;
}
