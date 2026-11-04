//! The Windows heap compactor behind `POST /collapse`.
//!
//! Windows has no transparent-hugepage API that can migrate base pages, so this
//! backend cannot reproduce Linux `MADV_COLLAPSE`. It still does real work on
//! the daemon's own process:
//!
//! 1. `SetProcessWorkingSetSizeEx(GetCurrentProcess(), usize::MAX, usize::MAX,
//!    0)` trims the working set;
//! 2. `GetProcessHeaps` + `HeapCompact` for every process heap;
//! 3. a `VirtualQuery` scan counts the daemon's committed, private,
//!    `PAGE_READWRITE` regions and the 2 MiB-aligned windows fully inside each.
//!
//! Because no window can be collapsed, `collapsed` and `alreadyHuge` are zero
//! and `skipped == chunks`; `regions`/`chunks` are real scan values. This
//! replaces the old documented "counters are reported as zero" line (see
//! `docs/windows-port.md`). Cancellation is checked between heaps and regions
//! and returns the same `context canceled` error the Linux backend does, so the
//! handler produces the same 500 envelope when a client disconnects.

use std::io;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Memory::{
    GetProcessHeaps, HeapCompact, SetProcessWorkingSetSizeEx, VirtualQuery,
    MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_PRIVATE, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::collapse::{CollapseStats, Collapser};

/// The x86_64 transparent-hugepage size the Linux backend aligns windows to.
const HUGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

/// The Windows implementation of the shared [`Collapser`] seam.
pub struct WindowsCollapser;

impl Collapser for WindowsCollapser {
    fn collapse_self(&self, cancelled: &Arc<AtomicBool>) -> io::Result<CollapseStats> {
        // SAFETY: GetCurrentProcess returns a pseudo-handle; the MAX/MAX size
        // pair asks Windows to trim the working set. Failure is best-effort.
        unsafe {
            SetProcessWorkingSetSizeEx(GetCurrentProcess(), usize::MAX, usize::MAX, 0);
        }

        for heap in process_heaps() {
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            // SAFETY: the handle comes from GetProcessHeaps and stays valid for
            // the process lifetime. A failed compact is a best-effort skip.
            unsafe {
                HeapCompact(heap, 0);
            }
        }

        let mut stats = CollapseStats::default();
        let mut address = 0usize;
        loop {
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            let mut info = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: VirtualQuery writes at most the size passed and reads the
            // address only. It returns 0 at the end of the address space.
            let written = unsafe {
                VirtualQuery(
                    address as *const core::ffi::c_void,
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if written == 0 {
                break;
            }
            let base = info.BaseAddress as usize;
            if info.State == MEM_COMMIT
                && info.Type == MEM_PRIVATE
                && info.Protect == PAGE_READWRITE
            {
                stats.regions += 1;
                stats.chunks += chunks_in_region(base, info.RegionSize) as i64;
            }
            let next = base.saturating_add(info.RegionSize);
            if next <= address {
                break;
            }
            address = next;
        }
        // No window can become a huge page without a migration API; every
        // window is un-collapsible by definition.
        stats.collapsed = 0;
        stats.already_huge = 0;
        stats.skipped = stats.chunks;
        if cancelled.load(Ordering::SeqCst) {
            return Err(io::Error::other("context canceled"));
        }
        Ok(stats)
    }
}

/// The process heaps, or none when the two-call enumeration fails (compaction
/// is best-effort and must not fail `/collapse`).
fn process_heaps() -> Vec<HANDLE> {
    // SAFETY: a null buffer with length 0 asks for the count.
    let count = unsafe { GetProcessHeaps(0, std::ptr::null_mut()) };
    if count == 0 {
        return Vec::new();
    }
    let mut heaps = vec![std::ptr::null_mut(); count as usize];
    // SAFETY: the buffer has room for `count` handles; the return is the
    // number actually written, never more than the buffer length.
    let written = unsafe { GetProcessHeaps(count, heaps.as_mut_ptr()) };
    heaps.truncate((written as usize).min(heaps.len()));
    heaps
}

/// Fully contained, 2 MiB-aligned windows inside `[base, base + size)`, the
/// same bound the Linux `collapse_range` loop uses.
fn chunks_in_region(base: usize, size: usize) -> usize {
    let end = base.saturating_add(size);
    let first = base.saturating_add(HUGE_PAGE_SIZE - 1) & !(HUGE_PAGE_SIZE - 1);
    let mut window = first;
    let mut chunks = 0;
    while window.saturating_add(HUGE_PAGE_SIZE) <= end {
        chunks += 1;
        window += HUGE_PAGE_SIZE;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_count_only_fully_contained_aligned_windows() {
        let base = 0x7f00_0000_0000usize;
        assert_eq!(chunks_in_region(base, 0), 0);
        assert_eq!(chunks_in_region(base, HUGE_PAGE_SIZE - 1), 0);
        assert_eq!(chunks_in_region(base, HUGE_PAGE_SIZE), 1);
        // An unaligned head and short tail do not create extra windows.
        assert_eq!(chunks_in_region(base + 4096, 2 * HUGE_PAGE_SIZE + 4096), 1);
        assert_eq!(chunks_in_region(base, 3 * HUGE_PAGE_SIZE), 3);
    }

    #[test]
    fn a_cancelled_collapse_reports_context_canceled() {
        let cancelled = Arc::new(AtomicBool::new(true));
        let error = WindowsCollapser.collapse_self(&cancelled).unwrap_err();
        assert_eq!(error.to_string(), "context canceled");
    }

    #[test]
    fn a_released_collapse_scans_the_process_heap() {
        let stats = WindowsCollapser
            .collapse_self(&Arc::new(AtomicBool::new(false)))
            .expect("collapse");
        assert!(stats.regions > 0, "the daemon has committed private pages");
        assert_eq!(stats.collapsed, 0);
        assert_eq!(stats.already_huge, 0);
        assert_eq!(stats.skipped, stats.chunks);
    }
}
