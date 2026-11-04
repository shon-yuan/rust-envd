//! Windows logging sinks: the event log for SCM service mode and stderr for
//! console/development mode.
//!
//! The service process has no console, so startup and handover diagnostics
//! would otherwise vanish. The sink deliberately stays small: the shared
//! `crate::logs` logger keeps its own contract, and the daemon's request paths
//! do not depend on this module.

use std::io;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::GetConsoleWindow;
use windows_sys::Win32::System::EventLog::{
    DeregisterEventSource, RegisterEventSourceW, ReportEventW, EVENTLOG_ERROR_TYPE,
    EVENTLOG_INFORMATION_TYPE, EVENTLOG_WARNING_TYPE,
};

/// The event-log source name. It matches the installed service name so
/// operators find both under one label.
pub const SOURCE_NAME: &str = "coorworker-envd";

/// Event id 1 is a generic "daemon message"; the entry type carries severity.
const EVENT_ID: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// A reported event-log source, closed on drop.
pub struct EventLog {
    source: HANDLE,
}

// SAFETY: an event-source handle is a kernel handle usable from any thread of
// the owning process, and this wrapper is its unique owner.
unsafe impl Send for EventLog {}
unsafe impl Sync for EventLog {}

impl EventLog {
    pub fn open(name: &str) -> io::Result<Self> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the source name is null-terminated and lives for the call;
        // a null server name means the local machine.
        let source = unsafe { RegisterEventSourceW(std::ptr::null(), wide.as_ptr()) };
        if source.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { source })
    }

    pub fn report(&self, level: Level, message: &str) {
        let message: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        let strings = [message.as_ptr()];
        // SAFETY: `source` is live; the string array and its buffer outlive
        // the call; no user SID and no raw data are attached.
        unsafe {
            ReportEventW(
                self.source,
                event_type(level),
                0,
                EVENT_ID,
                std::ptr::null_mut(),
                1,
                0,
                strings.as_ptr(),
                std::ptr::null(),
            );
        }
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by RegisterEventSourceW and is
        // closed exactly once here.
        unsafe {
            DeregisterEventSource(self.source);
        }
    }
}

fn event_type(level: Level) -> u16 {
    match level {
        Level::Info => EVENTLOG_INFORMATION_TYPE,
        Level::Warning => EVENTLOG_WARNING_TYPE,
        Level::Error => EVENTLOG_ERROR_TYPE,
    }
}

/// Whether this process is attached to a console. An SCM service and the
/// successor it spawns have none, so their diagnostics go to the event log
/// (stderr is redirected to NUL in those processes).
pub fn console_attached() -> bool {
    // SAFETY: GetConsoleWindow takes no arguments and only reads process
    // state.
    !unsafe { GetConsoleWindow() }.is_null()
}

/// The process's log destination.
pub enum Sink {
    Console,
    Event(EventLog),
}

impl Sink {
    /// Opens the sink for the launch mode. A failed event-log open degrades to
    /// stderr rather than failing startup: a service that cannot report to
    /// the event log should still serve.
    pub fn open(service: bool, name: &str) -> Self {
        if !service {
            return Sink::Console;
        }
        match EventLog::open(name) {
            Ok(log) => Sink::Event(log),
            Err(error) => {
                eprintln!("cannot open the event log source {name:?}: {error}");
                Sink::Console
            }
        }
    }

    pub fn log(&self, level: Level, message: &str) {
        match self {
            Sink::Console => eprintln!("{message}"),
            Sink::Event(log) => {
                log.report(level, message);
                // Keep a copy on stderr for a console-attached service start
                // and for CI logs; the event log is the durable record.
                if level != Level::Info {
                    eprintln!("{message}");
                }
            }
        }
    }

    pub fn info(&self, message: &str) {
        self.log(Level::Info, message);
    }

    pub fn warn(&self, message: &str) {
        self.log(Level::Warning, message);
    }

    pub fn error(&self, message: &str) {
        self.log(Level::Error, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_maps_to_the_event_log_type() {
        assert_eq!(event_type(Level::Info), EVENTLOG_INFORMATION_TYPE);
        assert_eq!(event_type(Level::Warning), EVENTLOG_WARNING_TYPE);
        assert_eq!(event_type(Level::Error), EVENTLOG_ERROR_TYPE);
    }

    #[test]
    fn console_mode_never_touches_the_event_log() {
        assert!(matches!(Sink::open(false, SOURCE_NAME), Sink::Console));
    }
}
