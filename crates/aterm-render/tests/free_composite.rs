// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Free-floating overlay layer, CPU side (FREE_OVERLAY_LAYER_DESIGN.md §3.2.2/§5). The
// shared `composite_free` phase runner (A bg → B1 legacy per-row sprites → B2 under-text
// free sprites → C fg) runs on both the damaged (`render_core`) and full (`full_render`)
// paths; over-text free sprites stamp last before the cursor (the GPU `FreeOver` slot).
// Pinned: a free rect spanning >= 3 row bands equals its legacy per-row `cat_quads` slices
// on both paths (§5.2); moved sprites, including signed off-grid origins spilling into the
// pad strips, leave cached == fresh (§5.5); a settled translucent sprite is never
// double-stamped when a row repaints; z order (§5.6).

use std::sync::Arc;

use aterm_core::render::{
    DecoBlend, DecoGlyph, FreeSampler, FreeSprite, FreeZ, GlowQuad, SceneAtlas, SpriteQuad,
    WordDecoration,
};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu, premul_rgb, rgb_to_u32};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// A patterned RGBA atlas tall enough to span several row bands: distinct per-texel colours
/// (a wrong NEAREST index shows) and mixed alpha below the top strip.
fn free_atlas(version: u64) -> SceneAtlas {
    let (w, h) = (64u32, 128u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if y < 16 {
                255u8
            } else {
                (60 + (x * 3 + y) % 180) as u8
            };
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                a,
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

/// A NEAREST 1:1 free sprite (`aw/ah == w/h`), under text, with a signed dest origin.
fn free_1to1(x: i32, y: i32, w: u16, h: u16, src_xy: [u16; 2]) -> FreeSprite {
    let [ax, ay] = src_xy;
    FreeSprite {
        x,
        y,
        w,
        h,
        ax,
        ay,
        aw: w,
        ah: h,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    }
}

/// The legacy emission of the same art: one `SpriteQuad` per row band the rect overlaps,
/// each sub-windowing the same atlas region.
fn legacy_slices(x: i32, y: i32, w: u16, h: u16, src_xy: [u16; 2], ch: usize) -> Vec<SpriteQuad> {
    let [ax, ay] = src_xy;
    let (y0, y1) = (y as usize, y as usize + h as usize);
    let mut slices = Vec::new();
    for r in y0 / ch..=(y1 - 1) / ch {
        let band_y0 = y0.max(r * ch);
        let band_y1 = y1.min((r + 1) * ch);
        slices.push(SpriteQuad {
            row: r as u16,
            x: x as u16,
            y: band_y0 as u16,
            w,
            h: (band_y1 - band_y0) as u16,
            ax,
            ay: ay + (band_y0 - y0) as u16,
            aw: w,
            ah: (band_y1 - band_y0) as u16,
            tint: 0x00FF_FFFF,
            alpha: 255,
            flip_x: false,
        });
    }
    slices
}

/// §5.2: a >= 3-band free rect equals the legacy per-row slices byte for byte, on the full
/// path (`render_input`) and the damaged path (`render_input_cached` after priming at
/// another position, so every prev∪cur band repaints through the same phase runner).
#[test]
fn free_multirow_rect_matches_legacy_perrow_slices_on_both_paths() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (6usize, 12usize);
    // Glyph-free terminal: the comparison is sprite-only.
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(1));

    // Sub-cell origin mid-band of row 1, spanning into band 3 (>= 2 crossings).
    let (x, w) = (4i32, 40u16);
    let h = (2 * ch + ch / 2) as u16;
    let (y_a, y_b) = ((ch + ch / 2) as i32, (2 * ch + ch / 2) as i32);
    let src = [2u16, 3u16];

    let make_free = |term: &mut Terminal, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![free_1to1(x, y, w, h, src)];
        input
    };
    let make_legacy = |term: &mut Terminal, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        input.cat_atlas = Some(atlas.clone());
        input.cat_quads = legacy_slices(x, y, w, h, src, ch);
        assert!(
            input.cat_quads.len() >= 3,
            "the rect must span >= 3 bands (multi-row premise)"
        );
        input
    };

    // Full path: identical, and non-vacuous.
    let base = rend
        .render_input(&term.cell_frame(rows, cols))
        .pixels
        .clone();
    let free_full = rend.render_input(&make_free(&mut term, y_a)).pixels.clone();
    let legacy_full = rend
        .render_input(&make_legacy(&mut term, y_a))
        .pixels
        .clone();
    assert_ne!(
        free_full, base,
        "the multi-row free rect must actually paint"
    );
    assert_eq!(
        free_full, legacy_full,
        "FULL path: one multi-row free rect must equal its legacy per-row \
         slices byte-for-byte on the CPU (NEAREST 1:1 has no seam)"
    );

    // Damaged path: prime each cache at y_a, then move to y_b.
    let mut wc_free = WindowCpu::new();
    let mut wc_legacy = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc_free, &make_free(&mut term, y_a));
    let free_dmg = rend
        .render_input_cached(&mut wc_free, &make_free(&mut term, y_b))
        .pixels()
        .to_vec();
    let _ = rend.render_input_cached(&mut wc_legacy, &make_legacy(&mut term, y_a));
    let legacy_dmg = rend
        .render_input_cached(&mut wc_legacy, &make_legacy(&mut term, y_b))
        .pixels()
        .to_vec();
    assert_eq!(
        free_dmg, legacy_dmg,
        "DAMAGED path: the moved multi-row free rect must equal its moved \
         legacy slices byte-for-byte on the CPU"
    );
    let free_fresh = rend.render_input(&make_free(&mut term, y_b)).pixels.clone();
    assert_eq!(
        free_dmg, free_fresh,
        "the damaged-path free rect must equal a fresh full render"
    );
}

/// §5.5: with a non-zero pad, a sprite peeking in above the grid, moving on-grid with a
/// negative x, rising from below the bottom edge, then gone: every cached repaint equals a
/// fresh full render (vacated bands and pad strips are re-cleared).
#[test]
fn free_no_ghosting_cached_equals_fresh_including_off_grid_pad_spill() {
    let mut rend = renderer();
    rend.set_pad(6);
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(2));

    let make = |term: &mut Terminal, x: i32, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![free_1to1(x, y, 40, (ch + 8) as u16, [2, 0])];
        input
    };
    let frames = [
        make(&mut term, 2, -8),
        make(&mut term, -5, (ch + 2) as i32),
        make(&mut term, 2, (rows * ch - 4) as i32),
        {
            let mut input = term.cell_frame(rows, cols);
            input.free_atlas = Some(atlas.clone());
            input
        },
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

/// A settled translucent multi-row sprite is never double-stamped: a text edit in a row it
/// does not overlap leaves it untouched, and one in a row it overlaps restamps only that
/// band's slice. Both cached frames equal a fresh full render.
#[test]
fn settled_translucent_sprite_survives_text_edits_without_double_stamp() {
    let mut rend = renderer();
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (6usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lhello free overlay");
    term.process(b"\x1b[3;1Hmiddle row");

    let atlas = Arc::new(free_atlas(5));
    // Translucent (alpha 140) rect over bands 1..=3 — double-stamping is visible.
    let sprite = FreeSprite {
        alpha: 140,
        ..free_1to1(6, (ch + ch / 2) as i32, 48, (2 * ch) as u16, [1, 2])
    };
    let make = |t: &mut Terminal| {
        let mut input = t.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![sprite];
        input
    };

    let mut wc = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc, &make(&mut term));

    // (a) Text edit in row 5 — far below the sprite's bands.
    term.process(b"\x1b[6;1Hunrelated edit");
    let in_a = make(&mut term);
    let cached_a = rend.render_input_cached(&mut wc, &in_a).pixels().to_vec();
    let fresh_a = rend.render_input(&in_a).pixels.clone();
    assert_eq!(
        cached_a, fresh_a,
        "an edit in a non-overlapped row must not restamp (or double-blend) \
         the settled sprite"
    );

    // (b) Text edit in row 3 — a band the sprite overlaps.
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

/// §5.6 z order: an `UnderText` sprite sits under glyphs (a full block stays pure fg) but
/// over a legacy cat quad (B1 then B2); an `OverText` sprite draws over everything except
/// the cursor: glyphs, a wdeco `Over` stamp and the additive glow/nova light.
#[test]
fn free_z_under_text_over_legacy_and_over_text() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (3usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l█".as_bytes());

    let solid = |c: [u8; 4], version: u64| SceneAtlas {
        width: 8,
        height: 8,
        rgba: (0..8 * 8).flat_map(|_| c).collect(),
        version,
    };
    let red = 0x00C8_2020u32;

    // A multi-row red free sprite over cells (0,0)..(1,1); a blue cat quad under it at
    // row 0, cell 1.
    let mut input = term.cell_frame(rows, cols);
    input.free_atlas = Some(Arc::new(solid([0xC8, 0x20, 0x20, 255], 1)));
    input.free_sprites = vec![FreeSprite {
        aw: 8,
        ah: 8,
        ..free_1to1(0, 0, (2 * cw) as u16, (2 * ch) as u16, [0, 0])
    }];
    input.cat_atlas = Some(Arc::new(solid([0x20, 0x20, 0xC8, 255], 2)));
    input.cat_quads = vec![SpriteQuad {
        row: 0,
        x: cw as u16,
        y: 0,
        w: cw as u16,
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
    assert!(
        px(0, 1).iter().all(|&p| p == red),
        "B1-then-B2: the free under-sprite must draw OVER the legacy cat quad"
    );
    assert!(
        px(1, 0).iter().all(|&p| p == red) && px(1, 1).iter().all(|&p| p == red),
        "the sprite's second band must not be clobbered by that row's bg"
    );

    // OverText: the same sprite in the over-text slot.
    let mut input_over = input.clone();
    input_over.cat_quads.clear();
    input_over.cat_atlas = None;
    input_over.free_sprites[0].z = FreeZ::OverText;
    // A wdeco stamp plus glow and nova light under the sprite's footprint.
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
        // Additive light (see `GlowQuad::alpha`).
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
        // Additive light (see `GlowQuad::alpha`).
        alpha: 0,
        color2: premul_rgb(0x00FF_C040, 200),
        alpha2: 0,
    });
    // Non-vacuity: without the sprite, the stamp and the light paint.
    let mut input_bare = input_over.clone();
    input_bare.free_sprites.clear();
    input_bare.free_atlas = None;
    let bare = rend.render_input(&input_bare);
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
