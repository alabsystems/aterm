// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid integrity: for any font metrics (`cell_h` from the leading law), monospace advance
//! (`cell_w`), per-window display scale (the interior pad) and raw window size (the
//! padding-absorption split), the grid tiles the padded surface exactly. Every cell edge is
//! a whole device pixel, no two cells overlap, and cells plus the four padding bands
//! partition the surface on both axes.
//!
//! It composes three laws proven alone elsewhere: `pad_split` (`pad_absorption.rs`,
//! `pad_absorption_model`), `cell_h_baseline` (`leading_law.rs`), and the per-window pad
//! `round(12·scale)` (aterm-gui's `per_window_padding_absorption_holds_for_each_window_scale`).
//! The tiling identity is multiplicative and the cell sizes come from `round(·)`, which
//! ty's `Expr` language lacks, so this exhaustive lattice is the machine check; the ordering
//! policies they compose are in `pad_absorption_model` and `per_window_metrics_model`.

use aterm_render::{PadSplit, cell_h_baseline, pad_split};

/// The default per-window interior pad, device px per edge: `round(12·scale)`. Mirrors
/// `aterm-gui`'s crate-private `pad_for_scale`, which its own
/// `per_window_padding_absorption_holds_for_each_window_scale` exercises.
fn pad_for_scale(scale: f32) -> usize {
    (12.0 * scale).round().max(0.0) as usize
}

/// Device cell width from a monospace advance: `round(advance)`, min 1. Mirrors the
/// crate-private `cell_w_from_advance`.
fn cell_w_from_advance(advance: f32) -> usize {
    (advance.round() as usize).max(1)
}

/// A per-window configuration and the grid it induces on a raw surface.
struct Grid {
    win_w: usize,
    win_h: usize,
    cell_w: usize,
    cell_h: usize,
    x: PadSplit, // horizontal split (cols)
    y: PadSplit, // vertical split (rows)
}

/// The grid-integrity theorem, exhaustive over a mixed-DPI lattice of windows.
#[test]
fn grid_tiles_the_padded_surface_exactly_for_every_config() {
    // A mixed-DPI session: laptop, fractional, external monitor, hi-dpi.
    let scales = [1.0f32, 1.25, 1.5, 2.0, 3.0];
    // Monospace verticals (ascent, descent<0, lineGap) and line-height multipliers, fed to
    // the real `cell_h_baseline`.
    let vmetrics = [(9.6f32, -2.4, 0.0), (12.0, -3.0, 1.0), (15.5, -5.7, 2.3)];
    let line_heights = [0.9f32, 1.0, 1.25, 1.5];
    // Monospace advances, rounded to an integer cell.
    let advances = [6.4f32, 7.0, 8.5, 10.2];

    let mut odd_x = 0u64; // non-vacuity: odd-remainder split reached on each axis
    let mut odd_y = 0u64;
    let mut multi_cell = 0u64; // non-vacuity: real >1×>1 grids exercised
    let mut distinct_pads = std::collections::BTreeSet::new();
    let mut distinct_cells = std::collections::BTreeSet::new();
    let mut checked = 0u64;

    for &scale in &scales {
        let pad = pad_for_scale(scale);
        distinct_pads.insert(pad);
        for &(asc, desc, gap) in &vmetrics {
            for &lh in &line_heights {
                let (cell_h, baseline) = cell_h_baseline(asc, desc, gap, lh);
                // An integer cell height >= 1 with the baseline inside the box.
                assert!(cell_h >= 1, "cell_h must be a positive integer");
                assert!(
                    baseline >= 0 && (baseline as usize) <= cell_h,
                    "baseline {baseline} must sit inside its own cell (cell_h={cell_h})"
                );
                for &adv in &advances {
                    let cell_w = cell_w_from_advance(adv);
                    assert!(cell_w >= 1, "cell_w must be a positive integer");
                    distinct_cells.insert((cell_w, cell_h));
                    // Window sizes from one padded cell up past several columns/rows,
                    // hitting every remainder residue on each axis.
                    for wcols in 0..(3 * cell_w) {
                        let win_w = 2 * pad + 5 * cell_w + wcols;
                        for hrows in 0..(3 * cell_h) {
                            let win_h = 2 * pad + 4 * cell_h + hrows;
                            let g = Grid {
                                win_w,
                                win_h,
                                cell_w,
                                cell_h,
                                x: pad_split(win_w, pad, cell_w),
                                y: pad_split(win_h, pad, cell_h),
                            };
                            assert_tiles(&g, pad);
                            if g.x.pad_lo != g.x.pad_hi {
                                odd_x += 1;
                            }
                            if g.y.pad_lo != g.y.pad_hi {
                                odd_y += 1;
                            }
                            if g.x.cells > 1 && g.y.cells > 1 {
                                multi_cell += 1;
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
    }

    // Non-vacuity: mixed DPIs, real 2-D grids, and the odd-remainder branch on both axes.
    assert!(
        distinct_pads.len() >= 4,
        "must span distinct per-window DPIs ({distinct_pads:?})"
    );
    assert!(
        distinct_cells.len() >= 6,
        "must span distinct cell boxes ({})",
        distinct_cells.len()
    );
    assert!(
        multi_cell > 0,
        "must exercise real multi-row multi-column grids"
    );
    assert!(
        odd_x > 0 && odd_y > 0,
        "must reach an odd remainder on each axis (x={odd_x} y={odd_y})"
    );
    assert!(checked > 10_000, "lattice must be dense ({checked})");
}

/// Assert one grid is an integer, disjoint, exact-cover tiling of its surface.
fn assert_tiles(g: &Grid, pad: usize) {
    // In-domain only: a window smaller than one padded cell is cropped by the present, not
    // tiled (that edge belongs to `pad_absorption.rs`).
    if g.win_w < 2 * pad + g.cell_w || g.win_h < 2 * pad + g.cell_h {
        return;
    }
    let (cols, rows) = (g.x.cells, g.y.cells);
    assert!(cols >= 1 && rows >= 1, "grid must have at least one cell");

    // Exact cover on each axis: bands + cells == raw surface.
    assert_eq!(
        g.x.pad_lo + cols * g.cell_w + g.x.pad_hi,
        g.win_w,
        "x exact cover failed: {g:?}",
    );
    assert_eq!(
        g.y.pad_lo + rows * g.cell_h + g.y.pad_hi,
        g.win_h,
        "y exact cover failed: {g:?}",
    );
    // Pads keep the configured floor (the border never collapses into a cell).
    assert!(g.x.pad_lo >= pad && g.x.pad_hi >= pad, "x pad floor: {g:?}");
    assert!(g.y.pad_lo >= pad && g.y.pad_hi >= pad, "y pad floor: {g:?}");

    // Integer, disjoint, contiguous: every edge is the leading pad plus a multiple of the
    // integer cell size.
    let (ox, oy) = (g.x.pad_lo, g.y.pad_lo);
    let mut prev_right = ox;
    for c in 0..cols {
        let left = ox + c * g.cell_w;
        let right = left + g.cell_w;
        assert_eq!(
            left, prev_right,
            "columns must be contiguous, no gap/overlap: {g:?}"
        );
        assert!(
            right <= g.win_w - g.x.pad_hi,
            "column {c} overran the content band: {g:?}"
        );
        prev_right = right;
    }
    assert_eq!(
        prev_right,
        g.win_w - g.x.pad_hi,
        "columns must exactly reach the trailing pad: {g:?}"
    );

    let mut prev_bot = oy;
    for r in 0..rows {
        let top = oy + r * g.cell_h;
        let bot = top + g.cell_h;
        assert_eq!(
            top, prev_bot,
            "rows must be contiguous, no gap/overlap: {g:?}"
        );
        assert!(
            bot <= g.win_h - g.y.pad_hi,
            "row {r} overran the content band: {g:?}"
        );
        prev_bot = bot;
    }
    assert_eq!(
        prev_bot,
        g.win_h - g.y.pad_hi,
        "rows must exactly reach the trailing pad: {g:?}"
    );

    // 2-D area accounting: cell areas sum to the content rect, and content plus the four
    // bands is the whole surface.
    let content_w = cols * g.cell_w;
    let content_h = rows * g.cell_h;
    assert_eq!(
        content_w * content_h,
        (cols * rows) * (g.cell_w * g.cell_h),
        "cell areas must sum to the content rect"
    );
    // The border decomposes into full-width top and bottom strips plus left and right
    // strips spanning only the content rows.
    let top_band = g.win_w * g.y.pad_lo;
    let bot_band = g.win_w * g.y.pad_hi;
    let left_band = g.x.pad_lo * content_h;
    let right_band = g.x.pad_hi * content_h;
    assert_eq!(
        content_w * content_h + top_band + bot_band + left_band + right_band,
        g.win_w * g.win_h,
        "the content rect + four padding bands must partition the surface exactly: {g:?}",
    );
}

impl std::fmt::Debug for Grid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Grid{{win={}x{} cell={}x{} x={:?} y={:?}}}",
            self.win_w, self.win_h, self.cell_w, self.cell_h, self.x, self.y
        )
    }
}
