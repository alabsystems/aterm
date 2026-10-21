// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold program motion stays dark.

use super::*;

/// A cold cursor delta (a token streamer's one-cell step, a spinner/TUI warp)
/// emits no light in any channel, in every selectable style. Asserts presented
/// output (frame fingerprint plus the six accessor planes the compositor
/// copies), not just empty jump pools.
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
    ] {
        let c = cfg(style, true);
        let mut glow = CursorGlow::default();
        assert_eq!(glow.tick(Some((0, 0)), t0, &c, g, &mut out), 0);
        assert!(!frame_has_output(&out, &glow));
        // A one-cell output stream followed by repeated cold spinner
        // warps; no key, Return or reflow gesture licenses any of them.
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
            // No thermal either: the integrators earned-drama consumers
            // read stay at rest.
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

    // A far hidden->visible relocation is outside the bounded ConPTY
    // bridge, so `spawn` never runs; its declined Return/Tab-paste licence
    // must still be consumed there, or the next one-cell program update
    // borrows it.
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

    // A same-cell hidden completion never enters `spawn`, but it closes the
    // hide/echo choreography: the one-shot classes (Return, Tab/paste) are
    // consumed so the following cold CUP move cannot borrow them.
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

    // Banked typed stamps are per-press licenses whose echoes may land after
    // the bracket completes (a TUI hides inside DEC-2026 per keystroke). A
    // fresh stamp survives a same-cell completion and licenses exactly the
    // one echo it was banked for; stale stamps are retired (see
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
        // Only that echo: the bank is spent, so the next cold move finds no
        // license.
        let cold = hidden.tick(Some((2, 9)), t0 + Duration::from_millis(5), &c, g, &mut out);
        let _ = cold;
        assert_eq!(
            hidden.spawns(),
            1,
            "a second cold move cannot borrow anything past the spent bank"
        );
    }

    // A fresh/reset engine has no source anchor for a landing that completed
    // before its first visible sample, so the seed tick consumes every class
    // dark; otherwise the next cold CUP could borrow that witness.
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
    // consumes every class without licensing anything (the licence model's
    // swallow disposition); a surviving stamp is the control.
    let hidden_model = aterm_models_effects::cursor_hint_license_model();
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

/// Explicit preview candidates light visible output; every timestamp-only
/// classifier stays dark.
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

    // Every key class past its own freshness window is inert: the same warp
    // with a 0.6 s-old stamp is program output. `reflow` is not a license
    // term (a resize is not a keypress), so it stays dark even while fresh.
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

/// The rainbow ribbon drains without a visible step.
///
/// Walks a real typing run all the way out at 60 fps and measures, per
/// frame, the brightest cell that vanished and how many vanished at once. A
/// draining ease leaves only cells already near the raster floor; a drain
/// that deletes the cells it selects, or a `settled_at` cliff, removes cells
/// at full body coverage. Measured in per-cell coverage rather than relative
/// to the ribbon's peak, so the bar is independent of how brightness is
/// distributed along the ribbon.
#[test]
fn rainbow_ribbon_vanishes_without_a_visible_step() {
    let g = geom();
    // Both presentations ride the same drain (swoosh schedule, retract fade,
    // envelopes) and are measured on the same scale.
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

        // Ribbon light per cell, keeping each cell's brightest coverage: a
        // cell still carrying visible coverage may not be absent on the next
        // frame. Keyed by column, not (row, column): the underline strip
        // straddles two text rows, so a (row, col) key would count each
        // retirement twice. The caret cell (col 14) carries the head's
        // attach and leaves with the head, not as a separate cell.
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
            // The brightest cell this frame made vanish, and how many went
            // at once. An easing drain answers "one, already spent"; a
            // deleting drain or a cliff answers with full body coverage.
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

    // The bar, in coverage: a ribbon cell's body is well above it, and a
    // deleting drain removes cells at body coverage.
    const SPENT: u32 = 34;

    // Classic tall body.
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

    // Explicit underline strip: the same drain and the same bar; per-cell
    // coverage does not depend on how many rows a presentation spreads its
    // light over.
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
