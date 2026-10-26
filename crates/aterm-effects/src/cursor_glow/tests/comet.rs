// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comet style.

use super::*;

/// `comet`, `beam` and `lumen` each parse to their own variant.
#[test]
fn comet_parses_apart_from_beam_and_lumen() {
    assert_eq!(GlowStyle::parse("comet"), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse(" Comet "), GlowStyle::Comet);
    assert_eq!(GlowStyle::parse("beam"), GlowStyle::Beam);
    assert_eq!(GlowStyle::parse("lumen"), GlowStyle::Lumen);
    assert!(
        GlowStyle::Comet.has_particles(),
        "comet sheds debris glitter"
    );
    assert!(style_has_beam("comet"), "the comet keeps its beam");
}

fn chrome_geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 56,
        origin_y: 56,
        win_w: 384, // 56 + 40·8 + 8
        win_h: 160, // 56 + 6·16 + 8
        head: 0,
    }
}

/// The live-cursor bridge vertex is window-absolute like every other comet sample: with real chrome it attaches at `origin_x + (col + head_dx)·cw`.
#[test]
fn comet_bridge_attaches_window_absolute() {
    let g = chrome_geom();
    let mut c = cfg(GlowStyle::Comet, true);
    c.head_dx = 0.9; // an attach point clearly right of the cell centre
    let mut glow = CursorGlow::default();
    let mut t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 10)), t, &c, g, &mut out);
    for col in 11..=14u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let mut beam = Vec::new();
    let mut halos = Vec::new();
    let mut under = Vec::new();
    glow.emit_comet(
        t,
        &c,
        g,
        Some((2, 14)),
        &mut crate::cursor_glow::glow_sink(
            &mut beam,
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
        &mut halos,
        &mut crate::cursor_glow::glow_sink(
            &mut under,
            &mut aterm_render::GlowRamps::default(),
            true,
        ),
    );
    assert!(!beam.is_empty(), "a typing run draws a beam");
    let reach = beam
        .iter()
        .map(|q| i32::from(q.x) + i32::from(q.w))
        .max()
        .unwrap();
    assert!(
        reach >= 172,
        "bridge reaches the window-absolute head_dx attach point: {reach}"
    );
    assert!(
        beam.iter().all(|q| i32::from(q.x) >= i32::from(g.origin_x)),
        "every beam quad stays inside the window-absolute effects box"
    );
}

/// Comet debris is born window-absolute: jump grains sit on the true leap vector, and typing debris drifts behind the one-cell motion with jitter-only vy (not toward the window's top-left).
#[test]
fn comet_debris_born_window_absolute_with_chrome_origin() {
    let g = chrome_geom();
    let c = cfg(GlowStyle::Comet, true);
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((0, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t + Duration::from_millis(16));
    glow.tick(
        Some((5, 20)),
        t + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert!(!glow.particles.is_empty(), "a leap sheds a meteor train");
    let (fx, fy) = (56.0 + 2.5 * 8.0, 56.0 + 0.5 * 16.0);
    let (ox, oy) = (56.0 + 20.5 * 8.0, 56.0 + 5.5 * 16.0);
    for p in &glow.particles {
        let u = (p.x0 - fx) / (ox - fx);
        assert!(
            (-1e-3..=1.0 + 1e-3).contains(&u) && (p.y0 - (fy + (oy - fy) * u)).abs() < 1e-2,
            "grain ON the true leap vector, got ({}, {})",
            p.x0,
            p.y0
        );
    }
    let mut glow = CursorGlow::default();
    let mut t = Instant::now();
    glow.tick(Some((2, 10)), t, &c, g, &mut out);
    for col in 11..=16u16 {
        t += Duration::from_millis(40);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let cell = g.ch as f32;
    assert!(!glow.particles.is_empty(), "typing outgasses grains");
    for p in &glow.particles {
        assert!(p.vx < 0.0, "dust falls back along the tail: vx {}", p.vx);
        assert!(
            p.vy.abs() <= 0.175 * cell + 1e-3,
            "vy is dispersion jitter only, no origin skew: {}",
            p.vy
        );
    }
}

/// A leap's dust is born window-absolute: every mote lies inside the ±0.4-cell sleeve of the true leap vector.
#[test]
fn leap_dust_strewn_along_window_absolute_leap() {
    let g = chrome_geom();
    let c = cfg(GlowStyle::Comet, true);
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((0, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t + Duration::from_millis(16));
    glow.tick(
        Some((5, 20)),
        t + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    assert!(!glow.particles.is_empty(), "a leap sheds its dust");
    let (fx, fy) = (56.0 + 2.5 * 8.0, 56.0 + 0.5 * 16.0);
    let (ox, oy) = (56.0 + 20.5 * 8.0, 56.0 + 5.5 * 16.0);
    let sleeve = 0.4 * g.ch as f32 + 1e-3;
    for p in &glow.particles {
        let u = (p.x0 - fx) / (ox - fx);
        assert!(
            (-1e-3..=1.0 + 1e-3).contains(&u),
            "mote within the leap span: {}",
            p.x0
        );
        assert!(
            (p.y0 - (fy + (oy - fy) * u)).abs() <= sleeve,
            "mote inside the leap sleeve, got ({}, {})",
            p.x0,
            p.y0
        );
        assert!(
            p.x0 >= 56.0 && p.y0 >= 56.0,
            "no mote above/left of the grid interior: ({}, {})",
            p.x0,
            p.y0
        );
    }
}

/// The comet's emitted light (beam + coma crown + twinkling glitter) honours the shared quad invariants and decays to exactly empty.
#[test]
fn comet_light_respects_invariants_and_decays() {
    let g = geom();
    let c = cfg(GlowStyle::Comet, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    glow.note_synthetic_move(t0);
    glow.tick(Some((2, 30)), t0, &c, g, &mut out);
    out.clear();
    let fp = glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(60),
        &c,
        g,
        &mut out,
    );
    assert_ne!(fp, 0);
    assert!(!out.is_empty(), "beam + glitter render while alive");
    assert_invariants(&out, g);
    let fp = glow.tick(Some((2, 30)), t0 + Duration::from_secs(4), &c, g, &mut out);
    assert!(out.is_empty(), "comet light fully decayed");
    assert_eq!(fp, 0);
    assert!(!glow.is_active());
}

fn comet_newest_letter(gap_ms: u64, unbudgeted: bool) -> f32 {
    let g = tall_geom();
    let page = 0x0011_1318u32;
    let ink = 0x00D0_D0D0u32;
    let (row, start) = (3u16, 2u16);
    let mut glow_cfg = crate::cursor_fx::resolve_glow_config(
        crate::cursor_fx::GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: 1.0,
            radius: 0.6,
            ring: true,
        },
        crate::cursor_fx::GlowSpelling {
            style: Some(GlowStyle::Comet),
            beam: true,
            ribbon_tall: false,
            ribbon_flat: false,
            classic_mono: false,
        },
        0x0050_FA7B,
        true,
        ink,
        page,
        0.5,
    );
    glow_cfg.theme_fg = ink;
    let trail = crate::cursor_trail::TrailConfig {
        enabled: true,
        duration: Duration::from_millis(260),
        max_len: 24,
        color: glow_cfg.color,
        intensity: 0.0,
        warmth: 0.0,
    };
    let mut fx = crate::cursor_fx::CursorFx::default();
    fx.glow.streak_unbudgeted = unbudgeted;
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    let mut worst = f32::INFINITY;
    for k in 0..=24u16 {
        if k > 0 {
            t += Duration::from_millis(gap_ms);
            fx.cadence.on_keystroke(t);
            fx.momentum
                .on_key(t, crate::cursor_momentum::MOMENTUM_GLOW_TAU_S);
            fx.glow.note_typed_cells(t, 1);
            fx.trail.note_synthetic_typed(t);
            text[usize::from(start + k) - 1] = 'm';
        }
        let caret = start + k;
        fx.glow.observe_row(row, caret, &text, t);
        fx.begin(true, true);
        let input = crate::cursor_fx::CursorFxInput {
            now: t,
            cur: Some((row, caret)),
            live_viewport: true,
            cursor_style: aterm_core::terminal::CursorStyle::BlinkingBlock,
            blink_phase: true,
            live_cursor: 0x0050_FA7B,
            cursor_color_pinned: false,
            default_bg: page,
            cursor_cell_bg: None,
            geom: g,
            focused: true,
            trail_brightness: 1.0,
            shed_envelope: 1.0,
            body_allowed: true,
            momentum_glow: true,
            user_tinted: false,
            accent_pinned: false,
        };
        let frame = fx.tick(&input, &glow_cfg, trail);
        if k < 10 {
            continue;
        }
        let mut glass = Glass::new(g, page);
        for c in &fx.trail_scratch {
            let (x0, y0, x1, y1) = glass.cell_rect(c.row as u16, c.col as u16);
            let tint = aterm_render::blend_rgb(page, frame.trail_color, c.alpha);
            for y in y0..y1 {
                for x in x0..x1 {
                    glass.px[y as usize * glass.w + x as usize] = tint;
                }
            }
        }
        glass.add_quads(fx.glow.under_quads());
        for col in start..caret {
            for (y, x) in glyph(g, row, col).0 {
                glass.px[y as usize * glass.w + x as usize] = ink;
            }
        }
        glass.add_quads(&fx.glow_scratch);
        glass.add_halos(fx.glow.halos());
        let (i, cnt) = glyph(g, row, caret - 1);
        worst = worst.min(ratio(&glass, &i, &cnt));
    }
    worst
}

/// The letter beside a comet caret maintains at least 3:1 contrast on the dark default page with typing-momentum glow enabled.
#[test]
fn the_letter_beside_a_comet_caret_reads_with_the_momentum_glow_on() {
    for gap_ms in [57u64, 40] {
        let held = comet_newest_letter(gap_ms, false);
        assert!(
            held >= 3.0,
            "{gap_ms} ms a key: the letter beside the caret reads {held:.2}:1"
        );
        let washed = comet_newest_letter(gap_ms, true);
        assert!(
            washed < 3.0,
            "{gap_ms} ms a key: control: the unbudgeted streak leaves {washed:.2}:1"
        );
    }
}

/// On a light theme, the comet strings four-point ice glints along its tail outside the glyph band, whereas Lumen lays none.
#[test]
fn the_light_comet_strings_ice_glints_that_lumen_does_not() {
    let g = tall_geom();
    const PAPER: u32 = 0x00FF_FFFF;
    let run = |style: GlowStyle| {
        let mut c = cfg(style, true);
        c.dark_theme = false;
        (c.theme_fg, c.theme_bg) = (0x001F_2328, PAPER);
        c.color = crate::cursor_glow::COMET_DEFAULT_COLOR;
        c.intensity = 1.0;
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        let row = 2u16;
        glow.tick(Some((row, 4)), t, &c, g, &mut out);
        for col in 5..=14u16 {
            t += Duration::from_millis(60);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((row, col)), t, &c, g, &mut out);
        }
        let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
        let mut px = vec![PAPER; w * h];
        for q in glow.under_quads() {
            let qw = u32::from(q.w);
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for i in 0..qw {
                    let x = usize::from(q.x) + i as usize;
                    if x >= w {
                        break;
                    }
                    let col = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                    let a = aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, qw)
                        as u8;
                    px[y * w + x] = aterm_render::over_premul(px[y * w + x], col, a) & 0x00FF_FFFF;
                }
            }
        }
        (glow.light_glints.clone(), px, w)
    };
    let (glints, comet, w) = run(GlowStyle::Comet);
    let (none, _, _) = run(GlowStyle::Lumen);
    let caret_x = 14.0 * g.cw as f32;
    let arm = g.ch as f32 * CursorGlow::COMET_LIGHT_GLINT_ARM;
    let deep = |x: f32, y: f32| {
        1.0 - crate::color_math::relative_luminance(
            comet[y.round() as usize * w + x.round() as usize],
        )
    };
    let crossed = |(cx, cy): (f32, f32)| {
        let mean = |pts: [(f32, f32); 4]| {
            pts.iter()
                .map(|&(dx, dy)| deep(cx + dx, cy + dy))
                .sum::<f32>()
                / 4.0
        };
        let (a, d) = (arm * 0.35, arm * 0.35);
        mean([(a, 0.0), (-a, 0.0), (0.0, a), (0.0, -a)])
            > mean([(d, d), (-d, d), (d, -d), (-d, -d)]) + 0.04
    };
    assert!(
        glints.len() >= 2,
        "the light comet carries glints ({})",
        glints.len()
    );
    let row = (2 * g.ch) as f32..(3 * g.ch) as f32;
    let chf = g.ch as f32;
    for &centre in &glints {
        assert!(
            centre.0 < caret_x,
            "{centre:?}: a glint lies behind the caret"
        );
        assert!(
            row.contains(&centre.1),
            "{centre:?}: a glint lies in the typed row"
        );
        let (high, low) = CursorGlow::COMET_LIGHT_GLINT_RISE;
        let rise = chf
            * if centre.1 - row.start > 0.5 * chf {
                low
            } else {
                high
            };
        let (top, bottom) = (centre.1 - rise - row.start, centre.1 + rise - row.start);
        assert!(
            bottom <= 0.15 * chf || top >= 0.77 * chf,
            "{centre:?}: a glint reaches the letters' band ({:.2}..{:.2} of the row)",
            top / chf,
            bottom / chf
        );
    }
    let shown = glints.iter().filter(|&&c| crossed(c)).count();
    assert!(
        shown >= 2,
        "crosses of white in the tail: {shown} of {glints:?}"
    );
    assert!(none.is_empty(), "Lumen lays no glint");
    let between = (0.5 * (glints[0].0 + glints[1].0), row.start + 0.54 * chf);
    assert!(
        !crossed(between),
        "control: the tail between two glints shows no cross"
    );
}

/// On a light theme, the comet draws a round, rimmed coma past the caret's leading edge, whereas Lumen and Beam do not.
#[test]
fn the_light_comet_carries_a_round_rimmed_coma_that_lumen_does_not() {
    let g = tall_geom();
    const PAPER: u32 = 0x00FF_FFFF;
    let row = 2u16;
    let caret = 14u16;
    let run = |style: GlowStyle, linger_ms: u64| {
        let mut c = cfg(style, true);
        c.dark_theme = false;
        (c.theme_fg, c.theme_bg) = (0x001F_2328, PAPER);
        c.color = crate::cursor_glow::COMET_DEFAULT_COLOR;
        c.intensity = 1.0;
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        glow.tick(Some((row, 4)), t, &c, g, &mut out);
        for col in 5..=caret {
            t += Duration::from_millis(60);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((row, col)), t, &c, g, &mut out);
        }
        if linger_ms > 0 {
            t += Duration::from_millis(linger_ms);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
        }
        let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
        let mut px = vec![PAPER; w * h];
        for q in glow.under_quads() {
            let qw = u32::from(q.w);
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for i in 0..qw {
                    let x = usize::from(q.x) + i as usize;
                    if x >= w {
                        break;
                    }
                    let col = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                    let a = aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, qw)
                        as u8;
                    px[y * w + x] = aterm_render::over_premul(px[y * w + x], col, a) & 0x00FF_FFFF;
                }
            }
        }
        move |x: i32, y: i32| {
            1.0 - crate::color_math::relative_luminance(px[y as usize * w + x as usize])
        }
    };
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let lead = i32::from(caret + 1) * cw;
    let mid = i32::from(row) * ch + ch / 2;
    let past = |deep: &dyn Fn(i32, i32) -> f32, y: i32| {
        let mut best = (0.0f32, 0i32);
        for x in lead..lead + ch {
            let d = deep(x, y);
            if d > 0.02 {
                best = (best.0.max(d), x - lead + 1);
            }
        }
        best
    };
    let comet = run(GlowStyle::Comet, 0);
    let (_, reach_mid) = past(&comet, mid);
    let (_, reach_high) = past(&comet, mid - (ch * 2) / 5);
    assert!(
        reach_mid >= 3,
        "no coma past the caret: it reaches {reach_mid} px"
    );
    assert!(
        reach_high + 2 < reach_mid,
        "the coma is not round: {reach_high} px near the row's top against {reach_mid} px across its middle"
    );
    let fill = comet(lead + 1, mid);
    let rim = (lead..lead + reach_mid)
        .map(|x| comet(x, mid))
        .fold(0.0f32, f32::max);
    let edge = comet(lead + reach_mid - 2, mid);
    assert!(
        edge > fill * 1.5 && (rim - edge).abs() < rim * 0.5,
        "the coma has no rim: fill {fill:.3}, near its edge {edge:.3}, deepest {rim:.3}"
    );
    for style in [GlowStyle::Lumen, GlowStyle::Beam] {
        let other = run(style, 0);
        let (deep, _) = past(&other, mid);
        assert!(
            deep <= 0.02,
            "{style:?} lays ink past the caret ({deep:.3})"
        );
    }
    let cold = run(GlowStyle::Comet, 3000);
    let (deep, _) = past(&cold, mid);
    assert!(
        deep <= 0.02,
        "control: a cold comet keeps its coma ({deep:.3})"
    );
}

/// On a dark theme, the comet's dust grains stay in the leading outside the typed row's glyph band.
#[test]
fn the_dark_comet_dust_keeps_to_the_leading() {
    let g = tall_geom();
    let row = 3u16;
    let band = |unbudgeted: bool| -> (usize, usize) {
        let mut c = cfg(GlowStyle::Comet, true);
        c.intensity = 1.0;
        (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, 0x0011_1318);
        let mut glow = CursorGlow {
            streak_unbudgeted: unbudgeted,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        let (mut in_band, mut shown) = (0usize, 0usize);
        for k in 0..=24u16 {
            if k > 0 {
                t += Duration::from_millis(60);
                if k % 5 != 0 {
                    text[usize::from(2 + k - 1)] = 'm';
                }
                glow.note_synthetic_typed(t, 1);
            }
            let caret = 2 + k;
            glow.observe_row(row, caret, &text, t);
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for (col, &ch) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = ch;
            }
            glow.observe_frame_text(Some((row, caret)), &rows, t);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
            let top = (u32::from(row) * g.ch as u32) as f32;
            let chf = g.ch as f32;
            for h in glow.halos() {
                // A grain's mote (the crown is a wide halo).
                if h.rx > (g.ch / 4) as u16 {
                    continue;
                }
                shown += 1;
                let k = (f32::from(h.cy) - top) / chf;
                if (0.2..0.8).contains(&k) {
                    in_band += 1;
                }
            }
        }
        (in_band, shown)
    };
    let (in_band, shown) = band(false);
    assert!(shown > 0, "fixture: the comet sheds dust");
    assert_eq!(
        in_band, 0,
        "{in_band} of {shown} grains stand at the letters' x-height"
    );
    let (bare, _) = band(true);
    assert!(
        bare > 0,
        "control: with the over-ink laws off grains stand at the x-height"
    );
}
