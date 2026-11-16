// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The drawable worker's wake and reshape laws: they hold with no mutant on, and each falls to
//! its mutants.

use std::collections::BTreeSet;

use aterm_spec::interp;

#[test]
fn the_wake_and_reshape_laws_hold_over_every_interleaving_and_each_mutant_breaks_what_it_names() {
    let model = aterm_models_render::metal_drawable_acquire_model();
    let states = interp::bmc(&model)
        .unwrap_or_else(|(state, law)| panic!("{law} is broken with no mutant on, at {state:?}"));
    let fired = interp::fired_actions(&model);
    let dead: Vec<_> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !fired.contains(name))
        .collect();
    assert!(dead.is_empty(), "actions that never fire: {dead:?}");
    assert_eq!(model.mutant_dials(), (1..=11).collect::<Vec<i64>>());
    for (dial, mutant, laws) in [
        (5, "a close that wakes", &["WakeOnlyWhenWanted"][..]),
        (
            7,
            "the flag taken before the result is published",
            &["NoStrandedWait"],
        ),
        (
            8,
            "a frame that sets the flag and does not look again",
            &["NoStrandedWait"],
        ),
        (
            9,
            "a frame that takes a stale result and queues nothing",
            &["NoStrandedWait"],
        ),
        (10, "a wake nobody asked for", &["WakeOnlyWhenWanted"]),
        (
            11,
            "a frame that queues its request on a layer that still owes its reshape",
            &["ShapedBeforeRequested"],
        ),
    ] {
        let broken = interp::violated_invariants(&interp::with_buggy(&model, dial));
        assert_eq!(
            broken,
            laws.iter().copied().collect::<BTreeSet<_>>(),
            "mutant {dial} ({mutant})"
        );
    }
    eprintln!("MetalDrawableAcquire: {states} states with no mutant on");
}
