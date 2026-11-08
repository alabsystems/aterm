// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_spec::interp::State as Projection;

fn release_case(before: &Projection, mutant: u8) -> Projection {
    let (_world, _reader, env, mut receipt, _plan) = reserved("native-canary-model");
    let store = env.accounts_store_for(Cli::Codex).unwrap();
    let mut other = receipt.clone();
    other.id = "b".repeat(32);
    other.intent.wall = "00000000-0000-0000-0000-000000000003".into();
    let other_binding = codex_rotation::receipt_binding(&other).unwrap();
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &other))
        .unwrap()
        .unwrap();
    if before["mapped"] == 0 {
        receipt.phase = Phase::Retired;
        receipt.revoked = true;
        renewal::with_receipt_writer(&env, |writer| {
            renewal::write_receipt(&env, &receipt)?;
            writer.retire_pool_record(&receipt)
        })
        .unwrap()
        .unwrap();
    }
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            let mut current = state.canary.clone().unwrap();
            current.took = before["taken"] == 1;
            current.native_receipt = match before["owner"] {
                1 => Some(receipt.id.clone()),
                2 => Some(other.id.clone()),
                _ => None,
            };
            state.canary = (before["owner"] > 0).then_some(current);
            logins
                .native_rotations
                .insert(other.id.clone(), other_binding);
            Ok(Vec::new())
        })
        .unwrap();
    let original_rotations = store.load(NOW).unwrap().0.rotations.len();
    let wrong_strike = (mutant == 1 && before["mapped"] == 1)
        || (mutant == 7 && before["mapped"] == 0 && before["owner"] == 1);
    if wrong_strike {
        // The old implementation names only the reused session and account.
        store
            .transact(NOW, &mut None, |state| {
                // Reproduce the old writer, which had no reservation identity.
                if let Some(canary) = &mut state.canary {
                    canary.native_receipt = None;
                }
                Ok(accounts::strike(
                    state,
                    "work",
                    &receipt.sid,
                    &accounts::Strike::NotMoved("stale cancellation".into()),
                    NOW,
                ))
            })
            .unwrap();
    }
    if mutant == 4 && before["owner"] == 1 {
        store
            .transact(NOW, &mut None, |state| {
                if let Some(canary) = &mut state.canary {
                    canary.took = false;
                }
                Ok(Vec::new())
            })
            .unwrap();
    }
    if mutant == 5 && before["owner"] > 1 && before["mapped"] == 1 {
        store
            .transact(NOW, &mut None, |state| {
                state.rotations.pop();
                Ok(Vec::new())
            })
            .unwrap();
    }
    if mutant != 2 {
        let expected = if mutant == 3 && before["owner"] == 2 {
            &other
        } else {
            &receipt
        };
        renewal::cancel_unexecuted_rotation(&env, expected, "work")
            .unwrap()
            .unwrap();
    }
    if mutant == 6 && before["mapped"] == 1 {
        let binding = codex_rotation::receipt_binding(&receipt).unwrap();
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.native_rotations.insert(receipt.id.clone(), binding);
                Ok(Vec::new())
            })
            .unwrap();
    }
    let after = store.load(NOW).unwrap().0;
    let logins = store.load_logins(NOW).unwrap().0;
    let remaining =
        after
            .canary
            .as_ref()
            .map_or(0, |canary| match canary.native_receipt.as_deref() {
                Some(id) if id == receipt.id => 1,
                Some(id) if id == other.id => 2,
                _ => 3,
            });
    let mut next = before.clone();
    next.insert("done", 1);
    next.insert("remaining", remaining);
    next.insert(
        "refund",
        i64::from(after.rotations.len() < original_rotations),
    );
    next.insert(
        "removed",
        i64::from(before["mapped"] == 1 && !logins.native_rotations.contains_key(&receipt.id)),
    );
    next.insert(
        "other_removed",
        i64::from(!logins.native_rotations.contains_key(&other.id)),
    );
    next
}

pub(crate) fn transition(before: &Projection, action: &str) -> Projection {
    if action == "Release" {
        return release_case(before, 0);
    }
    let mut next = before.clone();
    match action {
        "NextOwner" => {
            next.insert("owner", before["owner"] + 1);
            next.insert("remaining", before["remaining"] + 1);
        }
        "Taken" => {
            next.insert("taken", 1);
        }
        "Forget" => {
            next.insert("mapped", 0);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn native_canary_ownership_refines_real_cancellation_and_pool_records() {
    let model = aterm_models_harness::native_canary_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
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
    for (mutant, owner, taken, mapped, law) in [
        (1, 2, 1, 1, "ForeignCanary"),
        (1, 3, 0, 1, "ForeignBudget"),
        (2, 1, 0, 1, "OwnCanary"),
        (2, 1, 0, 1, "OwnRefund"),
        (3, 2, 0, 1, "OtherReceipt"),
        (4, 1, 1, 1, "TakenBudget"),
        (5, 2, 0, 1, "ForeignBudget"),
        (6, 1, 0, 1, "ExactRetirement"),
        (7, 1, 0, 0, "MissingReceipt"),
    ] {
        let mut before = model.init_state();
        before.insert("owner", owner);
        before.insert("remaining", owner);
        before.insert("taken", taken);
        before.insert("mapped", mapped);
        let after = release_case(&before, mutant);
        assert!(!model.check_invariant(law, &after), "{law}");
        assert!(!model.successors("Release", &before).contains(&after));
    }
}
