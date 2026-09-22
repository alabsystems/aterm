// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TBC 3 (`CSI 3 g`, "clear ALL tab stops") IS TWO FACTS, AND BOTH MUST CROSS
//! EVERY SEAM.
//!
//! The array is the obvious half. The other is the standing instruction that a
//! later WIDEN must not re-seed the every-8 default into the columns it adds —
//! `GridCursorState::tab_defaults_suppressed`, which exists because this grid
//! sizes its tab array lazily where xterm uses a fixed 1024-column bitmap.
//!
//! `Grid::restore_tab_stops` used to carry only the array, so every seam that
//! moves tab state between grids dropped the second fact and inherited
//! `false`: the alt-screen switch (tab stops are global and shared between the
//! two screens, per xterm) and the seamless-update checkpoint. The next window
//! resize then resurrected stops the application had explicitly erased — and
//! on the alt-screen path the resurrected array was copied back onto the MAIN
//! screen at 1049l, where it persisted for the rest of the session.
//!
//! Each test carries its own CONTROL: the same gesture on a terminal that
//! never left the main screen / never round-tripped, which has always been
//! correct. Without the control these would also pass on a terminal that had
//! simply stopped seeding defaults at all.

use aterm_core::terminal::{HostBindings, Terminal};

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

/// Where a HT from column 0 lands. With every stop cleared it must run to the
/// last column, not to column 8.
fn tab_from_home(t: &mut Terminal) -> u16 {
    t.process(b"\x1b[H\t");
    t.grid().cursor_col()
}

#[test]
fn tbc3_survives_the_alt_screen_switch_and_a_widen_under_it() {
    // CONTROL: cleared, widened, never left the main screen.
    let mut control = Terminal::new(24, 80);
    control.process(b"\x1b[3g");
    control.resize(24, 120);
    assert_eq!(stops(&control), Vec::<usize>::new(), "control: still clear");

    // The alt screen shares tab state with the main screen, so a widen taken
    // while a full-screen program is up must obey the same clear.
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[3g");
    t.process(b"\x1b[?1049h");
    t.resize(24, 120);
    assert_eq!(
        stops(&t),
        Vec::<usize>::new(),
        "a widen under the alt screen resurrected the every-8 defaults"
    );

    // …and leaving the alt screen must not copy a resurrected array back onto
    // the main screen, where it would outlive the program that caused it.
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
}

#[test]
fn tbc3_survives_the_seamless_update_checkpoint_and_a_widen_after_it() {
    let mut source = Terminal::new(24, 80);
    source.process(b"\x1b[3g");
    let carry = source
        .checkpoint_carry(0)
        .expect("the terminal produces a carry checkpoint");
    let mut restored = Terminal::from_checkpoint(&carry, HostBindings::none());

    // Both widen. The restored terminal must behave like the one that never
    // went through the handoff — that IS the seamless-update contract.
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

/// Two-sided: a terminal that never issued TBC 3 must still GROW its defaults
/// into the columns a widen adds, or the fix above would be a regression that
/// silently deletes everyone else's tab stops.
#[test]
fn an_ordinary_terminal_still_seeds_defaults_into_the_columns_a_widen_adds() {
    let mut t = Terminal::new(24, 80);
    t.resize(24, 120);
    let got = stops(&t);
    assert!(
        got.contains(&88) && got.contains(&112),
        "a widen must seed the every-8 defaults past the old width: {got:?}"
    );
    // …and the same through both seams, so neither carries suppression it was
    // never given.
    let mut alt = Terminal::new(24, 80);
    alt.process(b"\x1b[?1049h");
    alt.resize(24, 120);
    assert!(
        stops(&alt).contains(&112),
        "the alt-screen seam invented a suppression nobody asked for"
    );
    let src = Terminal::new(24, 80);
    let carry = src.checkpoint_carry(0).expect("carry");
    let mut r = Terminal::from_checkpoint(&carry, HostBindings::none());
    r.resize(24, 120);
    assert!(
        stops(&r).contains(&112),
        "the checkpoint seam invented a suppression nobody asked for"
    );
}

// ---------------------------------------------- retention across the adopt ----

/// RETENTION IS THE HOST'S CONFIG, NOT THE CHECKPOINT'S.
///
/// The seamless-update adopt path builds a live terminal the normal way — the
/// host's `apply_config` calls `set_scrollback_line_limit(scrollback_lines)` —
/// and then calls `restore_checkpoint`, which replaces both grids wholesale
/// with grids `restore_grid` builds with NO limit (deliberately, so the ≤256
/// carried history lines are not dropped on the way in). Nothing put the
/// configured limit back, so every adopted session retained scrollback
/// unbounded for the rest of its life: a seamless update silently withdrew a
/// setting the user had set and the engine had already accepted. `apply_config`
/// could not repair it either — it early-outs when the limit is unchanged, so
/// only a live config EDIT re-armed it.
///
/// The control is the session that never updated: after the same 400 lines it
/// must retain the same number.
#[test]
fn an_adopted_session_keeps_the_configured_scrollback_limit() {
    let feed = |t: &mut Terminal, n: usize| {
        for i in 0..n {
            t.process(format!("line {i}\r\n").as_bytes());
        }
    };
    let retained = |t: &Terminal| t.grid().scrollback().map_or(0, |s| s.line_count());

    // CONTROL: configured, never adopted.
    let mut control = Terminal::new(24, 80);
    control.set_scrollback_line_limit(Some(50));
    feed(&mut control, 400);
    assert_eq!(control.scrollback_line_limit(), Some(50));

    // The adopt: an outgoing session hands its carry to an incoming one that
    // the host configured exactly like the control.
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

/// The same law with the alternate screen UP at the adopt — the polarity that
/// makes the re-imposition's placement load-bearing. `set_scrollback_line_limit`
/// targets the primary-content grid, which is `alt_grid` while the alt screen
/// is up, so the limit must be re-imposed AFTER the restored modes are in
/// place, or it lands on the wrong grid.
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
