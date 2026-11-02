// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn cancel(prepare: Prepare) -> Request {
    Request::cancel_prepare(prepare, process(4242, 1, 4242, 400, 2))
}

fn cancel_receipt(prepare: Prepare) -> Reply {
    let Request::CancelPrepare(cancel) = cancel(prepare) else {
        unreachable!()
    };
    Reply::Cancelled(cancel)
}

fn release(gate: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

fn auxiliary() -> SupervisorInputCaller {
    SupervisorInputCaller {
        connection: Some(72),
        ..caller()
    }
}

#[test]
fn cancellation_seals_a_delayed_original_before_any_custody_and_expiry_prevents_recreation() {
    let model = aterm_models_harness::initial_prepare_cancellation_model();
    let mut state = model.init_state();
    let h = crate::session_store::test_handle_on(7940, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let (arrived, seen) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let facts = Arc::new(Facts {
        prepare_gate: Some((arrived, Arc::clone(&gate))),
        ..Facts::new()
    });
    let request = plan(&ctx, &facts);
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let original = request.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::prepare(original),
            caller(),
        )
    });
    let reached = seen.recv_timeout(PATIENCE);
    let receipt = if reached.is_ok() {
        Some(run(&ctx, &facts, cancel(request.clone()), auxiliary()))
    } else {
        None
    };
    let before = {
        let table = ctx.fenced_writes.native.lock().unwrap();
        (
            table.by_id.len(),
            table.reservations.len(),
            table.cancelled.len(),
            table.slots(),
        )
    };
    let independent = FencedWrite::enter_fence(
        &ctx,
        &ticket(&ctx, true, Scope::Approvals),
        crate::metrics::now_us(),
    );
    let admitted_no_custody = independent.is_ok();
    drop(independent);
    release(&gate);
    let late = worker.join();
    assert!(
        reached.is_ok(),
        "original prepare did not reach bounded publication gate"
    );
    assert!(model.fire("Reserve", &mut state));
    assert_eq!(before, (0, 1, 1, 1));
    assert!(
        admitted_no_custody,
        "cancellation cannot reserve a native/input marker"
    );
    assert_eq!(
        Reply::parse(receipt.as_deref().unwrap()),
        Some(cancel_receipt(request.clone()))
    );
    assert!(model.fire("CancelBeforePublication", &mut state));
    assert_eq!(
        Reply::parse(&late.unwrap()),
        Some(cancel_receipt(request.clone()))
    );
    assert!(model.fire("DelayedPublication", &mut state));
    assert!(model.check_invariant("CancelPreventsLateAdmission", &state));
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert!(table.by_id.is_empty());
    assert!(table.reservations.is_empty());
    drop(table);
    facts.clock.store(request.deadline_us, Ordering::Release);
    let retired = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .reclaim(request.deadline_us);
    drop(retired);
    assert!(model.fire("OriginalDeadline", &mut state));
    assert_eq!(
        run(&ctx, &facts, Request::prepare(request), caller()),
        "ERR native operation deadline\n"
    );
    assert!(model.fire("ReclaimedSealReplay", &mut state));
    assert!(model.check_invariant("ExpiredOriginalCannotRecreate", &state));
    assert!(ctx.fenced_writes.native.lock().unwrap().by_id.is_empty());
}

#[test]
fn cancellation_of_a_started_old_image_is_plan_bound_and_never_cancels_committing() {
    let model = aterm_models_harness::initial_prepare_cancellation_model();
    let mut unresolved = model.init_state();
    let h = crate::session_store::test_handle_on(7941, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let original = plan(&ctx, &facts);
    let binding = admitted_binding(&run(
        &ctx,
        &facts,
        Request::prepare(original.clone()),
        caller(),
    ));
    let mut changed = original.clone();
    changed.deadline_us += 1;
    assert_eq!(run(&ctx, &facts, cancel(changed), auxiliary()), DENIED);
    let mut wrong = auxiliary();
    wrong.peer = Some(900);
    assert_eq!(run(&ctx, &facts, cancel(original.clone()), wrong), DENIED);
    facts.enter_target();
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            caller()
        ),
        "OK native-act state=precommitted\n"
    );
    assert!(model.fire("Committing", &mut unresolved));
    assert_eq!(
        run(&ctx, &facts, cancel(original.clone()), auxiliary()),
        UNKNOWN
    );
    assert!(model.fire("CancelUnresolved", &mut unresolved));
    assert!(model.check_invariant("UnknownMutationNeverProvesRollback", &unresolved));
    facts
        .clock
        .store(original.deadline_us + 1, Ordering::Release);
    assert_eq!(
        run(&ctx, &facts, cancel(original.clone()), auxiliary()),
        UNKNOWN
    );
    let done = run(
        &ctx,
        &facts,
        Request::Commit(binding, ImagePhase::Forward, "0.1.0+g123".into()),
        caller(),
    );
    assert!(matches!(Reply::parse(&done), Some(Reply::Committed(_))));
    assert_eq!(
        run(&ctx, &facts, cancel(original.clone()), auxiliary()),
        done
    );
    assert!(
        ctx.fenced_writes.native.lock().unwrap().by_id.is_empty(),
        "expired data history may be reclaimed"
    );
    let mut reclaimed = model.init_state();
    assert!(model.fire("ReclaimedCommittedHistory", &mut reclaimed));
    assert_eq!(
        run(&ctx, &facts, cancel(original.clone()), auxiliary()),
        DENIED,
        "a current target image cannot turn reclaimed committed history into rollback proof"
    );
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::cancel_prepare(original, facts.process(4242).unwrap()),
            auxiliary()
        ),
        UNKNOWN,
        "forging the original actor from the current target image cannot prove an old outcome"
    );
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .cancelled
            .is_empty()
    );
    assert!(model.fire("CancelFromTargetImage", &mut reclaimed));
    assert!(model.check_invariant("ReclaimedHistoryNeedsOriginalActor", &reclaimed));

    let h = crate::session_store::test_handle_on(7942, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let original = plan(&ctx, &facts);
    let binding = admitted_binding(&run(
        &ctx,
        &facts,
        Request::prepare(original.clone()),
        caller(),
    ));
    let receipt = run(&ctx, &facts, cancel(original.clone()), auxiliary());
    assert_eq!(
        Reply::parse(&receipt),
        Some(cancel_receipt(original.clone()))
    );
    assert_eq!(
        run(&ctx, &facts, Request::Query(binding), auxiliary()),
        ROLLBACK_SAFE
    );
    assert_eq!(run(&ctx, &facts, cancel(original), auxiliary()), receipt);
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().completed.len(), 1);
}

#[test]
fn queued_cancellation_retains_the_exact_drain_and_the_original_worker_cannot_restart_it() {
    let h = crate::session_store::test_handle_on(7945, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let old = FencedWrite::enter_fence(
        &ctx,
        &ticket(&ctx, true, Scope::Approvals),
        crate::metrics::now_us(),
    )
    .unwrap();
    let (arrived, seen) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let facts = Arc::new(Facts {
        reservation_gate: Some((arrived, Arc::clone(&gate))),
        ..Facts::new()
    });
    let original = plan(&ctx, &facts);
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let request = original.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::prepare(request),
            caller(),
        )
    });
    let reached = seen.recv_timeout(PATIENCE);
    let pending = if reached.is_ok() {
        Some(run(&ctx, &facts, cancel(original.clone()), auxiliary()))
    } else {
        None
    };
    let replay = if reached.is_ok() {
        Some(run(
            &ctx,
            &facts,
            Request::prepare(original.clone()),
            caller(),
        ))
    } else {
        None
    };
    let refused = FencedWrite::enter_fence(
        &ctx,
        &ticket(&ctx, true, Scope::Approvals),
        crate::metrics::now_us(),
    )
    .is_err();
    release(&gate);
    drop(old);
    let outcome = worker.join();
    assert!(
        reached.is_ok(),
        "original prepare did not reach bounded queue gate"
    );
    assert_eq!(pending.as_deref(), Some(UNKNOWN));
    assert_eq!(replay.as_deref(), Some(UNKNOWN));
    assert!(
        refused,
        "old queued operation still owns its exact marker until its worker drains"
    );
    assert_eq!(outcome.unwrap(), ROLLBACK_SAFE);
    assert_eq!(
        Reply::parse(&run(&ctx, &facts, cancel(original.clone()), auxiliary())),
        Some(cancel_receipt(original))
    );
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert_eq!(table.completed.len(), 1);
    assert!(matches!(
        table.by_id.values().next().unwrap().lock().unwrap().phase,
        Phase::Completed(Decision::ProvenNotCommittedRollbackSafe)
    ));
}

#[test]
fn cancellation_support_precedes_claim_and_absent_seals_require_kernel_actor_and_owned_child() {
    let h = crate::session_store::test_handle_on(7943, -1);
    let ctx = Arc::clone(&h.ctx);
    let facts = Facts::new();
    let authority = ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us());
    assert_eq!(
        Reply::parse(&run(&ctx, &facts, Request::CancelSupport, auxiliary())),
        Some(Reply::CancelSupport)
    );
    assert!(matches!(
        Reply::parse(&run(&ctx, &facts, Request::Info, auxiliary())),
        Some(Reply::Info { .. })
    ));
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us()),
        authority
    );
    let request = plan(&ctx, &facts);
    let mut wrong = auxiliary();
    wrong.proof = None;
    assert_eq!(run(&ctx, &facts, cancel(request.clone()), wrong), DENIED);
    let mut child_changed = request.clone();
    let NativePlan::OwnImage { child, .. } = &mut child_changed.plan else {
        unreachable!()
    };
    child.birth += 1;
    assert_eq!(
        run(&ctx, &facts, cancel(child_changed), auxiliary()),
        DENIED
    );
    let mut old_epoch = request.clone();
    let digit = if old_epoch.host_epoch.starts_with('0') {
        "1"
    } else {
        "0"
    };
    old_epoch.host_epoch.replace_range(..1, digit);
    assert_eq!(run(&ctx, &facts, cancel(old_epoch), auxiliary()), UNKNOWN);
    let receipt = run(&ctx, &facts, cancel(request.clone()), auxiliary());
    assert_eq!(
        Reply::parse(&receipt),
        Some(cancel_receipt(request.clone())),
        "no live claim is needed to cancel an absent request"
    );
    facts
        .processes
        .lock()
        .unwrap()
        .get_mut(&4242)
        .unwrap()
        .birth += 1;
    assert_eq!(
        run(&ctx, &facts, cancel(request), auxiliary()),
        DENIED,
        "PID reuse is not the captured cancelling actor"
    );
    assert!(ctx.fenced_writes.native.lock().unwrap().by_id.is_empty());
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us()),
        authority
    );
}

#[test]
fn own_image_admission_requires_an_actually_different_target_image() {
    let h = crate::session_store::test_handle_on(7946, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let mut request = plan(&ctx, &facts);
    let NativePlan::OwnImage { target, .. } = &mut request.plan else {
        unreachable!()
    };
    *target = facts.process(4242).unwrap().image;
    assert_eq!(
        run(&ctx, &facts, Request::prepare(request), caller()),
        DENIED
    );
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert!(table.by_id.is_empty());
    assert!(table.reservations.is_empty());
    assert!(table.cancelled.is_empty());
}

#[test]
fn cancellation_slots_are_bounded_and_never_evict_an_unexpired_no_future_receipt() {
    let model = aterm_models_harness::initial_prepare_cancellation_model();
    let mut state = model.init_state();
    let h = crate::session_store::test_handle_on(7944, -1);
    let ctx = Arc::clone(&h.ctx);
    let facts = Facts::new();
    let original = plan(&ctx, &facts);
    for index in 0..MAX_COMPLETED {
        let mut request = original.clone();
        request.id = format!("{index:032x}");
        assert!(matches!(
            Reply::parse(&run(&ctx, &facts, cancel(request), auxiliary())),
            Some(Reply::Cancelled(_))
        ));
    }
    assert!(model.fire("FullHistory", &mut state));
    let mut overflow = original.clone();
    overflow.id = "ff".repeat(16);
    assert_eq!(
        run(&ctx, &facts, cancel(overflow.clone()), auxiliary()),
        "ERR busy native operation history\n"
    );
    assert!(model.fire("CancelAtCapacity", &mut state));
    assert!(model.check_invariant("CancellationHistoryIsBounded", &state));
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert_eq!(table.slots(), MAX_COMPLETED);
    assert!(table.by_id.is_empty());
    assert!(table.reservations.is_empty());
    drop(table);
    let mut first = original.clone();
    first.id = "00".repeat(16);
    assert_eq!(
        Reply::parse(&run(&ctx, &facts, cancel(first), auxiliary())),
        Some(cancel_receipt(Prepare {
            id: "00".repeat(16),
            ..original.clone()
        }))
    );
    facts
        .clock
        .store(original.deadline_us + 1, Ordering::Release);
    overflow.deadline_us = original.deadline_us + 2_000_000;
    assert!(matches!(
        Reply::parse(&run(&ctx, &facts, cancel(overflow), auxiliary())),
        Some(Reply::Cancelled(_))
    ));
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().slots(), 1);
}

#[test]
fn initial_cancellation_negative_controls_falsify_every_law() {
    let base = aterm_models_harness::initial_prepare_cancellation_model();
    for (bug, law, actions) in [
        (
            1,
            "CancelPreventsLateAdmission",
            vec!["Reserve", "CancelBeforePublication", "DelayedPublication"],
        ),
        (
            2,
            "ExpiredOriginalCannotRecreate",
            vec![
                "Reserve",
                "CancelBeforePublication",
                "DelayedPublication",
                "OriginalDeadline",
                "ReclaimedSealReplay",
            ],
        ),
        (
            3,
            "UnknownMutationNeverProvesRollback",
            vec!["Committing", "CancelUnresolved"],
        ),
        (
            4,
            "CancellationHistoryIsBounded",
            vec!["FullHistory", "CancelAtCapacity"],
        ),
        (
            5,
            "ReclaimedHistoryNeedsOriginalActor",
            vec!["ReclaimedCommittedHistory", "CancelFromTargetImage"],
        ),
    ] {
        let model = aterm_spec::interp::with_buggy(&base, bug);
        let mut state = model.init_state();
        let mut caught = false;
        for action in actions {
            assert!(model.fire(action, &mut state));
            caught |= !base.check_invariant(law, &state);
        }
        assert!(caught, "negative control {bug}: {law}");
    }
}
