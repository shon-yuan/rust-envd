//! The daemon binary.
//!
//! The Linux entry point lives in `main_linux.rs`. The Windows entry point
//! lives in the library (`platform::windows::entry`) because the handover
//! sidecar seams are crate-internal; this file only dispatches to it.

#[cfg(target_os = "linux")]
#[path = "main_linux.rs"]
mod main_linux;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    main_linux::run()
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    coorworker_envd::platform::windows::entry::run()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
    eprintln!(
        "coorworker-envd: this platform's backend is not implemented yet \
         (see docs/windows-port.md)"
    );
    std::process::exit(1);
}
