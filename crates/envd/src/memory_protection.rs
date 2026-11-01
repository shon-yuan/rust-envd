//! Read-only cgroup memory protection report carried by the official init header.
use serde::Serialize;
use std::path::{Component, Path};

#[derive(Default, Serialize)]
pub struct MemoryProtection {
    pub request: u64,
    pub low: u64,
    pub floor: u64,
    pub partial: bool,
}

pub fn read_memory_protection(membership: &Path, root: &Path) -> MemoryProtection {
    let partial = || MemoryProtection {
        partial: true,
        ..Default::default()
    };
    let Ok(contents) = std::fs::read_to_string(membership) else {
        return partial();
    };
    let Some(path) = contents.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        (fields.next()? == "0").then_some(())?;
        fields.next()?;
        fields.next()
    }) else {
        return partial();
    };
    // Go memory.go:49-53 JOINS the membership path onto the root
    // (filepath.Join), so an absolute membership path is anchored under the
    // root rather than replacing it; dropping RootDir while walking the
    // components is that join. Only a path whose Clean climbs OUT of the root
    // (a ParentDir popping past the top, the ".." prefix Go's Rel reports at
    // memory.go:55-58) is a read failure.
    let mut relative_components = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => relative_components.push(part),
            Component::ParentDir => {
                if relative_components.pop().is_none() {
                    return partial();
                }
            }
            Component::Prefix(_) => return partial(),
        }
    }
    if relative_components.is_empty() {
        return MemoryProtection::default();
    }
    let mut report = MemoryProtection {
        floor: u64::MAX,
        ..Default::default()
    };
    let mut level = root.to_owned();
    for component in &relative_components {
        level.push(component);
        let value = read_value(&level.join("memory.min"), &mut report.partial);
        report.floor = report.floor.min(value);
        report.request = value;
    }
    report.low = read_value(&level.join("memory.low"), &mut report.partial);
    report
}

fn read_value(path: &Path, partial: &mut bool) -> u64 {
    let value = std::fs::read_to_string(path).ok().and_then(|value| {
        if value.trim() == "max" {
            Some(i64::MAX as u64)
        } else {
            value.trim().parse().ok()
        }
    });
    match value {
        Some(value) => value,
        None => {
            *partial = true;
            0
        }
    }
}
