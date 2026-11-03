use super::{ProcessRuntime, RuntimeError};
use crate::cgroups::{place_into_cgroup, ProcessType};
use crate::proto::process as pb;
use nix::unistd::{getegid, geteuid, getgroups, Gid, Uid};
use std::io;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Go's `*fs.PathError` over a preserved errno: the Display carries the full
/// two-layer text (`open /dev/ptmx: too many open files`), while `source()`
/// keeps the original os error — StartErrorCode-style classification
/// (start_error.go:24-31, by errno) still sees EMFILE/ENFILE/... through the
/// wrap, exactly as Go's `errors.Is` walks `PathError.Unwrap`.
#[derive(Debug)]
pub(super) struct PathTextError {
    text: String,
    source: io::Error,
}

impl std::fmt::Display for PathTextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for PathTextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The Go os-failure text for `op path: errno` (os.PathError's Display shape
/// for open/fork-exec sites), as an io::Error that keeps the errno. Re-exported
/// crate-wide for the handover blob writer's refusal text.
pub(crate) fn path_text_error(op: &str, path: &str, error: io::Error) -> io::Error {
    io::Error::other(PathTextError {
        text: format!("{op} {path}: {}", crate::errno_text::go_text(&error)),
        source: error,
    })
}

/// The errno an io::Error carries, including through [`PathTextError`]-style
/// text wrappers whose source chain keeps the original os error.
pub(super) fn raw_errno(error: &io::Error) -> Option<i32> {
    if let Some(raw) = error.raw_os_error() {
        return Some(raw);
    }
    let mut current: &dyn std::error::Error = error.get_ref()?;
    loop {
        if let Some(io_error) = current.downcast_ref::<io::Error>() {
            if let Some(raw) = io_error.raw_os_error() {
                return Some(raw);
            }
        }
        current = current.source()?;
    }
}

pub(super) fn error(error: &io::Error) -> RuntimeError {
    // Oracle handler/start_error.go:24-31 — EAGAIN: fork hit RLIMIT_NPROC /
    // pids.max; EMFILE/ENFILE: fd limits while wiring the child's pipes;
    // ENOSPC: out of ptys (/dev/ptmx, kernel.pty.max). All are transient
    // exhaustion mapping to CodeResourceExhausted, never CodeInvalidArgument;
    // anything unrecognized stays InvalidArgument (start_error.go:37-39).
    match raw_errno(error) {
        Some(11 | 12 | 23 | 24 | 28) => RuntimeError::ResourceExhausted(error.to_string()),
        _ => RuntimeError::InvalidArgument(error.to_string()),
    }
}

pub(super) fn command(
    runtime: &ProcessRuntime,
    request: &pb::StartRequest,
    process_type: ProcessType,
    cwd: &str,
) -> Result<Command, RuntimeError> {
    let config = &*request.process;
    let environment = runtime.environment.for_process(
        &runtime.account,
        runtime.daemon_path.as_deref(),
        &config
            .envs
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let mut command = Command::new("/bin/sh");
    let wrapper = wrapper(runtime.daemon_path.as_deref());
    command
        .args(["-c", &wrapper, "--", &config.cmd])
        .args(&config.args)
        .current_dir(cwd)
        .env_clear()
        .envs(environment)
        .stdin(if request.stdin.unwrap_or(true) {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let account = &runtime.account;
    let mut desired = account.groups.clone();
    desired.sort_unstable();
    desired.dedup();
    let mut current = getgroups()
        .map_err(|error| RuntimeError::Internal(error.to_string()))?
        .into_iter()
        .map(|gid| gid.as_raw())
        .collect::<Vec<_>>();
    current.sort_unstable();
    current.dedup();
    // Oracle handler.go:231-242: the manager's directory fd is what places
    // the child into its process-type cgroup (SysProcAttr.UseCgroupFD via
    // cgroupfd_linux.go), so a started process is never observable outside
    // the cgroup its admission belongs to.
    let cgroup_fd = runtime
        .freezer
        .as_ref()
        .and_then(|freezer| freezer.manager().get_file_descriptor(process_type));
    // An ordinary user may retain exactly its current groups. Changing them is
    // delegated to setgroups in the child and fails rather than silently ignoring it.
    let groups =
        (desired != current).then(|| desired.into_iter().map(Gid::from_raw).collect::<Vec<_>>());
    let gid = (account.gid != getegid().as_raw()).then_some(Gid::from_raw(account.gid));
    let uid = (account.uid != geteuid().as_raw()).then_some(Uid::from_raw(account.uid));
    if cgroup_fd.is_some() || groups.is_some() || gid.is_some() || uid.is_some() {
        // SAFETY: the closure runs between fork and exec in the child. It
        // calls openat/write/close on the inherited cgroup descriptor and
        // setgroups/setgid/setuid over preallocated storage only — all
        // async-signal-safe; no allocator, locks or user code. Placement
        // precedes the credential drop, mirroring Go's CLONE_INTO_CGROUP at
        // the clone followed by the child's Credential calls
        // (handler.go:231-242).
        unsafe {
            command.pre_exec(move || {
                if let Some(fd) = cgroup_fd {
                    place_into_cgroup(fd)?;
                }
                if let Some(groups) = groups.as_deref() {
                    nix::unistd::setgroups(groups).map_err(io::Error::from)?;
                }
                if let Some(gid) = gid {
                    nix::unistd::setgid(gid).map_err(io::Error::from)?;
                }
                if let Some(uid) = uid {
                    nix::unistd::setuid(uid).map_err(io::Error::from)?;
                }
                Ok(())
            });
        }
    }
    Ok(command)
}

fn wrapper(path: Option<&str>) -> String {
    let find = |name: &str| {
        path.and_then(|path| {
            std::env::split_paths(path)
                .map(|directory| directory.join(name))
                .find(|file| executable(file))
        })
    };
    let mut prefix = String::new();
    if let Some(path) = find("ionice") {
        prefix.push_str(&format!(
            "{} -c 2 -n 4 ",
            shell_quote(&path.to_string_lossy())
        ));
    }
    if let Some(path) = find("nice") {
        let priority = current_nice();
        prefix.push_str(&format!(
            "{} -n {} ",
            shell_quote(&path.to_string_lossy()),
            -priority
        ));
    }
    format!("echo 100 > /proc/$$/oom_score_adj && exec {prefix}\"${{@}}\"")
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn current_nice() -> i32 {
    // SAFETY: getpriority takes scalar arguments and has no memory ownership contract.
    unsafe { nix::libc::getpriority(nix::libc::PRIO_PROCESS, 0) }
}

#[cfg(test)]
mod errno_tests {
    use super::{error, io, RuntimeError};

    /// Oracle handler/start_error.go:24-31: EAGAIN (fork hit RLIMIT_NPROC /
    /// pids.max), EMFILE/ENFILE (fd limits while wiring the child's pipes) and
    /// ENOSPC (out of ptys, /dev/ptmx kernel.pty.max) are all transient
    /// exhaustion, so StartErrorCode sends every one of them to
    /// CodeResourceExhausted, never CodeInvalidArgument.
    #[test]
    fn every_oracle_resource_exhaustion_errno_maps_to_resource_exhausted() {
        // 11 EAGAIN, 12 ENOMEM, 23 ENFILE, 24 EMFILE, 28 ENOSPC.
        for errno in [11, 12, 23, 24, 28] {
            let mapped = error(&io::Error::from_raw_os_error(errno));
            assert!(
                matches!(mapped, RuntimeError::ResourceExhausted(_)),
                "errno {errno} must map to ResourceExhausted, got {mapped:?}"
            );
        }
    }

    /// The oracle default stays CodeInvalidArgument
    /// (start_error_test.go:88-97: ENOENT cwd, EACCES spawn).
    #[test]
    fn ordinary_errnos_stay_invalid_argument_like_the_oracle_default() {
        // 2 ENOENT, 13 EACCES, 20 ENOTDIR.
        for errno in [2, 13, 20] {
            let mapped = error(&io::Error::from_raw_os_error(errno));
            assert!(
                matches!(mapped, RuntimeError::InvalidArgument(_)),
                "errno {errno} must stay InvalidArgument, got {mapped:?}"
            );
        }
    }
}
