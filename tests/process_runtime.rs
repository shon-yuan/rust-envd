pub use coorworker_envd::{cgroups, exec_context, proto};
// Import the runtime directly so integration tests also cover its private modules.
#[path = "../src/process/mod.rs"]
mod runtime;
// The runtime's start refusals render Go errno text (`crate::errno_text`);
// this standalone compilation unit needs the same module under its own root.
#[path = "../src/errno_text.rs"]
mod errno_text;

use coorworker_envd::exec_context::{Account, Defaults, Environment};
use coorworker_envd::proto::process::{self as pb, process_event, process_input, process_selector};
use nix::unistd::{getegid, geteuid, getgroups};
use runtime::{ProcessRuntime, RuntimeError, Subscription};
use serde_json::json;
use std::time::Duration;

fn runtime() -> ProcessRuntime {
    assert!(
        !geteuid().is_root(),
        "tests may only spawn ordinary-user children"
    );
    ProcessRuntime::new(
        Account {
            name: "runtime-test".into(),
            home: "/tmp".into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: getgroups()
                .unwrap()
                .into_iter()
                .map(|g| g.as_raw())
                .collect(),
        },
        Defaults {
            user: "runtime-test".into(),
            workdir: Some("/tmp".into()),
            user_delivered: true,
        },
        Environment::default(),
        Some("/usr/bin:/bin".into()),
    )
    .unwrap()
}

fn request(cmd: &str, args: &[&str]) -> pb::StartRequest {
    serde_json::from_value(json!({"process": {"cmd":cmd,"args":args}, "tag":"test"})).unwrap()
}

fn pid_selector(pid: u32) -> pb::ProcessSelector {
    pb::ProcessSelector {
        selector: Some(process_selector::Selector::Pid(pid)),
        ..Default::default()
    }
}

async fn start_pid(stream: &mut Subscription) -> u32 {
    match stream.recv().await.unwrap().event.unwrap() {
        process_event::Event::Start(event) => event.pid,
        event => panic!("expected start, got {event:?}"),
    }
}

async fn finish(stream: &mut Subscription) -> (Vec<u8>, Vec<u8>, process_event::EndEvent) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .unwrap()
    {
        match event.event.unwrap() {
            process_event::Event::Data(data) => match data.output.unwrap() {
                process_event::data_event::Output::Stdout(bytes) => stdout.extend(bytes),
                process_event::data_event::Output::Stderr(bytes) => stderr.extend(bytes),
                process_event::data_event::Output::Pty(_) => panic!("unexpected pty"),
            },
            process_event::Event::End(end) => return (stdout, stderr, *end),
            process_event::Event::Keepalive(_) => {}
            process_event::Event::Start(_) => panic!("duplicate start"),
        }
    }
    panic!("missing end")
}

#[tokio::test]
async fn streams_before_exit_and_lists_then_closes_stdin() {
    let rt = runtime().with_context(
        Defaults {
            user: "runtime-test".into(),
            workdir: Some("/tmp".into()),
            user_delivered: true,
        },
        Environment::default(),
    );
    let rt = rt.with_account(Account {
        name: "runtime-test".into(),
        home: "/tmp".into(),
        uid: geteuid().as_raw(),
        gid: getegid().as_raw(),
        groups: getgroups()
            .unwrap()
            .iter()
            .map(|gid| gid.as_raw())
            .collect(),
    });
    let mut stream = rt.start(request("/bin/cat", &[]), None).await.unwrap();
    let pid = start_pid(&mut stream).await;
    let selector = pid_selector(pid);
    assert_eq!(rt.list().await[0].pid, pid);
    let input = pb::ProcessInput {
        input: Some(process_input::Input::Stdin(
            "literal 雪 ; $()\n".as_bytes().to_vec(),
        )),
        ..Default::default()
    };
    rt.send_input(&selector, &input).await.unwrap();
    match stream.recv().await.unwrap().event.unwrap() {
        process_event::Event::Data(data) => assert_eq!(
            data.output,
            Some(process_event::data_event::Output::Stdout(
                "literal 雪 ; $()\n".as_bytes().to_vec()
            ))
        ),
        event => panic!("expected realtime output, got {event:?}"),
    }
    rt.close_stdin(&selector).await.unwrap();
    rt.close_stdin(&selector).await.unwrap();
    let (_, _, end) = finish(&mut stream).await;
    assert_eq!(end.exit_code, 0);
    assert!(end.exited);
    assert_eq!(end.status, "exit status 0");
    assert_eq!(end.error, None);
    assert!(rt.list().await.is_empty());
    let mut retained = rt.connect(&selector).await.unwrap();
    assert_eq!(start_pid(&mut retained).await, pid);
    let (out, err, retained_end) = finish(&mut retained).await;
    assert!(out.is_empty() && err.is_empty());
    assert_eq!(retained_end, end);
}

#[tokio::test]
async fn literal_argv_env_and_nonzero_match_go_wrapper() {
    let rt = runtime();
    let mut stream = rt
        .start(
            request("/usr/bin/printf", &["%s", "雪 ; $(exit 17) '\""]),
            None,
        )
        .await
        .unwrap();
    start_pid(&mut stream).await;
    let (out, err, end) = finish(&mut stream).await;
    assert_eq!(out, "雪 ; $(exit 17) '\"".as_bytes());
    assert!(err.is_empty());
    assert_eq!(end.exit_code, 0);
    let mut stream = rt
        .start(
            request(
                "/bin/sh",
                &["-c", "printf '\\377\\000'; printf error >&2; exit 7"],
            ),
            None,
        )
        .await
        .unwrap();
    start_pid(&mut stream).await;
    let (out, err, end) = finish(&mut stream).await;
    assert_eq!(out, [255, 0]);
    assert_eq!(err, b"error");
    assert_eq!(end.exit_code, 7);
    assert_eq!(end.status, "exit status 7");
    assert_eq!(end.error.as_deref(), Some("exit status 7"));
}

#[tokio::test]
async fn disconnect_keeps_child_alive_and_connect_has_no_output_replay() {
    let rt = runtime();
    let mut original = rt
        .start(request("/bin/cat", &[]), Some(Duration::from_secs(3)))
        .await
        .unwrap();
    let pid = start_pid(&mut original).await;
    drop(original);
    let selector = pid_selector(pid);
    rt.send_input(
        &selector,
        &serde_json::from_value(json!({"stdin":"b2xk"})).unwrap(),
    )
    .await
    .unwrap();
    // Reading via a second write ensures attachment sees future data; no snapshots.
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(rt.list().await.len(), 1);
    let mut attached = rt.connect(&selector).await.unwrap();
    assert_eq!(start_pid(&mut attached).await, pid);
    rt.send_input(
        &selector,
        &serde_json::from_value(json!({"stdin":"bmV3"})).unwrap(),
    )
    .await
    .unwrap();
    rt.close_stdin(&selector).await.unwrap();
    let (out, _, _) = finish(&mut attached).await;
    assert_eq!(out, b"new");
}

#[tokio::test]
async fn deadline_and_signals_reap_and_retain_real_children() {
    let rt = runtime();
    let mut stream = rt
        .start(
            request("/bin/sleep", &["5"]),
            Some(Duration::from_millis(80)),
        )
        .await
        .unwrap();
    start_pid(&mut stream).await;
    let (_, _, end) = finish(&mut stream).await;
    assert_eq!(end.exit_code, -1);
    assert!(!end.exited);
    assert_eq!(end.status, "signal: killed");
    let mut stream = rt.start(request("/bin/sleep", &["5"]), None).await.unwrap();
    let pid = start_pid(&mut stream).await;
    assert!(matches!(
        rt.send_signal(&pid_selector(pid), 0).await,
        Err(RuntimeError::Unimplemented(_))
    ));
    rt.send_signal(&pid_selector(pid), 15).await.unwrap();
    let (_, _, end) = finish(&mut stream).await;
    assert_eq!(end.status, "signal: terminated");
    assert!(!end.exited);
    assert_eq!(end.error.as_deref(), Some("signal: terminated"));
}

#[tokio::test]
async fn rejects_wrong_input_missing_selector_and_bad_cwd() {
    let rt = runtime();
    assert!(matches!(
        rt.connect(&pb::ProcessSelector::default()).await,
        Err(RuntimeError::Unimplemented(_))
    ));
    assert!(matches!(
        rt.connect(&pid_selector(u32::MAX)).await,
        Err(RuntimeError::NotFound)
    ));
    let request: pb::StartRequest =
        serde_json::from_value(json!({"process":{"cmd":"/bin/cat"},"stdin":false})).unwrap();
    let mut stream = rt.start(request, None).await.unwrap();
    let pid = start_pid(&mut stream).await;
    let wrong: pb::ProcessInput = serde_json::from_value(json!({"pty":"eA=="})).unwrap();
    assert!(matches!(
        rt.send_input(&pid_selector(pid), &wrong).await,
        Err(RuntimeError::FailedPrecondition(_))
    ));
    rt.close_stdin(&pid_selector(pid)).await.unwrap();
    finish(&mut stream).await;
    let invalid: pb::StartRequest = serde_json::from_value(
        json!({"process":{"cmd":"/bin/true","cwd":"/definitely/absent/envd"}}),
    )
    .unwrap();
    assert!(matches!(
        rt.start(invalid, None).await,
        Err(RuntimeError::InvalidArgument(_))
    ));
}

#[tokio::test]
async fn environment_cwd_and_identity_use_preconfigured_context() {
    let mut environment = Environment::default();
    environment.store_internal("ENVD_INTERNAL", "internal");
    environment.replace_user(std::collections::BTreeMap::from([(
        "ORDER".into(),
        "default".into(),
    )]));
    let rt = ProcessRuntime::new(
        Account {
            name: "configured".into(),
            home: "/tmp".into(),
            uid: geteuid().as_raw(),
            gid: getegid().as_raw(),
            groups: getgroups()
                .unwrap()
                .iter()
                .map(|gid| gid.as_raw())
                .collect(),
        },
        Defaults {
            user: "configured".into(),
            workdir: Some("/tmp".into()),
            user_delivered: true,
        },
        environment,
        Some("/usr/bin:/bin".into()),
    )
    .unwrap();
    let req = serde_json::from_value(json!({"process":{"cmd":"/bin/sh", "args":["-c","printf '%s|%s|%s|%s' \"$ORDER\" \"$ENVD_INTERNAL\" \"$HOME\" \"$PWD\""], "envs":{"ORDER":"request"}}})).unwrap();
    let mut stream = rt.start(req, None).await.unwrap();
    start_pid(&mut stream).await;
    assert_eq!(finish(&mut stream).await.0, b"request|internal|/tmp|/tmp");
}

#[tokio::test]
async fn missing_executable_is_wrapper_exit_and_disabled_stdin_is_eof() {
    let rt = runtime();
    let mut stream = rt
        .start(request("/missing-envd-program", &[]), None)
        .await
        .unwrap();
    start_pid(&mut stream).await;
    assert_eq!(finish(&mut stream).await.2.exit_code, 127);
    let mut req = request("/bin/cat", &[]);
    req.stdin = Some(false);
    let mut stream = rt.start(req, None).await.unwrap();
    start_pid(&mut stream).await;
    assert_eq!(finish(&mut stream).await.0, b"");
}

#[tokio::test]
async fn two_live_subscribers_observe_identical_future_bytes() {
    let rt = runtime();
    let mut first = rt
        .start(request("/bin/cat", &[]), Some(Duration::from_secs(5)))
        .await
        .unwrap();
    let pid = start_pid(&mut first).await;
    let mut second = rt.connect(&pid_selector(pid)).await.unwrap();
    start_pid(&mut second).await;
    let bytes = (0..=255).cycle().take(10001).collect::<Vec<u8>>();
    rt.send_input(
        &pid_selector(pid),
        &pb::ProcessInput {
            input: Some(process_input::Input::Stdin(bytes.clone())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    rt.close_stdin(&pid_selector(pid)).await.unwrap();
    let (a, b) = tokio::join!(finish(&mut first), finish(&mut second));
    assert_eq!(a.0, bytes);
    assert_eq!(a, b);
}

#[tokio::test]
async fn retained_tag_reuse_cannot_return_predecessor_exit() {
    let rt = runtime();
    let mut old = rt
        .start(request("/bin/sh", &["-c", "exit 7"]), None)
        .await
        .unwrap();
    start_pid(&mut old).await;
    finish(&mut old).await;
    let mut fresh = rt.start(request("/bin/cat", &[]), None).await.unwrap();
    let pid = start_pid(&mut fresh).await;
    let selector = pb::ProcessSelector {
        selector: Some(process_selector::Selector::Tag("test".into())),
        ..Default::default()
    };
    let mut attached = rt.connect(&selector).await.unwrap();
    assert_eq!(start_pid(&mut attached).await, pid);
    rt.close_stdin(&selector).await.unwrap();
    let (first, second) = tokio::join!(finish(&mut fresh), finish(&mut attached));
    assert_eq!(first.2.exit_code, 0);
    assert_eq!(first, second);
}

#[tokio::test]
async fn terminal_retention_expires_at_thirty_seconds() {
    let rt = runtime();
    let mut stream = rt.start(request("/bin/true", &[]), None).await.unwrap();
    let pid = start_pid(&mut stream).await;
    finish(&mut stream).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(29)).await;
    let mut retained = rt.connect(&pid_selector(pid)).await.unwrap();
    start_pid(&mut retained).await;
    finish(&mut retained).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        rt.connect(&pid_selector(pid)).await,
        Err(RuntimeError::NotFound)
    ));
}

#[tokio::test]
async fn canceled_slow_subscriber_does_not_wedge_remaining_observer() {
    let rt = runtime();
    let mut slow = rt
        .start(request("/bin/cat", &[]), Some(Duration::from_secs(3)))
        .await
        .unwrap();
    let pid = start_pid(&mut slow).await;
    let mut active = rt.connect(&pid_selector(pid)).await.unwrap();
    start_pid(&mut active).await;
    for _ in 0..3 {
        rt.send_input(
            &pid_selector(pid),
            &pb::ProcessInput {
                input: Some(process_input::Input::Stdin(vec![b'x'; 1024])),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    drop(slow);
    rt.close_stdin(&pid_selector(pid)).await.unwrap();
    assert_eq!(finish(&mut active).await.0.len(), 3072);
}

#[tokio::test]
async fn stdin_after_close_and_missing_input_have_official_error_categories() {
    let rt = runtime();
    let mut stream = rt
        .start(
            request("/bin/sh", &["-c", "read line; sleep 0.1"]),
            Some(Duration::from_secs(1)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut stream).await;
    assert!(matches!(
        rt.send_input(&pid_selector(pid), &pb::ProcessInput::default())
            .await,
        Err(RuntimeError::Unimplemented(_))
    ));
    rt.close_stdin(&pid_selector(pid)).await.unwrap();
    assert!(matches!(
        rt.send_input(
            &pid_selector(pid),
            &pb::ProcessInput {
                input: Some(process_input::Input::Stdin(vec![])),
                ..Default::default()
            }
        )
        .await,
        Err(RuntimeError::FailedPrecondition(_))
    ));
    finish(&mut stream).await;
}

#[tokio::test]
async fn regression_slow_observer_connect_cannot_lock_registry() {
    let rt = runtime();
    let mut slow = rt
        .start(
            request(
                "/bin/sh",
                &[
                    "-c",
                    "printf ready; read x; printf second; printf third >&2; read x",
                ],
            ),
            Some(Duration::from_secs(2)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut slow).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    rt.send_input(
        &pid_selector(pid),
        &pb::ProcessInput {
            input: Some(process_input::Input::Stdin(b"go\n".to_vec())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    let other = rt.clone();
    let attach = tokio::spawn(async move { other.connect(&pid_selector(pid)).await });
    tokio::task::yield_now().await;
    // The guarded property is deadlock-freedom, not latency: a connect that
    // serialized the registry behind a slow observer would hold the lock
    // forever, so these operations would never complete and ANY finite bound
    // eventually fails. The former 200 ms windows instead failed on machine
    // speed alone — a fork/exec+reap of /bin/true spikes past 200 ms near
    // load 44 (measured: 331 ms worst spawn+reap under a fork/exec storm,
    // ~10 ms idle) with no registry contention at all. The watchdog bound
    // only has to exceed every plausible host delay, not measure latency.
    let wedge = Duration::from_secs(30);
    let listed = tokio::time::timeout(wedge, rt.list()).await;
    let independent = tokio::time::timeout(wedge, async {
        let mut stream = rt.start(request("/bin/true", &[]), None).await.unwrap();
        start_pid(&mut stream).await;
        finish(&mut stream).await
    })
    .await;
    let signaled = tokio::time::timeout(wedge, rt.send_signal(&pid_selector(pid), 15)).await;
    drop(slow);
    let mut attached = attach.await.unwrap().unwrap();
    start_pid(&mut attached).await;
    finish(&mut attached).await;
    assert!(
        listed.is_ok(),
        "Connect held registry while blocked by slow subscriber"
    );
    assert!(
        listed.unwrap().iter().any(|info| info.pid == pid),
        "list missed the running process"
    );
    assert!(
        independent.is_ok(),
        "slow observer blocked independent Start/reap"
    );
    assert!(
        signaled.is_ok_and(|result| result.is_ok()),
        "slow observer blocked signal"
    );
}

#[tokio::test]
async fn regression_deadline_bounds_descendant_held_output_after_parent_exit() {
    let rt = runtime();
    let mut stream = rt
        .start(
            request("/bin/sh", &["-c", "sleep 2 & printf '%s\\n' $!; exit 0"]),
            Some(Duration::from_millis(100)),
        )
        .await
        .unwrap();
    start_pid(&mut stream).await;
    let result = tokio::time::timeout(Duration::from_millis(500), finish(&mut stream)).await;
    assert!(
        result.is_ok(),
        "parent exited but inherited output pipes defeated deadline"
    );
    let (out, _, end) = result.unwrap();
    let descendant: i32 = String::from_utf8(out).unwrap().trim().parse().unwrap();
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(descendant),
        nix::sys::signal::Signal::SIGKILL,
    );
    assert_eq!(end.exit_code, 0);
    assert!(end.exited);
}

struct ChildCleanup(i32);

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.0),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[tokio::test]
async fn regression_signal_after_reap_does_not_wait_for_descendant_pipes() {
    let rt = runtime();
    let mut stream = rt
        .start(
            request("/bin/sh", &["-c", "sleep 2 & printf '%s\\n' $!; exit 0"]),
            Some(Duration::from_secs(2)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut stream).await;
    let data = stream.recv().await.unwrap();
    let descendant = match data.event.unwrap() {
        process_event::Event::Data(data) => match data.output.unwrap() {
            process_event::data_event::Output::Stdout(bytes) => String::from_utf8(bytes)
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap(),
            output => panic!("unexpected output: {output:?}"),
        },
        event => panic!("unexpected event: {event:?}"),
    };
    let _cleanup = ChildCleanup(descendant);
    tokio::time::timeout(Duration::from_millis(500), async {
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("parent was not reaped");
    assert!(rt.list().await.iter().any(|info| info.pid == pid));
    let result = tokio::time::timeout(
        Duration::from_millis(150),
        rt.send_signal(&pid_selector(pid), 15),
    )
    .await
    .expect("reaped child left a control receiver accepting requests without replies");
    assert!(matches!(result, Err(RuntimeError::NotFound)));
    drop(_cleanup);
    finish(&mut stream).await;
}

#[tokio::test]
async fn regression_deadline_retires_child_while_slow_observer_stays_open() {
    let rt = runtime();
    let mut slow = rt
        .start(
            request("/usr/bin/yes", &["fill-output-queues"]),
            Some(Duration::from_millis(120)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut slow).await;
    // Keep this subscriber alive and deliberately do not consume its output.
    tokio::time::timeout(Duration::from_millis(800), async {
        while rt.list().await.iter().any(|info| info.pid == pid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("output backpressure prevented deadline retirement");
    let mut retained = rt.connect(&pid_selector(pid)).await.unwrap();
    assert_eq!(start_pid(&mut retained).await, pid);
    let (out, err, end) = tokio::time::timeout(Duration::from_millis(150), finish(&mut retained))
        .await
        .expect("retained exit blocked behind the live slow observer");
    assert!(out.is_empty() && err.is_empty());
    assert_eq!(end.status, "signal: killed");
    assert_eq!(end.exit_code, -1);
    // Drop only after List and retained Connect succeeded.
    drop(slow);
}

#[tokio::test]
async fn regression_deadline_retires_exited_child_with_only_pump_blocked() {
    let rt = runtime();
    let mut slow = rt
        .start(
            request("/usr/bin/head", &["-c", "262144", "/dev/zero"]),
            Some(Duration::from_millis(120)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut slow).await;
    tokio::time::timeout(Duration::from_millis(800), async {
        while rt.list().await.iter().any(|info| info.pid == pid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed readers were polled again while retiring a blocked pump");
    let mut retained = rt.connect(&pid_selector(pid)).await.unwrap();
    assert_eq!(start_pid(&mut retained).await, pid);
    let (out, err, end) = finish(&mut retained).await;
    assert!(out.is_empty() && err.is_empty());
    assert_eq!(end.exit_code, 0);
    assert!(end.exited);
    drop(slow);
}

#[tokio::test]
async fn regression_term_handler_output_keeps_pipe_open_until_child_exit() {
    let rt = runtime();
    let mut stream = rt.start(request("/bin/sh", &["-c", "trap 'printf term-output; printf term-error >&2; exit 23' TERM; printf ready; while :; do :; done"]), Some(Duration::from_secs(2))).await.unwrap();
    let pid = start_pid(&mut stream).await;
    assert!(matches!(
        stream.recv().await.unwrap().event,
        Some(process_event::Event::Data(_))
    ));
    rt.send_signal(&pid_selector(pid), 15).await.unwrap();
    let (out, err, end) = finish(&mut stream).await;
    assert_eq!(out, b"term-output");
    assert_eq!(err, b"term-error");
    assert_eq!(end.exit_code, 23);
    assert!(end.exited);
}

fn pty_request(cmd: &str, args: &[&str], cols: u32, rows: u32) -> pb::StartRequest {
    serde_json::from_value(json!({
        "process": {"cmd":cmd,"args":args}, "tag":"pty-test", "stdin":false,
        "pty":{"size":{"cols":cols,"rows":rows}}
    }))
    .unwrap()
}

async fn pty_until(stream: &mut Subscription, expected: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut output = Vec::new();
        loop {
            match stream.recv().await.unwrap().event.unwrap() {
                process_event::Event::Data(data) => match data.output.unwrap() {
                    process_event::data_event::Output::Pty(bytes) => output.extend(bytes),
                    other => panic!("PTY output was misclassified: {other:?}"),
                },
                event => panic!("expected PTY data, got {event:?}"),
            }
            if output
                .windows(expected.len())
                .any(|window| window == expected)
            {
                return output;
            }
        }
    })
    .await
    .expect("PTY output did not arrive")
}

async fn pty_finish(stream: &mut Subscription) -> (Vec<u8>, process_event::EndEvent) {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut output = Vec::new();
        loop {
            match stream
                .recv()
                .await
                .expect("PTY stream lost its End")
                .event
                .unwrap()
            {
                process_event::Event::Data(data) => match data.output.unwrap() {
                    process_event::data_event::Output::Pty(bytes) => output.extend(bytes),
                    other => panic!("PTY output was misclassified: {other:?}"),
                },
                process_event::Event::End(end) => return (output, *end),
                event => panic!("unexpected event: {event:?}"),
            }
        }
    })
    .await
    .expect("PTY did not terminate")
}

#[tokio::test]
async fn pty_stdio_controlling_terminal_input_and_eof() {
    let rt = runtime();
    let mut stream = rt.start(pty_request("/bin/sh", &["-c",
        "test -t 0 && test -t 1 && test -t 2 && : </dev/tty || exit 91; stty -echo; stty size; printf 'stderr-marker\\n' >&2; printf ready; IFS= read -r line; printf 'reply:%s\\n' \"$line\"; cat"
    ], 80, 24), Some(Duration::from_secs(3))).await.unwrap();
    let pid = start_pid(&mut stream).await;
    let selector = pid_selector(pid);
    let out = pty_until(&mut stream, b"ready").await;
    assert!(out.windows(7).any(|bytes| bytes == b"24 80\r\n"));
    assert!(out.windows(15).any(|bytes| bytes == b"stderr-marker\r\n"));
    assert!(rt.list().await.iter().any(|info| info.pid == pid));
    let stdin: pb::ProcessInput = serde_json::from_value(json!({"stdin":"eA=="})).unwrap();
    assert!(matches!(
        rt.send_input(&selector, &stdin).await,
        Err(RuntimeError::FailedPrecondition(_))
    ));
    assert!(matches!(
        rt.close_stdin(&selector).await,
        Err(RuntimeError::FailedPrecondition(_))
    ));
    let input = pb::ProcessInput {
        input: Some(process_input::Input::Pty(
            "literal 雪 ; $()\n".as_bytes().to_vec(),
        )),
        ..Default::default()
    };
    rt.send_input(&selector, &input).await.unwrap();
    let out = pty_until(&mut stream, "reply:literal 雪 ; $()\r\n".as_bytes()).await;
    assert_eq!(out, "reply:literal 雪 ; $()\r\n".as_bytes());
    rt.send_input(
        &selector,
        &pb::ProcessInput {
            input: Some(process_input::Input::Pty(vec![4])),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_, end) = pty_finish(&mut stream).await;
    assert_eq!(end.exit_code, 0);
    let mut retained = rt.connect(&selector).await.unwrap();
    assert_eq!(start_pid(&mut retained).await, pid);
    let (out, end) = pty_finish(&mut retained).await;
    assert!(out.is_empty());
    assert_eq!(end.exit_code, 0);
}

#[tokio::test]
async fn pty_disconnect_connect_and_signal_preserve_child() {
    let rt = runtime();
    let mut stream = rt
        .start(
            pty_request(
                "/bin/sh",
                &[
                    "-c",
                    "stty -echo; printf old; read line; printf new; read line",
                ],
                0,
                0,
            ),
            Some(Duration::from_secs(3)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut stream).await;
    assert_eq!(pty_until(&mut stream, b"old").await, b"old");
    drop(stream);
    let selector = pid_selector(pid);
    let mut connected = rt.connect(&selector).await.unwrap();
    assert_eq!(start_pid(&mut connected).await, pid);
    rt.send_input(
        &selector,
        &pb::ProcessInput {
            input: Some(process_input::Input::Pty(vec![b'\n'])),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(pty_until(&mut connected, b"new").await, b"new");
    rt.send_signal(&selector, 9).await.unwrap();
    let (_, end) = pty_finish(&mut connected).await;
    assert_eq!(end.status, "signal: killed");
}

#[tokio::test]
async fn pty_deadline_retires_with_unconsumed_live_subscription() {
    let rt = runtime();
    let mut slow = rt
        .start(
            pty_request("/usr/bin/yes", &["pty-pressure"], 80, 24),
            Some(Duration::from_millis(120)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut slow).await;
    tokio::time::timeout(Duration::from_millis(800), async {
        while rt.list().await.iter().any(|info| info.pid == pid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("PTY backpressure blocked deadline retirement");
    let mut retained = rt.connect(&pid_selector(pid)).await.unwrap();
    assert_eq!(start_pid(&mut retained).await, pid);
    let (out, end) = pty_finish(&mut retained).await;
    assert!(out.is_empty());
    assert_eq!(end.status, "signal: killed");
    drop(slow);
}

#[tokio::test]
async fn pty_resize_casts_size_and_delivers_sigwinch() {
    let rt = runtime();
    for (cols, rows) in [(0, 0), (u32::MAX, u32::MAX), (65536 + 111, 65536 + 37)] {
        let mut stream = rt.start(pty_request("/bin/sh", &["-c",
            "stty -echo; trap 'stty size; printf resized; exit 42' WINCH; stty size; printf ready; while :; do :; done"
        ], 65536 + 80, 65536 + 24), Some(Duration::from_secs(3))).await.unwrap();
        let pid = start_pid(&mut stream).await;
        assert_eq!(pty_until(&mut stream, b"ready").await, b"24 80\r\nready");
        let noop: pb::UpdateRequest =
            serde_json::from_value(json!({"process":{"pid":pid}})).unwrap();
        rt.update(&noop).await.unwrap();
        let update: pb::UpdateRequest = serde_json::from_value(
            json!({"process":{"pid":pid}, "pty":{"size":{"cols":cols,"rows":rows}}}),
        )
        .unwrap();
        rt.update(&update).await.unwrap();
        let (out, end) = pty_finish(&mut stream).await;
        assert_eq!(
            out,
            format!("{} {}\r\nresized", rows as u16, cols as u16).as_bytes()
        );
        assert_eq!(end.exit_code, 42);
        assert!(matches!(
            rt.update(&noop).await,
            Err(RuntimeError::NotFound)
        ));
    }
}

#[tokio::test]
async fn pty_missing_size_large_binary_input_and_update_errors() {
    let rt = runtime();
    let mut req = pty_request(
        "/bin/sh",
        &[
            "-c",
            "stty raw -echo; stty size; printf ready; exec head -c 10001",
        ],
        0,
        0,
    );
    req.pty = serde_json::from_value::<pb::StartRequest>(json!({"pty":{}}))
        .unwrap()
        .pty;
    let mut stream = rt.start(req, Some(Duration::from_secs(3))).await.unwrap();
    let pid = start_pid(&mut stream).await;
    assert_eq!(pty_until(&mut stream, b"ready").await, b"0 0\nready");
    let bytes: Vec<u8> = (0..10001).map(|index| (index % 256) as u8).collect();
    rt.send_input(
        &pid_selector(pid),
        &pb::ProcessInput {
            input: Some(process_input::Input::Pty(Vec::new())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    rt.send_input(
        &pid_selector(pid),
        &pb::ProcessInput {
            input: Some(process_input::Input::Pty(bytes.clone())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (out, end) = pty_finish(&mut stream).await;
    assert_eq!(out, bytes);
    assert_eq!(end.exit_code, 0);
    let mut plain = rt
        .start(request("/bin/cat", &[]), Some(Duration::from_secs(3)))
        .await
        .unwrap();
    let pid = start_pid(&mut plain).await;
    rt.update(&serde_json::from_value(json!({"process":{"pid":pid}})).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        rt.update(&serde_json::from_value(json!({"process":{"pid":pid},"pty":{}})).unwrap())
            .await,
        Err(RuntimeError::Internal(_))
    ));
    rt.close_stdin(&pid_selector(pid)).await.unwrap();
    finish(&mut plain).await;
    let invalid: pb::StartRequest = serde_json::from_value(json!({
        "process":{"cmd":"/bin/true","cwd":"/definitely/absent/envd"},"pty":{}
    }))
    .unwrap();
    assert!(matches!(
        rt.start(invalid, None).await,
        Err(RuntimeError::InvalidArgument(_))
    ));
}

#[tokio::test]
async fn regression_pty_exit_cancels_blocked_and_queued_input() {
    let rt = runtime();
    let mut stream = rt
        .start(
            pty_request(
                "/bin/sh",
                &[
                    "-c",
                    "stty raw -echo; trap '' HUP; sleep 2 & printf '%s:ready' $!; wait",
                ],
                80,
                24,
            ),
            Some(Duration::from_millis(250)),
        )
        .await
        .unwrap();
    let pid = start_pid(&mut stream).await;
    let ready = pty_until(&mut stream, b":ready").await;
    let descendant: i32 = std::str::from_utf8(&ready)
        .unwrap()
        .split(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let _cleanup = ChildCleanup(descendant);
    let mut writers = Vec::new();
    for size in [4 * 1024 * 1024, 1] {
        let rt = rt.clone();
        writers.push(tokio::spawn(async move {
            rt.send_input(
                &pid_selector(pid),
                &pb::ProcessInput {
                    input: Some(process_input::Input::Pty(vec![b'x'; size])),
                    ..Default::default()
                },
            )
            .await
        }));
        // Poll each sender into its actual readiness/serialization wait.
        tokio::task::yield_now().await;
    }
    assert!(writers.iter().all(|writer| !writer.is_finished()));
    let (_, end) = pty_finish(&mut stream).await;
    assert_eq!(end.status, "signal: killed");
    assert!(!rt.list().await.iter().any(|info| info.pid == pid));
    for writer in writers {
        let result = tokio::time::timeout(Duration::from_millis(200), writer)
            .await
            .expect("terminal exit did not release blocked/queued PTY input")
            .unwrap();
        assert!(matches!(result, Err(RuntimeError::FailedPrecondition(_))));
    }
    let mut retained = rt.connect(&pid_selector(pid)).await.unwrap();
    assert_eq!(start_pid(&mut retained).await, pid);
    assert_eq!(pty_finish(&mut retained).await.1.status, "signal: killed");
    drop(_cleanup);
}

#[tokio::test]
async fn invalid_signal_names_the_oracle_enum_text() {
    // Oracle signal.go:20-26 — only SIGTERM/SIGKILL deliver; every other
    // value refuses with 501 "invalid signal: %s", where %s is the proto
    // enum's String(): the name for known values, the number otherwise.
    let rt = runtime();
    let mut stream = rt.start(request("/bin/sleep", &["5"]), None).await.unwrap();
    let pid = start_pid(&mut stream).await;
    let selector = pid_selector(pid);
    let omitted = rt.send_signal(&selector, 0).await.unwrap_err();
    assert!(matches!(omitted, RuntimeError::Unimplemented(_)));
    assert_eq!(omitted.to_string(), "invalid signal: SIGNAL_UNSPECIFIED");
    let unknown = rt.send_signal(&selector, 99).await.unwrap_err();
    assert!(matches!(unknown, RuntimeError::Unimplemented(_)));
    assert_eq!(unknown.to_string(), "invalid signal: 99");
    rt.send_signal(&selector, 15).await.unwrap();
}

#[tokio::test]
async fn an_unset_selector_refuses_with_the_oracle_type_text() {
    // Oracle service.go:171-200 getProcess default branch — with the oneof
    // unset, %T names the concrete pointer type, so every operation that
    // resolves a selector (SendSignal included) refuses 501 with
    // "invalid input type *process.ProcessSelector".
    let rt = runtime();
    let unset = pb::ProcessSelector::default();
    let error = rt.send_signal(&unset, 15).await.unwrap_err();
    assert!(matches!(error, RuntimeError::Unimplemented(_)));
    assert_eq!(
        error.to_string(),
        "invalid input type *process.ProcessSelector"
    );
}

#[tokio::test]
async fn a_missing_cwd_refuses_with_the_oracle_bespoke_text() {
    // Oracle handler.go:244-247: the cwd existence check only special-cases
    // ErrNotExist and returns the bespoke invalid-argument message
    // "cwd '<resolved>' does not exist" — not the raw stat error.
    let rt = runtime();
    let request: pb::StartRequest = serde_json::from_value(json!({
        "process": {"cmd":"/bin/sleep","args":["5"],"cwd":"/tmp/process-runtime-missing-cwd"},
        "tag":"test"
    }))
    .unwrap();
    let error = rt
        .start(request, None)
        .await
        .err()
        .expect("a missing cwd must refuse");
    assert!(
        matches!(error, RuntimeError::InvalidArgument(_)),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "cwd '/tmp/process-runtime-missing-cwd' does not exist"
    );
}

#[tokio::test]
async fn a_spawn_failure_carries_the_oracle_wrapped_start_error() {
    // Oracle handler.go:593 + start.go:184: the spawn failure is wrapped with
    // the user command ("error starting process '%s': %w") and that wrapped
    // message is what the client sees on the wire; the code stays
    // StartErrorCode's (start_error.go:37-39 default, InvalidArgument). The
    // %w tail is os/exec's FULL *fs.PathError — `fork/exec <resolved-path>:
    // <errno text>` (os/exec_posix.go:61; the oracle's own eagainStartError
    // pins the exact shape, start_error_test.go:18-21) — with /bin/sh
    // constant because handler.go:212-214 commands an absolute /bin/sh.
    let rt = runtime();
    let dir = tempfile::tempdir().unwrap();
    // A directory without the search bit denies the child's chdir at spawn
    // time, producing a deterministic EACCES spawn failure.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let request: pb::StartRequest = serde_json::from_value(json!({
        "process": {"cmd":"/bin/sleep","args":["5"],"cwd":dir.path()},
        "tag":"test"
    }))
    .unwrap();
    let error = rt
        .start(request, None)
        .await
        .err()
        .expect("a denied cwd must refuse the spawn");
    assert!(
        matches!(error, RuntimeError::InvalidArgument(_)),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "error starting process '/bin/sleep 5': fork/exec /bin/sh: permission denied"
    );
}

#[tokio::test]
async fn a_non_enoent_stat_failure_falls_through_to_the_wrapped_spawn_text() {
    // Oracle handler.go:244-247 intercepts ONLY ErrNotExist — there is no
    // else branch. A stat failure on another errno (here EACCES: the resolved
    // cwd sits inside a directory without the search bit) falls through to
    // the spawn, whose child chdir fails, and the client sees the WRAPPED
    // fork/exec text — never a bare stat error.
    let rt = runtime();
    let nox = tempfile::tempdir().unwrap();
    let inner = nox.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(nox.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let request: pb::StartRequest = serde_json::from_value(json!({
        "process": {"cmd":"/bin/sleep","args":["5"],"cwd":inner},
        "tag":"test"
    }))
    .unwrap();
    let error = rt
        .start(request, None)
        .await
        .err()
        .expect("a stat-blocked cwd must refuse the spawn");
    assert!(
        matches!(error, RuntimeError::InvalidArgument(_)),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "error starting process '/bin/sleep 5': fork/exec /bin/sh: permission denied"
    );
}

/// The rlimit table is process-wide, and tests within one binary run in
/// parallel threads — so the EMFILE window must run in a dedicated child
/// process (the tests/mmds.rs self-exec precedent) or it would starve every
/// sibling test's spawns.
const PTY_EMFILE_CHILD: &str = "ENVD_PROCESS_RUNTIME_PTY_EMFILE_CHILD";

#[tokio::test]
async fn a_pty_setup_failure_carries_the_oracle_pty_wrap() {
    // Oracle handler.go:326-328: a PTY spawn failure wraps with the user
    // command, the resolved dir and the requested window size, and
    // start_error.go:24-31 codes EMFILE as ResourceExhausted. Exhausting the
    // descriptor table makes posix_openpt fail with EMFILE deterministically,
    // and creack/pty v1.1.24's open() is the os.OpenFile("/dev/ptmx") PathError
    // (pty_linux.go:14), so the wrap tail carries `open /dev/ptmx: <errno>`.
    if std::env::var(PTY_EMFILE_CHILD).is_ok() {
        let rt = runtime();
        // SAFETY: scalar getrlimit over a local struct.
        let mut old = nix::libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut old) };
        let low = nix::libc::rlimit {
            rlim_cur: 3,
            rlim_max: old.rlim_max,
        };
        // SAFETY: scalar setrlimit over a local struct pointer; this process
        // exists only to run this one assertion under the low table.
        unsafe { nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &low) };
        let error = rt
            .start(pty_request("/bin/sleep", &["5"], 80, 24), None)
            .await
            .err()
            .expect("an fd-exhausted pty setup must refuse");
        assert!(
            matches!(error, RuntimeError::ResourceExhausted(_)),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "error starting pty with command '/bin/sleep 5' in dir '/tmp' \
             with '80' cols and '24' rows: open /dev/ptmx: too many open files"
        );
        return;
    }
    let status = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_pty_setup_failure_carries_the_oracle_pty_wrap",
            "--nocapture",
        ])
        .env(PTY_EMFILE_CHILD, "1")
        .status()
        .await
        .unwrap();
    assert!(
        status.success(),
        "the isolated EMFILE child failed: {status}"
    );
}

#[tokio::test]
async fn an_already_elapsed_request_timeout_refuses_pre_fork_with_the_oracle_deadline_text() {
    // Oracle start.go:34/:40-44 arms procCtx from connect-timeout-ms and
    // exec.CommandContext (handler.go:220) fails the spawn pre-fork with
    // context.DeadlineExceeded when it has already elapsed; start.go:184
    // wraps it and StartErrorCode (start_error.go:32-34) codes it
    // DeadlineExceeded — no child is ever forked.
    let rt = runtime();
    let error = rt
        .start(
            request("/bin/sleep", &["5"]),
            Some(Duration::from_millis(0)),
        )
        .await
        .err()
        .expect("an elapsed timeout must refuse the start pre-fork");
    assert_eq!(
        error.to_string(),
        "error starting process '/bin/sleep 5': context deadline exceeded"
    );
    assert!(
        rt.list().await.is_empty(),
        "no child is adopted behind a pre-fork refusal"
    );
}

#[tokio::test]
async fn start_refuses_with_the_oracle_frozen_message_while_the_barrier_is_up() {
    // Oracle spawn.go + handler.go:593 + start_error.go: a start during a
    // freeze barrier refuses with CodeUnavailable and the wrapped text
    // "error starting process '<cmd>': the target cgroup is frozen". The
    // no-op manager keeps the barrier semantics: a freeze holds the spawn
    // barrier until the thaw even where no cgroup can be written.
    use coorworker_envd::cgroups::freeze::FreezeOptions;
    use coorworker_envd::cgroups::freeze::WorkloadFreezer;
    use coorworker_envd::cgroups::noop::NoopManager;
    use std::sync::Arc;
    let freezer = Arc::new(WorkloadFreezer::new(Arc::new(NoopManager)));
    let rt = runtime().with_freezer(freezer.clone());
    freezer.freeze(FreezeOptions::default()).await;
    let error = rt
        .start(request("/bin/sleep", &["5"]), None)
        .await
        .err()
        .expect("a frozen barrier must refuse the start");
    assert!(matches!(error, RuntimeError::Unavailable(_)));
    assert_eq!(
        error.to_string(),
        "error starting process '/bin/sleep 5': the target cgroup is frozen"
    );
    // A spawn after the thaw is admitted again.
    freezer.unfreeze().await.unwrap();
    let mut stream = rt.start(request("/bin/sleep", &["5"]), None).await.unwrap();
    let pid = start_pid(&mut stream).await;
    rt.send_signal(&pid_selector(pid), 9).await.unwrap();
}

#[tokio::test]
async fn a_pty_start_refusal_uses_the_oracle_pty_shape() {
    // Oracle handler.go:317-328: a PTY spawn failure (the admission refusal
    // included) is wrapped with the user command, the RESOLVED dir, and the
    // requested window size, so the wire text differs from a plain start's
    // handler.go:593 shape. Probed in a real guest: the refused PTY start
    // into a host-frozen cgroup carried exactly this message.
    use coorworker_envd::cgroups::freeze::FreezeOptions;
    use coorworker_envd::cgroups::freeze::WorkloadFreezer;
    use coorworker_envd::cgroups::noop::NoopManager;
    use std::sync::Arc;
    let freezer = Arc::new(WorkloadFreezer::new(Arc::new(NoopManager)));
    let rt = runtime().with_freezer(freezer.clone());
    freezer.freeze(FreezeOptions::default()).await;
    let error = rt
        .start(pty_request("/bin/sleep", &["5"], 80, 24), None)
        .await
        .err()
        .expect("a frozen barrier must refuse the pty start");
    assert!(matches!(error, RuntimeError::Unavailable(_)), "{error:?}");
    assert_eq!(
        error.to_string(),
        "error starting pty with command '/bin/sleep 5' in dir '/tmp' \
         with '80' cols and '24' rows: the target cgroup is frozen"
    );
    freezer.unfreeze().await.unwrap();
}

#[tokio::test]
async fn start_places_the_child_into_the_admitted_cgroup() {
    // Oracle handler.go:231-242 (UseCgroupFD, applied by cgroupfd_linux.go)
    // and cgroup2.go:152: the child joins its process-type cgroup at spawn
    // time through the directory fd the manager hands out, so no process is
    // ever observable outside its cgroup. The fake manager below hands out a
    // REAL directory fd, so a pre-exec join (openat "cgroup.procs", write
    // "0") lands in a file the test can read back.
    use coorworker_envd::cgroups::freeze::WorkloadFreezer;
    use coorworker_envd::cgroups::{CgroupError, Manager, PathManager, ProcessType};
    use std::os::unix::io::AsRawFd;
    use std::sync::Arc;

    struct FdManager {
        _dir: std::fs::File,
        fd: i32,
    }
    impl Manager for FdManager {
        fn get_file_descriptor(&self, _process_type: ProcessType) -> Option<i32> {
            Some(self.fd)
        }
        fn freeze(&self, _process_type: ProcessType) -> Result<(), CgroupError> {
            Ok(())
        }
        fn unfreeze(&self, _process_type: ProcessType) -> Result<(), CgroupError> {
            Ok(())
        }
        fn frozen(&self, _process_type: ProcessType) -> Result<bool, CgroupError> {
            Ok(false)
        }
        fn close(&self) -> Result<(), CgroupError> {
            Ok(())
        }
        fn as_path_manager(&self) -> Option<&dyn PathManager> {
            None
        }
    }

    let dir = tempfile::TempDir::new().unwrap();
    let procs = dir.path().join("cgroup.procs");
    std::fs::write(&procs, b"").unwrap();
    let handle = std::fs::File::open(dir.path()).unwrap();
    let fd = handle.as_raw_fd();
    let freezer = Arc::new(WorkloadFreezer::new(Arc::new(FdManager {
        _dir: handle,
        fd,
    })));
    let rt = runtime().with_freezer(freezer);
    let mut stream = rt.start(request("/bin/true", &[]), None).await.unwrap();
    let _pid = start_pid(&mut stream).await;
    let (_, stderr, end) = finish(&mut stream).await;
    assert_eq!(end.exit_code, 0, "{stderr:?}");
    assert_eq!(
        std::fs::read_to_string(&procs).unwrap(),
        "0",
        "the child must write itself into the admitted cgroup before exec"
    );
}

/// Oracle start.go:40-44 arms `procCtx` from connect-timeout-ms and Go
/// os/exec CommandContext kills the child with SIGKILL when it expires;
/// ProcessState.String() then reports "signal: killed".
#[tokio::test]
async fn deadline_kills_a_live_child_and_reports_go_signal_text() {
    let rt = runtime();
    let mut stream = rt
        .start(
            request("/bin/sleep", &["10"]),
            Some(Duration::from_millis(100)),
        )
        .await
        .unwrap();
    start_pid(&mut stream).await;
    let (_, stderr, end) = tokio::time::timeout(Duration::from_secs(5), finish(&mut stream))
        .await
        .expect("deadline must terminate the live child");
    assert_eq!(end.status, "signal: killed", "{stderr:?}");
    assert_eq!(end.exit_code, -1);
    assert!(!end.exited);
    assert_eq!(end.error.as_deref(), Some("signal: killed"));
}
