// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! CROSS-CUTTING THEOREM (a) — THE GRID-INTEGRITY MASTER THEOREM.
//!
//! For ANY configuration — font metrics (→ `cell_h` via the W5 leading law),
//! monospace advance (→ `cell_w`), per-window display scale (→ interior pad, the
//! W12 mixed-DPI derivation) and raw window pixels (→ the W1 padding-absorption
//! split) — the terminal grid TILES the padded surface EXACTLY:
//!
//!   * **integer** — every cell rect edge is a whole device pixel (no cell is
//!     ever fractionally placed → the compositor never resamples);
//!   * **disjoint** — no two cells overlap;
//!   * **exact cover** — the cells plus the four padding bands partition the raw
//!     surface with no gap and no overlap, on BOTH axes.
//!
//! This is the theorem no single audit item owns. It COMPOSES three laws each
//! proven in isolation elsewhere:
//!   * **W1** `pad_split` — exact cover / maximal / pad-floor / near-even
//!     (`pad_absorption.rs`, `pad_split_kani`, `pad_absorption_model`);
//!   * **W5** `cell_h_baseline` — `cell_h = round((asc−desc+gap)·line_height)`,
//!     min 1 (`leading_law.rs`);
//!   * **W12** per-window pad = `round(12·scale)` (`main.rs`
//!     `per_window_padding_absorption_holds_for_each_window_scale`).
//!
//! Individually each guarantees its own axis/scalar; the SYSTEM property — that
//! their product is a clean 2-D integer tiling for every window in a mixed-DPI
//! session simultaneously — is what this file pins.
//!
//! ## Why an L0 lattice test (ty-waiver)
//!
//! The tiling identity is inherently multiplicative (`pad_lo + cols·cell_w +
//! pad_hi == win`, and the 2-D area accounting `cols·rows·cell_w·cell_h`), and
//! `cell_h`/`cell_w` come from `round(·)` of a scaled float. The `ty` Expr
//! language has no `*` / `/` / rounding, so — exactly as for the box-drawing
//! rounding law and the W1 arithmetic — the machine check is an exhaustive
//! lattice under plain `cargo test`. The ORDERING/coverage policies these
//! compose (near-even split, per-window no-clobber) are the parts `ty` carries
//! (`pad_absorption_model`, `per_window_metrics_model`).

use aterm_render::{PadSplit, cell_h_baseline, pad_split};

/// The W12 per-window interior pad, device px per edge, for a display `scale`:
/// `round(12·scale)`. Mirrors `aterm-gui`'s `pad_for_scale` (a binary-crate
/// private, so replicated here — the single source is exercised by the GUI's
/// own `per_window_padding_absorption_holds_for_each_window_scale`).
fn pad_for_scale(scale: f32) -> usize {
    (12.0 * scale).round().max(0.0) as usize
}

/// Monospace device cell width from an advance (W5): `round(advance)`, min 1.
/// Mirrors the crate-private `cell_w_from_advance`; the advance-rounding law is
/// owned separately — here it just supplies an integer `cell_w >= 1` to tile.
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

/// THE MASTER THEOREM, exhaustive over a mixed-DPI lattice of windows.
#[test]
fn grid_tiles_the_padded_surface_exactly_for_every_config() {
    // W12: a mixed-DPI session — laptop, fractional, external monitor, hi-dpi.
    let scales = [1.0f32, 1.25, 1.5, 2.0, 3.0];
    // W5 inputs: monospace verticals (ascent, descent<0, lineGap) and the config
    // line-height multiplier — feed the real `cell_h_baseline`.
    let vmetrics = [(9.6f32, -2.4, 0.0), (12.0, -3.0, 1.0), (15.5, -5.7, 2.3)];
    let line_heights = [0.9f32, 1.0, 1.25, 1.5];
    // W5 widths: monospace advances rounded to an integer cell.
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
                // W5 gives an INTEGER cell height, at least 1 px, with the
                // baseline inside the box (it is the tiling unit).
                assert!(cell_h >= 1, "cell_h must be a positive integer");
                assert!(
                    baseline >= 0 && (baseline as usize) <= cell_h,
                    "baseline {baseline} must sit inside its own cell (cell_h={cell_h})"
                );
                for &adv in &advances {
                    let cell_w = cell_w_from_advance(adv);
                    assert!(cell_w >= 1, "cell_w must be a positive integer");
                    distinct_cells.insert((cell_w, cell_h));
                    // Sweep window sizes from one padded cell up past several
                    // columns/rows, hitting every remainder residue on each axis.
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

    // NON-VACUITY: the lattice genuinely exercised mixed DPIs, real 2-D grids,
    // and the odd-remainder branch on BOTH axes (the case a lopsided split or a
    // fractional cell would corrupt).
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
    // In-domain only (a window smaller than one padded cell is CROPPED by the
    // present, not tiled — that totality edge is owned by `pad_absorption.rs`).
    if g.win_w < 2 * pad + g.cell_w || g.win_h < 2 * pad + g.cell_h {
        return;
    }
    let (cols, rows) = (g.x.cells, g.y.cells);
    assert!(cols >= 1 && rows >= 1, "grid must have at least one cell");

    // --- EXACT COVER on each axis (W1): bands + cells == raw surface. ---
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

    // --- INTEGER + DISJOINT + CONTIGUOUS: walk the cell rects. ---
    // Content origin (top-left of cell (0,0)) is the leading pad; every edge is
    // an integer multiple of the (integer) cell size added to it.
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

    // --- 2-D AREA ACCOUNTING: content rect area == Σ cell areas, and the four
    // bands + content == the whole surface (no double-counting, no hole). ---
    let content_w = cols * g.cell_w;
    let content_h = rows * g.cell_h;
    assert_eq!(
        content_w * content_h,
        (cols * rows) * (g.cell_w * g.cell_h),
        "cell areas must sum to the content rect"
    );
    // Surface = content + top band + bottom band + left band (over content rows)
    // + right band (over content rows). Decompose the border as full-width top &
    // bottom strips plus left/right strips spanning only the content rows.
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
