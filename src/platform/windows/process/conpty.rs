//! ConPTY ownership.
//!
//! A pseudoconsole is a console-host session wired to two synchronous pipes:
//! the daemon writes input to one and reads terminal output from the other.
//! `ClosePseudoConsole` releases the console-side ends, which is also what
//! makes the output reader observe EOF after the child exits.

use std::io;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};

use super::handle::OwnedHandle;
use super::pipe::{self, create_pipe};
use crate::proto::process as pb;

pub(crate) struct ConPty {
    hpc: Mutex<Option<HPCON>>,
    input: Mutex<Option<OwnedHandle>>,
    output: Mutex<Option<OwnedHandle>>,
    closed: tokio::sync::watch::Sender<bool>,
    writing: tokio::sync::Mutex<()>,
}

/// Linux casts `uint32` sizes to `uint16`; ConPTY takes signed `i16`, so the
/// same truncation is applied and a value below 1 is clamped to 1.
fn coordinate(value: u32) -> i16 {
    ((value as u16) as i16).max(1)
}

fn console_size(size: &pb::pty::Size) -> COORD {
    COORD {
        X: coordinate(size.cols),
        Y: coordinate(size.rows),
    }
}

impl ConPty {
    pub(crate) fn new(pty: &pb::PTY) -> io::Result<Arc<Self>> {
        // ConPTY requires synchronous, non-inheritable handles. The console
        // holds the read side of the input pipe and the write side of the
        // output pipe; the daemon keeps the opposite ends.
        let (input_read, input_write) = create_pipe(false)?;
        let (output_read, output_write) = create_pipe(false)?;
        let mut hpc: HPCON = 0;
        // SAFETY: both console-side handles are valid and owned by this
        // scope; `hpc` is a valid out-pointer. The handles are closed by this
        // function after the pseudoconsole has taken its own references.
        let result = unsafe {
            CreatePseudoConsole(
                console_size(&pty.size),
                input_read.raw(),
                output_write.raw(),
                0,
                &mut hpc,
            )
        };
        if result < 0 {
            return Err(io::Error::other(format!(
                "CreatePseudoConsole failed: 0x{:08X}",
                result as u32
            )));
        }
        drop(input_read);
        drop(output_write);
        Ok(Arc::new(Self {
            hpc: Mutex::new(Some(hpc)),
            input: Mutex::new(Some(input_write)),
            output: Mutex::new(Some(output_read)),
            closed: tokio::sync::watch::channel(false).0,
            writing: tokio::sync::Mutex::new(()),
        }))
    }

    /// The conhost handle passed through `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`.
    pub(crate) fn hpc(&self) -> io::Result<HPCON> {
        self.hpc
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .ok_or_else(|| io::ErrorKind::BrokenPipe.into())
    }

    /// The daemon-side output read handle. The supervision reader thread
    /// takes it exactly once.
    pub(crate) fn take_output(&self) -> Option<OwnedHandle> {
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub(crate) async fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let writing = async {
            let _serialized = self.writing.lock().await;
            let handle = self
                .input
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
                .duplicate()?;
            let data = bytes.to_vec();
            tokio::task::spawn_blocking(move || pipe::write_all(&handle, &data))
                .await
                .map_err(|error| io::Error::other(error.to_string()))?
        };
        tokio::select! {
            biased;
            _ = closed.changed() => Err(io::ErrorKind::BrokenPipe.into()),
            result = writing => result,
        }
    }

    pub(crate) fn resize(&self, pty: &pb::PTY) -> io::Result<()> {
        // Hold the pseudoconsole lock across the call: `close` takes the same
        // lock, so a resize racing the child's exit can never call
        // ResizePseudoConsole on an HPCON that ClosePseudoConsole already
        // destroyed. The lock is released before `close` can free it.
        let guard = self.hpc.lock().unwrap_or_else(|error| error.into_inner());
        let hpc = guard.ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        // SAFETY: `hpc` is the live pseudoconsole handle owned by this value
        // and the guard keeps `close` from freeing it during the call; COORD
        // is a plain by-value struct.
        let result = unsafe { ResizePseudoConsole(hpc, console_size(&pty.size)) };
        if result < 0 {
            return Err(io::Error::other(format!(
                "ResizePseudoConsole failed: 0x{:08X}",
                result as u32
            )));
        }
        Ok(())
    }

    /// Idempotent teardown: release the daemon-side ends and then the
    /// pseudoconsole, which lets the output reader see EOF.
    pub(crate) fn close(&self) {
        self.closed.send_replace(true);
        self.input
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(hpc) = self
            .hpc
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            // SAFETY: the handle was produced by CreatePseudoConsole and is
            // removed from the field, so it is closed exactly once.
            unsafe {
                ClosePseudoConsole(hpc);
            }
        }
    }
}

impl Drop for ConPty {
    fn drop(&mut self) {
        // The same release as `close`, except that the final
        // ClosePseudoConsole runs on a detached thread: it can block on
        // conhost's flush, and Drop may run on an executor worker (the
        // spawn-failure path) where an inline close would stall the runtime.
        self.closed.send_replace(true);
        self.input
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(hpc) = self
            .hpc
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            // SAFETY: the handle is removed from the field, so the detached
            // task is its unique closer; it is closed exactly once.
            let _ = std::thread::Builder::new()
                .name("conpty-close".to_owned())
                .spawn(move || unsafe {
                    ClosePseudoConsole(hpc);
                });
        }
    }
}
