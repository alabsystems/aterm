// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Licence-class and wake laws for the cursor glow seam.

use super::*;

fn last_row(glow: &CursorGlow) -> Option<RingRow> {
    ring_rows(glow).last().copied()
}

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
            press_mask: u128::MAX,
            v2_typed_before: 0,
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

#[test]
fn held_presses_pay_only_for_a_move_that_stays_on_the_hand_s_row() {
    let t0 = Instant::now();
    for held in [false, true] {
        let mut glow = CursorGlow {
            hand_row: Some(HandRow::at(5)),
            ..CursorGlow::default()
        };
        glow.type_press_ring.bank(t0, 1, Some('x'));
        if held {
            glow.type_press_ring.hold_through(t0);
        }
        let moves = [(5u16, 5u16), (5, 4), (4, 5), (4, 4)].map(|(from, to)| {
            let inside = glow.judging_on_rows(from, to, |g| g.typed_credits_within(t0));
            (inside, glow.typed_credits_within(t0))
        });
        let nested = glow.judging_on_rows(4, 5, |g| {
            g.judging_on_rows(5, 5, |_| ());
            g.typed_credits_within(t0)
        });
        assert_eq!(
            (moves, nested),
            if held {
                ([(1, 0), (0, 0), (0, 0), (0, 0)], 0)
            } else {
                ([(1, 1); 4], 1)
            },
            "held {held}: (credits for 5→5, 5→4, 4→5, 4→4 with the hand on row 5, each \
             inside its judgment and outside after it; credits for 4→5 once a nested 5→5 ended)"
        );
    }
}

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

#[test]
fn a_one_shot_licenses_its_own_move_and_nothing_after() {
    type OneShotArm = fn(&mut CursorGlow, Instant);
    let backspace: OneShotArm = |glow, t| glow.note_backspace_erasing(t, Some(1));
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
    glow.tick(Some((3, 8)), echo, &c, g, &mut out);
    assert_eq!(
        last_row(&glow),
        Some(("licensed", "key", (3, 5), (3, 8))),
        "{:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.typed_credits_within(echo), 0, "all three spent");
    assert!(
        glow.type_hint.any_fresh(echo, TYPE_HINT_FRESH_DUR),
        "a stamp dangles"
    );
    glow.note_return(echo + ms(10));
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
            glow.type_hint.any_fresh(echo, TYPE_HINT_FRESH_DUR),
            "{name}: a stamp dangles"
        );
        one_shot(&mut glow, echo + ms(10));
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

#[test]
fn a_glyph_typed_before_a_one_shots_move_lands_keeps_its_credit_and_its_cell() {
    type OneShotArm = fn(&mut CursorGlow, Instant);
    let enter: OneShotArm = |glow, t| {
        glow.supersede_typed_press();
        glow.note_return(t);
    };
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
            !paired || glow.type_hint.any_fresh(landed, TYPE_HINT_FRESH_DUR),
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
    glow.tick(Some((3, 12)), t0 + ms(90), &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| (r.0, r.1)),
        Some(("licensed", "key")),
        "the End hop keeps its own licence: {:?}",
        last_row(&glow)
    );

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

#[test]
fn a_key_queued_behind_an_unknown_insert_is_retired_with_the_hop_that_echoed_it() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let arm = |glow: &mut CursorGlow, pre: Instant, width: InsertWidth| -> Instant {
        let k = pre + ms(100);
        glow.note_typed(k);
        glow.revoke_input_hints_at(k);
        let d = k + ms(40);
        glow.note_insert_delivered_from(k - ms(50), d, width);
        glow.note_delivered(d + ms(1), DeliveredClass::Typed);
        d
    };
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

#[test]
fn a_mid_line_park_is_not_released_by_the_tails_re_lay() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
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
                    merged.take_fresh(at(n), Duration::MAX);
                }
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

#[test]
fn the_bands_walk_is_the_hands_whatever_the_echos_lag() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let row = 7_u16;
    let keys = 30_u16;
    let cadence = 100_u64;
    let floor = Duration::from_secs_f32(rk::SWEEP_BIRTH_FLOOR_S);
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
    assert_eq!(
        p.foreign_to_live(t0 + ms(10), 'c'),
        None,
        "a later glyph-less or multi-cell press makes the pool unknown even when an earlier slot matched"
    );
    let mut single_glyphless = PressCredits::default();
    single_glyphless.bank(t0, 1, Some('c'));
    single_glyphless.bank(t0 + ms(5), 1, None);
    assert_eq!(
        single_glyphless.foreign_to_live(t0 + ms(5), 'c'),
        None,
        "order-independent: a later one-cell glyph-less press still returns None"
    );
    let late = t0 + Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S + 1.0);
    assert_eq!(p.foreign_to_live(late, '_'), None, "no live press: unknown");
}

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

    glow.last_licensed_row = Some((3, t0));
    let key = t0 + ms(100);
    glow.note_typed_expected(key, 1, false, rk::TypedClass::Glyph, 'a');
    let echo = key + ms(8);
    let mut after = before;
    after[17] = '_';
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

/// A key that keeps the hand ([`CursorGlow::clear_typed_keeping_hand`]) while the caret still
/// stands at a lay's end ends the lay's wait for the caret's rest once the caret moves: the
/// program's answer that walks the caret back onto the glyph after it is that key's answer, no
/// keyless torn redisplay, and keeps the lay.
#[test]
fn a_caret_that_moved_behind_a_key_keeps_the_lay() {
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
    let key = e1 + ms(8);
    glow.clear_typed_keeping_hand(key);
    let back = key + ms(8);
    glow.observe_print_anchor(Some((4, 20, 2)));
    glow.observe_row(3, 5, &row("p%   a "), back);
    glow.tick(Some((3, 5)), back, &c, g, &mut out);
    glow.tick(Some((3, 5)), back + ms(40), &c, g, &mut out);
    assert!(
        ring_rows(&glow)
            .iter()
            .all(|r| r.1 != AdmissionRecord::LICENCE_RECALL),
        "a caret moved behind a key keeps the lay: {:?}",
        ring_rows(&glow)
    );
}

/// A key that comes once a torn redisplay has walked the caret back, inside its rest, keeps the
/// lay's wait: a caret that rests where the key found it is recalled, and one that moves on is the
/// key's answer and keeps the lay. The recall hands back what the lay took and nothing the key or
/// a frame changed since: the hand it put on its row (a click), less a doubt a navigation key
/// settled (an arrow), and not over a hand a Return took off the row or a doubt parked elsewhere.
#[test]
fn a_key_after_the_walk_back_keeps_the_recall_and_hands_back_only_the_lay_s_own() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let row = |s: &str| -> Vec<char> { s.chars().collect() };
    let doubt = |row: u16, parked: bool| Some(UnownedCaret { row, parked });
    for leg in ["click", "arrow", "return", "parked elsewhere", "moves on"] {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        glow.observe_row(3, 5, &row("p%   "), t0);
        glow.tick(Some((3, 5)), t0, &c, g, &mut out);
        let k1 = t0 + ms(200);
        glow.note_typed_expected(k1, 1, false, rk::TypedClass::Glyph, 'a');
        let e1 = k1 + ms(8);
        glow.observe_print_anchor(Some((3, 6, 1)));
        glow.observe_row(3, 6, &row("p%   a "), e1);
        glow.tick(Some((3, 6)), e1, &c, g, &mut out);
        let lay = glow.torn_lay.as_mut().expect("precondition: the lay waits");
        // What the lay saved before it took the row: another row's hand and a parked doubt on
        // this one, or (`parked elsewhere`) a doubt on another row that a frame parks meanwhile.
        lay.hand_before = Some(HandRow::at(7));
        lay.unowned_before = if leg == "parked elsewhere" {
            doubt(9, false)
        } else {
            doubt(3, true)
        };
        if leg == "parked elsewhere" {
            glow.unowned_caret = doubt(9, true);
        }
        let back = e1 + ms(8);
        glow.observe_print_anchor(Some((4, 20, 2)));
        glow.observe_row(3, 5, &row("p%   a "), back);
        glow.tick(Some((3, 5)), back, &c, g, &mut out);
        let key = back + ms(8);
        match leg {
            "arrow" => glow.note_motion(key),
            "return" => glow.note_return(key),
            _ => glow.clear_typed_keeping_hand(key),
        }
        let rest = back + ms(40);
        if leg == "moves on" {
            glow.observe_row(3, 4, &row("p%   a "), key + ms(8));
            glow.tick(Some((3, 4)), key + ms(8), &c, g, &mut out);
            glow.tick(Some((3, 4)), rest + ms(40), &c, g, &mut out);
            assert!(
                ring_rows(&glow)
                    .iter()
                    .all(|r| r.1 != AdmissionRecord::LICENCE_RECALL),
                "a caret the key moves on keeps the lay: {:?}",
                ring_rows(&glow)
            );
            continue;
        }
        glow.tick(Some((3, 5)), rest, &c, g, &mut out);
        assert!(
            ring_rows(&glow)
                .iter()
                .any(|r| r.1 == AdmissionRecord::LICENCE_RECALL),
            "{leg}: a caret that rests where the key found it is recalled: {:?}",
            ring_rows(&glow)
        );
        let (hand, doubt) = match leg {
            "click" => (Some(HandRow::at(7)), Some((3, true))),
            "arrow" => (Some(HandRow::at(7)), None),
            "return" => (None, None),
            _ => (Some(HandRow::at(7)), Some((9, true))),
        };
        assert_eq!(
            (glow.hand_row, glow.unowned_caret.map(|u| (u.row, u.parked))),
            (hand, doubt),
            "{leg}: the recall hands back the lay's own and keeps what came since"
        );
    }
}

/// A lift of the hand's line while a lay waits ([`CursorGlow::lift_hand_row`]) carries what the
/// lay saved of that line with it, as it carries the live hand and doubt, so the recall's doubt
/// stays on the lay's own row; what it saved of the line the lift replaced goes, and saved state
/// on any other row stays where it is.
#[test]
fn a_lift_during_the_wait_carries_the_lay_s_saved_hand_and_doubt() {
    let t0 = Instant::now();
    for (hand, doubt, lifted) in [
        (4, 4, (Some(HandRow::at(3)), Some((3, true)))),
        (3, 3, (None, None)),
        (7, 9, (Some(HandRow::at(7)), Some((9, true)))),
    ] {
        let mut glow = CursorGlow {
            torn_lay: Some(TornLay {
                row: 4,
                col0: 5,
                col1: 6,
                seen: Some((4, 5)),
                moved_at: t0,
                hand_before: Some(HandRow::at(hand)),
                unowned_before: Some(UnownedCaret {
                    row: doubt,
                    parked: true,
                }),
                anchor_seq: None,
                printed_off_row: true,
                keyed: false,
            }),
            ..CursorGlow::default()
        };
        glow.lift_hand_row(4, 3);
        let lay = glow.torn_lay.expect("a lift keeps the lay");
        assert_eq!(
            (lay.row, lay.seen),
            (3, Some((3, 5))),
            "precondition: the lift moves the lay"
        );
        assert_eq!(
            (
                lay.hand_before,
                lay.unowned_before.map(|u| (u.row, u.parked))
            ),
            lifted,
            "saved on rows {hand}/{doubt}: the lift moves its line's and drops the replaced line's"
        );
    }
}

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
        unowned_before: None,
        anchor_seq: Some(1),
        printed_off_row: true,
        keyed: false,
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
