// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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
