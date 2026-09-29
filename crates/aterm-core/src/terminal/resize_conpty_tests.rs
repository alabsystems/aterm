// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resize under ConPTY: the seam accounting `ResizePolicy::ConPty` selects,
//! driven exactly as conhost drives it.
//!
//! conhost repaints the WHOLE viewport after every `ResizePseudoConsole`,
//! row-0-anchored, from its own buffer (measured on Windows 11 26200 with
//! `cmd.exe` under a bare `CreatePseudoConsole`, 2026-09-22 — the byte shape
//! [`conhost_repaint`] reproduces is the captured one). Every test here feeds
//! that repaint after the resize, because that is the sequence the installed
//! 0.90.0 lost history to (audit 2026-09-22): a rows-grow 24→40 dropped the 16
//! lines the native reveal had re-labelled into the top of the viewport, a
//! widen duplicated the 80-col wrap fragment the deficit fill re-seated under
//! conhost's paint, and the search index kept reporting the overwritten rows
//! by their old text.

use super::Terminal;
use crate::grid::ResizePolicy;
use aterm_scrollback::Scrollback;

/// The bytes conhost emits after a resize (measured shape): hide cursor, an
/// optional `CSI 8 ; rows ; cols t` (seen on the first resize of a session),
/// `CSI H`, then one `<text> CSI K` per viewport row joined by CR LF, then a
/// CUP to conhost's cursor and show cursor. Rows past `rows.len()` are blank.
/// Every row here is shorter than the width, which is the shape conhost
/// sends for such rows; a logical line spanning several rows goes out as one
/// autowrapping run instead — the pwsh captures at the end of this file
/// replay that shape byte for byte.
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

/// Every retained row in reading order — history (oldest first) then the
/// viewport — trimmed of trailing blanks. This is the buffer a reader scrolls
/// through, so a lost line is missing from it and a duplicated one is in it
/// twice.
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

/// The viewport rows as conhost holds them after a rows-only resize: the
/// pre-resize viewport text, which conhost keeps row-0-anchored.
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

fn cached_results(t: &mut Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
    let res = t
        .indexed_search()
        .search_results_opts(pat, false, false)
        .expect("search ok");
    res.matches
        .iter()
        .map(|m| (m.line, m.start_col, m.len()))
        .collect()
}

/// The from-scratch oracle `search_index.rs` pins its cache against — a fresh
/// index over the current retained rows, keyed by absolute row.
fn legacy_results(t: &Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
    use crate::search::TerminalSearch;
    let grid = t.grid();
    let oldest = usize::try_from(grid.oldest_absolute_row()).unwrap_or(usize::MAX);
    let scrollback = grid.scrollback_lines();
    let history: Vec<String> = (0..scrollback)
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string())
                .unwrap_or_default()
        })
        .collect();
    let mut search = TerminalSearch::new();
    search.index_visible_content(oldest, &history);
    let visible: Vec<String> = (0..t.rows())
        .map(|r| t.get_line_text(i32::from(r), None).unwrap_or_default())
        .collect();
    search.index_visible_content(oldest.saturating_add(scrollback), &visible);
    let res = search
        .search_results_opts(pat, false, false)
        .expect("search ok");
    res.matches
        .iter()
        .map(|m| (m.line, m.start_col, m.len()))
        .collect()
}

/// A tiered-store terminal like the GUI's (small ring, so history really
/// lives in the store and the offloaded resize really detaches it).
fn tiered(rows: u16, cols: u16) -> Terminal {
    let sb = Scrollback::new(64, 512, 8_000_000);
    Terminal::with_scrollback(rows, cols, 8, sb)
}

/// The audit's grow case: 60 numbered lines on a 24-row screen (37 in
/// history, `grow 38..60` + a blank prompt row on screen), then a rows-only
/// grow to 40.
fn sixty_lines(t: &mut Terminal) {
    for i in 1..=60 {
        t.process(format!("grow {i:02}\r\n").as_bytes());
    }
}

/// The measured repaint for that grow: conhost keeps old row 0 at row 0 and
/// paints the 16 grown rows `CSI K`; its cursor stays on the prompt row.
fn grow_repaint(pre_resize_viewport: &[String]) -> Vec<u8> {
    conhost_repaint(pre_resize_viewport, 40, (24, 1))
}

// ---- rows-grow ----------------------------------------------------------

/// The core claim: under ConPTY a rows-grow appends blank rows at the bottom,
/// reveals nothing, and conhost's repaint then changes NOTHING — every one of
/// the 60 lines is still retained exactly once, in order, and the cursor row is
/// where conhost's CUP puts it.
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
            "{label}: precondition (audit shape)"
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

        // conhost's repaint: ordinary output that must be a no-op on this buffer.
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

/// TEETH for the test above: the same sequence on the Native policy is the
/// audit's defect — the reveal re-labels `grow 22..37` into the viewport, the
/// repaint paints `grow 38..` over them, and they are gone from the buffer.
/// If this ever stops failing on Native, the policy's premise has changed and
/// the measurements in `ResizePolicy` need redoing.
#[test]
fn native_rows_grow_under_a_conhost_repaint_loses_the_revealed_lines() {
    let mut t = Terminal::new(24, 80);
    sixty_lines(&mut t);
    let before = visible_rows(&t);
    t.resize(40, 80);
    assert_eq!(
        t.grid().scrollback_lines(),
        21,
        "precondition: the native grow revealed 16 lines (37 -> 21, the audit's numbers)"
    );
    t.process(&grow_repaint(&before));
    let rows = retained_rows(&t);
    let present = |n: usize| rows.iter().any(|r| r == &format!("grow {n:02}"));
    assert!(
        present(21) && present(38),
        "the lines around the lost band survive"
    );
    assert!(
        !(22..=37).any(present),
        "the native reveal + conhost repaint destroys exactly the revealed band"
    );
}

/// The search index after the grow + repaint: every line found, at the row a
/// from-scratch index gives it, and the overwritten-band staleness the audit
/// saw (`grow 22` reported at the row that holds `grow 38`) cannot happen — the
/// cached index equals the oracle after the resize AND after the repaint.
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
    // Each match names the row that really holds that line.
    let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
    for n in 1..=60usize {
        let hits = cached_results(&mut t, &format!("grow {n:02}"));
        assert_eq!(hits.len(), 1, "grow {n:02} found once");
        let row = hits[0].0;
        let idx = row - oldest;
        let text = if idx < t.grid().scrollback_lines() {
            t.grid()
                .get_history_line(idx)
                .map(|l| l.to_string())
                .unwrap_or_default()
        } else {
            t.get_line_text(
                i32::try_from(idx - t.grid().scrollback_lines()).unwrap(),
                None,
            )
            .unwrap_or_default()
        };
        assert_eq!(
            text.trim_end(),
            format!("grow {n:02}"),
            "row {row} really holds it"
        );
    }
}

/// A reader scrolled back into history keeps the same line under the eye
/// across a ConPTY grow: the viewport grew downward (blanks appended) and the
/// counter moved with those rows, so the line under the eye kept its absolute
/// key and the anchor re-seats the reader on it.
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

/// The text at absolute row `abs`, resolved the way every absolute-row
/// consumer resolves it (`control_query::abs_row_text`): history below
/// `oldest + scrollback`, the viewport above it.
fn abs_text(t: &Terminal, abs: u64) -> String {
    let grid = t.grid();
    let Some(rel) = abs.checked_sub(grid.oldest_absolute_row()) else {
        return String::new();
    };
    let rel = usize::try_from(rel).unwrap();
    let text = if rel < grid.scrollback_lines() {
        grid.get_history_line(rel)
            .map(|l| l.to_string())
            .unwrap_or_default()
    } else {
        let row = i32::try_from(rel - grid.scrollback_lines()).unwrap();
        t.get_line_text(row, None).unwrap_or_default()
    };
    text.trim_end().to_string()
}

/// OSC 133 command marks are keyed by absolute row and never consult the
/// renumber epoch, so they are only as good as the keys under them. A ConPTY
/// height drag — grow, conhost's repaint, shrink back, repaint — must leave a
/// completed command's prompt and output rows naming the same text, in a
/// session whose retention has already evicted history and in a young one.
/// The first is where the old bookkeeping (counter fixed while rows were
/// appended) slid every key: with nothing evicted, `oldest_absolute_row()`
/// saturates at 0 and hides the slide, which is why the audit-era probe
/// needed 30,000 lines to see the prompt row read `""`, 16 rows off, after a
/// 24→40 grow. A 100-line limit reaches the same state in 300.
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

/// The offloaded entry point on a rows-only grow: nothing to offload (`None`),
/// and the same seam law as the synchronous path.
#[test]
fn conpty_offloaded_rows_grow_is_the_synchronous_policy() {
    let mut t = tiered(24, 80);
    sixty_lines(&mut t);
    let before = visible_rows(&t);
    assert!(
        t.resize_offloading_scrollback_with_policy(40, 80, ResizePolicy::ConPty)
            .is_none(),
        "a rows-only resize offloads nothing"
    );
    assert_eq!(t.grid().scrollback_lines(), 37);
    t.process(&grow_repaint(&before));
    let rows = retained_rows(&t);
    let expected: Vec<String> = (1..=60).map(|i| format!("grow {i:02}")).collect();
    assert_eq!(&rows[..60], &expected[..]);
}

/// The alt screen gets the same policy: a grow while a TUI is up must not
/// reveal the SAVED primary's history into a viewport conhost then repaints.
#[test]
fn conpty_rows_grow_under_an_alt_screen_keeps_the_saved_primary_intact() {
    let mut t = Terminal::new(24, 80);
    sixty_lines(&mut t);
    let before = visible_rows(&t);
    t.process(b"\x1b[?1049h");
    t.resize_with_policy(40, 80, ResizePolicy::ConPty);
    // conhost repaints the (empty) alt viewport too.
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

// ---- rows-shrink --------------------------------------------------------

/// Rows-shrink is unchanged by the policy: conhost's repaint after a shrink
/// (measured 24→16 on a full screen: old row 8 at row 0, i.e. a top-demote;
/// trailing blanks simply dropped) is what the native shape already produces,
/// so the two policies must agree row for row.
#[test]
fn conpty_rows_shrink_is_the_native_shape() {
    let mut native = Terminal::new(24, 80);
    let mut conpty = Terminal::new(24, 80);
    sixty_lines(&mut native);
    sixty_lines(&mut conpty);
    native.resize(16, 80);
    conpty.resize_with_policy(16, 80, ResizePolicy::ConPty);
    assert_eq!(retained_rows(&native), retained_rows(&conpty));
    assert_eq!(native.grid().cursor_row(), conpty.grid().cursor_row());
    assert_eq!(
        native.grid().scrollback_lines(),
        conpty.grid().scrollback_lines()
    );
    // And the measured repaint is a no-op on it: with the cursor on the blank
    // prompt row (row 23 -> row 15 after an 8-row demote), old rows 8.. sit
    // at rows 0..16.
    let before = visible_rows(&conpty);
    conpty.process(&conhost_repaint(&before, 16, (16, 1)));
    assert_eq!(visible_rows(&conpty), before);
    assert_eq!(conpty.grid().scrollback_lines(), 45);
}

// ---- width change with a wrap continuation at the viewport top ----------

const UNIT: &str = "abcdefghij";

/// The audit's widen case: a logical line longer than the width whose HEAD is
/// the newest history line and whose TAIL (a soft-wrap continuation) is the
/// viewport's row 0. Built as: the 150-char line, 22 short lines, a prompt —
/// 25 rows on a 24-row screen, so exactly the head scrolls into history.
fn belt_setup(t: &mut Terminal) {
    t.process(format!("{}\r\n", UNIT.repeat(15)).as_bytes());
    for i in 1..=22 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    t.process(b">");
}

/// How many times the long line's 10-char unit occurs across the whole
/// retained buffer. The line is 15 units; a duplicated fragment adds more, a
/// lost one leaves fewer.
fn unit_count(t: &Terminal) -> usize {
    retained_rows(t)
        .iter()
        .map(|r| r.matches(UNIT).count())
        .sum()
}

/// conhost's measured repaint after a width change with a continuation at row
/// 0, at 120 and at 80 columns alike: the SAME 24 rows (the 70-char fragment
/// stays at row 0 as a standalone row, the line is not rejoined with its
/// head), cursor after the prompt.
fn belt_repaint(rows_before: &[String]) -> Vec<u8> {
    conhost_repaint(rows_before, 24, (24, 2))
}

#[test]
fn belt_setup_puts_the_continuation_at_row_zero() {
    let mut t = Terminal::new(24, 80);
    belt_setup(&mut t);
    assert_eq!(t.grid().scrollback_lines(), 1);
    assert_eq!(
        t.grid().get_history_line(0).map(|l| l.to_string()),
        Some(UNIT.repeat(8)),
        "the 80-char head is the one history line"
    );
    let rows = visible_rows(&t);
    assert_eq!(rows[0], UNIT.repeat(7), "row 0 is the 70-char tail");
    assert!(
        t.grid().row(0).is_some_and(|r| r.is_wrapped()),
        "and it is a continuation"
    );
    assert_eq!(rows[23], ">");
    assert_eq!(unit_count(&t), 15);
}

/// Widen with the continuation at row 0: the fragment stays where conhost
/// keeps it, the head stays the one history line, conhost's repaint is a no-op,
/// and narrowing back leaves the history count where it started (the audit's
/// 58-not-57 came from the re-seated tail).
#[test]
fn conpty_widen_keeps_the_boundary_fragment_where_conhost_paints_it() {
    let mut t = Terminal::new(24, 80);
    belt_setup(&mut t);
    let before = visible_rows(&t);

    t.resize_with_policy(24, 120, ResizePolicy::ConPty);
    assert_eq!(
        t.grid().scrollback_lines(),
        1,
        "the head is not lifted out of history"
    );
    assert_eq!(
        t.grid().get_history_line(0).map(|l| l.to_string()),
        Some(UNIT.repeat(8)),
        "history rewraps only itself — the head stays 80 chars, as in conhost's scrollback"
    );
    let after = visible_rows(&t);
    assert_eq!(
        after, before,
        "the viewport rewrap moved nothing: same 24 rows"
    );
    assert_eq!(t.grid().cursor_row(), 23);

    t.process(&belt_repaint(&before));
    assert_eq!(
        visible_rows(&t),
        before,
        "conhost's repaint is a no-op on this buffer"
    );
    assert_eq!(unit_count(&t), 15, "no fragment duplicated, none lost");

    t.resize_with_policy(24, 80, ResizePolicy::ConPty);
    t.process(&belt_repaint(&before));
    assert_eq!(
        t.grid().scrollback_lines(),
        1,
        "narrowing back does not grow the history"
    );
    assert_eq!(unit_count(&t), 15);
    assert_eq!(visible_rows(&t), before);
}

/// Same, on the OFFLOADED path with a tiered store, in BOTH orders conhost's
/// repaint can arrive relative to the worker's re-attach. The audit's
/// duplicate was born here: the native re-attach fill re-seated the rewrapped
/// tail on top of a viewport conhost had already repainted.
#[test]
fn conpty_offloaded_widen_is_duplicate_free_whichever_side_of_the_reattach_the_repaint_lands() {
    for repaint_before_reattach in [true, false] {
        let mut t = tiered(24, 80);
        belt_setup(&mut t);
        let before = visible_rows(&t);
        let pending = t
            .resize_offloading_scrollback_with_policy(24, 120, ResizePolicy::ConPty)
            .expect("a width change with a tiered store offloads");
        if repaint_before_reattach {
            t.process(&belt_repaint(&before));
        }
        let reflowed = pending.reflow();
        assert!(t.finish_resize_offload(reflowed).is_none());
        if !repaint_before_reattach {
            t.process(&belt_repaint(&before));
        }
        assert_eq!(
            visible_rows(&t),
            before,
            "repaint-before-reattach={repaint_before_reattach}: viewport unchanged"
        );
        assert_eq!(
            t.grid().scrollback_lines(),
            1,
            "repaint-before-reattach={repaint_before_reattach}: one history line"
        );
        assert_eq!(
            unit_count(&t),
            15,
            "repaint-before-reattach={repaint_before_reattach}: no duplicate, no loss"
        );
        assert_eq!(
            cached_results(&mut t, "line 7"),
            legacy_results(&t, "line 7"),
            "the search index equals a fresh build"
        );
    }
}

/// TEETH: the native offloaded path with the repaint landing before re-attach
/// is the audit's duplicate — the fill re-seats the rewrapped tail above the
/// fragment conhost already painted at row 0, so the long line's text is in
/// the buffer more than once.
#[test]
fn native_offloaded_widen_under_a_conhost_repaint_duplicates_the_fragment() {
    let mut t = tiered(24, 80);
    belt_setup(&mut t);
    let before = visible_rows(&t);
    let pending = t.resize_offloading_scrollback(24, 120).expect("offloads");
    t.process(&belt_repaint(&before));
    let reflowed = pending.reflow();
    assert!(t.finish_resize_offload(reflowed).is_none());
    assert!(
        unit_count(&t) > 15,
        "the native fill + conhost repaint duplicates the tail ({} units of 15)",
        unit_count(&t)
    );
}

/// Search after the widen + repaint finds every short line at the row that
/// really holds it — the index is keyed by absolute row and a width reflow
/// renumbers wholesale, so this pins that the rebuild fence still fires.
#[test]
fn conpty_widen_search_finds_every_line_at_its_row() {
    let mut t = Terminal::new(24, 80);
    belt_setup(&mut t);
    let _ = cached_results(&mut t, "line");
    let before = visible_rows(&t);
    t.resize_with_policy(24, 120, ResizePolicy::ConPty);
    t.process(&belt_repaint(&before));
    let got = cached_results(&mut t, "line ");
    assert_eq!(got, legacy_results(&t, "line "));
    assert_eq!(got.len(), 22);
    let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
    let scrollback = t.grid().scrollback_lines();
    for (n, hit) in (1..=22usize).zip(&got) {
        // History: [head]; viewport: [tail, line 1..22, '>'] -> line n is
        // viewport row n, absolute oldest + scrollback + n.
        assert_eq!(
            hit.0,
            oldest + scrollback + n,
            "line {n} at its absolute row"
        );
    }
}

// ---- the native policy is untouched --------------------------------------

/// FNV-1a over `bytes`, folded into `h` — a digest that is the same on every
/// platform and toolchain, unlike `DefaultHasher`.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Everything a reader or a key-holding consumer can observe after a resize:
/// the retained rows in reading order, the cursor, the reading position, and
/// the absolute-row bookkeeping (oldest key, counter, renumber epoch).
fn fold_state(h: &mut u64, t: &Terminal) {
    for row in retained_rows(t) {
        fnv(h, row.as_bytes());
        fnv(h, b"\n");
    }
    let g = t.grid();
    for n in [
        u64::from(g.cursor_row()),
        u64::from(g.cursor_col()),
        g.display_offset() as u64,
        g.scrollback_lines() as u64,
        g.oldest_absolute_row(),
        g.absolute_row_counter(),
        g.history_renumber_epoch(),
    ] {
        fnv(h, &n.to_le_bytes());
    }
}

/// 120 steps of output, an occasional scrollback erase, a reader scrolling
/// back on a quarter of them, and a resize after each — a third keep the
/// width, the rest change it too — resized synchronously (`native`) or
/// through the offloaded detach/re-attach. Measured on the ring-only run: 74
/// width changes, 13 revealing grows, 7 blank-appending grows, 25 shrinks,
/// and 19 rows-only resizes under a scrolled-back reader (the anchor arm).
/// Returns the digest of every step.
fn native_script(mut t: Terminal, offload: bool, native: impl Fn(&mut Terminal, u16, u16)) -> u64 {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
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
            // ED 3: no history, so the next grow appends instead of revealing.
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
            native(&mut t, rows, cols);
        }
        fold_state(&mut h, &t);
    }
    h
}

/// The Native policy is 0.94.0's resize, byte for byte: the script above
/// through the policy entry points, on a ring-only and on a tiered terminal,
/// digests to exactly what the same script gave through the plain `resize` /
/// `resize_offloading_scrollback` of origin/main at 0.94.0 (`fe5d19198`),
/// captured with a probe linked against that build's `aterm_core`. The
/// digest covers the retained rows, cursor, reading position, counter,
/// oldest key and renumber epoch at every step, so a ConPTY arm leaking into
/// the Native path moves it. A deliberate change to the native resize moves
/// it too: re-capture it then, from the script above run on the build that
/// predates the change.
#[test]
fn native_policy_matches_the_0_94_0_resize() {
    const RING: u64 = 0xc06f_0e9d_2a8d_b94e;
    const TIERED: u64 = 0x01a9_2c38_01ae_30cc;
    let plain = |t: &mut Terminal, r: u16, c: u16| t.resize(r, c);
    let native = |t: &mut Terminal, r: u16, c: u16| {
        t.resize_with_policy(r, c, ResizePolicy::Native);
    };
    assert_eq!(native_script(Terminal::new(10, 40), false, plain), RING);
    assert_eq!(native_script(Terminal::new(10, 40), false, native), RING);
    assert_eq!(native_script(tiered(10, 40), true, native), TIERED);
}

// ---- the round trip, the combined resize, the shrink corner --------------

/// The offloaded twin of the width round trip in
/// `conpty_widen_keeps_the_boundary_fragment_where_conhost_paints_it`: after
/// the first re-attach the head's ring row is rebuilt 120 wide, so a
/// continuation flag left on the row-0 fragment would make the next detach
/// read the head's trailing blanks as content (an extra blank history line
/// per width change).
#[test]
fn conpty_offloaded_width_round_trip_keeps_the_history_line_count() {
    let mut t = tiered(24, 80);
    belt_setup(&mut t);
    let before = visible_rows(&t);
    for cols in [120u16, 80, 120, 80] {
        match t.resize_offloading_scrollback_with_policy(24, cols, ResizePolicy::ConPty) {
            Some(pending) => {
                t.process(&belt_repaint(&before));
                assert!(t.finish_resize_offload(pending.reflow()).is_none());
            }
            None => t.process(&belt_repaint(&before)),
        }
        assert_eq!(
            t.grid().scrollback_lines(),
            1,
            "{cols} cols: one history line"
        );
        assert_eq!(
            t.grid().get_history_line(0).map(|l| l.to_string()),
            Some(UNIT.repeat(8)),
            "{cols} cols: the head, unchanged"
        );
        assert_eq!(visible_rows(&t), before, "{cols} cols: viewport unchanged");
        assert_eq!(
            unit_count(&t),
            15,
            "{cols} cols: no fragment duplicated, none lost"
        );
    }
}

/// The audit's combined case, 20x60 -> 40x120 with history: conhost keeps the
/// old viewport at the top and paints the rest blank (measured 80x24 ->
/// 120x40, `ResizePolicy`), so every history line must still be history after
/// its repaint.
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

/// TEETH for the combined case: the Native widen's deficit fill pulls the whole
/// history into the grown viewport, and conhost's repaint then writes the old
/// viewport over it — the audit's "all scrollback lines vanished".
#[test]
fn native_combined_grow_and_widen_under_a_conhost_repaint_loses_the_history() {
    let mut t = Terminal::new(20, 60);
    combined_setup(&mut t);
    let before = visible_rows(&t);
    t.resize(40, 120);
    t.process(&conhost_repaint(&before, 40, (20, 1)));
    let rows = retained_rows(&t);
    let present = |n: usize| rows.iter().any(|r| r == &format!("row {n:02}"));
    assert!(
        !(1..=19).any(present),
        "the native fill + conhost repaint destroys the history it pulled in"
    );
}

/// The bottom-push corner, measured 2026-09-27: a full screen with the cursor
/// on row 3 shrunk 24 -> 16. conhost repaints the cursor's row at row 0 and
/// old rows 4..18 below it, cursor on row 0; the grow back paints the grown
/// rows blank. The native shrink leaves exactly that viewport (so both
/// repaints are no-ops) and keeps the cut-off rows in history: nothing is
/// lost and nothing is retained twice.
#[test]
fn conpty_rows_shrink_in_the_bottom_push_corner_leaves_the_viewport_conhost_paints() {
    let mut t = Terminal::new(24, 80);
    for i in 1..=30 {
        t.process(format!("line {i:02}\r\n").as_bytes());
    }
    // cmd's prompt redrawn on row 3 (conhost's `CSI 4;1H >`), content below.
    t.process(b"\x1b[4;1H>");
    let all_before = retained_rows(&t);
    let screen = visible_rows(&t);
    assert_eq!(screen[3], ">ine 11", "precondition: the prompt over row 3");
    assert_eq!(t.grid().cursor_row(), 3);

    t.resize_with_policy(16, 80, ResizePolicy::ConPty);
    // conhost's measured repaint: old rows 3..18, cursor on row 0 col 1.
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

// ---- pwsh under conhost, byte for byte (captured 2026-09-27) --------------
//
// The fixtures below replay the shapes `aterm ctl cast` recorded from pwsh 7.6
// in an aterm 0.95.0 tab on Windows 11 26200, 23x80 (the presence band had
// taken a row). The prompt's text is replaced by a stand-in of the same length
// (116 columns: it wraps at 80), so every row and every CUP lands where the
// capture put it. The re-verification of 2026-09-27 saw, on this exact
// sequence, a widen to 120 staircase older history rows (40 blanks, then
// `col-test 02`) and a narrow to 60 put a shifted copy of the screen's top
// row into history. Both came from continuation links that outlived the text
// they described: conhost's `cls` erases row by row with `CSI K`, and EL did
// not break the link into the row below (see `Grid::clear_wrap_into_next_row`).

/// A stand-in for the measured 116-column pwsh prompt.
fn pwsh_prompt() -> String {
    format!("PS C:\\{}>", "w".repeat(109))
}

/// One `col-test NN` line of the verifier's recipe: 102 columns.
fn col_test(n: usize) -> String {
    format!("col-test {n:02} {}", UNIT.repeat(9))
}

/// The 22-column tail a `col-test` line leaves on its second row at 80.
const COL_TEST_TAIL: &str = "ijabcdefghijabcdefghij";

/// conhost's `cls` for pwsh (measured): two passes of `CSI K` down every
/// row from the top, the second after `CSI 3J`.
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

/// How conhost writes a run wider than 80 columns that starts on the BOTTOM
/// row (measured): the first 80 columns, CR LF (which scrolls), then
/// `CSI <rows-1>;80 H` re-writing the 80th character so that the rest
/// autowraps onto the new bottom row.
fn bottom_row_run(text: &str, rows: u16) -> Vec<u8> {
    let mut out = text[..80].as_bytes().to_vec();
    out.extend_from_slice(format!("\r\n\x1b[{};80H", rows - 1).as_bytes());
    out.extend_from_slice(text[79..].as_bytes());
    out
}

/// pwsh printing `col-test 01..=40` from the top of a cleared 23x80 screen,
/// then its next prompt (measured): a line that fits goes out as one
/// autowrapping run and CR LF; from `col-test 12` on every line starts on the
/// bottom row and takes the `bottom_row_run` shape, then CR, LF; the prompt
/// takes it too and ends with `CSI 1 C`.
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

/// A pwsh prompt with a typed command after it, as the screen holds it before
/// `cls`: 116 + 70 columns from row 0, so rows 1 and 2 are continuations.
fn prompt_and_command(t: &mut Terminal) {
    t.process(pwsh_prompt().as_bytes());
    t.process(format!("cls; {}\r\n", "x".repeat(65)).as_bytes());
    let g = t.grid();
    assert!(
        g.row(1).is_some_and(|r| r.is_wrapped()) && g.row(2).is_some_and(|r| r.is_wrapped()),
        "precondition: the prompt and its command leave rows 1 and 2 as continuations"
    );
}

/// Every history line, oldest first: its text (trailing blanks trimmed) and
/// whether it continues the line before it.
fn history(t: &Terminal) -> Vec<(String, bool)> {
    let g = t.grid();
    (0..g.scrollback_lines())
        .map(|i| {
            let line = g.get_history_line(i).expect("history line");
            (line.to_string().trim_end().to_string(), line.is_wrapped())
        })
        .collect()
}

/// The 59 history rows the fill leaves at 80 columns: `col-test 01..=29` as
/// head + continuation, then `col-test 30`'s head (its tail is screen row 0).
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

/// conhost's measured repaint after the widen to 23x120: the tail at row 0 as
/// a line of its own, `col-test 31..=40` one row each, the prompt padded to
/// the full 120 columns, ten blank rows, and the cursor after the prompt.
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

/// conhost's measured repaint after narrowing back to 23x80: the tail at row
/// 0, `col-test 31..=40` as autowrapping runs, the prompt, `CSI 1 C`.
fn narrow_to_80_repaint() -> Vec<u8> {
    let mut out = format!("\x1b[?25l\x1b[H{COL_TEST_TAIL}\x1b[K\r\n").into_bytes();
    for n in 31..=40 {
        out.extend_from_slice(format!("{}\x1b[K\r\n", col_test(n)).as_bytes());
    }
    out.extend_from_slice(format!("{}\x1b[K\x1b[1C\x1b[?25h", pwsh_prompt()).as_bytes());
    out
}

/// The terminal shapes a ConPTY tab can have: ring-only; the GUI's (history in
/// a ring thousands of lines deep, a store behind it); and a small ring, so the
/// history lives in the store and reaches the rewrap through the lazy buffer.
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

/// Resize under the ConPTY policy the way the GUI does (the offloaded entry
/// point) and feed conhost's `repaint` before or after the worker's re-attach
/// — conhost's bytes can arrive on either side of it.
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

/// The verifier's widen, replayed: after conhost's `cls` the fill leaves exact
/// history (no continuation link survived from the prompt that sat on those
/// rows); the widen to 120 gives each of `col-test 01..=29` ONE row with no
/// history row shifted, and the boundary line's head stays the newest history
/// line (conhost repaints its 22-column tail at row 0 as a line of its own —
/// the measured `widen_to_120_repaint`); narrowing back restores the 59 rows
/// exactly, three times over.
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

/// pwsh's `cls; 1..30 | % {'pad {0:D2}' -f $_}` at 23x80 (measured: `pad
/// 01..=23` each followed by `CSI K`, the rest scrolled in bare), the prompt
/// at the bottom.
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

/// conhost's measured repaint after that screen shrank to 20x60: `pad 13` at
/// row 0 (it demoted `pad 10..=12`), the prompt padded to two full rows.
fn shrink_to_60_repaint() -> Vec<u8> {
    let mut out = b"\x1b[?25l\x1b[H".to_vec();
    for n in 13..=30 {
        out.extend_from_slice(format!("pad {n:02}\x1b[K\r\n").as_bytes());
    }
    out.extend_from_slice(format!("{}    \x1b[20;58H\x1b[?25h", pwsh_prompt()).as_bytes());
    out
}

/// The verifier's stray row, replayed: short lines written over rows that
/// held wrapped lines, then a shrink to 20x60. History holds exactly the three
/// lines conhost demoted, nothing is retained twice and no row is shifted
/// right — the re-verification saw `pad 03, 05, .., 13` each 20 columns right
/// and a copy of `pad 13` in history while conhost painted it at row 0.
#[test]
fn conpty_column_shrink_after_conhost_cls_pushes_no_stray_row() {
    let pad = |range: std::ops::RangeInclusive<usize>| -> Vec<(String, bool)> {
        range.map(|n| (format!("pad {n:02}"), false)).collect()
    };
    for before_reattach in [true, false] {
        for (label, mut t) in conpty_shapes(23, 80) {
            // The session the capture came from: the widen test's fill, a
            // widen to 120 and back, then the pads.
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

// ---- a shrink with a non-blank row under the cursor (captured 2026-09-27) --
//
// The round-2 review's recipe, replayed from its `aterm ctl cast` (pwsh 7.6,
// aterm 0.95.0, a 24x80 tab): `cls; 1..30 | % { "pad {0:D2}" -f $_ }` went in
// as two input lines, so PSReadLine drew its `>>` continuation prompt, and the
// fill ended with the prompt drawn twice and `>>` on a fresh bottom row, the
// cursor back on the prompt ABOVE it (`CR LF >> CSI 23;38 H`). Every resize the
// review then made is replayed with conhost's repaint for it. The round-3
// review found the widths where the cell after the prompt starts a row of its
// own (58, 39, 29) and a one-line fill with the prompt alone on the bottom
// rows; those runs are replayed too. The captures' OSC marks and SGR changes
// are left out and the prompt is the 116-column stand-in, which is the
// measured prompt's length.

/// The two pad fills the reviews captured at 24x80.
#[derive(Clone, Copy, Debug)]
enum PadPrompt {
    /// The command went in as two input lines, so PSReadLine drew the prompt
    /// twice and its `>>` on a fresh bottom row, and put the cursor back on
    /// the prompt above it (`CR LF >> CSI 23;38 H`). History `pad 01..=11`;
    /// `pad 12..=30` on rows 0..=18, the prompts on rows 19..=22, `>>` on row
    /// 23, the cursor on row 22 one column past the prompt.
    AboveContinuation,
    /// One input line: one prompt, on rows 22..=23, the cursor after it on the
    /// bottom row. History `pad 01..=08`; `pad 09..=30` on rows 0..=21.
    Alone,
}

/// The capture's pad fill: conhost's `cls` (`CSI K` down every row, then
/// `CSI 3J`), `pad 01..=30` (the first 24 with `CSI K`), then the prompt in
/// the bottom-row shape ending in pwsh's `CSI 1 C` — twice with `>>` under the
/// second for `PadPrompt::AboveContinuation`.
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

/// Every resize the review made after the fill, in order, with the pad line
/// conhost repainted at row 0: 20x60 demoted `pad 12..=15` and 20x50 `pad
/// 16..=17`; no other step demoted anything.
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

/// Does the cursor's copy of the prompt take a row more than its text? conhost
/// keeps the cursor's row through the cursor's cell when it rewraps (the prompt
/// and its `CSI 1 C`: 118 cells), so at a width that fits the 116 columns of
/// text in one row fewer than those 118 cells — 58, 39 and 29 measured — the
/// cursor's cell is a row of its own, painted `CSI K`.
fn cursor_row_of_its_own(cols: u16) -> bool {
    let cols = usize::from(cols);
    let text = pwsh_prompt().len();
    (text + 2).div_ceil(cols) > text.div_ceil(cols)
}

/// The rows conhost paints after each of those resizes, down to the last
/// with content: `pad <first>..=30`, the prompt(s) cut at the width with the
/// cursor's own row when it has one, and `>>` for `AboveContinuation`.
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

/// The whole screen after that repaint: the painted rows, blank rows to the
/// bottom.
fn pad_screen(fill: PadPrompt, first: usize, rows: u16, cols: u16) -> Vec<String> {
    let mut screen = pad_painted(fill, first, cols);
    screen.resize(usize::from(rows), String::new());
    screen
}

/// Where conhost puts the cursor after that repaint: one column past the
/// cursor's prompt (its `CSI 1 C`), 0-based.
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

/// conhost's repaint bytes for that screen (the measured shape): each row's
/// text, a prompt's last row ended by its blanks as spaces when it has four
/// or fewer (at 60, 40 and 39 columns) and by `CSI K` otherwise, the cursor's
/// own row as `CSI K`, `>>` for `AboveContinuation`, rows joined by CR LF,
/// `CSI K` rows to the bottom, then the cursor: a CUP, or — when the paint
/// ended on the cursor's own row at the bottom (`Alone` at 58, 39, 29) — the
/// `CSI n C` from column 0 conhost sends there, or nothing at column 0.
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

/// Replay `fill`, then every `(rows, cols, first)` step with conhost's repaint
/// for it, on every ConPTY terminal shape and with the repaint on either side
/// of the off-thread re-attach. After each step the history is exactly what
/// conhost demoted (`pad 01..first`), the screen and the cursor are the ones
/// conhost painted, and every pad line is retained exactly once; at the end
/// the search index equals a fresh build.
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

/// The review's lost line, replayed: a ConPTY shrink with `>>` under the
/// cursor demotes every row conhost demotes, so after conhost's repaint the
/// history is `pad 01..=15` (the review saw `pad 01..=14`, and `pad 15` in
/// neither history nor screen), the screen is the one conhost painted, and no
/// pad line is lost or doubled at any of the fourteen steps — including
/// 20x50, which lost `pad 17` the same way.
#[test]
fn conpty_shrink_with_a_row_below_the_cursor_keeps_every_line() {
    replay_pad_resizes(PadPrompt::AboveContinuation, &PAD_RESIZES);
}

/// The round-3 review's widths, captured 2026-09-27 (pwsh 7.6 in a debug
/// aterm 0.95.0 of this tree, `aterm ctl cast`; `pad_repaint` reproduces every
/// one of these repaints byte for byte, less the SGR bytes and the first
/// resize's `CSI 8;20;58 t`). At 58, 39 and 29 columns conhost's rewrap puts
/// the cell after the prompt (the cursor's, past pwsh's `CSI 1 C`) on a row of
/// its own, so it demotes one row more than a rewrap that clamps the cursor to
/// the prompt's last glyph: with `>>` below, 20x58 demoted `pad 12..=16`
/// (row 0 `pad 17`), 20x39 `pad 17..=18` and 20x29 `pad 19..=20`; the review
/// saw `pad 16` lost at 20x58, in neither history nor screen.
const PAD_RESIZES_EXACT_WIDTHS: [(u16, u16, usize); 6] = [
    (20, 58, 17),
    (24, 80, 17),
    (20, 39, 19),
    (24, 80, 19),
    (20, 29, 21),
    (24, 80, 21),
];

/// The same capture's run with ONE prompt and nothing under it: 20x58
/// demoted `pad 09..=13` (the review saw `pad 13` lost), 20x60 nothing,
/// 20x39 `pad 14`, 20x29 `pad 15`.
const PAD_RESIZES_EXACT_WIDTHS_ALONE: [(u16, u16, usize); 7] = [
    (20, 58, 14),
    (24, 80, 14),
    (20, 60, 14),
    (20, 39, 15),
    (24, 80, 15),
    (20, 29, 16),
    (24, 80, 16),
];

/// A width that fits the prompt's text in one row fewer than the cells
/// through the cursor keeps every line: with `>>` under the prompt, and with
/// the prompt alone on the bottom rows.
#[test]
fn conpty_shrink_to_a_width_the_prompt_fills_exactly_keeps_every_line() {
    replay_pad_resizes(PadPrompt::AboveContinuation, &PAD_RESIZES_EXACT_WIDTHS);
    replay_pad_resizes(PadPrompt::Alone, &PAD_RESIZES_EXACT_WIDTHS_ALONE);
}

/// The rows-only path demotes the same four rows (its trim finds no blank row
/// under the cursor, so all four come off the top): a height-only shrink of
/// the same screen agrees with the rewrap path.
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

/// TEETH: the same first step on the Native policy is the review's defect —
/// the width shrink pushes only the three rows that bring the cursor on
/// screen and cuts `>>`, conhost paints `pad 16` over the `pad 15` it left at
/// row 0, and `pad 15` is gone from history and screen alike.
#[test]
fn native_shrink_with_a_row_below_the_cursor_under_a_conhost_repaint_loses_a_line() {
    let fill = PadPrompt::AboveContinuation;
    let mut t = Terminal::new(24, 80);
    t.process(&pwsh_pad_fill_24(fill));
    t.resize(20, 60);
    t.process(&pad_repaint(fill, 16, 20, 60));
    assert_eq!(history(&t), pad_history(14), "Native pushed three rows");
    assert!(
        !retained_rows(&t).iter().any(|r| r == "pad 15"),
        "pad 15 is in neither history nor screen"
    );
}

/// TEETH for the exact widths: the Native rewrap clamps the cursor to the
/// prompt's last glyph, so at 20x58 it keeps the cursor on the prompt's
/// second row and pushes one row fewer than conhost demoted; conhost's repaint
/// then paints `pad 14` over the `pad 13` left at row 0.
#[test]
fn native_shrink_to_a_width_the_prompt_fills_exactly_loses_a_line() {
    let fill = PadPrompt::Alone;
    let mut t = Terminal::new(24, 80);
    t.process(&pwsh_pad_fill_24(fill));
    t.resize(20, 58);
    assert_eq!(
        (t.grid().cursor_row(), t.grid().cursor_col()),
        (19, 57),
        "Native: the cursor clamped to the prompt's last glyph"
    );
    t.process(&pad_repaint(fill, 14, 20, 58));
    assert_eq!(history(&t), pad_history(12), "Native pushed four rows");
    assert!(
        !retained_rows(&t).iter().any(|r| r == "pad 13"),
        "pad 13 is in neither history nor screen"
    );
}
