//! `CreateProcessW` spawn: command line, environment block, stdio, Job
//! assignment and the `ResumeThread` hand-off.
//!
//! Windows has no `/bin/sh -c` wrapper here (declared deviation): the request
//! command is the executable itself and the system search path resolves it.
//! The child is created suspended so it can be assigned to its Job before any
//! thread runs; a failure after creation terminates it instead of leaking a
//! running process.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io;
use std::mem::size_of;

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::HPCON;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, ResumeThread,
    TerminateProcess, UpdateProcThreadAttribute, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::conpty::ConPty;
use super::handle::OwnedHandle;
use super::pipe::{self, StdinWriter};
use super::{wrapped_spawn_error, WindowsProcessRuntime, KILL_EXIT_CODE};
use crate::platform::windows::job::{JobHandle, JobLimits};
use crate::process_contract::{start_error, RuntimeError};
use crate::proto::process as pb;

/// `CreateProcessW` returns `(HANDLE)-1` from `ResumeThread` on failure.
const RESUME_FAILED: u32 = u32::MAX;

pub(super) struct Spawned {
    pub pid: u32,
    pub process: OwnedHandle,
    pub job: std::sync::Arc<JobHandle>,
    pub stdin: Option<StdinWriter>,
    pub stdout: Option<OwnedHandle>,
    pub stderr: Option<OwnedHandle>,
    pub pty: Option<std::sync::Arc<ConPty>>,
}

/// Creates the child, wires its I/O, assigns the Jobs and resumes it.
pub(super) fn spawn(
    runtime: &WindowsProcessRuntime,
    request: &pb::StartRequest,
    cwd: &str,
) -> Result<Spawned, RuntimeError> {
    let config = &*request.process;
    let command_line = command_line(config)
        .map_err(|detail| RuntimeError::InvalidArgument(start_error(request, cwd, detail)))?;
    let environment = environment_block(runtime, config)
        .map_err(|error| wrapped_spawn_error(request, cwd, &error))?;

    let pty = request
        .pty
        .as_option()
        .map(ConPty::new)
        .transpose()
        .map_err(|error| wrapped_spawn_error(request, cwd, &error))?;

    // PTY children get their standard handles from conhost; non-PTY children
    // inherit exactly the pipe ends named in the handle list.
    let (stdin_writer, mut child_handles, stdin_child) = if pty.is_some() {
        (None, Vec::new(), None)
    } else if request.stdin.unwrap_or(true) {
        let (daemon_write, child_read) =
            pipe::stdin_pipe().map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
        let handle = child_read.raw();
        (
            Some(StdinWriter::new(daemon_write)),
            vec![handle],
            Some(child_read),
        )
    } else {
        let nul = nul_handle().map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
        let handle = nul.raw();
        (None, vec![handle], Some(nul))
    };
    let (stdout_daemon, stdout_child) = if pty.is_some() {
        (None, None)
    } else {
        let (daemon_read, child_write) =
            pipe::output_pipe().map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
        child_handles.push(child_write.raw());
        (Some(daemon_read), Some(child_write))
    };
    let (stderr_daemon, stderr_child) = if pty.is_some() {
        (None, None)
    } else {
        let (daemon_read, child_write) =
            pipe::output_pipe().map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
        child_handles.push(child_write.raw());
        (Some(daemon_read), Some(child_write))
    };

    let attributes =
        ProcThreadAttributes::new().map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
    if let Some(pty) = &pty {
        let hpc = pty
            .hpc()
            .map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
        attributes
            .set_pseudo_console(&hpc)
            .map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
    } else {
        attributes
            .set_handle_list(&child_handles)
            .map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
    }

    let mut startup: STARTUPINFOEXW =
        // SAFETY: STARTUPINFOEXW is a plain C struct whose all-zero value is
        // the documented default before `cb`/the attribute list are set.
        unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes.list();
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    if pty.is_none() {
        startup.StartupInfo.hStdInput = stdin_child
            .as_ref()
            .map(OwnedHandle::raw)
            .unwrap_or(std::ptr::null_mut());
        startup.StartupInfo.hStdOutput = stdout_child
            .as_ref()
            .map(OwnedHandle::raw)
            .unwrap_or(std::ptr::null_mut());
        startup.StartupInfo.hStdError = stderr_child
            .as_ref()
            .map(OwnedHandle::raw)
            .unwrap_or(std::ptr::null_mut());
    } else {
        // A pseudoconsole child must get conhost's console handles, never the
        // daemon's. Without an explicit (invalid) standard-handle triple the
        // inherited redirect wins: a daemon whose stdout is a log file hands
        // that file to every PTY child, whose output then never reaches the
        // pseudoconsole. Verified in the guest: with this triple the child's
        // `echo` and `mode con` arrive on the PTY stream and report the
        // requested 133x47; without it only the pseudoconsole's own 86-byte
        // init sequence arrives and the child's text lands in the daemon log.
        startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    }

    let mut flags = CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;
    if pty.is_none() {
        flags |= CREATE_NEW_PROCESS_GROUP;
    }
    let mut command_line = command_line;
    let cwd_wide: Vec<u16> = cwd.encode_utf16().chain(std::iter::once(0)).collect();
    let mut process_information: PROCESS_INFORMATION =
        // SAFETY: PROCESS_INFORMATION is a plain C struct whose all-zero
        // value is a valid out-parameter initializer.
        unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is valid for the duration of the call: the
    // command line and environment are NUL-terminated UTF-16 buffers with
    // writable command-line storage, the startup block is a live
    // STARTUPINFOEXW whose first member is a STARTUPINFOW, and the
    // out-parameter is a valid PROCESS_INFORMATION.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            i32::from(pty.is_none()),
            flags,
            environment.as_ptr().cast::<c_void>(),
            cwd_wide.as_ptr(),
            &startup.StartupInfo,
            &mut process_information,
        )
    };
    if created == 0 {
        return Err(wrapped_spawn_error(
            request,
            cwd,
            &io::Error::last_os_error(),
        ));
    }
    let process = OwnedHandle::new(process_information.hProcess)
        .map_err(|error| wrapped_spawn_error(request, cwd, &error))?;
    let thread = OwnedHandle::new(process_information.hThread)
        .map_err(|error| terminate_failed(&process, request, cwd, &error))?;
    let pid = process_information.dwProcessId;

    // The parent's copies of the child ends are no longer needed; the child
    // owns the inherited copies.
    drop(stdin_child);
    drop(stdout_child);
    drop(stderr_child);

    let outcome = (|| -> io::Result<std::sync::Arc<JobHandle>> {
        // Nested jobs require root-first assignment. The sandbox Job is the
        // root every workload pid joins; the per-process Job is then a child
        // holding exactly this process, so `TerminateJobObject` on it reaps
        // the child's tree without touching other workloads. Assigning the
        // per-process Job first would make the sandbox Job its child, and the
        // next process's sandbox assignment would fail because the sandbox
        // would then hold a pid outside that first per-process Job.
        runtime.sandbox_job.assign(pid)?;
        let job = std::sync::Arc::new(JobHandle::create()?);
        job.set_limits(JobLimits {
            kill_on_job_close: true,
            ..Default::default()
        })?;
        job.assign(pid)?;
        // SAFETY: the thread handle is the suspended primary thread created
        // for this child and is owned by this scope.
        if unsafe { ResumeThread(thread.raw()) } == RESUME_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    })();
    let job = match outcome {
        Ok(job) => job,
        Err(error) => {
            return Err(terminate_failed(&process, request, cwd, &error));
        }
    };
    drop(thread);

    Ok(Spawned {
        pid,
        process,
        job,
        stdin: stdin_writer,
        stdout: stdout_daemon,
        stderr: stderr_daemon,
        pty,
    })
}

fn terminate_failed(
    process: &OwnedHandle,
    request: &pb::StartRequest,
    cwd: &str,
    error: &io::Error,
) -> RuntimeError {
    // SAFETY: the process handle is owned and valid while `process` lives.
    unsafe {
        TerminateProcess(process.raw(), KILL_EXIT_CODE);
    }
    wrapped_spawn_error(request, cwd, error)
}

/// `CommandLineToArgvW`-compatible quoting for `cmd` followed by `args`.
fn command_line(config: &pb::ProcessConfig) -> Result<Vec<u16>, &'static str> {
    if config.cmd.contains('\0') || config.args.iter().any(|arg| arg.contains('\0')) {
        return Err("command contains a NUL byte");
    }
    let mut line = Vec::new();
    append_quoted(&mut line, &config.cmd);
    for arg in &config.args {
        line.push(u16::from(b' '));
        append_quoted(&mut line, arg);
    }
    line.push(0);
    Ok(line)
}

fn append_quoted(out: &mut Vec<u16>, argument: &str) {
    let needs_quotes = argument.is_empty()
        || argument
            .chars()
            .any(|ch| ch == ' ' || ch == '\t' || ch == '"');
    if !needs_quotes {
        out.extend(argument.encode_utf16());
        return;
    }
    out.push(u16::from(b'"'));
    let mut backslashes = 0usize;
    for ch in argument.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // 2n backslashes plus an escaped quote become n backslashes
                // and one literal quote.
                out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2 + 1));
                out.push(u16::from(b'"'));
                backslashes = 0;
            }
            other => {
                out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
                backslashes = 0;
                let mut buffer = [0u16; 2];
                out.extend_from_slice(other.encode_utf16(&mut buffer));
            }
        }
    }
    // 2n trailing backslashes become n before the closing quote.
    out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
    out.push(u16::from(b'"'));
}

/// The UTF-16 environment block: the process environment contract shared with
/// Linux (`Environment::for_process` overlaid with the request's `envs`) plus
/// the variables a Windows child needs in order to start a normal process.
///
/// The daemon's own environment is deliberately *not* inherited, matching
/// Linux's `env_clear`: a credential or provider secret in the service
/// environment must not become readable by every sandbox process. The few
/// Windows defaults below are fixed, well-known names, so reading their values
/// from the daemon's environment cannot leak an unrelated variable.
fn environment_block(
    runtime: &WindowsProcessRuntime,
    config: &pb::ProcessConfig,
) -> io::Result<Vec<u16>> {
    validate_environment(config)?;
    let request_environment: BTreeMap<String, String> = config
        .envs
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let overlay = runtime.environment.for_process(
        &runtime.account,
        runtime.daemon_path.as_deref(),
        &request_environment,
    );
    // Windows environment names are case-insensitive. Keep the spelling the
    // overlay supplied, but merge on the folded name.
    let mut merged: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (key, value) in overlay {
        if !key.is_empty() {
            merged.insert(key.to_ascii_uppercase(), (key, value));
        }
    }
    let system_root = std::env::var("SystemRoot")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "C:\\Windows".to_owned());
    ensure(&mut merged, "SystemRoot", Some(system_root));
    let effective_root = merged
        .get("SYSTEMROOT")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| "C:\\Windows".to_owned());
    ensure(&mut merged, "SystemDrive", Some(drive_of(&effective_root)));
    let comspec = std::env::var("ComSpec")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{effective_root}\\System32\\cmd.exe"));
    ensure(&mut merged, "ComSpec", Some(comspec));
    let temp = std::env::var("TEMP")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    ensure(&mut merged, "TEMP", Some(temp.clone()));
    let tmp = std::env::var("TMP")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or(temp);
    ensure(&mut merged, "TMP", Some(tmp));
    let pathext = std::env::var("PATHEXT")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
    ensure(&mut merged, "PATHEXT", Some(pathext));

    let mut block = Vec::new();
    for (_, (key, value)) in merged {
        block.extend(key.encode_utf16());
        block.push(u16::from(b'='));
        block.extend(value.encode_utf16());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn ensure(merged: &mut BTreeMap<String, (String, String)>, key: &str, value: Option<String>) {
    let folded = key.to_ascii_uppercase();
    if let std::collections::btree_map::Entry::Vacant(slot) = merged.entry(folded) {
        if let Some(value) = value {
            slot.insert((key.to_owned(), value));
        }
    }
}

/// Rejects request environments the Windows block cannot represent faithfully.
///
/// A NUL would truncate an entry and splice the remainder into the block as
/// extra variables; Linux's `Command` refuses it with the same `InvalidInput`
/// text. An empty name or a name starting with `=` would produce a hidden
/// drive variable (`=C:=...`), which is not a name the child can read back as
/// the one it asked for. An embedded `=` elsewhere splits into the same
/// name/value pair on both platforms, so it stays allowed.
fn validate_environment(config: &pb::ProcessConfig) -> io::Result<()> {
    let invalid = |detail: &str| io::Error::new(io::ErrorKind::InvalidInput, detail.to_owned());
    for (key, value) in &config.envs {
        if key.contains('\0') || value.contains('\0') {
            return Err(invalid("nul byte found in provided data"));
        }
        if key.is_empty() {
            return Err(invalid("environment variable name must not be empty"));
        }
        if key.starts_with('=') {
            return Err(invalid("environment variable name cannot start with '='"));
        }
    }
    Ok(())
}

fn drive_of(system_root: &str) -> String {
    let mut chars = system_root.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) => format!("{drive}:"),
        _ => "C:".to_owned(),
    }
}

/// The NUL device handle for a child whose stdin was not requested. It is
/// inheritable so it can appear in the handle list.
fn nul_handle() -> io::Result<OwnedHandle> {
    let name: Vec<u16> = "NUL".encode_utf16().chain(std::iter::once(0)).collect();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: the name is a NUL-terminated UTF-16 string, `attributes` is a
    // live SECURITY_ATTRIBUTES, and the returned handle is checked before
    // ownership is taken.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    OwnedHandle::new(handle)
}

/// The initialized `PROC_THREAD_ATTRIBUTE_LIST` buffer and its cleanup.
struct ProcThreadAttributes {
    _buffer: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl ProcThreadAttributes {
    fn new() -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a null list with a valid size out-pointer is the documented
        // sizing call; it fails with ERROR_INSUFFICIENT_BUFFER and fills size.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = size.div_ceil(size_of::<usize>());
        let mut buffer = vec![0usize; words.max(1)];
        let list = buffer.as_mut_ptr().cast::<c_void>();
        // SAFETY: the allocation is at least `size` bytes and pointer
        // aligned; `size` is the value the sizing call reported.
        let initialized = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) };
        if initialized == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _buffer: buffer,
            list,
        })
    }

    fn list(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list
    }

    fn set_handle_list(&self, handles: &[HANDLE]) -> io::Result<()> {
        self.set(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            handles.as_ptr().cast::<c_void>(),
            std::mem::size_of_val(handles),
        )
    }

    fn set_pseudo_console(&self, hpc: &HPCON) -> io::Result<()> {
        // Unlike every other attribute, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE
        // takes the HPCON *value* as `lpValue`; passing the address of the
        // handle makes CreateProcessW connect the child to a garbage handle
        // and the child silently runs without a console (verified against the
        // EchoCon sample's `UpdateProcThreadAttribute(..., hPC, sizeof(HPCON))`
        // and by the ConPTY contract tests: no output, immediate exit 0).
        self.set(
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            *hpc as *const c_void,
            size_of::<HPCON>(),
        )
    }

    fn set(&self, attribute: u32, value: *const c_void, size: usize) -> io::Result<()> {
        // SAFETY: the list is initialized, and `value` points to `size`
        // readable bytes for the duration of the synchronous call.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                self.list,
                0,
                attribute as usize,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for ProcThreadAttributes {
    fn drop(&mut self) {
        // SAFETY: the list was successfully initialized in `new` and is
        // deleted exactly once here.
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{command_line, validate_environment};
    use crate::proto::process as pb;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    fn config(cmd: &str, args: &[&str]) -> pb::ProcessConfig {
        serde_json::from_value(serde_json::json!({"cmd": cmd, "args": args})).unwrap()
    }

    fn wide(pointer: *const u16) -> String {
        let mut length = 0usize;
        // SAFETY: the pointer is a NUL-terminated UTF-16 string owned by argv.
        while unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
    }

    /// The generated command line must round-trip through the same parser
    /// CreateProcessW's consumers use.
    #[test]
    fn command_line_quoting_round_trips_windows_arguments() {
        let original = [
            "",
            "plain",
            "two words",
            "\"quoted\"",
            "trailing\\\\",
            "tab\there",
            "雪",
        ];
        let mut line = command_line(&config("cmd.exe", &original)).unwrap();
        assert_eq!(line.last(), Some(&0));
        let mut argc = 0i32;
        // SAFETY: `line` is NUL-terminated and stays alive across the call;
        // CommandLineToArgvW returns a LocalAlloc'd argv array checked below.
        let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut argc) };
        assert!(!argv.is_null());
        assert_eq!(argc as usize, original.len() + 1);
        // SAFETY: argv holds `argc` valid pointers.
        assert_eq!(wide(unsafe { *argv }), "cmd.exe");
        for (index, expected) in original.iter().enumerate() {
            // SAFETY: index < argc - 1.
            let argument = unsafe { *argv.add(index + 1) };
            assert_eq!(&wide(argument), expected, "argument {index}");
        }
        // SAFETY: argv was allocated by CommandLineToArgvW/LocalAlloc.
        unsafe {
            LocalFree(argv.cast());
        }
        line.clear();
    }

    #[test]
    fn command_line_rejects_nul_before_create_process() {
        assert_eq!(
            command_line(&config("cmd\0.exe", &[])).unwrap_err(),
            "command contains a NUL byte"
        );
        assert_eq!(
            command_line(&config("cmd.exe", &["a\0b"])).unwrap_err(),
            "command contains a NUL byte"
        );
    }

    /// A NUL would splice extra variables into the block; an empty or
    /// `=`-leading name would become a hidden drive variable.
    #[test]
    fn environment_rejects_values_the_block_cannot_represent() {
        let environment = |envs: serde_json::Value| -> pb::ProcessConfig {
            serde_json::from_value(serde_json::json!({"cmd": "cmd.exe", "envs": envs})).unwrap()
        };
        for (envs, expected) in [
            (
                serde_json::json!({"A": "x\u{0}y"}),
                "nul byte found in provided data",
            ),
            (
                serde_json::json!({"A\u{0}B": "v"}),
                "nul byte found in provided data",
            ),
            (
                serde_json::json!({"": "v"}),
                "environment variable name must not be empty",
            ),
            (
                serde_json::json!({"=": "v"}),
                "environment variable name cannot start with '='",
            ),
        ] {
            let error = validate_environment(&environment(envs)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert_eq!(error.to_string(), expected);
        }
        // An embedded `=` splits into the same pair on both platforms.
        validate_environment(&environment(serde_json::json!({"A=B": "v"}))).unwrap();
    }
}
