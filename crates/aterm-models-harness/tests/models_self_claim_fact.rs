// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `SelfClaimFact`: it holds at `Buggy = 0`, each law breaks under its own mutant alone,
//! it stays small, and walks follow the paths gap 23 names. Tier 1 is
//! `aterm-agent/tests/conformance_self_claim_facts.rs`. The non-vacuity sweep is `aterm-models`'
//! `non_vacuity`.

use aterm_models_harness::self_claim_fact_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

/// Each mutant, its dial and the law it was written to break.
const MUTANTS: [(&str, i64, &str); 3] = [
    ("SlotSteal", 1, "TypedTwiceOnlyAfterALateAck"),
    ("R1AcksEarly", 2, "ToldOnlyWhatRode"),
    ("R2ReadsLeased1", 3, "TypedTwiceOnlyAfterALateAck"),
];

/// A bound on the states the model reaches at any dial.
const STATE_BOUND: usize = 20_000;

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

#[test]
fn self_claim_fact_fires_exactly_its_committed_moves() {
    let m = self_claim_fact_model();
    assert!(
        interp::violated_invariants(&m).is_empty(),
        "every law holds"
    );
    let fired = interp::fired_actions(&m);
    for action in m.actions.iter().map(|a| a.name) {
        let mutant = MUTANTS.iter().any(|(name, ..)| *name == action);
        assert_eq!(fired.contains(action), !mutant, "{action}: {fired:?}");
    }
}

#[test]
fn each_mutant_breaks_its_own_law_alone() {
    let m = self_claim_fact_model();
    for (mutant, dial, law) in MUTANTS {
        let one = alone(&m, mutant);
        let wrong = interp::with_buggy(&aterm_spec::walk::only(&one, law), dial);
        assert!(
            interp::violated_invariants(&wrong).contains(law),
            "{mutant} (Buggy = {dial}) breaks {law}"
        );
        let right = interp::with_buggy(&aterm_spec::walk::only(&one, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "{law} holds"
        );
    }
    let laws: Vec<&str> = MUTANTS.iter().map(|(_, _, law)| *law).collect();
    for inv in &m.invariants {
        assert!(laws.contains(&inv.name), "{} has its own mutant", inv.name);
    }
}

#[test]
fn self_claim_fact_stays_small() {
    let m = self_claim_fact_model();
    for dial in std::iter::once(0).chain(MUTANTS.iter().map(|(_, dial, _)| *dial)) {
        let n = interp::reachable_states(&interp::with_buggy(&m, dial)).len();
        assert!(n < STATE_BOUND, "Buggy = {dial}: {n} states");
    }
}

/// An offered episode is the reader's alone while its act runs: the other reader is offered the
/// other episode; once the first act's turn started and its ack told it, nobody is offered it
/// again; an ack lost leaves it offered again, after the lease, to the other reader, and the second
/// ride is then the only one the law allows.
#[test]
fn an_episode_rides_once_unless_its_ack_was_lost() {
    let m = self_claim_fact_model();
    let both = walk(&m, &["Open1", "Open2", "R1Reads1", "R2Reads2"]);
    assert_eq!((both["offer1"], both["offer2"]), (1, 2), "{both:?}");
    assert!(!m.action_enabled("R2Reads1", &both), "R2 holds an act");
    let told = walk(&m, &["Open1", "Open2", "R1Reads1", "R1Starts", "R1Acks"]);
    assert_eq!((told["told1"], told["typed1"], told["offer1"]), (1, 1, 0));
    assert!(!m.action_enabled("R1Reads1", &told), "told once");
    assert!(!m.action_enabled("R2Reads1", &told));
    let lost = walk(
        &m,
        &[
            "Open1",
            "R1Reads1",
            "R1Starts",
            "R1LosesAck",
            "Lapse1",
            "R2Reads1",
            "R2Starts",
        ],
    );
    assert_eq!((lost["typed1"], lost["late1"]), (2, 1), "{lost:?}");
    let leased = walk(&m, &["Open1", "R1Reads1", "R1Drops"]);
    assert!(!m.action_enabled("R2Reads1", &leased), "the lease stands");
    assert!(
        m.action_enabled("R1Reads1", &leased),
        "its own reader takes it again"
    );
    assert!(m.action_enabled("Lapse1", &leased));
    let held = walk(&m, &["Open1", "R1Reads1"]);
    assert!(
        !m.action_enabled("Lapse1", &held),
        "no lease ends under its act"
    );
}

/// Leases lapse in the order they were last offered: with both live, the older lapses while the
/// younger stands, and its reader, its act over, then takes the lapsed episode beside its own live
/// lease, the other reader offered neither; an episode taken again is the younger.
#[test]
fn the_older_lease_lapses_first_and_a_retake_is_the_younger() {
    let m = self_claim_fact_model();
    let both = walk(
        &m,
        &[
            "Open1", "Open2", "R1Reads1", "R1Drops", "R2Reads2", "R2Drops",
        ],
    );
    assert_eq!(both["older"], 1, "{both:?}");
    assert!(!m.action_enabled("Lapse2", &both), "the younger stands");
    let one = walk(
        &m,
        &[
            "Open1", "Open2", "R1Reads1", "R1Drops", "R2Reads2", "R2Drops", "Lapse1",
        ],
    );
    assert_eq!((one["lapsed1"], one["lapsed2"], one["older"]), (1, 0, 0));
    assert!(
        m.action_enabled("Lapse2", &one),
        "then the younger may lapse"
    );
    assert!(
        m.action_enabled("R2Reads1", &one),
        "the lapsed episode beside a live lease"
    );
    let taken = walk(
        &m,
        &[
            "Open1", "Open2", "R1Reads1", "R1Drops", "R2Reads2", "R2Drops", "Lapse1", "R2Reads1",
        ],
    );
    assert_eq!(
        (taken["offer1"], taken["offer2"], taken["older"]),
        (2, 2, 2)
    );
    assert!(!m.action_enabled("R1Reads1", &taken));
    assert!(
        !m.action_enabled("R1Reads2", &taken),
        "the other reader is offered neither"
    );
    let again = walk(
        &m,
        &[
            "Open1", "Open2", "R1Reads1", "R1Drops", "R2Reads2", "R2Drops", "R1Reads1", "R1Drops",
        ],
    );
    assert_eq!(again["older"], 2, "{again:?}");
    assert!(m.action_enabled("Lapse2", &again));
    assert!(
        !m.action_enabled("Lapse1", &again),
        "the episode taken again is the younger"
    );
}

/// The first draft's one offer slot types an episode again after its ack was refused; an ack
/// before the turn started tells what never rode; no lease lets both readers carry one episode.
#[test]
fn the_mutants_tell_twice_or_lie() {
    let m = self_claim_fact_model();
    let one = interp::with_buggy(&m, 1);
    let again = walk(
        &one,
        &[
            "Open1",
            "Open2",
            "R1Reads1",
            "R2Reads2",
            "SlotSteal",
            "R1Starts",
            "R1Acks",
            "R1Reads1",
            "R1Starts",
        ],
    );
    assert_eq!((again["told1"], again["typed1"], again["late1"]), (0, 2, 0));
    assert!(!one.check_invariant("TypedTwiceOnlyAfterALateAck", &again));
    let early = interp::with_buggy(&m, 2);
    let lie = walk(&early, &["Open1", "R1Reads1", "R1AcksEarly"]);
    assert_eq!((lie["told1"], lie["typed1"]), (1, 0));
    assert!(!early.check_invariant("ToldOnlyWhatRode", &lie));
    let open = interp::with_buggy(&m, 3);
    let both = walk(
        &open,
        &[
            "Open1",
            "R1Reads1",
            "R2ReadsLeased1",
            "R1Starts",
            "R2Starts",
        ],
    );
    assert_eq!(both["typed1"], 2);
    assert!(!open.check_invariant("TypedTwiceOnlyAfterALateAck", &both));
}
