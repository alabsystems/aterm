// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn quarantine_never_promotes_the_boundary_or_a_live_source() {
    assert!(!quarantine_eligible(0, 0, true));
    assert!(quarantine_eligible(1, 0, true));
    assert!(!quarantine_eligible(1, 1, true));
    assert!(!quarantine_eligible(u64::MAX, u64::MAX, true));
    assert!(quarantine_eligible(u64::MAX, u64::MAX - 1, true));
    assert!(!quarantine_eligible(u64::MAX, 0, false));
}
#[test]
fn full_width_accounting_does_not_wrap_or_reclaim_an_absent_peers_share() {
    assert!(u64::MAX.checked_add(1).is_none());
    assert!(admissible(u64::MAX, 0, u64::MAX));
    assert!(!admissible(10, 6, 5));
    assert!(!admissible(10, 6, 11));
    assert_eq!(merge_spent(7, 3), merge_spent(3, 7));
    assert_eq!(merge_spent(7, 7), 7);
}
#[test]
fn offline_rights_model_has_live_actions_and_rejects_double_allocation() {
    let model = aterm_models_db::offline_rights_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
}
fn rights_state(a: u64, b: u64, disconnected: i64) -> aterm_spec::interp::State {
    std::collections::BTreeMap::from([
        ("a", a as i64),
        ("b", b as i64),
        ("disconnected", disconnected),
    ])
}
#[test]
fn real_admission_conforms_at_every_reachable_partition_state_and_catches_reclaim() {
    let model = aterm_models_db::offline_rights_model();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let (mut a, mut b, mut d) =
                (state["a"] as u64, state["b"] as u64, state["disconnected"]);
            match action.name {
                "SpendA" => {
                    if admissible(2, a, a + 1) {
                        a += 1;
                    }
                }
                "SpendB" => {
                    if admissible(2, b, b + 1) {
                        b += 1;
                    }
                }
                "Separate" => d = 1,
                "Reconnect" => d = 0,
                other => panic!("unknown real transition {other}"),
            }
            let next = rights_state(a, b, d);
            assert!(
                model.successors(action.name, &state).contains(&next),
                "{}: {state:?} -> {next:?}",
                action.name
            );
        }
    }
    let full = rights_state(2, 0, 1);
    assert!(!admissible(2, 2, 3));
    let bad = rights_state(if admissible(4, 2, 3) { 3 } else { 2 }, 0, 1);
    assert!(
        !model.successors("SpendA", &full).contains(&bad),
        "negative control: reclaiming the silent peer's share must break conformance"
    );
    assert!(!model.check_invariant("ShareA", &bad));
}
