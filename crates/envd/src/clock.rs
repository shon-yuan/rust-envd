//! Clock contract of /init: RFC3339 timestamp decode, the handler-delay
//! correction, the newest-wins gate, and the system-clock step.
//!
//! Contract: pinned init.go:41-42 (`maxTimeInPast` 50 ms / `maxTimeInFuture`
//! 5 s), init.go:1089-1091 (`shouldSetSystemTime`, strict `Before`/`After` so
//! an exact boundary does not step), init.go:433-443 (SetData steps to the
//! timestamp first and only logs a failed step), clock_linux.go:11-15
//! (`unix.ClockSettime(CLOCK_REALTIME, NsecToTimespec(t.UnixNano()))`),
//! store.go:58-61/:164-166 (the sink and the clock reading are injectable
//! fields, tests never touch the host clock), utils/atomic.go:16-26
//! (`SetToGreater` refuses only strictly older values).
//!
//! Go types decode `timestamp` as RFC3339 `time.Time`. The acceptance set was
//! pinned empirically with the Go decoder (fraction digits beyond 9 truncate,
//! `Z`/`±HH:MM` only with offset hour <= 24 and offset minute <= 60, fixed
//! field widths, range-checked calendar, `null` is absent) and
//! `unix.NsecToTimespec` was probed on the pinned x/sys v0.49.0 (floor
//! normalization, `tv_nsec` always in `[0, 1e9)`). Beyond the int64
//! nanosecond domain Go's `UnixNano` is documented undefined and observed to
//! wrap; this module keeps full i128 precision and reduces to i64 only at
//! Go's own `UnixNano` call points (the gate compare and the sink target),
//! where `as i64` performs the same two's-complement reduction.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Go init.go:41: a sandbox clock more than this far behind the timestamp steps.
pub const MAX_TIME_IN_PAST_NANOS: i128 = 50_000_000;
/// Go init.go:42: a sandbox clock more than this far ahead of the timestamp steps.
pub const MAX_TIME_IN_FUTURE_NANOS: i128 = 5_000_000_000;

/// Go store.go:59: the step target sink; production wires the real
/// clock_settime, tests record. Go store.go:165 defaults the field to the
/// platform function the same way.
pub type SetSystemTimeFn = Arc<dyn Fn(i64) -> Result<(), String> + Send + Sync>;

/// Go store.go:60-61: the reading the correction measurement uses; tests
/// advance it at the points of the handler the correction should span.
pub type NowFn = Arc<dyn Fn() -> i128 + Send + Sync>;

/// Nanoseconds since the epoch read from the real system clock. Guest clocks
/// before the epoch are negative; production band checks use this exactly
/// where Go SetData uses `time.Now()` (init.go:435).
pub fn real_now_nanos() -> i128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(ahead) => ahead.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    }
}

/// Go `time.Time.UnixNano()`, called only at its own call points (the
/// init.go:336 gate compare and the clock_linux.go sink target): a
/// two's-complement i64 reduction of the full-precision value, which is
/// exactly how Go's int64 nanosecond arithmetic behaves outside its
/// documented domain.
pub fn unix_nanos_wrapping(nanos: i128) -> i64 {
    nanos as i64
}

/// Go init.go:1089-1091: `sandbox.Before(host-50ms) || sandbox.After(host+5s)`.
/// Comparisons are strict: an exact boundary does not step (init_test.go:83,
/// :113 assert both boundary rows as false).
pub fn should_set_system_time(sandbox_nanos: i128, host_nanos: i128) -> bool {
    sandbox_nanos < host_nanos - MAX_TIME_IN_PAST_NANOS
        || sandbox_nanos > host_nanos + MAX_TIME_IN_FUTURE_NANOS
}

/// Go `unix.NsecToTimespec` as shipped in the pinned x/sys v0.49.0:
/// floor-normalizing, so `tv_nsec` stays in `[0, 1e9)` for pre-epoch targets
/// too (probed: -1.5e9 -> sec -2 nsec 5e8, -1 -> sec -1 nsec 999999999).
pub fn go_timespec_parts(nanos: i64) -> (i64, i64) {
    let mut sec = nanos / 1_000_000_000;
    let mut nsec = nanos % 1_000_000_000;
    if nsec < 0 {
        nsec += 1_000_000_000;
        sec -= 1;
    }
    (sec, nsec)
}

/// Production sink: steps CLOCK_REALTIME to the target. Wired only on the
/// guest path (development daemons never touch the host clock), mirroring the
/// established development-boundary convention. The nix errno text differs
/// from Go's syscall error text (deviation, diagnostics only).
pub fn system_time_sink(nanos: i64) -> Result<(), String> {
    let (sec, nsec) = go_timespec_parts(nanos);
    nix::time::clock_settime(
        nix::time::ClockId::CLOCK_REALTIME,
        nix::sys::time::TimeSpec::new(sec, nsec),
    )
    .map_err(|error| error.to_string())
}

/// Decode an RFC3339 timestamp body value to nanoseconds since the epoch,
/// matching the pinned Go `time.Time` JSON decode (probed): `Z` or `±HH:MM`
/// with offset hour <= 24 and offset minute <= 60, `T`/`Z` uppercase, fixed
/// field widths, second <= 59, range-checked calendar, an optional fraction of
/// any digit count truncated to nanoseconds, no surrounding whitespace, and
/// no trailing text. `null` never reaches here (the caller treats it as
/// absent) and non-string values are rejected by the JSON layer.
pub fn rfc3339_nanos(raw: &str) -> Option<i128> {
    let bytes = raw.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let digits = |part: &[u8]| -> Option<i64> {
        part.iter().try_fold(0i64, |value, digit| {
            digit
                .is_ascii_digit()
                .then(|| value * 10 + i64::from(digit - b'0'))
        })
    };
    let year = digits(&bytes[0..4])?;
    if bytes[4] != b'-' {
        return None;
    }
    let month = digits(&bytes[5..7])?;
    if bytes[7] != b'-' {
        return None;
    }
    let day = digits(&bytes[8..10])?;
    if bytes[10] != b'T' {
        return None;
    }
    let hour = digits(&bytes[11..13])?;
    if bytes[13] != b':' {
        return None;
    }
    let minute = digits(&bytes[14..16])?;
    if bytes[16] != b':' {
        return None;
    }
    let second = digits(&bytes[17..19])?;
    if !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 59
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }
    let mut index = 19;
    let mut fraction: i128 = 0;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while bytes.get(index).is_some_and(|byte| byte.is_ascii_digit()) {
            index += 1;
        }
        if index == start {
            return None;
        }
        // Any digit count is accepted and truncates to the first nine digits
        // with no rounding (probed: .9999999999 -> 999999999,
        // .12345678949999 -> 123456789, 35 digits accepted).
        let mut scale = 100_000_000i128;
        for digit in &bytes[start..(start + 9).min(index)] {
            fraction += i128::from(digit - b'0') * scale;
            scale /= 10;
        }
    }
    let offset_seconds = match bytes.get(index) {
        Some(b'Z') => {
            index += 1;
            0i64
        }
        Some(sign @ (b'+' | b'-')) => {
            index += 1;
            let offset_hour = digits(bytes.get(index..index + 2)?)?;
            index += 2;
            if bytes.get(index) != Some(&b':') {
                return None;
            }
            index += 1;
            let offset_minute = digits(bytes.get(index..index + 2)?)?;
            index += 2;
            if offset_hour > 24 || offset_minute > 60 {
                return None;
            }
            let magnitude = (offset_hour * 3600) + (offset_minute * 60);
            if *sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        _ => return None,
    };
    if index != bytes.len() {
        return None;
    }
    let day_seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - offset_seconds;
    Some(i128::from(day_seconds) * 1_000_000_000 + fraction)
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Hinnant's days_from_civil: proleptic Gregorian day number relative to
/// 1970-01-01. Year 0 is leap, matching Go's calendar for "0000-02-29".
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let years = if month <= 2 { year - 1 } else { year };
    let era = if years >= 0 { years } else { years - 399 } / 400;
    let year_of_era = years - era * 400;
    let month_pattern = (month + 9) % 12;
    let day_of_year = (153 * month_pattern + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
