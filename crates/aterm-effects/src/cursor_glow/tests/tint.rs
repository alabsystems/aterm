// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Glyph tint and light-theme ink.

use super::*;

const TINT_THEMES: [(&str, u32, u32); 5] = [
    ("GitHub Light", 0x001F_2328, 0x00FF_FFFF),
    ("GitHub canvas", 0x001F_2328, 0x00F6_F8FA),
    ("Solarized Light", 0x0065_7B83, 0x00FD_F6E3), // 4.13:1 — sub-AA
    ("Grey on grey", 0x0064_6464, 0x00E0_E0E0),    // 3.86:1 — sub-AA
    ("Paper", 0x0033_3333, 0x00FA_F8F5),
];

#[test]
fn one_rainbow_sweep_is_continuous_and_shared() {
    let step = RAINBOW_LIGHT_RAIL_SPREAD;
    for phase_i in 0..8 {
        let phase = phase_i as f32 * 0.37;
        let mut prev = rainbow_sweep_at(0, phase);
        for col in 1..400u16 {
            let now = rainbow_sweep_at(col, phase);
            assert!(
                (now - prev).abs() <= step * 1.5 + 1e-5,
                "seam at col {col}, phase {phase}: {prev} -> {now}"
            );
            assert!((0.0..=1.0).contains(&now), "sweep stays in range: {now}");
            prev = now;
        }
    }
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for col in 0..400u16 {
        let v = rainbow_sweep_at(col, 0.0);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    assert!(
        lo < 0.05 && hi > 0.95,
        "the sweep spans the spectrum: {lo}..{hi}"
    );
    for col in 0..64u16 {
        for phase_i in 0..8 {
            let band = rainbow_band_at(col, phase_i as f32 * 0.37);
            assert!(
                RAINBOW_BANDS.contains(&band),
                "col {col}: {band:06X} is one of the seven anchors"
            );
        }
    }
}

#[test]
fn glyph_tint_bisection_predicate_is_monotone_and_its_answer_holds() {
    for (name, fg, bg) in TINT_THEMES {
        let (side, cap) = glyph_ink_bound(fg, bg).expect("a usable theme");
        for &band in &RAINBOW_BANDS {
            let (r, g, b) = (
                ((band >> 16) & 0xff) as f32,
                ((band >> 8) & 0xff) as f32,
                (band & 0xff) as f32,
            );
            let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
            let span = (hi - lo).max(1e-3);
            let sat = |c: f32| ((c - lo) / span * hi).clamp(0.0, 255.0);
            let s3 = [sat(r), sat(g), sat(b)];
            let cand = |k: f32| -> u32 {
                let ch = |i: usize| -> u32 {
                    let v = match side {
                        GlyphInkSide::Dark => s3[i] * k,
                        GlyphInkSide::Light => s3[i] + (255.0 - s3[i]) * (1.0 - k),
                    };
                    ((v + 0.5) as u32).min(255)
                };
                (ch(0) << 16) | (ch(1) << 8) | ch(2)
            };
            let ok = |ink: u32| -> bool {
                let corner = match side {
                    GlyphInkSide::Dark => chan_max(ink, fg),
                    GlyphInkSide::Light => chan_min(ink, fg),
                };
                let l = rel_luminance(corner);
                match side {
                    GlyphInkSide::Dark => l <= cap,
                    GlyphInkSide::Light => l >= cap,
                }
            };
            let mut seen_false = false;
            for step in 0..=256 {
                let k = step as f32 / 256.0;
                let v = ok(cand(k));
                if !v {
                    seen_false = true;
                } else {
                    assert!(
                        !seen_false,
                        "{name} band {band:06X}: the predicate went false -> true at k={k:.4}, \
                         so bisection is not valid on this family"
                    );
                }
            }
            let ink = fresh_ink_glyph_ink(band, fg, side, cap).expect("a usable band");
            assert!(
                ok(ink),
                "{name} band {band:06X}: the RETURNED ink {ink:06X} fails its own predicate"
            );
        }
    }
}

#[test]
fn glyph_tint_palette_is_memoized_on_the_theme_key() {
    let g = geom();
    let mut c = rainbow_pop_light_cfg();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    let first = glow
        .glyph_palette
        .expect("a key is memoized on the first tick");
    assert_eq!(first.0, (c.theme_fg, c.theme_bg));
    assert!(first.1.is_some(), "the fixture theme is usable");
    glow.tick(
        Some((3, 3)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert_eq!(glow.glyph_palette, Some(first));
    c.theme_fg = 0x0065_7B83;
    c.theme_bg = 0x00FD_F6E3;
    glow.tick(
        Some((3, 4)),
        t0 + Duration::from_millis(32),
        &c,
        g,
        &mut out,
    );
    let second = glow.glyph_palette.expect("re-derived");
    assert_eq!(second.0, (0x0065_7B83, 0x00FD_F6E3));
    assert_ne!(
        second.1, first.1,
        "a different theme yields a different palette"
    );
    c.theme_fg = 0x0012_3456;
    c.theme_bg = 0x0012_3456;
    glow.tick(
        Some((3, 5)),
        t0 + Duration::from_millis(48),
        &c,
        g,
        &mut out,
    );
    assert_eq!(glow.glyph_palette, Some(((0x0012_3456, 0x0012_3456), None)));
    assert!(
        glow.charred().is_empty(),
        "a conceal-shaped theme substitutes no foreground"
    );
}

#[test]
fn glyph_tint_suppresses_itself_on_a_theme_with_no_usable_side() {
    assert!(fresh_ink_glyph_palette(aterm_core::render::COLOR_UNSET, 0x00FF_FFFF).is_none());
    assert!(fresh_ink_glyph_palette(0x001F_2328, aterm_core::render::COLOR_UNSET).is_none());
    assert!(fresh_ink_glyph_palette(0x0012_3456, 0x0012_3456).is_none());
    assert!(fresh_ink_glyph_palette(0x00FF_FFFF, 0x00CC_CCCC).is_none());
    let a = 0x00D3_0000;
    let b = 0x0000_7A00;
    assert!(
        (rel_luminance(a) - rel_luminance(b)).abs() < 0.02,
        "fixture is equiluminant ({:.3} vs {:.3})",
        rel_luminance(a),
        rel_luminance(b)
    );
    assert!(fresh_ink_glyph_palette(a, b).is_none());
}

#[test]
fn glyph_tint_invariant_holds_across_every_grey_theme() {
    let mut usable = 0usize;
    for f in 0..=255u32 {
        for b in 0..=255u32 {
            let fg = (f << 16) | (f << 8) | f;
            let bg = (b << 16) | (b << 8) | b;
            let Some(palette) = fresh_ink_glyph_palette(fg, bg) else {
                continue;
            };
            usable += 1;
            let (side, cap) = glyph_ink_bound(fg, bg).expect("a palette implies a bound");
            let bar = wcag_ratio(fg, bg).min(FRESH_INK_GLYPH_TARGET_RATIO);
            for &ink in &palette {
                let corner = match side {
                    GlyphInkSide::Dark => chan_max(ink, fg),
                    GlyphInkSide::Light => chan_min(ink, fg),
                };
                match side {
                    GlyphInkSide::Dark => assert!(rel_luminance(corner) <= cap + 1e-6),
                    GlyphInkSide::Light => assert!(rel_luminance(corner) >= cap - 1e-6),
                }
                for step in 0..=8 {
                    let w = step as f32 / 8.0;
                    let px = lerp_rgb(fg, ink, w);
                    assert!(
                        wcag_ratio(px, bg) + 0.02 >= bar,
                        "fg {f:02X} bg {b:02X} ink {ink:06X} w {w:.2}: \
                         {:.2}:1 under the {bar:.2}:1 bar",
                        wcag_ratio(px, bg)
                    );
                }
            }
        }
    }
    assert!(
        usable > 4000,
        "the sweep actually exercised the tint on {usable} grey themes"
    );
}

#[test]
fn light_ink_darkens_every_band_to_one_bar() {
    for &band in &RAINBOW_BANDS {
        let ink = light_ink(band);
        assert!(
            luma709(ink) <= LIGHT_INK_MAX_LUMA + 1.0,
            "band {band:06X} -> {ink:06X} luma {} exceeds the bar",
            luma709(ink)
        );
        let hi = ((ink >> 16) & 0xff).max((ink >> 8) & 0xff).max(ink & 0xff) as f32;
        assert!(
            hi <= LIGHT_INK_MAX_CHANNEL + 1.0,
            "band {band:06X} -> {ink:06X} peaks at {hi}, over the channel bar"
        );
        for (hi, lo) in [(16u32, 8u32), (8, 0), (16, 0)] {
            let chan = |c: u32, sh: u32| ((c >> sh) & 0xff) as i32;
            let (bh, bl) = (chan(band, hi), chan(band, lo));
            let (ih, il) = (chan(ink, hi), chan(ink, lo));
            assert!(
                (bh <= bl || ih >= il) && (bh >= bl || ih <= il),
                "band {band:06X} inverts channels {hi}/{lo} at ink weight ({ink:06X})"
            );
        }
    }
    assert_eq!(light_ink(0x0010_2030), 0x0010_2030);
}

#[test]
fn light_ink_bold_holds_one_ink_weight_across_the_spectrum() {
    let order = |c: u32| {
        let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        (r >= g, g >= b, r >= b)
    };
    for &band in &RAINBOW_BANDS {
        let ink = light_ink_bold(band);
        let y = luma709(ink);
        assert!(
            (y - LIGHT_INK_BOLD_MAX_LUMA).abs() <= 1.5,
            "band {band:06X} -> {ink:06X} lands at luma {y}, not ON the bar"
        );
        let hi = ((ink >> 16) & 0xff).max((ink >> 8) & 0xff).max(ink & 0xff) as f32;
        assert!(
            hi <= LIGHT_INK_BOLD_MAX_CHANNEL + 1.0,
            "band {band:06X} -> {ink:06X} peaks at {hi}, over the channel bar"
        );
        assert_eq!(
            order(band),
            order(ink),
            "band {band:06X} keeps its hue at ink weight ({ink:06X})"
        );
    }
    for grey in [0x00FF_FFFF, 0x0080_8080] {
        let ink = light_ink_bold(grey);
        let (r, g, b) = ((ink >> 16) & 0xff, (ink >> 8) & 0xff, ink & 0xff);
        assert!(r == g && g == b && r > 0, "{grey:06X} -> {ink:06X} is grey");
    }
    assert_eq!(light_ink_bold(0), 0);
}

#[test]
fn light_ink_cap_keeps_glyphs_legible() {
    let cap = LIGHT_INK_ALPHA_CAP as u8;
    let max_c = |p: u32| ((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff);
    let min_c = |p: u32| ((p >> 16) & 0xff).min((p >> 8) & 0xff).min(p & 0xff);
    for &band in &RAINBOW_BANDS {
        let ink = light_ink(band) | (u32::from(cap) << 24);
        let over_ink = aterm_render::over_rgb(0x000A_0A0A, ink, cap);
        let over_white = aterm_render::over_rgb(0x00FF_FFFF, ink, cap);
        assert!(
            max_c(over_ink) <= 0x80,
            "band {band:06X} over near-black ink stays dark ({over_ink:06X})"
        );
        assert!(
            min_c(over_white) < 0xF0,
            "band {band:06X} actually darkens a white ground ({over_white:06X})"
        );
    }
}

fn rainbow_pop_cfg() -> GlowConfig {
    let mut c = cfg(GlowStyle::RainbowKitty, true);
    c.radius = 0.0;
    c.ring = false;
    c
}

fn rainbow_pop_light_cfg() -> GlowConfig {
    let mut c = rainbow_pop_cfg();
    c.dark_theme = false;
    c
}
