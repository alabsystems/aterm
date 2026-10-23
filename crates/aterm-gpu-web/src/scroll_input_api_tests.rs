// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn render_snapshot_carries_the_banked_residual() {
    let mut t = terminal_with_history();
    t.scroll_px(t.cell_height() as f64 / 2.0);
    let rows = t.rows;
    t.build_frame();
    assert!(t.frame_scratch.scroll_frac_px < 0, "residual is presented");
    assert_eq!(
        (t.frame_scratch.grid_top_row, t.frame_scratch.grid_bot_row),
        (0, rows),
        "the whole canvas frame is grid band (no spliced chrome)"
    );
    t.scroll_to_bottom();
    t.build_frame();
    assert_eq!(t.frame_scratch.scroll_frac_px, 0, "no stale shift");
}
