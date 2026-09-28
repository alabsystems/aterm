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
    // The tube leaves STARDUST in its wake (the space theme's motes).
    assert!(GlowStyle::Beam.has_particles());
}

/// A jumped BEAM is ONE SOLID ROD spanning the whole leap (the full-vector
/// rule it shares with laser) — no lightning bolts, only a modest wake of
/// weightless stardust — and it drains to exactly empty.
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
    // STARDUST, not exhaust: a modest weightless wake — every mote floats
    // (zero gravity) and lingers (star-length lives, not spark-length).
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

/// POWER-DOWN LAW: past its full-power hold the rod THINS — the lit
/// vertical extent at the rod's midpoint collapses toward a hairline as
/// the brightness dies, so the switch-off reads as one object powering
/// down (every bloom layer is a multiple of `core_thick`, so the extent
/// tracks the collapse).
#[test]
fn beam_powers_down_thinner() {
    let g = geom();
    let c = cfg(GlowStyle::Beam, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((2, 30)), t0, &c, g, &mut out);
    // Lit vertical extent of quads covering the rod's midpoint column.
    let extent_at = |out: &[GlowQuad]| -> i32 {
        let mx = (16 * g.cw + g.cw / 2) as u16; // column 16, mid-cell
        let (mut y0, mut y1) = (i32::MAX, i32::MIN);
        for q in out {
            if q.x <= mx && mx < q.x + q.w && q.color != 0 {
                y0 = y0.min(q.y as i32);
                y1 = y1.max((q.y + q.h) as i32);
            }
        }
        (y1 - y0).max(0)
    };
    let early = extent_at(&out);
    assert!(early > 0, "the rod is lit at its midpoint");
    // ~72% through the jump life (240ms): deep into the cosine power-down.
    glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(172),
        &c,
        g,
        &mut out,
    );
    let late = extent_at(&out);
    assert!(late > 0, "still powering down, not yet dark");
    assert!(
        late < early,
        "the tube thins as it powers down (early {early}px vs late {late}px)"
    );
}

/// TYPING CONTINUITY: at a human cadence the tube never goes dark between
/// keys — the chained spark lifetimes keep the rod lit mid-rhythm.
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
            // Since the stardust law the wake's body rides the HALO
            // stream (round motes); the quad stream carries the rod and
            // the dealt star accents. Lit is lit on either channel.
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

/// COOL-LIGHT LAW: with the photon ice-blue default the whole tube — body,
/// sheen, halo, white-mixed specular axis — keeps blue at least as strong
/// as red on every emitted quad, across spawn and power-down frames.
#[test]
fn beam_stays_cool_light() {
    let g = geom();
    let mut c = cfg(GlowStyle::Beam, true);
    c.color = BEAM_DEFAULT_COLOR;
    c.accent = BEAM_DEFAULT_COLOR;
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((1, 38)), t0, &c, g, &mut out);
    let mut all: Vec<GlowQuad> = out.clone();
    glow.note_synthetic_typed(t0 + Duration::from_millis(120), 1);
    glow.tick(
        Some((1, 39)),
        t0 + Duration::from_millis(120),
        &c,
        g,
        &mut out,
    );
    all.extend(out.iter().copied());
    assert!(!all.is_empty());
    for q in &all {
        let (r, b) = ((q.color >> 16) & 0xff, q.color & 0xff);
        assert!(b >= r, "beam quad left its cool hue: {:06x}", q.color);
    }
}
