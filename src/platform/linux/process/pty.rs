//! Linux PTY ownership and nonblocking readiness; no blocking worker or forkpty.
use crate::proto::process as pb;
use nix::fcntl::OFlag;
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt, Winsize};
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd as _};
use std::os::unix::fs::OpenOptionsExt;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, Interest, ReadBuf};
use tokio::process::Command;
use tokio::sync::Mutex;

pub(super) struct Terminal {
    // A plain File, not nix's PtyMaster: nix 0.29 offers no constructor from
    // a relocated OwnedFd (readopt.go's carried Tty), and File closes the
    // descriptor on drop exactly as PtyMaster would.
    fd: std::sync::Mutex<Option<Arc<AsyncFd<std::fs::File>>>>,
    closed: tokio::sync::watch::Sender<bool>,
    writing: Mutex<()>,
}

impl Terminal {
    /// The raw setup error propagates: the caller wraps it with the oracle's
    /// PTY start text (handler.go:326-328 via start.go:184), which needs the
    /// user command, resolved dir and window size this module does not see.
    pub fn configure(command: &mut Command, pty: &pb::PTY) -> io::Result<Arc<Self>> {
        let mut setup = || -> io::Result<_> {
            // CLOEXEC is atomic: another concurrent spawn must not inherit this
            // master or slave and prevent EOF. Only the master is nonblocking.
            // Go's funnel opens /dev/ptmx with os.OpenFile first
            // (creack/pty v1.1.24 pty_linux.go:14), so a failure here renders
            // as the *fs.PathError `open /dev/ptmx: <errno text>` — the layer
            // the oracle's handler.go:326-328 wrap carries.
            let master = posix_openpt(
                OFlag::O_RDWR | OFlag::O_NOCTTY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            )
            .map_err(|error| {
                super::spawn::path_text_error("open", "/dev/ptmx", io::Error::from(error))
            })?;
            grantpt(&master)?;
            unlockpt(&master)?;
            let slave_path = ptsname_r(&master)?;
            // SAFETY: `into_raw_fd` surrenders nix's owned master descriptor
            // and the File adopts it, closing it on drop exactly as
            // PtyMaster's own drop would. The nonblocking flag lives on the
            // shared open-file description and rides along unchanged.
            let master = unsafe { std::fs::File::from_raw_fd(master.into_raw_fd()) };
            let slave = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(nix::libc::O_NOCTTY | nix::libc::O_CLOEXEC)
                .open(&slave_path)
                .map_err(|error| {
                    // creack/pty's open() ends with os.OpenFile(sname) the
                    // same way (pty_linux.go:34), so this failure renders as
                    // `open /dev/pts/N: <errno text>`.
                    super::spawn::path_text_error("open", &slave_path, error)
                })?;
            // SAFETY: File exclusively owns the fresh master. Active operations
            // retain the same Arc registration; close never replaces its fd.
            let master = unsafe { AsyncFd::register(master)? };
            let terminal = Arc::new(Self {
                fd: std::sync::Mutex::new(Some(Arc::new(master))),
                closed: tokio::sync::watch::channel(false).0,
                writing: Mutex::new(()),
            });
            terminal.resize(pty)?;
            command
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // SAFETY: after stdio duplication, fd 0 is the owned slave. Only
            // async-signal-safe syscalls run here; no allocation or locks.
            unsafe {
                command.pre_exec(|| {
                    nix::unistd::setsid().map_err(io::Error::from)?;
                    if nix::libc::ioctl(0, nix::libc::TIOCSCTTY, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            Ok(terminal)
        };
        setup()
    }

    fn descriptor(&self) -> io::Result<Arc<AsyncFd<std::fs::File>>> {
        self.fd
            .lock()
            .map_err(|_| io::Error::other("terminal ownership lock poisoned"))?
            .clone()
            .ok_or_else(|| io::ErrorKind::BrokenPipe.into())
    }

    /// Keep the master registration alive until a handover duplicates it.
    /// Absent once closed or fully detached; a returned snapshot survives close.
    pub(super) fn handover_descriptor(&self) -> Option<Arc<AsyncFd<std::fs::File>>> {
        self.fd
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Rebuild the terminal around a master descriptor inherited across the
    /// upgrade execve (readopt.go's `Tty` file). The relocated fd shares its
    /// open-file-description with the outgoing master, so nonblocking and the
    /// kernel terminal state (winsize, termios) ride along unchanged.
    pub(super) fn from_master(fd: std::os::fd::OwnedFd) -> io::Result<Arc<Self>> {
        // SAFETY: the transferred master remains owned by File for the full
        // registration lifetime. Active operations retain the same Arc.
        let fd = unsafe { AsyncFd::register(std::fs::File::from(fd))? };
        Ok(Arc::new(Self {
            fd: std::sync::Mutex::new(Some(Arc::new(fd))),
            closed: tokio::sync::watch::channel(false).0,
            writing: Mutex::new(()),
        }))
    }

    pub fn close(&self) {
        // Persist closure even if no writer has subscribed yet. All active and
        // queued writers wake, cancel their waits and release descriptor clones.
        self.closed.send_replace(true);
        self.fd
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    pub fn resize(&self, pty: &pb::PTY) -> io::Result<()> {
        let fd = self.descriptor()?;
        // Go explicitly casts uint32 sizes to uint16, including zero/overflow.
        let size = Winsize {
            ws_row: pty.size.rows as u16,
            ws_col: pty.size.cols as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the master remains owned throughout this synchronous ioctl;
        // TIOCSWINSZ reads exactly one initialized winsize from this pointer.
        if unsafe { nix::libc::ioctl(fd.get_ref().as_raw_fd(), nix::libc::TIOCSWINSZ, &size) } == -1
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub async fn write(&self, mut bytes: &[u8]) -> io::Result<()> {
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let writing = async {
            let _writing = self.writing.lock().await;
            let fd = self.descriptor()?;
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
        };
        tokio::select! {
            biased;
            _ = closed.changed() => Err(io::ErrorKind::BrokenPipe.into()),
            result = writing => result,
        }
    }
}

pub(super) struct Reader(pub Arc<Terminal>);

impl AsyncRead for Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let fd = match self.0.descriptor() {
            Ok(fd) => fd,
            Err(error) => return Poll::Ready(Err(error)),
        };
        loop {
            let mut ready = ready!(fd.poll_read_ready(cx))?;
            let result = ready.try_io(|fd| {
                nix::unistd::read(fd.get_ref().as_raw_fd(), buf.initialize_unfilled())
                    .map_err(io::Error::from)
            });
            match result {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                // Linux reports EIO on the master when the last slave closes.
                Ok(Err(error)) if error.raw_os_error() == Some(nix::libc::EIO) => {
                    return Poll::Ready(Ok(()))
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_) => continue,
            }
        }
    }
}
