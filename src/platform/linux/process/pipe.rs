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

/// A read-end the entry owns and the pump reads, without allocating a second
/// descriptor: Go's `Readopt` stores `args.Stdout` and both the handler and
/// the pump use that one `*os.File`, so a chained export reports the same
/// number the pump is registered on.
pub(super) struct SharedFd(pub(super) std::sync::Arc<OwnedFd>);

impl AsRawFd for SharedFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// A pipe read-end relocated off the handover slot (readopt.go's `fileOrNil`
/// result as a pump reader).
pub(super) struct FdReader(pub(super) AsyncFd<SharedFd>);

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

#[cfg(test)]
mod tests {
    use super::relocate_carried_fd;

    #[test]
    fn relocate_carried_fd_rejects_closed_nonnegative_descriptor() {
        // This cannot be allocated by another parallel test, unlike a low fd
        // that this test opens and closes to manufacture EBADF.
        let result = relocate_carried_fd(i32::MAX);
        match result {
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::EBADF)),
            Ok(fd) => {
                // The current fallback constructs invalid ownership on EBADF.
                // Do not let OwnedFd::drop abort while reporting this RED.
                std::mem::forget(fd);
                panic!("relocate_carried_fd accepted a closed nonnegative descriptor");
            }
        }
    }

    #[test]
    fn relocate_carried_fd_rejects_absent_descriptor() {
        for absent in [-1, i32::MIN] {
            let error = relocate_carried_fd(absent).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        }
    }
}
