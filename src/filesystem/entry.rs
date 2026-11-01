use super::{internal, metadata, paths};
use crate::proto::filesystem::{EntryInfo, FileType};
use buffa_types::google::protobuf::Timestamp;
use connectrpc::ConnectError;
use nix::unistd::{Gid, Group, Uid, User};
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

pub(super) fn entry_info(path: &Path) -> Result<EntryInfo, ConnectError> {
    let raw = fs::symlink_metadata(path).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            ConnectError::not_found(format!(
                "file not found: lstat {}: no such file or directory",
                path.display()
            ))
        } else {
            internal("error getting file info", err)
        }
    })?;
    let is_symlink = raw.file_type().is_symlink();
    let resolved = is_symlink.then(|| paths::follow_symlinks(path));
    let target_path = resolved
        .as_ref()
        .map(|result| result.as_deref().unwrap_or(path));
    let target = target_path.and_then(|path| fs::metadata(path).ok());
    let effective = if is_symlink {
        target.as_ref()
    } else {
        Some(&raw)
    };
    let owner = User::from_uid(Uid::from_raw(raw.uid()))
        .ok()
        .flatten()
        .map(|u| u.name)
        .unwrap_or_else(|| raw.uid().to_string());
    let group = Group::from_gid(Gid::from_raw(raw.gid()))
        .ok()
        .flatten()
        .map(|g| g.name)
        .unwrap_or_else(|| raw.gid().to_string());
    // Read failure is intentionally best effort in the shared Go entry helper.
    let metadata = metadata::read_metadata(path)
        .unwrap_or_default()
        .into_iter()
        .collect();
    Ok(EntryInfo {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into()),
        path: path.to_string_lossy().into_owned(),
        r#type: effective
            .map(file_type)
            .unwrap_or(FileType::Unspecified)
            .into(),
        size: raw.size() as i64,
        mode: effective.map_or(0, |m| m.mode() & 0o777),
        permissions: permissions(&raw),
        owner,
        group,
        modified_time: Timestamp {
            seconds: raw.mtime(),
            nanos: raw.mtime_nsec() as i32,
            ..Default::default()
        }
        .into(),
        symlink_target: target_path.map(|p| p.to_string_lossy().into_owned()),
        metadata,
        is_symlink,
        ..Default::default()
    })
}

fn file_type(info: &Metadata) -> FileType {
    if info.is_file() {
        FileType::File
    } else if info.is_dir() {
        FileType::Directory
    } else if info.file_type().is_symlink() {
        FileType::Symlink
    } else {
        FileType::Unspecified
    }
}

// Go FileMode.String uses L for symlinks and prepends u/g/t for special bits,
// rather than the conventional ls-style s/t substitution in the permission triplets.
fn permissions(info: &Metadata) -> String {
    let kind = info.file_type();
    let mut result = String::new();
    if kind.is_dir() {
        result.push('d');
    }
    if kind.is_symlink() {
        result.push('L');
    }
    if kind.is_char_device() {
        result.push_str("Dc");
    } else if kind.is_block_device() {
        result.push('D');
    }
    if kind.is_fifo() {
        result.push('p');
    }
    if kind.is_socket() {
        result.push('S');
    }
    for (mask, flag) in [(0o4000, 'u'), (0o2000, 'g'), (0o1000, 't')] {
        if info.mode() & mask != 0 {
            result.push(flag);
        }
    }
    if result.is_empty() {
        result.push('-');
    }
    for (mask, flag) in [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ] {
        result.push(if info.mode() & mask != 0 { flag } else { '-' });
    }
    result
}
