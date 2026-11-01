//! `encoding/json` payloads for the process interceptor lines.
//!
//! zerolog `Interface` calls `encoding/json` on the generated Go structs
//! (internal/services/spec/process/process.pb.go): proto-declared names in the
//! json tags with omitempty, oneof wrappers serialized under their Go field
//! names (`Selector`, `Input`, `Stdin`, `Pty`) with no json tags of their own,
//! enums as their proto numbers, and bytes as standard base64.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Map, Value};

use crate::proto::process as pb;

/// A `*ProcessSelector`: the oneof under the Go interface field name
/// `Selector`; the wrapper structs (process.pb.go:1304-1310) carry no json
/// tags, so `Pid`/`Tag` are Go-identifier-cased and always present, even when
/// zero.
fn selector_field(selector: &pb::ProcessSelector) -> Value {
    let wrapped = match &selector.selector {
        Some(pb::process_selector::Selector::Pid(pid)) => json!({ "Pid": pid }),
        Some(pb::process_selector::Selector::Tag(tag)) => json!({ "Tag": tag }),
        None => Value::Null,
    };
    json!({ "Selector": wrapped })
}

/// json.Marshal of a nil []byte is null, and proto3 unmarshals an absent or
/// empty bytes field to nil; a non-nil empty slice would render as "".
fn bytes_value(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        Value::String(STANDARD.encode(bytes))
    }
}

/// A `*ProcessInput`: the oneof under the Go interface field name `Input`
/// (process.pb.go:867-872).
fn input_field(input: &pb::ProcessInput) -> Value {
    let wrapped = match &input.input {
        Some(pb::process_input::Input::Stdin(bytes)) => json!({ "Stdin": bytes_value(bytes) }),
        Some(pb::process_input::Input::Pty(bytes)) => json!({ "Pty": bytes_value(bytes) }),
        None => Value::Null,
    };
    json!({ "Input": wrapped })
}

/// ProcessConfig json tags: cmd/args/envs/cwd all omitempty; the envs map is
/// emitted with sorted keys like encoding/json.
fn config_value(config: &pb::ProcessConfig) -> Value {
    let mut object = Map::new();
    if !config.cmd.is_empty() {
        object.insert("cmd".into(), json!(config.cmd));
    }
    if !config.args.is_empty() {
        object.insert("args".into(), json!(config.args));
    }
    if !config.envs.is_empty() {
        let envs: Map<String, Value> = config
            .envs
            .iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect();
        object.insert("envs".into(), Value::Object(envs));
    }
    if let Some(cwd) = &config.cwd {
        object.insert("cwd".into(), json!(cwd));
    }
    Value::Object(object)
}

/// PTY json tag `size,omitempty`; PTY_Size cols/rows omitempty
/// (process.pb.go:77, :1321-1322).
fn pty_field(pty: &pb::PTY) -> Value {
    let mut object = Map::new();
    if let Some(size) = pty.size.as_option() {
        let mut sized = Map::new();
        if size.cols != 0 {
            sized.insert("cols".into(), json!(size.cols));
        }
        if size.rows != 0 {
            sized.insert("rows".into(), json!(size.rows));
        }
        object.insert("size".into(), Value::Object(sized));
    }
    Value::Object(object)
}

fn selector_ref(selector: &pb::ProcessSelector) -> (&'static str, Value) {
    ("process", selector_field(selector))
}

pub(crate) fn list_request() -> Value {
    json!({})
}

/// ListResponse json tag `processes,omitempty`; ProcessInfo config/pid/tag
/// omitempty (process.pb.go:233-235, :296).
pub(crate) fn list_response(processes: &[pb::ProcessInfo]) -> Value {
    if processes.is_empty() {
        return json!({});
    }
    let infos: Vec<Value> = processes
        .iter()
        .map(|info| {
            let mut object = Map::new();
            if let Some(config) = info.config.as_option() {
                object.insert("config".into(), config_value(config));
            }
            if info.pid != 0 {
                object.insert("pid".into(), json!(info.pid));
            }
            if let Some(tag) = &info.tag {
                object.insert("tag".into(), json!(tag));
            }
            Value::Object(object)
        })
        .collect();
    json!({ "processes": infos })
}

/// StartRequest json tags: process/pty/tag/stdin all omitempty, so a set
/// `stdin: false` is present while an unset one is absent
/// (process.pb.go:343-348).
pub(crate) fn start_request(request: &pb::StartRequest) -> Value {
    let mut object = Map::new();
    if let Some(process) = request.process.as_option() {
        object.insert("process".into(), config_value(process));
    }
    if let Some(pty) = request.pty.as_option() {
        object.insert("pty".into(), pty_field(pty));
    }
    if let Some(tag) = &request.tag {
        object.insert("tag".into(), json!(tag));
    }
    if let Some(stdin) = request.stdin {
        object.insert("stdin".into(), json!(stdin));
    }
    Value::Object(object)
}

pub(crate) fn connect_request(process: &pb::ProcessSelector) -> Value {
    let (key, value) = selector_ref(process);
    json!({ key: value })
}

pub(crate) fn close_stdin_request(process: &pb::ProcessSelector) -> Value {
    let (key, value) = selector_ref(process);
    json!({ key: value })
}

pub(crate) fn update_request(request: &pb::UpdateRequest) -> Value {
    let mut object = Map::new();
    if let Some(process) = request.process.as_option() {
        object.insert("process".into(), selector_field(process));
    }
    if let Some(pty) = request.pty.as_option() {
        object.insert("pty".into(), pty_field(pty));
    }
    Value::Object(object)
}

pub(crate) fn send_input_request(request: &pb::SendInputRequest) -> Value {
    let mut object = Map::new();
    if let Some(process) = request.process.as_option() {
        object.insert("process".into(), selector_field(process));
    }
    // `input` is a required field on the message; Go would omit only a nil
    // pointer, which the buffa shape cannot represent.
    object.insert("input".into(), input_field(&request.input));
    Value::Object(object)
}

/// SendSignalRequest json tags: signal omitempty on the enum number
/// (process.pb.go:1016-1017).
pub(crate) fn send_signal_request(process: &pb::ProcessSelector, signal: i32) -> Value {
    let mut object = Map::new();
    object.insert("process".into(), selector_field(process));
    if signal != 0 {
        object.insert("signal".into(), json!(signal));
    }
    Value::Object(object)
}

/// The empty response messages marshal as `{}`.
pub(crate) fn empty_response() -> Value {
    json!({})
}
