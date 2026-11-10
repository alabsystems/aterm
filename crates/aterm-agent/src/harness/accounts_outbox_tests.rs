// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts;
use aterm_db::accounts::publication::{Destination, test_completion};
use aterm_json::json;

const NOW: i64 = 1_800_000_000;

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        MUTANT.set(0);
        accounts::SYNC_COMMIT_CUT.set(None);
    }
}

fn draft(number: &str) -> Draft {
    Draft::new(
        Destination::new([1; 16], [2; 32], [3; 32], [4; 16], 1, [5; 48]).unwrap(),
        "phones".into(),
        "one".into(),
        None,
        json!({"number":number}),
    )
    .unwrap()
}

#[test]
fn queue_preserves_pending_entries_and_allocates_distinct_a_b_a_intents() {
    let mut queue = Queue::default();
    let a = queue.enqueue(draft("+15555550100")).unwrap();
    let b = queue.enqueue(draft("+15555550200")).unwrap();
    let again = queue.enqueue(draft("+15555550100")).unwrap();
    assert_eq!((a.ordinal, b.ordinal, again.ordinal), (1, 2, 3));
    assert_ne!(
        a.pending.request().unwrap(),
        again.pending.request().unwrap()
    );
    let done = test_completion(&a.pending, [9; 32]);
    assert!(queue.acknowledge(&a, &done).unwrap());
    let after = queue.encode().unwrap();
    assert!(!queue.acknowledge(&a, &done).unwrap());
    assert_eq!(queue.encode().unwrap(), after);
    let next = queue.enqueue(draft("+15555550300")).unwrap();
    assert_eq!(next.ordinal, 4);
    assert!(queue.acknowledge(&b, &done).is_err());
}

#[test]
fn encoded_capacity_failure_preserves_the_original_queue() {
    let mut value = json!({"number":"+15555550100", "details":{}});
    for n in 0..29 {
        value
            .as_object_mut()
            .unwrap()
            .get_mut("details")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                format!("note{n}"),
                json!({"kind":"note","value":"x".repeat(2048)}),
            );
    }
    let big = Draft::new(
        Destination::new([1; 16], [2; 32], [3; 32], [4; 16], 1, [5; 48]).unwrap(),
        "phones".into(),
        "one".into(),
        None,
        value,
    )
    .unwrap();
    let mut queue = Queue::default();
    for _ in 0..=MAX_ENTRIES {
        let before = queue.encode().unwrap();
        if queue.enqueue(big.clone()).is_err() {
            assert_eq!(queue.encode().unwrap(), before);
            assert!(queue.entries.len() < MAX_ENTRIES);
            return;
        }
    }
    panic!("encoded capacity was never enforced");
}

#[test]
fn queue_rejects_rebound_ordinals_and_receipt_high_water_rollback() {
    let mut queue = Queue::default();
    let first = queue.enqueue(draft("+15555550100")).unwrap();
    let second = queue.enqueue(draft("+15555550200")).unwrap();
    let completion = test_completion(&first.pending, [9; 32]);
    let before = queue.encode().unwrap();
    for ordinal in [0, second.ordinal, second.ordinal + 1] {
        let mut rebound = first.clone();
        rebound.ordinal = ordinal;
        assert!(queue.acknowledge(&rebound, &completion).is_err());
        assert_eq!(queue.encode().unwrap(), before);
    }
    let mut after = queue.clone();
    after.last -= 1;
    assert!(after.preserves_ordinals_after(&queue).is_err());
    after = queue.clone();
    after.entries[0].pending = draft("+15555550300").allocate().unwrap();
    assert!(after.preserves_ordinals_after(&queue).is_err());
    after = queue.clone();
    after.entries.remove(0);
    assert!(after.preserves_ordinals_after(&queue).is_ok());
}

#[test]
fn full_queue_refuses_before_editing_local_account_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(root.path());
    store
        .enqueue_publication_within(&[], NOW, draft("+15555550100"), |_, _| Ok(Vec::new()))
        .unwrap();
    let (_, mut queue) = store.outbox_unfenced().unwrap();
    for _ in 1..MAX_ENTRIES {
        queue.enqueue(draft("+15555550100")).unwrap();
    }
    let before = queue.encode().unwrap();
    std::fs::write(store.outbox_file(), &before).unwrap();
    let called = std::cell::Cell::new(false);
    assert!(
        store
            .enqueue_publication_within(&[], NOW, draft("+15555550200"), |_, _| {
                called.set(true);
                Ok(Vec::new())
            })
            .is_err()
    );
    assert!(!called.get());
    assert_eq!(
        std::fs::read_to_string(store.outbox_file()).unwrap(),
        before
    );
}

#[cfg(unix)]
#[test]
fn linked_queue_or_witness_is_never_followed() {
    for witness in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at(root.path());
        store
            .enqueue_publication_within(&[], NOW, draft("+15555550100"), |_, _| Ok(Vec::new()))
            .unwrap();
        let target = root.path().join("unrelated");
        std::fs::write(&target, "untouched").unwrap();
        let path = if witness {
            store.outbox_witness()
        } else {
            store.outbox_file()
        };
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(store.pending_publications().is_err());
        assert!(
            store
                .enqueue_publication_within(&[], NOW, draft("+15555550200"), |_, _| Ok(Vec::new()))
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    }
}

#[test]
fn reading_pending_publications_recovers_an_interrupted_local_receipt() {
    let _reset = Reset;
    let root = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(root.path());
    accounts::SYNC_COMMIT_CUT.set(Some(7));
    assert!(
        store
            .enqueue_publication_within(&[], NOW, draft("+15555550100"), |state, _| {
                state.swarm_label = Some("m8".into());
                Ok(Vec::new())
            })
            .is_err()
    );
    accounts::SYNC_COMMIT_CUT.set(None);
    assert_eq!(store.pending_publications().unwrap().len(), 1);
    assert_eq!(
        store.load(NOW).unwrap().0.swarm_label.as_deref(),
        Some("m8")
    );
    assert!(!store.recover_state_within(&[]).unwrap());
}

#[test]
fn first_outbox_witness_can_precede_its_image_during_recovery() {
    let _reset = Reset;
    let root = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(root.path());
    accounts::SYNC_COMMIT_CUT.set(Some(3));
    assert!(
        store
            .enqueue_publication_within(&[], NOW, draft("+15555550100"), |_, _| Ok(Vec::new()))
            .is_err()
    );
    assert!(!store.outbox_file().exists());
    // Replay retains this marker before installing the first queue image. A
    // process death between those writes must not make its receipt unrecoverable.
    std::fs::write(store.outbox_witness(), "").unwrap();
    assert_eq!(store.pending_publications().unwrap().len(), 1);
    assert!(!store.recover_state_within(&[]).unwrap());
}

#[test]
fn recovery_preserves_a_conflicting_outbox_image() {
    let _reset = Reset;
    let root = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(root.path());
    accounts::SYNC_COMMIT_CUT.set(Some(3));
    assert!(
        store
            .enqueue_publication_within(&[], NOW, draft("+15555550100"), |_, _| Ok(Vec::new()))
            .is_err()
    );
    let mut other = Queue::default();
    other.enqueue(draft("+15555550200")).unwrap();
    let conflicting = other.encode().unwrap();
    std::fs::write(store.outbox_witness(), "").unwrap();
    std::fs::write(store.outbox_file(), &conflicting).unwrap();
    assert!(store.pending_publications().is_err());
    assert_eq!(
        std::fs::read_to_string(store.outbox_file()).unwrap(),
        conflicting
    );
    assert!(store.sync_read_receipt().is_err());
}

fn receipt_case() -> bool {
    let mutant = MUTANT.get();
    let mut safe = true;
    for cut in [0, 1, 2, 3, 7, 8, 9] {
        MUTANT.set(mutant);
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at(root.path());
        accounts::SYNC_COMMIT_CUT.set((cut != 9).then_some(cut));
        let result =
            store.enqueue_publication_within(&[], NOW, draft("+15555550100"), |state, _| {
                state.swarm_label = Some("m8".into());
                Ok(Vec::new())
            });
        safe &= result.is_ok() == (cut == 9);
        if cut != 9 {
            safe &= store.sync_read_receipt().is_err() && store.load(NOW).is_err();
            accounts::SYNC_COMMIT_CUT.set(None);
            safe &= matches!(store.recover_state_within(&[]), Ok(true));
            safe &= matches!(store.recover_state_within(&[]), Ok(false));
        }
        safe &= store
            .pending_publications()
            .is_ok_and(|entries| entries.len() == 1);
        safe &= store
            .load(NOW)
            .is_ok_and(|(state, _)| state.swarm_label.as_deref() == Some("m8"));
    }
    safe
}

fn capacity_case() -> bool {
    let mut queue = Queue::default();
    for _ in 0..MAX_ENTRIES {
        if queue.enqueue(draft("+15555550100")).is_err() {
            return false;
        }
    }
    let before = queue.encode().unwrap();
    queue.enqueue(draft("+15555550200")).is_err() && queue.encode().unwrap() == before
}

fn stale_case() -> bool {
    let mut queue = Queue::default();
    let first = queue.enqueue(draft("+15555550100")).unwrap();
    let completion = test_completion(&first.pending, [9; 32]);
    if queue.acknowledge(&first, &completion) != Ok(true) {
        return false;
    }
    queue.enqueue(draft("+15555550200")).unwrap();
    let before = queue.encode().unwrap();
    queue.acknowledge(&first, &completion) == Ok(false) && queue.encode().unwrap() == before
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(state, action, 0)
}

fn transition_with(
    state: &aterm_spec::interp::State,
    action: &str,
    mutant: u8,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "Scenario" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Check");
    let _reset = Reset;
    MUTANT.set(mutant);
    let (field, result) = match state["scenario"] {
        0 => ("retained", receipt_case()),
        1 => ("capacity", capacity_case()),
        2 => ("isolated", stale_case()),
        _ => unreachable!(),
    };
    next.insert("done", 1);
    next.insert(field, i64::from(result));
    next
}

#[test]
fn outbox_model_matches_receipts_capacity_and_stale_completions() {
    let model = aterm_models_harness::account_publication_outbox_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                checked += usize::from(action.name == "Check");
            }
        }
    }
    assert_eq!(checked, 3);
    for (mutant, scenario, law) in [
        (1, 0, "ReceiptRetainsPublication"),
        (2, 1, "CapacityPreservesPending"),
        (3, 2, "StaleCompletionIsIsolated"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        let bad = transition_with(&state, "Check", mutant);
        assert!(!model.check_invariant(law, &bad), "mutant{mutant}: {law}");
        assert!(!model.successors("Check", &state).contains(&bad));
    }
}

#[test]
fn lost_outbox_or_witness_refuses_queue_updates_but_not_unrelated_usage() {
    for missing in [FILE, WITNESS] {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at(root.path());
        store
            .enqueue_publication_within(&[], NOW, draft("+15555550100"), |_, _| Ok(Vec::new()))
            .unwrap();
        std::fs::remove_file(if missing == FILE {
            store.outbox_file()
        } else {
            store.outbox_witness()
        })
        .unwrap();
        assert!(store.pending_publications().is_err());
        assert!(
            store
                .enqueue_publication_within(&[], NOW, draft("+15555550200"), |_, _| Ok(Vec::new()))
                .is_err()
        );
        let held = store.acquire().unwrap();
        store
            .transact_state_held(&held, NOW, &mut None, |state| {
                state.swarm_label = Some("m9".into());
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(
            store.load(NOW).unwrap().0.swarm_label.as_deref(),
            Some("m9")
        );
    }
}

#[test]
fn ack_is_durable_and_cannot_clear_a_later_entry() {
    for cut in [0, 1, 2, 3, 7, 8, 9] {
        let _reset = Reset;
        let root = aterm_tempfile::tempdir().unwrap();
        let store = Store::at(root.path());
        let (first, _) = store
            .enqueue_publication_within(&[], NOW, draft("+15555550100"), |_, _| Ok(Vec::new()))
            .unwrap();
        let completion = test_completion(&first.pending, [9; 32]);
        accounts::SYNC_COMMIT_CUT.set((cut != 9).then_some(cut));
        let result = store.acknowledge_publication_within(&[], NOW, &first, &completion);
        assert_eq!(result.is_ok(), cut == 9);
        accounts::SYNC_COMMIT_CUT.set(None);
        if cut != 9 {
            assert!(store.recover_state_within(&[]).unwrap());
        }
        assert!(store.pending_publications().unwrap().is_empty());
        let (second, _) = store
            .enqueue_publication_within(&[], NOW, draft("+15555550200"), |_, _| Ok(Vec::new()))
            .unwrap();
        assert!(second.ordinal > first.ordinal);
        assert!(
            !store
                .acknowledge_publication_within(&[], NOW, &first, &completion)
                .unwrap()
        );
        assert_eq!(store.pending_publications().unwrap(), vec![second]);
    }
}
