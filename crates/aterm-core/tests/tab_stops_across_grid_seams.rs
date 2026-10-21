// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TBC 3 (`CSI 3 g`) is two facts, and both cross every seam: the tab array, and
//! `GridCursorState::tab_defaults_suppressed`, which stops a later widen re-seeding every-8
//! defaults into the added columns (the grid sizes its tab array lazily; xterm uses a fixed
//! 1024-column bitmap). The seams are the alt-screen switch (both screens share tab stops,
//! per xterm) and the seamless-update checkpoint. Each test's control makes the same gesture
//! on a terminal that never crossed the seam.

use aterm_core::terminal::Terminal;

/// The columns that currently carry a stop.
fn stops(t: &Terminal) -> Vec<usize> {
    t.grid()
        .tab_stops()
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s)
        .map(|(i, _)| i)
        .collect()
}

/// Where a HT from column 0 lands: with every stop cleared, the last column.
fn tab_from_home(t: &mut Terminal) -> u16 {
    t.process(b"\x1b[H\t");
    t.grid().cursor_col()
}

#[test]
fn tbc3_survives_the_alt_screen_switch_and_a_widen_under_it() {
    // Control: cleared, widened, never left the main screen.
    let mut control = Terminal::new(24, 80);
    control.process(b"\x1b[3g");
    control.resize(24, 120);
    assert_eq!(stops(&control), Vec::<usize>::new(), "control: still clear");

    // The alt screen shares tab state, so a widen under it obeys the same clear.
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[3g");
    t.process(b"\x1b[?1049h");
    t.resize(24, 120);
    assert_eq!(
        stops(&t),
        Vec::<usize>::new(),
        "a widen under the alt screen resurrected the every-8 defaults"
    );

    // Leaving the alt screen must not copy a resurrected array back onto the main screen.
    t.process(b"\x1b[?1049l");
    assert_eq!(
        stops(&t),
        Vec::<usize>::new(),
        "1049l carried resurrected stops back onto the main screen"
    );
    assert_eq!(
        tab_from_home(&mut t),
        tab_from_home(&mut control),
        "HT must land where the control lands"
    );

    // Shared the other way too: a clear the program issues on the alternate
    // screen holds on the main screen after 1049l, through a widen there.
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?1049h\x1b[3g\x1b[?1049l");
    assert_eq!(stops(&t), Vec::<usize>::new(), "1049l kept the alt's clear");
    t.resize(24, 120);
    assert_eq!(
        stops(&t),
        Vec::<usize>::new(),
        "1049l dropped the alt's suppression"
    );
}

#[test]
fn tbc3_survives_the_seamless_update_checkpoint_and_a_widen_after_it() {
    let mut source = Terminal::new(24, 80);
    source.process(b"\x1b[3g");
    let carry = source
        .checkpoint_carry(0)
        .expect("the terminal produces a carry checkpoint");
    let mut restored = Terminal::from_checkpoint(&carry);

    // Both widen; the restored terminal must behave like the one never handed off.
    source.resize(24, 120);
    restored.resize(24, 120);
    assert_eq!(
        stops(&source),
        Vec::<usize>::new(),
        "control: the source is still clear after its widen"
    );
    assert_eq!(
        stops(&restored),
        stops(&source),
        "the checkpoint dropped the suppression flag, so the widen re-seeded \
         stops the application had erased"
    );
    assert_eq!(
        tab_from_home(&mut restored),
        tab_from_home(&mut source),
        "HT must land where the un-checkpointed terminal lands"
    );
}

/// The other side: without TBC 3 a widen still seeds every-8 defaults into the added columns.
#[test]
fn an_ordinary_terminal_still_seeds_defaults_into_the_columns_a_widen_adds() {
    let mut t = Terminal::new(24, 80);
    t.resize(24, 120);
    let got = stops(&t);
    assert!(
        got.contains(&88) && got.contains(&112),
        "a widen must seed the every-8 defaults past the old width: {got:?}"
    );
    // Neither seam invents a suppression.
    let mut alt = Terminal::new(24, 80);
    alt.process(b"\x1b[?1049h");
    alt.resize(24, 120);
    assert!(
        stops(&alt).contains(&112),
        "the alt-screen seam invented a suppression nobody asked for"
    );
    let src = Terminal::new(24, 80);
    let carry = src.checkpoint_carry(0).expect("carry");
    let mut r = Terminal::from_checkpoint(&carry);
    r.resize(24, 120);
    assert!(
        stops(&r).contains(&112),
        "the checkpoint seam invented a suppression nobody asked for"
    );
}

// ---------------------------------------------- retention across the adopt ----

/// Retention is the host's config, not the checkpoint's: `restore_checkpoint` replaces
/// both grids with unlimited ones (so the carried history is not dropped on the way in),
/// then puts the configured limit back, since `apply_config` only re-applies on change.
#[test]
fn an_adopted_session_keeps_the_configured_scrollback_limit() {
    let feed = |t: &mut Terminal, n: usize| {
        for i in 0..n {
            t.process(format!("line {i}\r\n").as_bytes());
        }
    };
    let retained = |t: &Terminal| t.grid().scrollback().map_or(0, |s| s.line_count());

    // Control: configured, never adopted.
    let mut control = Terminal::new(24, 80);
    control.set_scrollback_line_limit(Some(50));
    feed(&mut control, 400);
    assert_eq!(control.scrollback_line_limit(), Some(50));

    // The adopt: the incoming session is configured exactly like the control.
    let mut outgoing = Terminal::new(24, 80);
    outgoing.set_scrollback_line_limit(Some(50));
    feed(&mut outgoing, 100);
    let carry = outgoing.checkpoint_carry(256).expect("carry");

    let mut adopted = Terminal::new(24, 80);
    adopted.set_scrollback_line_limit(Some(50));
    assert_eq!(
        adopted.scrollback_line_limit(),
        Some(50),
        "fixture: the host's config reached the engine before the adopt"
    );
    adopted.restore_checkpoint(&carry);
    assert_eq!(
        adopted.scrollback_line_limit(),
        Some(50),
        "the adopt withdrew the configured retention limit"
    );
    feed(&mut adopted, 400);
    assert_eq!(
        adopted.scrollback_line_limit(),
        Some(50),
        "…and it must still be the limit after the session runs on"
    );
    assert_eq!(
        retained(&adopted),
        retained(&control),
        "an adopted session must retain what an un-adopted one retains"
    );
}

/// The same with the alt screen up at the adopt. `set_scrollback_line_limit` targets the
/// primary-content grid, which is `alt_grid` while the alt screen is up, so the limit is
/// re-imposed after the restored modes are in place or it lands on the wrong grid.
#[test]
fn an_adopted_session_on_the_alt_screen_keeps_its_limit_through_1049l() {
    let mut outgoing = Terminal::new(24, 80);
    outgoing.set_scrollback_line_limit(Some(50));
    outgoing.process(b"\x1b[?1049h");
    let carry = outgoing.checkpoint_carry(256).expect("carry");

    let mut adopted = Terminal::new(24, 80);
    adopted.set_scrollback_line_limit(Some(50));
    adopted.restore_checkpoint(&carry);
    assert_eq!(
        adopted.scrollback_line_limit(),
        Some(50),
        "on the alt screen"
    );
    adopted.process(b"\x1b[?1049l");
    assert_eq!(
        adopted.scrollback_line_limit(),
        Some(50),
        "…and after the program leaves it"
    );
}
