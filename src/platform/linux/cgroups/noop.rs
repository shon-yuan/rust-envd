//! Oracle noop.go: the manager for guests that disabled cgroups or could not
//! set them up. It manages no cgroups: freeze state is unobservable, which is
//! a distinct outcome from "not frozen yet" (iface.go:9-15).

use super::{CgroupError, Manager, ProcessType};

#[derive(Debug, Default)]
pub struct NoopManager;

impl Manager for NoopManager {
    fn get_file_descriptor(&self, _process_type: ProcessType) -> Option<i32> {
        None
    }
    fn freeze(&self, _process_type: ProcessType) -> Result<(), CgroupError> {
        Ok(())
    }
    fn unfreeze(&self, _process_type: ProcessType) -> Result<(), CgroupError> {
        Ok(())
    }
    fn frozen(&self, _process_type: ProcessType) -> Result<bool, CgroupError> {
        Err(CgroupError::FrozenUnobservable)
    }
    fn close(&self) -> Result<(), CgroupError> {
        Ok(())
    }
}
