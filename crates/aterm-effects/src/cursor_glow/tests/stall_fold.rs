// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold after a stall.

use super::*;

/// S8a: a 350 ms stall on the key whose echo wraps the row (the TUI
/// model — the caret moves to the next row's head). Licensed by the one
/// press in flight as a fold, the last column lit on the echoing tick
/// and one beat later, nothing yet on the new row, the press spent, the
/// pool not forgotten; the next key on the new row lays its own cell
/// with nothing to bridge.
#[test]
fn a_stalled_key_whose_echo_wraps_the_row_lights_the_last_column() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = roll_to(&mut glow, 11, 99, t0, &c, g);
    let declined_before = glow.admission_tally().declined;
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let echo = k + Duration::from_millis(350);
    glow.tick(Some((12, 0)), echo, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        tally.declined, declined_before,
        "the stalled fold is licensed: {:?}",
        tally.last_decline_reason
    );
    let last = glow
        .admission_log()
        .last()
        .map(|r| r.line(echo))
        .expect("a ring row");
    assert!(last.ends_with(" licence=inflight"), "{last}");
    assert!(
        v2_cols(&glow, 11).contains(&99),
        "the last column is lit on the echoing tick: {:?}",
        v2_cols(&glow, 11)
    );
    assert!(
        v2_cols(&glow, 12).is_empty(),
        "nothing on the new row yet: {:?}",
        v2_cols(&glow, 12)
    );
    glow.tick(
        Some((12, 0)),
        echo + Duration::from_millis(100),
        &c,
        g,
        &mut out,
    );
    assert!(
        v2_cols(&glow, 11).contains(&99),
        "…and one beat later (the birth floor)"
    );
    assert_eq!(
        glow.typed_credits_within(echo),
        0,
        "the fold spent its press"
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0, "…and forgot nothing");
    let k2 = echo + Duration::from_millis(200);
    glow.note_typed(k2);
    glow.tick(
        Some((12, 1)),
        k2 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(
        v2_cols(&glow, 12).contains(&0),
        "the next key lays its own cell: {:?}",
        v2_cols(&glow, 12)
    );
    assert_eq!(bridged(&glow), 0, "nothing to bridge");
}

/// The DEFERRED-WRAP echo after a stall (the xterm model: the wrap lands
/// with the next glyph, `(11,99) -> (12,1)`): a fold of two cells paid
/// by two of three presses in flight — both fold cells lit, and the
/// fold spends what it lays, not the whole pool.
#[test]
fn a_deferred_wrap_echo_after_a_stall_lays_both_fold_cells() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = roll_to(&mut glow, 11, 99, t0, &c, g);
    let last = stalled_keys(&mut glow, 3, pre);
    let echo = last + Duration::from_millis(400);
    glow.tick(Some((12, 1)), echo, &c, g, &mut out);
    assert_eq!(
        (
            glow.admission_tally().declined,
            glow.admission_tally().last_decline_reason
        ),
        (0, None)
    );
    assert!(v2_cols(&glow, 11).contains(&99), "{:?}", v2_cols(&glow, 11));
    assert!(v2_cols(&glow, 12).contains(&0), "{:?}", v2_cols(&glow, 12));
    assert_eq!(
        glow.typed_credits_within(echo),
        1,
        "a fold spends what it lays: one press remains for the new row"
    );
}

/// CONTROLS: a program's newline with NO press in flight lays nothing
/// (refused, dark); a fold with more cells than presses is refused AND
/// forgotten (the cross-row edge).
#[test]
fn a_program_newline_with_no_press_in_flight_lays_nothing() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let pre = roll_to(&mut glow, 11, 99, t0, &c, g);
    glow.tick(
        Some((12, 0)),
        pre + Duration::from_millis(600),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a keyless fold is program output"
    );
    assert!(
        !v2_cols(&glow, 11).contains(&99),
        "{:?}",
        v2_cols(&glow, 11)
    );
    assert!(v2_cols(&glow, 12).is_empty());

    let mut short = CursorGlow::default();
    let pre = roll_to(&mut short, 11, 99, t0, &c, g);
    short.note_typed(pre + Duration::from_millis(85));
    short.tick(
        Some((12, 1)),
        pre + Duration::from_millis(500),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        short.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "one press cannot pay a two-cell fold"
    );
    assert_eq!(
        short.in_flight_tally().forgotten,
        1,
        "…and the cross-row refusal forgets the pool"
    );
    assert!(!v2_cols(&short, 11).contains(&99));
    assert!(v2_cols(&short, 12).is_empty());
}

/// A PER-KEY FOLD IS BYTE-IDENTICAL under the origin-row sweep: the
/// key's `Typed` replay already folded its cell onto the row above, and
/// the sweep lays only what no live cell owns — exactly one new cell on
/// the fold, nothing on the landing row for a landing at column 0.
#[test]
fn a_per_key_fold_lays_the_same_cells_under_the_origin_row_sweep() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = roll_to(&mut glow, 11, 99, t0, &c, g);
    let before = v2_cols(&glow, 11);
    let cells_before = glow.v2.laid_cells();
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    glow.tick(Some((12, 0)), k + Duration::from_millis(3), &c, g, &mut out);
    let after = v2_cols(&glow, 11);
    let mut expected = before.clone();
    expected.insert(99);
    assert_eq!(after, expected, "exactly the fold's own cell was added");
    assert!(v2_cols(&glow, 12).is_empty(), "nothing on the landing row");
    assert_eq!(
        glow.v2.laid_cells(),
        cells_before + 1,
        "one new ribbon cell, laid once"
    );
    assert_eq!(glow.admission_tally().declined, 0);
}

/// A STALLED FOLD IS BORN NO EARLIER THAN THE BIRTH FLOOR: the origin-row
/// sweep is dated `max(press, now - 0.25 s)`, so after a 3 s stall the
/// last column is lit on the echoing frame and still lit a beat later,
/// not born three seconds into a cohort that has already faded.
#[test]
fn a_stalled_fold_is_born_no_earlier_than_the_birth_floor() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = roll_to(&mut glow, 11, 99, t0, &c, g);
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let echo = k + Duration::from_millis(3000);
    glow.tick(Some((12, 0)), echo, &c, g, &mut out);
    assert_eq!(glow.admission_tally().declined, 0);
    assert!(
        glow.v2.field_at(11, 99).is_some(),
        "lit on the echoing frame"
    );
    glow.tick(
        Some((12, 0)),
        echo + Duration::from_millis(100),
        &c,
        g,
        &mut out,
    );
    assert!(
        glow.v2.field_at(11, 99).is_some(),
        "…and still lit a beat later: born at the floor, not at the press"
    );
}

/// S8e: the PENDING-WRAP glyph at the last column (the xterm model) —
/// no cursor move is ever observed for it; the print anchor, counting
/// the deferred wrap, sits one past the caret parked on the pane's last
/// cell. The anchored lane admits it on the fresh stamp as a +1 echo
/// (the last column lit on time); the fold that follows with the next
/// key lays the new row's head, once. A keyless print of the same shape
/// is silent.
#[test]
fn a_pending_wrap_glyph_at_the_last_column_lights_on_its_own_anchored_echo() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    // Two keys echoed with a visible caret, the anchor following.
    glow.tick(Some((11, 97)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 97, 1)));
    glow.tick(
        Some((11, 97)),
        t0 + Duration::from_millis(5),
        &c,
        g,
        &mut out,
    );
    let k1 = t0 + Duration::from_millis(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((11, 98, 2)));
    glow.tick(
        Some((11, 98)),
        k1 + Duration::from_millis(3),
        &c,
        g,
        &mut out,
    );
    let k2 = k1 + Duration::from_millis(100);
    glow.note_typed(k2);
    glow.observe_print_anchor(Some((11, 99, 3)));
    glow.tick(
        Some((11, 99)),
        k2 + Duration::from_millis(3),
        &c,
        g,
        &mut out,
    );
    assert_eq!(glow.admission_tally().licensed, 2);
    // The glyph at the last column: no cursor move, the anchor one past.
    let k3 = k2 + Duration::from_millis(100);
    glow.note_typed(k3);
    glow.observe_print_anchor(Some((11, 100, 4)));
    glow.tick(
        Some((11, 99)),
        k3 + Duration::from_millis(3),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (3, 0),
        "the pending-wrap glyph is licensed through the anchored lane: {:?}",
        tally.last_decline_reason
    );
    assert!(
        v2_cols(&glow, 11).contains(&99),
        "the last column is lit on time: {:?}",
        v2_cols(&glow, 11)
    );
    // The fold with the next key: (11,99) -> (12,1).
    let k4 = k3 + Duration::from_millis(100);
    glow.note_typed(k4);
    glow.observe_print_anchor(Some((12, 1, 5)));
    glow.tick(
        Some((12, 1)),
        k4 + Duration::from_millis(3),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!((tally.licensed, tally.declined), (4, 0));
    assert!(v2_cols(&glow, 12).contains(&0), "{:?}", v2_cols(&glow, 12));
    assert_eq!(
        glow.v2.laid_cells(),
        4,
        "97, 98, 99 on row 11 and 0 on row 12 — laid once each"
    );
    let rows: Vec<_> = glow.admission_log().map(|r| (r.origin, r.target)).collect();
    assert_eq!(
        rows,
        vec![
            ((11, 97), (11, 98)),
            ((11, 98), (11, 99)),
            ((11, 99), (11, 100)),
            ((11, 99), (12, 1)),
        ],
        "the ring shows the pending-wrap echo and the fold"
    );

    // CONTROL: the same last-column print with no stamp and no press.
    let mut cold = CursorGlow::default();
    cold.tick(Some((11, 99)), t0, &c, g, &mut out);
    cold.observe_print_anchor(Some((11, 99, 1)));
    cold.tick(
        Some((11, 99)),
        t0 + Duration::from_millis(5),
        &c,
        g,
        &mut out,
    );
    cold.observe_print_anchor(Some((11, 100, 2)));
    cold.tick(
        Some((11, 99)),
        t0 + Duration::from_millis(400),
        &c,
        g,
        &mut out,
    );
    assert_eq!(cold.spawns(), 0, "a keyless last-column print is silent");
    assert!(v2_cols(&cold, 11).is_empty());
}
