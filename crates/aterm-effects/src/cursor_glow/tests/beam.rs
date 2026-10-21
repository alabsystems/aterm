// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The beam style.

use super::*;

#[test]
fn beam_parses_to_its_own_style() {
    assert_eq!(GlowStyle::parse("beam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse(" BEAM "), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse("lightbeam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse("comet"), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse("lumen"), GlowStyle::Lumen);
    assert_eq!(GlowStyle::parse("laser"), GlowStyle::Laser);
    // The tube leaves stardust in its wake.
    assert!(GlowStyle::Beam.has_particles());
}

/// A jumped beam is one solid rod spanning the whole leap (the full-vector
/// rule it shares with laser), with no lightning bolts and only a modest wake
/// of weightless stardust, and it drains to exactly empty.
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
    assert!(!out.is_empty(), "the rod is lit at spawn");
    assert_invariants(&out, g);
    // Full-vector: sparks span the leap even past the configured length (18).
    assert!(
        glow.sparks.len() > c.length,
        "the rod ignores cfg.length and spans the jump ({} sparks)",
        glow.sparks.len()
    );
    assert!(glow.bolts.is_empty(), "a beam is not a lightning weapon");
    // Stardust, not exhaust: a modest wake whose motes float (zero gravity) and
    // linger (star-length lives).
    assert!(!glow.particles.is_empty(), "a warp leap stirs stardust");
    assert!(
        glow.particles.len() <= 14,
        "a modest wake, not a shower ({} motes)",
        glow.particles.len()
    );
    for p in &glow.particles {
        assert_eq!(p.gy, 0.0, "space is weightless — no gravity on stardust");
        assert!(p.life >= 0.5, "stars linger, they don't blink out");
    }
    // Near-constant power: the tail spark carries ≥~65% of the head's power.
    let head_cov = glow.sparks.last().expect("head").born_cov as f32;
    let tail_cov = glow.sparks.first().expect("tail").born_cov as f32;
    assert!(
        tail_cov >= head_cov * 0.65,
        "a coherent rod, not a dying comet (tail {tail_cov} vs head {head_cov})"
    );
    // And the light drains to exactly empty.
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

/// Typing continuity: at a human cadence the tube never goes dark between
/// keys; chained spark lifetimes keep the rod lit mid-rhythm.
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
        // Just before the next key lands, the previous wake is still lit.
        glow.tick(
            Some((2, i - 1)),
            t + Duration::from_millis(140),
            &c,
            g,
            &mut out,
        );
        if i > 1 {
            // The wake's body rides the halo stream (round motes); the quad
            // stream carries the rod and the dealt star accents. Either counts.
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
