// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn render_presents_the_banked_residual_and_is_identity_at_zero() {
    let mut t = terminal_with_history();
    t.render().expect("renderable frame");
    let base = t.rgba();
    t.scroll_px(t.cell_height() as f64 / 2.0);
    assert!(t.scroll_frac_px() != 0, "non-vacuity: a shift is presented");
    t.render().expect("renderable frame");
    assert_ne!(t.rgba(), base, "the sub-row residual moves the pixels");
    t.scroll_to_bottom();
    t.render().expect("renderable frame");
    assert_eq!(t.rgba(), base, "identity at frac 0 after the snap");
}
