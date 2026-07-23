// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regression for Codex's inline-history insertion protocol.

use aterm_core::selection::{SelectionSide, SelectionState, SelectionType};
use aterm_core::terminal::Terminal;

const SEED_ROWS: &[u8] = b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE";
const CODEX_INSERT: &[u8] = b"\x1b[1;3r\x1b[3;1H\r\nX\x1b[r";

#[test]
fn codex_top_anchored_decstbm_output_enters_scrollback() {
    let mut term = Terminal::new(5, 10);

    // Seed five visible rows, then replay the byte sequence emitted by Codex
    // 0.144.6's insert_history implementation for a three-row history area:
    // DECSTBM 1..3, CUP to its bottom, CRLF to make room, print, reset margins.
    term.process(SEED_ROWS);
    term.process(CODEX_INSERT);

    let grid = term.grid();
    assert_eq!(term.absolute_row_revision(), 1);
    assert_eq!(
        grid.scrollback_lines(),
        1,
        "Codex's committed row must become scrollable history"
    );
    assert_eq!(
        grid.get_history_line(0)
            .expect("displaced top row is retained")
            .to_string()
            .trim_end(),
        "A"
    );
    for (row, expected) in ['B', 'C', 'X', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(
            grid.cell(row as u16, 0).unwrap().char(),
            expected,
            "visible row {row}; rows below the Codex history area stay fixed"
        );
    }

    term.scroll_display(1);
    assert_eq!(term.grid().display_offset(), 1);
    assert_eq!(
        term.display_row_text(0).as_deref().map(str::trim_end),
        Some("A"),
        "the mouse/control scroll path must make Codex's archived row visible"
    );
    term.scroll_to_bottom();
    assert_eq!(term.grid().display_offset(), 0);
    assert_eq!(
        term.display_row_text(0).as_deref().map(str::trim_end),
        Some("B")
    );
}

#[test]
fn codex_shaped_decstbm_output_on_alternate_screen_remains_ephemeral() {
    let mut term = Terminal::new(5, 10);
    term.process(b"\x1b[?1049h");
    term.process(SEED_ROWS);
    term.process(CODEX_INSERT);

    let grid = term.grid();
    assert_eq!(term.absolute_row_revision(), 0);
    assert_eq!(
        grid.scrollback_lines(),
        0,
        "alternate-screen output must not leak into persistent history"
    );
    for (row, expected) in ['B', 'C', 'X', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(grid.cell(row as u16, 0).unwrap().char(), expected);
    }

    term.process(b"\x1b[?1049l");
    assert_eq!(term.grid().scrollback_lines(), 0);
}

#[test]
fn ordinary_full_screen_scrollback_does_not_bump_piecewise_row_revision() {
    let mut term = Terminal::new(3, 10);
    term.process(b"A\r\nB\r\nC\r\nD");

    assert!(term.grid().scrollback_lines() > 0);
    assert_eq!(
        term.absolute_row_revision(),
        0,
        "a uniform full-screen scroll remains re-anchorable with base_y alone"
    );
}

#[test]
fn codex_scroll_keeps_footer_metadata_on_its_content_in_parser_order() {
    let mut term = Terminal::new(5, 10);
    term.process(SEED_ROWS);

    // Attach every public durable metadata shape to the last footer row.
    term.process(b"\x1b[5;1H\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07");
    term.add_named_mark("footer");
    term.add_annotation("footer annotation");

    // Finish with a new OSC marker in the SAME parser batch. Existing anchors
    // must move with the protected footer, while this post-scroll anchor must
    // be created directly in the new coordinate space (and never shifted twice).
    let mut batch = CODEX_INSERT.to_vec();
    batch.extend_from_slice(b"\x1b[5;1H\x1b]133;A\x07");
    term.process(&batch);

    let mark = &term.command_marks()[0];
    assert_eq!(mark.prompt_start_row, 5);
    assert_eq!(mark.command_start_row, Some(5));
    assert_eq!(mark.output_start_row, Some(5));
    assert_eq!(mark.output_end_row, Some(5));
    assert_eq!(term.terminal_marks()[0].row, 5);
    assert_eq!(term.annotations()[0].row, 5);

    let blocks: Vec<_> = term.all_blocks().collect();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].prompt_start_row, 5);
    assert_eq!(blocks[0].command_start_row, Some(5));
    assert_eq!(blocks[0].output_start_row, Some(5));
    assert_eq!(blocks[0].end_row, Some(5));
    assert_eq!(
        blocks[1].prompt_start_row, 5,
        "metadata created after the splice must not be shifted twice"
    );

    let queued: Vec<_> = std::iter::from_fn(|| term.take_osc_event()).collect();
    assert_eq!(
        queued,
        vec![
            (133, "A;row=5;col=0".into()),
            (133, "B;row=5;col=0".into()),
            (133, "C;row=5;col=0".into()),
            (133, "D;exit=0".into()),
            (133, "A;row=5;col=0".into()),
        ],
        "queued introspection events must follow their footer content, while the same-batch post-splice event is not shifted twice"
    );
}

#[test]
fn codex_scroll_repins_viewport_and_piecewise_reanchors_active_selection() {
    let mut term = Terminal::new(5, 10);
    term.process(SEED_ROWS);
    term.process(CODEX_INSERT);

    // Read one row into the newly-created history and select from live history
    // content (B) through protected-footer content (D). Selection coordinates
    // are terminal-relative even while the viewport is pinned.
    term.scroll_display(1);
    let offset_before = term.grid().display_offset();
    let top_before = term.display_row_text(0).expect("pinned top row");
    assert_eq!(offset_before, 1);
    assert_eq!(top_before.trim_end(), "A");
    {
        let selection = term.text_selection_mut();
        selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(3, 0, SelectionSide::Right);
        selection.complete_selection();
    }

    // Replay the exact Codex insert_history batch while the user is reading
    // scrollback. B enters history, D stays fixed below the DECSTBM margin.
    term.process(CODEX_INSERT);

    assert_eq!(
        term.grid().display_offset(),
        offset_before + 1,
        "the viewport re-pins by the newly archived row"
    );
    assert_eq!(
        term.display_row_text(0).as_deref().map(str::trim_end),
        Some(top_before.trim_end()),
        "the pinned viewport keeps the same top content"
    );

    let selection = term.text_selection();
    assert_eq!(selection.state(), SelectionState::Complete);
    assert_eq!(
        (selection.start().row, selection.end().row),
        (-1, 3),
        "the history endpoint moves by one while the protected-footer endpoint stays fixed"
    );
    assert_eq!(
        term.get_line_text(-1, None).as_deref().map(str::trim_end),
        Some("B"),
        "the translated start anchor remains attached to B in history"
    );
    assert_eq!(
        term.get_line_text(3, None).as_deref().map(str::trim_end),
        Some("D"),
        "the footer end anchor remains attached to D"
    );
    let copied = term
        .selection_to_string()
        .expect("selection remains copyable");
    assert!(
        copied.starts_with('B') && copied.ends_with('D'),
        "{copied:?}"
    );
}

/// FULLSCREEN-GROW regression (owner report, 2026-07-22), two halves.
///
/// Half 1 — a FRESH Codex session scrolls its top-anchored region while the
/// physical top rows are still never-written: those displaced blank rows
/// must NOT mint history (pre-fix they filled the ring with blanks that a
/// later rows-grow revealed as a dead black band above the content).
#[test]
fn codex_fresh_session_never_written_scrolls_mint_no_history() {
    let mut term = Terminal::new(24, 80);
    for i in 0..3u8 {
        let seq = format!("\x1b[1;20r\x1b[20;1H\r\nH{i}\x1b[r");
        term.process(seq.as_bytes());
    }
    assert_eq!(
        term.grid().scrollback_lines(),
        0,
        "never-written displaced rows must not mint blank history"
    );
}

/// Half 2 — a RUNNING Codex session (UI block painted from the top, real
/// transcript archived through its region inserts) grows rows on the
/// window -> fullscreen hop. The reveal must surface the REAL transcript at
/// the very top (no blank band above the content), and the cursor must ride
/// its content line down through the reveal so the post-SIGWINCH repaint and
/// CPR answers anchor correctly.
#[test]
fn codex_fullscreen_grow_reveals_transcript_not_blanks_and_cursor_rides() {
    let mut term = Terminal::new(24, 80);

    // Codex's first paint: a UI block filling the top rows.
    for (row, text) in [
        "codex-banner",
        "model: gpt",
        "dir: ~/aterm",
        "tip: hello",
        "bullet one",
    ]
    .into_iter()
    .enumerate()
    {
        let seq = format!("\x1b[{};1H{text}", row + 1);
        term.process(seq.as_bytes());
    }
    // Three insert-history scrolls displace the top three WRITTEN rows.
    for i in 0..3u8 {
        let seq = format!("\x1b[1;20r\x1b[20;1H\r\nT{i}\x1b[r");
        term.process(seq.as_bytes());
    }
    assert_eq!(
        term.grid().scrollback_lines(),
        3,
        "written displaced rows archive exactly as before"
    );
    // The prompt line, cursor parked on it.
    term.process(b"\x1b[21;1Hprompt>\x1b[21;8H");

    // Window -> fullscreen: rows-only grow.
    term.resize(72, 80);

    // The reveal surfaces the REAL transcript at the top: revealed rows
    // hold the archived lines, in order, starting at visual row 0 — no
    // blank band above the content.
    for (row, expected) in ["codex-banner", "model: gpt", "dir: ~/aterm"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            term.display_row_text(row).as_deref().map(str::trim_end),
            Some(expected),
            "revealed row {row} must hold the archived transcript"
        );
    }

    // The cursor rides its content line down through the reveal.
    let cur = term.grid().cursor();
    assert_eq!(
        term.display_row_text(cur.row as usize)
            .as_deref()
            .map(str::trim_end),
        Some("prompt>"),
        "cursor must stay on the prompt line through the grow (row {})",
        cur.row
    );
}
