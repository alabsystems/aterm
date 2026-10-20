// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the fx layers cost on the CPU (`docs/DESIGN-card-engine-2026-10-19.md`
//! §5 "Run-list CPU stamping", §4.9 "zero cost at rest"). A measurement, not a
//! gate: the numbers land in the design's §4.2 as-built note.
//!
//! * `sprite_vs_rects`: dithered Index8 arts drawn as sprite items (run-list
//!   stamping) against the SAME pixels drawn as one flat rect per texel run
//!   (the quad representation a producer would otherwise emit), both asserted
//!   byte-equal first, at an integer and a non-integer scale.
//! * `settled_at_rest`: the cached path's one-row text frame with and without
//!   settled layers (the same `Arc`s every frame) spanning that row.
//! * `glow_stream`: a `glow_under` frame of flat additive, translucent,
//!   opaque and gradient quads, the shared `draw_flat_add` loop the fx rects
//!   now share (`fill_glow_span`); its body uses only APIs older than the fx
//!   layers, so the same function measures a pre-fx tree.
//!
//! ```sh
//! targo --unverified bench -p aterm-render --bench fx_raster
//! ```

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxSprite, FxTexels,
    GlowQuad, PxRect, RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu, embedded_font};

fn main() {
    if cfg!(debug_assertions) {
        panic!("these are release-build numbers: run it with `targo --unverified bench`");
    }
    let mut rend = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("bundled monospace font builds");
    rend.set_pad(4);
    sprite_vs_rects(&mut rend, Scale::Integer);
    sprite_vs_rects(&mut rend, Scale::Between);
    settled_at_rest(&mut rend);
    glow_stream(&mut rend);
}

const ROWS: usize = 12;
const COLS: usize = 60;
const ITERS: usize = 400;

/// A text frame of `ROWS × COLS`, the cursor hidden.
fn text_frame(t: &mut Terminal) -> RenderInput {
    t.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(2);
    for r in 0..ROWS {
        t.process(format!("\x1b[{};1H{}", r + 1, &line[r % 7..r % 7 + COLS]).as_bytes());
    }
    t.cell_frame(ROWS, COLS)
}

/// Full-frame renders of each input, alternating so drift hits every side:
/// the median of each.
fn full_medians(rend: &mut Renderer, inputs: &[&RenderInput]) -> Vec<Duration> {
    for input in inputs {
        for _ in 0..4 {
            black_box(rend.render_input(input).pixels[0]);
        }
    }
    let mut times = vec![Vec::with_capacity(ITERS); inputs.len()];
    for _ in 0..ITERS {
        for (input, t) in inputs.iter().zip(&mut times) {
            let s = Instant::now();
            black_box(rend.render_input(input).pixels[0]);
            t.push(s.elapsed());
        }
    }
    times.into_iter().map(median).collect()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn us(d: Duration) -> f64 {
    d.as_nanos() as f64 / 1000.0
}

#[derive(Clone, Copy, Debug)]
enum Scale {
    /// `dw = 4·w`, `dh = 3·h`.
    Integer,
    /// `dw = 4·w − 5`, `dh = 3·h − 2`: every column map is a non-integer ratio.
    Between,
}

/// A 16 × 11 dithered art: an ellipse of indices 1..=3 in a checker-ish
/// dither, index 0 outside (the kitty's shape class).
fn dithered_art() -> Arc<FxArt> {
    let (w, h) = (16i32, 11i32);
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (2 * x - (w - 1), 2 * y - (h - 1));
            let inside = dx * dx * h * h + dy * dy * w * w <= w * w * h * h;
            data.push(if inside {
                [1u8, 2, 3, 2][((x / 2 + y) % 4) as usize]
            } else {
                0
            });
        }
    }
    Arc::new(FxArt::new(w as u16, h as u16, FxTexels::Index8, data).expect("sized"))
}

/// The sprite's pixels as flat rects: per art row, per texel run, the
/// destination columns and rows law F maps onto it (`⌈t·d / n⌉` edges).
fn as_rects(sp: &FxSprite) -> Vec<FxItem> {
    let art = &sp.art;
    let (aw, ah) = (u32::from(art.w()), u32::from(art.h()));
    let (dw, dh) = (u32::from(sp.dw), u32::from(sp.dh));
    let edge = |t: u32, d: u32, n: u32| (t * d).div_ceil(n) as i32;
    let mut out = Vec::new();
    for y in 0..art.h() as usize {
        let (r0, r1) = (edge(y as u32, dh, ah), edge(y as u32 + 1, dh, ah));
        if r0 >= r1 {
            continue;
        }
        for run in art.runs().row(y) {
            let (c0, c1) = (
                edge(u32::from(run.x), dw, aw),
                edge(u32::from(run.x + run.len), dw, aw),
            );
            if c0 >= c1 {
                continue;
            }
            out.push(FxItem::Rect(FxRect {
                x: sp.x + c0,
                y: sp.y + r0,
                w: (c1 - c0) as u16,
                h: (r1 - r0) as u16,
                // Index `k` paints entry `k − 1`; the rect palette doubles
                // every entry so each rect is flat.
                ink: 2 * (run.value - 1),
            }));
        }
    }
    out
}

/// The flat rects of a Window layer at offset 0 (over `as_rects`' doubled
/// palette, entry `2k` and `2k + 1` both `ink[k]`) as today's `glow_under`
/// quads: window pixels, one row band per quad (each rect cut at the band
/// edges), drawn by `draw_flat_add`.
fn as_glow_quads(rend: &Renderer, h: usize, rects: &[FxItem], ink: &[FxInk]) -> Vec<GlowQuad> {
    let mut out = Vec::new();
    for item in rects {
        let FxItem::Rect(r) = item else { continue };
        let paint = ink[r.ink as usize / 2];
        let (y0, y1) = (r.y as usize, r.y as usize + usize::from(r.h));
        for row in 0..ROWS {
            let (b0, b1) = rend.row_pixel_band(row, ROWS, h);
            let (q0, q1) = (y0.max(b0), y1.min(b1));
            if q0 < q1 {
                out.push(GlowQuad::flat(
                    row as u16,
                    r.x as u16,
                    q0 as u16,
                    r.w,
                    (q1 - q0) as u16,
                    paint.color,
                    paint.alpha,
                ));
            }
        }
    }
    out
}

/// RUN LISTS AGAINST RECTS AND QUADS: eight dithered sprites over the frame,
/// as sprite items, as their flat fx rects, and as today's band-cut
/// `glow_under` quads, all asserted byte-equal, then timed.
fn sprite_vs_rects(rend: &mut Renderer, scale: Scale) {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    let bare = text_frame(&mut t);
    let art = dithered_art();
    let (dw, dh) = match scale {
        Scale::Integer => (4 * art.w(), 3 * art.h()),
        Scale::Between => (4 * art.w() - 5, 3 * art.h() - 2),
    };
    let ink = [
        FxInk::add(0x0040_3020),
        FxInk::over(0x0030_2010, 0x80),
        FxInk::over(0x0011_2233, 255),
    ];
    let sprites: Vec<FxSprite> = (0..8)
        .map(|k| FxSprite {
            x: 8 + (k % 4) * 140,
            y: 6 + (k / 4) * 90,
            dw,
            dh,
            flip_x: false,
            art: art.clone(),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
        .collect();
    let rects: Vec<FxItem> = sprites.iter().flat_map(as_rects).collect();
    let rect_ink: Vec<FxInk> = ink.iter().flat_map(|&i| [i, i]).collect();
    let layer = |items: Vec<FxItem>, palette: Vec<FxInk>| {
        FxLayer::new(
            1,
            Arc::new(FxContent::new(items, palette)),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        )
    };
    let h = rend.render_input(&bare).height;
    let mut as_quads = bare.clone();
    as_quads.glow_under = as_glow_quads(rend, h, &rects, &ink);
    let (n_rects, n_quads) = (rects.len(), as_quads.glow_under.len());
    let mut as_sprites = bare.clone();
    as_sprites.fx_layers = vec![layer(
        sprites.into_iter().map(FxItem::Sprite).collect(),
        ink.to_vec(),
    )];
    let mut as_rects = bare.clone();
    as_rects.fx_layers = vec![layer(rects, rect_ink)];
    let a = rend.render_input(&as_sprites);
    assert!(
        a.pixels == rend.render_input(&as_rects).pixels,
        "the rects must be the sprites' pixels"
    );
    assert!(
        a.pixels == rend.render_input(&as_quads).pixels,
        "the quads must be the sprites' pixels"
    );
    assert!(
        a.pixels != rend.render_input(&bare).pixels,
        "the sprites must draw"
    );
    let m = full_medians(rend, &[&bare, &as_sprites, &as_rects, &as_quads]);
    let d = |i: usize| us(m[i]) - us(m[0]);
    println!(
        "sprite_vs_rects ({scale:?}, 8 x {}x{} art at {dw}x{dh}, {}x{} px frame): bare {:.1} us; \
         sprites +{:.1} us; the same pixels as {n_rects} fx rects +{:.1} us; as {n_quads} \
         glow_under quads +{:.1} us (sprites/rects {:.2}, sprites/quads {:.2})",
        art.w(),
        art.h(),
        a.width,
        a.height,
        us(m[0]),
        d(1),
        d(2),
        d(3),
        d(1) / d(2),
        d(1) / d(3),
    );
}

/// Cached-path medians of `inputs` drawn alternately, each frame checked to
/// take the damage path `want`.
fn cached_median(rend: &mut Renderer, inputs: &[RenderInput], want: DamageOutcome) -> Duration {
    let mut wc = WindowCpu::new();
    for input in inputs.iter().cycle().take(8) {
        black_box(rend.render_input_cached(&mut wc, input).pixels()[0]);
    }
    let mut times = Vec::with_capacity(ITERS);
    for input in inputs.iter().cycle().take(ITERS) {
        let s = Instant::now();
        black_box(rend.render_input_cached(&mut wc, input).pixels()[0]);
        times.push(s.elapsed());
        assert_eq!(wc.last_damage(), want, "the frame took another path");
    }
    median(times)
}

/// COST AT REST: the cached path with no layers against settled layers (the
/// same `Arc`s every frame): unchanged frames (gate hits: the identity
/// compare), and one-row text frames whose row the card spans (the row's
/// slices are restamped over the rebuilt band) or lies outside of (the
/// slicer skips every band).
fn settled_at_rest(rend: &mut Renderer) {
    let (cw, ch) = rend.cell_size();
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    let _ = text_frame(&mut t);
    let mut pair = |row: usize| {
        [0, 1].map(|k| {
            t.process(format!("\x1b[{};1Hframe {k} rewrites row {row}", row + 1).as_bytes());
            t.cell_frame(ROWS, COLS)
        })
    };
    let (inside, outside) = (pair(4), pair(10));
    let art = dithered_art();
    let card = FxLayer::new(
        1,
        Arc::new(FxContent::new(
            vec![
                FxItem::Rect(FxRect {
                    x: cw as i32,
                    y: 2 * ch as i32,
                    w: (40 * cw) as u16,
                    h: (5 * ch) as u16,
                    ink: 0,
                }),
                FxItem::Sprite(FxSprite {
                    x: 3 * cw as i32,
                    y: 3 * ch as i32,
                    dw: 4 * art.w(),
                    dh: 3 * art.h(),
                    flip_x: false,
                    art,
                    ink: 2,
                    clip: PxRect::EVERYWHERE,
                }),
            ],
            vec![
                FxInk::add(0x0020_1008),
                FxInk::add(0x0008_1020),
                FxInk::add(0x0040_3020),
                FxInk::add(0x0030_2010),
                FxInk::add(0x0010_2030),
            ],
        )),
        FxSlot::Under,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    let with_card = |pair: &[RenderInput; 2]| {
        pair.clone().map(|mut input| {
            input.fx_layers = vec![card.clone()];
            input
        })
    };
    let still = |input: &RenderInput| [input.clone(), input.clone()];
    let rows = [
        (
            "unchanged frame (gate hit)",
            still(&inside[0]),
            still(&with_card(&inside)[0]),
            DamageOutcome::GateHit,
        ),
        (
            "one-row frame, the card spans the row",
            inside.clone(),
            with_card(&inside),
            DamageOutcome::Rows,
        ),
        (
            "one-row frame, the row outside the card",
            outside.clone(),
            with_card(&outside),
            DamageOutcome::Rows,
        ),
    ];
    for (what, bare, held, want) in rows {
        let (a, b) = (
            cached_median(rend, &bare, want),
            cached_median(rend, &held, want),
        );
        println!(
            "settled_at_rest ({what}): no layers {:.2} us, a settled card layer {:.2} us \
             (+{:.2} us)",
            us(a),
            us(b),
            us(b) - us(a),
        );
    }
}

/// THE SHARED GLOW LOOP: a full frame with a `glow_under` stream of 24 quads
/// per row (a quarter each flat additive, flat translucent, flat opaque and
/// gradient), against the bare frame.
fn glow_stream(rend: &mut Renderer) {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    let bare = text_frame(&mut t);
    let frame = rend.render_input(&bare);
    let mut glow = bare.clone();
    for r in 0..ROWS {
        let (y0, y1) = rend.row_pixel_band(r, ROWS, frame.height);
        let y1 = y1.min(y0 + 40);
        for k in 0..24u16 {
            let x = 4 + k * 24;
            let (c, c2) = (0x0010_2030 + u32::from(k) * 0x0001_0101, 0x0030_1008);
            let mut q = GlowQuad::flat(r as u16, x, y0 as u16, 30, (y1 - y0) as u16, c, 0);
            match k % 4 {
                // Flat translucent (premultiplied: every channel under 0x60).
                1 => q.alpha = 0x60,
                // Flat opaque: the store arm.
                2 => (q.color, q.alpha) = (0x0022_3344, 255),
                // Flat additive, and the additive gradient set below.
                _ => {}
            }
            (q.color2, q.alpha2) = if k % 4 == 3 {
                (c2, 0)
            } else {
                (q.color, q.alpha)
            };
            glow.glow_under.push(q);
        }
    }
    let m = full_medians(rend, &[&bare, &glow]);
    println!(
        "glow_stream ({} quads, {}x{} px frame): bare {:.1} us, with the stream {:.1} us \
         (+{:.1} us)",
        glow.glow_under.len(),
        frame.width,
        frame.height,
        us(m[0]),
        us(m[1]),
        us(m[1]) - us(m[0]),
    );
}
