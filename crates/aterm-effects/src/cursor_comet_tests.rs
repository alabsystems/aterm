// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::effect_util::ink;
use std::time::Duration;

const fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    }
}
const fn cfg() -> CometConfig {
    CometConfig {
        light: crate::cursor_glow::Light::OFF,
        enabled: true,
        intensity: 1.0,
        color: 0x009E_D6FF,
        accent: 0x00C8_EAFF,
        dark_theme: true,
        theme_fg: 0x00C8_C8D2,
        theme_bg: 0x0010_1016,
        pinned: false,
        chosen: false,
    }
}

#[test]
fn disabled_or_zero_intensity_is_inert() {
    for (why, config) in [
        (
            "disabled",
            CometConfig {
                enabled: false,
                ..cfg()
            },
        ),
        (
            "zero intensity",
            CometConfig {
                intensity: 0.0,
                ..cfg()
            },
        ),
    ] {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let f = nc.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            geom(),
            &config,
            &mut out,
            &mut Vec::new(),
        );
        assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
        assert_eq!(f.fp, 0, "{why}");
        assert!(out.is_empty(), "{why}: no additive light");
        assert!(!nc.is_active(), "{why}: settled");
    }
}

#[test]
fn hidden_cursor_draws_no_coma() {
    let mut nc = CursorComet::default();
    let mut out = Vec::new();
    let f = nc.tick(
        None,
        Instant::now(),
        0.5,
        geom(),
        &cfg(),
        &mut out,
        &mut Vec::new(),
    );
    assert!(out.is_empty(), "no additive light without a cursor cell");
    assert!(f.fill.is_some());
}

#[test]
fn blaze_swells_and_brightens() {
    let g = geom();
    let c = cfg();
    let t = Instant::now();
    let run = |blaze: f32| -> (u64, i32, i32) {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        nc.tick(Some((3, 20)), t, blaze, g, &c, &mut out, &mut Vec::new());
        out.clear();
        nc.tick(
            Some((3, 20)),
            t + Duration::from_millis(16),
            blaze,
            g,
            &c,
            &mut out,
            &mut Vec::new(),
        );
        let x0 = out.iter().map(|q| q.x as i32).min().unwrap_or(0);
        let x1 = out.iter().map(|q| (q.x + q.w) as i32).max().unwrap_or(0);
        (ink(&out), x0, x1)
    };
    let (cold_ink, cold_x0, cold_x1) = run(0.0);
    let (hot_ink, hot_x0, hot_x1) = run(1.0);
    assert!(cold_ink > 0, "the resting nucleus is still visibly lit");
    assert!(
        hot_ink > cold_ink * 2,
        "full blaze far outshines the cold drift ({hot_ink} vs {cold_ink})"
    );
    assert!(
        hot_x1 - hot_x0 > cold_x1 - cold_x0,
        "the sunlit coma swells wider ({} vs {})",
        hot_x1 - hot_x0,
        cold_x1 - cold_x0
    );
}

const fn light_cfg() -> CometConfig {
    CometConfig {
        color: 0x0009_69DA,
        accent: 0x0040_90F0,
        dark_theme: false,
        theme_fg: 0x001F_2328,
        theme_bg: 0x00FF_FFFF,
        ..cfg()
    }
}

fn contrast(a: u32, b: u32) -> f32 {
    let (x, y) = (
        crate::color_math::relative_luminance(a),
        crate::color_math::relative_luminance(b),
    );
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

#[test]
fn light_nucleus_inks_a_round_head_under_the_letters() {
    let g = geom();
    let (row, col) = (3u16, 20u16);
    let inked = |blaze: f32| {
        let mut nc = CursorComet::default();
        let (mut out, mut under) = (Vec::new(), Vec::new());
        nc.tick(
            Some((row, col)),
            Instant::now(),
            blaze,
            g,
            &light_cfg(),
            &mut out,
            &mut under,
        );
        assert!(out.is_empty(), "no additive light on paper");
        under
    };
    assert!(
        inked(1.0).is_empty(),
        "at full blaze the streak's nucleus is the head"
    );
    let width = |blaze: f32| {
        let mut nc = CursorComet::default();
        let (mut out, mut under) = (Vec::new(), Vec::new());
        nc.tick(
            Some((row, col)),
            Instant::now(),
            blaze,
            g,
            &light_cfg(),
            &mut out,
            &mut under,
        );
        assert!(out.is_empty(), "no additive light on paper");
        assert!(!under.is_empty(), "the head is inked (blaze {blaze})");
        let top = row * g.ch as u16;
        assert!(
            under
                .iter()
                .all(|q| q.alpha > 0 && q.y >= top && q.y + q.h <= top + g.ch as u16),
            "source-over, inside its row"
        );
        let x0 = under.iter().map(|q| q.x).min().unwrap_or(0);
        let x1 = under.iter().map(|q| q.x + q.w).max().unwrap_or(0);
        let (left, right) = (col * g.cw as u16, (col + 1) * g.cw as u16);
        assert!(
            x0 + g.cw as u16 / 4 <= left && x1 >= right + g.cw as u16 / 4,
            "past both edges of the caret's cell: {x0}..{x1}"
        );
        x1 - x0
    };
    assert!(
        width(0.1) > g.cw as u16,
        "the resting head is round and wide"
    );
    let mut nc = CursorComet::default();
    let (mut out, mut under) = (Vec::new(), Vec::new());
    nc.tick(
        Some((row, col)),
        Instant::now(),
        0.5,
        g,
        &cfg(),
        &mut out,
        &mut under,
    );
    assert!(
        !out.is_empty() && under.is_empty(),
        "dark: the additive coma"
    );
}

#[test]
fn light_ground_nucleus_deepens_under_the_keys() {
    let g = geom();
    let fill = |c: &CometConfig, blaze: f32| {
        let mut nc = CursorComet::default();
        nc.tick(
            Some((1, 1)),
            Instant::now(),
            blaze,
            g,
            c,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .fill
        .expect("a fill")
    };
    let c = light_cfg();
    let (cold, hot) = (fill(&c, 0.0), fill(&c, 1.0));
    let paper = c.theme_bg;
    assert!(contrast(hot, paper) >= contrast(cold, paper));
    assert!(contrast(hot, paper) >= contrast(c.color, paper));
    let luma = crate::color_math::relative_luminance;
    let d = cfg();
    assert!(luma(fill(&d, 1.0)) > luma(fill(&d, 0.0)), "dark: it frosts");
}

#[test]
fn quads_respect_grid_hue_and_cap() {
    let g = geom();
    let c = cfg();
    let mut nc = CursorComet::default();
    let mut out = Vec::new();
    let t = Instant::now();
    nc.tick(Some((0, 0)), t, 1.0, g, &c, &mut out, &mut Vec::new());
    nc.tick(
        Some((0, 0)),
        t + Duration::from_millis(16),
        1.0,
        g,
        &c,
        &mut out,
        &mut Vec::new(),
    );
    assert!(!out.is_empty());
    let gw = (g.cols * g.cw) as u32;
    let gh = (g.rows * g.ch) as u32;
    for q in &out {
        let band = q.row as u32 * g.ch as u32;
        assert!(
            q.y as u32 >= band && q.y as u32 + q.h as u32 <= band + g.ch as u32,
            "single-row: {q:?}"
        );
        assert!(
            q.x as u32 + q.w as u32 <= gw && q.y as u32 + q.h as u32 <= gh,
            "in grid: {q:?}"
        );
        let (r, gg, b) = (
            (q.color >> 16) & 0xff,
            (q.color >> 8) & 0xff,
            q.color & 0xff,
        );
        assert!(
            b + 1 >= gg && gg + 1 >= r,
            "premultiplied icy hue ordering: {q:?}"
        );
        for sh in [16u32, 8, 0] {
            assert!(
                (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                "coverage capped: {q:?}"
            );
        }
    }
}

#[test]
fn settles_to_inactive_when_blaze_dies() {
    let g = geom();
    let c = cfg();
    let mut nc = CursorComet::default();
    let mut out = Vec::new();
    let t = Instant::now();
    nc.tick(Some((1, 1)), t, 0.8, g, &c, &mut out, &mut Vec::new());
    assert!(nc.is_active(), "a charged nucleus keeps the tick armed");
    nc.tick(
        Some((1, 1)),
        t + Duration::from_millis(16),
        0.0,
        g,
        &c,
        &mut out,
        &mut Vec::new(),
    );
    assert!(
        !nc.is_active(),
        "the settled nucleus idles on the blink cadence"
    );
}

#[test]
fn nucleus_follows_the_configured_hue() {
    let g = geom();
    let t = Instant::now();
    let mut out = Vec::new();
    let mut nc = CursorComet::default();
    let mut c = cfg();
    c.color = 0x00FF_4020;
    let f = nc
        .tick(Some((1, 1)), t, 0.2, g, &c, &mut out, &mut Vec::new())
        .fill
        .unwrap();
    let (r, gg, b) = ((f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff);
    assert!(
        r > b && r > gg,
        "the fill follows the pinned hue, got {f:#08x}"
    );
}

#[test]
fn coma_surrounds_the_caret_in_a_padded_headed_window() {
    let t0 = Instant::now();
    let real = Geom {
        cw: 15,
        ch: 28,
        rows: 30,
        cols: 100,
        origin_x: 24,
        origin_y: 84,
        win_w: 1548,
        win_h: 948,
        head: 80,
    };
    let (cr, cc) = (12u16, 47u16);
    let mut e = CursorComet::default();
    let mut out = Vec::new();
    e.tick(
        Some((cr, cc)),
        t0,
        1.0,
        real,
        &cfg(),
        &mut out,
        &mut Vec::new(),
    );
    assert!(!out.is_empty(), "the emitter drew nothing to place");
    let (x0c, y0c) = (
        i32::from(real.origin_x) + i32::from(cc) * real.cw as i32,
        i32::from(real.origin_y) + i32::from(cr) * real.ch as i32,
    );
    let (x1c, y1c) = (x0c + real.cw as i32, y0c + real.ch as i32);
    let x0 = out.iter().map(|q| i32::from(q.x)).min().unwrap();
    let x1 = out.iter().map(|q| i32::from(q.x + q.w)).max().unwrap();
    let y0 = out.iter().map(|q| i32::from(q.y)).min().unwrap();
    let y1 = out.iter().map(|q| i32::from(q.y + q.h)).max().unwrap();
    assert!(
        x0 < x0c && x1 > x1c && y0 < y0c && y1 > y1c,
        "bbox x {x0}..{x1} y {y0}..{y1} does not surround the caret cell \
         x {x0c}..{x1c} y {y0c}..{y1c} — a grid-relative emitter lands at \
         x {}..  y {}.., short by (origin_x, origin_y) = ({}, {})",
        x0 + i32::from(real.origin_x),
        y0 + i32::from(real.origin_y),
        real.origin_x,
        real.origin_y
    );
    for q in &out {
        assert!(
            i32::from(q.x) >= real.fx_left() && i32::from(q.x + q.w) <= real.fx_right(),
            "quad x {}..{} outside the effects box",
            q.x,
            q.x + q.w
        );
        assert!(
            i32::from(q.y) >= real.fx_top() && i32::from(q.y + q.h) <= real.fx_bot(),
            "quad y {}..{} outside the effects box",
            q.y,
            q.y + q.h
        );
    }
    let mut at_origin = Vec::new();
    let mut f = CursorComet::default();
    f.tick(
        Some((3, 10)),
        t0,
        1.0,
        geom(),
        &cfg(),
        &mut at_origin,
        &mut Vec::new(),
    );
    let mut shifted = Vec::new();
    let mut g = CursorComet::default();
    let mut o = geom();
    o.origin_x = 24;
    o.origin_y = 84;
    o.win_w += 48;
    o.win_h += 84;
    o.head = 84;
    g.tick(
        Some((3, 10)),
        t0,
        1.0,
        o,
        &cfg(),
        &mut shifted,
        &mut Vec::new(),
    );
    assert_eq!(
        at_origin.len(),
        shifted.len(),
        "the origin changed the quad COUNT"
    );
    for (a, b) in at_origin.iter().zip(&shifted) {
        assert_eq!(
            (i32::from(a.x) + 24, i32::from(a.y) + 84, a.w, a.h, a.color),
            (i32::from(b.x), i32::from(b.y), b.w, b.h, b.color),
            "a quad is not the grid form translated by the origin"
        );
    }
}

fn light_per_pixel(out: &[GlowQuad], w: usize, h: usize) -> Vec<u32> {
    let mut px = vec![[0u32; 3]; w * h];
    for q in out {
        assert_eq!(q.alpha, 0, "the dark coma is additive light");
        for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
            for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                    px[y * w + x][k] += (q.color >> sh) & 0xff;
                }
            }
        }
    }
    px.iter().map(|c| c[0].max(c[1]).max(c[2])).collect()
}

#[test]
fn the_coma_outside_the_caret_stays_inside_the_over_ink_budget() {
    let g = geom();
    let (w, h) = (g.cols * g.cw, g.rows * g.ch);
    let (cr, cc) = (3u16, 20u16);
    let (x0, y0) = (usize::from(cc) * g.cw, usize::from(cr) * g.ch);
    let mut nc = CursorComet::default();
    let t = Instant::now();
    let mut heart_max = 0u32;
    for i in 0..40u64 {
        let mut out = Vec::new();
        nc.tick(
            Some((cr, cc)),
            t + Duration::from_millis(16 * i),
            1.0,
            g,
            &cfg(),
            &mut out,
            &mut Vec::new(),
        );
        let light = light_per_pixel(&out, w, h);
        for y in 0..h {
            for x in 0..w {
                let inside = (x0..x0 + g.cw).contains(&x) && (y0..y0 + g.ch).contains(&y);
                let l = light[y * w + x];
                if inside {
                    heart_max = heart_max.max(l);
                } else {
                    assert!(
                        l as f32 <= OVER_INK_COV_CAP + 3.0,
                        "frame {i}: {l} levels of light on ({x},{y}) outside the caret"
                    );
                }
            }
        }
    }
    assert!(
        heart_max as f32 > OVER_INK_COV_CAP,
        "control: the stacked heart inside the caret ({heart_max}) exceeds the budget"
    );
}

/// looks10 L9, THE COMA IS ROUND LIGHT ALONE: through a second and a
/// half of a charged caret on a dark page, the coma lays no cross (a bar
/// one pixel tall with a stroke one pixel wide standing on it or hanging
/// from it inside its span). Negative control: its rim's twinkling
/// glints (`rim_glints`), the grey `+` beside the caret in the trail
/// gallery's dark Comet, lay crosses.
#[test]
fn the_coma_twinkles_no_cross() {
    let g = geom();
    let crosses = |glints: bool| {
        let mut nc = CursorComet {
            rim_glints: glints,
            ..CursorComet::default()
        };
        let t = Instant::now();
        let mut n = 0usize;
        for i in 0..90u64 {
            let mut out = Vec::new();
            nc.tick(
                Some((3, 20)),
                t + Duration::from_millis(16 * i),
                1.0,
                g,
                &cfg(),
                &mut out,
                &mut Vec::new(),
            );
            for bar in out.iter().filter(|q| q.h == 1 && q.w >= 3) {
                let (x0, x1, y) = (bar.x, bar.x + bar.w, bar.y);
                if out.iter().any(|s| {
                    s.w == 1 && s.x > x0 && s.x + 1 < x1 && (s.y + s.h == y || s.y == y + 1)
                }) {
                    n += 1;
                }
            }
        }
        n
    };
    assert_eq!(crosses(false), 0, "the coma lays a cross");
    assert!(
        crosses(true) > 0,
        "control: the rim's twinkling glints lay no cross"
    );
}

#[test]
fn the_light_comet_keeps_its_caret_and_lays_nothing_over_the_letters() {
    const WHITE: u32 = 0x00FF_FFFF;
    let g = geom();
    let (cr, cc) = (3u16, 20u16);
    let cell = (
        i32::from(cc) * g.cw as i32,
        i32::from(cr) * g.ch as i32,
        g.cw as i32,
        g.ch as i32,
    );
    let caret = |dark: bool| {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let c = CometConfig {
            dark_theme: dark,
            theme_fg: if dark { 0x00C8_C8D2 } else { 0x001F_2328 },
            theme_bg: if dark { 0x0010_1016 } else { WHITE },
            ..cfg()
        };
        let fill = nc
            .tick(
                Some((cr, cc)),
                Instant::now(),
                1.0,
                g,
                &c,
                &mut out,
                &mut Vec::new(),
            )
            .fill
            .expect("an enabled nucleus fills the block");
        let worst = crate::effect_util::caret_cell_worst_contrast(fill, &out, cell, WHITE);
        (worst, out)
    };
    let (light, over) = caret(false);
    assert!(
        light >= 3.0,
        "light arm: the nucleus must keep 3:1 on white, got {light:.2}:1"
    );
    assert!(
        over.is_empty(),
        "the light nucleus lays nothing over the letters: {over:?}"
    );
    let (dark_arm, coma) = caret(true);
    assert!(
        dark_arm < 3.0,
        "control: the frosting dark arm sinks into white ({dark_arm:.2}:1)"
    );
    assert!(!coma.is_empty(), "control: the dark arm lays its coma");
}

#[test]
fn the_light_nucleus_rests_clearly_above_the_page_floor() {
    use crate::color_math::{contrast_ratio, light_body_fill, rgb2hsv};
    let base = cfg().color;
    for page in [0x00FF_FFFFu32, 0x00F6_F7F9, 0x00FD_F6E3] {
        let fill_at = |blaze: f32| {
            let c = CometConfig {
                dark_theme: false,
                theme_bg: page,
                ..cfg()
            };
            CursorComet::default()
                .tick(
                    Some((1, 1)),
                    Instant::now(),
                    blaze,
                    geom(),
                    &c,
                    &mut Vec::new(),
                    &mut Vec::new(),
                )
                .fill
                .expect("an enabled nucleus fills the block")
        };
        let rest = fill_at(0.0);
        let ratio = contrast_ratio(rest, page);
        assert!(
            ratio >= LIGHT_NUCLEUS_CONTRAST,
            "on {page:06x} the resting nucleus {rest:06x} reads {ratio:.2}:1"
        );
        let (h0, _, _) = rgb2hsv(light_body_fill(base, 0.0));
        let (h1, _, v1) = rgb2hsv(rest);
        assert!((h0 - h1).abs() < 4.0, "the ice keeps its hue: {h0} vs {h1}");
        assert!(
            v1 < rgb2hsv(base).2,
            "on {page:06x} the nucleus deepens, never whitens: {rest:06x}"
        );
        let hot = fill_at(1.0);
        assert!(
            contrast_ratio(hot, page) >= ratio,
            "on {page:06x} the heat deepens the nucleus further"
        );
    }
    let unsolved = contrast_ratio(light_body_fill(base, 0.0), 0x00FF_FFFF);
    assert!(
        unsolved < 4.5,
        "control: the deepened ice alone reads {unsolved:.2}:1 on white"
    );
}
