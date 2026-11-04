//! Resource limits and freeze/thaw for Windows.
//!
//! The behaviour lives in [`super::resource`], which owns the workload
//! registry, the suspend/resume primitives and the optional Job Object. This
//! module only keeps the platform seam honest: `ResourceBackend` maps onto the
//! backend's legacy (registry) freeze path, while the daemon's HTTP surface
//! drives the async methods directly so it can report the full structured
//! result.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crate::platform::ResourceBackend;

pub use super::resource::WindowsResourceBackend;

// The watchdog needs a shared owner for its backstop task, so the platform
// seam is implemented for the `Arc` the daemon actually holds.
impl ResourceBackend for Arc<WindowsResourceBackend> {
    fn freeze(&self, max_wait: Duration) -> io::Result<()> {
        self.freeze_blocking(max_wait)
    }

    fn thaw(&self) -> io::Result<()> {
        self.thaw_blocking()
    }
}
