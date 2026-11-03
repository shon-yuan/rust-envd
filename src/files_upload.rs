//! Streaming POST /files behavior pinned to runtime 51083a83, upload.go.
//!
//! The decoder, the multipart scanner and the response shape are shared by
//! both daemons; the platform-owned parts (ownership, creation mode, file
//! identity, `user.e2b.*` metadata) go through [`crate::platform::file_writes`].
//! HTTP adapter errors carry complete protocol responses from shared auth helpers.
#![allow(clippy::result_large_err)]
use crate::{
    auth::ReadWrite,
    exec_context::{Account, Defaults},
    files_http::{authorize, json_error, lookup_account, Parameters},
    http::ConfigurationState,
    platform::file_writes as files,
};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Request},
    response::{IntoResponse, Response},
};
use flate2::read::MultiGzDecoder;
use futures_util::{Stream, StreamExt, TryStreamExt};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    path::Path,
    pin::Pin,
    sync::Arc,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, oneshot},
};
use tokio_util::io::{StreamReader, SyncIoBridge};

pub async fn upload<S: ConfigurationState>(
    State(state): State<Arc<S>>,
    request: Request<Body>,
) -> Response {
    match receive(state.as_ref(), request).await {
        Ok(response) | Err(response) => response,
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;
struct Input {
    stream: ByteStream,
    cancel: Option<oneshot::Sender<()>>,
}
impl Drop for Input {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

async fn receive<S: ConfigurationState>(
    state: &S,
    request: Request<Body>,
) -> Result<Response, Response> {
    let query = request.uri().query().unwrap_or("");
    let params = Parameters::parse(query)?;
    let supplied_path = params.path_present;
    let defaults = authorize(state, &params, request.headers(), ReadWrite::Write).await?;
    let username = defaults
        .resolve_username(params.username.as_deref())
        .map_err(|e| json_error(400, &e.to_string()))?
        .to_owned();
    let (parts, body) = request.into_parts();
    let mut input = decode(body, &parts.headers).await?;
    let account = tokio::task::spawn_blocking(move || lookup_account(&username))
        .await
        .map_err(|_| json_error(500, "account lookup failed"))??;
    let metadata = metadata_headers(&parts.headers)?;
    let content_type = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let parsed_type = content_type.parse::<mime_guess::mime::Mime>().ok();
    let media = parsed_type
        .as_ref()
        .map(|kind| kind.essence_str())
        .unwrap_or("");
    let length = parts
        .headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let entries = if media == "application/octet-stream" {
        if !supplied_path {
            return Err(json_error(
                400,
                "path query parameter is required for raw body upload",
            ));
        }
        let path = resolve(&defaults, &account, &params.path)?;
        let file = open(&path, &account).await?;
        persist(file, &mut input.stream, &metadata, length).await?;
        vec![entry(path).await?]
    } else if media.starts_with("multipart/") {
        let boundary = parsed_type
            .as_ref()
            .and_then(|kind| kind.get_param("boundary"))
            .filter(|value| !value.as_str().is_empty())
            .map(|value| value.as_str().to_owned())
            .ok_or_else(|| {
                json_error(
                    500,
                    "error parsing multipart form: no multipart boundary param in Content-Type",
                )
            })?;
        // Go MultipartReader accepts form-data and mixed; other multipart types fail.
        if !matches!(media, "multipart/form-data" | "multipart/mixed") {
            return Err(json_error(
                500,
                "error parsing multipart form: request Content-Type isn't multipart/form-data",
            ));
        }
        multipart(
            &mut input.stream,
            boundary,
            &params,
            supplied_path,
            &defaults,
            &account,
            &metadata,
            length,
        )
        .await?
    } else {
        return Err(json_error(400,&format!("unsupported content type: {content_type}, expected multipart/form-data or application/octet-stream")));
    };
    // Go writes the marshaled array with no explicit Content-Type, so net/http
    // sniffs it: JSON bytes are all printable text, yielding
    // "text/plain; charset=utf-8" (verified against http.DetectContentType).
    let mut response = Body::from(serde_json::Value::Array(entries).to_string()).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(response)
}

fn metadata_headers(headers: &HeaderMap) -> Result<HashMap<String, String>, Response> {
    let mut values = HashMap::new();
    for name in headers.keys() {
        if let Some(key) = name
            .as_str()
            .strip_prefix("x-metadata-")
            .filter(|key| !key.is_empty())
        {
            let value = headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    json_error(400, "invalid metadata: metadata must be printable ASCII")
                })?;
            values.insert(key.to_owned(), value.to_owned());
        }
    }
    files::validate_metadata(&values)
        .map_err(|e| json_error(400, &format!("invalid metadata: {e}")))?;
    Ok(values)
}
fn resolve(defaults: &Defaults, account: &Account, path: &str) -> Result<String, Response> {
    defaults
        .resolve_path(path, account)
        .map_err(|e| json_error(400, &format!("error resolving path: {e}")))
}

async fn decode(body: Body, headers: &HeaderMap) -> Result<Input, Response> {
    let value = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let encoding = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let source = body.into_data_stream().map_err(io::Error::other);
    if value.is_empty() || encoding == "identity" {
        return Ok(Input {
            stream: Box::pin(source),
            cancel: None,
        });
    }
    if encoding != "gzip" {
        return Err(json_error(400,&format!("error decompressing request body: unsupported Content-Encoding: {value}, supported: [gzip]")));
    }
    let (cancel, canceled) = oneshot::channel();
    let source = source.take_until(async {
        let _ = canceled.await;
    });
    let bridge = SyncIoBridge::new(StreamReader::new(Box::pin(source)));
    let (ready, started) = oneshot::channel();
    let (sender, receiver) = mpsc::channel(1);
    tokio::task::spawn_blocking(move || {
        let mut decoder = MultiGzDecoder::new(bridge);
        if decoder.header().is_none() {
            let error = decoder.read(&mut [0u8; 1]).err().unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "invalid gzip header")
            });
            let _ = ready.send(Err(go_gzip_text(&error)));
            return;
        }
        if ready.send(Ok(())).is_err() {
            return;
        }
        loop {
            let mut chunk = vec![0; 32768];
            let value = match decoder.read(&mut chunk) {
                Ok(0) => break,
                Ok(size) => {
                    chunk.truncate(size);
                    Ok(Bytes::from(chunk))
                }
                Err(error) => Err(error),
            };
            let terminal = value.is_err();
            if sender.blocking_send(value).is_err() || terminal {
                break;
            }
        }
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|value| (value, receiver))
    });
    let input = Input {
        stream: Box::pin(stream),
        cancel: Some(cancel),
    };
    started
        .await
        .map_err(|_| json_error(400, "error decompressing request body: gzip worker stopped"))?
        .map_err(|e| {
            json_error(
                400,
                &format!("error decompressing request body: failed to create gzip reader: {e}"),
            )
        })?;
    Ok(input)
}

/// flate2's gzip header errors rendered as Go compress/gzip's texts: the
/// oracle's getDecompressedBody wraps gzip.NewReader's error verbatim.
fn go_gzip_text(error: &io::Error) -> String {
    match error.to_string().as_str() {
        "invalid gzip header" => "gzip: invalid header".to_owned(),
        "unexpected end of file" => "unexpected EOF".to_owned(),
        other => other.to_owned(),
    }
}

async fn open(path: &str, account: &Account) -> Result<tokio::fs::File, Response> {
    let path = path.to_owned();
    let account = account.clone();
    let file = tokio::task::spawn_blocking(move || {
        let path = Path::new(&path);
        files::ensure_dirs(path.parent().unwrap_or(path), &account)
            .map_err(|e| json_error(500, &format!("error ensuring directories: {e}")))?;
        let mut adopted = false;
        match fs::metadata(path) {
            Ok(info) if info.is_dir() => {
                return Err(json_error(
                    400,
                    &format!("path is a directory: {}", path.display()),
                ))
            }
            Ok(_) => {
                adopted = files::adopt_path_owner(path, &account)
                    .map_err(|e| json_error(500, &format!("error changing file ownership: {e}")))?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(json_error(500, &format!("error getting file info: {e}"))),
        }
        let file = files::open_for_write(path).map_err(|e| {
            if files::is_no_space(&e) {
                json_error(507, &format!("not enough inodes available: {e}"))
            } else {
                json_error(500, &format!("error opening file: {e}"))
            }
        })?;
        if !adopted {
            files::adopt_file_owner(&file, &account)
                .map_err(|e| json_error(500, &format!("error changing file ownership: {e}")))?;
        }
        Ok(file)
    })
    .await
    .map_err(|_| json_error(500, "file open worker failed"))??;
    Ok(tokio::fs::File::from_std(file))
}
async fn persist<S>(
    mut file: tokio::fs::File,
    stream: &mut S,
    metadata: &HashMap<String, String>,
    length: Option<u64>,
) -> Result<(), Response>
where
    S: Stream<Item = io::Result<Bytes>> + Unpin + ?Sized,
{
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                file.flush().await.map_err(|e| write_error(e, length))?;
                return Err(write_error(error, length));
            }
        };
        file.write_all(&chunk)
            .await
            .map_err(|e| write_error(e, length))?;
    }
    file.flush().await.map_err(|e| write_error(e, length))?;
    let file = file.into_std().await;
    let values = metadata.clone();
    tokio::task::spawn_blocking(move || match files::write_metadata(&file, &values) {
        Ok(()) => Ok(()),
        Err(e) if files::metadata_unsupported(&e) => {
            eprintln!("upload filesystem does not support xattrs; metadata not persisted: {e}");
            Ok(())
        }
        Err(e) if files::is_no_space_or_quota(&e) => Err(json_error(
            507,
            &format!("not enough space for file metadata: {e}"),
        )),
        Err(e) => Err(json_error(
            500,
            &format!("error writing file metadata: {e}"),
        )),
    })
    .await
    .map_err(|_| json_error(500, "metadata worker failed"))?
}
fn write_error(error: io::Error, length: Option<u64>) -> Response {
    if files::is_no_space(&error) {
        let prefix = length
            .filter(|length| *length > 0)
            .map(|length| format!("attempted to write {length} bytes: "))
            .unwrap_or_default();
        json_error(507, &format!("{prefix}not enough disk space available"))
    } else {
        json_error(500, &format!("error writing file: {error}"))
    }
}
async fn entry(path: String) -> Result<serde_json::Value, Response> {
    tokio::task::spawn_blocking(move || {
        let mut entry=serde_json::json!({"name":Path::new(&path).file_name().unwrap_or_default().to_string_lossy(),"path":path,"type":"file"});
        match files::read_metadata(Path::new(&path)) {
            Ok(values) if !values.is_empty()=>entry["metadata"]=serde_json::json!(values),
            Err(e)=>eprintln!("failed to read back file metadata for upload response: {e}"),
            _=>{},
        }
        entry
    }).await.map_err(|_|json_error(500,"upload response worker failed"))
}
fn disposition(field: &multer::Field<'_>) -> Option<mime_guess::mime::Mime> {
    let header = field.headers().get("content-disposition")?.to_str().ok()?;
    format!("application/{header}")
        .parse::<mime_guess::mime::Mime>()
        .ok()
}
fn filename(parsed: &mime_guess::mime::Mime) -> Option<String> {
    if let Some(extended) = parsed.get_param("filename*") {
        let mut parts = extended.as_str().splitn(3, '\'');
        let charset = parts.next()?;
        let _language = parts.next()?;
        let encoded = parts.next()?;
        if charset.eq_ignore_ascii_case("utf-8") || charset.eq_ignore_ascii_case("us-ascii") {
            if let Ok(decoded) = percent_encoding::percent_decode_str(encoded).decode_utf8() {
                return Some(decoded.into_owned());
            }
        }
    }
    parsed
        .get_param("filename")
        .map(|value| value.as_str().to_owned())
}

// Compatibility implementation of Go mime/quotedprintable.Reader's documented
// deviations: preserve LF/high bytes, accept lowercase hex and literal invalid
// escapes, remove trailing whitespace and soft line breaks. State is bounded to
// the 4096-byte ReadSlice fragment used by Go's bufio.Reader.
fn quoted_stream<S>(source: S) -> impl Stream<Item = io::Result<Bytes>>
where
    S: Stream<Item = io::Result<Bytes>> + Unpin,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    let reader = BufReader::with_capacity(4096, StreamReader::new(source));
    futures_util::stream::unfold(
        (reader, Vec::new(), None, false),
        |(mut reader, mut line, pending, mut ended)| async move {
            if let Some(error) = pending {
                return Some((Err(error), (reader, line, None, true)));
            }
            if ended {
                return None;
            }
            loop {
                let available = match reader.fill_buf().await {
                    Ok(data) => data,
                    Err(error) => return Some((Err(error), (reader, line, None, true))),
                };
                let eof = available.is_empty();
                let count = available
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|n| n + 1)
                    .unwrap_or(available.len())
                    .min(4096 - line.len());
                line.extend_from_slice(&available[..count]);
                reader.consume(count);
                if eof && line.is_empty() {
                    return None;
                }
                if eof || line.ends_with(b"\n") || line.len() == 4096 {
                    ended = eof;
                    let too_long = line.len() == 4096 && !line.ends_with(b"\n");
                    let (output, error) = quoted_line(&line, eof);
                    line.clear();
                    let pending =
                        error.or_else(|| too_long.then(|| io::Error::other("bufio: buffer full")));
                    return Some((Ok(Bytes::from(output)), (reader, line, pending, ended)));
                }
            }
        },
    )
}
fn quoted_line(line: &[u8], eof: bool) -> (Vec<u8>, Option<io::Error>) {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\r' | b'\n' | b' ' | b'\t') {
        end -= 1;
    }
    let soft = end > 0 && line[end - 1] == b'=';
    if soft {
        end -= 1;
    }
    let mut output = Vec::new();
    let mut index = 0;
    while index < end {
        let byte = line[index];
        if byte == b'=' && index + 2 < end {
            let high = (line[index + 1] as char).to_digit(16);
            let low = (line[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                output.push(((high << 4) | low) as u8);
                index += 3;
                continue;
            }
        }
        if byte == b'=' && (index + 1 >= end || matches!(line[index + 1], b'\r' | b'\n')) {
            return (
                output,
                Some(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "quotedprintable: invalid escape",
                )),
            );
        }
        if (byte < 0x20 && !matches!(byte, b'\t' | b'\r' | b'\n')) || byte == 0x7f {
            return (
                output,
                Some(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("quotedprintable: invalid unescaped byte 0x{byte:02x} in body"),
                )),
            );
        }
        output.push(byte);
        index += 1;
    }
    if soft {
        let suffix = &line[end + 1..];
        let suffix = suffix
            .iter()
            .copied()
            .skip_while(|byte| matches!(byte, b' ' | b'\t'))
            .collect::<Vec<_>>();
        if !suffix.starts_with(b"\n")
            && !suffix.starts_with(b"\r\n")
            && !(suffix.is_empty() && end > 0 && eof)
        {
            return (
                output,
                Some(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "quotedprintable: invalid bytes after =",
                )),
            );
        }
    } else if line.ends_with(b"\r\n") {
        output.extend_from_slice(b"\r\n");
    } else if line.ends_with(b"\n") {
        output.push(b'\n');
    }
    (output, None)
}

// Header limits apply to each part, never to the file body. Inspect input before
// multer buffers it, and yield between chunks because multer otherwise eagerly
// drains every ready upstream frame before parsing any of them.
const MAX_PART_HEADERS: usize = 10 << 20;
struct HeaderGuard {
    marker: Vec<u8>,
    tail: Vec<u8>,
    headers: Option<Vec<u8>>,
    boundary_line: Option<Vec<u8>>,
}
impl HeaderGuard {
    fn inspect(&mut self, data: &[u8]) -> io::Result<()> {
        for &byte in data {
            if let Some(headers) = self.headers.as_mut() {
                headers.push(byte);
                if headers.len() > MAX_PART_HEADERS {
                    return Err(io::Error::other("multipart: message too large"));
                }
                if headers.ends_with(b"\r\n\r\n") || headers == b"\r\n" {
                    Self::budget(headers)?;
                    self.headers = None;
                }
            } else if let Some(line) = self.boundary_line.as_mut() {
                line.push(byte);
                if line.len() > 4096 {
                    return Err(io::Error::other("multipart: invalid boundary line"));
                }
                if byte == b'\n' {
                    let final_boundary = line.starts_with(b"--");
                    self.boundary_line = None;
                    if !final_boundary {
                        self.headers = Some(Vec::new());
                    }
                }
            } else {
                self.tail.push(byte);
                if self.tail.ends_with(&self.marker) {
                    self.tail.clear();
                    self.boundary_line = Some(Vec::new());
                } else if self.tail.len() > self.marker.len() {
                    self.tail.remove(0);
                }
            }
        }
        Ok(())
    }
    fn budget(headers: &[u8]) -> io::Result<()> {
        let mut seen = std::collections::HashSet::new();
        let mut total = 0;
        for line in headers.split(|byte| *byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(colon) = line.iter().position(|byte| *byte == b':') {
                let key = line[..colon].to_ascii_lowercase();
                if seen.insert(key.clone()) {
                    total += key.len() + 200;
                }
                total += line[colon + 1..]
                    .iter()
                    .skip_while(|byte| matches!(byte, b' ' | b'\t'))
                    .count();
            }
        }
        if total > MAX_PART_HEADERS {
            return Err(io::Error::other("multipart: message too large"));
        }
        Ok(())
    }
}
async fn first_boundary(
    source: &mut ByteStream,
    remaining: &mut Bytes,
    boundary: &str,
) -> io::Result<Bytes> {
    let marker = format!("--{boundary}");
    let mut line = Vec::new();
    loop {
        if remaining.is_empty() {
            *remaining = match source.next().await.transpose()? {
                Some(data) => data,
                None if line
                    .strip_prefix(format!("{marker}--").as_bytes())
                    .is_some_and(|rest| rest.iter().all(|byte| matches!(byte, b' ' | b'\t'))) =>
                {
                    return Ok(Bytes::from(line))
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "multipart: missing boundary",
                    ))
                }
            };
            if remaining.is_empty() {
                continue;
            }
        }
        let count = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|n| n + 1)
            .unwrap_or(remaining.len())
            .min(4096 - line.len());
        line.extend_from_slice(&remaining.split_to(count));
        if line.ends_with(b"\n") {
            if let Some(rest) = line.strip_prefix(marker.as_bytes()) {
                let rest = rest.strip_suffix(b"\n").unwrap_or(rest);
                let rest = rest.strip_suffix(b"\r").unwrap_or(rest);
                let rest = rest.strip_prefix(b"--").unwrap_or(rest);
                if rest.iter().all(|byte| matches!(byte, b' ' | b'\t')) {
                    return Ok(Bytes::from(line));
                }
            }
            line.clear();
        } else if line.len() == 4096 {
            return Err(io::Error::other("multipart: NextPart: bufio: buffer full"));
        }
    }
}
fn guarded_parts<S>(source: S, boundary: &str) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    S: Stream<Item = io::Result<Bytes>> + Send + Unpin,
{
    let guard = HeaderGuard {
        marker: format!("\r\n--{boundary}").into_bytes(),
        tail: b"\r\n".to_vec(),
        headers: None,
        boundary_line: None,
    };
    futures_util::stream::unfold(
        (source, guard, Bytes::new(), false),
        |(mut source, mut guard, mut remaining, ended)| async move {
            if ended {
                return None;
            }
            tokio::task::yield_now().await;
            if remaining.is_empty() {
                remaining = match source.next().await {
                    Some(Ok(data)) => data,
                    Some(Err(e)) => return Some((Err(e), (source, guard, remaining, true))),
                    None => return None,
                };
            }
            let data = remaining.split_to(remaining.len().min(32768));
            let result = guard.inspect(&data).map(|()| data);
            let ended = result.is_err();
            Some((result, (source, guard, remaining, ended)))
        },
    )
}

#[allow(clippy::too_many_arguments)]
async fn multipart(
    stream: &mut ByteStream,
    boundary: String,
    params: &Parameters,
    supplied_path: bool,
    defaults: &Defaults,
    account: &Account,
    metadata: &HashMap<String, String>,
    length: Option<u64>,
) -> Result<Vec<serde_json::Value>, Response> {
    let mut remaining = Bytes::new();
    let first = first_boundary(stream, &mut remaining, &boundary)
        .await
        .map_err(|e| json_error(500, &format!("error reading form: {e}")))?;
    let prefix = futures_util::stream::iter([Ok(first), Ok(remaining)]);
    let guarded = guarded_parts(prefix.chain(stream.by_ref()), &boundary);
    let mut reader = multer::Multipart::new(guarded, boundary);
    let mut entries = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    loop {
        // Go mime/multipart reports io.EOF when the stream ends inside the next
        // part's headers (nextPart returns io.EOF for that shape), and
        // PostFiles then answers with the parts collected so far. multer calls
        // the same condition IncompleteStream; the corpus' truncated multipart
        // body is exactly this case.
        let mut field = match reader.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) | Err(multer::Error::IncompleteStream) => break,
            Err(error) => {
                return Err(json_error(500, &format!("error reading form: {error}")));
            }
        };
        let parsed = disposition(&field);
        let is_file = parsed.as_ref().is_some_and(|value| {
            value.subtype() == "form-data"
                && value
                    .get_param("name")
                    .is_some_and(|name| name.as_str() == "file")
        });
        if !is_file {
            while field
                .chunk()
                .await
                .map_err(|e| json_error(500, &format!("error reading form: {e}")))?
                .is_some()
            {}
            continue;
        }
        let filename = parsed.as_ref().and_then(filename);
        let supplied = if supplied_path {
            &params.path
        } else {
            filename.as_deref().ok_or_else(||json_error(400,"error getting multipart custom part file name: filename not found in Content-Disposition header"))?
        };
        let path = resolve(defaults, account, supplied)?;
        let target = tokio::fs::metadata(&path).await.ok();
        for old in &paths {
            let same = if target.is_some() {
                let old = old.clone();
                let current = path.clone();
                tokio::task::spawn_blocking(move || {
                    files::same_file(Path::new(&old), Path::new(&current))
                })
                .await
                .unwrap_or(false)
            } else {
                false
            };
            if path == *old || same {
                let others = paths
                    .iter()
                    .filter(|path| *path != old)
                    .cloned()
                    .collect::<Vec<_>>();
                let suffix = if others.len() > 1 {
                    format!(
                        ", also the following files were uploaded: {}",
                        others.join(", ")
                    )
                } else {
                    String::new()
                };
                return Err(json_error(400,&format!("you cannot upload multiple files to the same path '{old}' in one upload request, only the first specified file was uploaded{suffix}")));
            }
        }
        let file = open(&path, account).await?;
        let quoted = field
            .headers()
            .get("content-transfer-encoding")
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"quoted-printable"));
        let chunks = field.by_ref().map_err(io::Error::other);
        tokio::pin!(chunks);
        if quoted {
            let decoded = quoted_stream(chunks);
            tokio::pin!(decoded);
            persist(file, &mut decoded, metadata, length).await?;
        } else {
            persist(file, &mut chunks, metadata, length).await?;
        }
        entries.push(entry(path.clone()).await?);
        paths.push(path);
    }
    Ok(entries)
}
