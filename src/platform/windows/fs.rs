//! Win32 filesystem metadata, mutation, and watcher implementation; P1.
//!
//! Metadata is synthesized as documented in `docs/windows-port.md` §5.2:
//! `mode` comes from the read-only attribute and the entry kind, `owner` and
//! `group` from the object's security descriptor, and `symlink_target` from
//! the resolved reparse point. NTFS extended attributes are not read yet, so
//! `metadata` is an empty map (the documented "otherwise empty" case), never
//! an error.
//!
//! The Connect watcher surface lives in `watchers.rs` over
//! `ReadDirectoryChangesW`.

use std::ffi::c_void;
use std::fs::{self, Metadata};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use buffa_types::google::protobuf::Timestamp;
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_SUCCESS, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    LookupAccountSidW, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileAttributeTagInfo, GetFileInformationByHandleEx, SetFileAttributesW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
};

use crate::platform::FsBackend;
use crate::proto::filesystem::{EntryInfo, FileType};

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
        match remove_entry(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
}

/// True only for a reparse point that names another entry: an NTFS symlink or
/// a directory junction/mount point. `FILE_ATTRIBUTE_REPARSE_POINT` alone is
/// not enough — cloud placeholders, deduplication and AppExecLink entries also
/// carry it — so the tag decides. If the tag cannot be read the entry is
/// treated as a link, which is the safe answer for removal and traversal.
/// `symlink_metadata` never follows a reparse point, so the lookups below
/// inspect the link itself.
pub(crate) fn is_link(path: &Path, info: &Metadata) -> bool {
    if info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return false;
    }
    match reparse_tag(path) {
        Some(tag) => tag == IO_REPARSE_TAG_SYMLINK || tag == IO_REPARSE_TAG_MOUNT_POINT,
        None => true,
    }
}

/// Reads the reparse tag without following the entry. `None` means the tag
/// could not be queried (for example the entry vanished or access was denied).
fn reparse_tag(path: &Path) -> Option<u32> {
    let wide = wide_path(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    unsafe {
        CloseHandle(handle);
    }
    (ok != 0).then_some(info.ReparseTag)
}

/// True when the *link itself* names a directory, without consulting
/// `FileType::is_dir` (which excludes symlinks on Windows).
fn is_directory(info: &Metadata) -> bool {
    info.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
}

/// Deletes an entry without following a symlink or junction. The recursion is
/// explicit and depth-first: every child is inspected with `symlink_metadata`,
/// a directory reparse point is unlinked with `RemoveDirectory` (which deletes
/// only the link, never the target's contents), and the walk never descends
/// into a reparse point. Read-only regular files have
/// `FILE_ATTRIBUTE_READONLY` cleared before `DeleteFile` so a read-only
/// attribute cannot mask the real outcome. `fs::remove_dir_all` is not used:
/// its reparse-point handling is what this module must not depend on.
pub(crate) fn remove_entry(path: &Path) -> io::Result<()> {
    let info = fs::symlink_metadata(path)?;
    remove_entry_with_metadata(path, &info)
}

fn remove_entry_with_metadata(path: &Path, info: &Metadata) -> io::Result<()> {
    if is_link(path, info) {
        return if is_directory(info) {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        };
    }
    if is_directory(info) {
        for child in fs::read_dir(path)? {
            let child = child?;
            let child_path = child.path();
            let child_info = fs::symlink_metadata(&child_path)?;
            remove_entry_with_metadata(&child_path, &child_info)?;
        }
        fs::remove_dir(path)
    } else {
        clear_read_only_attribute(path, info)?;
        fs::remove_file(path)
    }
}

/// Clears `FILE_ATTRIBUTE_READONLY` on a regular file before deleting it.
/// Removal still reports the real Win32 outcome if the attribute cannot be
/// changed.
fn clear_read_only_attribute(path: &Path, info: &Metadata) -> io::Result<()> {
    let attributes = info.file_attributes();
    if attributes & FILE_ATTRIBUTE_READONLY == 0 {
        return Ok(());
    }
    let wide = wide_path(path);
    let ok = unsafe { SetFileAttributesW(wide.as_ptr(), attributes & !FILE_ATTRIBUTE_READONLY) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn entry_info(path: &Path) -> io::Result<EntryInfo> {
    let raw = fs::symlink_metadata(path)?;
    let is_symlink = is_link(path, &raw);
    // Follow the link for the effective type, as the Linux entry helper does.
    // A dangling link has no effective metadata at all: Linux reports
    // `FILE_TYPE_UNSPECIFIED`/mode 0, never the link's own metadata.
    let target = if is_symlink {
        fs::metadata(path).ok()
    } else {
        None
    };
    let effective = if is_symlink {
        target.as_ref()
    } else {
        Some(&raw)
    };
    let full_mode = synthesized_mode(path, &raw, effective);
    let mode = if effective.is_some() { full_mode } else { 0 };
    let (owner, group) = owner_and_group(path);
    Ok(EntryInfo {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        path: path.to_string_lossy().into_owned(),
        r#type: effective
            .map(file_type)
            .unwrap_or(FileType::Unspecified)
            .into(),
        size: entry_size(path, &raw, is_symlink),
        mode,
        permissions: permissions(path, full_mode, &raw),
        owner,
        group,
        modified_time: modified_time(&raw).into(),
        symlink_target: is_symlink.then(|| resolved_target(path)),
        metadata: Default::default(),
        is_symlink,
        ..Default::default()
    })
}

/// Linux `lstat` reports a symlink's own size as the number of bytes in its
/// stored target path, not the target's size and never zero merely because the
/// entry is a link. Win32 reports a logical size of 0 for the link itself, so
/// use the UTF-16 byte length of the reparse target that `read_link` returns
/// (the closest analogue of the stored path); fall back to the file size if
/// the target cannot be read.
fn entry_size(path: &Path, raw: &Metadata, is_symlink: bool) -> i64 {
    if is_symlink {
        if let Ok(target) = fs::read_link(path) {
            return (target.as_os_str().encode_wide().count() * 2) as i64;
        }
    }
    raw.file_size() as i64
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
/// the entry kind, as documented. Links report `0777` for `permissions`,
/// matching a Linux symlink's own mode. Passing `None` for `effective` (a
/// dangling link) makes the caller report mode `0`.
fn synthesized_mode(path: &Path, raw: &Metadata, effective: Option<&Metadata>) -> u32 {
    if is_link(path, raw) {
        return 0o777;
    }
    let read_only = raw.file_attributes() & FILE_ATTRIBUTE_READONLY != 0;
    let directory = effective.map_or_else(|| is_directory(raw), is_directory);
    match (directory, read_only) {
        (true, true) => 0o555,
        (true, false) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    }
}

/// Renders the synthesized mode in Go `FileMode.String` form, the same shape
/// the Linux entry helper produces (`-rw-r--r--`, `drwxr-xr-x`, `Lrwxrwxrwx`).
fn permissions(path: &Path, mode: u32, info: &Metadata) -> String {
    let kind = info.file_type();
    let link = is_link(path, info);
    let mut result = String::new();
    if kind.is_dir() && !link {
        result.push('d');
    }
    if link {
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
            // Use the module's depth-first delete so a test that leaves a
            // junction or a read-only file behind still cleans up safely.
            let _ = remove_entry(&self.0);
        }
    }

    /// Creates a directory junction with `mklink /J`, the only link kind an
    /// ordinary Windows account can create without Developer Mode. Returns
    /// false when the runner refuses, so link tests skip cleanly.
    fn junction(link: &Path, target: &Path) -> bool {
        let command = format!(r#"mklink /J "{}" "{}""#, link.display(), target.display());
        std::process::Command::new("cmd")
            .args(["/C", &command])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn reparse_point_directory_is_classified_as_a_link() {
        let scratch = Scratch::new("junction-predicate");
        let target = scratch.join("target");
        fs::create_dir(&target).expect("target");
        let link = scratch.join("link");
        if !junction(&link, &target) {
            return;
        }
        let info = fs::symlink_metadata(&link).expect("link metadata");
        assert!(is_link(&link, &info));
        remove_entry(&link).expect("unlink junction");
    }

    #[test]
    fn removing_a_tree_does_not_follow_a_junction() {
        let scratch = Scratch::new("tree-junction");
        let target = scratch.join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("sentinel"), b"keep").expect("sentinel");
        let tree = scratch.join("tree");
        fs::create_dir(&tree).expect("tree");
        let link = tree.join("junction");
        if !junction(&link, &target) {
            return;
        }

        remove_entry(&tree).expect("remove tree");

        assert!(!tree.exists());
        assert!(
            target.join("sentinel").exists(),
            "the junction target must not be deleted by the containing tree"
        );
    }

    #[test]
    fn dangling_directory_link_reports_unspecified_type_and_zero_mode() {
        let scratch = Scratch::new("dangling");
        let target = scratch.join("gone-target");
        fs::create_dir(&target).expect("target");
        let link = scratch.join("link");
        if !junction(&link, &target) {
            return;
        }
        // Deleting the target leaves a dangling directory reparse point.
        fs::remove_dir(&target).expect("remove target");

        let info = WindowsFsBackend.stat(&link).expect("stat");
        assert!(info.is_symlink);
        assert_eq!(info.r#type, FileType::Unspecified);
        assert_eq!(info.mode, 0);
        // Linux keeps the link's own `Lrwxrwxrwx` permissions string.
        assert_eq!(info.permissions, "Lrwxrwxrwx");
        assert!(info.symlink_target.is_some());
        // Linux lstat reports the stored target length, never 0.
        assert!(info.size > 0, "dangling link size must not be 0");

        remove_entry(&link).expect("unlink dangling junction");
    }

    #[test]
    fn stat_reports_file_metadata() {
        let scratch = Scratch::new("stat");
        let path = scratch.join("hello.txt");
        fs::write(&path, b"hello").expect("write");

        let info = WindowsFsBackend.stat(&path).expect("stat");
        assert_eq!(info.name, "hello.txt");
        assert_eq!(info.r#type, FileType::File);
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

        // Scratch::drop cannot remove a read-only file, so clear the attribute
        // before the guard drops.
        let info = fs::metadata(&path).expect("metadata");
        clear_read_only_attribute(&path, &info).expect("clear readonly");
    }

    #[test]
    fn read_dir_orders_non_ascii_names_by_wtf8_bytes() {
        let scratch = Scratch::new("unicode-order");
        // U+E000 encodes as EE 80 80 and U+10000 as F0 90 80 80 in WTF-8, so
        // the private-use character sorts first. UTF-16 code-unit order would
        // put the surrogate pair first.
        fs::write(scratch.join("\u{e000}.txt"), b"a").expect("write");
        fs::write(scratch.join("\u{10000}.txt"), b"b").expect("write");

        let entries = WindowsFsBackend.read_dir(&scratch.0).expect("read_dir");
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["\u{e000}.txt", "\u{10000}.txt"]);
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
        assert_eq!(info.r#type, FileType::Directory);
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
