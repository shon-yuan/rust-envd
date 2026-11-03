//! Process execution skeleton; CreateProcess/Job Objects/ConPTY arrive in P1.

use std::io;

use crate::platform::ProcessBackend;
use crate::proto::process::{ProcessConfig, ProcessInfo};

#[derive(Debug, Default)]
pub struct WindowsProcessBackend;

impl ProcessBackend for WindowsProcessBackend {
    fn start(&self, _config: &ProcessConfig) -> io::Result<ProcessInfo> {
        Err(super::unsupported("process start"))
    }

    fn list(&self) -> io::Result<Vec<ProcessInfo>> {
        Err(super::unsupported("process list"))
    }

    fn terminate(&self, _pid: u32) -> io::Result<()> {
        Err(super::unsupported("process terminate"))
    }
}
