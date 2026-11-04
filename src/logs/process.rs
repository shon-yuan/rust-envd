//! Adapter from the process handler's log seam to structured log lines.
//!
//! Mirrors handler.go:599-605 (`process_start`) and :697-706 (`process_end`) at
//! 51083a83cc0903185d992725ea6bb33683814b29.

use super::{gojson, Logger};
use crate::{
    process_contract::{ProcessEnd, ProcessLogSink, ProcessStart},
    proto::process as pb,
};

pub struct ProcessLogger {
    logger: Logger,
}

impl ProcessLogger {
    pub fn new(logger: Logger) -> Self {
        Self { logger }
    }
}

impl ProcessLogSink for ProcessLogger {
    fn process_started(&self, start: &ProcessStart) {
        let mut event = self.logger.info();
        if let Some(operation_id) = &start.operation_id {
            event = event.str("operation_id", operation_id);
        }
        event
            .str("event_type", "process_start")
            .int("pid", i64::from(start.pid))
            .str("command", &start.command)
            .dur_ms(
                "request_timeout_ms",
                start.request_timeout.unwrap_or_default(),
            )
            .msg(format!("Process with pid {} started", start.pid));
    }

    fn process_ended(&self, end: &ProcessEnd) {
        let mut event = self.logger.info();
        if let Some(operation_id) = &end.operation_id {
            event = event.str("operation_id", operation_id);
        }
        event
            .str("event_type", "process_end")
            .raw_bytes("process_result", process_result(&end.end))
            .int("stdout_bytes", end.stdout_bytes)
            .int("stderr_bytes", end.stderr_bytes)
            .int("pty_bytes", end.pty_bytes)
            .msg(format!("Process with pid {} ended", end.pid));
    }
}

/// `encoding/json` of `rpc.ProcessEvent_EndEvent`: fields in Go declaration
/// order (`exit_code`, `exited`, `status`, `error`), each dropped at its zero
/// value by `omitempty`.
fn process_result(end: &pb::process_event::EndEvent) -> Vec<u8> {
    let mut output = Vec::new();
    output.push(b'{');
    let mut first = true;
    if end.exit_code != 0 {
        key(&mut output, &mut first, "exit_code");
        output.extend_from_slice(end.exit_code.to_string().as_bytes());
    }
    if end.exited {
        key(&mut output, &mut first, "exited");
        output.extend_from_slice(b"true");
    }
    if !end.status.is_empty() {
        key(&mut output, &mut first, "status");
        gojson::write_string(&mut output, &end.status);
    }
    if let Some(error) = &end.error {
        key(&mut output, &mut first, "error");
        gojson::write_string(&mut output, error);
    }
    output.push(b'}');
    output
}

fn key(output: &mut Vec<u8>, first: &mut bool, name: &str) {
    if !*first {
        output.push(b',');
    }
    *first = false;
    gojson::write_string(output, name);
    output.push(b':');
}
