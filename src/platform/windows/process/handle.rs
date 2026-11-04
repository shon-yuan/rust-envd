//! An owning Win32 handle.
//!
//! Every handle the process runtime owns (process, thread, pipe ends, console
//! input/output) goes through this RAII wrapper so a failure path cannot leak
//! a kernel object. `HANDLE` is an opaque pointer, so the wrapper carries the
//! explicit `Send`/`Sync` justification Windows documents for kernel handles.

use std::io;

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};

pub(crate) struct OwnedHandle {
    handle: HANDLE,
}

// SAFETY: a Win32 kernel handle is an opaque pointer value that the operating
// system documents as usable from any thread of the owning process. This
// wrapper is the unique owner, so the handle is closed exactly once.
unsafe impl Send for OwnedHandle {}
// SAFETY: see the `Send` justification; all operations performed through the
// handle (WaitForSingleObject, TerminateProcess, ReadFile, WriteFile,
// CancelSynchronousIo, CloseHandle) are thread-safe Win32 calls.
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    /// Adopts a handle returned by a Win32 call, rejecting the null and
    /// `INVALID_HANDLE_VALUE` sentinels.
    pub(crate) fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// The raw handle for a Win32 call. The wrapper keeps ownership; callers
    /// must not close the returned value.
    pub(crate) fn raw(&self) -> HANDLE {
        self.handle
    }

    /// A second owning handle to the same kernel object, used when a blocking
    /// worker or a grace timer must outlive the call that started it.
    pub(crate) fn duplicate(&self) -> io::Result<Self> {
        let mut target: HANDLE = std::ptr::null_mut();
        // SAFETY: both process pseudo-handles are valid for the calling
        // process, `target` is a valid out-pointer, and the source handle is
        // owned by this wrapper for the duration of the call.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle,
                GetCurrentProcess(),
                &mut target,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        Self::new(target)
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: this wrapper is the unique owner and the handle is not
            // used after the close.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

/// Duplicates the calling thread's handle for a later `CancelSynchronousIo`.
/// The caller must invoke this from the thread that will block in `ReadFile`.
pub(crate) fn duplicate_current_thread() -> io::Result<OwnedHandle> {
    let mut target: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentThread` returns a pseudo-handle valid to duplicate
    // from the calling thread; `target` is a valid out-pointer.
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut target,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 {
        return Err(io::Error::last_os_error());
    }
    OwnedHandle::new(target)
}
