// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn epoch() -> Epoch {
    Epoch {
        sequence: 1,
        backing: 2,
        roots: 1,
        reserved_bytes: 42,
        opened: true,
        retired: false,
        helper_exited: false,
        control_eof: false,
        endpoint_cleaned: false,
        settled_units: 0,
        uncertain_units: 0,
        remaining_units: 2,
        completed_roots: 1,
        failed_roots: 1,
        faulted: false,
        unbound: false,
        not_spawned: false,
        cancellation_started: false,
        failure: None,
        cache_work: Vec::new(),
    }
}

fn sample() -> Snapshot {
    Snapshot {
        generation: Generation([7; 16]),
        revision: 5,
        closed: false,
        jobs: vec![Job {
            attempt: 1,
            phase: Phase::Faulted,
            reserved_bytes: 42,
            failure: Some(Failure::of(&io::Error::other(
                "private environment SECRET payload",
            ))),
        }],
        epochs: vec![epoch()],
    }
}

fn measured(run: &str) -> Summary {
    Summary {
        run_id: run.into(),
        availability: Availability::Complete,
        denominators: Denominators {
            registered_units: 4,
            run_script_units: 1,
            observed_units: 1,
            units_without_attempts: 3,
            attempts_started: 2,
            attempts_finished: 2,
            captured_attempts: 1,
            uncacheable_attempts: 1,
        },
        totals: Some(Work {
            entries: 2,
            input_bytes: 9,
            bytes_hashed: 6,
            metadata_reads: 12,
            resolution_steps: 10,
            link_reads: 0,
            observations: 15,
            revalidations: 7,
        }),
    }
}

fn settled_epoch(sequence: u64, roots: u32) -> Epoch {
    Epoch {
        sequence,
        roots,
        backing: roots,
        settled_units: roots,
        remaining_units: 0,
        completed_roots: roots,
        retired: true,
        helper_exited: true,
        control_eof: true,
        endpoint_cleaned: true,
        ..epoch()
    }
}

#[test]
fn cache_work_roundtrips_exact_run_and_distinguishes_legacy_and_collection_failure() {
    let run = "20261105T103732783Z-1ab3ba0487a929a0";
    let mut legacy = measured("20261105T103732784Z-1ab3ba0487a929a0");
    legacy.availability = Availability::UnavailableLegacyLog;
    legacy.denominators.observed_units = 0;
    legacy.denominators.units_without_attempts = 4;
    legacy.denominators.attempts_started = 0;
    legacy.denominators.attempts_finished = 0;
    legacy.denominators.captured_attempts = 0;
    legacy.denominators.uncacheable_attempts = 0;
    legacy.totals = None;
    let mut snapshot = sample();
    let mut epoch = settled_epoch(1, 3);
    epoch.cache_work = vec![
        RootCacheWork {
            root: 0,
            summary: Some(measured(run)),
            unavailable: None,
        },
        RootCacheWork {
            root: 1,
            summary: Some(legacy),
            unavailable: None,
        },
        RootCacheWork {
            root: 2,
            summary: None,
            unavailable: Some(UnavailableReason::QueryFailed),
        },
    ];
    snapshot.epochs = vec![epoch];
    let bytes = snapshot.encode().unwrap();
    assert_eq!(Snapshot::decode(&bytes).unwrap(), snapshot);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains(run));
    assert!(text.contains(crate::cache_work::SCOPE));
    assert!(text.contains("\"totals\":null"));
    assert!(
        !text.contains("\"units\":"),
        "full reports do not enter the monitor"
    );
    assert!(!text.contains("SECRET"));
}

#[test]
fn cache_work_refuses_unknown_scope_duplicate_bindings_and_invalid_nullable_shapes() {
    let mut snapshot = sample();
    let mut epoch = settled_epoch(1, 2);
    epoch.cache_work.push(RootCacheWork {
        root: 0,
        summary: Some(measured("20261105T103732783Z-1ab3ba0487a929a0")),
        unavailable: None,
    });
    snapshot.epochs = vec![epoch];
    let mut malformed = snapshot.clone();
    malformed.epochs[0].cache_work[0].unavailable = Some(UnavailableReason::QueryFailed);
    assert!(malformed.encode().is_err());
    let mut malformed = snapshot.clone();
    let duplicate = malformed.epochs[0].cache_work[0].clone();
    malformed.epochs[0].cache_work.push(duplicate);
    assert!(malformed.encode().is_err());
    let mut malformed = snapshot.clone();
    let mut record = malformed.epochs[0].cache_work[0].clone();
    record.root = 1;
    malformed.epochs[0].cache_work.push(record);
    assert!(
        malformed.encode().is_err(),
        "one run cannot represent two roots"
    );
    let mut value: Value = aterm_json::from_slice(&snapshot.encode().unwrap()).unwrap();
    let epoch = &mut rows_mut(&mut value, "epochs")[0];
    let record = &mut rows_mut(epoch, "cache_work")[0];
    record
        .as_object_mut()
        .unwrap()
        .get_mut("summary")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("scope".into(), json!("all-compiler-costs"));
    refuses(&value);
}

#[test]
fn cache_work_uses_one_global_budget_and_requires_original_settled_observations() {
    let mut snapshot = sample();
    snapshot.epochs[0].cache_work.push(RootCacheWork {
        root: 0,
        summary: None,
        unavailable: Some(UnavailableReason::MissingSession),
    });
    assert!(
        snapshot.encode().is_err(),
        "running custody cannot carry a completed-root report"
    );
    snapshot.jobs.clear();
    snapshot.epochs = (1..=9)
        .map(|sequence| {
            let mut epoch = settled_epoch(sequence, 2);
            epoch.cache_work = (0..2)
                .map(|root| RootCacheWork {
                    root,
                    summary: None,
                    unavailable: Some(UnavailableReason::NotRecorded),
                })
                .collect();
            epoch
        })
        .collect();
    assert!(
        snapshot.encode().is_err(),
        "separate epochs cannot exceed the aggregate summary budget"
    );
}
#[test]
fn bounded_snapshot_roundtrips_without_diagnostic_payloads_or_claiming_machine_scope() {
    let snapshot = sample();
    let bytes = snapshot.encode().unwrap();
    assert_eq!(Snapshot::decode(&bytes).unwrap(), snapshot);
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("SECRET"));
    assert!(text.contains("explicit-native-session"));
}
#[test]
fn unknown_scope_version_fields_phases_and_duplicate_attempts_refuse() {
    let baseline: Value = aterm_json::from_slice(&sample().encode().unwrap()).unwrap();
    for (path, value) in [
        ("schema", json!(1)),
        ("schema", json!(SCHEMA + 1)),
        ("scope", json!("computer")),
        ("extra", json!(true)),
        ("generation", json!([0])),
    ] {
        let mut bad = baseline.clone();
        bad.as_object_mut().unwrap().insert(path.to_owned(), value);
        assert!(Snapshot::decode(&aterm_json::to_vec(&bad).unwrap()).is_err());
    }
    let mut bad = sample();
    bad.jobs.push(bad.jobs[0].clone());
    assert!(bad.encode().is_err());
    let mut unknown_phase = baseline.clone();
    rows_mut(&mut unknown_phase, "jobs")[0]
        .as_object_mut()
        .unwrap()
        .insert("phase".into(), json!("running"));
    refuses(&unknown_phase);
    let bytes = aterm_json::to_string(&baseline)
        .unwrap()
        .replace("\"os_error\":null", "\"extra\":null");
    assert!(Snapshot::decode(bytes.as_bytes()).is_err());
    let mut inconsistent = sample();
    inconsistent.jobs[0].phase = Phase::Waiting;
    assert!(inconsistent.encode().is_err());
    assert!(Snapshot::decode(&vec![0; MAX_FRAME_BYTES + 1]).is_err());
}

#[test]
fn failed_build_retirement_and_uncertain_settlement_remain_separate() {
    let mut snapshot = sample();
    let row = &mut snapshot.epochs[0];
    row.retired = true;
    row.control_eof = true;
    row.helper_exited = true;
    let observed = Snapshot::decode(&snapshot.encode().unwrap()).unwrap();
    assert_eq!(
        observed.epochs[0].remaining_units, 2,
        "retirement is not settlement"
    );
    assert_eq!(observed.epochs[0].failed_roots, 1);

    let row = &mut snapshot.epochs[0];
    row.endpoint_cleaned = true;
    row.settled_units = 1;
    row.uncertain_units = 1;
    row.remaining_units = 0;
    row.failure = Some(Failure::of(&io::Error::other(
        "private settlement detail SECRET",
    )));
    let observed = Snapshot::decode(&snapshot.encode().unwrap()).unwrap();
    assert_eq!(observed.epochs[0].settled_units, 1);
    assert_eq!(observed.epochs[0].uncertain_units, 1);
    assert_eq!(
        observed.epochs[0].failed_roots, 1,
        "cleanup cannot turn a failed root green"
    );
    assert!(
        !observed.epochs[0].faulted,
        "settlement error is not a protocol fault"
    );
    assert!(
        !String::from_utf8(snapshot.encode().unwrap())
            .unwrap()
            .contains("SECRET")
    );
}

#[test]
fn genuine_unpublished_cancellation_needs_no_fabricated_retirement() {
    let mut snapshot = sample();
    let row = &mut snapshot.epochs[0];
    row.opened = false;
    row.completed_roots = 0;
    row.failed_roots = 0;
    row.not_spawned = true;
    row.failure = Some(Failure::of(&io::Error::from(io::ErrorKind::NotFound)));
    assert_eq!(
        Snapshot::decode(&snapshot.encode().unwrap()).unwrap(),
        snapshot
    );
    let row = &mut snapshot.epochs[0];
    row.not_spawned = false;
    row.cancellation_started = true;
    row.endpoint_cleaned = true;
    row.settled_units = 2;
    row.remaining_units = 0;
    let observed = Snapshot::decode(&snapshot.encode().unwrap()).unwrap();
    assert!(observed.epochs[0].cancellation_started);
    assert!(!observed.epochs[0].retired);
    assert!(!observed.epochs[0].helper_exited);
    assert_eq!(observed.epochs[0].settled_units, 2);
}

fn value() -> Value {
    aterm_json::from_slice(&sample().encode().unwrap()).unwrap()
}
fn refuses(value: &Value) {
    assert!(Snapshot::decode(&aterm_json::to_vec(value).unwrap()).is_err());
}
fn rows_mut<'a>(value: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    value
        .as_object_mut()
        .unwrap()
        .get_mut(key)
        .unwrap()
        .as_array_mut()
        .unwrap()
}
fn set_epoch(value: &mut Value, key: &str, field: Value) {
    rows_mut(value, "epochs")[0]
        .as_object_mut()
        .unwrap()
        .insert(key.into(), field);
}

#[test]
fn decoder_refuses_release_without_actual_storage_closure() {
    let mut value = value();
    set_epoch(&mut value, "settled_units", json!(1));
    set_epoch(&mut value, "remaining_units", json!(1));
    refuses(&value);
    set_epoch(&mut value, "endpoint_cleaned", json!(true));
    refuses(&value);
    set_epoch(&mut value, "retired", json!(true));
    set_epoch(&mut value, "control_eof", json!(true));
    set_epoch(&mut value, "helper_exited", json!(true));
    assert!(Snapshot::decode(&aterm_json::to_vec(&value).unwrap()).is_ok());
}

#[test]
fn decoder_refuses_false_eof_and_conflicting_launch_custody() {
    let mut value = value();
    set_epoch(&mut value, "control_eof", json!(true));
    refuses(&value);
    set_epoch(&mut value, "control_eof", json!(false));
    set_epoch(&mut value, "cancellation_started", json!(true));
    refuses(&value);
    set_epoch(&mut value, "cancellation_started", json!(false));
    set_epoch(&mut value, "opened", json!(false));
    set_epoch(&mut value, "completed_roots", json!(0));
    set_epoch(&mut value, "failed_roots", json!(0));
    set_epoch(&mut value, "not_spawned", json!(true));
    set_epoch(&mut value, "unbound", json!(true));
    refuses(&value);
}

#[test]
fn epoch_counts_and_private_field_injection_refuse() {
    let mut value = value();
    set_epoch(&mut value, "failed_roots", json!(2));
    refuses(&value);
    set_epoch(&mut value, "failed_roots", json!(1));
    set_epoch(&mut value, "remaining_units", json!(u32::MAX));
    set_epoch(&mut value, "settled_units", json!(1));
    refuses(&value);
    let mut value = self::value();
    set_epoch(&mut value, "backing", json!(u64::MAX));
    refuses(&value);
    let mut value = self::value();
    set_epoch(&mut value, "fifo", json!("/private/SECRET"));
    refuses(&value);
    let mut value = self::value();
    let duplicate = value["epochs"][0].clone();
    rows_mut(&mut value, "epochs").push(duplicate);
    refuses(&value);
    let duplicate = String::from_utf8(sample().encode().unwrap())
        .unwrap()
        .replace("\"sequence\":1", "\"sequence\":1,\"sequence\":1");
    assert!(Snapshot::decode(duplicate.as_bytes()).is_err());
}

#[test]
fn jobs_and_epochs_share_one_row_limit_but_have_separate_identity_spaces() {
    let mut snapshot = sample();
    snapshot.jobs = (1..MAX_JOBS as u64)
        .map(|attempt| Job {
            attempt,
            phase: Phase::Waiting,
            reserved_bytes: 0,
            failure: None,
        })
        .collect();
    let bytes = snapshot.encode().unwrap();
    assert_eq!(
        Snapshot::decode(&bytes).unwrap(),
        snapshot,
        "native attempt one and epoch one are distinct"
    );
    snapshot.jobs.push(Job {
        attempt: MAX_JOBS as u64,
        phase: Phase::Waiting,
        reserved_bytes: 0,
        failure: None,
    });
    assert!(snapshot.encode().is_err());
    let mut value: Value = aterm_json::from_slice(&bytes).unwrap();
    rows_mut(&mut value, "jobs")
        .push(json!({"attempt":256,"phase":"waiting","reserved_bytes":0,"failure":null}));
    refuses(&value);
}
