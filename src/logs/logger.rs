//! zerolog-shaped structured logs.
//!
//! Mirrors `logs.NewLogger` (internal/logs/logger.go) at
//! 51083a83cc0903185d992725ea6bb33683814b29: one JSON object per line, field
//! order `level`, context, event fields, `message`, a trailing newline, RFC3339
//! nanosecond timestamps in the `timestamp` field, Debug level minimum, and
//! `encoding/json` string escaping (including HTML and U+2028/U+2029).

use std::{
    fmt,
    io::Write,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::{gojson, LogSink};

type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Clone)]
enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Raw(serde_json::Value),
    /// Pre-encoded JSON, preserving the producer's field order (`Interface`
    /// on a Go struct marshals in declaration order, unlike a map).
    RawBytes(Vec<u8>),
}

#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    sinks: Vec<Arc<dyn LogSink>>,
    context: Vec<(String, Value)>,
    clock: Clock,
}

impl Logger {
    pub fn new(sinks: Vec<Arc<dyn LogSink>>) -> Self {
        Self::with_clock(sinks, Arc::new(SystemTime::now))
    }

    /// Test seam for the timestamp source; production uses the system clock.
    pub fn with_clock(sinks: Vec<Arc<dyn LogSink>>, clock: Clock) -> Self {
        Self {
            inner: Arc::new(Inner {
                sinks,
                context: Vec::new(),
                clock,
            }),
        }
    }

    /// A logger with nowhere to write, matching Go's empty MultiWriter.
    pub fn none() -> Self {
        Self::new(Vec::new())
    }

    /// Go `l.With().Str(key, value).Logger()`: a context field emitted after
    /// the timestamp and before per-event fields.
    pub fn with_str(&self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let mut context = self.inner.context.clone();
        context.push((key.into(), Value::Str(value.into())));
        Self {
            inner: Arc::new(Inner {
                sinks: self.inner.sinks.clone(),
                context,
                clock: self.inner.clock.clone(),
            }),
        }
    }

    pub fn debug(&self) -> Event<'_> {
        self.log(Level::Debug)
    }

    pub fn info(&self) -> Event<'_> {
        self.log(Level::Info)
    }

    pub fn warn(&self) -> Event<'_> {
        self.log(Level::Warn)
    }

    pub fn error(&self) -> Event<'_> {
        self.log(Level::Error)
    }

    pub fn log(&self, level: Level) -> Event<'_> {
        Event {
            logger: self,
            level,
            fields: Vec::new(),
        }
    }

    fn emit(&self, level: Level, fields: &[(String, Value)], message: &str) {
        if self.inner.sinks.is_empty() {
            return;
        }
        let mut line = Vec::new();
        line.push(b'{');
        write_field(
            &mut line,
            "level",
            &Value::Str(level.as_str().to_owned()),
            false,
        );
        write_field(
            &mut line,
            "timestamp",
            &Value::Str(rfc3339_nano((self.inner.clock)())),
            true,
        );
        for (key, value) in &self.inner.context {
            write_field(&mut line, key, value, true);
        }
        for (key, value) in fields {
            write_field(&mut line, key, value, true);
        }
        if !message.is_empty() {
            write_field(&mut line, "message", &Value::Str(message.to_owned()), true);
        }
        line.extend_from_slice(b"}\n");
        for sink in &self.inner.sinks {
            sink.write_line(&line);
        }
    }
}

pub struct Event<'a> {
    logger: &'a Logger,
    level: Level,
    fields: Vec<(String, Value)>,
}

impl Event<'_> {
    pub fn str(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((key.into(), Value::Str(value.into())));
        self
    }

    pub fn int(mut self, key: impl Into<String>, value: i64) -> Self {
        self.fields.push((key.into(), Value::Int(value)));
        self
    }

    /// zerolog `Dur`: milliseconds rendered as a shortest-form float.
    pub fn dur_ms(mut self, key: impl Into<String>, value: Duration) -> Self {
        self.fields
            .push((key.into(), Value::Float(value.as_secs_f64() * 1000.0)));
        self
    }

    pub fn boolean(mut self, key: impl Into<String>, value: bool) -> Self {
        self.fields.push((key.into(), Value::Bool(value)));
        self
    }

    /// zerolog `Interface`: Encoded with Go `encoding/json` semantics.
    pub fn raw(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.fields.push((key.into(), Value::Raw(value)));
        self
    }

    /// Pre-encoded JSON for structs whose Go marshal order must be preserved.
    pub fn raw_bytes(mut self, key: impl Into<String>, value: Vec<u8>) -> Self {
        self.fields.push((key.into(), Value::RawBytes(value)));
        self
    }

    /// zerolog `Err`: the `error` field, rendered with `Display`.
    pub fn err<E: fmt::Display + ?Sized>(mut self, error: &E) -> Self {
        self.fields
            .push(("error".to_owned(), Value::Str(error.to_string())));
        self
    }

    pub fn msg(self, message: impl Into<String>) {
        self.logger.emit(self.level, &self.fields, &message.into());
    }
}

fn write_field(output: &mut Vec<u8>, key: &str, value: &Value, comma: bool) {
    if comma {
        output.push(b',');
    }
    gojson::write_string(output, key);
    output.push(b':');
    match value {
        Value::Str(text) => gojson::write_string(output, text),
        Value::Int(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::Float(number) => output.extend_from_slice(
            gojson::encode_number(*number)
                .unwrap_or_default()
                .as_bytes(),
        ),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Raw(value) => {
            if let Ok(encoded) = gojson::encode_value(value) {
                output.extend_from_slice(&encoded);
            } else {
                output.extend_from_slice(b"null");
            }
        }
        Value::RawBytes(bytes) => output.extend_from_slice(bytes),
    }
}

/// Go `os.Stdout` appended to the writers when `--verbose` is set.
pub struct StdoutSink;

impl LogSink for StdoutSink {
    fn write_line(&self, line: &[u8]) {
        let _ = std::io::stdout().lock().write_all(line);
    }
}

/// Go `time.RFC3339Nano` (`2006-01-02T15:04:05.999999999Z07:00`) rendered in
/// UTC: nine fractional digits with trailing zeros removed, and no fraction at
/// all when it is a whole second. Guests run in UTC; a host with another local
/// zone would show `Z` here where Go shows the local offset (disclosed).
pub fn rfc3339_nano(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs() as i64;
    let nanos = since_epoch.subsec_nanos();
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = of_day / 3_600;
    let minute = (of_day % 3_600) / 60;
    let second = of_day % 60;
    let mut fraction = format!("{nanos:09}");
    while fraction.ends_with('0') {
        fraction.pop();
    }
    let fraction = if fraction.is_empty() {
        String::new()
    } else {
        format!(".{fraction}")
    };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}Z")
}

/// Howard Hinnant's days-to-civil algorithm, valid for the whole i64 range.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
