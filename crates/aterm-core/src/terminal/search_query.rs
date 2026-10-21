// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The query layer of a full-history search: what one query over a built index means.
//!
//! - A run of soft-wrapped rows is ONE line to the reader. The index holds each run joined
//!   ([`join_wrapped_rows`]), keyed at the row it starts on, and a [`WrapFrame`] carries
//!   points between the joined line's columns and the grid's rows.
//! - [`query_search_index`] answers one query in physical coordinates, with the match a
//!   find anchor selects.
//! - [`query_search_index_narrowing`] answers a find bar's next keystroke off the previous
//!   keystroke's occurrence frame ([`NarrowStack`]), with the results of
//!   [`query_search_index`].
//!
//! Isearch-style prefix narrowing. A find bar re-runs its query on EVERY text edit. On
//! unchanged content the index is the same, but a stateless query layer re-runs the complete
//! batch query each keystroke (a full range scan for 1–2-char literals, posting-list decode
//! and intersection for longer ones, up to 100k matches materialized) even though the
//! consumer is an incremental search whose grown query can only ever match INSIDE the
//! previous query's occurrence set.
//!
//! A [`NarrowStack`] keeps, for one index generation and one case sensitivity, a PREFIX
//! STACK of frames: the ascending occurrence-line sets of each typed prefix, produced by
//! `TerminalSearch::search_literal_narrowed` (whose results are differential-tested equal to
//! the batch path in `aterm-search`). A keystroke that extends a stacked prefix verifies
//! ONLY that frame's lines (no range scan, no posting decode); backspace re-verifies the
//! shorter prefix off its own frame; a non-suffix edit, a regex query or a capped previous
//! set falls back to the batch path (and reseeds where legal). The holder keeps a stack
//! only for the generation it was read in ([`super::SearchKey::same_generation`]): any
//! content change kills it by key mismatch, never by explicit invalidation.
//!
//! Frames hold OCCURRENCE lines (fold-level containment), not reported-match lines: the
//! subset property `matches(q + c) ⊆ occurrences(q)` is pure string logic
//! (`lower_fold(q + c) == lower_fold(q) + lower_fold(c)`), immune to the zero-display-width
//! match drops that make reported-match frames unsound (ﬁ/ß multi-char folds, prepend
//! clusters; see the engine tests).

use aterm_search::{DirectedFind, LineFrame};

use super::{SearchKey, Terminal};
use crate::search::{
    LineCells, MAX_SEARCH_MATCHES, NarrowedSearch, SearchDirection, SearchMatch,
    SearchOptionsError, SearchResults, TerminalSearch,
};

/// The soft-wrap layout one query is answered in.
///
/// A run of soft-wrapped grid rows is ONE line to the reader, and a hit that
/// straddles the boundary lives in neither row's own text, so the index holds
/// the run JOINED, keyed at its first absolute row. Columns in that line
/// therefore run past the grid's width and address the run's later rows;
/// [`WrapFrame::cols`] is the uniform stride that inverts them back to a
/// physical `(row, col)`.
///
/// [`WrapFrame::logical_anchor`] is the caller's physical find anchor carried
/// into the same frame. It is not a convenience: the lazy point walks address
/// the index directly, and a physical point compared against a joined line's
/// columns would step over every remaining hit on the line the anchor sits in.
#[derive(Clone, Copy, Debug)]
pub struct WrapFrame {
    /// Display columns one grid row holds — the width each continued row was
    /// padded out to before the join.
    cols: usize,
    /// The index joins soft-wrap runs ([`super::SearchLayout::Logical`]); an
    /// index of one line per row needs no re-expression.
    joined: bool,
    /// [`WrapFrame::to_logical`] of the caller's anchor, when it has one.
    logical_anchor: Option<(usize, usize)>,
}

impl WrapFrame {
    /// The frame a query under `key` is answered in, read in the hold that
    /// captured `key`, so the stride that un-joins a logical line's columns is
    /// the width of the grid the rows are read from. The caller's physical
    /// anchor is carried to the row its logical line starts on, walking no
    /// lower than the oldest row the index retains.
    #[must_use]
    pub fn under_key(t: &Terminal, key: &SearchKey, anchor: Option<(usize, usize)>) -> Self {
        let cols = usize::from(key.cols);
        let floor = u64::try_from(key.window().retained_base).unwrap_or(u64::MAX);
        Self {
            cols,
            joined: true,
            logical_anchor: anchor.and_then(|(row, col)| {
                let origin = t.logical_origin(u64::try_from(row).unwrap_or(u64::MAX), floor)?;
                let origin = usize::try_from(origin).unwrap_or(row);
                Some(Self::to_logical(cols, origin, row, col))
            }),
        }
    }

    /// The frame of an index under `key` that joins no rows
    /// ([`super::SearchLayout::Physical`]): every match is already physical, and
    /// the anchor addresses the index as it is.
    #[must_use]
    pub(crate) fn unjoined(key: &SearchKey) -> Self {
        Self {
            cols: usize::from(key.cols),
            joined: false,
            logical_anchor: None,
        }
    }

    /// The stride: display columns one grid row holds.
    #[must_use]
    pub fn cols(self) -> usize {
        self.cols
    }

    /// The caller's anchor in the joined line's columns, `(row its logical line
    /// starts on, column along that line)`; `None` without an anchor, or when its
    /// run is too deep to walk back over (the walks then take the physical anchor).
    #[must_use]
    pub fn logical_anchor(self) -> Option<(usize, usize)> {
        self.logical_anchor
    }

    /// Carry a physical `(row, col)` point into the joined line's columns,
    /// given the row that line starts on.
    fn to_logical(cols: usize, origin: usize, row: usize, col: usize) -> (usize, usize) {
        (
            origin,
            row.saturating_sub(origin)
                .saturating_mul(cols)
                .saturating_add(col),
        )
    }

    /// Re-express one engine match — keyed at a logical line's first row, its
    /// columns measured along the joined line — as the physical row and column
    /// the hit STARTS on, with `end_col` still measured from that row's column
    /// 0. A hit that straddles the wrap therefore keeps `end_col > cols`: one
    /// record to navigate to, carrying the span the highlight has to paint
    /// across the rows below it.
    #[must_use]
    pub fn to_physical(self, m: &SearchMatch) -> SearchMatch {
        if !self.joined {
            return m.clone();
        }
        // A zero-width grid cannot lay anything out; treat it as one column so
        // the division is total and the match keeps its own row.
        let cols = self.cols.max(1);
        let row_offset = m.start_col / cols;
        let start_col = m.start_col % cols;
        SearchMatch::new(
            m.line.saturating_add(row_offset),
            start_col,
            start_col.saturating_add(m.len()),
        )
    }

    /// [`Self::to_physical`] over a whole result set, in place.
    fn results_to_physical(self, results: &mut SearchResults) {
        for m in &mut results.matches {
            *m = self.to_physical(m);
        }
    }
}

/// Fold each soft-wrapped run of snapshot rows into the ONE logical line the
/// reader sees, in place: the run's head takes the whole text and the cells it
/// was read from, and every continuation becomes empty, so a hit that
/// straddles the boundary is found exactly once, keyed at the row it starts
/// on.
///
/// Every row a continuation follows is padded back out to the full `cols`
/// columns it occupies on the glass, however few of them hold text. Grid rows
/// are read as WRITTEN, and the cells past a row's written extent (a tail the
/// program never wrote or erased, the cell a wide glyph left when it wrapped
/// early) are not read, so without the padding such a row would splice its
/// neighbour's first column onto its last written one, both inventing matches
/// that are not on the glass and shifting every column after it. With it, the
/// joined line has a UNIFORM stride: column `c` of the run is row `c / cols`,
/// column `c % cols`, which is the arithmetic [`WrapFrame::to_physical`]
/// inverts. That uniformity is the whole contract, so it is built in rather
/// than counted ([`LineCells::append_wrapped_row`], the join a double-click's
/// word window takes too): row `k` of the run is drawn from column `k * cols`,
/// each of its cells at that plus the column the grid drew it in, and the
/// padding runs from where the row's own cells end. (Ruling 2026-10-11, r15
/// content-4: counting the columns by re-measuring each row's text broke the
/// stride wherever the text measured wider than the row is, a VS16 base the
/// grid left one cell wide in the last column, and moved every later hit on
/// the run one cell right per such row.)
///
/// Joining also makes the logical line the unit a regex sees, so `^`/`$` bind to
/// the reader's line rather than to a grid row.
pub fn join_wrapped_rows(
    lines: &mut [String],
    cells: &mut [LineCells],
    wrapped: &[bool],
    cols: usize,
) {
    // A zero-column grid lays nothing out; leaving the rows alone keeps every
    // coordinate the caller already has, and there is no boundary to straddle.
    if cols == 0 {
        return;
    }
    let mut head = 0usize;
    while head < lines.len() {
        let mut end = head.saturating_add(1);
        while wrapped.get(end).copied().unwrap_or(false) {
            end = end.saturating_add(1);
        }
        let continuations = end.saturating_sub(head).saturating_sub(1);
        if continuations > 0
            && let Some((slot, rest)) = lines.get_mut(head..).and_then(<[String]>::split_first_mut)
            && let Some((slot_cells, rest_cells)) = cells
                .get_mut(head..)
                .and_then(<[LineCells]>::split_first_mut)
        {
            let mut joined = std::mem::take(slot);
            let mut joined_cells = std::mem::take(slot_cells);
            // Row `k` of the run from column `k * cols`, padded out to it
            // from where the rows before end: a blank row (`CSI 2 K` erases a
            // row without clearing the next row's continuation flag) is
            // padded across its whole width.
            let mut row_start = 0usize;
            for (row, row_cells) in rest
                .iter_mut()
                .zip(rest_cells.iter_mut())
                .take(continuations)
            {
                row_start = row_start.saturating_add(cols);
                let continuation = std::mem::take(row);
                let continuation_cells = std::mem::take(row_cells);
                joined_cells.append_wrapped_row(
                    &mut joined,
                    &continuation,
                    &continuation_cells,
                    row_start,
                );
            }
            *slot = joined;
            *slot_cells = joined_cells;
        }
        head = end;
    }
}

/// One query over an index of joined soft-wrap runs: the batch results in
/// physical coordinates ([`WrapFrame::to_physical`]) and, with an anchor, the
/// match it selects: the first (`Forward`) or last (`Backward`) hit past it
/// (`strict`) or at it, wrapping to the other end, and past the batch cap the
/// index's own walk from the anchor carried into the joined frame.
#[allow(
    clippy::too_many_arguments,
    reason = "the wrap frame rides alongside the query's own surface, like the anchor"
)]
pub fn query_search_index(
    index: &TerminalSearch,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: SearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
    wrap: WrapFrame,
) -> Result<(SearchResults, Option<SearchMatch>), SearchOptionsError> {
    let mut results =
        index.search_results_opts_direction(query, case_sensitive, is_regex, direction)?;
    // Physical FIRST: `select_point_match` orders results against the caller's
    // physical anchor, so a joined line's columns must never reach it.
    wrap.results_to_physical(&mut results);
    let point_match = select_point_match(
        index,
        &results,
        query,
        case_sensitive,
        is_regex,
        direction,
        anchor,
        strict,
        wrap,
    )?;
    Ok((results, point_match))
}

/// Anchored point-match selection over ALREADY-COMPUTED batch results — split
/// out of [`query_search_index`] verbatim so the narrowing path shares the
/// exact anchored/point/cap-edge policy instead of re-implementing it.
#[allow(
    clippy::too_many_arguments,
    reason = "verbatim split of query_search_index's policy tail; every flag is load-bearing"
)]
fn select_point_match(
    index: &TerminalSearch,
    results: &SearchResults,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: SearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
    wrap: WrapFrame,
) -> Result<Option<SearchMatch>, SearchOptionsError> {
    let point_match = if let Some(anchor) = anchor {
        if results.matches.len() < MAX_SEARCH_MATCHES {
            let qualifies = |found: &SearchMatch| {
                let point = (found.line, found.start_col);
                match direction {
                    SearchDirection::Forward => {
                        if strict {
                            point > anchor
                        } else {
                            point >= anchor
                        }
                    }
                    SearchDirection::Backward => {
                        if strict {
                            point < anchor
                        } else {
                            point <= anchor
                        }
                    }
                    _ => false,
                }
            };
            match direction {
                SearchDirection::Forward => results
                    .matches
                    .iter()
                    .find(|found| qualifies(found))
                    .or_else(|| results.matches.first())
                    .cloned(),
                SearchDirection::Backward => results
                    .matches
                    .iter()
                    .rfind(|found| qualifies(found))
                    .or_else(|| results.matches.last())
                    .cloned(),
                _ => None,
            }
        } else {
            // Past the batch cap the walk addresses the INDEX, whose lines are
            // joined soft-wrap runs — so it takes the anchor in that frame and
            // hands back a match that still has to be re-expressed physically.
            //
            // A walk that found nothing but could not finish some line has no
            // point to offer, and the batch beside it already carries the
            // matches and counts the line (`cut_short_lines`): keep the batch.
            match index.find_direction_opts(
                query,
                case_sensitive,
                is_regex,
                DirectedFind {
                    anchor: wrap.logical_anchor.unwrap_or(anchor),
                    direction,
                    inclusive: !strict,
                    wrap: true,
                },
            ) {
                Err(SearchOptionsError::ScanCutShort) => None,
                found => found?.map(|found| wrap.to_physical(&found)),
            }
        }
    } else {
        None
    };
    Ok(point_match)
}

/// One typed prefix's occurrence frame.
#[derive(Debug)]
struct NarrowFrame {
    query: String,
    /// Ascending retained lines whose folded text contains the folded query —
    /// absolute rows, held as `u32` offsets from the frame's own base, so a
    /// frame costs four bytes a line past row `u32::MAX` too.
    occurrence_lines: LineFrame,
}

/// A find bar's narrowing state over one index: the occurrence frames of the
/// prefixes it typed, valid for the index generation they were read in (the
/// holder's key) and for one case sensitivity (checked here).
#[derive(Debug)]
pub struct NarrowStack {
    case_sensitive: bool,
    /// Prefix stack: `frames[i].query` is a byte-prefix of `frames[i+1].query`.
    frames: Vec<NarrowFrame>,
}

/// Retained-entry budget per stack (~1.6 MB of `u32` offsets at the bound). The
/// SHALLOWEST frames (the biggest — short prefixes) are dropped first; a deep
/// backspace past a dropped frame reseeds from the engine, which is always
/// correct, merely batch-priced.
const NARROW_STACK_MAX_ENTRIES: usize = 4 * MAX_SEARCH_MATCHES;

#[cfg(any(test, debug_assertions))]
thread_local! {
    static NARROWED_QUERY_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, debug_assertions))]
fn record_narrowed_query() {
    NARROWED_QUERY_STEPS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, debug_assertions)))]
fn record_narrowed_query() {}

/// Take (read and reset) how many queries on THIS thread were served off a
/// narrowing frame — the deterministic reach signal for the wiring tests
/// (thread-local so parallel tests on other terminals cannot perturb it).
#[cfg(any(test, debug_assertions))]
pub fn take_narrowed_query_steps() -> usize {
    NARROWED_QUERY_STEPS.with(|count| {
        let value = count.get();
        count.set(0);
        value
    })
}

/// [`query_search_index`] with narrowing: literal queries consult and extend
/// `stack`; regex and empty queries (and any capped backward edge) take the
/// batch path unchanged. Results are ALWAYS equal to [`query_search_index`] on
/// the same inputs — narrowing changes which lines get verified, never what a
/// verified result is (engine-differential-tested).
///
/// `stack` comes in as the holder's stack for this index generation, or `None`;
/// a stack of the other case sensitivity is never read (a case-sensitive frame
/// lacks the lines only a folded query matches). It goes out as the stack to
/// keep for the next keystroke, or `None` when there is none.
#[allow(
    clippy::too_many_arguments,
    reason = "the narrowing stack rides alongside query_search_index's own surface"
)]
pub fn query_search_index_narrowing(
    index: &TerminalSearch,
    stack: &mut Option<NarrowStack>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: SearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
    wrap: WrapFrame,
) -> Result<(SearchResults, Option<SearchMatch>), SearchOptionsError> {
    if is_regex || query.is_empty() {
        // Regex has no prefix-subset property; empty is the neutral bar state.
        return query_search_index(
            index,
            query,
            case_sensitive,
            is_regex,
            direction,
            anchor,
            strict,
            wrap,
        );
    }
    let mut session = stack
        .take()
        .filter(|stack| stack.case_sensitive == case_sensitive);
    // Deepest stacked prefix of this query (equality included: a repeat or
    // backspaced-to query re-verifies off its OWN frame — occurrences ⊇
    // matches, so that is complete too).
    let frame_index = session.as_ref().and_then(|session| {
        session
            .frames
            .iter()
            .rposition(|frame| query.starts_with(frame.query.as_str()))
    });
    let narrowed = {
        let prev_lines = session
            .as_ref()
            .and_then(|session| frame_index.map(|at| &session.frames[at].occurrence_lines));
        // COST GUARD: a frame is a SUBSET property, not a cost guarantee. The
        // keystroke that turns a COMMON prefix into a RARE query inverts the
        // comparison — the engine's trigram intersection visits a handful of
        // candidates while the inherited frame still holds every line the
        // common prefix occurred on (measured on a 60k-line log-shaped probe:
        // 2.60 ms off a 60 000-line frame against 0.085 ms through the index,
        // on exactly the keystroke whose latency the user feels most). So
        // narrow only while the frame is no bigger than the candidate set the
        // engine would walk itself; the fallback is the plain seed path, which
        // reseeds a fresh, SMALLER frame for the next keystroke, so the guard
        // costs one step and never compounds.
        //
        // The comparison carries a 2x slack because the two sides do not cost
        // the same PER CANDIDATE: a frame line is one verify, while an engine
        // candidate additionally pays the driver list's varint decode and a
        // binary-search probe per remaining trigram (~1.5x per candidate on
        // the same probe). Only a DECISIVELY smaller candidate set is worth
        // abandoning a frame for.
        let prev_lines = prev_lines.filter(|lines| {
            match index.literal_candidate_bound(query, case_sensitive) {
                // Sub-trigram query: the engine range-scans every retained
                // line, so any frame is at least as cheap.
                None => true,
                Some(bound) => lines.len() as u64 <= bound.saturating_mul(2),
            }
        });
        if prev_lines.is_some() {
            record_narrowed_query();
        }
        index.search_literal_narrowed(query, case_sensitive, prev_lines)
    };
    let NarrowedSearch {
        results: narrowed_results,
        occurrence_lines,
    } = narrowed;
    // A capped narrowed walk carries the FORWARD (oldest-retaining) cap edge;
    // a capped BACKWARD batch retains the newest matches instead — fall back
    // so the direction-aware edge semantics stay byte-identical.
    let capped = narrowed_results.matches.len() >= MAX_SEARCH_MATCHES;
    let mut results = if capped && direction == SearchDirection::Backward {
        index.search_results_opts_direction(query, case_sensitive, false, direction)?
    } else {
        narrowed_results
    };
    // Physical FIRST, for the reason [`query_search_index`] states.
    wrap.results_to_physical(&mut results);
    if let Some(occurrence_lines) = occurrence_lines {
        let mut session = session.take().unwrap_or_else(|| NarrowStack {
            case_sensitive,
            frames: Vec::new(),
        });
        // Keep only ancestors (prefixes) of this query, then push/replace so
        // the stack invariant (each frame a byte-prefix of the next) holds.
        session.frames.truncate(frame_index.map_or(0, |at| at + 1));
        if session
            .frames
            .last()
            .is_some_and(|frame| frame.query == query)
        {
            session.frames.pop();
        }
        session.frames.push(NarrowFrame {
            query: query.to_string(),
            occurrence_lines,
        });
        let mut total: usize = session
            .frames
            .iter()
            .map(|frame| frame.occurrence_lines.len())
            .sum();
        while total > NARROW_STACK_MAX_ENTRIES && session.frames.len() > 1 {
            let dropped = session.frames.remove(0);
            total = total.saturating_sub(dropped.occurrence_lines.len());
        }
        *stack = Some(session);
    } else if let Some(session) = session {
        // Capped walk: no frame for THIS query, but the stacked ancestors are
        // still valid for other extensions of their prefixes — keep them.
        *stack = Some(session);
    }
    let point_match = select_point_match(
        index,
        &results,
        query,
        case_sensitive,
        false,
        direction,
        anchor,
        strict,
        wrap,
    )?;
    Ok((results, point_match))
}

#[cfg(test)]
mod tests {
    use super::{NarrowStack, WrapFrame, query_search_index, query_search_index_narrowing};
    use crate::search::{SearchDirection, SearchResults, TerminalSearch};

    fn index(lines: &[&str]) -> TerminalSearch {
        let mut index = TerminalSearch::new();
        for line in lines {
            index.index_scrollback_line(line);
        }
        index
    }

    /// `query` through the narrowing layer and the batch layer: (narrowed
    /// results, batch results, whether it was served off a frame).
    fn step(
        index: &TerminalSearch,
        stack: &mut Option<NarrowStack>,
        query: &str,
        case_sensitive: bool,
    ) -> (SearchResults, SearchResults, bool) {
        let wrap = WrapFrame {
            cols: 80,
            joined: true,
            logical_anchor: None,
        };
        let forward = SearchDirection::Forward;
        let _ = super::take_narrowed_query_steps();
        let (narrowed, _) = query_search_index_narrowing(
            index,
            stack,
            query,
            case_sensitive,
            false,
            forward,
            None,
            false,
            wrap,
        )
        .expect("a literal query cannot fail");
        let served_off_a_frame = super::take_narrowed_query_steps() == 1;
        let (batch, _) = query_search_index(
            index,
            query,
            case_sensitive,
            false,
            forward,
            None,
            false,
            wrap,
        )
        .expect("a literal query cannot fail");
        (narrowed, batch, served_off_a_frame)
    }

    /// A case-sensitive frame holds only the lines the exact prefix occurs on,
    /// so a folded query grown from it must not be verified over it: the stack
    /// the holder hands back for the index generation is read only at its own
    /// case sensitivity. NEGATIVE CONTROL: the folded `fin` verified over the
    /// case-sensitive `fi` frame, which the cost guard alone would take, keeps
    /// `find two` and loses `Find one` and `FIND three`; and the same
    /// keystrokes typed at one sensitivity are served off the frame, with the
    /// batch's results.
    #[test]
    fn a_narrowing_stack_is_read_only_at_its_own_case_sensitivity() {
        let index = index(&["Find one", "find two", "FIND three", "nothing"]);
        for (seed_case_sensitive, narrows) in [(false, true), (true, false)] {
            let mut stack = None;
            let (seeded, batch, _) = step(&index, &mut stack, "fi", seed_case_sensitive);
            assert_eq!(seeded, batch, "seed {seed_case_sensitive}");
            let frame = &stack
                .as_ref()
                .and_then(|stack| stack.frames.last())
                .expect("the seed keystroke leaves a frame")
                .occurrence_lines;
            let bound = index
                .literal_candidate_bound("fin", false)
                .expect("`fin` is a trigram query");
            assert!(
                frame.len() as u64 <= bound * 2,
                "precondition: the cost guard alone takes the frame"
            );
            let over_the_frame = index
                .search_literal_narrowed("fin", false, Some(frame))
                .results;
            assert_eq!(
                over_the_frame.matches.len(),
                if seed_case_sensitive { 1 } else { 3 },
                "seed {seed_case_sensitive}"
            );
            let (grown, batch, served_off_a_frame) = step(&index, &mut stack, "fin", false);
            assert_eq!(grown, batch, "seed {seed_case_sensitive}");
            assert_eq!(grown.matches.len(), 3, "every case of `fin`");
            assert_eq!(served_off_a_frame, narrows, "seed {seed_case_sensitive}");
        }
    }
}
