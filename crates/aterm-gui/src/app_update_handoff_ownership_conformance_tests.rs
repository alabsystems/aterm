// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    HandoffCommitFacts, handoff_commit_admitted, handoff_rejection_activity_shaped,
    handoff_rejection_reason,
};
use aterm_spec::derive::{Model, native_update_seamless_handoff_ownership_model};
use aterm_spec::interp::{State, admits, with_buggy};

const TO_DESCRIPTORS_TRANSFERRED: [&str; 4] = [
    "ParkOutgoingReaders",
    "CaptureCheckpoints",
    "StartReaderlessCandidate",
    "DuplicateDescriptorsToCandidate",
];

/// `TO_DESCRIPTORS_TRANSFERRED` plus the proof: the model's `ProofMatched`.
const TO_PROOF_MATCHED: [&str; 5] = [
    "ParkOutgoingReaders",
    "CaptureCheckpoints",
    "StartReaderlessCandidate",
    "DuplicateDescriptorsToCandidate",
    "MatchAdoptionProof",
];

/// Fire a deterministic sequence, checking at every step that the model admits
/// exactly the named action — never that it admits *something*.
fn walk(model: &Model, actions: &[&'static str]) -> State {
    let mut state = model.init_state();
    for &action in actions {
        let next = model.successors(action, &state);
        assert_eq!(
            next.len(),
            1,
            "{action} must be deterministically enabled at {state:?}"
        );
        assert_eq!(admits(model, &state, &next[0]), Some(action));
        state = next[0].clone();
    }
    state
}

fn assert_every_invariant_holds(model: &Model, state: &State) {
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, state),
            "state violates {}::{}: {state:?}",
            model.name,
            invariant.name,
        );
    }
}

/// Every reachable state, by BFS over the interpreter's successor relation.
/// Small by construction (two sessions, a chain protocol), so the bound is a
/// tripwire.
fn reachable(model: &Model) -> Vec<State> {
    let mut seen: std::collections::BTreeSet<State> = std::collections::BTreeSet::new();
    let mut queue: std::collections::VecDeque<State> = std::collections::VecDeque::new();
    let init = model.init_state();
    seen.insert(init.clone());
    queue.push_back(init);
    while let Some(state) = queue.pop_front() {
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                if seen.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
        }
    }
    assert!(
        seen.len() < 20_000,
        "the ownership model's bounded space regressed to {} states",
        seen.len()
    );
    seen.into_iter().collect()
}

/// Is a state satisfying `goal` reachable from `from`?
fn reaches(model: &Model, from: &State, goal: impl Fn(&State) -> bool) -> bool {
    let mut seen: std::collections::BTreeSet<State> = std::collections::BTreeSet::new();
    let mut queue: std::collections::VecDeque<State> = std::collections::VecDeque::new();
    seen.insert(from.clone());
    queue.push_back(from.clone());
    while let Some(state) = queue.pop_front() {
        if goal(&state) {
            return true;
        }
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                if seen.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
        }
    }
    false
}

fn facts_from_bits(bits: u32) -> HandoffCommitFacts {
    HandoffCommitFacts {
        exact_sessions: bits & (1 << 0) != 0,
        exact_layout: bits & (1 << 1) != 0,
        exact_activity: bits & (1 << 2) != 0,
        teardown_allows_commit: bits & (1 << 3) != 0,
        parent_still_parked: bits & (1 << 4) != 0,
        sessions_alive: bits & (1 << 5) != 0,
        input_dispatch_fenced: bits & (1 << 6) != 0,
        egress_settled: bits & (1 << 7) != 0,
        native_safe: bits & (1 << 8) != 0,
        proof_exact: bits & (1 << 9) != 0,
        commit_channel: bits & (1 << 10) != 0,
        exact_agent_carry: bits & (1 << 11) != 0,
        operator_idle: bits & (1 << 12) != 0,
        operator_quiesced: bits & (1 << 13) != 0,
    }
}

/// How many bounded facts the Commit admission reads ([`facts_from_bits`]).
const FACTS: u32 = 14;

/// Every fact true: the one combination that may Commit.
const ALL_FACTS: u32 = (1 << FACTS) - 1;

/// The model's `revoked` is "some mutable pre-Commit admission fact turned
/// false". `parent_still_parked` and `proof_exact` are separate model variables
/// (`out_readers`, `proof_matched`), since the ownership invariants are about
/// them.
fn model_revoked(facts: HandoffCommitFacts) -> bool {
    !(facts.exact_sessions
        && facts.exact_layout
        && facts.exact_agent_carry
        && facts.exact_activity
        && facts.teardown_allows_commit
        && facts.sessions_alive
        && facts.input_dispatch_fenced
        && facts.egress_settled
        && facts.native_safe
        && facts.commit_channel
        && facts.operator_idle
        && facts.operator_quiesced)
}

/// Projects one fact combination onto the model's pre-Commit state, including states the
/// healthy model never reaches (a reader resumed mid-attempt).
fn project_commit_facts(transferred: &State, matched: &State, facts: HandoffCommitFacts) -> State {
    let mut state = if facts.proof_exact {
        matched.clone()
    } else {
        transferred.clone()
    };
    state.insert("revoked", i64::from(model_revoked(facts)));
    state.insert("out_readers", i64::from(!facts.parent_still_parked));
    state
}

/// A handed session that ended between capture and Commit refuses the Commit as activity
/// (`ActivityRevoked`), the same filing the park gives it, never `Physical(Transient)`.
#[test]
fn session_death_before_commit_is_filed_as_the_activity_the_park_files_it_as() {
    use crate::app_native::HandoffFailureLane;
    use crate::native_updater_service::ApplyMode;
    let all = facts_from_bits(ALL_FACTS);
    assert!(
        handoff_commit_admitted(all),
        "PRECONDITION: all true admits"
    );
    assert!(!handoff_rejection_activity_shaped(all));
    let died = HandoffCommitFacts {
        sessions_alive: false,
        ..all
    };
    assert!(!handoff_commit_admitted(died), "death still refuses");
    assert!(handoff_rejection_activity_shaped(died));
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        for (outcome, main_thread_saw_it) in [
            (crate::UpdateHandoffOutcome::Rejected, true),
            (crate::UpdateHandoffOutcome::ActivityRevoked, false),
        ] {
            assert_eq!(
                HandoffFailureLane::classify(
                    mode,
                    outcome,
                    crate::ChildDeathEvidence::Unobserved,
                    main_thread_saw_it && handoff_rejection_activity_shaped(died),
                ),
                HandoffFailureLane::ActivityRevoked,
                "{mode:?} {outcome:?}"
            );
        }
    }
    // Negative control: a genuine fault (a mismatched proof) stays on the physical schedule.
    let proof = HandoffCommitFacts {
        proof_exact: false,
        ..all
    };
    assert!(!handoff_rejection_activity_shaped(proof));
}

/// A busy resident operator (a proposal in flight) refuses the Commit as activity, so the
/// automatic lane files it `ActivityRevoked`, never a strike. Idle admits; shutting down is a
/// fault on the physical schedule.
#[test]
fn a_busy_resident_operator_refuses_the_commit_as_activity_never_a_strike() {
    use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};
    use crate::native_updater_service::ApplyMode;
    use crate::operator_host::{ControlHandle, UpdateQuiesceRefusal};
    let all = facts_from_bits(ALL_FACTS);
    let with_operator = |refusal: Option<UpdateQuiesceRefusal>| {
        let (operator_idle, operator_quiesced) = super::operator_commit_facts(refusal);
        HandoffCommitFacts {
            operator_idle,
            operator_quiesced,
            ..all
        }
    };
    let lane = |mode: ApplyMode, facts: HandoffCommitFacts| {
        HandoffFailureLane::classify(
            mode,
            crate::UpdateHandoffOutcome::Rejected,
            crate::ChildDeathEvidence::Unobserved,
            handoff_rejection_activity_shaped(facts),
        )
    };
    let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
    let operator = ControlHandle::new("test-commit-operator-busy".to_string(), notify_tx);

    let proposal = operator.begin_action_activity().expect("a proposal starts");
    let busy = operator
        .try_begin_update_quiesce()
        .err()
        .expect("a proposal is in flight at the Commit");
    assert!(busy.is_busy(), "{busy:?}");
    let facts = with_operator(Some(busy));
    assert!(!handoff_commit_admitted(facts), "a busy operator refuses");
    assert!(handoff_rejection_activity_shaped(facts), "…as activity");
    assert_eq!(
        handoff_rejection_reason(facts, &Err(Vec::new()), Some(busy), false, false),
        "resident operator could not quiesce for Commit: operator transaction is in \
         flight; retry update"
    );
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        assert_eq!(
            lane(mode, facts),
            HandoffFailureLane::ActivityRevoked,
            "{mode:?}"
        );
    }
    // The Commit permit's moment (another thread holding the gate) is the same fact, re-read
    // at the `_exit`.
    let permit_busy = with_operator(Some(UpdateQuiesceRefusal::Busy(
        "operator authority transition is busy",
    )));
    assert_eq!(
        lane(ApplyMode::Automatic, permit_busy),
        HandoffFailureLane::ActivityRevoked
    );

    // The same operator idle: fenced, and the Commit admits.
    drop(proposal);
    let fenced = operator
        .try_begin_update_quiesce()
        .expect("an idle operator fences");
    assert!(handoff_commit_admitted(with_operator(None)));
    drop(fenced);

    // Negative control: an operator refusing for anything but work (shutting down) is a fault,
    // and stays on the physical schedule.
    operator.begin_shutdown();
    let shut = operator
        .try_begin_update_quiesce()
        .err()
        .expect("a shutting-down operator refuses");
    assert!(!shut.is_busy(), "{shut:?}");
    let facts = with_operator(Some(shut));
    assert!(!handoff_commit_admitted(facts));
    assert!(!handoff_rejection_activity_shaped(facts));
    assert_eq!(
        lane(ApplyMode::Automatic, facts),
        HandoffFailureLane::Physical(PhysicalFailureShape::Transient)
    );
}

/// An agent relaunch owed or settled between the two captures refuses the Commit by its own
/// name, as activity: the successor would restore a layout without it, and the rollback lets
/// this process relaunch it. Lossless and repeatable, never a fault against the build, and
/// never read as the topology changing.
#[test]
fn an_agent_carry_that_moved_refuses_the_commit_by_its_own_name() {
    let all = facts_from_bits(ALL_FACTS);
    let moved = HandoffCommitFacts {
        exact_agent_carry: false,
        ..all
    };
    assert!(!handoff_commit_admitted(moved));
    assert!(handoff_rejection_activity_shaped(moved));
    let reason = handoff_rejection_reason(moved, &Err(Vec::new()), None, false, false);
    assert_eq!(
        reason,
        "an agent's relaunch became owed or was settled during async preparation"
    );
    // Negative control: the topology's own reason is unchanged.
    let topology = HandoffCommitFacts {
        exact_layout: false,
        ..all
    };
    assert_eq!(
        handoff_rejection_reason(topology, &Err(Vec::new()), None, false, false),
        "window/tab/pane topology changed during async preparation"
    );
}

/// `handoff_commit_admitted` alone decides whether ownership moves; all 2^14 fact combinations
/// must get the same verdict from it and from the model's `CommitAtomically` guard.
#[test]
fn real_commit_admission_conforms_to_the_ownership_model_over_every_bounded_fact_combination() {
    let model = native_update_seamless_handoff_ownership_model();
    let transferred = walk(&model, &TO_DESCRIPTORS_TRANSFERRED);
    let matched = walk(&model, &TO_PROOF_MATCHED);
    let mut admitted_combinations = 0usize;
    let mut refused_parked: std::collections::BTreeSet<State> = std::collections::BTreeSet::new();

    for bits in 0..(1u32 << FACTS) {
        let facts = facts_from_bits(bits);
        let before = project_commit_facts(&transferred, &matched, facts);
        let admitted = handoff_commit_admitted(facts);
        let successors = model.successors("CommitAtomically", &before);
        assert_eq!(
            admitted,
            !successors.is_empty(),
            "the shipping admission and the model's Commit guard disagree for {facts:?}"
        );

        // An activity-shaped rejection is a refusal first: it never coexists with
        // an admitted Commit, or the automatic lane would retry a landed handoff.
        if handoff_rejection_activity_shaped(facts) {
            assert!(
                !admitted,
                "an activity-shaped rejection cannot also be admitted: {facts:?}"
            );
        }

        if !admitted {
            // Once a parent reader has resumed, that part of the rollback has happened; every
            // refusal with readers still parked must have a path back to Resumed.
            if facts.parent_still_parked {
                refused_parked.insert(before);
            }
            continue;
        }

        admitted_combinations += 1;
        let after = successors[0].clone();
        assert_eq!(admits(&model, &before, &after), Some("CommitAtomically"));
        assert_every_invariant_holds(&model, &after);
        assert_eq!(after["commits"], 1);
        assert_eq!(after["owner_a"], 2, "session a moved to the candidate");
        assert_eq!(after["owner_b"], 2, "session b moved to the candidate");
        assert_eq!(after["out_live"], 0, "the outgoing process exited");
        assert_eq!(
            after["cand_readers"], 0,
            "the candidate's reader gate opens after Commit, never with it"
        );
    }

    // Rollback is available for every refusal, checked once per distinct projected state (the
    // projection is many-to-one).
    assert!(refused_parked.len() > 1, "vacuous refusal projection");
    for state in &refused_parked {
        assert!(
            reaches(&model, state, |candidate| {
                candidate["phase"] == 7
                    && candidate["out_live"] == 1
                    && candidate["owner_a"] == 1
                    && candidate["owner_b"] == 1
                    && candidate["commits"] == 0
            }),
            "no rollback path to Resumed from the refused state {state:?}"
        );
    }

    // Non-vacuity: the conjunction admits exactly the all-facts-hold combination.
    assert_eq!(
        admitted_combinations, 1,
        "exactly one bounded combination may authorize a Commit"
    );
}

/// Every reachable pre-Commit state of the ownership model has a rollback path to Resumed
/// with both sessions still the outgoing process's; the Buggy=1 mutant can orphan a session
/// and put two readers on one master.
#[test]
fn every_pre_commit_state_has_a_path_back_to_resumed_with_both_sessions_still_ours() {
    let model = native_update_seamless_handoff_ownership_model();
    let states = reachable(&model);
    assert!(states.len() > 10, "vacuous reachable space");

    let (mut pre_commit, mut committed, mut settled) = (0usize, 0usize, 0usize);
    for state in &states {
        assert_every_invariant_holds(&model, state);
        if state["commits"] > 0 {
            committed += 1;
            continue;
        }
        if state["phase"] == 7 || state["phase"] == 8 {
            settled += 1;
            continue;
        }
        if state["phase"] == 0 {
            continue;
        }
        pre_commit += 1;
        assert!(
            reaches(&model, state, |candidate| {
                candidate["phase"] == 7
                    && candidate["out_live"] == 1
                    && candidate["out_readers"] == 1
                    && candidate["owner_a"] == 1
                    && candidate["owner_b"] == 1
                    && candidate["commits"] == 0
            }),
            "no rollback path to Resumed from {state:?}"
        );
    }
    assert!(
        pre_commit > 0 && committed > 0 && settled > 0,
        "the sweep must see all three lanes: {pre_commit} pre-commit, \
         {committed} committed, {settled} settled"
    );

    let buggy = with_buggy(&model, 1);
    let broken = reachable(&buggy);
    assert!(
        broken
            .iter()
            .any(|state| !buggy.check_invariant("NoSessionIsEverOrphaned", state)),
        "the mutant must be able to orphan a session"
    );
    assert!(
        broken
            .iter()
            .any(|state| !buggy.check_invariant("NeverTwoReadersOnOneMaster", state)),
        "the mutant must be able to put two readers on one master"
    );
}
