// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable v1 records written before native-child custody was recorded.

use super::*;

fn member<'a>(value: &'a mut Value, key: &str) -> &'a mut Value {
    value.as_object_mut().unwrap().get_mut(key).unwrap()
}

fn set(value: &mut Value, key: &str, replacement: Value) {
    value
        .as_object_mut()
        .unwrap()
        .insert(key.into(), replacement);
}

fn historical_body() -> Value {
    let process = WriterProcess::read(std::process::id()).unwrap();
    aterm_json::json!({
        "v": 1, "type": "intent", "id": "0-1a2b3c4d5e6f",
        "op": "ref.update-atomic", "slug": "fixture-dev/app",
        "identity": "dev", "fingerprint": "fixture-fingerprint",
        "key": "atomic refs", "request": request(),
        "target_commitish": null, "body_digest": null, "grant": null,
        "at_secs": SIM_START,
        "writer": {
            "boot": aterm_uds::process::boot_id().unwrap(),
            "pid": process.pid, "start": process.start ^ 1
        },
        "batch": [{"ref": "refs/heads/main", "before": "0".repeat(40), "after": "a".repeat(40)}]
    })
}

fn read_intent(body: &Value) -> Intent {
    let JournalRecord::Intent(intent) = decode(0, 0, &aterm_json::to_vec(body).unwrap()).unwrap()
    else {
        panic!("an intent body must decode as an intent");
    };
    *intent
}

fn history(body: &Value, close: bool) -> Vec<u8> {
    let mut bytes =
        astream_wire::steps::encode_step(0, &aterm_json::to_vec(body).unwrap()).unwrap();
    if close {
        let settled = aterm_json::json!({
            "v":1, "type":"settled", "intent":"0-1a2b3c4d5e6f", "outcome":"never_sent"
        });
        bytes.extend(
            astream_wire::steps::encode_step(1, &aterm_json::to_vec(&settled).unwrap()).unwrap(),
        );
    }
    bytes
}

#[test]
fn historical_settled_atomic_records_allow_new_writes_without_rewriting_history() {
    let root = scratch();
    let journal = journal(root.path(), &fx());
    let original = history(&historical_body(), true);
    write_raw(&journal, &original);
    let (records, end) = journal.read().unwrap();
    assert_eq!(end, JournalEnd::Clean);
    assert_eq!(records.len(), 2);
    assert!(journal.open().unwrap().is_empty());
    assert_eq!(bytes(&journal), original);
    let id = begin(&journal, draft(WriteOp::RefUpdateAtomic, "atomic refs"));
    assert_eq!(id.step(), 2);
    assert!(bytes(&journal).starts_with(&original));
    journal.settle(&id, &Settled::NeverSent).unwrap();
    let mut later = historical_body();
    set(&mut later, "id", Value::from("4-1a2b3c4d5e6f"));
    let record = decode(0, 4, &aterm_json::to_vec(&later).unwrap()).unwrap();
    journal.append(&record).unwrap();
    let later_id = IntentId::parse("4-1a2b3c4d5e6f").unwrap();
    journal.settle(&later_id, &Settled::NeverSent).unwrap();
    assert!(journal.open().unwrap().is_empty());
    assert_eq!(
        begin(&journal, draft(WriteOp::RefUpdateAtomic, "atomic refs")).step(),
        6
    );
    assert!(bytes(&journal).starts_with(&original));
}

#[test]
fn historical_frontend_death_never_invents_native_child_absence() {
    let root = scratch();
    let journal = journal(root.path(), &fx());
    let body = historical_body();
    let original = history(&body, false);
    write_raw(&journal, &original);
    let id = IntentId::parse("0-1a2b3c4d5e6f").unwrap();
    let view = journal.view(&id).unwrap().unwrap();
    assert!(view.open);
    let writer = view.intent.writer.as_ref().unwrap();
    assert!(matches!(writer, IntentWriter::Legacy { .. }));
    assert!(
        !writer.frontend().may_live(),
        "the recorded frontend identity is proven gone"
    );
    assert!(!writer.dead(), "its unrecorded native child may still live");
    assert!(!journal.closable(&view).unwrap());
    assert_eq!(view.intent.refs.len(), 1);
    assert_eq!(view.intent.refs[0].reference, "refs/heads/main");
    assert_eq!(view.intent.refs[0].after, "a".repeat(40));
    assert!(journal.writer_child(&id, std::process::id()).is_err());
    assert_eq!(bytes(&journal), original);

    let mut previous_boot = body;
    set(
        member(&mut previous_boot, "writer"),
        "boot",
        Value::from("previous-boot"),
    );
    assert!(
        read_intent(&previous_boot).writer.unwrap().dead(),
        "another boot proves every old child gone"
    );
}

#[test]
fn historical_and_tracked_custody_round_trip_with_their_complete_ref_batches() {
    let historical = historical_body();
    let intent = read_intent(&historical);
    let encoded: Value =
        aterm_json::from_slice(&encode(&JournalRecord::Intent(Box::new(intent)))).unwrap();
    assert_eq!(encoded, historical);

    let mut tracked = historical.clone();
    let object = tracked.as_object_mut().unwrap();
    let batch = object.remove("batch").unwrap();
    object.insert("refs".into(), batch);
    object.insert(
        "writer".into(),
        aterm_json::to_value(IntentWriter::current().unwrap()).unwrap(),
    );
    let intent = read_intent(&tracked);
    assert!(matches!(
        &intent.writer,
        Some(IntentWriter::Tracked { git: None, .. })
    ));
    assert!(!intent.writer.as_ref().unwrap().dead());
    let encoded: Value =
        aterm_json::from_slice(&encode(&JournalRecord::Intent(Box::new(intent)))).unwrap();
    assert_eq!(encoded, tracked);
}

#[test]
fn ambiguous_or_malformed_recovery_metadata_is_still_a_journal_fault() {
    let original = historical_body();
    let mut bad = Vec::new();
    for field in ["boot", "pid", "start"] {
        let mut value = original.clone();
        member(&mut value, "writer")
            .as_object_mut()
            .unwrap()
            .remove(field);
        bad.push(value);
    }
    let mut value = original.clone();
    set(
        member(&mut value, "writer"),
        "pid",
        Value::from("not a process id"),
    );
    bad.push(value);
    let mut value = original.clone();
    set(
        member(&mut value, "writer"),
        "agh",
        aterm_json::json!({"pid":1,"start":1}),
    );
    bad.push(value);
    let mut value = original.clone();
    set(&mut value, "refs", original["batch"].clone());
    bad.push(value);
    let mut value = original.clone();
    set(
        member(&mut value, "batch")
            .as_array_mut()
            .unwrap()
            .first_mut()
            .unwrap(),
        "after",
        Value::from(17),
    );
    bad.push(value);
    let mut value = original.clone();
    set(
        member(&mut value, "batch")
            .as_array_mut()
            .unwrap()
            .first_mut()
            .unwrap(),
        "unrecognized",
        Value::from(true),
    );
    bad.push(value);
    let mut value = original.clone();
    set(
        &mut value,
        "writer",
        aterm_json::to_value(IntentWriter::current().unwrap()).unwrap(),
    );
    member(&mut value, "writer")
        .as_object_mut()
        .unwrap()
        .remove("git");
    bad.push(value);
    for value in bad {
        let error = decode(73, 0, &aterm_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(
            matches!(error.sub, Sub::JournalFault { at: 73 }),
            "{error:?}"
        );
    }
}
