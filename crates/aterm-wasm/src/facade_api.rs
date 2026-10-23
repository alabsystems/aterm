// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Shared renderer-independent JS facade for `aterm-wasm` and `aterm-gpu-web`
//! (mounted via `#[path]`).

use super::*;

/// Convert a display-relative row into a terminal-relative row.
#[inline]
fn display_row_to_terminal(term: &Terminal, display_row: i32) -> i32 {
    let offset = i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
    display_row.saturating_sub(offset)
}

/// Project the current selection into the visible viewport, clipping off-screen ends.
#[inline]
pub(crate) fn selection_range_for(
    term: &Terminal,
    rows: usize,
    cols: usize,
) -> Option<SelectionRange> {
    let last_col = (cols as u16).saturating_sub(1);
    let proj = term.text_selection().project_range(last_col)?;
    let offset = i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
    let rows_i = i32::try_from(rows).unwrap_or(i32::MAX);
    let start_disp = proj.start_row.saturating_add(offset);
    let end_disp = proj.end_row.saturating_add(offset);
    if end_disp < 0 || start_disp >= rows_i {
        return None;
    }
    let (start_y, start_x) = if start_disp < 0 {
        (0u16, 0u16)
    } else {
        (start_disp as u16, proj.start_col)
    };
    let (end_y, end_x) = if end_disp >= rows_i {
        (rows_i.saturating_sub(1) as u16, last_col)
    } else {
        (end_disp as u16, proj.end_col)
    };
    Some(SelectionRange {
        start_x,
        start_y,
        end_x,
        end_y,
    })
}

/// Expand renderer-independent JS methods onto `$terminal` under `$export`.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Feed raw PTY output bytes into the engine.
            pub fn process(&mut self, bytes: &[u8]) {
                self.term.process(bytes);
                self.pump_reflow_on_output();
            }

            /// Feed UTF-8 PTY output from a JS string into the engine.
            pub fn process_str(&mut self, s: &str) {
                self.term.process(s.as_bytes());
                self.pump_reflow_on_output();
            }

            /// [`Self::new`] from a registered primary font handle.
            #[allow(clippy::too_many_arguments)]
            pub fn new_registered(
                rows: u16,
                cols: u16,
                font_handle: u32,
                px: f32,
                fg: u32,
                bg: u32,
                cursor: u32,
                selection: u32,
            ) -> Result<$terminal, String> {
                let bytes = registered_font(font_handle)?;
                Self::new(rows, cols, &bytes, px, fg, bg, cursor, selection)
            }

            /// Authorize OSC 52 clipboard write events for draining via [`Self::take_osc_events`].
            pub const fn authorize_clipboard_write(&mut self) {
                self.term.authorize_clipboard_access(ClipboardAccess::Write);
            }

            /// Revoke OSC 52 clipboard write authorization, returning to fail-closed.
            pub const fn revoke_clipboard_write(&mut self) {
                self.term.revoke_clipboard_access(ClipboardAccess::Write);
            }

            /// Add a custom URI scheme to the OSC 8 hyperlink allowlist; returns `false` if refused.
            pub fn authorize_hyperlink_scheme(&mut self, scheme: &str) -> bool {
                self.term.authorize_hyperlink_scheme(scheme)
            }

            /// Remove a custom URI scheme from the OSC 8 hyperlink allowlist.
            pub fn revoke_hyperlink_scheme(&mut self, scheme: &str) {
                self.term.revoke_hyperlink_scheme(scheme);
            }

            /// Drain pending terminal query replies to forward to the PTY.
            pub fn take_response(&mut self) -> Option<Vec<u8>> {
                self.term.take_response()
            }

            /// Drain pending OSC app events as a JSON array of `[code, payload]` pairs.
            pub fn take_osc_events(&mut self) -> Option<String> {
                if !self.term.has_osc_events() {
                    return None;
                }
                let mut pairs = Vec::new();
                while let Some((code, payload)) = self.term.take_osc_event() {
                    pairs.push(format!("[{code},{}]", json_string(&payload)));
                }
                Some(format!("[{}]", pairs.join(",")))
            }

            /// Display-relative cursor column (0-based).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn cursor_x(&self) -> u16 {
                self.term.grid().cursor().col
            }

            /// Display-relative cursor row (0-based, top of viewport).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn cursor_y(&self) -> u16 {
                self.term.grid().cursor().row
            }

            /// Active OSC 12 cursor color as `0x00RRGGBB`, or `None` when following foreground.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn cursor_color(&self) -> Option<u32> {
                self.term
                    .cursor_color()
                    .map(|c| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b))
            }

            /// Absolute row index of the bottom live line (`oldest_absolute_row + scrollback_lines`).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn base_y(&self) -> usize {
                self.term.grid().base_y()
            }

            /// Absolute row index of the top visible viewport line (`base_y - display_offset`).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn display_origin_absolute(&self) -> usize {
                self.term.grid().display_origin_absolute()
            }

            /// Whether visible `row` is soft-wrapped from the previous row (`None` if out of range).
            pub fn row_is_wrapped(&self, row: u16) -> Option<bool> {
                self.term.grid().row_is_wrapped(row)
            }

            /// Logical length of visible `row` (`None` if out of range).
            pub fn row_len(&self, row: u16) -> Option<u16> {
                self.term.grid().row_len(row)
            }

            /// Grapheme cluster text at display cell `(row, col)` (empty for blank/spacer/out-of-range).
            pub fn cell_text(&self, row: u16, col: u16) -> String {
                self.with_display_row_cell(row, col, |(text, _)| text.clone())
                    .unwrap_or_default()
            }

            /// Whether display cell `(row, col)` is a double-width character (`None` if out of range).
            pub fn cell_is_wide(&self, row: u16, col: u16) -> Option<bool> {
                self.with_display_row_cell(row, col, |(_, wide)| *wide)
            }

            /// Drain the edge-triggered BEL flag.
            pub const fn drain_bell(&mut self) -> bool {
                self.term.drain_bell()
            }

            /// Advance Kitty graphics animations to now and return ms until the next frame (`None` if idle).
            pub fn advance_kitty_animations(&mut self) -> Option<f64> {
                let now = aterm_time::Instant::now();
                self.term
                    .advance_kitty_animations(now)
                    .next_frame_at
                    .map(|at| at.saturating_duration_since(now).as_secs_f64() * 1000.0)
            }

            /// Set device-pixel cell dimensions for CSI 14t/16t size reports.
            pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
                self.term.set_cell_pixel_size(width, height);
            }

            /// Set total retained scrollback line limit (`0` = unlimited, bounded by byte budgets).
            pub fn set_scrollback_limit(&mut self, lines: u32) {
                self.note_host_visual_change();
                let limit = if lines == 0 {
                    None
                } else {
                    Some(lines as usize)
                };
                self.term.set_scrollback_line_limit(limit);
            }

            /// Advance a deferred width-change scrollback rewrap by one bounded step.
            /// Returns `true` while work remains, `false` when complete or idle.
            pub fn pump_reflow(&mut self) -> bool {
                let Some(pending) = self.pending_reflow.take() else {
                    return false;
                };
                self.reflow_grace = 0;
                match pending.reflow_step(self.reflow_budget) {
                    ReflowStep::InProgress(job) => {
                        self.pending_reflow = Some(job);
                        true
                    }
                    ReflowStep::Done(reflowed) => match self.term.finish_resize_offload(reflowed) {
                        Some(follow) => {
                            self.pending_reflow = Some(follow);
                            true
                        }
                        None => false,
                    },
                }
            }

            /// Set the input-line budget per [`Self::pump_reflow`] step (`0` restores default).
            pub const fn pump_reflow_budget(&mut self, max_lines: u32) {
                self.reflow_budget = if max_lines == 0 {
                    REFLOW_STEP_BUDGET_LINES
                } else {
                    max_lines as usize
                };
            }

            /// True while a deferred scrollback rewrap is pending.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn reflow_pending(&self) -> bool {
                self.pending_reflow.is_some()
            }

            /// True when the last `render()` was skipped because nothing visual changed.
            pub const fn last_render_skipped(&self) -> bool {
                self.last_render_gated
            }

            /// Monotone revision of the spill-band RGBA buffer.
            pub const fn spill_rev(&self) -> u32 {
                self.spill.rev()
            }

            /// Number of dirty spill rects from the last `render()`.
            pub const fn spill_rect_count(&self) -> u32 {
                (self.spill.rects().len() / 4) as u32
            }

            /// Wasm linear-memory offset of the `[x, y, w, h]` `i32` dirty spill rects.
            pub fn spill_rects_ptr(&self) -> usize {
                self.spill.rects().as_ptr() as usize
            }

            /// Wasm linear-memory offset of the packed top/bottom/left/right RGBA spill strips.
            pub fn spill_ptr(&self) -> usize {
                self.spill.rgba().as_ptr() as usize
            }

            /// Byte length of the RGBA spill buffer (`0` when chrome is 0/0).
            pub const fn spill_len(&self) -> usize {
                self.spill.rgba().len()
            }

            /// Configure whether `HaloMode::Over` veils are included in the spill band.
            pub const fn set_spill_include_veils(&mut self, on: bool) {
                self.note_host_visual_change();
                self.spill.set_include_veils(on);
            }

            /// Scroll the viewport by `delta` lines (positive = older, negative = newer).
            pub fn scroll_lines(&mut self, delta: i32) {
                self.scroll_input.reset();
                self.term.scroll_display(delta);
            }

            /// Snap the viewport to the live bottom (latest output).
            pub fn scroll_to_bottom(&mut self) {
                self.scroll_input.reset();
                self.term.scroll_to_bottom();
            }

            /// Snap the viewport to the oldest retained scrollback line.
            pub fn scroll_to_top(&mut self) {
                self.scroll_input.reset();
                self.term.scroll_to_top();
            }

            /// Lines the viewport is scrolled up from the live bottom (0 = at bottom).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn display_offset(&self) -> usize {
                self.term.grid().display_offset()
            }

            /// True when the alternate screen buffer is active.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn is_alt_screen(&self) -> bool {
                self.term.is_alternate_screen()
            }

            /// True when DEC mode 1007 (alternate scroll) is active.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn is_alternate_scroll(&self) -> bool {
                self.term.modes().alternate_scroll()
            }

            /// True when DECCKM (application cursor keys) is active.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn is_app_cursor_mode(&self) -> bool {
                self.term.modes().application_cursor_keys()
            }

            /// True when any mouse-tracking mode (DECSET 9/1000/1002/1003) is active.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn is_mouse_tracking(&self) -> bool {
                self.term.mouse_tracking_enabled()
            }

            /// True when the active mouse mode reports drag or all motion (1002 or 1003).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn mouse_wants_motion(&self) -> bool {
                matches!(
                    self.term.mouse_mode(),
                    MouseMode::ButtonEvent | MouseMode::AnyEvent
                )
            }

            /// True when the active mouse mode reports button-less motion (1003).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn mouse_wants_any_motion(&self) -> bool {
                matches!(self.term.mouse_mode(), MouseMode::AnyEvent)
            }

            /// True when DECSET 1004 focus reporting is active.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn is_focus_event_mode(&self) -> bool {
                self.term.focus_reporting_enabled()
            }

            /// True when DEC mode 2031 color-scheme notifications are enabled.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn is_color_scheme_updates_mode(&self) -> bool {
                self.term.report_color_scheme_enabled()
            }

            /// Active DECSCUSR cursor style discriminant (`CursorStyle as u8`).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn cursor_style(&self) -> u8 {
                self.term.cursor_style() as u8
            }

            /// Set the default DECSCUSR cursor style (`1..=6`; out-of-range ignored).
            pub fn set_default_cursor_style(&mut self, n: u8) {
                self.note_host_visual_change();
                if let Some(style) = CursorStyle::from_param(u16::from(n)) {
                    self.term.set_default_cursor_style(style);
                }
            }

            /// Set host dark/light appearance and queue a DEC mode 2031 notification if subscribed.
            pub fn set_color_scheme(&mut self, dark: bool) {
                self.note_host_visual_change();
                let scheme = if dark {
                    aterm_types::Appearance::Dark
                } else {
                    aterm_types::Appearance::Light
                };
                self.follow_appearance(scheme);
            }

            /// Encode a mouse press at 0-based `(col, row)` for the active mouse mode.
            pub fn encode_mouse_press(
                &self,
                col: u16,
                row: u16,
                button: u8,
                mods: u8,
            ) -> Option<Vec<u8>> {
                self.term.encode_mouse_press(button, col, row, mods)
            }

            /// Encode a mouse release at `(col, row)` (`None` in X10 press-only mode).
            pub fn encode_mouse_release(
                &self,
                col: u16,
                row: u16,
                button: u8,
                mods: u8,
            ) -> Option<Vec<u8>> {
                self.term.encode_mouse_release(button, col, row, mods)
            }

            /// Encode mouse motion at `(col, row)` (`button = 3` when no button is held).
            pub fn encode_mouse_motion(
                &self,
                col: u16,
                row: u16,
                button: u8,
                mods: u8,
            ) -> Option<Vec<u8>> {
                self.term.encode_mouse_motion(button, col, row, mods)
            }

            /// Encode a vertical mouse wheel tick at `(col, row)` (`up = true` for wheel-up).
            pub fn encode_mouse_wheel(
                &self,
                col: u16,
                row: u16,
                up: bool,
                mods: u8,
            ) -> Option<Vec<u8>> {
                let dir = if up {
                    aterm_types::mouse::WheelDir::Up
                } else {
                    aterm_types::mouse::WheelDir::Down
                };
                self.term.encode_mouse_wheel(dir, col, row, mods)
            }

            /// Encode a DOM `KeyboardEvent` (`key`, `mods`, `event_type`, `base_layout_key`)
            /// under the active terminal keyboard mode.
            pub fn encode_key(
                &self,
                key: &str,
                mods: u8,
                event_type: u8,
                base_layout_key: Option<char>,
            ) -> Option<Vec<u8>> {
                aterm_types::keyboard::encode_dom_key(
                    key,
                    mods,
                    event_type,
                    base_layout_key,
                    self.term.keyboard_mode(),
                )
            }

            /// Enable or disable Kitty keyboard protocol support.
            pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
                self.term.set_kitty_keyboard_enabled(enabled);
            }

            /// Enable or disable Windows ConPTY win32-input-mode (`CSI ? 9001 h`).
            pub fn set_win32_input_enabled(&mut self, enabled: bool) {
                self.term.set_win32_input_enabled(enabled);
            }

            /// Raw `KeyboardMode` bitflags for off-thread key encoding via [`encode_key_with_mode`].
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn keyboard_mode_bits(&self) -> u32 {
                self.term.keyboard_mode().bits()
            }

            /// Begin a character selection at display `row`/`col` (clears any prior one).
            pub fn selection_start(&mut self, row: i32, col: u16) {
                self.note_host_visual_change();
                let row = display_row_to_terminal(&self.term, row);
                self.term.text_selection_mut().start_selection(
                    row,
                    col,
                    SelectionSide::Left,
                    SelectionType::Simple,
                );
            }

            /// Select the word or URL at display `(row, col)` and return its text.
            pub fn selection_word(&mut self, row: i32, col: u16) -> Option<String> {
                self.note_host_visual_change();
                let (start, last) =
                    match self
                        .term
                        .smart_word_at(row as usize, col as usize, &self.smart)
                    {
                        Some((s, e)) => (s as u16, e.saturating_sub(1).max(s) as u16),
                        None => (col, col),
                    };
                let term_row = display_row_to_terminal(&self.term, row);
                let sel = self.term.text_selection_mut();
                sel.start_selection(term_row, col, SelectionSide::Left, SelectionType::Semantic);
                sel.expand_semantic(start, last);
                sel.complete_selection();
                self.term.selection_to_string()
            }

            /// Select the full physical row at display `row` and return its text.
            pub fn selection_line(&mut self, row: i32, col: u16) -> Option<String> {
                self.note_host_visual_change();
                let _ = col;
                let row = display_row_to_terminal(&self.term, row);
                let max_col = (self.cols as u16).saturating_sub(1);
                let sel = self.term.text_selection_mut();
                sel.start_selection(row, 0, SelectionSide::Left, SelectionType::Lines);
                sel.expand_lines(max_col);
                sel.complete_selection();
                self.term.selection_to_string()
            }

            /// Extend the active selection to display `(row, col)`.
            pub fn selection_extend(&mut self, row: i32, col: u16) {
                let row = display_row_to_terminal(&self.term, row);
                let before = self.term.text_selection().clone();
                self.term
                    .text_selection_mut()
                    .update_selection(row, col, SelectionSide::Right);
                if *self.term.text_selection() != before {
                    self.note_host_visual_change();
                }
            }

            /// Finalize the selection (mouse released).
            pub fn selection_finish(&mut self) {
                self.note_host_visual_change();
                self.term.text_selection_mut().complete_selection();
            }

            /// Drop the current selection so the highlight clears on the next render.
            pub fn selection_clear(&mut self) {
                self.note_host_visual_change();
                self.term.text_selection_mut().clear();
            }

            /// Set custom word-separator characters for double-click selection (`None` = default).
            pub fn set_word_separators(&mut self, separators: Option<String>) {
                self.smart.set_word_separators(separators.as_deref());
            }

            /// The selected text, if any (`None` when the selection is empty).
            pub fn selection_text(&self) -> Option<String> {
                self.term.selection_to_string()
            }

            /// Active selection bounds clipped to the visible viewport (`None` if empty or off-screen).
            pub fn selection_range(&self) -> Option<SelectionRange> {
                selection_range_for(&self.term, self.rows, self.cols)
            }

            /// Detect an OSC 8 or smart-selection link under painted display cell `(row, col)`.
            pub fn link_at(&self, row: u16, col: u16) -> Option<LinkHit> {
                let (col, _) = self.term.painted_cell_logical(row, col);
                if self.term.grid().display_offset() == 0 {
                    if let Some(url) = self.term.hyperlink_at(row, col) {
                        let url = url.to_string();
                        let (s, e) = self.osc8_span(row, col);
                        return Some(LinkHit {
                            url,
                            start_col: s,
                            end_col: e,
                            kind: 0,
                        });
                    }
                }

                let (matched, sc, ec) =
                    self.term
                        .smart_word_text_at(row as usize, col as usize, &self.smart)?;
                let kind = classify(&matched);
                if kind == 3 {
                    return None;
                }
                Some(LinkHit {
                    url: matched,
                    start_col: sc as u16,
                    end_col: ec as u16,
                    kind,
                })
            }

            /// Text of display `row` (accounting for `display_offset`).
            pub fn row_text(&self, row: u16) -> Option<String> {
                self.term.display_row_text(row as usize)
            }

            /// Serialize viewport and optional scrollback rows (`None` = all, `Some(0)` = viewport) as ANSI.
            pub fn serialize(&self, scrollback_rows: Option<u32>) -> String {
                self.term.serialize_ansi(self.rows, scrollback_rows)
            }

            /// Serialize scrollback history (`max_rows = None` for all) as ANSI text with CRLF.
            pub fn serialize_scrollback(&self, max_rows: Option<u32>) -> String {
                self.term.serialize_scrollback_ansi(max_rows)
            }

            /// Last completed OSC 133 command block output as JSON (`ok` or `evicted`), or `None`.
            pub fn last_command_output(&self) -> Option<String> {
                let block = self.term.last_completed_block()?;
                match self.term.block_output_text(block) {
                    BlockText::Text(text) => {
                        let exit = block
                            .exit_code
                            .map_or_else(|| "null".to_owned(), |c| c.to_string());
                        Some(format!(
                            "{{\"status\":\"ok\",\"text\":{},\"exitCode\":{exit}}}",
                            json_string(&text)
                        ))
                    }
                    BlockText::Evicted => Some("{\"status\":\"evicted\"}".to_owned()),
                    BlockText::NotAvailable => None,
                }
            }

            /// Current window title (OSC 0/2), or `None` when empty.
            pub fn title(&self) -> Option<String> {
                let title = self.term.title();
                if title.is_empty() {
                    None
                } else {
                    Some(title.to_string())
                }
            }

            /// True when bracketed-paste mode (DECSET 2004) is active.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub const fn bracketed_paste_mode(&self) -> bool {
                self.term.modes().bracketed_paste()
            }

            /// Sanitize clipboard text and wrap with bracketed-paste guards when DECSET 2004 is active.
            pub fn format_paste(&self, text: &str) -> Vec<u8> {
                self.term.format_paste(text)
            }

            /// Search retained scrollback and viewport for `query`, returning flat `[abs_line, start_col, len]` triplets.
            pub fn search(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
            ) -> Vec<u32> {
                if query.is_empty() {
                    return Vec::new();
                }
                let Ok(results) =
                    self.term
                        .indexed_search()
                        .search_results_opts(query, case_sensitive, is_regex)
                else {
                    return Vec::new();
                };
                let mut out = Vec::with_capacity(results.matches.len() * 3);
                for m in &results.matches {
                    out.push(u32::try_from(m.line).unwrap_or(u32::MAX));
                    out.push(u32::try_from(m.start_col).unwrap_or(u32::MAX));
                    out.push(u32::try_from(m.len()).unwrap_or(u32::MAX));
                }
                out
            }

            /// Return match count and truncation status (`incomplete`) for a [`Self::search`] query.
            pub fn search_meta(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
            ) -> SearchMeta {
                if query.is_empty() {
                    return SearchMeta {
                        incomplete: false,
                        match_count: 0,
                    };
                }
                let Ok(results) =
                    self.term
                        .indexed_search()
                        .search_results_opts(query, case_sensitive, is_regex)
                else {
                    return SearchMeta {
                        incomplete: false,
                        match_count: 0,
                    };
                };
                SearchMeta {
                    incomplete: results.incomplete,
                    match_count: u32::try_from(results.matches.len()).unwrap_or(u32::MAX),
                }
            }

            /// Absolute row index of display row 0 when `display_offset == 0`.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn search_display_origin(&self) -> u32 {
                let grid = self.term.grid();
                let origin = grid
                    .oldest_absolute_row()
                    .saturating_add(grid.scrollback_lines() as u64);
                u32::try_from(origin).unwrap_or(u32::MAX)
            }

            /// Scroll the viewport so absolute `line` is visible near the top.
            pub fn scroll_search_line_into_view(&mut self, line: u32) {
                let grid = self.term.grid();
                let origin = grid
                    .oldest_absolute_row()
                    .saturating_add(grid.scrollback_lines() as u64);
                let scrollback = grid.scrollback_lines();
                let current = grid.display_offset();
                let want = origin.saturating_sub(u64::from(line));
                let want = (want as usize).min(scrollback);
                let delta = want as i64 - current as i64;
                if let Ok(delta) = i32::try_from(delta) {
                    self.term.scroll_display(delta);
                }
            }

            /// Resumable budgeted search scanning at most `row_budget` rows per call.
            /// Pass `resume_cursor: None` to start, and each step's `cursor` to continue.
            pub fn search_budgeted(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
                resume_cursor: Option<u64>,
                row_budget: u32,
            ) -> BudgetedSearchResult {
                let empty = || BudgetedSearchResult {
                    matches: Vec::new(),
                    complete: true,
                    cursor: None,
                    search_id: None,
                    reset: true,
                    incomplete_index: false,
                    lowest_retained_line: 0,
                    rows_fed: 0,
                    total_rows: 0,
                };
                if query.is_empty() {
                    self.term.cancel_budgeted_search();
                    return empty();
                }
                let Ok(step) = self.term.search_budgeted(
                    query,
                    case_sensitive,
                    is_regex,
                    resume_cursor,
                    row_budget as usize,
                ) else {
                    return empty();
                };
                let mut matches = Vec::with_capacity(step.results.matches.len() * 3);
                for m in &step.results.matches {
                    matches.push(u32::try_from(m.line).unwrap_or(u32::MAX));
                    matches.push(u32::try_from(m.start_col).unwrap_or(u32::MAX));
                    matches.push(u32::try_from(m.len()).unwrap_or(u32::MAX));
                }
                BudgetedSearchResult {
                    matches,
                    complete: step.complete,
                    cursor: step.cursor,
                    search_id: Some(step.search_id),
                    reset: step.reset,
                    incomplete_index: step.results.incomplete,
                    lowest_retained_line: u32::try_from(step.results.lowest_retained_line)
                        .unwrap_or(u32::MAX),
                    rows_fed: u32::try_from(step.rows_fed).unwrap_or(u32::MAX),
                    total_rows: u32::try_from(step.total_rows).unwrap_or(u32::MAX),
                }
            }

            /// Cancel any in-flight [`Self::search_budgeted`] state and free its partial index.
            pub fn search_budgeted_cancel(&mut self) {
                self.term.cancel_budgeted_search();
            }

            /// Free the cached full-content search index and any in-flight budgeted search.
            pub fn search_index_release(&mut self) {
                self.term.release_search_index();
            }

            /// Export `count` display rows from `first_row` as a JSON array of `{text, wrapped, len, widths?}`.
            pub fn row_range_json(&self, first_row: u32, count: u32) -> Option<String> {
                let rows = u32::try_from(self.rows).unwrap_or(u32::MAX);
                let cols = self.cols;
                let end = first_row.checked_add(count)?;
                if end > rows {
                    return None;
                }
                let mut out = String::from("[");
                for y in first_row..end {
                    if y != first_row {
                        out.push(',');
                    }
                    let row_u16 = u16::try_from(y).ok()?;
                    let text = self.term.display_row_text(y as usize).unwrap_or_default();
                    let wrapped = self.term.grid().row_is_wrapped(row_u16).unwrap_or(false);
                    let len = self
                        .term
                        .grid()
                        .row_len(row_u16)
                        .map_or(self.cols, usize::from);
                    let view = self.term.grid().visible_row_view(row_u16);
                    let mut widths = String::with_capacity(cols);
                    let mut any_wide = false;
                    for col in 0..cols {
                        let wide = u16::try_from(col)
                            .ok()
                            .and_then(|c| view.cell(c))
                            .is_some_and(|c| c.is_wide());
                        if wide {
                            widths.push('2');
                            any_wide = true;
                        } else {
                            widths.push('1');
                        }
                    }
                    out.push_str("{\"text\":");
                    out.push_str(&json_string(&text));
                    out.push_str(",\"wrapped\":");
                    out.push_str(if wrapped { "true" } else { "false" });
                    out.push_str(",\"len\":");
                    out.push_str(&len.to_string());
                    if any_wide {
                        out.push_str(",\"widths\":");
                        out.push_str(&json_string(&widths));
                    }
                    out.push('}');
                }
                out.push(']');
                Some(out)
            }

            /// Return a JSON search summary `{matches:[{absRow,col,len,snippet}], total, incomplete}`
            /// with `matches` capped to `max_matches` (`0` = uncapped).
            pub fn search_summary(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
                max_matches: u32,
            ) -> Option<String> {
                let empty = || Some("{\"matches\":[],\"total\":0,\"incomplete\":false}".to_owned());
                if query.is_empty() {
                    return empty();
                }
                let (matches, total, incomplete) = {
                    let Ok(results) =
                        self.term
                            .search_summary_results(query, case_sensitive, is_regex)
                    else {
                        return empty();
                    };
                    let total = results.matches.len();
                    let cap = if max_matches == 0 {
                        total
                    } else {
                        (max_matches as usize).min(total)
                    };
                    let matches: Vec<(u64, usize, usize)> = results.matches[..cap]
                        .iter()
                        .map(|m| (m.line as u64, m.start_col, m.len()))
                        .collect();
                    (matches, total, results.incomplete)
                };
                let mut out = String::from("{\"matches\":[");
                for (i, (abs, col, len)) in matches.iter().enumerate() {
                    if i != 0 {
                        out.push(',');
                    }
                    let snippet = self.term.abs_row_text(*abs).unwrap_or_default();
                    out.push_str("{\"absRow\":");
                    out.push_str(&abs.to_string());
                    out.push_str(",\"col\":");
                    out.push_str(&col.to_string());
                    out.push_str(",\"len\":");
                    out.push_str(&len.to_string());
                    out.push_str(",\"snippet\":");
                    out.push_str(&json_string(&snippet));
                    out.push('}');
                }
                out.push_str("],\"total\":");
                out.push_str(&total.to_string());
                out.push_str(",\"incomplete\":");
                out.push_str(if incomplete { "true" } else { "false" });
                out.push('}');
                Some(out)
            }
        }
    };
}

web_terminal_impl!(bind);

#[cfg(test)]
mod tests {
    use super::*;

    /// Extreme `i32` selection rows stay clipped to the viewport without wrapping.
    #[test]
    fn hostile_selection_rows_stay_offscreen_without_wrapping() {
        let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
        let mut t = WebTerminal::new(3, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
        for _ in 0..8 {
            t.process(b"line\r\n");
        }
        t.selection_start(i32::MAX, 0);
        t.selection_extend(i32::MAX, 7);
        t.scroll_lines(4);
        assert!(
            t.selection_range().is_none(),
            "a far-below row must stay below"
        );

        t.selection_start(i32::MIN, 0);
        t.selection_extend(i32::MAX, 7);
        t.selection_finish();
        let range = t.selection_range().expect("selection crosses the viewport");
        assert_eq!((range.start_y(), range.end_y()), (0, 2));
        assert_eq!((range.start_x(), range.end_x()), (0, 7));

        assert!(t.selection_line(i32::MIN, 0).is_none());
        assert!(t.selection_range().is_none());
        assert!(t.selection_word(i32::MIN, 0).is_none());
        assert!(t.selection_range().is_none());
    }
}
