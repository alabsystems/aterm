// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing heat and the laser, sparkle and laser bursts.

use super::*;

/// Sustained fast typing leaves a brighter, longer wake than relaxed typing at
/// the same config (the heat model).
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

/// A laser jump strikes lightning: a jagged main channel plus branch forks
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

/// A raw Ctrl-A/Home timestamp forges no laser strike because it is not
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
