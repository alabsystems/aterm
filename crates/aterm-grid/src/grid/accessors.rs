// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Getter methods for [`Grid`] dimensions, cursor, subsystem accessors,
//! and damage control.
//!
//! O(1) forwarding to internal state — content queries that iterate over
//! grid data live in [`super::content_queries`].

use aterm_types::index::Dimensions;

use super::Grid;
use crate::Damage;
use crate::extra_collection::CellRenderData;
use crate::{CellExtra, CellExtras};
use crate::{ExtendedStyle, Style, StyleId, StyleTable};

/// Bridge-compatible grid dimensions (#3828).
///
/// Implements `aterm_types::index::Dimensions` so the Alacritty bridge (and
/// any future consumer) can use `Dimensions` methods on `Grid` without the
/// bridge needing to define a foreign impl.
impl Dimensions for Grid {
    fn total_lines(&self) -> usize {
        self.storage.total_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.storage.visible_rows())
    }

    fn columns(&self) -> usize {
        usize::from(self.storage.cols())
    }
}

impl Grid {
    // -------------------------------------------------------------------------
    // Dimension getters
    // -------------------------------------------------------------------------

    /// Get the number of visible rows.
    #[must_use]
    #[inline]
    pub fn rows(&self) -> u16 {
        self.storage.visible_rows()
    }

    /// Get the number of columns.
    #[must_use]
    #[inline]
    pub fn cols(&self) -> u16 {
        self.storage.cols()
    }

    /// Get effective column count for the current cursor row.
    ///
    /// Returns `cols` for normal lines, `cols/2` for double-width (DECDWL) lines.
    /// Callers can cache this to avoid redundant ring-buffer lookups when
    /// multiple operations target the same row.
    #[must_use]
    #[inline]
    pub fn effective_cols_for_current_row(&self) -> u16 {
        self.storage.effective_cols_for_row(self.storage.cursor.row)
    }

    /// Get effective column count for an arbitrary row.
    ///
    /// Returns `cols` for normal lines, `cols/2` for double-width (DECDWL) lines.
    /// Used when the caller needs the effective width of a row other than the
    /// current cursor row (e.g., combining character wrap-back to the previous
    /// row).
    #[must_use]
    #[inline]
    pub fn effective_cols_for_row(&self, row: u16) -> u16 {
        self.storage.effective_cols_for_row(row)
    }

    /// Get total lines in buffer (visible + scrollback).
    #[must_use]
    #[inline]
    pub fn total_lines(&self) -> usize {
        self.storage.total_lines()
    }

    /// Get the display offset (scroll position).
    #[must_use]
    #[inline]
    pub fn display_offset(&self) -> usize {
        self.storage.display_offset()
    }

    /// Take the accumulated content scroll delta, resetting it to 0.
    #[inline]
    pub fn take_content_scroll_delta(&mut self) -> i32 {
        self.storage.take_content_scroll_delta()
    }

    /// Take the pending change to the monotonic absolute-row coordinate space.
    ///
    /// Terminal metadata consumers must apply splices in order. `Invalidate`
    /// means multiple non-composable changes accumulated before this call.
    #[inline]
    pub fn take_absolute_row_update(&mut self) -> Option<crate::AbsoluteRowUpdate> {
        self.storage.presentation.take_absolute_row_update()
    }

    /// Take the logical-row insertion retained for text-selection remapping.
    ///
    /// This is independent of [`Self::take_absolute_row_update`], because the
    /// parser may drain metadata updates before terminal post-processing.
    #[inline]
    pub fn take_selection_row_update(&mut self) -> Option<crate::AbsoluteRowUpdate> {
        self.storage.presentation.take_selection_row_update()
    }

    /// Force selection invalidation by setting `content_scroll_delta` to `i32::MAX`.
    ///
    /// Used when the entire grid content changes non-incrementally (e.g.,
    /// alternate screen buffer switch) and any active selection is stale.
    #[inline]
    pub fn force_selection_invalidation(&mut self) {
        self.storage.content_scroll_delta = i32::MAX;
    }

    // -------------------------------------------------------------------------
    // Cursor getters
    // -------------------------------------------------------------------------

    /// Get the cursor position.
    #[must_use]
    #[inline]
    pub fn cursor(&self) -> super::Cursor {
        self.storage.cursor()
    }

    /// Get cursor row.
    #[must_use]
    #[inline]
    pub fn cursor_row(&self) -> u16 {
        self.storage.cursor().row
    }

    /// Get cursor column.
    #[must_use]
    #[inline]
    pub fn cursor_col(&self) -> u16 {
        self.storage.cursor().col
    }

    /// Soft-wrap flag for a visible row: `true` if this row is a wrap
    /// continuation of the previous row. `None` for an out-of-range row.
    ///
    /// Resolved through [`visible_row_view`](Self::visible_row_view) so a
    /// scrolled-back row past the ring base (materialized from lazy/tiered
    /// scrollback) reports its real wrap state instead of `None` — matching the
    /// tier-aware `row_text`/`cell_text` siblings. At `display_offset == 0` this
    /// is byte-identical to reading `Grid::row` directly.
    #[must_use]
    #[inline]
    pub fn row_is_wrapped(&self, visible_row: u16) -> Option<bool> {
        match self.visible_row_view(visible_row) {
            super::VisibleRowView::Empty => None,
            view => Some(view.is_wrapped()),
        }
    }

    /// Logical length of a visible row (last non-empty cell + 1, 0 if blank).
    /// `None` for an out-of-range row.
    ///
    /// Resolved through [`visible_row_view`](Self::visible_row_view) so a
    /// scrolled-back history row reports its materialized length instead of
    /// `None`, matching the tier-aware `row_text`/`cell_text` siblings. At
    /// `display_offset == 0` this is byte-identical to reading `Grid::row`.
    #[must_use]
    #[inline]
    pub fn row_len(&self, visible_row: u16) -> Option<u16> {
        match self.visible_row_view(visible_row) {
            super::VisibleRowView::Empty => None,
            view => Some(view.len()),
        }
    }

    /// Check if the cursor has a deferred wrap pending.
    #[must_use]
    #[inline]
    pub fn pending_wrap(&self) -> bool {
        self.storage.pending_wrap()
    }

    /// Set the pending wrap flag directly (#7283).
    ///
    /// Used by DECRC (cursor restore) to restore the saved wrap-next state.
    /// Normal cursor operations clear pending_wrap automatically.
    #[inline]
    pub fn set_pending_wrap(&mut self, wrap: bool) {
        if wrap {
            self.storage.mark_pending_wrap();
        } else {
            self.storage.clear_pending_wrap();
        }
    }

    /// Resolve pending wrap: if a deferred wrap is active, perform the actual
    /// line advance now. Call this before writing characters.
    #[inline]
    pub fn resolve_pending_wrap(&mut self) {
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }
    }

    // -------------------------------------------------------------------------
    // Subsystem accessors
    // -------------------------------------------------------------------------

    /// Get damage state.
    #[must_use]
    #[inline]
    pub fn damage(&self) -> &Damage {
        self.storage.damage()
    }

    /// Get mutable damage state.
    #[inline]
    pub fn damage_mut(&mut self) -> &mut Damage {
        self.storage.damage_mut()
    }

    /// Monotonic content-generation counter (P1.0).
    ///
    /// Advances by one on every CONTENT mutation (cell/line/scrollback change)
    /// but NOT on a pure viewport scroll (`scroll_display`). A cached search
    /// index or a peer session caches this value and does an O(1) compare to
    /// detect change. Starts at a NONZERO value, so `0` is a usable "never
    /// observed" sentinel.
    #[must_use]
    #[inline]
    pub fn content_gen(&self) -> u64 {
        self.storage.content_gen
    }

    /// Get cell extras storage.
    #[must_use]
    #[inline]
    pub fn extras(&self) -> &CellExtras {
        self.storage.extras()
    }

    /// Get mutable cell extras storage.
    #[inline]
    pub fn extras_mut(&mut self) -> &mut CellExtras {
        self.storage.extras_mut()
    }

    /// Get the style table.
    #[must_use]
    #[inline]
    pub fn styles(&self) -> &StyleTable {
        self.storage.styles()
    }

    /// Get mutable access to the style table.
    #[inline]
    pub fn styles_mut(&mut self) -> &mut StyleTable {
        self.storage.styles_mut()
    }

    /// L1 cache probe: check if the given style matches the last interned style.
    ///
    /// Returns `Some(StyleId)` on cache hit (refcount incremented), `None` on miss.
    /// Callers should fall back to `intern_extended_style` on miss.
    #[inline]
    pub fn try_intern_style_l1(&mut self, style: &Style) -> Option<StyleId> {
        self.storage.styles_mut().try_intern_l1(style)
    }

    /// L2 indexed-color cache probe without constructing ExtendedStyle.
    #[inline]
    pub fn try_intern_style_l2_indexed(&mut self, style: &Style, fg_index: u8) -> Option<StyleId> {
        self.storage
            .styles_mut()
            .try_intern_l2_indexed(style, fg_index)
    }

    /// Intern an extended style with color type information.
    ///
    /// This preserves the original color type (default/indexed/rgb) for
    /// later conversion back to `PackedColors` format.
    #[inline]
    pub fn intern_extended_style(&mut self, ext_style: ExtendedStyle) -> StyleId {
        self.storage.styles_mut().intern_extended(ext_style)
    }

    /// Mark that the grid has at least one double-width row (DECDWL/DECDHL).
    ///
    /// This enables the slow path in `effective_cols_for_row` that does a
    /// ring-buffer lookup to check each row's line_size. When false (the
    /// common case), the lookup is skipped entirely.
    #[inline]
    pub fn mark_has_double_width(&mut self) {
        self.storage.any_double_width = true;
    }

    /// Returns true if the grid has (or recently had) any double-width rows.
    ///
    /// This is the optimization flag checked by cursor operations to decide
    /// whether the expensive per-row `line_size` lookup is needed.
    #[must_use]
    #[inline]
    pub fn has_any_double_width(&self) -> bool {
        self.storage.any_double_width
    }

    /// Get extras for a specific cell.
    #[must_use]
    #[inline]
    pub fn cell_extra(&self, row: u16, col: u16) -> Option<&CellExtra> {
        self.storage.cell_extra(row, col)
    }

    /// Unified render/FFI lookup for a single cell's overflow data.
    ///
    /// Collapses ring-buffer and HashMap access into one pass keyed by the
    /// cell's flags, avoiding repeated probes for complex chars, combining
    /// marks, and RGB overflow on hot paths.
    #[must_use]
    #[inline]
    pub fn cell_render_data(&self, row: u16, col: u16, cell: crate::Cell) -> CellRenderData<'_> {
        self.storage.extras().render_data_for_cell(row, col, cell)
    }

    /// Get or create extras for a specific cell.
    ///
    /// Sets the HAS_EXTRAS flag on the cell so the rendering path can skip
    /// hash probes for cells without extras.
    #[inline]
    pub fn cell_extra_mut(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.storage.cell_extra_mut(row, col)
    }

    /// Get or create extras for a cell whose HAS_EXTRAS flag is already set.
    ///
    /// Skips the ring-buffer lookup that `cell_extra_mut` does to set the flag.
    /// The caller MUST have pre-set the HAS_EXTRAS bit in the cell's PackedColors.
    #[inline]
    pub fn cell_extra_mut_preflagged(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.storage.cell_extra_mut_preflagged(row, col)
    }

    /// Store a complex char codepoint in the dense ring buffer (O(1) flat-array write).
    ///
    /// Use this for the non-BMP write hot path instead of `cell_extra_mut_preflagged`
    /// + `set_complex_char`. The ring buffer avoids FxHashMap overhead entirely.
    ///   Stores raw `char` — no Arc allocation or atomic refcounting.
    #[inline]
    pub fn set_complex_char_ring(&mut self, row: u16, col: u16, value: char) {
        let visible_rows = self.storage.visible_rows;
        let cols = self.storage.cols;
        self.storage
            .extras_mut()
            .set_complex_char_ring(row, col, value, visible_rows, cols);
    }

    /// Look up a complex char codepoint: ring buffer first (O(1)), then HashMap.
    ///
    /// Returns the first codepoint of the complex character. For single-emoji
    /// cells (ring path), this is the full character. For multi-char strings
    /// in the HashMap (combining sequences, ZWJ families), this returns only
    /// the base character. Use `complex_char_str_at` for the full string.
    #[inline]
    pub fn complex_char_at(&self, row: u16, col: u16) -> Option<char> {
        self.storage.extras().complex_codepoint_for(row, col)
    }

    /// Look up a complex char as full string: ring buffer (char→String) or HashMap (Arc<str>).
    ///
    /// More expensive than `complex_char_at` — allocates a String for each call.
    /// Use for text extraction (row_text, content export). For rendering where
    /// only the base codepoint is needed, use `complex_char_at`.
    #[inline]
    pub fn complex_char_str_at(&self, row: u16, col: u16) -> Option<String> {
        self.storage.extras().complex_char_str_for(row, col)
    }

    /// Look up fg RGB: ring buffer first (O(1)), then HashMap.
    ///
    /// Unified read method that transparently checks the dense RGB ring
    /// buffer before falling back to the CellExtras HashMap.
    #[inline]
    pub fn fg_rgb_at(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        self.storage.extras().fg_rgb_for(row, col)
    }

    /// Look up bg RGB: ring buffer first (O(1)), then HashMap.
    #[inline]
    pub fn bg_rgb_at(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        self.storage.extras().bg_rgb_for(row, col)
    }

    /// Look up Kitty graphics placeholder data for a cell.
    ///
    /// Returns `Some` if this cell is a Kitty Unicode placeholder (U+10EEEE)
    /// with image/placement coordinate metadata. The renderer uses this to
    /// draw the corresponding sub-region of a Kitty image at this cell.
    #[must_use]
    #[inline]
    pub fn kitty_placeholder_at(
        &self,
        row: u16,
        col: u16,
    ) -> Option<&crate::extra::KittyPlaceholderData> {
        self.storage
            .cell_extra(row, col)
            .and_then(|e| e.kitty_placeholder())
    }

    /// Remove extras for a single cell and clear its HAS_EXTRAS flag.
    ///
    /// Returns `true` if an entry was present and removed.
    #[inline]
    #[allow(
        dead_code,
        reason = "API for explicit extras removal; callers pending #5551"
    )]
    pub(crate) fn remove_cell_extra(&mut self, row: u16, col: u16) -> bool {
        self.storage.remove_cell_extra(row, col)
    }

    /// Enforce the hyperlink entry limit to prevent memory exhaustion.
    ///
    /// Evicts hyperlink data from the oldest entries when the extras map
    /// exceeds [`crate::extra_collection::MAX_HYPERLINK_ENTRIES`].
    /// Should be called after setting hyperlinks on cells (#7172).
    #[inline]
    pub fn enforce_hyperlink_limit(&mut self) {
        self.storage.extras_mut().enforce_hyperlink_limit();
    }

    /// Sync HAS_EXTRAS per-cell flags from the extras map for a given row.
    ///
    /// Bidirectional: sets the flag on cells with extras entries, clears it
    /// on cells without. Called after bulk extras operations (checkpoint
    /// restore, compaction) where per-cell flag maintenance was deferred.
    pub fn sync_extras_flags_for_row(&mut self, row: u16, cols: u16) {
        self.storage.sync_extras_flags_for_row(row, cols);
    }

    // -------------------------------------------------------------------------
    // Scroll region / horizontal margins
    // -------------------------------------------------------------------------

    /// Get the current scroll region.
    #[must_use]
    #[inline]
    pub fn scroll_region(&self) -> crate::ScrollRegion {
        self.storage.scroll_region()
    }

    /// Set the scroll region (DECSTBM).
    #[inline]
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.storage.set_scroll_region(top, bottom);
    }

    /// Reset scroll region to full screen.
    #[inline]
    pub fn reset_scroll_region(&mut self) {
        self.storage.reset_scroll_region();
    }

    /// Get the current horizontal margins (DECSLRM).
    #[must_use]
    #[inline]
    pub fn horizontal_margins(&self) -> crate::HorizontalMargins {
        self.storage.horizontal_margins()
    }

    /// Set horizontal margins (DECSLRM, VT420+).
    #[inline]
    pub fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        self.storage.set_horizontal_margins(left, right);
    }

    /// Reset horizontal margins to full width.
    #[inline]
    pub fn reset_horizontal_margins(&mut self) {
        self.storage.reset_horizontal_margins();
    }

    /// Get the current tab stops as a boolean slice (column-indexed).
    ///
    /// Each element is `true` if a tab stop is set at that column.
    /// Used by checkpoint serialization to persist custom tab stops (#7280).
    #[must_use]
    #[inline]
    pub fn tab_stops(&self) -> &[bool] {
        &self.storage.tab_stops
    }

    /// Replace the tab stops with the given boolean slice.
    ///
    /// Used by checkpoint deserialization to restore custom tab stops (#7280).
    /// A narrowed grid intentionally retains stops beyond its current width so a
    /// later grow restores the user's exact semantics. Accept only a vector that
    /// covers every active column and stays within [`crate::MAX_GRID_COLS`]; an
    /// invalid untrusted projection is a no-op.
    pub fn restore_tab_stops(&mut self, stops: &[bool]) {
        if stops.len() < usize::from(self.cols()) || stops.len() > usize::from(crate::MAX_GRID_COLS)
        {
            return;
        }
        self.storage.tab_stops.clear();
        self.storage.tab_stops.extend_from_slice(stops);
    }

    // -------------------------------------------------------------------------
    // Scrollback
    // -------------------------------------------------------------------------

    /// Get a reference to the scrollback storage.
    #[must_use]
    #[inline]
    pub fn scrollback(&self) -> Option<&aterm_scrollback::ScrollbackStorage> {
        self.storage.scrollback()
    }

    /// Attach a scrollback storage backend.
    #[inline]
    pub fn attach_scrollback(
        &mut self,
        scrollback: impl Into<aterm_scrollback::ScrollbackStorage>,
    ) {
        self.storage.attach_scrollback(scrollback);
    }

    /// Get the scrollback line limit.
    #[must_use]
    #[inline]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        self.storage.scrollback_line_limit()
    }

    /// Set the retained scrollback line limit (`None` = unlimited).
    ///
    /// Tiered grids: the limit governs the store (truncating immediately);
    /// the ring stays the fixed-size hot tier. Ring-only grids (no store —
    /// the wasm engines) have no other retention, so the limit re-caps the
    /// ring itself: growth stays lazy (rows allocate only as lines scroll
    /// off) and a shrink evicts the OLDEST lines immediately, mirroring the
    /// store's `set_line_limit` truncation semantics.
    pub fn set_scrollback_line_limit(&mut self, limit: Option<usize>) {
        if self.storage.scrollback.is_some() || self.storage.scrollback_detached_for_reflow {
            // Retained-set size before the truncation (the lazy-buffer drain
            // below only MOVES staged lines into the store, so a change here
            // means lines were really evicted).
            let before = self.scrollback_lines();
            // Drain staged lazy lines first (via scrollback_mut) so the store's
            // truncation sees the full history — the historical Terminal path.
            // While the store is out for an off-thread reflow this is a no-op
            // (the pre-existing behavior); the ring must NOT be re-capped then.
            if let Some(sb) = self.scrollback_mut() {
                sb.set_line_limit(limit);
            }
            // A shrink that evicted lines changed the retained/indexed set:
            // re-clamp a scrolled-back viewport and invalidate content-keyed
            // caches (search index, peer polls) exactly like the ring arm's
            // eviction does — a stale index otherwise keeps returning absolute
            // rows `line` already reports evicted, until the next write.
            if self.scrollback_lines() != before {
                self.clamp_display_offset();
                self.storage.mark_content_full();
            }
            return;
        }
        self.storage.max_scrollback = limit.unwrap_or(super::state::UNLIMITED_RING_SCROLLBACK);
        let excess = self
            .storage
            .ring_buffer_scrollback()
            .saturating_sub(self.storage.max_scrollback);
        if excess > 0 {
            self.evict_oldest_ring_scrollback(excess);
        }
    }

    // -------------------------------------------------------------------------
    // Damage control
    // -------------------------------------------------------------------------

    /// Number of lines in the ring buffer scrollback (not tiered).
    #[must_use]
    #[inline]
    pub fn ring_buffer_scrollback(&self) -> usize {
        self.storage.ring_buffer_scrollback()
    }

    /// Clear damage after rendering.
    pub fn clear_damage(&mut self) {
        let visible_rows = self.storage.visible_rows();
        self.storage.clear_damage(visible_rows);
    }

    /// Mark the cursor cell as damaged.
    pub fn mark_cursor_damage(&mut self) {
        let cursor = self.storage.cursor();
        self.storage.damage.mark_cell(cursor.row, cursor.col);
    }

    /// Damage the whole screen and bump `content_gen`, invalidating every
    /// content-keyed cache (search index, cross-session polls).
    ///
    /// The public seam for callers that evict retained scrollback OUTSIDE a
    /// Grid mutation — e.g. `Terminal::set_memory_budget`, which drives the
    /// store's budget enforcer directly — so the content generation still
    /// advances past the shrunk retained set (matching `set_scrollback_line_limit`).
    pub fn mark_content_full(&mut self) {
        self.storage.mark_content_full();
    }

    /// Check if the grid needs a full redraw (Kani proofs + FFI bridge tests).
    #[cfg(any(test, kani, feature = "testing"))]
    #[must_use]
    pub fn needs_full_redraw(&self) -> bool {
        self.storage.damage.is_full()
    }

    // -------------------------------------------------------------------------
    // Test-only convenience helpers (moved from aterm-core/src/grid/tests/mod.rs)
    // -------------------------------------------------------------------------

    /// Detach and return the scrollback storage, if any.
    #[cfg(test)]
    pub(crate) fn detach_scrollback(&mut self) -> Option<aterm_scrollback::ScrollbackStorage> {
        self.storage.scrollback.take()
    }

    /// Intern a style and return its ID.
    #[cfg(test)]
    pub(crate) fn intern_style(&mut self, style: crate::Style) -> crate::StyleId {
        self.storage.styles.intern(style)
    }

    /// Get a style by its ID.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn get_style(&self, id: crate::StyleId) -> Option<&crate::Style> {
        self.storage.styles.get(id)
    }

    /// Get style table statistics.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn style_stats(&self) -> crate::style::StyleTableStats {
        self.storage.styles.stats()
    }

    /// Clear all styles except the default.
    #[cfg(test)]
    pub(crate) fn clear_styles(&mut self) {
        self.storage.styles.clear();
    }
}
