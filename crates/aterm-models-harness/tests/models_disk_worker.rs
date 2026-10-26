// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `DiskFreeWorker`: every law holds at `Buggy = 0`, each mutant breaks the law it was
//! written to break alone, the space stays small, and walks follow the paths the design's §8.4
//! names. Tier 1 is `aterm-agent/tests/conformance_disk_free_worker.rs`; the non-vacuity sweep
//! is `aterm-models`' `non_vacuity`.

use aterm_models_harness::disk_free_worker_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

/// Each mutant, its dial, and the law it was written to break.
const MUTANTS: [(&str, i64, &str); 8] = [
    ("ClaimTwice", 1, "OneWorker"),
    ("ClaimUndue", 2, "WorkerOnlyDue"),
    ("ClaimBeforeDone", 3, "WorkerOnlyDone"),
    ("ClaimInCooldown", 4, "NoWorkerInCooldown"),
    ("LineWhileWorker", 5, "NoLineWhileWorker"),
    ("LineAfterOver", 6, "NoLineWhileWorker"),
    ("AlertWhileWorker", 7, "AlertOnlyNeeded"),
    ("AlertStays", 8, "AlertOnlyNeeded"),
];

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

#[test]
fn the_worker_fires_exactly_its_committed_moves() {
    let m = disk_free_worker_model();
    assert!(
        interp::violated_invariants(&m).is_empty(),
        "every law holds at Buggy = 0"
    );
    let fired = interp::fired_actions(&m);
    for action in m.actions.iter().map(|a| a.name) {
        let mutant = MUTANTS.iter().any(|(name, ..)| *name == action);
        assert_eq!(
            fired.contains(action),
            !mutant,
            "{action} fires with no mutant on exactly when it is no mutant: {fired:?}"
        );
    }
}

#[test]
fn each_mutant_breaks_its_own_law_alone() {
    let base = disk_free_worker_model();
    for (mutant, dial, law) in MUTANTS {
        let m = alone(&base, mutant);
        let wrong = interp::with_buggy(&aterm_spec::walk::only(&m, law), dial);
        assert!(
            interp::violated_invariants(&wrong).contains(law),
            "{mutant} (Buggy = {dial}) breaks {law}"
        );
        let right = interp::with_buggy(&aterm_spec::walk::only(&m, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "the committed model keeps {law}"
        );
    }
    let laws: Vec<&str> = MUTANTS.iter().map(|(_, _, law)| *law).collect();
    for inv in base
        .invariants
        .iter()
        .filter(|i| i.kind == aterm_spec::derive::InvariantKind::Claim)
    {
        assert!(laws.contains(&inv.name), "{} has its own mutant", inv.name);
    }
}

#[test]
fn the_worker_stays_under_50k_states() {
    let base = disk_free_worker_model();
    let mut counts = Vec::new();
    for dial in std::iter::once(0).chain(MUTANTS.iter().map(|(_, dial, _)| *dial)) {
        let n = interp::reachable_states(&interp::with_buggy(&base, dial)).len();
        assert!(n < 50_000, "Buggy = {dial}: {n} states");
        counts.push(format!("Buggy={dial}: {n}"));
    }
    eprintln!("{}", counts.join("\n"));
}

/// Pressed and aterm done: the episode opens.
const OPENED: [&str; 3] = ["Press", "Spend", "Open"];

fn then(more: &[&'static str]) -> Vec<&'static str> {
    [&OPENED[..], more].concat()
}

/// An out episode claims one worker, which starts, works and finishes; the episode stays out,
/// no second worker is claimed in it, past the cooldown too, and the owner is needed only once
/// nothing aterm runs can act.
#[test]
fn an_out_episode_claims_one_worker_and_only_one() {
    let m = disk_free_worker_model();
    let out = walk(&m, &then(&["RunOut"]));
    assert_eq!((out["ep"], out["need"]), (3, 1));
    assert!(
        !m.action_enabled("Raise", &out),
        "a worker can still start: {out:?}"
    );
    let running = walk(&m, &then(&["RunOut", "Claim", "Spawn", "Start"]));
    assert_eq!((running["wk"], running["ws"], running["nW"]), (2, 1, 1));
    assert!(
        !m.action_enabled("Raise", &running),
        "the worker is in hand and in time: {running:?}"
    );
    assert!(
        m.action_enabled("Raise", &walk(&m, &then(&["RunOut", "Claim", "Late"]))),
        "a fill that outruns the worker's window needs the owner"
    );
    let over = walk(
        &m,
        &then(&[
            "RunOut",
            "Claim",
            "Spawn",
            "Start",
            "Stops",
            "Over",
            "WorkerCools",
        ]),
    );
    assert!(!m.action_enabled("Claim", &over), "one worker an episode");
    assert!(
        m.action_enabled("Raise", &over),
        "nothing aterm runs can act: {over:?}"
    );
    let twice = interp::with_buggy(&m, 1);
    let wrong = walk(
        &twice,
        &then(&[
            "RunOut",
            "Claim",
            "Spawn",
            "Start",
            "Stops",
            "Over",
            "WorkerCools",
            "ClaimTwice",
        ]),
    );
    assert!(!twice.check_invariant("OneWorker", &wrong));
}

/// While the worker works, a round elects nobody; once it is over, a waiting round elects
/// again and its line goes.
#[test]
fn a_round_waits_on_the_worker_and_elects_again_once_it_is_over() {
    let m = disk_free_worker_model();
    let working = walk(&m, &then(&["Wait", "Claim", "Spawn", "Start", "Unwait"]));
    assert!(!m.action_enabled("Elect", &working), "{working:?}");
    assert!(!m.action_enabled("RunOut", &working), "{working:?}");
    let over = walk(
        &m,
        &then(&[
            "Wait", "Claim", "Spawn", "Start", "Unwait", "Expire", "Elect", "Type",
        ]),
    );
    assert_eq!((over["ep"], over["ws"], over["lcool"]), (2, 2, 1));
    let blind = interp::with_buggy(&m, 5);
    let wrong = walk(
        &blind,
        &then(&["Wait", "Claim", "Spawn", "Start", "LineWhileWorker"]),
    );
    assert!(!blind.check_invariant("NoLineWhileWorker", &wrong));
    let forgot = interp::with_buggy(&m, 6);
    let wrong = walk(
        &forgot,
        &then(&["Wait", "Claim", "Spawn", "Start", "LineAfterOver", "Elect"]),
    );
    assert!(!forgot.check_invariant("NoLineWhileWorker", &wrong));
}

/// A worker that works holds the episode open; a new episode that the parked one gives way to
/// keeps it, and its round waits on it.
#[test]
fn a_working_worker_holds_its_episode_and_crosses_into_the_next() {
    let m = disk_free_worker_model();
    let running = walk(&m, &then(&["RunOut", "Claim", "Spawn", "Start", "Relax"]));
    assert!(!m.action_enabled("End", &running), "{running:?}");
    let next = walk(&m, &then(&["RunOut", "Claim", "Spawn", "Start", "GiveWay"]));
    assert_eq!((next["ep"], next["wk"], next["nW"]), (1, 2, 0));
    assert!(!m.action_enabled("Elect", &next), "{next:?}");
    assert!(!m.action_enabled("Claim", &next), "{next:?}");
}

/// No episode can open for the line cooldown: a worker is due all the same; and a settled
/// episode needs one only while a crossing is projected.
#[test]
fn the_line_cooldown_and_a_crossing_after_a_settle_make_a_worker_due() {
    let m = disk_free_worker_model();
    let blocked = walk(
        &m,
        &then(&["Elect", "Type", "Settle", "Relax", "End", "Press"]),
    );
    assert_eq!(
        (blocked["ep"], blocked["need"], blocked["lcool"]),
        (0, 1, 1)
    );
    assert!(m.action_enabled("Claim", &blocked), "{blocked:?}");
    let settled = walk(&m, &then(&["Elect", "Type", "Settle"]));
    assert!(!m.action_enabled("Claim", &settled), "{settled:?}");
    let crossing = walk(&m, &then(&["Elect", "Type", "Settle", "Cross"]));
    assert!(m.action_enabled("Claim", &crossing), "{crossing:?}");
}
