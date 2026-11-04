//! Rust envd protocol types pinned to the official 0.10.2 contract.
//!
//! This crate is under construction and is not the running sandbox daemon.

pub mod auth;
#[cfg(target_os = "linux")]
pub mod cacerts;
pub mod cli;
pub mod clock;
pub mod collapse;
pub(crate) mod compose_decode;
pub mod configuration;
#[cfg(target_os = "linux")]
pub mod daemon;
#[cfg(windows)]
#[path = "daemon_windows.rs"]
pub mod daemon;
#[cfg(target_os = "linux")]
pub mod errno_text;
pub mod exec_context;
pub mod files_compose;
pub mod files_http;
pub mod files_upload;
pub mod filesystem_payload;
pub mod filesystem_rpc;
pub mod freeze_wire;
pub mod fsfreeze;
pub mod goquote;
pub mod guest;
pub mod http;
pub mod http1;
pub mod hyperloop;
pub mod init_payload;
pub mod logs;
pub mod memory_protection;
pub mod metrics;
#[cfg(target_os = "linux")]
pub mod oom;
pub mod platform;
pub mod process_api;
pub mod process_contract;
pub mod rpc;
pub mod transport;
pub mod upgrade;
pub mod upgrade_http;

#[cfg(target_os = "linux")]
pub use platform::linux::{cgroups, cpus, filesystem, mmds, port, process};

pub mod proto {
    connectrpc::include_generated!();
}
