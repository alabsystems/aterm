// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor-shape regression tests for the CPU renderer.

use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{Frame, Renderer, Theme, for_each_cursor_rect};

const CURSOR: u32 = 0x0050_FA7B; // Theme::default().cursor
const FG: u32 = 0x00D0_D0D0; // Theme::default().fg

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

fn cursor_positions(f: &Frame) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for y in 0..f.height {
        for x in 0..f.width {
            if f.pixels[y * f.width + x] == CURSOR {
                out.push((x, y));
            }
        }
    }
    out
}

fn cursor_rects(style: CursorStyle, cw: usize, ch: usize) -> Vec<[usize; 4]> {
    let mut rects = Vec::new();
    for_each_cursor_rect(style, 0, 0, cw, ch, |rect| rects.push(rect));
    rects
}

fn term_with(bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(2, 4);
    t.process(bytes);
    t
}

#[test]
fn steady_cursor_styles_fill_exactly_their_rect() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let (ut, bt) = ((ch / 8).max(2), (cw / 8).max(2));
    let cases: [(&str, &[u8], [usize; 4]); 3] = [
        ("block", b"\x1b[2 q", [0, 0, cw, ch]),
        ("underline", b"\x1b[4 q", [0, ch - ut, cw, ch]),
        ("bar", b"\x1b[6 q", [0, 0, bt, ch]),
    ];
    for (style, bytes, [x0, y0, x1, y1]) in cases {
        let mut term = term_with(bytes);
        let pos = cursor_positions(&r.render_input(&term.cell_frame(2, 4)));
        assert_eq!(
            pos.len(),
            (x1 - x0) * (y1 - y0),
            "{style}: the rect must be filled exactly"
        );
        assert!(
            pos.iter()
                .all(|&(x, y)| (x0..x1).contains(&x) && (y0..y1).contains(&y)),
            "{style}: cursor pixels outside its rect"
        );
    }
}

#[test]
fn fill_override_recolors_a_steady_bar_without_theme_flash() {
    let mut r = renderer();
    const OVERRIDE: u32 = 0x00FE_017F;
    let (cw, ch) = r.cell_size();
    let mut term = term_with(b"\x1b[6 q"); // DECSCUSR 6 = steady bar
    let mut input = term.cell_frame(2, 4);
    input.cursor_fill_override = Some(OVERRIDE);
    let frame = r.render_input(&input);
    let rects = cursor_rects(CursorStyle::SteadyBar, cw, ch);
    let area: usize = rects.iter().map(|&[_, _, w, h]| w * h).sum();
    assert_eq!(
        frame
            .pixels
            .iter()
            .filter(|&&pixel| pixel == OVERRIDE)
            .count(),
        area,
        "every steady-bar pixel must use the host override"
    );
    assert!(
        cursor_positions(&frame).is_empty(),
        "the theme cursor colour must not flash through a non-block override"
    );
    for [x, y, w, h] in rects {
        for py in y..y + h {
            for px in x..x + w {
                assert_eq!(
                    frame.pixels[py * frame.width + px],
                    OVERRIDE,
                    "steady-bar override missing at ({px},{py})"
                );
            }
        }
    }
}

#[test]
fn hollow_block_draws_outline_but_not_center() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    r.set_cursor_style_override(Some(CursorStyle::HollowBlock));
    let mut term = term_with(b"");
    let f = r.render_input(&term.cell_frame(2, 4));
    let t = (ch / 16).max(1);
    let border = 2 * cw * t + 2 * t * (ch - 2 * t);
    let pos = cursor_positions(&f);
    assert_eq!(
        pos.len(),
        border,
        "hollow block should paint exactly the outline"
    );
    for &(x, y) in &[(0, 0), (cw - 1, 0), (0, ch - 1), (cw - 1, ch - 1)] {
        assert_eq!(
            f.pixels[y * f.width + x],
            CURSOR,
            "corner ({x},{y}) should be outlined"
        );
    }
    let (mx, my) = (cw / 2, ch / 2);
    assert_ne!(
        f.pixels[my * f.width + mx],
        CURSOR,
        "hollow center must stay unfilled"
    );

    r.set_cursor_style_override(None);
    let f2 = r.render_input(&term.cell_frame(2, 4));
    assert_eq!(
        cursor_positions(&f2).len(),
        cw * ch,
        "override cleared -> block again"
    );
}

#[test]
fn composed_effect_shape_preserves_decscusr_and_backend_override_stays_highest() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let mut term = term_with(b"\x1b[4 q"); // terminal owns steady underline
    let mut input = term.cell_frame(2, 4);
    assert_eq!(input.cursor_style, CursorStyle::SteadyUnderline);

    input.cursor_effect_style_override = Some(CursorStyle::Bolt);
    let effect = r.render_input(&input);
    let bolt_area: usize = cursor_rects(CursorStyle::Bolt, cw, ch)
        .iter()
        .map(|&[_, _, w, h]| w * h)
        .sum();
    assert_eq!(cursor_positions(&effect).len(), bolt_area);
    assert_eq!(input.cursor_style, CursorStyle::SteadyUnderline);

    r.set_cursor_style_override(Some(CursorStyle::HollowBlock));
    let unfocused = r.render_input(&input);
    let t = (ch / 16).max(1);
    let hollow_area = 2 * cw * t + 2 * t * (ch - 2 * t);
    assert_eq!(cursor_positions(&unfocused).len(), hollow_area);

    r.set_cursor_style_override(None);
    input.clear_overlays();
    assert_eq!(input.cursor_effect_style_override, None);
    assert_eq!(input.cursor_style, CursorStyle::SteadyUnderline);
    let plain = r.render_input(&input);
    let underline_h = (ch / 8).max(2);
    assert_eq!(cursor_positions(&plain).len(), cw * underline_h);
    assert!(
        cursor_positions(&plain)
            .iter()
            .all(|&(x, y)| x < cw && y >= ch - underline_h && y < ch)
    );
}

#[test]
fn blink_phase_off_suppresses_blinking_styles_only() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();

    let mut term = term_with(b"\x1b[1 q"); // DECSCUSR 1 = blinking block
    r.set_cursor_blink_phase(false);
    let off = r.render_input(&term.cell_frame(2, 4));
    assert!(
        cursor_positions(&off).is_empty(),
        "blink phase off -> no cursor pixels"
    );
    r.set_cursor_blink_phase(true);
    let on = r.render_input(&term.cell_frame(2, 4));
    assert_eq!(
        cursor_positions(&on).len(),
        cw * ch,
        "blink phase on -> full block again"
    );

    let mut steady = term_with(b"\x1b[2 q");
    r.set_cursor_blink_phase(false);
    let f = r.render_input(&steady.cell_frame(2, 4));
    assert_eq!(
        cursor_positions(&f).len(),
        cw * ch,
        "steady block must ignore the blink phase"
    );

    for (bytes, label) in [(&b"\x1b[3 q"[..], "underline"), (&b"\x1b[5 q"[..], "bar")] {
        let mut t = term_with(bytes);
        r.set_cursor_blink_phase(false);
        assert!(
            cursor_positions(&r.render_input(&t.cell_frame(2, 4))).is_empty(),
            "blinking {label}: phase off -> no cursor"
        );
        r.set_cursor_blink_phase(true);
        assert!(
            !cursor_positions(&r.render_input(&t.cell_frame(2, 4))).is_empty(),
            "blinking {label}: phase on -> cursor drawn"
        );
    }
}

#[test]
fn hidden_cursor_draws_nothing() {
    let mut r = renderer();
    let mut term = term_with(b"\x1b[?25l");
    let f = r.render_input(&term.cell_frame(2, 4));
    assert!(
        cursor_positions(&f).is_empty(),
        "DECTCEM off -> no cursor pixels"
    );
    let mut term = term_with(b"\x1b[?25l\x1b[?25h");
    let f = r.render_input(&term.cell_frame(2, 4));
    assert!(
        !cursor_positions(&f).is_empty(),
        "DECTCEM on -> cursor drawn again"
    );
}

#[test]
fn underline_and_bar_keep_glyph_in_normal_colors() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let strip = (ch / 8).max(2);
    let mut term = term_with(b"a\x1b[1;1H\x1b[4 q");
    let f = r.render_input(&term.cell_frame(2, 4));
    let near_fg = |p: u32| {
        let d = |a: u32, b: u32| ((a as i32) - (b as i32)).abs();
        d(p >> 16 & 0xff, FG >> 16 & 0xff) < 0x40
            && d(p >> 8 & 0xff, FG >> 8 & 0xff) < 0x40
            && d(p & 0xff, FG & 0xff) < 0x40
    };
    let glyph_above_strip = (0..ch - strip)
        .flat_map(|y| (0..cw).map(move |x| (x, y)))
        .any(|(x, y)| near_fg(f.pixels[y * f.width + x]));
    assert!(
        glyph_above_strip,
        "underline cursor must leave the glyph in its own fg"
    );
    for y in ch - strip..ch {
        for x in 0..cw {
            assert_eq!(
                f.pixels[y * f.width + x],
                CURSOR,
                "strip pixel ({x},{y}) overwritten"
            );
        }
    }
}

#[test]
fn bolt_cursor_paints_the_lightning_silhouette() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    r.set_cursor_style_override(Some(CursorStyle::Bolt));
    let mut term = term_with(b"");
    let f = r.render_input(&term.cell_frame(2, 4));

    let rects = cursor_rects(CursorStyle::Bolt, cw, ch);
    assert_eq!(rects.len(), ch, "bolt: one strip per pixel row");
    for (y, &[x, ry, w, h]) in rects.iter().enumerate() {
        assert_eq!((ry, h), (y, 1), "bolt: strip {y} must be its own row");
        assert!(w >= 1 && x + w <= cw, "bolt: strip {y} escapes the cell");
    }
    let jumps: Vec<usize> = rects
        .windows(2)
        .filter(|p| p[1][0] > p[0][0])
        .map(|p| p[1][0] - p[0][0])
        .collect();
    assert_eq!(
        jumps.len(),
        1,
        "bolt: exactly one waist step, got {jumps:?}"
    );
    assert!(
        jumps[0] >= (cw / 8).max(1),
        "bolt: the waist step must jut visibly right"
    );
    assert!(
        rects[ch - 1][2] < rects[0][2],
        "bolt: the tail must taper below the top stroke width"
    );
    assert!(
        rects[ch - 1][0] < rects[0][0],
        "bolt: the tip must land left of the top stroke"
    );

    let area: usize = rects.iter().map(|&[_, _, w, h]| w * h).sum();
    let pos = cursor_positions(&f);
    assert_eq!(pos.len(), area, "bolt: paint exactly the strips");
    for &(x, y) in &pos {
        let [sx, _, sw, _] = rects[y];
        assert!(
            x >= sx && x < sx + sw,
            "bolt: pixel ({x},{y}) outside its row strip"
        );
    }
}

#[test]
fn bolt_cursor_honours_the_fill_override_and_keeps_the_glyph() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    const LASER: u32 = 0x00FF_E01A;
    r.set_cursor_style_override(Some(CursorStyle::Bolt));
    let mut term = term_with(b"a\x1b[1;1H");
    let mut input = term.cell_frame(2, 4);
    input.cursor_fill_override = Some(LASER);
    let f = r.render_input(&input);
    let rects = cursor_rects(CursorStyle::Bolt, cw, ch);
    let area: usize = rects.iter().map(|&[_, _, w, h]| w * h).sum();
    let laser_px = f.pixels.iter().filter(|&&p| p == LASER).count();
    assert_eq!(
        laser_px, area,
        "bolt: every strip pixel takes the laser hue"
    );
    assert!(
        cursor_positions(&f).is_empty(),
        "bolt: nothing left in the theme cursor colour"
    );
    let near_fg = |p: u32| {
        let d = |a: u32, b: u32| ((a as i32) - (b as i32)).abs();
        d(p >> 16 & 0xff, FG >> 16 & 0xff) < 0x40
            && d(p >> 8 & 0xff, FG >> 8 & 0xff) < 0x40
            && d(p & 0xff, FG & 0xff) < 0x40
    };
    let glyph_ink = (0..ch)
        .flat_map(|y| (0..cw).map(move |x| (x, y)))
        .any(|(x, y)| near_fg(f.pixels[y * f.width + x]));
    assert!(glyph_ink, "bolt: the glyph must stay drawn (no cut-out)");
}
