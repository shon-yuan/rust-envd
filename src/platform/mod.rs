//! Platform seams, one module tree per supported operating system.
//!
//! The Linux tree holds the parity implementation for the pinned Go oracle and
//! is called directly by the daemon. The Windows tree implements the filesystem
//! slice of the port specified in `docs/windows-port.md`: account resolution,
//! filesystem metadata and mutation, the shared `POST /files` write seam, the
//! Connect filesystem runtime, the `ReadDirectoryChangesW` watcher registry,
//! local-port enumeration, the in-process loopback port forwarder, DPAPI
//! metadata provisioning and the workload registry/thread-suspension resource
//! backend. Process execution and clock
//! adjustment still answer [`std::io::ErrorKind::Unsupported`].
//!
//! The traits deliberately carry the request/response types that are already
//! platform-neutral (`crate::proto`) and `std::io::Error`, the daemon's
//! dominant error type. The daemon dispatches the filesystem and watcher
//! surfaces through them; the remaining Windows backends are still direct
//! stubs.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use crate::exec_context::{Account, Defaults};
use crate::proto::filesystem as pb;
use connectrpc::ConnectError;
use sha2::{Digest, Sha512};
use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

pub mod compose_files;
/// The `POST /files` write seam: directory creation, ownership, open mode,
/// file identity and `user.e2b.*` metadata, one implementation per platform.
pub mod file_writes;
/// The `POST /upgrade` staging seam: stream the delivered replacement binary
/// to the fixed upgrade path with the platform's own replace semantics.
pub mod upgrade_fs;

/// The Go `os.PathError` refusal text for a filesystem operation, used by the
/// handover blob and staged-binary writers. Linux renders the exact
/// `op path: errno` text the oracle emits; Windows has no errno vocabulary, so
/// its arm keeps the same shape with the stable Win32-derived text from
/// [`windows::fs_runtime::win32_code_text`] — Rust's `io::Error` Display would
/// append a ` (os error N)` suffix that Go never emits, and the localized
/// `FormatMessage` text is not reproducible across machines.
pub(crate) fn path_error_text(op: &str, path: &str, error: io::Error) -> io::Error {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::process::path_text_error(op, path, error)
    }
    #[cfg(windows)]
    {
        let text = match error.raw_os_error() {
            Some(code) => windows::fs_runtime::win32_code_text(code as u32),
            None => error.to_string(),
        };
        io::Error::new(error.kind(), format!("{op} {path}: {text}"))
    }
}
/// Platform-neutral port-scan contract types shared by the Linux `/proc`
/// scanner and the Windows `GetExtendedTcpTable` backend.
pub mod port;

/// Resolve a username to the account record the HTTP and filesystem adapters
/// pass around. Linux reads the password database; Windows resolves the SID
/// through the local SAM and synthesizes `uid`/`gid`.
#[cfg(target_os = "linux")]
pub use linux::account::lookup as lookup_account;
#[cfg(windows)]
pub use windows::account::lookup as lookup_account;

/// The daemon-owned watcher registry and snapshot types. Windows gains its own
/// registry in the watcher slice.
#[cfg(target_os = "linux")]
pub use linux::filesystem::watchers::{keepalive_interval, WatcherRegistry, WatcherSnapshot};
/// The filesystem runtime the shared Connect adapter drives. Linux is the
/// parity implementation; Windows serves the same surface over Win32.
#[cfg(target_os = "linux")]
pub use linux::filesystem::FilesystemRuntime;
#[cfg(windows)]
pub use windows::fs_runtime::FilesystemRuntime;
/// The Windows watcher registry and keepalive parser behind the same
/// `WatcherService` surface as the Linux inotify registry.
#[cfg(windows)]
pub use windows::watchers::{keepalive_interval, WatcherRegistry};

/// Resolve the account the daemon itself runs as. The Connect ID operations
/// validate an explicitly supplied Basic username against this identity
/// without requiring a default account; both arms return the same error text
/// the Linux daemon produced before the seam existed.
pub fn daemon_account() -> Result<Account, String> {
    #[cfg(target_os = "linux")]
    {
        linux::account::daemon_account()
    }
    #[cfg(windows)]
    {
        windows::account::lookup(&windows::account::current_user_name())
    }
}

/// Per-request filesystem operations behind the shared Connect adapter.
///
/// The Linux implementation forwards to [`FilesystemRuntime`] verbatim, so the
/// parity behaviour and error text stay in one place.
pub trait FilesystemService: Send + Sync {
    fn stat(&self, path: &str) -> Result<pb::EntryInfo, ConnectError>;
    fn make_dir(&self, path: &str) -> Result<pb::EntryInfo, ConnectError>;
    fn move_entry(&self, source: &str, destination: &str) -> Result<pb::EntryInfo, ConnectError>;
    fn list_dir(&self, path: &str, depth: u32) -> Result<Vec<pb::EntryInfo>, ConnectError>;
    fn remove(&self, path: &str) -> Result<(), ConnectError>;
}

/// Forwarding implementation: the inherent methods remain the single Linux
/// implementation and are not touched by the seam.
#[cfg(target_os = "linux")]
impl FilesystemService for linux::filesystem::FilesystemRuntime {
    fn stat(&self, path: &str) -> Result<pb::EntryInfo, ConnectError> {
        linux::filesystem::FilesystemRuntime::stat(self, path)
    }

    fn make_dir(&self, path: &str) -> Result<pb::EntryInfo, ConnectError> {
        linux::filesystem::FilesystemRuntime::make_dir(self, path)
    }

    fn move_entry(&self, source: &str, destination: &str) -> Result<pb::EntryInfo, ConnectError> {
        linux::filesystem::FilesystemRuntime::move_entry(self, source, destination)
    }

    fn list_dir(&self, path: &str, depth: u32) -> Result<Vec<pb::EntryInfo>, ConnectError> {
        linux::filesystem::FilesystemRuntime::list_dir(self, path, depth)
    }

    fn remove(&self, path: &str) -> Result<(), ConnectError> {
        linux::filesystem::FilesystemRuntime::remove(self, path)
    }
}

/// The next frame of a WatchDir server stream, boxed so the shared adapter can
/// stay generic over the platform subscription.
pub type WatchFrameFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Result<pb::WatchDirResponse, ConnectError>>> + Send + 'a>>;

/// One open WatchDir server stream. Implemented by the platform registry's
/// subscription type; the shared adapter only needs the next frame.
pub trait WatchSubscription: Send {
    fn next_frame(&mut self) -> WatchFrameFuture<'_>;
}

/// Forwarding implementation over the existing Linux subscription.
#[cfg(target_os = "linux")]
impl WatchSubscription for linux::filesystem::watchers::WatchSubscription {
    fn next_frame(&mut self) -> WatchFrameFuture<'_> {
        Box::pin(linux::filesystem::watchers::WatchSubscription::recv(self))
    }
}

/// The daemon-owned watcher registry the shared adapter drives. Linux forwards
/// to the existing inotify registry; the inotify backend stays private to it.
pub trait WatcherService: Send + Sync {
    type Subscription: WatchSubscription;

    fn create_watcher(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::CreateWatcherRequest,
    ) -> Result<pb::CreateWatcherResponse, ConnectError>;
    fn get_watcher_events(&self, id: &str) -> Result<pb::GetWatcherEventsResponse, ConnectError>;
    fn remove_watcher(&self, id: &str) -> Result<pb::RemoveWatcherResponse, ConnectError>;
    fn watch_dir(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::WatchDirRequest,
        keepalive: Option<&str>,
    ) -> Result<Self::Subscription, ConnectError>;
}

/// Forwarding implementation over the existing Linux registry.
#[cfg(target_os = "linux")]
impl WatcherService for linux::filesystem::watchers::WatcherRegistry {
    type Subscription = linux::filesystem::watchers::WatchSubscription;

    fn create_watcher(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::CreateWatcherRequest,
    ) -> Result<pb::CreateWatcherResponse, ConnectError> {
        linux::filesystem::watchers::WatcherRegistry::create_watcher(self, user, defaults, request)
    }

    fn get_watcher_events(&self, id: &str) -> Result<pb::GetWatcherEventsResponse, ConnectError> {
        linux::filesystem::watchers::WatcherRegistry::get_watcher_events(self, id)
    }

    fn remove_watcher(&self, id: &str) -> Result<pb::RemoveWatcherResponse, ConnectError> {
        linux::filesystem::watchers::WatcherRegistry::remove_watcher(self, id)
    }

    fn watch_dir(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::WatchDirRequest,
        keepalive: Option<&str>,
    ) -> Result<Self::Subscription, ConnectError> {
        linux::filesystem::watchers::WatcherRegistry::watch_dir(
            self, user, defaults, request, keepalive,
        )
    }
}

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

/// The production scan source for this platform: `/proc/net/tcp{,6}` on
/// Linux, the Win32 TCP table on Windows. [`port::scan::Scanner::new`] uses it
/// so the scan loop itself stays platform-neutral.
#[cfg(target_os = "linux")]
pub fn port_scan_source() -> std::sync::Arc<dyn port::ScanSource> {
    std::sync::Arc::new(linux::port::procnet::ProcNetSource::proc_default())
}

#[cfg(windows)]
pub fn port_scan_source() -> std::sync::Arc<dyn port::ScanSource> {
    std::sync::Arc::new(windows::net::WindowsPortScanBackend)
}

/// Local listening TCP ports, used by the forwarder.
///
/// The neutral contract is [`port::ScanSource`]; the default
/// [`PortScanBackend::listening_tcp_ports`] derives the port list from the
/// richer connection view with the Linux scanner's LISTEN filter.
pub trait PortScanBackend: port::ScanSource + Send + Sync {
    fn listening_tcp_ports(&self) -> io::Result<Vec<u16>> {
        let mut seen = std::collections::HashSet::new();
        Ok(self
            .connections()?
            .into_iter()
            .filter(|connection| connection.status == "LISTEN")
            .filter_map(|connection| u16::try_from(connection.laddr.port).ok())
            .filter(|port| seen.insert(*port))
            .collect())
    }
}

/// Sandbox clock and host-time alignment.
pub trait ClockBackend: Send + Sync {
    fn now_unix_nanos(&self) -> i128;
    fn set_system_time(&self, nanos: i64) -> Result<(), String>;
}

/// The ids and address a metadata source carries alongside the access token.
/// They become the internal `E2B_SANDBOX_ID`, `E2B_TEMPLATE_ID` and
/// `E2B_EVENTS_ADDRESS` environment values, never user-overridable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetadataIds {
    pub instance_id: String,
    pub env_id: String,
    pub address: String,
}

/// Access-token delivery outside the Linux MMDS path.
pub trait MetadataBackend: Send + Sync {
    fn init_token(&self) -> io::Result<Option<Vec<u8>>>;

    /// Ids delivered with the token; a source without them reports `None`.
    fn metadata_ids(&self) -> io::Result<Option<MetadataIds>> {
        Ok(None)
    }

    /// The lowercase SHA-512 hex of the delivered token, the exact form
    /// `auth::validate_init` compares against the out-of-band expected hash.
    /// The token buffer is zeroized before this returns.
    fn init_token_sha512(&self) -> io::Result<Option<String>> {
        let Some(token) = self.init_token()? else {
            return Ok(None);
        };
        let token = Zeroizing::new(token);
        Ok(Some(format!("{:x}", Sha512::digest(token.as_slice()))))
    }
}
