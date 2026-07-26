// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cmd-F / Emacs-navigation find overlay: the in-progress find state (`SearchState`),
//! the absolute→
//! selection coordinate map (`map_matches`), and the `App`-side enter/recompute/
//! apply/step/exit methods (an inherent-impl split).
//!
//! The heavy lifting — a FULL-history trigram search built off the term lock and
//! cached across keystrokes — lives in [`crate::control::search_full_history`],
//! shared with the socket `search` verb so a live ⌘F and a scripted `search` over the
//! same content reuse one index. This module is the GUI adapter: it calls that helper,
//! maps the engine's ABSOLUTE match rows into the overlay's SELECTION coordinates, and
//! drives the current-match cursor + highlight.

use std::time::Instant;

use crate::{App, term_lock};
use aterm_core::search::{SearchDirection as EngineSearchDirection, SearchMatch};
use aterm_core::selection::{SelectionSide, SelectionType};

/// Direction of an incremental terminal search. Forward is the legacy Cmd-F
/// default; backward is selected by Cmd-R/Ctrl-R and makes every query recompute
/// land at/before the Emacs search point immediately (there is never a
/// wrong-direction flash).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SearchDirection {
    #[default]
    Forward,
    Backward,
}

impl SearchDirection {
    fn from_forward(forward: bool) -> Self {
        if forward {
            Self::Forward
        } else {
            Self::Backward
        }
    }

    #[cfg(test)]
    fn initial_index(self, match_count: usize) -> usize {
        match self {
            Self::Forward => 0,
            Self::Backward => match_count.saturating_sub(1),
        }
    }
}

/// In-progress Cmd-F find over the full live screen + scrollback history. Matches are
/// `(row, start_col, end_col)` in SELECTION coordinates (0..rows = live screen,
/// negative = scrollback); the current one is highlighted by setting the text
/// selection (the existing overlay — no renderer change) and scrolled into view.
#[derive(Default)]
pub(crate) struct SearchState {
    pub(crate) query: String,
    pub(crate) matches: Vec<(i32, u16, u16)>,
    /// Exact point-relative hit when it lies outside the capped batch. Keeping
    /// it separate avoids inserting/removing/shifting 100k elements on every
    /// truncated Cmd-S/Cmd-R repeat.
    point_match: Option<(i32, u16, u16)>,
    /// The grid `base_y()` the selection rows in [`Self::matches`] were computed against
    /// (see [`App::search_recompute`]). Selection rows are FRAME-relative but are consumed
    /// a frame or more later (apply + splice), so if concurrent PTY output scrolls the grid
    /// meanwhile, the apply/highlight paths re-anchor by `current_base_y − match_base_y` —
    /// keeping the match on the right line even while output streams into the search.
    pub(crate) match_base_y: i64,
    /// Terminal revision for top-anchored protected-footer insertions when
    /// [`Self::matches`] was computed. Such a piecewise row splice cannot be
    /// corrected by a single `base_y` delta, so output handling recomputes the
    /// active search whenever this revision changes.
    pub(crate) match_absolute_row_revision: u64,
    /// Terminal content generation captured with [`Self::matches`]. Ordinary
    /// streaming output changes this even when absolute-row coordinates remain
    /// uniform; the output wake marks the batch dirty so stale matches are never
    /// applied or accepted, then the next navigation/edit refreshes off-lock.
    pub(crate) match_content_seq: u64,
    /// The terminal changed since the current result batch was built.
    pub(crate) results_dirty: bool,
    pub(crate) current: usize,
    /// Direction used both by the next repeat and by incremental recomputes. In
    /// particular, typing after Cmd-R selects the anchored previous match
    /// atomically rather than selecting a forward match and correcting it in a
    /// second render pass.
    pub(crate) direction: SearchDirection,
    /// Match case (default off = case-insensitive). Toggled with `⌥⌘C` in find mode.
    pub(crate) case_sensitive: bool,
    /// Treat the query as a regular expression (default off = literal). Toggled with
    /// `⌥⌘R`; a malformed pattern sets [`Self::regex_error`] instead of matching.
    pub(crate) is_regex: bool,
    /// The last recompute's query was an INVALID regex (only reachable in regex mode).
    /// Distinguishes "your pattern is broken" from "zero hits" in the find bar.
    pub(crate) regex_error: bool,
    /// The search index evicted the oldest history before this query ran (scrollback
    /// deeper than the configured index cap), so a "no matches" means "none in the
    /// searched history", not "none anywhere". Copied from [`aterm_search::SearchResults`]
    /// `incomplete` each recompute so the find bar can qualify a zero-match honestly.
    pub(crate) truncated: bool,
    /// The viewport's `display_offset` when find was ENTERED, so a cancel (⎋/^G)
    /// restores the view you were looking at — the emacs `C-g` "abort back to where
    /// you started" contract. Paired with [`Self::origin_base_y`] to survive PTY
    /// output scrolling the grid mid-find.
    pub(crate) origin_display_offset: i32,
    /// The grid `base_y()` [`Self::origin_display_offset`] was captured against. If
    /// output streams in during the find, base_y advances by `delta`; the content the
    /// user was reading is then `origin_display_offset + delta` lines above the (new)
    /// bottom, so the cancel path re-anchors exactly like the match paths do.
    pub(crate) origin_base_y: i64,
    /// Protected-footer insertion revision captured with the origin viewport.
    /// If it changes while find is open there is no single offset delta that can
    /// restore the old view, so cancel leaves the current viewport in place.
    pub(crate) origin_absolute_row_revision: u64,
    /// Absolute search origin used for Emacs-style point anchoring. At the live
    /// bottom this is the terminal cursor; in a scrolled viewport it is the
    /// visible edge in the active search direction.
    pub(crate) anchor_absolute_row: i64,
    pub(crate) anchor_col: u16,
}

impl SearchState {
    /// Temporary native-title projection while find owns the chrome. Kept on
    /// the search state so warning expiry can restore the exact latest status
    /// without duplicating its formatting policy.
    pub(crate) fn window_title(&self) -> String {
        if self.query.is_empty() {
            "aterm — find:".to_string()
        } else if self.matches.is_empty() {
            format!("aterm — find: {} (no matches)", self.query)
        } else if self.truncated {
            format!(
                "aterm — find: {} ({}/{}+)",
                self.query,
                self.current + 1,
                self.matches.len()
            )
        } else {
            format!(
                "aterm — find: {} ({}/{})",
                self.query,
                self.current + 1,
                self.matches.len()
            )
        }
    }

    /// Pure next/previous-match cursor step with wraparound (no window/render
    /// side effects). `forward` advances toward later matches; both directions
    /// wrap. A no-op when there are no matches. This is the testable core of
    /// [`App::search_step`], which calls it and then re-applies the highlight.
    pub(crate) fn step(&mut self, forward: bool) {
        // Direction changes even with zero matches: typing the first query after
        // a reverse command must start from the bottom.
        self.direction = SearchDirection::from_forward(forward);
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.point_match = None;
        self.current = if forward {
            (self.current + 1) % n
        } else {
            (self.current + n - 1) % n
        };
        self.anchor_to_current();
    }

    /// The currently-highlighted match, or `None` when the find has no matches.
    pub(crate) fn current_match(&self) -> Option<(i32, u16, u16)> {
        self.point_match
            .or_else(|| self.matches.get(self.current).copied())
    }

    /// Select the first/last match at the Emacs search origin, wrapping at the
    /// buffer edge. Matches and anchors use absolute rows for stream-safe order.
    fn anchored_index(&self, matches: &[(i32, u16, u16)], base_y: i64) -> usize {
        if matches.is_empty() {
            return 0;
        }
        let anchor = (self.anchor_absolute_row, self.anchor_col);
        match self.direction {
            SearchDirection::Forward => {
                let index = matches
                    .partition_point(|&(row, col, _)| (base_y + i64::from(row), col) < anchor);
                if index == matches.len() { 0 } else { index }
            }
            SearchDirection::Backward => {
                let index = matches
                    .partition_point(|&(row, col, _)| (base_y + i64::from(row), col) <= anchor);
                if index == 0 {
                    matches.len() - 1
                } else {
                    index - 1
                }
            }
        }
    }

    /// Install one exact match without changing the capped batch allocation.
    /// Returns true when the exact hit was already represented by the batch.
    fn install_point_match(&mut self, point: (i32, u16, u16)) -> bool {
        match match_position(&self.matches, point) {
            Ok(position) => {
                self.matches[position] = point;
                self.current = position;
                self.point_match = None;
                true
            }
            Err(_) => {
                self.point_match = Some(point);
                self.current = match self.direction {
                    SearchDirection::Forward => self.matches.len().saturating_sub(1),
                    SearchDirection::Backward => 0,
                };
                false
            }
        }
    }

    fn anchor_to_current(&mut self) {
        if let Some((row, col, _)) = self.current_match() {
            self.anchor_absolute_row = self.match_base_y + i64::from(row);
            self.anchor_col = col;
        }
    }

    /// Repaint fingerprint of the find bar's DISPLAYED state — exactly the fields
    /// [`splice_find_bar`](crate::App::splice_find_bar) reads to draw the bar:
    /// the query text, the current-match index + match count, the case/regex
    /// toggles, and the regex-error/truncated readouts. Folded into
    /// [`RepaintKey`](crate::RepaintKey) so an interactive find edit that does not
    /// move the highlighted match still presents. FNV-1a over those fields —
    /// allocation-free and order-stable; the value never needs a "0 means hidden"
    /// marker because the key builders substitute `0` when there is no active find.
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |b: u8| {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for &b in self.query.as_bytes() {
            mix(b);
        }
        for word in [self.current as u64, self.matches.len() as u64] {
            for b in word.to_le_bytes() {
                mix(b);
            }
        }
        mix(u8::from(self.case_sensitive));
        mix(u8::from(self.is_regex));
        mix(u8::from(self.regex_error));
        mix(u8::from(self.truncated));
        mix(u8::from(self.results_dirty));
        if let Some((row, start, end)) = self.point_match {
            for byte in row.to_le_bytes() {
                mix(byte);
            }
            for byte in start.to_le_bytes().into_iter().chain(end.to_le_bytes()) {
                mix(byte);
            }
        }
        h
    }
}

#[cfg(test)]
thread_local! {
    static POINT_LOOKUP_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn match_position(matches: &[(i32, u16, u16)], point: (i32, u16, u16)) -> Result<usize, usize> {
    matches.binary_search_by(|&(row, col, _)| {
        #[cfg(test)]
        POINT_LOOKUP_COMPARISONS.with(|count| count.set(count.get().saturating_add(1)));
        (row, col).cmp(&(point.0, point.1))
    })
}

#[cfg(test)]
fn take_point_lookup_comparisons() -> usize {
    POINT_LOOKUP_COMPARISONS.with(|count| {
        let value = count.get();
        count.set(0);
        value
    })
}

/// Map engine [`SearchMatch`]es (ABSOLUTE rows, EXCLUSIVE end columns) to the find
/// overlay's SELECTION coordinates: `sel_row = abs_row − base_y` (0..rows = live
/// screen, negative = scrollback) with INCLUSIVE end columns, sorted top-to-bottom
/// then left-to-right so next/prev reads in visual order. `base_y` is the absolute
/// row of the top visible line (`grid().base_y()`) — display-offset-independent, so
/// the coordinates are valid against the bottom-snapped viewport the apply path
/// resets to. Rows or columns that fall outside the overlay's `i32`/`u16` range are
/// dropped rather than wrapped. Pure, so it is unit-testable.
///
/// The engine's columns are DISPLAY/cell columns (its `ColumnMap` counts a wide CJK
/// glyph as two), so they pass straight through — the mapped `(start, end)` index the
/// render grid and the selection directly, with no per-cell width adjustment here.
pub(crate) fn map_matches(matches: &[SearchMatch], base_y: i64) -> Vec<(i32, u16, u16)> {
    let mut out: Vec<(i32, u16, u16)> = matches
        .iter()
        .filter_map(|m| {
            let row = i32::try_from(i64::try_from(m.line).ok()? - base_y).ok()?;
            let start = u16::try_from(m.start_col).ok()?;
            // end_col is EXCLUSIVE; the selection wants an INCLUSIVE end. A non-empty
            // match always has end_col > start_col.
            let end = u16::try_from(m.end_col.saturating_sub(1)).ok()?;
            Some((row, start, end))
        })
        .collect();
    // The engine's hot path already emits visual order. Validate that in one linear
    // pass and pay O(k log k) only for a defensive out-of-order producer (including
    // older/socket-derived result sources), so ordinary incremental search remains O(k).
    if out
        .windows(2)
        .any(|pair| (pair[0].0, pair[0].1) > (pair[1].0, pair[1].1))
    {
        out.sort_unstable_by_key(|&(row, start, _)| (row, start));
    }
    out
}

impl App {
    fn search_stamp_mismatch(&self, wid: crate::WindowId) -> Option<(bool, bool)> {
        let ws = self.windows.get(&wid)?;
        let search = ws.search.as_ref()?;
        let terminal = ws.front_terminal()?;
        let term = term_lock(&terminal.term);
        Some((
            term.absolute_row_revision() != search.match_absolute_row_revision,
            term.content_seq() != search.match_content_seq,
        ))
    }

    fn invalidate_search_results(&mut self, wid: crate::WindowId, revision_stale: bool) {
        let term = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.front_terminal())
            .map(|terminal| terminal.term.clone());
        if let Some(ws) = self.windows.get_mut(&wid)
            && let Some(search) = ws.search.as_mut()
        {
            search.anchor_to_current();
            if revision_stale {
                match search.direction {
                    SearchDirection::Forward => {
                        search.anchor_absolute_row = i64::MIN;
                        search.anchor_col = 0;
                    }
                    SearchDirection::Backward => {
                        search.anchor_absolute_row = i64::MAX;
                        search.anchor_col = u16::MAX;
                    }
                }
            }
            search.results_dirty = true;
            if let Some(window) = &ws.os_window {
                window.request_redraw();
            }
        }
        if let Some(term) = term {
            term_lock(&term).text_selection_mut().clear();
        }
    }

    /// A user-level "Find" request (the Edit ▸ Find… menu item / a rebound Find
    /// action): focus native Settings search when that app owns the active tab;
    /// otherwise enter terminal find. The menu key equivalent fires ahead of
    /// `keyDown`, so this seam must make the content-type decision explicitly.
    pub(crate) fn find_requested(&mut self) {
        let native_settings = self.frontmost_window.and_then(|wid| {
            let (instance, _) = self.active_native_view(wid)?;
            (self.native_runtime.app(instance)?.kind() == crate::native_app::AppKind::Settings)
                .then_some(wid)
        });
        if let Some(wid) = native_settings {
            let _ = self.dispatch_native_event(
                wid,
                crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new("settings/search"),
                    value: None,
                }),
            );
        } else if self.front().is_some_and(|ws| ws.settings().is_some()) {
            self.settings_search_begin();
        } else {
            self.search_enter();
        }
    }

    /// Enter (or refresh) legacy Cmd-F find mode in the forward direction.
    pub(crate) fn search_enter(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_enter_impl_in(wid, true, true);
    }

    /// Enter (or refresh) terminal find in an explicit direction, seeding a fresh find
    /// from the app-STICKY
    /// match-case / regex toggles so reopening find keeps the mode you last left it in.
    /// The ORIGIN viewport (display_offset + the base_y it was read against) is captured
    /// here, BEFORE any recompute snaps to the bottom, so a cancel (⎋/^G) can put the
    /// user back on the exact content they were reading (see [`Self::search_cancel`]).
    pub(crate) fn search_enter_direction(&mut self, forward: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_enter_direction_in(wid, forward);
    }

    /// Window-targeted Emacs entry used by physical-key ownership. Focus may
    /// change after key-down; the whole press episode remains bound to `wid`.
    pub(crate) fn search_enter_direction_in(&mut self, wid: crate::WindowId, forward: bool) {
        self.search_enter_impl_in(wid, forward, false);
    }

    /// Shared entry path. Legacy Cmd-F deliberately starts at the beginning of
    /// the buffer; Emacs Cmd-S/Cmd-R start at point (cursor or viewport edge).
    fn search_enter_impl_in(
        &mut self,
        wid: crate::WindowId,
        forward: bool,
        legacy_from_start: bool,
    ) {
        let (case_sensitive, is_regex) = (self.search_sticky_case, self.search_sticky_regex);
        let direction = SearchDirection::from_forward(forward);
        let origin = self
            .front_terminal(wid)
            .map(|t| t.term.clone())
            .map(|term| {
                let terminal = term_lock(&term);
                let display_offset = i32::try_from(terminal.grid().display_offset()).unwrap_or(0);
                let base_y = i64::try_from(terminal.grid().base_y()).unwrap_or(0);
                let top = base_y.saturating_sub(i64::from(display_offset));
                let (anchor_absolute_row, anchor_col) = if display_offset == 0 {
                    let cursor = terminal.cursor();
                    (base_y.saturating_add(i64::from(cursor.row)), cursor.col)
                } else if forward {
                    (top, 0)
                } else {
                    (
                        top.saturating_add(i64::from(terminal.rows().saturating_sub(1))),
                        u16::MAX,
                    )
                };
                (
                    display_offset,
                    base_y,
                    terminal.absolute_row_revision(),
                    anchor_absolute_row,
                    anchor_col,
                )
            });
        if let Some(ws) = self.windows.get_mut(&wid) {
            let (
                origin_display_offset,
                origin_base_y,
                origin_absolute_row_revision,
                anchor_absolute_row,
                anchor_col,
            ) = origin.unwrap_or((0, 0, 0, 0, 0));
            if let Some(search) = ws.search.as_mut() {
                search.direction = direction;
                if legacy_from_start {
                    search.anchor_absolute_row = i64::MIN;
                    search.anchor_col = 0;
                }
            } else {
                ws.search = Some(SearchState {
                    direction,
                    case_sensitive,
                    is_regex,
                    origin_display_offset,
                    origin_base_y,
                    origin_absolute_row_revision,
                    anchor_absolute_row: if legacy_from_start {
                        i64::MIN
                    } else {
                        anchor_absolute_row
                    },
                    anchor_col: if legacy_from_start { 0 } else { anchor_col },
                    ..SearchState::default()
                });
            }
        }
        self.search_recompute_in(wid);
    }

    /// Toggle match-case (⌥⌘C / a click on the `Aa` indicator): flip the active find's
    /// flag, remember it as the app-sticky default, and re-run. No-op when not in find
    /// mode. The regex twin is [`Self::search_toggle_regex`].
    pub(crate) fn search_toggle_case(&mut self) {
        let Some(now) = self.front_mut().and_then(|ws| ws.search.as_mut()).map(|s| {
            s.case_sensitive = !s.case_sensitive;
            s.case_sensitive
        }) else {
            return;
        };
        self.search_sticky_case = now;
        self.search_recompute();
    }

    /// Toggle regex mode (⌥⌘R / a click on the `.*` indicator): flip the active find's
    /// flag, remember it as the app-sticky default, and re-run. No-op when not in find
    /// mode. The case twin is [`Self::search_toggle_case`].
    pub(crate) fn search_toggle_regex(&mut self) {
        let Some(now) = self.front_mut().and_then(|ws| ws.search.as_mut()).map(|s| {
            s.is_regex = !s.is_regex;
            s.is_regex
        }) else {
            return;
        };
        self.search_sticky_regex = now;
        self.search_recompute();
    }

    /// Re-run the find for the current query over the FULL live screen + scrollback
    /// history, then show the first directional match at the Emacs search point.
    ///
    /// Delegates the search to [`crate::control::search_full_history`] — the
    /// off-lock, chunked, cached trigram index shared with the socket `search` verb —
    /// so a keystroke over unchanged content reuses the immutable index (then validates
    /// its generation under one short lock), while ordinary output incrementally
    /// refreshes the prior visible rows plus appended rows. Coordinate revisions and
    /// other unsafe-to-reuse changes rebuild only the retained suffix. The engine returns matches keyed by
    /// ABSOLUTE row; [`map_matches`] converts them to selection coordinates against
    /// `base_y`, and that `base_y` is stashed as [`SearchState::match_base_y`] so the apply
    /// and highlight paths can re-anchor the (frame-relative) rows if output scrolls the
    /// grid before they run. `incomplete` (history deeper than the index cap) becomes
    /// [`SearchState::truncated`]; an invalid regex sets `regex_error`.
    pub(crate) fn search_recompute(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_recompute_in(wid);
    }

    pub(crate) fn search_recompute_in(&mut self, wid: crate::WindowId) {
        self.search_recompute_from_anchor_in(wid, false);
    }

    fn search_recompute_from_anchor(&mut self, strict: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_recompute_from_anchor_in(wid, strict);
    }

    fn search_recompute_from_anchor_in(&mut self, wid: crate::WindowId, strict: bool) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let (query, case_sensitive, is_regex, direction, anchor, anchor_is_buffer_edge) =
            match &ws.search {
                Some(s) => (
                    s.query.clone(),
                    s.case_sensitive,
                    s.is_regex,
                    s.direction,
                    (
                        usize::try_from(s.anchor_absolute_row).unwrap_or(
                            if s.anchor_absolute_row < 0 {
                                0
                            } else {
                                usize::MAX
                            },
                        ),
                        usize::from(s.anchor_col),
                    ),
                    matches!(
                        (s.direction, s.anchor_absolute_row, s.anchor_col),
                        (SearchDirection::Forward, i64::MIN, 0)
                            | (SearchDirection::Backward, i64::MAX, u16::MAX)
                    ),
                ),
                None => return,
            };
        // (matches, regex_error, truncated, base_y): an empty query is the neutral "bar
        // open, nothing typed" state (no error, no truncation readout).
        let (
            matches,
            point_match,
            regex_error,
            truncated,
            base_y,
            absolute_row_revision,
            content_seq,
            consistent,
        ) = if query.is_empty() {
            let terminal = term_lock(&term);
            (
                Vec::new(),
                None,
                false,
                false,
                i64::try_from(terminal.grid().base_y()).unwrap_or(i64::MAX),
                terminal.absolute_row_revision(),
                terminal.content_seq(),
                true,
            )
        } else {
            // Snap to the bottom before searching. The search result carries
            // the exact base_y + protected-footer revision captured with its
            // own index key, so a splice between these two locks cannot mis-tag
            // coordinates from one grid state as another.
            {
                let mut terminal = term_lock(&term);
                terminal.scroll_to_bottom();
            }
            let engine_direction = match direction {
                SearchDirection::Forward => EngineSearchDirection::Forward,
                SearchDirection::Backward => EngineSearchDirection::Backward,
            };
            let run = || {
                crate::control::search_full_history_direction(
                    &term,
                    &query,
                    case_sensitive,
                    is_regex,
                    engine_direction,
                    Some(anchor),
                    strict && !anchor_is_buffer_edge,
                )
            };
            let result =
                run().and_then(|search| if search.consistent { Ok(search) } else { run() });
            match result {
                Ok(search) => (
                    map_matches(&search.results.matches, search.base_y),
                    search.point_match.as_ref().and_then(|point| {
                        map_matches(std::slice::from_ref(point), search.base_y).pop()
                    }),
                    false,
                    search.results.incomplete,
                    search.base_y,
                    search.absolute_row_revision,
                    search.content_seq,
                    search.consistent,
                ),
                // The only error is an invalid regex (literal search never errors) → no
                // hits, and the bar flags "bad regex" instead of a misleading "no matches".
                Err(_) => {
                    let terminal = term_lock(&term);
                    (
                        Vec::new(),
                        None,
                        true,
                        false,
                        i64::try_from(terminal.grid().base_y()).unwrap_or(i64::MAX),
                        terminal.absolute_row_revision(),
                        terminal.content_seq(),
                        true,
                    )
                }
            }
        };
        if !consistent {
            if let Some(search) = ws.search.as_mut() {
                search.results_dirty = true;
            }
            self.search_apply_current_in(wid);
            return;
        }
        if let Some(s) = ws.search.as_mut() {
            s.matches = matches;
            s.point_match = None;
            s.match_base_y = base_y;
            s.match_absolute_row_revision = absolute_row_revision;
            s.match_content_seq = content_seq;
            s.results_dirty = false;
            s.regex_error = regex_error;
            s.truncated = truncated;
            if let Some(point) = point_match {
                s.install_point_match(point);
            } else {
                s.current = s.anchored_index(&s.matches, base_y);
            }
            s.anchor_to_current();
        }
        self.search_apply_current_in(wid);
    }

    /// Invalidate an open search when the focused terminal emits output.
    ///
    /// A protected-footer splice changes absolute coordinates piecewise; all
    /// mutations mark the result batch dirty and clear its selection. The next
    /// search edit/repeat rebuilds once from the latest generation, avoiding one
    /// expensive scan per output chunk while guaranteeing stale coordinates are
    /// never navigated.
    pub(crate) fn search_refresh_for_output(&mut self, session: u64) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let Some((expected_revision, expected_content_seq)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map(|search| (search.match_absolute_row_revision, search.match_content_seq))
        else {
            return;
        };
        let Some(terminal) = self.front_terminal(wid) else {
            return;
        };
        if terminal.session != session {
            return;
        }
        // TRY-lock, never block. This runs from the `Wake::Output` handler, i.e. once
        // per PTY reader BATCH — at flood rate, on the UI thread. A blocking acquire
        // here parks the main thread behind the reader's whole-batch `process()` hold
        // for every burst, on the very mutex the keystroke echo's present also needs;
        // with a find bar open during a flood that is unbounded main-thread starvation.
        // Skipping one refresh is invisible (a stale search invalidation is corrected
        // by the next burst, and `match_content_seq` still catches the drift); a
        // blocked event loop is not. Mirrors the try_lock discipline the title
        // observer already uses on this same path.
        let Ok(term) = terminal.term.try_lock() else {
            return;
        };
        let revision_stale = term.absolute_row_revision() != expected_revision;
        let content_stale = term.content_seq() != expected_content_seq;
        drop(term);
        if revision_stale || content_stale {
            self.invalidate_search_results(wid, revision_stale);
        }
    }

    /// Highlight the current match via the text selection (the existing overlay —
    /// no renderer change), scroll it into view, and show the find state in the
    /// window title.
    fn search_apply_current_in(&mut self, wid: crate::WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let Some(terminal) = ws.front_terminal() else {
            return;
        };
        let rows = ws.rows;
        let (mat, match_base_y, match_revision, match_seq, dirty, search_title) = match &ws.search {
            Some(s) => (
                s.current_match(),
                s.match_base_y,
                s.match_absolute_row_revision,
                s.match_content_seq,
                s.results_dirty,
                s.window_title(),
            ),
            None => return,
        };
        {
            let mut term = term_lock(&terminal.term);
            if dirty
                || term.absolute_row_revision() != match_revision
                || term.content_seq() != match_seq
            {
                // A protected-footer insertion is piecewise: applying this stale
                // match with a uniform base_y delta could select unrelated text;
                // ordinary edits can invalidate the columns. Fail closed until
                // the output wake/next navigation refreshes the batch.
                term.text_selection_mut().clear();
                return;
            }
            term.scroll_to_bottom(); // reset to display_offset = 0 (stable coords)
            // Re-anchor the stored (frame-relative) selection row to THIS frame: if output
            // scrolled the grid since the search, base_y advanced by `delta`, so the match's
            // current row is `stored_row − delta`. Read base_y under this same lock so the
            // selection + scroll always land on the right line, even mid-stream.
            let base_y = i64::try_from(term.grid().base_y()).unwrap_or(0);
            let delta = base_y - match_base_y;
            let row = mat.and_then(|(r, _, _)| i32::try_from(i64::from(r) - delta).ok());
            {
                let sel = term.text_selection_mut();
                sel.clear();
                if let (Some(row), Some((_, c0, c1))) = (row, mat) {
                    sel.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
                    sel.update_selection(row, c1, SelectionSide::Right);
                }
            }
            // A scrollback match (re-anchored row < 0) is scrolled into view — landing on
            // row 1, one line BELOW the top, because the find bar rides the TOP row
            // (`splice_find_bar`): a row-0 landing would put every scrollback match under
            // the bar and bounce it to the bottom on each step. At the very oldest history
            // line there is nothing left to scroll past, so `scroll_display` clamps, the
            // match sits at row 0, and the bar's adaptive placement floats to the bottom
            // instead. A 1-row terminal has no "below the top" — land at 0.
            if let Some(row) = row
                && row < 0
            {
                term.scroll_display(-row + i32::from(rows > 1));
            }
        }
        let warning_armed = ws
            .close_warning_until
            .is_some_and(|deadline| Instant::now() < deadline);
        if crate::app_window::window_title_authority(warning_armed, true)
            == crate::app_window::WindowTitleAuthority::Search
            && let Some(w) = &ws.os_window
        {
            w.set_title(&search_title);
            w.request_redraw();
        }
    }

    fn search_step_truncated_in(&mut self, wid: crate::WindowId, forward: bool) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some((query, case_sensitive, is_regex, anchor_row, anchor_col)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map(|search| {
                (
                    search.query.clone(),
                    search.case_sensitive,
                    search.is_regex,
                    search.anchor_absolute_row,
                    search.anchor_col,
                )
            })
        else {
            return;
        };
        let anchor = (
            usize::try_from(anchor_row).unwrap_or(if anchor_row < 0 { 0 } else { usize::MAX }),
            usize::from(anchor_col),
        );
        let direction = if forward {
            EngineSearchDirection::Forward
        } else {
            EngineSearchDirection::Backward
        };
        let Ok(point) = crate::control::search_full_history_point(
            &term,
            &query,
            case_sensitive,
            is_regex,
            direction,
            anchor,
            true,
        ) else {
            return;
        };
        if !point.consistent {
            let revision_stale = self
                .search_stamp_mismatch(wid)
                .is_some_and(|(revision, _)| revision);
            self.invalidate_search_results(wid, revision_stale);
            return;
        }
        let mapped = point
            .point_match
            .as_ref()
            .and_then(|found| map_matches(std::slice::from_ref(found), point.base_y).pop());
        if let Some(search) = self.windows.get_mut(&wid).and_then(|ws| ws.search.as_mut()) {
            search.match_base_y = point.base_y;
            search.match_absolute_row_revision = point.absolute_row_revision;
            search.match_content_seq = point.content_seq;
            search.results_dirty = false;
            if let Some(mapped) = mapped {
                search.install_point_match(mapped);
                search.anchor_to_current();
            } else {
                search.matches.clear();
                search.point_match = None;
                search.current = 0;
            }
        }
        self.search_apply_current_in(wid);
    }

    /// Cycle to the next (`forward`) / previous match, wrapping.
    pub(crate) fn search_step(&mut self, forward: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_step_in(wid, forward);
    }

    pub(crate) fn search_step_in(&mut self, wid: crate::WindowId, forward: bool) {
        if let Some(search) = self.windows.get_mut(&wid).and_then(|ws| ws.search.as_mut()) {
            search.direction = SearchDirection::from_forward(forward);
            search.anchor_to_current();
        }
        if let Some((revision_stale, content_stale)) = self.search_stamp_mismatch(wid)
            && (revision_stale || content_stale)
        {
            self.invalidate_search_results(wid, revision_stale);
        }
        let Some((dirty, truncated)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map(|search| (search.results_dirty, search.truncated))
        else {
            return;
        };
        if dirty {
            self.search_recompute_from_anchor_in(wid, true);
            return;
        }
        if truncated {
            self.search_step_truncated_in(wid, forward);
            return;
        }
        if let Some(ws) = self.windows.get_mut(&wid)
            && let Some(s) = ws.search.as_mut()
        {
            s.step(forward);
        }
        self.search_apply_current_in(wid);
    }

    /// Standard Edit ▸ Find Next/Previous (Cmd-G / Shift-Cmd-G). While the bar
    /// is open this is an ordinary step. After Enter accepted and closed it,
    /// reopen the last accepted query and resume strictly after/before its
    /// absolute match anchor, wrapping when content changed or an edge is hit.
    pub(crate) fn search_find_again(&mut self, forward: bool) {
        if self.front().is_some_and(|ws| ws.search.is_some()) {
            self.search_step(forward);
            return;
        }
        if self.search_last_query.is_empty() {
            return;
        }

        let query = self.search_last_query.clone();
        let session_revision = self
            .frontmost_window
            .and_then(|wid| self.front_terminal(wid))
            .map(|terminal| {
                (
                    terminal.session,
                    term_lock(&terminal.term).absolute_row_revision(),
                )
            });
        let anchor = self
            .search_last_anchor
            .filter(|(anchor_session, anchor_revision, ..)| {
                Some((*anchor_session, *anchor_revision)) == session_revision
            })
            .map(|(_, _, row, start, end)| (row, start, end));
        self.search_enter_direction(forward);
        if let Some(search) = self.front_mut().and_then(|ws| ws.search.as_mut()) {
            search.query = query;
            if let Some((row, start, _)) = anchor {
                search.anchor_absolute_row = row;
                search.anchor_col = start;
            }
        }
        self.search_recompute_from_anchor(anchor.is_some());
    }

    /// `^S`/`^R` in find mode (emacs isearch-repeat): step to the next (`forward`) /
    /// previous match — except on an EMPTY query, where the chord RECALLS the last
    /// ACCEPTED query ([`crate::App`]'s `search_last_query`) instead: the emacs
    /// `C-s C-s` "search for the same thing again" idiom. A forward recall lands on
    /// the FIRST match, a backward one on the LAST (searching up from the bottom).
    /// Empty query + nothing to recall is a no-op (as is stepping with no matches).
    #[cfg(test)]
    pub(crate) fn search_repeat(&mut self, forward: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_repeat_in(wid, forward);
    }

    pub(crate) fn search_repeat_in(&mut self, wid: crate::WindowId, forward: bool) {
        if let Some(s) = self.windows.get_mut(&wid).and_then(|ws| ws.search.as_mut()) {
            // Set direction before a possible recall/recompute so backward recall
            // installs the last match directly, with no transient first match.
            s.direction = SearchDirection::from_forward(forward);
        }
        let query_empty = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .is_some_and(|s| s.query.is_empty());
        if query_empty && !self.search_last_query.is_empty() {
            let recalled = self.search_last_query.clone();
            if let Some(s) = self.windows.get_mut(&wid).and_then(|ws| ws.search.as_mut()) {
                s.query = recalled;
                // Empty-query recall has explicit whole-buffer semantics: the
                // first forward hit or last backward hit, independent of the
                // entry point captured for a still-empty find bar.
                if forward {
                    s.anchor_absolute_row = i64::MIN;
                    s.anchor_col = 0;
                } else {
                    s.anchor_absolute_row = i64::MAX;
                    s.anchor_col = u16::MAX;
                }
            }
            // Direction-aware recompute selects FIRST for forward, LAST for backward.
            self.search_recompute_in(wid);
            return;
        }
        self.search_step_in(wid, forward);
    }

    /// Leave find mode via ⏎ ACCEPT (emacs `RET`): the viewport STAYS wherever the find
    /// navigation left it and the current match KEEPS its selection highlight (ready for
    /// ⌘C), so find doubles as fast navigation through a big scrollback. The accepted
    /// query is remembered app-sticky for `^S`/`^R` empty-query recall next find.
    pub(crate) fn search_accept(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        if let Some((revision_stale, content_stale)) = self.search_stamp_mismatch(wid)
            && (revision_stale || content_stale)
        {
            self.invalidate_search_results(wid, revision_stale);
        }
        if self
            .front()
            .and_then(|ws| ws.search.as_ref())
            .is_some_and(|search| search.results_dirty)
        {
            self.search_recompute();
        }
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .is_some_and(|search| search.results_dirty)
        {
            // A second concurrent mutation won the bounded retry. Keep the bar
            // open and fail closed; the next user action retries from fresh state.
            return;
        }
        if let Some((revision_stale, content_stale)) = self.search_stamp_mismatch(wid)
            && (revision_stale || content_stale)
        {
            self.invalidate_search_results(wid, revision_stale);
            return;
        }
        let session = self
            .frontmost_window
            .and_then(|wid| self.front_terminal(wid))
            .map(|terminal| terminal.session);
        let accepted = self.front().and_then(|ws| ws.search.as_ref()).map(|s| {
            let anchor = s
                .current_match()
                .map(|(row, start, end)| (s.match_base_y + i64::from(row), start, end));
            (s.query.clone(), s.match_absolute_row_revision, anchor)
        });
        if let Some((q, revision, anchor)) = accepted
            && !q.is_empty()
        {
            self.search_last_query = q;
            self.search_last_anchor = session
                .zip(anchor)
                .map(|(session, (row, start, end))| (session, revision, row, start, end));
        }
        self.search_close(false);
    }

    /// Leave find mode via ⎋/^G CANCEL (emacs `C-g`): clear the highlight and RESTORE
    /// the viewport captured at [`Self::search_enter`] — re-anchored by
    /// `base_y_now − origin_base_y` past any output that streamed in mid-find — so an
    /// abandoned find never teleports the user away from what they were reading.
    pub(crate) fn search_cancel(&mut self) {
        let origin = self.front().and_then(|ws| ws.search.as_ref()).map(|s| {
            (
                s.origin_display_offset,
                s.origin_base_y,
                s.origin_absolute_row_revision,
            )
        });
        if let Some((origin_offset, origin_base_y, origin_revision)) = origin
            && let Some(term) = self
                .frontmost_window
                .and_then(|wid| self.front_terminal(wid).map(|t| t.term.clone()))
        {
            let mut terminal = term_lock(&term);
            if terminal.absolute_row_revision() == origin_revision {
                let base_y = i64::try_from(terminal.grid().base_y()).unwrap_or(0);
                let delta = base_y - origin_base_y;
                let target = i64::from(origin_offset) + delta;
                terminal.scroll_to_bottom();
                if let Ok(target) = i32::try_from(target.clamp(0, i64::from(i32::MAX)))
                    && target > 0
                {
                    terminal.scroll_display(target);
                }
            }
        }
        self.search_close(true);
    }

    /// Leave find mode NEUTRALLY (non-keystroke plumbing paths): clear the highlight +
    /// restore the title, leaving the viewport wherever it is. The user-facing exits are
    /// [`Self::search_accept`] (⏎) and [`Self::search_cancel`] (⎋/^G).
    #[allow(
        dead_code,
        reason = "neutral close seam is exercised by renderer lifecycle tests"
    )]
    pub(crate) fn search_exit(&mut self) {
        self.search_close(true);
    }

    /// Shared find-close core: drop the overlay state (+ its clickable-indicator
    /// geometry), optionally clear the selection highlight (accept keeps it on the
    /// match), and restore the newest title owned by the remaining authority. An
    /// armed close warning stays untouched; otherwise the canonical Smart Title
    /// cache is restored.
    fn search_close(&mut self, clear_selection: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(ws) = self.front_mut() else { return };
        ws.search = None;
        ws.find_bar_hit = None; // drop the clickable-indicator geometry with the overlay
        if clear_selection {
            term_lock(&term).text_selection_mut().clear();
        }
        let warning_armed = ws
            .close_warning_until
            .is_some_and(|deadline| Instant::now() < deadline);
        if crate::app_window::window_title_authority(warning_armed, false)
            == crate::app_window::WindowTitleAuthority::Canonical
            && let Some(w) = &ws.os_window
        {
            w.set_title(&ws.current_title);
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchDirection, SearchMatch, SearchState, map_matches};
    use crate::{App, WindowId, term_lock};

    /// A `SearchState` pre-seeded with three matches (scrollback row -1, live rows
    /// 0/1) so the find-state machine (`step`, `current_match`) can be driven
    /// headlessly — no window, no renderer.
    fn seeded() -> SearchState {
        SearchState {
            query: "foo".to_string(),
            matches: vec![(-1, 0, 2), (0, 2, 4), (0, 12, 14)],
            ..Default::default()
        }
    }

    /// Native-title ownership is an explicit stack: the destructive-close warning
    /// outranks find, and find outranks the continuously refreshed Smart Title.
    /// Canonical cache updates therefore cannot erase a live search status, while
    /// dismissing search reveals the newest (not the pre-search) composed title.
    #[test]
    fn native_title_override_transitions_preserve_latest_smart_title() {
        use crate::app_window::{WindowTitleAuthority, window_title_authority};

        assert_eq!(
            window_title_authority(false, false),
            WindowTitleAuthority::Canonical
        );
        assert_eq!(
            window_title_authority(false, true),
            WindowTitleAuthority::Search
        );
        assert_eq!(
            window_title_authority(true, true),
            WindowTitleAuthority::CloseWarning
        );
        assert_eq!(
            window_title_authority(true, false),
            WindowTitleAuthority::CloseWarning
        );

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).unwrap().current_title =
            "build — compiling workspace".to_string();
        app.search_enter();
        let search_status = app.windows[&wid]
            .search
            .as_ref()
            .expect("find active")
            .window_title();

        // This is the cache update `apply_title` performs when a live summary
        // changes under find. Search remains authoritative on glass.
        app.windows.get_mut(&wid).unwrap().current_title = "build — tests are passing".to_string();
        assert_eq!(
            window_title_authority(false, app.windows[&wid].search.is_some()),
            WindowTitleAuthority::Search
        );
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().window_title(),
            search_status,
            "a canonical Smart Title refresh cannot mutate the find status"
        );

        app.search_close(true);
        assert!(app.windows[&wid].search.is_none());
        assert_eq!(
            app.windows[&wid].current_title, "build — tests are passing",
            "close restores the latest canonical cache, not literal aterm or stale pre-find text"
        );
        assert_eq!(
            window_title_authority(false, app.windows[&wid].search.is_some()),
            WindowTitleAuthority::Canonical
        );
    }

    #[test]
    fn close_warning_remains_above_search_until_its_expiry() {
        use crate::app_window::{WindowTitleAuthority, window_title_authority};

        let search = seeded();
        assert_eq!(search.window_title(), "aterm — find: foo (1/3)");
        assert_eq!(
            window_title_authority(true, true),
            WindowTitleAuthority::CloseWarning,
            "search updates cannot erase the safety warning"
        );
        assert_eq!(
            window_title_authority(false, true),
            WindowTitleAuthority::Search,
            "warning expiry restores the still-active search status"
        );
        assert_eq!(
            window_title_authority(false, false),
            WindowTitleAuthority::Canonical,
            "closing search then restores the composed title"
        );
    }

    fn set_query(app: &mut App, wid: WindowId, query: &str) {
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.search.as_mut())
            .expect("active search")
            .query = query.into();
    }

    /// `map_matches` converts engine ABSOLUTE rows to SELECTION rows (`abs − base_y`,
    /// negative = scrollback), turns EXCLUSIVE end columns INCLUSIVE, and sorts into
    /// visual (row, start) order regardless of the engine's grouping order.
    #[test]
    fn map_matches_abs_to_selection_inclusive_sorted() {
        let base_y = 100; // absolute row of the top visible line
        // Deliberately out of order: live 102, scrollback 99, live 100 (two hits).
        let ms = vec![
            SearchMatch::new(102, 2, 4),
            SearchMatch::new(99, 0, 3),
            SearchMatch::new(100, 5, 6),
            SearchMatch::new(100, 1, 3),
        ];
        // sel_row = abs − base_y; end_inclusive = end_col − 1; sorted by (row, start).
        assert_eq!(
            map_matches(&ms, base_y),
            vec![(-1, 0, 2), (0, 1, 2), (0, 5, 5), (2, 2, 3)]
        );
    }

    /// Out-of-`i32`/`u16`-range rows or columns are dropped, never wrapped, so a
    /// pathological absolute row cannot corrupt the selection coordinates.
    #[test]
    fn map_matches_drops_out_of_range() {
        // A match so far below base_y that `abs − base_y` underflows i32 is dropped.
        let ms = vec![SearchMatch::new(0, 0, 1)];
        assert!(map_matches(&ms, i64::from(i32::MAX) + 10).is_empty());
        // A line beyond i64 is dropped, while a valid one alongside survives.
        let ms = vec![
            SearchMatch::new(5, 0, 1),
            SearchMatch::new(usize::MAX, 0, 1),
        ];
        assert_eq!(map_matches(&ms, 5), vec![(0, 0, 0)]);
    }

    /// open -> next/next/next (wrap) and prev (wrap) walks the current match through
    /// the set with correct wraparound, and the current offset tracks it at every step.
    #[test]
    fn find_state_next_prev_wraparound() {
        let mut s = seeded();
        let n = s.matches.len();
        assert_eq!(n, 3);

        // Starts on the first (top-most) match.
        assert_eq!(s.current_match(), Some((-1, 0, 2)));
        // next -> second, next -> third.
        s.step(true);
        assert_eq!(s.current_match(), Some((0, 2, 4)));
        s.step(true);
        assert_eq!(s.current_match(), Some((0, 12, 14)));
        // next off the end wraps to the first.
        s.step(true);
        assert_eq!(s.current, 0);
        assert_eq!(s.current_match(), Some((-1, 0, 2)));
        // prev off the front wraps to the last (Shift-Enter from the top).
        s.step(false);
        assert_eq!(s.current, n - 1);
        assert_eq!(s.current_match(), Some((0, 12, 14)));
        // prev -> middle, prev -> first.
        s.step(false);
        assert_eq!(s.current_match(), Some((0, 2, 4)));
        s.step(false);
        assert_eq!(s.current_match(), Some((-1, 0, 2)));
    }

    /// With no matches the cursor step is a no-op (Enter/Shift-Enter do nothing),
    /// and there is no current match to highlight.
    #[test]
    fn find_state_step_no_matches_is_noop() {
        let mut s = SearchState {
            query: "zzz".to_string(),
            ..Default::default()
        };
        assert!(s.matches.is_empty());
        assert_eq!(s.current_match(), None);
        s.step(true);
        assert_eq!(s.current, 0);
        s.step(false);
        assert_eq!(s.current, 0);
        assert_eq!(s.current_match(), None);
        assert_eq!(
            s.direction,
            SearchDirection::Backward,
            "a zero-hit reversal still governs the next incremental query"
        );
    }

    /// Truncated point navigation performs a logarithmic lookup and stores an
    /// out-of-batch point separately; it never shifts or reallocates the 100k
    /// capped match vector on a repeat.
    #[test]
    fn truncated_point_install_is_logarithmic_and_allocation_stable() {
        let mut state = SearchState {
            matches: (0..100_000).map(|row| (row, 0, 0)).collect(),
            truncated: true,
            ..Default::default()
        };
        let len = state.matches.len();
        let capacity = state.matches.capacity();
        let ptr = state.matches.as_ptr();
        let _ = super::take_point_lookup_comparisons();

        assert!(!state.install_point_match((100_001, 0, 0)));
        assert_eq!(state.current_match(), Some((100_001, 0, 0)));
        assert_eq!(state.matches.len(), len);
        assert_eq!(state.matches.capacity(), capacity);
        assert_eq!(state.matches.as_ptr(), ptr);
        assert!(
            super::take_point_lookup_comparisons() <= 18,
            "binary search over 100k coordinates needs at most ceil(log2(n))+1 comparisons"
        );
    }

    /// Direction chooses the initial result in one state installation: forward is
    /// always the first match, backward the last, including every small bounded match
    /// count and the empty set. This is the no-transient-wrong-selection core used by
    /// `search_recompute` before it calls the renderer-facing apply path.
    #[test]
    fn emacs_direction_initial_index_is_exhaustive_and_atomic() {
        for count in 0..=4096 {
            assert_eq!(SearchDirection::Forward.initial_index(count), 0);
            assert_eq!(
                SearchDirection::Backward.initial_index(count),
                count.saturating_sub(1),
                "count={count}"
            );
        }
    }

    /// End-to-end state semantics over a real terminal: backward initial typing lands
    /// on the last hit, reversals and repeats wrap, empty-query recall honors direction,
    /// and legacy Cmd-F resets an open search to forward/first-match behavior.
    #[test]
    fn emacs_direction_reversal_repeat_wrap_empty_recall_and_cmd_f_legacy() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"hit one\r\nhit two\r\nhit three");

        app.search_enter_direction(false);
        set_query(&mut app, wid, "hit");
        app.search_recompute();
        let search = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(search.matches.len(), 3);
        assert_eq!(search.current, 2, "backward typing installs the last hit");
        assert_eq!(search.direction, SearchDirection::Backward);

        app.search_repeat(true);
        let search = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(search.current, 0, "reverse to forward wraps last → first");
        assert_eq!(search.direction, SearchDirection::Forward);
        app.search_repeat(false);
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().current,
            2,
            "reverse to backward wraps first → last"
        );

        app.search_accept();
        app.search_enter_direction(false);
        app.search_repeat(false);
        let recalled = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(recalled.query, "hit");
        assert_eq!(
            recalled.current, 2,
            "backward empty recall lands last directly"
        );

        app.search_enter();
        let legacy = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(legacy.direction, SearchDirection::Forward);
        assert_eq!(legacy.current, 0, "Cmd-F remains forward/first-match");
    }

    /// Incremental search starts at the content the user is reading, matching
    /// Emacs point semantics instead of jumping to a global buffer extreme.
    #[test]
    fn emacs_search_is_anchored_to_scrolled_viewport_in_both_directions() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        let mut content = String::new();
        for line in 0..100 {
            let marker = if line % 10 == 0 { " NEEDLE" } else { "" };
            content.push_str(&format!("line {line:03}{marker}\r\n"));
        }
        {
            let mut terminal = term_lock(&term);
            terminal.process(content.as_bytes());
            terminal.scroll_display(30);
        }
        let (top, bottom) = {
            let terminal = term_lock(&term);
            let base = i64::try_from(terminal.grid().base_y()).unwrap();
            let top = base - i64::try_from(terminal.grid().display_offset()).unwrap();
            (top, top + i64::from(terminal.rows().saturating_sub(1)))
        };

        app.search_enter_direction(true);
        set_query(&mut app, wid, "NEEDLE");
        app.search_recompute();
        let forward = app.windows[&wid].search.as_ref().unwrap();
        let forward_abs =
            forward.match_base_y + i64::from(forward.current_match().expect("forward hit").0);
        assert!(forward_abs >= top);
        assert!(
            forward.matches[..forward.current]
                .iter()
                .all(|&(row, _, _)| forward.match_base_y + i64::from(row) < top)
        );
        assert!(
            forward.current > 0,
            "fixture must not choose the oldest hit"
        );
        app.search_cancel();

        app.search_enter_direction(false);
        set_query(&mut app, wid, "NEEDLE");
        app.search_recompute();
        let backward = app.windows[&wid].search.as_ref().unwrap();
        let backward_abs =
            backward.match_base_y + i64::from(backward.current_match().expect("backward hit").0);
        assert!(backward_abs <= bottom);
        assert!(
            backward.matches[backward.current + 1..]
                .iter()
                .all(|&(row, _, _)| backward.match_base_y + i64::from(row) > bottom)
        );
        assert!(
            backward.current + 1 < backward.matches.len(),
            "fixture must not choose the newest global hit"
        );
    }

    /// Claude/Codex-style streaming output invalidates a result batch without
    /// rescanning on every chunk. The next repeat refreshes exactly once and
    /// navigates relative to the old point, so a newly appended hit is visible.
    #[test]
    fn streaming_output_marks_search_dirty_then_repeat_refreshes_latest_content() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"hit one");

        app.search_enter_direction(true);
        set_query(&mut app, wid, "hit");
        app.search_recompute();
        let before = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(before.matches.len(), 1);
        let before_seq = before.match_content_seq;
        let before_abs = before.match_base_y + i64::from(before.current_match().unwrap().0);
        assert!(term_lock(&term).text_selection().has_selection());

        term_lock(&term).process(b"\r\nhit two");
        assert_ne!(term_lock(&term).content_seq(), before_seq);
        app.search_refresh_for_output(0);
        assert!(app.windows[&wid].search.as_ref().unwrap().results_dirty);
        assert!(!term_lock(&term).text_selection().has_selection());

        app.search_repeat(true);
        let after = app.windows[&wid].search.as_ref().unwrap();
        assert!(!after.results_dirty);
        assert_eq!(after.matches.len(), 2);
        assert_eq!(after.match_content_seq, term_lock(&term).content_seq());
        let after_abs = after.match_base_y + i64::from(after.current_match().unwrap().0);
        assert!(after_abs > before_abs, "repeat must reach the streamed hit");
        assert!(term_lock(&term).text_selection().has_selection());
    }

    #[test]
    fn query_edit_after_repeat_stays_at_emacs_point() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"target one\r\ntarget two\r\ntarget three");

        app.search_enter_direction(false);
        set_query(&mut app, wid, "tar");
        app.search_recompute();
        app.search_repeat(false);
        let anchored = {
            let search = app.windows[&wid].search.as_ref().unwrap();
            assert_eq!(search.current, 1);
            search.match_base_y + i64::from(search.current_match().unwrap().0)
        };

        set_query(&mut app, wid, "target");
        app.search_recompute();
        let edited = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(edited.current, 1, "editing must not snap back to newest");
        assert_eq!(
            edited.match_base_y + i64::from(edited.current_match().unwrap().0),
            anchored
        );
    }

    #[test]
    fn resize_without_output_wake_cannot_step_or_accept_stale_results() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"hit one\r\nhit two\r\nhit three");
        app.search_enter_direction(true);
        set_query(&mut app, wid, "hit");
        app.search_recompute();

        let (rows, cols, old_seq) = {
            let terminal = term_lock(&term);
            (terminal.rows(), terminal.cols(), terminal.content_seq())
        };
        term_lock(&term).resize(rows, cols.saturating_sub(1).max(1));
        assert_ne!(term_lock(&term).content_seq(), old_seq);
        app.search_step(true);
        let refreshed = app.windows[&wid].search.as_ref().unwrap();
        assert!(!refreshed.results_dirty);
        assert_eq!(refreshed.match_content_seq, term_lock(&term).content_seq());

        let current_cols = term_lock(&term).cols();
        term_lock(&term).resize(rows, current_cols.saturating_sub(1).max(1));
        app.search_accept();
        assert!(app.windows[&wid].search.is_none());
        assert!(term_lock(&term).text_selection().has_selection());
    }

    /// Tier-1 conformance for the derived `EmacsSearchNavigation` model.  This
    /// drives the real terminal snapshot/search, shipping `SearchState::step`,
    /// selection overlay, and cancel/accept paths, then projects their state onto
    /// the model after every transition.  The model's deliberate PTY-leak/linear-
    /// repeat mutant is checked as a negative control so this binding cannot pass
    /// vacuously.
    #[test]
    fn emacs_navigation_conforms_to_derived_transition_model() {
        let _serial = crate::control::search_cap_test_guard();
        let model = aterm_spec::derive::emacs_search_navigation_model();
        let mut state = model.init_state();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();

        let mut content = String::new();
        for line in 0..80 {
            let marker = if matches!(line, 7 | 39 | 71) {
                " NEEDLE"
            } else {
                ""
            };
            content.push_str(&format!("line {line:02}{marker}\r\n"));
        }
        {
            let mut terminal = term_lock(&term);
            terminal.process(content.as_bytes());
            terminal.scroll_display(9);
        }
        let origin_offset = term_lock(&term).grid().display_offset();
        assert!(
            origin_offset > 0,
            "fixture must start away from the live bottom"
        );

        let assert_active_projection =
            |app: &App, state: &aterm_spec::interp::State, expected_hits: usize| {
                let search = app.windows[&wid].search.as_ref().expect("active search");
                assert_eq!(state["active"], 1);
                assert_eq!(state["hits"], expected_hits as i64);
                assert_eq!(state["current"], search.current as i64);
                assert_eq!(
                    state["forward"] == 1,
                    search.direction == SearchDirection::Forward
                );
                assert_eq!(
                    state["selection"] == 1,
                    term_lock(&term).text_selection().has_selection(),
                );
                assert_eq!(state["dirty"] == 1, search.results_dirty);
                assert_eq!(state["pty_writes"], 0);
                assert!(state["nav_work"] <= 1);
            };
        let fire = |state: &mut aterm_spec::interp::State, action| {
            assert!(model.fire(action, state), "{action}: {state:?}");
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, state),
                    "{} after {action}: {state:?}",
                    invariant.name,
                );
            }
        };

        app.search_enter_direction(false);
        fire(&mut state, "OpenBackward");
        set_query(&mut app, wid, "NEEDLE");
        app.search_recompute();
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().matches.len(), 3);
        for _ in 0..3 {
            fire(&mut state, "PublishHit");
        }
        assert_active_projection(&app, &state, 3);

        app.search_repeat(true);
        fire(&mut state, "RepeatForward");
        assert_active_projection(&app, &state, 3);
        app.search_repeat(false);
        fire(&mut state, "RepeatBackward");
        assert_active_projection(&app, &state, 3);

        term_lock(&term).process(b"\r\nstreamed non-match");
        app.search_refresh_for_output(0);
        fire(&mut state, "Output");
        assert_active_projection(&app, &state, 3);
        app.search_repeat(true);
        fire(&mut state, "RefreshRepeatForward");
        assert_active_projection(&app, &state, 3);

        let expected_cancel_offset = {
            let search = app.windows[&wid].search.as_ref().unwrap();
            let base_now = i64::try_from(term_lock(&term).grid().base_y()).unwrap();
            usize::try_from(
                i64::from(search.origin_display_offset)
                    + base_now.saturating_sub(search.origin_base_y),
            )
            .unwrap()
        };
        app.search_cancel();
        fire(&mut state, "Cancel");
        assert!(app.windows[&wid].search.is_none());
        assert!(!term_lock(&term).text_selection().has_selection());
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            expected_cancel_offset
        );
        assert_eq!(state["active"], 0);
        assert_eq!(state["last_exit"], 1);

        // The accept branch keeps the current hit selected after closing.
        app.search_enter_direction(false);
        fire(&mut state, "OpenBackward");
        set_query(&mut app, wid, "NEEDLE");
        app.search_recompute();
        for _ in 0..3 {
            fire(&mut state, "PublishHit");
        }
        assert_active_projection(&app, &state, 3);
        app.search_accept();
        fire(&mut state, "Accept");
        assert!(app.windows[&wid].search.is_none());
        assert!(term_lock(&term).text_selection().has_selection());
        assert_eq!(state["last_exit"], 2);

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut mutant = buggy.init_state();
        assert!(buggy.fire("OpenForward", &mut mutant));
        assert!(!buggy.check_invariant("NoPtyLeak", &mutant));
        for _ in 0..3 {
            assert!(buggy.fire("PublishHit", &mut mutant));
        }
        assert!(buggy.fire("RepeatForward", &mut mutant));
        assert!(!buggy.check_invariant("RepeatWorkBounded", &mutant));
    }

    /// Codex-style protected footer refreshes rebuild absolute coordinates without
    /// losing reverse-search direction: after the piecewise insertion the last footer
    /// hit is still current, never a stale or transient first hit.
    #[test]
    fn codex_protected_footer_refresh_preserves_backward_last_match() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(
            format!("\x1b[{};1HFOOTERNEEDLE\x1b[{rows};1HFOOTERNEEDLE", rows - 1).as_bytes(),
        );

        app.search_enter_direction(false);
        set_query(&mut app, wid, "FOOTERNEEDLE");
        app.search_recompute();
        let before = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(before.matches.len(), 2);
        assert_eq!(before.current, 1);
        assert_eq!(before.match_absolute_row_revision, 0);

        // Row 0 must be WRITTEN for the displaced row to archive and splice.
        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r")
                .as_bytes(),
        );
        assert_eq!(term_lock(&term).absolute_row_revision(), 1);
        app.search_refresh_for_output(0);

        let invalidated = app.windows[&wid].search.as_ref().unwrap();
        assert!(invalidated.results_dirty);
        assert_eq!(invalidated.match_absolute_row_revision, 0);
        assert!(!term_lock(&term).text_selection().has_selection());
        app.search_repeat(false);

        let after = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(after.direction, SearchDirection::Backward);
        assert_eq!(after.current, after.matches.len() - 1);
        assert_eq!(after.match_absolute_row_revision, 1);
        let (row, _, _) = after.current_match().expect("last footer hit");
        assert_eq!(after.match_base_y + i64::from(row), i64::from(rows));
    }

    /// A protected-footer revision invalidates the old piecewise coordinate
    /// frame. Forward repeat must restart inclusively at the true beginning;
    /// clamping the internal before-first sentinel to `(0, 0)` and then using a
    /// strict comparison would incorrectly skip this first retained hit.
    #[test]
    fn codex_protected_footer_forward_refresh_keeps_row_zero_match() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"EDGE\r\nEDGE");

        app.search_enter_direction(false);
        set_query(&mut app, wid, "EDGE");
        app.search_recompute();
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().matches.len(), 2);

        // Row 0 already holds written content ("EDGE") — the displaced row
        // archives and splices without extra seeding.
        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
        );
        assert_eq!(term_lock(&term).absolute_row_revision(), 1);
        app.search_refresh_for_output(0);
        app.search_repeat(true); // reverse direction while refreshing

        let after = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(after.direction, SearchDirection::Forward);
        assert!(!after.results_dirty);
        assert_eq!(after.match_absolute_row_revision, 1);
        assert!(after.matches.len() >= 2);
        let (row, col, _) = after.current_match().expect("first retained hit");
        assert_eq!(after.match_base_y + i64::from(row), 0);
        assert_eq!(col, 0);
    }

    /// Claude's classic in-place UI and alternate-screen UI both search the currently
    /// active grid. Switching to alt excludes parked classic content; switching back
    /// restores it, with backward initial selection correct in both modes.
    #[test]
    fn claude_classic_and_alternate_active_grid_search_routing() {
        let _serial = crate::control::search_cap_test_guard();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"CLASSIC_NEEDLE\r\nCLASSIC_NEEDLE");

        app.search_enter_direction(false);
        set_query(&mut app, wid, "CLASSIC_NEEDLE");
        app.search_recompute();
        let classic = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(classic.matches.len(), 2);
        assert_eq!(classic.current, 1);
        app.search_cancel();

        term_lock(&term).process(b"\x1b[?1049h\x1b[HALT_NEEDLE\r\nALT_NEEDLE");
        app.search_enter_direction(false);
        set_query(&mut app, wid, "ALT_NEEDLE");
        app.search_recompute();
        let alternate = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(alternate.matches.len(), 2);
        assert_eq!(alternate.current, 1);

        set_query(&mut app, wid, "CLASSIC_NEEDLE");
        app.search_recompute();
        assert!(
            app.windows[&wid]
                .search
                .as_ref()
                .unwrap()
                .matches
                .is_empty(),
            "parked classic grid cannot leak into active alternate-grid search"
        );
        app.search_cancel();

        term_lock(&term).process(b"\x1b[?1049l");
        app.search_enter();
        set_query(&mut app, wid, "CLASSIC_NEEDLE");
        app.search_recompute();
        let restored = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(restored.matches.len(), 2);
        assert_eq!(restored.current, 0);
    }
}
