//! Linux file operations behind the shared `POST /files/compose` handler.
//!
//! These functions were moved verbatim out of the former Linux-only daemon
//! module. They own the POSIX surface: `/dev/urandom`, `0666` temp creation,
//! chown of the temp file and created directories, `copy_file_range` with the
//! buffered fallback, descriptor close and lowercased errno text.
use crate::exec_context::Account;
use crate::platform::compose_files::FileId;
use crate::platform::file_writes;
use nix::unistd::{chown, close, Gid, Uid};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, IntoRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

/// Sixteen bytes from `/dev/urandom` for the temporary-file UUID v4.
pub fn random_bytes() -> io::Result<[u8; 16]> {
    let mut random = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    Ok(random)
}

/// Creates the temporary file exclusively with the oracle's `0666` mode.
pub fn create_new_temp(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o666)
        .open(path)
}

/// Lexically cleaned parent of a POSIX destination path.
pub fn parent(destination: &str) -> PathBuf {
    use std::path::Component;
    let parent =
        destination.rsplit_once('/').map_or(
            ".",
            |(parent, _)| if parent.is_empty() { "/" } else { parent },
        );
    let mut clean = PathBuf::new();
    for component in Path::new(parent).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match clean.components().next_back() {
                Some(Component::Normal(_)) => {
                    clean.pop();
                }
                Some(Component::RootDir) => {}
                _ => clean.push(".."),
            },
            component => clean.push(component.as_os_str()),
        }
    }
    if clean.as_os_str().is_empty() {
        clean.push(".")
    }
    clean
}

/// Creates every missing directory with the oracle's `0755` mode and adopts it
/// for `account`, preserving the exact failure detail strings.
pub fn ensure_dirs(path: &Path, account: &Account) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::metadata(&current) {
            Ok(info) if info.is_dir() => {}
            Ok(_) => return Err(format!("path is a file: {}", current.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::DirBuilder::new().mode(0o755).create(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(format!(
                            "failed to create directory: mkdir {}: {}",
                            current.display(),
                            errno_text(&error)
                        ))
                    }
                }
                chown(
                    &current,
                    Some(Uid::from_raw(account.uid)),
                    Some(Gid::from_raw(account.gid)),
                )
                .map_err(|error| {
                    format!(
                        "failed to chown directory: chown {}: {}",
                        current.display(),
                        error.desc().to_ascii_lowercase()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to stat directory: stat {}: {}",
                    current.display(),
                    errno_text(&error)
                ))
            }
        }
    }
    Ok(())
}

/// Adopts the temporary file. The caller renders the failure with
/// [`errno_text`], matching the oracle's `%v`.
pub fn adopt_owner(path: &Path, account: &Account) -> io::Result<()> {
    chown(
        path,
        Some(Uid::from_raw(account.uid)),
        Some(Gid::from_raw(account.gid)),
    )
    .map_err(|error| io::Error::from_raw_os_error(error as i32))
}

/// `os.SameFile` identity. `follow = false` is `Lstat`, so a destination
/// symlink is compared as the link entry itself.
pub fn identity(path: &Path, follow: bool) -> io::Result<FileId> {
    let metadata = if follow {
        fs::metadata(path)?
    } else {
        fs::symlink_metadata(path)?
    };
    Ok((metadata.dev(), metadata.ino()))
}

pub fn same_file(left: FileId, right: FileId) -> bool {
    left == right
}

/// `os.File.ReadFrom`: `copy_file_range` first, buffered fallback only for
/// kernels/filesystems that do not support the accelerated operation.
pub fn copy_file(
    input: &mut File,
    output: &mut File,
    source: &str,
    temporary: &Path,
) -> Result<(), (String, io::Error)> {
    loop {
        // SAFETY: both live File objects own valid descriptors; null offsets use
        // and advance those descriptors' current positions, as Go ReadFrom does.
        let copied = unsafe {
            nix::libc::copy_file_range(
                input.as_raw_fd(),
                std::ptr::null_mut(),
                output.as_raw_fd(),
                std::ptr::null_mut(),
                1 << 30,
                0,
            )
        };
        if copied > 0 {
            continue;
        }
        if copied == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(nix::libc::EINTR) => continue,
            Some(
                nix::libc::ENOSYS
                | nix::libc::EXDEV
                | nix::libc::EINVAL
                | nix::libc::EIO
                | nix::libc::EOPNOTSUPP
                | nix::libc::EPERM,
            ) => break,
            _ => {
                return Err((
                    format!("write {}: copy_file_range", temporary.display()),
                    error,
                ))
            }
        }
    }
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| (format!("read {source}"), error))?;
        if count == 0 {
            return Ok(());
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| (format!("write {}", temporary.display()), error))?;
    }
}

/// `File::drop` cannot report delayed close errors; the fd is transferred to
/// `close` so the caller can surface them.
pub fn close_output(file: File) -> io::Result<()> {
    close(file.into_raw_fd()).map_err(|error| io::Error::from_raw_os_error(error as i32))
}

/// Renames the finished temp over the destination, replacing it atomically.
pub fn rename_over(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

/// The oracle renders errno failures with the lowercased `Errno.Desc` text.
pub fn errno_text(error: &io::Error) -> String {
    error.raw_os_error().map_or_else(
        || error.to_string(),
        |code| {
            nix::errno::Errno::from_raw(code)
                .desc()
                .to_ascii_lowercase()
        },
    )
}

/// `ENOSPC`, the only disk-full condition compose special-cases.
pub fn is_no_space(error: &io::Error) -> bool {
    file_writes::is_no_space(error)
}
