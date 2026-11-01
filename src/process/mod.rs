//! Plain and PTY process runtime for the pinned envd contract.
//!
//! The caller owns execution admission. Cgroup admission is supplied through
//! the daemon wiring; live-upgrade ADOPTION of live children is not possible
//! here (tokio-owned pipes cannot be relocated across execve — the disclosure
//! in `crate::upgrade`), so the handover carries this module's retention cache
//! only.
use crate::exec_context::{Account, Defaults, Environment};
use crate::proto::process as pb;

/// One retained terminal event, in the handover's wire shape
/// (upgrade.go:254-268 out, :542-570 in). Defined here — not in
/// `crate::upgrade` — because tests/process_runtime.rs compiles this module
/// directly, without the crate root; `upgrade` re-exports it.
#[allow(dead_code)] // Unused only where the test harness compiles this file directly.
#[derive(Clone, Debug)]
pub struct RetainedExit {
    pub pid: u32,
    pub tag: Option<String>,
    pub end: Option<pb::process_event::EndEvent>,
    pub remaining_ms: i64,
}

impl RetainedExit {
    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub fn to_wire(&self) -> crate::proto::upgrade::HandoverExit {
        crate::proto::upgrade::HandoverExit {
            pid: self.pid,
            tag: self.tag.clone().unwrap_or_default(),
            has_tag: self.tag.is_some(),
            end: self.end.clone().into(),
            remaining_ms: self.remaining_ms,
            ..Default::default()
        }
    }

    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub fn from_wire(entry: &crate::proto::upgrade::HandoverExit) -> Self {
        Self {
            pid: entry.pid,
            tag: entry.has_tag.then(|| entry.tag.clone()),
            end: entry.end.as_option().cloned(),
            remaining_ms: entry.remaining_ms,
        }
    }
}
use std::collections::HashMap;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, Mutex};
use tokio::time::Instant;

mod pty;
mod spawn;
mod supervise;

const RETENTION: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("process not found")]
    NotFound,
    #[error("{0}")]
    InvalidArgument(String),
    #[error("{0}")]
    FailedPrecondition(String),
    #[error("{0}")]
    Unimplemented(String),
    #[error("{0}")]
    Unavailable(String),
    /// Oracle start_error.go:32-34: an armed request deadline that ends the
    /// start pre-fork codes CodeDeadlineExceeded (start.go:40-44 arms procCtx
    /// from connect-timeout-ms; exec.CommandContext refuses with
    /// context.DeadlineExceeded).
    #[error("{0}")]
    DeadlineExceeded(String),
    #[error("{0}")]
    ResourceExhausted(String),
    #[error("{0}")]
    Internal(String),
}

/// Structured process lifecycle logs (Go handler.go:599-605, :697-706). The
/// runtime reaches logs through this seam so its private modules stay
/// independent of the logging stack; the production adapter is
/// `logs::ProcessLogger`.
pub trait ProcessLogSink: Send + Sync {
    fn process_started(&self, start: &ProcessStart);
    fn process_ended(&self, end: &ProcessEnd);
}

// The integration harness at tests/process_runtime.rs compiles this file
// directly without the production `logs::ProcessLogger` adapter, so the
// producer-side fields look unread in that compilation unit.
#[allow(dead_code)]
pub struct ProcessStart {
    pub pid: u32,
    pub command: String,
    pub request_timeout: Option<Duration>,
    pub operation_id: Option<String>,
}

#[allow(dead_code)]
pub struct ProcessEnd {
    pub pid: u32,
    pub end: pb::process_event::EndEvent,
    pub stdout_bytes: i64,
    pub stderr_bytes: i64,
    pub pty_bytes: i64,
    pub operation_id: Option<String>,
}

/// Dropping a subscription disconnects that observer, never the running child.
pub struct Subscription {
    receiver: mpsc::Receiver<pb::ProcessEvent>,
}

impl Subscription {
    pub async fn recv(&mut self) -> Option<pb::ProcessEvent> {
        self.receiver.recv().await
    }
}

type Sender = mpsc::Sender<pb::ProcessEvent>;

#[derive(Default)]
struct Subscribers {
    senders: Vec<Sender>,
    terminal: Option<pb::process_event::EndEvent>,
}

struct Entry {
    info: pb::ProcessInfo,
    stdin: Mutex<Option<ChildStdin>>,
    pty: Mutex<Option<Arc<pty::Terminal>>>,
    is_pty: bool,
    subscribers: Mutex<Subscribers>,
    control: mpsc::Sender<supervise::Control>,
    /// Go handler byte counters, logged once when the process ends.
    stdout_bytes: Arc<AtomicI64>,
    stderr_bytes: Arc<AtomicI64>,
    pty_bytes: Arc<AtomicI64>,
    operation_id: Option<String>,
}

/// Retains the selected process identity across tag or PID reuse.
pub(crate) struct SelectedProcess(Arc<Entry>);
impl SelectedProcess {
    pub(crate) async fn send_input(&self, input: &pb::ProcessInput) -> Result<(), RuntimeError> {
        write_input(&self.0, input).await
    }
}

struct Retained {
    info: pb::ProcessInfo,
    end: pb::process_event::EndEvent,
    expires: Instant,
}

#[derive(Default)]
struct Registry {
    live: HashMap<u32, Arc<Entry>>,
    retained: HashMap<u32, Retained>,
}

#[derive(Clone)]
pub struct ProcessRuntime {
    registry: Arc<Mutex<Registry>>,
    account: Account,
    defaults: Defaults,
    environment: Environment,
    daemon_path: Option<String>,
    logs: Option<Arc<dyn ProcessLogSink>>,
    /// Oracle spawn.go:267-294: a spawn probes the workload freezer before
    /// the clone — the barrier for freezes this process issued, the cgroup
    /// read for one the guest issued. Absent (direct runtime use, tests)
    /// means nothing to probe, exactly like a daemon built without one.
    freezer: Option<std::sync::Arc<crate::cgroups::freeze::WorkloadFreezer>>,
}

impl ProcessRuntime {
    /// Account data must be resolved by the caller, not by interpreting request strings.
    pub fn new(
        account: Account,
        defaults: Defaults,
        environment: Environment,
        daemon_path: Option<String>,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            registry: Arc::new(Mutex::new(Registry::default())),
            account,
            defaults,
            environment,
            daemon_path,
            logs: None,
            freezer: None,
        })
    }

    /// Oracle spawn.go:267-294: wire the workload freezer whose barrier and
    /// cgroup probes gate every start. Only the guest main passes one.
    pub fn with_freezer(
        self,
        freezer: std::sync::Arc<crate::cgroups::freeze::WorkloadFreezer>,
    ) -> Self {
        Self {
            freezer: Some(freezer),
            ..self
        }
    }

    /// The freezer this runtime probes before a spawn — the identity check
    /// for the one-freezer wiring (daemon::share_freezer, Go main.go:243).
    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub fn freezer(&self) -> Option<std::sync::Arc<crate::cgroups::freeze::WorkloadFreezer>> {
        self.freezer.clone()
    }

    /// Inject the production lifecycle log producer; absent by default so
    /// direct runtime use stays silent.
    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub fn with_process_logs(&self, logs: Arc<dyn ProcessLogSink>) -> Self {
        Self {
            logs: Some(logs),
            ..self.clone()
        }
    }

    pub fn with_account(&self, account: Account) -> Self {
        Self {
            account,
            ..self.clone()
        }
    }

    pub fn with_context(&self, defaults: Defaults, environment: Environment) -> Self {
        Self {
            defaults,
            environment,
            ..self.clone()
        }
    }

    pub async fn start(
        &self,
        request: pb::StartRequest,
        deadline: Option<Duration>,
    ) -> Result<Subscription, RuntimeError> {
        self.start_with_operation_id(request, deadline, None).await
    }

    /// Start with the RPC operation ID attached to the lifecycle log lines,
    /// as Go's per-request `handlerL` context does.
    pub async fn start_with_operation_id(
        &self,
        request: pb::StartRequest,
        deadline: Option<Duration>,
        operation_id: Option<String>,
    ) -> Result<Subscription, RuntimeError> {
        // Go handler.go:161-163: the user-facing command, without the wrapper.
        let command = user_command(&request.process);
        // Oracle handler.go:236-243 New resolves and stats the cwd BEFORE any
        // spawn attempt, so a bad workdir is an InvalidArgument even for a
        // request admission would also refuse.
        let cwd = self
            .defaults
            .resolve_path(request.process.cwd.as_deref().unwrap_or(""), &self.account)
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        // Oracle handler.go:244-247 intercepts ONLY ErrNotExist, with the
        // bespoke invalid-argument text naming the RESOLVED path. There is no
        // else branch: every other stat failure falls through to the spawn,
        // whose child chdir fails and surfaces as the WRAPPED fork/exec text —
        // exactly what wrapped_spawn_error renders below.
        if let Err(error) = std::fs::metadata(&cwd) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Err(RuntimeError::InvalidArgument(format!(
                    "cwd '{cwd}' does not exist"
                )));
            }
        }
        // Oracle start.go:40-44: procCtx is armed from connect-timeout-ms
        // before any fork; every start refusal below carries it.
        let deadline_at = deadline.map(|timeout| Instant::now() + timeout);
        // Oracle spawn.go:267-294: probe the freezer and take the barrier
        // permit BEFORE the clone, held until the child is registered, so a
        // spawn cannot land in a cgroup the sweep is about to freeze. The
        // refusal wraps like Go's start error, with the PTY shape when a
        // terminal was requested (handler.go:317-328 vs :593). An armed
        // caller deadline ends the admission wait early with
        // DeadlineExceeded (spawn.go:196-201), not Unavailable.
        let process_type = if request.pty.as_option().is_some() {
            crate::cgroups::ProcessType::Pty
        } else {
            crate::cgroups::ProcessType::User
        };
        let spawn_guard = match &self.freezer {
            Some(freezer) => Some(
                freezer
                    .begin_spawn(process_type, deadline_at)
                    .await
                    .map_err(|error| start_refusal(&request, &cwd, error))?,
            ),
            None => None,
        };
        // Oracle handler.go:220 exec.CommandContext refuses the spawn
        // pre-fork when procCtx is already done — no child is ever forked,
        // and start.go:184 wraps the context error like any other
        // spawn failure (StartErrorCode start_error.go:32-34).
        if deadline_at.is_some_and(|at| at <= Instant::now()) {
            return Err(RuntimeError::DeadlineExceeded(start_error(
                &request,
                &cwd,
                crate::cgroups::spawn::SpawnError::DeadlineExceeded,
            )));
        }
        // Acquire before spawning: cancellation at a lock await must not leave an
        // unregistered child without its independent reaper/deadline task.
        let mut registry = self.registry.lock().await;
        let mut command_builder = spawn::command(self, &request, process_type, &cwd)?;
        let terminal = request
            .pty
            .as_option()
            .map(|pty| pty::Terminal::configure(&mut command_builder, pty))
            .transpose()
            .map_err(|error| wrapped_spawn_error(&request, &cwd, &error))?;
        let mut child = command_builder
            .spawn()
            // os/exec's Start failure is *fs.PathError{Op:"fork/exec",
            // Path:"/bin/sh"} (os/exec_posix.go:61), so the wrap tail carries
            // that op and the resolved path (constant: /bin/sh is absolute).
            .map_err(|error| spawn::path_text_error("fork/exec", "/bin/sh", error))
            .map_err(|error| wrapped_spawn_error(&request, &cwd, &error))?;
        // Release the parent's slave copies before readers start waiting for EOF.
        drop(command_builder);
        let pid = child
            .id()
            .ok_or_else(|| RuntimeError::Internal("missing child PID".into()))?;
        let (sender, receiver) = mpsc::channel(1);
        // Start is queued before output readers exist, so data cannot precede it.
        sender
            .try_send(start_event(pid))
            .map_err(|error| RuntimeError::Internal(error.to_string()))?;
        let (control, controls) = mpsc::channel(1);
        let entry = Arc::new(Entry {
            info: pb::ProcessInfo {
                pid,
                config: request.process.clone(),
                tag: request.tag,
                ..Default::default()
            },
            stdin: Mutex::new(child.stdin.take()),
            pty: Mutex::new(terminal.clone()),
            is_pty: terminal.is_some(),
            subscribers: Mutex::new(Subscribers {
                senders: vec![sender],
                terminal: None,
            }),
            control,
            stdout_bytes: Arc::new(AtomicI64::new(0)),
            stderr_bytes: Arc::new(AtomicI64::new(0)),
            pty_bytes: Arc::new(AtomicI64::new(0)),
            operation_id: operation_id.clone(),
        });
        registry.retained.retain(|old_pid, exit| {
            *old_pid != pid
                && (entry.info.tag.is_none() || exit.info.tag != entry.info.tag)
                && exit.expires > Instant::now()
        });
        registry.live.insert(pid, entry.clone());
        drop(registry);
        // The barrier permit only has to cover the clone; the child is
        // registered, so the sweep's drain sees it as in-flight.
        drop(spawn_guard);
        if let Some(logs) = &self.logs {
            // handler.go:599-605.
            logs.process_started(&ProcessStart {
                pid,
                command,
                request_timeout: deadline,
                operation_id,
            });
        }
        tokio::spawn(supervise::run(
            self.registry.clone(),
            entry,
            child,
            controls,
            deadline,
            terminal,
            self.logs.clone(),
        ));
        Ok(Subscription { receiver })
    }

    pub async fn list(&self) -> Vec<pb::ProcessInfo> {
        self.registry
            .lock()
            .await
            .live
            .values()
            .map(|entry| entry.info.clone())
            .collect()
    }

    pub async fn connect(
        &self,
        selector: &pb::ProcessSelector,
    ) -> Result<Subscription, RuntimeError> {
        let mut registry = self.registry.lock().await;
        validate_selector(selector)?;
        registry
            .retained
            .retain(|_, exit| exit.expires > Instant::now());
        if let Some(entry) = registry
            .live
            .values()
            .find(|entry| matches_selector(selector, &entry.info))
            .cloned()
        {
            drop(registry);
            // Terminal publication and attachment share only this short lock, so
            // an exit during attachment returns End instead of losing the event.
            let mut subscribers = entry.subscribers.lock().await;
            let (sender, receiver) = mpsc::channel(2);
            sender
                .try_send(start_event(entry.info.pid))
                .map_err(|error| RuntimeError::Internal(error.to_string()))?;
            if let Some(end) = &subscribers.terminal {
                sender
                    .try_send(end_event(end.clone()))
                    .map_err(|error| RuntimeError::Internal(error.to_string()))?;
            } else {
                subscribers.senders.push(sender);
            }
            return Ok(Subscription { receiver });
        }
        if let Some(exit) = registry
            .retained
            .values()
            .find(|exit| matches_selector(selector, &exit.info))
        {
            let (sender, receiver) = mpsc::channel(2);
            sender
                .try_send(start_event(exit.info.pid))
                .map_err(|error| RuntimeError::Internal(error.to_string()))?;
            sender
                .try_send(end_event(exit.end.clone()))
                .map_err(|error| RuntimeError::Internal(error.to_string()))?;
            return Ok(Subscription { receiver });
        }
        Err(RuntimeError::NotFound)
    }

    async fn live(&self, selector: &pb::ProcessSelector) -> Result<Arc<Entry>, RuntimeError> {
        validate_selector(selector)?;
        self.registry
            .lock()
            .await
            .live
            .values()
            .find(|entry| matches_selector(selector, &entry.info))
            .cloned()
            .ok_or(RuntimeError::NotFound)
    }

    pub(crate) async fn select_input(
        &self,
        selector: &pb::ProcessSelector,
    ) -> Result<SelectedProcess, RuntimeError> {
        self.live(selector).await.map(SelectedProcess)
    }

    pub async fn send_input(
        &self,
        selector: &pb::ProcessSelector,
        input: &pb::ProcessInput,
    ) -> Result<(), RuntimeError> {
        self.select_input(selector).await?.send_input(input).await
    }

    pub async fn close_stdin(&self, selector: &pb::ProcessSelector) -> Result<(), RuntimeError> {
        let entry = self.live(selector).await?;
        if entry.is_pty {
            return Err(RuntimeError::FailedPrecondition(
                "cannot close stdin for PTY process — send Ctrl+D (0x04) instead".into(),
            ));
        }
        entry.stdin.lock().await.take();
        Ok(())
    }

    /// Apply the generated Update request; absent PTY options are a no-op.
    pub async fn update(&self, request: &pb::UpdateRequest) -> Result<(), RuntimeError> {
        let entry = self.live(&request.process).await?;
        if let Some(size) = request.pty.as_option() {
            let terminal = entry
                .pty
                .lock()
                .await
                .clone()
                .ok_or_else(|| RuntimeError::Internal("tty not assigned to process".into()))?;
            terminal
                .resize(size)
                .map_err(|error| RuntimeError::Internal(error.to_string()))?;
        }
        Ok(())
    }

    /// Oracle signal.go:17-26: the enum's wire number is accepted unchanged,
    /// but only SIGTERM/SIGKILL deliver; every other value refuses with the
    /// proto enum's String() in the message — the name for known values, the
    /// bare number otherwise (SIGNAL_UNSPECIFIED for the absent field).
    pub async fn send_signal(
        &self,
        selector: &pb::ProcessSelector,
        signal: i32,
    ) -> Result<(), RuntimeError> {
        let entry = self.live(selector).await?;
        let signal = match signal {
            15 => nix::sys::signal::Signal::SIGTERM,
            9 => nix::sys::signal::Signal::SIGKILL,
            0 => {
                return Err(RuntimeError::Unimplemented(
                    "invalid signal: SIGNAL_UNSPECIFIED".into(),
                ))
            }
            other => {
                return Err(RuntimeError::Unimplemented(format!(
                    "invalid signal: {other}"
                )))
            }
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        entry
            .control
            .send(supervise::Control { signal, reply })
            .await
            .map_err(|_| RuntimeError::NotFound)?;
        response.await.map_err(|_| RuntimeError::NotFound)?
    }

    /// Oracle upgrade.go:542-570 restoreTerminated: seed the retention cache
    /// from a handover blob. A PID that is LIVE again is skipped — a stale exit
    /// under a live PID would be served to the wrong process (upgrade.go:543-545).
    /// An absent end event is defaulted so retain always holds one
    /// (upgrade.go:548-553); the TTL runs from NOW (upgrade.go:559).
    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub async fn restore_retained(&self, entries: Vec<RetainedExit>) {
        let mut registry = self.registry.lock().await;
        for exit in entries {
            if registry.live.contains_key(&exit.pid) {
                continue;
            }
            let expires = Instant::now() + Duration::from_millis(exit.remaining_ms.max(0) as u64);
            registry.retained.insert(
                exit.pid,
                Retained {
                    info: pb::ProcessInfo {
                        pid: exit.pid,
                        tag: exit.tag,
                        ..Default::default()
                    },
                    end: exit.end.unwrap_or_default(),
                    expires,
                },
            );
        }
    }

    /// Oracle upgrade.go:254-268: the outgoing side. Only exits whose retention
    /// TTL has not run out are carried (rem <= 0 is skipped); each carries the
    /// remaining TTL so the incoming envd re-arms the same deadline.
    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub async fn export_retained(&self) -> Vec<RetainedExit> {
        let registry = self.registry.lock().await;
        registry
            .retained
            .values()
            .filter_map(|exit| {
                let remaining_ms = exit
                    .expires
                    .checked_duration_since(Instant::now())?
                    .as_millis() as i64;
                (remaining_ms > 0).then(|| RetainedExit {
                    pid: exit.info.pid,
                    tag: exit.info.tag.clone(),
                    end: Some(exit.end.clone()),
                    remaining_ms,
                })
            })
            .collect()
    }
}

async fn write_input(entry: &Entry, input: &pb::ProcessInput) -> Result<(), RuntimeError> {
    use pb::process_input::Input;
    let bytes = match &input.input {
        Some(Input::Stdin(_)) if entry.is_pty => {
            return Err(RuntimeError::FailedPrecondition(
                "tty assigned to process — input should be written to the pty, not the stdin"
                    .into(),
            ));
        }
        Some(Input::Stdin(bytes)) => bytes,
        Some(Input::Pty(bytes)) => {
            let terminal = entry.pty.lock().await.clone().ok_or_else(|| {
                RuntimeError::FailedPrecondition("tty not assigned to process — input should be written to the stdin, not the tty".into())
            })?;
            return terminal.write(bytes).await.map_err(input_error);
        }
        None => {
            return Err(RuntimeError::Unimplemented(
                "invalid input type <nil>".into(),
            ))
        }
    };
    let mut stdin = entry.stdin.lock().await;
    let pipe = stdin
        .as_mut()
        .ok_or_else(|| RuntimeError::FailedPrecondition("stdin not enabled or closed".into()))?;
    use tokio::io::AsyncWriteExt;
    pipe.write_all(bytes).await.map_err(input_error)
}

/// Go handler.go:161-163: `cmd` plus `args`, space joined.
fn user_command(config: &pb::ProcessConfig) -> String {
    let mut parts = Vec::with_capacity(1 + config.args.len());
    parts.push(config.cmd.clone());
    parts.extend(config.args.iter().cloned());
    parts.join(" ")
}

/// Oracle handler.go:317-328 (PTY) vs :593 (plain): both wrap the spawn
/// failure with the user command, and a PTY start additionally names the
/// resolved dir and the requested window size.
fn start_error(request: &pb::StartRequest, cwd: &str, error: impl std::fmt::Display) -> String {
    let command = user_command(&request.process);
    match request.pty.as_option() {
        Some(pty) => format!(
            "error starting pty with command '{command}' in dir '{cwd}' with '{}' cols and '{}' rows: {error}",
            pty.size.cols, pty.size.rows
        ),
        None => format!("error starting process '{command}': {error}"),
    }
}

/// start.go:184 wraps every admission refusal through StartErrorCode: a
/// frozen target stays retryable Unavailable (start_error.go:17-22), while an
/// armed caller deadline is the caller's own outcome, DeadlineExceeded
/// (spawn.go:197-201, start_error.go:32-34).
fn start_refusal(
    request: &pb::StartRequest,
    cwd: &str,
    error: crate::cgroups::spawn::SpawnError,
) -> RuntimeError {
    let message = start_error(request, cwd, &error);
    match error {
        crate::cgroups::spawn::SpawnError::DeadlineExceeded => {
            RuntimeError::DeadlineExceeded(message)
        }
        _ => RuntimeError::Unavailable(message),
    }
}

/// handler.go:593 (plain) and :326-328 (PTY) wrap the raw spawn failure
/// before start.go:184 hands it to StartErrorCode, so the wrapped text is
/// what the client sees on the wire. The tail is Go's `%w` rendering — the
/// full *fs.PathError text the sites attach (fork/exec /bin/sh at the spawn,
/// open /dev/ptmx at the pty), with the errno text in Go's own words
/// (errno_text) — and the connect code still follows the errno through the
/// PathError's Unwrap (start_error.go:24-31 exhaustion, :37-39 default
/// InvalidArgument; spawn::error walks the same chain).
fn wrapped_spawn_error(
    request: &pb::StartRequest,
    cwd: &str,
    error: &std::io::Error,
) -> RuntimeError {
    let mapped = spawn::error(error);
    let message = start_error(request, cwd, crate::errno_text::go_text(error));
    match mapped {
        RuntimeError::ResourceExhausted(_) => RuntimeError::ResourceExhausted(message),
        _ => RuntimeError::InvalidArgument(message),
    }
}

fn input_error(error: std::io::Error) -> RuntimeError {
    match error.kind() {
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected => {
            RuntimeError::FailedPrecondition(error.to_string())
        }
        _ => RuntimeError::Internal(error.to_string()),
    }
}

fn validate_selector(selector: &pb::ProcessSelector) -> Result<(), RuntimeError> {
    if selector.selector.is_none() {
        // Oracle service.go:171-200 getProcess default branch: %T names the
        // concrete pointer type of the unset oneof, so the refusal is this
        // exact text for every operation that resolves a selector.
        return Err(RuntimeError::Unimplemented(
            "invalid input type *process.ProcessSelector".into(),
        ));
    }
    Ok(())
}

fn matches_selector(selector: &pb::ProcessSelector, info: &pb::ProcessInfo) -> bool {
    match &selector.selector {
        Some(pb::process_selector::Selector::Pid(pid)) => *pid == info.pid,
        Some(pb::process_selector::Selector::Tag(tag)) => info.tag.as_ref() == Some(tag),
        None => false,
    }
}

fn start_event(pid: u32) -> pb::ProcessEvent {
    pb::ProcessEvent {
        event: Some(
            pb::process_event::StartEvent {
                pid,
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    }
}

fn end_event(end: pb::process_event::EndEvent) -> pb::ProcessEvent {
    pb::ProcessEvent {
        event: Some(end.into()),
        ..Default::default()
    }
}

async fn publish(entry: &Entry, event: pb::ProcessEvent, discard_if_blocked: bool) {
    let senders = {
        let mut subscribers = entry.subscribers.lock().await;
        subscribers.senders.retain(|sender| !sender.is_closed());
        if let Some(pb::process_event::Event::End(end)) = &event.event {
            subscribers.terminal = Some((**end).clone());
            std::mem::take(&mut subscribers.senders)
        } else {
            subscribers.senders.clone()
        }
    };
    for sender in senders {
        // At a process deadline no observer may hold lifecycle cleanup hostage.
        if discard_if_blocked {
            let _ = sender.try_send(event.clone());
        } else {
            let _ = sender.send(event.clone()).await;
        }
    }
}
