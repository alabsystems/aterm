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
    /// TBC 3 (`CSI 3 g`) cleared all tab stops and no reset has restored the
    /// defaults since. A resize grows the array seeding every-8 defaults in new
    /// columns; this flag suppresses that seeding, so a widen cannot resurrect
    /// cleared stops. The array's length stays the grid width, which the
    /// checkpoint validates.
    pub tab_defaults_suppressed: bool,
    /// Deferred wrap flag (xterm `do_wrap`).
    ///
    /// Writing the last column leaves the cursor there with this flag set; the
    /// next printable performs the wrap. Cursor movement (CR, LF, BS, CUP, ...)
    /// clears it without wrapping.
    pub pending_wrap: bool,
    /// BCE (background colour erase) template cell.
    ///
    /// Erase operations (ED, EL, ECH, IL, DL, scroll) fill cells with this
    /// template instead of `Cell::EMPTY`, so erased areas inherit the current
    /// SGR background. Updated whenever the SGR background changes.
    pub cursor_template: Cell,
    /// BCE background RGB for truecolor backgrounds.
    ///
    /// When `Some([r, g, b])`, erase/fill/scroll operations write this RGB into
    /// `RgbColorRing` for every cell they fill with `cursor_template`, since the
    /// cell's `PackedColors` only records RGB mode. `None` for a default or
    /// indexed background (self-contained in `cursor_template`).
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

impl GridCursorState {
    /// Create default tab stops (every 8 columns).
    pub(crate) fn default_tab_stops(cols: u16) -> Vec<bool> {
        (0..usize::from(cols))
            .map(crate::is_default_tab_stop)
            .collect()
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

    /// Clear any deferred wrap request (skips the store when already false).
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
    #[cfg(test)]
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
    #[cfg(test)]
    pub(crate) fn saved_cursor(&self) -> SavedCursor {
        self.saved_cursor
    }

    /// Restore the cursor position from a previously prepared DECRC target.
    #[inline]
    #[cfg(test)]
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

    /// Set the scroll region (DECSTBM), 0-indexed rows. An out-of-bounds
    /// `bottom` clamps to `visible_rows - 1` (xterm); if top >= bottom the
    /// region resets to full screen.
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

    /// Set horizontal margins (DECSLRM), 0-indexed columns; effective only with
    /// DECLRMM (mode 69). Invalid or out-of-bounds margins reset to full width.
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

    /// Clear every tab stop (TBC 3), including columns a later widen adds (via
    /// `tab_defaults_suppressed`). Like xterm's `TabZonk`, which clears its
    /// whole bitmap and is never re-seeded by `ScreenResize`.
    #[inline]
    pub(crate) fn clear_all_tab_stops(&mut self) {
        self.tab_defaults_suppressed = true;
        self.tab_stops.fill(false);
    }

    #[inline]
    pub(crate) fn reset_tab_stops(&mut self, cols: u16) {
        self.tab_defaults_suppressed = false;
        self.tab_stops = Self::default_tab_stops(cols);
    }
}
