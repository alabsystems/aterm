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
    // (`KNOCK_DILATE_OFFSETS`), clipped as the glyph is to the foot of its row: the cut shows
    // what lies under the body, the rain in the cell beside the block.
    let reach = |x: usize, y: usize| {
        y < ch
            && aterm_render::KNOCK_DILATE_OFFSETS.iter().any(|&(dx, dy)| {
                let (bx, by) = (x as i64 - i64::from(dx), y as i64 - i64::from(dy));
                (0..cw as i64).contains(&bx) && (0..ch as i64).contains(&by)
            })
    };
    let page = rgb_to_u32(input.cells[0][0].bg);
    let rain = 0x0020_20C8u32;
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
            .all(|&(x, y, p)| if reach(x, y) { p == rain } else { p == red }),
        "the companion draws OVER the rain under it, and the knock-out cuts it back to the \
         rain round the block's ink"
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

/// The rainbow ribbon's body across row 0 of a `cols`-wide grid: source-over as a light
/// theme lays it, and additive.
fn ribbon_light(input: &mut aterm_core::render::RenderInput, cols: usize, cw: usize, ch: usize) {
    for (alpha, color) in [(160u8, 0x0040_C0A0u32), (0, 0x0060_6000)] {
        let premul = premul_rgb(color, if alpha == 0 { 200 } else { alpha });
        input.glow_under.push(GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: (cols * cw) as u16,
            h: ch as u16,
            color: premul,
            alpha,
            color2: premul,
            alpha2: alpha,
        });
    }
}

/// A COMPANION STANDS IN FRONT OF THE LIGHT UNDER THE WORDS. A companion's body (a knock layer
/// under the text) sits on the under-glyph light (`glow_under`: the rainbow ribbon's body, the
/// meteor's train, the fire's flame body), never in it, and stays under the ink: drawn under
/// that light, a cat whose head reached into the typed row wore the ribbon's colours (GitHub
/// Light, round 14's open K5 item). The letters on the body keep their knock-out ring of page,
/// and only on the body: a letter in the body's box where the art is clear keeps the light
/// round it, as it is with no body at all.
#[test]
fn a_companion_sits_on_the_under_glyph_light() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // A full block at col 0 and a letter at col 1, both in the body's box.
    term.process("\x1b[?25l█x".as_bytes());
    let red = 0x00C8_2020u32;
    // The body's box spans cols 0..4 of row 0; its art is clear over col 1.
    let w = (4 * cw) as u16;
    let art = art_of(w, ch as u16, |x, _| {
        if (cw..2 * cw).contains(&usize::from(x)) {
            [0, 0, 0, 0]
        } else {
            [0xC8, 0x20, 0x20, 255]
        }
    });
    let mut input = term.cell_frame(rows, cols);
    ribbon_light(&mut input, cols, cw, ch);
    let lit = rend.render_input(&input).pixels.clone();
    SpriteLayers::new(SALT).publish(&[under(0, 0, art)], &mut input.fx_layers);
    let f = rend.render_input(&input);
    let fg = rgb_to_u32(input.cells[0][0].fg);
    let cell = |px: &[u32], col: usize| -> Vec<u32> {
        (0..ch)
            .flat_map(|y| (col * cw..(col + 1) * cw).map(move |x| (y, x)))
            .map(|(y, x)| px[y * f.width + x])
            .collect()
    };
    assert!(
        cell(&f.pixels, 3).iter().all(|&p| p == red),
        "the body is drawn over the under-glyph light, its own colour"
    );
    assert!(
        cell(&f.pixels, 0).iter().all(|&p| p == fg),
        "and under the glyph ink: the full block stays pure fg"
    );
    assert_eq!(
        cell(&f.pixels, 1),
        cell(&lit, 1),
        "where the art is clear, the letter keeps the light round it: no ring of page there"
    );
    assert_eq!(
        cell(&f.pixels, 5),
        cell(&lit, 5),
        "beside the body the light is as it was"
    );
    assert_ne!(
        cell(&lit, 5),
        cell(&rend.render_input(&term.cell_frame(rows, cols)).pixels, 5),
        "non-vacuity: the light paints the row"
    );
}

/// THE KNOCK-OUT CUTS THE BODY BACK TO WHAT LIES UNDER IT. A letter on a companion's body
/// reads against what lies under the body there (`Renderer::cut_knock_layers`): over the
/// under-glyph light, the cut shows the light, never a ring of bare page. Laid as a page,
/// the ring met the light round the rest of the letter in a hard edge across it, where the
/// gallery's judge read each channel's median of page and light, brighter than either (the
/// flat rainbow's burst on Solarized Dark: `f` at 3.77:1 of its own 4.06).
#[test]
fn a_letter_on_a_body_reads_against_what_lies_under_it() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // A letter at col 3, on the body.
    term.process("\x1b[?25l\x1b[1;4Ho".as_bytes());
    let page = rgb_to_u32(term.cell_frame(rows, cols).cells[0][0].bg);
    let mut input = term.cell_frame(rows, cols);
    ribbon_light(&mut input, cols, cw, ch);
    let lit = rend.render_input(&input).pixels.clone();
    SpriteLayers::new(SALT).publish(
        &[under(
            (2 * cw) as i32,
            0,
            solid([0xC8, 0x20, 0x20, 255], (4 * cw) as u16, ch as u16),
        )],
        &mut input.fx_layers,
    );
    let f = rend.render_input(&input);
    let cell: Vec<usize> = (0..ch)
        .flat_map(|y| (3 * cw..4 * cw).map(move |x| y * f.width + x))
        .collect();
    assert!(
        cell.iter()
            .any(|&i| f.pixels[i] == lit[i] && lit[i] != page),
        "the cut round the letter shows the light under the body"
    );
    assert!(
        cell.iter()
            .all(|&i| f.pixels[i] & 0x00FF_FFFF != page & 0x00FF_FFFF),
        "and no ring of bare page is laid over the light"
    );
}

/// A BODY'S CUT IS KEPT FOR ITS ART'S CONTENT. The knock-out keeps last frame's cut of a body
/// while nothing under it changed, keyed by what it draws: the art's content address, as the
/// sprite layers and the GPU's atlas name an art. Keyed by the art's address, a cut outlived
/// its art and could be dealt to a different art the allocator later put at that address; and
/// the same art published anew was cut again.
#[test]
fn a_bodys_cut_is_kept_for_its_arts_content() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l\x1b[1;4Ho".as_bytes());
    let base = term.cell_frame(rows, cols);
    let (w, h) = {
        let f = rend.render_input(&base);
        (f.width, f.height)
    };
    let mut cut_of = |rgba: [u8; 4]| {
        let mut input = base.clone();
        SpriteLayers::new(SALT).publish(
            &[under(
                (2 * cw) as i32,
                0,
                solid(rgba, (4 * cw) as u16, ch as u16),
            )],
            &mut input.fx_layers,
        );
        assert!(
            rend.cut_knock_layers(&input, w, h),
            "fixture: the letter cuts the body"
        );
        rend.cut_layers()
            .iter()
            .find(|l| l.knock)
            .map(|l| Arc::clone(&l.content))
            .expect("the cut body")
    };
    let red = [0xC8, 0x20, 0x20, 255];
    let first = cut_of(red);
    let again = cut_of(red);
    assert!(
        Arc::ptr_eq(&first, &again),
        "the same art, published anew, keeps its cut"
    );
    let green = cut_of([0x20, 0xC8, 0x20, 255]);
    assert!(
        !Arc::ptr_eq(&again, &green),
        "control: another art is cut anew"
    );
}

/// A LETTER'S CUT ABOVE ITS ROW IS REPAINTED WITH IT. A glyph typed under a body that spans
/// two rows cuts the body up to [`aterm_render::KNOCK_REACH`] pixels over its own row (a full
/// block's ring), so its change repaints the row above where a body lies there, and the
/// damaged repaint equals a fresh render: repainted alone, its row left the body over it uncut.
#[test]
fn a_letters_cut_above_its_row_is_repainted_with_it() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let body = || {
        under(
            cw as i32,
            0,
            solid([0xC8, 0x20, 0x20, 255], (5 * cw) as u16, (3 * ch) as u16),
        )
    };
    let mut wc = WindowCpu::new();
    let mut first = term.cell_frame(rows, cols);
    let mut pubr = SpriteLayers::new(SALT);
    pubr.publish(&[body()], &mut first.fx_layers);
    let _ = rend.render_input_cached(&mut wc, &first);
    // Full blocks typed on row 1, under the body: their ink fills the row to its top.
    term.process("\x1b[2;3H███".as_bytes());
    let mut typed = term.cell_frame(rows, cols);
    pubr.publish(&[body()], &mut typed.fx_layers);
    let damaged = rend.render_input_cached(&mut wc, &typed).pixels().to_vec();
    let fresh = rend.render_input(&typed).pixels;
    assert_ne!(
        fresh,
        rend.render_input(&first).pixels,
        "fixture: the letters cut the body"
    );
    assert_eq!(damaged, fresh, "the damaged repaint equals a fresh render");
}
