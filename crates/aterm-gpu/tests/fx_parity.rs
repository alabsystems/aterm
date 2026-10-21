// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fx layers on the GPU against the CPU (`docs/DESIGN-card-engine-2026-10-19.md`
//! §4.6, §7 "B, the primitive"), on the renderer's own arm (Metal on macOS, GL on
//! Linux, wgpu elsewhere) and, in the `--features wgpu-oracle` build, on the wgpu
//! oracle too:
//!
//! * Exact layers are byte-exact (delta 0) over random frames of every item kind,
//!   flip, scale, clip, occluder and additive or source-over ink, through the
//!   present path frame after frame; items reaching past their palette
//!   (which write nothing) sit where a read past the layer's own palette
//!   would land on a neighbour's entries.
//! * Each slot draws between the CPU's neighbours (`NovaAdd` | `FxOver` |
//!   `RainAdd` and the rest), where the order shows in the bytes.
//! * A `bloom` layer feeds the GPU's bloom with no aurora on the frame, the
//!   CPU twin's halo within 2, whole on a scissored present; it takes the
//!   HDR crown (the wgpu stand-in).
//! * A frame past the art budget refuses the same sprites on every arm.
//! * A settled layer under a damaged frame is cut at the dirty rows and draws
//!   byte-equal to a full render (the band law; an ungated build double-adds).
//! * Linear layers hold FreeSprite's tolerance (2).
//! * Every texel byte and every palette byte survives the arm's sampling and
//!   output conversion (`fx_texel_bytes_survive_the_arm`).
//! * A fresh renderer (a rebuilt device) and dropped arm residency both
//!   refill the atlases and draw byte-equal.
//!
//! The Metal hazard drill (185 pipelined fx frames settle no reader) is
//! `renderer`'s `fx_hazard_tests`: it drives the present encode, which keeps
//! frames in flight, where a readback here waits each frame out.
//!
//! The control for the comparator: FreeSprite's float-UV path, drawing the
//! same art at the same odd scales, shows a nonzero delta where the fx
//! integer map shows none (`free_sprites_float_path_misses_where_fx_maps_exactly`).
//!
//! No GPU: COULD NOT RUN.

use std::sync::Arc;

use aterm_core::render::{
    FreeSampler, FreeSprite, FreeZ, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps,
    FxRect, FxSlot, FxSpace, FxSprite, FxTexels, PxRect, RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, SceneAtlas, Theme};

use crate::common::{self, max_channel_delta};

const ROWS: usize = 8;
const COLS: usize = 24;

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
        self.next() % n.max(1)
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
}

/// The renderers under test: the renderer's own arm and, in the oracle build
/// on macOS or Linux, the wgpu oracle, each beside its own CPU twin.
fn arms(theme: Theme) -> Vec<(&'static str, Renderer, aterm_gpu::GpuRenderer)> {
    let (cpu, gpu) = common::backends(18.0, theme);
    #[allow(unused_mut, reason = "the oracle build adds an arm")]
    let mut out = vec![("native", cpu, gpu)];
    #[cfg(all(wgpu_arm, any(target_os = "macos", target_os = "linux")))]
    {
        let (cpu, mut gpu) = common::backends(18.0, theme);
        common::disarm_for_oracle(&mut gpu);
        out.push(("wgpu-oracle", cpu, gpu));
    }
    out
}

/// After a frame: that it drew on the arm `arm` names (Metal is the native
/// arm on macOS and GL on Linux; the oracle is neither).
fn assert_arm(arm: &str, gpu: &aterm_gpu::GpuRenderer) {
    #[cfg(target_os = "macos")]
    assert_eq!(
        gpu.metal_render_armed(),
        arm == "native",
        "{arm}: the frame's arm"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        gpu.last_frame_on_gl(),
        arm == "native",
        "{arm}: the frame's arm"
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = (arm, gpu);
}

/// A base whose CPU and GPU pixels are byte-equal (procedural blocks and
/// background fills, no glyph coverage), so any delta is the fx layers'.
fn base_terminal() -> Terminal {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    t.process("\x1b[1;1H\x1b[44m      \x1b[0m██████  ▀▀▄▄".as_bytes());
    t.process("\x1b[3;3H\x1b[31m████████\x1b[0m\x1b[42m    \x1b[0m".as_bytes());
    t.process("\x1b[5;1H\x1b[45m                \x1b[0m▌▐█".as_bytes());
    t.process("\x1b[7;5H\x1b[33m▄▄▄▄▀▀▀▀\x1b[0m".as_bytes());
    t
}

fn input_with(t: &mut Terminal, layers: Vec<FxLayer>, occluders: Vec<PxRect>) -> RenderInput {
    let mut i = t.cell_frame(ROWS, COLS);
    i.fx_layers = layers;
    i.fx_occluders = occluders;
    i
}

fn layer(
    id: u64,
    items: Vec<FxItem>,
    palette: Vec<FxInk>,
    slot: FxSlot,
    blend: FxBlend,
) -> FxLayer {
    FxLayer::new(
        id,
        Arc::new(FxContent::new(items, palette)),
        slot,
        FxSpace::Window,
        blend,
    )
}

/// A random premultiplied art: additive texels (`a = 0` with colour), clear,
/// opaque and translucent ones.
fn premul_art(rng: &mut Rng) -> Arc<FxArt> {
    let (w, h) = (rng.range(1, 7) as u16, rng.range(1, 7) as u16);
    let mut data = Vec::new();
    for _ in 0..w * h {
        let a = [0u8, 0, 255, rng.range(1, 254) as u8][rng.below(4) as usize];
        let top = if a == 0 { 0x50 } else { u64::from(a) };
        let clear = rng.below(5) == 0;
        for _ in 0..3 {
            data.push(if clear { 0 } else { rng.below(top + 1) as u8 });
        }
        data.push(if clear { 0 } else { a });
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Premul, data).expect("sized"))
}

/// A straight-alpha art with opaque, translucent and clear texels.
fn straight_art(rng: &mut Rng) -> Arc<FxArt> {
    let (w, h) = (rng.range(1, 7) as u16, rng.range(1, 7) as u16);
    let mut data = Vec::new();
    for _ in 0..w * h {
        let a = [0u8, 255, rng.range(1, 254) as u8][rng.below(3) as usize];
        data.extend_from_slice(&[
            rng.below(256) as u8,
            rng.below(256) as u8,
            rng.below(256) as u8,
            a,
        ]);
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, data).expect("sized"))
}

fn index_art(rng: &mut Rng, max: u8) -> Arc<FxArt> {
    let (w, h) = (rng.range(1, 7) as u16, rng.range(1, 7) as u16);
    let data = (0..w * h)
        .map(|_| rng.below(u64::from(max) + 1) as u8)
        .collect::<Vec<_>>();
    Arc::new(FxArt::new(w, h, FxTexels::Index8, data).expect("sized"))
}

fn sprite_clip(rng: &mut Rng, x: i32, y: i32) -> PxRect {
    if rng.below(3) == 0 {
        PxRect::new(
            x + rng.range(-4, 12) as i32,
            y + rng.range(-4, 12) as i32,
            x + rng.range(8, 60) as i32,
            y + rng.range(8, 60) as i32,
        )
    } else {
        PxRect::EVERYWHERE
    }
}

/// A random layer over a `w × h` frame: an all-additive or all-over palette
/// (so a rect is one kind at both ends), rects, ramps and Index8 and Premul
/// sprites at any scale for an Exact layer, Straight sprites for a Linear
/// one; sometimes clipped, sometimes off the frame's edges, in either space,
/// at any slot its blend allows. One item in five reaches past the palette
/// (a rect's far end, ramps rows, Index8 indices, a Straight tint), which
/// pixel law F says writes nothing; the frame palette packs the other
/// layers' entries right after this one's, so an arm that reads past the
/// layer's own palette paints them.
fn random_layer(rng: &mut Rng, id: u64, linear: bool, w: i64, h: i64) -> FxLayer {
    let additive = rng.below(2) == 0;
    let palette: Vec<FxInk> = (0..12)
        .map(|_| {
            let a = if additive { 0 } else { rng.range(1, 255) as u8 };
            let top = if additive { 0x40 } else { u64::from(a) };
            let c = |rng: &mut Rng| rng.below(top + 1) as u32;
            FxInk {
                color: c(rng) << 16 | c(rng) << 8 | c(rng),
                alpha: a,
            }
        })
        .collect();
    let items = (0..rng.range(1, 5))
        .map(|_| {
            let (x, y) = (rng.range(-30, w) as i32, rng.range(-30, h) as i32);
            let past = rng.below(5) == 0;
            if linear {
                return FxItem::Sprite(FxSprite {
                    x,
                    y,
                    dw: rng.range(1, 60) as u16,
                    dh: rng.range(1, 50) as u16,
                    flip_x: rng.below(2) == 0,
                    art: straight_art(rng),
                    // 12: no tint entry.
                    ink: if past { 12 } else { rng.below(12) as u32 },
                    clip: sprite_clip(rng, x, y),
                });
            }
            match rng.below(4) {
                0 => FxItem::Rect(FxRect {
                    x,
                    y,
                    w: rng.range(1, 120) as u16,
                    h: rng.range(1, 50) as u16,
                    // 11: the far end is past the palette.
                    ink: if past { 11 } else { rng.below(11) as u32 },
                }),
                1 => FxItem::Ramps(FxRamps {
                    x,
                    y,
                    w: rng.range(1, 90) as u16,
                    h: rng.range(1, 6) as u16,
                    // From 5 on, the last rows run past the palette.
                    ink: if past { rng.range(5, 11) as u32 } else { 0 },
                }),
                2 => FxItem::Sprite(FxSprite {
                    x,
                    y,
                    dw: rng.range(1, 70) as u16,
                    dh: rng.range(1, 50) as u16,
                    flip_x: rng.below(2) == 0,
                    art: premul_art(rng),
                    ink: 0,
                    clip: sprite_clip(rng, x, y),
                }),
                _ => FxItem::Sprite(FxSprite {
                    x,
                    y,
                    dw: rng.range(1, 70) as u16,
                    dh: rng.range(1, 50) as u16,
                    flip_x: rng.below(2) == 0,
                    art: index_art(rng, 4),
                    // From 9 on, index 4 (and up) names an entry past the end.
                    ink: if past {
                        rng.range(9, 11) as u32
                    } else {
                        rng.below(8) as u32
                    },
                    clip: sprite_clip(rng, x, y),
                }),
            }
        })
        .collect();
    let slot = if linear {
        [FxSlot::Under, FxSlot::Over][rng.below(2) as usize]
    } else {
        [FxSlot::Under, FxSlot::Glow, FxSlot::Over][rng.below(3) as usize]
    };
    let space = [FxSpace::Window, FxSpace::Grid][rng.below(2) as usize];
    let blend = if linear {
        FxBlend::Linear
    } else {
        FxBlend::Exact
    };
    let mut l = FxLayer::new(
        id,
        Arc::new(FxContent::new_unchecked(items, palette)),
        slot,
        space,
        blend,
    );
    l.offset = (rng.range(-12, 12) as i32, rng.range(-12, 12) as i32);
    if rng.below(3) == 0 {
        l.clip = PxRect::new(
            rng.range(-10, 60) as i32,
            rng.range(-10, 60) as i32,
            rng.range(40, w) as i32,
            rng.range(40, h) as i32,
        );
    }
    l
}

/// Items pixel law F says write nothing or write their lerped ink: a rect
/// whose right end is past the palette, an Index8 texel past it, a ramps
/// stack whose last rows run off the palette, and a rect additive at one end
/// and source-over at the other.
fn malformed_layer() -> FxLayer {
    let art =
        Arc::new(FxArt::new(3, 2, FxTexels::Index8, vec![1u8, 2, 3, 3, 2, 1]).expect("sized"));
    let items = vec![
        // ink 3: entry 4 is past the end.
        FxItem::Rect(FxRect {
            x: 4,
            y: 4,
            w: 30,
            h: 9,
            ink: 3,
        }),
        // Index 3 names entry 2 + 3 − 1 = 4: past the end.
        FxItem::Sprite(FxSprite {
            x: 40,
            y: 20,
            dw: 13,
            dh: 11,
            flip_x: true,
            art,
            ink: 2,
            clip: PxRect::EVERYWHERE,
        }),
        // Rows 0..2 have both ends; row 2 needs entries 4 and 5.
        FxItem::Ramps(FxRamps {
            x: 90,
            y: 30,
            w: 41,
            h: 4,
            ink: 0,
        }),
        // Entry 1 additive, entry 2 over: one end each.
        FxItem::Rect(FxRect {
            x: 60,
            y: 50,
            w: 57,
            h: 30,
            ink: 1,
        }),
    ];
    let palette = vec![
        FxInk::add(0x0030_1020),
        FxInk::add(0x0010_4030),
        FxInk::over(0x0040_2010, 0x80),
        FxInk::over(0x0011_2233, 0xff),
    ];
    FxLayer::new(
        7,
        Arc::new(FxContent::new_unchecked(items, palette)),
        FxSlot::Over,
        FxSpace::Window,
        FxBlend::Exact,
    )
}

/// One frame of every kind at once: an additive gradient rect crossing three
/// bands, a source-over flat rect, ramps, an Index8 sprite at an odd scale
/// and flipped, a Premul sprite magnified and clipped, a sprite whose origin
/// sits far left of the frame, two occluders, and the malformed layer. The
/// malformed layer comes first, so its palette opens the frame palette and a
/// read past its end lands on the next layer's entries (last, a read past
/// it would find nothing to paint and pass unseen).
fn every_kind() -> (Vec<FxLayer>, Vec<PxRect>) {
    let index = Arc::new(
        FxArt::new(
            5,
            3,
            FxTexels::Index8,
            vec![0u8, 1, 2, 3, 0, 4, 4, 0, 1, 2, 3, 2, 1, 0, 4],
        )
        .expect("sized"),
    );
    let premul = Arc::new(
        FxArt::new(
            2,
            2,
            FxTexels::Rgba8Premul,
            vec![
                0x30, 0x20, 0x10, 0, // additive
                200, 100, 50, 255, // opaque
                40, 30, 20, 90, // translucent
                0, 0, 0, 0, // clear
            ],
        )
        .expect("sized"),
    );
    let palette = vec![
        FxInk::add(0x0020_1008),
        FxInk::add(0x0008_3060),
        FxInk::over(0x0060_3010, 0xa0),
        FxInk::over(0x0010_5070, 0xc0),
        FxInk::over(0x0080_4020, 0xff),
        FxInk::over(0x0002_0408, 0x10),
    ];
    let exact = |id: u64, items: Vec<FxItem>, slot: FxSlot| {
        layer(id, items, palette.clone(), slot, FxBlend::Exact)
    };
    let layers = vec![
        malformed_layer(),
        exact(
            1,
            vec![FxItem::Rect(FxRect {
                x: 3,
                y: 2,
                w: 177,
                h: 61,
                ink: 0,
            })],
            FxSlot::Under,
        ),
        exact(
            2,
            vec![
                FxItem::Rect(FxRect {
                    x: 20,
                    y: 30,
                    w: 40,
                    h: 40,
                    ink: 4,
                }),
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
            FxSlot::Glow,
        ),
        exact(
            3,
            vec![
                FxItem::Sprite(FxSprite {
                    x: 150,
                    y: 70,
                    dw: 31,
                    dh: 29,
                    flip_x: false,
                    art: Arc::clone(&premul),
                    ink: 0,
                    clip: PxRect::new(152, 75, 175, 95),
                }),
                FxItem::Sprite(FxSprite {
                    x: -40_000,
                    y: 100,
                    dw: 65_000,
                    dh: 7,
                    flip_x: false,
                    art: Arc::clone(&index),
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                }),
            ],
            FxSlot::Over,
        ),
    ];
    let occluders = vec![PxRect::new(30, 35, 50, 60), PxRect::new(100, 0, 110, 200)];
    (layers, occluders)
}

/// THE BYTE LAW: Exact fx layers draw byte-equal to the CPU on every arm,
/// over the every-kind frame and random frames through the present path on
/// one window (a frame whose layers changed repaints in full: the row diff
/// takes only settled layers, `aterm_render::compute_dirty_rows`).
#[test]
fn exact_fx_layers_are_byte_exact_on_the_gpu() {
    let theme = Theme::default();
    let mut t = base_terminal();
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let base = t.cell_frame(ROWS, COLS);
        let (cb, gb) = (
            cpu.render_input(&base),
            gpu.render_input(&mut win, &base, None),
        );
        assert_arm(arm, &gpu);
        assert_eq!(
            max_channel_delta(&cb.pixels, &gb.pixels),
            0,
            "{arm}: the base must be byte-exact so any delta is the fx layers'"
        );
        let (fw, fh) = (cb.width as i64, cb.height as i64);
        let (layers, occluders) = every_kind();
        let input = input_with(&mut t, layers, occluders);
        let (c, g) = (
            cpu.render_input(&input),
            gpu.render_input(&mut win, &input, None),
        );
        assert_ne!(c.pixels, cb.pixels, "{arm}: the every-kind frame paints");
        let shape = gpu.frame_shape();
        assert!(shape.fx_instances > 0, "{arm}: the GPU built fx instances");
        assert_eq!(
            max_channel_delta(&c.pixels, &g.pixels),
            0,
            "{arm}: the every-kind frame is byte-exact"
        );
        let mut rng = Rng(0x5eed_f00d_0dd5_cafe);
        let mut layers: Vec<FxLayer> = Vec::new();
        let mut next_id = 10;
        for frame in 0..48 {
            next_id += 1;
            // Keep, replace, add and drop layers; toggle occluders.
            match rng.below(4) {
                0 if !layers.is_empty() => {
                    let at = rng.below(layers.len() as u64) as usize;
                    layers.remove(at);
                }
                1 if !layers.is_empty() => {
                    let at = rng.below(layers.len() as u64) as usize;
                    layers[at] = random_layer(&mut rng, next_id, false, fw, fh);
                }
                _ if layers.len() < 4 => {
                    layers.push(random_layer(&mut rng, next_id, false, fw, fh))
                }
                _ => {}
            }
            let occluders = (0..rng.below(3))
                .map(|_| {
                    let (x, y) = (rng.range(0, fw) as i32, rng.range(0, fh) as i32);
                    PxRect::new(
                        x,
                        y,
                        x + rng.range(4, 80) as i32,
                        y + rng.range(4, 40) as i32,
                    )
                })
                .collect();
            let input = input_with(&mut t, layers.clone(), occluders);
            let (c, g) = (
                cpu.render_input(&input),
                gpu.present_input_readback(&mut win, &input),
            );
            let delta = max_channel_delta(&c.pixels, &g.pixels);
            assert_eq!(delta, 0, "{arm}: frame {frame} over {layers:#?}");
        }
    }
}

/// THE BAND LAW on the GPU: a settled additive layer spans every row while
/// only rows 2 and 5 change, so the damaged frame repaints the band 2..=5 under
/// one scissor and Loads rows 3 and 4. Its fx instances are cut at the dirty
/// rows alone, and the frame equals a full CPU render of the same input; an
/// ungated slice build re-adds the light on the Loaded rows.
#[test]
fn fx_dirty_band_restamps_only_its_band_on_the_gpu() {
    let theme = Theme::default();
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let mut t = base_terminal();
        let full = t.cell_frame(ROWS, COLS);
        let fresh = cpu.render_input(&full);
        let (fw, fh) = (fresh.width as i32, fresh.height as i32);
        let art =
            Arc::new(FxArt::new(3, 2, FxTexels::Index8, vec![1u8, 0, 2, 2, 1, 1]).expect("sized"));
        let settled = layer(
            1,
            vec![
                FxItem::Rect(FxRect {
                    x: 6,
                    y: 3,
                    w: (fw - 40) as u16,
                    h: (fh - 6) as u16,
                    ink: 0,
                }),
                FxItem::Sprite(FxSprite {
                    x: fw - 30,
                    y: 2,
                    dw: 23,
                    dh: (fh - 5) as u16,
                    flip_x: false,
                    art,
                    ink: 2,
                    clip: PxRect::EVERYWHERE,
                }),
            ],
            vec![
                FxInk::add(0x0018_2a30),
                FxInk::add(0x0030_1008),
                FxInk::add(0x0020_2020),
                FxInk::add(0x0008_3018),
            ],
            FxSlot::Under,
            FxBlend::Exact,
        );
        let a = input_with(&mut t, vec![settled.clone()], Vec::new());
        let _ = gpu.present_input_readback(&mut win, &a);
        let full_slices = gpu.frame_shape().fx_instances;
        t.process("\x1b[3;2H\x1b[36m███\x1b[0m\x1b[6;9H\x1b[35m▀▄\x1b[0m".as_bytes());
        let b = input_with(&mut t, vec![settled], Vec::new());
        let scissored = gpu.scissor_taken();
        let g = gpu.present_input_readback(&mut win, &b).pixels;
        assert_arm(arm, &gpu);
        assert_eq!(
            gpu.scissor_taken(),
            scissored + 1,
            "{arm}: frame B is a scissored repaint"
        );
        let c = cpu.render_input(&b);
        assert_eq!(
            max_channel_delta(&c.pixels, &g),
            0,
            "{arm}: the damaged frame equals a full render"
        );
        let band_slices = gpu.frame_shape().fx_instances;
        assert!(
            0 < band_slices && band_slices < full_slices,
            "{arm}: the damaged frame cuts its slices at the dirty rows \
             ({band_slices} of {full_slices})"
        );
    }
}

/// Linear layers (Straight sprites in the sRGB groups) hold FreeSprite's
/// linear-light tolerance: at most 2 per channel.
#[test]
fn linear_fx_layers_hold_the_free_sprite_tolerance() {
    let theme = Theme::default();
    let mut t = base_terminal();
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let base = t.cell_frame(ROWS, COLS);
        let cb = cpu.render_input(&base);
        let (fw, fh) = (cb.width as i64, cb.height as i64);
        let mut rng = Rng(0x0dd_ba11_5eed);
        let mut painted = 0;
        for frame in 0..24 {
            let layers = (0..rng.range(1, 3))
                .map(|id| random_layer(&mut rng, id as u64, true, fw, fh))
                .collect();
            let input = input_with(&mut t, layers, Vec::new());
            let (c, g) = (
                cpu.render_input(&input),
                gpu.render_input(&mut win, &input, None),
            );
            assert_arm(arm, &gpu);
            painted += usize::from(c.pixels != cb.pixels);
            let delta = max_channel_delta(&c.pixels, &g.pixels);
            assert!(delta <= 2, "{arm}: frame {frame}: Linear delta {delta} > 2");
        }
        assert!(
            painted >= 12,
            "{arm}: the Linear frames paint ({painted} of 24)"
        );
    }
}

/// THE BYTE RECOVERY per arm: all 256 values of every channel through the
/// Rgba8 atlas (a Premul art), every index through the R8 atlas and every
/// palette byte (an Index8 art over 255 entries), drawn 1:1 and opaque,
/// reach the frame unchanged. The shaders read a texel as normalized floats
/// and recover its byte with `rint`. On Apple GPUs the Unorm-to-float read is
/// exact, so a truncating recovery passes here too (measured, Metal and wgpu
/// on Metal); the mutation this test catches on every arm is a palette word
/// decoded in the wrong byte order.
#[test]
fn fx_texel_bytes_survive_the_arm() {
    let theme = Theme::default();
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    // Premul: texel k carries (k, 255 − k, 37k mod 256), opaque.
    let premul: Vec<u8> = (0..256u32)
        .flat_map(|k| [k as u8, (255 - k) as u8, (k * 37) as u8, 255])
        .collect();
    let premul = Arc::new(FxArt::new(16, 16, FxTexels::Rgba8Premul, premul).expect("sized"));
    // Index8: texel k names entry k − 1 (0 is clear); entry j is opaque
    // (j, 3j, 255 − j).
    let index: Vec<u8> = (0..256u32).map(|k| k as u8).collect();
    let index = Arc::new(FxArt::new(16, 16, FxTexels::Index8, index).expect("sized"));
    let palette: Vec<FxInk> = (0..255u32)
        .map(|j| FxInk::over((j << 16) | (((3 * j) & 0xff) << 8) | (255 - j), 255))
        .collect();
    let sprite = |x: i32, art: &Arc<FxArt>| {
        FxItem::Sprite(FxSprite {
            x,
            y: 4,
            dw: 16,
            dh: 16,
            flip_x: false,
            art: Arc::clone(art),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
    };
    let layers = vec![layer(
        1,
        vec![sprite(4, &premul), sprite(30, &index)],
        palette,
        FxSlot::Over,
        FxBlend::Exact,
    )];
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let input = input_with(&mut t, layers.clone(), Vec::new());
        let (c, g) = (
            cpu.render_input(&input),
            gpu.render_input(&mut win, &input, None),
        );
        assert_arm(arm, &gpu);
        let at =
            |f: &aterm_render::Frame, x: usize, y: usize| f.pixels[y * f.width + x] & 0x00ff_ffff;
        for k in 0..256u32 {
            let (x, y) = ((k % 16) as usize, (k / 16) as usize + 4);
            let want = (k << 16) | ((255 - k) << 8) | ((k * 37) & 0xff);
            assert_eq!(at(&c, 4 + x, y), want, "cpu premul texel {k}");
            assert_eq!(at(&g, 4 + x, y), want, "{arm}: premul texel {k}");
            if k > 0 {
                let j = k - 1;
                let want = (j << 16) | (((3 * j) & 0xff) << 8) | (255 - j);
                assert_eq!(at(&c, 30 + x, y), want, "cpu palette entry {j}");
                assert_eq!(at(&g, 30 + x, y), want, "{arm}: palette entry {j}");
            }
        }
        assert_eq!(max_channel_delta(&c.pixels, &g.pixels), 0, "{arm}");
    }
}

/// THE COMPARATOR'S CONTROL: the same art through FreeSprite's float path
/// (a float UV picks the texel, a float linear-light blend composites it)
/// shows a nonzero delta at odd scales, where the fx integer path (integer
/// texel map, `over_premul` on the Unorm view) shows none. So the zero
/// deltas above are the law's, not a blind comparator's. The art mixes
/// opaque and translucent texels: on Metal the tie-biased float UV picks the
/// CPU's texel at every odd scale measured (opaque texels read delta 0), and
/// the float path's delta is its blend's, within FreeSprite's declared 2.
#[test]
fn free_sprites_float_path_misses_where_fx_maps_exactly() {
    let theme = Theme::default();
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    let (aw, ah) = (61u32, 47u32);
    let (mut straight, mut premul) = (Vec::new(), Vec::new());
    for y in 0..ah {
        for x in 0..aw {
            let c = [(x * 4) as u8, (y * 5) as u8, ((x * 7 + y * 3) % 256) as u8];
            let a = if (x + y) % 3 == 0 {
                255
            } else {
                (40 + (x * 13 + y * 29) % 190) as u8
            };
            straight.extend_from_slice(&[c[0], c[1], c[2], a]);
            let pm = |v: u8| ((u32::from(v) * u32::from(a) + 127) / 255) as u8;
            premul.extend_from_slice(&[pm(c[0]), pm(c[1]), pm(c[2]), a]);
        }
    }
    let atlas = Arc::new(SceneAtlas {
        width: aw,
        height: ah,
        rgba: straight,
        version: 1,
    });
    let art =
        Arc::new(FxArt::new(aw as u16, ah as u16, FxTexels::Rgba8Premul, premul).expect("sized"));
    let scales = [
        (7u16, 5u16),
        (23, 17),
        (37, 29),
        (51, 41),
        (97, 73),
        (131, 101),
        (181, 37),
    ];
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let (mut free_worst, mut fx_worst) = (0, 0);
        for (i, &(dw, dh)) in scales.iter().enumerate() {
            let flip = i % 2 == 1;
            let mut free = t.cell_frame(ROWS, COLS);
            free.free_atlas = Some(Arc::clone(&atlas));
            free.free_sprites = vec![FreeSprite {
                x: 4,
                y: 6,
                w: dw,
                h: dh,
                ax: 0,
                ay: 0,
                aw: aw as u16,
                ah: ah as u16,
                tint: 0x00FF_FFFF,
                alpha: 255,
                flip_x: flip,
                z: FreeZ::OverText,
                sampler: FreeSampler::Nearest,
            }];
            let (c, g) = (
                cpu.render_input(&free),
                gpu.render_input(&mut win, &free, None),
            );
            free_worst = free_worst.max(max_channel_delta(&c.pixels, &g.pixels));
            let fx = input_with(
                &mut t,
                vec![layer(
                    1,
                    vec![FxItem::Sprite(FxSprite {
                        x: 4,
                        y: 6,
                        dw,
                        dh,
                        flip_x: flip,
                        art: Arc::clone(&art),
                        ink: 0,
                        clip: PxRect::EVERYWHERE,
                    })],
                    Vec::new(),
                    FxSlot::Over,
                    FxBlend::Exact,
                )],
                Vec::new(),
            );
            let (c, g) = (cpu.render_input(&fx), gpu.render_input(&mut win, &fx, None));
            assert_arm(arm, &gpu);
            fx_worst = fx_worst.max(max_channel_delta(&c.pixels, &g.pixels));
        }
        eprintln!("{arm}: odd scales: FreeSprite delta {free_worst}, fx delta {fx_worst}");
        assert!(
            free_worst > 0,
            "{arm}: the control: FreeSprite's float path differs"
        );
        assert!(free_worst <= 2, "{arm}: FreeSprite holds its declared 2");
        assert_eq!(fx_worst, 0, "{arm}: the fx integer path is byte-exact");
    }
}

/// THE PASS LAW (design §4.5): an fx layer draws inside a group of its own
/// format, so a frame drawing one opens exactly the passes of the same frame
/// drawing the effect whose slot it shares: Exact Under as a `glow_under`
/// quad, Exact Glow and Over as a `nova_add` quad, Linear Under and Over as a
/// FreeSprite under and over the text.
#[test]
fn fx_layers_open_the_passes_their_slots_already_open() {
    let theme = Theme::default();
    let mut t = base_terminal();
    let quad = aterm_core::render::GlowQuad {
        row: 2,
        x: 20,
        y: 30,
        w: 90,
        h: 40,
        color: 0x0030_2010,
        alpha: 0,
        color2: 0x0030_2010,
        alpha2: 0,
    };
    let rect = |slot, blend| {
        let linear = matches!(blend, FxBlend::Linear);
        let item = if linear {
            FxItem::Sprite(FxSprite {
                x: 20,
                y: 30,
                dw: 90,
                dh: 40,
                flip_x: false,
                art: Arc::new(
                    FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![200, 90, 40, 160])
                        .expect("sized"),
                ),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })
        } else {
            FxItem::Rect(FxRect {
                x: 20,
                y: 30,
                w: 90,
                h: 40,
                ink: 0,
            })
        };
        let ink = if linear {
            FxInk::over(0x00ff_ffff, 0xff)
        } else {
            FxInk::add(0x0030_2010)
        };
        layer(1, vec![item], vec![ink, ink], slot, blend)
    };
    let atlas = Arc::new(SceneAtlas {
        width: 1,
        height: 1,
        rgba: vec![200, 90, 40, 160],
        version: 1,
    });
    let free = |z| FreeSprite {
        x: 20,
        y: 30,
        w: 90,
        h: 40,
        ax: 0,
        ay: 0,
        aw: 1,
        ah: 1,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z,
        sampler: FreeSampler::Nearest,
    };
    let passes = |gpu: &mut aterm_gpu::GpuRenderer, input: &RenderInput| {
        let mut win = aterm_gpu::WindowGpu::new();
        let _ = gpu.render_input(&mut win, input, None);
        gpu.frame_shape().passes
    };
    for (arm, _cpu, mut gpu) in arms(theme) {
        let cases: [(&str, FxSlot, FxBlend); 5] = [
            ("exact under", FxSlot::Under, FxBlend::Exact),
            ("exact glow", FxSlot::Glow, FxBlend::Exact),
            ("exact over", FxSlot::Over, FxBlend::Exact),
            ("linear under", FxSlot::Under, FxBlend::Linear),
            ("linear over", FxSlot::Over, FxBlend::Linear),
        ];
        for (name, slot, blend) in cases {
            let mut legacy = t.cell_frame(ROWS, COLS);
            match (slot, blend) {
                (FxSlot::Under, FxBlend::Exact) => legacy.glow_under.push(quad),
                (_, FxBlend::Exact) => legacy.nova_add.push(quad),
                (FxSlot::Under, _) => {
                    legacy.free_atlas = Some(Arc::clone(&atlas));
                    legacy.free_sprites.push(free(FreeZ::UnderText));
                }
                _ => {
                    legacy.free_atlas = Some(Arc::clone(&atlas));
                    legacy.free_sprites.push(free(FreeZ::OverText));
                }
            }
            let fx = input_with(&mut t, vec![rect(slot, blend)], Vec::new());
            let (want, got) = (passes(&mut gpu, &legacy), passes(&mut gpu, &fx));
            assert_arm(arm, &gpu);
            assert!(gpu.frame_shape().fx_instances > 0, "{arm} {name}: fx drew");
            assert_eq!(got, want, "{arm} {name}: the fx frame's passes");
        }
    }
}

/// `fx_prefetch` makes arts resident without drawing (design §4.6, eager
/// residency): a prefetch-only frame uploads the art's texels and draws
/// byte-equal to the frame without it; the same prefetch again uploads
/// nothing; and the first frame that draws the art uploads only its instances
/// and palette, where a renderer with no prefetch also uploads the texels.
#[test]
fn fx_prefetch_makes_arts_resident_without_drawing() {
    let theme = Theme::default();
    let mut t = base_terminal();
    let mut rng = Rng(0x0f0f_1234_5678_9abc);
    let (premul, index) = (premul_art(&mut rng), index_art(&mut rng, 3));
    let texels = |a: &FxArt| u64::from(a.w()) * u64::from(a.h());
    let art_bytes = texels(&premul) * 4 + texels(&index);
    let palette = vec![
        FxInk::add(0x0020_1008),
        FxInk::over(0x0040_3020, 0xc0),
        FxInk::over(0x0010_2030, 0xff),
    ];
    let drawn = layer(
        1,
        vec![
            FxItem::Sprite(FxSprite {
                x: 12,
                y: 9,
                dw: 37,
                dh: 23,
                flip_x: false,
                art: Arc::clone(&premul),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            }),
            FxItem::Sprite(FxSprite {
                x: 70,
                y: 40,
                dw: 29,
                dh: 31,
                flip_x: true,
                art: Arc::clone(&index),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            }),
        ],
        palette.clone(),
        FxSlot::Over,
        FxBlend::Exact,
    );
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let base = t.cell_frame(ROWS, COLS);
        let want = cpu.render_input(&base).pixels;
        let mut hint = t.cell_frame(ROWS, COLS);
        hint.fx_prefetch = vec![Arc::clone(&premul), Arc::clone(&index)];
        let got = gpu.render_input(&mut win, &hint, None).pixels;
        assert_arm(arm, &gpu);
        let shape = gpu.frame_shape();
        assert_eq!(shape.fx_instances, 0, "{arm}: a prefetch draws nothing");
        assert_eq!(shape.fx_upload_bytes, art_bytes, "{arm}: the arts' texels");
        assert_eq!(max_channel_delta(&want, &got), 0, "{arm}: no pixel moved");
        let _ = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &hint, None);
        assert_eq!(
            gpu.frame_shape().fx_upload_bytes,
            0,
            "{arm}: resident: nothing"
        );
        let mut draw = input_with(&mut t, vec![drawn.clone()], Vec::new());
        draw.fx_prefetch = hint.fx_prefetch.clone();
        let c = cpu.render_input(&draw).pixels;
        let g = gpu.render_input(&mut win, &draw, None).pixels;
        let shape = gpu.frame_shape();
        assert!(shape.fx_instances > 0, "{arm}: the layer draws");
        assert_eq!(
            shape.fx_upload_bytes,
            u64::from(shape.fx_instances) * 32 + 4 * palette.len() as u64,
            "{arm}: a prefetched art uploads no texel when drawn"
        );
        assert_eq!(max_channel_delta(&c, &g), 0, "{arm}: and draws byte-exact");
    }
    // Control: without the prefetch the drawing frame pays the texels.
    for (arm, _cpu, mut gpu) in arms(theme) {
        let draw = input_with(&mut t, vec![drawn.clone()], Vec::new());
        let _ = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &draw, None);
        let shape = gpu.frame_shape();
        assert_eq!(
            shape.fx_upload_bytes,
            u64::from(shape.fx_instances) * 32 + 4 * palette.len() as u64 + art_bytes,
            "{arm}: an art first met when drawn uploads with its frame"
        );
    }
}

/// A rebuilt device holds no fx residency: a fresh renderer handed the same
/// layer `Arc`s, and a renderer whose arm residency was dropped mid-life,
/// re-mint the atlases, refill them from the atlas log and draw byte-equal
/// to the CPU and to the frame before.
#[test]
fn fx_residency_refills_after_a_device_rebuild() {
    let theme = Theme::default();
    let mut t = base_terminal();
    let (layers, occluders) = every_kind();
    let input = input_with(&mut t, layers, occluders);
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let want = cpu.render_input(&input).pixels;
        let first = gpu.render_input(&mut win, &input, None).pixels;
        assert_arm(arm, &gpu);
        assert_eq!(max_channel_delta(&want, &first), 0, "{arm}: first frame");
        // Dropped residency: the next frame that encodes (a new window, so
        // no gate hit) refills.
        gpu.drop_fx_arm_residency_for_test();
        let mut win2 = aterm_gpu::WindowGpu::new();
        let again = gpu.render_input(&mut win2, &input, None).pixels;
        assert!(
            gpu.frame_shape().fx_upload_bytes > 0,
            "{arm}: the dropped atlases were refilled"
        );
        assert_eq!(
            again, first,
            "{arm}: refilled residency draws the same bytes"
        );
    }
    // A rebuilt stack (what the frontend does on device loss): a fresh
    // renderer on the same layer `Arc`s.
    let mut fresh = common::independent_gpu(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (mut cpu, _) = common::backends(18.0, theme);
    let want = cpu.render_input(&input).pixels;
    let got = fresh.render_input(&mut win, &input, None).pixels;
    assert_eq!(
        max_channel_delta(&want, &got),
        0,
        "a fresh renderer refills"
    );
}

/// Whether a neighbour stream changes a pixel the fx layer paints: some pixel
/// `solo` (the layer alone) moved from `base` (no layer) and `with` (the layer
/// and the neighbour) moved from `solo`. Only there does the order show.
fn shares_pixels(base: &[u32], solo: &[u32], with: &[u32]) -> bool {
    base.iter()
        .zip(solo)
        .zip(with)
        .any(|((b, s), w)| b != s && s != w)
}

/// THE SLOT LAW (design §4.5): each slot draws between the same neighbours on
/// the GPU as on the CPU. An Exact slot's source-over veil covers the frame
/// between an additive stream drawn just before it and one drawn just after
/// it, where light-then-veil and veil-then-light are different bytes:
/// `glow_under` | Under | `fire_patch`, `cursor_glow_add` | Glow |
/// `glow_halo`, `nova_add` | Over | `rain_add`. A Linear slot's translucent
/// sprite lies over the FreeSprite of its z and under what follows it: the
/// cursor trail's opaque cells for Under, the cursor block for Over. Every
/// neighbour is checked to change the frame, Exact frames are byte-exact and
/// Linear ones within FreeSprite's 2, so a stream drawn on the wrong side of
/// its slot shows (swapping the `NovaAdd` and `FxOver` pushes fails here).
#[test]
fn fx_slots_draw_between_their_neighbours_on_the_gpu() {
    use aterm_render::{FireMode, FirePatch, GlowQuad, HaloMode, RainHalo};
    let theme = Theme::default();
    let mut t = base_terminal();
    let exact = common::ByteExact::default();
    for (arm, mut cpu, mut gpu) in arms(theme) {
        // The bloom is GPU-only light over `cursor_glow_add` (the bloom test
        // below covers it); off, the frames compare the slot order alone.
        gpu.set_bloom(false);
        let mut win = aterm_gpu::WindowGpu::new();
        let (cw, ch) = cpu.cell_size();
        let base = t.cell_frame(ROWS, COLS);
        let cb = cpu.render_input(&base);
        let (fw, fh) = (cb.width, cb.height);
        let light = move |row: usize, x: usize, w: usize| GlowQuad {
            row: row as u16,
            x: x as u16,
            y: (row * ch) as u16,
            w: w as u16,
            h: ch as u16,
            color: 0x0050_3828,
            alpha: 0,
            color2: 0x0050_3828,
            alpha2: 0,
        };
        let halo = move |row: usize| RainHalo {
            row: row as u16,
            x: (3 * cw) as u16,
            y: (row * ch) as u16,
            w: (6 * cw) as u16,
            h: ch as u16,
            color: 0x0040_c060,
            cx: (6 * cw) as u16,
            cy: (row * ch + ch / 2) as u16,
            rx: (3 * cw) as u16,
            ry: (ch / 2).max(1) as u16,
            mode: HaloMode::Add,
        };
        let fire = (0..3)
            .map(|row| FirePatch {
                row,
                x: (2 * cw) as u16,
                y: (usize::from(row) * ch) as u16,
                w: (8 * cw) as u16,
                h: ch as u16,
                base_y: (3 * ch) as u16,
                peak_h: (3 * ch) as u16,
                phase: 99_999,
                temp: 230,
                strength: 240,
                lean: -48,
                cov_cap: 200,
                cell_h: ch as u16,
                mode: FireMode::Add,
            })
            .collect::<Vec<_>>();
        let veil = |slot| {
            layer(
                1,
                vec![FxItem::Rect(FxRect {
                    x: 0,
                    y: 0,
                    w: fw as u16,
                    h: fh as u16,
                    ink: 0,
                })],
                vec![FxInk::over(0x0020_3040, 0xc0); 2],
                slot,
                FxBlend::Exact,
            )
        };
        type Fill = Box<dyn Fn(&mut RenderInput)>;
        let cases: Vec<(&str, FxSlot, Fill, Fill)> = vec![
            (
                "under",
                FxSlot::Under,
                Box::new(move |i| i.glow_under.push(light(2, 2 * cw, 9 * cw))),
                Box::new({
                    let fire = fire.clone();
                    move |i| i.fire_patch.clone_from(&fire)
                }),
            ),
            (
                "glow",
                FxSlot::Glow,
                Box::new(move |i| i.cursor_glow_add.push(light(4, 3 * cw, 9 * cw))),
                Box::new(move |i| i.glow_halo.push(halo(4))),
            ),
            (
                "over",
                FxSlot::Over,
                Box::new(move |i| i.nova_add.push(light(5, cw, 9 * cw))),
                Box::new(move |i| i.rain_add.push(halo(5))),
            ),
        ];
        for (name, slot, before, after) in &cases {
            let alone = input_with(&mut t, vec![veil(*slot)], Vec::new());
            let mut both = alone.clone();
            before(&mut both);
            after(&mut both);
            let solo = cpu.render_input(&alone).pixels;
            for (side, fill) in [("before", before), ("after", after)] {
                let mut one = alone.clone();
                fill(&mut one);
                assert!(
                    shares_pixels(&cb.pixels, &solo, &cpu.render_input(&one).pixels),
                    "{arm} {name}: the stream {side} the slot paints where the veil does"
                );
            }
            let (c, g) = (
                cpu.render_input(&both),
                gpu.render_input(&mut win, &both, None),
            );
            assert_arm(arm, &gpu);
            assert!(gpu.frame_shape().fx_instances > 0, "{arm} {name}: fx drew");
            if exact.holds(&gpu, "the Exact slot frames") {
                assert_eq!(
                    max_channel_delta(&c.pixels, &g.pixels),
                    0,
                    "{arm} {name}: the slot's neighbours draw on the CPU's sides"
                );
            }
        }
        // Linear: a translucent FreeSprite of the slot's z first, the fx
        // sprite over it, then the trail's opaque cells (Under) or the cursor
        // block (Over) over both.
        let atlas = Arc::new(SceneAtlas {
            width: 1,
            height: 1,
            rgba: vec![220, 40, 30, 150],
            version: 1,
        });
        let art = Arc::new(
            FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![30, 60, 230, 170]).expect("sized"),
        );
        let (x, y, w, h) = (cw as i32, ch as i32, (10 * cw) as u16, (4 * ch) as u16);
        for (name, slot, z) in [
            ("linear under", FxSlot::Under, FreeZ::UnderText),
            ("linear over", FxSlot::Over, FreeZ::OverText),
        ] {
            let sprite = layer(
                2,
                vec![FxItem::Sprite(FxSprite {
                    x: x + (cw / 2) as i32,
                    y: y + (ch / 2) as i32,
                    dw: w,
                    dh: h,
                    flip_x: false,
                    art: Arc::clone(&art),
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                })],
                vec![FxInk::over(0x00ff_ffff, 0xff)],
                slot,
                FxBlend::Linear,
            );
            let alone = input_with(&mut t, vec![sprite], Vec::new());
            let solo = cpu.render_input(&alone).pixels;
            let before = |i: &mut RenderInput| {
                i.free_atlas = Some(Arc::clone(&atlas));
                i.free_sprites = vec![FreeSprite {
                    x,
                    y,
                    w,
                    h,
                    ax: 0,
                    ay: 0,
                    aw: 1,
                    ah: 1,
                    tint: 0x00FF_FFFF,
                    alpha: 255,
                    flip_x: false,
                    z,
                    sampler: FreeSampler::Nearest,
                }];
            };
            let after = |i: &mut RenderInput| {
                if matches!(slot, FxSlot::Under) {
                    // Row 3 is blank (row 2's blocks would hide both).
                    i.cursor_trail = (2..6)
                        .map(|col| aterm_core::render::TrailCell {
                            row: 3,
                            col,
                            alpha: 200,
                        })
                        .collect();
                    i.cursor_trail_color = 0x00e0_c040;
                } else {
                    i.cursor_visible = true;
                    i.cursor_row = 1;
                    i.cursor_col = 4;
                }
            };
            let mut both = alone.clone();
            before(&mut both);
            after(&mut both);
            for (side, fill) in [
                ("before", &before as &dyn Fn(&mut RenderInput)),
                ("after", &after),
            ] {
                let mut one = alone.clone();
                fill(&mut one);
                assert!(
                    shares_pixels(&cb.pixels, &solo, &cpu.render_input(&one).pixels),
                    "{arm} {name}: the stream {side} the slot paints where the sprite does"
                );
            }
            let (c, g) = (
                cpu.render_input(&both),
                gpu.render_input(&mut win, &both, None),
            );
            assert_arm(arm, &gpu);
            assert!(gpu.frame_shape().fx_instances > 0, "{arm} {name}: fx drew");
            let delta = max_channel_delta(&c.pixels, &g.pixels);
            assert!(delta <= 2, "{arm} {name}: delta {delta} > 2");
        }
    }
}

/// A `bloom` layer with no aurora on the frame: an additive rect and an
/// Index8 sprite at the Glow slot.
fn blooming_layer() -> FxLayer {
    let art =
        Arc::new(FxArt::new(3, 2, FxTexels::Index8, vec![1u8, 0, 2, 2, 1, 0]).expect("sized"));
    let mut l = layer(
        1,
        vec![
            FxItem::Rect(FxRect {
                x: 30,
                y: 40,
                w: 120,
                h: 12,
                ink: 0,
            }),
            FxItem::Sprite(FxSprite {
                x: 200,
                y: 70,
                dw: 31,
                dh: 22,
                flip_x: true,
                art,
                ink: 2,
                clip: PxRect::EVERYWHERE,
            }),
        ],
        vec![
            FxInk::add(0x0070_3010),
            FxInk::add(0x0010_5090),
            FxInk::add(0x0060_6060),
            FxInk::add(0x0020_8020),
        ],
        FxSlot::Glow,
        FxBlend::Exact,
    );
    l.bloom = true;
    l
}

/// THE BLOOM FEED (design §4.6): a `bloom` layer is the bloom's light on a
/// frame with no aurora. On every arm the GPU's halo is the CPU twin's
/// (`aterm_render::bloom::apply` over `fx_bloom_quads`) within its float
/// rounding (2) and the same light in total; the frame without the bloom is
/// byte-exact. On a scissored present (one row repainted) the halo still
/// spreads from the whole layer: the frame equals a full present of the same
/// input. Dropping the layers from the bloom source leaves no halo (the
/// light-in-total check fails).
#[test]
fn bloom_fx_layers_feed_the_gpu_bloom() {
    let theme = Theme::default();
    let params = aterm_render::bloom::BloomParams::default();
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut t = base_terminal();
        gpu.set_bloom_params(params.strength, params.radius);
        let input = input_with(&mut t, vec![blooming_layer()], Vec::new());
        assert!(input.cursor_glow_add.is_empty(), "no aurora on the frame");
        gpu.set_bloom(true);
        let lit = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &input, None);
        assert_arm(arm, &gpu);
        gpu.set_bloom(false);
        let off = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &input, None);
        let base = cpu.render_input(&input);
        assert_eq!(
            max_channel_delta(&base.pixels, &off.pixels),
            0,
            "{arm}: the unbloomed frame is byte-exact"
        );
        let f = cpu.fx_frame(base.width, base.height, ROWS);
        let mut quads = Vec::new();
        aterm_render::fx_bloom_quads(
            &f,
            &input.fx_layers,
            &input.fx_occluders,
            &mut aterm_render::FxSlicer::default(),
            &mut aterm_render::SpriteCols::default(),
            &mut quads,
        );
        assert!(!quads.is_empty(), "the layer is bloom light");
        let mut twin = base.clone();
        aterm_render::bloom::apply(&mut twin, &quads, ROWS, params).expect("blooms");
        let energy = |frame: &aterm_render::Frame| -> i64 {
            frame
                .pixels
                .iter()
                .zip(&base.pixels)
                .map(|(&p, &b)| {
                    [16u32, 8, 0]
                        .iter()
                        .map(|&sh| i64::from((p >> sh) & 0xff) - i64::from((b >> sh) & 0xff))
                        .sum::<i64>()
                })
                .sum()
        };
        let (gpu_energy, cpu_energy) = (energy(&lit), energy(&twin));
        assert!(gpu_energy > 0, "{arm}: an fx-only bloom frame blooms");
        assert!(
            (gpu_energy - cpu_energy).abs() * 100 <= gpu_energy,
            "{arm}: the same light in total: {cpu_energy} against {gpu_energy}"
        );
        let worst = max_channel_delta(&twin.pixels, &lit.pixels);
        assert!(worst <= 2, "{arm}: the CPU twin's halo: delta {worst}");

        // The scissored present: frame B repaints row 2 only.
        gpu.set_bloom(true);
        let mut win = aterm_gpu::WindowGpu::new();
        let _ = gpu.present_input_readback(&mut win, &input);
        t.process("\x1b[3;14H\x1b[36m██\x1b[0m".as_bytes());
        let b = input_with(&mut t, vec![blooming_layer()], Vec::new());
        let scissored = gpu.scissor_taken();
        let got = gpu.present_input_readback(&mut win, &b).pixels;
        assert_eq!(
            gpu.scissor_taken(),
            scissored + 1,
            "{arm}: frame B is a scissored present"
        );
        let want = gpu
            .present_input_readback(&mut aterm_gpu::WindowGpu::new(), &b)
            .pixels;
        assert_eq!(
            max_channel_delta(&want, &got),
            0,
            "{arm}: the scissored frame's halo spreads from the whole layer"
        );
        assert_ne!(got, cpu.render_input(&b).pixels, "{arm}: and it blooms");
    }
}

/// The crown (the present's HDR aurora boost over the bloom source) reads a
/// `bloom` layer on a frame with no aurora: on a 2.0× EDR panel the fx-only
/// frame emits above reference white, and the same layer without `bloom`
/// does not (the control). The HDR present stand-in is the wgpu arm's.
#[cfg(wgpu_arm)]
#[test]
fn an_fx_only_bloom_frame_takes_the_crown() {
    let mut gpu = common::wgpu_oracle(18.0, Theme::default());
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    gpu.set_hdr_glow(true);
    let mut t = base_terminal();
    let mut over_white = |bloom: bool| {
        let mut l = blooming_layer();
        l.bloom = bloom;
        let input = input_with(&mut t, vec![l], Vec::new());
        let mut win = aterm_gpu::WindowGpu::new();
        win.set_edr_max(2.0);
        let (px, _, _) = gpu.present_hdr_for_test(&mut win, &input, true);
        px.iter()
            .enumerate()
            .filter(|&(i, &v)| i % 4 != 3 && v > 1.0)
            .count()
    };
    assert!(over_white(true) > 0, "the bloom layer takes the crown");
    assert_eq!(over_white(false), 0, "control: a plain layer does not");
}

/// THE FRAME ART BUDGET on the GPU (design §4.6): a frame naming nine
/// distinct 2048×1024 Index8 arts (half again what a 4096² atlas holds)
/// draws the first two, which fill [`aterm_core::render::FX_FRAME_ART_TEXELS`],
/// and refuses the other seven on the CPU and every arm alike: byte-exact,
/// with the refusals counted in `fx_refused`. An RGBA art on the same frame
/// is ruled in its own width class and draws. Without the budget the CPU
/// draws all nine while the atlas holds eight (the control, by hand).
#[test]
fn an_over_budget_frame_draws_the_same_arts_on_the_gpu() {
    use aterm_core::render::{FX_ART_MAX_SIDE, FX_FRAME_ART_TEXELS};
    let (aw, ah) = (FX_ART_MAX_SIDE, FX_ART_MAX_SIDE / 2);
    let area = u64::from(aw) * u64::from(ah);
    assert_eq!(FX_FRAME_ART_TEXELS / area, 2, "two arts fill the budget");
    let theme = Theme::default();
    let mut t = base_terminal();
    let arts: Vec<Arc<FxArt>> = (1..=9u8)
        .map(|k| {
            let data = vec![k; usize::from(aw) * usize::from(ah)];
            Arc::new(FxArt::new(aw, ah, FxTexels::Index8, data).expect("sized"))
        })
        .collect();
    let rgba = Arc::new(
        FxArt::new(2, 2, FxTexels::Rgba8Premul, [90, 40, 20, 255].repeat(4)).expect("sized"),
    );
    // Entry k − 1 is what art k's texels name: nine opaque inks.
    let palette: Vec<FxInk> = (0..9u32)
        .map(|k| FxInk::over(0x0030_2010 + k * 0x0010_1418, 0xff))
        .collect();
    let sprite = |i: usize, art: &Arc<FxArt>| {
        FxItem::Sprite(FxSprite {
            x: 6 + (i as i32 % 5) * 40,
            y: 10 + (i as i32 / 5) * 50,
            dw: 33,
            dh: 21,
            flip_x: false,
            art: Arc::clone(art),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
    };
    let mut items: Vec<FxItem> = arts.iter().enumerate().map(|(i, a)| sprite(i, a)).collect();
    items.push(sprite(9, &rgba));
    let layers = vec![layer(1, items, palette, FxSlot::Over, FxBlend::Exact)];
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let base = cpu.render_input(&t.cell_frame(ROWS, COLS)).pixels;
        let input = input_with(&mut t, layers.clone(), Vec::new());
        let (c, g) = (
            cpu.render_input(&input),
            gpu.render_input(&mut win, &input, None),
        );
        assert_arm(arm, &gpu);
        let painted = |i: usize| {
            let (x, y) = (6 + (i % 5) * 40 + 16, 10 + (i / 5) * 50 + 10);
            c.pixels[y * c.width + x] != base[y * c.width + x]
        };
        assert!(painted(0) && painted(1), "{arm}: the first two arts draw");
        assert!(
            (2..9).all(|i| !painted(i)),
            "{arm}: the arts past the budget draw nothing"
        );
        assert!(painted(9), "{arm}: the RGBA art has its own budget");
        assert_eq!(gpu.frame_shape().fx_refused, 7, "{arm}: seven refused");
        assert_eq!(
            max_channel_delta(&c.pixels, &g.pixels),
            0,
            "{arm}: the GPU refuses what the CPU refuses"
        );
    }
}

/// THE GATE OWES THE PREFETCH (design §4.6, eager residency): an unchanged
/// frame re-presents cached pixels through `render_input_cached`'s gate, but
/// a new `fx_prefetch` list (which draws nothing, so the pixels compare
/// equal) misses once, and that encode uploads the arts; the same list again
/// hits. A gate blind to the prefetch would hit and leave the arts for the
/// frame that draws them (the control, by hand).
#[test]
fn a_new_prefetch_misses_the_cached_gate_once() {
    let theme = Theme::default();
    let mut rng = Rng(0x5a5a_0123_4567_89ab);
    let art = premul_art(&mut rng);
    let art_bytes = u64::from(art.w()) * u64::from(art.h()) * 4;
    for (arm, _cpu, mut gpu) in arms(theme) {
        let mut t = base_terminal();
        let mut win = aterm_gpu::WindowGpu::new();
        let plain = t.cell_frame(ROWS, COLS);
        let want = gpu.render_input_cached(&mut win, &plain).pixels().to_vec();
        let _ = gpu.render_input_cached(&mut win, &plain);
        assert_arm(arm, &gpu);
        let (hits, misses) = (gpu.gate_hits(), gpu.gate_misses());
        assert_eq!(hits, 1, "{arm}: the unchanged frame hits");
        let mut hinted = plain.clone();
        hinted.fx_prefetch = vec![Arc::clone(&art)];
        let got = gpu.render_input_cached(&mut win, &hinted).pixels().to_vec();
        assert_eq!(
            gpu.gate_misses(),
            misses + 1,
            "{arm}: a new prefetch misses"
        );
        assert_eq!(
            gpu.frame_shape().fx_upload_bytes,
            art_bytes,
            "{arm}: and uploads the art"
        );
        assert_eq!(got, want, "{arm}: drawing nothing new");
        let _ = gpu.render_input_cached(&mut win, &hinted);
        assert_eq!(gpu.gate_hits(), hits + 1, "{arm}: the same prefetch hits");
    }
}

/// A frame whose fx staging fails closed (design §4.6, device loss) draws no
/// fx and the rest of the frame, and its metrics say so: no fx instance, and
/// only the atlas texels the sync did upload. The next frame draws the
/// layer, its arts already resident (instances and palette only). Control:
/// counting the built instances whatever the arm staged reports them.
#[cfg(target_os = "macos")]
#[test]
fn a_frame_whose_fx_fails_closed_counts_no_fx() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = common::backends(18.0, theme);
    let mut t = base_terminal();
    let mut rng = Rng(0x00fa_11c1_05ed_0001);
    let art = index_art(&mut rng, 3);
    let art_bytes = u64::from(art.w()) * u64::from(art.h());
    let palette = vec![FxInk::over(0x0030_5070, 0xff); 4];
    let drawn = layer(
        1,
        vec![FxItem::Sprite(FxSprite {
            x: 10,
            y: 12,
            dw: 41,
            dh: 27,
            flip_x: false,
            art,
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })],
        palette.clone(),
        FxSlot::Over,
        FxBlend::Exact,
    );
    let input = input_with(&mut t, vec![drawn], Vec::new());
    let bare = cpu.render_input(&t.cell_frame(ROWS, COLS)).pixels;
    gpu.fail_fx_staging_for_test(true);
    let failed = gpu
        .render_input(&mut aterm_gpu::WindowGpu::new(), &input, None)
        .pixels;
    assert!(gpu.metal_render_armed(), "the frame drew on Metal");
    let shape = gpu.frame_shape();
    assert_eq!(shape.fx_instances, 0, "no fx instance was staged");
    assert_eq!(shape.fx_upload_bytes, art_bytes, "only the synced texels");
    assert_eq!(
        max_channel_delta(&bare, &failed),
        0,
        "the rest of the frame draws"
    );
    gpu.fail_fx_staging_for_test(false);
    let drew = gpu
        .render_input(&mut aterm_gpu::WindowGpu::new(), &input, None)
        .pixels;
    let shape = gpu.frame_shape();
    assert!(shape.fx_instances > 0, "the next frame draws the layer");
    assert_eq!(
        shape.fx_upload_bytes,
        u64::from(shape.fx_instances) * 32 + 4 * palette.len() as u64,
        "its arts already resident"
    );
    assert_eq!(
        max_channel_delta(&cpu.render_input(&input).pixels, &drew),
        0
    );
}
