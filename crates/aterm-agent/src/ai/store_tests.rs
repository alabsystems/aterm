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
fn another_schema_and_a_torn_record_are_never_written_over() {
    let (_dir, store) = store();
    store.ensure().expect("dir");
    let path = store.dir().join(RECORD_FILE);
    std::fs::write(&path, "{\"schema\":3,\"clis\":{}}").expect("write");
    assert_eq!(store.load(), Err(Unread::NotOurs(3)));
    store
        .note("claude", How::Review, Outcome::Worked, 1)
        .expect("note");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "{\"schema\":3,\"clis\":{}}"
    );

    std::fs::write(&path, "not json").expect("write");
    assert!(
        store
            .note("claude", How::Review, Outcome::Worked, 2)
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
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
fn scope_migration_preserves_legacy_and_unknown_scope_history() {
    let (_dir, store) = store();
    store.ensure().unwrap();
    let path = store.dir().join(RECORD_FILE);
    std::fs::write(&path, r#"{"schema":1,"clis":{"codex":{"ended_at":10,"ended_why":"review-auth"}},"scopes":{"future-provider":{"future-backend":{"used_at":20}}}}"#).unwrap();
    let before = store.load().unwrap();
    assert_eq!(before.of("codex").ended_at, Some(10));
    for scope in ["file", "keyring", "auto", "ephemeral"] {
        assert_eq!(before.of_scope("codex", Some(scope)), Record::default());
    }
    store
        .note_scope("codex", Some("keyring"), How::Login, Outcome::Worked, 30)
        .unwrap();
    let after = store.load().unwrap();
    assert_eq!(after.of("codex"), before.of("codex"));
    assert_eq!(
        after.of_scope("future-provider", Some("future-backend")),
        before.of_scope("future-provider", Some("future-backend"))
    );
    assert_eq!(after.of_scope("codex", Some("keyring")).used_at, Some(30));
    let written: Value = aterm_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(written.get("schema").and_then(Value::as_i64), Some(SCHEMA));
}

#[test]
fn malformed_scopes_are_preserved_without_a_wake() {
    let (_dir, store) = store();
    store.ensure().unwrap();
    let path = store.dir().join(RECORD_FILE);
    for text in [
        r#"{"schema":2,"clis":[]}"#,
        r#"{"schema":2,"clis":{"codex":false}}"#,
        r#"{"schema":2,"clis":{},"scopes":[]}"#,
        r#"{"schema":2,"clis":{},"scopes":{"codex":false}}"#,
        r#"{"schema":2,"clis":{},"scopes":{"codex":{"file":[]}}}"#,
        r#"{"schema":2,"clis":{},"scopes":{"codex":{"file":{"generation":"bad"}}}}"#,
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(matches!(store.load(), Err(Unread::Torn(_))));
        assert!(
            store
                .note_scope("codex", Some("file"), How::Login, Outcome::Worked, 100)
                .is_err()
        );
        assert!(
            !store
                .note_if_current_scope(
                    "codex",
                    Some("file"),
                    &Record::default(),
                    How::Review,
                    Outcome::Worked,
                    100
                )
                .unwrap()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(!store.wake_path().exists());
    }
}

#[test]
fn a_scoped_write_cannot_make_the_store_unreadable_by_its_size() {
    let (_dir, store) = store();
    store
        .note("claude", How::Login, Outcome::Worked, 10)
        .unwrap();
    let path = store.dir().join(RECORD_FILE);
    let before = std::fs::read(&path).unwrap();
    let too_large = "x".repeat(usize::try_from(RECORD_MAX_BYTES).unwrap());
    assert!(
        store
            .note_scope("codex", Some(&too_large), How::Login, Outcome::Worked, 20)
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(store.load().unwrap().of("claude").used_at, Some(10));
}

fn health_key(key: i64) -> (&'static str, Option<&'static str>) {
    let provider = if key < 3 { "codex" } else { "claude" };
    (
        provider,
        match key % 3 {
            0 => None,
            1 => Some("file"),
            _ => Some("keyring"),
        },
    )
}

fn scoped_health_case(observed: i64, changed: i64, outcome: i64) -> [bool; 3] {
    let (_dir, store) = store();
    // Legacy data exists, but it cannot become an observation of an unknown backend.
    store
        .note("codex", How::Login, Outcome::Worked, 10)
        .unwrap();
    let isolated = store.load().unwrap().of_scope("codex", Some("file")) == Record::default();
    let (provider, scope) = health_key(observed);
    let expected = store.load().unwrap().of_scope(provider, scope);
    let (changed_provider, changed_scope) = health_key(changed);
    store
        .note_scope(
            changed_provider,
            changed_scope,
            How::Login,
            Outcome::Worked,
            20,
        )
        .unwrap();
    let before = store.load().unwrap();
    let outcome = match outcome {
        0 => Outcome::Worked,
        1 => Outcome::Ended(Ended::ReviewAuth),
        _ => Outcome::Limited(Some(500)),
    };
    let accepted = store
        .note_if_current_scope(provider, scope, &expected, How::Review, outcome, 30)
        .unwrap();
    let after = store.load().unwrap();
    assert_eq!(Records::parse(&after.render()).unwrap(), after);
    let preserved = (0..6)
        .filter(|key| !accepted || *key != observed)
        .all(|key| {
            let (provider, scope) = health_key(key);
            before.of_scope(provider, scope) == after.of_scope(provider, scope)
        });
    [accepted, preserved, isolated]
}

pub(crate) fn scoped_health_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Observe" => {
            next.insert("observed", state["observed"] + 1);
        }
        "Change" => {
            next.insert("changed", state["changed"] + 1);
        }
        "Outcome" => {
            next.insert("outcome", state["outcome"] + 1);
        }
        "Commit" => {
            next.insert("phase", 1);
            for (field, value) in
                ["accepted", "preserved", "isolated"]
                    .into_iter()
                    .zip(scoped_health_case(
                        state["observed"],
                        state["changed"],
                        state["outcome"],
                    ))
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn scoped_health_conforms_to_exact_record_ownership() {
    let model = aterm_models_harness::scoped_health_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 216);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = scoped_health_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                if action.name == "Commit" {
                    for (field, law) in [
                        ("accepted", "ExactRecord"),
                        ("preserved", "OtherRecords"),
                        ("isolated", "LegacyIsSeparate"),
                    ] {
                        let mut bad = next.clone();
                        bad.insert(field, 1 - next[field]);
                        assert!(!model.check_invariant(law, &bad), "{law} negative control");
                    }
                }
            }
        }
    }
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
        r#"{"schema":3,"clis":{}}"#,
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

    #[cfg(unix)]
    {
        let lock_path = store.dir().join("ai.lock");
        let _ = std::fs::remove_file(&lock_path);
        let outside = store.dir().join("outside-lock");
        std::fs::write(&outside, b"").expect("outside");
        std::os::unix::fs::symlink(&outside, &lock_path).expect("symlink ai.lock");
        assert!(
            store.acquire().is_err(),
            "symlinked ai.lock must be refused"
        );
        std::fs::remove_file(&lock_path).expect("remove symlink");
        std::fs::create_dir(&lock_path).expect("dir ai.lock");
        assert!(
            store.acquire().is_err(),
            "directory ai.lock must be refused"
        );
        std::fs::remove_dir(&lock_path).expect("remove dir");
        drop(store.acquire().expect("regular ai.lock succeeds"));
    }
}
