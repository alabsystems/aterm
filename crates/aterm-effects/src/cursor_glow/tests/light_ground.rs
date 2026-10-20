// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every style on a light ground. Additive light cannot show on white, so
//! each style draws in ink there, its body under the letters and inside the
//! row it swept, in the shape its name promises.

use super::*;

/// A light config for `style`.
fn light(style: GlowStyle) -> GlowConfig {
    let mut c = cfg(style, true);
    c.dark_theme = false;
    c.theme_fg = 0x0024_292F;
    c.theme_bg = 0x00FF_FFFF;
    c.color = 0x0009_69DA;
    c.accent = 0x0040_90F0;
    c.intensity = 1.0;
    c
}

/// Type `keys` cells along row 2 from column 4 at a steady typing cadence,
/// and return the glow and the last frame's additive stream.
fn typed(c: &GlowConfig, keys: u16) -> (CursorGlow, Vec<GlowQuad>) {
    let g = geom();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((2, 4)), t, c, g, &mut out);
    for col in 5..5 + keys {
        t += Duration::from_millis(60);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, c, g, &mut out);
    }
    (glow, out)
}

/// [`typed`], then the same instant again with the grains cleared: the body
/// alone, for the tests that read its shape.
fn typed_body(c: &GlowConfig, keys: u16) -> CursorGlow {
    let g = geom();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((2, 4)), t, c, g, &mut out);
    for col in 5..5 + keys {
        t += Duration::from_millis(60);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, c, g, &mut out);
    }
    glow.particles.clear();
    glow.tick(Some((2, 4 + keys)), t, c, g, &mut out);
    glow
}

/// Nonzero one-pixel cells are a valid effects canvas too: a light beam may
/// round away, but must not panic or grow outside its row to meet a half-pixel
/// minimum. Ordinary light cells still draw, and a dark ground stays additive.
#[test]
fn light_beams_fit_one_pixel_high_cells_without_panicking() {
    for (height, dark) in [(20, true), (20, false), (2, false), (1, false)] {
        for style in [
            GlowStyle::Comet,
            GlowStyle::Beam,
            GlowStyle::Laser,
            GlowStyle::Phaser,
        ] {
            let mut g = geom();
            g.ch = height;
            g.win_h = (g.rows * g.ch) as u16;
            let c = if dark { cfg(style, true) } else { light(style) };
            let mut glow = CursorGlow::default();
            let mut out = Vec::new();
            let t0 = Instant::now();
            glow.tick(Some((2, 4)), t0, &c, g, &mut out);
            for step in 1..=6_u16 {
                let now = t0 + Duration::from_millis(60 * u64::from(step));
                glow.note_synthetic_typed(now, 1);
                glow.tick(Some((2, 4 + step)), now, &c, g, &mut out);
            }
            let under = glow.under_quads();
            if dark {
                assert!(under.is_empty(), "dark {style:?} keeps its additive body");
            } else {
                let top = 2 * height as u16;
                let bottom = top + height as u16;
                assert!(
                    under.iter().all(|q| q.y >= top && q.y + q.h <= bottom),
                    "{style:?} fits its {height}-pixel row"
                );
                if height >= 2 {
                    assert!(
                        !under.is_empty(),
                        "{style:?} still draws at height {height}"
                    );
                }
            }
        }
    }
}

/// The rows of ink each pixel column of the under-ink stream holds.
fn ink_heights(under: &[GlowQuad]) -> std::collections::BTreeMap<u16, u16> {
    let mut heights = std::collections::BTreeMap::new();
    for q in under {
        for x in q.x..q.x + q.w {
            *heights.entry(x).or_insert(0) += q.h;
        }
    }
    heights
}

/// The beam family's body on a light ground is source-over ink in the
/// under-ink stream (letters draw over it at full contrast) and never leaves
/// the row it swept, so the lines above and below stay clean; the comet and
/// the beam ink nothing over the letters. A dark ground keeps the additive
/// beam and draws nothing under the letters.
#[test]
fn light_beam_bodies_are_ink_under_the_letters_inside_their_row() {
    let g = geom();
    let row = 0..g.ch as u16;
    let row = row.start + 2 * g.ch as u16..row.end + 2 * g.ch as u16;
    for style in [
        GlowStyle::Comet,
        GlowStyle::Beam,
        GlowStyle::Laser,
        GlowStyle::Phaser,
    ] {
        let (glow, out) = typed(&light(style), 6);
        let under = glow.under_quads();
        assert!(
            !under.is_empty(),
            "{style:?} draws a body on a light ground"
        );
        assert!(
            under.iter().all(|q| q.alpha > 0 && q.alpha2 > 0),
            "{style:?}: the body is source-over ink"
        );
        assert!(
            under
                .iter()
                .all(|q| row.contains(&q.y) && q.y + q.h <= row.end),
            "{style:?}: the body stays inside its row"
        );
        assert!(
            out.iter().all(|q| q.alpha > 0),
            "{style:?}: no additive light on a light ground"
        );
        if matches!(style, GlowStyle::Comet | GlowStyle::Beam) {
            // Their body is their look: no dust is inked over the letters.
            assert!(
                glow.halos().is_empty(),
                "{style:?}: nothing over the letters"
            );
        }
        // Negative control: the same typing on a dark ground lays nothing
        // under the letters for the beam family.
        let (dark, _) = typed(&cfg(style, true), 6);
        assert!(
            dark.under_quads().is_empty(),
            "{style:?}: a dark ground keeps its additive beam"
        );
    }
}

/// The comet and the beam no longer draw one shape in two colours on a light
/// ground: the comet's ink swells toward the caret, the beam's tube is even.
#[test]
fn light_comet_tapers_and_light_beam_is_even() {
    let g = geom();
    let spread = |style| {
        let glow = typed_body(&light(style), 6);
        let heights = ink_heights(glow.under_quads());
        // Two cells behind the caret (col 10) and one cell behind it.
        let at = |col: u16| {
            heights
                .get(&(col * g.cw as u16 + g.cw as u16 / 2))
                .copied()
                .unwrap_or(0)
        };
        (at(8), at(9))
    };
    let (comet_far, comet_near) = spread(GlowStyle::Comet);
    assert!(
        comet_near > comet_far,
        "the comet swells toward its head: {comet_far} then {comet_near}"
    );
    let (beam_far, beam_near) = spread(GlowStyle::Beam);
    assert_eq!(beam_far, beam_near, "the beam's tube is even");
}

/// How dark each pixel row of one column is once the under-ink stream is
/// composited over the white ground in emission order: `255 - luma` per y.
fn column_darkness(under: &[GlowQuad], x: u16) -> std::collections::BTreeMap<u16, f32> {
    let mut rgb = std::collections::BTreeMap::<u16, [f32; 3]>::new();
    for q in under.iter().filter(|q| (q.x..q.x + q.w).contains(&x)) {
        let keep = 1.0 - f32::from(q.alpha) / 255.0;
        let src = [
            ((q.color >> 16) & 0xFF) as f32,
            ((q.color >> 8) & 0xFF) as f32,
            (q.color & 0xFF) as f32,
        ];
        for y in q.y..q.y + q.h {
            let dst = rgb.entry(y).or_insert([255.0; 3]);
            for (d, s) in dst.iter_mut().zip(src) {
                *d = s + *d * keep;
            }
        }
    }
    rgb.into_iter()
        .map(|(y, [r, g, b])| (y, 255.0 - (0.2126 * r + 0.7152 * g + 0.0722 * b)))
        .collect()
}

/// THE LIGHT BEAM NEVER STRIKES THROUGH THE LETTERS: behind the caret its
/// darkest ink (within a quarter of its peak, above the dither) is a band at
/// least a third of the row tall (a highlighter around the text), not a thin
/// line along the row's middle, both while it is fresh and as it powers
/// down. NEGATIVE CONTROL: Laser, whose hard thin
/// line is its look, peaks in a band under a third of the row.
#[test]
fn light_beam_is_a_band_around_the_letters_never_a_strike() {
    let g = geom();
    let x = 8 * g.cw as u16 + g.cw as u16 / 2;
    let peak_rows = |darkness: &std::collections::BTreeMap<u16, f32>| {
        let peak = darkness.values().copied().fold(0.0_f32, f32::max);
        (
            peak,
            darkness.values().filter(|d| **d >= peak * 0.75).count() as u16,
        )
    };
    let third = g.ch as u16 / 3;
    let c = light(GlowStyle::Beam);
    let mut glow = typed_body(&c, 6);
    let mut out = Vec::new();
    let mut t = Instant::now() + Duration::from_millis(60 * 7);
    for frame in 0..4 {
        let (peak, rows) = peak_rows(&column_darkness(glow.under_quads(), x));
        if peak < 4.0 {
            break;
        }
        assert!(
            rows >= third,
            "frame {frame}: the beam's darkest ink is {rows} px tall, a line through the letters"
        );
        t += Duration::from_millis(90);
        glow.particles.clear();
        glow.tick(Some((2, 10)), t, &c, g, &mut out);
    }
    let (laser, _) = typed(&light(GlowStyle::Laser), 6);
    let (peak, rows) = peak_rows(&column_darkness(laser.under_quads(), x));
    assert!(peak > 4.0 && rows < third, "laser: {rows} px at its peak");
}

/// Sparkle's stars and the fire's embers scatter on a light ground too, as
/// ink marks, not additive light that white would swallow.
#[test]
fn light_sparkles_and_embers_are_ink() {
    for style in [GlowStyle::Sparkle, GlowStyle::Fire] {
        let (glow, _) = typed(&light(style), 10);
        let halos = glow.halos();
        assert!(
            halos.iter().any(|h| h.mode == HaloMode::Over),
            "{style:?} scatters ink marks on a light ground"
        );
        assert!(
            halos.iter().all(|h| h.mode == HaloMode::Over),
            "{style:?}: no additive halos on a light ground"
        );
    }
}

/// Lumen's glow, its whole look, shows on a light ground: soft ink under the
/// letters on both sides of the caret.
#[test]
fn light_lumen_glows_around_the_caret() {
    let g = geom();
    let (glow, _) = typed(&light(GlowStyle::Lumen), 4);
    let heights = ink_heights(glow.under_quads());
    let caret_x = 8 * g.cw as u16 + g.cw as u16 / 2;
    let left = heights.range(..caret_x - g.cw as u16).count();
    let right = heights.range(caret_x + g.cw as u16..).count();
    assert!(
        left > 0 && right > 0,
        "a glow on both sides: {left} / {right}"
    );
    assert!(glow.under_quads().iter().all(|q| q.alpha > 0));
}

/// EVERY CROWN IS INK ON PAPER: typed on a light ground, each style with a
/// crown wears it as source-over ink under the letters around the caret
/// (inside its row), never as additive light. Negative control: on a dark
/// ground the same typing lays the additive crown halo.
#[test]
fn every_crown_is_ink_on_a_light_ground() {
    let g = geom();
    let caret_x = 10 * g.cw as u16 + g.cw as u16 / 2;
    let row = 2 * g.ch as u16..3 * g.ch as u16;
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Comet,
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Laser,
        GlowStyle::Water,
        GlowStyle::Fire,
    ] {
        let glow = typed_body(&light(style), 6);
        let near: Vec<&GlowQuad> = glow
            .under_quads()
            .iter()
            .filter(|q| {
                let mid = q.x + q.w / 2;
                mid.abs_diff(caret_x) <= g.cw as u16 / 2
            })
            .collect();
        assert!(!near.is_empty(), "{style:?}: a crown of ink at the caret");
        assert!(
            near.iter()
                .all(|q| q.alpha > 0 && row.contains(&q.y) && q.y + q.h <= row.end),
            "{style:?}: source-over, inside the caret's row"
        );
        assert!(
            glow.halos().iter().all(|h| h.mode == HaloMode::Over),
            "{style:?}: no additive halo on paper"
        );
        let dark = typed_body(&cfg(style, true), 6);
        assert!(
            dark.halos().iter().any(|h| h.mode == HaloMode::Add),
            "{style:?}: a dark ground lays the additive crown"
        );
    }
}

/// THE LIGHT FIRE BURNS ON THE PAPER: the ink-fire's coverage ceiling is set
/// by the theme's text over the flame's darkest ink, not the dark ground's
/// additive cap. On GitHub Light the darkest flame reads clearly on the
/// paper (at least 2:1) while the text keeps the bar over it; a theme with
/// paler text gets a lower ceiling that still keeps it. Negative control:
/// the old shared cap (34) left the darkest flame at under 1.3:1.
#[test]
fn light_fire_flames_ink_the_paper_and_keep_text_legible() {
    let contrast = |a: u32, b: u32| {
        let luma = crate::color_math::relative_luminance;
        let (x, y) = (luma(a), luma(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    };
    let rim = aterm_render::fire_field::FIRE_OVER_RIM_INK;
    let over = |bg: u32, a: u8| aterm_render::over_premul(bg, premul_rgb(rim, a), a) & 0x00FF_FFFF;
    let (fg, bg) = (0x001F_2328, 0x00FF_FFFF);
    let ceiling = light_fire_cov_ceiling(fg, bg);
    let flame = over(bg, ceiling);
    assert!(
        contrast(flame, bg) >= 2.0,
        "the flame reads: {:.2}:1",
        contrast(flame, bg)
    );
    assert!(
        contrast(fg, flame) >= BODY_CONTRAST_BAR,
        "the text keeps the bar"
    );
    let paler = light_fire_cov_ceiling(0x0040_4040, bg);
    assert!(
        paler < ceiling,
        "paler text, a lower ceiling: {paler} vs {ceiling}"
    );
    assert!(contrast(0x0040_4040, over(bg, paler)) >= BODY_CONTRAST_BAR);
    assert!(contrast(over(bg, 34), bg) < 1.3, "the old cap's flame");
    // The emitted field carries the ceiling: a hot run's patches reach it.
    let mut c = light(GlowStyle::Fire);
    c.theme_fg = fg;
    c.theme_bg = bg;
    let (glow, _) = typed(&c, 30);
    let top = glow.patches().iter().map(|p| p.cov_cap).max().unwrap_or(0);
    assert!(
        f32::from(top) >= f32::from(ceiling) * 0.45,
        "the light field's cap {top} rides the ceiling {ceiling}"
    );
}
