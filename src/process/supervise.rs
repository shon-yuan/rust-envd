use super::{
    end_event, publish, Entry, ProcessEnd, ProcessLogSink, Registry, Retained, RuntimeError,
    RETENTION,
};
use crate::proto::process as pb;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::Instant;

pub(super) struct Control {
    pub signal: Signal,
    pub reply: oneshot::Sender<Result<(), RuntimeError>>,
}

pub(super) async fn run(
    registry: Arc<Mutex<Registry>>,
    entry: Arc<Entry>,
    mut child: Child,
    mut controls: mpsc::Receiver<Control>,
    timeout: Option<Duration>,
    tty: Option<Arc<super::pty::Terminal>>,
    logs: Option<Arc<dyn ProcessLogSink>>,
) {
    let (output, mut events) = mpsc::channel(64);
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let stdout = if let Some(terminal) = tty {
        Some(tokio::spawn(read(
            super::pty::Reader(terminal),
            output.clone(),
            Output::Pty,
            stopping.clone(),
            entry.pty_bytes.clone(),
        )))
    } else {
        child.stdout.take().map(|pipe| {
            tokio::spawn(read(
                pipe,
                output.clone(),
                Output::Stdout,
                stopping.clone(),
                entry.stdout_bytes.clone(),
            ))
        })
    };
    let stderr = child.stderr.take().map(|pipe| {
        tokio::spawn(read(
            pipe,
            output.clone(),
            Output::Stderr,
            stopping.clone(),
            entry.stderr_bytes.clone(),
        ))
    });
    drop(output);
    let observing = entry.clone();
    let mut pump = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            publish(&observing, event, false).await;
        }
    });
    let deadline = async {
        match timeout.filter(|duration| !duration.is_zero()) {
            Some(timeout) => tokio::time::sleep(timeout).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline);
    let mut signaled = false;
    let mut expired = false;
    let status = loop {
        tokio::select! {
            biased;
            result = child.wait() => break result,
            _ = &mut deadline => {
                // Kill selected child only, matching Go CommandContext. Grandchildren
                // are not implied process-group members of this operation.
                let _ = child.start_kill();
                expired = true;
                break child.wait().await;
            }
            Some(control) = controls.recv() => {
                // Child is still owned/unreaped here, preventing a stale PID signal.
                signaled = true;
                let result = kill(Pid::from_raw(entry.info.pid as i32), control.signal)
                    .map_err(|error| RuntimeError::Internal(error.to_string()));
                let _ = control.reply.send(result);
            }
        }
    };
    // No new signal can be accepted after wait reaped the selected child. Reply
    // to already queued controls as well, instead of retaining an open receiver
    // throughout descendant-pipe/output draining.
    controls.close();
    while let Some(control) = controls.recv().await {
        let _ = control.reply.send(Err(RuntimeError::NotFound));
    }
    entry.stdin.lock().await.take();
    // Signals release Go's output wait; pipe ends remain open until wait returns.
    // Readers drain immediately available trap output before observing stop.
    if signaled || expired {
        let _ = stop.send(true);
    }
    let readers = async {
        if let Some(reader) = stdout {
            let _ = reader.await;
        }
        if let Some(reader) = stderr {
            let _ = reader.await;
        }
    };
    tokio::pin!(readers);
    let mut readers_done = false;
    if !expired {
        tokio::select! {
            _ = &mut readers => { readers_done = true; },
            _ = &mut deadline => { expired = true; }
        }
    }
    if !expired {
        tokio::select! {
            _ = &mut pump => {},
            _ = &mut deadline => { expired = true; }
        }
    }
    if expired {
        let _ = stop.send(true);
        // A slow observer can hold the pump in a send. Stop that wait as well
        // as reader enqueue waits; no registry lock is held by either task.
        pump.abort();
        let _ = pump.await;
        if !readers_done {
            readers.await;
        }
    }
    // Readers have released their master. Invalidate input/resize ownership too.
    if let Some(terminal) = entry.pty.lock().await.take() {
        terminal.close();
    }
    let end = match status {
        Ok(status) => terminal(status),
        Err(error) => pb::process_event::EndEvent {
            exit_code: -1,
            error: Some(error.to_string()),
            status: error.to_string(),
            ..Default::default()
        },
    };
    if let Some(logs) = &logs {
        // handler.go:697-706, after the terminal event is built.
        logs.process_ended(&ProcessEnd {
            pid: entry.info.pid,
            end: end.clone(),
            stdout_bytes: entry.stdout_bytes.load(Ordering::SeqCst),
            stderr_bytes: entry.stderr_bytes.load(Ordering::SeqCst),
            pty_bytes: entry.pty_bytes.load(Ordering::SeqCst),
            operation_id: entry.operation_id.clone(),
        });
    }
    {
        let mut registry = registry.lock().await;
        if registry
            .live
            .get(&entry.info.pid)
            .is_some_and(|live| Arc::ptr_eq(live, &entry))
        {
            registry.retained.insert(
                entry.info.pid,
                Retained {
                    info: entry.info.clone(),
                    end: end.clone(),
                    expires: Instant::now() + RETENTION,
                },
            );
            registry.live.remove(&entry.info.pid);
        }
    }
    let pid = entry.info.pid;
    let expiry = Instant::now() + RETENTION;
    tokio::spawn(async move {
        tokio::time::sleep_until(expiry).await;
        let mut registry = registry.lock().await;
        if registry
            .retained
            .get(&pid)
            .is_some_and(|exit| exit.expires <= Instant::now())
        {
            registry.retained.remove(&pid);
        }
    });
    // Retention cleanup runs independently even if terminal delivery encounters
    // a full subscriber queue without an execution deadline.
    let terminal = publish(&entry, end_event(end), expired);
    tokio::pin!(terminal);
    if expired {
        terminal.await;
    } else {
        tokio::select! {
            _ = &mut terminal => {},
            _ = &mut deadline => {}
        }
    }
}

#[derive(Clone, Copy)]
enum Output {
    Stdout,
    Stderr,
    Pty,
}

/// The inherited descriptor endpoints an adopted entry pumps (readopt.go's
/// reader set): relocated stdout/stderr read-ends and, for pty children, the
/// rebuilt terminal (its stdout/stderr slots are absent and never wrapped).
pub(super) struct AdoptedIo {
    pub(super) stdout: Option<super::pipe::FdReader>,
    pub(super) stderr: Option<super::pipe::FdReader>,
    pub(super) tty: Option<Arc<super::pty::Terminal>>,
}

/// The incoming handover's adopted-process supervision (readopt.go): readers
/// over the inherited descriptors, a pid-specific reaper instead of an owned
/// `Child`, and a kill timer that arms only once the workload thaws. The
/// child itself kept running as this process's child across the execve, so
/// its output keeps flowing and its exit status is collected with a
/// pid-specific wait.
pub(super) async fn run_adopted(
    registry: Arc<Mutex<Registry>>,
    entry: Arc<Entry>,
    mut controls: mpsc::Receiver<Control>,
    io: AdoptedIo,
    timeout: Option<Duration>,
    thawed: tokio::sync::watch::Receiver<()>,
    _logs: Option<Arc<dyn ProcessLogSink>>,
) {
    let (output, mut events) = mpsc::channel(64);
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let pty_reader = io.tty.map(|terminal| {
        tokio::spawn(read(
            super::pty::Reader(terminal),
            output.clone(),
            Output::Pty,
            stopping.clone(),
            entry.pty_bytes.clone(),
        ))
    });
    let stdout_reader = io.stdout.map(|pipe| {
        tokio::spawn(read(
            pipe,
            output.clone(),
            Output::Stdout,
            stopping.clone(),
            entry.stdout_bytes.clone(),
        ))
    });
    let stderr_reader = io.stderr.map(|pipe| {
        tokio::spawn(read(
            pipe,
            output.clone(),
            Output::Stderr,
            stopping.clone(),
            entry.stderr_bytes.clone(),
        ))
    });
    drop(output);
    let observing = entry.clone();
    let mut pump = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            publish(&observing, event, false).await;
        }
    });
    // The carried kill deadline arms only after the thaw (readopt.go
    // BeginReaping waits on `Thawed`): frozen time must not burn the
    // remaining timeout down before the process can run again. The absolute
    // deadline is recorded once armed, so a chained handover carries the
    // remaining time exactly as Go's `Deadline()` does.
    let deadline = async {
        match timeout.filter(|duration| !duration.is_zero()) {
            Some(timeout) => {
                let mut thawed = thawed;
                let _ = thawed.wait_for(|_| true).await;
                *entry.deadline_at.lock().await = Some(Instant::now() + timeout);
                tokio::time::sleep(timeout).await
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline);
    let mut signaled = false;
    let mut expired = false;
    let status = loop {
        tokio::select! {
            biased;
            result = wait_adopted(entry.info.pid) => break result,
            _ = &mut deadline => {
                // Kill the adopted child only; the pid-specific kill mirrors
                // the live path's selected-child kill.
                let _ = kill(Pid::from_raw(entry.info.pid as i32), Signal::SIGKILL);
                expired = true;
                break wait_adopted(entry.info.pid).await;
            }
            Some(control) = controls.recv() => {
                // The pid is ours to signal until wait_adopted reaps it; a
                // stale control after that is refused like the live path.
                signaled = true;
                let result = kill(Pid::from_raw(entry.info.pid as i32), control.signal)
                    .map_err(|error| RuntimeError::Internal(error.to_string()));
                let _ = control.reply.send(result);
            }
        }
    };
    // No new signal is accepted once the child is reaped and the pid could be
    // recycled (readopt.go closes the EndEvent source at the same point).
    controls.close();
    while let Some(control) = controls.recv().await {
        let _ = control.reply.send(Err(RuntimeError::NotFound));
    }
    entry.stdin.lock().await.take();
    if signaled || expired {
        let _ = stop.send(true);
    }
    let readers = async {
        if let Some(reader) = pty_reader {
            let _ = reader.await;
        }
        if let Some(reader) = stdout_reader {
            let _ = reader.await;
        }
        if let Some(reader) = stderr_reader {
            let _ = reader.await;
        }
    };
    tokio::pin!(readers);
    let mut readers_done = false;
    if !expired {
        tokio::select! {
            _ = &mut readers => { readers_done = true; },
            _ = &mut deadline => { expired = true; }
        }
    }
    if !expired {
        tokio::select! {
            _ = &mut pump => {},
            _ = &mut deadline => { expired = true; }
        }
    }
    if expired {
        let _ = stop.send(true);
        pump.abort();
        let _ = pump.await;
        if !readers_done {
            readers.await;
        }
    }
    if let Some(terminal) = entry.pty.lock().await.take() {
        terminal.close();
    }
    let end = match status {
        Ok(status) => adopted_terminal(status),
        Err(error) => pb::process_event::EndEvent {
            exit_code: -1,
            error: Some(error.to_string()),
            status: "wait4 error".into(),
            ..Default::default()
        },
    };
    // readopt.go reapByPidfd's event: the readopt end event, not the live
    // handler's process_end line.
    eprintln!(
        "envd: re-adopted process ended (pid {}, {})",
        entry.info.pid, end.status
    );
    {
        let mut registry = registry.lock().await;
        if registry
            .live
            .get(&entry.info.pid)
            .is_some_and(|live| Arc::ptr_eq(live, &entry))
        {
            registry.retained.insert(
                entry.info.pid,
                Retained {
                    info: entry.info.clone(),
                    end: end.clone(),
                    expires: Instant::now() + RETENTION,
                },
            );
            registry.live.remove(&entry.info.pid);
        }
    }
    let pid = entry.info.pid;
    let expiry = Instant::now() + RETENTION;
    tokio::spawn(async move {
        tokio::time::sleep_until(expiry).await;
        let mut registry = registry.lock().await;
        if registry
            .retained
            .get(&pid)
            .is_some_and(|exit| exit.expires <= Instant::now())
        {
            registry.retained.remove(&pid);
        }
    });
    let terminal = publish(&entry, end_event(end), expired);
    tokio::pin!(terminal);
    if expired {
        terminal.await;
    } else {
        tokio::select! {
            _ = &mut terminal => {},
            _ = &mut deadline => {}
        }
    }
}

/// Block until the adopted pid exits, then collect its status with a
/// pid-specific wait (readopt.go `reapByPidfd`: pidfd poll + `wait4(pid)` —
/// never a `wait4(-1)`, which could steal another component's child). tokio
/// 1.52 reaps only pid-specifically (registered reapers and the orphan drain
/// both `waitpid(pid)`), so the status survives until we collect it.
async fn wait_adopted(pid: u32) -> std::io::Result<std::process::ExitStatus> {
    // SAFETY: pidfd_open takes the pid and flags over registers; a negative
    // return means the kernel refused (unsupported, fd exhaustion, or an
    // already-reaped pid) and the fallback polls the pid directly.
    let pidfd = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_pidfd_open,
            pid as nix::libc::c_uint,
            nix::libc::O_NONBLOCK as nix::libc::c_uint,
        )
    };
    if pidfd >= 0 {
        // SAFETY: pidfd_open handed us a fresh descriptor this block solely
        // owns; AsyncFd closes it on drop.
        let owned = unsafe { OwnedFd::from_raw_fd(pidfd as i32) };
        let async_fd = tokio::io::unix::AsyncFd::new(owned)?;
        // The pidfd reports readable exactly once — when the process exits.
        let _ = async_fd.readable().await?;
        drop(async_fd);
    } else {
        // Fallback: poll the pid directly (readopt.go's "pidfd_open failed;
        // falling back to blocking wait4" path, in non-blocking shape).
        loop {
            let polled = wait_adopted_raw(pid, nix::libc::WNOHANG)?;
            if let Some(status) = polled {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    Ok(wait_adopted_raw(pid, 0)?.unwrap())
}

/// One pid-specific waitpid with a restart on EINTR. `Some` carries the raw
/// wait status for a reaped child; `None` is WNOHANG's still-alive.
fn wait_adopted_raw(
    pid: u32,
    flags: nix::libc::c_int,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    // SAFETY: waitpid takes the pid, a status out-pointer and flag bits; the
    // child is ours, so the kernel fills the status once it exits.
    let mut status: nix::libc::c_int = 0;
    let rc = unsafe { nix::libc::waitpid(pid as nix::libc::pid_t, &mut status, flags) };
    if rc < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return wait_adopted_raw(pid, flags);
        }
        return Err(error);
    }
    if rc == 0 {
        return Ok(None);
    }
    Ok(Some(std::process::ExitStatus::from_raw(status)))
}

/// readopt.go `reapByPidfd`'s terminal rendering. Deliberately different
/// from the live path's `ProcessState.String()` text: a normal exit renders
/// `"exited"` (not "exit status N") and a signal death renders the bare
/// signal name with exit code 128+signal — the oracle's two paths differ,
/// and an upgraded envd speaks the readopt variant.
fn adopted_terminal(status: std::process::ExitStatus) -> pb::process_event::EndEvent {
    let code = status.code();
    if let Some(code) = code {
        pb::process_event::EndEvent {
            exit_code: code,
            exited: true,
            status: "exited".into(),
            ..Default::default()
        }
    } else if let Some(signal) = status.signal() {
        pb::process_event::EndEvent {
            exit_code: 128 + signal,
            exited: false,
            status: crate::errno_text::signal_description(signal),
            ..Default::default()
        }
    } else {
        pb::process_event::EndEvent {
            exit_code: -1,
            exited: false,
            status: "exited".into(),
            ..Default::default()
        }
    }
}

async fn read(
    mut pipe: impl AsyncRead + Unpin,
    output: mpsc::Sender<pb::ProcessEvent>,
    output_type: Output,
    mut stopping: tokio::sync::watch::Receiver<bool>,
    bytes_written: Arc<AtomicI64>,
) {
    let chunk_size = if matches!(output_type, Output::Pty) {
        16 * 1024
    } else {
        32 * 1024
    };
    let mut bytes = vec![0; chunk_size];
    loop {
        let count = tokio::select! {
            biased;
            result = pipe.read(&mut bytes) => match result {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            },
            _ = stopping.changed() => break,
        };
        let bytes = bytes[..count].to_vec();
        // Go handler.go:337/:406/:442 counts every chunk read, subscribed or not.
        bytes_written.fetch_add(count as i64, Ordering::SeqCst);
        let output_kind = match output_type {
            Output::Stdout => pb::process_event::data_event::Output::Stdout(bytes),
            Output::Stderr => pb::process_event::data_event::Output::Stderr(bytes),
            Output::Pty => pb::process_event::data_event::Output::Pty(bytes),
        };
        let data = pb::process_event::DataEvent {
            output: Some(output_kind),
            ..Default::default()
        };
        let event = pb::ProcessEvent {
            event: Some(data.into()),
            ..Default::default()
        };
        // Preserve immediately available output, including TERM traps, but a
        // full source queue must not suppress the stop notification.
        tokio::select! {
            biased;
            result = output.send(event) => if result.is_err() { break; },
            _ = stopping.changed() => break,
        }
    }
}

fn terminal(status: std::process::ExitStatus) -> pb::process_event::EndEvent {
    let code = status.code();
    let mut description = match (code, status.signal()) {
        (Some(code), _) => format!("exit status {code}"),
        // Go ProcessState.String(): "signal: " + syscall.Signal.String().
        (None, Some(signal)) => {
            format!("signal: {}", crate::errno_text::signal_description(signal))
        }
        (None, None) => "exit status -1".into(),
    };
    // Go appends the WCOREDUMP note to the rendered state text
    // (os/exec_posix.go:149-150); core_dumped reads the same wait-status bit
    // children.rs does.
    if status.core_dumped() {
        description.push_str(" (core dumped)");
    }
    pb::process_event::EndEvent {
        exit_code: code.unwrap_or(-1),
        exited: code.is_some(),
        error: (!status.success()).then(|| description.clone()),
        status: description,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::terminal;
    use std::os::unix::process::ExitStatusExt;

    fn status(raw: i32) -> std::process::ExitStatus {
        std::process::ExitStatus::from_raw(raw)
    }

    /// Oracle handler.go:677-679 stores `ProcessState.String()`, i.e.
    /// "exit status N" or "signal: " + `syscall.Signal.String()`.
    #[test]
    fn terminal_status_strings_match_go_process_state() {
        let clean = terminal(status(0));
        assert_eq!(clean.exit_code, 0);
        assert!(clean.exited);
        assert_eq!(clean.status, "exit status 0");
        assert_eq!(clean.error, None);

        let failed = terminal(status(7 << 8));
        assert_eq!(failed.exit_code, 7);
        assert!(failed.exited);
        assert_eq!(failed.status, "exit status 7");
        assert_eq!(failed.error.as_deref(), Some("exit status 7"));

        let killed = terminal(status(9));
        assert_eq!(killed.exit_code, -1);
        assert!(!killed.exited);
        assert_eq!(killed.status, "signal: killed");
        assert_eq!(killed.error.as_deref(), Some("signal: killed"));

        let terminated = terminal(status(15));
        assert_eq!(terminated.status, "signal: terminated");

        // Go syscall.Signal.String renders names, not numbers: SIGINT is
        // "signal: interrupt" (os/exec_posix.go String + syscall sigtable).
        let interrupted = terminal(status(2));
        assert_eq!(interrupted.status, "signal: interrupt");

        let fault = terminal(status(11));
        assert_eq!(fault.status, "signal: segmentation fault");

        let unknown = terminal(status(40));
        assert_eq!(unknown.status, "signal: signal 40");
    }

    /// Go ProcessState.String() appends " (core dumped)" when the wait
    /// status carries WCOREDUMP (os/exec_posix.go:149-150) — the same bit
    /// `children.rs` reads via `ExitStatus::core_dumped`. Raw wait status
    /// 0x80|11: died by SIGSEGV with a core dump.
    #[test]
    fn terminal_reports_go_core_dumped_suffix() {
        let dumped = terminal(status(0x80 | 11));
        assert_eq!(dumped.exit_code, -1);
        assert!(!dumped.exited);
        assert_eq!(dumped.status, "signal: segmentation fault (core dumped)");
        assert_eq!(
            dumped.error.as_deref(),
            Some("signal: segmentation fault (core dumped)")
        );
    }
}
