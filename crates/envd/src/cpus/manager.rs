// SPDX-License-Identifier: Apache-2.0
use super::sysfs::{invalid, read_bounded};
use super::{ChildWrites, CpuSysfs};
use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex, Once},
    time::Duration,
};
use tokio::{
    sync::{oneshot, watch, Notify},
    time::{sleep, Instant},
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct State {
    pub online: usize,
    pub possible: usize,
    pub target: i64,
    pub attempts: usize,
    pub write_pending: Duration,
}
#[derive(Default)]
struct Target {
    value: i64,
    attempts: usize,
}
struct Real {
    sys: Arc<dyn CpuSysfs>,
    children: Arc<dyn ChildWrites>,
    target_file: PathBuf,
    target: Mutex<Target>,
    wake: Notify,
    pending: Arc<Mutex<Option<Instant>>>,
    mask_warned: Once,
}
pub struct CpuManager {
    real: Option<Arc<Real>>,
}
impl CpuManager {
    pub fn noop() -> Self {
        Self { real: None }
    }
    pub fn new(
        sys: Arc<dyn CpuSysfs>,
        children: Arc<dyn ChildWrites>,
        target_file: PathBuf,
    ) -> Self {
        Self {
            real: Some(Arc::new(Real {
                sys,
                children,
                target_file,
                target: Mutex::new(Target::default()),
                wake: Notify::new(),
                pending: Arc::new(Mutex::new(None)),
                mask_warned: Once::new(),
            })),
        }
    }
    pub fn set_target(&self, target: i64) -> io::Result<()> {
        let Some(real) = &self.real else {
            return Ok(());
        };
        real.validate(target)?;
        let mut state = real
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let parent = real
            .target_file
            .parent()
            .unwrap_or(std::path::Path::new("."));
        fs::create_dir_all(parent)?;
        let mut temp = real.target_file.as_os_str().to_owned();
        temp.push(".tmp");
        fs::write(&temp, target.to_string())?;
        fs::rename(temp, &real.target_file)?;
        state.value = target;
        state.attempts = 0;
        real.wake.notify_one();
        Ok(())
    }
    pub fn status(&self) -> State {
        let Some(real) = &self.real else {
            return State::default();
        };
        let possible = real.sys.possible();
        let online = if possible.is_ok() {
            real.sys.online()
        } else {
            Ok(vec![])
        };
        if possible.is_err() || online.is_err() {
            real.mask_warned
                .call_once(|| eprintln!("cpus: cannot read CPU masks, reporting zero counts"));
        }
        let pending = real
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map(|start| start.elapsed())
            .unwrap_or_default();
        let target = real
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        State {
            online: online.unwrap_or_default().len(),
            possible: possible.unwrap_or_default().len(),
            target: target.value,
            attempts: target.attempts,
            write_pending: pending,
        }
    }
    /// Run once per manager. Cancellation never kills a pending write; its owning
    /// OS thread retains the helper, registry entry and monotonic pending timestamp.
    pub async fn run(&self, mut stop: oneshot::Receiver<()>) {
        let Some(real) = &self.real else {
            return;
        };
        let (cancel, canceled) = watch::channel(false);
        tokio::select! { biased; _ = &mut stop => {}, _ = real.worker(canceled) => {} }
        let _ = cancel.send(true);
    }
    #[cfg(test)]
    fn backoff(&self, stalled: usize) -> Duration {
        backoff(stalled)
    }
}

impl Real {
    fn validate(&self, target: i64) -> io::Result<()> {
        let possible = self.sys.possible()?.len();
        if target < 1 || target > possible as i64 {
            return Err(invalid(format!(
                "cpu target {target} outside 1..{possible}"
            )));
        }
        Ok(())
    }
    fn load_target(&self) -> io::Result<i64> {
        let label = self.target_file.display().to_string();
        let bytes = read_bounded(&self.target_file, 64, &label)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid(format!("{label}: invalid syntax")))?
            .trim();
        let value = go_atoi(text).map_err(|reason| {
            invalid(format!("{label}: strconv.Atoi: parsing {text:?}: {reason}"))
        })?;
        self.validate(value)?;
        Ok(value)
    }
    async fn worker(self: &Arc<Self>, canceled: watch::Receiver<bool>) {
        match self.load_target() {
            Ok(value) => {
                let mut target = self
                    .target
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // Zero means no accepted target yet. SetTarget validates >=1 and
                // persists under this lock; restoration must never replace it,
                // even if load_target was paused while validating an older file.
                if target.value == 0 {
                    target.value = value;
                    self.wake.notify_one();
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("cpus: ignoring persisted target: {error}"),
        }
        let children = self.children.clone();
        let pending = self.pending.clone();
        if background(pending, move || {
            children.wait_adopted();
            Ok(())
        })
        .await
        .is_err()
        {
            return;
        }
        loop {
            self.wake.notified().await;
            self.converge(&canceled).await;
        }
    }
    async fn converge(&self, canceled: &watch::Receiver<bool>) {
        let mut stalled: usize = 0;
        loop {
            let (changed, error) = self.reconcile(canceled).await;
            if *canceled.borrow() {
                return;
            }
            {
                let mut target = self
                    .target
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                target.attempts += 1;
            }
            let Some(error) = error else {
                return;
            };
            stalled = if changed > 0 { 0 } else { stalled + 1 };
            if stalled >= 10 {
                eprintln!("cpus: giving up on target until a new one arrives: {error}");
                return;
            }
            tokio::select! { _ = self.wake.notified() => stalled = 0, _ = sleep(backoff(stalled)) => {} }
        }
    }
    async fn reconcile(&self, canceled: &watch::Receiver<bool>) -> (usize, Option<io::Error>) {
        let possible = match self.sys.possible() {
            Ok(value) => value,
            Err(error) => return (0, Some(error)),
        };
        let mut changed = 0;
        loop {
            if *canceled.borrow() {
                return (
                    changed,
                    Some(io::Error::new(io::ErrorKind::Interrupted, "cancelled")),
                );
            }
            let target = self
                .target
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .value;
            let online = match self.sys.online() {
                Ok(value) => value,
                Err(error) => return (changed, Some(error)),
            };
            if online.len() as i64 == target {
                return (changed, None);
            }
            let up = (online.len() as i64) < target;
            let cpu = if up {
                match possible
                    .iter()
                    .copied()
                    .find(|cpu| *cpu != 0 && !online.contains(cpu))
                {
                    Some(cpu) => cpu,
                    None => {
                        return (
                            changed,
                            Some(invalid(format!(
                                "cpu target {target} exceeds {} possible CPUs",
                                possible.len()
                            ))),
                        )
                    }
                }
            } else {
                match online.last().copied() {
                    Some(cpu) if cpu != 0 => cpu,
                    _ => {
                        return (
                            changed,
                            Some(invalid(format!(
                                "cpu target {target} leaves only CPU0, which cannot go offline"
                            ))),
                        )
                    }
                }
            };
            let sys = self.sys.clone();
            if let Err(error) =
                background(self.pending.clone(), move || sys.set_online(cpu, up)).await
            {
                return (
                    changed,
                    Some(io::Error::new(
                        error.kind(),
                        format!("set cpu{cpu} online={up}: {error}"),
                    )),
                );
            }
            let online = match self.sys.online() {
                Ok(value) => value,
                Err(error) => return (changed, Some(error)),
            };
            if online.contains(&cpu) != up {
                return (
                    changed,
                    Some(invalid(format!(
                        "cpu{cpu}: write reported success but the online mask did not change"
                    ))),
                );
            }
            changed += 1;
        }
    }
}
// Go Atoi's slow path parses an unsigned magnitude before checking the signed
// limit. Preserve whether a bad digit or a uint64 overflow is encountered first.
fn go_atoi(text: &str) -> Result<i64, &'static str> {
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() {
        return Err("invalid syntax");
    }
    let mut value = 0_u64;
    for digit in digits.bytes() {
        if !digit.is_ascii_digit() {
            return Err("invalid syntax");
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit - b'0')))
            .ok_or("value out of range")?;
    }
    let limit = 1_u64 << (isize::BITS - 1);
    if value > limit || (!negative && value == limit) {
        return Err("value out of range");
    }
    Ok(if negative {
        value.wrapping_neg() as i64
    } else {
        value as i64
    })
}

fn backoff(stalled: usize) -> Duration {
    Duration::from_secs(if stalled <= 1 {
        1
    } else {
        1_u64
            .checked_shl((stalled - 1).min(5) as u32)
            .unwrap_or(30)
            .min(30)
    })
}
struct Pending(Arc<Mutex<Option<Instant>>>);
impl Drop for Pending {
    fn drop(&mut self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}
async fn background(
    pending: Arc<Mutex<Option<Instant>>>,
    work: impl FnOnce() -> io::Result<()> + Send + 'static,
) -> io::Result<()> {
    *pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    let (reply, response) = oneshot::channel();
    let guard = Pending(pending);
    std::thread::Builder::new()
        .name("envd-cpu-write".into())
        .spawn(move || {
            let result = work();
            drop(guard);
            let _ = reply.send(result);
        })?;
    response
        .await
        .map_err(|_| io::Error::other("cpu write worker stopped"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Masks;
    impl CpuSysfs for Masks {
        fn possible(&self) -> io::Result<Vec<usize>> {
            Ok((0..8).collect())
        }
        fn online(&self) -> io::Result<Vec<usize>> {
            Ok(vec![0])
        }
        fn set_online(&self, _: usize, _: bool) -> io::Result<()> {
            panic!("unexpected write")
        }
    }
    struct Children;
    impl ChildWrites for Children {
        fn write(&self, _: &std::path::Path, _: bool) -> io::Result<()> {
            panic!("unexpected write")
        }
        fn wait_adopted(&self) {}
    }
    #[test]
    fn backoff_is_one_second_doubling_to_thirty_seconds() {
        let manager = CpuManager::new(Arc::new(Masks), Arc::new(Children), "/unused".into());
        for (stalled, seconds) in [
            (0, 1),
            (1, 1),
            (2, 2),
            (3, 4),
            (4, 8),
            (5, 16),
            (6, 30),
            (100, 30),
        ] {
            assert_eq!(manager.backoff(stalled), Duration::from_secs(seconds));
        }
    }
    struct MutableSys {
        online: Mutex<Vec<usize>>,
        calls: std::sync::atomic::AtomicUsize,
        hook: Box<dyn Fn(usize, bool, usize) -> io::Result<bool> + Send + Sync>,
    }
    impl CpuSysfs for MutableSys {
        fn possible(&self) -> io::Result<Vec<usize>> {
            Ok((0..32).collect())
        }
        fn online(&self) -> io::Result<Vec<usize>> {
            Ok(self.online.lock().unwrap().clone())
        }
        fn set_online(&self, cpu: usize, up: bool) -> io::Result<()> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if (self.hook)(cpu, up, call)? {
                let mut online = self.online.lock().unwrap();
                online.retain(|value| *value != cpu);
                if up {
                    online.push(cpu);
                    online.sort_unstable();
                }
            }
            Ok(())
        }
    }
    fn mutable(
        hook: impl Fn(usize, bool, usize) -> io::Result<bool> + Send + Sync + 'static,
    ) -> Arc<MutableSys> {
        Arc::new(MutableSys {
            online: Mutex::new(vec![0]),
            calls: std::sync::atomic::AtomicUsize::new(0),
            hook: Box::new(hook),
        })
    }
    fn real(sys: Arc<dyn CpuSysfs>, path: PathBuf) -> Arc<Real> {
        CpuManager::new(sys, Arc::new(Children), path).real.unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_runs_stop_after_ten_and_progress_resets_budget() {
        for progress in [false, true] {
            let sys = mutable(move |_, _, call| {
                if progress && call == 0 {
                    Ok(true)
                } else {
                    Err(io::Error::other("injected EIO"))
                }
            });
            let dir = tempfile::tempdir().unwrap();
            let real = real(sys.clone(), dir.path().join("target"));
            real.target.lock().unwrap().value = 4;
            let (_, stop) = watch::channel(false);
            real.converge(&stop).await;
            assert_eq!(
                real.target.lock().unwrap().attempts,
                if progress { 11 } else { 10 }
            );
            assert_eq!(
                sys.calls.load(std::sync::atomic::Ordering::SeqCst),
                if progress { 12 } else { 10 }
            );
        }
    }

    #[tokio::test]
    async fn no_effect_write_errors_instead_of_looping_and_preserves_progress() {
        let sys = mutable(|_, _, call| Ok(call == 0));
        let dir = tempfile::tempdir().unwrap();
        let real = real(sys, dir.path().join("target"));
        real.target.lock().unwrap().value = 4;
        let (_, stop) = watch::channel(false);
        let (changed, error) = real.reconcile(&stop).await;
        assert_eq!(changed, 1);
        assert_eq!(
            error.unwrap().to_string(),
            "cpu2: write reported success but the online mask did not change"
        );
    }

    #[tokio::test]
    async fn pending_write_survives_cancellation_and_reports_monotonic_elapsed() {
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (entered, mut entry) = tokio::sync::mpsc::unbounded_channel();
        let worker_gate = gate.clone();
        let sys = mutable(move |_, _, _| {
            entered.send(()).unwrap();
            let (lock, ready) = &*worker_gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = ready.wait(open).unwrap();
            }
            Ok(true)
        });
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(CpuManager::new(
            sys.clone(),
            Arc::new(Children),
            dir.path().join("target"),
        ));
        manager.set_target(2).unwrap();
        let (cancel, stop) = oneshot::channel();
        let run = tokio::spawn({
            let manager = manager.clone();
            async move { manager.run(stop).await }
        });
        entry.recv().await.unwrap();
        let pending = manager.status().write_pending;
        assert!(pending > Duration::ZERO);
        cancel.send(()).unwrap();
        run.await.unwrap();
        assert!(manager.status().write_pending >= pending);
        assert_eq!(sys.online().unwrap(), vec![0]);
        {
            let (lock, ready) = &*gate;
            *lock.lock().unwrap() = true;
            ready.notify_all();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.status().write_pending != Duration::ZERO {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(sys.online().unwrap(), vec![0, 1]);
    }

    #[tokio::test]
    async fn latest_target_is_read_after_each_completed_write() {
        let weak_real = Arc::new(Mutex::new(std::sync::Weak::<Real>::new()));
        let hook_real = weak_real.clone();
        let sys = mutable(move |cpu, up, _| {
            if cpu == 2 && up {
                hook_real
                    .lock()
                    .unwrap()
                    .upgrade()
                    .unwrap()
                    .target
                    .lock()
                    .unwrap()
                    .value = 2;
            }
            Ok(true)
        });
        let dir = tempfile::tempdir().unwrap();
        let real = real(sys.clone(), dir.path().join("target"));
        *weak_real.lock().unwrap() = Arc::downgrade(&real);
        real.target.lock().unwrap().value = 8;
        let (_, stop) = watch::channel(false);
        assert!(real.reconcile(&stop).await.1.is_none());
        assert_eq!(sys.online().unwrap(), vec![0, 1]);
        assert_eq!(sys.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_restore_cannot_overwrite_target_accepted_during_validation() {
        struct BlockingMasks {
            first: std::sync::atomic::AtomicBool,
            entered: tokio::sync::mpsc::UnboundedSender<()>,
            gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
            online: Mutex<Vec<usize>>,
        }
        impl CpuSysfs for BlockingMasks {
            fn possible(&self) -> io::Result<Vec<usize>> {
                if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    self.entered.send(()).unwrap();
                    let (lock, changed) = &*self.gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = changed.wait(released).unwrap();
                    }
                }
                Ok((0..8).collect())
            }
            fn online(&self) -> io::Result<Vec<usize>> {
                Ok(self.online.lock().unwrap().clone())
            }
            fn set_online(&self, cpu: usize, up: bool) -> io::Result<()> {
                let mut online = self.online.lock().unwrap();
                online.retain(|value| *value != cpu);
                if up {
                    online.push(cpu);
                    online.sort_unstable();
                }
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        fs::write(&path, "2").unwrap();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (entered, mut entry) = tokio::sync::mpsc::unbounded_channel();
        let sys = Arc::new(BlockingMasks {
            first: std::sync::atomic::AtomicBool::new(true),
            entered,
            gate: gate.clone(),
            online: Mutex::new(vec![0, 1, 2, 3]),
        });
        let manager = Arc::new(CpuManager::new(sys, Arc::new(Children), path.clone()));
        let (cancel, stop) = oneshot::channel();
        let run = tokio::spawn({
            let manager = manager.clone();
            async move { manager.run(stop).await }
        });
        entry.recv().await.unwrap();
        // Startup has read "2" and is now paused in its mask validation.
        manager.set_target(4).unwrap();
        let disk = fs::read_to_string(&path).unwrap();
        {
            let (lock, changed) = &*gate;
            *lock.lock().unwrap() = true;
            changed.notify_all();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.status().attempts == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let status = manager.status();
        cancel.send(()).unwrap();
        run.await.unwrap();
        assert_eq!(disk, "4");
        assert_eq!(fs::read_to_string(path).unwrap(), "4");
        assert_eq!(status.target, 4);
        assert_eq!(status.online, 4);
    }

    #[test]
    fn persisted_target_atoi_errors_distinguish_syntax_and_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        let real = real(Arc::new(Masks), path.clone());
        for (text, reason) in [
            ("9223372036854775808", "value out of range"),
            ("-9223372036854775809", "value out of range"),
            ("+99999999999999999999999", "value out of range"),
            ("99999999999999999999999x", "value out of range"),
            ("9223372036854775808x", "invalid syntax"),
            ("++1", "invalid syntax"),
            ("", "invalid syntax"),
        ] {
            fs::write(&path, text).unwrap();
            assert_eq!(
                real.load_target().unwrap_err().to_string(),
                format!(
                    "{}: strconv.Atoi: parsing {text:?}: {reason}",
                    path.display()
                )
            );
        }
    }

    #[tokio::test]
    async fn adopted_write_finishes_before_reconcile_and_clears_pending() {
        struct Adopted {
            gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
            entered: tokio::sync::mpsc::UnboundedSender<()>,
        }
        impl ChildWrites for Adopted {
            fn write(&self, _: &std::path::Path, _: bool) -> io::Result<()> {
                panic!("unexpected helper")
            }
            fn wait_adopted(&self) {
                self.entered.send(()).unwrap();
                let (lock, changed) = &*self.gate;
                let mut finished = lock.lock().unwrap();
                while !*finished {
                    finished = changed.wait(finished).unwrap();
                }
            }
        }
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (entered, mut entry) = tokio::sync::mpsc::unbounded_channel();
        let sys = mutable(|_, _, _| Ok(true));
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(CpuManager::new(
            sys.clone(),
            Arc::new(Adopted {
                gate: gate.clone(),
                entered,
            }),
            dir.path().join("target"),
        ));
        manager.set_target(1).unwrap();
        let (cancel, stop) = oneshot::channel();
        let run = tokio::spawn({
            let manager = manager.clone();
            async move { manager.run(stop).await }
        });
        entry.recv().await.unwrap();
        assert!(manager.status().write_pending > Duration::ZERO);
        assert_eq!(sys.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        // Simulate the inherited child landing while this manager waits for it.
        *sys.online.lock().unwrap() = vec![0, 1];
        {
            let (lock, changed) = &*gate;
            *lock.lock().unwrap() = true;
            changed.notify_all();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.status().attempts == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(sys.online().unwrap(), vec![0]);
        assert_eq!(sys.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(manager.status().write_pending, Duration::ZERO);
        cancel.send(()).unwrap();
        run.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn new_target_during_backoff_gets_full_stall_budget() {
        let sys = mutable(|_, _, _| Err(io::Error::other("injected EIO")));
        let dir = tempfile::tempdir().unwrap();
        let manager = CpuManager::new(sys.clone(), Arc::new(Children), dir.path().join("target"));
        let real = manager.real.as_ref().unwrap().clone();
        real.target.lock().unwrap().value = 8;
        let (_, stop) = watch::channel(false);
        let converge = real.converge(&stop);
        tokio::pin!(converge);
        while real.target.lock().unwrap().attempts < 3 {
            tokio::select! { biased; _ = &mut converge => panic!("gave up early"), _ = tokio::task::yield_now() => {} }
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        manager.set_target(6).unwrap();
        converge.await;
        assert_eq!(manager.status().attempts, 10);
        assert_eq!(sys.calls.load(std::sync::atomic::Ordering::SeqCst), 13);
    }

    #[test]
    fn target_validation_does_not_persist_invalid_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        let manager = CpuManager::new(Arc::new(Masks), Arc::new(Children), path.clone());
        for value in [i64::MIN, -1, 0, 9, i64::MAX] {
            assert_eq!(
                manager.set_target(value).unwrap_err().to_string(),
                format!("cpu target {value} outside 1..8")
            );
        }
        assert!(!path.exists());
        assert_eq!(manager.status().target, 0);
    }
}
