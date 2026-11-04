//! Platform-neutral freeze wire types and the Go-compatible query binder.
//!
//! The Linux cgroup freezer re-exports these under its historical paths
//! (`crate::cgroups::freeze::*`), and the Windows backend will build the same
//! wire values. Nothing here touches a platform API: the types are the
//! oracle's `api.gen.go` shapes and the binder reproduces Go `strconv` and
//! `url.ParseQuery` text exactly.

use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;

/// Oracle freeze.go:82: the cap every whole-tree walk (the thaw discovery, the
/// guest-freeze scan, the resume audit) is bounded by.
pub const DEFAULT_THAW_MAX_CGROUPS: usize = 8192;
/// Oracle freeze.go:94: the freeze sweep's own default bound.
pub const DEFAULT_FREEZE_MAX_CGROUPS: usize = 512;
/// Oracle freeze.go:66: the backstop window, measured in guest monotonic time
/// so a paused VM's freeze is backed up once it resumes. Shared so the Windows
/// suspend backend arms the same window as the Linux cgroup freezer.
pub const DEFAULT_THAW_WATCHDOG_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Oracle freeze.go:23-31.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum FreezeMode {
    #[default]
    #[serde(rename = "legacy")]
    Legacy,
    #[serde(rename = "hierarchy")]
    Hierarchy,
}

impl FreezeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FreezeMode::Legacy => "legacy",
            FreezeMode::Hierarchy => "hierarchy",
        }
    }
}

/// Oracle freeze.go:99-110.
#[derive(Debug, Clone)]
pub struct FreezeOptions {
    /// Bounds the wait for the written cgroups to read back frozen; zero
    /// observes nothing (oracle awaitFrozen: `budget <= 0` returns at once).
    pub max_wait: Duration,
    pub mode: FreezeMode,
    /// Bounds a hierarchy sweep; zero or negative means
    /// DEFAULT_FREEZE_MAX_CGROUPS — a caller cannot accidentally disable the
    /// guard (freeze.go:118-121) — and any positive value is clamped at
    /// DEFAULT_THAW_MAX_CGROUPS (freeze.go:123-129, the drift guard).
    pub max_cgroups: i64,
}

impl Default for FreezeOptions {
    fn default() -> Self {
        Self {
            max_wait: Duration::ZERO,
            mode: FreezeMode::Legacy,
            max_cgroups: 0,
        }
    }
}

impl FreezeOptions {
    /// Oracle freeze.go:112-117: anything but hierarchy is legacy, so a
    /// caller that has not been taught about modes keeps today's behaviour.
    pub fn effective_mode(&self) -> FreezeMode {
        match self.mode {
            FreezeMode::Hierarchy => FreezeMode::Hierarchy,
            FreezeMode::Legacy => FreezeMode::Legacy,
        }
    }
    /// Oracle freeze.go:118-130.
    pub fn effective_max_cgroups(&self) -> usize {
        if self.max_cgroups <= 0 {
            DEFAULT_FREEZE_MAX_CGROUPS
        } else {
            (self.max_cgroups as usize).min(DEFAULT_THAW_MAX_CGROUPS)
        }
    }
}

/// Oracle freeze.go:436-545. Frozen+NotFrozen+Unobservable accounts for every
/// cgroup whose state we read; Failed and Vanished span both the sweep and the
/// poll and deliberately do not reconcile against Requested (freeze.go:439-456).
#[derive(Debug, Default, Clone)]
pub struct FreezeResult {
    pub requested: i64,
    pub frozen: i64,
    pub not_frozen: i64,
    pub scan_failed: i64,
    pub pre_frozen: i64,
    pub failed: i64,
    pub vanished: i64,
    pub vanished_paths: Vec<String>,
    pub unobservable: i64,
    pub sweep_duration: Duration,
    pub mode: FreezeMode,
    pub visited: i64,
    pub allowlisted: i64,
    pub truncated: bool,
    pub wait_duration: Duration,
}

impl FreezeResult {
    /// Oracle freeze.go:547-550. Unobservable is not counted against it: a
    /// guest with no manager never had this guarantee.
    pub fn all_frozen(&self) -> bool {
        self.not_frozen == 0 && self.failed == 0
    }
}

/// The wire form the /freeze handler answers with; oracle api.gen.go:150-204,
/// every field filled by init.go:627-643, so nothing is omitted.
#[derive(Debug, Serialize)]
pub struct FreezeResultWire {
    pub allowlisted: i64,
    pub failed: i64,
    pub frozen: i64,
    pub mode: FreezeMode,
    #[serde(rename = "notFrozen")]
    pub not_frozen: i64,
    #[serde(rename = "preFrozen")]
    pub pre_frozen: i64,
    pub requested: i64,
    #[serde(rename = "sweepMs")]
    pub sweep_ms: i64,
    pub truncated: bool,
    pub unobservable: i64,
    pub vanished: i64,
    pub visited: i64,
    #[serde(rename = "waitMs")]
    pub wait_ms: i64,
}

impl FreezeResult {
    pub fn wire(&self) -> FreezeResultWire {
        FreezeResultWire {
            allowlisted: self.allowlisted,
            failed: self.failed,
            frozen: self.frozen,
            mode: self.mode,
            not_frozen: self.not_frozen,
            pre_frozen: self.pre_frozen,
            requested: self.requested,
            sweep_ms: self.sweep_duration.as_millis() as i64,
            truncated: self.truncated,
            unobservable: self.unobservable,
            vanished: self.vanished,
            visited: self.visited,
            wait_ms: self.wait_duration.as_millis() as i64,
        }
    }
}

/// Go api.gen.go:323-354, form-bound query parameters. Absent or unknown
/// values keep the contracts of callers predating each parameter.
#[derive(Debug, Default)]
pub struct FreezeParams {
    pub max_wait_ms: Option<i64>,
    pub mode: Option<String>,
    pub max_cgroups: Option<i64>,
}

/// api.gen.go:723-765 binding order: mode, maxCgroups, maxWaitMs, stopping at
/// the first invalid value. Go answers a binding failure with http.Error and
/// oapi's `Invalid format for parameter ...` text; axum's Query rejection text
/// would be different, so the binding is reproduced here.
pub fn freeze_params(query: Option<&str>) -> Result<FreezeParams, String> {
    let query = go_query_values(query.unwrap_or(""));
    let mut params = FreezeParams::default();
    if let Some(value) = query.get("mode") {
        params.mode = Some(value.clone());
    }
    if let Some(value) = query.get("maxCgroups") {
        params.max_cgroups = Some(bind_integer("maxCgroups", value)?);
    }
    if let Some(value) = query.get("maxWaitMs") {
        params.max_wait_ms = Some(bind_integer("maxWaitMs", value)?);
    }
    Ok(params)
}

fn bind_integer(name: &str, value: &str) -> Result<i64, String> {
    go_parse_int(value).map_err(|error| {
        format!("Invalid format for parameter {name}: error binding string parameter: {error}")
    })
}

/// Go `url.ParseQuery` as `r.URL.Query()` exposes it: malformed pairs (bad
/// percent escape, unescaped `;`) are dropped, `+` is a space, and oapi's
/// scalar binding takes the first value for a repeated key.
pub fn go_query_values(query: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for piece in query.split('&') {
        if piece.is_empty() || piece.contains(';') {
            continue;
        }
        let (raw_key, raw_value) = piece.split_once('=').unwrap_or((piece, ""));
        let (Some(key), Some(value)) = (go_query_unescape(raw_key), go_query_unescape(raw_value))
        else {
            continue;
        };
        values.entry(key).or_insert(value);
    }
    values
}

fn go_query_unescape(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' => {
                let high = (*bytes.get(index + 1)? as char).to_digit(16)?;
                let low = (*bytes.get(index + 2)? as char).to_digit(16)?;
                output.push((high << 4 | low) as u8);
                index += 3;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).ok()
}

/// strconv.ParseInt(value, 10, 64)'s failure text.
pub fn go_parse_int(value: &str) -> Result<i64, String> {
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'+') => (false, &value[1..]),
        Some(b'-') => (true, &value[1..]),
        _ => (false, value),
    };
    let out_of_range = || {
        format!(
            "strconv.ParseInt: parsing {}: value out of range",
            go_quote(value)
        )
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "strconv.ParseInt: parsing {}: invalid syntax",
            go_quote(value)
        ));
    }
    let magnitude = digits.parse::<u128>().map_err(|_| out_of_range())?;
    let limit = if negative {
        1u128 << 63
    } else {
        i64::MAX as u128
    };
    if magnitude > limit {
        return Err(out_of_range());
    }
    Ok(if negative {
        (-(magnitude as i128)) as i64
    } else {
        magnitude as i64
    })
}

/// strconv.Quote's shape for the ASCII query values a caller can send.
pub fn go_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if (character as u32) < 0x20 || character as u32 == 0x7f => {
                quoted.push_str(&format!("\\x{:02x}", character as u32));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_query_binds_in_mode_max_cgroups_max_wait_order() {
        let params = freeze_params(Some("mode=hierarchy&maxCgroups=7&maxWaitMs=25")).unwrap();
        assert_eq!(params.mode.as_deref(), Some("hierarchy"));
        assert_eq!(params.max_cgroups, Some(7));
        assert_eq!(params.max_wait_ms, Some(25));

        // An unknown mode is carried through; the handler maps it to legacy.
        assert_eq!(
            freeze_params(Some("mode=nonsense"))
                .unwrap()
                .mode
                .as_deref(),
            Some("nonsense")
        );
        // Absent or empty query keeps every option at its default.
        let empty = freeze_params(None).unwrap();
        assert!(empty.mode.is_none() && empty.max_cgroups.is_none() && empty.max_wait_ms.is_none());
        assert!(freeze_params(Some("")).unwrap().mode.is_none());
    }

    #[test]
    fn freeze_query_binding_errors_are_go_strconv_text() {
        let error = freeze_params(Some("maxWaitMs=x")).unwrap_err();
        assert_eq!(
            error,
            "Invalid format for parameter maxWaitMs: error binding string parameter: \
             strconv.ParseInt: parsing \"x\": invalid syntax"
        );
        // Binding order stops at maxCgroups before it ever reads maxWaitMs.
        let error = freeze_params(Some("maxCgroups=x&maxWaitMs=y")).unwrap_err();
        assert_eq!(
            error,
            "Invalid format for parameter maxCgroups: error binding string parameter: \
             strconv.ParseInt: parsing \"x\": invalid syntax"
        );
        let error = freeze_params(Some("maxWaitMs=99999999999999999999")).unwrap_err();
        assert_eq!(
            error,
            "Invalid format for parameter maxWaitMs: error binding string parameter: \
             strconv.ParseInt: parsing \"99999999999999999999\": value out of range"
        );
        // Go's ParseQuery drops malformed pairs and keeps the first value.
        let params = freeze_params(Some("maxWaitMs=5&maxWaitMs=x&bad=%zz")).unwrap();
        assert_eq!(params.max_wait_ms, Some(5));
    }

    #[test]
    fn effective_max_cgroups_clamps_like_the_oracle() {
        let zero = FreezeOptions::default();
        assert_eq!(zero.effective_max_cgroups(), DEFAULT_FREEZE_MAX_CGROUPS);
        let negative = FreezeOptions {
            max_cgroups: -3,
            ..Default::default()
        };
        assert_eq!(negative.effective_max_cgroups(), DEFAULT_FREEZE_MAX_CGROUPS);
        let huge = FreezeOptions {
            max_cgroups: i64::MAX,
            ..Default::default()
        };
        assert_eq!(huge.effective_max_cgroups(), DEFAULT_THAW_MAX_CGROUPS);
        let positive = FreezeOptions {
            max_cgroups: 9,
            ..Default::default()
        };
        assert_eq!(positive.effective_max_cgroups(), 9);
    }
}
