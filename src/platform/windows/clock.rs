//! Sandbox clock skeleton. Reading the system clock is implemented with
//! `std::time`; writing it needs Win32 privileges and arrives with the init
//! effects in P3.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::platform::ClockBackend;

#[derive(Debug, Default)]
pub struct WindowsClockBackend;

impl ClockBackend for WindowsClockBackend {
    fn now_unix_nanos(&self) -> i128 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_nanos() as i128,
            Err(error) => -(error.duration().as_nanos() as i128),
        }
    }

    fn set_system_time(&self, _nanos: i64) -> Result<(), String> {
        Err(super::unsupported("setting the system time").to_string())
    }
}
