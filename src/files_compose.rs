//! POST /files/compose, pinned to runtime 51083a83 compose.go.
//!
//! The decoder, validation order, error strings, HTML escaping and response
//! framing are shared by both daemons. The OS-owned parts (random temp name,
//! creation mode, ownership, file identity, copy and close) go through
//! [`crate::platform::compose_files`]. Shared HTTP helpers return complete
//! responses, including protocol error headers.
#![allow(clippy::result_large_err)]
use crate::{
    compose_decode::{folded_field, quote, space, Scanner},
    exec_context::{Account, Defaults},
    files_http::{json_error, lookup_account, resolve},
    http::ConfigurationState,
    platform::compose_files::{
        adopt_owner, close_output, copy_file, create_new_temp, ensure_dirs, errno_text, identity,
        is_no_space, parent, random_bytes, rename_over, same_file, FileId,
    },
};
use axum::{
    body::Body,
    extract::State,
    http::{header, Request},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::json;
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub async fn compose<S: ConfigurationState>(
    State(state): State<Arc<S>>,
    request: Request<Body>,
) -> Response {
    let request = match decode_body(request.into_body()).await {
        Ok(request) => request,
        Err(message) => return json_error(400, &format!("invalid request body: {message}")),
    };
    let defaults = state.configuration().lock().await.defaults.clone();
    match tokio::task::spawn_blocking(move || assemble(request, &defaults)).await {
        Ok(Ok(path)) => {
            let name = Path::new(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            (
                [(header::CONTENT_TYPE, "application/json")],
                format!("{}\n", json!({"path":path,"name":name,"type":"file"}))
                    .replace('<', "\\u003c")
                    .replace('>', "\\u003e")
                    .replace('&', "\\u0026")
                    .replace('\u{2028}', "\\u2028")
                    .replace('\u{2029}', "\\u2029"),
            )
                .into_response()
        }
        Ok(Err(response)) => response,
        Err(_) => json_error(500, "compose worker failed"),
    }
}

// Decoder.Decode scans one complete JSON value before unmarshalling. Scan each
// body chunk once and stop at that value, without waiting for request EOF or
// retaining trailing bytes. The explicit stack matches Go's 10,000-depth limit.
async fn decode_body(body: Body) -> Result<ComposeRequest, String> {
    let mut stream = body.into_data_stream();
    let mut scanner = Scanner::default();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        for byte in chunk {
            bytes.push(byte);
            if scanner.step(byte)? {
                return ComposeRequest::decode(&bytes);
            }
        }
    }
    if scanner.eof() {
        ComposeRequest::decode(&bytes)
    } else if bytes.iter().all(|byte| space(*byte)) {
        Err("EOF".into())
    } else {
        Err("unexpected EOF".into())
    }
}

#[derive(Default)]
struct ComposeRequest {
    sources: Vec<String>,
    source_count: usize,
    destination: String,
    username: Option<String>,
}
impl ComposeRequest {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut cursor = Cursor { bytes, offset: 0 };
        let mut request = Self::default();
        match cursor.peek() {
            b'n' => return Ok(request),
            b'{' => cursor.offset += 1,
            _ => return Err(type_error(cursor.value(), "", "api.ComposeRequest")),
        }
        let mut error = None;
        while cursor.peek() != b'}' {
            let key = unquote(cursor.value());
            cursor.peek();
            cursor.offset += 1; // colon, already validated by Scanner
            let value = cursor.value();
            let result = match folded_field(&key).as_str() {
                "SOURCE_PATHS" => request.sources(value),
                "DESTINATION" => assign_string(value, "destination", &mut request.destination),
                "USERNAME" => {
                    if value == b"null" {
                        request.username = None;
                        Ok(())
                    } else {
                        assign_string(
                            value,
                            "username",
                            request.username.get_or_insert_with(String::new),
                        )
                    }
                }
                _ => Ok(()),
            };
            if error.is_none() {
                error = result.err()
            }
            if cursor.peek() == b',' {
                cursor.offset += 1;
            }
        }
        request.sources.truncate(request.source_count);
        error.map_or(Ok(request), Err)
    }
    fn sources(&mut self, value: &[u8]) -> Result<(), String> {
        if value == b"null" {
            self.sources.clear();
            self.source_count = 0;
            return Ok(());
        }
        if value[0] != b'[' {
            return Err(type_error(value, "source_paths", "[]string"));
        }
        let mut cursor = Cursor {
            bytes: value,
            offset: 1,
        };
        let mut count = 0;
        let mut error = None;
        while cursor.peek() != b']' {
            if count == self.sources.len() {
                self.sources.push(String::new());
            }
            if let Err(message) =
                assign_string(cursor.value(), "source_paths", &mut self.sources[count])
            {
                if error.is_none() {
                    error = Some(message)
                }
            }
            count += 1;
            if cursor.peek() == b',' {
                cursor.offset += 1;
            }
        }
        self.source_count = count;
        if count == 0 {
            self.sources.clear();
        }
        error.map_or(Ok(()), Err)
    }
}
// Operates only on the fully syntax-validated first value. Unknown values are
// skipped without constructing numbers or recursively allocating JSON trees.
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    fn peek(&mut self) -> u8 {
        while self.offset < self.bytes.len() && space(self.bytes[self.offset]) {
            self.offset += 1
        }
        self.bytes.get(self.offset).copied().unwrap_or(b' ')
    }
    fn value(&mut self) -> &'a [u8] {
        let first = self.peek();
        let start = self.offset;
        if matches!(first, b'{' | b'[' | b'"') {
            let mut depth = 0usize;
            let mut quoted = false;
            let mut escaped = false;
            while let Some(&byte) = self.bytes.get(self.offset) {
                self.offset += 1;
                if quoted {
                    if escaped {
                        escaped = false
                    } else if byte == b'\\' {
                        escaped = true
                    } else if byte == b'"' {
                        quoted = false;
                        if depth == 0 {
                            break;
                        }
                    }
                } else {
                    match byte {
                        b'"' => quoted = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
        } else {
            while self.offset < self.bytes.len()
                && !space(self.bytes[self.offset])
                && !matches!(self.bytes[self.offset], b',' | b'}' | b']')
            {
                self.offset += 1
            }
        }
        &self.bytes[start..self.offset]
    }
}
fn assign_string(value: &[u8], field: &str, target: &mut String) -> Result<(), String> {
    if value == b"null" {
        return Ok(());
    }
    if value[0] != b'"' {
        return Err(type_error(value, field, "string"));
    }
    *target = unquote(value);
    Ok(())
}
fn unquote(value: &[u8]) -> String {
    let bytes = &value[1..value.len() - 1];
    let mut result = String::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'\\' {
            let end = bytes[offset..]
                .iter()
                .position(|byte| *byte == b'\\')
                .map_or(bytes.len(), |end| offset + end);
            let mut raw = &bytes[offset..end];
            while !raw.is_empty() {
                match std::str::from_utf8(raw) {
                    Ok(text) => {
                        result.push_str(text);
                        break;
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        result.push_str(std::str::from_utf8(&raw[..valid]).unwrap_or_default());
                        result.push('\u{fffd}');
                        raw = &raw[valid + 1..];
                    }
                }
            }
            offset = end;
            continue;
        }
        offset += 1;
        let escaped = bytes[offset];
        offset += 1;
        if escaped == b'u' {
            let hex = |slice: &[u8]| {
                slice.iter().fold(0u32, |value, digit| {
                    value * 16 + char::from(*digit).to_digit(16).unwrap_or_default()
                })
            };
            let mut rune = hex(&bytes[offset..offset + 4]);
            offset += 4;
            if (0xd800..=0xdbff).contains(&rune) && bytes.get(offset..offset + 2) == Some(b"\\u") {
                let low = hex(&bytes[offset + 2..offset + 6]);
                if (0xdc00..=0xdfff).contains(&low) {
                    rune = 0x10000 + ((rune - 0xd800) << 10) + low - 0xdc00;
                    offset += 6;
                }
            }
            result.push(char::from_u32(rune).unwrap_or('\u{fffd}'));
        } else {
            result.push(match escaped {
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                _ => char::from(escaped),
            });
        }
    }
    result
}
fn type_error(value: &[u8], field: &str, expected: &str) -> String {
    let kind = match value[0] {
        b'n' => "null",
        b't' | b'f' => "bool",
        b'"' => "string",
        b'[' => "array",
        b'{' => "object",
        _ => "number",
    };
    let target = if field.is_empty() {
        format!("Go value of type {expected}")
    } else {
        format!("Go struct field ComposeRequest.{field} of type {expected}")
    };
    format!("json: cannot unmarshal {kind} into {target}")
}

fn assemble(request: ComposeRequest, defaults: &Defaults) -> Result<String, Response> {
    if request.sources.is_empty() {
        return Err(json_error(400, "source_paths must not be empty"));
    }
    if request.destination.is_empty() {
        return Err(json_error(400, "destination is required"));
    }
    let username = defaults
        .resolve_username(request.username.as_deref())
        .map_err(|error| json_error(400, &error.to_string()))?;
    let account = lookup_account(username)?;
    let destination = resolve_path(defaults, &account, &request.destination, "destination path")?;
    // Lstat is intentional: rename replaces a destination symlink itself.
    let destination_info = identity(Path::new(&destination), false).ok();
    let sources = validate_sources(
        &request.sources,
        &destination,
        destination_info,
        defaults,
        &account,
    )?;
    let destination_path = Path::new(&destination);
    ensure_dirs(&parent(&destination), &account)
        .map_err(|error| json_error(500, &format!("error ensuring directories: {error}")))?;
    let mut temporary = temporary_file(&destination)?;
    let temporary_path = temporary.path().to_path_buf();
    adopt_owner(&temporary_path, &account).map_err(|error| {
        json_error(
            500,
            &format!(
                "error changing file ownership: chown {}: {}",
                temporary_path.display(),
                errno_text(&error)
            ),
        )
    })?;
    copy_sources(&sources, temporary.file(), &temporary_path)?;
    temporary.close().map_err(|error| {
        json_error(
            500,
            &format!(
                "error closing destination file: close {}: {}",
                temporary_path.display(),
                errno_text(&error)
            ),
        )
    })?;
    rename_over(&temporary_path, destination_path).map_err(|error| {
        json_error(
            500,
            &format!(
                "error finalizing compose: rename {} {}: {}",
                temporary_path.display(),
                destination,
                errno_text(&error)
            ),
        )
    })?;
    cleanup_sources(&sources, &destination);
    Ok(destination)
}
fn resolve_path(
    defaults: &Defaults,
    account: &Account,
    path: &str,
    context: &str,
) -> Result<String, Response> {
    // Add compose's operation context while reusing the shared path resolver.
    defaults.resolve_path(path, account).map_err(|error| {
        json_error(
            400,
            &format!(
                "error resolving {context}: failed to expand path '' for user '{}': {error}",
                account.name
            ),
        )
    })?;
    resolve(defaults, Some(&account.name), path)
}
fn validate_sources(
    sources: &[String],
    destination: &str,
    destination_info: Option<FileId>,
    defaults: &Defaults,
    account: &Account,
) -> Result<Vec<String>, Response> {
    sources
        .iter()
        .map(|source| {
            let path = resolve_path(
                defaults,
                account,
                source,
                &format!("source path {}", quote(source)),
            )?;
            let same_error = || {
                json_error(
                    400,
                    &format!(
                        "source path {} cannot be the same as destination",
                        quote(source)
                    ),
                )
            };
            if path == destination {
                return Err(same_error());
            }
            let info = fs::metadata(&path)
                .map_err(|_| json_error(404, &format!("source file not found: {source}")))?;
            if !info.is_file() {
                return Err(json_error(
                    400,
                    &format!("source path is not a regular file: {source}"),
                ));
            }
            if destination_info.is_some_and(|dest| {
                identity(Path::new(&path), true).is_ok_and(|info| same_file(info, dest))
            }) {
                return Err(same_error());
            }
            Ok(path)
        })
        .collect()
}
/// Owns the temporary path and the output handle. The handle is always closed
/// (or dropped) before the guard unlinks the file, which Windows requires.
struct Temporary {
    path: PathBuf,
    output: Option<File>,
}
impl Temporary {
    fn path(&self) -> &Path {
        &self.path
    }
    fn file(&mut self) -> &mut File {
        self.output
            .as_mut()
            .expect("temporary output is open until closed")
    }
    fn close(&mut self) -> io::Result<()> {
        match self.output.take() {
            Some(file) => close_output(file),
            None => Ok(()),
        }
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        // Close before unlinking: Windows refuses to remove a file that is
        // still open unless every handle allows delete sharing.
        drop(self.output.take());
        // Best-effort rollback, including after a successful rename removes this name.
        let _ = fs::remove_file(&self.path);
    }
}
fn temporary_file(destination: &str) -> Result<Temporary, Response> {
    let mut random = random_bytes()
        .map_err(|error| json_error(500, &format!("error creating destination file: {error}")))?;
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let id: String = random
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            format!(
                "{}{byte:02x}",
                if matches!(index, 4 | 6 | 8 | 10) {
                    "-"
                } else {
                    ""
                }
            )
        })
        .collect();
    let path = PathBuf::from(format!("{destination}.e2b-compose.{id}.tmp"));
    let file = create_new_temp(&path).map_err(|error| {
        storage_error(
            "error creating destination file",
            &format!("open {}: {}", path.display(), errno_text(&error)),
            &error,
        )
    })?;
    Ok(Temporary {
        path,
        output: Some(file),
    })
}
fn copy_sources(sources: &[String], output: &mut File, temporary: &Path) -> Result<(), Response> {
    for source in sources {
        let mut input = File::open(source).map_err(|error| {
            json_error(
                500,
                &format!(
                    "error opening source file {source}: open {source}: {}",
                    errno_text(&error)
                ),
            )
        })?;
        copy_file(&mut input, output, source, temporary).map_err(|(operation, error)| {
            storage_error(
                &format!("error composing source {source}"),
                &format!("{operation}: {}", errno_text(&error)),
                &error,
            )
        })?;
    }
    Ok(())
}
fn storage_error(context: &str, detail: &str, error: &io::Error) -> Response {
    if is_no_space(error) {
        json_error(507, "not enough disk space available")
    } else {
        json_error(500, &format!("{context}: {detail}"))
    }
}
fn cleanup_sources(sources: &[String], destination: &str) {
    let composed = identity(Path::new(destination), true).ok();
    for source in sources {
        if let (Ok(info), Some(composed)) = (identity(Path::new(source), false), composed) {
            if same_file(info, composed) {
                continue;
            }
        }
        // Official cleanup is best-effort, including duplicate paths and symlinks.
        let _ = fs::remove_file(source);
    }
}
