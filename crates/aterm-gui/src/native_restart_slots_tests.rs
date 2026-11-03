// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control_input_ack::{InputMark, testing};
use crate::input::{Delivery, EgressReceipt};
use std::sync::mpsc;

#[test]
fn no_go_forward_closes_only_after_prefix_and_lost_rollback_go_retains_watch() {
    let model = aterm_models_harness::native_restart_slots_model();
    let mut state = model.init_state();
    let (handle, mut reader) = testing::owned_handle(7950);
    let ctx = &handle.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let old = process(4242, 400, 4242, 400, 2);
    let shell = process(400, 1, 400, 400, 4);
    facts.processes.lock().unwrap().insert(old.pid, old.clone());
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
        observation: None,
        process: old,
        shell,
        launcher: image(20),
        socket: "/fake-native.sock".into(),
        forward: Box::new(restart_command(9)),
        rollback: Some(Box::new(restart_command(2))),
    };
    let deadline = request.deadline_us;
    let builtin = SupervisorInputCaller {
        principal: Principal::Harness,
        connection: Some(8),
        peer: Some(900),
        proof: None,
    };
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(request),
        builtin.clone(),
    ));
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Signal(binding.clone()),
            builtin.clone()
        ),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    let (pending_tx, pending_rx) = mpsc::sync_channel(1);
    let forward = serve_by(
        ctx,
        -1,
        Request::Resume(binding.clone(), ImagePhase::Forward, "1.1".into(), 0),
        builtin.clone(),
        &facts,
        &mut |_| {
            let mark = InputMark::at_epoch(
                ctx,
                handle.local_id,
                aterm_slog::Actor::Ctl,
                crate::session_log::InputVerb::Turn,
                None,
                true,
            );
            let producer = mark.native_producer();
            let completion = mark.completion();
            assert!(completion.start_supervised());
            let receipt = ctx.sink.write_frame_with_prefix(b"R").unwrap();
            assert!(
                ctx.sink
                    .wait_prefix_to_kernel(receipt.kernel_prefix().unwrap())
            );
            pending_tx.send((completion, receipt.order())).unwrap();
            drop(producer);
            "OK typed=1\n".into()
        },
    );
    assert_eq!(forward, "OK native-act state=resumed phase=forward\n");
    assert!(model.fire("ForwardQueued", &mut state));
    assert_eq!(testing::byte(&mut reader), b'R');
    let rollback_request =
        || Request::Resume(binding.clone(), ImagePhase::Rollback, "1.2".into(), 0);
    assert_eq!(
        run(ctx, &facts, rollback_request(), builtin.clone()),
        UNKNOWN
    );
    assert!(model.fire("PendingClose", &mut state));
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            builtin.clone()
        ),
        UNKNOWN
    );
    assert!(
        lookup(ctx, &binding)
            .unwrap()
            .lock()
            .unwrap()
            .closed_restart
            .is_none(),
        "an unsettled native input cannot produce continuation evidence"
    );
    let (completion, order) = pending_rx.recv_timeout(PATIENCE).unwrap();
    completion.finish(EgressReceipt::from_reported_delivery(Delivery::Full, order));
    assert!(model.fire("ForwardDrain", &mut state));
    let rollback = serve_by(
        ctx,
        -1,
        rollback_request(),
        builtin.clone(),
        &facts,
        &mut |body| {
            assert!(body.contains("phase=rollback"));
            let mark = InputMark::at_epoch(
                ctx,
                handle.local_id,
                aterm_slog::Actor::Ctl,
                crate::session_log::InputVerb::Turn,
                None,
                true,
            );
            let producer = mark.native_producer();
            let completion = mark.completion();
            assert!(completion.start_supervised());
            let receipt = ctx.sink.write_frame_with_prefix(b"B").unwrap();
            assert!(
                ctx.sink
                    .wait_prefix_to_kernel(receipt.kernel_prefix().unwrap())
            );
            completion.finish(EgressReceipt::from_reported_delivery(
                Delivery::Full,
                receipt.order(),
            ));
            drop(producer);
            "OK typed=1\n".into()
        },
    );
    assert_eq!(rollback, "OK native-act state=resumed phase=rollback\n");
    assert_eq!(testing::byte(&mut reader), b'B');
    assert!(model.fire("CloseNoGo", &mut state));
    let launcher = SupervisorInputCaller {
        principal: Principal::Instance,
        connection: Some(80),
        peer: Some(600),
        proof: None,
    };
    facts
        .processes
        .lock()
        .unwrap()
        .insert(600, process(600, 400, 600, 400, 20));
    facts.foreground.store(600, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Attach(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        ),
        DENIED
    );
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        ),
        DENIED
    );
    assert!(model.fire("LateForwardGo", &mut state));
    assert!(model.fire("FixedRollback", &mut state));
    let operation = lookup(ctx, &binding).unwrap();
    assert_eq!(operation.lock().unwrap().request.deadline_us, deadline);
    assert!(model.fire("RenewBudget", &mut state));
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Attach(binding.clone(), ImagePhase::Rollback),
            launcher.clone()
        )),
        Some(Reply::Launch { .. })
    ));
    // Deliberately ignore the Go ACK: its registered child is still known and watched.
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Rollback),
            launcher.clone()
        )),
        Some(Reply::Execute { .. })
    ));
    assert!(
        operation.lock().unwrap().children[1]
            .as_ref()
            .unwrap()
            .execute_sent
    );
    assert!(model.fire("UnknownGo", &mut state));
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            builtin.clone()
        ),
        UNKNOWN
    );
    assert!(model.fire("EarlyRetirement", &mut state));
    assert!(
        operation.lock().unwrap().closed_restart.is_none(),
        "a live Go-granted child never becomes a closed predecessor"
    );
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::ExecFailed(binding.clone(), ImagePhase::Rollback, libc::EACCES),
            launcher
        ),
        "OK native-act state=exec-failed\n"
    );
    facts.processes.lock().unwrap().remove(&600);
    facts.foreground.store(400, Ordering::SeqCst);
    assert!(model.fire("ChildDeath", &mut state));
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            builtin.clone()
        ),
        "OK native-act state=retired reason=cohort-exited\n"
    );
    assert!(model.fire("Retire", &mut state));
    let closed = operation
        .lock()
        .unwrap()
        .closed_restart
        .clone()
        .expect("retained source and granted-child deaths");
    assert!(closed.quiescent(&facts));
    facts
        .processes
        .lock()
        .unwrap()
        .insert(7001, process(7001, 400, 600, 400, 91));
    assert!(
        !closed.quiescent(&facts),
        "the named granted-child cohort must remain quiescent"
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 1);
    for law in &model.invariants {
        assert!(model.check_invariant(law.name, &state), "{}", law.name);
    }
}

#[test]
fn restart_slot_negative_controls_break_every_law() {
    let base = aterm_models_harness::native_restart_slots_model();
    for (bug, law) in [
        (1, "RollbackWaitsPrefix"),
        (2, "ClosedSlotNeverGrantsAgain"),
        (3, "OriginalBudgetIsImmutable"),
        (4, "LostGoRetainsWatch"),
        (5, "GrantedChildRetiresOnlyAfterDeath"),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&base, bug);
        let mut state = mutant.init_state();
        let mut caught = false;
        for action in [
            "ForwardQueued",
            "PendingClose",
            "ForwardDrain",
            "CloseNoGo",
            "LateForwardGo",
            "FixedRollback",
            "RenewBudget",
            "UnknownGo",
            "EarlyRetirement",
            "ChildDeath",
            "Retire",
        ] {
            assert!(mutant.fire(action, &mut state));
            caught |= !base.check_invariant(law, &state);
        }
        assert!(caught, "negative control {bug}: {law}");
    }
}

#[test]
fn unexpired_terminal_receipts_survive_pressure_and_expired_prepare_never_recreates() {
    let handle = crate::session_store::test_handle_on(7951, -1);
    let ctx = &handle.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let deadline = facts.clock.load(Ordering::SeqCst) + 60_000_000;
    let mut first = None;
    for index in 0..MAX_COMPLETED {
        facts
            .processes
            .lock()
            .unwrap()
            .get_mut(&4242)
            .unwrap()
            .image = image(2);
        let mut request = plan(ctx, &facts);
        request.id = format!("{index:032x}");
        request.deadline_us = deadline;
        if index == 0 {
            first = Some(request.clone());
        }
        let binding = admitted_binding(&run(ctx, &facts, Request::prepare(request), caller()));
        facts.enter_target();
        assert_eq!(
            run(
                ctx,
                &facts,
                Request::Precommit(binding.clone(), ImagePhase::Forward),
                caller()
            ),
            "OK native-act state=precommitted\n"
        );
        assert!(matches!(
            Reply::parse(&run(
                ctx,
                &facts,
                Request::Commit(binding, ImagePhase::Forward, "0.1.0+g123".into()),
                caller()
            )),
            Some(Reply::Committed(_))
        ));
    }
    facts
        .processes
        .lock()
        .unwrap()
        .get_mut(&4242)
        .unwrap()
        .image = image(2);
    let mut extra = plan(ctx, &facts);
    extra.id = format!("{:032x}", MAX_COMPLETED);
    extra.deadline_us = deadline;
    assert_eq!(
        run(ctx, &facts, Request::prepare(extra.clone()), caller()),
        "ERR busy native operation history\n"
    );
    {
        let table = ctx.fenced_writes.native.lock().unwrap();
        assert_eq!(table.by_id.len(), MAX_COMPLETED);
        assert_eq!(table.completed.len(), MAX_COMPLETED);
        assert!(table.reservations.is_empty());
    }
    assert!(
        FencedWrite::enter_fence(
            ctx,
            &ticket(ctx, true, Scope::Approvals),
            crate::metrics::now_us()
        )
        .is_ok(),
        "a refused new prepare acquires no native custody"
    );
    let first = first.unwrap();
    assert!(
        matches!(
            Reply::parse(&run(ctx, &facts, Request::prepare(first.clone()), caller())),
            Some(Reply::Committed(_))
        ),
        "lost final ACK still recovers the original outcome under pressure"
    );
    facts.clock.store(deadline + 1, Ordering::SeqCst);
    extra.deadline_us = deadline + 60_000_001;
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(extra), caller()));
    assert_eq!(
        ctx.fenced_writes.native.lock().unwrap().by_id.len(),
        1,
        "only expired and actually drained terminal records are reclaimed"
    );
    assert_eq!(
        run(ctx, &facts, Request::prepare(first), caller()),
        "ERR native operation deadline\n",
        "replaying an expired original id cannot create a new cap or renew its budget"
    );
    assert_eq!(
        run(ctx, &facts, Request::Abort(binding), caller()),
        ROLLBACK_SAFE
    );
}
