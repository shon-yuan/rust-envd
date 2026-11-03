//! Windows backend skeleton (P0).
//!
//! Every method answers `Unsupported` until the corresponding P1/P2
//! implementation lands. The endpoint/method surface is fixed by the traits in
//! `platform/mod.rs`; see `docs/windows-port.md` for the plan.

pub mod cgroup;
pub mod clock;
pub mod fs;
pub mod metadata;
pub mod net;
pub mod process;

use std::io;

/// The error every unimplemented Windows backend method returns.
pub(crate) fn unsupported(feature: &'static str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{feature} is not implemented on Windows yet"),
    )
}
