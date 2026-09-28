// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wrap re-anchors, bridged moves and box growth.

use super::*;

/// TYPED RE-ANCHOR LAW (the Claude Code line-fill fix): a typed-hinted
/// one-row move beyond the typed advance — an Ink input box rewrapping its
/// INSET prompt, launching from the interior right edge and landing right
/// of column 1, so the bare-terminal SHAPE detector misses — is CONTINUED
/// TYPING for EVERY style: no fire meteor, no rainbow ZOOM, no lightning
/// bolt, no landing ring, no jump-scale particle burst, and the only new
/// wake is the single landing spark. The paired flare stays cold (nothing
/// slams), so the next keystrokes on the new line don't erupt.
#[test]
fn claude_code_wrap_reanchors_all_styles() {
    let g = geom(); // rows:6 cols:40 — the Ink wrap is (4,36) -> (5,3)
    let t0 = Instant::now();
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
        let mut out = Vec::new();
        glow.tick(Some((4, 36)), t0, &c, g, &mut out); // caret at the box's right edge
        // A scripted typed candidate exercises the post-admission
        // re-anchor morphology without treating its timestamp as proof.
        glow.note_synthetic_typed(t0 + Duration::from_millis(5), 1);
        // …and the Ink repaint re-anchors the caret one row down, LEFT of
        // its launch column (dr=1, dc=-33 — the shape detector misses).
        glow.tick(
            Some((5, 3)),
            t0 + Duration::from_millis(10),
            &c,
            g,
            &mut out,
        );
        assert!(
            glow.fire_meteors.is_empty(),
            "{style:?}: a typed re-anchor flies no meteor"
        );
        assert!(
            glow.bolts.is_empty(),
            "{style:?}: a typed re-anchor strikes no lightning"
        );
        assert!(
            glow.ring.is_none(),
            "{style:?}: a typed re-anchor pings no landing ring"
        );
        assert!(
            glow.flare < 0.05,
            "{style:?}: a typed re-anchor never slams the flare ({})",
            glow.flare
        );
        // The wake is the single landing spark — nothing swept across the
        // cells the caret never travelled (right of the landing, or the
        // interpolated diagonal back to the old row's right edge).
        assert!(
            !glow.sparks.iter().any(|s| s.row == 5 && s.col > 3),
            "{style:?}: no wrap smear right of the landing: {:?}",
            glow.sparks
                .iter()
                .filter(|s| s.row == 5)
                .map(|s| s.col)
                .collect::<Vec<_>>()
        );
        // …and none on the DEPARTED row either: a half-regression that
        // sweeps only the old row would otherwise pass (adversarial
        // review). The first tick lays no wake (spawn needs a previous
        // position), so after both ticks the only legitimate spark is the
        // landing cell.
        assert!(
            !glow.sparks.iter().any(|s| s.row == 4),
            "{style:?}: no smear on the departed row: {:?}",
            glow.sparks
                .iter()
                .filter(|s| s.row == 4)
                .map(|s| s.col)
                .collect::<Vec<_>>()
        );
        assert!(
            glow.particles.len() <= 8,
            "{style:?}: particle burst stays typing-scale, got {}",
            glow.particles.len()
        );
        assert_invariants(&out, g);
    }
}

/// HIDE-BRIDGE LAW under explicit candidates: a ConPTY-bridged move is
/// chebyshev ≤ 2, and the re-anchor's `raw_dist > 2` guard keeps generic and
/// typed synthetic morphology byte-identical frame for frame.
#[test]
fn bridged_two_cell_move_is_byte_identical_with_hint() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let drive = |hint: bool| -> Vec<u64> {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut fps = Vec::new();
        fps.push(glow.tick(Some((2, 5)), t0, &c, g, &mut out));
        // ConPTY echo choreography: a hidden frame presents…
        fps.push(glow.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out));
        let armed = t0 + Duration::from_millis(20);
        if hint {
            glow.note_synthetic_typed(armed, 1);
        } else {
            glow.note_synthetic_move(armed);
        }
        // …then the cursor reappears a bridgeable (dr=1, dc=2) hop away.
        for ms in [45u64, 61, 77, 200] {
            fps.push(glow.tick(
                Some((3, 7)),
                t0 + Duration::from_millis(ms),
                &c,
                g,
                &mut out,
            ));
        }
        fps
    };
    assert_eq!(
        drive(false),
        drive(true),
        "a bridgeable move must not change by one byte under the typed hint"
    );
}

/// BOX-GROWTH WRAP morphology (live-verified in Claude Code): the
/// bottom-anchored input box grows a row up, so the caret's terminal row
/// stays constant while its column jumps. An explicitly admitted typed
/// preview plus repaint blink re-anchors without a meteor, and the same
/// shape with no press behind it stays wholly dark.
#[test]
fn box_growth_preview_reanchors_but_cold_output_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // An explicitly scripted typed dr==0 wrap under alt+blink re-anchors.
    let mut glow = CursorGlow::default();
    glow.note_context(true);
    glow.tick(Some((5, 36)), t0, &c, g, &mut out);
    glow.note_repaint_blink(t0 + Duration::from_millis(10));
    glow.note_synthetic_typed(t0 + Duration::from_millis(10), 1);
    let t1 = t0 + Duration::from_millis(26);
    glow.tick(Some((5, 3)), t1, &c, g, &mut out);
    assert!(
        glow.fire_meteors.is_empty(),
        "box-growth wrap (dr==0) re-anchors: no meteor"
    );
    assert!(glow.flare < 0.5, "no flare slam on the wrap");

    // The SAME shape with no key behind it stays wholly dark.
    let mut cold = CursorGlow::default();
    cold.note_context(true);
    cold.tick(Some((5, 36)), t0, &c, g, &mut out);
    cold.note_repaint_blink(t0 + Duration::from_millis(10));
    cold.tick(Some((5, 3)), t1, &c, g, &mut out);
    assert!(cold.fire_meteors.is_empty());
    assert!(!frame_has_output(&out, &cold));
}

#[test]
fn synthetic_alt_without_blink_keeps_the_jump() {
    let g = geom();
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Explicit alt-screen preview, no blink: the jump keeps its drama.
    let mut fire = CursorGlow::default();
    fire.note_context(true);
    fire.tick(Some((4, 36)), t0, &cfg(GlowStyle::Fire, true), g, &mut out);
    fire.note_synthetic_typed(t0 + Duration::from_millis(5), 1);
    fire.note_context(true);
    fire.tick(
        Some((5, 3)),
        t0 + Duration::from_millis(10),
        &cfg(GlowStyle::Fire, true),
        g,
        &mut out,
    );
    assert_eq!(
        fire.fire_meteors.len(),
        1,
        "admitted alt-screen preview + no blink: the fire meteor still flies"
    );

    // (The rainbow kitty arms of this law died with v1: the ZOOM streak
    // was v1's; v2's meteor has its own admission laws in
    // `rainbow_kitty::meteor`.)
    let mut cc = CursorGlow::default();
    cc.note_context(true);
    cc.tick(Some((4, 36)), t0, &cfg(GlowStyle::Fire, true), g, &mut out);
    cc.note_synthetic_typed(t0 + Duration::from_millis(5), 1);
    cc.note_repaint_blink(t0 + Duration::from_millis(8));
    cc.note_context(true);
    cc.tick(
        Some((5, 3)),
        t0 + Duration::from_millis(10),
        &cfg(GlowStyle::Fire, true),
        g,
        &mut out,
    );
    assert!(
        cc.fire_meteors.is_empty(),
        "typed candidate + fresh blink: the Ink wrap re-anchors"
    );
}
