// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_account_wire::{
    Denial, Predecessor, Relation, Reply as AccountReply,
};

fn relation(w: &World, phase: ImagePhase, id: &str) -> Relation {
    Relation::new(
        Predecessor::new(w.original.clone(), w.binding.clone()).unwrap(),
        phase,
        candidate(w, id, phase),
    )
    .unwrap()
}

fn fence(w: &World, relation: &Relation) -> AccountReply {
    account_fence::fence(&w.handle.ctx, relation, &w.caller, &w.facts)
}

fn claim(w: &World, relation: &Relation) -> AccountReply {
    account_claim::claim(&w.handle.ctx, -1, relation, &w.caller, &w.facts)
}

fn journal_relation(
    relation: &Relation,
) -> aterm_agent::harness::native_receipts::AccountRelation<'_> {
    aterm_agent::harness::native_receipts::AccountRelation {
        predecessor: relation.predecessor().prepare(),
        predecessor_binding: relation.predecessor().binding(),
        phase: relation.phase(),
        candidate: relation.candidate(),
    }
}

fn proof(w: &World) -> Arc<ClosedRestart> {
    if let Some(parent) = lookup(&w.handle.ctx, &w.binding) {
        return parent.lock().unwrap().closed_restart.clone().unwrap();
    }
    let table = w.handle.ctx.fenced_writes.native.lock().unwrap();
    table
        .by_id
        .values()
        .find_map(|operation| {
            let op = operation.lock().unwrap();
            let account = op.authority.account.as_ref()?;
            (account.relation().predecessor().binding() == &w.binding)
                .then(|| account.proof().clone())
        })
        .unwrap()
}

fn assert_fenced(reply: &AccountReply) {
    assert!(matches!(reply, AccountReply::Fenced(_)), "{reply:?}");
}

fn gate() -> (PublicationGate, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::sync_channel(1);
    (
        (tx, Arc::new((Mutex::new(false), std::sync::Condvar::new()))),
        rx,
    )
}

fn release(gate: &PublicationGate) {
    let (lock, ready) = &*gate.1;
    *lock.lock().unwrap() = true;
    ready.notify_all();
}

#[test]
fn typed_fence_is_permanent_and_preserves_the_original_receipt_in_both_phases() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let mut w = world(8900 + u64::from(phase == ImagePhase::Rollback), phase);
        let _home = attach_journal(&mut w);
        let first = relation(&w, phase, "a1");
        let next = relation(&w, phase, "a2");
        let reply = fence(&w, &first);
        assert_fenced(&reply);
        assert_eq!(fence(&w, &first), reply);
        assert!(proof(&w).available());
        assert!(!matches!(claim(&w, &first), AccountReply::Claimed { .. }));
        assert!(matches!(claim(&w, &next), AccountReply::Claimed { .. }));
        assert_eq!(fence(&w, &first), reply);
        assert_eq!(fence(&w, &next), AccountReply::Denied(Denial::Conflict));
    }
}

#[test]
fn typed_fence_cleans_an_unwritten_candidate_and_rejects_its_stale_writer() {
    let mut w = world(8902, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "a3");
    w.facts
        .prepare_journal_allowed
        .store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &first), AccountReply::Unknown);
    let old =
        w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&first.candidate().id].clone();
    assert!(!old.lock().unwrap().preparation.recorded);
    assert_fenced(&fence(&w, &first));
    assert!(
        !w.handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&first.candidate().id)
    );
    w.facts
        .prepare_journal_allowed
        .store(true, Ordering::SeqCst);
    assert_eq!(
        retry_preparation(&w.handle.ctx, -1, old, &w.caller, &w.facts, true),
        UNKNOWN
    );
    assert!(proof(&w).available());
    let next = relation(&w, ImagePhase::Forward, "a4");
    assert!(matches!(claim(&w, &next), AccountReply::Claimed { .. }));
}

#[test]
fn typed_fence_waits_for_a_claim_writer_then_releases_only_that_candidate() {
    for before_insert in [false, true] {
        let mut w = world(8903 + u64::from(before_insert), ImagePhase::Forward);
        let _home = attach_journal(&mut w);
        let first = relation(&w, ImagePhase::Forward, "a5");
        let (pause, entered) = gate();
        if before_insert {
            w.facts.prepare_gate = Some(pause.clone());
        } else {
            w.facts.prepare_journal_gate = Some(pause.clone());
        }
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| claim(&w, &first));
            entered.recv_timeout(PATIENCE).unwrap();
            assert_eq!(fence(&w, &first), AccountReply::Unknown);
            assert!(!proof(&w).available());
            assert!(!proof(&w).confirm_account_successor(first.candidate()));
            assert!(!proof(&w).reserve_account_successor(first.candidate()));
            release(&pause);
            assert_eq!(worker.join().unwrap(), AccountReply::Unknown);
        });
        w.facts.prepare_gate = None;
        w.facts.prepare_journal_gate = None;
        if before_insert {
            let expired = w.original.deadline_us + 1;
            w.facts.clock.store(expired, Ordering::SeqCst);
            let retired = w
                .handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .reclaim(expired);
            drop(retired);
            assert!(lookup(&w.handle.ctx, &w.binding).is_some());
            assert!(
                w.handle
                    .ctx
                    .fenced_writes
                    .native
                    .lock()
                    .unwrap()
                    .account_lineages
                    .pins(&w.binding.id)
            );
        }
        assert_fenced(&fence(&w, &first));
        assert!(proof(&w).available());
        let next = relation(&w, ImagePhase::Forward, "a6");
        assert!(matches!(claim(&w, &next), AccountReply::Claimed { .. }));
        let winning = proof(&w).account_state();
        assert_fenced(&fence(&w, &first));
        assert_eq!(proof(&w).account_state(), winning);
    }
}

#[test]
fn typed_fence_lost_reply_keeps_pending_debt_until_exact_retry() {
    let mut w = world(8905, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "a7");
    w.facts
        .prepare_journal_allowed
        .store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &first), AccountReply::Unknown);
    w.facts.fence_ack.store(false, Ordering::SeqCst);
    assert_eq!(fence(&w, &first), AccountReply::Unknown);
    assert!(!proof(&w).available());
    assert!(
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .contains_operation(&first.candidate().id)
            .unwrap()
    );
    let count = w
        .facts
        .native_journal
        .as_ref()
        .unwrap()
        .record_count()
        .unwrap();
    w.facts.fence_ack.store(true, Ordering::SeqCst);
    let reply = fence(&w, &first);
    assert_fenced(&reply);
    assert_eq!(
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap(),
        count
    );
    assert_eq!(fence(&w, &first), reply);
    assert!(proof(&w).available());
}

#[test]
fn typed_fence_waits_for_native_drain_before_releasing_parent() {
    let mut w = world(8906, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "a8");
    w.facts
        .prepare_journal_allowed
        .store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &first), AccountReply::Unknown);
    let producer = crate::session_store::test_handle_on(8907, -1);
    let boundary = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
        .unwrap();
    let pending = producer
        .ctx
        .fenced_writes
        .reserve_native(first.candidate().revision, "fence-pending-drain", boundary)
        .unwrap();
    let op = w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&first.candidate().id].clone();
    op.lock().unwrap().completion_drain = Some(pending.drain());
    assert_eq!(fence(&w, &first), AccountReply::Unknown);
    assert!(!proof(&w).available());
    drop(pending);
    assert_fenced(&fence(&w, &first));
    assert!(proof(&w).available());
}

#[test]
fn typed_fence_rechecks_identity_after_the_journal_commits() {
    let mut w = world(8908, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "a9");
    let (pause, entered) = gate();
    w.facts.fence_gate = Some(pause.clone());
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| fence(&w, &first));
        entered.recv_timeout(PATIENCE).unwrap();
        w.facts.conversation_valid.store(false, Ordering::SeqCst);
        release(&pause);
        assert_eq!(worker.join().unwrap(), AccountReply::Unknown);
    });
    w.facts.fence_gate = None;
    w.facts.conversation_valid.store(true, Ordering::SeqCst);
    assert_fenced(&fence(&w, &first));
}

#[test]
fn typed_fence_uses_retained_lineage_after_parent_compaction() {
    let mut w = world(8909, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "b9");
    let rival = relation(&w, ImagePhase::Forward, "ba");
    assert!(matches!(claim(&w, &first), AccountReply::Claimed { .. }));
    let retired = w
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .compact_account_lineages();
    drop(retired);
    assert!(
        !w.handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&w.binding.id)
    );
    assert_eq!(fence(&w, &first), AccountReply::Denied(Denial::Conflict));
    assert_fenced(&fence(&w, &rival));
}

#[test]
fn typed_fence_requires_original_identity_and_phase_but_not_execution_authority() {
    let mut w = world(8910, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "ac");
    let wrong_phase = relation(&w, ImagePhase::Rollback, "ad");
    assert_eq!(fence(&w, &wrong_phase), AccountReply::Denied(Denial::Phase));
    let mut caller = w.caller.clone();
    caller.peer = Some(999999);
    assert_eq!(
        account_fence::fence(&w.handle.ctx, &first, &caller, &w.facts),
        AccountReply::Denied(Denial::Authority)
    );
    w.facts.fence_allowed.store(false, Ordering::SeqCst);
    assert_eq!(fence(&w, &first), AccountReply::Unknown);
    assert!(proof(&w).available());
    w.facts.fence_allowed.store(true, Ordering::SeqCst);
    w.facts.account_allowed.store(false, Ordering::SeqCst);
    w.facts
        .clock
        .store(first.candidate().deadline_us + 1, Ordering::SeqCst);
    proof(&w).retire();
    assert_fenced(&fence(&w, &first));
}

fn project_pending(proof: &ClosedRestart, first: &Relation, next: &Relation) -> i64 {
    match proof.account_state().0.as_ref() {
        Some(request) if request == next.candidate() => 3,
        Some(request) if request == first.candidate() => {
            if proof.pending_account_by(request) {
                1
            } else {
                2
            }
        }
        None => 0,
        _ => panic!("unexpected candidate"),
    }
}

fn model_step(
    state: &aterm_spec::interp::State,
    action: &str,
    evidence: Option<&mut aterm_spec::xref::StepEvidence>,
) -> aterm_spec::interp::State {
    let mut w = world(8911, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "ae");
    let next = relation(&w, ImagePhase::Forward, "af");
    let proof = proof(&w);
    let operation = if state["operation"] == 1 {
        w.facts
            .prepare_journal_allowed
            .store(false, Ordering::SeqCst);
        assert_eq!(claim(&w, &first), AccountReply::Unknown);
        Some(w.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&first.candidate().id].clone())
    } else {
        None
    };
    let mut reservation =
        if state["reserved"] == 1 || (state["pending"] == 2 && operation.is_none()) {
            Some(
                PrepareSlot::reserve_relation(
                    &w.handle.ctx,
                    &first.candidate().id,
                    1,
                    false,
                    AccountSlot::Successor(w.binding.id.clone()),
                    Some(&first),
                )
                .unwrap(),
            )
        } else {
            None
        };
    let producer = crate::session_store::test_handle_on(8912, -1);
    let pending_drain = (state["drain"] == 1).then(|| {
        let boundary = aterm_types::update_boundary::Boundary::process()
            .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
            .unwrap();
        producer
            .ctx
            .fenced_writes
            .reserve_native(first.candidate().revision, "model-fence-drain", boundary)
            .unwrap()
    });
    if let Some(pending) = &pending_drain {
        operation.as_ref().unwrap().lock().unwrap().completion_drain = Some(pending.drain());
    }
    let mut writer = operation
        .as_ref()
        .filter(|_| state["writer"] == 1 || state["pending"] == 2)
        .map(|operation| {
            operation.lock().unwrap().preparation =
                Preparation::new(true, Some((w.original.clone(), w.binding.clone())));
            Preparing {
                operation: operation.clone(),
                serial: 1,
                initial_custody: None,
            }
        });
    let claim_next = || match claim(&w, &next) {
        AccountReply::Claimed { binding, .. } => binding,
        reply => panic!("expected next claim: {reply:?}"),
    };
    let mut next_binding = (state["next_before"] == 1).then(claim_next);
    if state["disk"] == 2 {
        assert!(w.facts.fence_account_claim(&first).is_some());
    } else if state["disk"] == 1 {
        let binding = operation.as_ref().unwrap().lock().unwrap().binding.clone();
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .claim_account(journal_relation(&first), &binding)
            .unwrap();
    }
    if state["next_disk"] == 1 && next_binding.is_none() {
        next_binding = Some(claim_next());
    }
    if state["pending"] == 2 {
        assert_eq!(fence(&w, &first), AccountReply::Unknown);
    }
    if state["writer"] == 0 {
        drop(writer.take());
    }
    if state["reserved"] == 0 {
        drop(reservation.take());
    }
    let _next_reservation = if state["pending"] == 3 {
        assert!(proof.reserve_account_successor(next.candidate()));
        Some(
            PrepareSlot::reserve_relation(
                &w.handle.ctx,
                &next.candidate().id,
                1,
                false,
                AccountSlot::Successor(w.binding.id.clone()),
                Some(&next),
            )
            .unwrap(),
        )
    } else {
        None
    };
    assert_eq!(project_pending(&proof, &first, &next), state["pending"]);
    if action == "Reclaim" {
        let invoke = || {
            let retired = w
                .handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .reclaim(w.original.deadline_us + 1);
            drop(retired);
        };
        if let Some(evidence) = evidence {
            evidence.step("Reclaim", invoke);
        } else {
            invoke();
        }
        let mut result = state.clone();
        result.insert("gc", 1);
        result.insert(
            "reclaimed",
            i64::from(lookup(&w.handle.ctx, &w.binding).is_none()),
        );
        return result;
    }
    assert_eq!(action, "Settle");
    let invoke = || fence(&w, &first);
    let reply = if let Some(evidence) = evidence {
        evidence.step("Settle", invoke)
    } else {
        invoke()
    };
    let mut result = state.clone();
    result.insert("checked", 1);
    result.insert("ack", i64::from(matches!(reply, AccountReply::Fenced(_))));
    result.insert("pending", project_pending(&proof, &first, &next));
    result.insert(
        "operation",
        i64::from(
            w.handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .by_id
                .contains_key(&first.candidate().id),
        ),
    );
    result.insert(
        "lost_next",
        i64::from(state["pending"] == 3 && result["pending"] != 3),
    );
    let next_recorded = next_binding.as_ref().is_some_and(|binding| {
        w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .recover_account(journal_relation(&next), binding)
            .unwrap()
            .is_some_and(|record| {
                matches!(
                    record.outcome,
                    aterm_agent::harness::native_receipts::AccountOutcome::Claimed(_)
                )
            })
    });
    result.insert("next_disk", i64::from(next_recorded));
    if next_recorded {
        assert!(proof.confirmed_successor(next.candidate()));
        let table = w.handle.ctx.fenced_writes.native.lock().unwrap();
        assert!(table.by_id.contains_key(&next.candidate().id));
        assert!(table.account_lineages.pins(&next.candidate().id));
    }
    if state["pending"] == 3 {
        let table = w.handle.ctx.fenced_writes.native.lock().unwrap();
        assert!(table.account_lineages.pins(&next.candidate().id));
        assert!(table.reservations.contains_key(&next.candidate().id));
    }
    result
}

pub(crate) fn fence_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    model_step(state, action, None)
}

#[test]
fn typed_fence_refines_pending_cleanup_and_rejects_premature_release() {
    let model = aterm_models_harness::account_fence_model();
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    let states = aterm_spec::interp::reachable_states(&model);
    let mut evidence = aterm_spec::xref::StepEvidence::new("AccountFence");
    let mut negative_control = false;
    for state in &states {
        if model.action_enabled("Reclaim", state) {
            let actual = model_step(state, "Reclaim", Some(&mut evidence));
            assert!(
                model.successors("Reclaim", state).contains(&actual),
                "{state:?} -> {actual:?}"
            );
            let mut evicted = actual.clone();
            evicted.insert("reclaimed", 1);
            assert!(!model.check_invariant("PendingRetained", &evicted));
        }
        if !model.action_enabled("Settle", state) {
            continue;
        }
        let actual = model_step(state, "Settle", Some(&mut evidence));
        assert!(
            model.successors("Settle", state).contains(&actual),
            "{state:?} -> {actual:?}"
        );
        if state["writer"] == 1 || state["drain"] == 1 || state["reserved"] == 1 {
            let mut premature = actual.clone();
            premature.insert("ack", 1);
            assert!(!model.successors("Settle", state).contains(&premature));
            negative_control = true;
        }
        if state["next_claimed"] == 1 {
            let mut lost = actual.clone();
            lost.insert("next_disk", 0);
            assert!(!model.check_invariant("NextPermanent", &lost));
        }
    }
    assert!(negative_control);
    assert!(evidence.audit().is_complete());
    let busy = states
        .iter()
        .find(|state| model.action_enabled("Settle", state) && state["writer"] == 1)
        .unwrap();
    let mut premature = fence_transition(busy, "Settle");
    premature.insert("ack", 1);
    assert!(!model.check_invariant("WriterDrained", &premature));
}

#[test]
fn typed_fence_preserves_a_durable_unacknowledged_claim_after_retirement() {
    let mut w = world(8913, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "b1");
    w.facts.prepare_journal_ack.store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &first), AccountReply::Unknown);
    let retained = proof(&w);
    retained.retire();
    assert_eq!(fence(&w, &first), AccountReply::Unknown);
    assert!(retained.recoverable_account(first.candidate()));
    assert!(!retained.available());
    w.facts.prepare_journal_ack.store(true, Ordering::SeqCst);
    assert!(matches!(claim(&w, &first), AccountReply::Claimed { .. }));
    assert!(retained.confirmed_successor(first.candidate()));
    assert_eq!(fence(&w, &first), AccountReply::Denied(Denial::Conflict));
}

#[test]
fn typed_fence_rejects_a_different_recipe_using_the_pending_id_before_append() {
    let mut w = world(8914, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "b2");
    w.facts
        .prepare_journal_allowed
        .store(false, Ordering::SeqCst);
    assert_eq!(claim(&w, &first), AccountReply::Unknown);
    let mut changed = first.candidate().clone();
    changed.deadline_us += 1;
    let changed = Relation::new(first.predecessor().clone(), first.phase(), changed).unwrap();
    assert_eq!(fence(&w, &changed), AccountReply::Denied(Denial::Conflict));
    assert!(
        !w.facts
            .native_journal
            .as_ref()
            .unwrap()
            .contains_operation(&first.candidate().id)
            .unwrap()
    );
    assert_fenced(&fence(&w, &first));
}

#[test]
fn typed_fence_does_not_confuse_a_plain_reservation_with_an_account_successor() {
    let mut w = world(8915, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "b3");
    let next = relation(&w, ImagePhase::Forward, "b4");
    assert_fenced(&fence(&w, &first));
    let plain = PrepareSlot::reserve(
        &w.handle.ctx,
        &first.candidate().id,
        1,
        false,
        AccountSlot::Plain,
    )
    .unwrap();
    let retained = proof(&w);
    assert!(retained.reserve_account_successor(next.candidate()));
    let account = PrepareSlot::reserve_relation(
        &w.handle.ctx,
        &next.candidate().id,
        1,
        false,
        AccountSlot::Successor(w.binding.id.clone()),
        Some(&next),
    )
    .unwrap();
    assert_eq!(fence(&w, &first), AccountReply::Denied(Denial::Conflict));
    assert!(retained.pending_account_by(next.candidate()));
    drop(plain);
    drop(account);
}

#[test]
fn typed_fence_excludes_new_id_owners_during_unlocked_journal_io() {
    let mut w = world(8916, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "b5");
    let (pause, entered) = gate();
    w.facts.fence_gate = Some(pause.clone());
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| fence(&w, &first));
        entered.recv_timeout(PATIENCE).unwrap();
        assert!(
            PrepareSlot::reserve(
                &w.handle.ctx,
                &first.candidate().id,
                1,
                false,
                AccountSlot::Plain
            )
            .is_err()
        );
        assert_eq!(fence(&w, &first), AccountReply::Unknown);
        release(&pause);
        assert_fenced(&worker.join().unwrap());
    });
}

#[test]
fn typed_fence_preserves_refusals_and_pins_an_inherited_witness() {
    let mut w = world(8917, ImagePhase::Forward);
    let _home = attach_journal(&mut w);
    let first = relation(&w, ImagePhase::Forward, "b6");
    let rival = relation(&w, ImagePhase::Forward, "b7");
    assert!(matches!(claim(&w, &first), AccountReply::Claimed { .. }));
    let retired = w
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .compact_account_lineages();
    drop(retired);
    let mut caller = w.caller.clone();
    caller.peer = Some(999999);
    assert_eq!(
        account_fence::fence(&w.handle.ctx, &rival, &caller, &w.facts),
        AccountReply::Denied(Denial::Authority)
    );
    let wrong_phase = relation(&w, ImagePhase::Rollback, "b8");
    assert_eq!(fence(&w, &wrong_phase), AccountReply::Denied(Denial::Phase));
    let mut wrong_cap = w.binding.clone();
    wrong_cap.capability = "ff".repeat(32);
    let wrong = Relation::new(
        Predecessor::new(w.original.clone(), wrong_cap).unwrap(),
        ImagePhase::Forward,
        rival.candidate().clone(),
    )
    .unwrap();
    assert_eq!(fence(&w, &wrong), AccountReply::Denied(Denial::Authority));
    let (pause, entered) = gate();
    w.facts.fence_gate = Some(pause.clone());
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| fence(&w, &rival));
        entered.recv_timeout(PATIENCE).unwrap();
        let removed = {
            let mut table = w.handle.ctx.fenced_writes.native.lock().unwrap();
            assert!(table.receipt_flights.contains_key(&first.candidate().id));
            // Force a registry loss beyond ordinary GC, which the pin prevents.
            // Even a retained Arc must not hide that loss at acknowledgement.
            table.by_id.remove(&first.candidate().id)
        };
        release(&pause);
        assert_eq!(worker.join().unwrap(), AccountReply::Unknown);
        drop(removed);
    });
}

#[test]
fn typed_fence_replays_preserve_a_concurrently_preparing_successor() {
    for before_insert in [false, true] {
        let mut w = world(8918 + u64::from(before_insert), ImagePhase::Forward);
        let _home = attach_journal(&mut w);
        let first = relation(&w, ImagePhase::Forward, "bb");
        let next = relation(&w, ImagePhase::Forward, "bc");
        let receipt = fence(&w, &first);
        assert_fenced(&receipt);
        let retained = proof(&w);
        let (pause, entered) = gate();
        if before_insert {
            w.facts.prepare_gate = Some(pause.clone());
        } else {
            w.facts.prepare_journal_gate = Some(pause.clone());
        }
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| claim(&w, &next));
            entered.recv_timeout(PATIENCE).unwrap();
            assert_eq!(fence(&w, &first), receipt);
            assert_eq!(fence(&w, &first), receipt);
            assert!(
                w.handle
                    .ctx
                    .fenced_writes
                    .native
                    .lock()
                    .unwrap()
                    .account_lineages
                    .pins(&next.candidate().id)
            );
            release(&pause);
            assert!(matches!(
                worker.join().unwrap(),
                AccountReply::Claimed { .. }
            ));
        });
        w.facts.prepare_gate = None;
        w.facts.prepare_journal_gate = None;
        assert!(retained.confirmed_successor(next.candidate()));
        assert_eq!(fence(&w, &first), receipt);
        assert!(retained.confirmed_successor(next.candidate()));
    }
}
