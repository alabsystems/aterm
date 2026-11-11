// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cursor_glow::BEAM_DEFAULT_COLOR;

const fn geom() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 10,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 400,
        win_h: 200,
        head: 0,
    }
}

const fn cfg(bar: bool) -> BeamRodConfig {
    BeamRodConfig {
        light: crate::cursor_glow::Light::OFF,
        enabled: true,
        intensity: 1.0,
        color: BEAM_DEFAULT_COLOR,
        haze: aterm_render::BEAM_SPACE_HAZE,
        bar,
        shimmer: false,
        dark_theme: true,
        theme_fg: 0x00C8_C8D2,
        theme_bg: 0x0010_1016,
        pinned: false,
        chosen: false,
    }
}

/// Disabled or zero amplitude: no quads, no fill, fingerprint 0, settled.
#[test]
fn disabled_or_reduced_motion_is_inert() {
    let mut rod = CursorBeamRod::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut c = cfg(true);
    c.enabled = false;
    let f = rod.tick(Some((2, 3)), t0, 1.0, geom(), &c, &mut out, &mut Vec::new());
    assert!(out.is_empty() && f.fill.is_none() && f.fp == 0);
    assert!(!rod.is_active());
    let mut c = cfg(true);
    c.intensity = 0.0;
    let f = rod.tick(Some((2, 3)), t0, 1.0, geom(), &c, &mut out, &mut Vec::new());
    assert!(out.is_empty() && f.fill.is_none() && f.fp == 0);
    assert!(!rod.is_active());
}

/// The bar rod hugs the bar's x, overrides no fill, stays in-grid, and caps
/// any quad crossing into the neighbour column to a tint.
#[test]
fn bar_rod_hugs_the_bar_and_stays_text_safe() {
    let mut rod = CursorBeamRod::default();
    let mut out = Vec::new();
    let g = geom();
    let f = rod.tick(
        Some((2, 3)),
        Instant::now(),
        1.0,
        g,
        &cfg(true),
        &mut out,
        &mut Vec::new(),
    );
    assert!(f.fill.is_none(), "bars have no fill-override channel");
    assert!(f.fp != 0, "a charged rod is drawable");
    assert!(rod.is_active(), "full blaze ⇒ still animating");
    assert!(!out.is_empty(), "the rod emits light");
    let cell_x0 = 3 * g.cw as i32;
    for q in &out {
        assert!((q.x as usize) < g.cols * g.cw && (q.y as usize) < g.rows * g.ch);
        assert!(
            (q.x as i32) >= cell_x0 - g.cw as i32 && (q.x as i32) <= cell_x0 + g.cw as i32,
            "rod light strays from the bar: x={}",
            q.x
        );
        let a = (q.color >> 24) & 0xff;
        if (q.x as i32) < cell_x0 {
            assert!(a <= COV_CAP as u32, "neighbour-crossing wash too hot: {a}");
        }
    }
}

/// The rod lands inside the padded window's effects box (origin and head band applied).
#[test]
fn rod_lands_at_the_window_origin() {
    for bar in [true, false] {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let mut g = geom();
        g.origin_x = 30;
        g.origin_y = 120;
        g.win_w = 460;
        g.win_h = 440;
        let f = rod.tick(
            Some((2, 3)),
            Instant::now(),
            1.0,
            g,
            &cfg(bar),
            &mut out,
            &mut Vec::new(),
        );
        assert!(f.fp != 0 && !out.is_empty(), "charged light must draw");
        for q in &out {
            assert!(
                q.y >= g.origin_y,
                "bar={bar}: quad above the grid origin below row 0: y={}",
                q.y
            );
            assert!(
                q.x >= g.origin_x.saturating_sub(g.cw as u16),
                "bar={bar}: quad left of the grid origin: x={}",
                q.x
            );
        }

        let mut top_out = Vec::new();
        let mut top_g = g;
        top_g.head = 12;
        rod.reset();
        let f0 = rod.tick(
            Some((0, 3)),
            Instant::now(),
            1.0,
            top_g,
            &cfg(bar),
            &mut top_out,
            &mut Vec::new(),
        );
        assert!(
            f0.fp != 0 && !top_out.is_empty(),
            "row-0 charged light must draw"
        );
        let min_y = if bar {
            top_g.origin_y.saturating_sub(top_g.head)
        } else {
            top_g.origin_y
        };
        for q in &top_out {
            assert!(
                q.y >= min_y,
                "bar={bar}: row-0 quad above the allowed top bound ({min_y}): y={}",
                q.y
            );
            assert!(
                q.x >= top_g.origin_x.saturating_sub(top_g.cw as u16),
                "bar={bar}: row-0 quad left of the grid origin: x={}",
                q.x
            );
        }
        if bar {
            assert!(
                top_out.iter().any(|q| q.y < top_g.origin_y),
                "row-0 bar rod overshoots into the head band"
            );
        }
    }
}

/// A dead blaze reports inactive, so the host disarms the tick.
#[test]
fn settles_when_the_blaze_dies() {
    let mut rod = CursorBeamRod::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    rod.tick(
        Some((2, 3)),
        t0,
        1.0,
        geom(),
        &cfg(true),
        &mut out,
        &mut Vec::new(),
    );
    assert!(rod.is_active());
    rod.tick(
        Some((2, 3)),
        t0,
        0.0,
        geom(),
        &cfg(true),
        &mut out,
        &mut Vec::new(),
    );
    assert!(!rod.is_active(), "a dead blaze settles the animator");
}

/// On a white page at full blaze the emitter block (GitHub Light's cursor
/// `#0969DA` as its colour) keeps 3:1 with everything it emits composited
/// over the caret cell, and the bar draws no additive rod.
#[test]
fn the_light_emitter_keeps_its_caret_on_a_white_page() {
    const WHITE: u32 = 0x00FF_FFFF;
    let g = geom();
    let cell = (3 * g.cw as i32, 2 * g.ch as i32, g.cw as i32, g.ch as i32);
    let caret = |dark: bool| {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let c = BeamRodConfig {
            color: 0x0009_69DA,
            haze: 0x0004_3466,
            dark_theme: dark,
            theme_fg: if dark { 0x00C8_C8D2 } else { 0x001F_2328 },
            theme_bg: if dark { 0x0010_1016 } else { WHITE },
            ..cfg(false)
        };
        let mut under = Vec::new();
        let fill = rod
            .tick(
                Some((2, 3)),
                Instant::now(),
                1.0,
                g,
                &c,
                &mut out,
                &mut under,
            )
            .fill
            .expect("block gets a fill");
        assert!(dark || under.is_empty(), "no aura of ink beside the block");
        crate::effect_util::caret_cell_worst_contrast(fill, &out, cell, WHITE)
    };
    let light = caret(false);
    assert!(
        light >= 3.0,
        "light arm: the caret must keep 3:1 on white, got {light:.2}:1"
    );
    let dark_arm = caret(true);
    assert!(
        dark_arm < 3.0,
        "control: the whitening dark arm sinks into white ({dark_arm:.2}:1)"
    );
    let mut rod = CursorBeamRod::default();
    let mut out = Vec::new();
    let bar = BeamRodConfig {
        dark_theme: false,
        theme_fg: 0x001F_2328,
        theme_bg: WHITE,
        ..cfg(true)
    };
    let mut under = Vec::new();
    rod.tick(
        Some((2, 3)),
        Instant::now(),
        1.0,
        g,
        &bar,
        &mut out,
        &mut under,
    );
    assert!(out.is_empty(), "no additive rod on a light page");
    assert!(!under.is_empty(), "the bar's rod is ink under the letters");
}

/// ON PAPER THE RESTING EMITTER READS CLEARLY ABOVE THE CARET FLOOR, IN
/// ITS OWN HUE: Sparkle's gold and Beam's ice at rest (and charged) keep
/// [`LIGHT_EMITTER_CONTRAST`] on GitHub Light and Default Light, within a
/// few degrees of their hue and no greyer than the hue itself.
#[test]
fn the_light_emitter_rests_clearly_above_the_floor_in_its_hue() {
    use crate::color_math::{contrast_ratio, light_body_fill, rgb2hsv};
    use crate::cursor_glow::{BEAM_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR};
    let hue_gap = |a: f32, b: f32| {
        let d = (a - b).abs().rem_euclid(360.0);
        d.min(360.0 - d)
    };
    for (paper, ink) in [(0x00FF_FFFF, 0x001F_2328), (0x00F6_F7F9, 0x002B_2D33)] {
        for (color, shimmer) in [(SPARKLE_DEFAULT_COLOR, true), (BEAM_DEFAULT_COLOR, false)] {
            for blaze in [0.0, 1.0] {
                let mut rod = CursorBeamRod::default();
                let (mut out, mut under) = (Vec::new(), Vec::new());
                let c = BeamRodConfig {
                    color,
                    shimmer,
                    dark_theme: false,
                    theme_fg: ink,
                    theme_bg: paper,
                    ..cfg(false)
                };
                let fill = rod
                    .tick(
                        Some((2, 3)),
                        Instant::now(),
                        blaze,
                        geom(),
                        &c,
                        &mut out,
                        &mut under,
                    )
                    .fill
                    .expect("a block gets a fill");
                let ratio = contrast_ratio(fill, paper);
                assert!(
                    ratio >= LIGHT_EMITTER_CONTRAST - 0.05,
                    "{color:06x} at blaze {blaze} on {paper:06x}: {fill:06x} reads {ratio:.2}:1"
                );
                let (h0, s0, _) = rgb2hsv(color);
                let (h, s, _) = rgb2hsv(fill);
                assert!(
                    hue_gap(h, h0) <= 6.0 && s >= s0 - 0.02,
                    "{color:06x} rests as {fill:06x}: off its hue or toward grey"
                );
            }
            let control = contrast_ratio(light_body_fill(color, 0.0), paper);
            assert!(
                control < 4.75,
                "control: the deepening alone of {color:06x} reads {control:.2}:1"
            );
        }
    }
    let pinned = 0x00FF_3399;
    let mut rod = CursorBeamRod::default();
    let (mut out, mut under) = (Vec::new(), Vec::new());
    let fill = rod
        .tick(
            Some((2, 3)),
            Instant::now(),
            0.0,
            geom(),
            &BeamRodConfig {
                color: pinned,
                pinned: true,
                chosen: true,
                dark_theme: false,
                theme_fg: 0x001F_2328,
                theme_bg: 0x00FF_FFFF,
                ..cfg(false)
            },
            &mut out,
            &mut under,
        )
        .fill;
    assert_eq!(fill, Some(pinned), "a pinned colour rests as pinned");
}

/// Sparkle's shimmer: at full blaze its breath advances no faster than
/// [`SHIMMER_MAX_TURNS_PER_S`], while at rest it still trembles faster
/// than the plain breath (the glitter is kept, only its ceiling is new).
#[test]
fn the_sparkle_shimmer_stays_inside_the_flash_bound() {
    let advance = |shimmer: bool, blaze: f32| {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        let c = BeamRodConfig {
            shimmer,
            ..cfg(false)
        };
        rod.tick(
            Some((2, 3)),
            t0,
            blaze,
            geom(),
            &c,
            &mut out,
            &mut Vec::new(),
        );
        let before = rod.breathe;
        rod.tick(
            Some((2, 3)),
            t0 + std::time::Duration::from_millis(200),
            blaze,
            geom(),
            &c,
            &mut out,
            &mut Vec::new(),
        );
        (rod.breathe - before).rem_euclid(1.0) / 0.2
    };
    let hot = advance(true, 1.0);
    assert!(
        hot <= SHIMMER_MAX_TURNS_PER_S + 1e-3,
        "charged shimmer at {hot:.2} turns/s exceeds the bound"
    );
    assert!(
        advance(true, 0.0) > advance(false, 0.0),
        "the idle shimmer still trembles faster than the plain breath"
    );
}

/// The light every quad lays on each pixel of a `w`×`h` window, summed
/// by the renderer's gradient law (the brightest channel).
fn light_per_pixel(quads: &[GlowQuad], w: usize, h: usize) -> Vec<u32> {
    let mut px = vec![0u32; w * h];
    for q in quads {
        let qw = u32::from(q.w);
        for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
            for i in 0..qw {
                let x = usize::from(q.x) + i as usize;
                if x >= w {
                    break;
                }
                let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                px[y * w + x] += (c >> 16 & 0xff).max(c >> 8 & 0xff).max(c & 0xff);
            }
        }
    }
    px
}

/// The emitter block's aura on a dark page is one round glow beside the
/// block, inside its row: at rest and at full blaze, the rows above and
/// below take no light (a selection row under the prompt keeps its
/// letters), no pixel outside the cell carries more than the over-ink
/// budget, the light shows on both sides of the block, it melts toward
/// the row's top and bottom edges instead of stopping at them, and along
/// the row it falls away from the block a few levels a pixel.
#[test]
fn the_block_aura_is_a_round_glow_inside_the_caret_row() {
    let g = geom();
    let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
    let (cr, cc) = (4u16, 20u16);
    let band = usize::from(cr) * g.ch..(usize::from(cr) + 1) * g.ch;
    let cell = usize::from(cc) * g.cw..(usize::from(cc) + 1) * g.cw;
    let mid = band.start + g.ch / 2;
    let shape = |quads: &[GlowQuad]| {
        let light = light_per_pixel(quads, w, h);
        let outside_row: u32 = (0..h)
            .filter(|y| !band.contains(y))
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| light[y * w + x])
            .sum();
        let step = (1..cell.start)
            .map(|x| light[mid * w + x].abs_diff(light[mid * w + x - 1]))
            .max()
            .unwrap_or(0);
        (light, outside_row, step)
    };
    for blaze in [0.0f32, 0.5, 1.0] {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        rod.tick(
            Some((cr, cc)),
            Instant::now(),
            blaze,
            g,
            &cfg(false),
            &mut out,
            &mut Vec::new(),
        );
        assert!(!out.is_empty(), "blaze {blaze}: the emitter glows");
        assert!(
            out.iter().all(|q| q.alpha == 0 && q.alpha2 == 0),
            "additive light on a dark page"
        );
        let (light, outside_row, step) = shape(&out);
        assert_eq!(outside_row, 0, "blaze {blaze}: the aura left its row");
        let outside_cell_max = band
            .clone()
            .flat_map(|y| (0..w).filter(|x| !cell.contains(x)).map(move |x| (x, y)))
            .map(|(x, y)| light[y * w + x])
            .max()
            .unwrap_or(0);
        assert!(
            outside_cell_max <= OVER_INK_COV_CAP as u32,
            "blaze {blaze}: {outside_cell_max} levels beside the block"
        );
        let lit = |x: usize| light[mid * w + x] > 0;
        assert!(
            lit(cell.start - 1) && lit(cell.end),
            "blaze {blaze}: the glow shows on both sides of the block"
        );
        let rim = (0..w)
            .map(|x| light[band.start * w + x].max(light[(band.end - 1) * w + x]))
            .max()
            .unwrap_or(0);
        assert!(
            rim * 4 <= outside_cell_max,
            "blaze {blaze}: an edge at the row's rim ({rim} of {outside_cell_max})"
        );
        assert!(
            step <= 12,
            "blaze {blaze}: a {step}-level step along the row"
        );
    }
    let mut boxed = Vec::new();
    let (x, y) = (
        (usize::from(cc) * g.cw) as i32,
        (usize::from(cr) * g.ch) as i32,
    );
    for (reach, cov) in [(9, 23u8), (5, 44), (2, 74)] {
        push_rect(
            &mut boxed,
            g,
            x - reach,
            y - reach,
            g.cw as i32 + 2 * reach,
            g.ch as i32 + 2 * reach,
            premul_rgb(BEAM_DEFAULT_COLOR, cov),
        );
    }
    let (_, outside_row, step) = shape(&boxed);
    assert!(
        outside_row > 0 && step > 12,
        "control: the nested box lights other rows ({outside_row}) and steps by {step}"
    );
}
