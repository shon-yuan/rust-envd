//! Hierarchy arithmetic and the whole-tree observers, ported from oracle
//! hierarchy.go (483 lines). Pure functions over paths plus the manager, so
//! tests drive them against injected trees.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::freeze::FreezeMode;
use super::PathManager;

/// Oracle hierarchy.go:12.
pub const PROC_SELF_CGROUP: &str = "/proc/self/cgroup";

/// Oracle hierarchy.go:46-54: cgroups that must keep running across a pause,
/// matched as exact entries or subtrees.
pub const LIVENESS_ALLOWLIST: [&str; 6] = [
    "init.scope",
    "system.slice/systemd-journald.service",
    "system.slice/rpcbind.service",
    "system.slice/rpcbind.socket",
    "system.slice/rpc-statd.service",
    "socats",
];

/// Oracle hierarchy.go:66-86: envd's own cgroup, read from the `0::` line of
/// the cgroup membership file. V1 lines carry a non-zero hierarchy ID and are
/// ignored; a missing 0:: line is an error rather than a guess.
pub fn self_cgroup_path(proc_file: &Path, root: &Path) -> Result<PathBuf, String> {
    let contents = std::fs::read_to_string(proc_file)
        .map_err(|error| format!("open {}: {error}", proc_file.display()))?;
    for line in contents.lines() {
        let mut parts = line.splitn(3, ':');
        if let (Some("0"), Some(_), Some(path)) = (parts.next(), parts.next(), parts.next()) {
            // filepath.Join semantics: the membership path is absolute, so a
            // plain PathBuf::join would REPLACE the root.
            return Ok(root.join(path.trim_start_matches('/')));
        }
    }
    Err(format!("no cgroup2 (0::) entry in {}", proc_file.display()))
}

/// Oracle hierarchy.go:95-116: root, then each directory between root and
/// self, then self. A self outside root yields just the root.
pub fn ancestor_chain(root: &Path, self_path: &Path) -> Vec<PathBuf> {
    let Ok(relative) = self_path.strip_prefix(root) else {
        return vec![root.to_path_buf()];
    };
    if relative.as_os_str().is_empty() {
        return vec![root.to_path_buf()];
    }
    let mut chain = vec![root.to_path_buf()];
    let mut current = root.to_path_buf();
    for segment in relative.components() {
        current = current.join(segment);
        chain.push(current.clone());
    }
    chain
}

/// Oracle hierarchy.go:134-159: every cgroup the walk must look INSIDE rather
/// than freeze — envd's own chain plus the ancestors of every allowlist entry.
/// The entries themselves are deliberately NOT added: they must be skipped,
/// not descended into.
pub fn descend_set(root: &Path, self_path: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    let mut add = |path: PathBuf| {
        if seen.insert(path.clone()) {
            order.push(path);
        }
    };
    for cgroup in ancestor_chain(root, self_path) {
        add(cgroup);
    }
    for entry in LIVENESS_ALLOWLIST {
        let full = root.join(entry);
        let parent = full
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| root.to_path_buf());
        for cgroup in ancestor_chain(root, &parent) {
            add(cgroup);
        }
    }
    order
}

/// Oracle hierarchy.go:168-181: an entry, or anything beneath one. Matching
/// the subtree is what makes the exemption mean the same thing the freeze does.
pub fn allowlisted(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let Some(relative) = relative.to_str() else {
        return false;
    };
    LIVENESS_ALLOWLIST.iter().any(|entry| {
        relative == *entry
            || relative
                .strip_prefix(entry)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Oracle hierarchy.go:240-254: the bounded offender sample.
pub const AUDIT_PATH_SAMPLE: usize = 8;

pub(crate) fn sample_path(into: &mut Vec<String>, path: impl Into<String>) {
    if into.len() < AUDIT_PATH_SAMPLE {
        into.push(path.into());
    }
}

fn sample_relative(into: &mut Vec<String>, root: &Path, path: &Path) {
    if into.len() >= AUDIT_PATH_SAMPLE {
        return;
    }
    if let Ok(relative) = path.strip_prefix(root) {
        into.push(relative.display().to_string());
    }
}

/// Oracle hierarchy.go:184-238: what the frozen state looked like at resume.
#[derive(Debug, Default, Clone)]
pub struct AuditResult {
    pub applicable: bool,
    pub visited: i64,
    pub frozen: i64,
    pub escaped: i64,
    pub violations: i64,
    pub truncated: bool,
    pub violation_paths: Vec<String>,
    pub escaped_paths: Vec<String>,
}

/// Oracle hierarchy.go:321-404: classifies every cgroup's SETTLED freeze state
/// against what the sweep should have done. Must run before the thaw. Only a
/// hierarchy sweep claims to have frozen everything outside envd's chain, so
/// only one can be audited; anything else declines without guessing.
pub fn audit_frozen_state(
    manager: &dyn PathManager,
    proc_file: &Path,
    max_cgroups: usize,
    sweep: Option<FreezeMode>,
) -> Result<AuditResult, String> {
    let mut result = AuditResult::default();
    // Only a hierarchy sweep claims to have frozen everything outside envd's
    // chain (hierarchy.go:324-330).
    if sweep != Some(FreezeMode::Hierarchy) {
        return Ok(result);
    }
    let root = manager.root();
    let self_path = self_cgroup_path(proc_file, &root)
        .map_err(|error| format!("locate envd cgroup for the audit: {error}"))?;
    let exempt: HashSet<PathBuf> = descend_set(&root, &self_path).into_iter().collect();

    let mut queue = std::collections::VecDeque::new();
    queue.push_back(root.clone());
    while let Some(current) = queue.pop_front() {
        if result.visited >= max_cgroups as i64 {
            result.truncated = true;
            break;
        }
        result.visited += 1;
        // Enumerate BEFORE reading state, so an unreadable cgroup costs its
        // own classification and not its whole subtree (hierarchy.go:355-358).
        if let Ok(children) = manager.children_of(&current) {
            queue.extend(children);
        }
        // cgroup v2 creates neither cgroup.events nor cgroup.freeze on the
        // mount root, so there is nothing to classify there
        // (hierarchy.go:364-369).
        if current == root {
            continue;
        }
        let Ok(frozen) = manager.frozen_at(&current) else {
            // A cgroup that vanished mid-walk is a race, not a finding
            // (hierarchy.go:372-376).
            continue;
        };
        let protected = exempt.contains(&current) || allowlisted(&root, &current);
        match (frozen, protected) {
            (true, true) => {
                result.violations += 1;
                sample_relative(&mut result.violation_paths, &root, &current);
            }
            (true, false) => result.frozen += 1,
            (false, false) => {
                result.escaped += 1;
                sample_relative(&mut result.escaped_paths, &root, &current);
            }
            (false, true) => {}
        }
    }
    // Applicable once the mode guard is passed, unconditionally: a hierarchy
    // sweep whose writes all failed is the case with the most escapes
    // (hierarchy.go:393-401).
    result.applicable = true;
    Ok(result)
}

/// Oracle hierarchy.go:423-483: records which cgroups the GUEST had frozen
/// before our sweep ran, so the resume thaw can leave them alone. Ours are
/// never recorded, so they are always thawed. Errors are collected, never
/// fatal: a cgroup that cannot be read is simply not recorded, which means
/// the thaw will clear it — the safe direction.
pub fn scan_guest_frozen(
    manager: &dyn PathManager,
    proc_file: &Path,
    max_cgroups: usize,
) -> (HashSet<PathBuf>, bool, Vec<String>) {
    let mut errors = Vec::new();
    let root = manager.root();
    let Ok(self_path) = self_cgroup_path(proc_file, &root) else {
        // Without our own path we cannot tell ours from the guest's; record
        // nothing so the thaw behaves as it did before this scan existed
        // (hierarchy.go:428-431).
        errors.push("locate envd cgroup for the guest-freeze scan".to_owned());
        return (HashSet::new(), false, errors);
    };
    let ours: HashSet<PathBuf> = descend_set(&root, &self_path).into_iter().collect();
    let mut found = HashSet::new();
    let mut visited = 0usize;
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(root.clone());
    while let Some(current) = queue.pop_front() {
        if visited >= max_cgroups {
            // Truncated: the rest of the tree is unclassified, so the thaw
            // will clear it — degrades in the safe direction
            // (hierarchy.go:446-451).
            return (found, true, errors);
        }
        visited += 1;
        if !ours.contains(&current) && !allowlisted(&root, &current) {
            match manager.freeze_requested_at(&current) {
                Err(error) if super::vanished(&error) => {}
                Err(error) => errors.push(format!(
                    "read freeze state of {}: {error}",
                    current.display()
                )),
                Ok(true) => {
                    found.insert(current.clone());
                }
                Ok(false) => {}
            }
        }
        match manager.children_of(&current) {
            Err(error) => {
                if !super::vanished(&error) {
                    errors.push(format!("list children of {}: {error}", current.display()));
                }
            }
            Ok(children) => queue.extend(children),
        }
    }
    (found, false, errors)
}
