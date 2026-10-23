// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `TrickFlashLifecycle` (`aterm_models_effects::trick_flash_lifecycle_model`):
//! proved at `Buggy = 0`, caught at `Buggy = 1`, every action live. Tier-1 is
//! `aterm-effects/tests/conformance_trick_flash.rs`.

use aterm_models_effects::trick_flash_lifecycle_model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

#[test]
fn trick_flash_lifecycle_has_no_dead_action() {
    let model = trick_flash_lifecycle_model();
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires: {fired:?}"
    );

    // Closing the bound sibling, then the bind that follows: a found flash
    // stays live, an unfound word goes. The mutant's bind takes the first.
    assert_eq!(
        walk(&model, &["NotePane", "Find", "CloseOther", "Rebind"])["phase"],
        2
    );
    assert_eq!(walk(&model, &["NotePane", "Rebind"])["phase"], 0);
    let buggy = interp::with_buggy(&model, 1);
    let s = walk(&buggy, &["NotePane", "Find", "Rebind"]);
    assert!(!buggy.check_invariant("ABindFromUnboundKeepsAPanesFoundFlash", &s));
}
