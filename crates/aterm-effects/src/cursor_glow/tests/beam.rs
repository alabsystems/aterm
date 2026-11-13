// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Beam cursor-glow style tests.

use super::*;

#[test]
fn beam_parses_to_its_own_style() {
    assert_eq!(GlowStyle::parse("beam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse(" BEAM "), GlowStyle::Beam);
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

/// After-land lows W6: Beam's tube on a dark page is a white-hot core in an ice rod whose
/// faint sleeve keeps to its own row. Its sleeve once reached five and a half rods of a third
/// of a cell, and over a Backspace's erased cells it stood as a white slab between two nested
/// lavender boxes over the letters of the rows above and below (composed r3's judges). A run
/// typed along a blank row lights nothing in the row above (below the row lies the bed's rail
/// in the leading, [`BedShape::dark_rail`], the bed's light and not the sleeve's). The
/// control lays composed r3's tube (`beam_tube_r3`). Its ends' law is aterm-render's
/// `a_feathered_beam_rounds_each_layer_s_ends`.
#[test]
fn beam_s_tube_keeps_to_its_row() {
    let g = tall_geom();
    let blank = vec![' '; g.cols];
    let mut c = cfg(GlowStyle::Beam, true);
    c.intensity = 1.0;
    let row = 3u16;
    // Lit pixels in the row above after twelve spaces typed along the blank row (the air a
    // Backspace leaves): Beam lays its tube over a typed run in the air; a leap's path is its
    // bed's.
    let above = |r3: bool| {
        let mut glow = CursorGlow {
            beam_tube_r3: r3,
            ..CursorGlow::default()
        };
        let (mut out, mut t) = (Vec::new(), Instant::now());
        glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        glow.tick(Some((row, 4)), t, &c, g, &mut out);
        for k in 1..=12u16 {
            t += Duration::from_millis(57);
            glow.note_synthetic_typed(t, 1);
            glow.observe_row(row, 4 + k, &blank, t);
            glow.observe_neighbor_rows(Some(&blank), Some(&blank));
            glow.tick(Some((row, 4 + k)), t, &c, g, &mut out);
        }
        let mut lit = Glass::new(g, 0);
        lit.add_quads(glow.under_quads());
        lit.add_quads(&out);
        lit.add_halos(glow.halos());
        let (mut n, mut tube) = (0usize, 0usize);
        for y in 0..i32::from(row + 1) * g.ch as i32 {
            for x in 0..g.win_w as i32 {
                if lit.level(y, x) > 2 {
                    if y < i32::from(row) * g.ch as i32 {
                        n += 1;
                    } else {
                        tube += 1;
                    }
                }
            }
        }
        assert!(tube > 0, "fixture: the run lays its tube");
        n
    };
    let lit = above(false);
    assert_eq!(lit, 0, "the tube lights {lit} px of the row above");
    assert!(
        above(true) > 0,
        "control: composed r3's sleeve reaches into the row above"
    );
}
