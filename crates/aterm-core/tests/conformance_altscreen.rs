// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 conformance: binds the real `Terminal` alt-screen (DEC mode 1049) round trip to
//! `aterm_models_core::alt_screen_model` (`AltScreen`).
//!
//! The model proves `MainUntouched` and `CursorRestored` (after 1049h…1049l the main cells
//! and the cursor are what they were) and catches an aliased alternate screen
//! (`Buggy = 1`) and a dropped cursor restore (`Buggy = 2`). This test drives the real
//! `Terminal` through `WriteMain → Enter → Scribble → Leave` via `process`, projects each
//! state onto `<<active, entered, touched, cursor, saved>>` and checks every transition is
//! one the named action admits. It also reads the real screens the projection folds away:
//! 1049h shows a blank alternate screen, and the scribble lands there and not on main. Two
//! negative controls must be refused: a leave that keeps the scribble's cursor (admitted
//! only by the `Buggy = 2` mutant) and a scribble that lands on main (admitted only by the
//! `Buggy = 1` mutant).
//!
//! Projection: a 1-row real terminal `Cells` wide; `active` is
//! `term.is_alternate_screen()`; `cursor` is the real cursor column, which after writing
//! one glyph at column `c - 1` is the 1-based index `c` of the cell written;
//! `touched` compares the real main cells (`Terminal::main_grid`, whichever screen is up)
//! with the ones read at the first `Enter`; `entered` and `saved` are what `Enter` latched
//! (the cursor 1049h saves).

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_models_core::alt_screen_model;
use aterm_spec::verify::validate_transition_tiered;

/// The model's `Cells`.
const CELLS: u16 = 3;

/// The real main cells, glyph by glyph (blank as `' '`).
fn main_cells(term: &Terminal) -> Vec<char> {
    (0..CELLS)
        .map(|c| term.main_grid().cell(0, c).map_or(' ', |cell| cell.char()))
        .collect()
}

/// The cells of the screen on display (the alternate one while 1049 is set).
fn shown_cells(term: &Terminal) -> Vec<char> {
    (0..CELLS)
        .map(|c| term.grid().cell(0, c).map_or(' ', |cell| cell.char()))
        .collect()
}

/// The harness's latches: what `Enter` recorded the first time it ran.
#[derive(Default)]
struct Latch {
    entered: bool,
    main_saved: Vec<char>,
    saved: i64,
}

fn project(term: &Terminal, latch: &Latch) -> BTreeMap<&'static str, i64> {
    let touched = latch.entered && main_cells(term) != latch.main_saved;
    BTreeMap::from([
        ("active", i64::from(term.is_alternate_screen())),
        ("entered", i64::from(latch.entered)),
        ("touched", i64::from(touched)),
        ("cursor", i64::from(term.grid().cursor_col())),
        ("saved", latch.saved),
    ])
}

#[test]
fn real_altscreen_roundtrip_conforms_to_the_altscreen_model() {
    let m = alt_screen_model();
    let check = |prev: &BTreeMap<&'static str, i64>,
                 next: &BTreeMap<&'static str, i64>,
                 action: &str,
                 overrides: &[(&'static str, i64)]| {
        validate_transition_tiered(&m, overrides, prev, next, Some(action), "AltScreen")
    };

    // A 1-row, `Cells`-wide terminal on the main screen, cleared and homed: the model's
    // initial state.
    let mut term = Terminal::new(1, CELLS);
    term.process(b"\x1b[?1049l\x1b[2J\x1b[H");
    let mut latch = Latch::default();
    let mut st = project(&term, &latch);
    assert_eq!(st, m.init_state(), "a fresh terminal projects to Init");

    // WriteMain: 'A' at main column 0, cell 1.
    let prev = st.clone();
    term.process(b"\x1b[1GA");
    st = project(&term, &latch);
    let (ok, why) = check(&prev, &st, "WriteMain", &[]);
    assert!(ok, "real WriteMain {prev:?} -> {st:?} must conform: {why}");

    // Enter (CSI ?1049h): the first entry latches the main cells and the saved cursor.
    let prev = st.clone();
    latch = Latch {
        entered: true,
        main_saved: main_cells(&term),
        saved: prev["cursor"],
    };
    term.process(b"\x1b[?1049h");
    assert!(
        term.is_alternate_screen(),
        "1049h shows the alternate screen"
    );
    assert_eq!(
        shown_cells(&term),
        [' '; CELLS as usize],
        "1049h shows a cleared alternate screen"
    );
    st = project(&term, &latch);
    let (ok, why) = check(&prev, &st, "Enter", &[]);
    assert!(ok, "real Enter {prev:?} -> {st:?} must conform: {why}");

    // Scribble: 'B' at alternate column 1, cell 2; main must stay as it was.
    let prev = st.clone();
    term.process(b"\x1b[2GB");
    st = project(&term, &latch);
    let (ok, why) = check(&prev, &st, "Scribble", &[]);
    assert!(ok, "real Scribble {prev:?} -> {st:?} must conform: {why}");
    assert_eq!(
        shown_cells(&term),
        [' ', 'B', ' '],
        "the scribble lands on the alternate screen"
    );
    assert_eq!(
        main_cells(&term),
        latch.main_saved,
        "the scribble leaves main as it was"
    );
    let after_scribble = st.clone();

    // Leave (CSI ?1049l): main again, the cursor restored to the one 1049h saved.
    term.process(b"\x1b[?1049l");
    st = project(&term, &latch);
    let (ok, why) = check(&after_scribble, &st, "Leave", &[]);
    assert!(
        ok,
        "real Leave {after_scribble:?} -> {st:?} must conform: {why}"
    );
    assert_eq!(
        main_cells(&term),
        latch.main_saved,
        "main survived the round trip"
    );

    // Negative control: a leave that keeps the scribble's cursor. Refused by the
    // committed model, and admitted by the dropped-restore mutant, so the projection the
    // check reads can tell the two apart.
    let mut kept = after_scribble.clone();
    kept.insert("active", 0);
    let (ok, _) = check(&after_scribble, &kept, "Leave", &[]);
    assert!(
        !ok,
        "a Leave that does not restore the cursor MUST be refused"
    );
    let (ok, why) = check(&after_scribble, &kept, "Leave", &[("Buggy", 2)]);
    assert!(ok, "the dropped-restore mutant admits it: {why}");

    // Negative control: a scribble that lands on main. Refused by the committed model, and
    // admitted by the aliasing mutant.
    let mut aliased = after_scribble.clone();
    aliased.insert("touched", 1);
    let before = {
        let mut s = after_scribble.clone();
        s.insert("cursor", 1);
        s
    };
    let (ok, _) = check(&before, &aliased, "Scribble", &[]);
    assert!(!ok, "a Scribble that touches main MUST be refused");
    let (ok, why) = check(&before, &aliased, "Scribble", &[("Buggy", 1)]);
    assert!(ok, "the aliasing mutant admits it: {why}");
}

/// A `?1049l` with the main screen up refines no action of the `AltScreen`
/// model: `Leave` is guarded by `active == 1`, and every other action writes a
/// cell or switches buffers. So it must be a stutter of the projection (same
/// buffer, cells and cursor), here after a full round trip whose saved cursor
/// a restore would reach. aterm ignores it on purpose (`exit_alternate_screen`
/// in aterm-core's `handler_dec.rs` says why); xterm restores the slot.
#[test]
fn a_stray_1049l_is_a_stutter_of_the_altscreen_projection() {
    let mut term = Terminal::new(1, CELLS);
    term.process(b"A\x1b[?1049h\x1b[3GB\x1b[?1049l\x1b[3G");
    let project = |t: &Terminal| (t.is_alternate_screen(), shown_cells(t), t.cursor().col);
    let before = project(&term);
    assert_eq!(
        before,
        (false, vec!['A', ' ', ' '], 2),
        "main screen, A kept, cursor on cell 3"
    );
    term.process(b"\x1b[?1049l");
    assert_eq!(project(&term), before, "the stray reset moved nothing");
}
