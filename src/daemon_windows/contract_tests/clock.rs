//! Clock slice of `POST /init` on Windows: the handler-delay correction
//! header, the newest-wins gate, and the `SetSystemTime` step.
//!
//! Same contract as `tests/clock.rs` on Linux (pinned init.go:41-42, :250,
//! :315-326, :336, :433-443, :1089-1091, utils/atomic.go:16-26): a corrected
//! timestamp rides `X-Envd-Init-Clock-Correction`, only a corrected value at
//! least as new as the last accepted one reaches SetData, and a stale replay
//! still answers 204. The platform difference is the sink: these run against
//! the recorded sink through `Daemon::with_set_system_time`, never the real
//! `SetSystemTime`, so the suite never steps the machine it runs on.
use super::*;

const CORRECTION: &str = "x-envd-init-clock-correction";

#[derive(Clone, Default)]
struct Steps(Arc<std::sync::Mutex<Vec<i64>>>);

impl Steps {
    fn sink(self) -> crate::clock::SetSystemTimeFn {
        Arc::new(move |nanos| {
            self.0.lock().unwrap().push(nanos);
            Ok(())
        })
    }

    fn failing_sink(self) -> crate::clock::SetSystemTimeFn {
        Arc::new(move |nanos| {
            self.0.lock().unwrap().push(nanos);
            Err("SetSystemTime: access denied".to_owned())
        })
    }

    fn calls(&self) -> Vec<i64> {
        self.0.lock().unwrap().clone()
    }
}

fn clock_daemon(
    now: crate::clock::NowFn,
    sink: crate::clock::SetSystemTimeFn,
) -> std::sync::Arc<Daemon> {
    Arc::new(Daemon::new().with_now(now).with_set_system_time(sink))
}

/// RFC3339 with nanosecond precision, the format the pinned `time.Time`
/// decode accepts; the inverse of `clock::rfc3339_nanos` for the test's own
/// range.
fn rfc(nanos: i128) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let sub = nanos.rem_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{sub:09}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

async fn init(address: SocketAddr, body: impl Into<String>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let body = body.into();
    send(
        address,
        Method::POST,
        "/init",
        &[("content-type", "application/json")],
        body.as_bytes(),
    )
    .await
}

async fn envs_at(address: SocketAddr, token: Option<&str>) -> Value {
    let headers: Vec<(&str, &str)> = token
        .map(|token| vec![("x-access-token", token)])
        .unwrap_or_default();
    let (status, _, body) = send(address, Method::GET, "/envs", &headers, b"").await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_slice(&body).expect("env json")
}

#[tokio::test]
async fn no_timestamp_has_no_correction_header_and_no_step() {
    let steps = Steps::default();
    let running = start_daemon(clock_daemon(
        Arc::new(crate::clock::real_now_nanos),
        steps.clone().sink(),
    ))
    .await;
    let (status, headers, _) = init(running.address, r#"{"envVars":{"MARK":"A"}}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!headers.contains_key(CORRECTION));
    assert!(steps.calls().is_empty());
    assert_eq!(envs_at(running.address, None).await["MARK"], "A");
}

#[tokio::test]
async fn timestamp_within_tolerance_steps_nothing() {
    let steps = Steps::default();
    let now = crate::clock::real_now_nanos();
    let running = start_daemon(clock_daemon(
        Arc::new(crate::clock::real_now_nanos),
        steps.clone().sink(),
    ))
    .await;
    // Four seconds behind is inside the 50 ms past band with a full second of
    // runtime slack; the 50 ms edge is pinned at the engine level instead,
    // exactly as the oracle keeps boundary rows out of its handler tests.
    let (status, headers, _) = init(
        running.address,
        format!(r#"{{"timestamp":"{}"}}"#, rfc(now - 4_000_000_000)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.contains_key(CORRECTION));
    assert!(steps.calls().is_empty(), "{:?}", steps.calls());
}

#[tokio::test]
async fn timestamp_ahead_beyond_tolerance_steps_to_the_corrected_target() {
    let steps = Steps::default();
    let now = crate::clock::real_now_nanos();
    let ts = now + 10_000_000_000;
    let running = start_daemon(clock_daemon(
        Arc::new(crate::clock::real_now_nanos),
        steps.clone().sink(),
    ))
    .await;
    let (status, headers, _) =
        init(running.address, format!(r#"{{"timestamp":"{}"}}"#, rfc(ts))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.contains_key(CORRECTION));
    let calls = steps.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    // The step target is the corrected timestamp: ts plus this handler's own
    // delay, small but non-negative here.
    assert!(
        i128::from(calls[0]) >= ts && i128::from(calls[0]) <= ts + 250_000_000,
        "target {} not in [{ts}, {}]",
        calls[0],
        ts + 250_000_000
    );
}

#[tokio::test]
async fn a_failed_step_is_log_only_and_still_applies_set_data() {
    let steps = Steps::default();
    let now = crate::clock::real_now_nanos();
    let running = start_daemon(clock_daemon(
        Arc::new(crate::clock::real_now_nanos),
        steps.clone().failing_sink(),
    ))
    .await;
    let (status, _, _) = init(
        running.address,
        format!(
            r#"{{"timestamp":"{}","envVars":{{"MARK":"A"}}}}"#,
            rfc(now + 10_000_000_000)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(steps.calls().len(), 1, "the step was attempted");
    assert_eq!(envs_at(running.address, None).await["MARK"], "A");
}

#[tokio::test]
async fn an_older_corrected_timestamp_is_refused_and_skips_set_data() {
    let steps = Steps::default();
    let now = crate::clock::real_now_nanos();
    let ts = now + 3_600_000_000_000;
    // A constant injected now makes every correction exactly zero, so the
    // equal-timestamp acceptance is deterministic (real-now jitter between
    // inits would otherwise flip the newest-wins compare).
    let running = start_daemon(clock_daemon(
        Arc::new(|| 1_800_000_000_000_000_000i128),
        steps.clone().sink(),
    ))
    .await;
    let (status, _, _) = init(
        running.address,
        format!(
            r#"{{"timestamp":"{}","envVars":{{"MARK":"A"}},"accessToken":"t1"}}"#,
            rfc(ts)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(steps.calls().len(), 1);

    // A corrected timestamp older than the last accepted one is refused: 204
    // with the header, but SetData (envs, token, step) never runs. A genuine
    // replay carries the same token; a different one would fail auth before
    // the gate.
    let (status, headers, _) = init(
        running.address,
        format!(
            r#"{{"timestamp":"{}","envVars":{{"MARK":"B"}},"accessToken":"t1"}}"#,
            rfc(ts - 1_000_000_000)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.contains_key(CORRECTION));
    assert_eq!(steps.calls().len(), 1, "a refused init must not step");
    assert_eq!(
        envs_at(running.address, Some("t1")).await["MARK"],
        "A",
        "a refused init must not replace envs or the token"
    );

    // A corrected timestamp equal to the last accepted one is accepted.
    let (status, _, _) = init(
        running.address,
        format!(
            r#"{{"timestamp":"{}","envVars":{{"MARK":"B"}},"accessToken":"t1"}}"#,
            rfc(ts)
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(steps.calls().len(), 2, "an equal corrected timestamp steps");
    assert_eq!(envs_at(running.address, Some("t1")).await["MARK"], "B");
}

#[tokio::test]
async fn the_correction_header_truncates_whole_milliseconds() {
    // Injected now: body read at T1, correction measured at T2. Go answers
    // correction.Milliseconds() (truncated toward zero) and steps to
    // timestamp + correction exactly.
    let steps = Steps::default();
    let base = 1_800_000_000_000_000_000i128;
    let delay = 300_900_000i128; // 300 ms + 900 us
    let now: crate::clock::NowFn = {
        let call = std::sync::Mutex::new(0u32);
        Arc::new(move || {
            let mut call = call.lock().unwrap();
            *call += 1;
            if *call == 1 {
                base
            } else {
                base + delay
            }
        })
    };
    let ts = crate::clock::real_now_nanos() + 3_600_000_000_000;
    let running = start_daemon(clock_daemon(now, steps.clone().sink())).await;
    let (status, headers, _) =
        init(running.address, format!(r#"{{"timestamp":"{}"}}"#, rfc(ts))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[CORRECTION], "300");
    assert_eq!(
        steps.calls(),
        vec![(ts + delay) as i64],
        "target is ts + correction"
    );

    // A negative correction truncates toward zero: -900 us answers "0".
    let steps = Steps::default();
    let negative: crate::clock::NowFn = {
        let call = std::sync::Mutex::new(0u32);
        Arc::new(move || {
            let mut call = call.lock().unwrap();
            *call += 1;
            if *call == 1 {
                base
            } else {
                base - 900_000
            }
        })
    };
    let running = start_daemon(clock_daemon(negative, steps.clone().sink())).await;
    let (status, headers, _) =
        init(running.address, format!(r#"{{"timestamp":"{}"}}"#, rfc(ts))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers[CORRECTION], "0");
}

#[tokio::test]
async fn a_malformed_timestamp_rejects_the_whole_init() {
    let steps = Steps::default();
    let running = start_daemon(clock_daemon(
        Arc::new(crate::clock::real_now_nanos),
        steps.clone().sink(),
    ))
    .await;
    for body in [
        r#"{"timestamp":"2026-10-31t01:23:45z"}"#,
        r#"{"timestamp":"2026-10-31T01:23:60Z"}"#,
        r#"{"timestamp":12345}"#,
        r#"{"timestamp":"2026-10-31T01:23:45.Z"}"#,
    ] {
        let (status, headers, _) = init(running.address, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!headers.contains_key(CORRECTION), "{body}");
    }
    assert!(steps.calls().is_empty());
}

#[tokio::test]
async fn an_escaped_json_timestamp_is_a_decode_error_like_go() {
    // Go decodes `timestamp` as *time.Time, and encoding/json hands
    // UnmarshalJSON the RAW bytes of the value: escape sequences are not
    // decoded before the RFC3339 parse, so any "\u..." fails it. Probed
    // against the pinned oracle in a real guest.
    let steps = Steps::default();
    let running = start_daemon(clock_daemon(
        Arc::new(crate::clock::real_now_nanos),
        steps.clone().sink(),
    ))
    .await;
    for body in [
        r#"{"timestamp":"2026-10-31T01:23:45\u002e5Z"}"#,
        r#"{"timestamp":"\u0032\u0030\u0032\u0036-10-31T01:23:45.5Z"}"#,
    ] {
        let (status, _, _) = init(running.address, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    assert!(steps.calls().is_empty(), "{:?}", steps.calls());
    let (status, headers, _) =
        init(running.address, r#"{"timestamp":"2026-10-31T01:23:45.5Z"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.contains_key(CORRECTION));
}

#[tokio::test]
async fn pre_epoch_and_wrapped_timestamps_refuse_at_the_gate() {
    // The newest-wins gate starts at 0, so a corrected timestamp whose i64
    // UnixNano is negative is refused: year 0001 directly, and year 9999
    // through Go's wrapped UnixNano reduction. Both still answer the header
    // and stay 204 with SetData skipped.
    let steps = Steps::default();
    let running = start_daemon(clock_daemon(
        Arc::new(|| 1_800_000_000_000_000_000i128),
        steps.clone().sink(),
    ))
    .await;
    for body in [
        r#"{"timestamp":"0001-01-01T00:00:00Z","envVars":{"MARK":"A"}}"#,
        r#"{"timestamp":"9999-12-31T23:59:59Z","envVars":{"MARK":"B"}}"#,
    ] {
        let (status, headers, _) = init(running.address, body).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(headers.contains_key(CORRECTION), "{body}");
    }
    assert!(steps.calls().is_empty(), "{:?}", steps.calls());
    let values = envs_at(running.address, None).await;
    assert!(values.get("MARK").is_none(), "{values}");
}

#[tokio::test]
async fn a_body_go_rejects_is_rejected_even_for_unused_fields() {
    // Go decodes the whole body before the handler runs, so a wrong-typed
    // field fails /init with 400 even when this daemon ignores its effect.
    let running = start().await;
    for body in [
        r#"{"cpuCount":"x"}"#,
        r#"{"cpuCount":1.5}"#,
        r#"{"volumeMounts":"x"}"#,
        r#"{"volumeMounts":[{"nfs_target":1}]}"#,
        r#"{"caBundle":5}"#,
        r#"{"lifecycleID":{}}"#,
        // A SecureToken is parsed from its raw bytes, so an escape sequence
        // fails it (the Linux decoder rejects the same body).
        r#"{"accessToken":"a\u002db"}"#,
    ] {
        let (status, _, _) = init(running.address, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    // Go maps an empty body, and a literal JSON null, to the zero struct.
    for body in ["", "null"] {
        let (status, _, _) = init(running.address, body).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body:?}");
    }
}

#[tokio::test]
async fn a_guest_shaped_daemon_without_a_sink_skips_the_step() {
    // Development daemons take no sink, so a timestamped /init answers its
    // correction header and still advances the gate, with no step — the same
    // observable end state as Go dev failing the step with a log line.
    let ts = crate::clock::real_now_nanos() + 3_600_000_000_000;
    let running = start_daemon(Arc::new(
        Daemon::new().with_now(Arc::new(|| 1_800_000_000_000_000_000i128)),
    ))
    .await;
    let first = init(
        running.address,
        format!(r#"{{"timestamp":"{}","envVars":{{"MARK":"A"}}}}"#, rfc(ts)),
    )
    .await;
    assert_eq!(first.0, StatusCode::NO_CONTENT);
    assert!(first.1.contains_key(CORRECTION));
    // An equal corrected timestamp is still accepted by the gate.
    let (status, _, _) = init(
        running.address,
        format!(r#"{{"timestamp":"{}","envVars":{{"MARK":"B"}}}}"#, rfc(ts)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(envs_at(running.address, None).await["MARK"], "B");
}
