// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The phaser band and coalesced echo sweeps.

use super::*;

/// PHASER: the band spans the last THREE letters as one continuous bar at
/// a typing rhythm, and NEVER parks — a key typed after a thinking pause
/// is a new burst with a short base life, not a multi-second chained one
/// (live review: "it stays back at the last time I typed").
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
    // A 2 s thinking pause, then ONE key: it must take the short base life
    // (well under a second), never a pause-scaled chained one.
    t += Duration::from_secs(2);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 13)), t, &c, g, &mut out);
    // (The typing spark lays at the just-typed ORIGIN cell — col 12.)
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

/// ECHO-RUN LAW: a coalesced typing echo (2-3 cells forward on one row,
/// backed by an admitted typed candidate mid-rhythm) is CONTINUED TYPING — every
/// swept cell gets its own typing spark in its own successive laid hue, no
/// flare slams, no landing ring. (Classified as a jump, it laid long-lived
/// jump sparks between short typing sparks — the audited picket-fence burst.)
#[test]
fn coalesced_echo_advance_is_typing_and_lays_every_cell() {
    let g = geom();
    let c = cfg(GlowStyle::Phaser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    // Two sequential echoes establish the rhythm…
    glow.note_synthetic_typed(t0 + Duration::from_millis(6), 1);
    glow.tick(Some((2, 5)), t0 + Duration::from_millis(8), &c, g, &mut out);
    glow.note_synthetic_typed(t0 + Duration::from_millis(14), 1);
    glow.tick(
        Some((2, 6)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    // …then two keys echo in ONE observed move (frame coalescing).
    glow.note_synthetic_typed(t0 + Duration::from_millis(22), 2);
    glow.tick(
        Some((2, 8)),
        t0 + Duration::from_millis(24),
        &c,
        g,
        &mut out,
    );
    for col in 6..=7u16 {
        let s = glow
            .sparks
            .iter()
            .find(|s| s.row == 2 && s.col == col)
            .unwrap_or_else(|| panic!("swept cell {col} got its spark"));
        assert!(s.typing, "cell {col} is a TYPING spark, not a jump");
    }
    let (h6, h7) = (
        glow.sparks.iter().find(|s| s.col == 6).unwrap().hue,
        glow.sparks.iter().find(|s| s.col == 7).unwrap().hue,
    );
    assert!(
        (h7 - h6).rem_euclid(1.0) > 1e-3,
        "each swept cell lays its own successive hue ({h6} vs {h7})"
    );
    assert!(glow.flare < 0.05, "no flare slam on a coalesced echo");
    assert!(glow.ring.is_none(), "no landing ring on a coalesced echo");
}

/// ECHO-RUN REACH: the coalesced-echo law extends to the SHARED typed-bridge
/// reach (8 cells, `HIDE_BRIDGE_TYPED_MAX_DIST`), not a private cap of 3. A
/// 4-8 cell typed advance in one observed frame (key-repeat across a frame
/// slip, SSH echo batching) must not fall through to `re_anchor`, which lays
/// ONLY the landing cell — a multi-cell dark notch splitting the band.
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
    // Six keys echo in ONE observed move (a frame slip under load).
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

/// WRAP-RUN LAW: a coalesced wrap echo mid-burst (last columns → the next
/// row's first columns) lays typing sparks on the pre-wrap tail AND the
/// landed post-wrap glyph cells — the band follows the typing through the
/// fold instead of stranding the old row lit and the fresh glyphs bare —
/// and fires no landing ring (the audited hollow-rectangle glitch at the
/// break).
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
    // The pre-wrap tail was NOT snuffed: its sparks keep their chained
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

/// VERTICAL-NAV LAW: Up/Down timestamps are classifier signals, not causal
/// evidence. Repeated one-row child relocations therefore stay dark in every
/// direction and cannot build heat or a wake.
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

/// FADE-AS-ONE, phaser edition: the first key of a burst takes the long
/// lone-key base life, but once the burst follows, the origin cell is
/// retro-clamped to the chained cadence — it can no longer outlive the
/// whole band as a parked opaque box (the audited stuck origin quad).
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

/// LIGHT-THEME LAW (beam family): additive light cannot brighten a white
/// ground and over fresh glyphs it lifts their ink toward the background —
/// so on `dark_theme: false` the comet/band renders as source-over VEIL
/// halos and emits NO additive beam quads; dark themes are untouched.
#[test]
fn light_theme_band_is_veils_not_additive() {
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
        "no additive quads on a light theme (band, crown, ring all veil/skip)"
    );
    assert!(
        glow.halos().iter().any(|h| h.mode == HaloMode::Over),
        "the light-theme band renders as source-over veils"
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

#[test]
fn hidden_cursor_does_not_spawn_on_reappear() {
    let mut glow = CursorGlow::default();
    let now = Instant::now();
    let g = geom();
    let mut out = Vec::new();
    let c = cfg(GlowStyle::Lumen, true);
    glow.tick(Some((2, 0)), now, &c, g, &mut out);
    glow.tick(None, now, &c, g, &mut out);
    let fp = glow.tick(Some((2, 30)), now, &c, g, &mut out);
    assert_eq!(fp, 0, "no comet across a hide/show gap");
}
