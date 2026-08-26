// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 exhaustive check for the exact cursor-input evidence policy. Tier-1
//! real-code projections live in `aterm-gui/src/app_input.rs`.

use aterm_spec::{derive::cursor_input_evidence_model, verify};

#[test]
fn cursor_input_evidence_proves_and_catches_each_missing_credential() {
    let model = cursor_input_evidence_model();
    if let Err(verify::NotRun { model }) = verify::prove_and_catch_tiered(&model, model.name) {
        eprintln!(
            "TIER-0 SKIPPED (this test is NOT a pass for it): `{model}` requires an unavailable external checker"
        );
    }
}

/// The authenticated bottom-scroll signal translates existing cell geometry
/// before the fresh-line material probe exists. The sole-next-generation hold
/// must preserve that resident trail, clear non-cell transients, and remain
/// dark until the exact material probe completes the fold.
#[test]
fn bottom_scroll_material_hold_preserves_only_translated_resident_cells() {
    let model = cursor_input_evidence_model();
    let healthy = aterm_spec::interp::with_buggy(&model, 0);

    let mut confirmed = healthy.init_state();
    assert!(healthy.fire("SeedTranslatedBottomScrollTrail", &mut confirmed));
    assert_eq!(confirmed["translated_trail_resident"], 1);
    assert_eq!(confirmed["non_cell_transient"], 1);
    assert_eq!(confirmed["bottom_candidate_pending"], 1);

    assert!(healthy.fire("HoldBottomScrollMaterialProbe", &mut confirmed));
    assert_eq!(confirmed["phase"], 1);
    assert_eq!(confirmed["bottom_material_hold"], 1);
    assert_eq!(confirmed["bottom_material_exact"], 0);
    assert_eq!(confirmed["translated_trail_resident"], 1);
    assert_eq!(confirmed["non_cell_transient"], 0);
    assert_eq!(confirmed["armed"], 0);
    assert_eq!(confirmed["trail_lit"], 0);
    assert!(healthy.check_invariant("BottomScrollHoldPreservesTranslatedTrail", &confirmed,));
    assert!(healthy.check_invariant("BottomScrollHoldClearsNonCellTransients", &confirmed,));
    assert!(healthy.check_invariant("BottomScrollHoldCannotAdmitFreshTrail", &confirmed,));

    assert!(healthy.fire("ConfirmHeldBottomScrollMaterial", &mut confirmed));
    assert_eq!(confirmed["phase"], 2);
    assert_eq!(confirmed["bottom_candidate_pending"], 0);
    assert_eq!(confirmed["bottom_material_hold"], 0);
    assert_eq!(confirmed["bottom_material_exact"], 1);
    assert_eq!(confirmed["translated_trail_resident"], 1);
    assert_eq!(confirmed["non_cell_transient"], 0);
    assert_eq!(confirmed["armed"], 1);
    assert_eq!(confirmed["bottom_scroll_fold"], 1);
    assert_eq!(confirmed["trail_lit"], 1);

    let mut rejected = healthy.init_state();
    for action in [
        "SeedTranslatedBottomScrollTrail",
        "HoldBottomScrollMaterialProbe",
        "RejectHeldBottomScrollMaterial",
    ] {
        assert!(
            healthy.fire(action, &mut rejected),
            "rejection trace: {action}"
        );
    }
    assert_eq!(rejected["phase"], 2);
    assert_eq!(rejected["bottom_candidate_pending"], 0);
    assert_eq!(
        rejected["translated_trail_resident"], 1,
        "a same-generation material rejection retires the candidate, not mature translated cells",
    );
    assert_eq!(rejected["non_cell_transient"], 0);
    assert_eq!(rejected["armed"], 0);
    assert_eq!(rejected["trail_lit"], 0);

    // Concrete blackout mutant: the exact same authenticated hold clears the
    // translated resident even though it still correctly clears transients
    // and refuses fresh admission. This pins the preservation property rather
    // than merely observing some unrelated Buggy=1 credential violation.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut blackout = buggy.init_state();
    assert!(buggy.fire("SeedTranslatedBottomScrollTrail", &mut blackout));
    assert!(buggy.fire("HoldBottomScrollMaterialProbe", &mut blackout));
    assert_eq!(blackout["translated_trail_resident"], 0);
    assert_eq!(blackout["non_cell_transient"], 0);
    assert_eq!(blackout["armed"], 0);
    assert_eq!(blackout["trail_lit"], 0);
    assert!(
        !buggy.check_invariant("BottomScrollHoldPreservesTranslatedTrail", &blackout),
        "the historical blackout must be a concrete invariant counterexample"
    );
    assert!(buggy.check_invariant("BottomScrollHoldClearsNonCellTransients", &blackout,));
    assert!(buggy.check_invariant("BottomScrollHoldCannotAdmitFreshTrail", &blackout,));
}
