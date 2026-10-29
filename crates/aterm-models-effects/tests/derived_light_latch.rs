// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `LightLatch` (`aterm_models_effects::light_latch_model`): proved at `Buggy = 0`,
//! every action live, and each control caught on the path its defect takes. Tier-1 is
//! `aterm-effects/tests/light_latch_conformance.rs`.

use aterm_models_effects::light_latch_model;
use aterm_spec::interp;
use aterm_spec::walk::walk;

#[test]
fn light_latch_holds_and_each_control_breaks_its_law() {
    let model = light_latch_model();
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires: {fired:?}"
    );
    let explored = interp::bmc(&model).expect("every law holds at Buggy = 0");
    assert!(explored > 0);

    // A light scheme's first light frame enters; its DECSCNM flash leaves at once and comes back
    // at once; a page away past the hold waits the hold.
    let lit = ["Flip", "Page", "Frame"];
    assert_eq!(walk(&model, &lit)["on"], 1);
    let flash = [&lit[..], &["Page", "Frame", "Tick", "Page", "Frame"]].concat();
    let s = walk(&model, &flash[..5]);
    assert_eq!(
        (s["on"], s["fell"]),
        (0, 1),
        "the flash leaves on its frame"
    );
    assert_eq!(walk(&model, &flash)["on"], 1, "and comes back at once");
    let away = [
        &lit[..],
        &[
            "Page", "Frame", "Tick", "Tick", "Tick", "Tick", "Page", "Frame",
        ],
    ]
    .concat();
    assert_eq!(walk(&model, &away)["on"], 0, "away past the hold waits it");
    // A key moved on a dark page enters on the first light frame after it.
    let pending = ["Frame", "Flip", "Frame", "Tick", "Page", "Frame"];
    assert_eq!(walk(&model, &pending)["on"], 1);
    // A page back after the hold waits it on every frame of the hold, its second too.
    let away_twice = [&away[..], &["Frame"]].concat();
    let s = walk(&model, &away_twice);
    assert_eq!(
        (s["on"], s["glast"]),
        (0, 4),
        "the hold is the last lit frame's"
    );

    for (dial, path, broken) in [
        (1, &away[..], "EntryIsLicensed"),
        (
            1,
            &["Flip", "Page", "Frame", "Page", "Rescheme", "Frame"][..],
            "EveryFrameSeesTheKey",
        ),
        (2, &pending[..], "ALicensedLightFrameEnters"),
        (3, &["Page", "Frame"][..], "NoDarkSchemeEnters"),
        (4, &flash[..5], "ExitIsImmediate"),
        (5, &away_twice[..], "EntryIsLicensed"),
    ] {
        let buggy = interp::with_buggy(&model, dial);
        let s = walk(&buggy, path);
        assert!(
            !buggy.check_invariant(broken, &s),
            "Buggy = {dial}: {path:?} breaks {broken}: {s:?}"
        );
    }
}
