//! The file-operation seam `POST /files/compose` uses.
//!
//! The shared compose flow (`crate::files_compose`) never calls a platform API
//! directly: it draws the temporary-file suffix, creates the temp file, adopts
//! ownership, copies the sources, closes the output, and compares file identity
//! through the helpers below. The Linux implementations keep the parity
//! behaviour (POSIX mode/uid/gid, `copy_file_range`, errno text) in one place;
//! the Windows implementations use Win32.

/// Device or volume plus inode or file index: the value `os.SameFile` compares.
/// Linux fills it from `MetadataExt::dev`/`ino`; Windows from the volume serial
/// number and `BY_HANDLE_FILE_INFORMATION` file index.
pub(crate) type FileId = (u64, u64);

#[cfg(target_os = "linux")]
pub(crate) use crate::platform::linux::compose_files::*;

#[cfg(windows)]
pub(crate) use crate::platform::windows::compose_files::*;
