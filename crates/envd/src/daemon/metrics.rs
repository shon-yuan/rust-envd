//! HTTP resource sampling, CPU manager status, and optional OOM history.
use std::{io, sync::Arc};

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

use super::Daemon;
use crate::metrics::{Collector, CpuStatus};

pub(super) async fn get_metrics(State(daemon): State<Arc<Daemon>>) -> Response {
    let collector = daemon.metrics.clone();
    let cpus = daemon.cpus.clone();
    let sample = tokio::task::spawn_blocking(move || {
        let mut state = collector
            .lock()
            .map_err(|_| io::Error::other("metrics collector lock poisoned"))?;
        if state.is_err() {
            *state = Collector::new();
        }
        match state.as_mut() {
            Ok(collector) => {
                let status = cpus.status();
                collector.sample(CpuStatus {
                    possible: status.possible as u32,
                    target: status.target as u32,
                    attempts: status.attempts as u32,
                    write_pending_ms: status.write_pending.as_millis() as u64,
                })
            }
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    })
    .await
    .map_err(io::Error::other)
    .and_then(|sample| sample)
    .and_then(|metrics| {
        #[derive(serde::Serialize)]
        struct ResponseMetrics {
            #[serde(flatten)]
            sample: crate::metrics::Metrics,
            #[serde(skip_serializing_if = "Option::is_none")]
            oom_kills: Option<Vec<crate::oom::OomKill>>,
        }
        let value = ResponseMetrics {
            sample: metrics,
            oom_kills: daemon.oom.as_ref().and_then(|watcher| watcher.kills()),
        };
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        Ok(bytes)
    });
    let mut response = match sample {
        Ok(bytes) => bytes.into_response(),
        Err(error) => {
            eprintln!("failed to collect metrics: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{daemon::app, metrics::MetricPaths};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn oom_history_is_omitted_until_known_then_reported_without_consuming() {
        for (records, known, expected) in [
            (Vec::<&str>::new(), false, None),
            (Vec::new(), true, Some(serde_json::json!([]))),
            (
                vec!["3,12,0,-;Out of memory: Killed process 1 (worker) total-vm:1kB"],
                true,
                Some(serde_json::json!([{"seq":12,"process":"worker"}])),
            ),
        ] {
            let mut daemon = Daemon::new();
            daemon.oom = Some(Arc::new(crate::oom::OomWatcher::test_snapshot(
                &records, known,
            )));
            let router = app(Arc::new(daemon));
            for _ in 0..2 {
                let response = router
                    .clone()
                    .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let value: serde_json::Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap())
                        .unwrap();
                assert_eq!(value.get("oom_kills"), expected.as_ref());
            }
        }
    }

    #[tokio::test]
    async fn collection_failure_is_empty_500_and_later_requests_recover() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("stat"), "cpu 10 0 0 90 0 0 0\n").unwrap();
        let collector = Collector::with_paths(MetricPaths {
            proc_root: root.path().into(),
            disk_root: "/".into(),
        })
        .unwrap();
        let daemon = Arc::new(Daemon::new());
        *daemon.metrics.lock().unwrap() = Ok(collector);
        let router = app(daemon);
        let response = router
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .is_empty());
        std::fs::write(
            root.path().join("meminfo"),
            "MemTotal: 1024 kB\nMemAvailable: 512 kB\nCached: 128 kB\n",
        )
        .unwrap();
        std::fs::write(root.path().join("cpuinfo"), "processor : 0\n").unwrap();
        let response = router
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(value["mem_total"], 1024 * 1024);
        assert_eq!(value["mem_used"], 512 * 1024);
    }
}
