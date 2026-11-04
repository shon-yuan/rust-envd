//! Windows platform backends.
//!
//! The filesystem slice is implemented: account resolution, metadata and
//! mutation (`fs`), the shared `POST /files` write seam (`file_writes`), the
//! Connect filesystem runtime (`fs_runtime`), the watcher registry
//! (`watchers`), local-port enumeration (`net`) and the in-process loopback
//! port forwarder (`port`) and DPAPI provisioning (`metadata`). The resource slice is implemented on top of real Win32
//! primitives: the workload registry and spawn gate (`registry`), per-thread
//! suspend/resume (`suspend`), the Job Object wrapper (`job`), the freeze/thaw
//! backend (`resource`, reached through the `ResourceBackend` seam in
//! `cgroup`), the metrics collector (`metrics`), the heap compactor
//! (`collapse`) and the volume-flush freezer (`fsfreeze`). Process execution
//! is implemented on `CreateProcessW`, Job Objects and ConPTY
//! (`process/`); clock adjustment still answers `Unsupported` until its slice
//! lands. The endpoint/method surface is fixed by the traits in
//! `platform/mod.rs`; see `docs/windows-port.md` for the plan.

pub mod account;
pub mod boot;
pub mod cgroup;
pub mod clock;
pub mod collapse;
pub mod compose_files;
pub mod entry;
pub mod file_writes;
pub mod fs;
pub mod fs_runtime;
pub mod fsfreeze;
pub mod handover;
pub mod job;
pub mod log;
pub mod metadata;
pub mod metrics;
pub mod net;
pub mod port;
pub mod process;
pub mod registry;
pub mod resource;
pub mod service;
pub mod suspend;
pub mod upgrade_fs;
pub mod watchers;
