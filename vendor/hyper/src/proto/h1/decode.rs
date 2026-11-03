use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::task::{Context, Poll};

use bytes::{BufMut, Bytes, BytesMut};
use futures_core::ready;
use http::{HeaderMap, HeaderName, HeaderValue};
use http_body::Frame;

use super::io::MemRead;
use super::role::DEFAULT_MAX_HEADERS;
use super::DecodedLength;

use self::Kind::{Chunked, Eof, Length};

/// Largest chunk-size line Go accepts, plus one.
///
/// `maxLineLength` is 4096, but `bufio.ReadSlice` must find the CRLF inside
/// its 4096-byte buffer, so a 4095-byte line already fails
/// (`internal/chunked.go:19,155-181`). A line of 4094 bytes is the last one
/// accepted; this constant is the exclusive bound.
const CHUNKED_MAX_LINE_LENGTH: u64 = 4095;

/// Go's allowance for chunked framing overhead, after charging 16 bytes plus
/// twice the chunk data per chunk (`internal/chunked.go:73-80`).
const CHUNKED_MAX_EXCESS: i64 = 16 * 1024;

/// Maximum number of bytes allowed for all trailer fields.
///
/// TODO: remove this when we land `h1_max_header_size` support.
const TRAILER_LIMIT: usize = 1024 * 16;

/// Decoders to handle different Transfer-Encodings.
///
/// If a message body does not include a Transfer-Encoding, it *should*
/// include a Content-Length header.
#[derive(Clone, PartialEq)]
pub(crate) struct Decoder {
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// A Reader used when a Content-Length header is passed with a positive integer.
    Length(u64),
    /// A Reader used when Transfer-Encoding is `chunked`.
    Chunked {
        state: ChunkedState,
        chunk_len: u64,
        /// Bytes consumed from the current chunk-size line, before CRLF.
        chunk_line_len: u64,
        /// Go's running chunked-overhead budget (`internal/chunked.go:41`).
        excess: i64,
        trailers_buf: Option<BytesMut>,
        trailers_cnt: usize,
        h1_max_headers: Option<usize>,
        h1_max_header_size: Option<usize>,
    },
    /// A Reader used for responses that don't indicate a length or chunked.
    ///
    /// The bool tracks when EOF is seen on the transport.
    ///
    /// Note: This should only used for `Response`s. It is illegal for a
    /// `Request` to be made with both `Content-Length` and
    /// `Transfer-Encoding: chunked` missing, as explained from the spec:
    ///
    /// > If a Transfer-Encoding header field is present in a response and
    /// > the chunked transfer coding is not the final encoding, the
    /// > message body length is determined by reading the connection until
    /// > it is closed by the server.  If a Transfer-Encoding header field
    /// > is present in a request and the chunked transfer coding is not
    /// > the final encoding, the message body length cannot be determined
    /// > reliably; the server MUST respond with the 400 (Bad Request)
    /// > status code and then close the connection.
    Eof(bool),
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum ChunkedState {
    Start,
    Size,
    SizeLws,
    Extension,
    SizeLf,
    Body,
    BodyCr,
    BodyLf,
    Trailer,
    TrailerLf,
    EndCr,
    EndLf,
    End,
}

impl Decoder {
    // constructors

    pub(crate) fn length(x: u64) -> Decoder {
        Decoder {
            kind: Kind::Length(x),
        }
    }

    pub(crate) fn chunked(
        h1_max_headers: Option<usize>,
        h1_max_header_size: Option<usize>,
    ) -> Decoder {
        Decoder {
            kind: Kind::Chunked {
                state: ChunkedState::new(),
                chunk_len: 0,
                chunk_line_len: 0,
                excess: 0,
                trailers_buf: None,
                trailers_cnt: 0,
                h1_max_headers,
                h1_max_header_size,
            },
        }
    }

    pub(crate) fn eof() -> Decoder {
        Decoder {
            kind: Kind::Eof(false),
        }
    }

    pub(super) fn new(
        len: DecodedLength,
        h1_max_headers: Option<usize>,
        h1_max_header_size: Option<usize>,
    ) -> Self {
        match len {
            DecodedLength::CHUNKED => Decoder::chunked(h1_max_headers, h1_max_header_size),
            DecodedLength::CLOSE_DELIMITED => Decoder::eof(),
            length => Decoder::length(length.danger_len()),
        }
    }

    // methods

    pub(crate) fn is_eof(&self) -> bool {
        matches!(
            self.kind,
            Length(0)
                | Chunked {
                    state: ChunkedState::End,
                    ..
                }
                | Eof(true)
        )
    }

    pub(crate) fn decode<R: MemRead>(
        &mut self,
        cx: &mut Context<'_>,
        body: &mut R,
    ) -> Poll<Result<Frame<Bytes>, io::Error>> {
        trace!("decode; state={:?}", self.kind);
        match &mut self.kind {
            Length(remaining) => {
                if *remaining == 0 {
                    Poll::Ready(Ok(Frame::data(Bytes::new())))
                } else {
                    let to_read = usize::try_from(*remaining).unwrap_or(usize::MAX);
                    let buf = ready!(body.read_mem(cx, to_read))?;
                    let num = buf.as_ref().len() as u64;
                    if num > *remaining {
                        *remaining = 0;
                    } else if num == 0 {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            IncompleteBody,
                        )));
                    } else {
                        *remaining -= num;
                    }
                    Poll::Ready(Ok(Frame::data(buf)))
                }
            }
            Chunked {
                state,
                chunk_len,
                chunk_line_len,
                excess,
                trailers_buf,
                trailers_cnt,
                h1_max_headers,
                h1_max_header_size,
            } => {
                let h1_max_headers = h1_max_headers.unwrap_or(DEFAULT_MAX_HEADERS);
                let h1_max_header_size = h1_max_header_size.unwrap_or(TRAILER_LIMIT);
                loop {
                    let mut buf = None;
                    // advances the chunked state
                    *state = ready!(state.step(
                        cx,
                        body,
                        StepArgs {
                            chunk_size: chunk_len,
                            chunk_line_len,
                            excess,
                            chunk_buf: &mut buf,
                            trailers_buf,
                            trailers_cnt,
                            max_headers_cnt: h1_max_headers,
                            max_headers_bytes: h1_max_header_size,
                        }
                    ))?;
                    if *state == ChunkedState::End {
                        trace!("end of chunked");

                        if trailers_buf.is_some() {
                            trace!("found possible trailers");

                            match decode_trailers(
                                &mut trailers_buf.take().expect("Trailer is None"),
                                *trailers_cnt,
                            ) {
                                Ok(headers) => {
                                    return Poll::Ready(Ok(Frame::trailers(headers)));
                                }
                                Err(e) => {
                                    return Poll::Ready(Err(e));
                                }
                            }
                        }

                        return Poll::Ready(Ok(Frame::data(Bytes::new())));
                    }
                    if let Some(buf) = buf {
                        return Poll::Ready(Ok(Frame::data(buf)));
                    }
                }
            }
            Eof(is_eof) => {
                if *is_eof {
                    Poll::Ready(Ok(Frame::data(Bytes::new())))
                } else {
                    // 8192 chosen because its about 2 packets, there probably
                    // won't be that much available, so don't have MemReaders
                    // allocate buffers to big
                    body.read_mem(cx, 8192).map_ok(|slice| {
                        *is_eof = slice.is_empty();
                        Frame::data(slice)
                    })
                }
            }
        }
    }

    #[cfg(test)]
    async fn decode_fut<R: MemRead>(&mut self, body: &mut R) -> Result<Frame<Bytes>, io::Error> {
        futures_util::future::poll_fn(move |cx| self.decode(cx, body)).await
    }
}

impl fmt::Debug for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.kind, f)
    }
}

macro_rules! byte (
    ($rdr:ident, $cx:expr) => ({
        let buf = ready!($rdr.read_mem($cx, 1))?;
        if !buf.is_empty() {
            buf[0]
        } else {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::UnexpectedEof,
                                      "unexpected EOF during chunk size line")));
        }
    })
);

macro_rules! or_overflow {
    ($e:expr) => (
        match $e {
            Some(val) => val,
            None => return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid chunk size: overflow",
            ))),
        }
    )
}

macro_rules! put_u8 {
    ($trailers_buf:expr, $byte:expr, $limit:expr) => {
        $trailers_buf.put_u8($byte);

        if $trailers_buf.len() >= $limit {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunk trailers bytes over limit",
            )));
        }
    };
}

struct StepArgs<'args> {
    chunk_size: &'args mut u64,
    chunk_line_len: &'args mut u64,
    excess: &'args mut i64,
    chunk_buf: &'args mut Option<Bytes>,
    trailers_buf: &'args mut Option<BytesMut>,
    trailers_cnt: &'args mut usize,
    max_headers_cnt: usize,
    max_headers_bytes: usize,
}

/// Count one non-CRLF byte of the chunk-size line and apply Go's length cap.
fn count_chunk_line_byte(line_len: &mut u64) -> Result<(), io::Error> {
    *line_len += 1;
    if *line_len >= CHUNKED_MAX_LINE_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chunk size line too long",
        ));
    }
    Ok(())
}

/// Go's chunked overhead accounting, applied when a chunk-size line completes
/// (`internal/chunked.go:53,73-80`).
fn charge_chunk_excess(line_len: u64, size: u64, excess: &mut i64) -> Result<(), io::Error> {
    let line = i64::try_from(line_len).unwrap_or(i64::MAX);
    *excess = excess.saturating_add(line.saturating_add(2));
    let data = i64::try_from(size).unwrap_or(i64::MAX);
    *excess = excess.saturating_sub(16i64.saturating_add(data.saturating_mul(2)));
    if *excess < 0 {
        *excess = 0;
    }
    if *excess > CHUNKED_MAX_EXCESS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chunked encoding contains too much non-data",
        ));
    }
    Ok(())
}

impl ChunkedState {
    fn new() -> ChunkedState {
        ChunkedState::Start
    }    fn step<R: MemRead>(
        &self,
        cx: &mut Context<'_>,
        body: &mut R,
        StepArgs {
            chunk_size,
            chunk_line_len,
            excess,
            chunk_buf,
            trailers_buf,
            trailers_cnt,
            max_headers_cnt,
            max_headers_bytes,
        }: StepArgs<'_>,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        use self::ChunkedState::{
            Body, BodyCr, BodyLf, End, EndCr, EndLf, Extension, Size, SizeLf, SizeLws, Start,
            Trailer, TrailerLf,
        };
        match *self {
            Start => ChunkedState::read_start(cx, body, chunk_size, chunk_line_len),
            Size => ChunkedState::read_size(cx, body, chunk_size, chunk_line_len),
            SizeLws => ChunkedState::read_size_lws(cx, body, chunk_line_len),
            Extension => ChunkedState::read_extension(cx, body, chunk_line_len),
            SizeLf => ChunkedState::read_size_lf(cx, body, *chunk_size, *chunk_line_len, excess),
            Body => ChunkedState::read_body(cx, body, chunk_size, chunk_buf),
            BodyCr => ChunkedState::read_body_cr(cx, body),
            BodyLf => ChunkedState::read_body_lf(cx, body),
            Trailer => ChunkedState::read_trailer(cx, body, trailers_buf, max_headers_bytes),
            TrailerLf => ChunkedState::read_trailer_lf(
                cx,
                body,
                trailers_buf,
                trailers_cnt,
                max_headers_cnt,
                max_headers_bytes,
            ),
            EndCr => ChunkedState::read_end_cr(cx, body, trailers_buf, max_headers_bytes),
            EndLf => ChunkedState::read_end_lf(cx, body, trailers_buf, max_headers_bytes),
            End => Poll::Ready(Ok(ChunkedState::End)),
        }
    }

    fn read_start<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        size: &mut u64,
        line_len: &mut u64,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        trace!("Read chunk start");
        // A new chunk-size line begins here (`internal/chunked.go:41-49`).
        *line_len = 0;

        let radix = 16;
        match byte!(rdr, cx) {
            b @ b'0'..=b'9' => {
                *size = or_overflow!(size.checked_mul(radix));
                *size = or_overflow!(size.checked_add(u64::from(b - b'0')));
            }
            b @ b'a'..=b'f' => {
                *size = or_overflow!(size.checked_mul(radix));
                *size = or_overflow!(size.checked_add(u64::from(b + 10 - b'a')));
            }
            b @ b'A'..=b'F' => {
                *size = or_overflow!(size.checked_mul(radix));
                *size = or_overflow!(size.checked_add(u64::from(b + 10 - b'A')));
            }
            _ => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid chunk size line: missing size digit",
                )));
            }
        }

        if let Err(e) = count_chunk_line_byte(line_len) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(ChunkedState::Size))
    }

    fn read_size<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        size: &mut u64,
        line_len: &mut u64,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        trace!("Read chunk hex size");

        let radix = 16;
        match byte!(rdr, cx) {
            b @ b'0'..=b'9' => {
                *size = or_overflow!(size.checked_mul(radix));
                *size = or_overflow!(size.checked_add(u64::from(b - b'0')));
            }
            b @ b'a'..=b'f' => {
                *size = or_overflow!(size.checked_mul(radix));
                *size = or_overflow!(size.checked_add(u64::from(b + 10 - b'a')));
            }
            b @ b'A'..=b'F' => {
                *size = or_overflow!(size.checked_mul(radix));
                *size = or_overflow!(size.checked_add(u64::from(b + 10 - b'A')));
            }
            b'\t' | b' ' => {
                return match count_chunk_line_byte(line_len) {
                    Ok(()) => Poll::Ready(Ok(ChunkedState::SizeLws)),
                    Err(e) => Poll::Ready(Err(e)),
                };
            }
            b';' => {
                return match count_chunk_line_byte(line_len) {
                    Ok(()) => Poll::Ready(Ok(ChunkedState::Extension)),
                    Err(e) => Poll::Ready(Err(e)),
                };
            }
            // The CR is not part of the line Go measures (`readChunkLine`
            // truncates CRLF before the length check).
            b'\r' => return Poll::Ready(Ok(ChunkedState::SizeLf)),
            _ => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid chunk size line: Invalid Size",
                )));
            }
        }
        if let Err(e) = count_chunk_line_byte(line_len) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(ChunkedState::Size))
    }
    fn read_size_lws<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        line_len: &mut u64,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        trace!("read_size_lws");
        match byte!(rdr, cx) {
            // LWS can follow the chunk size, but no more digits can come
            b'\t' | b' ' => match count_chunk_line_byte(line_len) {
                Ok(()) => Poll::Ready(Ok(ChunkedState::SizeLws)),
                Err(e) => Poll::Ready(Err(e)),
            },
            b';' => match count_chunk_line_byte(line_len) {
                Ok(()) => Poll::Ready(Ok(ChunkedState::Extension)),
                Err(e) => Poll::Ready(Err(e)),
            },
            b'\r' => Poll::Ready(Ok(ChunkedState::SizeLf)),
            _ => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid chunk size linear white space",
            ))),
        }
    }
    fn read_extension<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        line_len: &mut u64,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        trace!("read_extension");
        // We don't care about extensions really at all. Just ignore them.
        // They "end" at the next CRLF.
        //
        // However, some implementations may not check for the CR, so to save
        // them from themselves, we reject extensions containing plain LF as
        // well.
        match byte!(rdr, cx) {
            b'\r' => Poll::Ready(Ok(ChunkedState::SizeLf)),
            b'\n' => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid chunk extension contains newline",
            ))),
            _ => match count_chunk_line_byte(line_len) {
                Ok(()) => Poll::Ready(Ok(ChunkedState::Extension)),
                Err(e) => Poll::Ready(Err(e)),
            }, // no supported extensions
        }
    }
    fn read_size_lf<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        size: u64,
        line_len: u64,
        excess: &mut i64,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        trace!("Chunk size is {:?}", size);
        match byte!(rdr, cx) {
            b'\n' => {
                if let Err(e) = charge_chunk_excess(line_len, size, excess) {
                    return Poll::Ready(Err(e));
                }
                if size == 0 {
                    Poll::Ready(Ok(ChunkedState::EndCr))
                } else {
                    debug!("incoming chunked header: {0:#X} ({0} bytes)", size);
                    Poll::Ready(Ok(ChunkedState::Body))
                }
            }
            _ => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid chunk size LF",
            ))),
        }
    }

    fn read_body<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        rem: &mut u64,
        buf: &mut Option<Bytes>,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        trace!("Chunked read, remaining={:?}", rem);

        // cap remaining bytes at the max capacity of usize
        let to_read = usize::try_from(*rem).unwrap_or(usize::MAX);
        let slice = ready!(rdr.read_mem(cx, to_read))?;
        let count = slice.len();

        if count == 0 {
            *rem = 0;
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                IncompleteBody,
            )));
        }
        *buf = Some(slice);
        *rem -= count as u64;

        if *rem > 0 {
            Poll::Ready(Ok(ChunkedState::Body))
        } else {
            Poll::Ready(Ok(ChunkedState::BodyCr))
        }
    }
    fn read_body_cr<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        match byte!(rdr, cx) {
            b'\r' => Poll::Ready(Ok(ChunkedState::BodyLf)),
            _ => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid chunk body CR",
            ))),
        }
    }
    fn read_body_lf<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        match byte!(rdr, cx) {
            b'\n' => Poll::Ready(Ok(ChunkedState::Start)),
            _ => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid chunk body LF",
            ))),
        }
    }

    fn read_trailer<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        trailers_buf: &mut Option<BytesMut>,
        h1_max_header_size: usize,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        trace!("read_trailer");
        let byte = byte!(rdr, cx);

        put_u8!(
            trailers_buf.as_mut().expect("trailers_buf is None"),
            byte,
            h1_max_header_size
        );

        match byte {
            b'\r' => Poll::Ready(Ok(ChunkedState::TrailerLf)),
            _ => Poll::Ready(Ok(ChunkedState::Trailer)),
        }
    }

    fn read_trailer_lf<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        trailers_buf: &mut Option<BytesMut>,
        trailers_cnt: &mut usize,
        h1_max_headers: usize,
        h1_max_header_size: usize,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        let byte = byte!(rdr, cx);
        match byte {
            b'\n' => {
                if *trailers_cnt >= h1_max_headers {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "chunk trailers count overflow",
                    )));
                }
                *trailers_cnt += 1;

                put_u8!(
                    trailers_buf.as_mut().expect("trailers_buf is None"),
                    byte,
                    h1_max_header_size
                );

                Poll::Ready(Ok(ChunkedState::EndCr))
            }
            _ => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid trailer end LF",
            ))),
        }
    }

    fn read_end_cr<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        trailers_buf: &mut Option<BytesMut>,
        h1_max_header_size: usize,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        let byte = byte!(rdr, cx);
        match byte {
            b'\r' => {
                if let Some(trailers_buf) = trailers_buf {
                    put_u8!(trailers_buf, byte, h1_max_header_size);
                }
                Poll::Ready(Ok(ChunkedState::EndLf))
            }
            byte => {
                match trailers_buf {
                    None => {
                        // 64 will fit a single Expires header without reallocating
                        let mut buf = BytesMut::with_capacity(64);
                        buf.put_u8(byte);
                        *trailers_buf = Some(buf);
                    }
                    Some(trailers_buf) => {
                        put_u8!(trailers_buf, byte, h1_max_header_size);
                    }
                }

                Poll::Ready(Ok(ChunkedState::Trailer))
            }
        }
    }
    fn read_end_lf<R: MemRead>(
        cx: &mut Context<'_>,
        rdr: &mut R,
        trailers_buf: &mut Option<BytesMut>,
        h1_max_header_size: usize,
    ) -> Poll<Result<ChunkedState, io::Error>> {
        let byte = byte!(rdr, cx);
        match byte {
            b'\n' => {
                if let Some(trailers_buf) = trailers_buf {
                    put_u8!(trailers_buf, byte, h1_max_header_size);
                }
                Poll::Ready(Ok(ChunkedState::End))
            }
            _ => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid chunk end LF",
            ))),
        }
    }
}

// TODO: disallow Transfer-Encoding, Content-Length, Trailer, etc in trailers ??
fn decode_trailers(buf: &mut BytesMut, count: usize) -> Result<HeaderMap, io::Error> {
    let mut trailers = HeaderMap::new();
    let mut headers = vec![httparse::EMPTY_HEADER; count];
    let res = httparse::parse_headers(buf, &mut headers);
    match res {
        Ok(httparse::Status::Complete((_, headers))) => {
            for header in headers {
                use std::convert::TryFrom;
                let name = match HeaderName::try_from(header.name) {
                    Ok(name) => name,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Invalid header name: {header:?}"),
                        ));
                    }
                };

                let value = match HeaderValue::from_bytes(header.value) {
                    Ok(value) => value,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Invalid header value: {header:?}"),
                        ));
                    }
                };

                trailers.append(name, value);
            }

            Ok(trailers)
        }
        Ok(httparse::Status::Partial) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Partial header",
        )),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
    }
}

#[derive(Debug)]
struct IncompleteBody;

impl fmt::Display for IncompleteBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "end of file before message length reached")
    }
}

impl StdError for IncompleteBody {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::{Read, ReadBuf};
    use std::pin::Pin;
    use std::time::Duration;

    impl MemRead for &[u8] {
        fn read_mem(&mut self, _: &mut Context<'_>, len: usize) -> Poll<io::Result<Bytes>> {
            let n = std::cmp::min(len, self.len());
            if n > 0 {
                let (a, b) = self.split_at(n);
                let buf = Bytes::copy_from_slice(a);
                *self = b;
                Poll::Ready(Ok(buf))
            } else {
                Poll::Ready(Ok(Bytes::new()))
            }
        }
    }

    impl MemRead for &mut (dyn Read + Unpin) {
        fn read_mem(&mut self, cx: &mut Context<'_>, len: usize) -> Poll<io::Result<Bytes>> {
            let mut v = vec![0; len];
            let mut buf = ReadBuf::new(&mut v);
            ready!(Pin::new(self).poll_read(cx, buf.unfilled())?);
            Poll::Ready(Ok(Bytes::copy_from_slice(buf.filled())))
        }
    }

    impl MemRead for Bytes {
        fn read_mem(&mut self, _: &mut Context<'_>, len: usize) -> Poll<io::Result<Bytes>> {
            let n = std::cmp::min(len, self.len());
            let ret = self.split_to(n);
            Poll::Ready(Ok(ret))
        }
    }

    /*
    use std::io;
    use std::io::Write;
    use super::Decoder;
    use super::ChunkedState;
    use futures::{Async, Poll};
    use bytes::{BytesMut, Bytes};
    use crate::mock::AsyncIo;
    */

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_chunk_size() {
        use std::io::ErrorKind::{InvalidData, InvalidInput, UnexpectedEof};

        async fn read(s: &str) -> u64 {
            let mut state = ChunkedState::new();
            let rdr = &mut s.as_bytes();
            let mut size = 0;
            let mut line_len = 0;
            let mut excess = 0;
            let mut trailers_cnt = 0;
            loop {
                let result = futures_util::future::poll_fn(|cx| {
                    state.step(
                        cx,
                        rdr,
                        StepArgs {
                            chunk_size: &mut size,
                            chunk_line_len: &mut line_len,
                            excess: &mut excess,
                            chunk_buf: &mut None,
                            trailers_buf: &mut None,
                            trailers_cnt: &mut trailers_cnt,
                            max_headers_cnt: DEFAULT_MAX_HEADERS,
                            max_headers_bytes: TRAILER_LIMIT,
                        },
                    )
                })
                .await;
                let desc = format!("read_size failed for {:?}", s);
                state = result.expect(&desc);
                if state == ChunkedState::Body || state == ChunkedState::EndCr {
                    break;
                }
            }
            size
        }

        async fn read_err(s: &str, expected_err: io::ErrorKind) {
            let mut state = ChunkedState::new();
            let rdr = &mut s.as_bytes();
            let mut size = 0;
            let mut line_len = 0;
            let mut excess = 0;
            let mut trailers_cnt = 0;
            loop {
                let result = futures_util::future::poll_fn(|cx| {
                    state.step(
                        cx,
                        rdr,
                        StepArgs {
                            chunk_size: &mut size,
                            chunk_line_len: &mut line_len,
                            excess: &mut excess,
                            chunk_buf: &mut None,
                            trailers_buf: &mut None,
                            trailers_cnt: &mut trailers_cnt,
                            max_headers_cnt: DEFAULT_MAX_HEADERS,
                            max_headers_bytes: TRAILER_LIMIT,
                        },
                    )
                })
                .await;
                state = match result {
                    Ok(s) => s,
                    Err(e) => {
                        assert!(
                            expected_err == e.kind(),
                            "Reading {:?}, expected {:?}, but got {:?}",
                            s,
                            expected_err,
                            e.kind()
                        );
                        return;
                    }
                };
                if state == ChunkedState::Body || state == ChunkedState::End {
                    panic!("Was Ok. Expected Err for {:?}", s);
                }
            }
        }

        assert_eq!(1, read("1\r\n").await);
        assert_eq!(1, read("01\r\n").await);
        assert_eq!(0, read("0\r\n").await);
        assert_eq!(0, read("00\r\n").await);
        assert_eq!(10, read("A\r\n").await);
        assert_eq!(10, read("a\r\n").await);
        assert_eq!(255, read("Ff\r\n").await);
        assert_eq!(255, read("Ff   \r\n").await);
        // Missing LF or CRLF
        read_err("F\rF", InvalidInput).await;
        read_err("F", UnexpectedEof).await;
        // Missing digit
        read_err("\r\n\r\n", InvalidInput).await;
        read_err("\r\n", InvalidInput).await;
        // Invalid hex digit
        read_err("X\r\n", InvalidInput).await;
        read_err("1X\r\n", InvalidInput).await;
        read_err("-\r\n", InvalidInput).await;
        read_err("-1\r\n", InvalidInput).await;
        // Acceptable (if not fully valid) extensions do not influence the size
        assert_eq!(1, read("1;extension\r\n").await);
        assert_eq!(10, read("a;ext name=value\r\n").await);
        assert_eq!(1, read("1;extension;extension2\r\n").await);
        assert_eq!(1, read("1;;;  ;\r\n").await);
        assert_eq!(2, read("2; extension...\r\n").await);
        assert_eq!(3, read("3   ; extension=123\r\n").await);
        assert_eq!(3, read("3   ;\r\n").await);
        assert_eq!(3, read("3   ;   \r\n").await);
        // Invalid extensions cause an error
        read_err("1 invalid extension\r\n", InvalidInput).await;
        read_err("1 A\r\n", InvalidInput).await;
        read_err("1;no CRLF", UnexpectedEof).await;
        read_err("1;reject\nnewlines\r\n", InvalidData).await;
        // Overflow
        read_err("f0000000000000003\r\n", InvalidData).await;
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_sized_early_eof() {
        let mut bytes = &b"foo bar"[..];
        let mut decoder = Decoder::length(10);
        assert_eq!(
            decoder
                .decode_fut(&mut bytes)
                .await
                .unwrap()
                .data_ref()
                .unwrap()
                .len(),
            7
        );
        let e = decoder.decode_fut(&mut bytes).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_chunked_early_eof() {
        let mut bytes = &b"\
            9\r\n\
            foo bar\
        "[..];
        let mut decoder = Decoder::chunked(None, None);
        assert_eq!(
            decoder
                .decode_fut(&mut bytes)
                .await
                .unwrap()
                .data_ref()
                .unwrap()
                .len(),
            7
        );
        let e = decoder.decode_fut(&mut bytes).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_chunked_single_read() {
        let mut mock_buf = &b"10\r\n1234567890abcdef\r\n0\r\n"[..];
        let buf = Decoder::chunked(None, None)
            .decode_fut(&mut mock_buf)
            .await
            .expect("decode")
            .into_data()
            .expect("unknown frame type");
        assert_eq!(16, buf.len());
        let result = String::from_utf8(buf.as_ref().to_vec()).expect("decode String");
        assert_eq!("1234567890abcdef", &result);
    }

    #[tokio::test]
    async fn test_read_chunked_with_missing_zero_digit() {
        // After reading a valid chunk, the ending is missing a zero.
        let mut mock_buf = &b"1\r\nZ\r\n\r\n\r\n"[..];
        let mut decoder = Decoder::chunked(None, None);
        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect("decode")
            .into_data()
            .expect("unknown frame type");
        assert_eq!("Z", buf);

        let err = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect_err("decode 2");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn test_read_chunked_extension_line_too_long() {
        // Go rejects a chunk-size line at or above 4096 bytes
        // (`internal/chunked.go:180`). The whole body is one 5000-byte
        // extension line.
        let mut scratch = vec![];
        scratch.extend(b"5;");
        scratch.extend(b"x".repeat(5000));
        scratch.extend(b"\r\nhello\r\n0\r\n\r\n");
        let mut mock_buf = Bytes::from(scratch);

        let mut decoder = Decoder::chunked(None, None);
        let err = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect_err("decode");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "chunk size line too long");
    }

    #[tokio::test]
    async fn test_read_chunked_line_boundary() {
        // 4094 bytes is Go's last accepted chunk line; 4095 fails because
        // the CRLF no longer fits in bufio's 4096-byte buffer
        // (`internal/chunked.go:19,155-181`).
        let mut accepted = vec![];
        accepted.extend(b"5;");
        accepted.extend(b"x".repeat(4092));
        accepted.extend(b"\r\nhello\r\n0\r\n\r\n");
        let mut mock_buf = Bytes::from(accepted);
        let mut decoder = Decoder::chunked(None, None);
        let data = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect("4094-byte line decodes")
            .into_data()
            .expect("data frame");
        assert_eq!(&data[..], b"hello");

        let mut rejected = vec![];
        rejected.extend(b"5;");
        rejected.extend(b"x".repeat(4093));
        rejected.extend(b"\r\nhello\r\n0\r\n\r\n");
        let mut mock_buf = Bytes::from(rejected);
        let mut decoder = Decoder::chunked(None, None);
        let err = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect_err("4095-byte line is too long");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "chunk size line too long");
    }

    #[tokio::test]
    async fn test_read_chunked_too_much_non_data() {
        // Each 4000-byte extension is under the per-line cap, but Go's
        // running overhead budget (16 KiB after crediting 16 + 2n per chunk,
        // `internal/chunked.go:73-80`) rejects the fifth chunk header.
        let mut scratch = vec![];
        for _ in 0..4 {
            scratch.extend(b"1;");
            scratch.extend(b"x".repeat(4000));
            scratch.extend(b"\r\nA\r\n");
        }
        scratch.extend(b"1;");
        scratch.extend(b"x".repeat(4000));
        scratch.extend(b"\r\nA\r\n0\r\n\r\n");
        let mut mock_buf = Bytes::from(scratch);

        let mut decoder = Decoder::chunked(None, None);
        for _ in 0..4 {
            decoder
                .decode_fut(&mut mock_buf)
                .await
                .expect("decode data")
                .into_data()
                .expect("data frame");
        }

        let err = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect_err("decode");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            err.to_string(),
            "chunked encoding contains too much non-data"
        );
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_chunked_trailer_with_missing_lf() {
        let mut mock_buf = &b"10\r\n1234567890abcdef\r\n0\r\nbad\r\r\n"[..];
        let mut decoder = Decoder::chunked(None, None);
        decoder.decode_fut(&mut mock_buf).await.expect("decode");
        let e = decoder.decode_fut(&mut mock_buf).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_chunked_after_eof() {
        let mut mock_buf = &b"10\r\n1234567890abcdef\r\n0\r\n\r\n"[..];
        let mut decoder = Decoder::chunked(None, None);

        // normal read
        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .unwrap()
            .into_data()
            .expect("unknown frame type");
        assert_eq!(16, buf.len());
        let result = String::from_utf8(buf.as_ref().to_vec()).expect("decode String");
        assert_eq!("1234567890abcdef", &result);

        // eof read
        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect("decode")
            .into_data()
            .expect("unknown frame type");
        assert_eq!(0, buf.len());

        // ensure read after eof also returns eof
        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect("decode")
            .into_data()
            .expect("unknown frame type");
        assert_eq!(0, buf.len());
    }

    // perform an async read using a custom buffer size and causing a blocking
    // read at the specified byte
    async fn read_async(mut decoder: Decoder, content: &[u8], block_at: usize) -> String {
        let mut outs = Vec::new();

        let mut ins = crate::common::io::Compat::new(if block_at == 0 {
            tokio_test::io::Builder::new()
                .wait(Duration::from_millis(10))
                .read(content)
                .build()
        } else {
            tokio_test::io::Builder::new()
                .read(&content[..block_at])
                .wait(Duration::from_millis(10))
                .read(&content[block_at..])
                .build()
        });

        let mut ins = &mut ins as &mut (dyn Read + Unpin);

        loop {
            let buf = decoder
                .decode_fut(&mut ins)
                .await
                .expect("unexpected decode error")
                .into_data()
                .expect("unexpected frame type");
            if buf.is_empty() {
                break; // eof
            }
            outs.extend(buf.as_ref());
        }

        String::from_utf8(outs).expect("decode String")
    }

    // iterate over the different ways that this async read could go.
    // tests blocking a read at each byte along the content - The shotgun approach
    async fn all_async_cases(content: &str, expected: &str, decoder: Decoder) {
        let content_len = content.len();
        for block_at in 0..content_len {
            let actual = read_async(decoder.clone(), content.as_bytes(), block_at).await;
            assert_eq!(expected, &actual) //, "Failed async. Blocking at {}", block_at);
        }
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_length_async() {
        let content = "foobar";
        all_async_cases(content, content, Decoder::length(content.len() as u64)).await;
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_chunked_async() {
        let content = "3\r\nfoo\r\n3\r\nbar\r\n0\r\n\r\n";
        let expected = "foobar";
        all_async_cases(content, expected, Decoder::chunked(None, None)).await;
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn test_read_eof_async() {
        let content = "foobar";
        all_async_cases(content, content, Decoder::eof()).await;
    }

    #[cfg(all(feature = "nightly", not(miri)))]
    #[bench]
    fn bench_decode_chunked_1kb(b: &mut test::Bencher) {
        let rt = new_runtime();

        const LEN: usize = 1024;
        let mut vec = Vec::new();
        vec.extend(format!("{:x}\r\n", LEN).as_bytes());
        vec.extend(&[0; LEN][..]);
        vec.extend(b"\r\n");
        let content = Bytes::from(vec);

        b.bytes = LEN as u64;

        b.iter(|| {
            let mut decoder = Decoder::chunked(None, None);
            rt.block_on(async {
                let mut raw = content.clone();
                let chunk = decoder
                    .decode_fut(&mut raw)
                    .await
                    .unwrap()
                    .into_data()
                    .unwrap();
                assert_eq!(chunk.len(), LEN);
            });
        });
    }

    #[cfg(all(feature = "nightly", not(miri)))]
    #[bench]
    fn bench_decode_length_1kb(b: &mut test::Bencher) {
        let rt = new_runtime();

        const LEN: usize = 1024;
        let content = Bytes::from(&[0; LEN][..]);
        b.bytes = LEN as u64;

        b.iter(|| {
            let mut decoder = Decoder::length(LEN as u64);
            rt.block_on(async {
                let mut raw = content.clone();
                let chunk = decoder
                    .decode_fut(&mut raw)
                    .await
                    .unwrap()
                    .into_data()
                    .unwrap();
                assert_eq!(chunk.len(), LEN);
            });
        });
    }

    #[cfg(feature = "nightly")]
    fn new_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt build")
    }

    #[test]
    fn test_decode_trailers() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(
            b"Expires: Wed, 21 Oct 2015 07:28:00 GMT\r\nX-Stream-Error: failed to decode\r\n\r\n",
        );
        let headers = decode_trailers(&mut buf, 2).expect("decode_trailers");
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers.get("Expires").unwrap(),
            "Wed, 21 Oct 2015 07:28:00 GMT"
        );
        assert_eq!(headers.get("X-Stream-Error").unwrap(), "failed to decode");
    }

    #[test]
    fn test_decode_trailers_preserves_duplicate_values() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(b"X-Trace: first\r\nX-Trace: second\r\n\r\n");

        let headers = decode_trailers(&mut buf, 2).expect("decode_trailers");
        let values = headers
            .get_all("X-Trace")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(values, ["first", "second"]);
    }

    #[tokio::test]
    async fn test_trailer_max_headers_enforced() {
        let h1_max_headers = 10;
        let mut scratch = vec![];
        scratch.extend(b"10\r\n1234567890abcdef\r\n0\r\n");
        for i in 0..=h1_max_headers {
            scratch.extend(format!("trailer{}: {}\r\n", i, i).as_bytes());
        }
        scratch.extend(b"\r\n");
        let mut mock_buf = Bytes::from(scratch);

        let mut decoder = Decoder::chunked(Some(h1_max_headers), None);

        // ready chunked body
        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .unwrap()
            .into_data()
            .expect("unknown frame type");
        assert_eq!(16, buf.len());

        // eof read
        let err = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect_err("trailer fields over limit");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_trailer_max_headers_allows_exact_limit() {
        let h1_max_headers = 10;
        let mut scratch = vec![];
        scratch.extend(b"10\r\n1234567890abcdef\r\n0\r\n");
        for i in 0..h1_max_headers {
            scratch.extend(format!("trailer{}: {}\r\n", i, i).as_bytes());
        }
        scratch.extend(b"\r\n");
        let mut mock_buf = Bytes::from(scratch);

        let mut decoder = Decoder::chunked(Some(h1_max_headers), None);

        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .unwrap()
            .into_data()
            .expect("unknown frame type");
        assert_eq!(16, buf.len());

        let trailers = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect("trailers at limit")
            .into_trailers()
            .expect("unknown frame type");
        assert_eq!(trailers.len(), h1_max_headers);
    }

    #[tokio::test]
    async fn test_trailer_max_header_size_huge_trailer() {
        let max_header_size = 1024;
        let mut scratch = vec![];
        scratch.extend(b"10\r\n1234567890abcdef\r\n0\r\n");
        scratch.extend(format!("huge_trailer: {}\r\n", "x".repeat(max_header_size)).as_bytes());
        scratch.extend(b"\r\n");
        let mut mock_buf = Bytes::from(scratch);

        let mut decoder = Decoder::chunked(None, Some(max_header_size));

        // ready chunked body
        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .unwrap()
            .into_data()
            .expect("unknown frame type");
        assert_eq!(16, buf.len());

        // eof read
        let err = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect_err("trailers over limit");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_trailer_max_header_size_many_small_trailers() {
        let max_headers = 10;
        let header_size = 64;
        let mut scratch = vec![];
        scratch.extend(b"10\r\n1234567890abcdef\r\n0\r\n");

        for i in 0..max_headers {
            scratch.extend(format!("trailer{}: {}\r\n", i, "x".repeat(header_size)).as_bytes());
        }

        scratch.extend(b"\r\n");
        let mut mock_buf = Bytes::from(scratch);

        let mut decoder = Decoder::chunked(None, Some(max_headers * header_size));

        // ready chunked body
        let buf = decoder
            .decode_fut(&mut mock_buf)
            .await
            .unwrap()
            .into_data()
            .expect("unknown frame type");
        assert_eq!(16, buf.len());

        // eof read
        let err = decoder
            .decode_fut(&mut mock_buf)
            .await
            .expect_err("trailers over limit");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
