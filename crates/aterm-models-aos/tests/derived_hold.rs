// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 state-space and negative-control checks for `aos_key_hold_model`.

use std::collections::BTreeSet;

use aterm_models_aos::aos_key_hold_model;
use aterm_spec::derive::{InvariantKind, Model};
use aterm_spec::interp::{self, State};

/// Each dial and the one law it is written to break.
const DIALS: [(i64, &str); 5] = [
    (1, "NothingLoggedWhileHeld"),
    (2, "OnlyEchoedKeysLogged"),
    (3, "HostedGlowMatchesItsLog"),
    (4, "AttachStartsFromRest"),
    (5, "OnlyEchoedKeysLogged"),
];

/// The state after firing `steps` in order from `model`'s initial state, each of which must be
/// enabled.
fn walk(model: &Model, steps: &[&str]) -> State {
    let mut state = model.init_state();
    for (done, step) in steps.iter().enumerate() {
        assert!(
            model.fire(step, &mut state),
            "`{step}` is enabled after {:?}",
            &steps[..done]
        );
    }
    state
}

#[test]
fn the_hold_holds_and_each_law_has_its_own_mutant() {
    let model = aos_key_hold_model();

    let actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(interp::fired_actions(&model), actions);

    let dials: Vec<i64> = DIALS.iter().map(|&(dial, _)| dial).collect();
    assert_eq!(model.mutant_dials(), dials);
    let claims: BTreeSet<&str> = model
        .invariants
        .iter()
        .filter(|i| i.kind == InvariantKind::Claim)
        .map(|i| i.name)
        .collect();
    assert_eq!(claims, DIALS.iter().map(|&(_, law)| law).collect());

    for (dial, law) in DIALS {
        assert_eq!(
            interp::violated_invariants(&interp::with_buggy(&model, dial)),
            BTreeSet::from([law]),
            "Buggy {dial}"
        );
    }
}

#[test]
fn a_held_key_is_logged_only_once_its_echo_shows() {
    let model = aos_key_hold_model();
    let s = walk(&model, &["Attach", "Hold", "Hold", "Tick"]);
    assert_eq!((s["held"], s["echoed"], s["leak"]), (2, 0, 0));
    assert!(!model.action_enabled("Release", &s), "no echo, no release");
    assert!(!model.action_enabled("Hold", &s), "at most Cap keys wait");
    assert!(
        !model.action_enabled("Key", &s),
        "a key while keys wait joins them"
    );

    let s = walk(&model, &["Attach", "Hold", "Hold", "Echo", "Release"]);
    assert_eq!((s["held"], s["echoed"], s["unproven"]), (1, 0, 0));
    assert!(
        !model.action_enabled("Release", &s),
        "the second key waits for its own echo"
    );
    let s = walk(
        &model,
        &[
            "Attach", "Hold", "Hold", "Echo", "Release", "Echo", "Release", "Key",
        ],
    );
    assert_eq!((s["att"], s["held"], s["glow"]), (1, 0, 0));

    let blind = interp::with_buggy(&model, 2);
    let s = walk(&blind, &["Attach", "Hold", "Release"]);
    assert!(!blind.check_invariant("OnlyEchoedKeysLogged", &s));
}

#[test]
fn a_key_logged_at_its_press_is_one_that_carries_no_text_or_is_echoed() {
    let model = aos_key_hold_model();
    let s = walk(&model, &["Attach", "Key", "Key"]);
    assert_eq!((s["att"], s["held"], s["unproven"]), (1, 0, 0));
    let s = walk(&model, &["Attach", "Hold"]);
    assert!(
        !model.action_enabled("Key", &s),
        "a key while keys wait joins them or stops the recording"
    );

    let unechoed = interp::with_buggy(&model, 5);
    let s = walk(&unechoed, &["Attach", "Key"]);
    assert_eq!(s["unproven"], 1, "a text key logged at its press, unechoed");
    assert!(!unechoed.check_invariant("OnlyEchoedKeysLogged", &s));
    assert!(
        !unechoed.action_enabled("Key", &walk(&unechoed, &["Attach", "Hold"])),
        "dial 5 keeps Key's guard: it breaks the claim, not the hold"
    );
}

#[test]
fn an_unproven_run_stops_the_recording_and_the_next_attach_starts_from_rest() {
    let model = aos_key_hold_model();
    let s = walk(&model, &["Attach", "Hold", "Echo", "Refute"]);
    assert_eq!((s["att"], s["held"], s["echoed"], s["glow"]), (0, 0, 0, 1));
    let s = walk(&model, &["Attach", "Hold", "Refute", "Attach"]);
    assert_eq!((s["att"], s["glow"], s["dirty"]), (1, 0, 0));
    let s = walk(&model, &["Attach", "Hold", "Detach"]);
    assert_eq!((s["att"], s["glow"]), (0, 1), "a dropped hold taints too");
    let s = walk(&model, &["Attach", "Detach"]);
    assert_eq!(s["glow"], 0, "negative control: a detach with no key held");

    let stale = interp::with_buggy(&model, 4);
    let s = walk(&stale, &["Private", "Attach"]);
    assert!(!stale.check_invariant("AttachStartsFromRest", &s));
    let gapped = interp::with_buggy(&model, 3);
    let s = walk(&gapped, &["Attach", "Hold", "Refute"]);
    assert_eq!(s["att"], 1);
    assert!(!gapped.check_invariant("HostedGlowMatchesItsLog", &s));
    let leaky = interp::with_buggy(&model, 1);
    let s = walk(&leaky, &["Attach", "Hold", "Tick"]);
    assert!(!leaky.check_invariant("NothingLoggedWhileHeld", &s));
}
