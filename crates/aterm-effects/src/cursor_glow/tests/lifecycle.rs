// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Light is born on a move, fades to empty, and keeps its invariants.

use super::*;

#[test]
fn no_light_until_a_move() {
    let mut glow = CursorGlow::default();
    let now = Instant::now();
    let mut out = Vec::new();
    assert_eq!(
        glow.tick(
            Some((2, 0)),
            now,
            &cfg(GlowStyle::Lumen, true),
            geom(),
            &mut out
        ),
        0
    );
    assert!(out.is_empty());
    assert!(!glow.is_active());
}

#[test]
fn jump_spawns_light_and_fades_to_empty() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed
    glow.note_synthetic_move(t0);
    let fp = glow.tick(Some((2, 30)), t0, &c, g, &mut out); // jump
    assert_ne!(fp, 0);
    assert!(!out.is_empty());
    assert!(glow.is_active());
    assert_invariants(&out, g);
    // Past the duration + ring + crown windows → fully empty, idle.
    let gone = glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(600),
        &c,
        g,
        &mut out,
    );
    assert_eq!(gone, 0, "aurora must decay to EXACTLY empty (0% idle)");
    assert!(out.is_empty());
    assert!(!glow.is_active());
}

/// Typing continuity: single-cell advances at a 300 ms cadence chain, so
/// mid-gap the glow is still emitting and active; the streak never goes dark
/// between keys, and the renderer's SDR attack envelope does not reset per
/// key.
#[test]
fn typing_chain_never_goes_dark() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed position
    // Three keystroke advances 300ms apart (keys at +0.3s, +0.6s, +0.9s).
    for k in 1..=3u64 {
        let at = t0 + Duration::from_millis(300 * k);
        glow.note_synthetic_typed(at, 1);
        glow.tick(Some((2, k as u16)), at, &c, g, &mut out);
    }
    // Mid-gap probe, 280 ms after the third key: the chained spark
    // (gap+margin = 400 ms) and the 350 ms typing crown still shine.
    let fp = glow.tick(
        Some((2, 3)),
        t0 + Duration::from_millis(300 * 3 + 280),
        &c,
        g,
        &mut out,
    );
    assert!(glow.is_active(), "chained glow stays active mid-gap");
    assert!(
        !out.is_empty() && fp != 0,
        "chained glow still EMITS light 280ms after a 300ms-cadence key"
    );
    assert_invariants(&out, g);
}

/// ConPTY hide-bridge law: conhost hides the cursor for ~20-35 ms on every
/// keystroke echo (hide → advance → show). A presented hidden frame between
/// two adjacent positions must not eat the spark: the glow bridges the hide
/// and spawns from the last visible cell.
#[test]
fn conpty_hide_gap_still_spawns_the_typing_spark() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 5)), t0, &c, g, &mut out); // visible at col 5
    // ConPTY echo choreography: a ~30 ms hidden frame presents…
    glow.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out);
    // …then the cursor reappears one cell right (the echoed character).
    glow.note_synthetic_typed(t0 + Duration::from_millis(30), 1);
    let fp = glow.tick(
        Some((2, 6)),
        t0 + Duration::from_millis(45),
        &c,
        g,
        &mut out,
    );
    assert!(
        !out.is_empty() && fp != 0 && glow.is_active(),
        "a hide-bridged single-cell advance must spawn the typing spark"
    );
}

/// The bridge is conservative: a far teleport mid-hide (a full-screen app
/// repainting with the cursor hidden) spawns nothing.
#[test]
fn hidden_far_teleport_stays_suppressed() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 5)), t0, &c, g, &mut out);
    glow.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out);
    // Reappears far away (row 5, col 30): not a typing move, so no spark.
    let fp = glow.tick(
        Some((5, 30)),
        t0 + Duration::from_millis(45),
        &c,
        g,
        &mut out,
    );
    assert_eq!(fp, 0, "a far teleport out of a hide must not spawn");
    assert!(out.is_empty());
    // A long hide (alt-screen app) suppresses even an adjacent reappear.
    let mut glow2 = CursorGlow::default();
    glow2.tick(Some((2, 5)), t0, &c, g, &mut out);
    glow2.tick(None, t0 + Duration::from_millis(15), &c, g, &mut out);
    let fp2 = glow2.tick(
        Some((2, 6)),
        t0 + Duration::from_millis(400), // > HIDE_BRIDGE_MS after last visible
        &c,
        g,
        &mut out,
    );
    assert_eq!(fp2, 0, "a long hide must not bridge");
    assert!(out.is_empty());
}

/// A lone keystroke keeps the classic crisp wake: no chain floor applies (the
/// burst-opening gap is infinite), so everything is fully dark ~500 ms later.
#[test]
fn lone_key_wake_fades_crisply() {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    let g = geom();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed
    glow.note_synthetic_typed(t0 + Duration::from_millis(10), 1);
    glow.tick(
        Some((2, 1)),
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    ); // one advance
    let gone = glow.tick(
        Some((2, 1)),
        t0 + Duration::from_millis(10 + 500),
        &c,
        g,
        &mut out,
    );
    assert_eq!(gone, 0, "a lone keystroke's wake is gone by +500ms");
    assert!(out.is_empty());
    assert!(!glow.is_active());
}

#[test]
fn every_style_keeps_the_invariants() {
    let g = geom();
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        // Classic emits through the same `push_fx_rect` / `comet_glow_quads`
        // seams but via its own engine, so it is checked here for the same
        // single-cell-row banding and effects-box clamp.
        GlowStyle::Classic,
    ] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let c = cfg(style, true);
        glow.tick(Some((3, 5)), t0, &c, g, &mut out);
        // A big multi-row jump exercises comet + crown + ring + particles.
        glow.note_synthetic_move(t0);
        glow.tick(Some((1, 38)), t0, &c, g, &mut out);
        assert!(!out.is_empty(), "{style:?}: expected light on a jump");
        assert_invariants(&out, g);
        for ms in [16u64, 80, 160] {
            glow.tick(
                Some((1, 38)),
                t0 + Duration::from_millis(ms),
                &c,
                g,
                &mut out,
            );
            assert_invariants(&out, g);
        }
    }
}
