// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn witness(receipt: &mut Receipt, case: u8) {
    match case {
        0 => receipt.launch_attempted = true,
        1 => receipt.accepted_turn = Some(receipt.intent.wall.clone()),
        2 => {
            receipt.current_attempt = Some(renewal::AttemptRef {
                id: "b".repeat(32),
                generation: 1,
            });
        }
        3 => {
            receipt.candidate = Some(renewal::Candidate {
                pid: 10,
                birth: 1,
                parent: 1,
                group: 10,
                session: 10,
                launcher_device: 1,
                launcher_inode: 2,
                launcher_path: b"/native/launcher".to_vec(),
                forward: true,
            });
        }
        4 => receipt.successor_pid = Some(10),
        5 => receipt.successor_birth = Some(1),
        _ => panic!("unknown execution witness"),
    }
}

fn refuses_conflicting_evidence(cancel: bool) {
    for case in 0..6 {
        let (world, _, env, mut receipt, _) = reserved("terminal-execution-evidence");
        receipt.phase = if cancel { Phase::Ready } else { Phase::Retired };
        witness(&mut receipt, case);
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
            .unwrap()
            .unwrap();
        let store = world.seat().store(Cli::Codex);
        let before = store.load(NOW).unwrap().0.to_json();
        let logins = store.load_logins(NOW).unwrap().0.to_json();
        let path = renewal::receipt_path(&env, &receipt.id).unwrap();
        let retained = std::fs::read(&path).unwrap();
        let rejected = if cancel {
            renewal::cancel_unexecuted_rotation(&env, &receipt, "work").is_err()
        } else {
            renewal::retire_pool_record(&env, &receipt).is_err()
        };
        assert!(rejected, "execution witness {case} must prevent cleanup");
        assert_eq!(store.load(NOW).unwrap().0.to_json(), before);
        assert_eq!(store.load_logins(NOW).unwrap().0.to_json(), logins);
        assert_eq!(std::fs::read(path).unwrap(), retained);
    }
}

#[test]
fn unexecuted_cancellation_refuses_conflicting_execution_metadata() {
    refuses_conflicting_evidence(true);
}

#[test]
fn terminal_retirement_refuses_conflicting_execution_metadata() {
    refuses_conflicting_evidence(false);
}

#[test]
fn unattempted_lifecycle_keeps_conflicting_execution_metadata() {
    for case in 0..6 {
        let (_world, _, env, mut receipt, _) = reserved("unattempted-lifecycle-evidence");
        witness(&mut receipt, case);
        let mut retired = receipt.clone();
        retired.phase = Phase::Retired;
        assert!(
            !retired.may_replace(),
            "witness {case} must retain its index"
        );

        let mut committing = receipt.clone();
        committing.phase = Phase::Committing;
        let before = committing.clone();
        assert!(!committing.reset_before_signal(0));
        assert_eq!(committing, before);
        let before = receipt.clone();
        assert!(!receipt.retire_stale_ready(u64::MAX, u64::MAX, u64::MAX));
        assert_eq!(receipt, before);

        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
            .unwrap()
            .unwrap();
        let path = renewal::receipt_path(&env, &receipt.id).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(renewal::commit_receipt(&env, &receipt).is_err());
        assert!(
            codex_rotation::with_prepared_writer(
                &env,
                &receipt,
                |_, _| -> Result<(), &'static str> {
                    panic!("conflicting receipt must not reach prepared selection")
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn revoked_unattempted_receipt_cannot_reset_or_commit() {
    let (_world, _, env, mut receipt, _) = reserved("revoked-unattempted-lifecycle");
    receipt.revoked = true;
    receipt.phase = Phase::Committing;
    let before = receipt.clone();
    assert!(!receipt.reset_before_signal(0));
    assert_eq!(receipt, before);
    receipt.phase = Phase::Retired;
    assert!(!receipt.may_replace());
    receipt.phase = Phase::Ready;
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
        .unwrap()
        .unwrap();
    let path = renewal::receipt_path(&env, &receipt.id).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(renewal::commit_receipt(&env, &receipt).is_err());
    assert!(
        codex_rotation::with_prepared_writer(&env, &receipt, |_, _| -> Result<(), &'static str> {
            panic!("revoked receipt must not reach prepared selection")
        })
        .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

fn apply_case(before: &aterm_spec::interp::State, mutant: u8) -> aterm_spec::interp::State {
    let (world, _, env, mut receipt, _) = reserved("terminal-evidence-model");
    let retirement = before["retirement"] == 1;
    let case = before["case"];
    receipt.phase = if retirement {
        Phase::Retired
    } else {
        Phase::Ready
    };
    for bit in 0..6 {
        if case & (1 << bit) != 0 {
            witness(&mut receipt, bit);
        }
    }
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
        .unwrap()
        .unwrap();
    let store = world.seat().store(Cli::Codex);
    let rotations = store.load(NOW).unwrap().0.rotations.len();
    let path = renewal::receipt_path(&env, &receipt.id).unwrap();
    let retained = std::fs::read(&path).unwrap();
    if (1..=6).contains(&mutant) && case == 1 << (mutant - 1) {
        // Negative control: discard the contradictory witness before entering
        // the real writer, simulating a caller that trusts only its own marker.
        receipt.launch_attempted = false;
        receipt.accepted_turn = None;
        receipt.current_attempt = None;
        receipt.candidate = None;
        receipt.successor_pid = None;
        receipt.successor_birth = None;
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
            .unwrap()
            .unwrap();
    }
    let admitted = if mutant == 7 && case == 0 {
        false
    } else if retirement {
        renewal::retire_pool_record(&env, &receipt).is_ok_and(|value| value.is_some())
    } else {
        renewal::cancel_unexecuted_rotation(&env, &receipt, "work")
            .is_ok_and(|value| value.is_some())
    };
    if mutant == 8 && retirement {
        store
            .transact(NOW, &mut None, |state| {
                state.rotations.pop();
                Ok(Vec::new())
            })
            .unwrap();
    }
    if mutant == 9 && case > 0 {
        let mut changed = std::fs::read(&path).unwrap();
        changed.push(b'\n');
        std::fs::write(&path, changed).unwrap();
    }
    let mut after = before.clone();
    after.insert("done", 1);
    after.insert("admitted", i64::from(admitted));
    after.insert(
        "removed",
        i64::from(
            !store
                .load_logins(NOW)
                .unwrap()
                .0
                .native_rotations
                .contains_key(&receipt.id),
        ),
    );
    after.insert(
        "refunded",
        i64::from(store.load(NOW).unwrap().0.rotations.len() < rotations),
    );
    after.insert(
        "rewritten",
        i64::from(std::fs::read(path).unwrap() != retained),
    );
    after
}

pub(crate) fn transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut after = before.clone();
    match action {
        "NextCase" => {
            after.insert("case", before["case"] + 1);
        }
        "Retirement" => {
            after.insert("retirement", 1);
        }
        "Apply" => return apply_case(before, 0),
        _ => unreachable!(),
    }
    after
}

#[test]
fn terminal_evidence_refines_both_cleanup_entrypoints() {
    let model = aterm_models_harness::terminal_evidence_model();
    aterm_spec::interp::prove_and_catch(&model);
    for before in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &before) {
                let after = transition(&before, action.name);
                assert!(
                    model.successors(action.name, &before).contains(&after),
                    "{}: {before:?} -> {after:?}",
                    action.name
                );
            }
        }
    }
    for (mutant, case, retirement, law) in [
        (1, 1, 0, "NoConflictingAdmission"),
        (2, 2, 1, "NoConflictingCleanup"),
        (3, 4, 0, "NoConflictingRefund"),
        (4, 8, 1, "NoConflictingCleanup"),
        (5, 16, 0, "NoConflictingAdmission"),
        (6, 32, 1, "NoConflictingAdmission"),
        (7, 0, 1, "AvailableCleanup"),
        (7, 0, 0, "CancellationRefund"),
        (8, 0, 1, "RetirementKeepsBudget"),
        (9, 2, 0, "ConflictingReceiptUnchanged"),
    ] {
        let mut before = model.init_state();
        before.insert("case", case);
        before.insert("retirement", retirement);
        let after = apply_case(&before, mutant);
        assert!(!model.check_invariant(law, &after), "{law}");
        assert!(!model.successors("Apply", &before).contains(&after));
    }
}
