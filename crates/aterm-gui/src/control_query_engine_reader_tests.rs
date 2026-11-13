// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex};

use aterm_core::grid::Row;
use aterm_core::terminal::Terminal;

use super::{
    EngineSearchDirection, FullHistorySearch, cmd_line, cmd_search, search_full_history_direction,
};
use crate::session_waits::RewrapGauge;
use crate::term_lock;

fn corrupt_tiered_terminal() -> (Terminal, usize) {
    let mut sb = aterm_scrollback::Scrollback::with_block_size(4, 12, 10_000_000, 4);
    for i in 0..40 {
        match i {
            2 => sb.push_str("NEEDLE_before the corrupt block"),
            38 => sb.push_str("NEEDLE_after the corrupt block"),
            _ => sb.push_str(&format!("carried {i:03}")),
        }
    }
    assert!(sb.cold_line_count() > 2, "the first needle sits in cold");
    let corrupt = 5;
    sb.inject_corrupted_warm_block(corrupt);
    let mut t = Terminal::with_scrollback(6, 40, 5, sb);
    for i in 0..20 {
        if i % 4 == 1 {
            // 95 columns into a 40-column grid: one head row, two
            // continuation rows.
            t.process(format!("wrapped {i:03} {}\r\n", "w".repeat(83)).as_bytes());
        } else {
            t.process(format!("live {i:03}\r\n").as_bytes());
        }
    }
    t.process(format!("edge {}", "e".repeat(250)).as_bytes());
    assert_eq!(
        t.grid()
            .history_lines_from(0)
            .filter(Option::is_none)
            .count(),
        corrupt,
        "the fixture's corrupt block is still in the store"
    );
    (t, corrupt)
}

#[test]
fn native_search_over_a_corrupt_tiered_segment_keeps_absolute_rows() {
    let (t, corrupt) = corrupt_tiered_terminal();
    let oldest = t.grid().oldest_absolute_row();
    let term = Arc::new(Mutex::new(t));
    let reply = cmd_search(&term, "NEEDLE_", &RewrapGauge::default());
    let rows: Vec<u64> = reply
        .lines()
        .skip(1)
        .map(|hit| hit.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(reply.starts_with("OK 2"), "both needles are found: {reply}");
    assert_eq!(rows, [oldest + 2, oldest + 38 + corrupt as u64], "{reply}");
}

#[test]
fn line_over_a_corrupt_tiered_segment_says_unreadable_not_out_of_range() {
    let (t, _) = corrupt_tiered_terminal();
    let oldest = t.grid().oldest_absolute_row();
    let unreadable = oldest
        + t.grid()
            .history_lines_from(0)
            .position(|line| line.is_none())
            .expect("a corrupt row") as u64;
    let bottom = oldest + t.grid().scrollback_lines() as u64 + u64::from(t.rows());
    let term = Arc::new(Mutex::new(t));
    assert_eq!(cmd_line(&term, &unreadable.to_string()), "ERR unreadable\n");
    assert_eq!(cmd_line(&term, &bottom.to_string()), "ERR out of range\n");
    assert!(
        cmd_line(&term, &(oldest + 2).to_string()).contains("NEEDLE_before"),
        "a readable row in the same history remains available"
    );
}

#[test]
fn find_joins_a_soft_wrap_the_same_while_scrolled_back() {
    let scrolled = |offset: i32| {
        let mut t =
            Terminal::with_scrollback(6, 20, 8, aterm_scrollback::Scrollback::with_defaults());
        for i in 0..30 {
            t.process(format!("filler {i:02}\r\n").as_bytes());
        }
        t.process(b"abcdefghijklmnoNEEDLEtail\r\nplain row\r\nNEEDLE again");
        t.scroll_display(offset);
        assert_eq!(
            t.grid().display_offset(),
            usize::try_from(offset).expect("non-negative"),
            "precondition: scrolled"
        );
        let rows = t.rows();
        let live: Vec<bool> = (0..rows)
            .map(|row| t.grid().row_at_screen(row).is_some_and(Row::is_wrapped))
            .collect();
        assert!(live.iter().any(|&w| w), "the screen holds the soft wrap");
        if offset > 0 {
            let viewport: Vec<bool> = (0..rows)
                .map(|row| t.grid().row(row).is_some_and(Row::is_wrapped))
                .collect();
            assert_ne!(viewport, live, "offset {offset}");
        }
        let screen = t.grid().oldest_absolute_row() + t.grid().scrollback_lines() as u64;
        let term = Arc::new(Mutex::new(t));
        let anchor = (usize::try_from(screen).expect("row") + 3, 2);
        let search: FullHistorySearch = search_full_history_direction(
            &term,
            "NEEDLE",
            false,
            false,
            EngineSearchDirection::Forward,
            Some(anchor),
            false,
        )
        .expect("searched");
        let hits: Vec<(usize, usize, usize)> = search
            .results
            .matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect();
        let point = search.point_match.map(|m| (m.line, m.start_col, m.len()));
        (screen as usize, hits, point)
    };
    let (screen, live_hits, live_point) = scrolled(0);
    assert_eq!(
        live_hits,
        [(screen + 2, 15, 6), (screen + 5, 0, 6)],
        "the straddling hit at the row it starts on, and the one after"
    );
    assert_eq!(
        live_point,
        Some((screen + 5, 0, 6)),
        "the next hit after the anchor"
    );
    for offset in [1, 2, 20] {
        assert_eq!(
            scrolled(offset),
            (screen, live_hits.clone(), live_point),
            "scrolled back {offset}"
        );
    }
}

#[test]
fn search_names_a_history_line_cut_at_the_read_bound() {
    const BOUND: usize = 1024 * 1024;
    let terminal = |len: usize| {
        let mut line = b"NEEDLE ".to_vec();
        line.resize(len, b'A');
        let mut scrollback = aterm_scrollback::Scrollback::with_defaults();
        scrollback.push_line(aterm_scrollback::Line::from_bytes(&line));
        Arc::new(Mutex::new(Terminal::with_scrollback(
            4, 80, 1000, scrollback,
        )))
    };
    let header = |term: &Arc<Mutex<Terminal>>| {
        let reply = cmd_search(term, "needle", &RewrapGauge::default());
        reply.lines().next().expect("a header").to_string()
    };
    assert_eq!(header(&terminal(BOUND)), "OK 1 screen_first=1");
    let cut = terminal(BOUND + 1);
    assert_eq!(header(&cut), "OK 1 incomplete long-line screen_first=1");
    let oldest = term_lock(&cut).grid().oldest_absolute_row();
    let reply = cmd_line(&cut, &oldest.to_string());
    assert_eq!(reply.len(), "OK ".len() + BOUND + 1 + "\n".len());
    assert!(reply.starts_with("OK NEEDLE A"), "the row as written");
}
