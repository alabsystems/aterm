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
        caret: Some((2, 12)),
    };

    // Dark: an additive round bloom, never a source-over veil.
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

    // Light: source-over veil ring, no additive quads.
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
/// its cap allows. Judged on the veil as it ships, held to the caret's cell
/// and row, on a cell the gallery's size. Negative control: one veil held
/// under the same cap is flat out to half its radius.
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

/// THE PAPER LANDING VEIL STAYS IN THE CARET'S CELL AND ROW, AT THE TEXT
/// BAR (2026-10-22: after an Enter at the bottom row the veil, carried up
/// with the scrolled text, stood as a cloud five to eight cells wide and two
/// and a half rows tall over the lines just printed above the caret, on
/// GitHub Light and Default Light alike). A landing a row above the caret
/// (where the scroll left it), on two paper themes and every veiled style,
/// at the Enter's frame and at 120 ms: every pixel the veil touches lies in
/// the caret's row, within [`CursorGlow::LIGHT_VEIL_REACH_CELLS`] and a pixel
/// of the caret's centre, and the theme's text over it keeps 4.5:1. So on a
/// frame whose caret the host withholds (`cur: None`, a hidden caret): the
/// veil stays on the caret the ring last saw. NEGATIVE CONTROLS: the veil as
/// it was ([`CursorGlow::veil_unbound`]) leaves the row; and (as run on
/// 2026-10-22, the bound held only while `cur` was there) the hidden-caret
/// frame laid the old cloud over the landing.
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
                    let mut glass = Glass::new(g, bg);
                    glass.add_halos(&halos);
                    glass.add_quads(&out);
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

/// THE PAPER LANDING VEIL HOLDS ITS ROW THROUGH A HIDDEN CARET, through the
/// whole frame step: a jump to the bottom row lays the landing, an Enter's
/// scroll carries it a row up with the text, and one frame of the ring's
/// life the host withholds the caret (`cur: None`, as it does for a hidden
/// caret). On every frame, that one too, every pixel the halos lay lies in
/// the caret's row. Negative control (as run on 2026-10-22, the bound held
/// only while `cur` was there): the hidden frame laid the cloud two rows up
/// over the scrolled text (Comet's first pixel off the row at (30, 142)).
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

/// THE DARK FIRE'S LANDING IS A BRIEF FLASH, NOT A DISC LEFT BEHIND (round 1:
/// the Enter's landing bloom, spread to three cells with an aftershock
/// behind it, stood as a flat orange disc where the caret had landed, still
/// there after a clear took the line away). Through its life the flash
/// reaches no further than [`CursorGlow::FIRE_DARK_LANDING_REACH`] of a row
/// (graded by the jump) and is one bloom, and past half its life its peak
/// is under a quarter of its birth's. Negative control: the other dark
/// styles' bloom on the same probe, fading only linearly, still holds more
/// than that there.
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

/// A RING ON THE GRID'S LAST ROW MELTS INTO THE BOX'S EDGE (round-2 judges,
/// item 8: water's Return rings round a caret on the bottom row stood cut
/// flat along the grid's bottom, dark and light). A ripple ring round a
/// caret on the last row lays in the effects box's last pixel row at most a
/// tenth of what the same ring round a caret mid-grid lays at the same
/// offset from its centre, and nothing past the box. Negative control: mid
/// grid that offset is lit, so a straight cut at the box's edge would have
/// stood there whole.
#[test]
fn a_ring_on_the_last_row_melts_into_the_box_edge() {
    let g = tall_geom();
    let ring = |row: u16| {
        let mut out = Vec::new();
        let cy = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        push_ripple_ring(&mut out, g, (200.0, cy), (40.0, 22.0), 4.0, 0x00FF_FFFF);
        (cy as i32, out)
    };
    // The light a ring lays in window pixel row `y` (channels × width).
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
