//! Windows watcher service: one `ReadDirectoryChangesW` session per watcher.
//!
//! A dedicated OS thread issues overlapped reads into a 64 KiB buffer and
//! waits on `{stop, io}`. Dropping the session signals the stop event,
//! cancels the pending read, drains it with `GetOverlappedResult`, joins the
//! thread and closes the directory handle. The registry, drain buffer and
//! keepalive shape are the same as the Linux registry's externally visible
//! contract (`w<pid hex><counter hex>`, sticky terminal error, `{}` for an
//! empty drain, Start then filesystem/keepalive frames).
//!
//! Declared deviations: no `EVENT_TYPE_CHMOD`; recursive watching uses one
//! subtree handle, so a directory moved in produces no synthetic child Create
//! events and rename pairing is left to the OS's OLD then NEW records; moving
//! the watched root does not end the watch; `ERROR_NOTIFY_ENUM_DIR` becomes
//! the same terminal "watcher event queue overflow".

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use connectrpc::ConnectError;
use tokio::sync::mpsc::error::TryRecvError;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, ReadDirectoryChangesW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED,
    FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SECURITY,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::exec_context::{Account, Defaults};
use crate::platform::windows::fs_runtime::{win32_code_text, FilesystemRuntime};
use crate::proto::filesystem as pb;

/// `ReadDirectoryChangesW`'s documented buffer maximum.
const BUFFER_SIZE: usize = 64 * 1024;
/// How long `open` waits for the reader thread to issue its first read. The
/// wait is synchronization, not a tolerance: a timeout means the thread never
/// started.
const STARTUP_TIMEOUT_MS: u32 = 5_000;
/// `DRIVE_REMOTE` from `winbase.h` (the constant lives behind an unrelated
/// `windows-sys` feature).
const DRIVE_REMOTE: u32 = 4;

pub struct WindowsWatcherProvider;

impl WindowsWatcherProvider {
    /// Validates the path exactly like the Linux backend and starts the
    /// dedicated reader thread.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        user: &Account,
        defaults: &Defaults,
        path: &str,
        recursive: bool,
        include_entry: bool,
        allow_network: bool,
    ) -> Result<WindowsWatcherSession, ConnectError> {
        let path = resolve_watch_path(user, defaults, path)?;
        let info = std::fs::metadata(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ConnectError::not_found(format!(
                    "path {} not found: stat {}: no such file or directory",
                    path.display(),
                    path.display()
                ))
            } else {
                failure(win32_code_text(err.raw_os_error().unwrap_or(0) as u32))
            }
        })?;
        if !info.is_dir() {
            return Err(ConnectError::invalid_argument("path is not a directory"));
        }
        if !allow_network && is_network_path(&path) {
            return Err(ConnectError::invalid_argument(
                "cannot watch path on network filesystem",
            ));
        }
        let directory = RawHandle(open_directory(&path)?);
        let stop_event = match create_event() {
            Ok(event) => RawHandle(event),
            Err(err) => {
                unsafe {
                    CloseHandle(directory.0);
                }
                return Err(failure(win32_code_text(
                    err.raw_os_error().unwrap_or(0) as u32
                )));
            }
        };
        // Manual-reset event the reader thread sets once its first overlapped
        // read is pending. `open` does not return before that happens, so no
        // event between the caller's request and the first read is lost.
        let ready_event = match create_event() {
            Ok(event) => RawHandle(event),
            Err(err) => {
                unsafe {
                    CloseHandle(directory.0);
                    CloseHandle(stop_event.0);
                }
                return Err(failure(win32_code_text(
                    err.raw_os_error().unwrap_or(0) as u32
                )));
            }
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel(BUFFER_SIZE / 1024);
        let runtime = FilesystemRuntime::new(user.clone(), defaults.clone());
        let base = path.clone();
        let thread = {
            std::thread::Builder::new()
                .name("envd-rdcw".to_owned())
                .spawn(move || {
                    watcher_thread(
                        directory,
                        stop_event,
                        ready_event,
                        base,
                        recursive,
                        include_entry,
                        runtime,
                        sender,
                    )
                })
        };
        let thread = match thread {
            Ok(thread) => thread,
            Err(err) => {
                unsafe {
                    CloseHandle(directory.0);
                    CloseHandle(stop_event.0);
                    CloseHandle(ready_event.0);
                }
                return Err(failure(err.to_string()));
            }
        };
        let wait = unsafe { WaitForSingleObject(ready_event.0, STARTUP_TIMEOUT_MS) };
        if wait != WAIT_OBJECT_0 {
            let code = unsafe { GetLastError() };
            // The thread may be blocked publishing an event; close the
            // receiver so teardown cannot deadlock on a full channel.
            receiver.close();
            let mut handle = StopHandle {
                directory,
                stop_event,
                thread: Some(thread),
            };
            handle.stop();
            unsafe {
                CloseHandle(ready_event.0);
            }
            return Err(failure(if wait == WAIT_TIMEOUT {
                "watcher thread did not start within 5 seconds".to_owned()
            } else {
                win32_code_text(code)
            }));
        }
        unsafe {
            CloseHandle(ready_event.0);
        }
        // Drain anything the thread published between issuing the read and the
        // ready signal; a terminal error here is a startup failure.
        let mut pending = VecDeque::new();
        loop {
            match receiver.try_recv() {
                Ok(Ok(event)) => pending.push_back(Ok(event)),
                Ok(Err(error)) => {
                    receiver.close();
                    let mut handle = StopHandle {
                        directory,
                        stop_event,
                        thread: Some(thread),
                    };
                    handle.stop();
                    return Err(error);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let mut handle = StopHandle {
                        directory,
                        stop_event,
                        thread: Some(thread),
                    };
                    handle.stop();
                    return Err(failure("watcher thread failed to start"));
                }
            }
        }
        Ok(WindowsWatcherSession {
            receiver,
            pending,
            _stop: StopHandle {
                directory,
                stop_event,
                thread: Some(thread),
            },
        })
    }
}

fn resolve_watch_path(
    user: &Account,
    defaults: &Defaults,
    path: &str,
) -> Result<PathBuf, ConnectError> {
    let resolved = defaults
        .resolve_path(path, user)
        .map_err(|err| ConnectError::invalid_argument(err.to_string()))?;
    Ok(PathBuf::from(resolved.replace('/', "\\")))
}

fn open_directory(path: &Path) -> Result<HANDLE, ConnectError> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(failure(win32_code_text(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32,
        )))
    } else {
        Ok(handle)
    }
}

fn create_event() -> std::io::Result<HANDLE> {
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(event)
    }
}

fn failure(message: impl std::fmt::Display) -> ConnectError {
    ConnectError::internal(format!("watcher error: {message}"))
}

#[derive(Clone, Copy)]
struct RawHandle(HANDLE);

// A Win32 HANDLE is an opaque pointer that may be used from any thread.
unsafe impl Send for RawHandle {}

impl RawHandle {
    fn is_valid(self) -> bool {
        !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE
    }
}

struct StopHandle {
    directory: RawHandle,
    stop_event: RawHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StopHandle {
    fn stop(&mut self) {
        if self.stop_event.is_valid() {
            unsafe {
                SetEvent(self.stop_event.0);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if self.directory.is_valid() {
            unsafe {
                CloseHandle(self.directory.0);
                self.directory = RawHandle(std::ptr::null_mut());
            }
        }
        if self.stop_event.is_valid() {
            unsafe {
                CloseHandle(self.stop_event.0);
                self.stop_event = RawHandle(std::ptr::null_mut());
            }
        }
    }
}

impl Drop for StopHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct WindowsWatcherSession {
    receiver: tokio::sync::mpsc::Receiver<Result<pb::FilesystemEvent, ConnectError>>,
    /// Events the reader published before `open` observed the ready signal.
    pending: VecDeque<Result<pb::FilesystemEvent, ConnectError>>,
    _stop: StopHandle,
}

impl WindowsWatcherSession {
    pub async fn next(&mut self) -> Result<pb::FilesystemEvent, ConnectError> {
        if let Some(result) = self.pending.pop_front() {
            return result;
        }
        match self.receiver.recv().await {
            Some(result) => result,
            None => Err(ConnectError::internal("watcher event channel closed")),
        }
    }
}

impl Drop for WindowsWatcherSession {
    fn drop(&mut self) {
        // Wake a sender blocked on a full channel before the stop join, so a
        // full queue cannot deadlock the session teardown.
        self.receiver.close();
    }
}

#[allow(clippy::too_many_arguments)]
fn watcher_thread(
    directory: RawHandle,
    stop: RawHandle,
    ready: RawHandle,
    base: PathBuf,
    recursive: bool,
    include_entry: bool,
    runtime: FilesystemRuntime,
    sender: tokio::sync::mpsc::Sender<Result<pb::FilesystemEvent, ConnectError>>,
) {
    let io_event = match create_event() {
        Ok(event) => event,
        Err(err) => {
            // Queue the startup error before signalling readiness: `open`
            // drains the channel once the ready event is set, and the error
            // must be visible to that drain rather than racing it.
            let _ = sender.blocking_send(Err(failure(err.to_string())));
            signal_ready(ready);
            return;
        }
    };
    // ReadDirectoryChangesW requires a DWORD-aligned destination buffer.
    // `Vec<u64>` gives 8-byte alignment, which satisfies the stronger
    // requirement, and always holds a multiple of 8 bytes.
    let mut buffer = vec![0u64; BUFFER_SIZE / 8];
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_ATTRIBUTES
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_CREATION
        | FILE_NOTIFY_CHANGE_SECURITY;
    let mut ready_signaled = false;
    let mut mark_ready = || {
        if !ready_signaled {
            signal_ready(ready);
            ready_signaled = true;
        }
    };
    loop {
        let mut overlapped = OVERLAPPED {
            hEvent: io_event,
            ..Default::default()
        };
        unsafe {
            ResetEvent(io_event);
        }
        let mut transferred = 0u32;
        let issued = unsafe {
            ReadDirectoryChangesW(
                directory.0,
                buffer.as_mut_ptr().cast(),
                BUFFER_SIZE as u32,
                recursive as i32,
                filter,
                std::ptr::null_mut(),
                &mut overlapped,
                None,
            )
        };
        if issued == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_IO_PENDING {
                let _ = sender.blocking_send(Err(notify_error(code)));
                mark_ready();
                break;
            }
        }
        // The directory handle is opened `FILE_FLAG_OVERLAPPED`, so a nonzero
        // return means the request was queued, not that it completed: the
        // documented contract is "for asynchronous calls, this indicates that
        // the operation was successfully queued", and `lpBytesReturned` is
        // undefined until the completion is delivered. The completion is
        // announced through `io_event` (or the cancel path), so both branches
        // wait and then read the count from `GetOverlappedResult`. Calling
        // `GetOverlappedResult` with `bWait = 0` before the wait only ever
        // reports ERROR_IO_INCOMPLETE.
        mark_ready();
        let handles = [stop.0, io_event];
        let wait = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
        if wait == WAIT_OBJECT_0 {
            cancel_and_drain(directory.0, &overlapped);
            break;
        }
        if wait != WAIT_OBJECT_0 + 1 {
            let code = unsafe { GetLastError() };
            // WAIT_FAILED (or an unexpected value) leaves the read pending;
            // cancel and drain it before the stack OVERLAPPED and the event
            // handle are freed.
            cancel_and_drain(directory.0, &overlapped);
            let _ = sender.blocking_send(Err(failure(win32_code_text(code))));
            break;
        }
        let completed =
            unsafe { GetOverlappedResult(directory.0, &overlapped, &mut transferred, 0) };
        if completed == 0 {
            let code = unsafe { GetLastError() };
            cancel_and_drain(directory.0, &overlapped);
            if code == ERROR_OPERATION_ABORTED {
                break;
            }
            let _ = sender.blocking_send(Err(notify_error(code)));
            break;
        }
        if transferred == 0 {
            // A successful completion with no bytes means the notify buffer
            // overflowed and records were lost; that is terminal, exactly
            // like ERROR_NOTIFY_ENUM_DIR, not something to retry.
            let _ = sender.blocking_send(Err(overflow()));
            break;
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), transferred as usize)
        };
        match events_from_buffer(bytes, &base, include_entry, &runtime) {
            Ok(events) => {
                for event in events {
                    if sender.blocking_send(Ok(event)).is_err() {
                        unsafe {
                            CloseHandle(io_event);
                        }
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = sender.blocking_send(Err(error));
                break;
            }
        }
    }
    unsafe {
        CloseHandle(io_event);
    }
}

fn signal_ready(ready: RawHandle) {
    if ready.is_valid() {
        unsafe {
            SetEvent(ready.0);
        }
    }
}

/// Cancels a possibly-pending overlapped read and waits for its completion so
/// the operation can no longer touch the caller's OVERLAPPED or the event
/// handle once this returns.
fn cancel_and_drain(directory: HANDLE, overlapped: &OVERLAPPED) {
    let mut drained = 0u32;
    unsafe {
        CancelIoEx(directory, overlapped);
        GetOverlappedResult(directory, overlapped, &mut drained, 1);
    }
}

fn events_from_buffer(
    buffer: &[u8],
    base: &Path,
    include_entry: bool,
    runtime: &FilesystemRuntime,
) -> Result<Vec<pb::FilesystemEvent>, ConnectError> {
    let mut events = Vec::new();
    for record in parse_notify_buffer(buffer)? {
        let Some((kind, carries_entry)) = notify_kind(record.action) else {
            continue;
        };
        let entry = if include_entry && carries_entry {
            let relative = record.name.replace('/', "\\");
            // A name that does not round-trip through UTF-16 (or whose entry
            // vanished) still produces the event; only the optional entry
            // payload is omitted.
            runtime.stat(&base.join(relative).to_string_lossy()).ok()
        } else {
            None
        };
        events.push(pb::FilesystemEvent {
            name: record.name,
            r#type: kind.into(),
            entry: entry.into(),
            ..Default::default()
        });
    }
    Ok(events)
}

/// A parsed `FILE_NOTIFY_INFORMATION` record with the relative name already
/// normalized to `/` (`.` for the watched root).
#[derive(Debug)]
pub(crate) struct NotifyRecord {
    pub action: u32,
    pub name: String,
}

pub(crate) fn parse_notify_buffer(buffer: &[u8]) -> Result<Vec<NotifyRecord>, ConnectError> {
    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset < buffer.len() {
        if buffer.len() - offset < 12 {
            return Err(ConnectError::internal("watcher event buffer is malformed"));
        }
        let next = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
        let action = u32::from_le_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
        let name_bytes =
            u32::from_le_bytes(buffer[offset + 8..offset + 12].try_into().unwrap()) as usize;
        // FileNameLength counts UTF-16 bytes and must be even. An odd value is
        // a malformed record, not something to skip: skipping would hide
        // corruption and desynchronize the rest of the buffer.
        if !name_bytes.is_multiple_of(2) {
            return Err(ConnectError::internal("watcher event buffer is malformed"));
        }
        let name_start = offset + 12;
        let name_end = name_start
            .checked_add(name_bytes)
            .filter(|end| *end <= buffer.len())
            .ok_or_else(|| ConnectError::internal("watcher event buffer is malformed"))?;
        let units: Vec<u16> = buffer[name_start..name_end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let raw = String::from_utf16_lossy(&units);
        let name = if raw.is_empty() {
            ".".to_owned()
        } else {
            raw.replace('\\', "/")
        };
        records.push(NotifyRecord { action, name });
        if next == 0 {
            break;
        }
        let next_offset = offset
            .checked_add(next)
            .filter(|next_offset| *next_offset > offset && *next_offset <= buffer.len())
            .ok_or_else(|| ConnectError::internal("watcher event buffer is malformed"))?;
        offset = next_offset;
    }
    Ok(records)
}

pub(crate) fn notify_kind(action: u32) -> Option<(pb::EventType, bool)> {
    match action {
        FILE_ACTION_ADDED => Some((pb::EventType::Create, true)),
        FILE_ACTION_MODIFIED => Some((pb::EventType::Write, true)),
        FILE_ACTION_REMOVED => Some((pb::EventType::Remove, false)),
        FILE_ACTION_RENAMED_OLD_NAME => Some((pb::EventType::Rename, false)),
        FILE_ACTION_RENAMED_NEW_NAME => Some((pb::EventType::Create, true)),
        _ => None,
    }
}

fn overflow() -> ConnectError {
    ConnectError::internal("watcher event queue overflow")
}

fn notify_error(code: u32) -> ConnectError {
    if code == ERROR_NOTIFY_ENUM_DIR {
        return overflow();
    }
    failure(win32_code_text(code))
}

/// Two leading separators name a UNC share. The `\\?\` and `\\.\` verbatim
/// namespaces are local device paths, not network shares.
pub(crate) fn is_network_path(path: &Path) -> bool {
    is_network_with(path, |root| {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = root
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe { GetDriveTypeW(wide.as_ptr()) }
    })
}

/// Testable classification: UNC detection plus a drive-type probe.
pub(crate) fn is_network_with(path: &Path, drive_type: impl Fn(&Path) -> u32) -> bool {
    if is_unc(path) {
        return true;
    }
    drive_type(root_of(path)) == DRIVE_REMOTE
}

fn is_unc(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    let separators = matches!(bytes, [b'\\' | b'/', b'\\' | b'/', ..]);
    separators && !text.starts_with(r"\\?\") && !text.starts_with(r"\\.\")
}

fn root_of(path: &Path) -> &Path {
    path.ancestors().last().unwrap_or(path)
}

#[derive(Clone, Default)]
pub struct WatcherRegistry {
    inner: Arc<Mutex<HashMap<String, Persistent>>>,
}

struct Buffer {
    events: Vec<pb::FilesystemEvent>,
    error: Option<ConnectError>,
}

struct Persistent {
    buffer: Arc<Mutex<Buffer>>,
    task: tokio::task::JoinHandle<()>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Persistent {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        self.task.abort();
    }
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_persistent(
        &self,
        user: &Account,
        defaults: &Defaults,
        path: &str,
        recursive: bool,
        include_entry: bool,
        allow_network: bool,
        id: String,
        pending: Vec<pb::FilesystemEvent>,
    ) -> Result<(), ConnectError> {
        let mut session = WindowsWatcherProvider::open(
            user,
            defaults,
            path,
            recursive,
            include_entry,
            allow_network,
        )?;
        let buffer = Arc::new(Mutex::new(Buffer {
            events: pending,
            error: None,
        }));
        let sink = buffer.clone();
        let (cancel, mut canceled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut canceled => break,
                    event = session.next() => {
                        let Ok(mut state) = sink.lock() else { break; };
                        match event {
                            Ok(event) => state.events.push(event),
                            Err(error) => {
                                state.error = Some(error);
                                break;
                            }
                        }
                    }
                }
            }
        });
        self.inner.lock().map_err(|_| poisoned())?.insert(
            id,
            Persistent {
                buffer,
                task,
                cancel: Some(cancel),
            },
        );
        Ok(())
    }

    pub fn watch_dir(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::WatchDirRequest,
        keepalive: Option<&str>,
    ) -> Result<WatchSubscription, ConnectError> {
        let mut session = WindowsWatcherProvider::open(
            user,
            defaults,
            &request.path,
            request.recursive,
            request.include_entry,
            request.allow_network_mounts,
        )?;
        let interval = keepalive_interval(keepalive);
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let signal = closed.clone();
        let task = tokio::spawn(async move {
            loop {
                let event = session.next().await;
                let terminal = event.is_err();
                if sender.send(event).await.is_err() || terminal {
                    break;
                }
            }
            signal.store(true, Ordering::SeqCst);
        });
        Ok(WatchSubscription {
            receiver,
            task,
            first: true,
            interval,
            timer: Box::pin(tokio::time::sleep(interval)),
            closed,
            ended: false,
        })
    }
}

impl crate::platform::WatcherService for WatcherRegistry {
    type Subscription = WatchSubscription;

    fn create_watcher(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::CreateWatcherRequest,
    ) -> Result<pb::CreateWatcherResponse, ConnectError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = format!(
            "w{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        self.spawn_persistent(
            user,
            defaults,
            &request.path,
            request.recursive,
            request.include_entry,
            request.allow_network_mounts,
            id.clone(),
            Vec::new(),
        )?;
        Ok(pb::CreateWatcherResponse {
            watcher_id: id,
            ..Default::default()
        })
    }

    fn get_watcher_events(&self, id: &str) -> Result<pb::GetWatcherEventsResponse, ConnectError> {
        let registry = self.inner.lock().map_err(|_| poisoned())?;
        let watcher = registry.get(id).ok_or_else(|| missing(id))?;
        let mut buffer = watcher.buffer.lock().map_err(|_| poisoned())?;
        if let Some(error) = &buffer.error {
            return Err(error.clone());
        }
        Ok(pb::GetWatcherEventsResponse {
            events: std::mem::take(&mut buffer.events),
            ..Default::default()
        })
    }

    fn remove_watcher(&self, id: &str) -> Result<pb::RemoveWatcherResponse, ConnectError> {
        self.inner
            .lock()
            .map_err(|_| poisoned())?
            .remove(id)
            .ok_or_else(|| missing(id))?;
        Ok(pb::RemoveWatcherResponse::default())
    }

    fn watch_dir(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::WatchDirRequest,
        keepalive: Option<&str>,
    ) -> Result<Self::Subscription, ConnectError> {
        WatcherRegistry::watch_dir(self, user, defaults, request, keepalive)
    }
}

fn missing(id: &str) -> ConnectError {
    ConnectError::not_found(format!("watcher with id {id} not found"))
}

fn poisoned() -> ConnectError {
    ConnectError::internal("watcher state lock poisoned")
}

pub struct WatchSubscription {
    receiver: tokio::sync::mpsc::Receiver<Result<pb::FilesystemEvent, ConnectError>>,
    task: tokio::task::JoinHandle<()>,
    first: bool,
    interval: Duration,
    timer: std::pin::Pin<Box<tokio::time::Sleep>>,
    closed: Arc<AtomicBool>,
    ended: bool,
}

impl Drop for WatchSubscription {
    fn drop(&mut self) {
        self.task.abort();
        self.closed.store(true, Ordering::SeqCst);
    }
}

impl WatchSubscription {
    pub fn closed_signal(&self) -> Arc<AtomicBool> {
        self.closed.clone()
    }

    pub async fn recv(&mut self) -> Option<Result<pb::WatchDirResponse, ConnectError>> {
        use pb::watch_dir_response::Event;
        if self.ended {
            return None;
        }
        if self.first {
            self.first = false;
            return Some(Ok(pb::WatchDirResponse {
                event: Some(Event::Start(Box::default())),
                ..Default::default()
            }));
        }
        let event = tokio::select! {
            event = self.receiver.recv() => match event {
                Some(Ok(event)) => {
                    self.timer.as_mut().reset(tokio::time::Instant::now() + self.interval);
                    Event::Filesystem(Box::new(event))
                }
                Some(Err(error)) => {
                    self.ended = true;
                    return Some(Err(error));
                }
                None => {
                    self.ended = true;
                    return Some(Err(ConnectError::internal("watcher event channel closed")));
                }
            },
            _ = &mut self.timer => {
                self.timer.as_mut().reset(tokio::time::Instant::now() + self.interval);
                Event::Keepalive(Box::default())
            }
        };
        Some(Ok(pb::WatchDirResponse {
            event: Some(event),
            ..Default::default()
        }))
    }
}

impl crate::platform::WatchSubscription for WatchSubscription {
    fn next_frame(&mut self) -> crate::platform::WatchFrameFuture<'_> {
        Box::pin(self.recv())
    }
}

pub fn keepalive_interval(header: Option<&str>) -> Duration {
    header
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|seconds| *seconds > 0 && *seconds <= i64::MAX / 1_000_000_000)
        .map(|seconds| Duration::from_secs(seconds as u64))
        .unwrap_or(Duration::from_secs(90))
}

#[cfg(all(test, windows))]
mod tests;
