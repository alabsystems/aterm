// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Credit and park laws for the in-flight press pool.

use super::*;

#[test]
fn a_typed_reanchor_spends_its_credit_so_a_keyless_plus_one_after_it_is_refused() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((5, 36)), t0, &c, g, &mut out);
    glow.note_typed(t0 + Duration::from_millis(5));
    let wrap_at = t0 + Duration::from_millis(10);
    glow.tick(Some((5, 3)), wrap_at, &c, g, &mut out);
    let flushed_at = wrap_at + past_park_window();
    glow.tick(Some((5, 3)), flushed_at, &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow),
        vec![("licensed", "key", (5, 36), (5, 3))],
        "the wrap re-anchors on the key's stamp"
    );
    assert_eq!(
        glow.typed_credits_within(flushed_at),
        0,
        "the re-anchor spent the stamp's credit"
    );
    assert!(
        v2_cols(&glow, 5).contains(&2),
        "the re-anchor lays the landing it spent the credit on"
    );
    let keyless = flushed_at + Duration::from_millis(500);
    glow.tick(Some((5, 4)), keyless, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined, tally.last_decline_reason),
        (1, 1, Some(CursorGlow::DECLINE_NO_FRESH_HINT)),
        "a keyless +1 inside the patience is refused"
    );
    assert_eq!(
        glow.in_flight_tally().licensed,
        0,
        "…and never licensed as inflight"
    );
    let after = v2_cols(&glow, 5);
    assert!(
        !after.contains(&3) && !after.contains(&4),
        "…and lays nothing at the keyless move's own landing: {after:?}"
    );
    assert_eq!(
        glow.v2.caret_mirror(),
        (5, 3),
        "the mirror stays where the wrap left it"
    );
}

#[test]
fn a_park_survives_a_hidden_caret_boundary_in_the_gap_before_its_return() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let park_at = k + Duration::from_millis(10);
    glow.tick(Some((11, 2)), park_at, &c, g, &mut out);
    glow.tick(None, park_at + Duration::from_millis(15), &c, g, &mut out);
    glow.tick(
        Some((11, 2)),
        park_at + Duration::from_millis(30),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.in_flight_tally().park_flushed,
        0,
        "the fresh park is spared by the boundary"
    );
    let ret_at = park_at + Duration::from_millis(45);
    glow.tick(Some((11, 6)), ret_at, &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (11, 5), (11, 6))),
        "the return is one echo from the origin: {:?}",
        ring_rows(&glow)
    );
    let in_flight = glow.in_flight_tally();
    assert_eq!((in_flight.park_returns, in_flight.park_flushed), (1, 0));
    assert_eq!(glow.admission_tally().declined, 0);
    assert!(
        dark_in(&glow, 11, 2..6).is_empty(),
        "no dark cell: {:?}",
        dark_in(&glow, 11, 2..6)
    );
    assert_eq!(glow.v2.caret_mirror(), (11, 6));
    assert_eq!(
        glow.typed_credits_within(ret_at),
        0,
        "the press is spent by its own cell"
    );

    let mut stale = CursorGlow::default();
    let pre = pre_roll(&mut stale, 11, t0, &c, g);
    let k = pre + Duration::from_millis(85);
    stale.note_typed(k);
    let park_at = k + Duration::from_millis(10);
    stale.tick(Some((11, 2)), park_at, &c, g, &mut out);
    stale.tick(None, park_at + past_park_window(), &c, g, &mut out);
    stale.tick(
        Some((11, 2)),
        park_at + past_park_window() + Duration::from_millis(15),
        &c,
        g,
        &mut out,
    );
    assert_eq!(stale.in_flight_tally().park_flushed, 1);
    assert_eq!(
        ring_rows(&stale).last(),
        Some(&("licensed", "key", (11, 5), (11, 2))),
        "judged as the retreat it was"
    );
}

#[test]
fn a_stalled_batch_draining_across_the_wrap_wider_than_the_tight_fold_is_paid_exactly() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let pre = roll_to(&mut glow, 11, 99, t0, &c, g);
    let declined_before = glow.admission_tally().declined;
    let last = stalled_keys(&mut glow, 3, pre);
    let echo = last + Duration::from_millis(1000);
    glow.tick(Some((12, 2)), echo, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        tally.declined, declined_before,
        "the coalesced fold is licensed on the pool: {:?}",
        tally.last_decline_reason
    );
    let row = glow
        .admission_log()
        .last()
        .map(|r| (r.licence, r.origin, r.target))
        .expect("a ring row");
    assert_eq!(row, (AdmissionRecord::LICENCE_IN_FLIGHT, (11, 99), (12, 2)));
    assert!(
        v2_cols(&glow, 11).contains(&99),
        "the last column is lit: {:?}",
        v2_cols(&glow, 11)
    );
    assert!(
        dark_in(&glow, 12, 0..2).is_empty(),
        "the new row's head is lit; dark: {:?}",
        dark_in(&glow, 12, 0..2)
    );
    assert_eq!(glow.typed_credits_within(echo), 0, "paid exactly");
    assert_eq!(glow.in_flight_tally().forgotten, 0, "nothing forgotten");
    assert_eq!(glow.v2.caret_mirror(), (12, 2));

    let mut over = CursorGlow::default();
    let pre = roll_to(&mut over, 11, 99, t0, &c, g);
    let last = stalled_keys(&mut over, 4, pre);
    over.tick(
        Some((12, 2)),
        last + Duration::from_millis(1000),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        over.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "an inexact pool is refused"
    );
    assert_eq!(
        over.in_flight_tally().forgotten,
        1,
        "…and the cross-row refusal forgets it"
    );
    assert!(!v2_cols(&over, 11).contains(&99));
    assert!(v2_cols(&over, 12).is_empty());

    let mut cold = CursorGlow::default();
    let pre = roll_to(&mut cold, 11, 99, t0, &c, g);
    cold.tick(
        Some((12, 2)),
        pre + Duration::from_millis(1000),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        cold.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
    assert!(!v2_cols(&cold, 11).contains(&99));
    assert!(v2_cols(&cold, 12).is_empty());
}

#[test]
fn a_spinner_crossing_inside_a_hidden_park_gap_cannot_steal_the_keys_stamp() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let ms = Duration::from_millis;
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    glow.tick(Some((11, 2)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((9, 19, 1)));
    glow.tick(Some((11, 2)), t0 + ms(16), &c, g, &mut out);
    let mut seq = 1u64;
    let mut t = t0 + ms(16);
    for i in 0..6u16 {
        t += ms(120);
        glow.tick(Some((11, 2 + i)), t - ms(10), &c, g, &mut out);
        glow.note_typed(t);
        seq += 1;
        glow.observe_print_anchor(Some((9, 19, seq)));
        glow.tick(None, t + ms(10), &c, g, &mut out);
        seq += 1;
        glow.observe_print_anchor(Some((11, 3 + i, seq)));
        glow.tick(Some((11, 3 + i)), t + ms(50), &c, g, &mut out);
    }
    assert_eq!(glow.spawns(), 6, "PRECONDITION: six visible-lane echoes");
    assert_eq!(
        glow.last_anchor_sweep, None,
        "PRECONDITION: the visible lane never establishes an anchored echo row"
    );
    t += ms(120);
    let k7 = t;
    glow.tick(Some((11, 8)), k7 - ms(10), &c, g, &mut out);
    glow.note_typed(k7);
    glow.tick(None, k7 + ms(10), &c, g, &mut out);
    seq += 1;
    glow.observe_print_anchor(Some((9, 20, seq)));
    glow.tick(None, k7 + ms(30), &c, g, &mut out);
    let spinner_spends = glow
        .admission_log()
        .filter(|r| {
            (r.origin.0 == 9 || r.target.0 == 9) && r.reason != CursorGlow::DECLINE_PROGRAM_ROW
        })
        .count();
    assert_eq!(
        spinner_spends, 0,
        "a program row's crossing inside a hidden park gap must never spend the key's stamp"
    );
    let crossing = glow
        .admission_log()
        .last()
        .map(|r| (r.reason, r.licence, r.origin, r.target))
        .expect("the contested crossing is a ring row");
    assert_eq!(
        crossing,
        (
            CursorGlow::DECLINE_PROGRAM_ROW,
            AdmissionRecord::LICENCE_NONE,
            (9, 19),
            (9, 20)
        ),
        "refused `program-row`: a different row's licensed echo is younger than the stamp window"
    );
    assert_eq!(
        glow.typed_credits_within(k7 + ms(30)),
        1,
        "key 7's press is still in flight"
    );
    seq += 1;
    glow.observe_print_anchor(Some((11, 9, seq)));
    glow.tick(Some((11, 9)), k7 + ms(50), &c, g, &mut out);
    let echo = glow
        .admission_log()
        .last()
        .map(|r| (r.reason, r.licence, r.origin, r.target))
        .expect("the echo is a ring row");
    assert_eq!(
        echo,
        ("licensed", AdmissionRecord::LICENCE_KEY, (11, 8), (11, 9)),
        "key 7's echo is licensed by its own stamp"
    );
    assert!(
        dark_in(&glow, 11, 2..9).is_empty(),
        "every typed cell lit; dark: {:?}",
        dark_in(&glow, 11, 2..9)
    );
    assert_eq!(glow.spawns(), 7, "seven keys, seven echoes, none diverted");
    assert_eq!(
        glow.typed_credits_within(k7 + ms(50)),
        0,
        "…and key 7's press is spent by its own cell"
    );
}

#[test]
fn the_coalesced_fold_on_the_alt_screen_is_admitted_under_a_fresh_blink() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    let pre = roll_to(&mut glow, 11, 99, t0, &c, g);
    let declined_before = glow.admission_tally().declined;
    let last = stalled_keys(&mut glow, 3, pre);
    let echo = last + Duration::from_millis(1000);
    glow.note_repaint_blink(echo);
    glow.tick(Some((12, 2)), echo, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        tally.declined, declined_before,
        "the coalesced fold is licensed on the pool under the blink: {:?}",
        tally.last_decline_reason
    );
    let row = glow
        .admission_log()
        .last()
        .map(|r| (r.licence, r.origin, r.target))
        .expect("a ring row");
    assert_eq!(row, (AdmissionRecord::LICENCE_IN_FLIGHT, (11, 99), (12, 2)));
    assert!(
        v2_cols(&glow, 11).contains(&99),
        "the last column is lit: {:?}",
        v2_cols(&glow, 11)
    );
    assert!(
        dark_in(&glow, 12, 0..2).is_empty(),
        "the new row's head is lit; dark: {:?}",
        dark_in(&glow, 12, 0..2)
    );
    assert_eq!(glow.typed_credits_within(echo), 0, "paid exactly");
    assert_eq!(glow.in_flight_tally().forgotten, 0, "nothing forgotten");
    assert_eq!(glow.v2.caret_mirror(), (12, 2));

    let mut blinkless = CursorGlow::default();
    blinkless.note_context(true);
    let pre = roll_to(&mut blinkless, 11, 99, t0, &c, g);
    let last = stalled_keys(&mut blinkless, 3, pre);
    blinkless.tick(
        Some((12, 2)),
        last + Duration::from_millis(1000),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        blinkless.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a blinkless alt-screen hop across the wrap is not the fold"
    );
    assert_eq!(
        blinkless.in_flight_tally().forgotten,
        1,
        "…and the cross-row refusal forgets the pool"
    );
    assert!(!v2_cols(&blinkless, 11).contains(&99));
    assert!(v2_cols(&blinkless, 12).is_empty());
}

#[test]
fn a_keyed_motion_pair_is_not_a_park_return() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    let b = pre + Duration::from_millis(85);
    glow.note_typed(b);
    let park_at = b + Duration::from_millis(10);
    glow.tick(Some((11, 2)), park_at, &c, g, &mut out);
    let dollar = b + Duration::from_millis(100);
    glow.note_typed(dollar);
    let land_at = dollar + Duration::from_millis(10);
    glow.tick(Some((11, 7)), land_at, &c, g, &mut out);
    let in_flight = glow.in_flight_tally();
    assert_eq!(
        (in_flight.park_returns, in_flight.park_flushed),
        (0, 1),
        "not a return: the park flushes"
    );
    let rows = ring_rows(&glow);
    assert_eq!(
        rows[rows.len() - 2],
        ("licensed", "key", (11, 5), (11, 2)),
        "`b` is its own re-anchor: {rows:?}"
    );
    assert_eq!(
        (rows[rows.len() - 1].2, rows[rows.len() - 1].3),
        ((11, 2), (11, 7)),
        "`$` is judged from the landing: {rows:?}"
    );
    assert!(
        !v2_cols(&glow, 11).contains(&5),
        "the origin cell was never typed: {:?}",
        v2_cols(&glow, 11)
    );
    assert_eq!(
        glow.typed_credits_within(land_at),
        0,
        "both presses spent by the cells they laid"
    );
}

#[test]
fn a_stall_longer_than_the_patience_is_refused() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let last = stalled_keys(&mut glow, 4, pre);
    let echo = last + Duration::from_millis(10_500);
    glow.tick(Some((27, 9)), echo, &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a batch older than the patience is program output to the seam"
    );
    assert_eq!(dark_in(&glow, 27, 5..9).len(), 4, "and stays dark");
}

#[test]
fn a_swallowed_batch_cannot_fund_a_program_hop_after_an_unexplained_move() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 12)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 3, t0);
    let edge = last + Duration::from_millis(1000);
    glow.tick(Some((3, 2)), edge, &c, g, &mut out);
    glow.tick(Some((3, 2)), edge + past_park_window(), &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
    assert_eq!(glow.in_flight_tally().park_flushed, 1);
    glow.tick(
        Some((3, 5)),
        last + Duration::from_millis(1500),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (0, 2),
        "the forgotten presses must not license the program's hop"
    );
    assert!(
        v2_cols(&glow, 3).is_empty(),
        "nothing lit: {:?}",
        v2_cols(&glow, 3)
    );
}

#[test]
fn a_press_the_tty_will_not_echo_banks_nothing_for_a_same_row_program_hop() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 4)), t0, &c, g, &mut out);
    for _ in 0..3 {
        glow.note_typed_swallowed_no_echo();
    }
    let last = t0 + Duration::from_millis(255);
    assert!(
        !glow.move_licensed(last),
        "a withheld press stamps no typed licence"
    );
    assert_eq!(
        glow.typed_credits_within(last),
        0,
        "a withheld press banks no credit"
    );
    glow.tick(
        Some((3, 7)),
        last + Duration::from_millis(1000),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined, tally.last_decline_reason),
        (0, 1, Some(CursorGlow::DECLINE_NO_FRESH_HINT)),
        "the program's same-row hop finds no press to spend"
    );
    assert!(
        v2_cols(&glow, 3).is_empty(),
        "nothing lit: {:?}",
        v2_cols(&glow, 3)
    );
    let in_flight = glow.in_flight_tally();
    assert_eq!(
        (
            in_flight.swallowed_no_echo,
            in_flight.credits,
            in_flight.forgotten
        ),
        (3, 0, 0),
        "`trail status` names the three withheld presses and an empty pool"
    );

    let mut banked = CursorGlow::default();
    banked.tick(Some((3, 4)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut banked, 3, t0);
    banked.tick(
        Some((3, 7)),
        last + Duration::from_millis(1000),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        banked.admission_tally().licensed,
        1,
        "banked, the same three presses fund the hop (the admitted residual)"
    );
    assert_eq!(banked.in_flight_tally().swallowed_no_echo, 0);
}

#[test]
fn a_return_licensed_move_and_a_row_change_forget_the_in_flight_credits() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 9)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 5, t0);
    let ret = last + Duration::from_millis(100);
    glow.note_return(ret);
    glow.tick(
        Some((4, 2)),
        ret + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.admission_tally().licensed,
        1,
        "the Return's move is licensed"
    );
    glow.tick(
        Some((4, 6)),
        ret + Duration::from_millis(500),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.last_decline_reason),
        (1, Some(CursorGlow::DECLINE_NO_FRESH_HINT)),
        "a keyless hop on the new row finds no credit"
    );
    assert!(!v2_cols(&glow, 4).contains(&2) && !v2_cols(&glow, 4).contains(&5));
}

#[test]
fn a_glyph_echo_does_not_spend_the_enter_pressed_behind_it() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    glow.note_typed(t0 + ms(10));
    glow.note_typed(t0 + ms(20));
    glow.note_return(t0 + ms(30));
    glow.tick(Some((3, 3)), t0 + ms(50), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (3, 2), (3, 3))),
        "{:?}",
        ring_rows(&glow)
    );
    assert_eq!(
        (
            glow.in_flight_tally().forgotten,
            glow.typed_credits_within(t0 + ms(50))
        ),
        (0, 1),
        "the glyph's echo forgets nothing: `i` is still in flight"
    );
    assert!(
        hint_fresh(glow.return_hint, t0 + ms(50), CursorGlow::RETURN_HINT_FRESH),
        "the Return is kept for its own move"
    );
    glow.tick(Some((3, 4)), t0 + ms(70), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (3, 3), (3, 4))),
        "{:?}",
        ring_rows(&glow)
    );
    assert!(
        dark_in(&glow, 3, 2..4).is_empty(),
        "both letters lit: {:?}",
        v2_cols(&glow, 3)
    );
    assert_eq!(glow.typed_credits_within(t0 + ms(70)), 0);
    glow.tick(Some((4, 0)), t0 + ms(90), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (3, 4), (4, 0))),
        "the Return licenses its response: {:?}",
        ring_rows(&glow)
    );
    assert!(glow.return_hint.is_none(), "…and is consumed by it");
    glow.tick(Some((5, 0)), t0 + ms(110), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (4, 0), (5, 0))),
        "{:?}",
        ring_rows(&glow)
    );

    let mut bare = CursorGlow::default();
    let b0 = Instant::now();
    bare.tick(Some((3, 2)), b0, &c, g, &mut out);
    bare.note_return(b0 + ms(10));
    bare.tick(Some((3, 4)), b0 + ms(30), &c, g, &mut out);
    assert_eq!(
        ring_rows(&bare).last(),
        Some(&("licensed", "key", (3, 2), (3, 4))),
        "{:?}",
        ring_rows(&bare)
    );
    assert!(
        bare.return_hint.is_none(),
        "a bare Return's response spends it"
    );
    bare.tick(Some((3, 5)), b0 + ms(50), &c, g, &mut out);
    assert_eq!(
        ring_rows(&bare).last(),
        Some(&("no-fresh-hint", "none", (3, 4), (3, 5))),
        "{:?}",
        ring_rows(&bare)
    );
}

#[test]
fn a_kill_forgets_the_in_flight_presses_at_the_key() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 4, t0);
    glow.note_kill(last + Duration::from_millis(100), true);
    glow.tick(
        Some((3, 6)),
        last + Duration::from_millis(600),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "a kill forgets the presses at the key"
    );
    assert!(v2_cols(&glow, 3).is_empty());
}

#[test]
fn a_scroll_or_a_band_move_keeps_the_in_flight_pool() {
    type Seam = (&'static str, fn(&mut CursorGlow), u16);
    let seams: [Seam; 2] = [
        ("scroll", |glow| glow.note_scroll(1), 10),
        ("band move", |glow| glow.note_band_move(0, 20, 1), 12),
    ];
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    for (seam, shift, row) in seams {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let pre = pre_roll(&mut glow, 11, t0, &c, g);
        let last = stalled_keys(&mut glow, 3, pre);
        glow.tick(
            Some((11, 5)),
            last + Duration::from_millis(10),
            &c,
            g,
            &mut out,
        );
        shift(&mut glow);
        glow.drop_row_probe();
        let seen = last + Duration::from_millis(100);
        glow.tick(Some((row, 5)), seen, &c, g, &mut out);
        let tally = glow.in_flight_tally();
        assert_eq!(
            (tally.credits, tally.forgotten),
            (3, 0),
            "the {seam} translated the anchor and kept the pool"
        );
        let echo = seen + Duration::from_secs(2);
        glow.tick(Some((row, 8)), echo, &c, g, &mut out);
        assert_eq!(
            glow.in_flight_tally().licensed,
            1,
            "the batch echoed on the moved row ({seam}) and the pool paid ({:?})",
            glow.admission_tally().last_decline_reason
        );
        assert!(dark_in(&glow, row, 5..8).is_empty(), "{seam}");
    }
}

#[test]
fn a_forget_edge_that_finds_only_dead_presses_counts_nothing() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_typed(t0 + Duration::from_millis(85));
    let mut t = t0;
    while t < t0 + Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S + 0.5) {
        t += Duration::from_millis(500);
        glow.tick(Some((3, 5)), t, &c, g, &mut out);
    }
    assert_eq!(
        glow.in_flight_tally().credits,
        0,
        "the clock retired the press for every reader"
    );
    glow.note_kill(t + Duration::from_millis(10), true);
    assert_eq!(
        glow.in_flight_tally().forgotten,
        0,
        "an edge that forgot no live press is not a forget"
    );
    let k = t + Duration::from_millis(100);
    glow.note_typed(k);
    glow.note_kill(k + Duration::from_millis(50), true);
    assert_eq!(glow.in_flight_tally().forgotten, 1);
}

#[test]
fn a_backspace_retires_only_the_newest_in_flight_press() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 3, t0);
    let bs = last + Duration::from_millis(85);
    glow.note_backspace(bs);
    assert_eq!(
        glow.typed_credits_within(bs),
        2,
        "the Backspace pops the key it erases and nothing else"
    );
    let d = bs + Duration::from_millis(85);
    glow.note_typed(d);
    let echo = t0 + Duration::from_millis(2500);
    glow.tick(Some((3, 8)), echo, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (1, 0),
        "three cells against three credits: licensed ({:?})",
        tally.last_decline_reason
    );
    let dark = dark_in(&glow, 3, 5..8);
    assert!(dark.is_empty(), "dark: {dark:?}");
}

#[test]
fn a_batch_draining_inside_the_backspaces_quench_window_is_the_survivors_echo() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 3, t0);
    let bs = last + ms(85);
    glow.note_backspace(bs);
    assert_eq!(glow.typed_credits_within(bs), 2);
    let echo = bs + ms(60);
    glow.tick(Some((3, 7)), echo, &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (3, 5), (3, 7))),
        "{:?}",
        ring_rows(&glow)
    );
    let dark = dark_in(&glow, 3, 5..7);
    assert!(
        dark.is_empty(),
        "the survivors' cells stayed dark: {dark:?}"
    );
    assert_eq!(
        glow.typed_credits_within(echo),
        0,
        "the drain spent both presses"
    );
    for (label, later, target) in [
        ("inside the window", echo + ms(100), 8u16),
        ("past the window", echo + ms(400), 9),
    ] {
        glow.tick(Some((3, target)), later, &c, g, &mut out);
        assert_eq!(
            ring_rows(&glow).last(),
            Some(&("no-fresh-hint", "none", (3, target - 1), (3, target))),
            "{label}: {:?}",
            ring_rows(&glow)
        );
        assert!(
            !v2_cols(&glow, 3).contains(&(target - 1)),
            "{label}: a program cell lit on a phantom credit: {:?}",
            v2_cols(&glow, 3)
        );
    }
}

#[test]
fn a_wide_drain_inside_the_quench_window_is_not_a_backspace_fold() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 5, t0);
    let bs = last + ms(85);
    glow.note_backspace(bs);
    assert_eq!(glow.typed_credits_within(bs), 4);
    let _ = glow.take_cursor_cat_motion_pulse();
    let echo = bs + ms(60);
    glow.tick(Some((3, 9)), echo, &c, g, &mut out);
    let pulse = glow.take_cursor_cat_motion_pulse();
    assert!(
        pulse.is_none_or(|p| p.kind != CursorCatMotionKind::FoldReverse),
        "a forward drain minted a Backspace fold: {pulse:?}"
    );
    let dark = dark_in(&glow, 3, 5..9);
    assert!(dark.is_empty(), "dark: {dark:?}");
    assert_eq!(glow.typed_credits_within(echo), 0);
    assert_eq!(glow.admission_tally().declined, 0);
}

#[test]
fn a_fast_correction_inside_one_frame_gap_lays_its_survivors() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_typed(t0 + ms(10));
    glow.note_typed(t0 + ms(20));
    glow.note_backspace(t0 + ms(30));
    glow.note_typed(t0 + ms(40));
    let echo = t0 + ms(50);
    glow.tick(Some((3, 7)), echo, &c, g, &mut out);
    let dark = dark_in(&glow, 3, 5..7);
    assert!(
        dark.is_empty(),
        "dark: {dark:?} ring: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.typed_credits_within(echo), 0, "both survivors spent");
}

fn anchored_hidden_run(
    glow: &mut CursorGlow,
    t0: Instant,
    c: &GlowConfig,
    g: Geom,
    n_hidden: u16,
) -> (u64, u16) {
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    glow.tick(Some((3, 2)), t0, c, g, &mut out);
    let mut seq = 1u64;
    for col in 2..5u16 {
        let key = at(85 * u64::from(col - 1));
        glow.note_typed(key);
        glow.observe_print_anchor(Some((3, col + 1, seq)));
        seq += 1;
        glow.tick(
            Some((3, col + 1)),
            key + Duration::from_millis(3),
            c,
            g,
            &mut out,
        );
    }
    assert_eq!(glow.spawns(), 3);
    let mut end = 5u16;
    for i in 0..n_hidden {
        let key = at(400 + 100 * u64::from(i));
        glow.note_typed(key);
        end += 1;
        glow.observe_print_anchor(Some((3, end, seq)));
        seq += 1;
        glow.tick(None, key + Duration::from_millis(8), c, g, &mut out);
    }
    assert_eq!(
        glow.spawns(),
        3 + u64::from(n_hidden),
        "the anchored echoes: {:?}",
        ring_rows(glow)
    );
    assert!(dark_in(glow, 3, 2..end).is_empty());
    (seq, end)
}

#[test]
fn a_reappearance_over_an_anchored_run_keeps_the_in_flight_presses() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let (_, end) = anchored_hidden_run(&mut glow, t0, &c, g, 3);
    assert_eq!(end, 8);
    glow.note_typed(at(700));
    glow.note_typed(at(800));
    glow.tick(Some((3, 8)), at(1000), &c, g, &mut out);
    assert_eq!(
        glow.typed_credits_within(at(1000)),
        2,
        "the pool survives the show"
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0);
    glow.tick(Some((3, 10)), at(1100), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (3, 8), (3, 10))),
        "the stalled keys' echo, paid by the pool: {:?}",
        ring_rows(&glow)
    );
    let dark = dark_in(&glow, 3, 2..10);
    assert!(dark.is_empty(), "dark={dark:?}");
    assert_eq!(glow.in_flight_tally().forgotten, 0);
    let mut glow = CursorGlow::default();
    anchored_hidden_run(&mut glow, t0, &c, g, 3);
    glow.note_typed(at(700));
    glow.tick(Some((3, 8)), at(1000), &c, g, &mut out);
    glow.tick(Some((3, 9)), at(1100), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (3, 8), (3, 9))),
        "{:?}",
        ring_rows(&glow)
    );
    assert!(dark_in(&glow, 3, 2..9).is_empty());
    let mut glow = CursorGlow::default();
    anchored_hidden_run(&mut glow, t0, &c, g, 3);
    glow.tick(Some((3, 8)), at(700), &c, g, &mut out);
    glow.note_typed(at(800));
    glow.tick(Some((3, 9)), at(810), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (3, 8), (3, 9))),
        "{:?}",
        ring_rows(&glow)
    );
    assert!(dark_in(&glow, 3, 2..9).is_empty());
    let mut glow = CursorGlow::default();
    let (seq, _) = anchored_hidden_run(&mut glow, t0, &c, g, 3);
    glow.note_typed(at(700));
    glow.note_typed(at(800));
    glow.observe_print_anchor(Some((3, 10, seq)));
    glow.tick(Some((3, 10)), at(1100), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (3, 8), (3, 10))),
        "{:?}",
        ring_rows(&glow)
    );
    assert!(dark_in(&glow, 3, 2..10).is_empty());
    assert_eq!(glow.in_flight_tally().forgotten, 0);
}

fn pending_wrap_paid(
    glow: &mut CursorGlow,
    row: u16,
    t0: Instant,
    c: &GlowConfig,
    g: Geom,
) -> Instant {
    let mut out = Vec::new();
    let ms = Duration::from_millis;
    glow.tick(Some((row, 97)), t0, c, g, &mut out);
    glow.observe_print_anchor(Some((row, 97, 1)));
    glow.tick(Some((row, 97)), t0 + ms(5), c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((row, 98, 2)));
    glow.tick(Some((row, 98)), k1 + ms(3), c, g, &mut out);
    let k2 = k1 + ms(100);
    glow.note_typed(k2);
    glow.observe_print_anchor(Some((row, 99, 3)));
    glow.tick(Some((row, 99)), k2 + ms(3), c, g, &mut out);
    let k3 = k2 + ms(100);
    glow.note_typed(k3);
    glow.observe_print_anchor(Some((row, 100, 4)));
    glow.tick(Some((row, 99)), k3 + ms(3), c, g, &mut out);
    assert_eq!(glow.admission_tally().licensed, 3, "{:?}", ring_rows(glow));
    assert_eq!(glow.typed_credits_within(k3 + ms(3)), 0, "k3 paid");
    assert!(v2_cols(glow, row).contains(&99), "the last column lit");
    k3 + ms(3)
}

#[test]
fn a_keyless_fold_carrying_no_cell_from_a_paid_pending_wrap_is_declined() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let t0 = Instant::now();
    for key in [false, true] {
        let mut glow = CursorGlow::default();
        let paid = pending_wrap_paid(&mut glow, 11, t0, &c, g);
        let fold = paid + ms(350);
        if key {
            glow.note_typed(fold - ms(10));
        }
        glow.tick(Some((12, 0)), fold, &c, g, &mut out);
        let verdict = ring_rows(&glow)
            .last()
            .filter(|row| (row.2, row.3) == ((11, 99), (12, 0)))
            .map(|row| (row.0, row.1));
        assert_eq!(
            (verdict, v2_cols(&glow, 12).is_empty()),
            (
                Some(if key {
                    ("licensed", "key")
                } else {
                    (CursorGlow::DECLINE_NO_FRESH_HINT, "none")
                }),
                true
            ),
            "a key 10 ms before the fold {key}: (the fold's verdict and licence, row 12 dark)"
        );
    }
}

#[test]
fn a_fold_after_a_paid_pending_wrap_glyph_does_not_charge_the_last_column_twice() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let paid = pending_wrap_paid(&mut glow, 11, t0, &c, g);
    let k4 = paid + ms(100);
    glow.note_typed(k4);
    glow.observe_print_anchor(Some((12, 1, 5)));
    glow.tick(Some((12, 1)), k4 + ms(350), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (11, 99), (12, 1))),
        "{:?}",
        ring_rows(&glow)
    );
    assert!(
        v2_cols(&glow, 12).contains(&0),
        "row 12: {:?}",
        v2_cols(&glow, 12)
    );
    assert_eq!(glow.admission_tally().declined, 0);
    assert_eq!(glow.in_flight_tally().forgotten, 0);
    assert_eq!(glow.typed_credits_within(k4 + ms(350)), 0);
    let mut glow = CursorGlow::default();
    let paid = pending_wrap_paid(&mut glow, 11, t0, &c, g);
    let k4 = paid + ms(100);
    glow.note_typed(k4);
    let k5 = k4 + ms(100);
    glow.note_typed(k5);
    let echo4 = k5 + ms(350);
    glow.observe_print_anchor(Some((12, 1, 5)));
    glow.tick(Some((12, 1)), echo4, &c, g, &mut out);
    assert_eq!(
        glow.typed_credits_within(echo4),
        1,
        "the fold spent one press"
    );
    glow.observe_print_anchor(Some((12, 2, 6)));
    glow.tick(Some((12, 2)), echo4 + ms(40), &c, g, &mut out);
    assert!(
        v2_cols(&glow, 12).contains(&0) && v2_cols(&glow, 12).contains(&1),
        "row 12: {:?} ring: {:?}",
        v2_cols(&glow, 12),
        ring_rows(&glow)
    );
    assert_eq!(glow.admission_tally().declined, 0);
    let mut glow = CursorGlow::default();
    let r = g.rows as u16 - 1;
    let paid = pending_wrap_paid(&mut glow, r, t0, &c, g);
    let k4 = paid + ms(100);
    glow.note_typed(k4);
    glow.note_scroll(1);
    glow.observe_print_anchor(Some((r, 1, 5)));
    glow.tick(Some((r, 1)), k4 + ms(350), &c, g, &mut out);
    assert!(
        v2_cols(&glow, r).contains(&0) && glow.admission_tally().declined == 0,
        "row {r}: {:?} ring: {:?}",
        v2_cols(&glow, r),
        ring_rows(&glow)
    );
    let mut glow = CursorGlow::default();
    glow.tick(Some((11, 97)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 97, 1)));
    glow.tick(Some((11, 97)), t0 + ms(5), &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((11, 98, 2)));
    glow.tick(Some((11, 98)), k1 + ms(3), &c, g, &mut out);
    let k2 = k1 + ms(100);
    glow.note_typed(k2);
    glow.observe_print_anchor(Some((11, 99, 3)));
    glow.tick(Some((11, 99)), k2 + ms(3), &c, g, &mut out);
    let k3 = k2 + ms(100);
    glow.note_typed(k3);
    let k4 = k3 + ms(100);
    glow.note_typed(k4);
    let echo = k4 + ms(350);
    glow.observe_print_anchor(Some((12, 1, 4)));
    glow.tick(Some((12, 1)), echo, &c, g, &mut out);
    assert!(
        v2_cols(&glow, 11).contains(&99) && v2_cols(&glow, 12).contains(&0),
        "row 11: {:?} row 12: {:?}",
        v2_cols(&glow, 11),
        v2_cols(&glow, 12)
    );
    assert_eq!(glow.admission_tally().declined, 0);
    assert_eq!(
        glow.typed_credits_within(echo),
        0,
        "both presses spent by the fold"
    );
    let mut glow = CursorGlow::default();
    glow.tick(Some((11, 97)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 97, 1)));
    glow.tick(Some((11, 97)), t0 + ms(5), &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((11, 98, 2)));
    glow.tick(Some((11, 98)), k1 + ms(3), &c, g, &mut out);
    let k2 = k1 + ms(100);
    glow.note_typed(k2);
    let k3 = k2 + ms(100);
    glow.note_typed(k3);
    glow.observe_print_anchor(Some((11, 100, 3)));
    glow.tick(Some((11, 99)), k3 + ms(3), &c, g, &mut out);
    assert_eq!(glow.typed_credits_within(k3 + ms(3)), 1, "k3 unpaid");
    assert!(!v2_cols(&glow, 11).contains(&99));
    let k4 = k3 + ms(100);
    glow.note_typed(k4);
    let echo = k4 + ms(350);
    glow.observe_print_anchor(Some((12, 1, 4)));
    glow.tick(Some((12, 1)), echo, &c, g, &mut out);
    assert!(
        v2_cols(&glow, 11).contains(&99) && v2_cols(&glow, 12).contains(&0),
        "row 11: {:?} row 12: {:?}",
        v2_cols(&glow, 11),
        v2_cols(&glow, 12)
    );
    assert_eq!(glow.admission_tally().declined, 0);
    assert_eq!(glow.typed_credits_within(echo), 0);
}

#[test]
fn a_pending_wrap_print_under_a_hidden_caret_is_shown_on_the_pane_edge() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    glow.tick(Some((11, 97)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 97, 1)));
    glow.tick(Some((11, 97)), t0 + ms(5), &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((11, 98, 2)));
    glow.tick(Some((11, 98)), k1 + ms(3), &c, g, &mut out);
    let k2 = k1 + ms(100);
    glow.note_typed(k2);
    glow.observe_print_anchor(Some((11, 99, 3)));
    glow.tick(Some((11, 99)), k2 + ms(3), &c, g, &mut out);
    assert_eq!(glow.admission_tally().licensed, 2);
    let k3 = k2 + ms(100);
    glow.note_typed(k3);
    glow.observe_print_anchor(Some((11, 100, 4)));
    glow.tick(None, k3 + ms(8), &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().licensed,
        3,
        "the edge glyph is licensed through the anchored lane: {:?}",
        ring_rows(&glow)
    );
    assert!(v2_cols(&glow, 11).contains(&99), "the last column lit");
    assert_eq!(glow.wrap_paid_row, Some(11), "…and paid");
    let shown = k3 + ms(40);
    glow.tick(Some((11, 99)), shown, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (3, 0),
        "the show is a same-cell boundary, not a backward hop: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0);
    assert!(glow.held_park.is_none());
    assert_eq!(
        glow.wrap_paid_row,
        Some(11),
        "the paid edge cell's witness survives the show"
    );
    let k4 = shown + ms(100);
    glow.note_typed(k4);
    glow.observe_print_anchor(Some((12, 1, 5)));
    glow.tick(Some((12, 1)), k4 + ms(350), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (11, 99), (12, 1))),
        "the fold is priced at the landing head alone: {:?}",
        ring_rows(&glow)
    );
    assert!(
        v2_cols(&glow, 12).contains(&0),
        "row 12: {:?}",
        v2_cols(&glow, 12)
    );
    assert_eq!(glow.admission_tally().declined, 0);
    assert_eq!(glow.in_flight_tally().forgotten, 0);
    assert_eq!(glow.typed_credits_within(k4 + ms(350)), 0);
}

#[test]
fn mid_line_typing_under_a_hidden_caret_is_shown_where_the_glyph_put_the_caret() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    glow.tick(Some((5, 5)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((5, 10, 1)));
    glow.tick(Some((5, 5)), t0 + ms(5), &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((5, 11, 2)));
    glow.tick(None, k1 + ms(8), &c, g, &mut out);
    assert_eq!(glow.admission_tally().licensed, 1, "{:?}", ring_rows(&glow));
    let k2 = t0 + ms(130);
    glow.note_typed(k2);
    glow.tick(Some((5, 6)), k1 + ms(40), &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (1, 0),
        "the show is where the glyph put the caret: {:?}",
        ring_rows(&glow)
    );
    assert!(glow.held_park.is_none(), "the show is not a park");
    assert_eq!(
        glow.typed_credits_within(k1 + ms(40)),
        1,
        "the second key still in flight"
    );
    let k3 = t0 + ms(215);
    glow.note_typed(k3);
    glow.observe_print_anchor(Some((5, 13, 3)));
    glow.tick(None, t0 + ms(550), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "inflight", (5, 11), (5, 13))),
        "{:?}",
        ring_rows(&glow)
    );
    glow.tick(Some((5, 8)), t0 + ms(590), &c, g, &mut out);
    assert!(
        glow.held_park.is_none(),
        "the show is not a park: {:?}",
        ring_rows(&glow)
    );
    let k4 = t0 + ms(700);
    glow.note_typed(k4);
    glow.observe_print_anchor(Some((5, 14, 4)));
    glow.tick(None, t0 + ms(1100), &c, g, &mut out);
    glow.tick(Some((5, 9)), t0 + ms(1140), &c, g, &mut out);
    let tally = glow.admission_tally();
    let in_flight = glow.in_flight_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (3, 0),
        "three echoes licensed, no show refused: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(
        (
            in_flight.park_flushed,
            in_flight.park_returns,
            in_flight.forgotten
        ),
        (0, 0, 0),
        "nothing held, released or forgotten"
    );
    assert!(
        dark_in(&glow, 5, 10..14).is_empty(),
        "the re-laid tail's cells lit: {:?}",
        v2_cols(&glow, 5)
    );
    assert_eq!(glow.typed_credits_within(t0 + ms(1140)), 0);

    let mut glow = CursorGlow::default();
    glow.tick(Some((5, 5)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((5, 10, 1)));
    glow.tick(Some((5, 5)), t0 + ms(5), &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((5, 11, 2)));
    glow.tick(None, k1 + ms(8), &c, g, &mut out);
    assert_eq!(glow.admission_tally().licensed, 1);
    let k2 = t0 + ms(200);
    glow.note_typed(k2);
    glow.tick(Some((5, 5)), k1 + ms(400), &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().declined,
        0,
        "the caret is where it was: {:?}",
        ring_rows(&glow)
    );
    assert!(glow.held_park.is_none(), "not a park");
    assert_eq!(
        glow.typed_credits_within(k1 + ms(400)),
        1,
        "the second key's press is kept"
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0);
}

#[test]
fn a_backspace_into_an_ime_run_retires_one_glyphs_credit() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let commit = t0 + ms(85);
    glow.note_typed_cells(commit, 6);
    let bs = commit + ms(85);
    glow.note_backspace_erasing(bs, Some(2));
    assert_eq!(
        glow.typed_credits_within(bs),
        4,
        "one glyph's two cells retired, the run's other four kept"
    );
    let echo = t0 + ms(2500);
    glow.tick(Some((3, 9)), echo, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (1, 0),
        "four cells against four credits: licensed ({:?})",
        tally.last_decline_reason
    );
    let dark = dark_in(&glow, 3, 5..9);
    assert!(dark.is_empty(), "dark: {dark:?}");
    glow.note_typed_cells(echo + ms(85), 6);
    glow.note_backspace_erasing(echo + ms(170), None);
    assert_eq!(glow.typed_credits_within(echo + ms(170)), 0);
    glow.note_typed_cells(echo + ms(255), 1);
    glow.note_typed_cells(echo + ms(340), 1);
    glow.note_backspace_erasing(echo + ms(425), Some(2));
    assert_eq!(glow.typed_credits_within(echo + ms(425)), 1);
    glow.note_backspace_erasing(echo + ms(510), None);
    assert_eq!(glow.typed_credits_within(echo + ms(510)), 0);
    let key = echo + ms(595);
    glow.note_typed_cells(key, 1);
    glow.note_typed_cells(key + ms(85), 4);
    assert_eq!(glow.typed_credits_within(key + ms(85)), 5);
    glow.note_backspace_erasing(key + ms(170), Some(2));
    glow.note_backspace_erasing(key + ms(255), Some(2));
    assert_eq!(
        glow.typed_credits_within(key + ms(255)),
        1,
        "the run paid out"
    );
    glow.note_backspace_erasing(key + ms(340), Some(0));
    assert_eq!(
        glow.typed_credits_within(key + ms(340)),
        1,
        "a zero-width glyph's erase pays back nothing and takes no press"
    );
    glow.note_backspace_erasing(key + ms(425), None);
    assert_eq!(
        glow.typed_credits_within(key + ms(425)),
        0,
        "an unpriced erase is the whole newest press, as ever"
    );
}

#[test]
fn an_unpaid_credit_survives_a_hidden_boundary_for_the_whole_patience() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 8, t0);
    glow.tick(None, t0 + Duration::from_millis(3000), &c, g, &mut out);
    glow.tick(
        Some((3, 5)),
        t0 + Duration::from_millis(3010),
        &c,
        g,
        &mut out,
    );
    glow.tick(
        Some((3, 13)),
        last + Duration::from_millis(3100),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (1, 0),
        "the boundary spared the unpaid credits ({:?})",
        tally.last_decline_reason
    );
    assert!(dark_in(&glow, 3, 5..13).is_empty());
}

#[test]
fn a_hidden_caret_reappearing_at_the_end_of_a_stalled_batch_is_the_batch_echo() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let last = stalled_keys(&mut glow, 30, pre);
    glow.tick(None, last + Duration::from_millis(300), &c, g, &mut out);
    glow.tick(
        Some((27, 35)),
        last + Duration::from_millis(310),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.declined, tally.last_decline_reason),
        (0, None),
        "the reappearance is the batch's echo"
    );
    let dark = dark_in(&glow, 27, 5..35);
    assert!(dark.is_empty(), "dark: {dark:?}");
}

#[test]
fn a_key_typed_as_the_stalled_frame_lands_keeps_its_own_echo() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let last = stalled_keys(&mut glow, 30, pre);
    let k31 = last + Duration::from_millis(390);
    glow.note_typed(k31);
    glow.tick(
        Some((27, 35)),
        k31 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(dark_in(&glow, 27, 5..35).is_empty(), "the batch is laid");
    glow.tick(
        Some((27, 36)),
        k31 + Duration::from_millis(40),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.declined, tally.last_decline_reason),
        (0, None),
        "the key typed through the drain is licensed by its own stamp"
    );
    assert!(
        v2_cols(&glow, 27).contains(&35),
        "and its cell is lit: {:?}",
        v2_cols(&glow, 27)
    );
    assert_eq!(
        bridged(&glow),
        0,
        "lit by the host, not repaired by the ledger"
    );
}

#[test]
fn a_stalled_batch_drained_across_two_frames_keeps_its_tail_on_the_ledger() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let last = stalled_keys(&mut glow, 30, pre);
    let f1 = last + Duration::from_millis(400);
    glow.tick(Some((27, 34)), f1, &c, g, &mut out);
    assert!(
        dark_in(&glow, 27, 5..34).is_empty(),
        "the first frame lays 29"
    );
    assert_eq!(
        glow.typed_credits_within(f1),
        1,
        "the thirtieth press is kept on the ring"
    );
    let f2 = f1 + Duration::from_millis(50);
    glow.tick(Some((27, 35)), f2, &c, g, &mut out);
    assert_eq!(
        (
            glow.admission_tally().declined,
            glow.admission_tally().last_decline_reason
        ),
        (0, None),
        "the lone thirtieth is licensed by its own press in flight"
    );
    assert_eq!(
        glow.in_flight_tally().licensed,
        2,
        "batch and tail, both in flight"
    );
    assert!(v2_cols(&glow, 27).contains(&34), "…and lit at once");
    let k = f2 + Duration::from_millis(100);
    glow.note_typed(k);
    glow.tick(
        Some((27, 36)),
        k + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(
        v2_cols(&glow, 27).contains(&34) && v2_cols(&glow, 27).contains(&35),
        "the next key's cell joins the tail: {:?}",
        v2_cols(&glow, 27)
    );
    assert_eq!(
        bridged(&glow),
        0,
        "the host laid the tail; nothing to bridge"
    );
}

#[test]
fn a_hidden_caret_tui_batch_after_a_stall_is_laid_from_the_print_anchor_and_does_not_brand_the_row()
{
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.note_typed(at(100));
    glow.observe_print_anchor(Some((11, 3, 2)));
    glow.tick(None, at(105), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the per-key hidden echo is licensed");
    let last = stalled_keys(&mut glow, 12, at(200));
    glow.observe_print_anchor(Some((11, 15, 3)));
    glow.tick(None, last + Duration::from_millis(3000), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        2,
        "the stalled batch is licensed through the anchored lane ({:?})",
        glow.admission_tally().last_decline_reason
    );
    let dark = dark_in(&glow, 11, 3..15);
    assert!(dark.is_empty(), "dark: {dark:?}");
    let k = last + Duration::from_millis(4000);
    glow.note_typed(k);
    glow.observe_print_anchor(Some((11, 16, 4)));
    glow.tick(None, k + Duration::from_millis(5), &c, g, &mut out);
    assert_eq!(glow.spawns(), 3, "the row is still the echo row");
    assert!(
        glow.admission_log()
            .all(|r| r.reason != CursorGlow::DECLINE_PROGRAM_ROW),
        "the input row was never branded"
    );
}

#[test]
fn a_program_row_advancing_while_presses_are_in_flight_is_not_the_echo() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((9, 10, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 2)));
    glow.tick(None, at(32), &c, g, &mut out);
    glow.note_typed(at(240));
    glow.observe_print_anchor(Some((11, 3, 3)));
    glow.tick(None, at(245), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1);
    glow.note_typed(at(330));
    glow.note_typed(at(420));
    glow.observe_print_anchor(Some((9, 11, 4)));
    glow.tick(None, at(800), &c, g, &mut out);
    let status_row_spends = glow
        .admission_log()
        .filter(|r| {
            (r.origin.0 == 9 || r.target.0 == 9) && r.reason != CursorGlow::DECLINE_PROGRAM_ROW
        })
        .count();
    assert_eq!(
        status_row_spends, 0,
        "a program row must never spend the in-flight presses"
    );
    assert_eq!(glow.spawns(), 1, "nothing was licensed on the status row");
    glow.observe_print_anchor(Some((11, 5, 5)));
    glow.tick(None, at(900), &c, g, &mut out);
    let input_row_echoes = glow
        .admission_log()
        .filter(|r| r.origin.0 == 11 && r.phase == AdmissionPhase::Licensed)
        .count();
    assert_eq!(
        input_row_echoes, 2,
        "the established echo row's late batch is licensed in full"
    );
    assert!(dark_in(&glow, 11, 2..5).is_empty());
}

#[test]
fn an_insert_delivered_into_a_stall_lays_the_keys_behind_it_once() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let _ = stalled_keys(&mut glow, 4, t0);
    glow.note_insert_delivered(t0 + Duration::from_millis(2500), InsertWidth::Cells(10));
    let echo = t0 + Duration::from_millis(3500);
    glow.tick(Some((3, 19)), echo, &c, g, &mut out);
    let rows: Vec<_> = glow
        .admission_log()
        .map(|r| (r.reason, r.licence, r.origin, r.target))
        .collect();
    assert_eq!(
        rows,
        vec![("licensed", AdmissionRecord::LICENCE_INSERT, (3, 5), (3, 19))],
        "one admission, the insert's"
    );
    assert!(dark_in(&glow, 3, 5..19).is_empty());
    assert_eq!(glow.insert_tally().lit, 1);
    assert_eq!(
        glow.typed_credits_within(echo),
        0,
        "the surplus was paid from the in-flight credits"
    );
}
