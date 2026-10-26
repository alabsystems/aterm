// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Light-ground cursor-glow streak and body legibility tests.

use super::*;

fn tall_glyph(g: Geom, row: u16, col: u16) -> [(Pixels, Pixels); 3] {
    let x0 = i32::from(g.origin_x) + i32::from(col) * g.cw as i32;
    let y0 = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    let ch = g.ch as i32;
    let bands = [
        (y0 + 1, y0 + ch * 3 / 10),
        (y0 + ch * 3 / 10, y0 + ch * 7 / 10),
        (y0 + ch * 7 / 10, y0 + ch - 1),
    ];
    bands.map(|(top, bot)| {
        let (mut ink, mut counters) = (Vec::new(), Vec::new());
        for y in top..bot {
            for x in x0 + 1..x0 + g.cw as i32 - 1 {
                if matches!(x - x0, 2 | 3 | 6 | 7 | 10 | 11) {
                    ink.push((y, x));
                } else {
                    counters.push((y, x));
                }
            }
        }
        (ink, counters)
    })
}

fn light_frame(
    g: Geom,
    c: &GlowConfig,
    rows: [u16; 3],
    cols: std::ops::Range<u16>,
    (under, out, halos): (&[GlowQuad], &[GlowQuad], &[RainHalo]),
) -> Glass {
    let mut frame = Glass::new(g, c.theme_bg);
    frame.add_quads(under);
    for row in rows {
        for col in cols.clone() {
            for (ink, _) in tall_glyph(g, row, col) {
                for (y, x) in ink {
                    frame.px[y as usize * frame.w + x as usize] = c.theme_fg;
                }
            }
        }
    }
    frame.add_quads(out);
    frame.add_halos(halos);
    frame
}

const LIGHT_BODY_STYLES: [GlowStyle; 6] = [
    GlowStyle::Comet,
    GlowStyle::Beam,
    GlowStyle::Laser,
    GlowStyle::Phaser,
    GlowStyle::Sparkle,
    GlowStyle::Lumen,
];

fn light_body_burst(
    style: GlowStyle,
    (fg, bg): (u32, u32),
    unsolved: bool,
    failures: &mut Vec<String>,
    drew: &mut bool,
) -> f32 {
    let g = tall_geom();
    let (row, start) = (3u16, 4u16);
    let mut c = cfg(style, true);
    c.dark_theme = false;
    c.theme_fg = fg;
    c.theme_bg = bg;
    c.intensity = 1.0;
    let mut glow = CursorGlow {
        light_body_unsolved: unsolved,
        ..CursorGlow::default()
    };
    let mut out = Vec::new();
    let mut worst = f32::INFINITY;
    let theme = format!("{style:?} on #{bg:06x}");
    let probe = |glow: &mut CursorGlow, caret: u16, at: Instant| {
        let mut chars = [' '; 48];
        chars[usize::from(start)..usize::from(caret)].fill('x');
        glow.observe_row(row, caret, &chars, at);
    };
    let mut judge = |glow: &mut CursorGlow, out: &[GlowQuad], at: Instant, caret: u16| {
        let under = glow.under_quads().to_vec();
        let halos = glow.halos().to_vec();
        if let Some(q) = out.iter().chain(&under).find(|q| q.alpha == 0) {
            failures.push(format!(
                "{theme}: an additive quad on a light ground: {q:?}"
            ));
        }
        if let Some(h) = halos.iter().find(|h| h.mode != HaloMode::Over) {
            failures.push(format!(
                "{theme}: an additive halo on a light ground: {h:?}"
            ));
        }
        if let Some(q) = under
            .iter()
            .find(|q| f32::from(q.alpha.max(q.alpha2)) > LIGHT_BODY_ALPHA_CAP)
            .filter(|_| !unsolved)
        {
            failures.push(format!(
                "{theme}: an under-ink quad past the body cap: {q:?}"
            ));
        }
        let (mut b_out, mut b_halos, mut b_under) = (Vec::new(), Vec::new(), Vec::new());
        let cur = Some((row, caret));
        glow.emit_comet(
            at,
            &c,
            g,
            cur,
            &mut crate::cursor_glow::glow_sink(
                &mut b_out,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
            &mut b_halos,
            &mut crate::cursor_glow::glow_sink(
                &mut b_under,
                &mut aterm_render::GlowRamps::default(),
                true,
            ),
        );
        glow.emit_crown(at, &c, g, cur, &mut b_halos, &mut b_under);
        *drew |= !b_under.is_empty();
        let typed = start..caret;
        let rows = [row - 1, row, row + 1];
        let bare = light_frame(g, &c, rows, typed.clone(), (&[], &[], &[]));
        let body = light_frame(g, &c, rows, typed.clone(), (&b_under, &b_out, &b_halos));
        let row_top = i32::from(row) * g.ch as i32;
        'rows: for y in 0..i32::from(g.win_h) {
            if (row_top..row_top + g.ch as i32).contains(&y) {
                continue;
            }
            for x in 0..i32::from(g.win_w) {
                if body.at(y, x) != bare.at(y, x) {
                    failures.push(format!(
                        "{theme}: the body changed pixel ({x}, {y}) outside the typed row"
                    ));
                    break 'rows;
                }
            }
        }
        let lit = light_frame(g, &c, rows, typed.clone(), (&under, out, &halos));
        for col in typed {
            for (band, (ink, counters)) in tall_glyph(g, row, col).iter().enumerate() {
                let r = ratio(&lit, ink, counters);
                worst = worst.min(r);
                if r < BODY_CONTRAST_BAR {
                    failures.push(format!("{theme}: col {col} band {band} at {r:.2}:1"));
                }
            }
        }
    };
    let mut t = Instant::now();
    probe(&mut glow, start, t);
    glow.tick(Some((row, start)), t, &c, g, &mut out);
    let mut caret = start;
    for k in 1..=20u16 {
        t += Duration::from_millis(57);
        caret = start + k;
        glow.note_synthetic_typed(t, 1);
        probe(&mut glow, caret, t);
        glow.tick(Some((row, caret)), t, &c, g, &mut out);
        if k == 12 || k == 20 {
            judge(&mut glow, &out, t, caret);
        }
    }
    t += Duration::from_millis(120);
    glow.tick(Some((row, caret)), t, &c, g, &mut out);
    judge(&mut glow, &out, t, caret);
    for _ in 0..4 {
        t += Duration::from_millis(57);
        arm_exact_backspace(&mut glow, t, (row, caret), (row, caret - 1));
        caret -= 1;
        glow.tick(Some((row, caret)), t, &c, g, &mut out);
    }
    for _ in 0..4 {
        t += Duration::from_millis(57);
        caret += 1;
        glow.note_synthetic_typed(t, 1);
        probe(&mut glow, caret, t);
        glow.tick(Some((row, caret)), t, &c, g, &mut out);
    }
    judge(&mut glow, &out, t, caret);
    worst
}

#[test]
fn light_bodies_stay_in_the_typed_row_and_keep_its_letters_legible() {
    let mut failures = Vec::new();
    let mut unsolved_worst = f32::INFINITY;
    for theme in [(0x001F_2328, 0x00FF_FFFF), (0x002B_2D33, 0x00F6_F7F9)] {
        for style in LIGHT_BODY_STYLES {
            let mut drew = false;
            light_body_burst(style, theme, false, &mut failures, &mut drew);
            if !drew {
                failures.push(format!("{style:?} on #{:06x}: no body drawn", theme.1));
            }
            let control = light_body_burst(style, theme, true, &mut Vec::new(), &mut false);
            unsolved_worst = unsolved_worst.min(control);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        unsolved_worst < BODY_CONTRAST_BAR,
        "control: an unsolved body leaves every stroke at {unsolved_worst:.2}:1 or better"
    );
}
