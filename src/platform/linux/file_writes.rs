//! Linux side of the `POST /files` write seam.
//!
//! The shared upload flow calls these helpers; this module is the only place
//! that knows about POSIX ownership, `0666` creation modes, errno numbers and
//! `user.e2b.*` extended attributes. The implementations delegate to the
//! existing filesystem primitives so the Linux parity path is unchanged.
use crate::exec_context::Account;
use crate::platform::linux::filesystem::{metadata, paths};
use nix::unistd::{chown, fchown, Gid, Uid};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Creates every missing parent with the oracle's `0755` mode and adopts the
/// new directories for `account`.
pub fn ensure_dirs(path: &Path, account: &Account) -> io::Result<()> {
    paths::ensure_dirs(path, account)
}

/// Adopts an existing path. `Ok(false)` is `ENOENT`: the entry vanished before
/// the change, and the caller adopts the opened file instead.
pub fn adopt_path_owner(path: &Path, account: &Account) -> io::Result<bool> {
    match chown(
        path,
        Some(Uid::from_raw(account.uid)),
        Some(Gid::from_raw(account.gid)),
    ) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ENOENT) => Ok(false),
        // Keep nix's errno text so the response matches the oracle's `%v`.
        Err(error) => Err(io::Error::other(error.to_string())),
    }
}

/// Adopts the file behind an open descriptor.
pub fn adopt_file_owner(file: &File, account: &Account) -> io::Result<()> {
    fchown(
        file.as_raw_fd(),
        Some(Uid::from_raw(account.uid)),
        Some(Gid::from_raw(account.gid)),
    )
    .map_err(|error| io::Error::other(error.to_string()))
}

/// Creates or truncates the target with the oracle's `0666` mode.
pub fn open_for_write(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o666)
        .open(path)
}

/// `os.SameFile`: device and inode identity, so hard links and symlinks to the
/// same file are caught before the second upload truncates the first.
pub fn same_file(left: &Path, right: &Path) -> bool {
    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

pub fn read_metadata(path: &Path) -> io::Result<HashMap<String, String>> {
    metadata::read_metadata(path)
}

pub fn write_metadata(file: &File, values: &HashMap<String, String>) -> io::Result<()> {
    metadata::write_metadata(file, values)
}

/// `EOPNOTSUPP`: the filesystem has no extended attributes, so the upload
/// degrades to a warning instead of failing.
pub fn metadata_unsupported(error: &io::Error) -> bool {
    metadata::is_unsupported(error)
}

/// `ENOSPC`.
pub fn is_no_space(error: &io::Error) -> bool {
    error.raw_os_error() == Some(nix::libc::ENOSPC)
}

/// `ENOSPC` or `EDQUOT` for the metadata write.
pub fn is_no_space_or_quota(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(nix::libc::ENOSPC | nix::libc::EDQUOT)
    )
}
