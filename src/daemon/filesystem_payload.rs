//! `encoding/json` payloads for the filesystem interceptor lines.
//!
//! zerolog `Interface` calls `encoding/json` on the generated Go structs
//! (internal/services/spec/filesystem/filesystem.pb.go): proto-declared names
//! in the json tags with omitempty on every field, enums as their proto
//! numbers, the modified time as a nested timestamp struct (`seconds`,
//! `nanos`, each omitempty — timestamppb/timestamp.pb.go:178,184), and map
//! keys sorted like encoding/json. The `entry` of a FilesystemEvent is a
//! proto3 optional pointer, omitted when unset (filesystem.pb.go:835); the
//! WatchDirResponse oneof wrappers (:963-978) never reach a line here — the
//! server stream logs only `response: null` on a clean end
//! (interceptor.go:120-122).

use serde_json::{json, Map, Value};

use crate::proto::filesystem as pb;

/// json.Marshal of *timestamppb.Timestamp: seconds and nanos, each omitempty
/// (timestamp.pb.go:178,184).
fn timestamp_value(timestamp: &buffa_types::google::protobuf::Timestamp) -> Value {
    let mut object = Map::new();
    if timestamp.seconds != 0 {
        object.insert("seconds".into(), json!(timestamp.seconds));
    }
    if timestamp.nanos != 0 {
        object.insert("nanos".into(), json!(timestamp.nanos));
    }
    Value::Object(object)
}

/// EntryInfo json tags (filesystem.pb.go:514-530): `type` is the enum number,
/// `symlink_target` is a proto3 optional string omitted when unset, `metadata`
/// is a map emitted with sorted keys like encoding/json, `is_symlink` drops a
/// false.
fn entry_value(entry: &pb::EntryInfo) -> Value {
    let mut object = Map::new();
    if !entry.name.is_empty() {
        object.insert("name".into(), json!(entry.name));
    }
    let file_type = entry.r#type.to_i32();
    if file_type != 0 {
        object.insert("type".into(), json!(file_type));
    }
    if !entry.path.is_empty() {
        object.insert("path".into(), json!(entry.path));
    }
    if entry.size != 0 {
        object.insert("size".into(), json!(entry.size));
    }
    if entry.mode != 0 {
        object.insert("mode".into(), json!(entry.mode));
    }
    if !entry.permissions.is_empty() {
        object.insert("permissions".into(), json!(entry.permissions));
    }
    if !entry.owner.is_empty() {
        object.insert("owner".into(), json!(entry.owner));
    }
    if !entry.group.is_empty() {
        object.insert("group".into(), json!(entry.group));
    }
    if let Some(timestamp) = entry.modified_time.as_option() {
        object.insert("modified_time".into(), timestamp_value(timestamp));
    }
    if let Some(target) = &entry.symlink_target {
        object.insert("symlink_target".into(), json!(target));
    }
    if !entry.metadata.is_empty() {
        let metadata: Map<String, Value> = entry
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect();
        object.insert("metadata".into(), Value::Object(metadata));
    }
    if entry.is_symlink {
        object.insert("is_symlink".into(), json!(entry.is_symlink));
    }
    Value::Object(object)
}

/// FilesystemEvent json tags (filesystem.pb.go:830-835): name, enum number
/// type, and the optional entry pointer omitted when unset.
fn event_value(event: &pb::FilesystemEvent) -> Value {
    let mut object = Map::new();
    if !event.name.is_empty() {
        object.insert("name".into(), json!(event.name));
    }
    let event_type = event.r#type.to_i32();
    if event_type != 0 {
        object.insert("type".into(), json!(event_type));
    }
    if let Some(entry) = event.entry.as_option() {
        object.insert("entry".into(), entry_value(entry));
    }
    Value::Object(object)
}

/// StatRequest/MakeDirRequest/RemoveRequest share the single `path` json tag
/// (filesystem.pb.go:420, :241, :335).
fn path_request(path: &str) -> Value {
    let mut object = Map::new();
    if !path.is_empty() {
        object.insert("path".into(), json!(path));
    }
    Value::Object(object)
}

pub(crate) fn stat_request(request: &pb::StatRequest) -> Value {
    path_request(&request.path)
}

pub(crate) fn make_dir_request(request: &pb::MakeDirRequest) -> Value {
    path_request(&request.path)
}

pub(crate) fn remove_request(request: &pb::RemoveRequest) -> Value {
    path_request(&request.path)
}

/// MoveRequest json tags `source`/`destination`, both omitempty
/// (filesystem.pb.go:139-140).
pub(crate) fn move_request(request: &pb::MoveRequest) -> Value {
    let mut object = Map::new();
    if !request.source.is_empty() {
        object.insert("source".into(), json!(request.source));
    }
    if !request.destination.is_empty() {
        object.insert("destination".into(), json!(request.destination));
    }
    Value::Object(object)
}

/// ListDirRequest json tags `path`/`depth` with a zero depth omitted
/// (filesystem.pb.go:654-655).
pub(crate) fn list_dir_request(request: &pb::ListDirRequest) -> Value {
    let mut object = Map::new();
    if !request.path.is_empty() {
        object.insert("path".into(), json!(request.path));
    }
    if request.depth != 0 {
        object.insert("depth".into(), json!(request.depth));
    }
    Value::Object(object)
}

/// CreateWatcherRequest/WatchDirRequest json tags: path, recursive,
/// include_entry, allow_network_mounts, all omitempty so a false bool is
/// absent (filesystem.pb.go:756-762, :990-996).
pub(crate) fn watch_request(request: &pb::CreateWatcherRequest) -> Value {
    let mut object = Map::new();
    if !request.path.is_empty() {
        object.insert("path".into(), json!(request.path));
    }
    if request.recursive {
        object.insert("recursive".into(), json!(request.recursive));
    }
    if request.include_entry {
        object.insert("include_entry".into(), json!(request.include_entry));
    }
    if request.allow_network_mounts {
        object.insert(
            "allow_network_mounts".into(),
            json!(request.allow_network_mounts),
        );
    }
    Value::Object(object)
}

/// WatchDirRequest shares CreateWatcherRequest's field set and json tags
/// (filesystem.pb.go:756-762 vs :990-996).
pub(crate) fn watch_dir_request(request: &pb::WatchDirRequest) -> Value {
    let converted = pb::CreateWatcherRequest {
        path: request.path.clone(),
        recursive: request.recursive,
        include_entry: request.include_entry,
        allow_network_mounts: request.allow_network_mounts,
        ..Default::default()
    };
    watch_request(&converted)
}

/// GetWatcherEventsRequest/RemoveWatcherRequest share `watcher_id`
/// (filesystem.pb.go:1111, :1205).
pub(crate) fn watcher_id_request(watcher_id: &str) -> Value {
    let mut object = Map::new();
    if !watcher_id.is_empty() {
        object.insert("watcher_id".into(), json!(watcher_id));
    }
    Value::Object(object)
}

/// StatResponse/MakeDirResponse/MoveResponse carry `entry,omitempty`; the
/// handlers always set it on success (stat.go:37, dir.go, move.go), so the
/// key is present.
pub(crate) fn entry_response(entry: &pb::EntryInfo) -> Value {
    json!({ "entry": entry_value(entry) })
}

/// ListDirResponse json tag `entries,omitempty`, so an empty listing marshals
/// as {} (filesystem.pb.go:709).
pub(crate) fn list_dir_response(entries: &[pb::EntryInfo]) -> Value {
    if entries.is_empty() {
        return json!({});
    }
    let encoded: Vec<Value> = entries.iter().map(entry_value).collect();
    json!({ "entries": encoded })
}

/// CreateWatcherResponse json tag `watcher_id,omitempty`
/// (filesystem.pb.go:1064).
pub(crate) fn create_watcher_response(response: &pb::CreateWatcherResponse) -> Value {
    let mut object = Map::new();
    if !response.watcher_id.is_empty() {
        object.insert("watcher_id".into(), json!(response.watcher_id));
    }
    Value::Object(object)
}

/// GetWatcherEventsResponse json tag `events,omitempty`, so an empty drain
/// marshals as {} (filesystem.pb.go:1158).
pub(crate) fn get_watcher_events_response(response: &pb::GetWatcherEventsResponse) -> Value {
    if response.events.is_empty() {
        return json!({});
    }
    let encoded: Vec<Value> = response.events.iter().map(event_value).collect();
    json!({ "events": encoded })
}

/// RemoveResponse and RemoveWatcherResponse have no fields, so both marshal
/// as {} (filesystem.pb.go:377, :1247).
pub(crate) fn empty_response() -> Value {
    json!({})
}
