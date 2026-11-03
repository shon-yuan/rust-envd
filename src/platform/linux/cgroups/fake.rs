//! Test support: an injected fake cgroupfs over a real directory tree, so the
//! freeze machinery sees the [`Manager`] trait while the assertions see real
//! files. Mirrors Go's fake managers in freeze_test.go. Never referenced by
//! the daemon; tests build it over a tempfile root so no host cgroupfs is
//! ever touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::cgroup2::{child_cgroups, freeze_requested, set_freeze_request, settled_frozen};
use super::{CgroupError, Manager, PathManager, ProcessType};

#[derive(Debug)]
pub struct FakeCgroupfs {
    root: PathBuf,
    paths: HashMap<ProcessType, PathBuf>,
}

impl FakeCgroupfs {
    pub fn new(root: &Path, entries: &[(&str, ProcessType)]) -> Self {
        let paths = entries
            .iter()
            .map(|(relative, process_type)| {
                let path = root.join(relative);
                make_cgroup(&path);
                (*process_type, path)
            })
            .collect();
        Self {
            root: root.to_path_buf(),
            paths,
        }
    }

    /// Creates a customer (non-envd) cgroup in the tree.
    pub fn customer(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        make_cgroup(&path);
        path
    }
}

/// A real cgroupfs directory always carries its interface files; the fake
/// creates them so reads behave like the kernel's (empty files would read as
/// unfrozen, which is the state a fresh cgroup is in).
fn make_cgroup(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("cgroup.freeze"), "0\n").unwrap();
    std::fs::write(path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
}

impl Manager for FakeCgroupfs {
    fn get_file_descriptor(&self, _process_type: ProcessType) -> Option<i32> {
        None
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        set_freeze_request(path, true)?;
        Ok(())
    }
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        set_freeze_request(path, false)?;
        Ok(())
    }
    fn frozen(&self, process_type: ProcessType) -> Result<bool, CgroupError> {
        match self.paths.get(&process_type) {
            Some(path) => Ok(settled_frozen(path)?),
            None => Err(CgroupError::UnknownProcessType(process_type)),
        }
    }
    fn close(&self) -> Result<(), CgroupError> {
        Ok(())
    }
    fn as_path_manager(&self) -> Option<&dyn PathManager> {
        Some(self)
    }
}

impl PathManager for FakeCgroupfs {
    fn root(&self) -> PathBuf {
        self.root.clone()
    }
    fn path_of(&self, process_type: ProcessType) -> Option<PathBuf> {
        self.paths.get(&process_type).cloned()
    }
    fn children_of(&self, path: &Path) -> Result<Vec<PathBuf>, CgroupError> {
        Ok(child_cgroups(path)?)
    }
    fn freeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        set_freeze_request(path, true)?;
        Ok(())
    }
    fn unfreeze_at(&self, path: &Path) -> Result<(), CgroupError> {
        set_freeze_request(path, false)?;
        Ok(())
    }
    fn frozen_at(&self, path: &Path) -> Result<bool, CgroupError> {
        Ok(settled_frozen(path)?)
    }
    fn freeze_requested_at(&self, path: &Path) -> Result<bool, CgroupError> {
        Ok(freeze_requested(path)?)
    }
}
