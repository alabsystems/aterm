// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn store() -> (aterm_tempfile::TempDir, Store) {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let store = Store::at(dir.path());
    (dir, store)
}

#[test]
fn an_absent_store_reads_empty_and_writes_nothing_until_a_note() {
    let (_dir, store) = store();
    assert_eq!(store.load(), Ok(Records::default()));
    assert!(!store.dir().exists());
}

#[test]
fn a_note_writes_the_record_and_the_wake() {
    let (_dir, store) = store();
    let recording = aterm_durable::seam::Recording::start();
    store
        .note("codex", How::Login, Outcome::Worked, 100)
        .expect("note");
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered],
        "the record and its wake marker are only ordered"
    );
    let r = store.load().expect("load").of("codex");
    assert_eq!((r.used_at, r.used_how), (Some(100), Some(How::Login)));
    assert!(!r.ended());
    assert!(store.wake_path().exists());

    store
        .note("codex", How::Review, Outcome::Ended(Ended::ReviewAuth), 200)
        .expect("note");
    let r = store.load().expect("load").of("codex");
    assert!(r.ended());
    assert_eq!(r.ended_why, Some(Ended::ReviewAuth));
    assert_eq!(r.used_at, Some(100), "an end is not a use");

    // A use after the end lifts it.
    store
        .note("codex", How::Review, Outcome::Worked, 300)
        .expect("note");
    assert!(!store.load().expect("load").of("codex").ended());
}

#[test]
fn a_limit_is_a_use_and_lifts_at_its_time() {
    let mut r = Record::default();
    r.apply(How::Review, Outcome::Limited(Some(500)), 100);
    assert_eq!((r.used_at, r.limited_until), (Some(100), Some(500)));
    r.apply(How::Review, Outcome::Worked, 400);
    assert_eq!(r.limited_until, Some(500), "a use before the time keeps it");
    r.apply(How::Review, Outcome::Worked, 600);
    assert_eq!(r.limited_until, None);
}

#[test]
fn another_schema_is_never_written_over_and_a_torn_one_is_replaced() {
    let (_dir, store) = store();
    store.ensure().expect("dir");
    let path = store.dir().join(RECORD_FILE);
    std::fs::write(&path, "{\"schema\":2,\"clis\":{}}").expect("write");
    assert_eq!(store.load(), Err(Unread::NotOurs(2)));
    store
        .note("claude", How::Review, Outcome::Worked, 1)
        .expect("note");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "{\"schema\":2,\"clis\":{}}"
    );

    std::fs::write(&path, "not json").expect("write");
    store
        .note("claude", How::Review, Outcome::Worked, 2)
        .expect("note");
    assert_eq!(store.load().expect("load").of("claude").used_at, Some(2));
}

#[test]
fn a_record_keeps_only_known_clis_and_closed_words() {
    let text = "{\"schema\":1,\"clis\":{\"gpt\":{\"used_at\":1},\"codex\":{\"ended_at\":5,\
                \"ended_why\":\"Your token sk-abc was revoked\",\"used_how\":\"served\"}}}";
    let records = Records::parse(text).expect("parse");
    assert!(!records.by_id.contains_key("gpt"));
    let codex = records.of("codex");
    assert_eq!(
        (codex.ended_at, codex.ended_why, codex.used_how),
        (Some(5), None, None)
    );
    assert_eq!(Records::parse(&records.render()), Ok(records));
}

#[test]
fn a_legacy_observation_can_finish_but_cannot_own_a_replacement() {
    let (_dir, store) = store();
    store.ensure().expect("dir");
    std::fs::write(
        store.dir().join(RECORD_FILE),
        r#"{"schema":1,"clis":{"codex":{"used_at":100,"used_how":"login"}}}"#,
    )
    .expect("legacy record");
    let legacy = store.load().expect("legacy read").of("codex");
    assert_eq!(legacy.generation, None);
    assert!(
        store
            .note_if_current("codex", &legacy, How::Review, Outcome::Worked, 100)
            .expect("matching legacy observation")
    );
    let captured = store.load().expect("read").of("codex");
    assert!(captured.generation.is_some());
    store
        .note("codex", How::Login, Outcome::Worked, 100)
        .expect("same-second replacement");
    let replacement = store.load().expect("read").of("codex");
    assert_ne!(captured.generation, replacement.generation);
    assert!(
        !store
            .note_if_current(
                "codex",
                &captured,
                How::Review,
                Outcome::Ended(Ended::ReviewAuth),
                300,
            )
            .expect("stale observation")
    );
    assert_eq!(store.load().expect("read").of("codex"), replacement);
}

#[test]
fn a_completion_leaves_unreadable_records_untouched() {
    let (_dir, store) = store();
    let expected = Record::default();
    assert!(
        store
            .note_if_current("codex", &expected, How::Review, Outcome::Worked, 100)
            .expect("matching absent observation")
    );
    let expected = store.load().expect("read").of("codex");
    let path = store.dir().join(RECORD_FILE);
    for text in [
        "not JSON",
        r#"{"schema":2,"clis":{}}"#,
        r#"{"schema":1,"clis":{"codex":{"generation":"invalid"}}}"#,
    ] {
        std::fs::write(&path, text).expect("unreadable record");
        assert!(
            !store
                .note_if_current("codex", &expected, How::Review, Outcome::Worked, 200)
                .expect("completion")
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read raw"), text);
    }
}
