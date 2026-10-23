// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Memory-pressure history shedding while a tab is on the alternate screen.

use aterm_core::scrollback::{Scrollback, ScrollbackStorage};
use aterm_core::terminal::Terminal;

const HISTORY_LINES: usize = 12_000;

fn tiered_terminal() -> Terminal {
    Terminal::with_scrollback(24, 120, 8, Scrollback::new(8, 16, 256 * 1024 * 1024))
}

fn fill_history(term: &mut Terminal) {
    let mut state: u64 = 0x2026_0926;
    let mut chunk = String::with_capacity(64 * 1024);
    for i in 0..HISTORY_LINES {
        chunk.push_str(&format!("{i:06} "));
        for _ in 0..12 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            chunk.push_str(&format!("{:08x}", state >> 32));
        }
        chunk.push_str("\r\n");
        if chunk.len() > 60 * 1024 {
            term.process(chunk.as_bytes());
            chunk.clear();
        }
    }
    term.process(chunk.as_bytes());
}

#[test]
fn shed_trims_the_primary_history_while_the_alt_screen_is_up() {
    let mut term = tiered_terminal();
    fill_history(&mut term);
    let before = term
        .history_scrollback_bytes()
        .expect("a tiered terminal has a history store");
    assert!(
        before > 1 << 20,
        "{HISTORY_LINES} lines must put real bytes in the store (got {before})"
    );
    let lines_before = term.main_grid().scrollback_lines();

    term.process(b"\x1b[?1049h");
    assert!(
        term.modes().alternate_screen,
        "?1049h entered the alt screen"
    );
    assert!(
        term.scrollback().is_none(),
        "the active (alt) grid carries no tiered store"
    );
    assert_eq!(
        term.history_scrollback()
            .map(ScrollbackStorage::budgeted_memory_used),
        Some(before),
        "the history accessor sees the parked primary's store"
    );

    let keep = before / 4;
    let shed = term
        .shed_history_scrollback(|_budget| keep)
        .expect("the alt-screen tab's history is reachable");
    assert_eq!(shed.before, before);
    assert!(
        shed.freed() > 0 && shed.after < before,
        "the shed must give bytes back ({shed:?})"
    );
    assert!(
        shed.after <= keep,
        "evicted down to the requested watermark ({} > {keep})",
        shed.after
    );
    let store = term.history_scrollback().expect("store still attached");
    assert_eq!(store.budgeted_memory_used(), shed.after);
    assert_eq!(
        store.memory_budget(),
        256 * 1024 * 1024,
        "the budget is restored after the shed, so history can grow again"
    );

    term.process(b"\x1b[?1049l");
    assert!(!term.modes().alternate_screen);
    let lines_after = term.grid().scrollback_lines();
    assert!(
        lines_after < lines_before && lines_after > 0,
        "the primary lost its oldest lines only ({lines_before} -> {lines_after})"
    );
}

#[test]
fn shed_on_the_primary_screen_is_unchanged_and_ring_only_is_none() {
    let mut term = tiered_terminal();
    fill_history(&mut term);
    let before = term.history_scrollback_bytes().expect("tiered");
    assert_eq!(
        term.scrollback()
            .map(ScrollbackStorage::budgeted_memory_used),
        term.history_scrollback()
            .map(ScrollbackStorage::budgeted_memory_used)
    );
    let shed = term
        .shed_history_scrollback(|_| before / 4)
        .expect("tiered");
    assert!(shed.freed() > 0, "{shed:?}");

    let again = term
        .shed_history_scrollback(|budget| budget)
        .expect("tiered");
    assert_eq!(again.freed(), 0, "{again:?}");

    let mut ring_only = Terminal::new(24, 80);
    ring_only.process(b"hello\r\n\x1b[?1049h");
    assert!(ring_only.shed_history_scrollback(|b| b / 8).is_none());
    assert!(ring_only.history_scrollback_bytes().is_none());
}

#[test]
fn shed_sees_and_strips_the_links_the_ring_holds() {
    const ROWS: usize = 200;
    const URL_BYTES: usize = 1024;
    let mut term = Terminal::with_scrollback(24, 80, 6_123, Scrollback::with_defaults());
    let text = |row: usize| {
        let mut s = format!("R{row:04} ");
        s.extend(std::iter::repeat_n('.', 80 - s.len()));
        s
    };
    let mut stream = Vec::new();
    for row in 0..ROWS {
        for (col, ch) in text(row).chars().enumerate() {
            let mut url = format!("https://e.example/{row}/{col}/");
            url.extend(std::iter::repeat_n('x', URL_BYTES - url.len()));
            stream.extend_from_slice(format!("\x1b]8;;{url}\x07{ch}").as_bytes());
        }
        stream.extend_from_slice(b"\x1b]8;;\x07\r\n");
    }
    for _ in 0..24 {
        stream.extend_from_slice(b"plain\r\n");
    }
    term.process(&stream);

    let links = term.main_grid().history_link_bytes();
    assert!(
        links >= ROWS * 80 * URL_BYTES,
        "every row's links are still in the ring ({links} bytes)"
    );
    let bytes = term.history_scrollback_bytes().expect("tiered");
    assert!(
        bytes >= links,
        "the reclaimable figure the pressure lane and the cap read must include the \
         ring's links ({bytes} < {links})"
    );

    let budget = term.history_scrollback().expect("store").memory_budget();
    let keep = budget / 8;
    let shed = term.shed_history_scrollback(|b| b / 8).expect("tiered");
    assert_eq!(shed.before, bytes);
    let left = term.main_grid().history_link_bytes();
    assert!(
        left <= keep / 4,
        "the shed strips the ring's links to the lowered allowance ({left} > {})",
        keep / 4
    );
    assert!(
        shed.freed() >= links - left,
        "the stripped links are reported freed ({shed:?}, {links} -> {left})"
    );
    assert_eq!(
        term.history_scrollback().expect("store").memory_budget(),
        budget,
        "the budget is restored after the shed"
    );
    assert_eq!(term.main_grid().history_link_bytes(), left);

    let grid = term.main_grid();
    let linked: Vec<bool> = (0..ROWS)
        .map(|row| {
            let line = grid.get_history_line(row).expect("history row");
            assert_eq!(line.to_string(), text(row), "row {row} keeps its text");
            line.hyperlinks().is_some_and(|spans| !spans.is_empty())
        })
        .collect();
    let stripped = linked.iter().take_while(|&&l| !l).count();
    assert!(
        stripped > 0 && linked[stripped..].iter().all(|&l| l),
        "links went oldest row first: {linked:?}"
    );
}
