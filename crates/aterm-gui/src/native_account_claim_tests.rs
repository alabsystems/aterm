// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_account_wire::{
    ChildState, Predecessor, Relation, Reply as AccountReply,
};
use aterm_agent::harness::native_receipts::testing::Append;
use aterm_spec::{interp::State, xref::StepEvidence};

fn relation(w: &World, phase: ImagePhase, id: &str) -> Relation {
    Relation::new(
        Predecessor::new(w.original.clone(), w.binding.clone()).unwrap(),
        phase,
        candidate(w, id, phase),
    )
    .unwrap()
}

fn claim(w: &World, relation: &Relation) -> AccountReply {
    account_claim::claim(&w.handle.ctx, -1, relation, &w.caller, &w.facts)
}

fn claimed(reply: AccountReply, expected: ChildState) -> Binding {
    let AccountReply::Claimed {
        binding,
        state,
        receipt,
    } = reply
    else {
        panic!("expected a durable claim")
    };
    assert_eq!(state, expected);
    assert_eq!(receipt.sequence(), 2);
    binding
}

#[test]
fn typed_claim_admits_once_and_preserves_both_phases_on_retry() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let mut w = world(8700 + u64::from(phase == ImagePhase::Rollback), phase);
        let _home = attach_journal(&mut w);
        let relation = relation(&w, phase, "d1");
        let first = claim(&w, &relation);
        let binding = claimed(first.clone(), ChildState::Prepared);
        assert_eq!(claim(&w, &relation), first);
        assert_eq!(
            lookup(&w.handle.ctx, &binding)
                .unwrap()
                .lock()
                .unwrap()
                .phase,
            Phase::Started
        );
        assert_eq!(
            w.facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            3
        );
    }
}

#[test]
fn typed_claim_finishes_lost_ack_on_the_same_candidate() {
    let mut w = world(8710, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let relation = relation(&w, ImagePhase::Forward, "d2");
    w.facts.prepare_journal_ack.store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &relation), AccountReply::Unknown);
    let child =
        w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&relation.candidate().id].clone();
    let binding = child.lock().unwrap().binding.clone();
    assert_eq!(child.lock().unwrap().phase, Phase::Queued);
    w.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
    assert_eq!(claimed(claim(&w, &relation), ChildState::Prepared), binding);
    assert_eq!(child.lock().unwrap().phase, Phase::Started);
    assert_eq!(
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap(),
        3
    );
}

#[test]
fn typed_claim_closes_expired_or_revoked_admissions_without_losing_identity() {
    for revoked in [false, true] {
        let mut w = world(8720 + u64::from(revoked), ImagePhase::Forward);
        let _home = attach_journal(&mut w);
        let relation = relation(&w, ImagePhase::Forward, "d3");
        let initial = claim(&w, &relation);
        let binding = claimed(initial.clone(), ChildState::Prepared);
        let AccountReply::Claimed {
            receipt: original_receipt,
            ..
        } = initial
        else {
            unreachable!()
        };
        if revoked {
            w.facts.account_allowed.store(false, Ordering::SeqCst);
        } else {
            w.facts
                .clock
                .store(relation.candidate().deadline_us, Ordering::SeqCst);
        }
        let terminal = claim(&w, &relation);
        let AccountReply::Claimed { receipt, .. } = &terminal else {
            unreachable!()
        };
        assert_eq!(*receipt, original_receipt);
        assert_eq!(
            claimed(terminal.clone(), ChildState::NoExec(ImagePhase::Forward)),
            binding
        );
        assert_eq!(claim(&w, &relation), terminal);
        assert_eq!(
            w.facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            4
        );
    }
}

#[test]
fn typed_claim_does_not_cancel_an_inflight_admission() {
    let mut w = world(8730, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let relation = relation(&w, ImagePhase::Forward, "d4");
    let (entered, receiver) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    w.facts.prepare_journal_gate = Some((entered, Arc::clone(&gate)));
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| claim(&w, &relation));
        receiver.recv_timeout(PATIENCE).unwrap();
        let child = w.handle.ctx.fenced_writes.native.lock().unwrap().by_id
            [&relation.candidate().id]
            .clone();
        let binding = child.lock().unwrap().binding.clone();
        assert_eq!(claim(&w, &relation), AccountReply::Unknown);
        assert_eq!(child.lock().unwrap().phase, Phase::Queued);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert_eq!(
            claimed(writer.join().unwrap(), ChildState::Prepared),
            binding
        );
    });
    assert_eq!(
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap(),
        3
    );
}

#[test]
fn typed_claim_recovers_terminal_history_without_readmitting() {
    for state in [
        ChildState::NoExec(ImagePhase::Rollback),
        ChildState::MayExecute,
        ChildState::Sealed { may_execute: false },
    ] {
        let mut w = world(
            8740 + u64::from(state == ChildState::MayExecute),
            ImagePhase::Rollback,
        );
        let _home = attach_journal(&mut w);
        let relation = relation(&w, ImagePhase::Rollback, "d5");
        w.facts.prepare_journal_ack.store(false, Ordering::SeqCst);
        assert_eq!(claim(&w, &relation), AccountReply::Unknown);
        let child = w.handle.ctx.fenced_writes.native.lock().unwrap().by_id
            [&relation.candidate().id]
            .clone();
        let binding = child.lock().unwrap().binding.clone();
        let journal = w.facts.native_journal.as_ref().unwrap();
        journal
            .record(
                relation.candidate(),
                &binding,
                match state {
                    ChildState::NoExec(phase) => Append::NoExecInPhase(phase),
                    ChildState::MayExecute => Append::MayExecute,
                    _ => Append::Seal,
                },
            )
            .unwrap();
        w.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
        assert_eq!(claimed(claim(&w, &relation), state), binding);
        assert!(matches!(child.lock().unwrap().phase, Phase::Completed(_)));
        assert_eq!(journal.record_count().unwrap(), 4);
    }
}

#[test]
fn typed_claim_rechecks_prepared_state_after_unlocked_observation() {
    let mut w = world(8750, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let relation = relation(&w, ImagePhase::Forward, "d6");
    let binding = claimed(claim(&w, &relation), ChildState::Prepared);
    let child = lookup(&w.handle.ctx, &binding).unwrap();
    let (entered, receiver) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    w.facts.receipt_gate = Some((entered, Arc::clone(&gate)));
    std::thread::scope(|scope| {
        let observer = scope.spawn(|| claim(&w, &relation));
        receiver.recv_timeout(PATIENCE).unwrap();
        assert!(w.handle.ctx.meta.try_lock().is_ok());
        assert!(w.handle.ctx.fenced_writes.native.try_lock().is_ok());
        child.try_lock().unwrap().grant_record = GrantRecord::Recorded;
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .record(relation.candidate(), &binding, Append::MayExecute)
            .unwrap();
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert_eq!(observer.join().unwrap(), AccountReply::Unknown);
    });
    assert_eq!(
        claimed(claim(&w, &relation), ChildState::MayExecute),
        binding
    );
}

#[test]
fn typed_claim_rechecks_live_authority_after_unlocked_observation() {
    for revoked in [false, true] {
        let mut w = world(8752 + u64::from(revoked), ImagePhase::Forward);
        let _home = attach_journal(&mut w);
        let relation = relation(&w, ImagePhase::Forward, "da");
        let binding = claimed(claim(&w, &relation), ChildState::Prepared);
        let (entered, receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        w.facts.receipt_gate = Some((entered, Arc::clone(&gate)));
        std::thread::scope(|scope| {
            let observer = scope.spawn(|| claim(&w, &relation));
            receiver.recv_timeout(PATIENCE).unwrap();
            assert!(w.handle.ctx.meta.try_lock().is_ok());
            assert!(w.handle.ctx.fenced_writes.native.try_lock().is_ok());
            if revoked {
                w.facts.account_allowed.store(false, Ordering::SeqCst);
            } else {
                w.facts
                    .clock
                    .store(relation.candidate().deadline_us, Ordering::SeqCst);
            }
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
            assert_eq!(observer.join().unwrap(), AccountReply::Unknown);
        });
        w.facts.receipt_gate = None;
        assert_eq!(
            claimed(
                claim(&w, &relation),
                ChildState::NoExec(ImagePhase::Forward)
            ),
            binding
        );
        assert_eq!(
            w.facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            4
        );
    }
}

#[test]
fn typed_claim_cannot_acknowledge_a_refused_or_unwritten_admission() {
    for refused in [false, true] {
        let mut w = world(8754 + u64::from(refused), ImagePhase::Forward);
        let _home = attach_journal(&mut w);
        let relation = relation(&w, ImagePhase::Forward, "db");
        if refused {
            w.facts.account_allowed.store(false, Ordering::SeqCst);
        } else {
            w.facts
                .prepare_journal_allowed
                .store(false, Ordering::SeqCst);
        }
        assert_eq!(claim(&w, &relation), AccountReply::Unknown);
        assert_eq!(
            w.facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            2
        );
        assert_eq!(w.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn typed_claim_never_settles_or_readmits_a_dispatched_candidate() {
    for mode in 0..4 {
        let mut w = world(8770 + mode, ImagePhase::Forward);
        let _home = attach_journal(&mut w);
        let relation = relation(&w, ImagePhase::Forward, "dc");
        let binding = claimed(claim(&w, &relation), ChildState::Prepared);
        let child = lookup(&w.handle.ctx, &binding).unwrap();
        let snapshot = w
            .handle
            .ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us());
        {
            let mut op = child.lock().unwrap();
            match mode {
                0 => {
                    op.signal_sent = true;
                    op.signal_accepted = Some(true);
                }
                1 => op.resume_sent = true,
                2 => {
                    let input = Arc::new(NativeInput::new(
                        &w.handle.ctx,
                        op.custody.as_ref().unwrap().clone(),
                        w.handle.ctx.human_input.seq(),
                        snapshot,
                        relation.candidate().deadline_us,
                        Arc::new(AtomicBool::new(false)),
                        Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
                        Clock::Facts(Arc::clone(&w.facts.clock)),
                    ));
                    input.seal();
                    op.resume_attempt = Some(input);
                }
                _ => {
                    let process = process(600, 400, 600, 400, 20);
                    op.children[0] = Some(LaunchChild {
                        death: Arc::new(SharedDeath {
                            watch: DeathWatch::Unknown,
                            outcome: std::sync::OnceLock::new(),
                        }),
                        process,
                        connection: 80,
                        execute_sent: false,
                        exec_errno: None,
                    });
                }
            }
        }
        assert_eq!(claim(&w, &relation), AccountReply::Unknown);
        w.facts.account_allowed.store(false, Ordering::SeqCst);
        assert_eq!(claim(&w, &relation), AccountReply::Unknown);
        let op = child.lock().unwrap();
        assert_eq!(op.phase, Phase::Started);
        assert_eq!(op.grant_record, GrantRecord::None);
        assert!(op.closed_restart.is_none());
        assert_eq!(
            w.facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            3
        );
    }
}

#[test]
fn typed_claim_retries_a_failed_noexec_publication_with_the_same_binding() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let mut w = world(8780 + u64::from(phase == ImagePhase::Rollback), phase);
        let _home = attach_journal(&mut w);
        let relation = relation(&w, phase, "dd");
        let binding = claimed(claim(&w, &relation), ChildState::Prepared);
        w.facts.account_allowed.store(false, Ordering::SeqCst);
        w.facts.receipt_allowed.store(false, Ordering::SeqCst);
        assert_eq!(claim(&w, &relation), AccountReply::Unknown);
        assert_eq!(
            w.facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            3
        );
        w.facts.receipt_allowed.store(true, Ordering::SeqCst);
        assert_eq!(
            claimed(claim(&w, &relation), ChildState::NoExec(phase)),
            binding
        );
        assert_eq!(
            w.facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            4
        );
    }
}

#[test]
fn typed_claim_waits_for_source_quiescence_before_invalidating_rollback() {
    let mut w = world(8782, ImagePhase::Rollback);
    let _home = attach_journal(&mut w);
    let relation = relation(&w, ImagePhase::Rollback, "de");
    let binding = claimed(claim(&w, &relation), ChildState::Prepared);
    let NativePlan::Restart { process, .. } = &w.original.plan else {
        unreachable!()
    };
    w.facts
        .processes
        .lock()
        .unwrap()
        .insert(process.pid, process.clone());
    w.facts.account_allowed.store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &relation), AccountReply::Unknown);
    assert_eq!(
        lookup(&w.handle.ctx, &binding)
            .unwrap()
            .lock()
            .unwrap()
            .phase,
        Phase::Started
    );
    w.facts.processes.lock().unwrap().remove(&process.pid);
    assert_eq!(
        claimed(
            claim(&w, &relation),
            ChildState::NoExec(ImagePhase::Rollback)
        ),
        binding
    );
    w.facts.account_allowed.store(true, Ordering::SeqCst);
    let next = relation
        .candidate()
        .with_account_successor(
            ImagePhase::Rollback,
            "ef".repeat(16),
            w.handle
                .ctx
                .meta
                .lock()
                .unwrap()
                .supervisor_authority(crate::metrics::now_us())
                .revision(),
            w.handle.ctx.human_input.seq(),
            w.facts.clock.load(Ordering::SeqCst) + MAX_PREPARE_US,
            intent("/fixture/next"),
        )
        .unwrap();
    let next_relation = Relation::new(
        Predecessor::new(relation.candidate().clone(), binding).unwrap(),
        ImagePhase::Rollback,
        next,
    )
    .unwrap();
    let next_reply = claim(&w, &next_relation);
    assert!(
        next_reply.acknowledges(&aterm_agent::harness::native_account_wire::Request::Claim(
            next_relation
        ))
    );
    assert!(matches!(
        next_reply,
        AccountReply::Claimed {
            state: ChildState::Prepared,
            ..
        }
    ));
}

#[test]
fn typed_claim_keeps_exact_recovery_after_parent_compaction_and_rejects_a_rival() {
    let mut w = world(8783, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let relation = relation(&w, ImagePhase::Forward, "ed");
    let first = claim(&w, &relation);
    let _ = claimed(first.clone(), ChildState::Prepared);
    drop(w.handle.ctx.fenced_writes.native.lock().unwrap().reclaim(0));
    assert!(lookup(&w.handle.ctx, &w.binding).is_none());
    assert_eq!(claim(&w, &relation), first);
    let rival = Relation::new(
        relation.predecessor().clone(),
        ImagePhase::Forward,
        candidate(&w, "ee", ImagePhase::Forward),
    )
    .unwrap();
    assert_eq!(claim(&w, &rival), AccountReply::Unknown);
    assert!(
        !w.handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&rival.candidate().id)
    );
    assert_eq!(
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap(),
        3
    );
}

#[test]
fn typed_claim_checks_exact_parent_binding_and_original_actor() {
    let mut w = world(8784, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let relation = relation(&w, ImagePhase::Forward, "ec");
    let original = claim(&w, &relation);
    let _ = claimed(original.clone(), ChildState::Prepared);
    let mut incorrect = w.binding.clone();
    incorrect.capability = "00".repeat(32);
    let incorrect = Relation::new(
        Predecessor::new(w.original.clone(), incorrect).unwrap(),
        ImagePhase::Forward,
        relation.candidate().clone(),
    )
    .unwrap();
    assert!(matches!(claim(&w, &incorrect), AccountReply::Denied(_)));
    let mut caller = w.caller.clone();
    caller.peer = Some(999999);
    assert!(matches!(
        account_claim::claim(&w.handle.ctx, -1, &relation, &caller, &w.facts),
        AccountReply::Denied(_)
    ));
    assert_eq!(claim(&w, &relation), original);
    assert_eq!(
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap(),
        3
    );
}

#[test]
fn typed_claim_refreshes_queued_prepared_history_before_any_readmission() {
    for sealed in [false, true] {
        let mut w = world(8785 + u64::from(sealed), ImagePhase::Forward);
        let _home = attach_journal(&mut w);
        let relation = relation(&w, ImagePhase::Forward, "eb");
        let NativePlan::Restart { process, .. } = &w.original.plan else {
            unreachable!()
        };
        let (entered, receiver) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        w.facts.prepare_journal_gate = Some((entered, Arc::clone(&gate)));
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| claim(&w, &relation));
            receiver.recv_timeout(PATIENCE).unwrap();
            w.facts
                .processes
                .lock()
                .unwrap()
                .insert(process.pid, process.clone());
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
            assert_eq!(writer.join().unwrap(), AccountReply::Unknown);
        });
        w.facts.prepare_journal_gate = None;
        let child = w.handle.ctx.fenced_writes.native.lock().unwrap().by_id
            [&relation.candidate().id]
            .clone();
        let binding = child.lock().unwrap().binding.clone();
        assert_eq!(child.lock().unwrap().phase, Phase::Queued);
        assert!(child.lock().unwrap().preparation.recorded);
        w.facts.processes.lock().unwrap().remove(&process.pid);
        w.facts.prepare_journal_ack.store(false, Ordering::SeqCst);
        assert_eq!(claim(&w, &relation), AccountReply::Unknown);
        assert_eq!(child.lock().unwrap().phase, Phase::Queued);
        let state = if sealed {
            ChildState::Sealed { may_execute: false }
        } else {
            ChildState::MayExecute
        };
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .record(
                relation.candidate(),
                &binding,
                if sealed {
                    Append::Seal
                } else {
                    Append::MayExecute
                },
            )
            .unwrap();
        w.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
        assert_eq!(claimed(claim(&w, &relation), state), binding);
        assert_eq!(
            child.lock().unwrap().phase,
            Phase::Completed(Decision::Unknown)
        );
        assert_eq!(w.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
    }
}

fn readiness_step(state: &State, evidence: Option<&mut StepEvidence>) -> State {
    let w = world(8760, ImagePhase::Forward);
    let relation = relation(&w, ImagePhase::Forward, "d7");
    w.facts.prepare_journal_ack.store(
        state["confirmed"] == 1 && state["lifecycle"] != 4,
        Ordering::SeqCst,
    );
    let _ = admit(&w, relation.candidate().clone(), ImagePhase::Forward);
    let child =
        w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&relation.candidate().id].clone();
    if state["lifecycle"] == 4 {
        w.facts
            .journal
            .lock()
            .unwrap()
            .get_mut(&relation.candidate().id)
            .unwrap()
            .state = JournalState::MayExecute;
        w.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
        let _ = admit(&w, relation.candidate().clone(), ImagePhase::Forward);
        assert!(
            child
                .lock()
                .unwrap()
                .preparation
                .terminal_already_recorded()
        );
    }
    let mut caller = w.caller.clone();
    let completion = {
        let mut op = child.lock().unwrap();
        op.preparation.recorded = true;
        match state["lifecycle"] {
            0 => {
                op.phase = Phase::Started;
                None
            }
            1 => {
                op.phase = Phase::Queued;
                None
            }
            2 => op.seal_retirement(Retirement::NotGranted, &w.facts),
            _ => op.seal_completion(Decision::Unknown),
        }
    };
    if completion.is_some() {
        let _ = finish(&w.handle.ctx, &child, completion);
    }
    // Keep real custody alive across the predicate. A separate context allows
    // the model to inject an outstanding drain even into otherwise inconsistent
    // snapshots; the real context correctly refuses a second native operation.
    let pending = (state["drained"] == 0).then(|| {
        let producer = crate::session_store::test_handle_on(8762, -1);
        let boundary = aterm_types::update_boundary::Boundary::process()
            .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
            .unwrap();
        producer
            .ctx
            .fenced_writes
            .reserve_native(
                relation.candidate().revision,
                "pending-claim-fixture",
                boundary,
            )
            .unwrap()
    });
    {
        let mut op = child.lock().unwrap();
        if state["writer"] == 1 {
            op.preparation =
                preparation::Preparation::new(true, Some((w.original.clone(), w.binding.clone())));
        }
        op.preparation.recorded = state["published"] == 1;
        if state["granted"] == 1 {
            op.grant_record = GrantRecord::Recorded;
        }
        if state["phase"] == 0 {
            op.resume_phase = Some(ImagePhase::Rollback);
        }
        op.resume_sent = state["dispatched"] == 1;
        if let Some(pending) = &pending {
            op.completion_drain = Some(pending.drain());
        }
    }
    if state["identity"] == 0 {
        caller.peer = Some(999999);
    }
    if state["current"] == 0 {
        w.facts.account_allowed.store(false, Ordering::SeqCst);
    }
    if state["cohort"] == 0 {
        let NativePlan::Restart { process, .. } = &w.original.plan else {
            unreachable!()
        };
        w.facts
            .processes
            .lock()
            .unwrap()
            .insert(process.pid, process.clone());
    }
    let recorded = match state["state"] {
        0 => ChildState::Prepared,
        1 => ChildState::NoExec(ImagePhase::Forward),
        2 => ChildState::MayExecute,
        _ => ChildState::Sealed { may_execute: false },
    };
    let invoke = || {
        account_claim::ready(
            &w.handle.ctx,
            -1,
            &child,
            &relation,
            &caller,
            &w.facts,
            recorded,
        )
    };
    let ack = if let Some(evidence) = evidence {
        evidence.step("Check", invoke)
    } else {
        invoke()
    };
    let mut result = state.clone();
    result.insert("checked", 1);
    result.insert("ack", i64::from(ack));
    result
}

pub(crate) fn readiness_transition(state: &State, action: &str) -> State {
    assert_eq!(action, "Check");
    readiness_step(state, None)
}

#[test]
fn typed_claim_readiness_refines_live_state_and_rejects_history_as_admission() {
    let model = aterm_models_harness::account_claim_readiness_model();
    // The registry-wide non_vacuity test owns the full sweep; keep its primary mutant explicit.
    assert!(
        aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err(),
        "Buggy=1 must violate an invariant"
    );
    let mut evidence = StepEvidence::new("AccountClaimReadiness");
    for state in aterm_spec::interp::reachable_states(&model) {
        if model.action_enabled("Check", &state) {
            let next = readiness_step(&state, Some(&mut evidence));
            assert!(
                model.successors("Check", &state).contains(&next),
                "{state:?} -> {next:?}"
            );
        }
    }
    assert!(evidence.audit().is_complete());
    let mut w = world(8761, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let relation = relation(&w, ImagePhase::Forward, "d8");
    w.facts.prepare_journal_ack.store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &relation), AccountReply::Unknown);
    w.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
    let historical =
        account_observation::recover_claim(&w.handle.ctx, &relation, &w.caller, &w.facts);
    let mut state = model.init_state();
    state.insert("confirmed", 0);
    state.insert("published", 0);
    state.insert("lifecycle", 1);
    state = readiness_transition(&state, "Check");
    assert_eq!(state["ack"], 0);
    state.insert(
        "ack",
        i64::from(matches!(historical, AccountReply::Claimed { .. })),
    );
    assert!(!model.check_invariant("Published", &state));
    assert!(!model.check_invariant("Confirmed", &state));
}
