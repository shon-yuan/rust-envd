//! Linux backend using the vetted inotify crate, without debounce/rename pairing.
use super::is_network_mount_type;
use crate::{
    exec_context::{Account, Defaults},
    filesystem::FilesystemRuntime,
    proto::filesystem as pb,
};
use connectrpc::ConnectError;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use nix::sys::statfs::statfs;
use std::{
    collections::{HashMap, VecDeque},
    fs, io,
    path::{Path, PathBuf},
};
use tokio::io::unix::AsyncFd;

#[derive(Clone)]
pub(super) struct Config {
    pub path: PathBuf,
    pub recursive: bool,
    pub include_entry: bool,
}
pub(super) struct Backend {
    pub config: Config,
    fd: AsyncFd<Inotify>,
    watches: HashMap<WatchDescriptor, PathBuf>,
    cookies: VecDeque<(u32, PathBuf)>,
    pending: VecDeque<pb::FilesystemEvent>,
    runtime: FilesystemRuntime,
}
struct Raw {
    wd: WatchDescriptor,
    mask: EventMask,
    cookie: u32,
    name: Option<std::ffi::OsString>,
}
impl Backend {
    pub fn new(
        user: &Account,
        defaults: &Defaults,
        path: &str,
        recursive: bool,
        include_entry: bool,
        allow_network: bool,
    ) -> Result<Self, ConnectError> {
        let path = PathBuf::from(
            defaults
                .resolve_path(path, user)
                .map_err(|e| ConnectError::invalid_argument(e.to_string()))?,
        );
        let info = fs::metadata(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                ConnectError::not_found(format!(
                    "path {} not found: stat {}: no such file or directory",
                    path.display(),
                    path.display()
                ))
            } else {
                failure(e)
            }
        })?;
        if !info.is_dir() {
            return Err(ConnectError::invalid_argument("path is not a directory"));
        }
        if !allow_network
            && is_network_mount_type(statfs(&path).map_err(failure)?.filesystem_type().0)
        {
            return Err(ConnectError::invalid_argument(
                "cannot watch path on network filesystem",
            ));
        }
        let inotify = Inotify::init().map_err(failure)?;
        // SAFETY: the private backend retains Inotify until registration drops.
        // Adding/removing watches and reading events never replaces its fd.
        let fd = unsafe { AsyncFd::register(inotify) }.map_err(failure)?;
        let mut backend = Self {
            config: Config {
                path: path.clone(),
                recursive,
                include_entry,
            },
            fd,
            watches: HashMap::new(),
            cookies: VecDeque::new(),
            pending: VecDeque::new(),
            runtime: FilesystemRuntime::new(user.clone(), defaults.clone()),
        };
        backend.add_directory(&path)?;
        if recursive {
            backend.add_children(&path, false)?;
        }
        Ok(backend)
    }
    fn add_directory(&mut self, path: &Path) -> Result<(), ConnectError> {
        let mask = WatchMask::CREATE
            | WatchMask::MODIFY
            | WatchMask::ATTRIB
            | WatchMask::DELETE
            | WatchMask::DELETE_SELF
            | WatchMask::MOVED_FROM
            | WatchMask::MOVED_TO
            | WatchMask::MOVE_SELF;
        let wd = self
            .fd
            .get_mut()
            .watches()
            .add(path, mask)
            .map_err(failure)?;
        self.watches.entry(wd).or_insert_with(|| path.to_path_buf());
        Ok(())
    }
    fn add_children(&mut self, path: &Path, synthetic: bool) -> Result<(), ConnectError> {
        let children = fs::read_dir(path).map_err(failure)?;
        let mut children = children.collect::<io::Result<Vec<_>>>().map_err(failure)?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            if synthetic {
                self.emit(&path, EventMask::CREATE)?;
            }
            let info = match child.file_type() {
                Ok(info) => info,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(failure(e)),
            };
            if info.is_dir() {
                self.add_directory(&path)?;
                self.add_children(&path, synthetic)?;
            }
        }
        Ok(())
    }
    pub async fn next(&mut self) -> Result<pb::FilesystemEvent, ConnectError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            let mut buffer = [0u8; 65536];
            let raw = {
                let mut ready = self.fd.readable_mut().await.map_err(failure)?;
                let read = ready.try_io(|fd| {
                    fd.get_mut().read_events(&mut buffer).map(|events| {
                        events
                            .map(|event| Raw {
                                wd: event.wd.clone(),
                                mask: event.mask,
                                cookie: event.cookie,
                                name: event.name.map(|n| n.to_owned()),
                            })
                            .collect::<Vec<_>>()
                    })
                });
                match read {
                    Ok(result) => result.map_err(failure)?,
                    Err(_) => continue,
                }
            };
            if raw.is_empty() {
                return Err(ConnectError::internal("watcher event channel closed"));
            }
            for event in raw {
                self.handle(event)?;
            }
        }
    }
    fn handle(&mut self, event: Raw) -> Result<(), ConnectError> {
        let mask = event.mask;
        if mask.contains(EventMask::Q_OVERFLOW) {
            return Err(ConnectError::internal("watcher event queue overflow"));
        }
        let Some(base) = self.watches.get(&event.wd).cloned() else {
            return Ok(());
        };
        let path = event.name.map_or(base.clone(), |name| base.join(name));
        if mask.intersects(EventMask::IGNORED | EventMask::UNMOUNT) {
            self.watches.remove(&event.wd);
            return Ok(());
        }
        if mask.contains(EventMask::DELETE_SELF) {
            self.watches.remove(&event.wd);
            if self
                .watches
                .values()
                .any(|p| Some(p.as_path()) == path.parent())
            {
                return Ok(());
            }
        }
        if mask.contains(EventMask::MOVE_SELF) {
            if self.config.recursive && base != self.config.path {
                return Ok(());
            }
            let doomed: Vec<_> = self
                .watches
                .iter()
                .filter(|(_, p)| *p == &base || (self.config.recursive && p.starts_with(&base)))
                .map(|(wd, _)| wd.clone())
                .collect();
            for wd in doomed {
                self.watches.remove(&wd);
                self.fd.get_mut().watches().remove(wd).map_err(failure)?;
            }
        }
        if mask.contains(EventMask::MOVED_FROM) && event.cookie != 0 {
            if self.cookies.len() == 10 {
                self.cookies.pop_front();
            }
            self.cookies.push_back((event.cookie, path.clone()));
        }
        let moved_from = if mask.contains(EventMask::MOVED_TO) && event.cookie != 0 {
            self.cookies
                .iter()
                .find(|(cookie, _)| *cookie == event.cookie)
                .map(|(_, path)| path.clone())
        } else {
            None
        };
        if self.config.recursive
            && mask.contains(EventMask::ISDIR)
            && mask.intersects(EventMask::CREATE | EventMask::MOVED_TO)
        {
            if let Some(old) = moved_from {
                for watched in self.watches.values_mut() {
                    if let Ok(relative) = watched.strip_prefix(&old) {
                        *watched = path.join(relative);
                    }
                }
                self.add_directory(&path)?;
                self.emit(&path, mask)?;
            } else {
                self.add_directory(&path)?;
                self.emit(&path, mask)?;
                self.add_children(&path, true)?;
            }
        } else {
            self.emit(&path, mask)?;
        }
        Ok(())
    }
    fn emit(&mut self, path: &Path, mask: EventMask) -> Result<(), ConnectError> {
        let name = path
            .strip_prefix(&self.config.path)
            .map_err(failure)?
            .to_string_lossy()
            .into_owned();
        let name = if name.is_empty() { ".".into() } else { name };
        for (flag, kind, carries) in [
            (
                EventMask::CREATE | EventMask::MOVED_TO,
                pb::EventType::Create,
                true,
            ),
            (
                EventMask::MOVED_FROM | EventMask::MOVE_SELF,
                pb::EventType::Rename,
                false,
            ),
            (EventMask::ATTRIB, pb::EventType::Chmod, true),
            (EventMask::MODIFY, pb::EventType::Write, true),
            (
                EventMask::DELETE | EventMask::DELETE_SELF,
                pb::EventType::Remove,
                false,
            ),
        ] {
            if mask.intersects(flag) {
                let entry = if self.config.include_entry && carries {
                    self.runtime.stat(&path.to_string_lossy()).ok()
                } else {
                    None
                };
                self.pending.push_back(pb::FilesystemEvent {
                    name: name.clone(),
                    r#type: kind.into(),
                    entry: entry.into(),
                    ..Default::default()
                });
            }
        }
        Ok(())
    }
}
fn failure(error: impl std::fmt::Display) -> ConnectError {
    ConnectError::internal(format!("watcher error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::{getegid, geteuid};

    fn backend() -> (tempfile::TempDir, Backend) {
        let dir = tempfile::tempdir().unwrap();
        let user = Account {
            name: "test".into(),
            home: dir.path().to_str().unwrap().into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: vec![],
        };
        let watcher = Backend::new(
            &user,
            &Defaults::default(),
            dir.path().to_str().unwrap(),
            false,
            true,
            false,
        )
        .unwrap();
        (dir, watcher)
    }
    #[tokio::test]
    async fn unreadable_recursive_walk_propagates_for_initial_and_dynamic_scan() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, mut watcher) = backend();
        let path = dir.path().join("unreadable");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o111)).unwrap();
        let initial = watcher.add_children(&path, false);
        let dynamic = watcher.add_children(&path, true);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(initial.unwrap_err().code, connectrpc::ErrorCode::Internal);
        assert_eq!(dynamic.unwrap_err().code, connectrpc::ErrorCode::Internal);
    }

    #[tokio::test]
    async fn multiple_flags_follow_go_order_and_remove_rename_never_attach_entry() {
        let (dir, mut watcher) = backend();
        let path = dir.path().join("file");
        fs::write(&path, b"x").unwrap();
        watcher
            .emit(
                &path,
                EventMask::CREATE
                    | EventMask::MOVED_FROM
                    | EventMask::ATTRIB
                    | EventMask::MODIFY
                    | EventMask::DELETE,
            )
            .unwrap();
        let events: Vec<_> = watcher.pending.drain(..).collect();
        assert_eq!(
            events.iter().map(|e| e.r#type.to_i32()).collect::<Vec<_>>(),
            [1, 4, 5, 2, 3]
        );
        assert!(events[0].entry.is_set() && events[2].entry.is_set() && events[3].entry.is_set());
        assert!(!events[1].entry.is_set() && !events[4].entry.is_set());
    }
    #[tokio::test]
    async fn overflow_is_terminal_internal_and_ignored_does_not_create_empty_event() {
        let (_dir, mut watcher) = backend();
        let wd = watcher.watches.keys().next().unwrap().clone();
        assert_eq!(
            watcher
                .handle(Raw {
                    wd: wd.clone(),
                    mask: EventMask::Q_OVERFLOW,
                    cookie: 0,
                    name: None
                })
                .unwrap_err()
                .code,
            connectrpc::ErrorCode::Internal
        );
        watcher
            .handle(Raw {
                wd,
                mask: EventMask::IGNORED,
                cookie: 0,
                name: None,
            })
            .unwrap();
        assert!(watcher.pending.is_empty());
        assert!(watcher.watches.is_empty());
    }
}
