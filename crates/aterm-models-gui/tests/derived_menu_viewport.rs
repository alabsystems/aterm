// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_models_gui::menu_viewport_model;
use aterm_spec::interp;

#[test]
fn a_viewport_preserves_its_interval_and_each_law_detects_its_own_defect() {
    let model = menu_viewport_model();
    let malformed = model.malformed_bounds();
    assert!(
        malformed.is_empty(),
        "bounds describe numeric ranges: {malformed:?}"
    );
    assert!(interp::violated_invariants(&model).is_empty());
    for (dial, law) in [
        (1, "RequestedRowIsRevealed"),
        (2, "ViewportIsNormalized"),
        (3, "HitNamesVisibleLogicalRow"),
    ] {
        assert!(
            interp::violated_invariants(&interp::with_buggy(&model, dial)).contains(law),
            "Buggy={dial} must break {law}"
        );
    }
}
