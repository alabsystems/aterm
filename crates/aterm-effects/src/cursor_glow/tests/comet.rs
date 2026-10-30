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

/// On a light theme the comet's dust falls behind it as round motes (looks10
/// L4), into the blank row below the typed one, or above where only that one
/// is blank, and onto no lettered row (Q1): with both beside it filled, it
/// lays nothing outside its own row. Negative control: seated in the typed
/// row's own leading, as looks9's crosses were (`comet_dust_in_row`), the
/// blank row below holds no dust.
#[test]
fn the_paper_comet_drops_round_dust_into_the_blank_row_beside_it() {
    let g = tall_geom();
    const PAPER: u32 = 0x00FF_FFFF;
    let row = 3u16;
    let caret = 16u16;
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
    // The lit pixels (deeper than the paper by a step a 1x frame shows) of
    // the frame at the last key, the rows above and below the typed one
    // lettered or blank.
    let run = |above: bool, below: bool, in_row: bool| -> Vec<(i32, i32)> {
        let mut c = cfg(GlowStyle::Comet, true);
        c.dark_theme = false;
        (c.theme_fg, c.theme_bg) = (0x001F_2328, PAPER);
        c.color = crate::cursor_glow::COMET_DEFAULT_COLOR;
        c.intensity = 1.0;
        let mut glow = CursorGlow {
            comet_dust_in_row: in_row,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut t = Instant::now();
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for cell in rows.iter_mut().flatten() {
            cell.bg = [0xFF; 3];
        }
        for (fill, r) in [(above, row - 1), (below, row + 1)] {
            if fill {
                for cell in &mut rows[usize::from(r)][..30] {
                    cell.ch = 'm';
                }
            }
        }
        for col in 4..=caret {
            if col > 4 {
                t += Duration::from_millis(60);
                rows[usize::from(row)][usize::from(col - 1)].ch = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            glow.observe_frame_text(Some((row, col)), &rows, t);
            glow.tick(Some((row, col)), t, &c, g, &mut out);
        }
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
        let mut lit = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if 1.0 - crate::color_math::relative_luminance(px[y * w + x]) > 0.04 {
                    lit.push((x as i32, y as i32));
                }
            }
        }
        lit
    };
    let in_rows = |lit: &[(i32, i32)], r: u16| -> Vec<(i32, i32)> {
        let top = i32::from(r) * ch;
        lit.iter()
            .copied()
            .filter(|&(_, y)| (top..top + ch).contains(&y))
            .collect()
    };
    // The motes the pixels of one row make: groups of lit pixels touching.
    let motes = |pts: &[(i32, i32)]| -> Vec<(i32, i32, i32, i32)> {
        let mut left: Vec<(i32, i32)> = pts.to_vec();
        let mut boxes = Vec::new();
        while let Some(seed) = left.pop() {
            let mut group = vec![seed];
            let mut i = 0;
            while i < group.len() {
                let (gx, gy) = group[i];
                let (near, far): (Vec<_>, Vec<_>) = left
                    .iter()
                    .partition(|&&(x, y)| (x - gx).abs() <= 1 && (y - gy).abs() <= 1);
                group.extend(near);
                left = far;
                i += 1;
            }
            let x0 = group.iter().map(|p| p.0).min().unwrap_or(0);
            let x1 = group.iter().map(|p| p.0).max().unwrap_or(0);
            let y0 = group.iter().map(|p| p.1).min().unwrap_or(0);
            let y1 = group.iter().map(|p| p.1).max().unwrap_or(0);
            boxes.push((x0, y0, x1, y1));
        }
        boxes
    };
    let head = i32::from(caret) * cw;
    let r = g.ch as f32 * CursorGlow::COMET_PAPER_DUST_R;
    let span = (2.0 * r).ceil() as i32 + 1;
    for (above, below, dust_row) in [
        (false, false, row + 1),
        (true, false, row + 1),
        (false, true, row - 1),
    ] {
        let lit = run(above, below, false);
        let dust = motes(&in_rows(&lit, dust_row));
        assert!(
            dust.len() >= 2,
            "above lettered {above}, below {below}: {} motes in row {dust_row}",
            dust.len()
        );
        for &(x0, y0, x1, y1) in &dust {
            let (mw, mh) = (x1 - x0 + 1, y1 - y0 + 1);
            assert!(
                x1 < head,
                "a mote at x {x0}..{x1} lies past the head's cell ({head})"
            );
            assert!(
                mw <= span && mh <= span && (mw - mh).abs() <= 1,
                "a mote {mw}x{mh} px is no round speck (at most {span} px across)"
            );
        }
        let other = if dust_row > row { row - 1 } else { row + 1 };
        let stray = in_rows(&lit, other);
        assert!(
            stray.is_empty(),
            "above lettered {above}, below {below}: {} px lit in row {other}",
            stray.len()
        );
    }
    let boxed = run(true, true, false);
    let out_of_row: Vec<_> = boxed
        .iter()
        .filter(|&&(_, y)| !(i32::from(row) * ch..i32::from(row + 1) * ch).contains(&y))
        .collect();
    assert!(
        out_of_row.is_empty(),
        "between lettered rows the comet lights {} px outside its own row",
        out_of_row.len()
    );
    let control = run(false, false, true);
    assert!(
        in_rows(&control, row + 1).is_empty(),
        "control: dust seated in the typed row's leading still lights the blank row below"
    );
}

/// looks9's final look, THE NUCLEUS IS THE TAIL'S ROUND HEAD: typing a line
/// on the reference dark over a bare row, the icy tail running whole under
/// the nucleus to the caret's far edge saturated to a hard white bar at the
/// caret's foot. Feathered out across the caret's cell
/// ([`crate::cursor_glow::bed::BedScratch::nucleus_bar`]), the light under the
/// caret's far edge is under three fifths of the light under its middle,
/// where the nucleus stands. Negative control: run whole to the edge
/// (`comet_nucleus_bar`), it is the middle's light there too.
#[test]
fn the_dark_comet_s_nucleus_is_its_tail_s_round_head() {
    let g = tall_geom();
    const PAGE: u32 = 0x0011_1318;
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let row = 3u16;
    let (cw, ch) = (g.cw, g.ch);
    // The deepest light under the caret's middle and under its far edge.
    let lay = |bar: bool| -> (f32, f32) {
        let mut c = cfg(GlowStyle::Comet, true);
        c.intensity = 1.0;
        (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, PAGE);
        let mut glow = CursorGlow {
            comet_nucleus_bar: bar,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        let mut caret = 2u16;
        for k in 0..=12u16 {
            if k > 0 {
                t += Duration::from_millis(60);
                text[usize::from(2 + k - 1)] = 'm';
                glow.note_synthetic_typed(t, 1);
            }
            caret = 2 + k;
            glow.observe_row(row, caret, &text, t);
            let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
            for cell in rows.iter_mut().flatten() {
                cell.bg = rgb(PAGE);
            }
            for (col, &letter) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = letter;
            }
            glow.observe_frame_text(Some((row, caret)), &rows, t);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
        }
        let mut glass = Glass::new(g, PAGE);
        glass.add_quads(glow.under_quads());
        let bottom = (usize::from(row) + 1) * ch;
        let x0 = usize::from(caret) * cw;
        let light = |xs: std::ops::Range<usize>| {
            let mut deepest = 0.0f32;
            for y in bottom..bottom + ch / 3 {
                for x in xs.clone() {
                    deepest = deepest.max(crate::color_math::oklab_distance(
                        glass.px[y * glass.w + x],
                        PAGE,
                    ));
                }
            }
            deepest
        };
        (
            light(x0 + cw * 3 / 8..x0 + cw * 5 / 8),
            light(x0 + cw - 2..x0 + cw),
        )
    };
    let (mid, edge) = lay(false);
    assert!(
        mid > 0.3 && edge < 0.6 * mid,
        "under the caret's middle {mid:.2}, under its far edge {edge:.2}"
    );
    let (bar_mid, bar_edge) = lay(true);
    assert!(
        bar_edge > 0.85 * bar_mid,
        "control: the whole tail lays {bar_edge:.2} under the caret's far edge, \
         {bar_mid:.2} under its middle"
    );
}

/// THE DARK COMET'S DUST FLOATS OUT BESIDE AN EMPTY ROW (looks8 round 3's K4: "Comet should
/// show a nucleus and dust"): held to the leading's thin seats against the letters' tops and
/// feet, the letter guard kept its dust to grey specks. Typing on a row whose neighbours hold
/// no letter, its grains float out into them, a fifth of a row and more past the typed row's
/// edges, and none stands at the letters' x-height; where the row above is lettered, none
/// floats into it. Negative controls: the old dust ([`CursorGlow::comet_dust_grey`]) floats
/// nowhere, and with the over-ink laws off ([`CursorGlow::streak_unbudgeted`]) grains stand at
/// the letters' x-height.
#[test]
fn the_dark_comet_dust_floats_out_beside_an_empty_row() {
    let g = tall_geom();
    let row = 3u16;
    let seats = |grey: bool, above: bool, unbudgeted: bool| -> (usize, usize, usize) {
        let mut c = cfg(GlowStyle::Comet, true);
        c.intensity = 1.0;
        (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, 0x0011_1318);
        let mut glow = CursorGlow {
            comet_dust_grey: grey,
            streak_unbudgeted: unbudgeted,
            ..CursorGlow::default()
        };
        let mut out = Vec::new();
        let mut text = vec![' '; g.cols];
        let mut t = Instant::now();
        let (mut floated, mut in_band, mut into_above) = (0usize, 0usize, 0usize);
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
                if above {
                    rows[usize::from(row - 1)][col].ch = 'm';
                }
            }
            glow.observe_frame_text(Some((row, caret)), &rows, t);
            glow.tick(Some((row, caret)), t, &c, g, &mut out);
            let top = (u32::from(row) * g.ch as u32) as f32;
            let chf = g.ch as f32;
            for h in glow.halos() {
                if h.rx > (g.ch / 4) as u16 {
                    continue;
                }
                let k = (f32::from(h.cy) - top) / chf;
                floated += usize::from(!(-0.1..1.1).contains(&k));
                in_band += usize::from((0.2..0.8).contains(&k));
                into_above += usize::from(k < -0.1);
            }
        }
        (floated, in_band, into_above)
    };
    let (floated, in_band, _) = seats(false, false, false);
    assert!(
        floated >= 20,
        "the dust floats out into the empty rows {floated} times"
    );
    assert_eq!(in_band, 0, "a grain stands at the letters' x-height");
    let (_, _, into_above) = seats(false, true, false);
    assert_eq!(into_above, 0, "a grain floats into the lettered row above");
    let (old, _, _) = seats(true, false, false);
    assert_eq!(old, 0, "control: the old dust floats out {old} times");
    let (_, bare, _) = seats(false, false, true);
    assert!(
        bare > 0,
        "control: with the over-ink laws off grains stand at the x-height"
    );
}

/// THE DARK COMET'S DUST IS DRAWN WHOLE OR NOT AT ALL AT THE GRID'S EDGE (looks8 round 3's
/// judges): typing on the grid's last row (an Enter at the bottom row hands it the prompt),
/// the dust seated under the row lay across the grid's bottom edge, where the effects box cuts
/// a halo flat, and read as a bright `...` beside the caret's lower right on dark and
/// Solarized Dark (in this fixture, grains crossed the edge after 28 of its 30 keys). No
/// grain's disc crosses the box's edge, and the dust still stands by the typed row.
#[test]
fn the_dark_comet_s_dust_is_whole_at_the_grid_s_edge() {
    let g = tall_geom();
    let row = (g.rows - 1) as u16;
    let mut c = cfg(GlowStyle::Comet, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, 0x0011_1318);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let mut t = Instant::now();
    let (fx_top, fx_bot) = (g.fx_top(), g.fx_bot());
    let row_top = i32::from(row) * i32::from(g.ch as u16);
    let (mut by_row, mut crossing) = (0usize, Vec::new());
    for k in 0..=30u16 {
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
        for h in glow.halos().iter().filter(|h| h.rx <= (g.ch / 4) as u16) {
            let (top, bot) = (
                i32::from(h.cy) - i32::from(h.ry),
                i32::from(h.cy) + i32::from(h.ry),
            );
            by_row += usize::from(bot > row_top - i32::from(g.ch as u16));
            if top < fx_top - 1 || bot > fx_bot + 1 {
                crossing.push((k, h.cy, h.ry));
            }
        }
    }
    assert!(
        by_row >= 20,
        "fixture: the comet's dust stands by the last row ({by_row} grains)"
    );
    assert!(
        crossing.is_empty(),
        "grains cross the grid's edge, cut flat there: {crossing:?}"
    );
}

/// The dark Comet's dust grains (small halos) over `keys` keys typed on `row`
/// of `tall_geom`, each frame's `(key, cx, cy, caret_x)`: `above` decides
/// the row above's cells (`None`: blank on the page; `Some(bg)`: a bar of that
/// ground with one letter at its far left), and `regimented` reverts the
/// dust to looks8's seats ([`CursorGlow::comet_dust_regimented`]).
fn comet_dust_frames(
    row: u16,
    keys: u16,
    above: Option<[u8; 3]>,
    regimented: bool,
) -> Vec<(u16, i32, i32, i32)> {
    comet_dust_grains(row, keys, above, |glow| {
        glow.comet_dust_regimented = regimented;
    })
    .into_iter()
    .map(|(k, cx, cy, caret_x, _)| (k, cx, cy, caret_x))
    .collect()
}

/// [`comet_dust_frames`], `tune` setting the comet's controls, each grain's
/// radius too: `(key, cx, cy, caret_x, r)`.
fn comet_dust_grains(
    row: u16,
    keys: u16,
    above: Option<[u8; 3]>,
    tune: impl FnOnce(&mut CursorGlow),
) -> Vec<(u16, i32, i32, i32, u16)> {
    let g = tall_geom();
    let mut c = cfg(GlowStyle::Comet, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, 0x0011_1318);
    let mut glow = CursorGlow::default();
    tune(&mut glow);
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let mut t = Instant::now();
    let mut grains = Vec::new();
    for k in 0..=keys {
        if k > 0 {
            t += Duration::from_millis(70);
            if k % 6 != 0 {
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
        if let Some(bg) = above {
            for cell in &mut rows[usize::from(row - 1)] {
                cell.bg = bg;
            }
            rows[usize::from(row - 1)][0].ch = '2';
        }
        glow.observe_frame_text(Some((row, caret)), &rows, t);
        glow.tick(Some((row, caret)), t, &c, g, &mut out);
        let caret_x = i32::from(caret) * g.cw as i32 + g.cw as i32 / 2;
        for h in glow.halos().iter().filter(|h| h.rx <= (g.ch / 4) as u16) {
            grains.push((k, i32::from(h.cx), i32::from(h.cy), caret_x, h.rx));
        }
    }
    grains
}

/// looks9 Q1, THE DARK COMET'S DUST KEEPS OFF A FILLED ROW: a grain floats
/// only into air on the page's own ground (no letter, and no cell on another
/// ground, within two cells of its column). In the trail
/// gallery's `context` the row above the typed line is a prompt's powerline
/// bar, blank but for its segment at the far left, and the dust settled on
/// it as white specks (392 px lifted, peak 207). No grain's disc reaches into
/// the bar, and the dust still shows. Negative control: looks8's air (a row
/// with no letter near, [`CursorGlow::comet_dust_regimented`]) floats grains
/// onto the bar.
#[test]
fn the_dark_comet_s_dust_keeps_off_a_filled_row() {
    let g = tall_geom();
    let row = 3u16;
    let row_top = i32::from(row) * g.ch as i32;
    let bar = Some([0x30, 0x30, 0x30]);
    let onto_bar = |regimented: bool| {
        let grains = comet_dust_frames(row, 24, bar, regimented);
        let on = grains
            .iter()
            .filter(|&&(_, _, cy, _)| cy < row_top + 1)
            .count();
        (on, grains.len())
    };
    let (on, shown) = onto_bar(false);
    assert!(shown >= 20, "fixture: the dust shows ({shown} grains)");
    assert_eq!(
        on, 0,
        "{on} of {shown} grains settle on the filled row above"
    );
    let (old, _) = onto_bar(true);
    assert!(old > 0, "control: looks8's air floats grains onto the bar");
    // On the page's own ground the row above is air.
    let up = comet_dust_frames(row, 24, None, false)
        .iter()
        .filter(|&&(_, _, cy, _)| cy < row_top)
        .count();
    assert!(up > 0, "the dust floats into a bare row above");
}

/// looks9 Q2, THE DARK COMET SHEDS ITS DUST BEHIND THE NUCLEUS, SCATTERING
/// WITH DISTANCE: seated at one depth a side, the typing dust lay in two
/// dotted rules above and below the typed row, bright grains pinned at the
/// caret's corners. Typing on a row with bare rows round it, no grain stands
/// ahead of the caret's centre, and the grains far behind the head lie
/// deeper from the row than those near it (the far half's mean depth a
/// tenth of a row and more past the near half's). Negative control: looks8's
/// seats ([`CursorGlow::comet_dust_regimented`]) stand grains ahead of the
/// head and keep one depth at every distance.
#[test]
fn the_dark_comet_sheds_its_dust_behind_the_nucleus() {
    let g = tall_geom();
    let row = 3u16;
    let (row_top, row_bot) = (
        i32::from(row) * g.ch as i32,
        (i32::from(row) + 1) * g.ch as i32,
    );
    let read = |regimented: bool| -> (usize, f32, usize) {
        let grains = comet_dust_frames(row, 28, None, regimented);
        let ahead = grains
            .iter()
            .filter(|&&(_, cx, _, caret_x)| cx > caret_x)
            .count();
        // Depth past the row's edge against distance behind the head, from
        // the frames once the line is running.
        let mut pts: Vec<(i32, i32)> = grains
            .iter()
            .filter(|&&(k, ..)| k >= 12)
            .filter_map(|&(_, cx, cy, caret_x)| {
                let depth = if cy >= row_bot {
                    cy - row_bot
                } else if cy < row_top {
                    row_top - cy
                } else {
                    return None;
                };
                Some((caret_x - cx, depth))
            })
            .collect();
        pts.sort_unstable();
        let half = pts.len() / 2;
        let mean =
            |s: &[(i32, i32)]| s.iter().map(|p| p.1 as f32).sum::<f32>() / (s.len().max(1) as f32);
        (ahead, mean(&pts[half..]) - mean(&pts[..half]), pts.len())
    };
    let (ahead, widening, n) = read(false);
    assert!(n >= 40, "fixture: the dust floats out ({n} grains)");
    assert_eq!(ahead, 0, "{ahead} grains stand ahead of the nucleus");
    assert!(
        widening >= 0.10 * g.ch as f32,
        "the dust far behind the head lies only {widening:.1} px deeper than near it"
    );
    let (old_ahead, old_widening, _) = read(true);
    assert!(
        old_ahead > 0 && old_widening < 0.10 * g.ch as f32,
        "control: looks8's seats stand {old_ahead} grains ahead and widen {old_widening:.1} px"
    );
}

/// The dark Comet's plume read off [`comet_dust_grains`] typing 32 keys on
/// row 3 of [`tall_geom`] (bare rows round it), the frames once the line is
/// running: the farthest a grain above the row lies behind the caret (cells),
/// the spread of the depths past the row's edge (px, their interquartile
/// range) among the grains below in the far third of the plume less that in
/// its near third, the far third's count of grains over the near third's,
/// and the largest grain's radius over the smallest's.
fn comet_plume_reading(even: bool) -> (f32, f32, f32, f32) {
    let g = tall_geom();
    let row = 3u16;
    let row_bot = (i32::from(row) + 1) * g.ch as i32;
    let row_top = i32::from(row) * g.ch as i32;
    let grains = comet_dust_grains(row, 32, None, |glow| {
        glow.comet_plume_even = even;
    });
    let cw = g.cw as f32;
    let running: Vec<_> = grains.iter().filter(|&&(k, ..)| k >= 12).collect();
    let above_reach = running
        .iter()
        .filter(|&&&(_, _, cy, _, _)| cy < row_top)
        .map(|&&(_, cx, _, caret_x, _)| (caret_x - cx) as f32 / cw)
        .fold(0.0f32, f32::max);
    let below: Vec<(i32, i32)> = running
        .iter()
        .filter(|&&&(_, _, cy, _, _)| cy >= row_bot)
        .map(|&&(_, cx, cy, caret_x, _)| (caret_x - cx, cy - row_bot))
        .collect();
    let reach = below.iter().map(|p| p.0).max().unwrap_or(0).max(1);
    let third = |i: i32| {
        let mut d: Vec<i32> = below
            .iter()
            .filter(|p| p.0 * 3 >= i * reach && p.0 * 3 < (i + 1) * reach)
            .map(|p| p.1)
            .collect();
        d.sort_unstable();
        d
    };
    let (near, far) = (third(0), third(2));
    let iqr = |d: &[i32]| {
        if d.len() < 4 {
            0.0
        } else {
            (d[d.len() * 3 / 4] - d[d.len() / 4]) as f32
        }
    };
    let rs: Vec<u16> = running.iter().map(|g| g.4).collect();
    let (rmin, rmax) = (
        rs.iter().copied().min().unwrap_or(1).max(1),
        rs.iter().copied().max().unwrap_or(1),
    );
    (
        above_reach,
        iqr(&far) - iqr(&near),
        far.len() as f32 / near.len().max(1) as f32,
        f32::from(rmax) / f32::from(rmin),
    )
}

/// The dark Comet's dust below a typed `row` (`unseeded`: as looks9 round 2
/// laid it, [`CursorGlow::comet_plume_unseeded`]), each frame from the
/// twelfth key on that shows six grains or more there: the median over
/// those frames of the spread of the grains' depths (their interquartile
/// range, px) and of how many pixel rows their centres take.
fn comet_plume_depths(row: u16, unseeded: bool) -> (f32, f32) {
    let g = tall_geom();
    let row_bot = (i32::from(row) + 1) * g.ch as i32;
    let grains = comet_dust_grains(row, 32, None, |glow| {
        glow.comet_plume_unseeded = unseeded;
    });
    let (mut iqrs, mut rows) = (Vec::new(), Vec::new());
    for k in 12..=32u16 {
        let mut d: Vec<i32> = grains
            .iter()
            .filter(|&&(at, _, cy, _, _)| at == k && cy >= row_bot)
            .map(|&(_, _, cy, _, _)| cy)
            .collect();
        if d.len() < 6 {
            continue;
        }
        d.sort_unstable();
        iqrs.push((d[d.len() * 3 / 4] - d[d.len() / 4]) as f32);
        d.dedup();
        rows.push(d.len() as f32);
    }
    let median = |mut v: Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    (median(iqrs), median(rows))
}

/// looks9 round 3: THE DARK COMET'S PLUME KEEPS ITS SCATTER IN ONE ROW OF
/// AIR. Typed on a pane's next-to-last row, the dust under the line has one
/// row of air, the pane's last, and round 2's plume, every fresh grain at
/// the near edge of its air and room left for the largest grain alone, lay
/// there in one dotted rule above the pane's divider (the gallery's `split`,
/// its specks' depths spread 1 px over 3 or 4 pixel rows). There, frame by
/// frame, the grains' depths spread 3 px or more (their interquartile
/// range) over 7 pixel rows or more. Negative control: round 2's plume
/// (`comet_plume_unseeded`) spreads 1.5 px or less over 5 rows or fewer.
#[test]
fn the_dark_comet_s_plume_keeps_its_scatter_in_one_row_of_air() {
    let row = (tall_geom().rows - 2) as u16;
    let (spread, rows) = comet_plume_depths(row, false);
    assert!(
        spread >= 3.0 && rows >= 7.0,
        "in one row of air the plume spreads {spread:.1} px over {rows} pixel rows"
    );
    let (old_spread, old_rows) = comet_plume_depths(row, true);
    assert!(
        old_spread <= 1.5 && old_rows <= 5.0,
        "control: round 2's plume spreads {old_spread:.1} px over {old_rows} rows"
    );
}

/// The dark Comet's dust below a pane's next-to-last row (`drifting`: as
/// looks9 round 3 first laid it, [`CursorGlow::comet_plume_drifting`]): the
/// median, over the frames from the twelfth key on that show six grains or
/// more there, of the deepest grain centre's height over the pane's end; and
/// the least gap, over every frame, between a grain's disc and the pane's
/// end (px).
fn comet_plume_floor(drifting: bool) -> (f32, i32) {
    let g = tall_geom();
    let row = (g.rows - 2) as u16;
    let row_bot = (i32::from(row) + 1) * g.ch as i32;
    let end = g.rows as i32 * g.ch as i32;
    let grains = comet_dust_grains(row, 32, None, |glow| {
        glow.comet_plume_drifting = drifting;
        // Round 3 drew every grain as it lay, a disc on the pane's end too:
        // the law that now drops a mark the effects box would cut stands
        // aside for its replay.
        glow.set_motes_cut(drifting);
    });
    let under = |k: Option<u16>| -> Vec<(i32, i32)> {
        grains
            .iter()
            .filter(|&&(at, _, cy, _, _)| k.is_none_or(|k| at == k) && cy >= row_bot)
            .map(|&(_, _, cy, _, rx)| (cy, i32::from(rx)))
            .collect()
    };
    let mut heights: Vec<f32> = (12..=32u16)
        .map(|k| under(Some(k)))
        .filter(|u| u.len() >= 6)
        .map(|u| (end - u.iter().map(|p| p.0).max().unwrap_or(end)) as f32)
        .collect();
    heights.sort_by(f32::total_cmp);
    let gap = under(None)
        .iter()
        .map(|&(cy, rx)| end - (cy + rx))
        .min()
        .unwrap_or(end);
    (heights.get(heights.len() / 2).copied().unwrap_or(0.0), gap)
}

/// looks9 round 3: THE DARK COMET'S PLUME FLOATS CLEAR OF THE PANE'S END.
/// In one row of air (a pane's last row under the typed line) round 3 first
/// seeded each grain's depth and let it drift on to the air's end: the
/// scatter came back, but the dust lay along the pane's divider (the
/// gallery's `split`: its deepest specks 2 to 4 px over the pane's end in
/// every capture, base's 7 to 9). There each grain keeps the depth it was
/// shed at, its disc beneath the icy tail by its own reach and short of the
/// pane's end by the gap a lit rail keeps from its row: frame by frame the
/// deepest grain sits 4 px or more over the pane's end (the probe read 5),
/// and no grain's disc comes within a pixel of it (the probe: 2 px). Negative
/// control: round 3's drifting plume (`comet_plume_drifting`) sits 3 px or
/// less over the end, its discs on it (the probe: 2 px, 0 px), replayed with
/// every grain drawn as it lay ([`CursorGlow::set_motes_cut`]).
#[test]
fn the_dark_comet_s_plume_floats_clear_of_the_pane_s_end() {
    let (height, gap) = comet_plume_floor(false);
    assert!(
        height >= 4.0 && gap >= 1,
        "the plume's deepest grain sits {height:.1} px over the pane's end, a disc {gap} px off it"
    );
    let (old_height, old_gap) = comet_plume_floor(true);
    assert!(
        old_height <= 3.0 && old_gap <= 0,
        "control: round 3's drifting plume sits {old_height:.1} px over the end, a disc {old_gap} px off it"
    );
}

/// looks9 Q2, round 2: THE DARK COMET'S PLUME FANS OUT AND THINS BEHIND ITS
/// HEAD. Round 1's plume still lay in two even bands, one a side and a fifth
/// of a row deep, its beads alike: two dotted rules above and below the typed
/// row. The dust above the row is a spray beside the nucleus (none more than
/// eight cells behind the caret); below, the far third of the plume spreads
/// over a wider range of depths than its near third (by an eighth of a row
/// and more), holds under a quarter as many grains as it, and the grains run
/// from fine to coarse (the largest five times the smallest's radius and
/// more). Negative control: round 1's plume (`comet_plume_even`) keeps its
/// dust above the row to the end of the line, one depth range at every
/// distance, as many grains far as near, at one size.
#[test]
fn the_dark_comet_s_plume_fans_out_and_thins() {
    let ch = tall_geom().ch as f32;
    let (above, spread, thin, sizes) = comet_plume_reading(false);
    assert!(
        above <= 8.0,
        "the dust above the row lies {above:.1} cells behind the caret"
    );
    assert!(
        spread >= 0.13 * ch,
        "the plume's far third spreads only {spread:.1} px wider than its near third"
    );
    assert!(
        thin <= 0.27,
        "the plume's far third holds {thin:.2} of its near third's grains"
    );
    assert!(
        sizes >= 5.0,
        "the plume's largest grain is {sizes:.1} times its smallest"
    );
    let (e_above, e_spread, e_thin, e_sizes) = comet_plume_reading(true);
    assert!(
        e_above > 8.0 && e_spread < 0.13 * ch && e_thin > 0.27 && e_sizes < 5.0,
        "control: round 1's plume reads {:?}",
        (e_above, e_spread, e_thin, e_sizes)
    );
}

/// What [`comet_typed_over`] returns: the glow, its config, the frame's rows,
/// the last key's time, and each frame's grains `(key, cy)`.
type CometTyped = (
    CursorGlow,
    GlowConfig,
    Vec<Vec<aterm_core::terminal::RenderCell>>,
    Instant,
    Vec<(u16, i32)>,
);

/// The dark Comet typing `m` on row 3 of `tall_geom` from column 2 to
/// `caret - 1` at 70 ms a key, row 4 holding `below` from its first column;
/// `tune` sets the comet's controls. Returns the glow, its config and the
/// frame's rows, and the last key's time.
fn comet_typed_over(below: &str, caret: u16, tune: impl FnOnce(&mut CursorGlow)) -> CometTyped {
    let g = tall_geom();
    let row = 3u16;
    let mut c = cfg(GlowStyle::Comet, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, 0x0011_1318);
    let mut glow = CursorGlow::default();
    tune(&mut glow);
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let rows_of = |text: &[char]| {
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for (col, &ch) in text.iter().enumerate() {
            rows[usize::from(row)][col].ch = ch;
        }
        for (col, ch) in below.chars().enumerate() {
            rows[usize::from(row + 1)][col].ch = ch;
        }
        rows
    };
    let mut t = Instant::now();
    let mut grains = Vec::new();
    for k in 2..caret {
        t += Duration::from_millis(70);
        text[usize::from(k)] = 'm';
        glow.note_synthetic_typed(t, 1);
        glow.observe_row(row, k + 1, &text, t);
        glow.observe_frame_text(Some((row, k + 1)), &rows_of(&text), t);
        glow.tick(Some((row, k + 1)), t, &c, g, &mut out);
        for h in glow.halos().iter().filter(|h| h.rx <= (g.ch / 4) as u16) {
            grains.push((k, i32::from(h.cy)));
        }
    }
    let rows = rows_of(&text);
    (glow, c, rows, t, grains)
}

/// looks9's review of Q1: THE DARK COMET'S DUST FLOATS ONLY INTO A ROW BLANK
/// ACROSS ON THE PAGE'S OWN GROUND (the ruling). Read by the cells round
/// each grain's column, a row holding letters three cells off took the
/// dust, and a grain drifting toward them went out in one frame (the
/// review: under row 4's `mmmmmmmmmmmmm`, a grain shed at column 15 floated
/// into it for 13 frames, then dropped a row and went out). Typed on row 3
/// to column 30 over a row 4 lettered from its first column to its 13th, no
/// grain floats into row 4; over a bare row 4 the dust does. Negative
/// control (`comet_air_by_column`): read by the grain's column, grains float
/// into the lettered row.
#[test]
fn the_dark_comet_s_dust_floats_only_into_a_row_blank_across() {
    let g = tall_geom();
    let into = i32::from(4u16) * g.ch as i32 + g.ch as i32 / 10;
    let floated = |below: &str, by_column: bool| {
        let (.., grains) = comet_typed_over(below, 31, |glow| {
            glow.comet_air_by_column = by_column;
        });
        (
            grains.iter().filter(|&&(_, cy)| cy > into).count(),
            grains.len(),
        )
    };
    let (bare, shown) = floated("", false);
    assert!(
        shown >= 20 && bare >= 10,
        "fixture: over a bare row the dust floats into it ({bare} of {shown} grains)"
    );
    let lettered = "mmmmmmmmmmmmm";
    let (into_letters, shown) = floated(lettered, false);
    assert!(shown >= 20, "fixture: the dust shows ({shown} grains)");
    assert_eq!(
        into_letters, 0,
        "{into_letters} of {shown} grains float into the lettered row below"
    );
    let (old, _) = floated(lettered, true);
    assert!(
        old > 0,
        "control: read by the grain's column, no grain floats into the lettered row"
    );
}

/// One grain shed below row 3 of [`comet_typed_over`] (typed to column 17,
/// rows 4 and 5 bare, row 2 holding `let a = 1;`), its seat low and never
/// thinned out, drifting back at 6 px a second; each 16 ms frame the host
/// reads the frame and ticks, the caret stepping up to row 2 at `step_ms`.
/// Returns the grain's height each frame (`None`: not drawn).
fn comet_grain_heights(step_ms: Option<u64>, unlatched: bool) -> Vec<Option<i32>> {
    let g = tall_geom();
    let (row, caret) = (3u16, 18u16);
    let (mut glow, c, mut rows, born, _) = comet_typed_over("", caret, |glow| {
        glow.comet_seat_unlatched = unlatched;
    });
    let mut up = vec![' '; g.cols];
    for (col, ch) in "let a = 1;".chars().enumerate() {
        up[col] = ch;
        rows[usize::from(row - 1)][col].ch = ch;
    }
    let text: Vec<char> = rows[usize::from(row)].iter().map(|cell| cell.ch).collect();
    // A grain seated below the row (its seat past the dust's share above)
    // that the fan never thins out.
    let hue = (1u32..)
        .map(|i| i as f32 / 100_003.0)
        .find(|h| (0.3..0.8).contains(&(h * 977.0).fract()) && (h * 6007.0).fract() > 0.85)
        .unwrap_or(0.5);
    glow.particles.clear();
    glow.particles.push(Particle {
        x0: 14.4 * g.cw as f32,
        y0: (i32::from(row) * g.ch as i32 + g.ch as i32 / 2) as f32,
        vx: -6.0,
        vy: 0.0,
        gy: 0.25 * g.ch as f32,
        life: 1.05,
        hue,
        cov_scale: 1.0,
        born,
        latch: ParticleLatch::FRESH,
        edge_cut: false,
    });
    let mut out = Vec::new();
    (1..66u64)
        .map(|f| {
            let ms = f * 16;
            let now = born + Duration::from_millis(ms);
            let cur = match step_ms {
                Some(s) if ms >= s => (row - 1, caret),
                _ => (row, caret),
            };
            let on: &[char] = if cur.0 == row { &text } else { &up };
            glow.observe_row(cur.0, cur.1, on, now);
            glow.observe_frame_text(Some(cur), &rows, now);
            glow.tick(Some(cur), now, &c, g, &mut out);
            glow.particles.retain(|p| p.hue == hue);
            glow.halos()
                .iter()
                .find(|h| h.rx <= (g.ch / 4) as u16 && h.cx < 16 * g.cw as u16)
                .map(|h| i32::from(h.cy))
        })
        .collect()
}

/// looks9's review: A COMET GRAIN KEEPS ITS SEAT FOR ITS LIFE. Seated afresh
/// every frame, a grain floating in the air under the typed row read the
/// row past that air off the frame's read of the rows round the caret; when
/// the caret stepped up a row that row fell out of the read, and every deep
/// grain snapped up a row's height (14 px in one frame) and stopped. One
/// grain shed under the row drifts the same whether the caret stays or steps
/// up a row early, midway or late: drawn on every frame, it never moves more
/// than 2 px in a frame and ends within a pixel of where it ends with the
/// caret still. Negative control (`comet_seat_unlatched`): seated afresh,
/// the grain jumps 4 px and more in a frame.
#[test]
fn a_comet_grain_keeps_its_seat_for_its_life() {
    let read = |heights: &[Option<i32>]| -> (usize, i32, Option<i32>) {
        let drawn = heights.iter().filter(|h| h.is_some()).count();
        let jump = heights
            .windows(2)
            .filter_map(|w| Some((w[1]? - w[0]?).abs()))
            .max()
            .unwrap_or(0);
        (drawn, jump, heights.iter().rev().find_map(|&h| h))
    };
    let (still_drawn, still_jump, still_end) = read(&comet_grain_heights(None, false));
    assert!(
        still_drawn >= 60 && still_jump <= 2,
        "fixture: with the caret still the grain drifts ({still_drawn} frames, {still_jump} px)"
    );
    for step in [160u64, 480, 800] {
        let heights = comet_grain_heights(Some(step), false);
        let (drawn, jump, end) = read(&heights);
        assert_eq!(
            drawn, still_drawn,
            "caret up at {step} ms: the grain drawn on {drawn} frames ({heights:?})"
        );
        assert!(
            jump <= 2,
            "caret up at {step} ms: the grain jumps {jump} px in a frame ({heights:?})"
        );
        assert!(
            matches!((end, still_end), (Some(a), Some(b)) if (a - b).abs() <= 1),
            "caret up at {step} ms: the grain ends at {end:?}, {still_end:?} with the caret still"
        );
        let (_, old_jump, _) = read(&comet_grain_heights(Some(step), true));
        assert!(
            old_jump >= 4,
            "control: caret up at {step} ms, seated afresh, the grain jumps only {old_jump} px"
        );
    }
}

/// Comet typed along a dense row (`m`s on filled rows above and below the
/// caret's row 3, twelve keys) on `(page, ink)`, `seams` set on the glow:
/// the last frame's under stream on glass, with the caret's far edge, x px.
fn dense_row_comet((page, ink): (u32, u32), seams: impl Fn(&mut CursorGlow)) -> (Glass, usize) {
    const FILL: u32 = 0x0026_4F78;
    let g = tall_geom();
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let row = 3u16;
    let mut c = cfg(GlowStyle::Comet, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (ink, page);
    let mut glow = CursorGlow::default();
    seams(&mut glow);
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    let mut caret = 2u16;
    for k in 0..=12u16 {
        if k > 0 {
            t += Duration::from_millis(60);
            text[usize::from(2 + k - 1)] = 'm';
            glow.note_synthetic_typed(t, 1);
        }
        caret = 2 + k;
        glow.observe_row(row, caret, &text, t);
        let mut rows = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for (r, line) in rows.iter_mut().enumerate() {
            for (col, cell) in line.iter_mut().enumerate() {
                (cell.fg, cell.bg) = (rgb(ink), rgb(page));
                if r + 1 == usize::from(row) || r == usize::from(row) + 1 {
                    (cell.ch, cell.bg) = ('m', rgb(FILL));
                } else if r == usize::from(row) {
                    cell.ch = text[col];
                }
            }
        }
        glow.observe_frame_text(Some((row, caret)), &rows, t);
        glow.tick(Some((row, caret)), t, &c, g, &mut out);
    }
    let mut glass = Glass::new(g, page);
    glass.add_quads(glow.under_quads());
    (glass, (usize::from(caret) + 1) * g.cw)
}

/// looks10 round 2's L2, A DENSE ROW SEATS ITS COMET IN ITS OWN AIR: in the
/// gallery's `context` (a powerline bar over the typed line, a selection
/// under it) the nucleus had no room under the caret and Comet read as a
/// dim Beam rail, a 2 px underline with no head. Typing that line on the
/// reference dark and on Solarized Dark, its last frame lights the blank
/// air past the caret in the caret's own row: its brightest light a round
/// head [`CursorGlow::COMET_SEAT_CELLS`] out, well past the caret's glow,
/// and a wisp back toward the caret dimmer at its tail than at the head;
/// the lettered rows above and below take none of it. Negative control:
/// with no seat (`comet_seat_off`), the air past the caret stays dark.
#[test]
fn a_dense_row_seats_its_comet_in_its_own_air() {
    let g = tall_geom();
    let row = 3u16;
    let (cw, ch) = (g.cw, g.ch);
    // (the brightest light past the caret and its x, the wisp's light in its
    // tail third and its head third, lit px past the caret in the rows
    // beside it, lit px past the caret in its row)
    let lay = |(page, ink): (u32, u32), off: bool| {
        let (glass, edge) = dense_row_comet((page, ink), |g| g.comet_seat_off = off);
        let far = edge + 3 * cw;
        let light =
            |x: usize, y: usize| crate::color_math::oklab_distance(glass.px[y * glass.w + x], page);
        let top = usize::from(row) * ch;
        let (mut best, mut best_x) = (0.0f32, 0usize);
        let mut lit_in_row = 0usize;
        for y in top..top + ch {
            for x in edge..far {
                let d = light(x, y);
                if d > best {
                    (best, best_x) = (d, x);
                }
                lit_in_row += usize::from(d > 0.03);
            }
        }
        let mid = top + ch / 2;
        let third = |from: usize, to: usize| {
            (from..to)
                .map(|x| light(x, mid).max(light(x, mid - 1)))
                .sum::<f32>()
                / (to - from).max(1) as f32
        };
        let tail_from = edge + cw * 3 / 10 + 1;
        let span = best_x.saturating_sub(tail_from);
        let wisp = (
            third(tail_from, tail_from + span / 3),
            third(best_x.saturating_sub(span / 3), best_x),
        );
        let mut beside = 0usize;
        for y in (top - ch..top).chain(top + ch..top + 2 * ch) {
            for x in edge..far {
                beside += usize::from(light(x, y) > 0.03);
            }
        }
        (best, best_x.saturating_sub(edge), wisp, beside, lit_in_row)
    };
    for theme in [(0x0011_1318, 0x00D0_D0D0), (0x0000_2B36, 0x0083_9496)] {
        let page = theme.0;
        let (best, best_at, (tail, head), beside, _) = lay(theme, false);
        assert!(
            best > 0.3 && best_at >= cw * 3 / 2,
            "on {page:06x}: the brightest light past the caret is {best:.2}, {best_at} px past it"
        );
        assert!(
            tail > 0.0 && tail < 0.6 * head,
            "on {page:06x}: the wisp lays {tail:.3} at its tail, {head:.3} toward the head"
        );
        assert_eq!(
            beside, 0,
            "on {page:06x}: the seat lights {beside} px in the rows beside"
        );
        let (_, _, _, _, bare) = lay(theme, true);
        assert_eq!(
            bare, 0,
            "control: on {page:06x} with no seat the air past the caret lights {bare} px"
        );
    }
}

/// looks10 round 3, A DENSE ROW'S COMET LEAVES THE CARET: round 2's seat (a
/// needle wisp swelling to a small head, a third of a cell off the caret)
/// read as a pin (`—•`) apart from the caret in the gallery's `context`. On
/// the reference dark and on Solarized Dark, the seat's tail now lights the
/// row's middle from the caret's far edge to the head with no dark column,
/// and it is broad where it leaves the caret: a quarter of the way out it
/// lights at least as many rows as three quarters out. Negative control: the
/// seat as round 2 laid it (`comet_seat_pinned`) leaves dark columns by the
/// caret, and its needle is narrower there than by the head.
#[test]
fn a_dense_row_s_comet_leaves_the_caret() {
    let g = tall_geom();
    let (row, ch) = (3usize, g.ch);
    // (dark columns between the caret's edge and the head, lit rows a
    // quarter of the way out, lit rows three quarters of the way out)
    let read = |(page, ink): (u32, u32), pinned: bool| {
        let (glass, edge) = dense_row_comet((page, ink), |g| g.comet_seat_pinned = pinned);
        let light =
            |x: usize, y: usize| crate::color_math::oklab_distance(glass.px[y * glass.w + x], page);
        let (top, mid) = (row * ch, row * ch + ch / 2);
        let column = |x: usize| {
            (mid - 2..=mid + 2)
                .map(|y| light(x, y))
                .fold(0.0f32, f32::max)
        };
        let head = (edge..edge + 3 * g.cw)
            .max_by(|&a, &b| column(a).total_cmp(&column(b)))
            .expect("columns past the caret");
        let dark = (edge..head).filter(|&x| column(x) < 0.03).count();
        let lit_rows = |x: usize| (top..top + ch).filter(|&y| light(x, y) > 0.03).count();
        let span = head - edge;
        (
            dark,
            lit_rows(edge + span / 4),
            lit_rows(edge + 3 * span / 4),
        )
    };
    for theme in [(0x0011_1318, 0x00D0_D0D0), (0x0000_2B36, 0x0083_9496)] {
        let page = theme.0;
        let (dark, near, far) = read(theme, false);
        assert_eq!(
            dark, 0,
            "on {page:06x}: {dark} dark columns between the caret and the comet's head"
        );
        assert!(
            near >= far && near > 0,
            "on {page:06x}: the tail lights {near} rows by the caret, {far} by the head"
        );
        let (dark, near, far) = read(theme, true);
        assert!(
            dark > 0 && near < far,
            "control: on {page:06x} round 2's pin keeps {dark} dark columns by the caret, \
             {near} rows lit by the caret and {far} by the head"
        );
    }
}
