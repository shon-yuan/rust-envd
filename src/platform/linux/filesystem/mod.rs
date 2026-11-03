//! Guest filesystem primitives matching runtime 51083a83 (envd 0.10.2).
//! Operations run with daemon privileges; Account selects path defaults and ownership.
//! These synchronous operations belong on a blocking executor at the RPC boundary.
//! Watchers expose real Linux inotify subscriptions and detached buffered state.
mod entry;
pub mod metadata;
pub(crate) mod paths;
pub mod watchers;

use crate::exec_context::{Account, Defaults};
use crate::proto::filesystem::EntryInfo;
use connectrpc::ConnectError;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct FilesystemRuntime {
    account: Account,
    defaults: Defaults,
}

impl FilesystemRuntime {
    pub fn new(account: Account, defaults: Defaults) -> Self {
        Self { account, defaults }
    }

    fn resolve(&self, supplied: &str) -> Result<PathBuf, ConnectError> {
        self.defaults
            .resolve_path(supplied, &self.account)
            .map(PathBuf::from)
            .map_err(|err| ConnectError::invalid_argument(err.to_string()))
    }

    pub fn stat(&self, path: &str) -> Result<EntryInfo, ConnectError> {
        entry::entry_info(&self.resolve(path)?)
    }

    pub fn make_dir(&self, path: &str) -> Result<EntryInfo, ConnectError> {
        let path = self.resolve(path)?;
        match fs::metadata(&path) {
            Ok(info) if info.is_dir() => {
                // Oracle dir.go:74: the refusal names the resolved path.
                return Err(ConnectError::already_exists(format!(
                    "directory already exists: {}",
                    path.display()
                )));
            }
            Ok(_) => {
                return Err(ConnectError::invalid_argument(format!(
                    "path already exists but it is not a directory: {}",
                    path.display()
                )));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(internal("error getting file info", err)),
        }
        paths::ensure_dirs(&path, &self.account)
            .map_err(|err| internal("failed to create directory", err))?;
        entry::entry_info(&path)
    }

    pub fn move_entry(&self, source: &str, destination: &str) -> Result<EntryInfo, ConnectError> {
        let source = self.resolve(source)?;
        let destination = self.resolve(destination)?;
        paths::ensure_dirs(
            destination.parent().unwrap_or(Path::new("/")),
            &self.account,
        )
        .map_err(|err| internal("failed to create destination parents", err))?;
        fs::rename(&source, &destination).map_err(|err| {
            // Oracle move.go:49-54: ENOENT is a NotFound that keeps the raw
            // os.Rename text; everything else stays internal with the same
            // "error renaming: rename a b: ..." wrap.
            if err.kind() == io::ErrorKind::NotFound {
                ConnectError::not_found(format!(
                    "source file not found: rename {} {}: {}",
                    source.display(),
                    destination.display(),
                    go_errno(&err)
                ))
            } else {
                ConnectError::internal(format!(
                    "error renaming: rename {} {}: {}",
                    source.display(),
                    destination.display(),
                    go_errno(&err)
                ))
            }
        })?;
        entry::entry_info(&destination)
    }

    pub fn list_dir(&self, path: &str, depth: u32) -> Result<Vec<EntryInfo>, ConnectError> {
        let requested = self.resolve(path)?;
        let resolved =
            paths::follow_symlinks(&requested).map_err(|err| match err.raw_os_error() {
                Some(nix::libc::ELOOP) => ConnectError::failed_precondition(format!(
                    // Oracle dir.go:110: %q quoting on the requested path.
                    "cyclic symlink or chain >255 links at {}",
                    go_quote(&requested.to_string_lossy())
                )),
                _ if err.kind() == io::ErrorKind::NotFound => ConnectError::not_found(format!(
                    "path not found: lstat {}: {}",
                    requested.display(),
                    go_errno(&err)
                )),
                _ => internal("error resolving symlink", err),
            })?;
        let info = fs::metadata(&resolved).map_err(|err| {
            ConnectError::internal(format!(
                "error getting file info: stat {}: {}",
                resolved.display(),
                go_errno(&err)
            ))
        })?;
        if !info.is_dir() {
            // Oracle dir.go:131: the resolved path is what the caller named.
            return Err(ConnectError::invalid_argument(format!(
                "path is not a directory: {}",
                resolved.display()
            )));
        }
        let mut entries = Vec::new();
        walk(&requested, &resolved, depth.max(1), &mut entries)?;
        Ok(entries)
    }

    pub fn remove(&self, path: &str) -> Result<(), ConnectError> {
        let path = self.resolve(path)?;
        let result = match fs::symlink_metadata(&path) {
            Ok(info) if info.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
            Err(err) => Err(err),
        };
        match result {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(internal("error removing file or directory", err)),
        }
    }
}

fn walk(
    requested: &Path,
    resolved: &Path,
    remaining: u32,
    entries: &mut Vec<EntryInfo>,
) -> Result<(), ConnectError> {
    let mut children = fs::read_dir(resolved)
        .map_err(|err| internal("error reading directory", err))?
        .collect::<io::Result<Vec<_>>>()
        .map_err(|err| internal("error reading directory", err))?;
    children.sort_by_key(|child| child.file_name());
    for child in children {
        let real_path = child.path();
        let alias_path = requested.join(child.file_name());
        let mut info = match entry::entry_info(&real_path) {
            Ok(info) => info,
            Err(err) if err.code == connectrpc::ErrorCode::NotFound => continue,
            Err(err) => return Err(internal("error reading directory", err)),
        };
        info.path = alias_path.to_string_lossy().into_owned();
        entries.push(info);
        let raw = match fs::symlink_metadata(&real_path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(internal("error reading directory", err)),
        };
        if remaining > 1 && raw.is_dir() {
            walk(&alias_path, &real_path, remaining - 1, entries)?;
        }
    }
    Ok(())
}

fn internal(context: &str, error: impl std::fmt::Display) -> ConnectError {
    ConnectError::internal(format!("{context}: {error}"))
}

/// Go syscall errno text ("no such file or directory"): the shared Go-string
/// translation, so the wrapped messages read exactly like the oracle's `%w`
/// text even for the errnos where nix's desc table diverges from Go.
fn go_errno(error: &io::Error) -> String {
    crate::errno_text::go_text(error)
}

/// Go's %q (strconv.Quote) for the path in "cyclic symlink ... at %q" — the
/// shared crate port ([`crate::goquote`]), so this rendering carries the same
/// \a\b\v\f and \uXXXX escapes as the compose scanner's refusals.
fn go_quote(text: &str) -> String {
    crate::goquote::quote(text)
}
