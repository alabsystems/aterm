// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const REVISION: u64 = 7;
const BUILD: &str = "1.0.0+gabc";

fn reading(marks: Option<&str>, now: u64) -> Pace {
    Pace {
        build: BUILD.into(),
        marks: marks.map(str::to_owned),
        inputs: Some(marks.map_or_else(Inputs::default, |marks| Inputs {
            routes: Some(marks.into()),
            vault: Some("vault-state".into()),
            execution: Some("execution-state".into()),
            subscriber: Some(false),
        })),
        materialization: None,
        now,
    }
}

fn failed(root: &Path, revision: u64, pace: &Pace) {
    let mut rows = [Row {
        name: "Secrets vault".into(),
        status: "error".into(),
        detail: "the subscriber could not start".into(),
        elapsed_ms: None,
    }];
    record(root, revision, pace, false, &mut rows).unwrap();
}

fn materialization() -> MaterializationMark {
    MaterializationMark::from_cache_key(&"01".repeat(32)).unwrap()
}

#[test]
fn unknown_first_inputs_still_cap_and_the_first_known_fact_reopens() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    failed(root, REVISION, &reading(None, 1_000));
    assert!(paused(root, REVISION, &reading(None, 1_060)).is_none());
    failed(root, REVISION, &reading(None, 1_060));
    assert!(paused(root, REVISION, &reading(None, 9_000)).is_some());
    assert_eq!(Attempt::load(root).unwrap().failures, TRIES);

    let known = reading(Some("known-network-and-vault"), 9_000);
    assert!(paused(root, REVISION, &known).is_none());
    failed(root, REVISION, &known);
    assert_eq!(Attempt::load(root).unwrap().failures, 1);
    failed(
        root,
        REVISION,
        &reading(Some("known-network-and-vault"), 9_060),
    );
    assert!(paused(root, REVISION, &reading(None, 20_000)).is_some());
    assert!(
        paused(
            root,
            REVISION,
            &reading(Some("known-network-and-vault"), 20_000)
        )
        .is_some()
    );
}

#[test]
fn an_unknown_record_preserves_the_same_runs_known_hold() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let mut known = reading(Some("known-network-and-vault"), 1_000);
    known.materialization = Some(materialization());
    failed(root, REVISION, &known);
    known.now += FIRST_PAUSE.as_secs();
    failed(root, REVISION, &known);
    let unknown = reading(None, 9_000);
    assert!(paused(root, REVISION, &unknown).is_some());

    failed(root, REVISION, &unknown);
    let cached = Attempt::load(root).unwrap();
    assert_eq!(cached.marks, known.marks);
    assert_eq!(cached.materialization, known.materialization);
    assert_eq!(cached.failures, TRIES + 1);
    known.now = 20_000;
    assert!(paused(root, REVISION, &known).is_some());

    failed(root, REVISION + 1, &unknown);
    let next = Attempt::load(root).unwrap();
    assert_eq!(next.marks, None);
    assert_eq!(next.materialization, None);
    assert_eq!(next.failures, 1);
}

#[test]
fn an_unknown_post_read_keeps_the_first_known_failure_in_the_budget() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let mut before = reading(Some("known-network-and-vault"), 1_000);
    before.materialization = Some(materialization());
    let mut after = reading(None, 1_001);
    after.retain_known_from(&before);
    failed(root, REVISION, &after);

    before.now = after.now + FIRST_PAUSE.as_secs();
    assert!(paused(root, REVISION, &before).is_none());
    failed(root, REVISION, &before);
    before.now = 9_000;
    assert!(paused(root, REVISION, &before).is_some());
    assert_eq!(Attempt::load(root).unwrap().failures, TRIES);
}

#[test]
fn a_partial_network_change_reopens_without_unknown_materialization_rearming_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let mut known = reading(Some("network-a"), 1_000);
    known.materialization = Some(materialization());
    failed(root, REVISION, &known);
    known.now += FIRST_PAUSE.as_secs();
    failed(root, REVISION, &known);

    let mut changed = reading(None, 9_000);
    changed.inputs.as_mut().unwrap().routes = Some("network-b".into());
    assert!(paused(root, REVISION, &changed).is_none());
    failed(root, REVISION, &changed);
    let cached = Attempt::load(root).unwrap();
    assert_eq!(cached.failures, 1);
    assert_eq!(cached.marks, None);
    assert_eq!(cached.materialization, known.materialization);
    let cached_inputs = cached.inputs.unwrap();
    assert_eq!(cached_inputs.routes.as_deref(), Some("network-b"));
    assert_eq!(cached_inputs.vault, known.inputs.as_ref().unwrap().vault);
    assert_eq!(cached_inputs.subscriber, Some(false));

    changed.now += FIRST_PAUSE.as_secs();
    assert!(paused(root, REVISION, &changed).is_none());
    failed(root, REVISION, &changed);
    changed.now = 20_000;
    changed.materialization = known.materialization;
    assert!(paused(root, REVISION, &changed).is_some());
    changed.materialization = MaterializationMark::from_cache_key(&"02".repeat(32));
    assert!(paused(root, REVISION, &changed).is_none());
}

#[test]
fn a_known_post_change_retires_the_opaque_mark_but_keeps_independent_materialization() {
    for earlier in [None, Some("older-network-and-vault")] {
        let mut before = reading(earlier, 1_000);
        before.materialization = Some(materialization());
        let mut after = reading(None, 1_001);
        after.inputs.as_mut().unwrap().routes = Some("network-b".into());
        after.retain_known_from(&before);
        assert_eq!(after.marks, None);
        assert_eq!(after.materialization, before.materialization);
        assert_eq!(
            after.inputs.as_ref().unwrap().routes.as_deref(),
            Some("network-b")
        );
        assert_eq!(
            after.inputs.as_ref().unwrap().vault,
            before.inputs.as_ref().unwrap().vault
        );
    }
}

#[test]
fn legacy_string_marks_and_explicit_unknown_are_distinct_from_invalid_records() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let marks = "ab".repeat(32);
    let legacy = json!({
        "profile": PROFILE,
        "revision": REVISION,
        "builds": [BUILD],
        "marks": marks,
        "tries": TRIES,
        "failures": TRIES,
        "at": 1_000,
        "rows": [{"name": "Secrets vault", "status": "error", "detail": "start failed"}],
    });
    write_json(&root.join(RECORD), &legacy).unwrap();
    let old = Attempt::load(root).unwrap();
    assert_eq!(old.inputs, None);
    let mut changed = reading(None, 9_000);
    changed.inputs.as_mut().unwrap().routes = Some("network-b".into());
    assert!(paused(root, REVISION, &changed).is_some());
    assert_eq!(Attempt::load(root).unwrap(), old);
    assert!(paused(root, REVISION, &reading(Some(&marks), 9_000)).is_some());
    assert!(paused(root, REVISION, &reading(None, 9_000)).is_some());
    let seeded = Attempt::load(root).unwrap();
    assert_eq!(seeded.inputs, reading(Some(&marks), 9_000).inputs);
    assert_eq!(seeded.tries, old.tries);
    assert_eq!(seeded.failures, old.failures);
    assert_eq!(seeded.at, old.at);
    assert_eq!(seeded.rows, old.rows);
    assert_eq!(seeded.builds, old.builds);
    assert_eq!(seeded.materialization, old.materialization);
    assert!(paused(root, REVISION, &changed).is_none());

    let mut unknown = legacy.clone();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("marks".into(), Value::Null);
    write_json(&root.join(RECORD), &unknown).unwrap();
    assert!(Attempt::load(root).is_some());
    assert!(paused(root, REVISION, &reading(None, 9_000)).is_some());
    assert!(paused(root, REVISION, &reading(Some(&marks), 9_000)).is_none());

    let mut missing = legacy.clone();
    missing.as_object_mut().unwrap().remove("marks");
    let mut malformed = legacy.clone();
    malformed
        .as_object_mut()
        .unwrap()
        .insert("marks".into(), Value::Bool(false));
    let mut malformed_inputs = legacy.clone();
    malformed_inputs
        .as_object_mut()
        .unwrap()
        .insert("inputs".into(), Value::Null);
    let mut incomplete_inputs = legacy.clone();
    incomplete_inputs
        .as_object_mut()
        .unwrap()
        .insert("inputs".into(), json!({"routes": null}));
    let mut mistyped_inputs = legacy;
    mistyped_inputs.as_object_mut().unwrap().insert(
        "inputs".into(),
        json!({"routes": null, "vault": null, "execution": null, "subscriber": "unknown"}),
    );
    for invalid in [
        missing,
        malformed,
        malformed_inputs,
        incomplete_inputs,
        mistyped_inputs,
    ] {
        write_json(&root.join(RECORD), &invalid).unwrap();
        assert!(Attempt::load(root).is_none());
    }
}
