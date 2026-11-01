#![cfg(target_os = "linux")]

use coorworker_envd::oom::{OomKill, OomWatcher};

#[test]
fn kills_are_unknown_until_the_kernel_log_has_been_drained() {
    assert!(OomWatcher::new().kills().is_none());
}

#[test]
fn kill_json_matches_the_pinned_contract() {
    let kill = OomKill {
        seq: -3,
        process: "a) worker".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(kill).unwrap(),
        serde_json::json!({"seq": -3, "process": "a) worker"})
    );
}

#[tokio::test]
async fn cancellation_before_start_does_not_open_the_host_kernel_log() {
    let watcher = OomWatcher::new();
    let (cancel, stopped) = tokio::sync::oneshot::channel();
    cancel.send(()).unwrap();
    watcher.run(stopped).await;
    assert!(watcher.kills().is_none());
}
