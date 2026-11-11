// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::color_math::contrast_ratio;

const TAU: f32 = MOMENTUM_GLOW_TAU_S;

fn geom() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 800,
        win_h: 480,
        head: 0,
    }
}

fn on(base: u32, dark: bool) -> MomentumGlowConfig {
    MomentumGlowConfig {
        light: crate::cursor_glow::Light::OFF,
        enabled: true,
        intensity: 1.0,
        tau_s: TAU,
        radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
        base,
        dark_theme: dark,
        hue: base,
        theme_fg: if dark { 0x00C8_C8D2 } else { 0x0024_292F },
        theme_bg: if dark { 0x0010_1016 } else { 0x00FF_FFFF },
        block: true,
        bar: false,
        glyph_span: None,
        light_vivid: false,
        chosen_hue: false,
        chosen_base: false,
    }
}

fn charged(keys: u32, cell: (u16, u16), g0: Geom, cfg: &MomentumGlowConfig) -> Vec<GlowQuad> {
    let t0 = Instant::now();
    let mut g = MomentumGlow::default();
    for _ in 0..keys {
        g.on_key(t0, TAU);
    }
    let (mut out, mut under) = (Vec::new(), Vec::new());
    g.tick(Some(cell), t0, g0, cfg, &mut out, &mut under);
    out.extend(under);
    out
}

fn composite(quads: &[GlowQuad], ground: u32, w: usize, h: usize) -> Vec<u32> {
    let mut px = vec![ground; w * h];
    for q in quads {
        let qw = u32::from(q.w);
        for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
            for i in 0..qw {
                let x = usize::from(q.x) + i as usize;
                if x >= w {
                    break;
                }
                let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                let a =
                    aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, qw) as u8;
                px[y * w + x] = aterm_render::over_premul(px[y * w + x], c, a) & 0x00FF_FFFF;
            }
        }
    }
    px
}

fn spent_per_pixel(quads: &[GlowQuad], w: usize, h: usize) -> Vec<u32> {
    let mut px = vec![0u32; w * h];
    for q in quads {
        let qw = u32::from(q.w);
        for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
            for i in 0..qw {
                let x = usize::from(q.x) + i as usize;
                if x >= w {
                    break;
                }
                px[y * w + x] += if q.alpha == 0 {
                    let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                    ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff)
                } else {
                    aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, qw)
                };
            }
        }
    }
    px
}

#[test]
fn the_wake_deadline_never_outlives_what_the_glass_draws() {
    let t0 = Instant::now();
    let lane = Duration::from_millis(16);
    let mut glow = MomentumGlow::default();
    let mut cfg = on(0x0000_0000, true);
    let mut out: Vec<GlowQuad> = Vec::new();

    let last = script(&mut glow.rate, t0, 12.0, 2.0);
    assert!(
        glow.rate.value(last, cfg.tau_s) >= MOMENTUM_GLOW_HOT,
        "fixture: the raw rate is hot"
    );

    out.clear();
    let full = glow.tick(Some((1, 1)), last, geom(), &cfg, &mut out, &mut Vec::new());
    assert!(full.hot, "fixture: at intensity 1 it is on glass");
    assert!(
        glow.next_change_deadline(last, lane).is_some(),
        "and the host is asked for the next frame"
    );

    cfg.intensity = MOMENTUM_GLOW_HOT / glow.rate.value(last, cfg.tau_s) * 0.5;
    out.clear();
    let dim = glow.tick(Some((1, 1)), last, geom(), &cfg, &mut out, &mut Vec::new());
    assert!(
        !dim.hot && out.is_empty(),
        "fixture: scaled below the floor the effect is inert"
    );
    assert!(
        glow.next_change_deadline(last, lane).is_none() && !glow.settling(),
        "an inert effect asks for no frame"
    );
}

fn script(r: &mut TypingRate, t0: Instant, rate: f32, secs: f32) -> Instant {
    let gap = Duration::from_secs_f32(1.0 / rate);
    let mut t = t0;
    let n = (rate * secs) as usize;
    for k in 0..n {
        if k > 0 {
            t += gap;
        }
        r.on_key(t, TAU);
    }
    t
}

#[test]
fn rises_with_key_rate() {
    let t0 = Instant::now();
    let mut vals = Vec::new();
    for rate in [1.0f32, 2.0, 4.0, 8.0] {
        let mut r = TypingRate::default();
        let end = script(&mut r, t0, rate, 3.0);
        vals.push(r.value(end, TAU));
    }
    for w in vals.windows(2) {
        assert!(w[1] > w[0] + 0.05, "faster typing must glow more: {vals:?}");
    }
    assert!(vals[3] >= 0.9, "8 keys/s is white-hot: {vals:?}");
    assert!(vals[0] <= 0.25, "1 key/s is a warm ember: {vals:?}");
}

#[test]
fn cooldown_is_monotone_and_snaps_to_exact_zero() {
    let t0 = Instant::now();
    let mut g = MomentumGlow::default();
    for _ in 0..40 {
        g.on_key(t0, TAU);
    }
    assert!((g.value(t0, TAU) - 1.0).abs() < 1e-6);
    let mut out = Vec::new();
    assert!(
        g.tick(
            Some((1, 1)),
            t0,
            geom(),
            &on(0, true),
            &mut out,
            &mut Vec::new()
        )
        .hot
    );
    let analytic = g.cold_at().expect("a hot glow names its cold instant");
    let mut last = 1.0f32;
    let mut sampled_cold: Option<Instant> = None;
    for k in 1..=500 {
        let t = t0 + Duration::from_millis(16 * k);
        let v = g.value(t, TAU);
        assert!(
            v <= last + 1e-6,
            "cooling must be monotone at {k}: {v} > {last}"
        );
        if sampled_cold.is_none() && v < MOMENTUM_GLOW_HOT {
            sampled_cold = Some(t);
        }
        last = v;
    }
    assert_eq!(last, 0.0, "fully cooled is EXACTLY zero, not a drift");
    let half = g.value(t0 + Duration::from_secs_f32(TAU * 2f32.ln()), TAU);
    assert!((half - 0.5).abs() < 0.01, "half-life at τ·ln2: {half}");
    let sampled = sampled_cold.expect("it cools");
    let gap = sampled.saturating_duration_since(analytic).as_secs_f32();
    assert!(
        sampled >= analytic && gap < 0.02,
        "analytic crossing agrees with the sampled one ({gap} s)"
    );
}

#[test]
fn blink_is_suppressed_while_hot_and_returns_when_cold() {
    let t0 = Instant::now();
    let mut g = MomentumGlow::default();
    let mut out = Vec::new();
    assert!(
        !g.tick(
            Some((3, 3)),
            t0,
            geom(),
            &on(0x00A0_A0A0, true),
            &mut out,
            &mut Vec::new()
        )
        .hot
    );
    g.on_key(t0, TAU);
    let f = g.tick(
        Some((3, 3)),
        t0,
        geom(),
        &on(0x00A0_A0A0, true),
        &mut out,
        &mut Vec::new(),
    );
    assert!(f.hot, "one key makes the cursor hot");
    assert!(f.fill.is_some(), "a hot block carries the tint");
    assert!(!out.is_empty(), "and a halo is on glass");
    let later = t0 + Duration::from_secs(6);
    out.clear();
    let f = g.tick(
        Some((3, 3)),
        later,
        geom(),
        &on(0x00A0_A0A0, true),
        &mut out,
        &mut Vec::new(),
    );
    assert!(
        !f.hot && f.fill.is_none() && f.fp == 0,
        "cold is exactly the configured cursor"
    );
    assert!(out.is_empty(), "and nothing is drawn");
}

#[test]
fn absurd_rates_never_nan_or_exceed_one() {
    let t0 = Instant::now();
    let mut r = TypingRate::default();
    for _ in 0..100_000 {
        r.on_key(t0, TAU);
    }
    let v = r.value(t0, TAU);
    assert!(v.is_finite() && (0.0..=1.0).contains(&v));
    let mut t = t0;
    for _ in 0..10_000 {
        t += Duration::from_nanos(1);
        r.on_key(t, TAU);
    }
    assert!((r.value(t, TAU) - 1.0).abs() < 1e-6);
    let decade = t + Duration::from_secs(10 * 365 * 24 * 3600);
    assert_eq!(r.value(decade, TAU), 0.0);
    assert_eq!(r.value(t, f32::NAN), 0.0, "garbage τ is inert");
    assert_eq!(r.value(t, 0.0), 0.0);
    let mut g = MomentumGlow::default();
    g.on_key(t, TAU);
    let mut cfg = on(0x00FF_FFFF, true);
    cfg.intensity = f32::NAN;
    let mut out = Vec::new();
    let f = g.tick(Some((1, 1)), t, geom(), &cfg, &mut out, &mut Vec::new());
    assert_eq!(f, MomentumGlowFrame::default());
    assert!(out.is_empty());
}

#[test]
fn deterministic_given_timestamps() {
    let t0 = Instant::now();
    let (mut a, mut b) = (MomentumGlow::default(), MomentumGlow::default());
    let keys: Vec<Instant> = (0..30)
        .map(|k| t0 + Duration::from_millis(90 * k))
        .collect();
    for &k in &keys {
        a.on_key(k, TAU);
        b.on_key(k, TAU);
    }
    let mut out_a = Vec::new();
    let mut out_b = Vec::new();
    let end = keys[keys.len() - 1];
    let mut fa = MomentumGlowFrame::default();
    let mut fb = MomentumGlowFrame::default();
    for k in 0..240u64 {
        let t = end + Duration::from_millis(16 * k);
        out_a.clear();
        fa = a.tick(
            Some((2, 2)),
            t,
            geom(),
            &on(0x0080_80FF, true),
            &mut out_a,
            &mut Vec::new(),
        );
        if k % 8 == 0 {
            out_b.clear();
            fb = b.tick(
                Some((2, 2)),
                t,
                geom(),
                &on(0x0080_80FF, true),
                &mut out_b,
                &mut Vec::new(),
            );
            assert_eq!(
                fa, fb,
                "sampling cadence must not change the value at a shared instant"
            );
            assert_eq!(out_a, out_b);
        }
    }
    assert!(fa.fp != 0 || fb.fp == 0);
}

#[test]
fn off_is_byte_identical() {
    let t0 = Instant::now();
    let mut g = MomentumGlow::default();
    g.on_key(t0, TAU);
    let mut out = vec![GlowQuad {
        row: 0,
        x: 1,
        y: 1,
        w: 1,
        h: 1,
        color: 7,
        alpha: 0,
        color2: 7,
        alpha2: 0,
    }];
    let sentinel = out.clone();
    let mut off = on(0x00FF_FFFF, true);
    off.enabled = false;
    assert_eq!(
        g.tick(Some((1, 1)), t0, geom(), &off, &mut out, &mut Vec::new()),
        MomentumGlowFrame::default()
    );
    let mut zero = on(0x00FF_FFFF, true);
    zero.intensity = 0.0;
    assert_eq!(
        g.tick(Some((1, 1)), t0, geom(), &zero, &mut out, &mut Vec::new()),
        MomentumGlowFrame::default()
    );
    assert_eq!(
        g.tick(
            None,
            t0,
            geom(),
            &on(0x00FF_FFFF, true),
            &mut out,
            &mut Vec::new()
        ),
        MomentumGlowFrame::default()
    );
    assert_eq!(
        g.tick(
            Some((99, 1)),
            t0,
            geom(),
            &on(0x00FF_FFFF, true),
            &mut out,
            &mut Vec::new()
        ),
        MomentumGlowFrame::default()
    );
    assert_eq!(out, sentinel, "an inert tick touches nothing");
    assert!(!g.is_active() && !g.settling());
}

#[test]
fn halo_hugs_the_cell_in_bounded_quads() {
    let g0 = geom();
    let out = charged(40, (10, 40), g0, &on(0x00FF_FFFF, true));
    assert!(!out.is_empty());
    let cx = (40.0 + 0.5) * g0.cw as f32;
    let reach = g0.cw as f32 * (0.55 + MOMENTUM_GLOW_RADIUS_CELLS) + 1.0;
    for q in &out {
        assert!(u32::from(q.x) + u32::from(q.w) <= u32::from(g0.win_w));
        assert!(u32::from(q.y) + u32::from(q.h) <= u32::from(g0.win_h));
        assert!(
            (f32::from(q.x) - cx).abs() <= reach + 1.0,
            "quad at {} strays past {reach}",
            q.x
        );
        assert!((f32::from(q.x + q.w) - cx).abs() <= reach + 1.0);
        assert_eq!((q.alpha, q.alpha2), (0, 0), "dark: additive light");
    }
    let big = Geom {
        cw: 20,
        ch: 40,
        rows: 50,
        cols: 200,
        win_w: 4000,
        win_h: 2000,
        ..g0
    };
    let n = charged(40, (25, 100), big, &on(0x00FF_FFFF, true)).len();
    assert!(n < 400, "bounded halo geometry, got {n} quads");
    assert_ne!(ramp(1.0, false), 0x00FF_FFFF);
    assert_eq!(ramp(1.0, true), 0x00FF_FFFF);
}

#[test]
fn no_pixel_carries_more_than_the_over_ink_budget() {
    let padded = Geom {
        cw: 15,
        ch: 28,
        rows: 24,
        cols: 60,
        origin_x: 24,
        origin_y: 84,
        win_w: 948,
        win_h: 780,
        head: 80,
    };
    let odd = Geom {
        cw: 9,
        ch: 19,
        rows: 30,
        cols: 90,
        win_w: 810,
        win_h: 570,
        ..geom()
    };
    let cap = OVER_INK_COV_CAP as u32;
    for g0 in [geom(), padded, odd] {
        let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
        let cell = (6u16, 20u16);
        for keys in [1u32, 3, 5, 8, 40] {
            for block in [true, false] {
                let cfg = MomentumGlowConfig {
                    block,
                    ..on(0x0050_FA7B, true)
                };
                let out = charged(keys, cell, g0, &cfg);
                let light = spent_per_pixel(&out, w, h);
                let max = light.iter().copied().max().unwrap_or(0);
                assert!(
                    max <= cap,
                    "{keys} keys, block {block}, cell {}x{}: {max} levels on one pixel",
                    g0.cw,
                    g0.ch
                );
            }
        }
        let out = charged(40, cell, g0, &on(0x0050_FA7B, true));
        let light = spent_per_pixel(&out, w, h);
        let row =
            (i32::from(g0.origin_y) + i32::from(cell.0) * g0.ch as i32 + g0.ch as i32 / 2) as usize;
        let cell_x0 = i32::from(g0.origin_x) + i32::from(cell.1) * g0.cw as i32;
        let beside = light[row * w + (cell_x0 - 1) as usize];
        assert!(
            beside * 10 >= cap * 7,
            "the budget is spent beside the caret: {beside} of {cap}"
        );
        let far = cell_x0 - (g0.cw as i32 * 3) / 2;
        assert!(
            light[row * w + far as usize] > 0,
            "the halo reaches a cell and a half past the caret"
        );
        let mut twice = out.clone();
        twice.extend_from_slice(&out);
        let max = spent_per_pixel(&twice, w, h).into_iter().max().unwrap_or(0);
        assert!(max > cap, "control: two halos stacked overrun it ({max})");
    }
}

#[test]
fn the_letter_beside_the_caret_keeps_the_over_ink_bar() {
    const INK: u32 = 0x00C8_D3F5;
    const GROUND: u32 = 0x0016_171D;
    let g0 = geom();
    let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
    let cell = (6u16, 20u16);
    let out = charged(40, cell, g0, &on(0x0050_FA7B, true));
    let x = usize::from(cell.1) * g0.cw - 2;
    let rows = usize::from(cell.0) * g0.ch..(usize::from(cell.0) + 1) * g0.ch;
    let worst = |quads: &[GlowQuad]| {
        let (ink, ground) = (composite(quads, INK, w, h), composite(quads, GROUND, w, h));
        rows.clone()
            .map(|y| contrast_ratio(ink[y * w + x], ground[y * w + x]))
            .fold(f32::MAX, f32::min)
    };
    let lit = worst(&out);
    assert!(lit >= 5.25, "the letter beside the caret reads {lit:.2}:1");
    let stacked: Vec<GlowQuad> = (0..4).flat_map(|_| out.iter().copied()).collect();
    let old = worst(&stacked);
    assert!(old < 5.25, "control: four budgets of light read {old:.2}:1");
}

#[test]
fn the_light_page_halo_is_a_source_over_veil_under_the_budget() {
    const INK: u32 = 0x0024_292F;
    const PAPER: u32 = 0x00FF_FFFF;
    let g0 = geom();
    let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
    let cell = (6u16, 20u16);
    let veil = charged(40, cell, g0, &on(0x0009_69DA, false));
    assert!(!veil.is_empty(), "a hot light-page caret still glows");
    assert!(
        veil.iter().all(|q| q.alpha > 0 && q.alpha2 > 0),
        "every light-page quad is source-over"
    );
    let spent = spent_per_pixel(&veil, w, h);
    assert!(spent.iter().all(|&a| a <= LIGHT_GLOW_PEAK.ceil() as u32));
    let ground = composite(&veil, PAPER, w, h);
    let deepened = ground.iter().filter(|&&px| px != PAPER).count();
    assert!(
        deepened > g0.cw * g0.ch,
        "the veil deepens the page around the caret"
    );
    let worst = ground
        .iter()
        .map(|&g| contrast_ratio(INK, g))
        .fold(f32::MAX, f32::min);
    assert!(worst >= 5.25, "ink over the glow reads {worst:.2}:1");
    let dark_arm = charged(40, cell, g0, &on(0x0009_69DA, true));
    assert!(
        dark_arm.iter().all(|q| q.alpha == 0),
        "control: the dark arm is additive light"
    );
}

#[test]
fn the_light_page_glow_is_one_round_glow_inside_the_caret_row() {
    use crate::color_math::rgb2hsv;
    const PAPER: u32 = 0x00FF_FFFF;
    let g0 = geom();
    let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
    let cell = (6u16, 20u16);
    let band = usize::from(cell.0) * g0.ch..(usize::from(cell.0) + 1) * g0.ch;
    let cell_x = usize::from(cell.1) * g0.cw..(usize::from(cell.1) + 1) * g0.cw;
    let edge_and_peak = |spent: &[u32]| {
        let row_max = |y: usize| (0..w).map(|x| spent[y * w + x]).max().unwrap_or(0);
        let edge = row_max(band.start).max(row_max(band.end - 1));
        (edge, spent.iter().copied().max().unwrap_or(0))
    };
    for keys in [3u32, 6, 40] {
        let glow = charged(keys, cell, g0, &on(0x0009_69DA, false));
        assert!(!glow.is_empty(), "{keys} keys: a hot caret glows");
        let ground = composite(&glow, PAPER, w, h);
        for y in (0..h).filter(|y| !band.contains(y)) {
            for x in 0..w {
                assert_eq!(
                    ground[y * w + x],
                    PAPER,
                    "{keys} keys: the glow left the caret row at ({x}, {y})"
                );
            }
        }
        let mid = band.start + g0.ch / 2;
        let deepened =
            |xs: std::ops::Range<usize>| xs.into_iter().any(|x| ground[mid * w + x] != PAPER);
        assert!(
            deepened(0..cell_x.start) && deepened(cell_x.end..w),
            "{keys} keys: the glow shows on both sides of the caret"
        );
        let (edge, peak) = edge_and_peak(&spent_per_pixel(&glow, w, h));
        assert!(
            edge * 6 <= peak,
            "{keys} keys: a cut edge at the row's rim ({edge} of {peak})"
        );
    }
    let dark_arm = charged(40, cell, g0, &on(0x0009_69DA, true));
    let cut: Vec<GlowQuad> = dark_arm
        .iter()
        .filter(|q| band.contains(&usize::from(q.y)))
        .copied()
        .collect();
    let (edge, peak) = edge_and_peak(&spent_per_pixel(&cut, w, h));
    assert!(
        edge * 6 > peak,
        "control: a taller profile cut at the row leaves an edge ({edge} of {peak})"
    );
    let deepest_hue = |hue: u32| {
        let ground = composite(&charged(40, cell, g0, &on(hue, false)), PAPER, w, h);
        let deepest = ground
            .iter()
            .copied()
            .min_by_key(|&px| (crate::color_math::relative_luminance(px) * 1e6) as u32)
            .unwrap_or(PAPER);
        rgb2hsv(deepest).0
    };
    let blue = deepest_hue(0x0009_69DA);
    let (want, _, _) = rgb2hsv(0x0009_69DA);
    assert!(
        (blue - want).abs() < 12.0,
        "the glow wears the style's hue: {blue:.0} vs {want:.0}"
    );
    let red = deepest_hue(0x00CF_222E);
    assert!(
        (red - blue).abs() > 90.0,
        "control: a red style's glow ({red:.0}) is not a blue one's ({blue:.0})"
    );
}

#[test]
fn a_hot_light_caret_stays_in_its_own_hue() {
    use crate::color_math::rgb2hsv;
    let gap = |a: f32, b: f32| {
        let d = (a - b).abs().rem_euclid(360.0);
        d.min(360.0 - d)
    };
    for base in [0x0009_69DA, 0x00D0_3A8C, 0x001A_7F37] {
        let (h0, s0, _) = rgb2hsv(base);
        let mut worst_mix = 0.0f32;
        for keys in 1..=12u32 {
            let cfg = on(base, false);
            let t0 = Instant::now();
            let mut g = MomentumGlow::default();
            for _ in 0..keys {
                g.on_key(t0, TAU);
            }
            let frame = g.tick(
                Some((6, 20)),
                t0,
                geom(),
                &cfg,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            let fill = frame.fill.expect("a hot block wears a fill");
            let (h, sat, _) = rgb2hsv(fill);
            assert!(
                gap(h, h0) <= 4.0 && sat >= s0 - 0.02,
                "{base:06x} at {keys} keys: {fill:06x} left its hue"
            );
            let a = (keys as f32 * MOMENTUM_GLOW_KEY_CREDIT).min(1.0);
            let s = smoothstep(a);
            let mixed = mix_rgb(base, ramp(a, false), 0.55 * s);
            worst_mix = worst_mix.max(gap(rgb2hsv(mixed).0, h0));
        }
        assert_eq!(
            crate::color_math::light_charge_in_hue(base, 0.0),
            base,
            "cold, the caret is its own colour"
        );
        assert!(
            worst_mix > 20.0,
            "control: the warm ramp moves {base:06x} only {worst_mix:.0} degrees"
        );
    }
}

#[test]
fn the_light_glow_keeps_the_newest_letter_within_half_a_point() {
    let g0 = geom();
    let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
    let cell = (6u16, 20u16);
    let px = |row: u16, col: u16| {
        let (x0, y0) = (usize::from(col) * g0.cw, usize::from(row) * g0.ch);
        (y0..y0 + g0.ch).flat_map(move |y| (x0..x0 + g0.cw).map(move |x| y * w + x))
    };
    let reads = |ground: &[u32], ink: u32, cols: std::ops::Range<u16>| {
        cols.flat_map(|c| px(cell.0, c))
            .map(|i| contrast_ratio(ink, ground[i]))
            .fold(f32::MAX, f32::min)
    };
    for (paper, ink) in [(0x00FF_FFFF, 0x001F_2328), (0x00F6_F7F9, 0x002B_2D33)] {
        for bed in [paper, 0x00F3_E2C2] {
            for hue in [0x00FF_D9A0, 0x00E0_6CE8, 0x0009_69DA] {
                let lit = |span: Option<(i32, i32)>| {
                    let cfg = MomentumGlowConfig {
                        hue,
                        theme_fg: ink,
                        theme_bg: paper,
                        glyph_span: span,
                        ..on(hue, false)
                    };
                    composite(&charged(40, cell, g0, &cfg), bed, w, h)
                };
                let off = vec![bed; w * h];
                let bound = lit(Some((2, i32::from(cell.1))));
                for newest in [19..20u16, 18..20] {
                    let (was, now) = (
                        reads(&off, ink, newest.clone()),
                        reads(&bound, ink, newest.clone()),
                    );
                    assert!(
                        was - now <= 0.5,
                        "{hue:06x} over {bed:06x}: the newest letter {newest:?} reads {now:.2}, \
                         {was:.2} with the glow off"
                    );
                }
                let spent = spent_per_pixel(
                    &charged(
                        40,
                        cell,
                        g0,
                        &MomentumGlowConfig {
                            hue,
                            theme_fg: ink,
                            theme_bg: paper,
                            glyph_span: Some((2, i32::from(cell.1))),
                            ..on(hue, false)
                        },
                    ),
                    w,
                    h,
                );
                let ahead = px(cell.0, cell.1 + 1).map(|i| spent[i]).max().unwrap_or(0);
                let under = px(cell.0, cell.1).map(|i| spent[i]).max().unwrap_or(0);
                assert!(
                    ahead as f32 >= LIGHT_GLOW_PEAK * 0.6 && under as f32 >= LIGHT_GLOW_PEAK * 0.9,
                    "{hue:06x}: the glow must still show ahead of the caret ({ahead}) and \
                     under it ({under})"
                );
                let whole = lit(None);
                let cost = reads(&off, ink, 19..20) - reads(&whole, ink, 19..20);
                assert!(
                    cost > 0.5,
                    "control: {hue:06x} laid whole on the letters costs only {cost:.2}"
                );
            }
        }
    }
}

#[test]
fn the_light_glow_stacked_on_a_light_body_keeps_the_light_bar() {
    use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, light_body_ink};
    let hues: Vec<u32> = (0..36)
        .map(|k| crate::color_math::hsv2rgb(k as f32 * 10.0, 1.0, 1.0))
        .collect();
    let worst = |glow_ink: &dyn Fn(u32) -> u32, ink: u32, page: u32| {
        let lay = |ground: u32, rgb: u32, alpha: f32| {
            let alpha = alpha as u8;
            aterm_render::over_premul(ground, premul_rgb(rgb, alpha), alpha) & 0x00FF_FFFF
        };
        let mut worst = f32::MAX;
        for &body in &hues {
            let body_ink = light_body_ink(body, ink, page, LIGHT_BODY_ALPHA_CAP as u8);
            let under = lay(page, body_ink, LIGHT_BODY_ALPHA_CAP);
            for &glow in &hues {
                let glow_ink = glow_ink(glow);
                for k in 1..=17 {
                    let alpha = (LIGHT_GLOW_PEAK * k as f32 / 17.0).min(LIGHT_GLOW_PEAK);
                    worst = worst.min(contrast_ratio(ink, lay(under, glow_ink, alpha)));
                }
            }
        }
        worst
    };
    for (ink, page) in [(0x0024_292F, 0x00FF_FFFF), (0x002B_2D33, 0x00F6_F7F9)] {
        for vivid in [false, true] {
            let ratio = worst(&|hue| light_glow_ink(hue, ink, page, vivid), ink, page);
            assert!(
                ratio >= 5.25,
                "{ink:06x} on {page:06x} under a body and the glow (vivid {vivid}) reads {ratio:.2}:1"
            );
        }
    }
    let (ink, page) = (0x0024_292F, 0x00FF_FFFF);
    let body_law = worst(
        &|hue| light_body_ink(hue, ink, page, LIGHT_BODY_ALPHA_CAP as u8),
        ink,
        page,
    );
    assert!(
        body_law < 5.25,
        "control: a glow in the body's own ink law stacks to {body_law:.2}:1"
    );
}

#[test]
fn halo_surrounds_the_caret_in_a_padded_headed_window() {
    let t0 = Instant::now();
    let real = Geom {
        cw: 15,
        ch: 28,
        rows: 24,
        cols: 100,
        origin_x: 24,
        origin_y: 84,
        win_w: 1548,
        win_h: 780,
        head: 80,
    };
    let (cr, cc) = (4u16, 10u16);
    let mut g = MomentumGlow::default();
    for _ in 0..40 {
        g.on_key(t0, TAU);
    }
    let mut out = Vec::new();
    let f = g.tick(
        Some((cr, cc)),
        t0,
        real,
        &on(0x00FF_FFFF, true),
        &mut out,
        &mut Vec::new(),
    );
    assert!(f.hot && !out.is_empty());
    let (cell_x0, cell_y0) = (
        i32::from(real.origin_x) + i32::from(cc) * real.cw as i32,
        i32::from(real.origin_y) + i32::from(cr) * real.ch as i32,
    );
    let (cell_x1, cell_y1) = (cell_x0 + real.cw as i32, cell_y0 + real.ch as i32);
    let x0 = out.iter().map(|q| i32::from(q.x)).min().unwrap();
    let x1 = out.iter().map(|q| i32::from(q.x + q.w)).max().unwrap();
    let y0 = out.iter().map(|q| i32::from(q.y)).min().unwrap();
    let y1 = out.iter().map(|q| i32::from(q.y + q.h)).max().unwrap();
    assert!(
        x0 < cell_x0 && x1 > cell_x1 && y0 < cell_y0 && y1 > cell_y1,
        "halo bbox x {x0}..{x1} y {y0}..{y1} does not surround the caret cell \
         x {cell_x0}..{cell_x1} y {cell_y0}..{cell_y1}"
    );
    let (ecx, ecy) = real.cell_center(cr, cc);
    assert!(
        ((x0 + x1) as f32 * 0.5 - ecx).abs() <= 2.0,
        "x centre {} vs {ecx}",
        (x0 + x1) as f32 * 0.5
    );
    assert!(
        ((y0 + y1) as f32 * 0.5 - ecy).abs() <= real.ch as f32 * 0.25,
        "y centre {} vs {ecy}",
        (y0 + y1) as f32 * 0.5
    );
    for q in &out {
        assert!(i32::from(q.x) >= real.fx_left() && i32::from(q.x + q.w) <= real.fx_right());
        assert!(i32::from(q.y) >= real.fx_top() && i32::from(q.y + q.h) <= real.fx_bot());
    }
}

#[test]
fn fingerprint_settles_so_the_lane_releases() {
    let t0 = Instant::now();
    let mut g = MomentumGlow::default();
    g.on_key(t0, TAU);
    let mut out = Vec::new();
    let cfg = on(0x0010_2030, true);
    let f1 = g.tick(Some((1, 1)), t0, geom(), &cfg, &mut out, &mut Vec::new());
    assert!(f1.hot);
    let f2 = g.tick(Some((1, 1)), t0, geom(), &cfg, &mut out, &mut Vec::new());
    assert_eq!(f1.fp, f2.fp);
    assert!(!g.is_active(), "an unchanged frame must not hold the lane");
    assert!(
        g.value(t0, TAU) > 0.0,
        "…even though the value is still warm"
    );
    assert!(g.settling(), "…so it owes its next step instead");
    let f3 = g.tick(
        Some((1, 1)),
        t0 + Duration::from_millis(400),
        geom(),
        &cfg,
        &mut out,
        &mut Vec::new(),
    );
    assert_ne!(f3.fp, f2.fp);
    assert!(g.is_active());
}

#[test]
fn a_cooling_glow_keeps_the_lane_until_its_last_frame_clears_the_halo() {
    for lane_ms in [16.7f32, 33.3] {
        let lane = Duration::from_secs_f32(lane_ms / 1000.0);
        for honest in [true, false] {
            let t0 = Instant::now();
            let mut g = MomentumGlow::default();
            let cfg = on(0x0050_FA7B, true);
            let mut out = Vec::new();
            let (mut frames, mut presents, mut last_fp) = (0u32, 0u32, 0u64);
            let mut drawn = (0usize, MomentumGlowFrame::default());
            let mut frame = |g: &mut MomentumGlow, now: Instant, col: u16| {
                out.clear();
                let f = g.tick(Some((4, col)), now, geom(), &cfg, &mut out, &mut Vec::new());
                frames += 1;
                if f.fp != last_fp {
                    presents += 1;
                    last_fp = f.fp;
                }
                drawn = (out.len(), f);
            };
            let (mut now, mut col) = (t0, 10u16);
            for k in 0..24u16 {
                now = t0 + Duration::from_millis(60 * u64::from(k));
                col = 10 + k;
                g.on_key(now, TAU);
                frame(&mut g, now, col);
            }
            let burst_end = now;
            let parked_at = loop {
                let next = if g.is_active() {
                    Some(now + lane)
                } else if honest && g.settling() {
                    g.next_change_deadline(now, lane)
                } else {
                    None
                };
                let Some(next) = next else { break now };
                assert!(next > now, "a deadline at or before now is a busy re-arm");
                assert!(
                    next.saturating_duration_since(burst_end) < Duration::from_secs(60),
                    "the lane never parked"
                );
                now = next;
                frame(&mut g, now, col);
            };
            let tail = parked_at.saturating_duration_since(burst_end);
            println!(
                "momentum lane {lane_ms} ms, {}: {frames} frames, {presents} presents, \
                 parked {tail:?} after the last key with {} halo quads",
                if honest {
                    "settle arm"
                } else {
                    "frame cadence only"
                },
                drawn.0
            );
            if honest {
                assert!(
                    drawn.0 == 0 && drawn.1.fill.is_none() && !drawn.1.hot,
                    "{lane_ms} ms: parked on a frame still drawing {} quads",
                    drawn.0
                );
                assert!(
                    g.value(parked_at, TAU) < MOMENTUM_GLOW_HOT,
                    "{lane_ms} ms: parked while still hot"
                );
            } else {
                assert!(
                    drawn.0 > 0 && drawn.1.hot,
                    "control at {lane_ms} ms: the cadence alone parks with light on glass"
                );
            }
        }
    }

    let t0 = Instant::now();
    let mut light = MomentumGlow::default();
    light.on_key(t0, TAU);
    let white = on(0x00D8_8B18, false);
    let vivid = MomentumGlowConfig {
        light_vivid: true,
        ..white
    };
    let f0 = light.tick(
        Some((4, 10)),
        t0,
        geom(),
        &white,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    let f1 = light.tick(
        Some((4, 10)),
        t0,
        geom(),
        &white,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(f0.fp, f1.fp);
    assert!(!light.is_active() && light.settling());
    let f2 = light.tick(
        Some((4, 10)),
        t0,
        geom(),
        &vivid,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_ne!(f2.fp, f1.fp, "switching light_vivid moves the fingerprint");
    assert!(light.is_active() && !light.settling());
}

#[test]
fn each_settle_wake_is_the_glows_next_visible_step() {
    #[derive(Clone, Copy)]
    enum Schedule {
        Analytic,
        Poll,
        ColdOnly,
    }
    let cfg = on(0x0050_FA7B, true);
    let shown = |g: &MomentumGlow, t: Instant| {
        let a = g.value(t, TAU);
        if a < MOMENTUM_GLOW_HOT {
            0
        } else {
            (a * 255.0) as u8
        }
    };
    let run = |schedule: Schedule, lane: Duration| -> (u32, u32, u32) {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        let mut out = Vec::new();
        let mut now = t0;
        for k in 0..24u64 {
            now = t0 + Duration::from_millis(60 * k);
            g.on_key(now, TAU);
            out.clear();
            g.tick(Some((4, 10)), now, geom(), &cfg, &mut out, &mut Vec::new());
        }
        let mut drawn = shown(&g, now);
        let (mut wakes, mut wasted, mut slept) = (0u32, 0u32, 0u32);
        loop {
            let floor = now + lane;
            let settle = g.settling();
            let next = if g.is_active() {
                floor
            } else if settle {
                match schedule {
                    Schedule::Analytic => g
                        .next_change_deadline(now, lane)
                        .expect("a settling glow names its next step"),
                    Schedule::Poll => now + Duration::from_millis(33),
                    Schedule::ColdOnly => {
                        (g.cold_at().expect("still hot") + SETTLE_MARGIN).max(floor)
                    }
                }
            } else {
                break;
            };
            assert!(
                next.saturating_duration_since(t0) < Duration::from_secs(60),
                "fixture: the glow never went cold"
            );
            if settle {
                wakes += 1;
                if next > floor && shown(&g, next - Duration::from_millis(2)) != drawn {
                    slept += 1;
                }
            }
            now = next;
            out.clear();
            g.tick(Some((4, 10)), now, geom(), &cfg, &mut out, &mut Vec::new());
            let step = shown(&g, now);
            if settle && step == drawn {
                wasted += 1;
            }
            drawn = step;
        }
        (wakes, wasted, slept)
    };
    for lane in [Duration::from_micros(16_667), Duration::from_micros(33_333)] {
        let (wakes, wasted, slept) = run(Schedule::Analytic, lane);
        assert!(wakes > 0, "fixture at {lane:?}: the cool-down settles");
        assert_eq!(
            (wasted, slept),
            (0, 0),
            "at {lane:?}, of {wakes} settle wakes: wasted, slept through a step"
        );
        let (_, wasted, _) = run(Schedule::Poll, lane);
        assert!(wasted > 0, "control at {lane:?}: a 33 ms poll wastes wakes");
        let (_, _, slept) = run(Schedule::ColdOnly, lane);
        assert!(
            slept > 0,
            "control at {lane:?}: waking only at the cold crossing sleeps through steps"
        );
    }
}

#[test]
fn ramp_is_monotone_in_luminance_on_dark() {
    let lum = |c: u32| {
        0.299 * ((c >> 16) & 0xff) as f32
            + 0.587 * ((c >> 8) & 0xff) as f32
            + 0.114 * (c & 0xff) as f32
    };
    let mut last = 0.0;
    for k in 0..=100 {
        let l = lum(ramp(k as f32 / 100.0, true));
        assert!(l + 0.5 >= last, "ramp dips at {k}: {l} < {last}");
        last = l;
    }
}
