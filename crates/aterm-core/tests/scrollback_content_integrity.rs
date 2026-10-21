// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Scrollback content integrity: emit many lines, scroll them into history, read them back
//! by index and across resizes, and assert the exact text survived.

use aterm_core::terminal::Terminal;

/// The logical (pre-wrap) history lines: a row whose `wrapped` flag is set continues the
/// previous one.
fn logical_history_lines(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    let h = grid.scrollback_lines();
    let mut out: Vec<String> = Vec::new();
    for i in 0..h {
        let line = grid.get_history_line(i).expect("history line present");
        let wrapped = line.is_wrapped();
        let text = line.to_string();
        let text = text.trim_end_matches(' ').to_string();
        if wrapped && !out.is_empty() {
            out.last_mut().unwrap().push_str(text.trim_end_matches(' '));
        } else {
            out.push(text);
        }
    }
    out
}

#[test]
fn resize_preserves_scrollback_logical_lines_both_directions() {
    // 190 distinct near-full-width lines at 38x184: a resize must not collapse
    // scrollback or drop early lines.
    let mut term = Terminal::new(38, 184);
    let mut input = Vec::new();
    for i in 0..190u32 {
        let mut line = format!("ROW{i} ");
        while line.len() < 180 {
            line.push_str("wxyz");
        }
        line.truncate(180);
        input.extend_from_slice(line.as_bytes());
        input.extend_from_slice(b"\r\n");
    }
    term.process(&input);

    let before = logical_history_lines(&term);
    assert!(
        before.iter().any(|l| l.starts_with("ROW30 ")),
        "ROW30 must be in scrollback before resize"
    );
    let before_count = before.len();

    // Shrink: wide logical lines rewrap into more physical scrollback rows, so
    // scrollback_lines() must grow, but no logical line may be lost.
    term.resize(38, 140);
    let after_narrow = logical_history_lines(&term);
    assert!(
        term.grid().scrollback_lines() > before_count,
        "shrinking width must grow the physical scrollback row count"
    );
    for line in &before {
        assert!(
            after_narrow.contains(line),
            "logical line lost on shrink: {line:?}"
        );
    }

    // Widen past the original: lines unwrap; all logical content still present.
    term.resize(38, 200);
    let after_wide = logical_history_lines(&term);
    for line in &before {
        assert!(
            after_wide.contains(line),
            "logical line lost on widen: {line:?}"
        );
    }
}

#[test]
fn resize_preserves_scrollback_with_tiered_storage() {
    // Same invariant with a tiered store attached, as hosts build one.
    use aterm_core::scrollback::Scrollback;
    let sb = Scrollback::new(1000, 10_000, 100_000_000);
    let mut term = Terminal::with_scrollback(30, 120, 1000, sb);
    let mut input = Vec::new();
    for i in 0..400u32 {
        let mut line = format!("L{i} ");
        while line.len() < 110 {
            line.push_str("abcd");
        }
        line.truncate(110);
        input.extend_from_slice(line.as_bytes());
        input.extend_from_slice(b"\r\n");
    }
    term.process(&input);

    let before = logical_history_lines(&term);
    assert!(before.iter().any(|l| l.starts_with("L5 ")));

    term.resize(30, 70);
    let narrow = logical_history_lines(&term);
    for line in &before {
        assert!(
            narrow.contains(line),
            "tiered logical line lost on shrink: {line:?}"
        );
    }

    term.resize(30, 120);
    let wide = logical_history_lines(&term);
    for line in &before {
        assert!(
            wide.contains(line),
            "tiered logical line lost on widen: {line:?}"
        );
    }
}

#[test]
fn scrollback_preserves_line_content_in_order() {
    // Self-identifying lines. `Terminal::new`'s 10,000-line ring retains all 5000, so
    // history index 0 is the first line emitted.
    let mut term = Terminal::new(24, 80);
    const N: usize = 5000;
    let mut input = Vec::with_capacity(N * 8);
    for i in 0..N {
        input.extend_from_slice(format!("L{i}\r\n").as_bytes());
    }
    term.process(&input);

    let h = term.grid().scrollback_lines();
    // Nearly all of them scrolled off the 24-row screen into history.
    assert!(h >= N - 40, "expected ~{} history lines, got {h}", N - 24);

    // A spread of indices, the oldest (0) and the newest (h-1) included.
    for k in [0usize, 1, 100, 2500, h - 1] {
        let line = term
            .grid()
            .get_history_line(k)
            .unwrap_or_else(|| panic!("history line {k} missing (history len {h})"));
        assert_eq!(
            line.to_string().trim_end(),
            format!("L{k}"),
            "history line {k} content corrupted (history len {h})"
        );
    }
}

#[test]
fn synchronized_update_scrolls_nonblank_lines_into_scrollback() {
    // A tall batch emitted inside a DEC-2026 synchronized-update frame scrolls
    // the same lines into scrollback as the unsynchronized path: mode 2026
    // defers rendering only and never gates scroll_up.
    let scrolled = |sync: bool| {
        let mut term = Terminal::new(24, 80);
        let mut input = Vec::new();
        if sync {
            input.extend_from_slice(b"\x1b[?2026h");
        }
        for i in 0..200u32 {
            input.extend_from_slice(format!("SYNCROW{i}\r\n").as_bytes());
        }
        if sync {
            input.extend_from_slice(b"\x1b[?2026l");
        }
        term.process(&input);
        logical_history_lines(&term)
    };

    let with_sync = scrolled(true);
    let without_sync = scrolled(false);

    for i in [0u32, 1, 55, 150] {
        let want = format!("SYNCROW{i}");
        assert!(
            with_sync.iter().any(|l| l == &want),
            "{want} missing from scrollback under DEC-2026 synchronized update"
        );
    }
    // Sync mode is irrelevant to scrolling: identical history either way.
    assert_eq!(
        with_sync, without_sync,
        "DEC-2026 synchronized update changed which lines scrolled into scrollback"
    );
    let nonblank = with_sync.iter().filter(|l| !l.is_empty()).count();
    assert!(
        nonblank >= 150,
        "scrollback should be densely non-blank, got {nonblank} of {}",
        with_sync.len()
    );
}
