// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Free-floating overlay layer, Phase 2 (FREE_OVERLAY_LAYER_DESIGN.md §3.2.2/§5):
// CPU consumption through the shared `composite_free` phase runner (A bg → B1
// legacy per-row sprites → B2 under-text free sprites → C fg), run by BOTH the
// damaged (`render_core`) and full (`full_render`) paths; OVER-TEXT free
// sprites stamp as the last post-pass before the cursor (the GPU `FreeOver`
// slot — over the additive post-passes and the wdeco stamps). The contract
// under test:
//   * cross-row-band equality (§5.2): ONE free rect spanning >= 3 cell-row bands
//     is byte-for-byte identical to the legacy per-row-sliced `cat_quads`
//     emission of the same art, on the FULL path AND on the damaged path —
//     proving the host head/chin split is no longer needed (no seam, no
//     clobber by the next row's bg);
//   * damaged-path no-ghosting (§5.5): a moved free sprite — including SIGNED
//     off-grid origins that spill into the top/bottom/left pad strips — leaves
//     cached == fresh byte-for-byte (Phase A re-lays bg for every prev∪cur
//     band, plus the band-edge pad-strip reset);
//   * a SETTLED (unchanged) translucent sprite is never double-stamped when an
//     unrelated or overlapped text row repaints (the stamp is clipped to the
//     re-cleared bands — the CPU twin of the GPU dirty-band scissor);
//   * under-text z at an arbitrary multi-row position (§5.6): a glyph over any
//     part of the sprite stays pure theme fg; `FreeZ::OverText` draws over the
//     glyphs AND over a wdeco stamp AND over additive glow/nova light (over
//     everything except the cursor — the GPU `FreeOver` slot); and B1-then-B2
//     order — a free under-sprite sits OVER a legacy cat quad (mirroring the
//     GPU `FreeUnder`-after-`cat_over` slot);
//   * perf (§5.8): the tall-rect composite bench companion to
//     `bench_render_row_under_sprites` (manual, `--ignored --nocapture`).

use std::sync::Arc;

use aterm_core::render::{
    DecoBlend, DecoGlyph, FreeSampler, FreeSprite, FreeZ, GlowQuad, SceneAtlas, SpriteQuad,
    WordDecoration,
};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu, premul_rgb, rgb_to_u32};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// A deterministic patterned RGBA atlas, tall enough for a rect spanning
/// several cell-row bands: per-texel distinct colours (a wrong NEAREST index
/// shows up), mixed alpha below the top strip (real src-over blending).
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

/// A NEAREST-1:1 free sprite (`aw/ah == w/h`, the bake==dest contract), under
/// text by default; SIGNED grid-interior dest origin.
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

/// The legacy emission of the same art: one single-band `SpriteQuad` per
/// cell-row band the rect `[y, y+h)` overlaps, each sub-windowing the same
/// atlas region (the host head/chin split this layer retires).
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

/// §5.2 cross-row-band equality, CPU byte-exact, BOTH paths: one >= 3-band free
/// rect vs the equivalent legacy per-row slices. Full path via `render_input`
/// (throwaway cache => `full_render`), damaged path via `render_input_cached`
/// after priming with a moved variant (the row-union marks every prev∪cur
/// band, so `render_core` repaints exactly those bands through the SAME
/// phase runner).
#[test]
fn free_multirow_rect_matches_legacy_perrow_slices_on_both_paths() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (6usize, 12usize);
    // Glyph-free terminal: the sliced-vs-free comparison is sprite-only (the
    // glyph AA band quirk is a pre-existing damaged-vs-full note, not ours).
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

    // FULL path: byte-for-byte identical, and non-vacuous.
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

    // DAMAGED path: prime each cache at y_a, then move to y_b.
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
    // And the damaged repaint equals a fresh full render (twins by construction).
    let free_fresh = rend.render_input(&make_free(&mut term, y_b)).pixels.clone();
    assert_eq!(
        free_dmg, free_fresh,
        "the damaged-path free rect must equal a fresh full render"
    );
}

/// §5.5 damaged-path no-ghosting with SIGNED off-grid origins and a non-zero
/// pad: a sprite peeking in from above the grid (negative y, into the top pad
/// strip), moving on-grid (negative x, into the left pad columns), then rising
/// from below the bottom edge (into the bottom pad strip). Every cached
/// incremental repaint must equal a fresh full render byte-for-byte — the
/// vacated bands AND the pad strips are re-cleared (Phase A's band clear +
/// band-edge strip reset, the CPU twin of the GPU edge scissor).
#[test]
fn free_no_ghosting_cached_equals_fresh_including_off_grid_pad_spill() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // Top peek (spills into the top pad strip) → on-grid with a negative x
    // (spills into the left pad columns) → bottom peek (bottom pad strip) →
    // gone entirely.
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

/// A SETTLED translucent multi-row sprite must never be double-stamped by a
/// damaged repaint: (a) an unrelated text edit in a row the sprite does NOT
/// overlap leaves its pixels untouched; (b) a text edit in a row it DOES
/// overlap restamps ONLY that band's slice over the fresh bg. Both cached
/// frames must equal a fresh full render byte-for-byte. (The stamp is clipped
/// to the re-cleared bands — restamping an untouched band would re-blend the
/// translucent sprite over itself and over the glyphs above it.)
#[test]
fn settled_translucent_sprite_survives_text_edits_without_double_stamp() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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

/// §5.6 z-order at an arbitrary multi-row position: an UnderText sprite sits
/// under glyphs (a full-block cell stays pure theme fg) but OVER a legacy cat
/// quad (Phase B1 then B2, the GPU `FreeUnder`-after-`cat_over` slot); an
/// OverText sprite draws over EVERYTHING except the cursor — the glyphs, a
/// wdeco `Over` stamp, AND the additive glow/nova post-passes (the GPU
/// `FreeOver` slot: after the wdeco streams, immediately before the cursor).
#[test]
fn free_z_under_text_over_legacy_and_over_text() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    let blue = 0x0020_20C8u32;

    // A multi-row RED free sprite over cells (0,0)..(1,1); a single-band BLUE
    // legacy cat quad under it at row 0, cell 1.
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
    let _ = blue; // (blue must be fully covered — asserted via the red checks)

    // OverText: the same sprite flipped to the over-text slot covers the
    // glyph, a wdeco `Over` stamp, AND additive glow + nova light — over-text
    // means over everything except the cursor (the GPU `FreeOver` slot, after
    // the wdeco streams and the additive post-passes).
    let mut input_over = input.clone();
    input_over.cat_quads.clear();
    input_over.cat_atlas = None;
    input_over.free_sprites[0].z = FreeZ::OverText;
    // A wdeco stamp + aurora and nova light UNDER the sprite's footprint.
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
    });
    input_over.nova_add.push(GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: cw as u16,
        h: ch as u16,
        color: premul_rgb(0x00FF_C040, 200),
    });
    // Non-vacuous premise: WITHOUT the sprite, the stamp and the light paint.
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

/// §5.8 perf companion to `bench_render_row_under_sprites`: ONE tall free rect
/// spanning EVERY row of a 120×40 text frame, full render vs the no-sprite
/// baseline of the same frame. The GATED case is OPAQUE texels — the v1 cat
/// regime the pass-1c bench also measures, so the two numbers are
/// apples-to-apples (same bar, <10µs/row). A fully-TRANSLUCENT rect is also
/// measured and reported (no bar): its cost is the shared linear-light
/// `blend()` per pixel, which the legacy per-row slices pay identically — it
/// measures src-over, not the phase runner. Timing-sensitive — manual idiom:
///
/// ```sh
/// cargo test -p aterm-render --release --test free_composite \
///   bench_composite_free_tall_rect -- --ignored --nocapture
/// ```
#[test]
#[ignore = "perf gate (design §5.8): run manually in --release with --ignored --nocapture"]
fn bench_composite_free_tall_rect() {
    use std::time::Instant;
    let Some(mut rend) = renderer() else {
        panic!("bench needs a system monospace font");
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (40usize, 120usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(3);
    for r in 0..rows {
        term.process(format!("\x1b[{};1H{}", r + 1, &line[..cols]).as_bytes());
    }

    let base_input = term.cell_frame(rows, cols);
    let tall_h = (rows * ch) as u32;
    // Patterned atlases tall enough for the full-height 1:1 rect: opaque (the
    // gated cat regime) and translucent (the informational src-over worst case).
    let tall_atlas = |opaque: bool, version: u64| {
        let mut rgba = Vec::with_capacity((128 * tall_h * 4) as usize);
        for y in 0..tall_h {
            for x in 0..128u32 {
                rgba.extend_from_slice(&[
                    (x * 37 + y * 11) as u8,
                    (x * 5 + y * 53) as u8,
                    (x * 29 + y * 3) as u8,
                    if opaque {
                        255
                    } else {
                        (60 + (x * 3 + y) % 180) as u8
                    },
                ]);
            }
        }
        SceneAtlas {
            width: 128,
            height: tall_h,
            rgba,
            version,
        }
    };
    // ONE 80 px × full-grid-height rect — every row's band is composited.
    let sprite = free_1to1((4 * cw) as i32, 0, 80, tall_h as u16, [0, 0]);
    let mut opaque_input = base_input.clone();
    opaque_input.free_atlas = Some(Arc::new(tall_atlas(true, 1)));
    opaque_input.free_sprites = vec![sprite];
    let mut translucent_input = base_input.clone();
    translucent_input.free_atlas = Some(Arc::new(tall_atlas(false, 2)));
    translucent_input.free_sprites = vec![sprite];

    for _ in 0..4 {
        let _ = rend.render_input(&base_input);
        let _ = rend.render_input(&opaque_input);
        let _ = rend.render_input(&translucent_input);
    }
    let iters = 60usize;
    let mut t = [
        Vec::with_capacity(iters),
        Vec::with_capacity(iters),
        Vec::with_capacity(iters),
    ];
    for _ in 0..iters {
        for (i, input) in [&base_input, &opaque_input, &translucent_input]
            .iter()
            .enumerate()
        {
            let s = Instant::now();
            let _ = rend.render_input(input);
            t[i].push(s.elapsed());
        }
    }
    for v in &mut t {
        v.sort();
    }
    let (mb, mo, mt) = (t[0][iters / 2], t[1][iters / 2], t[2][iters / 2]);
    let per_row = |m: std::time::Duration| -> f64 {
        (m.as_nanos() as i128 - mb.as_nanos() as i128) as f64 / rows as f64
    };
    let (opaque_ns, translucent_ns) = (per_row(mo), per_row(mt));
    println!(
        "bench_composite_free_tall_rect: baseline full-frame median {mb:?}; ONE \
         {rows}-row free rect — OPAQUE {mo:?} ({:.2} us/row, the gated cat \
         regime), TRANSLUCENT {mt:?} ({:.2} us/row, informational: pure shared \
         src-over blend() cost) (120x40, {}x{} px cells, 80 px-wide rect)",
        opaque_ns / 1000.0,
        translucent_ns / 1000.0,
        cw,
        ch
    );
    assert!(
        opaque_ns < 10_000.0,
        "§5.8 gate: opaque free-composite row cost {opaque_ns:.0} ns/row >= 10 us/row"
    );
}
