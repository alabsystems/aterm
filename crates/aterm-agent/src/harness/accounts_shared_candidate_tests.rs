// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

struct Fixed(Grant);

impl Authority for Fixed {
    fn prepare(&self, _: &Row) -> Result<Grant, String> {
        Ok(self.0.clone())
    }

    fn current(&self, grant: &Grant, _: &Row) -> Result<(), String> {
        (*grant == self.0)
            .then_some(())
            .ok_or_else(|| "changed fixture approval".into())
    }
}

struct Denied;

impl Authority for Denied {
    fn prepare(&self, _: &Row) -> Result<Grant, String> {
        Err("fixture approval refused".into())
    }

    fn current(&self, _: &Grant, _: &Row) -> Result<(), String> {
        Err("fixture approval refused".into())
    }
}

fn stamp(guard: &mut AutoRenewGuard, candidate: &Candidate, when: Instant, attributed: bool) {
    let grant = candidate.authority.prepare(&candidate.row).unwrap();
    let key = shared_key(grant.source);
    guard.last_attempt.insert(key.clone(), when);
    if attributed {
        guard
            .shared_preparations
            .insert(key, (grant.source, grant.decision));
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut a = super::super::automatic_shared_tests::unstarted_candidate(1, &calls);
    let mut b = super::super::automatic_shared_tests::unstarted_candidate(2, &calls);
    let mut history = AutoRenewGuard::default();
    let start = Instant::now();
    let phase = match action {
        "Tie" => 1,
        "Unseen" => {
            stamp(&mut history, &a, start, true);
            2
        }
        "Oldest" | "Reissue" => {
            stamp(&mut history, &a, start, true);
            stamp(&mut history, &b, start + Duration::from_secs(1), true);
            if action == "Reissue" {
                let mut grant = b.authority.prepare(&b.row).unwrap();
                grant.decision = [99; 32];
                grant.generation += 1;
                b.authority = Arc::new(Fixed(grant));
                4
            } else {
                3
            }
        }
        "Unattributed" => {
            stamp(&mut history, &a, start + Duration::from_secs(1), false);
            stamp(&mut history, &b, start, true);
            5
        }
        "Refused" => {
            a.authority = Arc::new(Denied);
            6
        }
        "Context" | "ContextHistory" => {
            let mut grant_a = a.authority.prepare(&a.row).unwrap();
            let mut grant_b = b.authority.prepare(&b.row).unwrap();
            grant_a.source.network = [2; 32];
            grant_b.source.network = [1; 32];
            grant_b.source.account = grant_a.source.account;
            a.authority = Arc::new(Fixed(grant_a));
            b.authority = Arc::new(Fixed(grant_b));
            if action == "ContextHistory" {
                stamp(&mut history, &a, start + Duration::from_secs(1), true);
                stamp(&mut history, &b, start, true);
                8
            } else {
                7
            }
        }
        _ => panic!("unknown shared candidate action {action}"),
    };
    let before = (
        history.last_attempt.clone(),
        history.shared_preparations.clone(),
    );
    let guard = Arc::new(std::sync::Mutex::new(history));
    // Reverse discovery order makes deterministic tie-breaking observable.
    let ordered = ordered_shared_candidates(vec![b, a], &guard);
    let first = match ordered[0].row.label.as_str() {
        "candidate-1" => 1,
        "candidate-2" => 2,
        _ => panic!("unexpected candidate"),
    };
    let after = guard.lock().unwrap();
    let mut observed = state.clone();
    observed.insert("phase", phase);
    observed.insert("first", first);
    observed.insert("count", i64::try_from(ordered.len()).unwrap());
    observed.insert(
        "kept",
        i64::from(
            before
                == (
                    after.last_attempt.clone(),
                    after.shared_preparations.clone(),
                ),
        ),
    );
    assert!(calls.lock().unwrap().is_empty());
    observed
}

#[test]
fn shared_candidate_priority_conforms_and_rejects_wrong_order() {
    let model = aterm_models_harness::shared_candidate_priority_model();
    assert_eq!(model.mutant_dials(), (1..=9).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut wrong = observed.clone();
                wrong.insert("first", 3 - observed["first"]);
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}

pub(crate) fn progress_transition(state: &State, _: &str) -> State {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let a = super::super::automatic_shared_tests::unstarted_candidate(1, &calls);
    let b = super::super::automatic_shared_tests::unstarted_candidate(2, &calls);
    let grant_a = a.authority.prepare(&a.row).unwrap();
    let grant_b = b.authority.prepare(&b.row).unwrap();
    let mut history = AutoRenewGuard::default();
    let start = Instant::now();
    for (id, candidate, seen) in [(1, &a, state["a"]), (2, &b, state["b"])] {
        if seen == 1 {
            stamp(
                &mut history,
                candidate,
                start + Duration::from_secs(u64::from(state["newest"] == id)),
                true,
            );
        }
    }
    let guard = Arc::new(std::sync::Mutex::new(history));
    let ordered = ordered_shared_candidates(vec![b, a], &guard);
    let grant = ordered[0].authority.prepare(&ordered[0].row).unwrap();
    let selected = if grant.source == grant_a.source { 1 } else { 2 };
    let key = shared_key(grant.source);
    let permit =
        AutoRenewPermit::claim_preparation(&guard, &key, start + Duration::from_secs(901), &grant)
            .unwrap();
    drop(permit);
    let history = guard.lock().unwrap();
    let a_time = shared_priority(&history, &grant_a).0;
    let b_time = shared_priority(&history, &grant_b).0;
    let mut observed = state.clone();
    observed.insert("a", i64::from(a_time.is_some()));
    observed.insert("b", i64::from(b_time.is_some()));
    observed.insert("newest", if a_time > b_time { 1 } else { 2 });
    observed.insert("last", selected);
    observed.insert("repeated", i64::from(state["last"] == selected));
    observed
}

#[test]
fn shared_candidate_progress_conforms_across_repeated_admissions() {
    let model = aterm_models_harness::shared_candidate_progress_model();
    assert_eq!(model.mutant_dials(), (1..=4).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = progress_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut wrong = observed.clone();
                wrong.insert("last", 3 - observed["last"]);
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}

#[test]
fn busy_preparation_admission_does_not_record_a_new_decision() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = super::super::automatic_shared_tests::unstarted_candidate(1, &calls);
    let grant = candidate.authority.prepare(&candidate.row).unwrap();
    let key = shared_key(grant.source);
    let start = Instant::now();
    let mut history = AutoRenewGuard::default();
    history.last_attempt.insert(key.clone(), start);
    history
        .shared_preparations
        .insert(key.clone(), (grant.source, [99; 32]));
    let guard = Arc::new(std::sync::Mutex::new(history));
    let held = AutoRenewPermit::claim_with_mailbox(&guard, "other", start, false, None).unwrap();
    assert!(AutoRenewPermit::claim_preparation(&guard, &key, start, &grant).is_none());
    {
        let history = guard.lock().unwrap();
        assert_eq!(history.last_attempt[&key], start);
        assert_eq!(history.shared_preparations[&key], (grant.source, [99; 32]));
        assert!(history.in_flight);
    }
    drop(held);
    let permit = AutoRenewPermit::claim_preparation(&guard, &key, start, &grant).unwrap();
    assert_eq!(
        guard.lock().unwrap().shared_preparations[&key],
        (grant.source, grant.decision)
    );
    drop(permit);
}

#[test]
fn foreign_source_history_is_unseen_without_resetting_it() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let a = super::super::automatic_shared_tests::unstarted_candidate(1, &calls);
    let b = super::super::automatic_shared_tests::unstarted_candidate(2, &calls);
    let mut history = AutoRenewGuard::default();
    stamp(&mut history, &a, Instant::now(), true);
    let grant = a.authority.prepare(&a.row).unwrap();
    let mut foreign = grant.source;
    foreign.network = [99; 32];
    history
        .shared_preparations
        .insert(shared_key(grant.source), (foreign, grant.decision));
    assert_eq!(shared_priority(&history, &grant).0, None);
    let before = (
        history.last_attempt.clone(),
        history.shared_preparations.clone(),
    );
    let guard = Arc::new(std::sync::Mutex::new(history));
    let ordered = ordered_shared_candidates(vec![b, a], &guard);
    assert_eq!(ordered[0].row.label, "candidate-1");
    let after = guard.lock().unwrap();
    assert_eq!(
        before,
        (
            after.last_attempt.clone(),
            after.shared_preparations.clone()
        )
    );
}
