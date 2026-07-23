// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cursor- and region-oriented grid state.

use std::ops::{Deref, DerefMut};

use crate::cell::Cell;
use crate::cursor::{Cursor, SavedCursor};
use crate::scroll_region::{HorizontalMargins, ScrollRegion};

use super::GridPresentationState;

#[doc(hidden)]
#[derive(Debug)]
pub struct GridCursorState {
    /// Cursor position (within visible area).
    pub cursor: Cursor,
    /// Saved cursor (DECSC/DECRC).
    pub saved_cursor: SavedCursor,
    /// Scroll region (DECSTBM).
    pub scroll_region: ScrollRegion,
    /// Horizontal margins (DECSLRM, VT420+).
    /// Only active when DECLRMM (mode 69) is enabled.
    pub horizontal_margins: HorizontalMargins,
    /// Tab stops (true = tab stop at this column).
    /// Default tab stops are every 8 columns.
    pub tab_stops: Vec<bool>,
    /// Deferred wrap flag (xterm `wrapnext` / `do_wrap`).
    ///
    /// When a printable character is written to the last column, the cursor
    /// stays at the last column with this flag set. The next printable
    /// character triggers the actual wrap (advance to next line). Any cursor
    /// movement command (CR, LF, BS, CUP, etc.) clears this flag without
    /// wrapping, allowing the cursor to be repositioned from the last column.
    ///
    /// This matches xterm/VT220 behavior and is critical for applications
    /// that write exactly `cols` characters per line followed by newlines.
    pub pending_wrap: bool,
    /// BCE (Background Color Erase) template cell.
    ///
    /// Erase operations (ED, EL, ECH, IL, DL, scroll) fill cells with this
    /// template instead of `Cell::EMPTY`. The template carries the current
    /// SGR background color so that erased areas inherit the active background.
    /// Updated by the terminal handler whenever the SGR background changes.
    ///
    /// Per VT420/xterm BCE specification (#7522).
    pub cursor_template: Cell,
    /// BCE background RGB value for truecolor backgrounds.
    ///
    /// When `Some([r, g, b])`, erase/fill/scroll operations must write this
    /// RGB value into `RgbColorRing` for every cell they fill with
    /// `cursor_template`. Without this, the cell's `PackedColors` says "RGB
    /// mode" but the actual (R, G, B) values are lost because they live in
    /// the ring buffer, not inline in the 8-byte cell.
    ///
    /// `None` when the current SGR background is default or indexed (the
    /// 4-byte `PackedColors` in `cursor_template` is self-contained).
    ///
    /// Fixes #7685.
    pub cursor_template_bg_rgb: Option<[u8; 3]>,
    /// Presentation-oriented state layered under cursor/region state.
    pub presentation: GridPresentationState,
}

impl Deref for GridCursorState {
    type Target = GridPresentationState;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.presentation
    }
}

impl DerefMut for GridCursorState {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.presentation
    }
}

/// Loop-free tab_stops for Kani. Explicit branches instead of `.to_vec()` which
/// uses `ptr::copy_nonoverlapping` that CBMC struggles to bound.
#[cfg(kani)]
pub(crate) fn kani_tab_stops(cols: u16) -> Vec<bool> {
    let n = cols.clamp(1, crate::KANI_MAX_COLS) as usize;
    // Tab stops: only column 8 is true for cols <= 16
    let tab: [bool; 16] = [
        false, false, false, false, false, false, false, false, true, false, false, false, false,
        false, false, false,
    ];
    let mut v = Vec::with_capacity(16);
    if n >= 1 {
        v.push(tab[0]);
    }
    if n >= 2 {
        v.push(tab[1]);
    }
    if n >= 3 {
        v.push(tab[2]);
    }
    if n >= 4 {
        v.push(tab[3]);
    }
    if n >= 5 {
        v.push(tab[4]);
    }
    if n >= 6 {
        v.push(tab[5]);
    }
    if n >= 7 {
        v.push(tab[6]);
    }
    if n >= 8 {
        v.push(tab[7]);
    }
    if n >= 9 {
        v.push(tab[8]);
    }
    if n >= 10 {
        v.push(tab[9]);
    }
    if n >= 11 {
        v.push(tab[10]);
    }
    if n >= 12 {
        v.push(tab[11]);
    }
    if n >= 13 {
        v.push(tab[12]);
    }
    if n >= 14 {
        v.push(tab[13]);
    }
    if n >= 15 {
        v.push(tab[14]);
    }
    if n >= 16 {
        v.push(tab[15]);
    }
    v
}

impl GridCursorState {
    /// Create default tab stops (every 8 columns).
    pub(crate) fn default_tab_stops(cols: u16) -> Vec<bool> {
        #[cfg(kani)]
        {
            return kani_tab_stops(cols);
        }
        #[cfg(not(kani))]
        {
            (0..cols).map(|c| c > 0 && c % 8 == 0).collect()
        }
    }

    #[cfg(kani)]
    pub(crate) fn kani_stub(rows: u16, cols: u16) -> Self {
        Self {
            cursor: Cursor::default(),
            saved_cursor: SavedCursor::default(),
            scroll_region: ScrollRegion::full(rows),
            horizontal_margins: HorizontalMargins::full(cols),
            tab_stops: Self::default_tab_stops(cols),
            pending_wrap: false,
            cursor_template: Cell::EMPTY,
            cursor_template_bg_rgb: None,
            presentation: GridPresentationState::kani_stub(),
        }
    }

    /// Get the current cursor position.
    #[must_use]
    #[inline]
    pub(crate) fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Update the cursor position without applying additional policy.
    #[inline]
    pub(crate) fn set_cursor_position(&mut self, row: u16, col: u16) {
        self.cursor = Cursor::new(row, col);
    }

    /// Check whether deferred wrap is pending.
    #[must_use]
    #[inline]
    pub(crate) fn pending_wrap(&self) -> bool {
        self.pending_wrap
    }

    /// Clear any deferred wrap request.
    ///
    /// Uses a conditional store to avoid writing when already false.
    /// Eliminates unnecessary store-buffer traffic in the common case
    /// (pending_wrap is only set at the last column).
    #[inline]
    pub(crate) fn clear_pending_wrap(&mut self) {
        if self.pending_wrap {
            self.pending_wrap = false;
        }
    }

    /// Mark the cursor as having a deferred wrap request.
    #[inline]
    pub(crate) fn mark_pending_wrap(&mut self) {
        self.pending_wrap = true;
    }

    /// Consume the deferred wrap request, returning whether one was pending.
    ///
    /// Uses a conditional store: avoids the write when pending_wrap is
    /// already false (the common case — wrap is only set at last column).
    #[must_use]
    #[inline]
    pub(crate) fn take_pending_wrap(&mut self) -> bool {
        if self.pending_wrap {
            self.pending_wrap = false;
            true
        } else {
            false
        }
    }

    /// Save cursor state for DECSC/DECRC.
    #[inline]
    pub(crate) fn save_cursor(&mut self) {
        self.saved_cursor = SavedCursor {
            cursor: self.cursor,
            valid: true,
            pending_wrap: self.pending_wrap,
        };
    }

    /// Return the currently saved cursor snapshot.
    #[must_use]
    #[inline]
    pub(crate) fn saved_cursor(&self) -> SavedCursor {
        self.saved_cursor
    }

    /// Restore the cursor position from a previously prepared DECRC target.
    #[inline]
    pub(crate) fn restore_saved_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
        self.pending_wrap = self.saved_cursor.pending_wrap;
    }

    /// Get the current scroll region.
    #[must_use]
    #[inline]
    pub(crate) fn scroll_region(&self) -> ScrollRegion {
        self.scroll_region
    }

    /// Set the scroll region (DECSTBM).
    ///
    /// `top` and `bottom` are 0-indexed row numbers.
    /// Out-of-bounds `bottom` is clamped to `visible_rows - 1` (xterm behavior).
    /// If top >= clamped bottom, the region is reset to full screen.
    #[inline]
    pub(crate) fn set_scroll_region(&mut self, top: u16, bottom: u16, visible_rows: u16) {
        // Clamp bottom to the last visible row (xterm compatibility).
        let bottom = bottom.min(visible_rows.saturating_sub(1));
        if top < bottom {
            self.scroll_region = ScrollRegion { top, bottom };
        } else {
            self.scroll_region = ScrollRegion::full(visible_rows);
        }
    }

    /// Reset scroll region to full screen.
    #[inline]
    pub(crate) fn reset_scroll_region(&mut self, visible_rows: u16) {
        self.scroll_region = ScrollRegion::full(visible_rows);
    }

    /// Get the current horizontal margins (DECSLRM).
    #[must_use]
    #[inline]
    pub(crate) fn horizontal_margins(&self) -> HorizontalMargins {
        self.horizontal_margins
    }

    /// Set horizontal margins (DECSLRM, VT420+).
    ///
    /// `left` and `right` are 0-indexed column numbers.
    /// Only has effect when DECLRMM (mode 69) is enabled.
    /// If left >= right or either is out of bounds, the margins are reset to full width.
    #[inline]
    pub(crate) fn set_horizontal_margins(&mut self, left: u16, right: u16, cols: u16) {
        if left < right && right < cols {
            self.horizontal_margins = HorizontalMargins { left, right };
        } else {
            self.horizontal_margins = HorizontalMargins::full(cols);
        }
    }

    /// Reset horizontal margins to full width.
    #[inline]
    pub(crate) fn reset_horizontal_margins(&mut self, cols: u16) {
        self.horizontal_margins = HorizontalMargins::full(cols);
    }

    #[inline]
    pub(crate) fn set_tab_stop_at(&mut self, col: u16) {
        let col = usize::from(col);
        if col < self.tab_stops.len() {
            self.tab_stops[col] = true;
        }
    }

    #[inline]
    pub(crate) fn clear_tab_stop_at(&mut self, col: u16) {
        let col = usize::from(col);
        if col < self.tab_stops.len() {
            self.tab_stops[col] = false;
        }
    }

    #[inline]
    pub(crate) fn clear_all_tab_stops(&mut self) {
        self.tab_stops.fill(false);
    }

    #[inline]
    pub(crate) fn reset_tab_stops(&mut self, cols: u16) {
        self.tab_stops = Self::default_tab_stops(cols);
    }

    #[cfg(any(test, kani, feature = "testing"))]
    #[inline]
    #[must_use]
    pub(crate) fn is_tab_stop(&self, col: u16) -> bool {
        self.tab_stops
            .get(usize::from(col))
            .copied()
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn tab_stop_positions(&self) -> impl Iterator<Item = u16> + '_ {
        self.tab_stops
            .iter()
            .enumerate()
            .filter(|&(_, &is_stop)| is_stop)
            .map(|(col, _)| u16::try_from(col).unwrap_or(u16::MAX))
    }
}
