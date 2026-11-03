//! Windows side of the `POST /files` write seam.
//!
//! Windows has no POSIX ownership or mode bits, so the ownership adoptions are
//! no-ops and the file is created with the service account's default ACL. NTFS
//! extended attributes are not mapped yet (the documented `metadata` gap in
//! `docs/windows-port.md` §5.2): reads answer an empty map and writes answer
//! `Unsupported`, which the shared upload flow degrades to a logged warning.
use crate::exec_context::Account;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

/// Creates every missing parent. There is no ownership to adopt, so this is
/// the directory creation only.
pub fn ensure_dirs(path: &Path, _account: &Account) -> io::Result<()> {
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        // `C:` and the `\` after it are roots, not entries to create.
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::CurDir
        ) {
            continue;
        }
        match fs::metadata(&current) {
            Ok(info) if info.is_dir() => {}
            Ok(_) => return Err(io::Error::other("path is a file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// No POSIX ownership: the service account owns what it creates.
pub fn adopt_path_owner(_path: &Path, _account: &Account) -> io::Result<bool> {
    Ok(true)
}

/// No POSIX ownership to adopt on the descriptor.
pub fn adopt_file_owner(_file: &File, _account: &Account) -> io::Result<()> {
    Ok(())
}

/// Creates or truncates the target.
pub fn open_for_write(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// `os.SameFile` over the volume serial and file index, so hard links and
/// symlinks to the same file are caught before the second upload truncates the
/// first. Rust's stable `Metadata` does not expose the file index, so the two
/// paths are opened for attributes only (`GetFileInformationByHandle`).
pub fn same_file(left: &Path, right: &Path) -> bool {
    match (identity(left), identity(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn identity(path: &Path) -> Option<(u32, u64)> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Zero desired access + `OPEN_EXISTING` queries attributes without reading
    // or writing; `FILE_FLAG_BACKUP_SEMANTICS` allows directory handles.
    let handle: HANDLE = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe {
        CloseHandle(handle);
    }
    (ok != 0).then_some((
        info.dwVolumeSerialNumber,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ))
}

/// NTFS extended attributes are not mapped yet; the documented gap.
pub fn read_metadata(_path: &Path) -> io::Result<HashMap<String, String>> {
    Ok(HashMap::new())
}

/// See [`read_metadata`]: the shared flow treats `Unsupported` as "persist
/// nothing" rather than a failed upload.
pub fn write_metadata(_file: &File, _values: &HashMap<String, String>) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "NTFS extended attributes are not mapped on Windows yet",
    ))
}

pub fn metadata_unsupported(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
}

fn is_disk_full(code: i32) -> bool {
    let code = code as u32;
    code == windows_sys::Win32::Foundation::ERROR_DISK_FULL
        || code == windows_sys::Win32::Foundation::ERROR_HANDLE_DISK_FULL
}

/// The disk-full errors `os` reports on Windows.
pub fn is_no_space(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(is_disk_full)
}

/// Disk-full or quota exceeded.
pub fn is_no_space_or_quota(error: &io::Error) -> bool {
    is_no_space(error)
        || error.raw_os_error().is_some_and(|code| {
            code as u32 == windows_sys::Win32::Foundation::ERROR_DISK_QUOTA_EXCEEDED
        })
}
