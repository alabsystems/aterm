// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn published_task_passes_the_host_private_reader_without_replacement() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = aterm_tempfile::tempdir().unwrap();
    aterm_uds::private_file::ensure_private_dir(root.path()).unwrap();
    let task = "synthetic task bytes, not a signed constructor";
    publish_task(root.path(), task).unwrap();
    let (bytes, metadata) = aterm_uds::private_file::read_private_with_metadata(
        root.path(),
        "source-proof.task",
        32 * 1024,
    )
    .unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(bytes == task.as_bytes(), "task bytes must remain unchanged");
    assert_eq!(
        publish_task(root.path(), "replacement").unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert!(
        aterm_uds::private_file::read_private(root.path(), "source-proof.task", 32 * 1024).unwrap()
            == bytes,
        "a second publication must preserve the original task"
    );
}

#[test]
fn native_census_can_exceed_sdk_stdout_limit_without_unbounding_it() {
    for (limit, accepted) in [
        (SDK_REPLY_LIMIT, false),
        (SOURCE_PROOF_OBSERVATION_LIMIT, true),
    ] {
        let (mut writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        let payload = vec![b'x'; SDK_REPLY_LIMIT + 1];
        writer.write_all(&payload).unwrap();
        writer.write_all(b"\n").unwrap();
        let fd = reader.as_raw_fd();
        assert_eq!(line(&mut reader, fd, limit).is_ok(), accepted);
    }
}
fn outcome(state: &mut aterm_json::Value, index: usize) -> &mut aterm_json::Map {
    state
        .as_object_mut()
        .unwrap()
        .get_mut("outcomes")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .get_mut(index)
        .unwrap()
        .as_object_mut()
        .unwrap()
}
#[test]
fn committed_ready_loss_needs_same_body_replay_and_physical_current_completion() {
    // Sanitized correlation metadata, not a native permission constructor.
    let lost = aterm_json::json!({"action":"member-ready","operation":"a".repeat(32),"member":"b".repeat(32),
        "preload_sequence":"c".repeat(32),"registration":"d".repeat(32),"credential_generation":4,
        "report":[4,1,2,1],"body_digest":"e".repeat(64),"connection":10,"sequence":21});
    let mut replay = lost.clone();
    replay
        .as_object_mut()
        .unwrap()
        .insert("connection".into(), aterm_json::json!(11));
    replay
        .as_object_mut()
        .unwrap()
        .insert("sequence".into(), aterm_json::json!(23));
    let mut ready = replay.clone();
    ready
        .as_object_mut()
        .unwrap()
        .insert("action".into(), aterm_json::json!("member-rebound-ready"));
    ready
        .as_object_mut()
        .unwrap()
        .insert("sequence".into(), aterm_json::json!(24));
    let bound = aterm_json::json!({"action":"member-work-rebind","member":"b".repeat(32),"connection":11,"sequence":22});
    let mut state = aterm_json::json!({"readyLoss":{"fired":lost},"outcomes":[bound,replay,ready.clone()],"observation":{"ready_members":[ready]}});
    assert!(ready_loss_recovered(&state, 20));
    assert!(
        !ready_loss_recovered(&state, 21),
        "old fault cannot qualify a new leg"
    );
    for field in [
        "operation",
        "preload_sequence",
        "registration",
        "body_digest",
        "connection",
    ] {
        let old = state["outcomes"][1][field].clone();
        outcome(&mut state, 1).insert(field.into(), aterm_json::json!("foreign"));
        assert!(
            !ready_loss_recovered(&state, 20),
            "changed {field} is not an immutable retry"
        );
        outcome(&mut state, 1).insert(field.into(), old);
    }
    outcome(&mut state, 2).insert("action".into(), aterm_json::json!("member-ready"));
    assert!(
        !ready_loss_recovered(&state, 20),
        "historical Ready replay is not physical rebound"
    );
}
#[test]
fn native_response_bound_refuses_oversized_census() {
    let (mut writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || {
        // Reader intentionally refuses and closes once the exact bound is exceeded.
        let _ = writer.write_all(&vec![b'x'; SOURCE_PROOF_OBSERVATION_LIMIT + 1]);
    });
    let fd = reader.as_raw_fd();
    assert!(line(&mut reader, fd, SOURCE_PROOF_OBSERVATION_LIMIT).is_err());
    drop(reader);
    worker.join().unwrap();
}
