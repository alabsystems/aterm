// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Full-history search query execution over a built index.

use aterm_search::{DirectedFind, LineFrame};

use super::{SearchKey, Terminal};
use crate::search::{
    LineCells, MAX_SEARCH_MATCHES, NarrowedSearch, SearchDirection, SearchMatch,
    SearchOptionsError, SearchResults, TerminalSearch,
};

/// The soft-wrap layout one query is answered in.
#[derive(Clone, Copy, Debug)]
pub struct WrapFrame {
    /// Display columns one grid row holds.
    cols: usize,
    /// The index joins soft-wrap runs ([`super::SearchLayout::Logical`]).
    joined: bool,
    /// [`WrapFrame::to_logical`] of the caller's anchor, when it has one.
    logical_anchor: Option<(usize, usize)>,
}

impl WrapFrame {
    /// The frame a query under `key` is answered in, read in the hold that captured `key`.
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

    /// The frame of an index under `key` that joins no rows ([`super::SearchLayout::Physical`]).
    #[must_use]
    #[inline]
    pub(crate) const fn unjoined(key: &SearchKey) -> Self {
        Self {
            cols: key.cols as usize,
            joined: false,
            logical_anchor: None,
        }
    }

    /// The stride: display columns one grid row holds.
    #[must_use]
    #[inline]
    pub const fn cols(self) -> usize {
        self.cols
    }

    /// The caller's anchor in the joined line's columns.
    #[must_use]
    #[inline]
    pub const fn logical_anchor(self) -> Option<(usize, usize)> {
        self.logical_anchor
    }

    /// Carry a physical `(row, col)` point into the joined line's columns, given the row that line
    /// starts on.
    #[inline]
    const fn to_logical(cols: usize, origin: usize, row: usize, col: usize) -> (usize, usize) {
        (
            origin,
            row.saturating_sub(origin)
                .saturating_mul(cols)
                .saturating_add(col),
        )
    }

    /// Re-express one engine match.
    #[must_use]
    #[inline]
    pub fn to_physical(self, m: &SearchMatch) -> SearchMatch {
        if !self.joined {
            return m.clone();
        }
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
    #[inline]
    fn results_to_physical(self, results: &mut SearchResults) {
        for m in &mut results.matches {
            *m = self.to_physical(m);
        }
    }
}

/// Fold each soft-wrapped run of snapshot rows into the ONE logical line the reader sees.
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

/// One query over an index of joined soft-wrap runs ([`WrapFrame::to_physical`]).
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

/// Anchored point-match selection over ALREADY-COMPUTED batch results ([`query_search_index`]).
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
    /// Ascending retained lines whose folded text contains the folded query.
    occurrence_lines: LineFrame,
}

/// A find bar's narrowing state over one index.
#[derive(Debug)]
pub struct NarrowStack {
    case_sensitive: bool,
    /// Prefix stack: `frames[i].query` is a byte-prefix of `frames[i+1].query`.
    frames: Vec<NarrowFrame>,
}

/// Retained-entry budget per stack (~1.6 MB of `u32` offsets at the bound).
const NARROW_STACK_MAX_ENTRIES: usize = 4 * MAX_SEARCH_MATCHES;

#[cfg(any(test, debug_assertions))]
thread_local! {
    static NARROWED_QUERY_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, debug_assertions))]
#[inline]
fn record_narrowed_query() {
    NARROWED_QUERY_STEPS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(any(test, debug_assertions)))]
#[inline]
const fn record_narrowed_query() {}

/// Take (read and reset) how many queries on THIS thread were served off a narrowing frame.
#[cfg(any(test, debug_assertions))]
pub fn take_narrowed_query_steps() -> usize {
    NARROWED_QUERY_STEPS.with(|count| {
        let value = count.get();
        count.set(0);
        value
    })
}

/// [`query_search_index`] with narrowing: literal queries consult and extend `stack`.
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
        let prev_lines = prev_lines.filter(|lines| {
            match index.literal_candidate_bound(query, case_sensitive) {
                // Sub-trigram query.
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
    let capped = narrowed_results.matches.len() >= MAX_SEARCH_MATCHES;
    let mut results = if capped && direction == SearchDirection::Backward {
        index.search_results_opts_direction(query, case_sensitive, false, direction)?
    } else {
        narrowed_results
    };
    wrap.results_to_physical(&mut results);
    if let Some(occurrence_lines) = occurrence_lines {
        let mut session = session.take().unwrap_or_else(|| NarrowStack {
            case_sensitive,
            frames: Vec::new(),
        });
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
