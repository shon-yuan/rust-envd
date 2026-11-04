//! Service Control Manager lifecycle for the Windows daemon.
//!
//! The daemon serves as `coorworker-envd`. `dispatch` hands the SCM a service
//! table; `ServiceMain` reports START_PENDING → RUNNING, runs the async body on
//! the already-built runtime, and reports STOPPED once the body returns. STOP
//! and SHUTDOWN flip the serve shutdown watch (the handler cannot block the
//! SCM, so it reports STOP_PENDING and returns).
//!
//! Console and development modes never reach this module; they run the same
//! body with a pending shutdown future, matching Linux's no-handler default.

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::watch;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, NO_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};

/// The installed service name. The SCM table registers exactly this name.
pub const SERVICE_NAME: &str = "coorworker-envd";

/// The wait hint for pending transitions (SCM default guidance: generous).
pub const WAIT_HINT_MS: u32 = 30_000;

/// Where status transitions go. The real implementation writes to the SCM;
/// tests record them.
pub trait ServiceStatusReporter: Send + Sync {
    fn report(&self, state: u32, checkpoint: u32, wait_hint_ms: u32, controls: u32);
}

/// The SCM-backed reporter. `SetServiceStatus` is valid from any thread of
/// the service process.
pub struct ScmReporter {
    /// Stored as a bare address so the reporter is `Send + Sync`; the handle
    /// itself is an opaque SCM value used only by `SetServiceStatus`.
    handle: usize,
}

impl ServiceStatusReporter for ScmReporter {
    fn report(&self, state: u32, checkpoint: u32, wait_hint_ms: u32, controls: u32) {
        if self.handle == 0 {
            return;
        }
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: controls,
            dwWin32ExitCode: ERROR_SUCCESS,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: checkpoint,
            dwWaitHint: wait_hint_ms,
        };
        // SAFETY: the handle came from RegisterServiceCtrlHandlerExW and
        // stays valid until the service stops.
        unsafe {
            SetServiceStatus(self.handle as SERVICE_STATUS_HANDLE, &status);
        }
    }
}

/// The process-wide service state. A global is required because the SCM
/// callbacks are bare `extern "system"` functions with no captured context.
struct ServiceControl {
    shutdown: watch::Sender<bool>,
    reporter: Mutex<Option<Arc<dyn ServiceStatusReporter>>>,
    current: AtomicU32,
    checkpoint: AtomicU32,
}

impl ServiceControl {
    fn new() -> Self {
        Self {
            shutdown: watch::channel(false).0,
            reporter: Mutex::new(None),
            current: AtomicU32::new(SERVICE_START_PENDING),
            checkpoint: AtomicU32::new(0),
        }
    }

    fn install_reporter(&self, reporter: Arc<dyn ServiceStatusReporter>) {
        if let Ok(mut slot) = self.reporter.lock() {
            *slot = Some(reporter);
        }
    }

    fn report(&self, state: u32, wait_hint_ms: u32) {
        let checkpoint = if state == SERVICE_RUNNING || state == SERVICE_STOPPED {
            0
        } else {
            self.checkpoint.fetch_add(1, Ordering::SeqCst) + 1
        };
        self.current.store(state, Ordering::SeqCst);
        let controls = match state {
            SERVICE_RUNNING => SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
            SERVICE_STOP_PENDING => SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
            _ => 0,
        };
        if let Ok(slot) = self.reporter.lock() {
            if let Some(reporter) = slot.as_ref() {
                reporter.report(state, checkpoint, wait_hint_ms, controls);
            }
        }
    }

    fn report_current(&self) {
        let state = self.current.load(Ordering::SeqCst);
        let checkpoint = self.checkpoint.load(Ordering::SeqCst);
        let controls = if state == SERVICE_RUNNING || state == SERVICE_STOP_PENDING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        };
        if let Ok(slot) = self.reporter.lock() {
            if let Some(reporter) = slot.as_ref() {
                reporter.report(state, checkpoint, 0, controls);
            }
        }
    }

    fn request_stop(&self) {
        // `send` fails when no receiver exists yet; the published value must
        // change regardless, because `ServiceHandle::is_stopping` reads it and
        // the body subscribes later than the first control can arrive.
        self.shutdown.send_replace(true);
    }
}

/// The handle the async body receives: shutdown notification plus the final
/// STOPPED report.
#[derive(Clone)]
pub struct ServiceHandle {
    control: Arc<ServiceControl>,
}

impl ServiceHandle {
    /// A receiver that flips to `true` on STOP or SHUTDOWN.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.control.shutdown.subscribe()
    }

    pub fn is_stopping(&self) -> bool {
        *self.control.shutdown.borrow()
    }

    /// Reports SERVICE_STOPPED before a process replacement exits.
    pub fn report_stopped(&self) {
        self.control.report(SERVICE_STOPPED, 0);
    }
}

static SERVICE_CONTROL: OnceLock<Arc<ServiceControl>> = OnceLock::new();
/// The async body `ServiceMain` runs. It must be a boxed closure because the
/// SCM callback cannot capture state, and a boxed future because the runtime
/// is built inside `ServiceMain` (a console fallback builds its own).
pub type Body = Box<dyn FnOnce(ServiceHandle) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;
static SERVICE_BODY: Mutex<Option<Body>> = Mutex::new(None);

/// The service state for the process's upgrade run, or `None` in console
/// mode. `report_stopped_for_exit` is the only other caller.
pub fn current() -> Option<ServiceHandle> {
    SERVICE_CONTROL.get().map(|control| ServiceHandle {
        control: control.clone(),
    })
}

/// Reports SERVICE_STOPPED if this process is an SCM service. Called just
/// before `ExitProcess` on a successful upgrade, so the SCM does not wait for
/// the successor.
pub fn report_stopped_for_exit() {
    if let Some(control) = SERVICE_CONTROL.get() {
        control.report(SERVICE_STOPPED, 0);
    }
}

/// Registers the service table and blocks in the SCM dispatcher.
///
/// Returns the Win32 error code when the dispatcher cannot start the service
/// table; `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` means the process was not
/// started by the SCM (the caller then falls back to console mode and builds
/// its own runtime). The async body runs on a multi-thread runtime built here,
/// so a console fallback never shares or leaks one.
pub fn dispatch(body: Body) -> Result<(), u32> {
    let control = Arc::new(ServiceControl::new());
    let _ = SERVICE_CONTROL.set(control);
    if let Ok(mut slot) = SERVICE_BODY.lock() {
        *slot = Some(body);
    }
    let name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_ptr() as *mut u16,
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: `table` is null-terminated and lives until the dispatcher
    // returns; the call blocks for the service's lifetime.
    let started = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if started == 0 {
        let error = unsafe { GetLastError() };
        // The SCM never ran the body; drop the closure so its captures (the
        // parsed command line) do not outlive the failed dispatch.
        if let Ok(mut slot) = SERVICE_BODY.lock() {
            let _ = slot.take();
        }
        Err(error)
    } else {
        Ok(())
    }
}

unsafe extern "system" fn handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    let Some(state) = SERVICE_CONTROL.get() else {
        return NO_ERROR;
    };
    handle_control(control, state);
    NO_ERROR
}

fn handle_control(control: u32, state: &ServiceControl) {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            // Report before flipping the watch: the body may finish and the
            // SCM must already see STOP_PENDING.
            state.report(SERVICE_STOP_PENDING, WAIT_HINT_MS);
            state.request_stop();
        }
        SERVICE_CONTROL_INTERROGATE => state.report_current(),
        _ => {}
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = wide(SERVICE_NAME);
    // SAFETY: the name is null-terminated and lives for the call.
    let handle =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), std::ptr::null()) };
    let Some(control) = SERVICE_CONTROL.get().cloned() else {
        return;
    };
    if handle.is_null() {
        control.request_stop();
        return;
    }
    control.install_reporter(Arc::new(ScmReporter {
        handle: handle as usize,
    }));
    control.report(SERVICE_START_PENDING, WAIT_HINT_MS);
    control.report(SERVICE_RUNNING, 0);
    let body = SERVICE_BODY.lock().ok().and_then(|mut slot| slot.take());
    if let Some(body) = body {
        let handle = ServiceHandle {
            control: control.clone(),
        };
        match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            // `block_on` runs on this SCM thread, so the STOPPED report below
            // only happens after the serve future has fully drained.
            Ok(runtime) => runtime.block_on(body(handle)),
            Err(error) => eprintln!("cannot start the service runtime: {error}"),
        }
    }
    control.report(SERVICE_STOPPED, 0);
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Points the standard handles a console-less process lacks at the NUL
/// device. Rust's `println!`/`eprintln!` panic when the write fails, so every
/// path that can run without a console (an SCM service AND the successor it
/// spawns, whose handles are not inherited) must call this before any
/// diagnostic. A process with valid handles is left untouched.
pub(crate) fn ensure_std_handles() {
    let missing = |slot| {
        // SAFETY: the slot constant is a valid standard-handle kind.
        let handle = unsafe { GetStdHandle(slot) };
        handle.is_null() || handle == INVALID_HANDLE_VALUE
    };
    if !(missing(STD_OUTPUT_HANDLE) || missing(STD_ERROR_HANDLE)) {
        return;
    }
    redirect_std_handles_to_nul();
}

/// Replaces the standard handles with the NUL device. The handle is kept for
/// the process lifetime.
fn redirect_std_handles_to_nul() {
    let name = wide("NUL");
    // SAFETY: the name is null-terminated and lives for the call; no security
    // attributes and no template handle are used.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return;
    }
    // SAFETY: `handle` is a live file handle; the standard-handle slots are
    // process-wide and this is the daemon's only writer.
    unsafe {
        SetStdHandle(STD_INPUT_HANDLE, handle);
        SetStdHandle(STD_OUTPUT_HANDLE, handle);
        SetStdHandle(STD_ERROR_HANDLE, handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingReporter {
        events: Mutex<Vec<(u32, u32, u32, u32)>>,
    }

    impl ServiceStatusReporter for RecordingReporter {
        fn report(&self, state: u32, checkpoint: u32, wait_hint_ms: u32, controls: u32) {
            self.events
                .lock()
                .unwrap()
                .push((state, checkpoint, wait_hint_ms, controls));
        }
    }

    #[test]
    fn service_status_reports_running_then_stopped() {
        let recording = Arc::new(RecordingReporter::default());
        let control = Arc::new(ServiceControl::new());
        control.install_reporter(recording.clone());
        control.report(SERVICE_START_PENDING, WAIT_HINT_MS);
        control.report(SERVICE_RUNNING, 0);
        handle_control(SERVICE_CONTROL_STOP, &control);
        control.report(SERVICE_STOPPED, 0);
        let events = recording.events.lock().unwrap().clone();
        let states: Vec<u32> = events.iter().map(|event| event.0).collect();
        assert_eq!(
            states,
            vec![
                SERVICE_START_PENDING,
                SERVICE_RUNNING,
                SERVICE_STOP_PENDING,
                SERVICE_STOPPED
            ]
        );
        let pending: Vec<&(u32, u32, u32, u32)> = events
            .iter()
            .filter(|event| event.0 == SERVICE_START_PENDING || event.0 == SERVICE_STOP_PENDING)
            .collect();
        assert!(pending.iter().all(|event| event.2 == WAIT_HINT_MS));
        let checkpoints: Vec<u32> = events.iter().map(|event| event.1).collect();
        assert_eq!(checkpoints, vec![1, 0, 2, 0], "first probe is checkpoint 1");
        assert_eq!(events[1].3, SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN);
    }

    #[test]
    fn stop_control_sets_the_shutdown_signal() {
        let control = Arc::new(ServiceControl::new());
        let handle = ServiceHandle {
            control: control.clone(),
        };
        assert!(!handle.is_stopping());
        handle_control(SERVICE_CONTROL_INTERROGATE, &control);
        assert!(
            !handle.is_stopping(),
            "interrogate must not stop the daemon"
        );
        handle_control(SERVICE_CONTROL_STOP, &control);
        assert!(handle.is_stopping());

        let control = Arc::new(ServiceControl::new());
        let handle = ServiceHandle {
            control: control.clone(),
        };
        handle_control(SERVICE_CONTROL_SHUTDOWN, &control);
        assert!(handle.is_stopping());
    }
}
