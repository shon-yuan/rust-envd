use super::*;

fn notify_record(action: u32, name: &str, next: u32) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut record = Vec::new();
    record.extend_from_slice(&next.to_le_bytes());
    record.extend_from_slice(&action.to_le_bytes());
    record.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
    for unit in units {
        record.extend_from_slice(&unit.to_le_bytes());
    }
    record
}

fn buffer(records: &[(u32, &str)]) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut starts = Vec::new();
    for (action, name) in records {
        starts.push(encoded.len() as u32);
        // NextEntryOffset is patched below to the byte distance from this
        // record to the next one (winnt.h FILE_NOTIFY_INFORMATION).
        encoded.extend(notify_record(*action, name, 0));
    }
    for index in 0..starts.len().saturating_sub(1) {
        let next = starts[index + 1] - starts[index];
        let start = starts[index] as usize;
        encoded[start..start + 4].copy_from_slice(&next.to_le_bytes());
    }
    encoded
}

#[test]
fn read_directory_notify_records_map_actions_in_order() {
    let encoded = buffer(&[
        (FILE_ACTION_ADDED, "added"),
        (FILE_ACTION_MODIFIED, "modified"),
        (FILE_ACTION_REMOVED, "removed"),
        (FILE_ACTION_RENAMED_OLD_NAME, "old"),
        (FILE_ACTION_RENAMED_NEW_NAME, "new"),
    ]);
    let records = parse_notify_buffer(&encoded).expect("records");
    let names: Vec<_> = records.iter().map(|record| record.name.as_str()).collect();
    assert_eq!(names, ["added", "modified", "removed", "old", "new"]);
    let kinds: Vec<_> = records
        .iter()
        .map(|record| notify_kind(record.action).expect("known action"))
        .collect();
    assert_eq!(
        kinds,
        [
            (pb::EventType::Create, true),
            (pb::EventType::Write, true),
            (pb::EventType::Remove, false),
            (pb::EventType::Rename, false),
            (pb::EventType::Create, true),
        ]
    );
}

#[test]
fn relative_event_names_use_forward_slashes() {
    let encoded = buffer(&[
        (FILE_ACTION_ADDED, "sub\\file.txt"),
        (FILE_ACTION_MODIFIED, ""),
    ]);
    let records = parse_notify_buffer(&encoded).expect("records");
    assert_eq!(records[0].name, "sub/file.txt");
    assert_eq!(records[1].name, ".");
}

#[test]
fn notify_enum_dir_is_terminal_overflow() {
    let error = notify_error(ERROR_NOTIFY_ENUM_DIR);
    assert_eq!(error.code, connectrpc::ErrorCode::Internal);
    assert_eq!(
        error.message.as_deref(),
        Some("watcher event queue overflow")
    );
    // The zero-byte completion path uses the same terminal error.
    assert_eq!(
        overflow().message.as_deref(),
        Some("watcher event queue overflow")
    );
}

#[test]
fn odd_file_name_length_is_malformed() {
    let mut record = notify_record(FILE_ACTION_ADDED, "ab", 0);
    record[8..12].copy_from_slice(&3u32.to_le_bytes());
    let error = parse_notify_buffer(&record).expect_err("odd length");
    assert_eq!(
        error.message.as_deref(),
        Some("watcher event buffer is malformed")
    );
}

#[test]
fn events_survive_a_failed_entry_lookup() {
    let account = Account {
        name: crate::platform::windows::account::current_user_name(),
        home: std::env::temp_dir().to_string_lossy().into_owned(),
        uid: 0,
        gid: 0,
        groups: Vec::new(),
    };
    let defaults = Defaults {
        user: account.name.clone(),
        workdir: None,
        user_delivered: true,
    };
    let runtime = FilesystemRuntime::new(account, defaults);
    let base = std::env::temp_dir();
    let encoded = buffer(&[(FILE_ACTION_ADDED, "definitely-missing-entry-7f31.txt")]);

    let events = events_from_buffer(&encoded, &base, true, &runtime).expect("events");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "definitely-missing-entry-7f31.txt");
    assert_eq!(events[0].r#type, pb::EventType::Create);
    assert!(events[0].entry.is_unset());
}

#[test]
fn unc_and_remote_drive_paths_are_network() {
    assert!(is_network_path(Path::new(r"\\server\share")));
    assert!(is_network_with(Path::new(r"\\server\share"), |_| 0));
    let local = std::env::temp_dir();
    assert!(!is_network_with(&local, |_| 3));
    assert!(is_network_with(&local, |_| DRIVE_REMOTE));
}
