// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid erase operations: EL, ED, ED 3 (scrollback), selective erase
//! (DECSEL/DECSED), DECALN, and the VT420 rectangle and attribute ops (DECERA,
//! DECFRA, DECSERA, DECCARA, DECRARA, DECCRA).
//!
//! # The deferred wrap (`pending_wrap`) follows xterm
//!
//! A printable written to the last column parks the cursor there and arms a
//! deferred wrap (xterm `dotext` sets `do_wrap`). Which erases cancel it follows
//! where xterm's `util.c` calls `ResetWrap`:
//! - ED 0/1/2, EL 0/1/2 and DECALN always reset it. EL 0 and ED 0 go through
//!   `ClearRight`, which clears from `cur_col` inclusive (the parked glyph).
//! - DECSEL 0 always resets it (`ClearRight`'s trailing `ResetWrap` ignores
//!   protection). So does DECSED 0, except at the origin of a one-column line,
//!   where it acts as DECSED 2. DECSEL 1/2 and DECSED 1/2 reset it unless every
//!   cell of xterm's span is protected (`ClearInLine2` returns before its
//!   `ResetWrap`).
//! - ECH resets it (`do_erase_char` -> `ClearRight`; see `line_ops.rs`).
//! - ED 3 and the rectangle / attribute / copy ops keep it: `do_erase_display`
//!   case 3 only drops saved lines, and xterm's `screen.c` rectangle code never
//!   touches `do_wrap`.
//!
//! xterm.js differs (it stores a pending wrap as `x == cols` and erases the
//! empty range `[cols, cols)`); xterm and Ghostty match the rules above.
//!
//! # DECSLRM horizontal margins do not bound an erase
//!
//! VT510 says EL, ECH and ED "work inside or outside the scrolling margins",
//! while ICH and DCH "have no effect outside" them. xterm, Ghostty, iTerm2 and
//! WezTerm all follow that split; aterm's margin-bound operations (ICH, DCH,
//! DECIC, DECDC, autowrap) keep their margins: see `line_ops.rs`.

use super::{Cursor, Grid, ScrollRegion};
use crate::PageStore;
use crate::Row;

/// One captured source cell for a DECCRA (`copy_rect`) ring restore:
/// `(row_offset, col_offset, fg_rgb, bg_rgb, complex_char)`, all relative to the
/// destination rect. The `Option`s are `None` when the source cell carried no
/// out-of-line truecolor / complex codepoint, so the restore is a no-op there.
type RingCapture = (u16, u16, Option<[u8; 3]>, Option<[u8; 3]>, Option<char>);

impl Grid {
    // ---- BCE RGB fill helpers ----

    /// Write BCE truecolor background into `RgbColorRing` for a column range.
    ///
    /// No-op when `cursor_template_bg_rgb` is `None` (indexed or default bg).
    /// Must be called after `extras.clear_range()` to repopulate the ring.
    pub(in crate::grid) fn fill_bce_rgb_range(&mut self, row: u16, col_start: u16, col_end: u16) {
        if let Some(bg_rgb) = self.storage.cursor_template_bg_rgb {
            let vis = self.storage.visible_rows;
            let cols = self.storage.cols;
            self.storage.extras.set_rgb_ring_range(
                row,
                col_start,
                col_end,
                None,
                Some(bg_rgb),
                vis,
                cols,
            );
        }
    }

    /// Give the wide halves a row op blanked with the BCE fill
    /// ([`BlankedHalves`](crate::row::BlankedHalves)) the fill's ring bytes, as
    /// [`Self::fill_bce_rgb_range`] gives the span it vacated. Call it after the
    /// extras shift, which moves the ring with the cells.
    pub(in crate::grid) fn fill_bce_rgb_cols(&mut self, row: u16, cols: crate::row::BlankedHalves) {
        for col in cols.into_iter().flatten() {
            self.fill_bce_rgb_range(row, col, col.saturating_add(1));
        }
    }

    /// Write BCE truecolor background for a range of full rows.
    ///
    /// No-op when `cursor_template_bg_rgb` is `None`.
    pub(in crate::grid) fn fill_bce_rgb_rows(&mut self, range: core::ops::Range<u16>) {
        self.fill_bce_rgb_rect(range, 0..self.storage.cols);
    }

    /// Write BCE truecolor background for a rectangular area.
    ///
    /// No-op when `cursor_template_bg_rgb` is `None`.
    pub(in crate::grid) fn fill_bce_rgb_rect(
        &mut self,
        rows: core::ops::Range<u16>,
        cols: core::ops::Range<u16>,
    ) {
        if self.storage.cursor_template_bg_rgb.is_some() {
            for row in rows {
                self.fill_bce_rgb_range(row, cols.start, cols.end);
            }
        }
    }

    /// Write the BCE truecolor background for the cells of `cols` on `row`
    /// that a selective erase cleared: every cell there that is not protected
    /// afterwards (the fill never is, and a protected cell keeps its own
    /// colours and ring bytes).
    ///
    /// No-op when `cursor_template_bg_rgb` is `None`.
    fn fill_bce_rgb_unprotected(&mut self, row: u16, cols: core::ops::Range<u16>) {
        if self.storage.cursor_template_bg_rgb.is_none() {
            return;
        }
        let cleared: Vec<u16> = match self.row(row) {
            Some(r) => cols.filter(|&c| !r.is_cell_protected(c)).collect(),
            None => return,
        };
        for col in cleared {
            self.fill_bce_rgb_range(row, col, col.saturating_add(1));
        }
    }

    // ---- Private helpers: regular and selective erase ----

    /// Erase from cursor to end of line.
    ///
    /// When `selective`, only unprotected cells are cleared and extras are
    /// preserved. Non-selective erase uses the BCE cursor template for fill.
    fn erase_to_end_of_line_impl(&mut self, selective: bool) {
        // Record selection damage for the cursor row: a `\r` + EL progress bar
        // rewriting the row must not leave a stale highlight over new text.
        let cursor_row = self.storage.cursor.row;
        self.damage_selection_visible_rows(cursor_row, cursor_row);
        self.erase_to_end_of_line_core(selective);
    }

    /// Core implementation for erase-to-end-of-line.
    ///
    /// The span ends at the row's own edge; DECLRMM's right margin does not move
    /// it (xterm's `ClearRight` bounds by `MaxCols(screen)`; see the module
    /// note). Shared with ED/DECSED, which record their own selection damage.
    fn erase_to_end_of_line_core(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let right_bound = self.storage.effective_cols_for_row(cursor_row);
        let fill = self.storage.cursor_template;
        if cursor_col < right_bound {
            // The BCE span for the truecolor side table: `clear_range_with_orphan`
            // widens it by the wide halves it had to rewrite (see below).
            let mut bce = cursor_col..right_bound;
            if let Some(row) = self.row_mut(cursor_row) {
                if selective {
                    bce = row.selective_clear_range(cursor_col, right_bound, fill);
                } else {
                    // xterm `ClearRight`: the orphaned wide half goes through the
                    // same `ClearCells` as the erased span, so it receives the BCE
                    // blank, not a default-coloured one (rect ops differ — see
                    // `Row::clear_range_with_orphan`).
                    bce = row.clear_range_with_orphan(cursor_col, right_bound, fill, fill);
                }
            }
            if selective {
                // DECSEL resets the unprotected cells it clears (and the halves of
                // their pairs); their extras go with them, a protected cell keeps
                // its claim. The cleared cells take the BCE blank, so a truecolor
                // background needs its ring bytes, for them alone.
                self.drop_unclaimed_extras(
                    cursor_row,
                    cursor_col.saturating_sub(1)..right_bound.saturating_add(1),
                );
                self.fill_bce_rgb_unprotected(cursor_row, bce);
            } else {
                // `bce` reaches the orphaned head left of the span, so its extras
                // go too.
                self.storage
                    .extras
                    .clear_range(cursor_row, bce.start, bce.end);
                // A truecolor BCE cell holds only an RGB marker; the bytes live in
                // the dense ring, so the ring must also cover the orphan column.
                self.fill_bce_rgb_range(cursor_row, bce.start, bce.end);
            }
            self.storage.mark_content_row(cursor_row);
        }
        self.clear_wrap_into_next_row(cursor_row);
    }

    /// Break the soft wrap from `row` into the row below, as xterm `ClearRight`
    /// always ends with `LineClrWrapped(ld)` ("with the right part cleared, we
    /// can't be wrapping"): after EL 0 / ED 0 / DECSEL 0 / DECSED 0 (and an ECH
    /// reaching the last column, see `erase_chars`). xterm keeps the flag on the
    /// wrapping row; this grid keeps it on the continuation, the next row.
    ///
    /// A stale link would join unrelated lines: conhost's `cls` erases row by
    /// row with `CSI K`, and output written over those rows would otherwise
    /// rewrap on resize as one line with its predecessor.
    pub(in crate::grid) fn clear_wrap_into_next_row(&mut self, row: u16) {
        let next = row.saturating_add(1);
        if next >= self.storage.visible_rows {
            return;
        }
        if let Some(below) = self.row_mut(next)
            && below.is_wrapped()
        {
            below.set_wrapped(false);
            // The cells are unchanged but the logical line split in two, a
            // content change for every reader that joins continuations (search,
            // copy, rewrap).
            self.storage.mark_content_row(next);
        }
    }

    /// Erase from start of line to cursor.
    ///
    /// When `selective`, only unprotected cells are cleared and extras are
    /// preserved. Non-selective erase uses the BCE cursor template for fill.
    fn erase_from_start_of_line_impl(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        self.damage_selection_visible_rows(cursor_row, cursor_row);
        self.erase_from_start_of_line_core(selective);
    }

    /// Core implementation for erase-from-start-of-line.
    ///
    /// The span starts at column 0; DECLRMM's left margin does not move it
    /// (xterm's `ClearLeft` opens at column 0, Ghostty's EL 1 span is
    /// `{0, x + 1}`; see the module note).
    fn erase_from_start_of_line_core(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        let end = cursor_col.saturating_add(1).min(effective_cols);
        let fill = self.storage.cursor_template;
        if end > 0 {
            let mut bce = 0..end;
            if let Some(row) = self.row_mut(cursor_row) {
                if selective {
                    bce = row.selective_clear_range(0, end, fill);
                } else {
                    // BCE blank on the orphan, per xterm `ClearRight` (see
                    // `erase_to_end_of_line_core`).
                    bce = row.clear_range_with_orphan(0, end, fill, fill);
                }
            }
            if selective {
                // As in `erase_to_end_of_line_core`.
                self.drop_unclaimed_extras(cursor_row, 0..end.saturating_add(1));
                self.fill_bce_rgb_unprotected(cursor_row, bce);
            } else {
                // `bce` reaches the orphaned continuation right of the span.
                self.storage
                    .extras
                    .clear_range(cursor_row, bce.start, bce.end);
                self.fill_bce_rgb_range(cursor_row, bce.start, bce.end);
            }
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Erase entire line at cursor.
    ///
    /// The whole row, margins or not (xterm's `ClearLine` bounds by `MaxCols`;
    /// see the module note). When `selective`, only unprotected cells are
    /// cleared and extras are preserved. Non-selective erase uses the BCE
    /// cursor template for fill.
    fn erase_line_impl(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        self.damage_selection_visible_rows(cursor_row, cursor_row);
        let fill = self.storage.cursor_template;
        if let Some(row) = self.row_mut(cursor_row) {
            if selective {
                row.selective_clear(fill);
            } else {
                // `erase_with()` keeps DECDWL/DECDHL line attributes, which EL
                // keeps where ED 2 drops them, as in xterm (`ClearInLine2` never
                // writes `DblCS`).
                row.erase_with(fill);
            }
        }
        if selective {
            // As in `erase_to_end_of_line_core`.
            self.drop_unclaimed_extras(cursor_row, 0..self.storage.cols);
            self.fill_bce_rgb_unprotected(cursor_row, 0..self.storage.cols);
        } else {
            self.storage.extras.clear_row(cursor_row);
            self.fill_bce_rgb_range(cursor_row, 0, self.storage.cols);
        }
        self.storage.mark_content_row(cursor_row);
    }

    /// Clear a range of full rows for a screen-level erase: xterm's
    /// `ClearBufRows`, including its line-attribute reset. `selective` clears
    /// only unprotected cells and keeps extras; otherwise the fill is the BCE
    /// cursor template. Extras clear in one batch, O(E) rather than O(R × E).
    fn clear_rows(&mut self, range: core::ops::Range<u16>, selective: bool) {
        let fill = self.storage.cursor_template;
        for row in range.clone() {
            if let Some(r) = self.row_mut(row) {
                if selective {
                    r.selective_clear(fill);
                } else {
                    // A complete row a plain ED erases comes back single-width
                    // and single-height (xterm `ClearBufRows`: "clearing the
                    // whole row resets the doublesize characters"). DECSED must
                    // not reset: xterm's protected branch never reaches
                    // `ClearBufRows`.
                    r.reset_with(fill);
                }
            }
        }
        if selective {
            // As in `erase_to_end_of_line_core`.
            for row in range.clone() {
                self.drop_unclaimed_extras(row, 0..self.storage.cols);
                self.fill_bce_rgb_unprotected(row, 0..self.storage.cols);
            }
        } else {
            self.storage.extras.clear_rows(range.clone());
            self.fill_bce_rgb_rows(range);
            // ED 0 and ED 1 leave the cursor row's attribute alone (a partial
            // erase; `ClearRight` / `ClearLeft` never touch `DblCS`), so the
            // grid-level flag must be rescanned, not assumed false.
            self.resync_any_double_width();
        }
    }

    /// Recompute the `any_double_width` fast path from the visible rows.
    ///
    /// The flag is asymmetric: left `true` with no double row it only costs a
    /// per-row `line_size()` lookup, but left `false` while a row is double it
    /// drops the half-width column clamp (a correctness bug). So it is only
    /// lowered by a rescan, run only when it is raised.
    fn resync_any_double_width(&mut self) {
        if !self.storage.any_double_width {
            return;
        }
        self.storage.any_double_width =
            (0..self.storage.visible_rows).any(|row| self.storage.row_is_double_width(row));
    }

    // ---- Line erase (EL) ----

    /// Erase from cursor to end of line.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn erase_to_end_of_line(&mut self) {
        // xterm `do_erase_line` case 0 -> `ClearRight`, which clears from
        // `cur_col` inclusive (a wide glyph parked on its continuation loses its
        // head via `clear_range_with`'s fixup) and resets the wrap. The reset
        // lives here so CSI K and VT52 ESC K both get it.
        self.erase_to_end_of_line_impl(false);
        self.storage.clear_pending_wrap();
    }

    /// Erase from start of line to cursor.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn erase_from_start_of_line(&mut self) {
        // xterm `do_erase_line` case 1 -> `ClearLeft` -> `ClearInLine2`, which
        // resets the wrap, so a later glyph overwrites the last column.
        self.erase_from_start_of_line_impl(false);
        self.storage.clear_pending_wrap();
    }

    /// Erase entire line.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn erase_line(&mut self) {
        // xterm `do_erase_line` case 2 -> `ClearLine` -> `ClearInLine2`
        // (`ResetWrap`), over the whole row whatever DECLRMM says.
        self.erase_line_impl(false);
        self.storage.clear_pending_wrap();
    }

    // ---- Screen erase (ED) ----

    /// Erase from cursor to end of screen.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn erase_to_end_of_screen(&mut self) {
        // xterm `do_erase_display` case 0: `ClearBelow`, starting with
        // `ClearRight` (cursor row from `cur_col` inclusive, wrap reset), or
        // `ClearScreen` at the origin. `do_cd_xtra_scroll` cannot change the wrap
        // (xtermScroll saves and restores `do_wrap`). VT52 ESC J shares this path.
        self.erase_below_cursor(false);
        self.storage.clear_pending_wrap();
    }

    /// Erase from start of screen to cursor.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    pub fn erase_from_start_of_screen(&mut self) {
        // xterm `do_erase_display` case 1: `ClearAbove` ending in `ClearLeft` ->
        // `ClearInLine2` (`ResetWrap`), or `ClearScreen` on the bottom-right cell.
        self.erase_above_cursor(false);
        self.storage.clear_pending_wrap();
    }

    /// ED 0 / DECSED 0 without the wrap reset: the cursor row from the cursor
    /// on, then every row below it.
    fn erase_below_cursor(&mut self, selective: bool) {
        // `_core`: ED records the whole band's selection damage itself.
        self.erase_to_end_of_line_core(selective);
        let cursor_row = self.storage.cursor.row;
        let visible_rows = self.storage.visible_rows;
        self.clear_rows(cursor_row.saturating_add(1)..visible_rows, selective);
        // The blanks below the cursor are genuine now — an in-flight offloaded
        // reflow must not deficit-fill them from pre-erase history.
        self.invalidate_pending_fill_target();
        // Visible rows only, so a selection anchored in scrollback survives.
        // `moves_coordinates = false`: an erase rewrites cells in place, so no
        // host-cached coordinate goes stale (and every prompt redraw's `ESC[J`
        // leaves the GUI's cursor effects alone). Pinned by
        // `an_erase_rewrites_cells_in_place_and_invalidates_no_host_coordinate`.
        self.damage_selection_visible_rows_ext(cursor_row, visible_rows.saturating_sub(1), false);
        // cursor row already marked by erase_to_end_of_line_core; mark remaining rows.
        self.storage
            .mark_content_rows(cursor_row.saturating_add(1), visible_rows);
    }

    /// ED 1 / DECSED 1 without the wrap reset: every row above the cursor, then
    /// the cursor row up to and including the cursor.
    fn erase_above_cursor(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        self.clear_rows(0..cursor_row, selective);
        // `_core`: ED records the whole band's selection damage itself.
        self.erase_from_start_of_line_core(selective);
        self.damage_selection_visible_rows_ext(0, cursor_row, false);
        // cursor row already marked by erase_from_start_of_line_core; mark rows above.
        self.storage.mark_content_rows(0, cursor_row);
    }

    /// Erase entire screen (ED 2) with the BCE cursor template, and snap the
    /// viewport to the live screen (`display_offset` 0).
    pub fn erase_screen(&mut self) {
        // xterm `do_erase_display` case 2 -> `ClearScreen`, which resets the wrap
        // unconditionally. Every caller inherits it, as each runs `ClearScreen`
        // in xterm: CSI 2 J, the 1047 exit, the 1049 enter, DECCOLM and RIS.
        self.storage.clear_pending_wrap();
        // ED 2 shows the freshly erased live screen, not a scrolled-back view.
        self.storage.display_offset = 0;
        let fill = self.storage.cursor_template;
        for row in 0..self.storage.visible_rows {
            if let Some(r) = self.row_mut(row) {
                // ED 2 erases complete lines, which come back single-width and
                // single-height (VT510 ED: "When you erase complete lines, they
                // become single-height, single-width lines"; xterm `ClearScreen`
                // -> `ClearBufRows` -> `SetLineDblCS(ld, CSET_SWL)`).
                // `reset_with()` is `erase_with()` plus that reset. Partial
                // erases (EL, ECH, DECERA, the ED cursor row) and DECSED keep the
                // attribute.
                r.reset_with(fill);
            }
        }
        self.storage.extras.clear();
        self.fill_bce_rgb_rows(0..self.storage.visible_rows);
        // `reset_with` dropped every row's line attributes, so the grid-level
        // flag drops without a rescan.
        self.storage.any_double_width = false;
        // Visible rows only, `moves_coordinates = false`: see `erase_below_cursor`.
        let last = self.storage.visible_rows.saturating_sub(1);
        self.damage_selection_visible_rows_ext(0, last, false);
        // ED 2's blanks are genuine — an in-flight offloaded reflow must not
        // deficit-fill the cleared screen from pre-erase history.
        self.invalidate_pending_fill_target();
        self.storage.mark_content_full();
    }

    /// Clear all DEC line attributes (DECDWL/DECDHL) on every visible row.
    ///
    /// For RIS and DECCOLM, whose specs owe the reset: ED 2 resets the rows it
    /// erases, but a full reset must not depend on which erase a caller routed
    /// through, since partial erases keep the attributes.
    pub fn clear_line_attributes(&mut self) {
        for row_idx in 0..self.storage.visible_rows {
            if let Some(r) = self.row_mut(row_idx) {
                r.set_line_size(super::LineSize::SingleWidth);
            }
        }
        self.storage.any_double_width = false;
    }

    // ---- Scrollback erase ----

    /// Erase scrollback.
    ///
    /// ENSURES: self.storage.display_offset == 0
    /// ENSURES: self.storage.total_lines == self.storage.visible_rows as usize
    pub fn erase_scrollback(&mut self) {
        // Bump unconditionally, even while the store is detached for an
        // off-thread reflow: the in-flight reflow captured the pre-erase
        // generation and drops its stale store on re-attach instead of
        // resurrecting erased history.
        self.storage.scrollback_clear_gen = self.storage.scrollback_clear_gen.wrapping_add(1);
        let scrollback = self
            .storage
            .total_lines
            .saturating_sub(self.storage.visible_rows as usize);
        if scrollback == 0 {
            if let Some(scrollback) = self.storage.scrollback.as_mut()
                && let Err(e) = scrollback.clear()
            {
                aterm_log::warn!("scrollback clear failed: {e}");
            }
            self.storage.lazy_buffer.clear();
            self.storage.ring_extras.clear();
            self.storage.display_offset = 0;
            self.storage.mark_content_full();
            // ED 3 discards history wholesale: selection damage `All`, but no
            // live coordinate moves (see `discard_history_selection`).
            self.discard_history_selection();
            // ED 3 keeps the deferred wrap (module note).
            debug_assert_eq!(self.storage.display_offset, 0);
            debug_assert_eq!(self.storage.total_lines, self.storage.visible_rows as usize);
            return;
        }

        // Preserve the live (display_offset = 0) visible rows and drop scrollback.
        debug_assert!(
            !self.storage.rows.is_empty(),
            "clear_scrollback: ring buffer has zero rows"
        );
        let live_top = (self.storage.ring_head + scrollback) % self.storage.rows.len();
        let mut new_pages = PageStore::new();
        let mut new_rows = Vec::with_capacity(self.storage.visible_rows as usize);
        for i in 0..self.storage.visible_rows {
            let idx = (live_top + i as usize) % self.storage.rows.len();
            // SAFETY: `new_rows` and `new_pages` are installed into `self`
            // together before the rebuilt rows can outlive this method.
            let mut row = unsafe { Row::new(self.storage.cols, &mut new_pages) };
            row.copy_from(&self.storage.rows[idx]);
            new_rows.push(row);
        }

        self.storage.rows = new_rows;
        self.storage.pages = new_pages;
        self.storage.total_lines = self.storage.visible_rows as usize;
        self.storage.ring_head = 0;
        self.storage.display_offset = 0;
        if let Some(scrollback) = self.storage.scrollback.as_mut()
            && let Err(e) = scrollback.clear()
        {
            aterm_log::warn!("scrollback clear failed: {e}");
        }
        self.storage.lazy_buffer.clear();
        self.storage.ring_extras.clear();
        // Extras are keyed by visible row, and the visible rows stay put.
        self.storage.mark_content_full();
        // ED 3 discards history wholesale; see `discard_history_selection`.
        self.discard_history_selection();
        // ED 3 keeps the deferred wrap (module note).
        debug_assert_eq!(self.storage.display_offset, 0);
        debug_assert_eq!(self.storage.total_lines, self.storage.visible_rows as usize);
    }

    // ---- Selective erase (DECSED/DECSEL) ----

    /// Selectively erase from cursor to end of line (DECSEL 0): unprotected
    /// (DECSCA) cells only.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    pub fn selective_erase_to_end_of_line(&mut self) {
        // xterm `do_erase_line` case 0 with DEC_PROTECT -> `ClearRight`, whose
        // trailing `ResetWrap` ignores protection: the wrap resets even when a
        // protected parked cell survives.
        self.erase_to_end_of_line_impl(true);
        self.storage.clear_pending_wrap();
    }

    /// Selectively erase from start of line to cursor (DECSEL 1): unprotected
    /// (DECSCA) cells only.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    pub fn selective_erase_from_start_of_line(&mut self) {
        // xterm `do_erase_line` case 1 with DEC_PROTECT -> `ClearLeft` ->
        // `ClearInLine2` over `[0, cur_col]`: the wrap survives a fully
        // protected span.
        self.erase_from_start_of_line_impl(true);
        let cursor_row = self.storage.cursor.row;
        let span_end = self.storage.cursor.col.saturating_add(1);
        self.reset_wrap_unless_span_protected(0..0, Some((cursor_row, span_end)));
    }

    /// Selectively erase entire line (DECSEL 2): unprotected (DECSCA) cells
    /// only.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    pub fn selective_erase_line(&mut self) {
        // xterm `do_erase_line` case 2 with DEC_PROTECT -> `ClearLine` ->
        // `ClearInLine2` over the whole row: the wrap survives a fully
        // protected row.
        self.erase_line_impl(true);
        let cursor_row = self.storage.cursor.row;
        self.reset_wrap_unless_span_protected(cursor_row..cursor_row.saturating_add(1), None);
    }

    /// Selectively erase from cursor to end of screen (DECSED 0): unprotected
    /// (DECSCA) cells only.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    pub fn selective_erase_to_end_of_screen(&mut self) {
        // xterm `do_erase_display` case 0 with DEC_PROTECT: away from the origin,
        // `ClearBelow` -> `ClearRight`, whose trailing `ResetWrap` is
        // unconditional. At the origin (a wrap pending there needs a one-column
        // line) it reduces to the DECSED 2 loop, which resets only if some
        // visible cell is unprotected.
        let at_origin = self.storage.cursor.row == 0 && self.storage.cursor.col == 0;
        self.erase_below_cursor(true);
        if at_origin {
            self.reset_wrap_unless_span_protected(0..self.storage.visible_rows, None);
        } else {
            self.storage.clear_pending_wrap();
        }
    }

    /// Selectively erase from start of screen to cursor (DECSED 1): unprotected
    /// (DECSCA) cells only.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    pub fn selective_erase_from_start_of_screen(&mut self) {
        // xterm `do_erase_display` case 1 with DEC_PROTECT: `ClearAbove`'s
        // protected branch (`ClearInLine` per row above, then `ClearLeft` over
        // `[0, cur_col]`), or at the bottom-right cell the DECSED 2 loop. Each
        // `ClearInLine2` resets only if its span holds an unprotected cell; both
        // cases span the rows above plus `[0, cursor_col]` of the cursor row.
        let cursor_row = self.storage.cursor.row;
        let span_end = self.storage.cursor.col.saturating_add(1);
        self.erase_above_cursor(true);
        self.reset_wrap_unless_span_protected(0..cursor_row, Some((cursor_row, span_end)));
    }

    /// Selectively erase entire screen (DECSED 2): unprotected (DECSCA) cells
    /// only.
    pub fn selective_erase_screen(&mut self) {
        // xterm `do_erase_display` case 2 with DEC_PROTECT: with `protected_mode`
        // on, the per-row `ClearInLine` loop, which resets only if some visible
        // cell is unprotected; with it off (no DECSCA ever sent, so in aterm no
        // protected cell), `ClearScreen`, which always resets. The
        // any-unprotected rule covers both.
        let visible_rows = self.storage.visible_rows;
        self.clear_rows(0..self.storage.visible_rows, true);
        // Same debt invalidation as ED 2: the cleared screen's blanks are
        // genuine, never deficit-fill them from pre-erase history.
        self.invalidate_pending_fill_target();
        // Visible rows only, `moves_coordinates = false`: see `erase_below_cursor`.
        let last = self.storage.visible_rows.saturating_sub(1);
        self.damage_selection_visible_rows_ext(0, last, false);
        self.storage.mark_content_full();
        self.reset_wrap_unless_span_protected(0..visible_rows, None);
    }

    /// Reset the deferred wrap the way xterm's protected erase (`ClearInLine2`)
    /// does: only when some cell of the erase's span is unprotected. The span is
    /// every column of `full_rows` plus, when given, columns `[0, end)` of row
    /// `partial.0`, at full width (DECLRMM ignored). A selective clear never
    /// changes protection, so asking after the clear is equivalent.
    fn reset_wrap_unless_span_protected(
        &mut self,
        full_rows: core::ops::Range<u16>,
        partial: Option<(u16, u16)>,
    ) {
        if !self.storage.pending_wrap() {
            return;
        }
        let cols = self.storage.cols;
        // `Row::is_cell_protected` untangles the PROTECTED / WIDE_CONTINUATION
        // bit alias. A row that is not materialized holds no protected cell.
        let span_has_unprotected = |grid: &Self, row: u16, end: u16| {
            let end = end.min(cols);
            grid.row(row)
                .map_or(end > 0, |r| (0..end).any(|col| !r.is_cell_protected(col)))
        };
        let any_unprotected = full_rows
            .into_iter()
            .any(|row| span_has_unprotected(self, row, cols))
            || partial.is_some_and(|(row, end)| span_has_unprotected(self, row, end));
        if any_unprotected {
            self.storage.clear_pending_wrap();
        }
    }

    /// DECALN (`ESC # 8`): fill the screen with 'E' and reset all margins.
    ///
    /// ENSURES: self.storage.cursor.row == 0
    /// ENSURES: self.storage.cursor.col == 0
    /// ENSURES: self.storage.scroll_region.is_full(self.storage.visible_rows)
    pub fn screen_alignment_pattern(&mut self) {
        self.storage.clear_pending_wrap();
        // Per xterm/VT420: DECALN resets the DECSTBM and DECLRMM margins.
        self.storage.scroll_region = ScrollRegion::full(self.storage.visible_rows);
        self.storage.reset_horizontal_margins();

        let cols = self.storage.cols;

        // clear() drops stale colors/flags before the 'E' fill.
        for row in 0..self.storage.visible_rows {
            if let Some(r) = self.row_mut(row) {
                r.clear();
                for col in 0..cols {
                    r.write_char(col, 'E');
                }
            }
        }

        // Drop overflow extras (RGB, hyperlinks, complex chars) left orphaned.
        self.storage.extras.clear();

        // r.clear() above reset each row to single-width, so the grid-level flag
        // drops too.
        self.storage.any_double_width = false;

        self.storage.cursor = Cursor::default();

        // History is untouched, so a scrollback selection survives; no row moves.
        let last = self.storage.visible_rows.saturating_sub(1);
        self.damage_selection_visible_rows_ext(0, last, false);
        self.storage.mark_content_full();
        debug_assert_eq!(self.storage.cursor.row, 0);
        debug_assert_eq!(self.storage.cursor.col, 0);
        debug_assert!(
            self.storage
                .scroll_region
                .is_full(self.storage.visible_rows)
        );
    }

    /// Erase rectangular area (DECERA - VT420+): the rectangle (top,
    /// left)-(bottom, right), 0-indexed, becomes the BCE blank (the cursor
    /// template).
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cols > 0
    pub fn erase_rect(&mut self, top: u16, left: u16, bottom: u16, right: u16) {
        // DECERA is DECFRA with the BCE blank: the cursor template, plus its
        // truecolor background when that lives in the ring.
        self.fill_rect(
            self.storage.cursor_template,
            top,
            left,
            bottom,
            right,
            None,
            self.storage.cursor_template_bg_rgb,
        );
    }

    /// Fill rectangular area with a cell template (DECFRA - VT420+): the
    /// rectangle (top, left)-(bottom, right), 0-indexed, gets `fill`, which
    /// carries the character and current SGR attributes.
    ///
    /// `fg_rgb`/`bg_rgb` carry the truecolor bytes when `fill`'s colors are RGB
    /// (the cell holds only the marker); they are written back into the dense
    /// RGB ring after the rect's stale entries are cleared.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cols > 0
    #[allow(
        clippy::too_many_arguments,
        reason = "DECFRA fill needs the rect bounds + fill cell + the resolved \
                  fg/bg truecolor bytes to repopulate the out-of-line RGB ring"
    )]
    pub fn fill_rect(
        &mut self,
        fill: super::Cell,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        fg_rgb: Option<[u8; 3]>,
        bg_rgb: Option<[u8; 3]>,
    ) {
        let top = top.min(self.storage.visible_rows.saturating_sub(1));
        let bottom = bottom.min(self.storage.visible_rows.saturating_sub(1));
        let left = left.min(self.storage.cols.saturating_sub(1));
        let right = right.min(self.storage.cols.saturating_sub(1));

        if top > bottom || left > right {
            return;
        }

        // DECERA / DECFRA / DECSERA replace cells in place, so a highlight over
        // the rectangle is damaged. Row-granular: the band is the rect's rows,
        // wider than the rectangle, which is the safe direction.
        self.damage_selection_visible_rows(top, bottom);

        let end_col = right.saturating_add(1);

        // `clear_range_with` fixes up wide pairs at the range edges.
        for row_idx in top..=bottom {
            if let Some(row) = self.row_mut(row_idx) {
                row.clear_range_with(left, end_col, fill);
            }
            self.storage.mark_content_row(row_idx);
        }
        self.storage
            .extras
            .clear_rect(top..bottom.saturating_add(1), left..end_col);
        // The halves of pairs the rectangle's edges bisected, blanked by
        // `clear_range_with`.
        for row_idx in top..=bottom {
            self.drop_unclaimed_extras(row_idx, left.saturating_sub(1)..end_col.saturating_add(1));
        }

        // The cell carries only RGB markers and clear_rect wiped the ring, so
        // repopulate it. set_rgb_ring_range ignores a None axis.
        if fg_rgb.is_some() || bg_rgb.is_some() {
            let visible_rows = self.storage.visible_rows;
            let cols = self.storage.cols;
            for row_idx in top..=bottom {
                self.storage.extras.set_rgb_ring_range(
                    row_idx,
                    left,
                    end_col,
                    fg_rgb,
                    bg_rgb,
                    visible_rows,
                    cols,
                );
            }
        }
    }

    /// Selectively erase rectangular area (DECSERA - VT420+), 0-indexed:
    /// characters not protected by DECSCA become spaces.
    ///
    /// Per VT520 (EK-VT520-RM), "DECSERA does not change: visual attributes set
    /// by the select graphic rendition (SGR) function; protection attributes set
    /// by DECSCA; line attributes." xterm matches (`ScrnWipeRectangle` rewrites
    /// only the character), so each cell keeps its SGR flags and colors.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cols > 0
    pub fn selective_erase_rect(&mut self, top: u16, left: u16, bottom: u16, right: u16) {
        let top = top.min(self.storage.visible_rows.saturating_sub(1));
        let bottom = bottom.min(self.storage.visible_rows.saturating_sub(1));
        let left = left.min(self.storage.cols.saturating_sub(1));
        let right = right.min(self.storage.cols.saturating_sub(1));

        if top > bottom || left > right {
            return;
        }

        // Row-granular selection damage, as in `fill_rect`.
        self.damage_selection_visible_rows(top, bottom);

        let end_col = right.saturating_add(1);

        // `set_char` blanks an unprotected cell but keeps HAS_EXTRAS and its
        // CellExtra, so out-of-line SGR colors survive (VT520 DECSERA) but so
        // would combining marks and complex strings. Scrub those from exactly
        // the wiped cells, dropping an entry left with nothing color-bearing.
        // Protected cells keep their extras.
        let scrub_extras = !self.storage.extras.is_empty();
        let mut wiped_cols: Vec<u16> = Vec::new();
        for row_idx in top..=bottom {
            wiped_cols.clear();
            if let Some(row) = self.row_mut(row_idx) {
                row.selective_wipe_range(left, end_col, &mut wiped_cols);
            }
            if scrub_extras {
                for &col in &wiped_cols {
                    if self.storage.extras.scrub_char_marks(row_idx, col) {
                        // Entry is now empty — drop it and clear the flag to
                        // keep the HAS_EXTRAS ⇔ entry-exists invariant.
                        self.remove_cell_extra(row_idx, col);
                    }
                }
            }
            self.storage.mark_content_row(row_idx);
        }
    }

    /// Rewrite the attribute flags of every cell in a rectangle, or with
    /// `stream` (DECSACE stream mode) a character stream, through `edit`: the
    /// walk behind DECCARA and DECRARA. A stream's first row starts at `left`,
    /// middle rows span the full width, and its last row ends at `right`.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cols > 0
    fn edit_attrs(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        stream: bool,
        edit: impl Fn(super::CellFlags) -> super::CellFlags,
    ) {
        let top = top.min(self.storage.visible_rows.saturating_sub(1));
        let bottom = bottom.min(self.storage.visible_rows.saturating_sub(1));
        let left = left.min(self.storage.cols.saturating_sub(1));
        let right = right.min(self.storage.cols.saturating_sub(1));

        // A stream's `left > right` is legal across rows.
        if top > bottom || (!stream && left > right) {
            return;
        }

        let max_col = self.storage.cols.saturating_sub(1);

        for row_idx in top..=bottom {
            let start = if stream && row_idx != top { 0 } else { left };
            let end = if stream && row_idx != bottom {
                max_col
            } else {
                right
            };

            if start > end {
                continue;
            }

            if let Some(row) = self.row_mut(row_idx) {
                for col in start..=end {
                    if let Some(cell) = row.get_mut(col) {
                        let flags = edit(cell.flags());
                        cell.set_flags(flags);
                    }
                }
                // `get_mut` bypasses len maintenance: a flag on a blank cell
                // grows len, clearing the last flag can shrink it.
                row.recompute_len();
            }
            self.storage.mark_content_row(row_idx);
        }
    }

    /// Reverse one cell's attribute flags for DECRARA.
    ///
    /// `toggle` holds only single-bit attributes (bold, blink, inverse, hidden),
    /// which reverse by XOR. Underline is a three-bit family (`UNDERLINE`,
    /// `DOUBLE_UNDERLINE`, `CURLY_UNDERLINE`, also spelling dotted and dashed),
    /// so XOR-ing `UNDERLINE` alone could turn curly into dotted. Reverse mode 4
    /// therefore maps any underline to none and none to single.
    #[inline]
    fn reverse_cell_attrs(
        flags: super::CellFlags,
        toggle: super::CellFlags,
        toggle_underline: bool,
    ) -> super::CellFlags {
        let mut flags = super::CellFlags::from_bits(flags.bits() ^ toggle.bits());
        if toggle_underline {
            if flags.intersects(super::CellFlags::ALL_UNDERLINES) {
                flags = flags.difference(super::CellFlags::ALL_UNDERLINES);
            } else {
                flags = flags.union(super::CellFlags::UNDERLINE);
            }
        }
        flags
    }

    /// Change attributes in rectangular area (DECCARA - VT420+): over the
    /// rectangle (top, left)-(bottom, right), 0-indexed, `flags_to_set` are
    /// ORed in and `flags_to_clear` ANDed out.
    ///
    /// Records no selection damage: a selection still names the same text.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cols > 0
    pub fn change_attrs_rect(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        flags_to_set: super::CellFlags,
        flags_to_clear: super::CellFlags,
    ) {
        // Clear first, then set (so set takes priority)
        self.edit_attrs(top, left, bottom, right, false, |flags| {
            flags.difference(flags_to_clear).union(flags_to_set)
        });
    }

    /// Change attributes in a character stream (DECSACE stream mode): DECCARA
    /// over the stream from (top, left) to (bottom, right), 0-indexed.
    pub fn change_attrs_stream(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        flags_to_set: super::CellFlags,
        flags_to_clear: super::CellFlags,
    ) {
        self.edit_attrs(top, left, bottom, right, true, |flags| {
            flags.difference(flags_to_clear).union(flags_to_set)
        });
    }

    /// Reverse attributes in rectangular area (DECRARA - VT400+).
    ///
    /// Like [`Self::change_attrs_rect`], but each named attribute is reversed
    /// per cell: `toggle` is XOR-ed in, and `toggle_underline` reverses the
    /// underline family as a whole (see [`Self::reverse_cell_attrs`]). Records
    /// no selection damage.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cols > 0
    pub fn reverse_attrs_rect(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        toggle: super::CellFlags,
        toggle_underline: bool,
    ) {
        self.edit_attrs(top, left, bottom, right, false, |flags| {
            Self::reverse_cell_attrs(flags, toggle, toggle_underline)
        });
    }

    /// Reverse attributes in a character stream (DECSACE stream mode): the
    /// stream sibling of [`Self::reverse_attrs_rect`], 0-indexed.
    pub fn reverse_attrs_stream(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        toggle: super::CellFlags,
        toggle_underline: bool,
    ) {
        self.edit_attrs(top, left, bottom, right, true, |flags| {
            Self::reverse_cell_attrs(flags, toggle, toggle_underline)
        });
    }

    /// Copy rectangular area (DECCRA - VT420+): the source rectangle
    /// (src_top, src_left)-(src_bottom, src_right) to (dst_top, dst_left),
    /// 0-indexed, through a temporary buffer so overlapping rectangles copy
    /// correctly.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// REQUIRES: self.storage.cols > 0
    pub fn copy_rect(
        &mut self,
        src_top: u16,
        src_left: u16,
        src_bottom: u16,
        src_right: u16,
        dst_top: u16,
        dst_left: u16,
    ) {
        let max_row = self.storage.visible_rows.saturating_sub(1);
        let max_col = self.storage.cols.saturating_sub(1);
        let src_top = src_top.min(max_row);
        let src_bottom = src_bottom.min(max_row);
        let src_left = src_left.min(max_col);
        let src_right = src_right.min(max_col);

        if src_top > src_bottom || src_left > src_right {
            return;
        }

        let rect_height = src_bottom - src_top + 1;
        let rect_width = src_right - src_left + 1;

        let dst_top = dst_top.min(max_row);
        let dst_left = dst_left.min(max_col);

        // Copy source cells into a temporary buffer (handles overlap). If the
        // source rect cuts a wide pair, the orphaned half becomes a space.
        let mut buf: Vec<Vec<super::Cell>> = Vec::with_capacity(rect_height as usize);
        for row_offset in 0..rect_height {
            let src_row = src_top + row_offset;
            let mut row_cells = Vec::with_capacity(rect_width as usize);
            for col_offset in 0..rect_width {
                let src_col = src_left + col_offset;
                let cell = self
                    .cell(src_row, src_col)
                    .copied()
                    .unwrap_or(super::Cell::EMPTY);
                row_cells.push(cell);
            }
            // Left boundary: blank an orphaned continuation whose base sits at
            // src_left-1. Neighbour-aware check, not the raw bit: WIDE_CONTINUATION
            // shares bit 10 with PROTECTED, and DECCRA copies protected cells.
            if self
                .row(src_row)
                .is_some_and(|r| r.is_cell_wide_continuation(src_left))
            {
                row_cells[0] = super::Cell::EMPTY;
            }
            // Right boundary: last cell is WIDE without its continuation
            if let Some(last) = row_cells.last()
                && last.flags().contains(super::CellFlags::WIDE)
            {
                let last_idx = row_cells.len() - 1;
                row_cells[last_idx] = super::Cell::EMPTY;
            }
            buf.push(row_cells);
        }

        // Snapshot the source's HashMap extras and ring-resident truecolor and
        // complex codepoints before any destination write (the rects may
        // overlap); the copied cells carry only RGB/COMPLEX markers.
        let mut extras_buf: Vec<(u16, u16, super::CellExtra)> = Vec::new();
        let mut ring_buf: Vec<RingCapture> = Vec::new();
        for (row_cells, row_offset) in buf.iter().zip(0u16..) {
            let src_row = src_top + row_offset;
            for (cell, col_offset) in row_cells.iter().zip(0u16..) {
                let src_col = src_left + col_offset;
                let coord = crate::CellCoord {
                    row: src_row,
                    col: src_col,
                };
                if let Some(extra) = self.storage.extras.get(coord) {
                    extras_buf.push((row_offset, col_offset, extra.clone()));
                }
                let fg = if cell.fg_needs_overflow() {
                    self.storage.extras.fg_rgb_for(src_row, src_col)
                } else {
                    None
                };
                let bg = if cell.bg_needs_overflow() {
                    self.storage.extras.bg_rgb_for(src_row, src_col)
                } else {
                    None
                };
                // Only ring-resident complex codepoints need ring repopulation;
                // multi-codepoint Arc<str> clusters ride in `extras_buf` and
                // resolve through the HashMap once the dst ring is cleared.
                let cx = if cell.is_complex()
                    && self
                        .storage
                        .extras
                        .complex_char_arc_for(src_row, src_col)
                        .is_none()
                {
                    self.storage.extras.complex_codepoint_for(src_row, src_col)
                } else {
                    None
                };
                if fg.is_some() || bg.is_some() || cx.is_some() {
                    ring_buf.push((row_offset, col_offset, fg, bg, cx));
                }
            }
        }

        // Write to the destination, first blanking the halves of wide pairs its
        // edges bisect.
        for (row_cells, row_offset) in buf.iter().zip(0u16..) {
            let dst_row = dst_top.saturating_add(row_offset);
            if dst_row > max_row {
                break;
            }
            let actual_width = row_cells.len().min((max_col - dst_left + 1) as usize);
            if let Some(row) = self.row_mut(dst_row) {
                if actual_width > 0 {
                    let _ = row.fixup_wide_chars_in_range(dst_left, actual_width as u16);
                }
                for (&cell, col_offset) in row_cells.iter().zip(0u16..) {
                    let dst_col = dst_left.saturating_add(col_offset);
                    if dst_col > max_col {
                        break;
                    }
                    row.set(dst_col, cell);
                }
                // Row::set only grows len, so copied blanks overwriting the dst
                // row's tail would leave len stale-high (phantom trailing spaces
                // in row_text/search/scrollback). Recompute once per written row.
                row.recompute_len();
            }
            self.storage.mark_content_row(dst_row);
        }

        let dst_bottom_eff = dst_top
            .saturating_add(rect_height.saturating_sub(1))
            .min(max_row);
        // DECCRA replaces the destination cells (the source is only read), so
        // the destination rows are the selection damage.
        self.damage_selection_visible_rows(dst_top, dst_bottom_eff);
        let dst_right_eff = dst_left
            .saturating_add(rect_width.saturating_sub(1))
            .min(max_col);
        self.storage.extras.clear_rect(
            dst_top..dst_bottom_eff.saturating_add(1),
            dst_left..dst_right_eff.saturating_add(1),
        );
        for (row_offset, col_offset, extra) in extras_buf {
            let dst_row = dst_top.saturating_add(row_offset);
            let dst_col = dst_left.saturating_add(col_offset);
            if dst_row <= max_row && dst_col <= max_col {
                let coord = crate::CellCoord {
                    row: dst_row,
                    col: dst_col,
                };
                self.storage.extras.set(coord, extra);
            }
        }

        // clear_rect wiped both destination rings and the copied cells carry only
        // markers: repopulate them, inside the destination bounds as above.
        if !ring_buf.is_empty() {
            let visible_rows = self.storage.visible_rows;
            let cols = self.storage.cols;
            for (row_offset, col_offset, fg, bg, cx) in ring_buf {
                let dst_row = dst_top.saturating_add(row_offset);
                let dst_col = dst_left.saturating_add(col_offset);
                if dst_row > max_row || dst_col > max_col {
                    continue;
                }
                if fg.is_some() || bg.is_some() {
                    self.storage.extras.set_rgb_ring_range(
                        dst_row,
                        dst_col,
                        dst_col.saturating_add(1),
                        fg,
                        bg,
                        visible_rows,
                        cols,
                    );
                }
                if let Some(ch) = cx {
                    self.storage.extras.set_complex_char_ring(
                        dst_row,
                        dst_col,
                        ch,
                        visible_rows,
                        cols,
                    );
                }
            }
        }

        // Halves blanked by the source edges or by the destination fixup claim
        // nothing now.
        for dst_row in dst_top..=dst_bottom_eff {
            self.drop_unclaimed_extras(
                dst_row,
                dst_left.saturating_sub(1)..dst_right_eff.saturating_add(2),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Grid;
    use crate::cell_flags::CellFlags;
    use crate::grid::test_support::wrapped_pair;

    /// Helper: write ASCII text at the current cursor position.
    fn write_text(grid: &mut Grid, text: &str) {
        for c in text.chars() {
            grid.write_char(c);
        }
    }

    /// Helper: read the character at a visible grid position.
    fn char_at(grid: &Grid, row: u16, col: u16) -> char {
        grid.cell(row, col).map_or(' ', |c| c.char())
    }

    /// Helper: check if cell at position is empty (space with default colors, no flags).
    fn is_empty_at(grid: &Grid, row: u16, col: u16) -> bool {
        grid.cell(row, col).is_none_or(|c| c.is_empty())
    }

    /// Helper: set the PROTECTED flag on a cell.
    fn protect_cell(grid: &mut Grid, row: u16, col: u16) {
        if let Some(cell) = grid.cell_mut(row, col) {
            let flags = cell.flags().union(CellFlags::PROTECTED);
            cell.set_flags(flags);
        }
    }

    /// Helper: fill a row with distinct characters 'A'+col_offset.
    fn fill_row(grid: &mut Grid, row: u16, cols: u16) {
        grid.move_cursor_to(row, 0);
        for col in 0..cols {
            let ch = (b'A' + (col % 26) as u8) as char;
            grid.write_char(ch);
        }
    }

    /// Helper: fill entire grid with distinct characters.
    fn fill_grid(grid: &mut Grid, rows: u16, cols: u16) {
        for row in 0..rows {
            fill_row(grid, row, cols);
        }
    }

    /// Arm a real deferred wrap: print row `row` to its last column with
    /// deferred autowrap. The cursor ends parked on the last column with
    /// `pending_wrap` set.
    fn arm_wrap_on_row(grid: &mut Grid, row: u16, cols: u16) {
        grid.move_cursor_to(row, 0);
        for col in 0..cols {
            grid.write_char_wrap((b'A' + (col % 26) as u8) as char);
        }
        assert!(
            grid.pending_wrap(),
            "precondition: a filled row arms the wrap"
        );
        assert_eq!(
            grid.cursor_row(),
            row,
            "precondition: cursor stays on the row"
        );
        assert_eq!(
            grid.cursor_col(),
            cols - 1,
            "precondition: cursor parked on the last column"
        );
    }

    /// Helper: after an erase reset the wrap, the next printable must overwrite
    /// the parked last column of `row` — not wrap to the next row.
    fn assert_next_glyph_overwrites_last_col(grid: &mut Grid, row: u16, cols: u16) {
        grid.write_char_wrap('?');
        assert_eq!(
            char_at(grid, row, cols - 1),
            '?',
            "the glyph after the erase lands on the parked column (xterm)"
        );
        assert_eq!(grid.cursor_row(), row, "nothing wrapped");
        assert!(grid.pending_wrap(), "the new glyph re-arms the wrap");
    }

    #[derive(Clone, Copy, Debug)]
    enum Erase {
        El0,
        El1,
        El2,
        Ed0,
        Ed1,
        Ed2,
        Ed3,
        Sel0,
        Sel1,
        Sel2,
        Sed0,
        Sed1,
        Sed2,
    }

    impl Erase {
        const DISPLAY_AND_LINE: [Self; 12] = [
            Self::El0,
            Self::El1,
            Self::El2,
            Self::Ed0,
            Self::Ed1,
            Self::Ed2,
            Self::Sel0,
            Self::Sel1,
            Self::Sel2,
            Self::Sed0,
            Self::Sed1,
            Self::Sed2,
        ];

        fn apply(self, grid: &mut Grid) {
            match self {
                Self::El0 => grid.erase_to_end_of_line(),
                Self::El1 => grid.erase_from_start_of_line(),
                Self::El2 => grid.erase_line(),
                Self::Ed0 => grid.erase_to_end_of_screen(),
                Self::Ed1 => grid.erase_from_start_of_screen(),
                Self::Ed2 => grid.erase_screen(),
                Self::Ed3 => grid.erase_scrollback(),
                Self::Sel0 => grid.selective_erase_to_end_of_line(),
                Self::Sel1 => grid.selective_erase_from_start_of_line(),
                Self::Sel2 => grid.selective_erase_line(),
                Self::Sed0 => grid.selective_erase_to_end_of_screen(),
                Self::Sed1 => grid.selective_erase_from_start_of_screen(),
                Self::Sed2 => grid.selective_erase_screen(),
            }
        }

        /// Whether the erase reaches `(row, col)` with the cursor at
        /// `(cr, cc)`: the cursor cell is always inclusive.
        fn covers(self, (cr, cc): (u16, u16), (row, col): (u16, u16)) -> bool {
            match self {
                Self::El0 | Self::Sel0 => row == cr && col >= cc,
                Self::El1 | Self::Sel1 => row == cr && col <= cc,
                Self::El2 | Self::Sel2 => row == cr,
                Self::Ed0 | Self::Sed0 => row > cr || (row == cr && col >= cc),
                Self::Ed1 | Self::Sed1 => row < cr || (row == cr && col <= cc),
                Self::Ed2 | Self::Sed2 => true,
                Self::Ed3 => false,
            }
        }

        fn selective(self) -> bool {
            matches!(
                self,
                Self::Sel0 | Self::Sel1 | Self::Sel2 | Self::Sed0 | Self::Sed1 | Self::Sed2
            )
        }
    }

    /// Run `op` on a filled `size` grid with the cursor at `cursor` (parked
    /// there by a real deferred wrap when `armed`) and `protected` cells, then
    /// check every cell: erased exactly where the op reaches, minus protected
    /// cells for the selective ops. Returns the grid for the wrap checks.
    fn erase_and_check(
        op: Erase,
        size: (u16, u16),
        cursor: (u16, u16),
        armed: bool,
        protected: &[(u16, u16)],
    ) -> Grid {
        let (rows, cols) = size;
        let mut grid = Grid::new(rows, cols);
        fill_grid(&mut grid, rows, cols);
        if armed {
            assert_eq!(cursor.1, cols - 1, "a wrap parks on the last column");
            arm_wrap_on_row(&mut grid, cursor.0, cols);
        } else {
            grid.move_cursor_to(cursor.0, cursor.1);
        }
        for &(row, col) in protected {
            protect_cell(&mut grid, row, col);
        }
        op.apply(&mut grid);
        grid.assert_invariants();
        for row in 0..rows {
            for col in 0..cols {
                let erased = op.covers(cursor, (row, col))
                    && !(op.selective() && protected.contains(&(row, col)));
                let want = if erased {
                    ' '
                } else {
                    (b'A' + (col % 26) as u8) as char
                };
                assert_eq!(
                    char_at(&grid, row, col),
                    want,
                    "{op:?} at {cursor:?} (armed {armed}, protected {protected:?}): ({row}, {col})"
                );
            }
        }
        grid
    }

    /// EVERY EL/ED/DECSEL/DECSED ERASES EXACTLY ITS SPAN: from the cursor
    /// (inclusive) to the end, from the start, or the whole line or screen,
    /// leaving every other row alone, and the selective forms skip DECSCA
    /// protected cells — at every cursor corner and on a 1x1 grid.
    #[test]
    fn every_erase_clears_exactly_its_span() {
        const PROTECTED: &[(u16, u16)] = &[(0, 1), (1, 3), (1, 6), (2, 0), (2, 8), (3, 4), (3, 9)];
        for op in Erase::DISPLAY_AND_LINE {
            let protected = if op.selective() { PROTECTED } else { &[] };
            for cursor in [(0, 0), (1, 5), (2, 4), (3, 9)] {
                erase_and_check(op, (4, 10), cursor, false, protected);
            }
            erase_and_check(op, (1, 1), (0, 0), false, &[]);
            erase_and_check(op, (1, 10), (0, 5), false, &[]);
        }
    }

    /// THE DEFERRED WRAP FOLLOWS XTERM'S util.c. Every EL/ED resets it (ED 3
    /// touches only history and keeps it); DECSEL 0 / DECSED 0 away from the
    /// origin end in `ClearRight`'s unconditional `ResetWrap`, even over a
    /// protected parked glyph; DECSEL 1/2 and DECSED 1/2 reach `ResetWrap`
    /// through `ClearInLine2` only when their span holds an unprotected cell;
    /// and at the origin (DECSED 0) or bottom-right (DECSED 1) xterm reduces to
    /// the DECSED 2 loop. After a reset the next glyph overwrites the parked
    /// column instead of wrapping (Ghostty's "eraseLine resets pending wrap").
    #[test]
    fn erases_reset_the_pending_wrap_as_xterm_does() {
        const ROW_0: &[(u16, u16)] = &[(0, 0), (0, 1), (0, 2), (0, 3), (0, 4)];
        const ROWS_0_1: &[(u16, u16)] = &[
            (0, 0),
            (0, 1),
            (0, 2),
            (0, 3),
            (0, 4),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
            (1, 4),
        ];
        let screen = |rows: u16, cols: u16| -> Vec<(u16, u16)> {
            (0..rows)
                .flat_map(|r| (0..cols).map(move |c| (r, c)))
                .collect()
        };
        use Erase::*;
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let cases: Vec<(Erase, (u16, u16), u16, Vec<(u16, u16)>, bool)> = vec![
            (El0, (4, 5), 0, vec![], false),
            (El1, (4, 5), 0, vec![], false),
            (El2, (4, 5), 0, vec![], false),
            (Ed0, (4, 5), 0, vec![], false),
            (Ed0, (3, 1), 0, vec![], false),
            (Ed1, (4, 5), 1, vec![], false),
            (Ed1, (3, 5), 2, vec![], false),
            (Ed2, (4, 5), 1, vec![], false),
            (Ed3, (4, 5), 1, vec![], true),
            (Sel0, (4, 5), 0, vec![], false),
            (Sel0, (4, 5), 0, vec![(0, 4)], false),
            (Sel1, (4, 5), 0, vec![], false),
            (Sel1, (4, 5), 0, ROW_0.to_vec(), true),
            (Sel1, (4, 5), 0, vec![(0, 0), (0, 1), (0, 3), (0, 4)], false),
            (Sel2, (4, 5), 0, vec![(0, 4)], false),
            (Sel2, (4, 5), 0, ROW_0.to_vec(), true),
            (Sed0, (4, 5), 1, vec![], false),
            (Sed0, (4, 5), 1, vec![(1, 4)], false),
            (Sed0, (3, 1), 0, screen(3, 1), true),
            (Sed0, (3, 1), 0, vec![(0, 0), (1, 0)], false),
            (Sed1, (4, 5), 1, vec![], false),
            (Sed1, (4, 5), 1, ROWS_0_1.to_vec(), true),
            (Sed1, (2, 3), 1, screen(2, 3), true),
            (Sed2, (4, 5), 1, vec![(1, 4)], false),
            (Sed2, (3, 4), 1, screen(3, 4), true),
        ];
        for (op, size, row, protected, keeps_wrap) in cases {
            let cursor = (row, size.1 - 1);
            let mut grid = erase_and_check(op, size, cursor, true, &protected);
            assert_eq!(
                grid.pending_wrap(),
                keeps_wrap,
                "{op:?} on {size:?} row {row}, protected {protected:?}"
            );
            if !keeps_wrap {
                assert_next_glyph_overwrites_last_col(&mut grid, row, size.1);
            }
        }
    }

    /// DECALN fills the whole screen with default-attribute 'E's over whatever
    /// was there, homes the cursor, resets the scroll region, clears the
    /// pending wrap and returns every row to single width.
    #[test]
    fn decaln_fills_the_screen_and_resets_the_cursor_state() {
        for (rows, cols) in [(4, 10), (1, 1)] {
            let mut grid = Grid::new(rows, cols);
            fill_grid(&mut grid, rows, cols);
            if rows > 2 {
                grid.set_scroll_region(1, rows - 2);
            }
            if let Some(r) = grid.row_mut(0) {
                r.set_line_size(crate::LineSize::DoubleWidth);
            }
            grid.mark_has_double_width();
            grid.move_cursor_to(rows - 1, cols - 1);
            grid.set_pending_wrap(true);

            grid.screen_alignment_pattern();

            for row in 0..rows {
                for col in 0..cols {
                    let cell = grid.cell(row, col).expect("cell");
                    assert_eq!(cell.char(), 'E', "({row}, {col})");
                    assert!(cell.flags().is_empty(), "({row}, {col})");
                }
            }
            assert_eq!((grid.cursor_row(), grid.cursor_col()), (0, 0));
            let region = grid.scroll_region();
            assert_eq!((region.top, region.bottom), (0, rows - 1));
            assert!(!grid.pending_wrap());
            assert!(!grid.storage.any_double_width);
        }
    }

    /// DECERA, DECSERA, DECFRA and DECCARA touch exactly their rectangle,
    /// clamped to the grid; an inverted rectangle is a no-op; DECSERA alone
    /// skips protected cells; and none of them (nor DECCRA) consumes the
    /// pending wrap.
    #[test]
    fn rect_ops_touch_exactly_their_rectangle() {
        type Rect = (u16, u16, u16, u16);
        const PROTECTED: &[(u16, u16)] = &[(1, 3), (2, 4)];
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let ops: [(&str, fn(&mut Grid, Rect)); 5] = [
            ("DECERA", |g, (t, l, b, r)| g.erase_rect(t, l, b, r)),
            ("DECSERA", |g, (t, l, b, r)| {
                g.selective_erase_rect(t, l, b, r)
            }),
            ("DECFRA", |g, (t, l, b, r)| {
                g.fill_rect(crate::Cell::from_ascii_fast(b'X'), t, l, b, r, None, None);
            }),
            ("DECCARA set", |g, (t, l, b, r)| {
                g.change_attrs_rect(t, l, b, r, CellFlags::BOLD, CellFlags::empty());
            }),
            ("DECCARA clear", |g, (t, l, b, r)| {
                g.change_attrs_rect(0, 0, 3, 9, CellFlags::BOLD, CellFlags::empty());
                g.change_attrs_rect(t, l, b, r, CellFlags::empty(), CellFlags::BOLD);
            }),
        ];
        for rect in [
            (1, 2, 2, 5),
            (2, 3, 2, 3),
            (2, 8, 100, 100),
            (0, 0, 3, 9),
            (3, 5, 1, 2),
        ] {
            let (top, left, bottom, right) = rect;
            let inside =
                |r: u16, c: u16| (top..=bottom).contains(&r) && (left..=right).contains(&c);
            for (name, op) in ops {
                let mut grid = Grid::new(4, 10);
                fill_grid(&mut grid, 4, 10);
                for &(row, col) in PROTECTED {
                    protect_cell(&mut grid, row, col);
                }
                grid.set_pending_wrap(true);
                op(&mut grid, rect);
                assert!(grid.pending_wrap(), "{name} {rect:?} consumed the wrap");
                for row in 0..4 {
                    for col in 0..10 {
                        let original = (b'A' + col as u8) as char;
                        let hit = inside(row, col);
                        let at = format!("{name} {rect:?} at ({row}, {col})");
                        let cell = grid.cell(row, col).expect("cell");
                        match name {
                            "DECERA" => {
                                assert_eq!(cell.char(), if hit { ' ' } else { original }, "{at}");
                            }
                            "DECSERA" => {
                                let erased = hit && !PROTECTED.contains(&(row, col));
                                assert_eq!(
                                    cell.char(),
                                    if erased { ' ' } else { original },
                                    "{at}"
                                );
                            }
                            "DECFRA" => {
                                assert_eq!(cell.char(), if hit { 'X' } else { original }, "{at}");
                            }
                            "DECCARA set" => {
                                assert_eq!(cell.flags().contains(CellFlags::BOLD), hit, "{at}");
                            }
                            _ => {
                                assert_eq!(cell.flags().contains(CellFlags::BOLD), !hit, "{at}");
                            }
                        }
                    }
                }
            }
        }

        let mut grid = Grid::new(4, 10);
        fill_grid(&mut grid, 4, 10);
        grid.set_pending_wrap(true);
        grid.copy_rect(3, 5, 1, 2, 2, 0);
        assert_eq!(
            char_at(&grid, 2, 0),
            'A',
            "an inverted DECCRA source copies nothing"
        );
        grid.copy_rect(0, 0, 0, 4, 1, 0);
        assert!(grid.pending_wrap(), "DECCRA consumed the wrap");
    }

    #[test]
    fn test_el0_wide_glyph_parked_on_continuation_clears_both_halves_and_wrap() {
        let mut grid = Grid::new(4, 5);
        write_text(&mut grid, "ABC");
        // The production wide write parks the cursor on the continuation cell
        // (col 4) with the wrap armed.
        assert!(grid.write_wide_autowrap_fast(
            '中',
            crate::PackedColors::DEFAULT,
            CellFlags::empty()
        ));
        assert!(
            grid.pending_wrap(),
            "precondition: wide glyph arms the wrap"
        );
        assert_eq!(
            grid.cursor_col(),
            4,
            "precondition: parked on the continuation"
        );
        assert!(grid.cell(0, 3).unwrap().flags().contains(CellFlags::WIDE));

        grid.erase_to_end_of_line();

        for col in 3..5 {
            let cell = grid.cell(0, col).unwrap();
            assert_eq!(cell.char(), ' ', "col {col} of the wide glyph is blank");
            assert!(
                !cell.flags().contains(CellFlags::WIDE),
                "col {col} keeps no WIDE half"
            );
        }
        assert_eq!(char_at(&grid, 0, 2), 'C', "cells left of the glyph survive");
        assert!(!grid.pending_wrap(), "EL 0 resets the deferred wrap");
    }

    #[test]
    fn test_el0_breaks_the_wrap_into_the_row_below_as_xterm_clear_right_does() {
        // xterm util.c `ClearRight` ends with `LineClrWrapped(ld)`: "with the
        // right part cleared, we can't be wrapping". The flag lives on the
        // continuation here, so EL 0 on row 0 clears row 1's, whatever the
        // column, and leaves row 1's cells alone.
        for col in [0u16, 2, 4] {
            let mut grid = wrapped_pair();
            grid.move_cursor_to(0, col);
            grid.erase_to_end_of_line();
            assert!(
                !grid.row(1).unwrap().is_wrapped(),
                "EL 0 at col {col} breaks the link"
            );
            assert_eq!(char_at(&grid, 1, 0), 'F', "row 1's cells survive");
            assert_eq!(char_at(&grid, 1, 1), 'G');
        }
        // DECSEL 0 is the same `ClearRight`.
        let mut grid = wrapped_pair();
        grid.move_cursor_to(0, 3);
        grid.selective_erase_to_end_of_line();
        assert!(!grid.row(1).unwrap().is_wrapped(), "DECSEL 0 too");
    }

    #[test]
    fn test_el1_el2_and_el0_on_the_continuation_keep_the_link() {
        // `ClearLeft` and `ClearLine` never call `LineClrWrapped`; and EL 0 on
        // the continuation itself clears its right part, not the wrap that
        // brought the text into it.
        let mut grid = wrapped_pair();
        grid.move_cursor_to(0, 2);
        grid.erase_from_start_of_line();
        assert!(grid.row(1).unwrap().is_wrapped(), "EL 1 keeps the link");
        let mut grid = wrapped_pair();
        grid.move_cursor_to(0, 2);
        grid.erase_line();
        assert!(grid.row(1).unwrap().is_wrapped(), "EL 2 keeps the link");
        let mut grid = wrapped_pair();
        grid.move_cursor_to(1, 0);
        grid.erase_to_end_of_line();
        assert!(
            grid.row(1).unwrap().is_wrapped(),
            "EL 0 on row 1 leaves row 1's own flag"
        );
        // The bottom row has no row below to unlink.
        grid.move_cursor_to(3, 0);
        grid.erase_to_end_of_line();
    }

    #[test]
    fn test_ed2_resets_display_offset() {
        let mut grid = Grid::new(4, 10);
        grid.scroll_up(3);
        grid.scroll_display(2);
        assert_eq!(
            grid.display_offset(),
            2,
            "precondition: viewport in history"
        );
        grid.erase_screen();
        assert_eq!(grid.display_offset(), 0);
    }

    /// DECSEL/DECSED erase with the BCE background, as EL/ED do: xterm's
    /// protected erases reach the same `ClearCells` (screen.c), which writes
    /// the current colour pair over each unprotected segment, and Ghostty's
    /// `clearUnprotectedCells` clears each run with its blank cell. A protected
    /// cell keeps its glyph and its colours. Found by the astream-term
    /// differential oracle (aterm-conformance `tests/vt_oracle`), which had the
    /// selective erases leave a default-coloured hole where xterm paints.
    #[test]
    fn selective_erases_fill_with_the_bce_background() {
        use crate::{Cell, PackedColors};
        let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
        // (what, cursor, the erase, cells it must blank)
        type Case = (
            &'static str,
            (u16, u16),
            fn(&mut Grid),
            &'static [(u16, u16)],
        );
        let cases: [Case; 5] = [
            (
                "DECSEL 0",
                (1, 2),
                Grid::selective_erase_to_end_of_line,
                &[(1, 2), (1, 4)],
            ),
            (
                "DECSEL 1",
                (1, 4),
                Grid::selective_erase_from_start_of_line,
                &[(1, 0), (1, 2), (1, 4)],
            ),
            (
                "DECSEL 2",
                (1, 0),
                Grid::selective_erase_line,
                &[(1, 0), (1, 4)],
            ),
            (
                "DECSED 0",
                (1, 2),
                Grid::selective_erase_to_end_of_screen,
                &[(1, 2), (1, 4), (2, 0)],
            ),
            (
                "DECSED 2",
                (0, 0),
                Grid::selective_erase_screen,
                &[(0, 0), (1, 4), (2, 4)],
            ),
        ];
        for (what, (row, col), erase, erased) in cases {
            let mut grid = Grid::new(3, 5);
            fill_grid(&mut grid, 3, 5);
            protect_cell(&mut grid, 1, 3);
            grid.set_cursor_template(fill, None);
            grid.move_cursor_to(row, col);
            erase(&mut grid);
            for &(r, c) in erased {
                let cell = grid.cell(r, c).unwrap();
                assert_eq!(cell.char(), ' ', "{what}: ({r},{c}) erased");
                assert_eq!(
                    cell.colors(),
                    fill.colors(),
                    "{what}: ({r},{c}) takes the BCE blank"
                );
            }
            let kept = grid.cell(1, 3).unwrap();
            assert_ne!(
                kept.char(),
                ' ',
                "{what}: the protected cell keeps its glyph"
            );
            assert_eq!(
                kept.colors(),
                PackedColors::DEFAULT,
                "{what}: and its colours"
            );
        }
    }

    /// A BCE blank is content: a selective erase that paints past the old end
    /// of a short row grows the row's length to the last coloured cell, so the
    /// renderer draws the run (the non-selective erases' `len` rule).
    #[test]
    fn a_selective_bce_erase_counts_as_row_content() {
        use crate::{Cell, PackedColors};
        let mut grid = Grid::new(2, 8);
        grid.move_cursor_to(0, 0);
        grid.write_char('a');
        grid.set_cursor_template(Cell::bce_blank(PackedColors::with_indexed_bg(2)), None);
        grid.move_cursor_to(0, 3);
        grid.selective_erase_to_end_of_line();
        assert_eq!(
            grid.row(0).unwrap().len(),
            8,
            "coloured through the last column"
        );
        grid.set_cursor_template(Cell::EMPTY, None);
        grid.move_cursor_to(0, 1);
        grid.selective_erase_to_end_of_line();
        assert_eq!(
            grid.row(0).unwrap().len(),
            1,
            "a default erase shrinks it back to 'a'"
        );
    }

    /// The truecolor half: the cleared cells carry only the RGB marker, so the
    /// ring must hold their bytes; a protected cell's stay untouched.
    #[test]
    fn a_selective_erase_writes_the_truecolor_ring_for_the_cleared_cells_only() {
        use crate::{Cell, PackedColor};
        let mut grid = Grid::new(2, 5);
        fill_grid(&mut grid, 2, 5);
        protect_cell(&mut grid, 0, 2);
        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(10, 20, 30));
        grid.set_cursor_template(fill, Some([10, 20, 30]));
        grid.move_cursor_to(0, 0);
        grid.selective_erase_line();
        for col in [0, 1, 3, 4] {
            assert_eq!(grid.bg_rgb_at(0, col), Some([10, 20, 30]), "col {col}");
        }
        assert_eq!(
            grid.bg_rgb_at(0, 2),
            None,
            "the protected cell has no truecolor"
        );
        assert_ne!(char_at(&grid, 0, 2), ' ');
    }

    #[test]
    fn test_selective_erase_rect_scrubs_combining_marks() {
        // DECSERA wipes an unprotected cell to a space, but `set_char` keeps
        // HAS_EXTRAS and the cell's extra so out-of-line SGR colors survive. A
        // stale combining mark must not: the wiped space would carry a stray
        // accent in copy, selection, search and render.
        let mut grid = Grid::new(2, 10);

        // Column 4: base 'e' plus a combining acute (U+0301) in the extra.
        // The cell stays plain 'e' (not COMPLEX) with HAS_EXTRAS set.
        grid.move_cursor_to(0, 4);
        grid.write_char('e');
        if let Some(r) = grid.row_mut(0) {
            r.get_mut(4).unwrap().set_has_extras(true);
        }
        grid.extras_mut()
            .get_or_create(crate::CellCoord::new(0, 4))
            .add_combining('\u{0301}');

        // Precondition: the row reads "e" + combining acute.
        assert!(
            grid.row_text(0).unwrap().contains("e\u{0301}"),
            "precondition: combining mark present before DECSERA"
        );

        grid.selective_erase_rect(0, 4, 0, 4);

        // Character wiped to a space, and the accent is gone from reads.
        assert_eq!(char_at(&grid, 0, 4), ' ', "cell wiped to a space");
        let text = grid.row_text(0).unwrap();
        assert!(
            !text.contains('\u{0301}'),
            "combining acute must not survive DECSERA, got: {text:?}"
        );
        // The now-empty extra entry was dropped and HAS_EXTRAS cleared, keeping
        // the HAS_EXTRAS ⇔ entry-exists invariant.
        assert!(
            !grid.cell(0, 4).unwrap().has_extras(),
            "HAS_EXTRAS must be cleared once the extra is empty"
        );
        assert!(
            grid.extras().get(crate::CellCoord::new(0, 4)).is_none(),
            "empty extra entry must be dropped"
        );
    }

    #[test]
    fn test_selective_erase_rect_preserves_truecolor_extra() {
        // DECSERA must keep SGR colors. When truecolor lives in the extra
        // alongside a combining mark, the wipe scrubs only the mark and keeps
        // the entry (and HAS_EXTRAS) so the bg truecolor survives.
        let mut grid = Grid::new(2, 10);

        grid.move_cursor_to(0, 4);
        grid.write_char('e');
        if let Some(r) = grid.row_mut(0) {
            r.get_mut(4).unwrap().set_has_extras(true);
        }
        {
            let extra = grid.extras_mut().get_or_create(crate::CellCoord::new(0, 4));
            extra.add_combining('\u{0301}');
            extra.set_bg_rgb(Some([10, 20, 30]));
        }

        grid.selective_erase_rect(0, 4, 0, 4);

        assert_eq!(char_at(&grid, 0, 4), ' ', "cell wiped to a space");
        assert!(
            !grid.row_text(0).unwrap().contains('\u{0301}'),
            "combining acute must be scrubbed"
        );
        // Truecolor bg survives: entry kept and HAS_EXTRAS still set.
        assert!(
            grid.cell(0, 4).unwrap().has_extras(),
            "HAS_EXTRAS must remain set while color-bearing data survives"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 4),
            Some([10, 20, 30]),
            "bg truecolor must survive DECSERA"
        );
    }

    #[test]
    fn test_selective_erase_rect_wide_cell_leaves_no_orphan_continuation() {
        // DECSERA over the continuation column of an unprotected wide glyph must
        // not orphan the spacer: after the left-boundary fixup wipes the lead,
        // the continuation (bit 10, shared with PROTECTED) must not be misread
        // as a protected cell and skipped.
        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(
            grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: col 0 is the wide lead"
        );
        assert!(
            grid.cell(0, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: col 1 is the continuation"
        );

        // Erase only the continuation column (left == 1 fires the left-boundary fixup).
        grid.selective_erase_rect(0, 1, 0, 1);

        assert_eq!(char_at(&grid, 0, 0), ' ', "lead wiped to a space");
        assert_eq!(
            char_at(&grid, 0, 1),
            ' ',
            "continuation wiped, not orphaned"
        );
        assert!(
            !grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "lead WIDE flag cleared (no stale wide space)"
        );
        assert!(
            !grid
                .cell(0, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "continuation flag cleared (no orphaned spacer)"
        );
    }

    #[test]
    fn copy_rect_preserves_protected_cell_at_source_left_edge() {
        // DECCRA copies cells verbatim with no protection semantics: a protected
        // non-wide cell at src_left (bit 10 shared with WIDE_CONTINUATION) must
        // not be mistaken for an orphaned continuation and blanked.
        let mut grid = Grid::new(3, 10);
        grid.move_cursor_to(0, 0);
        for ch in ['A', 'B', 'C'] {
            grid.write_char(ch);
        }
        protect_cell(&mut grid, 0, 0); // 'A' at the source-rect left edge is protected
        assert!(
            grid.cell(0, 0)
                .unwrap()
                .flags()
                .contains(CellFlags::PROTECTED),
            "precondition: (0,0) carries the PROTECTED (bit-10) flag"
        );
        assert!(
            !grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: it is a NON-wide protected cell (no real continuation)"
        );

        // Copy row 0, cols 0..=2 to row 2, col 0.
        grid.copy_rect(0, 0, 0, 2, 2, 0);

        assert_eq!(
            char_at(&grid, 2, 0),
            'A',
            "protected left-edge cell must be copied verbatim, not blanked"
        );
        assert_eq!(char_at(&grid, 2, 1), 'B');
        assert_eq!(char_at(&grid, 2, 2), 'C');
    }

    #[test]
    fn copy_rect_shrinks_dst_len_when_copied_blanks_empty_the_tail() {
        // copy_rect writes via Row::set (grow-only len). Copying blank source
        // cells over the destination's tail must still shrink len, or emptied
        // cells surface as phantom trailing spaces.
        let mut grid = Grid::new(3, 10);
        grid.move_cursor_to(0, 0);
        for ch in ['h', 'e', 'l', 'l', 'o'] {
            grid.write_char(ch);
        }
        assert_eq!(
            grid.row_len(0),
            Some(5),
            "precondition: row 0 is \"hello\", len 5"
        );

        // Copy blank row-2 cells (cols 0..=1) over row 0 cols 3..=4, blanking the
        // 'l','o' tail. Real content is now just "hel".
        grid.copy_rect(2, 0, 2, 1, 0, 3);

        assert_eq!(char_at(&grid, 0, 0), 'h');
        assert_eq!(char_at(&grid, 0, 2), 'l');
        assert!(
            grid.cell(0, 3).unwrap().is_empty(),
            "blanked tail cell must be empty"
        );
        assert_eq!(
            grid.row_len(0),
            Some(3),
            "len must shrink to 3, not stay stale-high at 5"
        );
        assert_eq!(
            grid.row_text(0).as_deref(),
            Some("hel"),
            "no phantom trailing spaces"
        );
    }

    #[test]
    fn change_attrs_rect_maintains_len_both_directions() {
        // DECCARA/DECRARA change flags via row.get_mut with no len update:
        // setting an attribute on blank trailing cells must grow len, and
        // clearing the last attribute must shrink it.
        let mut grid = Grid::new(3, 10);
        assert_eq!(grid.row_len(0), Some(0), "precondition: row 0 blank");

        // DECCARA: set BOLD on cols 5..=9 of an otherwise blank row.
        grid.change_attrs_rect(0, 5, 0, 9, CellFlags::BOLD, CellFlags::empty());
        assert_eq!(
            grid.row_len(0),
            Some(10),
            "styled (non-empty) tail cells must be counted in len (stale-low fixed)"
        );

        // Clear BOLD again → cells become empty → len shrinks back.
        grid.change_attrs_rect(0, 5, 0, 9, CellFlags::empty(), CellFlags::BOLD);
        assert_eq!(
            grid.row_len(0),
            Some(0),
            "len must shrink when the tail becomes empty again (stale-high fixed)"
        );
    }

    #[test]
    fn reverse_attrs_rect_maintains_len_both_directions() {
        // The same len obligation on the DECRARA path: reversing an attribute
        // onto blank trailing cells and back must move len both ways (both paths
        // walk through `edit_attrs`).
        let mut grid = Grid::new(3, 10);
        assert_eq!(grid.row_len(0), Some(0), "precondition: row 0 blank");

        grid.reverse_attrs_rect(0, 5, 0, 9, CellFlags::BOLD, false);
        assert_eq!(
            grid.row_len(0),
            Some(10),
            "reversing BOLD onto blank cells makes them non-empty"
        );

        grid.reverse_attrs_rect(0, 5, 0, 9, CellFlags::BOLD, false);
        assert_eq!(
            grid.row_len(0),
            Some(0),
            "reversing it back empties the tail again"
        );
    }

    #[test]
    fn reverse_attrs_of_underline_family_never_yields_an_unnamed_style() {
        // Reverse mode 4 maps any underline to none and none to single (see
        // `reverse_cell_attrs`): an XOR of UNDERLINE would turn curly into dotted.
        for start in [
            CellFlags::UNDERLINE,
            CellFlags::DOUBLE_UNDERLINE,
            CellFlags::CURLY_UNDERLINE,
            CellFlags::DOTTED_UNDERLINE,
            CellFlags::DASHED_UNDERLINE,
        ] {
            let got = Grid::reverse_cell_attrs(start, CellFlags::empty(), true);
            assert_eq!(
                got.bits(),
                0,
                "every underline style reverses to no underline, got {got:?} from {start:?}"
            );
        }
        let got = Grid::reverse_cell_attrs(CellFlags::empty(), CellFlags::empty(), true);
        assert_eq!(
            got,
            CellFlags::UNDERLINE,
            "no underline reverses to the single underline SGR 4 itself sets"
        );
        // Other attributes on the cell ride through untouched.
        let got = Grid::reverse_cell_attrs(
            CellFlags::BOLD.union(CellFlags::CURLY_UNDERLINE),
            CellFlags::empty(),
            true,
        );
        assert_eq!(
            got,
            CellFlags::BOLD,
            "reversing the underline family leaves unrelated attributes alone"
        );
    }

    #[test]
    fn copy_rect_still_blanks_genuine_orphaned_wide_continuation_at_left_edge() {
        // A wide glyph whose base sits at src_left-1 (outside the copied rect)
        // leaves only its continuation at src_left, which must still be blanked
        // so no spacer without a lead is copied.
        let mut grid = Grid::new(3, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        grid.move_cursor_to(0, 2);
        grid.write_char('X');
        assert!(
            grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: (0,0) is the WIDE base"
        );
        assert!(
            grid.cell(0, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: (0,1) is the continuation"
        );

        // Copy cols 1..=2: col 1's WIDE base (col 0) is excluded ⇒ it is orphaned.
        grid.copy_rect(0, 1, 0, 2, 2, 0);

        assert!(
            is_empty_at(&grid, 2, 0),
            "genuine orphaned continuation at src_left must still be blanked"
        );
        assert_eq!(
            char_at(&grid, 2, 1),
            'X',
            "the in-range cell copies normally"
        );
    }

    #[test]
    fn erase_rect_preserves_protected_cell_left_of_the_region() {
        // Row::clear_range_with's left-boundary wide fixup (behind DECERA,
        // DECFRA, EL and ED) must not take a protected cell (bit 10 aliases
        // WIDE_CONTINUATION) at the left column for a continuation and blank
        // the cell outside the rectangle.
        let mut grid = Grid::new(3, 10);
        fill_row(&mut grid, 0, 10); // 'A','B','C',… — all narrow, no wide pairs
        protect_cell(&mut grid, 0, 1); // protect the region's left-edge cell (col 1)

        // DECERA over cols 1..=5 on row 0; col 0 is outside the region and survives.
        grid.erase_rect(0, 1, 0, 5);

        assert_eq!(
            char_at(&grid, 0, 0),
            'A',
            "the cell LEFT of the region must not be corrupted by the boundary fixup"
        );
        for col in 1..=5 {
            assert_eq!(
                char_at(&grid, 0, col),
                ' ',
                "col {col} inside the region is erased"
            );
        }
        assert_eq!(
            char_at(&grid, 0, 6),
            'G',
            "the cell right of the region survives"
        );
    }

    // Wide-orphan colour: the BCE erases vs the rectangle ops

    /// EL 0 that bisects a wide pair gives the orphaned half the BCE
    /// background, not default colours.
    ///
    /// xterm `util.c` `ClearRight` runs its wide fixup through the same
    /// `ClearInLine2` -> `ClearCells` (`screen.c`) as the erased span, and
    /// `ClearCells` writes `' '` plus `ld->color[col] = xtermColorPair(xw)`.
    #[test]
    fn erase_to_end_of_line_gives_the_wide_orphan_the_bce_background() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(
            grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: col 0 is the wide lead"
        );

        // SGR 44 (blue background) — the BCE template an EL then erases with.
        let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
        grid.set_cursor_template(fill, None);

        // EL 0 from col 1: the continuation goes, orphaning the head at col 0.
        grid.move_cursor_to(0, 1);
        grid.erase_to_end_of_line();

        let orphan = grid.cell(0, 0).unwrap();
        assert_eq!(orphan.char(), ' ', "the orphaned lead is blanked");
        assert_eq!(
            orphan.colors(),
            fill.colors(),
            "the orphan must take the BCE background like the erased span \
             (xterm ClearRight -> ClearCells)"
        );
        assert!(
            !is_empty_at(&grid, 0, 0),
            "a default-coloured orphan renders as a hole in the BCE run"
        );
    }

    /// The truecolor half: a BCE cell holds only an RGB marker, so the ring
    /// must extend to the orphan that now carries the marker.
    #[test]
    fn erase_to_end_of_line_extends_the_bce_truecolor_ring_to_the_wide_orphan() {
        use crate::{Cell, PackedColor};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );

        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(10, 20, 30));
        grid.set_cursor_template(fill, Some([10, 20, 30]));

        grid.move_cursor_to(0, 1);
        grid.erase_to_end_of_line();

        assert_eq!(
            grid.bg_rgb_at(0, 1),
            Some([10, 20, 30]),
            "precondition: the erased span resolves its truecolor background"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 0),
            Some([10, 20, 30]),
            "the orphan column must resolve the same truecolor background — the \
             cell carries only the RGB marker, so the ring has to reach it"
        );
    }

    /// EL 1 hits the right boundary instead: the continuation just past the
    /// erase is the orphan.
    #[test]
    fn erase_from_start_of_line_gives_the_wide_orphan_the_bce_background() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 4);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(
            grid.cell(0, 5)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: col 5 is the continuation"
        );

        let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
        grid.set_cursor_template(fill, None);

        // EL 1 erases [0, 5): the lead at col 4 goes, orphaning col 5.
        grid.move_cursor_to(0, 4);
        grid.erase_from_start_of_line();

        let orphan = grid.cell(0, 5).unwrap();
        assert_eq!(orphan.char(), ' ', "the orphaned continuation is blanked");
        assert_eq!(
            orphan.colors(),
            fill.colors(),
            "the right-hand orphan must take the BCE background too"
        );
    }

    /// The truecolor half of EL 1: the orphaned continuation sits one column
    /// past the erased range, so `fill_bce_rgb_range` runs to `span.end`.
    #[test]
    fn erase_from_start_of_line_extends_the_bce_truecolor_ring_to_the_wide_orphan() {
        use crate::{Cell, PackedColor};

        let (rows, cols) = (2u16, 10u16);
        let mut grid = Grid::new(rows, cols);

        // A wide glyph at cols 4-5 on its own truecolor background.
        let dead_bg = [7u8, 8, 9];
        grid.move_cursor_to(0, 4);
        grid.write_wide_char_styled(
            '中',
            PackedColor::DEFAULT_FG,
            PackedColor::rgb(dead_bg[0], dead_bg[1], dead_bg[2]),
            CellFlags::empty(),
        );
        grid.extras_mut()
            .set_rgb_ring_range(0, 4, 6, None, Some(dead_bg), rows, cols);
        assert_eq!(
            grid.bg_rgb_at(0, 5),
            Some(dead_bg),
            "precondition: the continuation carries the glyph's own bytes"
        );

        let bce_bg = [10u8, 20, 30];
        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(bce_bg[0], bce_bg[1], bce_bg[2]));
        grid.set_cursor_template(fill, Some(bce_bg));

        // EL 1 erases [0, 5): the lead at col 4 goes, orphaning col 5.
        grid.move_cursor_to(0, 4);
        grid.erase_from_start_of_line();

        assert_eq!(
            grid.bg_rgb_at(0, 4),
            Some(bce_bg),
            "precondition: the erased span resolves its truecolor background"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 5),
            Some(bce_bg),
            "the right-hand orphan must resolve the BCE truecolor background — \
             the cell carries only the RGB marker, so the ring has to reach one \
             column PAST the erased range or the orphan keeps the dead glyph's \
             bytes"
        );
    }

    /// EL 2 over a DECLRMM-margin row: the orphan just past the right margin
    /// takes the BCE blank, and the ring follows it.
    #[test]
    fn erase_line_within_margins_gives_the_wide_orphan_the_bce_background_and_ring() {
        use crate::{Cell, PackedColor};

        let (rows, cols) = (2u16, 12u16);
        let mut grid = Grid::new(rows, cols);

        // A wide glyph straddling the right margin: cols 6-7, margin at col 6.
        let dead_bg = [7u8, 8, 9];
        grid.move_cursor_to(0, 6);
        grid.write_wide_char_styled(
            '中',
            PackedColor::DEFAULT_FG,
            PackedColor::rgb(dead_bg[0], dead_bg[1], dead_bg[2]),
            CellFlags::empty(),
        );
        grid.extras_mut()
            .set_rgb_ring_range(0, 6, 8, None, Some(dead_bg), rows, cols);
        assert!(
            grid.cell(0, 7)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: col 7 is the continuation, one past the right margin"
        );

        grid.set_horizontal_margins(2, 6);
        let bce_bg = [10u8, 20, 30];
        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(bce_bg[0], bce_bg[1], bce_bg[2]));
        grid.set_cursor_template(fill, Some(bce_bg));

        // EL 2 inside the margins erases [2, 7): the lead at col 6 goes,
        // orphaning the continuation at col 7.
        grid.move_cursor_to(0, 4);
        grid.erase_line();

        let orphan = grid.cell(0, 7).unwrap();
        assert_eq!(orphan.char(), ' ', "the orphaned continuation is blanked");
        assert_eq!(
            orphan.colors(),
            fill.colors(),
            "the margin branch must give the orphan the BCE background too — \
             xterm reaches the same ClearRight -> ClearCells here"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 6),
            Some(bce_bg),
            "precondition: the erased span resolves its truecolor background"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 7),
            Some(bce_bg),
            "the orphan past the right margin must resolve it too — the ring \
             has to reach one column past the margin or the orphan keeps the \
             dead glyph's bytes behind a BCE marker"
        );
    }

    /// DECERA does not give the orphan the BCE background: xterm's
    /// `ScrnFillRectangle` fixup is `Clear1Cell(ld, left - 1)`, a bare clear
    /// without the colour pair. The orphan is outside the rectangle.
    #[test]
    fn erase_rect_keeps_the_wide_orphan_empty_under_a_bce_background() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );

        // A visible BCE template is live — the EL paths above would use it.
        grid.set_cursor_template(Cell::bce_blank(PackedColors::with_indexed_bg(4)), None);

        // DECERA over cols 1..=5: col 1 is the continuation, orphaning col 0.
        grid.erase_rect(0, 1, 0, 5);

        assert!(
            is_empty_at(&grid, 0, 0),
            "#7522: the orphan one column LEFT of the rect must stay Cell::EMPTY, \
             not take the rect's fill colour"
        );
    }

    /// DECFRA's fill glyph must not bleed into the orphan one column outside the
    /// rectangle.
    #[test]
    fn fill_rect_keeps_the_wide_orphan_empty() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        grid.set_cursor_template(Cell::bce_blank(PackedColors::with_indexed_bg(4)), None);

        let fill = Cell::new('X');
        grid.fill_rect(fill, 0, 1, 0, 5, None, None);

        assert_eq!(char_at(&grid, 0, 3), 'X', "the rect itself takes the fill");
        assert!(
            is_empty_at(&grid, 0, 0),
            "#7522: the DECFRA fill glyph must not bleed onto the orphan"
        );
    }

    #[test]
    fn test_fill_rect_preserves_cell_attributes() {
        use crate::{Cell, PackedColors};
        let mut grid = Grid::new(4, 10);
        // Create a styled cell template: 'X' with BOLD flag and indexed FG color.
        let colors = PackedColors::with_indexed(196, 0);
        let flags = CellFlags::BOLD;
        let template = Cell::from_ascii_styled(b'X', colors, flags);
        grid.fill_rect(template, 1, 2, 2, 5, None, None);

        for row in 1..=2 {
            for col in 2..=5 {
                let cell = grid.cell(row, col).expect("cell should exist");
                assert_eq!(cell.char(), 'X', "({row},{col}) should be 'X'");
                assert!(
                    cell.flags().contains(CellFlags::BOLD),
                    "({row},{col}) should have BOLD flag (#7655)"
                );
                assert!(
                    cell.colors().fg_is_indexed(),
                    "({row},{col}) should have indexed FG color (#7655)"
                );
            }
        }
    }

    #[test]
    fn test_fill_rect_repopulates_truecolor_ring() {
        // DECFRA with truecolor SGR: the cell carries only the RGB marker, so
        // fill_rect must write the resolved bytes back into the RGB ring.
        use crate::{Cell, PackedColors};
        let mut grid = Grid::new(4, 10);
        let colors = PackedColors::new().with_rgb_fg().with_rgb_bg();
        let fg = [30u8, 60, 90];
        let bg = [10u8, 20, 30];
        let template = Cell::from_ascii_styled(b'Z', colors, CellFlags::empty());
        grid.fill_rect(template, 1, 2, 2, 5, Some(fg), Some(bg));

        for row in 1..=2 {
            for col in 2..=5 {
                let cell = grid.cell(row, col).expect("cell should exist");
                assert_eq!(cell.char(), 'Z', "({row},{col}) should be 'Z'");
                // Markers present...
                assert!(
                    cell.colors().fg_is_rgb() && cell.colors().bg_is_rgb(),
                    "({row},{col}) should carry RGB markers"
                );
                // ...and the actual bytes resolve through the ring.
                assert_eq!(
                    grid.fg_rgb_at(row, col),
                    Some(fg),
                    "({row},{col}) fg truecolor must survive DECFRA"
                );
                assert_eq!(
                    grid.bg_rgb_at(row, col),
                    Some(bg),
                    "({row},{col}) bg truecolor must survive DECFRA"
                );
            }
        }
        // Cells outside the rect keep no stray truecolor.
        assert_eq!(grid.fg_rgb_at(0, 2), None, "row above rect untouched");
        assert_eq!(grid.bg_rgb_at(1, 1), None, "col left of rect untouched");
    }

    #[test]
    fn test_copy_rect_repopulates_truecolor_and_complex_rings() {
        // DECCRA carries ring-resident truecolor (rgb_ring) and complex
        // codepoints (complex_ring) to the destination; the cell copy holds only
        // the markers.
        use crate::{Cell, CellCoord, PackedColors};
        let mut grid = Grid::new(4, 10);
        let rows = grid.rows();
        let cols = grid.cols();

        // Source row 0, col 1: truecolor 'A' whose bytes live in the rgb ring
        // (HashMap empty), the bulk write path's layout.
        let fg = [200u8, 100, 50];
        let bg = [12u8, 34, 56];
        let rgb_colors = PackedColors::new().with_rgb_fg().with_rgb_bg();
        let truecolor_cell = Cell::from_ascii_styled(b'A', rgb_colors, CellFlags::empty());
        if let Some(r) = grid.row_mut(0) {
            r.set(1, truecolor_cell);
        }
        grid.extras_mut()
            .set_rgb_ring_range(0, 1, 2, Some(fg), Some(bg), rows, cols);

        // Source row 0, col 3: a complex codepoint whose value lives in the
        // complex ring (HashMap empty), with the COMPLEX marker on the cell.
        let emoji = '\u{1F680}'; // 🚀
        let mut complex_cell = Cell::EMPTY;
        let flags = complex_cell.flags().union(CellFlags::COMPLEX);
        complex_cell.set_flags(flags);
        if let Some(r) = grid.row_mut(0) {
            r.set(3, complex_cell);
        }
        grid.extras_mut()
            .set_complex_char_ring(0, 3, emoji, rows, cols);

        // Preconditions: the source resolves through the rings, with the HashMap
        // empty for these cells.
        assert_eq!(grid.fg_rgb_at(0, 1), Some(fg));
        assert_eq!(grid.bg_rgb_at(0, 1), Some(bg));
        assert_eq!(grid.complex_char_at(0, 3), Some(emoji));
        assert!(grid.extras().get(CellCoord::new(0, 1)).is_none());
        assert!(grid.extras().get(CellCoord::new(0, 3)).is_none());

        // DECCRA-copy the whole source row 0 down to row 2.
        grid.copy_rect(0, 0, 0, 9, 2, 0);

        // The destination reproduces both the truecolor and the complex char.
        let dst_tc = grid.cell(2, 1).expect("dst truecolor cell exists");
        assert!(
            dst_tc.colors().fg_is_rgb() && dst_tc.colors().bg_is_rgb(),
            "dst cell keeps RGB markers"
        );
        assert_eq!(
            grid.fg_rgb_at(2, 1),
            Some(fg),
            "fg truecolor must survive DECCRA copy"
        );
        assert_eq!(
            grid.bg_rgb_at(2, 1),
            Some(bg),
            "bg truecolor must survive DECCRA copy"
        );
        assert!(
            grid.cell(2, 3)
                .expect("dst complex cell exists")
                .is_complex(),
            "dst cell keeps COMPLEX marker"
        );
        assert_eq!(
            grid.complex_char_at(2, 3),
            Some(emoji),
            "complex codepoint must survive DECCRA copy"
        );
    }

    #[test]
    fn test_copy_rect_clears_stale_dst_complex_ring() {
        // clear_rect wipes the destination complex ring: copying a plain cell
        // over a former complex char must not leave the old codepoint readable
        // (reads consult the ring first regardless of the COMPLEX flag).
        use crate::Cell;
        let mut grid = Grid::new(4, 10);
        let rows = grid.rows();
        let cols = grid.cols();

        // Destination row 2, col 0 starts with a complex emoji in the ring.
        let mut complex_cell = Cell::EMPTY;
        let flags = complex_cell.flags().union(CellFlags::COMPLEX);
        complex_cell.set_flags(flags);
        if let Some(r) = grid.row_mut(2) {
            r.set(0, complex_cell);
        }
        grid.extras_mut()
            .set_complex_char_ring(2, 0, '\u{1F600}', rows, cols);
        assert_eq!(grid.complex_char_at(2, 0), Some('\u{1F600}'));

        // Source row 0, col 0 is a plain 'B'.
        grid.move_cursor_to(0, 0);
        write_text(&mut grid, "B");

        // Copy the plain source over the complex destination.
        grid.copy_rect(0, 0, 0, 0, 2, 0);

        assert_eq!(char_at(&grid, 2, 0), 'B', "dst now holds the copied 'B'");
        assert!(
            !grid.cell(2, 0).expect("dst cell exists").is_complex(),
            "dst cell is no longer complex"
        );
        assert_eq!(
            grid.complex_char_at(2, 0),
            None,
            "stale dst complex codepoint must be cleared by copy_rect"
        );
    }

    // ED erases complete lines back to single-width (DECDWL/DECDHL), as VT510
    // ED and xterm's `ClearBufRows` (reached from ED 0, 1 and 2) do.

    /// Set a row's DEC line attribute the way `ESC#3` / `ESC#4` / `ESC#6` does:
    /// the row flag plus the grid-level fast-path flag.
    fn poison_row(grid: &mut Grid, row: u16, size: crate::LineSize) {
        if let Some(r) = grid.row_mut(row) {
            r.set_line_size(size);
        }
        grid.mark_has_double_width();
    }

    fn line_size_of(grid: &Grid, row: u16) -> crate::LineSize {
        grid.row(row)
            .map_or(crate::LineSize::SingleWidth, |r| r.line_size())
    }

    #[test]
    fn ed2_returns_every_erased_row_to_single_width() {
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 0, crate::LineSize::DoubleWidth);
        poison_row(&mut grid, 1, crate::LineSize::DoubleHeightTop);
        poison_row(&mut grid, 2, crate::LineSize::DoubleHeightBottom);

        grid.erase_screen();

        for row in 0..4 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::SingleWidth,
                "ED 2 erases complete lines, so row {row} is single-width again"
            );
        }
        assert!(
            !grid.has_any_double_width(),
            "no visible row is double any more, so the fast-path flag goes with them"
        );
    }

    #[test]
    fn ed2_gives_back_the_columns_a_double_width_row_had_halved() {
        // On a double-width row only half the columns are reachable; after
        // ED 2 the whole row is addressable again.
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 0, crate::LineSize::DoubleWidth);
        grid.set_cursor(0, 19);
        assert_eq!(
            grid.cursor().col,
            9,
            "while double-width, column 19 of a 20-column row clamps to the halfway mark"
        );

        grid.erase_screen();
        grid.set_cursor(0, 19);

        assert_eq!(
            grid.cursor().col,
            19,
            "after ED 2 the erased row is single-width, so all 20 columns are usable"
        );
    }

    #[test]
    fn ed0_resets_the_complete_rows_below_and_leaves_the_cursor_row_alone() {
        // xterm `ClearBelow`: `ClearRight` over the cursor row (a partial erase,
        // which never touches DblCS) then `ClearBufRows(cur_row + 1, max_row)`.
        let mut grid = Grid::new(4, 20);
        for row in 0..4 {
            poison_row(&mut grid, row, crate::LineSize::DoubleWidth);
        }
        grid.set_cursor(1, 2);

        grid.erase_to_end_of_screen();

        assert_eq!(
            line_size_of(&grid, 0),
            crate::LineSize::DoubleWidth,
            "ED 0 does not reach the rows above the cursor"
        );
        assert_eq!(
            line_size_of(&grid, 1),
            crate::LineSize::DoubleWidth,
            "the cursor row is a partial erase and keeps its attribute"
        );
        for row in 2..4 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::SingleWidth,
                "row {row} was erased as a complete line"
            );
        }
        assert!(
            grid.has_any_double_width(),
            "rows 0 and 1 are still double, so the fast-path flag must stay raised"
        );
    }

    #[test]
    fn ed1_resets_the_complete_rows_above_and_leaves_the_cursor_row_alone() {
        // xterm `ClearAbove`: `ClearBufRows(0, cur_row - 1)` then `ClearLeft`
        // over the cursor row, which never touches DblCS.
        let mut grid = Grid::new(4, 20);
        for row in 0..4 {
            poison_row(&mut grid, row, crate::LineSize::DoubleWidth);
        }
        grid.set_cursor(2, 2);

        grid.erase_from_start_of_screen();

        for row in 0..2 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::SingleWidth,
                "row {row} was erased as a complete line"
            );
        }
        assert_eq!(
            line_size_of(&grid, 2),
            crate::LineSize::DoubleWidth,
            "the cursor row is a partial erase and keeps its attribute"
        );
        assert_eq!(
            line_size_of(&grid, 3),
            crate::LineSize::DoubleWidth,
            "ED 1 does not reach the rows below the cursor"
        );
    }

    #[test]
    fn an_ed0_that_clears_the_last_double_row_lowers_the_fast_path_flag() {
        // The flag may only be lowered by a rescan: ED 0 resets a subset of
        // rows, so a double row surviving above the cursor keeps it raised.
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 3, crate::LineSize::DoubleWidth);
        grid.set_cursor(0, 0);

        grid.erase_to_end_of_screen();

        assert_eq!(line_size_of(&grid, 3), crate::LineSize::SingleWidth);
        assert!(
            !grid.has_any_double_width(),
            "the last double row is gone, so the rescan lowers the flag"
        );
    }

    #[test]
    fn el2_keeps_the_line_attribute_because_xterm_does() {
        // The deliberate asymmetry: EL 2 (`ClearLine` -> `ClearInLine2`) never
        // reaches `ClearBufRows`, and the VT510 EL page has no single-width rule.
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 1, crate::LineSize::DoubleWidth);
        grid.set_cursor(1, 0);

        grid.erase_line();

        assert_eq!(
            line_size_of(&grid, 1),
            crate::LineSize::DoubleWidth,
            "EL 2 clears the characters, not the line attribute"
        );
    }

    #[test]
    fn decsed_preserves_line_attributes_on_every_row_it_erases() {
        // xterm's protected erase branch loops over `ClearInLine` and never
        // reaches `ClearBufRows`, so DECSED keeps the attribute on whole lines.
        let mut grid = Grid::new(4, 20);
        for row in 0..4 {
            poison_row(&mut grid, row, crate::LineSize::DoubleWidth);
        }

        grid.selective_erase_screen();

        for row in 0..4 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::DoubleWidth,
                "DECSED keeps row {row}'s line attribute"
            );
        }
    }
}
