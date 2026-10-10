// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Buffer and scrollback API for [`Terminal`](super::Terminal): scrollback
//! access, memory management, viewport scrolling, the response buffer and
//! paste formatting.

use super::Terminal;

/// Maximum paste size in bytes (16 MiB). Pastes exceeding this are truncated
/// at a char boundary to prevent unbounded memory allocation.
const MAX_PASTE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum UTF-8 prefix inspected synchronously by the GUI's cursor-gesture
/// admission probe. Formatting still consumes the full bounded paste off the
/// event thread; admission is deliberately conservative beyond this prefix.
const PASTE_GESTURE_PROBE_BYTES: usize = 4 * 1024;
/// Maximum UTF-8 body the delivered-insert pricer ([`Terminal::paste_insert_cells`])
/// walks. A single-line body longer than this is wider than any row and wraps,
/// so it is reported as an unknown width.
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
/// authored-payload probe.
#[inline]
fn paste_char_allowed(c: char) -> bool {
    !(c.is_control() && c != '\t' && c != '\n' && c != '\r')
}

/// Whether one sanitizer-surviving character can advance or reposition the
/// cursor. Zero-width marks, variation selectors and joiners cannot land a
/// cursor on their own.
#[inline]
fn paste_char_can_move(c: char) -> bool {
    paste_char_allowed(c) && (matches!(c, '\t' | '\n' | '\r') || aterm_grapheme::char_width(c) > 0)
}

impl Terminal {
    /// Whether a paste contains an authored character that survives the
    /// formatter's control-character sanitizer, within a bounded prefix.
    ///
    /// Bracketed-paste wrapper bytes are ignored: framing alone cannot land a
    /// cursor, so it must not license a cursor-trail gesture. The scan
    /// allocates nothing, stops at the first ordinary character and examines at
    /// most [`PASTE_GESTURE_PROBE_BYTES`], so a 16 MiB control-only clipboard
    /// cannot stall the UI thread (a payload behind a longer control-only
    /// prefix still pastes, just without a trail licence).
    #[must_use]
    pub fn paste_has_payload(text: &str) -> bool {
        let text = bounded_paste_text(text);
        let mut end = text.len().min(PASTE_GESTURE_PROBE_BYTES);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].chars().any(paste_char_can_move)
    }

    /// How many cells a paste of `text` advances the caret on its own row,
    /// priced from the same sanitized body [`Self::format_paste_framed`] sends
    /// (control-character sanitizer, then grapheme display width under the
    /// terminal's ambiguous-width mode).
    ///
    /// `Some(cells)` for a single-line body; `None` when the width is unknowable
    /// from the text: a TAB, a line break, or a body longer than
    /// [`PASTE_INSERT_PRICE_BYTES`]. The host then licenses an unknown-width
    /// insert bounded by the cursor engine's gesture cap. The length is checked
    /// before any character is scanned.
    #[must_use]
    pub fn paste_insert_cells(text: &str, ambiguous_width_double: bool) -> Option<u16> {
        use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};

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
        // A base-less run of combining marks, selectors or joiners advances
        // nothing, even if grouped into an emoji-shaped cluster.
        if aterm_grapheme::str_width(&body) == 0 {
            return Some(0);
        }
        let shaping = TextShapingConfig {
            ambiguous_width: if ambiguous_width_double {
                AmbiguousWidth::Double
            } else {
                AmbiguousWidth::Single
            },
            ..TextShapingConfig::default()
        };
        Some(
            u16::try_from(
                aterm_grapheme::grapheme_width_with_config(&body, &shaping).display_width,
            )
            .unwrap_or(u16::MAX),
        )
    }

    /// Get a reference to the tiered scrollback storage, if attached.
    #[must_use]
    pub fn scrollback(&self) -> Option<&crate::scrollback::ScrollbackStorage> {
        self.grid.scrollback()
    }

    /// The tiered store holding this terminal's history, whichever grid slot it
    /// sits in: the scrollback twin of [`Self::main_grid`].
    ///
    /// [`Self::scrollback`] reads the active grid, which is right for a viewport
    /// but wrong for accounting or reclaiming history: while the alternate
    /// screen is up the primary grid and its tiered store are parked in
    /// `alt_grid`. Memory-pressure shedding must read history through this
    /// accessor.
    #[must_use]
    pub fn history_scrollback(&self) -> Option<&crate::scrollback::ScrollbackStorage> {
        self.main_grid().scrollback()
    }

    /// Reclaimable history bytes: the history store's budgeted bytes
    /// ([`Self::history_scrollback`]), measured after the history grid's
    /// lazily-deferred rows are promoted into the store so the figure is what
    /// an eviction pass will see, plus the OSC 8 payload the grid's ring still
    /// holds uncompressed ([`Grid::history_link_bytes`]), which a shed strips
    /// with the store. `None` when no tiered store is attached (a ring-only
    /// grid, or the store detached for an off-thread reflow).
    ///
    /// [`Grid::history_link_bytes`]: crate::grid::Grid::history_link_bytes
    pub fn history_scrollback_bytes(&mut self) -> Option<usize> {
        let grid = self.history_grid_mut();
        let store = grid.scrollback_mut()?.budgeted_memory_used();
        Some(store + grid.history_link_bytes())
    }

    /// Evict the history store down to `keep(budget)` bytes, then restore its
    /// own `budget` so the tab can keep growing history. Used by the GUI's
    /// memory-pressure shed and aggregate-cap lane.
    ///
    /// The lowered budget also lowers the history grid's link allowance (a
    /// quarter of it, [`Grid::history_link_budget`]), so the OSC 8 payload its
    /// ring holds uncompressed is stripped oldest-first with the store's
    /// eviction: `after` is at most `keep(budget)` plus that quarter.
    ///
    /// Acts on the store that holds the history, including while the alternate
    /// screen is up (see [`Self::history_scrollback`]), and reports the bytes
    /// measured before and after, as [`Self::history_scrollback_bytes`] counts
    /// them. Budget-setter errors are not surfaced: the hot tier may refuse to
    /// evict rows it is serving, and [`HistoryShed::freed`] reports what was
    /// actually given back.
    ///
    /// Returns `None` (and changes nothing) when no tiered store is attached.
    ///
    /// [`Grid::history_link_budget`]: crate::grid::Grid::history_link_budget
    pub fn shed_history_scrollback(
        &mut self,
        keep: impl FnOnce(usize) -> usize,
    ) -> Option<HistoryShed> {
        let grid = self.history_grid_mut();
        let store = grid.scrollback_mut()?;
        let budget = store.memory_budget();
        let before = store.budgeted_memory_used() + grid.history_link_bytes();
        // `set_memory_budget` applies to both grid slots, reaching the parked
        // primary while alt is up; the alt grid has no tiered store.
        let _ = self.set_memory_budget(keep(budget));
        let _ = self.set_memory_budget(budget);
        let after = self.history_scrollback().map_or(
            0,
            crate::scrollback::ScrollbackStorage::budgeted_memory_used,
        ) + self.main_grid().history_link_bytes();
        Some(HistoryShed { before, after })
    }

    /// Drain deferred scrollback rows into attached tiered storage for all
    /// grids, so byte accounting and watermarks reflect the full history.
    pub fn sync_scrollback_buffers(&mut self) {
        let _ = self.grid.scrollback_mut();
        if let Some(ref mut alt) = self.alt_grid {
            let _ = alt.scrollback_mut();
        }
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
    /// Applied to the primary-content grid (the active grid, or the saved
    /// primary while the alt screen is up): the alt buffer has no scrollback
    /// and its ring cap stays 0. The limit is one total retention count: on
    /// tiered grids it caps ring + staged + store together; ring-only grids
    /// re-cap the ring, evicting the oldest lines on shrink (see
    /// [`Grid::set_scrollback_line_limit`]).
    ///
    /// [`Grid::set_scrollback_line_limit`]: crate::grid::Grid::set_scrollback_line_limit
    ///
    /// This is `SelectionCustody`'s `Evict`: shrinking drops the oldest lines
    /// with no content scroll and no damage band, so the selection is re-floored
    /// here. Partial eviction clamps the head and records the loss; only a
    /// selection whose whole interval fell off is destroyed. Driven by
    /// `aterm_gui::selection_custody_conformance`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
        // Keep the active grid's offset valid too (it is the primary when the
        // alt screen is down; a no-op re-clamp otherwise).
        self.grid.clamp_display_offset();
        // Shrinking evicts the oldest lines with no scroll or damage band, so
        // re-floor the selection here. Only off the alt screen: there the
        // eviction hit the saved primary in `alt_grid`, while `text_selection`
        // addresses the alt buffer; the primary's selection is re-floored when
        // it is restored.
        if !self.modes.alternate_screen {
            let live_top_abs = self
                .grid
                .absolute_row_counter()
                .saturating_sub(u64::from(self.grid.rows()));
            self.text_selection
                .truncate_to_floor(self.grid.oldest_absolute_row(), live_top_abs);
        }
        // A vi cursor on an evicted line rests on the oldest line kept.
        self.vi_shift(0);
    }

    /// The retained scrollback line limit of the primary-content grid
    /// (`None` = unlimited). Getter twin of
    /// [`set_scrollback_line_limit`](Self::set_scrollback_line_limit).
    #[must_use]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        self.main_grid().scrollback_line_limit()
    }

    /// Highest scrollback watermark pressure across the main and alternate
    /// grids: the tiered stores' budget watermarks plus the advisory ring byte
    /// watermark (`set_ring_byte_watermark`), so a ring-only unlimited terminal
    /// still reports pressure.
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

    /// Set the advisory ring byte watermark on the primary-content grid: with
    /// no tiered store and no line limit, it makes
    /// [`scrollback_pressure_level`](Self::scrollback_pressure_level) report
    /// Yellow/Red as ring memory approaches it. Advisory only (no eviction);
    /// `None` disables it.
    #[cfg(test)]
    pub fn set_ring_byte_watermark(&mut self, budget: Option<usize>) {
        self.grid.set_ring_byte_watermark(budget);
    }

    /// Monotonic count of history lines lost to non-user-requested truncation
    /// across the main and alternate grids: flood-backpressure staged-line
    /// drops, detached-reflow-window cap drops and memory-pressure store
    /// evictions. Hosts surface it (`metrics`, `lines`). Flood drops are also
    /// marked in band with a dim `— aterm dropped N lines here …` row at each
    /// cut; see [`Grid::truncated_lines`](aterm_grid::Grid::truncated_lines).
    /// It also counts the lines of a quarantined (undecodable) store block,
    /// which stay in the scrollback line count as blank rows when older
    /// history sits before them, so the two can overlap.
    #[must_use]
    pub fn scrollback_truncated_lines(&self) -> u64 {
        let mut total = self.grid.truncated_lines();
        if let Some(ref alt) = self.alt_grid {
            total += alt.truncated_lines();
        }
        total
    }

    /// Monotonic count of history rows that crossed the image retention horizon
    /// across the main and alternate grids: rows that kept their text but lost
    /// their inline picture when the tiered store compressed them. Separate from
    /// [`scrollback_truncated_lines`](Self::scrollback_truncated_lines) because
    /// no line was lost.
    ///
    /// An inline image is kept only while its lines are held uncompressed, so a
    /// footprint ages out one row at a time from its top. Hosts poll this; the
    /// user's content is never marked.
    #[must_use]
    pub fn scrollback_image_rows_dropped(&self) -> u64 {
        let mut total = self.grid.image_rows_dropped_by_compression();
        if let Some(ref alt) = self.alt_grid {
            total += alt.image_rows_dropped_by_compression();
        }
        total
    }

    /// Clear all scrollback history (main and alt grids): the ring buffer and
    /// every tier of the tiered store. Visible rows are kept; the text selection
    /// is cleared, since its anchors would dangle.
    ///
    /// This destroys like `SelectionCustody`'s `WholesaleInvalidate` but is not
    /// its anchor: it is host-facing, and the VT producers (ED 3, RIS, Kitty
    /// unscroll) reach the selection through `post_process`'s
    /// `SelectionDamage::All` arm, where the anchor lives
    /// ([`Terminal::post_process`]).
    pub fn clear_scrollback(&mut self) {
        self.grid.erase_scrollback();
        if let Some(ref mut alt) = self.alt_grid {
            alt.erase_scrollback();
        }
        self.text_selection.clear();
        // `erase_scrollback` ran on both grids, so the parked selection dangles
        // too; clear it now rather than waiting for the exit batch's damage.
        self.parked_text_selection.clear();
        // Shell integration marks hold absolute row numbers that dangle once
        // scrollback is erased.
        self.shell.command_marks.clear();
        self.shell.output_blocks.clear();
        self.shell.current_block = None;
        self.shell.current_mark = None;
        self.marks_state.marks.clear();
        self.marks_state.annotations.clear();
        // A vi cursor in history rests on the screen.
        self.vi_shift(0);
    }

    /// Scroll display by delta lines.
    ///
    /// Press custody: one of the entry points that can raise `display_offset`,
    /// so it records [`crate::terminal::CustodyTransition::UserScroll`] on a
    /// rise and `SnapToLive` / `UserScrollTowardLive` on a fall
    /// (`Terminal::note_scroll_custody`). Recording here covers every caller,
    /// including the wheel's instant and glide paths.
    pub fn scroll_display(&mut self, delta: i32) {
        let before = self.grid.display_offset();
        self.grid.scroll_display(delta);
        self.note_scroll_custody(before);
    }

    /// Scroll to top of scrollback. Records `UserScroll` on a rise (see
    /// [`Self::scroll_display`]); nothing when already at the top.
    pub fn scroll_to_top(&mut self) {
        let before = self.grid.display_offset();
        self.grid.scroll_to_top();
        self.note_scroll_custody(before);
    }

    /// Scroll to bottom (live content).
    ///
    /// Not a custody recording site: the typing snap runs through here and
    /// records its own `TypingPress`. A non-typing return to live (End,
    /// `ScrollToBottom`, the paste and IME snaps) is [`Self::return_to_live`].
    pub fn scroll_to_bottom(&mut self) {
        self.grid.scroll_to_bottom();
    }

    /// Scroll to bottom as a non-typing user gesture (End, `ScrollToBottom`,
    /// the paste and IME snaps, the find bar's restore), recording
    /// [`crate::terminal::CustodyTransition::SnapToLive`] when the view moved.
    /// The highlight is left alone. Already at live it records nothing but
    /// still counts as a reader gesture ([`Self::reader_gesture_count`]).
    pub fn return_to_live(&mut self) {
        let before = self.grid.display_offset();
        self.grid.scroll_to_bottom();
        if before == 0 {
            self.reader_gestures = self.reader_gestures.wrapping_add(1);
        }
        self.note_scroll_custody(before);
    }

    /// Scroll the viewport so `target_abs_row` (an absolute row, e.g. a command
    /// mark's `prompt_start_row`) sits at the top line, clamped to retained
    /// history: the primitive behind prompt-to-prompt navigation. Records
    /// custody like [`Self::scroll_display`].
    pub fn scroll_to_absolute_row(&mut self, target_abs_row: u64) {
        let before = self.grid.display_offset();
        self.grid.scroll_to_absolute_row(target_abs_row);
        self.note_scroll_custody(before);
    }

    /// Take pending response data (DSR/DA replies) for the PTY and clear the
    /// buffer; `None` if empty. Clones and clears rather than `mem::take` so the
    /// buffer keeps its allocation.
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

    /// Expose response buffer capacity for test assertions.
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

    /// Format text for pasting into the terminal.
    ///
    /// Strips control bytes that could inject commands, converts line breaks to
    /// carriage returns, and with bracketed paste mode on wraps the body in
    /// `\x1b[200~` / `\x1b[201~`.
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
        self.format_paste_framed(text, self.modes.bracketed_paste)
    }

    /// [`Self::format_paste`] with the bracketed-paste decision supplied by the
    /// caller rather than read from the live mode; the body is identical either
    /// way.
    ///
    /// A queued paste is framed later than the moment a person was asked about
    /// it, and the program may flip DEC 2004 in between; a host that asked (the
    /// multi-line paste confirmation) frames by the mode the question was asked
    /// under. Passing `self.modes().bracketed_paste` reproduces
    /// [`Self::format_paste`].
    ///
    /// # Example
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
        // Truncate at char boundary to prevent unbounded allocation.
        let text = bounded_paste_text(text);

        if bracketed {
            // Strip every non-printable control except TAB and line breaks: ESC
            // (could inject \x1b[201~ to end the bracket early), C1 0x80-0x9F
            // (\x9B201~ too), DEL, and C0 signals (ETX, EOT, SUB, …).
            let sanitized: String = text.chars().filter(|&c| paste_char_allowed(c)).collect();
            // Line breaks become CR, as typed Enter would send.
            let sanitized = sanitized.replace("\r\n", "\r").replace('\n', "\r");
            let mut result = Vec::with_capacity(sanitized.len() + 12);
            result.extend_from_slice(b"\x1b[200~");
            result.extend_from_slice(sanitized.as_bytes());
            result.extend_from_slice(b"\x1b[201~");
            result
        } else {
            // The same sanitization outside bracketed paste: C1 controls can
            // inject commands with 8-bit controls enabled, and C0 signals (0x03,
            // 0x04, 0x1a, …) could hijack a non-bracketed reader mid-paste.
            let cleaned: String = text.chars().filter(|&c| paste_char_allowed(c)).collect();
            // Convert newlines to CR.
            cleaned
                .replace("\r\n", "\r")
                .replace('\n', "\r")
                .into_bytes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PASTE_BYTES, PASTE_GESTURE_PROBE_BYTES, PASTE_INSERT_PRICE_BYTES, Terminal};

    fn write_lines(t: &mut Terminal, start: usize, n: usize) {
        for i in start..start + n {
            t.process(format!("line-{i}\r\n").as_bytes());
        }
    }

    /// A ring-only terminal (`Terminal::new`, as the wasm engines use) honors
    /// `set_scrollback_line_limit` as its retention bound: shrinking evicts the
    /// oldest lines, growing lets history accumulate past the initial ring cap.
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

    /// The limit reaches the saved primary while the alt screen is up, and the
    /// alt buffer never grows scrollback.
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

    /// The advisory ring byte watermark folds into the pressure query, and the
    /// truncation counter aggregates grid and store loss, counting nothing for
    /// user-requested limits.
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
        // through the same query hosts already poll.
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

    /// A paste carrying ESC[201~ cannot end the bracket early: ESC is stripped,
    /// so the only ESC[201~ on the wire is the final guard.
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

    /// A supplied framing overrides the live mode in both directions, and only
    /// the guards move: the body is the same either way.
    #[test]
    fn a_supplied_framing_decides_the_guards_and_nothing_else() {
        let mut term = Terminal::new(24, 80);
        // 2004 off, framed bracketed anyway.
        assert_eq!(
            term.format_paste_framed("a\x1b[31mb\nc", true),
            b"\x1b[200~a[31mb\rc\x1b[201~"
        );
        term.process(b"\x1b[?2004h");
        // 2004 on, framed unbracketed anyway — same body, no guards.
        assert_eq!(
            term.format_paste_framed("a\x1b[31mb\nc", false),
            b"a[31mb\rc"
        );
        // Passing the live mode is `format_paste`, which is how a caller with no
        // captured answer keeps the write-time contract.
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

    /// C0 signal bytes (0x03, 0x04, 0x1a, DEL, …) are stripped from a
    /// non-bracketed paste; TAB and line breaks survive.
    #[test]
    fn unbracketed_paste_strips_c0_signal_bytes() {
        let term = Terminal::new(24, 80);
        assert_eq!(term.format_paste("a\x03b\x04c\x1ad\x08e\x7ff"), b"abcdef");
        // TAB is legitimate in pasted code; a trailing newline still becomes CR.
        assert_eq!(term.format_paste("a\tb\n"), b"a\tb\r");
    }

    /// Line breaks become CR for PTY input, in both modes.
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

    /// OSC 7 queues the parsed cwd as an app-event and updates the terminal's
    /// working directory. A named URI host is kept in the event's `//host/path`
    /// form, while the terminal's own cwd is the plain local path.
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

    /// OSC 133 marks queue compact real payloads (A carries the cursor
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

    /// OSC 52 set queues the real decoded clipboard string (base64 "aGk=" → "hi").
    #[test]
    fn osc_52_set_queues_decoded_clipboard() {
        let mut term = Terminal::new(24, 80);
        // Clipboard write must be host-authorized (default posture is deny).
        term.authorize_clipboard_access(crate::terminal::ClipboardAccess::Write);
        term.process(b"\x1b]52;c;aGk=\x07");
        assert_eq!(term.take_osc_event(), Some((52, "hi".to_string())));
    }

    /// A flood of large OSC 52 sets is held to the queue's byte budget, not
    /// 1024 x 64 KiB, and a late poll still sees the newest sets in order —
    /// the native hosts never drain this queue, so the bound is all there is.
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

    /// CUP positions the cursor; the grid exposes the real display-relative col/row.
    #[test]
    fn cursor_position_reads_real_grid_state() {
        let mut term = Terminal::new(24, 80);
        // CSI 5;10 H → row 5, col 10 (1-based) == (4, 9) 0-based.
        term.process(b"\x1b[5;10H");
        assert_eq!(term.grid().cursor().row, 4);
        assert_eq!(term.grid().cursor().col, 9);
    }

    /// cell_grapheme returns the real written character (base + combining mark).
    #[test]
    fn cell_grapheme_reads_real_cell() {
        let mut term = Terminal::new(24, 80);
        // 'e' + combining acute accent (U+0301).
        term.process("e\u{0301}".as_bytes());
        assert_eq!(term.cell_grapheme(0, 0).as_deref(), Some("e\u{0301}"));
        // Out-of-range yields None.
        assert_eq!(term.cell_grapheme(999, 0), None);
    }

    /// Insert width is priced from the sanitized single-line body as sent;
    /// `None` for a TAB, a line break or a body past the probe; zero for a
    /// base-less run of marks; controls stripped before pricing.
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
        // The ambiguous-width mode is the terminal's, priced the same way the
        // GUI prices a committed run.
        assert!(
            Terminal::paste_insert_cells("±", true) >= Terminal::paste_insert_cells("±", false)
        );
    }

    /// The echo anchor counts the pending-wrap column: a glyph printed at the
    /// last column with DECAWM on leaves the cursor on that column, and the
    /// anchor reports one past it (the end of the print run). A cursor address
    /// clears it; text followed by an explicit CUP reports the cursor column.
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
        // The wrap lands with the next glyph: anchor and cursor agree again.
        t.process(b"a");
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((1, 1)));
        assert_eq!((t.cursor().row, t.cursor().col), (1, 1));
        // Text to the last column, then a CUP to the next row: the anchor keeps
        // the print's end and the CUP moves only the cursor.
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

    /// The anchor's glyph is the run's last cell: the cell left of the echo
    /// anchor, read live in `row_cols_into`'s per-column convention. It tells a
    /// key's echo from zsh's i-search minibuffer, whose run ends on its fake
    /// cursor.
    #[test]
    fn print_anchor_glyph_is_the_run_s_last_cell() {
        let mut t = Terminal::new(6, 10);
        assert_eq!(t.print_anchor_glyph(), None, "nothing printed yet");
        t.process(b"p% ab");
        assert_eq!(t.print_anchor_glyph(), Some('b'));
        // zle's i-search refresh: down a row, `c_`, back up onto the match.
        t.process(b"\x1b[1B\r bck: c_\x1b[A\x1b[3G");
        assert_eq!(
            t.print_anchor().map(|(r, c, _)| (r, c)),
            Some((1, 8)),
            "the run ended one past the fake cursor"
        );
        assert_eq!((t.cursor().row, t.cursor().col), (0, 2));
        assert_eq!(t.print_anchor_glyph(), Some('_'), "…on the fake cursor");
        // The last column with the wrap deferred: the anchor is one past the
        // row, its glyph the one the caret stays parked on.
        t.process(b"\x1b[3;1H0123456789");
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((2, 10)));
        assert_eq!(t.print_anchor_glyph(), Some('9'));
        // A wide glyph's run ends on its continuation column.
        t.process("\x1b[4;1H中".as_bytes());
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((3, 2)));
        assert_eq!(t.print_anchor_glyph(), Some('\0'));
        // Erased after the print: the grid's truth now, a blank.
        t.process(b"\x1b[5;1Hxy\x1b[2K");
        assert_eq!(t.print_anchor_glyph(), Some(' '));
        // Scrolled back: the anchor is an active-grid coordinate, and so is
        // its glyph — never the history row the viewport shows there.
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
        // A shrink since the print leaves the anchor off the grid: unknown.
        t.scroll_display(-3);
        t.resize(3, 10);
        assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((5, 2)));
        assert_eq!(t.print_anchor_glyph(), None);
        // A reset has no anchor, and so no glyph.
        let mut u = Terminal::new(2, 4);
        u.process(b"ab");
        u.process(b"\x1bc");
        assert_eq!(u.print_anchor(), None);
        assert_eq!(u.print_anchor_glyph(), None);
    }
}

/// What one [`Terminal::shed_history_scrollback`] pass measured on the
/// history store: reclaimable bytes before the eviction and after the budget
/// was restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryShed {
    /// Reclaimable history bytes before the pass (lazy rows promoted first).
    pub before: usize,
    /// Reclaimable history bytes after the pass.
    pub after: usize,
}

impl HistoryShed {
    /// Bytes the pass actually gave back — zero when the store was already
    /// under the target, which is the common case under a WARN-tier shed.
    #[must_use]
    pub const fn freed(self) -> usize {
        self.before.saturating_sub(self.after)
    }
}
