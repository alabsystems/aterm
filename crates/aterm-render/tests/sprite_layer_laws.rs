// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sprite layers' laws on the CPU renderer (lane B7 of
//! `docs/DESIGN-card-engine-2026-10-19.md`): a Linear Under layer draws under
//! the glyphs, a 1:1 sprite lands its art's exact texels, an integer scale
//! samples nearest, and a moved sprite leaves no ghost (identity damage is
//! `free_overlay`'s).

use std::sync::Arc;

use aterm_core::render::{
    FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect,
};
use aterm_core::terminal::Terminal;
use aterm_render::sprites::{FreeSprite, SpriteLayers};
use aterm_render::{Frame, Renderer, Theme, WindowCpu, rgb_to_u32};

use crate::sprites::{SALT, art_of, patterned, solid};

fn renderer() -> Renderer {
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

/// A plain Linear Under layer (the rain's kind: no knock-out) drawing `art`
/// stretched to `w × h` at `(x, y)`.
fn plain_under(x: i32, y: i32, w: u16, h: u16, art: Arc<aterm_core::render::FxArt>) -> FxLayer {
    FxLayer::new(
        1,
        Arc::new(FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x,
                y,
                dw: w,
                dh: h,
                flip_x: false,
                art,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00FF_FFFF, 255)],
        )),
        FxSlot::Under,
        FxSpace::Grid,
        FxBlend::Linear,
    )
}

#[test]
fn a_glyph_draws_over_a_linear_under_layer() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 8);
    term.process("\x1b[?25l█".as_bytes());
    let mut input = term.cell_frame(2, 8);
    input.fx_layers = vec![plain_under(
        0,
        0,
        (2 * cw) as u16,
        ch as u16,
        solid([0xC8, 0x20, 0x20, 255], 8, 8),
    )];
    let f = rend.render_input(&input);
    let fg = rgb_to_u32(input.cells[0][0].fg);
    assert!(
        cell_pixels(&f, cw, ch, 0, 0).iter().all(|&p| p == fg),
        "the full-block glyph draws OVER the layer (cell 0 is pure fg)"
    );
    assert!(
        cell_pixels(&f, cw, ch, 0, 1)
            .iter()
            .all(|&p| p == 0x00C8_2020),
        "the uncovered half is the exact sprite colour (cell 1 pure red)"
    );
}

#[test]
fn a_sprite_is_texel_exact_at_one_to_one_and_nearest_at_an_integer_scale() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25l");
    let (aw, ah) = (24u16, (ch as u16).min(24));
    let art = patterned(aw, ah);
    let mut input = term.cell_frame(3, 12);
    crate::sprites::put(&mut input, &[FreeSprite::whole(4, ch as i32, art.clone())]);
    let f = rend.render_input(&input);
    for dy in 0..usize::from(ah) {
        for dx in 0..usize::from(aw) {
            let i = (dy * usize::from(aw) + dx) * 4;
            let d = art.data();
            let want = (u32::from(d[i]) << 16) | (u32::from(d[i + 1]) << 8) | u32::from(d[i + 2]);
            assert_eq!(
                f.pixels[(ch + dy) * f.width + 4 + dx],
                want,
                "1:1 lands the exact texel at ({dx},{dy})"
            );
        }
    }

    let checker = art_of(2, 2, |x, y| {
        if (x + y) % 2 == 0 {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 255]
        }
    });
    let mut input = term.cell_frame(3, 12);
    crate::sprites::put(
        &mut input,
        &[FreeSprite {
            w: 16,
            h: 2 * (ch as u16 / 2),
            ..FreeSprite::whole(0, 2 * ch as i32, checker)
        }],
    );
    let f = rend.render_input(&input);
    for dy in 0..2 * (ch / 2) {
        for dx in 0..16usize {
            let p = f.pixels[(2 * ch + dy) * f.width + dx];
            let want = if (dx / 8 + dy / (ch / 2)) % 2 == 0 {
                0x00FF_FFFF
            } else {
                0
            };
            assert_eq!(p, want, "nearest at an integer scale, at ({dx},{dy})");
        }
    }
}

#[test]
fn a_moved_sprite_leaves_no_ghost_on_the_damaged_path() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let mut term = Terminal::new(4, 12);
    term.process(b"\x1b[?25l");
    let art = patterned(24, (ch as u16).min(24));
    let mut pubr = SpriteLayers::new(SALT);
    let mut make = |row: usize| {
        let mut input = term.cell_frame(4, 12);
        pubr.publish(
            &[FreeSprite::whole(2, (row * ch) as i32, art.clone())],
            &mut input.fx_layers,
        );
        input
    };
    let (in_a, in_b) = (make(1), make(2));
    let mut wc = WindowCpu::new();
    let a = rend.render_input_cached(&mut wc, &in_a).pixels().to_vec();
    let b = rend.render_input_cached(&mut wc, &in_b).pixels().to_vec();
    assert_ne!(a, b, "the sprite moved");
    assert_eq!(
        b,
        rend.render_input(&in_b).pixels,
        "the damaged repaint after a move equals a full render"
    );
}
