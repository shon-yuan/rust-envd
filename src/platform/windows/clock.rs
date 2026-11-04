//! Sandbox clock backend. Reading uses `std::time`; writing steps the guest
//! clock with `SetSystemTime`, the Windows analogue of the oracle's
//! `clock_settime(CLOCK_REALTIME)` (`clock_linux.go:11-15`).
//!
//! `SetSystemTime` needs `SeSystemtimePrivilege` held *and enabled* in the
//! process token. The SCM service runs as LocalSystem, whose token carries the
//! privilege disabled by default, so the sink enables it before the first
//! step; a console guest started by an administrator has it too. A caller
//! without the privilege gets an error, which `/init` logs and never fails on
//! (init.go:433-443).
//!
//! The conversion is pure and unit-tested: Go's target is nanoseconds since
//! the Unix epoch, while `SYSTEMTIME` carries calendar fields (always UTC for
//! `SetSystemTime`), so the arithmetic runs through the proleptic Gregorian
//! civil-date algorithm rather than a struct conversion API.

use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NOT_ALL_ASSIGNED, HANDLE, LUID, SYSTEMTIME,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    SE_SYSTEMTIME_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::SystemInformation::SetSystemTime;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::platform::ClockBackend;

/// Seconds from the Windows epoch (1601-01-01T00:00:00Z) to the Unix epoch.
const WINDOWS_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
/// 100-nanosecond ticks per second (the `SYSTEMTIME` millisecond resolution
/// loses the last four decimal digits, exactly as `clock_settime` rounding
/// on a millisecond-resolution clock would).
const TICKS_PER_SECOND: i128 = 10_000_000;
const TICKS_PER_MILLISECOND: i128 = 10_000;
const TICKS_PER_DAY: i128 = 86_400 * TICKS_PER_SECOND;
/// Days from 1601-01-01 to 1970-01-01.
const WINDOWS_EPOCH_DAYS: i128 = 134_774;

#[derive(Debug, Default)]
pub struct WindowsClockBackend;

impl ClockBackend for WindowsClockBackend {
    fn now_unix_nanos(&self) -> i128 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_nanos() as i128,
            Err(error) => -(error.duration().as_nanos() as i128),
        }
    }

    fn set_system_time(&self, nanos: i64) -> Result<(), String> {
        set_system_time(nanos)
    }
}

/// The `/init` clock sink: step the system clock to `nanos` since the Unix
/// epoch. Failure is reported to the caller, which logs it and keeps serving
/// (init.go:433-443).
pub fn set_system_time(nanos: i64) -> Result<(), String> {
    enable_systemtime_privilege()?;
    let target = system_time_from_unix_nanos(nanos);
    // SAFETY: `SYSTEMTIME` is plain C data; `SetSystemTime` reads it and
    // writes no memory. Its fields are always UTC.
    if unsafe { SetSystemTime(&target) } == 0 {
        return Err(format!(
            "SetSystemTime: {}",
            super::fs_runtime::win32_code_text(last_error_code())
        ));
    }
    Ok(())
}

/// `(year, month, day)` for a day count since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`, exact for the proleptic Gregorian calendar).
fn civil_from_days(days: i128) -> (i128, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u128; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe as i128 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Nanoseconds since the Unix epoch -> the `SYSTEMTIME` `SetSystemTime`
/// expects (UTC calendar fields, millisecond precision).
pub(crate) fn system_time_from_unix_nanos(nanos: i64) -> SYSTEMTIME {
    let (seconds, nanos_remainder) = crate::clock::go_timespec_parts(nanos);
    // 100-ns ticks since 1601-01-01T00:00:00Z, computed wide so a target far
    // from the epoch cannot overflow the intermediate.
    let ticks = (i128::from(seconds) + i128::from(WINDOWS_EPOCH_OFFSET_SECS)) * TICKS_PER_SECOND
        + i128::from(nanos_remainder / 100);
    let days = ticks.div_euclid(TICKS_PER_DAY);
    let day_ticks = ticks.rem_euclid(TICKS_PER_DAY);
    let seconds_of_day = day_ticks / TICKS_PER_SECOND;
    let milliseconds = (day_ticks % TICKS_PER_SECOND) / TICKS_PER_MILLISECOND;
    let (year, month, day) = civil_from_days(days - WINDOWS_EPOCH_DAYS);
    SYSTEMTIME {
        wYear: year as u16,
        wMonth: month as u16,
        // SetSystemTime ignores the day of week; the APIs derive it.
        wDayOfWeek: 0,
        wDay: day as u16,
        wHour: (seconds_of_day / 3_600) as u16,
        wMinute: ((seconds_of_day % 3_600) / 60) as u16,
        wSecond: (seconds_of_day % 60) as u16,
        wMilliseconds: milliseconds as u16,
    }
}

/// Enables `SeSystemtimePrivilege` in the process token. `AdjustTokenPrivileges`
/// reports success even when the privilege was not assigned, so the residual
/// `ERROR_NOT_ALL_ASSIGNED` is checked explicitly.
fn enable_systemtime_privilege() -> Result<(), String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the handle is opened here and closed on every path below; the
    // `TOKEN_PRIVILEGES` value is a plain C struct owned by this frame.
    unsafe {
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Err(format!(
                "OpenProcessToken: {}",
                super::fs_runtime::win32_code_text(last_error_code())
            ));
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(std::ptr::null(), SE_SYSTEMTIME_NAME, &mut luid) == 0 {
            let code = last_error_code();
            CloseHandle(token);
            return Err(format!(
                "LookupPrivilegeValueW: {}",
                super::fs_runtime::win32_code_text(code)
            ));
        }
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let adjusted = AdjustTokenPrivileges(
            token,
            0,
            &privileges,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        let code = last_error_code();
        CloseHandle(token);
        if adjusted == 0 {
            return Err(format!(
                "AdjustTokenPrivileges: {}",
                super::fs_runtime::win32_code_text(code)
            ));
        }
        if code == ERROR_NOT_ALL_ASSIGNED {
            return Err("the process token lacks SeSystemtimePrivilege".to_owned());
        }
    }
    Ok(())
}

/// The pending Win32 error code as the `u32` the text table expects.
fn last_error_code() -> u32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(nanos: i64) -> (u16, u16, u16, u16, u16, u16, u16) {
        let time = system_time_from_unix_nanos(nanos);
        (
            time.wYear,
            time.wMonth,
            time.wDay,
            time.wHour,
            time.wMinute,
            time.wSecond,
            time.wMilliseconds,
        )
    }

    #[test]
    fn the_unix_epoch_converts_to_the_windows_calendar() {
        assert_eq!(fields(0), (1970, 1, 1, 0, 0, 0, 0));
    }

    #[test]
    fn a_known_instant_keeps_utc_fields_and_milliseconds() {
        // 2026-11-04T10:26:29.123456789Z.
        let nanos = 1_793_787_989_123_456_789;
        assert_eq!(fields(nanos), (2026, 11, 4, 10, 26, 29, 123));
    }

    /// Go's `time.Time` can carry a pre-epoch instant; the tick arithmetic
    /// floors, so the calendar stays correct.
    #[test]
    fn a_pre_epoch_instant_floors_into_the_previous_day() {
        assert_eq!(fields(-1), (1969, 12, 31, 23, 59, 59, 999));
    }

    /// Leap years and the century rules through the civil-date algorithm.
    #[test]
    fn leap_days_are_recognized() {
        // 2024-02-29T00:00:00Z.
        assert_eq!(fields(1_709_164_800_000_000_000), (2024, 2, 29, 0, 0, 0, 0));
        // 2000-02-29T00:00:00Z (a leap century).
        assert_eq!(fields(951_782_400_000_000_000), (2000, 2, 29, 0, 0, 0, 0));
    }
}
