//! The `/proc/net/tcp{,6}` scan source, replicating the pinned gopsutil v4.26.6
//! `net.Connections("tcp")` behaviour the oracle scanner consumes
//! (internal/port/scan.go:47). Kind "tcp" is tcp4 then tcp6
//! (net_linux.go:321-326); a failed scan returns an error and is not broadcast
//! (scan.go:48-49); a missing tcp6 means IPv6 is unsupported and is skipped
//! (net_linux.go:702-706).
//!
//! Replicated details, each pinned by a test below:
//! - the inode -> pid map walks `root/<pid>/fd/*` readlinks of the form
//!   `socket:[inode]`, first pid wins (net_linux.go:482-516, 731-738);
//! - the status table is the tcp_states hex map, an unknown hex stays as the
//!   zero value, i.e. the empty string (net_linux.go:278-292, 742-745);
//! - addresses decode from `hex-addr:hex-port` — IPv4 bytes reversed
//!   (little endian), IPv6 in four reversed 4-byte groups, rendered with Go
//!   `net.IP.String` formatting (RFC 5952; a v4-mapped address renders DOTTED
//!   because appendTo takes To4 first, ip.go:341-348) (net_linux.go:630-678);
//! - rows with fewer than 10 fields and rows whose addresses fail to decode
//!   are skipped (net_linux.go:708-761);
//! - connections dedupe across both files by
//!   `(sock_type, laddr, raddr, status)` — family is NOT in the key — first
//!   wins (net_linux.go:437-445, 456-478).

use std::{
    io,
    path::{Path, PathBuf},
};

/// `syscall.AF_INET` (net_linux.go:293-301 kindTCP4).
pub const AF_INET: u32 = nix::libc::AF_INET as u32;
/// `syscall.AF_INET6` (net_linux.go:303-311 kindTCP6).
pub const AF_INET6: u32 = nix::libc::AF_INET6 as u32;
/// `syscall.SOCK_STREAM` — the only socket type the "tcp" kind scans.
const SOCK_STREAM: u32 = nix::libc::SOCK_STREAM as u32;

/// gopsutil `net.Addr` (the fields the port package and the dedup key use).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addr {
    pub ip: String,
    pub port: u32,
}

/// gopsutil `net.ConnectionStat`, reduced to the fields `net.Connections`
/// populates that the oracle port package reads (`Family`, `Laddr`, `Status`,
/// `Pid`; forward.go:159-191) plus the dedup-key inputs. `Uids` is dropped:
/// gopsutil fetches and then drops every read error (`conn.Uids, _ =`) and no
/// port-package code reads it (net_linux.go:467-470).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionStat {
    pub fd: u32,
    pub family: u32,
    pub sock_type: u32,
    pub laddr: Addr,
    pub raddr: Addr,
    pub status: String,
    pub pid: i32,
}

/// The scan the scanner broadcasts. The production source reads `/proc`; tests
/// inject a synthetic proc tree or a fully scripted source.
pub trait ScanSource: Send + Sync {
    /// gopsutil `net.Connections("tcp")`: both address families or an error.
    fn connections(&self) -> io::Result<Vec<ConnectionStat>>;
}

/// Reads `$root/net/tcp` and `$root/net/tcp6` the way gopsutil does with
/// `HOST_PROC` set to `root`.
#[derive(Clone, Debug)]
pub struct ProcNetSource {
    root: PathBuf,
}

impl ProcNetSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Production: the real procfs.
    pub fn proc_default() -> Self {
        Self::new("/proc")
    }
}

impl ScanSource for ProcNetSource {
    fn connections(&self) -> io::Result<Vec<ConnectionStat>> {
        connections_in_proc_root(&self.root)
    }
}

/// The inode map: hex inode -> the (pid, fd) pairs that hold it, first scanned
/// pid first (net_linux.go:731-738 `i[0]`).
type Inodes = std::collections::HashMap<String, Vec<(i32, u32)>>;

pub(crate) fn connections_in_proc_root(root: &Path) -> io::Result<Vec<ConnectionStat>> {
    let inodes = proc_inodes_all(root)?;
    let mut found = Vec::new();
    // tmap order is tcp4 then tcp6 (net_linux.go:321-326); the dedup map is
    // shared across the two files (net_linux.go:448-451).
    let mut seen = std::collections::HashSet::new();
    for (family, file) in [(AF_INET, "tcp"), (AF_INET6, "tcp6")] {
        let path = root.join("net").join(file);
        // IPv6 not supported is an empty result, not an error
        // (net_linux.go:702-706); only files with a "6" suffix get the check.
        if file.ends_with('6') && !path.exists() {
            continue;
        }
        let contents = std::fs::read_to_string(&path)?;
        for row in contents.lines().skip(1) {
            let Some(entry) = parse_inet_row(row, family, &inodes) else {
                continue;
            };
            let key = (
                entry.sock_type,
                entry.laddr.ip.clone(),
                entry.laddr.port,
                entry.raddr.ip.clone(),
                entry.raddr.port,
                entry.status.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
            found.push(entry);
        }
    }
    Ok(found)
}

/// gopsutil `getProcInodesAll` (net_linux.go:596-625): enumerate numeric
/// `root/<pid>` directories in directory order, map every `socket:[inode]`
/// fd. A pid whose fd directory cannot be opened is skipped for permission
/// and not-found (process exited mid-scan); any other open error fails the
/// whole scan, exactly as the Go classification does.
fn proc_inodes_all(root: &Path) -> io::Result<Inodes> {
    let mut inodes = Inodes::new();
    let entries = std::fs::read_dir(root)?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            // A vanished directory entry mid-iteration: skip it, matching the
            // per-entry `continue` on readlink failures.
            Err(_) => continue,
        };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            // os.IsPermission || os.IsNotExist -> skip the pid
            // (net_linux.go:605-612).
            continue;
        };
        for fd in fds {
            let Ok(fd) = fd else { continue };
            let Ok(link) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = socket_inode(&link.to_string_lossy()) else {
                continue;
            };
            let Ok(fd_number) = fd.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            inodes.entry(inode).or_default().push((pid, fd_number));
        }
    }
    Ok(inodes)
}

/// `socket:[12345]` -> `12345` (net_linux.go:496-506).
fn socket_inode(link: &str) -> Option<String> {
    let rest = link.strip_prefix("socket:[")?;
    let inode = rest.strip_suffix(']')?;
    (!inode.is_empty()).then(|| inode.to_owned())
}

/// gopsutil `processInet` for one row (net_linux.go:708-761).
fn parse_inet_row(row: &str, family: u32, inodes: &Inodes) -> Option<ConnectionStat> {
    let fields = row.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 10 {
        return None;
    }
    let inode = fields[9];
    // First (pid, fd) wins: `i[0]` (net_linux.go:731-738).
    let (pid, fd) = inodes
        .get(inode)
        .and_then(|holders| holders.first())
        .copied()
        .unwrap_or((0, 0));
    let laddr = decode_address(family, fields[1])?;
    let raddr = decode_address(family, fields[2])?;
    Some(ConnectionStat {
        fd,
        family,
        sock_type: SOCK_STREAM,
        laddr,
        raddr,
        status: tcp_status(fields[3]),
        pid,
    })
}

/// http://students.mimuw.edu.pl/lxr/source/include/net/tcp_states.h via
/// net_linux.go:278-292; a hex outside the map renders as the Go zero value,
/// the empty string (net_linux.go:742-745 `status = tcpStatuses[status]`).
fn tcp_status(hex: &str) -> String {
    match hex {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        _ => "",
    }
    .to_owned()
}

/// gopsutil `decodeAddress` (net_linux.go:630-662): `hex:hex` into a Go-rendered
/// IP plus port. A malformed address rejects the ROW (the caller continues).
fn decode_address(family: u32, src: &str) -> Option<Addr> {
    let (hex_ip, hex_port) = src.split_once(':')?;
    let port = u32::from(u16::from_str_radix(hex_port, 16).ok()?);
    let bytes = hex::decode_bytes(hex_ip)?;
    let ip = match family {
        AF_INET => {
            let mut reversed = bytes;
            reversed.reverse();
            ip_v4_string(&reversed)?
        }
        // IPv6: four groups of four bytes, each group reversed
        // (parseIPv6HexString, net_linux.go:664-678); rendered with Go
        // net.IP.String formatting.
        _ => {
            let mut groups = [0u8; 16];
            let len = bytes.len();
            if len != 16 {
                return None;
            }
            for group in 0..4 {
                for offset in 0..4 {
                    groups[group * 4 + offset] = bytes[group * 4 + (3 - offset)];
                }
            }
            ip_v6_string(&groups)
        }
    };
    Some(Addr { ip, port })
}

/// Minimal hex decoder producing exactly `hex.len()/2` bytes; an odd-length or
/// non-hex input rejects the row like `hex.DecodeString` does.
mod hex {
    pub fn decode_bytes(input: &str) -> Option<Vec<u8>> {
        if !input.len().is_multiple_of(2) {
            return None;
        }
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks(2) {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            out.push(((high << 4) | low) as u8);
        }
        Some(out)
    }
}

fn ip_v4_string(bytes: &[u8]) -> Option<String> {
    let [a, b, c, d] = bytes.try_into().ok()?;
    Some(format!("{a}.{b}.{c}.{d}"))
}

/// Go `net.IP` (16 bytes) `String()`: `appendTo` (ip.go:341-348) calls To4()
/// FIRST, replacing a v4-mapped address with its 4-byte form before
/// AddrFromSlice — so it renders dotted ("127.0.0.1"); everything else takes
/// netip.appendTo6 with the longest (first on ties) zero run of at least two
/// groups compressed. The `::ffff:a.b.c.d` shape (netip.appendTo4In6) is only
/// reached when a 16-byte slice goes straight to AddrFromSlice, a path
/// gopsutil's decodeAddress (net_linux.go:658) never takes.
fn ip_v6_string(bytes: &[u8; 16]) -> String {
    if bytes[..10].iter().all(|byte| *byte == 0) && bytes[10] == 0xff && bytes[11] == 0xff {
        return format!("{}.{}.{}.{}", bytes[12], bytes[13], bytes[14], bytes[15]);
    }
    let groups: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    let (mut zero_start, mut zero_end) = (255u8, 255u8);
    for i in 0..8u8 {
        let mut j = i;
        while j < 8 && groups[j as usize] == 0 {
            j += 1;
        }
        let length = j - i;
        if length >= 2 && length > zero_end.saturating_sub(zero_start) {
            zero_start = i;
            zero_end = j;
        }
    }
    let mut out = String::new();
    let mut i = 0u8;
    while i < 8 {
        if i == zero_start {
            out.push_str("::");
            i = zero_end;
            if i >= 8 {
                break;
            }
        } else if i > 0 {
            out.push(':');
        }
        // Go's appendTo6 falls through after either branch: the group at the
        // (updated) index is appended directly, so the group ending a zero
        // run follows the "::" with no extra colon (netip.appendTo6).
        out.push_str(&format!("{:x}", groups[i as usize]));
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::scan_subscriber::ScannerFilter;
    use std::fs;

    /// Builds a synthetic proc tree: `net/tcp`-style files plus pid/fd
    /// socket:[inode] symlinks, mirroring the oracle scanner_linux_test.go
    /// HOST_PROC fixture (a temp tree instead of the env var — the Rust scan
    /// source is constructor-injected).
    struct ProcTree {
        root: PathBuf,
    }

    impl ProcTree {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap().keep();
            Self { root }
        }

        fn net_file(&self, name: &str, contents: &str) {
            let dir = self.root.join("net");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(name), contents).unwrap();
        }

        fn pid_holding(&self, pid: i32, inodes: &[(&str, u32)]) {
            for (inode, fd) in inodes {
                let dir = self.root.join(pid.to_string()).join("fd");
                fs::create_dir_all(&dir).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(format!("socket:[{inode}]"), dir.join(fd.to_string()))
                    .unwrap();
            }
        }

        fn source(&self) -> ProcNetSource {
            ProcNetSource::new(&self.root)
        }
    }

    const HEADER: &str =
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

    fn listen_row(local: &str, remote: &str, state: &str, inode: &str) -> String {
        format!(
            "   0: {local} {remote} {state} 00000000:00000000 00:00000000 00000000     0        0 {inode} 1 0000000000000000 100 0 0 10 0\n"
        )
    }

    /// The oracle scanner_linux_test.go fixture row: one LISTEN socket on
    /// 127.0.0.1:8080 (0100007F:1F90), state 0A, inode 12345.
    #[test]
    fn parses_a_listening_ipv4_row_with_its_pid() {
        let tree = ProcTree::new();
        tree.net_file(
            "tcp",
            &format!(
                "{HEADER}{}",
                listen_row("0100007F:1F90", "00000000:0000", "0A", "12345")
            ),
        );
        tree.pid_holding(4242, &[("12345", 7)]);
        let rows = tree.source().connections().unwrap();
        assert_eq!(
            rows,
            vec![ConnectionStat {
                fd: 7,
                family: AF_INET,
                sock_type: SOCK_STREAM,
                laddr: Addr {
                    ip: "127.0.0.1".into(),
                    port: 8080
                },
                raddr: Addr {
                    ip: "0.0.0.0".into(),
                    port: 0
                },
                status: "LISTEN".into(),
                pid: 4242,
            }]
        );
    }

    #[test]
    fn tcp6_rows_decode_with_family_inet6_and_go_ip_formatting() {
        let tree = ProcTree::new();
        // gopsutil reads tcp4 first and errors the whole scan when it is
        // missing, so a legal proc tree carries both files.
        tree.net_file(
            "tcp",
            &format!(
                "{HEADER}{}",
                listen_row("0100007F:1F90", "00000000:0000", "0A", "1")
            ),
        );
        tree.pid_holding(5, &[("1", 0)]);
        // ::1 in the proc format is 16 reversed-4-byte-group hex bytes:
        // 00000000000000000000000001000000, port 1F90 (8080).
        tree.net_file(
            "tcp6",
            &format!(
                "{HEADER}{}",
                listen_row(
                    "00000000000000000000000001000000:1F90",
                    "00000000000000000000000000000000:0000",
                    "0A",
                    "9"
                )
            ),
        );
        tree.pid_holding(77, &[("9", 3)]);
        let rows = tree.source().connections().unwrap();
        let v6 = rows
            .iter()
            .find(|row| row.family == AF_INET6)
            .expect("the tcp6 row must be decoded with family AF_INET6");
        assert_eq!(v6.laddr.ip, "::1");
        assert_eq!(v6.laddr.port, 8080);
        assert_eq!(v6.status, "LISTEN");
        assert_eq!(v6.pid, 77);
    }

    #[test]
    fn missing_tcp6_is_ipv6_unsupported_but_missing_tcp_fails_the_scan() {
        let tree = ProcTree::new();
        tree.net_file(
            "tcp",
            &format!(
                "{HEADER}{}",
                listen_row("0100007F:1F90", "00000000:0000", "0A", "1")
            ),
        );
        tree.pid_holding(5, &[("1", 0)]);
        assert_eq!(tree.source().connections().unwrap().len(), 1);

        let empty = ProcTree::new();
        // No net files at all: the whole scan errors (gopsutil read fails),
        // which the scanner must not broadcast (scan.go:48-49).
        assert!(empty.source().connections().is_err());
    }

    #[test]
    fn unknown_status_hex_becomes_the_go_zero_value_and_short_rows_are_skipped() {
        let tree = ProcTree::new();
        tree.net_file(
            "tcp",
            &format!(
                "{HEADER}{}{}",
                listen_row("0100007F:1F90", "00000000:0000", "0C", "1"),
                "   0: 0100007F:1F90 00000000:0000 0A 00000000\n",
            ),
        );
        tree.pid_holding(5, &[("1", 0)]);
        let rows = tree.source().connections().unwrap();
        assert_eq!(rows.len(), 1, "the 5-field row is skipped");
        assert_eq!(
            rows[0].status, "",
            "unknown state hex reads as the Go zero value"
        );
    }

    #[test]
    fn dedupes_the_same_tuple_with_the_first_row_winning() {
        let tree = ProcTree::new();
        // The dedup key (net_linux.go:437-445) is
        // (sock_type, laddr, raddr, status) — family and pid are NOT in it,
        // so duplicate kernel rows collapse onto the first one seen.
        let first = listen_row("0100007F:1F90", "00000000:0000", "0A", "1");
        let second = listen_row("0100007F:1F90", "00000000:0000", "0A", "2");
        tree.net_file("tcp", &format!("{HEADER}{first}{second}"));
        tree.pid_holding(41, &[("1", 0)]);
        tree.pid_holding(42, &[("2", 0)]);
        let rows = tree.source().connections().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 41);
        assert_eq!(rows[0].fd, 0);
        assert_eq!(rows[0].family, AF_INET);
    }

    #[test]
    fn an_inode_no_process_holds_reports_pid_zero() {
        let tree = ProcTree::new();
        tree.net_file(
            "tcp",
            &format!(
                "{HEADER}{}",
                listen_row("0100007F:1F90", "00000000:0000", "0A", "777777")
            ),
        );
        let rows = tree.source().connections().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 0);
        assert_eq!(rows[0].fd, 0);
    }

    #[test]
    fn a_v4_mapped_tcp6_address_renders_dotted_like_go_net_ip_string() {
        // gopsutil renders with net.IP.String() (net_linux.go:658), whose
        // appendTo (Go ip.go:341-348) calls To4() FIRST: a v4-mapped 16-byte
        // address is replaced by its 4-byte form before AddrFromSlice, so it
        // renders dotted — "127.0.0.1". The `::ffff:a.b.c.d` form
        // (netip.appendTo4In6) is only reached when a 16-byte slice goes
        // straight to AddrFromSlice, a path gopsutil never takes.
        // 0000000000000000FFFF00000100007F (bytes 0..10 zero, 10..12 ffff,
        // then 7F 00 00 01 reversed per group) is exactly the row the kernel
        // writes for a ::ffff:127.0.0.1 listener.
        let tree = ProcTree::new();
        tree.net_file(
            "tcp",
            &format!(
                "{HEADER}{}",
                listen_row("0100007F:1F90", "00000000:0000", "0A", "1")
            ),
        );
        tree.pid_holding(5, &[("1", 0)]);
        tree.net_file(
            "tcp6",
            &format!(
                "{HEADER}{}",
                listen_row(
                    "0000000000000000FFFF00000100007F:1F90",
                    "00000000000000000000000000000000:0000",
                    "0A",
                    "3"
                )
            ),
        );
        tree.pid_holding(9, &[("3", 0)]);
        let rows = tree.source().connections().unwrap();
        // Select the tcp6 row itself (the tcp4 twin renders "127.0.0.1" and
        // would satisfy the assert vacuously).
        let tcp6_row = rows
            .iter()
            .find(|row| row.family == AF_INET6)
            .expect("the tcp6 row must be scanned");
        assert_eq!(tcp6_row.laddr.ip, "127.0.0.1");
    }

    #[test]
    fn a_real_loopback_listener_is_seen_through_the_production_procfs_layout() {
        // Real controlled listener on a high 127.0.0.1 port; the production
        // ProcNetSource at /proc must find it, with this test process's pid.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected_pid = std::process::id() as i32;
        let rows = ProcNetSource::proc_default().connections().unwrap();
        let found = rows
            .iter()
            .find(|row| row.status == "LISTEN" && row.laddr.port == u32::from(port))
            .expect("the bound listener must appear in the scan");
        assert_eq!(found.laddr.ip, "127.0.0.1");
        assert_eq!(found.pid, expected_pid);
    }

    /// The consequence the rendering feeds: the forwarder's filter lists
    /// 127.0.0.1/localhost/::1, and a guest listener bound ::ffff:127.0.0.1
    /// (AF_INET6, bindv6only=0 default — the only form a v4 loopback listener
    /// takes in tcp6) must be matched, i.e. rendered dotted, or the port is
    /// silently never forwarded.
    #[test]
    fn a_real_v4_mapped_listener_renders_dotted_and_hits_the_forwarder_filter() {
        let listener = std::net::TcpListener::bind("[::ffff:127.0.0.1]:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let rows = ProcNetSource::proc_default().connections().unwrap();
        let found = rows
            .iter()
            .find(|row| row.status == "LISTEN" && row.laddr.port == u32::from(port))
            .expect("the v4-mapped listener must appear in the tcp6 scan");
        assert_eq!(
            found.laddr.ip, "127.0.0.1",
            "the kernel row 0000000000000000FFFF00000100007F must render dotted, \
             exactly as gopsutil's net.IP.String() would"
        );
        let filter = ScannerFilter {
            state: "LISTEN".into(),
            ips: vec!["127.0.0.1".into(), "localhost".into(), "::1".into()],
        };
        assert!(
            filter.matches(found),
            "a ::ffff:127.0.0.1 listener must hit the 127.0.0.1 filter or the \
             port is silently never forwarded"
        );
    }
}
