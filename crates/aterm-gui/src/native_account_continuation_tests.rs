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
    account_world(handle, phase, true)
}

fn account_world(
    handle: crate::session_store::SessionHandle,
    phase: ImagePhase,
    stored_rollback: bool,
) -> World {
    let (handle, facts, mut original, caller) = closed_restart::fixture_on(handle);
    let NativePlan::Restart {
        forward, rollback, ..
    } = &mut original.plan
    else {
        unreachable!()
    };
    for (index, command) in [forward.as_mut(), rollback.as_deref_mut().unwrap()]
        .into_iter()
        .enumerate()
    {
        if index == 1 && !stored_rollback {
            continue;
        }
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

#[test]
fn account_lineage_keeps_its_spare_through_three_full_history_handoffs() {
    let mut world = world(8253, ImagePhase::Rollback);
    let ctx = Arc::clone(&world.handle.ctx);
    let mut other_slots = Vec::new();
    for index in 0..MAX_COMPLETED - 2 {
        other_slots.push(
            PrepareSlot::reserve(
                &ctx,
                &format!("other-{index}"),
                0,
                false,
                AccountSlot::Plain,
            )
            .unwrap(),
        );
    }
    assert_eq!(
        ctx.fenced_writes.native.lock().unwrap().slots(),
        MAX_COMPLETED
    );
    assert!(PrepareSlot::reserve(&ctx, "extra", 0, false, AccountSlot::Plain).is_err());
    for id in ["a1", "a2", "a3"] {
        world
            .facts
            .clock
            .store(world.original.deadline_us + 1, Ordering::SeqCst);
        let retired = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .reclaim(world.original.deadline_us + 1);
        assert!(
            retired.is_empty(),
            "waiting NoExec frontier must outlive its deadline"
        );
        let next = candidate(&world, id, ImagePhase::Rollback);
        let binding = admitted_binding(&admit(&world, next.clone(), ImagePhase::Rollback));
        assert_eq!(
            ctx.fenced_writes.native.lock().unwrap().slots(),
            MAX_COMPLETED
        );
        let retired = ctx.fenced_writes.native.lock().unwrap().reclaim(0);
        assert_eq!(
            retired.len(),
            1,
            "acknowledged child replaces its consumed parent before expiry"
        );
        drop(retired);
        assert_eq!(
            run(
                &ctx,
                &world.facts,
                Request::Query(world.binding.clone()),
                world.caller.clone()
            ),
            NOEXEC
        );
        assert_eq!(
            run(
                &ctx,
                &world.facts,
                Request::Reconcile(binding.clone()),
                world.caller.clone()
            ),
            NOEXEC
        );
        world.original = next;
        world.binding = binding;
        let table = ctx.fenced_writes.native.lock().unwrap();
        assert_eq!(table.by_id.len(), 1);
        assert_eq!(table.slots(), MAX_COMPLETED);
    }
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
    drop(other_slots);
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().slots(), 2);
}

#[test]
fn failed_child_append_and_active_receipt_prevent_parent_compaction() {
    for (local, lost_ack) in [(8254, false), (8255, true)] {
        check_retention_until_publication_and_drain(local, lost_ack);
    }
}

#[test]
fn a_non_stored_active_rollback_does_not_pin_the_lineage() {
    let world = account_world(
        crate::session_store::test_handle_on(8256, -1),
        ImagePhase::Rollback,
        false,
    );
    let ctx = &world.handle.ctx;
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().slots(), 2);
    let retired = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .reclaim(world.original.deadline_us + 1);
    assert_eq!(retired.len(), 1);
    drop(retired);
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().slots(), 0);
}

fn check_retention_until_publication_and_drain(local: u64, lost_ack: bool) {
    let world = world(local, ImagePhase::Forward);
    let ctx = &world.handle.ctx;
    let next = candidate(&world, "b1", ImagePhase::Forward);
    world
        .facts
        .prepare_journal_allowed
        .store(lost_ack, Ordering::SeqCst);
    world
        .facts
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    assert_eq!(admit(&world, next.clone(), ImagePhase::Forward), UNKNOWN);
    assert_eq!(
        world.facts.journal.lock().unwrap().contains_key(&next.id),
        lost_ack
    );
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .reclaim(u64::MAX)
            .is_empty()
    );
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().by_id.len(), 2);
    world
        .facts
        .prepare_journal_allowed
        .store(true, Ordering::SeqCst);
    world
        .facts
        .prepare_journal_ack
        .store(true, Ordering::SeqCst);
    let child = admitted_binding(&admit(&world, next.clone(), ImagePhase::Forward));
    let flight = ReceiptFlight::begin(ctx, &Request::Query(world.binding.clone())).unwrap();
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .reclaim(u64::MAX)
            .is_empty()
    );
    drop(flight);
    let parent = lookup(ctx, &world.binding).unwrap();
    let child_operation = lookup(ctx, &child).unwrap();
    let pending = child_operation
        .lock()
        .unwrap()
        .custody
        .as_ref()
        .unwrap()
        .drain();
    let old_drain = parent.lock().unwrap().completion_drain.replace(pending);
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .reclaim(0)
            .is_empty(),
        "an undrained completion still owns its parent evidence"
    );
    parent.lock().unwrap().completion_drain = old_drain;
    drop(parent);
    drop(child_operation);
    let retired = ctx.fenced_writes.native.lock().unwrap().reclaim(0);
    assert_eq!(retired.len(), 1);
    drop(retired);
    assert!(lookup(ctx, &world.binding).is_none());
    assert!(lookup(ctx, &child).is_some());
    let competing = candidate(&world, "b2", ImagePhase::Forward);
    assert_eq!(
        admit(&world, competing.clone(), ImagePhase::Forward),
        UNKNOWN
    );
    assert!(
        !world
            .facts
            .journal
            .lock()
            .unwrap()
            .contains_key(&competing.id)
    );
    assert_eq!(
        admitted_binding(&admit(&world, next, ImagePhase::Forward)),
        child
    );
}
