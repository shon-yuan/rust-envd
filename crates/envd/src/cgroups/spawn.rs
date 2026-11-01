//! The spawn barrier, ported from oracle spawn.go (294 lines).
//!
//! The hazard it closes (spawn.go:65-86): a process placed with
//! clone3(CLONE_INTO_CGROUP) into an already-frozen cgroup is stopped by the
//! freezer on its first return from the fork, before it reaches execve — and
//! the Go fork/exec path carries CLONE_VM|CLONE_VFORK, so the spawning thread
//! blocks in the kernel until the child execs. It never does. The runtime
//! cannot preempt that thread, so the next garbage-collection
//! stop-the-world waits forever: every goroutine in envd stops, the resume thaw
//! and the thaw watchdog included. The barrier is therefore held from before a
//! sweep writes cgroup.freeze until the thaw of the cgroups spawns land in —
//! the WHOLE frozen window, not merely the sweep.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

use super::freeze::WorkloadFreezer;
use super::hierarchy::ancestor_chain;
use super::{ProcessType, WORKLOAD_PROCESS_TYPES};

/// Oracle spawn.go:19. Retryable at the RPC boundary, not invalid-argument:
/// the state ends without the caller changing its request.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("the target cgroup is frozen")]
    WorkloadFrozen,
    #[error("the target cgroup is frozen: {0}")]
    WorkloadFrozenPath(String),
    #[error("timed out waiting for an in-flight process spawn")]
    SpawnDrainTimeout,
    /// Oracle start_error.go:32-34 via spawn.go:197-201: the caller's own
    /// context ending during admission is ITS outcome —
    /// context.DeadlineExceeded codes DeadlineExceeded (start.go:184 wraps it
    /// into "error starting process ..."). Only the barrier's admit bound
    /// running out means WorkloadFrozen.
    #[error("context deadline exceeded")]
    DeadlineExceeded,
    /// Oracle upgrade-side wrap (freeze.go:1083-1086): ResumeFrozen re-raises
    /// the barrier for an inherited freeze and names the operation in the
    /// error. The only source today is the drain timeout.
    #[error("raise the spawn barrier for an inherited freeze: {0}")]
    InheritedRaise(#[source] Box<SpawnError>),
}

/// Oracle spawn.go:36: the semaphore's capacity, not a concurrency limit —
/// a spawn takes 1 and the barrier takes all of it ("many readers, one
/// writer"). Deliberately far above any real in-flight count.
const MAX_SPAWN_WEIGHT: u32 = 1 << 20;

/// Oracle spawn.go:47: bounds a spawn's wait when the barrier is being raised
/// rather than held. Deliberately NOT the established frozen window, which
/// lasts for a pause and a resume.
pub const SPAWN_ADMIT_WAIT: Duration = Duration::from_millis(100);

/// Oracle spawn.go:63: bounds a freeze's wait for in-flight spawns. Timing out
/// aborts the freeze — the conservative direction (a pause that captures a
/// running workload is the behaviour a failed freeze already has).
pub const SPAWN_DRAIN_WAIT: Duration = Duration::from_millis(250);

/// A guard admitting one spawn; dropping it returns the weight, exactly once.
/// Go returns a release func the caller must invoke once the FORK has returned
/// (spawn.go:217-221); the Rust guard makes the once-part structural
/// (spawn.go:211-215). `None` admits nothing — a socat spawn takes no
/// admission, so there is nothing to hand back.
#[derive(Debug)]
pub struct SpawnGuard {
    // Held, never read: the permit's lifetime IS the admission.
    _permit: Option<OwnedSemaphorePermit>,
}

pub(crate) struct SpawnBarrier {
    semaphore: Arc<Semaphore>,
    admit_wait: Duration,
    drain_wait: Duration,
    /// Serializes raises so two freezes cannot both queue for the full weight
    /// (spawn.go:95-100). Held across the drain, hence an async mutex.
    raise_lock: tokio::sync::Mutex<()>,
    /// The barrier is up: spawns are refused outright instead of waiting
    /// (spawn.go:104-110). Makes raise and lower idempotent, which they must
    /// be — a second freeze inside the same window is normal, and several thaw
    /// paths can run for one freeze.
    held: std::sync::atomic::AtomicBool,
}

impl SpawnBarrier {
    pub(crate) fn new() -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(MAX_SPAWN_WEIGHT as usize)),
            admit_wait: SPAWN_ADMIT_WAIT,
            drain_wait: SPAWN_DRAIN_WAIT,
            raise_lock: tokio::sync::Mutex::new(()),
            held: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn is_held(&self) -> bool {
        self.held.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Oracle spawn.go:132-159. Bounded by the drain wait; on timeout it
    /// acquires NOTHING — the caller must not freeze.
    pub(crate) async fn raise(&self) -> Result<(), SpawnError> {
        let _serial = self.raise_lock.lock().await;
        if self.is_held() {
            return Ok(());
        }
        match timeout(
            self.drain_wait,
            self.semaphore.clone().acquire_many(MAX_SPAWN_WEIGHT),
        )
        .await
        {
            Ok(Ok(permit)) => {
                // The weight stays OUT until lower() hands it back — the
                // barrier holds the window open for the whole freeze.
                std::mem::forget(permit);
                self.held.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            }
            // The semaphore is never closed; a closed error is treated like a
            // refused drain all the same.
            Ok(Err(_)) | Err(_) => Err(SpawnError::SpawnDrainTimeout),
        }
    }

    /// Oracle spawn.go:164-173. Idempotent: one freeze can be followed by
    /// several thaws and the first to clear the static cgroups lowers it.
    pub(crate) fn lower(&self) {
        if self.held.swap(false, std::sync::atomic::Ordering::AcqRel) {
            self.semaphore.add_permits(MAX_SPAWN_WEIGHT as usize);
        }
    }

    /// Oracle spawn.go:183-206. Barrier UP fails immediately — the window
    /// lasts a pause and a resume, so waiting in it would replace a prompt
    /// retryable error with a client timeout. Behind a queued raise, the FIFO
    /// wait is bounded by the admit wait AND the caller's own deadline: the
    /// oracle derives admitCtx from the caller's context
    /// (spawn.go:196 context.WithTimeout(ctx, b.admitWait)), so an already
    /// armed request timeout ends the wait early, and when the wait gives up
    /// the caller's own deadline outranks the barrier's bound
    /// (spawn.go:197-201: ctx.Err() is reported as such; only OUR bound
    /// running out means frozen).
    pub(crate) async fn enter(
        &self,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<SpawnGuard, SpawnError> {
        if let Ok(permit) = self.semaphore.clone().try_acquire_owned() {
            return Ok(SpawnGuard {
                _permit: Some(permit),
            });
        }
        if self.is_held() {
            return Err(SpawnError::WorkloadFrozen);
        }
        // The oracle's admitCtx is born already elapsed in this case, so the
        // acquire refuses before waiting; checking first only makes the same
        // outcome deterministic.
        if deadline.is_some_and(|at| at <= tokio::time::Instant::now()) {
            return Err(SpawnError::DeadlineExceeded);
        }
        let wait = match deadline {
            Some(at) => at
                .saturating_duration_since(tokio::time::Instant::now())
                .min(self.admit_wait),
            None => self.admit_wait,
        };
        match timeout(wait, self.semaphore.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(SpawnGuard {
                _permit: Some(permit),
            }),
            // An elapsed caller deadline outranks the admit bound
            // (spawn.go:197-201). A dropped future — the Rust shape of a
            // cancelled caller — never reaches this arm at all: the acquire
            // is dropped with it, no permit is taken, and the spawn that
            // would have followed never starts.
            Ok(Err(_)) | Err(_) => {
                if deadline.is_some_and(|at| at <= tokio::time::Instant::now()) {
                    Err(SpawnError::DeadlineExceeded)
                } else {
                    Err(SpawnError::WorkloadFrozen)
                }
            }
        }
    }
}

/// Oracle spawn.go:236-253, a method on the freezer: admit a spawn destined
/// for `process_type`'s cgroup. Two checks for two sources of a frozen
/// cgroup: the barrier for freezes WE issue (exact, only for the workload
/// cgroups a sweep freezes), the probe for a freeze the GUEST issued
/// (best-effort read immediately before the clone).
impl WorkloadFreezer {
    pub async fn begin_spawn(
        &self,
        process_type: ProcessType,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<SpawnGuard, SpawnError> {
        let admission = if WORKLOAD_PROCESS_TYPES.contains(&process_type) {
            Some(self.spawns.enter(deadline).await?)
        } else {
            // The socat's cgroup is never swept, so its spawn takes only the
            // probe (spawn.go:66-68).
            None
        };
        if let Err(error) = self.probe_thawed(process_type).await {
            drop(admission);
            return Err(error);
        }
        Ok(admission.unwrap_or(SpawnGuard { _permit: None }))
    }

    /// Oracle spawn.go:267-294: refuse a spawn into a cgroup that reads
    /// frozen, or whose ancestor has a freeze REQUESTED — written but not yet
    /// settled still stops a task arriving afterwards. A read that FAILS does
    /// not refuse: the probe guards against a freeze this process cannot see,
    /// and turning an unreadable cgroupfs into a process-start outage would be
    /// a worse failure than the one it protects against.
    async fn probe_thawed(&self, process_type: ProcessType) -> Result<(), SpawnError> {
        let Some(manager) = self.manager().as_path_manager() else {
            // No paths to read, and such a manager hands out no cgroup fd, so
            // the spawn is not placed into a cgroup at all (spawn.go:269-273).
            return Ok(());
        };
        let Some(path) = manager.path_of(process_type) else {
            // ProcessTypeSystem, deliberately unregistered (spawn.go:275-280).
            return Ok(());
        };
        if matches!(manager.frozen_at(&path), Ok(true)) {
            return Err(SpawnError::WorkloadFrozenPath(format!(
                "{} reads frozen",
                path.display()
            )));
        }
        let root = manager.root();
        for ancestor in ancestor_chain(&root, &path) {
            if matches!(manager.freeze_requested_at(&ancestor), Ok(true)) {
                return Err(SpawnError::WorkloadFrozenPath(format!(
                    "{} has a freeze requested",
                    ancestor.display()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod admission_tests {
    use super::{SpawnBarrier, SpawnError, MAX_SPAWN_WEIGHT};
    use std::time::Duration;
    use tokio::time::Instant;

    /// Oracle spawn.go:183-201 with an already-elapsed caller ctx BEHIND A
    /// BUSY BARRIER: TryAcquire fails (spawn.go:184 consults no ctx), the
    /// admitCtx it derives is born done, so Acquire refuses at once and the
    /// caller's own context error comes back (spawn.go:197-201) —
    /// DeadlineExceeded (start_error.go:32-34), never "the target cgroup is
    /// frozen". An uncontended barrier admits regardless of the deadline,
    /// exactly like the oracle's TryAcquire-first order.
    #[tokio::test]
    async fn an_elapsed_caller_deadline_behind_a_busy_barrier_refuses_with_the_deadline_error() {
        let barrier = SpawnBarrier::new();
        let _drain = barrier
            .semaphore
            .clone()
            .acquire_many_owned(MAX_SPAWN_WEIGHT)
            .await
            .unwrap();
        let error = barrier
            .enter(Some(Instant::now()))
            .await
            .expect_err("an elapsed deadline behind the barrier must refuse the admission");
        assert!(matches!(error, SpawnError::DeadlineExceeded), "{error:?}");
        assert_eq!(error.to_string(), "context deadline exceeded");
    }

    /// Oracle spawn.go:196 derives admitCtx from the CALLER's context, so a
    /// caller deadline shorter than the admit bound ends the wait at the
    /// caller's bound and reports the caller's outcome (spawn.go:197-201),
    /// not the barrier's frozen text.
    #[tokio::test]
    async fn a_mid_wait_caller_deadline_outranks_the_admit_bound() {
        let barrier = SpawnBarrier::new();
        // Take the whole weight WITHOUT raising: the barrier is not held, so
        // the enter below takes the bounded-wait path (spawn.go:196).
        let _drain = barrier
            .semaphore
            .clone()
            .acquire_many_owned(MAX_SPAWN_WEIGHT)
            .await
            .unwrap();
        let error = barrier
            .enter(Some(Instant::now() + Duration::from_millis(20)))
            .await
            .expect_err("an elapsed caller deadline must refuse the admission");
        assert!(
            matches!(error, SpawnError::DeadlineExceeded),
            "the caller's own deadline is its outcome, got {error:?}"
        );
    }

    /// Only the barrier's own admit bound running out means WorkloadFrozen
    /// (spawn.go:202-204) — the deadline-only refusals above must not have
    /// collapsed into the frozen text.
    #[tokio::test]
    async fn the_admit_bound_alone_still_refuses_frozen() {
        let barrier = SpawnBarrier::new();
        let _drain = barrier
            .semaphore
            .clone()
            .acquire_many_owned(MAX_SPAWN_WEIGHT)
            .await
            .unwrap();
        let error = barrier
            .enter(None)
            .await
            .expect_err("the admit bound must refuse while the weight is out");
        assert!(matches!(error, SpawnError::WorkloadFrozen), "{error:?}");
    }

    /// spawn.go:197-201 outcome semantics for a cancelled caller: the acquire
    /// dies with the caller's future, so no permit is taken and no spawn
    /// starts — the weight is whole again the moment the holder lets go.
    /// (Post-fork the oracle's procCtx is Background (start.go:40-44), so a
    /// caller vanishing AFTER the fork keeps the child on both sides; this
    /// guard pins the pre-fork half that this module owns.)
    #[tokio::test]
    async fn a_dropped_admission_wait_leaks_no_permit() {
        let barrier = std::sync::Arc::new(SpawnBarrier::new());
        let _drain = barrier
            .semaphore
            .clone()
            .acquire_many_owned(MAX_SPAWN_WEIGHT)
            .await
            .unwrap();
        let waiter = tokio::spawn({
            let barrier = barrier.clone();
            async move { barrier.enter(None).await }
        });
        // Let the waiter reach the bounded wait, then drop it mid-flight.
        tokio::time::sleep(Duration::from_millis(10)).await;
        waiter.abort();
        let _ = waiter.await;
        drop(_drain);
        // A leaked permit would make this enter hang on the admit bound and
        // return WorkloadFrozen; the correct outcome admits immediately.
        let admitted = tokio::time::timeout(Duration::from_millis(50), barrier.enter(None))
            .await
            .expect("the dropped waiter must not have leaked an admission");
        assert!(admitted.is_ok(), "{admitted:?}");
    }
}
