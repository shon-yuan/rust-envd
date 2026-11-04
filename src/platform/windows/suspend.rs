//! Suspending and resuming every thread of a Windows process.
//!
//! cgroup v2 has a kernel freeze switch with read-back state. Win32 does not:
//! suspension is per-thread and counted, so this module has to be explicit
//! about what it did. Every `SuspendThread` records the previous suspend count;
//! a resumed thread therefore keeps any suspend the guest itself installed,
//! and [`resume_process`] removes exactly one increment per recorded tid.
//!
//! Enumeration uses a `TH32CS_SNAPTHREAD` snapshot and is repeated until a
//! pass finds no tid we have not seen, which is the closest local equivalent
//! of "no unsuspended thread remains". A process that exits mid-pass is
//! classified as vanished; an access or per-thread failure is a real failure.
//! This distinction is what keeps a best-effort `/freeze` from reporting an
//! exited child as a broken sandbox.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::mem::size_of;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::Threading::{
    GetProcessIdOfThread, OpenProcess, OpenThread, ResumeThread, SuspendThread,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SUSPEND_RESUME, THREAD_QUERY_LIMITED_INFORMATION,
    THREAD_SUSPEND_RESUME,
};

/// A fixed bound on the "suspend, re-enumerate, suspend the newcomers" loop.
/// A process that creates threads forever is still left with a consistent
/// recorded set; the freeze poll's budget is the real stop.
const MAX_SUSPEND_PASSES: usize = 64;

/// One thread this backend suspended, and whether it was already suspended by
/// the guest before we touched it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuspendedThread {
    pub tid: u32,
    pub was_already_suspended: bool,
}

/// Every thread of one pid that this backend suspended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuspensionSet {
    pub pid: u32,
    pub threads: Vec<SuspendedThread>,
}

impl SuspensionSet {
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            threads: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }
}

/// The outcome of one suspension attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuspendError {
    /// The process exited before or during the sweep. Counted as vanished,
    /// never as a failure.
    Vanished,
    /// The process or one of its threads could not be manipulated. The text
    /// carries the Win32 reason for the handler log.
    Failed(String),
}

impl fmt::Display for SuspendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuspendError::Vanished => formatter.write_str("process vanished"),
            SuspendError::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SuspendError {}

/// Enumerates one snapshot of `pid`'s thread ids.
///
/// An empty list is indistinguishable from "the process is gone" in a
/// Toolhelp snapshot, and a live Windows process always has at least one
/// thread, so callers treat it as [`SuspendError::Vanished`].
pub fn enumerate_threads(pid: u32) -> io::Result<Vec<u32>> {
    // SAFETY: the snapshot call takes scalar arguments; the handle is checked
    // before use and closed on every path below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut threads = Vec::new();
    // SAFETY: `entry` is a live, correctly sized THREADENTRY32 and the
    // snapshot handle is valid; the API writes only into `entry`.
    let mut present = unsafe { Thread32First(snapshot, &mut entry) };
    while present != 0 {
        if entry.th32OwnerProcessID == pid {
            threads.push(entry.th32ThreadID);
        }
        // SAFETY: same buffer and handle; Thread32Next reuses the dwSize the
        // first call established.
        present = unsafe { Thread32Next(snapshot, &mut entry) };
    }
    // SAFETY: the handle came from CreateToolhelp32Snapshot and is not used
    // after this close.
    unsafe {
        CloseHandle(snapshot);
    }
    Ok(threads)
}

fn classify_process_error(pid: u32, error: io::Error) -> SuspendError {
    match error.raw_os_error().map(|code| code as u32) {
        // OpenProcess on an exited pid reports ERROR_INVALID_PARAMETER.
        Some(ERROR_INVALID_PARAMETER) => SuspendError::Vanished,
        _ => SuspendError::Failed(format!("open process {pid}: {error}")),
    }
}

fn open_process(pid: u32) -> Result<HANDLE, SuspendError> {
    // SAFETY: scalar arguments; the returned handle is checked and closed by
    // the caller.
    let handle = unsafe {
        OpenProcess(
            PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(classify_process_error(pid, io::Error::last_os_error()));
    }
    Ok(handle)
}

/// Opens a thread for suspend/resume and verifies it still belongs to `pid`.
///
/// A tid recorded before a thaw can be recycled by an unrelated process after
/// the owner exits; `OpenThread` would then succeed and the resume would
/// decrement that other thread's suspend count. `GetProcessIdOfThread` rejects
/// the impostor. A failed query (0) leaves the decision to the caller: a
/// handle that opened is a real thread, and refusing to resume it would strand
/// a genuine suspension.
fn open_thread(pid: u32, tid: u32) -> io::Result<Option<HANDLE>> {
    // SAFETY: scalar arguments; the handle is checked and closed by the
    // caller.
    let thread = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            0,
            tid,
        )
    };
    if thread.is_null() || thread == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `thread` is a valid handle opened with THREAD_QUERY_LIMITED_INFORMATION.
    let owner = unsafe { GetProcessIdOfThread(thread) };
    if owner != 0 && owner != pid {
        // SAFETY: the handle is ours and not used after this close.
        unsafe {
            CloseHandle(thread);
        }
        return Ok(None);
    }
    Ok(Some(thread))
}

fn resume_one_tid(pid: u32, tid: u32) -> Result<(), String> {
    let thread = match open_thread(pid, tid) {
        Ok(Some(thread)) => thread,
        // The thread exited while we held our suspend, or the tid was
        // recycled: nothing of ours left to resume.
        Ok(None) => return Ok(()),
        Err(error) => match error.raw_os_error().map(|code| code as u32) {
            // The thread exited while we held our suspend; nothing to resume.
            Some(ERROR_INVALID_PARAMETER) => return Ok(()),
            _ => {
                return Err(format!(
                    "resume pid {pid} thread {tid}: open thread: {error}"
                ))
            }
        },
    };
    // SAFETY: `thread` is a valid handle opened with THREAD_SUSPEND_RESUME.
    let previous = unsafe { ResumeThread(thread) };
    // SAFETY: the handle is ours and is not used after this close.
    unsafe {
        CloseHandle(thread);
    }
    if previous == u32::MAX {
        return Err(format!(
            "resume pid {pid} thread {tid}: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Suspends every thread of `pid` not already recorded in `set`.
///
/// One pass only: returns `Ok(true)` when at least one new tid was suspended
/// and `Ok(false)` when the enumeration held no unseen tid. A process that
/// disappeared is `Vanished`; a thread that exited between enumeration and
/// open is ignored (the design's dead-tid rule). If a later thread fails, the
/// threads this pass had just suspended are resumed again, so a failed sweep
/// never leaks a suspension.
pub fn suspend_new_threads(set: &mut SuspensionSet) -> Result<bool, SuspendError> {
    let pid = set.pid;
    let process = open_process(pid)?;
    let threads = match enumerate_threads(pid) {
        Ok(threads) => threads,
        Err(error) => {
            // SAFETY: handle from open_process, unused afterwards.
            unsafe {
                CloseHandle(process);
            }
            return Err(SuspendError::Failed(format!(
                "enumerate threads of pid {pid}: {error}"
            )));
        }
    };
    if threads.is_empty() {
        // SAFETY: handle from open_process, unused afterwards.
        unsafe {
            CloseHandle(process);
        }
        return Err(SuspendError::Vanished);
    }
    let known: HashSet<u32> = set.threads.iter().map(|thread| thread.tid).collect();
    let mut newly: Vec<u32> = Vec::new();
    let mut failure: Option<SuspendError> = None;
    for tid in threads {
        if known.contains(&tid) {
            continue;
        }
        match open_thread(pid, tid) {
            // The thread died on its own before we opened it, or its tid was
            // reused by another process.
            Ok(None) => continue,
            Ok(Some(thread)) => {
                // SAFETY: `thread` is a valid THREAD_SUSPEND_RESUME handle.
                let previous = unsafe { SuspendThread(thread) };
                if previous == u32::MAX {
                    let error = io::Error::last_os_error();
                    // SAFETY: handle ours, unused after close.
                    unsafe {
                        CloseHandle(thread);
                    }
                    failure = Some(SuspendError::Failed(format!(
                        "suspend thread {tid} of pid {pid}: {error}"
                    )));
                    break;
                }
                // SAFETY: handle ours, unused after close.
                unsafe {
                    CloseHandle(thread);
                }
                newly.push(tid);
                set.threads.push(SuspendedThread {
                    tid,
                    was_already_suspended: previous > 0,
                });
            }
            Err(error) => {
                if error.raw_os_error().map(|code| code as u32) == Some(ERROR_INVALID_PARAMETER) {
                    // The thread died on its own before we opened it.
                    continue;
                }
                failure = Some(SuspendError::Failed(format!(
                    "open thread {tid} of pid {pid}: {error}"
                )));
                break;
            }
        }
    }
    // SAFETY: handle from open_process, unused afterwards.
    unsafe {
        CloseHandle(process);
    }
    if let Some(error) = failure {
        // Remove and resume exactly the tids this pass added. A fatal
        // permission error must not leave a half-suspended process behind.
        for tid in &newly {
            set.threads.retain(|thread| thread.tid != *tid);
            let _ = resume_one_tid(pid, *tid);
        }
        return Err(error);
    }
    Ok(!newly.is_empty())
}

/// Suspends all threads of a fresh process, re-enumerating until a pass finds
/// no unseen tid (bounded by [`MAX_SUSPEND_PASSES`]).
pub fn suspend_process(pid: u32) -> Result<SuspensionSet, SuspendError> {
    let mut set = SuspensionSet::new(pid);
    if let Err(error) = suspend_process_into(&mut set) {
        // Never return a partial set as a failure: the threads earlier passes
        // suspended would have no record and nothing could resume them.
        let _ = resume_process(&set);
        return Err(error);
    }
    Ok(set)
}

/// Extends an existing set, which lets a second freeze reuse the threads the
/// first one suspended instead of incrementing their suspend count twice.
pub fn suspend_process_into(set: &mut SuspensionSet) -> Result<(), SuspendError> {
    for _ in 0..MAX_SUSPEND_PASSES {
        if !suspend_new_threads(set)? {
            return Ok(());
        }
    }
    Ok(())
}

/// Resumes exactly one increment per recorded tid. Threads the guest had
/// suspended keep their own increment; threads that already exited are
/// ignored. All failures are joined with `\n`, the Linux unfreeze contract.
pub fn resume_process(set: &SuspensionSet) -> Result<(), String> {
    let (remaining, errors) = resume_process_partial(set);
    debug_assert!(remaining.is_empty() == errors.is_empty());
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Resumes one increment per recorded tid and returns the records that could
/// not be resumed.
///
/// The caller keeps `remaining` for a retry: resuming is not repeatable for
/// threads that succeeded, so a partially failed thaw must remember exactly
/// which threads still carry our increment (`freeze.go:1317-1331`).
pub fn resume_process_partial(set: &SuspensionSet) -> (Vec<SuspendedThread>, Vec<String>) {
    let mut remaining = Vec::new();
    let mut errors = Vec::new();
    // Sorted for deterministic error ordering; suspension order is otherwise
    // irrelevant because every tid is resumed exactly once.
    let mut threads = set.threads.clone();
    threads.sort_by_key(|thread| thread.tid);
    for thread in threads {
        if let Err(error) = resume_one_tid(set.pid, thread.tid) {
            remaining.push(thread);
            errors.push(error);
        }
    }
    (remaining, errors)
}

/// Records that a process the sweep could not open is gone rather than broken.
///
/// Exposed to the resource backend so it can classify the same Win32 codes
/// without duplicating the table.
pub fn is_access_denied(error: &io::Error) -> bool {
    error.raw_os_error().map(|code| code as u32) == Some(ERROR_ACCESS_DENIED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    struct Helper {
        child: Option<Child>,
        suspended: Option<SuspensionSet>,
    }

    impl Helper {
        fn spawn() -> Self {
            let child = Command::new("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn ping helper");
            Self {
                child: Some(child),
                suspended: None,
            }
        }

        fn pid(&self) -> u32 {
            self.child.as_ref().expect("helper alive").id()
        }
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            if let Some(set) = self.suspended.take() {
                let _ = resume_process(&set);
            }
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn probe_suspend_count(tid: u32) -> u32 {
        // SAFETY: scalar arguments; the handle is checked and closed.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
        assert!(!thread.is_null(), "open thread {tid}");
        // SAFETY: valid THREAD_SUSPEND_RESUME handle.
        let previous = unsafe { SuspendThread(thread) };
        assert_ne!(previous, u32::MAX, "probe suspend");
        // SAFETY: `previous` came from our own probe, so one resume restores
        // the count exactly.
        let restored = unsafe { ResumeThread(thread) };
        assert_ne!(restored, u32::MAX, "probe resume");
        // SAFETY: handle ours, not used afterwards.
        unsafe {
            CloseHandle(thread);
        }
        previous
    }

    #[test]
    fn suspend_process_covers_every_thread_and_resume_restores_the_count() {
        let mut helper = Helper::spawn();
        let pid = helper.pid();
        // Give the helper a moment to reach its loop; the suspend path only
        // needs one live thread.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let set = suspend_process(pid).expect("suspend helper");
        assert!(!set.threads.is_empty(), "a live process has a thread");
        helper.suspended = Some(set.clone());
        let enumerated = enumerate_threads(pid).expect("re-enumerate");
        assert!(
            enumerated
                .iter()
                .all(|tid| set.threads.iter().any(|thread| thread.tid == *tid)),
            "every enumerated tid must be recorded"
        );
        let tid = set.threads[0].tid;
        // We suspended the thread once, so the probe's own suspend sees 1.
        assert_eq!(probe_suspend_count(tid), 1);
        resume_process(&set).expect("resume helper");
        helper.suspended = None;
        // Our increment is gone; a probe sees zero previous suspends.
        assert_eq!(probe_suspend_count(tid), 0);
    }

    #[test]
    fn already_suspended_threads_are_pre_frozen_and_left_suspended() {
        let helper = Helper::spawn();
        let pid = helper.pid();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let tid = enumerate_threads(pid).expect("enumerate")[0];
        // The guest suspends one thread by hand.
        // SAFETY: scalar arguments; handle checked and closed.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
        assert!(!thread.is_null());
        // SAFETY: valid handle.
        assert_eq!(unsafe { SuspendThread(thread) }, 0);
        // SAFETY: handle ours, unused afterwards.
        unsafe {
            CloseHandle(thread);
        }
        let set = suspend_process(pid).expect("suspend helper");
        assert!(
            set.threads
                .iter()
                .any(|thread| thread.tid == tid && thread.was_already_suspended),
            "the guest's own suspend must be recorded as pre-frozen"
        );
        resume_process(&set).expect("resume our increment");
        // The guest's manual suspend remains: a probe sees one previous
        // suspend, which the probe then gives back.
        assert_eq!(probe_suspend_count(tid), 1);
    }

    #[test]
    fn open_thread_rejects_a_tid_that_belongs_to_another_process() {
        let helper = Helper::spawn();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let foreign_tid = enumerate_threads(helper.pid()).expect("enumerate helper")[0];
        // The tid is live but owned by the helper, not by this process: a
        // recycled-tid resume must refuse it rather than touch that thread.
        assert!(open_thread(std::process::id(), foreign_tid)
            .expect("open foreign thread")
            .is_none());
        // The true owner still opens.
        let thread = open_thread(helper.pid(), foreign_tid)
            .expect("open own thread")
            .expect("owner matches");
        // SAFETY: handle from open_thread, unused afterwards.
        unsafe {
            CloseHandle(thread);
        }
    }

    #[test]
    fn vanished_and_access_denied_pids_are_classified() {
        // A just-exited helper cannot express "vanished": its id is reusable as
        // soon as the process object is destroyed, and the guest observed
        // ERROR_ACCESS_DENIED when the kernel had already handed the id to an
        // unrelated process. Windows allocates process ids in multiples of four,
        // so the top such value is never a real process and OpenProcess always
        // rejects it as an invalid parameter.
        const NEVER_EXISTING_PID: u32 = u32::MAX - 3;
        assert_eq!(
            suspend_process(NEVER_EXISTING_PID),
            Err(SuspendError::Vanished)
        );
        // The same table classifies a permission problem as a real failure.
        assert!(matches!(
            classify_process_error(
                NEVER_EXISTING_PID,
                io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32)
            ),
            SuspendError::Failed(_)
        ));
        assert!(is_access_denied(&io::Error::from_raw_os_error(
            ERROR_ACCESS_DENIED as i32
        )));
    }
}
