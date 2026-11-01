//! The workload cgroup freezer, ported from the pinned oracle
//! `internal/services/cgroups` at 51083a83cc0903185d992725ea6bb33683814b29.
//!
//! Layout mirrors the oracle tree: [`cgroup2`] is the real v2 manager (its
//! construction touches the cgroup filesystem, so only the guest main builds
//! it — development daemons and tests inject [`noop::NoopManager`] or the
//! documented-hidden [`fake`] tree), [`hierarchy`] holds the chain/allowlist
//! arithmetic, [`spawn`] the spawn barrier, [`freeze`] the
//! [`freeze::WorkloadFreezer`].
//!
//! The live-upgrade handover slice is landed: [`freeze::WorkloadFreezer::freeze_workload_hold`]
//! keeps the freeze lock across the execve (process/upgrade.go:585-605),
//! [`freeze::WorkloadFreezer::resume_frozen`] is the incoming image's
//! inheritance (freeze.go:995-1041), [`freeze::WorkloadFreezer::thawed`] is
//! the `Thawed` wait channel (its Go consumer — Readopt's carried kill-timer —
//! belongs to the disclosed live-adoption gap in `crate::process`), and
//! [`freeze::WorkloadFreezer::guest_frozen_paths`]/[`freeze::WorkloadFreezer::set_guest_frozen_paths`]
//! carry the guest-frozen record through the blob. Main wires the hold, the
//! resume and the record in main.rs.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Oracle iface.go:57-61. `System` stays in envd's root cgroup and is
/// deliberately never registered, so it is unaffected by any freeze.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcessType {
    Pty,
    User,
    Socat,
    System,
}

impl ProcessType {
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessType::Pty => "pty",
            ProcessType::User => "user",
            ProcessType::Socat => "socat",
            ProcessType::System => "system",
        }
    }
}

impl fmt::Display for ProcessType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Oracle freeze.go:20: the static sweep's targets, and the cgroups whose
/// spawns the barrier guards.
pub const WORKLOAD_PROCESS_TYPES: [ProcessType; 2] = [ProcessType::User, ProcessType::Pty];

/// Oracle freeze.go:82: the cap every whole-tree walk (the thaw discovery, the
/// guest-freeze scan, the resume audit) is bounded by.
pub const DEFAULT_THAW_MAX_CGROUPS: usize = 8192;
/// Oracle freeze.go:94: the freeze sweep's own default bound.
pub const DEFAULT_FREEZE_MAX_CGROUPS: usize = 512;
/// Oracle freeze.go:66: the backstop window, measured in guest monotonic time
/// so a paused VM's freeze is backed up once it resumes.
pub const DEFAULT_THAW_WATCHDOG_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Oracle iface.go:9-15 plus the errors the v2 manager surfaces. `Io` keeps
/// the raw error so [`vanished`] can classify exactly the two errnos a removal
/// produces (iface.go:80-98: ENOENT for remove-then-open, ENODEV for
/// open-then-remove-then-use; the second is a cgroupfs property a tmpfs tree
/// cannot reproduce).
#[derive(Debug, thiserror::Error)]
pub enum CgroupError {
    #[error("cgroup freeze state unobservable")]
    FrozenUnobservable,
    #[error("unknown process type: {0}")]
    UnknownProcessType(ProcessType),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Oracle iface.go:99-101. Deliberately NOT "any error": the sweep's `Failed`
/// count describes the tree about to be snapshotted, so only the errnos a
/// removal can produce belong here.
pub fn vanished(error: &CgroupError) -> bool {
    match error {
        CgroupError::Io(error) => {
            error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(19)
        }
        _ => false,
    }
}

/// `cgroup.procs`, as a NUL-terminated literal for openat (no allocation in
/// the pre-exec context).
const CGROUP_PROCS: &[u8] = b"cgroup.procs\0";

/// Joins the cgroup referenced by a `cgroup.procs`-bearing directory fd by
/// writing "0" (the calling task) into it — the pre-exec equivalent of Go's
/// CLONE_INTO_CGROUP placement (handler.go:231-242 via cgroupfd_linux.go).
///
/// Shared by the socat forwarder and the user/pty spawn path so every child
/// is placed the same way, before execve. Public because the process runtime
/// is compiled directly into its integration tests (tests/process_runtime.rs
/// includes src/process/mod.rs), which can only reach public items.
///
/// Contract for callers: call between fork and exec, with a directory fd the
/// parent owns; it performs only openat/write/close on that descriptor
/// (async-signal-safe, no allocator, no locks).
pub fn place_into_cgroup(cgroup_fd: i32) -> std::io::Result<()> {
    // SAFETY: openat/write/close with a static path literal and scalar
    // arguments; no allocation, no locks, async-signal-safe throughout.
    let procs_fd = unsafe {
        nix::libc::openat(
            cgroup_fd,
            CGROUP_PROCS.as_ptr().cast(),
            nix::libc::O_WRONLY | nix::libc::O_CLOEXEC,
        )
    };
    if procs_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let zero = b"0";
    // SAFETY: write of a one-byte static buffer to the just-opened descriptor.
    let written = unsafe { nix::libc::write(procs_fd, zero.as_ptr().cast(), 1) };
    let write_result = if written < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    };
    // SAFETY: closing the descriptor this function opened.
    unsafe { nix::libc::close(procs_fd) };
    write_result
}

/// Oracle iface.go:64-78. Managers with no cgroups to read return
/// [`CgroupError::FrozenUnobservable`] from `frozen`.
pub trait Manager: Send + Sync {
    fn get_file_descriptor(&self, process_type: ProcessType) -> Option<i32>;
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError>;
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError>;
    fn frozen(&self, process_type: ProcessType) -> Result<bool, CgroupError>;
    fn close(&self) -> Result<(), CgroupError>;
    /// Go discovers path support by type assertion (`f.mgr.(PathManager)`);
    /// the honest Rust equivalent is an opt-in downcast hook rather than a
    /// Manager full of unimplemented path methods (iface.go:17-25).
    fn as_path_manager(&self) -> Option<&dyn PathManager> {
        None
    }
}

/// Oracle iface.go:26-52: a Manager that can also address cgroups by absolute
/// path, which the hierarchy walk needs.
pub trait PathManager: Manager {
    fn root(&self) -> PathBuf;
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf>;
    /// Only directories: cgroupfs represents every cgroup as one, and the
    /// interface files alongside them are not cgroups (iface.go:34-37).
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError>;
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError>;
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError>;
    /// The SETTLED state, from the "frozen" field of cgroup.events; what
    /// confirmation waits on (iface.go:40-43).
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError>;
    /// The cgroup's OWN requested state, from cgroup.freeze; the thaw and the
    /// pre-spawn probe need this one rather than the settled state
    /// (iface.go:44-51).
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError>;
}

pub mod cgroup2;
/// Test support only: an injected fake cgroupfs, never referenced by the
/// daemon. Hidden from the docs because it is plumbing, not API.
#[doc(hidden)]
pub mod fake;
pub mod freeze;
pub mod hierarchy;
pub mod noop;
pub mod spawn;

pub use freeze::FreezeMode;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_type_names_match_the_oracle_strings() {
        for (process_type, name) in [
            (ProcessType::Pty, "pty"),
            (ProcessType::User, "user"),
            (ProcessType::Socat, "socat"),
            (ProcessType::System, "system"),
        ] {
            assert_eq!(process_type.as_str(), name);
            assert_eq!(process_type.to_string(), name);
        }
    }

    /// The pre-exec placement opens `cgroup.procs` relative to the caller's
    /// directory fd and writes the one-byte task id. A plain directory fd
    /// stands in for the cgroup directory the guest main holds.
    #[test]
    fn place_into_cgroup_writes_zero_into_the_relative_cgroup_procs() {
        let dir = tempfile::TempDir::new().unwrap();
        let procs = dir.path().join("cgroup.procs");
        std::fs::write(&procs, b"").unwrap();
        let cgroup_fd = nix::fcntl::open(
            dir.path(),
            nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_DIRECTORY,
            nix::sys::stat::Mode::empty(),
        )
        .unwrap();
        place_into_cgroup(cgroup_fd).unwrap();
        assert_eq!(std::fs::read_to_string(&procs).unwrap(), "0");
        nix::unistd::close(cgroup_fd).unwrap();
    }

    #[test]
    fn place_into_cgroup_reports_open_failures_before_writing() {
        let dir = tempfile::TempDir::new().unwrap();
        let cgroup_fd = nix::fcntl::open(
            dir.path(),
            nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_DIRECTORY,
            nix::sys::stat::Mode::empty(),
        )
        .unwrap();
        // No cgroup.procs in the directory: ENOENT from openat.
        let error = place_into_cgroup(cgroup_fd).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::ENOENT));
        // A directory named cgroup.procs cannot be opened O_WRONLY: EISDIR.
        std::fs::create_dir(dir.path().join("cgroup.procs")).unwrap();
        let error = place_into_cgroup(cgroup_fd).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::EISDIR));
        nix::unistd::close(cgroup_fd).unwrap();
        // An invalid directory descriptor fails at openat, never at write.
        let error = place_into_cgroup(-1).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::EBADF));
    }
}
