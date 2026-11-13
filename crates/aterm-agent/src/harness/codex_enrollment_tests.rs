// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn native_cancel_dispatch_metadata_covers_prestage_but_grants_no_recovery() {
    let request = request();
    let mut native = Operations::default();
    native.admit_native(&request).unwrap();
    assert!(native.native_cancellation(&request).unwrap());
    assert!(!native.native_recovery_pending(&request).unwrap());
    let mut foreign = request.clone();
    foreign.caller = "f".repeat(64);
    assert!(native.native_cancellation(&foreign).is_err());
    let mut legacy = Operations::default();
    legacy.admit(&request).unwrap();
    assert!(!legacy.native_cancellation(&request).unwrap());
}

#[test]
fn new_or_replayed_metadata_cannot_mint_retired_work() {
    let empty = Operations::default();
    let mut forged = empty.clone();
    let request = request();
    forged.admit(&request).unwrap();
    forged.0.get_mut(&request.id).unwrap().retired = true;
    assert!(
        forged
            .commit(&empty, &Default::default(), &Default::default())
            .is_err()
    );
    let mut running = empty.clone();
    running.admit(&request).unwrap();
    running
        .commit(&empty, &Default::default(), &Default::default())
        .unwrap();
    let mut changed = running.clone();
    changed.0.get_mut(&request.id).unwrap().retired = true;
    assert!(
        changed
            .commit(&running, &Default::default(), &Default::default())
            .is_err()
    );
    let lifetime = super::super::account_work::Lifetime::default();
    let (work, control) = owned_work(request, lifetime.enter().unwrap());
    drop(work);
    assert!(
        control.retired().is_none(),
        "a fresh scope is not the original admitted work"
    );
}

#[test]
fn original_and_stale_retirement_cannot_retire_a_new_recovery_claim() {
    let request = request();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut book = Operations::default();
    let before = book.clone();
    book.admit(&request).unwrap();
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let before = book.clone();
    book.bind_staging(Staging {
        native_row: None,
        request: request.clone(),
        store: root.path().to_path_buf(),
        directory: root.path().join("staging"),
        store_identity: None,
        directory_identity: None,
        parent: accounts::RenewalParent::capture(&accounts::Logins::default(), &request.label),
        workflow: 1,
        policy: "d".repeat(64),
        selection: None,
    })
    .unwrap();
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let (original, control) = owned_work(request.clone(), lifetime.enter().unwrap());
    original
        .admitted
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(control.retired().is_none());
    drop(original);
    let original = control.retired().unwrap();
    let before = book.clone();
    book.retire(&original).unwrap();
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let before = book.clone();
    let generation = book.claim_recovery(&request).unwrap();
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let active = book.clone();
    assert!(book.retire(&original).is_err());
    assert_eq!(book, active);
    let scope = lifetime.enter().unwrap();
    let detached = scope.clone();
    let (recovery, control) = owned_work(request.clone(), scope);
    let admission = Admission {
        scope: &recovery.scope,
        request: &recovery.request,
        original: &recovery.admitted,
        recovery: &recovery.recovery,
    };
    admission.recovery(generation).unwrap();
    drop(recovery);
    assert!(
        control.retired().is_none(),
        "detached original recovery resources remain owned"
    );
    drop(detached);
    let proof = control.retired().unwrap();
    let before = book.clone();
    book.retire(&proof).unwrap();
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let mut restored = Operations::decode(&book.encode()).unwrap();
    assert!(restored.recoverable(&request).unwrap());
    let before = restored.clone();
    let second = restored.claim_recovery(&request).unwrap();
    assert_eq!(second, generation + 1);
    restored
        .commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let active = restored.clone();
    assert!(restored.retire(&proof).is_err());
    assert_eq!(restored, active);
    // A serialized edit cannot launder an old original or recovery proof
    // into retirement of the currently live claim.
    for stale in [&original, &proof] {
        let mut forged = Operations::decode(&restored.encode()).unwrap();
        forged
            .0
            .get_mut(&request.id)
            .unwrap()
            .recovery
            .as_mut()
            .unwrap()
            .retired = true;
        let _ = forged.retire(stale);
        assert!(
            forged
                .commit(&restored, &Default::default(), &Default::default())
                .is_err()
        );
    }
    let (second_work, second_control) = owned_work(request.clone(), lifetime.enter().unwrap());
    Admission {
        scope: &second_work.scope,
        request: &second_work.request,
        original: &second_work.admitted,
        recovery: &second_work.recovery,
    }
    .recovery(second)
    .unwrap();
    drop(second_work);
    let second_proof = second_control.retired().unwrap();
    let before = restored.clone();
    restored.retire(&second_proof).unwrap();
    restored
        .commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let before = restored.clone();
    let third = restored.claim_recovery(&request).unwrap();
    assert_eq!(third, second + 1);
    restored
        .commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let active = restored.clone();
    assert!(restored.retire(&proof).is_err());
    assert!(restored.retire(&second_proof).is_err());
    assert_eq!(restored, active);
    let mut exhausted = restored;
    exhausted.0.get_mut(&request.id).unwrap().recovery = Some(RecoveryClaim {
        generation: u64::MAX,
        retired: true,
        kind: None,
        occurrence: None,
    });
    let exhausted = Operations::decode(&exhausted.encode()).unwrap();
    let mut attempted = exhausted.clone();
    assert!(attempted.claim_recovery(&request).is_err());
    assert_eq!(attempted, exhausted);
}

fn request() -> Request {
    Request {
        id: "a".repeat(32),
        caller: "b".repeat(64),
        label: "work".into(),
        revision: "c".repeat(64),
    }
}

#[test]
fn native_admission_requires_authenticated_receipt_before_publication() {
    let request = request();
    let empty = Operations::default();
    let mut running = empty.clone();
    assert!(running.admit_native(&request).unwrap());
    running
        .commit(&empty, &Default::default(), &Default::default())
        .unwrap();
    assert!(running.requires_extended());
    assert!(running.checkpoint_reference(&request).is_err());
    assert!(running.clone().publishing(&request).is_err());
    let mut downgraded = running.clone();
    downgraded.0.get_mut(&request.id).unwrap().native_required = false;
    assert!(
        downgraded
            .commit(&running, &Default::default(), &Default::default())
            .is_err()
    );
    // Deserialization is correlation data, never a live retained capability.
    let reference = aterm_json::from_value(aterm_json::json!({
        "operation": request.id, "capability": "d".repeat(64),
        "issuance_digest": "e".repeat(64), "producer_mac": "f".repeat(64)
    }))
    .unwrap();
    let mut forged = running.clone();
    forged.0.get_mut(&request.id).unwrap().grant_reference = Some(reference);
    assert!(
        forged
            .commit(&running, &Default::default(), &Default::default())
            .is_err()
    );
    assert!(
        forged
            .commit(&empty, &Default::default(), &Default::default())
            .is_err()
    );
    assert!(forged.published_grant(&request.label, 9).is_none());
    forged.0.get_mut(&request.id).unwrap().status = Status {
        phase: Phase::Applied,
        generation: Some(9),
    };
    let binding = forged.published_grant(&request.label, 9).unwrap();
    assert_eq!(binding.request, request);
    assert_eq!(binding.generation, 9);
    assert!(forged.published_grant(&request.label, 0).is_none());
    assert!(forged.published_grant(&request.label, 8).is_none());
    assert!(forged.published_grant("other", 9).is_none());
    let mut duplicate = forged.0.get(&request.id).unwrap().clone();
    let duplicate_id = "e".repeat(32);
    duplicate.request.as_mut().unwrap().id = duplicate_id.clone();
    forged.0.insert(duplicate_id, duplicate);
    assert!(
        forged.published_grant(&request.label, 9).is_none(),
        "ambiguous published receipt is not selected"
    );
    let mut legacy = Operations::default();
    legacy.admit(&request).unwrap();
    assert!(!legacy.admit_native(&request).unwrap());
    assert!(
        !legacy.requires_extended(),
        "replay cannot upgrade ownership mode"
    );
}

#[test]
fn owner_witness_requires_canonical_schema_and_private_mutation_evidence() {
    let request = request();
    let mut before = Operations::default();
    before.admit_native(&request).unwrap();
    // Synthetic correlation data supplies no encrypted owner authority.
    let reference = aterm_json::from_value(aterm_json::json!({
        "operation":request.id, "capability":"d".repeat(64), "issuance_digest":"e".repeat(64), "producer_mac":"f".repeat(64)
    })).unwrap();
    let record = before.0.get_mut(&request.id).unwrap();
    // This control exercises the historical schema-nine owner boundary,
    // before new native admissions required a global catalogue witness.
    record.catalogue_required = false;
    record.grant_reference = Some(reference);
    record.status = Status {
        phase: Phase::Applied,
        generation: Some(9),
    };
    let binding = before.published_grant(&request.label, 9).unwrap();
    assert!(before.owner_witness(&binding).unwrap().is_none());
    let witness = OwnerWitness {
        revision: super::super::codex_session_slot::Revision {
            generation: 1,
            digest: "1".repeat(64),
        },
        journal_digest: "2".repeat(64),
        pending: None,
    };
    let mut forged = before.clone();
    forged.0.get_mut(&request.id).unwrap().owner_witness = Some(witness.clone());
    assert!(
        forged
            .commit(&before, &Default::default(), &Default::default())
            .is_err()
    );
    let mut logins = accounts::Logins {
        enrollment: forged,
        ..Default::default()
    };
    let text = logins.to_json();
    let mut doc: Value = aterm_json::from_str(&text).unwrap();
    assert_eq!(doc["schema"].as_u64(), Some(9));
    assert!(
        !text.contains("\"rejected\""),
        "old witness bytes stay exact"
    );
    let decoded = accounts::Logins::from_json(&text, 1_800_000_000).unwrap();
    assert_eq!(
        decoded.enrollment.owner_witness(&binding).unwrap(),
        Some(witness.clone())
    );
    doc.as_object_mut()
        .unwrap()
        .insert("schema".into(), Value::from(8));
    assert!(accounts::Logins::from_json(&doc.to_string(), 1_800_000_000).is_err());
    let mut wrong = binding.clone();
    wrong.generation = 8;
    assert!(decoded.enrollment.owner_witness(&wrong).is_err());
    logins
        .enrollment
        .0
        .get_mut(&request.id)
        .unwrap()
        .owner_witness
        .as_mut()
        .unwrap()
        .pending = Some(OwnerAttempt {
        operation: "3".repeat(32),
        request_digest: "4".repeat(64),
        base_generation: 2,
        base_digest: witness.revision.digest.clone(),
        token_digest: "5".repeat(64),
    });
    assert!(
        accounts::Logins::from_json(&logins.to_json(), 1_800_000_000).is_err(),
        "pending attempt must match canonical current revision"
    );
    assert!(!format!("{witness:?}").contains(&"2".repeat(64)));
}

#[test]
fn cancellation_commit_requires_exact_retirement_and_keeps_tombstone() {
    let request = request();
    let mut running = Operations::default();
    running.admit(&request).unwrap();
    let mut forged = running.clone();
    forged.0.get_mut(&request.id).unwrap().status.phase = Phase::Cancelled;
    assert!(
        forged
            .commit(&running, &Default::default(), &Default::default())
            .is_err()
    );
    let mut settled = running.clone();
    settled
        .retire(&Retired {
            request: request.clone(),
            kind: RetiredKind::Original,
        })
        .unwrap();
    settled
        .commit(&running, &Default::default(), &Default::default())
        .unwrap();
    assert!(settled.is_retired(&request).unwrap());
    assert_eq!(
        settled.lookup(&request).unwrap().unwrap().phase,
        Phase::Running
    );
    let mut pending = running.clone();
    pending.finish(&request, Phase::CancelRequested).unwrap();
    pending
        .commit(&running, &Default::default(), &Default::default())
        .unwrap();
    let mut forged = pending.clone();
    forged.0.get_mut(&request.id).unwrap().status.phase = Phase::Cancelled;
    assert!(
        forged
            .commit(&pending, &Default::default(), &Default::default())
            .is_err()
    );
    let mut retired = pending.clone();
    let mut wrong = request.clone();
    wrong.caller = "f".repeat(64);
    assert!(
        retired
            .retire(&Retired {
                request: wrong,
                kind: RetiredKind::Original
            })
            .is_err()
    );
    retired
        .retire(&Retired {
            request: request.clone(),
            kind: RetiredKind::Original,
        })
        .unwrap();
    retired
        .commit(&pending, &Default::default(), &Default::default())
        .unwrap();
    assert!(retired.1.is_empty());
    let mut restored = Operations::decode(&retired.encode()).unwrap();
    assert_eq!(
        restored.lookup(&request).unwrap().unwrap().phase,
        Phase::Cancelled
    );
    assert!(!restored.admit(&request).unwrap());
    let before = restored.clone();
    restored
        .retire(&Retired {
            request: request.clone(),
            kind: RetiredKind::Original,
        })
        .unwrap();
    assert!(
        restored.1.is_empty(),
        "repeat finalization supplies no new transition marker"
    );
    restored
        .commit(&before, &Default::default(), &Default::default())
        .unwrap();
    assert_eq!(restored, before);
    let mut wrong = request;
    wrong.caller = "f".repeat(64);
    assert!(
        restored
            .retire(&Retired {
                request: wrong,
                kind: RetiredKind::Original
            })
            .is_err()
    );
}

#[test]
fn unadmitted_or_still_owned_work_cannot_prove_retirement() {
    let lifetime = super::super::account_work::Lifetime::default();
    let (work, control) = owned_work(request(), lifetime.enter().unwrap());
    assert!(control.retired().is_none());
    drop(work);
    assert!(control.retired().is_none(), "a replay cannot mint a permit");
    let (work, control) = owned_work(request(), lifetime.enter().unwrap());
    work.admitted
        .store(true, std::sync::atomic::Ordering::Release);
    let detached = work.scope.clone();
    drop(work);
    assert!(control.retired().is_none());
    drop(detached);
    assert_eq!(control.retired().unwrap().request, request());
}

#[test]
fn durable_custody_refuses_replay_retarget_and_uncertain_restart() {
    let request = request();
    let mut book = Operations::default();
    assert!(book.admit(&request).unwrap());
    let mut restored = Operations::decode(&book.encode()).unwrap();
    assert!(!restored.admit(&request).unwrap());
    let mut changed = request.clone();
    changed.label = "other".into();
    assert!(restored.admit(&changed).is_err());
    changed = request.clone();
    changed.id = "d".repeat(32);
    assert!(restored.admit(&changed).is_err());
    assert!(restored.finish(&request, Phase::Cancelled).is_err());
    restored.finish(&request, Phase::CancelRequested).unwrap();
    assert!(restored.require_running(&request).is_err());
    assert!(
        restored.admit(&changed).is_err(),
        "unretired cancellation keeps custody"
    );
}

#[test]
fn publication_needs_an_admitted_renewal_and_transaction_derived_generation() {
    let request = request();
    let mut before = Operations::default();
    before.admit(&request).unwrap();
    let mut after = before.clone();
    after.publishing(&request).unwrap();
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "work".into(),
        Binding {
            last_success_generation: 9,
            ..Binding::default()
        },
    );
    assert!(
        after
            .clone()
            .commit(&before, &Default::default(), &bindings)
            .is_err()
    );
    let renewed = ["work".into()].into_iter().collect();
    let mut forged = after.clone();
    forged.0.get_mut(&request.id).unwrap().status.generation = Some(9);
    assert!(forged.commit(&before, &renewed, &bindings).is_err());
    assert!(
        after
            .clone()
            .commit(&Operations::default(), &renewed, &bindings)
            .is_err()
    );
    after.commit(&before, &renewed, &bindings).unwrap();
    assert_eq!(
        after.lookup(&request).unwrap().unwrap(),
        Status {
            phase: Phase::Applied,
            generation: Some(9)
        }
    );
    assert!(
        Operations::default()
            .commit(&after, &renewed, &bindings)
            .is_err()
    );
    let mut forged = after.clone();
    forged.0.get_mut(&request.id).unwrap().status.phase = Phase::Running;
    assert!(forged.commit(&after, &renewed, &bindings).is_err());
}

#[test]
fn schema_seven_retains_operations_and_legacy_metadata_stays_legacy() {
    let mut logins = accounts::Logins::default();
    assert_ne!(
        aterm_json::from_str::<Value>(&logins.to_json()).unwrap()["schema"].as_u64(),
        Some(7)
    );
    logins.enrollment.admit(&request()).unwrap();
    let value = aterm_json::from_str::<Value>(&logins.to_json()).unwrap();
    assert_eq!(value["schema"].as_u64(), Some(7));
    let restored = accounts::Logins::from_json(&value.to_string(), 1794000000).unwrap();
    assert_eq!(restored.enrollment, logins.enrollment);
    let mut legacy = value.clone();
    legacy
        .as_object_mut()
        .unwrap()
        .insert("schema".into(), Value::from(6));
    assert!(accounts::Logins::from_json(&legacy.to_string(), 1794000000).is_err());
    let mut missing = value;
    missing.as_object_mut().unwrap().remove("enrollment");
    assert!(accounts::Logins::from_json(&missing.to_string(), 1794000000).is_err());
}

#[test]
fn opaque_refusal_is_immutable_and_never_grants_a_provider_row() {
    let request = request();
    let intent = request.intent();
    let before = Operations::default();
    let mut book = before.clone();
    let refused = book.reject(&intent).unwrap();
    assert_eq!(
        refused,
        Status {
            phase: Phase::Refused,
            generation: None
        }
    );
    let mut without_proof = book.clone();
    without_proof.2.clear();
    assert!(
        without_proof
            .commit(&before, &Default::default(), &Default::default())
            .is_err()
    );
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    assert!(
        !book.admit(&request).unwrap(),
        "a source row appearing later cannot revive the ID"
    );
    assert!(book.require_running(&request).is_err());
    assert!(book.publishing(&request).is_err());
    assert!(book.admitted(&intent.id, &intent.caller).is_err());
    let staging = aterm_tempfile::tempdir().unwrap();
    let refused_logins = accounts::Logins {
        enrollment: book.clone(),
        ..Default::default()
    };
    assert!(
        accounts::write_enrollment_checkpoint(
            &request,
            "work@example.test",
            staging.path().to_str().unwrap(),
            accounts::Proof::Status,
            1794000000,
            &refused_logins
        )
        .is_err()
    );
    assert_eq!(
        staging.path().read_dir().unwrap().count(),
        0,
        "no provider checkpoint was created"
    );
    assert_eq!(book.reject(&intent).unwrap(), refused);
    let mut changed = intent.clone();
    changed.account_id = "f".repeat(64);
    assert!(book.reject(&changed).is_err());
    changed = intent.clone();
    changed.caller = "f".repeat(64);
    assert!(book.reject(&changed).is_err());
    changed = intent.clone();
    changed.revision = "f".repeat(64);
    assert!(book.reject(&changed).is_err());
    for phase in [Phase::Running, Phase::Applied] {
        let mut forged = book.clone();
        forged.0.get_mut(&intent.id).unwrap().status.phase = phase;
        assert!(Operations::decode(&forged.encode()).is_err());
        assert!(
            forged
                .commit(&book, &Default::default(), &Default::default())
                .is_err()
        );
    }
    let mut admitted = Operations::default();
    admitted.admit(&request).unwrap();
    let original = admitted.clone();
    assert_eq!(admitted.reject(&intent).unwrap().phase, Phase::Running);
    assert_eq!(admitted, original, "refusal never downgrades admitted work");
}

#[test]
fn schema_eight_preserves_existing_admissions_and_binding_metadata() {
    let request = request();
    let mut logins = accounts::Logins::default();
    logins.labels.insert(
        "work".into(),
        Binding {
            dir: Some(crate::harness::host_path::abs(
                "/fixture/existing-protected-home",
            )),
            renewal_generation: 7,
            last_success_generation: 7,
            ..Default::default()
        },
    );
    logins.generation_clock = 7;
    logins.enrollment.admit(&request).unwrap();
    let old = logins.to_json();
    assert_eq!(
        aterm_json::from_str::<Value>(&old).unwrap()["schema"].as_u64(),
        Some(7)
    );
    let mut restored = accounts::Logins::from_json(&old, 1794000000).unwrap();
    let before = restored.enrollment.clone();
    let mut rejected = request.intent();
    rejected.id = "d".repeat(32);
    rejected.account_id = "e".repeat(64);
    restored.enrollment.reject(&rejected).unwrap();
    restored
        .enrollment
        .commit(&before, &Default::default(), &Default::default())
        .unwrap();
    let encoded = restored.to_json();
    let mut value = aterm_json::from_str::<Value>(&encoded).unwrap();
    assert_eq!(value["schema"].as_u64(), Some(8));
    let roundtrip = accounts::Logins::from_json(&encoded, 1794000000).unwrap();
    assert_eq!(roundtrip.get("work"), logins.get("work"));
    assert_eq!(
        roundtrip
            .enrollment
            .lookup(&request)
            .unwrap()
            .unwrap()
            .phase,
        Phase::Running
    );
    assert_eq!(
        roundtrip
            .enrollment
            .retained(&rejected.id, &rejected.caller)
            .unwrap()
            .1
            .phase,
        Phase::Refused
    );
    value
        .as_object_mut()
        .unwrap()
        .insert("schema".into(), Value::from(7));
    assert!(
        accounts::Logins::from_json(&value.to_string(), 1794000000).is_err(),
        "new record kind cannot masquerade as old schema"
    );
}

#[test]
fn bounded_custody_never_evicts_uncertain_or_completed_tombstones() {
    let mut book = Operations::default();
    for n in 0..MAX_OPERATIONS {
        let mut request = request();
        request.id = format!("{n:032x}");
        request.label = format!("work-{n}");
        book.admit(&request).unwrap();
    }
    let mut next = request();
    next.id = "f".repeat(32);
    assert!(book.admit(&next).is_err());
    assert!(
        book.reject(&next.intent()).is_err(),
        "capacity cannot invent a refusal proof"
    );
    assert_eq!(Operations::decode(&book.encode()).unwrap(), book);
}
#[test]
fn cancelled_operation_cannot_recover_its_verified_completion_checkpoint() {
    let staging = aterm_tempfile::tempdir().unwrap();
    let request = request();
    let row = accounts::Row {
        label: "work".into(),
        email: Some("work@example.invalid".into()),
        ..Default::default()
    };
    let mut logins = accounts::Logins::default();
    logins.enrollment.admit(&request).unwrap();
    accounts::write_enrollment_checkpoint(
        &request,
        "work@example.invalid",
        staging.path().to_str().unwrap(),
        accounts::Proof::Status,
        1794000000,
        &logins,
    )
    .unwrap();
    let actual = staging.path().canonicalize().unwrap();
    let checkpoint = accounts::read_renewal_checkpoint_for(
        accounts::Cli::Codex,
        staging.path(),
        &actual,
        1794000001,
    )
    .unwrap();
    assert_eq!(checkpoint.enrollment.as_ref(), Some(&request));
    assert!(accounts::checkpoint_current(
        &row,
        &accounts::State::default(),
        &logins,
        &checkpoint,
        None
    ));
    logins
        .enrollment
        .finish(&request, Phase::CancelRequested)
        .unwrap();
    assert!(!accounts::checkpoint_current(
        &row,
        &accounts::State::default(),
        &logins,
        &checkpoint,
        None
    ));
    assert!(logins.enrollment.publishing(&request).is_err());
}
