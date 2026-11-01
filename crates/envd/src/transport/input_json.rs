//! protojson's diagnostics for the small StreamInput request schema.
//! Source of wording and positions: protobuf v1.36.12's JSON decoder.
use serde_json::value::RawValue;

#[derive(Clone, Copy)]
enum Message {
    Request,
    Start,
    Data,
    Keepalive,
    Selector,
    Input,
}
struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Decoder<'a> {
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }
    fn location(&self, pos: usize) -> String {
        let prefix = &self.bytes[..pos.min(self.bytes.len())];
        let line = prefix.iter().filter(|&&b| b == b'\n').count() + 1;
        let start = prefix
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let col = String::from_utf8_lossy(&prefix[start..]).chars().count() + 1;
        format!("(line {line}:{col})")
    }
    fn syntax(&self, pos: usize, detail: impl std::fmt::Display) -> String {
        format!("syntax error {}: {detail}", self.location(pos))
    }
    fn token(&self, pos: usize) -> String {
        let bytes = self.bytes.get(pos..).unwrap_or_default();
        let count = bytes
            .iter()
            .take(32)
            .take_while(|&&b| b.is_ascii_alphanumeric() || b"-+._".contains(&b))
            .count();
        if count != 0 {
            String::from_utf8_lossy(&bytes[..count]).into_owned()
        } else {
            String::from_utf8_lossy(bytes)
                .chars()
                .next()
                .map(String::from)
                .unwrap_or_default()
        }
    }
    fn unexpected(&self) -> String {
        if self.pos >= self.bytes.len() {
            return "unexpected EOF".into();
        }
        self.syntax(
            self.pos,
            format!("unexpected token {}", self.token(self.pos)),
        )
    }
    fn raw(&mut self) -> Result<&'a RawValue, String> {
        self.whitespace();
        let start = self.pos;
        let mut stream =
            serde_json::Deserializer::from_slice(&self.bytes[start..]).into_iter::<&RawValue>();
        match stream.next() {
            Some(Ok(raw)) => {
                self.pos += stream.byte_offset();
                Ok(raw)
            }
            Some(Err(error)) if error.is_eof() => Err("unexpected EOF".into()),
            Some(Err(error)) => {
                let relative = self.bytes[start..]
                    .split(|&b| b == b'\n')
                    .take(error.line().saturating_sub(1))
                    .map(|line| line.len() + 1)
                    .sum::<usize>()
                    + error.column().saturating_sub(1);
                let pos = start + relative;
                let token = self.token(pos);
                let detail = if matches!(self.bytes.get(pos), Some(b'}' | b']' | b',')) {
                    format!("unexpected token {token}")
                } else {
                    format!("invalid value {token}")
                };
                Err(self.syntax(pos, detail))
            }
            None => Err("unexpected EOF".into()),
        }
    }
    fn message(&mut self, kind: Message) -> Result<(), String> {
        self.whitespace();
        if self.bytes.get(self.pos) != Some(&b'{') {
            let pos = self.pos;
            let raw = self.raw()?;
            return Err(self.syntax(pos, format!("unexpected token {}", raw.get())));
        }
        self.pos += 1;
        let mut seen = Vec::<String>::new();
        let mut oneof = false;
        self.whitespace();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.whitespace();
            let name_pos = self.pos;
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.unexpected());
            }
            let raw = self.raw()?;
            let name = serde_json::from_str::<String>(raw.get()).map_err(|_| self.unexpected())?;
            self.whitespace();
            if self.pos == self.bytes.len() {
                return Err("unexpected EOF".into());
            }
            if self.bytes[self.pos] != b':' {
                return Err(self.syntax(
                    self.pos,
                    format!(
                        "unexpected character {}, missing \":\" after field name",
                        self.token(self.pos)
                    ),
                ));
            }
            self.pos += 1;
            self.whitespace();
            let (child, scalar, group) = field(kind, &name);
            let known = child.is_some() || scalar.is_some();
            if known {
                if seen.contains(&name) {
                    return Err(format!(
                        "{}: duplicate field {}",
                        self.location(name_pos),
                        serde_json::to_string(&name).unwrap_or_default()
                    ));
                }
                seen.push(name.clone());
            }
            // Null counts as a seen field but does not set the oneof.
            if self.bytes[self.pos..].starts_with(b"null") {
                self.raw()?;
            } else if known {
                if let Some(group) = group {
                    if oneof {
                        return Err(format!(
                            "{}: error parsing {}, oneof {group} is already set",
                            self.location(name_pos),
                            serde_json::to_string(&name).unwrap_or_default()
                        ));
                    }
                    oneof = true;
                }
                if let Some(child) = child {
                    self.message(child)?;
                } else if let Some(scalar) = scalar {
                    let pos = self.pos;
                    let raw = self.raw()?;
                    if !valid_scalar(raw.get(), scalar) {
                        return Err(format!(
                            "{}: invalid value for {scalar} field {name}: {}",
                            self.location(pos),
                            raw.get()
                        ));
                    }
                }
            } else {
                self.raw()?;
            }
            self.whitespace();
            match self.bytes.get(self.pos) {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b',') => {
                    self.pos += 1;
                }
                None => return Err("unexpected EOF".into()),
                _ => return Err(self.unexpected()),
            }
        }
    }
}
fn field(
    kind: Message,
    name: &str,
) -> (Option<Message>, Option<&'static str>, Option<&'static str>) {
    use Message::*;
    match (kind, name) {
        (Request, "start") => (Some(Start), None, Some("process.StreamInputRequest.event")),
        (Request, "data") => (Some(Data), None, Some("process.StreamInputRequest.event")),
        (Request, "keepalive") => (
            Some(Keepalive),
            None,
            Some("process.StreamInputRequest.event"),
        ),
        (Start, "process") => (Some(Selector), None, None),
        (Data, "input") => (Some(Input), None, None),
        (Selector, "pid") => (
            None,
            Some("uint32"),
            Some("process.ProcessSelector.selector"),
        ),
        (Selector, "tag") => (
            None,
            Some("string"),
            Some("process.ProcessSelector.selector"),
        ),
        (Input, "stdin" | "pty") => (None, Some("bytes"), Some("process.ProcessInput.input")),
        _ => (None, None, None),
    }
}
fn valid_scalar(raw: &str, kind: &str) -> bool {
    if kind == "uint32" {
        let text = serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_owned());
        return uint32(&text).is_some();
    }
    let Ok(text) = serde_json::from_str::<String>(raw) else {
        return false;
    };
    if kind != "bytes" {
        return true;
    }
    decode_bytes(&text).is_ok()
}
fn decode_bytes(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::{
        alphabet,
        engine::{
            general_purpose::{GeneralPurpose, GeneralPurposeConfig},
            DecodePaddingMode,
        },
        Engine,
    };
    let alphabet = if text.contains(['-', '_']) {
        &alphabet::URL_SAFE
    } else {
        &alphabet::STANDARD
    };
    let padding = if text.len().is_multiple_of(4) {
        DecodePaddingMode::RequireCanonical
    } else {
        DecodePaddingMode::RequireNone
    };
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(padding)
        .with_decode_allow_trailing_bits(true);
    GeneralPurpose::new(alphabet, config).decode(text.replace(['\r', '\n'], ""))
}
fn uint32(text: &str) -> Option<u32> {
    // Keep the original decimal digits: f64 rounds nonintegral values near an integer.
    serde_json::from_str::<serde_json::Number>(text).ok()?;
    let (mantissa, exponent) = text.split_once(['e', 'E']).map_or((text, 0), |(m, e)| {
        (m, e.parse::<i64>().unwrap_or(i64::MAX))
    });
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.trim_start_matches('-');
    let fraction = mantissa.split_once('.').map_or(0, |(_, f)| f.len()) as i64;
    let digits = mantissa.replace('.', "");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    if negative {
        return None;
    }
    let scale = exponent.saturating_sub(fraction);
    let trailing_zeros = digits.len() - digits.trim_end_matches('0').len();
    let trimmed = if scale < 0 {
        trailing_zeros.min(scale.unsigned_abs() as usize)
    } else {
        0
    };
    let digits = &digits[..digits.len() - trimmed];
    let scale = scale.saturating_add(trimmed as i64);
    if !(0..=10).contains(&scale) {
        return None;
    }
    let integer = format!("{digits}{}", "0".repeat(scale as usize));
    integer.parse().ok()
}
pub(super) fn normalized_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(input) = value
        .get_mut("data")
        .and_then(|v| v.get_mut("input"))
        .and_then(|v| v.as_object_mut())
    {
        use base64::{engine::general_purpose, Engine};
        for name in ["stdin", "pty"] {
            if let Some(serde_json::Value::String(text)) = input.get_mut(name) {
                let bytes = decode_bytes(text).map_err(|e| e.to_string())?;
                *text = general_purpose::STANDARD.encode(bytes);
            }
        }
    }
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}
pub(super) fn validate(bytes: &[u8]) -> Result<(), String> {
    let mut decoder = Decoder { bytes, pos: 0 };
    decoder.message(Message::Request)?;
    decoder.whitespace();
    if decoder.pos != bytes.len() {
        let pos = decoder.pos;
        let raw = decoder.raw()?;
        return Err(decoder.syntax(pos, format!("unexpected token {}", raw.get())));
    }
    Ok(())
}
// Keep every member in input order: Go retains type errors even when a
// subsequent duplicate would overwrite the field's value.
#[derive(Debug)]
enum TerminalValue {
    Null,
    Bool,
    Number,
    String(String),
    Array(Vec<TerminalValue>),
    Object(Vec<(String, TerminalValue)>),
}
impl TerminalValue {
    fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }
    fn as_str(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    fn as_array(&self) -> Option<&Vec<Self>> {
        if let Self::Array(value) = self {
            Some(value)
        } else {
            None
        }
    }
    fn as_object(&self) -> Option<&Vec<(String, Self)>> {
        if let Self::Object(value) = self {
            Some(value)
        } else {
            None
        }
    }
}
impl<'de> serde::Deserialize<'de> for TerminalValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = TerminalValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(TerminalValue::Null)
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(TerminalValue::Bool)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(TerminalValue::Number)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(TerminalValue::Number)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(TerminalValue::Number)
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(TerminalValue::String(value.into()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element()? {
                    values.push(value);
                }
                Ok(TerminalValue::Array(values))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(pair) = map.next_entry()? {
                    values.push(pair);
                }
                Ok(TerminalValue::Object(values))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
fn terminal_type(value: &TerminalValue) -> &'static str {
    match value {
        TerminalValue::Array(_) => "array",
        TerminalValue::String(_) => "string",
        TerminalValue::Bool => "bool",
        TerminalValue::Object(_) => "object",
        _ => "number",
    }
}
fn terminal_mismatch(value: &TerminalValue, field: &str, target: &str) -> String {
    format!("json: cannot unmarshal {} into Go struct field connectEndStreamMessage.{field} of type {target}",terminal_type(value))
}
fn terminal_object(object: &[(String, TerminalValue)]) -> Result<(), String> {
    for (key, value) in object {
        if value.is_null() {
            continue;
        }
        if key.eq_ignore_ascii_case("metadata") {
            let headers = value
                .as_object()
                .ok_or_else(|| terminal_mismatch(value, "metadata", "http.Header"))?;
            for value in headers
                .iter()
                .map(|(_, value)| value)
                .filter(|v| !v.is_null())
            {
                let values = value
                    .as_array()
                    .ok_or_else(|| terminal_mismatch(value, "metadata", "[]string"))?;
                for value in values.iter().filter(|v| !v.is_null()) {
                    if !value.is_string() {
                        return Err(terminal_mismatch(value, "metadata", "string"));
                    }
                }
            }
        } else if key.eq_ignore_ascii_case("error") {
            terminal_error(value)?;
        }
    }
    Ok(())
}
fn terminal_error(value: &TerminalValue) -> Result<(), String> {
    const ERROR_TYPE: &str = "struct { Code string \"json:\\\"code\\\"\"; Message string \"json:\\\"message\\\"\"; Details []*connect.connectWireDetail \"json:\\\"details\\\"\" }";
    let object = value
        .as_object()
        .ok_or_else(|| terminal_mismatch(value, "error", ERROR_TYPE))?;
    for (key, value) in object {
        if value.is_null() {
            continue;
        }
        if key.eq_ignore_ascii_case("code") || key.eq_ignore_ascii_case("message") {
            if !value.is_string() {
                return Err(terminal_mismatch(value, &format!("error.{key}"), "string"));
            }
        } else if key.eq_ignore_ascii_case("details") {
            let values = value.as_array().ok_or_else(|| {
                terminal_mismatch(value, "error.details", "[]*connect.connectWireDetail")
            })?;
            for value in values.iter().filter(|v| !v.is_null()) {
                let object=value.as_object().ok_or_else(||terminal_mismatch(value,"error.details","struct { Type string \\\"json:\\\\\\\"type\\\\\\\"\\\"; Value string \\\"json:\\\\\\\"value\\\\\\\"\\\" }"))?;
                for (key, value) in object {
                    if value.is_null() {
                        continue;
                    }
                    if key.eq_ignore_ascii_case("type") || key.eq_ignore_ascii_case("value") {
                        let text = value.as_str().ok_or_else(|| {
                            terminal_mismatch(value, &format!("error.details.{key}"), "string")
                        })?;
                        if key.eq_ignore_ascii_case("value") {
                            use base64::{engine::general_purpose, Engine};
                            let text = text.replace(['\r', '\n'], "");
                            if general_purpose::STANDARD.decode(&text).is_err()
                                && general_purpose::STANDARD_NO_PAD.decode(&text).is_err()
                            {
                                let index = text
                                    .bytes()
                                    .position(|b| {
                                        !(b.is_ascii_alphanumeric() || b"+/=".contains(&b))
                                    })
                                    .unwrap_or(text.len().saturating_sub(1));
                                return Err(format!(
                                    "decode base64: illegal base64 data at input byte {index}"
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
pub(super) fn validate_terminal(bytes: &[u8]) -> Result<(), String> {
    match serde_json::from_slice::<TerminalValue>(bytes) {
        Ok(TerminalValue::Object(object)) => terminal_object(&object),
        Ok(TerminalValue::Null) => Ok(()),
        Ok(value) => {
            let kind = match value {
                TerminalValue::Array(_) => "array",
                TerminalValue::String(_) => "string",
                TerminalValue::Bool => "bool",
                _ => "number",
            };
            Err(format!("json: cannot unmarshal {kind} into Go value of type connect.connectEndStreamMessage"))
        }
        Err(error) if error.is_eof() => Err("unexpected end of JSON input".into()),
        Err(error) => {
            let offset = bytes
                .split(|&b| b == b'\n')
                .take(error.line().saturating_sub(1))
                .map(|line| line.len() + 1)
                .sum::<usize>()
                + error.column().saturating_sub(1);
            let character = bytes.get(offset).copied().unwrap_or_default() as char;
            let context = if error.to_string().starts_with("trailing characters") {
                "after top-level value"
            } else {
                "looking for beginning of value"
            };
            Err(format!("invalid character '{character}' {context}"))
        }
    }
}
