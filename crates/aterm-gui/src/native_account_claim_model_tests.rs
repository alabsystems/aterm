// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::xref::StepEvidence;

fn selected(w: &World, choice: i64) -> Prepare {
    candidate(
        w,
        if choice == 1 { "a7" } else { "b7" },
        ImagePhase::Forward,
    )
}

fn proof(w: &World) -> Arc<ClosedRestart> {
    lookup(&w.handle.ctx, &w.binding)
        .unwrap()
        .lock()
        .unwrap()
        .closed_restart
        .clone()
        .unwrap()
}

fn project(w: &World, mut state: State) -> State {
    let proof = proof(w);
    let (pending, committed, retired) = proof.account_state();
    let choice = |prepare: Option<Prepare>| {
        prepare.map_or(
            0,
            |prepare| {
                if prepare.id == "a7".repeat(16) { 1 } else { 2 }
            },
        )
    };
    state.insert("pending", choice(pending));
    state.insert("committed", choice(committed));
    state.insert("retired", i64::from(retired));
    if state["chosen"] != 0 {
        let candidate = selected(w, state["chosen"]);
        state.insert(
            "disk",
            if w.facts.journal.lock().unwrap().contains_key(&candidate.id) {
                state["chosen"]
            } else {
                0
            },
        );
        state.insert(
            "terminal",
            i64::from(
                w.facts
                    .journal
                    .lock()
                    .unwrap()
                    .get(&candidate.id)
                    .is_some_and(|entry| entry.state != JournalState::Prepared),
            ),
        );
        state.insert("live", i64::from(proof.claimed_by(&candidate)));
    }
    state
}

fn restore(state: &State) -> World {
    let w = world(8495, ImagePhase::Forward);
    if state["chosen"] != 0 {
        let candidate = selected(&w, state["chosen"]);
        w.facts
            .prepare_journal_allowed
            .store(state["disk"] != 0, Ordering::SeqCst);
        w.facts.prepare_journal_ack.store(false, Ordering::SeqCst);
        assert_eq!(admit(&w, candidate.clone(), ImagePhase::Forward), UNKNOWN);
        if state["terminal"] == 1 {
            let binding = w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&candidate.id]
                .lock()
                .unwrap()
                .binding
                .clone();
            assert!(
                w.facts
                    .record_noexec(&candidate, &binding, ImagePhase::Forward)
            );
        }
        if state["retired"] == 1 {
            proof(&w).retire();
        }
        if state["committed"] != 0 {
            w.facts
                .prepare_journal_allowed
                .store(true, Ordering::SeqCst);
            w.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
            let reply = admit(&w, candidate, ImagePhase::Forward);
            assert_eq!(
                i64::from(matches!(Reply::parse(&reply), Some(Reply::Admitted(_)))),
                state["admitted"]
            );
        }
    }
    assert_eq!(project(&w, state.clone()), *state);
    w
}

fn step(state: &State, action: &str, evidence: Option<&mut StepEvidence>) -> State {
    let w = restore(state);
    let mut next = state.clone();
    let run = || match action {
        "ReserveA" | "ReserveB" => {
            let choice = if action == "ReserveA" { 1 } else { 2 };
            w.facts
                .prepare_journal_allowed
                .store(false, Ordering::SeqCst);
            assert_eq!(
                admit(&w, selected(&w, choice), ImagePhase::Forward),
                UNKNOWN
            );
            next.insert("chosen", choice);
        }
        "Persist" | "Confirm" => {
            w.facts
                .prepare_journal_allowed
                .store(true, Ordering::SeqCst);
            w.facts
                .prepare_journal_ack
                .store(action == "Confirm", Ordering::SeqCst);
            let reply = admit(&w, selected(&w, state["chosen"]), ImagePhase::Forward);
            if action == "Confirm" {
                let candidate = selected(&w, state["chosen"]);
                next.insert(
                    "acknowledged",
                    i64::from(
                        w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&candidate.id]
                            .lock()
                            .unwrap()
                            .preparation
                            .recorded,
                    ),
                );
                next.insert(
                    "admitted",
                    i64::from(matches!(Reply::parse(&reply), Some(Reply::Admitted(_)))),
                );
            } else {
                assert_eq!(reply, UNKNOWN);
            }
        }
        "Retire" => {
            proof(&w).retire();
            next.insert("retired_unwritten", i64::from(state["disk"] == 0));
        }
        "CloseOnDisk" => {
            let candidate = selected(&w, state["chosen"]);
            let binding = w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&candidate.id]
                .lock()
                .unwrap()
                .binding
                .clone();
            assert!(
                w.facts
                    .record_noexec(&candidate, &binding, ImagePhase::Forward)
            );
        }
        "Rival" => {
            next.insert("rival", 1);
            let rival = selected(&w, if state["chosen"] == 1 { 2 } else { 1 });
            next.insert("other", i64::from(proof(&w).claim_successor(&rival)));
        }
        _ => unreachable!(),
    };
    if let Some(evidence) = evidence.filter(|_| action != "CloseOnDisk") {
        let anchor = match action {
            "ReserveA" => "ReserveA",
            "ReserveB" => "ReserveB",
            "Persist" => "Persist",
            "Confirm" => "Confirm",
            "Retire" => "Retire",
            "Rival" => "Rival",
            _ => unreachable!(),
        };
        evidence.step(anchor, run);
    } else {
        let mut run = run;
        run();
    }
    project(&w, next)
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    step(state, action, None)
}

#[test]
fn pending_claim_refines_actual_admission_and_journal_acknowledgement() {
    let model = aterm_models_harness::account_claim_publication_model();
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    let mut evidence = StepEvidence::new("AccountClaimPublication");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = step(&state, action.name, Some(&mut evidence));
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    assert!(evidence.audit().is_complete(), "{:?}", evidence.audit());
    let reserved = transition(&model.init_state(), "ReserveA");
    let w = restore(&reserved);
    // This real latch call deliberately skips the journal acknowledgement.
    // Moving it before record_account_claim would break the production law.
    assert!(proof(&w).confirm_account_successor(&selected(&w, 1)));
    assert!(!model.check_invariant("NoEarlyClaim", &project(&w, reserved)));
}

fn noeffect_step(state: &State, action: &str, evidence: Option<&mut StepEvidence>) -> State {
    let w = world(8560, ImagePhase::Forward);
    let next = selected(&w, 1);
    let binding = admitted_binding(&admit(&w, next.clone(), ImagePhase::Forward));
    let operation = lookup(&w.handle.ctx, &binding).unwrap();
    let NativePlan::Restart { process, .. } = &next.plan else {
        unreachable!()
    };
    if state["quiet"] == 0 {
        w.facts
            .processes
            .lock()
            .unwrap()
            .insert(process.pid, process.clone());
    }
    if state["closed"] == 1 {
        assert!(
            operation
                .lock()
                .unwrap()
                .seal_without_effect(&w.facts)
                .is_some()
        );
    }
    let mut result = state.clone();
    match action {
        "CohortGone" => {
            w.facts.processes.lock().unwrap().remove(&process.pid);
            result.insert("quiet", 1);
            result.insert("checked", 0);
        }
        "Close" => {
            let close = || operation.lock().unwrap().seal_without_effect(&w.facts);
            let completion = if let Some(evidence) = evidence {
                evidence.step("Close", close)
            } else {
                close()
            };
            result.insert("checked", 1);
            result.insert("closed", i64::from(completion.is_some()));
        }
        _ => unreachable!(),
    }
    result
}

pub(crate) fn noeffect_transition(state: &State, action: &str) -> State {
    noeffect_step(state, action, None)
}

#[test]
fn account_noeffect_closure_requires_retained_cohort_evidence() {
    let model = aterm_models_harness::account_noeffect_closure_model();
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    let mut evidence = StepEvidence::new("AccountNoEffectClosure");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&noeffect_step(&state, action.name, Some(&mut evidence)))
                );
            }
        }
    }
    assert!(evidence.audit().is_complete());
    let w = world(8561, ImagePhase::Forward);
    let next = selected(&w, 1);
    let binding = admitted_binding(&admit(&w, next.clone(), ImagePhase::Forward));
    let NativePlan::Restart { process, .. } = next.plan else {
        unreachable!()
    };
    w.facts
        .processes
        .lock()
        .unwrap()
        .insert(process.pid, process);
    let mut unchecked = noeffect_transition(&model.init_state(), "Close");
    // This is the real old fallback. It can close without consulting the cohort.
    let completion = lookup(&w.handle.ctx, &binding)
        .unwrap()
        .lock()
        .unwrap()
        .seal_completion(Decision::ProvenNotCommittedRollbackSafe);
    unchecked.insert("closed", i64::from(completion.is_some()));
    assert!(!model.check_invariant("NoUnprovenClosure", &unchecked));
}
