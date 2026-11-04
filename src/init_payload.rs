//! The `/init` request body, decoded once for every platform.
//!
//! Go decodes the whole body before the handler runs (`init.go:222-235`), so
//! a body it rejects is a 400 even when the rejecting field has no effect on
//! this platform, and the fields it accepts must decode exactly the same way
//! on both targets. The Windows daemon used to carry its own walker; the two
//! implementations disagreed on the values Go hands a `json.Unmarshaler`
//! unresolved — an escaped `timestamp` or `accessToken` is a decode error —
//! which the native Windows suite caught. This module is now the single
//! implementation, so that class of divergence cannot recur.
//!
//! Nothing here touches a platform API: the types are the oracle's
//! `api.gen.go` shapes, and the fields a platform ignores are still validated
//! and kept (Linux reads them all; Windows reads the subset with effects).

use std::collections::BTreeMap;

use serde::de::IgnoredAny;
use serde::Deserialize;
use serde_json::value::RawValue;
use zeroize::Zeroizing;

use crate::compose_decode::folded_field;

/// Oracle `api.gen.go:245-252` VolumeMount: two plain non-omitempty strings.
/// Go decodes JSON null for a string field as its zero value and a null ARRAY
/// element as the zero struct, both without error; a wrong JSON type fails
/// the whole-body decode. Field names fold ASCII case like `encoding/json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))]
pub struct VolumeMount {
    pub nfs_target: String,
    pub path: String,
}

impl<'de> serde::Deserialize<'de> for VolumeMount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct VolumeMountVisitor;
        impl<'de> serde::de::Visitor<'de> for VolumeMountVisitor {
            type Value = VolumeMount;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a volume mount object or null")
            }
            // A null array element decodes as the zero struct, like Go.
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(VolumeMount::default())
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut mount = VolumeMount::default();
                while let Some(key) = map.next_key::<String>()? {
                    match folded_field(&key).as_str() {
                        "NFS_TARGET" => {
                            mount.nfs_target =
                                map.next_value::<Option<String>>()?.unwrap_or_default();
                        }
                        "PATH" => {
                            mount.path = map.next_value::<Option<String>>()?.unwrap_or_default();
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(mount)
            }
        }
        deserializer.deserialize_any(VolumeMountVisitor)
    }
}

/// The `api.gen.go` init body. `Default` is the zero struct Go decodes from an
/// empty body or a literal `null`.
///
/// The fields a given platform does not act on are still decoded: Go's
/// decoder validates the whole body, so a wrong-typed `caBundle` is a 400 even
/// where the value is never used.
#[derive(Default)]
#[cfg_attr(windows, allow(dead_code))]
pub struct InitRequest {
    pub access_token: Option<Zeroizing<String>>,
    pub env_vars: Option<BTreeMap<String, String>>,
    pub hyperloop_ip: Option<String>,
    pub default_user: Option<String>,
    pub default_workdir: Option<String>,
    pub cpu_count: Option<i64>,
    // Go api.gen.go:386: *time.Time json:"timestamp,omitempty", decoded as
    // RFC3339 into nanoseconds since the epoch at full precision.
    pub timestamp: Option<i128>,
    // Go api.gen.go:362: *string json:"caBundle,omitempty"; "" behaves as nil
    // at the init.go:474 gate.
    pub ca_bundle: Option<String>,
    // Go api.gen.go:383: *PostInitJSONBodyThaw json:"thaw,omitempty" with the
    // enum values "inline"/"skip" (api.gen.go:68-79). Go decodes any string;
    // only "skip" withholds the thaw (init.go:695-697).
    pub thaw: Option<String>,
    // Go api.gen.go:380: *string json:"lifecycleID,omitempty"; nil behaves as
    // "" everywhere the ledger and the failure memo read it (init.go:797,
    // :1009-1015 derefString).
    pub lifecycle_id: Option<String>,
    // Go api.gen.go:387: *[]VolumeMount json:"volumeMounts,omitempty".
    // Absent and JSON null decode to None (the nil pointer that skips
    // setupNFS, init.go:480); an empty slice is Some(vec![]) — setupNFS still
    // runs for it, including the memoized-failure short-circuit; any other
    // JSON type fails the whole-body decode as 400.
    pub volume_mounts: Option<Vec<VolumeMount>>,
}

// Go encoding/json folds ASCII case plus Unicode simple-fold equivalents.
// For these ASCII field names, only long-s and Kelvin sign add equivalents.
impl<'de> Deserialize<'de> for InitRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InitVisitor;
        impl<'de> serde::de::Visitor<'de> for InitVisitor {
            type Value = InitRequest;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an init object or null")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(InitRequest::default())
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut init = InitRequest::default();
                while let Some(key) = map.next_key::<String>()? {
                    match folded_field(&key).as_str() {
                        "ACCESSTOKEN" => {
                            let raw = map.next_value::<&RawValue>()?;
                            init.access_token =
                                parse_token(Some(raw)).map_err(serde::de::Error::custom)?;
                        }
                        "ENVVARS" => match map.next_value::<Option<BTreeMap<String, String>>>()? {
                            Some(values) => init
                                .env_vars
                                .get_or_insert_with(BTreeMap::new)
                                .extend(values),
                            None => init.env_vars = None,
                        },
                        "DEFAULTUSER" => init.default_user = map.next_value()?,
                        "DEFAULTWORKDIR" => init.default_workdir = map.next_value()?,
                        "CPUCOUNT" => {
                            let raw = map.next_value::<&RawValue>()?;
                            init.cpu_count = if raw.get() == "null" {
                                None
                            } else {
                                Some(raw.get().parse::<i64>().map_err(serde::de::Error::custom)?)
                            };
                        }
                        // Go api.gen.go:376: *string json:"hyperloopIP,omitempty"
                        "HYPERLOOPIP" => init.hyperloop_ip = map.next_value()?,
                        // Go api.gen.go:386: *time.Time json:"timestamp,omitempty"
                        "TIMESTAMP" => {
                            let raw = map.next_value::<&RawValue>()?;
                            init.timestamp =
                                decode_timestamp(raw).map_err(serde::de::Error::custom)?;
                        }
                        // Go api.gen.go:362: *string json:"caBundle,omitempty".
                        // Any other JSON type is a decode error failing the
                        // whole /init as a 400.
                        "CABUNDLE" => init.ca_bundle = map.next_value::<Option<String>>()?,
                        // Go api.gen.go:383: any non-string JSON value is a
                        // decode error failing the whole /init as a 400.
                        "THAW" => init.thaw = map.next_value::<Option<String>>()?,
                        // Go api.gen.go:380: *string json:"lifecycleID";
                        // null/absent is the nil pointer, any other JSON type
                        // a decode error.
                        "LIFECYCLEID" => init.lifecycle_id = map.next_value::<Option<String>>()?,
                        // Go api.gen.go:387 decodes VolumeMounts as
                        // *[]VolumeMount: absent/null is None (init.go:480
                        // treats it as absent), [] is Some(vec![]) and still
                        // runs setupNFS (init.go:796-864), a null ELEMENT is
                        // the zero VolumeMount, and any other JSON type fails
                        // the whole-body decode as 400.
                        "VOLUMEMOUNTS" => {
                            init.volume_mounts = map.next_value::<Option<Vec<VolumeMount>>>()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(init)
            }
        }
        deserializer.deserialize_any(InitVisitor)
    }
}

/// Reads the whole init body without a size cap, the way the oracle's handler
/// does: the decoder sees the stream to its end and a failed read is the
/// handler's 400. Both platforms stream the same way, so a body size that one
/// accepts is never refused by the other.
pub async fn collect_body(body: axum::body::Body) -> Result<Zeroizing<Vec<u8>>, axum::Error> {
    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut chunks = Vec::new();
    let mut length = 0usize;
    while let Some(chunk) = stream.next().await {
        // Consume Bytes into its Vec rather than copy then free its allocation.
        // Unique transport allocations transfer directly; externally shared/static
        // Bytes necessarily copy, and their other owners retain responsibility.
        let chunk = Zeroizing::new(Vec::<u8>::from(chunk?));
        length = length
            .checked_add(chunk.len())
            .ok_or_else(|| axum::Error::new(std::io::Error::other("init body length overflow")))?;
        chunks.push(chunk);
    }
    if chunks.len() == 1 {
        return Ok(chunks.remove(0));
    }
    // Allocate once so growing a Vec never releases an unwiped partial body.
    // Every consumed chunk is wiped when dropped, including on read errors and cancellation.
    let mut body = Zeroizing::new(Vec::with_capacity(length));
    for chunk in chunks {
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Decodes an init body the way Go's handler does: an empty body and a literal
/// `null` are the zero struct, and any decode error fails the request.
pub fn decode(body: &[u8]) -> Result<InitRequest, serde_json::Error> {
    if body.is_empty() {
        return Ok(InitRequest::default());
    }
    serde_json::from_slice(body)
}

/// Go api.gen.go:386 decodes `timestamp` as *time.Time: a JSON string holding
/// RFC3339, `null` meaning absent, any other JSON type a decode error (which
/// fails the whole /init as a 400).
fn decode_timestamp(raw: &RawValue) -> Result<Option<i128>, &'static str> {
    let text = raw.get();
    if text == "null" {
        return Ok(None);
    }
    // Go's encoding/json hands a json.Unmarshaler the RAW bytes of the value,
    // so time.Time.UnmarshalJSON parses the still-escaped text: escape
    // sequences are NOT decoded first, and any backslash fails the RFC3339
    // parse (probed against the pinned oracle in a real guest: an escaped
    // timestamp answers 400, while an unescaped fraction separator is still
    // accepted). A non-string value fails the same way Go's quoted-string
    // guard does.
    if text.len() < 2 || !text.starts_with('"') || !text.ends_with('"') {
        return Err("invalid timestamp JSON string");
    }
    crate::clock::rfc3339_nanos(&text[1..text.len() - 1])
        .map(Some)
        .ok_or("invalid RFC3339 timestamp")
}

/// Go `SecureToken.UnmarshalJSON`: a JSON string with no escape sequences and
/// no empty token; null is the nil pointer.
fn parse_token(raw: Option<&RawValue>) -> Result<Option<Zeroizing<String>>, &'static str> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw.get();
    if raw == "null" {
        return Ok(None);
    }
    if !raw.starts_with('"') || !raw.ends_with('"') {
        return Err("invalid secure token JSON string");
    }
    let token = &raw[1..raw.len() - 1];
    if token.is_empty() {
        return Err("empty token not allowed");
    }
    if token.contains('\\') {
        return Err("invalid secure token: unexpected escape sequence");
    }
    Ok(Some(Zeroizing::new(token.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_text(body: &str) -> Result<InitRequest, serde_json::Error> {
        decode(body.as_bytes())
    }

    #[test]
    fn empty_and_null_bodies_are_the_zero_struct() {
        for body in ["", "null"] {
            let init = decode_text(body).expect("zero body decodes");
            assert!(init.env_vars.is_none(), "{body:?}");
            assert!(init.access_token.is_none(), "{body:?}");
            assert!(init.timestamp.is_none(), "{body:?}");
            assert!(init.volume_mounts.is_none(), "{body:?}");
        }
    }

    #[test]
    fn a_complete_body_decodes_every_field() {
        let init = decode_text(
            r#"{"accessToken":"t0k","envVars":{"A":"1","B":"2"},
                "defaultUser":"user","defaultWorkdir":"/w","cpuCount":4,
                "timestamp":"2026-10-31T01:23:45.5Z","caBundle":"pem",
                "thaw":"skip","lifecycleID":"l1","hyperloopIP":"169.254.0.20",
                "volumeMounts":[{"nfs_target":"10.0.0.1:/x","path":"/mnt"}]}"#,
        )
        .expect("valid body decodes");
        assert_eq!(
            init.access_token.as_deref().map(String::as_str),
            Some("t0k")
        );
        assert_eq!(init.env_vars.as_ref().map(|e| e.len()), Some(2));
        assert_eq!(init.default_user.as_deref(), Some("user"));
        assert_eq!(init.default_workdir.as_deref(), Some("/w"));
        assert_eq!(init.cpu_count, Some(4));
        assert_eq!(init.timestamp, Some(1_793_409_825_500_000_000));
        assert_eq!(init.ca_bundle.as_deref(), Some("pem"));
        assert_eq!(init.thaw.as_deref(), Some("skip"));
        assert_eq!(init.lifecycle_id.as_deref(), Some("l1"));
        assert_eq!(init.hyperloop_ip.as_deref(), Some("169.254.0.20"));
        let mounts = init.volume_mounts.expect("mounts");
        assert_eq!(mounts[0].nfs_target, "10.0.0.1:/x");
        assert_eq!(mounts[0].path, "/mnt");
    }

    #[test]
    fn untouched_null_members_stay_absent() {
        let init = decode_text(
            r#"{"accessToken":null,"timestamp":null,"cpuCount":null,
                "volumeMounts":null,"envVars":null,"caBundle":null}"#,
        )
        .expect("nulls decode");
        assert!(init.access_token.is_none());
        assert!(init.timestamp.is_none());
        assert!(init.cpu_count.is_none());
        assert!(init.volume_mounts.is_none());
        assert!(init.env_vars.is_none());
        assert!(init.ca_bundle.is_none());
    }

    #[test]
    fn volume_mounts_accept_empty_and_null_elements() {
        let empty = decode_text(r#"{"volumeMounts":[]}"#).expect("empty array");
        assert_eq!(empty.volume_mounts.as_ref().map(Vec::len), Some(0));
        let null_element =
            decode_text(r#"{"volumeMounts":[null]}"#).expect("null element is the zero struct");
        let mounts = null_element.volume_mounts.expect("mounts");
        assert_eq!(mounts, vec![VolumeMount::default()]);
    }

    #[test]
    fn field_names_fold_like_go() {
        // Long-s folds to S, so this member is accessToken. Rust's own escape
        // builds the key, which keeps the JSON text free of ambiguous
        // re-encodings. (None of the oracle's init members contains a `k`, so
        // the Kelvin fold has nothing to apply to here; the shared folder is
        // tested directly in its own module.)
        let body = concat!("{\"acce", "\u{17f}", "sToken\":\"t0k\"}");
        let init = decode_text(body).expect("folded names decode");
        assert_eq!(
            init.access_token.as_deref().map(String::as_str),
            Some("t0k")
        );
    }

    #[test]
    fn repeated_env_vars_merge_like_go() {
        let init = decode_text(r#"{"envVars":{"A":"1"},"envVars":{"B":"2"}}"#)
            .expect("repeated members decode");
        let envs = init.env_vars.expect("envs");
        assert_eq!(envs.get("A").map(String::as_str), Some("1"));
        assert_eq!(envs.get("B").map(String::as_str), Some("2"));
    }

    #[test]
    fn raw_json_members_reject_escapes_like_go() {
        // Go hands time.Time and SecureToken the still-escaped bytes.
        for body in [
            r#"{"timestamp":"2026-10-31T01:23:45\u002e5Z"}"#,
            r#"{"timestamp":"\u0032\u0030\u0032\u0036-10-31T01:23:45.5Z"}"#,
            r#"{"accessToken":"a\u002db"}"#,
        ] {
            assert!(decode_text(body).is_err(), "{body}");
        }
        assert!(decode_text(r#"{"timestamp":"2026-10-31T01:23:45.5Z"}"#).is_ok());
    }

    #[test]
    fn wrong_typed_fields_fail_the_whole_body() {
        for body in [
            r#"{"cpuCount":"x"}"#,
            r#"{"cpuCount":1.5}"#,
            r#"{"volumeMounts":"x"}"#,
            r#"{"volumeMounts":[{"nfs_target":1}]}"#,
            r#"{"caBundle":5}"#,
            r#"{"lifecycleID":{}}"#,
            r#"{"envVars":{"A":1}}"#,
            r#"{"thaw":1}"#,
            r#"{"defaultUser":[]}"#,
            r#"[]"#,
            r#"42"#,
        ] {
            assert!(decode_text(body).is_err(), "{body}");
        }
    }

    #[test]
    fn malformed_timestamps_and_tokens_are_rejected() {
        for body in [
            r#"{"timestamp":12345}"#,
            r#"{"timestamp":"2026-10-31t01:23:45z"}"#,
            r#"{"timestamp":"2026-10-31T01:23:60Z"}"#,
            r#"{"accessToken":""}"#,
            r#"{"accessToken":5}"#,
        ] {
            assert!(decode_text(body).is_err(), "{body}");
        }
    }
}
