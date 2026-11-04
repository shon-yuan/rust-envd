//! Process supervision for the Windows runtime.
//!
//! The Linux supervisor selects over `child.wait()`, a deadline and the
//! control channel. Windows has no `tokio::process` integration for Job
//! Objects or ConPTY, so the wait runs on a blocking thread and the same
//! select drives deadline termination and the two signals. Everything after
//! the wait — control drain, stdin close, reader drain, retention and the
//! terminal publication — mirrors `supervise.rs` on Linux.

use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::Instant;
use windows_sys::Win32::Foundation::{
    HWND, LPARAM, STATUS_CONTROL_C_EXIT, STATUS_DLL_INIT_FAILED, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

use super::conpty::ConPty;
use super::handle::OwnedHandle;
use super::pipe::{Output, Reader, ReaderShared};
use super::{
    end_event, publish, Entry, ProcessEnd, ProcessLogSink, Registry, Retained, RuntimeError,
    KILL_EXIT_CODE,
};
use crate::platform::windows::registry::WorkloadRegistry;
use crate::proto::process as pb;

/// How long a SIGTERM may take before the grace timer terminates the process.
const GRACE: Duration = Duration::from_secs(5);
const OUTCOME_NONE: u8 = 0;
const OUTCOME_KILLED: u8 = 1;
const OUTCOME_TERMINATED: u8 = 2;

pub(crate) enum Signal {
    Kill,
    Term,
}

pub(crate) struct Control {
    pub signal: Signal,
    pub reply: oneshot::Sender<Result<(), RuntimeError>>,
}

pub(super) struct Io {
    pub stdout: Option<OwnedHandle>,
    pub stderr: Option<OwnedHandle>,
    pub pty: Option<Arc<ConPty>>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    registry: Arc<Mutex<Registry>>,
    workload: Arc<WorkloadRegistry>,
    entry: Arc<Entry>,
    mut controls: mpsc::Receiver<Control>,
    timeout: Option<Duration>,
    io: Io,
    logs: Option<Arc<dyn ProcessLogSink>>,
    retention: Duration,
) {
    let Io {
        stdout,
        stderr,
        pty,
    } = io;
    let (output, mut events) = mpsc::channel(64);
    let mut readers = Vec::new();
    if let Some(pty) = pty.as_ref() {
        if let Some(handle) = pty.take_output() {
            match Reader::spawn(
                handle,
                Output::Pty,
                output.clone(),
                entry.pty_bytes.clone(),
                16 * 1024,
            ) {
                Ok(reader) => readers.push(reader),
                Err(error) => eprintln!("process supervision: cannot start pty reader: {error}"),
            }
        }
    } else {
        if let Some(handle) = stdout {
            match Reader::spawn(
                handle,
                Output::Stdout,
                output.clone(),
                entry.stdout_bytes.clone(),
                32 * 1024,
            ) {
                Ok(reader) => readers.push(reader),
                Err(error) => eprintln!("process supervision: cannot start stdout reader: {error}"),
            }
        }
        if let Some(handle) = stderr {
            match Reader::spawn(
                handle,
                Output::Stderr,
                output.clone(),
                entry.stderr_bytes.clone(),
                32 * 1024,
            ) {
                Ok(reader) => readers.push(reader),
                Err(error) => eprintln!("process supervision: cannot start stderr reader: {error}"),
            }
        }
    }
    drop(output);
    let reader_stops: Vec<Arc<ReaderShared>> = readers.iter().map(Reader::shared).collect();
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

    // The blocking wait owns its own duplicated process handle: if this
    // supervision task is cancelled (server teardown), the `Entry` may drop
    // and close the original while `WaitForSingleObject` is still running on
    // the blocking pool.
    let wait_handle = entry.process.duplicate();
    let mut wait = Box::pin(async move {
        match wait_handle {
            Ok(handle) => wait_for_exit(handle).await,
            Err(error) => Err(error),
        }
    });
    let outcome = Arc::new(AtomicU8::new(OUTCOME_NONE));
    let mut signaled = false;
    let mut expired = false;
    let status = loop {
        tokio::select! {
            biased;
            result = &mut wait => break result,
            _ = &mut deadline => {
                let _ = entry.job.terminate(KILL_EXIT_CODE);
                let _ = terminate(&entry.process, KILL_EXIT_CODE);
                outcome.store(OUTCOME_KILLED, Ordering::SeqCst);
                expired = true;
                break wait.as_mut().await;
            }
            Some(control) = controls.recv() => {
                signaled = true;
                match control.signal {
                    Signal::Kill => {
                        let result = terminate(&entry.process, KILL_EXIT_CODE);
                        let _ = entry.job.terminate(KILL_EXIT_CODE);
                        if result.is_ok() {
                            outcome.store(OUTCOME_KILLED, Ordering::SeqCst);
                        }
                        let _ = control.reply.send(
                            result.map_err(|error| RuntimeError::Internal(error.to_string())),
                        );
                    }
                    Signal::Term => {
                        // Record the accepted signal before arming the grace
                        // window: a Ctrl-Break delivered while the child is
                        // still initializing aborts it with
                        // STATUS_DLL_INIT_FAILED, which the terminal synthesis
                        // can only attribute to us through this flag.
                        entry.term_accepted.store(true, Ordering::SeqCst);
                        // The grace window starts when SIGTERM is accepted,
                        // not once delivery succeeds: a PTY writer blocked
                        // behind another write must not postpone escalation.
                        arm_grace(&entry, outcome.clone());
                        // Delivery is best effort and may queue behind a
                        // backpressured PTY writer, so it runs detached: the
                        // select must keep polling the execution deadline and
                        // later signals.
                        let entry = entry.clone();
                        let pty = pty.clone();
                        tokio::spawn(async move {
                            deliver_term(&entry, &pty).await;
                        });
                        // The reply is the request outcome, not the child's
                        // exit.
                        let _ = control.reply.send(Ok(()));
                    }
                }
            }
        }
    };

    // No new signal can be accepted once the selected child is reaped; queued
    // controls get the same NotFound the Linux supervisor returns.
    controls.close();
    while let Some(control) = controls.recv().await {
        let _ = control.reply.send(Err(RuntimeError::NotFound));
    }
    workload.unregister(entry.info.pid);
    // The direct child is reaped, but descendants that inherited its stdio
    // handles keep the readers from reaching EOF. The per-process Job is
    // kill-on-close and contains that tree; release it now so the readers
    // observe EOF and `End` does not wait for an arbitrary grandchild.
    //
    // This must run BEFORE the stdin take below: a `SendInput` blocked in
    // `WriteFile` holds the entry's stdin lock, and only the Job termination
    // makes that write fail (ERROR_BROKEN_PIPE) and release the guard. The
    // later duplicate call stays idempotent.
    // Declared deviation: Linux lets such a descendant keep running.
    let _ = entry.job.terminate(KILL_EXIT_CODE);
    entry.stdin.lock().await.take();

    // ConPTY keeps the output pipe open until the pseudoconsole closes, so
    // release it once the child has exited; the reader then drains buffered
    // bytes and observes EOF. Linux closes the terminal after readers drain
    // because a PTY master reaches EIO without an explicit close.
    //
    // Closing waits for conhost to flush a full output pipe, so it must run
    // off the executor: on a current-thread runtime an inline close would
    // stop the pump from draining the reader channel and deadlock both.
    if pty.is_some() {
        if let Some(pty) = entry.pty.lock().await.take() {
            let _ = tokio::task::spawn_blocking(move || pty.close()).await;
        }
    }
    if signaled || expired {
        for reader in &reader_stops {
            reader.stop();
        }
    }

    let join_readers = async move {
        for reader in readers {
            reader.join().await;
        }
    };
    tokio::pin!(join_readers);
    let mut readers_done = false;
    if !expired {
        tokio::select! {
            _ = &mut join_readers => readers_done = true,
            _ = &mut deadline => {
                expired = true;
                for reader in &reader_stops {
                    reader.stop();
                }
            }
        }
    }
    if !expired {
        tokio::select! {
            _ = &mut pump => {},
            _ = &mut deadline => { expired = true; }
        }
    }
    if expired {
        for reader in &reader_stops {
            reader.stop();
        }
        pump.abort();
        let _ = pump.await;
        if !readers_done {
            join_readers.await;
        }
    }
    // Idempotent for PTY children and a no-op for pipes.
    if let Some(pty) = entry.pty.lock().await.take() {
        pty.close();
    }

    let end = match status {
        Ok(code) => terminal(&outcome, code, entry.term_accepted.load(Ordering::SeqCst)),
        Err(error) => wait_error(&error),
    };
    if let Some(logs) = &logs {
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
                    expires: Instant::now() + retention,
                },
            );
            registry.live.remove(&entry.info.pid);
        }
    }
    let pid = entry.info.pid;
    let expiry = Instant::now() + retention;
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
    // Retention cleanup runs independently even if terminal delivery hits a
    // full subscriber queue without an execution deadline.
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

async fn wait_for_exit(handle: OwnedHandle) -> io::Result<u32> {
    tokio::task::spawn_blocking(move || {
        // SAFETY: `handle` is this call's duplicated process handle, moved
        // into the closure so it stays valid even if the awaiting future is
        // dropped; it is only read, never closed, here.
        let waited = unsafe { WaitForSingleObject(handle.raw(), INFINITE) };
        if waited == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        if waited != WAIT_OBJECT_0 {
            return Err(io::Error::other(format!(
                "unexpected wait result for process handle: {waited}"
            )));
        }
        let mut code: u32 = 0;
        // SAFETY: the process handle is the same live handle and `code` is a
        // valid out-pointer.
        let ok = unsafe { GetExitCodeProcess(handle.raw(), &mut code) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code)
    })
    .await
    .map_err(|error| io::Error::other(error.to_string()))?
}

fn terminate(process: &OwnedHandle, code: u32) -> io::Result<()> {
    // SAFETY: the handle is owned and valid for the duration of the call.
    let ok = unsafe { TerminateProcess(process.raw(), code) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn arm_grace(entry: &Entry, outcome: Arc<AtomicU8>) {
    let Ok(handle) = entry.process.duplicate() else {
        return;
    };
    tokio::spawn(async move {
        tokio::time::sleep(GRACE).await;
        // SAFETY: the duplicated process handle is owned by this task.
        let waited = unsafe { WaitForSingleObject(handle.raw(), 0) };
        if waited == WAIT_TIMEOUT {
            // A SIGKILL request keeps its own synthesis; a plain grace kill
            // reports "terminated".
            let _ = outcome.compare_exchange(
                OUTCOME_NONE,
                OUTCOME_TERMINATED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            // SAFETY: see above; the handle is live and owned here.
            unsafe {
                TerminateProcess(handle.raw(), KILL_EXIT_CODE);
            }
        }
    });
}

async fn deliver_term(entry: &Entry, pty: &Option<Arc<ConPty>>) {
    if let Some(pty) = pty {
        // ETX is the console break a PTY child receives as Ctrl+C.
        let _ = pty.write(&[0x03]).await;
    } else {
        // SAFETY: scalar arguments; the call is a best-effort console
        // control event aimed at the child's own process group.
        unsafe {
            GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, entry.info.pid);
        }
        post_wm_close(entry.info.pid);
    }
}

/// Best-effort `WM_CLOSE` for a GUI child with a top-level window.
fn post_wm_close(pid: u32) {
    struct Search {
        pid: u32,
        posted: bool,
    }
    unsafe extern "system" fn callback(window: HWND, parameter: LPARAM) -> i32 {
        // SAFETY: `parameter` is the live `*mut Search` passed by
        // `post_wm_close` for the duration of this synchronous enumeration.
        let search = unsafe { &mut *(parameter as *mut Search) };
        let mut owner: u32 = 0;
        // SAFETY: `window` came from EnumWindows and `owner` is a valid
        // out-pointer.
        unsafe {
            GetWindowThreadProcessId(window, &mut owner);
        }
        if owner == search.pid {
            // SAFETY: `window` is a valid top-level window handle.
            unsafe {
                PostMessageW(window, WM_CLOSE, 0, 0);
            }
            search.posted = true;
            return 0;
        }
        1
    }
    let mut search = Search { pid, posted: false };
    // SAFETY: the callback only accesses `search` during this synchronous
    // call, and the pointer stays valid until EnumWindows returns.
    unsafe {
        EnumWindows(Some(callback), &mut search as *mut Search as LPARAM);
    }
}

fn terminal(outcome: &AtomicU8, code: u32, term_accepted: bool) -> pb::process_event::EndEvent {
    match outcome.load(Ordering::SeqCst) {
        OUTCOME_KILLED => signal_end("killed"),
        OUTCOME_TERMINATED => signal_end("terminated"),
        // A console CTRL_BREAK default handler exits with this NTSTATUS.
        _ if code == STATUS_CONTROL_C_EXIT as u32 => signal_end("terminated"),
        // The same control event delivered before the child's loader finished
        // aborts it with STATUS_DLL_INIT_FAILED. It is still the child dying
        // of the signal we accepted, and Linux reports SIGTERM as
        // `signal: terminated`; an unsolicited loader failure keeps its raw
        // exit status because no Term was accepted for it.
        _ if term_accepted && code == STATUS_DLL_INIT_FAILED as u32 => signal_end("terminated"),
        _ => {
            let exit_code = code as i32;
            let status = format!("exit status {exit_code}");
            pb::process_event::EndEvent {
                exit_code,
                exited: true,
                error: (exit_code != 0).then(|| status.clone()),
                status,
                ..Default::default()
            }
        }
    }
}

fn signal_end(signal: &str) -> pb::process_event::EndEvent {
    let status = format!("signal: {signal}");
    pb::process_event::EndEvent {
        exit_code: -1,
        exited: false,
        error: Some(status.clone()),
        status,
        ..Default::default()
    }
}

fn wait_error(error: &io::Error) -> pb::process_event::EndEvent {
    let text = error.to_string();
    pb::process_event::EndEvent {
        exit_code: -1,
        error: Some(text.clone()),
        status: text,
        ..Default::default()
    }
}
