// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::shared_file_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

/// The state `steps` reach from the model's first, each step enabled where it is taken.
fn walk(model: &Model, steps: &[&str]) -> State {
    let mut state = model.init_state();
    for step in steps {
        assert!(
            model.fire(step, &mut state),
            "{step} is not enabled at {state:?}"
        );
    }
    state
}

/// The laws `state` breaks.
fn broken(model: &Model, state: &State) -> Vec<&'static str> {
    model
        .invariants
        .iter()
        .map(|law| law.name)
        .filter(|law| !model.check_invariant(law, state))
        .collect()
}

fn enabled(model: &Model, state: &State, among: &[&'static str]) -> Vec<&'static str> {
    among
        .iter()
        .copied()
        .filter(|action| model.action_enabled(action, state))
        .collect()
}

const PUBLISHER: [&str; 3] = ["Sign", "Done", "Hold"];

/// A succession completes, the runtime pass snapshots the successor's home, and the completion
/// is invalidated: the next pass opens the original home as unsettled, keeps the snapshot of
/// the later generation and says `held`.
#[test]
fn an_unsettled_pass_at_the_original_home_keeps_the_successors_snapshot_and_says_held() {
    let model = shared_file_model();
    let steps = [
        "ChooseRead",
        "Grow",
        "Fence",
        "Admit",
        "Materialize",
        "Invalidate",
    ];
    let unsettled = walk(&model, &steps);
    assert_eq!(
        (unsettled["custody"], unsettled["era"], unsettled["rfr"]),
        (2, 0, 1),
        "the pass opens the original home, with the frontier it was fenced at"
    );
    assert_eq!(
        (unsettled["sera"], unsettled["sfr"], unsettled["word"]),
        (1, 0, 1)
    );
    let mut held = unsettled.clone();
    assert!(model.fire("Materialize", &mut held));
    assert_eq!(
        (held["sera"], held["sfr"], held["word"], held["src"]),
        (1, 0, 2, 2),
        "the older home replaced nothing, and the word is the unsettled pass's"
    );
    assert_eq!(broken(&model, &held), [] as [&str; 0]);
    // The completion made again, the same snapshot is live once more.
    let mut live = held.clone();
    assert!(model.fire("Admit", &mut live));
    assert!(model.fire("Materialize", &mut live));
    assert_eq!(
        (live["sera"], live["sfr"], live["word"], live["src"]),
        (1, 0, 1, 0)
    );

    // NEGATIVE CONTROL: a pass with no replace rule writes the original home's view over it.
    let mutant = interp::with_buggy(&model, 2);
    let mut replaced = walk(&mutant, &steps);
    assert!(mutant.fire("Materialize", &mut replaced));
    assert_eq!((replaced["sera"], replaced["sfr"]), (0, 1));
    assert_eq!(broken(&mutant, &replaced), ["SnapshotMonotone"]);
}

/// `aterm file refresh` opens live custody, the Mac is fenced, and the runtime's pass records
/// `held`: the view is the older pass by then, and it records nothing.
#[test]
fn a_view_taken_before_a_fence_never_turns_the_fenced_pass_s_word_live() {
    let model = shared_file_model();
    let steps = ["ChooseRead", "RefreshLook", "Fence", "Materialize"];
    let held = walk(&model, &steps);
    assert_eq!(
        (held["word"], held["src"], held["look"], held["vsrc"]),
        (2, 1, 2, 0)
    );
    let mut after = held.clone();
    assert!(model.fire("RefreshWrite", &mut after));
    for kept in ["sera", "sfr", "word", "src", "ssecret", "hera", "hfr"] {
        assert_eq!(after[kept], held[kept], "{kept}");
    }
    assert_eq!(after["look"], 0);
    // A view no pass has recorded since is the newest pass, and records its own word.
    let unraced = walk(
        &model,
        &[
            "ChooseRead",
            "Fence",
            "Materialize",
            "Admit",
            "RefreshLook",
            "RefreshWrite",
        ],
    );
    assert_eq!(
        (unraced["sera"], unraced["word"], unraced["src"]),
        (1, 1, 0)
    );

    // Whatever the walk, a view taken before a recorded pass changes nothing a reader sees.
    let mut raced = 0;
    for state in interp::reachable_states(&model) {
        if state["look"] != 2 {
            continue;
        }
        raced += 1;
        for next in model.successors("RefreshWrite", &state) {
            for kept in ["sera", "sfr", "word", "src", "ssecret"] {
                assert_eq!(next[kept], state[kept], "{kept} at {state:?}");
            }
        }
    }
    assert!(raced > 3, "the reading walk reached {raced} raced views");

    // NEGATIVE CONTROL: a refresh that writes what it took marks the fenced Mac's file live.
    let mutant = interp::with_buggy(&model, 2);
    let mut live = walk(&mutant, &steps);
    assert!(mutant.fire("RefreshWrite", &mut live));
    assert_eq!((live["word"], live["custody"]), (1, 1));
    assert_eq!(broken(&mutant, &live), ["SnapshotMonotone"]);
}

/// The publisher signs, crashes before the record's witness, and the Mac is fenced. The
/// successor imports the witnessed prefix, so its home has no record of the intent: it signs
/// the intent again, and the intent has one record.
#[test]
fn a_record_the_successor_never_imported_is_signed_again_and_counted_once() {
    let model = shared_file_model();
    let fenced = walk(&model, &["ChooseWrite", "Accept", "Sign", "Fence"]);
    assert_eq!(enabled(&model, &fenced, &PUBLISHER), [] as [&str; 0]);

    let mut dropped = fenced.clone();
    assert!(model.fire("AdmitUnwitnessed", &mut dropped));
    assert_eq!((dropped["mine"], dropped["recs"], dropped["q"]), (0, 0, 1));
    assert_eq!(enabled(&model, &dropped, &PUBLISHER), ["Sign"]);
    let mut again = dropped.clone();
    assert!(model.fire("Sign", &mut again));
    assert_eq!((again["recs"], again["fsig"]), (1, 0));
    assert!(model.fire("Done", &mut again));
    assert_eq!((again["q"], again["h"], again["p"]), (0, 0, 1));
    assert_eq!(broken(&model, &again), [] as [&str; 0]);

    // The record imported, the intent is done with nothing signed again.
    let mut imported = fenced.clone();
    assert!(model.fire("Admit", &mut imported));
    assert_eq!(enabled(&model, &imported, &PUBLISHER), ["Done"]);

    // NEGATIVE CONTROL: a publisher that remembers signing, and counts the intent published
    // with no record in the live home, takes no step of the model.
    let mut counted = dropped.clone();
    counted.insert("q", 0);
    counted.insert("p", 1);
    assert_eq!(interp::admits(&model, &dropped, &counted), None);
    assert_eq!(interp::admits(&model, &imported, &counted), Some("Done"));
}

/// The publisher signs and crashes before the dequeue; a peer that ingested the record writes
/// over it. The intent's record is in history, so the intent is published, not held.
#[test]
fn a_peer_that_replaced_this_macs_record_leaves_its_intent_published_not_held() {
    let model = shared_file_model();
    let replaced = walk(&model, &["ChooseWrite", "Accept", "Sign", "PeerReplaces"]);
    assert_eq!(
        (replaced["mine"], replaced["cb"], replaced["recs"]),
        (0, 1, 1),
        "the register holds the peer's candidate alone, and history this Mac's record"
    );
    assert_eq!(enabled(&model, &replaced, &PUBLISHER), ["Done"]);
    let mut done = replaced.clone();
    assert!(model.fire("Done", &mut done));
    assert_eq!((done["q"], done["h"], done["p"]), (0, 0, 1));
    assert_eq!(broken(&model, &done), [] as [&str; 0]);

    // NEGATIVE CONTROL: the same register with no record of the intent is one that moved.
    let moved = walk(&model, &["ChooseWrite", "Accept", "PeerWrite"]);
    assert_eq!((moved["mine"], moved["cb"], moved["recs"]), (0, 1, 0));
    assert_eq!(enabled(&model, &moved, &PUBLISHER), ["Hold"]);
    // Neither a retry that signs twice nor a publisher that writes over a moved register signs
    // over the peer's write there: each mutant keeps to its own law.
    for dial in [5, 6] {
        let mutant = interp::with_buggy(&model, dial);
        assert_eq!(enabled(&mutant, &replaced, &PUBLISHER), ["Done"], "{dial}");
    }
}
