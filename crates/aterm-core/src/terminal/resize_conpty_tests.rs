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
/// autowrapping run instead, and no fixture in this file repaints one, so the
/// continuation flags such a run would set are not exercised here.
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
