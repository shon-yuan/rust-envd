//! Rust envd protocol types pinned to the official 0.10.2 contract.
//!
//! This crate is under construction and is not the running sandbox daemon.

pub mod auth;
#[cfg(target_os = "linux")]
pub mod cacerts;
#[cfg(target_os = "linux")]
pub mod clock;
#[cfg(target_os = "linux")]
pub mod collapse;
#[cfg(target_os = "linux")]
pub mod daemon;
#[cfg(target_os = "linux")]
pub mod errno_text;
#[cfg(target_os = "linux")]
pub mod exec_context;
#[cfg(target_os = "linux")]
pub mod fsfreeze;
pub mod goquote;
pub mod guest;
pub mod hyperloop;
#[cfg(target_os = "linux")]
pub mod logs;
pub mod memory_protection;
#[cfg(target_os = "linux")]
pub mod metrics;
#[cfg(target_os = "linux")]
pub mod oom;
pub mod platform;
#[cfg(target_os = "linux")]
pub mod transport;
#[cfg(target_os = "linux")]
pub mod upgrade;

#[cfg(target_os = "linux")]
pub use platform::linux::{cgroups, cpus, filesystem, mmds, port, process};

pub mod proto {
    connectrpc::include_generated!();
}
