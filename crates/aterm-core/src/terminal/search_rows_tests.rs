// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_grid::Row;
use aterm_scrollback::{Line, Scrollback};

use super::{AbsRowText, SearchRows};
use crate::grid::Grid;
use crate::search::LineCells;
use crate::terminal::Terminal;
use crate::terminal::selection::MAX_SCROLLBACK_LINE_SCAN_BYTES;

/// A terminal whose history runs through a TIERED store (a five-row ring over it)
/// holding a corrupt warm block between its cold tier and its healthy warm blocks,
/// a needle on each side of it, and soft-wrapped lines in the ring and tiered
/// history and across the history/screen edge: the shapes a skipping reader, or
/// one that took the wrap flag from a different row than the text, gets wrong.
fn corrupt_tiered_terminal() -> (Terminal, usize) {
    let (mut t, corrupt) = crate::terminal::search_index::tests::corrupt_tiered_terminal();
    for i in 0..20 {
        if i % 4 == 1 {
            // 95 columns into a 40-column grid: one head row, two continuation rows.
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

/// A ring-only terminal whose history has been capped, so the oldest absolute rows
/// are EVICTED and the oldest retained row is a soft-wrap continuation whose head
/// is gone.
fn evicted_terminal() -> Terminal {
    let mut t = Terminal::new(4, 20);
    t.set_scrollback_line_limit(Some(9));
    for i in 0..30 {
        t.process(format!("row {i:02} {}\r\n", "r".repeat(30)).as_bytes());
    }
    if !oldest_is_a_continuation(&t) {
        let keep = t.grid().scrollback_lines() - 1;
        t.set_scrollback_line_limit(Some(keep));
    }
    assert!(t.grid().oldest_absolute_row() > 0, "rows were evicted");
    assert!(
        oldest_is_a_continuation(&t),
        "the oldest retained row is a continuation whose head was evicted"
    );
    t
}

fn oldest_is_a_continuation(t: &Terminal) -> bool {
    t.grid()
        .history_lines_from(0)
        .next()
        .flatten()
        .is_some_and(|line| line.is_wrapped())
}

type Read = (Vec<String>, Vec<LineCells>, Vec<bool>);

/// The per-row oracle: what `abs_row_text_within` (at the search bound),
/// `written_line_cells` and `abs_row_continues` answer row by row.
fn per_row(t: &Terminal, first: u64, count: usize) -> Read {
    let oldest = i128::from(t.grid().oldest_absolute_row());
    let scrollback = t.grid().scrollback_lines() as i128;
    let mut read: Read = Default::default();
    for abs in first..first + count as u64 {
        read.0.push(
            match t.abs_row_text_within(abs, MAX_SCROLLBACK_LINE_SCAN_BYTES) {
                AbsRowText::Text(text) => text,
                AbsRowText::Evicted | AbsRowText::OutOfRange | AbsRowText::Unreadable => {
                    String::new()
                }
            },
        );
        let row = i128::from(abs) - oldest - scrollback;
        read.1.push(
            (i128::from(abs) >= oldest)
                .then(|| i32::try_from(row).ok())
                .flatten()
                .and_then(|row| t.written_line_cells(row))
                .map(|(_, cells)| cells)
                .unwrap_or_default(),
        );
        read.2.push(t.abs_row_continues(abs));
    }
    read
}

fn dense(t: &Terminal, first: u64, count: usize) -> Read {
    let mut rows = SearchRows::default();
    t.read_search_rows(first, count, &mut rows);
    (rows.text, rows.cells, rows.wrapped)
}

/// Every window `first..first + count` over evicted rows, the whole retained
/// history, the screen and past its bottom.
fn assert_matches_everywhere(t: &Terminal) {
    let oldest = t.grid().oldest_absolute_row();
    let bottom = oldest + t.grid().scrollback_lines() as u64 + u64::from(t.rows());
    let lo = oldest.saturating_sub(3);
    for first in lo..=bottom + 2 {
        for count in 0..=usize::try_from(bottom + 3 - first).expect("small") {
            assert_eq!(
                dense(t, first, count),
                per_row(t, first, count),
                "rows {first}..{} (oldest {oldest})",
                first + count as u64
            );
        }
    }
}

/// The whole retained window, read at the live tail, then scrolled back by each of
/// `offsets`: every read equals the live one and the per-row readers.
fn assert_scroll_invariant(t: &mut Terminal, offsets: &[i32]) {
    let oldest = t.grid().oldest_absolute_row();
    let total = t.grid().scrollback_lines() + usize::from(t.rows());
    assert_eq!(
        t.grid().display_offset(),
        0,
        "precondition: at the live tail"
    );
    let live = dense(t, oldest, total);
    for &offset in offsets {
        t.scroll_display(offset);
        assert_eq!(
            t.grid().display_offset(),
            usize::try_from(offset).expect("non-negative"),
            "precondition: scrolled back"
        );
        assert_eq!(dense(t, oldest, total), live, "scrolled back {offset}");
        assert_matches_everywhere(t);
        t.scroll_display(-offset);
    }
}

#[test]
fn search_rows_read_matches_the_per_row_readers_over_a_corrupt_tiered_segment() {
    let (mut t, corrupt) = corrupt_tiered_terminal();
    assert_matches_everywhere(&t);
    let oldest = t.grid().oldest_absolute_row();
    let total = t.grid().scrollback_lines() + usize::from(t.rows());
    let (text, _, wrapped) = dense(&t, oldest, total);
    let scrollback = t.grid().scrollback_lines();
    assert!(
        wrapped[..scrollback].iter().any(|&w| w),
        "history holds soft wraps"
    );
    assert!(
        wrapped[scrollback] && wrapped[total - 1],
        "the last run straddles the history/screen edge"
    );
    let skipping: Vec<String> = t
        .grid()
        .history_lines_from(0)
        .flatten()
        .map(|line| line.to_string())
        .collect();
    let after = |rows: &[String]| {
        rows.iter()
            .position(|row| row.starts_with("NEEDLE_after"))
            .expect("the second needle is read")
    };
    assert_eq!(after(&skipping) + corrupt, after(&text));
    assert_scroll_invariant(&mut t, &[2, 30]);
}

#[test]
fn search_rows_read_matches_the_per_row_readers_over_evicted_history() {
    let mut t = evicted_terminal();
    assert_matches_everywhere(&t);
    let oldest = t.grid().oldest_absolute_row();
    let (_, _, wrapped) = dense(&t, oldest, 1);
    assert_eq!(wrapped, [false], "which reads as the start of a line");
    assert_scroll_invariant(&mut t, &[1, 5]);
}

/// A screen row's soft-wrap flag is the live row's, whatever the viewport shows:
/// scrolled back inside the ring or into the tiered history past it, every screen
/// row reads the flag it reads at the live tail.
#[test]
fn scrolled_back_wrap_bits_are_live() {
    let mut t = Terminal::with_scrollback(6, 20, 10, Scrollback::with_defaults());
    for i in 0..40 {
        t.process(format!("{i:02} {}\r\n", "x".repeat((i % 3) * 20)).as_bytes());
    }
    t.process(format!("tail {}", "t".repeat(30)).as_bytes());
    let rows = t.rows();
    let screen = t.grid().oldest_absolute_row() + t.grid().scrollback_lines() as u64;
    let read = |t: &Terminal| -> Vec<bool> {
        (screen..screen + u64::from(rows))
            .map(|abs| t.abs_row_continues(abs))
            .collect()
    };
    let live = read(&t);
    assert!(live.iter().any(|&w| w), "the screen holds soft wraps");
    for offset in [1, 3, 30] {
        t.scroll_display(offset);
        assert_eq!(
            t.grid().display_offset(),
            usize::try_from(offset).expect("non-negative"),
            "precondition: scrolled back"
        );
        assert_eq!(read(&t), live, "scrolled back {offset}");
        let viewport: Vec<bool> = (0..rows)
            .map(|row| t.grid().row(row).is_some_and(Row::is_wrapped))
            .collect();
        assert_ne!(viewport, live, "scrolled back {offset}");
        t.scroll_display(-offset);
    }
}

/// A history line past the read bound (a crafted checkpoint can store one) is read
/// to the bound for search, its cells marked truncated, and whole only when the
/// reader asks for the whole row.
#[test]
fn a_multi_mib_history_row_reads_bounded() {
    let mut line = b"NEEDLE ".to_vec();
    line.resize(2 * MAX_SCROLLBACK_LINE_SCAN_BYTES, b'A');
    let mut scrollback = Scrollback::with_defaults();
    scrollback.push_line(Line::from_bytes(&line));
    let t = Terminal::with_grid(Grid::with_tiered_scrollback(4, 80, 1000, scrollback));
    let oldest = t.grid().oldest_absolute_row();
    let stored = t.grid().get_history_line(0).expect("the line").to_string();
    assert_eq!(stored.len(), line.len());

    let mut rows = SearchRows::default();
    t.read_search_rows(oldest, 1, &mut rows);
    assert_eq!(rows.text[0].len(), MAX_SCROLLBACK_LINE_SCAN_BYTES);
    assert!(stored.starts_with(&rows.text[0]), "a prefix of the line");
    assert!(rows.cells[0].is_truncated(), "the cut is recorded");
    assert_eq!(
        t.abs_row_text(oldest).map(|text| text.len()),
        Some(MAX_SCROLLBACK_LINE_SCAN_BYTES)
    );
    assert_eq!(
        t.abs_row_text_within(oldest, usize::MAX),
        AbsRowText::Text(stored),
        "read whole when asked"
    );
    rows = SearchRows::default();
    t.read_search_rows(oldest + 1, 1, &mut rows);
    assert!(
        !rows.cells[0].is_truncated(),
        "the screen row is read whole"
    );
}
