//! Go-compatible decoration of one structured log line.
//!
//! Mirrors `MMDSOpts.AddOptsToJSON` (internal/host/mmds.go:41-55) used by the
//! exporter at host commit 51083a83cc0903185d992725ea6bb33683814b29:
//! `json.Unmarshal` into `map[string]any` (numbers become float64), set the two
//! IDs, then `encoding/json.Marshal` (sorted keys, HTML escaping, Go float
//! formatting). Expected bytes below were produced with the Go toolchain using
//! that exact function.

use std::fmt;

#[derive(Debug)]
pub struct JsonError(String);

impl JsonError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for JsonError {}

/// Parse one JSON log line, attach `instanceID`/`envID`, and re-encode it the
/// way Go's `map[string]any` marshaling would.
pub fn decorate(line: &[u8], instance_id: &str, env_id: &str) -> Result<Vec<u8>, JsonError> {
    let value: serde_json::Value = serde_json::from_slice(line)
        .map_err(|error| JsonError::new(format!("invalid JSON: {error}")))?;
    let serde_json::Value::Object(mut object) = value else {
        let kind = match value {
            serde_json::Value::Array(_) => "array",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::Bool(_) => "bool",
            serde_json::Value::Null => "null",
            serde_json::Value::Object(_) => unreachable!(),
        };
        return Err(JsonError::new(format!(
            "json: cannot unmarshal {kind} into Go value of type map[string]interface {{}}"
        )));
    };
    object.insert(
        "instanceID".to_owned(),
        serde_json::Value::String(instance_id.to_owned()),
    );
    object.insert(
        "envID".to_owned(),
        serde_json::Value::String(env_id.to_owned()),
    );
    let mut output = Vec::new();
    write_value(&mut output, &serde_json::Value::Object(object))?;
    Ok(output)
}

fn write_value(output: &mut Vec<u8>, value: &serde_json::Value) -> Result<(), JsonError> {
    match value {
        serde_json::Value::Null => output.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => output.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => output.extend_from_slice(b"false"),
        serde_json::Value::Number(number) => {
            let value = number
                .as_f64()
                .ok_or_else(|| JsonError::new("json: unsupported number"))?;
            output.extend_from_slice(go_float(value)?.as_bytes());
        }
        serde_json::Value::String(text) => write_string(output, text),
        serde_json::Value::Array(items) => {
            output.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_value(output, item)?;
            }
            output.push(b']');
        }
        serde_json::Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort(); // Go sorts map[string]any keys by their UTF-8 bytes.
            output.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_string(output, key);
                output.push(b':');
                write_value(output, &object[key])?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

/// encoding/json stringEncoder with escapeHTML: control characters, quote,
/// backslash, `<`, `>`, `&`, U+2028 and U+2029 are escaped; everything else is
/// emitted as UTF-8.
pub(crate) fn write_string(output: &mut Vec<u8>, value: &str) {
    output.push(b'"');
    for character in value.chars() {
        match character {
            '"' => output.extend_from_slice(br#"\""#),
            '\\' => output.extend_from_slice(br#"\\"#),
            '\u{8}' => output.extend_from_slice(br#"\b"#),
            '\u{c}' => output.extend_from_slice(br#"\f"#),
            '\n' => output.extend_from_slice(br#"\n"#),
            '\r' => output.extend_from_slice(br#"\r"#),
            '\t' => output.extend_from_slice(br#"\t"#),
            '<' => output.extend_from_slice(b"\\u003c"),
            '>' => output.extend_from_slice(b"\\u003e"),
            '&' => output.extend_from_slice(b"\\u0026"),
            '\u{2028}' => output.extend_from_slice(b"\\u2028"),
            '\u{2029}' => output.extend_from_slice(b"\\u2029"),
            character if (character as u32) < 0x20 => {
                output.extend_from_slice(format!("\\u{:04x}", character as u32).as_bytes());
            }
            character => {
                let mut buffer = [0u8; 4];
                output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    output.push(b'"');
}

/// Encode an already-parsed value with Go `encoding/json` semantics (sorted
/// object keys, HTML escaping, float64 number formatting).
pub fn encode_value(value: &serde_json::Value) -> Result<Vec<u8>, JsonError> {
    let mut output = Vec::new();
    write_value(&mut output, value)?;
    Ok(output)
}

/// Go float64 formatting for a bare number, used by the logger's `Dur` field.
pub(crate) fn encode_number(value: f64) -> Result<String, JsonError> {
    go_float(value)
}

/// Go encoding/json floatEncoder: shortest float64 digits (strconv 'e'/'f'
/// with precision -1), switching to exponent form below 1e-6 and at 1e21.
fn go_float(value: f64) -> Result<String, JsonError> {
    if !value.is_finite() {
        return Err(JsonError::new(format!("json: unsupported value: {value}")));
    }
    if value == 0.0 {
        return Ok(if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        });
    }
    let negative = value.is_sign_negative();
    let absolute = value.abs();
    // Rust's LowerExp formatting is the shortest round-tripping decimal, the
    // same digits Go's strconv with precision -1 produces.
    let scientific = format!("{absolute:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .ok_or_else(|| JsonError::new("json: internal float formatting failure"))?;
    let exponent: i32 = exponent
        .parse()
        .map_err(|_| JsonError::new("json: internal float exponent failure"))?;
    let digits = mantissa.replace('.', "");
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if absolute < 1e-6 || absolute >= 1e21 {
        output.push_str(&digits[..1]);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        output.push('e');
        output.push(if exponent >= 0 { '+' } else { '-' });
        output.push_str(&exponent.abs().to_string());
    } else {
        let point = exponent + 1;
        if point <= 0 {
            output.push_str("0.");
            output.push_str(&"0".repeat((-point) as usize));
            output.push_str(&digits);
        } else if point as usize >= digits.len() {
            output.push_str(&digits);
            output.push_str(&"0".repeat(point as usize - digits.len()));
        } else {
            output.push_str(&digits[..point as usize]);
            output.push('.');
            output.push_str(&digits[point as usize..]);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::decorate;

    fn decorated(input: &str) -> String {
        String::from_utf8(decorate(input.as_bytes(), "sid", "tid").unwrap()).unwrap()
    }

    fn rejected(input: &str) -> String {
        decorate(input.as_bytes(), "sid", "tid")
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn sorts_keys_recursively_and_appends_both_ids() {
        assert_eq!(
            decorated(r#"{"z":1,"a":2,"m":{"y":true,"b":null}}"#),
            r#"{"a":2,"envID":"tid","instanceID":"sid","m":{"b":null,"y":true},"z":1}"#
        );
    }

    #[test]
    fn renders_numbers_as_go_float64() {
        assert_eq!(
            decorated(r#"{"n":1,"big":12345678901234567890,"f":1.5,"e":1e21,"e2":1e20}"#),
            r#"{"big":12345678901234567000,"e":1e+21,"e2":100000000000000000000,"envID":"tid","f":1.5,"instanceID":"sid","n":1}"#
        );
    }

    #[test]
    fn applies_the_go_exponent_cutoffs() {
        assert_eq!(
            decorated(
                r#"{"small":1e-7,"tiny":1e-6,"tiny2":9.999999e-7,"huge":1.2345678901234568e20}"#
            ),
            r#"{"envID":"tid","huge":123456789012345680000,"instanceID":"sid","small":1e-7,"tiny":0.000001,"tiny2":9.999999e-7}"#
        );
    }

    #[test]
    fn escapes_html_and_control_characters_like_encoding_json() {
        assert_eq!(
            decorated(r#"{"s":"<&>  \b\f\n\r\t\u0001","u":"héllo"}"#),
            "{\"envID\":\"tid\",\"instanceID\":\"sid\",\"s\":\"\\u003c\\u0026\\u003e\\u2028\\u2029\\b\\f\\n\\r\\t\\u0001\",\"u\":\"héllo\"}"
        );
    }

    #[test]
    fn duplicate_keys_keep_the_last_value() {
        assert_eq!(
            decorated(r#"{"dup":1,"dup":2}"#),
            r#"{"dup":2,"envID":"tid","instanceID":"sid"}"#
        );
    }

    #[test]
    fn overwrites_existing_id_fields_and_keeps_other_metadata() {
        assert_eq!(
            decorated(r#"{"instanceID":"old","envID":"old","address":"x"}"#),
            r#"{"address":"x","envID":"tid","instanceID":"sid"}"#
        );
    }

    #[test]
    fn an_empty_object_only_gains_the_ids() {
        assert_eq!(decorated("{}"), r#"{"envID":"tid","instanceID":"sid"}"#);
    }

    #[test]
    fn non_object_documents_are_rejected() {
        assert!(rejected("[]").contains("array"));
        assert!(rejected(r#""str""#).contains("string"));
        assert!(rejected("123").contains("number"));
        assert!(rejected("{").contains("EOF") || rejected("{").contains("end of file"));
        // Go panics on `null` (assignment to a nil map); the Rust contract is a
        // dropped line instead of a crashed daemon (disclosed difference).
        assert!(decorate(b"null", "sid", "tid").is_err());
    }

    #[test]
    fn out_of_range_numbers_are_rejected_and_underflow_becomes_zero() {
        assert!(decorate(br#"{"num":1e400}"#, "sid", "tid").is_err());
        assert_eq!(
            decorated(r#"{"num":1e-400}"#),
            r#"{"envID":"tid","instanceID":"sid","num":0}"#
        );
    }

    #[test]
    fn large_integers_round_through_float64() {
        assert_eq!(
            decorated(r#"{"num":9007199254740993}"#),
            r#"{"envID":"tid","instanceID":"sid","num":9007199254740992}"#
        );
    }

    #[test]
    fn negative_zero_keeps_go_sign_handling() {
        // Go parses `-0` to float64 negative zero and marshals "-0".
        assert_eq!(
            decorated(r#"{"neg":-0}"#),
            r#"{"envID":"tid","instanceID":"sid","neg":-0}"#
        );
    }

    #[test]
    fn negative_exponents_keep_two_digits_until_cleaned() {
        assert_eq!(
            decorated(r#"{"num":-1e21}"#),
            r#"{"envID":"tid","instanceID":"sid","num":-1e+21}"#
        );
        assert_eq!(
            decorated(r#"{"num":123.456}"#),
            r#"{"envID":"tid","instanceID":"sid","num":123.456}"#
        );
    }
}
