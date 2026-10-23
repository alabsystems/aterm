// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Peeking-cat sprite (`RenderInput.cat_quads` + `cat_atlas`) tests on the CPU renderer.

use std::sync::Arc;

use aterm_core::render::{SceneAtlas, SpriteQuad};
use aterm_core::terminal::Terminal;
use aterm_render::{
    DirtyDecision, Frame, Renderer, Theme, WindowCpu, compute_dirty_rows, rgb_to_u32,
};

pub(crate) fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
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

/// Deterministic opaque atlas with a distinct colour per texel.
pub(crate) fn patterned_atlas(w: u32, h: u32, version: u64) -> SceneAtlas {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                255,
            ]);
        }
    }
    SceneAtlas {
        width: w,
        height: h,
        rgba,
        version,
    }
}

fn checker_atlas(version: u64) -> SceneAtlas {
    #[rustfmt::skip]
    let rgba = vec![
        255, 255, 255, 255,   0, 0, 0, 255,
        0, 0, 0, 255,         255, 255, 255, 255,
    ];
    SceneAtlas {
        width: 2,
        height: 2,
        rgba,
        version,
    }
}

/// Opaque untinted quad: `dest = [x, y, w, h]`, `src = [ax, ay, aw, ah]`.
pub(crate) fn quad(row: u16, dest: [u16; 4], src: [u16; 4]) -> SpriteQuad {
    SpriteQuad {
        row,
        x: dest[0],
        y: dest[1],
        w: dest[2],
        h: dest[3],
        ax: src[0],
        ay: src[1],
        aw: src[2],
        ah: src[3],
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }
}

#[test]
fn glyph_draws_over_cat_sprite() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 8);
    term.process("\x1b[?25l█".as_bytes());

    let red_atlas = SceneAtlas {
        width: 8,
        height: 8,
        rgba: (0..8 * 8).flat_map(|_| [0xC8u8, 0x20, 0x20, 255]).collect(),
        version: 1,
    };
    let mut input = term.cell_frame(2, 8);
    input.cat_atlas = Some(Arc::new(red_atlas));
    input.cat_quads = vec![SpriteQuad {
        row: 0,
        x: 0,
        y: 0,
        w: (2 * cw) as u16,
        h: ch as u16,
        ax: 0,
        ay: 0,
        aw: 8,
        ah: 8,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }];

    let f = rend.render_input(&input);
    let red = 0x00C8_2020;
    let fg = rgb_to_u32(input.cells[0][0].fg);
    let block_cell = cell_pixels(&f, cw, ch, 0, 0);
    assert!(
        block_cell.iter().all(|&p| p == fg),
        "the full-block glyph must draw OVER the cat sprite (cell 0 is pure fg)"
    );
    let bare_cell = cell_pixels(&f, cw, ch, 0, 1);
    assert!(
        bare_cell.iter().all(|&p| p == red),
        "the uncovered sprite half must be the exact sprite colour (cell 1 pure red)"
    );
}

#[test]
fn cat_stamp_is_nearest_and_endpoint_exact_at_one_to_one() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25l");

    let (aw, ahh) = (24u16, (ch as u16).min(24));
    let atlas = patterned_atlas(32, 32, 7);
    let mut input = term.cell_frame(3, 12);
    input.cat_atlas = Some(Arc::new(patterned_atlas(32, 32, 7)));
    input.cat_quads = vec![quad(1, [4, ch as u16, aw, ahh], [3, 5, aw, ahh])];
    let f = rend.render_input(&input);
    for dy in 0..ahh as usize {
        for dx in 0..aw as usize {
            let i = (((5 + dy) * 32) + 3 + dx) * 4;
            let want = ((atlas.rgba[i] as u32) << 16)
                | ((atlas.rgba[i + 1] as u32) << 8)
                | atlas.rgba[i + 2] as u32;
            let got = f.pixels[(ch + dy) * f.width + 4 + dx];
            assert_eq!(
                got, want,
                "NEAREST 1:1 must land the exact atlas texel at ({dx},{dy})"
            );
        }
    }

    let mut input = term.cell_frame(3, 12);
    input.cat_atlas = Some(Arc::new(checker_atlas(9)));
    input.cat_quads = vec![quad(2, [0, (2 * ch) as u16, 16, ch as u16], [0, 0, 2, 2])];
    let f = rend.render_input(&input);
    for dy in 0..ch {
        for dx in 0..16usize {
            let p = f.pixels[(2 * ch + dy) * f.width + dx];
            assert!(
                p == 0x00FF_FFFF || p == 0,
                "the cat stamp must be NEAREST: no intermediate colour, got #{p:06X} at ({dx},{dy})"
            );
        }
    }
}

#[test]
fn damaged_path_no_ghosting_when_cat_moves() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let mut term = Terminal::new(4, 12);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(patterned_atlas(32, 32, 4));

    let mut make = |row: u16| {
        let mut input = term.cell_frame(4, 12);
        input.cat_atlas = Some(atlas.clone());
        input.cat_quads = vec![quad(
            row,
            [2, row * ch as u16, 24, (ch as u16).min(24)],
            [0, 0, 24, (ch as u16).min(24)],
        )];
        input
    };
    let in_a = make(1);
    let in_b = make(2);

    let mut wc = WindowCpu::new();
    let a_cached = rend.render_input_cached(&mut wc, &in_a).pixels().to_vec();
    let b_cached = rend.render_input_cached(&mut wc, &in_b).pixels().to_vec();
    assert_ne!(a_cached, b_cached, "the cat actually moved between frames");

    let b_fresh = rend.render_input(&in_b).pixels.clone();
    assert_eq!(
        b_cached, b_fresh,
        "damaged-path repaint after a cat move must equal a fresh full render \
         (no ghost at the vacated row, no missing stamp at the new row)"
    );
}

#[test]
fn cat_dirty_gate_settled_hits_changed_marks_prev_union_cur_rows() {
    let mut term = Terminal::new(5, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let atlas_v1 = Arc::new(patterned_atlas(16, 16, 1));
    let settled = vec![quad(1, [0, 20, 16, 16], [0, 0, 16, 16])];

    let mut prev = term.cell_frame(5, 8);
    let mut cur = term.cell_frame(5, 8);
    prev.cat_atlas = Some(atlas_v1.clone());
    prev.cat_quads = settled.clone();
    cur.cat_atlas = Some(atlas_v1.clone());
    cur.cat_quads = settled.clone();
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.cat_changed,
        "equal quads + same atlas must not set cat_changed"
    );
    assert!(
        d.is_gate_hit(),
        "settled (non-empty but equal) cat must gate-hit: steady state is free"
    );
    assert!(dirty.iter().all(|&b| !b), "settled cat must mark no rows");

    cur.cat_quads = vec![quad(3, [0, 60, 16, 16], [0, 0, 16, 16])];
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.cat_changed, "moved quads must set cat_changed");
    assert!(!d.is_gate_hit(), "changed cat must NOT gate-hit");
    let marked: Vec<usize> = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    assert_eq!(
        marked,
        vec![1, 3],
        "changed cat must mark exactly the prev∪cur cat rows"
    );

    cur.cat_quads = settled;
    cur.cat_atlas = Some(Arc::new(patterned_atlas(16, 16, 2)));
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.cat_changed,
        "an atlas-version bump alone must set cat_changed (a rebake repaints)"
    );
    assert!(!d.is_gate_hit(), "a rebaked atlas must NOT gate-hit");
    let marked: Vec<usize> = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    assert_eq!(marked, vec![1], "the rebake marks the (unmoved) cat's row");
}
