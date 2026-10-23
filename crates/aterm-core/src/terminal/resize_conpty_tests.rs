// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resize under ConPTY (`ResizePolicy::ConPty`), driven as conhost drives it.

use super::Terminal;

use crate::grid::ResizePolicy;
use crate::terminal::search_index::tests::{cached_results, legacy_results};
use aterm_scrollback::Scrollback;
use std::collections::BTreeSet;

fn conhost_repaint(rows: &[String], visible_rows: u16, cursor_1based: (u16, u16)) -> Vec<u8> {
    let mut out = b"\x1b[?25l\x1b[H".to_vec();
    for r in 0..visible_rows {
        if r > 0 {
            out.extend_from_slice(b"\r\n");
        }
        if let Some(text) = rows.get(usize::from(r)) {
            out.extend_from_slice(text.as_bytes());
        }
        out.extend_from_slice(b"\x1b[K");
    }
    out.extend_from_slice(
        format!("\x1b[{};{}H\x1b[?25h", cursor_1based.0, cursor_1based.1).as_bytes(),
    );
    out
}

fn retained_rows(t: &Terminal) -> Vec<String> {
    let grid = t.grid();
    let mut rows: Vec<String> = (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    for r in 0..t.rows() {
        rows.push(
            t.get_line_text(i32::from(r), None)
                .unwrap_or_default()
                .trim_end()
                .to_string(),
        );
    }
    rows
}

fn visible_rows(t: &Terminal) -> Vec<String> {
    (0..t.rows())
        .map(|r| {
            t.get_line_text(i32::from(r), None)
                .unwrap_or_default()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn tiered(rows: u16, cols: u16) -> Terminal {
    let sb = Scrollback::new(64, 512, 8_000_000);
    Terminal::with_scrollback(rows, cols, 8, sb)
}

fn sixty_lines(t: &mut Terminal) {
    for i in 1..=60 {
        t.process(format!("grow {i:02}\r\n").as_bytes());
    }
}

fn grow_repaint(pre_resize_viewport: &[String]) -> Vec<u8> {
    conhost_repaint(pre_resize_viewport, 40, (24, 1))
}

#[test]
fn conpty_rows_grow_keeps_every_history_line_across_the_conhost_repaint() {
    for (label, mut t) in [
        ("ring-only", Terminal::new(24, 80)),
        ("tiered", tiered(24, 80)),
    ] {
        sixty_lines(&mut t);
        let before = visible_rows(&t);
        assert_eq!(
            t.grid().scrollback_lines(),
            37,
            "{label}: precondition: 37 history lines"
        );
        assert_eq!(before[0], "grow 38", "{label}: precondition: old row 0");
        assert_eq!(
            t.grid().cursor_row(),
            23,
            "{label}: precondition: cursor on the prompt row"
        );

        t.resize_with_policy(40, 80, ResizePolicy::ConPty);
        assert_eq!(t.rows(), 40);
        assert_eq!(
            t.grid().scrollback_lines(),
            37,
            "{label}: a ConPTY grow reveals no history — the ring keeps every line"
        );
        let after = visible_rows(&t);
        assert_eq!(
            &after[..24],
            &before[..],
            "{label}: old viewport rows stay at rows 0..24"
        );
        assert!(
            after[24..].iter().all(String::is_empty),
            "{label}: the grown rows are fresh blanks at the bottom"
        );
        assert_eq!(
            t.grid().cursor_row(),
            23,
            "{label}: the cursor row is unchanged"
        );
        assert_eq!(
            t.grid_mut().take_last_resize_row_shift(),
            0,
            "{label}: nothing moved, so the selection compensation is zero"
        );

        t.process(&grow_repaint(&before));
        let rows = retained_rows(&t);
        let expected: Vec<String> = (1..=60).map(|i| format!("grow {i:02}")).collect();
        assert_eq!(
            &rows[..60],
            &expected[..],
            "{label}: all 60 lines retained once, in order, after the repaint"
        );
        assert!(
            rows[60..].iter().all(String::is_empty),
            "{label}: only blanks follow"
        );
        assert_eq!(
            t.grid().cursor_row(),
            23,
            "{label}: conhost's CUP lands on the same row"
        );
    }
}

#[test]
fn conpty_rows_grow_search_index_matches_a_fresh_build_after_the_repaint() {
    let mut t = Terminal::new(24, 80);
    sixty_lines(&mut t);
    let primed = cached_results(&mut t, "grow");
    assert_eq!(primed.len(), 60);
    let before = visible_rows(&t);

    t.resize_with_policy(40, 80, ResizePolicy::ConPty);
    assert_eq!(
        cached_results(&mut t, "grow"),
        legacy_results(&t, "grow"),
        "after the resize the cache equals a fresh build"
    );
    t.process(&grow_repaint(&before));
    let got = cached_results(&mut t, "grow");
    assert_eq!(got, legacy_results(&t, "grow"), "after the repaint too");
    assert_eq!(got.len(), 60, "every line is found exactly once");
}

#[test]
fn conpty_rows_grow_keeps_a_scrolled_back_reader_on_the_same_line() {
    let mut t = Terminal::new(24, 80);
    sixty_lines(&mut t);
    t.grid_mut().scroll_display(10);
    let top_before = t.grid().row(0).map(|r| r.to_string()).unwrap_or_default();
    assert_eq!(
        top_before.trim_end(),
        "grow 28",
        "precondition: scrolled 10 into history"
    );

    t.resize_with_policy(40, 80, ResizePolicy::ConPty);
    let top_after = t.grid().row(0).map(|r| r.to_string()).unwrap_or_default();
    assert_eq!(
        top_after.trim_end(),
        "grow 28",
        "the line under the eye is unchanged by a grow that only appended rows below"
    );
    t.resize_with_policy(24, 80, ResizePolicy::ConPty);
    let top_back = t.grid().row(0).map(|r| r.to_string()).unwrap_or_default();
    assert_eq!(
        top_back.trim_end(),
        "grow 28",
        "nor by the trim that takes those rows away again"
    );
}

fn abs_text(t: &Terminal, abs: u64) -> String {
    match t.abs_row_text_within(abs, usize::MAX) {
        crate::terminal::AbsRowText::Text(text) => text.trim_end().to_string(),
        crate::terminal::AbsRowText::Evicted
        | crate::terminal::AbsRowText::OutOfRange
        | crate::terminal::AbsRowText::Unreadable => String::new(),
    }
}

#[test]
fn conpty_height_drag_keeps_command_marks_on_their_rows() {
    for (label, lead) in [("evicting", 300usize), ("young", 2)] {
        let mut t = Terminal::new(24, 80);
        t.set_scrollback_line_limit(Some(100));
        for i in 0..lead {
            t.process(format!("lead {i}\r\n").as_bytes());
        }
        t.process(
            b"\x1b]133;A\x07PROMPT-ONE> \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\
              \x1b]133;D;0\x07\x1b]133;A\x07PROMPT-TWO> ",
        );
        let mark = t
            .command_marks()
            .last()
            .expect("a completed command")
            .clone();
        let output = mark.output_start_row.expect("an output row");
        let check = |t: &Terminal, when: &str| {
            assert_eq!(
                abs_text(t, mark.prompt_start_row),
                "PROMPT-ONE> echo hi",
                "{label}: prompt row {when}"
            );
            assert_eq!(abs_text(t, output), "hi", "{label}: output row {when}");
        };
        check(&t, "before");
        let screen = visible_rows(&t);
        let cursor = (t.grid().cursor_row() + 1, t.grid().cursor_col() + 1);

        t.resize_with_policy(40, 80, ResizePolicy::ConPty);
        t.process(&conhost_repaint(&screen, 40, cursor));
        check(&t, "after the grow and its repaint");
        t.resize_with_policy(24, 80, ResizePolicy::ConPty);
        t.process(&conhost_repaint(&screen, 24, cursor));
        check(&t, "after the shrink back and its repaint");
    }
}

#[test]
fn conpty_rows_grow_under_an_alt_screen_keeps_the_saved_primary_intact() {
    let mut t = Terminal::new(24, 80);
    sixty_lines(&mut t);
    let before = visible_rows(&t);
    t.process(b"\x1b[?1049h");
    t.resize_with_policy(40, 80, ResizePolicy::ConPty);
    t.process(&conhost_repaint(&[], 40, (1, 1)));
    t.process(b"\x1b[?1049l");
    assert_eq!(t.rows(), 40);
    assert_eq!(
        t.grid().scrollback_lines(),
        37,
        "saved primary history untouched"
    );
    let after = visible_rows(&t);
    assert_eq!(
        &after[..24],
        &before[..],
        "saved primary viewport row-0-anchored"
    );
    assert!(after[24..].iter().all(String::is_empty));
}

const UNIT: &str = "abcdefghij";

fn steps_kept(t: &Terminal) -> BTreeSet<u64> {
    retained_rows(t)
        .iter()
        .filter_map(|row| row.strip_prefix("step ")?.split(' ').next()?.parse().ok())
        .collect()
}

fn native_script(mut t: Terminal, offload: bool) -> Vec<BTreeSet<u64>> {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut kept = Vec::with_capacity(120);
    for step in 0..120u64 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let r = state;
        let text = format!(
            "step {step} {}\r\n",
            UNIT.repeat(usize::try_from(r % 6).unwrap())
        );
        t.process(text.as_bytes());
        if (r >> 24) % 8 == 0 {
            t.process(b"\x1b[3J");
        }
        if (r >> 16) % 4 == 0 {
            t.scroll_display(i32::try_from((r >> 20) % 12).unwrap());
        }
        let rows = 4 + u16::try_from((r >> 4) % 12).unwrap();
        let cols = if (r >> 12) % 3 == 0 {
            t.cols()
        } else {
            20 + u16::try_from((r >> 8) % 40).unwrap()
        };
        if offload {
            let mut next =
                t.resize_offloading_scrollback_with_policy(rows, cols, ResizePolicy::Native);
            while let Some(pending) = next {
                next = t.finish_resize_offload(pending.reflow());
            }
        } else {
            t.resize_with_policy(rows, cols, ResizePolicy::Native);
        }
        kept.push(steps_kept(&t));
    }
    kept
}

#[test]
fn offloaded_native_script_keeps_every_line_the_synchronous_one_keeps() {
    let synchronous = native_script(tiered(10, 40), false);
    let offloaded = native_script(tiered(10, 40), true);
    for (step, (synchronous, offloaded)) in synchronous.iter().zip(&offloaded).enumerate() {
        let lost: Vec<_> = synchronous.difference(offloaded).collect();
        assert!(
            lost.is_empty(),
            "step {step}: the offloaded reflow lost the lines of steps {lost:?}"
        );
    }
}

fn combined_setup(t: &mut Terminal) {
    for i in 1..=38 {
        t.process(format!("row {i:02}\r\n").as_bytes());
    }
}

#[test]
fn conpty_combined_grow_and_widen_keeps_every_history_line() {
    let mut t = Terminal::new(20, 60);
    combined_setup(&mut t);
    let history = t.grid().scrollback_lines();
    assert_eq!(history, 19, "precondition");
    let before = visible_rows(&t);
    t.resize_with_policy(40, 120, ResizePolicy::ConPty);
    t.process(&conhost_repaint(&before, 40, (20, 1)));
    assert_eq!(t.grid().scrollback_lines(), history, "no history pulled in");
    let rows = retained_rows(&t);
    let expected: Vec<String> = (1..=38).map(|i| format!("row {i:02}")).collect();
    assert_eq!(&rows[..38], &expected[..], "all 38 lines once, in order");
    assert!(
        rows[38..].iter().all(String::is_empty),
        "only blanks follow"
    );
    assert_eq!(t.grid().cursor_row(), 19);
    assert_eq!(
        cached_results(&mut t, "row "),
        legacy_results(&t, "row "),
        "the search index equals a fresh build"
    );
}

#[test]
fn conpty_rows_shrink_in_the_bottom_push_corner_leaves_the_viewport_conhost_paints() {
    let mut t = Terminal::new(24, 80);
    for i in 1..=30 {
        t.process(format!("line {i:02}\r\n").as_bytes());
    }
    t.process(b"\x1b[4;1H>");
    let all_before = retained_rows(&t);
    let screen = visible_rows(&t);
    assert_eq!(screen[3], ">ine 11", "precondition: the prompt over row 3");
    assert_eq!(t.grid().cursor_row(), 3);

    t.resize_with_policy(16, 80, ResizePolicy::ConPty);
    let conhost_view: Vec<String> = screen[3..19].to_vec();
    assert_eq!(
        visible_rows(&t),
        conhost_view,
        "the viewport conhost paints"
    );
    assert_eq!(t.grid().cursor_row(), 0);
    t.process(&conhost_repaint(&conhost_view, 16, (1, 2)));
    assert_eq!(visible_rows(&t), conhost_view, "its repaint is a no-op");

    t.resize_with_policy(24, 80, ResizePolicy::ConPty);
    t.process(&conhost_repaint(&conhost_view, 24, (1, 2)));
    let after = visible_rows(&t);
    assert_eq!(&after[..16], &conhost_view[..]);
    assert!(after[16..].iter().all(String::is_empty), "grown rows blank");

    let mut a = all_before;
    let mut b = retained_rows(&t);
    a.retain(|r| !r.is_empty());
    b.retain(|r| !r.is_empty());
    a.sort();
    b.sort();
    assert_eq!(a, b, "every line retained exactly once (order aside)");
    assert_eq!(
        cached_results(&mut t, "line "),
        legacy_results(&t, "line "),
        "the search index equals a fresh build"
    );
}

fn pwsh_prompt() -> String {
    format!("PS C:\\{}>", "w".repeat(109))
}

fn col_test(n: usize) -> String {
    format!("col-test {n:02} {}", UNIT.repeat(9))
}

const COL_TEST_TAIL: &str = "ijabcdefghijabcdefghij";

fn conhost_cls(rows: u16) -> Vec<u8> {
    let mut out = Vec::new();
    for lead in [&b"\x1b[?25l\x1b[H"[..], &b"\x1b[?25l\x1b[3J"[..]] {
        out.extend_from_slice(lead);
        for r in 0..rows {
            if r > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"\x1b[K");
        }
        out.extend_from_slice(b"\x1b[H\x1b[?25h");
    }
    out
}

fn bottom_row_run(text: &str, rows: u16) -> Vec<u8> {
    let mut out = text[..80].as_bytes().to_vec();
    out.extend_from_slice(format!("\r\n\x1b[{};80H", rows - 1).as_bytes());
    out.extend_from_slice(text[79..].as_bytes());
    out
}

fn pwsh_col_test_fill() -> Vec<u8> {
    let mut out = Vec::new();
    for n in 1..=40 {
        if n <= 11 {
            out.extend_from_slice(col_test(n).as_bytes());
        } else {
            out.extend(bottom_row_run(&col_test(n), 23));
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend(bottom_row_run(&pwsh_prompt(), 23));
    out.extend_from_slice(b"\x1b[1C");
    out
}

fn prompt_and_command(t: &mut Terminal) {
    t.process(pwsh_prompt().as_bytes());
    t.process(format!("cls; {}\r\n", "x".repeat(65)).as_bytes());
    let g = t.grid();
    assert!(
        g.row(1).is_some_and(|r| r.is_wrapped()) && g.row(2).is_some_and(|r| r.is_wrapped()),
        "precondition: the prompt and its command leave rows 1 and 2 as continuations"
    );
}

fn history(t: &Terminal) -> Vec<(String, bool)> {
    let g = t.grid();
    (0..g.scrollback_lines())
        .map(|i| {
            let line = g.get_history_line(i).expect("history line");
            (line.to_string().trim_end().to_string(), line.is_wrapped())
        })
        .collect()
}

fn col_test_history_at_80() -> Vec<(String, bool)> {
    let mut rows = Vec::new();
    for n in 1..=30 {
        let line = col_test(n);
        rows.push((line[..80].to_string(), false));
        if n < 30 {
            rows.push((line[80..].to_string(), true));
        }
    }
    rows
}

fn widen_to_120_repaint() -> Vec<u8> {
    let mut out = format!("\x1b[?25l\x1b[H{COL_TEST_TAIL}\x1b[K\r\n").into_bytes();
    for n in 31..=40 {
        out.extend_from_slice(format!("{}\x1b[K\r\n", col_test(n)).as_bytes());
    }
    out.extend_from_slice(format!("{}    \r\n", pwsh_prompt()).as_bytes());
    for _ in 0..10 {
        out.extend_from_slice(b"\x1b[K\r\n");
    }
    out.extend_from_slice(b"\x1b[K\x1b[12;118H\x1b[?25h");
    out
}

fn narrow_to_80_repaint() -> Vec<u8> {
    let mut out = format!("\x1b[?25l\x1b[H{COL_TEST_TAIL}\x1b[K\r\n").into_bytes();
    for n in 31..=40 {
        out.extend_from_slice(format!("{}\x1b[K\r\n", col_test(n)).as_bytes());
    }
    out.extend_from_slice(format!("{}\x1b[K\x1b[1C\x1b[?25h", pwsh_prompt()).as_bytes());
    out
}

fn conpty_shapes(rows: u16, cols: u16) -> [(&'static str, Terminal); 3] {
    [
        ("ring-only", Terminal::new(rows, cols)),
        (
            "gui-ring",
            Terminal::with_scrollback(rows, cols, 6_123, Scrollback::new(64, 512, 8_000_000)),
        ),
        ("store", tiered(rows, cols)),
    ]
}

fn conpty_resize(t: &mut Terminal, rows: u16, cols: u16, repaint: &[u8], before_reattach: bool) {
    match t.resize_offloading_scrollback_with_policy(rows, cols, ResizePolicy::ConPty) {
        Some(pending) => {
            if before_reattach {
                t.process(repaint);
            }
            assert!(t.finish_resize_offload(pending.reflow()).is_none());
            if !before_reattach {
                t.process(repaint);
            }
        }
        None => t.process(repaint),
    }
}

#[test]
fn conpty_widen_after_conhost_cls_rewraps_history_with_no_row_shifted() {
    for before_reattach in [true, false] {
        for (label, mut t) in conpty_shapes(23, 80) {
            prompt_and_command(&mut t);
            t.process(&conhost_cls(23));
            assert!(
                (1..23).all(|r| !t.grid().row(r).is_some_and(|row| row.is_wrapped())),
                "{label}: conhost's row-by-row `CSI K` breaks every link below row 0"
            );
            t.process(&pwsh_col_test_fill());
            assert_eq!(
                history(&t),
                col_test_history_at_80(),
                "{label}: the fill's history"
            );
            let screen_at_80 = visible_rows(&t);
            assert_eq!(screen_at_80[0], COL_TEST_TAIL, "{label}: tail at row 0");

            let widened: Vec<(String, bool)> = (1..=29)
                .map(|n| (col_test(n), false))
                .chain([(col_test(30)[..80].to_string(), false)])
                .collect();
            for cycle in 1..=3 {
                conpty_resize(&mut t, 23, 120, &widen_to_120_repaint(), before_reattach);
                assert_eq!(
                    history(&t),
                    widened,
                    "{label} cycle {cycle} (repaint before re-attach: {before_reattach}): \
                     at 120 every line is one row, none shifted"
                );
                let screen = visible_rows(&t);
                assert_eq!(screen[0], COL_TEST_TAIL, "{label}: conhost's row 0");
                assert_eq!(screen[1], col_test(31), "{label}: row 1");
                assert_eq!(screen[11], pwsh_prompt(), "{label}: the prompt row");

                conpty_resize(&mut t, 23, 80, &narrow_to_80_repaint(), before_reattach);
                assert_eq!(
                    history(&t),
                    col_test_history_at_80(),
                    "{label} cycle {cycle}: narrowing back restores the 59 rows exactly"
                );
                assert_eq!(visible_rows(&t), screen_at_80, "{label}: and the screen");
            }
        }
    }
}

fn pwsh_pad_fill() -> Vec<u8> {
    let mut out = conhost_cls(23);
    for n in 1..=30 {
        out.extend_from_slice(format!("pad {n:02}").as_bytes());
        if n <= 23 {
            out.extend_from_slice(b"\x1b[K");
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend(bottom_row_run(&pwsh_prompt(), 23));
    out.extend_from_slice(b"\x1b[1C");
    out
}

fn shrink_to_60_repaint() -> Vec<u8> {
    let mut out = b"\x1b[?25l\x1b[H".to_vec();
    for n in 13..=30 {
        out.extend_from_slice(format!("pad {n:02}\x1b[K\r\n").as_bytes());
    }
    out.extend_from_slice(format!("{}    \x1b[20;58H\x1b[?25h", pwsh_prompt()).as_bytes());
    out
}

#[test]
fn conpty_column_shrink_after_conhost_cls_pushes_no_stray_row() {
    let pad = |range: std::ops::RangeInclusive<usize>| -> Vec<(String, bool)> {
        range.map(|n| (format!("pad {n:02}"), false)).collect()
    };
    for before_reattach in [true, false] {
        for (label, mut t) in conpty_shapes(23, 80) {
            prompt_and_command(&mut t);
            t.process(&conhost_cls(23));
            t.process(&pwsh_col_test_fill());
            conpty_resize(&mut t, 23, 120, &widen_to_120_repaint(), before_reattach);
            conpty_resize(&mut t, 23, 80, &narrow_to_80_repaint(), before_reattach);
            assert!(
                (0..23).any(|r| t.grid().row(r).is_some_and(|row| row.is_wrapped())),
                "{label}: precondition: the screen the pads overwrite holds continuations"
            );
            t.process(&pwsh_pad_fill());
            assert_eq!(
                history(&t),
                pad(1..=9),
                "{label}: history before the shrink"
            );

            conpty_resize(&mut t, 20, 60, &shrink_to_60_repaint(), before_reattach);
            assert_eq!(
                history(&t),
                pad(1..=12),
                "{label} (repaint before re-attach: {before_reattach}): exactly the \
                 demoted lines, unshifted"
            );
            let prompt = pwsh_prompt();
            let expected: Vec<String> = (13..=30)
                .map(|n| format!("pad {n:02}"))
                .chain([prompt[..60].to_string(), prompt[60..].to_string()])
                .collect();
            assert_eq!(
                visible_rows(&t),
                expected,
                "{label}: the screen conhost painted"
            );
            let retained = retained_rows(&t);
            for n in 1..=30 {
                let text = format!("pad {n:02}");
                assert_eq!(
                    retained.iter().filter(|r| **r == text).count(),
                    1,
                    "{label}: {text} retained once"
                );
            }
        }
    }
}

/// The two pad fills captured at 24x80.
#[derive(Clone, Copy, Debug)]
enum PadPrompt {
    /// The command went in as two input lines, so PSReadLine drew the prompt
    /// twice with `>>` on a fresh bottom row and put the cursor back on the
    /// prompt above it (`CR LF >> CSI 23;38 H`).
    AboveContinuation,
    /// One input line: one prompt, on rows 22..=23, the cursor after it on the
    /// bottom row.
    Alone,
}

fn pwsh_pad_fill_24(fill: PadPrompt) -> Vec<u8> {
    let mut out = b"\x1b[?25l\x1b[H".to_vec();
    for r in 0..24 {
        if r > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\x1b[K");
    }
    out.extend_from_slice(b"\x1b[H\x1b[?25h\x1b[3J");
    for n in 1..=30 {
        out.extend_from_slice(format!("pad {n:02}").as_bytes());
        if n <= 24 {
            out.extend_from_slice(b"\x1b[K");
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend(bottom_row_run(&pwsh_prompt(), 24));
    out.extend_from_slice(b"\x1b[1C");
    if let PadPrompt::AboveContinuation = fill {
        out.extend_from_slice(b"\r\n");
        out.extend(bottom_row_run(&pwsh_prompt(), 24));
        out.extend_from_slice(b"\x1b[1C\x1b[?25l\r\n>>\x1b[23;38H\x1b[?25h");
    }
    out
}

const PAD_RESIZES: [(u16, u16, usize); 14] = [
    (20, 60, 16),
    (24, 80, 16),
    (24, 60, 16),
    (24, 80, 16),
    (23, 70, 16),
    (24, 80, 16),
    (24, 40, 16),
    (24, 80, 16),
    (22, 60, 16),
    (24, 80, 16),
    (20, 50, 18),
    (24, 80, 18),
    (21, 75, 18),
    (24, 80, 18),
];

fn cursor_row_of_its_own(cols: u16) -> bool {
    let cols = usize::from(cols);
    let text = pwsh_prompt().len();
    (text + 2).div_ceil(cols) > text.div_ceil(cols)
}

fn pad_painted(fill: PadPrompt, first: usize, cols: u16) -> Vec<String> {
    let prompt_rows: Vec<String> = pwsh_prompt()
        .as_bytes()
        .chunks(usize::from(cols))
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    let mut screen: Vec<String> = (first..=30).map(|n| format!("pad {n:02}")).collect();
    if let PadPrompt::AboveContinuation = fill {
        screen.extend(prompt_rows.iter().cloned());
    }
    screen.extend(prompt_rows);
    if cursor_row_of_its_own(cols) {
        screen.push(String::new());
    }
    if let PadPrompt::AboveContinuation = fill {
        screen.push(">>".to_string());
    }
    screen
}

fn pad_screen(fill: PadPrompt, first: usize, rows: u16, cols: u16) -> Vec<String> {
    let mut screen = pad_painted(fill, first, cols);
    screen.resize(usize::from(rows), String::new());
    screen
}

fn pad_cursor(fill: PadPrompt, first: usize, cols: u16) -> (u16, u16) {
    let cols = usize::from(cols);
    let prompt = pwsh_prompt().len();
    let offset = prompt + 1;
    let above = match fill {
        PadPrompt::AboveContinuation => prompt.div_ceil(cols),
        PadPrompt::Alone => 0,
    };
    let row = (31 - first) + above + offset / cols;
    (
        u16::try_from(row).unwrap(),
        u16::try_from(offset % cols).unwrap(),
    )
}

fn pad_repaint(fill: PadPrompt, first: usize, rows: u16, cols: u16) -> Vec<u8> {
    let prompt = pwsh_prompt();
    let width = usize::from(cols);
    let blanks = prompt.len().div_ceil(width) * width - prompt.len();
    let prompt_end = if blanks <= 4 {
        " ".repeat(blanks)
    } else {
        "\x1b[K".to_string()
    };
    let mut lines: Vec<String> = (first..=30).map(|n| format!("pad {n:02}\x1b[K")).collect();
    if let PadPrompt::AboveContinuation = fill {
        lines.push(format!("{prompt}{prompt_end}"));
    }
    lines.push(format!("{prompt}{prompt_end}"));
    if cursor_row_of_its_own(cols) {
        lines.push("\x1b[K".to_string());
    }
    if let PadPrompt::AboveContinuation = fill {
        lines.push(">>\x1b[K".to_string());
    }
    let mut out = b"\x1b[?25l\x1b[H".to_vec();
    out.extend_from_slice(lines.join("\r\n").as_bytes());
    let painted = pad_painted(fill, first, cols).len();
    for _ in painted..usize::from(rows) {
        out.extend_from_slice(b"\r\n\x1b[K");
    }
    let (row, col) = pad_cursor(fill, first, cols);
    let ends_on_the_cursor_row = cursor_row_of_its_own(cols)
        && matches!(fill, PadPrompt::Alone)
        && painted == usize::from(rows);
    if !ends_on_the_cursor_row {
        out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    } else if col > 0 {
        out.extend_from_slice(format!("\x1b[{col}C").as_bytes());
    }
    out.extend_from_slice(b"\x1b[?25h");
    out
}

fn pad_history(last: usize) -> Vec<(String, bool)> {
    (1..=last).map(|n| (format!("pad {n:02}"), false)).collect()
}

fn replay_pad_resizes(fill: PadPrompt, steps: &[(u16, u16, usize)]) {
    let (fill_first, fill_cursor) = match fill {
        PadPrompt::AboveContinuation => (12, (22, 37)),
        PadPrompt::Alone => (9, (23, 37)),
    };
    for before_reattach in [true, false] {
        for (label, mut t) in conpty_shapes(24, 80) {
            let label = format!("{label} {fill:?}");
            t.process(&pwsh_pad_fill_24(fill));
            assert_eq!(
                history(&t),
                pad_history(fill_first - 1),
                "{label}: the fill's history"
            );
            assert_eq!(
                visible_rows(&t),
                pad_screen(fill, fill_first, 24, 80),
                "{label}: the fill's screen"
            );
            assert_eq!(
                (t.grid().cursor_row(), t.grid().cursor_col()),
                fill_cursor,
                "{label}: the cursor one column past the prompt"
            );

            for &(rows, cols, first) in steps {
                let step =
                    format!("{label} {cols}x{rows} (repaint before re-attach: {before_reattach})");
                conpty_resize(
                    &mut t,
                    rows,
                    cols,
                    &pad_repaint(fill, first, rows, cols),
                    before_reattach,
                );
                assert_eq!(
                    history(&t),
                    pad_history(first - 1),
                    "{step}: history is what conhost demoted"
                );
                assert_eq!(
                    visible_rows(&t),
                    pad_screen(fill, first, rows, cols),
                    "{step}: the screen conhost painted"
                );
                assert_eq!(
                    (t.grid().cursor_row(), t.grid().cursor_col()),
                    pad_cursor(fill, first, cols),
                    "{step}: conhost's cursor"
                );
                let retained = retained_rows(&t);
                for n in 1..=30 {
                    let text = format!("pad {n:02}");
                    assert_eq!(
                        retained.iter().filter(|r| **r == text).count(),
                        1,
                        "{step}: {text} retained once"
                    );
                }
            }
            assert_eq!(
                cached_results(&mut t, "pad "),
                legacy_results(&t, "pad "),
                "{label}: the search index equals a fresh build"
            );
        }
    }
}

#[test]
fn conpty_shrink_with_a_row_below_the_cursor_keeps_every_line() {
    replay_pad_resizes(PadPrompt::AboveContinuation, &PAD_RESIZES);
}

const PAD_RESIZES_EXACT_WIDTHS: [(u16, u16, usize); 6] = [
    (20, 58, 17),
    (24, 80, 17),
    (20, 39, 19),
    (24, 80, 19),
    (20, 29, 21),
    (24, 80, 21),
];

const PAD_RESIZES_EXACT_WIDTHS_ALONE: [(u16, u16, usize); 7] = [
    (20, 58, 14),
    (24, 80, 14),
    (20, 60, 14),
    (20, 39, 15),
    (24, 80, 15),
    (20, 29, 16),
    (24, 80, 16),
];

#[test]
fn conpty_shrink_to_a_width_the_prompt_fills_exactly_keeps_every_line() {
    replay_pad_resizes(PadPrompt::AboveContinuation, &PAD_RESIZES_EXACT_WIDTHS);
    replay_pad_resizes(PadPrompt::Alone, &PAD_RESIZES_EXACT_WIDTHS_ALONE);
}

#[test]
fn conpty_rows_only_shrink_with_a_row_below_the_cursor_demotes_the_same_rows() {
    let fill = PadPrompt::AboveContinuation;
    let mut t = Terminal::new(24, 80);
    t.process(&pwsh_pad_fill_24(fill));
    t.resize_with_policy(20, 80, ResizePolicy::ConPty);
    assert_eq!(history(&t), pad_history(15));
    assert_eq!(visible_rows(&t), pad_screen(fill, 16, 20, 80));
    assert_eq!(
        (t.grid().cursor_row(), t.grid().cursor_col()),
        pad_cursor(fill, 16, 80)
    );
}
