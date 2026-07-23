// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! End-to-end scrollback CONTENT integrity under load.
//!
//! The scrollback unit tests cover error propagation (`None` on a corrupt/failed
//! backend) and the lz4 layer has its own round-trip + fuzz tests, but nothing
//! drove the full engine path — emit many lines → scroll them off into history
//! (through the hot ring and, past its capacity, the lz4 cold tier) → read them
//! back BY INDEX — and asserted the exact text survived. This pins that: a
//! corruption or off-by-one in the ring / materialization / cold-tier round-trip
//! (a class that has produced real bugs here) now fails a test instead of
//! silently mangling a user's history.

use aterm_core::terminal::Terminal;

/// Reconstruct the logical (pre-wrap) scrollback lines by joining soft-wrapped
/// continuation rows. A history row is a continuation of the previous logical
/// line when its `wrapped` flag is set, mirroring how a terminal stores reflow.
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
    // 190 distinct near-full-width lines so they occupy scrollback and each is
    // self-identifying. At 38x184 this is the exact shape of the reported bug:
    // a resize collapsed scrollback and dropped early lines (#7906).
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

    // Shrink: wide logical lines rewrap into MORE physical scrollback rows, so
    // scrollback_lines() must GROW, but no logical line may be lost.
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
fn resize_round_trip_restores_known_early_line() {
    // The exact 184 -> 140 -> 184 round-trip from the bug report: a known early
    // line ("ROW30") must survive both legs, and the round-trip must restore the
    // scrollback row count to its original value (rewrap is reversible).
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

    let sb0 = term.grid().scrollback_lines();
    let present = |t: &Terminal| {
        logical_history_lines(t)
            .iter()
            .any(|l| l.starts_with("ROW30 "))
    };
    assert!(present(&term), "ROW30 present initially");

    term.resize(38, 140);
    assert!(present(&term), "ROW30 survives 184 -> 140");

    term.resize(38, 184);
    assert!(present(&term), "ROW30 survives 140 -> 184 round-trip");
    assert_eq!(
        term.grid().scrollback_lines(),
        sb0,
        "round-trip must restore the original scrollback row count"
    );
}

#[test]
fn resize_preserves_scrollback_with_tiered_storage() {
    // Same invariant with an explicit tiered scrollback (the builder path used
    // by hosts that want unlimited history), exercising the lazy-buffer restore
    // path rather than the ring-only path.
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
    // Distinctly-numbered lines so each is self-identifying. The default
    // scrollback limit is 100k, so all 5000 are retained (no eviction) and
    // history index 0 == oldest == the first emitted line, contiguous upward.
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

    // Read a spread of indices — including the oldest (0, likely cold-tiered)
    // and the newest history line (h-1) — and assert the exact content.
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
    // Box-table repro: a tall batch emitted INSIDE a DEC-2026 synchronized-update
    // frame (CSI ?2026h … CSI ?2026l) must scroll the exact same non-blank lines
    // into scrollback as the un-synchronized path. Mode 2026 is a render-deferral
    // ONLY; it must never gate scroll_up / drop history. (An ad-hoc host probe once
    // mis-read off-screen rows as blank and suspected an engine bug here — this
    // pins that the engine is correct, so a real regression fails loudly.)
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
