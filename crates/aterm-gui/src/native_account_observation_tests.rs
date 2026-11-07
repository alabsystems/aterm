// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_account_wire::{
    Predecessor, Relation, Reply as AccountReply, Request as AccountRequest,
};
use aterm_agent::harness::native_receipts::testing::{Append, Journal};
use aterm_spec::interp::State;
use aterm_spec::xref::StepEvidence;

fn observe(
    world: &World,
    request: &AccountRequest,
    caller: &SupervisorInputCaller,
) -> AccountReply {
    match request {
        AccountRequest::Closure(parent) => {
            account_observation::closure(&world.handle.ctx, parent, caller, &world.facts)
        }
        AccountRequest::Claim(relation) => {
            account_observation::recover_claim(&world.handle.ctx, relation, caller, &world.facts)
        }
        _ => unreachable!(),
    }
}

fn step(state: &State, evidence: Option<&mut StepEvidence>) -> State {
    let mut world = world(8600, ImagePhase::Forward);
    let _home = attach_journal(&mut world);
    let mut next = candidate(&world, "e1", ImagePhase::Forward);
    let child = (state["kind"] == 1)
        .then(|| admitted_binding(&admit(&world, next.clone(), ImagePhase::Forward)));
    let operation = lookup(&world.handle.ctx, child.as_ref().unwrap_or(&world.binding)).unwrap();
    let mut parent = world.original.clone();
    let mut parent_binding = world.binding.clone();
    let mut caller = world.caller.clone();
    let empty_journal = (state["case"] == 12).then(|| aterm_tempfile::tempdir().unwrap());
    match state["case"] {
        1 => parent.revision += 1,
        2 => parent_binding.capability = "fe".repeat(32),
        3 => caller.peer = Some(999999),
        4 => {
            parent.host_epoch = "fe".repeat(16);
            next.host_epoch = parent.host_epoch.clone();
        }
        5 => world
            .facts
            .conversation_valid
            .store(false, Ordering::SeqCst),
        6 => {
            let removed = world
                .handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .by_id
                .remove(&operation.lock().unwrap().request.id);
            drop(removed);
        }
        7 => {
            world.facts.receipt_allowed.store(false, Ordering::SeqCst);
            world.facts.receipt_readable.store(false, Ordering::SeqCst);
        }
        8 => operation.lock().unwrap().preparation.recorded = false,
        9 => {
            let NativePlan::Restart { process, .. } = &parent.plan else {
                unreachable!()
            };
            world
                .facts
                .processes
                .lock()
                .unwrap()
                .insert(process.pid, process.clone());
        }
        10 => world.facts.clock.store(next.deadline_us, Ordering::SeqCst),
        11 => world.facts.account_allowed.store(false, Ordering::SeqCst),
        12 => {
            world.facts.native_journal = Some(Journal::new(empty_journal.as_ref().unwrap().path()));
        }
        13 | 14 => {
            if let Some(binding) = &child {
                let journal = world.facts.native_journal.as_ref().unwrap();
                journal
                    .record(
                        &next,
                        binding,
                        if state["case"] == 13 {
                            Append::MayExecute
                        } else {
                            Append::Seal
                        },
                    )
                    .unwrap();
            } else {
                let mut op = operation.lock().unwrap();
                if state["case"] == 13 {
                    op.grant_record = GrantRecord::Recorded;
                } else {
                    op.phase = Phase::Completed(Decision::Committed);
                }
            }
        }
        15 => {
            let binding = admitted_binding(&admit(&world, next.clone(), ImagePhase::Forward));
            let child = lookup(&world.handle.ctx, &binding).unwrap();
            let drain = child.lock().unwrap().custody.as_ref().unwrap().drain();
            assert!(!drain.is_drained());
            operation.lock().unwrap().completion_drain = Some(drain);
        }
        16 => operation.lock().unwrap().resume_phase = Some(ImagePhase::Rollback),
        _ => {}
    }
    let parent = Predecessor::new(parent, parent_binding).unwrap();
    let request = if state["kind"] == 0 {
        AccountRequest::Closure(parent)
    } else {
        AccountRequest::Claim(Relation::new(parent, ImagePhase::Forward, next).unwrap())
    };
    let run = || observe(&world, &request, &caller);
    let reply = if let Some(evidence) = evidence {
        evidence.step("Check", run)
    } else {
        run()
    };
    if let Some(empty) = empty_journal {
        assert!(std::fs::read_dir(empty.path()).unwrap().next().is_none());
    }
    let mut result = state.clone();
    result.insert("checked", 1);
    result.insert("accepted", i64::from(reply.acknowledges(&request)));
    result
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    assert_eq!(action, "Check");
    step(state, None)
}

#[test]
fn typed_observations_refine_live_identity_and_durable_history() {
    let model = aterm_models_harness::account_host_observation_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let mut evidence = StepEvidence::new("AccountHostObservation");
    for state in aterm_spec::interp::reachable_states(&model) {
        if model.action_enabled("Check", &state) {
            let next = step(&state, Some(&mut evidence));
            assert!(
                model.successors("Check", &state).contains(&next),
                "{state:?} -> {next:?}"
            );
        }
    }
    assert!(evidence.audit().is_complete());
    let mut world = world(8601, ImagePhase::Forward);
    let _home = attach_journal(&mut world);
    let parent = Predecessor::new(world.original.clone(), world.binding.clone()).unwrap();
    // The real journal and reply constructor alone do not establish caller identity.
    let record = world
        .facts
        .record_account_closure(parent.prepare(), parent.binding(), ImagePhase::Forward)
        .unwrap();
    let forged = AccountReply::Closure {
        receipt: aterm_agent::harness::native_account_wire::ClosureReceipt::for_predecessor(
            &parent,
            record.sequence,
            record.recorded_ms,
        ),
        phase: ImagePhase::Forward,
        successor: None,
    };
    let mut wrong_actor = model.init_state();
    wrong_actor.insert("case", 3);
    wrong_actor = transition(&wrong_actor, "Check");
    assert_eq!(wrong_actor["accepted"], 0);
    wrong_actor.insert(
        "accepted",
        i64::from(forged.acknowledges(&AccountRequest::Closure(parent))),
    );
    assert!(!model.check_invariant("ExactActor", &wrong_actor));
}

#[test]
fn closure_finishes_an_unacknowledged_terminal_write_once() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let mut world = world(8605 + u64::from(phase == ImagePhase::Rollback), phase);
        let home = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path());
        journal
            .record(&world.original, &world.binding, Append::Prepare)
            .unwrap();
        world.facts.native_journal = Some(journal);
        let request = AccountRequest::Closure(
            Predecessor::new(world.original.clone(), world.binding.clone()).unwrap(),
        );
        let first = observe(&world, &request, &world.caller);
        assert!(first.acknowledges(&request));
        let AccountReply::Closure {
            receipt,
            phase: actual,
            ..
        } = &first
        else {
            unreachable!()
        };
        assert_eq!(*actual, phase);
        assert_eq!(receipt.sequence(), 1);
        assert_eq!(
            world
                .facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            2
        );
        assert_eq!(observe(&world, &request, &world.caller), first);
        assert_eq!(
            world
                .facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            2
        );
    }
}

#[test]
fn observations_keep_original_provenance_after_later_claims_and_retries() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let mut world = world(8610 + u64::from(phase == ImagePhase::Rollback), phase);
        let _home = attach_journal(&mut world);
        let parent = Predecessor::new(world.original.clone(), world.binding.clone()).unwrap();
        let closed = world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .observe_phase(&world.original, &world.binding)
            .unwrap()
            .unwrap();
        let next = candidate(&world, "e2", phase);
        let binding = admitted_binding(&admit(&world, next.clone(), phase));
        let relation = Relation::new(parent.clone(), phase, next.clone()).unwrap();
        let count = world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap();
        for _ in 0..2 {
            let reply = observe(
                &world,
                &AccountRequest::Closure(parent.clone()),
                &world.caller,
            );
            let AccountReply::Closure {
                receipt,
                phase: actual,
                successor,
            } = reply
            else {
                panic!("missing closure")
            };
            assert_eq!(actual, phase);
            assert_eq!(receipt.sequence(), closed.sequence);
            assert_eq!(receipt.recorded_ms(), closed.recorded_ms);
            assert_eq!(successor.as_deref(), Some(next.id.as_str()));
            let request = AccountRequest::Claim(relation.clone());
            let reply = observe(&world, &request, &world.caller);
            assert!(reply.acknowledges(&request));
            let encoded = reply.encode().unwrap();
            assert_eq!(AccountReply::decode(&encoded), Some(reply.clone()));
            let AccountReply::Claimed {
                receipt,
                binding: actual,
                ..
            } = reply
            else {
                unreachable!()
            };
            assert_eq!(actual, binding);
            assert_eq!(receipt.sequence(), 2);
            assert_eq!(
                world
                    .facts
                    .native_journal
                    .as_ref()
                    .unwrap()
                    .record_count()
                    .unwrap(),
                count
            );
        }
    }
}

#[test]
fn observation_rechecks_actor_after_unlocked_journal_io() {
    for claimed in [false, true] {
        let mut world = world(8620 + u64::from(claimed), ImagePhase::Forward);
        let _home = attach_journal(&mut world);
        let parent = Predecessor::new(world.original.clone(), world.binding.clone()).unwrap();
        let next = candidate(&world, "e3", ImagePhase::Forward);
        let binding = if claimed {
            admitted_binding(&admit(&world, next.clone(), ImagePhase::Forward))
        } else {
            world.binding.clone()
        };
        let request = if claimed {
            AccountRequest::Claim(Relation::new(parent, ImagePhase::Forward, next).unwrap())
        } else {
            AccountRequest::Closure(parent)
        };
        let records = world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap();
        let (entered, receive) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        world.facts.receipt_gate = Some((entered, gate.clone()));
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| observe(&world, &request, &world.caller));
            receive.recv_timeout(PATIENCE).unwrap();
            assert!(world.handle.ctx.meta.try_lock().is_ok());
            assert!(world.handle.ctx.fenced_writes.native.try_lock().is_ok());
            assert!(
                lookup(&world.handle.ctx, &binding)
                    .unwrap()
                    .try_lock()
                    .is_ok()
            );
            world
                .facts
                .processes
                .lock()
                .unwrap()
                .get_mut(&world.caller.peer.unwrap())
                .unwrap()
                .birth += 1;
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
            assert_eq!(worker.join().unwrap(), AccountReply::Unknown);
        });
        assert_eq!(
            world
                .facts
                .native_journal
                .as_ref()
                .unwrap()
                .record_count()
                .unwrap(),
            records
        );
    }
}

#[test]
fn claim_recovery_retains_binding_after_parent_compaction_and_allocates_nothing() {
    let mut world = world(8622, ImagePhase::Forward);
    let _home = attach_journal(&mut world);
    let next = candidate(&world, "e4", ImagePhase::Forward);
    let relation = Relation::new(
        Predecessor::new(world.original.clone(), world.binding.clone()).unwrap(),
        ImagePhase::Forward,
        next.clone(),
    )
    .unwrap();
    let request = AccountRequest::Claim(relation);
    let before = world
        .facts
        .native_journal
        .as_ref()
        .unwrap()
        .record_count()
        .unwrap();
    assert_eq!(
        observe(&world, &request, &world.caller),
        AccountReply::Unknown
    );
    assert!(
        !world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&next.id)
    );
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .record_count()
            .unwrap(),
        before
    );
    let binding = admitted_binding(&admit(&world, next, ImagePhase::Forward));
    let retired = world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .compact_account_lineages();
    drop(retired);
    assert!(lookup(&world.handle.ctx, &world.binding).is_none());
    let AccountReply::Claimed {
        binding: actual, ..
    } = observe(&world, &request, &world.caller)
    else {
        panic!("lost retained claim")
    };
    assert_eq!(actual, binding);
}
