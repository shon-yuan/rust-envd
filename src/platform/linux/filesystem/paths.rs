//! Go filepath.EvalSymlinks uses 255 links rather than Linux stat's 40-link cap.
use crate::exec_context::Account;
use nix::unistd::{chown, Gid, Uid};
use std::collections::VecDeque;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};

pub(super) fn follow_symlinks(path: &Path) -> io::Result<PathBuf> {
    let mut pending: VecDeque<_> = path
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "/" {
            resolved = PathBuf::from("/");
        } else if part == "." {
            continue;
        } else if part == ".." {
            resolved.pop();
        } else {
            resolved.push(&part);
            let info = fs::symlink_metadata(&resolved)?;
            if info.file_type().is_symlink() {
                links += 1;
                if links > 255 {
                    return Err(io::Error::from_raw_os_error(nix::libc::ELOOP));
                }
                let target = fs::read_link(&resolved)?;
                resolved.pop();
                for component in target.components().rev() {
                    pending.push_front(component.as_os_str().to_owned());
                }
            } else if !pending.is_empty() && !info.is_dir() {
                return Err(io::Error::from_raw_os_error(nix::libc::ENOTDIR));
            }
        }
    }
    Ok(resolved)
}

pub(crate) fn ensure_dirs(path: &Path, account: &Account) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if component == Component::RootDir {
            continue;
        }
        match fs::metadata(&current) {
            Ok(info) if info.is_dir() => {}
            Ok(_) => return Err(io::Error::other("path is a file")),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                match DirBuilder::new().mode(0o755).create(&current) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(err),
                }
                // Like Go EnsureDirs, also chown a concurrent creator's directory.
                chown(
                    &current,
                    Some(Uid::from_raw(account.uid)),
                    Some(Gid::from_raw(account.gid)),
                )
                .map_err(io::Error::from)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
