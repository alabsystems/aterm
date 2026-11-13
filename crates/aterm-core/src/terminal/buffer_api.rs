// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Buffer, scrollback, response drain, and paste formatting API for [`Terminal`].

use super::Terminal;

const MAX_PASTE_BYTES: usize = 16 * 1024 * 1024;
const PASTE_GESTURE_PROBE_BYTES: usize = 4 * 1024;
const PASTE_INSERT_PRICE_BYTES: usize = PASTE_GESTURE_PROBE_BYTES;

/// The prefix the paste formatter may inspect, ending on a UTF-8 boundary. The
/// payload probe and the formatter share it, so text beyond the 16 MiB cap
/// cannot arm an input gesture no emitted byte represents.
#[inline]
fn bounded_paste_text(text: &str) -> &str {
    if text.len() <= MAX_PASTE_BYTES {
        return text;
    }
    let mut end = MAX_PASTE_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Sanitizer policy shared by paste formatting and its allocation-free
/// authored-payload probe. Not `const`: stable's `char::is_control` is not,
/// and the cross cells build this crate on stable.
#[inline]
fn paste_char_allowed(c: char) -> bool {
    !(c.is_control() && c != '\t' && c != '\n' && c != '\r')
}

/// Whether one byte of a paste's UTF-8 shows that the text holds a character
/// the sanitizer keeps ([`paste_char_allowed`]), so that
/// `text.bytes().any(paste_byte_kept) == text.chars().any(paste_char_allowed)`
/// for every `&str` (`paste_sends_bytes_is_the_formatters_output_for_every_char`
/// proves it over every `char`). A control (`char::is_control`: U+0000-U+001F
/// and U+007F-U+009F) is one byte below 0x20 or 0x7F, or 0xC2 then
/// 0x80-0x9F. So a byte that is TAB, LF, CR or 0x20-0x7E is a kept ASCII
/// character; a lead byte 0xC3 or above starts a character at U+00C0 or
/// above, never a control; a continuation byte 0xA0-0xBF follows either
/// 0xC2 (U+00A0-U+00BF, kept) or a lead of a kept character. Every other
/// byte (the other C0 bytes, 0x7F, 0x80-0x9F and 0xC2) belongs to a control
/// or to a character another of its bytes already shows.
#[inline]
const fn paste_byte_kept(b: u8) -> bool {
    matches!(
        b,
        b'\t' | b'\n' | b'\r' | 0x20..=0x7e | 0xa0..=0xbf | 0xc3..=0xff
    )
}

/// Whether one sanitizer-surviving character can advance or reposition the
/// cursor. Zero-width marks, variation selectors and joiners cannot land a
/// cursor on their own. Not `const`, as [`paste_char_allowed`] is not.
#[inline]
fn paste_char_can_move(c: char) -> bool {
    paste_char_allowed(c) && (matches!(c, '\t' | '\n' | '\r') || aterm_grapheme::char_width(c) > 0)
}

impl Terminal {
    /// The text a paste of `text` sends before its framing: the prefix the formatter takes, with
    /// every control character but Tab, LF and CR dropped ([`Self::format_paste_framed`] sends
    /// it, each line break as a CR).
    #[must_use]
    pub fn paste_kept_text(text: &str) -> String {
        bounded_paste_text(text)
            .chars()
            .filter(|&c| paste_char_allowed(c))
            .collect()
    }

    /// Whether a paste contains a cursor-advancing character within the bounded probe prefix.
    #[must_use]
    pub fn paste_has_payload(text: &str) -> bool {
        let text = bounded_paste_text(text);
        let mut end = text.len().min(PASTE_GESTURE_PROBE_BYTES);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].chars().any(paste_char_can_move)
    }

    /// Columns a single-line paste advances on its own row, or `None` if multiline, tabbed, or past the probe cap.
    #[must_use]
    pub fn paste_insert_cells(text: &str, ambiguous_width_double: bool) -> Option<u16> {
        use aterm_types::text_shaping::AmbiguousWidth;

        let text = bounded_paste_text(text);
        if text.len() > PASTE_INSERT_PRICE_BYTES {
            return None;
        }
        let mut body = String::with_capacity(text.len());
        for c in text.chars() {
            if matches!(c, '\t' | '\n' | '\r') {
                return None;
            }
            if paste_char_allowed(c) {
                body.push(c);
            }
        }
        if aterm_grapheme::str_width(&body) == 0 {
            return Some(0);
        }
        let ambiguous = if ambiguous_width_double {
            AmbiguousWidth::Double
        } else {
            AmbiguousWidth::Single
        };
        Some(
            u16::try_from(aterm_grapheme::str_grid_columns_with(&body, ambiguous))
                .unwrap_or(u16::MAX),
        )
    }

    /// Reference to the active grid's tiered scrollback storage, if attached.
    #[inline]
    #[must_use]
    pub const fn scrollback(&self) -> Option<&crate::scrollback::ScrollbackStorage> {
        self.grid.scrollback()
    }

    /// Reference to the primary grid's tiered scrollback storage, even while the alternate screen is active.
    #[inline]
    #[must_use]
    pub const fn history_scrollback(&self) -> Option<&crate::scrollback::ScrollbackStorage> {
        self.main_grid().scrollback()
    }

    /// Reclaimable history bytes (promoted store budget, ring hyperlink bytes and what taking
    /// the inline images only history holds off it frees beyond the shares of them the store's
    /// hot lines book), or `None` without a tiered store.
    pub fn history_scrollback_bytes(&mut self) -> Option<usize> {
        let stored = self.transient.kitty_store_images();
        let grid = self.history_grid_mut();
        let store = grid.scrollback_mut()?.budgeted_memory_used();
        let links = grid.history_link_bytes();
        let images = super::handler_osc_1337::unbooked_history_image_bytes(grid, &stored);
        Some(store + links + images)
    }

    /// Evicts history down to `keep(budget)` bytes and restores `budget`, returning the before/after byte counts.
    pub fn shed_history_scrollback(
        &mut self,
        keep: impl FnOnce(usize) -> usize,
    ) -> Option<HistoryShed> {
        let budget = self.history_grid_mut().scrollback_mut()?.memory_budget();
        let before = self.history_scrollback_bytes()?;
        let _ = self.set_memory_budget(keep(budget));
        let _ = self.set_memory_budget(budget);
        let after = self.history_scrollback_bytes().unwrap_or(0);
        Some(HistoryShed { before, after })
    }

    /// Hold the primary history's inline images to its (possibly just lowered) budget now, and
    /// restart the placement room from what is left.
    fn enforce_history_image_budget(&mut self) {
        let stored = self.transient.kitty_store_images();
        let grid = self.history_grid_mut();
        let budget = super::handler_osc_1337::history_image_budget(grid);
        let found = super::handler_osc_1337::measure_history_images(grid, &stored, budget);
        self.transient.history_image_room = found.room(budget);
    }

    /// Drains deferred scrollback rows into attached tiered storage for all grids.
    #[inline]
    pub fn sync_scrollback_buffers(&mut self) {
        let _ = self.grid.scrollback_mut();
        if let Some(ref mut alt) = self.alt_grid {
            let _ = alt.scrollback_mut();
        }
    }

    /// Sets the scrollback memory budget (bytes) for the main and alternate grids.
    pub fn set_memory_budget(
        &mut self,
        budget: usize,
    ) -> Result<(), aterm_scrollback::ScrollbackError> {
        let mut first_err = None;
        if let Err(e) = self.grid.set_scrollback_memory_budget(budget) {
            first_err = Some(e);
        }
        if let Some(ref mut alt) = self.alt_grid {
            if let Err(e) = alt.set_scrollback_memory_budget(budget) {
                first_err.get_or_insert(e);
            }
        }
        self.enforce_history_image_budget();
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Sets the retained scrollback line limit (`None` = unlimited) on the primary-content grid.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "Evict",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    pub fn set_scrollback_line_limit(&mut self, limit: Option<usize>) {
        let primary = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        if let Some(grid) = primary {
            grid.set_scrollback_line_limit(limit);
            grid.clamp_display_offset();
        }
        self.grid.clamp_display_offset();
        if !self.modes.alternate_screen {
            let live_top_abs = self
                .grid
                .absolute_row_counter()
                .saturating_sub(u64::from(self.grid.rows()));
            self.text_selection
                .truncate_to_floor(self.grid.oldest_absolute_row(), live_top_abs);
        }
    }

    /// Retained scrollback line limit of the primary-content grid (`None` = unlimited).
    #[inline]
    #[must_use]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        self.main_grid().scrollback_line_limit()
    }

    /// Highest scrollback watermark pressure across the main and alternate grids.
    #[inline]
    #[must_use]
    pub fn scrollback_pressure_level(&self) -> crate::scrollback::WatermarkLevel {
        let mut level = self.grid.scrollback().map_or(
            crate::scrollback::WatermarkLevel::Green,
            aterm_scrollback::ScrollbackStorage::watermark_level,
        );
        if let Some(ref alt) = self.alt_grid {
            if let Some(scrollback) = alt.scrollback() {
                level = level.max(scrollback.watermark_level());
            }
        }
        level
    }

    /// Monotonic count of history lines lost to non-user-requested truncation across both grids.
    #[inline]
    #[must_use]
    pub fn scrollback_truncated_lines(&self) -> u64 {
        let mut total = self.grid.truncated_lines();
        if let Some(ref alt) = self.alt_grid {
            total += alt.truncated_lines();
        }
        total
    }

    /// Monotonic count of history rows whose inline images were dropped when compressed into the tiered store.
    #[inline]
    #[must_use]
    pub fn scrollback_image_rows_dropped(&self) -> u64 {
        let mut total = self.grid.image_rows_dropped_by_compression();
        if let Some(ref alt) = self.alt_grid {
            total += alt.image_rows_dropped_by_compression();
        }
        total
    }

    /// Clears all scrollback history across both grids and clears any active or parked text selection.
    pub fn clear_scrollback(&mut self) {
        self.grid.erase_scrollback();
        if let Some(ref mut alt) = self.alt_grid {
            alt.erase_scrollback();
        }
        self.text_selection.clear();
        self.parked_text_selection.clear();
        let oldest = self.main_grid().oldest_absolute_row();
        super::handler::forget_erased_history(oldest, &mut self.shell, &mut self.marks_state);
        self.host_journal.push(super::HostOp::ClearScrollback);
    }

    /// Scrolls the viewport by `delta` lines and records the scroll custody transition.
    #[inline]
    pub fn scroll_display(&mut self, delta: i32) {
        let before = self.grid.display_offset();
        self.grid.scroll_display(delta);
        self.note_scroll_custody(before);
    }

    /// Scrolls to the top of scrollback and records the scroll custody transition.
    #[inline]
    pub fn scroll_to_top(&mut self) {
        let before = self.grid.display_offset();
        self.grid.scroll_to_top();
        self.note_scroll_custody(before);
    }

    /// Scrolls to the bottom (live content) without recording a non-typing user scroll gesture.
    #[inline]
    pub fn scroll_to_bottom(&mut self) {
        self.grid.scroll_to_bottom();
    }

    /// Scrolls to the bottom as a non-typing user gesture and records the custody transition.
    #[inline]
    pub fn return_to_live(&mut self) {
        let before = self.grid.display_offset();
        self.grid.scroll_to_bottom();
        if before == 0 {
            self.reader_gestures = self.reader_gestures.wrapping_add(1);
        }
        self.note_scroll_custody(before);
    }

    /// Scrolls the viewport so `target_abs_row` sits at the top line, clamped to retained history.
    #[inline]
    pub fn scroll_to_absolute_row(&mut self, target_abs_row: u64) {
        let before = self.grid.display_offset();
        self.grid.scroll_to_absolute_row(target_abs_row);
        self.note_scroll_custody(before);
    }

    /// Drains all queued PTY responses and reports, or returns `None` if both buffers are empty.
    #[must_use]
    pub fn take_response(&mut self) -> Option<Vec<u8>> {
        let transient = &mut self.transient;
        if transient.response_buffer.is_empty() && transient.report_buffer.is_empty() {
            return None;
        }
        let mut data = Vec::with_capacity(transient.owed_len());
        data.extend_from_slice(&transient.response_buffer);
        data.extend_from_slice(&transient.report_buffer);
        transient.response_buffer.clear();
        transient.report_buffer.clear();
        Some(data)
    }

    /// Drains unsolicited terminal reports (DEC 2048 size reports and DSR 997 colour-scheme pushes).
    #[inline]
    #[must_use]
    pub fn take_reports(&mut self) -> Option<Vec<u8>> {
        if self.transient.report_buffer.is_empty() {
            None
        } else {
            let data = self.transient.report_buffer.clone();
            self.transient.report_buffer.clear();
            Some(data)
        }
    }

    /// Drains the edge-triggered BEL flag, returning `true` if a bell fired since the last drain.
    #[inline]
    pub const fn drain_bell(&mut self) -> bool {
        let pending = self.transient.bell_pending;
        self.transient.bell_pending = false;
        pending
    }

    /// Queue OSC app-events for [`Self::take_osc_event`] (`true`), or drop the queued ones and
    /// queue none (`false`, the default). A host that never drains the queue leaves it off, so
    /// the terminal keeps no events for it and a row splice has none to move.
    #[inline]
    pub fn set_osc_event_polling(&mut self, on: bool) {
        self.transient.osc_events.set_polled(on);
    }

    /// Pops the oldest queued OSC app-event `(code, payload)`; none unless the host polls
    /// ([`Self::set_osc_event_polling`]).
    #[inline]
    #[must_use]
    pub fn take_osc_event(&mut self) -> Option<(u32, String)> {
        self.transient.osc_events.pop_front()
    }

    /// Whether any OSC app-event is queued.
    #[inline]
    #[must_use]
    pub fn has_osc_events(&self) -> bool {
        !self.transient.osc_events.is_empty()
    }

    /// Formats `text` for pasting using the terminal's current bracketed-paste mode.
    ///
    /// ```
    /// use aterm_core::terminal::Terminal;
    ///
    /// let mut term = Terminal::new(24, 80);
    /// assert_eq!(term.format_paste("hello"), b"hello");
    /// term.process(b"\x1b[?2004h");
    /// assert_eq!(term.format_paste("hello"), b"\x1b[200~hello\x1b[201~");
    /// ```
    #[inline]
    #[must_use]
    pub fn format_paste(&self, text: &str) -> Vec<u8> {
        self.format_paste_framed(text, self.modes.bracketed_paste)
    }

    /// Whether pasting `text` with `bracketed` framing writes at least one byte.
    #[inline]
    #[must_use]
    pub fn paste_sends_bytes(text: &str, bracketed: bool) -> bool {
        bracketed
            || bounded_paste_text(text).as_bytes().chunks(64).any(|block| {
                block
                    .iter()
                    .fold(false, |kept, &b| kept | paste_byte_kept(b))
            })
    }

    /// Formats `text` for pasting with explicit `bracketed` framing, stripping unsafe control bytes.
    ///
    /// ```
    /// use aterm_core::terminal::Terminal;
    ///
    /// let mut term = Terminal::new(24, 80);
    /// // The mode says nothing here: the caller's decision is what frames it.
    /// assert_eq!(term.format_paste_framed("hello", true), b"\x1b[200~hello\x1b[201~");
    /// term.process(b"\x1b[?2004h");
    /// assert_eq!(term.format_paste_framed("hello", false), b"hello");
    /// ```
    #[must_use]
    pub fn format_paste_framed(&self, text: &str, bracketed: bool) -> Vec<u8> {
        let kept = Self::paste_kept_text(text)
            .replace("\r\n", "\r")
            .replace('\n', "\r");
        if bracketed {
            let mut result = Vec::with_capacity(kept.len() + 12);
            result.extend_from_slice(b"\x1b[200~");
            result.extend_from_slice(kept.as_bytes());
            result.extend_from_slice(b"\x1b[201~");
            result
        } else {
            kept.into_bytes()
        }
    }
}

#[cfg(test)]
#[path = "buffer_api_tests.rs"]
mod tests;

/// Reclaimable history bytes before and after a [`Terminal::shed_history_scrollback`] pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryShed {
    /// Reclaimable history bytes before the pass.
    pub before: usize,
    /// Reclaimable history bytes after the pass.
    pub after: usize,
}

impl HistoryShed {
    /// Bytes reclaimed by the pass.
    #[inline]
    #[must_use]
    pub const fn freed(self) -> usize {
        self.before.saturating_sub(self.after)
    }
}
