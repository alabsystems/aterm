// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_render::{
    DecoBlend, DecoGlyph, DirtyDecision, Frame, Renderer, Theme, WindowCpu, WordDecoration,
    compute_dirty_rows,
};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// A settled lifted additive sparkle whose stamp spills into the neighbouring row.
fn lifted_add_deco(row: u16, col: u16, dy: i8) -> WordDecoration {
    WordDecoration {
        row,
        col,
        dx: 0,
        dy,
        glyph: DecoGlyph::Star4,
        blend: DecoBlend::Add,
        color: 0x0060_5030,
        alpha: 220,
    }
}

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

#[test]
fn paw_over_blend_paints_the_target_cell_only() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"i love cats");

    let base = rend.render_input(&term.cell_frame(3, 12));
    let base_target = cell_pixels(&base, cw, ch, 0, 7);
    let base_far = cell_pixels(&base, cw, ch, 2, 0);

    let mut input = term.cell_frame(3, 12);
    input.word_decorations.push(WordDecoration {
        row: 0,
        col: 7,
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Paw,
        blend: DecoBlend::Over,
        color: 0x00FF_00FF, // bright magenta — distinct from text/bg
        alpha: 255,
    });
    let f = rend.render_input(&input);
    let now_target = cell_pixels(&f, cw, ch, 0, 7);
    let now_far = cell_pixels(&f, cw, ch, 2, 0);

    assert_ne!(
        base_target, now_target,
        "the decorated cell must have changed pixels"
    );
    assert_eq!(
        base_far, now_far,
        "a cell far from the decoration must be untouched"
    );
    assert!(
        now_target.contains(&0x00FF_00FF),
        "Over-blend at alpha 255 must paint the stamp colour at the paw core"
    );
}

#[test]
fn add_blend_only_brightens() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"fuck this");

    let base = rend.render_input(&term.cell_frame(3, 12));
    let base_target = cell_pixels(&base, cw, ch, 0, 0);

    let mut input = term.cell_frame(3, 12);
    input.word_decorations.push(WordDecoration {
        row: 0,
        col: 0,
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Star4,
        blend: DecoBlend::Add,
        color: 0x0040_4040,
        alpha: 255,
    });
    let f = rend.render_input(&input);
    let now_target = cell_pixels(&f, cw, ch, 0, 0);

    for (b, n) in base_target.iter().zip(now_target.iter()) {
        for sh in [16, 8, 0] {
            let bc = (b >> sh) & 0xff;
            let nc = (n >> sh) & 0xff;
            assert!(nc >= bc, "additive blend darkened a channel");
        }
    }
    assert_ne!(base_target, now_target, "additive sparkle must brighten");
}

#[test]
fn settled_deco_dy_spill_marks_neighbour_of_unrelated_dirt_to_fixpoint() {
    const CELL_H: usize = 16;
    let marked = |dirty: &[bool]| -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &b)| b.then_some(r))
            .collect()
    };
    let mut term = Terminal::new(8, 12);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let mut dirty = Vec::new();

    let mut prev = term.cell_frame(8, 12);
    prev.word_decorations.push(lifted_add_deco(4, 9, -5));
    term.process(b"\x1b[5;1Hdirt");
    let mut cur = term.cell_frame(8, 12);
    cur.word_decorations.push(lifted_add_deco(4, 9, -5));
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.deco_changed,
        "a settled stream must not set deco_changed"
    );
    assert_eq!(
        marked(&dirty),
        vec![3, 4],
        "unrelated dirt on a lifted deco's row must mark the dy-spill neighbour"
    );

    let mut prev = term.cell_frame(8, 12);
    prev.word_decorations.push(lifted_add_deco(4, 9, 4));
    prev.word_decorations.push(lifted_add_deco(5, 2, 4));
    term.process(b"\x1b[5;1Hmore");
    let mut cur = term.cell_frame(8, 12);
    cur.word_decorations = prev.word_decorations.clone();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.deco_changed,
        "a settled stream must not set deco_changed"
    );
    assert_eq!(
        marked(&dirty),
        vec![4, 5, 6],
        "the spill closure must iterate: a newly marked neighbour that is \
         itself a lifted deco's row marks the next row"
    );

    let prev = cur.clone();
    let cur2 = cur.clone();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur2,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.is_gate_hit(),
        "a settled lifted deco with no dirt must still gate-hit"
    );
    assert!(
        dirty.iter().all(|&b| !b),
        "the spill closure must mark nothing on a clean frame"
    );
}

#[test]
fn settled_lifted_add_deco_with_dirt_on_its_row_stays_byte_stable() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    term.process(b"\x1b[4;1Hstatic spill row");

    let deco = lifted_add_deco(4, 16, -((ch / 3).min(120) as i8));
    assert!(deco.dy != 0, "the lift must be non-zero (spill premise)");
    let make = |term: &mut Terminal, n: usize| {
        term.process(format!("\x1b[5;1Hdirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.word_decorations.push(deco);
        input
    };

    let mut wc = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc, &make(&mut term, 0));
    for n in 1..=4usize {
        let input = make(&mut term, n);
        let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
        let fresh = rend.render_input(&input).pixels.clone();
        assert_eq!(
            cached, fresh,
            "frame {n}: cached repaint must be byte-stable — the settled \
             lifted Add deco's dy spill must land on a rebuilt neighbour row, \
             never re-Add over its own cached pixels"
        );
    }
}

/// `text` in `fg` on a page of `bg` (every cell of it, by an erase in the page's colour), its
/// frame's pixels and width.
fn on_page(rend: &mut Renderer, text: &str, fg: u32, bg: u32) -> (Vec<u32>, usize) {
    let (rows, cols) = (3u16, 12u16);
    let mut term = Terminal::new(rows, cols);
    let sgr = |c: u32| format!("{};{};{}", (c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
    term.process(
        format!(
            "\x1b[48;2;{}m\x1b[2J\x1b[2;3H\x1b[38;2;{}m{text}",
            sgr(bg),
            sgr(fg)
        )
        .as_bytes(),
    );
    let f = rend.render_input(&term.cell_frame(usize::from(rows), usize::from(cols)));
    (f.pixels.clone(), f.width)
}

/// A WORD'S GLOW KEEPS ITS LETTERS' READING (CT-51 Rule C;
/// `aterm_effects::cursor_glow::word_glow_floor`): Feline's self-glow on paper at its whole
/// amplitude lays `fox` in `#8a8f99` (3.3:1 on white) at no less than 97% of each letter's own
/// reading, and in `#727272` (4.8:1), `#757575` (4.61:1) and `#767676` (4.54:1) at 4.5 or more
/// or, where its raster reads its own colour under that, 97% of it, each letter read by its raster as
/// `trail-contrast` reads it. NEGATIVE CONTROL
/// (`aterm_effects::word_decorations::with_paper_glow_floor_old`): held to nine tenths of its
/// own colour's contrast, as before, the glow lowers a letter of each under that bar.
#[test]
fn a_word_s_glow_keeps_its_letters_reading() {
    use crate::trail_letters_read::Pair;
    use aterm_effects::word_decorations::{paper_glow_peak, with_paper_glow_floor_old};
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let bg = 0x00FF_FFFF;
    let mut worst = |fg: u32| {
        let peak = paper_glow_peak(fg, bg);
        let (clean, w) = on_page(&mut rend, "fox", fg, bg);
        let (frame, _) = on_page(&mut rend, "fox", peak, bg);
        let pair = Pair {
            frame: &frame,
            clean: &clean,
            w,
            cw,
            ch,
        };
        let mut lows = Vec::new();
        for (i, c) in "fox".chars().enumerate() {
            let own = pair.own(1, 2 + i, 1).expect("a letter");
            let local = pair.letter(1, 2 + i, 1, c).map_or(own, |l| l.local);
            lows.push((c, local, own));
        }
        (peak, lows)
    };
    // Not lowered, as `trail-contrast` judges it: at the bar, or under it where its own reading
    // is and it keeps 97% of that.
    let keeps = |lows: &[(char, f64, f64)]| {
        lows.iter()
            .all(|&(_, local, own)| local >= 4.5 || (own < 4.5 && local >= 0.97 * own))
    };
    // The inks about the bar's edge (4.54 and 4.61 on white) too, whose raster reads just over
    // or under the bar.
    for fg in [0x008A_8F99, 0x0072_7272, 0x0076_7676, 0x0075_7575] {
        let (peak, lows) = worst(fg);
        assert_ne!(peak, fg, "{fg:06x}: the glow lights the word");
        assert!(keeps(&lows), "{fg:06x} glowing {peak:06x}: {lows:?}");
        let (peak, lows) = with_paper_glow_floor_old(|| worst(fg));
        assert!(
            !keeps(&lows),
            "control: {fg:06x} glowing {peak:06x} at nine tenths keeps every letter: {lows:?}"
        );
    }
}
