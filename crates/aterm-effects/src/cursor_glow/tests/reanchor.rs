// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wrap re-anchors, bridged moves and box growth.

use super::*;

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
        glow.note_synthetic_typed(t0 + Duration::from_millis(5), 1);
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
        assert!(
            !glow.sparks.iter().any(|s| s.row == 5 && s.col > 3),
            "{style:?}: no wrap smear right of the landing: {:?}",
            glow.sparks
                .iter()
                .filter(|s| s.row == 5)
                .map(|s| s.col)
                .collect::<Vec<_>>()
        );
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
        fps.push(glow.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out));
        let armed = t0 + Duration::from_millis(20);
        if hint {
            glow.note_synthetic_typed(armed, 1);
        } else {
            glow.note_synthetic_move(armed);
        }
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

#[test]
fn box_growth_preview_reanchors_but_cold_output_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::Fire, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

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
