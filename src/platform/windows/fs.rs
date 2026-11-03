//! Win32 filesystem metadata, mutation, and watcher skeleton; P1.

use std::io;
use std::path::Path;

use crate::platform::{FsBackend, WatchId, WatcherBackend};
use crate::proto::filesystem::{EntryInfo, FilesystemEvent};

#[derive(Debug, Default)]
pub struct WindowsFsBackend;

impl FsBackend for WindowsFsBackend {
    fn stat(&self, _path: &Path) -> io::Result<EntryInfo> {
        Err(super::unsupported("filesystem stat"))
    }

    fn read_dir(&self, _path: &Path) -> io::Result<Vec<EntryInfo>> {
        Err(super::unsupported("filesystem list"))
    }

    fn create_dir(&self, _path: &Path) -> io::Result<()> {
        Err(super::unsupported("filesystem make directory"))
    }

    fn remove(&self, _path: &Path) -> io::Result<()> {
        Err(super::unsupported("filesystem remove"))
    }

    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(super::unsupported("filesystem move"))
    }
}

#[derive(Debug, Default)]
pub struct WindowsWatcherBackend;

impl WatcherBackend for WindowsWatcherBackend {
    fn watch(&self, _path: &Path) -> io::Result<WatchId> {
        Err(super::unsupported("filesystem watcher"))
    }

    fn unwatch(&self, _id: WatchId) -> io::Result<()> {
        Err(super::unsupported("filesystem watcher removal"))
    }

    fn events(&self, _id: WatchId) -> io::Result<Vec<FilesystemEvent>> {
        Err(super::unsupported("filesystem watcher events"))
    }
}
