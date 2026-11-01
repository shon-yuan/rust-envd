// SPDX-License-Identifier: Apache-2.0
//! Route repair follows pinned internal/host/mmds_route_linux.go.
use super::BoxFuture;
use std::{io, sync::Arc};
pub trait RoutePin: Send + Sync {
    fn pin(&self) -> BoxFuture<'_, io::Result<()>>;
}
#[derive(Debug)]
pub struct CommandOutput {
    pub success: bool,
    pub output: Vec<u8>,
    pub error: String,
}
pub trait CommandRunner: Send + Sync {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, io::Result<CommandOutput>>;
}
/// Actual subprocess runner; construction/activation belongs to guarded guest startup.
///
/// Error text mirrors Go `os/exec` byte-for-byte because the pin failure text
/// is production-observable through the warn line (init.go:88-93): the runner
/// reproduces `exec.Command(...).CombinedOutput` lookup/exec errors
/// (`exec: "name": ...`, `fork/exec <path>: ...`) and `os.ProcessState`
/// renderings (`exit status N`, `signal: <name>`). The program is injectable
/// for tests; production always uses the default `iptables`.
pub struct ProcessRunner {
    program: String,
}
impl Default for ProcessRunner {
    fn default() -> Self {
        Self {
            program: "iptables".into(),
        }
    }
}
impl ProcessRunner {
    /// Test seam: run a harmless stand-in program through the real
    /// lookup/exec machinery instead of the host iptables.
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

/// Go `os/exec.Error`: `exec: "<name>": <reason>`; the name renders through
/// `strconv.Quote`.
fn exec_error(name: &str, reason: &str) -> io::Error {
    io::Error::other(format!("exec: {}: {}", go_quote(name), reason))
}

/// Go `strconv.Quote` for the exec error name rendering.
fn go_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for unit in text.chars() {
        match unit {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            control if (control as u32) < 0x20 || (control as u32) == 0x7f => {
                quoted.push_str(&format!("\\x{:02x}", control as u32));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Go `syscall.Errno.Error()` values (zerrors_linux_amd64.go errorList),
/// rendered lowercase without the errno name.
fn go_errno_text(errno: i32) -> Option<&'static str> {
    Some(match errno {
        1 => "operation not permitted",
        2 => "no such file or directory",
        3 => "no such process",
        4 => "interrupted system call",
        5 => "input/output error",
        6 => "no such device or address",
        7 => "argument list too long",
        8 => "exec format error",
        9 => "bad file descriptor",
        10 => "no child processes",
        11 => "resource temporarily unavailable",
        12 => "cannot allocate memory",
        13 => "permission denied",
        14 => "bad address",
        15 => "block device required",
        16 => "device or resource busy",
        17 => "file exists",
        18 => "invalid cross-device link",
        19 => "no such device",
        20 => "not a directory",
        21 => "is a directory",
        22 => "invalid argument",
        23 => "too many open files in system",
        24 => "too many open files",
        25 => "inappropriate ioctl for device",
        26 => "text file busy",
        27 => "file too large",
        28 => "no space left on device",
        29 => "illegal seek",
        30 => "read-only file system",
        31 => "too many links",
        32 => "broken pipe",
        33 => "numerical argument out of domain",
        34 => "numerical result out of range",
        35 => "resource deadlock avoided",
        36 => "file name too long",
        37 => "no locks available",
        38 => "function not implemented",
        39 => "directory not empty",
        40 => "too many levels of symbolic links",
        _ => return None,
    })
}

/// Go `syscall.Signal.String()` descriptions (sigtable), lowercase without
/// the SIG prefix; unknown numbers render as `signal N`.
fn go_signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "hangup",
        2 => "interrupt",
        3 => "quit",
        4 => "illegal instruction",
        5 => "trace/breakpoint trap",
        6 => "aborted",
        7 => "bus error",
        8 => "floating point exception",
        9 => "killed",
        10 => "user defined signal 1",
        11 => "segmentation fault",
        12 => "user defined signal 2",
        13 => "broken pipe",
        14 => "alarm clock",
        15 => "terminated",
        16 => "stack fault",
        17 => "child exited",
        18 => "continued",
        19 => "stopped (signal)",
        20 => "stopped",
        21 => "stopped (tty input)",
        22 => "stopped (tty output)",
        23 => "urgent I/O condition",
        24 => "CPU time limit exceeded",
        25 => "file size limit exceeded",
        26 => "virtual timer expired",
        27 => "profiling timer expired",
        28 => "window changed",
        29 => "I/O possible",
        30 => "power failure",
        31 => "bad system call",
        _ => return format!("signal {signal}"),
    };
    format!("signal: {name}")
}

/// Why `findExecutable` refused a candidate. `Os` carries either a raw errno
/// or a message for errors without one (Rust synthesizes some, like a NUL in
/// the path).
enum FindError {
    Errno(i32),
    Message(String),
    IsDir,
    Permission,
}

/// Go `findExecutable` (os/exec lp_unix.go): a path is executable when it
/// stats, is not a directory, and carries any execute bit.
fn find_executable(path: &std::path::Path) -> Result<(), FindError> {
    let metadata = fs_metadata(path)?;
    // SAFETY: unix-only crate path; metadata comes from a successful stat.
    let mode = std::os::unix::fs::MetadataExt::mode(&metadata);
    if metadata.is_dir() {
        return Err(FindError::IsDir);
    }
    if mode & 0o111 != 0 {
        return Ok(());
    }
    Err(FindError::Permission)
}

/// `fs::metadata` with the error classified into Go's errno shape.
fn fs_metadata(path: &std::path::Path) -> Result<std::fs::Metadata, FindError> {
    std::fs::metadata(path).map_err(|error| match error.raw_os_error() {
        Some(errno) => FindError::Errno(errno),
        None => FindError::Message(error.to_string()),
    })
}

impl FindError {
    /// The error text Go's findExecutable caller would observe for a
    /// slash-carrying name: a stat PathError, `is a directory`, or
    /// `permission denied`.
    fn text(&self, path: &std::path::Path) -> String {
        match self {
            FindError::Errno(errno) => {
                let text =
                    go_errno_text(*errno).map_or_else(|| format!("errno {errno}"), str::to_owned);
                format!("stat {}: {}", path.display(), text)
            }
            FindError::Message(message) => format!("stat {}: {}", path.display(), message),
            FindError::IsDir => "is a directory".into(),
            FindError::Permission => "permission denied".into(),
        }
    }
}

/// Go `filepath.Join` + `Clean` for one directory element and the program
/// name: collapse slashes, drop `.` elements, resolve `..` lexically.
fn join_clean(directory: &str, name: &str) -> String {
    let joined = format!("{directory}/{name}");
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                // An absolute path's root has no parent to pop.
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !joined.starts_with('/') {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let cleaned = parts.join("/");
    if joined.starts_with('/') {
        format!("/{cleaned}")
    } else {
        cleaned
    }
}

/// Go `exec.LookPath` (os/exec lp_unix.go): slash-carrying names skip the
/// walk; otherwise each PATH entry (empty entry = current directory, an empty
/// PATH = no entries) is searched for an executable file, and a hit below a
/// relative entry is refused like Go's ErrDot.
fn look_path(program: &str) -> io::Result<String> {
    if program.contains('/') {
        let path = std::path::Path::new(program);
        return find_executable(path)
            .map(|()| program.to_owned())
            .map_err(|error| exec_error(program, &error.text(path)));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    if path.is_empty() {
        return Err(exec_error(program, "executable file not found in $PATH"));
    }
    for entry in path.to_string_lossy().split(':') {
        let directory = if entry.is_empty() { "." } else { entry };
        let candidate = join_clean(directory, program);
        let candidate_path = std::path::Path::new(&candidate);
        if find_executable(candidate_path).is_err() {
            continue;
        }
        if candidate_path.is_absolute() {
            return Ok(candidate);
        }
        return Err(exec_error(
            program,
            "cannot run executable found relative to current directory",
        ));
    }
    Err(exec_error(program, "executable file not found in $PATH"))
}

/// Go `os.PathError{Op:"fork/exec"}` for a post-lookup exec failure.
fn fork_exec_error(path: &str, error: &io::Error) -> io::Error {
    let text = match error.raw_os_error() {
        Some(errno) => go_errno_text(errno)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("errno {errno}")),
        None => error.to_string(),
    };
    io::Error::other(format!("fork/exec {path}: {text}"))
}

impl CommandRunner for ProcessRunner {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, io::Result<CommandOutput>> {
        let program = self.program.clone();
        Box::pin(async move {
            // Go resolves the command path eagerly in exec.Command; the
            // resolved path is what forks, and lookup failures carry Go's
            // exec.Error text.
            let resolved = look_path(&program)?;
            // A single shared output pipe preserves stdout/stderr ordering, matching
            // Go CombinedOutput. Owned pipe endpoints close on cancellation.
            use std::os::fd::{FromRawFd, OwnedFd};
            let mut fds = [-1; 2];
            // SAFETY: fds points to two writable descriptor slots.
            if unsafe { nix::libc::pipe2(fds.as_mut_ptr(), nix::libc::O_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful pipe2 yielded two fresh uniquely-owned descriptors.
            let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
            // SAFETY: the second endpoint is distinct and transferred once.
            let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
            let duplicate = write.try_clone()?;
            let mut command = tokio::process::Command::new(&resolved);
            command
                .args(args)
                .kill_on_drop(true)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(write))
                .stderr(std::process::Stdio::from(duplicate));
            let mut child = command
                .spawn()
                .map_err(|error| fork_exec_error(&resolved, &error))?;
            drop(command); // No parent-side writer may keep the read pipe alive.
            let mut read = tokio::fs::File::from_std(std::fs::File::from(read));
            let mut output = Vec::new();
            use tokio::io::AsyncReadExt;
            let (status, bytes) = tokio::join!(child.wait(), read.read_to_end(&mut output));
            bytes?;
            let status = status?;
            // Go os.ProcessState.String(): `exit status N` for a normal exit,
            // `signal: <name>` for a signal death (the ctx-cancel kill
            // included).
            let error = match status.code() {
                Some(code) => format!("exit status {code}"),
                None => {
                    use std::os::unix::process::ExitStatusExt;
                    status
                        .signal()
                        .map_or_else(|| status.to_string(), go_signal_name)
                }
            };
            Ok(CommandOutput {
                success: status.success(),
                output,
                error,
            })
        })
    }
}

pub struct IptablesRoute {
    runner: Arc<dyn CommandRunner>,
    gate: tokio::sync::Mutex<()>,
}
impl IptablesRoute {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            gate: tokio::sync::Mutex::new(()),
        }
    }
}
impl RoutePin for IptablesRoute {
    fn pin(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async {
            let Ok(_guard) = self.gate.try_lock() else {
                return Ok(());
            };
            for chain in ["PREROUTING", "OUTPUT"] {
                for operation in ["-D", "-I"] {
                    let mut args: Vec<String> = ["-w", "5", "-t", "nat", operation, chain]
                        .into_iter()
                        .map(str::to_owned)
                        .collect();
                    if operation == "-I" {
                        args.push("1".into());
                    }
                    args.extend(
                        [
                            "-d",
                            "169.254.169.254",
                            "-p",
                            "tcp",
                            "--dport",
                            "80",
                            "-j",
                            "RETURN",
                        ]
                        .into_iter()
                        .map(str::to_owned),
                    );
                    let result = self.runner.run(args).await;
                    if operation == "-D" {
                        continue;
                    }
                    let error = match result {
                        Ok(output) if output.success => continue,
                        Ok(output) => format!(
                            "{}: {}",
                            output.error,
                            String::from_utf8_lossy(&output.output)
                        ),
                        Err(error) => error.to_string(),
                    };
                    return Err(io::Error::other(format!(
                        "iptables -I nat {chain}: {error}"
                    )));
                }
            }
            Ok(())
        })
    }
}
