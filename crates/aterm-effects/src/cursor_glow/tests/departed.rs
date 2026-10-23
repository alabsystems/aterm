// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Departed-cell glow tests when pressing Enter at the bottom of a full pane.

use super::*;
use aterm_core::terminal::RenderCell;

/// [`enter_at_the_bottom`]'s answer.
type Entered = (
    CursorGlow,
    Vec<GlowQuad>,
    Vec<Vec<RenderCell>>,
    (u16, u16),
    GlowConfig,
);

/// A hot run typed on the last row of a `tall_geom` pane, then Enter.
fn enter_at_the_bottom(style: GlowStyle, dark: bool, after_ms: u64) -> Entered {
    let g = tall_geom();
    let bottom = g.rows as u16 - 1;
    let (start, keys) = (2u16, 22u16);
    let mut c = cfg(style, true);
    c.intensity = 1.0;
    c.dark_theme = dark;
    (c.theme_fg, c.theme_bg) = if dark {
        (0x00D0_D0D0, 0x0011_1318)
    } else {
        (0x001F_2328, 0x00FF_FFFF)
    };
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let frame = |lines: &[(u16, &[char])]| {
        let mut rows = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(c.theme_fg), rgb(c.theme_bg));
            }
        }
        for &(row, text) in lines {
            for (col, &ch) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = ch;
            }
        }
        rows
    };
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let mut t = Instant::now();
    for k in 0..=keys {
        if k > 0 {
            t += Duration::from_millis(60);
            text[usize::from(start + k - 1)] = 'm';
            glow.note_synthetic_typed(t, 1);
        }
        let caret = start + k;
        glow.observe_row(bottom, caret, &text, t);
        glow.observe_frame_text(Some((bottom, caret)), &frame(&[(bottom, &text)]), t);
        glow.tick(Some((bottom, caret)), t, &c, g, &mut out);
    }
    t += Duration::from_millis(30);
    let mut output = vec![' '; g.cols];
    output[..19].fill('m');
    let mut prompt = vec![' '; g.cols];
    prompt[0] = '$';
    glow.note_scroll(2);
    let rows = frame(&[
        (bottom - 2, &text),
        (bottom - 1, &output),
        (bottom, &prompt),
    ]);
    glow.note_synthetic_move(t);
    let landing = (bottom, 2u16);
    let mut frames = vec![t];
    let mut k = 16;
    while k < after_ms {
        frames.push(t + Duration::from_millis(k));
        k += 16;
    }
    frames.push(t + Duration::from_millis(after_ms));
    for at in frames {
        glow.observe_row(landing.0, landing.1, &prompt, at);
        glow.observe_frame_text(Some(landing), &rows, at);
        glow.tick(Some(landing), at, &c, g, &mut out);
    }
    (glow, out, rows, (bottom - 2, start + keys), c)
}

/// The light the frame lays in cell `(row, col)`.
fn light_in_cell(glow: &CursorGlow, out: &[GlowQuad], (row, col): (u16, u16)) -> (u64, usize) {
    let g = tall_geom();
    let (x0, y0) = (i32::from(col) * g.cw as i32, i32::from(row) * g.ch as i32);
    let (x1, y1) = (x0 + g.cw as i32, y0 + g.ch as i32);
    let hit = |x: u16, y: u16, w: u16, h: u16| {
        let (x, y) = (i32::from(x), i32::from(y));
        x < x1 && x + i32::from(w) > x0 && y < y1 && y + i32::from(h) > y0
    };
    let lum = |qs: &[GlowQuad]| -> u64 {
        qs.iter()
            .filter(|q| hit(q.x, q.y, q.w, q.h))
            .map(|q| {
                u64::from(q.w)
                    * u64::from(q.h)
                    * u64::from(
                        ((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff),
                    )
            })
            .sum()
    };
    let halos = glow
        .halos()
        .iter()
        .filter(|h| hit(h.x, h.y, h.w, h.h))
        .count();
    (lum(out) + lum(glow.under_quads()), halos)
}

/// THE CELL A CARET LEAVES KEEPS NO MARK: an Enter at the bottom of a full
/// pane carries the typed command up two rows and the caret leaps home. The
/// cell the caret stood on after the command's last letter is the caret's,
/// not the trail's: by 160 ms after the Enter neither Beam, nor Comet, nor
/// Laser lays any light there (a clipped white half-disc and a lavender bar
/// after `fox`, a white dash, a blocky mosaic), and Beam and Comet none in
/// the first frame either. Negative control: before the Enter, with the
/// caret on that cell, each style's head lights it.
#[test]
fn the_cell_a_caret_leaves_keeps_no_mark() {
    for style in [GlowStyle::Beam, GlowStyle::Comet, GlowStyle::Laser] {
        for (ms, strict) in [(40u64, !matches!(style, GlowStyle::Laser)), (160, true)] {
            if !strict {
                continue;
            }
            let (glow, out, _rows, cell, _c) = enter_at_the_bottom(style, true, ms);
            let (lum, halos) = light_in_cell(&glow, &out, cell);
            assert!(
                lum == 0 && halos == 0,
                "{style:?} +{ms} ms: the cell the caret left holds light ({lum}, {halos} halos)"
            );
        }
        let (glow, out, cell) = typed_at_the_bottom(style);
        let (lum, halos) = light_in_cell(&glow, &out, cell);
        assert!(
            lum > 0 || halos > 0,
            "control: {style:?}'s head lights the caret's cell"
        );
    }
}

/// [`enter_at_the_bottom`]'s run up to the last key.
fn typed_at_the_bottom(style: GlowStyle) -> (CursorGlow, Vec<GlowQuad>, (u16, u16)) {
    let g = tall_geom();
    let bottom = g.rows as u16 - 1;
    let (start, keys) = (2u16, 22u16);
    let mut c = cfg(style, true);
    c.intensity = 1.0;
    (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, 0x0011_1318);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let mut t = Instant::now();
    for k in 0..=keys {
        if k > 0 {
            t += Duration::from_millis(60);
            text[usize::from(start + k - 1)] = 'm';
            glow.note_synthetic_typed(t, 1);
        }
        let caret = start + k;
        glow.observe_row(bottom, caret, &text, t);
        glow.tick(Some((bottom, caret)), t, &c, g, &mut out);
    }
    (glow, out, (bottom, start + keys))
}

/// ON PAPER A LASER'S LEAP GOES WITH ITS STRIKE: an Enter at the bottom of a
/// full pane, the laser on GitHub Light: 160 ms after it (past the strike's
/// one flash) no ink lies under the command's output, which only the leap
/// crosses, where the fired beam once stood through its letters as a
/// struck-through rule.
#[test]
fn the_light_laser_leap_is_gone_with_its_strike() {
    let g = tall_geom();
    let bottom = g.rows as u16 - 1;
    let ink_over = |glow: &CursorGlow, rows: std::ops::Range<u16>| -> u64 {
        glow.under_quads()
            .iter()
            .filter(|q| {
                let row = (u32::from(q.y) / g.ch as u32) as u16;
                rows.contains(&row) && (u32::from(q.x) / g.cw as u32) < 24
            })
            .map(|q| u64::from(q.alpha.max(q.alpha2)) * u64::from(q.w) * u64::from(q.h))
            .sum()
    };
    let (glow, _out, _rows, _cell, _c) = enter_at_the_bottom(GlowStyle::Laser, false, 160);
    let left = ink_over(&glow, bottom - 1..bottom);
    assert_eq!(
        left, 0,
        "the leap's beam still stands under the printed lines"
    );
    let mut c = cfg(GlowStyle::Laser, true);
    c.intensity = 1.0;
    c.dark_theme = false;
    (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((bottom, 2)), t, &c, g, &mut out);
    for col in 3..=24u16 {
        t += Duration::from_millis(60);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((bottom, col)), t, &c, g, &mut out);
    }
    assert!(
        ink_over(&glow, bottom..bottom + 1) > 0,
        "control: the typed laser lays its ink under its letters"
    );
}

/// The laser's Enter at the bottom of a full pane, at a host's geometry.
fn laser_enter_on_a_host_pane(unheld: bool, after_ms: u64) -> (CursorGlow, Geom, (f32, f32)) {
    let g = Geom {
        cw: 14,
        ch: 26,
        rows: 14,
        cols: 72,
        origin_x: 12,
        origin_y: 2,
        win_w: 1032,
        win_h: 378,
        head: 0,
    };
    let bottom = g.rows as u16 - 1;
    let (start, keys) = (2u16, 24u16);
    let mut c = cfg(GlowStyle::Laser, true);
    c.intensity = 1.0;
    c.color = LASER_DEFAULT_COLOR;
    (c.theme_fg, c.theme_bg) = (0x00D0_D0D0, 0x0011_1318);
    let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    let frame = |lines: &[(u16, &[char])]| {
        let mut rows = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for line in &mut rows {
            for cell in line.iter_mut() {
                (cell.fg, cell.bg) = (rgb(c.theme_fg), rgb(c.theme_bg));
            }
        }
        for &(row, text) in lines {
            for (col, &ch) in text.iter().enumerate() {
                rows[usize::from(row)][col].ch = ch;
            }
        }
        rows
    };
    let mut glow = CursorGlow {
        laser_aura_unheld: unheld,
        ..CursorGlow::default()
    };
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    text[0] = '$';
    let mut t = Instant::now();
    for k in 0..=keys {
        if k > 0 {
            t += Duration::from_millis(60);
            text[usize::from(start + k - 1)] = 'm';
            glow.note_synthetic_typed(t, 1);
        }
        let caret = start + k;
        glow.observe_row(bottom, caret, &text, t);
        glow.observe_frame_text(Some((bottom, caret)), &frame(&[(bottom, &text)]), t);
        glow.tick(Some((bottom, caret)), t, &c, g, &mut out);
    }
    t += Duration::from_millis(57);
    let mut output = vec![' '; g.cols];
    output[..19].fill('m');
    let mut prompt = vec![' '; g.cols];
    prompt[0] = '$';
    glow.note_scroll(2);
    let rows = frame(&[
        (bottom - 2, &text),
        (bottom - 1, &output),
        (bottom, &prompt),
    ]);
    glow.note_synthetic_move(t);
    let landing = (bottom, 2u16);
    let mut k = 0;
    while k <= after_ms {
        let at = t + Duration::from_millis(k);
        glow.observe_row(landing.0, landing.1, &prompt, at);
        glow.observe_frame_text(Some(landing), &rows, at);
        glow.tick(Some(landing), at, &c, g, &mut out);
        k += 16;
    }
    let caret = g.cell_center(landing.0, landing.1);
    (glow, g, caret)
}

/// A LASER'S AURA IS ITS HEAD'S: after an Enter at the bottom of a full
/// pane, at 32 and 128 ms, every halo of the frame stands within
/// [`CursorGlow::LASER_AURA_REACH_CELLS`] of the caret (the landing ring's
/// bloom, fitted a cell inward at the edge, included), and none reaches past
/// the grid's bottom edge, where it would be cut square. The aura laid on
/// every air sample of the leap stood as a fat white-lilac slab along the
/// prompt row, cut at the grid's bottom, and as a detached grey-violet block
/// on the blank after the printed output. Negative control: the unheld aura
/// lays halos farther out (on that blank and along the prompt) and past the
/// bottom edge.
#[test]
fn the_laser_aura_stays_on_its_head() {
    let far = |glow: &CursorGlow, g: Geom, (hx, hy): (f32, f32)| -> (usize, usize) {
        let reach = CursorGlow::LASER_AURA_REACH_CELLS;
        let mut far = 0;
        let mut cut = 0;
        for h in glow.halos() {
            let d =
                ((f32::from(h.cx) - hx) / g.cw as f32).hypot((f32::from(h.cy) - hy) / g.ch as f32);
            if d > reach {
                far += 1;
            }
            if i32::from(h.cy) + i32::from(h.ry) > g.fx_bot() + 1 {
                cut += 1;
            }
        }
        (far, cut)
    };
    for ms in [32u64, 128] {
        let (glow, g, caret) = laser_enter_on_a_host_pane(false, ms);
        assert!(
            !glow.halos().is_empty(),
            "+{ms} ms: the head shows its light"
        );
        let (far_n, cut) = far(&glow, g, caret);
        assert_eq!(
            far_n, 0,
            "+{ms} ms: {far_n} halo bands stand off the laser's head"
        );
        assert_eq!(
            cut, 0,
            "+{ms} ms: {cut} halo bands reach past the grid's bottom edge"
        );
        let (glow, g, caret) = laser_enter_on_a_host_pane(true, ms);
        let (far_n, cut) = far(&glow, g, caret);
        assert!(
            far_n > 0 && cut > 0,
            "control +{ms} ms: the unheld aura stands off the head ({far_n}) and past the edge ({cut})"
        );
    }
}
