// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control_input_ack::{InputMark, testing};
use crate::input::{Delivery, EgressReceipt};

fn ready(
    local: u64,
) -> (
    crate::session_store::SessionHandle,
    Facts,
    Binding,
    SupervisorInputCaller,
    SupervisorInputCaller,
) {
    let (handle, mut reader) = testing::owned_handle(local);
    let (handle, facts, prepare, actor) = closed_restart::fixture_on(handle);
    let ctx = &handle.ctx;
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(prepare), actor.clone()));
    assert_eq!(
        run(ctx, &facts, Request::Signal(binding.clone()), actor.clone()),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    let mut dispatch = |_: &str| {
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
        completion.finish(EgressReceipt::from_reported_delivery(
            Delivery::Full,
            receipt.order(),
        ));
        drop(producer);
        "OK typed=1\n".into()
    };
    assert_eq!(
        serve_by(
            ctx,
            -1,
            Request::Resume(
                binding.clone(),
                ImagePhase::Forward,
                "1.1".into(),
                ctx.human_input.seq()
            ),
            actor.clone(),
            &facts,
            &mut dispatch
        ),
        "OK native-act state=resumed phase=forward\n"
    );
    assert_eq!(testing::byte(&mut reader), b'R');
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
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Attach(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        )),
        Some(Reply::Launch { .. })
    ));
    (handle, facts, binding, actor, launcher)
}

#[test]
fn append_is_unlocked_and_blocks_noexec_replacement_and_second_grant_until_revalidated() {
    let (handle, mut facts, binding, actor, launcher) = ready(8100);
    let ctx = Arc::clone(&handle.ctx);
    let (entered, waiting) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    facts.grant_journal_gate = Some((entered, Arc::clone(&proceed)));
    let facts = Arc::new(facts);
    let worker_ctx = ctx.clone();
    let worker_facts = facts.clone();
    let worker_binding = binding.clone();
    let worker_launcher = launcher.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::Execute(worker_binding, ImagePhase::Forward),
            worker_launcher,
        )
    });
    waiting.recv_timeout(PATIENCE).expect("grant append began");
    assert!(ctx.meta.try_lock().is_ok());
    assert!(ctx.fenced_writes.native.try_lock().is_ok());
    let operation = lookup(&ctx, &binding).unwrap();
    {
        let op = operation.try_lock().unwrap();
        assert_eq!(op.grant_record, GrantRecord::Writing(1));
        assert!(!op.children[0].as_ref().unwrap().execute_sent);
    }
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        ),
        UNKNOWN
    );
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Resume(
                binding.clone(),
                ImagePhase::Rollback,
                "1.1".into(),
                ctx.human_input.seq()
            ),
            actor.clone()
        ),
        UNKNOWN
    );
    facts.conversation_valid.store(false, Ordering::SeqCst);
    let (lock, wake) = &*proceed;
    *lock.lock().unwrap() = true;
    wake.notify_all();
    assert_eq!(
        worker.join().unwrap(),
        DENIED,
        "the post-append conversation check is mandatory"
    );
    assert_eq!(
        operation.lock().unwrap().grant_record,
        GrantRecord::Recorded
    );
    facts.conversation_valid.store(true, Ordering::SeqCst);
    assert_eq!(
        run(&ctx, &facts, Request::Reconcile(binding.clone()), actor),
        UNKNOWN
    );
    assert!(facts.receipt_history.lock().unwrap().is_empty());
    assert!(matches!(
        Reply::parse(&run(
            &ctx,
            &facts,
            Request::Execute(binding, ImagePhase::Forward),
            launcher
        )),
        Some(Reply::Execute { .. })
    ));
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn ambiguous_append_never_grants_and_stale_cleanup_cannot_clear_a_new_attempt() {
    let (handle, facts, binding, actor, launcher) = ready(8101);
    let ctx = &handle.ctx;
    facts.grant_journal_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        ),
        UNKNOWN
    );
    let operation = lookup(ctx, &binding).unwrap();
    assert_eq!(
        operation.lock().unwrap().grant_record,
        GrantRecord::Uncertain(1)
    );
    assert!(
        !operation.lock().unwrap().children[0]
            .as_ref()
            .unwrap()
            .execute_sent
    );
    assert_eq!(
        run(ctx, &facts, Request::Reconcile(binding.clone()), actor),
        UNKNOWN
    );
    let stale = GrantWriting {
        operation: operation.clone(),
        attempt: 1,
    };
    operation.lock().unwrap().grant_record = GrantRecord::Writing(2);
    drop(stale);
    assert_eq!(
        operation.lock().unwrap().grant_record,
        GrantRecord::Writing(2)
    );
    operation.lock().unwrap().grant_record = GrantRecord::Uncertain(2);
    facts.grant_journal_allowed.store(true, Ordering::SeqCst);
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Execute(binding, ImagePhase::Forward),
            launcher
        )),
        Some(Reply::Execute { .. })
    ));
    assert_eq!(
        operation.lock().unwrap().grant_record,
        GrantRecord::Recorded
    );
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn preparation_without_durable_admission_exposes_no_mutation_authority() {
    let (handle, facts, prepare, actor) = closed_restart::fixture(8102);
    facts.prepare_journal_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    let operation = handle.ctx.fenced_writes.native.lock().unwrap().by_id[&prepare.id].clone();
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert!(operation.lock().unwrap().custody.is_none());
    assert_eq!(terminal_record(&operation.lock().unwrap(), UNKNOWN), None);
    assert!(
        operation
            .lock()
            .unwrap()
            .seal_completion(Decision::Unknown)
            .is_none()
    );
    let binding = operation.lock().unwrap().binding.clone();
    assert_ne!(
        run(&handle.ctx, &facts, Request::Signal(binding), actor),
        "OK native-act state=signalled\n"
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_recorded_preparation_waits_for_custody_without_repeating_its_append() {
    let (handle, facts, prepare, actor) = closed_restart::fixture(8119);
    let ctx = &handle.ctx;
    facts.prepare_journal_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    let operation = ctx.fenced_writes.native.lock().unwrap().by_id[&prepare.id].clone();
    let binding = operation.lock().unwrap().binding.clone();
    let boundary = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
        .unwrap();
    let busy = ctx
        .fenced_writes
        .reserve_native(prepare.revision, "another-operation", boundary)
        .unwrap();
    facts.prepare_journal_allowed.store(true, Ordering::SeqCst);
    assert_eq!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone()
        )),
        Some(Reply::Pending(binding.clone()))
    );
    assert!(operation.lock().unwrap().preparation.recorded);
    assert!(operation.lock().unwrap().custody.is_none());
    drop(busy);
    facts.prepare_journal_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        admitted_binding(&run(ctx, &facts, Request::prepare(prepare), actor)),
        binding
    );
}

#[test]
fn an_expired_root_preparation_resolves_and_closes_the_real_journal() {
    use aterm_agent::harness::native_receipts::testing::{Journal, State};
    let (handle, mut facts, prepare, actor) = closed_restart::fixture(8120);
    let home = aterm_tempfile::tempdir().unwrap();
    facts.native_journal = Some(Journal::new(home.path()));
    facts.prepare_journal_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    facts.prepare_journal_allowed.store(true, Ordering::SeqCst);
    facts.clock.store(prepare.deadline_us + 1, Ordering::SeqCst);
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor
        ),
        ROLLBACK_SAFE
    );
    let binding = facts.prepared_history.lock().unwrap()[0].1.clone();
    assert_eq!(
        facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&binding)
            .unwrap(),
        Some(State::Sealed { may_execute: false })
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
}

#[test]
fn direct_continuation_capture_cannot_bypass_operation_wide_grant_history() {
    let (handle, facts, binding, _, _) = ready(8105);
    let operation = lookup(&handle.ctx, &binding).unwrap();
    let mut op = operation.lock().unwrap();
    assert!(ClosedRestart::capture(&op, Retirement::NotGranted, &facts).is_some());
    for record in [
        GrantRecord::Writing(1),
        GrantRecord::Uncertain(1),
        GrantRecord::Recorded,
    ] {
        op.grant_record = record;
        assert!(
            ClosedRestart::capture(&op, Retirement::NotGranted, &facts).is_none(),
            "{record:?}"
        );
    }
}

#[test]
fn uncertain_grant_closes_only_as_unknown_after_positive_quiescence() {
    let (handle, facts, binding, actor, launcher) = ready(8103);
    let ctx = &handle.ctx;
    facts.grant_journal_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        ),
        UNKNOWN
    );
    let operation = lookup(ctx, &binding).unwrap();
    // Even a dead launcher birth is insufficient while its process group lives.
    facts.processes.lock().unwrap().remove(&600);
    facts
        .processes
        .lock()
        .unwrap()
        .insert(601, process(601, 400, 600, 400, 20));
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    assert_eq!(operation.lock().unwrap().phase, Phase::Started);
    facts.processes.lock().unwrap().remove(&601);
    facts.foreground.store(400, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    assert_eq!(
        operation.lock().unwrap().phase,
        Phase::Completed(Decision::Unknown)
    );
    assert!(facts.receipt_history.lock().unwrap().is_empty());
    assert_eq!(facts.sealed_history.lock().unwrap().len(), 1);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher
        ),
        DENIED
    );
    let deadline = operation.lock().unwrap().request.deadline_us;
    let retired = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .reclaim(deadline + 1);
    assert_eq!(retired.len(), 1);
    drop(retired);
    assert_eq!(run(ctx, &facts, Request::Query(binding), actor), DENIED);
    assert!(facts.receipt_history.lock().unwrap().is_empty());
}

fn grant_number(record: GrantRecord) -> i64 {
    match record {
        GrantRecord::None => 0,
        GrantRecord::Writing(_) => 1,
        GrantRecord::Uncertain(_) => 2,
        GrantRecord::Recorded => 3,
    }
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_by(state, action, false)
}

fn transition_by(
    state: &aterm_spec::interp::State,
    action: &str,
    cache_permission: bool,
) -> aterm_spec::interp::State {
    let (handle, facts, binding, actor, launcher) = ready(8104);
    let ctx = &handle.ctx;
    let operation = lookup(ctx, &binding).unwrap();
    {
        let mut op = operation.lock().unwrap();
        op.grant_record = match state["record"] {
            0 => GrantRecord::None,
            1 => GrantRecord::Writing(state["attempt"] as u64),
            2 => GrantRecord::Uncertain(state["attempt"] as u64),
            3 => GrantRecord::Recorded,
            _ => unreachable!(),
        };
        op.children[0].as_mut().unwrap().execute_sent = state["granted"] == 1;
        op.phase = match state["phase"] {
            0 => Phase::Started,
            1 => Phase::Completed(Decision::Retired(Retirement::NotGranted)),
            2 => Phase::Completed(Decision::Unknown),
            _ => unreachable!(),
        };
    }
    facts
        .conversation_valid
        .store(state["valid"] == 1 || cache_permission, Ordering::SeqCst);
    let mut next = state.clone();
    match action {
        "Reserve" => {
            assert_eq!(
                serve_launcher_step(
                    ctx,
                    -1,
                    &Request::Execute(binding.clone(), ImagePhase::Forward),
                    &launcher,
                    &operation,
                    &facts,
                    GrantStep::Reserve
                ),
                JOURNAL_GRANT
            );
            let GrantRecord::Writing(attempt) = operation.lock().unwrap().grant_record else {
                panic!("reserve lost")
            };
            next.insert("attempt", attempt as i64);
        }
        "Ack" | "Fail" => {
            assert!(
                operation
                    .lock()
                    .unwrap()
                    .grant_record
                    .finish(state["attempt"] as u64, action == "Ack")
            );
        }
        "Invalidate" => {
            next.insert("valid", 0);
        }
        "Grant" => {
            let reply = serve_launcher_step(
                ctx,
                -1,
                &Request::Execute(binding.clone(), ImagePhase::Forward),
                &launcher,
                &operation,
                &facts,
                GrantStep::Deliver,
            );
            let emitted = matches!(Reply::parse(&reply), Some(Reply::Execute { .. }));
            if emitted {
                next.insert("granted", 1);
                if state["record"] != 3 || state["valid"] == 0 {
                    next.insert("bad_grant", 1);
                }
            }
        }
        "OldCleanup" => {
            let old = operation.lock().unwrap().grant_record;
            drop(GrantWriting {
                operation: operation.clone(),
                attempt: 1,
            });
            next.insert("cleanup", 1);
            next.insert(
                "bad_cleanup",
                i64::from(operation.lock().unwrap().grant_record != old),
            );
        }
        "Close" => {
            facts.processes.lock().unwrap().remove(&600);
            facts.foreground.store(400, Ordering::SeqCst);
            let _ = run(ctx, &facts, Request::Reconcile(binding.clone()), actor);
            let phase = operation.lock().unwrap().phase;
            next.insert(
                "phase",
                match phase {
                    Phase::Completed(Decision::Retired(Retirement::NotGranted)) => 1,
                    Phase::Completed(Decision::Unknown) => 2,
                    Phase::Started => 0,
                    _ => panic!("unexpected close {phase:?}"),
                },
            );
            next.insert(
                "bad_close",
                i64::from(state["record"] == 1 && matches!(phase, Phase::Completed(_))),
            );
        }
        "ProbeReplacement" => {
            facts.processes.lock().unwrap().remove(&600);
            let allowed = launch_unissued(&operation.lock().unwrap(), ImagePhase::Forward, &facts);
            next.insert("probed", 1);
            next.insert("unsafe_replace", i64::from(state["attempt"] > 0 && allowed));
        }
        _ => panic!("unknown issuer grant action {action}"),
    }
    next.insert(
        "record",
        grant_number(operation.lock().unwrap().grant_record),
    );
    next
}

#[test]
fn issuer_grant_refines_real_host_checks_and_attempt_ownership() {
    let model = aterm_models_harness::issuer_grant_model();
    aterm_spec::interp::prove_and_catch(&model);
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
    // Negative implementation: pretend the conversation permission captured
    // before the append is still current. The real launcher issues a grant if
    // fed that stale probe, and the projection must catch it.
    let mut stale = model.init_state();
    stale.insert("record", 3);
    stale.insert("attempt", 1);
    stale.insert("valid", 0);
    let bad = transition_by(&stale, "Grant", true);
    assert!(!model.check_invariant("GrantNeedsAcknowledgementAndRevalidation", &bad));
}

#[test]
fn a_pending_capability_cannot_mutate_during_failed_preparation_io() {
    let (handle, mut facts, prepare, actor) = closed_restart::fixture(8106);
    let ctx = Arc::clone(&handle.ctx);
    let (entered, waiting) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    facts.prepare_journal_gate = Some((entered, Arc::clone(&proceed)));
    facts.prepare_journal_allowed.store(false, Ordering::SeqCst);
    let facts = Arc::new(facts);
    let worker_ctx = ctx.clone();
    let worker_facts = facts.clone();
    let worker_prepare = prepare.clone();
    let worker_actor = actor.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::prepare(worker_prepare),
            worker_actor,
        )
    });
    waiting
        .recv_timeout(PATIENCE)
        .expect("prepare append began");
    assert!(ctx.meta.try_lock().is_ok());
    assert!(ctx.fenced_writes.native.try_lock().is_ok());
    let operation = ctx.fenced_writes.native.lock().unwrap().by_id[&prepare.id].clone();
    let binding = operation.lock().unwrap().binding.clone();
    assert_eq!(
        run(&ctx, &facts, Request::prepare(prepare), actor.clone()),
        UNKNOWN,
        "a concurrent unacknowledged writer discloses no capability"
    );
    for request in [
        Request::Signal(binding.clone()),
        Request::Resume(
            binding.clone(),
            ImagePhase::Forward,
            "1.1".into(),
            ctx.human_input.seq(),
        ),
        Request::Precommit(binding.clone(), ImagePhase::Forward),
        Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0+g123".into()),
        Request::Abort(binding.clone()),
    ] {
        let reply = run(&ctx, &facts, request, actor.clone());
        assert!(
            reply == UNKNOWN || reply == DENIED,
            "pending mutation: {reply}"
        );
    }
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    *proceed.0.lock().unwrap() = true;
    proceed.1.notify_all();
    assert_eq!(worker.join().unwrap(), UNKNOWN);
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert!(operation.lock().unwrap().custody.is_none());
    assert!(!operation.lock().unwrap().preparation.recorded);
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_drain_that_settles_after_an_unknown_reply_still_records_the_strong_noexec_fact() {
    let (handle, facts, prepare, actor) = closed_restart::fixture(8107);
    let binding = admitted_binding(&run(&handle.ctx, &facts, Request::prepare(prepare), actor));
    facts.processes.lock().unwrap().remove(&4242);
    let operation = lookup(&handle.ctx, &binding).unwrap();
    let completion = operation
        .lock()
        .unwrap()
        .seal_retirement(Retirement::NotGranted, &facts);
    assert_eq!(finish(&handle.ctx, &operation, completion), NOEXEC);
    let op = operation.lock().unwrap();
    // Reproduce the state seen by the outer worker after the input drain has
    // settled since its inner reply was computed. Text is not terminal evidence.
    assert_eq!(terminal_record(&op, UNKNOWN), Some(TerminalRecord::NoExec));
    assert_eq!(terminal_record(&op, NOEXEC), Some(TerminalRecord::NoExec));
    assert_ne!(terminal_record(&op, UNKNOWN), Some(TerminalRecord::Sealed));
}

#[test]
fn a_precommit_poll_cannot_downgrade_a_sealed_noexec_to_an_opaque_seal() {
    let (handle, facts, prepare, actor) = closed_restart::fixture(8108);
    let ctx = &handle.ctx;
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(prepare), actor.clone()));
    facts.processes.lock().unwrap().remove(&4242);
    let operation = lookup(ctx, &binding).unwrap();
    let completion = operation
        .lock()
        .unwrap()
        .seal_retirement(Retirement::NotGranted, &facts);
    assert_eq!(finish(ctx, &operation, completion), NOEXEC);
    assert!(facts.receipt_history.lock().unwrap().is_empty());
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            actor.clone()
        ),
        UNKNOWN
    );
    assert_eq!(
        facts.journal.lock().unwrap()[&binding.id].state,
        JournalState::NoExec
    );
    assert!(facts.sealed_history.lock().unwrap().is_empty());
    assert_eq!(run(ctx, &facts, Request::Query(binding), actor), NOEXEC);
    assert_eq!(facts.receipt_history.lock().unwrap().len(), 1);
}

#[test]
fn failed_terminal_persistence_withholds_success_and_retry_keeps_the_exact_completion() {
    for case in 0..3 {
        let (handle, facts, binding, actor, launcher) = if case == 0 {
            let (handle, facts, prepare, actor) = closed_restart::fixture(8109);
            let binding = admitted_binding(&run(
                &handle.ctx,
                &facts,
                Request::prepare(prepare),
                actor.clone(),
            ));
            (handle, facts, binding, actor.clone(), actor)
        } else {
            ready(8109 + case)
        };
        let ctx = &handle.ctx;
        let operation = lookup(ctx, &binding).unwrap();
        let prepare = operation.lock().unwrap().request.clone();
        if case > 0 {
            assert!(matches!(
                Reply::parse(&run(
                    ctx,
                    &facts,
                    Request::Execute(binding.clone(), ImagePhase::Forward),
                    launcher
                )),
                Some(Reply::Execute { .. })
            ));
        }
        let request = match case {
            0 => Request::Abort(binding.clone()),
            1 => {
                facts.processes.lock().unwrap().get_mut(&600).unwrap().image = image(9);
                assert_eq!(
                    run(
                        ctx,
                        &facts,
                        Request::Precommit(binding.clone(), ImagePhase::Forward),
                        actor.clone()
                    ),
                    "OK native-act state=precommitted\n"
                );
                Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0+g123".into())
            }
            _ => {
                facts.processes.lock().unwrap().remove(&600);
                facts.foreground.store(400, Ordering::SeqCst);
                Request::Reconcile(binding.clone())
            }
        };
        facts.seal_journal_allowed.store(false, Ordering::SeqCst);
        assert_eq!(run(ctx, &facts, request.clone(), actor.clone()), UNKNOWN);
        let expected = completed_reply(&operation);
        assert_ne!(expected, UNKNOWN);
        let observation = operation.lock().unwrap().completed_observation.clone();
        if case == 1 {
            assert!(
                observation.is_some(),
                "the committed observation must survive the failed seal"
            );
        }
        assert!(facts.sealed_history.lock().unwrap().is_empty());
        facts.seal_journal_allowed.store(true, Ordering::SeqCst);
        for retry in [
            Request::Query(binding.clone()),
            Request::prepare(prepare),
            request,
        ] {
            assert_eq!(run(ctx, &facts, retry, actor.clone()), expected);
            assert_eq!(operation.lock().unwrap().completed_observation, observation);
        }
        assert_eq!(facts.sealed_history.lock().unwrap().len(), 1);
        assert_eq!(
            facts.journal.lock().unwrap()[&binding.id].state,
            JournalState::Sealed
        );
    }
}
