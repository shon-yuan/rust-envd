// SPDX-License-Identifier: Apache-2.0
//! Hyperloop init side-effects adapted from E2B runtime
//! 51083a83cc0903185d992725ea6bb33683814b29
//! `packages/envd/internal/api/init.go` (`SetupHyperloop`, `rewriteHostsFile`,
//! `getIPFamily`) and the observable hosts-file semantics of
//! `github.com/txn2/txeh v1.8.1`.
//!
//! txeh is Apache-2.0 (Copyright 2019 txen2x / txn2 contributors); this module
//! reimplements only the behavior the pinned Go oracle exercises rather than
//! copying its code, and records that provenance here as required by the
//! license. Rendering contract from txeh v1.8.1: parsed address lines are
//! re-rendered as `%-16s %s` (+ ` # comment`); comment, blank, and unknown
//! lines keep their raw bytes; every rendered line ends with `\n`. `Save` is
//! an in-place `0644` write, not a tmp+rename. Go's `TrimSpace`, `Fields`,
//! and `ToLower` are Unicode aware; this module trims and lowercases ASCII
//! hosts tokens only, which is equivalent for real hosts files and the pinned
//! flows (disclosed deviation).

use std::{fmt, io, net::IpAddr, path::Path};

/// The single hostname the pinned oracle rewrites.
pub const EVENTS_HOST: &str = "events.e2b.local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IPFamily {
    V4,
    V6,
}

#[derive(Debug)]
pub enum RewriteError {
    IpFamily(Box<RewriteError>),
    ParseAddr { source: String },
    OpenHosts { path: String, source: io::Error },
    WriteHosts { path: String, source: io::Error },
}

impl fmt::Display for RewriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IpFamily(inner) => {
                write!(formatter, "failed to get ip family: {inner}")
            }
            Self::ParseAddr { source } => {
                write!(formatter, "failed to parse IP address: {source}")
            }
            Self::OpenHosts { path, source } => write!(
                formatter,
                "failed to create hosts: read hosts file {path}: {source}"
            ),
            Self::WriteHosts { path, source } => {
                write!(formatter, "write hosts file {path}: {source}")
            }
        }
    }
}

/// Mirrors Go `getIPFamily`: `netip.ParseAddr` accepts a `%zone` suffix and
/// classifies by the address part alone, but an empty zone is an error
/// ("zone must be a non-empty string"). The zone survives in the caller's
/// request string, while txeh's `net.ParseIP` downstream rejects zoned
/// addresses (silent `AddHost` no-op, unchanged bytes, env still stored).
pub fn get_ip_family(address: &str) -> Result<IPFamily, RewriteError> {
    let bare = match address.split_once('%') {
        Some((_, "")) => {
            return Err(RewriteError::ParseAddr {
                source: "zone must be a non-empty string".to_string(),
            })
        }
        Some((head, _zone)) => head,
        None => address,
    };
    let parsed: IpAddr =
        bare.parse()
            .map_err(|error: std::net::AddrParseError| RewriteError::ParseAddr {
                source: error.to_string(),
            })?;
    Ok(if parsed.is_ipv4() {
        IPFamily::V4
    } else {
        IPFamily::V6
    })
}

/// Mirrors Go `rewriteHostsFile`: read + parse the file, skip the write when
/// the hostname already resolves to `address` in the same family, otherwise
/// add the host and save in place. Callers store the env on any `Ok`.
pub fn rewrite_hosts_file(address: &str, path: &Path) -> Result<(), RewriteError> {
    let family = get_ip_family(address).map_err(|error| RewriteError::IpFamily(Box::new(error)))?;
    let path_display = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|source| RewriteError::OpenHosts {
        path: path_display.clone(),
        source,
    })?;
    let mut hosts = HostsFile::parse(&bytes);
    if hosts
        .lookup(EVENTS_HOST.as_bytes(), family)
        .is_some_and(|current| current == address.as_bytes())
    {
        return Ok(());
    }
    hosts.add_host(address.as_bytes(), EVENTS_HOST.as_bytes());
    std::fs::write(path, hosts.render()).map_err(|source| RewriteError::WriteHosts {
        path: path_display,
        source,
    })?;
    Ok(())
}

#[derive(Debug)]
enum Line {
    /// Comment, blank, and unknown lines keep their raw bytes verbatim.
    Raw(Vec<u8>),
    Entry {
        address: Vec<u8>,
        hostnames: Vec<Vec<u8>>,
        comment: Option<Vec<u8>>,
    },
}

#[derive(Debug, Default)]
pub struct HostsFile {
    lines: Vec<Line>,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r' | b' ')
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !is_space(*byte))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !is_space(*byte))
        .map_or(start, |position| position + 1);
    &bytes[start..end]
}

fn fields(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|byte| is_space(*byte))
        .filter(|part| !part.is_empty())
        .collect()
}

fn ascii_lowercase(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|byte| byte.to_ascii_lowercase()).collect()
}

/// Go parses line addresses with `net.ParseIP`; unparseable lines never match.
fn parse_ip(bytes: &[u8]) -> Option<IpAddr> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse().ok())
}

/// txeh classifies addresses with `To4()`: IPv4-mapped IPv6 counts as V4.
fn txeh_family(parsed: IpAddr) -> IPFamily {
    match parsed {
        IpAddr::V4(_) => IPFamily::V4,
        IpAddr::V6(ipv6) if ipv6.to_ipv4_mapped().is_some() => IPFamily::V4,
        IpAddr::V6(_) => IPFamily::V6,
    }
}

impl HostsFile {
    /// Mirrors txeh v1.8.1 `ParseHostsFromString` (CRLF normalized, trailing
    /// newline drops its empty tail, first `#` starts a comment, one-field
    /// lines stay unknown).
    pub fn parse(input: &[u8]) -> Self {
        let mut normalized = Vec::with_capacity(input.len());
        let mut iterator = input.iter().copied().peekable();
        while let Some(byte) = iterator.next() {
            if byte == b'\r' && iterator.peek() == Some(&b'\n') {
                continue;
            }
            normalized.push(byte);
        }
        let mut lines = Vec::new();
        let mut chunks: Vec<&[u8]> = normalized.split(|byte| *byte == b'\n').collect();
        if chunks.last().is_some_and(|last| last.is_empty()) {
            chunks.pop();
        }
        for raw in chunks {
            let line = raw.to_vec();
            let trimmed = trim(raw);
            if trimmed.first() == Some(&b'#') || trimmed.is_empty() {
                lines.push(Line::Raw(line));
                continue;
            }
            let hash = trimmed.iter().position(|byte| *byte == b'#');
            let (head, comment) = match hash {
                Some(at) => (&trimmed[..at], Some(trim(&trimmed[at + 1..]).to_vec())),
                None => (trimmed, None),
            };
            let parts = fields(head);
            if parts.len() > 1 {
                lines.push(Line::Entry {
                    address: ascii_lowercase(parts[0]),
                    hostnames: parts[1..]
                        .iter()
                        .map(|part| ascii_lowercase(part))
                        .collect(),
                    comment,
                });
            } else {
                lines.push(Line::Raw(line));
            }
        }
        Self { lines }
    }

    /// Mirrors txeh `HostAddressLookup`: first entry whose address parses in
    /// `family` and lists `host`.
    pub fn lookup(&self, host: &[u8], family: IPFamily) -> Option<Vec<u8>> {
        self.lookup_index(host, family).map(|(_, address)| address)
    }

    /// Mirrors txeh `AddHost`: lowercase/trim, silently ignore unparseable
    /// addresses, remove an existing same-family entry for the host unless the
    /// new address is localhost (txeh's `removeHostFromLineLocked` guard),
    /// merge onto an existing line with the same address and comment, else
    /// append a new entry.
    pub fn add_host(&mut self, address: &[u8], host: &[u8]) {
        let address = ascii_lowercase(trim(address));
        let host = ascii_lowercase(trim(host));
        let Some(parsed) = parse_ip(&address) else {
            return;
        };
        let family = txeh_family(parsed);
        let existing = self.lookup_index(host.as_slice(), family);
        if let Some((index, current)) = existing {
            if current == address {
                return;
            }
            if !is_localhost(&address) {
                let Line::Entry { hostnames, .. } = &mut self.lines[index] else {
                    unreachable!("lookup only returns entry indexes")
                };
                if let Some(at) = hostnames.iter().position(|name| name == &host) {
                    hostnames.remove(at);
                }
                if matches!(&self.lines[index], Line::Entry { hostnames, .. } if hostnames.is_empty())
                {
                    self.lines.remove(index);
                }
            }
        }
        for line in &mut self.lines {
            let Line::Entry {
                address: line_address,
                hostnames,
                comment,
            } = line
            else {
                continue;
            };
            // Go compares hfl.Comment == "": bare-`#` lines (empty comment)
            // are merge targets just like uncommented ones.
            if line_address == &address && comment.as_ref().is_none_or(|part| part.is_empty()) {
                hostnames.push(host);
                return;
            }
        }
        self.lines.push(Line::Entry {
            address: address.to_vec(),
            hostnames: vec![host],
            comment: None,
        });
    }

    fn lookup_index(&self, host: &[u8], family: IPFamily) -> Option<(usize, Vec<u8>)> {
        for (index, line) in self.lines.iter().enumerate() {
            let Line::Entry {
                address, hostnames, ..
            } = line
            else {
                continue;
            };
            let Some(parsed) = parse_ip(address) else {
                continue;
            };
            if txeh_family(parsed) == family && hostnames.iter().any(|candidate| candidate == host)
            {
                return Some((index, address.clone()));
            }
        }
        None
    }

    /// Mirrors txeh `RenderHostsFile`: `%-16s %s` per entry (+ ` # comment`),
    /// raw lines verbatim, always `\n`-terminated.
    pub fn render(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for line in &self.lines {
            match line {
                Line::Raw(raw) => {
                    output.extend_from_slice(raw);
                }
                Line::Entry {
                    address,
                    hostnames,
                    comment,
                } => {
                    output.extend_from_slice(address);
                    if address.len() < 16 {
                        output.extend(std::iter::repeat_n(b' ', 16 - address.len()));
                    }
                    // One space before each hostname: the field separator after
                    // the address, then the single-space join between names.
                    for hostname in hostnames {
                        output.push(b' ');
                        output.extend_from_slice(hostname);
                    }
                    // Go renders the suffix only when hfl.Comment != "".
                    if let Some(comment) = comment.as_deref().filter(|part| !part.is_empty()) {
                        output.extend_from_slice(b" # ");
                        output.extend_from_slice(comment);
                    }
                }
            }
            output.push(b'\n');
        }
        output
    }
}

/// Mirrors txeh v1.8.1's unanchored `((127\.(\d{1,3}\.){2}\d{1,3})|(::1)$)`.
fn is_localhost(address: &[u8]) -> bool {
    if address.ends_with(b"::1") {
        return true;
    }
    (0..address.len()).any(|start| starts_localhost(&address[start..]))
}

fn starts_localhost(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"127.") {
        return false;
    }
    let mut position = 4;
    for group in 0..3 {
        let take = bytes[position..]
            .iter()
            .take(3)
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if take == 0 {
            return false;
        }
        position += take;
        if group == 2 {
            return true;
        }
        if bytes.get(position) != Some(&b'.') {
            // Greedy \d{1,3} may need to give a digit back before the dot.
            let mut shortened = take;
            loop {
                shortened -= 1;
                if shortened == 0 {
                    return false;
                }
                if bytes.get(position - take + shortened) == Some(&b'.') {
                    position -= take - shortened;
                    position += 1;
                    break;
                }
            }
        } else {
            position += 1;
        }
    }
    unreachable!("loop returns on group 2")
}
