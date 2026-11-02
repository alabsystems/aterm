// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn retained(lab: &Lab, now: u64) -> usize {
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    directory
        .ring_view(&session, &RECOVERIES)
        .window(&RECOVERIES, now)
        .map(|period| {
            let scope = RECOVERIES.scope(directory.trust(), period).unwrap();
            Store::observe_if_exists(&session, &directory.path(scope).unwrap(), scope)
                .unwrap()
                .map_or(0, |store| {
                    Log::read_with(&session, &store, scope, decode)
                        .unwrap()
                        .snapshot()
                        .retained
                })
        })
        .sum()
}

fn snapshot(lab: &Lab, state: &State) -> State {
    let now = NOW + state["day"] as u64 * 86_400;
    let requests = Recovery::at(lab.paths[0].clone()).pending_at(now).unwrap();
    let decisions: std::collections::BTreeSet<_> = requests.iter().map(Request::decision).collect();
    let mut wanted = std::collections::BTreeSet::new();
    if state["old"] == 1 {
        wanted.insert([2; 32]);
    }
    if state["current"] == 1 {
        wanted.insert([4; 32]);
    }
    assert_eq!(decisions, wanted);
    let mut observed = state.clone();
    observed.insert("visible", requests.len() as i64);
    observed.insert("retained", retained(lab, now) as i64);
    if state["repaired"] == 1 {
        let remote = Recovery::at(lab.paths[1].clone()).pending_at(now).unwrap();
        assert_eq!(
            remote
                .iter()
                .map(Request::decision)
                .collect::<std::collections::BTreeSet<_>>(),
            decisions
        );
        observed.insert("remote", remote.len() as i64);
    }
    observed
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let writer = Recovery::at(lab.paths[0].clone());
    if state["old"] == 1 {
        writer.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    }
    if state["current"] == 1 {
        writer.request_at([1; 32], [4; 32], [5; 16], NOW).unwrap();
    }
    if state["retried"] == 1 {
        writer.request_at([1; 32], [4; 32], [5; 16], NOW).unwrap();
    }
    if state["day"] == 1 {
        writer
            .request_at([1; 32], [2; 32], [3; 16], NOW + 86_400)
            .unwrap();
    }
    assert_eq!(snapshot(&lab, state), *state);
    let mut next = state.clone();
    match action {
        "Current" | "Retry" => {
            writer.request_at([1; 32], [4; 32], [5; 16], NOW).unwrap();
            next.insert(
                if action == "Current" {
                    "current"
                } else {
                    "retried"
                },
                1,
            );
        }
        "Old" => {
            writer.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
            next.insert("old", 1);
        }
        "NextDay" => {
            writer
                .request_at([1; 32], [2; 32], [3; 16], NOW + 86_400)
                .unwrap();
            next.insert("day", 1);
        }
        "Repair" => {
            repair_commons_at(&lab, 3, &[NOW + 86_400, NOW + 86_400]);
            next.insert("repaired", 1);
        }
        _ => panic!("unknown recovery action {action}"),
    }
    snapshot(&lab, &next)
}

#[test]
fn recovery_decisions_conform_across_ordering_retry_day_and_repair() {
    let model = aterm_models_db::recovery_decision_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                assert!(observed["visible"] > 0);
                let mut hidden = observed.clone();
                hidden.insert("visible", observed["visible"] - 1);
                assert!(!model.successors(action.name, &state).contains(&hidden));
            }
        }
    }
}

#[test]
fn newer_attempt_only_replaces_the_same_decisions_attempt() {
    let lab = Lab::new(&[Role::Writer]);
    let writer = Recovery::at(lab.paths[0].clone());
    writer.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    writer.request_at([1; 32], [4; 32], [5; 16], NOW).unwrap();
    writer.request_at([1; 32], [4; 32], [6; 16], NOW).unwrap();
    let requests = writer.pending_at(NOW).unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .iter()
            .find(|r| r.decision() == [2; 32])
            .unwrap()
            .attempt(),
        [3; 16]
    );
    assert_eq!(
        requests
            .iter()
            .find(|r| r.decision() == [4; 32])
            .unwrap()
            .attempt(),
        [6; 16]
    );
}
