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

    /// Reclaimable history bytes (promoted store budget plus ring hyperlink bytes), or `None` without a tiered store.
    pub fn history_scrollback_bytes(&mut self) -> Option<usize> {
        let grid = self.history_grid_mut();
        let store = grid.scrollback_mut()?.budgeted_memory_used();
        Some(store + grid.history_link_bytes())
    }

    /// Evicts history down to `keep(budget)` bytes and restores `budget`, returning the before/after byte counts.
    pub fn shed_history_scrollback(
        &mut self,
        keep: impl FnOnce(usize) -> usize,
    ) -> Option<HistoryShed> {
        let grid = self.history_grid_mut();
        let store = grid.scrollback_mut()?;
        let budget = store.memory_budget();
        let before = store.budgeted_memory_used() + grid.history_link_bytes();
        let _ = self.set_memory_budget(keep(budget));
        let _ = self.set_memory_budget(budget);
        let after = self.history_scrollback().map_or(
            0,
            crate::scrollback::ScrollbackStorage::budgeted_memory_used,
        ) + self.main_grid().history_link_bytes();
        Some(HistoryShed { before, after })
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

    /// Pops the oldest queued OSC app-event `(code, payload)`.
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
        let text = bounded_paste_text(text);

        if bracketed {
            let sanitized: String = text.chars().filter(|&c| paste_char_allowed(c)).collect();
            let sanitized = sanitized.replace("\r\n", "\r").replace('\n', "\r");
            let mut result = Vec::with_capacity(sanitized.len() + 12);
            result.extend_from_slice(b"\x1b[200~");
            result.extend_from_slice(sanitized.as_bytes());
            result.extend_from_slice(b"\x1b[201~");
            result
        } else {
            let cleaned: String = text.chars().filter(|&c| paste_char_allowed(c)).collect();
            cleaned
                .replace("\r\n", "\r")
                .replace('\n', "\r")
                .into_bytes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PASTE_BYTES, PASTE_GESTURE_PROBE_BYTES, PASTE_INSERT_PRICE_BYTES, Terminal,
        paste_char_allowed,
    };

    fn write_lines(t: &mut Terminal, start: usize, n: usize) {
        for i in start..start + n {
            t.process(format!("line-{i}\r\n").as_bytes());
        }
    }

    #[test]
    fn ring_only_scrollback_limit_governs_retention() {
        let mut t = Terminal::new(5, 20);
        t.set_scrollback_line_limit(Some(100));
        write_lines(&mut t, 0, 500);
        assert_eq!(
            t.grid().scrollback_lines(),
            100,
            "cap applies as lines scroll off"
        );

        t.set_scrollback_line_limit(Some(40));
        assert_eq!(t.grid().scrollback_lines(), 40);
        let oldest = t
            .grid()
            .get_history_line(0)
            .map(|l| l.to_string().trim_end().to_string());
        assert_eq!(
            oldest.as_deref(),
            Some("line-456"),
            "oldest lines evicted, newest kept"
        );

        t.set_scrollback_line_limit(Some(20_000));
        write_lines(&mut t, 500, 11_000);
        assert_eq!(
            t.grid().scrollback_lines(),
            40 + 11_000,
            "a raised limit really lifts the old ring cap"
        );
    }

    #[test]
    fn scrollback_limit_targets_primary_content_never_the_alt_buffer() {
        let mut t = Terminal::new(5, 20);
        write_lines(&mut t, 0, 300);
        t.process(b"\x1b[?1049h"); // enter alt screen (primary saved off)

        t.set_scrollback_line_limit(Some(50));
        assert_eq!(
            t.main_grid().scrollback_lines(),
            50,
            "saved primary truncated through the alt screen"
        );

        write_lines(&mut t, 1000, 30);
        assert_eq!(
            t.grid().scrollback_lines(),
            0,
            "the alt buffer's zero-scrollback ring must not inherit the limit"
        );

        t.process(b"\x1b[?1049l"); // back to primary
        assert_eq!(t.grid().scrollback_lines(), 50);
        assert_eq!(t.scrollback_line_limit(), Some(50));
    }

    #[test]
    fn requested_truncation_is_not_counted_as_loss() {
        let mut t = Terminal::new(5, 20);
        write_lines(&mut t, 0, 500);
        assert_eq!(t.scrollback_truncated_lines(), 0);
        t.set_scrollback_line_limit(Some(10));
        assert_eq!(t.grid().scrollback_lines(), 10);
        assert_eq!(t.scrollback_truncated_lines(), 0);
    }

    #[test]
    fn bracketed_paste_blocks_embedded_terminators() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2004h");
        for (paste, wire) in [
            (
                "safe\x1b[201~rm -rf ~",
                &b"\x1b[200~safe[201~rm -rf ~\x1b[201~"[..],
            ),
            ("a\u{009B}201~b", &b"\x1b[200~a201~b\x1b[201~"[..]),
        ] {
            assert_eq!(term.format_paste(paste), wire, "{paste:?}");
        }
    }

    #[test]
    fn unbracketed_paste_strips_escape_and_c1() {
        let term = Terminal::new(24, 80);
        assert_eq!(term.format_paste("a\x1b[31mb\u{009D}c"), b"a[31mbc");
    }

    #[test]
    fn a_supplied_framing_decides_the_guards_and_nothing_else() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(
            term.format_paste_framed("a\x1b[31mb\nc", true),
            b"\x1b[200~a[31mb\rc\x1b[201~"
        );
        term.process(b"\x1b[?2004h");
        assert_eq!(
            term.format_paste_framed("a\x1b[31mb\nc", false),
            b"a[31mb\rc"
        );
        assert_eq!(
            term.format_paste_framed("a\x1b[31mb\nc", term.modes().bracketed_paste),
            term.format_paste("a\x1b[31mb\nc")
        );
    }

    #[test]
    fn paste_payload_probe_is_the_formatter_sanitizer_without_protocol_wrappers() {
        assert!(!Terminal::paste_has_payload(""));
        assert!(!Terminal::paste_has_payload("\x1b\x03\u{009b}\x7f"));
        assert!(!Terminal::paste_has_payload("\u{0301}\u{fe0f}\u{200d}"));
        assert!(Terminal::paste_has_payload("\t"));
        assert!(Terminal::paste_has_payload("\n"));
        assert!(Terminal::paste_has_payload("中文🙂"));

        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2004h");
        assert_eq!(term.format_paste(""), b"\x1b[200~\x1b[201~");
        assert!(
            !Terminal::paste_has_payload(""),
            "bracket wrappers are protocol framing, not authored movement"
        );

        let adversarial = "\x1b".repeat(MAX_PASTE_BYTES);
        assert!(
            !Terminal::paste_has_payload(&adversarial),
            "a cap-sized control-only paste stays dark on the bounded probe"
        );
        let hidden = format!("{}x", "\x1b".repeat(PASTE_GESTURE_PROBE_BYTES));
        assert!(
            !Terminal::paste_has_payload(&hidden),
            "content beyond the constant-cost prefix is conservatively unlicensed"
        );
    }

    #[test]
    fn paste_sends_bytes_is_the_formatters_output_for_every_char() {
        let term = Terminal::new(24, 80);
        let mut buf = [0u8; 4];
        let mut kept = 0usize;
        for c in (0..=0x10_ffff_u32).filter_map(char::from_u32) {
            let alone = c.encode_utf8(&mut buf);
            let wrapped = format!("\x1b\u{0085}{alone}\x03");
            for text in [&*alone, &*wrapped] {
                for bracketed in [false, true] {
                    assert_eq!(
                        Terminal::paste_sends_bytes(text, bracketed),
                        !term.format_paste_framed(text, bracketed).is_empty(),
                        "{c:?} in {text:?}, bracketed={bracketed}"
                    );
                }
            }
            kept += usize::from(paste_char_allowed(c));
        }
        assert!(kept > 1_000_000, "the walk met the kept chars: {kept}");
        let controls: String = (0..=0x9f_u32)
            .filter_map(char::from_u32)
            .filter(|c| !paste_char_allowed(*c))
            .collect();
        assert_eq!(
            controls.chars().count(),
            0x20 + 0x21 - 3,
            "every control but TAB, LF, CR"
        );
        for text in [
            controls.clone(),
            controls.repeat(3),
            "\x1b".repeat(MAX_PASTE_BYTES + 1),
            format!("{}x", "\x1b".repeat(MAX_PASTE_BYTES)),
        ] {
            assert!(
                !Terminal::paste_sends_bytes(&text, false),
                "{:?}",
                &text[..8]
            );
            assert!(Terminal::paste_sends_bytes(&text, true));
        }
        assert!(Terminal::paste_sends_bytes(
            &format!("{}x", "\x1b".repeat(200)),
            false
        ));
    }

    #[test]
    fn unbracketed_paste_strips_c0_signal_bytes() {
        let term = Terminal::new(24, 80);
        assert_eq!(term.format_paste("a\x03b\x04c\x1ad\x08e\x7ff"), b"abcdef");
        assert_eq!(term.format_paste("a\tb\n"), b"a\tb\r");
    }

    #[test]
    fn paste_converts_line_breaks_to_cr() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(term.format_paste("x\r\ny\nz"), b"x\ry\rz");
        term.process(b"\x1b[?2004h");
        assert_eq!(term.format_paste("x\r\ny\nz"), b"\x1b[200~x\ry\rz\x1b[201~");
    }

    #[test]
    fn drain_bell_reflects_real_bel_byte() {
        let mut term = Terminal::new(24, 80);
        assert!(!term.drain_bell(), "no bell before any BEL byte");
        term.process(b"\x07");
        assert!(term.drain_bell(), "BEL byte sets the flag");
        assert!(!term.drain_bell(), "flag is cleared after draining");
    }

    #[test]
    fn bels_ring_at_most_once_per_hundred_ms() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::Duration;

        use crate::terminal::ClockReading;

        let mut term = Terminal::new(24, 80);
        let rang = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&rang);
        term.set_bell_callback(move || {
            seen.fetch_add(1, Ordering::Relaxed);
        });
        let t0 = ClockReading::now();
        let at = |ms: u64| ClockReading {
            monotonic: t0.monotonic + Duration::from_millis(ms),
            wall_ms: None,
        };
        term.process_at(b"\x07\x07\x07", at(0));
        term.process_at(b"\x07", at(99));
        assert_eq!(rang.load(Ordering::Relaxed), 1, "a burst is one bell");
        assert_eq!(term.bell_total(), 1);
        term.process_at(b"\x07", at(100));
        assert_eq!(rang.load(Ordering::Relaxed), 2, "a bell 100 ms on rings");
        assert_eq!(term.bell_total(), 2);

        let flood = 10_000;
        let mut rang_at = Vec::new();
        for i in 0..21u64 {
            let before = term.bell_total();
            term.process_at(b"\x07", at(flood + i * 60));
            if term.bell_total() > before {
                rang_at.push(i * 60);
            }
        }
        assert_eq!(
            rang_at,
            (0..=10u64).map(|k| k * 120).collect::<Vec<_>>(),
            "a flood rings once each 100 ms, not once"
        );
        assert_eq!(
            rang.load(Ordering::Relaxed),
            2 + 11,
            "the callback each time"
        );
        assert_eq!(term.bell_total(), 2 + 11);
    }

    #[test]
    fn osc_7_queues_real_cwd_path() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]7;file://host/home/user/project\x07");
        assert_eq!(term.current_working_directory(), Some("/home/user/project"));
        assert_eq!(
            term.take_osc_event(),
            Some((7, "//host/home/user/project".to_string()))
        );
        assert_eq!(term.take_osc_event(), None, "queue drained");

        term.process(b"\x1b]7;file:///var/log\x07");
        assert_eq!(term.take_osc_event(), Some((7, "/var/log".to_string())));
        term.process(b"\x1b]7;file://localhost/var/tmp\x07");
        assert_eq!(term.take_osc_event(), Some((7, "/var/tmp".to_string())));
    }

    #[test]
    fn osc_133_marks_queue_real_payloads() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]133;A\x07");
        term.process(b"\x1b]133;B\x07");
        term.process(b"\x1b]133;C\x07");
        term.process(b"\x1b]133;D;42\x07");

        let codes: Vec<(u32, String)> = std::iter::from_fn(|| term.take_osc_event()).collect();
        assert_eq!(codes.len(), 4, "one event per accepted mark");
        assert_eq!(codes[0].0, 133);
        assert!(codes[0].1.starts_with("A;row="), "A carries row/col");
        assert_eq!(codes[3].1, "D;exit=42", "D carries the real exit code");
    }

    #[test]
    fn osc_52_flood_is_held_to_the_event_byte_budget() {
        use super::super::transient_state::OscEventQueue;
        let mut term = Terminal::new(24, 80);
        term.authorize_clipboard_access(crate::terminal::ClipboardAccess::Write);
        let payload = |i: u8| char::from(b'a' + i % 26).to_string().repeat(60_000);
        for i in 0..40 {
            let encoded = aterm_codec::base64::encode(payload(i).as_bytes()).expect("encodes");
            term.process(format!("\x1b]52;c;{encoded}\x07").as_bytes());
        }
        assert!(term.transient.osc_events.bytes() <= OscEventQueue::MAX_BYTES);

        let kept: Vec<_> = std::iter::from_fn(|| term.take_osc_event()).collect();
        let newest = 40 - kept.len();
        assert_eq!(kept.len(), OscEventQueue::MAX_BYTES / 60_000);
        for (offset, (code, text)) in kept.iter().enumerate() {
            assert_eq!(*code, 52);
            assert!(*text == payload(u8::try_from(newest + offset).unwrap()));
        }
    }

    #[test]
    fn paste_insert_cells_prices_the_sanitized_single_line_body() {
        let path = "/Users//example/Desktop/Screenshot\\ 2026-09-10\\ at\\ 14.02.11.png ";
        assert_eq!(
            Terminal::paste_insert_cells(path, false),
            Some(u16::try_from(path.chars().count()).unwrap()),
            "an ASCII path is one cell per character"
        );
        assert_eq!(Terminal::paste_insert_cells("foo bar ", false), Some(8));
        assert_eq!(
            Terminal::paste_insert_cells("中🙂", false),
            Some(4),
            "CJK and emoji are two cells each"
        );
        assert_eq!(
            Terminal::paste_insert_cells("\u{0301}\u{fe0f}\u{200d}", false),
            Some(0),
            "a base-less run of marks advances nothing"
        );
        assert_eq!(
            Terminal::paste_insert_cells("a\x1b\x03b\u{009b}c\x7f", false),
            Some(3),
            "controls are stripped before pricing, as the formatter strips them"
        );
        assert_eq!(Terminal::paste_insert_cells("", false), Some(0));
        assert_eq!(
            Terminal::paste_insert_cells("a\nb", false),
            None,
            "a line break: the landing row is unknowable"
        );
        assert_eq!(Terminal::paste_insert_cells("a\r\nb", false), None);
        assert_eq!(
            Terminal::paste_insert_cells("a\tb", false),
            None,
            "a TAB: the shell's tab stops are unknowable"
        );
        let long = "x".repeat(PASTE_INSERT_PRICE_BYTES + 1);
        assert_eq!(
            Terminal::paste_insert_cells(&long, false),
            None,
            "a body past the probe wraps or is collapsed: unknown width"
        );
        let exact = "x".repeat(PASTE_INSERT_PRICE_BYTES);
        assert_eq!(
            Terminal::paste_insert_cells(&exact, false),
            Some(u16::try_from(PASTE_INSERT_PRICE_BYTES).unwrap())
        );
        assert!(
            Terminal::paste_insert_cells("±", true) >= Terminal::paste_insert_cells("±", false)
        );
    }

    #[test]
    fn print_anchor_counts_the_pending_wrap_column() {
        let mut t = Terminal::new(4, 10);
        t.process(b"\x1b[?7h");
        t.process(b"123456789");
        let (row, col, seq) = t.print_anchor().expect("printed");
        assert_eq!((row, col), (0, 9), "nine glyphs: the anchor is the cursor");
        assert_eq!(t.cursor().col, 9);
        t.process(b"0");
        let (row, col, seq2) = t.print_anchor().expect("printed");
        assert_eq!(
            (row, col),
            (0, 10),
            "the tenth glyph: the anchor is one past the last column"
        );
        assert_eq!(t.cursor().col, 9, "…while the cursor stays parked on it");
        assert!(t.grid().pending_wrap());
        assert!(seq2 > seq);
        t.process(b"a");
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((1, 1)));
        assert_eq!((t.cursor().row, t.cursor().col), (1, 1));
        t.process(b"\x1b[3;1H");
        t.process(b"abcdefghij");
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((2, 10)));
        t.process(b"\x1b[4;1H");
        assert!(
            !t.grid().pending_wrap(),
            "a cursor address clears the deferred wrap"
        );
        assert_eq!((t.cursor().row, t.cursor().col), (3, 0));
        assert_eq!(
            t.print_anchor().map(|(r, c, _)| (r, c)),
            Some((2, 10)),
            "the anchor is the last PRINT's, not the cursor's"
        );
    }

    #[test]
    fn print_anchor_glyph_is_the_run_s_last_cell() {
        let mut t = Terminal::new(6, 10);
        assert_eq!(t.print_anchor_glyph(), None, "nothing printed yet");
        t.process(b"p% ab");
        assert_eq!(t.print_anchor_glyph(), Some('b'));
        t.process(b"\x1b[1B\r bck: c_\x1b[A\x1b[3G");
        assert_eq!(
            t.print_anchor().map(|(r, c, _)| (r, c)),
            Some((1, 8)),
            "the run ended one past the fake cursor"
        );
        assert_eq!((t.cursor().row, t.cursor().col), (0, 2));
        assert_eq!(t.print_anchor_glyph(), Some('_'), "…on the fake cursor");
        t.process(b"\x1b[3;1H0123456789");
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((2, 10)));
        assert_eq!(t.print_anchor_glyph(), Some('9'));
        t.process("\x1b[4;1H中".as_bytes());
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((3, 2)));
        assert_eq!(t.print_anchor_glyph(), Some('\0'));
        t.process(b"\x1b[5;1Hxy\x1b[2K");
        assert_eq!(t.print_anchor_glyph(), Some(' '));
        t.process(b"\x1b[6;1H");
        for i in 0..12 {
            t.process(format!("\r\nline{i}").as_bytes());
        }
        t.process(b"\x1b[6;1Hzz");
        t.scroll_display(3);
        assert!(t.grid().display_offset() > 0, "the viewport scrolled back");
        let mut shown = Vec::new();
        t.row_cols_into(5, &mut shown);
        assert_ne!(
            shown.get(1),
            Some(&'z'),
            "non-vacuity: the viewport shows history on that row"
        );
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((5, 2)));
        assert_eq!(t.print_anchor_glyph(), Some('z'));
        t.scroll_display(-3);
        t.resize(3, 10);
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((5, 2)));
        assert_eq!(t.print_anchor_glyph(), None);
        let mut u = Terminal::new(2, 4);
        u.process(b"ab");
        u.process(b"\x1bc");
        assert_eq!(u.print_anchor(), None);
        assert_eq!(u.print_anchor_glyph(), None);
    }

    #[test]
    fn paste_preserves_non_ascii_spaces() {
        let mut t = Terminal::new(4, 80);
        let macos_screenshot =
            "'/Users//example/Desktop/Screenshot 2026-10-19 at 12.17.46\u{202f}PM.png'";
        let expected = macos_screenshot.as_bytes();
        assert_eq!(t.format_paste_framed(macos_screenshot, false), expected);
        assert_eq!(
            t.format_paste_framed(macos_screenshot, true),
            [b"\x1b[200~".as_slice(), expected, b"\x1b[201~".as_slice()].concat()
        );
        assert_eq!(
            Terminal::paste_insert_cells(macos_screenshot, false),
            Some(u16::try_from(macos_screenshot.chars().count()).unwrap())
        );
        t.process(b"\x1b[?2004h");
        assert_eq!(
            t.format_paste("a\u{00a0}b\u{202f}c"),
            "\x1b[200~a\u{00a0}b\u{202f}c\x1b[201~".as_bytes()
        );
    }
}

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
