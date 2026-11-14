// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeMap;

use super::*;
use crate::cursor_glow::SoundCue;
use crate::rainbow_kitty::stardust::{FAN_RISE_MAX_CH, Star, StarClass, StarLane};
use crate::rainbow_kitty::timing::FAN_HERO_N;
use crate::rainbow_kitty::{CaretSeam, Config};
use crate::spectrum::SPECTRUM_ANCHORS;

const fn geom() -> Geom {
    Geom {
        cw: 9,
        ch: 18,
        rows: 40,
        cols: 120,
        origin_x: 0,
        origin_y: 0,
        win_w: 1200,
        win_h: 800,
        head: 0,
    }
}

const fn config() -> Config {
    Config {
        dark_theme: true,
        intensity: 1.0,
        duration: Duration::from_millis(900),
        ribbon_tall: true,
        ribbon_flat: false,
        theme_fg: 0x00E8_E8F0,
        theme_bg: 0x0016_161C,
        reduced_motion: false,
        light: crate::cursor_glow::Light::OFF,
        head_dx: 0.5,
    }
}

#[derive(Default)]
struct Scratch {
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    under_ramps: aterm_render::GlowRamps,
    out_ramps: aterm_render::GlowRamps,
    halos: Vec<RainHalo>,
    beams: Vec<BeamVertex>,
    cues: Vec<SoundCue>,
}

impl Scratch {
    const fn frame(&mut self) -> Frame<'_> {
        Frame {
            under: &mut self.under,
            out: &mut self.out,
            under_ramps: &mut self.under_ramps,
            out_ramps: &mut self.out_ramps,
            halos: &mut self.halos,
            beams: &mut self.beams,
            cues: &mut self.cues,
            caret: CaretSeam::EMPTY,
            companion: None,
            fp: 0,
            rows_cost: Some(aterm_render::RampCost::NEVER),
        }
    }
    fn clear(&mut self) {
        self.under.clear();
        self.out.clear();
        self.halos.clear();
        self.beams.clear();
        self.cues.clear();
    }
    fn emit(&mut self, m: &mut Meteors, ctx: &Ctx<'_>) {
        self.clear();
        let mut fr = self.frame();
        m.emit(ctx, &mut fr);
    }
}

const fn ctx_with(
    now: Instant,
    cfg: &Config,
    caret: (u16, u16),
    caret_t: f32,
    disp: f32,
    birth_disp: f32,
) -> Ctx<'_> {
    Ctx {
        now,
        geom: geom(),
        cfg,
        disp,
        birth_disp,
        phase: 0.0,
        caret,
        caret_t,
        caret_walk: None,
        mend: None,
        surge: 0.0,
        flow: crate::rainbow_kitty::spine::Flow::EMPTY,
    }
}

const fn ctx_at(now: Instant, cfg: &Config, caret: (u16, u16), caret_t: f32) -> Ctx<'_> {
    ctx_with(now, cfg, caret, caret_t, 0.5, 0.5)
}

const fn mv_as(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
    Event::Move {
        from,
        to,
        licence,
        dir: Dir::of(to.1 as i32 - from.1 as i32, to.0 as i32 - from.0 as i32),
    }
}

const fn mv(from: (u16, u16), to: (u16, u16)) -> Event {
    mv_as(from, to, Licence::Nav)
}

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn sky() -> Stardust {
    let mut dust = Stardust::new();
    for row in 3..=10 {
        dust.probe_mut().probe_row(row, &[false; 120]);
    }
    dust
}

fn sky_around(row: i32) -> Stardust {
    let mut dust = Stardust::new();
    for r in row - 1..=row + 1 {
        dust.probe_mut().probe_row(r, &[false; 120]);
    }
    dust
}

fn lane(dust: &Stardust, lane: StarLane) -> Vec<&Star> {
    dust.live_iter().filter(|s| s.lane == lane).collect()
}

const fn lum(c: u32) -> f32 {
    0.2126 * ((c >> 16) & 0xFF) as f32
        + 0.7152 * ((c >> 8) & 0xFF) as f32
        + 0.0722 * (c & 0xFF) as f32
}

const fn chan(c: u32) -> (u32, u32, u32) {
    ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
}

const fn is_white(c: u32) -> bool {
    let (r, g, b) = chan(c);
    r == g && g == b && r > 0
}

const fn is_chromatic(c: u32) -> bool {
    let (r, g, b) = chan(c);
    let m = if r > g { r } else { g };
    let m = if m > b { m } else { b };
    m > 0 && !(r == g && g == b)
}

fn stop_of(c: u32) -> usize {
    let (r, g, b) = chan(c);
    let hi = r.max(g).max(b).max(1) as f32;
    let n = |v: u32| v as f32 * 255.0 / hi;
    let (nr, ng, nb) = (n(r), n(g), n(b));
    let mut best = 0;
    let mut near = f32::INFINITY;
    for (i, &a) in SPECTRUM_ANCHORS.iter().enumerate() {
        let (ar, ag, ab) = chan(a);
        let d = (nr - ar as f32).powi(2) + (ng - ag as f32).powi(2) + (nb - ab as f32).powi(2);
        if d < near {
            near = d;
            best = i;
        }
    }
    best
}

/// The heads' nuclei, by shape not colour (on a dark page the nucleus
/// wears the train's hue at the head): each the light its corona's
/// petals ring, the centroid of the [`COMA_PETALS`] lights nearest it.
/// A petal's nearest lights lean to the ring's centre, off the petal.
fn nuclei(halos: &[RainHalo]) -> Vec<&RainHalo> {
    let mut lights: Vec<&RainHalo> = Vec::new();
    for h in halos {
        if !lights
            .iter()
            .any(|l| (l.cx, l.cy, l.color) == (h.cx, h.cy, h.color))
        {
            lights.push(h);
        }
    }
    let at = |h: &RainHalo| (f32::from(h.cx), f32::from(h.cy));
    lights
        .iter()
        .copied()
        .filter(|h| {
            let (x, y) = at(h);
            let mut near: Vec<(f32, f32)> = lights
                .iter()
                .map(|o| at(o))
                .filter(|&(ox, oy)| (ox, oy) != (x, y))
                .collect();
            if near.len() < COMA_PETALS {
                return false;
            }
            near.sort_by(|a, b| {
                (a.0 - x)
                    .hypot(a.1 - y)
                    .total_cmp(&(b.0 - x).hypot(b.1 - y))
            });
            let ring = &near[..COMA_PETALS];
            let n = COMA_PETALS as f32;
            let (mx, my) = ring
                .iter()
                .fold((0.0, 0.0), |(sx, sy), p| (sx + p.0 / n, sy + p.1 / n));
            (mx - x).hypot(my - y) <= 1.5
        })
        .collect()
}

/// The brightest light in a halo's colour: its largest channel.
const fn peak_of(c: u32) -> f32 {
    let (r, g, b) = chan(c);
    let rg = if r >= g { r } else { g };
    (if rg >= b { rg } else { b }) as f32
}

fn nucleus(halos: &[RainHalo]) -> Option<&RainHalo> {
    nuclei(halos)
        .into_iter()
        .max_by(|a, b| peak_of(a.color).total_cmp(&peak_of(b.color)))
}

fn nucleus_cx(halos: &[RainHalo]) -> Option<(f32, f32, f32)> {
    nucleus(halos).map(|h| (f32::from(h.cx), f32::from(h.cy), peak_of(h.color)))
}

#[test]
fn the_head_is_already_moving_and_lit_on_frame_zero() {
    let cfg = config();
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
    let spawn = m
        .on_event(&mv((5, 0), (5, 80)), t0, &ctx)
        .expect("an 80-cell Home/End must fly");
    assert_eq!(spawn.t0, t0, "§6.7: the spawn edge IS the observed frame");

    let mut sc = Scratch::default();
    sc.emit(&mut m, &ctx);
    assert!(
        !sc.under.is_empty(),
        "T2: the colour train must be lit on frame zero"
    );
    assert!(
        !sc.out.is_empty(),
        "T2: the white heat must be lit on frame zero"
    );

    let (cx, _, peak) = nucleus_cx(&sc.halos).expect("T2: the nucleus must exist at age 0");
    let (x0, _) = geom().cell_center(5, 0);
    let (x1, _) = geom().cell_center(5, 80);
    let l = x1 - x0;
    let want = x0 + FLIGHT_P0 * l;
    assert!(
        (cx - want).abs() <= 0.02 * l + NUCLEUS_LEAD_PX + 1.0,
        "the head is at {cx}, not at p0 = {want} (0.28 of the path)"
    );
    assert!(peak >= 100.0, "the nucleus is dim on frame zero: {peak}");

    let hot = sc
        .out
        .iter()
        .filter(|q| {
            let x = f32::from(q.x);
            (cx - SHOULDER_SHARE * l..=cx).contains(&x) && lum(q.color) >= 100.0
        })
        .count();
    assert!(
        hot > 0,
        "T2: no white ≥ 100 within 0.06·L behind the head on frame zero"
    );

    for cells in [8.0_f32, 40.0, 200.0] {
        let t = super::super::timing::flight_ms(cells);
        let d = enter_at_speed(8.0 / t) - enter_at_speed(0.0);
        assert!(
            d >= 0.10,
            "T = {t}: the head moved {d} of the path in 8 ms, not 0.10"
        );
    }
}

#[test]
fn the_impact_is_off_glass_by_t_plus_600() {
    let cfg = config();
    for cols in [8_u16, 34, 80] {
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, cols), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, cols)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        let mut t = t0;
        while t <= arrival + ms(400) {
            let at = ctx_at(t, &cfg, (5, cols), 0.25);
            sc.emit(&mut m, &at);
            t += ms(8);
        }
        let at = ctx_at(arrival + ms(400), &cfg, (5, cols), 0.25);
        sc.emit(&mut m, &at);
        let (_, ly) = geom().cell_center(5, cols);
        let small = |q: &GlowQuad| i32::from(q.w) <= SPARK_PX && i32::from(q.h) <= SPARK_PX;
        let sparks = sc
            .out
            .iter()
            .filter(|q| is_chromatic(q.color) && small(q))
            .count();
        assert!(
            sparks > 0 && !sc.under.is_empty(),
            "{cols} cells: the impact is already gone at T + 400 ({sparks} sparks, {} under)",
            sc.under.len()
        );
        assert!(
            sc.out
                .iter()
                .any(|q| is_chromatic(q.color) && small(q) && (f32::from(q.y) - ly).abs() >= 4.0),
            "{cols} cells: no spark has left the row by T + 400"
        );
        while t <= arrival + ms(600) {
            let at = ctx_at(t, &cfg, (5, cols), 0.25);
            sc.emit(&mut m, &at);
            t += ms(8);
        }
        for after in [600_u64, 650] {
            let at = ctx_at(arrival + ms(after), &cfg, (5, cols), 0.25);
            sc.emit(&mut m, &at);
            assert!(
                sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty(),
                "{cols} cells: meteor light survived to T + {after} ms"
            );
        }
        assert!(m.at_rest(), "{cols} cells: the pool never came to rest");
        assert!(
            m.next_change_deadline(arrival + ms(650)).is_none(),
            "{cols} cells: a resting pool still named a deadline (T6)"
        );
    }
}

#[test]
fn the_train_carries_the_whole_spectrum_on_every_frame_of_the_flight() {
    use crate::spectrum::{SPECTRUM_LUT, SPECTRUM_LUT_LEN};
    let cfg = config();
    let lut_index = |c: u32| -> usize {
        let (r, g, b) = chan(c);
        let hi = r.max(g).max(b).max(1) as f32;
        let n = |v: u32| v as f32 * 255.0 / hi;
        let (nr, ng, nb) = (n(r), n(g), n(b));
        let mut best = 0;
        let mut near = f32::INFINITY;
        for (i, &a) in SPECTRUM_LUT.iter().enumerate() {
            let (ar, ag, ab) = chan(a);
            let d = (nr - ar as f32).powi(2) + (ng - ag as f32).powi(2) + (nb - ab as f32).powi(2);
            if d < near {
                near = d;
                best = i;
            }
        }
        best
    };
    let (red_end, violet_end) = (
        (0.08 * (SPECTRUM_LUT_LEN - 1) as f32) as usize,
        (0.92 * (SPECTRUM_LUT_LEN - 1) as f32) as usize,
    );
    for t_land in [0.0_f32, 9.0 / 36.0, 0.6] {
        for cols in [8_u16, 40, 80] {
            for (from, to) in [((5_u16, 0_u16), (5_u16, cols)), ((5, cols), (5, 0))] {
                let t0 = Instant::now();
                let mut m = Meteors::new();
                let ctx = ctx_at(t0, &cfg, to, t_land);
                let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("fly");
                let mut sc = Scratch::default();
                let mut t = t0;
                let mut frame = 0;
                while t <= t0 + spawn.t_flight {
                    sc.emit(&mut m, &ctx_at(t, &cfg, to, t_land));
                    let mut seen = [false; 7];
                    let (mut lo, mut hi) = (usize::MAX, 0_usize);
                    for q in &sc.under {
                        let (r, g, b) = chan(q.color);
                        if r.max(g).max(b) >= 16 && is_chromatic(q.color) {
                            seen[stop_of(q.color)] = true;
                            let i = lut_index(q.color);
                            lo = lo.min(i);
                            hi = hi.max(i);
                        }
                    }
                    let n = seen.iter().filter(|s| **s).count();
                    let want = if frame == 0 { 4 } else { 5 };
                    assert!(
                        n >= want,
                        "t_land {t_land}, {from:?}->{to:?}, frame {frame}: the train showed \
                         {n} stops, not the rainbow: {seen:?}"
                    );
                    if frame > 0 {
                        assert!(
                            lo <= red_end && hi >= violet_end,
                            "t_land {t_land}, {from:?}->{to:?}, frame {frame}: the train \
                             spans table entries {lo}..{hi}, not red to violet"
                        );
                    }
                    t += ms(8);
                    frame += 1;
                }
                assert!(frame >= 8, "{cols} cells: only {frame} frames were checked");
            }
        }
    }
}

#[test]
fn a_vertical_recall_meteor_draws_its_train() {
    let cfg = config();
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (8, 40), 0.25);
    let spawn = m
        .on_event(&mv((5, 40), (8, 40)), t0, &ctx)
        .expect("a 3-row recall must fly");
    assert!(spawn.ring, "D8: a history recall rings");
    assert!(spawn.rain, "§12.3 / D6: a history recall rains on its hero");

    let mut sc = Scratch::default();
    sc.emit(&mut m, &ctx);
    assert!(
        !sc.under.is_empty(),
        "D15: the vertical train drew NOTHING — the x-major arm was taken"
    );

    let land = ctx_at(t0 + spawn.t_flight, &cfg, (8, 40), 0.25);
    sc.emit(&mut m, &land);
    for row in 5_u16..=8 {
        assert!(
            sc.under.iter().any(|q| q.row == row),
            "row {row} was crossed but carries no train quad"
        );
    }
    assert!(
        m.landings.iter().any(|l| l.burst.is_some()),
        "a recall's landing minted no ring"
    );
}

#[test]
fn a_flight_off_its_row_spares_the_line_it_left() {
    let cfg = config();
    let t0 = Instant::now();
    let white_on = |from: (u16, u16), to: (u16, u16), row: u16| -> f32 {
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, to, 0.25);
        m.on_event(&mv(from, to), t0, &ctx)
            .expect("a long leap flies");
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        let near = (f32::from(from.1) - 6.0) * ctx.geom.cw as f32;
        sc.out
            .iter()
            .filter(|q| q.row == row && f32::from(q.x) >= near)
            .map(|q| lum(q.color))
            .fold(0.0f32, f32::max)
    };
    let off = white_on((5, 60), (8, 2), 5);
    let along = white_on((5, 60), (5, 2), 5);
    assert!(
        along > 0.0,
        "control: the leap along the row lays its white heat there"
    );
    assert!(
        off * 5.0 <= along * 2.0,
        "the white heat over the line it left: {off:.1} against {along:.1} along it"
    );
}

#[test]
fn a_train_eases_in_at_both_ends() {
    let cfg = config();
    let t0 = Instant::now();
    let steps = |quads: &[GlowQuad], row: u16| -> (f32, f32) {
        let mut col = vec![0.0f32; 4096];
        for q in quads.iter().filter(|q| q.row == row) {
            for x in q.x..q.x + q.w {
                let v = lum(q.color).max(lum(q.color2));
                col[usize::from(x)] = col[usize::from(x)].max(v);
            }
        }
        let peak = col.iter().copied().fold(0.0, f32::max);
        let step = col
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        (step, peak)
    };
    let worst = |square: bool| -> (f32, f32) {
        let (mut white, mut colour) = (0.0f32, 0.0f32);
        for (from, to) in [((5u16, 40u16), (5u16, 2u16)), ((5, 2), (5, 40))] {
            let mut m = Meteors::new();
            m.ends_square = square;
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            m.on_event(&mv(from, to), t0, &ctx).expect("a leap flies");
            for at in [16u64, 40] {
                let mut sc = Scratch::default();
                sc.emit(&mut m, &ctx_at(t0 + ms(at), &cfg, to, 0.25));
                let (ws, wp) = steps(&sc.out, 5);
                let (cs, cp) = steps(&sc.under, 5);
                assert!(wp > 0.0 && cp > 0.0, "fixture: both layers fly at {at} ms");
                white = white.max(ws / wp);
                colour = colour.max(cs / cp);
            }
        }
        (white, colour)
    };
    let (white, colour) = worst(false);
    assert!(
        white <= 0.4 && colour <= 0.4,
        "a train steps by {white:.2} (white) and {colour:.2} (colour) of its peak in a column"
    );
    let (white_sq, colour_sq) = worst(true);
    assert!(
        white_sq.max(colour_sq) >= 0.6,
        "control: cut square, the trains step by only {white_sq:.2} and {colour_sq:.2}"
    );
}

#[test]
fn a_returns_white_heat_eases_off_the_line_it_printed() {
    let cfg = config();
    let t0 = Instant::now();
    let worst = |square: bool| -> f32 {
        let mut worst = 0.0f32;
        for at in [16u64, 35, 60, 90] {
            let (from, to) = ((5u16, 40u16), (7u16, 2u16));
            let mut m = Meteors::new();
            m.ends_square = square;
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                .expect("a Return flies");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(t0 + ms(at), &cfg, to, 0.25));
            let mut col = vec![0.0f32; 4096];
            for q in &sc.out {
                for x in q.x..q.x + q.w {
                    let v = lum(q.color).max(lum(q.color2));
                    col[usize::from(x)] = col[usize::from(x)].max(v);
                }
            }
            let peak = col.iter().copied().fold(0.0, f32::max);
            assert!(peak > 0.0, "fixture: the white heat flies at {at} ms");
            let lit: Vec<usize> = (0..col.len()).filter(|&x| col[x] > 0.0).collect();
            let (lo, hi) = (lit[0] + 24, lit[lit.len() - 1].saturating_sub(24));
            for x in lo..hi.max(lo) {
                let big = col[x].max(col[x + 1]);
                if big < 0.1 * peak {
                    continue;
                }
                worst = worst.max((col[x + 1] - col[x]).abs() / big);
            }
        }
        worst
    };
    let eased = worst(false);
    assert!(
        eased <= 0.25,
        "the white heat steps by {eased:.2} of its light in a column"
    );
    let hard = worst(true);
    assert!(
        hard >= 0.45,
        "control: switched hard, the white heat steps by only {hard:.2}"
    );
}

#[test]
fn a_dark_return_leaves_no_grey_or_brown_smudge() {
    // A dimmed white reads grey and a dimmed orange brown on a dark page,
    // so a dark Return's train light is its hue at a clean level or
    // nothing: few lit pixels read as a grey or brown smudge, and the
    // dimming trains (the negative control) leave many.
    let cfg = config();
    let page = cfg.theme_bg & 0x00FF_FFFF;
    let t0 = Instant::now();
    let smudge = |dims: bool| -> (usize, usize) {
        let (mut smudged, mut lit) = (0usize, 0usize);
        for at in [16u64, 35, 60, 90, 140, 200] {
            let (from, to) = ((5u16, 40u16), (7u16, 2u16));
            let mut m = Meteors::new();
            m.dims_dark_trains = dims;
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                .expect("a Return flies");
            let mut sc = Scratch::default();
            let ctx = ctx_at(t0 + ms(at), &cfg, to, 0.25);
            sc.emit(&mut m, &ctx);
            let (w, h) = (usize::from(ctx.geom.win_w), usize::from(ctx.geom.win_h));
            let mut px = vec![page; w * h];
            for q in sc.under.iter().chain(&sc.out) {
                let qw = u32::from(q.w.max(1));
                let step = |i: u32, a: u32, b: u32| {
                    (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw)
                };
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        let i = (x - usize::from(q.x)) as u32;
                        let (color, alpha) = if q.is_flat() {
                            (q.color, q.alpha)
                        } else {
                            let lerp = |sh: u32| {
                                step(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh
                            };
                            (
                                lerp(16) | lerp(8) | lerp(0),
                                step(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                            )
                        };
                        let slot = &mut px[y * w + x];
                        *slot = aterm_render::over_premul(*slot, color, alpha);
                    }
                }
            }
            // The head's lights, added as the renderer adds a halo.
            let add_sat = |a: u32, b: u32| {
                [16u32, 8, 0].iter().fold(0u32, |acc, &sh| {
                    acc | ((((a >> sh) & 0xff) + ((b >> sh) & 0xff)).min(255) << sh)
                })
            };
            for q in sc.halos.iter().filter(|q| q.mode == HaloMode::Add) {
                let (cx, cy) = (i32::from(q.cx), i32::from(q.cy));
                let rx2 = i32::from(q.rx) * i32::from(q.rx);
                let ry2 = i32::from(q.ry) * i32::from(q.ry);
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                    if ny >= 256 {
                        continue;
                    }
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2).min(255);
                        if wt > 0 {
                            let slot = &mut px[y * w + x];
                            *slot = add_sat(*slot, premul_rgb(q.color, wt as u8));
                        }
                    }
                }
            }
            let kind = |p: u32| {
                let (hue, sat, val) = crate::color_math::rgb2hsv(p);
                if p == page || val < 0.16 {
                    0u8
                } else if (sat < 0.12 && (0.25..0.62).contains(&val))
                    || ((8.0..48.0).contains(&hue) && sat > 0.3 && val < 0.45)
                {
                    2
                } else if val >= 0.45 {
                    1
                } else {
                    3
                }
            };
            let kinds: Vec<u8> = px.iter().map(|&p| kind(p)).collect();
            for y in 0..h {
                for x in 0..w {
                    match kinds[y * w + x] {
                        0 => continue,
                        2 => {}
                        _ => {
                            lit += 1;
                            continue;
                        }
                    }
                    lit += 1;
                    // A grey or brown pixel at a clean light's soft
                    // edge is its fringe; one with no clean light
                    // within three pixels is a smudge.
                    let near_clean = (y.saturating_sub(3)..(y + 4).min(h)).any(|yy| {
                        (x.saturating_sub(3)..(x + 4).min(w)).any(|xx| kinds[yy * w + xx] == 1)
                    });
                    if !near_clean {
                        smudged += 1;
                    }
                }
            }
        }
        (smudged, lit)
    };
    let (shipped, lit) = smudge(false);
    let (dimmed, dim_lit) = smudge(true);
    assert!(lit > 0 && dim_lit > 0, "fixture: the Return flies lit");
    assert!(
        shipped * 50 <= lit,
        "a dark Return smudges {shipped} of {lit} lit pixels grey or brown"
    );
    assert!(
        dimmed * 50 > dim_lit,
        "control: the dimming trains smudge only {dimmed} of {dim_lit}"
    );
}

/// On paper the pin draws nothing within a cell of the caret's block (the
/// landing's flash marks it); on dark (the control) its arms reach there.
#[test]
fn on_paper_the_pin_draws_nothing_by_the_caret_s_block() {
    let t0 = Instant::now();
    let pin = |dark: bool| {
        let mut cfg = config();
        cfg.dark_theme = dark;
        if !dark {
            cfg.theme_fg = 0x0024_292F;
            cfg.theme_bg = 0x00FF_FFFF;
        }
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");
        let mut sc = Scratch::default();
        sc.emit(
            &mut m,
            &ctx_at(t0 + spawn.t_flight + ms(4), &cfg, (5, 80), 0.25),
        );
        let l = *m.landings.first().expect("fixture: the meteor landed");
        sc.clear();
        {
            let at = ctx_at(t0 + spawn.t_flight + ms(20), &cfg, (5, 80), 0.25);
            let mut fr = sc.frame();
            draw_pin(&l, &at, &mut fr);
        }
        (l, sc.out.clone(), sc.halos.len())
    };
    let g = geom();
    let near = |l: &Landing, q: &GlowQuad| {
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let x0 = (l.x as i32 - i32::from(g.origin_x)).div_euclid(cw) * cw + i32::from(g.origin_x);
        let y0 = (l.y as i32 - i32::from(g.origin_y)).div_euclid(ch) * ch + i32::from(g.origin_y);
        let (qx, qy) = (i32::from(q.x), i32::from(q.y));
        let (qw, qh) = (i32::from(q.w), i32::from(q.h));
        qy < y0 + 2 * ch && qy + qh > y0 - ch && qx < x0 + 2 * cw && qx + qw > x0 - cw
    };
    let (l, paper, paper_halos) = pin(false);
    assert!(
        paper.iter().all(|q| !near(&l, q)) && paper_halos == 0,
        "a paper pin draws by the caret's block: {paper:?} and {paper_halos} halos"
    );
    let (l, dark, _) = pin(true);
    assert!(
        dark.iter().any(|q| near(&l, q)),
        "control: the dark pin's arms reach by the block"
    );
}

/// A train's ramps cut behind a lettered row are the quads their rows
/// stand for cut ([`Behind::cut_ramps`], lane B5): the same lights, and
/// a row the cut leaves lit but of no colour is the dark piece the
/// quad's cut lays, so the stream counts alike (the negative control
/// lays no dark piece).
#[test]
fn ramps_cut_behind_text_are_their_quads_cut() {
    let mut keep = KeepOut::default();
    keep.set_rows([(40, 200, 60, 80)]);
    let cut = |text: Behind<'_>, dark_pieces: bool| {
        // Two slabs down through the row's band: dim additive rows (the
        // eased edge cuts the dimmest to no colour) and source-over ones.
        let lead = GlowQuad::flat(2, 0, 50, 4, 4, 0x0010_1010, 0);
        let (mut legacy, mut quads) = (vec![lead], vec![lead]);
        let mut ramps = GlowRamps::default();
        let mark = ramps.mark();
        {
            let mut sink =
                RampSink::new(&mut quads, &mut ramps).costed(aterm_render::RampCost::ALWAYS);
            for (x, over) in [(60u16, false), (120, true)] {
                sink.slab();
                for y in 50u16..92 {
                    let band = i32::from(y / 20);
                    let c = u32::from(1 + y % 4) * 0x0001_0101;
                    let (a, b) = if over {
                        ((c * 8, 0x40), (c * 6, 0x30))
                    } else {
                        ((c, 0), (c * 2, 0))
                    };
                    sink.row(band, band as u16, (x, y, 24), a, b);
                    legacy.row(band, band as u16, (x, y, 24), a, b);
                }
            }
        }
        text.cut_quads(&mut legacy, 1);
        if dark_pieces {
            text.cut_ramps((&mut quads, &mut ramps), mark);
        } else {
            let mut none = Vec::new();
            text.cut_ramps((&mut none, &mut ramps), mark);
        }
        let mut got = Vec::new();
        ramps.expand_into(&quads, &mut got);
        let key = |q: &GlowQuad| (q.y, q.x, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2);
        got.sort_by_key(key);
        legacy.sort_by_key(key);
        let dark = legacy
            .iter()
            .filter(|q| q.color | q.color2 == 0 && q.alpha | q.alpha2 == 0)
            .count();
        (got == legacy, ramps.laid(quads.len()) == legacy.len(), dark)
    };
    for text in [
        Behind::bodies(&keep, true),
        Behind::bodies(&keep, true).guarded(),
    ] {
        let (same, counted, dark) = cut(text, true);
        assert!(same && counted, "the cut ramps are the cut quads");
        assert!(dark > 0, "premise: the cut leaves a dark piece");
        assert!(
            !cut(text, false).1,
            "control: without its dark pieces the stream counts short"
        );
    }
}

/// The head's own lights at `at`, alone: each live meteor's head drawn
/// as its emit draws it, `clean` and folding unless told otherwise.
fn head_lights(
    m: &Meteors,
    at: &Ctx<'_>,
    (clean, fold): (bool, bool),
    keep: &KeepOut,
) -> Vec<RainHalo> {
    let mut sc = Scratch::default();
    {
        let mut fr = sc.frame();
        for me in &m.live {
            if let Some(f) = Flight::of(me, at) {
                draw_head(
                    me,
                    &f,
                    at,
                    &mut fr,
                    (clean, fold, true),
                    Behind::bodies(keep, true),
                );
            }
        }
    }
    sc.halos
}

/// Past [`HEAD_FOLD_MS`] after a Return's arrival the head lays no light;
/// kept standing (the negative control), it does.
#[test]
fn the_head_folds_into_its_landing() {
    let cfg = config();
    let t0 = Instant::now();
    let keep = KeepOut::default();
    let lit = |stay: bool, after: u64| -> usize {
        let (from, to) = ((5u16, 40u16), (7u16, 2u16));
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, to, 0.25);
        let spawn = m
            .on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
            .expect("a Return flies");
        let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, to, 0.25);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &at);
        head_lights(&m, &at, (true, !stay), &keep).len()
    };
    for after in [HEAD_FOLD_MS as u64 + 4, 70, 120] {
        assert_eq!(
            lit(false, after),
            0,
            "{after} ms after the arrival the head still lays its lights"
        );
    }
    assert!(
        lit(true, HEAD_FOLD_MS as u64 + 4) > 0,
        "control: kept standing, the head is already gone"
    );
}

/// Across a lettered row the trains pass behind the letters' bodies: no
/// train quad stands inside the row's letter band; laid over the text
/// (the negative control), one does.
#[test]
fn a_train_passes_behind_a_lettered_row() {
    let cfg = config();
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let (ox, oy) = (i32::from(g.origin_x), i32::from(g.origin_y));
    let row = (ox + 2 * cw, ox + 40 * cw, oy + 6 * ch, oy + 7 * ch);
    let band = (
        row.2 as f32 + TRAIN_TEXT_BAND.0 * ch as f32 + TRAIN_TEXT_FEATHER_PX,
        row.2 as f32 + TRAIN_TEXT_BAND.1 * ch as f32 - TRAIN_TEXT_FEATHER_PX,
    );
    let inside = |x: f32, y: f32| {
        x > row.0 as f32 + TRAIN_TEXT_FEATHER_PX
            && x < row.1 as f32 - TRAIN_TEXT_FEATHER_PX
            && (band.0..=band.1).contains(&y)
    };
    let t0 = Instant::now();
    let struck = |over: bool| -> usize {
        let mut n = 0;
        for at in [16u64, 35, 60, 90, 140] {
            let (from, to) = ((5u16, 40u16), (7u16, 2u16));
            let mut m = Meteors::new();
            m.trains_over_text = over;
            m.keep_out.set_rows([row]);
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                .expect("a Return flies");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(t0 + ms(at), &cfg, to, 0.25));
            n += sc
                .under
                .iter()
                .chain(&sc.out)
                .filter(|q| {
                    // Premultiplied: a dark train's light is its colour.
                    (q.color | q.color2) & 0x00FF_FFFF != 0
                        && inside(
                            f32::from(q.x) + 0.5 * f32::from(q.w),
                            f32::from(q.y) + 0.5 * f32::from(q.h),
                        )
                })
                .count();
        }
        n
    };
    let behind = struck(false);
    assert_eq!(
        behind, 0,
        "{behind} train lights stand in the letters' band"
    );
    assert!(
        struck(true) > 0,
        "control: laid over the text, the thread never crosses the row"
    );
}

/// On a dark page at most one lit pixel in fifty of the head reads grey or
/// brown, even at its lights' soft edges; the white head with the warm
/// petals (the negative control) reads so far more.
#[test]
fn a_dark_head_is_never_grey_or_brown() {
    let cfg = config();
    let page = cfg.theme_bg & 0x00FF_FFFF;
    let t0 = Instant::now();
    let keep = KeepOut::default();
    let muddy = |clean: bool| -> (usize, usize) {
        let (mut bad, mut lit) = (0usize, 0usize);
        for at in [8u64, 16, 30, 45, 60] {
            let (from, to) = ((5u16, 40u16), (7u16, 2u16));
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                .expect("a Return flies");
            let at = ctx_at(t0 + ms(at), &cfg, to, 0.25);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &at);
            let halos = head_lights(&m, &at, (clean, true), &keep);
            let (w, h) = (usize::from(at.geom.win_w), usize::from(at.geom.win_h));
            let mut px = vec![page; w * h];
            for q in halos.iter().filter(|q| q.mode == HaloMode::Add) {
                let (cx, cy) = (i32::from(q.cx), i32::from(q.cy));
                let rx2 = i32::from(q.rx) * i32::from(q.rx);
                let ry2 = i32::from(q.ry) * i32::from(q.ry);
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                    if ny >= 256 {
                        continue;
                    }
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2).min(255);
                        if wt > 0 {
                            let slot = &mut px[y * w + x];
                            *slot = aterm_render::add_sat(*slot, premul_rgb(q.color, wt as u8));
                        }
                    }
                }
            }
            for &p in &px {
                let (hue, sat, val) = crate::color_math::rgb2hsv(p);
                if p == page || val < 0.16 {
                    continue;
                }
                lit += 1;
                if (sat < 0.12 && (0.25..0.62).contains(&val))
                    || ((8.0..48.0).contains(&hue) && sat > 0.3 && val < 0.45)
                {
                    bad += 1;
                }
            }
        }
        (bad, lit)
    };
    let (bad, lit) = muddy(true);
    assert!(lit > 0, "fixture: the Return's head is lit");
    assert!(
        bad * 50 <= lit,
        "{bad} of the head's {lit} lit pixels read grey or brown"
    );
    let (old_bad, old_lit) = muddy(false);
    assert!(
        old_bad * 50 > old_lit,
        "control: the white head reads grey or brown at only {old_bad} of {old_lit}"
    );
}

/// On paper the meteor draws no head (the train's own eased end is the
/// head); on dark (the control) it does.
#[test]
fn on_paper_the_meteor_draws_no_head() {
    let t0 = Instant::now();
    let keep = KeepOut::default();
    let heads = |dark: bool| -> usize {
        let mut cfg = config();
        cfg.dark_theme = dark;
        if !dark {
            cfg.theme_fg = 0x0024_292F;
            cfg.theme_bg = 0x00FF_FFFF;
        }
        let mut n = 0;
        for at in [0u64, 16, 35, 60] {
            let (from, to) = ((5u16, 40u16), (7u16, 2u16));
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            m.on_event(&mv_as(from, to, Licence::Return), t0, &ctx)
                .expect("a Return flies");
            let at = ctx_at(t0 + ms(at), &cfg, to, 0.25);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &at);
            n += head_lights(&m, &at, (true, true), &keep).len();
        }
        n
    };
    assert_eq!(heads(false), 0, "the paper meteor drew a head");
    assert!(heads(true) > 0, "control: the dark meteor drew no head");
}

/// On paper a landing's sparks stand a cell clear of the caret's block on
/// every side; on dark (the control) they fly from the block itself.
#[test]
fn paper_sparks_stand_a_cell_clear_of_the_caret() {
    let t0 = Instant::now();
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let near = |dark: bool| -> usize {
        let mut cfg = config();
        cfg.dark_theme = dark;
        if !dark {
            cfg.theme_fg = 0x0024_292F;
            cfg.theme_bg = 0x00FF_FFFF;
        }
        let to = (5u16, 40u16);
        let x0 = i32::from(g.origin_x) + i32::from(to.1) * cw;
        let y0 = i32::from(g.origin_y) + i32::from(to.0) * ch;
        let boxed = |x: i32, y: i32, w: i32, h: i32| {
            x < x0 + 2 * cw && x + w > x0 - cw && y < y0 + 2 * ch && y + h > y0 - ch
        };
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, to, 0.25);
        let spawn = m.on_event(&mv((5, 0), to), t0, &ctx).expect("fly");
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(t0 + spawn.t_flight + ms(2), &cfg, to, 0.25));
        let l = *m.landings.first().expect("fixture: the meteor landed");
        let mut n = 0;
        for after in [4u64, 12, 24, 40, 60, 90, 120, 160] {
            sc.clear();
            let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, to, 0.25);
            {
                let mut fr = sc.frame();
                draw_sparks(&l, &at, &mut fr, usize::MAX);
            }
            n += sc
                .out
                .iter()
                .filter(|q| {
                    boxed(
                        i32::from(q.x),
                        i32::from(q.y),
                        i32::from(q.w),
                        i32::from(q.h),
                    )
                })
                .count();
        }
        n
    };
    let paper = near(false);
    assert_eq!(
        paper, 0,
        "{paper} spark quads within a cell of the paper caret"
    );
    assert!(
        near(true) > 0,
        "control: the dark sparks never fly by the block"
    );
}

#[test]
fn a_dark_coma_is_never_olive() {
    let olive = |rgb: u32| {
        let (h, s, _) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
        s > 0.2 && (40.0..=100.0).contains(&h)
    };
    assert!(
        (0..COMA_PETALS).any(|i| olive(spectrum_stop(i))),
        "control: the spectrum carries a yellow stop"
    );
    let cfg = config();
    assert!(cfg.dark_theme, "fixture: the page is dark");
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (8, 2), 0.25);
    m.on_event(&mv((5, 60), (8, 2)), t0, &ctx)
        .expect("a Return flies");
    let mut sc = Scratch::default();
    sc.emit(&mut m, &ctx_at(t0 + ms(40), &cfg, (8, 2), 0.25));
    let petals: Vec<u32> = sc.halos.iter().map(|h| h.color).collect();
    assert!(
        petals.len() >= COMA_PETALS,
        "fixture: the coma is drawn ({})",
        petals.len()
    );
    for c in petals {
        assert!(!olive(c), "a dark coma petal is olive: #{c:06x}");
    }
}

#[test]
fn a_second_meteor_retires_the_first_without_a_pop() {
    let cfg = config();
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let a = ctx_at(t0, &cfg, (5, 80), 0.25);
    m.on_event(&mv((5, 0), (5, 80)), t0, &a).expect("E flies");
    let before = m.live[0].colour_alpha(t0 + ms(30));

    let b = ctx_at(t0 + ms(30), &cfg, (5, 0), 0.25);
    m.on_event(&mv((5, 80), (5, 0)), t0 + ms(30), &b)
        .expect("A flies");
    let first = m.live[0];
    assert!(
        first.retiring(),
        "the crossing meteor did not retire the first"
    );
    assert!(
        (first.colour_alpha(t0 + ms(30)) - before).abs() < 1e-3,
        "the retire POPPED: alpha jumped at the edge"
    );

    let mut last = f32::INFINITY;
    for at in [30_u64, 45, 60, 75, 90] {
        let v = first.colour_alpha(t0 + ms(at));
        assert!(v <= last + 1e-4, "the retired train brightened at +{at} ms");
        last = v;
    }
    assert!(
        first.colour_alpha(t0 + ms(90)) <= CHROMA_CULL_ALPHA,
        "the retired train is still above the cull at +90 ms"
    );

    let cap = m.live.capacity();
    for i in 0..10_u64 {
        let now = t0 + ms(100 + i * 5);
        let (from, to) = if i % 2 == 0 {
            ((5_u16, 0_u16), (5_u16, 80_u16))
        } else {
            ((5, 80), (5, 0))
        };
        let c = ctx_at(now, &cfg, to, 0.25);
        let _ = m.on_event(&mv(from, to), now, &c);
        let flying = m.live.iter().filter(|x| !x.retiring()).count();
        assert!(
            flying <= FLIGHT_MAX_LIVE,
            "{flying} heads in the air at once"
        );
        assert_eq!(m.live(), flying, "`live()` counts finishing trains");
        assert!(
            m.live.len() <= METEOR_POOL,
            "the pool grew to {} under a mash",
            m.live.len()
        );
    }
    assert_eq!(
        m.live.capacity(),
        cap,
        "the meteor pool reallocated under a mash"
    );
}

#[test]
fn a_retired_head_sheds_nothing_more() {
    let cfg = config();
    let t0 = Instant::now();
    let run = |retire: bool| -> (u8, u8) {
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("E flies");
        let mut sc = Scratch::default();
        let edge = t0 + ms(8);
        sc.emit(&mut m, &ctx_at(edge, &cfg, (5, 80), 0.25));
        let early = m.live[0].shed;
        if retire {
            let b = ctx_at(edge, &cfg, (5, 70), 0.25);
            m.on_event(&mv((5, 80), (5, 70)), edge, &b)
                .expect("the crossing flies");
            assert!(m.live[0].retiring());
        }
        for after in [12_u64, 30, 45, 59] {
            sc.emit(&mut m, &ctx_at(edge + ms(after), &cfg, (5, 70), 0.25));
        }
        (early, m.live[0].shed)
    };
    let all = (1_u8 << shed_n(80.0)) - 1;
    let (early, late) = run(false);
    assert_ne!(
        early, all,
        "every station was already born at +8 ms — nothing to test"
    );
    assert_eq!(late, all, "a live head passes every station by +67 ms");
    let (early_r, late_r) = run(true);
    assert_eq!(early_r, early, "the two runs diverged before the retire");
    assert_eq!(
        late_r, early_r,
        "a retired head shed {:#b} → {:#b}",
        early_r, late_r
    );
}

#[test]
fn a_chained_continuation_never_puts_the_head_behind_where_it_was() {
    let cfg = config();
    let t0 = Instant::now();
    let (x0, _) = geom().cell_center(5, 0);
    let (x40, _) = geom().cell_center(5, 40);

    let mut m = Meteors::new();
    let a = ctx_at(t0, &cfg, (5, 40), 0.25);
    let s1 = m.on_event(&mv((5, 0), (5, 40)), t0, &a).expect("flies");
    let at = t0 + s1.t_flight;
    let b = ctx_at(at, &cfg, (5, 80), 0.25);
    m.on_event(&mv((5, 40), (5, 80)), at, &b)
        .expect("the continuation flies");
    assert_eq!(m.live.len(), 2);
    assert!(m.live[0].chained && m.live[0].retired_at.is_none());
    assert!(!m.live[1].chained);
    let mut sc = Scratch::default();
    sc.emit(&mut m, &b);
    let (cx, _, _) = nucleus_cx(&sc.halos).expect("one head");
    assert!(
        cx >= x40 - 1.0,
        "the chained head appeared at {cx}, behind the old head at {x40}"
    );
    let heads: std::collections::BTreeSet<u16> = nuclei(&sc.halos).iter().map(|h| h.cx).collect();
    assert_eq!(
        heads.len(),
        1,
        "two heads in the air after a chain: {heads:?}"
    );
    assert!(
        m.landings.iter().all(|l| (l.x - x40).abs() > 1.0),
        "the handed-over head celebrated a landing the caret left"
    );

    let mut m = Meteors::new();
    let a = ctx_at(t0, &cfg, (5, 40), 0.25);
    let s1 = m.on_event(&mv((5, 0), (5, 40)), t0, &a).expect("flies");
    let at = t0 + ms(30);
    let t_ms = s1.t_flight.as_secs_f32() * 1000.0;
    let hx = x0 + enter_at_speed(30.0 / t_ms) * (x40 - x0);
    let from = (5_u16, ((hx - x0) / geom().cw as f32).round() as u16);
    let b = ctx_at(at, &cfg, (5, 70), 0.25);
    m.on_event(&mv(from, (5, 70)), at, &b)
        .expect("the continuation flies");
    let old = m.live[0];
    assert!(
        old.chained,
        "a continuation within a cell of the head must chain"
    );
    assert!(
        (old.x1 - hx).abs() <= 1.0,
        "the old train was not cut at its head"
    );
    assert_eq!(
        old.t_flight,
        ms(30),
        "the old arrival edge is the chain edge"
    );
    sc.emit(&mut m, &b);
    let (cx, _, _) = nucleus_cx(&sc.halos).expect("one head");
    assert!(
        cx >= hx - 1.0,
        "the head jumped backwards on the chain frame"
    );
}

#[test]
fn a_chained_stub_never_costs_another_row_its_head() {
    let cfg = config();
    let t0 = Instant::now();
    let (x0, _) = geom().cell_center(5, 0);
    let (x40, _) = geom().cell_center(5, 40);
    let mut m = Meteors::new();
    let b = ctx_at(t0, &cfg, (7, 40), 0.25);
    m.on_event(&mv((7, 0), (7, 40)), t0, &b).expect("B flies");
    let a_at = t0 + ms(5);
    let a = ctx_at(a_at, &cfg, (5, 40), 0.25);
    let s_a = m.on_event(&mv((5, 0), (5, 40)), a_at, &a).expect("A flies");
    assert_eq!(m.live(), FLIGHT_MAX_LIVE);
    let at = a_at + ms(30);
    let t_ms = s_a.t_flight.as_secs_f32() * 1000.0;
    let hx = x0 + enter_at_speed(30.0 / t_ms) * (x40 - x0);
    let from = (5_u16, ((hx - x0) / geom().cw as f32).round() as u16);
    let c = ctx_at(at, &cfg, (5, 70), 0.25);
    m.on_event(&mv(from, (5, 70)), at, &c).expect("A′ flies");
    let (old_b, old_a, new_a) = (m.live[0], m.live[1], m.live[2]);
    assert!(old_a.chained, "A′ did not chain A");
    assert!(
        old_a.retiring(),
        "the chained stub is the third train on glass and takes the 60 ms finish"
    );
    assert!(
        !old_b.retiring(),
        "B — a head in flight on another row — was evicted by A's own continuation"
    );
    assert!(!new_a.retiring(), "the continuation itself was retired");
    assert_eq!(
        m.live(),
        FLIGHT_MAX_LIVE,
        "two live meteors after the chain"
    );
    let mut sc = Scratch::default();
    sc.emit(&mut m, &c);
    let (_, y7) = geom().cell_center(7, 40);
    assert!(
        nuclei(&sc.halos)
            .iter()
            .any(|h| (f32::from(h.cy) - y7).abs() <= 1.0),
        "B's nucleus is gone from row 7"
    );
}

#[test]
fn only_one_head_is_in_the_air_per_row() {
    let cfg = config();
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let a = ctx_at(t0, &cfg, (5, 10), 0.25);
    m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
    let b = ctx_at(t0 + ms(10), &cfg, (5, 70), 0.25);
    m.on_event(&mv((5, 60), (5, 70)), t0 + ms(10), &b)
        .expect("flies");
    assert!(m.live[0].retiring(), "two heads in the air on one row");
    assert_eq!(m.live(), 1);

    let mut m = Meteors::new();
    m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
    let c = ctx_at(t0 + ms(10), &cfg, (7, 70), 0.25);
    m.on_event(&mv((7, 60), (7, 70)), t0 + ms(10), &c)
        .expect("flies");
    assert_eq!(m.live(), 2, "flights on different rows retired each other");

    let mut m = Meteors::new();
    let s = m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
    let later = t0 + s.t_flight + ms(10);
    let d = ctx_at(later, &cfg, (5, 70), 0.25);
    m.on_event(&mv((5, 60), (5, 70)), later, &d).expect("flies");
    assert!(
        !m.live[0].retiring(),
        "a landed train was retired by a distant newcomer"
    );
}

#[test]
fn a_ping_pong_never_spends_more_than_its_under_share() {
    let cfg = config();
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let a = ctx_at(t0, &cfg, (5, 119), 1.0);
    m.on_event(&mv((5, 0), (5, 119)), t0, &a).expect("E flies");
    let b = ctx_at(t0 + ms(20), &cfg, (5, 0), 1.0);
    m.on_event(&mv((5, 119), (5, 0)), t0 + ms(20), &b)
        .expect("A flies");

    let mut sc = Scratch::default();
    let mut t = t0 + ms(20);
    while t <= t0 + ms(200) {
        let at = ctx_at(t, &cfg, (5, 0), 1.0);
        sc.emit(&mut m, &at);
        let share = FLIGHT_MAX_LIVE * UNDER_QUAD_CAP;
        assert!(
            sc.under.len() <= share,
            "two meteors spent {} under quads — over their {share} share",
            sc.under.len()
        );
        t += ms(8);
    }
}

#[test]
fn reduced_motion_draws_the_static_form() {
    let mut cfg = config();
    cfg.reduced_motion = true;
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
    let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");

    let mut sc = Scratch::default();
    sc.emit(&mut m, &ctx);
    assert!(!sc.under.is_empty(), "the static train must still be drawn");
    let (cx, _, _) = nucleus_cx(&sc.halos).expect("the static head must be drawn");
    let (x1, ly) = geom().cell_center(5, 80);
    assert!(
        (cx - x1).abs() <= NUCLEUS_LEAD_PX + 2.0,
        "reduced motion put the head at {cx}, not at the landing {x1} — it flew"
    );

    let land = ctx_at(t0 + spawn.t_flight + ms(4), &cfg, (5, 80), 0.25);
    sc.emit(&mut m, &land);
    assert_eq!(
        m.landings.len(),
        1,
        "reduced motion did not mint the static landing (the flash and the colours)"
    );
    let mut dust = sky_around(5);
    m.sow_into(&mut dust, &land);
    let l = m.landings[0];
    let snapshot = |sc: &mut Scratch, after: u64| {
        sc.clear();
        let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, (5, 80), 0.25);
        let mut fr = sc.frame();
        draw_burst(&l, &at, &mut fr);
        draw_flash(&l, &at, &mut fr);
        draw_sparks(&l, &at, &mut fr, SPARK_HALO_CAP);
        (sc.under.clone(), sc.out.clone(), sc.halos.clone())
    };
    let a = snapshot(&mut sc, 100);
    let b = snapshot(&mut sc, 300);
    assert!(!a.1.is_empty(), "the static landing drew nothing");
    assert!(
        a.0 == b.0 && a.1 == b.1 && a.2 == b.2,
        "the reduced-motion landing MOVED between T + 100 and T + 300"
    );
    assert!(
        a.2.iter().all(|h| !is_chromatic(h.color)),
        "a spark's halo (motion) was drawn under reduced motion"
    );
    sc.clear();
    {
        let at = ctx_at(t0 + spawn.t_flight + ms(100), &cfg, (5, 80), 0.25);
        let mut fr = sc.frame();
        draw_pin(&l, &at, &mut fr);
    }
    assert!(
        sc.out.is_empty() && sc.halos.is_empty(),
        "the pin (motion) was drawn under reduced motion"
    );
    let g = geom();
    let cell = |q: &GlowQuad| usize::from(q.w) == g.cw && usize::from(q.h) == g.ch;
    assert!(
        a.1.iter().any(|q| cell(q) && is_white(q.color))
            && a.1.iter().any(|q| cell(q) && is_chromatic(q.color)),
        "the static flash is not white and colour"
    );
    let ch = g.ch as f32;
    assert!(
        a.1.iter()
            .any(|q| f32::from(q.y) + f32::from(q.h) <= ly - 0.5 * ch)
            && a.1.iter().any(|q| f32::from(q.y) >= ly + 0.5 * ch),
        "the static splash is not in the sky above and below the row"
    );
    let fan = lane(&dust, StarLane::Fan);
    assert!(!fan.is_empty(), "reduced motion dropped the (static) fan");
    let later = land.now + ms(400);
    for s in &fan {
        let birth = (s.x.round() as i32, s.y.round() as i32);
        assert_eq!(
            s.pos(later, land.cfg.reduced_motion),
            birth,
            "the reduced-motion fan is thrown, not static"
        );
    }
    assert!(
        fan.iter()
            .any(|s| s.pos(later, false) != (s.x.round() as i32, s.y.round() as i32)),
        "the oracle is vacuous: the same stars would not move under motion either"
    );
}

#[test]
fn a_word_hop_is_a_mini_fan_not_a_flight() {
    let cfg = config();
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 25), 0.25);
    assert!(
        m.on_event(&mv((5, 20), (5, 25)), t0, &ctx).is_none(),
        "a 5-cell hop must not mint a flight"
    );
    assert_eq!(m.live(), 0, "a 5-cell hop put a meteor in the air");

    let mut sc = Scratch::default();
    sc.emit(&mut m, &ctx);
    assert!(sc.under.is_empty(), "a mini-fan drew a train");
    assert!(m.landings.is_empty(), "a mini-fan rang");

    let mut dust = sky();
    m.sow_into(&mut dust, &ctx);
    let stars = lane(&dust, StarLane::MiniFan);
    let m2 = stars.iter().filter(|s| s.class == StarClass::M2).count();
    let m3 = stars.iter().filter(|s| s.class == StarClass::M3).count();
    assert_eq!((m2, m3), (1, 2), "the mini-fan is 1 m2 + 2 m3 (D17)");
    assert_eq!(dust.live(), 3, "a mini-fan star escaped its lane");
    let (lx, ly) = geom().cell_center(5, 25);
    for s in &stars {
        assert!(
            (s.x - lx).abs() <= 2.0 && (s.y - ly).abs() <= 2.0,
            "a mini-fan star was born away from the landing cell"
        );
    }

    let mut dust = sky();
    assert!(m.on_event(&mv((5, 25), (5, 26)), t0, &ctx).is_none());
    sc.emit(&mut m, &ctx);
    m.sow_into(&mut dust, &ctx);
    assert_eq!(dust.live(), 0, "a one-cell scrub minted light");
}

#[test]
fn a_row_zero_fan_may_use_the_head_band_but_never_the_tab_strip_beyond_it() {
    let cfg = config();
    let t0 = Instant::now();
    let landing = (0_u16, 40_u16);
    for head in [40_u16, 0] {
        let at = |now: Instant| {
            let mut c = ctx_at(now, &cfg, landing, 0.25);
            c.geom.head = head;
            c.geom.origin_y = head;
            c
        };
        let mut m = Meteors::new();
        let spawn = m.on_event(&mv((0, 0), landing), t0, &at(t0)).expect("fly");
        let land = at(t0 + spawn.t_flight);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &land);
        let mut dust = sky();
        m.sow_into(&mut dust, &land);
        let fan = lane(&dust, StarLane::Fan);
        assert_eq!(
            fan.iter()
                .filter(|s| s.class == StarClass::M1 && s.gold)
                .count(),
            1,
            "head {head}: the gold hero"
        );
        assert_eq!(
            fan.iter().filter(|s| s.class == StarClass::M2).count(),
            4,
            "head {head}: its four m2"
        );
        let g = land.geom;
        let (top, grid_top) = (g.fx_top(), i32::from(g.origin_y));
        let cap = (FAN_RISE_MAX_CH * g.ch as f32).round() as i32;
        let (_, cy) = g.cell_center(0, 40);
        let cy = cy.round() as i32;
        let mut in_band = 0;
        for s in &fan {
            let (rx, ry) = s.rest();
            assert!(
                ry >= top,
                "head {head}: a {:?} fan star rests at ({rx}, {ry}), above the effects \
                 box's top {top} — in the tab strip",
                s.class
            );
            let rise = (ry - cy).abs();
            assert!(
                rise <= cap,
                "head {head}: a {:?} fan star rests {rise} px off row 0's line at \
                 ({rx}, {ry}) — the cap is {cap} px (1.6 ch) on row 0 too",
                s.class
            );
            in_band += usize::from(ry < grid_top);
        }
        if head > 0 {
            assert!(in_band >= 1, "head {head}: no fan star used the head band");
        } else {
            assert_eq!(in_band, 0, "head 0: a fan star rests above the grid");
        }
    }
}

#[test]
fn a_fan_star_core_never_lands_on_a_probed_glyph() {
    let cfg = config();
    let t0 = Instant::now();
    let landing = (5_u16, 40_u16);
    let g = geom();
    let window = StarLane::Fan.throw_ms().expect("the fan is a throw");
    let census = |dust: &mut Stardust| -> usize {
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, landing, 0.25);
        let spawn = m.on_event(&mv((5, 0), landing), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let land = ctx_at(arrival, &cfg, landing, 0.25);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &land);
        m.sow_into(dust, &land);
        let done = arrival + Duration::from_secs_f32(window / 1000.0) + ms(1);
        for s in lane(dust, StarLane::Fan) {
            let (rx, ry) = s.pos(done, false);
            assert_ne!(
                dust.probe().at_px(rx, ry, g),
                Some(true),
                "a {:?} fan core came to rest on a probed glyph at ({rx}, {ry})",
                s.class
            );
        }
        assert!(!m.landings.is_empty(), "the pin and ring need no clearance");
        lane(dust, StarLane::Fan).len()
    };
    let probed = |ink: &[bool; 120]| {
        let mut dust = Stardust::new();
        dust.probe_mut().probe_row(i32::from(landing.0), ink);
        dust
    };

    let mut inked = [false; 120];
    inked[usize::from(landing.1)] = true;
    let on_ink = census(&mut probed(&inked));
    assert!(
        on_ink >= FAN_HERO_N,
        "a glyph AT the landing cut the fan to {on_ink} stars — the hero and its four m2 \
         are gone, and the five rain glints have nothing to pair with (D6)"
    );

    let mut beside = [false; 120];
    beside[usize::from(landing.1) + 1] = true;
    let full = census(&mut probed(&beside));
    assert!(
        full >= FAN_HERO_N,
        "a glyph BESIDE the landing thinned the fan"
    );
    assert_eq!(on_ink, full, "ink under the caret thinned the fan");

    let mut row = [true; 120];
    row[usize::from(landing.1)] = false;
    let boxed = census(&mut probed(&row));
    assert!(
        boxed >= 1 && boxed <= full,
        "a row of ink around the landing left {boxed} of {full} stars"
    );

    let mut dust = Stardust::new();
    assert!(dust.probe().is_empty());
    assert_eq!(
        census(&mut dust),
        full,
        "an unprobed row refused a free transient"
    );

    let mut dust = probed(&inked);
    let mut m = Meteors::new();
    let hop = ctx_at(t0, &cfg, landing, 0.25);
    assert!(m.on_event(&mv((5, 45), landing), t0, &hop).is_none());
    let mut sc = Scratch::default();
    sc.emit(&mut m, &hop);
    m.sow_into(&mut dust, &hop);
    assert_eq!(
        lane(&dust, StarLane::MiniFan).len(),
        3,
        "Alt-B onto a word's first glyph minted no mini-fan (§6.12)"
    );
}

#[test]
fn a_band_move_carries_a_flight_inside_the_band_and_retires_one_that_straddles_it() {
    let cfg = config();
    let t0 = Instant::now();
    let g = geom();
    let ch = g.ch as u16;

    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
    let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
    let (_, y5) = g.cell_center(5, 40);
    assert!(
        (m.live[0].y1 - y5).abs() < 0.5,
        "fixture: the head is on row 5"
    );
    m.translate_band(3, 10, 1, ch, 0);
    assert_eq!(m.live.len(), 1, "a train wholly inside the band is kept");
    let (wx, wy) = g.cell_center(6, 40);
    assert!(
        (m.live[0].y0 - wy).abs() < 0.5 && (m.live[0].y1 - wy).abs() < 0.5,
        "the train rode one row down with its text: y0 {} y1 {} want {wy}",
        m.live[0].y0,
        m.live[0].y1
    );
    let mut sc = Scratch::default();
    let land = ctx_at(t0 + spawn.t_flight, &cfg, (6, 40), 0.25);
    sc.emit(&mut m, &land);
    let l = m
        .landings
        .first()
        .copied()
        .expect("the landing is minted at T");
    assert!(
        (l.x - wx).abs() < 0.5 && (l.y - wy).abs() < 0.5,
        "the landing was minted at ({}, {}), not at the moved endpoint ({wx}, {wy})",
        l.x,
        l.y
    );
    m.translate_band(6, 10, -1, ch, 0);
    assert!(
        m.landings.is_empty(),
        "a landing carried past the band's edge is gone, not parked on it"
    );

    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
    m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
    let (y0, y1) = (m.live[0].y0, m.live[0].y1);
    m.translate_band(8, 20, 1, ch, 0);
    assert_eq!(m.live.len(), 1);
    assert_eq!(
        (m.live[0].y0, m.live[0].y1),
        (y0, y1),
        "a band below the flight does not touch it"
    );

    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (3, 40), 0.25);
    let spawn = m.on_event(&mv((9, 40), (3, 40)), t0, &ctx).expect("flies");
    assert_eq!(m.live.len(), 1);
    m.translate_band(5, 20, 1, ch, 0);
    assert!(
        m.live.is_empty(),
        "a train straddling the band's edge is dropped, never bent to fit"
    );
    let mut sc = Scratch::default();
    let land = ctx_at(t0 + spawn.t_flight, &cfg, (3, 40), 0.25);
    sc.emit(&mut m, &land);
    assert!(m.landings.is_empty(), "a dropped flight mints no landing");

    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
    m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
    m.translate_band(3, 10, 1, 0, 0);
    assert!(
        m.live.is_empty(),
        "with no cell height the pools fail closed"
    );
}

#[test]
fn a_scroll_between_spawn_and_arrival_moves_the_landing_with_the_train() {
    let cfg = config();
    let t0 = Instant::now();
    let g = geom();
    let rows = 2_u16;
    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
    let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
    m.translate_scroll(rows, g.ch as u16);
    let (wx, wy) = g.cell_center(5 - rows, 40);
    let arrival = t0 + spawn.t_flight;
    let land = ctx_at(arrival, &cfg, (5 - rows, 40), 0.25);
    let mut sc = Scratch::default();
    sc.emit(&mut m, &land);
    let l = m
        .landings
        .first()
        .copied()
        .expect("the landing is minted at T");
    assert!(
        (l.x - wx).abs() < 0.5 && (l.y - wy).abs() < 0.5,
        "the landing was minted at ({}, {}), not at the scrolled endpoint ({wx}, {wy})",
        l.x,
        l.y
    );
    assert_eq!(
        (l.x, l.y),
        (m.live[0].x1, m.live[0].y1),
        "the landing is the train's end"
    );
    let (cx, cy) = (wx.round() as i32, wy.round() as i32);
    assert!(
        sc.out.iter().any(|q| is_white(q.color)
            && q.w == 2
            && q.h == 2
            && i32::from(q.x) == cx - 1
            && i32::from(q.y) == cy - 1),
        "the pin's heart is not on the scrolled landing"
    );
    let mut dust = sky();
    m.sow_into(&mut dust, &land);
    let fan = lane(&dust, StarLane::Fan);
    assert!(fan.len() >= FAN_HERO_N);
    for s in &fan {
        assert!(
            (s.x - wx).abs() <= 2.0 && (s.y - wy).abs() <= 2.0,
            "a fan star was born at ({}, {}), not on the scrolled landing ({wx}, {wy})",
            s.x,
            s.y
        );
    }

    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (3, 40), 0.25);
    let spawn = m.on_event(&mv((9, 40), (3, 40)), t0, &ctx).expect("flies");
    m.translate_scroll(7, g.ch as u16);
    assert_eq!(
        m.live.len(),
        1,
        "a train whose launch is still on glass is kept"
    );
    assert!(m.live[0].y1 < 0.0 && m.live[0].y0 >= 0.0);
    let mut dust = sky();
    let land = ctx_at(t0 + spawn.t_flight, &cfg, (0, 40), 0.25);
    sc.emit(&mut m, &land);
    m.sow_into(&mut dust, &land);
    assert!(m.live[0].landed, "the arrival edge is still spent");
    assert!(
        m.landings.is_empty(),
        "a landing above the glass was minted"
    );
    assert!(
        lane(&dust, StarLane::Fan).is_empty(),
        "a fan was sown above the glass"
    );
    assert!(
        !lane(&dust, StarLane::Shed).is_empty(),
        "the stations still on glass were not sown"
    );
    assert!(
        dust.live_iter().all(|s| s.y >= 0.0),
        "a fragment was sown above the glass"
    );
}

const fn owner_geom() -> Geom {
    Geom {
        cw: 20,
        ch: 38,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 1600,
        win_h: 912,
        head: 0,
    }
}

const fn ctx_on(now: Instant, cfg: &Config, caret: (u16, u16), g: Geom) -> Ctx<'_> {
    let mut c = ctx_at(now, cfg, caret, 0.25);
    c.geom = g;
    c
}

fn landed(cells: u16, cfg: &Config, g: Geom, blank: bool) -> (Meteors, Landing, Instant) {
    landed_with(cells, cfg, g, blank, 0.25, false)
}

fn landed_with(
    cells: u16,
    cfg: &Config,
    g: Geom,
    blank: bool,
    t_land: f32,
    leftward: bool,
) -> (Meteors, Landing, Instant) {
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let (from, to) = if leftward {
        ((5_u16, cells + 20), (5_u16, 20))
    } else {
        ((5_u16, 0), (5_u16, cells))
    };
    let mut ctx = ctx_on(t0, cfg, to, g);
    ctx.caret_t = t_land;
    let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("fly");
    let arrival = t0 + spawn.t_flight;
    let mut sc = Scratch::default();
    let at = ctx_on(arrival, cfg, to, g);
    {
        let mut fr = sc.frame();
        m.emit(&at, &mut fr);
    }
    let mut dust = Stardust::new();
    for r in 4..=6 {
        dust.probe_mut().probe_row(r, &[!blank; 120]);
    }
    m.sow_into(&mut dust, &at);
    let l = m.landings[0];
    (m, l, arrival)
}

fn burst_only(sc: &mut Scratch, l: &Landing, cfg: &Config, g: Geom, arrival: Instant, after: u64) {
    sc.clear();
    let at = ctx_on(arrival + ms(after), cfg, (5, 40), g);
    let mut fr = sc.frame();
    draw_burst(l, &at, &mut fr);
}

#[test]
fn the_retire_decay_lands_on_the_chroma_cull() {
    let at_r = (-RETIRE_DECAY).exp();
    assert!(
        (at_r - CHROMA_CULL_ALPHA).abs() < 1e-3,
        "a retired train is at {at_r} after R, not the cull {CHROMA_CULL_ALPHA}"
    );
}

#[test]
fn a_spike_root_is_derived_from_the_spacing_it_must_keep_open() {
    let want = 2.0 * (std::f32::consts::PI / BURST_N_MAX as f32).sin();
    assert!(
        (BURST_ROOT_SEP_AT_MAX_N - want).abs() < 1e-6,
        "the separation coefficient is {BURST_ROOT_SEP_AT_MAX_N}, not 2 sin(pi/12) = {want}"
    );
}

#[test]
fn a_jet_breaks_over_a_glyph_and_the_star_survives() {
    let cfg = config();
    let g = owner_geom();
    let mut span = [0.0_f32; 2];
    let mut core = [0_usize; 2];
    for (i, blank) in [true, false].into_iter().enumerate() {
        let (_m, l, arrival) = landed(40, &cfg, g, blank);
        let b = l.burst.expect("a 40-cell nav landing bursts");
        let mut sc = Scratch::default();
        burst_only(&mut sc, &l, &cfg, g, arrival, (b.ms * 0.9) as u64);
        for q in &sc.out {
            let x = f32::from(q.x) + f32::from(q.w) * 0.5;
            span[i] = span[i].max((x - l.x).abs());
            let (r, gg, bb) = chan(q.color);
            assert!(
                f32::from(r.max(gg).max(bb) as u8) <= TRANSIENT_STAR_COV_CEIL + 1.0,
                "a burst quad asks {} — over the transient ceiling",
                r.max(gg).max(bb)
            );
            if (x - l.x).abs() <= BURST_CORE_CH * g.ch as f32 {
                core[i] += 1;
            }
        }
    }
    assert!(
        span[1] < BURST_CORE_CH * g.ch as f32 + 2.0,
        "over a full row of text the burst still reaches {} px — the jets did not break",
        span[1]
    );
    assert!(
        span[0] > 2.0 * BURST_CORE_CH * g.ch as f32,
        "over blank cells the jets only reached {} px",
        span[0]
    );
    assert!(
        core[1] * 4 >= core[0] * 3,
        "the core star lost {} of its {} quads to the gate — it must not be gated at all",
        core[0] - core[1],
        core[0]
    );
}

#[test]
fn a_burst_stays_inside_its_quad_cap_at_every_grade() {
    let cfg = config();
    let big = Geom {
        cw: 40,
        ch: 76,
        ..owner_geom()
    };
    for g in [owner_geom(), geom(), big] {
        for cells in [8_u16, 19, 40, 60] {
            let (_m, l, arrival) = landed(cells, &cfg, g, true);
            let b = l.burst.expect("a nav landing bursts");
            let mut sc = Scratch::default();
            let mut peak = 0_usize;
            let mut after = 0_u64;
            while after <= b.ms as u64 {
                burst_only(&mut sc, &l, &cfg, g, arrival, after);
                peak = peak.max(sc.out.len());
                assert!(
                    sc.out.len() <= BURST_QUAD_CAP,
                    "cw{} ch{} {cells} cells at T+{after}: {} quads, over the cap",
                    g.cw,
                    g.ch,
                    sc.out.len()
                );
                after += 8;
            }
            assert!(peak > 0, "the burst never drew");
            assert!(sc.under.is_empty(), "the burst moved light UNDER the ink");
            assert!(sc.halos.is_empty(), "the burst added a halo");
        }
    }
}

#[test]
fn the_burst_is_off_glass_by_the_flight_horizon() {
    let cfg = config();
    let g = owner_geom();
    for cells in [8_u16, 40, 60] {
        let (_m, l, arrival) = landed(cells, &cfg, g, true);
        let mut sc = Scratch::default();
        burst_only(&mut sc, &l, &cfg, g, arrival, FLIGHT_OFF_GLASS_MS as u64);
        assert!(
            sc.out.is_empty(),
            "{cells} cells: {} burst quads still on glass at the horizon",
            sc.out.len()
        );
    }
}

#[test]
fn the_light_burst_is_ink_not_additive_light() {
    let mut cfg = config();
    cfg.dark_theme = false;
    let g = owner_geom();
    let (_m, l, arrival) = landed(40, &cfg, g, true);
    let b = l.burst.expect("a 40-cell nav landing bursts");
    let mut sc = Scratch::default();
    burst_only(&mut sc, &l, &cfg, g, arrival, (b.ms * 0.59) as u64);
    assert!(!sc.out.is_empty(), "the light burst drew nothing");
    for q in &sc.out {
        assert!(q.alpha > 0, "an additive quad on a light page");
        assert!(
            f32::from(q.alpha) <= LIGHT_ALPHA_CAP + 1.0,
            "a light burst quad at alpha {}",
            q.alpha
        );
    }
}

#[test]
fn the_reduced_motion_burst_is_a_still_star() {
    let mut cfg = config();
    cfg.reduced_motion = true;
    let g = owner_geom();
    let (_m, l, arrival) = landed(40, &cfg, g, true);
    let mut sc = Scratch::default();
    let mut spans = Vec::new();
    for after in [0_u64, 120, 300] {
        burst_only(&mut sc, &l, &cfg, g, arrival, after);
        let mut r = 0.0_f32;
        for q in &sc.out {
            let dx = f32::from(q.x) + f32::from(q.w) * 0.5 - l.x;
            let dy = f32::from(q.y) + f32::from(q.h) * 0.5 - l.y;
            r = r.max((dx * dx + dy * dy).sqrt());
        }
        spans.push(r);
    }
    assert!(
        (spans[0] - spans[2]).abs() < 2.0,
        "the static star changed size: {spans:?}"
    );
    burst_only(&mut sc, &l, &cfg, g, arrival, FLIGHT_OFF_GLASS_MS as u64);
    assert!(sc.out.is_empty(), "the static star outlived the horizon");
}

/// Every probed cell of the landing row blank.
const OPEN_ROW: u32 = (1 << (2 * JET_PROBE_CELLS + 1)) - 1;

/// The landing row of a line of words: each fourth cell from the landing's a gap, the rest
/// letters.
fn row_of_words() -> u32 {
    (-JET_PROBE_CELLS..=JET_PROBE_CELLS)
        .filter(|k| k.rem_euclid(4) == 0)
        .fold(0, |row, k| row | 1 << (k + JET_PROBE_CELLS))
}

/// A dark jet's two sections meet where the first ends and the second begins, and a section
/// shorter than two of the jet's lattice pitches still draws: one jet straight along the row,
/// at every phase of its root on the lattice and from a pitch and a fifth to five pitches long,
/// lays light on every pixel column from its root to the end of its first section. The jet
/// from the lattice alone ended the first section at its last station short of the end and
/// began the second at the first past it, a gap a pitch long, and laid nothing for a section
/// that held fewer than two stations.
#[test]
fn a_dark_jet_s_sections_meet_and_a_short_one_draws() {
    let cfg = config();
    for g in [owner_geom(), geom()] {
        let (_m, mut l, arrival) = landed(40, &cfg, g, true);
        l.sky = Some(SkyMask { row: OPEN_ROW });
        let ctx = ctx_on(arrival, &cfg, (5, 40), g);
        let pitch = g.cw as f32 / JET_STATIONS_PER_CELL as f32;
        for pitches in [1.2_f32, 1.9, 2.5, 5.0] {
            for phase in 0..8 {
                let root = BURST_CORE_CH * g.ch as f32 + pitch * phase as f32 / 8.0;
                let tip = root + pitches * pitch;
                let s1 = root + (tip - root) * JET_SEC_SHARE[0];
                let mut sc = Scratch::default();
                burst_jet(
                    &mut sc.frame(),
                    &ctx,
                    &l,
                    (1.0, 0.0),
                    root,
                    tip,
                    150.0,
                    None,
                    &mut None,
                );
                let cols: std::collections::BTreeSet<i32> = sc
                    .out
                    .iter()
                    .flat_map(|q| i32::from(q.x)..i32::from(q.x) + i32::from(q.w))
                    .collect();
                let (from, to) = ((l.x + root).floor() as i32, (l.x + s1).floor() as i32);
                let holes: Vec<i32> = (from..=to).filter(|x| !cols.contains(x)).collect();
                assert!(
                    holes.is_empty(),
                    "cw{} ch{}: a jet {pitches} pitches long, its root {phase}/8 of a pitch on, \
                     leaves columns {holes:?} of {from}..={to} dark",
                    g.cw,
                    g.ch
                );
            }
        }
    }
}

/// Each pixel's lightings over a landing burst's life past the core star's reach (the round
/// star's lances end inside it), every 16 ms frame: the jets, and on paper their light squares.
fn jet_lightings(cfg: &Config, row: u32) -> usize {
    let g = owner_geom();
    let (_m, mut l, arrival) = landed(40, cfg, g, true);
    l.sky = Some(SkyMask { row });
    let b = l.burst.expect("a 40-cell nav landing bursts");
    let reach = BURST_CORE_CH * g.ch as f32 + BURST_SEC_THICK_CH[0] * g.ch as f32;
    let mut sc = Scratch::default();
    let frames: Vec<_> = (0..=(b.ms as u64 / 16) + 1)
        .map(|f| {
            burst_only(&mut sc, &l, cfg, g, arrival, f * 16);
            let mut now = light_map(&sc.out, &[]);
            now.retain(|&(x, _), _| (f32::from(x) + 0.5 - l.x).abs() > reach);
            now
        })
        .collect();
    assert!(
        frames.iter().any(|f| !f.is_empty()),
        "the jets drew nothing past the core star"
    );
    lightings(frames).values().copied().max().unwrap_or(0)
}

/// A landing burst's jets, and on paper their light squares, stand on a lattice from the
/// landing, so as the ring grows no pixel of them goes dark and lit again: over an open row
/// and a line of words (the jets break over each letter), on a dark page and on paper, a pixel
/// is lit once. Control: spread over each section from its moving start
/// ([`with_bursts_spread`]), the stations slide, a square's edge falls on and off a pixel and
/// a station crosses between a gap and a letter, the jet's piece in the gap lit again.
#[test]
fn a_burst_s_jets_light_each_pixel_once_as_the_ring_grows() {
    let dark = config();
    let paper = Config {
        dark_theme: false,
        theme_fg: 0x0024_292F,
        theme_bg: 0x00FF_FFFF,
        ..config()
    };
    for (cfg, page) in [(&dark, "dark"), (&paper, "paper")] {
        for (row, name) in [
            (OPEN_ROW, "an open row"),
            (row_of_words(), "a line of words"),
        ] {
            let once = jet_lightings(cfg, row);
            assert_eq!(
                once, 1,
                "{page}, {name}: a pixel of the jets lit {once} times"
            );
        }
        let spread = with_bursts_spread(|| jet_lightings(cfg, row_of_words()));
        assert!(
            spread > 1,
            "control: {page}, spread over its sections, no pixel of the jets lit twice"
        );
    }
}

#[test]
fn a_jet_is_probed_over_its_whole_capped_reach() {
    for g in [owner_geom(), geom()] {
        let cells = JET_LEN_SHARE[0]
            * (RING_R_CH + RING_R_PER_IMPACT_CH * (super::super::timing::IMPACT_MAX - 1.0))
            * (g.ch as f32 / g.cw as f32);
        assert!(
            cells <= JET_PROBE_CELLS as f32,
            "cw{} ch{}: the capped jet reaches {cells:.1} cells, past the {} the probe covers",
            g.cw,
            g.ch,
            JET_PROBE_CELLS
        );
    }
}

#[test]
fn the_burst_paints_inside_the_fan_s_rise_at_every_cell() {
    let cfg = config();
    let mut worst = 0.0_f32;
    for ch in [12_usize, 18, 38, 76] {
        let g = Geom {
            cw: (ch as f32 * 0.52).round() as usize,
            ch,
            rows: 24,
            cols: 100,
            origin_x: 0,
            origin_y: 0,
            win_w: 2000,
            win_h: 2000,
            head: 0,
        };
        for cells in [8_u16, 40, 60] {
            let (_m, l, arrival) = landed(cells, &cfg, g, true);
            let b = l.burst.expect("a nav landing bursts");
            let mut sc = Scratch::default();
            let mut rise = 0.0_f32;
            let mut after = 0_u64;
            while after <= b.ms as u64 + 8 {
                burst_only(&mut sc, &l, &cfg, g, arrival, after);
                for q in &sc.out {
                    let top = f32::from(q.y);
                    let bot = f32::from(q.y) + f32::from(q.h);
                    rise = rise.max((l.y - top).max(bot - l.y));
                }
                after += 4;
            }
            let cap = FAN_RISE_MAX_CH * ch as f32;
            let in_ch = rise / ch as f32;
            worst = worst.max(in_ch);
            assert!(
                rise <= cap,
                "ch {ch}, {cells} cells: the burst PAINTS to {in_ch:.3} ch, past the fan's \
                 rise of {FAN_RISE_MAX_CH} ch — the shipped ring painted to 2.10 ch under an \
                 assert that said 1.5"
            );
        }
    }
    assert!(
        worst < 2.10,
        "the burst paints to {worst:.3} ch — no tighter than the ring's own 2.10 ch"
    );
}

#[test]
fn an_idle_meteor_pool_draws_nothing_and_names_no_deadline() {
    let cfg = config();
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let mut sc = Scratch::default();
    for k in 0..8_u64 {
        let ctx = ctx_at(t0 + ms(k * 16), &cfg, (5, 10), 0.25);
        sc.emit(&mut m, &ctx);
        assert!(sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty());
    }
    assert!(m.at_rest());
    assert_eq!(m.live(), 0);
    assert!(m.next_change_deadline(t0).is_none());

    let ctx = ctx_at(t0, &cfg, (5, 0), 0.25);
    let wrap = Event::Move {
        from: (5, 119),
        to: (6, 0),
        licence: Licence::Typed,
        dir: Dir::Down,
    };
    assert!(
        m.on_event(&wrap, t0, &ctx).is_none(),
        "a typed wrap flew — `ink_wrap_rainbow_keeps_the_ribbon_no_zoom`"
    );
    assert!(m.at_rest(), "a typed wrap left something in the air");
}

#[test]
fn the_attack_asks_for_every_frame_and_the_release_rides_the_host_s_train() {
    use crate::rainbow_kitty::{FRAME_CADENCE, TAIL_FLOOR};
    let cfg = config();
    let t0 = Instant::now();
    let close = Duration::from_secs_f32(PIN_MS / 1000.0);
    let root_suck = Duration::from_secs_f32(ROOT_SUCK_MS / 1000.0);
    let flights = [
        ((5, 0), (5, 8), Licence::Nav),
        ((5, 0), (5, 40), Licence::Nav),
        ((5, 80), (5, 0), Licence::Nav),
        ((5, 40), (5, 80), Licence::Nav),
        ((5, 20), (6, 0), Licence::Return),
    ];
    for (from, to, licence) in flights {
        let name = format!("{from:?} → {to:?} {licence:?}");
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, to, 0.25);
        let spawn = m
            .on_event(&mv_as(from, to, licence), t0, &ctx)
            .expect("fly");
        let arrival = t0 + spawn.t_flight;
        let end = m.live[0].end();
        let mut sc = Scratch::default();
        let mut moving_until: Option<Instant> = None;
        let mut sub_floor = 0_u32;
        let mut release_wakes = 0_u32;
        let mut t = t0;
        while t < end + ms(50) {
            sc.emit(&mut m, &ctx_at(t, &cfg, to, 0.25));
            if moving_until.is_none()
                && let Some(l) = m.landings.first()
            {
                moving_until = Some(l.moving_until().max(arrival + root_suck));
            }
            let brisk = m.brisk(t);
            let due = m.next_change_deadline(t);
            let after = t.saturating_duration_since(arrival).as_millis();
            if let Some(d) = due
                && d < t + TAIL_FLOOR
            {
                sub_floor += 1;
            }
            if t < arrival + close {
                assert!(brisk, "{name}: not brisk at T + {after} ms, in the attack");
                assert_eq!(
                    due,
                    Some(t + FRAME_CADENCE),
                    "{name}: the attack did not ask for the next frame at T + {after} ms"
                );
            } else if moving_until.is_some_and(|mu| t < mu) {
                release_wakes += 1;
                assert!(brisk, "{name}: the release is not brisk at T + {after} ms");
                assert!(
                    due.is_some_and(|d| d >= t + TAIL_FLOOR),
                    "{name}: the release named {:?} at T + {after} ms — under the floor",
                    due.map(|d| d.saturating_duration_since(t))
                );
            } else if t < end {
                assert!(
                    !brisk,
                    "{name}: brisk at T + {after} ms with nothing moving"
                );
                assert!(
                    sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty(),
                    "{name}: the meteor drew at T + {after} ms, past its last motion"
                );
                assert!(
                    due.is_some_and(|d| d >= t + TAIL_FLOOR),
                    "{name}: the horizon named {:?} at T + {after} ms",
                    due.map(|d| d.saturating_duration_since(t))
                );
            } else {
                assert!(m.at_rest(), "{name}: not at rest at T + {after} ms");
                assert!(due.is_none(), "{name}: a resting pool named a deadline");
            }
            t += ms(8);
        }
        let mu = moving_until.expect("the landing was minted");
        let release = mu.saturating_duration_since(arrival + close);
        let floor = if licence == Licence::Return {
            root_suck - close
        } else {
            Duration::from_secs_f32(RING_MS / 1000.0) - close
        };
        assert!(
            release >= floor && release_wakes > 0,
            "{name}: the release is {release:?} ({release_wakes} wakes), less than {floor:?}"
        );
        let attack = (spawn.t_flight + close).as_secs_f32() * 1000.0 / 8.0;
        assert!(
            (sub_floor as f32 - attack).abs() <= 1.0,
            "{name}: {sub_floor} sub-floor deadlines over the life; the attack is {attack:.0} \
             frames"
        );
    }
}

#[test]
fn a_spark_is_culled_exactly_where_the_cadence_law_says_it_is() {
    let ch = geom().ch as f32;
    let cull = spark_cull_u();
    let seed = mix32(0xBEEF ^ 0xFA5E);
    for k in 0..SPARK_N_MAX {
        let s = mint_spark(seed, k, ch);
        let at = s.life * cull;
        assert!(
            spark_at(s, at - 1.0, ch).is_some(),
            "spark {k}: already gone 1 ms before its cull at {at:.1} ms"
        );
        assert!(
            spark_at(s, at + 1.0, ch).is_none(),
            "spark {k}: still drawn 1 ms after its cull at {at:.1} ms"
        );
    }
}

/// On a dark page the sparks are haloed stars: a 48-cell jump (landed ten
/// cells in so the leftward half is on the glass) throws at least sixty
/// sparks, and once the last is born ([`SPARK_STAND_LAST_MS`]) at least
/// fifty chromatic halos are on the frame and at least forty sparks stand
/// ≥ 5 px on both axes (replaces the pruned
/// `the_sparks_are_haloed_stars_twice_the_bold_round_s_count`; 60, 64 and
/// 60 at Prism Glass P0.11).
#[test]
fn the_dark_sparks_are_haloed_stars_twice_the_bold_round_s_count() {
    let cfg = config();
    let g = geom();
    let ch = g.ch as f32;
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let ctx = ctx_at(t0, &cfg, (5, 10), 0.25);
    let spawn = m.on_event(&mv((5, 58), (5, 10)), t0, &ctx).expect("fly");
    let arrival = t0 + spawn.t_flight;
    let mut sc = Scratch::default();
    sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 10), 0.25));
    let l = m.landings[0];
    assert!(
        usize::from(l.sparks) >= 60,
        "a 48-cell landing throws {} sparks, not twice the bold round's thirty",
        l.sparks
    );
    let born = SPARK_STAND_LAST_MS.ceil() as u64;
    sc.emit(&mut m, &ctx_at(arrival + ms(born), &cfg, (5, 10), 0.25));
    let halos = sc.halos.iter().filter(|h| is_chromatic(h.color)).count();
    assert!(
        halos >= 50,
        "{halos} chromatic halos at T + {born}: the sparks have none"
    );
    let cell_sized = |q: &GlowQuad| usize::from(q.w) == g.cw && usize::from(q.h) == g.ch;
    let mut big = 0_usize;
    for spark in l.spark.iter().take(usize::from(l.sparks)) {
        let Some(((dx, dy), _)) = spark_at(*spark, born as f32, ch) else {
            continue;
        };
        let (px, py) = ((l.x + dx).round() as i32, (l.y + dy).round() as i32);
        let (mut x_lo, mut x_hi, mut y_lo, mut y_hi) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for q in sc.out.iter().filter(|q| {
            is_chromatic(q.color)
                && !cell_sized(q)
                && (i32::from(q.x) + i32::from(q.w) / 2 - px).abs() <= 4
                && (i32::from(q.y) + i32::from(q.h) / 2 - py).abs() <= 4
        }) {
            x_lo = x_lo.min(i32::from(q.x));
            x_hi = x_hi.max(i32::from(q.x) + i32::from(q.w));
            y_lo = y_lo.min(i32::from(q.y));
            y_hi = y_hi.max(i32::from(q.y) + i32::from(q.h));
        }
        if x_hi - x_lo >= 5 && y_hi - y_lo >= 5 {
            big += 1;
        }
    }
    assert!(
        big >= 40,
        "only {big} sparks stand ≥ 5 × 5 px at T + {born}: points, not stars"
    );
}

#[test]
fn a_party_landing_is_a_fan_off_the_glass_inside_315_ms() {
    let t0 = Instant::now();
    let cfg = config();
    let mut m = Meteors::new();
    let mut sc = Scratch::default();
    let at = ctx_at(t0, &cfg, (5, 40), 0.3);

    m.party(t0, 5, false, &at);
    assert_eq!(m.sown.len(), 1, "the fan is staged for the sky");
    assert!(m.landings.is_empty(), "no ring bar, no landing in the pool");
    m.party(t0, 11, true, &at);
    assert_eq!(m.landings.len(), 1);
    let l = m.landings[0];
    assert!(l.burst.is_some(), "the fourth bar carries the starburst");
    assert!(
        !l.flash && l.sparks == 0,
        "a bar line is a beat, not an impact"
    );
    let mut flash_only = Scratch::default();
    draw_flash(&l, &at, &mut flash_only.frame());
    assert!(
        flash_only.out.is_empty(),
        "the celebration emits no flash quads"
    );
    assert!(!l.fan.hero, "no gold hero: the party is the spectrum");
    assert_eq!(l.fan.n, 11);
    let (cx, cy) = at.geom.cell_center(5, 40);
    assert!(
        (l.x - cx).abs() < 1e-3 && (l.y - cy).abs() < 1e-3,
        "minted at the caret"
    );

    sc.emit(&mut m, &at);
    let mut dust = sky_around(5);
    m.sow_into(&mut dust, &at);
    assert_eq!(
        dust.live(),
        16,
        "5 + 11 stars, all admitted on a dark theme"
    );
    let fan = lane(&dust, StarLane::Fan);
    assert_eq!(
        fan.len(),
        16,
        "every party star is a fan-lane (silent, transient) star"
    );
    assert!(m.sown.is_empty(), "the stage is drained");

    let mut t = t0;
    let mut gone_at = None;
    while t <= t0 + ms(700) {
        let ctx = ctx_at(t, &cfg, (5, 40), 0.3);
        sc.clear();
        let mut fr = sc.frame();
        dust.emit(&ctx, &mut fr);
        if gone_at.is_none() && dust.at_rest() {
            gone_at = Some(t);
        }
        t += ms(16);
    }
    let gone = gone_at.expect("the fan leaves the glass");
    assert!(
        gone <= t0 + ms(315),
        "the party's last star must be gone by T + 315 ms, was {:?}",
        gone.saturating_duration_since(t0)
    );
    let late = ctx_at(t0 + ms(700), &cfg, (5, 40), 0.3);
    sc.emit(&mut m, &late);
    assert!(
        m.at_rest(),
        "the ring landing is out of the pool with the meteor horizon"
    );

    let mut off = Meteors::new();
    let far = ctx_at(t0, &cfg, (200, 40), 0.3);
    off.party(t0, 9, true, &far);
    assert!(
        off.sown.is_empty() && off.landings.is_empty(),
        "a caret off the glass mints nothing"
    );
}

/// Each pixel's light on a frame, `0..=255`, added up: a quad's ink alpha on paper or its
/// brightest channel of added light on a dark page, and a halo's peak under the renderer's
/// falloff ([`aterm_render::halo_weight`]).
fn light_map(out: &[GlowQuad], halos: &[RainHalo]) -> BTreeMap<(u16, u16), u32> {
    let mut light: BTreeMap<(u16, u16), u32> = BTreeMap::new();
    for q in out {
        let (r, g, b) = chan(q.color);
        let v = u32::from(q.alpha).max(r.max(g).max(b));
        for x in q.x..q.x + q.w {
            for y in q.y..q.y + q.h {
                *light.entry((x, y)).or_default() += v;
            }
        }
    }
    for h in halos {
        let (r, g, b) = chan(h.color);
        let peak = r.max(g).max(b);
        let (rx2, ry2) = (i32::from(h.rx.max(1)).pow(2), i32::from(h.ry.max(1)).pow(2));
        for x in h.x..h.x + h.w {
            for y in h.y..h.y + h.h {
                let ny = aterm_render::halo_row_ny(i32::from(y) - i32::from(h.cy), ry2);
                let w = aterm_render::halo_weight(i32::from(x) - i32::from(h.cx), ny, rx2);
                *light.entry((x, y)).or_default() += peak * w as u32 / 256;
            }
        }
    }
    for v in light.values_mut() {
        *v = (*v).min(255);
    }
    light
}

/// How many times each pixel lights over `frames` ([`light_map`]s), as the flash law reads a
/// turn: its light rises a tenth of full or more over the least it has held since it last
/// fell, and falls when it drops a tenth or more under the most it held since it lit (an
/// antialiased edge wavering a few steps is no turn).
fn lightings(
    frames: impl IntoIterator<Item = BTreeMap<(u16, u16), u32>>,
) -> BTreeMap<(u16, u16), usize> {
    const TENTH: u32 = 26;
    // Per pixel: lit, the least (dark) or most (lit) since its last turn, and its lightings.
    let mut at: BTreeMap<(u16, u16), (bool, u32, usize)> = BTreeMap::new();
    for frame in frames {
        for &px in frame.keys() {
            at.entry(px).or_insert((false, 0, 0));
        }
        for (px, (lit, edge, n)) in &mut at {
            let v = frame.get(px).copied().unwrap_or(0);
            if !*lit {
                *edge = (*edge).min(v);
                if v >= *edge + TENTH {
                    (*lit, *edge, *n) = (true, v, *n + 1);
                }
            } else {
                *edge = (*edge).max(v);
                if v + TENTH <= *edge {
                    (*lit, *edge) = (false, v);
                }
            }
        }
    }
    at.into_iter().map(|(px, (_, _, n))| (px, n)).collect()
}

/// A landing's sparks stand, so no pixel of the shower goes dark and lit again more than three
/// times in a second (the flash law's bound): a 48-cell jump on a dark page, every 16 ms frame
/// of the sparks alone, a pixel lit when a star's quad or its halo covers it. Control: thrown
/// ([`with_sparks_thrown`]), forty to sixty stars cross the pixels round the landing in turn.
#[test]
fn a_shower_lights_no_pixel_more_than_three_times_a_second() {
    let cfg = config();
    let to = (5_u16, 10_u16);
    let most_lit = || {
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, to, 0.25);
        let spawn = m.on_event(&mv((5, 58), to), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(arrival, &cfg, to, 0.25));
        let l = m.landings[0];
        let frames = (0..=(SPARK_LIFE_MAX_MS as u64 / 16)).map(|f| {
            let ctx = ctx_at(arrival + ms(f * 16), &cfg, to, 0.25);
            let mut sp = Scratch::default();
            draw_sparks(&l, &ctx, &mut sp.frame(), SPARK_HALO_CAP);
            assert!(
                sp.halos.len() <= SPARK_HALO_CAP,
                "{} halo quads, over the cap",
                sp.halos.len()
            );
            light_map(&sp.out, &sp.halos)
        });
        lightings(frames).values().copied().max().unwrap_or(0)
    };
    let standing = most_lit();
    assert!(
        (1..=3).contains(&standing),
        "a pixel of the shower is lit {standing} times in its life (a second at most)"
    );
    let thrown = with_sparks_thrown(most_lit);
    assert!(
        thrown > 3,
        "control: thrown, the shower lights a pixel at most {thrown} times"
    );
}

/// Per landing, each spark's (bright frames, halo frames, lightings); and a frame's most spark
/// halo quads ([`spark_halo_film`]).
type HaloFilm = (Vec<Vec<(usize, usize, usize)>>, usize);

/// The spark halos of a 48-cell jump's landing and, when `back_ms` is given, of a jump back keyed
/// that long after the first lands, drawn in frames 16 ms apart with the frame's halos held to
/// `cap` ([`draw_sparks`]). Per landing, each spark's bright frames (its star at or over
/// [`SPARK_SHRINK_ALPHA`]), the frames it wore its halo and the times its halo lit; and the most
/// spark halo quads one frame laid.
fn spark_halo_film(back_ms: Option<u64>, cap: usize) -> HaloFilm {
    let cfg = config();
    let (a, b) = ((5_u16, 58_u16), (5_u16, 10_u16));
    let t0 = Instant::now();
    let mut m = Meteors::new();
    let flight = m
        .on_event(&mv(a, b), t0, &ctx_at(t0, &cfg, b, 0.25))
        .expect("a 48-cell jump flies")
        .t_flight;
    let back = back_ms.map(|gap| t0 + flight + ms(gap));
    let g = SPARK_G_CH_PER_S2 * geom().ch as f32 * (1.0 / 1_000_000.0);
    let mut caret = b;
    let mut seen: BTreeMap<Instant, Vec<(usize, usize, usize, bool)>> = BTreeMap::new();
    let mut most = 0;
    for f in 0..=150_u64 {
        let at = t0 + ms(f * 16);
        if caret == b && back.is_some_and(|back| at >= back) {
            caret = a;
            m.on_event(&mv(b, a), at, &ctx_at(at, &cfg, a, 0.25))
                .expect("the jump back flies");
        }
        let ctx = ctx_at(at, &cfg, caret, 0.25);
        Scratch::default().emit(&mut m, &ctx);
        let mut sp = Scratch::default();
        {
            let mut fr = sp.frame();
            for l in &m.landings {
                draw_sparks(l, &ctx, &mut fr, cap);
            }
        }
        most = most.max(sp.halos.len());
        for l in &m.landings {
            let n = usize::from(l.sparks);
            let age = ms_since(l.pin.at, at);
            let film = seen
                .entry(l.pin.at)
                .or_insert_with(|| vec![(0, 0, 0, false); n]);
            for (s, r) in l.spark.iter().zip(film.iter_mut()) {
                let seen_at = spark_at_with_gravity(*s, age, g);
                let on = seen_at.is_some_and(|((dx, dy), _)| {
                    let (x, y) = ((l.x + dx).round(), (l.y + dy).round());
                    sp.halos
                        .iter()
                        .any(|h| f32::from(h.cx) == x && f32::from(h.cy) == y)
                });
                r.0 += usize::from(seen_at.is_some_and(|(_, alpha)| alpha >= SPARK_SHRINK_ALPHA));
                r.1 += usize::from(on);
                r.2 += usize::from(on && !r.3);
                r.3 = on;
            }
        }
    }
    let landings = seen
        .into_values()
        .map(|v| v.into_iter().map(|(b, w, l, _)| (b, w, l)).collect())
        .collect();
    (landings, most)
}

/// A landing's bright spark frames without a halo, and its bright spark frames.
fn bare_of(landing: &[(usize, usize, usize)]) -> (usize, usize) {
    landing.iter().fold((0, 0), |(bare, bright), &(b, w, _)| {
        (bare + b.saturating_sub(w), bright + b)
    })
}

/// A spark's halo is lit for one stretch of its bright life, granted when its landing comes, so
/// it lights once whatever the sparks and landings before it do, and with no cap on the frame
/// every live landing's halos keep under [`SPARK_HALO_CAP`]'s quads: a 48-cell jump on a dark
/// page alone, and with a jump back landing among its bright halos or after them. A lone shower,
/// and the jump back after the first's halos are out, wear their halos for four fifths of their
/// bright frames at least. Controls: every bright spark wearing a halo while the frame has room
/// ([`with_halos_regranted`]) lights one more than once; halos granted whole or not at all
/// ([`with_halos_whole_or_none`]) leave over a fifth of a lone shower's bright frames bare.
#[test]
fn a_spark_s_halo_lights_once_and_lasts_most_of_its_bright_life() {
    // Read with no cap on the frame, the grants alone keep the halos under it.
    let films = |cap| [None, Some(100), Some(250)].map(|back| spark_halo_film(back, cap));
    let lit_most = |films: &[HaloFilm]| {
        films
            .iter()
            .flat_map(|(ls, _)| ls.iter().flatten())
            .map(|&(_, _, lit)| lit)
            .max()
            .unwrap_or(0)
    };
    let read = films(usize::MAX);
    for (back, (landings, most)) in ["none", "100 ms", "250 ms"].iter().zip(&read) {
        assert_eq!(
            landings.len(),
            1 + usize::from(*back != "none"),
            "back {back}"
        );
        assert!(
            *most <= SPARK_HALO_CAP,
            "back {back}: a frame laid {most} spark halo quads, over the cap's {SPARK_HALO_CAP}"
        );
    }
    let once = lit_most(&read);
    assert_eq!(once, 1, "a spark's halo lit {once} times");
    // A lone shower, and a jump back landing once the first's halos are out: no more than a fifth
    // of a bright star's frames go without its halo. A jump back landing among the first's bright
    // halos shares the cap with them and is held to the once law alone.
    for (what, landing) in [("lone", &read[0].0[0]), ("back", &read[2].0[1])] {
        let (bare, bright) = bare_of(landing);
        assert!(
            5 * bare <= bright,
            "{what}: {bare} of {bright} bright spark frames wore no halo"
        );
    }
    let regranted = lit_most(&with_halos_regranted(|| films(SPARK_HALO_CAP)));
    assert!(
        regranted > 1,
        "control: every bright spark wearing a halo while the frame had room lit none twice"
    );
    let whole = with_halos_whole_or_none(|| films(usize::MAX));
    let (bare, bright) = bare_of(&whole[0].0[0]);
    assert!(
        5 * bare > bright,
        "control: halos whole or none left only {bare} of {bright} bright frames bare"
    );
}

#[test]
fn the_shower_stops_where_the_hand_stopped() {
    let cfg = config();
    let g = geom();
    let cw = g.cw as f32;
    let ch = g.ch as f32;
    let col_of = |x: f32| (x / cw).floor() as i32;

    let floor = (6_u16, 28_u16);
    let (from, to) = ((6_u16, 42_u16), (6_u16, 28_u16));

    let run = |bounded: bool| {
        let t0 = Instant::now();
        let mut m = Meteors::new();
        if bounded {
            m.set_hand_floor(Some(floor));
        }
        let ctx = ctx_at(t0, &cfg, to, 0.30);
        m.on_event(&mv_as(from, to, Licence::Nav), t0, &ctx)
            .expect("a 14-cell keyed jump must fly");
        let mut sc = Scratch::default();
        let mut ink_min = i32::MAX;
        let mut ink_max = i32::MIN;
        let mut station_min = i32::MAX;
        let mut seen = 0_usize;
        for f in 0..45_u64 {
            let at = t0 + ms(f * 16);
            let ctx = ctx_at(at, &cfg, to, 0.30);
            sc.emit(&mut m, &ctx);
            let mut sp = Scratch::default();
            {
                let mut fr = sp.frame();
                for l in &m.landings {
                    draw_sparks(l, &ctx, &mut fr, SPARK_HALO_CAP);
                }
            }
            for x in sp
                .out
                .iter()
                .map(|q| f32::from(q.x))
                .chain(sp.halos.iter().map(|q| f32::from(q.x)))
            {
                seen += 1;
                ink_min = ink_min.min(col_of(x));
            }
            for q in &sp.out {
                ink_max = ink_max.max(col_of(f32::from(q.x) + f32::from(q.w)));
            }
            for l in &m.landings {
                let age = ms_since(l.pin.at, ctx.now);
                for s in l.spark.iter().take(usize::from(l.sparks)) {
                    if let Some(((dx, _), _)) = spark_at(*s, age, ch) {
                        station_min = station_min.min(col_of(l.x + dx));
                    }
                }
            }
        }
        (ink_min, ink_max, station_min, seen)
    };

    let (ink_min, ink_max, station_min, seen) = run(true);
    assert!(
        seen > 0,
        "the shower drew nothing at all — nothing is pinned"
    );

    assert!(
        ink_min >= 27,
        "the shower painted column {ink_min}: the hand's floor is column 28 and a star \
         centred on it spills no further than 27"
    );
    assert!(
        station_min >= 28,
        "a spark came to rest in column {station_min}, left of the hand's floor (28)"
    );

    assert!(
        ink_max >= 32,
        "the bounded shower reached only column {ink_max} — it is a wall, not a shower"
    );

    let (free_min, _, free_station, free_seen) = run(false);
    assert_eq!(free_seen, seen, "the wall must not cost the shower a spark");
    // A spark stands at one point of its throw (spread over the shower's area), so the free
    // shower's leftmost spark stands short of where the leftmost thrown one was last seen
    // (column 18 when they flew).
    assert!(
        free_min <= 22 && free_station <= 22,
        "with no floor the shower reached column {free_min} (stations {free_station}), \
         not six cells past the floor"
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PoolStep {
    Arrive,
    Ring,
    Expire,
    Reset,
}

impl PoolStep {
    const ALL: [Self; 4] = [Self::Arrive, Self::Ring, Self::Expire, Self::Reset];

    const fn action(self) -> &'static str {
        match self {
            Self::Arrive | Self::Ring => "Land",
            Self::Expire => "ExpireOne",
            Self::Reset => "Reset",
        }
    }
}

struct Pool {
    m: Meteors,
    now: Instant,
    arrivals: Vec<Instant>,
    at_right: bool,
}

const POOL_TICK: Duration = Duration::from_micros(1);

type PoolPatch = fn(&mut Meteors, PoolStep, &[Landing]);

const fn no_pool_patch(_: &mut Meteors, _: PoolStep, _: &[Landing]) {}

fn drop_the_arrival_at(site: PoolStep, m: &mut Meteors, step: PoolStep, before: &[Landing]) {
    if step == site && before.len() == LANDING_POOL {
        m.landings = before.to_vec();
    }
}

fn drop_the_arrival_in_emit(m: &mut Meteors, step: PoolStep, before: &[Landing]) {
    drop_the_arrival_at(PoolStep::Arrive, m, step, before);
}

fn drop_the_arrival_in_party(m: &mut Meteors, step: PoolStep, before: &[Landing]) {
    drop_the_arrival_at(PoolStep::Ring, m, step, before);
}

impl Pool {
    fn new() -> Self {
        Self {
            m: Meteors::new(),
            now: Instant::now(),
            arrivals: Vec::new(),
            at_right: false,
        }
    }

    fn drive(&mut self, step: PoolStep, patch: PoolPatch) -> Result<bool, String> {
        let cfg = config();
        let before = self.m.landings.clone();
        match step {
            PoolStep::Arrive => {
                let (from, to) = if self.at_right {
                    ((5, 30), (5, 10))
                } else {
                    ((5, 10), (5, 30))
                };
                let t0 = self.now + POOL_TICK;
                let arrival = t0 + flight(20.0);
                if self.m.landings.iter().any(|l| l.end() <= arrival) {
                    return Ok(false);
                }
                let spawn = self
                    .m
                    .on_event(&mv(from, to), t0, &ctx_at(t0, &cfg, to, 0.3))
                    .ok_or("a 20-cell same-row jump must fly")?;
                if spawn.t0 + spawn.t_flight != arrival {
                    return Err(format!("the flight lands at {:?}", spawn.t_flight));
                }
                Scratch::default().emit(&mut self.m, &ctx_at(arrival, &cfg, to, 0.3));
                self.at_right = !self.at_right;
                self.now = arrival;
                self.arrivals.push(arrival);
            }
            PoolStep::Ring => {
                self.now += POOL_TICK;
                let at = self.now;
                self.m.party(at, 11, true, &ctx_at(at, &cfg, (5, 40), 0.3));
                self.arrivals.push(at);
            }
            PoolStep::Expire => {
                let oldest = self.m.landings.first().ok_or("nothing to expire")?;
                self.now = self.now.max(oldest.end());
                Scratch::default().emit(&mut self.m, &ctx_at(self.now, &cfg, (5, 40), 0.3));
            }
            PoolStep::Reset => self.m.reset(),
        }
        patch(&mut self.m, step, &before);
        Ok(true)
    }

    fn project(&self) -> aterm_spec::interp::State {
        let ordinal = |l: &Landing| {
            self.arrivals
                .iter()
                .position(|&t| t == l.pin.at)
                .map_or(-1, |i| i64::try_from(i).unwrap_or(i64::MAX) + 1)
        };
        BTreeMap::from([
            (
                "resident",
                i64::try_from(self.m.landings.len()).unwrap_or(i64::MAX),
            ),
            ("newest", self.m.landings.last().map_or(0, ordinal)),
            ("oldest", self.m.landings.first().map_or(0, ordinal)),
            (
                "issued",
                i64::try_from(self.arrivals.len()).unwrap_or(i64::MAX),
            ),
        ])
    }
}

#[derive(Default)]
struct PoolCoverage {
    states: usize,
    actions: std::collections::BTreeSet<&'static str>,
    saturated_sites: std::collections::BTreeSet<&'static str>,
}

fn pool_walk(model: &aterm_spec::derive::Model, patch: PoolPatch) -> Result<PoolCoverage, String> {
    let init = model.init_state();
    if Pool::new().project() != init {
        return Err(format!("an empty pool is not the model's Init {init:?}"));
    }
    let mut seen = std::collections::BTreeSet::from([init.clone()]);
    let mut queue = std::collections::VecDeque::from([(init, Vec::<PoolStep>::new())]);
    let mut coverage = PoolCoverage::default();
    while let Some((state, path)) = queue.pop_front() {
        coverage.states += 1;
        for step in PoolStep::ALL {
            let action = step.action();
            let expected = model.successors(action, &state);
            if expected.is_empty() {
                continue;
            }
            let mut pool = Pool::new();
            for &earlier in &path {
                if !pool.drive(earlier, patch)? {
                    return Err(format!("{path:?} no longer replays at {earlier:?}"));
                }
            }
            if !pool.drive(step, patch)? {
                continue;
            }
            let after = pool.project();
            if expected != [after.clone()] {
                return Err(format!(
                    "{}: real {step:?} after {path:?} took {state:?} to {after:?}, \
                     the model's {action} to {expected:?}",
                    model.name
                ));
            }
            coverage.actions.insert(action);
            if action == "Land" && state["resident"] == 3 {
                coverage.saturated_sites.insert(match step {
                    PoolStep::Arrive => "emit",
                    _ => "party",
                });
            }
            if seen.insert(after.clone()) {
                let mut next = path.clone();
                next.push(step);
                queue.push_back((after, next));
            }
        }
    }
    Ok(coverage)
}

#[test]
fn the_landing_pool_conforms_to_the_model() {
    let model = aterm_models_effects::rainbow_landing_pool_model();
    assert_eq!(
        model
            .consts
            .iter()
            .find(|(name, _)| *name == "Cap")
            .map(|(_, cap)| *cap),
        Some(i64::try_from(LANDING_POOL).unwrap()),
        "the model's capacity is the shipped pool's"
    );
    let coverage = pool_walk(&model, no_pool_patch).unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(
        coverage.states,
        aterm_spec::interp::bmc(&model).expect("the healthy model holds"),
        "the real pool must reach every state the model can"
    );
    assert_eq!(
        coverage.actions,
        model.actions.iter().map(|a| a.name).collect(),
        "every model action must be driven on the real pool"
    );
    assert_eq!(
        coverage.saturated_sites,
        ["emit", "party"].into_iter().collect(),
        "both admission sites must be driven into a full pool"
    );

    for (site, patch) in [
        ("emit", drop_the_arrival_in_emit as PoolPatch),
        ("party", drop_the_arrival_in_party),
    ] {
        let refused = pool_walk(&model, patch)
            .err()
            .unwrap_or_else(|| panic!("the healthy model must refuse a dropped arrival in {site}"));
        assert!(refused.contains("the model's Land"), "{site}: {refused}");
    }
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let shipped = pool_walk(&buggy, no_pool_patch)
        .err()
        .expect("the shipped pool must not conform to the drop-the-arrival model");
    assert!(shipped.contains("the model's Land"), "{shipped}");
}

#[test]
fn meteors_hand_floor_and_reduced_follow_scroll_band_and_reset() {
    let mut m = Meteors::new();
    m.set_hand_floor(Some((5, 4)));
    m.translate_scroll(2, 18);
    assert_eq!(m.hand_floor_cell(), Some((3, 4)));

    m.translate_band(2, 6, 1, 18, 0);
    assert_eq!(m.hand_floor_cell(), Some((4, 4)));

    m.translate_band(2, 6, -3, 18, 0);
    assert_eq!(m.hand_floor_cell(), None);

    m.set_hand_floor(Some((2, 3)));
    m.reduced = true;
    m.reset();
    assert_eq!(m.hand_floor_cell(), None);
    assert!(!m.reduced);
}

/// The trail gallery's glass: 72x14 cells of 14x26 px from (12, 2).
const fn gallery_geom() -> Geom {
    Geom {
        cw: 14,
        ch: 26,
        rows: 14,
        cols: 72,
        origin_x: 12,
        origin_y: 2,
        win_w: 1032,
        win_h: 378,
        head: 0,
    }
}

/// `sc`'s streams laid on a `page` the size of `g`'s window as the
/// renderer lays them: the under quads, the over quads, then the halos
/// (added, then laid over).
fn composite_scratch(sc: &Scratch, g: Geom, page: u32) -> Vec<u32> {
    let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
    let mut px = vec![page; w * h];
    for q in sc.under.iter().chain(&sc.out) {
        let qw = u32::from(q.w.max(1));
        let step =
            |i: u32, a: u32, b: u32| (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw);
        for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
            for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                let i = (x - usize::from(q.x)) as u32;
                let (color, alpha) = if q.is_flat() {
                    (q.color, q.alpha)
                } else {
                    let lerp =
                        |sh: u32| step(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh;
                    (
                        lerp(16) | lerp(8) | lerp(0),
                        step(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                    )
                };
                let slot = &mut px[y * w + x];
                *slot = aterm_render::over_premul(*slot, color, alpha);
            }
        }
    }
    for mode in [HaloMode::Add, HaloMode::Over] {
        for q in sc.halos.iter().filter(|q| q.mode == mode) {
            let over_cap = aterm_render::halo_over_cap(q.color);
            let (cx, cy) = (i32::from(q.cx), i32::from(q.cy));
            let rx2 = i32::from(q.rx) * i32::from(q.rx);
            let ry2 = i32::from(q.ry) * i32::from(q.ry);
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                if ny >= 256 {
                    continue;
                }
                for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                    let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2).min(255);
                    if wt <= 0 {
                        continue;
                    }
                    let slot = &mut px[y * w + x];
                    *slot = match mode {
                        HaloMode::Add => {
                            aterm_render::add_sat(*slot, premul_rgb(q.color, wt as u8))
                        }
                        HaloMode::Over => {
                            aterm_render::over_rgb(*slot, q.color, (wt as u8).min(over_cap))
                        }
                    };
                }
            }
        }
    }
    px
}

/// The trail gallery's `enter-bottom` Return on `cfg`'s page at `after`
/// ms: from the end of the echo row (11, 26) to the new prompt (13, 2),
/// the rows above lettered as the gallery's are.
fn gallery_return(cfg: &Config, after: u64, keep: Option<i32>, in_guard: bool) -> (Scratch, Geom) {
    let g = gallery_geom();
    let t0 = Instant::now();
    let ctx_g = |now: Instant| Ctx {
        geom: g,
        ..ctx_at(now, cfg, (13, 2), 0.25)
    };
    let mut m = Meteors::new();
    m.trains_in_guard = in_guard;
    if let Some(past) = keep {
        let (ox, oy, cw, ch) = (12, 2, 14, 26);
        // The echo row's letters (its last at column 25) and the printed
        // line's (its last at column 18), a dark train's span reaching
        // `past` cells past them.
        m.keep_out.set_rows_past(
            [
                (ox, ox + 26 * cw, oy + 11 * ch, oy + 12 * ch),
                (ox, ox + 19 * cw, oy + 12 * ch, oy + 13 * ch),
            ],
            past * cw,
        );
    }
    m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
        .expect("a Return flies");
    let mut sc = Scratch::default();
    sc.emit(&mut m, &ctx_g(t0 + ms(after)));
    (sc, g)
}

/// After a dark `enter-bottom` Return the echo row's old caret cell
/// (column 26) keeps the page's dark across the flight, since the line's
/// kept span reaches the cell its caret stood in
/// (`CursorGlow::keep_out_boxes`), whether or not the train also keeps
/// clear of the letter guard's reach (`DARK_TRAIN_TEXT_PAD_PX`); with the
/// span cut at the letters and the train laid into the guard's reach (the
/// negative control) a brown stub stands there. The reach is a dark span's
/// alone ([`KeepOut::past`]): on paper the stick crosses that cell (looks7 M2).
#[test]
fn a_return_leaves_no_stub_at_the_cell_its_line_ended_in() {
    let mut cfg = config();
    cfg.theme_bg = 0x0011_1318;
    let (x0, x1) = (12 + 26 * 14 - 4, 12 + 27 * 14);
    let (y0, y1) = (2 + 11 * 26, 2 + 12 * 26);
    let lift = |past: i32, in_guard: bool| -> i32 {
        let mut peak = 0;
        for after in [20u64, 35, 60, 90, 120, 156] {
            let (sc, g) = gallery_return(&cfg, after, Some(past), in_guard);
            let px = composite_scratch(&sc, g, cfg.theme_bg);
            let w = usize::from(g.win_w);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = px[y as usize * w + x as usize];
                    for sh in [16, 8, 0] {
                        let d = ((p >> sh) & 0xff) as i32 - ((cfg.theme_bg >> sh) & 0xff) as i32;
                        peak = peak.max(d);
                    }
                }
            }
        }
        peak
    };
    let kept = lift(1, false);
    let stub = lift(0, true);
    assert!(
        kept <= 12,
        "the old caret cell lifts by {kept} across the flight"
    );
    assert!(
        stub >= 30,
        "control: cut at the letters the stub lifts only {stub}"
    );
}

/// Along the gallery's dark `enter-bottom` Return, every column whose
/// brightest pixel is a warm hue holds it at a clean level, few of them
/// dark; laid at the other stops' level (the negative control), many are.
#[test]
fn a_dark_return_s_warm_stops_read_clean_not_brown() {
    let mut cfg = config();
    cfg.theme_bg = 0x0011_1318;
    let muddy = |dim: bool| -> (usize, usize) {
        let (mut muddy, mut warm) = (0usize, 0usize);
        for after in [20u64, 35, 60, 90, 120, 156, 200] {
            let g = gallery_geom();
            let t0 = Instant::now();
            let ctx_g = |now: Instant| Ctx {
                geom: g,
                ..ctx_at(now, &cfg, (13, 2), 0.25)
            };
            let mut m = Meteors::new();
            m.warm_stops_plain = dim;
            m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
                .expect("a Return flies");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_g(t0 + ms(after)));
            let px = composite_scratch(&sc, g, cfg.theme_bg);
            let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
            for x in 0..w {
                let peak = (0..h)
                    .map(|y| px[y * w + x])
                    .max_by(|a, b| {
                        let v = |p: u32| crate::color_math::rgb2hsv(p).2;
                        v(*a).total_cmp(&v(*b))
                    })
                    .unwrap_or(cfg.theme_bg);
                let (hue, sat, val) = crate::color_math::rgb2hsv(peak);
                if val < 0.16 || sat < 0.3 || !(hue < 70.0 || hue >= 330.0) {
                    continue;
                }
                warm += 1;
                if val < 0.6 {
                    muddy += 1;
                }
            }
        }
        (muddy, warm)
    };
    let (clean, warm) = muddy(false);
    let (dim, dim_warm) = muddy(true);
    assert!(
        warm > 0 && dim_warm > 0,
        "fixture: the Return lays warm stops"
    );
    assert!(
        clean * 5 <= warm,
        "{clean} of {warm} warm columns of the Return read dark"
    );
    assert!(
        dim * 2 > dim_warm,
        "control: at the other stops' level only {dim} of {dim_warm} warm columns read dark"
    );
}

/// The 8-connected runs of lit pixels in `px` (a pixel off `page` by
/// more than `delta` in any channel), their sizes, largest first.
fn lit_runs(px: &[u32], w: usize, page: u32, delta: i32) -> Vec<usize> {
    let lit = |p: u32| {
        [16u32, 8, 0]
            .iter()
            .any(|&sh| (((p >> sh) & 0xff) as i32 - ((page >> sh) & 0xff) as i32).abs() > delta)
    };
    let h = px.len() / w;
    let mut seen = vec![false; px.len()];
    let mut runs = Vec::new();
    for start in 0..px.len() {
        if seen[start] || !lit(px[start]) {
            continue;
        }
        let (mut n, mut stack) = (0usize, vec![start]);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 {
                        continue;
                    }
                    let k = yy as usize * w + xx as usize;
                    if !seen[k] && lit(px[k]) {
                        seen[k] = true;
                        stack.push(k);
                    }
                }
            }
        }
        runs.push(n);
    }
    runs.sort_unstable_by(|a, b| b.cmp(a));
    runs
}

/// The gallery's judges (looks7 M2, after looks6 round 3): on paper the
/// `enter-bottom` Return's stick was cut in two over the printed line's
/// blank cell after `fox` (the line's span held a dark train behind the
/// cell after its last letter, and the paper train with it), thinned to
/// beads on olive necks through its yellow, and its fades laid its
/// darkened amber thin over the paper, tan and khaki (`b5a053`,
/// `cf9859`, `d3a46b`), as its thread's fade still laid its gold
/// (`c79b58`). Along the gallery's Return the stick is one run
/// of lit pixels whose width rises to one peak and falls from it (a
/// pixel of anti-aliasing aside), and no column's core (its most
/// saturated pixel) reads tan or khaki, nor olive but in the one column
/// where the walk steps from gold to green (looks8 round 2's K10b: it
/// read olive for three). Negative controls: the span held a cell past
/// the letters cuts the stick in two; the fades laid as dims read tan;
/// the caret's bend walks through the olives; blended across the
/// bend's skip, the turn reads olive for columns. The laws hold on the
/// stick as it shows under the printed line ([`paper_floor`]) and laid
/// whole over it, and the controls are judged on the whole stick.
#[test]
fn a_paper_return_lays_one_clean_stick() {
    let mut cfg = config();
    cfg.dark_theme = false;
    cfg.theme_fg = 0x001F_2328;
    cfg.theme_bg = 0x00FF_FFFF;
    let page = cfg.theme_bg;
    // (most runs, worst pinch px, tan cores, widest olive crossing in
    // columns) over the flight.
    let stick = |past_cell: bool,
                 dims: bool,
                 plain: bool,
                 blended: bool,
                 floorless: bool|
     -> (usize, usize, usize, usize) {
        let (mut runs, mut pinch, mut tan, mut olive) = (0usize, 0usize, 0usize, 0usize);
        for after in [20u64, 35, 60, 90, 120, 156] {
            let g = gallery_geom();
            let t0 = Instant::now();
            let ctx_g = |now: Instant| Ctx {
                geom: g,
                ..ctx_at(now, &cfg, (13, 2), 0.25)
            };
            let mut m = Meteors::new();
            m.dims_dark_trains = dims;
            m.warm_stops_plain = plain;
            m.train_skip_blended = blended;
            m.paper_trains_floorless = floorless;
            let (ox, oy, cw, ch) = (12, 2, 14, 26);
            let reach = i32::from(past_cell);
            m.keep_out.set_rows([
                (ox, ox + (26 + reach) * cw, oy + 11 * ch, oy + 12 * ch),
                (ox, ox + (19 + reach) * cw, oy + 12 * ch, oy + 13 * ch),
            ]);
            m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
                .expect("a Return flies");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_g(t0 + ms(after)));
            // The stick alone: the landing's marks are the sky's.
            sc.out.clear();
            sc.halos.clear();
            let px = composite_scratch(&sc, g, page);
            let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
            runs = runs.max(lit_runs(&px, w, page, 24).len());
            let mut widths = Vec::new();
            let mut crossing = 0usize;
            for x in 0..w {
                let (mut n, mut core) = (0usize, None::<(f32, u32)>);
                for y in 0..h {
                    let p = px[y * w + x];
                    let lit = [16u32, 8, 0].iter().any(|&sh| {
                        (((p >> sh) & 0xff) as i32 - ((page >> sh) & 0xff) as i32).abs() > 24
                    });
                    if !lit {
                        continue;
                    }
                    n += 1;
                    let sat = crate::color_math::rgb2hsv(p).1;
                    if core.is_none_or(|(s, _)| sat > s) {
                        core = Some((sat, p));
                    }
                }
                widths.push(n);
                if let Some((sat, p)) = core {
                    let (hue, _, val) = crate::color_math::rgb2hsv(p);
                    // A tint of ochre: a warm mid value, half grey.
                    if (20.0..50.0).contains(&hue) && (0.2..0.6).contains(&sat) && val < 0.86 {
                        tan += 1;
                    }
                    if (48.0..80.0).contains(&hue) && val < 0.85 {
                        crossing += 1;
                        olive = olive.max(crossing);
                    } else {
                        crossing = 0;
                    }
                } else {
                    crossing = 0;
                }
            }
            // Away from its widest column, each column is no wider than
            // the last but by a pixel of anti-aliasing; a pinch is how
            // far it narrows before it widens again.
            let Some(peak) = (0..w).max_by_key(|&x| widths[x]) else {
                continue;
            };
            for side in [-1i32, 1] {
                let mut least = widths[peak];
                let mut x = peak as i32 + side;
                while x >= 0 && (x as usize) < w && widths[x as usize] > 0 {
                    let here = widths[x as usize];
                    least = least.min(here);
                    pinch = pinch.max(here.saturating_sub(least + 1));
                    x += side;
                }
            }
        }
        (runs, pinch, tan, olive)
    };
    let mut whole = (0, 0, 0, 0);
    for floorless in [false, true] {
        let (runs, pinch, tan, olive) = stick(false, false, false, false, floorless);
        assert_eq!(runs, 1, "the paper Return's stick falls into {runs} runs");
        assert_eq!(pinch, 0, "the stick pinches by {pinch} px and widens again");
        assert_eq!(tan, 0, "{tan} columns of the stick read tan at their core");
        assert!(
            olive <= 1,
            "the stick reads olive across {olive} columns in a row"
        );
        whole = (runs, pinch, tan, olive);
    }
    let olive = whole.3;
    let (_, _, _, blended) = stick(false, false, false, true, true);
    assert!(
        blended >= olive + 2,
        "control: blended across the bend's skip the stick reads olive across only \
         {blended} columns ({olive})"
    );
    let (cut, ..) = stick(true, false, false, false, true);
    assert!(
        cut >= 2,
        "control: held a cell past the letters the stick is one run"
    );
    let (_, _, dim_tan, _) = stick(false, true, false, false, true);
    assert!(
        dim_tan >= 10,
        "control: its fades laid as dims read tan in only {dim_tan} columns"
    );
    let (_, _, _, bent) = stick(false, false, true, false, true);
    assert!(
        bent >= olive + 2,
        "control: the caret's bend reads olive across only {bent} columns ({olive})"
    );
}

/// The gallery's judges (looks9 Q5): on paper the `enter-bottom`
/// Return's stick floated a rainbow dash past the printed line's `fox`
/// in flight (`enter`, x 290..377 y 318..330) and, landed, grazed the
/// foot of its `x` (x 262..276 y 334..335). Over the flight and its
/// fade the paper stick lays no light on the printed line or above it,
/// and below it is one run, which shows once the head is under the line.
/// Negative control: laid over the rows it falls past, as it was, it
/// lays light on the printed line in flight.
#[test]
fn a_paper_return_comes_out_from_under_the_printed_line() {
    let mut cfg = config();
    cfg.dark_theme = false;
    cfg.theme_fg = 0x001F_2328;
    cfg.theme_bg = 0x00FF_FFFF;
    let page = cfg.theme_bg;
    let (ox, oy, cw, ch) = (12, 2, 14, 26);
    // The printed line's bottom, window px.
    let printed_foot = usize::try_from(oy + 13 * ch).expect("on glass");
    // (light on or above the printed line, most runs below it, frames
    // that show the stick) over the flight.
    let lay = |floorless: bool| -> (usize, usize, usize) {
        let (mut above, mut runs, mut shown) = (0usize, 0usize, 0usize);
        for after in [0u64, 8, 16, 24, 35, 60, 90, 120, 156, 240] {
            let g = gallery_geom();
            let t0 = Instant::now();
            let ctx_g = |now: Instant| Ctx {
                geom: g,
                ..ctx_at(now, &cfg, (13, 2), 0.25)
            };
            let mut m = Meteors::new();
            m.paper_trains_floorless = floorless;
            m.keep_out.set_rows([
                (ox, ox + 26 * cw, oy + 11 * ch, oy + 12 * ch),
                (ox, ox + 19 * cw, oy + 12 * ch, oy + 13 * ch),
                (ox, ox + cw, oy + 13 * ch, oy + 14 * ch),
            ]);
            m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
                .expect("a Return flies");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_g(t0 + ms(after)));
            // The stick alone: the landing's marks are the sky's.
            sc.out.clear();
            sc.halos.clear();
            let px = composite_scratch(&sc, g, page);
            let w = usize::from(g.win_w);
            let lit = |p: u32| {
                [16u32, 8, 0].iter().any(|&sh| {
                    (((p >> sh) & 0xff) as i32 - ((page >> sh) & 0xff) as i32).abs() > 24
                })
            };
            above += px[..printed_foot * w].iter().filter(|&&p| lit(p)).count();
            let below = lit_runs(&px[printed_foot * w..], w, page, 24).len();
            runs = runs.max(below);
            shown += usize::from(below > 0);
        }
        (above, runs, shown)
    };
    let (above, runs, shown) = lay(false);
    assert_eq!(
        above, 0,
        "the paper stick lays {above} px on the printed line or above"
    );
    assert_eq!(
        runs, 1,
        "under the printed line the stick falls into {runs} runs"
    );
    assert!(shown >= 4, "the stick shows in only {shown} frames");
    let (over, ..) = lay(true);
    assert!(
        over >= 100,
        "control: laid over the rows it falls past the stick lays only {over} px on them"
    );
}

/// On a dark page a Return's landing lays no pin: its crosshair stood
/// on the new caret after the gallery's dark Return, a hairline left of
/// the block and a tick on its top (looks7 M3, `e120`). A jump's landing
/// (the negative control) pins its caret.
#[test]
fn a_dark_return_lands_without_a_pin() {
    let cfg = config();
    assert!(cfg.dark_theme, "fixture: the page is dark");
    let t0 = Instant::now();
    let pin = |licence: Licence| -> usize {
        let (from, to) = ((5u16, 40u16), (7u16, 2u16));
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, to, 0.25);
        let spawn = m
            .on_event(&mv_as(from, to, licence), t0, &ctx)
            .expect("a meteor flies");
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(t0 + spawn.t_flight + ms(4), &cfg, to, 0.25));
        let l = *m.landings.first().expect("fixture: the meteor landed");
        let mut n = 0;
        for after in [8u64, 20, 40, 80] {
            sc.clear();
            {
                let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, to, 0.25);
                let mut fr = sc.frame();
                draw_pin(&l, &at, &mut fr);
            }
            n += sc.out.len() + sc.halos.len();
        }
        n
    };
    let peel = pin(Licence::Return);
    assert_eq!(peel, 0, "a dark Return's landing lays {peel} pin lights");
    assert!(
        pin(Licence::Nav) > 0,
        "control: a jump's landing lays no pin"
    );
}

/// The gallery's judges (looks7 M1): after a dark Return in
/// `enter-bottom` coloured dabs floated in the gap under `brown fox`
/// and beside the train's head: the head's coma, seven lights orbiting
/// it apart, laid where the train passed behind the echo and the
/// printed line. Over the gallery's Return (its landing's marks aside)
/// the lit pixels are one connected run or none; ringed with its coma
/// and laid behind the letters' bodies alone (the negative control),
/// they fall apart.
#[test]
fn a_dark_return_lays_one_train_or_none() {
    let mut cfg = config();
    cfg.theme_bg = 0x0011_1318;
    let page = cfg.theme_bg;
    let runs = |coma: bool| -> (usize, usize) {
        let (mut worst, mut lit) = (0usize, 0usize);
        for after in [10u64, 20, 35, 50, 70, 90, 120, 156, 200] {
            let g = gallery_geom();
            let t0 = Instant::now();
            let ctx_g = |now: Instant| Ctx {
                geom: g,
                ..ctx_at(now, &cfg, (13, 2), 0.25)
            };
            let mut m = Meteors::new();
            m.peel_coma = coma;
            let (ox, oy, cw, ch) = (12, 2, 14, 26);
            m.keep_out.set_rows_past(
                [
                    (ox, ox + 26 * cw, oy + 11 * ch, oy + 12 * ch),
                    (ox, ox + 19 * cw, oy + 12 * ch, oy + 13 * ch),
                ],
                cw,
            );
            m.on_event(&mv_as((11, 26), (13, 2), Licence::Return), t0, &ctx_g(t0))
                .expect("a Return flies");
            // The flight alone: the landing's marks are the landing's.
            for me in &mut m.live {
                me.landed = true;
            }
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_g(t0 + ms(after)));
            let px = composite_scratch(&sc, g, page);
            let n = lit_runs(&px, usize::from(g.win_w), page, 24).len();
            worst = worst.max(n);
            lit += usize::from(n > 0);
        }
        (worst, lit)
    };
    let (worst, lit) = runs(false);
    assert!(lit >= 4, "fixture: the Return shows in only {lit} frames");
    assert!(
        worst <= 1,
        "the dark Return's light falls into {worst} runs"
    );
    let (ringed, _) = runs(true);
    assert!(
        ringed >= 3,
        "control: ringed with its coma it lays only {ringed} runs"
    );
}
