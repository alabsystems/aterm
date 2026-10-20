// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

// aterm-wasm's tests of the sub-row scroll input surface: `scroll_input_api.rs`, which both web
// crates compile, includes this file into its tests, so these drive this
// crate's own frame path.

use super::*;

#[test]
fn render_presents_the_banked_residual_and_is_identity_at_zero() {
    let mut t = terminal_with_history();
    t.render().expect("renderable frame");
    let base = t.rgba();
    // Bank half a row: same engine offset, but the presented frame shifts.
    t.scroll_px(t.cell_height() as f64 / 2.0);
    assert!(t.scroll_frac_px() != 0, "non-vacuity: a shift is presented");
    t.render().expect("renderable frame");
    assert_ne!(t.rgba(), base, "the sub-row residual moves the pixels");
    // Snap resets: frac 0 must be BYTE-IDENTICAL to the whole-row frame
    // (the translate's identity-at-zero, through the whole export path).
    t.scroll_to_bottom();
    t.render().expect("renderable frame");
    assert_eq!(t.rgba(), base, "identity at frac 0 after the snap");
}
