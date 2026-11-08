// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::{interp::State, xref::StepEvidence};

fn cap(state: &State) -> Limits {
    Limits {
        records: state["limit"] as usize,
        bytes: state["byte_limit"] as usize,
        record_bytes: ACCOUNT_BYTES,
    }
}

fn candidate(w: &World, which: i64, phase: i64) -> Prepare {
    w.parent
        .with_account_successor(
            if phase == 0 {
                ImagePhase::Forward
            } else {
                ImagePhase::Rollback
            },
            format!("{:032x}", which + 1),
            2,
            8,
            w.parent.deadline_us + 1,
            stored((which + 1) as u8),
        )
        .unwrap()
}

fn lay(state: &State) -> World {
    let w = World::new(ImagePhase::Forward);
    w.seed(cap(state), state["known"] == 1);
    for (which, name) in [(1, "a"), (2, "b")] {
        let c = candidate(&w, which, 0);
        let binding = candidate_binding(&c);
        match state[name] {
            0 => {}
            1 => {
                record_successor_at(&w.paths, &w.parent, &w.binding, &c, &binding, cap(state))
                    .unwrap();
            }
            2 => {
                account::claim_at(&w.paths, w.relation(&c), &binding, cap(state)).unwrap();
            }
            3 => {
                account::fence_at(&w.paths, w.relation(&c), cap(state)).unwrap();
            }
            _ => unreachable!(),
        }
    }
    if state["winner"] > 0 {
        let c = candidate(&w, state["winner"], 0);
        let binding = candidate_binding(&c);
        if state["child"] == 1 || state["child"] == 4 {
            record_event_at_limits(&w.paths, &c, &binding, Some(Event::MayExecute), cap(state))
                .unwrap();
        }
        match state["child"] {
            2 => {
                record_with_phase_at_limits(
                    &w.paths,
                    &c,
                    &binding,
                    Some(Event::NoExec),
                    Some(ImagePhase::Forward),
                    cap(state),
                )
                .unwrap();
            }
            3 | 4 => {
                record_event_at_limits(&w.paths, &c, &binding, Some(Event::Seal), cap(state))
                    .unwrap();
            }
            _ => {}
        }
    }
    assert_eq!(
        project(&w, state.clone()),
        *state,
        "reconstruct exact journal"
    );
    w
}

fn project(w: &World, mut state: State) -> State {
    let (key, view) = read(&w.paths).unwrap().unwrap();
    let parent = &view.entries[&key.operation(&w.parent.id)];
    state.insert("known", i64::from(parent.closed_phase.is_some()));
    let mut winner = 0;
    let mut child = 0;
    for (which, name, phase_name, auth_name, claimed, fenced) in [
        (1, "a", "a_phase", "a_auth", "ca", "fa"),
        (2, "b", "b_phase", "b_auth", "cb", "fb"),
    ] {
        let c = candidate(w, which, 0);
        let operation = key.operation(&c.id);
        let entry = view.entries.get(&operation);
        let kind = entry.map_or(0, |entry| {
            if entry.state == IssuerState::Fenced {
                3
            } else if entry.account.is_some() {
                2
            } else {
                1
            }
        });
        state.insert(name, kind);
        state.insert(claimed, i64::from(kind == 1 || kind == 2));
        state.insert(fenced, i64::from(kind == 3));
        if which == 1 {
            state.insert("old_a", i64::from(kind == 1));
        }
        let link = entry.and_then(|entry| entry.account);
        state.insert(
            phase_name,
            link.map_or(2, |link| i64::from(link.phase == ImagePhase::Rollback)),
        );
        state.insert(auth_name, if link.is_some() { 1 } else { 2 });
        if parent.successor == Some(operation) {
            winner = which;
            child = match entry.unwrap().state {
                IssuerState::Prepared => 0,
                IssuerState::MayExecute => 1,
                IssuerState::NoExec => 2,
                IssuerState::Sealed { may_execute: false } => 3,
                IssuerState::Sealed { may_execute: true } => 4,
                IssuerState::Fenced => panic!("a fence consumed its parent"),
            };
        }
    }
    state.insert("winner", winner);
    state.insert("child", child);
    state.insert("records", view.sequence as i64);
    state.insert("bytes", view.committed_bytes as i64);
    state.insert(
        "held",
        view.entries
            .values()
            .map(|entry| entry.state.reserved() as i64)
            .sum(),
    );
    state
}

fn kind(outcome: AccountOutcome) -> i64 {
    match outcome {
        AccountOutcome::Claimed(_) => 2,
        AccountOutcome::Fenced => 3,
    }
}

fn witnessed<T>(
    evidence: &mut Option<&mut StepEvidence>,
    action: &'static str,
    f: impl FnOnce() -> T,
) -> T {
    match evidence.as_deref_mut() {
        Some(evidence) => evidence.step(action, f),
        None => f(),
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    step(state, action, 0, None)
}

fn step(state: &State, action: &str, fault: u8, mut evidence: Option<&mut StepEvidence>) -> State {
    let mut next = state.clone();
    next.insert("ack", 0);
    next.insert("observed", 0);
    next.insert("which", 0);
    // These unmodeled choices select fixture parameters, not journal transitions.
    // The following real operations reconstruct and verify the selected journal.
    let parameter = match action {
        "LegacyParent" => Some(("known", 0)),
        "WrongPhase" => Some(("phase", 1)),
        "RightPhase" => Some(("phase", 0)),
        "WrongBinding" => Some(("binding", 0)),
        "RightBinding" => Some(("binding", 1)),
        "TightRecords" => Some(("limit", 4)),
        "TightBytes" => Some(("byte_limit", 2455)),
        _ => None,
    };
    if let Some((name, value)) = parameter {
        next.insert(name, value);
        return next;
    }
    let w = lay(state);
    let which = if action.ends_with('B') { 2 } else { 1 };
    let phase = if fault == 1 { 0 } else { state["phase"] };
    let c = candidate(&w, which, phase);
    let binding = candidate_binding(&c);
    let mut parent_binding = w.binding.clone();
    if state["binding"] == 0 && fault != 6 {
        parent_binding.capability = "ee".repeat(32);
    }
    let relation = AccountRelation {
        predecessor: &w.parent,
        predecessor_binding: &parent_binding,
        phase: if phase == 0 {
            ImagePhase::Forward
        } else {
            ImagePhase::Rollback
        },
        candidate: &c,
    };
    match action {
        "ClaimA" | "ClaimB" | "RefuseA" | "RefuseB" | "ReplayA" | "ReplayB" => {
            let name = match action {
                "ClaimA" => "ClaimA",
                "ClaimB" => "ClaimB",
                "RefuseA" => "RefuseA",
                "RefuseB" => "RefuseB",
                "ReplayA" => "ReplayA",
                _ => "ReplayB",
            };
            let result = witnessed(&mut evidence, name, || {
                account::claim_at(&w.paths, relation, &binding, cap(state))
            });
            next.insert("ack", result.map_or(0, |result| kind(result.outcome)));
            next.insert("which", which);
            if fault == 9 && action == "ReplayA" {
                let extra = w.candidate(99);
                account::fence_at(&w.paths, w.relation(&extra), cap(state)).unwrap();
            }
        }
        "FenceA" | "FenceB" | "RefuseFenceA" | "RefuseFenceB" | "ReplayFenceA" | "ReplayFenceB" => {
            let name = match action {
                "FenceA" => "FenceA",
                "FenceB" => "FenceB",
                "RefuseFenceA" => "RefuseFenceA",
                "RefuseFenceB" => "RefuseFenceB",
                "ReplayFenceA" => "ReplayFenceA",
                _ => "ReplayFenceB",
            };
            let result = witnessed(&mut evidence, name, || {
                account::fence_at(&w.paths, relation, cap(state))
            });
            next.insert("ack", result.map_or(0, |result| kind(result.outcome)));
            next.insert("which", which);
        }
        "ObserveA" | "ObserveB" => {
            let result = witnessed(
                &mut evidence,
                if which == 1 { "ObserveA" } else { "ObserveB" },
                // A malformed request is refused rather than observed. The
                // model records no positive observation in either case; this
                // does not turn an API error into evidence of absence.
                || account::observe_at(&w.paths, relation).ok().flatten(),
            );
            let observed = if fault == 7 && result.is_none() {
                // A real positive parent closure is not evidence of a child claim.
                assert!(observe_at(&w.paths, &w.binding).unwrap().is_some());
                2
            } else {
                result.map_or(0, |result| kind(result.outcome))
            };
            next.insert("observed", observed);
            next.insert("which", which);
        }
        "LegacyA" => {
            let c = candidate(&w, 1, 0);
            witnessed(&mut evidence, "LegacyA", || {
                record_successor_at(
                    &w.paths,
                    &w.parent,
                    &w.binding,
                    &c,
                    &candidate_binding(&c),
                    cap(state),
                )
                .unwrap()
            });
        }
        "May" | "NoExec" | "Seal" => {
            let c = candidate(&w, state["winner"], 0);
            let binding = candidate_binding(&c);
            let event = match action {
                "May" => Event::MayExecute,
                "NoExec" => Event::NoExec,
                _ => Event::Seal,
            };
            let name = match action {
                "May" => "May",
                "NoExec" => "NoExec",
                _ => "Seal",
            };
            witnessed(&mut evidence, name, || {
                record_with_phase_at_limits(
                    &w.paths,
                    &c,
                    &binding,
                    Some(event),
                    (action == "NoExec").then_some(ImagePhase::Forward),
                    cap(state),
                )
                .unwrap()
            });
        }
        _ => panic!("unknown account journal action {action}"),
    }
    project(&w, next)
}

#[test]
fn account_journal_refines_real_claim_fence_and_retirement_transitions() {
    let model = aterm_models_harness::account_journal_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    assert_eq!(ACCOUNT_FRAME, 520);
    assert_eq!(FENCE_FRAME, 524);
    let mut evidence = StepEvidence::new("AccountJournal");
    let mut cache = std::collections::BTreeMap::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            // Every action overwrites these output-only fields. Exercise each
            // storage/input equivalence class once, then compare every model edge.
            let mut inputs = state.clone();
            for name in ["ack", "observed", "which"] {
                inputs.insert(name, 0);
            }
            let next = cache
                .entry((inputs, action.name))
                .or_insert_with(|| step(&state, action.name, 0, Some(&mut evidence)));
            assert!(
                model.successors(action.name, &state).contains(next),
                "{}: {state:?} -> {next:?}",
                action.name
            );
        }
    }
    let audit = evidence.audit();
    assert!(audit.is_complete(), "{audit:?}");
}

#[test]
fn ignoring_request_evidence_or_writing_on_replay_breaks_real_conformance() {
    let model = aterm_models_harness::account_journal_model();
    let mut wrong_phase = model.init_state();
    wrong_phase.insert("phase", 1);
    let accepted = step(&wrong_phase, "RefuseA", 1, None);
    assert_eq!(accepted["a"], 2);
    assert!(!model.check_invariant("AcknowledgedExact", &accepted));

    let mut wrong_binding = model.init_state();
    wrong_binding.insert("binding", 0);
    let accepted = step(&wrong_binding, "RefuseA", 6, None);
    assert!(!model.check_invariant("AcknowledgedExact", &accepted));

    let invented = step(&model.init_state(), "ObserveA", 7, None);
    assert!(!model.check_invariant("ObservationExact", &invented));
    let claimed = transition(&model.init_state(), "ClaimA");
    let wrote = step(&claimed, "ReplayA", 9, None);
    assert!(!model.check_invariant("CountedRecords", &wrote));
}
