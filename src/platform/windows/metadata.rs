//! Access-token delivery skeleton; the Windows service receives it from the
//! control plane instead of MMDS, wired in P3.

use std::io;

use crate::platform::MetadataBackend;

#[derive(Debug, Default)]
pub struct WindowsMetadataBackend;

impl MetadataBackend for WindowsMetadataBackend {
    fn init_token(&self) -> io::Result<Option<Vec<u8>>> {
        Err(super::unsupported("metadata token delivery"))
    }
}
