//! HTTP adapter for plain and PTY processes, including live terminal updates.
//!
//! The interceptor lines mirror internal/logs/interceptor.go and the process
//! service wrappers (start.go:20, connect.go:16, input.go:50, service.go:162)
//! at 51083a83cc0903185d992725ea6bb33683814b29: unary methods log one line
//! after the call, server streams log start before the handler and end after
//! it, and the client stream logs start without a request payload. Payload
//! fields are Go `encoding/json` shapes (daemon::process_payload).
use super::{
    process_payload as payload,
    rpc_log::{event as interceptor_event, unary_event, StreamEndError},
    Daemon,
};
use crate::{
    process::{ProcessRuntime, RuntimeError, Subscription},
    proto::process as pb,
};
use connectrpc::{
    handler::{client_streaming_handler_fn, handler_fn, streaming_handler_fn},
    ConnectError, RequestContext, Response, Router, ServiceStream,
};
use futures_util::{stream, Stream, StreamExt};
use serde_json::Value;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

fn error(error: RuntimeError) -> ConnectError {
    match error {
        RuntimeError::NotFound => ConnectError::not_found("process not found"),
        RuntimeError::InvalidArgument(message) => ConnectError::invalid_argument(message),
        RuntimeError::FailedPrecondition(message) => ConnectError::failed_precondition(message),
        RuntimeError::Unimplemented(message) => ConnectError::unimplemented(message),
        RuntimeError::Unavailable(message) => ConnectError::unavailable(message),
        // start_error.go:32-34 via start.go:184: an armed request deadline
        // ending the start pre-fork is the caller's own outcome, code 4.
        RuntimeError::DeadlineExceeded(message) => ConnectError::deadline_exceeded(message),
        RuntimeError::ResourceExhausted(message) => ConnectError::resource_exhausted(message),
        RuntimeError::Internal(message) => ConnectError::internal(message),
    }
}

fn selection_error(cause: RuntimeError, selector: &pb::ProcessSelector) -> ConnectError {
    if matches!(cause, RuntimeError::NotFound) {
        return match &selector.selector {
            Some(pb::process_selector::Selector::Pid(pid)) => {
                ConnectError::not_found(format!("process with pid {pid} not found"))
            }
            Some(pb::process_selector::Selector::Tag(tag)) => {
                ConnectError::not_found(format!("process with tag {tag} not found"))
            }
            None => ConnectError::unimplemented("invalid input type *process.ProcessSelector"),
        };
    }
    if selector.selector.is_none() {
        ConnectError::unimplemented("invalid input type *process.ProcessSelector")
    } else {
        error(cause)
    }
}

fn input_error(cause: RuntimeError, input: &pb::ProcessInput) -> ConnectError {
    let mut mapped = error(cause);
    let channel = match input.input {
        Some(pb::process_input::Input::Stdin(_)) => "stdin",
        Some(pb::process_input::Input::Pty(_)) => "tty",
        None => return mapped,
    };
    if mapped.code != connectrpc::ErrorCode::NotFound {
        mapped.message = mapped
            .message
            .map(|message| format!("error writing to {channel}: {message}"));
    }
    mapped
}

async fn configured(
    daemon: &Daemon,
    runtime: &ProcessRuntime,
    ctx: RequestContext,
) -> Result<ProcessRuntime, ConnectError> {
    let state = daemon.state.lock().await;
    let defaults = state.defaults.clone();
    let environment = state.environment.clone();
    drop(state);
    let account = tokio::task::spawn_blocking({
        let defaults = defaults.clone();
        move || crate::exec_context::request_account(&defaults, &ctx)
    })
    .await
    .map_err(|_| ConnectError::internal("account lookup failed"))??;
    Ok(runtime
        .with_context(defaults, environment)
        .with_account(account))
}

fn keepalive_interval(ctx: &RequestContext) -> Duration {
    let seconds = ctx
        .header("keepalive-ping-interval")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0 && *v <= i64::MAX / 1_000_000_000)
        .unwrap_or(90);
    Duration::from_secs(seconds as u64)
}

fn events<T: Send + 'static>(
    subscription: Subscription,
    interval: Duration,
    wrap: fn(pb::ProcessEvent) -> T,
) -> impl futures_util::Stream<Item = Result<T, ConnectError>> + Send {
    stream::unfold(subscription, move |mut subscription| async move {
        let event = tokio::select! {
            event=subscription.recv()=>event?,
            _=tokio::time::sleep(interval)=>pb::ProcessEvent {event:Some(pb::process_event::KeepAlive::default().into()),..Default::default()},
        };
        Some((Ok(wrap(event)), subscription))
    })
}

/// Go `NewUnaryLogInterceptor` (interceptor.go:52-93) for the process service:
/// one line after the call, Info on success (interceptor.go:63-64
/// `logger.Err(err)` is zerolog Info for a nil err, log.go:373-382) and Error
/// on failure, with the request payload always present (interceptor.go:74-76)
/// and the response only on success (interceptor.go:78-83). The stream lines
/// keep the Debug level via getErrDebugLogEvent (interceptor.go:166-172).
fn log_unary(
    logger: &crate::logs::Logger,
    procedure: &str,
    operation_id: &str,
    error: Option<&ConnectError>,
    response: Option<Value>,
    request: Value,
) {
    let mapped = error.map(|error| StreamEndError::Connect(error.clone()));
    let mut line = unary_event(logger, procedure, operation_id, mapped.as_ref());
    line = line.raw("request", request);
    if mapped.is_none() {
        line = line.raw("response", response.unwrap_or(Value::Null));
    }
    line.msg(crate::logs::format_method(procedure));
}

/// Go `LogServerStreamWithoutEvents` start line (interceptor.go:104-110).
fn log_server_stream_start(
    logger: &crate::logs::Logger,
    procedure: &str,
    operation_id: &str,
    request: Value,
) {
    logger
        .debug()
        .str("method", format!("POST {procedure}"))
        .str("operation_id", operation_id)
        .raw("request", request)
        .msg(format!(
            "{} (server stream start)",
            crate::logs::format_method(procedure)
        ));
}

/// Go `LogServerStreamWithoutEvents` end line (interceptor.go:113-127) and the
/// client-stream twin (interceptor.go:141-158): error first on failure with
/// the connect code, `response` null for a server stream that ran to
/// completion, the response payload for a client stream.
fn log_stream_end(
    logger: &crate::logs::Logger,
    procedure: &str,
    operation_id: &str,
    phase: &str,
    error: Option<StreamEndError>,
    response: Option<Value>,
) {
    let mut line = interceptor_event(logger, procedure, operation_id, error.as_ref());
    if error.is_none() {
        line = line.raw("response", response.unwrap_or(Value::Null));
    }
    line.msg(format!(
        "{} ({phase})",
        crate::logs::format_method(procedure)
    ));
}

/// Reports the end of a server stream, including the client disconnect that
/// drops a still-live body: Go's handler then returns the bare ctx.Err()
/// (watch.go:88-89 shape), which `connect.CodeOf` maps to CodeUnknown
/// (connect@v1.18.1 code.go:221-226).
struct LoggedStream<T> {
    inner: Pin<Box<dyn Stream<Item = Result<T, ConnectError>> + Send>>,
    logger: crate::logs::Logger,
    operation_id: String,
    procedure: &'static str,
    ended: bool,
    last_error: Option<ConnectError>,
}

impl<T: Send + 'static> Stream for LoggedStream<T> {
    type Item = Result<T, ConnectError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
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

impl<T> Drop for LoggedStream<T> {
    fn drop(&mut self) {
        let error = if let Some(last_error) = &self.last_error {
            Some(StreamEndError::Connect(last_error.clone()))
        } else if !self.ended {
            Some(StreamEndError::ContextCanceled)
        } else {
            None
        };
        log_stream_end(
            &self.logger,
            self.procedure,
            &self.operation_id,
            "server stream end",
            error,
            None,
        );
    }
}

pub(super) fn router(daemon: Arc<Daemon>, runtime: ProcessRuntime) -> Router {
    let router = Router::new();
    let listed = runtime.clone();
    let list_daemon = daemon.clone();
    let router = router.route(
        "process.Process",
        "List",
        handler_fn(move |_: RequestContext, _: pb::ListRequest| {
            let daemon = list_daemon.clone();
            let runtime = listed.clone();
            async move {
                let logger = daemon.process_logger.clone();
                let operation_id = crate::logs::assign_operation_id();
                let procedure = "/process.Process/List";
                let processes = runtime.list().await;
                let outcome: Result<pb::ListResponse, ConnectError> = Ok(pb::ListResponse {
                    processes,
                    ..Default::default()
                });
                log_unary(
                    &logger,
                    procedure,
                    &operation_id,
                    outcome.as_ref().err(),
                    outcome
                        .as_ref()
                        .ok()
                        .map(|response| payload::list_response(&response.processes)),
                    payload::list_request(),
                );
                let response = outcome?;
                Response::ok(response)
            }
        }),
    );
    let started = runtime.clone();
    let start_daemon = daemon.clone();
    let router = router.route_server_stream(
        "process.Process",
        "Start",
        streaming_handler_fn(move |ctx: RequestContext, request: pb::StartRequest| {
            let daemon = start_daemon.clone();
            let runtime = started.clone();
            async move {
                let logger = daemon.process_logger.clone();
                // Go's per-request operation ID reaches the interceptor lines
                // (start.go:20) and the handler's own process_start/process_end
                // lines (start.go:25-27).
                let operation_id = crate::logs::assign_operation_id();
                let procedure = "/process.Process/Start";
                // interceptor.go:104-110: the start line carries the request
                // payload and precedes the handler.
                log_server_stream_start(
                    &logger,
                    procedure,
                    &operation_id,
                    payload::start_request(&request),
                );
                let timeout = ctx
                    .header("connect-timeout-ms")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|v| *v > 0)
                    .map(Duration::from_millis);
                let interval = keepalive_interval(&ctx);
                let started = async {
                    let runtime = configured(&daemon, &runtime, ctx).await?;
                    runtime
                        .start_with_operation_id(request, timeout, Some(operation_id.clone()))
                        .await
                        .map_err(error)
                }
                .await;
                match started {
                    Ok(subscription) => {
                        let stream = events(subscription, interval, |event| pb::StartResponse {
                            event: event.into(),
                            ..Default::default()
                        });
                        Response::stream_ok(LoggedStream {
                            inner: Box::pin(stream),
                            logger,
                            operation_id,
                            procedure,
                            ended: false,
                            last_error: None,
                        })
                    }
                    Err(start_error) => {
                        log_stream_end(
                            &logger,
                            procedure,
                            &operation_id,
                            "server stream end",
                            Some(StreamEndError::Connect(start_error.clone())),
                            None,
                        );
                        Err(start_error)
                    }
                }
            }
        }),
    );
    let connected = runtime.clone();
    let connect_daemon = daemon.clone();
    let router = router.route_server_stream(
        "process.Process",
        "Connect",
        streaming_handler_fn(move |ctx: RequestContext, request: pb::ConnectRequest| {
            let daemon = connect_daemon.clone();
            let runtime = connected.clone();
            async move {
                let logger = daemon.process_logger.clone();
                let operation_id = crate::logs::assign_operation_id();
                let procedure = "/process.Process/Connect";
                log_server_stream_start(
                    &logger,
                    procedure,
                    &operation_id,
                    payload::connect_request(&request.process),
                );
                let interval = keepalive_interval(&ctx);
                let connected = runtime
                    .connect(&request.process)
                    .await
                    .map_err(|cause| selection_error(cause, &request.process));
                match connected {
                    Ok(subscription) => {
                        let stream = events(subscription, interval, |event| pb::ConnectResponse {
                            event: event.into(),
                            ..Default::default()
                        });
                        Response::stream_ok(LoggedStream {
                            inner: Box::pin(stream),
                            logger,
                            operation_id,
                            procedure,
                            ended: false,
                            last_error: None,
                        })
                    }
                    Err(connect_error) => {
                        log_stream_end(
                            &logger,
                            procedure,
                            &operation_id,
                            "server stream end",
                            Some(StreamEndError::Connect(connect_error.clone())),
                            None,
                        );
                        Err(connect_error)
                    }
                }
            }
        }),
    );
    let fed = runtime.clone();
    let input_daemon = daemon.clone();
    let router = router.route(
        "process.Process",
        "SendInput",
        handler_fn(move |_: RequestContext, request: pb::SendInputRequest| {
            let daemon = input_daemon.clone();
            let runtime = fed.clone();
            async move {
                let logger = daemon.process_logger.clone();
                let operation_id = crate::logs::assign_operation_id();
                let procedure = "/process.Process/SendInput";
                let request_value = payload::send_input_request(&request);
                let outcome = runtime
                    .send_input(&request.process, &request.input)
                    .await
                    .map_err(|cause| selection_error(cause, &request.process));
                log_unary(
                    &logger,
                    procedure,
                    &operation_id,
                    outcome.as_ref().err(),
                    Some(payload::empty_response()),
                    request_value,
                );
                outcome?;
                Response::ok(pb::SendInputResponse::default())
            }
        }),
    );
    let closed = runtime.clone();
    let close_daemon = daemon.clone();
    let router = router.route(
        "process.Process",
        "CloseStdin",
        handler_fn(move |_: RequestContext, request: pb::CloseStdinRequest| {
            let daemon = close_daemon.clone();
            let runtime = closed.clone();
            async move {
                let logger = daemon.process_logger.clone();
                let operation_id = crate::logs::assign_operation_id();
                let procedure = "/process.Process/CloseStdin";
                let request_value = payload::close_stdin_request(&request.process);
                let outcome = runtime
                    .close_stdin(&request.process)
                    .await
                    .map_err(|cause| {
                        // Oracle input.go:36/:90 — the selector lookup goes
                        // through getProcess, so a pid/tag miss carries the
                        // oracle's per-selector text before any wrap; only a
                        // post-lookup failure gets the
                        // "error closing stdin: %w" wrap (input.go:93-95).
                        let is_lookup_error = matches!(
                            cause,
                            RuntimeError::NotFound | RuntimeError::Unimplemented(_)
                        );
                        let mut mapped = selection_error(cause, &request.process);
                        if !is_lookup_error {
                            mapped.message = mapped
                                .message
                                .map(|message| format!("error closing stdin: {message}"));
                        }
                        mapped
                    });
                log_unary(
                    &logger,
                    procedure,
                    &operation_id,
                    outcome.as_ref().err(),
                    Some(payload::empty_response()),
                    request_value,
                );
                outcome?;
                Response::ok(pb::CloseStdinResponse::default())
            }
        }),
    );
    let signalled = runtime.clone();
    let signal_daemon = daemon.clone();
    let router = router.route(
        "process.Process",
        "SendSignal",
        handler_fn(move |_: RequestContext, request: pb::SendSignalRequest| {
            let daemon = signal_daemon.clone();
            let runtime = signalled.clone();
            async move {
                let logger = daemon.process_logger.clone();
                let operation_id = crate::logs::assign_operation_id();
                let procedure = "/process.Process/SendSignal";
                let request_value =
                    payload::send_signal_request(&request.process, request.signal.to_i32());
                let outcome = runtime
                    .send_signal(&request.process, request.signal.to_i32())
                    .await
                    .map_err(|cause| selection_error(cause, &request.process));
                log_unary(
                    &logger,
                    procedure,
                    &operation_id,
                    outcome.as_ref().err(),
                    Some(payload::empty_response()),
                    request_value,
                );
                outcome?;
                Response::ok(pb::SendSignalResponse::default())
            }
        }),
    );
    let updated = runtime.clone();
    let update_daemon = daemon.clone();
    let router = router.route(
        "process.Process",
        "Update",
        handler_fn(move |_: RequestContext, request: pb::UpdateRequest| {
            let daemon = update_daemon.clone();
            let runtime = updated.clone();
            async move {
                let logger = daemon.process_logger.clone();
                let operation_id = crate::logs::assign_operation_id();
                let procedure = "/process.Process/Update";
                let request_value = payload::update_request(&request);
                let outcome = runtime.update(&request).await.map_err(|cause| match cause {
                    RuntimeError::Internal(message) => {
                        ConnectError::internal(format!("error resizing tty: {message}"))
                    }
                    // Update resolves the tty through getProcess too
                    // (update.go), so its lookup refusals carry the same
                    // pid/tag/type texts as every other procedure.
                    cause => selection_error(cause, &request.process),
                });
                log_unary(
                    &logger,
                    procedure,
                    &operation_id,
                    outcome.as_ref().err(),
                    Some(payload::empty_response()),
                    request_value,
                );
                outcome?;
                Response::ok(pb::UpdateResponse::default())
            }
        }),
    );
    let streamed = runtime.clone();
    let stream_daemon = daemon.clone();
    router.route_client_stream(
        "process.Process",
        "StreamInput",
        client_streaming_handler_fn(
            move |_: RequestContext, mut requests: ServiceStream<pb::StreamInputRequest>| {
                let daemon = stream_daemon.clone();
                let runtime = streamed.clone();
                async move {
                    let logger = daemon.process_logger.clone();
                    let operation_id = crate::logs::assign_operation_id();
                    let procedure = "/process.Process/StreamInput";
                    // interceptor.go:132-138: the client-stream start line
                    // carries no request payload.
                    logger
                        .debug()
                        .str("method", format!("POST {procedure}"))
                        .str("operation_id", &operation_id)
                        .msg(format!(
                            "{} (client stream start)",
                            crate::logs::format_method(procedure)
                        ));
                    let outcome: Result<pb::StreamInputResponse, ConnectError> = async {
                        let mut selected = None;
                        while let Some(request) = requests.next().await {
                            let request = request.map_err(|cause| {
                                ConnectError::unknown(format!("error streaming input: {cause}"))
                            })?;
                            match request.event {
                                Some(pb::stream_input_request::Event::Start(start)) => {
                                    selected =
                                        Some(runtime.select_input(&start.process).await.map_err(
                                            |cause| selection_error(cause, &start.process),
                                        )?);
                                }
                                Some(pb::stream_input_request::Event::Data(data)) => {
                                    let selected = selected.as_ref().ok_or_else(|| {
                                        ConnectError::failed_precondition("process not selected")
                                    })?;
                                    selected
                                        .send_input(&data.input)
                                        .await
                                        .map_err(|cause| input_error(cause, &data.input))?;
                                }
                                Some(pb::stream_input_request::Event::Keepalive(_)) => {}
                                None => {
                                    return Err(ConnectError::unimplemented(
                                        "invalid event type <nil>",
                                    ))
                                }
                            }
                        }
                        Ok(pb::StreamInputResponse::default())
                    }
                    .await;
                    // interceptor.go:141-158: the end line carries the response
                    // payload on success, the error and connect code on failure.
                    log_stream_end(
                        &logger,
                        procedure,
                        &operation_id,
                        "client stream end",
                        outcome
                            .as_ref()
                            .err()
                            .map(|error| StreamEndError::Connect(error.clone())),
                        Some(payload::empty_response()),
                    );
                    let response = outcome?;
                    Response::ok(response)
                }
            },
        ),
    )
}

#[cfg(test)]
mod error_mapping {
    use super::{error, ConnectError};
    use crate::process::RuntimeError;
    use connectrpc::ErrorCode;

    /// start_error.go:32-34 via start.go:184: an armed request deadline that
    /// ends the start pre-fork is the caller's own outcome and codes
    /// CodeDeadlineExceeded — never the InvalidArgument default — with the
    /// wrapped start text on the wire.
    #[test]
    fn an_armed_deadline_refusal_maps_to_the_deadline_code() {
        let mapped: ConnectError = error(RuntimeError::DeadlineExceeded(
            "error starting process '/bin/sleep 5': context deadline exceeded".into(),
        ));
        assert_eq!(mapped.code, ErrorCode::DeadlineExceeded);
        assert_eq!(
            mapped.message.as_deref(),
            Some("error starting process '/bin/sleep 5': context deadline exceeded")
        );
    }
}
