//! Guest rootfs freeze (FIFREEZE/FITHAW) — a port of the oracle's
//! services/fsfreeze/fsfreeze_linux.go and api/fsfreeze.go at
//! 51083a83cc0903185d992725ea6bb33683814b29. The daemon serves /fsfreeze and
//! /fsthaw through the [`FsFreezer`] seam so tests script the outcome instead
//! of freezing a real host mount.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

/// The guest root filesystem — the only persisted filesystem in a
/// filesystem-only snapshot (oracle fsfreeze.go rootfsMountpoint).
pub const ROOTFS_MOUNTPOINT: &str = "/";

/// FIFREEZE / FITHAW ioctl request codes — _IOWR('X', 119/120, int). Not
/// exported by libc; stable on the architectures envd runs on (x86_64, arm64),
/// which use the generic ioctl encoding. Both ioctls ignore their argument
/// (oracle fsfreeze_linux.go:17-24).
const FIFREEZE: nix::libc::c_ulong = 0xC0045877;
const FITHAW: nix::libc::c_ulong = 0xC0045878;

/// The failure text carries the operation and mountpoint exactly as Go's
/// fmt.Errorf("FIFREEZE %s: %w") wrap does, because the handler's jsonError
/// body is the error string verbatim.
pub trait FsFreezer: Send + Sync {
    fn freeze(&self, mountpoint: &str) -> io::Result<()>;
    fn thaw(&self, mountpoint: &str) -> io::Result<()>;
}

/// Go syscall errno text ("operation not supported"): the shared Go-string
/// translation — nix's desc table diverges from Go for 23 values, and this
/// handler's ramfs refusal is one of them.
fn go_errno_text(error: &io::Error) -> String {
    crate::errno_text::go_text(error)
}

fn ioctl_mount(
    mountpoint: &str,
    request: nix::libc::c_ulong,
    op: &str,
    success_errno: i32,
) -> io::Result<()> {
    // os.Open: read-only is enough for both ioctls. Go's os.Open failure is a
    // PathError reading "open %s: <errno>", which the oracle wraps AGAIN with
    // fmt.Errorf("open %s: %w") (fsfreeze_linux.go:31-34/:55-58) — the double
    // prefix is the oracle's actual bytes on the wire.
    let file = File::open(mountpoint).map_err(|error| {
        io::Error::other(format!(
            "open {mountpoint}: open {mountpoint}: {}",
            go_errno_text(&error)
        ))
    })?;
    // SAFETY: the fd is valid for the call, and FIFREEZE/FITHAW ignore the
    // ioctl argument, so passing 0 is safe (oracle fsfreeze_linux.go:19-24).
    // glibc declares the request as c_ulong; musl as c_int. The code
    // 0xC0045877 fits both, so the musl target narrows the binding.
    #[cfg(target_env = "musl")]
    let request = request as nix::libc::c_int;
    let outcome = unsafe { nix::libc::ioctl(file.as_raw_fd(), request, 0) };
    if outcome == 0 {
        return Ok(());
    }
    let raw = io::Error::last_os_error();
    settle_ioctl(op, mountpoint, raw, success_errno)
}

/// Settles a failed FIFREEZE/FITHAW ioctl with the oracle's asymmetric
/// idempotence: FIFREEZE succeeds only on EBUSY (the filesystem is already
/// frozen) and FITHAW only on EINVAL (it is not frozen)
/// (fsfreeze_linux.go:36-43, :56-63). Each operation passes its own errno as
/// `success_errno`; the wrong-pair errno is a real refusal the handler turns
/// into a 500, wrapped like Go's fmt.Errorf("%s %s: %w").
fn settle_ioctl(
    op: &str,
    mountpoint: &str,
    error: io::Error,
    success_errno: i32,
) -> io::Result<()> {
    match error.raw_os_error() {
        Some(code) if code == success_errno => Ok(()),
        _ => Err(io::Error::other(format!(
            "{op} {mountpoint}: {}",
            go_errno_text(&error)
        ))),
    }
}

/// The FIFREEZE/FITHAW-backed freezer used in the guest.
pub struct LinuxFsFreezer;

impl FsFreezer for LinuxFsFreezer {
    fn freeze(&self, mountpoint: &str) -> io::Result<()> {
        ioctl_mount(mountpoint, FIFREEZE, "FIFREEZE", nix::libc::EBUSY)
    }

    fn thaw(&self, mountpoint: &str) -> io::Result<()> {
        ioctl_mount(mountpoint, FITHAW, "FITHAW", nix::libc::EINVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_failure_wraps_like_go() {
        // Oracle fsfreeze_linux.go:30-33/:51-53: os.Open's PathError already
        // reads "open %s: <errno>", and the handler wraps it AGAIN with
        // "open %s: %w" — the double prefix is Go's actual bytes.
        let error = LinuxFsFreezer
            .freeze("/nonexistent-envd-test-mount")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "open /nonexistent-envd-test-mount: \
             open /nonexistent-envd-test-mount: no such file or directory"
        );
        // The daemon stores the freezer behind a trait object; the trait must
        // stay object-safe.
        let boxed: std::sync::Arc<dyn FsFreezer> = std::sync::Arc::new(LinuxFsFreezer);
        let error = boxed.freeze("/nonexistent-envd-test-mount").unwrap_err();
        assert!(error
            .to_string()
            .starts_with("open /nonexistent-envd-test-mount: open "));
    }

    #[test]
    fn errno_text_matches_go_strings_not_nix_descriptions() {
        // The oracle's message is Go's `syscall.Errno.Error()` text; nix's
        // desc table diverges for 23 values — EOPNOTSUPP among them, the one
        // a ramfs rootfs freeze actually produces (Go: "operation not
        // supported"; nix: "Operation not supported on transport endpoint").
        let eopnotsupp = io::Error::from_raw_os_error(nix::libc::EOPNOTSUPP);
        assert_eq!(go_errno_text(&eopnotsupp), "operation not supported");
        // Rename across filesystems surfaces EXDEV with Go's own prefix.
        let exdev = io::Error::from_raw_os_error(nix::libc::EXDEV);
        assert_eq!(go_errno_text(&exdev), "invalid cross-device link");
        // ELOOP: Go "too many levels of symbolic links".
        let eloop = io::Error::from_raw_os_error(nix::libc::ELOOP);
        assert_eq!(go_errno_text(&eloop), "too many levels of symbolic links");
    }

    #[test]
    fn freeze_and_thaw_succeed_only_on_the_oracle_asymmetric_errnos() {
        // Oracle fsfreeze_linux.go:36-43 — FIFREEZE treats only EBUSY (the
        // filesystem is already frozen) as success; :56-63 — FITHAW treats
        // only EINVAL (it is not frozen) as success. The wrong-pair errno is
        // a real refusal the handler reports as 500.
        let freeze_ebusy = settle_ioctl(
            "FIFREEZE",
            "/",
            io::Error::from_raw_os_error(nix::libc::EBUSY),
            nix::libc::EBUSY,
        );
        assert!(freeze_ebusy.is_ok());
        let freeze_einval = settle_ioctl(
            "FIFREEZE",
            "/",
            io::Error::from_raw_os_error(nix::libc::EINVAL),
            nix::libc::EBUSY,
        )
        .unwrap_err();
        assert_eq!(freeze_einval.to_string(), "FIFREEZE /: invalid argument");
        let thaw_einval = settle_ioctl(
            "FITHAW",
            "/",
            io::Error::from_raw_os_error(nix::libc::EINVAL),
            nix::libc::EINVAL,
        );
        assert!(thaw_einval.is_ok());
        let thaw_ebusy = settle_ioctl(
            "FITHAW",
            "/",
            io::Error::from_raw_os_error(nix::libc::EBUSY),
            nix::libc::EINVAL,
        )
        .unwrap_err();
        assert_eq!(thaw_ebusy.to_string(), "FITHAW /: device or resource busy");
    }
}
