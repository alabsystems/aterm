// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed stamps across gestures, hidden carets and program rows.

use super::*;

/// Sparkle's glitter train survives the next keystroke: an unlicensed batch
/// reaches no teardown and a licensed one mints without clearing. Shed a train
/// over real `geom()` cell centres, type the next key, and count what is left.
#[test]
fn the_sparkle_glitter_train_outlives_the_keystroke_that_follows_it() {
    let g = geom();
    let c = cfg(GlowStyle::Sparkle, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut row = [' '; 16];
    row[..6].fill('s');
    glow.observe_row(2, 6, &row, t0);
    let t1 = t0 + Duration::from_millis(8);
    glow.tick(Some((2, 6)), t1, &c, g, &mut out);
    // Glitter over the typed cells (columns 2..6), each grain born inside its
    // cell as `spawn_burst_particles` places it.
    for col in 2..6u16 {
        let (cx, cy) = g.cell_center(2, col);
        glow.particles.push(Particle {
            x0: cx,
            y0: cy,
            vx: 0.0,
            vy: 0.0,
            gy: 0.0,
            life: 5.0,
            hue: 0.5,
            cov_scale: 1.0,
            born: t1,
        });
    }
    // …and one grain over a cell this batch is about to rewrite.
    let (rx, ry) = g.cell_center(2, 9);
    glow.particles.push(Particle {
        x0: rx,
        y0: ry,
        vx: 0.0,
        vy: 0.0,
        gy: 0.0,
        life: 5.0,
        hue: 0.5,
        cov_scale: 1.0,
        born: t1,
    });
    // The next key: a licensed typed advance whose batch also rewrites the cell one of the
    // grains is flying over.
    let mut rewritten = row;
    rewritten[6] = 's';
    rewritten[9] = 'Z';
    let t2 = t1 + Duration::from_millis(90);
    glow.note_typed_cells(t2, 1);
    glow.observe_row(2, 7, &rewritten, t2);
    glow.tick(Some((2, 7)), t2, &c, g, &mut out);
    assert!(
        glow.particles.len() >= 5,
        "the glitter behind the caret survives its own keystroke's echo"
    );
    // All five grains remain: a grain is a projectile already leaving its
    // birth pixel, so the rewritten cell under it does not speak for it. Only
    // age and the wholesale teardowns (reset / dark) retire the family.
    for col in 2..6u16 {
        let (cx, _) = g.cell_center(2, col);
        assert!(
            glow.particles.iter().any(|p| (p.x0 - cx).abs() < 0.5),
            "the grain born over column {col} must survive the echo"
        );
    }
    assert!(
        glow.particles.iter().any(|p| (p.x0 - rx).abs() < 0.5),
        "an at-cursor transient is not fenced by the content under it"
    );
}

/// A denied program relocation retires only movement-owned click-dedup
/// credit. The already-earned key-time sound remains queued, while the next
/// genuine admitted key can establish and spend a fresh credit normally.
#[test]
fn denied_move_preserves_queued_sound_but_retires_stale_keyed_credit() {
    let g = geom();
    let c = cfg(GlowStyle::Water, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);

    let first = t0 + Duration::from_millis(1);
    assert!(glow.cue_keystroke(first));
    glow.note_typed(first);
    assert_eq!(glow.sound_cues.len(), 1);
    assert_eq!(glow.keyed_clicks, 1);

    // No exact candidate: this CUP is dark and consumes the uncorrelated
    // classifier. It must not erase the sound already earned at key time.
    glow.tick(
        Some((2, 3)),
        first + Duration::from_millis(1),
        &c,
        g,
        &mut out,
    );
    assert_eq!(
        glow.sound_cues.len(),
        1,
        "earned sound survives visual denial"
    );
    assert_eq!(glow.keyed_clicks, 0, "stale echo-dedup credit retires");

    let second = first + Duration::from_millis(20);
    assert!(
        glow.cue_keystroke(second),
        "the next genuine key is not muted"
    );
    assert_eq!(glow.sound_cues.len(), 2);
    assert_eq!(glow.keyed_clicks, 1);
    glow.note_synthetic_typed(second, 1);
    glow.tick(Some((2, 4)), second, &c, g, &mut out);
    assert_eq!(
        glow.keyed_clicks, 0,
        "the new exact echo spends its own credit"
    );
    assert_eq!(glow.sound_cues.len(), 2, "echo adds no duplicate click");
}

/// `note_user_gesture` keeps the banked typed stamps: a Tab-class gesture landing with two keys'
/// echoes in flight licenses its own sweep and leaves the bank intact. The Tab's 4-cell sweep is
/// a hop the two presses cannot explain (`no-credits`, a forget edge), so the pool is forgotten
/// and the following keyless `+1` is refused.
#[test]
fn user_gesture_keeps_banked_typed_stamps_for_in_flight_echoes() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    // Two real keys banked, echoes not yet observed.
    let k1 = t0 + Duration::from_millis(10);
    glow.note_typed(k1);
    glow.note_typed(k1 + Duration::from_millis(5));
    // Tab lands mid-burst: the gesture class arms without wiping the bank.
    glow.note_user_gesture(k1 + Duration::from_millis(8));
    assert!(
        glow.type_hint
            .any_fresh(k1 + Duration::from_millis(9), CursorGlow::TYPE_HINT_FRESH),
        "arming the gesture class must not destroy banked typed stamps"
    );
    // The tab sweep spawns (licensed by the gesture; spawn's class-blind
    // FIFO consumption also pops the oldest stamp)…
    let e1 = k1 + Duration::from_millis(20);
    glow.tick(Some((2, 6)), e1, &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the tab sweep itself is licensed");
    // The two presses could not explain a 4-cell hop, so the sweep forgot
    // them, and the remaining fresh stamp is no licence without a press: the
    // next `+1` is refused.
    assert_eq!(glow.in_flight_tally().forgotten, 1);
    assert_eq!(glow.typed_credits_within(e1), 0);
    let e2 = e1 + Duration::from_millis(16);
    glow.tick(Some((2, 7)), e2, &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (2, 6), (2, 7))),
        "a keyless +1 on a stamp whose presses were forgotten: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.spawns(), 1);
}

/// The completed hidden→visible boundary spares typed stamps younger than
/// `TYPE_HINT_FRESH` (keys whose echoes are in flight through a TUI's DECTCEM
/// repaint bracket) while retiring a stale bank.
#[test]
fn hidden_boundary_spares_fresh_typed_stamps_and_retires_stale() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    // A repaint bracket catches a frame hidden…
    glow.tick(None, t0 + Duration::from_millis(16), &c, g, &mut out);
    // …two real keys land while hidden…
    let k = t0 + Duration::from_millis(30);
    glow.note_typed(k);
    glow.note_typed(k + Duration::from_millis(5));
    // …and the bracket completes: hidden→visible at the same cell.
    glow.tick(Some((2, 2)), k + Duration::from_millis(10), &c, g, &mut out);
    assert!(
        glow.type_hint
            .any_fresh(k + Duration::from_millis(11), CursorGlow::TYPE_HINT_FRESH),
        "fresh typed stamps must survive the hidden→visible boundary"
    );
    // Their echoes are licensed and lay light.
    glow.tick(Some((2, 3)), k + Duration::from_millis(20), &c, g, &mut out);
    glow.tick(Some((2, 4)), k + Duration::from_millis(36), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        2,
        "echoes of keys pressed inside the hide bracket must spawn after it completes"
    );

    // The stale half: a bank older than TYPE_HINT_FRESH is fully retired at
    // the boundary.
    let mut stale = CursorGlow::default();
    let s0 = Instant::now();
    stale.tick(Some((2, 2)), s0, &c, g, &mut out);
    stale.note_typed(s0 + Duration::from_millis(1));
    stale.tick(None, s0 + Duration::from_millis(16), &c, g, &mut out);
    // Boundary completes 400 ms later — the stamp is stale by then.
    stale.tick(
        Some((2, 2)),
        s0 + Duration::from_millis(400),
        &c,
        g,
        &mut out,
    );
    assert!(
        !stale.type_hint.armed(),
        "a stale bank is still fully retired at the boundary"
    );
}

/// With the DEC cursor hidden across frames (the repaint bracket never
/// re-shows), a keystroke's echo is anchored to the print-run mutation site
/// and spawns there (`echo_anchor_pass`); program-only output (no stamps)
/// paints and spawns nothing.
#[test]
fn hidden_caret_typed_echo_anchors_to_print_site() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    // The TUI drew its input row; the cursor is hidden from the start.
    glow.observe_print_anchor(Some((2, 2, 1)));
    glow.tick(None, t0, &c, g, &mut out);
    assert_eq!(glow.spawns(), 0);
    // A program-only rewrite advancing a status row's end: no stamps, nothing
    // lit, and that row is branded a program row; the input row, which only
    // advances with keys, is not, so the echo below lights.
    glow.observe_print_anchor(Some((5, 10, 2)));
    glow.tick(None, t0 + Duration::from_millis(8), &c, g, &mut out);
    glow.observe_print_anchor(Some((5, 11, 3)));
    glow.tick(None, t0 + Duration::from_millis(16), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        0,
        "program-only output under a hidden cursor must paint nothing"
    );
    // A real key; its echo lands while the cursor stays hidden.
    let k = t0 + Duration::from_millis(30);
    glow.note_typed(k);
    glow.observe_print_anchor(Some((2, 3, 4)));
    glow.tick(None, k + Duration::from_millis(10), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "a licensed echo under a hidden cursor must anchor to its mutation site"
    );
}

/// With the DEC cursor visible but parked away from the caret (CUP 1;1 before
/// show), the licensed echo anchors to the mutated row; and a visible cursor
/// move owns its own tick, so the anchor lane never double-judges the same PTY
/// delta (`cursor_move_observed`).
#[test]
fn parked_caret_typed_echo_anchors_and_visible_moves_own_their_tick() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    // Parked at the origin cell, every frame.
    glow.tick(Some((0, 0)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((2, 2, 1)));
    glow.tick(
        Some((0, 0)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert_eq!(glow.spawns(), 0);
    let k = t0 + Duration::from_millis(30);
    glow.note_typed(k);
    glow.observe_print_anchor(Some((2, 3, 2)));
    glow.tick(Some((0, 0)), k + Duration::from_millis(10), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "a licensed echo under a parked cursor must anchor to the echoed row"
    );

    // Visible-move ownership: a normal shell echo (cursor moves with the
    // anchor) spawns exactly once even with surplus stamps banked.
    let mut shell = CursorGlow::default();
    let s0 = Instant::now();
    shell.tick(Some((2, 2)), s0, &c, g, &mut out);
    shell.note_typed(s0 + Duration::from_millis(1));
    shell.observe_print_anchor(Some((2, 3, 1)));
    shell.tick(
        Some((2, 3)),
        s0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert_eq!(shell.spawns(), 1);
    let k2 = s0 + Duration::from_millis(20);
    shell.note_typed(k2);
    shell.note_typed(k2 + Duration::from_millis(2));
    shell.observe_print_anchor(Some((2, 4, 2)));
    shell.tick(Some((2, 4)), k2 + Duration::from_millis(8), &c, g, &mut out);
    assert_eq!(
        shell.spawns(),
        2,
        "a visible cursor move owns its tick — the anchor lane must not double-judge it"
    );
}

/// A program row's forward end-advance never consumes a banked typed stamp. With the cursor
/// hidden, the input row (11) advances only after `note_typed` while the status row (9) grows on
/// its own ~150 ms cadence (a Claude Code timer); a row seen advancing its end with no gesture
/// fresh is branded a program row and refuses anchored sweeps, so every input echo lights.
#[test]
fn program_row_end_advance_never_consumes_a_typed_stamp() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    // Hidden from the start; both rows drawn once (seed, no advance yet).
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((9, 10, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 2)));
    glow.tick(None, at(32), &c, g, &mut out);
    // The status row's keyless advance (its 150 ms tick, nobody typing) spawns nothing and
    // brands row 9 a program row.
    glow.observe_print_anchor(Some((9, 11, 3)));
    glow.tick(None, at(150), &c, g, &mut out);
    assert_eq!(glow.spawns(), 0, "keyless growth must stay dark");
    // k1 and its echo on the input row: licensed, anchored, lit.
    glow.note_typed(at(240));
    glow.observe_print_anchor(Some((11, 3, 4)));
    glow.tick(None, at(245), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the input echo must still light");
    // k2 banks a stamp; the status row's next tick is sampled before k2's echo,
    // inside the stamp's freshness window.
    glow.note_typed(at(330));
    glow.observe_print_anchor(Some((9, 12, 5)));
    glow.tick(None, at(336), &c, g, &mut out);
    // The status row must never be judged as an echo: any ring record on row 9
    // other than the row-discrimination refusal is a spent stamp.
    let status_row_spends = glow
        .admission_log()
        .filter(|r| {
            (r.origin.0 == 9 || r.target.0 == 9) && r.reason != CursorGlow::DECLINE_PROGRAM_ROW
        })
        .count();
    assert_eq!(
        status_row_spends, 0,
        "a program row's end-advance must never consume a banked typed stamp"
    );
    // …and because the stamp was not spent on the status row, k2's real echo still lights:
    // judged input-row echoes == keys typed.
    glow.observe_print_anchor(Some((11, 4, 6)));
    glow.tick(None, at(340), &c, g, &mut out);
    let input_row_echoes = glow.admission_log().filter(|r| r.origin.0 == 11).count();
    assert_eq!(
        input_row_echoes, 2,
        "every typed stamp funds its own input-row echo — none diverted"
    );
}

/// The spoken-for arm: a spinner counter holds constant width until `(9)`
/// becomes `(10)`, so its first end-advance has no keyless history and the
/// taint brand cannot refuse it mid-burst. While another row's licensed
/// anchored echo is younger than the stamp window, the echo lane is spoken for
/// and no other row may spend a stamp.
#[test]
fn a_program_rows_first_advance_mid_burst_cannot_steal_the_echo() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    glow.tick(None, t0, &c, g, &mut out);
    // Spinner row 10 seeded at its constant width; input row 12 seeded.
    glow.observe_print_anchor(Some((10, 19, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.observe_print_anchor(Some((12, 2, 2)));
    glow.tick(None, at(32), &c, g, &mut out);
    // Spinner repaints at the same end column: no advance, no history for row 10.
    glow.observe_print_anchor(Some((10, 19, 3)));
    glow.tick(None, at(150), &c, g, &mut out);
    // k1's echo establishes the input row as the echo row.
    glow.note_typed(at(240));
    glow.observe_print_anchor(Some((12, 3, 4)));
    glow.tick(None, at(245), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the input echo must light");
    // k2 banks a stamp; the counter crosses its digit width: row 10's first advance, brandless,
    // mid-burst, stamp-fresh.
    glow.note_typed(at(330));
    glow.observe_print_anchor(Some((10, 20, 5)));
    glow.tick(None, at(336), &c, g, &mut out);
    let crossing_spends = glow
        .admission_log()
        .filter(|r| {
            (r.origin.0 == 10 || r.target.0 == 10) && r.reason != CursorGlow::DECLINE_PROGRAM_ROW
        })
        .count();
    assert_eq!(
        crossing_spends, 0,
        "a program row's first advance mid-burst must not steal the echo"
    );
    // k2's real echo still lights off its surviving stamp.
    glow.observe_print_anchor(Some((12, 4, 6)));
    glow.tick(None, at(340), &c, g, &mut out);
    let input_row_echoes = glow.admission_log().filter(|r| r.origin.0 == 12).count();
    assert_eq!(
        input_row_echoes, 2,
        "the spoken-for hold must never divert the input row's own echo"
    );
}

/// A program row's brand survives a busy TUI printing on five rows (Claude Code mid-turn:
/// spinner, composer, bullet, tool timer, workflow timer). After a hesitation beyond the 250 ms
/// spoken-for hold, the spinner prints a digit between the `i` press and its echo: it is refused
/// `program-row`, forgets nothing, and `i`'s echo lights its cell (no gap in `s▯de`).
#[test]
fn a_busy_tuis_status_row_keeps_its_brand_across_five_print_rows() {
    let g = Geom {
        rows: 24,
        cols: 60,
        win_w: 480,
        win_h: 384,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    // Claude Code's rows, top to bottom; the caret is drawn on the composer row and every status
    // write walks it back there.
    const BULLET: u16 = 4;
    const TOOL: u16 = 7;
    const SPINNER: u16 = 10;
    const COMPOSER: u16 = 14;
    const WORKFLOW: u16 = 18;
    let mut seq = 0u64;
    let mut frame =
        |glow: &mut CursorGlow, print: Option<(u16, u16, char)>, caret: u16, ms: u64| {
            if let Some((row, end, glyph)) = print {
                seq += 1;
                glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
            }
            glow.observe_caret_drawn(true);
            glow.tick(Some((COMPOSER, caret)), at(ms), &c, g, &mut out);
        };
    // The busy screen with nobody typing: the spinner's seconds digit
    // advances its row's end keylessly — the brand.
    frame(&mut glow, None, 2, 0);
    frame(&mut glow, Some((SPINNER, 11, '✻')), 2, 16);
    frame(&mut glow, Some((SPINNER, 18, '1')), 2, 100);
    frame(&mut glow, Some((BULLET, 1, '●')), 2, 150);
    frame(&mut glow, Some((TOOL, 20, '2')), 2, 200);
    frame(&mut glow, Some((WORKFLOW, 25, '5')), 2, 250);
    // The fifth row: the composer draws its prompt.
    frame(&mut glow, Some((COMPOSER, 2, ' ')), 2, 300);
    // The spinner's glyph frame retreats its end (no advance, no brand).
    frame(&mut glow, Some((SPINNER, 11, '✽')), 2, 350);
    assert_eq!(glow.spawns(), 0, "the busy screen alone lights nothing");
    // `s`, echoed on the composer row: the visible lane's licensed echo.
    glow.note_typed_expected(at(1000), 1, false, rk::TypedClass::Glyph, 's');
    frame(&mut glow, Some((COMPOSER, 3, 's')), 3, 1012);
    frame(&mut glow, Some((SPINNER, 11, '✻')), 3, 1100);
    // The hesitation: `i` pressed 322 ms after `s` echoed, the spinner's digit
    // 3 ms later and 13 ms before `i`'s echo, outside the spoken-for hold.
    glow.note_typed_expected(at(1334), 1, false, rk::TypedClass::Glyph, 'i');
    frame(&mut glow, Some((SPINNER, 18, '3')), 3, 1337);
    let spinner = glow
        .admission_log()
        .find(|r| r.origin == (SPINNER, 11) && r.target == (SPINNER, 18))
        .map(|r| r.reason);
    assert_eq!(
        spinner,
        Some(CursorGlow::DECLINE_PROGRAM_ROW),
        "PRECONDITION: the spinner's advance was judged inside `i`'s pre-echo window"
    );
    frame(&mut glow, Some((COMPOSER, 4, 'i')), 4, 1350);
    glow.note_typed_expected(at(1488), 1, false, rk::TypedClass::Glyph, 'd');
    frame(&mut glow, Some((COMPOSER, 5, 'd')), 5, 1500);
    for ms in (1516..=1600).step_by(16) {
        frame(&mut glow, None, 5, ms);
    }
    // The digit is no key's echo, every composer echo is licensed, and each
    // key lights its own cell.
    let refused: Vec<_> = glow
        .admission_log()
        .filter(|r| r.origin.0 == COMPOSER && r.reason == CursorGlow::DECLINE_NO_FRESH_HINT)
        .map(|r| (r.origin, r.target))
        .collect();
    assert_eq!(
        (
            glow.in_flight_tally().forgotten,
            refused,
            dark_in(&glow, COMPOSER, 2..5)
        ),
        (0, Vec::new(), Vec::new()),
        "(forgotten presses, refused composer echoes, dark typed columns)"
    );
}

/// The anchored lane's row memory evicts the least recently printed row, never a status row that
/// is still ticking, so a spinner keeps its slot and brand through twenty transcript rows printed
/// after it.
#[test]
fn a_ticking_status_row_keeps_its_brand_beside_a_streaming_transcript() {
    let g = Geom {
        rows: 40,
        cols: 60,
        win_w: 480,
        win_h: 640,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    const SPINNER: u16 = 2;
    const BULLET: u16 = 3;
    const COMPOSER: u16 = 34;
    let mut seq = 0u64;
    let mut ms = 0u64;
    let mut frame = |glow: &mut CursorGlow, row: u16, end: u16, glyph: char| {
        seq += 1;
        ms += 16;
        glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
        glow.observe_caret_drawn(true);
        glow.tick(Some((COMPOSER, 2)), at(ms), &c, g, &mut out);
    };
    // The spinner's glyph frame, then its digit: a keyless advance — the
    // brand. The bullet prints once and never again.
    frame(&mut glow, SPINNER, 11, '✻');
    frame(&mut glow, SPINNER, 18, '1');
    frame(&mut glow, BULLET, 1, '●');
    assert!(
        glow.anchor_row(SPINNER).is_some_and(|(_, m)| m.tainted),
        "PRECONDITION: the spinner is branded"
    );
    // A transcript streams onto twenty new rows, the spinner's glyph frame
    // printing after every fourth: twenty-two rows, more than the lane holds.
    for k in 0..20u16 {
        frame(&mut glow, 5 + k, 29, 'e');
        if k % 4 == 3 {
            frame(&mut glow, SPINNER, 11, '✽');
        }
    }
    assert!(
        glow.anchor_row(SPINNER).is_some_and(|(_, m)| m.tainted),
        "the ticking spinner kept its slot and its brand"
    );
    assert!(
        glow.anchor_row(BULLET).is_none(),
        "the row that stopped printing is the one forgotten"
    );
    assert_eq!(glow.spawns(), 0, "program output alone lights nothing");
}

/// What [`footer_tick_in_a_hesitated_keys_window`] measured.
struct FooterTick {
    /// Presses forgotten.
    forgotten: u64,
    /// Composer echoes refused `no-fresh-hint`.
    refused: Vec<((u16, u16), (u16, u16))>,
    /// Typed composer columns left dark.
    dark: Vec<u16>,
    /// The footer advance's verdict.
    verdict: Option<&'static str>,
}

/// What the hand presses between `s`'s echo and `i` in
/// [`footer_tick_in_a_hesitated_keys_window`].
#[derive(Clone, Copy, Debug)]
enum BeforeI {
    /// Nothing: `i` follows `s`.
    Nothing,
    /// A different-class press 4 ms before `i` — zle's Ctrl-R, as the host
    /// sends it (`clear_typed`).
    CtrlR,
    /// A typo erased: `x` echoed, then a Backspace (the host's line-editing
    /// disarm and one priced erase), its echo 274 ms before `i`'s press.
    Backspace,
    /// The caret stepped left and back: two arrows (line-editing disarm and a
    /// motion each), the second's echo 274 ms before `i`'s press.
    Arrows,
}

/// How the screen scrolls between `s`'s echo and `i` in
/// [`footer_tick_in_a_hesitated_keys_window`].
#[derive(Clone, Copy, Debug)]
enum ScrollBeforeI {
    /// It does not.
    None,
    /// Up a row, the composer and its footer with the content (a line printed below them
    /// pushed them up).
    WithTheContent,
    /// Up a row under a bottom-anchored composer (full-screen Claude Code): a
    /// transcript line above pushed the content up, and the composer and its
    /// footer are redrawn on their own screen rows.
    ComposerStays,
}

/// The frames of [`a_status_tick_below_the_caret_forgets_no_press_while_the_hand_types`]:
/// a composer on row 10 with its caret drawn, and a footer on row 12 whose
/// first print (a timer ending at 15) came before typing began, so it is
/// unbranded. `s` echoes on the composer; then `before_i` is pressed, the
/// screen scrolls per `scroll_before_i`, `i` is pressed, and the footer's token
/// counter advances to 35 3 ms after `i`'s press and 13 ms before its echo,
/// over 250 ms after the composer's last echo.
fn footer_tick_in_a_hesitated_keys_window(
    before_i: BeforeI,
    scroll_before_i: ScrollBeforeI,
) -> FooterTick {
    let g = Geom {
        rows: 24,
        cols: 60,
        win_w: 480,
        win_h: 384,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut composer: u16 = 10;
    let mut footer: u16 = 12;
    let mut seq = 0u64;
    let mut frame =
        |glow: &mut CursorGlow, print: Option<(u16, u16, char)>, caret: (u16, u16), ms: u64| {
            if let Some((row, end, glyph)) = print {
                seq += 1;
                glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
            }
            glow.observe_caret_drawn(true);
            glow.tick(Some(caret), at(ms), &c, g, &mut out);
        };
    frame(&mut glow, Some((composer, 2, ' ')), (composer, 2), 0);
    frame(&mut glow, Some((footer, 15, '1')), (composer, 2), 100);
    glow.note_typed_expected(at(1000), 1, false, rk::TypedClass::Glyph, 's');
    frame(&mut glow, Some((composer, 3, 's')), (composer, 3), 1012);
    match before_i {
        BeforeI::Nothing | BeforeI::CtrlR => {}
        BeforeI::Backspace => {
            glow.note_typed_expected(at(1020), 1, false, rk::TypedClass::Glyph, 'x');
            frame(&mut glow, Some((composer, 4, 'x')), (composer, 4), 1030);
            glow.clear_typed_keeping_hand(at(1045));
            glow.note_backspace_erasing(at(1045), Some(1));
            frame(&mut glow, None, (composer, 3), 1060);
        }
        BeforeI::Arrows => {
            glow.clear_typed_keeping_hand(at(1020));
            glow.note_motion(at(1020));
            frame(&mut glow, None, (composer, 2), 1030);
            glow.clear_typed_keeping_hand(at(1045));
            glow.note_motion(at(1045));
            frame(&mut glow, None, (composer, 3), 1060);
        }
    }
    match scroll_before_i {
        ScrollBeforeI::None => {}
        ScrollBeforeI::WithTheContent => {
            glow.note_scroll(1);
            glow.drop_row_probe();
            composer -= 1;
            footer -= 1;
            frame(&mut glow, None, (composer, 3), 1100);
            // The footer's timer is its first print in the new coordinates.
            frame(&mut glow, Some((footer, 15, '2')), (composer, 3), 1200);
        }
        ScrollBeforeI::ComposerStays => {
            glow.note_scroll(1);
            glow.drop_row_probe();
            // The composer redrawn on its own row, its caret where it was.
            frame(&mut glow, Some((composer, 3, 's')), (composer, 3), 1100);
            frame(&mut glow, Some((footer, 15, '2')), (composer, 3), 1200);
        }
    }
    if matches!(before_i, BeforeI::CtrlR) {
        glow.clear_typed(at(1330));
    }
    glow.note_typed_expected(at(1334), 1, false, rk::TypedClass::Glyph, 'i');
    frame(&mut glow, Some((footer, 35, 'k')), (composer, 3), 1337);
    let verdict = glow
        .admission_log()
        .find(|r| r.origin == (footer, 15) && r.target == (footer, 35))
        .map(|r| r.reason);
    frame(&mut glow, Some((composer, 4, 'i')), (composer, 4), 1350);
    for ms in (1366..=1450).step_by(16) {
        frame(&mut glow, None, (composer, 4), ms);
    }
    let refused = glow
        .admission_log()
        .filter(|r| r.origin.0 == composer && r.reason == CursorGlow::DECLINE_NO_FRESH_HINT)
        .map(|r| (r.origin, r.target))
        .collect();
    FooterTick {
        forgotten: glow.in_flight_tally().forgotten,
        refused,
        dark: dark_in(&glow, composer, 2..4),
        verdict,
    }
}

/// The visible-parked minibuffer gate refuses a run ending on a glyph no live
/// press typed, and forgets the presses only when that run is the keys' own
/// echo: below the drawn caret with the hand not typing on the caret's row. An
/// unbranded footer below Claude Code's composer ticking between a hesitated
/// key's press and echo is refused `program-row` and forgets nothing. Control:
/// a different-class press first (zle's Ctrl-R) takes the hand off the row,
/// and the same refusal forgets the press.
#[test]
fn a_status_tick_below_the_caret_forgets_no_press_while_the_hand_types() {
    for scroll in [ScrollBeforeI::None, ScrollBeforeI::WithTheContent] {
        let m = footer_tick_in_a_hesitated_keys_window(BeforeI::Nothing, scroll);
        assert_eq!(
            m.verdict,
            Some(CursorGlow::DECLINE_PROGRAM_ROW),
            "PRECONDITION (scroll {scroll:?}): the footer's advance was refused inside `i`'s window"
        );
        assert_eq!(
            (m.forgotten, m.refused, m.dark),
            (0, Vec::new(), Vec::new()),
            "scroll {scroll:?}: (forgotten presses, refused composer echoes, dark typed columns)"
        );
    }
    let m = footer_tick_in_a_hesitated_keys_window(BeforeI::CtrlR, ScrollBeforeI::None);
    assert_eq!(m.verdict, Some(CursorGlow::DECLINE_PROGRAM_ROW));
    assert_eq!(
        m.forgotten, 1,
        "after a different-class press, a refused run below the caret is the minibuffer's echo"
    );
}

/// A line-editing press (a Backspace, an arrow) keeps the hand on its row
/// ([`CursorGlow::clear_typed_keeping_hand`]), so the next key is guarded: the
/// footer's unbranded tick in its window, over 250 ms after the edit's echo, is
/// refused `program-row` and forgets nothing. zle's Ctrl-R still takes the hand
/// off (control in
/// [`a_status_tick_below_the_caret_forgets_no_press_while_the_hand_types`]).
#[test]
fn a_line_editing_key_keeps_the_hand_on_its_row() {
    for before_i in [BeforeI::Backspace, BeforeI::Arrows] {
        for scroll in [ScrollBeforeI::None, ScrollBeforeI::WithTheContent] {
            let m = footer_tick_in_a_hesitated_keys_window(before_i, scroll);
            assert_eq!(
                m.verdict,
                Some(CursorGlow::DECLINE_PROGRAM_ROW),
                "PRECONDITION ({before_i:?}, scroll {scroll:?}): the footer's advance was refused inside `i`'s window"
            );
            assert_eq!(
                (m.forgotten, m.refused, m.dark),
                (0, Vec::new(), Vec::new()),
                "{before_i:?}, scroll {scroll:?}: (forgotten presses, refused composer echoes, dark typed columns)"
            );
        }
    }
}

/// When the caret stays on its screen row under a scroll, the hand's row stays with the caret,
/// not the content. A full-screen Claude Code is bottom-anchored: each transcript line scrolls
/// the screen above the composer and redraws composer and footer on the same rows, so the
/// footer's tick in the next key's window forgets nothing. (`s`'s cell rode the scroll up;
/// `i`'s, column 3, is typed on the redrawn composer.)
#[test]
fn a_composer_that_stays_put_under_a_scroll_keeps_the_hand_on_its_row() {
    for before_i in [BeforeI::Nothing, BeforeI::Backspace, BeforeI::Arrows] {
        let m = footer_tick_in_a_hesitated_keys_window(before_i, ScrollBeforeI::ComposerStays);
        assert_eq!(
            m.verdict,
            Some(CursorGlow::DECLINE_PROGRAM_ROW),
            "PRECONDITION ({before_i:?}): the footer's advance was refused inside `i`'s window"
        );
        assert_eq!(
            (m.forgotten, m.refused, m.dark.contains(&3)),
            (0, Vec::new(), false),
            "{before_i:?}: (forgotten presses, refused composer echoes, `i`'s column dark)"
        );
    }
}

/// Enter takes the hand off its row only when it hands the line to a shell, or
/// when the host cannot tell ([`CursorGlow::note_return`]); into a foreground
/// program's composer (shell integration says a command runs, or the alternate
/// screen is up) it keeps it ([`CursorGlow::note_return_keeping_hand`]). Claude
/// Code redraws its empty composer on the same row with the spinner above; the
/// spinner's seconds reach the next key's glyph 3 ms after its press and are
/// refused `program-row`, and the key's cell lights.
#[test]
fn the_first_key_after_a_submit_into_a_program_s_composer_is_guarded() {
    let g = Geom {
        rows: 24,
        cols: 60,
        win_w: 480,
        win_h: 384,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    const SPINNER: u16 = 8;
    const COMPOSER: u16 = 10;
    let mut seq = 0u64;
    let mut frame = |glow: &mut CursorGlow, print: Option<(u16, u16, char)>, col: u16, ms: u64| {
        if let Some((row, end, glyph)) = print {
            seq += 1;
            glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
        }
        glow.observe_caret_drawn(true);
        glow.tick(Some((COMPOSER, col)), at(ms), &c, g, &mut out);
    };
    // The composer holds a line typed earlier; `ls` ends it.
    frame(&mut glow, Some((COMPOSER, 20, ' ')), 20, 0);
    glow.note_typed_expected(at(1000), 1, false, rk::TypedClass::Glyph, 'l');
    frame(&mut glow, Some((COMPOSER, 21, 'l')), 21, 1012);
    glow.note_typed_expected(at(1100), 1, false, rk::TypedClass::Glyph, 's');
    frame(&mut glow, Some((COMPOSER, 22, 's')), 22, 1112);
    // Enter, into the program that owns the line; the composer comes back
    // empty on its row, and the spinner prints its glyph frame above it.
    glow.note_return_keeping_hand(at(1200));
    frame(&mut glow, Some((COMPOSER, 2, ' ')), 2, 1230);
    frame(&mut glow, Some((SPINNER, 11, '✻')), 2, 1300);
    // The first key after the submit, and the spinner's seconds reaching
    // `…1` 3 ms after its press, 373 ms after the submit's licensed response.
    glow.note_typed_expected(at(1600), 1, false, rk::TypedClass::Glyph, '1');
    frame(&mut glow, Some((SPINNER, 18, '1')), 2, 1603);
    let verdict = glow
        .admission_log()
        .find(|r| r.origin == (SPINNER, 11) && r.target == (SPINNER, 18))
        .map(|r| r.reason);
    frame(&mut glow, Some((COMPOSER, 3, '1')), 3, 1615);
    for ms in (1631..=1700).step_by(16) {
        frame(&mut glow, None, 3, ms);
    }
    let refused: Vec<_> = glow
        .admission_log()
        .filter(|r| r.origin == (COMPOSER, 2) && r.reason == CursorGlow::DECLINE_NO_FRESH_HINT)
        .map(|r| (r.origin, r.target))
        .collect();
    assert_eq!(
        (
            verdict,
            glow.in_flight_tally().forgotten,
            refused,
            dark_in(&glow, COMPOSER, 2..3),
            v2_cols(&glow, SPINNER).into_iter().collect::<Vec<_>>(),
        ),
        (
            Some(CursorGlow::DECLINE_PROGRAM_ROW),
            0,
            Vec::new(),
            Vec::new(),
            Vec::new()
        ),
        "(the spinner advance's verdict, forgotten presses, refused composer echoes, \
         dark typed columns, spinner-row ink)"
    );
}

/// A press the hand's-row hold kept ([`PressCredits::held_through`]) pays only for a move on the
/// hand's row. In Emacs `C-u 8` echoes `C-u 8-` below the caret, refused `program-row` without
/// forgetting the press; the caret's keyless fold to the next row 600 ms later then finds no
/// credit and is refused.
#[test]
fn a_press_the_hold_kept_pays_for_no_hop_off_the_hand_s_row() {
    let g = Geom {
        rows: 24,
        cols: 60,
        win_w: 480,
        win_h: 384,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    const BUFFER: u16 = 10;
    const ECHO_AREA: u16 = 23;
    let mut seq = 0u64;
    let mut frame =
        |glow: &mut CursorGlow, print: Option<(u16, u16, char)>, caret: (u16, u16), ms: u64| {
            if let Some((row, end, glyph)) = print {
                seq += 1;
                glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
            }
            glow.observe_caret_drawn(true);
            glow.tick(Some(caret), at(ms), &c, g, &mut out);
        };
    frame(&mut glow, Some((BUFFER, 58, ' ')), (BUFFER, 58), 0);
    // `a` typed at the caret, to the row's last column: the hand is on its row.
    glow.note_typed_expected(at(1000), 1, false, rk::TypedClass::Glyph, 'a');
    frame(&mut glow, Some((BUFFER, 59, 'a')), (BUFFER, 59), 1012);
    // C-u, as the host sends it (a kill: the line-editing close, then the
    // kill class); after `echo-keystrokes` Emacs prints `C-u-`.
    glow.clear_typed_keeping_hand(at(1100));
    glow.note_kill(at(1100), true);
    frame(&mut glow, Some((ECHO_AREA, 4, '-')), (BUFFER, 59), 2100);
    // `8`, and the echo area's `C-u 8-` 3 ms later.
    glow.note_typed_expected(at(2500), 1, false, rk::TypedClass::Glyph, '8');
    frame(&mut glow, Some((ECHO_AREA, 6, '-')), (BUFFER, 59), 2503);
    let verdict = glow
        .admission_log()
        .find(|r| r.origin == (ECHO_AREA, 4) && r.target == (ECHO_AREA, 6))
        .map(|r| r.reason);
    for ms in (2519..=3000).step_by(16) {
        frame(&mut glow, None, (BUFFER, 59), ms);
    }
    // A keyless fold of the caret off the hand's row, no key behind it.
    frame(&mut glow, None, (BUFFER + 1, 0), 3100);
    let hop = glow
        .admission_log()
        .find(|r| r.origin == (BUFFER, 59) && r.target == (BUFFER + 1, 0))
        .map(|r| r.reason);
    assert_eq!(
        (verdict, hop, v2_cols(&glow, BUFFER).contains(&59)),
        (
            Some(CursorGlow::DECLINE_PROGRAM_ROW),
            Some(CursorGlow::DECLINE_NO_FRESH_HINT),
            false
        ),
        "(the echo area's verdict, the fold's verdict, ink laid by the fold)"
    );
}

/// A press the hand's-row hold kept is paid by its own late echo on the hand's
/// row, however late. A footer ticks 3 ms after `i`'s press and is refused
/// `program-row` (ending on a foreign glyph, or on `i` itself); the stalled
/// composer echoes `i` 400 ms or 2 s later with a stale stamp, the in-flight
/// press pays, and the cell lights. Only the in-flight patience bounds a held
/// press.
#[test]
fn a_held_press_pays_for_its_own_late_echo_on_the_hand_s_row() {
    let g = Geom {
        rows: 24,
        cols: 60,
        win_w: 480,
        win_h: 384,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    const COMPOSER: u16 = 10;
    const FOOTER: u16 = 12;
    for (tick_glyph, delay_ms) in [('k', 400), ('k', 2000), ('i', 400), ('i', 2000)] {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut seq = 0u64;
        let mut frame =
            |glow: &mut CursorGlow, print: Option<(u16, u16, char)>, col: u16, ms: u64| {
                if let Some((row, end, glyph)) = print {
                    seq += 1;
                    glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
                }
                glow.observe_caret_drawn(true);
                glow.tick(Some((COMPOSER, col)), at(ms), &c, g, &mut out);
            };
        frame(&mut glow, Some((COMPOSER, 2, ' ')), 2, 0);
        frame(&mut glow, Some((FOOTER, 15, '1')), 2, 100);
        glow.note_typed_expected(at(1000), 1, false, rk::TypedClass::Glyph, 's');
        frame(&mut glow, Some((COMPOSER, 3, 's')), 3, 1012);
        glow.note_typed_expected(at(1334), 1, false, rk::TypedClass::Glyph, 'i');
        frame(&mut glow, Some((FOOTER, 35, tick_glyph)), 3, 1337);
        let verdict = glow
            .admission_log()
            .find(|r| r.origin == (FOOTER, 15) && r.target == (FOOTER, 35))
            .map(|r| r.reason);
        let echo = 1334 + delay_ms;
        for ms in (1353..echo).step_by(16) {
            frame(&mut glow, None, 3, ms);
        }
        frame(&mut glow, Some((COMPOSER, 4, 'i')), 4, echo);
        for ms in (echo + 16..=echo + 80).step_by(16) {
            frame(&mut glow, None, 4, ms);
        }
        let refused: Vec<_> = glow
            .admission_log()
            .filter(|r| r.origin.0 == COMPOSER && r.reason == CursorGlow::DECLINE_NO_FRESH_HINT)
            .map(|r| (r.origin, r.target))
            .collect();
        assert_eq!(
            (
                verdict,
                glow.in_flight_tally().forgotten,
                refused,
                dark_in(&glow, COMPOSER, 3..4)
            ),
            (
                Some(CursorGlow::DECLINE_PROGRAM_ROW),
                0,
                Vec::new(),
                Vec::new()
            ),
            "tick ending on {tick_glyph:?}, echo {delay_ms} ms after the press: (the \
             footer's verdict, forgotten presses, refused composer echoes, `i`'s column dark)"
        );
    }
}

/// While the hand types on the drawn caret's row, an advance on another row is
/// no key's echo, whatever glyph it ends on. An unbranded spinner prints its
/// seconds' `0` between a hesitated `0` press and its echo; the run ends on a
/// live press's glyph, so the glyph test passes it, but the hand's row refuses
/// it `program-row`: no stamp or credit spent, nothing laid on the spinner
/// row, and the key's own echo lights its cell.
#[test]
fn a_status_tick_ending_on_a_live_press_s_glyph_is_no_key_s_echo() {
    let g = Geom {
        rows: 24,
        cols: 60,
        win_w: 480,
        win_h: 384,
        ..geom()
    };
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    const SPINNER: u16 = 8;
    const COMPOSER: u16 = 10;
    let mut seq = 0u64;
    let mut frame = |glow: &mut CursorGlow, print: Option<(u16, u16, char)>, col: u16, ms: u64| {
        if let Some((row, end, glyph)) = print {
            seq += 1;
            glow.observe_print_anchor_glyph(Some((row, end, seq)), Some(glyph));
        }
        glow.observe_caret_drawn(true);
        glow.tick(Some((COMPOSER, col)), at(ms), &c, g, &mut out);
    };
    frame(&mut glow, Some((COMPOSER, 2, ' ')), 2, 0);
    // The spinner's glyph frame: its first print, an endpoint and no brand.
    frame(&mut glow, Some((SPINNER, 11, '✻')), 2, 100);
    glow.note_typed_expected(at(1000), 1, false, rk::TypedClass::Glyph, '1');
    frame(&mut glow, Some((COMPOSER, 3, '1')), 3, 1012);
    // The hesitated `0`, and the spinner's seconds reaching `…0` 3 ms after
    // its press, 325 ms after `1` echoed (outside the spoken-for hold).
    glow.note_typed_expected(at(1334), 1, false, rk::TypedClass::Glyph, '0');
    frame(&mut glow, Some((SPINNER, 18, '0')), 3, 1337);
    let verdict = glow
        .admission_log()
        .find(|r| r.origin == (SPINNER, 11) && r.target == (SPINNER, 18))
        .map(|r| r.reason);
    frame(&mut glow, Some((COMPOSER, 4, '0')), 4, 1350);
    for ms in (1366..=1450).step_by(16) {
        frame(&mut glow, None, 4, ms);
    }
    let refused: Vec<_> = glow
        .admission_log()
        .filter(|r| r.origin.0 == COMPOSER && r.reason == CursorGlow::DECLINE_NO_FRESH_HINT)
        .map(|r| (r.origin, r.target))
        .collect();
    assert_eq!(
        (
            verdict,
            glow.in_flight_tally().forgotten,
            refused,
            dark_in(&glow, COMPOSER, 2..4),
            v2_cols(&glow, SPINNER).into_iter().collect::<Vec<_>>(),
        ),
        (
            Some(CursorGlow::DECLINE_PROGRAM_ROW),
            0,
            Vec::new(),
            Vec::new(),
            Vec::new()
        ),
        "(the spinner advance's verdict, forgotten presses, refused composer echoes, \
         dark typed columns, spinner-row ink)"
    );
}

/// The taint brand never brands the established echo row (the `last_anchor_sweep` holder). After
/// more than 250 ms of quiet an undelivered paste (stamped and revoked in one event-loop turn)
/// advances that row with no licence fresh: it stays dark and brands nothing, and later typed
/// echoes are licensed. The exemption is safe because only a licensed anchored echo writes
/// `last_anchor_sweep`, which a program row never gets; it goes by identity, not freshness, since
/// the quiet gap outlasts any stamp window. The delivered sibling is
/// `a_delivered_insert_lights_a_hidden_caret_tui_at_its_print_anchor`.
#[test]
fn an_unlicensed_paste_must_not_brand_the_established_echo_row() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    // Hidden caret from the start; input row 11 seeded.
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    // Typed echoes licensed on the input row make it the established echo row (the
    // `last_anchor_sweep` holder).
    glow.note_typed(at(30));
    glow.observe_print_anchor(Some((11, 3, 2)));
    glow.tick(None, at(35), &c, g, &mut out);
    glow.note_typed(at(120));
    glow.observe_print_anchor(Some((11, 4, 3)));
    glow.tick(None, at(125), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "phase-1 typed echoes must light");
    // >250 ms quiet (every stamp stale), then the paste: gesture stamped and
    // revoked in the same turn (enqueue is not delivery), bytes advancing the
    // input row 8 cells.
    let paste = at(420);
    glow.note_user_gesture(paste);
    glow.revoke_input_hints_at(paste);
    glow.observe_print_anchor(Some((11, 12, 4)));
    glow.tick(None, at(430), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        2,
        "an UNDELIVERED paste advance stays dark — nothing licensed it"
    );
    // Typed keys at 90 ms; each echo advances the input row by one cell.
    for (i, key_ms) in [520u64, 610, 700].into_iter().enumerate() {
        let i = i as u64;
        glow.note_typed(at(key_ms));
        glow.observe_print_anchor(Some((11, 13 + i as u16, 5 + i)));
        glow.tick(None, at(key_ms + 6), &c, g, &mut out);
    }
    let program_row_refusals = glow
        .admission_log()
        .filter(|r| r.origin.0 == 11 && r.reason == CursorGlow::DECLINE_PROGRAM_ROW)
        .count();
    assert_eq!(
        program_row_refusals, 0,
        "an unlicensed paste must not brand the established echo row a program row"
    );
    assert_eq!(
        glow.spawns(),
        5,
        "every post-paste typed echo must still anchor and light — the anchor survives the paste"
    );
}
