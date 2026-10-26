// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 0 of `DiskProjectedHold`: it holds at `Buggy = 0`, each law breaks under its own mutant
//! alone, the registry sweep finds nothing wrong with it, and walks follow the paths the design
//! names (`docs/DESIGN-disk-free-ask-2026-10-24.md` §8.3). Tier 1 is
//! `aterm-agent/tests/conformance_disk_projected_hold.rs`; the registry-wide sweep is
//! `aterm-models`' `non_vacuity`.

use aterm_models_harness::disk_projected_hold_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

/// Each mutant, its dial and the law it was written to break.
const MUTANTS: [(&str, i64, &str); 2] = [
    ("EndAtOwnPlain", 1, "NoEndWhileShort"),
    ("EndWhileProjected", 2, "NoEndWhileProjected"),
];

/// `m` with every mutant removed but `keep`.
fn alone(m: &Model, keep: &str) -> Model {
    let mut one = m.clone();
    one.actions
        .retain(|a| a.name == keep || MUTANTS.iter().all(|(mutant, ..)| *mutant != a.name));
    one
}

#[test]
fn disk_projected_hold_fires_exactly_its_committed_moves() {
    let m = disk_projected_hold_model();
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
fn each_mutant_breaks_its_own_law_alone_and_every_law_has_one() {
    let base = disk_projected_hold_model();
    for (mutant, dial, law) in MUTANTS {
        let m = alone(&base, mutant);
        let wrong = interp::with_buggy(&aterm_spec::walk::only(&m, law), dial);
        assert!(
            interp::violated_invariants(&wrong).contains(&law),
            "{mutant} (Buggy = {dial}) breaks {law}"
        );
        let right = interp::with_buggy(&aterm_spec::walk::only(&m, law), 0);
        assert!(
            interp::violated_invariants(&right).is_empty(),
            "the committed model keeps {law}"
        );
    }
    for inv in &base.invariants {
        let claim = MUTANTS.iter().any(|(_, _, law)| *law == inv.name);
        assert!(
            claim || inv.name.ends_with("Box"),
            "{} has its own mutant",
            inv.name
        );
    }
}

/// What `aterm-models`' `non_vacuity` sweep asks of every registered model, of this one alone:
/// no law broken with no mutant on, every invariant broken by some mutant, no bound a mutant
/// breaks, and every action dead with no mutant on a negative control.
#[test]
fn the_registry_sweep_finds_nothing_wrong_with_it() {
    let faults = aterm_spec::verify::sweep_faults(&[disk_projected_hold_model()]);
    assert!(faults.is_empty(), "{faults:#?}");
}

/// The design's case (§8.3): Q reads the volume short and opens the episode, P projects a crossing
/// of its lower floor, and its projection lapses at a free figure past P's floor that Q still reads
/// short. P's look holds the episode to Q's floor and ends nothing; the first version's single
/// floor slot (`EndAtOwnPlain`) ended it there.
#[test]
fn a_lapsed_projection_ends_nothing_while_the_higher_floor_reads_the_volume_short() {
    let m = disk_projected_hold_model();
    let st = walk(&m, &["Fall", "OpenShortQ", "Project", "HoldP"]);
    assert_eq!((st["ep"], st["bar"], st["rec"]), (1, 2, 0), "{st:?}");
    assert!(!m.action_enabled("EndP", &st), "{st:?}");
    let bug = interp::with_buggy(&m, 1);
    let ended = walk(&bug, &["Fall", "OpenShortQ", "Project", "EndAtOwnPlain"]);
    assert!(!bug.check_invariant("NoEndWhileShort", &ended), "{ended:?}");
}

/// P's projection opens the episode above both floors and holds it while it stands: Q's look
/// past its floor ends nothing until the record lapses, by P's warm look that projects nothing or
/// by time.
#[test]
fn a_standing_projection_holds_the_episode_until_it_lapses() {
    let m = disk_projected_hold_model();
    let st = walk(&m, &["OpenProjected", "HoldQ"]);
    assert_eq!((st["ep"], st["bar"], st["rec"]), (1, 2, 1), "{st:?}");
    assert!(!m.action_enabled("EndQ", &st), "{st:?}");
    let dropped = walk(&m, &["OpenProjected", "HoldQ", "EndP"]);
    assert_eq!(
        dropped["ep"], 0,
        "P's own lapsed look ends it past Q's floor"
    );
    let lapsed = walk(&m, &["OpenProjected", "HoldQ", "Lapse", "EndQ"]);
    assert_eq!(lapsed["ep"], 0, "Q's look ends it once the record lapsed");
    let bug = interp::with_buggy(&m, 2);
    let ended = walk(&bug, &["OpenProjected", "EndWhileProjected"]);
    assert!(
        !bug.check_invariant("NoEndWhileProjected", &ended),
        "{ended:?}"
    );
}
