// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    HandoffCommitFacts, handoff_commit_admitted, handoff_rejection_activity_shaped,
    handoff_rejection_reason,
};
use aterm_models_update::native_update_seamless_handoff_ownership_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

const TO_DESCRIPTORS_TRANSFERRED: [&str; 4] = [
    "ParkOutgoingReaders",
    "CaptureCheckpoints",
    "StartReaderlessCandidate",
    "DuplicateDescriptorsToCandidate",
];

const TO_PROOF_MATCHED: [&str; 5] = [
    "ParkOutgoingReaders",
    "CaptureCheckpoints",
    "StartReaderlessCandidate",
    "DuplicateDescriptorsToCandidate",
    "MatchAdoptionProof",
];

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
        recovery_count_ready: bits & (1 << 12) != 0,
    }
}

const FACTS: u32 = 13;
const ALL_FACTS: u32 = (1 << FACTS) - 1;

fn model_revoked(facts: HandoffCommitFacts) -> bool {
    !(facts.exact_sessions
        && facts.exact_layout
        && facts.exact_agent_carry
        && facts.recovery_count_ready
        && facts.exact_activity
        && facts.teardown_allows_commit
        && facts.sessions_alive
        && facts.input_dispatch_fenced
        && facts.egress_settled
        && facts.native_safe
        && facts.commit_channel)
}

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
    let proof = HandoffCommitFacts {
        proof_exact: false,
        ..all
    };
    assert!(!handoff_rejection_activity_shaped(proof));
}

#[test]
fn an_agent_carry_that_moved_refuses_the_commit_by_its_own_name() {
    let all = facts_from_bits(ALL_FACTS);
    let moved = HandoffCommitFacts {
        exact_agent_carry: false,
        ..all
    };
    assert!(!handoff_commit_admitted(moved));
    assert!(handoff_rejection_activity_shaped(moved));
    let reason = handoff_rejection_reason(moved, &Err(Vec::new()), false, false);
    let topology = HandoffCommitFacts {
        exact_layout: false,
        ..all
    };
    assert_ne!(
        reason,
        handoff_rejection_reason(topology, &Err(Vec::new()), false, false),
        "the carry is named apart from the window/tab/pane topology"
    );
}

#[test]
fn real_commit_admission_conforms_to_the_ownership_model_over_every_bounded_fact_combination() {
    let model = native_update_seamless_handoff_ownership_model();
    let all = facts_from_bits(ALL_FACTS);
    let waiting = HandoffCommitFacts {
        recovery_count_ready: false,
        ..all
    };
    assert!(!handoff_commit_admitted(waiting));
    assert!(handoff_rejection_activity_shaped(waiting));
    assert_ne!(
        handoff_rejection_reason(waiting, &Err(Vec::new()), false, false),
        handoff_rejection_reason(
            HandoffCommitFacts {
                exact_activity: false,
                ..all
            },
            &Err(Vec::new()),
            false,
            false
        ),
        "unconfirmed recovery custody is named apart from structural activity",
    );
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

    assert_eq!(
        admitted_combinations, 1,
        "exactly one bounded combination may authorize a Commit"
    );
}
