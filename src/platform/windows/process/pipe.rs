//! Anonymous pipes, blocking readers and the stdin writer.
//!
//! Windows has no portable async pipe readiness in `windows-sys`, and ConPTY
//! requires synchronous handles, so every stream uses a dedicated blocking
//! thread. A reader publishes bytes into the tokio event channel and exposes a
//! duplicated thread handle so the supervisor can interrupt a blocked
//! `ReadFile` with `CancelSynchronousIo` on stop or at a deadline.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{
    SetHandleInformation, ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, HANDLE, HANDLE_FLAG_INHERIT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::IO::CancelSynchronousIo;

use super::handle::{duplicate_current_thread, OwnedHandle};
use crate::proto::process as pb;

/// The pipe size requested from `CreatePipe` (the system rounds it up).
const PIPE_BUFFER: u32 = 64 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum Output {
    Stdout,
    Stderr,
    Pty,
}

/// `CreatePipe`. Both ends are inheritable when `inheritable` is true; the
/// caller marks the daemon-side end non-inheritable immediately afterwards.
pub(crate) fn create_pipe(inheritable: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: i32::from(inheritable),
    };
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: both out-pointers are valid, and `attributes` is a live,
    // correctly sized SECURITY_ATTRIBUTES for the duration of the call.
    let created = unsafe { CreatePipe(&mut read, &mut write, &attributes, PIPE_BUFFER) };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    match (OwnedHandle::new(read), OwnedHandle::new(write)) {
        (Ok(read), Ok(write)) => Ok((read, write)),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// Clears the inherit flag so only the child end reaches the child process.
pub(crate) fn make_non_inheritable(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: the handle is owned and valid; the flags are scalar values.
    let changed = unsafe { SetHandleInformation(handle.raw(), HANDLE_FLAG_INHERIT, 0) };
    if changed == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The stdin pair: `(daemon write end, child read end)`.
pub(crate) fn stdin_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (child_read, daemon_write) = create_pipe(true)?;
    make_non_inheritable(&daemon_write)?;
    Ok((daemon_write, child_read))
}

/// An output pair: `(daemon read end, child write end)`.
pub(crate) fn output_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (daemon_read, child_write) = create_pipe(true)?;
    make_non_inheritable(&daemon_read)?;
    Ok((daemon_read, child_write))
}

/// Maps the pipe-closure errors the oracle treats as a broken channel onto
/// the `io::ErrorKind`s `process_contract::input_error` classifies.
pub(crate) fn map_pipe_error(error: io::Error) -> io::Error {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_BROKEN_PIPE) => io::Error::new(io::ErrorKind::BrokenPipe, error),
        Some(ERROR_PIPE_NOT_CONNECTED) => io::Error::new(io::ErrorKind::NotConnected, error),
        _ => error,
    }
}

/// Writes every byte to a synchronous handle on a blocking worker.
pub(crate) fn write_all(handle: &OwnedHandle, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let mut written: u32 = 0;
        // SAFETY: `bytes` is a live slice, `written` is a valid out-pointer,
        // and the synchronous pipe handle is owned for the duration.
        let ok = unsafe {
            WriteFile(
                handle.raw(),
                bytes.as_ptr(),
                bytes.len().min(u32::MAX as usize) as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(map_pipe_error(io::Error::last_os_error()));
        }
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

/// The daemon-side stdin write end. Writes are serialized and can be
/// interrupted by closing the writer or by the write failing at the pipe.
pub(crate) struct StdinWriter {
    handle: Option<OwnedHandle>,
    closed: tokio::sync::watch::Sender<bool>,
    writing: tokio::sync::Mutex<()>,
}

impl StdinWriter {
    pub(crate) fn new(handle: OwnedHandle) -> Self {
        Self {
            handle: Some(handle),
            closed: tokio::sync::watch::channel(false).0,
            writing: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let writing = async {
            let _serialized = self.writing.lock().await;
            let handle = self
                .handle
                .as_ref()
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
                .duplicate()?;
            let data = bytes.to_vec();
            tokio::task::spawn_blocking(move || write_all(&handle, &data))
                .await
                .map_err(|error| io::Error::other(error.to_string()))?
        };
        tokio::select! {
            biased;
            _ = closed.changed() => Err(io::ErrorKind::BrokenPipe.into()),
            result = writing => result,
        }
    }
}

impl Drop for StdinWriter {
    fn drop(&mut self) {
        // Persist closure so a queued writer wakes and reports a broken
        // channel instead of hanging until the runtime disappears.
        self.closed.send_replace(true);
        self.handle.take();
    }
}

/// One blocking reader thread plus the handle the supervisor uses to
/// interrupt it.
pub(crate) struct ReaderShared {
    stop: AtomicBool,
    cancel: Mutex<Option<OwnedHandle>>,
}

impl ReaderShared {
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self
            .cancel
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            // SAFETY: the duplicated thread handle identifies the reader
            // thread and stays valid until the reader stores/drops it.
            unsafe {
                CancelSynchronousIo(handle.raw());
            }
        }
    }
}

pub(crate) struct Reader {
    shared: Arc<ReaderShared>,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// Starts a dedicated reader thread.
    pub(crate) fn spawn(
        handle: OwnedHandle,
        output_type: Output,
        output: tokio::sync::mpsc::Sender<pb::ProcessEvent>,
        bytes_written: Arc<AtomicI64>,
        chunk_size: usize,
    ) -> io::Result<Self> {
        let shared = Arc::new(ReaderShared {
            stop: AtomicBool::new(false),
            cancel: Mutex::new(None),
        });
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("envd-process-reader".into())
            .spawn(move || {
                match duplicate_current_thread() {
                    Ok(handle) => {
                        *thread_shared
                            .cancel
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = Some(handle);
                    }
                    Err(error) => {
                        eprintln!("process reader: cannot duplicate thread handle: {error}");
                    }
                }
                read_loop(
                    handle,
                    output_type,
                    output,
                    bytes_written,
                    thread_shared,
                    chunk_size,
                );
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub(crate) fn shared(&self) -> Arc<ReaderShared> {
        self.shared.clone()
    }

    pub(crate) async fn join(mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    }
}

fn read_loop(
    handle: OwnedHandle,
    output_type: Output,
    output: tokio::sync::mpsc::Sender<pb::ProcessEvent>,
    bytes_written: Arc<AtomicI64>,
    shared: Arc<ReaderShared>,
    chunk_size: usize,
) {
    let mut buffer = vec![0u8; chunk_size];
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let mut read: u32 = 0;
        // SAFETY: `buffer` is a live, writable allocation and `read` is a
        // valid out-pointer; the synchronous handle is owned by this thread.
        let ok = unsafe {
            ReadFile(
                handle.raw(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // ERROR_OPERATION_ABORTED is the expected outcome of stop();
            // ERROR_BROKEN_PIPE is the writer closing. Everything else is a
            // terminal read failure, which Linux also treats as EOF.
            break;
        }
        if read == 0 {
            break;
        }
        let count = read as usize;
        let chunk = buffer[..count].to_vec();
        // Go handler byte counters count every chunk read, subscribed or not.
        bytes_written.fetch_add(count as i64, Ordering::SeqCst);
        let output_kind = match output_type {
            Output::Stdout => pb::process_event::data_event::Output::Stdout(chunk),
            Output::Stderr => pb::process_event::data_event::Output::Stderr(chunk),
            Output::Pty => pb::process_event::data_event::Output::Pty(chunk),
        };
        let event = pb::ProcessEvent {
            event: Some(
                pb::process_event::DataEvent {
                    output: Some(output_kind),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        };
        if output.blocking_send(event).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_contract::{input_error, RuntimeError};

    /// The pipe-closure errors the oracle treats as a broken channel must
    /// reach `input_error` as `FailedPrecondition`; every other WriteFile
    /// failure stays `Internal`.
    #[test]
    fn closed_pipe_errors_map_to_failed_precondition() {
        for (code, kind) in [
            (ERROR_BROKEN_PIPE as i32, io::ErrorKind::BrokenPipe),
            (ERROR_PIPE_NOT_CONNECTED as i32, io::ErrorKind::NotConnected),
        ] {
            let mapped = map_pipe_error(io::Error::from_raw_os_error(code));
            assert_eq!(mapped.kind(), kind);
            assert!(matches!(
                input_error(mapped),
                RuntimeError::FailedPrecondition(_)
            ));
        }
        let other = map_pipe_error(io::Error::from_raw_os_error(1234));
        assert!(!matches!(
            other.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected
        ));
        assert!(matches!(input_error(other), RuntimeError::Internal(_)));
    }
}
