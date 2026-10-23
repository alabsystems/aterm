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
    /// Horizontal margins (DECSLRM), active only while DECLRMM (mode 69) is set.
    pub horizontal_margins: HorizontalMargins,
    /// Tab stops (`true` = stop at this column), every 8 columns by default.
    pub tab_stops: Vec<bool>,
    /// Whether `TBC 3` (`CSI 3 g`) suppressed default tab stops on widen.
    pub tab_defaults_suppressed: bool,
    /// Deferred wrap flag (xterm `do_wrap`).
    pub pending_wrap: bool,
    /// BCE (background color erase) template cell.
    pub cursor_template: Cell,
    /// Truecolor BCE background RGB (`None` for default or indexed background).
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
    pub(crate) const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Update the cursor position without applying additional policy.
    #[inline]
    pub(crate) const fn set_cursor_position(&mut self, row: u16, col: u16) {
        self.cursor = Cursor::new(row, col);
    }

    /// Check whether deferred wrap is pending.
    #[must_use]
    #[inline]
    pub(crate) const fn pending_wrap(&self) -> bool {
        self.pending_wrap
    }

    /// Clear any deferred wrap request (skips the store when already false).
    #[inline]
    pub(crate) const fn clear_pending_wrap(&mut self) {
        if self.pending_wrap {
            self.pending_wrap = false;
        }
    }

    /// Mark the cursor as having a deferred wrap request.
    #[inline]
    pub(crate) const fn mark_pending_wrap(&mut self) {
        self.pending_wrap = true;
    }

    /// Consume the deferred wrap request, returning whether one was pending.
    #[must_use]
    #[inline]
    pub(crate) const fn take_pending_wrap(&mut self) -> bool {
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
    pub(crate) const fn save_cursor(&mut self) {
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
    pub(crate) const fn saved_cursor(&self) -> SavedCursor {
        self.saved_cursor
    }

    /// Restore the cursor position from a previously prepared DECRC target.
    #[inline]
    #[cfg(test)]
    pub(crate) const fn restore_saved_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
        self.pending_wrap = self.saved_cursor.pending_wrap;
    }

    /// Get the current scroll region.
    #[must_use]
    #[inline]
    pub(crate) const fn scroll_region(&self) -> ScrollRegion {
        self.scroll_region
    }

    /// Sets the scroll region (DECSTBM), clamping `bottom` and resetting on `top >= bottom`.
    #[inline]
    pub(crate) const fn set_scroll_region(&mut self, top: u16, bottom: u16, visible_rows: u16) {
        let max_row = visible_rows.saturating_sub(1);
        let bottom = if bottom < max_row { bottom } else { max_row };
        if top < bottom {
            self.scroll_region = ScrollRegion { top, bottom };
        } else {
            self.scroll_region = ScrollRegion::full(visible_rows);
        }
    }

    /// Reset scroll region to full screen.
    #[inline]
    pub(crate) const fn reset_scroll_region(&mut self, visible_rows: u16) {
        self.scroll_region = ScrollRegion::full(visible_rows);
    }

    /// Get the current horizontal margins (DECSLRM).
    #[must_use]
    #[inline]
    pub(crate) const fn horizontal_margins(&self) -> HorizontalMargins {
        self.horizontal_margins
    }

    /// Sets horizontal margins (DECSLRM), resetting to full width on invalid bounds.
    #[inline]
    pub(crate) const fn set_horizontal_margins(&mut self, left: u16, right: u16, cols: u16) {
        if left < right && right < cols {
            self.horizontal_margins = HorizontalMargins { left, right };
        } else {
            self.horizontal_margins = HorizontalMargins::full(cols);
        }
    }

    /// Reset horizontal margins to full width.
    #[inline]
    pub(crate) const fn reset_horizontal_margins(&mut self, cols: u16) {
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

    /// Clears every tab stop (`TBC 3`) and suppresses defaults on widen.
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
