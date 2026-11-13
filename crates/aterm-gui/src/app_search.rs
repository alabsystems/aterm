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
    const fn from_forward(forward: bool) -> Self {
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
/// caret is in, or else of the word before it.
fn prev_word_boundary(s: &str, at: usize) -> usize {
    aterm_grapheme::word_start_before(s, at, aterm_grapheme::is_word_char).unwrap_or(0)
}

/// Byte offset of the next word boundary after `at`: the end of the word the caret is
/// in, or else of the word after it — the forward twin of [`prev_word_boundary`],
/// read from the last word break before the caret.
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
/// have moved the text under it.
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

impl FindOrigin {
    /// Put `terminal`'s viewport back where this origin found it, while its numbering still holds.
    pub(crate) fn restore(&self, terminal: &mut Terminal) {
        let coherent = self.on_alt_screen == terminal.is_alternate_screen()
            && self.history_renumber_epoch == terminal.grid().history_renumber_epoch();
        if coherent {
            if self.was_live {
                terminal.return_to_live();
            } else {
                terminal.scroll_to_absolute_row(self.top_visible_absolute_row);
            }
        }
    }
}

/// Where a find over `terminal` starts: the viewport ⎋ restores, and the search point in `forward`'s direction (the cursor at the live tail, else the viewport's near edge).
fn find_start(terminal: &Terminal, forward: bool) -> (FindOrigin, i64, u16) {
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
    pub(crate) const fn plus(&self) -> &'static str {
        if self.truncated { "+" } else { "" }
    }

    /// The compact marks: `+` (a floor), `…` (stale), both, or none.
    pub(crate) const fn mark(&self) -> &'static str {
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
    /// does not.
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
    /// field in the same tab later has the same query.
    #[cfg(any(target_os = "linux", all(test, unix)))]
    paste_lifetime: std::sync::Arc<()>,
    pub(crate) query: String,
    /// Caret byte offset in [`Self::query`], always on a char boundary ([`Self::edit`] and [`Self::set_query`] keep it there).
    pub(crate) cursor: usize,
    pub(crate) matches: Vec<(i32, u16, u32)>,
    /// The exact point-relative hit when it lies outside the capped batch, kept apart
    /// so a truncated ⌘S/⌘R repeat never shifts 100k elements.
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

/// Structural repaint key for an open [`SearchState`], folded into [`crate::RepaintKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SearchRepaintKey {
    pub(crate) query_hash: u64,
    pub(crate) query_len: usize,
    pub(crate) cursor: usize,
    pub(crate) current: usize,
    pub(crate) matches_len: usize,
    pub(crate) searched_lines: Option<usize>,
    pub(crate) flags: u8,
    pub(crate) point_match: Option<(i32, u16, u32)>,
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
    pub(crate) const fn count_facts(&self) -> CountFacts {
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
            format!("aterm — find: {} (bad regex)", self.query)
        } else if self.matches.is_empty() && self.history_away {
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
    /// buffer edge.
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
    /// row it starts on, for a grid `cols` wide ([`Self::max_match_rows`]).
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

    /// Structural repaint key of the find bar over the fields [`splice_find_bar_from_plan`](crate::App::splice_find_bar_from_plan) paints, folded into [`RepaintKey`](crate::RepaintKey) so an edit that does not move the highlight still presents (`None` with no find open).
    pub(crate) fn fingerprint(&self) -> SearchRepaintKey {
        let mut flags = 0u8;
        for (bit, flag) in [
            self.case_sensitive,
            self.is_regex,
            self.regex_error,
            self.truncated,
            self.match_capped,
            self.cut_short,
            self.history_away,
            self.results_dirty,
        ]
        .into_iter()
        .enumerate()
        {
            flags |= u8::from(flag) << bit;
        }
        SearchRepaintKey {
            query_hash: aterm_hash::fnv1a_64(self.query.as_bytes()),
            query_len: self.query.len(),
            cursor: self.cursor,
            current: self.current,
            matches_len: self.matches.len(),
            searched_lines: self.searched_lines,
            flags,
            point_match: self.point_match,
        }
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
        let search = self.windows.get(&wid)?.search.as_ref()?;
        let (term, _) = self.front_shown(wid)?;
        let term = term_lock(&term);
        Some((
            term.absolute_row_revision() != search.match_absolute_row_revision
                || term.grid().history_renumber_epoch() != search.match_history_renumber_epoch,
            term.content_seq() != search.match_content_seq,
        ))
    }

    fn invalidate_search_results(&mut self, wid: crate::WindowId, revision_stale: bool) {
        let term = self.front_shown(wid).map(|(term, _)| term);
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
                ws.search.as_ref().map_or(
                    !self.search_last_query.is_empty() || !self.guest_search_last_query.is_empty(),
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
                    crate::native_app::AppKind::Mail => Some("mail/search"),
                    crate::native_app::AppKind::Markdown
                    | crate::native_app::AppKind::About
                    | crate::native_app::AppKind::SystemState
                    | crate::native_app::AppKind::Recovery
                    | crate::native_app::AppKind::Allocation
                    | crate::native_app::AppKind::Workers => None,
                    crate::native_app::AppKind::Tables => Some("tables/find"),
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

    /// Shared entry path.
    fn search_enter_impl_in(
        &mut self,
        wid: crate::WindowId,
        forward: bool,
        legacy_from_start: bool,
    ) {
        let Some((term, session)) = self.front_shown(wid) else {
            if let Some(window) = self.windows.get_mut(&wid) {
                window.search = None;
                window.find_bar_hit = None;
            }
            return;
        };
        let (case_sensitive, is_regex) = (self.search_sticky_case, self.search_sticky_regex);
        let direction = SearchDirection::from_forward(forward);
        let (find_origin, anchor_absolute_row, anchor_col) = find_start(&term_lock(&term), forward);
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
    #[cfg(test)]
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
    #[cfg(test)]
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

    /// Start window `wid`'s open find over the engine now shown, as a fresh find there would: ⎋ restores that engine's viewport, and the search point is its cursor or viewport edge (a buffer-edge point stays).
    pub(crate) fn search_restart_in(&mut self, wid: crate::WindowId) {
        let Some((term, session)) = self.front_shown(wid) else {
            return;
        };
        let Some(direction) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .map(|s| s.direction)
        else {
            return;
        };
        let (origin, row, col) =
            find_start(&term_lock(&term), direction == SearchDirection::Forward);
        self.find_origins.insert((session, wid), origin);
        if let Some(search) = self.windows.get_mut(&wid).and_then(|ws| ws.search.as_mut())
            && !matches!(
                (
                    search.direction,
                    search.anchor_absolute_row,
                    search.anchor_col
                ),
                (SearchDirection::Forward, i64::MIN, 0)
                    | (SearchDirection::Backward, i64::MAX, u16::MAX)
            )
        {
            search.anchor_absolute_row = row;
            search.anchor_col = col;
        }
    }

    /// Re-run window `wid`'s find over the screen and scrollback, then show the first match in its direction from the search point.
    pub(crate) fn search_recompute_in(&mut self, wid: crate::WindowId) {
        self.search_recompute_from_anchor_in(wid, false);
    }

    fn search_recompute_from_anchor_in(&mut self, wid: crate::WindowId, strict: bool) {
        let Some((term, session)) = self.front_shown(wid) else {
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

    /// Invalidate an open find in any window whose focused terminal is `session`: any output change marks the batch dirty and clears a highlight the output could have moved.
    pub(crate) fn search_refresh_for_output(&mut self, session: u64) {
        if !self.windows.values().any(|ws| ws.search.is_some()) {
            return;
        }
        let open: Vec<(crate::WindowId, u64, u64, u64)> = self
            .windows
            .iter()
            .filter_map(|(&wid, ws)| {
                let search = ws.search.as_ref()?;
                Some((
                    wid,
                    search.match_absolute_row_revision,
                    search.match_history_renumber_epoch,
                    search.match_content_seq,
                ))
            })
            .collect();
        for (wid, expected_revision, expected_epoch, expected_content_seq) in open {
            let Some((term, front)) = self.front_shown(wid) else {
                continue;
            };
            if front != session {
                continue;
            }
            let Ok(term) = term.try_lock() else {
                continue;
            };
            let revision_stale = term.absolute_row_revision() != expected_revision
                || term.grid().history_renumber_epoch() != expected_epoch;
            let content_stale = term.content_seq() != expected_content_seq;
            drop(term);
            if revision_stale || content_stale {
                self.invalidate_search_results(wid, revision_stale);
            }
        }
    }

    /// Highlight the current match through the text selection, scroll it into view,
    /// and show the find state in the window title.
    fn search_apply_current_in(&mut self, wid: crate::WindowId) {
        let Some((shown, _)) = self.front_shown(wid) else {
            return;
        };
        let Some(ws) = self.windows.get(&wid) else {
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
            let mut term = term_lock(&shown);
            if dirty
                || term.absolute_row_revision() != match_revision
                || term.grid().history_renumber_epoch() != match_epoch
                || term.content_seq() != match_seq
            {
                // A footer splice is piecewise and a reflow renumbers wholesale, so a
                // uniform base_y delta could select unrelated text, and ordinary edits
                // can move the columns: refuse until the find re-runs and refreshes the batch.
                if clear_selection_if_content_is_mutable(&mut term) {
                    note_selection_custody(&mut term, false);
                }
                // The find state the caller changed is still the bar's to show (a torn rerun
                // marks the count stale, and one after a rewrap stops saying the history is
                // away), so the window draws it.
                ws.request_redraw();
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
        let Some(term) = self.front_shown(wid).map(|(term, _)| term) else {
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
        let (query, last_anchor) = self.remembered_find();
        if query.is_empty() {
            return Err("there is no search to step \u{2014} find something first");
        }

        let query = query.to_owned();
        let session_identity = self.front_shown(wid).map(|(term, session)| {
            let term = term_lock(&term);
            (
                session,
                term.absolute_row_revision(),
                term.grid().history_renumber_epoch(),
            )
        });
        let anchor = last_anchor
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

    /// The last terminal find query the gesture being dispatched accepted, and where its match
    /// was: the person's ([`App::search_last_query`]), or for a control client's gesture its own,
    /// so a client's Find Next or `^S` on an empty query never recalls what the person searched
    /// for (a token checked for and not found, say) into a find bar it reads, as an Editor
    /// view's remembered search is split.
    fn remembered_find(&self) -> (&str, Option<crate::FindAnchor>) {
        if self.person_gesture_refused() {
            (&self.guest_search_last_query, self.guest_search_last_anchor)
        } else {
            (&self.search_last_query, self.search_last_anchor)
        }
    }

    /// Keep `query` and its match as the gesture being dispatched's last accepted terminal find
    /// ([`Self::remembered_find`]).
    fn remember_find(&mut self, query: String, anchor: Option<crate::FindAnchor>) {
        if self.person_gesture_refused() {
            self.guest_search_last_query = query;
            self.guest_search_last_anchor = anchor;
        } else {
            self.search_last_query = query;
            self.search_last_anchor = anchor;
        }
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
            s.direction = SearchDirection::from_forward(forward);
        }
        let query_empty = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.search.as_ref())
            .is_some_and(|s| s.query.is_empty());
        let recalled = self.remembered_find().0.to_owned();
        if query_empty && !recalled.is_empty() {
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

    /// ⌘V in the find field: insert the clipboard at the caret ([`SearchState::edit`] strips control bytes and line breaks). A control client's gesture reads no clipboard ([`Self::refuse_clipboard_gesture`]).
    pub(crate) fn search_paste_in(&mut self, wid: crate::WindowId) {
        if self.windows.get(&wid).is_none_or(|ws| ws.search.is_none())
            || self.refuse_clipboard_gesture("find-field paste")
        {
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
            let anchor = session
                .zip(*anchor)
                .map(|(session, (row, start, end))| (session, *revision, *epoch, row, start, end));
            self.remember_find(q.clone(), anchor);
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
        if let Some((term, session)) = self.front_shown(wid) {
            // Removed whether or not the restore applies: ⎋ ends this find, and a
            // stale entry would be inherited by the next ⌘F on this terminal.
            if let Some(origin) = self.find_origins.remove(&(session, wid)) {
                origin.restore(&mut term_lock(&term));
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
        let term = self.front_shown(wid).map(|(term, _)| term);
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

    /// Run one [`FindAction`] against the front window's terminal find bar; native apps remain unchanged and report closed. `None` with no front window.
    pub(crate) fn find_cmd(&mut self, action: FindAction) -> Option<FindStatus> {
        let wid = self.frontmost_window?;
        if self.front_terminal(wid).is_none() {
            return Some(FindStatus::CLOSED);
        }
        match action {
            FindAction::Open => self.find_requested_in(wid),
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
    /// [`SearchState`] the bar paints, so the wire and the glass agree.
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
    /// Open the terminal find bar (⌘F in a terminal).
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
/// revision, renumber epoch, content seq, width) under one short guard, in a free
/// fn so the lock census sees the guard end (see the call site).
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
#[path = "app_search_tests.rs"]
mod tests;
