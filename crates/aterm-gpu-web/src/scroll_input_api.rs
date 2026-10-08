// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The host-facing sub-row scroll input API on [`AtermGpuTerminal`]:
//! fractional/pixel wheel deltas accumulate here, whole rows flip into the
//! engine's `scroll_display`, and the sub-row residual is presented at render
//! time by the grid-band translate (aterm-gpu's `shift_offscreen_band`, the
//! GPU counterpart of `aterm_render::scroll_translate`). Mirrors aterm-wasm's
//! `scroll_input_api`; both stamp the same `RenderInput` fields.
//!
//! ## Contract
//!
//! * Deltas accumulate: two `scroll_px(cell_height / 2)` calls reveal exactly
//!   one older row. The residual is kept in row units, so it survives a font
//!   zoom (`set_px`).
//! * A row flips at ±1.0 accumulated rows (truncation toward zero).
//! * The residual resets on every whole-row navigation (`scroll_lines`,
//!   `scroll_to_bottom`, `scroll_to_top`).
//! * At a history end the residual clamps to zero on the empty side; with no
//!   scrollback (the alternate screen) nothing banks.
//!
//! Positive input reveals older lines, as in
//! [`AtermGpuTerminal::scroll_lines`]. The presented shift is the negated
//! residual: partway toward an older row the band shifts down
//! (`scroll_frac_px < 0`), toward a newer row up.
//!
//! Float accumulation is outside the ty `Expr` language, so the semantics are
//! pinned by the unit tests below rather than a derived spec.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use crate::AtermGpuTerminal;
use aterm_render::RenderInput;

/// The banked sub-row scroll state: a signed fractional-row residual in
/// `(-1.0, 1.0)` relative to the engine's `display_offset` (positive =
/// toward older lines). Pure logic, no engine or clock.
#[derive(Debug, Default)]
pub(crate) struct ScrollInputState {
    frac_rows: f64,
}

impl ScrollInputState {
    /// Accumulate a fractional row delta and return the whole rows that
    /// flipped (truncation toward zero). The remainder stays banked.
    /// Non-finite deltas are ignored. The return saturates to `i32`; the
    /// engine clamps to the history bound.
    pub(crate) fn add_rows(&mut self, delta_rows: f64) -> i32 {
        if !delta_rows.is_finite() {
            return 0;
        }
        let total = self.frac_rows + delta_rows;
        let whole = total.trunc();
        self.frac_rows = total - whole;
        whole.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }

    /// Zero the residual on a side with no content: below the live bottom
    /// (`display_offset == 0`, negative residual) or above the oldest line
    /// (`display_offset >= history_lines`, positive). With no history both
    /// sides clamp.
    pub(crate) fn clamp_at_history_edges(&mut self, display_offset: usize, history_lines: usize) {
        if display_offset == 0 && self.frac_rows < 0.0 {
            self.frac_rows = 0.0;
        }
        if display_offset >= history_lines && self.frac_rows > 0.0 {
            self.frac_rows = 0.0;
        }
    }

    /// Drop the residual (whole-row navigation lands row-aligned).
    pub(crate) fn reset(&mut self) {
        self.frac_rows = 0.0;
    }

    /// The banked residual in rows — signed, in `(-1.0, 1.0)`.
    pub(crate) fn frac_rows(&self) -> f64 {
        self.frac_rows
    }

    /// The signed device-px shift for this residual: the negated residual,
    /// rounded to whole px and clamped inside `(-cell_h, cell_h)`.
    pub(crate) fn frac_px(&self, cell_h: usize) -> i32 {
        if cell_h == 0 {
            return 0;
        }
        let max = i32::try_from(cell_h).unwrap_or(i32::MAX).saturating_sub(1);
        let px = (-self.frac_rows * cell_h as f64).round() as i32;
        px.clamp(-max, max)
    }

    /// Stamp this residual onto a frame snapshot: `scroll_frac_px` plus the
    /// grid band, which is the whole grid `[0, grid_rows)` on the web canvas.
    /// Called every frame so a kept scratch never carries a stale shift.
    // The present paths (`render`/`render_offscreen`) are wasm-only; on the
    // native verification target only the tests drive this.
    pub(crate) fn stamp(&self, input: &mut RenderInput, grid_rows: usize, cell_h: usize) {
        input.scroll_frac_px = self.frac_px(cell_h);
        input.grid_top_row = 0;
        input.grid_bot_row = grid_rows;
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermGpuTerminal {
    /// Sub-row scroll input in device pixels (`deltaY` at
    /// `DOM_DELTA_PIXEL`), signed so positive reveals older lines. Each whole
    /// `cell_height` accumulated flips one row; the remainder is presented by
    /// the next `render()` as a pixel shift of the grid band.
    pub fn scroll_px(&mut self, delta_px: f64) {
        let cell_h = self.cell_height();
        if cell_h == 0 {
            return;
        }
        self.scroll_rows_input(delta_px / cell_h as f64);
    }

    /// Sub-row scroll input in fractional lines (`DOM_DELTA_LINE`, or a host
    /// that scales pixels itself). Same accumulation contract as
    /// [`scroll_px`](Self::scroll_px).
    pub fn scroll_lines_frac(&mut self, delta_rows: f64) {
        self.scroll_rows_input(delta_rows);
    }

    /// The banked residual in rows, in `(-1.0, 1.0)`, positive = toward older
    /// lines. `0` whenever the viewport is row-aligned.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn scroll_frac_rows(&self) -> f64 {
        self.scroll_input.frac_rows()
    }

    /// The signed device-px band shift the next `render()` presents (negative
    /// = shifted down, toward older), so harnesses can compare the CPU and GPU
    /// bundles.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn scroll_frac_px(&self) -> i32 {
        self.scroll_input.frac_px(self.cell_height())
    }
}

// Not wasm-exported: the shared ingress for both input units.
impl AtermGpuTerminal {
    fn scroll_rows_input(&mut self, delta_rows: f64) {
        let whole = self.scroll_input.add_rows(delta_rows);
        if whole != 0 {
            self.term.scroll_display(whole);
        }
        // Clamp against where the engine ACTUALLY landed: input parked at a
        // history end must not bank a phantom sub-row shift.
        let grid = self.term.grid();
        self.scroll_input
            .clamp_at_history_edges(grid.display_offset(), grid.scrollback_lines());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    // ---- pure accumulator semantics (no engine, no font) -------------------

    #[test]
    fn fractional_deltas_accumulate_without_flipping() {
        let mut s = ScrollInputState::default();
        assert_eq!(s.add_rows(0.3), 0);
        assert_eq!(s.add_rows(0.3), 0);
        assert_eq!(s.add_rows(0.3), 0);
        assert!((s.frac_rows() - 0.9).abs() < 1e-9, "residual banks");
    }

    #[test]
    fn row_flips_at_exactly_plus_and_minus_one() {
        // 0.25 is exactly representable, so ±1.0 is reached EXACTLY.
        let mut s = ScrollInputState::default();
        for _ in 0..3 {
            assert_eq!(s.add_rows(0.25), 0);
        }
        assert_eq!(s.add_rows(0.25), 1, "flips at exactly +1.0");
        assert_eq!(s.frac_rows(), 0.0, "the flip consumes the residual");
        for _ in 0..3 {
            assert_eq!(s.add_rows(-0.25), 0);
        }
        assert_eq!(s.add_rows(-0.25), -1, "flips at exactly -1.0");
        assert_eq!(s.frac_rows(), 0.0);
    }

    #[test]
    fn multi_row_deltas_flip_whole_rows_and_bank_the_remainder() {
        let mut s = ScrollInputState::default();
        assert_eq!(s.add_rows(2.75), 2);
        assert_eq!(s.frac_rows(), 0.75);
        // Truncation toward zero: -4.0 from +0.75 lands at -3.25 ⇒ -3 whole.
        assert_eq!(s.add_rows(-4.0), -3);
        assert_eq!(s.frac_rows(), -0.25);
    }

    #[test]
    fn direction_reversal_can_cancel_without_a_flip() {
        let mut s = ScrollInputState::default();
        assert_eq!(s.add_rows(-0.75), 0);
        // Net +0.25: crosses zero, no whole row in either direction.
        assert_eq!(s.add_rows(1.0), 0);
        assert_eq!(s.frac_rows(), 0.25);
    }

    #[test]
    fn reset_drops_the_residual() {
        let mut s = ScrollInputState::default();
        s.add_rows(0.5);
        s.reset();
        assert_eq!(s.frac_rows(), 0.0);
        assert_eq!(s.frac_px(16), 0);
    }

    #[test]
    fn history_edges_clamp_the_empty_side_only() {
        // At the live bottom a NEGATIVE residual has nowhere to go...
        let mut s = ScrollInputState::default();
        s.add_rows(-0.4);
        s.clamp_at_history_edges(0, 10);
        assert_eq!(s.frac_rows(), 0.0);
        // ...but a positive one banks (older content exists above).
        s.add_rows(0.4);
        s.clamp_at_history_edges(0, 10);
        assert_eq!(s.frac_rows(), 0.4);
        // At the top the mirror holds.
        s.clamp_at_history_edges(10, 10);
        assert_eq!(s.frac_rows(), 0.0);
        s.add_rows(-0.4);
        s.clamp_at_history_edges(10, 10);
        assert_eq!(s.frac_rows(), -0.4);
        // No history at all (alt screen): both sides clamp.
        s.clamp_at_history_edges(0, 0);
        assert_eq!(s.frac_rows(), 0.0);
        s.add_rows(0.4);
        s.clamp_at_history_edges(0, 0);
        assert_eq!(s.frac_rows(), 0.0);
    }

    #[test]
    fn frac_px_negates_rounds_and_clamps_into_the_translate_domain() {
        let mut s = ScrollInputState::default();
        s.add_rows(0.5);
        assert_eq!(s.frac_px(16), -8, "toward older ⇒ band shifts DOWN");
        s.reset();
        s.add_rows(-0.5);
        assert_eq!(s.frac_px(16), 8, "toward newer ⇒ band shifts UP");
        s.reset();
        s.add_rows(0.3);
        assert_eq!(s.frac_px(16), -5, "rounds to the nearest device px (4.8)");
        // Near ±1.0 the rounded px must stay INSIDE (-cell_h, cell_h).
        s.reset();
        s.add_rows(0.999_999);
        assert_eq!(s.frac_px(16), -15, "clamped below cell_h");
        // Degenerate cells: no sub-pixel exists at cell_h <= 1.
        assert_eq!(s.frac_px(1), 0);
        assert_eq!(s.frac_px(0), 0);
    }

    #[test]
    fn non_finite_deltas_are_ignored() {
        let mut s = ScrollInputState::default();
        s.add_rows(0.5);
        assert_eq!(s.add_rows(f64::NAN), 0);
        assert_eq!(s.add_rows(f64::INFINITY), 0);
        assert_eq!(s.add_rows(f64::NEG_INFINITY), 0);
        assert_eq!(s.frac_rows(), 0.5, "residual survives hostile input");
    }

    #[test]
    fn huge_deltas_saturate_without_panicking() {
        let mut s = ScrollInputState::default();
        assert_eq!(s.add_rows(1e18), i32::MAX);
        assert_eq!(s.frac_rows(), 0.0);
        assert_eq!(s.add_rows(-1e18), i32::MIN);
    }

    #[test]
    fn stamp_fills_the_full_grid_band_every_frame() {
        let mut s = ScrollInputState::default();
        s.add_rows(0.5);
        let mut input = RenderInput::empty();
        s.stamp(&mut input, 24, 16);
        assert_eq!(input.scroll_frac_px, -8);
        assert_eq!((input.grid_top_row, input.grid_bot_row), (0, 24));
        // A kept scratch must be re-stamped to ZERO after the residual drops.
        s.reset();
        s.stamp(&mut input, 24, 16);
        assert_eq!(input.scroll_frac_px, 0);
    }

    // ---- wasm-export smoke: the exported surface end to end ----------------

    /// Build a terminal with real scrollback, or skip when the environment has
    /// no system font (the same posture as the other binding tests).
    fn terminal_with_history() -> Option<AtermGpuTerminal> {
        let mut t = AtermGpuTerminal::new_from_system(4, 20, 16.0)?;
        for i in 0..12 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        Some(t)
    }

    #[test]
    fn scroll_px_accumulates_and_flips_through_the_export() {
        let Some(mut t) = terminal_with_history() else {
            eprintln!("no system font; skipping export smoke");
            return;
        };
        assert_eq!(t.display_offset(), 0);
        // Half a cell of pixels: banks, no flip. (x/2)/x divides exactly.
        let half = t.cell_height() as f64 / 2.0;
        t.scroll_px(half);
        assert_eq!(t.display_offset(), 0, "half a row banks, no flip");
        assert!((t.scroll_frac_rows() - 0.5).abs() < 1e-9);
        assert!(t.scroll_frac_px() < 0, "toward older presents a DOWN shift");
        t.scroll_px(half);
        assert_eq!(t.display_offset(), 1, "±1.0 accumulated flips one row");
        assert_eq!(t.scroll_frac_rows(), 0.0, "flip consumes the residual");
        assert_eq!(t.scroll_frac_px(), 0);
        // And back down through the line-unit entry.
        t.scroll_lines_frac(-0.5);
        assert!((t.scroll_frac_rows() + 0.5).abs() < 1e-9);
        assert!(t.scroll_frac_px() > 0, "toward newer presents an UP shift");
        t.scroll_lines_frac(-0.5);
        assert_eq!(t.display_offset(), 0);
        assert_eq!(t.scroll_frac_rows(), 0.0);
    }

    #[test]
    fn whole_row_navigation_resets_the_residual() {
        let Some(mut t) = terminal_with_history() else {
            return;
        };
        t.scroll_lines_frac(1.5);
        assert!(t.scroll_frac_rows() > 0.0);
        t.scroll_to_bottom();
        assert_eq!((t.display_offset(), t.scroll_frac_rows()), (0, 0.0));
        t.scroll_lines_frac(0.5);
        t.scroll_lines(1);
        assert_eq!(t.scroll_frac_rows(), 0.0, "whole-row jump lands aligned");
        t.scroll_lines_frac(-0.5);
        t.scroll_to_top();
        assert_eq!(t.scroll_frac_rows(), 0.0);
    }

    #[test]
    fn history_ends_clamp_the_residual_through_the_export() {
        let Some(mut t) = terminal_with_history() else {
            return;
        };
        // At the live bottom, scrolling toward newer banks nothing.
        t.scroll_lines_frac(-0.7);
        assert_eq!((t.display_offset(), t.scroll_frac_rows()), (0, 0.0));
        // Parked at the top, scrolling toward older banks nothing either.
        t.scroll_to_top();
        let top = t.display_offset();
        t.scroll_lines_frac(0.7);
        assert_eq!((t.display_offset(), t.scroll_frac_rows()), (top, 0.0));
        // A fresh terminal (no history at all) never banks.
        let Some(mut fresh) = AtermGpuTerminal::new_from_system(4, 20, 16.0) else {
            return;
        };
        fresh.scroll_lines_frac(0.7);
        assert_eq!(fresh.scroll_frac_rows(), 0.0);
    }

    /// `init` is wasm-only, so this drives `build_frame` (the engine-side half
    /// of `render`/`render_offscreen`) and checks the snapshot carries the
    /// banked residual, and that a kept scratch is re-stamped to zero after a
    /// snap.
    #[test]
    fn render_snapshot_carries_the_banked_residual() {
        let Some(mut t) = terminal_with_history() else {
            eprintln!("no system font; skipping snapshot smoke");
            return;
        };
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
}
