// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comet style.

use super::*;

/// `comet` and `beam` each graduated to their own variant; `lumen` stays
/// the plain Lumen tracer (raw-string routing in the GUI resolver untouched).
#[test]
fn comet_parses_apart_from_beam_and_lumen() {
    assert_eq!(GlowStyle::parse("comet"), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse(" Comet "), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse("beam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse("lumen"), GlowStyle::Lumen);
    assert!(
        GlowStyle::Comet.has_particles(),
        "comet sheds debris glitter"
    );
    assert!(style_has_beam("comet"), "the comet keeps its beam");
}

/// A real-window geometry: pad + tab strip/titlebar chrome put the grid
/// interior at origin (56, 56). The identity-law geoms all use origin 0
/// (where grid-relative and window-absolute coincide), so only a chrome
/// origin can expose a producer mixing the two coordinate spaces.
fn chrome_geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 56,
        origin_y: 56,
        win_w: 384, // 56 + 40·8 + 8
        win_h: 160, // 56 + 6·16 + 8
        head: 0,
    }
}

/// The live-cursor bridge vertex is window-absolute like every other comet
/// sample: with real chrome it attaches at `origin_x + (col + head_dx)·cw`. A
/// grid-relative x fails the ±1.5-cell adjacency gate and detaches the beam.
#[test]
fn comet_bridge_attaches_window_absolute() {
    let g = chrome_geom();
    let mut c = cfg(GlowStyle::Comet, true);
    c.head_dx = 0.9; // an attach point clearly right of the cell centre
    let mut glow = CursorGlow::default();
    let mut t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 10)), t, &c, g, &mut out);
    for col in 11..=14u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    // Re-emit the beam stream alone (no crown/ring/particle quads).
    let mut beam = Vec::new();
    let mut halos = Vec::new();
    let mut under = Vec::new();
    glow.emit_comet(t, &c, g, Some((2, 14)), &mut beam, &mut halos, &mut under);
    assert!(!beam.is_empty(), "a typing run draws a beam");
    let reach = beam
        .iter()
        .map(|q| i32::from(q.x) + i32::from(q.w))
        .max()
        .unwrap();
    // Window-absolute attach: 56 + (14 + 0.9)·8 = 175.2 px. The head sample
    // (cell 13's centre) reaches only 56 + 13.5·8 = 164, so a reach ≥ 172
    // proves the bridge segment exists and lands at head_dx.
    assert!(
        reach >= 172,
        "bridge reaches the window-absolute head_dx attach point: {reach}"
    );
    assert!(
        beam.iter().all(|q| i32::from(q.x) >= i32::from(g.origin_x)),
        "every beam quad stays inside the window-absolute effects box"
    );
}

/// Comet debris is born window-absolute: jump grains sit on the true leap
/// vector, and typing debris drifts behind the one-cell motion with
/// jitter-only vy (not toward the window's top-left).
#[test]
fn comet_debris_born_window_absolute_with_chrome_origin() {
    let g = chrome_geom();
    let c = cfg(GlowStyle::Comet, true);
    // A leap: grains strewn along the window-absolute vector.
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((0, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t + Duration::from_millis(16));
    glow.tick(
        Some((5, 20)),
        t + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert!(!glow.particles.is_empty(), "a leap sheds a meteor train");
    let (fx, fy) = (56.0 + 2.5 * 8.0, 56.0 + 0.5 * 16.0);
    let (ox, oy) = (56.0 + 20.5 * 8.0, 56.0 + 5.5 * 16.0);
    for p in &glow.particles {
        let u = (p.x0 - fx) / (ox - fx);
        assert!(
            (-1e-3..=1.0 + 1e-3).contains(&u) && (p.y0 - (fy + (oy - fy) * u)).abs() < 1e-2,
            "grain ON the true leap vector, got ({}, {})",
            p.x0,
            p.y0
        );
    }
    // Typing: the drift vector is the true one-cell motion, leftward along
    // the row, vy pure dispersion jitter (±0.175·cell).
    let mut glow = CursorGlow::default();
    let mut t = Instant::now();
    glow.tick(Some((2, 10)), t, &c, g, &mut out);
    for col in 11..=16u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let cell = g.ch as f32;
    assert!(!glow.particles.is_empty(), "typing outgasses grains");
    for p in &glow.particles {
        assert!(p.vx < 0.0, "dust falls back along the tail: vx {}", p.vx);
        assert!(
            p.vy.abs() <= 0.175 * cell + 1e-3,
            "vy is dispersion jitter only, no origin skew: {}",
            p.vy
        );
    }
}

/// Beam stardust jump births are window-absolute: every mote lies inside the
/// ±0.4-cell sleeve of the true leap vector.
#[test]
fn beam_stardust_strewn_along_window_absolute_leap() {
    let g = chrome_geom();
    let c = cfg(GlowStyle::Beam, true);
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((0, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t + Duration::from_millis(16));
    glow.tick(
        Some((5, 20)),
        t + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert!(!glow.particles.is_empty(), "a warp leap stirs stardust");
    let (fx, fy) = (56.0 + 2.5 * 8.0, 56.0 + 0.5 * 16.0);
    let (ox, oy) = (56.0 + 20.5 * 8.0, 56.0 + 5.5 * 16.0);
    let sleeve = 0.4 * g.ch as f32 + 1e-3;
    for p in &glow.particles {
        let u = (p.x0 - fx) / (ox - fx);
        assert!(
            (-1e-3..=1.0 + 1e-3).contains(&u),
            "mote within the leap span: {}",
            p.x0
        );
        assert!(
            (p.y0 - (fy + (oy - fy) * u)).abs() <= sleeve,
            "mote inside the leap sleeve, got ({}, {})",
            p.x0,
            p.y0
        );
        assert!(
            p.x0 >= 56.0 && p.y0 >= 56.0,
            "no mote above/left of the grid interior: ({}, {})",
            p.x0,
            p.y0
        );
    }
}

/// The comet's emitted light (beam + coma crown + twinkling glitter) honours
/// the shared quad invariants and decays to exactly empty.
#[test]
fn comet_light_respects_invariants_and_decays() {
    let g = geom();
    let c = cfg(GlowStyle::Comet, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((2, 30)), t0, &c, g, &mut out);
    out.clear();
    let fp = glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(60),
        &c,
        g,
        &mut out,
    );
    assert_ne!(fp, 0);
    assert!(!out.is_empty(), "beam + glitter render while alive");
    assert_invariants(&out, g);
    let fp = glow.tick(Some((2, 30)), t0 + Duration::from_secs(4), &c, g, &mut out);
    assert!(out.is_empty(), "comet light fully decayed");
    assert_eq!(fp, 0);
    assert!(!glow.is_active());
}
