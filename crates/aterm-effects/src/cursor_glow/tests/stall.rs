// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stall: keys typed into a stalled event loop.

use super::*;

/// THE SCREENSHOT: thirty keys at 85 ms into a stall, the row echoing
/// 400 ms after the last key as ONE 30-cell hop. Licensed as ONE sweep,
/// every cell lit, no `no-credits`, nothing for the ledger to bridge.
///
/// RED on 81dea89c8: `declined no-credits origin=27,5 target=27,35` —
/// the presses older than the 2 s credit life were invisible at the
/// echo (22 of 30 credits, 88 < 90), thirty dark cells.
#[test]
fn thirty_keys_typed_into_a_stall_are_laid_as_one_sweep_when_the_row_echoes() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let licensed_before = glow.admission_tally().licensed;
    let last = stalled_keys(&mut glow, 30, pre);
    let echo = last + Duration::from_millis(400);
    glow.tick(Some((27, 35)), echo, &c, g, &mut out);
    let dark = dark_in(&glow, 27, 5..35);
    assert!(
        dark.is_empty(),
        "thirty keys typed into a 2.9 s stall left {dark:?} dark; lit: {:?}",
        v2_cols(&glow, 27)
    );
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.declined, tally.last_decline_reason),
        (0, None),
        "the batch is typing observed late, not a refused hop"
    );
    assert_eq!(
        tally.licensed,
        licensed_before + 1,
        "one merged frame, one licensed sweep"
    );
    assert_eq!(
        bridged(&glow),
        0,
        "the host laid it; the ledger bridged nothing"
    );
}

/// Fifty keys into a five-second stall: over the old 32-cell cap and
/// far past the old 2 s life. One sweep, 5..55, nothing refused.
///
/// RED on 81dea89c8: the hop is over `RAINBOW_TYPED_SWEEP_MAX` (32) and
/// falls to a re-anchor — the landing alone is lit.
#[test]
fn a_fifty_key_batch_after_a_five_second_stall_is_one_sweep() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let last = stalled_keys(&mut glow, 50, pre);
    // 50 × 85 ms = 4.25 s of typing, the echo 1 s after the last key: the
    // oldest press is 5.3 s old at the echo.
    let echo = last + Duration::from_millis(1000);
    glow.tick(Some((27, 55)), echo, &c, g, &mut out);
    let dark = dark_in(&glow, 27, 5..55);
    assert!(dark.is_empty(), "the 50-key batch left {dark:?} dark");
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.declined, tally.last_decline_reason),
        (0, None),
        "a 50-cell echo backed by 50 presses is typing"
    );
}

/// A batch whose LAST key is three seconds old when the row echoes (the
/// matrix's hold-N12-D3): licensed, and LIT in the echoing frame and the
/// frame after — the sweep's cells are born no earlier than one stamp
/// window before the echo, so they are not born three seconds into a
/// cohort that has already faded (the matrix's `held_lit_late`).
///
/// RED on 81dea89c8 twice over: `no-fresh-hint` at the gate; and with
/// the gate alone fixed, cells born at the oldest press's clock into a
/// cohort idle 3 s are retired on the very tick that laid them.
#[test]
fn a_batch_whose_last_key_is_three_seconds_old_is_lit_in_the_echoing_frame() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let last = stalled_keys(&mut glow, 12, pre);
    let echo = last + Duration::from_millis(3000);
    glow.tick(Some((27, 17)), echo, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.declined, tally.last_decline_reason),
        (0, None),
        "twelve unpaid presses license the echo however stale the stamps"
    );
    let dark = dark_in(&glow, 27, 5..17);
    assert!(
        dark.is_empty(),
        "the echoing frame must show the batch lit; dark: {dark:?}"
    );
    // No key, one more frame: still lit — the batch joined a refreshed
    // cohort, not a three-second-old one.
    glow.tick(
        Some((27, 17)),
        echo + Duration::from_millis(100),
        &c,
        g,
        &mut out,
    );
    let dark = dark_in(&glow, 27, 5..17);
    assert!(
        dark.is_empty(),
        "100 ms after the echo the batch must still be lit; dark: {dark:?}"
    );
}

/// ONE key stalled three seconds (S9, the stalled LAST key followed by a
/// pause): its own +1 echo is licensed by the press in flight
/// (`licence=inflight`), its cell is lit on the echoing frame and one
/// beat later, and the press is spent — so the next key, echoed on
/// time, has nothing left to bridge.
///
/// RED on c45b13300: `no-fresh-hint` at the seam (the in-flight arm
/// needed two presses), the cell a dark notch for the whole pause and
/// relit only by the next key's ledger bridge.
#[test]
fn one_key_stalled_three_seconds_lights_its_own_cell_on_the_late_echo() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let declined_before = glow.admission_tally().declined;
    let k = pre + Duration::from_millis(85);
    glow.note_typed(k);
    let echo = k + Duration::from_millis(3000);
    glow.tick(Some((27, 6)), echo, &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().declined,
        declined_before,
        "one stalled press licenses its own +1: {:?}",
        glow.admission_tally().last_decline_reason
    );
    assert_eq!(
        glow.in_flight_tally().licensed,
        1,
        "…by the in-flight pool alone"
    );
    let last = glow
        .admission_log()
        .last()
        .map(|r| r.line(echo))
        .expect("a ring row");
    assert!(last.ends_with(" licence=inflight"), "{last}");
    assert!(
        v2_cols(&glow, 27).contains(&5),
        "its cell is lit in the echoing frame: {:?}",
        v2_cols(&glow, 27)
    );
    glow.tick(
        Some((27, 6)),
        echo + Duration::from_millis(100),
        &c,
        g,
        &mut out,
    );
    assert!(
        v2_cols(&glow, 27).contains(&5),
        "…and one beat later (born no earlier than the birth floor)"
    );
    assert_eq!(
        glow.typed_credits_within(echo),
        0,
        "the press is spent by its own cell"
    );
    // The next key, echoed on time: its own cell, nothing to bridge.
    let k2 = echo + Duration::from_millis(200);
    glow.note_typed(k2);
    glow.tick(
        Some((27, 7)),
        k2 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    let lit = v2_cols(&glow, 27);
    assert!(
        lit.contains(&5) && lit.contains(&6),
        "both keys' cells are lit: {lit:?}"
    );
    assert_eq!(
        bridged(&glow),
        0,
        "nothing was left for the ledger to bridge"
    );
}

/// THE ANTI-STRAY BOUND of the one-press echo: a press the app swallowed
/// (a modal's key, vim's `x`) followed by a program's +1 on the same row
/// buys AT MOST its own one cell, once — the credit is spent on
/// admission, so the next keyless +1 finds an empty pool and is refused;
/// and a press an unexplained edge forgot buys nothing at all.
#[test]
fn a_swallowed_press_buys_at_most_its_own_one_cell_and_only_once() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 4)), t0, &c, g, &mut out);
    glow.note_typed(t0 + Duration::from_millis(10));
    let first = t0 + Duration::from_millis(510);
    glow.tick(Some((3, 5)), first, &c, g, &mut out);
    assert_eq!(
        glow.admission_tally().licensed,
        1,
        "one swallowed press buys the caret's own +1 (the admitted, bounded stray)"
    );
    assert_eq!(glow.in_flight_tally().licensed, 1);
    assert_eq!(glow.typed_credits_within(first), 0, "…and is spent");
    let second = first + Duration::from_millis(200);
    glow.tick(Some((3, 6)), second, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined, tally.last_decline_reason),
        (1, 1, Some(CursorGlow::DECLINE_NO_FRESH_HINT)),
        "a second +1 finds an empty pool"
    );
    assert!(
        !v2_cols(&glow, 3).contains(&5),
        "…and its cell stays dark: {:?}",
        v2_cols(&glow, 3)
    );

    // THE FORGET CONTROL: press, keyless backward hop, then +1 — refused.
    let mut forgot = CursorGlow::default();
    forgot.tick(Some((3, 8)), t0, &c, g, &mut out);
    forgot.note_typed(t0 + Duration::from_millis(10));
    forgot.tick(
        Some((3, 2)),
        t0 + Duration::from_millis(400),
        &c,
        g,
        &mut out,
    );
    forgot.tick(
        Some((3, 3)),
        t0 + Duration::from_millis(800),
        &c,
        g,
        &mut out,
    );
    let tally = forgot.admission_tally();
    assert_eq!(
        (tally.licensed, tally.last_decline_reason),
        (0, Some(CursorGlow::DECLINE_NO_FRESH_HINT)),
        "a forgotten press licenses nothing"
    );
    assert!(v2_cols(&forgot, 3).is_empty(), "{:?}", v2_cols(&forgot, 3));
}

/// ONE stalled press against a hop WIDER than one cell is still refused
/// (the multi-cell floor: vim's `w` on a stale stamp) — and the credit
/// is KEPT, not forgotten: it is the delivered insert receipt's and the
/// next key's ledger bridge.
#[test]
fn a_single_stalled_press_wider_than_one_cell_is_still_refused_and_kept() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 4)), t0, &c, g, &mut out);
    glow.note_typed(t0 + Duration::from_millis(10));
    let hop = t0 + Duration::from_millis(410);
    glow.tick(Some((3, 6)), hop, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.last_decline_reason),
        (0, Some(CursorGlow::DECLINE_NO_FRESH_HINT)),
        "one press cannot buy two cells"
    );
    assert_eq!(
        glow.typed_credits_within(hop),
        1,
        "the credit is kept for the receipt / the next key's bridge"
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0, "…not forgotten");
    assert!(v2_cols(&glow, 3).is_empty());
}

/// A HIDDEN-CARET TUI's single stalled key (Claude Code's bracket) is
/// admitted by the same one-press rule on the anchored lane: the
/// established echo row's end advancing by one with one press in
/// flight and no stamp fresh is that press's own echo. A row no licensed
/// anchored echo has established stays silent, as today.
#[test]
fn a_hidden_caret_single_key_stall_relights_on_its_own_anchored_echo() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((5, 2)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((5, 2, 1)));
    glow.tick(None, t0 + Duration::from_millis(10), &c, g, &mut out);
    // One licensed anchored echo establishes the row.
    let k1 = t0 + Duration::from_millis(100);
    glow.note_typed(k1);
    glow.observe_print_anchor(Some((5, 3, 2)));
    glow.tick(None, k1 + Duration::from_millis(10), &c, g, &mut out);
    assert_eq!(glow.admission_tally().licensed, 1, "the row is established");
    // The stalled key: pressed, its echo 400 ms later, no stamp fresh.
    let k2 = k1 + Duration::from_millis(200);
    glow.note_typed(k2);
    let echo = k2 + Duration::from_millis(400);
    glow.observe_print_anchor(Some((5, 4, 3)));
    glow.tick(None, echo, &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        (tally.licensed, tally.declined),
        (2, 0),
        "the single stalled key's anchored echo is licensed: {:?}",
        tally.last_decline_reason
    );
    assert_eq!(glow.in_flight_tally().licensed, 1);
    assert!(
        v2_cols(&glow, 5).contains(&3),
        "its cell is lit: {:?}",
        v2_cols(&glow, 5)
    );

    // CONTROL: a row no licensed anchored echo has established is silent.
    let mut cold = CursorGlow::default();
    cold.tick(Some((5, 2)), t0, &c, g, &mut out);
    cold.observe_print_anchor(Some((7, 10, 1)));
    cold.tick(None, t0 + Duration::from_millis(10), &c, g, &mut out);
    cold.note_typed(t0 + Duration::from_millis(100));
    cold.observe_print_anchor(Some((7, 11, 2)));
    cold.tick(None, t0 + Duration::from_millis(500), &c, g, &mut out);
    assert_eq!(cold.spawns(), 0, "an unestablished row spends no press");
    assert!(v2_cols(&cold, 7).is_empty());
}
