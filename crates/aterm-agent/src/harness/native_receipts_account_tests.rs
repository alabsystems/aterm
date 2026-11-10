// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts_pool::ExecutionIntent;

#[path = "native_receipts_account_model_tests.rs"]
mod model;

struct World {
    _home: aterm_tempfile::TempDir,
    paths: Paths,
    parent: Prepare,
    binding: Binding,
    phase: ImagePhase,
}

#[test]
fn complete_receipt_mode_cannot_claim_or_fence_a_legacy_account_relation() {
    use crate::harness::native_act::{BootStamp, ReceiptMode};

    for marked_parent in [false, true] {
        let mut world = World::closed(ImagePhase::Forward);
        let mut candidate = world.candidate(2);
        let binding = candidate_binding(&candidate);
        assert!(
            world
                .parent
                .account_successor_recipe(&candidate, world.phase)
        );
        let before = world.bytes();
        let marked = if marked_parent {
            &mut world.parent
        } else {
            &mut candidate
        };
        marked.receipt_mode = Some(ReceiptMode::Recipients5);
        marked.boot = Some(BootStamp {
            host: format!("macos:{}", "12".repeat(16)),
            boot: "34".repeat(16),
        });
        marked.custody = Some("56".repeat(32));
        marked.rejection_domain = Some("78".repeat(32));
        assert!(Request::prepare(marked.clone()).encode().is_some());
        let relation = world.relation(&candidate);
        assert!(account::claim_at(&world.paths, relation, &binding, LIMITS).is_err());
        assert!(account::fence_at(&world.paths, relation, LIMITS).is_err());
        assert_eq!(world.bytes(), before);
    }
}

fn closure_phase_step(
    state: &aterm_spec::interp::State,
    action: &str,
    fault: u8,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "InitialRollback" => {
            next.insert("initial", 1);
        }
        "ProposeForward" => {
            next.insert("proposed", 1);
        }
        "ProposeRollback" => {
            next.insert("proposed", 2);
        }
        "Check" => {
            let initial = if state["initial"] == 0 {
                ImagePhase::Forward
            } else {
                ImagePhase::Rollback
            };
            let w = World::closed(initial);
            let candidate = w.candidate(2);
            let binding = candidate_binding(&candidate);
            account::claim_at(&w.paths, w.relation(&candidate), &binding, LIMITS).unwrap();
            let (key, mut view) = read(&w.paths).unwrap().unwrap();
            let entry = view.entries.get_mut(&key.operation(&candidate.id)).unwrap();
            if fault == 1 {
                entry.account = None;
            }
            if fault == 2 {
                entry.account.as_mut().unwrap().phase = ImagePhase::Forward;
            }
            let proposed = if fault == 3 {
                None
            } else {
                match state["proposed"] {
                    0 => None,
                    1 => Some(ImagePhase::Forward),
                    _ => Some(ImagePhase::Rollback),
                }
            };
            let mut evidence = aterm_spec::xref::StepEvidence::new("AccountClosurePhase");
            let allowed = evidence.step("Check", || account::permits_noexec(entry, proposed));
            assert!(evidence.audit().is_complete());
            next.insert("checked", 1);
            next.insert("allowed", i64::from(allowed));
        }
        _ => unreachable!(),
    }
    next
}

pub(crate) fn closure_phase_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    closure_phase_step(state, action, 0)
}

#[test]
fn closure_policy_refines_known_phase_and_rollback_retention() {
    let model = aterm_models_harness::account_closure_phase_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&closure_phase_transition(&state, action.name))
                );
            }
        }
    }
    let missing = closure_phase_step(&model.init_state(), "Check", 1);
    assert!(!model.check_invariant("NoMissingPhase", &missing));
    let mut rollback = model.init_state();
    rollback.insert("initial", 1);
    rollback.insert("proposed", 1);
    assert!(!model.check_invariant("NoRollbackLoss", &closure_phase_step(&rollback, "Check", 2)));
    rollback.insert("proposed", 2);
    assert!(!model.check_invariant(
        "ValidPhaseAvailable",
        &closure_phase_step(&rollback, "Check", 3)
    ));
}

fn stored(choice: u8) -> ExecutionIntent {
    ExecutionIntent::Stored {
        namespace: "12".repeat(32),
        login: None,
        owner: Some("account".into()),
        generation: format!("{choice:02x}").repeat(32),
    }
}

fn candidate_binding(candidate: &Prepare) -> Binding {
    Binding {
        id: candidate.id.clone(),
        capability: candidate.id.repeat(2),
    }
}

#[test]
fn recovery_requires_the_exact_existing_claim_and_never_creates_one() {
    let w = World::closed(ImagePhase::Forward);
    let candidate = w.candidate(2);
    let binding = candidate_binding(&candidate);
    assert_eq!(
        account::recover_at(&w.paths, w.relation(&candidate), &binding).unwrap(),
        None
    );
    let before = read(&w.paths).unwrap().unwrap().1.sequence;
    assert_eq!(before, 2);
    let claimed = account::claim_at(&w.paths, w.relation(&candidate), &binding, LIMITS).unwrap();
    let recovered = account::recover_at(&w.paths, w.relation(&candidate), &binding)
        .unwrap()
        .unwrap();
    assert_eq!(claimed, recovered);
    let mut wrong = binding.clone();
    wrong.capability = "ff".repeat(32);
    assert!(account::recover_at(&w.paths, w.relation(&candidate), &wrong).is_err());
    record_with_phase_at_limits(
        &w.paths,
        &candidate,
        &binding,
        Some(Event::NoExec),
        Some(ImagePhase::Forward),
        LIMITS,
    )
    .unwrap();
    assert_eq!(
        account::recover_at(&w.paths, w.relation(&candidate), &binding)
            .unwrap()
            .unwrap()
            .outcome,
        AccountOutcome::Claimed(ClaimedState::NoExec {
            phase: ImagePhase::Forward
        })
    );
    assert_eq!(observe_prepare_at(&w.paths, &candidate).unwrap(), None);
    assert_eq!(read(&w.paths).unwrap().unwrap().1.sequence, 4);
}

impl World {
    fn new(phase: ImagePhase) -> Self {
        let (_home, paths, mut parent, binding) = super::fixture();
        if let NativePlan::Restart {
            forward, rollback, ..
        } = &mut parent.plan
        {
            forward.account_intent = Some(Box::new(stored(1)));
            *rollback = Some(forward.clone());
        }
        Self {
            _home,
            paths,
            parent,
            binding,
            phase,
        }
    }

    fn closed(phase: ImagePhase) -> Self {
        let world = Self::new(phase);
        world.seed(LIMITS, true);
        world
    }

    /// A world of its own whose home holds what `template`'s holds, each directory and file with
    /// its mode, beside the same fixture: what a process that restarted there would reopen.
    fn cloned_from(template: &World) -> Self {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        crate::test_tree::clone_or_copy(&template.paths.home, &home);
        Self {
            _home: tmp,
            paths: Paths::for_home(&home),
            parent: template.parent.clone(),
            binding: template.binding.clone(),
            phase: template.phase,
        }
    }

    fn seed(&self, limits: Limits, phased: bool) {
        record_event_at_limits(
            &self.paths,
            &self.parent,
            &self.binding,
            Some(Event::Prepare),
            limits,
        )
        .unwrap();
        record_with_phase_at_limits(
            &self.paths,
            &self.parent,
            &self.binding,
            Some(Event::NoExec),
            phased.then_some(self.phase),
            limits,
        )
        .unwrap();
    }

    fn candidate(&self, choice: u8) -> Prepare {
        self.parent
            .with_account_successor(
                self.phase,
                format!("{choice:032x}"),
                2,
                8,
                self.parent.deadline_us + 1,
                stored(choice),
            )
            .unwrap()
    }

    fn relation<'a>(&'a self, candidate: &'a Prepare) -> AccountRelation<'a> {
        AccountRelation {
            predecessor: &self.parent,
            predecessor_binding: &self.binding,
            phase: self.phase,
            candidate,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(directory(&self.paths).join("noexec.astream")).unwrap()
    }
}

#[test]
fn claims_keep_their_identity_and_receipt_after_the_child_closes() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let w = World::closed(phase);
        let candidate = w.candidate(2);
        let binding = candidate_binding(&candidate);
        let relation = w.relation(&candidate);
        let claim = account::claim_at(&w.paths, relation, &binding, LIMITS).unwrap();
        assert_eq!(claim.sequence, 2);
        let parent_closure = observe_phase_at(&w.paths, &w.parent, &w.binding)
            .unwrap()
            .unwrap();
        assert_eq!(parent_closure.sequence, 1);
        assert_eq!(
            claim.outcome,
            AccountOutcome::Claimed(ClaimedState::Prepared)
        );
        let before = w.bytes();
        assert_eq!(
            account::claim_at(&w.paths, relation, &binding, LIMITS).unwrap(),
            claim
        );
        assert_eq!(
            account::observe_at(&w.paths, relation).unwrap(),
            Some(claim)
        );
        assert!(account::fence_at(&w.paths, relation, LIMITS).is_err());
        assert!(
            record_successor_at(
                &w.paths, &w.parent, &w.binding, &candidate, &binding, LIMITS
            )
            .is_err()
        );
        assert!(record_event_at(&w.paths, &candidate, &binding, Some(Event::Prepare)).is_err());
        let mut wrong_binding = binding.clone();
        wrong_binding.capability = "ee".repeat(32);
        assert!(account::claim_at(&w.paths, relation, &wrong_binding, LIMITS).is_err());
        assert_eq!(w.bytes(), before);

        let child_closure = record_with_phase_at_limits(
            &w.paths,
            &candidate,
            &binding,
            Some(Event::NoExec),
            Some(phase),
            LIMITS,
        )
        .unwrap();
        assert_eq!(child_closure.sequence, 3);
        let replayed = observe_phase_at(&w.paths, &candidate, &binding)
            .unwrap()
            .unwrap();
        assert_eq!(replayed.sequence, child_closure.sequence);
        assert_eq!(replayed.recorded_ms, child_closure.recorded_ms);
        assert_ne!(replayed.sequence, claim.sequence);
        assert_eq!(
            observe_phase_at(&w.paths, &w.parent, &w.binding).unwrap(),
            Some(parent_closure)
        );
        let closed = AccountObservation {
            outcome: AccountOutcome::Claimed(ClaimedState::NoExec { phase }),
            ..claim
        };
        assert_eq!(
            account::observe_at(&w.paths, relation).unwrap(),
            Some(closed)
        );
        let before = w.bytes();
        assert_eq!(
            account::claim_at(&w.paths, relation, &binding, LIMITS).unwrap(),
            closed
        );
        assert!(account::claim_at(&w.paths, relation, &wrong_binding, LIMITS).is_err());
        assert_eq!(w.bytes(), before);
        for private in [&w.parent.id, &candidate.id, &binding.capability] {
            assert!(
                !before
                    .windows(private.len())
                    .any(|bytes| bytes == private.as_bytes())
            );
        }
    }
}

#[test]
fn claim_observation_retains_its_origin_across_possible_execution_and_seal() {
    let w = World::closed(ImagePhase::Forward);
    let candidate = w.candidate(2);
    let binding = candidate_binding(&candidate);
    let relation = w.relation(&candidate);
    let claim = account::claim_at(&w.paths, relation, &binding, LIMITS).unwrap();
    record_event_at(&w.paths, &candidate, &binding, Some(Event::MayExecute)).unwrap();
    assert_eq!(
        account::observe_at(&w.paths, relation).unwrap(),
        Some(AccountObservation {
            outcome: AccountOutcome::Claimed(ClaimedState::MayExecute),
            ..claim
        })
    );
    record_event_at(&w.paths, &candidate, &binding, Some(Event::Seal)).unwrap();
    let sealed = AccountObservation {
        outcome: AccountOutcome::Claimed(ClaimedState::Sealed { may_execute: true }),
        ..claim
    };
    assert_eq!(
        account::observe_at(&w.paths, relation).unwrap(),
        Some(sealed)
    );
    let bytes = w.bytes();
    assert_eq!(
        account::claim_at(&w.paths, relation, &binding, LIMITS).unwrap(),
        sealed
    );
    assert_eq!(bytes, w.bytes());
}

#[test]
fn absent_issuer_custody_is_not_created_by_continuations() {
    for identity_present in [false, true] {
        let w = World::new(ImagePhase::Forward);
        if identity_present {
            aterm_secrets::client::machine_identity(&w.paths).unwrap();
        }
        let names = || {
            let mut names = if w.paths.dir.exists() {
                std::fs::read_dir(&w.paths.dir)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            names.sort();
            names
        };
        let before = names();
        let candidate = w.candidate(2);
        let binding = candidate_binding(&candidate);
        let relation = w.relation(&candidate);
        assert_eq!(account::observe_at(&w.paths, relation).unwrap(), None);
        assert!(account::claim_at(&w.paths, relation, &binding, LIMITS).is_err());
        assert!(account::fence_at(&w.paths, relation, LIMITS).is_err());
        assert!(account::recover_at(&w.paths, relation, &binding).is_err());
        assert!(
            record_successor_at(
                &w.paths, &w.parent, &w.binding, &candidate, &binding, LIMITS
            )
            .is_err()
        );
        for event in [Event::MayExecute, Event::NoExec, Event::Seal] {
            assert!(record_event_at(&w.paths, &w.parent, &w.binding, Some(event)).is_err());
        }
        assert!(
            record_with_phase_at_limits(
                &w.paths,
                &w.parent,
                &w.binding,
                Some(Event::NoExec),
                Some(ImagePhase::Forward),
                LIMITS,
            )
            .is_err()
        );
        assert_eq!(names(), before);
        assert!(!directory(&w.paths).exists());
        assert!(!w.paths.dir.join(ENROLLMENT).exists());
        assert_eq!(w.paths.dir.exists(), identity_present);
    }
}

#[test]
fn fences_exclude_every_writer_but_leave_a_different_candidate_available() {
    let w = World::closed(ImagePhase::Rollback);
    let candidate = w.candidate(2);
    let binding = candidate_binding(&candidate);
    let relation = w.relation(&candidate);
    let fence = account::fence_at(&w.paths, relation, LIMITS).unwrap();
    assert_eq!(fence.outcome, AccountOutcome::Fenced);
    let before = w.bytes();
    assert_eq!(
        account::fence_at(&w.paths, relation, LIMITS).unwrap(),
        fence
    );
    assert_eq!(
        account::observe_at(&w.paths, relation).unwrap(),
        Some(fence)
    );
    assert!(account::claim_at(&w.paths, relation, &binding, LIMITS).is_err());
    assert!(
        record_successor_at(
            &w.paths, &w.parent, &w.binding, &candidate, &binding, LIMITS
        )
        .is_err()
    );
    for event in [
        Event::Prepare,
        Event::MayExecute,
        Event::NoExec,
        Event::Seal,
    ] {
        assert!(record_event_at(&w.paths, &candidate, &binding, Some(event)).is_err());
    }
    assert!(record_at(&w.paths, &candidate, &binding).is_err());
    assert_eq!(w.bytes(), before);
    let other = w.candidate(3);
    account::claim_at(
        &w.paths,
        w.relation(&other),
        &candidate_binding(&other),
        LIMITS,
    )
    .unwrap();
    assert_eq!(
        account::observe_at(&w.paths, relation).unwrap(),
        Some(fence)
    );
}

#[test]
fn the_closure_reader_reads_account_claims_and_fences_as_other_records() {
    let w = World::closed(ImagePhase::Rollback);
    assert_eq!(
        observe_closure_at(&w.paths, &w.parent).unwrap(),
        Closure::Exact(Recorded::NoExec)
    );
    let claimed = w.candidate(2);
    account::claim_at(
        &w.paths,
        w.relation(&claimed),
        &candidate_binding(&claimed),
        LIMITS,
    )
    .unwrap();
    let fenced = w.candidate(3);
    account::fence_at(&w.paths, w.relation(&fenced), LIMITS).unwrap();
    for candidate in [&claimed, &fenced] {
        assert_eq!(
            observe_closure_at(&w.paths, candidate).unwrap(),
            Closure::Other
        );
    }
}

#[test]
fn an_already_claimed_parent_can_fence_a_rival_without_releasing_its_claim() {
    let w = World::closed(ImagePhase::Forward);
    let first = w.candidate(2);
    let rival = w.candidate(3);
    let first_binding = candidate_binding(&first);
    let claim = account::claim_at(&w.paths, w.relation(&first), &first_binding, LIMITS).unwrap();
    assert!(
        account::claim_at(
            &w.paths,
            w.relation(&rival),
            &candidate_binding(&rival),
            LIMITS
        )
        .is_err()
    );
    assert_eq!(
        account::fence_at(&w.paths, w.relation(&rival), LIMITS)
            .unwrap()
            .outcome,
        AccountOutcome::Fenced
    );
    assert_eq!(
        account::claim_at(&w.paths, w.relation(&first), &first_binding, LIMITS).unwrap(),
        claim
    );
}

#[test]
fn legacy_parent_and_successor_records_are_never_account_outcomes() {
    for old_receipt in [true, false] {
        let w = World::new(ImagePhase::Forward);
        if old_receipt {
            record_at(&w.paths, &w.parent, &w.binding).unwrap();
        } else {
            w.seed(LIMITS, false);
        }
        let candidate = w.candidate(2);
        let relation = w.relation(&candidate);
        assert!(
            account::claim_at(&w.paths, relation, &candidate_binding(&candidate), LIMITS).is_err()
        );
        assert!(account::fence_at(&w.paths, relation, LIMITS).is_err());
        assert_eq!(account::observe_at(&w.paths, relation).unwrap(), None);
    }
    let w = World::closed(ImagePhase::Forward);
    let candidate = w.candidate(2);
    let binding = candidate_binding(&candidate);
    record_successor_at(
        &w.paths, &w.parent, &w.binding, &candidate, &binding, LIMITS,
    )
    .unwrap();
    assert_eq!(
        account::observe_at(&w.paths, w.relation(&candidate)).unwrap(),
        None
    );
    assert!(account::claim_at(&w.paths, w.relation(&candidate), &binding, LIMITS).is_err());
    assert!(account::fence_at(&w.paths, w.relation(&candidate), LIMITS).is_err());
}

#[test]
fn relation_phase_recipe_and_capability_are_exact() {
    let w = World::closed(ImagePhase::Forward);
    let candidate = w.candidate(2);
    let relation = w.relation(&candidate);
    account::fence_at(&w.paths, relation, LIMITS).unwrap();
    let before = w.bytes();
    let mut changed = candidate.clone();
    changed.deadline_us += 1;
    let mut parent = w.parent.clone();
    parent.deadline_us += 1;
    let mut wrong_binding = w.binding.clone();
    wrong_binding.capability = "ee".repeat(32);
    for wrong in [
        AccountRelation {
            predecessor: &parent,
            ..relation
        },
        AccountRelation {
            predecessor_binding: &wrong_binding,
            ..relation
        },
        AccountRelation {
            candidate: &changed,
            ..relation
        },
        AccountRelation {
            phase: ImagePhase::Rollback,
            ..relation
        },
    ] {
        assert!(!matches!(account::observe_at(&w.paths, wrong), Ok(Some(_))));
        assert!(account::fence_at(&w.paths, wrong, LIMITS).is_err());
        assert!(
            account::claim_at(&w.paths, wrong, &candidate_binding(wrong.candidate), LIMITS)
                .is_err()
        );
    }
    assert_eq!(w.bytes(), before);

    let rollback_candidate = w
        .parent
        .with_account_successor(
            ImagePhase::Rollback,
            "04".repeat(16),
            2,
            8,
            w.parent.deadline_us + 1,
            stored(4),
        )
        .unwrap();
    assert!(
        w.parent
            .account_successor_recipe(&rollback_candidate, ImagePhase::Rollback)
    );
    let wrong_phase = AccountRelation {
        phase: ImagePhase::Rollback,
        ..w.relation(&rollback_candidate)
    };
    assert!(
        account::claim_at(
            &w.paths,
            wrong_phase,
            &candidate_binding(&rollback_candidate),
            LIMITS
        )
        .is_err()
    );
    assert!(account::fence_at(&w.paths, wrong_phase, LIMITS).is_err());

    let mut other_parent = w.parent.clone();
    other_parent.host_epoch = "34".repeat(16);
    let mut other_candidate = candidate.clone();
    other_candidate
        .host_epoch
        .clone_from(&other_parent.host_epoch);
    let wrong_epoch = AccountRelation {
        predecessor: &other_parent,
        candidate: &other_candidate,
        ..relation
    };
    assert!(other_parent.account_successor_recipe(&other_candidate, w.phase));
    assert!(
        account::claim_at(
            &w.paths,
            wrong_epoch,
            &candidate_binding(&other_candidate),
            LIMITS
        )
        .is_err()
    );
    assert!(account::fence_at(&w.paths, wrong_epoch, LIMITS).is_err());
    assert_eq!(w.bytes(), before);
}

#[test]
fn racing_claim_and_fence_publish_one_permanent_outcome() {
    let w = World::closed(ImagePhase::Forward);
    let candidate = w.candidate(2);
    let binding = candidate_binding(&candidate);
    let paths = w.paths.clone();
    let parent = w.parent.clone();
    let parent_binding = w.binding.clone();
    let child = candidate.clone();
    let child_binding = binding.clone();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_gate = std::sync::Arc::clone(&gate);
    let worker = std::thread::spawn(move || {
        worker_gate.wait();
        account::claim_at(
            &paths,
            AccountRelation {
                predecessor: &parent,
                predecessor_binding: &parent_binding,
                phase: ImagePhase::Forward,
                candidate: &child,
            },
            &child_binding,
            LIMITS,
        )
    });
    gate.wait();
    let fenced = account::fence_at(&w.paths, w.relation(&candidate), LIMITS);
    let claimed = worker.join().unwrap();
    assert_ne!(fenced.is_ok(), claimed.is_ok());
    let retained = account::observe_at(&w.paths, w.relation(&candidate))
        .unwrap()
        .unwrap();
    assert_eq!(retained, claimed.or(fenced).unwrap());
    assert_eq!(read(&w.paths).unwrap().unwrap().1.sequence, 3);
}

#[test]
fn old_and_account_claims_race_for_the_same_parent_slot() {
    let w = World::closed(ImagePhase::Forward);
    let legacy = w.candidate(2);
    let typed = w.candidate(3);
    let paths = w.paths.clone();
    let parent = w.parent.clone();
    let parent_binding = w.binding.clone();
    let old = legacy.clone();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_gate = std::sync::Arc::clone(&gate);
    let worker = std::thread::spawn(move || {
        worker_gate.wait();
        record_successor_at(
            &paths,
            &parent,
            &parent_binding,
            &old,
            &candidate_binding(&old),
            LIMITS,
        )
    });
    gate.wait();
    let account = account::claim_at(
        &w.paths,
        w.relation(&typed),
        &candidate_binding(&typed),
        LIMITS,
    );
    let old = worker.join().unwrap();
    assert_ne!(account.is_ok(), old.is_ok());
    let (key, view) = read(&w.paths).unwrap().unwrap();
    let parent = &view.entries[&key.operation(&w.parent.id)];
    assert_eq!(
        parent.successor,
        Some(key.operation(if account.is_ok() {
            &typed.id
        } else {
            &legacy.id
        }))
    );
    assert_eq!(view.entries.len(), 2);
    assert_eq!(view.sequence, 3);
}

#[test]
fn a_childs_later_closed_phase_does_not_rewrite_its_parent_phase() {
    let w = World::closed(ImagePhase::Forward);
    let candidate = w.candidate(2);
    let binding = candidate_binding(&candidate);
    let relation = w.relation(&candidate);
    let claim = account::claim_at(&w.paths, relation, &binding, LIMITS).unwrap();
    record_with_phase_at_limits(
        &w.paths,
        &candidate,
        &binding,
        Some(Event::NoExec),
        Some(ImagePhase::Rollback),
        LIMITS,
    )
    .unwrap();
    let observed = account::observe_at(&w.paths, relation).unwrap().unwrap();
    assert_eq!(observed.sequence, claim.sequence);
    assert_eq!(observed.recorded_ms, claim.recorded_ms);
    assert_eq!(
        observed.outcome,
        AccountOutcome::Claimed(ClaimedState::NoExec {
            phase: ImagePhase::Rollback
        })
    );
    assert_eq!(
        observe_phase_at(&w.paths, &w.parent, &w.binding)
            .unwrap()
            .unwrap()
            .phase,
        ImagePhase::Forward
    );
}

#[test]
fn a_closed_child_can_claim_one_grandchild_without_changing_earlier_outcomes() {
    let w = World::closed(ImagePhase::Forward);
    let child = w.candidate(2);
    let child_binding = candidate_binding(&child);
    let initial = account::claim_at(&w.paths, w.relation(&child), &child_binding, LIMITS).unwrap();
    record_with_phase_at_limits(
        &w.paths,
        &child,
        &child_binding,
        Some(Event::NoExec),
        Some(ImagePhase::Rollback),
        LIMITS,
    )
    .unwrap();
    let grandchild = child
        .with_account_successor(
            ImagePhase::Rollback,
            "03".repeat(16),
            3,
            9,
            child.deadline_us + 1,
            stored(3),
        )
        .unwrap();
    let grandchild_binding = candidate_binding(&grandchild);
    let relation = AccountRelation {
        predecessor: &child,
        predecessor_binding: &child_binding,
        phase: ImagePhase::Rollback,
        candidate: &grandchild,
    };
    let claim = account::claim_at(&w.paths, relation, &grandchild_binding, LIMITS).unwrap();
    assert_eq!(
        claim.outcome,
        AccountOutcome::Claimed(ClaimedState::Prepared)
    );
    let retained = account::claim_at(&w.paths, w.relation(&child), &child_binding, LIMITS).unwrap();
    assert_eq!(retained.sequence, initial.sequence);
    assert_eq!(retained.recorded_ms, initial.recorded_ms);
    assert_eq!(
        retained.outcome,
        AccountOutcome::Claimed(ClaimedState::NoExec {
            phase: ImagePhase::Rollback
        })
    );
    let mut rival = grandchild.clone();
    rival.id = "04".repeat(16);
    let rival_relation = AccountRelation {
        candidate: &rival,
        ..relation
    };
    assert!(
        account::claim_at(&w.paths, rival_relation, &candidate_binding(&rival), LIMITS).is_err()
    );
    account::fence_at(&w.paths, rival_relation, LIMITS).unwrap();
    assert_eq!(
        account::claim_at(&w.paths, relation, &grandchild_binding, LIMITS).unwrap(),
        claim
    );
    assert_eq!(
        account::observe_at(&w.paths, w.relation(&child)).unwrap(),
        Some(retained)
    );
}

#[test]
fn failed_barriers_withhold_both_kinds_of_acknowledgement() {
    for fence in [false, true] {
        let w = World::closed(ImagePhase::Forward);
        let candidate = w.candidate(2);
        let binding = candidate_binding(&candidate);
        let write = || {
            if fence {
                account::fence_at(&w.paths, w.relation(&candidate), LIMITS)
            } else {
                account::claim_at(&w.paths, w.relation(&candidate), &binding, LIMITS)
            }
        };
        let fault = aterm_durable::seam::FailingDirs::with_kind(io::ErrorKind::Unsupported);
        assert!(write().is_err());
        drop(fault);
        let committed = write().unwrap();
        assert_eq!(write().unwrap(), committed);
        assert_eq!(
            account::observe_at(&w.paths, w.relation(&candidate)).unwrap(),
            Some(committed)
        );
        assert_eq!(read(&w.paths).unwrap().unwrap().1.sequence, 3);
    }
}

#[test]
fn typed_children_cannot_lose_their_phase_in_a_writer_or_authenticated_replay() {
    for initial in [ImagePhase::Forward, ImagePhase::Rollback] {
        for closed in [None, Some(ImagePhase::Forward), Some(ImagePhase::Rollback)] {
            let w = World::closed(initial);
            let candidate = w.candidate(2);
            let binding = candidate_binding(&candidate);
            account::claim_at(&w.paths, w.relation(&candidate), &binding, LIMITS).unwrap();
            let permitted = closed.is_some()
                && (initial == ImagePhase::Forward || closed == Some(ImagePhase::Rollback));
            let (key, mut replay) = read(&w.paths).unwrap().unwrap();
            let receipt = receipt(&key, &candidate, &binding).unwrap();
            let epoch = key.digest(b"epoch\0", candidate.host_epoch.as_bytes());
            let body = closed.map_or_else(
                || replay.encode_event(&key, &receipt, epoch, Event::NoExec, 20),
                |phase| replay.encode_phase(&key, &receipt, epoch, phase, 20),
            );
            assert_eq!(replay.ingest(&key, &body).is_ok(), permitted);
            let before = w.bytes();
            let result = record_with_phase_at_limits(
                &w.paths,
                &candidate,
                &binding,
                Some(Event::NoExec),
                closed,
                LIMITS,
            );
            assert_eq!(result.is_ok(), permitted);
            if !permitted {
                assert_eq!(w.bytes(), before);
            }
        }
    }
}

#[test]
fn claims_and_fences_charge_exact_bytes_without_enlarging_old_closures() {
    for fence in [false, true] {
        let required = if fence {
            FENCE_FRAME
        } else {
            ACCOUNT_FRAME + 2 * ISSUER_FRAME
        };
        for spare in [0, 1] {
            let limits = Limits {
                records: if fence { 3 } else { 5 },
                bytes: 2 * ISSUER_FRAME + required - spare,
                record_bytes: ACCOUNT_BYTES,
            };
            let w = World::new(ImagePhase::Rollback);
            w.seed(limits, true);
            let candidate = w.candidate(2);
            let binding = candidate_binding(&candidate);
            let result = if fence {
                account::fence_at(&w.paths, w.relation(&candidate), limits)
            } else {
                account::claim_at(&w.paths, w.relation(&candidate), &binding, limits)
            };
            assert_eq!(result.is_ok(), spare == 0);
            if !fence && spare == 0 {
                record_event_at_limits(
                    &w.paths,
                    &candidate,
                    &binding,
                    Some(Event::MayExecute),
                    limits,
                )
                .unwrap();
                record_event_at_limits(&w.paths, &candidate, &binding, Some(Event::Seal), limits)
                    .unwrap();
            }
            let (_, view) = read(&w.paths).unwrap().unwrap();
            assert!(view.within_capacity(limits, 0));
            if spare == 0 {
                assert_eq!(view.committed_bytes, limits.bytes);
                assert_eq!(view.sequence as usize, limits.records);
                let bytes = w.bytes();
                let retry = if fence {
                    account::fence_at(&w.paths, w.relation(&candidate), limits)
                } else {
                    account::claim_at(&w.paths, w.relation(&candidate), &binding, limits)
                }
                .unwrap();
                let first = result.unwrap();
                assert_eq!(retry.sequence, first.sequence);
                assert_eq!(retry.recorded_ms, first.recorded_ms);
                assert_eq!(
                    retry.outcome,
                    if fence {
                        AccountOutcome::Fenced
                    } else {
                        AccountOutcome::Claimed(ClaimedState::Sealed { may_execute: true })
                    }
                );
                assert_eq!(w.bytes(), bytes);
            }
        }
    }
}

#[test]
fn account_frames_reject_tampering_and_authenticated_invalid_relations() {
    for fence in [false, true] {
        let w = World::closed(ImagePhase::Rollback);
        let candidate = w.candidate(2);
        let binding = candidate_binding(&candidate);
        let (key, view) = read(&w.paths).unwrap().unwrap();
        let bound = account::Bound::new(&key, w.relation(&candidate)).unwrap();
        let mut receipt = receipt(&key, &candidate, &binding).unwrap();
        if fence {
            receipt.capability = [0; 32];
        }
        let body = account::encode(&view, &key, &bound, &receipt, 11, fence);
        assert_eq!(body.len(), ACCOUNT_BYTES);
        let subject = if fence {
            FENCE_SUBJECT
        } else {
            ACCOUNT_SUBJECT
        };
        assert!(record_subject(subject, &body));
        assert!(!record_subject(SUCCESSOR_SUBJECT, &body));
        for at in 0..body.len() {
            let (_, mut replay) = read(&w.paths).unwrap().unwrap();
            let mut bad = body.clone();
            bad[at] ^= 1;
            assert!(replay.ingest(&key, &bad).is_err(), "byte {at}");
        }
        let mut invalid_fields = vec![4, 20, 148, 149, 181, 213];
        if fence {
            invalid_fields.push(116);
        }
        for at in invalid_fields {
            let (_, mut replay) = read(&w.paths).unwrap().unwrap();
            let mut bad = body[..ACCOUNT_BYTES - 32].to_vec();
            bad[at] ^= 128;
            bad.extend_from_slice(hmac::sign(&key.mac, &bad).as_ref());
            assert!(
                replay.ingest(&key, &bad).is_err(),
                "authenticated field {at}"
            );
        }
        let (_, mut replay) = read(&w.paths).unwrap().unwrap();
        replay.ingest(&key, &body).unwrap();
        assert!(replay.ingest(&key, &body).is_err());
    }
}
