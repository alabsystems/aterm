// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Beam cursor-glow style tests.

use super::*;

#[test]
fn beam_parses_to_its_own_style() {
    assert_eq!(GlowStyle::parse("beam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse(" BEAM "), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse("lightbeam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse("comet"), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse("lumen"), GlowStyle::Lumen);
    assert_eq!(GlowStyle::parse("laser"), GlowStyle::Laser);
    assert!(GlowStyle::Beam.has_particles());
}

#[test]
fn beam_jump_is_one_clean_full_vector_rod() {
    let g = geom();
    let c = cfg(GlowStyle::Beam, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((2, 30)), t0, &c, g, &mut out); // a same-row tractor leap
    assert!(!glow.under_quads().is_empty(), "the rod is laid at spawn");
    assert_invariants(&out, g);
    assert!(
        glow.sparks.len() > c.length,
        "the rod ignores cfg.length and spans the jump ({} sparks)",
        glow.sparks.len()
    );
    assert!(glow.bolts.is_empty(), "a beam is not a lightning weapon");
    assert!(
        glow.particles.is_empty(),
        "the tube arrives alone: a leap's dust is the comet's ({} motes)",
        glow.particles.len()
    );
    let head_cov = glow.sparks.last().expect("head").born_cov as f32;
    let tail_cov = glow.sparks.first().expect("tail").born_cov as f32;
    assert!(
        tail_cov >= head_cov * 0.65,
        "a coherent rod, not a dying comet (tail {tail_cov} vs head {head_cov})"
    );
    let fp = glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(2000),
        &c,
        g,
        &mut out,
    );
    assert!(glow.sparks.is_empty(), "every spark drains");
    assert_eq!(fp, 0, "the rod decays to exactly empty");
}

#[test]
fn beam_typing_never_goes_dark_mid_rhythm() {
    let g = geom();
    let c = cfg(GlowStyle::Beam, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=8u16 {
        glow.tick(
            Some((2, i - 1)),
            t + Duration::from_millis(140),
            &c,
            g,
            &mut out,
        );
        if i > 1 {
            assert!(
                lum(&out) > 0 || !glow.halos().is_empty(),
                "key {i}: the rod went dark inside a 150ms rhythm"
            );
        }
        t += Duration::from_millis(150);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
}
