// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn historical_fallback_keeps_the_request_pin_through_the_journal_read() {
    let (handle, mut facts, prepare, actor) = closed_restart::fixture(8250);
    let ctx = &handle.ctx;
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(prepare.clone()),
        actor.clone(),
    ));
    assert_eq!(
        run(ctx, &facts, Request::Signal(binding.clone()), actor.clone()),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    assert_eq!(
        run(ctx, &facts, Request::Reconcile(binding), actor.clone()),
        NOEXEC
    );
    // A different current actor cannot replay live authority, but a local
    // authenticated reader can still observe an exact historical receipt.
    facts.processes.lock().unwrap().get_mut(&900).unwrap().image = image(21);
    let observed = Arc::new(AtomicBool::new(false));
    facts.historical_pin_probe = Some((Arc::downgrade(ctx), Arc::clone(&observed)));
    assert_eq!(run(ctx, &facts, Request::prepare(prepare), actor), NOEXEC);
    assert!(observed.load(Ordering::SeqCst));
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .receipt_flights
            .is_empty()
    );
}

pub(crate) fn flight_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    flight_transition_by(state, action, false)
}

fn flight_transition_by(
    state: &aterm_spec::interp::State,
    action: &str,
    ignore_pin: bool,
) -> aterm_spec::interp::State {
    let (handle, facts, prepare, actor) = closed_restart::fixture(8002);
    let ctx = &handle.ctx;
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(prepare.clone()), actor));
    facts.processes.lock().unwrap().remove(&4242);
    let operation = lookup(ctx, &binding).unwrap();
    let completion = operation
        .lock()
        .unwrap()
        .seal_retirement(Retirement::NotGranted, &facts);
    assert_eq!(finish(ctx, &operation, completion), NOEXEC);
    let query = Request::Query(binding.clone());
    let begin = || ReceiptFlight::begin(ctx, &query).unwrap().unwrap();
    let mut a = (state["a"] == 1).then(begin);
    let mut b = (state["b"] == 1).then(begin);
    let mut next = state.clone();
    match action {
        "BeginA" => {
            a = Some(begin());
            next.insert("a", 1);
        }
        "BeginB" => {
            b = Some(begin());
            next.insert("b", 1);
        }
        "EndA" => {
            drop(a.take());
            next.insert("a", 2);
        }
        "EndB" => {
            drop(b.take());
            next.insert("b", 2);
        }
        "Expire" => {
            next.insert("expired", 1);
        }
        "Reclaim" => {
            let mut table = ctx.fenced_writes.native.lock().unwrap();
            if ignore_pin {
                // Negative control: the old reclamation path did not consult
                // active receipt requests before dropping terminal history.
                table.receipt_flights.remove(&binding.id);
            }
            let reclaimed = table.reclaim(prepare.deadline_us + 1);
            next.insert(
                "idle_refusal",
                i64::from(state["pins"] == 0 && table.by_id.contains_key(&binding.id)),
            );
            drop(table);
            drop(reclaimed);
        }
        _ => panic!("unknown flight action {action}"),
    }
    let table = ctx.fenced_writes.native.lock().unwrap();
    next.insert("gone", i64::from(!table.by_id.contains_key(&binding.id)));
    next.insert(
        "pins",
        i64::try_from(*table.receipt_flights.get(&binding.id).unwrap_or(&0)).unwrap(),
    );
    drop(table);
    drop((a, b));
    next
}

#[test]
fn receipt_flight_refines_overlap_expiry_and_reclamation() {
    let model = aterm_models_harness::noexec_flight_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = flight_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut active = model.init_state();
    active.insert("a", 1);
    active.insert("pins", 1);
    active.insert("expired", 1);
    let unprotected = flight_transition_by(&active, "Reclaim", true);
    assert!(!model.check_invariant("RetainedWhileActive", &unprotected));
}

#[test]
fn receipt_io_holds_no_native_locks_and_the_terminal_seal_fences_execute() {
    let (handle, mut facts, prepare, actor) = closed_restart::fixture(7999);
    let ctx = Arc::clone(&handle.ctx);
    let binding = admitted_binding(&run(&ctx, &facts, Request::prepare(prepare), actor.clone()));
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Signal(binding.clone()),
            actor.clone()
        ),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    let guard = {
        let operation = lookup(&ctx, &binding).unwrap();
        let op = operation.lock().unwrap();
        Arc::downgrade(&op.custody.as_ref().unwrap().handoff_guard().unwrap())
    };
    let (entered, waiting) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    facts.receipt_gate = Some((entered, Arc::clone(&proceed)));
    let facts = Arc::new(facts);
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let worker_binding = binding.clone();
    let worker_actor = actor.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::Reconcile(worker_binding),
            worker_actor,
        )
    });
    waiting.recv_timeout(PATIENCE).expect("receipt I/O began");
    assert!(ctx.meta.try_lock().is_ok());
    assert!(ctx.fenced_writes.native.try_lock().is_ok());
    assert!(lookup(&ctx, &binding).unwrap().try_lock().is_ok());
    let reclaimed = ctx.fenced_writes.native.lock().unwrap().reclaim(u64::MAX);
    assert!(
        reclaimed.is_empty(),
        "an in-flight durable acknowledgement pins its operation"
    );
    {
        let operation = lookup(&ctx, &binding).unwrap();
        let op = operation.lock().unwrap();
        assert!(op.completion_drain.as_ref().unwrap().is_drained());
    }
    assert!(
        guard.upgrade().is_some(),
        "receipt worker retains the original handoff hold"
    );
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Execute(binding, ImagePhase::Forward),
            actor
        ),
        DENIED
    );
    let (lock, ready) = &*proceed;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    assert_eq!(worker.join().unwrap(), NOEXEC);
    assert!(
        guard.upgrade().is_none(),
        "durable acknowledgement releases the handoff hold"
    );
}

#[test]
fn noexec_ack_waits_for_storage_and_replays_only_an_authenticated_observation() {
    let (handle, facts, prepare, actor) = closed_restart::fixture(7998);
    let ctx = &handle.ctx;
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(prepare.clone()),
        actor.clone(),
    ));
    assert_eq!(
        run(ctx, &facts, Request::Signal(binding.clone()), actor.clone()),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    facts.receipt_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    let operation = lookup(ctx, &binding).unwrap();
    assert_eq!(
        operation.lock().unwrap().phase,
        Phase::Completed(Decision::Retired(Retirement::NotGranted))
    );
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .receipt_flights
            .is_empty()
    );
    assert!(facts.receipt_history.lock().unwrap().is_empty());
    // Negative control: the old in-memory reply would acknowledge despite
    // unavailable durability. The actual public worker path withholds it.
    assert_eq!(completed_reply(&operation), NOEXEC);
    assert_eq!(
        run(ctx, &facts, Request::Query(binding.clone()), actor.clone()),
        UNKNOWN
    );
    facts.receipt_allowed.store(true, Ordering::SeqCst);
    assert_eq!(
        run(ctx, &facts, Request::Query(binding.clone()), actor.clone()),
        NOEXEC
    );
    // Reclaim the actual deadline-expired operation. Its exact old preparation
    // must remain observable despite deadline and constructor refusals.
    facts.clock.store(prepare.deadline_us + 1, Ordering::SeqCst);
    let reclaimed = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .reclaim(prepare.deadline_us + 1);
    assert_eq!(reclaimed.len(), 1);
    drop(reclaimed);
    assert_eq!(
        run(ctx, &facts, Request::Query(binding.clone()), actor.clone()),
        NOEXEC
    );
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            actor.clone()
        ),
        NOEXEC
    );
    facts.constructors.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone()
        ),
        NOEXEC
    );
    facts.constructors.store(true, Ordering::SeqCst);
    let mut reused = prepare.clone();
    reused.deadline_us += 1_000_000;
    if let NativePlan::Restart { process, .. } = &mut reused.plan {
        process.birth += 1_000;
        facts
            .processes
            .lock()
            .unwrap()
            .insert(process.pid, process.clone());
        facts.foreground.store(process.group, Ordering::SeqCst);
    }
    assert_eq!(
        run(ctx, &facts, Request::prepare(reused.clone()), actor.clone()),
        DENIED
    );
    assert!(ctx.fenced_writes.native.lock().unwrap().by_id.is_empty());
    let mut fresh_control = reused.clone();
    fresh_control.id = "98".repeat(16);
    reused.id = "dc".repeat(16);
    facts.receipt_readable.store(false, Ordering::SeqCst);
    assert_eq!(
        run(ctx, &facts, Request::prepare(reused), actor.clone()),
        UNKNOWN
    );
    assert!(ctx.fenced_writes.native.lock().unwrap().by_id.is_empty());
    facts.receipt_readable.store(true, Ordering::SeqCst);
    let mut prior_epoch = prepare.clone();
    prior_epoch.host_epoch = "34".repeat(16);
    prior_epoch.id = "56".repeat(16);
    let mut prior_binding = binding.clone();
    prior_binding.id = prior_epoch.id.clone();
    facts
        .receipt_history
        .lock()
        .unwrap()
        .push((prior_epoch.clone(), prior_binding));
    assert_eq!(
        run(ctx, &facts, Request::prepare(prior_epoch), actor.clone()),
        NOEXEC
    );
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone()
        ),
        NOEXEC
    );
    for request in [
        Request::Signal(binding.clone()),
        Request::Recover(binding.clone()),
        Request::Abort(binding.clone()),
        Request::Attach(binding.clone(), ImagePhase::Forward),
        Request::Execute(binding.clone(), ImagePhase::Forward),
        Request::Resume(binding.clone(), ImagePhase::Forward, "1.1".into(), 0),
    ] {
        assert_eq!(run(ctx, &facts, request, actor.clone()), DENIED);
    }
    let mut other = binding.clone();
    other.capability = "ff".repeat(32);
    assert_eq!(
        run(ctx, &facts, Request::Query(other), actor.clone()),
        DENIED
    );
    let mut other_actor = actor.clone();
    other_actor.principal = Principal::Instance;
    assert_eq!(
        run(ctx, &facts, Request::Query(binding.clone()), other_actor),
        DENIED
    );
    let mut remote = actor.clone();
    remote.peer = None;
    assert_eq!(run(ctx, &facts, Request::Query(binding), remote), DENIED);
    let mut changed = prepare;
    changed.deadline_us += 1;
    assert_ne!(
        run(ctx, &facts, Request::prepare(changed), actor.clone()),
        NOEXEC
    );
    assert!(ctx.fenced_writes.native.lock().unwrap().by_id.is_empty());
    assert_eq!(facts.signals.load(Ordering::SeqCst), 1);
    // The replacement source is genuinely admissible with a fresh ID. A
    // dead-source refusal must not mask a missing durable-ID fence in this test.
    let fresh_binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(fresh_control),
        actor.clone(),
    ));
    assert_eq!(
        run(ctx, &facts, Request::Abort(fresh_binding), actor),
        ROLLBACK_SAFE
    );
}

#[test]
fn expired_failed_and_orphaned_receipts_do_not_pin_history() {
    for orphaned in [false, true] {
        let (handle, facts, prepare, actor) = closed_restart::fixture(8000 + u64::from(orphaned));
        let ctx = &handle.ctx;
        let binding = admitted_binding(&run(
            ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone(),
        ));
        assert_eq!(
            run(ctx, &facts, Request::Signal(binding.clone()), actor.clone()),
            "OK native-act state=signalled\n"
        );
        facts.processes.lock().unwrap().remove(&4242);
        facts.foreground.store(400, Ordering::SeqCst);
        facts.receipt_allowed.store(false, Ordering::SeqCst);
        if orphaned {
            facts.clock.store(prepare.deadline_us + 1, Ordering::SeqCst);
            reconcile_extinct_by(ctx, &facts);
        } else {
            assert_eq!(
                run(ctx, &facts, Request::Reconcile(binding.clone()), actor),
                UNKNOWN
            );
        }
        assert_eq!(
            lookup(ctx, &binding).unwrap().lock().unwrap().phase,
            Phase::Completed(Decision::Retired(Retirement::NotGranted))
        );
        let mut table = ctx.fenced_writes.native.lock().unwrap();
        assert!(table.receipt_flights.is_empty());
        let reclaimed = table.reclaim(prepare.deadline_us + 1);
        assert_eq!(reclaimed.len(), 1, "orphaned={orphaned}");
        assert!(table.by_id.is_empty());
        drop(table);
        drop(reclaimed);
    }
}
