//! Plain and PTY process runtime for the pinned envd contract on Windows.
//!
//! The registry, connect/retention and start ordering are the Linux algorithms
//! one-for-one (`platform/linux/process/mod.rs`), with `CreateProcessW`
//! instead of fork/exec. A child is created suspended, assigned to a
//! per-process kill-on-close Job and to the sandbox Job used by hierarchy
//! freeze, then resumed. `WorkloadRegistry::begin_spawn` admits the spawn and
//! the child is registered before the barrier permit is released, so a
//! `/freeze` sweep sees the same in-flight window Linux does.
//!
//! Declared deviations from Linux: no shell wrapper (the request command is
//! the executable), no live-upgrade handover (`export_processes` /
//! `readopt_processes` are absent), ConPTY bytes are ConPTY-encoded rather
//! than termios bytes, and a requested account other than the daemon's own
//! SID is refused because no credential provider is wired.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};
use tokio::time::Instant;

use crate::exec_context::{Account, Defaults, Environment};
use crate::platform::windows::account;
use crate::platform::windows::job::JobHandle;
use crate::platform::windows::registry::{WorkloadClass, WorkloadRegistry};
use crate::platform::ProcessBackend;
use crate::process_api::{BoxFuture, ProcessRuntimeApi, ProcessSubscription, SelectedInput};
use crate::proto::process as pb;

pub(crate) use crate::process_contract::{
    end_event, input_error, matches_selector, publish, signal_refusal, start_error, start_event,
    user_command, validate_selector, Subscribers, RETENTION,
};
pub use crate::process_contract::{
    ProcessEnd, ProcessLogSink, ProcessStart, RetainedExit, RuntimeError, Subscription,
};

mod conpty;
pub(crate) mod handle;
mod pipe;
mod spawn;
pub(crate) mod supervise;

use conpty::ConPty;
use handle::OwnedHandle;
use pipe::StdinWriter;

/// The exit code reported for a hard kill (`TerminateProcess` /
/// `TerminateJobObject`). It never reaches the wire: the supervisor reports
/// `-1`/`exited=false` for a kill it issued.
pub(crate) const KILL_EXIT_CODE: u32 = 1;

/// Retains the selected process identity across tag or PID reuse.
pub(crate) struct SelectedProcess(Arc<Entry>);

impl SelectedProcess {
    pub(crate) async fn send_input(&self, input: &pb::ProcessInput) -> Result<(), RuntimeError> {
        write_input(&self.0, input).await
    }
}

pub(crate) struct Entry {
    pub info: pb::ProcessInfo,
    pub stdin: Mutex<Option<StdinWriter>>,
    pub pty: Mutex<Option<Arc<ConPty>>>,
    pub is_pty: bool,
    pub subscribers: Mutex<Subscribers>,
    pub control: mpsc::Sender<supervise::Control>,
    /// The child's process handle. `supervise` reads it (wait, terminate,
    /// duplicate for the grace timer); the entry keeps it alive.
    pub process: OwnedHandle,
    /// The per-process kill-on-close Job. `TerminateJobObject` reaps the
    /// process tree without touching any other workload.
    pub job: Arc<JobHandle>,
    /// Go handler byte counters, logged once when the process ends.
    pub stdout_bytes: Arc<AtomicI64>,
    pub stderr_bytes: Arc<AtomicI64>,
    pub pty_bytes: Arc<AtomicI64>,
    /// Set once a Term signal is accepted for this child. A console control
    /// event delivered while the child is still in loader initialization
    /// aborts it with `STATUS_DLL_INIT_FAILED` instead of the usual
    /// `STATUS_CONTROL_C_EXIT`; the flag is what lets the terminal synthesis
    /// attribute that exit code to the signal we sent.
    pub term_accepted: AtomicBool,
    pub operation_id: Option<String>,
}

impl crate::process_contract::SubscriptionHolder for Entry {
    fn subscribers(&self) -> &Mutex<Subscribers> {
        &self.subscribers
    }
}

struct Retained {
    info: pb::ProcessInfo,
    end: pb::process_event::EndEvent,
    expires: Instant,
}

#[derive(Default)]
pub(crate) struct Registry {
    live: HashMap<u32, Arc<Entry>>,
    retained: HashMap<u32, Retained>,
}

/// The runtime handed to the shared Connect adapter. Cloning shares the
/// process registry, the workload registry and the sandbox Job.
#[derive(Clone)]
pub struct WindowsProcessRuntime {
    registry: Arc<Mutex<Registry>>,
    account: Account,
    defaults: Defaults,
    environment: Environment,
    daemon_path: Option<String>,
    logs: Option<Arc<dyn ProcessLogSink>>,
    workload: Arc<WorkloadRegistry>,
    sandbox_job: Arc<JobHandle>,
    /// How long a finished process stays reachable through `Connect`. The
    /// oracle's 30 s window; tests override it to avoid real-time waits.
    retention: Duration,
}

impl WindowsProcessRuntime {
    /// `workload` is the daemon-wide registry a `/freeze` sweep drains;
    /// `sandbox_job` is the Job hierarchy freeze enumerates. The same
    /// `JobHandle` should be passed to
    /// `WindowsResourceBackend::with_job`.
    pub fn new(
        account: Account,
        defaults: Defaults,
        environment: Environment,
        daemon_path: Option<String>,
        workload: Arc<WorkloadRegistry>,
        sandbox_job: Arc<JobHandle>,
    ) -> Self {
        Self {
            registry: Arc::new(Mutex::new(Registry::default())),
            account,
            defaults,
            environment,
            daemon_path,
            logs: None,
            workload,
            sandbox_job,
            retention: RETENTION,
        }
    }

    /// Inject the shared process lifecycle logger; absent by default so
    /// direct runtime use stays silent.
    pub fn with_process_logs(&self, logs: Arc<dyn ProcessLogSink>) -> Self {
        Self {
            logs: Some(logs),
            ..self.clone()
        }
    }

    /// Shortens the retained-exit window so tests can exercise expiry without
    /// waiting the production 30 s.
    #[cfg(test)]
    pub(crate) fn with_retention_for_tests(&self, retention: Duration) -> Self {
        Self {
            retention,
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

    /// Oracle upgrade.go:254-268, the outgoing side: every retained exit whose
    /// TTL has not run out, with the remaining TTL so the successor re-arms
    /// the same deadline. A live exit is kept even when its deadline is
    /// within the current tick; zero or negative leftovers are skipped.
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

    /// Oracle upgrade.go:488-493, the incoming side: re-arm the retention
    /// cache with each carried remaining TTL, skipping a pid that is live in
    /// this process. Each restored entry gets the same expiry cleanup the
    /// supervisor schedules, so the map cannot grow without bound.
    pub async fn restore_retained(&self, entries: Vec<RetainedExit>) {
        let mut restored = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for exit in entries {
                if registry.live.contains_key(&exit.pid) {
                    continue;
                }
                let remaining = Duration::from_millis(exit.remaining_ms.max(0) as u64);
                let expires = Instant::now() + remaining;
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
                restored.push((exit.pid, remaining));
            }
        }
        for (pid, remaining) in restored {
            let registry = self.registry.clone();
            tokio::spawn(async move {
                tokio::time::sleep(remaining).await;
                let mut registry = registry.lock().await;
                if registry
                    .retained
                    .get(&pid)
                    .is_some_and(|exit| exit.expires <= Instant::now())
                {
                    registry.retained.remove(&pid);
                }
            });
        }
    }

    pub async fn start(
        &self,
        request: pb::StartRequest,
        deadline: Option<Duration>,
    ) -> Result<Subscription, RuntimeError> {
        self.start_with_operation_id(request, deadline, None).await
    }

    pub async fn start_with_operation_id(
        &self,
        request: pb::StartRequest,
        deadline: Option<Duration>,
        operation_id: Option<String>,
    ) -> Result<Subscription, RuntimeError> {
        self.start_inner(request, deadline, operation_id)
            .await
            .map(|(subscription, _)| subscription)
    }

    async fn start_inner(
        &self,
        request: pb::StartRequest,
        deadline: Option<Duration>,
        operation_id: Option<String>,
    ) -> Result<(Subscription, pb::ProcessInfo), RuntimeError> {
        // Go handler.go:161-163: the user-facing command, without a wrapper.
        let command = user_command(&request.process);
        // handler.go:236-243 resolves and stats the cwd BEFORE any spawn
        // attempt; only NotFound gets the bespoke text, every other stat
        // failure falls through to the spawn error below.
        let cwd = self
            .defaults
            .resolve_path(request.process.cwd.as_deref().unwrap_or(""), &self.account)
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        if let Err(error) = std::fs::metadata(&cwd) {
            if error.kind() == io::ErrorKind::NotFound {
                return Err(RuntimeError::InvalidArgument(format!(
                    "cwd '{cwd}' does not exist"
                )));
            }
        }
        // start.go:40-44 arms the request context before any spawn.
        let deadline_at = deadline.map(|timeout| Instant::now() + timeout);
        let permit = self.admit(&request, &cwd, deadline_at).await?;
        // exec.CommandContext refuses pre-fork once procCtx is done.
        if deadline_at.is_some_and(|at| at <= Instant::now()) {
            return Err(RuntimeError::DeadlineExceeded(start_error(
                &request,
                &cwd,
                "context deadline exceeded",
            )));
        }
        // No credential provider is wired: CreateProcessW always runs as the
        // daemon. A request for another real account is refused before any
        // child exists (declared deviation from Linux setuid).
        if !account::is_current_user(&self.account.name) {
            return Err(RuntimeError::FailedPrecondition(format!(
                "cannot start process as user '{}': the daemon can only run processes as its own account",
                self.account.name
            )));
        }
        // Acquire before spawning: cancellation at a lock await must not leave
        // an unregistered child without its supervisor.
        let mut registry = self.registry.lock().await;
        let spawned = spawn::spawn(self, &request, &cwd)?;
        let spawn::Spawned {
            pid,
            process,
            job,
            stdin,
            stdout,
            stderr,
            pty,
        } = spawned;
        // Registered before the barrier permit drops, so a concurrent
        // `/freeze` sweeps this pid like any other in-flight child.
        let class = if pty.is_some() {
            WorkloadClass::Pty
        } else {
            WorkloadClass::User
        };
        self.workload.register(pid, class, request.tag.clone());
        // The barrier permit only has to cover the spawn and registration.
        drop(permit);
        let (sender, receiver) = mpsc::channel(1);
        if let Err(error) = sender.try_send(start_event(pid)) {
            self.workload.unregister(pid);
            let _ = job.terminate(KILL_EXIT_CODE);
            return Err(RuntimeError::Internal(error.to_string()));
        }
        let (control, controls) = mpsc::channel(1);
        let info = pb::ProcessInfo {
            pid,
            config: request.process.clone(),
            tag: request.tag,
            ..Default::default()
        };
        let entry = Arc::new(Entry {
            info: info.clone(),
            stdin: Mutex::new(stdin),
            pty: Mutex::new(pty.clone()),
            is_pty: pty.is_some(),
            subscribers: Mutex::new(Subscribers {
                senders: vec![sender],
                terminal: None,
            }),
            control,
            process,
            job,
            stdout_bytes: Arc::new(AtomicI64::new(0)),
            stderr_bytes: Arc::new(AtomicI64::new(0)),
            pty_bytes: Arc::new(AtomicI64::new(0)),
            term_accepted: AtomicBool::new(false),
            operation_id: operation_id.clone(),
        });
        registry.retained.retain(|old_pid, exit| {
            *old_pid != pid
                && (entry.info.tag.is_none() || exit.info.tag != entry.info.tag)
                && exit.expires > Instant::now()
        });
        registry.live.insert(pid, entry.clone());
        drop(registry);
        if let Some(logs) = &self.logs {
            logs.process_started(&ProcessStart {
                pid,
                command,
                request_timeout: deadline,
                operation_id,
            });
        }
        tokio::spawn(supervise::run(
            self.registry.clone(),
            self.workload.clone(),
            entry,
            controls,
            deadline,
            supervise::Io {
                stdout,
                stderr,
                pty,
            },
            self.logs.clone(),
            self.retention,
        ));
        Ok((Subscription { receiver }, info))
    }

    /// The workload spawn gate. A frozen target stays retryable Unavailable;
    /// an armed caller deadline is the caller's own DeadlineExceeded.
    async fn admit(
        &self,
        request: &pb::StartRequest,
        cwd: &str,
        deadline_at: Option<Instant>,
    ) -> Result<crate::platform::windows::registry::SpawnPermit, RuntimeError> {
        let admission = self.workload.begin_spawn();
        let refused = |error| RuntimeError::Unavailable(start_error(request, cwd, error));
        match deadline_at {
            Some(at) => {
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(at) => Err(RuntimeError::DeadlineExceeded(
                        start_error(request, cwd, "context deadline exceeded"),
                    )),
                    result = admission => result.map_err(refused),
                }
            }
            None => admission.await.map_err(refused),
        }
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
            // Terminal publication and attachment share this lock, so an exit
            // during attachment returns End instead of losing the event.
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

    /// Only SIGTERM/SIGKILL deliver; every other value refuses with the proto
    /// enum's String() in the message. Delivery returns once the request is
    /// accepted, not when the child exits.
    pub async fn send_signal(
        &self,
        selector: &pb::ProcessSelector,
        signal: i32,
    ) -> Result<(), RuntimeError> {
        let entry = self.live(selector).await?;
        let signal = match signal {
            15 => supervise::Signal::Term,
            9 => supervise::Signal::Kill,
            other => return Err(signal_refusal(other)),
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        entry
            .control
            .send(supervise::Control { signal, reply })
            .await
            .map_err(|_| RuntimeError::NotFound)?;
        response.await.map_err(|_| RuntimeError::NotFound)?
    }

    /// Blocking bridge for the P0 `ProcessBackend` seam. A current-thread
    /// runtime has no sound answer (it cannot progress while blocked), so that
    /// case reports an error. With no ambient runtime a small process-wide
    /// multi-thread runtime is kept for the process lifetime, because a
    /// temporary runtime would drop the supervision tasks it just spawned.
    fn block_on<F: std::future::Future>(&self, future: F) -> io::Result<F::Output> {
        static BACKEND_RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
            std::sync::OnceLock::new();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                Ok(tokio::task::block_in_place(|| handle.block_on(future)))
            }
            Ok(_) => Err(io::Error::other(
                "cannot run the blocking Windows process backend inside a current-thread runtime",
            )),
            Err(_) => match BACKEND_RUNTIME.get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
            }) {
                Ok(runtime) => Ok(runtime.block_on(future)),
                Err(error) => Err(io::Error::other(error.clone())),
            },
        }
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
    pipe.write_all(bytes).await.map_err(input_error)
}

/// `start.go:184` wraps every spawn failure through the shared start-error
/// templates. Windows classifies the OS error exactly where Linux walks its
/// errno chain: memory/quota/resource exhaustion is retryable
/// ResourceExhausted; every other spawn failure is InvalidArgument.
fn wrapped_spawn_error(request: &pb::StartRequest, cwd: &str, error: &io::Error) -> RuntimeError {
    // ERROR_NOT_ENOUGH_MEMORY, ERROR_OUTOFMEMORY, ERROR_NO_SYSTEM_RESOURCES,
    // ERROR_NOT_ENOUGH_QUOTA.
    const EXHAUSTION: [i32; 4] = [8, 14, 1450, 1816];
    let message = start_error(request, cwd, error);
    if error
        .raw_os_error()
        .is_some_and(|code| EXHAUSTION.contains(&code))
    {
        RuntimeError::ResourceExhausted(message)
    } else {
        RuntimeError::InvalidArgument(message)
    }
}

fn backend_error(error: RuntimeError) -> io::Error {
    let kind = match &error {
        RuntimeError::NotFound => io::ErrorKind::NotFound,
        RuntimeError::InvalidArgument(_) => io::ErrorKind::InvalidInput,
        RuntimeError::Unimplemented(_) => io::ErrorKind::Unsupported,
        RuntimeError::DeadlineExceeded(_) => io::ErrorKind::TimedOut,
        RuntimeError::FailedPrecondition(_)
        | RuntimeError::Unavailable(_)
        | RuntimeError::ResourceExhausted(_)
        | RuntimeError::Internal(_) => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.to_string())
}

/// The P0 platform trait: thin delegates over the Connect-facing runtime.
impl ProcessBackend for WindowsProcessRuntime {
    fn start(&self, config: &pb::ProcessConfig) -> io::Result<pb::ProcessInfo> {
        let request = pb::StartRequest {
            process: Some(config.clone()).into(),
            ..Default::default()
        };
        let (_subscription, info) = self
            .block_on(self.start_inner(request, None, None))?
            .map_err(backend_error)?;
        Ok(info)
    }

    fn list(&self) -> io::Result<Vec<pb::ProcessInfo>> {
        self.block_on(WindowsProcessRuntime::list(self))
    }

    fn terminate(&self, pid: u32) -> io::Result<()> {
        let selector = pb::ProcessSelector {
            selector: Some(pb::process_selector::Selector::Pid(pid)),
            ..Default::default()
        };
        self.block_on(self.send_signal(&selector, 9))?
            .map_err(backend_error)
    }
}

impl ProcessRuntimeApi for WindowsProcessRuntime {
    fn with_context(
        &self,
        defaults: Defaults,
        environment: Environment,
    ) -> Arc<dyn ProcessRuntimeApi> {
        Arc::new(WindowsProcessRuntime::with_context(
            self,
            defaults,
            environment,
        ))
    }

    fn with_account(&self, account: Account) -> Arc<dyn ProcessRuntimeApi> {
        Arc::new(WindowsProcessRuntime::with_account(self, account))
    }

    fn list(&self) -> BoxFuture<'_, Vec<pb::ProcessInfo>> {
        Box::pin(WindowsProcessRuntime::list(self))
    }

    fn start(
        &self,
        request: pb::StartRequest,
        deadline: Option<Duration>,
        operation_id: Option<String>,
    ) -> BoxFuture<'_, Result<Box<dyn ProcessSubscription>, RuntimeError>> {
        Box::pin(async move {
            let (subscription, _) = self.start_inner(request, deadline, operation_id).await?;
            Ok(Box::new(subscription) as Box<dyn ProcessSubscription>)
        })
    }

    fn connect<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
    ) -> BoxFuture<'a, Result<Box<dyn ProcessSubscription>, RuntimeError>> {
        Box::pin(async move {
            let subscription = WindowsProcessRuntime::connect(self, selector).await?;
            Ok(Box::new(subscription) as Box<dyn ProcessSubscription>)
        })
    }

    fn select_input<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
    ) -> BoxFuture<'a, Result<Box<dyn SelectedInput>, RuntimeError>> {
        Box::pin(async move {
            let selected = WindowsProcessRuntime::select_input(self, selector).await?;
            Ok(Box::new(selected) as Box<dyn SelectedInput>)
        })
    }

    fn send_input<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
        input: &'a pb::ProcessInput,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(WindowsProcessRuntime::send_input(self, selector, input))
    }

    fn close_stdin<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(WindowsProcessRuntime::close_stdin(self, selector))
    }

    fn send_signal<'a>(
        &'a self,
        selector: &'a pb::ProcessSelector,
        signal: i32,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(WindowsProcessRuntime::send_signal(self, selector, signal))
    }

    fn update<'a>(
        &'a self,
        request: &'a pb::UpdateRequest,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(WindowsProcessRuntime::update(self, request))
    }
}

impl ProcessSubscription for Subscription {
    fn recv(&mut self) -> BoxFuture<'_, Option<pb::ProcessEvent>> {
        Box::pin(Subscription::recv(self))
    }
}

impl SelectedInput for SelectedProcess {
    fn send_input<'a>(
        &'a self,
        input: &'a pb::ProcessInput,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(SelectedProcess::send_input(self, input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_context::{Defaults, Environment};
    use crate::platform::windows::account;
    use crate::platform::windows::job::JobHandle;
    use crate::platform::windows::registry::WorkloadRegistry;

    fn test_runtime(retention: Duration) -> WindowsProcessRuntime {
        let account =
            account::lookup(&account::current_user_name()).expect("the service account resolves");
        WindowsProcessRuntime::new(
            account,
            Defaults::default(),
            Environment::default(),
            std::env::var("PATH").ok(),
            Arc::new(WorkloadRegistry::new()),
            Arc::new(JobHandle::create().expect("the sandbox Job Object")),
        )
        .with_retention_for_tests(retention)
    }

    fn exit_request(tag: &str) -> pb::StartRequest {
        serde_json::from_value(serde_json::json!({
            "process": {"cmd": "cmd.exe", "args": ["/d", "/c", "exit 0"]},
            "tag": tag,
            "stdin": false,
        }))
        .expect("start request")
    }

    fn pid_selector(pid: u32) -> pb::ProcessSelector {
        pb::ProcessSelector {
            selector: Some(pb::process_selector::Selector::Pid(pid)),
            ..Default::default()
        }
    }

    fn tag_selector(tag: &str) -> pb::ProcessSelector {
        pb::ProcessSelector {
            selector: Some(pb::process_selector::Selector::Tag(tag.to_owned())),
            ..Default::default()
        }
    }

    /// Runs `cmd.exe` to completion and returns its pid plus the End event.
    async fn exit(
        runtime: &WindowsProcessRuntime,
        tag: &str,
    ) -> (u32, pb::process_event::EndEvent) {
        let mut subscription = runtime
            .start(exit_request(tag), None)
            .await
            .expect("a start request for cmd.exe succeeds");
        let mut pid = 0;
        while let Some(event) = subscription.recv().await {
            match event.event.expect("an event payload") {
                pb::process_event::Event::Start(start) => pid = start.pid,
                pb::process_event::Event::End(end) => return (pid, *end),
                _ => {}
            }
        }
        panic!("the stream ended without a terminal event");
    }

    /// The retained window covers a finished process: `Connect` resolves by
    /// pid and by tag while it is open. The 30 s production window keeps this
    /// assertion free of scheduler-timing margins.
    #[tokio::test]
    async fn a_finished_process_is_reachable_during_the_retention_window() {
        let runtime = test_runtime(Duration::from_secs(30));
        let (pid, end) = exit(&runtime, "windows-retained").await;
        assert!(pid > 0);
        assert_eq!(end.exit_code, 0);

        for selector in [pid_selector(pid), tag_selector("windows-retained")] {
            let mut retained = runtime
                .connect(&selector)
                .await
                .expect("the exit is retained");
            let mut events = 0;
            while let Some(event) = retained.recv().await {
                events += 1;
                if matches!(event.event, Some(pb::process_event::Event::End(_))) {
                    break;
                }
            }
            assert_eq!(events, 2, "a retained stream is Start then End");
        }
    }

    /// The retained window is bounded: once it closes the same selectors are
    /// `NotFound`. A bounded poll absorbs the cleanup task's scheduling delay
    /// instead of depending on a fixed sleep margin.
    #[tokio::test]
    async fn retained_exits_expire_after_the_retention_window() {
        let runtime = test_runtime(Duration::from_millis(300));
        let (pid, _) = exit(&runtime, "windows-expired").await;
        for _ in 0..40 {
            let gone = matches!(
                runtime.connect(&pid_selector(pid)).await,
                Err(RuntimeError::NotFound)
            ) && matches!(
                runtime.connect(&tag_selector("windows-expired")).await,
                Err(RuntimeError::NotFound)
            );
            if gone {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the retained exit did not expire within the test bound");
    }

    /// A start that reuses a retained tag evicts the previous exit when both
    /// starts are sequential, so the selector resolves to the newest exit.
    #[tokio::test]
    async fn a_reused_tag_prunes_the_previous_retained_exit() {
        let runtime = test_runtime(Duration::from_secs(30));
        let _ = exit(&runtime, "windows-tag").await;
        let (second, _) = exit(&runtime, "windows-tag").await;

        let mut retained = runtime
            .connect(&tag_selector("windows-tag"))
            .await
            .expect("the newest exit is retained");
        let mut pid = 0;
        while let Some(event) = retained.recv().await {
            match event.event.expect("an event payload") {
                pb::process_event::Event::Start(start) => pid = start.pid,
                pb::process_event::Event::End(_) => break,
                _ => {}
            }
        }
        assert_eq!(pid, second, "the reused tag resolves to the newest exit");
    }
}
