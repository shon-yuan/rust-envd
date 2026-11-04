//! Local-port enumeration through `GetExtendedTcpTable`, mirroring the Linux
//! `/proc/net/tcp{,6}` scanner's contract for the loopback forwarder.
//!
//! One query per address family, IPv4 first, with
//! `TCP_TABLE_OWNER_PID_LISTENER`: every returned row is a LISTEN socket with
//! its owning pid. Row layouts are read unaligned out of the returned byte
//! buffer because `Vec<u8>` has no alignment guarantee. Ports arrive in
//! network order in the low 16 bits; addresses are converted with the
//! platform's byte-order helpers. Results are deduplicated with the Linux key
//! `(sock_type, laddr, raddr, status)`, family excluded, first wins, so the
//! IPv4 row is preferred when both families describe the same socket.

use std::{
    collections::HashSet,
    ffi::c_void,
    io,
    net::{Ipv4Addr, Ipv6Addr},
    ptr,
};

use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

use crate::platform::port::{Addr, ConnectionStat, ScanSource};
use crate::platform::PortScanBackend;

/// AF_INET/AF_INET6 are the Windows Winsock values; Linux uses different
/// numeric values for AF_INET6, which is why the field is platform-local.
const FAMILY_INET: u32 = AF_INET as u32;
const FAMILY_INET6: u32 = AF_INET6 as u32;
/// SOCK_STREAM, the only socket type the "tcp" kind scans.
const SOCK_STREAM: u32 = 1;
/// `GetExtendedTcpTable` may race with new listeners; retry this many times
/// while it reports a larger table size.
const MAX_RETRIES: u32 = 4;

#[derive(Debug, Default)]
pub struct WindowsPortScanBackend;

impl ScanSource for WindowsPortScanBackend {
    fn connections(&self) -> io::Result<Vec<ConnectionStat>> {
        let mut rows = query_family(FAMILY_INET)?;
        rows.extend(query_family(FAMILY_INET6)?);
        Ok(dedup_connections(rows))
    }
}

impl PortScanBackend for WindowsPortScanBackend {}

/// Calls the API for one family and parses the returned table.
fn query_family(family: u32) -> io::Result<Vec<ConnectionStat>> {
    let bytes = table_bytes(|table, size| unsafe {
        GetExtendedTcpTable(table, size, 0, family, TCP_TABLE_OWNER_PID_LISTENER, 0)
    })?;
    parse_table(&bytes, family)
}

/// Sizes the table, then retries the allocation while the API reports
/// `ERROR_INSUFFICIENT_BUFFER`. The callable returns a Win32 status code.
fn table_bytes(mut call: impl FnMut(*mut c_void, &mut u32) -> u32) -> io::Result<Vec<u8>> {
    let mut size = 0u32;
    let status = call(ptr::null_mut(), &mut size);
    if status == 0 && size == 0 {
        return Ok(Vec::new());
    }
    if status != 0 && status != ERROR_INSUFFICIENT_BUFFER {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut retries = 0u32;
    loop {
        let mut buffer = vec![0u8; size as usize];
        let mut written = size;
        let status = call(buffer.as_mut_ptr().cast(), &mut written);
        if status == 0 {
            buffer.truncate(written as usize);
            return Ok(buffer);
        }
        if status != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if retries >= MAX_RETRIES {
            return Err(io::Error::other(
                "GetExtendedTcpTable kept growing the listener table",
            ));
        }
        retries += 1;
        size = written;
    }
}

/// Parses `MIB_TCPTABLE_OWNER_PID` / `MIB_TCP6TABLE_OWNER_PID` bytes.
fn parse_table(bytes: &[u8], family: u32) -> io::Result<Vec<ConnectionStat>> {
    if bytes.len() < std::mem::size_of::<u32>() {
        return Ok(Vec::new());
    }
    let count = u32::from_ne_bytes(bytes[..4].try_into().expect("four bytes")) as usize;
    let row_size = if family == FAMILY_INET {
        std::mem::size_of::<MIB_TCPROW_OWNER_PID>()
    } else {
        std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>()
    };
    let mut rows = Vec::with_capacity(count);
    let mut offset = 4usize;
    for _ in 0..count {
        let end = offset
            .checked_add(row_size)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "GetExtendedTcpTable returned a truncated table",
                )
            })?;
        // SAFETY: the checked range holds one complete row; the read is
        // unaligned because the Vec<u8> base is not guaranteed to be.
        let row = &bytes[offset..end];
        if family == FAMILY_INET {
            let row = unsafe { ptr::read_unaligned(row.as_ptr().cast::<MIB_TCPROW_OWNER_PID>()) };
            rows.push(ConnectionStat {
                fd: 0,
                family,
                sock_type: SOCK_STREAM,
                laddr: Addr {
                    ip: Ipv4Addr::from(u32::from_be(row.dwLocalAddr)).to_string(),
                    port: network_port(row.dwLocalPort),
                },
                raddr: Addr {
                    ip: Ipv4Addr::from(u32::from_be(row.dwRemoteAddr)).to_string(),
                    port: network_port(row.dwRemotePort),
                },
                status: "LISTEN".to_owned(),
                pid: row.dwOwningPid as i32,
            });
        } else {
            let row = unsafe { ptr::read_unaligned(row.as_ptr().cast::<MIB_TCP6ROW_OWNER_PID>()) };
            rows.push(ConnectionStat {
                fd: 0,
                family,
                sock_type: SOCK_STREAM,
                laddr: Addr {
                    ip: Ipv6Addr::from(row.ucLocalAddr).to_string(),
                    port: network_port(row.dwLocalPort),
                },
                raddr: Addr {
                    ip: Ipv6Addr::from(row.ucRemoteAddr).to_string(),
                    port: network_port(row.dwRemotePort),
                },
                status: "LISTEN".to_owned(),
                pid: row.dwOwningPid as i32,
            });
        }
        offset = end;
    }
    Ok(rows)
}

/// `dwLocalPort`/`dwRemotePort` are network-order ports in the low 16 bits.
fn network_port(value: u32) -> u32 {
    u32::from(u16::from_be((value & 0xFFFF) as u16))
}

/// The Linux `net_linux.go` dedup key: family is deliberately excluded and the
/// first row wins, with IPv4 queried first.
fn dedup_connections(rows: Vec<ConnectionStat>) -> Vec<ConnectionStat> {
    let mut seen = HashSet::new();
    rows.into_iter()
        .filter(|entry| {
            seen.insert((
                entry.sock_type,
                entry.laddr.ip.clone(),
                entry.laddr.port,
                entry.raddr.ip.clone(),
                entry.raddr.port,
                entry.status.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipv4_table(rows: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut bytes = (rows.len() as u32).to_ne_bytes().to_vec();
        for (local_addr, local_port, pid) in rows {
            bytes.extend_from_slice(&2u32.to_ne_bytes()); // MIB_TCP_STATE_LISTEN
            bytes.extend_from_slice(&local_addr.to_ne_bytes());
            bytes.extend_from_slice(&local_port.to_ne_bytes());
            bytes.extend_from_slice(&0u32.to_ne_bytes()); // remote address
            bytes.extend_from_slice(&0u32.to_ne_bytes()); // remote port
            bytes.extend_from_slice(&pid.to_ne_bytes());
        }
        bytes
    }

    fn ipv6_table(rows: &[([u8; 16], u32, u32)]) -> Vec<u8> {
        let mut bytes = (rows.len() as u32).to_ne_bytes().to_vec();
        for (local_addr, local_port, pid) in rows {
            bytes.extend_from_slice(local_addr);
            bytes.extend_from_slice(&0u32.to_ne_bytes()); // local scope id
            bytes.extend_from_slice(&local_port.to_ne_bytes());
            bytes.extend_from_slice(&[0u8; 16]); // remote address
            bytes.extend_from_slice(&0u32.to_ne_bytes()); // remote scope id
            bytes.extend_from_slice(&0u32.to_ne_bytes()); // remote port
            bytes.extend_from_slice(&2u32.to_ne_bytes()); // state
            bytes.extend_from_slice(&pid.to_ne_bytes());
        }
        bytes
    }

    #[test]
    fn parses_byte_order_and_pid() {
        let v4 = ipv4_table(&[(u32::from_ne_bytes([127, 0, 0, 1]), 0x5000, 4242)]);
        let rows = parse_table(&v4, FAMILY_INET).expect("ipv4 table");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].laddr.ip, "127.0.0.1");
        assert_eq!(rows[0].laddr.port, 80, "network-order port 0x5000 -> 80");
        assert_eq!(rows[0].raddr.ip, "0.0.0.0");
        assert_eq!(rows[0].raddr.port, 0);
        assert_eq!(rows[0].pid, 4242);
        assert_eq!(rows[0].family, FAMILY_INET);
        assert_eq!(rows[0].sock_type, SOCK_STREAM);
        assert_eq!(rows[0].status, "LISTEN");

        let mut loopback = [0u8; 16];
        loopback[15] = 1;
        let v6 = ipv6_table(&[(loopback, 0x5000, 4343)]);
        let rows = parse_table(&v6, FAMILY_INET6).expect("ipv6 table");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].laddr.ip, "::1");
        assert_eq!(rows[0].laddr.port, 80);
        assert_eq!(rows[0].raddr.ip, "::");
        assert_eq!(rows[0].pid, 4343);
        assert_eq!(rows[0].family, FAMILY_INET6);
    }

    #[test]
    fn dedups_with_the_family_excluded_key() {
        let row = ConnectionStat {
            fd: 0,
            family: FAMILY_INET,
            sock_type: SOCK_STREAM,
            laddr: Addr {
                ip: "127.0.0.1".to_owned(),
                port: 8080,
            },
            raddr: Addr {
                ip: "0.0.0.0".to_owned(),
                port: 0,
            },
            status: "LISTEN".to_owned(),
            pid: 7,
        };
        let mut second_family = row.clone();
        second_family.family = FAMILY_INET6;
        assert_eq!(
            dedup_connections(vec![row.clone(), second_family]),
            vec![row]
        );
    }

    #[test]
    fn surfaces_api_errors_and_caps_retries() {
        let error = table_bytes(|_, _| 5).expect_err("non-122 error");
        assert_eq!(error.raw_os_error(), Some(5));

        let mut calls = 0;
        let error = table_bytes(|_, size| {
            calls += 1;
            *size = 64;
            ERROR_INSUFFICIENT_BUFFER
        })
        .expect_err("retry cap");
        assert_eq!(calls, 6, "one sizing call plus one call and four retries");
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn truncated_table_is_an_error() {
        let error = parse_table(&1u32.to_ne_bytes(), FAMILY_INET).expect_err("truncated");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn reports_a_bound_ipv4_loopback_listener() {
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
            eprintln!("skipping: could not bind 127.0.0.1");
            return;
        };
        let port = listener.local_addr().expect("local addr").port();
        let rows = WindowsPortScanBackend.connections().expect("scan");
        let row = rows
            .iter()
            .find(|row| row.laddr.ip == "127.0.0.1" && row.laddr.port == u32::from(port))
            .expect("bound loopback listener");
        assert_eq!(row.status, "LISTEN");
        assert_eq!(row.family, FAMILY_INET);
        assert_eq!(row.pid, std::process::id() as i32);
        let ports = WindowsPortScanBackend
            .listening_tcp_ports()
            .expect("port list");
        assert!(ports.contains(&port));
    }

    #[test]
    fn reports_a_bound_ipv6_loopback_listener() {
        let Ok(listener) = std::net::TcpListener::bind(("::1", 0)) else {
            eprintln!("skipping: could not bind ::1");
            return;
        };
        let port = listener.local_addr().expect("local addr").port();
        let rows = WindowsPortScanBackend.connections().expect("scan");
        let row = rows
            .iter()
            .find(|row| row.laddr.ip == "::1" && row.laddr.port == u32::from(port))
            .expect("bound IPv6 loopback listener");
        assert_eq!(row.status, "LISTEN");
        assert_eq!(row.family, FAMILY_INET6);
        assert_eq!(row.pid, std::process::id() as i32);
    }
}
