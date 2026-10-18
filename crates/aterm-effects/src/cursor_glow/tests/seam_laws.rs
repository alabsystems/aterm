// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam's licence-class laws: one-shots spend once, a glyph supersedes
//! nothing whose echo is still owed, one kill is one `Kill`, a held park is a
//! wake source. Each test is a minimal repro of a user-visible shape (a laggy
//! link, Claude Code's composer, a stalled app).

use super::*;

/// The last ring row, in `ring_rows`' order: `(reason, licence, origin,
/// target)` — `("licensed", "key", ..)` for a lit key.
fn last_row(glow: &CursorGlow) -> Option<RingRow> {
    ring_rows(glow).last().copied()
}

// ──────── the tick: wake sources and guards ────────

/// Under `classic` the engine returns before `poof_scan`, where erase licences
/// expire, so the Classic branch drops them: a Backspace or kill leaves no
/// `is_active()` and no 40 ms deadline (else a ~25 Hz wake loop).
#[test]
fn a_classic_erase_key_leaves_no_standing_wake() {
    let g = geom();
    let c = cfg(GlowStyle::Classic, true);
    let frame = ms(16);
    for label in ["backspace", "kill"] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((2, 5)), t0, &c, g, &mut out);
        if label == "backspace" {
            glow.note_backspace_erasing(t0 + ms(100), Some(1));
        } else {
            glow.note_kill(t0 + ms(100), false);
        }
        // Well past KILL_HINT_FRESH (0.35 s), the caret never moved.
        let mut t = t0 + ms(100);
        for _ in 0..60 {
            t += ms(40);
            glow.tick(Some((2, 5)), t, &c, g, &mut out);
        }
        assert!(
            !glow.is_active(),
            "{label}: a classic engine cannot service an erase licence, so it must not keep the host awake"
        );
        assert!(
            glow.next_change_deadline(t, frame).is_none(),
            "{label}: no deadline once the licence is past its window"
        );
    }
}

/// While the display temperature (`disp_t`) climbs above the visibility threshold the ember is a
/// pending visible change, and both wake predicates say so: the host does not sleep before the
/// ember appears.
#[test]
fn the_forge_ember_keeps_the_host_awake_while_the_metal_heats() {
    let g = Geom {
        rows: 60,
        cols: 2,
        ..wide_geom()
    };
    let c = cfg(GlowStyle::Fire, true);
    let frame = ms(16);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((27, 1)), t0 + ms(8055), &c, g, &mut out);
    glow.note_band_move(27, 55, 2);
    glow.note_backspace_erasing(t0 + ms(8281), Some(2));
    let mut t = 8281u64;
    let mut slept_hot = Vec::new();
    // Non-vacuity: frames on which the metal was still heating toward a
    // `disp_t` above the threshold.
    let mut heating = 0u32;
    let mut crossed = false;
    while t < 8281 + 3000 {
        t += 16;
        let now = t0 + ms(t);
        glow.tick(Some((27, 1)), now, &c, g, &mut out);
        let due = glow.next_change_deadline(now, frame);
        let metal_heating =
            glow.cursor_temp < glow.disp_t && glow.disp_t > CursorGlow::FORGE_MIN_TEMP;
        if metal_heating {
            heating += 1;
        }
        if glow.cursor_temp > CursorGlow::FORGE_MIN_TEMP {
            crossed = true;
        }
        if due.is_none() && metal_heating {
            slept_hot.push(t - 8281);
        }
        if due.is_none() && glow.cursor_temp == 0.0 && glow.disp_t <= CursorGlow::FORGE_MIN_TEMP {
            break;
        }
    }
    assert!(
        heating > 0,
        "the strike heated the metal (frames with `cursor_temp` chasing a `disp_t` above the threshold)"
    );
    assert!(
        crossed,
        "the ember crossed the threshold upward — the change the wake exists to show"
    );
    assert!(
        slept_hot.is_empty(),
        "the host was told to sleep while the metal was still heating at +{slept_hot:?} ms"
    );
}

/// A held park schedules its own flush: `is_active` and `next_change_deadline`
/// read it, so a deadline-driven host does not sleep through the ≤ 0.25 s
/// verdict.
#[test]
fn a_held_park_is_a_wake_source() {
    let g = Geom {
        rows: 3,
        cols: 40,
        ..wide_geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let frame = ms(16);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((1, 20)), t0 + ms(1000), &c, g, &mut out);
    glow.note_typed(t0 + ms(1100));
    glow.tick(Some((1, 21)), t0 + ms(1110), &c, g, &mut out);
    // A key typed into a stall; the host ticks while the engine asks, then
    // sleeps on `None`.
    glow.note_typed(t0 + ms(2000));
    let mut t = 2010u64;
    loop {
        let now = t0 + ms(t);
        glow.tick(Some((1, 21)), now, &c, g, &mut out);
        if t >= 5000 {
            break;
        }
        match glow.next_change_deadline(now, frame) {
            Some(d) => {
                t = (t + (d.saturating_duration_since(now).as_millis() as u64).max(1)).min(5000);
            }
            None => t = 5000,
        }
    }
    // The program parks the caret left with the press still in flight.
    let park_at = t0 + ms(5010);
    glow.tick(Some((1, 5)), park_at, &c, g, &mut out);
    assert!(
        glow.held_park.is_some(),
        "the backward hop over a press in flight is parked"
    );
    assert!(glow.is_active(), "a held park is pending work");
    let due = glow
        .next_change_deadline(park_at, frame)
        .expect("a held park schedules its own flush");
    let expected = park_at + Duration::from_secs_f32(CursorGlow::TYPE_HINT_FRESH) + rk::ARM_MIN;
    assert_eq!(
        due, expected,
        "the flush is armed just past the park's window"
    );
    glow.tick(Some((1, 5)), due, &c, g, &mut out);
    assert_eq!(
        glow.in_flight_tally().park_flushed,
        1,
        "the scheduled tick flushed the park"
    );
    assert!(glow.held_park.is_none());
    // Idle-zero restored once the flush's own light (if any) has settled.
    let mut now = due;
    for _ in 0..400 {
        match glow.next_change_deadline(now, frame) {
            Some(d) => {
                now = d.max(now + rk::ARM_MIN);
                glow.tick(Some((1, 5)), now, &c, g, &mut out);
            }
            None => break,
        }
    }
    assert!(
        glow.next_change_deadline(now, frame).is_none(),
        "idle → None after the flush"
    );
    assert!(!glow.is_active(), "idle → inactive after the flush");
}

/// A foreign-row park holds an unpaid press for ten seconds, not the same-row
/// quarter second, and its deadline follows that, so an idle window does not
/// spin at `ARM_MIN` (1 ms) wakes until the park expires. The ten seconds run
/// from the press's credit, which may be nine seconds older than the foreign
/// repaint that parked it: the park wakes and retires when that credit expires.
#[test]
fn a_cross_row_park_sleeps_until_its_own_expiry() {
    let g = Geom {
        rows: 3,
        cols: 40,
        ..wide_geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let frame = ms(16);
    let t0 = Instant::now();
    for (cross_row, credit_age) in [(false, 0u64), (true, 0), (true, 9000)] {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let park_at = t0 + ms(credit_age);
        glow.tick(Some((1, 20)), park_at, &c, g, &mut out);
        glow.type_press_ring.bank(t0, 1, Some('x'));
        glow.held_park = Some(HeldPark {
            row: 1,
            landing_row: if cross_row { 2 } else { 1 },
            cross_row,
            origin: 20,
            landing: 5,
            at: park_at,
            cfg: c,
            geom: g,
            landing_glyph: false,
            landing_cell: None,
            content_followed: false,
        });
        let patience = if cross_row {
            Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S)
        } else {
            Duration::from_secs_f32(CursorGlow::TYPE_HINT_FRESH)
        };
        let expiry = t0 + patience + rk::ARM_MIN;
        assert_eq!(
            glow.next_change_deadline(park_at, frame),
            Some(expiry),
            "the park arms its credit's expiry ({cross_row}, {credit_age} ms)"
        );
        if credit_age > 0 {
            assert!(
                glow.held_park.unwrap().fresh(expiry),
                "the park itself outlives its last credit"
            );
        } else if cross_row {
            let after_same_row_window = t0 + ms(300);
            assert!(glow.held_park.unwrap().fresh(after_same_row_window));
            assert_eq!(
                glow.next_change_deadline(after_same_row_window, frame),
                Some(expiry),
                "a live cross-row park must not re-arm at 1 ms"
            );
            glow.tick(Some((1, 20)), after_same_row_window, &c, g, &mut out);
            assert!(glow.held_park.is_some(), "still in custody after 250 ms");
            assert_eq!(
                glow.next_change_deadline(after_same_row_window, frame),
                Some(expiry)
            );
        }
        glow.tick(Some((1, 20)), expiry, &c, g, &mut out);
        assert!(
            glow.held_park.is_none(),
            "park expired on its deadline ({cross_row}, {credit_age} ms)"
        );
    }
}

/// The tick's degenerate-geometry guard covers rows and columns beside
/// `cw`/`ch`, matching the v2 engage gate; on a 0-row or 0-column grid the
/// first settling star would otherwise hit `clamp` with `min > max`.
#[test]
fn a_zero_row_or_zero_column_grid_ticks_dark_without_panic() {
    let c = cfg(GlowStyle::RainbowKitty, true);
    for (rows, cols) in [(0usize, 40usize), (24, 0), (0, 0)] {
        let g = Geom {
            rows,
            cols,
            cw: 8,
            ch: 16,
            origin_x: 4,
            origin_y: 20,
            win_w: 800,
            win_h: 480,
            head: 0,
        };
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        assert_eq!(glow.tick(None, t0, &c, g, &mut out), 0);
        glow.note_typed(t0 + ms(10));
        glow.note_backspace_erasing(t0 + ms(20), Some(1));
        for i in 1..10u64 {
            assert_eq!(
                glow.tick(None, t0 + ms(20 + 16 * i), &c, g, &mut out),
                0,
                "{rows}x{cols}: a degenerate grid draws nothing"
            );
        }
        assert!(
            !glow.v2.engaged(),
            "{rows}x{cols}: v2 is not engaged on a grid with no cells"
        );
    }
}

// ──────── the one-shots spend once ────────

/// A one-shot licence is taken at the spawn seam by the one move it paired
/// with, whatever that move's shape: the Backspace quench (`typing` gates only
/// the deletion class) and the composer newline (`newline_hint`, Shift+Enter),
/// like the Return. Otherwise one key licenses every program move for 250 ms
/// (jump streaks under Lumen / Laser, a park-rewrite pair in Claude Code, a
/// Return meteor for a keyless move).
#[test]
fn a_one_shot_licenses_its_own_move_and_nothing_after() {
    type OneShotArm = fn(&mut CursorGlow, Instant);
    let backspace: OneShotArm = |glow, t| glow.note_backspace_erasing(t, Some(1));
    // The host's Shift+Enter arm: a typed stamp beside the newline hint.
    let newline: OneShotArm = |glow, t| {
        glow.note_typed(t);
        glow.note_newline_break(t);
    };
    let g = Geom {
        rows: 24,
        cols: 80,
        ..wide_geom()
    };
    let all = [GlowStyle::Lumen, GlowStyle::RainbowKitty, GlowStyle::Fire];
    // (label, the one-shot, an alt-screen composer, the move it pairs with)
    for (label, arm, composer, paired) in [
        (
            "Backspace, a two-row relocation",
            backspace,
            false,
            (8u16, 20u16),
        ),
        (
            "Shift+Enter, the composer's row change",
            newline,
            true,
            (11, 0),
        ),
    ] {
        for style in all {
            let c = cfg(style, true);
            let mut out = Vec::new();
            let mut glow = CursorGlow::default();
            if composer {
                glow.note_context(true);
            }
            let t0 = Instant::now();
            glow.tick(Some((10, 20)), t0 + ms(1000), &c, g, &mut out);
            arm(&mut glow, t0 + ms(2000));
            glow.tick(Some(paired), t0 + ms(2020), &c, g, &mut out);
            let spawns = glow.spawns();
            assert_eq!(spawns, 1, "{label}, {style:?}: the paired move is licensed");
            assert!(
                !glow.move_licensed(t0 + ms(2021)),
                "{label}, {style:?}: the one-shot is spent by the move it paired with"
            );
            let meteors = glow.v2_status().map(|s| s.meteors);
            // A keyless +1, then a keyless relocation, both inside 250 ms.
            for (to, at) in [((paired.0, paired.1 + 1), 2100), ((5, 60), 2200)] {
                glow.tick(Some(to), t0 + ms(at), &c, g, &mut out);
                assert_eq!(
                    last_row(&glow).map(|r| (r.0, r.1)),
                    Some(("no-fresh-hint", "none")),
                    "{label}, {style:?}: a keyless move to {to:?} after the spent one-shot is \
                     refused: {:?}",
                    last_row(&glow)
                );
            }
            assert_eq!(
                glow.spawns(),
                spawns,
                "{label}, {style:?}: nothing more flies"
            );
            assert!(
                glow.v2_status().map(|s| s.meteors) <= meteors,
                "{label}, {style:?}: no Return meteor for a keyless move"
            );
        }
    }
}

/// The seam's typed `Sweep` needs typed pairing, never `mv.typing` (dist ≤ 1) alone: an arrow's
/// hop keeps its wake, and a quench/kill-licensed keyless +1 (a spinner, a status redraw) lays
/// nothing. Nothing at or past the hop column joins the word's cohort, and its right bound `col1`,
/// the one a rightward hop would extend, is untouched (`col0` is the exit swoosh's, reaching
/// backward).
#[test]
fn a_one_shot_licensed_plus_one_lays_no_typed_cell() {
    let g = Geom {
        rows: 24,
        cols: 80,
        ..wide_geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    for key in ["right arrow", "backspace", "kill"] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((3, 10)), t0 + ms(1000), &c, g, &mut out);
        // One real typed echo seats the mirror and lays the word's cell.
        glow.note_typed(t0 + ms(1100));
        glow.tick(Some((3, 11)), t0 + ms(1110), &c, g, &mut out);
        let word = glow.v2_ribbon().expect("engaged").cohorts()[0];
        assert!(!word.wake);
        let mut col = 11;
        for k in 0..4u64 {
            let t = t0 + ms(2000 + k * 200);
            match key {
                "right arrow" => glow.note_navigation(t),
                "backspace" => glow.note_backspace(t),
                _ => glow.note_kill(t, true),
            }
            col += 1;
            glow.tick(Some((3, col)), t + ms(10), &c, g, &mut out);
        }
        let rib = glow.v2_ribbon().expect("engaged");
        let typed: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|c| c.row == 3 && c.typing && c.col >= 11)
            .map(|c| c.col)
            .collect();
        assert!(
            typed.is_empty(),
            "{key}: a keyless +1 laid a typing cell at {typed:?}"
        );
        let word_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == word.id)
            .expect("the word's cohort lives");
        assert_eq!(
            word_now.col1, word.col1,
            "{key}: the word's right bound — the one a forward hop would extend — is untouched"
        );
        let joined: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|c| c.cohort == word.id)
            .map(|c| c.col)
            .collect();
        assert!(
            joined.iter().all(|&col| col < 11),
            "{key}: a keyless +1 attached a cell to the word at the hop: {joined:?}"
        );
        if key == "right arrow" {
            assert_eq!(
                word_now.alive_at, word.alive_at,
                "an arrow beside the word does not restart its grace"
            );
            let hop: Vec<&rk::ribbon::Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 3 && c.col == 11)
                .collect();
            assert_eq!(hop.len(), 1, "the arrow's hop lays its wake cell");
            assert!(!hop[0].typing);
            assert!(
                rib.cohorts()
                    .iter()
                    .any(|k| k.id == hop[0].cohort && k.wake),
                "…in a wake cohort"
            );
        } else {
            assert!(
                rib.cells().iter().all(|c| c.row != 3 || c.col < 11),
                "{key}: a keyless +1 lays nothing: {:?}",
                rib.cells()
                    .iter()
                    .map(|c| (c.row, c.col, c.typing))
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// `glyph_echo` (the Return's exemption for a key's echo) needs an unpaid
/// press, read before the classifier spends the pool; a stamp alone is not a
/// key's echo. A coalesced `+3` spends three presses but pops one stamp, and
/// the two dangling stamps must not let each keyless same-row `+1` after a
/// fresh Enter through as a glyph's echo.
#[test]
fn a_dangling_stamp_does_not_hold_the_enter_open_for_a_keyless_plus_one() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_typed(t0 + ms(10));
    glow.note_typed(t0 + ms(20));
    glow.note_typed(t0 + ms(30));
    let echo = t0 + ms(50);
    // The coalesced +3: every press paid, two stamps dangling.
    glow.tick(Some((3, 8)), echo, &c, g, &mut out);
    assert_eq!(
        last_row(&glow),
        Some(("licensed", "key", (3, 5), (3, 8))),
        "{:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.typed_credits_within(echo), 0, "all three spent");
    assert!(
        glow.type_hint.any_fresh(echo, CursorGlow::TYPE_HINT_FRESH),
        "a stamp dangles"
    );
    glow.note_return(echo + ms(10));
    // Three keyless same-row +1s inside both windows: the first is the
    // Return's own (spent there), the rest are program output on a stamp with
    // no press behind it.
    glow.tick(Some((3, 9)), echo + ms(30), &c, g, &mut out);
    assert_eq!(
        last_row(&glow),
        Some(("licensed", "key", (3, 8), (3, 9))),
        "the first +1 is the Return's response: {:?}",
        ring_rows(&glow)
    );
    assert!(
        glow.return_hint.is_none(),
        "the Return is spent on its own +1, not held open by the dangling stamp"
    );
    glow.tick(Some((3, 10)), echo + ms(60), &c, g, &mut out);
    assert_eq!(
        last_row(&glow),
        Some(("no-fresh-hint", "none", (3, 9), (3, 10))),
        "{:?}",
        ring_rows(&glow)
    );
    glow.tick(Some((3, 11)), echo + ms(90), &c, g, &mut out);
    assert_eq!(
        last_row(&glow),
        Some(("no-fresh-hint", "none", (3, 10), (3, 11))),
        "{:?}",
        ring_rows(&glow)
    );
    let lit = v2_cols(&glow, 3);
    assert!(
        !lit.contains(&9) && !lit.contains(&10),
        "a program cell lit on a dangling stamp: {lit:?}"
    );
}

/// A keyless wide hop under a one-shot licence (Enter, a Tab gesture) lights no
/// landing on a dangling stamp: it is judged a typed re-anchor, and
/// `re_anchor_lays_landing`'s credit conjunct keeps the seam from sweeping
/// `cc-1..cc`.
#[test]
fn a_keyless_wide_hop_under_a_one_shot_licence_lights_no_landing_on_a_dangling_stamp() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    type OneShotArm = fn(&mut CursorGlow, Instant);
    let arms: [(&str, OneShotArm); 2] = [
        ("return", CursorGlow::note_return),
        ("gesture", CursorGlow::note_user_gesture),
    ];
    for (name, one_shot) in arms {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((3, 5)), t0, &c, g, &mut out);
        glow.note_typed(t0 + ms(10));
        glow.note_typed(t0 + ms(20));
        glow.note_typed(t0 + ms(30));
        let echo = t0 + ms(50);
        glow.tick(Some((3, 8)), echo, &c, g, &mut out);
        assert_eq!(
            glow.typed_credits_within(echo),
            0,
            "{name}: all three spent"
        );
        assert!(
            glow.type_hint.any_fresh(echo, CursorGlow::TYPE_HINT_FRESH),
            "{name}: a stamp dangles"
        );
        one_shot(&mut glow, echo + ms(10));
        // A keyless +32 inside both windows: licensed by the one-shot, judged a
        // typed re-anchor, refused by the share rule, and dark.
        glow.tick(Some((3, 40)), echo + ms(60), &c, g, &mut out);
        assert_eq!(
            last_row(&glow),
            Some(("no-credits", "none", (3, 8), (3, 40))),
            "{name}: {:?}",
            ring_rows(&glow)
        );
        assert!(
            !v2_cols(&glow, 3).contains(&39),
            "{name}: a program cell lit on a dangling stamp: {:?}",
            v2_cols(&glow, 3)
        );
    }
}

// ──────── the typed press across a one-shot: nothing whose echo is owed is superseded ────────

/// A one-shot's own move can land after the next glyph's dispatch: a slow
/// prompt (SSH, a p10k precmd) answering Enter, Claude Code's composer clearing
/// to its inset, Shift+Enter's line break, a Backspace's or a ^W / ^U retreat.
/// The one-shot survives the typed press (its echo is still owed); its move is
/// licensed and spends or forgets nothing of the glyph's credit (a forget edge
/// forgets only presses banked at or before the one-shot), and a Return-paired
/// move leaves the glyph's stamp for its own echo. The glyph's +1 then lights
/// the glyph's cell, the cell nobody typed stays dark, and a keyless +1 after
/// it is refused.
#[test]
fn a_glyph_typed_before_a_one_shots_move_lands_keeps_its_credit_and_its_cell() {
    type OneShotArm = fn(&mut CursorGlow, Instant);
    let enter: OneShotArm = |glow, t| {
        glow.supersede_typed_press();
        glow.note_return(t);
    };
    // The host's Shift+Enter arm: a typed stamp beside the newline hint.
    let newline: OneShotArm = |glow, t| {
        glow.supersede_typed_press();
        glow.note_typed(t);
        glow.note_newline_break(t);
    };
    let backspace: OneShotArm = |glow, t| glow.note_backspace_erasing(t, Some(1));
    let word_kill: OneShotArm = |glow, t| glow.note_word_kill(t, true);
    let line_kill: OneShotArm = |glow, t| glow.note_kill(t, true);
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    // (label, the one-shot, an alt-screen composer, Return-paired, where its
    // move lands, the cell left of it that nobody typed). The pre-roll leaves
    // `xxx` at (27, 2..5) and the caret at (27, 5).
    for (label, arm, composer, paired, (row, col), untyped) in [
        (
            "Enter, one row down",
            enter,
            false,
            true,
            (28u16, 2u16),
            Some(1u16),
        ),
        ("Enter, output rows", enter, false, true, (29, 2), Some(1)),
        ("Enter, |dc| = 1", enter, false, true, (28, 4), Some(3)),
        ("the composer clear", enter, true, true, (27, 2), Some(1)),
        ("Shift+Enter", newline, true, true, (28, 2), Some(1)),
        ("Backspace", backspace, false, false, (27, 4), None),
        ("^W", word_kill, false, false, (27, 4), Some(1)),
        ("^U", line_kill, false, false, (27, 2), Some(1)),
    ] {
        let mut glow = CursorGlow::default();
        if composer {
            glow.note_context(true);
        }
        let t0 = Instant::now();
        let pre = pre_roll(&mut glow, 27, t0, &c, g);
        let one_shot = pre + ms(200);
        arm(&mut glow, one_shot);
        // The next glyph, before the one-shot's move landed.
        let k = one_shot + ms(100);
        glow.supersede_typed_press();
        glow.note_typed(k);
        let landed = k + ms(30);
        if composer {
            glow.note_repaint_blink(landed);
        }
        glow.tick(Some((row, col)), landed, &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| r.0),
            Some("licensed"),
            "{label}: the move is the one-shot's: {:?}",
            last_row(&glow)
        );
        assert_eq!(
            glow.in_flight_tally().credits,
            1,
            "{label}: the move spends nothing of the glyph's: {:?}",
            last_row(&glow)
        );
        assert!(
            !paired
                || glow
                    .type_hint
                    .any_fresh(landed, CursorGlow::TYPE_HINT_FRESH),
            "{label}: the glyph's stamp is left for its own echo"
        );
        let echo = landed + ms(20);
        if composer {
            glow.note_repaint_blink(echo);
        }
        glow.tick(Some((row, col + 1)), echo, &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| r.0),
            Some("licensed"),
            "{label}: the glyph's +1 is licensed: {:?}",
            last_row(&glow)
        );
        assert_eq!(
            glow.in_flight_tally().credits,
            0,
            "{label}: the glyph's credit is spent by its echo"
        );
        let lit = v2_cols(&glow, row);
        assert!(
            lit.contains(&col),
            "{label}: the glyph's cell ({row}, {col}) is lit: {lit:?}"
        );
        if let Some(untyped) = untyped {
            assert!(
                !lit.contains(&untyped),
                "{label}: ({row}, {untyped}) is dark — nobody typed it: {lit:?}"
            );
        }
        let spawns = glow.spawns();
        glow.tick(Some((row, col + 2)), echo + ms(400), &c, g, &mut out);
        assert_eq!(
            glow.spawns(),
            spawns,
            "{label}: a keyless +1 is refused: {:?}",
            last_row(&glow)
        );
    }
}

/// A superseding edit or navigation key cancels the composer home at the key
/// edge, since those can be swallowed without an observed move. Tab keeps the
/// in-flight newline classifier.
#[test]
fn a_superseding_key_cancels_a_composer_home_even_without_a_move() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    for label in ["class change", "navigation", "kill"] {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.note_context(true);
        glow.tick(Some((27, 5)), t0, &c, g, &mut out);
        assert!(glow.v2.engaged());
        let chord = t0 + ms(100);
        glow.note_newline_break(chord);
        assert!(matches!(
            glow.v2.pending_events().last(),
            Some((rk::Event::ComposerNewline, at)) if *at == chord
        ));
        let next = chord + ms(10);
        match label {
            "class change" => glow.clear_typed(next),
            "navigation" => glow.note_navigation(next),
            "kill" => glow.note_kill(next, false),
            _ => unreachable!("the case table above is exhaustive"),
        }
        assert!(
            glow.v2.pending_events().iter().any(|(event, at)| {
                matches!(event, rk::Event::CancelComposerNewline) && *at == next
            }),
            "{label} must cancel on the key even if no caret move follows"
        );
    }

    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.note_context(true);
    glow.tick(Some((27, 5)), t0, &c, g, &mut out);
    glow.clear_typed(t0 + ms(100));
    assert!(
        !glow
            .v2
            .pending_events()
            .iter()
            .any(|(event, _)| matches!(event, rk::Event::CancelComposerNewline)),
        "an ordinary class change queues no composer-only event"
    );
    glow.note_newline_break(t0 + ms(200));
    glow.note_user_gesture(t0 + ms(210));
    assert!(
        !glow
            .v2
            .pending_events()
            .iter()
            .any(|(event, _)| matches!(event, rk::Event::CancelComposerNewline)),
        "a Tab gesture preserves the composer break still in flight"
    );
}

/// The composer's new line starts its walk again however soon the next glyph follows the newline
/// (`Ribbon::fresh_line`, through the seam). Thirty glyphs on row 27, Shift+Enter, and the next
/// glyph pressed before or after the break lands: either way row 28 continues row 27's colour
/// one step past its last glyph, at `d0 = 0`. This is a top-anchored box; the bottom-anchored one
/// is pinned in `tests/composer_box_growth_wrap.rs`.
#[test]
fn a_composer_newline_starts_the_walk_again_however_soon_the_next_glyph_follows() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    for (what, raced) in [
        ("the glyph after the break", false),
        ("the glyph before it", true),
    ] {
        let mut glow = CursorGlow::default();
        glow.note_context(true);
        let t0 = Instant::now();
        glow.tick(Some((27, 2)), t0, &c, g, &mut out);
        let pre = type_echoed(
            &mut glow,
            27,
            2,
            "abcdefghijklmnopqrstuvwxyzabcd",
            t0,
            &c,
            g,
        );
        let line = *glow
            .v2
            .ribbon()
            .cohorts()
            .iter()
            .find(|k| k.row == 27 && !k.wake)
            .expect("row 27's line");
        assert_eq!(
            (line.anchor_col, line.col1, line.d0),
            (2, 32, 0.0),
            "{what}: the premise, a cold thirty-cell line"
        );
        let t_next = line.t_at(32);
        let chord = pre + ms(200);
        glow.supersede_typed_press();
        glow.note_typed(chord);
        glow.note_newline_break(chord);
        let (key, brk) = if raced {
            let key = chord + ms(120);
            (key, key + ms(30))
        } else {
            let brk = chord + ms(150);
            (brk + ms(20), brk)
        };
        let press = |glow: &mut CursorGlow| {
            glow.supersede_typed_press();
            glow.note_typed(key);
        };
        if raced {
            press(&mut glow);
        }
        glow.note_repaint_blink(brk);
        glow.tick(Some((28, 2)), brk, &c, g, &mut out);
        if !raced {
            press(&mut glow);
        }
        let echo = key.max(brk) + ms(20);
        glow.note_repaint_blink(echo);
        glow.tick(Some((28, 3)), echo, &c, g, &mut out);
        assert!(
            v2_cols(&glow, 28).contains(&2),
            "{what}: the glyph's cell (28, 2) is lit: {:?}",
            v2_cols(&glow, 28)
        );
        type_echoed(&mut glow, 28, 3, "efghijklmnopqrstuv", echo, &c, g);
        let new_line = *glow
            .v2
            .ribbon()
            .cohorts()
            .iter()
            .filter(|k| k.row == 28 && !k.wake)
            .min_by_key(|k| k.anchor_col)
            .expect("row 28's line");
        assert_eq!(
            (new_line.anchor_col, new_line.t0.to_bits(), new_line.d0),
            (2, t_next.to_bits(), 0.0),
            "{what}: row 28 continues the colour ({} against {t_next}) and starts its walk \
             (d0 {})",
            new_line.t0,
            new_line.d0
        );
    }
}

/// A same-row forward hop the in-flight pool pays for is a glyph's echo, never
/// the arrow's (or a moving kill's) move, and the nav hint is left for the
/// arrow's own hop. Shape: `abcd` then End over a 300 ms link, echoes draining
/// +2, +2, then the End hop.
#[test]
fn a_glyph_echo_landing_after_an_arrow_is_the_glyphs_not_the_arrows() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    for k in 1..=4u64 {
        glow.note_typed(t0 + ms(10 * k));
    }
    // The host's navigation arm: the typed stamps are wiped, the credits
    // stay, the nav hint is armed.
    glow.clear_typed(t0 + ms(45));
    glow.note_motion(t0 + ms(45));
    glow.tick(Some((3, 4)), t0 + ms(50), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "inflight")),
        "`ab`'s echo is the pool's: {:?}",
        last_row(&glow)
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0, "…and forgets nothing");
    glow.tick(Some((3, 6)), t0 + ms(70), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| r.0),
        Some("licensed"),
        "`cd`'s echo is licensed: {:?}",
        last_row(&glow)
    );
    assert!(
        dark_in(&glow, 3, 2..6).is_empty(),
        "every typed cell is lit: dark {:?}",
        dark_in(&glow, 3, 2..6)
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0);
    // The End hop itself, on its own frame, is the arrow's.
    glow.tick(Some((3, 12)), t0 + ms(90), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "the End hop keeps its own licence: {:?}",
        last_row(&glow)
    );

    // `ab` then ^W with late echoes: the +2 is the glyphs' (the kill forgot
    // their pool at the key), and the retreat after it is the kill's.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    glow.note_typed(t0 + ms(10));
    glow.note_typed(t0 + ms(20));
    glow.clear_typed(t0 + ms(30));
    glow.note_word_kill(t0 + ms(30), true);
    glow.tick(Some((3, 4)), t0 + ms(50), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| r.0),
        Some("licensed"),
        "`ab`'s echo behind the kill: {:?}",
        last_row(&glow)
    );
    assert!(
        glow.kill_retreat_pending.is_some(),
        "…and it did not spend the kill's one retreat"
    );
    glow.tick(Some((3, 2)), t0 + ms(70), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "the kill's retreat keeps its licence: {:?}",
        last_row(&glow)
    );
    assert!(glow.kill_retreat_pending.is_none(), "…and spends it");

    // Control: a hop the pool cannot pay for is still the arrow's: four
    // presses in flight, Home, and a keyless +4 half a second later is refused.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 9)), t0, &c, g, &mut out);
    let last = stalled_keys(&mut glow, 4, t0);
    glow.note_navigation(last + ms(100));
    glow.tick(Some((3, 2)), last + ms(110), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key"))
    );
    glow.tick(Some((3, 6)), last + ms(600), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("no-fresh-hint", "none")),
        "after the arrow's own move the pool is empty: {:?}",
        last_row(&glow)
    );
}

/// A `Typed` with no move after it in the frame has not been echoed: the ribbon
/// holds it (the row's cohort kept alive) and the key's cell comes with its
/// echo from the seam's sweep. Replaying it at the mirror would light the
/// prompt's trailing space, or with the mirror at column 0 the previous row's
/// last cell.
#[test]
fn a_key_whose_echo_lands_a_frame_late_lays_no_cell_left_of_the_caret() {
    let g = Geom {
        rows: 24,
        cols: 80,
        ..wide_geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let live = |glow: &CursorGlow| -> Vec<(u16, u16)> {
        glow.v2_ribbon().map_or(Vec::new(), |r| {
            r.cells()
                .iter()
                .filter(|c| !c.leaving())
                .map(|c| (c.row, c.col))
                .collect()
        })
    };
    // A plain shell over a laggy link: `$ ls⏎`, output, a new prompt `$ `,
    // and the first key's echo lands a frame late.
    for lag in [10u64, 40, 120, 300] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((5, 2)), t0 + ms(1000), &c, g, &mut out);
        glow.note_typed(t0 + ms(1100));
        glow.tick(Some((5, 3)), t0 + ms(1110), &c, g, &mut out);
        glow.note_typed(t0 + ms(1200));
        glow.tick(Some((5, 4)), t0 + ms(1210), &c, g, &mut out);
        glow.note_return(t0 + ms(1500));
        glow.tick(Some((6, 0)), t0 + ms(1520), &c, g, &mut out);
        glow.tick(Some((7, 2)), t0 + ms(1560), &c, g, &mut out);
        glow.tick(Some((7, 2)), t0 + ms(2900), &c, g, &mut out);
        glow.note_typed(t0 + ms(3000));
        // A frame before the echo: the link is slow.
        glow.tick(Some((7, 2)), t0 + ms(3000 + lag / 2), &c, g, &mut out);
        assert!(
            !live(&glow).contains(&(7, 1)),
            "lag {lag} ms: the prompt's space lit before the echo: {:?}",
            live(&glow)
        );
        glow.tick(Some((7, 3)), t0 + ms(3000 + lag), &c, g, &mut out);
        assert!(
            live(&glow).contains(&(7, 2)),
            "lag {lag} ms: the key's cell is lit with its echo: {:?}",
            live(&glow)
        );
        assert!(
            !live(&glow).contains(&(7, 1)),
            "lag {lag} ms: the prompt's space stays dark: {:?}",
            live(&glow)
        );
    }
    // The mirror at column 0 (after Enter): the fold's phantom on the
    // previous row's last cell.
    let g = Geom {
        rows: 24,
        cols: 40,
        ..wide_geom()
    };
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((5, 10)), t0 + ms(1000), &c, g, &mut out);
    glow.note_typed(t0 + ms(1100));
    glow.tick(Some((5, 11)), t0 + ms(1110), &c, g, &mut out);
    glow.note_return(t0 + ms(1500));
    glow.tick(Some((6, 0)), t0 + ms(1520), &c, g, &mut out);
    glow.note_typed(t0 + ms(2000));
    glow.tick(Some((6, 0)), t0 + ms(2010), &c, g, &mut out);
    assert!(
        !live(&glow).contains(&(5, 39)),
        "the previous row's last cell lit by the fold: {:?}",
        live(&glow)
    );
    glow.tick(Some((6, 1)), t0 + ms(2100), &c, g, &mut out);
    assert!(live(&glow).contains(&(6, 0)), "{:?}", live(&glow));
    assert!(!live(&glow).contains(&(5, 39)), "{:?}", live(&glow));
}

// ──────── the delivered insert's presses ────────

/// Bound, don't spend: presses banked at or before an unknown-width insert's delivery
/// (multi-line paste, ⌃V, Tab) can only echo in its hop or the next repaint, so they retire 2 s
/// after the hop (`INSERT_REWRITE_FRESH`) rather than living the 10 s patience and licensing the
/// next keyless +1 as `inflight`.
#[test]
fn a_key_queued_behind_an_unknown_insert_is_retired_with_the_hop_that_echoed_it() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let arm = |glow: &mut CursorGlow, pre: Instant, width: InsertWidth| -> Instant {
        // The key queued behind the paste: banked at dispatch, its stamp
        // revoked at enqueue; the paste's receipt then the key's.
        let k = pre + ms(100);
        glow.note_typed(k);
        glow.revoke_input_hints_at(k);
        let d = k + ms(40);
        glow.note_insert_delivered_from(k - ms(50), d, width);
        glow.note_delivered(d + ms(1), DeliveredClass::Typed);
        d
    };
    // One hop: the placeholder (16 cells) and the key (1) echo as +17.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let d = arm(&mut glow, pre, InsertWidth::Unknown);
    let echo = d + ms(30);
    glow.tick(Some((27, 5 + 17)), echo, &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "insert")),
        "{:?}",
        last_row(&glow)
    );
    let spawns = glow.spawns();
    glow.tick(Some((27, 5 + 17)), echo + ms(2500), &c, g, &mut out);
    assert_eq!(
        glow.in_flight_tally().credits,
        0,
        "the queued key's credit was in the insert's hop and is retired with it"
    );
    glow.tick(Some((27, 5 + 18)), echo + ms(2600), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        spawns,
        "a keyless +1 on the row is refused: {:?}",
        last_row(&glow)
    );

    // Two frames: the placeholder echoes as +16, the key as its own +1 a frame later, spending
    // its credit as `key`.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let d = arm(&mut glow, pre, InsertWidth::Unknown);
    let echo = d + ms(30);
    glow.tick(Some((27, 5 + 16)), echo, &c, g, &mut out);
    assert_eq!(last_row(&glow).map(|r| r.1), Some("insert"));
    glow.tick(Some((27, 5 + 17)), echo + ms(20), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "the key's own echo a frame later is its own: {:?}",
        last_row(&glow)
    );
    assert!(
        v2_cols(&glow, 27).contains(&21),
        "…and lit: {:?}",
        v2_cols(&glow, 27)
    );
    assert_eq!(glow.in_flight_tally().credits, 0);

    // Control: the priced twin (`Cells(16)`, the key as +17) spends the key at the hop.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let d = arm(&mut glow, pre, InsertWidth::Cells(16));
    glow.tick(Some((27, 5 + 17)), d + ms(30), &c, g, &mut out);
    assert_eq!(last_row(&glow).map(|r| r.1), Some("insert"));
    assert_eq!(
        glow.in_flight_tally().credits,
        0,
        "the priced surplus spends the key"
    );
}

// ──────── the kill relay: one kill, one `Kill` ────────

/// The seam emits the `Kill` at a ^U/^W retreat, before the `Move`, so the
/// ribbon takes the erase-retreat arm rather than the composer re-anchor, which
/// would relay the killed word's width under the prompt and keep those cells
/// lit while typing continues.
#[test]
fn a_kill_retreat_relays_nothing_under_the_prompt() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    for (label, prompt_end, word) in [
        ("^U under a 44-column zsh prompt", 44u16, false),
        ("^W under a 44-column zsh prompt", 44, true),
        ("^U under `% `", 2, false),
    ] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((10, prompt_end)), t0, &c, g, &mut out);
        let typed = type_echoed(&mut glow, 10, prompt_end, "ab cd", t0, &c, g);
        let stray = |glow: &CursorGlow| -> Vec<u16> {
            v2_cols(glow, 10)
                .into_iter()
                .filter(|&col| col < prompt_end)
                .collect()
        };
        assert!(
            stray(&glow).is_empty(),
            "{label}: nothing under the prompt before the kill"
        );
        // ^U (or ^W): the kill at the key, its retreat 10 ms later.
        let kill = typed + ms(200);
        if word {
            glow.note_word_kill(kill, true);
        } else {
            glow.note_kill(kill, true);
        }
        glow.tick(Some((10, prompt_end)), kill + ms(10), &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| r.0),
            Some("licensed"),
            "{label}: the kill's retreat is licensed: {:?}",
            last_row(&glow)
        );
        assert!(
            stray(&glow).is_empty(),
            "{label}: the kill relayed cells under the prompt at the retreat: {:?}",
            stray(&glow)
        );
        // The hand keeps typing on the row — the deferred relay's trigger.
        let after = type_echoed(&mut glow, 10, prompt_end, "abcdefg", kill + ms(300), &c, g);
        assert!(
            stray(&glow).is_empty(),
            "{label}: the kill relayed cells under the prompt as the hand typed on: {:?}",
            stray(&glow)
        );
        let lit = v2_cols(&glow, 10);
        assert!(
            (prompt_end..prompt_end + 7).all(|col| lit.contains(&col)),
            "{label}: the typed cells are lit: {lit:?}"
        );
        glow.tick(
            Some((10, prompt_end + 7)),
            after + ms(3000),
            &c,
            g,
            &mut out,
        );
        assert!(
            stray(&glow).is_empty(),
            "{label}: …and 3 s later: {:?}",
            stray(&glow)
        );
    }
}

/// One kill, one `Kill`: `kill_reported_to_v2` is the key whose `Kill` went
/// out, and the retreat and the poof each emit only for a kill not yet
/// reported, whichever comes first; a new kill is a new key.
#[test]
fn a_kill_reaches_v2_once_whichever_witness_comes_first() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(100, ' ');
        v
    };
    let mut out = Vec::new();

    // Poof first: ^U over a slow link; the caret fallback answers at the grace
    // with the caret unmoved and the retreat lands 30 ms later.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.observe_row(10, 7, &row("$ ab cd"), t0);
    glow.tick(Some((10, 7)), t0, &c, g, &mut out);
    let kill = t0 + ms(100);
    glow.note_kill(kill, true);
    let grace = kill + ms(70);
    glow.observe_row(10, 7, &row("$ ab cd"), grace);
    glow.tick(Some((10, 7)), grace, &c, g, &mut out);
    assert!(glow.last_poof.is_some(), "the caret fallback answered");
    assert_eq!(glow.v2.kills(), 1, "the poof minted the kill's `Kill`");
    let retreat = kill + ms(100);
    glow.observe_row(10, 2, &row("$ "), retreat);
    glow.tick(Some((10, 2)), retreat, &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "the retreat is the kill's: {:?}",
        last_row(&glow)
    );
    assert_eq!(
        glow.v2.kills(),
        1,
        "the retreat does not mint the kill's `Kill` a second time"
    );

    // A second kill before the first's poof scan: ^W, its retreat, ^W again,
    // the probe, the second retreat. Two kills, two `Kill`s.
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.observe_row(10, 7, &row("$ ab cd"), t0);
    glow.tick(Some((10, 7)), t0, &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_word_kill(k1, true);
    glow.tick(Some((10, 5)), k1 + ms(10), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "{:?}",
        last_row(&glow)
    );
    assert_eq!(glow.v2.kills(), 1, "the first retreat carries its kill");
    let k2 = k1 + ms(30);
    glow.note_word_kill(k2, true);
    let probe = k2 + ms(10);
    glow.observe_row(10, 5, &row("$ ab "), probe);
    glow.tick(Some((10, 5)), probe, &c, g, &mut out);
    assert!(glow.last_poof.is_some(), "the span branch answered");
    let retreat2 = k2 + ms(20);
    glow.tick(Some((10, 2)), retreat2, &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "the second retreat is the second kill's: {:?}",
        last_row(&glow)
    );
    assert_eq!(glow.v2.kills(), 2, "two kills, two `Kill`s — not three");
}

// ──────── the held park ────────

/// A park from a mid-line origin is not the anchored return: `pc == p.origin`
/// confines the anchored-lane return arm to end-of-line typing, so the tail's
/// re-lay past the row's end flushes the park rather than releasing it. Pins
/// only what the conjunct guarantees, not the flush's verdict.
#[test]
fn a_mid_line_park_is_not_released_by_the_tails_re_lay() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    // The row's end is at 10; the caret sits mid-line at 5.
    glow.tick(Some((5, 5)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((5, 10, 1)));
    glow.tick(Some((5, 5)), t0 + ms(5), &c, g, &mut out);
    let k1 = t0 + ms(100);
    glow.note_typed(k1);
    let park_at = k1 + ms(10);
    glow.tick(Some((5, 2)), park_at, &c, g, &mut out);
    assert_eq!(
        glow.held_park.map(|p| (p.origin, p.landing)),
        Some((5, 2)),
        "{:?}",
        ring_rows(&glow)
    );
    // Hidden frame, print anchor past the old end: the anchored return for an
    // end-of-line origin, not from mid-line.
    glow.observe_print_anchor(Some((5, 11, 2)));
    glow.tick(None, park_at + ms(15), &c, g, &mut out);
    let tally = glow.in_flight_tally();
    assert_eq!(
        tally.park_returns,
        0,
        "a mid-line park is not the anchored return: {:?}",
        ring_rows(&glow)
    );
    assert!(glow.held_park.is_none(), "it flushed instead");
    assert_eq!(tally.park_flushed, 1, "{tally:?}");
    assert_ne!(
        last_row(&glow),
        Some(("licensed", "key", (5, 10), (5, 11))),
        "the tail's re-lay is not the key's echo: {:?}",
        ring_rows(&glow)
    );
}

// ──────── the stamp bank ────────

/// v2's `Typed` banks the press credit's bounded width (`RAINBOW_TYPED_SWEEP_MAX`), so a
/// saturated IME/dictation commit (`u16::MAX` cells) cannot lay an O(n²) ribbon folding through
/// the grid: an echo of the cap's width lays exactly the cap.
#[test]
fn a_wide_committed_text_lays_at_most_the_sweep_cap() {
    let g = Geom {
        rows: 60,
        cols: 300,
        ..wide_geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((59, 0)), t0, &c, g, &mut out);
    // One echoed key seats the mirror; the wide commit follows it.
    glow.note_typed(t0 + ms(16));
    glow.tick(Some((59, 1)), t0 + ms(32), &c, g, &mut out);
    let before = glow.v2_ribbon().map_or(0, |r| r.live_cells());
    assert_eq!(before, 1, "the seating key's cell");
    glow.note_typed_glyph(t0 + ms(100), u16::MAX, false, rk::TypedClass::Glyph);
    let cap = u16::try_from(CursorGlow::RAINBOW_TYPED_SWEEP_MAX).expect("the cap is a width");
    glow.tick(Some((59, 1 + cap)), t0 + ms(110), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "the commit's echo is licensed as one coalesced echo: {:?}",
        last_row(&glow)
    );
    let live = glow.v2_ribbon().map_or(0, |r| r.live_cells());
    assert_eq!(
        live - before,
        CursorGlow::RAINBOW_TYPED_SWEEP_MAX,
        "the commit lays exactly the sweep cap ({}) — every cell of the echo the seam licensed, and not one more",
        CursorGlow::RAINBOW_TYPED_SWEEP_MAX
    );
}

/// `TypedStamps::split_off_after` and `merge` are index-addressed (re-banking through `stamp()`
/// is quadratic) and must leave the bank as one-by-one banking would: packed oldest-first, the
/// newest `TYPED_STAMP_DEPTH` surviving an overflow, at every fill.
#[test]
fn a_stamp_banks_split_and_merge_match_one_by_one_banking_at_every_fill() {
    let t0 = Instant::now();
    let at = |i: usize| t0 + ms(i as u64 + 1);
    for n_before in [0usize, 1, 7, 64, TYPED_STAMP_DEPTH - 1, TYPED_STAMP_DEPTH] {
        for n_after in [0usize, 1, 3, 64, TYPED_STAMP_DEPTH - n_before] {
            let n = n_before + n_after;
            if n == 0 || n > TYPED_STAMP_DEPTH {
                continue;
            }
            // Bank n stamps with the cut after the n_before-th; the split takes
            // the later ones, a judgment consumes `spent` kept ones, the merge
            // puts the later ones back.
            let mut bank = TypedStamps::default();
            for i in 0..n {
                bank.stamp(at(i));
            }
            let cut = if n_before == 0 { t0 } else { at(n_before - 1) };
            let mut later = bank.split_off_after(cut);
            let kept: Vec<_> = bank.slots.iter().flatten().copied().collect();
            let taken: Vec<_> = later.slots.iter().flatten().copied().collect();
            assert_eq!(
                kept.len(),
                n_before,
                "n_before={n_before} n_after={n_after}"
            );
            assert_eq!(
                taken.len(),
                n_after,
                "n_before={n_before} n_after={n_after}"
            );
            assert!(
                kept.iter().all(|t| *t <= cut) && taken.iter().all(|t| *t > cut),
                "n_before={n_before} n_after={n_after}: the cut"
            );
            assert!(
                bank.slots[kept.len()..].iter().all(Option::is_none)
                    && later.slots[taken.len()..].iter().all(Option::is_none),
                "n_before={n_before} n_after={n_after}: both halves packed"
            );
            for spent in [0usize, 1, n_before] {
                let mut merged = bank;
                for _ in 0..spent {
                    merged.take_fresh(at(n), f32::MAX);
                }
                // The reference: bank the later stamps one by one.
                let mut reference = merged;
                for t in taken.iter().copied() {
                    reference.stamp(t);
                }
                merged.merge(later);
                assert_eq!(
                    merged.slots.to_vec(),
                    reference.slots.to_vec(),
                    "n_before={n_before} n_after={n_after} spent={spent}"
                );
            }
            // Overflow: the merged bank fills before the later stamps return,
            // so the oldest kept ones drop, as one-by-one banking would.
            if n_after > 0 {
                let mut full = TypedStamps::default();
                for i in 0..TYPED_STAMP_DEPTH {
                    full.stamp(at(1000 + i));
                }
                let mut reference = full;
                for t in taken.iter().copied() {
                    reference.stamp(t);
                }
                full.merge(later);
                assert_eq!(
                    full.slots.to_vec(),
                    reference.slots.to_vec(),
                    "n_before={n_before} n_after={n_after}: overflow"
                );
                later.clear();
            }
        }
    }
}

// ──────── the band's walk ────────

/// The band's walk is the hand's, whatever the echo's lag. A cell's `t` is a function of its
/// column (`Cohort::t_at`), never a clock, and the seam dates a typed sweep at the key's clock
/// (the press's stamp, or the oldest unpaid press for a stalled batch), floored one stamp window
/// before the echo (`rainbow_kitty::SWEEP_BIRTH_FLOOR_S`). Thirty keys at 100 ms, echoed 3 to
/// 400 ms late or as one hop 3 s after the last press, lay the same `t` on every column, and
/// every cell is born at a key's clock, never its echo's.
#[test]
fn the_bands_walk_is_the_hands_whatever_the_echos_lag() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let row = 7_u16;
    let keys = 30_u16;
    let cadence = 100_u64;
    let floor = Duration::from_secs_f32(rk::SWEEP_BIRTH_FLOOR_S);
    // The run as laid: `(col, t, born)` per live typing cell, by column.
    let run = |glow: &CursorGlow| -> Vec<(u16, f32, Instant)> {
        let mut cells: Vec<(u16, f32, Instant)> = glow.v2_ribbon().map_or(Vec::new(), |r| {
            r.cells()
                .iter()
                .filter(|c| c.row == row && c.typing && !c.leaving())
                .map(|c| (c.col, c.t, c.born))
                .collect()
        });
        cells.sort_by_key(|c| c.0);
        cells
    };
    let mut walks: Vec<(String, Vec<(u16, f32)>)> = Vec::new();
    for lag in [3_u64, 8, 120, 400] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((row, 2)), t0, &c, g, &mut out);
        // Presses and echoes in clock order, one frame each: a press's frame
        // sees no move, an echo's frame moves the mirror one cell.
        let mut events: Vec<(u64, bool)> = (0..u64::from(keys))
            .flat_map(|k| {
                let press = cadence * (k + 1);
                [(press, false), (press + lag, true)]
            })
            .collect();
        events.sort_unstable();
        let mut echoed = 0_u16;
        for (at, is_echo) in events {
            let t = t0 + ms(at);
            if is_echo {
                echoed += 1;
            } else {
                glow.note_typed(t);
            }
            glow.tick(Some((row, 2 + echoed)), t, &c, g, &mut out);
        }
        let cells = run(&glow);
        assert_eq!(
            cells.iter().map(|c| c.0).collect::<Vec<_>>(),
            (2..2 + keys).collect::<Vec<_>>(),
            "lag {lag} ms: every key's cell is lit, and only those"
        );
        for &(col, _, born) in &cells {
            let k = u64::from(col - 2);
            let press = t0 + ms(cadence * (k + 1));
            let echo = press + ms(lag);
            let since_echo = echo.saturating_duration_since(born);
            if ms(lag) <= floor {
                assert_eq!(
                    born, press,
                    "lag {lag} ms: column {col} is born at its own press, not {:?} before its echo",
                    since_echo
                );
            } else {
                // Past the stamp window the sweep's stamp is the oldest press
                // still inside it (`peek_fresh`): a key's clock, never the
                // echo's, never before the floor.
                assert!(
                    born >= press && born >= echo - floor && born < echo,
                    "lag {lag} ms: column {col} is born {:?} before its echo — a key's clock \
                     inside the stamp window, not the echo's",
                    since_echo
                );
            }
        }
        walks.push((
            format!("echo {lag} ms late"),
            cells.iter().map(|c| (c.0, c.1)).collect(),
        ));
    }
    // The stall: thirty presses, each held in its own frame, then the row
    // echoes as one thirty-cell hop three seconds after the last key.
    {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((row, 2)), t0, &c, g, &mut out);
        let mut last = t0;
        for k in 0..u64::from(keys) {
            last = t0 + ms(cadence * (k + 1));
            glow.note_typed(last);
            glow.tick(Some((row, 2)), last, &c, g, &mut out);
        }
        assert!(
            run(&glow).is_empty(),
            "held keys lay nothing before their echo: {:?}",
            run(&glow)
        );
        let echo = last + ms(3000);
        glow.tick(Some((row, 2 + keys)), echo, &c, g, &mut out);
        let cells = run(&glow);
        assert_eq!(
            cells.iter().map(|c| c.0).collect::<Vec<_>>(),
            (2..2 + keys).collect::<Vec<_>>(),
            "the batch: every key's cell is lit on the echoing frame"
        );
        for &(col, _, born) in &cells {
            assert_eq!(
                born,
                echo - floor,
                "the batch: column {col} is born at the floor — one stamp window before the \
                 echo — not at the echo"
            );
        }
        walks.push((
            "a batch echoing 3 s late".to_owned(),
            cells.iter().map(|c| (c.0, c.1)).collect(),
        ));
    }
    let (first_name, first) = &walks[0];
    assert!(
        first
            .iter()
            .all(|&(col, t)| (t - rk::ribbon::walk_t(f32::from(col - 2))).abs() < 1e-6),
        "{first_name}: the run is one cohort walking from its first key: {first:?}"
    );
    for (name, walk) in &walks[1..] {
        assert_eq!(
            walk, first,
            "{name}: the band's walk differs from {first_name}'s"
        );
    }
}

// ──────── the minibuffer glyph gate ────────

/// The gate refuses only on a known foreign glyph: the run's last cell is
/// foreign when every live press carries its exact glyph and none matches. Any
/// live press may own the run's end (typeahead); an empty pool, a glyph-less
/// press (wide glyph, IME commit, count-only host) or expired presses make the
/// judgment unknown and the lane lays as without the gate.
#[test]
fn the_glyph_gate_refuses_only_a_known_foreign_glyph() {
    let t0 = Instant::now();
    let mut p = PressCredits::default();
    assert_eq!(p.foreign_to_live(t0, '_'), None, "an empty pool is unknown");
    p.bank(t0, 1, Some('c'));
    assert_eq!(p.foreign_to_live(t0, '_'), Some(true), "zle's fake cursor");
    assert_eq!(
        p.foreign_to_live(t0, 'c'),
        Some(false),
        "the key's own glyph"
    );
    p.bank(t0 + ms(5), 1, Some('l'));
    assert_eq!(
        p.foreign_to_live(t0 + ms(5), 'c'),
        Some(false),
        "typeahead: the older press still owns its echo"
    );
    p.bank(t0 + ms(10), 2, None);
    assert_eq!(
        p.foreign_to_live(t0 + ms(10), '_'),
        None,
        "a glyph-less press makes the run unknown"
    );
    let late = t0 + Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S + 1.0);
    assert_eq!(p.foreign_to_live(late, '_'), None, "no live press: unknown");
}

/// The run's last glyph belongs only to the anchor it came with: an anchor handed over without
/// one ([`CursorGlow::observe_print_anchor`]) forgets it, and a `None` anchor keeps the previous
/// pair. Otherwise the glyph is this frame's probe of the caret's row or a flanking row.
#[test]
fn the_glyph_before_a_cell_is_the_host_s_for_its_own_anchor_else_this_frame_s_probe() {
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    glow.observe_print_anchor_glyph(Some((7, 12, 1)), Some('_'));
    assert_eq!(glow.glyph_before(7, 12, t0), Some('_'), "the host's glyph");
    assert_eq!(glow.glyph_before(7, 13, t0), None, "not another run's end");
    glow.observe_print_anchor_glyph(None, Some('x'));
    assert_eq!(
        glow.glyph_before(7, 12, t0),
        Some('_'),
        "a `None` anchor keeps the pair"
    );
    glow.observe_print_anchor(Some((7, 13, 2)));
    assert_eq!(
        glow.glyph_before(7, 13, t0),
        None,
        "an anchor handed over alone carries no glyph"
    );
    let status: Vec<char> = "search: c_".chars().collect();
    glow.observe_row(6, 3, &['p', '%', ' '], t0);
    glow.observe_neighbor_rows(Some(&[]), Some(&status));
    assert_eq!(glow.glyph_before(7, 10, t0), Some('_'), "the row below");
    assert_eq!(glow.glyph_before(6, 2, t0), Some('%'), "the caret's row");
    assert_eq!(
        glow.glyph_before(7, 40, t0),
        Some(' '),
        "blank past the row"
    );
    assert_eq!(
        glow.glyph_before(7, 10, t0 + ms(16)),
        None,
        "a probe from another frame"
    );
    assert_eq!(glow.glyph_before(8, 10, t0), None, "two rows down: unknown");
    assert_eq!(glow.glyph_before(7, 0, t0), None, "a run with no last cell");
}

/// The typed re-anchor lays its landing only under a live press's glyph (zsh's
/// Ctrl-R whose match hops). One `l` press and the caret hops twelve cells with
/// `-` at the landing cell: declined `program-row`, nothing laid, the press
/// forgotten. Controls: landing on the key's own glyph lays exactly the
/// landing, and a count-only press lays it.
#[test]
fn a_typed_re_anchor_lays_its_landing_only_under_a_live_press_s_glyph() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut row: Vec<char> = "p% ".chars().collect();
    row.resize(16, 'x');
    for (label, landing, glyph, laid) in [
        ("foreign", '-', true, false),
        ("the key's own glyph", 'l', true, true),
        ("a count-only press", '-', false, true),
    ] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((3, 4)), t0, &c, g, &mut out);
        let key = t0 + ms(100);
        if glyph {
            glow.note_typed_expected(key, 1, false, rk::TypedClass::Glyph, 'l');
        } else {
            glow.note_typed(key);
        }
        let echo = key + ms(8);
        row[15] = landing;
        glow.observe_row(3, 16, &row, echo);
        glow.tick(Some((3, 16)), echo, &c, g, &mut out);
        let cols: Vec<u16> = v2_cols(&glow, 3).into_iter().collect();
        if laid {
            assert_eq!(
                last_row(&glow),
                Some(("no-credits", "none", (3, 4), (3, 16))),
                "{label}: {:?}",
                ring_rows(&glow)
            );
            assert_eq!(cols, vec![15], "{label}: the landing and nothing else");
        } else {
            assert_eq!(
                last_row(&glow),
                Some((
                    CursorGlow::DECLINE_PROGRAM_ROW,
                    AdmissionRecord::LICENCE_NONE,
                    (3, 4),
                    (3, 16)
                )),
                "{label}: {:?}",
                ring_rows(&glow)
            );
            assert_eq!(cols, Vec::<u16>::new(), "{label}: nothing laid");
            assert_eq!(
                glow.in_flight_tally().forgotten,
                1,
                "{label}: the press is forgotten, not spent"
            );
        }
        assert_eq!(glow.typed_credits_within(echo), 0, "{label}");
    }
}

// ──────── the landing gate meets the composer's re-wraps ────────

/// The landing gate reads the cell a soft-wrapped caret lays. One `a`: the glyph lands at the
/// row's last text column (3, 17) and the caret alone wraps to the indent (4, 2); the re-anchor
/// lays the origin ([`CursorGlow::soft_wrapped_caret`]), so the gate reads the origin. The key's
/// glyph there lays exactly (3, 17); a foreign glyph is refused `program-row` and the press
/// forgotten; a count-only press lays the origin. With the origin row rewritten left of the
/// origin (zle switching to an older two-line entry) the move is no soft wrap and is refused.
#[test]
fn a_soft_wrapped_re_anchor_is_witnessed_at_the_origin_it_lays() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut before: Vec<char> = "> ".chars().collect();
    before.resize(17, 'x');
    before.push(' ');
    for (label, origin, glyph, rewritten, laid) in [
        ("the key's own glyph", 'a', true, false, true),
        ("foreign", '_', true, false, false),
        ("a count-only press", 'a', false, false, true),
        ("the prefix rewritten", 'a', true, true, false),
    ] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.observe_row(3, 17, &before, t0);
        glow.observe_neighbor_rows(Some(&[]), Some(&[]));
        glow.tick(Some((3, 17)), t0, &c, g, &mut out);
        let key = t0 + ms(100);
        if glyph {
            glow.note_typed_expected(key, 1, false, rk::TypedClass::Glyph, 'a');
        } else {
            glow.note_typed(key);
        }
        let echo = key + ms(8);
        let mut after = before.clone();
        after[17] = origin;
        if rewritten {
            after[5] = 'y';
        }
        glow.observe_row(4, 2, &[' ', ' '], echo);
        glow.observe_neighbor_rows(Some(&after), Some(&[]));
        glow.tick(Some((4, 2)), echo, &c, g, &mut out);
        let row3: Vec<u16> = v2_cols(&glow, 3).into_iter().collect();
        let row4: Vec<u16> = v2_cols(&glow, 4).into_iter().collect();
        let refused = last_row(&glow).is_some_and(|r| r.0 == CursorGlow::DECLINE_PROGRAM_ROW);
        if laid {
            assert!(!refused, "{label}: {:?}", ring_rows(&glow));
            assert_eq!(row3, vec![17], "{label}: the key's own cell, at the origin");
            assert_eq!(row4, Vec::<u16>::new(), "{label}: nothing on the indent");
            assert_eq!(glow.in_flight_tally().forgotten, 0, "{label}: spent");
        } else {
            assert_eq!(
                last_row(&glow),
                Some((
                    CursorGlow::DECLINE_PROGRAM_ROW,
                    AdmissionRecord::LICENCE_NONE,
                    (3, 17),
                    (4, 2)
                )),
                "{label}: {:?}",
                ring_rows(&glow)
            );
            assert_eq!(
                (row3, row4),
                (Vec::new(), Vec::new()),
                "{label}: nothing laid"
            );
            assert_eq!(
                glow.in_flight_tally().forgotten,
                1,
                "{label}: the press is forgotten, not spent"
            );
        }
        assert_eq!(glow.typed_credits_within(echo), 0, "{label}");
    }
}

/// A rejected visible hop must not claim the row a later hidden-caret echo
/// uses to identify the hand: `last_licensed_row` is restored to its value
/// from before the tentative write.
#[test]
fn a_foreign_landing_restores_the_prior_licensed_hand_row() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut before: Vec<char> = "> ".chars().collect();
    before.resize(17, 'x');
    before.push(' ');
    glow.observe_row(3, 17, &before, t0);
    glow.observe_neighbor_rows(Some(&[]), Some(&[]));
    glow.tick(Some((3, 17)), t0, &c, g, &mut out);

    // The input row was established by an earlier licensed echo. Its clock
    // remains fresh through this foreign move and the next hidden frame.
    glow.last_licensed_row = Some((3, t0));
    let key = t0 + ms(100);
    glow.note_typed_expected(key, 1, false, rk::TypedClass::Glyph, 'a');
    let echo = key + ms(8);
    let mut after = before;
    after[17] = '_'; // zle's foreign caret glyph, not the typed `a`.
    glow.observe_row(4, 2, &[' ', ' '], echo);
    glow.observe_neighbor_rows(Some(&after), Some(&[]));
    glow.tick(Some((4, 2)), echo, &c, g, &mut out);

    assert_eq!(
        last_row(&glow).map(|row| row.0),
        Some(CursorGlow::DECLINE_PROGRAM_ROW),
        "precondition: the visible hop really was rejected"
    );
    assert_eq!(glow.last_licensed_row, Some((3, t0)));
    let hidden = echo + ms(8);
    glow.tick(None, hidden, &c, g, &mut out);
    assert_eq!(
        glow.insert_row(hidden),
        Some(3),
        "a hidden-caret echo still belongs to the prior licensed input row"
    );
}

/// A lifted word lays no landing, so the landing gate stands down. In a bordered composer a Space
/// lets `already` on row 4 fit row 3, and the caret stays at (4, 1): a same-row typed retreat
/// whose `landing − 1` is `│`. The move is judged, not refused, its press spent, and nothing is
/// laid under the caret.
#[test]
fn a_lifted_word_is_not_refused_by_the_landing_gate() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let text = |s: &str| -> Vec<char> { s.chars().collect() };
    let row3_before = text("│the text");
    let row4_before = text("│already[Image #1]");
    let row3_after = text("│the text already");
    let row4_after = text("│[Image #1]");
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.observe_row(4, 8, &row4_before, t0);
    glow.observe_neighbor_rows(Some(&row3_before), Some(&[]));
    glow.tick(Some((4, 8)), t0, &c, g, &mut out);
    let key = t0 + ms(100);
    glow.note_typed_expected(key, 1, false, rk::TypedClass::Space, ' ');
    let echo = key + ms(8);
    glow.observe_row(4, 1, &row4_after, echo);
    glow.observe_neighbor_rows(Some(&row3_after), Some(&[]));
    glow.tick(Some((4, 1)), echo, &c, g, &mut out);
    let rows = ring_rows(&glow);
    assert!(
        rows.last()
            .is_some_and(|r| r.0 != CursorGlow::DECLINE_PROGRAM_ROW
                && r.2 == (4, 8)
                && r.3 == (4, 1)),
        "the lift is judged, not refused: {rows:?}"
    );
    assert_eq!(glow.in_flight_tally().forgotten, 0, "spent, not forgotten");
    assert_eq!(glow.typed_credits_within(echo), 0, "the Space is paid");
    assert!(
        !v2_cols(&glow, 4).contains(&0),
        "nothing under the border left of the caret"
    );
}

/// A flushed park is witnessed by the glyph it was held over (zsh's Ctrl-R walking back along its
/// row). Flushed past the window, with no probe from that frame, the gate reads
/// [`HeldPark::landing_cell`]: a foreign glyph is declined `program-row` and the press forgotten;
/// the key's own glyph lays exactly the landing; a blank (Ink's park to the input's start) is
/// licensed with the landing withheld ([`HeldPark::landing_glyph`]); a count-only press lays it.
#[test]
fn a_flushed_park_is_witnessed_by_the_glyph_it_was_held_over() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut row: Vec<char> = "p% ".chars().collect();
    row.resize(24, 'x');
    for (label, landing, glyph, refused, laid) in [
        ("foreign", 'l', true, true, false),
        ("the key's own glyph", 'o', true, false, true),
        ("a blank", ' ', true, false, false),
        ("a count-only press", 'l', false, false, true),
    ] {
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((3, 20)), t0, &c, g, &mut out);
        let key = t0 + ms(100);
        if glyph {
            glow.note_typed_expected(key, 1, false, rk::TypedClass::Glyph, 'o');
        } else {
            glow.note_typed(key);
        }
        let echo = key + ms(8);
        row[13] = landing;
        glow.observe_row(3, 14, &row, echo);
        glow.tick(Some((3, 14)), echo, &c, g, &mut out);
        assert!(glow.held_park.is_some(), "{label}: held as a park");
        let quiet = echo + past_park_window();
        glow.tick(Some((3, 14)), quiet, &c, g, &mut out);
        assert_eq!(glow.in_flight_tally().park_flushed, 1, "{label}: flushed");
        let cols: Vec<u16> = v2_cols(&glow, 3).into_iter().collect();
        if refused {
            assert_eq!(
                last_row(&glow),
                Some((
                    CursorGlow::DECLINE_PROGRAM_ROW,
                    AdmissionRecord::LICENCE_NONE,
                    (3, 20),
                    (3, 14)
                )),
                "{label}: {:?}",
                ring_rows(&glow)
            );
            assert_eq!(
                glow.in_flight_tally().forgotten,
                1,
                "{label}: the press is forgotten, not spent"
            );
        } else {
            assert_eq!(
                last_row(&glow).map(|r| (r.1, r.2, r.3)),
                Some((AdmissionRecord::LICENCE_KEY, (3, 20), (3, 14))),
                "{label}: {:?}",
                ring_rows(&glow)
            );
            assert_eq!(glow.in_flight_tally().forgotten, 0, "{label}: spent");
        }
        let want: Vec<u16> = if laid { vec![13] } else { Vec::new() };
        assert_eq!(cols, want, "{label}: the landing, or nothing");
        assert_eq!(glow.typed_credits_within(quiet), 0, "{label}");
    }
}

/// A recall at the next key leaves that key's hand on its row ([`TornLay`]). Shape: a typed lay
/// made with the hand off its row (the print ends on the row below), the caret walked back onto
/// the lay, and the next key's echo before the caret rested.
#[test]
fn a_recall_at_the_next_key_leaves_that_key_s_hand_on_its_row() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let row = |s: &str| -> Vec<char> { s.chars().collect() };
    let t0 = Instant::now();
    glow.observe_row(3, 5, &row("p%   "), t0);
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    let k1 = t0 + ms(200);
    glow.note_typed_expected(k1, 1, false, rk::TypedClass::Glyph, 'a');
    let e1 = k1 + ms(8);
    glow.observe_print_anchor(Some((3, 6, 1)));
    glow.observe_row(3, 6, &row("p%   a "), e1);
    glow.tick(Some((3, 6)), e1, &c, g, &mut out);
    assert!(glow.torn_lay.is_some(), "precondition: the lay waits");
    let back = e1 + ms(8);
    glow.observe_print_anchor(Some((4, 20, 2)));
    glow.observe_row(3, 5, &row("p%   a "), back);
    glow.tick(Some((3, 5)), back, &c, g, &mut out);
    let k2 = back + ms(8);
    glow.note_typed_expected(k2, 1, false, rk::TypedClass::Glyph, 'b');
    let e2 = k2 + ms(4);
    glow.observe_print_anchor(Some((3, 6, 3)));
    glow.observe_row(3, 6, &row("p%   b "), e2);
    glow.tick(Some((3, 6)), e2, &c, g, &mut out);
    assert!(
        ring_rows(&glow)
            .iter()
            .any(|r| r.1 == AdmissionRecord::LICENCE_RECALL),
        "precondition: recalled at the next key: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(
        glow.hand_row,
        Some(HandRow::at(3)),
        "{:?}",
        ring_rows(&glow)
    );
}

/// A recall judged over no live cell of the lay ([`TornLay`]) retracts nothing, so it hands back
/// no hand and logs no `licence=recall` (the engine may lay none of a typed lay's cells, e.g. for
/// a key held for its echo).
#[test]
fn a_lay_with_no_live_cell_is_not_recalled() {
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    glow.tick(Some((3, 5)), t0, &c, wide_geom(), &mut out);
    assert!(glow.v2.engaged(), "precondition: the ribbon is live");
    glow.hand_row = Some(HandRow::at(3));
    let lay = TornLay {
        row: 3,
        col0: 5,
        col1: 6,
        seen: Some((3, 5)),
        moved_at: t0,
        hand_before: None,
        anchor_seq: Some(1),
        printed_off_row: true,
    };
    glow.recall_torn_lay(lay, (3, 5), t0 + ms(40));
    assert_eq!(glow.hand_row, Some(HandRow::at(3)));
    assert!(
        ring_rows(&glow)
            .iter()
            .all(|r| r.1 != AdmissionRecord::LICENCE_RECALL),
        "{:?}",
        ring_rows(&glow)
    );
}
