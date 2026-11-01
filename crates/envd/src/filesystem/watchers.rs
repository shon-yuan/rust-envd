//! Real Linux watchers matching the pinned Go event ordering and drain semantics.
//! Creation can block on filesystem/NSS work; adapters must create on blocking workers.
#[path = "watchers_inotify.rs"]
mod backend;

use crate::{
    exec_context::{Account, Defaults},
    proto::filesystem as pb,
};
use connectrpc::ConnectError;
use std::pin::Pin;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, Sleep},
};

#[derive(Clone, Default)]
pub struct WatcherRegistry {
    inner: Arc<Mutex<HashMap<String, Persistent>>>,
}
struct Buffer {
    events: Vec<pb::FilesystemEvent>,
    error: Option<ConnectError>,
}
struct Persistent {
    config: backend::Config,
    buffer: Arc<Mutex<Buffer>>,
    task: JoinHandle<()>,
    cancel: Option<oneshot::Sender<()>>,
}
impl Drop for Persistent {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        self.task.abort();
    }
}
#[derive(Clone, Debug)]
pub struct WatcherSnapshot {
    pub id: String,
    pub watch_path: PathBuf,
    pub recursive: bool,
    pub include_entry: bool,
    pub events: Vec<pb::FilesystemEvent>,
    pub error: Option<ConnectError>,
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shared body of create and import: build the backend, seed its buffer,
    /// spawn its reader task and register it under `id`.
    // The argument list is Go's CreateFileWatcher input plus the two
    // handover-only fields (id, pending) — the create and import paths differ
    // in exactly those.
    #[allow(clippy::too_many_arguments)]
    fn spawn_persistent(
        &self,
        user: &Account,
        defaults: &Defaults,
        path: &str,
        recursive: bool,
        include_entry: bool,
        allow_network_mounts: bool,
        id: String,
        pending: Vec<pb::FilesystemEvent>,
    ) -> Result<(), ConnectError> {
        let mut watcher = backend::Backend::new(
            user,
            defaults,
            path,
            recursive,
            include_entry,
            allow_network_mounts,
        )?;
        let config = watcher.config.clone();
        let buffer = Arc::new(Mutex::new(Buffer {
            events: pending,
            error: None,
        }));
        let sink = buffer.clone();
        let (cancel, mut canceled) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _=&mut canceled=>break,
                    event=watcher.next()=> {
                        let Ok(mut state)=sink.lock() else {break;};
                        match event {Ok(event)=>state.events.push(event),Err(error)=>{state.error=Some(error);break;}}
                    }
                }
            }
        });
        self.inner.lock().map_err(|_| poisoned())?.insert(
            id,
            Persistent {
                config,
                buffer,
                task,
                cancel: Some(cancel),
            },
        );
        Ok(())
    }

    pub fn create_watcher(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::CreateWatcherRequest,
    ) -> Result<pb::CreateWatcherResponse, ConnectError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = format!(
            "w{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        self.spawn_persistent(
            user,
            defaults,
            &request.path,
            request.recursive,
            request.include_entry,
            request.allow_network_mounts,
            id.clone(),
            Vec::new(),
        )?;
        Ok(pb::CreateWatcherResponse {
            watcher_id: id,
            ..Default::default()
        })
    }
    pub fn get_watcher_events(
        &self,
        id: &str,
    ) -> Result<pb::GetWatcherEventsResponse, ConnectError> {
        let registry = self.inner.lock().map_err(|_| poisoned())?;
        let watcher = registry.get(id).ok_or_else(|| missing(id))?;
        let mut buffer = watcher.buffer.lock().map_err(|_| poisoned())?;
        if let Some(error) = &buffer.error {
            return Err(error.clone());
        }
        Ok(pb::GetWatcherEventsResponse {
            events: std::mem::take(&mut buffer.events),
            ..Default::default()
        })
    }
    pub fn remove_watcher(&self, id: &str) -> Result<pb::RemoveWatcherResponse, ConnectError> {
        self.inner
            .lock()
            .map_err(|_| poisoned())?
            .remove(id)
            .ok_or_else(|| missing(id))?;
        Ok(pb::RemoveWatcherResponse::default())
    }
    /// Oracle watch_handover.go ExportWatchersHold: snapshot the active
    /// watchers under the registry lock and KEEP the lock, so a concurrent
    /// GetWatcherEvents cannot drain buffered events the snapshot already
    /// carried (the re-armed watcher would re-deliver them — double delivery).
    /// A successful execve replaces this process and the held lock vanishes
    /// with it; dropping the guard on a failure path restores serving.
    pub fn export_watchers_hold(
        &self,
    ) -> Result<
        (
            Vec<crate::proto::upgrade::HandoverWatcher>,
            WatchersHold<'_>,
        ),
        ConnectError,
    > {
        let registry = self.inner.lock().map_err(|_| poisoned())?;
        let mut out = Vec::new();
        for (id, watcher) in registry.iter() {
            let buffer = watcher.buffer.lock().map_err(|_| poisoned())?;
            out.push(crate::proto::upgrade::HandoverWatcher {
                id: id.clone(),
                path: watcher.config.path.to_string_lossy().into_owned(),
                recursive: watcher.config.recursive,
                include_entry_info: watcher.config.include_entry,
                pending_events: buffer.events.clone(),
                ..Default::default()
            });
        }
        Ok((out, WatchersHold { _guard: registry }))
    }

    /// Oracle watch_handover.go ImportWatchers: re-arm watchers carried across
    /// a live-upgrade — a fresh backend per carried watcher under the PRESERVED
    /// id (GetWatcherEvents keeps working without a client change), pending
    /// events re-queued ahead of anything the fresh backend produced. The
    /// resume freeze makes this lossless: nothing mutates the filesystem
    /// during the handover window. Failures are counted, not fatal.
    /// `allow_network_mounts` defaults to true, Go's CreateFileWatcher default.
    pub fn import_watchers(
        &self,
        user: &Account,
        defaults: &Defaults,
        watchers: &[crate::proto::upgrade::HandoverWatcher],
    ) -> (usize, usize) {
        let mut rearmed = 0;
        let mut failed = 0;
        for carried in watchers {
            let pending = carried.pending_events.clone();
            let seeded = self.spawn_persistent(
                user,
                defaults,
                &carried.path,
                carried.recursive,
                carried.include_entry_info,
                true,
                carried.id.clone(),
                pending,
            );
            match seeded {
                // Don't log carried.path — it can be a customer code/data
                // location; the opaque watcher id suffices.
                Err(_) => failed += 1,
                Ok(()) => rearmed += 1,
            }
        }
        (rearmed, failed)
    }

    /// Consistent, nondestructive state export.
    pub fn snapshots(&self) -> Result<Vec<WatcherSnapshot>, ConnectError> {
        let registry = self.inner.lock().map_err(|_| poisoned())?;
        registry
            .iter()
            .map(|(id, watcher)| {
                let buffer = watcher.buffer.lock().map_err(|_| poisoned())?;
                Ok(WatcherSnapshot {
                    id: id.clone(),
                    watch_path: watcher.config.path.clone(),
                    recursive: watcher.config.recursive,
                    include_entry: watcher.config.include_entry,
                    events: buffer.events.clone(),
                    error: buffer.error.clone(),
                })
            })
            .collect()
    }
    pub fn watch_dir(
        &self,
        user: &Account,
        defaults: &Defaults,
        request: pb::WatchDirRequest,
        keepalive: Option<&str>,
    ) -> Result<WatchSubscription, ConnectError> {
        let mut backend = backend::Backend::new(
            user,
            defaults,
            &request.path,
            request.recursive,
            request.include_entry,
            request.allow_network_mounts,
        )?;
        let interval = keepalive_interval(keepalive);
        // Go has an independent inotify reader ahead of stream.Send. A single
        // queued event permits rearm while idle without an unbounded stream queue.
        let (sender, receiver) = mpsc::channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let signal = closed.clone();
        let task = tokio::spawn(async move {
            loop {
                let event = backend.next().await;
                let terminal = event.is_err();
                if sender.send(event).await.is_err() || terminal {
                    break;
                }
            }
            signal.store(true, Ordering::SeqCst);
        });
        Ok(WatchSubscription {
            receiver,
            task,
            first: true,
            interval,
            timer: Box::pin(tokio::time::sleep(interval)),
            closed,
            ended: false,
        })
    }
}
fn missing(id: &str) -> ConnectError {
    ConnectError::not_found(format!("watcher with id {id} not found"))
}
fn poisoned() -> ConnectError {
    ConnectError::internal("watcher state lock poisoned")
}

/// The registry lock held from an ExportWatchersHold snapshot through the
/// execve (watch_handover.go:49-57's release func, made structural). Dropping
/// it releases the registry, so a failed upgrade resumes serving.
pub struct WatchersHold<'a> {
    _guard: MutexGuard<'a, HashMap<String, Persistent>>,
}

pub struct WatchSubscription {
    receiver: mpsc::Receiver<Result<pb::FilesystemEvent, ConnectError>>,
    task: JoinHandle<()>,
    first: bool,
    interval: Duration,
    timer: Pin<Box<Sleep>>,
    closed: Arc<AtomicBool>,
    ended: bool,
}
impl Drop for WatchSubscription {
    fn drop(&mut self) {
        self.task.abort();
        self.closed.store(true, Ordering::SeqCst);
    }
}
impl WatchSubscription {
    pub fn closed_signal(&self) -> Arc<AtomicBool> {
        self.closed.clone()
    }
    pub async fn recv(&mut self) -> Option<Result<pb::WatchDirResponse, ConnectError>> {
        use pb::watch_dir_response::Event;
        if self.ended {
            return None;
        }
        if self.first {
            self.first = false;
            return Some(Ok(pb::WatchDirResponse {
                event: Some(Event::Start(Box::default())),
                ..Default::default()
            }));
        }
        let event = tokio::select! {
            event=self.receiver.recv()=>match event {
                Some(Ok(event))=> {self.timer.as_mut().reset(Instant::now()+self.interval);Event::Filesystem(Box::new(event))},
                Some(Err(error))=> {self.ended=true;return Some(Err(error));}
                None=> {self.ended=true;return Some(Err(ConnectError::internal("watcher event channel closed")));}
            },
            _=&mut self.timer=> {self.timer.as_mut().reset(Instant::now()+self.interval);Event::Keepalive(Box::default())}
        };
        Some(Ok(pb::WatchDirResponse {
            event: Some(event),
            ..Default::default()
        }))
    }
}
pub fn keepalive_interval(header: Option<&str>) -> Duration {
    header
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0 && *n <= i64::MAX / 1_000_000_000)
        .map(|n| Duration::from_secs(n as u64))
        .unwrap_or(Duration::from_secs(90))
}
pub fn is_network_mount_type(kind: impl Into<i128>) -> bool {
    // Linux statfs uses signed f_type on glibc and unsigned f_type on musl.
    // Both convert losslessly to i128, preserving every network magic value.
    matches!(
        kind.into(),
        0x6969 | 0xff534d42 | 0x517b | 0xfe534d42 | 0x65735546
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::{getegid, geteuid};

    #[tokio::test]
    async fn snapshots_preserve_events_and_terminal_error_precedes_drain() {
        let dir = tempfile::tempdir().unwrap();
        let user = Account {
            name: "test".into(),
            home: dir.path().to_str().unwrap().into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: vec![],
        };
        let registry = WatcherRegistry::new();
        let id = registry
            .create_watcher(
                &user,
                &Defaults::default(),
                pb::CreateWatcherRequest {
                    path: dir.path().to_str().unwrap().into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .watcher_id;
        {
            let guard = registry.inner.lock().unwrap();
            let watcher = guard.get(&id).unwrap();
            watcher
                .buffer
                .lock()
                .unwrap()
                .events
                .push(pb::FilesystemEvent {
                    name: "queued".into(),
                    ..Default::default()
                });
        }
        assert_eq!(registry.snapshots().unwrap()[0].events[0].name, "queued");
        assert_eq!(
            registry.get_watcher_events(&id).unwrap().events[0].name,
            "queued"
        );
        assert!(registry.get_watcher_events(&id).unwrap().events.is_empty());
        {
            let guard = registry.inner.lock().unwrap();
            let watcher = guard.get(&id).unwrap();
            let mut buffer = watcher.buffer.lock().unwrap();
            buffer.events.push(pb::FilesystemEvent::default());
            buffer.error = Some(ConnectError::internal("terminal"));
        }
        assert_eq!(
            registry.get_watcher_events(&id).unwrap_err().code,
            connectrpc::ErrorCode::Internal
        );
        assert_eq!(registry.snapshots().unwrap()[0].events.len(), 1);
        registry.remove_watcher(&id).unwrap();
    }

    /// Oracle watch_handover.go ExportWatchersHold + ImportWatchers: the
    /// snapshot carries every active watcher with its buffered events (not
    /// drained — GetWatcherEvents still owns them), and a resume re-arms under
    /// the PRESERVED id with the pending events re-queued ahead of anything the
    /// fresh backend produces.
    #[tokio::test]
    async fn export_snapshot_and_import_rearm_carry_watchers_across_the_handover() {
        let dir = tempfile::tempdir().unwrap();
        let user = Account {
            name: "test".into(),
            home: dir.path().to_str().unwrap().into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: vec![],
        };
        let defaults = Defaults::default();
        let registry = WatcherRegistry::new();
        let id = registry
            .create_watcher(
                &user,
                &defaults,
                pb::CreateWatcherRequest {
                    path: dir.path().to_str().unwrap().into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .watcher_id;
        registry
            .inner
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .buffer
            .lock()
            .unwrap()
            .events
            .push(pb::FilesystemEvent {
                name: "carried".into(),
                ..Default::default()
            });

        let (snapshot, hold) = registry.export_watchers_hold().unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].id, id);
        assert_eq!(snapshot[0].path, dir.path().to_string_lossy().into_owned());
        assert_eq!(snapshot[0].pending_events[0].name, "carried");
        drop(hold);
        // The snapshot did not drain the live buffer (only the hold kept
        // GetWatcherEvents out).
        assert_eq!(
            registry.get_watcher_events(&id).unwrap().events[0].name,
            "carried"
        );

        // Re-arm into a fresh registry the way the incoming envd does.
        let resumed = WatcherRegistry::new();
        let (rearmed, failed) = resumed.import_watchers(&user, &defaults, &snapshot);
        assert_eq!((rearmed, failed), (1, 0));
        // The preserved id serves events, pending re-queued first.
        assert_eq!(
            resumed.get_watcher_events(&id).unwrap().events[0].name,
            "carried"
        );
    }

    /// Oracle watch_handover.go ImportWatchers: a carried watcher whose backend
    /// cannot be built is counted, not fatal — the surviving watchers still
    /// re-arm and the resume proceeds.
    #[tokio::test]
    async fn import_counts_a_watcher_that_cannot_rearm() {
        let dir = tempfile::tempdir().unwrap();
        let user = Account {
            name: "test".into(),
            home: dir.path().to_str().unwrap().into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: vec![],
        };
        let registry = WatcherRegistry::new();
        let (rearmed, failed) = registry.import_watchers(
            &user,
            &Defaults::default(),
            &[crate::proto::upgrade::HandoverWatcher {
                id: "w-dead".into(),
                path: "/nonexistent/no/such/dir".into(),
                ..Default::default()
            }],
        );
        assert_eq!((rearmed, failed), (0, 1));
        assert_eq!(
            registry.get_watcher_events("w-dead").unwrap_err().code,
            connectrpc::ErrorCode::NotFound
        );
    }
}
