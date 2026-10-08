// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The memory-pressure shed reaches a tab's history while that tab is on the
//! alternate screen.
//!
//! `Terminal::scrollback` is the active grid's store, and `CSI ?1049h` parks
//! the primary grid, the only tiered store, in the inactive slot.
//! `Terminal::history_scrollback` / `shed_history_scrollback` read and trim
//! the grid holding the history whichever slot it is in.
//!
//! Each test drives the real parser with 50k lines of history, then `?1049h`.

use aterm_core::scrollback::{Scrollback, ScrollbackStorage};
use aterm_core::terminal::Terminal;

const HISTORY_LINES: usize = 50_000;

/// A tiered terminal whose tiny hot/warm tiers push history into the
/// byte-budgeted cold tier, with a budget large enough to keep all of it.
fn tiered_terminal() -> Terminal {
    Terminal::with_scrollback(24, 120, 8, Scrollback::new(8, 16, 256 * 1024 * 1024))
}

/// Feed `HISTORY_LINES` lines of pseudo-random hex (a fixed LCG, so the run
/// is deterministic) — varied enough that the cold tier's compression cannot
/// fold the history down to nothing.
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
        "50k lines must put real bytes in the store (got {before})"
    );
    let lines_before = term.main_grid().scrollback_lines();

    term.process(b"\x1b[?1049h");
    assert!(
        term.modes().alternate_screen,
        "?1049h entered the alt screen"
    );
    // The active grid is the store-less alt buffer.
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

    // Leaving the alt screen brings back the trimmed primary: fewer lines,
    // not none.
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
    // Off the alt screen both accessors name the same store.
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

    // A store already under the target frees nothing, and says so.
    let again = term
        .shed_history_scrollback(|budget| budget)
        .expect("tiered");
    assert_eq!(again.freed(), 0, "{again:?}");

    // No tiered store: nothing to shed, on either screen.
    let mut ring_only = Terminal::new(24, 80);
    ring_only.process(b"hello\r\n\x1b[?1049h");
    assert!(ring_only.shed_history_scrollback(|b| b / 8).is_none());
    assert!(ring_only.history_scrollback_bytes().is_none());
}
