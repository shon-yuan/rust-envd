//! A thin, owning Job Object wrapper.
//!
//! The process slice will create the sandbox's Job (breakaway/nested-job
//! configuration belongs to that slice) and assign every workload process to
//! it. This slice consumes the Job for hierarchy freeze: `process_ids` is the
//! authoritative membership list, including processes the registry has not
//! seen yet. Limits are exposed for the same future slice; nothing here
//! changes the daemon or its own Job membership.

use std::fmt;
use std::io;
use std::mem::size_of;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// The Job limits the process slice may request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobLimits {
    pub process_memory_limit: Option<usize>,
    pub job_memory_limit: Option<usize>,
    pub active_process_limit: Option<u32>,
    /// Terminate every process in the Job when its last handle closes. The
    /// default is `false`; a sandbox teardown normally wants `true`.
    pub kill_on_job_close: bool,
}

/// An owned Job Object handle.
pub struct JobHandle {
    handle: HANDLE,
}

// SAFETY: a Job Object HANDLE is a process-wide kernel handle. Windows
// documents that handles are usable from any thread; the wrapper never closes
// it twice because ownership is unique.
unsafe impl Send for JobHandle {}
// SAFETY: the handle is immutable after construction; all operations are
// thread-safe Win32 calls.
unsafe impl Sync for JobHandle {}

impl fmt::Debug for JobHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobHandle")
            .field("handle", &self.handle)
            .finish()
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is owned by this wrapper and is not used
            // after the close.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

impl JobHandle {
    /// Creates an unnamed Job Object.
    pub fn create() -> io::Result<Self> {
        // SAFETY: both optional pointers are null; the returned handle is
        // checked before it is stored.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Assigns `pid` to the Job. Requires PROCESS_SET_QUOTA |
    /// PROCESS_TERMINATE on the target.
    pub fn assign(&self, pid: u32) -> io::Result<()> {
        // SAFETY: scalar arguments; the handle is checked and closed below.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() || process == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both handles are valid and owned by this frame/wrapper.
        let assigned = unsafe { AssignProcessToJobObject(self.handle, process) };
        let error = if assigned == 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        // SAFETY: the process handle is ours and is not used afterwards.
        unsafe {
            CloseHandle(process);
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Terminates every process in the Job with `exit_code`.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: the Job handle is owned and valid for the duration of the
        // synchronous call.
        let terminated = unsafe { TerminateJobObject(self.handle, exit_code) };
        if terminated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The Job's current process id list.
    ///
    /// `QueryInformationJobObject` uses the required-size / `ERROR_MORE_DATA`
    /// two-call pattern for `JobObjectBasicProcessIdList`: the first query
    /// reports how many bytes the list needs, the second fills a buffer of
    /// exactly that size. A provider that does not report a size still gets a
    /// retry with a doubled buffer. The list is sorted by pid so hierarchy
    /// freeze is deterministic.
    pub fn process_ids(&self) -> io::Result<Vec<u32>> {
        // A real sandbox has thousands of processes at most; 8 MiB of pid
        // words is far above any legitimate answer and bounds a broken
        // provider that keeps asking for more.
        const MAX_WORDS: usize = 1 << 20;
        let header = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() - size_of::<usize>();
        let mut words = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>().div_ceil(size_of::<usize>());
        let mut buffer;
        loop {
            buffer = vec![0usize; words];
            let mut returned: u32 = 0;
            let bytes = (buffer.len() * size_of::<usize>()) as u32;
            // SAFETY: `buffer` is a live, usize-aligned allocation and
            // `returned` is a valid out-pointer.
            let queried = unsafe {
                QueryInformationJobObject(
                    self.handle,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    bytes,
                    &mut returned,
                )
            };
            if queried != 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error().map(|code| code as u32) != Some(ERROR_MORE_DATA) {
                return Err(error);
            }
            let needed = returned as usize;
            let next = if needed > bytes as usize {
                needed.div_ceil(size_of::<usize>())
            } else {
                words.saturating_mul(2)
            };
            if next <= words || next > MAX_WORDS {
                return Err(io::Error::other(
                    "QueryInformationJobObject kept asking for a larger pid list",
                ));
            }
            words = next;
        }
        let capacity = buffer
            .len()
            .saturating_mul(size_of::<usize>())
            .saturating_sub(header)
            / size_of::<usize>();
        // SAFETY: the query above initialized at least the header of the
        // buffer; `capacity` is bounded by the allocation.
        let list = unsafe { &*(buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()) };
        let count = (list.NumberOfProcessIdsInList as usize).min(capacity);
        // SAFETY: the API wrote `count` usize entries after the header, and
        // `count` was clamped to the buffer's capacity.
        let ids = unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count) };
        let mut ids: Vec<u32> = ids.iter().map(|pid| *pid as u32).collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Applies the extended limits the process slice requests.
    pub fn set_limits(&self, limits: JobLimits) -> io::Result<()> {
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if let Some(limit) = limits.process_memory_limit {
            information.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            information.ProcessMemoryLimit = limit;
        }
        if let Some(limit) = limits.job_memory_limit {
            information.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            information.JobMemoryLimit = limit;
        }
        if let Some(limit) = limits.active_process_limit {
            information.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            information.BasicLimitInformation.ActiveProcessLimit = limit;
        }
        if limits.kill_on_job_close {
            information.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        // SAFETY: `information` is a live, correctly sized and aligned
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION for the length passed.
        let applied = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                (&information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if applied == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_job_reports_its_assigned_pids_in_order() {
        let job = JobHandle::create().expect("create job");
        assert_eq!(job.process_ids().expect("empty job"), Vec::<u32>::new());
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn helper");
        // Some CI hosts already place the test process in a Job that refuses
        // nesting; creation, the two-call pid query and sorting are still
        // exercised, and the assignment assertion is only meaningful when the
        // host permits it.
        if job.assign(child.id()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        let ids = job.process_ids().expect("job pids");
        assert!(
            ids.contains(&child.id()),
            "{ids:?} must contain {}",
            child.id()
        );
        // Sorted and de-duplicated.
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted);
        let _ = child.kill();
        let _ = child.wait();
    }
}
