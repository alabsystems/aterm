// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The host-facing SUB-ROW scroll input API on [`AtermTerminal`]: fractional /
//! pixel wheel deltas accumulate here, whole rows flip into the engine's
//! `scroll_display`, and the banked sub-row residual is presented at render
//! time through the existing M1b grid-band translate
//! (`aterm_render::scroll_translate`) — pixel-true trackpad scrolling for web
//! hosts instead of `scroll_lines`' whole-row jumps.
//!
//! ## Contract
//!
//! * Deltas ACCUMULATE: two `scroll_px(cell_height / 2)` calls reveal exactly
//!   one older row. The residual banks in ROW units, so it survives a font
//!   zoom (`set_px`) unscaled.
//! * A row FLIPS at ±1.0 accumulated rows (truncation toward zero — the
//!   engine's `display_offset` only ever moves by whole rows).
//! * The residual RESETS on every whole-row navigation (`scroll_lines`,
//!   `scroll_to_bottom`, `scroll_to_top`): row-aligned jumps land row-aligned.
//! * At a history end the residual CLAMPS to zero on the empty side (no
//!   sub-row position exists past the live bottom or the oldest retained
//!   line); with no scrollback at all (the alternate screen) nothing banks.
//!
//! Sign convention matches [`AtermTerminal::scroll_lines`]: POSITIVE input
//! reveals OLDER lines. The presented shift is the NEGATED residual — partway
//! toward an older row the band shifts DOWN (`scroll_frac_px < 0`, the
//! incoming older row's strip exposed at the top), partway toward a newer row
//! it shifts UP — continuous with the whole-row flip on both sides.
//!
//! Float accumulation is outside the ty `Expr` language (the same documented
//! waiver as `aterm-gui`'s scroll kinematics), so the semantics are proven by
//! the unit lattice below, not a derived spec.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use crate::AtermTerminal;
use aterm_render::RenderInput;

/// The banked sub-row scroll state: a signed fractional-row residual in
/// `(-1.0, 1.0)`, relative to the engine's whole-row `display_offset`
/// (positive = partway toward OLDER lines). Pure accumulation/rounding logic —
/// no engine, no clock — so the semantics are unit-tested exhaustively below.
#[derive(Debug, Default)]
pub(crate) struct ScrollInputState {
    frac_rows: f64,
}

impl ScrollInputState {
    /// Accumulate a fractional row delta and return the WHOLE rows that
    /// flipped (truncation toward zero: a flip happens exactly at ±1.0
    /// accumulated). The sub-row remainder stays banked. Non-finite deltas (a
    /// hostile JS `NaN`/`Infinity`) are ignored. The return saturates to
    /// `i32` — the engine clamps to the real history bound anyway.
    pub(crate) fn add_rows(&mut self, delta_rows: f64) -> i32 {
        if !delta_rows.is_finite() {
            return 0;
        }
        let total = self.frac_rows + delta_rows;
        let whole = total.trunc();
        self.frac_rows = total - whole;
        whole.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }

    /// Zero the residual on a side with no content: nothing sub-row exists
    /// below the live bottom (`display_offset == 0`, negative residual) or
    /// above the oldest retained line (`display_offset >= history_lines`,
    /// positive residual). With no history at all BOTH sides clamp, so the
    /// alternate screen can never bank a shift.
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

    /// The SIGNED device-px shift presenting this residual through the M1b
    /// grid-band translate: the NEGATED residual (partway toward older ⇒ band
    /// shifts DOWN ⇒ negative), rounded to whole px and clamped inside the
    /// translate's domain `(-cell_h, cell_h)`.
    pub(crate) fn frac_px(&self, cell_h: usize) -> i32 {
        if cell_h == 0 {
            return 0;
        }
        let max = i32::try_from(cell_h).unwrap_or(i32::MAX).saturating_sub(1);
        let px = (-self.frac_rows * cell_h as f64).round() as i32;
        px.clamp(-max, max)
    }

    /// Stamp this residual onto a frame snapshot: the presented
    /// `scroll_frac_px` plus the grid band. The web canvas has NO spliced
    /// chrome rows (no tab strip / HUD in the framebuffer), so the band is
    /// the whole grid `[0, grid_rows)`. Called EVERY frame — a kept scratch
    /// would otherwise carry a stale shift after the residual resets.
    pub(crate) fn stamp(&self, input: &mut RenderInput, grid_rows: usize, cell_h: usize) {
        input.scroll_frac_px = self.frac_px(cell_h);
        input.grid_top_row = 0;
        input.grid_bot_row = grid_rows;
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermTerminal {
    /// Sub-row scroll input in device PIXELS — the wheel/trackpad `deltaY` at
    /// `deltaMode == DOM_DELTA_PIXEL`, sign-adjusted by the host so POSITIVE
    /// reveals older lines (the [`scroll_lines`](Self::scroll_lines)
    /// convention). Fractions accumulate across calls; each whole
    /// `cell_height` of accumulation flips one engine row, and the sub-row
    /// remainder is presented by the next `render()` as a pixel shift of the
    /// grid band — the host only needs to redraw afterwards.
    pub fn scroll_px(&mut self, delta_px: f64) {
        let cell_h = self.cell_height();
        if cell_h == 0 {
            return;
        }
        self.scroll_rows_input(delta_px / cell_h as f64);
    }

    /// Sub-row scroll input in fractional LINES (`deltaMode ==
    /// DOM_DELTA_LINE` hosts, or a host that scales pixels itself). Same
    /// accumulation contract as [`scroll_px`](Self::scroll_px): whole rows
    /// flip at ±1.0 accumulated, the remainder banks.
    pub fn scroll_lines_frac(&mut self, delta_rows: f64) {
        self.scroll_rows_input(delta_rows);
    }

    /// The banked sub-row residual in ROWS — signed, in `(-1.0, 1.0)`,
    /// positive = partway toward OLDER lines. `0` whenever the viewport is
    /// row-aligned (after a flip, a whole-row navigation, or at a clamped
    /// history end).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn scroll_frac_rows(&self) -> f64 {
        self.scroll_input.frac_rows()
    }

    /// The SIGNED device-px band shift the next `render()` presents for the
    /// banked residual (negative = band shifted DOWN, toward older). Exposed
    /// so hosts/harnesses can assert the CPU and GPU bundles present the same
    /// sub-row frame.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn scroll_frac_px(&self) -> i32 {
        self.scroll_input.frac_px(self.cell_height())
    }
}

// Not wasm-exported: the shared ingress for both input units.
impl AtermTerminal {
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
    fn terminal_with_history() -> Option<AtermTerminal> {
        let mut t = AtermTerminal::new_from_system(4, 20, 16.0)?;
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
        let Some(mut fresh) = AtermTerminal::new_from_system(4, 20, 16.0) else {
            return;
        };
        fresh.scroll_lines_frac(0.7);
        assert_eq!(fresh.scroll_frac_rows(), 0.0);
    }

    #[test]
    fn render_presents_the_banked_residual_and_is_identity_at_zero() {
        let Some(mut t) = terminal_with_history() else {
            eprintln!("no system font; skipping render smoke");
            return;
        };
        t.render();
        let base = t.rgba();
        // Bank half a row: same engine offset, but the presented frame shifts.
        t.scroll_px(t.cell_height() as f64 / 2.0);
        assert!(t.scroll_frac_px() != 0, "non-vacuity: a shift is presented");
        t.render();
        assert_ne!(t.rgba(), base, "the sub-row residual moves the pixels");
        // Snap resets: frac 0 must be BYTE-IDENTICAL to the whole-row frame
        // (the translate's identity-at-zero, through the whole export path).
        t.scroll_to_bottom();
        t.render();
        assert_eq!(t.rgba(), base, "identity at frac 0 after the snap");
    }
}
