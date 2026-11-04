//! Shared Connect wire adapters mounted by both daemons.
//!
//! The process adapter is platform-neutral: it drives `crate::process_api`
//! and only the runtime object behind the trait differs per target.

pub mod process;
pub mod process_payload;
pub mod rpc_log;
