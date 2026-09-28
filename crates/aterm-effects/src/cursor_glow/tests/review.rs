// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The review's findings on the stall, fold and park.

use super::*;

// =======================================================================
// THE REVIEW: four findings on the tree above, each with
// the host test.
// =======================================================================

/// H1 — A TYPED RE-ANCHOR SPENDS THE CREDIT OF THE STAMP IT CONSUMES.
/// The box-growth wrap lights its landing on the key's stamp and, before
/// this, spent nothing: with the one-press arm the leftover credit
/// licensed the next KEYLESS +1 on the row inside the patience as
/// `inflight` — light no keystroke asked for. Now the re-anchor spends
/// one credit with the stamp, so the keyless +1 that follows finds an
/// empty pool: refused, nothing laid, the mirror where the wrap left it.
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
    // THE RE-ANCHOR'S OWN LANDING is laid by the seam's one-cell sweep
    // — "lays exactly ONE cell, the landing", made true where the park
    // had deferred the mirror the key's own
    // `Typed` replay folds back from.
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
    // "Lays nothing" is read AT THE LANDING the keyless +1 would have
    // lit, not as a cell census: since the re-anchor lays its own
    // landing (above) the pool also carries that cell's exit-swoosh
    // reach, which grows BACKWARD from it on a later frame and is not
    // this move's light.
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

/// M1 — A HELD PARK SURVIVES A HIDDEN-CARET BOUNDARY IN THE GAP BEFORE
/// ITS RETURN. Ink brackets the park and the rewrite in DECTCEM hides,
/// and a frame can sample the hidden caret between them; the completed
/// boundary flushed the park (S6b through the hide: judged as the
/// re-anchor, the return refused, the credit stranded). The boundary now
/// spares a fresh park like the typed bank: the return is one +1 from
/// the origin, no cell dark, nothing flushed. Control: a park past its
/// window is judged before the boundary completes, as at any tick.
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

    // CONTROL: a park past its window does not survive the hide.
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

/// M2 — THE COALESCED FOLD ON THE POOL. Three keys typed into a stall at
/// the row's end, the drain crossing the wrap WIDER than the tight fold:
/// `(11,99) → (12,2)`. Before: refused `no-fresh-hint` (the tight arm
/// stops at column 1; the coalesced arm wanted a typing rhythm the stall
/// aged out) AND forgotten as a cross-row refusal — every cell of the
/// drain dark on both rows. Now: licensed by the pool alone with EXACT
/// payment, the last column and the new row's first two cells lit, the
/// presses spent, nothing forgotten, the mirror on the new row.
/// Controls: a pool that OVERPAYS the shape (four presses for three
/// cells) does not describe the hop — refused and forgotten; an empty
/// pool is program output.
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

    // CONTROL: four presses for three cells — the pool overpays the
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

    // CONTROL: no press in flight — program output.
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

/// P1 — THE SPINNER CROSSING INSIDE A HIDDEN PARK GAP. Ink parks the
/// caret at col 2 under a
/// DECTCEM hide it leaves on across the ~40 ms gap, then rewrites the
/// row and shows the caret one cell on — so every echo is judged on the
/// VISIBLE lane (a hide-bridged `+1`), and `last_anchor_sweep`, which
/// only a licensed ANCHORED echo writes, is never set. Inside key 7's
/// gap the status row's counter crosses a digit width `(9)` → `(10)`:
/// row 9's FIRST advance, brandless, the key's stamp fresh, the caret
/// hidden. The anchored lane's spoken-for hold read `last_anchor_sweep`
/// alone, so the crossing was licensed `key`, the stamp AND the credit
/// spent on it, and key 7's real echo `11,8 → 11,9` was refused
/// `no-fresh-hint` — col 8 exact ground for good (the installed build
/// refused the crossing `program-row` only because its timing branded
/// row 9 first). The hold now reads the last LICENSED row on EITHER
/// lane (`last_licensed_row`): while a different row's licensed echo is
/// younger than the stamp window, no other row may spend a stamp on the
/// anchored lane. A program row never spends a typed stamp.
///
/// RED-PROOF (before the `last_licensed_row` conjunct): the
/// zero-spinner-spends assert fails with one `licensed key` row at
/// `(9,19) → (9,20)`, and key 7's echo row reads `declined
/// no-fresh-hint`.
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
    // Six keys at 120 ms, each echoed Ink's way: bracket A parks the
    // caret hidden (nothing printed; the spinner repaints at its
    // constant width meanwhile), bracket B rewrites the row and shows
    // the caret one cell on — the visible lane bridges the hide.
    for i in 0..6u16 {
        t += ms(120);
        // The frame before the key sees the caret where the last
        // rewrite left it (frames run at 50 Hz under the fake).
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

/// P2 — THE COALESCED FOLD ON THE ALT SCREEN. A flat `!ctx_alt` on the
/// coalesced arm meant that on the alt screen — where Claude Code and
/// every measured shape run — a stalled
/// batch draining across the wrap wider than the tight fold was licensed
/// as a typed re-anchor laying only the landing (a 350 ms stall, the
/// credits stranded) or refused and forgotten (1 s). The arm now takes
/// the alt screen's own discriminator, exactly as `rainbow_coalesce`
/// does — `(!ctx_alt || blink_fresh)`: the drain's own repaint bracket
/// (a DECTCEM hide inside DEC 2026) arms the blink in the frame that
/// judges it, and a blinkless cross-row hop on the alt screen stays the
/// relocation it always was (vim's `w` across the wrap keeps its
/// owner-mandated drama).
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

    // CONTROL: the alt screen with NO blink and no rhythm — the shape is
    // not the fold there, so the cross-row hop is refused
    // `no-fresh-hint` and, as every cross-row refusal, the pool is
    // forgotten: the law for a blinkless relocation on the alt screen.
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

/// L1 — A KEYED MOTION PAIR IS NOT A RETURN. vim's `b` (a printable key
/// the host stamps as typed; it cannot see the mode) parks the caret
/// back a word, and `$` a beat later lands two cells past the origin.
/// Judged as `origin → target` the return painted the two cells past
/// the origin, which vim never echoed. The return is funded only by the
/// presses banked at or before the park: `b`'s one press cannot pay two
/// cells, so the park flushes as `b`'s own re-anchor and `$`'s hop is
/// judged from the landing — the origin cell stays dark.
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

/// THE BOUND: a stall longer than the in-flight patience is refused
/// exactly as before — the patience is a bound, not a licence.
/// (Green today and after; it pins the number.)
#[test]
fn a_stall_longer_than_the_patience_is_refused_as_before() {
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

/// A SWALLOWED BATCH CANNOT FUND A PROGRAM HOP AFTER AN UNEXPLAINED MOVE.
/// Three keys never echoed; a keyless BACKWARD hop (the ctrl+c clear of
/// the matrix, a modal's repaint) is refused and FORGETS them; a keyless
/// forward hop of three on the same row half a second later finds an
/// empty pool.
///
/// RED on 81dea89c8: the three credits are still alive at 1.5 s and
/// license the forward hop as a coalesce.
#[test]
fn a_swallowed_batch_cannot_fund_a_program_hop_after_an_unexplained_move() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 12)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 3, t0);
    // The unexplained edge: back to col 2, keyless. Held as a park
    // candidate for one stamp window (the Ink rewrite's park-and-return),
    // then judged exactly as before on the next tick past
    // it: refused, and the presses forgotten — the forget is DEFERRED
    // past `TYPE_HINT_FRESH` (`past_park_window()`).
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

/// A PRESS THE TTY WILL NOT ECHO BANKS NOTHING — the `read -s` half of
/// the same-row swallowed-press residual. The host reads
/// canonical no-echo off the pty at the key and calls
/// [`CursorGlow::note_typed_swallowed_no_echo`] instead of the typed
/// hint. The shape with NO edge: three presses, then a same-row forward
/// program hop of exactly three cells one second later (`read -s`'s
/// caller printing on the prompt row) — refused, dark, and the tally
/// says why. The positive control is the same three presses BANKED (a
/// tty whose mode the host could not read, or a raw-mode program): the
/// hop is licensed by the in-flight pool — the residual as admitted,
/// which is what makes the withheld half non-vacuous.
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

    // POSITIVE CONTROL: the same presses banked license the same hop.
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

/// A RETURN-LICENSED MOVE AND A ROW CHANGE FORGET THE IN-FLIGHT CREDITS.
/// Five keys never echoed (a password), Return, the prompt's move to the
/// next row licensed by the Return: the five presses are gone, and a
/// keyless forward hop on the new row is refused.
///
/// RED on 81dea89c8: the five credits fund the hop.
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

/// A GLYPH'S ECHO DOES NOT SPEND THE ENTER PRESSED BEHIND IT: a
/// same-row FORWARD hop with a typed pair is a glyph's echo and never
/// the Return's move, so it neither takes the Return (nor the composer
/// newline) nor forgets; the Return stays for its own row change and is
/// spent there — §17.3's law, "the in-flight echoes that precede a
/// Return's own move are still licensed by their credits", by
/// construction. Shape: `gi⏎` typed inside a quarter second with `g`'s
/// echo still in flight (Claude Code's own measured input p99 is
/// 268 ms; an SSH round trip makes it ordinary) — a dark cell for the
/// second letter of every short reply followed by Enter.
///
/// RED before the fix: frame 1 `forgotten=1` (the `+1` taken
/// class-blind as the Return's, the forget edge dropping `i`'s credit),
/// frame 2 `("no-fresh-hint","none",(3,3),(3,4))` with cell 3 dark.
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

    // CONTROL — a same-row forward hop with NO typed pair is the
    // Return's (a prompt that answers on the row): taken, consumed once.
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

/// A NAVIGATION LICENCE AND A KILL FORGET THE IN-FLIGHT PRESSES. The
/// arrow's own licensed move forgets at the MOVE (an arrow pressed into
/// the stall is judged by its hop's shape, so a batch that still echoes
/// forward keeps its light); a kill forgets at the KEY (the line's
/// content is gone).
///
/// RED on 81dea89c8: four credits fund the keyless hop in both halves.
#[test]
fn a_navigation_licence_and_a_kill_forget_the_in_flight_presses() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    // The arrow: four keys in flight, Home, the caret's licensed jump to
    // col 2, then a keyless forward hop of four.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 9)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 4, t0);
    let nav = last + Duration::from_millis(100);
    glow.note_navigation(nav);
    glow.tick(
        Some((3, 2)),
        nav + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.admission_tally().licensed,
        1,
        "the arrow's move is licensed"
    );
    glow.tick(
        Some((3, 6)),
        nav + Duration::from_millis(500),
        &c,
        g,
        &mut out,
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.last_decline_reason),
        (1, Some(CursorGlow::DECLINE_NO_FRESH_HINT)),
        "after the arrow's move the pool is empty"
    );
    // The kill: four keys in flight, ^U at the key, then a keyless
    // forward hop of four half a second later.
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

/// A SCROLL — OR A ROW-BAND MOVE — KEEPS THE POOL. Three keys in flight
/// on row 11, the grid scrolls one row (the box grew before the batch
/// echoed), the caret is seen on row 10 at the same column, and the batch
/// echoes there two seconds later as a +3: the presses were never
/// forgotten (`forgotten == 0`, `credits == 3` after the scroll) and the
/// echo is licensed by the pool alone (`licence=inflight`).
/// `translate_scroll_state` moves the anchors and never touches the ring;
/// the engine's own ledger clears on a scroll, and this pins that the
/// host pool does not. The band row restates it for `note_band_move`:
/// rows `0..=20` move down one, the caret is seen on row 12, the batch
/// echoes there as a +3 on the pool.
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

/// A FORGET EDGE THAT FINDS ONLY DEAD PRESSES COUNTS NOTHING. One press,
/// then the whole in-flight patience of silent ticks on a VISIBLE caret
/// (no hidden boundary, so nothing physically retires the slot), then a
/// kill: every life-filtered reader already reported the pool empty
/// (`credits=0`), so `inflight_forgotten=` must not rise — the tally's
/// doc and the `trail` help both say it counts edges that dropped a
/// non-empty pool.
///
/// RED before the fix: `forget_typed_credits` judged "non-empty" by the
/// slots alone and ticked the tally for a press the clock had retired.
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
    // …and the same edge with a LIVE press behind it still counts.
    let k = t + Duration::from_millis(100);
    glow.note_typed(k);
    glow.note_kill(k + Duration::from_millis(50), true);
    assert_eq!(glow.in_flight_tally().forgotten, 1);
}

/// A BACKSPACE RETIRES ONLY THE NEWEST IN-FLIGHT PRESS. `abc⌫d` typed
/// into a stall echoes as three cells against the credits of `a`, `b`
/// and `d` — 2.5 s after the first key.
///
/// RED on 81dea89c8: the 2 s life refuses the echo (`no-fresh-hint`),
/// and the ring holds three credits after the Backspace, not two.
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

/// A BACKSPACE'S ECHO IS A RETREAT; IT CAN NEVER EXPLAIN A FORWARD HOP.
/// `abc` typed into a stall, `⌫` (the pool
/// holds `a` and `b`), and the app catches up 60 ms later echoing the
/// two survivors as one `+2` — inside the Backspace's quench window.
/// The batch is the two presses' echo exactly as it would be 260 ms
/// later: coalesced, both credits spent, both cells lit, and a keyless
/// `+1` on the row afterwards finds an empty pool and is refused.
///
/// RED before the fix: `bs_pair` vetoed every same-row forward typed
/// arm (`rainbow_coalesce`, `credit_starved`, `lays_typed_cells`), so
/// the hop was licensed `inflight` yet laid nothing, spent nothing and
/// swept nothing — cell 5 dark, two phantom credits banked for the
/// whole patience, and the keyless `+1` lit cell 7 as `inflight`.
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
    // A keyless program `+1` INSIDE the quench window: the drain
    // consumed the Backspace's one-shot — the
    // retreat it was armed for is in the drained row — so no press
    // and no hint license it. Before that the hint was left standing
    // for a forward hop, and licensed every keyless `+1` on the row
    // until it expired (a spinner advancing one cell at a time lit
    // every step). Then one once the window has closed: no press in
    // flight, refused.
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

/// The same drain three cells wide (`abcde⌫`, four survivors) is a
/// coalesce, not a Backspace re-anchor: no `FoldReverse` cat pulse is
/// minted for a forward hop, every survivor's cell is lit and the pool
/// is spent.
///
/// RED before the fix: with the typed arms vetoed the +4 fell to the
/// `deletion` re-anchor, which minted the FoldReverse pulse for a move
/// that went forward.
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

/// The non-stall twin: `ab⌫c` inside one frame gap, echoed as one `+2`.
/// The two cells the survivors occupy are lit and nothing is left in
/// the pool.
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

/// Pre-roll `2..5` visible with the print anchor fed, then `n_hidden`
/// keys echoed through the anchored lane under a HIDDEN caret (Claude
/// Code's per-keystroke bracket). Returns the next anchor sequence and
/// the row's end column.
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

/// THE REAPPEARANCE OVER AN ANCHORED RUN KEEPS THE PRESSES IN FLIGHT.
/// While the caret is hidden across
/// per-keystroke brackets, the anchored lane licenses each echo and
/// moves the mirror, but `last_visible` stayed at the pre-hide cell —
/// so when the caret was shown where the anchor already put it, with
/// two presses in flight, the hide-bridge's in-flight arm re-judged the
/// WHOLE run `last_visible -> cur` as one hop: the share rule refused
/// it (`no-credits`), one press was spent on the landing, the other
/// forgotten, and the two keys' real echo was refused `no-fresh-hint`
/// — permanent dark cells, and the ledger could not bridge them either.
/// A licensed anchored echo under a hidden caret now moves the
/// hide-bridge source to its landing (where the mirror already sits),
/// so the reappearance is judged as the hop from THAT cell — a
/// same-cell reappearance keeps the pool and the fresh stamps, and the
/// stalled keys echo licensed.
///
/// RED before the fix: `forgotten=1 dark=[8, 9]`, ring after the show
/// `("no-credits","none",(3,5),(3,8))`, then `("no-fresh-hint","none",
/// (3,8),(3,10))`. Four shapes: two presses in flight at the show, one,
/// none (a new key after the show), and the show landing with the
/// stalled echo already in.
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

/// The S8e pre-roll on `row` of a 100-column grid: `k1` echoes
/// `97 -> 98`, `k2` `98 -> 99` (visible, the anchor fed), and `k3` —
/// the glyph at the pane's LAST column — prints with no caret move (the
/// xterm pending wrap: the anchor one past the caret parked at 99),
/// licensed by the anchored lane's `wrap_parked` arm. Returns the clock
/// after `k3`'s echo with every press paid.
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

/// A FOLD AFTER A PAID PENDING-WRAP GLYPH DOES NOT CHARGE THE LAST
/// COLUMN TWICE. `fold_shape` priced
/// `99 -> (r+1, 1)` at two cells — the origin row's tail plus the
/// landing head — which was right when the last-column print produced
/// no observed move and nothing else paid for it. Since the anchored
/// lane's `wrap_parked` arm licenses that print and spends
/// its credit, the fold that follows asked the pool for two (tight) or
/// exactly the sum (coalesced) when only the new row's glyph was
/// unpaid: a stalled key after the last-column glyph was refused
/// `no-fresh-hint` and forgotten, its cell on the new row dark; two
/// stalled keys had the fold spend both and the second's `+1` starve.
/// The row whose edge glyph the anchored lane paid is remembered
/// (`wrap_paid_row`, consumed by the next observed caret move, riding
/// its row across a scroll or band move), and the fold from that
/// column prices the landing head alone.
///
/// RED before the fix: `row 12 lit: {}; declined=1 reason=Some("no-
/// fresh-hint") forgotten=1`. Shapes: one stalled key; two (the fold
/// then a `+1`); the grid's bottom row (the wrap scrolls before the
/// fold is observed, so the witness must ride its row rather than sit
/// in the anchor memory the scroll wipes). Controls that must keep
/// paying two: the last-column glyph arriving WITH the fold, and the
/// pending-wrap glyph arriving in the same tick as a caret move (the
/// visible lane owns that tick, nothing paid the edge cell).
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
    // CONTROL: the last-column glyph arrives WITH the fold (k3 and k4
    // stall together): two cells from two presses, as before.
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
    // CONTROL: the pending-wrap glyph arrives in the SAME tick as a
    // caret move (k2's `98 -> 99` with the anchor already at the edge):
    // the visible lane owns that tick and nothing paid cell 99, so the
    // fold still lays both cells from two presses.
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

/// THE PENDING-WRAP PRINT UNDER A HIDDEN CARET (shape a). The
/// hide-bridge relocation moves the hide-bridge source
/// to the anchored echo's landing `(ar, ac)` — and at the pane's edge
/// `ac == pane_col1` is OFF-GRID (the print anchor counts the deferred
/// wrap) while the DEC caret is shown parked at `pane_col1 - 1`. The
/// show frame then bridged `(11,100) -> (11,99)`: a same-row BACKWARD
/// hop, refused `no-fresh-hint` with the pool forgotten, and the tick's
/// `cur != last_visible` consumption cleared `wrap_paid_row`, so the
/// fold `99 -> (12,1)` that followed priced two cells against one
/// press — refused, forgotten, the new row's head dark. The source is
/// clamped to the last on-grid column, so the show is a same-cell
/// completed boundary and the paid edge cell's witness survives it.
///
/// RED before the fix: `declined=1` (`no-fresh-hint (11,100)->(11,99)`)
/// at the show, `wrap_paid_row=None`, then the fold refused and
/// `forgotten=1`, row 12 empty.
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
    // The glyph at the last column printed under a HIDDEN caret (the
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

/// MID-LINE TYPING UNDER A HIDDEN CARET (shape b). Ink re-lays the
/// row's tail on every key, so the print
/// anchor lands at the row's END while the caret is shown mid-row, one
/// past the glyph typed. Relocated to the anchor, the hide-bridge
/// source sat at the row's end and the show frame bridged back to the
/// caret: a same-row BACKWARD hop — held as a park when a press was in
/// flight (released on the anchored lane by the next echo, which then
/// relocated the source again), refused `no-fresh-hint` with the pool
/// forgotten when the stamp was fresh and nothing was in flight, and
/// past the bridge's reach a DECLINED hidden relocation whose silent
/// wipe dropped the pool uncounted. (Before the relocation the show
/// was a refused FORWARD `+1`, the pool kept.) A caret last shown LEFT
/// of the row's remembered end now advances by the echo's width, so
/// every show is a same-cell boundary: nothing refused, held, spent or
/// forgotten, and the stalled keys' echoes through the hidden bracket
/// are the pool's to pay as ever.
///
/// RED before the fix: the first show held a park `(5,11)->(5,6)` on
/// the second key's credit (`park_returns=1` once its echo released
/// it), and the last show wiped that key's successor from the pool
/// (`credits=0` with the third key unechoed).
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
    // The second key stalls with a third; they echo together (+2) under
    // the hide, the caret shown at 8; then a fourth key's own +1, shown
    // at 9. No park, no refusal, nothing forgotten.
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

    // THE ESTIMATE'S WITNESS: a caret shown exactly where it was last
    // observed, left of the relocated source, is no move at all — the
    // relocation was a guess and the caret never left. Not a park, not
    // a refusal, not the declined relocation's silent wipe; the pool
    // kept: an advance at the row's end inside the stamp window that
    // was not the caret's must not turn the caret's own reappearance
    // into a forget.
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

/// A BACKSPACE INTO A COMMITTED IME RUN RETIRES ONE GLYPH'S CREDIT, NOT
/// THE RUN'S. The host banks an IME commit as ONE press priced at its
/// summed width (three CJK glyphs: one slot, six cells), and a Backspace
/// erases one glyph of it. The host prices that glyph (two cells,
/// [`CursorGlow::note_backspace_erasing`]) and the slot keeps the other
/// four, so when the stalled prompt catches up and echoes the surviving
/// two glyphs as a +4, the pool pays for it. An erase the host could
/// not price (`None`) still retires the whole newest press — a plain
/// key laid one glyph, however wide. A PRICED ZERO (`Some(0)`, a
/// zero-width cluster inside the run) retires nothing: not the run's
/// slot, and not the press before the run once the run is paid out.
///
/// RED before the fix: `retire_newest` dropped the whole slot at the
/// Backspace (0 credits) and the +4 was refused `no-fresh-hint` — four
/// dark cells for two glyphs the hand never erased. RED again before
/// the priced zero: `0` was the unpriced value, so the
/// zero-width erase below took the key before the run (left 0,
/// right 1).
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
    // An UNPRICED erase is still the whole-slot retire.
    glow.note_typed_cells(echo + ms(85), 6);
    glow.note_backspace_erasing(echo + ms(170), None);
    assert_eq!(glow.typed_credits_within(echo + ms(170)), 0);
    // …and a priced erase wider than what the newest press still holds
    // takes the slot and no more: the glyph belonged to that press.
    glow.note_typed_cells(echo + ms(255), 1);
    glow.note_typed_cells(echo + ms(340), 1);
    glow.note_backspace_erasing(echo + ms(425), Some(2));
    assert_eq!(glow.typed_credits_within(echo + ms(425)), 1);
    // A PRICED ZERO retires nothing. A plain key (`x`, one cell) and
    // then a run whose first cluster is a base-less combining mark
    // (`\u{0301}日本`: widths 0, 2, 2 — one press, four cells): erasing
    // `本` and `日` pays the run out, and erasing the mark gives back
    // its zero WITHOUT taking `x`, the newest press left — that key
    // was never erased and its echo is still the pool's to pay. Only
    // an unpriced erase takes it. (The pool is emptied first: the
    // older of the two one-cell presses above is still banked.)
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

/// AN UNPAID CREDIT SURVIVES A HIDDEN BOUNDARY FOR THE WHOLE PATIENCE.
/// Eight keys in flight, a repaint bracket's hidden→visible boundary at
/// the same cell three seconds in, the echo 100 ms later: licensed.
///
/// RED on 81dea89c8: `retire_hidden_movement_provenance` retired the
/// credits at the 2 s life.
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

/// A HIDDEN CARET REAPPEARING AT THE END OF A STALLED BATCH IS THE
/// BATCH'S ECHO. A frame catches the merged repaint's bracket hidden;
/// the next sees the caret thirty cells on. The hide bridge's reach is
/// two cells (eight under a fresh stamp), so the reappearance was a
/// DECLINED relocation that wiped the whole bank. Under Rainbow Kitty a
/// same-row forward reappearance the in-flight credits pay for is the
/// echo shape, and is judged as one.
///
/// RED on 81dea89c8: `retire_all_movement_provenance`, nothing spawned.
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

/// A KEY TYPED AS THE STALLED FRAME LANDS KEEPS ITS OWN ECHO. Thirty keys
/// into the stall; the 31st is pressed between the merged frame's
/// arrival and the tick that judges it, so its stamp is the one fresh
/// stamp beside thirty stale ones. The batch is the thirty older
/// presses' — dated at the oldest of them, spending only them — and the
/// fresh stamp stays in the bank for its own key's echo, which lands a
/// frame later and is licensed per key.
///
/// RED on 81dea89c8: the batch pops the fresh stamp, dates its sweep at
/// that key, the ledger forfeits the key's press, and the key's own
/// echo is refused `no-fresh-hint` — one dark cell after the recovery,
/// never relit.
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

/// A STALLED BATCH DRAINED ACROSS TWO FRAMES KEEPS ITS TAIL ON THE
/// LEDGER. Thirty keys; the first frame echoes twenty-nine, the second
/// the thirtieth alone — ONE press still in flight, licensed for its own
/// +1 by the one-press arm and lit at once: the batch sweep
/// on the OLDEST press's clock let the ring SPEND the twenty-nine and
/// keep the one, and the kept one pays for its cell.
///
/// RED on 81dea89c8: the whole batch is refused at the first frame.
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

/// A HIDDEN-CARET TUI'S BATCH AFTER A STALL IS LAID FROM THE PRINT ANCHOR
/// AND DOES NOT BRAND THE ROW. fc_hidden's shape: the caret hidden across
/// frames, the input row's identity proven by one licensed anchored echo,
/// then twelve keys into a stall and the row's end advancing twelve cells
/// three seconds later. Licensed through the anchored lane, lit, and the
/// row is still the echo row afterwards (a later per-key hidden echo on
/// it is licensed, not refused `program-row`).
///
/// RED on 81dea89c8: the correlation gate needs a fresh stamp — the
/// batch is refused silently, and the advance brands the row.
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

/// A PROGRAM ROW ADVANCING WHILE PRESSES ARE IN FLIGHT IS NOT THE ECHO.
/// The refute round's fixture shape with the stall added: the input
/// row's identity proven by k1's anchored echo; k2 and k3 unpaid (the
/// app slow); the status row's end advances 555 ms after the last input
/// echo — outside the spoken-for hold, inside the in-flight pool. It is
/// NOT the echo (the unpaid presses carry no row identity; only the
/// established echo row may spend them), it spends nothing, and k2/k3's
/// real echo on the input row is then licensed in full.
///
/// RED on 81dea89c8 at the last assert (the batch is refused); the
/// status-row assert is the negative control the in-flight licence must
/// keep green.
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
    // The status row's FIRST advance, brandless, 555 ms after the input
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

/// AN INSERT DELIVERED INTO A STALL LAYS THE KEYS BEHIND IT ONCE. Four
/// keys typed into the stall, a paste delivered into it, the row echoing
/// both as ONE hop: the insert arm pays the surplus from the in-flight
/// credits (which are still there at 3.5 s), lays one sweep under
/// `licence=insert`, and the typed path never runs for the span.
///
/// RED on 81dea89c8: `insert_reach` = 10 + 0 stale credits < 14, the
/// hop is refused `no-fresh-hint`.
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
