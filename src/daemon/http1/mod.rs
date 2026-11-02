//! Go `net/http`-compatible HTTP/1.1 serving.
//!
//! Go envd 0.10.2 serves with `net/http.Server`, which answers malformed
//! requests with its own dateless `400`/`431`/`501`/`505` responses. Hyper's
//! parser answers them differently, so this module puts a validation guard
//! between the socket and hyper:
//!
//! * [`GuardedStream`] parses every request head with the Go rules in
//!   [`head`], withholds bytes until the head is accepted, and tracks the body
//!   framing so it can find the next head. Rejected heads are never forwarded.
//! * [`serve`] accepts connections, runs hyper on the guard, and — after
//!   hyper's connection future ends — writes Go's exact error bytes on a
//!   cloned socket for the connection's first rejection.
//!
//! Everything hyper accepts through the guard is a request Go would also hand
//! to its handler, so the two servers agree on accept/reject and on the
//! framing of each body.

mod head;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tower::ServiceExt as _;

use head::{Framing, Reject};

/// Bytes the guard reads from the socket per wakeup. Heads are withheld until
/// complete, so this only bounds peak memory while one is arriving.
const READ_CHUNK: usize = 16 * 1024;

/// Serve `app` on `listener` with Go's HTTP/1 rejection semantics.
///
/// Equivalent to `axum::serve(listener, app).with_graceful_shutdown(shutdown)`
/// except that malformed requests are answered with Go's exact bytes. On
/// shutdown, active connections finish their in-flight request and idle ones
/// close.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    tokio::pin!(shutdown);
    let (stop, _) = watch::channel(false);
    let mut connections: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = accept(&listener) => {
                let (stream, _) = accepted?;
                // Go's net package enables TCP_NODELAY on accepted sockets.
                let _ = stream.set_nodelay(true);
                let shared = Arc::new(Mutex::new(None));
                connections.spawn(serve_connection(
                    stream,
                    app.clone(),
                    stop.subscribe(),
                    shared,
                ));
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    let _ = stop.send(true);
    while connections.join_next().await.is_some() {}
    Ok(())
}

/// `axum::serve`'s listener behaviour: retry transient accept errors with a
/// small backoff instead of tearing the whole server down.
async fn accept(listener: &TcpListener) -> io::Result<(TcpStream, SocketAddr)> {
    let mut backoff = Duration::from_millis(1);
    loop {
        match listener.accept().await {
            Ok(accepted) => return Ok(accepted),
            Err(error) => {
                if backoff > Duration::from_millis(64) {
                    return Err(error);
                }
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
        }
    }
}

async fn serve_connection(
    stream: TcpStream,
    app: Router,
    mut shutdown: watch::Receiver<bool>,
    shared: Arc<Mutex<Option<Reject>>>,
) {
    // Duplicate the descriptor before handing the stream to hyper so the
    // rejection response can still be written after the connection ends.
    // `into_std` keeps non-blocking mode, which the dup shares.
    let std_stream = match stream.into_std() {
        Ok(std_stream) => std_stream,
        Err(_) => return,
    };
    let writer = match std_stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let (stream, writer) = match (TcpStream::from_std(std_stream), TcpStream::from_std(writer)) {
        (Ok(stream), Ok(writer)) => (stream, writer),
        _ => return,
    };
    let mut writer = writer;
    {
        let io = TokioIo::new(GuardedStream::new(stream, shared.clone()));
        let mut builder = http1::Builder::new();
        builder
            // Go allows 500 header values and 1 MiB of head; hyper defaults
            // to 100 values and would answer 431 on its own.
            .max_headers(head::MAX_HEADER_VALUES)
            .max_header_size(head::MAX_HEAD_BYTES)
            .max_buf_size(head::MAX_HEAD_BYTES + READ_CHUNK)
            // Go keeps answering a request whose client half-closed the
            // write side; hyper's default aborts the response instead.
            .half_close(true)
            // Go envd sets no ReadHeaderTimeout; hyper defaults to 30s.
            .header_read_timeout(None);
        let service = service_fn(move |request| {
            let app = app.clone();
            async move { app.oneshot(request).await }
        });
        let mut connection = std::pin::pin!(builder.serve_connection(io, service));
        let mut closing = false;
        loop {
            tokio::select! {
                result = connection.as_mut() => {
                    let _ = result;
                    break;
                }
                changed = shutdown.changed(), if !closing => {
                    if changed.is_err() {
                        break;
                    }
                    closing = true;
                    connection.as_mut().graceful_shutdown();
                }
            }
        }
        // Dropping the connection drops the guard, which closes the original
        // descriptor; `poll_shutdown` was held back when a rejection is
        // pending so the socket is still writable here.
    }
    let reject = shared.lock().expect("guard lock").take();
    if let Some(reject) = reject {
        let _ = writer.write_all(reject.response()).await;
        let _ = writer.shutdown().await;
    }
}

/// How the guard is reading the connection.
#[derive(Clone, Copy)]
enum Mode {
    /// The next request head starts at `scan`.
    Head,
    /// A Content-Length body has this many bytes left.
    Body { remaining: u64 },
    /// A chunked body is in progress.
    Chunk { state: ChunkState },
    /// A CONNECT request was answered; never parse another head.
    Halt,
    /// The body framing is unparseable; forward the rest without inspection.
    Pass,
}

/// Incremental chunked-body scanner state.
#[derive(Clone, Copy)]
enum ChunkState {
    Size,
    Data(u64),
    DataEnd,
    Trailers,
}

enum ChunkProgress {
    NeedMore,
    Done,
    Broken,
}

/// An `AsyncRead`/`AsyncWrite` wrapper that only lets hyper see request bytes
/// Go's `net/http` would accept.
struct GuardedStream<S> {
    inner: S,
    shared: Arc<Mutex<Option<Reject>>>,
    /// Bytes read from the socket that have not been discarded yet.
    buf: Vec<u8>,
    /// Next byte to inspect: everything below it has been validated.
    scan: usize,
    /// Bytes `[0, deliverable)` are safe to hand to hyper.
    deliverable: usize,
    /// Bytes `[0, delivered)` were already handed to hyper.
    delivered: usize,
    mode: Mode,
    /// Go discards up to four leading CR/LF bytes after a POST request.
    skip_crlf: bool,
    /// A request was already served, so Go waits for four buffered bytes
    /// before attempting the next head (`server.go:2193`). A connection that
    /// ends with fewer is closed without a reply.
    idle_gate: bool,
    /// The first rejection found on this connection.
    reject: Option<Reject>,
    /// The guard gave hyper (or will give it) end-of-file.
    closed: bool,
}

impl<S> GuardedStream<S> {
    fn new(inner: S, shared: Arc<Mutex<Option<Reject>>>) -> Self {
        Self {
            inner,
            shared,
            buf: Vec::new(),
            scan: 0,
            deliverable: 0,
            delivered: 0,
            mode: Mode::Head,
            skip_crlf: false,
            idle_gate: false,
            reject: None,
            closed: false,
        }
    }

    /// Record a rejection and stop reading. Bytes already validated are still
    /// delivered first; hyper then sees end-of-file instead of the bad head.
    fn reject(&mut self, reason: Reject) {
        if self.reject.is_none() {
            self.reject = Some(reason);
            *self.shared.lock().expect("guard lock") = Some(reason);
        }
        self.closed = true;
    }

    /// Drop bytes that were both validated and delivered to hyper, keeping the
    /// scan positions in step.
    fn compact(&mut self) {
        if self.delivered == 0 {
            return;
        }
        self.buf.drain(..self.delivered);
        self.scan = self.scan.saturating_sub(self.delivered);
        self.deliverable -= self.delivered;
        self.delivered = 0;
    }

    /// Consume as much of `buf` as the current framing allows, extending
    /// `deliverable` over the parts that are safe for hyper.
    fn advance(&mut self) {
        loop {
            match self.mode {
                Mode::Head => {
                    // After the first request, Go's serve loop peeks four
                    // bytes before reading the next head. A short tail at EOF
                    // is therefore dropped silently rather than parsed.
                    if self.idle_gate && self.buf.len() - self.scan < 4 {
                        break;
                    }
                    let parsed = head::parse(&self.buf[self.scan..], self.skip_crlf);
                    match parsed {
                        Ok(Some(head)) => {
                            self.scan += head.len;
                            self.deliverable = self.scan;
                            self.skip_crlf = head.method_post;
                            self.idle_gate = true;
                            self.mode = match head.framing {
                                Framing::Empty => Mode::Head,
                                Framing::Length(remaining) => Mode::Body { remaining },
                                Framing::Chunked => Mode::Chunk {
                                    state: ChunkState::Size,
                                },
                                Framing::Close => Mode::Halt,
                            };
                        }
                        Ok(None) => break,
                        Err(reason) => {
                            self.reject(reason);
                            break;
                        }
                    }
                }
                Mode::Body { mut remaining } => {
                    let available = (self.buf.len() - self.scan) as u64;
                    let take = available.min(remaining);
                    self.scan += take as usize;
                    self.deliverable = self.scan;
                    remaining -= take;
                    self.mode = if remaining == 0 {
                        Mode::Head
                    } else {
                        Mode::Body { remaining }
                    };
                    // All buffered bytes were body; no head is in progress.
                    break;
                }
                Mode::Chunk { state } => {
                    let mut pos = self.scan;
                    let mut state = state;
                    let progress = scan_chunked(&self.buf, &mut pos, &mut state);
                    self.scan = pos;
                    self.deliverable = pos;
                    match progress {
                        ChunkProgress::NeedMore => {
                            self.mode = Mode::Chunk { state };
                            break;
                        }
                        ChunkProgress::Done => {
                            self.mode = Mode::Head;
                        }
                        ChunkProgress::Broken => {
                            self.mode = Mode::Pass;
                        }
                    }
                }
                Mode::Halt => {
                    // Go never reuses a connection after CONNECT, so whatever
                    // follows the head is not another request. Withhold it
                    // instead of letting hyper parse it, and report EOF once
                    // the validated head has been delivered.
                    self.closed = true;
                    break;
                }
                Mode::Pass => {
                    self.scan = self.buf.len();
                    self.deliverable = self.scan;
                    break;
                }
            }
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for GuardedStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.delivered < this.deliverable {
                if out.remaining() == 0 {
                    return Poll::Ready(Ok(()));
                }
                let end = this.deliverable.min(this.buf.len());
                let take = (end - this.delivered).min(out.remaining());
                out.put_slice(&this.buf[this.delivered..this.delivered + take]);
                this.delivered += take;
                return Poll::Ready(Ok(()));
            }
            if this.closed {
                return Poll::Ready(Ok(()));
            }
            let mut chunk = [0u8; READ_CHUNK];
            let mut read = ReadBuf::new(&mut chunk);
            match Pin::new(&mut this.inner).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {}
            }
            let filled = read.filled().len();
            if filled == 0 {
                let pending = this.buf.len() - this.scan;
                // Go peeks four bytes before the next head; fewer at EOF is
                // a silent close, not a request to parse.
                let gated_out = this.idle_gate && pending < 4;
                if matches!(this.mode, Mode::Head)
                    && !gated_out
                    && head::eof_gets_reply(&this.buf[this.scan..], this.skip_crlf)
                {
                    this.reject(Reject::BadRequest);
                } else {
                    this.closed = true;
                }
                continue;
            }
            this.compact();
            this.buf.extend_from_slice(&read.filled()[..filled]);
            this.advance();
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for GuardedStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // Go writes its rejection after the previous response. Keep the socket
        // writable so the connection task can do the same once hyper is done.
        if this.reject.is_some() {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// Walk chunked framing from `*pos` until the next head begins or bytes run
/// out. `*pos` always marks how far the framing is unambiguous.
fn scan_chunked(buf: &[u8], pos: &mut usize, state: &mut ChunkState) -> ChunkProgress {
    loop {
        match *state {
            ChunkState::Size => {
                let Some(rel) = buf[*pos..].iter().position(|&b| b == b'\n') else {
                    return ChunkProgress::NeedMore;
                };
                let line = &buf[*pos..*pos + rel];
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let digits = line.iter().take_while(|b| b.is_ascii_hexdigit()).count();
                if digits == 0 {
                    return ChunkProgress::Broken;
                }
                let mut size = 0u64;
                for &b in &line[..digits] {
                    let digit = (b as char).to_digit(16).expect("hex digit") as u64;
                    size = match size.checked_mul(16).and_then(|v| v.checked_add(digit)) {
                        Some(size) => size,
                        None => return ChunkProgress::Broken,
                    };
                }
                if digits < line.len() && line[digits] != b';' {
                    return ChunkProgress::Broken;
                }
                *pos += rel + 1;
                *state = if size == 0 {
                    ChunkState::Trailers
                } else {
                    ChunkState::Data(size)
                };
            }
            ChunkState::Data(remaining) => {
                let available = (buf.len() - *pos) as u64;
                if available < remaining {
                    *pos = buf.len();
                    *state = ChunkState::Data(remaining - available);
                    return ChunkProgress::NeedMore;
                }
                *pos += remaining as usize;
                *state = ChunkState::DataEnd;
            }
            ChunkState::DataEnd => {
                match buf.get(*pos..) {
                    Some([b'\n', ..]) => *pos += 1,
                    Some([b'\r', b'\n', ..]) => *pos += 2,
                    Some([_, ..]) => return ChunkProgress::Broken,
                    _ => return ChunkProgress::NeedMore,
                }
                *state = ChunkState::Size;
            }
            ChunkState::Trailers => {
                let Some(rel) = buf[*pos..].iter().position(|&b| b == b'\n') else {
                    return ChunkProgress::NeedMore;
                };
                let line = &buf[*pos..*pos + rel];
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                *pos += rel + 1;
                if line.is_empty() {
                    return ChunkProgress::Done;
                }
                let Some(colon) = line.iter().position(|&b| b == b':') else {
                    return ChunkProgress::Broken;
                };
                let name_valid = !line[..colon].is_empty()
                    && line[..colon].iter().all(|&b| {
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
                    });
                let value_valid = line[colon + 1..]
                    .iter()
                    .all(|&b| b == b'\t' || (0x20..=0x7e).contains(&b) || b >= 0x80);
                if !name_valid || !value_valid {
                    return ChunkProgress::Broken;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn run(input: &[u8]) -> (Vec<u8>, Option<Reject>) {
        let (mut client, server) = tokio::io::duplex(64);
        let shared = Arc::new(Mutex::new(None));
        let mut guard = GuardedStream::new(server, shared.clone());
        let input = input.to_vec();
        let writer = tokio::spawn(async move {
            client.write_all(&input).await.expect("write");
            client.shutdown().await.expect("shutdown");
        });
        let mut out = Vec::new();
        guard.read_to_end(&mut out).await.expect("read");
        writer.await.expect("writer task");
        let reject = *shared.lock().expect("lock");
        (out, reject)
    }

    #[tokio::test]
    async fn forwards_complete_requests() {
        let raw = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\nGET /envs HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, raw);
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn forwards_the_body_of_a_fixed_length_request() {
        let raw = b"POST /init HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\n\r\n{}{}GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, raw);
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn forwards_a_chunked_body_then_the_next_head() {
        let raw = b"POST /init HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, raw);
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn withholds_a_bad_head_and_keeps_the_previous_request() {
        let raw = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\nBAD REQUEST LINE\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(reject, Some(Reject::BadRequest));
    }

    #[tokio::test]
    async fn withholds_a_bad_first_head_entirely() {
        let raw = b"GET / HTTP/1.1\r\nHost: x\r\nJunk Header Without Colon\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert!(out.is_empty());
        assert_eq!(reject, Some(Reject::BadRequest));
    }

    #[tokio::test]
    async fn eof_mid_request_line_answers_400() {
        // `bufio.ReadLine` returns a non-empty partial line at EOF as a
        // complete line, so Go's parser rejects it like any malformed line.
        let (out, reject) = run(b"GE").await;
        assert!(out.is_empty());
        assert_eq!(reject, Some(Reject::BadRequest));
    }

    #[tokio::test]
    async fn eof_on_an_empty_connection_is_silent() {
        let (out, reject) = run(b"").await;
        assert!(out.is_empty());
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn eof_mid_header_block_answers_400() {
        let (out, reject) = run(b"GET / HTTP/1.1\r\nHost: x").await;
        assert!(out.is_empty());
        assert_eq!(reject, Some(Reject::BadRequest));
    }

    #[tokio::test]
    async fn trailing_partial_request_line_answers_400() {
        // The corpus' common shape: a framed request, then enough bytes for
        // Go's four-byte peek to attempt another request line.
        let head = b"GET /health HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\n";
        let raw = [head.as_slice(), b"=r2", b"\x96\xdf?\x01"].concat();
        let (out, reject) = run(&raw).await;
        assert_eq!(out, [head.as_slice(), b"=r2"].concat());
        assert_eq!(reject, Some(Reject::BadRequest));
    }

    #[tokio::test]
    async fn a_short_tail_after_a_request_closes_silently() {
        // Go peeks four bytes before the next head; a shorter tail at EOF is
        // dropped without a reply (`server.go:2193`).
        let head = b"GET /health HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\n";
        let raw = [head.as_slice(), b"=r2", b"\xea\xfe"].concat();
        let (out, reject) = run(&raw).await;
        assert_eq!(out, [head.as_slice(), b"=r2"].concat());
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn connect_withholds_everything_after_the_head() {
        // Go never reuses a connection after CONNECT; the trailing bytes are
        // not a request and must not reach hyper's parser.
        let head = b"CONNECT /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut raw = head.to_vec();
        raw.extend_from_slice(b"\xdd\xb2\xdd\xf8tail");
        let (out, reject) = run(&raw).await;
        assert_eq!(out, head);
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn eof_after_a_complete_request_is_silent() {
        let raw = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, raw);
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn post_tolerates_leading_crlf_on_the_next_request() {
        let raw = b"POST /init HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, raw);
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn chunk_framing_stops_at_trailers() {
        let raw = b"POST /init HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX-Trailer: y\r\n\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, raw);
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn malformed_chunk_framing_falls_back_to_pass_through() {
        let raw = b"POST /init HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(out, raw);
        assert_eq!(reject, None);
    }

    /// Spin a real listener + hyper, then exchange raw bytes with it.
    async fn exchange_roundtrip(payload: &[u8]) -> Vec<u8> {
        use axum::routing::get;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let app = Router::new().route("/health", get(|| async { "ok" }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            serve(listener, app, async move {
                let _ = stopped.await;
            })
            .await
        });

        let mut client = TcpStream::connect(address).await.expect("connect");
        client.write_all(payload).await.expect("write");
        client.shutdown().await.expect("shutdown");
        let mut out = Vec::new();
        client.read_to_end(&mut out).await.expect("read");
        stop.send(()).expect("stop");
        server.await.expect("join").expect("serve");
        out
    }

    #[tokio::test]
    async fn a_valid_request_is_served_normally() {
        let out = exchange_roundtrip(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(out.starts_with(b"HTTP/1.1 200 OK\r\n"), "got {:?}", out);
        assert!(
            out.ends_with(b"\r\n\r\nok") || out.ends_with(b"ok"),
            "got {:?}",
            out
        );
    }

    #[tokio::test]
    async fn a_bad_first_head_gets_go_bytes() {
        let out = exchange_roundtrip(b"GET /health HTTP/1.1\r\nJunk Header\r\n\r\n").await;
        assert_eq!(out, Reject::BadRequest.response());
    }

    #[tokio::test]
    async fn a_bad_head_after_a_valid_one_gets_go_bytes() {
        let out = exchange_roundtrip(
            b"GET /health HTTP/1.1\r\nHost: x\r\n\r\nGET /health HTTP/1.1\r\nJunk Header\r\n\r\n",
        )
        .await;
        assert!(out.starts_with(b"HTTP/1.1 200 OK\r\n"), "got {:?}", out);
        assert!(
            out.ends_with(Reject::BadRequest.response()),
            "got {:?}",
            out
        );
    }

    #[tokio::test]
    async fn trailing_partial_line_after_a_valid_request_gets_go_bytes() {
        let payload =
            b"GET /health HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nr2\x96\xdf?\x01AB";
        let out = exchange_roundtrip(payload).await;
        assert!(out.starts_with(b"HTTP/1.1 200 OK\r\n"), "got {:?}", out);
        assert!(
            out.ends_with(Reject::BadRequest.response()),
            "got {:?}",
            out
        );
    }
}
