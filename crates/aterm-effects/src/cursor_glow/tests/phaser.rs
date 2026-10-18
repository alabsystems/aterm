// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The phaser band and coalesced echo sweeps.

use super::*;

/// Phaser: the band spans the last three letters as one continuous bar at a
/// typing rhythm and never parks: a key typed after a thinking pause is a new
/// burst with a short base life.
#[test]
fn phaser_band_spans_three_letters_and_never_parks() {
    let g = geom();
    let c = cfg(GlowStyle::Phaser, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut t = t0;
    // A steady 50 ms rhythm across row 2.
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=12u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    // The three letters behind the head are all still alive (continuous bar).
    for col in 9..=11u16 {
        assert!(
            glow.sparks.iter().any(|s| {
                s.row == 2
                    && s.col == col
                    && t.saturating_duration_since(s.born).as_secs_f32() < s.life
            }),
            "cell {col} inside the three-letter band is lit"
        );
    }
    // A 2 s thinking pause, then one key: it must take the short base life
    // (well under a second), never a pause-scaled chained one.
    t += Duration::from_secs(2);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 13)), t, &c, g, &mut out);
    // The typing spark lays at the just-typed origin cell, col 12.
    let head = glow
        .sparks
        .iter()
        .rev()
        .find(|s| s.row == 2 && s.col == 12)
        .expect("the lone key laid its spark");
    assert!(
        head.life < 1.0,
        "a post-pause key fades crisply, got {}s",
        head.life
    );
}

/// Echo-run reach: the coalesced-echo law extends to the shared typed-bridge
/// reach (8 cells, `HIDE_BRIDGE_TYPED_MAX_DIST`). A 4-8 cell typed advance in
/// one frame (key-repeat across a frame slip, SSH echo batching) must not fall
/// to `re_anchor`, which lays only the landing cell.
#[test]
fn wide_coalesced_echo_still_sweeps_every_cell() {
    let g = geom();
    let c = cfg(GlowStyle::Phaser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(t0 + Duration::from_millis(6), 1);
    glow.tick(Some((2, 5)), t0 + Duration::from_millis(8), &c, g, &mut out);
    // Six keys echo in one observed move (a frame slip under load).
    glow.note_synthetic_typed(t0 + Duration::from_millis(14), 6);
    glow.tick(
        Some((2, 11)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    for col in 5..=10u16 {
        let s = glow
            .sparks
            .iter()
            .find(|s| s.row == 2 && s.col == col)
            .unwrap_or_else(|| panic!("swept cell {col} got its spark (no notch)"));
        assert!(s.typing, "cell {col} is a TYPING spark, not a jump");
    }
    assert!(glow.flare < 0.05, "no flare slam on a wide coalesced echo");
    assert!(
        glow.ring.is_none(),
        "no landing ring on a wide coalesced echo"
    );
}

/// Wrap-run law: a coalesced wrap echo mid-burst (last columns → the next
/// row's first columns) lays typing sparks on the pre-wrap tail and the landed
/// post-wrap cells, and fires no landing ring.
#[test]
fn wrap_echo_continues_the_band_across_the_fold() {
    let g = geom(); // cols: 40
    let c = cfg(GlowStyle::Phaser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 36)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(t0 + Duration::from_millis(6), 1);
    glow.tick(
        Some((2, 37)),
        t0 + Duration::from_millis(8),
        &c,
        g,
        &mut out,
    );
    // The coalesced fold echo: (2,37) → (3,2) in one observed move.
    glow.note_synthetic_typed(t0 + Duration::from_millis(14), 5);
    glow.tick(
        Some((3, 2)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    for (row, col) in [(2u16, 37u16), (2, 38), (2, 39), (3, 0), (3, 1)] {
        assert!(
            glow.sparks
                .iter()
                .any(|s| s.row == row && s.col == col && s.typing),
            "the fold's typed cell ({row},{col}) is lit"
        );
    }
    assert!(glow.ring.is_none(), "a typing wrap fires no landing ring");
    assert!(
        glow.fire_meteors.is_empty(),
        "a typing wrap flies no meteor for any style"
    );
    // The pre-wrap tail is not snuffed: its sparks keep their chained
    // lives (the phaser wrap-follow exemption).
    let tail = glow
        .sparks
        .iter()
        .find(|s| s.row == 2 && s.col == 36)
        .expect("the pre-fold spark is still resident");
    let age = (t0 + Duration::from_millis(16))
        .saturating_duration_since(tail.born)
        .as_secs_f32();
    assert!(
        tail.life - age > 0.05,
        "the old row fades on its own life, not a snuff clamp"
    );
}

/// Vertical-nav law: Up/Down timestamps are classifier signals, not causal
/// evidence, so repeated one-row child relocations stay dark in every
/// direction and build no heat or wake.
#[test]
fn vertical_navigation_timestamps_stay_dark() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 10)), t0, &c, g, &mut out);
    let mut t = t0;
    // A held Down arrow at key-repeat cadence, each press hinted.
    for row in 3..=5u16 {
        t += Duration::from_millis(35);
        glow.note_navigation(t);
        glow.tick(
            Some((row, 10)),
            t + Duration::from_millis(2),
            &c,
            g,
            &mut out,
        );
    }
    assert_eq!(glow.heat, 0.0, "held vertical arrows earn no heat");
    assert!(glow.sparks.is_empty(), "nav scrubbing lays no wake");
    assert!(glow.flare < 0.05, "nav scrubbing never slams the flare");
}

/// Fade as one: the first key of a burst takes the long lone-key base life,
/// but once the burst follows, the origin cell is retro-clamped to the chained
/// cadence and cannot outlive the band.
#[test]
fn phaser_burst_origin_cannot_outlive_the_band() {
    let g = geom();
    let c = cfg(GlowStyle::Phaser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    // First key after an idle window: the lone-key base life (~0.4-0.75s).
    let mut t = t0 + Duration::from_millis(5);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 1)), t, &c, g, &mut out);
    let first_life = glow.sparks.iter().find(|s| s.col == 0).unwrap().life;
    assert!(first_life >= 0.40, "a lone first key keeps the base life");
    // A fast burst follows: 8 ms cadence.
    for col in 2..=8u16 {
        t += Duration::from_millis(8);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let origin = glow
        .sparks
        .iter()
        .find(|s| s.col == 0)
        .expect("origin spark still resident");
    let age = t.saturating_duration_since(origin.born).as_secs_f32();
    assert!(
        origin.life - age
            <= CursorGlow::PHASER_CHAIN_KEYS * 0.008 + CursorGlow::CHAIN_MARGIN + 1e-3,
        "the origin's lone-key bet is voided by the burst (remaining {})",
        origin.life - age
    );
}

/// Light-theme law (beam family): additive light cannot brighten a white
/// ground, so on `dark_theme: false` the band renders as source-over ink
/// under the letters with no additive quads or halos; dark themes are
/// untouched.
#[test]
fn light_theme_band_is_ink_not_additive() {
    let g = geom();
    let mut c = cfg(GlowStyle::Phaser, true);
    c.dark_theme = false;
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    let mut t = t0;
    for col in 5..=8u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(
        out.is_empty(),
        "no additive quads on a light theme (band, crown, ring all ink/skip)"
    );
    assert!(
        !glow.under_quads().is_empty() && glow.under_quads().iter().all(|q| q.alpha > 0),
        "the light-theme band renders as source-over ink under the letters"
    );
    assert!(
        glow.halos().iter().all(|h| h.mode == HaloMode::Over),
        "no additive halos sneak in on light"
    );
}

#[test]
fn disabled_or_zero_intensity_emits_nothing() {
    let g = geom();
    let mut glow = CursorGlow::default();
    let now = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), now, &cfg(GlowStyle::Fire, false), g, &mut out);
    let fp = glow.tick(
        Some((2, 30)),
        now,
        &cfg(GlowStyle::Fire, false),
        g,
        &mut out,
    );
    assert_eq!(fp, 0);
    assert!(out.is_empty());
    let mut c = cfg(GlowStyle::Lumen, true);
    c.intensity = 0.0;
    glow.tick(Some((2, 0)), now, &c, g, &mut out);
    let fp = glow.tick(Some((2, 30)), now, &c, g, &mut out);
    assert_eq!(fp, 0);
}
