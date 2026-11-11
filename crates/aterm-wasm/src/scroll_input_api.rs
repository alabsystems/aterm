// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Sub-row scroll input on [`WebTerminal`].

use crate::WebTerminal;
use aterm_render::RenderInput;

/// The banked sub-row scroll state.
#[derive(Debug, Default)]
pub(crate) struct ScrollInputState {
    frac_rows: f64,
}

impl ScrollInputState {
    /// Accumulate a row delta and return the whole rows that flipped.
    #[inline]
    pub(crate) fn add_rows(&mut self, delta_rows: f64) -> i32 {
        if !delta_rows.is_finite() {
            return 0;
        }
        let total = self.frac_rows + delta_rows;
        let whole = total.trunc();
        self.frac_rows = total - whole;
        whole.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }

    /// Zero the residual on a side with no content.
    #[inline]
    pub(crate) const fn clamp_at_history_edges(
        &mut self,
        display_offset: usize,
        history_lines: usize,
    ) {
        if display_offset == 0 && self.frac_rows < 0.0 {
            self.frac_rows = 0.0;
        }
        if display_offset >= history_lines && self.frac_rows > 0.0 {
            self.frac_rows = 0.0;
        }
    }

    /// Drop the residual (whole-row navigation lands row-aligned).
    #[inline]
    pub(crate) const fn reset(&mut self) {
        self.frac_rows = 0.0;
    }

    /// The banked residual in rows, in `(-1.0, 1.0)`.
    #[inline]
    pub(crate) const fn frac_rows(&self) -> f64 {
        self.frac_rows
    }

    /// The device-px shift presenting this residual.
    #[inline]
    pub(crate) fn frac_px(&self, cell_h: usize) -> i32 {
        if cell_h == 0 {
            return 0;
        }
        let max = i32::try_from(cell_h).unwrap_or(i32::MAX).saturating_sub(1);
        let px = (-self.frac_rows * cell_h as f64).round() as i32;
        px.clamp(-max, max)
    }

    /// Stamp the presented `scroll_frac_px` and a whole-frame grid band `[0, grid_rows)` onto the
    /// frame.
    #[inline]
    pub(crate) fn stamp(&self, input: &mut RenderInput, grid_rows: usize, cell_h: usize) {
        input.scroll_frac_px = self.frac_px(cell_h);
        input.grid_top_row = 0;
        input.grid_bot_row = grid_rows;
    }
}

/// The JS-visible surface.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Sub-row scroll input in device pixels, positive revealing older lines as in
            /// [`scroll_lines`](Self::scroll_lines).
            pub fn scroll_px(&mut self, delta_px: f64) {
                let cell_h = self.cell_height();
                if cell_h == 0 {
                    return;
                }
                self.scroll_rows_input(delta_px / cell_h as f64);
            }

            /// Sub-row scroll input in fractional lines ([`scroll_px`](Self::scroll_px)).
            pub fn scroll_lines_frac(&mut self, delta_rows: f64) {
                self.scroll_rows_input(delta_rows);
            }

            /// The banked sub-row residual in rows.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn scroll_frac_rows(&self) -> f64 {
                self.scroll_input.frac_rows()
            }

            /// The signed device-px band shift the next `render()` presents.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn scroll_frac_px(&self) -> i32 {
                self.scroll_input.frac_px(self.cell_height())
            }
        }
    };
}
web_terminal_impl!(bind);

impl WebTerminal {
    #[inline]
    fn scroll_rows_input(&mut self, delta_rows: f64) {
        let whole = self.scroll_input.add_rows(delta_rows);
        if whole != 0 {
            self.term.scroll_display(whole);
        }
        let grid = self.term.grid();
        self.scroll_input
            .clamp_at_history_edges(grid.display_offset(), grid.scrollback_lines());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "scroll_input_api_own_tests.rs"]
mod tests;
