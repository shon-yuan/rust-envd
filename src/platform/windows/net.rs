//! Local-port enumeration skeleton; GetExtendedTcpTable arrives in P3.

use std::io;

use crate::platform::PortScanBackend;

#[derive(Debug, Default)]
pub struct WindowsPortScanBackend;

impl PortScanBackend for WindowsPortScanBackend {
    fn listening_tcp_ports(&self) -> io::Result<Vec<u16>> {
        Err(super::unsupported("local port scan"))
    }
}
