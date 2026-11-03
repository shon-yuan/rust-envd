//! The daemon binary.
//!
//! The Linux entry point lives in `main_linux.rs`; the Windows build is a
//! skeleton until the Windows port reaches P1 (`docs/windows-port.md`).

#[cfg(target_os = "linux")]
#[path = "main_linux.rs"]
mod main_linux;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    main_linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "coorworker-envd: this platform's backend is not implemented yet \
         (see docs/windows-port.md)"
    );
    std::process::exit(1);
}
