//! The Windows filesystem runtime behind the shared Connect adapter.
//!
//! Each operation reproduces the Linux runtime's validation order, Connect
//! codes and message shapes, but reads and mutates the filesystem with Win32
//! APIs. Value synthesis itself (mode, permissions, owner, group) stays in
//! [`super::fs`]; this module owns path resolution, the directory walk, the
//! symlink chain and the error-text table.
//!
//! Declared deviations (also in `docs/windows-port.md`):
//! - returned paths use native `\` separators;
//! - `EntryInfo.metadata` is always empty;
//! - `Move` refuses to replace an existing directory;
//! - `Remove` clears `FILE_ATTRIBUTE_READONLY` once before retrying, which
//!   mutates metadata the Linux path never touches;
//! - directory symlinks and junctions are unlinked with `RemoveDirectory`.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use connectrpc::ConnectError;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_DIRECTORY, ERROR_DIR_NOT_EMPTY,
    ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INVALID_NAME, ERROR_NOT_SAME_DEVICE,
    ERROR_PATH_NOT_FOUND,
};
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, SetFileAttributesW, FILE_ATTRIBUTE_READONLY, MOVEFILE_REPLACE_EXISTING,
};

use crate::exec_context::{Account, Defaults};
use crate::platform::windows::fs as wfs;
use crate::proto::filesystem::EntryInfo;

/// Go `path/filepath.EvalSymlinks` follows at most 255 links on the Windows
/// port; the failure is reported as `failed_precondition`, not `ELOOP`.
const MAX_LINKS: usize = 255;

pub struct FilesystemRuntime {
    account: Account,
    defaults: Defaults,
}

impl FilesystemRuntime {
    pub fn new(account: Account, defaults: Defaults) -> Self {
        Self { account, defaults }
    }

    fn resolve(&self, supplied: &str) -> Result<PathBuf, ConnectError> {
        let resolved = self
            .defaults
            .resolve_path(supplied, &self.account)
            .map_err(|err| ConnectError::invalid_argument(err.to_string()))?;
        // The seam resolves with `/` joins. Every value this runtime returns
        // (entry paths, error messages) uses the platform separator.
        Ok(PathBuf::from(resolved.replace('/', "\\")))
    }

    pub fn stat(&self, path: &str) -> Result<EntryInfo, ConnectError> {
        let path = self.resolve(path)?;
        stat_entry(&path)
    }

    pub fn make_dir(&self, path: &str) -> Result<EntryInfo, ConnectError> {
        let path = self.resolve(path)?;
        match fs::metadata(&path) {
            Ok(info) if info.is_dir() => {
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
            Err(err) => {
                return Err(internal(format!(
                    "error getting file info: {}",
                    win32_error_text(&err)
                )));
            }
        }
        crate::platform::file_writes::ensure_dirs(&path, &self.account).map_err(|err| {
            internal(format!(
                "failed to create directory: {}",
                win32_error_text(&err)
            ))
        })?;
        stat_entry(&path)
    }

    pub fn move_entry(&self, source: &str, destination: &str) -> Result<EntryInfo, ConnectError> {
        let source = self.resolve(source)?;
        let destination = self.resolve(destination)?;
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(""));
        // Oracle move.go creates the destination parents BEFORE it examines
        // the source: a missing source still leaves the directories behind.
        crate::platform::file_writes::ensure_dirs(parent, &self.account).map_err(|err| {
            internal(format!(
                "failed to create destination parents: {}",
                win32_error_text(&err)
            ))
        })?;
        move_file(&source, &destination).map_err(|err| {
            let code = err.raw_os_error().unwrap_or(0) as u32;
            if code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND {
                ConnectError::not_found(format!(
                    "source file not found: rename {} {}: no such file or directory",
                    source.display(),
                    destination.display()
                ))
            } else {
                internal(format!(
                    "error renaming: rename {} {}: {}",
                    source.display(),
                    destination.display(),
                    win32_error_text(&err)
                ))
            }
        })?;
        stat_entry(&destination)
    }

    pub fn list_dir(&self, path: &str, depth: u32) -> Result<Vec<EntryInfo>, ConnectError> {
        let requested = self.resolve(path)?;
        let resolved = follow_symlinks(&requested).map_err(|error| match error {
            LinkError::Cycle => ConnectError::failed_precondition(format!(
                "cyclic symlink or chain >255 links at {}",
                crate::goquote::quote(&requested.to_string_lossy())
            )),
            LinkError::NotFound => ConnectError::not_found(format!(
                "path not found: lstat {}: no such file or directory",
                requested.display()
            )),
            LinkError::Io(err) => internal(format!(
                "error resolving symlink: {}",
                win32_error_text(&err)
            )),
        })?;
        let info = fs::metadata(&resolved).map_err(|err| {
            internal(format!(
                "error getting file info: stat {}: {}",
                resolved.display(),
                win32_error_text(&err)
            ))
        })?;
        if !info.is_dir() {
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
        match wfs::remove_entry(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                // Linux removes directories through their parent's permission;
                // Win32 additionally refuses read-only children. Clear the
                // flag once and retry, mutating metadata Linux never touches.
                clear_read_only(&path);
                match wfs::remove_entry(&path) {
                    Ok(()) => Ok(()),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(err) => Err(internal(format!(
                        "error removing file or directory: {}",
                        win32_error_text(&err)
                    ))),
                }
            }
            Err(err) => Err(internal(format!(
                "error removing file or directory: {}",
                win32_error_text(&err)
            ))),
        }
    }
}

impl crate::platform::FilesystemService for FilesystemRuntime {
    fn stat(&self, path: &str) -> Result<EntryInfo, ConnectError> {
        FilesystemRuntime::stat(self, path)
    }

    fn make_dir(&self, path: &str) -> Result<EntryInfo, ConnectError> {
        FilesystemRuntime::make_dir(self, path)
    }

    fn move_entry(&self, source: &str, destination: &str) -> Result<EntryInfo, ConnectError> {
        FilesystemRuntime::move_entry(self, source, destination)
    }

    fn list_dir(&self, path: &str, depth: u32) -> Result<Vec<EntryInfo>, ConnectError> {
        FilesystemRuntime::list_dir(self, path, depth)
    }

    fn remove(&self, path: &str) -> Result<(), ConnectError> {
        FilesystemRuntime::remove(self, path)
    }
}

fn stat_entry(path: &Path) -> Result<EntryInfo, ConnectError> {
    wfs::entry_info(path).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            ConnectError::not_found(format!(
                "file not found: lstat {}: no such file or directory",
                path.display()
            ))
        } else {
            internal(format!(
                "error getting file info: {}",
                win32_error_text(&err)
            ))
        }
    })
}

fn move_file(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide(source);
    let destination = wide(destination);
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn walk(
    requested: &Path,
    resolved: &Path,
    remaining: u32,
    entries: &mut Vec<EntryInfo>,
) -> Result<(), ConnectError> {
    let mut children = fs::read_dir(resolved)
        .map_err(|err| {
            internal(format!(
                "error reading directory: {}",
                win32_error_text(&err)
            ))
        })?
        .collect::<io::Result<Vec<_>>>()
        .map_err(|err| {
            internal(format!(
                "error reading directory: {}",
                win32_error_text(&err)
            ))
        })?;
    children.sort_by_key(|child| child.file_name());
    for child in children {
        let real_path = child.path();
        let alias_path = requested.join(child.file_name());
        let mut info = match wfs::entry_info(&real_path) {
            Ok(info) => info,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(internal(format!(
                    "error reading directory: {}",
                    win32_error_text(&err)
                )));
            }
        };
        info.path = alias_path.to_string_lossy().into_owned();
        entries.push(info);
        let raw = match fs::symlink_metadata(&real_path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(internal(format!(
                    "error reading directory: {}",
                    win32_error_text(&err)
                )));
            }
        };
        // Only a real directory is walked. A symlink or junction is reported
        // as an entry and never followed by the recursive walk.
        if remaining > 1 && raw.is_dir() && !wfs::is_link(&real_path, &raw) {
            walk(&alias_path, &real_path, remaining - 1, entries)?;
        }
    }
    Ok(())
}

#[derive(Clone)]
enum Part {
    Prefix(OsString),
    Root(OsString),
    Cur,
    Parent,
    Normal(OsString),
}

impl Part {
    fn from_component(component: Component<'_>) -> Self {
        match component {
            Component::Prefix(prefix) => Part::Prefix(prefix.as_os_str().to_owned()),
            Component::RootDir => Part::Root(component.as_os_str().to_owned()),
            Component::CurDir => Part::Cur,
            Component::ParentDir => Part::Parent,
            Component::Normal(name) => Part::Normal(name.to_owned()),
        }
    }
}

enum LinkError {
    NotFound,
    Cycle,
    Io(io::Error),
}

/// Windows `EvalSymlinks`: a reparse point is read with `read_link`; an
/// absolute target resets the accumulated path, a relative one is prepended to
/// the pending components. The 255-link cap matches the Go filepath walk.
fn follow_symlinks(path: &Path) -> Result<PathBuf, LinkError> {
    let mut pending: VecDeque<Part> = path.components().map(Part::from_component).collect();
    let mut resolved = PathBuf::new();
    let mut links = 0usize;
    while let Some(part) = pending.pop_front() {
        match part {
            Part::Prefix(prefix) => resolved.push(prefix),
            Part::Root(root) => resolved.push(root),
            Part::Cur => {}
            Part::Parent => {
                resolved.pop();
            }
            Part::Normal(name) => {
                resolved.push(&name);
                let info = fs::symlink_metadata(&resolved).map_err(|err| {
                    if err.kind() == io::ErrorKind::NotFound {
                        LinkError::NotFound
                    } else {
                        LinkError::Io(err)
                    }
                })?;
                if wfs::is_link(&resolved, &info) {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(LinkError::Cycle);
                    }
                    let target = fs::read_link(&resolved).map_err(LinkError::Io)?;
                    resolved.pop();
                    if target.is_absolute() {
                        // Feed an absolute target back through the queue, as
                        // the Linux walk does: every component is rebuilt from
                        // its prefix/root and the leaf is stat-ed and counted.
                        resolved = PathBuf::new();
                    }
                    for component in target.components().rev() {
                        pending.push_front(Part::from_component(component));
                    }
                } else if !pending.is_empty() && !info.is_dir() {
                    return Err(LinkError::Io(io::Error::from_raw_os_error(
                        ERROR_DIRECTORY as i32,
                    )));
                }
            }
        }
    }
    Ok(resolved)
}

/// Clears `FILE_ATTRIBUTE_READONLY` on a failed removal, children first. It is
/// best effort: the retry reports the real outcome.
fn clear_read_only(path: &Path) {
    let Ok(info) = fs::symlink_metadata(path) else {
        return;
    };
    if wfs::is_link(path, &info) {
        return;
    }
    if info.is_dir() {
        if let Ok(children) = fs::read_dir(path) {
            for child in children.flatten() {
                clear_read_only(&child.path());
            }
        }
    }
    let attributes = info.file_attributes();
    if attributes & FILE_ATTRIBUTE_READONLY != 0 {
        let wide = wide(path);
        unsafe {
            SetFileAttributesW(wide.as_ptr(), attributes & !FILE_ATTRIBUTE_READONLY);
        }
    }
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn internal(message: String) -> ConnectError {
    ConnectError::internal(message)
}

/// Translates the common Win32 failures to the Go/errno text the oracle
/// wraps into its messages. Unknown codes fall back to the OS message.
pub(crate) fn win32_error_text(error: &io::Error) -> String {
    let Some(code) = error.raw_os_error() else {
        return error.to_string();
    };
    win32_code_text(code as u32)
}

pub(crate) fn win32_code_text(code: u32) -> String {
    let text = match code {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => "no such file or directory",
        ERROR_ACCESS_DENIED => "permission denied",
        ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS => "file exists",
        ERROR_DIR_NOT_EMPTY => "directory not empty",
        ERROR_NOT_SAME_DEVICE => "invalid cross-device link",
        ERROR_DIRECTORY => "not a directory",
        ERROR_INVALID_NAME => "invalid argument",
        _ => return io::Error::from_raw_os_error(code as i32).to_string(),
    };
    text.to_owned()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::proto::filesystem::FileType;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "coorworker-envd-runtime-{}-{unique}-{label}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        fn runtime(&self) -> FilesystemRuntime {
            let account = Account {
                name: crate::platform::windows::account::current_user_name(),
                home: self.0.to_string_lossy().into_owned(),
                uid: 0,
                gid: 0,
                groups: Vec::new(),
            };
            let defaults = Defaults {
                user: account.name.clone(),
                workdir: None,
                user_delivered: true,
            };
            FilesystemRuntime::new(account, defaults)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Depth-first and reparse-point safe, so a test that leaves a
            // junction behind cannot delete its target.
            let _ = wfs::remove_entry(&self.0);
        }
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Creates a directory junction with `mklink /J`, the only link kind an
    /// ordinary Windows account can create without Developer Mode.
    fn junction(link: &Path, target: &Path) -> bool {
        let command = format!(r#"mklink /J "{}" "{}""#, link.display(), target.display());
        std::process::Command::new("cmd")
            .args(["/C", &command])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn stat_synthesizes_mode_permissions_and_never_empty_owner_group() {
        let scratch = Scratch::new("stat");
        let path = scratch.join("hello.txt");
        fs::write(&path, b"hello").expect("write");
        let entry = scratch.runtime().stat(&text(&path)).expect("stat");
        assert_eq!(entry.name, "hello.txt");
        assert_eq!(entry.r#type, FileType::File);
        assert_eq!(entry.size, 5);
        assert_eq!(entry.mode, 0o644);
        assert_eq!(entry.permissions, "-rw-r--r--");
        assert!(!entry.is_symlink);
        assert!(!entry.owner.is_empty());
        assert!(!entry.group.is_empty());
        assert!(entry.metadata.is_empty());
        assert!(entry.modified_time.seconds > 0);
    }

    #[test]
    fn move_replaces_existing_file_and_keeps_parent_side_effect() {
        let scratch = Scratch::new("move");
        let runtime = scratch.runtime();
        fs::write(scratch.join("src"), b"payload").expect("src");
        fs::write(scratch.join("dst"), b"old").expect("dst");
        let entry = runtime
            .move_entry(&text(&scratch.join("src")), &text(&scratch.join("dst")))
            .expect("move");
        assert_eq!(entry.name, "dst");
        assert_eq!(fs::read(scratch.join("dst")).expect("read"), b"payload");
        assert!(!scratch.join("src").exists());

        let missing = runtime
            .move_entry(
                &text(&scratch.join("absent")),
                &text(&scratch.join("created/dst")),
            )
            .expect_err("missing source");
        assert_eq!(missing.code, connectrpc::ErrorCode::NotFound);
        assert!(scratch.join("created").is_dir());
    }

    #[test]
    fn remove_unlinks_a_directory_junction_without_deleting_the_target() {
        let scratch = Scratch::new("junction");
        let target = scratch.join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("keep"), b"keep").expect("keep");
        let link = scratch.join("link");
        if !junction(&link, &target) {
            // Junction creation can be unavailable on a locked-down runner.
            return;
        }
        scratch
            .runtime()
            .remove(&text(&link))
            .expect("remove junction");
        assert!(!link.exists());
        assert!(target.join("keep").exists());
    }

    #[test]
    fn list_dir_walk_skips_symlink_children_and_keeps_depth_order() {
        let scratch = Scratch::new("walk");
        let outside = Scratch::new("walk-target");
        fs::create_dir_all(scratch.join("a/inner")).expect("tree");
        fs::write(scratch.join("a/inner/leaf"), b"x").expect("leaf");
        fs::create_dir(scratch.join("z")).expect("z");
        let target = outside.join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("hidden"), b"x").expect("hidden");
        let link = scratch.join("link");
        if !junction(&link, &target) {
            return;
        }
        let entries = scratch
            .runtime()
            .list_dir(&text(&scratch.0), 3)
            .expect("list");
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["a", "inner", "leaf", "link", "z"]);
        assert!(!names.contains(&"hidden"));
        assert!(target.join("hidden").exists());
        assert!(entries
            .iter()
            .all(|entry| entry.path.starts_with(&text(&scratch.0))));
    }

    #[test]
    fn win32_error_text_maps_not_found_access_denied_exists_not_empty_and_cross_device() {
        let message = |code| win32_error_text(&io::Error::from_raw_os_error(code as i32));
        assert_eq!(message(ERROR_FILE_NOT_FOUND), "no such file or directory");
        assert_eq!(message(ERROR_PATH_NOT_FOUND), "no such file or directory");
        assert_eq!(message(ERROR_ACCESS_DENIED), "permission denied");
        assert_eq!(message(ERROR_ALREADY_EXISTS), "file exists");
        assert_eq!(message(ERROR_FILE_EXISTS), "file exists");
        assert_eq!(message(ERROR_DIR_NOT_EMPTY), "directory not empty");
        assert_eq!(message(ERROR_NOT_SAME_DEVICE), "invalid cross-device link");
        assert_eq!(message(ERROR_DIRECTORY), "not a directory");
    }
}
