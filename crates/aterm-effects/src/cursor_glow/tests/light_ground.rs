// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor glow light-ground rendering tests.

use super::*;

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

fn body(
    glow: &mut CursorGlow,
    c: &GlowConfig,
    g: Geom,
    at: Instant,
    cur: (u16, u16),
) -> Vec<GlowQuad> {
    let (mut out, mut halos, mut under) = (Vec::new(), Vec::new(), Vec::new());
    glow.emit_comet(
        at,
        c,
        g,
        Some(cur),
        &mut crate::cursor_glow::glow_sink(&mut out, &mut aterm_render::GlowRamps::default(), true),
        &mut halos,
        &mut crate::cursor_glow::glow_sink(
            &mut under,
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
    );
    under
}

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

fn ink_heights(under: &[GlowQuad]) -> std::collections::BTreeMap<u16, u16> {
    let mut heights = std::collections::BTreeMap::new();
    for q in under {
        for x in q.x..q.x + q.w {
            *heights.entry(x).or_insert(0) += q.h;
        }
    }
    heights
}

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
                latch: ParticleLatch::FRESH,
                edge_cut: false,
            });
            let (mut q, mut h, mut u) = (Vec::new(), Vec::new(), Vec::new());
            glow.emit_particles(
                t,
                &c,
                g,
                &mut q,
                &mut h,
                &mut crate::cursor_glow::glow_sink(
                    &mut u,
                    &mut aterm_render::GlowRamps::default(),
                    true,
                ),
                CursorGlow::MAX_QUADS,
            );
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
        glow.emit_bolts(
            t0,
            &c,
            g,
            &mut crate::cursor_glow::glow_sink(
                &mut bolts,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        bolts
    };
    assert!(
        strike
            .iter()
            .any(|q| u32::from(q.y) + u32::from(q.h) <= band(landing).start),
        "control: the laser's strike crosses the rows the jump spans"
    );
}

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
    let bold = ink(&c, true);
    let (_, _, bv) = crate::color_math::rgb2hsv(bold);
    assert!(
        bv < 0.8,
        "control: the vivid ink {bold:06X} is the muddy one (v {bv:.2})"
    );
}

/// A COLOUR THE USER CHOSE KEEPS ITS HUE AND CHROMA IN CLASSIC'S PAPER WAKE
/// ([`crate::classic_wake::WakeInk::chosen`]): a trail pinned to `#b0b0c8`
/// draws Classic's one-colour face, and on white every pixel its paper crown
/// and tracer lay beside the caret keeps the pin's hue and no more than the
/// chroma of the tone it is drawn from (the pin, or its accent: the pin
/// brightened, which clips to `#ffffff`). The gallery's `#d03a8c` keeps its two
/// tones' hues (the pin's 327 and its clipped accent's 316) and the
/// brighter tone's chroma. Solved from the hue's vivid ink, `#b0b0c8` (chroma
/// 24 of 255) was laid up to `#9595e1` (76) beside the caret. Control:
/// [`crate::classic_wake::ClassicWake::paper_pin_saturated`], the paper
/// crown and tracer alone reading no colour as chosen, which lays them past
/// the pin's chroma.
#[test]
fn a_pinned_classic_s_paper_wake_keeps_the_pin_s_hue_and_chroma() {
    use crate::color_math::rgb2hsv;
    let g = geom();
    let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
    let chroma = |c: u32| {
        let (r, gg, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        r.max(gg).max(b) - r.min(gg).min(b)
    };
    let hue = |c: u32| rgb2hsv(c & 0x00FF_FFFF).0;
    let apart = |a: f32, b: f32| (a - b).abs().min(360.0 - (a - b).abs());
    // The wake's paper pixels over a neutral white, frame by frame, across a
    // burst of keys.
    let laid = |c: &GlowConfig, saturated: bool| -> Vec<u32> {
        let mut glow = CursorGlow::default();
        glow.set_colour_pins(ColourPins {
            trail: true,
            accent: false,
        });
        glow.classic.paper_pin_saturated = saturated;
        let mut out = Vec::new();
        let mut t = Instant::now();
        glow.tick(Some((2, 4)), t, c, g, &mut out);
        let mut px_all = Vec::new();
        for col in 5..17 {
            t += Duration::from_millis(60);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((2, col)), t, c, g, &mut out);
            const NEUTRAL: u32 = 0x00FF_FFFF;
            let mut px = vec![NEUTRAL; w * h];
            for q in glow.under_quads() {
                let qw = u32::from(q.w.max(1));
                let at = |i: u32, a: u32, b: u32| {
                    (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw)
                };
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        let i = (x - usize::from(q.x)) as u32;
                        let (color, alpha) = if q.is_flat() {
                            (q.color, q.alpha)
                        } else {
                            let lerp = |sh: u32| {
                                at(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh
                            };
                            (
                                lerp(16) | lerp(8) | lerp(0),
                                at(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                            )
                        };
                        let slot = &mut px[y * w + x];
                        *slot = aterm_render::over_premul(*slot, color, alpha) & 0x00FF_FFFF;
                    }
                }
            }
            px_all.extend(px.into_iter().filter(|&p| p != NEUTRAL));
        }
        px_all
    };
    let mut control_over = 0usize;
    for pin in [0x00B0_B0C8u32, 0x00D0_3A8C] {
        let mut c = light(GlowStyle::Classic);
        c.classic_mono = true;
        c.color = pin;
        c.accent = crate::cursor_fx::brighten(pin, 1.5);
        let tones = [pin, c.accent];
        let most = tones.iter().map(|&t| chroma(t)).max().unwrap_or(0);
        // A pixel holds a hue once it has chroma to hold one (8 of 255); it
        // keeps the hue of one of its tones, or lies between them, to within
        // its channels' rounding (a level of its middle channel moves its hue
        // 60 degrees over its chroma).
        let hues: Vec<f32> = tones
            .iter()
            .filter(|&&t| chroma(t) >= 8)
            .map(|&t| hue(t))
            .collect();
        let off = |p: u32| {
            let tol = 2.0 + 60.0 / chroma(p).max(1) as f32;
            chroma(p) > most + 1
                || (chroma(p) >= 8
                    && !hues.is_empty()
                    && hues.iter().all(|&t| apart(hue(p), t) > tol)
                    && !(hues.len() == 2
                        && apart(hue(p), hues[0]) + apart(hue(p), hues[1])
                            <= apart(hues[0], hues[1]) + tol))
        };
        let px = laid(&c, false);
        assert!(
            px.len() > 500,
            "{pin:06x}: the paper wake lays ({} px)",
            px.len()
        );
        let bad: Vec<String> = px
            .iter()
            .filter(|&&p| off(p))
            .map(|p| format!("{p:06x}"))
            .collect();
        assert!(
            bad.is_empty(),
            "{pin:06x}: {} of {} paper pixels leave its tones' hues or pass chroma {most}: {:?}",
            bad.len(),
            px.len(),
            &bad[..bad.len().min(8)]
        );
        control_over += laid(&c, true)
            .iter()
            .filter(|&&p| chroma(p) > most + 1)
            .count();
    }
    assert!(
        control_over > 0,
        "control: solved from the hue's vivid ink, the paper wake keeps within the pins' chroma"
    );
}
