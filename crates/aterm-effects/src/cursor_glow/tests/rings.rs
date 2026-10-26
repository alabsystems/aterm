// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Landing rings and halo culling.

use super::*;

/// How far from `centre` the farthest and the nearest pixel `quads` light stand: a ring's rim
/// lies between its inner and outer radius.
fn rim_reach(quads: &[GlowQuad], (cx, cy): (f32, f32)) -> Option<(f32, f32)> {
    let mut reach: Option<(f32, f32)> = None;
    for q in quads.iter().filter(|q| q.color != 0) {
        for y in q.y..q.y + q.h {
            for x in q.x..q.x + q.w {
                let d = (f32::from(x) + 0.5 - cx).hypot(f32::from(y) + 0.5 - cy);
                reach = Some(reach.map_or((d, d), |(near, far)| (near.min(d), far.max(d))));
            }
        }
    }
    reach
}

/// A ring's radius and rim at `ms` of a 300 ms life on `g`, as [`CursorGlow::emit_ring_rim`]
/// lays it for a hop (`scale` 1).
fn rim_at(g: Geom, ms: u64, life_s: f32, scale: f32) -> (f32, f32) {
    let t = (ms as f32 / 1000.0 / life_s).clamp(0.0, 1.0);
    (
        (0.7 + t) * g.ch as f32 * classic_ring_radius_factor(scale),
        (g.ch as f32 / 8.0).max(2.0),
    )
}

/// Whether `quads` lie on the ring about `centre` ([`rim_at`]), within two pixels' slack (the
/// rim is laid in whole pixels from a centre cut to one).
fn on_the_rim(quads: &[GlowQuad], centre: (f32, f32), (radius, rim): (f32, f32)) -> bool {
    rim_reach(quads, centre)
        .is_some_and(|(near, far)| near >= radius - rim - 2.0 && far <= radius + 2.0)
}

#[test]
fn rainbow_landing_ring_light_theme_forks_to_source_over() {
    let g = geom();
    let born = Instant::now();
    let at = born + Duration::from_millis(60);
    let ring = Ring {
        cx: 100.0,
        cy: 40.0,
        born,
        life: 0.30,
        scale: 1.0,
        caret: Some((2, 12)),
    };

    let c_dark = cfg(GlowStyle::RainbowKitty, true);
    let dark = CursorGlow {
        ring: Some(ring),
        ..Default::default()
    };
    let (mut d_out, mut d_h) = (Vec::new(), Vec::new());
    dark.emit_ring(at, &c_dark, g, None, &mut d_out, &mut d_h);
    assert!(
        !d_h.is_empty() && d_out.is_empty(),
        "dark draws the additive round shockwave as halos"
    );
    assert!(
        d_h.iter().all(|h| h.mode != HaloMode::Over),
        "the dark ring is additive, never source-over"
    );

    let mut c_light = cfg(GlowStyle::RainbowKitty, true);
    c_light.dark_theme = false;
    let light = CursorGlow {
        ring: Some(ring),
        ..Default::default()
    };
    let (mut l_out, mut l_h) = (Vec::new(), Vec::new());
    light.emit_ring(at, &c_light, g, None, &mut l_out, &mut l_h);
    assert!(l_out.is_empty(), "no additive shockwave on light");
    assert!(
        !l_h.is_empty() && l_h.iter().all(|h| h.mode == HaloMode::Over),
        "the light ring is a source-over darken veil"
    );
    assert!(
        l_h.iter().all(|h| {
            let cap = (h.color >> 24) & 0xff;
            (1..=120).contains(&cap)
        }),
        "the light ring veil is capped for legibility"
    );
}

#[test]
fn every_style_lands_a_veil_and_a_rim_on_light() {
    let g = geom();
    let born = Instant::now();
    let ring = Ring {
        cx: 100.0,
        cy: 40.0,
        born,
        life: 0.30,
        scale: 1.0,
        caret: Some((2, 12)),
    };
    for style in [
        GlowStyle::Fire,
        GlowStyle::Water,
        GlowStyle::Laser,
        GlowStyle::Phaser,
        GlowStyle::Comet,
    ] {
        let mut c = cfg(style, true);
        c.dark_theme = false;
        let glow = CursorGlow {
            ring: Some(ring),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(60),
            &c,
            g,
            None,
            &mut out,
            &mut halos,
        );
        if style == GlowStyle::Water {
            assert!(halos.is_empty(), "the light ripples are quads");
            assert!(
                !out.is_empty()
                    && out
                        .iter()
                        .all(|q| q.alpha > 0 && f32::from(q.alpha) <= LIGHT_INK_ALPHA_CAP),
                "the light ripples are source-over ink under the over-text ceiling"
            );
            continue;
        }
        assert!(
            !out.is_empty()
                && out
                    .iter()
                    .all(|q| q.alpha > 0 && f32::from(q.alpha) <= LIGHT_INK_ALPHA_CAP),
            "{style:?}: the light ring's rim is source-over ink under the over-text ceiling"
        );
        assert!(
            on_the_rim(&out, (100.0, 40.0), rim_at(g, 60, 0.30, 1.0)),
            "{style:?}: the light ring's rim is round about its landing"
        );
        assert!(
            !halos.is_empty() && halos.iter().all(|h| h.mode == HaloMode::Over),
            "{style:?}: the light ring is a source-over veil"
        );
        assert!(
            halos
                .iter()
                .all(|h| (1..=120).contains(&((h.color >> 24) & 0xff))),
            "{style:?}: the veil is capped for legibility"
        );
    }
}

/// On dark a capped-impact landing's light reaches at least 1.8 times as far as a hop's, in every
/// style.
#[test]
fn classic_landing_ring_expands_wider_with_the_jump() {
    let g = geom();
    let born = Instant::now();
    let extent = |style: GlowStyle, scale: f32| -> i32 {
        let c = cfg(style, true);
        let glow = CursorGlow {
            ring: Some(Ring {
                cx: 100.0,
                cy: 40.0,
                born,
                life: 0.30,
                scale,
                caret: None,
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(120),
            &c,
            g,
            None,
            &mut out,
            &mut halos,
        );
        let quads = out
            .iter()
            .map(|q| ((q.x as i32 + q.w as i32 - 1) - 100).max(100 - q.x as i32))
            .max()
            .unwrap_or(0);
        let halos = halos
            .iter()
            .map(|h| ((h.x as i32 + h.w as i32 - 1) - 100).max(100 - h.x as i32))
            .max()
            .unwrap_or(0);
        quads.max(halos)
    };
    for style in [
        GlowStyle::Phaser,
        GlowStyle::Comet,
        GlowStyle::Sparkle,
        GlowStyle::Laser,
        GlowStyle::Lumen,
        GlowStyle::Beam,
        GlowStyle::Fire,
        GlowStyle::Water,
    ] {
        let hop = extent(style, 1.0);
        let leap = extent(style, 3.5);
        assert!(hop > 0, "{style:?}: the hop ring draws");
        assert!(
            leap as f32 >= hop as f32 * 1.8,
            "{style:?}: a capped-impact ring reaches {leap} px, a hop {hop} px"
        );
    }
}

#[test]
fn the_dark_landing_ring_is_a_round_rim_over_a_soft_bloom_in_its_row() {
    let g = geom();
    let born = Instant::now();
    let c = cfg(GlowStyle::Phaser, true);
    let lines = |quads: &[GlowQuad], halos: &[RainHalo], cx: i32, cy: i32| {
        let mut glass = Glass::new(g, 0);
        glass.add_quads(quads);
        glass.add_halos(halos);
        let across: Vec<i32> = (0..i32::from(g.win_w))
            .map(|x| glass.level(cy, x))
            .collect();
        let down: Vec<i32> = (0..i32::from(g.win_h))
            .map(|y| glass.level(y, cx))
            .collect();
        (across, down)
    };
    let steepest = |l: &[i32]| l.windows(2).map(|w| (w[1] - w[0]).abs()).max().unwrap_or(0);
    let span = |l: &[i32]| l.iter().filter(|&&v| v > 0).count();
    for ms in [10u64, 60, 120, 170] {
        let ring = Ring {
            cx: 100.0,
            cy: 40.0,
            born,
            life: 0.30,
            scale: 1.0,
            caret: None,
        };
        let glow = CursorGlow {
            ring: Some(ring),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(ms),
            &c,
            g,
            None,
            &mut out,
            &mut halos,
        );
        assert!(
            on_the_rim(&out, (100.0, 40.0), rim_at(g, ms, 0.30, 1.0)),
            "the ring's rim is round about its landing at {ms}ms: {:?}",
            rim_reach(&out, (100.0, 40.0))
        );
        assert!(!halos.is_empty(), "the ring blooms at {ms}ms");
        let (across, down) = lines(&[], &halos, 100, 40);
        let peak = *across.iter().max().unwrap_or(&0);
        assert!(peak > 0, "the bloom is lit at {ms}ms");
        assert!(
            across[100] == peak,
            "brightest at its middle at {ms}ms: {} of {peak}",
            across[100]
        );
        assert!(
            steepest(&across) * 4 <= peak.max(4),
            "a hard edge in the bloom at {ms}ms: {} in one pixel of {peak}",
            steepest(&across)
        );
        let (w, h) = (span(&across), span(&down));
        let reach = (CursorGlow::LIGHT_VEIL_REACH_CELLS * g.cw as f32) as usize;
        assert!(
            h > 0 && h <= g.ch && w <= 2 * reach + 2,
            "in its row at {ms}ms: {w} px across, {h} down"
        );
        let s = ((0.6 + 1.0 * (ms as f32 / 300.0)) * g.ch as f32) as i32;
        let premul = aterm_render::premul_rgb(c.accent, 160);
        let mut bars = Vec::new();
        for (x, y, w, h) in [
            (100 - s, 40 - s, 2 * s, 2),
            (100 - s, 40 + s - 2, 2 * s, 2),
            (100 - s, 40 - s + 2, 2, 2 * s - 4),
            (100 + s - 2, 40 - s + 2, 2, 2 * s - 4),
        ] {
            crate::effect_util::push_fx_rect(&mut bars, g, x, y, w, h, premul);
        }
        let (old, _) = lines(&bars, &[], 100, 40);
        let old_peak = *old.iter().max().unwrap_or(&0);
        assert!(
            steepest(&old) * 4 > old_peak,
            "control: the outline's edge is hard at {ms}ms"
        );
    }
}

#[test]
fn halo_centres_past_the_box_stay_true_and_negative_centres_cull() {
    let g = geom(); // box [0,320) x [0,96)
    let mut out = Vec::new();
    push_halo(&mut out, g, 322.0, 48.0, 5.0, 5.0, 0x0020_2020);
    assert!(!out.is_empty(), "the sliver band still draws");
    assert!(
        out.iter().all(|h| h.cx == 322),
        "the falloff centre is the TRUE centre, not the clamped edge: {:?}",
        out.iter().map(|h| h.cx).collect::<Vec<_>>()
    );
    let mut neg = Vec::new();
    push_halo(&mut neg, g, -2.0, 48.0, 5.0, 5.0, 0x0020_2020);
    assert!(neg.is_empty(), "an unrepresentable centre culls the halo");

    let mut over = Vec::new();
    push_halo_over(&mut over, g, 322.0, 48.0, 5.0, 5.0, 0x0033_3333, 255);
    assert!(
        over.iter().all(|h| h.cx == 322) && !over.is_empty(),
        "the Over veil follows the same centre law"
    );
    let mut over_neg = Vec::new();
    push_halo_over(&mut over_neg, g, 48.0, -2.0, 5.0, 5.0, 0x0033_3333, 255);
    assert!(over_neg.is_empty(), "…including the cull side");
}

#[test]
fn cold_navigation_timestamp_scatters_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 30)), t0, &c, g, &mut out); // seed: the cursor stays cold
    glow.note_navigation(t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // Ctrl-A leap on a cold cursor
    assert!(
        glow.particles.is_empty(),
        "a cold-cursor jump dissolves no phantom terminus (finding 3)"
    );
    assert!(
        glow.v2_status().is_some_and(|s| s.meteors >= 1),
        "a cold-cursor JUMP (30 cells) flies its meteor — punctuation, \
         not stray rainbow"
    );
    let mut recall = CursorGlow::default();
    recall.tick(Some((2, 3)), t0, &c, g, &mut out);
    recall.note_navigation(t0);
    recall.tick(Some((2, 0)), t0, &c, g, &mut out); // a 3-cell cold nudge
    assert!(
        recall.v2_status().is_none_or(|s| s.meteors == 0),
        "a cold one-row RECALL throws no landing stars — this is the stray \
         rainbow the floor exists to stop"
    );
}

#[test]
fn a_landing_at_the_grids_edge_fades_before_the_edge() {
    let g = geom();
    let born = Instant::now();
    let (cx, cy) = g.cell_center(2, 0);
    for style in [GlowStyle::Fire, GlowStyle::Phaser, GlowStyle::Lumen] {
        let c = cfg(style, true);
        let glow = CursorGlow {
            ring: Some(Ring {
                cx,
                cy,
                born,
                life: 0.30,
                scale: 1.0,
                caret: None,
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(90),
            &c,
            g,
            None,
            &mut out,
            &mut halos,
        );
        assert!(!halos.is_empty(), "{style:?}: the landing blooms");
        for h in &halos {
            let (hx, hy, rx, ry) = (
                i32::from(h.cx),
                i32::from(h.cy),
                i32::from(h.rx),
                i32::from(h.ry),
            );
            assert!(
                hx - rx >= g.fx_left() - 1
                    && hx + rx <= g.fx_right() + 1
                    && hy - ry >= g.fx_top() - 1
                    && hy + ry <= g.fx_bot() + 1,
                "{style:?}: the bloom {h:?} reaches past the effects box"
            );
        }
        let edge_and_peak = |halos: &[RainHalo]| {
            let mut glass = Glass::new(g, 0);
            glass.add_halos(halos);
            let edge = (0..i32::from(g.win_h))
                .map(|y| glass.level(y, 0))
                .max()
                .unwrap_or(0);
            let peak = (0..i32::from(g.win_h))
                .flat_map(|y| (0..i32::from(g.win_w)).map(move |x| (y, x)))
                .map(|(y, x)| glass.level(y, x))
                .max()
                .unwrap_or(0);
            (edge, peak)
        };
        let (edge, peak) = edge_and_peak(&halos);
        assert!(peak > 0, "{style:?}: the bloom is lit");
        assert!(
            edge * 5 <= peak,
            "{style:?}: the grid's edge column stands at {edge} of {peak}"
        );
        let h = halos[0];
        let grow = (0.55 + 1.25 * 0.3) * g.ch as f32;
        let mut cut = Vec::new();
        let (rx, ry) = if style == GlowStyle::Fire {
            (grow * 1.5, grow * 1.1)
        } else {
            let grow = (0.7 + 0.3) * g.ch as f32;
            (grow, grow * 0.85)
        };
        push_halo(&mut cut, g, cx, cy, rx, ry, h.color);
        let (edge, peak) = edge_and_peak(&cut);
        assert!(
            edge * 2 >= peak,
            "control: {style:?} unfitted lights the edge at only {edge} of {peak}"
        );
    }
}

/// Every pixel a dark Fire landing's bloom lights, through its life, is an orange-red (its added
/// light within 30 degrees of hue, saturated), never the khaki a thin amber lays over a dark
/// page. Negative control: the white-hot flash cooling to amber lights khaki pixels.
#[test]
fn a_dark_fire_landing_flash_burns_as_an_ember_to_its_rim() {
    let g = tall_geom();
    let caret = (3u16, 12u16);
    let (cx, cy) = g.cell_center(caret.0, caret.1);
    let c = cfg(GlowStyle::Fire, true);
    // The light a pixel adds over the page, read when its largest channel is at least 10 (past
    // the halo's rounding): warm when it is an orange-red.
    let warm = |p: u32, bg: u32| {
        let ch = |sh: u32| ((p >> sh) & 0xff).saturating_sub((bg >> sh) & 0xff);
        let most = ch(16).max(ch(8)).max(ch(0));
        let (hue, sat, _) = crate::color_math::rgb2hsv((ch(16) << 16) | (ch(8) << 8) | ch(0));
        (most >= 10).then_some(hue <= 30.0 && sat >= 0.6)
    };
    let (mut lit, mut cold, mut control) = (0usize, Vec::new(), 0usize);
    for bg in [0x0011_1318u32, 0x001A_1B26] {
        let born = Instant::now();
        let glow = CursorGlow {
            ring: Some(Ring {
                cx,
                cy,
                born,
                life: 0.30,
                scale: 1.6,
                caret: Some(caret),
            }),
            ..Default::default()
        };
        for ms in [10u64, 50, 120, 200, 270] {
            let (mut out, mut halos) = (Vec::new(), Vec::new());
            glow.emit_ring(
                born + Duration::from_millis(ms),
                &c,
                g,
                Some(caret),
                &mut out,
                &mut halos,
            );
            let t = ms as f32 / 300.0;
            let flash = CursorGlow::fire_dark_flash(t);
            let hot = fire_ramp(0.95 - 0.55 * t);
            let was: Vec<RainHalo> = halos
                .iter()
                .map(|h| {
                    let a = ((h.color >> 16) & 0xff) * 255 / ((flash >> 16) & 0xff).max(1);
                    RainHalo {
                        color: aterm_render::premul_rgb(hot, a.min(255) as u8),
                        ..*h
                    }
                })
                .collect();
            for (halos, is_control) in [(&halos, false), (&was, true)] {
                let mut glass = Glass::new(g, bg);
                glass.add_halos(halos);
                glass.add_quads(&out);
                for y in 0..i32::from(g.win_h) {
                    for x in 0..i32::from(g.win_w) {
                        let Some(ok) = warm(glass.at(y, x) & 0x00FF_FFFF, bg) else {
                            continue;
                        };
                        if is_control {
                            control += usize::from(!ok);
                            continue;
                        }
                        lit += 1;
                        if !ok {
                            cold.push((bg, ms, x, y, glass.at(y, x) & 0x00FF_FFFF));
                        }
                    }
                }
            }
        }
    }
    assert!(lit > 200, "fixture: the flash lights {lit} px");
    assert!(
        cold.is_empty(),
        "{} of {lit} lit px are no ember's colour, first (bg, ms, x, y, px) {:x?}",
        cold.len(),
        cold.first()
    );
    assert!(
        control > 50,
        "control: the white-hot flash cooling to amber lights only {control} khaki px"
    );
}

#[test]
fn a_light_landing_veil_falls_off_from_its_centre() {
    let g = tall_geom();
    let born = Instant::now();
    let caret = (3u16, 12u16);
    let (cx, cy) = g.cell_center(caret.0, caret.1);
    let mut c = cfg(GlowStyle::Fire, true);
    c.dark_theme = false;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    let glow = CursorGlow {
        ring: Some(Ring {
            cx,
            cy,
            born,
            life: 0.30,
            scale: 1.0,
            caret: Some(caret),
        }),
        ..Default::default()
    };
    let (mut out, mut halos) = (Vec::new(), Vec::new());
    glow.emit_ring(
        born + Duration::from_millis(30),
        &c,
        g,
        None,
        &mut out,
        &mut halos,
    );
    assert!(!halos.is_empty(), "the light landing lays its veil");
    let profile = |halos: &[RainHalo]| -> Vec<f32> {
        let mut glass = Glass::new(g, 0x00FF_FFFF);
        glass.add_halos(halos);
        (cx as i32..i32::from(g.win_w))
            .map(|x| {
                1.0 - crate::color_math::relative_luminance(glass.at(cy as i32, x) & 0x00FF_FFFF)
            })
            .collect()
    };
    let shipped = profile(&halos);
    let reach = shipped.iter().rposition(|&d| d > 0.002).unwrap_or(0);
    assert!(
        reach > 6,
        "the veil reaches out from the landing ({reach} px)"
    );
    for w in shipped[..reach].windows(2) {
        assert!(w[1] <= w[0] + 1e-4, "the veil deepens outward: {shipped:?}");
    }
    let half = shipped[reach * 9 / 20];
    assert!(
        half <= 0.75 * shipped[0],
        "a flat disc: {half:.3} at half its reach against {:.3} at its centre",
        shipped[0]
    );
    let cap = halos[0].color >> 24;
    let one = halos.iter().map(|h| h.rx).max().unwrap_or(1);
    let mut flat = Vec::new();
    push_halo_rows(
        &mut flat,
        g,
        (cx, cy),
        (f32::from(one), f32::from(one) * 0.85),
        (halos[0].color & 0x00FF_FFFF) | ((cap * SOFT_VEIL_LAYERS as u32).min(120) << 24),
        HaloMode::Over,
    );
    let old = profile(&flat);
    assert!(
        old[reach * 9 / 20] > 0.95 * old[0],
        "control: one capped veil is flat at half its reach ({:.3} of {:.3})",
        old[reach * 9 / 20],
        old[0]
    );
}

#[test]
fn a_light_landing_veil_wears_its_style_hue() {
    let g = geom();
    let born = Instant::now();
    let (cx, cy) = (100.0f32, 40.0f32);
    for style in [GlowStyle::Phaser, GlowStyle::Sparkle, GlowStyle::Beam] {
        let mut c = cfg(style, true);
        c.dark_theme = false;
        (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
        let glow = CursorGlow {
            ring: Some(Ring {
                cx,
                cy,
                born,
                life: 0.30,
                scale: 1.0,
                caret: Some((2, 12)),
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(30),
            &c,
            g,
            None,
            &mut out,
            &mut halos,
        );
        assert!(!halos.is_empty(), "{style:?}: the landing lays its veil");
        let deepest = |halos: &[RainHalo]| {
            let mut glass = Glass::new(g, 0x00FF_FFFF);
            glass.add_halos(halos);
            glass.at(cy as i32, cx as i32) & 0x00FF_FFFF
        };
        let veil = deepest(&halos);
        let ink = halos[0].color & 0x00FF_FFFF;
        let (want, _, _) = crate::color_math::rgb2hsv(ink);
        let (hue, sat, _) = crate::color_math::rgb2hsv(veil);
        let off = ((hue - want + 540.0) % 360.0 - 180.0).abs();
        assert!(
            off <= 20.0 && sat >= 0.18,
            "{style:?}: the veil reads {veil:06x} ({hue:.0}°, {sat:.2}), not its hue {want:.0}°"
        );
        assert!(
            crate::color_math::contrast_ratio(c.theme_fg, veil) >= 5.25,
            "{style:?}: a letter under the veil reads {:.2}:1",
            crate::color_math::contrast_ratio(c.theme_fg, veil)
        );
        let greyed: Vec<RainHalo> = halos
            .iter()
            .map(|h| RainHalo {
                color: (h.color & 0xFF00_0000) | InkRole::OverText.ink(ink),
                ..*h
            })
            .collect();
        let (_, gsat, _) = crate::color_math::rgb2hsv(deepest(&greyed));
        assert!(
            gsat < sat,
            "control: the over-text ink's veil is as coloured ({gsat:.2} against {sat:.2})"
        );
    }
}

#[test]
fn a_paper_landing_veil_stays_in_the_caret_row_at_the_text_bar() {
    let g = tall_geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let caret = (6u16, 2u16);
    let (ccx, _) = g.cell_center(caret.0, caret.1);
    let (lx, ly) = g.cell_center(caret.0 - 1, caret.1);
    let reach = (CursorGlow::LIGHT_VEIL_REACH_CELLS * cw as f32) as i32 + 1;
    let (mut escapes, mut drawn) = (0usize, 0usize);
    for (fg, bg) in [(0x001F_2328, 0x00FF_FFFF), (0x0033_3333, 0x00FA_F8F5)] {
        for style in [
            GlowStyle::Sparkle,
            GlowStyle::Comet,
            GlowStyle::Lumen,
            GlowStyle::Phaser,
            GlowStyle::Laser,
            GlowStyle::Beam,
            GlowStyle::Fire,
        ] {
            let mut c = cfg(style, true);
            c.dark_theme = false;
            (c.theme_fg, c.theme_bg) = (fg, bg);
            for unbound in [false, true] {
                let born = Instant::now();
                let glow = CursorGlow {
                    ring: Some(Ring {
                        cx: lx,
                        cy: ly,
                        born,
                        life: 0.40,
                        scale: 1.6,
                        caret: Some(caret),
                    }),
                    veil_unbound: unbound,
                    ..Default::default()
                };
                for (ms, cur) in [(30u64, Some(caret)), (120, Some(caret)), (60, None)] {
                    let (mut out, mut halos) = (Vec::new(), Vec::new());
                    glow.emit_ring(
                        born + Duration::from_millis(ms),
                        &c,
                        g,
                        cur,
                        &mut out,
                        &mut halos,
                    );
                    // The rim is round about the caret's cell, past its row as a ring
                    // is; the veil under it stays in the row.
                    if style.reads_knob(TrailKnob::Ring, KnobContext::widest(false)) {
                        assert!(
                            on_the_rim(
                                &out,
                                g.cell_center(caret.0, caret.1),
                                rim_at(g, ms, 0.40, 1.6)
                            ),
                            "{style:?} on {bg:06x}, {ms} ms: the rim is round about the caret: {:?}",
                            rim_reach(&out, g.cell_center(caret.0, caret.1))
                        );
                    } else {
                        assert!(out.is_empty(), "{style:?}: no rim without a Ring knob");
                    }
                    let mut glass = Glass::new(g, bg);
                    glass.add_halos(&halos);
                    for y in 0..i32::from(g.win_h) {
                        for x in 0..i32::from(g.win_w) {
                            let p = glass.at(y, x);
                            if p == bg {
                                continue;
                            }
                            let inside =
                                y / ch == i32::from(caret.0) && (x - ccx as i32).abs() <= reach;
                            if unbound {
                                escapes += usize::from(!inside);
                                continue;
                            }
                            drawn += 1;
                            assert!(
                                inside,
                                "{style:?} on {bg:06x}, {ms} ms, caret {cur:?}: the veil at \
                                 ({x}, {y}) leaves the caret's cell and row"
                            );
                            let k = crate::color_math::contrast_ratio(fg, p & 0x00FF_FFFF);
                            assert!(
                                k >= 4.5,
                                "{style:?} on {bg:06x}, {ms} ms: the text over the veil at \
                                 ({x}, {y}) reads {k:.2}:1"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(drawn > 0, "the paper landing lays no veil");
    assert!(
        escapes > 0,
        "control: the unbound veil stays in the caret's row"
    );
}

#[test]
fn a_paper_landing_veil_keeps_the_caret_row_through_a_hidden_caret() {
    let g = tall_geom();
    let ch = g.ch as i32;
    let bottom = g.rows as u16 - 1;
    let mut lit_hidden = 0usize;
    for style in [GlowStyle::Comet, GlowStyle::Sparkle, GlowStyle::Phaser] {
        let mut c = cfg(style, true);
        c.dark_theme = false;
        (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        glow.tick(Some((bottom, 24)), t0, &c, g, &mut out);
        let t1 = t0 + Duration::from_millis(16);
        glow.note_synthetic_move(t1);
        glow.tick(Some((bottom, 2)), t1, &c, g, &mut out);
        assert!(
            glow.ring.is_some(),
            "{style:?}: fixture: the jump lays a landing"
        );
        glow.note_scroll(1);
        for (ms, cur) in [
            (32u64, Some((bottom, 2u16))),
            (48, None),
            (64, Some((bottom, 2))),
        ] {
            glow.tick(cur, t1 + Duration::from_millis(ms), &c, g, &mut out);
            let mut glass = Glass::new(g, c.theme_bg);
            glass.add_halos(glow.halos());
            for y in 0..i32::from(g.win_h) {
                for x in 0..i32::from(g.win_w) {
                    if glass.at(y, x) & 0x00FF_FFFF == c.theme_bg {
                        continue;
                    }
                    lit_hidden += usize::from(cur.is_none());
                    assert_eq!(
                        (y - i32::from(g.origin_y)) / ch,
                        i32::from(bottom),
                        "{style:?} +{ms} ms, caret {cur:?}: a veil pixel at ({x}, {y}) off \
                         the caret's row"
                    );
                }
            }
        }
    }
    assert!(lit_hidden > 0, "the hidden-caret frame lays no veil at all");
}

/// On two dark themes, each veiled style's landing light lies over no row above the caret's
/// through a jump to the bottom row, two scrolls and a frame with no caret. A ring landed two rows
/// up, where the scrolls left it, stays in the caret's row within
/// [`CursorGlow::LIGHT_VEIL_REACH_CELLS`] of its centre (graded by [`classic_ring_radius_factor`]),
/// keeps the theme's text 4.5:1 and wears its style's hue (its lift saturated 0.6 or more).
/// Negative control: the veil unbound ([`CursorGlow::veil_unbound`]) lies over the rows above,
/// leaves the caret's row, and is grey.
#[test]
fn a_dark_landing_veil_stays_in_the_caret_row_in_its_hue() {
    let g = tall_geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let bottom = g.rows as u16 - 1;
    let caret = (bottom, 2u16);
    let (ccx, _) = g.cell_center(caret.0, caret.1);
    let (lx, ly) = g.cell_center(caret.0 - 2, caret.1);
    let reach = (CursorGlow::LIGHT_VEIL_REACH_CELLS * cw as f32 * classic_ring_radius_factor(1.6))
        as i32
        + 1;
    // A pixel's light over the page: its saturation, and its largest channel.
    let lift = |p: u32, bg: u32| {
        let ch = |sh: u32| ((p >> sh) & 0xff).saturating_sub((bg >> sh) & 0xff);
        let rgb = (ch(16) << 16) | (ch(8) << 8) | ch(0);
        (
            crate::color_math::rgb2hsv(rgb).1,
            ch(16).max(ch(8)).max(ch(0)),
        )
    };
    let (mut drawn, mut above, mut escapes, mut grey) = (0usize, 0usize, 0usize, 0usize);
    for (fg, bg) in [(0x00D0_D0D0, 0x0011_1318), (0x00C8_D3F5, 0x001A_1B26)] {
        for style in [
            GlowStyle::Sparkle,
            GlowStyle::Comet,
            GlowStyle::Lumen,
            GlowStyle::Phaser,
            GlowStyle::Laser,
        ] {
            let mut c = cfg(style, true);
            (c.theme_fg, c.theme_bg) = (fg, bg);
            c.color = crate::cursor_fx::canonical_glow_color(style).unwrap_or(0x00C8_F0D0);
            c.accent = crate::cursor_fx::brighten(c.color, 1.5);
            for unbound in [false, true] {
                let mut glow = CursorGlow {
                    veil_unbound: unbound,
                    ..Default::default()
                };
                let mut out = Vec::new();
                let t0 = Instant::now();
                glow.tick(Some((bottom, 24)), t0, &c, g, &mut out);
                let t1 = t0 + Duration::from_millis(16);
                glow.note_synthetic_move(t1);
                glow.tick(Some(caret), t1, &c, g, &mut out);
                assert!(
                    glow.ring.is_some(),
                    "{style:?}: fixture: the jump lays a landing"
                );
                glow.note_scroll(1);
                glow.note_scroll(1);
                for (ms, cur) in [(32u64, Some(caret)), (48, None), (120, Some(caret))] {
                    let now = t1 + Duration::from_millis(ms);
                    glow.tick(cur, now, &c, g, &mut out);
                    // The landing's own light alone (a sparkle's stars fly free).
                    let (mut ring_out, mut halos) = (Vec::new(), Vec::new());
                    glow.emit_ring(now, &c, g, cur, &mut ring_out, &mut halos);
                    let mut glass = Glass::new(g, bg);
                    glass.add_halos(&halos);
                    for y in 0..i32::from(bottom) * ch {
                        for x in 0..i32::from(g.win_w) {
                            if glass.at(y, x) & 0x00FF_FFFF == bg {
                                continue;
                            }
                            assert!(
                                unbound,
                                "{style:?} on {bg:06x} +{ms} ms, caret {cur:?}: the landing \
                                 lights ({x}, {y}), over the rows above the caret's"
                            );
                            above += 1;
                        }
                    }
                }
                let born = Instant::now();
                let ring = CursorGlow {
                    ring: Some(Ring {
                        cx: lx,
                        cy: ly,
                        born,
                        life: 0.40,
                        scale: 1.6,
                        caret: Some(caret),
                    }),
                    veil_unbound: unbound,
                    ..Default::default()
                };
                for ms in [30u64, 120] {
                    let (mut out, mut halos) = (Vec::new(), Vec::new());
                    ring.emit_ring(
                        born + Duration::from_millis(ms),
                        &c,
                        g,
                        Some(caret),
                        &mut out,
                        &mut halos,
                    );
                    // The rim is round about the caret's cell, past its row as a ring
                    // is; the veil under it stays in the row.
                    assert!(
                        on_the_rim(
                            &out,
                            g.cell_center(caret.0, caret.1),
                            rim_at(g, ms, 0.40, 1.6)
                        ),
                        "{style:?} on {bg:06x}, {ms} ms: the rim is round about the caret: {:?}",
                        rim_reach(&out, g.cell_center(caret.0, caret.1))
                    );
                    let mut glass = Glass::new(g, bg);
                    glass.add_halos(&halos);
                    for y in 0..i32::from(g.win_h) {
                        for x in 0..i32::from(g.win_w) {
                            let p = glass.at(y, x) & 0x00FF_FFFF;
                            if p == bg {
                                continue;
                            }
                            let inside =
                                y / ch == i32::from(caret.0) && (x - ccx as i32).abs() <= reach;
                            let (sat, most) = lift(p, bg);
                            if unbound {
                                escapes += usize::from(!inside);
                                grey += usize::from(most >= 8 && sat < 0.4);
                                continue;
                            }
                            drawn += 1;
                            assert!(
                                inside,
                                "{style:?} on {bg:06x}, {ms} ms: the veil at ({x}, {y}) \
                                 leaves the caret's cell and row"
                            );
                            let k = crate::color_math::contrast_ratio(fg, p);
                            assert!(
                                k >= 4.5,
                                "{style:?} on {bg:06x}, {ms} ms: the text over the veil at \
                                 ({x}, {y}) reads {k:.2}:1"
                            );
                            assert!(
                                most < 8 || sat >= 0.6,
                                "{style:?} on {bg:06x}, {ms} ms: the veil at ({x}, {y}) is \
                                 {p:06x}, its light saturated only {sat:.2}"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(drawn > 0, "the dark landing lays no veil");
    assert!(
        above > 0,
        "control: the unbound veil lies over no row above the caret's"
    );
    assert!(
        escapes > 0,
        "control: the unbound veil stays in the caret's row"
    );
    assert!(grey > 0, "control: the unbound veil's light is never grey");
}

#[test]
fn the_dark_fire_landing_is_a_brief_flash() {
    let g = geom();
    let born = Instant::now();
    let light = |style: GlowStyle, ms: u64| -> (u64, i32, usize) {
        let c = cfg(style, true);
        let glow = CursorGlow {
            ring: Some(Ring {
                cx: 100.0,
                cy: 40.0,
                born,
                life: 0.30,
                scale: 1.0,
                caret: None,
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(ms),
            &c,
            g,
            None,
            &mut out,
            &mut halos,
        );
        let mut glass = Glass::new(g, 0);
        glass.add_quads(&out);
        glass.add_halos(&halos);
        let (mut sum, mut reach) = (0u64, 0i32);
        for y in 0..i32::from(g.win_h) {
            for x in 0..i32::from(g.win_w) {
                let v = glass.level(y, x);
                if v > 0 {
                    sum = sum.max(v as u64);
                    reach = reach.max((x - 100).abs());
                }
            }
        }
        let blooms = halos
            .iter()
            .map(|h| (h.cx, h.cy, h.rx, h.ry))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        (sum, reach, blooms)
    };
    let (birth, _, _) = light(GlowStyle::Fire, 5);
    assert!(birth > 0, "the landing flashes");
    let most = (CursorGlow::FIRE_DARK_LANDING_REACH * 1.3 * g.ch as f32).ceil() as i32 + 1;
    for ms in [5u64, 60, 150, 240] {
        let (_, reach, blooms) = light(GlowStyle::Fire, ms);
        assert!(
            reach <= most,
            "at {ms} ms the flash reaches {reach} px (most {most})"
        );
        assert!(blooms <= 1, "at {ms} ms the landing lays {blooms} blooms");
    }
    let (late, _, _) = light(GlowStyle::Fire, 180);
    assert!(
        late * 4 < birth,
        "past half its life the flash keeps {late} of its birth's {birth}"
    );
    let (other_birth, _, _) = light(GlowStyle::Phaser, 5);
    let (other_late, _, _) = light(GlowStyle::Phaser, 180);
    assert!(
        other_late * 4 >= other_birth,
        "control: the linear bloom keeps {other_late} of {other_birth} on the same probe"
    );
}

#[test]
fn a_ring_on_the_last_row_melts_into_the_box_edge() {
    let g = tall_geom();
    let ring = |row: u16| {
        let mut out = Vec::new();
        let cy = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        push_ripple_ring(&mut out, g, (200.0, cy), (40.0, 22.0), 4.0, 0x00FF_FFFF);
        (cy as i32, out)
    };
    let lit_at = |quads: &[GlowQuad], y: i32| -> u64 {
        quads
            .iter()
            .filter(|q| i32::from(q.y) <= y && y < i32::from(q.y) + i32::from(q.h))
            .map(|q| {
                u64::from(((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff))
                    * u64::from(q.w)
            })
            .sum()
    };
    let (cy_b, bottom) = ring(g.rows as u16 - 1);
    let (cy_m, mid) = ring(3);
    let last = g.fx_bot() - 1;
    let edge = lit_at(&bottom, last);
    let twin = lit_at(&mid, cy_m + (last - cy_b));
    assert!(
        bottom
            .iter()
            .all(|q| i32::from(q.y) + i32::from(q.h) <= g.fx_bot()),
        "a ring lays light past the box's bottom"
    );
    assert!(
        edge * 10 <= twin,
        "the last row's ring lays {edge} along the box's edge, against {twin} mid-grid"
    );
    assert!(
        twin > 0,
        "control: mid-grid the ring lays nothing at that offset"
    );
}

/// A colour the user chose ([`ColourPins`]) lands as itself on dark: the veil keeps its hue and
/// saturation (a grey stays grey), held legible by its coverage alone. Negative control: the same
/// colours unchosen are re-hued or saturated.
#[test]
fn a_chosen_colour_lands_as_itself_on_dark() {
    let g = geom();
    let born = Instant::now();
    let pinned_trail = ColourPins {
        trail: true,
        accent: false,
    };
    let pinned_accent = ColourPins {
        trail: false,
        accent: true,
    };
    let cases = [
        (
            GlowStyle::Laser,
            0x0090_70B0u32,
            0x0090_70B0u32,
            pinned_trail,
        ),
        (GlowStyle::Comet, 0x0050_FA7B, 0x00FF_FFFF, pinned_accent),
        (GlowStyle::Phaser, 0x0050_FA7B, 0x0060_6060, pinned_accent),
    ];
    // The veil's colour: the strongest halo's light unpremultiplied.
    let veil = |style: GlowStyle, color: u32, accent: u32, pins: ColourPins| -> u32 {
        let mut c = cfg(style, true);
        (c.color, c.accent) = (color, accent);
        let glow = CursorGlow {
            ring: Some(Ring {
                cx: 100.0,
                cy: 40.0,
                born,
                life: 0.30,
                scale: 1.0,
                caret: Some((2, 12)),
            }),
            colour_pins: pins,
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(60),
            &c,
            g,
            Some((2, 12)),
            &mut out,
            &mut halos,
        );
        let h = halos
            .iter()
            .max_by_key(|h| h.color & 0x00FF_FFFF)
            .expect("the landing lays a veil");
        h.color & 0x00FF_FFFF
    };
    let hsv = |c: u32| crate::color_math::rgb2hsv(c);
    for (style, color, accent, pins) in cases {
        let worn = if style == GlowStyle::Laser {
            color
        } else {
            accent
        };
        let (h0, s0, _) = hsv(worn);
        let (h, s, _) = hsv(veil(style, color, accent, pins));
        let off = (h - h0).abs().min(360.0 - (h - h0).abs());
        assert!(
            (s - s0).abs() <= 0.06 && (s0 < 0.05 || off <= 4.0),
            "{style:?}: the chosen #{worn:06x} ({h0:.0}°, {s0:.2}) lands as {h:.0}°, {s:.2}"
        );
        let (ch, cs, _) = hsv(veil(style, color, accent, ColourPins::default()));
        let c_off = (ch - h0).abs().min(360.0 - (ch - h0).abs());
        assert!(
            (cs - s0).abs() > 0.1 || c_off > 10.0,
            "control, {style:?}: unchosen, #{worn:06x} still lands as itself ({ch:.0}°, {cs:.2})"
        );
    }
}
