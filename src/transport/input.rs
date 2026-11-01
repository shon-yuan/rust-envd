//! Match connect-go's receive errors while retaining the library's codec and backpressure.
use std::sync::{Arc, Mutex};

use axum::{
    body::{Body, Bytes},
    http::Request,
};
use connectrpc::{
    interceptor::{NextStream, PayloadStream, StreamRequest, StreamResponse},
    ConnectError, Interceptor, Payload,
};
use futures_util::StreamExt;

use crate::proto::process::StreamInputRequest;
#[path = "input_json.rs"]
mod json;

/// connect-go prefers the request encoding; Accept-Encoding is consulted only
/// when the request uses identity (connect v1.18.1 protocol.go).
pub(crate) fn input_response_encoding(request: &Request<Body>) -> bool {
    if request.uri().path() != "/process.Process/StreamInput"
        || !request
            .headers()
            .get("content-type")
            .is_some_and(|v| v.as_bytes().starts_with(b"application/connect+"))
    {
        return false;
    }
    let sent = request
        .headers()
        .get("connect-content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if sent == "gzip" {
        return true;
    }
    if !sent.is_empty() && sent != "identity" {
        return false;
    }
    request
        .headers()
        .get("connect-accept-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| {
            value
                .split(|c: char| c == ',' || c.is_ascii_whitespace())
                .any(|name| name == "gzip")
        })
}

/// The pinned library emits complete terminal envelopes in individual body
/// chunks, always uncompressed, including successful responses. Compress those
/// chunks alone and forward data chunks without copying or collecting the stream.
pub(crate) async fn encode_input_response(
    mut response: axum::response::Response,
    gzip: bool,
) -> axum::response::Response {
    if !gzip
        || response.status() != axum::http::StatusCode::OK
        || !response
            .headers()
            .get("content-type")
            .is_some_and(|v| v.as_bytes().starts_with(b"application/connect+"))
    {
        return response;
    }
    response.headers_mut().insert(
        "connect-content-encoding",
        axum::http::HeaderValue::from_static("gzip"),
    );
    response.headers_mut().remove("content-length");
    let (parts, body) = response.into_parts();
    let stream = body.into_data_stream().map(|item| {
        let bytes = item.map_err(std::io::Error::other)?;
        if bytes.first() != Some(&2) {
            return Ok(bytes);
        }
        if bytes.len() < 5 {
            return Err(std::io::Error::other(
                "unexpected StreamInput terminal response frame",
            ));
        }
        let length = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        if bytes.len() - 5 != length {
            return Err(std::io::Error::other(
                "incomplete StreamInput terminal response frame",
            ));
        }
        let compressed = connectrpc::CompressionProvider::compress(&GoGzip, &bytes[5..])
            .map_err(std::io::Error::other)?;
        let length = u32::try_from(compressed.len()).map_err(std::io::Error::other)?;
        let mut frame = Vec::with_capacity(5 + compressed.len());
        frame.push(3);
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(&compressed);
        Ok(Bytes::from(frame))
    });
    axum::response::Response::from_parts(parts, Body::from_stream(stream))
}

#[derive(Default)]
pub(super) struct GoGzip;
impl connectrpc::CompressionProvider for GoGzip {
    fn name(&self) -> &'static str {
        "gzip"
    }
    fn compress(&self, data: &[u8]) -> Result<Bytes, ConnectError> {
        connectrpc::CompressionProvider::compress(
            &connectrpc::compression::GzipProvider::default(),
            data,
        )
    }
    fn decompressor<'a>(
        &self,
        data: &'a [u8],
    ) -> Result<Box<dyn std::io::Read + 'a>, ConnectError> {
        Ok(Box::new(flate2::read::MultiGzDecoder::new(data)))
    }
    fn decompress_with_limit(&self, data: &[u8], _max_size: usize) -> Result<Bytes, ConnectError> {
        use std::io::Read;
        let mut reader = flate2::read::MultiGzDecoder::new(data);
        let initialized = reader.header().is_some();
        let mut output = Vec::new();
        reader.read_to_end(&mut output).map_err(|error| {
            let operation = if initialized {
                "decompress"
            } else {
                "get decompressor"
            };
            let (operation, detail) = match error.kind() {
                std::io::ErrorKind::UnexpectedEof => (operation, "unexpected EOF"),
                _ if error.to_string().contains("header") => (operation, "gzip: invalid header"),
                _ if error.to_string().contains("checksum") => {
                    ("decompress", "gzip: invalid checksum")
                }
                _ => return ConnectError::invalid_argument(format!("decompress: {error}")),
            };
            ConnectError::invalid_argument(format!("{operation}: {detail}"))
        })?;
        Ok(Bytes::from(output))
    }
}

#[derive(Default)]
struct FrameProgress {
    header: [u8; 5],
    header_len: usize,
    length: usize,
    received: usize,
    frames: usize,
    terminal: Option<(usize, u8)>,
    trailing: usize,
    error: Option<ConnectError>,
}
impl FrameProgress {
    fn complete(&self) -> bool {
        self.header_len == 5 && self.received == self.length
    }
    fn body_error(&mut self, message: &str) {
        self.error = Some(if self.terminal.is_some() && self.complete() {
            ConnectError::internal(format!(
                "corrupt response: I/O error after end-stream message: {message}"
            ))
        } else if self.header_len < 5 {
            ConnectError::invalid_argument(format!(
                "protocol error: incomplete envelope: {message}"
            ))
        } else {
            ConnectError::unknown(format!("read enveloped message: {message}"))
        });
    }
    fn eof(&mut self) {
        if self.header_len == 0 || self.complete() {
            return;
        }
        self.error = Some(ConnectError::invalid_argument(if self.header_len < 5 {
            "protocol error: incomplete envelope: unexpected EOF".into()
        } else {
            format!(
                "protocol error: promised {} bytes in enveloped message, got {} bytes",
                self.length, self.received
            )
        }));
    }
}

#[derive(Clone, Default)]
struct Progress(Arc<Mutex<FrameProgress>>);

pub(crate) fn observe_request(mut request: Request<Body>) -> Request<Body> {
    if request.uri().path() != "/process.Process/StreamInput"
        || !request
            .headers()
            .get("content-type")
            .is_some_and(|value| value.as_bytes().starts_with(b"application/connect+"))
    {
        return request;
    }
    // Normalize only this RPC's negotiated response encoding before entering
    // Rust's service: it otherwise gives Accept-Encoding priority over sent gzip.
    if input_response_encoding(&request) {
        request.headers_mut().insert(
            "connect-accept-encoding",
            axum::http::HeaderValue::from_static("gzip"),
        );
    }
    let (mut parts, body) = request.into_parts();
    let progress = Progress::default();
    parts.extensions.insert(progress.clone());
    // Yield at envelope boundaries even when a peer places many envelopes in one
    // HTTP chunk. State is a five-byte prefix and counters, never a message copy.
    // Special frames become ordinary envelopes so the existing decompressor can
    // deliver their payload to the interceptor instead of silently discarding it.
    let stream = futures_util::stream::unfold(
        (body.into_data_stream(), Bytes::new(), progress),
        |(mut body, mut pending, progress)| async move {
            loop {
                if pending.is_empty() {
                    match body.next().await {
                        Some(Ok(bytes)) => pending = bytes,
                        Some(Err(error)) => {
                            if let Ok(mut state) = progress.0.lock() {
                                state.body_error(&error.to_string());
                            }
                            return Some((Err(error), (body, pending, progress)));
                        }
                        None => {
                            if let Ok(mut state) = progress.0.lock() {
                                state.eof();
                            }
                            return None;
                        }
                    }
                }
                let output = {
                    let mut state = progress
                        .0
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    if state.terminal.is_some() && state.complete() {
                        state.trailing = state.trailing.saturating_add(pending.len());
                        pending = Bytes::new();
                        None
                    } else if state.header_len < 5 {
                        let count = (5 - state.header_len).min(pending.len());
                        let start = state.header_len;
                        state.header[start..start + count].copy_from_slice(&pending[..count]);
                        pending = pending.slice(count..);
                        state.header_len += count;
                        if state.header_len == 5 {
                            state.frames += 1;
                            state.length = u32::from_be_bytes([
                                state.header[1],
                                state.header[2],
                                state.header[3],
                                state.header[4],
                            ]) as usize;
                            state.received = 0;
                            let mut header = state.header;
                            if header[0] & !1 != 0 {
                                state.terminal = Some((state.frames, header[0]));
                                header[0] &= 1;
                            }
                            if state.complete() && state.terminal.is_none() {
                                state.header_len = 0;
                            }
                            Some(Bytes::copy_from_slice(&header))
                        } else {
                            None
                        }
                    } else {
                        let count = (state.length - state.received).min(pending.len());
                        let bytes = pending.split_to(count);
                        state.received += count;
                        if state.complete() && state.terminal.is_none() {
                            state.header_len = 0;
                        }
                        Some(bytes)
                    }
                };
                if let Some(bytes) = output {
                    return Some((Ok(bytes), (body, pending, progress)));
                }
            }
        },
    );
    Request::from_parts(parts, Body::from_stream(stream))
}

pub(super) struct InputErrors;
#[connectrpc::async_trait]
impl Interceptor for InputErrors {
    async fn intercept_streaming(
        &self,
        request: StreamRequest,
        inbound: PayloadStream,
        next: NextStream<'_>,
    ) -> Result<StreamResponse, ConnectError> {
        if request.ctx.path() != Some("/process.Process/StreamInput") {
            return next.run(request, inbound).await;
        }
        let progress = request.ctx.extensions().get::<Progress>().cloned();
        let inbound: PayloadStream = Box::pin(futures_util::stream::unfold(
            (inbound, progress, 0usize, false),
            |(mut inbound, progress, count, done)| async move {
                if done {
                    return None;
                }
                let item = match inbound.next().await {
                    Some(item) => item,
                    None => {
                        let error = progress
                            .as_ref()
                            .and_then(|p| p.0.lock().ok()?.error.clone())?;
                        return Some((Err(error), (inbound, progress, count, true)));
                    }
                };
                let count = count + 1;
                let terminal = progress
                    .as_ref()
                    .and_then(|p| p.0.lock().ok()?.terminal)
                    .filter(|(index, _)| *index == count);
                let (item, done) = if let (Some((_, flags)), Ok(payload)) = (terminal, &item) {
                    // connect-go drains a special frame before interpreting its
                    // flags or JSON. Ordinary messages are never read ahead here.
                    while inbound.next().await.is_some() {}
                    let failure = progress.as_ref().and_then(|p| {
                        let state = p.0.lock().ok()?;
                        state.error.clone().or_else(|| {
                            (state.trailing > 0).then(|| {
                                ConnectError::internal(format!(
                                    "corrupt response: {} extra bytes after end of stream",
                                    state.trailing
                                ))
                            })
                        })
                    });
                    let failure =
                        failure.or_else(|| validate_terminal(flags, payload.bytes()).err());
                    match failure {
                        Some(error) => (Err(error), true),
                        None => return None,
                    }
                } else {
                    let item = match item {
                        Ok(payload) => validate_payload(payload),
                        Err(error) => {
                            let replace = matches!(
                                error.message.as_deref(),
                                Some("incomplete request envelope")
                            ) || error.message.as_deref().is_some_and(|message| {
                                message.starts_with("failed to read request body:")
                            });
                            Err(if replace {
                                progress
                                    .as_ref()
                                    .and_then(|p| p.0.lock().ok()?.error.clone())
                                    .unwrap_or(error)
                            } else {
                                error
                            })
                        }
                    };
                    (item, false)
                };
                Some((item, (inbound, progress, count, done)))
            },
        ));
        // The handler adds the pinned envd unknown/error-streaming-input wrapper.
        next.run(request, inbound).await
    }
}
fn validate_terminal(flags: u8, bytes: &[u8]) -> Result<(), ConnectError> {
    if flags & 2 == 0 {
        return Err(ConnectError::internal(format!(
            "protocol error: invalid envelope flags {flags}"
        )));
    }
    json::validate_terminal(bytes)
        .map_err(|detail| ConnectError::internal(format!("unmarshal end stream message: {detail}")))
}
fn validate_payload(payload: Payload) -> Result<Payload, ConnectError> {
    if payload.bytes().is_empty() {
        return Ok(Payload::from_message(
            StreamInputRequest::default(),
            payload.format(),
        ));
    }
    if payload.format() == connectrpc::CodecFormat::Json {
        json::validate(payload.bytes()).map_err(|detail| {
            ConnectError::invalid_argument(format!(
                "unmarshal message: unmarshal into *process.StreamInputRequest: proto: {detail}"
            ))
        })?;
        if let Err(error) = payload.message::<StreamInputRequest>() {
            // Go permits CR/LF in base64 values. Normalize only after validating
            // the original tokens, preserving their positions in any diagnostics.
            if let Ok(bytes) = json::normalized_bytes(payload.bytes()) {
                let normalized = Payload::new(Bytes::from(bytes), payload.format());
                normalized.message::<StreamInputRequest>()?;
                return Ok(normalized);
            }
            return Err(error);
        }
    } else if payload.message::<StreamInputRequest>().is_err() {
        return Err(ConnectError::invalid_argument(
            "unmarshal message: proto: cannot parse invalid wire-format data",
        ));
    }
    Ok(payload)
}
