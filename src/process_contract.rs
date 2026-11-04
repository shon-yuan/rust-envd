//! Platform-neutral process contract: runtime errors, lifecycle log seams,
//! event subscriptions, retained exits and the pure helper functions every
//! process runtime shares.
//!
//! The Linux runtime in `crate::platform::linux::process` re-exports these
//! items so its behaviour and call sites are unchanged; the Windows runtime
//! and the shared Connect adapter (crate::rpc::process) consume the same
//! definitions.

use crate::proto::process as pb;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

/// One retained terminal event, in the handover's wire shape
/// (upgrade.go:254-268 out, :542-570 in). Defined here — not in
/// `crate::upgrade` — because tests/process_runtime.rs compiles this module
/// through the crate; `upgrade` re-exports it.
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

pub const RETENTION: Duration = Duration::from_secs(30);

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

// The integration harness at tests/process_runtime.rs compiles the Linux
// runtime directly without the production `logs::ProcessLogger` adapter, so
// the producer-side fields look unread in that compilation unit.
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
    pub receiver: mpsc::Receiver<pb::ProcessEvent>,
}

impl Subscription {
    pub async fn recv(&mut self) -> Option<pb::ProcessEvent> {
        self.receiver.recv().await
    }
}

type Sender = mpsc::Sender<pb::ProcessEvent>;

#[derive(Default)]
pub struct Subscribers {
    pub senders: Vec<Sender>,
    pub terminal: Option<pb::process_event::EndEvent>,
}

/// The part of a runtime entry `publish` needs. Implemented by each platform's
/// owned entry (and forwarded through `Arc`), so the publication algorithm
/// lives here without depending on a platform type.
pub trait SubscriptionHolder {
    fn subscribers(&self) -> &Mutex<Subscribers>;
}

impl<T: SubscriptionHolder + ?Sized> SubscriptionHolder for std::sync::Arc<T> {
    fn subscribers(&self) -> &Mutex<Subscribers> {
        (**self).subscribers()
    }
}

/// Go handler.go:161-163: `cmd` plus `args`, space joined.
pub fn user_command(config: &pb::ProcessConfig) -> String {
    let mut parts = Vec::with_capacity(1 + config.args.len());
    parts.push(config.cmd.clone());
    parts.extend(config.args.iter().cloned());
    parts.join(" ")
}

/// Oracle handler.go:317-328 (PTY) vs :593 (plain): both wrap the spawn
/// failure with the user command, and a PTY start additionally names the
/// resolved dir and the requested window size.
pub fn start_error(request: &pb::StartRequest, cwd: &str, error: impl std::fmt::Display) -> String {
    let command = user_command(&request.process);
    match request.pty.as_option() {
        Some(pty) => format!(
            "error starting pty with command '{command}' in dir '{cwd}' with '{}' cols and '{}' rows: {error}",
            pty.size.cols, pty.size.rows
        ),
        None => format!("error starting process '{command}': {error}"),
    }
}

pub fn input_error(error: std::io::Error) -> RuntimeError {
    match error.kind() {
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected => {
            RuntimeError::FailedPrecondition(error.to_string())
        }
        _ => RuntimeError::Internal(error.to_string()),
    }
}

/// Oracle signal.go:17-26: only SIGTERM/SIGKILL deliver; every other value
/// refuses with the proto enum's String() in the message — the name for known
/// values (including SIGNAL_UNSPECIFIED for the absent field), the bare
/// number otherwise.
pub fn signal_refusal(signal: i32) -> RuntimeError {
    match signal {
        0 => RuntimeError::Unimplemented("invalid signal: SIGNAL_UNSPECIFIED".into()),
        other => RuntimeError::Unimplemented(format!("invalid signal: {other}")),
    }
}

pub fn validate_selector(selector: &pb::ProcessSelector) -> Result<(), RuntimeError> {
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

pub fn matches_selector(selector: &pb::ProcessSelector, info: &pb::ProcessInfo) -> bool {
    match &selector.selector {
        Some(pb::process_selector::Selector::Pid(pid)) => *pid == info.pid,
        Some(pb::process_selector::Selector::Tag(tag)) => info.tag.as_ref() == Some(tag),
        None => false,
    }
}

pub fn start_event(pid: u32) -> pb::ProcessEvent {
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

pub fn end_event(end: pb::process_event::EndEvent) -> pb::ProcessEvent {
    pb::ProcessEvent {
        event: Some(end.into()),
        ..Default::default()
    }
}

pub async fn publish<E: SubscriptionHolder + Sync>(
    entry: &E,
    event: pb::ProcessEvent,
    discard_if_blocked: bool,
) {
    let senders = {
        let mut subscribers = entry.subscribers().lock().await;
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
