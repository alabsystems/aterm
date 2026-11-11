// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::effect_util::ink;
use crate::effect_util::push_fx_rect;
use std::time::Duration;

const fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 8,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 128,
        head: 0,
    }
}
const fn cfg() -> DropletConfig {
    DropletConfig {
        enabled: true,
        intensity: 1.0,
        ground: None,
    }
}

/// Disabled or zero intensity: fully inert (no fill, no bead, fp 0, settled).
#[test]
fn disabled_or_zero_intensity_is_inert() {
    for (why, config) in [
        (
            "disabled",
            DropletConfig {
                enabled: false,
                intensity: 1.0,
                ground: None,
            },
        ),
        (
            "zero intensity",
            DropletConfig {
                enabled: true,
                intensity: 0.0,
                ground: None,
            },
        ),
    ] {
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let f = d.tick(Some((1, 1)), Instant::now(), 1.0, geom(), &config, &mut out);
        assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
        assert_eq!(f.fp, 0, "{why}");
        assert!(out.is_empty(), "{why}: no additive light");
        assert!(!d.is_active(), "{why}: settled");
    }
}

/// A hidden cursor draws no bead; the fill stays resolved.
#[test]
fn hidden_cursor_draws_no_bead() {
    let mut d = CursorDroplet::default();
    let mut out = Vec::new();
    let f = d.tick(None, Instant::now(), 0.5, geom(), &cfg(), &mut out);
    assert!(out.is_empty(), "no additive light without a cursor cell");
    assert!(f.fill.is_some());
}

/// Full surge swells the bead and shines far brighter than still water.
#[test]
fn surge_swells_and_brightens() {
    let g = geom();
    let c = cfg();
    let t = Instant::now();
    let run = |surge: f32| -> u64 {
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        d.tick(Some((3, 20)), t, surge, g, &c, &mut out);
        out.clear();
        d.tick(
            Some((3, 20)),
            t + Duration::from_millis(16),
            surge,
            g,
            &c,
            &mut out,
        );
        ink(&out)
    };
    let still_ink = run(0.0);
    let surge_ink = run(1.0);
    assert!(still_ink > 0, "still water is still visibly lit");
    assert!(
        surge_ink > still_ink * 2,
        "full surge far outshines still water ({surge_ink} vs {still_ink})"
    );
}

/// Through two whole ripple and drip cycles at full surge, nothing the
/// droplet draws lies lower than its bead's own foot (the bead overhangs
/// the cell, the rest is cut at the row's foot). The negative control,
/// uncut, runs rings and drops into the row below, past the bead: in the
/// trail gallery's `context` on dark, onto the selected line under the
/// prompt.
#[test]
fn ripples_and_drips_stay_in_the_caret_s_row() {
    let g = geom();
    let lowest = |uncut: bool| {
        let mut d = CursorDroplet {
            uncut,
            ..CursorDroplet::default()
        };
        let t = Instant::now();
        let mut low = 0i32;
        for i in 0..400u64 {
            let mut out = Vec::new();
            d.tick(
                Some((2, 5)),
                t + Duration::from_millis(16 * i),
                1.0,
                g,
                &cfg(),
                &mut out,
            );
            for q in &out {
                low = low.max(i32::from(q.y) + i32::from(q.h));
            }
        }
        low
    };
    let ch = g.ch as f32;
    let bead_foot = ((2.0 + 0.46) * ch + RADIUS_MAX * ch).ceil() as i32 + 1;
    let cut = lowest(false);
    assert!(
        cut <= bead_foot,
        "a ring or a drop reached pixel row {cut}, under the bead's foot {bead_foot}"
    );
    let uncut = lowest(true);
    assert!(
        uncut > bead_foot + 2,
        "the negative control stays at pixel row {uncut}: the law above reads nothing"
    );
}

/// Every emitted quad is single-row, grid-interior, and water-hued with capped
/// coverage (the renderer row-gate / parity / legibility invariants).
#[test]
fn quads_respect_grid_hue_and_cap() {
    let g = geom();
    let c = cfg();
    let mut d = CursorDroplet::default();
    let mut out = Vec::new();
    let t = Instant::now();
    for i in 0..8 {
        if i == 7 {
            out.clear();
        }
        d.tick(
            Some((0, 0)),
            t + Duration::from_millis(16 * i),
            1.0,
            g,
            &c,
            &mut out,
        );
    }
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
            "premultiplied water hue ordering: {q:?}"
        );
        for sh in [16u32, 8, 0] {
            assert!(
                (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                "coverage capped: {q:?}"
            );
        }
    }
}

/// Surging keeps the tick armed; a dead surge settles to inactive.
#[test]
fn settles_to_inactive_when_surge_dies() {
    let g = geom();
    let c = cfg();
    let mut d = CursorDroplet::default();
    let mut out = Vec::new();
    let t = Instant::now();
    d.tick(Some((1, 1)), t, 0.8, g, &c, &mut out);
    assert!(d.is_active(), "surging water keeps the tick armed");
    d.tick(
        Some((1, 1)),
        t + Duration::from_millis(16),
        0.0,
        g,
        &c,
        &mut out,
    );
    assert!(!d.is_active(), "still water idles on the blink cadence");
}

/// The bead surrounds the caret in a padded, headed window: the producer
/// folds in the grid origin, which the origin (0,0) tests cannot tell.
#[test]
fn bead_surrounds_the_caret_in_a_padded_headed_window() {
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
    let mut e = CursorDroplet::default();
    let mut out = Vec::new();
    e.tick(Some((cr, cc)), t0, 1.0, real, &cfg(), &mut out);
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
    let mut f = CursorDroplet::default();
    f.tick(Some((3, 10)), t0, 1.0, geom(), &cfg(), &mut at_origin);
    let mut shifted = Vec::new();
    let mut g = CursorDroplet::default();
    let mut o = geom();
    o.origin_x = 24;
    o.origin_y = 84;
    o.win_w += 48;
    o.win_h += 84;
    o.head = 84;
    g.tick(Some((3, 10)), t0, 1.0, o, &cfg(), &mut shifted);
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

/// The caret cell at `(1, 1)` of [`geom`], composited with everything one
/// full-surge tick emits over a page, and the fill the tick returned.
fn surged_caret(ground: Option<u32>, page: u32) -> (f32, u32, usize) {
    let g = geom();
    let mut d = CursorDroplet::default();
    let mut out = Vec::new();
    let c = DropletConfig { ground, ..cfg() };
    let fill = d
        .tick(Some((1, 1)), Instant::now(), 1.0, g, &c, &mut out)
        .fill
        .expect("an enabled droplet fills the block");
    let cell = (g.cw as i32, g.ch as i32, g.cw as i32, g.ch as i32);
    (
        crate::effect_util::caret_cell_worst_contrast(fill, &out, cell, page),
        fill,
        out.len(),
    )
}

/// THE LIQUID CARET KEEPS A MARGIN PAST THE PAGE FLOOR (the after-land lows' W8): solved to
/// the floor itself, the band's edge laid Water's caret on Solarized Dark at 4.51:1. On the
/// reference dark, Solarized Dark, white and Solarized Light, both ends of the band read at
/// least [`DROPLET_CARET_FLOOR`] against the page. Control: the band solved to
/// [`CARET_PAGE_MIN_CONTRAST`] has an end under it on Solarized Dark.
#[test]
fn the_liquid_caret_keeps_a_margin_past_the_page_floor() {
    let ends = |band: (f32, f32), page: u32| {
        let c = |p: f32| crate::color_math::contrast_ratio(water_ramp(p), page);
        c(band.0).min(c(band.1))
    };
    for (page, dark) in [
        (crate::cursor_glow::REF_DARK_PAGE, true),
        (0x0000_2B36, true),
        (0x00FF_FFFF, false),
        (0x00FD_F6E3, false),
    ] {
        let worst = ends(ramp_band_on(page, dark), page);
        assert!(
            worst >= DROPLET_CARET_FLOOR,
            "#{page:06x}: the caret's band reads down to {worst:.3}:1"
        );
    }
    let control = ends(
        ramp_band_solve(0x0000_2B36, true, CARET_PAGE_MIN_CONTRAST),
        0x0000_2B36,
    );
    assert!(
        control < DROPLET_CARET_FLOOR,
        "control: the band solved to the floor itself keeps {control:.3}:1"
    );
}

/// On a light page at full surge the droplet keeps its caret: the fill is
/// solved against the actual ground (white and Solarized's off-white) and
/// no additive bead lands on it.
#[test]
fn the_light_droplet_keeps_its_caret_on_a_light_page() {
    for page in [0x00FF_FFFF, 0x00FD_F6E3] {
        let (worst, fill, quads) = surged_caret(Some(page), page);
        assert_eq!(quads, 0, "no additive light on a light page");
        assert!(
            worst >= 3.0,
            "#{page:06x}: the caret must keep 3:1, got {worst:.2}:1"
        );
        assert!(
            crate::color_math::contrast_ratio(fill, page) >= CARET_PAGE_MIN_CONTRAST,
            "#{page:06x}: the fill {fill:06x} must clear the page floor"
        );
        let (r, gg, b) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
        assert!(b >= gg && gg >= r, "still a water hue, got {fill:06x}");
    }
    let (control, _, _) = surged_caret(None, 0x00FF_FFFF);
    assert!(
        control < 3.0,
        "control: the dark arm sinks into white ({control:.2}:1)"
    );
}

/// On the dark default page still water is solved up the ramp until the
/// fill clears the page floor, so the glyph cut out of it in the page
/// colour stays readable.
#[test]
fn still_water_is_solved_against_a_dark_page() {
    const PAGE: u32 = 0x0011_1318;
    let still = |ground| {
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let c = DropletConfig { ground, ..cfg() };
        d.tick(Some((1, 1)), Instant::now(), 0.0, geom(), &c, &mut out)
            .fill
            .expect("an enabled droplet fills the block")
    };
    let solved = still(Some(PAGE));
    assert!(
        crate::color_math::contrast_ratio(solved, PAGE) >= CARET_PAGE_MIN_CONTRAST,
        "still water {solved:06x} must clear the page floor"
    );
    let unsolved = still(None);
    assert!(
        crate::color_math::contrast_ratio(unsolved, PAGE) < CARET_PAGE_MIN_CONTRAST,
        "control: unsolved still water {unsolved:06x} sits under it"
    );
}

/// THE BEAD IS ROUND AND SOFT: at full surge the light across the bead's
/// middle row climbs to its heart and falls away a little at a time (no
/// pixel steps by more than a quarter of the peak), and down its middle
/// column too, where hard-edged slabs once stood it in stairs.
#[test]
fn the_bead_is_round_and_soft() {
    let g = Geom {
        cw: 14,
        ch: 26,
        rows: 8,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 560,
        win_h: 208,
        head: 0,
    };
    let mut d = CursorDroplet::default();
    let mut out = Vec::new();
    d.tick(Some((3, 20)), Instant::now(), 1.0, g, &cfg(), &mut out);
    let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
    let light = |quads: &[GlowQuad]| {
        let mut px = vec![0u32; w * h];
        for q in quads {
            let qw = u32::from(q.w);
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for i in 0..qw {
                    let x = usize::from(q.x) + i as usize;
                    if x < w {
                        let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                        px[y * w + x] = aterm_render::add_sat(px[y * w + x], c);
                    }
                }
            }
        }
        px.iter()
            .map(|&p| ((p >> 8) & 0xff) as i32)
            .collect::<Vec<i32>>()
    };
    let lit = light(&out);
    let (cx, cy) = (20 * 14 + 7, 3 * 26 + 12);
    let across: Vec<i32> = (cx - 20..cx + 20).map(|x| lit[cy * w + x]).collect();
    let down: Vec<i32> = (cy - 16..cy + 14).map(|y| lit[y * w + cx]).collect();
    let peak = across.iter().chain(&down).copied().max().unwrap_or(0);
    assert!(peak > 40, "the bead is lit ({peak})");
    let steepest = |l: &[i32]| l.windows(2).map(|p| (p[1] - p[0]).abs()).max().unwrap_or(0);
    assert!(
        steepest(&across) * 4 <= peak && steepest(&down) * 4 <= peak,
        "a hard edge in the bead: {} across, {} down, of {peak}",
        steepest(&across),
        steepest(&down)
    );
    let mut slab = Vec::new();
    push_fx_rect(
        &mut slab,
        g,
        cx as i32 - 8,
        cy as i32 - 8,
        16,
        16,
        premul_rgb(0x0032_DCDE, 92),
    );
    let hard = light(&slab);
    let row: Vec<i32> = (cx - 20..cx + 20).map(|x| hard[cy * w + x]).collect();
    assert!(
        steepest(&row) * 4 > *row.iter().max().unwrap_or(&0),
        "control: a slab's edge is a step"
    );
}
