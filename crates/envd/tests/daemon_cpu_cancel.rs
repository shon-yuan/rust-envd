use std::sync::{Arc, Condvar, Mutex};

use axum::{body::Body, http::Request};
use coorworker_envd::{
    cpus::{ChildWrites, CpuManager, CpuSysfs},
    daemon::{app, Daemon},
};
use tower::ServiceExt;

struct PausedValidation {
    first: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    changed: Condvar,
}
impl CpuSysfs for PausedValidation {
    fn possible(&self) -> std::io::Result<Vec<usize>> {
        if !self.first.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            let release = self.released.lock().unwrap();
            drop(
                self.changed
                    .wait_while(release, |released| !*released)
                    .unwrap(),
            );
        }
        Ok(vec![0, 1, 2, 3])
    }
    fn online(&self) -> std::io::Result<Vec<usize>> {
        Ok(vec![0])
    }
    fn set_online(&self, _: usize, _: bool) -> std::io::Result<()> {
        panic!("no writes")
    }
}
struct Children;
impl ChildWrites for Children {
    fn write(&self, _: &std::path::Path, _: bool) -> std::io::Result<()> {
        panic!("no children")
    }
    fn wait_adopted(&self) {}
}
struct Release(Arc<PausedValidation>);
impl Drop for Release {
    fn drop(&mut self) {
        *self.0.released.lock().unwrap() = true;
        self.0.changed.notify_all();
    }
}

#[tokio::test]
async fn cancelled_init_keeps_serialization_until_cpu_update_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let sys = Arc::new(PausedValidation {
        first: std::sync::atomic::AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
        released: Mutex::new(false),
        changed: Condvar::new(),
    });
    let release = Release(sys.clone());
    let manager = Arc::new(CpuManager::new(
        sys.clone(),
        Arc::new(Children),
        dir.path().join("target"),
    ));
    let router = app(Arc::new(Daemon::with_cpu_manager(manager.clone())));
    let first_router = router.clone();
    let first = tokio::spawn(async move {
        first_router
            .oneshot(
                Request::post("/init")
                    .body(Body::from(r#"{"cpuCount":2}"#))
                    .unwrap(),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), sys.entered.notified())
        .await
        .unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let mut second = tokio::spawn(async move {
        router
            .oneshot(
                Request::post("/init")
                    .body(Body::from(r#"{"cpuCount":4}"#))
                    .unwrap(),
            )
            .await
    });
    // The second transaction must wait while the canceled first transaction still owns its write.
    let completed_early =
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut second).await;
    let was_early = completed_early.is_ok();
    drop(release);
    if !was_early {
        assert_eq!(second.await.unwrap().unwrap().status(), 204);
    }
    assert!(
        !was_early,
        "newer init escaped the canceled transaction's serialization"
    );
    assert_eq!(manager.status().target, 4);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target")).unwrap(),
        "4"
    );
}
