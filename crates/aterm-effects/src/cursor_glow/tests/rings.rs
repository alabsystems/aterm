// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Landing rings and halo culling.

use super::*;

/// Landing ring, light-theme fork: on dark the ring is an additive square
/// shockwave (GlowQuads); on light it is an expanding darkened source-over
/// veil ring in the halo stream, capped for legibility, and the additive
/// stream stays empty.
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
    };

    // Dark: an additive round bloom, never a source-over veil.
    let c_dark = cfg(GlowStyle::RainbowKitty, true);
    let dark = CursorGlow {
        ring: Some(ring),
        ..Default::default()
    };
    let (mut d_out, mut d_h) = (Vec::new(), Vec::new());
    dark.emit_ring(at, &c_dark, g, &mut d_out, &mut d_h);
    assert!(
        !d_h.is_empty() && d_out.is_empty(),
        "dark draws the additive round shockwave as halos"
    );
    assert!(
        d_h.iter().all(|h| h.mode != HaloMode::Over),
        "the dark ring is additive, never source-over"
    );

    // Light: source-over veil ring, no additive quads.
    let mut c_light = cfg(GlowStyle::RainbowKitty, true);
    c_light.dark_theme = false;
    let light = CursorGlow {
        ring: Some(ring),
        ..Default::default()
    };
    let (mut l_out, mut l_h) = (Vec::new(), Vec::new());
    light.emit_ring(at, &c_light, g, &mut l_out, &mut l_h);
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

/// Every style lands a ring on a light ground, and none of it is additive:
/// Water lands its two-ripple train in conservative ink over the text
/// (source-over quads under the over-text ceiling), and every other style,
/// Fire included, the shared source-over veil ring, capped for legibility.
#[test]
fn every_style_lands_a_veil_ring_on_light() {
    let g = geom();
    let born = Instant::now();
    let ring = Ring {
        cx: 100.0,
        cy: 40.0,
        born,
        life: 0.30,
        scale: 1.0,
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
        assert!(out.is_empty(), "{style:?}: no additive ring quads on light");
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

/// The same ring at the same age: a full-line jump's outline reaches
/// further from the landing cell than a hop's, on every square-outline
/// theme arm and for the fire flash — the grade is not a no-op.
#[test]
fn classic_landing_ring_expands_wider_with_the_jump() {
    let g = geom();
    let born = Instant::now();
    let extent = |style: GlowStyle, dark: bool, scale: f32| -> i32 {
        let c = cfg(style, dark);
        let glow = CursorGlow {
            ring: Some(Ring {
                cx: 100.0,
                cy: 40.0,
                born,
                life: 0.30,
                scale,
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(120),
            &c,
            g,
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
    for (style, dark) in [
        (GlowStyle::Phaser, true),
        (GlowStyle::Phaser, false),
        (GlowStyle::Fire, true),
    ] {
        let hop = extent(style, dark, 1.0);
        let leap = extent(style, dark, 3.5);
        assert!(hop > 0, "{style:?} dark={dark}: the hop ring draws");
        assert!(
            leap as f32 >= hop as f32 * 1.8,
            "{style:?} dark={dark}: a capped-impact ring reaches {leap} px, a hop {hop} px"
        );
    }
}

/// The dark landing ring is one soft round bloom, not a thin square outline
/// (which read as a stray rectangle left on the page, a side of it cut off by
/// the pad): through its life its light is radial halos and no quad, along
/// the row through its middle it is brightest at the middle and falls off a
/// little at a time to nothing, and it is round (as wide as tall, within the
/// halo's own squash). Negative control: the old outline's four 2 px bars at
/// the same size, laid on the same probe, show the hard edge it looks for.
#[test]
fn the_dark_landing_ring_is_a_soft_round_bloom() {
    let g = geom();
    let born = Instant::now();
    let c = cfg(GlowStyle::Phaser, true);
    // The light along the row through the ring's middle, and down the column.
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
            &mut out,
            &mut halos,
        );
        assert!(out.is_empty(), "no quad in the ring at {ms}ms: {out:?}");
        assert!(!halos.is_empty(), "the ring blooms at {ms}ms");
        let (across, down) = lines(&out, &halos, 100, 40);
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
        assert!(
            (h as f32) >= 0.7 * w as f32 && (h as f32) <= 1.05 * w as f32,
            "round at {ms}ms: {w} px across, {h} down"
        );
        // Control: the old square outline at this size.
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

/// A halo whose centre lies past the effects box keeps its true centre in the
/// emitted bands (the falloff reads ~0 on the surviving sliver), and a centre
/// that cannot be represented (negative) culls the halo. Clamping it onto the
/// edge would flash a particle sliding off the grid.
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

/// A cold navigation timestamp is not a jump witness: with no candidate and no
/// ribbon it births no terminus scatter; a real jump still flies its meteor
/// and a short cold nudge throws nothing.
#[test]
fn cold_navigation_timestamp_scatters_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 30)), t0, &c, g, &mut out); // seed: the cursor stays cold
    // (rainbow.disp left at its default 0.0 — no typing momentum, no ribbon.)
    glow.note_navigation(t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // Ctrl-A leap on a cold cursor
    assert!(
        glow.particles.is_empty(),
        "a cold-cursor jump dissolves no phantom terminus (finding 3)"
    );
    // The starburst forks on distance: a cold Home/End past
    // `timing::JUMP_MIN_CELLS` (this 30-cell Ctrl-A) celebrates, while a short
    // cold move such as a history recall throws nothing (pinned below).
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

/// A LANDING AT THE GRID'S EDGE FADES BEFORE THE EDGE: a jump that lands on
/// the left column (an Enter's new line, or a jump to a prompt) blooms with
/// its whole radial falloff inside the effects box on a dark ground, Fire's
/// flash and every other style's bloom, so the grid's edge column is all but
/// dark instead of standing cut through the bloom's middle. Negative
/// control: the same bloom laid unfitted at its landing lights the edge
/// column at most of its peak.
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
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(90),
            &c,
            g,
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
        // Control: the first bloom unfitted, at its landing and full size.
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

/// ON PAPER A LANDING VEIL FALLS OFF FROM ITS CENTRE: the veil ring a light
/// ground lays darkens the page most at the landing and less and less
/// outward (no flat disc of ink, which reads as a stain), never deeper than
/// its cap allows. Negative control: one veil held under the same cap is
/// flat out to half its radius.
#[test]
fn a_light_landing_veil_falls_off_from_its_centre() {
    let g = geom();
    let born = Instant::now();
    let (cx, cy) = (100.0f32, 40.0f32);
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
        }),
        ..Default::default()
    };
    let (mut out, mut halos) = (Vec::new(), Vec::new());
    glow.emit_ring(
        born + Duration::from_millis(30),
        &c,
        g,
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
    // Control: one veil under the same cap, at the same reach.
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

/// ON PAPER A LANDING VEIL WEARS ITS STYLE'S HUE: the light landing veil of
/// the phaser and of sparkle reads, where it is deepest, in the hue of the
/// style's own colour and clearly coloured (a veil of the darkened
/// over-text ink thinned to a grey halo round the caret), while the letters
/// under it keep the light bar. Negative control: the same veil in the
/// over-text ink reads grey.
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
            }),
            ..Default::default()
        };
        let (mut out, mut halos) = (Vec::new(), Vec::new());
        glow.emit_ring(
            born + Duration::from_millis(30),
            &c,
            g,
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
