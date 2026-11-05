// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn phase(value: Option<ImagePhase>) -> i64 {
    i64::from(value == Some(ImagePhase::Rollback))
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let (handle, facts, request, actor) = closed_restart::fixture(8240);
    let binding = admitted_binding(&run(&handle.ctx, &facts, Request::prepare(request), actor));
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    let operation = lookup(&handle.ctx, &binding).unwrap();
    let mut op = operation.lock().unwrap();
    let initial = match action {
        "Admit" => state["captured"],
        "RetireChild" => state["child"],
        _ => state["active"],
    };
    op.resume_phase = (initial == 1).then_some(ImagePhase::Rollback);
    let mut next = state.clone();
    let completion = match action {
        "StartRollback" => {
            op.resume_phase = Some(ImagePhase::Rollback);
            next.insert("active", phase(op.resume_phase));
            None
        }
        "Capture" => {
            let proof = ClosedRestart::capture(&op, Retirement::NotGranted, &facts).unwrap();
            next.insert("captured", phase(proof.initial_resume_phase()));
            next.insert("closed", 1);
            None
        }
        "Admit" => {
            let proof = ClosedRestart::capture(&op, Retirement::NotGranted, &facts).unwrap();
            op.resume_phase = proof.initial_resume_phase();
            next.insert("child", phase(op.resume_phase));
            next.insert("admitted", 1);
            None
        }
        "RetireChild" => {
            let completion = op.seal_retirement(Retirement::NotGranted, &facts).unwrap();
            next.insert(
                "active",
                phase(op.closed_restart.as_ref().unwrap().initial_resume_phase()),
            );
            next.insert("closed", 0);
            next.insert("admitted", 0);
            next.insert("round", 1);
            Some(completion)
        }
        _ => unreachable!("unmapped continuation phase action"),
    };
    drop(op);
    drop(completion);
    next
}

#[test]
fn continuation_phase_refines_capture_and_inheritance() {
    let model = aterm_models_harness::continuation_phase_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn defaulting_the_child_to_forward_breaks_phase_inheritance() {
    let model = aterm_models_harness::continuation_phase_model();
    let mut state = model.init_state();
    assert!(model.fire("StartRollback", &mut state));
    state = transition(&state, "Capture");
    // The previous initializer unconditionally set a child to resume_phase=None.
    let mut bad = state.clone();
    bad.insert("admitted", 1);
    bad.insert("child", phase(None));
    assert!(!model.check_invariant("AdmissionPreservesPhase", &bad));
    assert!(!model.successors("Admit", &state).contains(&bad));
}
