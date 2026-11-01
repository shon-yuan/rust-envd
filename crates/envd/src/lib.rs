//! Rust envd protocol types pinned to the official 0.10.2 contract.
//!
//! This crate is under construction and is not the running sandbox daemon.

pub mod auth;
pub mod cacerts;
pub mod cgroups;
pub mod clock;
pub mod collapse;
pub mod cpus;
pub mod daemon;
pub mod errno_text;
pub mod exec_context;
pub mod filesystem;
pub mod fsfreeze;
pub mod goquote;
pub mod guest;
pub mod hyperloop;
pub mod logs;
pub mod memory_protection;
pub mod metrics;
pub mod mmds;
pub mod oom;
pub mod port;
pub mod process;
pub mod transport;
pub mod upgrade;

pub mod proto {
    connectrpc::include_generated!();
}
