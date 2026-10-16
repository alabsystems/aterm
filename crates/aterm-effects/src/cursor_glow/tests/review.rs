// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Credit and park laws for the in-flight press pool (stall, fold, park),
//! each driven by a host shape.

use super::*;

/// A typed re-anchor spends the credit of the stamp it consumes, so a keyless
/// +1 on the row afterwards finds an empty pool: refused, nothing laid, the
/// mirror where the wrap left it.
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
    // The park flushes as the re-anchor on the tick past its window.
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
    // The re-anchor lays exactly one cell, its landing, via the seam's
    // one-cell sweep.
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
    // "Lays nothing" is read at the landing the keyless +1 would light, not as
    // a cell census: the re-anchor's own landing later grows an exit-swoosh
    // backward that is not this move's light.
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

/// A held park survives a hidden-caret boundary in the gap before its return.
/// Ink brackets the park and rewrite in DECTCEM hides and a frame can sample
/// the hidden caret between them; the boundary spares a fresh park, so the
/// return is one +1 from the origin with nothing dark or flushed. Control: a
/// park past its window is judged before the boundary completes.
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
    // The hidden frame, then the caret seen back at the park: a
    // completed hidden boundary with no relocation.
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

    // Control: a park past its window does not survive the hide.
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

/// The coalesced fold on the pool: three keys typed into a stall at the row's
/// end drain across the wrap wider than the tight fold, `(11,99) → (12,2)`.
/// The pool alone licenses it with exact payment: the last column and the new
/// row's first two cells lit, presses spent, nothing forgotten. Controls: a
/// pool that overpays the shape is refused and forgotten; an empty pool is
/// program output.
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
    // A one-second stall: past the typing rhythm's window, so the
    // classifier's coalesced arm stands on the in-flight licence alone.
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

    // Control: four presses for three cells — the pool overpays the
    // shape and does not describe the hop.
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

    // Control: no press in flight — program output.
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

/// A spinner crossing inside a hidden park gap. Ink parks the caret hidden for
/// ~40 ms and shows it one cell on, so every echo is judged on the visible lane
/// and `last_anchor_sweep` is never set. Inside a key's gap the status counter
/// crosses `(9)` → `(10)`, a brandless first advance on another row with the
/// stamp fresh. The anchored lane's hold reads the last licensed row on either
/// lane (`last_licensed_row`), so the spinner spends nothing and the key's real
/// echo is licensed.
#[test]
fn a_spinner_crossing_inside_a_hidden_park_gap_cannot_steal_the_keys_stamp() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let ms = Duration::from_millis;
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    // The caret visible at the prompt; the spinner row seeded at its
    // constant width (`  ⠋ thinking... (1)` ends at column 19).
    glow.tick(Some((11, 2)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((9, 19, 1)));
    glow.tick(Some((11, 2)), t0 + ms(16), &c, g, &mut out);
    let mut seq = 1u64;
    let mut t = t0 + ms(16);
    // Six keys at 120 ms, echoed Ink's way: bracket A parks the caret hidden
    // (the spinner repaints at constant width), bracket B rewrites the row and
    // shows the caret one cell on.
    for i in 0..6u16 {
        t += ms(120);
        // The frame before the key sees the caret where the last
        // rewrite left it.
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
    // Key 7: parked hidden; inside the gap the counter crosses `(9)` →
    // `(10)` — row 9's first-ever advance, the stamp fresh.
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
    // Bracket B: the rewrite and the show — key 7's own echo, licensed
    // by its surviving stamp on the visible lane.
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

/// The coalesced fold on the alt screen: the arm uses the alt screen's own
/// discriminator, `(!ctx_alt || blink_fresh)`, as `rainbow_coalesce` does. A
/// stalled batch draining across the wrap arms the blink with its own repaint
/// bracket (a DECTCEM hide inside DEC 2026) and is licensed; a blinkless
/// cross-row hop on the alt screen stays a relocation (vim's `w` across the
/// wrap).
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
    // A one-second stall: the typing rhythm is aged out, the drain's
    // own repaint bracket blinks in the judging frame.
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

    // Control: alt screen with no blink and no rhythm is not the fold; the hop
    // is refused `no-fresh-hint` and the pool forgotten.
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

/// A keyed motion pair is not a return. vim's `b` (stamped as typed) parks the
/// caret back a word and `$` lands two cells past the origin. The return is
/// funded only by presses banked at or before the park; one press cannot pay
/// two cells, so the park flushes as `b`'s re-anchor and the origin cell stays
/// dark.
#[test]
fn a_keyed_motion_pair_is_not_a_park_return() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 11, t0, &c, g);
    // `b`: back to col 2.
    let b = pre + Duration::from_millis(85);
    glow.note_typed(b);
    let park_at = b + Duration::from_millis(10);
    glow.tick(Some((11, 2)), park_at, &c, g, &mut out);
    // `$`: to col 7, two past the origin, inside the park window.
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

/// A stall longer than the in-flight patience is refused: the patience is a
/// bound, not a licence.
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

/// A swallowed batch cannot fund a program hop after an unexplained move: three
/// unechoed keys, a keyless backward hop (a clear, a modal repaint) that is
/// refused and forgets them, then a keyless forward hop of three on the same
/// row finds an empty pool.
#[test]
fn a_swallowed_batch_cannot_fund_a_program_hop_after_an_unexplained_move() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 12)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 3, t0);
    // The unexplained edge: back to col 2, keyless, held as a park candidate
    // and refused (with its forget) past the park window.
    let edge = last + Duration::from_millis(1000);
    glow.tick(Some((3, 2)), edge, &c, g, &mut out);
    glow.tick(Some((3, 2)), edge + past_park_window(), &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
    assert_eq!(glow.in_flight_tally().park_flushed, 1);
    // A program's same-row forward hop of exactly the swallowed count.
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

/// A press the tty will not echo banks nothing: the host reads canonical
/// no-echo off the pty and calls
/// [`CursorGlow::note_typed_swallowed_no_echo`] instead of the typed hint.
/// Three such presses, then a same-row forward program hop of three cells
/// (`read -s`'s caller printing) is refused, and the tally says why. Positive
/// control: the same presses banked license the hop.
#[test]
fn a_press_the_tty_will_not_echo_banks_nothing_for_a_same_row_program_hop() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 4)), t0, &c, g, &mut out);
    let mut last = t0;
    for i in 0..3u64 {
        last = t0 + Duration::from_millis(85 * (i + 1));
        glow.note_typed_swallowed_no_echo();
    }
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

    // Positive control: the same presses banked license the same hop.
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

/// A Return-licensed move and a row change forget the in-flight credits: five
/// unechoed keys (a password), Return, the prompt's row move; a keyless forward
/// hop on the new row is refused.
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

/// A glyph's echo does not spend the Enter pressed behind it: a same-row
/// forward hop with a typed pair is a glyph's echo, so it neither takes the
/// Return (or composer newline) nor forgets, and the Return is spent on its own
/// row change. Shape: `gi⏎` inside a quarter second with `g`'s echo in flight
/// (ordinary over SSH).
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
    // Frame 1: `g`'s echo, one cell, with the Return hint fresh.
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
    // Frame 2: `i`'s echo, licensed by its stamp and its unpaid press.
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
    // Frame 3: the Return's own move — the row change it licenses and
    // is spent on.
    glow.tick(Some((4, 0)), t0 + ms(90), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "key", (3, 4), (4, 0))),
        "the Return licenses its response: {:?}",
        ring_rows(&glow)
    );
    assert!(glow.return_hint.is_none(), "…and is consumed by it");
    // Frame 4: consume-once — the program flood after it is refused.
    glow.tick(Some((5, 0)), t0 + ms(110), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (4, 0), (5, 0))),
        "{:?}",
        ring_rows(&glow)
    );

    // Control: a same-row forward hop with no typed pair is the Return's
    // (a prompt that answers on the row): taken, consumed once.
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

/// A kill forgets the in-flight presses at the key (the line's content is
/// gone). The arrow's twin, which forgets at its own licensed move, is the
/// control in `seam_laws.rs`'s
/// `a_glyph_echo_landing_after_an_arrow_is_the_glyphs_not_the_arrows`.
#[test]
fn a_kill_forgets_the_in_flight_presses_at_the_key() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    // Four keys in flight, ^U at the key, then a keyless forward hop of
    // four half a second later.
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

/// A scroll or row-band move keeps the pool. Three keys in flight on row 11,
/// the grid scrolls one row, and the batch echoes on row 10 two seconds later
/// as a +3 licensed by the pool (`licence=inflight`, nothing forgotten).
/// `translate_scroll_state` moves anchors but not the ring. The band case does
/// the same through `note_band_move`.
#[test]
fn a_scroll_or_a_band_move_keeps_the_in_flight_pool() {
    /// The seam, the move, and the row the caret is seen on after it.
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

/// A forget edge that finds only dead presses counts nothing: one press, the
/// whole patience of silent ticks on a visible caret, then a kill. The pool
/// already reads empty, so `inflight_forgotten=` must not rise.
#[test]
fn a_forget_edge_that_finds_only_dead_presses_counts_nothing() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_typed(t0 + Duration::from_millis(85));
    // The patience and a beat more, every tick visible at the same cell.
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
    // …and the same edge with a live press behind it still counts.
    let k = t + Duration::from_millis(100);
    glow.note_typed(k);
    glow.note_kill(k + Duration::from_millis(50), true);
    assert_eq!(glow.in_flight_tally().forgotten, 1);
}

/// A Backspace retires only the newest in-flight press: `abc⌫d` typed into a
/// stall echoes as three cells against `a`, `b` and `d`, 2.5 s after the first
/// key.
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

/// A Backspace's echo is a retreat and never explains a forward hop: `abc`
/// into a stall, `⌫`, and the app echoes the two survivors as one `+2` inside
/// the Backspace's quench window. The batch is coalesced, both credits spent,
/// both cells lit, and a keyless `+1` afterwards is refused.
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
    // A keyless `+1` inside the quench window: the drain consumed the
    // Backspace's one-shot, so nothing licenses it (a spinner must not light
    // each step). Then one after the window closes: no press in flight,
    // refused.
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

/// The same drain three cells wide (`abcde⌫`) is a coalesce, not a Backspace
/// re-anchor: no `FoldReverse` cat pulse for a forward hop, every survivor's
/// cell lit, the pool spent.
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

/// The non-stall case: `ab⌫c` inside one frame gap, echoed as one `+2`; both
/// cells lit, pool empty.
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

/// Pre-roll `2..5` visible with the print anchor fed, then `n_hidden` keys
/// echoed through the anchored lane under a hidden caret (Claude Code's
/// per-keystroke bracket). Returns the next anchor sequence and the row's end
/// column.
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

/// The reappearance over an anchored run keeps the presses in flight. A
/// licensed anchored echo under a hidden caret moves the hide-bridge source to
/// its landing (where the mirror is), so a later show is judged from there: a
/// same-cell reappearance keeps the pool and fresh stamps, and stalled keys
/// echo licensed.
#[test]
fn a_reappearance_over_an_anchored_run_keeps_the_in_flight_presses() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    // Two presses in flight when the caret is shown at the anchor.
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
    // One press in flight.
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
    // No press in flight; a new key after the show echoes visibly.
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
    // The show lands with the stalled echo already in: two cells for
    // two presses, judged from the anchored run's end.
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

/// Pre-roll on `row` of a 100-column grid: `k1` echoes `97 -> 98`, `k2`
/// `98 -> 99` (visible, anchor fed), and `k3` prints at the last column with no
/// caret move (xterm pending wrap), licensed by the anchored lane's
/// `wrap_parked` arm. Returns the clock after `k3`'s echo, every press paid.
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

/// A fold after a paid pending-wrap glyph does not charge the last column
/// twice. The row whose edge glyph the anchored lane paid is remembered
/// (`wrap_paid_row`, consumed by the next observed caret move, carried across
/// a scroll or band move), and the fold `99 -> (r+1, 1)` from it prices only
/// the landing head.
#[test]
fn a_fold_after_a_paid_pending_wrap_glyph_does_not_charge_the_last_column_twice() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let t0 = Instant::now();
    // One stalled key folds.
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
    // Two stalled keys: the fold, then the second key's `+1`.
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
    // The grid's bottom row: the wrap scrolls before the fold is seen.
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
    // Control: the last-column glyph arrives with the fold (k3 and k4
    // stall together): two cells from two presses.
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
    // Control: the pending-wrap glyph in the same tick as a caret move
    // (`98 -> 99`): the visible lane owns the tick and nothing paid cell 99, so
    // the fold lays both cells from two presses.
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

/// The pending-wrap print under a hidden caret. At the pane's edge the anchored
/// landing `ac == pane_col1` is off-grid while the caret shows at
/// `pane_col1 - 1`, so the hide-bridge source is clamped to the last on-grid
/// column: the show is a same-cell boundary, `wrap_paid_row` survives, and the
/// following fold lights the new row's head.
#[test]
fn a_pending_wrap_print_under_a_hidden_caret_is_shown_on_the_pane_edge() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    // Two keys echoed with a visible caret, the anchor following.
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
    // The glyph at the last column printed under a hidden caret (the
    // TUI's bracket): the anchor one past the pane's edge.
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
    // The show: the caret parked on the pane's last column.
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
    // The stalled fold from the paid edge: one press, the new row's
    // head alone.
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

/// Mid-line typing under a hidden caret. Ink re-lays the row's tail on each
/// key, so the print anchor lands at the row's end while the caret shows
/// mid-row. A caret last shown left of the row's remembered end advances by
/// the echo's width, so every show is a same-cell boundary: nothing refused,
/// held, spent or forgotten, and stalled keys' echoes are paid by the pool.
#[test]
fn mid_line_typing_under_a_hidden_caret_is_shown_where_the_glyph_put_the_caret() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    // A row of ten cells, the caret shown mid-row at col 5, the print
    // anchor at the row's end.
    glow.tick(Some((5, 5)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((5, 10, 1)));
    glow.tick(Some((5, 5)), t0 + ms(5), &c, g, &mut out);
    // One key on time: hidden, the tail re-laid to 11 — and a second
    // key pressed before the caret is shown at 6.
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
    // The second key stalls with a third; they echo together (+2) under the
    // hide, caret shown at 8; then a fourth key's +1, shown at 9. No park, no
    // refusal, nothing forgotten.
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

    // A caret shown exactly where last observed, left of the relocated source,
    // is no move: not a park, refusal or wipe, and the pool is kept.
    let mut glow = CursorGlow::default();
    glow.tick(Some((5, 5)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((5, 10, 1)));
    glow.tick(Some((5, 5)), t0 + ms(5), &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((5, 11, 2)));
    glow.tick(None, k1 + ms(8), &c, g, &mut out);
    assert_eq!(glow.admission_tally().licensed, 1);
    // A second key pressed; the caret shown back at 5 — the end's
    // advance was not the caret's.
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

/// A Backspace into a committed IME run retires one glyph's credit, not the
/// run's. An IME commit banks as one press priced at its summed width (three
/// CJK glyphs: six cells); [`CursorGlow::note_backspace_erasing`] prices the
/// erased glyph (two cells) and the slot keeps four, so the survivors' +4 echo
/// is paid. An unpriced erase (`None`) retires the whole newest press; a priced
/// zero (`Some(0)`) retires nothing.
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
    // An unpriced erase is still the whole-slot retire.
    glow.note_typed_cells(echo + ms(85), 6);
    glow.note_backspace_erasing(echo + ms(170), None);
    assert_eq!(glow.typed_credits_within(echo + ms(170)), 0);
    // …and a priced erase wider than what the newest press still holds
    // takes the slot and no more: the glyph belonged to that press.
    glow.note_typed_cells(echo + ms(255), 1);
    glow.note_typed_cells(echo + ms(340), 1);
    glow.note_backspace_erasing(echo + ms(425), Some(2));
    assert_eq!(glow.typed_credits_within(echo + ms(425)), 1);
    // A priced zero retires nothing: a key `x`, then a run starting with a
    // baseless combining mark (`\u{0301}日本`: widths 0, 2, 2). Erasing `本`
    // and `日` pays the run out; erasing the mark does not take `x`. Only an
    // unpriced erase does. (The pool is emptied of the older press first.)
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

/// An unpaid credit survives a hidden boundary for the whole patience: eight
/// keys in flight, a same-cell hidden→visible boundary at 3 s, the echo 100 ms
/// later is licensed.
#[test]
fn an_unpaid_credit_survives_a_hidden_boundary_for_the_whole_patience() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 8, t0);
    // A frame catches the bracket hidden, the next sees the caret back
    // where it was.
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

/// A hidden caret reappearing at the end of a stalled batch is the batch's
/// echo: the caret reappears thirty cells on, beyond the hide bridge's reach.
/// Under Rainbow Kitty a same-row forward reappearance the credits pay for is
/// judged as the echo, not a relocation that wipes the bank.
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

/// A key typed as the stalled frame lands keeps its own echo: thirty keys into
/// the stall, the 31st pressed between the merged frame's arrival and its tick.
/// The batch spends only the thirty older presses, dated at the oldest, and the
/// fresh stamp stays for its own echo a frame later.
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
    // The merged frame, 10 ms after the 31st key.
    glow.tick(
        Some((27, 35)),
        k31 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(dark_in(&glow, 27, 5..35).is_empty(), "the batch is laid");
    // The 31st key's own echo, one frame later.
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

/// A stalled batch drained across two frames keeps its tail: thirty keys, the
/// first frame echoes twenty-nine, the second the last alone, licensed by the
/// one-press arm and lit at once.
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

/// A hidden-caret TUI's batch after a stall is laid from the print anchor and
/// does not brand the row: the input row proven by one anchored echo, twelve
/// keys into a stall, the row's end advancing twelve cells 3 s later. Licensed
/// on the anchored lane, and a later hidden echo on the row is still licensed.
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
    // k1: a licensed anchored echo establishes row 11 as the echo row.
    glow.note_typed(at(100));
    glow.observe_print_anchor(Some((11, 3, 2)));
    glow.tick(None, at(105), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the per-key hidden echo is licensed");
    // Twelve keys into a stall; the row drains three seconds after the last.
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
    // A later per-key hidden echo on the same row is still licensed: the
    // batch did not brand the row a program row.
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

/// A program row advancing while presses are in flight is not the echo: the
/// input row proven by k1's anchored echo, k2 and k3 unpaid, and the status
/// row's end advances 555 ms later (outside the spoken-for hold, inside the
/// pool). Only the established echo row may spend unpaid presses, so the
/// status advance spends nothing and k2/k3's echo is then licensed in full.
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
    // The status row's first advance, brandless, 555 ms after the input
    // echo, with two presses in flight.
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
    // The input row's late batch: k2 and k3 echo together.
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

/// An insert delivered into a stall lays the keys behind it once: four keys
/// then a paste, echoed as one hop. The insert arm pays the surplus from the
/// in-flight credits (still live at 3.5 s) and lays one sweep under
/// `licence=insert`; the typed path does not run for the span.
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
