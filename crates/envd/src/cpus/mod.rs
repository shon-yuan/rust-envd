// SPDX-License-Identifier: Apache-2.0
//! CPU hotplug manager adapted from E2B runtime 51083a83 internal/services/cpus.
//! Main must explicitly dispatch the write helper and hand registry PIDs across exec.

mod children;
mod manager;
mod sysfs;

pub use children::{ChildRegistry, ExportHold};
pub use manager::{CpuManager, State};
pub use sysfs::{parse_mask, write_online_file, ChildWrites, CpuSysfs, KernelSysfs};
