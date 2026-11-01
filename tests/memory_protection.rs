use coorworker_envd::memory_protection::read_memory_protection;
use serde_json::json;
use std::fs;

#[test]
fn reads_the_entire_cgroup_chain_and_max_is_unbounded() {
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path().join("cgroup");
    let leaf = root.join("system.slice/envd.service");
    fs::create_dir_all(&leaf).unwrap();
    fs::write(root.join("system.slice/memory.min"), "4096\n").unwrap();
    fs::write(leaf.join("memory.min"), "max\n").unwrap();
    fs::write(leaf.join("memory.low"), "8192\n").unwrap();
    let membership = tree.path().join("membership");
    fs::write(&membership, "0::/system.slice/envd.service\n").unwrap();
    let report = serde_json::to_value(read_memory_protection(&membership, &root)).unwrap();
    assert_eq!(
        report,
        json!({"request":i64::MAX,"low":8192,"floor":4096,"partial":false})
    );
    fs::remove_file(root.join("system.slice/memory.min")).unwrap();
    let report = serde_json::to_value(read_memory_protection(&membership, &root)).unwrap();
    assert_eq!(report["partial"], true);
    assert_eq!(report["floor"], 0);
}

#[test]
fn numeric_u64_protection_is_not_clamped_to_the_max_keyword_value() {
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path().join("cgroup");
    let leaf = root.join("envd");
    fs::create_dir_all(&leaf).unwrap();
    fs::write(leaf.join("memory.min"), u64::MAX.to_string()).unwrap();
    fs::write(leaf.join("memory.low"), "0").unwrap();
    let membership = tree.path().join("membership");
    fs::write(&membership, "0::/envd\n").unwrap();
    let report = read_memory_protection(&membership, &root);
    assert_eq!(report.request, u64::MAX);
    assert_eq!(report.floor, u64::MAX);
    assert!(!report.partial);
}

#[test]
fn absolute_membership_is_anchored_under_the_root_like_gos_filepath_join() {
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path().join("cgroup");
    fs::create_dir_all(root.join("other/place")).unwrap();
    fs::write(root.join("other/memory.min"), "8192\n").unwrap();
    fs::write(root.join("other/place/memory.min"), "4096\n").unwrap();
    fs::write(root.join("other/place/memory.low"), "8192\n").unwrap();
    let membership = tree.path().join("membership");
    // Go memory.go:49-53: SelfCgroupPath JOINS the membership path onto the
    // root (filepath.Join never replaces), so an absolute membership path is
    // anchored under the root and read there; only a path whose Clean climbs
    // OUT of it (memory.go:55-58) reports Partial.
    fs::write(&membership, "0::/other/place\n").unwrap();
    let report = serde_json::to_value(read_memory_protection(&membership, &root)).unwrap();
    assert_eq!(
        report,
        json!({"request":4096,"low":8192,"floor":4096,"partial":false})
    );
}

#[test]
fn root_is_unprotected_and_missing_or_escaping_membership_is_partial() {
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path().join("cgroup");
    fs::create_dir(&root).unwrap();
    let membership = tree.path().join("membership");
    for contents in ["0::/\n", "1:cpu:/wrong\n0::/\n"] {
        fs::write(&membership, contents).unwrap();
        assert_eq!(
            serde_json::to_value(read_memory_protection(&membership, &root)).unwrap(),
            json!({"request":0,"low":0,"floor":0,"partial":false})
        );
    }
    for contents in ["", "0::/../../outside\n", "1:cpu:/wrong\n"] {
        fs::write(&membership, contents).unwrap();
        assert!(read_memory_protection(&membership, &root).partial);
    }
}
