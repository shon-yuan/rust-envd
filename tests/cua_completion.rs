//! Completion means the real blocking action finished, even when its driver
//! wrapper already returned a timeout. All workers are isolated in-memory fixtures.
#![cfg(all(feature = "cua-desktop", any(target_os = "linux", windows)))]

#[path = "../vendor/cua-driver/rust/crates/platform-linux/src/wayland/shell_helper/deadline.rs"]
mod compositor_deadline;

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};

use coorworker_envd::cua::{
    CallFuture, CuaError, CuaService, DesktopTools, ToolDescription, ToolOutput,
};
use cua_driver_core::work_scope::{spawn_blocking, InvocationScope};
use serde_json::{json, Value};
use tokio::sync::Notify;

#[derive(Default)]
struct Blocker {
    released: Mutex<bool>,
    wake: Condvar,
    entered: Notify,
}

impl Blocker {
    fn wait(&self) {
        let mut released = self.released.lock().unwrap();
        self.entered.notify_one();
        while !*released {
            released = self.wake.wait(released).unwrap();
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

// An assertion failure must still release the real thread before Tokio shuts down.
struct ReleaseOnDrop(Arc<Blocker>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[derive(Default)]
struct BoundedTools {
    blocker: Arc<Blocker>,
    first: AtomicBool,
    bounded_returned: Notify,
}

impl DesktopTools for BoundedTools {
    fn tools(&self) -> Vec<ToolDescription> {
        vec![ToolDescription {
            name: "bounded_action".into(),
            input_schema: json!({"type":"object","additionalProperties":false}),
            ..ToolDescription::default()
        }]
    }

    fn invoke<'a>(&'a self, _: &'a str, _: Value) -> CallFuture<'a> {
        Box::pin(async move {
            let scope = InvocationScope::new();
            scope
                .run(async {
                    if !self.first.swap(true, Ordering::SeqCst) {
                        let blocker = self.blocker.clone();
                        let worker = spawn_blocking(move || blocker.wait());
                        let result = tokio::time::timeout(Duration::from_secs(12), worker).await;
                        self.bounded_returned.notify_one();
                        assert!(
                            result.is_err(),
                            "fixture must exercise the detached timeout path"
                        );
                        ToolOutput {
                            success: false,
                            content: vec![json!({"type":"text","text":"bounded driver timeout"})],
                            data: Some(json!({"code":"tool_error"})),
                        }
                    } else {
                        ToolOutput {
                            success: true,
                            content: vec![],
                            data: None,
                        }
                    }
                })
                .await
        })
    }
}

async fn start() -> (
    Arc<BoundedTools>,
    Arc<CuaService>,
    tokio::task::JoinHandle<Result<ToolOutput, CuaError>>,
    ReleaseOnDrop,
) {
    let backend = Arc::new(BoundedTools::default());
    let cleanup = ReleaseOnDrop(backend.blocker.clone());
    let service = Arc::new(CuaService::new(backend.clone()));
    let caller = service.clone();
    let request =
        tokio::spawn(async move { caller.call("bounded_action".into(), json!({})).await });
    backend.blocker.entered.notified().await;
    tokio::time::advance(Duration::from_secs(13)).await;
    backend.bounded_returned.notified().await;
    (backend, service, request, cleanup)
}

async fn assert_busy(service: &Arc<CuaService>) {
    assert!(matches!(
        service.call("bounded_action".into(), json!({})).await,
        Err(CuaError::Busy)
    ));
}

async fn wait_for_release(service: &Arc<CuaService>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match service.call("bounded_action".into(), json!({})).await {
                Ok(output) => {
                    assert!(output.success);
                    break;
                }
                Err(CuaError::Busy) => tokio::task::yield_now().await,
                other => panic!("unexpected result after completion: {other:?}"),
            }
        }
    })
    .await
    .expect("blocking completion must release admission");
}

#[tokio::test(start_paused = true)]
async fn driver_timeout_cannot_release_service_slot_before_real_action_finishes() {
    let (backend, service, request, _cleanup) = start().await;
    assert!(
        !request.is_finished(),
        "driver timeout must drain the real worker first"
    );
    assert_busy(&service).await;
    backend.blocker.release();
    let output = request.await.unwrap().unwrap();
    assert!(!output.success);
    assert_eq!(output.data, Some(json!({"code":"tool_error"})));
    wait_for_release(&service).await;
}

#[tokio::test(start_paused = true)]
async fn envd_deadline_keeps_slot_while_detached_driver_worker_is_running() {
    let (backend, service, request, _cleanup) = start().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(matches!(request.await.unwrap(), Err(CuaError::Timeout)));
    assert_busy(&service).await;
    backend.blocker.release();
    wait_for_release(&service).await;
}

#[tokio::test(start_paused = true)]
async fn caller_disconnect_keeps_slot_while_detached_driver_worker_is_running() {
    let (backend, service, request, _cleanup) = start().await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert_busy(&service).await;
    backend.blocker.release();
    wait_for_release(&service).await;
}

struct PanickingTools {
    blocker: Arc<Blocker>,
    panicking: Notify,
}

impl DesktopTools for PanickingTools {
    fn tools(&self) -> Vec<ToolDescription> {
        vec![ToolDescription {
            name: "bounded_action".into(),
            input_schema: json!({"type":"object"}),
            ..ToolDescription::default()
        }]
    }

    fn invoke<'a>(&'a self, _: &'a str, _: Value) -> CallFuture<'a> {
        Box::pin(async move {
            InvocationScope::new()
                .run(async {
                    let blocker = self.blocker.clone();
                    drop(spawn_blocking(move || blocker.wait()));
                    self.blocker.entered.notified().await;
                    self.panicking.notify_one();
                    panic!("isolated driver panic after starting a real worker");
                })
                .await
        })
    }
}

#[tokio::test]
async fn driver_panic_drains_blocked_worker_before_service_returns_internal() {
    let blocker = Arc::new(Blocker::default());
    let _cleanup = ReleaseOnDrop(blocker.clone());
    let backend = Arc::new(PanickingTools {
        blocker: blocker.clone(),
        panicking: Notify::new(),
    });
    let service = Arc::new(CuaService::new(backend.clone()));
    let caller = service.clone();
    let request =
        tokio::spawn(async move { caller.call("bounded_action".into(), json!({})).await });
    backend.panicking.notified().await;
    assert!(
        !request.is_finished(),
        "panic must drain the actual worker before propagating"
    );
    assert_busy(&service).await;
    blocker.release();
    assert!(matches!(request.await.unwrap(), Err(CuaError::Internal)));
    assert!(matches!(
        service.call("bounded_action".into(), json!({})).await,
        Err(CuaError::Internal)
    ));
}

#[tokio::test]
async fn blocking_worker_propagates_scope_to_nested_detached_jobs() {
    let blocker = Arc::new(Blocker::default());
    let _cleanup = ReleaseOnDrop(blocker.clone());
    let nested = blocker.clone();
    let parent_returned = Arc::new(Notify::new());
    let returned = parent_returned.clone();
    let request = tokio::spawn(async move {
        let scope = InvocationScope::new();
        scope
            .run(async move {
                spawn_blocking(move || {
                    drop(spawn_blocking(move || nested.wait()));
                })
                .await
                .unwrap();
                returned.notify_one();
            })
            .await;
    });
    blocker.entered.notified().await;
    parent_returned.notified().await;
    assert!(
        !request.is_finished(),
        "nested work must remain tracked after its parent exits"
    );
    blocker.release();
    tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn finite_async_cleanup_inherits_scope_for_nested_blocking_work() {
    let blocker = Arc::new(Blocker::default());
    let _cleanup = ReleaseOnDrop(blocker.clone());
    let nested = blocker.clone();
    let request = tokio::spawn(async move {
        InvocationScope::new()
            .run(async move {
                drop(cua_driver_core::work_scope::spawn(async move {
                    drop(spawn_blocking(move || nested.wait()));
                }));
            })
            .await;
    });
    blocker.entered.notified().await;
    assert!(
        !request.is_finished(),
        "handed-off cleanup must hold admission"
    );
    blocker.release();
    tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn raw_thread_handoff_is_tracked_before_worker_starts() {
    let blocker = Arc::new(Blocker::default());
    let _cleanup = ReleaseOnDrop(blocker.clone());
    let nested = blocker.clone();
    let request = tokio::spawn(async move {
        InvocationScope::new()
            .run(async move {
                let work = cua_driver_core::work_scope::bind_current(move || nested.wait());
                drop(std::thread::spawn(work));
            })
            .await;
    });
    blocker.entered.notified().await;
    assert!(
        !request.is_finished(),
        "raw thread cannot outlive invocation admission"
    );
    blocker.release();
    tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn worker_binding_is_restored_before_thread_is_reused() {
    let blocker = Arc::new(Blocker::default());
    let _cleanup = ReleaseOnDrop(blocker.clone());
    let nested = blocker.clone();
    let runtime = tokio::runtime::Handle::current();
    let request = tokio::spawn(async move {
        InvocationScope::new()
            .run(async move {
                let bound = cua_driver_core::work_scope::bind_current(|| {});
                std::thread::spawn(move || {
                    let _entered = runtime.enter();
                    bound();
                    // This unrelated job runs after the tracked closure has ended.
                    drop(spawn_blocking(move || nested.wait()));
                })
                .join()
                .unwrap();
            })
            .await;
    });
    blocker.entered.notified().await;
    tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .expect("a reused worker must not retain the previous scope")
        .unwrap();
    blocker.release();
}

#[tokio::test]
async fn unstarted_closure_and_panicking_worker_release_their_markers() {
    let scope = InvocationScope::new();
    scope
        .run(async {
            drop(cua_driver_core::work_scope::bind_current(|| {
                panic!("must not run")
            }));
            let work =
                cua_driver_core::work_scope::bind_current(|| panic!("isolated worker panic"));
            assert!(std::thread::spawn(work).join().is_err());
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), scope.drain())
        .await
        .unwrap();
}

#[tokio::test]
async fn bound_future_holds_scope_before_enqueue_and_releases_when_dropped_unpolled() {
    let (send, receive) = tokio::sync::oneshot::channel();
    let request = tokio::spawn(async move {
        InvocationScope::new()
            .run(async move {
                let future = cua_driver_core::work_scope::bind_future(async { 42 });
                assert!(send.send(future).is_ok());
            })
            .await;
    });
    let unpolled = receive.await.unwrap();
    assert!(
        !request.is_finished(),
        "binding must acquire its marker before enqueueing"
    );
    drop(unpolled);
    tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .expect("dropping an unpolled future must release its marker")
        .unwrap();
}

struct DedicatedRuntime {
    stop: std::sync::mpsc::Sender<()>,
    owner: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DedicatedRuntime {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

#[tokio::test]
async fn bound_future_preserves_dedicated_runtime_and_tracks_nested_blocking_work() {
    let (send_runtime, receive_runtime) = tokio::sync::oneshot::channel();
    let (stop, stopped) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("completion-dedicated")
            .enable_all()
            .build()
            .unwrap();
        send_runtime.send(runtime.handle().clone()).unwrap();
        let _ = stopped.recv();
    });
    let _runtime = DedicatedRuntime {
        stop,
        owner: Some(owner),
    };
    let dedicated = receive_runtime.await.unwrap();
    // Release a blocked worker before the dedicated runtime is dropped, even on assertion failure.
    let blocker = Arc::new(Blocker::default());
    let _cleanup = ReleaseOnDrop(blocker.clone());
    let nested = blocker.clone();
    let (send_name, receive_name) = tokio::sync::oneshot::channel();
    let request = tokio::spawn(async move {
        InvocationScope::new()
            .run(async move {
                let future = cua_driver_core::work_scope::bind_future(async move {
                    send_name
                        .send(std::thread::current().name().unwrap_or("").to_owned())
                        .unwrap();
                    drop(spawn_blocking(move || nested.wait()));
                });
                drop(dedicated.spawn(future));
            })
            .await;
    });
    assert_eq!(receive_name.await.unwrap(), "completion-dedicated");
    blocker.entered.notified().await;
    assert!(
        !request.is_finished(),
        "nested work must inherit its cross-runtime scope"
    );
    blocker.release();
    tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn bound_future_outside_scope_preserves_ordinary_execution() {
    let output = cua_driver_core::work_scope::bind_future(async { 42 }).await;
    assert_eq!(output, 42);
}
