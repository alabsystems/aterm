// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `LeftWrite`: it holds at `Buggy = 0`, each law breaks under its own mutant alone, the
//! liveness mutant has an invariant twin, and walks follow the paths the slice-2 plan's 2.9b
//! names. Tier 1 is `aterm-agent/tests/conformance_left.rs`. The non-vacuity sweep and the
//! liveness audit are `aterm-models`' `non_vacuity`.

use aterm_models_harness::{left_write_liveness, left_write_model};
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

/// Each mutant, its dial and the law it was written to break.
const MUTANTS: [(&str, i64, &str); 5] = [
    ("KeepOnClear", 1, "AClearedLineGivesItsClaimBack"),
    ("ReleaseOnLeft", 2, "ToldOnce"),
    ("SubmitWithoutPrompt", 3, "NothingSentWithoutPrompt"),
    ("ToldOnClear", 4, "BookedAsTold"),
    ("ReleaseUncounted", 5, "ToldOnce"),
];

/// A bound on the states the model reaches at any dial.
const STATE_BOUND: usize = 5_000;

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

#[test]
fn left_write_fires_exactly_its_committed_moves() {
    let m = left_write_model();
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
    let m = left_write_model();
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

/// The liveness mutant (dial 1) is dead with no mutant on, fires at its dial, and breaks an
/// invariant twin, so a safety walk catches it too.
#[test]
fn the_liveness_mutant_has_an_invariant_twin() {
    let m = left_write_model();
    let committed = interp::fired_actions(&interp::with_buggy(&m, 0));
    let dialled = interp::fired_actions(&interp::with_buggy(&m, 1));
    for mutant in &left_write_liveness().mutants {
        assert!(!committed.contains(mutant), "{mutant} is dead at Buggy = 0");
        assert!(dialled.contains(mutant), "{mutant} fires at Buggy = 1");
    }
    assert!(
        interp::violated_invariants(&interp::with_buggy(&m, 1))
            .contains("AClearedLineGivesItsClaimBack")
    );
}

#[test]
fn left_write_stays_small() {
    let m = left_write_model();
    for dial in std::iter::once(0).chain(MUTANTS.iter().map(|(_, dial, _)| *dial)) {
        let n = interp::reachable_states(&interp::with_buggy(&m, dial)).len();
        assert!(n < STATE_BOUND, "Buggy = {dial}: {n} states");
    }
}

/// A line left is told by no door while it stands; the person's Enter tells it at the look, once,
/// and a cleared composer gives it back for one tell later.
#[test]
fn a_left_line_is_settled_at_the_look() {
    let m = left_write_model();
    let left = walk(&m, &["Leaves"]);
    assert_eq!((left["st"], left["comp"], left["reached"]), (1, 1, 0));
    assert!(!m.action_enabled("Types", &left), "no door writes it again");
    assert!(
        !m.action_enabled("LookTold", &left),
        "nothing settles while it stands"
    );
    assert!(!m.action_enabled("LookReleased", &left));
    let told = walk(&m, &["Leaves", "PersonSends", "LookTold"]);
    assert_eq!((told["st"], told["reached"], told["booked"]), (2, 1, 1));
    assert!(!m.action_enabled("Types", &told), "told once");
    let back = walk(&m, &["Leaves", "PersonClears", "LookReleased", "Types"]);
    assert_eq!((back["st"], back["reached"], back["booked"]), (2, 1, 1));
    let sent = walk(&m, &["Leaves", "DraftSends"]);
    assert_eq!((sent["st"], sent["reached"], sent["booked"]), (2, 1, 1));
    let gone = walk(&m, &["Leaves", "Ends", "GoneReleased"]);
    assert_eq!(gone["st"], 0);
    assert!(
        !m.action_enabled("Types", &gone),
        "nothing is typed into a gone session"
    );
}

/// Once `prompt` is withdrawn the draft step sends nothing `prompt` left: the person may, and the
/// look settles it.
#[test]
fn withdrawn_prompt_leaves_the_line_to_the_person() {
    let m = left_write_model();
    let withdrawn = walk(&m, &["Leaves", "Withdraw"]);
    assert!(!m.action_enabled("DraftSends", &withdrawn), "{withdrawn:?}");
    assert!(m.action_enabled("PersonSends", &withdrawn));
    let told = walk(&m, &["Leaves", "Withdraw", "PersonSends", "LookTold"]);
    assert_eq!(
        (told["st"], told["reached"], told["booked"], told["obs"]),
        (2, 1, 1, 0)
    );
    let wrong = interp::with_buggy(&m, 3);
    let sent = walk(&wrong, &["Leaves", "Withdraw", "SubmitWithoutPrompt"]);
    assert!(!wrong.check_invariant("NothingSentWithoutPrompt", &sent));
}

/// Booked untold at its write (every door before 2.9b), the line goes out and the episode is told
/// again; booked told though cleared, the row says told and nothing reached the agent.
#[test]
fn the_old_doors_tell_twice_or_lie() {
    let m = left_write_model();
    let old = interp::with_buggy(&m, 2);
    let twice = walk(&old, &["ReleaseOnLeft", "PersonSends", "Types"]);
    assert_eq!(twice["reached"], 2);
    assert!(!old.check_invariant("ToldOnce", &twice));
    let lie = interp::with_buggy(&m, 4);
    let told = walk(&lie, &["Leaves", "PersonClears", "ToldOnClear"]);
    assert_eq!((told["booked"], told["reached"]), (1, 0));
    assert!(!lie.check_invariant("BookedAsTold", &told));
}

/// A count no look may weigh (a successor's, or one from before an outage: each host process
/// counts its own submits from 0) is never weighed: the look at a clear composer books the line
/// told whether the person sent it or cleared it, and no door types it again; the cleared one is
/// the one tell lost (`lost`). Released there (`ReleaseUncounted`), the line the person sent is
/// typed a second time.
#[test]
fn a_count_no_look_may_weigh_tells_the_line_and_never_twice() {
    let m = left_write_model();
    let recounted = walk(&m, &["Leaves", "Recount"]);
    assert_eq!((recounted["st"], recounted["known"]), (1, 0));
    for weighs in ["LookTold", "LookReleased"] {
        let cleared = walk(&m, &["Leaves", "Recount", "PersonClears"]);
        assert!(!m.action_enabled(weighs, &cleared), "{weighs}: {cleared:?}");
    }
    let sent = walk(&m, &["Leaves", "Recount", "PersonSends", "LookUncounted"]);
    assert_eq!(
        (sent["st"], sent["reached"], sent["booked"], sent["lost"]),
        (2, 1, 1, 0)
    );
    assert!(!m.action_enabled("Types", &sent), "told once");
    let cleared = walk(&m, &["Leaves", "Recount", "PersonClears", "LookUncounted"]);
    assert_eq!(
        (
            cleared["st"],
            cleared["reached"],
            cleared["booked"],
            cleared["lost"]
        ),
        (2, 0, 1, 1),
        "a tell lost, never a second"
    );
    let gone = walk(&m, &["Leaves", "Recount", "Ends", "GoneReleased"]);
    assert_eq!(gone["st"], 0);
    let wrong = interp::with_buggy(&m, 5);
    let twice = walk(
        &wrong,
        &[
            "Leaves",
            "PersonSends",
            "Recount",
            "ReleaseUncounted",
            "Types",
        ],
    );
    assert_eq!(twice["reached"], 2);
    assert!(!wrong.check_invariant("ToldOnce", &twice));
}
