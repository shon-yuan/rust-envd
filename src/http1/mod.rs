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

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::response::Response;
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
    serve_gated(listener, app, shutdown, AcceptGate::always_open()).await
}

/// Controls whether [`serve_gated`] accepts new connections. In-flight
/// connections are never touched: closing the gate pauses the accept loop and
/// keeps the listener and every established connection alive, so the handover
/// can stop accepting from a shared backlog without dropping clients that are
/// already being served.
pub struct AcceptGate {
    open: watch::Receiver<bool>,
}

impl AcceptGate {
    /// A gate that is never closed (the Linux behaviour).
    pub fn always_open() -> Self {
        let (send, open) = watch::channel(true);
        drop(send);
        Self { open }
    }

    /// A gate the caller opens and closes through the returned sender. The
    /// initial state applies until the first send.
    pub fn channel(initial: bool) -> (watch::Sender<bool>, Self) {
        let (send, open) = watch::channel(initial);
        (send, Self { open })
    }

    pub fn is_open(&self) -> bool {
        *self.open.borrow()
    }
}

/// [`serve`] with an accept gate. Linux passes an always-open gate, so its
/// behaviour is unchanged; a Windows handover closes the gate when the
/// successor starts serving from the inherited listener.
pub async fn serve_gated(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
    gate: AcceptGate,
) -> io::Result<()> {
    tokio::pin!(shutdown);
    let (stop, _) = watch::channel(false);
    let mut connections: JoinSet<()> = JoinSet::new();
    // A clone for the change future keeps the gate readable in its branch.
    let mut observer = gate.open.clone();
    let mut accepting = gate.is_open();
    let mut gate_watched = true;
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = accept(&listener), if accepting => {
                let (stream, _) = accepted?;
                configure_accepted(&stream);
                let shared = Arc::new(Mutex::new(None));
                connections.spawn(serve_connection(
                    stream,
                    app.clone(),
                    stop.subscribe(),
                    shared,
                ));
            }
            _ = observer.changed(), if gate_watched => {
                accepting = gate.is_open();
                // A dropped sender means no further transitions are coming;
                // stop polling rather than spinning on the closed channel.
                gate_watched = observer.has_changed().is_ok();
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    let _ = stop.send(true);
    while connections.join_next().await.is_some() {}
    Ok(())
}

/// Go's `net` defaults for an accepted TCP connection: `SetNoDelay(true)`
/// (`net.TCPConn` enables it by default), keep-alive on, and
/// `SetKeepAlivePeriod(15s)`, which on Linux sets both `TCP_KEEPIDLE` and
/// `TCP_KEEPINTVL` to 15s; the probe count stays at Linux's default of 9.
fn configure_accepted(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let socket = socket2::SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(15))
        .with_interval(Duration::from_secs(15))
        .with_retries(9);
    let _ = socket.set_tcp_keepalive(&keepalive);
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
            // Go's only bound is `MaxHeaderBytes`; it has no header-value
            // count limit (Go 1.26.10). Hyper's default of 100 would answer
            // 431 on its own, so raise its count bound to the same byte
            // budget the guard enforces.
            .max_headers(head::MAX_HEAD_BYTES)
            .max_header_size(head::MAX_HEAD_BYTES)
            .max_buf_size(head::MAX_HEAD_BYTES + READ_CHUNK)
            // Go keeps answering a request whose client half-closed the
            // write side; hyper's default aborts the response instead.
            .half_close(true)
            // Go envd sets no ReadHeaderTimeout; hyper defaults to 30s.
            .header_read_timeout(None);
        let service = service_fn(move |request| {
            let app = app.clone();
            async move { respond(app, request).await }
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
        if reject == Reject::TooLarge {
            // Go's `closeWriteAndWait` for errTooLarge: half-close, then keep
            // reading for a second so unread request bytes cannot reset the
            // connection and discard the 431 (`server.go:2039`).
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                tokio::io::copy(&mut writer, &mut tokio::io::sink()),
            )
            .await;
        }
    }
}

/// Serve one request through `app`, applying Go's HEAD framing and its
/// post-handler request-body drain.
///
/// `net/http` never sends a response body for HEAD, and it never invents a
/// `Content-Length` for one: the automatic length is suppressed when the
/// method is HEAD and the buffered body is empty (`server.go:68-74`), and HEAD
/// responses are never chunked (`server.go:210`). axum's top-level route
/// future computes `content-length: 0` from the empty body *before* it
/// discards that body for HEAD, so the header has to be removed before hyper
/// writes the response.
///
/// Go also drains up to `maxPostHandlerReadBytes+1` bytes of any request body
/// the handler did not read before it decides whether the response can be
/// followed by another request (`server.go:1393-1435`). A body that cannot be
/// consumed — corrupt chunked framing, a declared length over the limit — sets
/// `closeAfterReply`, and the HTTP/1.1 response carries `Connection: close`
/// (`server.go:1529-1540`). Hyper's own "drain or close" drops the connection
/// silently and never touches the response header, so the drain happens here,
/// before the response future resolves.
async fn respond<B>(app: Router, request: Request<B>) -> Result<Response, Infallible>
where
    B: http_body::Body<Data = axum::body::Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    let is_head = request.method() == Method::HEAD;
    let is_connect = request.method() == Method::CONNECT;
    let request_version = request.version();
    let request_wants_close = request_wants_close(&request);
    let expects_continue = request_expects_continue(&request);
    let (parts, body) = request.into_parts();
    let eof = Arc::new(AtomicBool::new(body.is_end_stream()));
    let slot: Arc<Mutex<Option<Pin<Box<B>>>>> = Arc::new(Mutex::new(None));
    let (slot_tx, drained) = (slot.clone(), eof.clone());
    let request = Request::from_parts(parts, Drainable::new(body, slot, drained));
    let mut response = app.oneshot(request).await?;
    if is_head {
        response.headers_mut().remove(header::CONTENT_LENGTH);
        *response.body_mut() = Body::empty();
    }
    // A CONNECT is never followed by another request on the same connection
    // (`server.go:1372-1375`); for HTTP/1.1 Go also announces the close on
    // every non-2xx response (`server.go:1529-1540`), which is what our
    // unauthorized reply always is.
    if is_connect && !response.status().is_success() {
        insert_close(&mut response, request_version);
    }

    let response_wants_close = response
        .headers()
        .get(header::CONNECTION)
        .is_some_and(|value| header_has_token(value.as_bytes(), b"close"));
    let body_fully_read = eof.load(Ordering::Relaxed);
    let leftover = slot_tx.lock().expect("body slot lock").take();
    let protocol_close = is_connect
        || request_wants_close
        || response_wants_close
        || response.status() == StatusCode::SWITCHING_PROTOCOLS;
    if let Some(body) = leftover {
        // Go skips the drain when the connection is already closing, and it
        // sets `closeAfterReply` without draining when an `Expect:
        // 100-continue` body was never consumed (`server.go:1369-1375`).
        if expects_continue && !body_fully_read {
            insert_close(&mut response, request_version);
        } else if !protocol_close {
            match drain_body(body).await {
                DrainOutcome::Drained => {}
                DrainOutcome::TooLarge | DrainOutcome::Broken => {
                    insert_close(&mut response, request_version)
                }
            }
        }
    }
    Ok(response)
}

/// Go's post-handler read budget (`server.go:1102`).
const MAX_POST_HANDLER_READ_BYTES: usize = 256 * 1024;

/// Add `Connection: close` to a response unless it already asks to close.
///
/// Go's `delConnectionHeader` replaces any existing `Connection` value with
/// the close announcement on HTTP/1.1 (`server.go:1529-1540`). For HTTP/1.0
/// it deletes the header instead and announces nothing, because the version
/// already means "close" (`setHeader.connection = ""`).
fn insert_close(response: &mut Response, request_version: axum::http::Version) {
    if request_version < axum::http::Version::HTTP_11 {
        response.headers_mut().remove(header::CONNECTION);
        return;
    }
    response.headers_mut().insert(
        header::CONNECTION,
        axum::http::HeaderValue::from_static("close"),
    );
}

fn header_has_token(value: &[u8], token: &[u8]) -> bool {
    value
        .split(|&byte| byte == b',')
        .any(|item| trim_ascii(item).eq_ignore_ascii_case(token))
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = value {
        if first.is_ascii_whitespace() {
            value = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = value {
        if last.is_ascii_whitespace() {
            value = rest;
        } else {
            break;
        }
    }
    value
}

/// `shouldClose` for a request Go has already accepted (`request.go:1513`).
fn request_wants_close<B>(request: &Request<B>) -> bool {
    let connection = request.headers().get_all(header::CONNECTION);
    let closes = connection
        .iter()
        .any(|value| header_has_token(value.as_bytes(), b"close"));
    if request.version() == axum::http::Version::HTTP_10 {
        let first_keeps_alive = connection
            .iter()
            .next()
            .is_some_and(|value| header_has_token(value.as_bytes(), b"keep-alive"));
        return closes || !first_keeps_alive;
    }
    closes
}

fn request_expects_continue<B>(request: &Request<B>) -> bool {
    request
        .headers()
        .get(header::EXPECT)
        .is_some_and(|value| header_has_token(value.as_bytes(), b"100-continue"))
}

enum DrainOutcome {
    Drained,
    TooLarge,
    Broken,
}

/// Consume the rest of a request body exactly like Go's post-handler drain.
async fn drain_body<B>(mut body: Pin<Box<B>>) -> DrainOutcome
where
    B: http_body::Body<Data = axum::body::Bytes>,
{
    let mut seen = 0usize;
    loop {
        let frame = std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await;
        match frame {
            None => return DrainOutcome::Drained,
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    seen = seen.saturating_add(data.len());
                    // Go copies at most maxPostHandlerReadBytes+1 bytes; a
                    // body still alive after that is too large to reuse the
                    // connection for (`server.go:1417-1432`).
                    if seen > MAX_POST_HANDLER_READ_BYTES {
                        return DrainOutcome::TooLarge;
                    }
                }
            }
            // A corrupt body must never be parsed as another request
            // (`server.go:1425-1428`).
            Some(Err(_)) => return DrainOutcome::Broken,
        }
    }
}

/// Hands the request body back to [`respond`] when the handler drops it, so
/// the drain can run after the handler and before the response is written.
struct Drainable<B> {
    inner: Option<Pin<Box<B>>>,
    slot: Arc<Mutex<Option<Pin<Box<B>>>>>,
    eof: Arc<AtomicBool>,
}

impl<B> Drainable<B> {
    fn new(body: B, slot: Arc<Mutex<Option<Pin<Box<B>>>>>, eof: Arc<AtomicBool>) -> Self {
        Self {
            inner: Some(Box::pin(body)),
            slot,
            eof,
        }
    }
}

impl<B> http_body::Body for Drainable<B>
where
    B: http_body::Body<Data = axum::body::Bytes>,
{
    type Data = axum::body::Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        let polled = inner.as_mut().poll_frame(cx);
        if matches!(polled, Poll::Ready(None)) {
            this.eof.store(true, Ordering::Relaxed);
            this.inner = None;
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner
            .as_ref()
            .is_none_or(|inner| inner.is_end_stream())
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner
            .as_ref()
            .map_or_else(http_body::SizeHint::default, |inner| inner.size_hint())
    }
}

impl<B> Drop for Drainable<B> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            *self.slot.lock().expect("body slot lock") = Some(inner);
        }
    }
}

/// How the guard is reading the connection.
#[derive(Clone, Copy)]
enum Mode {
    /// The next request head starts at `scan`.
    Head,
    /// A Content-Length body has this many bytes left.
    Body {
        remaining: u64,
        /// Stop parsing once the body is complete (Go closes after replying).
        halt_after: bool,
    },
    /// A chunked body is in progress.
    Chunk { state: ChunkState, halt_after: bool },
    /// Go answers this request and never parses another head on the
    /// connection (CONNECT, `Connection: close`, HTTP/1.0, failed `Expect`,
    /// the HTTP/2 preface).
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

/// Go envd's `idleTimeout` (`main.go:39`): the maximum wait for the first
/// bytes of the next request once a previous response has finished.
const IDLE_TIMEOUT: Duration = Duration::from_secs(640);

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
    /// Deadline for the next request's first bytes, mirroring Go's
    /// `SetReadDeadline(IdleTimeout)` + `Peek(4)` after a response.
    idle_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
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
            idle_deadline: None,
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
                            let start = self.scan;
                            let end = start + head.len;
                            // Go accepts more than hyper's parser does; apply
                            // the delivery rewrites before handing the bytes
                            // over, and account for any length change.
                            let delivered_len = if head.needs_rewrite() {
                                let rewritten = head.rewrite(&self.buf[start..end]);
                                let len = rewritten.len();
                                self.buf.splice(start..end, rewritten).for_each(drop);
                                len
                            } else {
                                head.len
                            };
                            self.scan = start + delivered_len;
                            self.deliverable = self.scan;
                            self.skip_crlf = head.method_post;
                            self.idle_gate = true;
                            self.mode = match head.framing {
                                Framing::Empty if head.halt_after => Mode::Halt,
                                Framing::Empty => Mode::Head,
                                Framing::Length(remaining) => Mode::Body {
                                    remaining,
                                    halt_after: head.halt_after,
                                },
                                Framing::Chunked => Mode::Chunk {
                                    state: ChunkState::Size,
                                    halt_after: head.halt_after,
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
                Mode::Body {
                    mut remaining,
                    halt_after,
                } => {
                    let available = (self.buf.len() - self.scan) as u64;
                    let take = available.min(remaining);
                    self.scan += take as usize;
                    self.deliverable = self.scan;
                    remaining -= take;
                    let done = remaining == 0;
                    self.mode = if done {
                        if halt_after {
                            Mode::Halt
                        } else {
                            Mode::Head
                        }
                    } else {
                        Mode::Body {
                            remaining,
                            halt_after,
                        }
                    };
                    if done && halt_after {
                        // Report EOF from the Halt arm in this same pass.
                        continue;
                    }
                    // All buffered bytes were body; no head is in progress.
                    break;
                }
                Mode::Chunk { state, halt_after } => {
                    let mut pos = self.scan;
                    let mut state = state;
                    let progress = scan_chunked(&self.buf, &mut pos, &mut state);
                    self.scan = pos;
                    self.deliverable = pos;
                    match progress {
                        ChunkProgress::NeedMore => {
                            self.mode = Mode::Chunk { state, halt_after };
                            break;
                        }
                        ChunkProgress::Done => {
                            if halt_after {
                                self.mode = Mode::Halt;
                                continue;
                            }
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
            // Go arms `IdleTimeout` after each response and waits for four
            // bytes of the next request (`server.go:2113-2131`). Fewer than
            // four buffered bytes in head state is that wait.
            let idle =
                this.idle_gate && matches!(this.mode, Mode::Head) && this.buf.len() - this.scan < 4;
            if idle {
                let deadline = this
                    .idle_deadline
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(IDLE_TIMEOUT)));
                if deadline.as_mut().poll(cx).is_ready() {
                    // Go's read deadline expires and `serve` returns without
                    // a reply; hyper sees the same by dropping the stream.
                    this.closed = true;
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "idle timeout",
                    )));
                }
            } else {
                this.idle_deadline = None;
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
        run_with_capacity(input, 64).await
    }

    /// Like [`run`], but with a client conduit large enough to buffer the
    /// whole input: a rejection stops the guard from reading, so a bounded
    /// conduit would leave the writer blocked.
    async fn run_with_capacity(input: &[u8], capacity: usize) -> (Vec<u8>, Option<Reject>) {
        let (mut client, server) = tokio::io::duplex(capacity);
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

    #[tokio::test(start_paused = true)]
    async fn idle_connections_close_after_go_idle_timeout() {
        use tokio::io::AsyncReadExt;

        let (mut client, server) = tokio::io::duplex(64);
        let shared = Arc::new(Mutex::new(None));
        let mut guard = GuardedStream::new(server, shared);
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        let mut out = [0u8; 64];
        let read = guard.read(&mut out).await.expect("read");
        assert_eq!(
            &out[..read],
            b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n".as_slice()
        );

        // The guard now waits for the next request's first bytes, exactly
        // where Go arms `IdleTimeout` (main.go:39, server.go:2113).
        let mut next = [0u8; 64];
        let read = guard.read(&mut next);
        tokio::pin!(read);
        assert!(futures_util::poll!(read.as_mut()).is_pending());
        tokio::time::advance(IDLE_TIMEOUT - Duration::from_secs(1)).await;
        assert!(futures_util::poll!(read.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(2)).await;
        let error = match futures_util::poll!(read.as_mut()) {
            Poll::Ready(result) => result.expect_err("idle connection was not closed"),
            Poll::Pending => panic!("idle connection was not closed at 640s"),
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
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
        // Go deletes Transfer-Encoding from the handler view but still frames
        // the body as chunked; the canonical framing header is kept so hyper
        // does the same.
        assert_eq!(
            out,
            b"POST /init HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n"
        );
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
    async fn a_complete_oversized_head_never_reaches_hyper() {
        // The head is well formed but longer than Go's read limit. Hyper would
        // answer it with its own 431; the guard must withhold every byte so
        // the connection writer can send Go's exact errTooLarge response.
        let mut raw = b"GET /health HTTP/1.1\r\nHost: x\r\nX-Filler: ".to_vec();
        raw.resize(head::MAX_HEAD_BYTES + 1000, b'a');
        raw.extend_from_slice(b"\r\n\r\n");
        let (out, reject) = run_with_capacity(&raw, raw.len() + 1).await;
        assert!(out.is_empty(), "hyper saw the oversized head");
        assert_eq!(reject, Some(Reject::TooLarge));
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
    async fn head_responses_carry_no_content_length() {
        // Go never invents a Content-Length for HEAD (`server.go:68-74`) and
        // never chunks one (`server.go:210`). axum computes one from the
        // empty body before discarding it, so `respond` has to remove it.
        use axum::{http::StatusCode, routing::any};

        let app = Router::new().route("/health", any(|| async { StatusCode::METHOD_NOT_ALLOWED }));
        let request = Request::builder()
            .method(Method::HEAD)
            .uri("/health")
            .body(Body::empty())
            .expect("request");
        let response = respond(app.clone(), request).await.expect("infallible");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());

        // The same response to GET keeps net/http's automatic zero length.
        let request = Request::builder()
            .method(Method::GET)
            .uri("/health")
            .body(Body::empty())
            .expect("request");
        let response = respond(app, request).await.expect("infallible");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "0");
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
        assert_eq!(
            out,
            b"POST /init HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\n\r\n0\r\nX-Trailer: y\r\n\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n"
        );
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn malformed_chunk_framing_falls_back_to_pass_through() {
        let raw = b"POST /init HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(
            out,
            b"POST /init HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\n\r\nzz\r\nGET /health HTTP/1.1\r\nHost: x\r\n\r\n"
        );
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn a_corrupt_unread_chunked_body_announces_the_close() {
        // Go drains up to 256 KiB of an unread request body before writing
        // the response; a corrupt chunked body makes that drain fail, sets
        // closeAfterReply, and the HTTP/1.1 response carries
        // `Connection: close` (`server.go:1393-1435,1529-1540`).
        let out = exchange_roundtrip(
            b"GET /health HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n",
        )
        .await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "got {text:?}");
        assert!(text.contains("connection: close"), "got {text:?}");
    }

    #[tokio::test]
    async fn a_consumed_chunked_body_keeps_the_connection() {
        // A well-formed body is drained without setting closeAfterReply, so
        // the response must not announce a close.
        let out = exchange_roundtrip(
            b"GET /health HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        )
        .await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "got {text:?}");
        assert!(!text.contains("connection: close"), "got {text:?}");
    }

    #[tokio::test]
    async fn connect_responses_announce_the_close() {
        // `net/http` never reuses a CONNECT connection and, for a non-2xx
        // HTTP/1.1 response, writes `Connection: close` (`server.go:1372`,
        // `server.go:1529-1540`).
        use axum::routing::any;
        let app = Router::new().route("/health", any(|| async { StatusCode::UNAUTHORIZED }));
        let request = Request::builder()
            .method(Method::CONNECT)
            .uri("/health")
            .body(Body::empty())
            .expect("request");
        let response = respond(app, request).await.expect("infallible");
        assert_eq!(response.headers()[header::CONNECTION], "close");
    }

    #[tokio::test]
    async fn http10_connect_responses_do_not_announce_the_close() {
        // Go deletes the handler's `Connection` header on HTTP/1.0 and sets
        // no replacement (`server.go:1529-1540`): the version itself already
        // means "close".
        use axum::routing::any;
        let app = Router::new().route("/health", any(|| async { StatusCode::UNAUTHORIZED }));
        let request = Request::builder()
            .method(Method::CONNECT)
            .uri("/health")
            .version(axum::http::Version::HTTP_10)
            .body(Body::empty())
            .expect("request");
        let response = respond(app, request).await.expect("infallible");
        assert!(response.headers().get(header::CONNECTION).is_none());
    }

    /// Spin a real listener + hyper, then exchange raw bytes with it.
    async fn exchange_roundtrip(payload: &[u8]) -> Vec<u8> {
        use axum::routing::get;

        let app = Router::new().route("/health", get(|| async { "ok" }));
        exchange_roundtrip_app(app, payload).await
    }

    async fn exchange_roundtrip_app(app: Router, payload: &[u8]) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    async fn response_trailers_are_written_without_te_or_trailer_header() {
        // Go's net/http writes `http.TrailerPrefix` fields after the terminal
        // chunk regardless of the request's TE header and without announcing
        // them in a Trailer response header. Hyper 1.12.0 would drop them;
        // vendor/hyper/PATCHES.md restores Go's behavior.
        use bytes::Bytes;
        use futures_util::stream;
        use http_body::Frame;
        use http_body_util::StreamBody;

        let app = Router::new().route(
            "/trailers",
            axum::routing::get(|| async {
                let stream = stream::iter(vec![
                    Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from_static(b"body"))),
                    Ok(Frame::trailers({
                        let mut trailers = axum::http::HeaderMap::new();
                        trailers.insert("grpc-status", axum::http::HeaderValue::from_static("0"));
                        trailers
                    })),
                ]);
                axum::body::Body::new(StreamBody::new(stream))
            }),
        );
        let out = exchange_roundtrip_app(
            app,
            b"GET /trailers HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("transfer-encoding: chunked"), "got {text:?}");
        assert!(
            text.ends_with("0\r\ngrpc-status: 0\r\n\r\n"),
            "got {text:?}"
        );
    }

    #[tokio::test]
    async fn rewrites_the_preface_and_withholds_the_frames_after_it() {
        // Go answers the bare preface through its handler and closes; hyper
        // cannot parse `HTTP/2.0`, so the guard delivers the preface line as
        // HTTP/1.1 with `Connection: close` and drops the SETTINGS bytes.
        let raw = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x12\x04\x00\x00\x00\x00\x00";
        let (out, reject) = run(raw).await;
        assert_eq!(out, b"PRI * HTTP/1.1\r\nConnection: close\r\n\r\n");
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn connection_close_withholds_the_tail() {
        // Go stops parsing after a close-bound response (closeAfterReply); a
        // complete bad tail must not earn an extra 400.
        let raw =
            b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\nBAD REQUEST LINE\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(
            out,
            b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn failed_expect_withholds_the_tail() {
        let raw =
            b"GET /health HTTP/1.1\r\nHost: x\r\nExpect: 201-created\r\n\r\nBAD REQUEST LINE\r\n\r\n";
        let (out, reject) = run(raw).await;
        assert_eq!(
            out,
            b"GET /health HTTP/1.1\r\nHost: x\r\nExpect: 201-created\r\n\r\n"
        );
        assert_eq!(reject, None);
    }

    #[tokio::test]
    async fn the_body_still_reaches_hyper_before_a_close_bound_halt() {
        let head =
            b"POST /init HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 4\r\n\r\n";
        let raw = [
            head.as_slice(),
            b"body",
            b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n",
        ]
        .concat();
        let (out, reject) = run(&raw).await;
        assert_eq!(out, [head.as_slice(), b"body"].concat());
        assert_eq!(reject, None);
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
