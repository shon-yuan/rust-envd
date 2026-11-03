//! GET /files for runtime 51083a83 (envd 0.10.2), including ServeContent semantics.
// HTTP file adapters return axum responses as first-class error payloads;
// boxing every shared-helper boundary only re-wraps the same allocation.
#![allow(clippy::result_large_err)]
use crate::{
    auth::{AuthError, FileAccess, ReadWrite},
    exec_context::{Account, Defaults},
    http::ConfigurationState,
};
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::stream;
use http_range::{HttpRange, HttpRangeParseError};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, BufReader},
};
use tokio_util::io::ReaderStream;

pub struct Parameters {
    pub path: String,
    pub path_present: bool,
    pub username: Option<String>,
    pub signature: Option<String>,
    pub expiration: Option<i64>,
}
impl Parameters {
    pub fn parse(query: &str) -> Result<Self, Response> {
        // Generated Go scalar binding rejects repeated known parameters.
        let mut values: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            let entry = values.entry(key.clone().into_owned()).or_default();
            if entry.replace(value.into_owned()).is_some() {
                return Err(plain_error(
                    400,
                    &format!("parameter '{key}' is specified multiple times"),
                ));
            }
        }
        let mut taken = |key: &str| values.remove(key).flatten();
        let path = taken("path");
        let path_present = path.is_some();
        let expiration = taken("signature_expiration")
            .map(|value| {
                value.parse::<i64>().map_err(|_| {
                    plain_error(400, "Invalid format for parameter signature_expiration: invalid signature_expiration")
                })
            })
            .transpose()?;
        Ok(Self {
            path: path.unwrap_or_default(),
            path_present,
            username: taken("username"),
            signature: taken("signature"),
            expiration,
        })
    }
}

pub async fn get_files<S: ConfigurationState>(
    State(state): State<Arc<S>>,
    request: Request<Body>,
) -> Response {
    match download(state.as_ref(), request).await {
        Ok(response) | Err(response) => response,
    }
}

async fn download<S: ConfigurationState>(
    state: &S,
    request: Request<Body>,
) -> Result<Response, Response> {
    let params = Parameters::parse(request.uri().query().unwrap_or(""))?;
    let defaults = authorize(state, &params, request.headers(), ReadWrite::Read).await?;
    let supplied = params.username.clone();
    let requested = params.path.clone();
    let resolved =
        tokio::task::spawn_blocking(move || resolve(&defaults, supplied.as_deref(), &requested))
            .await
            .map_err(|_| json_error(500, "account lookup failed"))??;
    let metadata = tokio::fs::metadata(&resolved).await.map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            json_error(404, &format!("path '{resolved}' does not exist"))
        } else {
            // Go wraps os.Stat's *fs.PathError ("stat <path>: <errno text>").
            json_error(
                500,
                &format!(
                    "error checking if path exists '{resolved}': {}",
                    stat_text(&resolved, &error)
                ),
            )
        }
    })?;
    if metadata.is_dir() {
        return Err(json_error(
            400,
            &format!("path '{resolved}' is a directory"),
        ));
    }
    let (mut encoding, identity_ok) =
        encoding(request.headers()).map_err(|message| json_error(406, &message))?;
    let conditional = ["range", "if-modified-since", "if-none-match", "if-range"]
        .iter()
        .any(|name| !header_text(request.headers(), name).is_empty());
    if conditional {
        if !identity_ok {
            let mut response = json_error(
                406,
                "identity encoding not acceptable for Range or conditional request",
            );
            response
                .headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
            return Err(response);
        }
        encoding = Encoding::Identity;
    }
    let mut file = File::open(&resolved)
        .await
        .map_err(|error| json_error(500, &format!("error opening file '{resolved}': {error}")))?;
    let mut headers = HeaderMap::new();
    headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    set(
        &mut headers,
        header::CONTENT_DISPOSITION,
        &disposition(
            Path::new(&resolved)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref(),
        ),
    )?;
    if matches!(encoding, Encoding::Gzip) {
        set(
            &mut headers,
            header::CONTENT_TYPE,
            &extension_type(&params.path).unwrap_or_else(|| "application/octet-stream".into()),
        )?;
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        let gzip = async_compression::tokio::bufread::GzipEncoder::new(BufReader::new(file));
        return Ok(response(
            200,
            headers,
            Body::from_stream(ReaderStream::new(gzip)),
        ));
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(seconds)
        .filter(|time| *time != 0);
    if let Some(modified) = modified {
        set(
            &mut headers,
            header::LAST_MODIFIED,
            &httpdate::fmt_http_date(UNIX_EPOCH + std::time::Duration::from_secs(modified)),
        )?;
    }
    if let Some(status) = precondition(request.headers(), modified) {
        return Ok(response(status, headers, Body::empty()));
    }
    let mime = if let Some(mime) = extension_type(&params.path) {
        mime
    } else {
        let mut sample = Vec::new();
        (&mut file)
            .take(512)
            .read_to_end(&mut sample)
            .await
            .map_err(|error| json_error(500, &error.to_string()))?;
        file.seek(io::SeekFrom::Start(0))
            .await
            .map_err(|error| json_error(500, &error.to_string()))?;
        sniff(&sample).to_owned()
    };
    set(&mut headers, header::CONTENT_TYPE, &mime)?;
    let size = metadata.len();
    let range_header = header_text(request.headers(), "range");
    let range_header = if if_range(request.headers(), modified) {
        range_header
    } else {
        ""
    };
    let ranges = match ranges(range_header, size) {
        Ok(ranges) => ranges,
        Err(HttpRangeParseError::NoOverlap) if size == 0 => Vec::new(),
        Err(error) => {
            headers.remove(header::LAST_MODIFIED);
            let message = if matches!(error, HttpRangeParseError::NoOverlap) {
                set(
                    &mut headers,
                    header::CONTENT_RANGE,
                    &format!("bytes */{size}"),
                )?;
                "invalid range: failed to overlap"
            } else {
                "invalid range"
            };
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            return Ok(response(416, headers, Body::from(format!("{message}\n"))));
        }
    };
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    let (status, segments, length) = segments(&ranges, &mime, size, &mut headers)?;
    set(&mut headers, header::CONTENT_LENGTH, &length.to_string())?;
    Ok(response(status, headers, stream_file(file, segments)))
}

pub async fn authorize<S: ConfigurationState>(
    state: &S,
    params: &Parameters,
    headers: &HeaderMap,
    operation: ReadWrite,
) -> Result<Defaults, Response> {
    let configuration = state.configuration().lock().await;
    let token = header_text(headers, "x-access-token");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let access = FileAccess {
        path: &params.path,
        operation,
        username: params.username.as_deref(),
        header_token: Some(token),
        signature: params.signature.as_deref(),
        expiration: params.expiration,
        now,
    };
    configuration
        .access
        .authorize_file(&access)
        .map_err(|error| {
            let message = if error == AuthError::Unauthorized && !token.is_empty() {
                "access token present in header but does not match".to_owned()
            } else {
                error.to_string()
            };
            json_error(401, &message)
        })?;
    Ok(configuration.defaults.clone())
}

pub fn resolve(
    defaults: &Defaults,
    supplied: Option<&str>,
    path: &str,
) -> Result<String, Response> {
    let username = defaults
        .resolve_username(supplied)
        .map_err(|error| json_error(400, &error.to_string()))?;
    let account = lookup_account(username)?;
    defaults.resolve_path(path, &account).map_err(|error| {
        json_error(
            400,
            &format!("error expanding and resolving path '{path}': {error}"),
        )
    })
}

/// OS lookup is blocking; call from spawn_blocking at either file adapter.
pub fn lookup_account(username: &str) -> Result<Account, Response> {
    crate::platform::lookup_account(username)
        .map_err(|error| json_error(401, &format!("error looking up user '{username}': {error}")))
}

/// Go's `os.Stat` failure text. The oracle runs on Linux; Windows keeps the
/// same prefix and lets `std::io::Error` render its own message, since no
/// Windows oracle exists to match.
#[cfg(target_os = "linux")]
fn stat_text(path: &str, error: &io::Error) -> String {
    crate::errno_text::stat_text(path, error)
}
#[cfg(windows)]
fn stat_text(path: &str, error: &io::Error) -> String {
    format!("stat {path}: {error}")
}
fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
fn set(headers: &mut HeaderMap, name: header::HeaderName, value: &str) -> Result<(), Response> {
    headers.insert(
        name,
        HeaderValue::from_str(value).map_err(|_| json_error(500, "invalid response header"))?,
    );
    Ok(())
}
fn response(status: u16, headers: HeaderMap, body: Body) -> Response {
    let mut response = body.into_response();
    *response.status_mut() =
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    *response.headers_mut() = headers;
    response
}
/// Mirrors Go http.Error as used by the generated query binder fallback.
pub fn plain_error(status: u16, message: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response(status, headers, Body::from(format!("{message}\n")))
}

pub fn json_error(status: u16, message: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response(
        status,
        headers,
        Body::from(format!(
            "{}\n",
            serde_json::json!({"code":status,"message":message})
        )),
    )
}

#[derive(Clone, Copy)]
enum Encoding {
    Identity,
    Gzip,
}
fn encoding(headers: &HeaderMap) -> Result<(Encoding, bool), String> {
    let raw = header_text(headers, "accept-encoding");
    let mut values: Vec<_> = raw
        .split(',')
        .map(|value| {
            let mut parts = value.trim().split(';');
            let coding = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            let mut quality = 1.0_f64;
            for parameter in parts {
                if let Some((key, value)) = parameter.trim().split_once('=') {
                    if key.eq_ignore_ascii_case("q") {
                        if let Ok(parsed) = value.parse::<f64>() {
                            quality = parsed;
                        }
                    }
                }
            }
            (coding, quality)
        })
        .collect();
    let rejected = values
        .iter()
        .any(|(coding, quality)| coding == "identity" && *quality == 0.0)
        || (values
            .iter()
            .any(|(coding, quality)| coding == "*" && *quality == 0.0)
            && !values
                .iter()
                .any(|(coding, quality)| coding == "identity" && *quality != 0.0));
    // Go cmp.Compare orders NaN before every other value, including -Inf.
    values.sort_by(|a, b| match (a.1.is_nan(), b.1.is_nan()) {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal),
    });
    for (coding, quality) in values {
        if quality == 0.0 {
            continue;
        }
        match coding.as_str() {
            "identity" => return Ok((Encoding::Identity, !rejected)),
            "gzip" => return Ok((Encoding::Gzip, !rejected)),
            "*" => {
                return Ok((
                    if rejected {
                        Encoding::Gzip
                    } else {
                        Encoding::Identity
                    },
                    !rejected,
                ))
            }
            _ => {}
        }
    }
    if rejected {
        Err("error parsing Accept-Encoding: no acceptable encoding found, supported: [gzip]".into())
    } else {
        Ok((Encoding::Identity, true))
    }
}
fn seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|time| time.as_secs())
}
fn date(headers: &HeaderMap, name: &str) -> Option<u64> {
    httpdate::parse_http_date(header_text(headers, name))
        .ok()
        .and_then(seconds)
}
fn precondition(headers: &HeaderMap, modified: Option<u64>) -> Option<u16> {
    let matches = header_text(headers, "if-match");
    if !matches.is_empty() {
        if !wildcard(matches) {
            return Some(412);
        }
    } else if let (Some(modified), Some(before)) = (modified, date(headers, "if-unmodified-since"))
    {
        if modified > before {
            return Some(412);
        }
    }
    let none = header_text(headers, "if-none-match");
    if !none.is_empty() {
        if wildcard(none) {
            return Some(304);
        }
    } else if let (Some(modified), Some(after)) = (modified, date(headers, "if-modified-since")) {
        if modified <= after {
            return Some(304);
        }
    }
    None
}
// No ETag is emitted by the pinned Go handler. Only a syntactically reached
// wildcard can match; commas inside quoted opaque tags must not be split.
fn wildcard(mut value: &str) -> bool {
    loop {
        value = value.trim_start_matches([' ', '\t', '\r', '\n']);
        if let Some(rest) = value.strip_prefix(',') {
            value = rest;
            continue;
        }
        if value.starts_with('*') {
            return true;
        }
        let opaque = value.strip_prefix("W/").unwrap_or(value);
        let Some(opaque) = opaque.strip_prefix('"') else {
            return false;
        };
        let Some(end) = opaque
            .bytes()
            .position(|byte| byte == b'"' || byte < 0x21 || byte == 0x7f)
        else {
            return false;
        };
        if opaque.as_bytes()[end] != b'"' {
            return false;
        }
        value = &opaque[end + 1..];
    }
}
fn if_range(headers: &HeaderMap, modified: Option<u64>) -> bool {
    let value = header_text(headers, "if-range");
    value.is_empty() || modified.is_some_and(|modified| date(headers, "if-range") == Some(modified))
}
fn ranges(value: &str, size: u64) -> Result<Vec<HttpRange>, HttpRangeParseError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries = value
        .strip_prefix("bytes=")
        .ok_or(HttpRangeParseError::InvalidRange)?;
    // Match Go 1.26 net/http.parseRange's default cap and parsing order. In
    // particular an out-of-bounds start skips the end parser, even if invalid.
    if entries.bytes().filter(|byte| *byte == b',').count() >= 200 {
        return Ok(Vec::new());
    }
    let number = |value: &str| -> Result<u64, HttpRangeParseError> {
        value
            .parse::<i64>()
            .ok()
            .filter(|value| *value >= 0)
            .map(|value| value as u64)
            .ok_or(HttpRangeParseError::InvalidRange)
    };
    let mut output = Vec::new();
    let mut no_overlap = false;
    for entry in entries
        .split(',')
        .map(|entry| entry.trim_matches([' ', '\t']))
    {
        if entry.is_empty() {
            continue;
        }
        let (start, end) = entry
            .split_once('-')
            .ok_or(HttpRangeParseError::InvalidRange)?;
        let start = start.trim_matches([' ', '\t']);
        let end = end.trim_matches([' ', '\t']);
        let range = if start.is_empty() {
            if end.is_empty() || end.starts_with('-') {
                return Err(HttpRangeParseError::InvalidRange);
            }
            let length = number(end)?.min(size);
            HttpRange {
                start: size - length,
                length,
            }
        } else {
            let start = number(start)?;
            if start >= size {
                no_overlap = true;
                continue;
            }
            let end = if end.is_empty() {
                size - 1
            } else {
                number(end)?.min(size - 1)
            };
            if end < start {
                return Err(HttpRangeParseError::InvalidRange);
            }
            HttpRange {
                start,
                length: end - start + 1,
            }
        };
        output.push(range);
    }
    if no_overlap && output.is_empty() {
        return Err(HttpRangeParseError::NoOverlap);
    }
    if output
        .iter()
        .try_fold(0_u64, |sum, range| sum.checked_add(range.length))
        .is_none_or(|sum| sum > size)
    {
        Ok(Vec::new())
    } else {
        Ok(output)
    }
}

fn content_range(range: HttpRange, size: u64) -> String {
    // Go int64 arithmetic: a zero-length slice prints start..start-1 verbatim
    // (negative on an empty file), e.g. "bytes 0--1/0".
    let end = range.start as i128 + range.length as i128 - 1;
    format!("bytes {}-{end}/{size}", range.start)
}

enum Segment {
    Bytes(Vec<u8>),
    File { offset: u64, remaining: u64 },
}
fn segments(
    ranges: &[HttpRange],
    mime: &str,
    size: u64,
    headers: &mut HeaderMap,
) -> Result<(u16, VecDeque<Segment>, u64), Response> {
    if ranges.is_empty() {
        return Ok((
            200,
            VecDeque::from([Segment::File {
                offset: 0,
                remaining: size,
            }]),
            size,
        ));
    }
    if let [range] = ranges {
        set(headers, header::CONTENT_RANGE, &content_range(*range, size))?;
        return Ok((
            206,
            VecDeque::from([Segment::File {
                offset: range.start,
                remaining: range.length,
            }]),
            range.length,
        ));
    }
    // Boundary values have no semantic identity. Use a fresh counter plus time
    // digest, keeping exactly Go multipart.Writer's 60 hexadecimal characters.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    use sha2::{Digest, Sha256};
    let boundary = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{:?}-{}",
                SystemTime::now(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
            .as_bytes()
        )
    );
    let boundary = &boundary[..60];
    set(
        headers,
        header::CONTENT_TYPE,
        &format!("multipart/byteranges; boundary={boundary}"),
    )?;
    let mut parts = VecDeque::new();
    let mut length = 0;
    for (index, range) in ranges.iter().enumerate() {
        let prefix = if index == 0 { "" } else { "\r\n" };
        let header = format!(
            "{prefix}--{boundary}\r\nContent-Range: {}\r\nContent-Type: {mime}\r\n\r\n",
            content_range(*range, size)
        )
        .into_bytes();
        length += header.len() as u64 + range.length;
        parts.push_back(Segment::Bytes(header));
        parts.push_back(Segment::File {
            offset: range.start,
            remaining: range.length,
        });
    }
    let closing = format!("\r\n--{boundary}--\r\n").into_bytes();
    length += closing.len() as u64;
    parts.push_back(Segment::Bytes(closing));
    Ok((206, parts, length))
}
fn stream_file(file: File, segments: VecDeque<Segment>) -> Body {
    Body::from_stream(stream::try_unfold(
        (file, segments),
        |(mut file, mut segments)| async move {
            while let Some(segment) = segments.pop_front() {
                match segment {
                    Segment::Bytes(bytes) => {
                        return Ok::<_, io::Error>(Some((bytes, (file, segments))))
                    }
                    Segment::File {
                        offset,
                        remaining: 0,
                    } => {
                        let _ = offset;
                    }
                    Segment::File { offset, remaining } => {
                        file.seek(io::SeekFrom::Start(offset)).await?;
                        let mut chunk = vec![0; remaining.min(32 * 1024) as usize];
                        let count = file.read(&mut chunk).await?;
                        if count == 0 {
                            return Err(io::ErrorKind::UnexpectedEof.into());
                        }
                        chunk.truncate(count);
                        segments.push_front(Segment::File {
                            offset: offset + count as u64,
                            remaining: remaining - count as u64,
                        });
                        return Ok(Some((chunk, (file, segments))));
                    }
                }
            }
            Ok(None)
        },
    ))
}
fn disposition(filename: &str) -> String {
    if filename
        .bytes()
        .any(|byte| !byte.is_ascii() || byte < 0x20 || byte == 0x7f)
    {
        let encoded: String = filename
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
                    (byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect();
        format!("inline; filename*=utf-8''{encoded}")
    } else if filename
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        && !filename.is_empty()
    {
        format!("inline; filename={filename}")
    } else {
        format!(
            "inline; filename=\"{}\"",
            filename.replace('\\', "\\\\").replace('"', "\\\"")
        )
    }
}
fn extension_type(path: &str) -> Option<String> {
    let mime = mime_guess::from_path(path).first_raw()?;
    let mime = match mime {
        "application/javascript" => "text/javascript",
        "application/xml" => "text/xml",
        mime => mime,
    };
    Some(if mime.starts_with("text/") {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_owned()
    })
}

// WHATWG sniff signatures used by Go net/http.DetectContentType. The bounded
// sample is 512 bytes; text is determined by control bytes rather than UTF-8.
fn sniff(data: &[u8]) -> &'static str {
    let trimmed = data.trim_ascii_start();
    for tag in [
        "<!DOCTYPE HTML",
        "<HTML",
        "<HEAD",
        "<SCRIPT",
        "<IFRAME",
        "<H1",
        "<DIV",
        "<FONT",
        "<TABLE",
        "<A",
        "<STYLE",
        "<TITLE",
        "<B",
        "<BODY",
        "<BR",
        "<P",
        "<!--",
    ] {
        if trimmed
            .get(..tag.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(tag.as_bytes()))
            && trimmed
                .get(tag.len())
                .is_some_and(|byte| matches!(byte, b' ' | b'>'))
        {
            return "text/html; charset=utf-8";
        }
    }
    if trimmed.starts_with(b"<?xml") {
        return "text/xml; charset=utf-8";
    }
    for (pattern, mime) in [
        (b"%PDF-".as_slice(), "application/pdf"),
        (b"%!PS-Adobe-", "application/postscript"),
        (b"\0\0\x01\0", "image/x-icon"),
        (b"\0\0\x02\0", "image/x-icon"),
        (b"BM", "image/bmp"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"ID3", "audio/mpeg"),
        (b"OggS\0", "application/ogg"),
        (b"MThd\0\0\0\x06", "audio/midi"),
        (b"\x1aE\xdf\xa3", "video/webm"),
        (b"\0\x01\0\0", "font/ttf"),
        (b"OTTO", "font/otf"),
        (b"ttcf", "font/collection"),
        (b"wOFF", "font/woff"),
        (b"wOF2", "font/woff2"),
        (b"\x1f\x8b\x08", "application/x-gzip"),
        (b"PK\x03\x04", "application/zip"),
        (b"Rar!\x1a\x07\0", "application/x-rar-compressed"),
        (b"Rar!\x1a\x07\x01\0", "application/x-rar-compressed"),
        (b"\0asm", "application/wasm"),
    ] {
        if data.starts_with(pattern) {
            return mime;
        }
    }
    if data.len() >= 4 {
        if data.starts_with(b"\xfe\xff") {
            return "text/plain; charset=utf-16be";
        }
        if data.starts_with(b"\xff\xfe") {
            return "text/plain; charset=utf-16le";
        }
        if data.starts_with(b"\xef\xbb\xbf") {
            return "text/plain; charset=utf-8";
        }
    }
    for (container, kind, mime) in [
        (b"RIFF", b"WEBPVP".as_slice(), "image/webp"),
        (b"RIFF", b"AVI ", "video/avi"),
        (b"RIFF", b"WAVE", "audio/wave"),
        (b"FORM", b"AIFF", "audio/aiff"),
    ] {
        if data.starts_with(container) && data.get(8..8 + kind.len()) == Some(kind) {
            return mime;
        }
    }
    if data.get(34..36) == Some(b"LP") {
        return "application/vnd.ms-fontobject";
    }
    if data.len() >= 12 {
        let size = u32::from_be_bytes(data[..4].try_into().unwrap_or_default()) as usize;
        if size >= 12
            && size <= data.len()
            && size.is_multiple_of(4)
            && &data[4..8] == b"ftyp"
            && (8..size)
                .step_by(4)
                .any(|offset| offset != 12 && &data[offset..offset + 3] == b"mp4")
        {
            return "video/mp4";
        }
    }
    if trimmed
        .iter()
        .any(|byte| matches!(byte, 0..=8 | 11 | 14..=26 | 28..=31))
    {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    }
}
