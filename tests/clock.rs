//! Clock slice: /init timestamp correction header, newest-wins gate, and the
//! system-clock step. Contract: pinned init.go:41-42, :194, :250, :315-326,
//! :336, :433-443, :1089-1091, clock_linux.go:11-15, store.go:56-61/:164-166,
//! utils/atomic.go:16-26, init_test.go:68-135; Go acceptance set probed with
//! time.Time JSON decode and x/sys v0.49.0 NsecToTimespec (values inline).

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::clock::{
    go_timespec_parts, real_now_nanos, rfc3339_nanos, should_set_system_time,
};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

async fn init_response(router: axum::Router, body: &str) -> axum::http::Response<Body> {
    router
        .oneshot(
            Request::post("/init")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn init_with(router: axum::Router, body: &str) -> StatusCode {
    init_response(router, body).await.status()
}

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

fn header_of(response: &axum::http::Response<Body>) -> impl Fn(&str) -> Option<String> + '_ {
    move |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }
}

#[derive(Clone, Default)]
struct Steps(Arc<Mutex<Vec<i64>>>);

impl Steps {
    fn sink(self) -> coorworker_envd::clock::SetSystemTimeFn {
        Arc::new(move |nanos| {
            self.0.lock().unwrap().push(nanos);
            Ok(())
        })
    }

    fn failing_sink(self) -> coorworker_envd::clock::SetSystemTimeFn {
        Arc::new(move |nanos| {
            self.0.lock().unwrap().push(nanos);
            Err("clock_settime: operation not permitted".to_owned())
        })
    }

    fn calls(&self) -> Vec<i64> {
        self.0.lock().unwrap().clone()
    }
}

fn clock_daemon(
    now: coorworker_envd::clock::NowFn,
    sink: coorworker_envd::clock::SetSystemTimeFn,
) -> axum::Router {
    use coorworker_envd::daemon::{app, Daemon};
    app(Arc::new(
        Daemon::new().with_now(now).with_set_system_time(sink),
    ))
}

/// RFC3339 with nanosecond precision, the format the pinned time.Time decode
/// accepts; the inverse of `rfc3339_nanos` for the test's own range.
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

/// Hinnant's civil_from_days: days since 1970-01-01 to (year, month, day).
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

// --- engine: bands, decode, timespec, wrap ---

#[test]
fn should_set_system_time_matches_the_go_boundary_table() {
    // Go init_test.go:68-135 rows. The oracle test calls
    // shouldSetSystemTime(tt.hostTime, sandboxTime): the delta-shifted value
    // lands in the sandbox argument, so a negative delta is a guest clock
    // BEHIND the timestamp (step forward) and a positive one ahead.
    let base = 1_800_000_000_123_456_789i128;
    let rows: &[(i128, bool)] = &[
        (-10_000_000_000, true), // guest 10s behind the timestamp: set
        (-50_000_000, false),    // guest 50ms behind: at maxTimeInPast boundary
        (-40_000_000, false),    // just within: not set
        (-10_000_000, false),    // slightly behind: not set
        (0, false),              // equal: not set
        (1_000_000_000, false),  // guest 1s ahead: within maxTimeInFuture
        (4_000_000_000, false),  // just within: not set
        (5_000_000_000, false),  // at maxTimeInFuture boundary: not set
        (60_000_000_000, true),  // guest 60s ahead: set
    ];
    for (sandbox_delta, want) in rows {
        assert_eq!(
            should_set_system_time(base + sandbox_delta, base),
            *want,
            "sandbox delta {sandbox_delta}"
        );
    }
}

#[test]
fn rfc3339_decode_matches_the_probed_go_acceptance() {
    // Accepted by Go time.Time JSON decode; values from the go1.27 probe.
    let ok: &[(&str, i128)] = &[
        ("2026-10-31T01:23:45Z", 1_793_409_825_000_000_000),
        ("2026-10-31T01:23:45.5Z", 1_793_409_825_500_000_000),
        (
            "2026-10-31T01:23:45.123456789012Z",
            1_793_409_825_123_456_789,
        ),
        ("2026-10-31T01:23:45+02:00", 1_793_402_625_000_000_000),
        ("2026-10-31T01:23:45.5+02:00", 1_793_402_625_500_000_000),
        ("2026-10-31T01:23:45-00:00", 1_793_409_825_000_000_000),
        ("2026-10-31T01:23:45+24:00", 1_793_323_425_000_000_000),
        ("2026-10-31T01:23:45+02:60", 1_793_399_025_000_000_000),
        ("2024-02-29T00:00:00Z", 1_709_164_800_000_000_000),
        ("2026-10-31T01:23:45.000000000Z", 1_793_409_825_000_000_000),
        ("1970-01-01T00:00:00.9999999999Z", 999_999_999),
        ("1970-01-01T00:00:00.0000000001Z", 0),
        ("1970-01-01T00:00:00.000000000000000000000000000045Z", 0),
        ("1970-01-01T00:00:00.12345678949999Z", 123_456_789),
        // Full i128 precision; the int64 wrap is pinned separately below.
        ("0000-01-01T00:00:00Z", -62_167_219_200_000_000_000),
        ("0001-01-01T00:00:00Z", -62_135_596_800_000_000_000),
    ];
    for (text, want) in ok {
        assert_eq!(rfc3339_nanos(text), Some(*want), "{text}");
    }
    // Rejected by Go (decode error -> the whole /init is a 400).
    let bad: &[&str] = &[
        "2026-10-31T01:23:60Z",      // second out of range
        "2026-10-31T01:23:45",       // missing zone
        "2026-10-31t01:23:45z",      // lowercase t/z
        "2026-13-01T00:00:00Z",      // month out of range
        "2026-02-30T00:00:00Z",      // day out of range
        "2023-02-29T00:00:00Z",      // not a leap year
        "2026-10-31T24:00:00Z",      // hour out of range
        "2026-10-31T01:23:45+99:00", // offset hour out of range
        "2026-10-31T01:23:45+02:99", // offset minute out of range
        "2026-10-31T01:23:45+0200",  // offset missing colon
        "2026-10-31T01:23:45.Z",     // fraction without digits
        "2026-10-31T01:23:45+02",    // short offset
        "2026-1-1T1:2:3Z",           // unpadded fields
        " 2026-10-31T01:23:45Z",     // leading space
        "2026-10-31T01:23:45Z ",     // trailing space
        "",                          // empty
    ];
    for text in bad {
        assert_eq!(rfc3339_nanos(text), None, "{text}");
    }
}

#[test]
fn unix_nanos_wraps_like_go_beyond_the_int64_domain() {
    // Go UnixNano is documented undefined outside i64; observed it wraps
    // (0001 -> negative, 9999 -> wrapped negative). Rust `as i64` is the same
    // two's-complement reduction from the i128 nanosecond value.
    assert_eq!(
        rfc3339_nanos("0001-01-01T00:00:00Z").unwrap() as i64,
        -6_795_364_578_871_345_152
    );
    assert_eq!(
        rfc3339_nanos("0000-01-01T00:00:00Z").unwrap() as i64,
        -6_826_986_978_871_345_152
    );
    assert_eq!(
        rfc3339_nanos("9999-12-31T23:59:59Z").unwrap() as i64,
        -4_852_116_232_933_722_624
    );
}

#[test]
fn go_timespec_parts_normalizes_like_x_sys_v0_49() {
    // unix.NsecToTimespec floor-normalizes negative nanoseconds so tv_nsec is
    // always in [0, 1e9); values from the pinned-version probe.
    let rows: &[(i64, i64, i64)] = &[
        (1_500_000_000, 1, 500_000_000),
        (-1_500_000_000, -2, 500_000_000),
        (-1, -1, 999_999_999),
        (1_999_999_999, 1, 999_999_999),
        (0, 0, 0),
    ];
    for (nanos, sec, nsec) in rows {
        assert_eq!(go_timespec_parts(*nanos), (*sec, *nsec), "{nanos}");
    }
}

// --- daemon: correction header, newest-wins gate, step placement ---

#[tokio::test]
async fn timestamp_within_tolerance_steps_nothing() {
    let steps = Steps::default();
    let now = real_now_nanos();
    let router = clock_daemon(Arc::new(real_now_nanos), steps.clone().sink());
    // Four seconds ahead of the timestamp is inside the 5s future band with a
    // full second of runtime slack; the 50ms edge is pinned only at the
    // engine level above, exactly like the oracle keeps boundary rows out of
    // its handler tests.
    let response = init_response(
        router,
        &format!(r#"{{"timestamp":"{}"}}"#, rfc(now - 4_000_000_000)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let header = header_of(&response);
    assert!(header("x-envd-init-clock-correction").is_some());
    assert!(steps.calls().is_empty(), "{:?}", steps.calls());
}

#[tokio::test]
async fn timestamp_ahead_beyond_tolerance_steps_to_corrected_target() {
    let steps = Steps::default();
    let now = real_now_nanos();
    let ts = now + 10_000_000_000;
    let router = clock_daemon(Arc::new(real_now_nanos), steps.clone().sink());
    let response = init_response(router, &format!(r#"{{"timestamp":"{}"}}"#, rfc(ts))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let header = header_of(&response);
    assert!(header("x-envd-init-clock-correction").is_some());
    let calls = steps.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    // The step target is the corrected timestamp: ts plus the handler's own
    // delay (small but non-negative here).
    assert!(
        i128::from(calls[0]) >= ts && i128::from(calls[0]) <= ts + 250_000_000,
        "target {} not in [{ts}, {}]",
        calls[0],
        ts + 250_000_000
    );
}

#[tokio::test]
async fn set_system_time_failure_is_log_only_and_still_204() {
    let steps = Steps::default();
    let now = real_now_nanos();
    let router = clock_daemon(Arc::new(real_now_nanos), steps.clone().failing_sink());
    let body = format!(
        r#"{{"timestamp":"{}","envVars":{{"MARK":"A"}}}}"#,
        rfc(now + 10_000_000_000)
    );
    let response = init_response(router.clone(), &body).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(steps.calls().len(), 1, "the step was attempted");
    // SetData continues past the failed step: the env is still applied.
    assert_eq!(envs(router, None).await["MARK"], "A");
}

#[tokio::test]
async fn older_corrected_timestamp_refused_keeps_state_and_skips_step() {
    let steps = Steps::default();
    let now = real_now_nanos();
    let ts = now + 3_600_000_000_000;
    // A constant injected now makes every correction exactly zero, so the
    // equal-timestamp acceptance is deterministic (real-now jitter between
    // inits would otherwise flip the newest-wins compare).
    let router = clock_daemon(
        Arc::new(|| 1_800_000_000_000_000_000i128),
        steps.clone().sink(),
    );
    let first = init_response(
        router.clone(),
        &format!(
            r#"{{"timestamp":"{}","envVars":{{"MARK":"A"}},"accessToken":"t1"}}"#,
            rfc(ts)
        ),
    )
    .await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    assert_eq!(steps.calls().len(), 1);

    // A corrected timestamp older than the last accepted one is refused:
    // 204 with the header, but SetData (env, token, step) never runs. A
    // genuine replay carries the same token; a different one would fail auth
    // before the gate.
    let second = init_response(
        router.clone(),
        &format!(
            r#"{{"timestamp":"{}","envVars":{{"MARK":"B"}},"accessToken":"t1"}}"#,
            rfc(ts - 1_000_000_000)
        ),
    )
    .await;
    assert_eq!(second.status(), StatusCode::NO_CONTENT);
    let header = header_of(&second);
    assert!(header("x-envd-init-clock-correction").is_some());
    assert_eq!(steps.calls().len(), 1, "refused init must not step");
    assert_eq!(
        envs(router.clone(), Some("t1")).await["MARK"],
        "A",
        "refused init must not replace envs or the token"
    );

    // A corrected timestamp equal to the last accepted one is accepted.
    let third = init_response(
        router.clone(),
        &format!(
            r#"{{"timestamp":"{}","envVars":{{"MARK":"B"}},"accessToken":"t1"}}"#,
            rfc(ts)
        ),
    )
    .await;
    assert_eq!(third.status(), StatusCode::NO_CONTENT);
    assert_eq!(steps.calls().len(), 2, "equal corrected timestamp steps");
    assert_eq!(envs(router, Some("t1")).await["MARK"], "B");
}

#[tokio::test]
async fn no_timestamp_has_no_correction_header_and_no_step() {
    let steps = Steps::default();
    let router = clock_daemon(Arc::new(real_now_nanos), steps.clone().sink());
    let response = init_response(router, r#"{"envVars":{"MARK":"A"}}"#).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let header = header_of(&response);
    assert!(header("x-envd-init-clock-correction").is_none());
    assert!(steps.calls().is_empty());
}

#[tokio::test]
async fn malformed_timestamp_rejects_the_whole_init() {
    let steps = Steps::default();
    let router = clock_daemon(Arc::new(real_now_nanos), steps.clone().sink());
    for body in [
        r#"{"timestamp":"2026-10-31t01:23:45z"}"#.to_owned(),
        r#"{"timestamp":"2026-10-31T01:23:60Z"}"#.to_owned(),
        r#"{"timestamp":12345}"#.to_owned(),
        r#"{"timestamp":"2026-10-31T01:23:45.Z"}"#.to_owned(),
    ] {
        assert_eq!(
            init_with(router.clone(), &body).await,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    assert!(steps.calls().is_empty());
}

#[tokio::test]
async fn timestamp_null_is_treated_as_absent() {
    let steps = Steps::default();
    let router = clock_daemon(Arc::new(real_now_nanos), steps.clone().sink());
    let response = init_response(router, r#"{"timestamp":null}"#).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let header = header_of(&response);
    assert!(header("x-envd-init-clock-correction").is_none());
    assert!(steps.calls().is_empty());
}

#[tokio::test]
async fn escaped_json_timestamp_is_a_decode_error_like_go() {
    // Go decodes `timestamp` as *time.Time, and encoding/json hands
    // UnmarshalJSON the RAW bytes of the value: escape sequences are NOT
    // decoded before the RFC3339 parse, so any "\u..." fails it. Probed
    // against the pinned oracle in a real guest: an escaped timestamp answers
    // 400 with no correction header and no step, while an unescaped fraction
    // separator is ordinary RFC3339 for Go and still accepted.
    let steps = Steps::default();
    let router = clock_daemon(Arc::new(real_now_nanos), steps.clone().sink());
    for body in [
        r#"{"timestamp":"2026-10-31T01:23:45\u002e5Z"}"#,
        r#"{"timestamp":"\u0032\u0030\u0032\u0036-10-31T01:23:45.5Z"}"#,
        r#"{"timestamp":"2026-10-31T01:23:45\/31T01:23:45.5Z"}"#,
    ] {
        let response = init_response(router.clone(), body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert!(
            header_of(&response)("x-envd-init-clock-correction").is_none(),
            "{body}"
        );
    }
    assert!(steps.calls().is_empty(), "{:?}", steps.calls());
    let response = init_response(router, r#"{"timestamp":"2026-10-31T01:23:45.5Z"}"#).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(header_of(&response)("x-envd-init-clock-correction").is_some());
}

#[tokio::test]
async fn pre_epoch_and_wrapped_timestamps_refuse_at_the_gate() {
    // The newest-wins gate starts at 0, so a corrected timestamp whose i64
    // UnixNano is negative is refused: year 0001 directly (defined value), and
    // year 9999 through Go's wrapped UnixNano reduction (probed
    // -4852116232933722624). Both still answer the header and stay 204.
    let steps = Steps::default();
    let router = clock_daemon(
        Arc::new(|| 1_800_000_000_000_000_000i128),
        steps.clone().sink(),
    );
    for body in [
        r#"{"timestamp":"0001-01-01T00:00:00Z","envVars":{"MARK":"A"}}"#,
        r#"{"timestamp":"9999-12-31T23:59:59Z","envVars":{"MARK":"B"}}"#,
    ] {
        let response = init_response(router.clone(), body).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
        let header = header_of(&response);
        assert!(header("x-envd-init-clock-correction").is_some());
    }
    assert!(steps.calls().is_empty(), "{:?}", steps.calls());
    let values = envs(router, None).await;
    assert!(values.get("MARK").is_none(), "{values}");
}

#[tokio::test]
async fn correction_header_truncates_whole_milliseconds() {
    // Injected now: body read at T1, clock gates at T2. Go answers
    // correction.Milliseconds() (truncated toward zero) and steps to
    // timestamp + correction exactly (clock_correction_header_test.go).
    let steps = Steps::default();
    let base = 1_800_000_000_000_000_000i128;
    let delay = 300_900_000i128; // 300ms + 900us
    let now: coorworker_envd::clock::NowFn = {
        let call = Mutex::new(0u32);
        Arc::new(move || {
            *call.lock().unwrap() += 1;
            if *call.lock().unwrap() == 1 {
                base
            } else {
                base + delay
            }
        })
    };
    let ts = real_now_nanos() + 3_600_000_000_000;
    let router = clock_daemon(now, steps.clone().sink());
    let response = init_response(router, &format!(r#"{{"timestamp":"{}"}}"#, rfc(ts))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let header = header_of(&response);
    assert_eq!(
        header("x-envd-init-clock-correction").as_deref(),
        Some("300")
    );
    assert_eq!(
        steps.calls(),
        vec![(ts + delay) as i64],
        "target is ts + correction"
    );

    // A negative correction truncates toward zero: -900us answers "0".
    let steps = Steps::default();
    let negative: coorworker_envd::clock::NowFn = {
        let call = Mutex::new(0u32);
        Arc::new(move || {
            *call.lock().unwrap() += 1;
            if *call.lock().unwrap() == 1 {
                base
            } else {
                base - 900_000
            }
        })
    };
    let router = clock_daemon(negative, steps.clone().sink());
    let response = init_response(router, &format!(r#"{{"timestamp":"{}"}}"#, rfc(ts))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let header = header_of(&response);
    assert_eq!(header("x-envd-init-clock-correction").as_deref(), Some("0"));
}

#[tokio::test]
async fn development_daemon_without_sink_skips_the_step() {
    // Go dev attempts the step and fails harmlessly; the Rust development
    // daemon has no sink wired, so the step is skipped with the same
    // observable end state: 204, header present, correction still corrected,
    // and the newest-wins gate still advances.
    let now = real_now_nanos();
    let ts = now + 3_600_000_000_000;
    use coorworker_envd::daemon::{app, Daemon};
    // A constant injected now makes the correction exactly zero twice, so the
    // equal-timestamp acceptance is deterministic.
    let router = app(Arc::new(
        Daemon::new().with_now(Arc::new(|| 1_800_000_000_000_000_000i128)),
    ));
    let first = init_response(
        router.clone(),
        &format!(r#"{{"timestamp":"{}","envVars":{{"MARK":"A"}}}}"#, rfc(ts)),
    )
    .await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    let header = header_of(&first);
    assert!(header("x-envd-init-clock-correction").is_some());
    // Equal corrected timestamp is still accepted by the gate.
    let second = init_response(
        router.clone(),
        &format!(r#"{{"timestamp":"{}","envVars":{{"MARK":"B"}}}}"#, rfc(ts)),
    )
    .await;
    assert_eq!(second.status(), StatusCode::NO_CONTENT);
    assert_eq!(envs(router, None).await["MARK"], "B");
}
