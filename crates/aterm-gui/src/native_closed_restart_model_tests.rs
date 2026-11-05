// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn winner(proof: &ClosedRestart, a: &Prepare, b: &Prepare) -> i64 {
    match proof.projected_state().1 {
        None => 0,
        Some(ref found) if found == a => 1,
        Some(ref found) if found == b => 2,
        Some(_) => panic!("successor differs from both exact candidate recipes"),
    }
}

/// Project the actual retained-watch capture and atomic successor slot. The
/// fixture changes injected kernel facts; no missing PID is treated as a watch.
pub(crate) fn transition(state: &State, action: &str) -> State {
    let (handle, facts, request, actor) = closed_restart::fixture(7986);
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request.clone()),
        actor,
    ));
    let operation = lookup(&handle.ctx, &binding).unwrap();
    let mut removed_watch = None;
    let mut op = operation.lock().unwrap();
    let attempt = Arc::new(NativeInput::new(
        &handle.ctx,
        Arc::clone(op.custody.as_ref().unwrap()),
        op.human_seq,
        op.authority.snapshot.clone(),
        op.request.deadline_us,
        Arc::clone(&op.effect_started),
        op.authority.conversation.clone(),
        op.clock.clone(),
    ));
    if state["settled"] == 1 {
        attempt.seal();
    }
    op.resume_attempt = Some(Arc::clone(&attempt));
    if state["watch"] == 0 {
        // Remove the retained kernel object, never replace it with a new PID lookup.
        removed_watch = op.source_death.take();
    }
    if state["dead"] == 1 || state["group"] == 1 {
        facts.processes.lock().unwrap().remove(&4242);
        // Cohort absence and a retained death notification are independent
        // observations. An unresolved watch cannot be replaced by the former.
        if state["dead"] == 0 && state["watch"] == 1 {
            removed_watch = op.source_death.replace(Arc::new(SharedDeath {
                watch: DeathWatch::Unknown,
                outcome: std::sync::OnceLock::new(),
            }));
        }
        if state["group"] == 0 {
            facts
                .processes
                .lock()
                .unwrap()
                .insert(7000, process(7000, 400, 4242, 400, 90));
        }
    }
    if state["granted"] == 1 {
        let child = process(600, 400, 600, 400, 20);
        if state["child_group"] == 0 {
            let live = if state["child_dead"] == 0 {
                child.clone()
            } else {
                process(6001, 400, 600, 400, 90)
            };
            facts.processes.lock().unwrap().insert(live.pid, live);
        }
        op.children[0] = Some(LaunchChild {
            death: Arc::new(SharedDeath {
                watch: if state["child_dead"] == 0 && state["child_group"] == 1 {
                    DeathWatch::Unknown
                } else {
                    DeathWatch::Facts(child.clone(), Arc::clone(&facts.processes))
                },
                outcome: std::sync::OnceLock::new(),
            }),
            process: child,
            connection: 80,
            execute_sent: true,
            exec_errno: None,
        });
    }
    let mut proof = (state["closed"] == 1).then(|| {
        ClosedRestart::capture(
            &op,
            if state["kind"] == 1 {
                Retirement::NotGranted
            } else {
                Retirement::Exited
            },
            &facts,
        )
        .unwrap()
    });
    let mut a = request.clone();
    a.id = "cd".repeat(16);
    let mut b = request.clone();
    b.id = "ef".repeat(16);
    if let Some(proof) = &proof {
        if state["winner"] > 0 {
            assert!(proof.claim_successor(if state["winner"] == 1 { &a } else { &b }));
        }
        if state["retired"] == 1 {
            proof.retire();
        }
    }
    let mut next = state.clone();
    match action {
        "LoseWatch" => {
            next.insert("watch", 0);
        }
        "SourceDies" => {
            next.insert("dead", 1);
        }
        "GroupDrains" => {
            next.insert("group", 1);
        }
        "InputSettles" => {
            next.insert("settled", 1);
        }
        "IssueExec" => {
            next.insert("granted", 1);
        }
        "ChildDies" => {
            next.insert("child_dead", 1);
        }
        "ChildGroupDrains" => {
            next.insert("child_group", 1);
        }
        "Capture" | "CaptureExited" => {
            let kind = if action == "Capture" { 1 } else { 2 };
            next.insert("kind", kind);
            proof = ClosedRestart::capture(
                &op,
                if kind == 1 {
                    Retirement::NotGranted
                } else {
                    Retirement::Exited
                },
                &facts,
            );
        }
        "ClaimA" | "ClaimB" => {
            let proof = proof.as_ref().unwrap();
            let winner = if action == "ClaimA" { 1 } else { 2 };
            let admitted = proof.claim_successor(if winner == 1 { &a } else { &b });
            next.insert("claims", state["claims"] + i64::from(admitted));
            if admitted {
                next.insert("first_winner", winner);
            }
        }
        "Retry" => {
            let proof = proof.as_ref().unwrap();
            // Both a replay and a competing operation must use the existing slot.
            assert!(!proof.claim_successor(&a));
            assert!(!proof.claim_successor(&b));
            next.insert("retried", 1);
        }
        "Retire" => {
            let before = winner(proof.as_ref().unwrap(), &a, &b);
            proof.as_ref().unwrap().retire();
            assert!(!proof.as_ref().unwrap().available());
            next.insert("at_retire", before);
        }
        _ => unreachable!(),
    }
    next.insert("closed", i64::from(proof.is_some()));
    if let Some(proof) = &proof {
        let (retired, _, no_exec) = proof.projected_state();
        next.insert("winner", winner(proof, &a, &b));
        next.insert("retired", i64::from(retired));
        next.insert("kind", if no_exec { 1 } else { 2 });
    }
    drop(op);
    drop(removed_watch);
    // Close the synthetic input producer before releasing its retained custody.
    attempt.seal();
    next
}

#[test]
fn retained_restart_evidence_refines_capture_and_single_successor_model() {
    let model = aterm_models_harness::closed_restart_model();
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
fn retained_restart_model_catches_reminting_the_real_successor_slot() {
    let model = aterm_models_harness::closed_restart_model();
    let (handle, facts, request, actor) = closed_restart::fixture(7987);
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request.clone()),
        actor,
    ));
    facts.processes.lock().unwrap().remove(&4242);
    let operation = lookup(&handle.ctx, &binding).unwrap();
    let op = operation.lock().unwrap();
    let first = ClosedRestart::capture(&op, Retirement::NotGranted, &facts).unwrap();
    // Deliberate negative implementation: reconstruct evidence for each request
    // instead of consuming the one object retained by the terminal operation.
    let reminted = ClosedRestart::capture(&op, Retirement::NotGranted, &facts).unwrap();
    let mut a = request.clone();
    a.id = "cd".repeat(16);
    let mut b = request;
    b.id = "ef".repeat(16);
    let claims = i64::from(first.claim_successor(&a)) + i64::from(reminted.claim_successor(&b));
    assert_eq!(claims, 2);
    let mut bad = model.init_state();
    bad.insert("claims", claims);
    assert!(!model.check_invariant("OneSuccessor", &bad));
}

#[test]
fn retired_operation_reuses_its_one_proof_and_preserves_the_claimed_recipe() {
    let (handle, facts, request, actor) = closed_restart::fixture(7988);
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request.clone()),
        actor.clone(),
    ));
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::Signal(binding.clone()),
            actor.clone()
        ),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    let retired = "OK native-act state=retired reason=no-exec-grant\n";
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            actor.clone()
        ),
        retired
    );
    let operation = lookup(&handle.ctx, &binding).unwrap();
    let proof = operation.lock().unwrap().closed_restart.clone().unwrap();
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            actor.clone()
        ),
        retired
    );
    let op = operation.lock().unwrap();
    assert!(Arc::ptr_eq(&proof, op.closed_restart.as_ref().unwrap()));
    drop(op);
    let mut a = request.clone();
    a.id = "cd".repeat(16);
    let mut b = request;
    b.id = "ef".repeat(16);
    assert!(proof.claim_successor(&a));
    assert_eq!(winner(&proof, &a, &b), 1);
    assert_eq!(
        run(&handle.ctx, &facts, Request::Reconcile(binding), actor),
        retired
    );
    let op = operation.lock().unwrap();
    assert!(Arc::ptr_eq(&proof, op.closed_restart.as_ref().unwrap()));
    drop(op);
    assert_eq!(winner(&proof, &a, &b), 1);
    assert!(!proof.claim_successor(&b));
    assert!(!proof.claim_successor(&a));
    assert_eq!(winner(&proof, &a, &b), 1);
    proof.retire();
    assert!(proof.projected_state().0);
    assert!(!proof.claim_successor(&b));
    assert_eq!(winner(&proof, &a, &b), 1);
}

#[test]
fn attached_ungranted_helper_keeps_the_no_exec_classification() {
    let (handle, facts, request, actor) = closed_restart::fixture(7989);
    let binding = admitted_binding(&run(&handle.ctx, &facts, Request::prepare(request), actor));
    facts.processes.lock().unwrap().remove(&4242);
    let child = process(600, 400, 600, 400, 20);
    facts.processes.lock().unwrap().insert(600, child.clone());
    let operation = lookup(&handle.ctx, &binding).unwrap();
    let mut op = operation.lock().unwrap();
    op.children[1] = Some(LaunchChild {
        process: child.clone(),
        death: Arc::new(SharedDeath {
            watch: DeathWatch::Unknown,
            outcome: std::sync::OnceLock::new(),
        }),
        connection: 80,
        execute_sent: false,
        exec_errno: None,
    });
    let unissued = ClosedRestart::capture(&op, Retirement::NotGranted, &facts).unwrap();
    assert!(unissued.projected_state().2);
    assert!(ClosedRestart::capture(&op, Retirement::Exited, &facts).is_none());
    op.children[1].as_mut().unwrap().execute_sent = true;
    assert!(ClosedRestart::capture(&op, Retirement::NotGranted, &facts).is_none());
    assert!(ClosedRestart::capture(&op, Retirement::Exited, &facts).is_none());
    op.children[1].as_mut().unwrap().death = Arc::new(SharedDeath {
        watch: DeathWatch::Facts(child, Arc::clone(&facts.processes)),
        outcome: std::sync::OnceLock::new(),
    });
    assert!(ClosedRestart::capture(&op, Retirement::Exited, &facts).is_none());
    facts.processes.lock().unwrap().remove(&600);
    let issued = ClosedRestart::capture(&op, Retirement::Exited, &facts).unwrap();
    assert!(!issued.projected_state().2);
}
