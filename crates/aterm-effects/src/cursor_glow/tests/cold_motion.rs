// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold program motion stays dark.

use super::*;

/// ANTI-STRAY (owner: "a stray piece of trail appear without text"): a
/// COLD cursor delta — the one-cell shape a token streamer walks, and the
/// large warp of a spinner/TUI repaint — emits NO light in ANY channel, in
/// EVERY selectable style.
///
/// THE RE-PIN, and why the style sweep is here rather than beside the
/// engine-state twin (`an_unlicensed_move_mints_nothing_and_retires_
/// nothing_in_every_style`): this asserts PRESENTED OUTPUT — the frame
/// fingerprint plus all six accessor planes the compositor copies. The
/// proof era's gate declined the ZOOM branch and then fell through to
/// generic ribbon sparks, a landing ring and an unconditional crown, so
/// "the jump pools are empty" was never the claim worth making. Under the
/// license there is no fall-through at all: an unlicensed move returns
/// before `classify_move`.
#[test]
fn cold_program_motion_emits_no_rainbow_light() {
    let g = geom();
    let t0 = Instant::now();
    let mut out = Vec::new();
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
        GlowStyle::Custom,
    ] {
        let mut c = cfg(style, true);
        if style == GlowStyle::Custom {
            c.pack = Some(TrailParams::defaults());
        }
        let mut glow = CursorGlow::default();
        assert_eq!(glow.tick(Some((0, 0)), t0, &c, g, &mut out), 0);
        assert!(!frame_has_output(&out, &glow));
        // A one-cell output stream followed by repeated cold spinner
        // warps; no committed key, Return or reflow gesture ever licenses
        // any of them.
        let mut t = t0;
        for &(cr, cc) in &[(0u16, 1u16), (3, 30), (0, 5), (4, 35), (1, 12), (5, 20)] {
            t += Duration::from_millis(120);
            let fingerprint = glow.tick(Some((cr, cc)), t, &c, g, &mut out);
            assert!(
                !frame_has_output(&out, &glow),
                "{style:?}: unlicensed program move to ({cr},{cc}) emitted visible light: \
                 fp={fingerprint:#x}, quads={}, halos={}, under={}, patches={}, \
                 chars={}, halo_cells={}",
                out.len(),
                glow.halos().len(),
                glow.under_quads().len(),
                glow.patches().len(),
                glow.charred().len(),
                glow.halo_cells().len(),
            );
            assert_eq!(
                fingerprint, 0,
                "{style:?}: unlicensed program move to ({cr},{cc}) changed the frame"
            );
            assert!(
                glow.sparks.is_empty()
                    && glow.particles.is_empty()
                    && glow.ring.is_none()
                    && glow.crown_until.is_none()
                    && glow.v2_status().is_none_or(|s| s.fp == 0),
                "{style:?}: unlicensed motion retained future or resident light"
            );
            assert!(
                !glow.is_active(),
                "{style:?}: a dark program move must stay idle"
            );
            // …and no THERMAL: the integrators every earned-drama consumer
            // reads stay at rest. v0.43.0 let this exact one-cell advance
            // buy heat; the license is deliberately stronger.
            assert_eq!(
                [glow.heat, glow.coal, glow.flare, glow.quench,],
                [0.0f32; 4],
                "{style:?}: unlicensed motion to ({cr},{cc}) bought thermals"
            );
            assert_eq!(
                glow.typing_momentum(t),
                0.0,
                "{style:?}: unlicensed motion earned momentum"
            );
        }
        assert_eq!(
            glow.drain_sound_cues().count(),
            0,
            "{style:?}: a visually declined move is silent too"
        );
        assert!(
            glow.last_type.is_none(),
            "{style:?}: program output earns no cadence"
        );
        assert_eq!(
            glow.admission_tally().last_decline_reason,
            Some(CursorGlow::DECLINE_NO_FRESH_HINT),
            "{style:?}: and the ring names the license seam"
        );
    }
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((0, 0)), t0, &c, g, &mut out);
    for &(cr, cc) in &[(0u16, 1u16), (3, 30)] {
        t += Duration::from_millis(120);
        glow.tick(Some((cr, cc)), t, &c, g, &mut out);
    }

    // A far hidden→visible relocation is outside the bounded ConPTY
    // bridge, so `spawn` never runs. Its declined Return/Tab-paste licence
    // must still be consumed at that decision boundary; otherwise the next
    // one-cell program update can borrow it and create the reported stray.
    for arm in [
        CursorGlow::note_typed as fn(&mut CursorGlow, Instant),
        CursorGlow::note_return as fn(&mut CursorGlow, Instant),
        CursorGlow::note_user_gesture,
    ] {
        let mut hidden = CursorGlow::default();
        hidden.tick(Some((0, 0)), t0, &c, g, &mut out);
        hidden.tick(None, t0 + Duration::from_millis(1), &c, g, &mut out);
        hidden.kill_hint = Some(t0);
        hidden.bs_poof_hint = Some(t0);
        hidden.bs_baseline = Some((0, 4));
        arm(&mut hidden, t0 + Duration::from_millis(2));
        let declined = hidden.tick(
            Some((0, 20)),
            t0 + Duration::from_millis(3),
            &c,
            g,
            &mut out,
        );
        assert_eq!(declined, 0);
        assert!(!frame_has_output(&out, &hidden));
        assert!(
            !hidden.type_hint.armed()
                && hidden.quench_hint.is_none()
                && hidden.nav_hint.is_none()
                && hidden.return_hint.is_none()
                && hidden.user_gesture_hint.is_none()
                && hidden.reflow_hint.is_none()
                && hidden.kill_hint.is_none()
                && hidden.bs_poof_hint.is_none()
                && hidden.bs_baseline.is_none(),
            "a declined hidden relocation consumes movement and row-bound proof"
        );
        let next = hidden.tick(
            Some((0, 21)),
            t0 + Duration::from_millis(4),
            &c,
            g,
            &mut out,
        );
        assert_eq!(next, 0, "the next cold move cannot borrow the hint");
        assert!(!frame_has_output(&out, &hidden));
    }

    // SAME-CELL hidden completion never enters `spawn` and is not a
    // declined relocation, but it still closes the authored hide/echo
    // choreography. The ONE-SHOT classes (Return, Tab/paste) are consumed
    // at that boundary so the following cold CUP move cannot borrow them.
    for arm in [
        CursorGlow::note_return as fn(&mut CursorGlow, Instant),
        CursorGlow::note_user_gesture,
    ] {
        let mut hidden = CursorGlow::default();
        hidden.tick(Some((2, 4)), t0, &c, g, &mut out);
        hidden.tick(None, t0 + Duration::from_millis(1), &c, g, &mut out);
        arm(&mut hidden, t0 + Duration::from_millis(2));
        let same = hidden.tick(Some((2, 4)), t0 + Duration::from_millis(3), &c, g, &mut out);
        assert_eq!(same, 0);
        assert!(!frame_has_output(&out, &hidden));
        assert!(
            !hidden.type_hint.armed()
                && hidden.quench_hint.is_none()
                && hidden.nav_hint.is_none()
                && hidden.return_hint.is_none()
                && hidden.user_gesture_hint.is_none()
                && hidden.newline_hint.is_none()
                && hidden.reflow_hint.is_none()
                && hidden.type_press_ring.is_empty(),
            "same-cell hidden completion consumes every one-shot movement class"
        );
        let next = hidden.tick(Some((2, 5)), t0 + Duration::from_millis(4), &c, g, &mut out);
        assert_eq!(next, 0, "cold move cannot borrow same-cell hide provenance");
        assert!(!frame_has_output(&out, &hidden));
    }

    // …the BANKED TYPED stamps, by contrast, are per-press licenses whose
    // echoes may legitimately land AFTER the bracket completes (2026-08-30,
    // deliverable 3): a TUI hides inside DEC-2026 per keystroke, so a
    // frame catching the hide phase used to wipe the bank mid-burst and
    // orphan every in-flight echo (the owner's-TUI-window unlit notches).
    // A FRESH stamp now survives a same-cell completion and licenses the
    // one echo it was banked for — bounded exactly as always (one stamp,
    // one sweep, TYPE_HINT_FRESH). Stale stamps are still retired (see
    // `hidden_boundary_spares_fresh_typed_stamps_and_retires_stale`).
    {
        let mut hidden = CursorGlow::default();
        hidden.tick(Some((2, 4)), t0, &c, g, &mut out);
        hidden.tick(None, t0 + Duration::from_millis(1), &c, g, &mut out);
        hidden.note_typed(t0 + Duration::from_millis(2));
        let same = hidden.tick(Some((2, 4)), t0 + Duration::from_millis(3), &c, g, &mut out);
        assert_eq!(same, 0);
        assert!(!frame_has_output(&out, &hidden));
        assert!(
            hidden.type_hint.armed(),
            "a fresh typed stamp survives its own key's hide bracket"
        );
        hidden.tick(Some((2, 5)), t0 + Duration::from_millis(4), &c, g, &mut out);
        assert_eq!(
            hidden.spawns(),
            1,
            "the surviving stamp licenses exactly the echo it was banked for"
        );
        // …and ONLY that echo: the bank is spent, the next cold program
        // move finds no license.
        let cold = hidden.tick(Some((2, 9)), t0 + Duration::from_millis(5), &c, g, &mut out);
        let _ = cold;
        assert_eq!(
            hidden.spawns(),
            1,
            "a second cold move cannot borrow anything past the spent bank"
        );
    }

    // A fresh/reset engine cannot draw an authored landing that completed
    // before its first visible sample because it has no honest source
    // anchor. The seed tick consumes every class dark; otherwise the next
    // cold CUP can borrow that already-completed input/reflow witness.
    for reset_first in [false, true] {
        for arm in [
            CursorGlow::note_typed as fn(&mut CursorGlow, Instant),
            CursorGlow::note_backspace,
            CursorGlow::note_navigation,
            CursorGlow::note_return,
            CursorGlow::note_user_gesture,
            CursorGlow::note_newline_break,
            CursorGlow::note_reflow,
        ] {
            let mut unseeded = CursorGlow::default();
            if reset_first {
                unseeded.tick(Some((4, 4)), t0, &c, g, &mut out);
                unseeded.reset();
            }
            arm(&mut unseeded, t0 + Duration::from_millis(1));
            let seed = unseeded.tick(
                Some((4, 10)),
                t0 + Duration::from_millis(2),
                &c,
                g,
                &mut out,
            );
            assert_eq!(seed, 0, "a source-less seed changes no frame");
            assert!(!frame_has_output(&out, &unseeded));
            assert!(
                !unseeded.type_hint.armed()
                    && unseeded.quench_hint.is_none()
                    && unseeded.nav_hint.is_none()
                    && unseeded.return_hint.is_none()
                    && unseeded.user_gesture_hint.is_none()
                    && unseeded.newline_hint.is_none()
                    && unseeded.reflow_hint.is_none()
                    && unseeded.type_press_ring.is_empty(),
                "a source-less seed consumes every movement class"
            );
            let next = unseeded.tick(
                Some((4, 11)),
                t0 + Duration::from_millis(3),
                &c,
                g,
                &mut out,
            );
            assert_eq!(next, 0, "cold CUP cannot borrow a pre-seed class");
            assert!(!frame_has_output(&out, &unseeded));
        }
    }
    // Tier 1: a hidden->visible completion that reaches no `spawn` decision
    // consumes every class without licensing anything — the licence model's
    // swallow disposition. A stamp that survives it is the control.
    let hidden_model = aterm_spec::derive::cursor_hint_license_model();
    let mut hidden_source = hidden_model.init_state();
    hidden_source.insert("hint", 1);
    hidden_source.insert("arms", 1);
    hidden_source.insert("credit_arms", 1);
    let mut hidden_projected = hidden_source.clone();
    hidden_projected.insert("hint", 0);
    hidden_projected.insert("cleared", 1);
    hidden_projected.insert("swallowed", 1);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &hidden_model,
        &[],
        &hidden_source,
        &hidden_projected,
        Some("SwallowedKeyClearsLicense"),
        "CursorGlow declined hidden relocation",
    );
    assert!(ok, "real hidden-decline transition rejected: {why}");
    let mut sticky_hidden = hidden_projected;
    sticky_hidden.insert("hint", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &hidden_model,
        &[],
        &hidden_source,
        &sticky_hidden,
        Some("SwallowedKeyClearsLicense"),
        "CursorGlow declined-hidden negative control",
    );
    assert!(!ok, "a declined hidden glow hint must not survive");
}

/// Positive visible-output controls for explicit preview candidates, plus
/// dark controls for every legacy timestamp-only classifier.
#[test]
fn synthetic_rainbow_moves_are_visible_and_cold_output_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    for (label, typed, target) in [
        ("synthetic typed", true, (0, 1)),
        ("synthetic move", false, (3, 30)),
    ] {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(16);
        let sample = t1 + Duration::from_millis(16);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((0, 0)), t0, &c, g, &mut out);
        if typed {
            glow.note_synthetic_typed(t1, 1);
        } else {
            glow.note_synthetic_move(t1);
        }
        glow.tick(Some(target), t1, &c, g, &mut out);
        let fingerprint = glow.tick(Some(target), sample, &c, g, &mut out);
        assert_ne!(fingerprint, 0, "{label} move must change the frame");
        assert!(
            frame_has_output(&out, &glow),
            "{label} move must present visible output"
        );
        assert!(
            glow.crown_until.is_some(),
            "{label} admission keeps the cursor crown"
        );
        assert!(glow.is_active(), "{label} light keeps animation armed");
        assert!(
            glow.v2_status().is_some_and(|s| s.fp != 0),
            "{label} lays visible trail light"
        );
    }

    // …and every key class STALE past its own freshness window is inert:
    // the same warp with a 0.6 s-old stamp is program output as far as
    // the license is concerned. `reflow` is not a license term at all
    // (a resize is not a keypress), so it stays dark even while fresh.
    type Arm = fn(&mut CursorGlow, Instant);
    let stale: [(&str, Arm); 6] = [
        ("typed", CursorGlow::note_typed),
        ("Backspace", CursorGlow::note_backspace),
        ("navigation", CursorGlow::note_navigation),
        ("Return", CursorGlow::note_return),
        ("gesture (Tab / scripted)", CursorGlow::note_user_gesture),
        ("reflow", CursorGlow::note_reflow),
    ];
    for (label, arm) in stale {
        let t0 = Instant::now();
        let pressed = t0 + Duration::from_millis(16);
        let late = pressed + Duration::from_millis(600);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((0, 4)), t0, &c, g, &mut out);
        arm(&mut glow, pressed);
        glow.tick(Some((3, 30)), late, &c, g, &mut out);
        assert!(
            !frame_has_output(&out, &glow),
            "{label}: a stale stamp forged light"
        );
        assert!(
            glow.crown_until.is_none(),
            "{label}: a stale stamp forged a crown"
        );
        assert_eq!(
            glow.admission_tally().last_decline_reason,
            Some(CursorGlow::DECLINE_NO_FRESH_HINT),
            "{label}: and the ring names the license seam"
        );
    }
}

/// The universal candidate gate is style-blind: a reflow timestamp cannot
/// birth a streak in any built-in style.
#[test]
fn every_style_keeps_a_timestamp_only_relayout_dark() {
    let g = geom();
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(120);
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
    ] {
        let c = cfg(style, true);
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        glow.tick(Some((0, 0)), t0, &c, g, &mut out);
        glow.note_reflow(t1);
        glow.tick(Some((3, 30)), t1, &c, g, &mut out);
        let laid =
            !glow.sparks.is_empty() || !glow.fire_meteors.is_empty() || !glow.particles.is_empty();
        assert!(!laid, "{style:?}: reflow timestamp forged a streak");
        assert!(!frame_has_output(&out, &glow), "{style:?}: must stay dark");
    }
}

/// One reflow timestamp produces no streaks, including across a relocation
/// train that follows it.
#[test]
fn one_reflow_timestamp_produces_no_streaks() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((0, 0)), t0, &c, g, &mut out);
    let mut t = t0 + Duration::from_millis(60);
    glow.note_reflow(t);
    // Five relayout-shaped leaps, one timestamp between them.
    for &(cr, cc) in &[(3u16, 30u16), (0, 5), (4, 35), (1, 12), (5, 20)] {
        t += Duration::from_millis(60);
        glow.tick(Some((cr, cc)), t, &c, g, &mut out);
    }
    assert_eq!(
        glow.v2_status().map_or(0, |s| s.meteors),
        0,
        "one reflow timestamp must produce no streaks"
    );
    assert!(!frame_has_output(&out, &glow));
}

/// THE RIBBON VANISHES SMOOTHLY (owner: "the rainbow on the cursor isn't
/// always smoothly vanishing").
///
/// The exit swoosh's drain used to DELETE the cells it selected, so the
/// vanish was a staircase of whole-cell removals — and the `settled_at`
/// branch then took every remaining cell to nothing in a single frame. This
/// walks a real typing run all the way out at 60 fps and bounds the largest
/// single-frame loss of ribbon light.
///
/// WHAT IS MEASURED CHANGED (2026-08-24), and it is a STRICTLY sharper test
/// than the one it replaces — it asks the question the defect is written in
/// instead of a proxy for it.
///
/// The old measure divided each frame's total light loss by the PEAK the
/// ribbon reached, and then had to argue a bound from an estimate ("a whole
/// cell is ~1/9 of the resident strip, so 6 % refutes deleting one"). Two
/// things are wrong with that. It is INDIRECT: a frame can lose 6 % of peak
/// by easing thirty cells a little or by deleting one outright, and the
/// number cannot tell them apart. And its denominator is not a property of
/// the drain at all: when the body's brightness moved onto the mark's own
/// coordinate the ribbon's far end began dissolving at every momentum, so
/// the PEAK fell ~18 % while the drain-time content ROSE — the identical
/// physical drain scored half again worse through the denominator alone.
///
/// The question is simply: what is the brightest cell any frame made VANISH?
/// A drain that eases has no answer above the raster's own floor. The
/// deleting drain this pin was written against answers with full body
/// coverage, and the `settled_at` cliff answers with several cells at once.
/// No estimate, no calibration, and the same bar for both presentations.
#[test]
fn rainbow_ribbon_vanishes_without_a_visible_step() {
    let g = geom();
    // Both presentations ride the SAME drain (swoosh schedule, retract
    // fade, envelopes) and are now measured on the same scale — their own
    // brightest live cell — so the two bounds no longer need separate
    // derivations.
    let drain = |tall: bool| -> (i64, i64, u32, usize) {
        let mut c = cfg(GlowStyle::RainbowKitty, true);
        c.ribbon_tall = tall;
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        let mut t = t0;
        glow.tick(Some((2, 4)), t, &c, g, &mut out);
        for i in 1..=10u16 {
            t += Duration::from_millis(90);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, 4 + i)), t, &c, g, &mut out);
        }

        // Ribbon light, split by the CELL it lands in. Each cell keeps its
        // BRIGHTEST coverage, which is the quantity the defect is stated in
        // ("the drain deleted the cells it selected"): a cell that is still
        // carrying visible coverage may not be absent on the next frame.
        // Keyed by COLUMN, not by (row, column): the underline strip
        // straddles the boundary between two text rows on purpose, so one
        // ribbon cell lands in two of them and a (row, col) key would count
        // every ordinary retirement twice.
        // The CARET cell (col 14) carries the head's ATTACH (2026-09-14:
        // the band continues one cell under the block) — the head's own
        // light, not a cell of its own: it leaves with the head, on the
        // same frame, and is not a second cell "retired in a block".
        let lit_by_cell = |glow: &CursorGlow| -> std::collections::BTreeMap<u16, u32> {
            let mut per_cell: std::collections::BTreeMap<u16, u32> =
                std::collections::BTreeMap::new();
            for q in glow.under_quads() {
                let col = q.x / g.cw as u16;
                if col == 14 {
                    continue;
                }
                let cov = ((q.color >> 16) & 0xff)
                    .max((q.color >> 8) & 0xff)
                    .max(q.color & 0xff);
                let e = per_cell.entry(col).or_default();
                *e = (*e).max(cov);
            }
            per_cell
        };
        let lit = |glow: &CursorGlow| -> i64 {
            glow.under_quads()
                .iter()
                .map(|q| {
                    let cov = ((q.color >> 16) & 0xff)
                        .max((q.color >> 8) & 0xff)
                        .max(q.color & 0xff) as i64;
                    cov * i64::from(q.w) * i64::from(q.h)
                })
                .sum()
        };

        let mut prev_cells = lit_by_cell(&glow);
        let (mut prev, mut peak, mut worst, mut most_gone) = (0i64, 0i64, 0u32, 0usize);
        for f in 0..200 {
            t += Duration::from_millis(16);
            out.clear();
            glow.tick(Some((2, 14)), t, &c, g, &mut out);
            let cells = lit_by_cell(&glow);
            let total = lit(&glow);
            peak = peak.max(total);
            // THE DEFECT, asked directly: what is the brightest cell that
            // this frame made VANISH, and how many went at once? A drain
            // that eases answers "one, already spent". A drain that deletes
            // what it selects answers with the full body coverage of
            // whatever it took, and the `settled_at` cliff answers with the
            // whole remaining ribbon.
            let mut gone = 0usize;
            for (cell, cov) in &prev_cells {
                if !cells.contains_key(cell) {
                    gone += 1;
                    if std::env::var_os("RAINBOW_DRAIN_TRACE").is_some() {
                        println!(
                            "tall={tall} f={f} cell={cell} vanished at cov={cov} \
                             total {prev}->{total} sparks={}",
                            glow.sparks.len()
                        );
                    }
                    worst = worst.max(*cov);
                }
            }
            most_gone = most_gone.max(gone);
            prev = total;
            prev_cells = cells;
        }
        (peak, prev, worst, most_gone)
    };

    // THE BAR, in coverage. A ribbon cell's body on this fixture runs
    // 40-115/255, and the deleting drain removed the cells it selected AT
    // that value; a cell that leaves at or under 20 has already shed at
    // least two thirds of its body and is a couple of counts of dark grey
    // on black. The bar is quoted against the body it is a fraction of
    // rather than against a whole-animation peak, so it means the same
    // thing on both presentations and does not move when the ribbon's
    // brightness is redistributed. Measured: the shipped law leaves at 15
    // on the tall body and 3 on the strip, and the law this pin was
    // originally tuned against leaves at 16 — the change did not spend this
    // budget, it tightened it.
    //
    // `20 -> 34` WITH THE STRIP RESTORATION (2026-08-30): the per-cell
    // peak this reads now carries the baseline strip on top of the ink
    // (close to `1.9x` the body — [`RAINBOW_STRIP_GAIN`]), and the drain's
    // last 16 ms frame therefore leaves from a proportionally taller
    // body. The bar means the same thing it did — at least two thirds of
    // the two-part body shed before the cell may vanish — measured 30.
    const SPENT: u32 = 34;

    // Classic TALL body.
    let (peak, end, worst, most_gone) = drain(true);
    assert!(
        peak > 10_000,
        "the tall fixture must lay a real ribbon: {peak}"
    );
    assert_eq!(end, 0, "and it must be fully gone by the end");
    assert!(
        worst <= SPENT,
        "the tall ribbon must ease out, not step: a cell vanished while \
         still carrying {worst}/255 coverage"
    );
    assert!(
        most_gone <= 1,
        "the tall drain must retire cells one at a time, not in blocks: \
         {most_gone} went in one frame"
    );

    // Explicit underline strip: the same drain and now the same yardstick.
    // It used to need a bound of its own because its peak is roughly a
    // quarter of the tall body's, and a peak-relative measure therefore read
    // the same physical step differently on the two geometries; coverage
    // does not care how many rows a presentation spends its light over.
    let (peak, end, worst, most_gone) = drain(false);
    assert!(
        peak > 10_000,
        "the underline fixture must lay a real ribbon: {peak}"
    );
    assert_eq!(end, 0, "and it must be fully gone by the end");
    assert!(
        worst <= SPENT,
        "the underline must ease out, not step: a cell vanished while \
         still carrying {worst}/255 coverage"
    );
    assert!(
        most_gone <= 1,
        "the underline drain must retire cells one at a time, not in \
         blocks: {most_gone} went in one frame"
    );
}

/// PRESS BUDGET: one keypress echoing as a multi-cell same-row hop (vim
/// normal-mode `w`, even when a statusline repaint arms the blink
/// discriminator) must NOT classify as coalesced typing and paint ribbon
/// over the skimmed word — N swept cells demand ≥N recent presses. A
/// genuine batched echo (N presses, one observed move) still coalesces.
#[test]
fn one_press_multi_cell_hop_is_not_coalesced_typing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // ONE press, five-cell hop (vim `w` shape): sparks stay confined to
    // the landing — no swept-ribbon cells over the skimmed word.
    let mut vim = CursorGlow::default();
    vim.tick(Some((2, 0)), t0, &c, g, &mut out);
    let t1 = t0 + Duration::from_millis(30);
    vim.note_synthetic_typed(t1, 1);
    vim.tick(Some((2, 5)), t1, &c, g, &mut out);
    let swept: Vec<u16> = v2_cols(&vim, 2)
        .into_iter()
        .filter(|c| (0..4).contains(c))
        .collect();
    assert!(
        swept.is_empty(),
        "one press must not ribbon the skimmed cells, got {swept:?}"
    );

    // FIVE presses batched into one observed five-cell move: coalesces.
    let mut batch = CursorGlow::default();
    batch.tick(Some((2, 0)), t0, &c, g, &mut out);
    let mut t = t0;
    for i in 0..5 {
        t += Duration::from_millis(10);
        if i == 4 {
            batch.note_synthetic_typed(t, 1);
        } else {
            batch.note_typed(t);
        }
    }
    batch.tick(Some((2, 5)), t, &c, g, &mut out);
    let swept = v2_cols(&batch, 2);
    assert!(
        (0..=4u16).all(|c| swept.contains(&c)),
        "a genuinely batched echo still sweeps the ribbon: {swept:?}"
    );
}
