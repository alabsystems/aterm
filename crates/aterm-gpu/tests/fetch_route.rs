// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The framebuffer-fetch route against the split (`docs/DESIGN-card-engine-2026-10-19.md` §5):
//! on a Metal device whose fetch probe passed, a frame's Unorm-law light draws through its rows'
//! fetch twins inside the one sRGB pass, and lands the same bytes as the split and the CPU.

use std::sync::Arc;

use aterm_core::render::{
    DecoBlend, DecoGlyph, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect,
    FxSlot, FxSpace, FxSprite, FxTexels, PxRect, RenderInput, WordDecoration,
};
use aterm_gpu::{FetchProbe, FrameShape, UnormRoute};
use aterm_render::{FireMode, FirePatch, GlowQuad, HaloMode, RainHalo, Theme};

use crate::common::{ByteExact, assert_byte_exact, backends, block_text_term, max_channel_delta};

const ROWS: usize = 10;
const COLS: usize = 40;

/// Random scenes beside the fixed one, per theme.
const SEEDS: u64 = 12;

/// xorshift64: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn byte(&mut self) -> u8 {
        self.below(256) as u8
    }

    fn rgb(&mut self) -> u32 {
        (self.next() & 0x00FF_FFFF) as u32
    }
}

/// The streams each twin stands in for, by `FrameShape::STREAMS` label.
const TWIN_STREAMS: [(&str, &[&str]); 7] = [
    ("glow", &["glow_under", "glow_add", "nova_add"]),
    ("rain glow", &["glow_halo", "rain_add"]),
    ("rain glow over", &["glow_halo_over", "rain_add_over"]),
    ("fire add", &["fire_add"]),
    ("fire over", &["fire_over"]),
    ("deco add", &["wdeco_add"]),
    ("fx", &["fx_under", "fx_glow", "fx_over"]),
];

/// One row band's radial halo quads around `(cx, cy)`, clipped to the grid.
fn halo(
    out: &mut Vec<RainHalo>,
    (cx, cy): (usize, usize),
    (rx, ry): (u16, u16),
    color: u32,
    mode: HaloMode,
    (ch, grid_w, grid_h): (usize, usize, usize),
) {
    let (x0, x1) = (
        cx.saturating_sub(rx as usize),
        (cx + rx as usize + 1).min(grid_w),
    );
    let (mut y, y1) = (
        cy.saturating_sub(ry as usize),
        (cy + ry as usize + 1).min(grid_h),
    );
    while y < y1 && x0 < x1 {
        let band_end = ((y / ch + 1) * ch).min(y1);
        out.push(RainHalo {
            row: (y / ch) as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            color,
            cx: cx as u16,
            cy: cy as u16,
            rx,
            ry,
            mode,
        });
        y = band_end;
    }
}

/// One burn's row-band fire patches, standing on `base_y`.
fn burn(
    out: &mut Vec<FirePatch>,
    (x, w, base_y): (usize, usize, usize),
    (temp, strength, phase): (u8, u8, u32),
    mode: FireMode,
    (ch, grid_w): (usize, usize),
) {
    let peak_h = (2 * ch) as u16;
    let (x0, x1) = (x.min(grid_w), (x + w).min(grid_w));
    let mut y = base_y.saturating_sub(usize::from(peak_h) * 6 / 5 + 2);
    while y <= base_y && x0 < x1 {
        let band_end = ((y / ch + 1) * ch).min(base_y + 1);
        out.push(FirePatch {
            row: (y / ch) as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            base_y: base_y as u16,
            peak_h,
            phase,
            temp,
            strength,
            lean: 24,
            cov_cap: 220,
            cell_h: ch as u16,
            mode,
        });
        y = band_end;
    }
}

fn fx_layer(id: u64, items: Vec<FxItem>, palette: Vec<FxInk>, slot: FxSlot) -> FxLayer {
    FxLayer::new(
        id,
        Arc::new(FxContent::new(items, palette)),
        slot,
        FxSpace::Window,
        FxBlend::Exact,
    )
}

/// A frame whose light reaches every twin: aurora, under-glyph and nova glow (additive and
/// source-over), additive and veiling halos and rain heads, both fire modes, a sparkle word and
/// Exact fx layers at each slot, over block text; `rng` picks the colours and opacities.
fn scene(rng: &mut Rng, (cw, ch): (usize, usize)) -> RenderInput {
    let mut term = block_text_term(ROWS, COLS, 1..8);
    let mut input = term.cell_frame(ROWS, COLS);
    let (grid_w, grid_h) = (COLS * cw, ROWS * ch);
    let quad = |rng: &mut Rng, row: usize, col: usize, cells: usize| {
        let (a, a2) = if rng.below(2) == 0 {
            (0, 0)
        } else {
            (rng.byte().max(1), rng.byte().max(1))
        };
        GlowQuad {
            row: row as u16,
            x: (col * cw) as u16,
            y: (row * ch) as u16,
            w: (cells * cw) as u16,
            h: ch as u16,
            color: aterm_render::premul_rgb(rng.rgb(), a.max(160)),
            alpha: a,
            color2: aterm_render::premul_rgb(rng.rgb(), a2.max(160)),
            alpha2: a2,
        }
    };
    input.cursor_glow_add = vec![quad(rng, 1, 2, 6), quad(rng, 2, 4, 3)];
    input.glow_under = vec![quad(rng, 3, 10, 8)];
    input.nova_add = vec![quad(rng, 5, 20, 5), quad(rng, 0, 0, 2)];
    let geo = (ch, grid_w, grid_h);
    halo(
        &mut input.glow_halo,
        (6 * cw + cw / 2, 2 * ch + ch / 3),
        ((2 * cw) as u16, ch as u16),
        rng.rgb(),
        HaloMode::Add,
        geo,
    );
    halo(
        &mut input.glow_halo,
        (14 * cw, 4 * ch + ch / 2),
        ((2 * cw) as u16, (3 * ch / 2) as u16),
        rng.rgb(),
        HaloMode::Over,
        geo,
    );
    halo(
        &mut input.rain_add,
        (24 * cw, 6 * ch),
        (cw as u16, ch as u16),
        rng.rgb(),
        HaloMode::Add,
        geo,
    );
    halo(
        &mut input.rain_add,
        (30 * cw, 3 * ch),
        (cw as u16, ch as u16),
        rng.rgb(),
        HaloMode::Over,
        geo,
    );
    let phase = rng.next() as u32;
    burn(
        &mut input.fire_patch,
        (cw, 8 * cw, grid_h - ch / 2),
        (rng.byte(), rng.byte().max(64), phase),
        FireMode::Add,
        (ch, grid_w),
    );
    burn(
        &mut input.fire_patch,
        (20 * cw, 6 * cw, grid_h - 2),
        (rng.byte(), rng.byte().max(64), phase.wrapping_add(7777)),
        FireMode::Over,
        (ch, grid_w),
    );
    for col in 3..9 {
        input.word_decorations.push(WordDecoration {
            row: 4,
            col,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Star4,
            blend: DecoBlend::Add,
            color: rng.rgb(),
            alpha: rng.byte().max(1),
        });
    }
    let index = Arc::new(
        FxArt::new(
            5,
            3,
            FxTexels::Index8,
            vec![0u8, 1, 2, 3, 0, 4, 4, 0, 1, 2, 3, 2, 1, 0, 4],
        )
        .expect("sized"),
    );
    // A rect's two ends share a mode, so each layer's palette is all light or all ink.
    let palette = |rng: &mut Rng| -> Vec<FxInk> {
        let additive = rng.below(3) == 0;
        (0..6)
            .map(|_| {
                if additive {
                    FxInk::add(rng.rgb() & 0x003F_3F3F)
                } else {
                    FxInk::over(rng.rgb(), rng.byte().max(1))
                }
            })
            .collect()
    };
    let rect = |x, y, w, h, ink| FxItem::Rect(FxRect { x, y, w, h, ink });
    input.fx_layers = vec![
        fx_layer(1, vec![rect(3, 2, 177, 61, 0)], palette(rng), FxSlot::Under),
        fx_layer(
            2,
            vec![
                rect(20, 30, 40, 40, 4),
                FxItem::Ramps(FxRamps {
                    x: 70,
                    y: 10,
                    w: 63,
                    h: 2,
                    ink: 2,
                }),
                FxItem::Sprite(FxSprite {
                    x: 120,
                    y: 40,
                    dw: 37,
                    dh: 23,
                    flip_x: true,
                    art: Arc::clone(&index),
                    ink: 1,
                    clip: PxRect::EVERYWHERE,
                }),
            ],
            palette(rng),
            FxSlot::Glow,
        ),
        fx_layer(
            3,
            vec![rect(200, 60, 90, 50, 3), rect(240, 20, 30, 90, 4)],
            palette(rng),
            FxSlot::Over,
        ),
    ];
    input
}

/// The scene's frame on a fresh window under `route`, with the shape the frame took.
fn frame_under(
    gpu: &mut aterm_gpu::GpuRenderer,
    input: &RenderInput,
    route: UnormRoute,
) -> (aterm_render::Frame, FrameShape) {
    gpu.set_unorm_route(route);
    let mut win = aterm_gpu::WindowGpu::new();
    let frame = gpu.render_input(&mut win, input, None);
    (frame, gpu.frame_shape())
}

/// On a device whose probe passed, every twin's light lands the bytes the split lands and the
/// CPU's integer law, in one pass; elsewhere the route stays the split.
#[test]
fn the_fetch_route_lands_the_splits_bytes_in_one_pass() {
    let light = Theme {
        bg: 0x00F4_F1EA,
        fg: 0x0020_2020,
        ..Theme::default()
    };
    const TEST: &str = "the_fetch_route_lands_the_splits_bytes_in_one_pass";
    let exact = ByteExact::default();
    let mut drawn: Vec<&str> = Vec::new();
    for (label, theme) in [("dark", Theme::default()), ("light", light)] {
        let (mut cpu, mut gpu) = backends(18.0, theme);
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        let cell = cpu.cell_size();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for seed in 0..=SEEDS {
            let what = format!("{label} scene {seed}");
            let input = scene(&mut rng, cell);
            let (split, split_shape) = frame_under(&mut gpu, &input, UnormRoute::Split);
            let (fetch, fetch_shape) = frame_under(&mut gpu, &input, UnormRoute::Fetch);
            let probe = gpu.fetch_probe().expect("the frames above minted the arm");
            if exact.holds(&gpu, &what) {
                let cpu_frame = cpu.render_input(&input);
                for (route, frame) in [("split", &split), ("fetch", &fetch)] {
                    let delta = max_channel_delta(&cpu_frame.pixels, &frame.pixels);
                    assert_byte_exact(
                        TEST,
                        &what,
                        &gpu,
                        delta,
                        format_args!("{what}: the {route} route lands the CPU's bytes"),
                    );
                }
            }
            assert!(fetch.pixels == split.pixels, "{what}: the routes agree");
            assert_eq!(split_shape.route, UnormRoute::Split, "{what}");
            assert_eq!(
                (fetch_shape.instances, fetch_shape.unorm_draws),
                (split_shape.instances, split_shape.unorm_draws),
                "{what}: the route changes where the light draws, not what draws"
            );
            if probe == FetchProbe::Passed {
                assert_eq!(fetch_shape.route, UnormRoute::Fetch, "{what}");
                assert_eq!(fetch_shape.passes, 1, "{what}: one pass");
                assert!(split_shape.passes > 1, "{what}: the split pays passes");
            } else {
                assert_eq!(fetch_shape.route, UnormRoute::Split, "{what}: {probe:?}");
                assert_eq!(fetch_shape.passes, split_shape.passes, "{what}");
            }
            for (twin, streams) in TWIN_STREAMS {
                if fetch_shape
                    .streams()
                    .any(|(name, _)| streams.contains(&name))
                    && !drawn.contains(&twin)
                {
                    drawn.push(twin);
                }
            }
        }
    }
    let missing: Vec<&str> = TWIN_STREAMS
        .iter()
        .map(|(twin, _)| *twin)
        .filter(|twin| !drawn.contains(twin))
        .collect();
    assert!(missing.is_empty(), "the scenes never drew {missing:?}");
}

/// Route (1) of the pass economy: an Exact Under layer whose every pixel is opaque is stored at the
/// end of the base pass through the sRGB view and lands the bytes its Unorm-law `over` lands (the
/// CPU's), on both routes, and its frame pays fewer passes; a translucent one, or one a cursor glow
/// under the text sits on, keeps its own group.
#[test]
fn an_opaque_under_layer_stored_in_the_base_pass_lands_the_cpus_bytes() {
    const TEST: &str = "an_opaque_under_layer_stored_in_the_base_pass_lands_the_cpus_bytes";
    let light = Theme {
        bg: 0x00F4_F1EA,
        fg: 0x0020_2020,
        ..Theme::default()
    };
    let exact = ByteExact::default();
    let (mut cpu, mut gpu) = backends(18.0, light);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let (cw, ch) = cpu.cell_size();
    let index = Arc::new(
        FxArt::new(
            5,
            3,
            FxTexels::Index8,
            vec![0u8, 1, 2, 3, 0, 4, 4, 0, 1, 2, 3, 2, 1, 0, 4],
        )
        .expect("sized"),
    );
    let rect = |x, y, w, h, ink| FxItem::Rect(FxRect { x, y, w, h, ink });
    let under = |alpha: u8| {
        fx_layer(
            1,
            vec![
                rect(3, 2, 177, 61, 0),
                FxItem::Ramps(FxRamps {
                    x: 70,
                    y: 10,
                    w: 63,
                    h: 2,
                    ink: 2,
                }),
                FxItem::Sprite(FxSprite {
                    x: 120,
                    y: 40,
                    dw: 37,
                    dh: 23,
                    flip_x: true,
                    art: Arc::clone(&index),
                    ink: 1,
                    clip: PxRect::EVERYWHERE,
                }),
            ],
            (0..6u32)
                .map(|i| FxInk::over(0x0030_6090 + i * 0x0011_0503, alpha))
                .collect(),
            FxSlot::Under,
        )
    };
    let over = fx_layer(
        3,
        vec![rect(200, 60, 90, 50, 0)],
        vec![FxInk::add(0x0020_1008); 2],
        FxSlot::Over,
    );
    let base = block_text_term(ROWS, COLS, 1..8).cell_frame(ROWS, COLS);
    let case = |alpha: u8, glow_under: bool| {
        let mut input = base.clone();
        input.fx_layers = vec![under(alpha), over.clone()];
        if glow_under {
            input.glow_under = vec![GlowQuad {
                row: 2,
                x: (4 * cw) as u16,
                y: (2 * ch) as u16,
                w: (6 * cw) as u16,
                h: ch as u16,
                color: 0x0030_4050,
                alpha: 0,
                color2: 0x0030_4050,
                alpha2: 0,
            }];
        }
        input
    };
    let cases = [
        ("opaque", case(255, false), true),
        ("translucent", case(254, false), false),
        ("under a glow", case(255, true), false),
    ];
    #[allow(unused_mut, reason = "the oracle build adds an arm")]
    let mut arms = vec![("native", gpu)];
    #[cfg(wgpu_arm)]
    {
        let (_, mut oracle) = backends(18.0, light);
        oracle.set_bloom(false);
        oracle.set_shimmer(false);
        crate::common::disarm_for_oracle(&mut oracle);
        arms.push(("wgpu-oracle", oracle));
    }
    for (arm, mut gpu) in arms {
        let mut split_passes = Vec::new();
        for (what, input, stored) in &cases {
            let cpu_frame = cpu.render_input(input);
            for route in [UnormRoute::Split, UnormRoute::Fetch] {
                let what = format!("{arm}: {what} under, {route:?}");
                let (frame, shape) = frame_under(&mut gpu, input, route);
                assert!(shape.fx_instances > 0, "{what}: the layers drew");
                assert_eq!(shape.under_store, *stored, "{what}");
                if route == UnormRoute::Split {
                    split_passes.push(shape.passes);
                }
                if exact.holds(&gpu, &what) {
                    let delta = max_channel_delta(&cpu_frame.pixels, &frame.pixels);
                    assert_byte_exact(
                        TEST,
                        &what,
                        &gpu,
                        delta,
                        format_args!("{what}: the GPU lands the CPU's bytes"),
                    );
                }
            }
        }
        assert!(
            split_passes[0] < split_passes[1],
            "{arm}: the stored layer's split frame pays fewer passes: {split_passes:?}"
        );
    }
}
