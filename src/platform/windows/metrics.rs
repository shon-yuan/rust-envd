//! The Windows metrics collector: the shared [`crate::metrics::Metrics`] wire
//! shape filled from real Win32 counters instead of `/proc`.
//!
//! The CPU delta arithmetic ([`crate::metrics::CpuTimes::percent`]), the
//! two-decimal rounding and the disk usage helper are the exact code the Linux
//! collector uses, so the values are computed one way for both platforms:
//!
//! - `cpu_used_pct` from `GetSystemTimes` (kernel time includes idle; busy is
//!   `(kernel - idle) + user`);
//! - `cpu_count` from `GetActiveProcessorCount`, with `GetSystemInfo` as the
//!   fallback;
//! - `mem_total`/`mem_used` from `GlobalMemoryStatusEx` and `mem_cache` from
//!   `GetPerformanceInfo().SystemCache * PageSize` (the closest analog of the
//!   Linux `Cached + SReclaimable`);
//! - `disk_*` from `GetDiskFreeSpaceExW` on the system volume.
//!
//! The four CPU hotplug fields stay zero: Windows has no CPU manager, which is
//! exactly the Linux `CpuManager::noop` `State::default()` fallback. OOM kills
//! are omitted (`oom_kills` absent) because this slice has no source for them,
//! which the OpenAPI explicitly permits. A Win32 failure is returned as an
//! [`std::io::Error`] so the shared handler emits the same empty 500.

use std::io;
use std::mem::size_of;
use std::time::SystemTime;

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows_sys::Win32::System::Threading::{
    GetActiveProcessorCount, GetSystemTimes, ALL_PROCESSOR_GROUPS,
};

use crate::metrics::{disk_usage, CpuStatus, CpuTimes, Metrics};

use super::fsfreeze::{system_volume_root, wide};

/// Samples the daemon host with the shared Linux formulas.
pub struct WindowsCollector {
    previous: CpuTimes,
    /// The system-volume root (`C:\`) `GetDiskFreeSpaceExW` is issued against.
    volume: String,
}

impl WindowsCollector {
    /// Captures the first `GetSystemTimes` baseline, like the Linux collector's
    /// `/proc/stat` read at construction.
    pub fn new() -> io::Result<Self> {
        let previous = read_cpu_times()?;
        let volume = system_volume_root()?;
        Ok(Self { previous, volume })
    }

    /// Test seam: a real collector whose disk sample targets an unresolvable
    /// volume, so the shared empty-500 path can be asserted on a host where
    /// the real system volume always answers.
    #[cfg(test)]
    pub(crate) fn with_volume_for_tests(volume: &str) -> io::Result<Self> {
        let previous = read_cpu_times()?;
        Ok(Self {
            previous,
            volume: volume.to_owned(),
        })
    }

    /// Follows the Linux collection order: memory, cpu count, the CPU delta,
    /// then disk. A later disk failure therefore still consumes this CPU
    /// interval, and a later request measures the next one.
    pub fn sample(&mut self, status: CpuStatus) -> io::Result<Metrics> {
        let memory = read_memory()?;
        let cpu_count = logical_cpu_count();
        let current = read_cpu_times()?;
        let percent = self.previous.percent(current);
        self.previous = current;
        let (disk_total, disk_used) = read_disk(&self.volume)?;
        let seconds = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        Ok(Metrics {
            ts: seconds as i64,
            cpu_count,
            cpu_used_pct: if percent > 0.0 {
                ((percent * 100.0).round() / 100.0) as f32
            } else {
                percent as f32
            },
            cpu_possible: status.possible,
            cpu_target: status.target,
            cpu_target_attempts: status.attempts,
            cpu_write_pending_ms: status.write_pending_ms,
            mem_total: memory.total,
            mem_used: memory.total.wrapping_sub(memory.available),
            mem_cache: memory.cached,
            disk_used,
            disk_total,
        })
    }
}

fn filetime_units(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

fn read_cpu_times() -> io::Result<CpuTimes> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: three out-pointers to initialized FILETIME values.
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let idle = filetime_units(idle);
    let kernel = filetime_units(kernel);
    let user = filetime_units(user);
    // GetSystemTimes' kernel time includes idle time, so the non-idle kernel
    // share is kernel - idle; total == kernel + user.
    Ok(CpuTimes {
        total: kernel.wrapping_add(user) as f64,
        busy: kernel.saturating_sub(idle).wrapping_add(user) as f64,
    })
}

fn logical_cpu_count() -> u32 {
    // SAFETY: scalar argument and no out-parameters.
    let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    if count > 0 {
        return count;
    }
    let mut info = SYSTEM_INFO::default();
    // SAFETY: GetSystemInfo fills the caller-provided struct.
    unsafe { GetSystemInfo(&mut info) };
    info.dwNumberOfProcessors
}

struct Memory {
    total: u64,
    available: u64,
    cached: u64,
}

fn read_memory() -> io::Result<Memory> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: dwLength is set to this struct's size and the pointer is valid.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Memory {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
        cached: system_cache_bytes(),
    })
}

/// `GetPerformanceInfo`'s system cache in bytes; zero when the call fails, the
/// same tolerant behavior the Linux collector has for a missing optional
/// meminfo field.
fn system_cache_bytes() -> u64 {
    let mut info = PERFORMANCE_INFORMATION {
        cb: size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    // SAFETY: cb is set to this struct's size and the pointer is valid.
    if unsafe { GetPerformanceInfo(&mut info, size_of::<PERFORMANCE_INFORMATION>() as u32) } == 0 {
        return 0;
    }
    (info.SystemCache as u64).wrapping_mul(info.PageSize as u64)
}

fn read_disk(volume: &str) -> io::Result<(u64, u64)> {
    let volume = wide(volume);
    let mut available_to_caller = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    // SAFETY: the path is NUL-terminated and the three out-pointers are valid.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            volume.as_ptr(),
            &mut available_to_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // The shared helper subtracts the caller-available bytes from the total;
    // `total_free` is deliberately unused, matching Go statfs' Bavail.
    let _ = total_free;
    Ok(disk_usage(total, available_to_caller, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cpu_sample_matches_the_shared_formula() {
        let previous = CpuTimes {
            total: 100.0,
            busy: 50.0,
        };
        // A busy regression is zero, not negative.
        assert_eq!(
            previous.percent(CpuTimes {
                total: 50.0,
                busy: 10.0
            }),
            0.0
        );
        // A total-counter reversal with growing busy is 100, not a division
        // by a non-positive interval.
        assert_eq!(
            previous.percent(CpuTimes {
                total: 50.0,
                busy: 90.0
            }),
            100.0
        );
        // 1/6 of the interval rounds to the same two decimals as Linux.
        let percent = CpuTimes {
            total: 0.0,
            busy: 0.0,
        }
        .percent(CpuTimes {
            total: 6.0,
            busy: 1.0,
        });
        assert_eq!(((percent * 100.0).round() / 100.0) as f32, 16.67);
    }

    #[test]
    fn a_real_sample_reports_the_twelve_shared_fields() {
        let mut collector = WindowsCollector::new().expect("collector");
        let metrics = collector.sample(CpuStatus::default()).expect("sample");
        assert!(metrics.cpu_count > 0);
        assert!((0.0..=100.0).contains(&metrics.cpu_used_pct));
        assert!(metrics.mem_total > 0);
        assert!(metrics.mem_used <= metrics.mem_total);
        assert!(metrics.disk_total > 0);
        assert!(metrics.disk_used <= metrics.disk_total);
        assert_eq!(metrics.cpu_possible, 0);
        assert_eq!(metrics.cpu_target, 0);
        assert_eq!(metrics.cpu_target_attempts, 0);
        assert_eq!(metrics.cpu_write_pending_ms, 0);
    }
}
