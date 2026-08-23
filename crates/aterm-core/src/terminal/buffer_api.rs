// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Buffer and scrollback API for [`Terminal`](super::Terminal).
//!
//! Scrollback buffer access, memory management, viewport scrolling,
//! response buffer operations, and paste formatting.
//! Extracted from `mod.rs` as part of #5524.

use super::Terminal;

/// Maximum paste size in bytes (16 MiB). Pastes exceeding this are truncated
/// at a char boundary to prevent unbounded memory allocation (#7379).
const MAX_PASTE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum UTF-8 prefix inspected synchronously by the GUI's cursor-gesture
/// admission probe. Formatting still consumes the full bounded paste off the
/// event thread; admission is deliberately conservative beyond this prefix.
const PASTE_GESTURE_PROBE_BYTES: usize = 4 * 1024;

/// Return the prefix the paste formatter is permitted to inspect, ending on a
/// UTF-8 boundary.  Keep the payload probe and formatter on this one boundary
/// so text surviving only beyond the 16 MiB cap cannot arm an input gesture
/// that no emitted byte can represent.
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
/// authored-payload probe.
#[inline]
fn paste_char_allowed(c: char) -> bool {
    !(c.is_control() && c != '\t' && c != '\n' && c != '\r')
}

/// Whether one sanitizer-surviving character can advance/reposition a terminal
/// cursor. Zero-width combining marks, variation selectors and joiners may
/// legitimately egress as text, but cannot own a landing by themselves.
#[inline]
fn paste_char_can_move(c: char) -> bool {
    paste_char_allowed(c) && (matches!(c, '\t' | '\n' | '\r') || aterm_grapheme::char_width(c) > 0)
}

impl Terminal {
    /// Whether a paste contains at least one authored character that survives
    /// the formatter's control-character sanitizer in a small bounded prefix.
    ///
    /// This deliberately ignores bracketed-paste wrapper bytes: an empty pair
    /// of wrappers carries protocol framing but cannot author a cursor landing,
    /// so it must not license a cursor-trail gesture. The scan allocates
    /// nothing, stops at the first ordinary character (the typical path), and
    /// examines at most [`PASTE_GESTURE_PROBE_BYTES`] bytes so an adversarial
    /// 16 MiB control-only clipboard cannot stall the UI thread. A payload
    /// hidden behind a larger control-only prefix still egresses normally but
    /// conservatively receives no trail licence.
    #[must_use]
    pub fn paste_has_payload(text: &str) -> bool {
        let text = bounded_paste_text(text);
        let mut end = text.len().min(PASTE_GESTURE_PROBE_BYTES);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].chars().any(paste_char_can_move)
    }

    /// Get a reference to the tiered scrollback storage, if attached.
    #[must_use]
    pub fn scrollback(&self) -> Option<&crate::scrollback::ScrollbackStorage> {
        self.grid.scrollback()
    }

    /// Get a mutable reference to the tiered scrollback storage, if attached.
    pub fn scrollback_mut(&mut self) -> Option<&mut crate::scrollback::ScrollbackStorage> {
        self.grid.scrollback_mut()
    }

    /// Drain deferred scrollback rows into attached tiered storage for all grids.
    ///
    /// The grid keeps recently scrolled rows in a lazy buffer for write-path
    /// efficiency. Diagnostics such as memory pressure need those rows promoted
    /// first so their byte accounting and watermarks reflect the full history.
    pub fn sync_scrollback_buffers(&mut self) {
        let _ = self.grid.scrollback_mut();
        if let Some(ref mut alt) = self.alt_grid {
            let _ = alt.scrollback_mut();
        }
    }

    /// Estimate total memory used by the terminal (grid + alt screen + scrollback).
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let mut total = self.grid.memory_used();
        if let Some(ref alt) = self.alt_grid {
            total += alt.memory_used();
        }
        total
    }

    /// Set the scrollback memory budget (bytes) for the main and alt grids.
    ///
    /// Returns the first enforcement error encountered, if any.
    pub fn set_memory_budget(
        &mut self,
        budget: usize,
    ) -> Result<(), aterm_scrollback::ScrollbackError> {
        let mut first_err = None;
        // The Grid-level setter also persists a request while the tiered store
        // is detached for off-thread reflow, then reconciles it on re-attach.
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

    /// Set the retained scrollback line limit (`None` = unlimited).
    ///
    /// Applied to the PRIMARY-content grid — the active grid, or the saved
    /// primary while the alt screen is up — because scrollback belongs to
    /// primary content: the alt buffer has none by xterm spec (its ring cap
    /// is 0 and must stay 0). The limit is ONE TOTAL retention count (audit
    /// E1): on tiered grids it caps ring + staged + store together (the store
    /// takes the remainder after the ring's share); ring-only grids re-cap
    /// the ring itself, evicting the oldest lines on shrink (see
    /// [`Grid::set_scrollback_line_limit`]).
    ///
    /// [`Grid::set_scrollback_line_limit`]: crate::grid::Grid::set_scrollback_line_limit
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
        // Keep the ACTIVE grid's offset valid too (it IS the primary when the
        // alt screen is down; a no-op re-clamp otherwise).
        self.grid.clamp_display_offset();
        // SELECTION CUSTODY Phase 4: shrinking the limit EVICTS the oldest lines with
        // no scroll and no damage band, so nothing else re-floors the selection —
        // this is the entry point `adjust_for_scroll` cannot serve.
        //
        // Only off the alt screen. While alt is up the eviction above hit
        // `self.alt_grid`, which holds the SAVED PRIMARY, whereas `text_selection`
        // addresses `self.grid` — the alt buffer. Those are unrelated coordinate
        // spaces: re-flooring here would clamp the live selection against a floor that
        // does not describe its grid. The saved primary's own selection is re-floored
        // when it is restored.
        if !self.modes.alternate_screen {
            let live_top_abs = self
                .grid
                .absolute_row_counter()
                .saturating_sub(u64::from(self.grid.rows()));
            self.text_selection
                .truncate_to_floor(self.grid.oldest_absolute_row(), live_top_abs);
        }
    }

    /// The retained scrollback line limit of the primary-content grid
    /// (`None` = unlimited). Getter twin of
    /// [`set_scrollback_line_limit`](Self::set_scrollback_line_limit).
    #[must_use]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        self.main_grid().scrollback_line_limit()
    }

    /// Highest scrollback watermark pressure across the main and alternate
    /// grids — the tiered stores' budget watermarks, plus the advisory RING
    /// byte watermark (audit E10a) when one is configured via
    /// [`set_ring_byte_watermark`](Self::set_ring_byte_watermark), so a
    /// ring-only "unlimited" terminal still reports memory pressure.
    #[must_use]
    pub fn scrollback_pressure_level(&self) -> crate::scrollback::WatermarkLevel {
        let mut level = self.grid.scrollback().map_or(
            crate::scrollback::WatermarkLevel::Green,
            aterm_scrollback::ScrollbackStorage::watermark_level,
        );
        level = level.max(self.grid.ring_watermark_level());
        if let Some(ref alt) = self.alt_grid {
            if let Some(scrollback) = alt.scrollback() {
                level = level.max(scrollback.watermark_level());
            }
        }
        level
    }

    /// Set the advisory RING byte watermark budget on the primary-content
    /// grid (audit E10a): with no tiered store and no line limit, retention
    /// is otherwise unbounded and silent — a configured budget makes
    /// [`scrollback_pressure_level`](Self::scrollback_pressure_level) report
    /// Yellow/Red as ring memory approaches it. Advisory only (no eviction).
    /// `None` (default) disables it.
    pub fn set_ring_byte_watermark(&mut self, budget: Option<usize>) {
        self.grid.set_ring_byte_watermark(budget);
    }

    /// Monotonic count of history lines LOST to non-user-requested truncation
    /// across the main and alternate grids (audit E10a): flood-backpressure
    /// staged-line drops, detached-reflow-window cap drops, and
    /// memory-pressure store evictions. The OUT-OF-BAND truncation signal —
    /// content never carries a sentinel line; hosts poll this and surface the
    /// loss in their own UI chrome.
    #[must_use]
    pub fn scrollback_truncated_lines(&self) -> u64 {
        let mut total = self.grid.truncated_lines();
        if let Some(ref alt) = self.alt_grid {
            total += alt.truncated_lines();
        }
        total
    }

    /// Clear all scrollback history (main and alt grids).
    ///
    /// Resets both the ring buffer scrollback (`total_lines`, `ring_head`)
    /// and all tiers (hot, warm, cold) of the tiered scrollback.
    /// Preserves live visible rows. Clears any active text selection
    /// since scrollback-anchored selection coordinates become dangling.
    pub fn clear_scrollback(&mut self) {
        self.grid.erase_scrollback();
        if let Some(ref mut alt) = self.alt_grid {
            alt.erase_scrollback();
        }
        self.text_selection.clear();
        // `erase_scrollback` above ran on BOTH grids, so the parked selection's
        // anchors dangle in exactly the same way. The parked grid's own `All` damage
        // would also reach the drain on the exit batch, but a selection that is
        // already known to be dangling should not stay alive until then, and a later
        // narrowing of that damage must not silently resurrect it.
        self.parked_text_selection.clear();
        // Clear shell integration marks and marks state that contain absolute
        // row numbers — these become dangling references after scrollback is
        // erased (#7667).
        self.shell.command_marks.clear();
        self.shell.output_blocks.clear();
        self.shell.current_block = None;
        self.shell.current_mark = None;
        self.marks_state.marks.clear();
        self.marks_state.annotations.clear();
    }

    /// Scroll display by delta lines.
    pub fn scroll_display(&mut self, delta: i32) {
        self.grid.scroll_display(delta);
    }

    /// Scroll to top of scrollback.
    pub fn scroll_to_top(&mut self) {
        self.grid.scroll_to_top();
    }

    /// Scroll to bottom (live content).
    pub fn scroll_to_bottom(&mut self) {
        self.grid.scroll_to_bottom();
    }

    /// Scroll the viewport so `target_abs_row` (an absolute row number, e.g. a
    /// command mark's `prompt_start_row`) sits at the top visible line, clamped
    /// to the retained history — the primitive behind prompt-to-prompt navigation.
    pub fn scroll_to_absolute_row(&mut self, target_abs_row: u64) {
        self.grid.scroll_to_absolute_row(target_abs_row);
    }

    /// Take pending response data.
    ///
    /// Returns any data accumulated in the response buffer (from DSR/DA
    /// responses) and clears the buffer. The returned data should be
    /// written to the PTY.
    ///
    /// Uses `clone()+clear()` instead of `mem::take()` to preserve the
    /// internal buffer's heap allocation. Subsequent `process()` calls
    /// reuse the existing capacity instead of re-allocating (#4073).
    ///
    /// Returns `None` if the response buffer is empty.
    #[must_use]
    pub fn take_response(&mut self) -> Option<Vec<u8>> {
        if self.transient.response_buffer.is_empty() {
            None
        } else {
            let data = self.transient.response_buffer.clone();
            self.transient.response_buffer.clear();
            Some(data)
        }
    }

    /// Expose response buffer capacity for test assertions (#4544).
    #[cfg(test)]
    #[must_use]
    pub fn response_buffer_capacity(&self) -> usize {
        self.transient.response_buffer.capacity()
    }

    /// Drain the edge-triggered BEL flag: returns `true` if a (rate-limited)
    /// BEL fired since the last drain, then clears it. Lets a poll-based host
    /// react to a bell without wiring the synchronous `bell_callback`.
    pub fn drain_bell(&mut self) -> bool {
        let pending = self.transient.bell_pending;
        self.transient.bell_pending = false;
        pending
    }

    /// Pop the oldest queued OSC app-event `(code, payload)`, or `None` when the
    /// queue is empty. Mirrors `take_response`'s drain contract for the
    /// structured OSC 52/7/133 payloads the host polls each frame.
    #[must_use]
    pub fn take_osc_event(&mut self) -> Option<(u32, String)> {
        self.transient.osc_events.pop_front()
    }

    /// Whether any OSC app-event is queued (cheap pre-check before draining).
    #[must_use]
    pub fn has_osc_events(&self) -> bool {
        !self.transient.osc_events.is_empty()
    }

    /// Check if there is pending response data.
    #[must_use]
    pub fn has_pending_response(&self) -> bool {
        !self.transient.response_buffer.is_empty()
    }

    /// Get the number of bytes in the response buffer.
    #[must_use]
    pub fn pending_response_len(&self) -> usize {
        self.transient.response_buffer.len()
    }

    /// Format text for pasting into the terminal.
    ///
    /// Strips terminal control bytes that can inject commands, converts line
    /// breaks to carriage returns for PTY input, and when bracketed paste mode
    /// is enabled wraps the body with the bracketed paste markers
    /// (`\x1b[200~` prefix and `\x1b[201~` suffix).
    ///
    /// This is useful for host applications that need to send paste data
    /// to the PTY in the correct format based on the terminal's current mode.
    ///
    /// # Example
    ///
    /// ```
    /// use aterm_core::terminal::Terminal;
    ///
    /// let mut term = Terminal::new(24, 80);
    ///
    /// // Without bracketed paste mode
    /// assert_eq!(term.format_paste("hello"), b"hello");
    ///
    /// // Enable bracketed paste mode
    /// term.process(b"\x1b[?2004h");
    /// assert_eq!(
    ///     term.format_paste("hello"),
    ///     b"\x1b[200~hello\x1b[201~"
    /// );
    /// ```
    #[must_use]
    pub fn format_paste(&self, text: &str) -> Vec<u8> {
        // Truncate at char boundary to prevent unbounded allocation (#7379).
        let text = bounded_paste_text(text);

        if self.modes.bracketed_paste {
            // Strip every non-printable control char except TAB and the line breaks
            // (\n/\r, normalized to CR just below). This covers ESC (which could inject
            // \x1b[201~ to end the bracket region early), the C1 controls 0x80-0x9F
            // (0x9B C1 CSI can also terminate it: \x9B201~), DEL, and the C0 signals
            // (ETX/Ctrl-C, EOT, SUB, …) a hostile clipboard could otherwise smuggle in.
            let sanitized: String = text.chars().filter(|&c| paste_char_allowed(c)).collect();
            // Convert newlines to CR: terminals expect CR for line breaks in
            // pasted text; LF alone moves the cursor down without returning
            // to column 0 (#7773).
            let sanitized = sanitized.replace("\r\n", "\r").replace('\n', "\r");
            let mut result = Vec::with_capacity(sanitized.len() + 12);
            result.extend_from_slice(b"\x1b[200~");
            result.extend_from_slice(sanitized.as_bytes());
            result.extend_from_slice(b"\x1b[201~");
            result
        } else {
            // Same sanitization OUTSIDE bracketed paste: strip every non-printable
            // control except TAB and the line breaks. Beyond ESC and the C1 controls
            // (0x9B CSI / 0x9D OSC / 0x90 DCS can inject commands with 8-bit controls
            // enabled, #7411), this also drops the C0 signal bytes (0x03 SIGINT, 0x04
            // EOF, 0x1a SUSP, …) a hostile clipboard could deliver to a non-bracketed
            // reader (REPL / `read` / cooked-mode app) to hijack the rest of the paste.
            let cleaned: String = text.chars().filter(|&c| paste_char_allowed(c)).collect();
            // Convert newlines to CR (#7773).
            cleaned
                .replace("\r\n", "\r")
                .replace('\n', "\r")
                .into_bytes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PASTE_BYTES, PASTE_GESTURE_PROBE_BYTES, Terminal};

    fn write_lines(t: &mut Terminal, start: usize, n: usize) {
        for i in start..start + n {
            t.process(format!("line-{i}\r\n").as_bytes());
        }
    }

    /// A ring-only terminal (`Terminal::new` — the wasm engines' shape) must
    /// honor `set_scrollback_line_limit` as its RETENTION bound: shrinking
    /// evicts the oldest lines immediately, growing lets history keep
    /// accumulating past the construction-time ring cap.
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

        // Shrink after the fact: immediate oldest-first truncation.
        t.set_scrollback_line_limit(Some(40));
        assert_eq!(t.grid().scrollback_lines(), 40);
        let oldest = t
            .grid()
            .get_history_line(0)
            .map(|l| l.to_string().trim_end().to_string());
        // 496 lines scrolled off (500 written − 4 that filled the screen);
        // keeping the newest 40 leaves line-456..line-495.
        assert_eq!(
            oldest.as_deref(),
            Some("line-456"),
            "oldest lines evicted, newest kept"
        );

        // Grow past the default 10k ring: retention actually extends.
        t.set_scrollback_line_limit(Some(20_000));
        write_lines(&mut t, 500, 11_000);
        assert_eq!(
            t.grid().scrollback_lines(),
            40 + 11_000,
            "a raised limit really lifts the old ring cap"
        );
    }

    /// The limit reaches the SAVED PRIMARY while the alt screen is up, and the
    /// alt buffer itself never grows scrollback (xterm spec: alt has none).
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

        // Scroll output inside the alt screen: still no alt scrollback.
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

    /// E10a: the advisory ring byte watermark folds into the ONE pressure
    /// query, and the out-of-band truncation counter aggregates grid + store
    /// loss — with nothing counted for user-requested limits.
    #[test]
    fn ring_watermark_and_truncation_counter_are_out_of_band() {
        use crate::scrollback::WatermarkLevel;
        let mut t = Terminal::new(5, 20);
        write_lines(&mut t, 0, 500);
        assert_eq!(
            t.scrollback_pressure_level(),
            WatermarkLevel::Green,
            "no store, no configured ring watermark → Green"
        );
        assert_eq!(t.scrollback_truncated_lines(), 0);

        // Configure a budget the ring already exceeds: pressure surfaces
        // through the SAME query hosts already poll.
        t.set_ring_byte_watermark(Some(1));
        assert_eq!(
            t.scrollback_pressure_level(),
            WatermarkLevel::Red,
            "ring bytes past the advisory budget read Red"
        );
        t.set_ring_byte_watermark(None);
        assert_eq!(t.scrollback_pressure_level(), WatermarkLevel::Green);

        // A user limit shrink evicts intentionally — never counted as loss.
        t.set_scrollback_line_limit(Some(10));
        assert_eq!(
            t.scrollback_truncated_lines(),
            0,
            "requested truncation is not loss"
        );
    }

    /// The whole point of the bracket guard: a paste planted with ESC[201~
    /// must not terminate the region early and have its tail run as
    /// keystrokes. ESC is stripped, so the only ESC[201~ on the wire is the
    /// final guard and the planted "[201~" is inert text.
    #[test]
    fn bracketed_paste_blocks_embedded_escape_terminator() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2004h");
        let out = term.format_paste("safe\x1b[201~rm -rf ~");
        assert_eq!(out, b"\x1b[200~safe[201~rm -rf ~\x1b[201~");
    }

    /// C1 CSI (0x9B) terminates the bracket region just like ESC[ when 8-bit
    /// controls are honored; it must be stripped too.
    #[test]
    fn bracketed_paste_blocks_c1_csi_terminator() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2004h");
        let out = term.format_paste("a\u{009B}201~b");
        assert_eq!(out, b"\x1b[200~a201~b\x1b[201~");
    }

    /// Without bracketed paste there is no guard at all, so ESC and C1
    /// controls must still be stripped to keep pasted text inert.
    #[test]
    fn unbracketed_paste_strips_escape_and_c1() {
        let term = Terminal::new(24, 80);
        assert_eq!(term.format_paste("a\x1b[31mb\u{009D}c"), b"a[31mbc");
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

    /// A hostile clipboard must not deliver C0 SIGNAL bytes (Ctrl-C 0x03, EOF 0x04,
    /// SUSP 0x1a, DEL 0x7f, …) to a NON-bracketed reader (a REPL / `read` / cooked-mode
    /// app), where they could interrupt/suspend a program mid-paste and let the shell
    /// run the rest as typed input. They are stripped; TAB and line breaks survive.
    #[test]
    fn unbracketed_paste_strips_c0_signal_bytes() {
        let term = Terminal::new(24, 80);
        assert_eq!(term.format_paste("a\x03b\x04c\x1ad\x08e\x7ff"), b"abcdef");
        // TAB is legitimate in pasted code; a trailing newline still becomes CR.
        assert_eq!(term.format_paste("a\tb\n"), b"a\tb\r");
    }

    /// Line breaks become CR for PTY input, in both modes (#7773).
    #[test]
    fn paste_converts_line_breaks_to_cr() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(term.format_paste("x\r\ny\nz"), b"x\ry\rz");
        term.process(b"\x1b[?2004h");
        assert_eq!(term.format_paste("x\r\ny\nz"), b"\x1b[200~x\ry\rz\x1b[201~");
    }

    /// BEL sets the edge-triggered flag; drain reads it once then clears it.
    #[test]
    fn drain_bell_reflects_real_bel_byte() {
        let mut term = Terminal::new(24, 80);
        assert!(!term.drain_bell(), "no bell before any BEL byte");
        term.process(b"\x07");
        assert!(term.drain_bell(), "BEL byte sets the flag");
        assert!(!term.drain_bell(), "flag is cleared after draining");
    }

    /// OSC 7 queues the REAL parsed cwd as an app-event AND updates the
    /// terminal's working directory (proving it reads real state, not a stub).
    /// A named URI host is preserved in the event's `//host/path` form (a UNC
    /// cwd must reach the embedder), while the terminal's own cwd keeps the
    /// plain local path.
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

        // The RFC 8089 local forms — empty host and "localhost" — keep the
        // bare path, byte-identical to the historical payload.
        term.process(b"\x1b]7;file:///var/log\x07");
        assert_eq!(term.take_osc_event(), Some((7, "/var/log".to_string())));
        term.process(b"\x1b]7;file://localhost/var/tmp\x07");
        assert_eq!(term.take_osc_event(), Some((7, "/var/tmp".to_string())));
    }

    /// OSC 133 marks queue compact REAL payloads (A carries the cursor
    /// row/col; D carries the parsed exit code).
    #[test]
    fn osc_133_marks_queue_real_payloads() {
        let mut term = Terminal::new(24, 80);
        // A → B → C → D is the accepted state machine.
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

    /// OSC 52 set queues the REAL decoded clipboard string (base64 "aGk=" → "hi").
    #[test]
    fn osc_52_set_queues_decoded_clipboard() {
        let mut term = Terminal::new(24, 80);
        // Clipboard write must be host-authorized (default posture is deny).
        term.authorize_clipboard_access(crate::terminal::ClipboardAccess::Write);
        term.process(b"\x1b]52;c;aGk=\x07");
        assert_eq!(term.take_osc_event(), Some((52, "hi".to_string())));
    }

    /// DEC mode 2031 (color-scheme update notifications) flips the real getter.
    #[test]
    fn mode_2031_toggles_color_scheme_getter() {
        let mut term = Terminal::new(24, 80);
        assert!(!term.report_color_scheme_enabled(), "defaults off");
        term.process(b"\x1b[?2031h");
        assert!(term.report_color_scheme_enabled(), "2031 set enables it");
        term.process(b"\x1b[?2031l");
        assert!(
            !term.report_color_scheme_enabled(),
            "2031 reset disables it"
        );
    }

    /// CUP positions the cursor; the grid exposes the REAL display-relative col/row.
    #[test]
    fn cursor_position_reads_real_grid_state() {
        let mut term = Terminal::new(24, 80);
        // CSI 5;10 H → row 5, col 10 (1-based) == (4, 9) 0-based.
        term.process(b"\x1b[5;10H");
        assert_eq!(term.grid().cursor().row, 4);
        assert_eq!(term.grid().cursor().col, 9);
    }

    /// cell_grapheme returns the REAL written character (base + combining mark).
    #[test]
    fn cell_grapheme_reads_real_cell() {
        let mut term = Terminal::new(24, 80);
        // 'e' + combining acute accent (U+0301).
        term.process("e\u{0301}".as_bytes());
        assert_eq!(term.cell_grapheme(0, 0).as_deref(), Some("e\u{0301}"));
        // Out-of-range yields None.
        assert_eq!(term.cell_grapheme(999, 0), None);
    }
}
