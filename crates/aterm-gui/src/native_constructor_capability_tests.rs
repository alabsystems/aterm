// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn release(gate: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

#[test]
fn shipping_generic_constructors_stay_disabled_and_new_prepare_has_no_custody() {
    let h = crate::session_store::test_handle_on(7950, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let shipping = serve_by(
        &ctx,
        -1,
        Request::Constructors,
        caller(),
        &System(&ctx.native_policy, false),
        &mut |_| panic!("no mutation"),
    );
    let Some(Reply::Constructors(enabled)) = Reply::parse(shipping.trim_end()) else {
        panic!("actual constructor discovery: {shipping}");
    };
    assert!(!enabled.contains(&Constructor::OwnImage));
    assert!(!enabled.contains(&Constructor::PrivateRestart));
    assert_eq!(
        enabled,
        if crate::native_session_policy::cold_restart_qualified() {
            vec![Constructor::ColdRestart, Constructor::AskedUpgrade]
        } else {
            Vec::new()
        }
    );
    let facts = Facts::new();
    facts.constructors.store(false, Ordering::Release);
    let model = aterm_models_harness::scoped_native_handoff_model();
    let mut state = model.init_state();
    assert_eq!(
        run(&ctx, &facts, Request::prepare(plan(&ctx, &facts)), caller()),
        "ERR native constructor disabled\n"
    );
    assert!(model.fire("DisabledConstructor", &mut state));
    assert!(model.check_invariant("ConstructorCapabilityPrecedesAdmission", &state));
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert!(table.by_id.is_empty());
    assert!(table.reservations.is_empty());
    assert!(!table.builtin_coordinated);
    drop(table);
    assert!(
        FencedWrite::enter_fence(
            &ctx,
            &ticket(&ctx, true, Scope::Approvals),
            crate::metrics::now_us()
        )
        .is_ok()
    );
}

#[test]
fn constructor_disable_during_preparation_is_rechecked_at_final_publication() {
    let h = crate::session_store::test_handle_on(7951, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let (arrived, seen) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let facts = Arc::new(Facts {
        prepare_gate: Some((arrived, Arc::clone(&gate))),
        ..Facts::new()
    });
    let original = plan(&ctx, &facts);
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::prepare(original),
            caller(),
        )
    });
    let reached = seen.recv_timeout(PATIENCE);
    facts.constructors.store(false, Ordering::Release);
    release(&gate);
    let result = worker.join();
    assert!(
        reached.is_ok(),
        "original reached bounded final-admission gate"
    );
    assert_eq!(result.unwrap(), "ERR native constructor disabled\n");
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert!(table.by_id.is_empty());
    assert!(table.reservations.is_empty());
}

#[test]
fn queued_disable_cancels_before_admission_but_same_id_reconciliation_remains_available() {
    let h = crate::session_store::test_handle_on(7952, -1);
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
    facts.constructors.store(false, Ordering::Release);
    release(&gate);
    drop(old);
    let result = worker.join();
    assert!(reached.is_ok(), "original reached bounded queued gate");
    assert_eq!(result.unwrap(), ROLLBACK_SAFE);
    assert_eq!(
        run(&ctx, &facts, Request::prepare(original), caller()),
        ROLLBACK_SAFE
    );
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().completed.len(), 1);
}

#[test]
fn an_admitted_outcome_can_finish_after_constructor_support_is_disabled() {
    let h = crate::session_store::test_handle_on(7953, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let request = plan(&ctx, &facts);
    let binding = admitted_binding(&run(
        &ctx,
        &facts,
        Request::prepare(request.clone()),
        caller(),
    ));
    facts.constructors.store(false, Ordering::Release);
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
    let committed = run(
        &ctx,
        &facts,
        Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0+g123".into()),
        caller(),
    );
    assert!(matches!(
        Reply::parse(&committed),
        Some(Reply::Committed(_))
    ));
    assert_eq!(
        run(&ctx, &facts, Request::Query(binding), caller()),
        committed
    );
    assert_eq!(
        run(&ctx, &facts, Request::prepare(request), caller()),
        committed
    );
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().completed.len(), 1);
}
