// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed stamps across gestures, hidden carets and program rows.

use super::*;

/// R2 — THE SPARKLES LOST THEIR TRAIL. Sparkle's tail is its GLITTER
/// PARTICLE train, and the proof era's generation fence cleared the whole
/// particle population on every judged batch — so every keystroke wiped
/// the previous keystroke's glitter before its own burst spawned, and
/// sparkle could only ever be a puff at the caret.
///
/// Under the LICENSE law the train survives by construction: an
/// unlicensed batch reaches no teardown at all, and a licensed one mints
/// without clearing. This pins that end-to-end through the style whose
/// entire wake IS the glitter, over real `geom()` cell centres: shed a
/// train, type the NEXT key, and count what is left.
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
    // Glitter shed over the cells the user already typed (columns 2..6),
    // each grain born inside its own cell exactly as `spawn_burst_particles`
    // places them.
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
    // …and one grain over a cell this batch is about to REWRITE.
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
    // THE NEXT KEY: a licensed typed advance whose batch also rewrites
    // the cell one of the grains is flying over.
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
    // The four grains over the cells the user typed — the trail proper —
    // are all still there, and so is the fifth: a grain is a projectile
    // already flying away from its birth pixel, so the rewritten cell
    // underneath it does not speak for it. Only age and the WHOLESALE
    // teardowns (reset / dark) retire the family.
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

/// GATE (lane-license, deliverable 1/2 engine half): `note_user_gesture`
/// KEEPS the banked typed stamps — the supersede shape. Two real keys are
/// banked with their echoes still in flight when a Tab-class gesture
/// lands; the gesture licenses its own sweep and the bank is intact
/// behind it.
///
/// RED-PROOF (2026-08-30, corrected by the refute round and re-verified
/// on the merged tree): with `note_user_gesture`'s body reverted to its
/// pre-fix `self.clear_typed(now)` wipe, this fails FIRST at the
/// `any_fresh` assert — "arming the gesture class must not destroy
/// banked typed stamps" — the wipe empties the bank before any sweep is
/// even observed.
///
/// The tail: the Tab's 4-cell sweep is a hop the two in-flight presses
/// cannot explain (`no-credits`, a forget edge), so the pool is
/// forgotten with it, and a stamp whose
/// press is no longer unpaid licenses nothing on its own (a stamp is a
/// licence only while the ledger owes a cell) — the `+1` that follows
/// is program output and is refused. Before that it was licensed on
/// the dangling stamp and laid a cell no press paid for.
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
    // Tab lands mid-burst: the gesture class arms WITHOUT wiping the bank.
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
    // …the two presses could not explain a 4-cell hop, so the sweep
    // forgot them, and the stamp the pop left fresh is not a licence
    // without a press behind it: the next `+1` is refused.
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

/// GATE (lane-license, deliverable 3): the completed hidden→visible
/// boundary spares typed stamps younger than `TYPE_HINT_FRESH` (real keys
/// whose echoes are in flight through a TUI's DECTCEM repaint bracket)
/// while still retiring a stale bank.
///
/// RED-PROOF (2026-08-30): with `retire_hidden_movement_provenance`
/// reverted to the unconditional `self.type_hint.clear()` +
/// ring wipe, the fresh half fails at the `any_fresh` assert (and the
/// echo spawns stay at 0 — the owner's-TUI-window frozen ledger shape).
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

    // The STALE half: a bank older than TYPE_HINT_FRESH is fully retired
    // at the boundary (the pre-fix guarantee, kept).
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

/// GATE (lane-license, deliverable 4, hidden half): with the DEC cursor
/// HIDDEN across frames (fc_hidden.py — the repaint bracket never
/// re-shows), a keystroke's echo is anchored to the print-run mutation
/// site and spawns there; program-only output (no stamps) paints nothing
/// and spawns nothing.
///
/// RED-PROOF (2026-08-30): with the `echo_anchor_pass` call removed from
/// `tick`, the licensed-echo assert fails with `spawns == 0` — the
/// fixture's measured total, ledger-silent suppression.
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
    // Program-only rewrite advancing a status row's end: no stamps →
    // nothing (and — row discrimination — that row is now branded a
    // program row; the INPUT row, which only ever advances with keys,
    // is not, so the echo below still lights).
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

/// GATE (lane-license, deliverable 4, parked half): with the DEC cursor
/// visible but PARKED at a row away from the caret (fc_parked.py — CUP
/// 1;1 before show), the licensed echo anchors to the mutated row; and a
/// VISIBLE cursor move always owns its own tick — the anchor lane must
/// not double-judge the same PTY delta.
///
/// RED-PROOF (2026-08-30): without `echo_anchor_pass` the parked assert
/// fails at `spawns == 0`; without the `cursor_move_observed` guard the
/// final assert fails at `spawns == 3` (double-judged echo, two stamps
/// spent on one key's move).
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

    // VISIBLE-MOVE OWNERSHIP: a normal shell echo (cursor moves with the
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

/// GATE (lane-license refute round, the stray-light defect): a PROGRAM
/// row's forward end-advance must NEVER consume a banked typed stamp.
/// The refuter's fixture shape, deterministically: cursor hidden across
/// frames, two interleaved rows — the input row (11) advances only after
/// `note_typed`, the status row (9) grows on its own ~150 ms cadence (the
/// Claude Code elapsed-timer/token-counter shape). During concurrent
/// typing the status row's advances land inside the typed stamps'
/// freshness window, and without row affinity the anchor lane judges and
/// LICENSES them — spending stamps that belonged to input echoes
/// (measured on glass: sweeps targeting the status row at seq 6/15/18/21,
/// saturated band slabs under "thinking....."). The fix: a row observed
/// advancing its end while NO user gesture was fresh is tainted as a
/// program row (a spinner advances keylessly all day; the input row only
/// advances with keys) and tainted rows refuse anchored sweeps for the
/// memory entry's lifetime.
///
/// RED-PROOF (2026-08-30, pre-fix lane code): the zero-spawns-target-
/// the-status-row assert fails with 1 licensed sweep at (9,11)->(9,12),
/// and the licensed == input-echo-count assert fails at spawns == 1 of 2
/// (the stray spent k2's stamp, so k2's real echo went dark).
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
    // The status row's KEYLESS advance (its 150 ms tick, nobody typing):
    // spawns nothing today and — post-fix — brands row 9 a program row.
    glow.observe_print_anchor(Some((9, 11, 3)));
    glow.tick(None, at(150), &c, g, &mut out);
    assert_eq!(glow.spawns(), 0, "keyless growth must stay dark");
    // k1 and its echo on the input row: licensed, anchored, lit.
    glow.note_typed(at(240));
    glow.observe_print_anchor(Some((11, 3, 4)));
    glow.tick(None, at(245), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the input echo must still light");
    // k2 banks a stamp; the status row's next 150 ms tick is sampled
    // BEFORE k2's echo (the refuter's interleave). Its end-advance lands
    // inside the stamp's freshness window.
    glow.note_typed(at(330));
    glow.observe_print_anchor(Some((9, 12, 5)));
    glow.tick(None, at(336), &c, g, &mut out);
    // The status row must never be JUDGED as an echo: any ring record on
    // row 9 other than the row-discrimination refusal itself is a spent
    // stamp (`spawns` counts it even when the shape gates then mint no
    // geometry in this harness — on glass it minted the saturated slabs).
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
    // …and because the stamp was NOT spent on the status row, k2's real
    // echo still lights: judged input-row echoes == keys typed.
    glow.observe_print_anchor(Some((11, 4, 6)));
    glow.tick(None, at(340), &c, g, &mut out);
    let input_row_echoes = glow.admission_log().filter(|r| r.origin.0 == 11).count();
    assert_eq!(
        input_row_echoes, 2,
        "every typed stamp funds its own input-row echo — none diverted"
    );
}

/// GATE (lane-license refute round, the SPOKEN-FOR arm): fc_hidden's
/// spinner counter holds a constant width until `(9)` becomes `(10)`, so
/// its first-ever end-advance has NO keyless history — the taint brand
/// alone cannot refuse it when it lands mid-burst inside a typed stamp's
/// freshness window. The second arm must: while a different row's
/// licensed anchored echo is younger than the stamp window, the echo lane
/// is spoken for and no other row may spend a stamp.
///
/// RED-PROOF (2026-08-30, taint-only build — the `spoken_for` conjunct
/// forced false): the zero-crossing-spends assert fails with 1 record at
/// (10,19)->(10,20), the crossing judged and the stamp diverted.
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
    // Spinner repaints at the SAME end column (no advance — no history
    // of any kind for row 10).
    glow.observe_print_anchor(Some((10, 19, 3)));
    glow.tick(None, at(150), &c, g, &mut out);
    // k1's echo establishes the input row as THE echo row.
    glow.note_typed(at(240));
    glow.observe_print_anchor(Some((12, 3, 4)));
    glow.tick(None, at(245), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the input echo must light");
    // k2 banks a stamp; the counter crosses its digit width — row 10's
    // FIRST advance, brandless, mid-burst, stamp-fresh.
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

/// GATE (lane-license correction round, the PERMANENT-TAINT
/// overcorrection): the taint brand exists to discriminate program rows
/// from THE echo row — it must never execute the echo row itself. The
/// re-refuter's shape: phase-1 typed echoes licensed on the input row
/// (the row is the established `last_anchor_sweep` holder); >250 ms
/// quiet, so every stamp goes stale; then an UNDELIVERED PASTE (the host
/// stamps the gesture at the input boundary and revokes it in the same
/// event-loop turn; the delivery edge that would re-arm it never fires —
/// the bytes never provably landed), whose echo advances that same row
/// with no license term fresh. KEPT AS THE NO-DELIVERY CONTROL:
/// a paste whose bytes never provably landed stays dark and
/// brands nothing; its delivered sibling is
/// `a_delivered_insert_lights_a_hidden_caret_tui_at_its_print_anchor`.
/// The undelivered advance itself must stay dark (nothing
/// licensed it), but branding the row a PROGRAM row for the entry's
/// lifetime overcorrects: every later typed echo at 90 ms is refused
/// `DECLINE_PROGRAM_ROW` and the TUI anchor is dead until a scroll or
/// reset tears the coordinate space down. The fix: an unlicensed advance
/// landing on the row currently holding `last_anchor_sweep` is exempt
/// from branding — that row earned its identity through a licensed
/// anchored echo, which is the one thing a spinner/status row can never
/// do (`last_anchor_sweep` has exactly one writer, the licensed anchored
/// spawn, and taint + spoken-for refuse a program row before it).
/// Identity, not freshness: the quiet gap is longer than any stamp
/// window, so a freshness-scoped exemption would not cover the repro.
///
/// RED-PROOF (2026-08-30, pre-fix lane code): the paste-stays-dark
/// assert holds, then the post-paste typed echoes are all refused —
/// `spawns` freezes at 2 (expected 5) and the zero-program-row-declines
/// assert fails with 3 `DECLINE_PROGRAM_ROW` records on the input row.
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
    // PHASE 1: typed echoes licensed on the input row — the row becomes
    // the established echo row (the `last_anchor_sweep` holder).
    glow.note_typed(at(30));
    glow.observe_print_anchor(Some((11, 3, 2)));
    glow.tick(None, at(35), &c, g, &mut out);
    glow.note_typed(at(120));
    glow.observe_print_anchor(Some((11, 4, 3)));
    glow.tick(None, at(125), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "phase-1 typed echoes must light");
    // >250 ms QUIET (every stamp stale), then the paste: gesture stamped
    // at the input boundary, revoked in the same turn (the host's
    // queue-boundary shape — neither FIFO enqueue nor detached write is
    // delivery), bytes echo and advance the input row 8 cells.
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
