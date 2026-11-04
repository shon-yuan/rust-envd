//! The Windows resource backend: freeze and unfreeze without cgroups.
//!
//! Linux freezes a cgroup and reads `cgroup.events` back. Windows has neither,
//! so the same wire contract is produced from two real sources:
//!
//! - **legacy** mode suspends every User/Pty pid the registry knows about;
//! - **hierarchy** mode suspends every pid in the sandbox Job Object except the
//!   daemon (and future service helpers), falling back to legacy when no Job
//!   has been shared yet, exactly like Linux's no-`PathManager` fallback.
//!
//! "Frozen" means every enumerable thread accepted `SuspendThread` and a
//! re-enumeration found nothing new. There is no kernel read-back, so
//! `unobservable` is always zero. Counters use the Linux reconciliation:
//! `frozen + notFrozen + unobservable` accounts for what was read back while
//! `failed` and `vanished` deliberately do not reconcile against `requested`.
//!
//! A sweep keeps the thread records it suspended in `last_sweep`; `unfreeze`
//! resumes exactly those tids, so threads the guest suspended itself keep
//! their own suspend count. A second freeze reuses the records instead of
//! calling `SuspendThread` again, which is what keeps counted per-thread
//! suspension idempotent across repeated `/freeze` calls.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{sleep, Instant};

use crate::freeze_wire::{FreezeMode, FreezeOptions, FreezeResult, DEFAULT_THAW_WATCHDOG_WINDOW};

use super::job::JobHandle;
use super::registry::WorkloadRegistry;
use super::suspend::{self, SuspendError, SuspensionSet};

/// How often the freeze poll re-enumerates a target that still had unseen
/// threads. Same 2 ms cadence as the Linux cgroup poll.
const FREEZE_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// The Linux static sweep's two workload cgroups. A spawn-drain refusal
/// reports them as failed even though Windows has no cgroups: the structured
/// caller sees the same `failed` count and the same zero walk counters
/// (Linux `freeze.go:365-375`).
const WORKLOAD_KINDS: i64 = 2;

/// The freeze lock permit. Holding it serializes every sweep and thaw, the
/// same single-lock rule as the Linux `WorkloadFreezer`.
#[derive(Debug)]
pub struct FreezeLease(pub(crate) OwnedSemaphorePermit);

/// The strict handover hold: the freeze permit held across the whole upgrade,
/// so no `/freeze` or `/unfreeze` can interleave between the sweep and the
/// swap. Dropping it on a failure path releases the lock (the caller then
/// thaws); the success path exits the process and never drops it, exactly
/// like Linux's `WorkloadHold`.
#[must_use = "dropping the hold releases the freeze lock"]
pub struct WorkloadFreezeHold {
    _permit: OwnedSemaphorePermit,
}

/// What the last sweep suspended, so a thaw touches only threads this backend
/// stopped.
#[derive(Debug, Clone, Default)]
pub struct SweepRecord {
    pub suspensions: Vec<SuspensionSet>,
}

/// The freeze-outside backstop state (oracle `freeze.go:206-216`). The
/// generation makes a superseded timer a no-op: a later freeze or a clean thaw
/// disarms the armed task, but one already dispatched still has to find itself
/// stale under the freeze lock.
#[derive(Debug)]
struct WatchdogState {
    window: Duration,
    generation: u64,
    armed: Option<tokio::task::JoinHandle<()>>,
}

impl Default for WatchdogState {
    fn default() -> Self {
        Self {
            window: DEFAULT_THAW_WATCHDOG_WINDOW,
            generation: 0,
            armed: None,
        }
    }
}

/// The Windows implementation of the platform resource seam.
pub struct WindowsResourceBackend {
    registry: Arc<WorkloadRegistry>,
    /// The sandbox Job Object, once the process slice shares one. `None` makes
    /// hierarchy mode fall back to legacy.
    job: Option<Arc<JobHandle>>,
    /// One permit: freeze and thaw never overlap.
    lock: Arc<Semaphore>,
    last_sweep: Mutex<Option<SweepRecord>>,
    /// The daemon itself must never be suspended by hierarchy mode.
    daemon_pid: u32,
    /// Advances on every freeze attempt that takes the lock, including one the
    /// spawn drain refuses. `/init`'s thaw compares a generation captured on
    /// entry so a freeze that started later survives the thaw.
    generation: AtomicU64,
    /// The backstop that resumes a workload no `/unfreeze` or thawing `/init`
    /// ever released. Armed by a sweep that froze something, disarmed only by
    /// a clean thaw.
    watchdog: Mutex<WatchdogState>,
    /// Test seam for the `/unfreeze` 500 envelope; absent in production.
    #[cfg(test)]
    unfreeze_error: Mutex<Option<String>>,
}

impl Default for WindowsResourceBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsResourceBackend {
    pub fn new() -> Self {
        Self::with_registry(Arc::new(WorkloadRegistry::new()))
    }

    pub fn with_registry(registry: Arc<WorkloadRegistry>) -> Self {
        Self {
            registry,
            job: None,
            lock: Arc::new(Semaphore::new(1)),
            last_sweep: Mutex::new(None),
            daemon_pid: std::process::id(),
            generation: AtomicU64::new(0),
            watchdog: Mutex::new(WatchdogState::default()),
            #[cfg(test)]
            unfreeze_error: Mutex::new(None),
        }
    }

    /// Shares the sandbox Job Object created by the process slice.
    pub fn with_job(mut self, job: Arc<JobHandle>) -> Self {
        self.job = Some(job);
        self
    }

    /// Overrides the pid hierarchy mode must exclude (tests use a child as a
    /// stand-in daemon).
    pub fn with_daemon_pid(mut self, pid: u32) -> Self {
        self.daemon_pid = pid;
        self
    }

    pub fn registry(&self) -> &Arc<WorkloadRegistry> {
        &self.registry
    }

    /// Test seam: shortens the backstop window without waiting ten minutes.
    #[cfg(test)]
    pub(crate) fn with_thaw_watchdog_window(self, window: Duration) -> Self {
        self.watchdog.lock().expect("watchdog mutex").window = window;
        self
    }

    /// Test seam: make the next thaw fail with `error` so the handler's 500
    /// envelope can be asserted without breaking a real Job.
    #[cfg(test)]
    pub(crate) fn with_unfreeze_error_for_tests(self, error: &str) -> Self {
        *self.unfreeze_error.lock().expect("unfreeze error seam") = Some(error.to_owned());
        self
    }

    /// The current freeze generation, captured by `/init` before it starts.
    ///
    /// Taken under the freeze lock, like Linux's `freeze_generation`: a
    /// `/freeze` that has been accepted but has not yet bumped the counter is
    /// waited out, so `/init` never captures the pre-freeze value and then
    /// skips its own thaw.
    pub async fn generation(&self) -> u64 {
        let _permit = self
            .lock
            .clone()
            .acquire_owned()
            .await
            .expect("freeze lock is never closed");
        self.generation.load(Ordering::SeqCst)
    }

    /// Acquires the freeze lock. This is the one cancellable await of the
    /// `/freeze` handler: a caller that leaves while waiting sweeps nothing.
    pub async fn freeze_lease(&self) -> FreezeLease {
        FreezeLease(
            self.lock
                .clone()
                .acquire_owned()
                .await
                .expect("freeze lock is never closed"),
        )
    }

    /// Runs one sweep under an already acquired lease.
    pub async fn freeze_held(
        self: &Arc<Self>,
        lease: FreezeLease,
        options: FreezeOptions,
    ) -> (FreezeResult, Option<String>) {
        let _permit = lease.0;
        self.sweep_locked(options).await
    }

    /// The strict handover hold (the Windows analogue of Linux
    /// `freeze_workload_hold`): acquire the freeze lock for the WHOLE
    /// handover, sweep with the caller's budget, and keep the permit while
    /// the returned guard lives. A workload that did not stop is refused with
    /// the shared `WorkloadNotFrozen` text after a best-effort thaw, so the
    /// /upgrade handler answers 500 and the old daemon keeps serving.
    pub async fn freeze_workload_hold(
        self: &Arc<Self>,
        max_wait: Duration,
    ) -> Result<WorkloadFreezeHold, String> {
        let permit = self.freeze_lease().await.0;
        let (result, error) = self
            .sweep_locked(FreezeOptions {
                max_wait,
                ..Default::default()
            })
            .await;
        if error.is_none() && result.all_frozen() {
            return Ok(WorkloadFreezeHold { _permit: permit });
        }
        if let Err(thaw) = self.unfreeze_locked() {
            eprintln!("handover: unfreeze failed: {thaw}");
        }
        Err(match error {
            Some(error) => error,
            None => crate::upgrade::WorkloadNotFrozen.to_string(),
        })
    }

    /// One sweep; the caller already owns the freeze permit.
    async fn sweep_locked(
        self: &Arc<Self>,
        options: FreezeOptions,
    ) -> (FreezeResult, Option<String>) {
        // First, so a sweep that fails part-way still voids the thaw of every
        // `/init` that entered before it.
        self.generation.fetch_add(1, Ordering::SeqCst);

        let mut result = FreezeResult {
            mode: options.effective_mode(),
            ..Default::default()
        };

        // Block new starts and wait for in-flight ones before enumerating
        // threads. A drain timeout refuses the sweep; it is a counted failure,
        // not a 500 (a pause with a running workload is what a failed freeze
        // already produces).
        if let Err(error) = self.registry.raise_and_drain(None).await {
            // Mirror the Linux drain refusal: the two workload kinds count as
            // failed, the requested mode is kept, and the hierarchy walk
            // counters stay zero because no walk happened.
            result.failed = WORKLOAD_KINDS;
            // No window was opened; lower is idempotent.
            self.registry.lower();
            return (result, Some(error.to_string()));
        }

        let sweep_start = Instant::now();
        let selection = self.select_targets(&options);
        result.mode = selection.mode;
        result.visited = selection.visited;
        result.allowlisted = selection.allowlisted;
        result.truncated = selection.truncated;

        // Start from the previous sweep's records. A pid that is no longer a
        // target keeps its record so a later thaw still resumes it; a pid that
        // is still a target is extended instead of suspended twice.
        let mut suspensions: BTreeMap<u32, SuspensionSet> = {
            let guard = self.last_sweep.lock().expect("sweep record mutex");
            match guard.as_ref() {
                Some(record) => record
                    .suspensions
                    .iter()
                    .cloned()
                    .map(|set| (set.pid, set))
                    .collect(),
                None => BTreeMap::new(),
            }
        };

        let mut requested_pids: Vec<u32> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for pid in &selection.pids {
            let set = suspensions
                .entry(*pid)
                .or_insert_with(|| SuspensionSet::new(*pid));
            match suspend::suspend_process_into(set) {
                Ok(()) => {
                    result.requested += 1;
                    requested_pids.push(*pid);
                }
                Err(SuspendError::Vanished) => result.vanished += 1,
                Err(SuspendError::Failed(message)) => {
                    result.failed += 1;
                    errors.push(message);
                }
            }
        }
        // Failed targets may have left an empty placeholder; there is nothing
        // to resume for them.
        suspensions.retain(|_, set| !set.is_empty());
        // Threads the guest had suspended before our sweep. Counted per pid,
        // like Linux's `guest_frozen` cgroup set: one pre-suspended process is
        // one preFrozen regardless of how many of its threads were stopped.
        // Recorded from the set, so a repeated freeze in the same window still
        // reports it.
        result.pre_frozen = suspensions
            .values()
            .filter(|set| {
                set.threads
                    .iter()
                    .any(|thread| thread.was_already_suspended)
            })
            .count() as i64;
        result.sweep_duration = sweep_start.elapsed();

        // A zero budget observes nothing; every accepted process is
        // NotFrozen, matching Linux `awaitFrozen` returning at once.
        let mut poll_frozen = 0i64;
        let mut poll_failed = 0i64;
        let mut poll_vanished = 0i64;
        if !requested_pids.is_empty() && !options.max_wait.is_zero() {
            let deadline = Instant::now() + options.max_wait;
            let mut pending = requested_pids;
            loop {
                let mut next = Vec::new();
                for pid in &pending {
                    let Some(set) = suspensions.get_mut(pid) else {
                        continue;
                    };
                    match suspend::suspend_new_threads(set) {
                        Ok(false) => poll_frozen += 1,
                        Ok(true) => next.push(*pid),
                        Err(SuspendError::Vanished) => poll_vanished += 1,
                        Err(SuspendError::Failed(message)) => {
                            poll_failed += 1;
                            errors.push(message);
                        }
                    }
                }
                pending = next;
                if pending.is_empty() {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let remaining = deadline.saturating_duration_since(now);
                sleep(FREEZE_POLL_INTERVAL.min(remaining)).await;
            }
        }

        result.frozen = poll_frozen;
        // There is no kernel read-back to be unobservable.
        result.unobservable = 0;
        result.not_frozen =
            reconcile_not_frozen(result.requested, poll_frozen, poll_failed, poll_vanished, 0);
        result.failed += poll_failed;
        result.vanished += poll_vanished;
        result.wait_duration = sweep_start.elapsed().saturating_sub(result.sweep_duration);

        // The gate stays raised while anything is suspended (including a
        // carried record from an earlier sweep), exactly like the Linux
        // freeze-active OR; it closes only when the window is empty.
        let active = !suspensions.is_empty();
        {
            let mut guard = self.last_sweep.lock().expect("sweep record mutex");
            *guard = Some(SweepRecord {
                suspensions: suspensions.into_values().collect(),
            });
        }
        if !active {
            self.registry.lower();
        }

        // The backstop exists to undo what this sweep did; a sweep that
        // suspended nothing has left nothing for it to undo (`freeze.go:702-710`).
        if result.requested > 0 {
            self.arm_watchdog();
        }

        let error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n"))
        };
        (result, error)
    }

    /// Resumes exactly the threads recorded by the last sweep. Thawing
    /// nothing is success; a real resume failure is joined and returned for
    /// the 500 envelope.
    pub async fn unfreeze(&self) -> Result<(), String> {
        let _permit = self
            .lock
            .clone()
            .acquire_owned()
            .await
            .expect("freeze lock is never closed");
        self.unfreeze_locked()
    }

    /// `/init` thaw with the generation guard: a freeze that started after
    /// `/init` captured `want` survives. Returns whether the thaw ran.
    pub async fn thaw_if_generation(&self, want: u64) -> Result<bool, String> {
        let _permit = self
            .lock
            .clone()
            .acquire_owned()
            .await
            .expect("freeze lock is never closed");
        if self.generation.load(Ordering::SeqCst) != want {
            return Ok(false);
        }
        self.unfreeze_locked().map(|()| true)
    }

    fn unfreeze_locked(&self) -> Result<(), String> {
        #[cfg(test)]
        if let Some(error) = self
            .unfreeze_error
            .lock()
            .expect("unfreeze error seam")
            .take()
        {
            // Still close the window so the seam cannot strand a test.
            let _ = self.last_sweep.lock().expect("sweep record mutex").take();
            self.registry.lower();
            self.disarm_watchdog();
            return Err(error);
        }

        let record = self.last_sweep.lock().expect("sweep record mutex").take();
        let mut errors = Vec::new();
        // Threads that could not be resumed stay recorded and keep the gate
        // up, so a retried thaw resumes exactly them. Dropping the record on a
        // partial failure would answer the retry 204 while threads stay
        // suspended (`freeze.go:1317-1331`).
        let mut remaining = Vec::new();
        if let Some(record) = record {
            // Sorted by pid for deterministic error text.
            let mut suspensions = record.suspensions;
            suspensions.sort_by_key(|set| set.pid);
            for set in suspensions {
                let (unresumed, failures) = suspend::resume_process_partial(&set);
                errors.extend(failures);
                if !unresumed.is_empty() {
                    remaining.push(SuspensionSet {
                        pid: set.pid,
                        threads: unresumed,
                    });
                }
            }
        }
        let clean = remaining.is_empty();
        if clean {
            self.registry.lower();
            // Only a CLEAN thaw disarms: a failed resume leaves work for the
            // backstop, and nothing else would retry it.
            self.disarm_watchdog();
        } else {
            *self.last_sweep.lock().expect("sweep record mutex") = Some(SweepRecord {
                suspensions: remaining,
            });
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    /// (Re)arms the backstop after a sweep that suspended something. Called
    /// with the freeze lock held, the invariant the watchdog's own re-arm
    /// keeps (`freeze.go:254-280`).
    fn arm_watchdog(self: &Arc<Self>) {
        let (window, generation) = {
            let mut watchdog = self.watchdog.lock().expect("watchdog mutex");
            if watchdog.window.is_zero() {
                return;
            }
            if let Some(armed) = watchdog.armed.take() {
                armed.abort();
            }
            watchdog.generation += 1;
            (watchdog.window, watchdog.generation)
        };
        let weak = Arc::downgrade(self);
        let armed = tokio::spawn(async move {
            sleep(window).await;
            let Some(backend) = weak.upgrade() else {
                return;
            };
            backend.thaw_for_watchdog(generation).await;
        });
        self.watchdog.lock().expect("watchdog mutex").armed = Some(armed);
    }

    /// The backstop thaw, only if this timer is still the armed one. The
    /// staleness check runs with the freeze lock held: a later freeze arms its
    /// own timer while holding that lock, so a check made earlier could be
    /// overtaken (`freeze.go:296-345`).
    async fn thaw_for_watchdog(self: &Arc<Self>, generation: u64) {
        let _permit = self
            .lock
            .clone()
            .acquire_owned()
            .await
            .expect("freeze lock is never closed");
        if self.watchdog.lock().expect("watchdog mutex").generation != generation {
            return;
        }
        if let Err(error) = self.unfreeze_locked() {
            // A rescue that did not fully succeed must arm the backstop again;
            // nothing else will retry. Armed with the freeze lock still held.
            eprintln!("thaw watchdog could not resume every workload; re-arming: {error}");
            self.arm_watchdog();
        }
    }

    /// Stops the backstop and invalidates a timer already dispatched.
    fn disarm_watchdog(&self) {
        let mut watchdog = self.watchdog.lock().expect("watchdog mutex");
        if let Some(armed) = watchdog.armed.take() {
            armed.abort();
        }
        watchdog.generation += 1;
    }

    /// The `ResourceBackend` trait's synchronous freeze: legacy mode, no Job.
    pub fn freeze_blocking(self: &Arc<Self>, max_wait: Duration) -> io::Result<()> {
        let (result, error) = self.block_on(async {
            let lease = self.freeze_lease().await;
            self.freeze_held(
                lease,
                FreezeOptions {
                    max_wait,
                    ..Default::default()
                },
            )
            .await
        })?;
        match error {
            Some(error) => Err(io::Error::other(error)),
            None if result.all_frozen() => Ok(()),
            None => Err(io::Error::other(format!(
                "freeze incomplete: requested={} frozen={} notFrozen={} failed={} vanished={}",
                result.requested, result.frozen, result.not_frozen, result.failed, result.vanished
            ))),
        }
    }

    /// The `ResourceBackend` trait's synchronous thaw.
    pub fn thaw_blocking(self: &Arc<Self>) -> io::Result<()> {
        match self.block_on(self.unfreeze())? {
            Ok(()) => Ok(()),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    /// Bridges the synchronous platform trait onto the async backend.
    ///
    /// The daemon itself drives the async methods; the trait is only the
    /// platform seam. Blocking inside a current-thread runtime has no
    /// sound answer (the runtime cannot make progress while it is blocked),
    /// so that one case reports an error rather than panicking.
    fn block_on<F: Future>(&self, future: F) -> io::Result<F::Output> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                Ok(tokio::task::block_in_place(|| handle.block_on(future)))
            }
            Ok(_) => Err(io::Error::other(
                "cannot run the blocking Windows resource backend inside a current-thread runtime",
            )),
            Err(_) => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                Ok(runtime.block_on(future))
            }
        }
    }

    fn allowlisted(&self, pid: u32) -> bool {
        // The daemon must stay runnable to answer `/unfreeze`. The process
        // slice may extend this with service helpers; until then the daemon
        // pid is the only entry both sides agree on.
        pid == self.daemon_pid
    }

    fn select_targets(&self, options: &FreezeOptions) -> TargetSelection {
        if options.effective_mode() == FreezeMode::Hierarchy {
            if let Some(job) = &self.job {
                if let Ok(ids) = job.process_ids() {
                    let max = options.effective_max_cgroups();
                    let mut pids = Vec::new();
                    let mut visited = 0usize;
                    let mut allowlisted = 0i64;
                    let mut truncated = false;
                    for pid in ids {
                        if visited >= max {
                            truncated = true;
                            break;
                        }
                        visited += 1;
                        if self.allowlisted(pid) {
                            allowlisted += 1;
                            continue;
                        }
                        pids.push(pid);
                    }
                    return TargetSelection {
                        pids,
                        mode: FreezeMode::Hierarchy,
                        visited: visited as i64,
                        allowlisted,
                        truncated,
                    };
                }
                // A Job that cannot be queried is no better than no Job:
                // fall back rather than freeze nothing.
            }
        }
        // No walker: the static registry list. `visited`/`allowlisted`/
        // `truncated` describe the hierarchy walk and stay zero/false here,
        // matching the Linux legacy arm.
        TargetSelection {
            pids: self.registry.workload_pids(),
            mode: FreezeMode::Legacy,
            visited: 0,
            allowlisted: 0,
            truncated: false,
        }
    }
}

struct TargetSelection {
    pids: Vec<u32>,
    mode: FreezeMode,
    visited: i64,
    allowlisted: i64,
    truncated: bool,
}

/// The Linux `freeze.go:672` difference formula, shared with the tests.
pub(crate) fn reconcile_not_frozen(
    requested: i64,
    frozen: i64,
    failed: i64,
    vanished: i64,
    unobservable: i64,
) -> i64 {
    requested - frozen - failed - vanished - unobservable
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::registry::WorkloadClass;
    use std::process::{Child, Command, Stdio};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
    };

    struct Helper {
        child: Option<Child>,
    }

    impl Helper {
        fn spawn() -> Self {
            let child = Command::new("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn ping helper");
            Self { child: Some(child) }
        }

        fn pid(&self) -> u32 {
            self.child.as_ref().expect("helper alive").id()
        }
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                // TerminateProcess stops a suspended process too, so a failed
                // test cannot leak a frozen child.
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn first_tid(pid: u32) -> u32 {
        suspend::enumerate_threads(pid).expect("enumerate helper")[0]
    }

    fn manually_suspend(tid: u32) {
        // SAFETY: scalar arguments; the handle is checked and closed.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
        assert!(!thread.is_null(), "open thread {tid}");
        // SAFETY: valid THREAD_SUSPEND_RESUME handle.
        assert_eq!(unsafe { SuspendThread(thread) }, 0, "first manual suspend");
        // SAFETY: handle ours, unused afterwards.
        unsafe {
            CloseHandle(thread);
        }
    }

    fn probe_suspend_count(tid: u32) -> u32 {
        // SAFETY: scalar arguments; the handle is checked and closed.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
        assert!(!thread.is_null(), "open thread {tid}");
        // SAFETY: valid THREAD_SUSPEND_RESUME handle.
        let previous = unsafe { SuspendThread(thread) };
        assert_ne!(previous, u32::MAX, "probe suspend");
        // SAFETY: our own probe is the only increment being removed.
        let restored = unsafe { ResumeThread(thread) };
        assert_ne!(restored, u32::MAX, "probe resume");
        // SAFETY: handle ours, unused afterwards.
        unsafe {
            CloseHandle(thread);
        }
        previous
    }

    #[test]
    fn freeze_counts_reconcile_like_linux() {
        // frozen + notFrozen + unobservable accounts for the read-back set.
        assert_eq!(reconcile_not_frozen(10, 6, 0, 0, 0), 4);
        // failed and vanished do not reconcile against requested.
        assert_eq!(reconcile_not_frozen(10, 6, 2, 3, 0), -1);
        // A zero budget observes nothing: everything requested is notFrozen.
        assert_eq!(reconcile_not_frozen(4, 0, 0, 0, 0), 4);
    }

    #[tokio::test]
    async fn a_zero_wait_reports_every_requested_process_as_not_frozen() {
        let registry = Arc::new(WorkloadRegistry::new());
        let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
        let helper = Helper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        registry.register(helper.pid(), WorkloadClass::User, None);
        let lease = backend.freeze_lease().await;
        let (result, error) = backend
            .freeze_held(
                lease,
                FreezeOptions {
                    max_wait: Duration::ZERO,
                    ..Default::default()
                },
            )
            .await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(result.mode, FreezeMode::Legacy);
        assert_eq!(result.requested, 1);
        assert_eq!(result.frozen, 0);
        assert_eq!(result.not_frozen, 1);
        backend.unfreeze().await.expect("thaw");
    }

    #[tokio::test]
    async fn repeated_freezes_do_not_stack_suspensions() {
        let registry = Arc::new(WorkloadRegistry::new());
        let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
        let helper = Helper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        registry.register(helper.pid(), WorkloadClass::User, None);
        let options = FreezeOptions {
            max_wait: Duration::from_millis(500),
            ..Default::default()
        };
        let lease = backend.freeze_lease().await;
        let (first, error) = backend.freeze_held(lease, options.clone()).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(first.requested, 1);
        assert_eq!(first.frozen, 1);
        assert_eq!(first.not_frozen, 0);
        let lease = backend.freeze_lease().await;
        let (second, error) = backend.freeze_held(lease, options).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(second.requested, 1);
        assert_eq!(second.frozen, 1);
        // Exactly one increment, not two.
        assert_eq!(probe_suspend_count(first_tid(helper.pid())), 1);
        backend.unfreeze().await.expect("thaw");
        assert_eq!(probe_suspend_count(first_tid(helper.pid())), 0);
        // Unfreeze is idempotent.
        backend.unfreeze().await.expect("second thaw");
    }

    #[tokio::test]
    async fn already_suspended_threads_are_pre_frozen_and_left_suspended() {
        let registry = Arc::new(WorkloadRegistry::new());
        let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
        let helper = Helper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        registry.register(helper.pid(), WorkloadClass::User, None);
        let tid = first_tid(helper.pid());
        manually_suspend(tid);
        let lease = backend.freeze_lease().await;
        let (result, error) = backend
            .freeze_held(
                lease,
                FreezeOptions {
                    max_wait: Duration::from_millis(500),
                    ..Default::default()
                },
            )
            .await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            result.pre_frozen, 1,
            "one pre-suspended process, not its threads"
        );
        backend.unfreeze().await.expect("thaw our increment");
        // The guest's own suspend remains.
        assert_eq!(probe_suspend_count(tid), 1);
    }

    #[tokio::test]
    async fn the_watchdog_resumes_a_freeze_that_was_never_thawed() {
        let registry = Arc::new(WorkloadRegistry::new());
        let backend = Arc::new(
            WindowsResourceBackend::with_registry(registry.clone())
                .with_thaw_watchdog_window(Duration::from_millis(100)),
        );
        let helper = Helper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        registry.register(helper.pid(), WorkloadClass::User, None);
        let tid = first_tid(helper.pid());
        let lease = backend.freeze_lease().await;
        let (result, error) = backend
            .freeze_held(
                lease,
                FreezeOptions {
                    max_wait: Duration::from_millis(500),
                    ..Default::default()
                },
            )
            .await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(result.frozen, 1);
        assert_eq!(probe_suspend_count(tid), 1);
        // No thaw call: the backstop must resume the workload on its own.
        let mut resumed = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if probe_suspend_count(tid) == 0 {
                resumed = true;
                break;
            }
        }
        assert!(resumed, "the backstop must undo a lost freeze");
        // The record is gone, so a later thaw is a clean no-op.
        backend.unfreeze().await.expect("idempotent thaw");
    }

    #[tokio::test]
    async fn hierarchy_falls_back_to_legacy_without_a_job() {
        let registry = Arc::new(WorkloadRegistry::new());
        let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
        let helper = Helper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        registry.register(helper.pid(), WorkloadClass::User, None);
        let lease = backend.freeze_lease().await;
        let (result, error) = backend
            .freeze_held(
                lease,
                FreezeOptions {
                    mode: FreezeMode::Hierarchy,
                    max_wait: Duration::ZERO,
                    ..Default::default()
                },
            )
            .await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(result.mode, FreezeMode::Legacy);
        assert_eq!(result.requested, 1);
        assert_eq!(result.visited, 0);
        assert!(!result.truncated);
        backend.unfreeze().await.expect("thaw");
    }

    #[tokio::test]
    async fn thaw_if_generation_skips_a_freeze_that_started_later() {
        let registry = Arc::new(WorkloadRegistry::new());
        let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
        let helper = Helper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        registry.register(helper.pid(), WorkloadClass::User, None);
        // /init captures this before the freeze below starts.
        let entry_generation = backend.generation().await;
        let lease = backend.freeze_lease().await;
        let (result, error) = backend
            .freeze_held(
                lease,
                FreezeOptions {
                    max_wait: Duration::from_millis(500),
                    ..Default::default()
                },
            )
            .await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(result.frozen, 1);

        // The freeze invalidated the entry generation, so /init leaves it
        // suspended; the next unfreeze still resumes it.
        assert!(!backend
            .thaw_if_generation(entry_generation)
            .await
            .expect("guarded thaw"));
        assert_eq!(probe_suspend_count(first_tid(helper.pid())), 1);
        backend.unfreeze().await.expect("thaw");
        assert_eq!(probe_suspend_count(first_tid(helper.pid())), 0);
    }

    #[tokio::test]
    async fn thaw_waits_for_the_freeze_lease() {
        let backend = Arc::new(WindowsResourceBackend::with_registry(Arc::new(
            WorkloadRegistry::new(),
        )));
        let lease = backend.freeze_lease().await;
        let owner = backend.clone();
        let waiting = tokio::spawn(async move { owner.unfreeze().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "a thaw waits behind the freeze lease"
        );
        drop(lease);
        let outcome = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("thaw completes")
            .expect("join");
        assert!(outcome.is_ok());
    }

    #[tokio::test]
    async fn hierarchy_freeze_uses_the_job_pid_list_and_excludes_the_daemon() {
        let job = match JobHandle::create() {
            Ok(job) => Arc::new(job),
            Err(_) => return,
        };
        let helper = Helper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        // A nested-job host refuses the assignment; the capability is then
        // absent, so the test skips rather than asserting the wrong path.
        if job.assign(helper.pid()).is_err() {
            return;
        }
        let backend = Arc::new(
            WindowsResourceBackend::with_registry(Arc::new(WorkloadRegistry::new())).with_job(job),
        );
        let lease = backend.freeze_lease().await;
        let (result, error) = backend
            .freeze_held(
                lease,
                FreezeOptions {
                    mode: FreezeMode::Hierarchy,
                    max_wait: Duration::from_millis(500),
                    ..Default::default()
                },
            )
            .await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(result.mode, FreezeMode::Hierarchy);
        assert!(result.visited >= 1);
        assert!(result.requested >= 1);
        assert!(result.frozen >= 1);
        backend.unfreeze().await.expect("thaw");
    }

    #[tokio::test]
    async fn vanished_pids_are_classified_without_aborting_the_sweep() {
        let registry = Arc::new(WorkloadRegistry::new());
        let backend = Arc::new(WindowsResourceBackend::with_registry(registry.clone()));
        // A pid that never existed is vanished, not failed. A terminated helper
        // cannot express that: its id becomes reusable as soon as the process
        // object is destroyed, and the guest observed ERROR_ACCESS_DENIED when
        // the kernel had already reused it. Windows allocates process ids in
        // multiples of four, so the top such value is never a real process. The
        // access-denied case is left to a real target: probing a protected
        // system process is not a safe test.
        const NEVER_EXISTING_PID: u32 = u32::MAX - 3;
        registry.register(NEVER_EXISTING_PID, WorkloadClass::User, None);
        let lease = backend.freeze_lease().await;
        let (result, error) = backend
            .freeze_held(
                lease,
                FreezeOptions {
                    max_wait: Duration::from_millis(500),
                    ..Default::default()
                },
            )
            .await;
        assert!(error.is_none(), "per-target failures are counts: {error:?}");
        assert_eq!(result.vanished, 1, "the dead pid is vanished");
        assert_eq!(result.failed, 0);
        backend.unfreeze().await.expect("thaw");
    }
}
