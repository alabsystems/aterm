// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE SEAM'S LICENCE-CLASS LAWS**: one-shots spend once, a glyph
//! supersedes nothing whose echo is still owed, one kill is one `Kill`, a
//! held park is a wake source. Each test is the minimal repro of a shape a
//! user saw — a laggy link, Claude Code's composer, a stalled app — and
//! keeps its RED line: what the glass showed without the law.

use super::*;

/// The last ring row, in `ring_rows`' order: `(reason, licence, origin,
/// target)` — `("licensed", "key", ..)` for a lit key.
fn last_row(glow: &CursorGlow) -> Option<RingRow> {
    ring_rows(glow).last().copied()
}

// ──────── the tick: wake sources and guards ────────

/// Under `classic` the engine returns before `poof_scan`, the only place
/// the erase licences expire, so the Classic branch drops the licences it
/// will never consume: one Backspace or kill leaves no `is_active()` and no
/// 40 ms deadline armed. RED without the drop: a permanent ~25 Hz wake loop
/// for the rest of the focused session.
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

/// While the display temperature the metal chases (`disp_t`, a 1.5 s
/// attack) is above the visibility threshold the ember is a pending visible
/// change, and both wake predicates say so. RED on `cursor_temp >
/// FORGE_MIN_TEMP` alone: after a Fire strike whose moving light dies first
/// the host is told to sleep ~60 ms before the ember crosses the threshold
/// upward, with nobody to draw it.
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
    // The non-vacuity witness (the final review, 2026-09-14): the frames on
    // which the metal was still HEATING toward a `disp_t` above the
    // threshold — the arc the assertion is about. Without it a strike that
    // never heated would leave `slept_hot` empty and the pin green.
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

/// A held park schedules its own flush: `is_active` and
/// `next_change_deadline` read it. RED without either: a deadline-driven
/// host sleeps with the park held and its "≤ 0.25 s later" verdict comes at
/// the next event, at the park's frozen clock.
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

/// The tick's degenerate-geometry guard covers rows and columns beside
/// `cw`/`ch`, in lockstep with the v2 engage gate. RED without it: on a
/// 0-row or 0-column grid the effects box is empty and the first settling
/// star hits `clamp_to_glass`'s `clamp` with `min > max`.
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

/// The Backspace quench one-shot is taken by ANY move it paired with;
/// `typing` gates the deletion class alone. RED when only a `typing`
/// (dist ≤ 1) move consumes it: one Backspace licenses every program move
/// for the next 250 ms — three relocations, three `licence=key` spawns (a
/// jump streak under Lumen / Laser, a park-then-rewrite pair in Claude Code
/// on every Backspace).
#[test]
fn a_backspace_licenses_one_move_whatever_its_shape() {
    let g = Geom {
        rows: 24,
        cols: 80,
        ..wide_geom()
    };
    for style in [GlowStyle::Lumen, GlowStyle::RainbowKitty, GlowStyle::Fire] {
        let c = cfg(style, true);
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((10, 20)), t0 + ms(1000), &c, g, &mut out);
        glow.note_backspace_erasing(t0 + ms(2000), Some(1));
        // A two-row program relocation inside the quench window: the one
        // move the Backspace licenses.
        glow.tick(Some((8, 20)), t0 + ms(2020), &c, g, &mut out);
        let spawns = glow.spawns();
        assert_eq!(spawns, 1, "{style:?}: the paired move is licensed");
        assert!(
            !glow.move_licensed(t0 + ms(2021)),
            "{style:?}: the quench one-shot is spent by the move it paired with"
        );
        // A second program relocation, still inside the window.
        glow.tick(Some((5, 60)), t0 + ms(2100), &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| (r.0, r.1)),
            Some(("no-fresh-hint", "none")),
            "{style:?}: a keyless relocation after the spent quench is refused: {:?}",
            last_row(&glow)
        );
        assert_eq!(glow.spawns(), spawns, "{style:?}: nothing more flies");
    }
}

/// The composer newline (`newline_hint`, Shift+Enter) is consumed once at
/// the spawn seam, mirroring the Return's take. RED when peeked and never
/// consumed: one chord licenses every move for 250 ms — a keyless +1 flies
/// a `Licence::Return` meteor under RainbowKitty, a keyless relocation a
/// 1965-quad jump under Lumen.
#[test]
fn a_composer_newline_licenses_its_own_row_change_and_nothing_after() {
    let g = Geom {
        rows: 24,
        cols: 80,
        ..wide_geom()
    };
    for style in [GlowStyle::Lumen, GlowStyle::RainbowKitty] {
        let c = cfg(style, true);
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        glow.note_context(true);
        let t0 = Instant::now();
        glow.tick(Some((10, 20)), t0 + ms(1000), &c, g, &mut out);
        // The host's Shift+Enter arm: a typed stamp beside the newline hint.
        glow.note_typed(t0 + ms(2000));
        glow.note_newline_break(t0 + ms(2000));
        // The composer's own row change: licensed (and, under Rainbow
        // Kitty, a Return meteor of its own).
        glow.tick(Some((11, 0)), t0 + ms(2020), &c, g, &mut out);
        let spawns = glow.spawns();
        assert_eq!(spawns, 1, "{style:?}: the newline's row change is licensed");
        let meteors = glow.v2_status().map(|s| s.meteors);
        // A keyless +1, then a keyless relocation, both inside 250 ms.
        glow.tick(Some((11, 1)), t0 + ms(2100), &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| (r.0, r.1)),
            Some(("no-fresh-hint", "none")),
            "{style:?}: a keyless +1 after the newline is refused: {:?}",
            last_row(&glow)
        );
        glow.tick(Some((5, 60)), t0 + ms(2200), &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| (r.0, r.1)),
            Some(("no-fresh-hint", "none")),
            "{style:?}: a keyless relocation after the newline is refused: {:?}",
            last_row(&glow)
        );
        assert_eq!(glow.spawns(), spawns, "{style:?}: nothing more flies");
        assert!(
            glow.v2_status().map(|s| s.meteors) <= meteors,
            "{style:?}: no Return meteor for a keyless move ({:?} after {meteors:?})",
            glow.v2_status().map(|s| s.meteors)
        );
    }
}

/// The seam's typed `Sweep` needs typed pairing, never the geometric
/// `mv.typing` (dist ≤ 1) alone: an arrow's hop keeps its wake, a
/// quench/kill-licensed keyless +1 lays nothing. RED on `mv.typing` alone:
/// the first → after a word extends the word's cohort with a full typing
/// cell, and a Backspace or a moving kill followed by a keyless program +1
/// (a spinner, a status redraw) lights it as typed.
///
/// (Merge note, 0.86: the law above is upstream's statement of it and
/// supersedes the candidate's longer narration of the same claim — upstream
/// dropped the `property-fuzz-*` tag from all nine tests in this file and
/// leads each with law-then-red-proof. Nothing the candidate asserted is
/// gone; what follows is the 0.86 reasoning, which upstream has no
/// counterpart for.)
///
/// **THE BOUND IT READS WAS RESTATED FOR THE 0.86 CANDIDATE** (the Rainbow
/// Path v3 merge, 2026-09-14). The pin read the word's WHOLE bounds,
/// `(col0, col1)` — and `col0` is not the arrow's to move. A mark that is
/// LEAVING reaches backwards for its four letters, one cell per
/// [`rk::ribbon::REACH_STEP_S`] beat ([`rk::ribbon::FOUR_LETTER_CELLS`],
/// laid by the exit swoosh, not by any key), and that reach widens the
/// cohort's left bound by design. v3 derives the grace from the melody's
/// phrase rest ([`rk::ribbon::PHRASE_REST_MIN_S`], 0.90 s) where main read
/// a `LIFT_GRACE_S` literal of 0.75, so the reach window moved from idle
/// `[0.75, 0.90)` to `[0.90, 1.05)` — and this test's own cadence (last
/// key at +1100 ms, first probe frame at +2010 ms: idle 0.910 s) now lands
/// INSIDE it, where on main it landed 10 ms past its end. Nothing about
/// the arrow changed.
///
/// Measured, not reasoned (2026-09-14): with the caret PARKED and no
/// `note_navigation` at all, the same frame lays the same cell — row 3,
/// column 9, the word's own cohort, `typing: false`, born and priced from
/// the word's key — and `col0` moves 10 → 9 exactly as it does here; and
/// with the same four keyless +1s delivered INSIDE the rest the word's
/// bounds do not move at all. So the pin reads the bound the hand's side
/// owns — `col1`, the one a rightward hop would extend — and the word
/// cohort's own cells besides: nothing at or past the hop column joined
/// it, its only typed cell is still the seating key's, and anything that
/// did join is left of the word's own left bound and is not typed. A
/// keyless +1 that painted a typed cell, or that attached ANY cell at the
/// hop, still fails here.
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
        // THE HAND'S SIDE of the word is untouched. The +1 hops RIGHTWARD,
        // so anything it attached to the word extends `col1`; `col0` is the
        // exit swoosh's to move, backwards, and only ever backwards.
        assert_eq!(
            word_now.col1, word.col1,
            "{key}: the word's right bound — the one a forward hop would extend — is untouched"
        );
        assert!(
            word_now.col0 <= word.col0,
            "{key}: the word's left bound only ever reaches backwards ({} after {})",
            word_now.col0,
            word.col0
        );
        // NOTHING THE ARROW CROSSED JOINED THE WORD: every cell of the
        // word's own cohort still sits left of the hop column, its only
        // typed cell is the seating key's, and whatever did join is the
        // swoosh's backward reach — left of the word's own left bound, and
        // never typed.
        let joined: Vec<(u16, bool)> = rib
            .cells()
            .iter()
            .filter(|c| c.cohort == word.id)
            .map(|c| (c.col, c.typing))
            .collect();
        assert!(
            joined.iter().all(|&(col, _)| col < 11),
            "{key}: a keyless +1 attached a cell to the word at the hop: {joined:?}"
        );
        assert_eq!(
            joined
                .iter()
                .filter(|&&(_, typing)| typing)
                .map(|&(col, _)| col)
                .collect::<Vec<_>>(),
            vec![10],
            "{key}: the word's only typed cell is its own key's: {joined:?}"
        );
        assert!(
            joined
                .iter()
                .all(|&(col, typing)| typing || col < word.col0),
            "{key}: an untyped cell joined the word inside its span: {joined:?}"
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
                (hop[0].life_s - rk::ribbon::WAKE_LIFE_S * rk::ribbon::WAKE_HOP_LIFE_SHARE).abs()
                    < 1e-6
            );
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
/// press too, read BEFORE the classifier spends the pool — a stamp alone is
/// not a key's echo: a coalesced `+3` spends all three presses but pops one
/// stamp, leaving two dangling for `TYPE_HINT_FRESH`, and a fresh Enter
/// would then let each keyless same-row `+1` (a form field, a prompt
/// redrawing to the right) through as a glyph's echo — the Return not
/// consumed, the licence mapped to `Typed` on the dangling stamp, the seam
/// sweeping a program cell as `licensed key` per stamp, the Return finally
/// spent on the `+1` after the last one. RED before the fix: `+1` #1
/// `("licensed","key",(3,9),(3,10))` with the Return still fresh, cell 9
/// lit; the class-blind take lights cell 8 alone and refuses #1/#2
/// `no-fresh-hint`.
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
    // Return's own (spent there), the rest are program output on a stamp
    // no press stands behind.
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

/// A KEYLESS WIDE HOP UNDER A ONE-SHOT LICENCE LIGHTS NO LANDING ON A
/// DANGLING STAMP: `seam_licensed` refuses a keyless hop when only a paid
/// stamp is fresh, but a one-shot class (Enter, a Tab gesture) passes it for
/// the same hop; the dangling stamp makes it `typed_pair`, the width makes
/// it a `re_anchor`, and only `re_anchor_lays_landing`'s credit conjunct
/// keeps the seam from sweeping `cc-1..cc`. RED with the conjunct removed:
/// `("no-credits","none",(3,8),(3,40))` and cell 39 lit, on both arms.
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
        // A keyless +32 on the row inside both windows: licensed by the
        // one-shot class, judged a typed re-anchor, refused by the share
        // rule — and dark.
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

/// The one-shots survive a typed press (their echoes are still owed, the
/// law that keeps the typed stamps), a Return-paired move neither
/// re-anchors nor pops the glyph's stamp, a Return / Nav move's forget edge
/// forgets only the presses banked at or before the one-shot, and the
/// engine replays a `Typed` buffered before a one-shot's move against the
/// caret BEFORE that move. Shape: a slow prompt (SSH, a starship / p10k
/// precmd, Claude Code's measured p99 input latency of 268 ms) lands the
/// Enter's response after the glyph's dispatch. RED with the one-shots
/// dropped by the typed press: a one-row Return response is a typed
/// re-anchor that lays the landing cell LEFT of the new caret (the prompt's
/// cell), spends the glyph's credit and pops its stamp, and the glyph's own
/// +1 is refused dark; with output rows or a short hop the glyph lights via
/// the pool but v2 replays the buffered `Typed` against the post-move caret
/// and lights the prompt's cell — a phantom on every fast Enter → glyph.
#[test]
fn a_glyph_typed_before_the_enters_response_lands_keeps_its_credit_and_its_cell() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    // The response's shape: a one-row hop to the prompt (a command with
    // no output), the prompt three rows down (output rows), and a hop of
    // |dc| = 1 (a `cd`-length command).
    for (label, response) in [
        ("one-row response", (28u16, 2u16)),
        ("output rows", (29, 2)),
        ("|dc| = 1", (28, 4)),
    ] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let pre = pre_roll(&mut glow, 27, t0, &c, g);
        // Enter — the host: supersede (keeps the typed stamps) + note_return.
        let enter = pre + ms(200);
        glow.supersede_typed_press();
        glow.note_return(enter);
        // The next command's first glyph, before the prompt came back.
        let k = enter + ms(120);
        glow.supersede_typed_press();
        glow.note_typed(k);
        let resp = k + ms(30);
        glow.tick(Some(response), resp, &c, g, &mut out);
        let (row, col) = response;
        assert_eq!(
            glow.in_flight_tally().credits,
            1,
            "{label}: the Enter's response spends nothing of the glyph's: {:?}",
            last_row(&glow)
        );
        assert!(
            !v2_cols(&glow, row).contains(&(col - 1)),
            "{label}: the response lit the prompt's cell ({row}, {}): {:?}",
            col - 1,
            v2_cols(&glow, row)
        );
        // The glyph's own echo.
        let echo = resp + ms(20);
        glow.tick(Some((row, col + 1)), echo, &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| r.0),
            Some("licensed"),
            "{label}: the glyph's +1 is licensed: {:?}",
            last_row(&glow)
        );
        assert!(
            v2_cols(&glow, row).contains(&col),
            "{label}: the glyph's cell ({row}, {col}) is lit: {:?}",
            v2_cols(&glow, row)
        );
        assert!(
            !v2_cols(&glow, row).contains(&(col - 1)),
            "{label}: the prompt's cell ({row}, {}) is dark — nobody typed it: {:?}",
            col - 1,
            v2_cols(&glow, row)
        );
        assert_eq!(
            glow.in_flight_tally().credits,
            0,
            "{label}: the glyph's credit is spent"
        );
        // A keyless program +1 after that is refused.
        let spawns = glow.spawns();
        glow.tick(Some((row, col + 2)), echo + ms(400), &c, g, &mut out);
        assert_eq!(glow.spawns(), spawns, "{label}: a keyless +1 is refused");
    }
}

/// The Claude Code shape of the same law: alt screen, Enter submits, the
/// composer clears back to its inset column on the SAME row, and the next
/// glyph was typed before that clear landed. RED: the clear is HELD as a
/// park under the glyph's credit and flushed by the glyph's own +1, which
/// is refused `no-fresh-hint` with a phantom at the `> ` prompt's cell.
#[test]
fn a_glyph_typed_before_the_composer_clears_keeps_its_cell() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let enter = pre + ms(200);
    glow.supersede_typed_press();
    glow.note_return(enter);
    let k = enter + ms(120);
    glow.supersede_typed_press();
    glow.note_typed(k);
    let resp = k + ms(30);
    glow.note_repaint_blink(resp);
    glow.tick(Some((27, 2)), resp, &c, g, &mut out);
    let echo = resp + ms(40);
    glow.note_repaint_blink(echo);
    glow.tick(Some((27, 3)), echo, &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| r.0),
        Some("licensed"),
        "the glyph's +1 is licensed: {:?}",
        last_row(&glow)
    );
    glow.tick(Some((27, 3)), echo + ms(300), &c, g, &mut out);
    assert!(
        v2_cols(&glow, 27).contains(&2),
        "the glyph's cell (27, 2) is lit: {:?}",
        v2_cols(&glow, 27)
    );
    assert!(
        !v2_cols(&glow, 27).contains(&1),
        "the prompt's cell (27, 1) is dark: {:?}",
        v2_cols(&glow, 27)
    );
    assert_eq!(glow.in_flight_tally().credits, 0);
}

/// The Backspace twin: a glyph typed before the Backspace's retreat lands
/// (SSH). RED with the quench dropped by the typed press: the retreat is
/// parked, the glyph's +1 is the park's "same-end return" and dropped
/// silently, the glyph's credit is never spent, and a keyless +1 up to 10 s
/// later lights as `inflight`.
#[test]
fn a_glyph_typed_before_the_backspaces_retreat_lands_spends_its_credit() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    let bs = pre + ms(200);
    glow.note_backspace_erasing(bs, Some(1));
    let k = bs + ms(100);
    glow.supersede_typed_press();
    glow.note_typed(k);
    let retreat = k + ms(30);
    glow.tick(Some((27, 4)), retreat, &c, g, &mut out);
    let echo = retreat + ms(20);
    glow.tick(Some((27, 5)), echo, &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| r.0),
        Some("licensed"),
        "the glyph's +1 is licensed: {:?}",
        last_row(&glow)
    );
    assert!(
        v2_cols(&glow, 27).contains(&4),
        "the glyph's cell is lit: {:?}",
        v2_cols(&glow, 27)
    );
    glow.tick(Some((27, 5)), echo + ms(300), &c, g, &mut out);
    assert_eq!(
        glow.in_flight_tally().credits,
        0,
        "the glyph's credit is spent by its echo"
    );
    let spawns = glow.spawns();
    glow.tick(Some((27, 6)), echo + ms(1800), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        spawns,
        "a keyless +1 1.8 s later is refused: {:?}",
        last_row(&glow)
    );
}

/// The composer newline survives a typed press, its row change is
/// Return-paired at the classifier (never the glyph's re-anchor; the
/// chord's OWN stamp is the one it pops), and its forget edge spares the
/// presses typed after the chord. RED with `newline_hint` dropped by the
/// supersede and `return_paired` reading `return_hint` alone: Shift+Enter
/// then a glyph typed before the line break landed reproduces the
/// re-anchor phantom — the break (one row down, back to the box's inset)
/// is judged under the glyph's fresh stamp, lights the inset's neighbour,
/// spends the glyph's credit and pops its stamp, and the glyph's own +1 is
/// refused dark.
#[test]
fn a_glyph_typed_before_the_composer_newline_lands_keeps_its_credit_and_its_cell() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    let t0 = Instant::now();
    let pre = pre_roll(&mut glow, 27, t0, &c, g);
    // Shift+Enter — the host's arm: a typed stamp beside the newline hint.
    let chord = pre + ms(200);
    glow.supersede_typed_press();
    glow.note_typed(chord);
    glow.note_newline_break(chord);
    // The next line's first glyph, before the break landed.
    let k = chord + ms(120);
    glow.supersede_typed_press();
    glow.note_typed(k);
    let resp = k + ms(30);
    glow.note_repaint_blink(resp);
    glow.tick(Some((28, 2)), resp, &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| r.0),
        Some("licensed"),
        "the break is the newline's own move: {:?}",
        last_row(&glow)
    );
    assert_eq!(
        glow.in_flight_tally().credits,
        1,
        "the break spends nothing of the glyph's: {:?}",
        last_row(&glow)
    );
    assert!(
        glow.type_hint.any_fresh(resp, CursorGlow::TYPE_HINT_FRESH),
        "the glyph's stamp is left for its own echo"
    );
    assert!(
        !v2_cols(&glow, 28).contains(&1),
        "the break lit the inset's neighbour (28, 1): {:?}",
        v2_cols(&glow, 28)
    );
    // The glyph's own echo.
    let echo = resp + ms(20);
    glow.note_repaint_blink(echo);
    glow.tick(Some((28, 3)), echo, &c, g, &mut out);
    assert_eq!(
        last_row(&glow).map(|r| r.0),
        Some("licensed"),
        "the glyph's +1 is licensed: {:?}",
        last_row(&glow)
    );
    assert!(
        v2_cols(&glow, 28).contains(&2),
        "the glyph's cell (28, 2) is lit: {:?}",
        v2_cols(&glow, 28)
    );
    assert!(
        !v2_cols(&glow, 28).contains(&1),
        "the inset's neighbour (28, 1) stays dark — nobody typed it: {:?}",
        v2_cols(&glow, 28)
    );
    assert_eq!(
        glow.in_flight_tally().credits,
        0,
        "the glyph's credit is spent by its echo"
    );
    // A keyless program +1 after that is refused.
    let spawns = glow.spawns();
    glow.tick(Some((28, 4)), echo + ms(400), &c, g, &mut out);
    assert_eq!(glow.spawns(), spawns, "a keyless +1 is refused");
}

/// A caret-moving kill's retreat forgets only the presses banked at or
/// before the kill's key, exactly as the Return's and the arrow's moves do.
/// RED when the retreat's forget edge spares nothing: `note_kill` forgets
/// the pool at the KEY, and a glyph typed after ^W / ^U and before its
/// retreat landed (SSH, a slow prompt) is banked after that forget and
/// dropped again at the retreat — its own +1 then has a fresh stamp with no
/// press behind it and is refused, its cell dark for good.
#[test]
fn a_glyph_typed_before_the_kills_retreat_lands_keeps_its_credit_and_its_cell() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    // ^W takes the last word (`t`), ^U the whole line — the retreat's
    // landing differs, the law does not.
    for (label, word, landing) in [("^W", true, 4u16), ("^U", false, 2)] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let pre = pre_roll(&mut glow, 27, t0, &c, g);
        let kill = pre + ms(200);
        if word {
            glow.note_word_kill(kill, true);
        } else {
            glow.note_kill(kill, true);
        }
        // The next glyph, before the retreat landed.
        let k = kill + ms(100);
        glow.supersede_typed_press();
        glow.note_typed(k);
        let retreat = k + ms(30);
        glow.tick(Some((27, landing)), retreat, &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| (r.0, r.1)),
            Some(("licensed", "key")),
            "{label}: the retreat is the kill's: {:?}",
            last_row(&glow)
        );
        assert_eq!(
            glow.in_flight_tally().credits,
            1,
            "{label}: the retreat forgets nothing of the glyph's"
        );
        // The glyph's own echo.
        let echo = retreat + ms(20);
        glow.tick(Some((27, landing + 1)), echo, &c, g, &mut out);
        assert_eq!(
            last_row(&glow).map(|r| r.0),
            Some("licensed"),
            "{label}: the glyph's +1 is licensed: {:?}",
            last_row(&glow)
        );
        assert!(
            v2_cols(&glow, 27).contains(&landing),
            "{label}: the glyph's cell (27, {landing}) is lit: {:?}",
            v2_cols(&glow, 27)
        );
        assert!(
            !v2_cols(&glow, 27).contains(&1),
            "{label}: the prompt's cell (27, 1) is dark — nobody typed it: {:?}",
            v2_cols(&glow, 27)
        );
        assert_eq!(
            glow.in_flight_tally().credits,
            0,
            "{label}: the glyph's credit is spent by its echo"
        );
        let spawns = glow.spawns();
        glow.tick(Some((27, landing + 2)), echo + ms(400), &c, g, &mut out);
        assert_eq!(
            glow.spawns(),
            spawns,
            "{label}: a keyless +1 is refused: {:?}",
            last_row(&glow)
        );
    }
}

/// A same-row forward hop the in-flight pool pays for is a glyph's echo,
/// never the arrow's (or a moving kill's) move, and the nav hint is left
/// for the arrow's own hop — the Nav twin of `glyph_echo`. Shape: `abcd`
/// then End over a 300 ms link, the echoes draining +2, +2, then the End
/// hop. RED when judged as the arrow's: `nav_paired` vetoes the typed arms,
/// `navigation` consumes the one-shot and the forget edge drops the pool,
/// so the remaining glyph echoes are refused `no-fresh-hint` (dark cells
/// for good) and the arrow's own hop arrives with its hint already spent.
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

    // `ab` then ^W with the echoes late: the +2 is the glyphs' (the kill
    // forgot their pool at the key, so it is a bare licensed hop, never
    // the kill's retreat), and the retreat that follows is the kill's.
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

    // CONTROL: an End hop the pool cannot pay for is still the arrow's —
    // four presses in flight, Home, and a keyless +4 half a second later
    // is refused as before.
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

/// A `Typed` with no move after it in the frame has not been echoed: the
/// ribbon HOLDS it (the row's cohort kept alive, as an erase would) and the
/// key's cell comes with its echo, from the seam's sweep. RED when replayed
/// at the engine's mirror (`Ribbon::lay` lays `caret.col − n`, right only
/// when the key's echo already moved the mirror in the same frame): on a
/// fresh line the first key lights the prompt's trailing space; with the
/// mirror at column 0 (after Enter, after backspacing to col 0, a `cat` /
/// `read` / empty PS1) the fold lights the previous row's LAST cell — a
/// phantom over a blank, which the content witness never arms.
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

/// Bound, don't spend: the presses banked at or before an UNKNOWN-width
/// insert's delivery (a multi-line paste, ⌃V, Tab) could only echo in its
/// hop or the program's next repaint, so they are retired 2 s after the hop
/// was laid (a key whose echo comes the next frame spends its own credit
/// first). RED without the retire: `lay_insert` pays a surplus only beyond
/// the 32-cell bound, so a 17-cell `[Pasted text #1 +3 lines] k` hop spends
/// nothing from the ring, and the queued key's credit lives on for the 10 s
/// patience, licensing the next keyless +1 on the row as `inflight`.
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
    // ONE HOP: the placeholder (16 cells) and the key (1) echo as +17.
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

    // TWO FRAMES: the placeholder echoes as +16, the key as its own +1 a
    // frame later — the key spends its credit and lights `key`.
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

    // CONTROL: the PRICED twin (`Cells(16)` + the key as +17) spends the
    // key at the hop, as before.
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
/// ribbon takes the erase-retreat arm. RED when the retreat reaches the
/// ribbon as a `Typed`-licensed backward move with `pending_erase == 0`:
/// the COMPOSER path's `re_anchor` → `begin_relocate(w = moved_word_len)`
/// relays the killed last word's width UNDER THE PROMPT — at the kill
/// itself under a long prompt (`'d '` of a 44-col zsh prompt), at the
/// `(w+1)`-th later key under a two-cell one (`% `, Claude Code's `❯ `) —
/// and the row's cohort holds those cells lit for as long as the hand types
/// on that row.
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

/// One kill, one `Kill`: `kill_reported_to_v2` is the KEY whose `Kill`
/// went out — the retreat and the poof each emit only for a kill not yet
/// reported, whichever comes first, and a new kill is a new key. RED as a
/// bool set at the retreat and taken by the poof, two double-`Kill` edges:
/// the retreat emits even when the poof already minted the kill's `Kill`
/// (the caret fallback answers after `POOF_FALLBACK_GRACE`, a slow link
/// lands the retreat later), and a second `note_kill` before the first's
/// poof scan resets the flag, so that poof mints the first kill again.
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

    // THE POOF FIRST: ^U over a slow link — the caret fallback answers at
    // the grace with the caret unmoved, the retreat lands 30 ms later.
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

    // A SECOND KILL BEFORE THE FIRST'S POOF SCAN: ^W, its retreat (the
    // host's probe a frame behind), ^W again, the probe, the second
    // retreat. Two kills, two `Kill`s.
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

/// A park from a MID-LINE origin is not the anchored return: `pc ==
/// p.origin` confines the anchored-lane return arm to end-of-line typing,
/// so the tail's re-lay past the row's end flushes the park instead of
/// releasing it — a mid-line key under Ink's park+rewrite must not have
/// `end -> end+1` judged as its echo (the row's tail cell lit for the typed
/// one). This pins only what the conjunct guarantees, not the flush's own
/// verdict for the shape. RED with the conjunct widened: `park_returns` 1,
/// the park released, the tail's re-lay judged
/// `("licensed","key",(5,10),(5,11))`.
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
    // Hidden frame, the print anchor advanced past the OLD end: on an
    // end-of-line origin this is the anchored return; from mid-line it is
    // not.
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

/// v2's `Typed` banks the same bounded width the press credit takes
/// (`RAINBOW_TYPED_SWEEP_MAX`), so a saturated IME/dictation commit
/// (`u16::MAX` cells) cannot lay a ribbon folding up through the whole grid
/// at O(n²). The commit's echo lands in the same frame as its key (a
/// `Typed` with no move after it is HELD, not laid — ticking with the caret
/// unmoved measures one held key whatever its width), so the echo is the
/// cap's worth of cells, the widest hop the seam licenses as one coalesced
/// echo, and `[Typed, Sweep, Move]` reaches `Ribbon::lay`; the commit is
/// measured as the cells it added. RED with the clamp removed: 17 828
/// cells.
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

/// `TypedStamps::split_off_after` and `merge` are index-addressed, the way
/// `retire_stale` and the `PressCredits` twins are: re-banking every stamp
/// through `stamp()`'s first-free-slot scan is n(n+1)/2 slot reads for a
/// full bank, on every held-park flush (an Ink TUI parks and returns per
/// keystroke) — measured at 5-6x the baseline flush at a full bank. The
/// contract the shape must keep: a split and a merge leave the bank
/// exactly as one-by-one banking would — packed oldest-first, the newest
/// `TYPED_STAMP_DEPTH` surviving an overflow — at every fill, including a
/// full bank on both sides of the cut.
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
            // Bank n stamps, the cut after the n_before-th; the split takes
            // the later ones, a judgment consumes `spent` of the kept ones,
            // and the merge puts the later ones back.
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
            // An overflow: the merged bank is full before the later stamps
            // return, so the oldest kept ones fall off — the same ones
            // one-by-one banking drops.
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

/// **THE BAND'S WALK IS THE HAND'S, WHATEVER THE ECHO'S LAG.** A cell's
/// `t` is a function of its COLUMN in its cohort (`Cohort::t_at`, C2: `t0 +
/// walk_t(col − anchor_col)`) — the walk is never read off a clock — and
/// the seam dates a typed sweep at the KEY's clock (the press's stamp for a
/// per-key echo, the OLDEST unpaid press for a stalled batch), which the
/// engine floors at one stamp window before the echo
/// (`rainbow_kitty::SWEEP_BIRTH_FLOOR_S`, so a batch is not born into a cohort
/// that has already faded). The key's own cell always comes from this
/// sweep, under the hold law as before it (a `Typed` a frame early laid
/// its NEIGHBOUR). Pinned at the host seam: thirty keys at 100 ms with
/// every echo 3, 8, 120 or 400 ms late, and the same thirty echoing as ONE
/// hop three seconds after the last press, lay the same `t` on every
/// column; every cell is born at a KEY's clock — its own press inside the
/// stamp window, the oldest press still inside it past that (`peek_fresh`),
/// the floor for the batch — never at its echo.
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
        // Every press and every echo in clock order, one frame each: a
        // press's frame sees no move (the key is held), an echo's frame
        // moves the mirror one cell.
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
                // Past the stamp window the sweep's stamp is the oldest
                // press still inside it (`peek_fresh`): a KEY's clock, a
                // later key's — never the echo's, never before the floor.
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
