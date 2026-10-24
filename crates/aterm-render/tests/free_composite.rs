// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The companions' sprite layers composited on the CPU: a multi-row sprite is
//! the old per-row stamp byte for byte, a moved one leaves no ghost, a settled
//! translucent one is never stamped twice, and each sits at its z.

use std::sync::Arc;

use aterm_core::render::{
    DecoBlend, DecoGlyph, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace,
    FxSprite, GlowQuad, PxRect, WordDecoration,
};
use aterm_core::terminal::Terminal;
use aterm_render::legacy_sprite_oracle::{LegacySprite, LegacySprites};
use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers};
use aterm_render::{Renderer, Theme, WindowCpu, premul_rgb, rgb_to_u32};

use crate::sprites::{SALT, art_of, solid, under};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// A `w × h` cut of a 64×128 pattern at `src`: opaque in its first 16 rows,
/// translucent below.
fn free_art(w: u16, h: u16, [sx, sy]: [u16; 2]) -> Arc<FxArt> {
    art_of(w, h, |x, y| {
        let (x, y) = (u32::from(x + sx), u32::from(y + sy));
        let a = if y < 16 {
            255u8
        } else {
            (60 + (x * 3 + y) % 180) as u8
        };
        [
            (x * 37 + y * 11) as u8,
            (x * 5 + y * 53) as u8,
            (x * 29 + y * 3) as u8,
            a,
        ]
    })
}

#[test]
fn a_multirow_sprite_is_the_old_stamp_and_its_move_repaints_exactly() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let h = (2 * ch + ch / 2) as u16;
    let art = free_art(40, h, [2, 3]);
    let (y_a, y_b) = ((ch + ch / 2) as i32, (2 * ch + ch / 2) as i32);
    let mut pubr = SpriteLayers::new(SALT);
    let mut make = |term: &mut Terminal, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        pubr.publish(&[under(4, y, art.clone())], &mut input.fx_layers);
        input
    };

    let base = rend
        .render_input(&term.cell_frame(rows, cols))
        .pixels
        .clone();
    let full = rend.render_input(&make(&mut term, y_a)).pixels.clone();
    assert_ne!(full, base, "the multi-row sprite paints");
    rend.set_legacy_sprites(Some(LegacySprites {
        sprites: vec![LegacySprite::of(&under(4, y_a, art.clone())).expect("whole")],
    }));
    let old = rend
        .render_input(&term.cell_frame(rows, cols))
        .pixels
        .clone();
    rend.set_legacy_sprites(None);
    assert_eq!(
        full, old,
        "one multi-row layer is the old stamp byte for byte"
    );

    let mut wc = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc, &make(&mut term, y_a));
    let moved = make(&mut term, y_b);
    let dmg = rend.render_input_cached(&mut wc, &moved).pixels().to_vec();
    assert_eq!(
        dmg,
        rend.render_input(&moved).pixels,
        "the damaged move equals a full render"
    );
}

#[test]
fn free_no_ghosting_cached_equals_fresh_including_off_grid_pad_spill() {
    let mut rend = renderer();
    rend.set_pad(6);
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let art = free_art(40, (ch + 8) as u16, [2, 0]);
    let mut pubr = SpriteLayers::new(SALT);
    let mut make = |term: &mut Terminal, at: Option<(i32, i32)>| {
        let mut input = term.cell_frame(rows, cols);
        let sprites: Vec<FreeSprite> = at
            .map(|(x, y)| under(x, y, art.clone()))
            .into_iter()
            .collect();
        pubr.publish(&sprites, &mut input.fx_layers);
        input
    };
    let frames = [
        make(&mut term, Some((2, -8))),
        make(&mut term, Some((-5, (ch + 2) as i32))),
        make(&mut term, Some((2, (rows * ch - 4) as i32))),
        make(&mut term, None),
    ];

    let mut wc = WindowCpu::new();
    let mut prev_px: Option<Vec<u32>> = None;
    for (i, input) in frames.iter().enumerate() {
        let cached = rend.render_input_cached(&mut wc, input).pixels().to_vec();
        let fresh = rend.render_input(input).pixels.clone();
        assert_eq!(
            cached, fresh,
            "frame {i}: cached incremental repaint must equal a fresh full \
             render (no ghost in any vacated band or pad strip)"
        );
        if let Some(prev) = prev_px.take() {
            assert_ne!(
                prev, cached,
                "frame {i}: the move must actually change pixels"
            );
        }
        prev_px = Some(cached);
    }
}

#[test]
fn settled_translucent_sprite_survives_text_edits_without_double_stamp() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (6usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lhello free overlay");
    term.process(b"\x1b[3;1Hmiddle row");

    let sprite = FreeSprite {
        alpha: 140,
        ..under(
            6,
            (ch + ch / 2) as i32,
            free_art(48, (2 * ch) as u16, [1, 2]),
        )
    };
    let mut pubr = SpriteLayers::new(SALT);
    let mut make = |t: &mut Terminal| {
        let mut input = t.cell_frame(rows, cols);
        pubr.publish(std::slice::from_ref(&sprite), &mut input.fx_layers);
        input
    };

    let mut wc = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc, &make(&mut term));

    term.process(b"\x1b[6;1Hunrelated edit");
    let in_a = make(&mut term);
    let cached_a = rend.render_input_cached(&mut wc, &in_a).pixels().to_vec();
    let fresh_a = rend.render_input(&in_a).pixels.clone();
    assert_eq!(
        cached_a, fresh_a,
        "an edit in a non-overlapped row must not restamp (or double-blend) \
         the settled sprite"
    );

    term.process(b"\x1b[3;1Hburrowed row");
    let in_b = make(&mut term);
    let cached_b = rend.render_input_cached(&mut wc, &in_b).pixels().to_vec();
    let fresh_b = rend.render_input(&in_b).pixels.clone();
    assert_eq!(
        cached_b, fresh_b,
        "an edit under the sprite must restamp exactly the re-cleared band's \
         slice (no double-blend in the untouched bands)"
    );
}

/// The rain's kind of layer: Linear, Under, no knock-out, `art` stretched to
/// `w × h` at `(x, y)`.
fn rain_like(x: i32, y: i32, w: u16, h: u16, art: Arc<FxArt>) -> FxLayer {
    FxLayer::new(
        2,
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
fn free_z_under_text_over_the_rain_and_over_text() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l█".as_bytes());
    let red = 0x00C8_2020u32;
    let body = FreeSprite {
        w: (2 * cw) as u16,
        h: (2 * ch) as u16,
        ..under(0, 0, solid([0xC8, 0x20, 0x20, 255], 8, 8))
    };

    let mut input = term.cell_frame(rows, cols);
    input.fx_layers.push(rain_like(
        cw as i32,
        0,
        cw as u16,
        ch as u16,
        solid([0x20, 0x20, 0xC8, 255], 8, 8),
    ));
    SpriteLayers::new(SALT).publish(std::slice::from_ref(&body), &mut input.fx_layers);

    let f = rend.render_input(&input);
    let fg = rgb_to_u32(input.cells[0][0].fg);
    let px = |row: usize, col: usize| -> Vec<u32> {
        let mut out = Vec::new();
        for y in row * ch..(row + 1) * ch {
            for x in col * cw..(col + 1) * cw {
                out.push(f.pixels[y * f.width + x]);
            }
        }
        out
    };
    assert!(
        px(0, 0).iter().all(|&p| p == fg),
        "UnderText: the full-block glyph must draw OVER the multi-row sprite"
    );
    // The knock-out cuts the body under the glyph's ink and two pixels round it
    // (`KNOCK_DILATE_OFFSETS`), clipped as the glyph is to the foot of its row: the block's
    // page ring lies in the cell beside it.
    let reach = |x: usize, y: usize| {
        y < ch
            && aterm_render::KNOCK_DILATE_OFFSETS.iter().any(|&(dx, dy)| {
                let (bx, by) = (x as i64 - i64::from(dx), y as i64 - i64::from(dy));
                (0..cw as i64).contains(&bx) && (0..ch as i64).contains(&by)
            })
    };
    let page = rgb_to_u32(input.cells[0][0].bg);
    let band = |row: usize, col: usize| -> Vec<(usize, usize, u32)> {
        let mut out = Vec::new();
        for y in row * ch..(row + 1) * ch {
            for x in col * cw..(col + 1) * cw {
                out.push((x, y, f.pixels[y * f.width + x]));
            }
        }
        out
    };
    assert!(
        band(0, 1)
            .iter()
            .all(|&(x, y, p)| if reach(x, y) { p == page } else { p == red }),
        "the companion draws OVER the rain under it, and the knock-out lays the page \
         round the block's ink"
    );
    assert!(
        band(1, 0)
            .iter()
            .chain(&band(1, 1))
            .all(|&(x, y, p)| if reach(x, y) { p == page } else { p == red }),
        "the sprite's second band must not be clobbered by that row's bg"
    );

    let mut input_over = term.cell_frame(rows, cols);
    input_over.word_decorations.push(WordDecoration {
        row: 0,
        col: 1,
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Paw,
        blend: DecoBlend::Over,
        color: 0x0020_C020,
        alpha: 255,
    });
    input_over.cursor_glow_add.push(GlowQuad {
        row: 1,
        x: 0,
        y: ch as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: premul_rgb(0x0040_80FF, 200),
        alpha: 0,
        color2: premul_rgb(0x0040_80FF, 200),
        alpha2: 0,
    });
    input_over.nova_add.push(GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: cw as u16,
        h: ch as u16,
        color: premul_rgb(0x00FF_C040, 200),
        alpha: 0,
        color2: premul_rgb(0x00FF_C040, 200),
        alpha2: 0,
    });
    let bare = rend.render_input(&input_over);
    SpriteLayers::new(SALT).publish(
        &[FreeSprite {
            z: FreeZ::OverText,
            ..body
        }],
        &mut input_over.fx_layers,
    );
    let f = rend.render_input(&input_over);
    assert_ne!(
        bare.pixels, f.pixels,
        "the wdeco stamp + additive light must actually paint under the sprite"
    );
    let over = |row: usize, col: usize| -> Vec<u32> {
        let mut out = Vec::new();
        for y in row * ch..(row + 1) * ch {
            for x in col * cw..(col + 1) * cw {
                out.push(f.pixels[y * f.width + x]);
            }
        }
        out
    };
    assert!(
        over(0, 0).iter().all(|&p| p == red),
        "OverText: the sprite must draw OVER the glyph and the nova light"
    );
    assert!(
        over(0, 1).iter().all(|&p| p == red),
        "OverText: the sprite must draw OVER the wdeco stamp"
    );
    assert!(
        over(1, 0).iter().all(|&p| p == red) && over(1, 1).iter().all(|&p| p == red),
        "OverText: the sprite must draw OVER the additive glow"
    );
}
