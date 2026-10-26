// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GPU vs. CPU fx layer parity tests.

use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, FxSprite,
    FxTexels, PxRect, RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

use crate::common::{self, max_channel_delta};

const ROWS: usize = 8;
const COLS: usize = 24;

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

fn arms(theme: Theme) -> Vec<(&'static str, Renderer, aterm_gpu::GpuRenderer)> {
    let (cpu, gpu) = common::backends(18.0, theme);
    #[allow(unused_mut, reason = "the oracle build adds an arm")]
    let mut out = vec![("native", cpu, gpu)];
    #[cfg(all(wgpu_arm, any(target_os = "macos", target_os = "linux", windows)))]
    {
        let (cpu, mut gpu) = common::backends(18.0, theme);
        common::disarm_for_oracle(&mut gpu);
        out.push(("wgpu-oracle", cpu, gpu));
    }
    out
}

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
    #[cfg(windows)]
    assert_eq!(
        gpu.last_frame_on_d3d11(),
        arm == "native",
        "{arm}: the frame's arm"
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let _ = (arm, gpu);
}

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
                    ink: if past { 11 } else { rng.below(11) as u32 },
                }),
                1 => FxItem::Ramps(FxRamps {
                    x,
                    y,
                    w: rng.range(1, 90) as u16,
                    h: rng.range(1, 6) as u16,
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

fn malformed_layer() -> FxLayer {
    let art =
        Arc::new(FxArt::new(3, 2, FxTexels::Index8, vec![1u8, 2, 3, 3, 2, 1]).expect("sized"));
    let items = vec![
        FxItem::Rect(FxRect {
            x: 4,
            y: 4,
            w: 30,
            h: 9,
            ink: 3,
        }),
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
        FxItem::Ramps(FxRamps {
            x: 90,
            y: 30,
            w: 41,
            h: 4,
            ink: 0,
        }),
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
                0x30, 0x20, 0x10, 0, 200, 100, 50, 255, 40, 30, 20, 90, 0, 0, 0, 0,
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
        let delta = max_channel_delta(&c.pixels, &g.pixels);
        common::assert_byte_exact(
            "exact_fx_layers_are_byte_exact_on_the_gpu",
            &format!("{arm}: the every-kind frame"),
            &gpu,
            delta,
            format_args!("{arm}: the every-kind frame is byte-exact (got {delta})"),
        );
        let mut rng = Rng(0x5eed_f00d_0dd5_cafe);
        let mut layers: Vec<FxLayer> = Vec::new();
        let mut next_id = 10;
        for frame in 0..48 {
            next_id += 1;
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
            common::assert_byte_exact(
                "exact_fx_layers_are_byte_exact_on_the_gpu",
                &format!("{arm}: frame {frame}"),
                &gpu,
                delta,
                format_args!("{arm}: frame {frame} (got {delta}) over {layers:#?}"),
            );
        }
    }
}

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
            assert!(
                delta <= i32::from(FxBlend::Linear.parity()),
                "{arm}: frame {frame}: Linear delta {delta}"
            );
        }
        assert!(
            painted >= 12,
            "{arm}: the Linear frames paint ({painted} of 24)"
        );
    }
}

#[test]
fn fx_texel_bytes_survive_the_arm() {
    let theme = Theme::default();
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    let premul: Vec<u8> = (0..256u32)
        .flat_map(|k| [k as u8, (255 - k) as u8, (k * 37) as u8, 255])
        .collect();
    let premul = Arc::new(FxArt::new(16, 16, FxTexels::Rgba8Premul, premul).expect("sized"));
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

/// The integer nearest map (`docs/DESIGN-card-engine-2026-10-19.md` §4.1)
/// at odd scales and mirrors, under both laws: an Exact sprite is byte-exact
/// whatever its texels, and a Linear one is byte-exact wherever its texels
/// and ink are opaque (its blend has nothing to round), so a delta there is
/// a texel picked differently. Translucent Linear texels hold
/// `FxBlend::Linear.parity()`.
#[test]
fn sprites_map_texels_exactly_at_odd_scales_under_both_laws() {
    let theme = Theme::default();
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    let (aw, ah) = (61u16, 47u16);
    let (mut straight, mut opaque, mut premul) = (Vec::new(), Vec::new(), Vec::new());
    for y in 0..u32::from(ah) {
        for x in 0..u32::from(aw) {
            let c = [(x * 4) as u8, (y * 5) as u8, ((x * 7 + y * 3) % 256) as u8];
            let a = if (x + y) % 3 == 0 {
                255
            } else {
                (40 + (x * 13 + y * 29) % 190) as u8
            };
            straight.extend_from_slice(&[c[0], c[1], c[2], a]);
            opaque.extend_from_slice(&[c[0], c[1], c[2], 255]);
            let pm = |v: u8| ((u32::from(v) * u32::from(a) + 127) / 255) as u8;
            premul.extend_from_slice(&[pm(c[0]), pm(c[1]), pm(c[2]), a]);
        }
    }
    let art = |texels, data| Arc::new(FxArt::new(aw, ah, texels, data).expect("sized"));
    let arts = [
        (
            "exact premul",
            FxBlend::Exact,
            art(FxTexels::Rgba8Premul, premul),
        ),
        (
            "linear opaque",
            FxBlend::Linear,
            art(FxTexels::Rgba8Straight, opaque),
        ),
        (
            "linear translucent",
            FxBlend::Linear,
            art(FxTexels::Rgba8Straight, straight),
        ),
    ];
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
        for (name, blend, art) in &arts {
            let mut worst = 0;
            for (i, &(dw, dh)) in scales.iter().enumerate() {
                let flip = i % 2 == 1;
                let ink = match blend {
                    FxBlend::Linear => vec![FxInk::over(0x00ff_ffff, 0xff)],
                    _ => Vec::new(),
                };
                let input = input_with(
                    &mut t,
                    vec![layer(
                        1,
                        vec![FxItem::Sprite(FxSprite {
                            x: 4,
                            y: 6,
                            dw,
                            dh,
                            flip_x: flip,
                            art: Arc::clone(art),
                            ink: 0,
                            clip: PxRect::EVERYWHERE,
                        })],
                        ink,
                        FxSlot::Over,
                        *blend,
                    )],
                    Vec::new(),
                );
                let (c, g) = (
                    cpu.render_input(&input),
                    gpu.render_input(&mut win, &input, None),
                );
                assert_arm(arm, &gpu);
                worst = worst.max(max_channel_delta(&c.pixels, &g.pixels));
            }
            eprintln!("{arm}: odd scales: {name} delta {worst}");
            if *name == "linear translucent" {
                assert!(
                    worst <= i32::from(FxBlend::Linear.parity()),
                    "{arm}: {name}: the Linear bar, got {worst}"
                );
            } else {
                assert_eq!(worst, 0, "{arm}: {name}: the integer map is byte-exact");
            }
        }
    }
}

/// An fx layer opens no render pass its slot's stream does not already
/// open: an Exact layer the passes of the legacy channel it stands beside,
/// a Linear sprite layer (the companions and the rain) the bare frame's.
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
        let mut l = layer(1, vec![item], vec![ink, ink], slot, blend);
        l.knock = linear && slot == FxSlot::Under;
        l
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
                _ => {}
            }
            let fx = input_with(&mut t, vec![rect(slot, blend)], Vec::new());
            let (want, got) = (passes(&mut gpu, &legacy), passes(&mut gpu, &fx));
            assert_arm(arm, &gpu);
            assert!(gpu.frame_shape().fx_instances > 0, "{arm} {name}: fx drew");
            assert_eq!(got, want, "{arm} {name}: the fx frame's passes");
        }
    }
}

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
        assert_eq!(
            shape.fx_atlas_bytes, art_bytes,
            "{arm}: all of them atlas texels"
        );
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
        assert_eq!(shape.fx_atlas_bytes, 0, "{arm}: no atlas texel");
        assert_eq!(max_channel_delta(&c, &g), 0, "{arm}: and draws byte-exact");
    }
    for (arm, _cpu, mut gpu) in arms(theme) {
        let draw = input_with(&mut t, vec![drawn.clone()], Vec::new());
        let _ = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &draw, None);
        let shape = gpu.frame_shape();
        assert_eq!(
            shape.fx_upload_bytes,
            u64::from(shape.fx_instances) * 32 + 4 * palette.len() as u64 + art_bytes,
            "{arm}: an art first met when drawn uploads with its frame"
        );
        assert_eq!(shape.fx_atlas_bytes, art_bytes, "{arm}: its texels");
    }
}

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

/// An opaque `w × h` premultiplied art whose every texel is its own colour
/// (`seed`, x, y hashed): a region left holding another art's texels shows.
fn tile_art(seed: u64, w: u16, h: u16) -> Arc<FxArt> {
    let mut data = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
    for y in 0..h {
        for x in 0..w {
            let v = (seed ^ (u64::from(x) << 20) ^ (u64::from(y) << 40))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
            data.extend_from_slice(&[(v >> 24) as u8, (v >> 32) as u8, (v >> 40) as u8, 255]);
        }
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Premul, data).expect("sized"))
}

/// `arts` drawn at a quarter of their size, side by side (a wide one on a
/// second row): every fourth texel of each art lands in the frame.
fn tiles_layer(arts: &[Arc<FxArt>]) -> FxLayer {
    let mut x = 4;
    let items = arts
        .iter()
        .map(|a| {
            let (dw, dh) = (a.w() / 4, a.h() / 4);
            let (ix, iy) = if a.w() > a.h() { (4, 90) } else { (x, 6) };
            if a.w() <= a.h() {
                x += i32::from(dw) + 2;
            }
            FxItem::Sprite(FxSprite {
                x: ix,
                y: iy,
                dw,
                dh,
                flip_x: false,
                art: Arc::clone(a),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })
        })
        .collect();
    layer(
        7,
        items,
        vec![FxInk::over(0x00FF_FFFF, 0xff)],
        FxSlot::Over,
        FxBlend::Exact,
    )
}

/// Frames whose arts outgrow the fx atlas evict arts the frame does not draw and reuse their
/// regions; every frame, a window's frame after another window's frame evicted its arts, and a
/// frame after a device rebuild that followed evictions land the CPU's bytes on every arm. A
/// region still holding its old art shows as another art's texels.
#[test]
fn fx_atlas_evictions_draw_every_frame_as_the_cpu() {
    let theme = Theme::default();
    let mut t = base_terminal();
    let group =
        |g: u64| -> Vec<Arc<FxArt>> { (0..4).map(|i| tile_art(g * 16 + i, 256, 256)).collect() };
    let (g0, g1, g2, g3) = (group(1), group(2), group(3), group(4));
    // Two of g1's and one 512-wide art: a whole shelf of the 512 atlas.
    let wide = vec![
        Arc::clone(&g1[0]),
        Arc::clone(&g1[1]),
        tile_art(99, 512, 256),
    ];
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut a = aterm_gpu::WindowGpu::new();
        let mut b = aterm_gpu::WindowGpu::new();
        let mut frame = |gpu: &mut aterm_gpu::GpuRenderer,
                         win: &mut aterm_gpu::WindowGpu,
                         arts: &[Arc<FxArt>],
                         what: &str| {
            let input = input_with(&mut t, vec![tiles_layer(arts)], Vec::new());
            let want = cpu.render_input(&input).pixels;
            let got = gpu.render_input(win, &input, None).pixels;
            assert_arm(arm, gpu);
            assert_eq!(
                max_channel_delta(&want, &got),
                0,
                "{arm}: {what} draws the CPU's bytes"
            );
            gpu.frame_shape()
        };
        let first = frame(&mut gpu, &mut a, &g0, "the first group");
        assert_eq!(first.fx_atlas_bytes, 4 * 256 * 256 * 4, "{arm}: g0 uploads");
        let next = frame(&mut gpu, &mut a, &g1, "a group that evicts the first");
        assert_eq!(next.fx_atlas_bytes, 4 * 256 * 256 * 4, "{arm}: g1 uploads");
        let back = frame(&mut gpu, &mut a, &g0, "the first group again");
        assert_eq!(
            back.fx_atlas_bytes,
            4 * 256 * 256 * 4,
            "{arm}: g0 was evicted and uploads again"
        );
        let _ = frame(&mut gpu, &mut a, &wide, "a wide art beside two of g1");
        let _ = frame(&mut gpu, &mut a, &g2, "window A's group");
        let _ = frame(&mut gpu, &mut b, &g3, "window B's group, evicting A's");
        let again = frame(&mut gpu, &mut a, &g2, "window A's group after B's");
        assert!(
            again.fx_atlas_bytes > 0,
            "{arm}: B's frame evicted A's arts"
        );
        let _ = frame(&mut gpu, &mut b, &g3, "window B's group after A's");
        gpu.drop_fx_arm_residency_for_test();
        let refilled = frame(
            &mut gpu,
            &mut aterm_gpu::WindowGpu::new(),
            &g3,
            "a group after a device rebuild",
        );
        assert!(refilled.fx_atlas_bytes > 0, "{arm}: the rebuild refilled");
        let _ = frame(&mut gpu, &mut a, &g0, "an evicted group after the rebuild");
    }
}

fn shares_pixels(base: &[u32], solo: &[u32], with: &[u32]) -> bool {
    base.iter()
        .zip(solo)
        .zip(with)
        .any(|((b, s), w)| b != s && s != w)
}

#[test]
fn fx_slots_draw_between_their_neighbours_on_the_gpu() {
    use aterm_render::{FireMode, FirePatch, GlowQuad, HaloMode, RainHalo};
    let theme = Theme::default();
    let mut t = base_terminal();
    let exact = common::ByteExact::default();
    for (arm, mut cpu, mut gpu) in arms(theme) {
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
        let tile = |rgba: Vec<u8>| {
            Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, rgba).expect("sized"))
        };
        let (first, art) = (tile(vec![220, 40, 30, 150]), tile(vec![30, 60, 230, 170]));
        let (x, y, w, h) = (cw as i32, ch as i32, (10 * cw) as u16, (4 * ch) as u16);
        let sprite_layer = |id: u64, slot, art: &Arc<FxArt>, dx: i32, dy: i32| {
            layer(
                id,
                vec![FxItem::Sprite(FxSprite {
                    x: x + dx,
                    y: y + dy,
                    dw: w,
                    dh: h,
                    flip_x: false,
                    art: Arc::clone(art),
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                })],
                vec![FxInk::over(0x00ff_ffff, 0xff)],
                slot,
                FxBlend::Linear,
            )
        };
        for (name, slot) in [
            ("linear under", FxSlot::Under),
            ("linear over", FxSlot::Over),
        ] {
            let sprite = sprite_layer(2, slot, &art, (cw / 2) as i32, (ch / 2) as i32);
            let alone = input_with(&mut t, vec![sprite], Vec::new());
            let solo = cpu.render_input(&alone).pixels;
            // Before: a layer of the same slot earlier in the frame's list (the
            // rain's rows under a companion, an earlier companion over).
            let before = |i: &mut RenderInput| {
                i.fx_layers.insert(0, sprite_layer(3, slot, &first, 0, 0));
            };
            let after = |i: &mut RenderInput| {
                if matches!(slot, FxSlot::Under) {
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
            assert!(
                delta <= i32::from(FxBlend::Linear.parity()),
                "{arm} {name}: delta {delta}"
            );
        }
    }
}

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
/// does not (the control).
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
/// only the atlas texels the sync did upload.
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

/// The scrollbar's rounded bars are byte-exact on every arm: the body rectangle and the two
/// antialiased caps of one Index8 art a `(width, radius)`, each cap clipped to its own rows,
/// land the CPU's bytes on the native arm and the wgpu oracle, opaque and translucent, at every
/// width the chrome draws and on and across row bands.
#[test]
fn rounded_bars_are_byte_exact_on_the_gpu() {
    use aterm_render::rounded_bar::{CapArts, RoundedBar, push_items};
    let theme = Theme::default();
    let mut t = base_terminal();
    for (arm, mut cpu, mut gpu) in arms(theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let base = t.cell_frame(ROWS, COLS);
        let cb = cpu.render_input(&base);
        let mut arts = CapArts::default();
        for (rgb, alpha) in [
            ([200, 200, 200], 92),
            ([80, 250, 123], 255),
            ([17, 99, 240], 22),
        ] {
            let mut layers = Vec::new();
            let (mut x, mut y) = (2u32, 1u32);
            for w in (1..=14).chain([18]) {
                for h in [1, 2, 3, 5, 6, 7, 9, 13, 16, 17, 29, 61] {
                    if x + w + 2 > cb.width as u32 - 4 {
                        (x, y) = (2, y + 64);
                    }
                    let bar = RoundedBar {
                        x,
                        y,
                        w,
                        h,
                        rgb,
                        alpha,
                    };
                    let (mut items, mut palette) = (Vec::new(), Vec::new());
                    push_items(bar, &mut arts, &mut items, &mut palette);
                    layers.push(FxLayer::new(
                        layers.len() as u64 + 1,
                        Arc::new(FxContent::new(items, palette)),
                        FxSlot::Over,
                        FxSpace::Grid,
                        FxBlend::Exact,
                    ));
                    x += w + 2;
                }
            }
            let input = input_with(&mut t, layers, Vec::new());
            let (c, g) = (
                cpu.render_input(&input),
                gpu.render_input(&mut win, &input, None),
            );
            assert_arm(arm, &gpu);
            assert_ne!(c.pixels, cb.pixels, "{arm}: the bars paint");
            let delta = max_channel_delta(&c.pixels, &g.pixels);
            common::assert_byte_exact(
                "rounded_bars_are_byte_exact_on_the_gpu",
                &format!("{arm} {rgb:?}@{alpha}"),
                &gpu,
                delta,
                format_args!("{arm} {rgb:?}@{alpha}: the bars are byte-exact (got {delta})"),
            );
        }
    }
}
