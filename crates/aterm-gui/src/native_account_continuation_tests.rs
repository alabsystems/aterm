// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::{accounts, accounts_pool::ExecutionIntent};

struct World {
    handle: crate::session_store::SessionHandle,
    facts: Facts,
    original: Prepare,
    binding: Binding,
    caller: SupervisorInputCaller,
}

fn intent(login: &str) -> ExecutionIntent {
    ExecutionIntent::Stored {
        namespace: "12".repeat(32),
        login: Some(login.into()),
        owner: Some("fixture".into()),
        generation: "34".repeat(32),
    }
}

fn world(local: u64, phase: ImagePhase) -> World {
    world_on(crate::session_store::test_handle_on(local, -1), phase)
}

fn world_on(handle: crate::session_store::SessionHandle, phase: ImagePhase) -> World {
    let (handle, facts, mut original, caller) = closed_restart::fixture_on(handle);
    let NativePlan::Restart {
        forward, rollback, ..
    } = &mut original.plan
    else {
        unreachable!()
    };
    for command in [forward.as_mut(), rollback.as_deref_mut().unwrap()] {
        command.account_intent = Some(Box::new(intent("/fixture/old")));
        command
            .env
            .push((accounts::LOGIN_ENV.into(), "/fixture/old".into()));
    }
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(original.clone()),
        caller.clone(),
    ));
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::Signal(binding.clone()),
            caller.clone()
        ),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    if phase == ImagePhase::Rollback {
        lookup(&handle.ctx, &binding)
            .unwrap()
            .lock()
            .unwrap()
            .resume_phase = Some(phase);
    }
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::Reconcile(binding.clone()),
            caller.clone()
        ),
        NOEXEC
    );
    World {
        handle,
        facts,
        original,
        binding,
        caller,
    }
}

fn candidate(world: &World, id: &str, phase: ImagePhase) -> Prepare {
    let ctx = &world.handle.ctx;
    world
        .original
        .with_account_successor(
            phase,
            id.repeat(16),
            ctx.meta
                .lock()
                .unwrap()
                .supervisor_authority(crate::metrics::now_us())
                .revision(),
            ctx.human_input.seq(),
            world.facts.clock.load(Ordering::SeqCst) + MAX_PREPARE_US,
            intent("/fixture/new"),
        )
        .unwrap()
}

fn admit(world: &World, candidate: Prepare, phase: ImagePhase) -> String {
    account_continuation::prepare_account(
        &world.handle.ctx,
        -1,
        &world.binding,
        candidate,
        phase,
        world.caller.clone(),
        &world.facts,
    )
}

#[test]
fn ordinary_account_child_uses_normal_authority_and_exact_relation() {
    let world = world(8243, ImagePhase::Forward);
    let next = candidate(&world, "f1", ImagePhase::Forward);
    let binding = admitted_binding(&admit(&world, next.clone(), ImagePhase::Forward));
    let operation = lookup(&world.handle.ctx, &binding).unwrap();
    let op = operation.lock().unwrap();
    assert!(op.resume_only);
    assert!(op.authority.renewal.is_none());
    assert!(op.authority.account.is_some());
    assert_eq!(
        op.authority.continuation().unwrap().predecessor,
        world.original
    );
    drop(op);
    assert_eq!(
        admit(&world, next.clone(), ImagePhase::Forward),
        admitted(&binding)
    );
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(next.clone()),
            world.caller.clone()
        ),
        DENIED
    );
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(next.clone()),
            world.caller.clone()
        ),
        DENIED
    );
    assert_eq!(admit(&world, next, ImagePhase::Rollback), DENIED);
    assert_eq!(
        admit(
            &world,
            candidate(&world, "f2", ImagePhase::Forward),
            ImagePhase::Forward
        ),
        DENIED
    );
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Signal(binding),
            world.caller.clone()
        ),
        DENIED
    );
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
}

#[test]
fn ordinary_account_does_not_inherit_renewal_policy_exceptions() {
    let world = world(8244, ImagePhase::Forward);
    // This external claim takes Relaunch away from the builtin harness. A
    // Codex renewal seal may separately authorize it; an account recipe cannot.
    install(&world.handle.ctx, 71, &[Scope::Upgrade, Scope::Relaunch]);
    let next = candidate(&world, "f3", ImagePhase::Forward);
    assert_eq!(admit(&world, next.clone(), ImagePhase::Forward), DENIED);
    assert!(!world.facts.journal.lock().unwrap().contains_key(&next.id));
    assert!(
        lookup(&world.handle.ctx, &world.binding)
            .unwrap()
            .lock()
            .unwrap()
            .closed_restart
            .as_ref()
            .unwrap()
            .available()
    );
}

#[test]
fn ordinary_account_lost_append_ack_retains_exact_child_and_phase() {
    let mut world = world(8245, ImagePhase::Rollback);
    #[cfg(unix)]
    let _journal_home = {
        use aterm_agent::harness::native_receipts::testing::{Append, Journal};
        let home = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        journal
            .record(&world.original, &world.binding, Append::Prepare)
            .unwrap();
        journal
            .record(&world.original, &world.binding, Append::NoExec)
            .unwrap();
        world.facts.native_journal = Some(journal);
        home
    };
    #[cfg(not(unix))]
    let _ = &mut world;
    let next = candidate(&world, "f4", ImagePhase::Rollback);
    world
        .facts
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    assert_eq!(admit(&world, next.clone(), ImagePhase::Rollback), UNKNOWN);
    let operation = world.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&next.id].clone();
    let pending = operation.lock().unwrap().binding.clone();
    assert!(operation.lock().unwrap().custody.is_none());
    assert_eq!(
        operation.lock().unwrap().resume_phase,
        Some(ImagePhase::Rollback)
    );
    assert_eq!(
        admit(
            &world,
            candidate(&world, "f5", ImagePhase::Rollback),
            ImagePhase::Rollback
        ),
        DENIED
    );
    world
        .facts
        .prepare_journal_ack
        .store(true, Ordering::SeqCst);
    assert_eq!(
        admit(&world, next.clone(), ImagePhase::Rollback),
        admitted(&pending)
    );
    assert_eq!(world.facts.prepared_history.lock().unwrap().len(), 2);
    for request in [
        Request::Signal(pending.clone()),
        Request::Resume(pending, ImagePhase::Forward, "1.1".into(), 0),
    ] {
        assert_eq!(
            run(
                &world.handle.ctx,
                &world.facts,
                request,
                world.caller.clone()
            ),
            DENIED
        );
    }
}

#[test]
fn ineligible_account_cannot_consume_the_predecessor() {
    let world = world(8251, ImagePhase::Forward);
    let next = candidate(&world, "fa", ImagePhase::Forward);
    world.facts.account_allowed.store(false, Ordering::SeqCst);
    assert_eq!(admit(&world, next.clone(), ImagePhase::Forward), DENIED);
    assert!(!world.facts.journal.lock().unwrap().contains_key(&next.id));
    assert!(
        lookup(&world.handle.ctx, &world.binding)
            .unwrap()
            .lock()
            .unwrap()
            .closed_restart
            .as_ref()
            .unwrap()
            .available()
    );
    world.facts.account_allowed.store(true, Ordering::SeqCst);
    admitted_binding(&admit(&world, next, ImagePhase::Forward));
}

#[test]
fn an_exact_claim_without_a_published_child_remains_unknown() {
    let world = world(8252, ImagePhase::Forward);
    let next = candidate(&world, "fb", ImagePhase::Forward);
    let proof = lookup(&world.handle.ctx, &world.binding)
        .unwrap()
        .lock()
        .unwrap()
        .closed_restart
        .clone()
        .unwrap();
    // Model a publisher which has chosen this exact child but has not exposed
    // its operation yet. A retry cannot mint a replacement or report rejection.
    assert!(proof.claim_successor(&next));
    assert_eq!(admit(&world, next.clone(), ImagePhase::Forward), UNKNOWN);
    assert!(!world.facts.journal.lock().unwrap().contains_key(&next.id));
    assert_eq!(
        admit(
            &world,
            candidate(&world, "fc", ImagePhase::Forward),
            ImagePhase::Forward
        ),
        DENIED
    );
}

#[cfg(unix)]
#[test]
fn ordinary_rollback_checks_account_at_both_execution_boundaries() {
    use crate::control_input_ack::{InputMark, testing};
    use crate::input::{Delivery, EgressReceipt};
    let (handle, mut reader) = testing::owned_handle(8248);
    let world = world_on(handle, ImagePhase::Rollback);
    let ctx = &world.handle.ctx;
    let binding = admitted_binding(&admit(
        &world,
        candidate(&world, "f8", ImagePhase::Rollback),
        ImagePhase::Rollback,
    ));
    assert_eq!(
        serve_by(
            ctx,
            -1,
            Request::Resume(
                binding.clone(),
                ImagePhase::Rollback,
                "1.1".into(),
                ctx.human_input.seq()
            ),
            world.caller.clone(),
            &world.facts,
            &mut |_| {
                let mark = InputMark::at_epoch(
                    ctx,
                    world.handle.local_id,
                    aterm_slog::Actor::Ctl,
                    crate::session_log::InputVerb::Turn,
                    None,
                    true,
                );
                let producer = mark.native_producer();
                let completion = mark.completion();
                assert!(completion.start_supervised());
                let receipt = ctx.sink.write_frame_with_prefix(b"A").unwrap();
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
            }
        ),
        "OK native-act state=resumed phase=rollback\n"
    );
    assert_eq!(testing::byte(&mut reader), b'A');
    let launcher = SupervisorInputCaller {
        principal: Principal::Instance,
        connection: Some(80),
        peer: Some(600),
        proof: None,
    };
    world
        .facts
        .processes
        .lock()
        .unwrap()
        .insert(600, process(600, 400, 600, 400, 20));
    world.facts.foreground.store(600, Ordering::SeqCst);
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &world.facts,
            Request::Attach(binding.clone(), ImagePhase::Rollback),
            launcher.clone()
        )),
        Some(Reply::Launch { .. })
    ));
    let operation = lookup(ctx, &binding).unwrap();
    let request = Request::Execute(binding, ImagePhase::Rollback);
    world.facts.account_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        serve_launcher_step(
            ctx,
            -1,
            &request,
            &launcher,
            &operation,
            &world.facts,
            GrantStep::Reserve
        ),
        DENIED
    );
    assert_eq!(operation.lock().unwrap().grant_record, GrantRecord::None);
    world.facts.account_allowed.store(true, Ordering::SeqCst);
    assert_eq!(
        serve_launcher_step(
            ctx,
            -1,
            &request,
            &launcher,
            &operation,
            &world.facts,
            GrantStep::Reserve
        ),
        JOURNAL_GRANT
    );
    {
        let mut op = operation.lock().unwrap();
        let GrantRecord::Writing(attempt) = op.grant_record else {
            panic!("grant not reserved")
        };
        assert!(world.facts.record_may_execute(&op.request, &op.binding));
        assert!(op.grant_record.finish(attempt, true));
    }
    world.facts.account_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        serve_launcher_step(
            ctx,
            -1,
            &request,
            &launcher,
            &operation,
            &world.facts,
            GrantStep::Deliver
        ),
        DENIED
    );
    assert!(
        !operation.lock().unwrap().children[1]
            .as_ref()
            .unwrap()
            .execute_sent
    );
    world.facts.account_allowed.store(true, Ordering::SeqCst);
    assert!(matches!(
        Reply::parse(&serve_launcher_step(
            ctx,
            -1,
            &request,
            &launcher,
            &operation,
            &world.facts,
            GrantStep::Deliver
        )),
        Some(Reply::Execute { .. })
    ));
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
}

#[test]
fn account_replay_publishes_terminal_evidence_before_acknowledging_it() {
    let world = world(8249, ImagePhase::Forward);
    let next = candidate(&world, "f9", ImagePhase::Forward);
    let binding = admitted_binding(&admit(&world, next.clone(), ImagePhase::Forward));
    let operation = lookup(&world.handle.ctx, &binding).unwrap();
    // A GUI sweep can close the operation without writing a journal receipt.
    // Exercise the later private-entry replay, not the normal Reconcile wrapper.
    let completion = operation
        .lock()
        .unwrap()
        .seal_retirement(Retirement::NotGranted, &world.facts);
    assert_eq!(finish(&world.handle.ctx, &operation, completion), NOEXEC);
    world.facts.receipt_allowed.store(false, Ordering::SeqCst);
    assert_eq!(admit(&world, next.clone(), ImagePhase::Forward), UNKNOWN);
    assert_eq!(
        world.facts.journal.lock().unwrap()[&next.id].state,
        JournalState::Prepared
    );
    world.facts.receipt_allowed.store(true, Ordering::SeqCst);
    assert_eq!(admit(&world, next.clone(), ImagePhase::Forward), NOEXEC);
    assert_eq!(
        world.facts.journal.lock().unwrap()[&next.id].state,
        JournalState::NoExec
    );
    // The plain reader may have been classified before this account child
    // appeared. Recheck the retained relation after its request returns.
    assert_eq!(
        serve_with_publication(
            &world.handle.ctx,
            Request::prepare(next.clone()),
            &world.facts,
            true,
            |_| DENIED.into()
        ),
        DENIED
    );
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(next.clone()),
            world.caller.clone()
        ),
        DENIED
    );
    assert_eq!(admit(&world, next, ImagePhase::Rollback), DENIED);
    assert!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .receipt_flights
            .is_empty()
    );
}

use aterm_spec::interp::State;

pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut result = state.clone();
    if action == "Check" {
        let mut world = world(8246, ImagePhase::Forward);
        if state["case"] == 8 {
            install(&world.handle.ctx, 71, &[Scope::Upgrade, Scope::Relaunch]);
        }
        let mut next = candidate(&world, "f6", ImagePhase::Forward);
        let mut phase = ImagePhase::Forward;
        match state["case"] {
            0 | 8 => {}
            10 => world.facts.account_allowed.store(false, Ordering::SeqCst),
            3 => world.binding.capability = "bad".into(),
            4 => world.caller.peer = Some(901),
            5 => world
                .facts
                .conversation_valid
                .store(false, Ordering::SeqCst),
            9 => {
                world
                    .facts
                    .processes
                    .lock()
                    .unwrap()
                    .insert(7000, process(7000, 400, 4242, 400, 90));
            }
            7 => phase = ImagePhase::Rollback,
            6 => {
                let NativePlan::Restart { forward, .. } = &mut next.plan else {
                    unreachable!()
                };
                forward.argv.push("--other-conversation".into());
            }
            2 => {
                lookup(&world.handle.ctx, &world.binding)
                    .unwrap()
                    .lock()
                    .unwrap()
                    .grant_record = GrantRecord::Recorded
            }
            1 => {
                lookup(&world.handle.ctx, &world.binding)
                    .unwrap()
                    .lock()
                    .unwrap()
                    .phase = Phase::Completed(Decision::Retired(Retirement::Exited))
            }
            _ => unreachable!(),
        }
        let reply = admit(&world, next, phase);
        result.insert(
            "accepted",
            i64::from(matches!(Reply::parse(&reply), Some(Reply::Admitted(_)))),
        );
        result.insert("checked", 1);
    } else {
        let case = match action {
            "Exited" => 1,
            "Grant" => 2,
            "Capability" => 3,
            "Actor" => 4,
            "Conversation" => 5,
            "Recipe" => 6,
            "Phase" => 7,
            "Policy" => 8,
            "Cohort" => 9,
            "AccountPool" => 10,
            _ => unreachable!("unmapped account admission action"),
        };
        result.insert("case", case);
    }
    result
}

#[test]
fn account_continuation_refines_actual_host_admission() {
    let model = aterm_models_harness::account_continuation_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn treating_a_valid_account_recipe_as_authority_breaks_policy_law() {
    let model = aterm_models_harness::account_continuation_model();
    let world = world(8247, ImagePhase::Forward);
    install(&world.handle.ctx, 71, &[Scope::Upgrade, Scope::Relaunch]);
    let next = candidate(&world, "f7", ImagePhase::Forward);
    let mut state = model.init_state();
    assert!(model.fire("Policy", &mut state));
    // Deliberately substitute only the real structural predicate for the host's
    // authority decision: the former stays true after policy is taken away.
    let accepted = world
        .original
        .account_successor_recipe(&next, ImagePhase::Forward);
    state.insert("checked", 1);
    state.insert("accepted", i64::from(accepted));
    assert!(accepted);
    assert!(!model.check_invariant("OrdinaryPolicy", &state));
}
