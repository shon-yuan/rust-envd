//! Windows live-upgrade handover: the sidecar, the successor spawn and the
//! inherited-listener adoption.
//!
//! The pinned `upgrade.HandoverState` protobuf stays byte-identical to Linux
//! (and the Linux schema gate applies unchanged). Windows-only data travels in
//! a versioned sidecar written next to the blob: the
//! `WSAPROTOCOL_INFOW` that reconstructs the listening socket in the
//! successor, the readiness-pipe handle and the per-process handle table.
//!
//! Declared deviations (docs/windows-port.md):
//! - the listener is duplicated into the successor instead of rebinding, so
//!   the successor PID differs from the parent's;
//! - a two-phase readiness handshake ('S' = listener reconstructed, 'R' =
//!   resume complete and serving) replaces Linux's atomic execve;
//! - live processes and watchers are NOT carried in this slice: the outgoing
//!   process's kill-on-close per-process Jobs end every child at exit, and the
//!   successor starts with an empty process and watcher registry. The sidecar
//!   format reserves the per-process records so a later slice can fill them
//!   without changing the wire version.

use std::ffi::{c_void, OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{FromRawSocket, RawSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::{
    WSADuplicateSocketW, WSASocketW, WSAStartup, INVALID_SOCKET, WSADATA, WSAPROTOCOL_INFOW,
    WSA_FLAG_OVERLAPPED,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, ResumeThread,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTUPINFOEXW,
};

use crate::platform::windows::process::handle::OwnedHandle;

/// Sidecar magic: "ENVDWIN1". A Linux blob never has a sidecar, and a
/// Windows sidecar is gated by this magic plus its own version.
const SIDECAR_MAGIC: [u8; 8] = *b"ENVDWIN1";
const SIDECAR_VERSION: u32 = 1;
/// How long the successor has to report 'S' and then 'R'.
const SUCCESSOR_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// One live process record. Reserved for a later carry slice; the current
/// writer emits an empty table (declared deviation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SidecarProcess {
    pub pid: u32,
    pub stdout_read: u64,
    pub stderr_read: u64,
    pub stdin_write: u64,
    pub tty_handle: u64,
    pub flags: u32,
}

/// The Windows-only handover payload written next to the pinned blob.
#[derive(Clone)]
pub(crate) struct Sidecar {
    pub flags: u32,
    /// The duplicated listening socket, valid to `WSASocketW` in the child
    /// only.
    pub socket_info: WSAPROTOCOL_INFOW,
    /// The readiness-pipe write handle as inherited by the child (handle
    /// values are preserved by `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
    pub ready_pipe: u64,
    /// The sandbox Job handle, child-relative. Zero when not carried.
    pub job: u64,
    pub processes: Vec<SidecarProcess>,
}

impl std::fmt::Debug for Sidecar {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Sidecar")
            .field("flags", &self.flags)
            .field("socket_family", &self.socket_info.iAddressFamily)
            .field("ready_pipe", &self.ready_pipe)
            .field("job", &self.job)
            .field("processes", &self.processes)
            .finish()
    }
}

/// `envd-handover.win`, next to the pinned `envd-handover.pb`.
pub(crate) fn sidecar_path() -> PathBuf {
    crate::upgrade::handover_path().with_extension("win")
}

pub(crate) fn write_sidecar(path: &Path, sidecar: &Sidecar) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, encode(sidecar))
}

/// Reads and validates the sidecar. A missing file is `Ok(None)`; a bad magic
/// or an unknown version is a typed refusal, never a mis-read.
pub(crate) fn read_sidecar(path: &Path) -> io::Result<Option<Sidecar>> {
    match std::fs::read(path) {
        Ok(bytes) => decode(&bytes).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn delete_sidecar(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Removes images a previous Windows upgrade renamed out of the way
/// (`<name>.old-<pid>`). A running `.exe` cannot be deleted, so staging renames
/// it; the successor deletes the leftovers once the old process is gone.
pub(crate) fn cleanup_stale_images(target: &Path) {
    let Some(directory) = target.parent() else {
        return;
    };
    let Some(name) = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return;
    };
    let prefix = format!("{name}.old-");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn encode(sidecar: &Sidecar) -> Vec<u8> {
    let socket = protocol_info_bytes(&sidecar.socket_info);
    let mut out = Vec::with_capacity(
        SIDECAR_MAGIC.len() + 16 + 4 + socket.len() + 16 + 4 + sidecar.processes.len() * 36,
    );
    out.extend_from_slice(&SIDECAR_MAGIC);
    out.extend_from_slice(&SIDECAR_VERSION.to_le_bytes());
    out.extend_from_slice(&sidecar.flags.to_le_bytes());
    out.extend_from_slice(&(socket.len() as u32).to_le_bytes());
    out.extend_from_slice(socket);
    out.extend_from_slice(&sidecar.ready_pipe.to_le_bytes());
    out.extend_from_slice(&sidecar.job.to_le_bytes());
    out.extend_from_slice(&(sidecar.processes.len() as u32).to_le_bytes());
    for process in &sidecar.processes {
        out.extend_from_slice(&process.pid.to_le_bytes());
        out.extend_from_slice(&process.stdout_read.to_le_bytes());
        out.extend_from_slice(&process.stderr_read.to_le_bytes());
        out.extend_from_slice(&process.stdin_write.to_le_bytes());
        out.extend_from_slice(&process.tty_handle.to_le_bytes());
        out.extend_from_slice(&process.flags.to_le_bytes());
    }
    out
}

fn decode(bytes: &[u8]) -> io::Result<Sidecar> {
    let invalid = |detail: &str| io::Error::new(io::ErrorKind::InvalidData, detail.to_owned());
    if bytes.len() < SIDECAR_MAGIC.len() + 4 {
        return Err(invalid("handover sidecar is truncated"));
    }
    if bytes[..SIDECAR_MAGIC.len()] != SIDECAR_MAGIC {
        return Err(invalid("handover sidecar has an unknown magic"));
    }
    let mut cursor = SIDECAR_MAGIC.len();
    let u32_at = |cursor: &mut usize| -> io::Result<u32> {
        let end = *cursor + 4;
        let value = bytes
            .get(*cursor..end)
            .ok_or_else(|| invalid("handover sidecar is truncated"))?;
        *cursor = end;
        Ok(u32::from_le_bytes(value.try_into().expect("four bytes")))
    };
    let u64_at = |cursor: &mut usize| -> io::Result<u64> {
        let end = *cursor + 8;
        let value = bytes
            .get(*cursor..end)
            .ok_or_else(|| invalid("handover sidecar is truncated"))?;
        *cursor = end;
        Ok(u64::from_le_bytes(value.try_into().expect("eight bytes")))
    };
    let version = u32_at(&mut cursor)?;
    if version != SIDECAR_VERSION {
        return Err(invalid(&format!(
            "handover sidecar version {version} is not supported (want {SIDECAR_VERSION})"
        )));
    }
    let flags = u32_at(&mut cursor)?;
    let socket_len = u32_at(&mut cursor)? as usize;
    let socket_end = cursor + socket_len;
    let socket_bytes = bytes
        .get(cursor..socket_end)
        .ok_or_else(|| invalid("handover sidecar is truncated"))?;
    let socket_info = protocol_info_from_bytes(socket_bytes)?;
    cursor = socket_end;
    let ready_pipe = u64_at(&mut cursor)?;
    let job = u64_at(&mut cursor)?;
    let count = u32_at(&mut cursor)? as usize;
    let mut processes = Vec::with_capacity(count);
    for _ in 0..count {
        processes.push(SidecarProcess {
            pid: u32_at(&mut cursor)?,
            stdout_read: u64_at(&mut cursor)?,
            stderr_read: u64_at(&mut cursor)?,
            stdin_write: u64_at(&mut cursor)?,
            tty_handle: u64_at(&mut cursor)?,
            flags: u32_at(&mut cursor)?,
        });
    }
    if cursor != bytes.len() {
        return Err(invalid("handover sidecar has trailing bytes"));
    }
    Ok(Sidecar {
        flags,
        socket_info,
        ready_pipe,
        job,
        processes,
    })
}

fn protocol_info_bytes(info: &WSAPROTOCOL_INFOW) -> &[u8] {
    // SAFETY: WSAPROTOCOL_INFOW is plain-old-data (integers, a GUID and a
    // fixed u16 array), so its whole object representation is readable.
    unsafe {
        std::slice::from_raw_parts(
            info as *const WSAPROTOCOL_INFOW as *const u8,
            std::mem::size_of::<WSAPROTOCOL_INFOW>(),
        )
    }
}

fn protocol_info_from_bytes(bytes: &[u8]) -> io::Result<WSAPROTOCOL_INFOW> {
    if bytes.len() != std::mem::size_of::<WSAPROTOCOL_INFOW>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "handover sidecar socket record has the wrong size",
        ));
    }
    // SAFETY: WSAPROTOCOL_INFOW is plain-old-data, and the destination is a
    // fully zeroed value of exactly that type; the copy fills every byte.
    let mut info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            &mut info as *mut WSAPROTOCOL_INFOW as *mut u8,
            bytes.len(),
        );
    }
    Ok(info)
}

/// Reconstructs the listening socket the parent duplicated into this process.
pub(crate) fn adopt_listener(sidecar: &Sidecar) -> io::Result<tokio::net::TcpListener> {
    // The parent duplicates the socket while the successor is suspended, so
    // this process has never touched Winsock: initialize it first. Repeated
    // WSAStartup calls are counted, and process exit releases the reference.
    let mut data: WSADATA = unsafe { std::mem::zeroed() };
    // SAFETY: `data` is a valid out-pointer for the lifetime of the call.
    let started = unsafe { WSAStartup(0x0202, &mut data) };
    if started != 0 {
        return Err(io::Error::other(format!("WSAStartup failed: {started}")));
    }
    let info = &sidecar.socket_info;
    // SAFETY: `info` came from WSADuplicateSocketW in the parent and is valid
    // in this process; the returned SOCKET is owned by the listener below.
    let socket = unsafe {
        WSASocketW(
            info.iAddressFamily,
            info.iSocketType,
            info.iProtocol,
            info,
            0,
            WSA_FLAG_OVERLAPPED,
        )
    };
    if socket == INVALID_SOCKET {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the socket was just created for this process and is not owned
    // elsewhere; from_raw_socket takes ownership.
    let listener = unsafe { std::net::TcpListener::from_raw_socket(socket as RawSocket) };
    listener.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(listener)
}

/// Writes one readiness byte to the inherited pipe. The successor calls this
/// with the handle value from the sidecar; the handle belongs to the parent's
/// pipe and must not be closed here.
pub(crate) fn write_ready(handle: u64, byte: u8) -> io::Result<()> {
    let handle = handle as usize as HANDLE;
    let mut written = 0u32;
    // SAFETY: the handle was inherited from the parent and is valid for the
    // lifetime of the pipe; the buffer is one byte.
    let ok = unsafe { WriteFile(handle, &byte, 1, &mut written, std::ptr::null_mut()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A spawned successor that has reconstructed the listener and reported 'S'.
///
/// The outgoing daemon closes its accept gate, calls [`Successor::wait_serving`]
/// and only then exits. Dropping an unconfirmed successor aborts it (and
/// removes the sidecar), so every early-return path keeps the old process
/// serving alone.
pub(crate) struct Successor {
    process: OwnedHandle,
    ready_read: OwnedHandle,
    sidecar: PathBuf,
    confirmed: bool,
}

impl Successor {
    /// Waits for 'R': the successor finished its resume actions and is about
    /// to serve on the inherited socket.
    pub(crate) fn wait_serving(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + SUCCESSOR_READY_TIMEOUT;
        read_ready(self.ready_read.raw(), deadline, b'R')?;
        self.confirmed = true;
        Ok(())
    }

    fn stop(&mut self) {
        if self.confirmed {
            return;
        }
        self.confirmed = true;
        // SAFETY: the process handle is live and owned here.
        unsafe {
            TerminateProcess(self.process.raw(), 1);
        }
        delete_sidecar(&self.sidecar);
    }
}

impl Drop for Successor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawns the successor suspended, duplicates the listening socket and the
/// readiness pipe into it, writes the sidecar, resumes it and waits for 'S'
/// (listener reconstructed). The caller then closes its accept gate and waits
/// for 'R' with [`Successor::wait_serving`].
///
/// The caller writes the pinned blob before calling this; on success it exits
/// without sending an HTTP response.
pub(crate) fn start_successor(
    socket: RawSocket,
    executable: &Path,
    args: &[OsString],
    sidecar: &Path,
) -> Result<Successor, String> {
    let (ready_read, ready_write) = create_ready_pipe().map_err(|error| error.to_string())?;
    // The handle-list attribute requires an inheritable handle. CreatePipe
    // leaves it non-inheritable, so opt in explicitly.
    // SAFETY: the handle is owned and live for this call.
    if unsafe { SetHandleInformation(ready_write.raw(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
        == 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    let application = wide(executable.as_os_str());
    // CreateProcessW may modify the command-line buffer, so it must be
    // mutable and null-terminated.
    let mut command_line = command_line(executable, args);
    let attributes = AttributeList::new(&[ready_write.raw()]).map_err(|error| error.to_string())?;
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes.list();
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is valid for the duration of the call; the
    // application and command lines are null-terminated and outlive it.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            // Non-zero BOOL: the child inherits exactly the handles listed in
            // the attribute below.
            1,
            CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null(),
            std::ptr::null(),
            &startup.StartupInfo,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let process = match OwnedHandle::new(process_info.hProcess) {
        Ok(process) => process,
        Err(error) => return Err(error.to_string()),
    };
    let thread = match OwnedHandle::new(process_info.hThread) {
        Ok(thread) => thread,
        Err(error) => return Err(error.to_string()),
    };
    let outcome = (|| -> Result<(), String> {
        let mut socket_info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
        // SAFETY: the socket is the caller's live listening socket (it stays
        // open until this process exits), the child is suspended so its pid
        // is stable, and `socket_info` is a valid out-pointer.
        let duplicated = unsafe {
            WSADuplicateSocketW(socket as usize, process_info.dwProcessId, &mut socket_info)
        };
        if duplicated != 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        let sidecar_value = Sidecar {
            flags: 0,
            socket_info,
            ready_pipe: ready_write.raw() as usize as u64,
            job: 0,
            processes: Vec::new(),
        };
        write_sidecar(sidecar, &sidecar_value).map_err(|error| error.to_string())?;
        // SAFETY: the primary thread handle is live and owned here.
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            return Err(io::Error::last_os_error().to_string());
        }
        read_ready(
            ready_read.raw(),
            Instant::now() + SUCCESSOR_READY_TIMEOUT,
            b'S',
        )
    })();
    if let Err(error) = outcome {
        // SAFETY: the process handle is live and owned here. This is the
        // failure path: the successor never reconstructed the listener, so it
        // must not keep serving on the inherited socket.
        unsafe {
            TerminateProcess(process.raw(), 1);
        }
        delete_sidecar(sidecar);
        return Err(error);
    }
    Ok(Successor {
        process,
        ready_read,
        sidecar: sidecar.to_path_buf(),
        confirmed: false,
    })
}

fn create_ready_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: both out-pointers are valid; no security attributes means
    // default (non-inheritable) handles, which the caller opts in explicitly.
    let created = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((OwnedHandle::new(read)?, OwnedHandle::new(write)?))
}

fn read_ready(handle: HANDLE, deadline: Instant, expected: u8) -> Result<(), String> {
    let now = Instant::now();
    if now >= deadline {
        return Err("the successor did not report readiness before the timeout".to_owned());
    }
    let remaining = deadline.saturating_duration_since(now).as_millis() as u32;
    // SAFETY: the handle is the live read end of the readiness pipe.
    let waited = unsafe { WaitForSingleObject(handle, remaining.max(1)) };
    if waited == WAIT_TIMEOUT {
        return Err("the successor did not report readiness before the timeout".to_owned());
    }
    if waited != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let mut byte = 0u8;
    let mut read = 0u32;
    // SAFETY: one-byte buffer; the handle is live.
    let ok = unsafe { ReadFile(handle, &mut byte, 1, &mut read, std::ptr::null_mut()) };
    if ok == 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    if read == 0 {
        return Err("the successor closed the readiness pipe".to_owned());
    }
    if byte != expected {
        return Err(format!(
            "the successor reported {byte:?} where {expected:?} was expected"
        ));
    }
    Ok(())
}

struct AttributeList {
    buffer: Vec<u8>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttributeList {
    fn new(handles: &[HANDLE]) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: the sizing call accepts a null list and a valid size
        // out-pointer; it always fails with ERROR_INSUFFICIENT_BUFFER.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; size];
        let list = buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `list` points at `size` writable bytes.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let list = Self { buffer, list };
        // SAFETY: `handles` is a live slice for this call; the attribute list
        // copies the values.
        if unsafe {
            UpdateProcThreadAttribute(
                list.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr() as *const c_void,
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn list(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialized by InitializeProcThreadAttributeList
        // and is dropped exactly once; `buffer` outlives this call.
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
        // The attribute list points into `buffer`; keep the relation explicit.
        debug_assert!(!self.buffer.is_empty());
    }
}

/// The MSVCRT command line for the successor: the executable, the original
/// argv (without argv[0]) and the appended resume flag.
fn command_line(executable: &Path, args: &[OsString]) -> Vec<u16> {
    let mut line = Vec::new();
    let argv =
        std::iter::once(executable.as_os_str().to_owned()).chain(args.iter().cloned().chain(
            std::iter::once(OsString::from(crate::upgrade::RESUME_HANDOVER_FLAG)),
        ));
    for (index, argument) in argv.enumerate() {
        if index > 0 {
            line.push(b' ' as u16);
        }
        append_quoted(&mut line, &argument);
    }
    line.push(0);
    line
}

fn append_quoted(line: &mut Vec<u16>, argument: &OsStr) {
    let wide: Vec<u16> = argument.encode_wide().collect();
    let needs_quotes = wide.is_empty()
        || wide
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16 || *unit == b'"' as u16);
    if !needs_quotes {
        line.extend_from_slice(&wide);
        return;
    }
    line.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in wide {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else if unit == b'"' as u16 {
            for _ in 0..backslashes * 2 + 1 {
                line.push(b'\\' as u16);
            }
            line.push(b'"' as u16);
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                line.push(b'\\' as u16);
            }
            line.push(unit);
            backslashes = 0;
        }
    }
    for _ in 0..backslashes * 2 {
        line.push(b'\\' as u16);
    }
    line.push(b'"' as u16);
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(socket_info: WSAPROTOCOL_INFOW) -> Sidecar {
        Sidecar {
            flags: 7,
            socket_info,
            ready_pipe: 0x1234,
            job: 0,
            processes: vec![SidecarProcess {
                pid: 42,
                stdout_read: 1,
                stderr_read: 2,
                stdin_write: 3,
                tty_handle: 4,
                flags: 5,
            }],
        }
    }

    #[test]
    fn the_sidecar_round_trips() {
        let mut info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
        info.iAddressFamily = 23;
        info.iSocketType = 1;
        info.iProtocol = 6;
        info.szProtocol[0] = b'T' as u16;
        let sidecar = sample(info);
        let decoded = decode(&encode(&sidecar)).expect("round trip");
        assert_eq!(decoded.flags, sidecar.flags);
        assert_eq!(decoded.ready_pipe, sidecar.ready_pipe);
        assert_eq!(decoded.processes, sidecar.processes);
        assert_eq!(decoded.socket_info.iAddressFamily, 23);
        assert_eq!(decoded.socket_info.szProtocol[0], b'T' as u16);
    }

    #[test]
    fn a_bad_magic_or_version_is_refused() {
        let info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
        let mut bytes = encode(&sample(info));
        bytes[0] = b'X';
        assert!(decode(&bytes).is_err(), "unknown magic");

        let mut bytes = encode(&sample(info));
        bytes[8..12].copy_from_slice(&(SIDECAR_VERSION + 1).to_le_bytes());
        let error = decode(&bytes).expect_err("unknown version");
        assert!(error.to_string().contains("not supported"), "{error}");
    }

    #[test]
    fn a_truncated_or_padded_sidecar_is_refused() {
        let info: WSAPROTOCOL_INFOW = unsafe { std::mem::zeroed() };
        let bytes = encode(&sample(info));
        assert!(decode(&bytes[..bytes.len() - 1]).is_err(), "truncated");
        let mut padded = bytes.clone();
        padded.push(0);
        assert!(decode(&padded).is_err(), "trailing bytes");
    }

    #[test]
    fn the_command_line_quotes_argv_like_the_msvcrt() {
        let line = command_line(
            Path::new(r"C:\Program Files\e2b\envd.next.exe"),
            &[
                OsString::from("--listen"),
                OsString::from("127.0.0.1:49983"),
                OsString::from(""),
            ],
        );
        let text: String = line
            .iter()
            .copied()
            .take_while(|unit| *unit != 0)
            .map(|unit| char::from_u32(unit as u32).unwrap())
            .collect();
        assert_eq!(
            text,
            r#""C:\Program Files\e2b\envd.next.exe" --listen 127.0.0.1:49983 "" --resume-handover"#
        );
        assert_eq!(*line.last().unwrap(), 0);
    }
}
