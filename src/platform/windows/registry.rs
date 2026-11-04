//! The Windows workload registry and its spawn gate.
//!
//! Linux discovers the freeze targets from cgroup membership; Windows needs
//! the process runtime to tell it which processes belong to the workload. This
//! registry is that bookkeeping: the process slice registers a pid once it has
//! started (inside the gate), the exit supervisor unregisters it, and the
//! resource slice snapshots the sorted User/Pty pids for a legacy freeze.
//!
//! The gate mirrors the Linux spawn barrier ([`crate::platform::linux::cgroups::spawn`]):
//! a freeze takes the whole semaphore weight so an in-flight start finishes
//! before the sweep enumerates threads, and new starts either wait briefly for
//! the gate to come back down or are refused with a retryable
//! [`SpawnError::WorkloadFrozen`]. Unlike the Linux barrier a Windows start is
//! not placed into a frozen kernel object by its parent, so a process that
//! slips in during the drain is caught by the next sweep's snapshot rather
//! than deadlocking the spawner.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{timeout, Instant};

/// The same "many readers, one writer" weight as the Linux barrier: the
/// semaphore is a gate, not a concurrency limit.
const MAX_SPAWN_WEIGHT: u32 = 1 << 20;
/// Bounds how long a start waits behind a raising gate before it is refused.
const SPAWN_ADMIT_WAIT: Duration = Duration::from_millis(100);
/// Bounds a freeze's wait for in-flight starts. Timing out aborts the freeze.
const SPAWN_DRAIN_WAIT: Duration = Duration::from_millis(250);

/// Which part of the sandbox a tracked process belongs to.
///
/// `System` is deliberately never returned by [`WorkloadRegistry::workload_pids`]:
/// those are the daemon's own helpers and must survive every freeze, exactly
/// like Linux `ProcessType::System`, which is never registered in a workload
/// cgroup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkloadClass {
    User,
    Pty,
    System,
}

impl WorkloadClass {
    /// Linux `WORKLOAD_PROCESS_TYPES` is User+Pty; System is excluded.
    fn is_workload(self) -> bool {
        matches!(self, WorkloadClass::User | WorkloadClass::Pty)
    }
}

/// One registered pid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedProcess {
    pub pid: u32,
    pub tag: Option<String>,
    pub class: WorkloadClass,
}

/// The errors the spawn gate can return. `WorkloadFrozen` is retryable at the
/// RPC boundary, not invalid-argument: the condition ends without the caller
/// changing its request.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpawnError {
    #[error("the workload is frozen")]
    WorkloadFrozen,
    #[error("timed out waiting for an in-flight process spawn")]
    SpawnDrainTimeout,
}

/// One start admitted by the gate. Dropping it returns the weight exactly
/// once, the way the Linux `SpawnGuard` does.
#[derive(Debug)]
pub struct SpawnPermit {
    _permit: Option<OwnedSemaphorePermit>,
}

/// Sorted pid bookkeeping plus the freeze/spawn mutual exclusion.
#[derive(Debug)]
pub struct WorkloadRegistry {
    live: Mutex<BTreeMap<u32, TrackedProcess>>,
    semaphore: Arc<Semaphore>,
    held: AtomicBool,
    /// Serializes raises so two freezes cannot both queue for the full weight.
    raise_lock: tokio::sync::Mutex<()>,
    /// The weight taken by `raise_and_drain`, held until `lower`. Storing it
    /// returns the permits exactly once through `Drop`; `mem::forget` would
    /// leak one `Arc<Semaphore>` refcount per freeze.
    held_permit: Mutex<Option<OwnedSemaphorePermit>>,
    admit_wait: Duration,
    drain_wait: Duration,
}

impl Default for WorkloadRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkloadRegistry {
    pub fn new() -> Self {
        Self::with_waits(SPAWN_ADMIT_WAIT, SPAWN_DRAIN_WAIT)
    }

    /// Test/tuning seam: the production bounds are the constants above.
    pub(crate) fn with_waits(admit_wait: Duration, drain_wait: Duration) -> Self {
        Self {
            live: Mutex::new(BTreeMap::new()),
            semaphore: Arc::new(Semaphore::new(MAX_SPAWN_WEIGHT as usize)),
            held: AtomicBool::new(false),
            raise_lock: tokio::sync::Mutex::new(()),
            held_permit: Mutex::new(None),
            admit_wait,
            drain_wait,
        }
    }

    /// Records a pid the process runtime has just started. Re-registering the
    /// same pid replaces its tag/class, which keeps a recycled pid from
    /// resurrecting stale metadata.
    pub fn register(&self, pid: u32, class: WorkloadClass, tag: Option<String>) {
        self.live
            .lock()
            .expect("workload registry mutex")
            .insert(pid, TrackedProcess { pid, tag, class });
    }

    /// Removes a pid. Unknown pids are ignored: the exit supervisor and the
    /// start-failure path can race.
    pub fn unregister(&self, pid: u32) {
        self.live
            .lock()
            .expect("workload registry mutex")
            .remove(&pid);
    }

    /// The legacy freeze target set: sorted User/Pty pids, System excluded.
    pub fn workload_pids(&self) -> Vec<u32> {
        self.live
            .lock()
            .expect("workload registry mutex")
            .values()
            .filter(|process| process.class.is_workload())
            .map(|process| process.pid)
            .collect()
    }

    /// Whether any pid is currently tracked (including System helpers).
    pub fn is_empty(&self) -> bool {
        self.live
            .lock()
            .expect("workload registry mutex")
            .is_empty()
    }

    fn is_held(&self) -> bool {
        self.held.load(Ordering::Acquire)
    }

    /// Admits one start, mirroring the Linux barrier's `enter`.
    ///
    /// The uncontended path takes a permit immediately. While the gate is
    /// being raised, the start waits up to the admit bound for the weight to
    /// come back and is then refused with [`SpawnError::WorkloadFrozen`] so a
    /// process manager can retry instead of hanging for the whole pause.
    pub async fn begin_spawn(&self) -> Result<SpawnPermit, SpawnError> {
        if let Ok(permit) = self.semaphore.clone().try_acquire_owned() {
            return Ok(SpawnPermit {
                _permit: Some(permit),
            });
        }
        match timeout(self.admit_wait, self.semaphore.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(SpawnPermit {
                _permit: Some(permit),
            }),
            // A closed semaphore is never constructed; treat it like a
            // refused admission all the same.
            Ok(Err(_)) | Err(_) => Err(SpawnError::WorkloadFrozen),
        }
    }

    /// Raises the gate and waits for every in-flight start to finish.
    ///
    /// On timeout it acquires nothing and returns
    /// [`SpawnError::SpawnDrainTimeout`]; the caller must not freeze. The
    /// optional deadline shortens the fixed drain bound (used by tests and by
    /// callers that already have a tighter budget).
    pub async fn raise_and_drain(&self, deadline: Option<Instant>) -> Result<(), SpawnError> {
        let _serial = self.raise_lock.lock().await;
        if self.is_held() {
            return Ok(());
        }
        let bound = match deadline {
            Some(at) => self
                .drain_wait
                .min(at.saturating_duration_since(Instant::now())),
            None => self.drain_wait,
        };
        match timeout(
            bound,
            self.semaphore.clone().acquire_many_owned(MAX_SPAWN_WEIGHT),
        )
        .await
        {
            Ok(Ok(permit)) => {
                // The weight stays out until `lower`, holding the frozen
                // window open for the whole sweep. Stored before `held` is
                // set, so a concurrent `lower` either sees neither or takes
                // the permit.
                let mut slot = self.held_permit.lock().expect("held permit mutex");
                if slot.is_none() {
                    *slot = Some(permit);
                    self.held.store(true, Ordering::Release);
                }
                Ok(())
            }
            Ok(Err(_)) | Err(_) => Err(SpawnError::SpawnDrainTimeout),
        }
    }

    /// Lowers the gate. Idempotent: several thaw paths can run for one freeze.
    pub fn lower(&self) {
        let mut slot = self.held_permit.lock().expect("held permit mutex");
        if self.held.swap(false, Ordering::AcqRel) {
            drop(slot.take());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_snapshots_pids_in_order_and_gates_spawns() {
        let registry = WorkloadRegistry::new();
        registry.register(30, WorkloadClass::User, Some("u30".into()));
        registry.register(10, WorkloadClass::Pty, None);
        registry.register(20, WorkloadClass::System, Some("helper".into()));
        registry.register(40, WorkloadClass::User, None);
        assert_eq!(registry.workload_pids(), vec![10, 30, 40]);
        registry.unregister(30);
        assert_eq!(registry.workload_pids(), vec![10, 40]);
        // Unknown pids are ignored.
        registry.unregister(99);
        assert_eq!(registry.workload_pids(), vec![10, 40]);
        // Re-registering replaces the class, so a recycled pid cannot stay a
        // workload by accident.
        registry.register(10, WorkloadClass::System, None);
        assert_eq!(registry.workload_pids(), vec![40]);
    }

    #[tokio::test]
    async fn begin_spawn_waits_while_the_gate_is_raised_and_resumes_after_lower() {
        let registry =
            WorkloadRegistry::with_waits(Duration::from_millis(500), Duration::from_millis(20));
        registry.raise_and_drain(None).await.expect("raise");
        // The gate is raised; an admission is not granted immediately.
        assert!(
            timeout(Duration::from_millis(20), registry.begin_spawn())
                .await
                .is_err(),
            "a raised gate must not admit a spawn instantly"
        );
        registry.lower();
        let permit = timeout(Duration::from_millis(100), registry.begin_spawn())
            .await
            .expect("the lowered gate admits")
            .expect("permit");
        drop(permit);
    }

    #[tokio::test]
    async fn raise_and_drain_waits_for_in_flight_starts_and_times_out() {
        let registry =
            WorkloadRegistry::with_waits(Duration::from_millis(20), Duration::from_millis(20));
        let permit = registry.begin_spawn().await.expect("admit");
        assert_eq!(
            registry.raise_and_drain(None).await,
            Err(SpawnError::SpawnDrainTimeout)
        );
        drop(permit);
        registry.lower();
        registry.raise_and_drain(None).await.expect("retry raises");
        registry.lower();
    }
}
