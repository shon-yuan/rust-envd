//! Go `net/http` request-head validation.
//!
//! The parity oracle is Go envd 0.10.2, whose HTTP server is
//! `net/http.Server`. Hyper rejects malformed requests with its own 400
//! response, which differs from Go byte-for-byte (status text body, header
//! set, date, and connection handling). This module reproduces the decisions
//! of Go's `conn.readRequest` (request line, URL, MIME headers, Host rules,
//! Content-Length, Transfer-Encoding) and the exact error bytes of
//! `conn.serve`, so the daemon can answer them itself before hyper ever sees
//! the bytes.
//!
//! The check order mirrors `(*conn).readRequest` (server.go:1046-1087):
//! request line and URL inside `readRequestLimit`, then the transfer framing
//! (501 for an unsupported Transfer-Encoding, 400 for a bad Content-Length),
//! then `http1ServerSupportsRequest` (505), then the Host and header-value
//! validations that produce `statusError` texts.
//!
//! Only acceptance and framing are modelled. Once [`parse`] returns a
//! [`Head`], the same bytes are also valid for hyper and are handed over
//! unchanged.

/// `net/http.Server`'s default `MaxHeaderBytes` plus its bufio slop
/// (`initialReadLimitSize`). Exceeding it yields Go's 431 response.
pub(crate) const MAX_HEAD_BYTES: usize = (1 << 20) + 4096;

/// `net/http.DefaultMaxHeaderValueCount`.
pub(crate) const MAX_HEADER_VALUES: usize = 500;

/// Which Go error response the guard owes the client, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reject {
    /// Generic `400 Bad Request` (request-line, URL, MIME, framing errors).
    BadRequest,
    /// `statusError{400, "missing required Host header"}`.
    MissingHost,
    /// `statusError{400, "malformed Host header"}`.
    MalformedHost,
    /// `statusError{400, "invalid header name"}`.
    InvalidHeaderName,
    /// `statusError{400, "invalid header value"}`.
    InvalidHeaderValue,
    /// `errTooLarge` — `431 Request Header Fields Too Large`.
    TooLarge,
    /// `unsupportedTEError` — `501 Not Implemented`.
    UnsupportedTransferEncoding,
    /// Unsupported protocol version — `505 HTTP Version Not Supported`.
    VersionNotSupported,
}

impl Reject {
    /// The exact bytes Go's `net/http` writes for this rejection.
    ///
    /// From `(*conn).serve`: the shared `errorHeaders` block, with the
    /// generic `publicErr` body for a plain 400 and `statusError` text for
    /// the host/header variants. None of these carry a Date header.
    pub(crate) fn response(self) -> &'static [u8] {
        match self {
            Self::BadRequest => b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request",
            Self::MissingHost => b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: missing required Host header",
            Self::MalformedHost => b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: malformed Host header",
            Self::InvalidHeaderName => b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: invalid header name",
            Self::InvalidHeaderValue => b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: invalid header value",
            Self::TooLarge => b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n431 Request Header Fields Too Large",
            Self::UnsupportedTransferEncoding => b"HTTP/1.1 501 Not Implemented\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\nUnsupported transfer encoding",
            Self::VersionNotSupported => b"HTTP/1.1 505 HTTP Version Not Supported\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n505 HTTP Version Not Supported: unsupported protocol version",
        }
    }
}

/// How the request body that follows the head is framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Framing {
    /// No body: neither Content-Length nor chunked Transfer-Encoding.
    Empty,
    /// A fixed-length body.
    Length(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// A CONNECT request: Go answers it and never reuses the connection.
    Close,
}

/// An accepted request head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Head {
    /// Length of the head in bytes, including any skipped leading CR/LF.
    pub(crate) len: usize,
    /// Whether the method is POST (Go tolerates leading CR/LF only after it).
    pub(crate) method_post: bool,
    /// Body framing that follows.
    pub(crate) framing: Framing,
}

/// One logical header after continuation lines were merged.
struct Header {
    name: Vec<u8>,
    value: Vec<u8>,
}

/// Parse one request head from the front of `buf`.
///
/// `skip_leading_crlf` mirrors Go's tolerance after a POST request
/// (`numLeadingCRorLF` over a four-byte peek).
///
/// Returns `Ok(None)` while more bytes are needed. A complete but rejected
/// head returns the response kind; callers must not forward those bytes.
pub(crate) fn parse(buf: &[u8], skip_leading_crlf: bool) -> Result<Option<Head>, Reject> {
    match parse_inner(buf, skip_leading_crlf) {
        // Go's limited reader turns an oversized head into `errTooLarge`
        // before the request line is ever completed.
        Ok(None) if buf.len() > MAX_HEAD_BYTES => Err(Reject::TooLarge),
        other => other,
    }
}

/// Whether EOF at this point makes Go write a `400 Bad Request`.
///
/// `bufio.ReadLine` returns any non-empty partial line at EOF as a complete
/// line, so even a truncated request line reaches the parser and fails there.
/// Only an empty remainder — no bytes at all, or nothing left after Go
/// discards a POST's leading CR/LF — closes the connection silently.
pub(crate) fn eof_gets_reply(buf: &[u8], skip_leading_crlf: bool) -> bool {
    let rest = if skip_leading_crlf {
        skip_leading_crlf_bytes(buf)
    } else {
        buf
    };
    !rest.is_empty()
}

fn skip_leading_crlf_bytes(buf: &[u8]) -> &[u8] {
    let leading = buf
        .iter()
        .take(4)
        .take_while(|&&b| b == b'\r' || b == b'\n')
        .count();
    &buf[leading..]
}

fn parse_inner(buf: &[u8], skip_leading_crlf: bool) -> Result<Option<Head>, Reject> {
    let start = if skip_leading_crlf {
        buf.len() - skip_leading_crlf_bytes(buf).len()
    } else {
        0
    };
    let Some((after_line, line)) = next_line(buf, start) else {
        return Ok(None);
    };
    let (method, uri, proto) = request_line(line)?;
    if !is_token(method) {
        return Err(Reject::BadRequest);
    }
    let (major, minor) = parse_http_version(proto).ok_or(Reject::BadRequest)?;
    if !valid_request_uri(uri) {
        return Err(Reject::BadRequest);
    }
    // `http1ServerSupportsRequest` accepts the HTTP/2 connection preface line,
    // but hyper's HTTP/1 parser cannot (it rejects `HTTP/2.0` outright, and its
    // h2 sniffing wants the full 24-byte preface). Go's observable answer to
    // the preface is a 400, so the guard answers that directly instead of
    // handing hyper a head it would reject with its own 505.
    if method == b"PRI" && uri == b"*" && (major, minor) == (2, 0) {
        return Err(Reject::BadRequest);
    }

    let is_connect = method == b"CONNECT";
    let proto_at_least_11 = major > 1 || minor >= 1;
    let mut headers: Vec<Header> = Vec::new();
    let mut count = 0usize;
    let mut pos = after_line;
    loop {
        let Some((after, line)) = next_line(buf, pos) else {
            return Ok(None);
        };
        pos = after;
        if line.is_empty() {
            break;
        }
        if line[0] == b' ' || line[0] == b'\t' {
            // Continuation line: Go appends it with a single space and
            // trims the previous line, this line, and the separator.
            let Some(last) = headers.last_mut() else {
                return Err(Reject::BadRequest);
            };
            last.value.push(b' ');
            last.value.extend_from_slice(trim(line));
            continue;
        }
        count += 1;
        if count > MAX_HEADER_VALUES {
            return Err(Reject::TooLarge);
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            return Err(Reject::BadRequest);
        };
        let raw_name = &line[..colon];
        // `canonicalMIMEHeaderKey` accepts spaces before the colon and lets
        // the later `ValidHeaderFieldName` check report them; anything else
        // outside the token set is a plain MIME error.
        if raw_name.is_empty()
            || !raw_name
                .iter()
                .all(|&b| valid_header_field_byte(b) || b == b' ')
        {
            return Err(Reject::BadRequest);
        }
        let raw_value = &line[colon + 1..];
        if !raw_value.iter().all(|&b| valid_header_value_byte(b)) {
            return Err(Reject::BadRequest);
        }
        headers.push(Header {
            name: raw_name.iter().map(u8::to_ascii_lowercase).collect(),
            value: trim(raw_value).to_vec(),
        });
    }

    // `readMIMEHeader` trims the merged line (the first line is right-trimmed
    // before continuations are joined) and then left-trims the value, so the
    // separator space before a continuation never survives.
    for header in &mut headers {
        header.value = trim(&header.value).to_vec();
    }

    let host: Vec<&[u8]> = headers_with(&headers, b"host")
        .map(|header| header.value.as_slice())
        .collect();
    let content_length: Vec<&[u8]> = headers_with(&headers, b"content-length")
        .map(|header| header.value.as_slice())
        .collect();
    let transfer_encoding: Vec<&[u8]> = headers_with(&headers, b"transfer-encoding")
        .map(|header| header.value.as_slice())
        .collect();

    // `readRequestLimit`: more than one Host line is a hard parse error.
    if host.len() > 1 {
        return Err(Reject::BadRequest);
    }
    // `parseTransferEncoding` (transfer.go:638-664): HTTP/1.1 only, exactly
    // one field value, and it must be `chunked`; everything else is 501.
    let chunked = if !transfer_encoding.is_empty() && proto_at_least_11 {
        if transfer_encoding.len() != 1 || !eq_ignore_ascii_case(transfer_encoding[0], b"chunked") {
            return Err(Reject::UnsupportedTransferEncoding);
        }
        true
    } else {
        false
    };
    // `fixLength` (transfer.go:668-760) validates Content-Length even when
    // chunked already decided the framing, then lets Transfer-Encoding win.
    let mut length = 0u64;
    if let Some((first, rest)) = content_length.split_first() {
        if rest.iter().any(|other| other != first) {
            return Err(Reject::BadRequest);
        }
        if first.is_empty() {
            return Err(Reject::BadRequest);
        }
        for &b in *first {
            if !b.is_ascii_digit() {
                return Err(Reject::BadRequest);
            }
            length = length
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(b - b'0')))
                .filter(|v| *v <= i64::MAX as u64)
                .ok_or(Reject::BadRequest)?;
        }
    }
    // `http1ServerSupportsRequest`: only HTTP/1.x (and the preface, handled
    // above) reaches a handler.
    if major != 1 {
        return Err(Reject::VersionNotSupported);
    }
    if proto_at_least_11
        && host.is_empty()
        && !is_connect
        && !is_h2_upgrade(method, major, minor, &headers)
    {
        return Err(Reject::MissingHost);
    }
    if let Some(value) = host.first() {
        if !value.iter().all(|&b| valid_host_byte(b)) {
            return Err(Reject::MalformedHost);
        }
    }
    // `(*conn).readRequest`: names were accepted by textproto even with a
    // space before the colon; the server then rejects non-token names.
    for header in &headers {
        if !header.name.iter().all(|&b| valid_header_field_byte(b)) {
            return Err(Reject::InvalidHeaderName);
        }
        if !header.value.iter().all(|&b| valid_header_value_byte(b)) {
            return Err(Reject::InvalidHeaderValue);
        }
    }

    let framing = if is_connect {
        Framing::Close
    } else if chunked {
        Framing::Chunked
    } else if length > 0 {
        Framing::Length(length)
    } else {
        Framing::Empty
    };
    Ok(Some(Head {
        len: pos,
        method_post: method == b"POST",
        framing,
    }))
}

fn headers_with<'a>(
    headers: &'a [Header],
    name: &'static [u8],
) -> impl Iterator<Item = &'a Header> + 'a {
    headers.iter().filter(move |header| header.name == name)
}

/// Split one LF-terminated line, stripping one trailing CR.
fn next_line(buf: &[u8], pos: usize) -> Option<(usize, &[u8])> {
    let rel = buf.get(pos..)?.iter().position(|&b| b == b'\n')?;
    let end = pos + rel;
    let mut line = &buf[pos..end];
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    Some((end + 1, line))
}

/// The method, request target and protocol version of one request line.
type RequestLine<'a> = (&'a [u8], &'a [u8], &'a [u8]);

fn request_line(line: &[u8]) -> Result<RequestLine<'_>, Reject> {
    let first = line
        .iter()
        .position(|&b| b == b' ')
        .ok_or(Reject::BadRequest)?;
    let rest = &line[first + 1..];
    let second = rest
        .iter()
        .position(|&b| b == b' ')
        .ok_or(Reject::BadRequest)?;
    Ok((&line[..first], &rest[..second], &rest[second + 1..]))
}

/// `net/http.ParseHTTPVersion`; `http1ServerSupportsRequest` checks the rest.
fn parse_http_version(proto: &[u8]) -> Option<(u8, u8)> {
    if proto == b"HTTP/1.1" {
        return Some((1, 1));
    }
    if proto == b"HTTP/1.0" {
        return Some((1, 0));
    }
    if proto.len() == 8 && proto.starts_with(b"HTTP/") && proto[6] == b'.' {
        let major = (proto[5] as char).to_digit(10)? as u8;
        let minor = (proto[7] as char).to_digit(10)? as u8;
        return Some((major, minor));
    }
    None
}

/// `net/http.Request.isH2Upgrade`: Connection contains `Upgrade` and the
/// Upgrade header is `h2c`.
fn is_h2_upgrade(method: &[u8], major: u8, minor: u8, headers: &[Header]) -> bool {
    if method == b"PRI" || major != 1 || minor < 1 {
        return false;
    }
    let mut connection_upgrade = false;
    let mut upgrade_h2c = false;
    for header in headers {
        if header.name == b"connection" {
            connection_upgrade |= header
                .value
                .split(|&b| b == b',')
                .any(|token| eq_ignore_ascii_case(trim(token), b"upgrade"));
        } else if header.name == b"upgrade" {
            upgrade_h2c |= eq_ignore_ascii_case(trim(&header.value), b"h2c");
        }
    }
    connection_upgrade && upgrade_h2c
}

fn trim(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if *first != b' ' && *first != b'\t' {
            break;
        }
        bytes = rest;
    }
    while let [rest @ .., last] = bytes {
        if *last != b' ' && *last != b'\t' {
            break;
        }
        bytes = rest;
    }
    bytes
}

fn eq_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// RFC 7230 token (Go's `validMethod` / `validHeaderFieldByte`).
fn is_token(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|&b| valid_token_byte(b))
}

fn valid_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn valid_header_field_byte(b: u8) -> bool {
    valid_token_byte(b)
}

fn valid_header_value_byte(b: u8) -> bool {
    b == b'\t' || (0x20..=0x7e).contains(&b) || b >= 0x80
}

fn valid_host_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'$'
                | b'%'
                | b'&'
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b'-'
                | b'.'
                | b':'
                | b';'
                | b'='
                | b'['
                | b'\''
                | b']'
                | b'_'
                | b'~'
        )
}

/// `url.ParseRequestURI` acceptance for the request-target forms Go's server
/// hands it. Control bytes, empty targets, relative paths without a leading
/// slash, and invalid percent escapes are rejected.
fn valid_request_uri(raw: &[u8]) -> bool {
    if raw.iter().any(|&b| b < 0x20 || b == 0x7f) {
        return false;
    }
    if raw.is_empty() {
        return false;
    }
    if raw == b"*" {
        return true;
    }
    let (scheme, rest) = split_scheme(raw);
    if !scheme.is_empty() {
        if !rest.starts_with(b"/") {
            // Opaque (rootless) URL path: Go accepts it after a scheme.
            return true;
        }
        if let Some(authority) = rest.strip_prefix(b"//") {
            let host = authority.split(|&b| b == b'/').next().unwrap_or(authority);
            return valid_authority(host);
        }
        return valid_path_and_query(rest);
    }
    if !rest.starts_with(b"/") {
        return false;
    }
    valid_path_and_query(rest)
}

fn split_scheme(raw: &[u8]) -> (&[u8], &[u8]) {
    if !raw.first().is_some_and(u8::is_ascii_alphabetic) {
        return (&[], raw);
    }
    let mut end = 1;
    while end < raw.len() {
        let b = raw[end];
        if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.') {
            end += 1;
            continue;
        }
        break;
    }
    if raw.get(end) == Some(&b':') {
        (&raw[..end], &raw[end + 1..])
    } else {
        (&[], raw)
    }
}

fn valid_authority(authority: &[u8]) -> bool {
    let host = match authority.iter().rposition(|&b| b == b'@') {
        Some(at) => {
            if !valid_percent_escapes(&authority[..at]) {
                return false;
            }
            &authority[at + 1..]
        }
        None => authority,
    };
    if host.is_empty() {
        return true;
    }
    if host.starts_with(b"[") {
        let Some(close) = host.iter().position(|&b| b == b']') else {
            return false;
        };
        let literal = &host[1..close];
        if !literal.iter().all(|&b| {
            b.is_ascii_hexdigit()
                || matches!(b, b':' | b'.' | b'v' | b'%')
                || b.is_ascii_alphanumeric()
        }) {
            return false;
        }
        return match &host[close + 1..] {
            [] => true,
            [b':', port @ ..] => port.iter().all(u8::is_ascii_digit),
            _ => false,
        };
    }
    let (name, port) = match host.iter().rposition(|&b| b == b':') {
        Some(colon) => (&host[..colon], Some(&host[colon + 1..])),
        None => (host, None),
    };
    if name.is_empty() || !name.iter().all(|&b| valid_host_byte(b) || b == b'.') {
        return false;
    }
    if let Some(port) = port {
        if !port.is_empty() && !port.iter().all(u8::is_ascii_digit) {
            return false;
        }
    }
    true
}

/// Path and query: `setPath` validates percent escapes in the path only.
fn valid_path_and_query(rest: &[u8]) -> bool {
    let path = rest.split(|&b| b == b'?').next().unwrap_or(rest);
    valid_percent_escapes(path)
}

fn valid_percent_escapes(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return false;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_head(raw: &[u8]) -> Result<Option<Head>, Reject> {
        parse(raw, false)
    }

    #[test]
    fn accepts_a_plain_get() {
        let raw = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert_eq!(head.len, raw.len());
        assert_eq!(head.framing, Framing::Empty);
        assert!(!head.method_post);
    }

    #[test]
    fn accepts_content_length_and_chunked_framing() {
        let head = parse_head(b"POST /init HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(head.framing, Framing::Length(12));
        assert!(head.method_post);

        let head =
            parse_head(b"POST /init HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n")
                .unwrap()
                .unwrap();
        assert_eq!(head.framing, Framing::Chunked);
    }

    #[test]
    fn accepts_http10_without_host() {
        let head = parse_head(b"GET / HTTP/1.0\r\n\r\n").unwrap().unwrap();
        assert_eq!(head.framing, Framing::Empty);
    }

    #[test]
    fn connect_is_the_only_target_needing_no_host() {
        let head = parse_head(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(head.framing, Framing::Close);
    }

    #[test]
    fn rejects_junk_header_without_colon() {
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\nJunk Header Without Colon\r\n\r\n"),
            Err(Reject::BadRequest)
        );
    }

    #[test]
    fn rejects_nul_in_header_value() {
        assert_eq!(
            parse_head(b"GET /envs HTTP/1.1\r\nHost: x\r\nX-Access-Token: a\x00b\r\n\r\n"),
            Err(Reject::BadRequest)
        );
    }

    #[test]
    fn rejects_missing_malformed_and_duplicate_host() {
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\n\r\n"),
            Err(Reject::MissingHost)
        );
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\nHost: bad host\r\n\r\n"),
            Err(Reject::MalformedHost)
        );
        assert_eq!(
            parse_head(b"GET / HTTP/1.1\r\nHost: x\r\nHost: y\r\n\r\n"),
            Err(Reject::BadRequest)
        );
    }

    #[test]
    fn rejects_conflicting_and_bad_content_lengths() {
        assert_eq!(
            parse_head(
                b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nContent-Length: 9\r\n\r\n"
            ),
            Err(Reject::BadRequest)
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: nope\r\n\r\n"),
            Err(Reject::BadRequest)
        );
        // `fixLength` validates Content-Length before Transfer-Encoding wins.
        assert_eq!(
            parse_head(
                b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nContent-Length: nope\r\n\r\n"
            ),
            Err(Reject::BadRequest)
        );
    }

    #[test]
    fn accepts_equal_duplicate_content_lengths() {
        let head = parse_head(
            b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(head.framing, Framing::Length(3));
    }

    #[test]
    fn rejects_unsupported_transfer_encoding() {
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: gzip\r\n\r\n"),
            Err(Reject::UnsupportedTransferEncoding)
        );
        // A transfer-encoding error outranks a bad Content-Length, as in Go.
        assert_eq!(
            parse_head(
                b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: gzip\r\nContent-Length: nope\r\n\r\n"
            ),
            Err(Reject::UnsupportedTransferEncoding)
        );
    }

    #[test]
    fn ignores_transfer_encoding_on_http10() {
        let head =
            parse_head(b"POST / HTTP/1.0\r\nContent-Length: 2\r\nTransfer-Encoding: gzip\r\n\r\n")
                .unwrap()
                .unwrap();
        assert_eq!(head.framing, Framing::Length(2));
    }

    #[test]
    fn rejects_unsupported_versions() {
        assert_eq!(
            parse_head(b"GET / HTTP/0.9\r\nHost: x\r\n\r\n"),
            Err(Reject::VersionNotSupported)
        );
        assert_eq!(
            parse_head(b"GET / HTTP/2.0\r\nHost: x\r\n\r\n"),
            Err(Reject::VersionNotSupported)
        );
        assert_eq!(
            parse_head(b"GET / HTTP/3\r\nHost: x\r\n\r\n"),
            Err(Reject::BadRequest)
        );
        // The framing checks run before the version check, as in Go.
        assert_eq!(
            parse_head(b"GET / HTTP/2.0\r\nTransfer-Encoding: gzip\r\n\r\n"),
            Err(Reject::UnsupportedTransferEncoding)
        );
    }

    #[test]
    fn rejects_the_http2_preface() {
        // Go hands the preface line to the mux and then rejects `SM`; hyper
        // cannot parse it at all, so the guard answers Go's generic 400.
        assert_eq!(
            parse_head(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"),
            Err(Reject::BadRequest)
        );
    }

    #[test]
    fn validates_target_forms() {
        assert_eq!(
            parse_head(b"GET /files?path=%2Ftmp HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap()
                .unwrap()
                .framing,
            Framing::Empty
        );
        assert_eq!(
            parse_head(b"GET * HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap()
                .unwrap()
                .framing,
            Framing::Empty
        );
        assert_eq!(
            parse_head(b"GET files HTTP/1.1\r\nHost: x\r\n\r\n"),
            Err(Reject::BadRequest)
        );
        assert_eq!(
            parse_head(b"GET /a%zz HTTP/1.1\r\nHost: x\r\n\r\n"),
            Err(Reject::BadRequest)
        );
        assert_eq!(
            parse_head(b"GET /caf\xc3\xa9 HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap()
                .unwrap()
                .framing,
            Framing::Empty
        );
    }

    #[test]
    fn accepts_field_value_continuations() {
        let head =
            parse_head(b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding:\r\n chunked\r\n\r\n")
                .unwrap()
                .unwrap();
        assert_eq!(head.framing, Framing::Chunked);
    }

    #[test]
    fn incomplete_heads_ask_for_more() {
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nHost: x\r\n"), Ok(None));
        assert_eq!(parse_head(b""), Ok(None));
    }

    #[test]
    fn oversized_incomplete_heads_are_too_large() {
        let mut raw = b"GET / HTTP/1.1\r\nHost: x\r\nX-Fill: ".to_vec();
        raw.resize(MAX_HEAD_BYTES + 1, b'a');
        assert_eq!(parse_head(&raw), Err(Reject::TooLarge));
    }

    #[test]
    fn skips_leading_crlf_after_post() {
        let head = parse(b"\r\nGET / HTTP/1.1\r\nHost: x\r\n\r\n", true)
            .unwrap()
            .unwrap();
        assert_eq!(head.len, 29);
    }

    #[test]
    fn eof_reply_depends_on_the_request_line() {
        // `bufio.ReadLine` hands any non-empty partial line to the parser as
        // a complete line, so a truncated request line still earns Go's 400.
        assert!(eof_gets_reply(b"GE", false));
        assert!(eof_gets_reply(b"G", false));
        assert!(eof_gets_reply(b"\r", false));
        assert!(!eof_gets_reply(b"", false));
        assert!(!eof_gets_reply(b"\r\n", true));
        assert!(eof_gets_reply(b"\r\n\r\n\r", true));
        assert!(eof_gets_reply(b"GET / HTTP/1.1\r\nHost: x", false));
        assert!(eof_gets_reply(b"GET / HTTP/1.1\r\n", false));
    }

    #[test]
    fn go_error_bytes_are_pinned() {
        assert!(Reject::BadRequest
            .response()
            .ends_with(b"\r\n\r\n400 Bad Request"));
        assert!(Reject::MissingHost
            .response()
            .ends_with(b"\r\n\r\n400 Bad Request: missing required Host header"));
        assert!(Reject::TooLarge
            .response()
            .starts_with(b"HTTP/1.1 431 Request Header Fields Too Large\r\n"));
        assert!(Reject::UnsupportedTransferEncoding
            .response()
            .ends_with(b"\r\n\r\nUnsupported transfer encoding"));
    }
}
