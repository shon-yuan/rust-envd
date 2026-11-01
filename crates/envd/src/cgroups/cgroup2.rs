//! The real cgroup v2 manager, ported from oracle cgroup2.go (283 lines).
//! Construction writes to the cgroup filesystem, so only the guest main builds
//! one; the statfs gate at the top refuses anything that is not a cgroup2
//! mount before a single byte is written.

use std::collections::HashMap;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{CgroupError, Manager, PathManager, ProcessType};

/// Free-form path operations shared with the injected fake, so both managers
/// exercise identical file semantics. Each cites its oracle line.
pub fn frozen_from_events(bytes: &[u8]) -> bool {
    for line in String::from_utf8_lossy(bytes).split('\n') {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(' ') {
            if key == "frozen" {
                return value == "1";
            }
        }
    }
    false
}

/// Oracle cgroup2.go:115-126: O_WRONLY|O_TRUNC without O_CREATE, so a missing
/// property errors instead of being silently created on a tmpfs fallback.
pub fn write_cgroup_prop(path: &Path, value: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(value.as_bytes())
}

pub fn set_freeze_request(path: &Path, frozen: bool) -> std::io::Result<()> {
    write_cgroup_prop(&path.join("cgroup.freeze"), if frozen { "1" } else { "0" })
}

/// Oracle cgroup2.go:203-212.
pub fn settled_frozen(path: &Path) -> std::io::Result<bool> {
    let bytes = std::fs::read(path.join("cgroup.events"))?;
    Ok(frozen_from_events(&bytes))
}

/// Oracle cgroup2.go:214-223: the cgroup's own requested state, trimmed.
pub fn freeze_requested(path: &Path) -> std::io::Result<bool> {
    let body = std::fs::read_to_string(path.join("cgroup.freeze"))?;
    Ok(body.trim() == "1")
}

/// Oracle cgroup2.go:179-193: immediate child cgroups, directories only.
pub fn child_cgroups(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut children = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            children.push(path.join(entry.file_name()));
        }
    }
    Ok(children)
}

#[derive(Debug, Clone)]
pub struct Cgroup2Entry {
    pub process_type: ProcessType,
    /// Relative to the root, the way main.go configures it ("ptys", "user",
    /// "socats" at main.go:516-533).
    pub relative_path: String,
    pub properties: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct Cgroup2Options {
    pub root: Option<PathBuf>,
    pub entries: Vec<Cgroup2Entry>,
}

pub struct Cgroup2Manager {
    // Mutex only so Manager::close can take &self while draining the table
    // the way oracle cgroup2.go:273-283 deletes from its map as it closes.
    fds: std::sync::Mutex<HashMap<ProcessType, i32>>,
    paths: HashMap<ProcessType, PathBuf>,
    root: PathBuf,
}

impl Cgroup2Manager {
    /// Oracle cgroup2.go:53-80. The statfs gate runs BEFORE any cgroup is
    /// created: on cgroup v1 /sys/fs/cgroup is a tmpfs whose directories can
    /// be created freely, yielding invalid fds the kernel only rejects at
    /// clone3(CLONE_INTO_CGROUP) (cgroup2.go:62-65).
    pub fn new(options: Cgroup2Options) -> Result<Self, String> {
        let root = options
            .root
            .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup"));
        let stat = nix::sys::statfs::statfs(&root)
            .map_err(|error| format!("failed to statfs cgroup root {}: {error}", root.display()))?;
        if stat.filesystem_type() != nix::sys::statfs::CGROUP2_SUPER_MAGIC {
            return Err(format!(
                "cgroup root {} is not a cgroup2 filesystem",
                root.display()
            ));
        }
        let fds = std::sync::Mutex::new(HashMap::new());
        let mut paths = HashMap::new();
        let mut errors: Vec<String> = Vec::new();
        for entry in &options.entries {
            let full_path = root.join(&entry.relative_path);
            match create_cgroup(&full_path, &entry.properties) {
                Ok(fd) => {
                    fds.lock()
                        .expect("cgroup fd table")
                        .insert(entry.process_type, fd);
                    paths.insert(entry.process_type, full_path);
                }
                Err(error) => errors.push(format!(
                    "failed to create {} cgroup: {error}",
                    entry.process_type
                )),
            }
        }
        if !errors.is_empty() {
            // Discard whatever was created; a failed close still deletes its
            // entry, but the table dies with the manager here either way.
            if let Err(CgroupError::Io(close_errors)) =
                close_fd_table(&mut fds.lock().expect("cgroup fd table"))
            {
                errors.push(close_errors.to_string());
            }
            return Err(errors.join("\n"));
        }
        Ok(Self { fds, paths, root })
    }
}

/// Oracle cgroup2.go:128-150: MkdirAll 0755, write the properties (skipping
/// the two benign errors with a stderr note), then keep an O_RDONLY fd.
fn create_cgroup(full_path: &Path, properties: &[(String, String)]) -> Result<i32, String> {
    let created = !full_path.exists();
    std::fs::create_dir_all(full_path)
        .map_err(|error| format!("failed to create cgroup root: {error}"))?;
    if created {
        std::fs::set_permissions(full_path, std::fs::Permissions::from_mode(0o755)).ok();
    }
    let mut errors: Vec<String> = Vec::new();
    for (name, value) in properties {
        let result = write_cgroup_prop(&full_path.join(name), value);
        match result {
            Ok(()) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                // Skip properties whose controller isn't enabled in
                // subtree_control (cgroup2.go:136-140).
                eprintln!(
                    "cgroup property {name:?} unavailable at {:?}, skipping",
                    full_path
                );
            }
            Err(error) => errors.push(format!("failed to write cgroup property {name:?}: {error}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let fd = nix::fcntl::open(
        full_path,
        nix::fcntl::OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|error| format!("failed to open cgroup: {error}"))?;
    Ok(fd)
}

impl Manager for Cgroup2Manager {
    fn get_file_descriptor(&self, process_type: ProcessType) -> Option<i32> {
        self.fds
            .lock()
            .expect("cgroup fd table")
            .get(&process_type)
            .copied()
    }
    fn freeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        self.set_freeze_state(process_type, "1")
    }
    fn unfreeze(&self, process_type: ProcessType) -> Result<(), CgroupError> {
        self.set_freeze_state(process_type, "0")
    }
    fn frozen(&self, process_type: ProcessType) -> Result<bool, CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        Ok(settled_frozen(path)?)
    }
    fn close(&self) -> Result<(), CgroupError> {
        close_fd_table(&mut self.fds.lock().expect("cgroup fd table"))
    }
    fn as_path_manager(&self) -> Option<&dyn PathManager> {
        Some(self)
    }
}

/// Oracle cgroup2.go:273-283 Close: every fd is closed and its map entry is
/// deleted as the loop goes, so a second Close iterates an empty map —
/// idempotent — and the deletions stand even when a close reports an error.
/// The semantics live in a free function over the plain table so tests can
/// exercise them over an ordinary fd, with no cgroupfs anywhere.
fn close_fd_table(fds: &mut HashMap<ProcessType, i32>) -> Result<(), CgroupError> {
    let mut errors: Vec<String> = Vec::new();
    for (process_type, fd) in fds.drain() {
        if let Err(error) = nix::unistd::close(fd) {
            errors.push(format!(
                "failed to close cgroup fd for {process_type:?}: {error}"
            ));
        }
    }
    match errors.is_empty() {
        true => Ok(()),
        false => Err(CgroupError::Io(std::io::Error::other(errors.join("\n")))),
    }
}

impl PathManager for Cgroup2Manager {
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

impl Cgroup2Manager {
    /// Oracle cgroup2.go:264-271.
    fn set_freeze_state(&self, process_type: ProcessType, value: &str) -> Result<(), CgroupError> {
        let Some(path) = self.paths.get(&process_type) else {
            return Err(CgroupError::UnknownProcessType(process_type));
        };
        write_cgroup_prop(&path.join("cgroup.freeze"), value)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::IntoRawFd;

    #[test]
    fn close_is_idempotent_like_gos_map_delete() {
        // An ordinary owned fd stands in for the cgroup dir fd: the table
        // only ever sees integers, and no host cgroupfs is involved.
        let dir = tempfile::TempDir::new().unwrap();
        let fd = std::fs::File::create(dir.path().join("cgroup"))
            .unwrap()
            .into_raw_fd();
        let mut fds = HashMap::from([(ProcessType::User, fd)]);
        close_fd_table(&mut fds).unwrap();
        // The entry was deleted as it closed (cgroup2.go:281), so the second
        // Close has nothing left to close: no EBADF, no error.
        close_fd_table(&mut fds).unwrap();
        assert!(fds.is_empty());
    }

    #[test]
    fn a_failed_close_still_deletes_its_entry() {
        // A number no open fd carries: the close errors, but the delete must
        // stand anyway (oracle deletes unconditionally inside the loop), so
        // the retry sees an empty table and succeeds.
        let mut fds = HashMap::from([(ProcessType::User, 9_999_999)]);
        assert!(close_fd_table(&mut fds).is_err());
        assert!(close_fd_table(&mut fds).is_ok());
        assert!(fds.is_empty());
    }
}
