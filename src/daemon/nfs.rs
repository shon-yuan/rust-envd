//! NFS volume mounts, ported from the pinned oracle
//! `internal/api/nfs_mount.go`, `nfs_mount_linux.go`, `mounts_handover.go`
//! and the setupNFS arm of `internal/api/init.go:796-1007` at
//! 51083a83cc0903185d992725ea6bb33683814b29.
//!
//! The oracle never calls mount(2) for the mount itself: it shells out to the
//! guest's PATH-resolved `mount`(8) (which dispatches to nfs-utils'
//! mount.nfs) and `mkdir`(1) from `nfs_mount.go:32-48`, in a fresh process
//! group killed with SIGKILL when the bounding context ends
//! (`nfs_mount.go:50-69`). Only unmount and the mount-ID probe are syscalls
//! (`nfs_mount_linux.go`). The mount-ID probe is what makes success
//! observable without a server round trip: a mount only counts when the
//! path's statx STATX_MNT_ID changed.
//!
//! [`NfsMounter`] is the seam the oracle's `nfsMounter` interface
//! (`nfs_mount.go:12-21`) is: tests replace it; production wires
//! [`ExecNfsMounter`]. Where Go threads a context through the method, the
//! Rust seam takes the context's deadline directly — the executor races the
//! command against it exactly like `exec.CommandContext` does, so a timed-out
//! attempt surfaces the exec-shaped error Go's `CombinedOutput` produces
//! ("signal: killed") rather than a scheduler-level timeout.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

/// Oracle nfs_mount.go nfsMounter: futures bound to the mounter's borrow, the
/// shape mmds::TokenHashProvider uses.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Oracle init.go:749-770 nfsOptions — every option is a compile-time
/// constant; the only request-derived mount inputs are the target and path.
pub const NFS_OPTIONS: &str = "sync,rsize=1048576,wsize=1048576,mountproto=tcp,mountport=2049,\
proto=tcp,port=2049,nfsvers=3,noacl,noac,lookupcache=none,retry=0";

/// Oracle init.go:772-774: bounds one mount command; starts when it starts,
/// since volumes are mounted one at a time.
pub const NFS_MOUNT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Oracle init.go:776-777: how many times a mount that times out is tried.
pub const NFS_MOUNT_ATTEMPTS: u32 = 2;

/// Oracle init.go:779-780: bounds one volume's mount point plus all attempts
/// (nfsVolumeBudget = nfsMountAttempts * nfsMountTimeout = 20s; written out
/// because const trait arithmetic is not stable).
pub const NFS_VOLUME_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// Oracle init.go:782-783: the pause after a mount that failed without timing
/// out.
pub const NFS_MOUNT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Oracle store.go:74-76 nfsMountRetryFor, defaulting to nfsMountTimeout
/// (store.go:178): how long an immediately-failing mount is retried.
pub const NFS_MOUNT_RETRY_FOR: std::time::Duration = NFS_MOUNT_TIMEOUT;

/// Oracle nfs_mount.go:25-26: caps the mount output kept in an error.
pub const MAX_MOUNT_OUTPUT_BYTES: usize = 512;

/// Oracle nfs_mount.go:28-30: bounds how long a canceled command may keep its
/// output pipes open before the wait gives up on them.
pub const NFS_CMD_WAIT_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Go os/exec ErrWaitDelay (exec.go): what Wait reports when the WaitDelay
/// expires before the command's pipes close.
const GO_WAIT_DELAY_TEXT: &str = "exec: WaitDelay expired before I/O complete";

/// Go context.Err text for an expired deadline; what CombinedOutput reports
/// when a command exits successfully after its context killed it
/// (os/exec Wait: the context error is returned unless the exit is non-zero).
const GO_DEADLINE_EXCEEDED_TEXT: &str = "context deadline exceeded";

/// Oracle api.gen.go:245-252 VolumeMount: two plain non-omitempty strings.
/// Go decodes JSON null for a string field as its zero value and a null ARRAY
/// element as the zero struct, both without error; a wrong JSON type fails
/// the whole-body decode. Field names fold ASCII case like encoding/json.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeMount {
    pub nfs_target: String,
    pub path: String,
}

impl<'de> serde::Deserialize<'de> for VolumeMount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct VolumeMountVisitor;
        impl<'de> serde::de::Visitor<'de> for VolumeMountVisitor {
            type Value = VolumeMount;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a volume mount object or null")
            }
            // A null array element decodes as the zero struct, like Go.
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(VolumeMount::default())
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut mount = VolumeMount::default();
                while let Some(key) = map.next_key::<String>()? {
                    match folded_field(&key).as_str() {
                        "NFS_TARGET" => {
                            mount.nfs_target =
                                map.next_value::<Option<String>>()?.unwrap_or_default();
                        }
                        "PATH" => {
                            mount.path = map.next_value::<Option<String>>()?.unwrap_or_default();
                        }
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(mount)
            }
        }
        deserializer.deserialize_any(VolumeMountVisitor)
    }
}

/// Go encoding/json folds ASCII case (plus the Unicode simple folds the
/// daemon's other decoded fields already share).
fn folded_field(key: &str) -> String {
    key.chars()
        .map(|ch| match ch {
            'ſ' => 'S',
            'K' => 'K',
            ch => ch.to_ascii_uppercase(),
        })
        .collect()
}

/// One result of a `mount`/`mkdir` exec, the shape CombinedOutput hands the
/// oracle: an error text (already Go-shaped and exec-wrapped) plus whether
/// that error IS a context-deadline error (init.go:928's errors.Is arm; the
/// other timeout arm, ctx.Err() != nil, the caller measures on its own clock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub error: Option<String>,
    pub deadline_error: bool,
}

impl CommandOutcome {
    pub fn ok() -> Self {
        Self {
            error: None,
            deadline_error: false,
        }
    }
    pub fn failure(text: impl Into<String>) -> Self {
        Self {
            error: Some(text.into()),
            deadline_error: false,
        }
    }
}

/// Oracle nfs_mount.go:12-21 nfsMounter. `deadline` is the caller's context
/// deadline: the executor kills the process group when it passes and shapes
/// the error the way Go's exec does (nfs_mount.go:54-69).
pub trait NfsMounter: Send + Sync {
    /// nfs_mount.go:32-40.
    fn mount(
        &self,
        nfs_target: &str,
        path: &str,
        deadline: tokio::time::Instant,
    ) -> BoxFut<'_, CommandOutcome>;
    /// nfs_mount.go:42-48.
    fn make_dir(&self, path: &str, deadline: tokio::time::Instant) -> BoxFut<'_, CommandOutcome>;
    /// nfs_mount_linux.go:9-18: detaches whatever is mounted at path; EINVAL
    /// and ENOENT are not-a-mount, not errors.
    fn unmount(&self, path: &str) -> BoxFut<'_, Result<(), String>>;
    /// nfs_mount_linux.go:20-31: the ID of the mount path resolves to.
    fn mount_id(&self, path: &str) -> BoxFut<'_, Result<u64, String>>;
}

/// The compile-time argv builders, shared by the real executor and tests so
/// the byte shape has exactly one definition (nfs_mount.go:33, :43).
pub fn mount_command(nfs_target: &str, path: &str) -> Vec<String> {
    vec![
        "-v".to_owned(),
        "-t".to_owned(),
        "nfs".to_owned(),
        "-o".to_owned(),
        format!("fg,hard,{NFS_OPTIONS}"),
        nfs_target.to_owned(),
        path.to_owned(),
    ]
}

pub fn make_dir_command(path: &str) -> Vec<String> {
    vec!["-p".to_owned(), path.to_owned()]
}

/// Oracle nfs_mount.go:36/:44: `` `<program> <args joined>` failed: <err>: <output> ``.
pub fn exec_failure_text(program: &str, args: &[String], error: &str, output: &str) -> String {
    format!("`{program} {}` failed: {error}: {output}", args.join(" "))
}

/// Oracle nfs_mount.go:66-68: the combined output, capped at
/// MAX_MOUNT_OUTPUT_BYTES BEFORE trimming (data[:min], then TrimSpace).
fn capped_trimmed_output(data: &[u8]) -> String {
    let end = data.len().min(MAX_MOUNT_OUTPUT_BYTES);
    String::from_utf8_lossy(&data[..end]).trim().to_owned()
}

/// Go os.ProcessState.String() for the CombinedOutput error column
/// (process/supervise.rs renders the same shape for the process service).
fn exit_status_text(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit status {code}"),
        (None, Some(signal)) => {
            format!("signal: {}", crate::errno_text::signal_description(signal))
        }
        (None, None) => "exit status -1".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// The real executor
// ---------------------------------------------------------------------------

/// Oracle store.go:177 execNFSMounter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExecNfsMounter;

impl NfsMounter for ExecNfsMounter {
    fn mount(
        &self,
        nfs_target: &str,
        path: &str,
        deadline: tokio::time::Instant,
    ) -> BoxFut<'_, CommandOutcome> {
        let args = mount_command(nfs_target, path);
        Box::pin(run_process_group("mount", args, deadline))
    }

    fn make_dir(&self, path: &str, deadline: tokio::time::Instant) -> BoxFut<'_, CommandOutcome> {
        let args = make_dir_command(path);
        Box::pin(run_process_group("mkdir", args, deadline))
    }

    fn unmount(&self, path: &str) -> BoxFut<'_, Result<(), String>> {
        let path = path.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let Ok(target) = std::ffi::CString::new(path) else {
                    return Err("invalid argument".to_owned());
                };
                // SAFETY: umount2 with a valid NUL-terminated path pointer.
                let rc =
                    unsafe { libc::umount2(target.as_ptr(), libc::MNT_FORCE | libc::MNT_DETACH) };
                if rc == 0 {
                    return Ok(());
                }
                let error = std::io::Error::last_os_error();
                // nfs_mount_linux.go:13-15: not a mount point is success.
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EINVAL) | Some(libc::ENOENT)
                ) {
                    return Ok(());
                }
                Err(crate::errno_text::go_text(&error))
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()))
        })
    }

    fn mount_id(&self, path: &str) -> BoxFut<'_, Result<u64, String>> {
        let path = path.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let Ok(target) = std::ffi::CString::new(path) else {
                    // unix.BytePtrFromString's EINVAL for an interior NUL.
                    return Err("invalid argument".to_owned());
                };
                let mut buffer: libc::statx = unsafe { std::mem::zeroed() };
                // SAFETY: statx with a valid path pointer and an owned buffer.
                // AT_STATX_DONT_SYNC asks for no attribute the NFS client
                // would fetch from the server (nfs_mount_linux.go:22-23).
                let rc = unsafe {
                    libc::statx(
                        libc::AT_FDCWD,
                        target.as_ptr(),
                        libc::AT_STATX_DONT_SYNC,
                        libc::STATX_MNT_ID,
                        &mut buffer,
                    )
                };
                if rc != 0 {
                    return Err(crate::errno_text::go_text(&std::io::Error::last_os_error()));
                }
                if buffer.stx_mask & libc::STATX_MNT_ID == 0 {
                    // nfs_mount_linux.go:26-28.
                    return Err("the kernel does not report mount IDs".to_owned());
                }
                Ok(buffer.stx_mnt_id)
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|inner| inner)
        })
    }
}

/// The spawned command plus the reader thread's completion channel. The
/// thread reads the shared pipe to EOF and then reaps the child, exactly the
/// two things Go's CombinedOutput waits for.
struct GroupCommand {
    child: std::sync::Arc<Mutex<std::process::Child>>,
    done: std::sync::mpsc::Receiver<(Vec<u8>, std::io::Result<std::process::ExitStatus>)>,
}

/// nfs_mount.go:54-64: spawn in a fresh process group with stdout and stderr
/// on ONE pipe (CombinedOutput semantics), so group-wide SIGKILL reaches the
/// forked mount.nfs too and the output interleaves as the child wrote it.
fn spawn_group_command(name: &str, args: &[String]) -> std::io::Result<GroupCommand> {
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let (reader, writer) = UnixStream::pair()?;
    // SAFETY: each fd is consumed into exactly one child stdio handle and
    // never used again on this side.
    let stdout = unsafe { Stdio::from_raw_fd(writer.try_clone()?.into_raw_fd()) };
    let mut command = std::process::Command::new(name);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(unsafe { Stdio::from_raw_fd(writer.into_raw_fd()) })
        // nfs_mount.go:56 Setpgid: the child leads its own process group.
        .process_group(0);
    let child = command.spawn()?;

    let child = std::sync::Arc::new(Mutex::new(child));
    let reaper = child.clone();
    let (done_tx, done) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("envd-nfs-command".to_owned())
        .spawn(move || {
            let mut output = Vec::new();
            let _ = std::io::Read::read_to_end(&mut { reader }, &mut output);
            let status = reaper.lock().unwrap().wait();
            let _ = done_tx.send((output, status));
        })?;
    Ok(GroupCommand { child, done })
}

/// nfs_mount.go:57-63: kill the WHOLE process group, so the forked mount.nfs
/// cannot finish the mount after the timeout. A no-op once the group is gone.
fn kill_process_group(child: &Mutex<std::process::Child>) {
    let guard = child.lock().unwrap();
    // SAFETY: kill(2) on the negative pid (the process group).
    let _ = unsafe { libc::kill(-(guard.id() as libc::pid_t), libc::SIGKILL) };
}

/// nfs_mount.go:54-69 runProcessGroup: run with the group kill on the
/// deadline, bound the post-kill pipe wait at WaitDelay, and produce the
/// CombinedOutput-shaped outcome.
async fn run_process_group(
    name: &str,
    args: Vec<String>,
    deadline: tokio::time::Instant,
) -> CommandOutcome {
    let spawned = match spawn_group_command(name, &args) {
        Ok(spawned) => spawned,
        // Go resolves the executable at exec.Command time (LookPath), so a
        // missing helper fails before any process exists — init.go:942-961
        // retries it like any non-timeout failure. NotFound carries Go's
        // LookPath text; every other spawn failure carries the errno text.
        Err(error) => {
            let text = if error.kind() == std::io::ErrorKind::NotFound {
                format!("exec: {name:?}: executable file not found in $PATH")
            } else {
                crate::errno_text::go_text(&error)
            };
            return CommandOutcome::failure(exec_failure_text(name, &args, &text, ""));
        }
    };
    let GroupCommand { child, done } = spawned;
    let mut done = tokio::task::spawn_blocking(move || done.recv());

    let sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(sleep);
    let mut context_done = false;
    let received = tokio::select! {
        _ = &mut sleep => {
            context_done = true;
            kill_process_group(&child);
            // nfs_mount.go:64 WaitDelay: stop waiting on the pipes 2s after
            // the kill.
            match tokio::time::timeout(NFS_CMD_WAIT_DELAY, &mut done).await {
                Ok(Ok(Ok(received))) => Some(received),
                // The reader panicked, or a grandchild outside the group is
                // still holding the pipe past the delay (ErrWaitDelay).
                _ => None,
            }
        }
        joined = &mut done => match joined {
            Ok(Ok(received)) => Some(received),
            _ => None,
        },
    };

    let Some((output, status)) = received else {
        // Either the WaitDelay elapsed after the group kill with a grandchild
        // outside the group still holding the pipe (Go's ErrWaitDelay), or
        // the reader thread panicked. Both read as failures; the WaitDelay
        // one is a deadline error like its Go counterpart.
        let text = if context_done {
            GO_WAIT_DELAY_TEXT
        } else {
            "command wait failed"
        };
        return CommandOutcome {
            error: Some(exec_failure_text(name, &args, text, "")),
            deadline_error: context_done,
        };
    };

    let text = capped_trimmed_output(&output);
    match status {
        Ok(status) if status.success() => {
            if context_done {
                // os/exec Wait: a command that exits ZERO after its context
                // killed it reports the context error, so the attempt still
                // fails and still classifies as a deadline error.
                CommandOutcome {
                    error: Some(exec_failure_text(
                        name,
                        &args,
                        GO_DEADLINE_EXCEEDED_TEXT,
                        &text,
                    )),
                    deadline_error: true,
                }
            } else {
                CommandOutcome::ok()
            }
        }
        Ok(status) => CommandOutcome {
            error: Some(exec_failure_text(
                name,
                &args,
                &exit_status_text(&status),
                &text,
            )),
            // init.go:945-961 classifies by the CONTEXT, not by how the
            // command died: a command our deadline killed reports "signal:
            // killed" and must still read as a deadline error.
            deadline_error: context_done,
        },
        Err(error) => CommandOutcome {
            error: Some(exec_failure_text(
                name,
                &args,
                &crate::errno_text::go_text(&error),
                &text,
            )),
            deadline_error: false,
        },
    }
}

// ---------------------------------------------------------------------------
// Path arithmetic (Go filepath ports used by the ordering rules)
// ---------------------------------------------------------------------------

/// Go filepath.Clean (unix), the lexical normalizer pathDepth and
/// resolveExisting are defined against. The first two branches keep Go's
/// case structure on purpose (the "." case reads as its own case in
/// paths.go), so the identical bodies stay.
#[allow(clippy::if_same_then_else)]
pub fn go_clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let bytes = path.as_bytes();
    let rooted = bytes[0] == b'/';
    let n = bytes.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let (mut r, mut dotdot);
    if rooted {
        out.push(b'/');
        r = 1;
        dotdot = 1;
    } else {
        r = 0;
        dotdot = 0;
    }
    while r < n {
        if bytes[r] == b'/' {
            r += 1;
        } else if bytes[r] == b'.' && (r + 1 == n || bytes[r + 1] == b'/') {
            r += 1;
        } else if bytes[r] == b'.' && bytes[r + 1] == b'.' && (r + 2 == n || bytes[r + 2] == b'/') {
            r += 2;
            if out.len() > dotdot {
                out.pop();
                while out.len() > dotdot && *out.last().expect("non-empty after pop") != b'/' {
                    out.pop();
                }
                // Go's out is an exclusive index pointing AT the separator it
                // stopped on; the final string slice never includes it. The
                // next component re-adds the separator.
                if out.len() > dotdot {
                    out.pop();
                }
            } else if !rooted {
                if !out.is_empty() {
                    out.push(b'/');
                }
                out.extend_from_slice(b"..");
                dotdot = out.len();
            }
        } else {
            if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
                out.push(b'/');
            }
            while r < n && bytes[r] != b'/' {
                out.push(bytes[r]);
                r += 1;
            }
        }
    }
    if out.is_empty() {
        out.push(b'.');
    }
    String::from_utf8(out).expect("Clean only rearranges UTF-8 bytes")
}

/// Go filepath.Join for two elements: empty elements are ignored, the result
/// is Cleaned, and two empty elements yield "".
fn go_join2(a: &str, b: &str) -> String {
    let mut joined = String::new();
    for part in [a, b] {
        if part.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(part);
        } else {
            joined.push('/');
            joined.push_str(part);
        }
    }
    if joined.is_empty() {
        return joined;
    }
    go_clean(&joined)
}

/// Go filepath.Base (unix).
fn go_base(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let stripped = path.trim_end_matches('/');
    if stripped.is_empty() {
        return "/".to_owned();
    }
    match stripped.rfind('/') {
        Some(index) => stripped[index + 1..].to_owned(),
        None => stripped.to_owned(),
    }
}

/// Go filepath.Dir (unix): everything up to the last separator, cleaned.
fn go_dir(path: &str) -> String {
    let cut = path.rfind('/').map_or(0, |index| index + 1);
    go_clean(&path[..cut])
}

/// Oracle init.go:988-990 pathDepth: the slash count of the CLEANED path.
pub fn path_depth(path: &str) -> i64 {
    go_clean(path).matches('/').count() as i64
}

/// Go filepath.EvalSymlinks (walkSymlinks, symlink.go:26-138): resolves
/// symlinks component by component with Lstat, keeps "." and ".." LEXICAL in
/// dest (the trailing Clean folds them), bounds followed links at 255, and
/// errors when a component does not exist (the final one included) or an
/// intermediate one is not a directory. Go's loop post-statement `start = end`
/// runs on every continue; the same advance is spelled out here.
fn walk_symlinks(path: &str) -> Result<String, std::io::Error> {
    let mut path = path.to_owned();
    // Go consumes a leading separator into the volume: unix vol is "/" for a
    // rooted path (volLen 1) and "" otherwise (symlink.go:19-25).
    let mut vol_len = usize::from(path.starts_with('/'));
    let mut dest = path[..vol_len].to_owned();
    let mut links_walked = 0usize;
    let mut start = vol_len;
    let mut end;
    loop {
        let bytes = path.as_bytes();
        let n = path.len();
        if start >= n {
            break;
        }
        while start < n && bytes[start] == b'/' {
            start += 1;
        }
        end = start;
        while end < n && bytes[end] != b'/' {
            end += 1;
        }
        if end == start {
            // No more path components.
            break;
        }
        let component = &path[start..end];
        if component == "." {
            start = end;
            continue;
        }
        if component == ".." {
            // Back up to the previous component if possible. r is the index
            // of the last separator in dest at or after vol_len, ending below
            // vol_len when none exists (symlink.go:56-77).
            let mut r: i64 = dest.len() as i64 - 1;
            while r >= vol_len as i64 {
                if dest.as_bytes()[r as usize] == b'/' {
                    break;
                }
                r -= 1;
            }
            let tail = if r >= vol_len as i64 {
                &dest[(r as usize + 1)..]
            } else {
                ""
            };
            if r < vol_len as i64 || tail == ".." {
                // Nothing to pop (empty, or only the root), or the tail is
                // itself a kept "..": keep this one lexically.
                if dest.len() > vol_len {
                    dest.push('/');
                }
                dest.push_str("..");
            } else {
                // Discard everything since the last slash.
                dest.truncate(r as usize);
            }
            start = end;
            continue;
        }
        // Ordinary path component: add it to the result.
        if dest.len() > vol_len && !dest.ends_with('/') {
            dest.push('/');
        }
        dest.push_str(component);

        let meta = std::fs::symlink_metadata(&dest)?;
        if !meta.file_type().is_symlink() {
            if !meta.is_dir() && end < n {
                return Err(std::io::Error::from_raw_os_error(libc::ENOTDIR));
            }
            start = end;
            continue;
        }
        links_walked += 1;
        if links_walked > 255 {
            return Err(std::io::Error::other("EvalSymlinks: too many links"));
        }
        let link = std::fs::read_link(&dest)?;
        let link_text = link.to_string_lossy().into_owned();
        let link_path = format!("{link_text}{}", &path[end..]);
        if link_text.starts_with('/') {
            // Symlink to an absolute path: restart dest at the root
            // (symlink.go:117-124).
            dest = "/".to_owned();
            vol_len = 1;
            end = 1;
        } else {
            // Symlink to a relative path: replace the last component of dest
            // (symlink.go:125-136). With no separator at or after the volume,
            // dest resets to the volume itself.
            let mut r: i64 = dest.len() as i64 - 1;
            while r >= vol_len as i64 {
                if dest.as_bytes()[r as usize] == b'/' {
                    break;
                }
                r -= 1;
            }
            if r < vol_len as i64 {
                dest = if vol_len == 1 {
                    "/".to_owned()
                } else {
                    String::new()
                };
            } else {
                dest.truncate(r as usize);
            }
            end = 0;
        }
        path = link_path;
        start = end;
    }
    Ok(go_clean(&dest))
}

/// Oracle init.go:887-900 resolveExisting: resolve symlinks in the longest
/// EXISTING prefix of the cleaned path.
pub fn resolve_existing(path: &str) -> String {
    let path = go_clean(path);
    let mut dir = path.clone();
    let mut rest = String::new();
    loop {
        if let Ok(resolved) = walk_symlinks(&dir) {
            return go_join2(&resolved, &rest);
        }
        let parent = go_dir(&dir);
        if parent == dir {
            return path;
        }
        rest = go_join2(&go_base(&dir), &rest);
        dir = parent;
    }
}

// ---------------------------------------------------------------------------
// The setupNFS orchestration
// ---------------------------------------------------------------------------

/// The memoized failure (init.go:785-789): why mounting last failed, for the
/// lifecycle it failed under. Cloned out on the short-circuit path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfsFailure {
    pub lifecycle_id: String,
    pub volume_error: NfsVolumeError,
}

/// Oracle init.go:866-874 nfsVolumeError: names the volume path that could
/// not be mounted, in Go's %q quoting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfsVolumeError {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for NfsVolumeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "NFS volume at {}: {}",
            crate::goquote::quote(&self.path),
            self.message
        )
    }
}

impl NfsVolumeError {
    /// The raw failing Path, for X-Envd-Failed-Volume (init.go:730-733).
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// The in-memory pieces setupNFS reads and writes (store.go:71-77): the
/// mountedPaths ledger (shared with the live-upgrade handover export) and the
/// memoized failure. The daemon holds initLock across every access; the Mutex
/// only keeps the container accesses themselves serialized.
pub struct NfsBooks<'a> {
    pub mounted: &'a Mutex<HashMap<String, String>>,
    pub failure: &'a Mutex<Option<NfsFailure>>,
}

impl NfsBooks<'_> {
    fn lookup(&self, path: &str) -> Option<String> {
        self.mounted.lock().unwrap().get(path).cloned()
    }
    fn remove(&self, path: &str) {
        self.mounted.lock().unwrap().remove(path);
    }
    fn store(&self, path: String, lifecycle_id: String) {
        self.mounted.lock().unwrap().insert(path, lifecycle_id);
    }
    fn remember(&self, lifecycle_id: &str, volume_error: NfsVolumeError) {
        if !lifecycle_id.is_empty() {
            // init.go:877-884 failNFS.
            *self.failure.lock().unwrap() = Some(NfsFailure {
                lifecycle_id: lifecycle_id.to_owned(),
                volume_error,
            });
        }
    }
    fn memoized(&self, lifecycle_id: &str) -> Option<NfsVolumeError> {
        let failure = self.failure.lock().unwrap();
        failure
            .as_ref()
            .filter(|failure| failure.lifecycle_id == lifecycle_id)
            .map(|failure| failure.volume_error.clone())
    }
}

/// Oracle init.go:992-1007 shouldRemountNFS: mount when the path is not in
/// the ledger or its lifecycle differs (treating nil and "" as equivalent).
fn should_remount_nfs(mounted_lifecycle: Option<&str>, request_lifecycle: &str) -> bool {
    match mounted_lifecycle {
        None => true, // not mounted yet, need to mount
        Some(mounted) => mounted != request_lifecycle,
    }
}

/// Oracle init.go:796-864 setupNFS: mount the volumes one at a time, parents
/// before children, and stop at the first volume that is not mounted
/// afterwards. The caller holds initLock. A failure is remembered for the
/// lifecycle so the caller's retry gets the same error at once instead of a
/// remount.
pub async fn setup_nfs(
    books: &NfsBooks<'_>,
    nfs: &dyn NfsMounter,
    lifecycle_id: &str,
    mounts: &[VolumeMount],
) -> Result<(), NfsVolumeError> {
    // init.go:798-800: a remembered failure for THIS lifecycle answers at
    // once, even for an empty mounts list.
    if let Some(volume_error) = books.memoized(lifecycle_id) {
        return Err(volume_error);
    }

    // init.go:807-810: sort by raw path depth (STABLE — ties keep request
    // order), so a parent mount lands before a child's.
    let mut ordered = mounts.to_vec();
    ordered.sort_by_key(|volume| path_depth(&volume.path));

    // init.go:812-818: only the volumes the ledger says need (re)mounting.
    let mut pending: Vec<VolumeMount> = ordered
        .into_iter()
        .filter(|volume| should_remount_nfs(books.lookup(&volume.path).as_deref(), lifecycle_id))
        .collect();

    // init.go:820-828: detach every stale mount BEFORE mounting any — the
    // guest keeps one NFS client per server, and while an old mount still
    // holds it, a new mount reuses its connection, which is dead after a
    // resume.
    for volume in &pending {
        books.remove(&volume.path);
        if let Err(error) = nfs.unmount(&volume.path).await {
            return Err(fail_nfs(
                books,
                lifecycle_id,
                volume,
                format!("unmount: {error}"),
            ));
        }
    }

    // init.go:830-838: order by where each path really leads — a parent
    // reached through a symlink must still be mounted before the paths
    // beneath it.
    let depths: HashMap<String, i64> = pending
        .iter()
        .map(|volume| {
            (
                volume.path.clone(),
                path_depth(&resolve_existing(&volume.path)),
            )
        })
        .collect();
    pending.sort_by_key(|volume| depths[&volume.path]);

    // init.go:840-847: mount sequentially, recording each path's mount ID.
    let mut mount_ids: HashMap<String, u64> = HashMap::with_capacity(pending.len());
    for volume in &pending {
        match mount_nfs(books, nfs, volume, lifecycle_id).await {
            Ok(id) => {
                mount_ids.insert(volume.path.clone(), id);
            }
            Err(message) => return Err(fail_nfs(books, lifecycle_id, volume, message)),
        }
    }

    // init.go:849-856: a later mount can still cover an earlier one, so
    // check each path once more.
    for volume in &pending {
        let hidden = match nfs.mount_id(&volume.path).await {
            Ok(id) => id != mount_ids[&volume.path],
            // A statx failure reads as covered, like Go's `err != nil ||`.
            Err(_) => true,
        };
        if hidden {
            books.remove(&volume.path);
            return Err(fail_nfs(
                books,
                lifecycle_id,
                volume,
                "hidden by another volume mounted over it".to_owned(),
            ));
        }
    }

    Ok(())
}

/// Oracle init.go:876-884 failNFS: wrap the failure with the volume path and
/// remember it for the lifecycle (only when the lifecycle is non-empty).
fn fail_nfs(
    books: &NfsBooks<'_>,
    lifecycle_id: &str,
    volume: &VolumeMount,
    message: String,
) -> NfsVolumeError {
    let volume_error = NfsVolumeError {
        path: volume.path.clone(),
        message,
    };
    books.remember(lifecycle_id, volume_error.clone());
    volume_error
}

/// Oracle init.go:902-986 mountNFS: create the mount point, run the mount
/// with the timeout/retry matrix, and only on a CHANGED mount ID record the
/// path in the ledger. Returns the verified mount ID.
async fn mount_nfs(
    books: &NfsBooks<'_>,
    nfs: &dyn NfsMounter,
    volume: &VolumeMount,
    lifecycle_id: &str,
) -> Result<u64, String> {
    // init.go:903-905: the mount point and every attempt share one budget per
    // volume.
    let volume_deadline = tokio::time::Instant::now() + NFS_VOLUME_BUDGET;

    // init.go:908-915: a killable process creates the mount point — inside
    // another volume, the lookup waits on that volume's NFS server.
    let dir_deadline = std::cmp::min(
        volume_deadline,
        tokio::time::Instant::now() + NFS_MOUNT_TIMEOUT,
    );
    if let Some(error) = nfs.make_dir(&volume.path, dir_deadline).await.error {
        return Err(format!("create the mount point: {error}"));
    }

    // init.go:917-920.
    let before = nfs.mount_id(&volume.path).await?;

    let start = tokio::time::Instant::now();
    let mut attempts = 0u32;
    let mut timeouts = 0u32;
    // The last attempt's error, when the volume ends up failing (init.go:924-963).
    let last_error: Option<String> = loop {
        attempts += 1;
        // init.go:926: each attempt gets its own 10s context inside the
        // volume budget.
        let attempt_deadline = std::cmp::min(
            volume_deadline,
            tokio::time::Instant::now() + NFS_MOUNT_TIMEOUT,
        );
        let outcome = nfs
            .mount(&volume.nfs_target, &volume.path, attempt_deadline)
            .await;
        // init.go:928: timeout classification is the context expiring OR the
        // error being a deadline error.
        let timed_out = tokio::time::Instant::now() >= attempt_deadline || outcome.deadline_error;
        let Some(error) = outcome.error else {
            break None;
        };
        eprintln!(
            "NFS mount attempt failed: attempt={attempts} timed_out={timed_out} \
             target={} path={} error={error:?}",
            volume.nfs_target, volume.path
        );

        if timed_out {
            // init.go:942-953: under load a single mount can stall, so one
            // that times out is tried once more.
            timeouts += 1;
            if timeouts == NFS_MOUNT_ATTEMPTS || tokio::time::Instant::now() >= volume_deadline {
                break Some(error);
            }
            if let Err(unmount_error) = nfs.unmount(&volume.path).await {
                return Err(format!("unmount after a timed-out mount: {unmount_error}"));
            }
            continue;
        }

        // init.go:955-960: a mount that fails at once, as while the NFS proxy
        // is starting and refuses connections, is tried again until the
        // retry window has passed.
        if start.elapsed() + NFS_MOUNT_RETRY_DELAY > NFS_MOUNT_RETRY_FOR {
            break Some(error);
        }
        tokio::time::sleep(NFS_MOUNT_RETRY_DELAY).await;
    };
    // init.go:962-964.
    if let Some(error) = last_error {
        return Err(format!("after {attempts} attempts: {error}"));
    }

    // init.go:966-974: the mount ID identifies the mount the path resolves
    // to, so this also holds for a path behind a symlink or inside another
    // volume.
    let after = nfs.mount_id(&volume.path).await?;
    if after == before {
        return Err("mount reported success, but the path is not on a new mount".to_owned());
    }

    // init.go:976: only a VERIFIED mount enters the ledger.
    books.store(volume.path.clone(), lifecycle_id.to_owned());

    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go filepath.Clean's unix table (paths.go CleanTests).
    #[test]
    fn go_clean_matches_the_go_table() {
        for (input, want) in [
            ("", "."),
            ("abc", "abc"),
            ("abc/def", "abc/def"),
            ("a/b/c", "a/b/c"),
            ("./", "."),
            ("../", ".."),
            ("../../", "../.."),
            ("/abc", "/abc"),
            ("/", "/"),
            ("a//b", "a/b"),
            ("a/./b", "a/b"),
            ("a/b/c/../..", "a"),
            ("a/../b", "b"),
            ("a/..", "."),
            ("/../a", "/a"),
            ("./..", ".."),
            ("abc/def/../..", "."),
            ("/abc/def/../..", "/"),
            ("//abc", "/abc"),
            ("abc//def//ghi", "abc/def/ghi"),
            ("a/.../b", "a/.../b"),
        ] {
            assert_eq!(go_clean(input), want, "Clean({input:?})");
        }
    }

    /// Oracle init.go:988-990 pathDepth: slash count of the CLEANED path.
    #[test]
    fn path_depth_counts_slashes_of_the_cleaned_path() {
        for (input, want) in [
            ("", 0),
            (".", 0),
            ("data", 0),
            ("/data", 1),
            ("/a/b", 2),
            ("/a/b/c", 3),
            ("//data", 1),
        ] {
            assert_eq!(path_depth(input), want, "pathDepth({input:?})");
        }
    }

    /// The truth table behind init.go:830-836 / mounts_handover: an unknown
    /// or changed lifecycle remounts; the same lifecycle keeps the mounts.
    #[test]
    fn remount_decision_follows_the_lifecycle_truth_table() {
        for (mounted, request, want) in [
            (None, "", true),
            (None, "l", true),
            (Some("l"), "l", false),
            (Some("a"), "b", true),
            (Some(""), "", false),
            (Some("l"), "", true),
        ] {
            assert_eq!(
                should_remount_nfs(mounted, request),
                want,
                "mounted={mounted:?} request={request:?}"
            );
        }
    }

    /// nfs_mount.go:33/:43 argv shapes, byte for byte, and the options
    /// const they are built from.
    #[test]
    fn argv_builders_are_byte_exact() {
        assert_eq!(
            NFS_OPTIONS,
            "sync,rsize=1048576,wsize=1048576,mountproto=tcp,mountport=2049,\
             proto=tcp,port=2049,nfsvers=3,noacl,noac,lookupcache=none,retry=0"
        );
        assert_eq!(
            mount_command("srv:/vol", "/data"),
            vec![
                "-v",
                "-t",
                "nfs",
                "-o",
                "fg,hard,sync,rsize=1048576,wsize=1048576,mountproto=tcp,mountport=2049,\
                 proto=tcp,port=2049,nfsvers=3,noacl,noac,lookupcache=none,retry=0",
                "srv:/vol",
                "/data",
            ]
        );
        assert_eq!(make_dir_command("/data"), vec!["-p", "/data"]);
    }

    /// nfs_mount.go:36/:44: the Go backquote error shape.
    #[test]
    fn failure_text_matches_the_go_backquote_shape() {
        assert_eq!(
            exec_failure_text(
                "mount",
                &mount_command("srv:/v", "/data"),
                "exit status 1",
                "boom"
            ),
            "`mount -v -t nfs -o fg,hard,sync,rsize=1048576,wsize=1048576,mountproto=tcp,\
             mountport=2049,proto=tcp,port=2049,nfsvers=3,noacl,noac,lookupcache=none,retry=0 \
             srv:/v /data` failed: exit status 1: boom"
        );
    }

    /// nfs_mount.go:66-68: the 512-byte cap applies BEFORE the trim, so the
    /// capped head loses its surrounding whitespace and the (trimmed-away)
    /// tail never hides the cap.
    #[test]
    fn output_cap_applies_before_the_trim() {
        assert_eq!(capped_trimmed_output(b"  hello \n"), "hello");
        // 10 leading spaces + 505 x's + 200 trailing spaces: the first 512
        // bytes are 10 spaces + 502 x's, and trimming keeps 502 x's.
        let mut data = vec![b' '; 10];
        data.extend(std::iter::repeat_n(b'x', 505));
        data.extend(std::iter::repeat_n(b' ', 200));
        assert_eq!(capped_trimmed_output(&data), "x".repeat(502));
        // Invalid UTF-8 reads through from_utf8_lossy like Go's string cast.
        assert_eq!(capped_trimmed_output(&[0xFF, b'a']), "\u{FFFD}a");
    }

    /// Go os.ProcessState.String(): "exit status N" and "signal: killed".
    #[test]
    fn exit_status_text_matches_go_process_state_strings() {
        let ok = std::process::Command::new("true").status().unwrap();
        assert_eq!(exit_status_text(&ok), "exit status 0");
        let killed = std::process::Command::new("sh")
            .arg("-c")
            .arg("kill -9 $$")
            .status()
            .unwrap();
        assert_eq!(exit_status_text(&killed), "signal: killed");
    }

    /// Go filepath.EvalSymlinks via resolveExisting, against a real tempdir
    /// fixture: relative and absolute symlinks resolve, a missing tail stays
    /// lexical, and a path through a FILE resolves only its existing prefix.
    #[test]
    fn resolve_existing_resolves_the_longest_existing_prefix() {
        let base = std::env::temp_dir().join(format!("envd-nfs-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real/nested")).unwrap();
        std::fs::write(base.join("real/file.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("real", base.join("rel")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("abs")).unwrap();
        let base = std::fs::canonicalize(&base).unwrap();
        let base = base.to_str().unwrap().to_owned();

        // A relative symlink resolves component-wise.
        assert_eq!(
            resolve_existing(&format!("{base}/rel/nested")),
            format!("{base}/real/nested")
        );
        // So does an absolute one.
        assert_eq!(
            resolve_existing(&format!("{base}/abs/file.txt")),
            format!("{base}/real/file.txt")
        );
        // A missing tail stays lexical over the resolved prefix.
        assert_eq!(
            resolve_existing(&format!("{base}/real/missing/deeper")),
            format!("{base}/real/missing/deeper")
        );
        // ".." folds lexically against the resolved prefix and the symlink
        // that follows still resolves.
        assert_eq!(
            resolve_existing(&format!("{base}/real/../rel")),
            format!("{base}/real")
        );
        // A path through a FILE: only the existing prefix resolves, the
        // non-directory tail rides along unresolved.
        assert_eq!(
            resolve_existing(&format!("{base}/real/file.txt/inside")),
            format!("{base}/real/file.txt/inside")
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// nfs_mount_linux.go:13-15: umount2 on a path with nothing mounted —
    /// including the empty path (ENOENT) — is not an error, and an interior
    /// NUL reads as Go's EINVAL before any syscall. (The EINVAL-on-a-real-
    /// directory case needs umount privileges this host's tests do not have.)
    #[tokio::test]
    async fn unmount_treats_not_a_mount_as_success() {
        assert_eq!(
            ExecNfsMounter.unmount("").await,
            Ok(()),
            "ENOENT is success"
        );
        assert_eq!(
            ExecNfsMounter.unmount("a\0b").await,
            Err("invalid argument".to_owned()),
            "an interior NUL is Go's EINVAL"
        );
    }

    /// nfs_mount_linux.go:20-31: the host kernel reports STATX_MNT_ID for a
    /// real directory, and a missing path reports the errno text.
    #[tokio::test]
    async fn mount_id_reads_statx_and_errors_like_go() {
        let dir = std::env::temp_dir().join(format!("envd-nfs-mntid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id = ExecNfsMounter
            .mount_id(dir.to_str().unwrap())
            .await
            .expect("the kernel reports mount IDs on this host");
        let _ = id;
        let missing = ExecNfsMounter
            .mount_id(&format!("{}/gone", dir.display()))
            .await
            .expect_err("a missing path has no mount id");
        assert_eq!(missing, "no such file or directory");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// nfs_mount.go:42-48: `mkdir -p` nests and is idempotent.
    #[tokio::test]
    async fn make_dir_nests_like_mkdir_p() {
        let dir = std::env::temp_dir().join(format!("envd-nfs-mkdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let deep = dir.join("a/b/c");
        let outcome = ExecNfsMounter
            .make_dir(
                deep.to_str().unwrap(),
                tokio::time::Instant::now() + NFS_MOUNT_TIMEOUT,
            )
            .await;
        assert_eq!(outcome.error, None, "mkdir -p a/b/c succeeds");
        assert!(deep.is_dir());
        // Idempotent on the second run.
        let outcome = ExecNfsMounter
            .make_dir(
                deep.to_str().unwrap(),
                tokio::time::Instant::now() + NFS_MOUNT_TIMEOUT,
            )
            .await;
        assert_eq!(outcome.error, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// nfs_mount.go:54-69: CombinedOutput shape, the 512-byte cap, and the
    /// exit-status column, from a REAL combined-output process.
    #[tokio::test]
    async fn combined_output_is_trimmed_and_capped_at_512() {
        let script = "printf '  '; head -c 600 /dev/zero | tr '\\0' 'x'; printf '\\n  '; exit 3";
        let outcome = run_process_group(
            "sh",
            vec!["-c".to_owned(), script.to_owned()],
            tokio::time::Instant::now() + NFS_MOUNT_TIMEOUT,
        )
        .await;
        let error = outcome.error.expect("exit 3 fails the command");
        // 605 bytes of output cap to the first 512 (2 spaces + 510 x's) and
        // then trim to the x's.
        assert_eq!(
            error,
            exec_failure_text(
                "sh",
                &["-c".to_owned(), script.to_owned()],
                "exit status 3",
                &"x".repeat(510),
            )
        );
        assert!(!outcome.deadline_error);
    }

    /// init.go:942-961: a helper that is not on $PATH fails before any
    /// process exists, with Go's LookPath text.
    #[tokio::test]
    async fn a_missing_helper_carries_the_go_lookpath_text() {
        let outcome = run_process_group(
            "envd-nfs-definitely-missing-helper",
            Vec::new(),
            tokio::time::Instant::now() + NFS_MOUNT_TIMEOUT,
        )
        .await;
        assert_eq!(
            outcome.error.as_deref(),
            Some(
                "`envd-nfs-definitely-missing-helper ` failed: \
                 exec: \"envd-nfs-definitely-missing-helper\": \
                 executable file not found in $PATH: "
            )
        );
        assert!(!outcome.deadline_error);
    }

    /// nfs_mount.go:54-63: at the deadline the WHOLE group is SIGKILLed and
    /// the attempt reports Go's "signal: killed" as a deadline error.
    #[tokio::test]
    async fn the_deadline_kills_the_process_group() {
        let outcome =
            run_process_group("sleep", vec!["30".to_owned()], tokio::time::Instant::now()).await;
        let error = outcome.error.expect("the killed sleep fails the command");
        assert_eq!(
            error,
            exec_failure_text("sleep", &["30".to_owned()], "signal: killed", "")
        );
        assert!(outcome.deadline_error);
    }
}
