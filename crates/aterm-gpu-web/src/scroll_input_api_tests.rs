// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

// aterm-gpu-web's tests of the sub-row scroll input surface: `scroll_input_api.rs`, which both web
// crates compile, includes this file into its tests, so these drive this
// crate's own frame path.

use super::*;

/// `init` is wasm-only, so this drives `build_frame` (the engine-side half
/// of `render`/`render_offscreen`) and checks the snapshot carries the
/// banked residual, and that a kept scratch is re-stamped to zero after a
/// snap.
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
    // Snap: the KEPT scratch must be re-stamped to zero next frame.
    t.scroll_to_bottom();
    t.build_frame();
    assert_eq!(t.frame_scratch.scroll_frac_px, 0, "no stale shift");
}
