use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::mmds::{
    poll::{poll, MetadataFiles, MetadataLifecycle, MetadataSink, PollOutcome},
    BoxFuture, HttpReply, HttpRequest, HttpTransport, MmdsClient, MmdsOpts, RoutePin,
};
use std::{
    collections::VecDeque,
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tower::ServiceExt;

async fn envs(router: axum::Router, token: Option<&str>) -> serde_json::Value {
    let mut request = Request::get("/envs");
    if let Some(token) = token {
        request = request.header("X-Access-Token", token);
    }
    let response = router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}
#[tokio::test(start_paused = true)]
async fn guest_non_fc_skips_boot_but_every_successful_init_refreshes_internal_ids() {
    use coorworker_envd::daemon::{app, Daemon};
    let dir = tempfile::tempdir().unwrap();
    let (client, transport, _, _) = setup(vec![
        reply("t"),
        reply(r#"{"instanceID":"sandbox","envID":"template"}"#),
        reply("fresh"),
        reply(r#"{"instanceID":"updated","envID":"next"}"#),
    ]);
    let daemon = Arc::new(
        Daemon::new()
            .with_metadata_polling(client, dir.path().into(), false)
            .await,
    );
    let router = app(daemon.clone());
    step(100).await;
    assert!(transport.calls.lock().unwrap().is_empty());
    assert_eq!(envs(router.clone(), None).await["E2B_SANDBOX"], "false");
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"envVars":{"E2B_SANDBOX_ID":"userwrong"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    step(0).await;
    step(50).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
        if dir.path().join(".E2B_TEMPLATE_ID").exists() {
            break;
        }
    }
    let values = envs(router.clone(), None).await;
    assert_eq!(values["E2B_SANDBOX_ID"], "sandbox");
    assert_eq!(values["E2B_TEMPLATE_ID"], "template");
    assert_eq!(
        std::fs::read(dir.path().join(".E2B_TEMPLATE_ID")).unwrap(),
        b"template"
    );
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(
                    r#"{"envVars":{"E2B_SANDBOX_ID":"changed","E2B_TEMPLATE_ID":"bad"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        envs(router.clone(), None).await["E2B_SANDBOX_ID"],
        "sandbox"
    );
    step(0).await;
    step(50).await;
    assert_eq!(envs(router, None).await["E2B_SANDBOX_ID"], "updated");
    daemon.shutdown_metadata().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn boot_fc_starts_poll_while_auth_and_pending_effects_failures_do_not_refresh() {
    use coorworker_envd::daemon::{app, Daemon};
    let dir = tempfile::tempdir().unwrap();
    let (client, transport, _, _) = setup(vec![]);
    let daemon = Arc::new(
        Daemon::new()
            .with_metadata_polling(client, dir.path().into(), true)
            .await,
    );
    step(0).await;
    step(49).await;
    assert!(transport.calls.lock().unwrap().is_empty());
    step(1).await;
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
    daemon.shutdown_metadata().await.unwrap();
    let (client, transport, _, _) = setup(vec![]);
    let daemon = Arc::new(
        Daemon::new()
            .with_metadata_polling(client, dir.path().into(), false)
            .await,
    );
    let router = app(daemon.clone());
    // The clock slice decodes timestamps (tests/clock.rs), so a non-string
    // timestamp is now a whole-body decode failure; neither it nor a real
    // pending-effect refusal may start a refresh.
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"timestamp":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(
                    // A non-empty volumeMounts list is the pending-effect
                    // refusal: a null or empty list is the oracle's successful
                    // no-op (init.go:480 / init.go:796-874), covered by
                    // tests/daemon.rs.
                    r#"{"volumeMounts":[{"path":"/data"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    step(100).await;
    assert!(transport.calls.lock().unwrap().is_empty());
    router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"installed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    step(0).await;
    step(50).await;
    let count = transport.calls.lock().unwrap().len();
    let response = router
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"accessToken":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    step(50).await;
    assert_eq!(transport.calls.lock().unwrap().len(), count);
    daemon.shutdown_metadata().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn accepted_init_refresh_survives_http_cancel_and_health_stays_available() {
    use coorworker_envd::{
        cpus::{ChildWrites, CpuManager, CpuSysfs},
        daemon::{app, Daemon},
    };
    struct Block {
        entered: tokio::sync::Notify,
        open: Mutex<bool>,
        changed: std::sync::Condvar,
    }
    impl CpuSysfs for Block {
        fn possible(&self) -> io::Result<Vec<usize>> {
            self.entered.notify_one();
            let open = self.open.lock().unwrap();
            drop(self.changed.wait_while(open, |open| !*open).unwrap());
            Ok(vec![0, 1])
        }
        fn online(&self) -> io::Result<Vec<usize>> {
            Ok(vec![0])
        }
        fn set_online(&self, _: usize, _: bool) -> io::Result<()> {
            panic!("no sysfs writes")
        }
    }
    struct Children;
    impl ChildWrites for Children {
        fn write(&self, _: &std::path::Path, _: bool) -> io::Result<()> {
            panic!("no child")
        }
        fn wait_adopted(&self) {}
    }
    struct Release(Arc<Block>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.open.lock().unwrap() = true;
            self.0.changed.notify_all();
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let block = Arc::new(Block {
        entered: tokio::sync::Notify::new(),
        open: Mutex::new(false),
        changed: std::sync::Condvar::new(),
    });
    let release = Release(block.clone());
    let manager = Arc::new(CpuManager::new(
        block.clone(),
        Arc::new(Children),
        directory.path().join("target"),
    ));
    let (client, transport, _, _) = setup(vec![]);
    let daemon = Arc::new(
        Daemon::with_cpu_manager(manager)
            .with_metadata_polling(client, directory.path().into(), false)
            .await,
    );
    let router = app(daemon.clone());
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
    block.entered.notified().await;
    first.abort();
    let _ = first.await;
    assert_eq!(
        router
            .clone()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    drop(release);
    for _ in 0..100 {
        tokio::task::yield_now().await;
        if directory.path().join("target").exists() {
            break;
        }
    }
    // The next init only takes the init lock once the accepted transaction has
    // finished its blocking phase and started its background refresh.
    let response = router
        .clone()
        .oneshot(
            Request::post("/init")
                .body(Body::from(r#"{"cpuCount":2}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    step(0).await;
    step(50).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
        if !transport.calls.lock().unwrap().is_empty() {
            break;
        }
    }
    assert!(
        !transport.calls.lock().unwrap().is_empty(),
        "accepted transaction lost refresh after HTTP cancellation"
    );
    // Both polls' tickers fire on the same paused instant, so the cancelled
    // transaction's refresh must be here too: the count attributes one call to
    // it directly. A regression that moves start_post_init out of the blocking
    // closure leaves only the second init's poll, i.e. calls == 1.
    assert_eq!(
        transport.calls.lock().unwrap().len(),
        2,
        "refresh must be attributed to the cancelled transaction as well"
    );
    step(60000).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
    daemon.shutdown_metadata().await.unwrap();
}

#[test]
fn startup_run_dir_writes_sandbox_flag_with_requested_modes_and_boolean() {
    use coorworker_envd::mmds::poll::initialize_run_dir;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    for flag in [true, false] {
        let root = dir.path().join(flag.to_string());
        initialize_run_dir(&root, flag);
        assert_eq!(
            std::fs::read(root.join(".E2B_SANDBOX")).unwrap(),
            flag.to_string().as_bytes()
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(root.join(".E2B_SANDBOX"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
    }
}

struct Reply {
    delay: Duration,
    body: io::Result<Vec<u8>>,
}
struct Script {
    replies: Mutex<VecDeque<Reply>>,
    calls: Mutex<Vec<(tokio::time::Instant, HttpRequest)>>,
    active: Arc<AtomicUsize>,
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl HttpTransport for Script {
    fn request(&self, request: HttpRequest) -> BoxFuture<'_, io::Result<HttpReply>> {
        self.calls
            .lock()
            .unwrap()
            .push((tokio::time::Instant::now(), request));
        let reply = self.replies.lock().unwrap().pop_front();
        self.active.fetch_add(1, Ordering::SeqCst);
        let active = Active(self.active.clone());
        Box::pin(async move {
            let _active = active;
            match reply {
                Some(reply) => {
                    tokio::time::sleep(reply.delay).await;
                    Ok(HttpReply {
                        status: 200,
                        headers: vec![],
                        body: reply.body?,
                    })
                }
                None => std::future::pending().await,
            }
        })
    }
}
struct NoRoute(AtomicUsize);
impl RoutePin for NoRoute {
    fn pin(&self) -> BoxFuture<'_, io::Result<()>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
#[derive(Default)]
struct Sink(Mutex<Vec<MmdsOpts>>);
impl MetadataSink for Sink {
    fn store_ids<'a>(&'a self, opts: &'a MmdsOpts) -> BoxFuture<'a, ()> {
        Box::pin(async {
            self.0.lock().unwrap().push(opts.clone());
        })
    }
}
fn reply(body: &str) -> Reply {
    Reply {
        delay: Duration::ZERO,
        body: Ok(body.as_bytes().to_vec()),
    }
}
fn setup(replies: Vec<Reply>) -> (Arc<MmdsClient>, Arc<Script>, Arc<NoRoute>, Arc<Sink>) {
    let transport = Arc::new(Script {
        replies: Mutex::new(replies.into()),
        calls: Mutex::new(vec![]),
        active: Arc::new(AtomicUsize::new(0)),
    });
    let route = Arc::new(NoRoute(AtomicUsize::new(0)));
    (
        Arc::new(MmdsClient::new(transport.clone(), route.clone())),
        transport,
        route,
        Arc::new(Sink::default()),
    )
}
async fn step(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}
#[tokio::test(start_paused = true)]
async fn first_tick_is_50ms_retries_fresh_exchange_without_route_and_stops_on_success() {
    let (client, transport, route, sink) = setup(vec![
        reply(""),
        reply("one"),
        reply("bad json"),
        reply("two"),
        reply(r#"{"instanceID":"s","envID":"t","address":"logs"}"#),
    ]);
    let (options, mut received) = mpsc::channel(1);
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink.clone(), options, rx, None));
    step(0).await;
    step(49).await;
    assert!(transport.calls.lock().unwrap().is_empty());
    step(1).await;
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
    step(50).await;
    assert_eq!(transport.calls.lock().unwrap().len(), 3);
    step(50).await;
    task.await.unwrap();
    let opts = received.recv().await.unwrap();
    assert_eq!(opts.instance_id, "s");
    assert_eq!(opts.env_id, "t");
    assert_eq!(route.0.load(Ordering::SeqCst), 0);
    let calls = transport.calls.lock().unwrap();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].1.method, "PUT");
    assert_eq!(calls[1].1.method, "PUT");
    assert_eq!(calls[2].1.method, "GET");
    assert_eq!(calls[2].1.headers[0].1, b"one");
    assert_eq!(calls[4].1.headers[0].1, b"two");
    drop(calls);
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    drop(stop);
}
#[tokio::test(start_paused = true)]
async fn slow_lookup_keeps_one_pending_tick_then_realigns_future_ticks() {
    let (client, transport, _, sink) = setup(vec![
        Reply {
            delay: Duration::from_millis(225),
            body: Err(io::Error::other("late")),
        },
        reply(""),
        reply(""),
    ]);
    let (options, _received) = mpsc::channel(1);
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink, options, rx, None));
    step(0).await;
    step(50).await;
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
    step(225).await;
    assert_eq!(transport.calls.lock().unwrap().len(), 2);
    step(24).await;
    assert_eq!(transport.calls.lock().unwrap().len(), 2);
    step(1).await;
    assert_eq!(transport.calls.lock().unwrap().len(), 3);
    stop.send(true).unwrap();
    task.await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn empty_address_exits_without_sending_and_cancel_drops_query_and_full_channel_wait() {
    let (client, transport, _, sink) = setup(vec![reply("token"), reply(r#"{"instanceID":"s"}"#)]);
    let (options, mut received) = mpsc::channel(1);
    let (_stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink, options, rx, None));
    step(0).await;
    step(50).await;
    task.await.unwrap();
    assert!(received.try_recv().is_err());
    assert_eq!(transport.calls.lock().unwrap().len(), 2);
    let (client, transport, _, sink) = setup(vec![]);
    let (options, _received) = mpsc::channel(1);
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink, options, rx, None));
    step(0).await;
    step(50).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 1);
    stop.send(true).unwrap();
    task.await.unwrap();
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
    let (client, _, _, sink) = setup(vec![reply("t"), reply(r#"{"address":"logs"}"#)]);
    let (options, _received) = mpsc::channel(1);
    options.send(MmdsOpts::default()).await.unwrap();
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink.clone(), options, rx, None));
    step(0).await;
    step(50).await;
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert!(!task.is_finished());
    stop.send(true).unwrap();
    task.await.unwrap();
}
#[tokio::test]
async fn files_are_best_effort_continue_second_write_and_do_not_create_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".E2B_SANDBOX_ID")).unwrap();
    let opts = MmdsOpts {
        instance_id: "s".into(),
        env_id: "t".into(),
        ..Default::default()
    };
    MetadataFiles::new(dir.path().into()).write(&opts).await;
    assert_eq!(
        std::fs::read(dir.path().join(".E2B_TEMPLATE_ID")).unwrap(),
        b"t"
    );
    let absent = dir.path().join("absent");
    MetadataFiles::new(absent.clone()).write(&opts).await;
    assert!(!absent.exists());
}
#[tokio::test(start_paused = true)]
async fn stop_before_success_prints_unsuffixed_gave_up_and_returns_stopped() {
    let (client, transport, _, sink) = setup(vec![reply("")]);
    let (options, _received) = mpsc::channel(1);
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink, options, rx, None));
    step(0).await;
    step(49).await;
    assert!(transport.calls.lock().unwrap().is_empty());
    stop.send(true).unwrap();
    assert!(matches!(
        task.await.unwrap(),
        PollOutcome::Stopped { last_error: None }
    ));
}
#[tokio::test(start_paused = true)]
async fn stop_after_failure_reports_stopped_with_last_error_suffix() {
    let (client, transport, _, sink) = setup(vec![Reply {
        delay: Duration::ZERO,
        body: Err(io::Error::other("drop")),
    }]);
    let (options, _received) = mpsc::channel(1);
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink, options, rx, None));
    step(0).await;
    step(50).await;
    stop.send(true).unwrap();
    let outcome = task.await.unwrap();
    let PollOutcome::Stopped {
        last_error: Some(error),
    } = outcome
    else {
        panic!("expected stopped with last error, got {outcome:?}")
    };
    assert!(error.to_string().ends_with("drop"), "{error}");
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn deadline_expiry_after_failure_reports_timed_out_with_last_error_suffix() {
    let (client, _, _, sink) = setup(vec![Reply {
        delay: Duration::ZERO,
        body: Err(io::Error::other("drop")),
    }]);
    let (options, _received) = mpsc::channel(1);
    let (_stop, rx) = watch::channel(false);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(60);
    let task = tokio::spawn(poll(client, sink, options, rx, Some(deadline)));
    step(0).await;
    step(50).await;
    // The failed attempt waits for the next tick, but the deadline wins first.
    let outcome = task.await.unwrap();
    let PollOutcome::TimedOut {
        last_error: Some(error),
    } = outcome
    else {
        panic!("expected timed out with last error, got {outcome:?}")
    };
    assert!(error.to_string().ends_with("drop"), "{error}");
}
#[tokio::test(start_paused = true)]
async fn closed_receiver_still_delivers_after_send_error() {
    let (client, _, _, sink) = setup(vec![reply("t"), reply(r#"{"address":"logs"}"#)]);
    let (options, received) = mpsc::channel(1);
    // Nobody holds the receiver, so the send must fail; the options are still
    // counted as delivered and the poll must not hang or report cancellation.
    drop(received);
    let (_stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink.clone(), options, rx, None));
    step(0).await;
    step(50).await;
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert!(matches!(task.await.unwrap(), PollOutcome::Delivered));
}
#[tokio::test(start_paused = true)]
async fn stop_racing_blocked_send_wins_and_marks_send_cancelled() {
    let (client, _, _, sink) = setup(vec![reply("t"), reply(r#"{"address":"logs"}"#)]);
    let (options, _received) = mpsc::channel(1);
    options.send(MmdsOpts::default()).await.unwrap();
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(poll(client, sink.clone(), options, rx, None));
    step(0).await;
    step(50).await;
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert!(!task.is_finished());
    stop.send(true).unwrap();
    assert!(matches!(task.await.unwrap(), PollOutcome::SendCancelled));
}
#[tokio::test(start_paused = true)]
async fn deadline_racing_send_reports_send_cancelled_and_slow_store_survives_stop() {
    let (client, _, _, sink) = setup(vec![reply("t"), reply(r#"{"address":"logs"}"#)]);
    let (options, mut received) = mpsc::channel(1);
    // Occupy the only slot so the fetched options cannot be delivered.
    options.send(MmdsOpts::default()).await.unwrap();
    let (stop, rx) = watch::channel(false);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(60);
    let task = tokio::spawn(poll(client, sink.clone(), options, rx, Some(deadline)));
    step(0).await;
    step(50).await;
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    assert!(!task.is_finished());
    // Go's send select treats a deadline as cancellation, unlike outer fetch/deadline.
    assert!(matches!(task.await.unwrap(), PollOutcome::SendCancelled));
    assert!(received.try_recv().is_ok());
    assert!(received.try_recv().is_err());
    drop(stop);
    struct SlowSink(Mutex<Vec<MmdsOpts>>);
    impl MetadataSink for SlowSink {
        fn store_ids<'a>(&'a self, opts: &'a MmdsOpts) -> BoxFuture<'a, ()> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                self.0.lock().unwrap().push(opts.clone());
            })
        }
    }
    let slow = Arc::new(SlowSink(Mutex::new(vec![])));
    let (client, transport, _, _) = setup(vec![reply("t"), reply(r#"{"instanceID":"kept"}"#)]);
    let (options, _received) = mpsc::channel(1);
    let (stop, rx) = watch::channel(false);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(60);
    let task = tokio::spawn(poll(client, slow.clone(), options, rx, Some(deadline)));
    step(0).await;
    step(50).await;
    step(10).await;
    stop.send(true).unwrap();
    step(25).await;
    // The store is synchronous in Go, so neither deadline nor stop interrupts it.
    assert_eq!(slow.0.lock().unwrap().len(), 1);
    assert!(matches!(task.await.unwrap(), PollOutcome::Delivered));
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
}
#[tokio::test(start_paused = true)]
async fn shutdown_cancels_pending_deadline_poll_without_waiting_for_it() {
    let (client, transport, _, sink) = setup(vec![]);
    let lifecycle = MetadataLifecycle::new(client, sink);
    lifecycle.start_post_init();
    step(0).await;
    step(50).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 1);
    // Stop must win over the pending 60s deadline, not wait for it. Paused time
    // auto-advances when the runtime goes idle, so observe the clock as well.
    let before = tokio::time::Instant::now();
    lifecycle.shutdown().await.unwrap();
    assert!(
        tokio::time::Instant::now().duration_since(before) < Duration::from_secs(1),
        "shutdown waited for the deadline instead of cancelling"
    );
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
}
#[tokio::test(start_paused = true)]
async fn post_init_budgets_report_deadline_outcome_and_boot_reports_stopped() {
    let (client, transport, _, sink) = setup(vec![]);
    let lifecycle = MetadataLifecycle::new(client, sink);
    lifecycle.start_post_init();
    step(0).await;
    step(50).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 1);
    step(29950).await;
    lifecycle.start_post_init();
    step(0).await;
    step(50).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 2);
    step(29950).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 1);
    step(30000).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
    lifecycle.start_boot();
    step(0).await;
    step(50).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 1);
    lifecycle.shutdown().await.unwrap();
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
    lifecycle.start_boot();
    step(100).await;
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
}
