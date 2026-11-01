// SPDX-License-Identifier: BSD-3-Clause
// Copyright 2009 The Go Authors.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
// * Redistributions of source code must retain the above copyright notice,
//   this list of conditions and the following disclaimer.
// * Redistributions in binary form must reproduce the above copyright notice,
//   this list of conditions and the following disclaimer in the documentation
//   and/or other materials provided with the distribution.
// * Neither the name of Google LLC nor the names of its contributors may be
//   used to endorse or promote products derived from this software without
//   specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
// POSSIBILITY OF SUCH DAMAGE.
//! Modified adaptation of Go 1.26.10 net/url: parse,
//! resolvePath and ResolveReference.
//! Primary url.go SHA256: 7bec001746c2cd512275c7269bc293f3d36cb004ddb193d736e02e361a3778aa
//! https://github.com/golang/go/blob/go1.26.10/src/net/url/url.go
//! Retain escaped paths independently: WHATWG resolution changes destinations
//! for backslashes and normalizes percent-encoded dot segments.
use std::io;

#[derive(Clone, Default)]
pub(super) struct GoUrl {
    pub scheme: String,
    pub authority: String,
    pub user: Option<(Vec<u8>, Vec<u8>)>,
    path: String,
    query: String,
    force_query: bool,
    fragment: String,
    opaque: String,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn unescape(value: &str) -> io::Result<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .ok_or_else(|| invalid("invalid URL escape"))?;
            let digit = |ch: u8| (ch as char).to_digit(16).map(|d| d as u8);
            let high = digit(hex[0]).ok_or_else(|| invalid("invalid URL escape"))?;
            let low = digit(hex[1]).ok_or_else(|| invalid("invalid URL escape"))?;
            result.push(high * 16 + low);
            i += 3;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    Ok(result)
}
fn escaped(value: &str, fragment: bool) -> io::Result<String> {
    let decoded = unescape(value)?;
    let safe = |byte: u8| {
        byte.is_ascii_alphanumeric()
            || b"-_.~$&+,/:;=@".contains(&byte)
            || fragment && b"!?()*".contains(&byte)
    };
    // Go retains RawPath/RawFragment only when the entire original spelling is
    // validEncoded. Otherwise it re-escapes the decoded bytes as one value.
    if value
        .bytes()
        .all(|byte| safe(byte) || b"!$&'()*+,;=:@[]%".contains(&byte))
    {
        return Ok(value.to_owned());
    }
    let mut result = String::new();
    for byte in decoded {
        if safe(byte) {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(result)
}
fn host_byte_allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-_.~!$&'()*+,;=:[\x5d<>\"".contains(&byte)
}
// Go unescape encodeHost / encodeZone, including their distinct percent rules.
fn unescape_host_part(value: &str, zone: bool) -> io::Result<String> {
    let mut index = 0;
    let bytes = value.as_bytes();
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let part = value
                .get(index..index + 3)
                .ok_or_else(|| invalid("invalid URL escape"))?;
            let decoded = unescape(part)?[0];
            if !zone && decoded < 128 && decoded != b'%'
                || zone && decoded != b'%' && decoded != b' ' && !host_byte_allowed(decoded)
            {
                return Err(invalid("invalid URL host escape"));
            }
            index += 3;
        } else {
            if bytes[index].is_ascii() && !host_byte_allowed(bytes[index]) {
                return Err(invalid("invalid character in host name"));
            }
            index += 1;
        }
    }
    String::from_utf8(unescape(value)?).map_err(|_| invalid("invalid UTF-8 hostname"))
}
fn decode_host(host: &str) -> io::Result<String> {
    if host.starts_with('[') {
        if let Some(zone) = host.find("%25") {
            let close = host
                .rfind(']')
                .ok_or_else(|| invalid("missing ']' in host"))?;
            if zone > close {
                return Err(invalid("invalid IP-literal"));
            }
            return Ok(format!(
                "{}{}{}",
                unescape_host_part(&host[..zone], false)?,
                unescape_host_part(&host[zone..close], true)?,
                unescape_host_part(&host[close..], false)?
            ));
        }
    }
    unescape_host_part(host, false)
}

impl GoUrl {
    pub fn parse(raw: &str) -> io::Result<Self> {
        let (raw, fragment) = raw.split_once('#').unwrap_or((raw, ""));
        if raw.bytes().any(|byte| byte < 32 || byte == 127) {
            return Err(invalid("net/url: invalid control character in URL"));
        }
        let mut value = Self {
            fragment: escaped(fragment, true)?,
            ..Self::default()
        };
        let mut rest = raw;
        for (index, byte) in raw.bytes().enumerate() {
            if byte == b':' {
                if index == 0 {
                    return Err(invalid("missing protocol scheme"));
                }
                value.scheme = raw[..index].to_ascii_lowercase();
                rest = &raw[index + 1..];
                break;
            }
            if !byte.is_ascii_alphabetic()
                && !(index > 0 && (byte.is_ascii_digit() || b"+-.".contains(&byte)))
            {
                break;
            }
        }
        if let Some((path, query)) = rest.split_once('?') {
            rest = path;
            value.query = query.to_owned();
            value.force_query = query.is_empty();
        }
        if !rest.starts_with('/') && !value.scheme.is_empty() {
            value.opaque = rest.to_owned();
            return Ok(value);
        }
        if value.scheme.is_empty()
            && rest
                .split('/')
                .next()
                .is_some_and(|part| part.contains(':'))
        {
            return Err(invalid("first path segment in URL cannot contain colon"));
        }
        if rest.starts_with("//") && (!value.scheme.is_empty() || !rest.starts_with("///")) {
            let (authority, path) = match rest[2..].find('/') {
                Some(index) => (&rest[2..2 + index], &rest[2 + index..]),
                None => (&rest[2..], ""),
            };
            rest = path;
            value.set_authority(authority)?;
        }
        value.path = escaped(rest, false)?;
        Ok(value)
    }
    fn set_authority(&mut self, authority: &str) -> io::Result<()> {
        let host = if let Some((user, host)) = authority.rsplit_once('@') {
            if user
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b"-._:~!$&'()*+,;=%@".contains(&b))
            {
                return Err(invalid("net/url: invalid userinfo"));
            }
            let (name, password) = user.split_once(':').unwrap_or((user, ""));
            self.user = Some((unescape(name)?, unescape(password)?));
            host
        } else {
            authority
        };
        let host = decode_host(host)?;
        if host.starts_with('[') {
            let end = host
                .find(']')
                .ok_or_else(|| invalid("missing ']' in host"))?;
            host[1..end]
                .split('%')
                .next()
                .unwrap_or_default()
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| invalid("invalid IP-literal"))?;
            if !host[end + 1..].is_empty() && !host[end + 1..].starts_with(':') {
                return Err(invalid("invalid port"));
            }
        } else if host.matches(':').count() > 1 {
            return Err(invalid("invalid port"));
        }
        if let Some((_, port)) = host.rsplit_once(':') {
            if !host.ends_with(']') && !port.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid("invalid port"));
            }
        }
        self.authority = host.to_owned();
        Ok(())
    }
    pub fn resolve(&self, reference: &str) -> io::Result<Self> {
        let mut next = Self::parse(reference)?;
        let absolute = !next.scheme.is_empty();
        if !absolute {
            next.scheme.clone_from(&self.scheme);
        }
        if absolute || !next.authority.is_empty() || next.user.is_some() {
            next.path = resolve_path(&next.path, "");
            return Ok(next);
        }
        if !next.opaque.is_empty() {
            return Ok(next);
        }
        if next.path.is_empty() && !next.force_query && next.query.is_empty() {
            next.query.clone_from(&self.query);
            if next.fragment.is_empty() {
                next.fragment.clone_from(&self.fragment);
            }
        }
        if next.path.is_empty() && !self.opaque.is_empty() {
            next.opaque.clone_from(&self.opaque);
            return Ok(next);
        }
        next.authority.clone_from(&self.authority);
        next.user.clone_from(&self.user);
        next.path = resolve_path(&self.path, &next.path);
        Ok(next)
    }
    pub fn hostname(&self) -> &str {
        if self.authority.starts_with('[') {
            self.authority
                .split(']')
                .next()
                .unwrap_or_default()
                .trim_start_matches('[')
        } else {
            self.authority.split(':').next().unwrap_or_default()
        }
    }
    pub fn dial_host(&self) -> io::Result<String> {
        let host = self.hostname();
        if host.is_ascii() {
            return Ok(host.to_owned());
        }
        let ascii = url::quirks::domain_to_ascii(host);
        if ascii.is_empty() {
            return Err(invalid("invalid international hostname"));
        }
        Ok(ascii)
    }
    pub fn wire_authority(&self) -> io::Result<String> {
        if self.authority.starts_with('[') {
            let end = self
                .authority
                .find(']')
                .ok_or_else(|| invalid("missing ']' in host"))?;
            let host = self.hostname().split('%').next().unwrap_or_default();
            return Ok(format!("[{host}]{}", &self.authority[end + 1..]));
        }
        Ok(format!(
            "{}{}",
            self.dial_host()?,
            &self.authority[self.hostname().len()..]
        ))
    }
    pub fn scoped_address(&self) -> io::Result<Option<std::net::SocketAddrV6>> {
        if !self.authority.starts_with('[') {
            return Ok(None);
        }
        let (ip, zone) = self
            .hostname()
            .split_once('%')
            .unwrap_or((self.hostname(), ""));
        let ip = ip.parse().map_err(|_| invalid("invalid IP-literal"))?;
        let scope = if zone.is_empty() {
            0
        } else {
            let name = std::ffi::CString::new(zone).map_err(|_| invalid("invalid IPv6 zone"))?;
            // SAFETY: name is a NUL-terminated string live for the duration of
            // this read-only interface-name lookup; libc retains no pointer.
            let index = unsafe { nix::libc::if_nametoindex(name.as_ptr()) };
            if index == 0 {
                zone_decimal_fallback(zone)
            } else {
                index
            }
        };
        Ok(Some(std::net::SocketAddrV6::new(
            ip,
            self.port()?,
            0,
            scope,
        )))
    }
    pub fn port(&self) -> io::Result<u16> {
        let end = if self.authority.starts_with('[') {
            self.authority.find(']').map_or(0, |i| i + 1)
        } else {
            0
        };
        if let Some((_, port)) = self.authority[end..].rsplit_once(':') {
            if !port.is_empty() {
                return port.parse().map_err(|_| invalid("invalid port"));
            }
        }
        match self.scheme.as_str() {
            "http" => Ok(80),
            "https" => Ok(443),
            _ => Err(invalid("unsupported URL scheme")),
        }
    }
    pub fn target(&self) -> String {
        let mut result = if !self.opaque.is_empty() {
            self.opaque.clone()
        } else if self.path.is_empty() {
            "/".into()
        } else {
            self.path.clone()
        };
        if self.force_query || !self.query.is_empty() {
            result.push('?');
            result.push_str(&self.query);
        }
        result
    }
    pub fn referer(&self) -> String {
        let mut authority = String::new();
        for byte in self.authority.bytes() {
            if host_byte_allowed(byte) {
                authority.push(byte as char);
            } else {
                authority.push_str(&format!("%{byte:02X}"));
            }
        }
        let mut result = if self.opaque.is_empty() {
            format!("{}://{}{}", self.scheme, authority, self.path)
        } else {
            format!("{}:{}", self.scheme, self.opaque)
        };
        if self.force_query || !self.query.is_empty() {
            result.push('?');
            result.push_str(&self.query);
        }
        if !self.fragment.is_empty() {
            result.push('#');
            result.push_str(&self.fragment);
        }
        result
    }
}
// Modified from Go 1.26.10 net/interface.go ipv6ZoneCache.index and
// net/parse.go dtoi: unknown names use the decimal prefix, saturating at big.
// The caller intentionally ignores dtoi's consumed-length and success flags.
fn zone_decimal_fallback(zone: &str) -> u32 {
    const BIG: u32 = 0xffffff;
    let mut number = 0;
    for byte in zone.bytes().take_while(u8::is_ascii_digit) {
        number = number * 10 + u32::from(byte - b'0');
        if number >= BIG {
            return BIG;
        }
    }
    number
}

// Go's resolvePath acts on escaped segments: only literal '.' and '..' fold.
fn resolve_path(base: &str, reference: &str) -> String {
    let full = if reference.is_empty() {
        base.to_owned()
    } else if reference.starts_with('/') {
        reference.to_owned()
    } else {
        format!(
            "{}{reference}",
            base.rfind('/').map_or("", |i| &base[..i + 1])
        )
    };
    if full.is_empty() {
        return full;
    }
    let mut destination = String::from("/");
    let mut first = true;
    let mut last = "";
    for element in full.split('/') {
        last = element;
        match element {
            "." => {
                first = false;
            }
            ".." => {
                if let Some(index) = destination[1..].rfind('/') {
                    destination.truncate(index + 1);
                } else {
                    destination.truncate(1);
                }
                first = destination.len() == 1;
            }
            _ => {
                if !first {
                    destination.push('/');
                }
                destination.push_str(element);
                first = false;
            }
        }
    }
    if matches!(last, "." | "..") {
        destination.push('/');
    }
    if destination.starts_with("//") {
        destination.remove(0);
    }
    destination
}

#[cfg(test)]
mod tests {
    use super::GoUrl;

    #[test]
    fn escaped_unicode_host_decodes_before_idna_without_dns() {
        let url = GoUrl::parse("http://b%C3%BCcher.example:8080/metadata").unwrap();
        assert_eq!(url.hostname(), "bücher.example");
        assert_eq!(url.dial_host().unwrap(), "xn--bcher-kva.example");
        assert_eq!(url.port().unwrap(), 8080);
        assert_eq!(url.wire_authority().unwrap(), "xn--bcher-kva.example:8080");
        for invalid in [
            "http://%61.example/",
            "http://[::1%25bad%0a]/",
            "http://[::1%25bad%2f]/",
        ] {
            assert!(GoUrl::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn unknown_zone_uses_go_decimal_prefix_fallback_and_saturation() {
        for (zone, expected) in [
            ("noenvd0", 0),
            ("3suffix", 3),
            ("+3", 0),
            ("-1", 0),
            ("0007tail", 7),
            ("16777215", 0xffffff),
            ("4294967295", 0xffffff),
            ("9999999999999999999999", 0xffffff),
        ] {
            let url = GoUrl::parse(&format!("http://[::1%25{zone}]:8080/")).unwrap();
            assert_eq!(
                url.scoped_address().unwrap().unwrap().scope_id(),
                expected,
                "{zone}"
            );
        }
    }

    #[test]
    fn unicode_referer_uses_go_encode_host_and_omits_userinfo() {
        for raw in [
            "http://b%C3%BCcher.example:8080",
            "http://user:secret@bücher.example:8080",
        ] {
            let url = GoUrl::parse(raw).unwrap();
            assert_eq!(url.referer(), "http://b%C3%BCcher.example:8080");
        }
    }

    #[test]
    fn ipv6_zone_decodes_and_referer_keeps_empty_path() {
        let url = GoUrl::parse("http://[::1%25l%6f]:8080").unwrap();
        assert_eq!(url.hostname(), "::1%lo");
        assert_eq!(url.dial_host().unwrap(), "::1%lo");
        assert_eq!(url.referer(), "http://[::1%25lo]:8080");
        assert_eq!(url.wire_authority().unwrap(), "[::1]:8080");
        let address = url.scoped_address().unwrap().unwrap();
        assert_eq!(address.ip(), &std::net::Ipv6Addr::LOCALHOST);
        assert_eq!(address.port(), 8080);
        assert!(address.scope_id() > 0);
        assert_eq!(
            GoUrl::parse("http://[::1%253]:99")
                .unwrap()
                .scoped_address()
                .unwrap()
                .unwrap()
                .scope_id(),
            3
        );
    }
}
