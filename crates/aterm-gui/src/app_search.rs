// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The find overlay (⌘F, Emacs-style navigation): the find state (`SearchState`), the absolute → selection row map (`map_matches`), and the `App` methods that open, recompute, step and close it.

use crate::app_mouse::note_selection_custody;
use crate::{App, term_lock};
use aterm_core::search::{SearchDirection as EngineSearchDirection, SearchMatch};
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

/// Direction of an incremental find.
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
}

/// One text-field edit of the find query: the macOS/readline motions (^A/^E/^B/^F, ⌥←/⌥→, Home/End) and kills (^K/^U/^W, ⌥⌫, ⌘⌫).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SearchEdit {
    /// Insert typed/pasted text at the caret, control characters stripped: ⎋/⏎/⇥ are commands here, never content.
    Insert(String),
    DeleteBack,
    DeleteForward,
    DeleteWordBack,
    DeleteWordForward,
    KillToStart,
    KillToEnd,
    MoveCharLeft,
    MoveCharRight,
    MoveWordLeft,
    MoveWordRight,
    MoveStart,
    MoveEnd,
    /// Put the caret at a byte offset (a click in the field's well).
    MoveTo(usize),
}

/// Byte offset of the previous word boundary before `at`: the start of the word the
fn prev_word_boundary(s: &str, at: usize) -> usize {
    aterm_grapheme::word_start_before(s, at, aterm_grapheme::is_word_char).unwrap_or(0)
}

/// Byte offset of the next word boundary after `at`: the end of the word the caret is
fn next_word_boundary(s: &str, at: usize) -> usize {
    let from = aterm_grapheme::word_break_before(s, at, aterm_grapheme::is_word_char);
    aterm_grapheme::words(
        s.get(from..).unwrap_or_default(),
        aterm_grapheme::is_word_char,
    )
    .map(|word| from.saturating_add(word.end))
    .find(|&end| end > at)
    .unwrap_or(s.len())
}

/// Apply one [`SearchEdit`] to a single-line `(text, cursor)` field: `true` when the text changed, `false` for a caret move.
pub(crate) fn apply_field_edit(text: &mut String, cursor: &mut usize, edit: SearchEdit) -> bool {
    let floor = |text: &str, at: usize| -> usize {
        let mut at = at.min(text.len());
        while at > 0 && !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let prev_char = |text: &str, at: usize| -> usize {
        text[..at]
            .chars()
            .next_back()
            .map_or(at, |c| at - c.len_utf8())
    };
    let next_char = |text: &str, at: usize| -> usize {
        text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
    };
    *cursor = floor(text, *cursor);
    let at = *cursor;
    match edit {
        SearchEdit::Insert(insert) => {
            let insert: String = insert
                .chars()
                .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'))
                .collect();
            if insert.is_empty() {
                return false;
            }
            text.insert_str(at, &insert);
            *cursor = at + insert.len();
            true
        }
        SearchEdit::DeleteBack => {
            let start = prev_char(text, at);
            if start == at {
                return false;
            }
            text.replace_range(start..at, "");
            *cursor = start;
            true
        }
        SearchEdit::DeleteForward => {
            let end = next_char(text, at);
            if end == at {
                return false;
            }
            text.replace_range(at..end, "");
            true
        }
        SearchEdit::DeleteWordBack => {
            let start = prev_word_boundary(text, at);
            if start == at {
                return false;
            }
            text.replace_range(start..at, "");
            *cursor = start;
            true
        }
        SearchEdit::DeleteWordForward => {
            let end = next_word_boundary(text, at);
            if end == at {
                return false;
            }
            text.replace_range(at..end, "");
            true
        }
        SearchEdit::KillToStart => {
            if at == 0 {
                return false;
            }
            text.replace_range(..at, "");
            *cursor = 0;
            true
        }
        SearchEdit::KillToEnd => {
            if at == text.len() {
                return false;
            }
            text.truncate(at);
            true
        }
        SearchEdit::MoveCharLeft => {
            *cursor = prev_char(text, at);
            false
        }
        SearchEdit::MoveCharRight => {
            *cursor = next_char(text, at);
            false
        }
        SearchEdit::MoveWordLeft => {
            *cursor = prev_word_boundary(text, at);
            false
        }
        SearchEdit::MoveWordRight => {
            *cursor = next_word_boundary(text, at);
            false
        }
        SearchEdit::MoveStart => {
            *cursor = 0;
            false
        }
        SearchEdit::MoveEnd => {
            *cursor = text.len();
            false
        }
        SearchEdit::MoveTo(offset) => {
            *cursor = floor(text, offset);
            false
        }
    }
}

/// Drop the find highlight only when the output that invalidated the matches could
fn clear_selection_if_content_is_mutable(term: &mut crate::TermGuard<'_>) -> bool {
    let live_top_abs = u64::try_from(term.grid().base_y()).unwrap_or(u64::MAX);
    let live_bottom_abs = live_top_abs.saturating_add(u64::from(term.rows().saturating_sub(1)));
    if term
        .text_selection()
        .intersects_absolute_band(live_top_abs, live_top_abs, live_bottom_abs)
    {
        term.text_selection_mut().clear();
        return true;
    }
    false
}

/// The viewport a find was opened from, as an absolute anchor rather than a `display_offset` + `base_y` pair to re-derive a delta from.
#[derive(Clone, Copy)]
pub(crate) struct FindOrigin {
    /// The absolute row at the top of the viewport when find opened.
    pub(crate) top_visible_absolute_row: u64,
    /// Find opened at the live tail (`display_offset == 0`).
    pub(crate) was_live: bool,
    /// Which buffer the anchor's absolute numbering belongs to.
    pub(crate) on_alt_screen: bool,
    /// Grid renumber epoch the anchor was captured against.
    pub(crate) history_renumber_epoch: u64,
}

/// What the find bar, the title and a screen reader say of a search that found nothing while the history was away for a rewrap (the long form; the bar shortens it to fit, `find_bar::status_seg`).
pub(crate) const HISTORY_AWAY_NONE: &str = "none yet, history rewrapping";

/// What a find count rests on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CountFacts {
    /// The count is a floor: some of the history was not searched, or the list stopped at the match limit.
    pub(crate) truncated: bool,
    /// The terminal changed since the count was taken.
    pub(crate) stale: bool,
    /// A rewrap held the history away; only the screen was searched.
    pub(crate) history_away: bool,
    /// The depth cap stopped the search after this many of the newest lines.
    pub(crate) searched_lines: Option<usize>,
    /// The list stopped at [`aterm_search::MAX_SEARCH_MATCHES`].
    pub(crate) match_capped: bool,
    /// Some line was not searched to its end ([`aterm_search::SearchResults::cut_short_lines`]): a regex scan could not finish it within the line's budget (a soft-wrapped run of megabytes, say), or it is a history line read only up to its first MiB.
    pub(crate) cut_short: bool,
}

impl CountFacts {
    /// `+` when the count is a floor.
    pub(crate) fn plus(&self) -> &'static str {
        if self.truncated { "+" } else { "" }
    }

    /// The compact marks: `+` (a floor), `…` (stale), both, or none.
    pub(crate) fn mark(&self) -> &'static str {
        match (self.truncated, self.stale) {
            (true, true) => "+…",
            (true, false) => "+",
            (false, true) => "…",
            (false, false) => "",
        }
    }

    /// `newest 100,000 lines` (`short`: `newest 100k lines`), when the depth cap stopped the search.
    pub(crate) fn depth(&self, short: bool) -> Option<String> {
        self.searched_lines
            .map(|lines| format!("newest {} lines", line_count(lines, short)))
    }

    /// Why a floor is a floor, when the code knows; `partial history` when it
    fn cause(&self, hits: bool, short: bool) -> Option<String> {
        if self.history_away {
            None
        } else if self.match_capped && hits {
            Some("match limit".to_string())
        } else if let Some(depth) = self.depth(short) {
            Some(depth)
        } else {
            (self.truncated && !self.cut_short).then(|| "partial history".to_string())
        }
    }

    /// `long line partly searched` (`short`: `line cut short`, which fits the bar's status zone) when some line was not searched to its end.
    pub(crate) fn cut_short_word(&self, short: bool) -> Option<String> {
        self.cut_short.then(|| {
            if short {
                "line cut short".to_string()
            } else {
                "long line partly searched".to_string()
            }
        })
    }

    /// The qualifiers of a count of hits, widest first: the cause, then a cut-short line, then `rewrapping`, then `stale`.
    pub(crate) fn hit_words(&self, short: bool) -> Vec<String> {
        let mut words: Vec<String> = self.cause(true, short).into_iter().collect();
        words.extend(self.cut_short_word(short));
        if self.history_away {
            words.push("rewrapping".to_string());
        }
        if self.stale {
            words.push("stale".to_string());
        }
        words
    }

    /// A zero-match's verdict: `no matches in newest 100,000 lines` when the depth cap stopped it, else `no matches`.
    pub(crate) fn none_base(&self, short: bool) -> String {
        match self.depth(short) {
            Some(depth) if !self.history_away => format!("no matches in {depth}"),
            _ => "no matches".to_string(),
        }
    }

    /// The qualifiers a zero-match carries after [`Self::none_base`].
    pub(crate) fn none_words(&self) -> Vec<String> {
        let mut words: Vec<String> = if self.searched_lines.is_some() {
            Vec::new()
        } else {
            self.cause(false, false).into_iter().collect()
        };
        words.extend(self.cut_short_word(false));
        if self.stale {
            words.push("stale".to_string());
        }
        words
    }
}

/// `100,000`; `short` writes a round thousand as `100k`.
pub(crate) fn line_count(lines: usize, short: bool) -> String {
    if short && lines >= 1000 && lines.is_multiple_of(1000) {
        return format!("{}k", lines / 1000);
    }
    let digits = lines.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// ` (a, b)`, or nothing for no words.
pub(crate) fn parenthesized(words: &[String]) -> String {
    if words.is_empty() {
        String::new()
    } else {
        format!(" ({})", words.join(", "))
    }
}

/// `, a, b`, or nothing for no words.
fn comma_list(words: &[String]) -> String {
    words.iter().map(|word| format!(", {word}")).collect()
}

/// In-progress find over the live screen and scrollback.
#[derive(Default)]
pub(crate) struct SearchState {
    /// A delayed clipboard reply belongs to this open field, even if a new
    #[cfg(any(target_os = "linux", all(test, unix)))]
    paste_lifetime: std::sync::Arc<()>,
    pub(crate) query: String,
    /// Caret byte offset in [`Self::query`], always on a char boundary ([`Self::edit`] and [`Self::set_query`] keep it there).
    pub(crate) cursor: usize,
    pub(crate) matches: Vec<(i32, u16, u32)>,
    /// The exact point-relative hit when it lies outside the capped batch, kept apart
    point_match: Option<(i32, u16, u32)>,
    /// The most rows any match in [`Self::matches`] spans past its own (0 while none straddles a soft wrap).
    pub(crate) max_match_rows: i32,
    /// The grid `base_y()` [`Self::matches`] was computed against ([`App::search_recompute_in`]).
    pub(crate) match_base_y: i64,
    /// Terminal revision for top-anchored protected-footer insertions when [`Self::matches`] was computed.
    pub(crate) match_absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()` when [`Self::matches`] was computed.
    pub(crate) match_history_renumber_epoch: u64,
    /// Terminal content generation captured with [`Self::matches`].
    pub(crate) match_content_seq: u64,
    /// The terminal changed since the current result batch was built.
    pub(crate) results_dirty: bool,
    pub(crate) current: usize,
    /// Direction of the next repeat and of incremental recomputes, so typing after ⌘R lands on the anchored previous match in one render.
    pub(crate) direction: SearchDirection,
    /// Match case (default off = case-insensitive).
    pub(crate) case_sensitive: bool,
    /// Treat the query as a regular expression (default off = literal).
    pub(crate) is_regex: bool,
    /// The last recompute's query was an invalid regex (regex mode only): the bar says "bad regex", not zero hits.
    pub(crate) regex_error: bool,
    /// The count is a floor: the depth cap left older history unsearched ([`Self::searched_lines`]), the list stopped at the match limit ([`Self::match_capped`]), or a width change's rewrap held the history detached and only the screen was searched.
    pub(crate) truncated: bool,
    /// How many of the newest lines the search read when the depth cap (`search_history_lines`) left older history unread; `None` when it read the whole buffer.
    pub(crate) searched_lines: Option<usize>,
    /// The batch stopped at [`aterm_search::MAX_SEARCH_MATCHES`]: there are more hits than the list holds.
    pub(crate) match_capped: bool,
    /// Some line was not searched to its end: a regex scan out of budget, or a history line read only up to its first MiB ([`CountFacts::cut_short`]).
    pub(crate) cut_short: bool,
    /// The search ran while a width change's rewrap held the session's history detached: the text may be there, the history is simply away.
    pub(crate) history_away: bool,
    /// Absolute search origin used for Emacs-style point anchoring.
    pub(crate) anchor_absolute_row: i64,
    pub(crate) anchor_col: u16,
}

impl SearchState {
    /// Replace the whole query (recall, find-again, scripted seeds) and park the caret at its end, keeping [`Self::cursor`] on a char boundary.
    pub(crate) fn set_query(&mut self, query: String) {
        self.query = query;
        self.cursor = self.query.len();
    }

    /// Apply one text-field [`SearchEdit`], returning `true` when the query text changed (the caller re-runs the search) and `false` for a pure caret move (only a repaint).
    pub(crate) fn edit(&mut self, edit: SearchEdit) -> bool {
        apply_field_edit(&mut self.query, &mut self.cursor, edit)
    }

    /// The facts behind the count, in the one shape the bar, the title and the spoken status all word it from.
    pub(crate) fn count_facts(&self) -> CountFacts {
        CountFacts {
            truncated: self.truncated,
            stale: self.results_dirty,
            history_away: self.history_away,
            searched_lines: self.searched_lines,
            match_capped: self.match_capped,
            cut_short: self.cut_short,
        }
    }

    /// The `+` (the count is a floor) and `…` (the terminal changed since this was counted) marks the find bar carries, in that order.
    #[cfg(test)]
    pub(crate) fn count_qualifier(&self) -> &'static str {
        self.count_facts().mark()
    }

    /// The find panel's answer as a screen reader hears it (`match 2 of 7`, `no matches (stale)`, `bad regex`), with every qualifier the panel paints spelled out.
    #[cfg(any(a11y_tree, test))]
    pub(crate) fn spoken_status(&self) -> Option<String> {
        let facts = self.count_facts();
        if self.regex_error {
            Some("bad regex".to_string())
        } else if self.query.is_empty() {
            None
        } else if self.matches.is_empty() && self.history_away {
            // The painter's long form, at every width.
            Some(HISTORY_AWAY_NONE.to_string())
        } else if self.matches.is_empty() {
            Some(format!(
                "{}{}",
                facts.none_base(false),
                parenthesized(&facts.none_words())
            ))
        } else {
            Some(format!(
                "match {} of {}{}",
                self.current + 1,
                self.matches.len(),
                parenthesized(&facts.hit_words(false))
            ))
        }
    }

    pub(crate) fn window_title(&self) -> String {
        let facts = self.count_facts();
        if self.query.is_empty() {
            "aterm — find:".to_string()
        } else if self.regex_error {
            // The panel's own word: an invalid pattern is not a miss.
            format!("aterm — find: {} (bad regex)", self.query)
        } else if self.matches.is_empty() && self.history_away {
            // The history is away, not empty of the text.
            format!("aterm — find: {} ({HISTORY_AWAY_NONE})", self.query)
        } else if self.matches.is_empty() {
            format!(
                "aterm — find: {} ({}{})",
                self.query,
                facts.none_base(false),
                comma_list(&facts.none_words())
            )
        } else {
            format!(
                "aterm — find: {} ({}/{}{}{})",
                self.query,
                self.current + 1,
                self.matches.len(),
                facts.plus(),
                comma_list(&facts.hit_words(false))
            )
        }
    }

    /// Next/previous-match step with wraparound and no window or render side effects; with no matches it only sets the direction.
    pub(crate) fn step(&mut self, forward: bool) {
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
    pub(crate) fn current_match(&self) -> Option<(i32, u16, u32)> {
        self.point_match
            .or_else(|| self.matches.get(self.current).copied())
    }

    /// Select the first/last match at the Emacs search origin, wrapping at the
    fn anchored_index(&self, matches: &[(i32, u16, u32)], base_y: i64) -> usize {
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
    fn install_point_match(&mut self, point: (i32, u16, u32)) -> bool {
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

    /// Record how far the deepest hit in the installed batch reaches below the
    fn note_match_rows(&mut self, cols: usize) {
        let cols = cols.max(1);
        let depth = |&(_, _, end): &(i32, u16, u32)| end as usize / cols;
        let deepest = self
            .matches
            .iter()
            .map(depth)
            .chain(self.point_match.iter().map(depth))
            .max()
            .unwrap_or(0);
        self.max_match_rows = i32::try_from(deepest).unwrap_or(i32::MAX);
    }

    fn anchor_to_current(&mut self) {
        if let Some((row, col, _)) = self.current_match() {
            self.anchor_absolute_row = self.match_base_y + i64::from(row);
            self.anchor_col = col;
        }
    }

    /// Repaint fingerprint of the find bar: FNV-1a over the fields [`splice_find_bar`](crate::App::splice_find_bar) paints, folded into [`RepaintKey`](crate::RepaintKey) so an edit that does not move the highlight still presents (the key builders use `0` with no find open).
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut h = aterm_hash::Fnv1a64::new();
        h.write(self.query.as_bytes());
        for word in [
            self.cursor as u64,
            self.current as u64,
            self.matches.len() as u64,
            self.searched_lines.map_or(0, |lines| lines as u64 + 1),
        ] {
            h.write(&word.to_le_bytes());
        }
        for flag in [
            self.case_sensitive,
            self.is_regex,
            self.regex_error,
            self.truncated,
            self.match_capped,
            self.cut_short,
            self.history_away,
            self.results_dirty,
        ] {
            h.write_u8(u8::from(flag));
        }
        if let Some((row, start, end)) = self.point_match {
            h.write(&row.to_le_bytes());
            h.write(&start.to_le_bytes());
            h.write(&end.to_le_bytes());
        }
        h.finish()
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: while set, every find recompute on this thread reads as torn (`consistent == false`), as both full-history passes do when output moves `content_seq` under them.
    pub(crate) static FORCE_TORN_SEARCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn match_position(matches: &[(i32, u16, u32)], point: (i32, u16, u32)) -> Result<usize, usize> {
    matches.binary_search_by(|&(row, col, _)| (row, col).cmp(&(point.0, point.1)))
}

/// Map engine [`SearchMatch`]es (absolute rows, exclusive end, cell columns) to the overlay's selection coordinates: `sel_row = abs_row − base_y`, where `base_y` is `grid().base_y()` (independent of the display offset), with an inclusive end, sorted in visual order.
pub(crate) fn map_matches(matches: &[SearchMatch], base_y: i64) -> Vec<(i32, u16, u32)> {
    let mut out: Vec<(i32, u16, u32)> = matches
        .iter()
        .filter_map(|m| {
            let row = i32::try_from(i64::try_from(m.line).ok()? - base_y).ok()?;
            let start = u16::try_from(m.start_col).ok()?;
            let end = u32::try_from(m.end_col.saturating_sub(1)).ok()?;
            Some((row, start, end))
        })
        .collect();
    if out
        .windows(2)
        .any(|pair| (pair[0].0, pair[0].1) > (pair[1].0, pair[1].1))
    {
        out.sort_unstable_by_key(|&(row, start, _)| (row, start));
    }
    out
}

impl App {
    /// `(row geometry died, content moved)` for the open find, `None` without one.
    fn search_stamp_mismatch(&self, wid: crate::WindowId) -> Option<(bool, bool)> {
        let ws = self.windows.get(&wid)?;
        let search = ws.search.as_ref()?;
        let terminal = ws.front_terminal()?;
        let term = term_lock(&terminal.term);
        Some((
            term.absolute_row_revision() != search.match_absolute_row_revision
                || term.grid().history_renumber_epoch() != search.match_history_renumber_epoch,
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
            let mut t = term_lock(&term);
            if clear_selection_if_content_is_mutable(&mut t) {
                crate::app_mouse::note_selection_custody(&mut t, false);
            }
        }
    }

    /// Whether the focused content in `wid` can open its own search.
    pub(crate) fn can_find(&self, wid: crate::WindowId) -> bool {
        if let Some((instance, view)) = self.active_native_view(wid) {
            return match self.native_runtime.app(instance).map(|app| app.kind()) {
                Some(crate::native_app::AppKind::Settings) => true,
                Some(crate::native_app::AppKind::Editor) => matches!(
                    self.native_runtime.view_state(view),
                    Some(crate::native_app::AppViewState::Editor(state)) if state.buffer.as_ref()
                        .is_some_and(crate::native_editor::EditorBufferView::can_start_search)
                ),
                _ => false,
            };
        }
        self.front_terminal(wid).is_some()
    }

    /// Whether Find Next/Previous has a query in the focused content.
    pub(crate) fn can_find_again(&self, wid: crate::WindowId) -> bool {
        if let Some((_, view)) = self.active_native_view(wid) {
            return matches!(
                self.native_runtime.view_state(view),
                Some(crate::native_app::AppViewState::Editor(state))
                    if state.buffer.as_ref().is_some_and(
                        crate::native_editor::EditorBufferView::has_search_query
                    )
            );
        }
        self.front_terminal(wid).is_some()
            && self.windows.get(&wid).is_some_and(|ws| {
                ws.search.as_ref().map_or_else(
                    || !self.search_last_query.is_empty(),
                    |search| !search.query.is_empty(),
                )
            })
    }

    /// A user-level "Find" request (the Edit ▸ Find ▸ Find… menu item / a rebound Find action), routed by the canonical focused content.
    pub(crate) fn find_requested(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.find_requested_in(wid);
    }

    /// A routed key belongs to its originating window even if focus moved while a tab was starting and held the key for replay.
    pub(crate) fn find_requested_in(&mut self, wid: crate::WindowId) {
        if let Some((instance, _)) = self.active_native_view(wid) {
            let action = self
                .native_runtime
                .app(instance)
                .and_then(|app| match app.kind() {
                    crate::native_app::AppKind::Settings => Some("settings/search"),
                    crate::native_app::AppKind::Editor => Some("editor/find"),
                    crate::native_app::AppKind::Markdown
                    | crate::native_app::AppKind::SystemState
                    | crate::native_app::AppKind::Recovery => None,
                });
            if let Some(action) = action {
                let _ = self.dispatch_native_event(
                    wid,
                    crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new(action),
                        value: None,
                    }),
                );
            }
            return;
        }
        if self.front_terminal(wid).is_some() {
            self.search_enter_impl_in(wid, true, true);
        }
    }

    /// Frontmost-window convenience wrapper for headless tests.
    #[cfg(test)]
    pub(crate) fn search_enter(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_enter_impl_in(wid, true, true);
    }

    /// Enter (or refresh) terminal find in an explicit direction; a fresh find takes the app-sticky case/regex toggles.
    #[cfg(test)]
    pub(crate) fn search_enter_direction(&mut self, forward: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_enter_direction_in(wid, forward);
    }

    /// Window-targeted Emacs entry used by physical-key ownership.
    pub(crate) fn search_enter_direction_in(&mut self, wid: crate::WindowId, forward: bool) {
        self.search_enter_impl_in(wid, forward, false);
    }

    /// Shared entry path. Plain Cmd-F starts at the beginning of the buffer;
    fn search_enter_impl_in(
        &mut self,
        wid: crate::WindowId,
        forward: bool,
        legacy_from_start: bool,
    ) {
        let Some((term, session)) = self
            .front_terminal(wid)
            .map(|terminal| (terminal.term.clone(), terminal.session))
        else {
            if let Some(window) = self.windows.get_mut(&wid) {
                window.search = None;
                window.find_bar_hit = None;
            }
            return;
        };
        let (case_sensitive, is_regex) = (self.search_sticky_case, self.search_sticky_regex);
        let direction = SearchDirection::from_forward(forward);
        let origin = {
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
                FindOrigin {
                    top_visible_absolute_row: terminal.grid().top_visible_absolute_row(),
                    was_live: display_offset == 0,
                    on_alt_screen: terminal.is_alternate_screen(),
                    history_renumber_epoch: terminal.grid().history_renumber_epoch(),
                },
                anchor_absolute_row,
                anchor_col,
            )
        };
        let (find_origin, anchor_absolute_row, anchor_col) = origin;
        let already_open = self.windows.get(&wid).is_some_and(|ws| ws.search.is_some());
        if !already_open {
            self.find_origins
                .entry((session, wid))
                .or_insert(find_origin);
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
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

    /// Toggle match-case (⌥⌘C / a click on the `Aa` indicator): flip the active find's flag, remember it as the app-sticky default, and re-run.
    pub(crate) fn search_toggle_case(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_toggle_case_in(wid);
    }

    /// Window-targeted match-case toggle (the routed-keystroke twin, like [`Self::search_accept_in`]).
    pub(crate) fn search_toggle_case_in(&mut self, wid: crate::WindowId) {
        let Some(now) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.search.as_mut())
            .map(|s| {
                s.case_sensitive = !s.case_sensitive;
                s.case_sensitive
            })
        else {
            return;
        };
        self.search_sticky_case = now;
        self.search_recompute_in(wid);
    }

    /// Toggle regex mode (⌥⌘R / a click on the `.*` indicator): flip the active find's flag, remember it as the app-sticky default, and re-run.
    pub(crate) fn search_toggle_regex(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_toggle_regex_in(wid);
    }

    /// Window-targeted regex toggle — the twin of [`Self::search_toggle_case_in`].
    pub(crate) fn search_toggle_regex_in(&mut self, wid: crate::WindowId) {
        let Some(now) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.search.as_mut())
            .map(|s| {
                s.is_regex = !s.is_regex;
                s.is_regex
            })
        else {
            return;
        };
        self.search_sticky_regex = now;
        self.search_recompute_in(wid);
    }

    /// Frontmost-window convenience wrapper for headless tests; production callers use [`Self::search_recompute_in`].
    #[cfg(test)]
    pub(crate) fn search_recompute(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_recompute_in(wid);
    }

    /// Re-run window `wid`'s find over the screen and scrollback, then show the first match in its direction from the search point.
    pub(crate) fn search_recompute_in(&mut self, wid: crate::WindowId) {
        self.search_recompute_from_anchor_in(wid, false);
    }

    fn search_recompute_from_anchor_in(&mut self, wid: crate::WindowId, strict: bool) {
        let Some((term, session)) = self
            .front_terminal(wid)
            .map(|terminal| (terminal.term.clone(), terminal.session))
        else {
            return;
        };
        let rewrapping = self.rewrap_in_flight(session);
        if rewrapping
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.search.as_ref().is_some_and(|s| !s.query.is_empty()))
        {
            self.note_history_wanted(session);
        }
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
        let (
            matches,
            point_match,
            regex_error,
            truncated,
            searched_lines,
            match_capped,
            cut_short,
            base_y,
            absolute_row_revision,
            history_renumber_epoch,
            content_seq,
            cols,
            consistent,
        ) = if query.is_empty() {
            let (base_y, row_rev, epoch, seq, cols) = empty_query_term_snapshot(&term);
            (
                Vec::new(),
                None,
                false,
                false,
                None,
                false,
                false,
                base_y,
                row_rev,
                epoch,
                seq,
                cols,
                true,
            )
        } else {
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
                    search.searched_lines,
                    search.results.matches.len() >= aterm_search::MAX_SEARCH_MATCHES,
                    search.results.cut_short_lines > 0,
                    search.base_y,
                    search.absolute_row_revision,
                    search.history_renumber_epoch,
                    search.content_seq,
                    search.cols,
                    search.consistent,
                ),
                Err(_) => {
                    let terminal = term_lock(&term);
                    (
                        Vec::new(),
                        None,
                        true,
                        false,
                        None,
                        false,
                        false,
                        i64::try_from(terminal.grid().base_y()).unwrap_or(i64::MAX),
                        terminal.absolute_row_revision(),
                        terminal.grid().history_renumber_epoch(),
                        terminal.content_seq(),
                        usize::from(terminal.grid().cols()),
                        true,
                    )
                }
            }
        };
        #[cfg(test)]
        let consistent = consistent && !FORCE_TORN_SEARCH.with(std::cell::Cell::get);
        if !consistent {
            if let Some(search) = ws.search.as_mut() {
                search.results_dirty = true;
                let away = rewrapping && !query.is_empty();
                search.history_away = away;
                search.truncated |= away;
            }
            self.search_apply_current_in(wid);
            return;
        }
        if let Some(s) = ws.search.as_mut() {
            s.matches = matches;
            s.point_match = None;
            s.match_base_y = base_y;
            s.match_absolute_row_revision = absolute_row_revision;
            s.match_history_renumber_epoch = history_renumber_epoch;
            s.match_content_seq = content_seq;
            s.results_dirty = false;
            s.regex_error = regex_error;
            s.truncated = truncated || (rewrapping && !query.is_empty());
            s.searched_lines = searched_lines;
            s.match_capped = match_capped;
            s.cut_short = cut_short;
            s.history_away = rewrapping && !query.is_empty();
            if let Some(point) = point_match {
                s.install_point_match(point);
            } else {
                s.current = s.anchored_index(&s.matches, base_y);
            }
            s.note_match_rows(cols);
            s.anchor_to_current();
        }
        self.search_apply_current_in(wid);
    }

    /// Invalidate an open find when the focused terminal emits output: any change marks the batch dirty and clears a highlight the output could have moved.
    pub(crate) fn search_refresh_for_output(&mut self, session: u64) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let Some((expected_revision, expected_epoch, expected_content_seq)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map(|search| {
                (
                    search.match_absolute_row_revision,
                    search.match_history_renumber_epoch,
                    search.match_content_seq,
                )
            })
        else {
            return;
        };
        let Some(terminal) = self.front_terminal(wid) else {
            return;
        };
        if terminal.session != session {
            return;
        }
        let Ok(term) = terminal.term.try_lock() else {
            return;
        };
        let revision_stale = term.absolute_row_revision() != expected_revision
            || term.grid().history_renumber_epoch() != expected_epoch;
        let content_stale = term.content_seq() != expected_content_seq;
        drop(term);
        if revision_stale || content_stale {
            self.invalidate_search_results(wid, revision_stale);
        }
    }

    /// Highlight the current match through the text selection, scroll it into view,
    fn search_apply_current_in(&mut self, wid: crate::WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let Some(terminal) = ws.front_terminal() else {
            return;
        };
        let rows = ws.rows;
        let (mat, match_base_y, match_revision, match_epoch, match_seq, dirty, search_title) =
            match &ws.search {
                Some(s) => (
                    s.current_match(),
                    s.match_base_y,
                    s.match_absolute_row_revision,
                    s.match_history_renumber_epoch,
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
                || term.grid().history_renumber_epoch() != match_epoch
                || term.content_seq() != match_seq
            {
                // A footer splice is piecewise and a reflow renumbers wholesale, so a
                if clear_selection_if_content_is_mutable(&mut term) {
                    note_selection_custody(&mut term, false);
                }
                return;
            }
            let base_y = i64::try_from(term.grid().base_y()).unwrap_or(0);
            let delta = base_y - match_base_y;
            let row = mat.and_then(|(r, _, _)| i32::try_from(i64::from(r) - delta).ok());
            let cols = usize::from(term.grid().cols()).max(1);
            let end = mat.and_then(|(_, _, c1)| {
                let row = row?;
                let c1 = c1 as usize;
                let end_row = i32::try_from(c1 / cols).ok()?.checked_add(row)?;
                Some((end_row, u16::try_from(c1 % cols).ok()?))
            });
            {
                let sel = term.text_selection_mut();
                let had = sel.has_selection();
                sel.clear();
                let armed = if let (Some(row), Some((_, c0, _)), Some((end_row, end_col))) =
                    (row, mat, end)
                {
                    sel.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
                    sel.update_selection(end_row, end_col, SelectionSide::Right);
                    true
                } else {
                    false
                };
                if armed {
                    note_selection_custody(&mut term, true);
                } else if had {
                    note_selection_custody(&mut term, false);
                }
            }
            if let Some(row) = row {
                let clearance = i32::try_from(crate::find_bar::FIND_BAR_ROWS)
                    .unwrap_or(1)
                    .min(i32::from(rows.saturating_sub(1)))
                    .max(0);
                let target = base_y
                    .saturating_add(i64::from(row))
                    .saturating_sub(i64::from(clearance));
                term.scroll_to_absolute_row(u64::try_from(target).unwrap_or(0));
            }
        }
        if let Some(w) = &ws.os_window {
            crate::elevated_caption::set_title(w, &search_title);
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
            search.match_history_renumber_epoch = point.history_renumber_epoch;
            search.match_content_seq = point.content_seq;
            search.results_dirty = false;
            if let Some(mapped) = mapped {
                search.install_point_match(mapped);
                search.note_match_rows(point.cols);
                search.anchor_to_current();
            } else {
                search.matches.clear();
                search.point_match = None;
                search.current = 0;
                search.max_match_rows = 0;
            }
        }
        self.search_apply_current_in(wid);
    }

    /// Cycle to the next (`forward`) / previous match, wrapping.
    #[cfg(test)]
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

    /// Edit ▸ Find ▸ Find Next/Previous (⌘G/⇧⌘G).
    pub(crate) fn search_find_again(&mut self, forward: bool) -> Result<(), &'static str> {
        let wid = self
            .frontmost_window
            .ok_or("there is no window to search")?;
        self.search_find_again_in(wid, forward)
    }

    /// Window-targeted repeat for configured shortcuts.
    pub(crate) fn search_find_again_in(
        &mut self,
        wid: crate::WindowId,
        forward: bool,
    ) -> Result<(), &'static str> {
        if let Some((_, view)) = self.active_native_view(wid) {
            let Some(crate::native_app::AppViewState::Editor(state)) =
                self.native_runtime.view_state(view)
            else {
                return Err("the active app does not support Find Next or Find Previous");
            };
            if !state
                .buffer
                .as_ref()
                .is_some_and(crate::native_editor::EditorBufferView::has_search_query)
            {
                return Err("there is no editor search to step — find something first");
            }
            let command = if forward {
                crate::native_editor::EditorCommand::SearchNext
            } else {
                crate::native_editor::EditorCommand::SearchPrevious
            };
            self.dispatch_native_event(wid, crate::native_app::AppEvent::EditorCommand(command))
                .map_err(|_| "the editor could not navigate its search")?;
            return Ok(());
        }
        if self.front_terminal(wid).is_none() {
            return Err("there is no terminal search in this window");
        }
        if self.windows.get(&wid).is_some_and(|ws| ws.search.is_some()) {
            self.search_step_in(wid, forward);
            return Ok(());
        }
        if self.search_last_query.is_empty() {
            return Err("there is no search to step \u{2014} find something first");
        }

        let query = self.search_last_query.clone();
        let session_identity = self.front_terminal(wid).map(|terminal| {
            let term = term_lock(&terminal.term);
            (
                terminal.session,
                term.absolute_row_revision(),
                term.grid().history_renumber_epoch(),
            )
        });
        let anchor = self
            .search_last_anchor
            .filter(|(anchor_session, anchor_revision, anchor_epoch, ..)| {
                Some((*anchor_session, *anchor_revision, *anchor_epoch)) == session_identity
            })
            .map(|(_, _, _, row, start, end)| (row, start, end));
        self.search_enter_direction_in(wid, forward);
        if let Some(search) = self.windows.get_mut(&wid).and_then(|ws| ws.search.as_mut()) {
            search.set_query(query);
            if let Some((row, start, _)) = anchor {
                search.anchor_absolute_row = row;
                search.anchor_col = start;
            }
        }
        self.search_recompute_from_anchor_in(wid, anchor.is_some());
        Ok(())
    }

    /// `^S`/`^R` in find mode (isearch-repeat): step to the next/previous match, or on an empty query recall the last accepted one (`search_last_query`, emacs `C-s C-s`); forward lands on the first match, backward on the last.
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
                s.set_query(recalled);
                if forward {
                    s.anchor_absolute_row = i64::MIN;
                    s.anchor_col = 0;
                } else {
                    s.anchor_absolute_row = i64::MAX;
                    s.anchor_col = u16::MAX;
                }
            }
            self.search_recompute_in(wid);
            return;
        }
        self.search_step_in(wid, forward);
    }

    /// Apply one [`SearchEdit`] to window `wid`'s find query: a text change re-runs the search; a caret move only repaints (the caret is part of [`SearchState::fingerprint`]).
    pub(crate) fn search_edit_in(&mut self, wid: crate::WindowId, edit: SearchEdit) {
        let Some(changed) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.search.as_mut())
            .map(|search| search.edit(edit))
        else {
            return;
        };
        if changed {
            self.search_recompute_in(wid);
        } else if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }

    /// ⌘V in the find field: insert the clipboard at the caret ([`SearchState::edit`] strips control bytes and line breaks).
    pub(crate) fn search_paste_in(&mut self, wid: crate::WindowId) {
        if self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()) {
            return;
        }
        #[cfg(not(target_os = "linux"))]
        {
            if let Some(text) = crate::control::pbpaste() {
                self.search_edit_in(wid, SearchEdit::Insert(text));
            }
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(text) = crate::control::pbpaste_owned() {
                self.search_edit_in(wid, SearchEdit::Insert(text));
                return;
            }
            let Some(proxy) = self.proxy.clone() else {
                return;
            };
            let Some(owner) = self
                .windows
                .get(&wid)
                .and_then(|ws| ws.search.as_ref())
                .map(|search| search.paste_lifetime.clone())
            else {
                return;
            };
            let _ = std::thread::Builder::new()
                .name("aterm-x11-paste".into())
                .spawn(move || {
                    if let Some(text) = crate::control::pbpaste() {
                        let _ = proxy.send_event(crate::Wake::FindPasteReady { wid, owner, text });
                    }
                });
        }
    }

    /// Complete a foreign clipboard read only while its original find field remains open.
    #[cfg(any(target_os = "linux", all(test, unix)))]
    pub(crate) fn complete_search_paste_in(
        &mut self,
        wid: crate::WindowId,
        owner: std::sync::Arc<()>,
        text: String,
    ) -> bool {
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .is_none_or(|search| !std::sync::Arc::ptr_eq(&search.paste_lifetime, &owner))
        {
            return false;
        }
        self.search_edit_in(wid, SearchEdit::Insert(text));
        true
    }

    /// A click inside the find field's well: put the caret before character `slot` (the well's scroll plus the click's cell distance into it, both recorded by the paint in [`crate::FindBarHit`]), or at the end past the text.
    pub(crate) fn search_click_caret_in(&mut self, wid: crate::WindowId, slot: usize) {
        let Some(byte) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map(|search| {
                search
                    .query
                    .char_indices()
                    .nth(slot)
                    .map_or(search.query.len(), |(byte, _)| byte)
            })
        else {
            return;
        };
        self.search_edit_in(wid, SearchEdit::MoveTo(byte));
    }

    /// Leave find via ⏎ (emacs `RET`): the viewport stays where navigation left it and the current match keeps its highlight (ready for ⌘C); the query is remembered for `^S`/`^R` recall.
    #[cfg(test)]
    pub(crate) fn search_accept(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_accept_in(wid);
    }

    /// Window-targeted ⏎.
    pub(crate) fn search_accept_in(&mut self, wid: crate::WindowId) {
        if let Some((revision_stale, content_stale)) = self.search_stamp_mismatch(wid)
            && (revision_stale || content_stale)
        {
            self.invalidate_search_results(wid, revision_stale);
        }
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .is_some_and(|search| search.results_dirty)
        {
            self.search_recompute_in(wid);
        }
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .is_some_and(|search| search.results_dirty)
        {
            // A second concurrent mutation won the bounded retry. Keep the bar
            return;
        }
        if let Some((revision_stale, content_stale)) = self.search_stamp_mismatch(wid)
            && (revision_stale || content_stale)
        {
            self.invalidate_search_results(wid, revision_stale);
            return;
        }
        let session = self.front_terminal(wid).map(|terminal| terminal.session);
        let accepted = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map(|s| {
                let anchor = s
                    .current_match()
                    .map(|(row, start, end)| (s.match_base_y + i64::from(row), start, end));
                (
                    s.query.clone(),
                    s.match_absolute_row_revision,
                    s.match_history_renumber_epoch,
                    anchor,
                )
            });
        if let Some((q, revision, epoch, anchor)) = accepted.as_ref()
            && !q.is_empty()
        {
            self.search_last_query = q.clone();
            self.search_last_anchor = session
                .zip(*anchor)
                .map(|(session, (row, start, end))| (session, *revision, *epoch, row, start, end));
        }
        if let Some(session) = session {
            self.find_origins.remove(&(session, wid));
        }
        self.search_close_in(wid, false);
    }

    /// Leave find via ⎋/^G (emacs `C-g`): clear the highlight and restore the viewport captured at open by its absolute anchor ([`FindOrigin`]), so the user lands on the same line however much output streamed past.
    #[cfg(test)]
    pub(crate) fn search_cancel(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_cancel_in(wid);
    }

    /// Window-targeted ⎋/^G, bound to its press episode's window like [`Self::search_accept_in`].
    pub(crate) fn search_cancel_in(&mut self, wid: crate::WindowId) {
        if let Some((term, session)) = self
            .front_terminal(wid)
            .map(|terminal| (terminal.term.clone(), terminal.session))
        {
            // Removed whether or not the restore applies: ⎋ ends this find, and a
            if let Some(origin) = self.find_origins.remove(&(session, wid)) {
                let mut terminal = term_lock(&term);
                let coherent = origin.on_alt_screen == terminal.is_alternate_screen()
                    && origin.history_renumber_epoch == terminal.grid().history_renumber_epoch();
                if coherent {
                    if origin.was_live {
                        terminal.return_to_live();
                    } else {
                        terminal.scroll_to_absolute_row(origin.top_visible_absolute_row);
                    }
                }
            }
        }
        self.search_close_in(wid, true);
    }

    /// Leave find neutrally on the frontmost window: clear the highlight and restore the title, leaving the viewport.
    #[cfg(test)]
    pub(crate) fn search_exit(&mut self) {
        self.search_close(true);
    }

    /// [`Self::search_close_in`] on the frontmost window, for the tests.
    #[cfg(test)]
    fn search_close(&mut self, clear_selection: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.search_close_in(wid, clear_selection);
    }

    /// Shared find-close core: drop the overlay state (+ its clickable-indicator geometry), optionally clear the selection highlight (accept keeps it on the match), and restore the newest title owned by the remaining authority.
    pub(crate) fn search_close_in(&mut self, wid: crate::WindowId, clear_selection: bool) {
        let term = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone());
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let was_open = ws.search.is_some() || ws.find_bar_hit.is_some();
        ws.search = None;
        ws.find_bar_hit = None;
        if clear_selection && let Some(term) = term {
            term_lock(&term).text_selection_mut().clear();
        }
        if was_open && let Some(w) = &ws.os_window {
            crate::elevated_caption::set_title(
                w,
                crate::app_window::os_window_title(&ws.current_title),
            );
            w.request_redraw();
        }
    }

    /// Run one [`FindAction`] against the front window's find bar and report the state it left; `None` with no front window.
    pub(crate) fn find_cmd(&mut self, action: FindAction) -> Option<FindStatus> {
        let wid = self.frontmost_window?;
        match action {
            FindAction::Open => self.find_requested(),
            FindAction::Type(text) => self.search_edit_in(wid, SearchEdit::Insert(text)),
            FindAction::Edit(edit) => self.search_edit_in(wid, edit),
            FindAction::Step { forward } => self.search_step_in(wid, forward),
            FindAction::ToggleCase => self.search_toggle_case_in(wid),
            FindAction::ToggleRegex => self.search_toggle_regex_in(wid),
            FindAction::Accept => self.search_accept_in(wid),
            FindAction::Cancel => self.search_cancel_in(wid),
            FindAction::Status => {}
        }
        Some(self.find_status_in(wid))
    }

    /// Window `wid`'s find bar as the `find` verb reports it, read from the
    fn find_status_in(&self, wid: crate::WindowId) -> FindStatus {
        let Some(search) = self.windows.get(&wid).and_then(|ws| ws.search.as_ref()) else {
            return FindStatus::CLOSED;
        };
        FindStatus {
            open: true,
            query: search.query.clone(),
            case_sensitive: search.case_sensitive,
            is_regex: search.is_regex,
            regex_error: search.regex_error,
            matches: search.matches.len(),
            current: (!search.matches.is_empty()).then_some(search.current),
            truncated: search.truncated,
            stale: search.results_dirty,
            current_match: search.current_match(),
        }
    }
}

/// One step the `find` verb drives the find bar through; [`App::find_cmd`] runs the function its keystroke runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FindAction {
    /// Edit ▸ Find ▸ Find… (⌘F).
    Open,
    /// Insert text at the caret — a printable keypress, or an IME commit.
    Type(String),
    /// One field edit: caret motion or a kill.
    Edit(SearchEdit),
    /// ⌘S / ⌘R: step to the next/previous match, wrapping.
    Step { forward: bool },
    /// ⌥⌘C: toggle match-case, as the keystroke does.
    ToggleCase,
    /// ⌥⌘R: toggle regex, as the keystroke does.
    ToggleRegex,
    /// ⏎: leave find mode, staying on the current match.
    Accept,
    /// ⎋: leave find mode, restoring the pre-find viewport.
    Cancel,
    /// Report only — the one form that presses nothing.
    Status,
}

/// What the `find` verb answers with: the find bar's state after the action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FindStatus {
    pub(crate) open: bool,
    pub(crate) query: String,
    pub(crate) case_sensitive: bool,
    pub(crate) is_regex: bool,
    pub(crate) regex_error: bool,
    pub(crate) matches: usize,
    /// 0-based index of the current match within `matches`; `None` with an empty batch.
    pub(crate) current: Option<usize>,
    pub(crate) truncated: bool,
    /// The terminal changed since `matches` was counted ([`SearchState::results_dirty`]).
    pub(crate) stale: bool,
    /// `(row, start_col, end_col)`, `end_col` inclusive, in selection coordinates (0..rows the live screen, negative scrollback): not `search`'s absolute rows, so not a `line` argument.
    pub(crate) current_match: Option<(i32, u16, u32)>,
}

impl FindStatus {
    /// No find bar is up.
    pub(crate) const CLOSED: FindStatus = FindStatus {
        open: false,
        query: String::new(),
        case_sensitive: false,
        is_regex: false,
        regex_error: false,
        matches: 0,
        current: None,
        truncated: false,
        stale: false,
        current_match: None,
    };
}

/// The empty-query snapshot for `search_recompute_from_anchor_in` (base row, row
fn empty_query_term_snapshot(term: &std::sync::Mutex<Terminal>) -> (i64, u64, u64, u64, usize) {
    let terminal = term_lock(term);
    (
        i64::try_from(terminal.grid().base_y()).unwrap_or(i64::MAX),
        terminal.absolute_row_revision(),
        terminal.grid().history_renumber_epoch(),
        terminal.content_seq(),
        usize::from(terminal.grid().cols()),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        FindAction, SearchDirection, SearchEdit, SearchMatch, SearchState, map_matches,
        next_word_boundary, prev_word_boundary,
    };
    use crate::{App, WindowId, term_lock};

    /// An App whose session writes to an observer pipe, so a test can assert that a
    #[cfg(unix)]
    fn app_observing_pty() -> (App, [i32; 2]) {
        use std::sync::Arc;

        use aterm_session::sink::SinkWriter;

        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        (
            App::headless_for_test_with_sink(Arc::new(SinkWriter::new(pipe[1]))),
            pipe,
        )
    }

    #[cfg(unix)]
    fn drained(pipe: [i32; 2]) -> Vec<u8> {
        let mut bytes = [0u8; 256];
        let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
        if read <= 0 {
            return Vec::new();
        }
        bytes[..read as usize].to_vec()
    }

    /// The `find` verb never puts a byte on the PTY: with the bar closed every editing
    #[cfg(unix)]
    #[test]
    fn typing_into_the_find_bar_never_reaches_the_pty() {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);

        let status = app
            .find_cmd(FindAction::Type("rm -rf /".to_string()))
            .expect("a front window");
        assert!(!status.open, "no find bar was opened by typing at one");
        assert!(app.windows[&wid].search.is_none());
        assert_eq!(
            drained(pipe),
            Vec::<u8>::new(),
            "a find type against a CLOSED bar put bytes on the PTY"
        );

        assert!(app.find_cmd(FindAction::Open).expect("front").open);
        let status = app
            .find_cmd(FindAction::Type("needle".to_string()))
            .expect("front");
        assert!(status.open);
        assert_eq!(status.query, "needle");
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .expect("find active")
                .query,
            "needle"
        );
        assert_eq!(
            drained(pipe),
            Vec::<u8>::new(),
            "a find type against an OPEN bar put bytes on the PTY"
        );
    }

    #[cfg(unix)]
    #[test]
    fn delayed_find_paste_keeps_its_field_across_focus_but_not_close_and_reopen() {
        let (mut app, pipe) = app_observing_pty();
        let a = WindowId(0);
        app.find_requested_in(a);
        app.search_edit_in(a, SearchEdit::Insert("same".to_string()));
        let owner = app.windows[&a]
            .search
            .as_ref()
            .unwrap()
            .paste_lifetime
            .clone();
        assert!(app.complete_search_paste_in(a, owner.clone(), " query".to_string()));
        assert_eq!(app.windows[&a].search.as_ref().unwrap().query, "same query");

        app.search_close_in(a, false);
        assert!(!app.complete_search_paste_in(a, owner.clone(), " closed".to_string()));
        assert!(app.windows[&a].search.is_none());
        app.find_requested_in(a);
        app.search_edit_in(a, SearchEdit::Insert("same query".to_string()));
        assert!(!app.complete_search_paste_in(a, owner, " reopened".to_string()));
        assert_eq!(app.windows[&a].search.as_ref().unwrap().query, "same query");

        let owner = app.windows[&a]
            .search
            .as_ref()
            .unwrap()
            .paste_lifetime
            .clone();
        let session = app.front_terminal(a).unwrap().session;
        let sink = app.pool.get(session).unwrap().ctx.sink.clone();
        let sid = app.next_session_id;
        let b = app.insert_logical_window(crate::stub_session_with_sink(sid, sink), 24, 80);
        app.find_requested_in(b);
        app.search_edit_in(b, SearchEdit::Insert("other".to_string()));
        assert!(app.complete_search_paste_in(a, owner.clone(), " kept".to_string()));
        assert_eq!(app.frontmost_window, Some(b));
        assert_eq!(
            app.windows[&a].search.as_ref().unwrap().query,
            "same query kept"
        );
        assert_eq!(app.windows[&b].search.as_ref().unwrap().query, "other");
        assert!(!app.complete_search_paste_in(b, owner, " wrong window".to_string()));
        assert_eq!(app.windows[&b].search.as_ref().unwrap().query, "other");
        assert!(
            drained(pipe).is_empty(),
            "find paste never writes to either PTY"
        );
        drop(app);
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn delayed_find_paste_does_not_follow_a_replaced_tab_pane_or_window() {
        for replacement in ["tab", "pane", "window"] {
            let (mut app, pipe) = app_observing_pty();
            let a = WindowId(0);
            app.find_requested_in(a);
            app.search_edit_in(a, SearchEdit::Insert("same query".to_string()));
            let owner = app.windows[&a]
                .search
                .as_ref()
                .unwrap()
                .paste_lifetime
                .clone();
            let session = app.front_terminal(a).unwrap().session;
            let sink = app.pool.get(session).unwrap().ctx.sink.clone();
            let sid = app.next_session_id;
            let target = match replacement {
                "tab" => {
                    app.next_session_id += 1;
                    app.install_tab_session(a, crate::stub_session_with_sink(sid, sink), None);
                    a
                }
                "pane" => {
                    app.install_split_pane(
                        a,
                        crate::pane::SplitDir::Vertical,
                        crate::stub_session_with_sink(sid, sink),
                    )
                    .unwrap();
                    a
                }
                _ => {
                    app.close_window_logical(a);
                    app.insert_logical_window(crate::stub_session_with_sink(sid, sink), 24, 80)
                }
            };
            assert!(app.windows.get(&a).is_none_or(|ws| ws.search.is_none()));
            app.find_requested_in(target);
            app.search_edit_in(target, SearchEdit::Insert("same query".to_string()));
            assert!(app.front_terminal(target).unwrap().session != session);
            assert!(!app.complete_search_paste_in(a, owner, " late".to_string()));
            assert_eq!(
                app.windows[&target].search.as_ref().unwrap().query,
                "same query",
                "{replacement}"
            );
            assert!(drained(pipe).is_empty(), "{replacement}: no PTY input");
            drop(app);
            unsafe {
                libc::close(pipe[0]);
                libc::close(pipe[1]);
            }
        }
    }

    /// The verb runs the field editor the keystrokes run, and reports the query the
    #[test]
    fn find_key_edits_run_the_field_editor_the_keystrokes_run() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.find_cmd(FindAction::Open).expect("front").open);
        app.find_cmd(FindAction::Type("hello world".to_string()));
        let status = app
            .find_cmd(FindAction::Edit(SearchEdit::DeleteWordBack))
            .expect("front");
        assert_eq!(status.query, "hello ");
        app.find_cmd(FindAction::Edit(SearchEdit::MoveStart));
        let status = app
            .find_cmd(FindAction::Edit(SearchEdit::KillToEnd))
            .expect("front");
        assert_eq!(status.query, "");
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .expect("find active")
                .query,
            "",
            "the wire and the bar report one query, not two"
        );
    }

    /// `case`/`regex` toggle, as their keystrokes do, and the reply says which way the
    #[test]
    fn the_find_toggles_report_the_flag_they_left_behind() {
        let mut app = App::headless_for_test();
        assert!(app.find_cmd(FindAction::Open).expect("front").open);
        let before = app.find_cmd(FindAction::Status).expect("front");
        let after = app.find_cmd(FindAction::ToggleCase).expect("front");
        assert_eq!(after.case_sensitive, !before.case_sensitive);
        let after_regex = app.find_cmd(FindAction::ToggleRegex).expect("front");
        assert_eq!(after_regex.is_regex, !before.is_regex);

        assert!(!app.find_cmd(FindAction::Cancel).expect("front").open);
        assert!(
            !app.find_cmd(FindAction::Step { forward: true })
                .expect("front")
                .open
        );
    }

    /// A `SearchState` seeded with three matches (scrollback row -1, live rows 0/1),
    fn seeded() -> SearchState {
        SearchState {
            query: "foo".to_string(),
            matches: vec![(-1, 0, 2), (0, 2, 4), (0, 12, 14)],
            ..Default::default()
        }
    }

    #[test]
    fn native_front_never_arms_or_traps_host_terminal_search() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

        app.search_enter();
        assert!(
            app.windows[&wid].search.is_none(),
            "direct terminal-search entry fails closed on native content"
        );

        app.windows.get_mut(&wid).unwrap().search = Some(SearchState::default());
        app.search_exit();
        assert!(
            app.windows[&wid].search.is_none(),
            "close always clears stale host search even without a front terminal"
        );
    }

    /// Native-title ownership: find outranks the continuously refreshed canonical
    #[test]
    fn native_title_override_transitions_preserve_latest_canonical_title() {
        use crate::app_window::{WindowTitleAuthority, window_title_authority};

        assert_eq!(
            window_title_authority(false),
            WindowTitleAuthority::Canonical
        );
        assert_eq!(window_title_authority(true), WindowTitleAuthority::Search);

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

        app.windows.get_mut(&wid).unwrap().current_title = "build — tests are passing".to_string();
        assert_eq!(
            window_title_authority(app.windows[&wid].search.is_some()),
            WindowTitleAuthority::Search
        );
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().window_title(),
            search_status,
            "a canonical title refresh cannot mutate the find status"
        );

        app.search_close(true);
        assert!(app.windows[&wid].search.is_none());
        assert_eq!(
            app.windows[&wid].current_title, "build — tests are passing",
            "close restores the latest canonical cache, not literal aterm or stale pre-find text"
        );
        assert_eq!(
            window_title_authority(app.windows[&wid].search.is_some()),
            WindowTitleAuthority::Canonical
        );
    }

    /// An invalid pattern is not a miss: while the history is away for a rewrap,
    #[test]
    fn an_invalid_pattern_outranks_the_history_being_away() {
        let pattern = |regex_error, history_away| SearchState {
            query: "(".to_string(),
            regex_error,
            history_away,
            ..Default::default()
        };
        let (home, away, miss) = (
            pattern(true, false),
            pattern(true, true),
            pattern(false, true),
        );
        assert_eq!(away.window_title(), home.window_title());
        assert_eq!(away.spoken_status(), home.spoken_status());
        assert!(!away.window_title().contains(super::HISTORY_AWAY_NONE));
        assert_ne!(
            away.window_title(),
            miss.window_title(),
            "non-vacuous: a valid pattern's miss reads otherwise"
        );
    }

    fn set_query(app: &mut App, wid: WindowId, query: &str) {
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.search.as_mut())
            .expect("active search")
            .set_query(query.into());
    }

    /// Occurrences of `needle` in the visible rows, read off the grid: the ground
    #[cfg(test)]
    fn visible_occurrences(
        term: &std::sync::Arc<std::sync::Mutex<super::Terminal>>,
        needle: &str,
    ) -> usize {
        let t = term_lock(term);
        (0..t.rows())
            .map(|row| {
                t.get_line_text(i32::from(row), None)
                    .unwrap_or_default()
                    .matches(needle)
                    .count()
            })
            .sum()
    }

    fn hits(app: &App, wid: WindowId) -> usize {
        app.windows[&wid]
            .search
            .as_ref()
            .expect("active search")
            .matches
            .len()
    }

    /// A count the terminal has moved past is never stated as a census: after output
    #[test]
    fn output_since_the_count_is_marked_on_the_glass_and_on_the_wire() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"needle one\r\nneedle two\r\nneedle three\r\n");

        app.search_enter();
        set_query(&mut app, wid, "needle");
        app.search_recompute();
        let counted = hits(&app, wid);
        assert_eq!(counted, 3);
        let fresh = app.find_status_in(wid);
        assert!(!fresh.stale, "a just-run count is the present census");
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .expect("find")
                .count_qualifier(),
            "",
            "and is painted bare"
        );

        term_lock(&term).process(b"needle four\r\nneedle five\r\n");
        app.search_refresh_for_output(0);
        assert_eq!(
            visible_occurrences(&term, "needle"),
            5,
            "five hits are on the glass"
        );

        let stale = app.find_status_in(wid);
        assert_eq!(
            stale.matches, counted,
            "the batch is deliberately not re-run"
        );
        assert!(
            stale.stale,
            "so the reply says the count is older than the screen"
        );
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .expect("find")
                .count_qualifier(),
            "…",
            "and the bar marks it rather than asserting 3/3"
        );

        app.search_recompute();
        assert_eq!(hits(&app, wid), 5);
        let refreshed = app.find_status_in(wid);
        assert!(!refreshed.stale);
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .expect("find")
                .count_qualifier(),
            "",
            "an exact count is stated plainly — the marker is not a permanent scar"
        );
    }

    /// After a `clear` (`ED 2` + `ED 3`: no history left) a rows-grow appends blank rows
    #[test]
    fn growing_the_window_after_a_clear_keeps_the_visible_screen_searchable() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut t = term_lock(&term);
            for line in 0..40 {
                t.process(format!("old {line:02}\r\n").as_bytes());
            }
            t.process(b"\x1b[H\x1b[2J\x1b[3J");
            t.process(b"needle alpha\r\nneedle beta\r\nneedle gamma\r\n");
        }
        let before = visible_occurrences(&term, "needle");
        assert_eq!(before, 3, "fixture: three hits on the glass");

        app.search_enter();
        set_query(&mut app, wid, "needle");
        app.search_recompute();
        assert_eq!(hits(&app, wid), before, "the pre-resize count is exact");

        let (rows, cols) = {
            let t = term_lock(&term);
            (t.rows(), t.cols())
        };
        term_lock(&term).resize(rows.saturating_add(8), cols);
        let after = visible_occurrences(&term, "needle");
        assert_eq!(after, before, "the grow keeps every hit on the glass");

        app.search_recompute();
        assert_eq!(
            hits(&app, wid),
            after,
            "find must see every hit the reader can see after the window grew"
        );
    }

    /// A shrink that trims a screen-clear's trailing blanks moves the counter with the
    #[test]
    fn shrinking_the_window_after_a_screen_clear_keeps_history_correctly_keyed() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut t = term_lock(&term);
            for line in 0..40 {
                t.process(format!("needle {line:02}\r\n").as_bytes());
            }
            t.process(b"\x1b[H\x1b[2J");
        }
        let deep = "needle 03";
        let absolute_of = |app: &mut App, query: &str| -> Option<i64> {
            set_query(app, wid, query);
            app.search_recompute();
            let search = app.windows[&wid].search.as_ref().expect("find");
            search
                .current_match()
                .map(|(row, _, _)| search.match_base_y + i64::from(row))
        };

        app.search_enter();
        let before = absolute_of(&mut app, deep).expect("the deep needle is in history");

        let (rows, cols) = {
            let t = term_lock(&term);
            (t.rows(), t.cols())
        };
        let oldest_before = term_lock(&term).grid().oldest_absolute_row();
        let history_before = term_lock(&term).grid().scrollback_lines();
        term_lock(&term).resize(rows.saturating_sub(6).max(6), cols);
        assert_eq!(
            term_lock(&term).grid().scrollback_lines(),
            history_before,
            "fixture: the shrink only trimmed blanks — no row was demoted into history"
        );
        assert_eq!(
            term_lock(&term).grid().oldest_absolute_row(),
            oldest_before,
            "the trailing-blank trim slides no key"
        );

        let after = absolute_of(&mut app, deep).expect("the deep needle is still in history");
        assert_eq!(after, before, "the needle keeps its absolute row");
        let text = {
            let t = term_lock(&term);
            let oldest = i64::try_from(t.grid().oldest_absolute_row()).unwrap_or(0);
            let index = usize::try_from(after - oldest).expect("a retained history row");
            t.grid()
                .get_history_line(index)
                .map(|line| line.to_string())
                .unwrap_or_default()
        };
        assert!(
            text.contains(deep),
            "the reported absolute row holds the match: {text:?}"
        );
    }

    /// A find state holding `query` with the caret at its end — the state typing
    fn field(query: &str) -> SearchState {
        let mut state = SearchState::default();
        state.set_query(query.to_string());
        assert_eq!(
            state.cursor,
            query.len(),
            "typing parks the caret at the end"
        );
        state
    }

    /// Insertion happens at the caret, and the caret follows the inserted text.
    #[test]
    fn insert_lands_at_the_caret() {
        let mut s = field("bc");
        assert!(!s.edit(SearchEdit::MoveStart));
        assert_eq!((s.query.as_str(), s.cursor), ("bc", 0));

        assert!(s.edit(SearchEdit::Insert("a".into())));
        assert_eq!((s.query.as_str(), s.cursor), ("abc", 1));
        assert!(!s.edit(SearchEdit::MoveEnd));
        assert!(s.edit(SearchEdit::Insert("d".into())));
        assert_eq!((s.query.as_str(), s.cursor), ("abcd", 4));
    }

    /// ⎋/⏎/⇥ are commands in find mode: their text (`\u{1b}`, `\r`, `\t`, as winit
    #[test]
    fn control_characters_never_enter_the_query() {
        let mut s = field("");
        for control in ["\u{1b}", "\r", "\n", "\t", "\u{8}"] {
            assert!(
                !s.edit(SearchEdit::Insert(control.into())),
                "{control:?} is not query text"
            );
            assert_eq!(s.query, "", "{control:?} left the query untouched");
        }
        assert!(s.edit(SearchEdit::Insert("two\nlines\there".into())));
        assert_eq!(s.query, "twolineshere", "a pasted query stays one line");
    }

    /// Deletions respect the caret and the word boundaries readline uses: ⌫ / ⌦ by
    #[test]
    fn deletions_respect_caret_and_word_boundaries() {
        let mut s = field("alpha beta");
        assert!(s.edit(SearchEdit::DeleteBack));
        assert_eq!((s.query.as_str(), s.cursor), ("alpha bet", 9));

        assert!(s.edit(SearchEdit::DeleteWordBack));
        assert_eq!((s.query.as_str(), s.cursor), ("alpha ", 6));

        let mut s = field("src/main.rs");
        s.edit(SearchEdit::MoveStart);
        assert!(s.edit(SearchEdit::DeleteForward));
        assert_eq!((s.query.as_str(), s.cursor), ("rc/main.rs", 0));
        assert!(s.edit(SearchEdit::DeleteWordForward));
        assert_eq!(s.query, "/main.rs");

        let mut s = field("keep this");
        s.edit(SearchEdit::MoveWordLeft);
        assert_eq!(s.cursor, 5, "⌥← lands before the last word");
        assert!(s.edit(SearchEdit::KillToEnd));
        assert_eq!((s.query.as_str(), s.cursor), ("keep ", 5));
        assert!(s.edit(SearchEdit::KillToStart));
        assert_eq!((s.query.as_str(), s.cursor), ("", 0));
        assert!(!s.edit(SearchEdit::KillToEnd));
        assert!(!s.edit(SearchEdit::DeleteBack));
        assert!(!s.edit(SearchEdit::DeleteForward));
    }

    /// Every motion and edit is character-wise on a multi-byte query, and a caret
    #[test]
    fn editing_is_utf8_safe() {
        let mut s = field("héllo");
        assert!(s.edit(SearchEdit::DeleteBack));
        assert_eq!(s.query, "héll");
        s.edit(SearchEdit::MoveStart);
        s.edit(SearchEdit::MoveCharRight);
        assert_eq!(s.cursor, 1);
        s.edit(SearchEdit::MoveCharRight);
        assert_eq!(s.cursor, 3, "the caret steps OVER the two-byte é");
        assert!(s.edit(SearchEdit::DeleteBack));
        assert_eq!((s.query.as_str(), s.cursor), ("hll", 1));

        let mut s = field("éß");
        s.cursor = 1;
        assert!(s.edit(SearchEdit::Insert("x".into())));
        assert_eq!(
            s.query, "xéß",
            "a mid-codepoint caret floors to the boundary"
        );
        let mut s = field("é");
        s.cursor = 99;
        assert!(s.edit(SearchEdit::DeleteBack));
        assert_eq!(s.query, "");
        let mut s = field("abc");
        assert!(!s.edit(SearchEdit::MoveTo(99)));
        assert_eq!(s.cursor, 3);
    }

    /// The words ⌥→ then ⌥← find in `text`, as byte ranges.
    fn field_words(text: &str) -> Vec<(usize, usize)> {
        let mut words = Vec::new();
        let mut at = 0;
        loop {
            let end = next_word_boundary(text, at);
            let start = prev_word_boundary(text, end);
            if end == at || start < at {
                return words;
            }
            words.push((start, end));
            at = end;
        }
    }

    /// The words a double-click selects in `text`, as byte ranges.
    fn clicked_words(text: &str) -> Vec<(usize, usize)> {
        let smart = aterm_core::selection::SmartSelection::new();
        let mut words: Vec<(usize, usize)> = text
            .char_indices()
            .filter_map(|(at, _)| smart.word_boundaries_at(text, at))
            .collect();
        words.dedup();
        words
    }

    /// ⌥←/⌥→, ^W and ⌥⌫ step over a word whole in every script and stop where a
    #[test]
    fn word_motions_step_over_whole_words_like_a_double_click() {
        for word in [
            "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}",
            "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
            "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}",
            "\u{0926}\u{0930}\u{094D}\u{200D}\u{092F}\u{093E}",
            "\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}",
            "\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}",
            "cafe\u{301}",
        ] {
            let text = format!("x {word} y");
            let end = 2 + word.len();
            assert_eq!(
                field_words(&text),
                [(0, 1), (2, end), (end + 1, end + 2)],
                "{word:?}"
            );
            assert_eq!(field_words(&text), clicked_words(&text), "{word:?}");
            let mut s = field(&format!("x {word}"));
            assert!(s.edit(SearchEdit::DeleteWordBack));
            assert_eq!(s.query, "x ", "⌥⌫ deletes {word:?} whole");
        }
        for text in [
            "foo\u{200D}bar",
            "foo\u{200C}bar",
            "abc\u{202E}def",
            "a\u{2060}b",
            "x\u{200E}y",
            "ab\u{180E}cd",
        ] {
            let split = text.find(|c: char| !c.is_ascii()).unwrap();
            let after = split + text[split..].chars().next().unwrap().len_utf8();
            assert_eq!(
                field_words(text),
                [(0, split), (after, text.len())],
                "{text:?}"
            );
            assert_eq!(field_words(text), clicked_words(text), "{text:?}");
        }
    }

    /// The caret is painted state, so it is part of the repaint fingerprint.
    #[test]
    fn fingerprint_tracks_the_caret() {
        let mut s = field("abc");
        let at_end = s.fingerprint();
        s.edit(SearchEdit::MoveStart);
        assert_ne!(s.fingerprint(), at_end, "a caret move must re-present");
        s.edit(SearchEdit::MoveEnd);
        assert_eq!(s.fingerprint(), at_end, "and settle back when it returns");
    }

    /// `map_matches` turns absolute rows into selection rows (`abs − base_y`, negative
    #[test]
    fn map_matches_abs_to_selection_inclusive_sorted() {
        let base_y = 100;
        let ms = vec![
            SearchMatch::new(102, 2, 4),
            SearchMatch::new(99, 0, 3),
            SearchMatch::new(100, 5, 6),
            SearchMatch::new(100, 1, 3),
        ];
        assert_eq!(
            map_matches(&ms, base_y),
            vec![(-1, 0, 2), (0, 1, 2), (0, 5, 5), (2, 2, 3)]
        );
    }

    /// Out-of-`i32`/`u16`-range rows or columns are dropped, never wrapped, so a
    #[test]
    fn map_matches_drops_out_of_range() {
        let ms = vec![SearchMatch::new(0, 0, 1)];
        assert!(map_matches(&ms, i64::from(i32::MAX) + 10).is_empty());
        let ms = vec![
            SearchMatch::new(5, 0, 1),
            SearchMatch::new(usize::MAX, 0, 1),
        ];
        assert_eq!(map_matches(&ms, 5), vec![(0, 0, 0)]);
    }

    /// Next and previous walk the matches with wraparound at both ends.
    #[test]
    fn find_state_next_prev_wraparound() {
        let mut s = seeded();
        let n = s.matches.len();
        assert_eq!(n, 3);

        assert_eq!(s.current_match(), Some((-1, 0, 2)));
        s.step(true);
        assert_eq!(s.current_match(), Some((0, 2, 4)));
        s.step(true);
        assert_eq!(s.current_match(), Some((0, 12, 14)));
        s.step(true);
        assert_eq!(s.current, 0);
        assert_eq!(s.current_match(), Some((-1, 0, 2)));
        s.step(false);
        assert_eq!(s.current, n - 1);
        assert_eq!(s.current_match(), Some((0, 12, 14)));
        s.step(false);
        assert_eq!(s.current_match(), Some((0, 2, 4)));
        s.step(false);
        assert_eq!(s.current_match(), Some((-1, 0, 2)));
    }

    /// With no matches a step is a no-op with no current match, but it still sets the
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

    /// Truncated point navigation: an Emacs point outside the capped batch is
    #[test]
    fn truncated_point_install_selects_an_out_of_batch_point_without_touching_the_batch() {
        let mut state = SearchState {
            matches: (0..8).map(|row| (row, 0, 0)).collect(),
            truncated: true,
            ..Default::default()
        };
        let batch = state.matches.clone();
        assert!(!state.install_point_match((100, 0, 0)));
        assert_eq!(state.current_match(), Some((100, 0, 0)));
        assert_eq!(state.matches, batch, "the capped batch is untouched");
        assert!(state.install_point_match((3, 0, 0)));
        assert_eq!(state.current_match(), Some((3, 0, 0)));
        assert_eq!(state.matches, batch);
    }

    /// End-to-end state semantics over a real terminal: backward initial typing lands
    #[test]
    fn emacs_direction_reversal_repeat_wrap_empty_recall_and_cmd_f_legacy() {
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
    #[test]
    fn emacs_search_is_anchored_to_scrolled_viewport_in_both_directions() {
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

    /// Streaming output invalidates a batch without a rescan per chunk; the next repeat
    #[test]
    fn streaming_output_marks_search_dirty_then_repeat_refreshes_latest_content() {
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
        assert!(
            before_abs >= i64::try_from(term_lock(&term).grid().base_y()).unwrap(),
            "fixture hit must be on the live screen"
        );
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

    /// Scrollback twin of the test above, pinning the narrowed clear: output that
    #[test]
    fn streaming_output_keeps_a_scrollback_highlight_alive() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut terminal = term_lock(&term);
            terminal.process(b"NEEDLE here\r\n");
            for line in 0..200 {
                terminal.process(format!("filler {line:03}\r\n").as_bytes());
            }
        }

        app.search_enter_direction(true);
        set_query(&mut app, wid, "NEEDLE");
        app.search_recompute();
        let hit_abs = {
            let search = app.windows[&wid].search.as_ref().unwrap();
            assert_eq!(search.matches.len(), 1);
            search.match_base_y + i64::from(search.current_match().unwrap().0)
        };
        assert!(
            hit_abs < i64::try_from(term_lock(&term).grid().base_y()).unwrap(),
            "fixture hit must be in scrollback"
        );
        let highlighted = term_lock(&term)
            .selection_to_string()
            .expect("the match is highlighted");

        term_lock(&term).process(b"streamed non-match\r\n");
        app.search_refresh_for_output(0);
        assert!(
            app.windows[&wid].search.as_ref().unwrap().results_dirty,
            "the match VECTOR is still refused — only the highlight survives"
        );
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some(highlighted.as_str()),
            "an archived highlight still covers exactly the text it did before"
        );
    }

    /// ⌘F on a scrolled-back viewport with nothing to go to (empty query, no match)
    #[test]
    fn opening_find_with_nothing_to_show_does_not_move_the_viewport() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut terminal = term_lock(&term);
            for line in 0..120 {
                terminal.process(format!("line {line:03}\r\n").as_bytes());
            }
            terminal.scroll_display(17);
        }
        let reading = term_lock(&term).row_text(0).expect("a top visible row");

        app.search_enter();
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            17,
            "a bare ⌘F on an empty query must not move the view"
        );
        set_query(&mut app, wid, "no such text anywhere");
        app.search_recompute();
        assert_eq!(
            app.windows[&wid].search.as_ref().unwrap().matches.len(),
            0,
            "fixture query must miss"
        );
        assert_eq!(
            term_lock(&term).row_text(0).as_deref(),
            Some(reading.as_str()),
            "a zero-match query leaves the reader where they were"
        );
    }

    /// ⎋ restores a scrolled-back origin across a protected-footer splice (the
    #[test]
    fn cancel_restores_a_scrolled_back_origin_across_a_footer_splice() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut terminal = term_lock(&term);
            for line in 0..120 {
                terminal.process(format!("hit {line:03}\r\n").as_bytes());
            }
            terminal.scroll_display(23);
        }
        let reading = term_lock(&term).row_text(0).expect("a top visible row");

        app.search_enter();
        set_query(&mut app, wid, "hit 004");
        app.search_recompute();
        assert_ne!(
            term_lock(&term).grid().display_offset(),
            23,
            "find navigation must have moved the viewport away from the origin"
        );

        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r")
                .as_bytes(),
        );
        assert_eq!(term_lock(&term).absolute_row_revision(), 1);

        app.search_cancel();
        assert_eq!(
            term_lock(&term).row_text(0).as_deref(),
            Some(reading.as_str()),
            "⎋ puts the reader back on the LINE they were reading, splice or no splice"
        );
    }

    /// A tab/pane switch is not a find exit: the neutral close hands the title back and
    #[test]
    fn a_neutral_close_parks_the_find_origin_for_the_next_open() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        {
            let mut terminal = term_lock(&term);
            for line in 0..120 {
                terminal.process(format!("hit {line:03}\r\n").as_bytes());
            }
            terminal.scroll_display(31);
        }
        let reading = term_lock(&term).row_text(0).expect("a top visible row");

        app.search_enter();
        set_query(&mut app, wid, "hit 007");
        app.search_recompute();
        assert_ne!(term_lock(&term).grid().display_offset(), 31);

        app.search_close_in(wid, false);
        assert!(app.windows[&wid].search.is_none(), "the bar is gone");
        assert!(
            app.find_origins.contains_key(&(0, wid)),
            "but the origin is parked, not discarded"
        );

        app.search_enter();
        app.search_cancel();
        assert_eq!(
            term_lock(&term).row_text(0).as_deref(),
            Some(reading.as_str()),
            "the reopened find cancels home, not to where the interrupted one stopped"
        );
        assert!(
            !app.find_origins.contains_key(&(0, wid)),
            "⎋ is a user exit and takes the parked origin with it"
        );
    }

    #[test]
    fn query_edit_after_repeat_stays_at_emacs_point() {
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

    /// A fault injected into the real app right after a real step, named by
    #[cfg(unix)]
    type NavPatch = fn(&mut App, WindowId, &str);

    /// The shipped host, untouched.
    #[cfg(unix)]
    fn no_nav_patch(_: &mut App, _: WindowId, _: &str) {}

    /// `EmacsSearchNavigation`'s `Buggy = 1`, one member per law, each as the
    #[cfg(unix)]
    fn nav_member(member: &str, app: &mut App, wid: WindowId, action: &str) {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let term = app.front_terminal(wid).expect("terminal").term.clone();
        match (member, action) {
            ("leak", "OpenBackward") => {
                let sink = app.windows[&wid]
                    .active_terminal
                    .as_ref()
                    .expect("session")
                    .sink
                    .clone();
                sink.write_frame(b"\x12")
                    .expect("the observer pipe accepts a byte");
            }
            ("past the end", "RepeatForward") => {
                let search = app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap();
                if search.current == 0 {
                    search.current = search.matches.len();
                }
            }
            ("stale selection", "Output") => {
                let mut terminal = term_lock(&term);
                let sel = terminal.text_selection_mut();
                sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
                sel.update_selection(0, 3, SelectionSide::Right);
                sel.complete_selection();
            }
            ("viewport kept", "Cancel") => term_lock(&term).scroll_to_bottom(),
            ("hit dropped", "Accept") => term_lock(&term).text_selection_mut().clear(),
            _ => {}
        }
    }

    /// One run of the navigation trace: the real app, the model state it must
    #[cfg(unix)]
    struct NavRun<'m> {
        app: App,
        wid: WindowId,
        pipe: [i32; 2],
        pty_bytes: usize,
        model: &'m aterm_spec::derive::Model,
        state: aterm_spec::interp::State,
        patch: NavPatch,
    }

    #[cfg(unix)]
    impl NavRun<'_> {
        /// Drive one real step, inject the patch, advance the model through
        fn step(
            &mut self,
            actions: &[&'static str],
            drive: impl FnOnce(&mut App, WindowId),
        ) -> Result<(), String> {
            let action = *actions.last().expect("a step is at least one action");
            drive(&mut self.app, self.wid);
            (self.patch)(&mut self.app, self.wid, action);
            for &each in actions {
                let next = self.model.successors(each, &self.state);
                let [next] = next.as_slice() else {
                    return Err(format!(
                        "the model's {each} is not one successor from {:?}: {next:?}",
                        self.state
                    ));
                };
                self.state = next.clone();
            }
            self.pty_bytes += drained(self.pipe).len();
            let term = self
                .app
                .front_terminal(self.wid)
                .expect("terminal")
                .term
                .clone();
            let mut real = vec![
                (
                    "active",
                    i64::from(self.app.windows[&self.wid].search.is_some()),
                ),
                (
                    "selection",
                    i64::from(term_lock(&term).text_selection().has_selection()),
                ),
                ("pty_writes", i64::from(self.pty_bytes > 0)),
            ];
            if let Some(search) = self.app.windows[&self.wid].search.as_ref() {
                real.extend([
                    ("hits", i64::try_from(search.matches.len()).unwrap()),
                    ("current", i64::try_from(search.current).unwrap()),
                    (
                        "forward",
                        i64::from(search.direction == SearchDirection::Forward),
                    ),
                    ("dirty", i64::from(search.results_dirty)),
                ]);
            }
            for (var, value) in real {
                if self.state[var] != value {
                    return Err(format!(
                        "real {action}: {var} = {value}, the model's {action} says {} ({:?})",
                        self.state[var], self.state
                    ));
                }
            }
            Ok(())
        }
    }

    /// Drive the navigation trace on a real headless App whose PTY is an observer
    #[cfg(unix)]
    fn emacs_navigation_trace(
        model: &aterm_spec::derive::Model,
        patch: NavPatch,
    ) -> Result<(), String> {
        use aterm_window::keyboard::{KeyCode, PhysicalKey};

        let (app, pipe) = app_observing_pty();
        let mut run = NavRun {
            app,
            wid: WindowId(0),
            pipe,
            pty_bytes: 0,
            model,
            state: model.init_state(),
            patch,
        };
        let term = run
            .app
            .front_terminal(run.wid)
            .expect("terminal")
            .term
            .clone();
        let mut content = String::from("\x1b[>2u");
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
        assert!(
            term_lock(&term).grid().display_offset() > 0,
            "the fixture must start away from the live bottom"
        );
        let chord = |forward: bool| {
            let key = PhysicalKey::Code(if forward {
                KeyCode::KeyS
            } else {
                KeyCode::KeyR
            });
            move |app: &mut App, wid: WindowId| {
                app.terminal_emacs_search_pressed(wid, key, forward);
                app.release_physical_press(wid, key);
            }
        };
        let publish = |app: &mut App, wid: WindowId| {
            set_query(app, wid, "NEEDLE");
            app.search_recompute();
        };
        const THREE_HITS: [&str; 3] = ["PublishHit"; 3];

        run.step(&["OpenBackward"], chord(false))?;
        run.step(&THREE_HITS, publish)?;
        run.step(&["RepeatForward"], chord(true))?;
        run.step(&["RepeatBackward"], chord(false))?;
        run.step(&["Output"], |app, _| {
            term_lock(&term).process(b"\r\nstreamed non-match");
            app.search_refresh_for_output(0);
        })?;
        run.step(&["RefreshRepeatForward"], chord(true))?;

        let origin_offset = {
            let origin = run
                .app
                .find_origins
                .get(&(0, run.wid))
                .expect("parked find origin");
            let base_now = i64::try_from(term_lock(&term).grid().base_y()).unwrap();
            usize::try_from(base_now - i64::try_from(origin.top_visible_absolute_row).unwrap())
                .unwrap()
        };
        run.step(&["Cancel"], |app, _| app.search_cancel())?;
        let restored = term_lock(&term).grid().display_offset() == origin_offset;
        if restored != (run.state["viewport"] == run.state["origin"]) {
            return Err(format!(
                "real Cancel: viewport restored = {restored}, the model's Cancel says {} ({:?})",
                run.state["viewport"] == run.state["origin"],
                run.state
            ));
        }

        run.step(&["OpenBackward"], chord(false))?;
        run.step(&THREE_HITS, publish)?;
        run.step(&["Accept"], |app, _| app.search_accept())?;

        unsafe {
            libc::close(run.pipe[0]);
            libc::close(run.pipe[1]);
        }
        Ok(())
    }

    /// Tier-1 conformance for the derived `EmacsSearchNavigation` model: the trace
    #[cfg(unix)]
    #[test]
    fn emacs_navigation_conforms_to_derived_transition_model() {
        let model = aterm_models_render::emacs_search_navigation_model();
        emacs_navigation_trace(&model, no_nav_patch).unwrap_or_else(|why| panic!("{why}"));

        let members: [(&str, NavPatch, &str); 5] = [
            (
                "leak",
                |a, w, x| nav_member("leak", a, w, x),
                "OpenBackward",
            ),
            (
                "past the end",
                |a, w, x| nav_member("past the end", a, w, x),
                "RepeatForward",
            ),
            (
                "stale selection",
                |a, w, x| nav_member("stale selection", a, w, x),
                "Output",
            ),
            (
                "viewport kept",
                |a, w, x| nav_member("viewport kept", a, w, x),
                "Cancel",
            ),
            (
                "hit dropped",
                |a, w, x| nav_member("hit dropped", a, w, x),
                "Accept",
            ),
        ];
        for (member, patch, at) in members {
            let Err(refused) = emacs_navigation_trace(&model, patch) else {
                panic!("the healthy model must refuse the {member} member");
            };
            assert!(
                refused.contains(&format!("the model's {at} ")),
                "the {member} member must be refused at {at}: {refused}"
            );
        }
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let shipped = emacs_navigation_trace(&buggy, no_nav_patch)
            .expect_err("the shipped host must not conform to the Buggy = 1 model");
        assert!(shipped.contains("the model's"), "{shipped}");
    }

    /// Codex-style protected footer refreshes rebuild absolute coordinates without
    #[test]
    fn codex_protected_footer_refresh_preserves_backward_last_match() {
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

    /// After a protected-footer revision a forward repeat restarts inclusively at the
    #[test]
    fn codex_protected_footer_forward_refresh_keeps_row_zero_match() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let rows = app.windows[&wid].rows;
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        term_lock(&term).process(b"EDGE\r\nEDGE");

        app.search_enter_direction(false);
        set_query(&mut app, wid, "EDGE");
        app.search_recompute();
        assert_eq!(app.windows[&wid].search.as_ref().unwrap().matches.len(), 2);

        let region_bottom = rows - 2;
        term_lock(&term).process(
            format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
        );
        assert_eq!(term_lock(&term).absolute_row_revision(), 1);
        app.search_refresh_for_output(0);
        app.search_repeat(true);

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
    #[test]
    fn claude_classic_and_alternate_active_grid_search_routing() {
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

    /// Press custody: a search jump selects with no gesture around it, so it records
    #[test]
    fn a_search_jump_records_who_took_the_selection() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let Some(terminal) = app.front_terminal(wid).map(|t| t.term.clone()) else {
            panic!("headless app must front a terminal");
        };
        {
            let mut t = crate::term_lock(&terminal);
            for i in 0..6 {
                t.process(format!("needle{i} tail\r\n").as_bytes());
            }
        }

        let (base_y, revision, seq) = {
            let t = crate::term_lock(&terminal);
            (
                t.grid().base_y() as i64,
                t.absolute_row_revision(),
                t.content_seq(),
            )
        };
        let st = SearchState {
            query: "needle".to_string(),
            matches: vec![(1, 0, 5)],
            current: 0,
            match_base_y: base_y,
            match_absolute_row_revision: revision,
            match_content_seq: seq,
            ..SearchState::default()
        };
        app.windows.get_mut(&wid).unwrap().search = Some(st);

        app.search_apply_current_in(wid);

        let t = crate::term_lock(&terminal);
        assert!(
            t.text_selection().has_selection(),
            "precondition: the jump really did select the hit"
        );
        assert_eq!(
            t.last_custody_transition(),
            Some(aterm_core::terminal::CustodyTransition::UserSelect),
            "the search jump is a UserSelect producer and must say so — without this \
             the `custody` verb answers a stale transition beside a changed selection"
        );
    }
}
