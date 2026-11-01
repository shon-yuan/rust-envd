//! The WorkloadFreezer, ported from oracle freeze.go (1431 lines). One
//! instance is shared by every freeze/unfreeze caller — /freeze, /unfreeze,
//! the /init thaw, the watchdog — so their sweeps serialize on a single lock
//! (store.go:64-68, main.go:226-228).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;

use super::hierarchy::{
    ancestor_chain, audit_frozen_state, descend_set, sample_path, scan_guest_frozen,
    self_cgroup_path, AuditResult,
};
use super::spawn::{SpawnBarrier, SpawnError};
use super::{Manager, PathManager, ProcessType, DEFAULT_THAW_MAX_CGROUPS, WORKLOAD_PROCESS_TYPES};

/// Oracle freeze.go:23-31.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum FreezeMode {
    #[default]
    #[serde(rename = "legacy")]
    Legacy,
    #[serde(rename = "hierarchy")]
    Hierarchy,
}

impl FreezeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FreezeMode::Legacy => "legacy",
            FreezeMode::Hierarchy => "hierarchy",
        }
    }
}

/// Oracle freeze.go:99-110.
#[derive(Debug, Clone)]
pub struct FreezeOptions {
    /// Bounds the wait for the written cgroups to read back frozen; zero
    /// observes nothing (oracle awaitFrozen: `budget <= 0` returns at once).
    pub max_wait: Duration,
    pub mode: FreezeMode,
    /// Bounds a hierarchy sweep; zero or negative means
    /// DEFAULT_FREEZE_MAX_CGROUPS — a caller cannot accidentally disable the
    /// guard (freeze.go:118-121) — and any positive value is clamped at
    /// DEFAULT_THAW_MAX_CGROUPS (freeze.go:123-129, the drift guard).
    pub max_cgroups: i64,
}

impl Default for FreezeOptions {
    fn default() -> Self {
        Self {
            max_wait: Duration::ZERO,
            mode: FreezeMode::Legacy,
            max_cgroups: 0,
        }
    }
}

impl FreezeOptions {
    /// Oracle freeze.go:112-117: anything but hierarchy is legacy, so a
    /// caller that has not been taught about modes keeps today's behaviour.
    pub fn effective_mode(&self) -> FreezeMode {
        match self.mode {
            FreezeMode::Hierarchy => FreezeMode::Hierarchy,
            FreezeMode::Legacy => FreezeMode::Legacy,
        }
    }
    /// Oracle freeze.go:118-130.
    pub fn effective_max_cgroups(&self) -> usize {
        if self.max_cgroups <= 0 {
            super::DEFAULT_FREEZE_MAX_CGROUPS
        } else {
            (self.max_cgroups as usize).min(super::DEFAULT_THAW_MAX_CGROUPS)
        }
    }
}

/// Oracle freeze.go:436-545. Frozen+NotFrozen+Unobservable accounts for every
/// cgroup whose state we read; Failed and Vanished span both the sweep and the
/// poll and deliberately do not reconcile against Requested (freeze.go:439-456).
#[derive(Debug, Default, Clone)]
pub struct FreezeResult {
    pub requested: i64,
    pub frozen: i64,
    pub not_frozen: i64,
    pub scan_failed: i64,
    pub pre_frozen: i64,
    pub failed: i64,
    pub vanished: i64,
    pub vanished_paths: Vec<String>,
    pub unobservable: i64,
    pub sweep_duration: Duration,
    pub mode: FreezeMode,
    pub visited: i64,
    pub allowlisted: i64,
    pub truncated: bool,
    pub wait_duration: Duration,
}

impl FreezeResult {
    /// Oracle freeze.go:547-550. Unobservable is not counted against it: a
    /// guest with no manager never had this guarantee.
    pub fn all_frozen(&self) -> bool {
        self.not_frozen == 0 && self.failed == 0
    }
}

/// The wire form the /freeze handler answers with; oracle api.gen.go:150-204,
/// every field filled by init.go:627-643, so nothing is omitted.
#[derive(Debug, Serialize)]
pub struct FreezeResultWire {
    pub allowlisted: i64,
    pub failed: i64,
    pub frozen: i64,
    pub mode: FreezeMode,
    #[serde(rename = "notFrozen")]
    pub not_frozen: i64,
    #[serde(rename = "preFrozen")]
    pub pre_frozen: i64,
    pub requested: i64,
    #[serde(rename = "sweepMs")]
    pub sweep_ms: i64,
    pub truncated: bool,
    pub unobservable: i64,
    pub vanished: i64,
    pub visited: i64,
    #[serde(rename = "waitMs")]
    pub wait_ms: i64,
}

impl FreezeResult {
    pub fn wire(&self) -> FreezeResultWire {
        FreezeResultWire {
            allowlisted: self.allowlisted,
            failed: self.failed,
            frozen: self.frozen,
            mode: self.mode,
            not_frozen: self.not_frozen,
            pre_frozen: self.pre_frozen,
            requested: self.requested,
            sweep_ms: self.sweep_duration.as_millis() as i64,
            truncated: self.truncated,
            unobservable: self.unobservable,
            vanished: self.vanished,
            visited: self.visited,
            wait_ms: self.wait_duration.as_millis() as i64,
        }
    }
}

/// Oracle freeze.go:1187-1206.
#[derive(Debug, Default, Clone)]
pub struct ThawResult {
    pub visited: i64,
    pub thawed: i64,
    pub failed: i64,
    pub truncated: bool,
    pub preserved: i64,
    pub discovered: bool,
}

/// Oracle freeze.go:608 plus the joined per-cgroup errors (errors.Join of the
/// sweep and poll failures). The drain timeout is deliberately NOT a wrapped
/// context error (spawn.go:21-28): /freeze tells a caller that went away apart
/// from a sweep that ran and collected per-cgroup errors.
#[derive(Debug, thiserror::Error)]
pub enum FreezeError {
    #[error("timed out waiting for an in-flight process spawn")]
    SpawnDrainTimeout,
    #[error("{0}")]
    Sweeps(String),
}

/// The FreezeWorkloadHold failures, displayed pre-wrapped the way main.go's
/// doUpgrade returns them (`handover aborted: freeze workload: %w`,
/// main.go:387). The freeze lock is released before either variant is
/// returned, so the caller's rollback thaw cannot self-deadlock.
#[derive(Debug, thiserror::Error)]
pub enum FreezeHoldError {
    #[error("handover aborted: freeze workload: {0}")]
    Freeze(#[from] FreezeError),
    #[error("handover aborted: freeze workload: {error}")]
    NotFrozen {
        #[source]
        error: crate::upgrade::WorkloadNotFrozen,
        /// The sweep result the refusal read, for the caller's warning — Go
        /// logs frozen/not_frozen/failed/wait_duration internally
        /// (process/upgrade.go:594-599). Boxed to keep the error type small;
        /// field access dereferences.
        result: Box<FreezeResult>,
    },
}

/// The freeze hold carried across the handover (process/upgrade.go:586-588):
/// the lock stays held so a concurrent /init or /unfreeze thaw blocks until
/// release, and the caller MUST release it on any path that does not execve.
/// Drop it BEFORE thawing — the thaw acquires the lock — the way Go's deferred
/// releaseFreeze + UnfreezeWorkload pair is ordered (main.go:377-383). A
/// successful execve drops it with the process image.
#[derive(Debug)]
pub struct WorkloadHold {
    _permit: OwnedSemaphorePermit,
}

/// Oracle freeze.go:715-738: one cgroup a sweep wrote to, paired with how to
/// read its settled state back.
#[derive(Debug, Clone)]
enum FreezeTarget {
    Static { process_type: ProcessType },
    Path { path: PathBuf },
}

impl FreezeTarget {
    fn label(&self) -> String {
        match self {
            FreezeTarget::Static { process_type } => process_type.to_string(),
            FreezeTarget::Path { path } => path.display().to_string(),
        }
    }
}

/// The state sweepMu guards in the oracle (freeze.go:178-204): which sweep the
/// most recent freeze performed, a generation that advances on every freeze
/// AND thaw so a change is detectable even when the mode is not, the cgroups
/// the guest had frozen before our last sweep, and whether a freeze of ours is
/// in effect.
#[derive(Debug, Default)]
struct SweepState {
    last_sweep_mode: Option<FreezeMode>,
    sweep_gen: u64,
    guest_frozen: HashSet<PathBuf>,
    freeze_active: bool,
}

/// The freeze-outside observer (oracle freeze.go:247-256 SetThawWatchdog).
type ThawWatchdog = dyn Fn(ThawResult, Option<String>) + Send + Sync;

/// Oracle freeze.go:206-216: the backstop state. `watchdog_gen` makes a
/// superseded fire a no-op — Stop cannot recall a callback already dispatched.
struct WatchdogState {
    window: Duration,
    on_fire: Option<Arc<ThawWatchdog>>,
    generation: u64,
    armed: Option<tokio::task::JoinHandle<()>>,
}

impl Default for WatchdogState {
    fn default() -> Self {
        Self {
            window: Duration::ZERO,
            on_fire: None,
            generation: 0,
            armed: None,
        }
    }
}

/// Oracle freeze.go:147-234.
pub struct WorkloadFreezer {
    manager: Arc<dyn Manager>,
    /// The shared freeze lock: one permit, held across sweeps, waits and the
    /// whole FreezeHold. Serialization is its only job.
    lock: Arc<Semaphore>,
    pub(crate) spawns: SpawnBarrier,
    proc_self_cgroup: PathBuf,
    sweep: Mutex<SweepState>,
    watchdog: Mutex<WatchdogState>,
    /// Oracle freeze.go:218-226: advances on every freeze attempt that takes
    /// the lock, including one the spawn drain then refuses. Mutations happen
    /// under the lock; the atomic only spares us a second mutex.
    freeze_gen: AtomicU64,
    /// Oracle freeze.go:228-233.
    unobserved_inits: AtomicI64,
    /// Oracle freeze.go:409-410 (`thawedCh`): closed by every thaw so the
    /// upgrade caller can wait out a freeze it did not take. A `watch` sender
    /// stands in for Go's close-on-thaw channel: `subscribe()` is Go's receive
    /// of the fresh channel, a send is the close, and a fresh subscriber sees
    /// nothing until the NEXT thaw — the resolution semantic Thawed sells.
    thawed_signal: watch::Sender<()>,
}

/// Oracle freeze.go:358-360: seeded below 2^52 so the values stay exactly
/// representable as doubles through 2^52 freezes.
const MAX_FREEZE_GEN_SEED: u64 = 1 << 52;

fn seed_freeze_gen() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    seeded_freeze_gen(nanos, std::process::id() as u64)
}

fn seeded_freeze_gen(nanos: u64, pid: u64) -> u64 {
    // Parenthesised deliberately: Rust binds & tighter than ^, so an unmasked
    // form would mask only the pid term and let the rotated nanos term through
    // whole — the seed invariant (freeze.go:354-360) is that the WHOLE value
    // stays below 2^52.
    (nanos.rotate_left(32) ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15)) & (MAX_FREEZE_GEN_SEED - 1)
}

#[cfg(test)]
mod seed_tests {
    use super::*;

    #[test]
    fn seed_is_whole_masked_below_the_double_safe_bound() {
        // Oracle freeze.go:354-360 seeds with rand.Uint64N(1<<52), so the
        // invariant is that the WHOLE seed stays below 2^52 — the mask must
        // cover the XOR of both terms, not ride at a precedence level that
        // lets one term through whole (Rust binds & tighter than ^).
        for nanos in [u64::MAX, 0xFFFF_FFFF_FFFF_0000, 0x0000_0001_0000_0000, 1, 0] {
            let seed = seeded_freeze_gen(nanos, u32::MAX as u64);
            assert!(
                seed < MAX_FREEZE_GEN_SEED,
                "nanos={nanos:#x} seeded {seed:#x}, at or above 2^52"
            );
        }
    }
}

impl WorkloadFreezer {
    pub fn new(manager: Arc<dyn Manager>) -> Self {
        Self {
            manager,
            lock: Arc::new(Semaphore::new(1)),
            spawns: SpawnBarrier::new(),
            proc_self_cgroup: PathBuf::from(super::hierarchy::PROC_SELF_CGROUP),
            sweep: Mutex::new(SweepState::default()),
            watchdog: Mutex::new(WatchdogState::default()),
            freeze_gen: AtomicU64::new(seed_freeze_gen()),
            unobserved_inits: AtomicI64::new(0),
            thawed_signal: watch::channel(()).0,
        }
    }

    /// Points the membership read at a fixture; production reads
    /// /proc/self/cgroup (freeze.go:174-176).
    pub fn with_proc_self_cgroup(mut self, path: PathBuf) -> Self {
        self.proc_self_cgroup = path;
        self
    }

    pub fn manager(&self) -> &Arc<dyn Manager> {
        &self.manager
    }

    /// Oracle freeze.go:236-252. Zero window disables the backstop.
    pub fn set_thaw_watchdog(&self, window: Duration, on_fire: Option<Arc<ThawWatchdog>>) {
        let mut watchdog = self.watchdog.lock().unwrap();
        watchdog.window = window;
        watchdog.on_fire = on_fire;
    }

    /// Oracle freeze.go:362-372: read with the freeze lock held so it is
    /// ordered against every freeze. The wait is not bounded — a caller whose
    /// request went away still reads it.
    pub async fn freeze_generation(&self) -> u64 {
        let _permit = self.acquire().await;
        self.freeze_gen.load(Ordering::SeqCst)
    }

    async fn acquire(&self) -> OwnedSemaphorePermit {
        self.lock
            .clone()
            .acquire_owned()
            .await
            .expect("freeze lock is never closed")
    }

    /// Test seam: hold the freeze lock so a test can park an
    /// already-dispatched watchdog callback behind it, the way the Go
    /// scheduler parks a fired timer between dispatch and run
    /// (freeze.go:211-216). #[doc(hidden)]: plumbing, not API.
    #[doc(hidden)]
    pub async fn hold_freeze_lock_for_tests(&self) -> OwnedSemaphorePermit {
        self.freeze_hold().await
    }

    /// Oracle freeze.go:569-576 FreezeHold: acquire and KEEP the freeze lock,
    /// returning the permit that holds it. The acquire is the caller's own
    /// await — as cancellable as oracle's `f.lock.Acquire(ctx, 1)`
    /// (freeze.go:575): a caller that gives up while waiting holds nothing
    /// and sweeps nothing, the way a gone /freeze must not sit on the lock
    /// polling and delay the rollback thaw behind it (freeze.go:555-557).
    /// Pair with [`WorkloadFreezer::freeze_held`], handing the permit to work
    /// that must run to completion.
    pub async fn freeze_hold(&self) -> OwnedSemaphorePermit {
        self.acquire().await
    }

    /// Oracle freeze.go:578-613: the FreezeHold sweep run under a permit the
    /// caller already holds, released only when the sweep is done. Split from
    /// the acquire so the HTTP layer can make the acquire its one cancellable
    /// await and run this part detached from its dropped caller — once the
    /// lock is held, the sweep and its accounting must complete
    /// (freeze.go:677-710); a dropped half-sweep would leave a retry to adopt
    /// our own walked cgroups as the guest's.
    pub async fn freeze_held(
        self: &Arc<Self>,
        permit: OwnedSemaphorePermit,
        options: FreezeOptions,
    ) -> (FreezeResult, Option<FreezeError>) {
        let held = self.freeze_locked(&options).await;
        drop(permit);
        held
    }

    /// Oracle process/upgrade.go:585-605 (`FreezeWorkloadHold`), on the freezer
    /// itself — Go's process Service method is this exact call. The sweep runs
    /// with the handover max-wait (the caller passes its HandoverMaxWait), and
    /// the hold is STRICT: a workload that did not fully stop refuses the
    /// handover, because execve-ing over a still-running workload is exactly
    /// what the freeze exists to prevent (process/upgrade.go:579-584). Go
    /// passes context.Background() so the acquire cannot be cancelled out from
    /// under the handover (main.go:375); the Rust acquire is likewise
    /// uncancellable. On any refusal the lock is released before returning —
    /// Go hands back the release func and the caller's defer releases it before
    /// thawing (main.go:377-383).
    pub async fn freeze_workload_hold(
        self: &Arc<Self>,
        max_wait: Duration,
    ) -> Result<WorkloadHold, FreezeHoldError> {
        let permit = self.freeze_hold().await;
        let (result, error) = self
            .freeze_locked(&FreezeOptions {
                max_wait,
                ..Default::default()
            })
            .await;
        match error {
            Some(error) => {
                drop(permit);
                Err(FreezeHoldError::Freeze(error))
            }
            None if !result.all_frozen() => {
                drop(permit);
                Err(FreezeHoldError::NotFrozen {
                    error: crate::upgrade::WorkloadNotFrozen,
                    result: Box::new(result),
                })
            }
            None => Ok(WorkloadHold { _permit: permit }),
        }
    }

    /// Oracle freeze.go:400-403.
    pub fn add_unobserved_init(&self) {
        self.unobserved_inits.fetch_add(1, Ordering::SeqCst);
    }
    pub fn take_unobserved_inits(&self) -> i64 {
        self.unobserved_inits.swap(0, Ordering::SeqCst)
    }

    /// Oracle freeze.go:558-563 + 574-713.
    pub async fn freeze(
        self: &Arc<Self>,
        options: FreezeOptions,
    ) -> (FreezeResult, Option<FreezeError>) {
        let permit = self.acquire().await;
        let held = self.freeze_locked(&options).await;
        drop(permit);
        held
    }

    /// The body of FreezeHold (freeze.go:574-713) with the lock held by the
    /// caller. The external callers are the /freeze endpoint and the
    /// live-upgrade hold ([`Self::freeze_workload_hold`],
    /// process/upgrade.go:585-605): the latter exports the world under the
    /// permit and only ever releases it by exec'ing the new binary or by the
    /// failure rollback's thaw.
    async fn freeze_locked(
        self: &Arc<Self>,
        options: &FreezeOptions,
    ) -> (FreezeResult, Option<FreezeError>) {
        // First, so a sweep that fails part-way has still voided the thaw of
        // every /init handler that entered before it (freeze.go:579-583).
        self.freeze_gen.fetch_add(1, Ordering::SeqCst);
        // A count from before this freeze belongs to the life it ends.
        self.unobserved_inits.store(0, Ordering::SeqCst);

        let mut result = FreezeResult {
            mode: options.effective_mode(),
            ..Default::default()
        };

        // Before ANY cgroup is written: block new spawns and wait for the
        // in-flight ones (freeze.go:588-609). A drain that times out refuses
        // the freeze: the caller sees an error and pauses an unfrozen
        // workload, which is what it already does whenever a freeze fails.
        // Counted as Failed so AllFrozen reads false (freeze.go:601-608).
        if let Err(error) = self.spawns.raise().await {
            result.failed = WORKLOAD_PROCESS_TYPES.len() as i64;
            let joined = match error {
                super::spawn::SpawnError::SpawnDrainTimeout => FreezeError::SpawnDrainTimeout,
                other => FreezeError::Sweeps(other.to_string()),
            };
            return (result, Some(joined));
        }

        let sweep_start = Instant::now();
        let mut errors: Vec<String> = Vec::new();
        let path_manager = self.manager.as_path_manager();
        let can_walk = path_manager.is_some();

        // Rescan only when no freeze of ours is already in place.
        // freezeInEffect, not freezeActive alone: an envd that froze and then
        // crashed has lost the flag but not the freeze, and rescanning there
        // records our own frozen cgroups as the guest's (freeze.go:620-623).
        let rescan = can_walk && !self.freeze_in_effect();
        if rescan {
            let manager = path_manager.expect("rescan implies a walker");
            let (found, truncated, scan_errors) =
                scan_guest_frozen(manager, &self.proc_self_cgroup, DEFAULT_THAW_MAX_CGROUPS);
            // Counted, NOT returned as an error: the only consumer treating a
            // freeze error as fatal is the live-upgrade handover, and a
            // cgroup that vanished mid-scan is a race the sweep tolerates
            // (freeze.go:634-637).
            result.scan_failed = scan_errors.len() as i64;
            result.truncated |= truncated;
            self.sweep.lock().unwrap().guest_frozen = found;
        }
        // Reported from the record rather than from this call's scan, so a
        // second freeze in the same window still says how many cgroups it is
        // leaving to the guest (freeze.go:647-651).
        result.pre_frozen = self.sweep.lock().unwrap().guest_frozen.len() as i64;

        let pending = if options.effective_mode() == FreezeMode::Hierarchy && can_walk {
            result.mode = FreezeMode::Hierarchy;
            self.sweep_hierarchy(
                path_manager.expect("checked above"),
                &mut result,
                &mut errors,
                options.effective_max_cgroups(),
                rescan,
            )
        } else {
            // A hierarchy sweep needs a manager that can address cgroups by
            // path; fall back to the static list rather than freezing nothing
            // (freeze.go:615-618).
            result.mode = FreezeMode::Legacy;
            self.sweep_legacy(&mut result, &mut errors)
        };
        result.sweep_duration = sweep_start.elapsed();

        let poll = self.await_frozen(pending, options.max_wait).await;
        result.frozen = poll.frozen;
        result.unobservable = poll.unobservable;
        result.vanished += poll.vanished;
        for path in &poll.vanished_paths {
            sample_path(&mut result.vanished_paths, path.clone());
        }
        // Oracle freeze.go:672: derived by difference, so a cgroup dropped
        // from the poll set without being counted somewhere lands here — the
        // loudest outcome of the set.
        result.not_frozen =
            result.requested - poll.frozen - poll.failed - poll.unobservable - poll.vanished;
        result.failed += poll.failed;
        result.wait_duration = sweep_start.elapsed() - result.sweep_duration;
        errors.extend(poll.errors);

        {
            let mut sweep = self.sweep.lock().unwrap();
            sweep.last_sweep_mode = Some(result.mode);
            sweep.sweep_gen = sweep.sweep_gen.wrapping_add(1);
            // ORed rather than assigned: a sweep that writes nothing does not
            // CLOSE a window an earlier one opened (freeze.go:680-686).
            sweep.freeze_active |= result.requested > 0;
            let active = sweep.freeze_active;
            drop(sweep);
            // A sweep with no accepted write opened no window; holding spawns
            // out of it would refuse process starts until the next thaw for
            // no reason (freeze.go:690-700).
            if !active {
                self.spawns.lower();
            }
        }

        // The backstop exists to undo what this sweep did; a sweep that wrote
        // nothing has left nothing for it to undo (freeze.go:702-710).
        if result.requested > 0 {
            self.arm_watchdog();
        }

        let error = match errors.is_empty() {
            true => None,
            false => Some(FreezeError::Sweeps(errors.join("\n"))),
        };
        (result, error)
    }

    /// Oracle freeze.go:742-761.
    fn sweep_legacy(
        &self,
        result: &mut FreezeResult,
        errors: &mut Vec<String>,
    ) -> Vec<FreezeTarget> {
        let mut pending = Vec::with_capacity(WORKLOAD_PROCESS_TYPES.len());
        for process_type in WORKLOAD_PROCESS_TYPES {
            match self.manager.freeze(process_type) {
                Err(error) => {
                    errors.push(format!("freeze {process_type} cgroup: {error}"));
                    result.failed += 1;
                }
                Ok(()) => {
                    result.requested += 1;
                    pending.push(FreezeTarget::Static { process_type });
                }
            }
        }
        pending
    }

    /// Oracle freeze.go:774-927: freezes the complement of envd's own ancestor
    /// chain. It does NOT recurse below the children it freezes — one write to
    /// a child stops its whole subtree (freeze.go:763-773).
    fn sweep_hierarchy(
        &self,
        manager: &dyn PathManager,
        result: &mut FreezeResult,
        errors: &mut Vec<String>,
        max_cgroups: usize,
        adopt_late_freezes: bool,
    ) -> Vec<FreezeTarget> {
        let guest_frozen = self.sweep.lock().unwrap().guest_frozen.clone();
        let root = manager.root();
        let Ok(self_path) = self_cgroup_path(&self.proc_self_cgroup, &root) else {
            // Without our own path there is no complement to compute, and
            // freezing a guess could stop envd itself (freeze.go:780-789).
            result.mode = FreezeMode::Legacy;
            let mut local_errors = Vec::new();
            let pending = self.sweep_legacy(result, &mut local_errors);
            errors.push(format!(
                "locate envd cgroup, falling back to legacy mode: no cgroup2 (0::) entry in {}",
                self.proc_self_cgroup.display()
            ));
            errors.extend(local_errors);
            return pending;
        };

        // The static list is frozen FIRST and unconditionally, so it is a
        // guaranteed subset of every sweep in either mode (freeze.go:791-805).
        let mut pending = self.sweep_legacy(result, errors);

        let descend = descend_set(&root, &self_path);
        let mut inside: HashSet<PathBuf> = descend.iter().cloned().collect();
        for process_type in WORKLOAD_PROCESS_TYPES {
            if let Some(path) = manager.path_of(process_type) {
                // Already requested above; skipped rather than re-written so
                // one cgroup is counted once (freeze.go:815-822).
                inside.insert(path);
            }
        }

        let mut late_guest: Vec<String> = Vec::new();
        for ancestor in descend {
            let children = match manager.children_of(&ancestor) {
                Err(error) => {
                    if super::vanished(&error) {
                        // Nothing to descend into — expected: the allowlist is
                        // a superset of what any one guest has
                        // (freeze.go:834-841).
                        continue;
                    }
                    errors.push(format!("list children of {}: {error}", ancestor.display()));
                    result.failed += 1;
                    continue;
                }
                Ok(children) => children,
            };
            for child in children {
                if result.visited >= max_cgroups as i64 {
                    result.truncated = true;
                    self.adopt_late_guest_freezes(result, &late_guest);
                    return pending;
                }
                result.visited += 1;
                if inside.contains(&child) {
                    // A cgroup we descend into is never frozen: it either
                    // holds envd or holds something the resume depends on.
                    continue;
                }
                if super::hierarchy::allowlisted(&root, &child) {
                    result.allowlisted += 1;
                    continue;
                }
                if guest_frozen.contains(&child) {
                    // Already stopped, and stopped by the guest; writing our
                    // own request here would make it ours to clear on resume
                    // (freeze.go:869-874).
                    continue;
                }
                if adopt_late_freezes {
                    match manager.freeze_requested_at(&child) {
                        Err(error) if super::vanished(&error) => {
                            record_vanished(result, &child);
                            continue;
                        }
                        Ok(true) => {
                            // Frozen BY THE GUEST between the scan and this
                            // write (freeze.go:875-899).
                            late_guest.push(child.display().to_string());
                            continue;
                        }
                        _ => {}
                    }
                }
                match manager.freeze_at(&child) {
                    Err(error) if super::vanished(&error) => {
                        // Gone between the parent's readdir and this write.
                        // Not Requested: the write never landed
                        // (freeze.go:903-910).
                        record_vanished(result, &child);
                    }
                    Err(error) => {
                        errors.push(format!("freeze {}: {error}", child.display()));
                        result.failed += 1;
                    }
                    Ok(()) => {
                        result.requested += 1;
                        pending.push(FreezeTarget::Path { path: child });
                    }
                }
            }
        }
        // Deferred in the oracle (freeze.go:826-831): a cgroup skipped as the
        // guest's but never recorded is one the thaw then clears.
        self.adopt_late_guest_freezes(result, &late_guest);
        pending
    }

    /// Oracle freeze.go:929-950.
    fn adopt_late_guest_freezes(&self, result: &mut FreezeResult, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let mut sweep = self.sweep.lock().unwrap();
        for path in paths {
            if sweep.guest_frozen.insert(PathBuf::from(path)) {
                result.pre_frozen += 1;
            }
        }
    }

    /// Oracle freeze.go:979-996: judged from the tree rather than from this
    /// process's memory. The static list is the evidence because every sweep
    /// writes it FIRST, in both modes.
    fn freeze_in_effect(&self) -> bool {
        if self.sweep.lock().unwrap().freeze_active {
            return true;
        }
        for process_type in WORKLOAD_PROCESS_TYPES {
            if matches!(self.manager.frozen(process_type), Ok(true)) {
                return true;
            }
        }
        false
    }

    /// Oracle freeze.go:1116-1185: all cgroups polled together, bounded by the
    /// slowest rather than the sum.
    async fn await_frozen(&self, targets: Vec<FreezeTarget>, budget: Duration) -> PollOutcome {
        let mut outcome = PollOutcome::default();
        if targets.is_empty() || budget.is_zero() {
            return outcome;
        }
        let mut remaining: Vec<FreezeTarget> = targets;
        let deadline = Instant::now() + budget;
        loop {
            let mut next = Vec::new();
            for target in &remaining {
                let read = match target {
                    FreezeTarget::Static { process_type } => self.manager.frozen(*process_type),
                    FreezeTarget::Path { path } => match self.manager.as_path_manager() {
                        Some(manager) => manager.frozen_at(path),
                        None => Err(super::CgroupError::FrozenUnobservable),
                    },
                };
                match read {
                    Err(super::CgroupError::FrozenUnobservable) => {
                        // Nothing to read and nothing to wait for; dropped so
                        // the loop exits instead of spinning out the budget.
                        outcome.unobservable += 1;
                    }
                    Err(error)
                        if super::vanished(&error)
                            && matches!(target, FreezeTarget::Path { .. }) =>
                    {
                        // A cgroup the WALK found went away while we waited —
                        // counted, not discarded: NotFrozen is derived by
                        // subtraction (freeze.go:1139-1157).
                        outcome.vanished += 1;
                        sample_path(&mut outcome.vanished_paths, target.label());
                    }
                    Err(error) => {
                        outcome.failed += 1;
                        outcome
                            .errors
                            .push(format!("read {} cgroup.events: {error}", target.label()));
                    }
                    Ok(true) => outcome.frozen += 1,
                    Ok(false) => next.push(target.clone()),
                }
            }
            remaining = next;
            if remaining.is_empty() || Instant::now() >= deadline {
                return outcome;
            }
            let remaining_budget = deadline.saturating_duration_since(Instant::now());
            sleep(freeze_poll_interval().min(remaining_budget)).await;
        }
    }

    /// Oracle freeze.go:380-396: runs the thaw only if `want` is still the
    /// current generation, checked with the lock held so no freeze can come
    /// between the check and the thaw. `client_gone` is read AFTER the thaw,
    /// inside this critical section — the exact point Go reads ctx.Err()
    /// (the client-gone count of init.go:706-713, "decided after the thaw,
    /// which is the event the count is about").
    pub async fn thaw_if_generation(
        &self,
        want: u64,
        client_gone: impl FnOnce() -> bool,
    ) -> ThawIfGeneration {
        let _permit = self.acquire().await;
        let current = self.freeze_gen.load(Ordering::SeqCst);
        if want != current {
            return ThawIfGeneration {
                current,
                matched: false,
                result: ThawResult::default(),
                error: None,
            };
        }
        let (result, error) = self.unfreeze_locked(DEFAULT_THAW_MAX_CGROUPS);
        if client_gone() {
            self.add_unobserved_init();
        }
        ThawIfGeneration {
            current,
            matched: true,
            result,
            error,
        }
    }

    /// Oracle freeze.go:1233-1247 (Unfreeze/UnfreezeReporting). The lock wait
    /// is not bounded: a dropped unfreeze would strand the workload frozen.
    pub async fn unfreeze(&self) -> Result<ThawResult, String> {
        let _permit = self.acquire().await;
        let (result, error) = self.unfreeze_locked(DEFAULT_THAW_MAX_CGROUPS);
        match error {
            Some(error) => Err(error),
            None => Ok(result),
        }
    }

    /// Oracle freeze.go:1252-1338. Callers hold the freeze lock.
    fn unfreeze_locked(&self, max_cgroups: usize) -> (ThawResult, Option<String>) {
        // Cleared up front, before any cgroup is touched: from here on the
        // tree no longer holds the state the sweep produced, so nothing may
        // audit against it. Unconditional on the outcome (freeze.go:1253-1261).
        {
            let mut sweep = self.sweep.lock().unwrap();
            sweep.last_sweep_mode = None;
            sweep.sweep_gen = sweep.sweep_gen.wrapping_add(1);
        }

        let mut result = ThawResult::default();
        let mut errors: Vec<String> = Vec::new();
        let path_manager = self.manager.as_path_manager();
        // Decided BEFORE anything is thawed: the static thaw below destroys
        // the evidence freeze_in_effect reads (freeze.go:1266-1273).
        let discover = path_manager.is_some() && self.freeze_in_effect();

        // Always thaw the static list: cheap, idempotent, and the only thing
        // a manager without path support can do (freeze.go:1275-1290).
        let mut static_thawed = true;
        for process_type in WORKLOAD_PROCESS_TYPES {
            if let Err(error) = self.manager.unfreeze(process_type) {
                errors.push(format!("unfreeze {process_type} cgroup: {error}"));
                result.failed += 1;
                // A static cgroup that no longer exists holds no frozen task
                // and takes no spawn, so it does not keep the barrier up.
                if !super::vanished(&error) {
                    static_thawed = false;
                }
            }
        }
        // The barrier guards spawns into the static cgroups only, so it
        // follows THEIR thaw (freeze.go:1292-1305).
        if static_thawed {
            self.spawns.lower();
        }
        // Discover ONLY when a freeze of ours is in effect; walking
        // unconditionally would clear cgroups the guest froze itself
        // (freeze.go:1307-1315).
        if discover {
            result.discovered = true;
            let manager = path_manager.expect("discover implies a walker");
            let discovered = self.thaw_discovered(manager, &mut result, max_cgroups);
            errors.extend(discovered);
        }
        // Disarm only on a CLEAN thaw; a thaw that truncated or failed leaves
        // cgroups frozen and nothing else would retry them
        // (freeze.go:1317-1331).
        let clean = errors.is_empty() && !result.truncated && result.failed == 0;
        if clean {
            self.disarm_watchdog();
            self.sweep.lock().unwrap().freeze_active = false;
        }
        let error = match errors.is_empty() {
            true => None,
            false => Some(errors.join("\n")),
        };
        // Oracle freeze.go:1335: signalThawed runs at the END of unfreezeLocked
        // unconditionally — every thaw path (the /init thaw, /unfreeze, the
        // watchdog, thaw-if-generation) funnels through here, so every waiter's
        // channel closes even when the thaw itself found nothing to do.
        self.signal_thawed();
        (result, error)
    }

    /// Oracle freeze.go:1340-1431: walks the whole tree and thaws every cgroup
    /// outside envd's chain whose OWN cgroup.freeze reads 1. It recurses (a
    /// frozen cgroup can be anywhere) and reads the request, not the settled
    /// state, because only the former distinguishes "something froze this
    /// cgroup" from "an ancestor is frozen".
    fn thaw_discovered(
        &self,
        manager: &dyn PathManager,
        result: &mut ThawResult,
        max_cgroups: usize,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        let guest_frozen = self.sweep.lock().unwrap().guest_frozen.clone();
        let root = manager.root();
        let Ok(self_path) = self_cgroup_path(&self.proc_self_cgroup, &root) else {
            // Without our own path we cannot exclude ourselves and cannot
            // trust the walk; report and leave the static thaw as the outcome
            // (freeze.go:1356-1362).
            return vec![format!(
                "locate envd cgroup for the thaw: no cgroup2 (0::) entry in {}",
                self.proc_self_cgroup.display()
            )];
        };
        let on_chain: HashSet<PathBuf> = ancestor_chain(&root, &self_path).into_iter().collect();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(root.clone());
        while let Some(current) = queue.pop_front() {
            if result.visited >= max_cgroups as i64 {
                result.truncated = true;
                errors.push(format!(
                    "thaw walk truncated at {max_cgroups} cgroups: some may still be frozen"
                ));
                return errors;
            }
            result.visited += 1;
            let mut skip = on_chain.contains(&current);
            if guest_frozen.contains(&current) && !skip {
                // The guest froze this before we ever swept; clearing it would
                // restart processes it deliberately suspended
                // (freeze.go:1381-1387).
                result.preserved += 1;
                skip = true;
            }
            if !skip {
                match manager.freeze_requested_at(&current) {
                    Err(error) if super::vanished(&error) => {}
                    Err(error) => {
                        errors.push(format!(
                            "read freeze state of {}: {error}",
                            current.display()
                        ));
                        result.failed += 1;
                    }
                    Ok(true) => match manager.unfreeze_at(&current) {
                        Err(error) if super::vanished(&error) => {}
                        Err(error) => {
                            errors.push(format!("thaw {}: {error}", current.display()));
                            result.failed += 1;
                        }
                        Ok(()) => result.thawed += 1,
                    },
                    Ok(false) => {}
                }
            }
            match manager.children_of(&current) {
                Err(error) if super::vanished(&error) => {}
                Err(error) => {
                    errors.push(format!("list children of {}: {error}", current.display()));
                    result.failed += 1;
                }
                Ok(children) => queue.extend(children),
            }
        }
        errors
    }

    /// Oracle hierarchy.go:289-302, exposed through the freezer like
    /// init.go:1116-1122 calls it. The walk is not atomic with respect to a
    /// thaw; the monotonic sweep generation turns a walk that spanned one into
    /// no report at all. Holding the freeze lock across the walk would close
    /// the same window but make a thaw wait on an observer — no observer may
    /// delay a thaw.
    pub async fn audit_frozen_set(
        &self,
        max_cgroups: usize,
    ) -> Option<Result<AuditResult, String>> {
        let manager = self.manager.as_path_manager()?;
        let (mode, generation) = {
            let sweep = self.sweep.lock().unwrap();
            (sweep.last_sweep_mode, sweep.sweep_gen)
        };
        let result = match audit_frozen_state(manager, &self.proc_self_cgroup, max_cgroups, mode) {
            Ok(result) => result,
            Err(error) => return Some(Err(error)),
        };
        if self.sweep.lock().unwrap().sweep_gen != generation {
            // A thaw began while the walk was in flight; a partial count is no
            // report (hierarchy.go:296-299).
            return Some(Ok(AuditResult::default()));
        }
        Some(Ok(result))
    }

    /// Oracle freeze.go:254-280: (re)start the backstop. Called only after a
    /// freeze actually wrote to at least one cgroup, and from the freeze paths
    /// while holding the freeze lock — the invariant the watchdog's own re-arm
    /// keeps (freeze.go:332-338). Measured in guest monotonic time: the timer
    /// is armed by the freeze and the guest's clock barely advances while the
    /// VM is paused, so it effectively starts counting when the sandbox
    /// resumes (freeze.go:239-243).
    fn arm_watchdog(self: &Arc<Self>) {
        let (window, on_fire, generation) = {
            let mut watchdog = self.watchdog.lock().unwrap();
            if watchdog.window.is_zero() {
                return;
            }
            if let Some(armed) = watchdog.armed.take() {
                armed.abort();
            }
            watchdog.generation += 1;
            (
                watchdog.window,
                watchdog.on_fire.clone(),
                watchdog.generation,
            )
        };
        let weak = Arc::downgrade(self);
        let armed = tokio::spawn(async move {
            sleep(window).await;
            let Some(freezer) = weak.upgrade() else {
                return;
            };
            let (result, fired, error) = freezer.thaw_for_watchdog(generation).await;
            if fired {
                if let Some(on_fire) = on_fire {
                    on_fire(result, error);
                }
            }
        });
        self.watchdog.lock().unwrap().armed = Some(armed);
    }

    /// Oracle freeze.go:296-345: runs the backstop thaw only if this timer is
    /// still the armed one. The staleness check happens with the freeze lock
    /// HELD — a later freeze arms its own timer while holding that lock, so a
    /// check made before blocking on it could still be overtaken. Lock order
    /// is f.lock then watchdogMu everywhere.
    async fn thaw_for_watchdog(
        self: &Arc<Self>,
        generation: u64,
    ) -> (ThawResult, bool, Option<String>) {
        let _permit = self.acquire().await;
        let stale = self.watchdog.lock().unwrap().generation != generation;
        if stale {
            return (ThawResult::default(), false, None);
        }
        let (result, error) = self.unfreeze_locked(DEFAULT_THAW_MAX_CGROUPS);
        // A rescue that did not fully succeed must arm the backstop again —
        // nothing else will (freeze.go:317-341). Armed with the freeze lock
        // still held, matching the freeze paths' invariant.
        if error.is_some() || result.truncated || result.failed > 0 {
            self.arm_watchdog();
        }
        (result, true, error)
    }

    /// Oracle freeze.go:282-294.
    fn disarm_watchdog(&self) {
        let mut watchdog = self.watchdog.lock().unwrap();
        if let Some(armed) = watchdog.armed.take() {
            armed.abort();
        }
        // Bumped as well as stopped, so a callback already dispatched becomes
        // a no-op instead of thawing whatever is frozen when it finally runs.
        watchdog.generation += 1;
    }

    /// Oracle freeze.go:412-424 Thawed: a fresh wait for the next thaw. Go's
    /// consumer is the resume's carried kill-timer (`thawed :=
    /// s.workloadFreezer.Thawed()`, upgrade.go:412) — a timer that starts
    /// counting only once the re-adopted process can actually run again. The
    /// carried kill-timer belongs to the disclosed live-adoption gap
    /// (crate::process); the channel is landed so the resume side can hand it
    /// to that consumer when it exists, and signalThawed keeps firing.
    pub fn thawed(&self) -> watch::Receiver<()> {
        self.thawed_signal.subscribe()
    }

    /// Oracle freeze.go:422-428 signalThawed: close every pending channel.
    /// Called unconditionally from unfreezeLocked; a caller with no receiver is
    /// not an error, so the send result is dropped.
    fn signal_thawed(&self) {
        let _ = self.thawed_signal.send(());
    }

    /// Oracle freeze.go:1001-1025 GuestFrozenPaths: the recorded guest-frozen
    /// cgroups, root-relative and sorted — relative because the value crosses a
    /// live upgrade in the handover blob, where an absolute path from another
    /// process image means nothing. Empty without a path-capable manager; an
    /// entry outside the root is skipped (filepath.Rel's error).
    pub fn guest_frozen_paths(&self) -> Vec<String> {
        let Some(manager) = self.manager.as_path_manager() else {
            return Vec::new();
        };
        let root = manager.root();
        let sweep = self.sweep.lock().unwrap();
        let mut out: Vec<String> = sweep
            .guest_frozen
            .iter()
            .filter_map(|path| relative_to(&root, path))
            .collect();
        out.sort();
        out
    }

    /// Oracle freeze.go:1027-1043 SetGuestFrozenPaths: restore the record from
    /// relative paths. An empty list means "no record" and is a NO-OP — it must
    /// not clear what this process already recorded.
    pub fn set_guest_frozen_paths(&self, relative: &[String]) {
        if relative.is_empty() {
            return;
        }
        let Some(manager) = self.manager.as_path_manager() else {
            return;
        };
        let root = manager.root();
        let set: HashSet<PathBuf> = relative.iter().map(|r| root.join(r)).collect();
        self.sweep.lock().unwrap().guest_frozen = set;
    }

    /// Oracle freeze.go:1064-1087 ResumeFrozen: the freezer has inherited a
    /// workload already frozen BY US — the state a live-upgrade handover leaves
    /// behind. Three pieces of state do not survive an execve:
    ///
    /// - the freeze counts as ACTIVE again, so a freeze taken before the thaw
    ///   does not re-run the guest scan and adopt our own frozen cgroups as the
    ///   guest's (freeze_in_effect reads the flag first);
    /// - the WATCHDOG is armed, because the timer belonged to the previous
    ///   image — an /init that never arrives must still hit the backstop;
    /// - the SPAWN BARRIER is raised, making the frozen refusal exact. It
    ///   cannot block: a freshly execve'd image has no spawns in flight.
    ///
    /// The sweep MODE is deliberately not restored: this process did not
    /// perform the freeze and cannot know what its predecessor covered.
    pub async fn resume_frozen(self: &Arc<Self>) -> Result<(), SpawnError> {
        self.sweep.lock().unwrap().freeze_active = true;
        // The watchdog first, so a barrier that fails to rise cannot leave the
        // inherited freeze without its backstop as well (freeze.go:1081-1082).
        self.arm_watchdog();
        self.spawns
            .raise()
            .await
            .map_err(|error| SpawnError::InheritedRaise(Box::new(error)))
    }
}

/// Go's filepath.Rel for the two shapes this module produces: `path` under
/// `root` yields the clean suffix; anything else has no relative form.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|rel| rel.to_string_lossy().into_owned())
}

/// Oracle freeze.go:1096-1106 plus 1108-1114.
#[derive(Debug, Default)]
struct PollOutcome {
    frozen: i64,
    failed: i64,
    unobservable: i64,
    vanished: i64,
    vanished_paths: Vec<String>,
    errors: Vec<String>,
}

fn record_vanished(result: &mut FreezeResult, path: &Path) {
    result.vanished += 1;
    sample_path(&mut result.vanished_paths, path.display().to_string());
}

/// Oracle freeze.go:374-396 returns these four fields together.
#[derive(Debug)]
pub struct ThawIfGeneration {
    pub current: u64,
    pub matched: bool,
    pub result: ThawResult,
    pub error: Option<String>,
}

/// Oracle freeze.go:132-138.
pub(crate) fn freeze_poll_interval() -> Duration {
    Duration::from_millis(2)
}
