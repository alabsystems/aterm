// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The beam family's streak over text on light grounds: the under-ink bodies.
//! (The dark ground's bed and over-ink account are `streak_bed`'s.)

use super::*;

/// A full-height glyph at `(row, col)` for the light bodies: three 2 px stems
/// from the ascender line to the descender line, and the counters beside
/// them, split into the top edge band, the x-height and the bottom edge band.
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

/// A light frame composited in the renderer's order: the page, the under-ink
/// stream `under`, full-height glyphs on the typed row and the rows above and
/// below it (columns `cols`), then the over-ink quads `out` and the `halos`.
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

/// The light-ground styles the body test judges: the beam family and Lumen,
/// whose glow is its body.
const LIGHT_BODY_STYLES: [GlowStyle; 6] = [
    GlowStyle::Comet,
    GlowStyle::Beam,
    GlowStyle::Laser,
    GlowStyle::Phaser,
    GlowStyle::Sparkle,
    GlowStyle::Lumen,
];

/// One style's typed burst on one light theme (`fg` on `bg`): 20 keys at the
/// gallery's cadence, judged at the 12th and 20th key and 120 ms later, then
/// four backspaces and four keys retyped, judged at the last. Each judge
/// point pushes what breaks the light law into `failures` and returns the
/// worst stroke ratio; `drew` records that the body drew.
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
    // The host probes the typed row every frame, so a grain keeps off it.
    let probe = |glow: &mut CursorGlow, caret: u16, at: Instant| {
        let mut chars = [' '; 48];
        chars[usize::from(start)..usize::from(caret)].fill('x');
        glow.observe_row(row, caret, &chars, at);
    };
    let mut judge = |glow: &mut CursorGlow, out: &[GlowQuad], at: Instant, caret: u16| {
        // The full tick's streams: nothing additive, nothing under the ink
        // past the body cap.
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
        // The body alone (the streak, and Lumen's glow): inside the typed row.
        let (mut b_out, mut b_halos, mut b_under) = (Vec::new(), Vec::new(), Vec::new());
        let cur = Some((row, caret));
        glow.emit_comet(at, &c, g, cur, &mut b_out, &mut b_halos, &mut b_under);
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
        // Legibility over the whole frame, crackles and grains included.
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
    // A revisit: four backspaces and the four keys retyped over the same
    // cells, whose bodies must not stack.
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

/// Light grounds: the beam family's body (and Lumen's glow) is ink under the
/// letters, held inside the typed row. Over a typed burst and a
/// backspace-and-retype pass, on GitHub Light (`#1f2328` on `#ffffff`) and
/// Default Light (`#2b2d33` on `#f6f7f9`) at full intensity, a full tick lays
/// no additive quad or halo and nothing under the ink past
/// [`LIGHT_BODY_ALPHA_CAP`]; every stroke of the typed letters keeps
/// [`BODY_CONTRAST_BAR`] against its counter with the frame composited in the
/// renderer's order (page, the under-ink stream, the glyphs, the over-ink
/// quads, the halos), Laser's crackles included; and the body alone leaves
/// every pixel outside the typed row as the bare frame has it. Negative
/// control: with the bodies laid in their unsolved vivid ink
/// (`light_body_unsolved`), a stroke drops under the bar, so the probe sees
/// the solve.
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
