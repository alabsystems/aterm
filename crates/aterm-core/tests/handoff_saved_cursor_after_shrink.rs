// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REGRESSION: an in-session update refused to apply — on every release, on
//! every attempt, until a hand restart — on any desk where a full-screen app
//! had been started from a shell prompt on the LAST row and the grid then
//! lost a row.
//!
//! Measured 2026-09-22 on the owner's daily driver (v0.87.0 → v0.90.0, six
//! refusals in the log, every one `session N: meta out of bounds at 55x149
//! with 0 carried line(s)`): the "update staged" status bar takes one row of
//! chrome, so every 56-row session was 55 rows when the park ran. The Claude
//! Code tab had entered the alternate screen (1049, which saves the cursor
//! into the main DECSC slot exactly as DECSC does) from a prompt on row 55.
//! The engine, like xterm, never touches that slot on a resize and clamps only
//! when DECRC or the 1049 exit finally reads it (`Grid::set_cursor`) — so the
//! checkpoint honestly carried `saved_cursor_main.cursor_row == 55` at
//! `rows == 55`, and the handoff wire's predicate demanded every saved cursor
//! strictly inside the grid.
//!
//! The wire's bound on a DECSC slot is now the engine's ceiling (see
//! `seamless::checkpoint_meta_bound_violation`), and the slot travels RAW.
//! This fix's first draft clamped the slot at capture instead; it was measured
//! lossy — the successor grows back to 56 rows seconds after Commit and its
//! 1049 exit then landed one row above the live engine's — and the tests
//! below pin the raw carry, the same-size parity AND the grow-back parity that
//! draft would have failed.

use aterm_core::terminal::{HostBindings, Terminal};

const ROWS: u16 = 56;
const COLS: u16 = 149;
/// The producer's real carry target (`seamless::max_handoff_history_lines`).
const CARRY: usize = 256;

/// A 56-row primary screen with the shell prompt on its LAST row, as a tab
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

/// The desk itself: 1049 from the last row, then the one-row shrink the
/// status bar causes. The checkpoint carries the slot as the engine holds it
/// — on a row the grid no longer has — and that is the state the wire admits.
#[test]
fn a_shrink_after_entering_the_alternate_screen_carries_the_saved_cursor_raw() {
    let mut t = prompt_on_the_last_row();
    t.process(b"\x1b[?1049h");
    t.resize(ROWS - 1, COLS);
    let cp = t.checkpoint_carry(CARRY).expect("parser is Ground");
    assert_eq!(cp.rows, ROWS - 1);
    let saved = cp
        .saved_cursor_main
        .expect("1049 saved the main-screen cursor");
    assert_eq!(saved.cursor_row, ROWS - 1, "raw: the row it was saved on");
    assert!(
        saved.cursor_row >= cp.rows,
        "honest state one row past the shrunken grid"
    );
}

/// The same shape sideways: a prompt at the last column, then a narrowing.
#[test]
fn a_narrowing_after_entering_the_alternate_screen_carries_the_saved_column_raw() {
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
        saved.cursor_col,
        COLS - 1,
        "raw: the column it was saved on"
    );
    assert!(saved.cursor_col >= cp.cols);
}

/// The alternate screen has a DECSC slot of its own, projected by the same
/// function: a full-screen app that saved its cursor on the last row, then
/// lost a row, carries that slot raw too.
#[test]
fn a_shrink_carries_the_alternate_screen_saved_cursor_raw() {
    let mut t = Terminal::new(ROWS, COLS);
    t.process(b"\x1b[?1049h");
    t.process(format!("\x1b[{ROWS};1H\x1b7").as_bytes());
    t.resize(ROWS - 1, COLS);
    let cp = t.checkpoint_carry(CARRY).expect("parser is Ground");
    let saved = cp
        .saved_cursor_alt
        .expect("DECSC on the alternate screen saved into the alt slot");
    assert_eq!(saved.cursor_row, ROWS - 1);
}

/// SAME-SIZE PARITY. Read at the shrunken size, the raw slot is clamped by
/// the 1049 exit on both engines: the live one and one restored from the
/// checkpoint land on the same cell, the new last row.
#[test]
fn a_restored_terminal_leaves_the_alternate_screen_onto_the_same_cell_at_the_same_size() {
    let mut live = prompt_on_the_last_row();
    live.process(b"\x1b[?1049h");
    live.resize(ROWS - 1, COLS);
    let cp = live.checkpoint_carry(CARRY).expect("parser is Ground");
    let mut restored = Terminal::from_checkpoint(&cp, HostBindings::none());

    for t in [&mut live, &mut restored] {
        t.process(b"\x1b[?1049l");
    }
    assert_eq!(restored.cursor(), live.cursor());
    assert_eq!(live.cursor().row, ROWS - 2);
}

/// GROW-BACK PARITY — the production sequence. The outgoing process parks at
/// 55 rows; the successor restores at 55, its "finishing" bar folds seconds
/// later and every session grows back to 56; the user quits the full-screen
/// app. The slot was saved on row 55 and the grid has row 55 again, so the
/// cursor belongs on row 55 — on the live engine, on xterm, and on the
/// restored engine. A slot clamped at capture would land on 54.
#[test]
fn a_restored_terminal_leaves_the_alternate_screen_onto_the_same_cell_after_growing_back() {
    let mut live = prompt_on_the_last_row();
    live.process(b"\x1b[?1049h");
    live.resize(ROWS - 1, COLS);
    let cp = live.checkpoint_carry(CARRY).expect("parser is Ground");
    let mut restored = Terminal::from_checkpoint(&cp, HostBindings::none());

    for t in [&mut live, &mut restored] {
        t.resize(ROWS, COLS);
        t.process(b"\x1b[?1049l");
    }
    assert_eq!(live.cursor().row, ROWS - 1, "the row the slot was saved on");
    assert_eq!(restored.cursor(), live.cursor());
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
        let mut restored = Terminal::from_checkpoint(&cp, HostBindings::none());

        for t in [&mut live, &mut restored] {
            if grow_back {
                t.resize(ROWS, COLS);
            }
            t.process(b"\x1b[1;1H\x1b8");
        }
        assert_eq!(restored.cursor(), live.cursor(), "grow_back={grow_back}");
        let expected = if grow_back { ROWS - 1 } else { ROWS - 2 };
        assert_eq!(live.cursor().row, expected, "grow_back={grow_back}");
    }
}

/// Pending-wrap parity: a slot saved at the last column with the wrap flag
/// set, narrowed, carried, widened again, then restored — the next glyph
/// lands on the same cell on both engines. A column clamped at capture would
/// have lost the deferred wrap.
#[test]
fn a_restored_terminal_keeps_the_deferred_wrap_the_live_one_keeps() {
    let mut live = Terminal::new(ROWS, COLS);
    let fill: String = "x".repeat(usize::from(COLS));
    live.process(fill.as_bytes());
    live.process(b"\x1b7");
    live.resize(ROWS, COLS - 10);
    let cp = live.checkpoint_carry(CARRY).expect("parser is Ground");
    let mut restored = Terminal::from_checkpoint(&cp, HostBindings::none());

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
    let restored = Terminal::from_checkpoint(&first, HostBindings::none());
    let second = restored.checkpoint_carry(CARRY).expect("parser is Ground");
    assert_eq!(second.saved_cursor_main, first.saved_cursor_main);
    assert_eq!(second.saved_cursor_alt, first.saved_cursor_alt);
    assert_eq!(second.rows, first.rows);
    assert_eq!(second.cols, first.cols);
}
