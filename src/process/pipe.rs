//! I/O for re-adopted processes over descriptors inherited across the execve
//! (Go upgrade.go:406-425 `fileOrNil` and the readopt pump loops).
//!
//! The outgoing envd dup3'd the live children's pipe ends onto fixed slots
//! with CLOEXEC cleared, so the kernel objects rode the fd table across
//! `execve`. The incoming envd relocates each slot off the handover range
//! onto a fresh CLOEXEC-set descriptor and wraps it here. Nonblocking rides
//! along: the relocated descriptor shares its open-file-description with the
//! tokio pipe the outgoing envd held, so readiness semantics are unchanged.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, Interest, ReadBuf};
use tokio::process::ChildStdin;

/// A pipe read-end relocated off the handover slot (readopt.go's `fileOrNil`
/// result as a pump reader).
pub(super) struct FdReader(pub(super) AsyncFd<std::fs::File>);

impl AsyncRead for FdReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            let mut ready = ready!(this.0.poll_read_ready(cx))?;
            let result = ready.try_io(|fd| {
                nix::unistd::read(fd.get_ref().as_raw_fd(), buf.initialize_unfilled())
                    .map_err(io::Error::from)
            });
            match result {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_) => continue,
            }
        }
    }
}

/// The daemon-side stdin write-end: tokio's own pipe for started children, or
/// a relocated descriptor for adopted ones (readopt.go sets `h.stdin` from
/// the carried file).
pub(super) enum StdinPipe {
    Started(ChildStdin),
    Adopted(AsyncFd<std::fs::File>),
}

impl StdinPipe {
    pub(super) async fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        match self {
            StdinPipe::Started(pipe) => {
                use tokio::io::AsyncWriteExt;
                pipe.write_all(bytes).await
            }
            StdinPipe::Adopted(fd) => {
                while !bytes.is_empty() {
                    let written = fd
                        .async_io(Interest::WRITABLE, |fd| {
                            nix::unistd::write(fd, bytes).map_err(io::Error::from)
                        })
                        .await?;
                    if written == 0 {
                        return Err(io::ErrorKind::WriteZero.into());
                    }
                    bytes = &bytes[written..];
                }
                Ok(())
            }
        }
    }

    /// The descriptor a chained upgrade relocates (Go `HandoverFds` reads the
    /// live stdin file the same way).
    pub(super) fn raw_fd(&self) -> RawFd {
        match self {
            StdinPipe::Started(pipe) => pipe.as_raw_fd(),
            StdinPipe::Adopted(fd) => fd.get_ref().as_raw_fd(),
        }
    }
}

/// Relocate a carried descriptor off the handover slot range onto a fresh
/// fd with CLOEXEC set, closing the slot (upgrade.go:406-425 `fileOrNil`).
/// This frees the slot for the NEXT upgrade's relocation — a chained upgrade
/// dup3's back onto the same slots, and a parked slot would abort it — and
/// stops the fd from leaking into processes this envd spawns (the next
/// handover re-clears CLOEXEC on its own relocations).
///
/// A dup failure falls back to the slot as-is: the current re-adoption still
/// works; only a later chained upgrade might collide (upgrade.go:412-421).
pub(super) fn relocate_carried_fd(slot: RawFd) -> io::Result<OwnedFd> {
    use nix::fcntl::{fcntl, FcntlArg::F_DUPFD_CLOEXEC};
    if slot < 0 {
        return Err(io::ErrorKind::NotFound.into());
    }
    match fcntl(slot, F_DUPFD_CLOEXEC(0)) {
        Ok(fresh) => {
            let _ = nix::unistd::close(slot);
            // SAFETY: F_DUPFD_CLOEXEC returned a newly allocated descriptor
            // owned by this process; nothing else closes it.
            Ok(unsafe { OwnedFd::from_raw_fd(fresh) })
        }
        Err(_) => {
            // SAFETY: the slot is a live inherited descriptor the registry
            // now solely owns; it is closed when this OwnedFd drops.
            Ok(unsafe { OwnedFd::from_raw_fd(slot) })
        }
    }
}
