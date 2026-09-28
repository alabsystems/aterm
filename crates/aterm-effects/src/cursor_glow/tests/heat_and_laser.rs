// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing heat and the laser, sparkle and laser bursts.

use super::*;

/// Sustained fast typing must leave a brighter, longer wake than relaxed
/// typing at the SAME config — the heat ("acceleration") model.
#[test]
fn sustained_typing_outshines_relaxed_typing() {
    let g = geom();
    let c = cfg(GlowStyle::Laser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();

    // Relaxed: one single-cell advance every 500 ms.
    let mut slow = CursorGlow::default();
    let mut t = t0;
    slow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=10u16 {
        t += Duration::from_millis(500);
        slow.note_synthetic_typed(t, 1);
        slow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let slow_lum = lum(&out);
    assert!(slow_lum > 0, "relaxed typing still glows (faintly)");

    // Sustained fast: one advance every 50 ms.
    let mut fast = CursorGlow::default();
    let mut t = t0;
    fast.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=10u16 {
        t += Duration::from_millis(50);
        fast.note_synthetic_typed(t, 1);
        fast.tick(Some((2, i)), t, &c, g, &mut out);
    }
    let fast_lum = lum(&out);
    assert!(
        fast_lum > slow_lum * 2,
        "sustained typing must overdrive the wake: fast {fast_lum} vs slow {slow_lum}"
    );
}

/// Heat builds under fast typing and cools across a pause (lazy decay).
#[test]
fn heat_cools_after_a_pause() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=10u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    assert!(
        glow.heat > 0.8,
        "sustained fast typing reaches high heat: {}",
        glow.heat
    );
    glow.tick(Some((2, 10)), t + Duration::from_secs(3), &c, g, &mut out);
    assert!(glow.heat < 0.1, "a pause cools the wake: {}", glow.heat);
}

/// A laser JUMP strikes LIGHTNING: a jagged main channel plus branch forks
/// spawn with the move (geometry frozen at spawn), keep emitting strobing
/// light while alive, and decay to exactly nothing.
#[test]
fn laser_jump_strikes_lightning() {
    let g = geom();
    let c = cfg(GlowStyle::Laser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((1, 38)), t0, &c, g, &mut out); // a real jump = a strike
    assert!(
        glow.bolts.len() >= 3,
        "a jump forges a main channel + branch forks, got {}",
        glow.bolts.len()
    );
    assert!(
        glow.bolts.iter().any(|b| !b.branch),
        "the strike has a main channel"
    );
    assert!(
        glow.bolts.iter().any(|b| b.branch),
        "the strike throws branch forks"
    );
    let main = glow
        .bolts
        .iter()
        .find(|b| !b.branch)
        .expect("main channel exists");
    assert!(
        main.pts.len() > 8,
        "midpoint displacement kinks the channel into a real polyline: {} pts",
        main.pts.len()
    );
    glow.tick(
        Some((1, 38)),
        t0 + Duration::from_millis(100),
        &c,
        g,
        &mut out,
    );
    assert!(!out.is_empty(), "the strike is still lit at +100ms");
    assert!(!glow.bolts.is_empty(), "bolts outlive the first frames");
    glow.tick(
        Some((1, 38)),
        t0 + Duration::from_millis(2000),
        &c,
        g,
        &mut out,
    );
    assert!(glow.bolts.is_empty(), "every bolt decays to nothing");
}

/// LIGHTNING TRAIL: with a long configured fade, a hot typing run leaves a
/// lingering CHARGED trail — still lit well past where the old generic
/// wake (~0.25s ceiling) died — whose aged cells bleed down to a dim
/// residual instead of holding full beam power over the text, and whose
/// crackle arcs root along the trail, not only at the write head. The
/// charge still drains to exactly empty.
#[test]
fn laser_typing_leaves_a_charged_crackling_trail() {
    let g = geom();
    let mut c = cfg(GlowStyle::Laser, true);
    c.duration = Duration::from_millis(1200); // a long cursor_trail_ms is honoured
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    // A hot run: 14 keys at 50 ms cadence along one row. Watch each tick
    // for a crackle arc FORGED THIS KEY (born == t) rooted ≥1.5 cells
    // behind the current head — the trail-rooting tell (the old code
    // rooted every arc exactly at the head cell).
    let mut trail_rooted = false;
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=14u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
        let head_x = (i as f32 + 0.5) * g.cw as f32;
        if glow
            .bolts
            .iter()
            .any(|b| b.branch && b.born == t && b.pts[0].0 <= head_x - 1.5 * g.cw as f32)
        {
            trail_rooted = true;
        }
    }
    let fresh_lum = lum(&out);
    assert!(fresh_lum > 0, "the hot run is lit");
    assert_invariants(&out, g);
    assert!(
        trail_rooted,
        "crackle arcs root along the charged trail, not only at the head"
    );
    // 0.45s after the last key the charged trail is STILL lit…
    glow.tick(
        Some((2, 14)),
        t + Duration::from_millis(450),
        &c,
        g,
        &mut out,
    );
    assert!(
        glow.sparks.iter().any(|s| s.typing),
        "the charged trail lingers past the old wake ceiling"
    );
    let residual_lum = lum(&out);
    assert!(residual_lum > 0, "the residual charge still glows");
    // …but only as a dim residual, well below the freshly typed run.
    assert!(
        residual_lum * 2 < fresh_lum,
        "aged trail bleeds to a dim residual (residual {residual_lum} vs fresh {fresh_lum})"
    );
    // And the charge drains to exactly empty.
    let fp = glow.tick(
        Some((2, 14)),
        t + Duration::from_millis(3000),
        &c,
        g,
        &mut out,
    );
    assert!(glow.sparks.is_empty(), "every charged cell drains");
    assert_eq!(fp, 0, "the trail decays to exactly empty");
}

/// Laser must stay in its own hue — with a pure-green config no emitted quad
/// may approach white (the old look flashed to R=G=B at the head, crown and
/// ring). The shared hot-core layer's subtle highlight (≤70% white on the
/// thin core only) is the permitted ceiling.
#[test]
fn laser_is_monochrome() {
    let g = geom();
    let mut c = cfg(GlowStyle::Laser, true);
    c.color = 0x0000_FF00;
    c.accent = 0x0000_FF00;
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((3, 5)), t0, &c, g, &mut out);
    // A big jump exercises comet + crown + ring; then typing advances.
    glow.note_synthetic_move(t0);
    glow.tick(Some((1, 38)), t0, &c, g, &mut out);
    let mut all: Vec<GlowQuad> = out.clone();
    glow.note_synthetic_typed(t0 + Duration::from_millis(60), 1);
    glow.tick(
        Some((1, 39)),
        t0 + Duration::from_millis(60),
        &c,
        g,
        &mut out,
    );
    all.extend(out.iter().copied());
    assert!(!all.is_empty());
    for q in &all {
        let (r, gr, b) = (
            (q.color >> 16) & 0xff,
            (q.color >> 8) & 0xff,
            q.color & 0xff,
        );
        let ceil = (gr as f32 * 0.75 + 2.0) as u32;
        assert!(
            r <= ceil && b <= ceil,
            "laser quad left its hue: {:06x}",
            q.color
        );
    }
}

/// A raw Ctrl-A/Home timestamp forges no LASER strike because it is not
/// movement provenance. The same geometry under an explicit synthetic
/// candidate still exercises the strike morphology.
#[test]
fn laser_navigation_forges_no_strike() {
    let g = geom();
    let c = cfg(GlowStyle::Laser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    // A timestamp-only relocation: no strike, no flare slam.
    let mut nav = CursorGlow::default();
    nav.tick(Some((2, 35)), t0, &c, g, &mut out);
    nav.note_navigation(t0 + Duration::from_millis(5));
    nav.tick(
        Some((2, 0)),
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(
        nav.bolts.is_empty(),
        "Ctrl-A forges no lightning strike, got {} bolts",
        nav.bolts.len()
    );
    assert!(
        nav.flare < 0.05,
        "and never slams the flare ({})",
        nav.flare
    );
    // The same explicitly scripted jump still strikes.
    let mut jump = CursorGlow::default();
    jump.tick(Some((2, 35)), t0, &c, g, &mut out);
    jump.note_synthetic_move(t0 + Duration::from_millis(5));
    jump.tick(
        Some((2, 0)),
        t0 + Duration::from_millis(10),
        &c,
        g,
        &mut out,
    );
    assert!(
        !jump.bolts.is_empty(),
        "a synthetic preview jump still strikes lightning"
    );
}

/// Sparkle bursts SPREAD; Laser divides its punch. Sparkle births jitter
/// across the cell via the already-drawn (previously unused) r2/r4
/// randoms — no RNG stream shifts for any style — and a jump strews the
/// burst along the leap vector like Fire/Comet/Beam. Laser keeps its
/// exact-point cutting-torch births but scales each spark's birth
/// coverage by 1/√burst, so a 30-spark landing stacks to the beam hue
/// instead of clamping the impact point to a flat white blob.
#[test]
fn sparkle_and_laser_bursts_never_stack_one_point() {
    let g = geom();
    let c = cfg(GlowStyle::Sparkle, true);
    // Sparkle typing: births stay in the cell's jitter window but leave
    // the exact centre point.
    let mut glow = CursorGlow::default();
    let mut t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 10)), t, &c, g, &mut out);
    for col in 11..=16u16 {
        t += Duration::from_millis(30);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let oy = 2.5 * g.ch as f32;
    assert!(glow.particles.len() >= 4, "a warm run bursts sparkles");
    for p in &glow.particles {
        assert!(
            (p.y0 - oy).abs() <= 0.25 * g.ch as f32 + 1e-3,
            "birth stays within the cell jitter window: y0 {}",
            p.y0
        );
        assert_eq!(
            p.cov_scale, 1.0,
            "sparkle keeps full punch — SPREAD is its stacking remedy"
        );
    }
    assert!(
        glow.particles
            .iter()
            .any(|p| ((p.x0 / g.cw as f32).fract() - 0.5).abs() > 1e-3),
        "births jitter across the cell, never pinned to the centre"
    );
    assert!(
        glow.particles.iter().any(|p| (p.y0 - oy).abs() > 0.01),
        "births jitter off the centre line, never a point-stack"
    );
    // Sparkle jump: the burst walks the leap vector.
    let mut glow = CursorGlow::default();
    let t2 = Instant::now();
    glow.tick(Some((0, 2)), t2, &c, g, &mut out);
    glow.note_synthetic_move(t2 + Duration::from_millis(16));
    glow.tick(
        Some((5, 20)),
        t2 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    let (fx, fy) = (2.5 * 8.0, 0.5 * 16.0);
    let (jx, jy) = (20.5 * 8.0, 5.5 * 16.0);
    let (mut lo_u, mut hi_u) = (f32::MAX, f32::MIN);
    assert!(!glow.particles.is_empty(), "a jump bursts sparkles");
    for p in &glow.particles {
        let u = (p.x0 - fx) / (jx - fx);
        assert!(
            (-1e-3..=1.0 + 1e-3).contains(&u),
            "sparkle on the leap span: {}",
            p.x0
        );
        assert!(
            (p.y0 - (fy + (jy - fy) * u)).abs() <= 0.3 * 16.0 + 1e-3,
            "sparkle within the leap sleeve: ({}, {})",
            p.x0,
            p.y0
        );
        lo_u = lo_u.min(u);
        hi_u = hi_u.max(u);
    }
    assert!(
        hi_u - lo_u > 0.25,
        "the burst STREWS along the path ({lo_u}..{hi_u})"
    );
    // Laser: exact-point births, √burst-scaled punch.
    let cl = cfg(GlowStyle::Laser, true);
    let mut glow = CursorGlow::default();
    let t3 = Instant::now();
    glow.tick(Some((0, 2)), t3, &cl, g, &mut out);
    glow.note_synthetic_move(t3 + Duration::from_millis(16));
    glow.tick(
        Some((5, 32)),
        t3 + Duration::from_millis(16),
        &cl,
        g,
        &mut out,
    );
    let n = glow.particles.len();
    assert_eq!(n, 30, "a monster jump lands the full ablation shower");
    let expect = (n as f32).sqrt().recip();
    for p in &glow.particles {
        assert_eq!(
            (p.x0, p.y0),
            (32.5 * 8.0, 5.5 * 16.0),
            "the cutting-torch impact point stays EXACT"
        );
        assert!(
            (p.cov_scale - expect).abs() < 1e-6,
            "the shower divides its punch √burst-fold: {} vs {expect}",
            p.cov_scale
        );
    }
}
