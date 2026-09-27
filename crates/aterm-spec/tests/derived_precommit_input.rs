// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `NativeUpdatePreCommitInput`
//! (`aterm_spec::derive::native_update_precommit_input_model`): a self-update
//! successor's pre-Commit input queue (gap #33). Proved at `Buggy = 0` and
//! caught at `Buggy = 1` on every invariant; the handoff's paths are walked as
//! the owner meets them. Tier-1 is `aterm-gui/src/precommit_input_conformance.rs`.

use aterm_spec::derive::{Model, native_update_precommit_input_model};
use aterm_spec::{interp, verify};

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

fn walk(m: &Model, actions: &[&str]) -> interp::State {
    let mut state = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

#[test]
fn precommit_input_proves_and_catches() {
    let model = native_update_precommit_input_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the pre-Commit input queue must stay enrolled in the spec-link registry"
    );
    let committed: Vec<&str> = interp::fired_actions(&model).into_iter().collect();
    for action in ["Type", "LoseFocus", "Drop", "Settle"] {
        assert!(
            committed.contains(&action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    verify::prove_and_catch_scalar(
        &model,
        "pre-Commit input: a reordered or lossy stream is repaired, and only such a stream",
    );
}

/// The owner's handoffs, the way they meet the queue. The 17-of-17 shape —
/// winit's startup focus loss for each new window, the activation's flip,
/// nothing typed — replays with no repair. Typing after those flips replays
/// with no repair. A chord queued and then ⌘-Tab away is repaired; so is a
/// queue that overflowed.
#[test]
fn precommit_input_walks_the_handoff_the_way_the_owner_meets_it() {
    let m = native_update_precommit_input_model();

    let startup = walk(&m, &["LoseFocus", "LoseFocus", "Settle"]);
    assert_eq!(
        (startup["incoherent"], startup["repaired"]),
        (0, 0),
        "the startup focus losses reorder nothing: {startup:?}"
    );

    let typed_after = walk(&m, &["LoseFocus", "Type", "Settle"]);
    assert_eq!(
        (typed_after["reordered"], typed_after["repaired"]),
        (0, 0),
        "input queued after a loss replays after it: {typed_after:?}"
    );

    let tabbed_away = walk(&m, &["Type", "LoseFocus", "Settle"]);
    assert_eq!(
        (
            tabbed_away["reordered"],
            tabbed_away["repaired"],
            tabbed_away["stranded"]
        ),
        (1, 1, 0),
        "a loss after input was queued is repaired: {tabbed_away:?}"
    );

    let overflowed = walk(&m, &["Type", "Drop", "Settle"]);
    assert_eq!(
        (overflowed["dropped"], overflowed["repaired"]),
        (1, 1),
        "a queue that lost an event is repaired: {overflowed:?}"
    );

    assert!(
        !m.action_enabled("Drop", &m.init_state()),
        "an empty queue has nothing to overflow"
    );
    for action in ["Type", "LoseFocus", "Drop", "Settle"] {
        assert!(
            !m.action_enabled(action, &tabbed_away),
            "after Commit the queue is gone: {action}"
        );
    }
}

/// `Buggy = 1` — the latch this replaced (every focus loss marks, the old
/// `incoherent |= pending` verbatim) and the silent drop (an event refused past
/// the cap marks nothing) — is caught on each invariant by its own schedule.
#[test]
fn precommit_input_the_defects_are_caught_on_every_invariant() {
    let m = interp::with_buggy(&native_update_precommit_input_model(), 1);
    let (noise, _) =
        interp::bmc(&only(&m, "RepairOnlyForACause")).expect_err("the unconditional latch");
    assert_eq!(
        (
            noise["repaired"],
            noise["reordered"],
            noise["dropped"],
            noise["queued"]
        ),
        (1, 0, 0, 0),
        "a repair with no cause, nothing ever typed: {noise:?}"
    );
    let (silent, _) =
        interp::bmc(&only(&m, "ReorderedInputIsRepaired")).expect_err("the silent drop");
    assert_eq!(
        (
            silent["dropped"],
            silent["incoherent"],
            silent["repaired"],
            silent["stranded"]
        ),
        (1, 0, 0, 1),
        "a stream that lost an event replayed on trust: {silent:?}"
    );
    assert!(verify::uncaught_invariants(&native_update_precommit_input_model()).is_empty());
}
