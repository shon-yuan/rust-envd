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
//! Go also accepts a superset of what hyper's parser does: the HTTP/2
//! preface line, `HTTP/1.2`–`HTTP/1.9` version tokens, absolute-form targets
//! with an empty authority, and target bytes http's `Uri` rejects. [`parse`]
//! therefore returns a [`Head`] carrying the rewrites that make the validated
//! head parseable by hyper without changing the request Go's handler sees;
//! [`Head::rewrite`] applies them to the raw bytes before delivery.

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
    /// From `(*conn).serve`: the shared `errorHeaders` block. `statusError`
    /// values (bad request texts and 505) repeat their text in the status
    /// line after a colon (`"HTTP/1.1 %d %s: %s"`, server.go:2049); the
    /// generic 400, 431 and 501 status lines carry no reason. None of these
    /// carry a Date header.
    pub(crate) fn response(self) -> &'static [u8] {
        match self {
            Self::BadRequest => b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request",
            Self::MissingHost => b"HTTP/1.1 400 Bad Request: missing required Host header\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: missing required Host header",
            Self::MalformedHost => b"HTTP/1.1 400 Bad Request: malformed Host header\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: malformed Host header",
            Self::InvalidHeaderName => b"HTTP/1.1 400 Bad Request: invalid header name\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: invalid header name",
            Self::InvalidHeaderValue => b"HTTP/1.1 400 Bad Request: invalid header value\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n400 Bad Request: invalid header value",
            Self::TooLarge => b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n431 Request Header Fields Too Large",
            Self::UnsupportedTransferEncoding => b"HTTP/1.1 501 Not Implemented\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\nUnsupported transfer encoding",
            Self::VersionNotSupported => b"HTTP/1.1 505 HTTP Version Not Supported: unsupported protocol version\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n505 HTTP Version Not Supported: unsupported protocol version",
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

/// A request-target that hyper's parser would reject but Go accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetRewrite {
    /// Deliver the target unchanged.
    None,
    /// Absolute-form with an empty authority (`http://`): http's `Uri`
    /// requires an authority for absolute forms, while Go records an empty
    /// path. Deliver Go's path (`/`) so the request reaches the handler.
    Slash,
    /// Percent-encode the bytes http's `Uri` rejects in paths and queries but
    /// Go's `url.ParseRequestURI` keeps (`<`, `>`, backtick, `#`, a query
    /// `"`, and non-UTF-8 bytes).
    Sanitize,
}

/// An accepted request head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Head {
    /// Length of the head in bytes, including any skipped leading CR/LF.
    pub(crate) len: usize,
    /// Whether the method is POST (Go tolerates leading CR/LF only after it).
    pub(crate) method_post: bool,
    /// Body framing that follows.
    pub(crate) framing: Framing,
    /// The request-target span within those bytes.
    uri: std::ops::Range<usize>,
    /// The version-token span within those bytes.
    version: std::ops::Range<usize>,
    /// Deliver the version token as `HTTP/1.1`: Go serves any `HTTP/1.x`
    /// and the HTTP/2 preface, but httparse only accepts `HTTP/1.0` and
    /// `HTTP/1.1`.
    rewrite_version: bool,
    /// The request-target rewrite hyper needs.
    target: TargetRewrite,
    /// Inject `Connection: close` so hyper closes after the response, as Go
    /// does for the HTTP/2 preface (`w.closeAfterReply`).
    inject_close: bool,
    /// Stop parsing after this request's body: Go closes the connection after
    /// a `Connection: close`, an HTTP/1.0 request without keep-alive, a
    /// failed `Expect`, or the HTTP/2 preface.
    pub(crate) halt_after: bool,
}

impl Head {
    /// Whether [`Head::rewrite`] would change the head bytes.
    pub(crate) fn needs_rewrite(&self) -> bool {
        self.rewrite_version || self.target != TargetRewrite::None || self.inject_close
    }

    /// The head bytes to deliver to hyper.
    ///
    /// `raw` must be this head's bytes (`len` bytes). Every rewrite keeps the
    /// request Go's handler observes intact: the version token only affects
    /// parsing (Go answers any `HTTP/1.x` with an `HTTP/1.1` status line), and
    /// the target edits only touch bytes http's `Uri` cannot carry.
    pub(crate) fn rewrite(&self, raw: &[u8]) -> Vec<u8> {
        let mut out = raw.to_vec();
        if self.rewrite_version {
            out[self.version.clone()].copy_from_slice(b"HTTP/1.1");
        }
        let replacement = match self.target {
            TargetRewrite::None => None,
            TargetRewrite::Slash => Some(vec![b'/']),
            TargetRewrite::Sanitize => Some(sanitize_target(&raw[self.uri.clone()])),
        };
        if let Some(replacement) = replacement {
            out.splice(self.uri.clone(), replacement).for_each(drop);
        }
        if self.inject_close {
            // The head always ends with the blank line's CRLF; the injected
            // field line belongs before it.
            let at = out.len() - 2;
            out.splice(at..at, b"Connection: close\r\n".iter().copied())
                .for_each(drop);
        }
        out
    }
}

/// Percent-encode the request-target bytes http's `Uri` parser rejects.
///
/// `scan_path_and_query` (http 1.5.0) marks `<`, `>`, backtick and `#` as
/// invalid in the path, and `<`, `>`, `"` and `#` as invalid in the query;
/// bytes of 0x80 and above are accepted only when the whole target is valid
/// UTF-8.
pub(crate) fn sanitize_target(target: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let invalid_utf8 = std::str::from_utf8(target).is_err();
    let query_at = target.iter().position(|&b| b == b'?');
    let mut out = Vec::with_capacity(target.len());
    for (index, &byte) in target.iter().enumerate() {
        let in_query = query_at.is_some_and(|query| index > query);
        let encode = (invalid_utf8 && byte >= 0x80)
            || matches!(byte, b'<' | b'>' | b'#')
            || (!in_query && byte == b'`')
            || (in_query && byte == b'"');
        if encode {
            out.push(b'%');
            out.push(HEX[(byte >> 4) as usize]);
            out.push(HEX[(byte & 0x0f) as usize]);
        } else {
            out.push(byte);
        }
    }
    out
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
    // `(*conn).readRequest` sets the connection read limit to
    // `initialReadLimitSize` before parsing (`server.go:1048`): the limited
    // reader hands the parser at most that many bytes and then reports EOF.
    // A head that completes inside the budget is parsed normally; anything
    // longer is `errTooLarge` even when the blank line arrives later
    // (`server.go:1055-1058`).
    if buf.len() > MAX_HEAD_BYTES {
        return match parse_inner(&buf[..MAX_HEAD_BYTES], skip_leading_crlf) {
            Ok(Some(head)) => Ok(Some(head)),
            Err(reason) => Err(reason),
            Ok(None) => Err(Reject::TooLarge),
        };
    }
    parse_inner(buf, skip_leading_crlf)
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
    // `http1ServerSupportsRequest` lets the HTTP/2 connection preface through
    // to the handler; every other major version is a 505.
    let is_preface = method == b"PRI" && uri == b"*" && (major, minor) == (2, 0);
    // Offsets of the two tokens the delivery rewrite may touch.
    let uri_start = start + method.len() + 1;
    let uri_range = uri_start..uri_start + uri.len();
    let version_start = start + line.len() - proto.len();
    let version_range = version_start..version_start + proto.len();

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
    // `http1ServerSupportsRequest`: HTTP/1.x and the Preface reach a handler.
    if major != 1 && !is_preface {
        return Err(Reject::VersionNotSupported);
    }
    // `Request.isH2Upgrade`: the bare preface (no header lines at all, so the
    // Host line a real client sends disqualifies it, as in Go).
    let is_h2_upgrade = is_preface && headers.is_empty();
    if proto_at_least_11 && host.is_empty() && !is_connect && !is_h2_upgrade {
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

    // `Request.wantsClose` (`shouldClose`) checks every Connection value;
    // `expectsContinue` reads the first Expect value only (`Header.get`).
    let close = headers_with(&headers, b"connection").any(|header| {
        header
            .value
            .split(|&b| b == b',')
            .any(|token| eq_ignore_ascii_case(trim(token), b"close"))
    });
    let keep_alive_10 = headers_with(&headers, b"connection").any(|header| {
        header
            .value
            .split(|&b| b == b',')
            .any(|token| eq_ignore_ascii_case(trim(token), b"keep-alive"))
    });
    let expect = headers_with(&headers, b"expect")
        .next()
        .map(|header| header.value.as_slice());
    let expect_continue = expect.is_some_and(|value| {
        value
            .split(|&b| b == b',')
            .any(|token| eq_ignore_ascii_case(trim(token), b"100-continue"))
    });
    // `(*conn).serve`: any non-empty Expect without the 100-continue token is
    // answered 417 before a handler runs.
    let failed_expect = expect.is_some_and(|value| !value.is_empty() && !expect_continue);
    let halt_after =
        is_h2_upgrade || failed_expect || close || ((major, minor) == (1, 0) && !keep_alive_10);

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
        uri: uri_range,
        version: version_range,
        // Go's status line is HTTP/1.1 for any version at least 1.1, and
        // httparse accepts only the literal 1.0/1.1 tokens.
        rewrite_version: is_preface || (major == 1 && minor > 1),
        target: target_rewrite(uri),
        inject_close: is_h2_upgrade,
        halt_after,
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

/// The target rewrite the delivery step must apply before hyper parses it.
fn target_rewrite(uri: &[u8]) -> TargetRewrite {
    if uri == b"*" {
        return TargetRewrite::None;
    }
    if empty_authority(uri) {
        return TargetRewrite::Slash;
    }
    if needs_sanitize(uri) {
        return TargetRewrite::Sanitize;
    }
    TargetRewrite::None
}

/// Whether `uri` is an absolute-form target whose authority is empty
/// (`http://`, optionally followed by a query). Go accepts it; http's `Uri`
/// rejects absolute forms without an authority.
fn empty_authority(uri: &[u8]) -> bool {
    let (scheme, rest) = split_scheme(uri);
    if scheme.is_empty() {
        return false;
    }
    let rest = match rest.iter().position(|&b| b == b'?') {
        Some(query) => &rest[..query],
        None => rest,
    };
    match rest.strip_prefix(b"//") {
        Some(authority_and_path) => {
            let end = authority_and_path
                .iter()
                .position(|&b| b == b'/')
                .unwrap_or(authority_and_path.len());
            end == 0
        }
        None => false,
    }
}

/// Whether [`sanitize_target`] would change any byte.
fn needs_sanitize(uri: &[u8]) -> bool {
    sanitize_target(uri) != uri
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
/// slash, and invalid percent escapes are rejected. As in Go's `url.parse`,
/// the query is cut before the authority and path are examined.
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
    let (scheme, after_scheme) = split_scheme(raw);
    let rest = match after_scheme.iter().position(|&b| b == b'?') {
        Some(query) => &after_scheme[..query],
        None => after_scheme,
    };
    if !scheme.is_empty() {
        if !rest.starts_with(b"/") {
            // Opaque (rootless) URL path: Go accepts it after a scheme.
            return true;
        }
        if let Some(authority_and_path) = rest.strip_prefix(b"//") {
            let end = authority_and_path
                .iter()
                .position(|&b| b == b'/')
                .unwrap_or(authority_and_path.len());
            let (authority, path) = authority_and_path.split_at(end);
            return valid_authority(authority) && valid_percent_escapes(path);
        }
        return valid_percent_escapes(rest);
    }
    if !rest.starts_with(b"/") {
        return false;
    }
    valid_percent_escapes(rest)
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
    fn accepts_the_http2_preface_with_go_decisions() {
        // The bare preface (no header lines) is Go's isH2Upgrade: the Host
        // requirement is waived, the response closes the connection, and no
        // further request is parsed.
        let raw = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert_eq!(head.len, 18);
        assert!(head.halt_after);
        assert!(head.needs_rewrite());
        assert_eq!(
            head.rewrite(&raw[..head.len]),
            b"PRI * HTTP/1.1\r\nConnection: close\r\n\r\n"
        );
        // The same request line with headers is an ordinary request (Go's
        // isH2Upgrade needs an empty header map); it is delivered as HTTP/1.1
        // and the connection stays open.
        let raw = b"PRI * HTTP/2.0\r\nHost: x\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert!(!head.halt_after);
        assert_eq!(head.rewrite(raw), b"PRI * HTTP/1.1\r\nHost: x\r\n\r\n");
    }

    #[test]
    fn rewrites_versions_httparse_cannot_parse() {
        let raw = b"GET /health HTTP/1.2\r\nHost: x\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert!(head.needs_rewrite());
        assert_eq!(
            head.rewrite(raw),
            b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n"
        );
        // HTTP/1.0 and HTTP/1.1 are delivered unchanged.
        let raw = b"GET /health HTTP/1.0\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert!(!head.needs_rewrite());
        assert_eq!(head.rewrite(raw), raw.as_slice());
    }

    #[test]
    fn rewrites_absolute_targets_with_an_empty_authority() {
        let raw = b"GET http:// HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert_eq!(head.rewrite(raw), b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        let raw = b"GET http://?q HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert_eq!(head.rewrite(raw), b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        // A non-empty authority is delivered unchanged.
        let raw = b"GET http://x/health HTTP/1.1\r\nHost: x\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert!(!head.needs_rewrite());
    }

    #[test]
    fn sanitizes_targets_http_uri_rejects() {
        for (raw, expected) in [
            (
                b"GET /a<b>c HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
                b"GET /a%3Cb%3Ec HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
            ),
            (
                b"GET /health#x HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
                b"GET /health%23x HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
            ),
            (
                b"GET /q?a=\"b HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
                b"GET /q?a=%22b HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
            ),
            (
                b"GET /health\xff HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
                b"GET /health%FF HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
            ),
        ] {
            let head = parse_head(raw).unwrap().unwrap();
            assert_eq!(head.rewrite(raw), expected, "{:?}", raw);
        }
        // Bytes http accepts are delivered unchanged: a path quote, a query
        // backtick and valid UTF-8.
        for raw in [
            b"GET /a\"b HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
            b"GET /q?a=`b HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
            b"GET /caf\xc3\xa9 HTTP/1.1\r\nHost: x\r\n\r\n".as_slice(),
        ] {
            let head = parse_head(raw).unwrap().unwrap();
            assert!(!head.needs_rewrite(), "{:?}", raw);
        }
    }

    #[test]
    fn absolute_form_paths_must_keep_valid_percent_escapes() {
        assert_eq!(
            parse_head(b"GET http://x/%zz HTTP/1.1\r\nHost: x\r\n\r\n"),
            Err(Reject::BadRequest)
        );
        assert!(parse_head(b"GET http://?q HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap()
            .is_some());
    }

    #[test]
    fn halt_after_matches_go_connection_reuse() {
        let halted = |raw: &[u8]| parse_head(raw).unwrap().unwrap().halt_after;
        assert!(halted(
            b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        ));
        assert!(halted(b"GET /health HTTP/1.0\r\n\r\n"));
        assert!(!halted(
            b"GET /health HTTP/1.0\r\nConnection: keep-alive\r\n\r\n"
        ));
        assert!(halted(
            b"GET /health HTTP/1.1\r\nHost: x\r\nExpect: 201-created\r\n\r\n"
        ));
        // Go's `hasToken` splits on commas: the 100-continue token may follow
        // others in the same value.
        assert!(!halted(
            b"GET /health HTTP/1.1\r\nHost: x\r\nExpect: gzip, 100-continue\r\nContent-Length: 0\r\n\r\n"
        ));
        // An empty Expect value is not a failed expectation.
        assert!(!halted(
            b"GET /health HTTP/1.1\r\nHost: x\r\nExpect:\r\n\r\n"
        ));
        // An h2c upgrade attempt is an ordinary request for Go 1.26.
        assert!(!halted(
            b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: Upgrade, HTTP2-Settings\r\nUpgrade: h2c\r\n\r\n"
        ));
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
    fn complete_oversized_heads_are_too_large() {
        // Go's limited reader gives up at `initialReadLimitSize` bytes even
        // when the blank line is still to come (`server.go:1048-1058`), so a
        // well-formed head past the limit is errTooLarge, not a 400.
        let mut raw = b"GET / HTTP/1.1\r\nHost: x\r\nX-Fill: ".to_vec();
        raw.resize(MAX_HEAD_BYTES + 1, b'a');
        raw.extend_from_slice(b"\r\n\r\n");
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
            .starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
        // `statusError` repeats its text in the status line (`server.go:2049`).
        assert!(Reject::MissingHost
            .response()
            .starts_with(b"HTTP/1.1 400 Bad Request: missing required Host header\r\n"));
        assert!(Reject::MissingHost
            .response()
            .ends_with(b"\r\n\r\n400 Bad Request: missing required Host header"));
        assert!(Reject::MalformedHost
            .response()
            .starts_with(b"HTTP/1.1 400 Bad Request: malformed Host header\r\n"));
        assert!(Reject::InvalidHeaderName
            .response()
            .starts_with(b"HTTP/1.1 400 Bad Request: invalid header name\r\n"));
        assert!(Reject::InvalidHeaderValue
            .response()
            .starts_with(b"HTTP/1.1 400 Bad Request: invalid header value\r\n"));
        assert!(Reject::VersionNotSupported.response().starts_with(
            b"HTTP/1.1 505 HTTP Version Not Supported: unsupported protocol version\r\n"
        ));
        assert!(Reject::TooLarge
            .response()
            .starts_with(b"HTTP/1.1 431 Request Header Fields Too Large\r\n"));
        // 431 and 501 are not statusError values: no reason in the status line.
        assert!(Reject::UnsupportedTransferEncoding
            .response()
            .starts_with(b"HTTP/1.1 501 Not Implemented\r\n"));
        assert!(Reject::UnsupportedTransferEncoding
            .response()
            .ends_with(b"\r\n\r\nUnsupported transfer encoding"));
    }
}
