// SPDX-License-Identifier: Apache-2.0
use super::ChildWrites;
use std::{
    collections::BTreeSet,
    io,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Condvar, Mutex, MutexGuard},
};

#[derive(Default)]
struct State {
    pids: BTreeSet<i32>,
    adopted: usize,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// Tracks helper children only. No process-global wait(-1), signals, or kill-on-drop.
/// Adopt must finish registering all inherited PIDs before wait_adopted is called.
#[derive(Default, Clone)]
pub struct ChildRegistry {
    shared: Arc<Shared>,
}

/// Holding this guard prevents any helper from starting during exec handover.
pub struct ExportHold<'a> {
    _guard: MutexGuard<'a, State>,
    pids: Vec<i32>,
}
impl ExportHold<'_> {
    pub fn pids(&self) -> &[i32] {
        &self.pids
    }
}

impl ChildRegistry {
    pub fn export_hold(&self) -> ExportHold<'_> {
        let guard = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pids = guard.pids.iter().copied().collect();
        ExportHold {
            _guard: guard,
            pids,
        }
    }

    pub fn adopt(&self, pid: i32) -> io::Result<()> {
        if pid <= 0 {
            return Ok(());
        }
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.pids.contains(&pid) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "child is already registered",
            ));
        }
        state.pids.insert(pid);
        state.adopted += 1;
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name("envd-cpu-adopt".into())
            .spawn(move || {
                loop {
                    // SAFETY: waits for exactly the caller-supplied inherited child;
                    // no other child process's status can be consumed here.
                    let result = unsafe { nix::libc::waitpid(pid, std::ptr::null_mut(), 0) };
                    if result >= 0
                        || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                    {
                        break;
                    }
                }
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.pids.remove(&pid);
                state.adopted -= 1;
                shared.changed.notify_all();
            });
        if let Err(error) = thread {
            state.pids.remove(&pid);
            state.adopted -= 1;
            return Err(error);
        }
        Ok(())
    }

    /// Spawn under the export lock, then retain ownership until the helper exits.
    /// Main dispatch must recognize cpu-online and E2B_REEXEC_HELPER before startup.
    fn run(&self, command: &mut Command) -> io::Result<()> {
        command.stderr(Stdio::piped());
        let child = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let child = command.spawn()?;
            state.pids.insert(child.id() as i32);
            child
        };
        let pid = child.id() as i32;
        let result = child.wait_with_output();
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pids
            .remove(&pid);
        let output = result?;
        if output.status.success() {
            return Ok(());
        }
        use std::os::unix::process::ExitStatusExt;
        let error = match output.status.code() {
            Some(code) => format!("exit status {code}"),
            None => {
                let signal = output.status.signal().unwrap_or_default();
                let description = crate::errno_text::signal_description(signal);
                format!(
                    "signal: {description}{}",
                    if output.status.core_dumped() {
                        " (core dumped)"
                    } else {
                        ""
                    }
                )
            }
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        Err(io::Error::other(if stderr.is_empty() {
            error
        } else {
            format!("{error}: {stderr}")
        }))
    }
}

// Go's syscall.Signal.String uses these Linux descriptions rather than Rust's
// platform-dependent ExitStatus display format; the table lives in
// crate::errno_text so the process runtime can share it.

impl ChildWrites for ChildRegistry {
    fn write(&self, path: &Path, online: bool) -> io::Result<()> {
        let mut command = Command::new("/proc/self/exe");
        command
            .arg("cpu-online")
            .arg(path)
            .arg(if online { "1" } else { "0" })
            .env("E2B_REEXEC_HELPER", "cpu-online")
            .stdin(Stdio::null());
        self.run(&mut command)
    }
    fn wait_adopted(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.adopted > 0 {
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_failure_returns_go_exit_status_with_trimmed_stderr() {
        let registry = ChildRegistry::default();
        for (script, expected) in [
            (
                "printf '  injected EBUSY \\n' >&2; exit 7",
                "exit status 7: injected EBUSY",
            ),
            ("printf ' \\n\\t' >&2; exit 2", "exit status 2"),
            (
                "printf 'first\\nsecond\\n' >&2; exit 1",
                "exit status 1: first\nsecond",
            ),
        ] {
            let error = registry
                .run(Command::new("/bin/sh").args(["-c", script]))
                .unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert!(registry.export_hold().pids().is_empty());
        }
        registry
            .run(Command::new("/bin/sh").args(["-c", "printf 'ignored' >&2; exit 0"]))
            .unwrap();
    }

    /// A helper killed by a signal reports Go's `exec.ExitError` text:
    /// "signal: " + syscall.Signal.String() (no numeric rendering).
    #[test]
    fn helper_killed_by_signal_reports_go_signal_text() {
        let registry = ChildRegistry::default();
        let error = registry
            .run(Command::new("/bin/sh").args(["-c", "kill -INT $$"]))
            .unwrap_err();
        assert_eq!(error.to_string(), "signal: interrupt");
        assert!(registry.export_hold().pids().is_empty());
    }

    #[test]
    fn export_hold_blocks_spawn_and_finished_helpers_leave_registry_empty() {
        let registry = Arc::new(ChildRegistry::default());
        let hold = registry.export_hold();
        let worker = registry.clone();
        let (ready, entered) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            ready.send(()).unwrap();
            let result = worker.run(Command::new("/bin/sh").args(["-c", "exit 0"]));
            done.send(result).unwrap();
        });
        entered.recv().unwrap();
        assert!(finished.try_recv().is_err());
        drop(hold);
        finished.recv().unwrap().unwrap();
        thread.join().unwrap();
        assert!(registry.export_hold().pids().is_empty());
        assert!(registry
            .run(Command::new("/bin/sh").args(["-c", "exit 7"]))
            .is_err());
        assert!(registry.export_hold().pids().is_empty());
    }
    #[test]
    fn adopted_child_is_exported_until_reaped_and_export_blocks_starts() {
        let registry = std::sync::Arc::new(ChildRegistry::default());
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read value"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let input = child.stdin.take().unwrap();
        drop(child);
        registry.adopt(pid).unwrap();
        {
            let hold = registry.export_hold();
            assert_eq!(hold.pids(), &[pid]);
        }
        drop(input);
        registry.wait_adopted();
        assert!(registry.export_hold().pids().is_empty());
    }
}
