// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(super) fn fixture(
    local: u64,
) -> (
    crate::session_store::SessionHandle,
    Facts,
    Prepare,
    SupervisorInputCaller,
) {
    fixture_on(crate::session_store::test_handle_on(local, -1))
}

pub(super) fn fixture_on(
    handle: crate::session_store::SessionHandle,
) -> (
    crate::session_store::SessionHandle,
    Facts,
    Prepare,
    SupervisorInputCaller,
) {
    let ctx = &handle.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let source = process(4242, 400, 4242, 400, 2);
    let shell = process(400, 1, 400, 400, 4);
    facts
        .processes
        .lock()
        .unwrap()
        .insert(source.pid, source.clone());
    facts
        .processes
        .lock()
        .unwrap()
        .insert(shell.pid, shell.clone());
    let mut request = plan(ctx, &facts);
    request.holder = crate::harness_host::holder();
    request.expected_human_seq = Some(ctx.human_input.seq());
    request.duty = Scope::Relaunch;
    request.plan = NativePlan::Restart {
        process: source,
        shell,
        observation: None,
        launcher: image(20),
        socket: "/fake-native.sock".into(),
        forward: Box::new(restart_command(9)),
        rollback: Some(Box::new(restart_command(2))),
    };
    let actor = SupervisorInputCaller {
        principal: Principal::Harness,
        connection: Some(8),
        peer: Some(900),
        proof: None,
    };
    (handle, facts, request, actor)
}

fn operation(ctx: &crate::SessionCtx, id: &str) -> Arc<Mutex<Operation>> {
    ctx.fenced_writes
        .native
        .lock()
        .unwrap()
        .by_id
        .get(id)
        .unwrap()
        .clone()
}

#[test]
fn restart_watch_precedes_signal_and_closed_proof_survives_completion() {
    let (handle, facts, request, actor) = fixture(7981);
    let original = request.clone();
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request),
        actor.clone(),
    ));
    assert_eq!(facts.watch_count.load(Ordering::SeqCst), 1);
    assert_eq!(facts.watched_after_signals.load(Ordering::SeqCst), 0);
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
    assert_eq!(
        run(&handle.ctx, &facts, Request::Reconcile(binding), actor),
        "OK native-act state=retired reason=no-exec-grant\n"
    );
    let operation = operation(&handle.ctx, &original.id);
    let proof = operation
        .lock()
        .unwrap()
        .closed_restart
        .clone()
        .expect("retained kernel proof");
    assert_eq!(proof.predecessor, original);
    assert!(proof.quiescent(&facts));
    assert!(!proof.cancelled_without_successor(&original, &facts));
    proof.retire();
    assert!(proof.cancelled_without_successor(&original, &facts));
    let mut other = original.clone();
    other.id = "ef".repeat(16);
    assert!(!proof.cancelled_without_successor(&other, &facts));
    assert_eq!(
        facts.watch_count.load(Ordering::SeqCst),
        1,
        "no watch opened after death"
    );
    // Reintroducing any process in the exact named cohort invalidates quiescence.
    facts
        .processes
        .lock()
        .unwrap()
        .insert(7000, process(7000, 400, 4242, 400, 90));
    assert!(!proof.quiescent(&facts));
    assert!(!proof.cancelled_without_successor(&original, &facts));
}

#[test]
fn restart_watch_missing_or_identity_changed_during_arm_never_admits_signal() {
    for (local, changed) in [(7982, false), (7983, true)] {
        let (handle, facts, request, actor) = fixture(local);
        facts.watch_available.store(changed, Ordering::SeqCst);
        facts.change_birth_on_watch.store(changed, Ordering::SeqCst);
        let reply = run(&handle.ctx, &facts, Request::prepare(request), actor);
        assert_eq!(reply, if changed { DENIED } else { UNKNOWN });
        assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
        assert!(
            handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .by_id
                .is_empty()
        );
    }
}

#[test]
fn restart_unknown_retained_watch_cannot_turn_pid_absence_into_closed_proof() {
    let (handle, facts, request, actor) = fixture(7984);
    facts.watch_unknown.store(true, Ordering::SeqCst);
    let id = request.id.clone();
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request),
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
    assert_eq!(
        run(&handle.ctx, &facts, Request::Reconcile(binding), actor),
        UNKNOWN
    );
    let operation = operation(&handle.ctx, &id);
    let op = operation.lock().unwrap();
    assert_eq!(op.phase, Phase::Started);
    assert!(op.closed_restart.is_none());
}

#[test]
fn restart_missing_retained_kernel_object_cannot_create_continuation_evidence() {
    let (handle, facts, request, actor) = fixture(7985);
    let id = request.id.clone();
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request),
        actor.clone(),
    ));
    let operation = operation(&handle.ctx, &id);
    // Model a cold host or lost retained object, not an excuse to open a watch
    // against a dead PID. Disposal occurs outside the operation predicate.
    let removed = operation.lock().unwrap().source_death.take();
    drop(removed);
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::Signal(binding.clone()),
            actor.clone()
        ),
        DENIED
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    assert_eq!(
        run(&handle.ctx, &facts, Request::Reconcile(binding), actor),
        "OK native-act state=retired reason=no-exec-grant\n"
    );
    assert!(operation.lock().unwrap().closed_restart.is_none());
    assert_eq!(facts.watch_count.load(Ordering::SeqCst), 1);
}
