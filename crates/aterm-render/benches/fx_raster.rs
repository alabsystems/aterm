// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU rasterization benchmarks for fx layers.

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
    scroll_under_card(&mut rend);
}

const ROWS: usize = 12;
const COLS: usize = 60;
const ITERS: usize = 400;

fn text_frame(t: &mut Terminal) -> RenderInput {
    t.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(2);
    for r in 0..ROWS {
        t.process(format!("\x1b[{};1H{}", r + 1, &line[r % 7..r % 7 + COLS]).as_bytes());
    }
    t.cell_frame(ROWS, COLS)
}

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
                ink: 2 * (run.value - 1),
            }));
        }
    }
    out
}

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
                1 => q.alpha = 0x60,
                2 => (q.color, q.alpha) = (0x0022_3344, 255),
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

fn scroll_under_card(rend: &mut Renderer) {
    const SROWS: usize = 40;
    const SCOLS: usize = 100;
    let (cw, ch) = rend.cell_size();
    let art = dithered_art();
    let (left, top) = (cw as i32, 28 * ch as i32);
    let (w, h) = ((90 * cw) as u16, (8 * ch) as u16);
    let kitty = FxItem::Sprite(FxSprite {
        x: 70 * cw as i32,
        y: top + ch as i32,
        dw: 4 * art.w(),
        dh: 3 * art.h(),
        flip_x: false,
        art,
        ink: 2,
        clip: PxRect::EVERYWHERE,
    });
    let rect = |x: i32, y: i32, w: u16, h: u16| FxItem::Rect(FxRect { x, y, w, h, ink: 0 });
    let card = |items: Vec<FxItem>| {
        FxLayer::new(
            7,
            Arc::new(FxContent::new(
                items,
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
        )
    };
    let ring = card(vec![
        rect(left, top, w, 2),
        rect(left, top + i32::from(h) - 2, w, 2),
        rect(left, top, 2, h),
        rect(left + i32::from(w) - 2, top, 2, h),
        kitty.clone(),
    ]);
    let filled = card(vec![rect(left, top, w, h), kitty]);
    let mut run = |layers: &[FxLayer], cached: bool| {
        let mut t = Terminal::new(SROWS as u16, SCOLS as u16);
        t.process(b"\x1b[?25l");
        for i in 0..SROWS {
            t.process(format!("warm {i}\r\n").as_bytes());
        }
        let mut wc = WindowCpu::new();
        let (mut times, mut marked) = (Vec::with_capacity(ITERS), 0);
        for i in 0..8 + ITERS {
            let pad = "the quick brown fox ".repeat(1 + i % 4);
            t.process(format!("streamed line {i} {pad}\r\n").as_bytes());
            let mut input = t.cell_frame(SROWS, SCOLS);
            input.fx_layers = layers.to_vec();
            let s = Instant::now();
            if cached {
                black_box(rend.render_input_cached(&mut wc, &input).pixels()[0]);
            } else {
                black_box(rend.render_input(&input).pixels[0]);
            }
            let spent = s.elapsed();
            if i < 8 {
                continue;
            }
            times.push(spent);
            if cached {
                assert!(
                    matches!(wc.last_damage(), DamageOutcome::Scroll { .. }),
                    "every streamed frame takes the blit ({:?})",
                    wc.last_damage()
                );
                marked += wc.dirty_rows().iter().filter(|&&d| d).count();
            }
        }
        (us(median(times)), marked as f64 / ITERS as f64)
    };
    let (blit, rows) = run(&[], true);
    let (full, _) = run(&[], false);
    println!(
        "scroll_under_card (one streamed line a frame, {SCOLS}x{SROWS} cells, an 8-row card): \
         no layer: the blit {blit:.1} us ({rows:.1} rows), a full repaint {full:.1} us"
    );
    for (what, layer) in [
        ("the card's ring and kitty", &ring),
        ("a filled box", &filled),
    ] {
        let (held, rows) = run(std::slice::from_ref(layer), true);
        let (full, _) = run(std::slice::from_ref(layer), false);
        println!(
            "scroll_under_card ({what}): the blit under it {held:.1} us ({rows:.1} rows), \
             a full repaint under it (B1a's refusal) {full:.1} us ({:.2}x)",
            full / held,
        );
    }
}
