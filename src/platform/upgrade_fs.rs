//! The staged replacement-binary write seam behind `POST /upgrade`.
//!
//! The shared upgrade handler never writes the delivered body itself: it hands
//! the stream to [`stage_binary`], whose platform arm owns the fixed-path
//! write semantics. Linux keeps the oracle's unlink-first `0o755` write; a
//! running Windows image cannot be deleted, so its arm renames the live image
//! out of the way before writing the canonical path.

#[cfg(target_os = "linux")]
pub use crate::platform::linux::upgrade_fs::stage_binary;

#[cfg(windows)]
pub use crate::platform::windows::upgrade_fs::stage_binary;
