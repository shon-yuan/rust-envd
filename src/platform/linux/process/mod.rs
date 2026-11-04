//! Plain and PTY process runtime for the pinned envd contract.
//!
//! The caller owns execution admission. Cgroup admission is supplied through
//! the daemon wiring. Live-upgrade adoption works like the oracle's
//! (upgrade.go:156-250 out, readopt.go in): the outgoing envd dup3's each
//! live child's daemon-side pipe ends onto fixed handover slots with CLOEXEC
//! cleared, the blob carries the descriptor numbers plus each process's
//! config, and the incoming envd relocates them off the slots and rebuilds
//! readers, stdin and terminals around the inherited descriptors — `pipe.rs`
//! and `supervise::run_adopted`. tokio never reaps another pid (every reap
//! is pid-specific: the registered-child reaper and the orphan drain both
//! `waitpid(pid)`), so a re-adopted child's exit status stays recoverable
//! with a pid-specific wait.
use crate::exec_context::{Account, Defaults, Environment};
use crate::proto::process as pb;

pub use crate::process_contract::{
    ProcessEnd, ProcessLogSink, ProcessStart, RetainedExit, RuntimeError, Subscription,
};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::time::Instant;

mod pipe;
mod pty;
mod spawn;
pub(crate) mod supervise;
/// Shared with the handover blob writer, whose refusal body carries the same
/// Go `*fs.PathError` Display shape (upgrade.go:270-276).
#[allow(unused_imports)] // Unused only where the test harness compiles this file directly.
pub(crate) use spawn::path_text_error;

pub(crate) use crate::process_contract::{
    end_event, input_error, matches_selector, publish, signal_refusal, start_error, start_event,
    user_command, validate_selector, Subscribers, RETENTION,
};

/// Go upgrade.go `fdBase` (200): the first handover slot. Each carried
/// process gets five slots (`fdBase + i*5`): stdout, stderr, stdin, tty, and
/// one unused — matching the oracle's layout so a chained upgrade between a
/// Go and a Rust envd reuses the same free window.
const HANDOVER_FD_BASE: RawFd = 200;

struct Entry {
    info: pb::ProcessInfo,
    stdin: Mutex<Option<pipe::StdinPipe>>,
    pty: Mutex<Option<Arc<pty::Terminal>>>,
    is_pty: bool,
    subscribers: Mutex<Subscribers>,
    control: mpsc::Sender<supervise::Control>,
    /// Go handler byte counters, logged once when the process ends.
    stdout_bytes: Arc<AtomicI64>,
    stderr_bytes: Arc<AtomicI64>,
    pty_bytes: Arc<AtomicI64>,
    operation_id: Option<String>,
    /// Daemon-side duplicates of the child's stdout/stderr pipe ends, taken
    /// at spawn with CLOEXEC set. The supervise task owns the original pipes;
    /// these dups are what a handover relocates (Go `HandoverFds` reads the
    /// handler's files). Absent for PTY children and dropped on end — the
    /// kernel object lives as long as either handle does, and the entry
    /// leaves the live map when the process ends, matching Go's handler
    /// lifetime. Adopted entries keep the relocated endpoints here too, so a
    /// CHAINED handover carries them again exactly like Go's readopted
    /// handler (whose `stdoutF`/`stderrF` stay live); their supervision
    /// registers this same descriptor number rather than allocating a
    /// duplicate, so adoption uses one fd per stream like Go.
    stdout_dup: Option<Arc<std::os::fd::OwnedFd>>,
    stderr_dup: Option<Arc<std::os::fd::OwnedFd>>,
    /// The absolute kill deadline (request timeout) this process was started
    /// with, as Go's `Deadline()` reports it. Set for started processes at
    /// spawn; for adopted ones `run_adopted` records it when the carried
    /// timer actually arms after the thaw (readopt.go `setDeadline`), so a
    /// frozen window never burns the remaining time down.
    deadline_at: Mutex<Option<Instant>>,
}

impl crate::process_contract::SubscriptionHolder for Entry {
    fn subscribers(&self) -> &Mutex<Subscribers> {
        &self.subscribers
    }
}

/// The handover slots a successful `export_processes` relocated. Dropping
/// this guard closes them: on the exec path the process image is replaced
/// before the guard ever drops (the slots and their originals vanish
/// together), and on any earlier failure the slots close so a retry sees a
/// clean target window (upgrade.go:228-241 and the deferred cleanup).
pub struct HandoverFdGuard(Vec<RawFd>);

impl Drop for HandoverFdGuard {
    fn drop(&mut self) {
        for fd in &self.0 {
            let _ = nix::unistd::close(*fd);
        }
    }
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
        // Handover duplicates of the pipe ends the supervise task is about to
        // own (Go `HandoverFds` reads the handler's own files): CLOEXEC-set
        // dups that only a handover's dupKeep re-clears onto its slots.
        fn dup_end<T: AsRawFd>(pipe: Option<&T>) -> Option<std::os::fd::OwnedFd> {
            let fd = pipe?.as_raw_fd();
            match nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(0)) {
                // SAFETY: F_DUPFD_CLOEXEC allocated a descriptor this entry
                // solely owns; it closes when the entry leaves the live map.
                Ok(fresh) => Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fresh) }),
                Err(_) => None,
            }
        }
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
            // A PTY child has no daemon-side stdin pipe (Go's start keeps
            // h.stdin nil and writes go to the master), so its handover
            // carries only the tty — upgrade.go:218-250 sets stdin nil for
            // cgType "pty".
            stdin: Mutex::new(if terminal.is_some() {
                child.stdin.take(); // dropped: the slave dup is not a pipe
                None
            } else {
                child.stdin.take().map(pipe::StdinPipe::Started)
            }),
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
            stdout_dup: dup_end(child.stdout.as_ref()).map(Arc::new),
            stderr_dup: dup_end(child.stderr.as_ref()).map(Arc::new),
            deadline_at: Mutex::new(deadline_at),
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

    /// The outgoing handover's process table (upgrade.go:218-250): each live
    /// child carries its pid, tag, config, remaining kill deadline and the
    /// daemon-side descriptors dup3'd onto the fixed slot window
    /// (`fdBase + i*5`) with CLOEXEC cleared, so they ride the execve. The
    /// snapshot takes the registry lock; the spawn barrier — the freeze hold
    /// `do_upgrade` already holds — keeps concurrent Start admissions out of
    /// the snapshot→exec window, the way Go's snapshotMu does. The caller
    /// MUST keep the returned guard alive until exec (or drop it on failure,
    /// closing the slots so a retry starts clean).
    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub async fn export_processes(
        &self,
    ) -> Result<(Vec<crate::proto::upgrade::HandoverProc>, HandoverFdGuard), String> {
        let registry = self.registry.lock().await;
        // Sorted pid order: the slot each process gets depends on its index,
        // so the assignment is deterministic where Go's map range was not.
        // Unobservable either way — the blob carries the actual fd numbers.
        let mut pids: Vec<u32> = registry.live.keys().copied().collect();
        pids.sort_unstable();
        let mut carried = Vec::with_capacity(pids.len());
        let mut relocated: Vec<RawFd> = Vec::new();
        for (index, pid) in pids.iter().enumerate() {
            let entry = &registry.live[pid];
            let slot = HANDOVER_FD_BASE + (index as RawFd) * 5;
            let stdout = entry
                .stdout_dup
                .as_ref()
                .map(|fd| fd.as_ref().as_raw_fd())
                .unwrap_or(-1);
            let stderr = entry
                .stderr_dup
                .as_ref()
                .map(|fd| fd.as_ref().as_raw_fd())
                .unwrap_or(-1);
            let stdin = entry
                .stdin
                .lock()
                .await
                .as_ref()
                .map(pipe::StdinPipe::raw_fd)
                .unwrap_or(-1);
            let tty = entry
                .pty
                .lock()
                .await
                .as_ref()
                .and_then(|terminal| terminal.raw_fd())
                .unwrap_or(-1);
            let mut carried_proc = crate::proto::upgrade::HandoverProc {
                pid: *pid,
                tag: entry.info.tag.clone().unwrap_or_default(),
                has_tag: entry.info.tag.is_some(),
                cg_type: if entry.is_pty { "pty" } else { "user" }.into(),
                config: entry.info.config.clone(),
                stdout_fd: -1,
                stderr_fd: -1,
                stdin_fd: -1,
                tty_fd: -1,
                timeout_ms: 0,
                ..Default::default()
            };
            let endpoints = [stdout, stderr, stdin, tty];
            for (which, old) in endpoints.iter().enumerate() {
                let target = slot + which as RawFd;
                match dup_keep(*old, target) {
                    Ok(fd) => {
                        match which {
                            0 => carried_proc.stdout_fd = fd,
                            1 => carried_proc.stderr_fd = fd,
                            2 => carried_proc.stdin_fd = fd,
                            _ => carried_proc.tty_fd = fd,
                        }
                        if fd >= 0 {
                            relocated.push(fd);
                        }
                    }
                    Err(error) => {
                        // Close the slots already relocated so a retry sees
                        // the clean target window (upgrade.go:228-237).
                        for fd in relocated {
                            let _ = nix::unistd::close(fd);
                        }
                        return Err(error);
                    }
                }
            }
            // Carry the remaining timeout; a deadline already in the past is
            // clamped to 1ms (kill ASAP) (upgrade.go:244-250).
            if let Some(at) = entry.deadline_at.lock().await.as_ref() {
                let remaining = at.checked_duration_since(Instant::now());
                carried_proc.timeout_ms = match remaining {
                    Some(remaining) if remaining.as_millis() > 0 => remaining.as_millis() as i64,
                    _ => 1,
                };
            }
            carried.push(carried_proc);
        }
        Ok((carried, HandoverFdGuard(relocated)))
    }

    /// The incoming handover's re-adoption loop (readopt.go `ResumeFromHandover`
    /// per-process section): relocate each carried descriptor off the slot
    /// window, rebuild the entry around the inherited descriptors, register
    /// it and start the adopted supervision (readers, pid-specific reaper,
    /// thaw-gated kill timer). Returns (adopted, failed); a failure is
    /// logged, not fatal — the workload keeps the rest of its handover.
    #[allow(dead_code)] // Unused only where the test harness compiles this file directly.
    pub async fn readopt_processes(
        &self,
        processes: &[crate::proto::upgrade::HandoverProc],
        thawed: watch::Receiver<()>,
    ) -> (usize, usize) {
        let mut adopted = 0;
        let mut failed = 0;
        for proc in processes {
            match self.readopt_one(proc, thawed.clone()).await {
                Ok(()) => adopted += 1,
                Err(error) => {
                    eprintln!("handover: readopt pid {}: {error}", proc.pid);
                    // No entry will supervise this inherited child; reap it
                    // instead of leaving an invisible, unreaped process.
                    supervise::reap_unadopted(proc.pid).await;
                    failed += 1;
                }
            }
        }
        (adopted, failed)
    }

    async fn readopt_one(
        &self,
        proc: &crate::proto::upgrade::HandoverProc,
        thawed: watch::Receiver<()>,
    ) -> Result<(), String> {
        let relocate =
            |slot: i32| -> Option<std::os::fd::OwnedFd> { pipe::relocate_carried_fd(slot).ok() };
        let is_pty = proc.cg_type == "pty";
        // A PTY child's I/O is the master; its carried stdout/stderr (absent
        // from the outgoing side) are dropped rather than wrapped. Non-PTY
        // endpoints are relocated first and stay owned by the entry, so the
        // next handover can carry them (Go `HandoverFds` reports the
        // readopted handler's live files).
        let (terminal, stdout_owned, stderr_owned) = if is_pty {
            let tty = relocate(proc.tty_fd)
                .ok_or_else(|| format!("tty descriptor absent for pty pid {}", proc.pid))?;
            let terminal = pty::Terminal::from_master(tty)
                .map_err(|error| format!("adopted master for pid {}: {error}", proc.pid))?;
            (Some(terminal), None, None)
        } else {
            (
                None,
                relocate(proc.stdout_fd).map(Arc::new),
                relocate(proc.stderr_fd).map(Arc::new),
            )
        };
        // Relocate every carried descriptor before any fallible registration:
        // on failure the owned values below drop and close every slot this
        // process owned, so no CLOEXEC-cleared handover fd survives into the
        // next export (which keys slots by sorted-pid index).
        let stdin_fd = relocate(proc.stdin_fd);
        // The pump registers the entry's own descriptor: Go's `Readopt` keeps
        // `args.Stdout` and both the handler and the pump share that one
        // `*os.File`, so a chained export reports the same endpoint. A failed
        // registration fails the adoption instead of leaving the child with
        // an unread pipe and reporting a clean handover.
        let read_side = |fd: &Option<Arc<std::os::fd::OwnedFd>>| -> Result<_, String> {
            let Some(fd) = fd else {
                return Ok(None);
            };
            tokio::io::unix::AsyncFd::new(pipe::SharedFd(fd.clone()))
                .map(pipe::FdReader)
                .map(Some)
                .map_err(|error| format!("adopted reader for pid {}: {error}", proc.pid))
        };
        let stdout = read_side(&stdout_owned)?;
        let stderr = read_side(&stderr_owned)?;
        let stdin = match stdin_fd {
            None => None,
            Some(fd) => Some(pipe::StdinPipe::Adopted(
                tokio::io::unix::AsyncFd::new(std::fs::File::from(fd))
                    .map_err(|error| format!("adopted stdin for pid {}: {error}", proc.pid))?,
            )),
        };
        let (control, controls) = mpsc::channel(1);
        let entry = Arc::new(Entry {
            info: pb::ProcessInfo {
                pid: proc.pid,
                config: proc.config.clone(),
                tag: proc.has_tag.then(|| proc.tag.clone()),
                ..Default::default()
            },
            stdin: Mutex::new(stdin),
            pty: Mutex::new(terminal.clone()),
            is_pty,
            subscribers: Mutex::new(Subscribers::default()),
            control,
            stdout_bytes: Arc::new(AtomicI64::new(0)),
            stderr_bytes: Arc::new(AtomicI64::new(0)),
            pty_bytes: Arc::new(AtomicI64::new(0)),
            operation_id: None,
            stdout_dup: stdout_owned,
            stderr_dup: stderr_owned,
            deadline_at: Mutex::new(None),
        });
        // Register before the supervision starts, so a Connect racing the
        // adoption sees the entry; the terminal event reaches it through the
        // normal publication path (readopt.go: Store → OnExit → BeginReaping
        // orders the same lifetime).
        self.registry
            .lock()
            .await
            .live
            .insert(proc.pid, entry.clone());
        tokio::spawn(supervise::run_adopted(
            self.registry.clone(),
            entry,
            controls,
            supervise::AdoptedIo {
                stdout,
                stderr,
                tty: terminal,
            },
            (proc.timeout_ms > 0).then(|| Duration::from_millis(proc.timeout_ms as u64)),
            thawed,
            self.logs.clone(),
        ));
        Ok(())
    }
}

/// Go upgrade.go:58-77 (`dupKeep`): relocate `old` onto `target` with CLOEXEC
/// cleared so the descriptor survives the execve. Returns -1 when `old` is
/// absent, and errors if `target` is already open: dup3 silently closes the
/// occupant, so a collision must abort the upgrade rather than corrupt a
/// live descriptor.
fn dup_keep(old: RawFd, target: RawFd) -> Result<RawFd, String> {
    use nix::fcntl::{fcntl, FcntlArg::F_GETFD};
    use nix::unistd::dup3;
    if old < 0 {
        return Ok(-1);
    }
    if fcntl(target, F_GETFD).is_ok() {
        return Err(format!("handover fd target {target} already in use"));
    }
    dup3(old, target, nix::fcntl::OFlag::empty())
        .map(|_| target)
        .map_err(|error| format!("dup3 {old}->{target}: {error}"))
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
