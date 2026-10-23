// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor glow light-ground rendering tests.

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
/// and return the glow, the last frame's additive stream and that frame's
/// instant.
fn typed(c: &GlowConfig, keys: u16) -> (CursorGlow, Vec<GlowQuad>, Instant) {
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
    (glow, out, t)
}

/// The streak's body alone at `at` with the caret at `cur`, into fresh
/// streams: its under-ink stream.
fn body(
    glow: &mut CursorGlow,
    c: &GlowConfig,
    g: Geom,
    at: Instant,
    cur: (u16, u16),
) -> Vec<GlowQuad> {
    let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
    glow.emit_comet(at, c, g, Some(cur), &mut out, &mut halos, &mut under);
    under
}

/// Nonzero one-pixel cells are a valid effects canvas too: a beam's
/// under-ink body (the light ground's ink, the dark ground's bed) may round
/// away, but must not panic or grow outside its row to meet a half-pixel
/// minimum.
#[test]
fn light_beams_fit_one_pixel_high_cells_without_panicking() {
    for (height, dark) in [
        (20, true),
        (2, true),
        (1, true),
        (20, false),
        (2, false),
        (1, false),
    ] {
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
            let mut now = t0;
            for step in 1..=6_u16 {
                now = t0 + Duration::from_millis(60 * u64::from(step));
                glow.note_synthetic_typed(now, 1);
                glow.tick(Some((2, 4 + step)), now, &c, g, &mut out);
            }
            let under = body(&mut glow, &c, g, now, (2, 10));
            let top = 2 * height as u16;
            let bottom = top + height as u16;
            let rail = if dark {
                (height as f32 * 0.2).ceil() as u16
            } else {
                0
            };
            assert!(
                under
                    .iter()
                    .all(|q| q.y >= top && q.y + q.h <= bottom + rail),
                "{style:?} (dark {dark}) fits its {height}-pixel row"
            );
            if height >= 2 {
                assert!(
                    !under.is_empty(),
                    "{style:?} (dark {dark}) still draws at height {height}"
                );
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
/// the beam ink nothing over the letters.
#[test]
fn light_beam_bodies_are_ink_under_the_letters_inside_their_row() {
    let g = geom();
    let row = 2 * g.ch as u16..3 * g.ch as u16;
    for style in [
        GlowStyle::Comet,
        GlowStyle::Beam,
        GlowStyle::Laser,
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Lumen,
    ] {
        let c = light(style);
        let (mut glow, out, t) = typed(&c, 6);
        let under = glow.under_quads().to_vec();
        assert!(
            !under.is_empty(),
            "{style:?} draws a body on a light ground"
        );
        assert!(
            under.iter().all(|q| q.alpha > 0 && q.alpha2 > 0),
            "{style:?}: the body is source-over ink"
        );
        let body = body(&mut glow, &c, g, t, (2, 10));
        assert!(!body.is_empty(), "{style:?}: the streak's body is drawn");
        assert!(
            body.iter()
                .all(|q| row.contains(&q.y) && q.y + q.h <= row.end),
            "{style:?}: the body stays inside its row"
        );
        assert!(
            out.iter().all(|q| q.alpha > 0),
            "{style:?}: no additive light on a light ground"
        );
        if matches!(style, GlowStyle::Comet | GlowStyle::Beam) {
            assert!(
                glow.halos().is_empty(),
                "{style:?}: nothing over the letters"
            );
        }
        let (_, dark, _) = typed(&cfg(style, true), 6);
        assert!(
            dark.iter().any(|q| q.alpha == 0),
            "{style:?}: a dark ground keeps its additive beam"
        );
    }
}

/// Sparkle's stars and the fire's embers scatter on a light ground too, as
/// ink marks, not additive light that white would swallow.
#[test]
fn light_sparkles_and_embers_are_ink() {
    for style in [GlowStyle::Sparkle, GlowStyle::Fire] {
        let (glow, out, _) = typed(&light(style), 10);
        let halos = glow.halos();
        assert!(
            halos.iter().any(|h| h.mode == HaloMode::Over)
                || glow.under_quads().iter().any(|q| q.alpha > 0),
            "{style:?} scatters ink marks on a light ground"
        );
        assert!(
            halos.iter().all(|h| h.mode == HaloMode::Over) && out.iter().all(|q| q.alpha > 0),
            "{style:?}: no additive light on a light ground"
        );
    }
}

/// The comet and the beam no longer draw one shape in two colours on a light
/// ground: the comet's ink swells toward the caret, the beam's tube is even.
#[test]
fn light_comet_tapers_and_light_beam_is_even() {
    let g = geom();
    let spine = 2.0 * g.ch as f32 + g.ch as f32 * 0.54;
    let reach = g.ch as f32 * 0.2;
    let spread = |style| {
        let c = light(style);
        let (mut glow, _, t) = typed(&c, 6);
        let under = body(&mut glow, &c, g, t, (2, 10));
        let heights = ink_heights(&under);
        let x = |col: u16| col * g.cw as u16 + g.cw as u16 / 2;
        let at = |col: u16| heights.get(&x(col)).copied().unwrap_or(0);
        let ink = |col: u16| {
            column_darkness(&under, x(col))
                .iter()
                .filter(|(y, _)| (f32::from(**y) + 0.5 - spine).abs() <= reach)
                .map(|(_, d)| *d)
                .sum::<f32>()
        };
        ((at(8), at(9)), (ink(8), ink(9)))
    };
    let (_, (comet_far, comet_near)) = spread(GlowStyle::Comet);
    assert!(
        comet_near > comet_far,
        "the comet swells toward its head: {comet_far:.0} then {comet_near:.0}"
    );
    let ((beam_far, beam_near), _) = spread(GlowStyle::Beam);
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
/// down.
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
    let (mut glow, mut out, mut t) = typed(&c, 6);
    for frame in 0..4 {
        let (peak, rows) = peak_rows(&column_darkness(&body(&mut glow, &c, g, t, (2, 10)), x));
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
    let laser_cfg = light(GlowStyle::Laser);
    let (mut laser, _, t) = typed(&laser_cfg, 6);
    let laser_body = body(&mut laser, &laser_cfg, g, t, (2, 10));
    let (peak, rows) = peak_rows(&column_darkness(&laser_body, x));
    assert!(peak > 4.0 && rows < third, "laser: {rows} px at its peak");
}

/// Lumen's look shows on a light ground as a tracer, not a lamp: soft ink
/// under the letters behind the caret (the glow over the letters just typed
/// and the tracer under the run), and nothing ahead of the caret's own cell.
#[test]
fn light_lumen_traces_behind_the_caret_and_not_ahead() {
    let g = geom();
    let c = light(GlowStyle::Lumen);
    let (glow, _, _) = typed(&c, 4);
    let heights = ink_heights(glow.under_quads());
    let cw = g.cw as u16;
    let caret_x = 8 * cw + cw / 2;
    let behind = heights.range(..caret_x - cw).count();
    let ahead = heights.range(caret_x + cw..).count();
    assert!(behind > 0, "no ink behind the caret");
    assert_eq!(ahead, 0, "ink ahead of the caret's cell: {ahead} columns");
    assert!(glow.under_quads().iter().all(|q| q.alpha > 0));
    let mut lamp = Vec::new();
    push_ink_ellipse(
        &mut lamp,
        g,
        g.cell_center(2, 8),
        (g.cw as f32 * 1.6, g.cw as f32 * 1.6),
        g.ch as f32 * 0.46,
        0x0000_5599,
        120.0,
        LIGHT_MARK_QUADS,
    );
    let lamp_ahead = ink_heights(&lamp).range(caret_x + cw..).count();
    assert!(
        lamp_ahead > 0,
        "control: a lamp lays nothing ahead of the caret"
    );
}

/// The light comet's head stays in the caret's cell and wears a nucleus:
/// after a typed burst on GitHub Light, no comet ink lies in the typed row
/// past the caret cell's leading edge but its round coma (where a cap
/// stood a few pixels past the block caret as a nub): nothing reaches past
/// the coma's radius from the caret's centre, and the head behind the caret is
/// a dense round nucleus: the comet's ink over the newest letter's cell is
/// markedly deeper than over the tail two cells back, while the newest
/// letter keeps the light body's bar over it.
#[test]
fn the_light_comet_head_stays_in_the_caret_cell_and_wears_a_nucleus() {
    let g = tall_geom();
    let mut c = light(GlowStyle::Comet);
    c.theme_fg = 0x001F_2328;
    let (row, start) = (3u16, 4u16);
    let keys = 10u16;
    let caret = start + keys;
    let burst = |c: &GlowConfig| {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        glow.tick(Some((row, start)), t, c, g, &mut out);
        for k in 1..=keys {
            t += Duration::from_millis(60);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((row, start + k)), t, c, g, &mut out);
        }
        let ink = body(&mut glow, c, g, t, (row, caret));
        (glow, ink)
    };
    let row_top = i32::from(row) * g.ch as i32;
    let lead = i32::from(caret + 1) * g.cw as i32;
    let (glow, ink) = burst(&c);
    assert!(!ink.is_empty(), "the comet draws");
    let coma =
        (g.cell_center(row, caret).0 + g.ch as f32 * CursorGlow::COMET_LIGHT_COMA_R).ceil() as i32;
    for q in ink
        .iter()
        .filter(|q| i32::from(q.y) < row_top + g.ch as i32 && i32::from(q.y + q.h) > row_top)
    {
        assert!(
            i32::from(q.x + q.w) <= lead.max(coma),
            "comet ink past the caret's leading edge ({lead}) and its coma ({coma}): {q:?}"
        );
    }
    let depth = |ink: &[GlowQuad], col: u16| -> f32 {
        let mut glass = Glass::new(g, 0x00FF_FFFF);
        glass.add_quads(ink);
        let (x0, y0, x1, _) = glass.cell_rect(row, col);
        let y = y0 + g.ch as i32 / 2;
        (x0..x1)
            .map(|x| 1.0 - crate::color_math::relative_luminance(glass.at(y, x) & 0x00FF_FFFF))
            .fold(0.0f32, f32::max)
    };
    let (head, tail) = (depth(&ink, caret - 1), depth(&ink, caret - 3));
    assert!(
        head >= 1.3 * tail && head > 0.2,
        "a nucleus: the head's ink {head:.3} against the tail's {tail:.3}"
    );
    let mut lumen = light(GlowStyle::Lumen);
    lumen.theme_fg = c.theme_fg;
    let (_, even) = burst(&lumen);
    let (lh, lt) = (depth(&even, caret - 1), depth(&even, caret - 3));
    assert!(
        lh < 1.3 * lt,
        "control: lumen's even tracer reads as a nucleus ({lh:.3} against {lt:.3})"
    );
    let mut lit = Glass::new(g, 0x00FF_FFFF);
    lit.add_quads(glow.under_quads());
    let (strokes, counters) = glyph(g, row, caret - 1);
    for &(y, x) in &strokes {
        lit.px[y as usize * lit.w + x as usize] = c.theme_fg;
    }
    let r = ratio(&lit, &strokes, &counters);
    assert!(
        r >= 5.25,
        "the newest letter over the nucleus reads at {r:.2}:1"
    );
}

/// On a light ground no grain stands half under the block caret: a grain
/// whose mark would reach into the caret's cell is not drawn (a half-disc
/// standing off the caret's edge read as a nub stuck to it), while the same
/// grain two cells out is.
#[test]
fn light_grains_keep_off_the_block_caret() {
    let g = tall_geom();
    for style in [GlowStyle::Comet, GlowStyle::Sparkle, GlowStyle::Water] {
        let c = light(style);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t = Instant::now();
        let (row, col) = (3u16, 10u16);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
        let (x0, y0, x1, _) = Glass::new(g, c.theme_bg).cell_rect(row, col);
        let drawn = |glow: &mut CursorGlow, x: f32| {
            glow.particles.clear();
            glow.particles.push(Particle {
                x0: x,
                y0: (y0 + g.ch as i32 / 2) as f32,
                vx: 0.0,
                vy: 0.0,
                gy: 0.0,
                life: 1.0,
                hue: 0.5,
                cov_scale: 1.0,
                born: t,
            });
            let (mut q, mut h, mut u) = (Vec::new(), Vec::new(), Vec::new());
            glow.emit_particles(t, &c, g, &mut q, &mut h, &mut u);
            !q.is_empty() || !h.is_empty() || !u.is_empty()
        };
        assert!(
            !drawn(&mut glow, (x1 + 2) as f32),
            "{style:?}: a grain at the caret's edge is drawn"
        );
        assert!(
            drawn(&mut glow, (x1 + 2 * g.cw as i32) as f32),
            "control: {style:?}'s grain two cells out is not drawn"
        );
        let _ = x0;
    }
}

/// A light band is ink under the letters of the row it lies in, so a run
/// that leaps rows (an Enter onto the next prompt, a jump) draws only its
/// stretch on the landing row: for the beam, sparkle and comet, a
/// jump from the end of row 2 down to the start of row 5 leaves every under
/// quad of the body inside row 5's band, and the rows it crossed (the output
/// an Enter just printed) take no ink.
#[test]
fn a_light_band_that_leaps_rows_stays_on_the_landing_row() {
    let g = geom();
    let landing = 5u16;
    let band = |row: u16| {
        let y0 = u32::from(row) * g.ch as u32;
        y0..y0 + g.ch as u32
    };
    let leap = |style: GlowStyle| {
        let c = light(style);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        glow.tick(Some((2, 30)), t0, &c, g, &mut out);
        glow.note_synthetic_move(t0);
        glow.tick(Some((landing, 2)), t0, &c, g, &mut out);
        body(&mut glow, &c, g, t0, (landing, 2))
    };
    for style in [GlowStyle::Beam, GlowStyle::Sparkle, GlowStyle::Comet] {
        let under = leap(style);
        assert!(!under.is_empty(), "{style:?}: the landing still draws");
        for q in &under {
            let (y0, y1) = (u32::from(q.y), u32::from(q.y) + u32::from(q.h));
            assert!(
                y0 >= band(landing).start && y1 <= band(landing).end,
                "{style:?}: a quad at rows {y0}..{y1} left the landing row"
            );
        }
    }
    let strike = {
        let c = light(GlowStyle::Laser);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        glow.tick(Some((2, 30)), t0, &c, g, &mut out);
        glow.note_synthetic_move(t0);
        glow.tick(Some((landing, 2)), t0, &c, g, &mut out);
        let mut bolts = Vec::new();
        glow.emit_bolts(t0, &c, g, &mut bolts, &mut Vec::new(), &mut Vec::new());
        bolts
    };
    assert!(
        strike
            .iter()
            .any(|q| u32::from(q.y) + u32::from(q.h) <= band(landing).start),
        "control: the laser's strike crosses the rows the jump spans"
    );
}

/// OVER WIDE GLYPHS A LIGHT BAND KEEPS ITS SHAPE AND HUE, LIGHTER. A band
/// fills a wide glyph's whole box (CJK, emoji), the gaps between its strokes
/// included, where a Latin letter keeps paper above and below its x-height.
#[test]
fn light_bands_over_wide_glyphs_keep_their_shape_and_hue_lighter() {
    let g = geom();
    let row = 2u16;
    let start = 4u16;
    let keys = 4u16;
    let run = |style: GlowStyle, wide: bool| {
        let mut c = light(style);
        if style == GlowStyle::Sparkle {
            c.color = crate::cursor_glow::SPARKLE_DEFAULT_COLOR;
        }
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        let mut chars = vec![' '; g.cols];
        glow.observe_row(row, start, &chars, t);
        glow.tick(Some((row, start)), t, &c, g, &mut out);
        for k in 0..keys {
            t += Duration::from_millis(60);
            let col = usize::from(start + 2 * k);
            if wide {
                chars[col] = '你';
                chars[col + 1] = '\0';
            } else {
                chars[col] = 'a';
                chars[col + 1] = 'b';
            }
            let caret = start + 2 * (k + 1);
            glow.note_synthetic_typed(t, 2);
            glow.observe_row(row, caret, &chars, t);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
        }
        (c, glow.under_quads().to_vec(), glow.halos().to_vec())
    };
    let x0 = i32::from(start) * g.cw as i32;
    let x1 = i32::from(start + 2 * keys) * g.cw as i32;
    let (y0, y1) = (
        i32::from(row) * g.ch as i32,
        i32::from(row + 1) * g.ch as i32,
    );
    let inside = |q: &GlowQuad| {
        let (qx0, qx1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        qx0 < x1 && qx1 > x0 && q.alpha.max(q.alpha2) > 0
    };
    let reading = |c: &GlowConfig, under: &[GlowQuad]| {
        let mut glass = Glass::new(g, c.theme_bg);
        glass.add_quads(under);
        let read = |y: i32| {
            (x0..x1)
                .map(|x| crate::color_math::contrast_ratio(c.theme_fg, glass.at(y, x)))
                .fold(f32::INFINITY, f32::min)
        };
        let body = (y0..y1).map(read).fold(f32::INFINITY, f32::min);
        (body, read(y0).min(read(y1 - 1)))
    };
    let mut held = false;
    for style in [GlowStyle::Beam, GlowStyle::Phaser, GlowStyle::Sparkle] {
        let (c, under, halos) = run(style, true);
        let clean = crate::color_math::contrast_ratio(c.theme_fg, c.theme_bg);
        let keep = clean * LIGHT_WIDE_BED_KEEP;
        assert!(
            under.iter().any(inside),
            "{style:?}: no band lies under the wide glyphs"
        );
        let (body, edges) = reading(&c, &under);
        assert!(
            body >= keep * 0.98,
            "{style:?}: the band over the wide glyphs leaves the text {body:.2}:1 (keep {keep:.2}:1)"
        );
        assert!(
            body < clean * 0.9,
            "{style:?}: the band over the wide glyphs does not show ({body:.2}:1 of {clean:.2}:1)"
        );
        assert!(
            edges >= clean * 0.95,
            "{style:?}: the band stands hard at the row's edge ({edges:.2}:1 of {clean:.2}:1)"
        );
        if style == GlowStyle::Sparkle {
            for q in under.iter().filter(|q| inside(q) && q.alpha >= 24) {
                let (h, s, _) = crate::color_math::rgb2hsv(q.color & 0x00FF_FFFF);
                assert!(
                    (22.0..=58.0).contains(&h) && s >= 0.35,
                    "the sparkle band over a wide glyph left gold: {:06x}",
                    q.color & 0x00FF_FFFF
                );
            }
        }
        let (c, narrow, narrow_halos) = run(style, false);
        assert!(
            halos.len() <= narrow_halos.len(),
            "{style:?}: veils stand in for the band over the wide glyphs ({} against {})",
            halos.len(),
            narrow_halos.len()
        );
        let (narrow_body, _) = reading(&c, &narrow);
        held |= narrow_body < keep * 0.98;
    }
    assert!(
        held,
        "control: no style's band over narrow letters darkens them past the keep"
    );
}

/// ON PAPER SPARKLE'S BAND STAYS GOLD AT THE HEAD: over a 24-key run on
/// GitHub Light, every frame's band under the two newest letters is ink of
/// the gold family (whatever hue its wheel has rolled to), so the bed behind
/// the letter just typed never turns leaf-green or salmon.
#[test]
fn the_light_sparkle_band_stays_gold_at_the_head() {
    let g = geom();
    let strays = |style: GlowStyle| {
        let mut c = light(style);
        c.color = crate::cursor_glow::SPARKLE_DEFAULT_COLOR;
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        glow.tick(Some((2, 4)), t, &c, g, &mut out);
        let (mut seen, mut off) = (0usize, Vec::new());
        for col in 5..29u16 {
            t += Duration::from_millis(60);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, col)), t, &c, g, &mut out);
            let x0 = (col - 2) * g.cw as u16;
            let x1 = col * g.cw as u16;
            for q in glow.under_quads() {
                if q.x + q.w <= x0 || q.x >= x1 || q.alpha < 24 {
                    continue;
                }
                seen += 1;
                let (h, s, _) = crate::color_math::rgb2hsv(q.color & 0x00FF_FFFF);
                if !((22.0..=58.0).contains(&h) && s >= 0.35) {
                    off.push((col, q.color & 0x00FF_FFFF));
                }
            }
        }
        (seen, off)
    };
    let (seen, off) = strays(GlowStyle::Sparkle);
    assert!(
        seen > 20,
        "fixture: the band lies under the newest letters ({seen})"
    );
    assert!(off.is_empty(), "the band left gold at the head: {off:06x?}");
    let (_, phaser) = strays(GlowStyle::Phaser);
    assert!(
        !phaser.is_empty(),
        "control: the phaser's spectrum band never left the gold family"
    );
}

/// A LIGHT BAND'S TAIL FADES OUT: after a leap back along its row on paper
/// (the Settings preview's demo does this), the beam's and sparkle's band
/// rises from the paper over its oldest cell and a half, so
/// along its spine no pixel column steps by more than a fifth of the band's
/// depth (a cut at the tail's end stepped from paper to the whole band in
/// one column).
#[test]
fn a_light_bands_tail_fades_out() {
    let g = geom();
    for style in [GlowStyle::Beam, GlowStyle::Sparkle] {
        let c = light(style);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        glow.tick(Some((2, 30)), t0, &c, g, &mut out);
        let t = t0 + Duration::from_millis(16);
        glow.note_synthetic_move(t);
        glow.tick(Some((2, 4)), t, &c, g, &mut out);
        let mut glass = Glass::new(g, c.theme_bg);
        glass.add_quads(glow.under_quads());
        let paper = crate::color_math::relative_luminance(c.theme_bg);
        let y = 2 * g.ch as i32 + g.ch as i32 / 2;
        let depth: Vec<f32> = (6 * g.cw as i32..g.win_w as i32)
            .map(|x| 1.0 - crate::color_math::relative_luminance(glass.at(y, x)) / paper)
            .collect();
        let deep = depth.iter().copied().fold(0.0f32, f32::max);
        assert!(
            deep > 0.08,
            "{style:?}: control: the band lies on the row ({deep:.3})"
        );
        let step = depth
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            step <= 0.2 * deep,
            "{style:?}: the band steps {step:.3} in one column (depth {deep:.3})"
        );
    }
}

/// The ink a light body's source-over quads lay, alpha-weighted: each quad's
/// premultiplied colour over its opacity, for the quads at least `min_alpha`
/// opaque. `None` when no quad is.
fn body_ink_of(under: &[GlowQuad], min_alpha: u8) -> Option<u32> {
    let (mut sum, mut weight) = ([0.0f64; 3], 0.0f64);
    for q in under.iter().filter(|q| q.alpha >= min_alpha) {
        let a = f64::from(q.alpha);
        let w = a * f64::from(q.w) * f64::from(q.h);
        for (i, sh) in [16u32, 8, 0].into_iter().enumerate() {
            sum[i] += f64::from((q.color >> sh) & 0xff) * 255.0 / a * w;
        }
        weight += w;
    }
    (weight > 0.0).then(|| {
        let ch = |i: usize| ((sum[i] / weight).round() as u32).min(255);
        (ch(0) << 16) | (ch(1) << 8) | ch(2)
    })
}

/// SPARKLE'S BODY ON PAPER IS GOLD: typed on GitHub Light in its automatic
/// colour, the body under the letters lays a bright, saturated gold (hue
/// 38°–50°, saturation and value at least 0.8), the gold its paper beads are
/// cored with, and the theme's text keeps [`BODY_CONTRAST_BAR`] over it
/// even opaque; with a pinned colour it keeps the pin's hue. Laid in its
/// hue's one-weight vivid ink, the starlight gold came out a burnt
/// orange-brown that read under the letters as a muddy caramel smear.
/// Negative control: that ink's value is under 0.8.
#[test]
fn sparkles_paper_body_is_gold() {
    let mut c = light(GlowStyle::Sparkle);
    c.theme_fg = 0x001F_2328;
    c.color = SPARKLE_DEFAULT_COLOR;
    let g = geom();
    let ink = |c: &GlowConfig, bold: bool| {
        let (mut glow, _out, t) = typed(c, 8);
        glow.sparkle_paper_bold_ink = bold;
        body_ink_of(&body(&mut glow, c, g, t, (2, 13)), 32).expect("the body lays ink")
    };
    let gold = ink(&c, false);
    let (h, s, v) = crate::color_math::rgb2hsv(gold);
    assert!(
        (38.0..=50.0).contains(&h) && s >= 0.8 && v >= 0.8,
        "the paper body is {gold:06X} (h {h:.0}, s {s:.2}, v {v:.2}), not a clean gold"
    );
    let contrast = {
        let (a, b) = (rel_luminance(c.theme_fg), rel_luminance(gold));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    };
    assert!(
        contrast >= crate::cursor_glow::light_body::BODY_CONTRAST_BAR,
        "the text keeps only {contrast:.2}:1 over the opaque gold"
    );
    // A pinned colour keeps its own hue.
    let mut pinned = c;
    pinned.color = 0x00D0_3A8C;
    let pin = ink(&pinned, false);
    let (ph, _, _) = crate::color_math::rgb2hsv(pin);
    let (want, _, _) = crate::color_math::rgb2hsv(0x00D0_3A8C);
    let off = (ph - want).abs().min(360.0 - (ph - want).abs());
    assert!(
        off <= 12.0,
        "a pinned {:06X} body came out {pin:06X}",
        0x00D0_3A8C
    );
    // The control: the one-weight vivid ink.
    let bold = ink(&c, true);
    let (_, _, bv) = crate::color_math::rgb2hsv(bold);
    assert!(
        bv < 0.8,
        "control: the vivid ink {bold:06X} is the muddy one (v {bv:.2})"
    );
}
