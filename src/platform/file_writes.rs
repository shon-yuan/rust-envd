//! The file-write seam `POST /files` uses.
//!
//! The shared upload flow (`crate::files_upload`) never calls a platform API
//! directly: it creates parent directories, adopts ownership, opens the
//! target and persists `user.e2b.*` metadata through the helpers below.
//! [`validate_metadata`] is platform-independent policy and lives here so both
//! platforms enforce exactly the same keys and limits.
use std::collections::HashMap;
use std::io;

pub(crate) const METADATA_PREFIX: &str = "user.e2b.";
const MAX_KEY: usize = 255 - METADATA_PREFIX.len();
const MAX_TOTAL: usize = 4096;

/// The pinned oracle's metadata validation (filesystem/xattr.go): keys fit in
/// one attribute name, everything is printable ASCII, and the `user.e2b.`
/// prefix plus all keys and values stay within 4096 bytes.
pub fn validate_metadata(values: &HashMap<String, String>) -> io::Result<()> {
    let mut total = 0usize;
    for (key, value) in values {
        if key.is_empty() || key.len() > MAX_KEY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid metadata key length",
            ));
        }
        if !key
            .bytes()
            .chain(value.bytes())
            .all(|b| (0x20..=0x7e).contains(&b))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata must be printable ASCII",
            ));
        }
        total = total.saturating_add(METADATA_PREFIX.len() + key.len() + value.len());
    }
    if total > MAX_TOTAL {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "metadata exceeds 4096 bytes",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) use crate::platform::linux::file_writes::*;

#[cfg(windows)]
pub(crate) use crate::platform::windows::file_writes::*;
