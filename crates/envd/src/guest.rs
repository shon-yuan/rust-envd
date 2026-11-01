//! Explicit disposable-guest boot opt-in; checked before any privileged startup effects.
#[derive(Debug, thiserror::Error)]
#[error("guest mode requires root PID 1 and kernel argument coorworker.envd=1")]
pub struct GuestBootError;

pub fn validate_guest_boot(uid: u32, pid: u32, cmdline: &str) -> Result<(), GuestBootError> {
    if uid == 0 && pid == 1 && opted_in(cmdline) {
        Ok(())
    } else {
        Err(GuestBootError)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("CPU helper requires root, guest boot opt-in, and the guest PID 1 executable")]
pub struct CpuHelperBootError;

pub fn validate_cpu_helper_boot(
    uid: u32,
    cmdline: &str,
    init_executable: &std::path::Path,
    helper_executable: &std::path::Path,
) -> Result<(), CpuHelperBootError> {
    if uid == 0 && opted_in(cmdline) && init_executable == helper_executable {
        Ok(())
    } else {
        Err(CpuHelperBootError)
    }
}

fn opted_in(cmdline: &str) -> bool {
    cmdline
        .split_whitespace()
        .any(|arg| arg == "coorworker.envd=1")
}
