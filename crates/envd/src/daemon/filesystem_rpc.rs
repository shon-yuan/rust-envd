//! Unary filesystem RPC adapters. File I/O and NSS lookup run off async workers.
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::exec_context::{Account, Defaults};
use crate::filesystem::watchers::WatcherRegistry;
use crate::logs::{assign_operation_id, format_method, Logger};
use connectrpc::{
    handler::{handler_fn, streaming_handler_fn},
    ConnectError, RequestContext, Response, Router,
};
use futures_util::{stream, Stream};

use super::rpc_log::{event as interceptor_event, unary_event, StreamEndError};
use super::{filesystem_payload as payload, Daemon};
use crate::{
    exec_context::request_account, filesystem::FilesystemRuntime, proto::filesystem as pb,
};
use serde_json::Value;

/// Go `NewUnaryLogInterceptor` (internal/logs/interceptor.go:52-93): one line
/// per unary call, Info on success — `logger.Err(err)` with a nil err is
/// zerolog `Info()` (log.go:373-382) — and Error on failure, with the method,
/// operation ID, and connect error code. The request payload is always present
/// (interceptor.go:72-74) and the response payload only on success
/// (interceptor.go:76-82, `response` null for a nil response).
fn log_unary(
    logger: &Logger,
    method: &str,
    operation_id: &str,
    error: Option<&ConnectError>,
    response: Option<Value>,
    request: Value,
) {
    let mapped = error.map(|error| StreamEndError::Connect(error.clone()));
    let procedure = format!("/filesystem.Filesystem/{method}");
    let mut line = unary_event(logger, &procedure, operation_id, mapped.as_ref());
    line = line.raw("request", request);
    if mapped.is_none() {
        line = line.raw("response", response.unwrap_or(Value::Null));
    }
    line.msg(format_method(&procedure));
}

/// Go `LogServerStreamWithoutEvents` start/end lines
/// (interceptor.go:100-128). The start line carries the request payload
/// (interceptor.go:106-108); both stream lines stay Debug on success via
/// `getErrDebugLogEvent` (interceptor.go:166-172).
fn log_stream_start(logger: &Logger, method: &str, operation_id: &str, request: Value) {
    let procedure = format!("/filesystem.Filesystem/{method}");
    interceptor_event(logger, &procedure, operation_id, None)
        .raw("request", request)
        .msg(format!(
            "{} (server stream start)",
            format_method(&procedure)
        ));
}

/// The end line reports the error and connect code on failure, and
/// `response` null for a handler that returned without an error
/// (interceptor.go:113-124).
fn log_stream_end(
    logger: &Logger,
    method: &str,
    operation_id: &str,
    error: Option<&StreamEndError>,
) {
    let procedure = format!("/filesystem.Filesystem/{method}");
    let mut line = interceptor_event(logger, &procedure, operation_id, error);
    if error.is_none() {
        line = line.raw("response", Value::Null);
    }
    line.msg(format!("{} (server stream end)", format_method(&procedure)));
}

/// Reports the end of the server stream when the response body is dropped,
/// which is the Rust shape of the Go stream handler returning.
struct LoggedWatchStream {
    inner: Pin<Box<dyn Stream<Item = Result<pb::WatchDirResponse, ConnectError>> + Send>>,
    logger: Logger,
    operation_id: String,
    /// Set once the stream returns None: Go's handler then returned without an
    /// error, so the end line is Debug (interceptor.go:122).
    ended: bool,
    /// The last item error, whose code and text the end line reports
    /// (interceptor.go:117-121).
    last_error: Option<ConnectError>,
}

impl Stream for LoggedWatchStream {
    type Item = Result<pb::WatchDirResponse, ConnectError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(context) {
            Poll::Ready(Some(Err(error))) => {
                self.last_error = Some(error.clone());
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.ended = true;
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl Drop for LoggedWatchStream {
    fn drop(&mut self) {
        let error = if let Some(last_error) = &self.last_error {
            Some(StreamEndError::Connect(last_error.clone()))
        } else if !self.ended {
            // A drop while the stream is live is the Rust shape of the client
            // disconnect that makes watch.go:88-89 return the bare ctx.Err();
            // connect.CodeOf maps that to CodeUnknown (connect@v1.18.1
            // code.go:221-226), not CodeCanceled.
            Some(StreamEndError::ContextCanceled)
        } else {
            None
        };
        log_stream_end(&self.logger, "WatchDir", &self.operation_id, error.as_ref());
    }
}

async fn execute<T, F, R>(
    daemon: Arc<Daemon>,
    context: RequestContext,
    method: &'static str,
    request_value: Value,
    encode_response: R,
    operation: F,
) -> Result<T, ConnectError>
where
    T: Send + 'static,
    F: FnOnce(&FilesystemRuntime) -> Result<T, ConnectError> + Send + 'static,
    R: FnOnce(&T) -> Value + Send + 'static,
{
    let logger = daemon.filesystem_logger.clone();
    let operation_id = assign_operation_id();
    let defaults = daemon.state.lock().await.defaults.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let account = request_account(&defaults, &context)?;
        operation(&FilesystemRuntime::new(account, defaults))
    })
    .await
    .map_err(|_| ConnectError::internal("filesystem worker failed"))?;
    log_unary(
        &logger,
        method,
        &operation_id,
        outcome.as_ref().err(),
        outcome.as_ref().ok().map(encode_response),
        request_value,
    );
    outcome
}

pub(super) fn router(daemon: Arc<Daemon>) -> Router {
    let stat = daemon.clone();
    let make_dir = daemon.clone();
    let move_entry = daemon.clone();
    let list_dir = daemon.clone();
    let remove = daemon.clone();
    let router = Router::new()
        .route(
            "filesystem.Filesystem",
            "Stat",
            handler_fn(move |context: RequestContext, request: pb::StatRequest| {
                let daemon = stat.clone();
                async move {
                    let request_value = payload::stat_request(&request);
                    let entry = execute(
                        daemon,
                        context,
                        "Stat",
                        request_value,
                        payload::entry_response,
                        move |runtime| runtime.stat(&request.path),
                    )
                    .await?;
                    Response::ok(pb::StatResponse {
                        entry: entry.into(),
                        ..Default::default()
                    })
                }
            }),
        )
        .route(
            "filesystem.Filesystem",
            "MakeDir",
            handler_fn(
                move |context: RequestContext, request: pb::MakeDirRequest| {
                    let daemon = make_dir.clone();
                    async move {
                        let request_value = payload::make_dir_request(&request);
                        let entry = execute(
                            daemon,
                            context,
                            "MakeDir",
                            request_value,
                            payload::entry_response,
                            move |runtime| runtime.make_dir(&request.path),
                        )
                        .await?;
                        Response::ok(pb::MakeDirResponse {
                            entry: entry.into(),
                            ..Default::default()
                        })
                    }
                },
            ),
        )
        .route(
            "filesystem.Filesystem",
            "Move",
            handler_fn(move |context: RequestContext, request: pb::MoveRequest| {
                let daemon = move_entry.clone();
                async move {
                    let request_value = payload::move_request(&request);
                    let entry = execute(
                        daemon,
                        context,
                        "Move",
                        request_value,
                        payload::entry_response,
                        move |runtime| runtime.move_entry(&request.source, &request.destination),
                    )
                    .await?;
                    Response::ok(pb::MoveResponse {
                        entry: entry.into(),
                        ..Default::default()
                    })
                }
            }),
        )
        .route(
            "filesystem.Filesystem",
            "ListDir",
            handler_fn(
                move |context: RequestContext, request: pb::ListDirRequest| {
                    let daemon = list_dir.clone();
                    async move {
                        let request_value = payload::list_dir_request(&request);
                        let entries = execute(
                            daemon,
                            context,
                            "ListDir",
                            request_value,
                            |entries: &Vec<pb::EntryInfo>| payload::list_dir_response(entries),
                            move |runtime| runtime.list_dir(&request.path, request.depth),
                        )
                        .await?;
                        Response::ok(pb::ListDirResponse {
                            entries,
                            ..Default::default()
                        })
                    }
                },
            ),
        )
        .route(
            "filesystem.Filesystem",
            "Remove",
            handler_fn(move |context: RequestContext, request: pb::RemoveRequest| {
                let daemon = remove.clone();
                async move {
                    let request_value = payload::remove_request(&request);
                    execute(
                        daemon,
                        context,
                        "Remove",
                        request_value,
                        |_| payload::empty_response(),
                        move |runtime| runtime.remove(&request.path),
                    )
                    .await?;
                    Response::ok(pb::RemoveResponse::default())
                }
            }),
        );
    watcher_routes(router, daemon)
}

async fn watcher_context<T, F>(
    daemon: Arc<Daemon>,
    context: RequestContext,
    operation: F,
) -> Result<T, ConnectError>
where
    T: Send + 'static,
    F: FnOnce(WatcherRegistry, Account, Defaults) -> Result<T, ConnectError> + Send + 'static,
{
    let defaults = daemon.state.lock().await.defaults.clone();
    let registry = daemon.watchers.clone();
    tokio::task::spawn_blocking(move || {
        let account = request_account(&defaults, &context)?;
        operation(registry, account, defaults)
    })
    .await
    .map_err(|_| ConnectError::internal("watcher worker failed"))?
}

// Go ID operations never require a default account; its global Basic middleware
// still rejects an explicitly supplied unknown username. Reuse the same parser
// with a known valid fallback only when a Basic header is present.
async fn validate_explicit_user(context: RequestContext) -> Result<(), ConnectError> {
    if context.header("authorization").is_none() {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || {
        let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .map_err(|_| ConnectError::internal("failed to look up daemon user"))?
            .ok_or_else(|| ConnectError::internal("daemon user not found"))?;
        request_account(
            &Defaults {
                user: user.name,
                ..Default::default()
            },
            &context,
        )?;
        Ok(())
    })
    .await
    .map_err(|_| ConnectError::internal("watcher worker failed"))?
}

fn watcher_routes(router: Router, daemon: Arc<Daemon>) -> Router {
    let create = daemon.clone();
    let events = daemon.clone();
    let remove = daemon.clone();
    router
        .route(
            "filesystem.Filesystem",
            "CreateWatcher",
            handler_fn(
                move |context: RequestContext, request: pb::CreateWatcherRequest| {
                    let daemon = create.clone();
                    async move {
                        let logger = daemon.filesystem_logger.clone();
                        let operation_id = assign_operation_id();
                        let request_value = payload::watch_request(&request);
                        let outcome =
                            watcher_context(daemon, context, move |registry, account, defaults| {
                                registry.create_watcher(&account, &defaults, request)
                            })
                            .await;
                        log_unary(
                            &logger,
                            "CreateWatcher",
                            &operation_id,
                            outcome.as_ref().err(),
                            outcome.as_ref().ok().map(payload::create_watcher_response),
                            request_value,
                        );
                        Response::ok(outcome?)
                    }
                },
            ),
        )
        .route(
            "filesystem.Filesystem",
            "GetWatcherEvents",
            handler_fn(
                move |context: RequestContext, request: pb::GetWatcherEventsRequest| {
                    let daemon = events.clone();
                    async move {
                        let logger = daemon.filesystem_logger.clone();
                        let operation_id = assign_operation_id();
                        let request_value = payload::watcher_id_request(&request.watcher_id);
                        let outcome = async {
                            validate_explicit_user(context).await?;
                            daemon.watchers.get_watcher_events(&request.watcher_id)
                        }
                        .await;
                        log_unary(
                            &logger,
                            "GetWatcherEvents",
                            &operation_id,
                            outcome.as_ref().err(),
                            outcome
                                .as_ref()
                                .ok()
                                .map(payload::get_watcher_events_response),
                            request_value,
                        );
                        Response::ok(outcome?)
                    }
                },
            ),
        )
        .route(
            "filesystem.Filesystem",
            "RemoveWatcher",
            handler_fn(
                move |context: RequestContext, request: pb::RemoveWatcherRequest| {
                    let daemon = remove.clone();
                    async move {
                        let logger = daemon.filesystem_logger.clone();
                        let operation_id = assign_operation_id();
                        let request_value = payload::watcher_id_request(&request.watcher_id);
                        let outcome = async {
                            validate_explicit_user(context).await?;
                            daemon.watchers.remove_watcher(&request.watcher_id)
                        }
                        .await;
                        log_unary(
                            &logger,
                            "RemoveWatcher",
                            &operation_id,
                            outcome.as_ref().err(),
                            outcome.as_ref().ok().map(|_| payload::empty_response()),
                            request_value,
                        );
                        Response::ok(outcome?)
                    }
                },
            ),
        )
        .route_server_stream(
            "filesystem.Filesystem",
            "WatchDir",
            streaming_handler_fn(
                move |context: RequestContext, request: pb::WatchDirRequest| {
                    let daemon = daemon.clone();
                    let keepalive = context
                        .header("keepalive-ping-interval")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    async move {
                        let logger = daemon.filesystem_logger.clone();
                        let operation_id = assign_operation_id();
                        // interceptor.go:104-110: the start line precedes the
                        // handler unconditionally and carries the request
                        // payload, so a failed setup still yields both lines.
                        log_stream_start(
                            &logger,
                            "WatchDir",
                            &operation_id,
                            payload::watch_dir_request(&request),
                        );
                        let outcome =
                            watcher_context(daemon, context, move |registry, account, defaults| {
                                registry.watch_dir(
                                    &account,
                                    &defaults,
                                    request,
                                    keepalive.as_deref(),
                                )
                            })
                            .await;
                        let subscription = match outcome {
                            Ok(subscription) => subscription,
                            Err(error) => {
                                let end = StreamEndError::Connect(error.clone());
                                log_stream_end(&logger, "WatchDir", &operation_id, Some(&end));
                                return Err(error);
                            }
                        };
                        let events = stream::unfold(subscription, |mut subscription| async move {
                            subscription.recv().await.map(|event| (event, subscription))
                        });
                        Response::stream_ok(LoggedWatchStream {
                            inner: Box::pin(events),
                            logger,
                            operation_id,
                            ended: false,
                            last_error: None,
                        })
                    }
                },
            ),
        )
}
