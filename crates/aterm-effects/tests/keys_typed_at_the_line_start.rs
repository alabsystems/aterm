// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keys typed at the start of a lit line step one stop a key (found by
//! `tests/walk_continuity_census.rs`, scenario `home-type`).
//!
//! The take: a shell line typed at 12 cps, Home (the host stamps a navigation
//! key: `clear_typed`, `note_motion`), and `XYZ ` typed at the line's start
//! inside the grace, each key an insert the shell answers by reprinting the
//! line one column on. The shift pass (`Ribbon::shift_run`) re-reads each
//! pushed cell's stop at the column it moved to and leaves the walk's anchor
//! (`Cohort::anchor_col`) in place: the colour stays with the screen column
//! (C2) while the light rides the glyph. The keys take the line's first stops
//! `0, 1/16, 2/16, 3/16`, and the pushed text walks on from `4/16`.
//!
//! This is the insert family (`tests/midline_insert_band.rs` reads mid-line
//! inserts for holes, `tests/midline_insert_colour_seam.rs` for colour).
//!
//! The law is the walk's own: text-adjacent lit cells of one line never repeat
//! a stop and never step backward; every column lit before the insert shows the
//! stop it showed. Controls: the same keys typed at the line's end each step
//! forward, and one key at the start steps into the pushed line.

use crate::walk_census::*;

const LINE_CELLS: usize = 30;
const INSERT: &str = "XYZ ";
const SETTLE_MS: u64 = 48;

fn stops(s: &Shell) -> Vec<Option<f32>> {
    s.snaps
        .last()
        .expect("a frame")
        .pos
        .iter()
        .map(|p| p.lit.then_some(p.t).flatten())
        .collect()
}

/// The line typed, `keys` typed at its start (after Home) or at its end,
/// read settled: `(before, after)`.
fn take(keys: &str, at_start: bool) -> (Shell, Vec<Option<f32>>, Vec<Option<f32>>) {
    let mut s = Shell::new(60, 83, Echo::After(2));
    s.type_str(&prose(LINE_CELLS));
    s.idle(SETTLE_MS);
    let before = stops(&s);
    if at_start {
        s.home();
    }
    s.type_str(keys);
    s.idle(SETTLE_MS);
    let after = stops(&s);
    (s, before, after)
}

/// The steps between text-adjacent cells `a..b` of `ts` that do not step
/// forward: `(left index, dt)`.
fn not_forward(ts: &[Option<f32>], a: usize, b: usize) -> Vec<(usize, f32)> {
    (a..b)
        .filter_map(|i| {
            let dt = ts[i + 1]? - ts[i]?;
            (dt <= EPS).then_some((i, dt))
        })
        .collect()
}

#[test]
fn keys_typed_at_the_start_of_a_lit_line_step_one_stop_a_key() {
    // The control: the same keys at the line's end each step forward.
    let (_, _, after) = take(INSERT, false);
    let n = LINE_CELLS + INSERT.len();
    assert!(
        (0..n).all(|i| after[i].is_some()),
        "the control: the whole line is lit"
    );
    let bad = not_forward(&after, LINE_CELLS - 1, n - 1);
    assert!(bad.is_empty(), "the control: typed at the end, {bad:?}");
    // The control: one key at the start steps into the pushed line.
    let (_, _, after) = take("X", true);
    let bad = not_forward(&after, 0, 1);
    assert!(bad.is_empty(), "the control: one key at the start, {bad:?}");

    let (s, before, after) = take(INSERT, true);
    let k = INSERT.len();
    // The premise: the whole line is lit.
    assert!(
        (0..LINE_CELLS + k).all(|i| after[i].is_some()),
        "the premise: the whole line is lit\n{}",
        s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
    );
    // The law this file was written for: the keys step forward.
    let bad = not_forward(&after, 0, k);
    assert!(
        bad.is_empty(),
        "the keys typed at the line's start do not step forward — (left text index, \
         dt) {bad:?}: {:?}\n{}",
        &after[..=k],
        s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
    );
    // §41: every column lit before the insert shows the stop it showed (the
    // colour stays with the screen column while the text slides under it).
    // Checked before the keys' own law so a §41 regression reads as such.
    for i in 0..LINE_CELLS {
        let (b, a) = (before[i].expect("lit"), after[i].expect("lit"));
        assert!(
            (a - b).abs() < 1e-4,
            "§41's law: column {i} kept its stop under the pushed text ({b} -> {a}) — the \
             colour stays with the column\n{}",
            s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
        );
    }
    // And the whole phrase — keys, the pushed text, and the text pushed
    // past the old end — walks on.
    let bad = not_forward(&after, 0, LINE_CELLS + k - 1);
    assert!(
        bad.is_empty(),
        "the line after the insert does not step forward — (left text index, dt) \
         {bad:?}\n{}",
        s.dump(s.snaps.len() - 1, s.snaps.len() - 1)
    );
}
