// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

const fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: (40 * 8) as u16,
        win_h: (6 * 16) as u16,
        head: 0,
    }
}
const fn cfg() -> RainbowConfig {
    RainbowConfig {
        light: crate::cursor_glow::Light::OFF,
        enabled: true,
        intensity: 1.0,
        blinking: false,
        base: None,
        head_rgb: None,
        paint: None,
        ground: None,
        flare_at: None,
    }
}

/// The settle probe latches the frame's light with the rest of the config
/// (`docs/design/PRISM-GLASS.md` §15.2 1.3), and the light moves neither the frame nor the
/// rainbow's state words: nothing reads it yet.
#[test]
fn the_settle_probe_latches_the_frames_light() {
    use crate::cursor_glow::{Light, Live};
    let t0 = Instant::now();
    let lit = RainbowConfig {
        light: Light::at(Live::Glass),
        ..cfg()
    };
    let run = |c: &RainbowConfig| {
        let mut caret = CursorRainbow::new();
        let mut out = Vec::new();
        let frame = caret.tick(Some((2, 3)), t0, 1.0, false, false, geom(), c, &mut out);
        let mut w = crate::state_words::StateWords::new(t0);
        caret.state_words(&mut w);
        (caret, frame.fp, out.len(), w.into_words())
    };
    let (glass, glass_fp, glass_quads, glass_words) = run(&lit);
    let (dark, dark_fp, dark_quads, dark_words) = run(&cfg());
    assert_eq!(
        glass.settle_probe.map(|p| p.cfg.light),
        Some(lit.light),
        "the probe latches the light"
    );
    assert_eq!(dark.settle_probe.map(|p| p.cfg.light), Some(Light::OFF));
    assert_eq!(
        (glass_fp, glass_quads),
        (dark_fp, dark_quads),
        "the frame is main's"
    );
    assert_eq!(glass_words, dark_words, "the light is not hashed");
}

#[test]
fn the_flare_is_white_on_frame_zero_and_the_identity_once_settled() {
    let g = geom();
    let t0 = Instant::now();
    let plain = cfg();
    let flared = RainbowConfig {
        flare_at: Some(t0),
        ..plain
    };
    let mut out = Vec::new();
    let f0 = CursorRainbow::default()
        .tick(Some((1, 1)), t0, 1.0, true, true, g, &flared, &mut out)
        .fill
        .unwrap();
    assert_eq!(f0, 0x00FF_FFFF, "frame 0 of the flare is white");
    let late = t0 + Duration::from_secs(2);
    let mut a = Vec::new();
    let mut b = Vec::new();
    let fa = CursorRainbow::default()
        .tick(Some((1, 1)), late, 1.0, true, true, g, &flared, &mut a)
        .fill;
    let fb = CursorRainbow::default()
        .tick(Some((1, 1)), late, 1.0, true, true, g, &plain, &mut b)
        .fill;
    assert_eq!(fa, fb, "a settled flare is the plain fill");
    assert_eq!(a, b, "a settled flare's halo is the plain halo");
}

#[test]
fn the_caret_settles_on_its_own_stop_without_a_pop() {
    use crate::cursor_glow::{CursorGlow, GlowConfig};
    use crate::rainbow_kitty::ribbon::walk_arc;
    use crate::spectrum::spectrum;

    let g = geom();
    let glow_cfg = GlowConfig {
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    };
    fn host_cfg(glow: &CursorGlow, glow_cfg: &GlowConfig, now: Instant) -> RainbowConfig {
        RainbowConfig {
            light: crate::cursor_glow::Light::OFF,
            enabled: true,
            intensity: 1.0,
            blinking: false,
            base: None,
            head_rgb: glow.rainbow_head_rgb(glow_cfg),
            paint: Some(glow.caret_paint(now)),
            ground: Some(glow_cfg.theme_bg),
            flare_at: glow.caret_flare_at(),
        }
    }
    fn frame(
        glow: &mut CursorGlow,
        body: &mut CursorRainbow,
        glow_cfg: &GlowConfig,
        g: Geom,
        now: Instant,
        cell: (u16, u16),
    ) -> u32 {
        let mut glow_out = Vec::new();
        let mut body_out = Vec::new();
        glow.tick(Some(cell), now, glow_cfg, g, &mut glow_out);
        let cfg = host_cfg(glow, glow_cfg, now);
        body.tick_with_family_phase(
            Some(cell),
            now,
            0.0,
            glow.rainbow_phase(),
            glow.rainbow_field(),
            false,
            true,
            g,
            &cfg,
            &mut body_out,
        )
        .fill
        .expect("enabled caret fill")
    }
    let max_delta = |a: u32, b: u32| -> u32 {
        (0..3)
            .map(|s| ((a >> (8 * s)) & 0xff).abs_diff((b >> (8 * s)) & 0xff))
            .max()
            .unwrap_or(0)
    };

    const HZ: u64 = 8_333;
    let t0 = Instant::now();
    let row = 2u16;
    let mut glow = CursorGlow::default();
    let mut body = CursorRainbow::default();
    frame(&mut glow, &mut body, &glow_cfg, g, t0, (row, 4));
    assert!(glow.v2_status().is_some(), "v2 must own the frame");
    let mut last_key = t0;
    for k in 1..=9u64 {
        last_key = t0 + Duration::from_millis(70 * k);
        glow.note_typed(last_key);
        frame(
            &mut glow,
            &mut body,
            &glow_cfg,
            g,
            last_key,
            (row, 4 + k as u16),
        );
    }
    let mut now = last_key;
    let jump_at = last_key + Duration::from_millis(1_200);
    while now + Duration::from_micros(HZ) < jump_at {
        now += Duration::from_micros(HZ);
        frame(&mut glow, &mut body, &glow_cfg, g, now, (row, 13));
    }
    glow.note_motion(jump_at);
    let landing = (row, 33);
    let f0 = frame(&mut glow, &mut body, &glow_cfg, g, jump_at, landing);
    assert_eq!(
        glow.caret_flare_at(),
        Some(jump_at),
        "a credited jump flares on the frame it is observed"
    );
    assert_eq!(f0, 0x00FF_FFFF, "frame 0 of the landing is white");
    let field_t = glow.rainbow_field();
    assert!(
        field_t > 0.25,
        "non-vacuous: the walk laid several stops before the jump (t = {field_t})"
    );
    let own_stop = spectrum(walk_arc(field_t));
    assert_eq!(
        glow.rainbow_head_rgb(&glow_cfg),
        Some(own_stop),
        "on the landing frame the seam's head colour IS the caret's own stop"
    );

    let mut fills = vec![f0];
    let mut now = jump_at;
    let dwell_end = jump_at + Duration::from_millis(400);
    while now + Duration::from_micros(HZ) <= dwell_end {
        now += Duration::from_micros(HZ);
        fills.push(frame(&mut glow, &mut body, &glow_cfg, g, now, landing));
    }

    let mut worst = (0u32, 0usize);
    for (i, pair) in fills.windows(2).enumerate().skip(1) {
        let d = max_delta(pair[0], pair[1]);
        if d > worst.0 {
            worst = (d, i + 1);
        }
    }
    assert!(
        worst.0 <= 40,
        "the caret popped {}/255 on dwell frame {} (+{:.1} ms): {:06X} -> {:06X}",
        worst.0,
        worst.1,
        worst.1 as f32 * HZ as f32 / 1000.0,
        fills[worst.1 - 1],
        fills[worst.1]
    );

    let settled = *fills.last().unwrap();
    let mut law = CursorRainbow::default();
    let mut out = Vec::new();
    let law_cfg = RainbowConfig {
        head_rgb: Some(own_stop),
        ..host_cfg(&glow, &glow_cfg, now)
    };
    let expected = law
        .tick_with_family_phase(
            Some(landing),
            now,
            0.0,
            glow.rainbow_phase(),
            field_t,
            false,
            true,
            g,
            &law_cfg,
            &mut out,
        )
        .fill
        .unwrap();
    assert_eq!(
        settled, expected,
        "settled on {settled:06X}, but the paint law on the caret's own stop \
         {own_stop:06X} (t = {field_t}) gives {expected:06X}"
    );
}

fn continuity_ceiling(dt: f32) -> u32 {
    (crate::spectrum::spectrum_max_byte_rate() * dt).ceil() as u32 + 1
}

fn caret_continuity_ceiling(base: u32, mix: f32, dt: f32) -> u32 {
    const OVERSAMPLE: usize = 8;
    let steps = (1.0 / dt.max(f32::MIN_POSITIVE)).ceil() as usize * OVERSAMPLE;
    let at = |i: usize| {
        caret_law(
            base,
            crate::spectrum::spectrum(i as f32 / steps as f32),
            mix,
            1.0,
        )
    };
    let mut worst = 0u32;
    for i in 0..steps.saturating_sub(OVERSAMPLE) {
        worst = worst.max(rgb_max_delta(at(i), at(i + OVERSAMPLE)));
    }
    worst + 1
}

fn caret_law(base: u32, rainbow: u32, mix: f32, e: f32) -> u32 {
    lift_to_light_floor(
        mix_rgb(base, rainbow, mix),
        RAINBOW_CARET_LIGHT_FLOOR * aterm_render::smoothstep01(e / CARET_LIGHT_KNEE),
    )
}

fn assert_white_lift_of(actual: u32, source: u32, context: &str) {
    let channels = |rgb: u32| {
        [
            ((rgb >> 16) & 0xff) as f32,
            ((rgb >> 8) & 0xff) as f32,
            (rgb & 0xff) as f32,
        ]
    };
    let from = channels(source);
    let to = channels(actual);
    assert!(
        to.into_iter().zip(from).all(|(to, from)| to >= from),
        "{context}: {actual:#08x} is not a non-darkening lift of {source:#08x}"
    );
    let axis = (0..3)
        .max_by(|&a, &b| (255.0 - from[a]).total_cmp(&(255.0 - from[b])))
        .unwrap_or(0);
    let t = if from[axis] < 255.0 {
        (to[axis] - from[axis]) / (255.0 - from[axis])
    } else {
        0.0
    };
    let rebuilt = mix_rgb(source, 0x00FF_FFFF, t);
    assert!(
        rgb_max_delta(actual, rebuilt) <= 1,
        "{context}: {actual:#08x} left the white-lift ray from {source:#08x} (rebuilt {rebuilt:#08x})"
    );
}

fn rgb_max_delta(a: u32, b: u32) -> u32 {
    [16, 8, 0]
        .into_iter()
        .map(|shift| ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff))
        .max()
        .unwrap_or(0)
}

const fn blink_cfg() -> RainbowConfig {
    RainbowConfig {
        blinking: true,
        ..cfg()
    }
}

#[test]
fn disabled_or_zero_intensity_is_inert() {
    for (why, config) in [
        (
            "disabled",
            RainbowConfig {
                enabled: false,
                ..cfg()
            },
        ),
        (
            "zero intensity",
            RainbowConfig {
                intensity: 0.0,
                ..cfg()
            },
        ),
    ] {
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let f = cr.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            true,
            true,
            geom(),
            &config,
            &mut out,
        );
        assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
        assert_eq!(f.fp, 0, "{why}");
        assert!(out.is_empty(), "{why}: no additive light");
        assert!(!cr.is_active(), "{why}: settled");
    }
}

#[test]
fn active_rim_reuses_its_warmed_scratch() {
    let g = geom();
    let c = cfg();
    let mut cursor = CursorRainbow::default();
    let mut out = Vec::new();
    let mut now = Instant::now();
    for step in 0..64 {
        out.clear();
        now += Duration::from_millis(8);
        let _ = cursor.tick(
            Some((1, (step % 8) as u16)),
            now,
            1.0,
            step % 2 == 0,
            true,
            g,
            &c,
            &mut out,
        );
    }
    let before = (out.capacity(), cursor.rim_scratch.capacity());
    assert!(before.1 > 0, "fixture must exercise the active rim");
    out.clear();
    now += Duration::from_millis(8);
    let _ = cursor.tick(Some((1, 0)), now, 1.0, true, true, g, &c, &mut out);
    assert_eq!(
        before,
        (out.capacity(), cursor.rim_scratch.capacity()),
        "a warmed active rim frame must not grow scratch storage"
    );
}

#[test]
fn fill_blooms_from_base_with_energy() {
    let g = geom();
    let c = cfg();
    let mut idle = CursorRainbow::default();
    let mut out = Vec::new();
    let t = Instant::now();
    let f_idle = idle
        .tick(Some((1, 1)), t, 0.0, true, true, g, &c, &mut out)
        .fill
        .unwrap();
    let minch = |c: u32| {
        [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
            .into_iter()
            .min()
            .unwrap()
    };
    assert!(
        minch(f_idle) > 150,
        "idle block stays near white on dark, got {f_idle:#08x}"
    );
    let mut hot = CursorRainbow::default();
    out.clear();
    let f_hot = hot
        .tick(Some((1, 1)), t, 1.0, true, true, g, &c, &mut out)
        .fill
        .unwrap();
    let spread = |c: u32| {
        let (r, gg, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        r.max(gg).max(b) - r.min(gg).min(b)
    };
    assert!(
        spread(f_hot) > spread(f_idle) + 40,
        "energy saturates the fill"
    );
    let f_light = CursorRainbow::default()
        .tick(Some((1, 1)), t, 0.0, true, false, g, &c, &mut out)
        .fill
        .unwrap();
    let maxch = [
        (f_light >> 16) & 0xff,
        (f_light >> 8) & 0xff,
        f_light & 0xff,
    ]
    .into_iter()
    .max()
    .unwrap();
    assert!(
        maxch < 90,
        "idle block near black on a light theme, got {f_light:#08x}"
    );
}

#[test]
fn host_base_is_the_settled_block_on_either_polarity() {
    let g = geom();
    let mut out = Vec::new();
    let t = Instant::now();
    let settled = |base: u32, dark: bool, out: &mut Vec<GlowQuad>| {
        let c = RainbowConfig {
            base: Some(base),
            ..cfg()
        };
        out.clear();
        CursorRainbow::default()
            .tick(Some((1, 1)), t, 0.0, true, dark, g, &c, out)
            .fill
            .unwrap()
    };
    let chans = |c: u32| ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
    for dark in [true, false] {
        let (rr, rg, rb) = chans(settled(0x00FF_0000, dark, &mut out));
        assert!(
            rr > 200 && rg < 110 && rb < 110,
            "a red cursor colour settles red (dark={dark})"
        );
        let (br, bg, bb) = chans(settled(0x0000_00FF, dark, &mut out));
        assert!(
            bb > 200 && br < 110 && bg < 110,
            "a blue cursor colour settles blue (dark={dark})"
        );
        assert_ne!(
            settled(0x00FF_0000, dark, &mut out),
            settled(0x0000_00FF, dark, &mut out),
            "two cursor colours must not paint one identical block (dark={dark})"
        );
    }
}

#[test]
fn emitted_ribbon_head_is_the_hot_caret_authority() {
    use crate::cursor_glow::RAINBOW_PHASE_RING;

    let g = geom();
    let now = Instant::now();
    let render = |head_rgb: u32, family_phase: f32| {
        let c = RainbowConfig {
            base: Some(0x0020_2020),
            head_rgb: Some(head_rgb),
            ..cfg()
        };
        let mut out = Vec::new();
        let frame = CursorRainbow::default().tick_with_family_phase(
            Some((1, 7)),
            now,
            1.0,
            family_phase,
            rainbow_sweep_at(7, family_phase),
            false,
            true,
            g,
            &c,
            &mut out,
        );
        (frame, out)
    };

    let (green_a, halo_a) = render(0x0033_FF00, 0.0);
    let (green_b, _) = render(0x0033_FF00, RAINBOW_PHASE_RING * 0.37);
    let (violet, _) = render(0x0066_33FF, 0.0);
    let green_source = caret_law(0x0020_2020, 0x0033_FF00, MIX_MAX, 1.0);
    assert_white_lift_of(
        green_a.fill.expect("green caret fill"),
        green_source,
        "green head at phase A owns the block hue",
    );
    assert_white_lift_of(
        green_b.fill.expect("green caret fill"),
        green_source,
        "green head at phase B owns the block hue",
    );
    assert_white_lift_of(
        violet.fill.expect("violet caret fill"),
        caret_law(0x0020_2020, 0x0066_33FF, MIX_MAX, 1.0),
        "violet head owns the block hue",
    );
    assert_ne!(
        green_a.fill, violet.fill,
        "two emitted head hues must paint two different hot carets"
    );
    let fill = green_a.fill.expect("rainbow block fill");
    let (fr, fg, fb) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
    assert!(
        fg > fr && fg > fb,
        "green emitted head must produce a green-dominant caret: {fill:#08x}"
    );
    assert!(
        halo_a.iter().any(|q| {
            let r = (q.color >> 16) & 0xff;
            let g = (q.color >> 8) & 0xff;
            let b = q.color & 0xff;
            g > r && g > b
        }),
        "the innermost rim must visibly carry the emitted head hue"
    );
}

#[test]
fn a_light_page_caret_is_not_lifted_toward_white() {
    let g = geom();
    let c = RainbowConfig {
        head_rgb: Some(0x0060_20C0),
        ..cfg()
    };
    let fill = |dark: bool| {
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            true,
            dark,
            g,
            &c,
            &mut out,
        )
        .fill
        .expect("a painted rainbow fills the block")
    };
    let floor = RAINBOW_CARET_LIGHT_FLOOR / 255.0;
    let lum = crate::color_math::relative_luminance;
    let (on_dark, on_light) = (fill(true), fill(false));
    assert!(
        lum(on_dark) >= floor - 0.005,
        "control: a dark page lifts the caret to the floor, got {on_dark:06x}"
    );
    assert!(
        lum(on_light) < floor,
        "a light page keeps the hue unlifted, got {on_light:06x}"
    );
    let (_, s, _) = crate::color_math::rgb2hsv(on_light);
    assert!(
        s > 0.7,
        "the light caret stays saturated, got {on_light:06x}"
    );
}

#[test]
fn the_block_outshines_its_own_rim_on_a_spent_field() {
    const POSITIONS: u32 = 24;
    let (row, col) = (2u16, 17u16);
    let light = |rgb: u32| crate::color_math::relative_luminance(rgb) * 255.0;
    let level = RAINBOW_FIELD_LEVEL.round() as u32;
    let field_grey = (level << 16) | (level << 8) | level;
    let giant = Geom {
        cw: 160,
        ch: 320,
        win_w: 40 * 160,
        win_h: 6 * 320,
        ..geom()
    };
    let polar = vec![("<none: theme-polar>".to_string(), None)];
    let cases = [
        (geom(), shipped_caret_bases(), POSITIONS, &[false, true][..]),
        (giant, polar, 4, &[false][..]),
    ];
    let promised = RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
    let mut checked = 0usize;
    let mut past_the_floor = 0usize;
    let mut past_the_raster = 0usize;
    for (g, bases, positions, flares) in cases {
        let cell = (
            usize::from(col) * g.cw,
            usize::from(row) * g.ch,
            usize::from(col + 1) * g.cw,
            usize::from(row + 1) * g.ch,
        );
        for (name, base) in bases {
            for step in 0..=positions {
                let field = step as f32 / positions as f32;
                for &flare in flares {
                    let c = RainbowConfig {
                        base,
                        paint: Some(1.0),
                        blinking: flare,
                        ..cfg()
                    };
                    let mut caret = CursorRainbow::default();
                    let mut out = Vec::new();
                    let now = Instant::now();
                    let mut tick = |blink: bool, at: Instant, out: &mut Vec<GlowQuad>| {
                        out.clear();
                        caret
                            .tick_with_family_phase(
                                Some((row, col)),
                                at,
                                1.0,
                                0.0,
                                field,
                                blink,
                                true,
                                g,
                                &c,
                                out,
                            )
                            .fill
                            .expect("an enabled block fills")
                    };
                    let fill = if flare {
                        tick(false, now, &mut out);
                        tick(true, now, &mut out);
                        tick(true, now + Duration::from_millis(70), &mut out)
                    } else {
                        tick(false, now, &mut out)
                    };
                    let x0 = out.iter().map(|q| usize::from(q.x)).min().unwrap_or(0);
                    let y0 = out.iter().map(|q| usize::from(q.y)).min().unwrap_or(0);
                    let x1 = out
                        .iter()
                        .map(|q| usize::from(q.x + q.w))
                        .max()
                        .unwrap_or(0);
                    let y1 = out
                        .iter()
                        .map(|q| usize::from(q.y + q.h))
                        .max()
                        .unwrap_or(0);
                    assert!(x1 > x0 && y1 > y0, "{name}: no rim at full paint");
                    let (w, h) = (x1 - x0, y1 - y0);
                    let moved: Vec<GlowQuad> = out
                        .iter()
                        .map(|q| GlowQuad {
                            x: q.x - x0 as u16,
                            y: q.y - y0 as u16,
                            ..*q
                        })
                        .collect();
                    let mut px = vec![field_grey; w * h];
                    aterm_render::fx::paint_quads(&mut px, w, h, &moved, (0, 0));
                    let mut peak = (0.0f32, 0usize, 0usize, 0u32);
                    for (i, &p) in px.iter().enumerate() {
                        let (x, y) = (x0 + i % w, y0 + i / w);
                        let in_cell =
                            (cell.0..cell.2).contains(&x) && (cell.1..cell.3).contains(&y);
                        if !in_cell && light(p) > peak.0 {
                            peak = (light(p), x, y, p);
                        }
                    }
                    let lead = light(fill) - peak.0;
                    assert!(
                        lead + 1e-3 >= promised,
                        "{name} at a {}x{} cell, sweep {step}/{positions}, flare {flare}: \
                         the block #{fill:06X} (luminance {:.1}) leads its rim's #{:06X} \
                         (luminance {:.1}) at ({}, {}) by {lead:.1}, under the {promised:.0} \
                         the caret's floor promises",
                        g.cw,
                        g.ch,
                        light(fill),
                        peak.3,
                        peak.0,
                        peak.1,
                        peak.2
                    );
                    past_the_floor += usize::from(peak.0 > RAINBOW_CARET_LIGHT_FLOOR);
                    past_the_raster += usize::from(w * h > CursorRainbow::CARET_RASTER_MAX);
                    checked += 1;
                }
            }
        }
    }
    assert!(
        checked >= 12 * 2 * (POSITIONS as usize + 1) + 5
            && past_the_floor > 0
            && past_the_raster == 5,
        "CONTROL: {past_the_floor} of {checked} rims past the plain floor, \
         {past_the_raster} past the raster"
    );
}

#[test]
fn halo_quads_respect_grid_and_cap() {
    let g = geom();
    let c = blink_cfg();
    let t = Instant::now();
    let gw = (g.cols * g.cw) as u32;
    let gh = (g.rows * g.ch) as u32;
    let cw = g.cw as i32;
    for (row, col) in [(0u16, 0u16), (2, 20)] {
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((row, col)), t, 0.1, true, true, g, &c, &mut out);
        let cross = t + Duration::from_millis(16);
        cr.tick(Some((row, col)), cross, 1.0, false, true, g, &c, &mut out);
        out.clear();
        let mid = cross + Duration::from_secs_f32(TWINKLE_DUR / 2.0);
        cr.tick(Some((row, col)), mid, 1.0, false, true, g, &c, &mut out);
        assert!(!out.is_empty(), "a hot cursor glows");
        let (cell_l, cell_r) = (i32::from(col) * cw, (i32::from(col) + 1) * cw);
        let max_reach = cw / 2 + 1;
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
            for sh in [16, 8, 0] {
                assert!((q.color >> sh) & 0xff <= 180, "coverage capped: {q:?}");
            }
            assert!(
                cell_l - q.x as i32 <= max_reach && (q.x as i32 + q.w as i32) - cell_r <= max_reach,
                "reaches past half a cell: {q:?}"
            );
        }
    }
}

#[test]
fn momentum_rung_crossing_fires_twinkle_star() {
    let g = geom();
    let t = Instant::now();
    let cross = t + Duration::from_millis(16);
    let mid = cross + Duration::from_secs_f32(TWINKLE_DUR / 2.0);
    let minch = |c: u32| {
        [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
            .into_iter()
            .min()
            .unwrap()
    };
    let run = |c: &RainbowConfig, dark: bool, e: f32| {
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((2, 20)), t, 0.1, true, dark, g, c, &mut out);
        let calm_quads = out.len();
        cr.tick(Some((2, 20)), cross, e, true, dark, g, c, &mut out);
        out.clear();
        let fill = cr
            .tick(Some((2, 20)), mid, e, true, dark, g, c, &mut out)
            .fill
            .unwrap();
        (cr.twinkling, fill, out.len(), calm_quads)
    };
    let (twinkling, flared, quads, calm_quads) = run(&blink_cfg(), true, 0.3);
    assert!(twinkling, "a rung crossing arms the flare");
    assert!(
        quads > calm_quads,
        "the flare adds star quads over the idle halo ({quads} vs {calm_quads})"
    );
    let (steady_twinkling, calm, _, _) = run(&cfg(), true, 0.3);
    assert!(!steady_twinkling, "the steady twin never flares");
    assert!(
        minch(flared) >= minch(calm) && flared != calm,
        "mid-flare the dark-theme fill glints toward white ({flared:#08x} vs {calm:#08x})"
    );
    let (_, light, _, _) = run(&blink_cfg(), false, 0.8);
    assert!(
        minch(light) < 160,
        "the light-theme glint keeps saturation (never washes to white), got {light:#08x}"
    );
}

#[test]
fn idle_phase_flips_never_arm_effect_timer() {
    let g = geom();
    let t = Instant::now();
    let mut cr = CursorRainbow::default();
    let mut out = Vec::new();
    let mut phase = true;
    cr.tick(Some((1, 1)), t, 0.0, phase, true, g, &blink_cfg(), &mut out);
    for i in 1..=20u64 {
        phase = !phase;
        out.clear();
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(530 * i),
            0.0,
            phase,
            true,
            g,
            &blink_cfg(),
            &mut out,
        );
        assert!(!cr.is_active(), "idle flip {i} armed an effect wake");
        assert_eq!(cr.twinkle_seq, 0, "idle flip {i} consumed a flare identity");
    }
    let mut steady = CursorRainbow::default();
    steady.tick(Some((1, 1)), t, 0.0, true, true, g, &cfg(), &mut out);
    steady.tick(
        Some((1, 1)),
        t + Duration::from_millis(16),
        1.0,
        false,
        true,
        g,
        &cfg(),
        &mut out,
    );
    assert_eq!(
        steady.twinkle_seq, 0,
        "a steady block's wind-up fires no star"
    );
}

#[test]
fn idle_blink_transition_conforms_to_model() {
    let model = aterm_models_effects::rainbow_idle_twinkle_model();
    let state = |charged: i64,
                 twinkle: i64,
                 remaining: i64,
                 flare_seq: i64,
                 idle_restarts: i64,
                 steps: i64| {
        BTreeMap::from([
            ("charged", charged),
            ("twinkle", twinkle),
            ("remaining", remaining),
            ("flare_seq", flare_seq),
            ("idle_restarts", idle_restarts),
            ("steps", steps),
        ])
    };
    let project = |rainbow: &CursorRainbow, now: Instant, idle_restarts: i64, steps: i64| {
        let remaining = rainbow.twinkle_at.map_or(0, |started| {
            let u = now.saturating_duration_since(started).as_secs_f32() / TWINKLE_DUR;
            if u < 0.5 { 2 } else { 1 }
        });
        state(
            i64::from(rainbow.energy > SETTLED_ENERGY),
            i64::from(rainbow.twinkling),
            remaining,
            i64::from(rainbow.twinkle_seq),
            idle_restarts,
            steps,
        )
    };
    let g = geom();
    let c = blink_cfg();
    let t = Instant::now();

    let mut rainbow = CursorRainbow::default();
    let mut out = Vec::new();

    let before = project(&rainbow, t, 0, 0);
    rainbow.tick(Some((1, 1)), t, 0.2, true, true, g, &c, &mut out);
    let after = project(&rainbow, t, 0, 1);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &after,
        Some("Charge"),
        "Nyan charge conformance",
    );
    assert!(ok, "shipping charge transition rejected: {why}");

    let before = after;
    rainbow.tick(
        Some((1, 1)),
        t + Duration::from_millis(16),
        0.8,
        false,
        true,
        g,
        &c,
        &mut out,
    );
    let after = project(&rainbow, t + Duration::from_millis(16), 0, 2);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &after,
        Some("BlinkCharged"),
        "Nyan charged-blink conformance",
    );
    assert!(ok, "shipping charged-blink transition rejected: {why}");
    assert!(rainbow.twinkling, "the fixture has a live charged flare");
    assert_eq!(rainbow.twinkle_seq, 1, "exactly one flare generation");

    let before = after;
    rainbow.tick(
        Some((1, 1)),
        t + Duration::from_millis(32),
        0.0,
        false,
        true,
        g,
        &c,
        &mut out,
    );
    let after = project(&rainbow, t + Duration::from_millis(32), 0, 3);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &after,
        Some("Cool"),
        "Nyan cool conformance",
    );
    assert!(ok, "shipping cool transition rejected: {why}");
    assert!(rainbow.twinkling, "cooling preserves the in-flight flare");

    let before_idle = after;
    let seq_before = rainbow.twinkle_seq;
    rainbow.tick(
        Some((1, 1)),
        t + Duration::from_millis(48),
        0.0,
        true,
        true,
        g,
        &c,
        &mut out,
    );
    let idle_restarts = i64::from(rainbow.twinkle_seq != seq_before);
    let after_idle = project(&rainbow, t + Duration::from_millis(48), idle_restarts, 4);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before_idle,
        &after_idle,
        Some("BlinkIdle"),
        "Nyan active-to-idle blink conformance",
    );
    assert!(ok, "shipping idle-blink transition rejected: {why}");
    assert_eq!(idle_restarts, 0, "an idle blink never restarts a flare");
    assert_eq!(rainbow.twinkle_seq, seq_before);

    let corrupted = state(0, 1, 6, i64::from(seq_before) + 1, 1, 4);
    assert_eq!(
        before_idle["twinkle"], corrupted["twinkle"],
        "the old Boolean-only projection cannot distinguish this restart"
    );
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before_idle,
        &corrupted,
        Some("BlinkIdle"),
        "Nyan idle-restart negative control",
    );
    assert!(!ok, "an idle generation restart must fail conformance");

    let aged_at = t + Duration::from_millis(112);
    rainbow.tick(Some((1, 1)), aged_at, 0.0, true, true, g, &c, &mut out);
    let aged = project(&rainbow, aged_at, 0, 5);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &after_idle,
        &aged,
        Some("Age"),
        "Nyan flare-age conformance",
    );
    assert!(ok, "shipping flare-age transition rejected: {why}");
    assert_eq!(aged["remaining"], 1);

    let finished_at = t + Duration::from_millis(200);
    rainbow.tick(Some((1, 1)), finished_at, 0.0, true, true, g, &c, &mut out);
    let finished = project(&rainbow, finished_at, 0, 6);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &aged,
        &finished,
        Some("Finish"),
        "Nyan flare-finish conformance",
    );
    assert!(ok, "shipping flare-finish transition rejected: {why}");
    assert_eq!(finished["remaining"], 0);
    assert!(
        !rainbow.is_active(),
        "the bounded flare disarms the host wake"
    );
}

#[test]
fn twinkle_completes_and_resettles() {
    let g = geom();
    let t = Instant::now();
    let step16 = Duration::from_millis(16);
    let on_rung = RainbowConfig {
        paint: Some(0.25),
        ..blink_cfg()
    };
    let under_rung = RainbowConfig {
        paint: Some(0.24),
        ..blink_cfg()
    };
    let at_rest = RainbowConfig {
        paint: Some(0.0),
        ..blink_cfg()
    };
    let mut flared = CursorRainbow::default();
    let mut control = CursorRainbow::default();
    let (mut out_f, mut out_c) = (Vec::new(), Vec::new());
    flared.tick(Some((1, 1)), t, 0.0, true, true, g, &at_rest, &mut out_f);
    control.tick(Some((1, 1)), t, 0.0, true, true, g, &at_rest, &mut out_c);
    flared.tick(
        Some((1, 1)),
        t + step16,
        0.2,
        true,
        true,
        g,
        &on_rung,
        &mut out_f,
    );
    control.tick(
        Some((1, 1)),
        t + step16,
        0.2,
        true,
        true,
        g,
        &under_rung,
        &mut out_c,
    );
    assert!(flared.twinkling && !control.twinkling);
    let after = t + step16 + Duration::from_secs_f32(TWINKLE_DUR + 0.05);
    out_f.clear();
    out_c.clear();
    let ff = flared.tick(
        Some((1, 1)),
        after,
        0.0,
        true,
        true,
        g,
        &at_rest,
        &mut out_f,
    );
    let fc = control.tick(
        Some((1, 1)),
        after,
        0.0,
        true,
        true,
        g,
        &at_rest,
        &mut out_c,
    );
    assert_eq!(flared.flare_rung, 0, "the rung released on the way down");
    assert!(!flared.is_active(), "the flare completes and disarms");
    assert_eq!(out_f, out_c, "no residue: post-flare light == never-flared");
    assert_eq!(ff.fill, fc.fill, "post-flare fill == never-flared");
}

#[test]
fn the_halo_rides_the_paint_not_the_ignition_heat() {
    let g = geom();
    let t = Instant::now();
    let ink_and_reach = |paint: Option<f32>| -> (u64, i32) {
        let c = RainbowConfig { paint, ..cfg() };
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((2, 20)), t, 0.0, true, true, g, &c, &mut out);
        let ink = out
            .iter()
            .map(|q| (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64)
            .sum::<u64>();
        let cell_l = 20 * g.cw as i32;
        let reach = out.iter().map(|q| cell_l - q.x as i32).max().unwrap_or(0);
        (ink, reach)
    };
    let (ember_ink, ember_reach) = ink_and_reach(None);
    let (hot_ink, hot_reach) = ink_and_reach(Some(0.8));
    assert!(
        hot_ink > ember_ink * 3,
        "at zero cadence a painted caret still glows ({hot_ink} vs ember {ember_ink})"
    );
    assert!(
        hot_reach > ember_reach,
        "…and its rim reaches further ({hot_reach}px vs ember {ember_reach}px)"
    );

    let mut cr = CursorRainbow::default();
    let mut out = Vec::new();
    let mut last_cov = u32::MAX;
    let mut steps_down = 0;
    for i in 0..200u64 {
        let at = t + Duration::from_millis(i * 16);
        let paint = (-(i as f32 * 0.016) / 0.85).exp();
        let c = RainbowConfig {
            paint: Some(paint),
            ..cfg()
        };
        out.clear();
        cr.tick(Some((2, 20)), at, 0.0, true, true, g, &c, &mut out);
        let cov = out
            .iter()
            .map(|q| {
                ((q.color >> 16) & 0xff)
                    .max((q.color >> 8) & 0xff)
                    .max(q.color & 0xff)
            })
            .max()
            .unwrap_or(0);
        assert!(
            cov <= last_cov,
            "frame {i}: the halo brightened on a falling paint ({cov} > {last_cov})"
        );
        if cov < last_cov {
            steps_down += 1;
        }
        last_cov = cov;
    }
    assert!(
        steps_down >= 6,
        "the glow bleeds off in visible steps, not one ({steps_down})"
    );
    assert!(
        !cr.is_active(),
        "and at the end of the release the caret rests"
    );
}

#[test]
fn momentum_rungs_fire_once_each_and_never_on_the_way_down() {
    let g = geom();
    let t = Instant::now();
    let mut cr = CursorRainbow::default();
    let mut out = Vec::new();
    let mut i = 0u64;
    let mut tick = |cr: &mut CursorRainbow, paint: f32| {
        i += 1;
        let c = RainbowConfig {
            paint: Some(paint),
            ..blink_cfg()
        };
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(i * 16),
            0.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        cr.twinkle_seq
    };
    assert_eq!(tick(&mut cr, 0.0), 0, "at rest nothing fires");
    assert_eq!(tick(&mut cr, 0.24), 0, "under the first rung nothing fires");
    assert_eq!(tick(&mut cr, 0.25), 1, "reaching the first rung fires once");
    assert_eq!(tick(&mut cr, 0.26), 1, "hovering above it does not re-fire");
    assert_eq!(
        tick(&mut cr, 0.22),
        1,
        "dipping inside the hysteresis band does not release"
    );
    assert_eq!(
        tick(&mut cr, 0.25),
        1,
        "…so climbing back onto it does not re-fire"
    );
    assert_eq!(tick(&mut cr, 0.55), 2, "the second rung fires once");
    assert_eq!(
        tick(&mut cr, 0.10),
        2,
        "falling through every rung fires nothing"
    );
    assert_eq!(cr.flare_rung, 0, "…and releases them all");
    assert_eq!(
        tick(&mut cr, 1.0),
        3,
        "a one-tick jump to full paint fires ONE flare"
    );
    assert_eq!(cr.flare_rung, 4, "…holding the top rung");
    assert_eq!(tick(&mut cr, 0.0), 3, "a full stop fires nothing");
    assert_eq!(tick(&mut cr, 0.0), 3, "and rest stays silent");
}

#[test]
fn the_rim_turns_with_momentum_and_freezes_at_rest() {
    let g = geom();
    let t = Instant::now();
    let mut cr = CursorRainbow::default();
    let mut out = Vec::new();
    let hot = RainbowConfig {
        paint: Some(1.0),
        ..cfg()
    };
    let rest = RainbowConfig {
        paint: Some(0.0),
        ..cfg()
    };
    for i in 0..30u64 {
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(i * 16),
            0.0,
            true,
            true,
            g,
            &hot,
            &mut out,
        );
    }
    let turned = cr.spin;
    assert!(
        turned > 0.1,
        "half a second at full paint turns the rim ({turned} turns)"
    );
    let expect = RIM_SPIN_TURNS_PER_S * 29.0 * 0.016;
    assert!((turned - expect).abs() < 1e-3, "{turned} vs {expect}");
    out.clear();
    let a = cr.tick(
        Some((1, 1)),
        t + Duration::from_secs(2),
        0.0,
        true,
        true,
        g,
        &rest,
        &mut out,
    );
    let quads_a = out.clone();
    out.clear();
    let b = cr.tick(
        Some((1, 1)),
        t + Duration::from_secs(4),
        0.0,
        true,
        true,
        g,
        &rest,
        &mut out,
    );
    assert_eq!(
        cr.spin, turned,
        "the offset is frozen at rest, not integrated"
    );
    assert_eq!(a.fp, b.fp, "a resting rim's fingerprint is constant");
    assert_eq!(quads_a, out, "…and so are its bytes");
    assert!(!cr.is_active(), "the fingerprint law releases the tick");
}

#[test]
fn spin_is_continuous_across_its_wrap() {
    let g = geom();
    let t = Instant::now();
    let hot = RainbowConfig {
        paint: Some(1.0),
        ..cfg()
    };
    let mut cr = CursorRainbow::default();
    let mut prev: Option<Vec<u32>> = None;
    let mut out = Vec::new();
    let ceiling = continuity_ceiling(RIM_SPIN_TURNS_PER_S * 0.016) + 2;
    let mut worst = 0u32;
    let mut wrapped = false;
    for i in 0..320u64 {
        out.clear();
        let before = cr.spin;
        cr.tick(
            Some((2, 20)),
            t + Duration::from_millis(i * 16),
            0.0,
            true,
            true,
            g,
            &hot,
            &mut out,
        );
        assert!(
            (0.0..2.0).contains(&cr.spin),
            "spin left the sweep's period: {}",
            cr.spin
        );
        wrapped |= cr.spin < before;
        let colours: Vec<u32> = out.iter().map(|q| q.color).collect();
        if let Some(p) = &prev
            && p.len() == colours.len()
        {
            for (a, b) in p.iter().zip(&colours) {
                let d = rgb_max_delta(*a, *b);
                worst = worst.max(d);
                assert!(
                    d <= ceiling,
                    "frame {i}: a ring stepped one channel by {d} (ceiling {ceiling}) \
                     at spin {} — {a:#08x} -> {b:#08x}",
                    cr.spin
                );
            }
        }
        prev = Some(colours);
    }
    assert!(
        cr.spin > 0.0 && wrapped,
        "the walk must cross the wrap to prove it"
    );
    assert!(worst > 0, "…and the rim must actually turn");
}

#[test]
fn shared_phase_complete_frame_is_exact_across_the_family_ring() {
    use crate::cursor_glow::RAINBOW_PHASE_RING;

    let g = geom();
    let c = cfg();
    let now = Instant::now();
    let render = |phase: f32| {
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let frame = cr.tick_with_family_phase(
            Some((2, 11)),
            now,
            1.0,
            phase,
            rainbow_sweep_at(11, phase),
            false,
            true,
            g,
            &c,
            &mut out,
        );
        (frame.fill, frame.fp, out)
    };

    assert_eq!(render(0.0), render(RAINBOW_PHASE_RING));
    assert_ne!(
        render(0.0),
        render(rainbow_phase_from_unit_turn(0.25)),
        "a quarter-sweep must change a complete caret frame"
    );
}

#[test]
fn fallback_fill_flows_through_the_anchors_without_temporal_hue_steps() {
    let g = geom();
    let c = cfg();
    let now = Instant::now();
    let mut previous = None;
    let mut max_step = 0;
    let mut colours = BTreeSet::new();
    for sample in 0..=2400 {
        let phase = sample as f32 * (6.0 / 2400.0);
        let mut cursor = CursorRainbow::default();
        let mut out = Vec::new();
        let fill = cursor
            .tick_with_family_phase(
                Some((2, 17)),
                now,
                1.0,
                phase,
                rainbow_sweep_at(17, phase),
                false,
                true,
                g,
                &c,
                &mut out,
            )
            .fill
            .expect("enabled block fill");
        if let Some(prior) = previous {
            max_step = max_step.max(rgb_max_delta(prior, fill));
        }
        previous = Some(fill);
        colours.insert(fill);
    }
    let dt = (rainbow_sweep_at(17, 6.0 / 2400.0) - rainbow_sweep_at(17, 0.0)).abs();
    let ceiling = caret_continuity_ceiling(BASE_DARK_THEME, MIX_MAX, dt);
    assert!(
        max_step <= ceiling,
        "a 2.5 ms phase step changed one fill channel by {max_step} \
         (ceiling {ceiling})"
    );
    assert!(
        colours.len() > 256,
        "continuity must not mean a frozen/six-colour cursor: {} colours",
        colours.len()
    );
}

#[test]
fn the_rim_light_is_hue_invariant_and_monotone_in_paint() {
    const RIM_LIGHT_TOLERANCE: f64 = 1.05;
    const RIM_LIGHT_TOLERANCE_IDLE: f64 = 1.25;
    const GROUNDS: [u32; 2] = [0x0011_1318, 0x001A_1B26];
    const PAINTS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
    const POSITIONS: u32 = 128;
    let lum = |rgb: u32| f64::from(crate::color_math::relative_luminance(rgb));
    let raster_light = |quads: &[GlowQuad], ground: u32| -> f64 {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for q in quads {
            x0 = x0.min(u32::from(q.x));
            y0 = y0.min(u32::from(q.y));
            x1 = x1.max(u32::from(q.x) + u32::from(q.w));
            y1 = y1.max(u32::from(q.y) + u32::from(q.h));
        }
        if x1 <= x0 || y1 <= y0 {
            return 0.0;
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut px = vec![ground; w * h];
        let mut aurora = 0.0f64;
        for q in quads {
            for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                for xx in u32::from(q.x)..u32::from(q.x) + u32::from(q.w) {
                    let i = (yy - y0) as usize * w + (xx - x0) as usize;
                    px[i] = crate::spectrum::compose_on_glass(px[i], q.color, q.alpha);
                    aurora += lum(q.color);
                }
            }
        }
        let floor = lum(ground);
        let bytes: f64 = px.iter().map(|&p| (lum(p) - floor).max(0.0)).sum();
        bytes + f64::from(aterm_render::hdr::HDR_GLOW_BOOST) * aurora
    };

    let g = geom();
    let now = Instant::now();
    let mut worst_spread = [1.0f64; PAINTS.len()];
    let mut worst_at: [String; PAINTS.len()] = Default::default();
    for (name, base) in shipped_caret_bases() {
        for ground in GROUNDS {
            let mut light = vec![[0.0f64; PAINTS.len()]; POSITIONS as usize + 1];
            for (pi, &paint) in PAINTS.iter().enumerate() {
                let c = RainbowConfig {
                    base,
                    paint: Some(paint),
                    blinking: false,
                    ground: Some(ground),
                    ..cfg()
                };
                for step in 0..=POSITIONS {
                    let field = step as f32 / POSITIONS as f32;
                    let mut cursor = CursorRainbow::default();
                    let mut out = Vec::new();
                    cursor.tick_with_family_phase(
                        Some((2, 17)),
                        now,
                        0.0,
                        0.0,
                        field,
                        false,
                        true,
                        g,
                        &c,
                        &mut out,
                    );
                    light[step as usize][pi] = raster_light(&out, ground);
                }
            }
            for (pi, &paint) in PAINTS.iter().enumerate() {
                let col: Vec<f64> = light.iter().map(|row| row[pi]).collect();
                let lo = col.iter().copied().fold(f64::INFINITY, f64::min);
                let hi = col.iter().copied().fold(0.0, f64::max);
                assert!(
                    lo > 0.0,
                    "{name} over #{ground:06X} at paint {paint}: no rim light"
                );
                let spread = hi / lo;
                if spread > worst_spread[pi] {
                    worst_spread[pi] = spread;
                    worst_at[pi] = format!("{name} over #{ground:06X} {lo:.4}..{hi:.4}");
                }
            }
            for (step, row) in light.iter().enumerate() {
                for pi in 1..PAINTS.len() {
                    assert!(
                        row[pi] > row[pi - 1],
                        "{name} over #{ground:06X} at sweep {}/{POSITIONS}: the rim's \
                         light did not rise with the paint {} -> {}: {:.4} -> {:.4}",
                        step,
                        PAINTS[pi - 1],
                        PAINTS[pi],
                        row[pi - 1],
                        row[pi]
                    );
                }
            }
        }
    }
    for (pi, &paint) in PAINTS.iter().enumerate() {
        let tolerance = if paint > 0.0 {
            RIM_LIGHT_TOLERANCE
        } else {
            RIM_LIGHT_TOLERANCE_IDLE
        };
        assert!(
            worst_spread[pi] <= tolerance,
            "at paint {paint} the rim's light depends on where the spin parked it: \
             ×{:.3} at {} (tolerance ×{tolerance})",
            worst_spread[pi],
            worst_at[pi]
        );
    }

    let mut lo = f64::INFINITY;
    let mut hi = 0.0f64;
    for step in 0..=POSITIONS {
        let sweep = step as f32 / POSITIONS as f32;
        let l = lum(shade(spectrum_at(sweep, 0.0), SAT_MAX, VAL_MAX));
        lo = lo.min(l);
        hi = hi.max(l);
    }
    assert!(
        hi / lo > 10.0,
        "the un-normalised arc spans only ×{:.2} in light — this gate cannot \
         see the defect it exists for",
        hi / lo
    );
    let mid = crate::spectrum::spectrum_crossing_position();
    let half = crate::spectrum::spectrum_crossing_span() * 0.5;
    let flank_lo = lum(shade(
        crate::spectrum::spectrum(mid - half),
        SAT_MAX,
        VAL_MAX,
    ));
    let flank_hi = lum(shade(
        crate::spectrum::spectrum(mid + half),
        SAT_MAX,
        VAL_MAX,
    ));
    let (dim, bright) = (flank_lo.min(flank_hi), flank_lo.max(flank_hi));
    assert!(
        bright / dim > 2.0,
        "the raw arc's light steps only ×{:.2} across the crossing — the incident \
         this law answers for could not have happened on it",
        bright / dim
    );
}

#[test]
fn the_caret_cools_with_its_trail_and_not_with_the_ignition_heat() {
    use crate::cursor_trail::TypingCadence;
    use crate::typing_momentum::TypingMomentum;

    const BASE: u32 = 0x0050_FA7B;
    const FIELD: f32 = 0.05;
    const KEYS: u32 = 43;
    const GAP: Duration = Duration::from_millis(92);

    let g = geom();
    let t0 = Instant::now();
    let mut cadence = TypingCadence::default();
    let mut spine = TypingMomentum::default();
    let mut last_key = t0;
    for k in 0..KEYS {
        last_key = t0 + GAP * k;
        cadence.on_keystroke(last_key);
        spine.advance(last_key);
    }

    let arc = spectrum_at(FIELD, 0.0);
    let render = |at: Instant, paint: Option<f32>| -> (u32, CursorRainbow) {
        let c = RainbowConfig {
            base: Some(BASE),
            head_rgb: Some(arc),
            paint,
            ..cfg()
        };
        let mut cursor = CursorRainbow::default();
        let mut out = Vec::new();
        let fill = cursor
            .tick_with_family_phase(
                Some((2, 17)),
                at,
                cadence.intensity(at),
                0.0,
                FIELD,
                false,
                true,
                g,
                &c,
                &mut out,
            )
            .fill
            .expect("enabled block fill");
        (fill, cursor)
    };

    let quarter = last_key + Duration::from_millis(250);
    assert_eq!(
        cadence.intensity(quarter),
        0.0,
        "the ignition heat must really be dead a quarter second after a burst \
         — if it is not, this test is not exercising the defect"
    );
    assert!(
        spine.value(quarter) > 0.8,
        "…while the ribbon's own spine is still all but full: {}",
        spine.value(quarter)
    );

    let (old, _) = render(quarter, None);
    assert!(
        rgb_max_delta(old, BASE) < 32,
        "the OLD law must still collapse to the cursor colour (#{old:06X} vs \
         #{BASE:06X}) — the control has stopped controlling"
    );
    assert!(
        rgb_max_delta(old, arc) > 3 * rgb_max_delta(old, BASE),
        "…and it must be nowhere near the arc it is leading (#{arc:06X})"
    );

    let (new, state) = render(quarter, Some(spine.value(quarter)));
    assert!(
        rgb_max_delta(new, arc) < rgb_max_delta(new, BASE),
        "the caret must wear its trail's colour while the trail is painted: \
         #{new:06X} is {} from the arc #{arc:06X} and {} from the base \
         #{BASE:06X}",
        rgb_max_delta(new, arc),
        rgb_max_delta(new, BASE)
    );
    assert!(
        rgb_max_delta(new, old) > 64,
        "…and that must be a REAL change from the shipped caret, not a nudge"
    );
    assert!(
        state.is_active(),
        "a painted caret must keep the host's tick armed, or it freezes \
         mid-cool and snaps to the base on the next unrelated frame"
    );

    const STEP: Duration = Duration::from_millis(16);
    let colour_span = ((MIX_MAX - MIX_IDLE) + (SAT_MAX - SAT_IDLE) + (VAL_MAX - VAL_IDLE)) * 255.0;
    let ceiling_for = |dpaint: f32| (colour_span * dpaint.abs()).ceil() as u32 + 4;
    let mut previous: Option<(u32, u32, f32)> = None;
    let mut worst_step = 0u32;
    let mut cooled = 0usize;
    let mut frames = 0usize;
    for frame in 0..=190u32 {
        let at = last_key + STEP * frame;
        let paint = spine.value(at);
        let (fill, _) = render(at, Some(paint));
        let toward_base = rgb_max_delta(fill, BASE);
        if paint >= CARET_LIGHT_KNEE
            && let Some((prev_fill, prev_base_delta, prev_paint)) = previous
        {
            let step = rgb_max_delta(prev_fill, fill);
            let ceiling = ceiling_for(paint - prev_paint);
            worst_step = worst_step.max(step);
            assert!(
                step <= ceiling,
                "frame {frame}: the cooling caret jumped one channel by \
                 {step} (ceiling {ceiling}) — #{prev_fill:06X} -> #{fill:06X}"
            );
            assert!(
                toward_base <= prev_base_delta + 2,
                "frame {frame}: the caret warmed back up mid-release \
                 ({prev_base_delta} -> {toward_base})"
            );
            cooled += usize::from(toward_base < prev_base_delta);
        }
        frames += 1;
        previous = Some((fill, toward_base, paint));
    }
    assert!(
        frames > 180 && cooled > 40,
        "the release walk must actually move: {cooled} cooling steps over \
         {frames} frames"
    );
    assert!(
        worst_step > 0,
        "…and it must not be a frozen block: worst channel step {worst_step}"
    );

    let long = last_key + Duration::from_secs(20);
    let (settled, settled_state) = render(long, Some(spine.value(long)));
    assert_eq!(
        spine.value(long),
        0.0,
        "the spine must be provably at rest before the settled claim"
    );
    assert_eq!(
        settled,
        render(long, None).0,
        "a dead spine must leave the caret exactly where the energy law puts \
         it — the block IS the cursor at rest"
    );
    assert!(
        !settled_state.is_active(),
        "…and the host's tick must disarm again"
    );
}

fn shipped_caret_bases() -> Vec<(String, Option<u32>)> {
    let mut bases: Vec<(String, Option<u32>)> = vec![
        ("<none: theme-polar>".to_string(), None),
        ("<white>".to_string(), Some(0x00FF_FFFF)),
        ("<near-black>".to_string(), Some(0x0016_161C)),
    ];
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name)
            .unwrap_or_else(|| panic!("built-in theme {name} must resolve"));
        bases.push((name.to_string(), Some(scheme.to_theme_parts().cursor)));
    }
    assert!(
        bases.len() >= 12,
        "the shipped theme roster must be enumerated, not empty: {bases:?}"
    );
    bases
}

#[test]
fn caret_and_ribbon_share_one_live_clock_through_a_streaming_run() {
    use crate::cursor_glow::{CursorGlow, GlowConfig};

    let g = geom();
    let glow_cfg = GlowConfig {
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    };
    let body_cfg = cfg();
    let t0 = Instant::now();
    let row = 2u16;
    let mut glow = CursorGlow::default();
    let mut body = CursorRainbow::default();
    let mut glow_out = Vec::new();
    let mut body_out = Vec::new();
    glow.tick(Some((row, 0)), t0, &glow_cfg, g, &mut glow_out);

    let mut phases = Vec::new();
    let mut colours = BTreeSet::new();
    for key in 0..18u64 {
        let press_at = t0 + Duration::from_millis(key * 72 + 1);
        let frame_at = press_at + Duration::from_millis(8);
        let col = (key + 1) as u16;
        glow.note_typed(press_at);
        glow.tick(Some((row, col)), frame_at, &glow_cfg, g, &mut glow_out);
        let family_phase = glow.rainbow_phase();
        phases.push(family_phase);

        body_out.clear();
        let family_field = glow.rainbow_field();
        let body_frame = body.tick_with_family_phase(
            Some((row, col)),
            frame_at,
            1.0,
            family_phase,
            family_field,
            false,
            true,
            g,
            &body_cfg,
            &mut body_out,
        );
        let ribbon_colour = spectrum_at(family_field, 0.0);
        colours.insert(ribbon_colour);
        assert_white_lift_of(
            body_frame.fill.expect("enabled caret fill"),
            caret_law(BASE_DARK_THEME, ribbon_colour, MIX_MAX, 1.0),
            &format!("key {key}: caret and ribbon diverged at phase {family_phase}, col {col}"),
        );
    }

    assert!(
        Duration::from_millis(17 * 72 + 9) > Duration::from_secs(1),
        "fixture must exercise more than one second of live engine time"
    );
    assert!(
        phases.last().copied().unwrap_or(0.0) > phases[1] + 0.05,
        "the real ribbon clock must advance non-vacuously: {phases:?}"
    );
    assert!(
        colours.len() >= 5,
        "the streaming run must traverse the unroll's colours: {colours:#x?}"
    );
}

#[test]
fn the_settle_split_is_a_partition_of_still_moving() {
    let levels = [0.0_f32, SETTLED_ENERGY, SETTLED_ENERGY + 1e-4, 0.3, 1.0];
    for &energy in &levels {
        for &paint in &levels {
            for twinkling in [false, true] {
                for flaring in [false, true] {
                    for repeated in [false, true] {
                        let rb = CursorRainbow {
                            energy,
                            paint,
                            twinkling,
                            flaring,
                            fp_last: 7,
                            fp_prev: if repeated { 7 } else { 8 },
                            ..CursorRainbow::default()
                        };
                        let moving = energy > SETTLED_ENERGY
                            || paint > SETTLED_ENERGY
                            || twinkling
                            || flaring;
                        let at = format!(
                            "energy={energy} paint={paint} twinkling={twinkling} \
                             flaring={flaring} repeated={repeated}"
                        );
                        assert_eq!(rb.is_active() || rb.settling(), moving, "cover: {at}");
                        assert!(!(rb.is_active() && rb.settling()), "disjoint: {at}");
                    }
                }
            }
        }
    }
}

fn host_paint_spine(peak: f32) -> Vec<f32> {
    let mut disp = peak;
    let mut out = vec![disp];
    for ms in 1..=20_000_u32 {
        let target = peak * (-(ms as f32 / 1000.0) / 2.0).exp();
        let rise = target - disp;
        let ease = if rise > 0.0 { 0.10 } else { 0.85 };
        disp += rise * (1.0 - (-0.001_f32 / ease).exp());
        if disp < 0.005 {
            disp = 0.0;
        }
        out.push(disp);
    }
    out
}

#[test]
fn the_caret_colour_release_is_drawn_to_its_last_step_on_demand() {
    const HEAD: u32 = 0x00FF_5A4E;
    let hands: [(f32, u64, bool, Option<u32>, bool); 6] = [
        (1.00, 8, true, Some(0x0050_FA7B), true),
        (1.00, 16, true, Some(0x0050_FA7B), true),
        (0.45, 8, true, Some(0x0050_FA7B), false),
        (1.00, 8, false, Some(0x0016_161C), true),
        (0.70, 16, true, None, false),
        (1.00, 33, true, Some(0x0050_FA7B), true),
    ];
    for (hand, &(peak, step_ms, dark, base, blinking)) in hands.iter().enumerate() {
        let frame = Duration::from_millis(step_ms);
        let spine = host_paint_spine(peak);
        let t0 = Instant::now();
        let cur = Some((2_u16, 5_u16));
        let cfg_at = |ms: u64| RainbowConfig {
            light: crate::cursor_glow::Light::OFF,
            enabled: true,
            intensity: 1.0,
            blinking,
            base,
            head_rgb: Some(HEAD),
            paint: Some(spine[(ms as usize).min(spine.len() - 1)]),
            ground: None,
            flare_at: None,
        };
        let energy_at = |ms: u64| peak * 0.5_f32.powf(ms as f32 / 1000.0 / 0.22);
        let tick = |rb: &mut CursorRainbow, ms: u64, out: &mut Vec<GlowQuad>| {
            out.clear();
            rb.tick_with_family_phase(
                cur,
                t0 + Duration::from_millis(ms),
                energy_at(ms),
                412.0,
                0.37,
                false,
                dark,
                geom(),
                &cfg_at(ms),
                out,
            )
            .fp
        };
        let mut out = Vec::new();

        let mut level = CursorRainbow::default();
        let mut level_fp: BTreeMap<u64, u64> = BTreeMap::new();
        let mut ms = 0_u64;
        let level_release = loop {
            level_fp.insert(ms, tick(&mut level, ms, &mut out));
            if !level.is_active() && !level.settling() {
                break ms;
            }
            ms += step_ms;
            assert!(ms < 30_000, "hand {hand}: the level twin never released");
        };
        let changes: BTreeSet<u64> = level_fp
            .iter()
            .zip(level_fp.iter().skip(1))
            .filter(|((_, a), (_, b))| a != b)
            .map(|(_, (at, _))| *at)
            .collect();
        assert!(
            changes.len() > 30,
            "hand {hand}: {} visible steps is not the release under test",
            changes.len()
        );

        let mut parked = CursorRainbow::default();
        let mut ms = 0_u64;
        loop {
            tick(&mut parked, ms, &mut out);
            if !parked.is_active() {
                break;
            }
            ms += step_ms;
        }
        let parked_at = ms;
        let never_drawn = changes.iter().filter(|&&at| at > parked_at).count();
        assert!(
            parked.settling() && parked_at < level_release && never_drawn > 0,
            "hand {hand}: negative control — the fingerprint law alone parked at \
             {parked_at} ms (settling {}), level release {level_release} ms, {never_drawn} \
             steps never drawn",
            parked.settling()
        );

        let mut demand = CursorRainbow::default();
        let mut demand_ticks = 0_usize;
        let mut ms = 0_u64;
        let demand_release = loop {
            let fp = tick(&mut demand, ms, &mut out);
            demand_ticks += 1;
            let now = t0 + Duration::from_millis(ms);
            let next = if demand.is_active() {
                Some(now + frame)
            } else {
                demand.next_change_deadline(now, frame)
            };
            let Some(next) = next else {
                break ms;
            };
            let step = next.saturating_duration_since(now).as_millis() as u64;
            assert!(
                step > 0 && step.is_multiple_of(step_ms),
                "hand {hand}: a deadline lands on the frame grid (+{step} ms at {ms} ms)"
            );
            assert!(
                step <= SETTLE_POLL_CAP.as_millis() as u64,
                "hand {hand}: a deadline stays inside the poll cap (+{step} ms)"
            );
            let mut fork = demand.clone();
            let mut k = ms + step_ms;
            while k < ms + step {
                let forked = tick(&mut fork, k, &mut out);
                assert_eq!(
                    forked,
                    fp,
                    "hand {hand}: the demand lane slept through a visible step at {k} ms \
                     (woke {ms} ms, next {} ms)",
                    ms + step
                );
                k += step_ms;
            }
            ms += step;
            assert!(ms < 30_000, "hand {hand}: the demand twin never released");
        };
        assert!(
            !demand.is_active() && !demand.settling(),
            "hand {hand}: the demand lane parks only on the caret's own release"
        );
        assert_eq!(
            demand_release, level_release,
            "hand {hand}: the demand lane releases on the level lane's frame"
        );
        assert!(
            demand_ticks <= level_fp.len(),
            "hand {hand}: the demand lane may never cost more than the frame train"
        );
    }
}

#[test]
fn the_release_deadline_is_one_dry_run_per_tick() {
    let spine = host_paint_spine(1.0);
    let t0 = Instant::now();
    let mut rb = CursorRainbow::default();
    let mut out = Vec::new();
    let frame = Duration::from_millis(16);
    let cfg_at = |ms: u64| RainbowConfig {
        paint: Some(spine[ms as usize]),
        ..cfg()
    };
    let mut ms = 0_u64;
    while !rb.settling() {
        out.clear();
        rb.tick(
            Some((2, 5)),
            t0 + Duration::from_millis(ms),
            0.0,
            false,
            true,
            geom(),
            &cfg_at(ms),
            &mut out,
        );
        ms += 16;
        assert!(ms < 10_000, "fixture: the caret never settled");
    }
    let now = t0 + Duration::from_millis(ms - 16);
    let first = rb
        .next_change_deadline(now, frame)
        .expect("a settling caret owns a wake");
    assert_eq!(
        rb.next_change_deadline(now + Duration::from_millis(1), frame),
        Some(first.max(now + Duration::from_millis(1) + frame)),
        "a second plan before the next tick reads the memo"
    );
    out.clear();
    rb.tick(
        Some((2, 5)),
        first,
        0.0,
        false,
        true,
        geom(),
        &cfg_at(ms),
        &mut out,
    );
    out.clear();
    rb.tick(
        Some((2, 5)),
        first + frame,
        0.0,
        false,
        true,
        geom(),
        &RainbowConfig {
            enabled: false,
            ..cfg_at(ms)
        },
        &mut out,
    );
    assert!(
        !rb.settling() && rb.next_change_deadline(first + frame, frame).is_none(),
        "an inert tick drops the latched inputs and the release with them"
    );
}

#[test]
fn flare_pop_drives_fingerprint_on_a_pinned_white_cursor() {
    let t0 = Instant::now();
    let wide_geom = Geom {
        cw: 93,
        ch: 186,
        rows: 12,
        cols: 40,
        win_w: 3720,
        win_h: 2232,
        ..geom()
    };
    let mut out_0 = Vec::new();
    let f_0 = CursorRainbow::default().tick(
        Some((2, 5)),
        t0,
        0.0,
        false,
        true,
        wide_geom,
        &RainbowConfig {
            paint: Some(1.0),
            flare_at: Some(t0),
            ..cfg()
        },
        &mut out_0,
    );
    let mut out_3ms = Vec::new();
    let f_3ms = CursorRainbow::default().tick(
        Some((2, 5)),
        t0,
        0.0,
        false,
        true,
        wide_geom,
        &RainbowConfig {
            paint: Some(1.0),
            flare_at: Some(t0 - Duration::from_millis(3)),
            ..cfg()
        },
        &mut out_3ms,
    );
    assert_eq!(
        f_0.fill, f_3ms.fill,
        "fixture: white hold keeps fill identical across the first 3 ms of the flare"
    );
    assert_ne!(
        out_0, out_3ms,
        "fixture: flare_pop decay moves the halo rings during the white hold"
    );
    assert_ne!(
        f_0.fp, f_3ms.fp,
        "the flare ring pop must change the frame fingerprint even when fill and phase are identical"
    );
}

#[test]
fn meteor_flare_and_relight_hold_keep_caret_active_and_wake_for_decay() {
    let t0 = Instant::now();
    let frame = Duration::from_millis(20);
    let mut rb = CursorRainbow::default();
    let mut out = Vec::new();
    let steady_cfg = |paint: f32, flare_at: Option<Instant>| RainbowConfig {
        blinking: false,
        paint: Some(paint),
        flare_at,
        ..cfg()
    };

    for step in 0..9_u64 {
        let at = t0 + Duration::from_millis(step * 20);
        out.clear();
        rb.tick_with_family_phase(
            Some((2, 5)),
            at,
            0.0,
            256.0,
            0.25,
            false,
            true,
            geom(),
            &steady_cfg(1.0, Some(t0)),
            &mut out,
        );
        assert!(
            rb.is_active() && !rb.settling(),
            "step {step} ({} ms): caret must stay active while flare_at is within 180 ms",
            step * 20
        );
    }

    let boundary = t0 + Duration::from_millis(180);
    out.clear();
    rb.tick_with_family_phase(
        Some((2, 5)),
        boundary,
        0.0,
        256.0,
        0.25,
        false,
        true,
        geom(),
        &steady_cfg(1.0, None),
        &mut out,
    );
    assert_eq!(
        rb.next_change_deadline(boundary, frame),
        Some(boundary + frame),
        "flat live paint plateau at the end of the 180 ms relight hold must wake on the next frame"
    );
}

#[test]
fn cursor_cell_and_visibility_drive_fp_and_rim_memo_skips_repeat_rasters() {
    let t0 = Instant::now();
    let g = geom();
    let c = RainbowConfig {
        blinking: false,
        paint: Some(0.8),
        ..cfg()
    };
    let mut rb = CursorRainbow::default();
    let mut out_a = Vec::new();
    let mut out_b = Vec::new();

    let fa = rb.tick_with_family_phase(
        Some((2, 5)),
        t0,
        0.0,
        128.0,
        0.25,
        false,
        true,
        g,
        &c,
        &mut out_a,
    );
    let fb = rb.tick_with_family_phase(
        Some((3, 5)),
        t0,
        0.0,
        128.0,
        0.25,
        false,
        true,
        g,
        &c,
        &mut out_b,
    );
    assert_eq!(fa.fill, fb.fill);
    assert_ne!(out_a, out_b, "halo rings move with the cursor row");
    assert_ne!(
        fa.fp, fb.fp,
        "a cursor row move must change RainbowFrame.fp"
    );

    let mut rest_rb = CursorRainbow::default();
    out_a.clear();
    let f_vis = rest_rb.tick(Some((0, 0)), t0, 0.0, false, true, g, &cfg(), &mut out_a);
    out_b.clear();
    let f_hid = rest_rb.tick(None, t0, 0.0, false, true, g, &cfg(), &mut out_b);
    assert!(!out_a.is_empty() && out_b.is_empty());
    assert_ne!(
        f_vis.fp, f_hid.fp,
        "hiding the cursor must change RainbowFrame.fp"
    );
    assert!(
        !rest_rb.is_active(),
        "a resting cursor visibility change does not arm the animation timer"
    );

    let mut memo_rb = CursorRainbow::default();
    out_a.clear();
    let first = memo_rb.tick_with_family_phase(
        Some((2, 5)),
        t0,
        0.0,
        128.0,
        0.25,
        false,
        true,
        g,
        &c,
        &mut out_a,
    );
    let first_quads = out_a.clone();
    memo_rb.rim_scratch = Vec::new();
    out_b.clear();
    let second = memo_rb.tick_with_family_phase(
        Some((2, 5)),
        t0,
        0.0,
        128.0,
        0.25,
        false,
        true,
        g,
        &c,
        &mut out_b,
    );
    assert_eq!(
        memo_rb.rim_scratch.capacity(),
        0,
        "rim_memo hit must skip both equalise_rim_light and rim_light_peak rasterizations"
    );
    assert_eq!((first.fill, first.fp), (second.fill, second.fp));
    assert_eq!(first_quads, out_b);
}
