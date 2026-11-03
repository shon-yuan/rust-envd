//! Win32 filesystem metadata, mutation, and watcher implementation; P1.
//!
//! Metadata is synthesized as documented in `docs/windows-port.md` §5.2:
//! `mode` comes from the read-only attribute and the entry kind, `owner` and
//! `group` from the object's security descriptor, and `symlink_target` from
//! the resolved reparse point. NTFS extended attributes are not read yet, so
//! `metadata` is an empty map (the documented "otherwise empty" case), never
//! an error.
//!
//! Watchers still return [`std::io::ErrorKind::Unsupported`]; they land with
//! the `ReadDirectoryChangesW` slice.

use std::ffi::c_void;
use std::fs::{self, Metadata};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use buffa_types::google::protobuf::Timestamp;
use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    LookupAccountSidW, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE,
};

use crate::platform::{FsBackend, WatchId, WatcherBackend};
use crate::proto::filesystem::{EntryInfo, FileType, FilesystemEvent};

/// `FILE_ATTRIBUTE_READONLY` from `winnt.h`.
const FILE_ATTRIBUTE_READONLY: u32 = 0x1;

/// Fallback account name when the security descriptor cannot be read.
const UNKNOWN_ACCOUNT: &str = "None";

#[derive(Debug, Default)]
pub struct WindowsFsBackend;

impl FsBackend for WindowsFsBackend {
    fn stat(&self, path: &Path) -> io::Result<EntryInfo> {
        entry_info(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<EntryInfo>> {
        let mut children = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(|child| child.file_name());
        let mut entries = Vec::with_capacity(children.len());
        for child in children {
            // A child can disappear between enumeration and stat; the shared
            // directory walk skips those entries the same way.
            match entry_info(&child.path()) {
                Ok(info) => entries.push(info),
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            }
        }
        Ok(entries)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let result = match fs::symlink_metadata(path) {
            Ok(info) if info.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
            Err(err) => Err(err),
        };
        match result {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
}

#[derive(Debug, Default)]
pub struct WindowsWatcherBackend;

impl WatcherBackend for WindowsWatcherBackend {
    fn watch(&self, _path: &Path) -> io::Result<WatchId> {
        Err(super::unsupported("filesystem watcher"))
    }

    fn unwatch(&self, _id: WatchId) -> io::Result<()> {
        Err(super::unsupported("filesystem watcher removal"))
    }

    fn events(&self, _id: WatchId) -> io::Result<Vec<FilesystemEvent>> {
        Err(super::unsupported("filesystem watcher events"))
    }
}

fn entry_info(path: &Path) -> io::Result<EntryInfo> {
    let raw = fs::symlink_metadata(path)?;
    let is_symlink = raw.file_type().is_symlink();
    // Follow the link for the effective type, as the Linux entry helper does;
    // a dangling link keeps the raw metadata and reports Symlink.
    let target = if is_symlink {
        fs::metadata(path).ok()
    } else {
        None
    };
    let effective = target.as_ref().unwrap_or(&raw);
    let mode = synthesized_mode(&raw, effective);
    let (owner, group) = owner_and_group(path);
    Ok(EntryInfo {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        path: path.to_string_lossy().into_owned(),
        r#type: file_type(effective).into(),
        size: raw.file_size() as i64,
        mode,
        permissions: permissions(mode, &raw),
        owner,
        group,
        modified_time: modified_time(&raw).into(),
        symlink_target: is_symlink.then(|| resolved_target(path)),
        metadata: Default::default(),
        is_symlink,
        ..Default::default()
    })
}

fn file_type(info: &Metadata) -> FileType {
    let kind = info.file_type();
    if kind.is_file() {
        FileType::File
    } else if kind.is_dir() {
        FileType::Directory
    } else if kind.is_symlink() {
        FileType::Symlink
    } else {
        FileType::Unspecified
    }
}

/// Windows has no POSIX mode: synthesize one from the read-only attribute and
/// the entry kind, as documented. Symlinks report `0777`, matching a Linux
/// symlink's own mode, and the value is never zero.
fn synthesized_mode(raw: &Metadata, effective: &Metadata) -> u32 {
    if raw.file_type().is_symlink() {
        return 0o777;
    }
    let read_only = raw.file_attributes() & FILE_ATTRIBUTE_READONLY != 0;
    match (effective.is_dir(), read_only) {
        (true, true) => 0o555,
        (true, false) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    }
}

/// Renders the synthesized mode in Go `FileMode.String` form, the same shape
/// the Linux entry helper produces (`-rw-r--r--`, `drwxr-xr-x`, `Lrwxrwxrwx`).
fn permissions(mode: u32, info: &Metadata) -> String {
    let kind = info.file_type();
    let mut result = String::new();
    if kind.is_dir() {
        result.push('d');
    }
    if kind.is_symlink() {
        result.push('L');
    }
    if result.is_empty() {
        result.push('-');
    }
    for (mask, flag) in [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ] {
        result.push(if mode & mask != 0 { flag } else { '-' });
    }
    result
}

fn modified_time(info: &Metadata) -> Timestamp {
    let duration = info
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    Timestamp {
        seconds: duration.as_secs() as i64,
        nanos: duration.subsec_nanos() as i32,
        ..Default::default()
    }
}

/// The resolved reparse-point target with the `\\?\` verbatim prefix removed,
/// so the value reads like a normal Windows path.
fn resolved_target(path: &Path) -> String {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim(&target).to_string_lossy().into_owned()
}

fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Reads the object's owner and primary-group SIDs and resolves them to
/// `DOMAIN\name`. Falls back to the SID string, then to `None`, so the value
/// is never empty.
fn owner_and_group(path: &Path) -> (String, String) {
    let wide = wide_path(path);
    let mut owner: PSID = std::ptr::null_mut();
    let mut group: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION,
            &mut owner,
            &mut group,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    let (owner_name, group_name) = if status == ERROR_SUCCESS {
        (
            account_or_sid(owner).unwrap_or_else(|| UNKNOWN_ACCOUNT.to_string()),
            account_or_sid(group).unwrap_or_else(|| UNKNOWN_ACCOUNT.to_string()),
        )
    } else {
        (UNKNOWN_ACCOUNT.to_string(), UNKNOWN_ACCOUNT.to_string())
    };
    if !descriptor.is_null() {
        unsafe {
            LocalFree(descriptor as HLOCAL);
        }
    }
    (owner_name, group_name)
}

fn account_or_sid(sid: PSID) -> Option<String> {
    account_name(sid).or_else(|| sid_string(sid))
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn account_name(sid: PSID) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut name_len = 0u32;
    let mut domain_len = 0u32;
    let mut use_kind: SID_NAME_USE = 0;
    unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            std::ptr::null_mut(),
            &mut name_len,
            std::ptr::null_mut(),
            &mut domain_len,
            &mut use_kind,
        );
    }
    if name_len == 0 {
        return None;
    }
    let mut name = vec![0u16; name_len as usize];
    let mut domain = vec![0u16; domain_len.max(1) as usize];
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut use_kind,
        )
    };
    if ok == 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&name[..name_len as usize]);
    let domain = String::from_utf16_lossy(&domain[..domain_len as usize]);
    if domain.is_empty() {
        Some(name)
    } else {
        Some(format!("{domain}\\{name}"))
    }
}

fn sid_string(sid: PSID) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut text: *mut u16 = std::ptr::null_mut();
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut text) };
    if ok == 0 || text.is_null() {
        return None;
    }
    let mut len = 0usize;
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
    }
    let result = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    unsafe {
        LocalFree(text as *mut c_void as HLOCAL);
    }
    Some(result)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "coorworker-envd-fs-{}-{unique}-{label}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn stat_reports_file_metadata() {
        let scratch = Scratch::new("stat");
        let path = scratch.join("hello.txt");
        fs::write(&path, b"hello").expect("write");

        let info = WindowsFsBackend.stat(&path).expect("stat");
        assert_eq!(info.name, "hello.txt");
        assert_eq!(info.r#type, FileType::File as i32);
        assert_eq!(info.size, 5);
        assert_eq!(info.mode, 0o644);
        assert_eq!(info.permissions, "-rw-r--r--");
        assert!(!info.is_symlink);
        assert!(!info.owner.is_empty());
        assert!(!info.group.is_empty());
        assert!(info.symlink_target.is_none());
        assert!(info.modified_time.seconds > 0);
    }

    #[test]
    fn read_only_file_reports_a_read_only_mode() {
        let scratch = Scratch::new("readonly");
        let path = scratch.join("locked.txt");
        fs::write(&path, b"x").expect("write");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).expect("set readonly");

        let info = WindowsFsBackend.stat(&path).expect("stat");
        assert_eq!(info.mode, 0o444);
        assert_eq!(info.permissions, "-r--r--r--");
    }

    #[test]
    fn read_dir_lists_children_in_name_order() {
        let scratch = Scratch::new("list");
        fs::write(scratch.join("b.txt"), b"b").expect("write");
        fs::write(scratch.join("a.txt"), b"a").expect("write");

        let entries = WindowsFsBackend.read_dir(&scratch.0).expect("read_dir");
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.txt"]);
    }

    #[test]
    fn create_dir_and_remove_round_trip() {
        let scratch = Scratch::new("mkdir");
        let nested = scratch.join("one").join("two");
        WindowsFsBackend.create_dir(&nested).expect("create_dir");

        let info = WindowsFsBackend.stat(&nested).expect("stat");
        assert_eq!(info.r#type, FileType::Directory as i32);
        assert_eq!(info.mode, 0o755);
        assert_eq!(info.permissions, "drwxr-xr-x");

        WindowsFsBackend
            .remove(&scratch.join("one"))
            .expect("remove");
        assert!(!scratch.join("one").exists());
        // Removing a missing entry is a no-op, matching the Linux runtime.
        WindowsFsBackend
            .remove(&scratch.join("one"))
            .expect("remove missing");
    }

    #[test]
    fn rename_moves_an_entry() {
        let scratch = Scratch::new("rename");
        let from = scratch.join("from.txt");
        let to = scratch.join("to.txt");
        let mut file = fs::File::create(&from).expect("create");
        file.write_all(b"moved").expect("write");
        drop(file);

        WindowsFsBackend.rename(&from, &to).expect("rename");
        assert!(!from.exists());
        assert_eq!(fs::read(&to).expect("read"), b"moved");
    }
}
