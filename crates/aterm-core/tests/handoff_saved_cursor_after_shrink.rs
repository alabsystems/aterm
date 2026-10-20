// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A saved cursor survives a checkpoint where the engine holds it. After 1049 from a prompt
//! on the last row and a one-row shrink, the demote moves the saved slot up with the prompt,
//! as xterm (`AdjustSavedCursor`), Alacritty, kitty, Ghostty and WezTerm do
//! (`Grid::lend_saved_cursor`). A slot on a row the shrink trims stays past the grid,
//! clamped only when DECRC or the 1049 exit reads it (`Grid::set_cursor`), so the
//! checkpoint carries it unclamped and the handoff wire bounds it by the engine's ceiling
//! (`seamless::checkpoint_meta_bound_violation`), not by the grid.
//!
//! The parity tests compare the text under the cursor: a successor restored from the wire
//! has no history behind the saved primary, so a grow back appends a row where the live
//! engine reveals the demoted one.

use aterm_core::terminal::Terminal;

const ROWS: u16 = 56;
const COLS: u16 = 149;
/// The producer's real carry target (`seamless::max_handoff_history_lines`).
const CARRY: usize = 256;

/// A 56-row primary screen with the shell prompt on its last row, as a tab
/// that has scrolled looks when `claude` is typed into it.
fn prompt_on_the_last_row() -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    for i in 0..(usize::from(ROWS) + 4) {
        t.process(format!("$ command {i}\r\n").as_bytes());
    }
    t.process(b"$ claude");
    assert_eq!(t.cursor().row, ROWS - 1, "the prompt sits on the last row");
    t
}

/// The text of the row the cursor is on, and its column.
fn under_cursor(t: &Terminal) -> (String, u16) {
    let cursor = t.cursor();
    (
        t.grid().row_text(cursor.row).expect("cursor row"),
        cursor.col,
    )
}

/// A slot whose row a shrink trims (a blank row below the cursor) names no
/// text: it stays past the shrunken grid, and the wire admits and carries it
/// as the engine holds it. Grown back, both engines restore it onto its row.
#[test]
fn a_slot_on_a_trimmed_row_is_carried_past_the_grid() {
    let mut live = Terminal::new(ROWS, COLS);
    live.process(format!("\x1b[{ROWS};5H\x1b7\x1b[H").as_bytes());
    live.resize(ROWS - 1, COLS);
    let cp = live.checkpoint_carry(CARRY).expect("parser is Ground");
    let saved = cp.saved_cursor_main.expect("DECSC saved the cursor");
    assert_eq!((saved.cursor_row, saved.cursor_col), (ROWS - 1, 4));
    assert!(
        saved.cursor_row >= cp.rows,
        "one row past the shrunken grid"
    );
    let mut restored = Terminal::from_checkpoint(&cp);
    for t in [&mut live, &mut restored] {
        t.resize(ROWS, COLS);
        t.process(b"\x1b8");
    }
    assert_eq!(live.cursor().row, ROWS - 1);
    assert_eq!(restored.cursor(), live.cursor());
}

/// The same shape sideways: a prompt at the last column, then a narrowing. The
/// slot follows its cell through the rewrap, onto the continuation row.
#[test]
fn a_narrowing_after_entering_the_alternate_screen_carries_the_saved_cell_through_the_rewrap() {
    let mut t = Terminal::new(ROWS, COLS);
    let fill: String = "x".repeat(usize::from(COLS) - 1);
    t.process(fill.as_bytes());
    assert_eq!(
        t.cursor().col,
        COLS - 1,
        "the cursor sits on the last column"
    );
    t.process(b"\x1b[?1049h");
    t.resize(ROWS, COLS - 10);
    let cp = t.checkpoint_carry(CARRY).expect("parser is Ground");
    let saved = cp
        .saved_cursor_main
        .expect("1049 saved the main-screen cursor");
    assert_eq!(
        (saved.cursor_row, saved.cursor_col),
        (1, 9),
        "the cell after the 148th `x`, rewrapped at 139 columns"
    );
    t.process(b"\x1b[?1049lZ");
    assert_eq!(
        t.grid().row_text(1).as_deref(),
        Some("xxxxxxxxxZ"),
        "the next glyph follows the line"
    );
}

/// The alternate screen has a DECSC slot of its own, projected by the same
/// function: a full-screen app that saved its cursor on the last row, then
/// lost a row, keeps the slot on the row the demote moved up.
#[test]
fn a_shrink_carries_the_alternate_screen_saved_cursor_with_its_row() {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[?1049h");
    t.process(format!("\x1b[{ROWS};1H\x1b7").as_bytes());
    t.resize(ROWS - 1, COLS);
    let cp = t.checkpoint_carry(CARRY).expect("parser is Ground");
    let saved = cp
        .saved_cursor_alt
        .expect("DECSC on the alternate screen saved into the alt slot");
    assert_eq!(saved.cursor_row, ROWS - 2);
}

/// The live engine and one restored from the checkpoint leave the alternate
/// screen onto the same cell, after the prompt: at the shrunken size, where both
/// cursors agree, and grown back, the production sequence (the outgoing process
/// parks at 55 rows, the successor restores at 55, grows back to 56, and the
/// full-screen app quits). The live grow reveals the demoted row (prompt back on
/// row 55); the restored engine has no history behind the saved primary,
/// appends a row and keeps the prompt on row 54. Both slots followed their
/// prompt, so both land after `$ claude`.
#[test]
fn a_restored_terminal_leaves_the_alternate_screen_onto_the_same_cell_as_the_live_one() {
    for grow_back in [false, true] {
        let mut live = prompt_on_the_last_row();
        live.process(b"\x1b[?1049h");
        live.resize(ROWS - 1, COLS);
        let cp = live.checkpoint_carry(CARRY).expect("parser is Ground");
        let mut restored = Terminal::from_checkpoint(&cp);

        for t in [&mut live, &mut restored] {
            if grow_back {
                t.resize(ROWS, COLS);
            }
            t.process(b"\x1b[?1049l");
        }
        if !grow_back {
            assert_eq!(restored.cursor(), live.cursor());
        }
        let expected = if grow_back { ROWS - 1 } else { ROWS - 2 };
        assert_eq!(live.cursor().row, expected, "grow_back={grow_back}");
        assert_eq!(
            under_cursor(&live),
            ("$ claude".to_string(), 8),
            "grow_back={grow_back}"
        );
        assert_eq!(
            under_cursor(&restored),
            under_cursor(&live),
            "grow_back={grow_back}"
        );
    }
}

/// The same parity for a bare DECRC on the main screen, at the shrunken size
/// and after growing back.
#[test]
fn a_restored_terminal_restores_a_bare_decrc_onto_the_same_cell_as_the_live_one() {
    for grow_back in [false, true] {
        let mut live = prompt_on_the_last_row();
        live.process(b"\x1b7");
        live.resize(ROWS - 1, COLS);
        let cp = live.checkpoint_carry(CARRY).expect("parser is Ground");
        let mut restored = Terminal::from_checkpoint(&cp);

        for t in [&mut live, &mut restored] {
            if grow_back {
                t.resize(ROWS, COLS);
            }
            t.process(b"\x1b[1;1H\x1b8");
        }
        let expected = if grow_back { ROWS - 1 } else { ROWS - 2 };
        assert_eq!(live.cursor().row, expected, "grow_back={grow_back}");
        assert_eq!(
            under_cursor(&live),
            ("$ claude".to_string(), 8),
            "grow_back={grow_back}"
        );
        assert_eq!(
            under_cursor(&restored),
            under_cursor(&live),
            "grow_back={grow_back}"
        );
    }
}

/// Pending-wrap parity: a slot saved at the last column with the wrap flag set, narrowed,
/// carried, widened again, then restored: the next glyph lands on the same cell on both
/// engines (a column clamped at capture would lose the deferred wrap).
#[test]
fn a_restored_terminal_keeps_the_deferred_wrap_the_live_one_keeps() {
    let mut live = Terminal::new(ROWS, COLS);
    let fill: String = "x".repeat(usize::from(COLS));
    live.process(fill.as_bytes());
    live.process(b"\x1b7");
    live.resize(ROWS, COLS - 10);
    let cp = live.checkpoint_carry(CARRY).expect("parser is Ground");
    let mut restored = Terminal::from_checkpoint(&cp);

    for t in [&mut live, &mut restored] {
        t.resize(ROWS, COLS);
        t.process(b"\x1b[1;1H\x1b8Z");
    }
    assert_eq!(restored.cursor(), live.cursor());
}

/// The re-checkpoint identity: the raw projection is a fixed point, so a
/// second handoff carries exactly what the first did.
#[test]
fn the_raw_checkpoint_is_a_fixed_point_of_restore_and_recapture() {
    let mut t = prompt_on_the_last_row();
    t.process(b"\x1b[?1049h");
    t.resize(ROWS - 1, COLS);
    let first = t.checkpoint_carry(CARRY).expect("parser is Ground");
    let restored = Terminal::from_checkpoint(&first);
    let second = restored.checkpoint_carry(CARRY).expect("parser is Ground");
    assert_eq!(second.saved_cursor_main, first.saved_cursor_main);
    assert_eq!(second.saved_cursor_alt, first.saved_cursor_alt);
    assert_eq!(second.rows, first.rows);
    assert_eq!(second.cols, first.cols);
}
