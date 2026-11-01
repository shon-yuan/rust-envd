use buffa::Message;
use coorworker_envd::proto::{filesystem, legacyfilesystem, process, upgrade};
use serde_json::{json, Value};

#[test]
fn process_preserves_optional_stdin_and_binary_output() {
    let absent: process::StartRequest = serde_json::from_value(json!({})).unwrap();
    let disabled: process::StartRequest = serde_json::from_value(json!({"stdin": false})).unwrap();
    let enabled: process::StartRequest = serde_json::from_value(json!({"stdin": true})).unwrap();
    assert_eq!(absent.stdin, None);
    assert_eq!(disabled.stdin, Some(false));
    assert_eq!(enabled.stdin, Some(true));
    assert_ne!(absent.encode_to_vec(), disabled.encode_to_vec());

    // stdout bytes are 0xff, 0x00, 0x80: never round-trip through a UTF-8 string.
    let input = json!({"event": {"data": {"stdout": "/wCA"}}});
    let response: process::StartResponse = serde_json::from_value(input.clone()).unwrap();
    let encoded = response.encode_to_vec();
    assert!(encoded.windows(3).any(|bytes| bytes == [255, 0, 128]));
    let decoded = process::StartResponse::decode_from_slice(&encoded).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), input);
}

#[test]
fn oneof_presence_and_signed_exit_code_match_protobuf_json() {
    let selector: process::ProcessSelector = serde_json::from_value(json!({"pid": 0})).unwrap();
    assert_eq!(selector.encode_to_vec(), [8, 0]);
    let end: process::StartResponse = serde_json::from_value(json!({
        "event": {"end": {"exitCode": -15, "exited": true, "status": "signal", "error": ""}}
    }))
    .unwrap();
    let value = serde_json::to_value(
        process::StartResponse::decode_from_slice(&end.encode_to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(value["event"]["end"]["exitCode"], -15);
    assert_eq!(value["event"]["end"]["error"], "");
    // sint32 -15 is zigzag 29; fixed bytes avoid a self-roundtrip masking int32 drift.
    let terminal: process::process_event::EndEvent =
        serde_json::from_value(json!({"exitCode": -15})).unwrap();
    assert_eq!(terminal.encode_to_vec(), [0x08, 0x1d]);
}

#[test]
fn filesystem_preserves_large_int64_timestamp_and_metadata() {
    let input = json!({
        "name": "link", "type": "FILE_TYPE_FILE", "path": "/tmp/link",
        "size": "9007199254740993", "mode": 420, "permissions": "-rw-r--r--",
        "owner": "root", "group": "root", "modifiedTime": "2026-10-30T08:28:35.123456789Z",
        "symlinkTarget": "", "metadata": {"source": "fixture"}, "isSymlink": true
    });
    let entry: filesystem::EntryInfo = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(entry.size, 9_007_199_254_740_993);
    let decoded = filesystem::EntryInfo::decode_from_slice(&entry.encode_to_vec()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), input);
}

#[test]
fn upgrade_carries_process_watchers_defaults_and_helper_pids() {
    let input = json!({
        "schema": 3, "fromVer": "0.10.2",
        "processes": [{"pid": 17, "hasTag": true, "tag": "", "stdinFd": 202,
            "timeoutMs": "9007199254740993"}],
        "watchers": [{"id": "w-fixture", "path": "/tmp", "recursive": true,
            "includeEntryInfo": true, "pendingEvents": [{"name": "file", "type": "EVENT_TYPE_CREATE"}]}],
        "mounts": [{"path": "/mnt/test", "lifecycleId": "life"}],
        "forwards": [{"key": "17-8000", "port": 8000, "listenerPid": 17,
            "family": 4, "socatPid": 18}],
        "guestFrozenCgroups": ["customer/paused"],
        "defaults": {"user": "root", "hasWorkdir": true, "workdir": "/tmp"},
        "helperPids": [19]
    });
    let state: upgrade::HandoverState = serde_json::from_value(input.clone()).unwrap();
    let decoded = upgrade::HandoverState::decode_from_slice(&state.encode_to_vec()).unwrap();
    assert!(decoded.processes[0].has_tag);
    assert!(decoded.processes[0].tag.is_empty());
    // tag is a plain proto3 string; hasTag carries presence for empty tags.
    let mut expected = input;
    expected["processes"][0]
        .as_object_mut()
        .unwrap()
        .remove("tag");
    assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
}

#[test]
fn legacy_filesystem_keeps_the_old_entry_shape() {
    let entry: legacyfilesystem::EntryInfo = serde_json::from_value(json!({
        "name": "file", "type": "FILE_TYPE_FILE", "path": "/tmp/file"
    }))
    .unwrap();
    let value: Value = serde_json::to_value(entry).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert!(value.get("size").is_none());
}
