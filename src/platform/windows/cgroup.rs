//! Resource limits, freeze/thaw, and accounting skeleton; Job Objects arrive
//! in P2.

use std::io;
use std::time::Duration;

use crate::platform::ResourceBackend;

#[derive(Debug, Default)]
pub struct WindowsResourceBackend;

impl ResourceBackend for WindowsResourceBackend {
    fn freeze(&self, _max_wait: Duration) -> io::Result<()> {
        Err(super::unsupported("freeze"))
    }

    fn thaw(&self) -> io::Result<()> {
        Err(super::unsupported("unfreeze"))
    }
}
