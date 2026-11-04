//! Windows file operations behind the shared `POST /files/compose` handler.
//!
//! Real Win32 implementations: `BCryptGenRandom` for the UUID bytes,
//! `CreateFileW` + `GetFileInformationByHandle` for the volume serial/file
//! index identity, and `FlushFileBuffers` + `CloseHandle` for the explicit
//! close. There is no POSIX mode or ownership: the temp file inherits the
//! destination directory's ACL and the service account becomes its creator.
use crate::exec_context::Account;
use crate::platform::compose_files::FileId;
use crate::platform::file_writes;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        windows::ffi::OsStrExt,
        windows::fs::{MetadataExt, OpenOptionsExt},
        windows::io::IntoRawHandle,
    },
    path::{Component, Path, PathBuf},
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, GetFileInformationByHandle, SetFileAttributesW,
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_READONLY, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

/// Sixteen bytes from the system-preferred CSPRNG for the temporary-file UUID.
pub fn random_bytes() -> io::Result<[u8; 16]> {
    let mut random = [0u8; 16];
    // SAFETY: the buffer is valid for 16 bytes and the null algorithm handle is
    // required by BCRYPT_USE_SYSTEM_PREFERRED_RNG.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        // BCryptGenRandom returns an NTSTATUS, not a Win32 error, so
        // `from_raw_os_error` would print an unrelated system message.
        return Err(io::Error::other(format!(
            "BCryptGenRandom failed: NTSTATUS 0x{status:08x}"
        )));
    }
    Ok(random)
}

/// Creates the temporary file exclusively, sharing delete so the cleanup guard
/// can remove it even while a handle is still open.
pub fn create_new_temp(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
}

/// Lexically cleaned parent of a Windows destination path, separator- and
/// drive/UNC-aware, equivalent to the oracle's `go_parent`.
pub fn parent(destination: &str) -> PathBuf {
    // Split first, exactly as Go's `filepath.Dir` does: a trailing separator
    // names a directory, so `a/b/` has parent `a/b`, not `a`.
    let parent = match destination.rsplit_once(['/', '\\']) {
        Some(("", _)) => {
            if destination.starts_with('\\') {
                "\\"
            } else {
                "/"
            }
        }
        Some((parent, _)) => parent,
        None => ".",
    };
    // A bare drive designator is the drive root, not a relative name.
    if parent.len() == 2 && parent.as_bytes()[1] == b':' {
        return PathBuf::from(format!("{parent}{}", std::path::MAIN_SEPARATOR));
    }
    let mut clean = PathBuf::new();
    for component in Path::new(parent).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match clean.components().next_back() {
                Some(Component::Normal(_)) => {
                    clean.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
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

/// Creates every missing directory with the oracle's exact failure detail
/// strings. There is no ownership to adopt.
pub fn ensure_dirs(path: &Path, _account: &Account) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            // The prefix and root are the anchor for relative pushes below;
            // they are never entries to create.
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            Component::CurDir => continue,
            _ => {}
        }
        current.push(component.as_os_str());
        match fs::metadata(&current) {
            Ok(info) if info.is_dir() => {}
            Ok(_) => return Err(format!("path is a file: {}", current.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
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

/// No POSIX ownership: the service account owns what it creates.
pub fn adopt_owner(_path: &Path, _account: &Account) -> io::Result<()> {
    Ok(())
}

/// `os.SameFile` identity as (volume serial, file index). `follow = false`
/// opens with `FILE_FLAG_OPEN_REPARSE_POINT`, so a destination symlink is
/// compared as the link itself.
pub fn identity(path: &Path, follow: bool) -> io::Result<FileId> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    // Zero desired access + OPEN_EXISTING queries attributes without reading or
    // writing; backup semantics allows directory handles.
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let handle: HANDLE = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    let error = (ok == 0).then(io::Error::last_os_error);
    unsafe {
        CloseHandle(handle);
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok((
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ))
}

pub fn same_file(left: FileId, right: FileId) -> bool {
    left == right
}

/// Buffered copy with the oracle's operation strings. `CopyFileExW` is
/// path-based and cannot append to the already-open temp file.
pub fn copy_file(
    input: &mut File,
    output: &mut File,
    source: &str,
    temporary: &Path,
) -> Result<(), (String, io::Error)> {
    let mut buffer = vec![0u8; 1024 * 1024];
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

/// Flushes and closes the raw handle, surfacing the Win32 error.
pub fn close_output(file: File) -> io::Result<()> {
    let handle = file.into_raw_handle() as HANDLE;
    // SAFETY: the raw handle is owned by this function and closed exactly once.
    let flushed = unsafe { FlushFileBuffers(handle) };
    let flush_error = (flushed == 0).then(io::Error::last_os_error);
    let closed = unsafe { CloseHandle(handle) };
    let close_error = (closed == 0).then(io::Error::last_os_error);
    flush_error.or(close_error).map_or(Ok(()), Err)
}

/// Replaces `destination` with `temporary`. `MoveFileExW(REPLACE_EXISTING)`
/// refuses a read-only destination, while `rename(2)` replaces it; clear the
/// attribute for the retry and restore it if the retry still fails.
pub fn rename_over(temporary: &Path, destination: &Path) -> io::Result<()> {
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            let info = fs::symlink_metadata(destination)?;
            let attributes = info.file_attributes();
            if attributes & FILE_ATTRIBUTE_READONLY == 0 {
                return Err(error);
            }
            let wide: Vec<u16> = destination
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            if unsafe { SetFileAttributesW(wide.as_ptr(), attributes & !FILE_ATTRIBUTE_READONLY) }
                == 0
            {
                return Err(error);
            }
            match fs::rename(temporary, destination) {
                Ok(()) => Ok(()),
                Err(retry) => {
                    // Restore the attribute the destination had before the
                    // attempt; the rename error is the one the caller needs.
                    unsafe {
                        SetFileAttributesW(wide.as_ptr(), attributes);
                    }
                    Err(retry)
                }
            }
        }
        Err(error) => Err(error),
    }
}

/// Windows system-message text instead of the oracle's lowercased errno text.
pub fn errno_text(error: &io::Error) -> String {
    error.to_string()
}

/// `ERROR_DISK_FULL`/`ERROR_HANDLE_DISK_FULL` map to the compose 507 branch.
pub fn is_no_space(error: &io::Error) -> bool {
    file_writes::is_no_space(error)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "coorworker-envd-compose-{}-{unique}-{label}",
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
    fn compose_parent_cleans_drive_unc_and_relative_paths() {
        assert_eq!(parent(r"C:\a\b\..\c"), PathBuf::from(r"C:\a"));
        assert_eq!(parent(r"C:\b"), PathBuf::from(r"C:\"));
        assert_eq!(
            parent(r"\\server\share\dir\..\x"),
            PathBuf::from(r"\\server\share")
        );
        assert_eq!(parent(r"relative\name"), PathBuf::from("relative"));
        assert_eq!(parent("relative/name"), PathBuf::from("relative"));
        assert_eq!(parent("name"), PathBuf::from("."));
        assert_eq!(parent("./name"), PathBuf::from("."));
    }

    #[test]
    fn compose_identity_matches_hard_links_and_distinguishes_symlinks() {
        let scratch = Scratch::new("identity");
        let file = scratch.join("file.bin");
        fs::write(&file, b"identity").expect("file");
        let hard = scratch.join("hard.bin");
        fs::hard_link(&file, &hard).expect("hard link");
        let file_id = identity(&file, true).expect("file identity");
        assert!(same_file(
            file_id,
            identity(&hard, true).expect("hard identity")
        ));

        let link = scratch.join("link.bin");
        if std::os::windows::fs::symlink_file(&file, &link).is_err() {
            // Creating a symlink needs Developer Mode or SeCreateSymbolicLinkPrivilege.
            return;
        }
        // Lstat sees the link entry, not its target.
        assert!(!same_file(
            identity(&link, false).expect("link identity"),
            file_id
        ));
        assert!(same_file(
            identity(&link, true).expect("followed identity"),
            file_id
        ));
    }

    #[test]
    fn compose_buffered_copy_preserves_bytes_and_uses_operation_strings() {
        let scratch = Scratch::new("copy");
        let input_path = scratch.join("input.bin");
        let output_path = scratch.join("output.bin");
        let payload: Vec<u8> = (0..(3 * 1024 * 1024 + 7))
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(&input_path, &payload).expect("input");
        let mut input = File::open(&input_path).expect("open input");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .expect("open output");
        copy_file(&mut input, &mut output, "source.bin", Path::new("temp.bin"))
            .expect("buffered copy");
        drop(output);
        assert_eq!(fs::read(&output_path).expect("output"), payload);

        // The oracle's operation strings survive the buffered loop.
        let temporary = Path::new("temp.bin");
        let mut write_only = OpenOptions::new()
            .write(true)
            .open(&input_path)
            .expect("write-only input");
        let mut discard = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(scratch.join("discard.bin"))
            .expect("discard output");
        let (operation, _) = copy_file(&mut write_only, &mut discard, "source.bin", temporary)
            .expect_err("a write-only input cannot be read");
        assert_eq!(operation, "read source.bin");
        drop(discard);

        let mut readable = File::open(&input_path).expect("open input");
        let mut read_only = File::open(&output_path).expect("read-only output");
        let (operation, _) = copy_file(&mut readable, &mut read_only, "source.bin", temporary)
            .expect_err("a read-only output cannot be written");
        assert_eq!(operation, "write temp.bin");
    }

    #[test]
    fn compose_disk_full_mapping_is_507_but_quota_stays_500() {
        assert!(is_no_space(&io::Error::from_raw_os_error(112)));
        assert!(is_no_space(&io::Error::from_raw_os_error(39)));
        assert!(!is_no_space(&io::Error::from_raw_os_error(1295)));
        assert!(!is_no_space(&io::Error::other("other")));
    }
}
