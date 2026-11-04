//! The Windows fsfreeze backend: a volume flush, not a FIFREEZE barrier.
//!
//! Linux issues `FIFREEZE`/`FITHAW` ioctls on the guest rootfs. Windows has no
//! equivalent write barrier without VSS, so this slice opens the system volume
//! for read/write and calls `FlushFileBuffers`; `/fsthaw` repeats the flush and
//! succeeds as a no-op because nothing is held. Both operations are idempotent.
//!
//! Declared deviations (also in `docs/windows-port.md`): flush-only semantics,
//! no VSS snapshot (the COM interfaces are not in `windows-sys`), Win32 error
//! text instead of Go errno text, and a privilege requirement — flushing a
//! volume needs write access, typically an elevated service account.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, GetVolumePathNameW, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

use crate::fsfreeze::FsFreezer;

/// The resolved system volume: the human root (`C:\`) used in the Go-shaped
/// error text and the device path (`\\.\C:`) opened for the flush.
pub struct WindowsFsFreezer {
    root: String,
    volume: Option<String>,
    resolution_error: Option<String>,
}

impl WindowsFsFreezer {
    /// Resolves the system volume once, at daemon construction.
    pub fn system_volume() -> Self {
        match resolve_system_volume() {
            Ok((root, volume)) => Self {
                root,
                volume: Some(volume),
                resolution_error: None,
            },
            // A resolution failure is reported by every freeze/thaw call with
            // the same envelope shape as a failed flush. The root still gets a
            // best-effort name so the Go-shaped `FIFREEZE <mountpoint>: ...`
            // text never carries an empty mountpoint.
            Err(error) => Self {
                root: fallback_root(),
                volume: None,
                resolution_error: Some(error.to_string()),
            },
        }
    }

    /// The volume root (`C:\`) the daemon passes as the error-text mountpoint.
    pub fn root(&self) -> &str {
        &self.root
    }

    fn flush(&self, operation: &str, mountpoint: &str) -> io::Result<()> {
        let Some(volume) = &self.volume else {
            return Err(io::Error::other(format!(
                "{operation} {mountpoint}: {}",
                self.resolution_error
                    .as_deref()
                    .unwrap_or("system volume resolution failed")
            )));
        };
        let path = wide(volume);
        // SAFETY: NUL-terminated path, null security attributes and template;
        // the returned handle is checked and closed exactly once below.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                windows_sys::Win32::Foundation::GENERIC_READ
                    | windows_sys::Win32::Foundation::GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::other(format!(
                "{operation} {mountpoint}: {}",
                io::Error::last_os_error()
            )));
        }
        // SAFETY: an open volume handle owned by this function.
        let flushed = unsafe { FlushFileBuffers(handle) };
        let error = (flushed == 0).then(io::Error::last_os_error);
        // SAFETY: the handle was opened here and is not used afterwards.
        unsafe {
            CloseHandle(handle);
        }
        match error {
            Some(error) => Err(io::Error::other(format!(
                "{operation} {mountpoint}: {error}"
            ))),
            None => Ok(()),
        }
    }
}

impl FsFreezer for WindowsFsFreezer {
    fn freeze(&self, mountpoint: &str) -> io::Result<()> {
        self.flush("FIFREEZE", mountpoint)
    }

    fn thaw(&self, mountpoint: &str) -> io::Result<()> {
        self.flush("FITHAW", mountpoint)
    }
}

/// The system-volume root (`C:\`), shared with the metrics collector's disk
/// sampling.
pub(crate) fn system_volume_root() -> io::Result<String> {
    resolve_system_volume().map(|(root, _)| root)
}

/// `%SystemDrive%\` (or `C:\`) for error text when resolution itself failed.
fn fallback_root() -> String {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
    format!("{}\\", drive.trim_end_matches('\\'))
}

/// The system-volume root and its device path, resolved once.
fn resolve_system_volume() -> io::Result<(String, String)> {
    let mut directory = vec![0u16; 32768];
    // SAFETY: the buffer is valid for the length passed.
    let length = unsafe { GetWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= directory.len() {
        return Err(io::Error::other(
            "Windows directory path exceeds the resolution buffer",
        ));
    }
    directory.truncate(length as usize);
    let directory: Vec<u16> = directory.into_iter().chain(std::iter::once(0)).collect();

    let mut volume = vec![0u16; 32768];
    // SAFETY: NUL-terminated input, valid output buffer and length.
    let ok =
        unsafe { GetVolumePathNameW(directory.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let end = volume
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(volume.len());
    let root = String::from_utf16_lossy(&volume[..end]);
    if root.is_empty() {
        return Err(io::Error::other("empty system volume path"));
    }
    let device = volume_device_path(&root);
    Ok((root, device))
}

/// `C:\` -> `\\.\C:`; a UNC root (`\\server\share\`) stays as its share path.
pub(crate) fn volume_device_path(root: &str) -> String {
    let trimmed = root.trim_end_matches('\\');
    if trimmed.starts_with(r"\\") {
        trimmed.to_owned()
    } else {
        format!(r"\\.\{trimmed}")
    }
}

/// NUL-terminated UTF-16 for a Win32 wide-string parameter.
pub(crate) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_roots_map_to_the_volume_device_path() {
        assert_eq!(volume_device_path(r"C:\"), r"\\.\C:");
        assert_eq!(volume_device_path(r"C:"), r"\\.\C:");
        assert_eq!(volume_device_path(r"\\server\share\"), r"\\server\share");
    }

    #[test]
    fn windows_fs_freezer_flushes_the_system_volume() {
        let freezer = WindowsFsFreezer::system_volume();
        let root = freezer.root().to_owned();
        match freezer.freeze(&root) {
            Ok(()) => {
                // A second flush is idempotent, exactly like a repeated freeze.
                freezer.freeze(&root).expect("second freeze");
                freezer.thaw(&root).expect("thaw");
                freezer.thaw(&root).expect("second thaw");
            }
            // Documented privilege precondition: a non-elevated runner cannot
            // open the volume for write. The capability is absent, so the test
            // skips rather than reporting a false failure.
            Err(error) if error.to_string().contains("os error 5") => {}
            Err(error) => panic!("volume flush failed unexpectedly: {error}"),
        }
    }
}
