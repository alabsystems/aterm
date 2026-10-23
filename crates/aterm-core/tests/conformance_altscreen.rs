// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for `Terminal` alt-screen (DEC mode 1049) against `AltScreen`.

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_models_core::alt_screen_model;
use aterm_spec::verify::validate_transition_tiered;

const CELLS: u16 = 3;

fn main_cells(term: &Terminal) -> Vec<char> {
    (0..CELLS)
        .map(|c| term.main_grid().cell(0, c).map_or(' ', |cell| cell.char()))
        .collect()
}

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

    let mut term = Terminal::new(1, CELLS);
    term.process(b"\x1b[?1049l\x1b[2J\x1b[H");
    let mut latch = Latch::default();
    let mut st = project(&term, &latch);
    assert_eq!(st, m.init_state(), "a fresh terminal projects to Init");

    let prev = st.clone();
    term.process(b"\x1b[1GA");
    st = project(&term, &latch);
    let (ok, why) = check(&prev, &st, "WriteMain", &[]);
    assert!(ok, "real WriteMain {prev:?} -> {st:?} must conform: {why}");

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

    let mut kept = after_scribble.clone();
    kept.insert("active", 0);
    let (ok, _) = check(&after_scribble, &kept, "Leave", &[]);
    assert!(
        !ok,
        "a Leave that does not restore the cursor MUST be refused"
    );
    let (ok, why) = check(&after_scribble, &kept, "Leave", &[("Buggy", 2)]);
    assert!(ok, "the dropped-restore mutant admits it: {why}");

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
