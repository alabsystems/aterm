// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use crate::rain_common::RainScene;
use aterm_core::render::{
    CharFg, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect,
    RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_render::sprites::published;
use aterm_render::{
    FireMode, FirePatch, GlowQuad, HaloMode, RainHalo, Renderer, Theme, premul_rgb,
};

use crate::common::sprites::{cut, put, under};
use crate::common::{backends, backends_fontdue, max_channel_delta};

struct Ctx {
    rows: usize,
    cols: usize,
    cw: usize,
    ch: usize,
}

type Setter = fn(&mut RenderInput, &Ctx);

struct Channel {
    /// The `RenderInput` field(s) under test.
    name: &'static str,
    rows: usize,
    cols: usize,
    text: &'static [u8],
    /// The fontdue-rasterized backend pair (the aurora and nova suites'); the
    /// default pair otherwise.
    fontdue: bool,
    /// Also run every leg on the CPU renderer.
    cpu_too: bool,
    /// Arts resident or published with no draws (`None`: the channel carries
    /// no art).
    atlas_only: Option<Setter>,
    /// A payload that paints.
    populate: Setter,
    /// The payload really carries what the channel paints from.
    loaded: fn(&RenderInput) -> bool,
    /// The channel's own drain, exercised after a populated render.
    drain: Option<fn(&mut RenderInput)>,
    /// Every field of the channel is empty.
    is_empty: fn(&RenderInput) -> bool,
}

/// A Linear Under knock-out layer over an opaque art whose ink is clear: it
/// draws nothing, so it knocks nothing out.
fn clear_knock_layer(c: &Ctx) -> FxLayer {
    let art = cut(0, 0, 2, 2);
    let mut l = FxLayer::new(
        1,
        Arc::new(FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x: 0,
                y: 0,
                dw: (4 * c.cw) as u16,
                dh: (2 * c.ch) as u16,
                flip_x: false,
                art,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00FF_FFFF, 0)],
        )),
        FxSlot::Under,
        FxSpace::Grid,
        FxBlend::Linear,
    );
    l.knock = true;
    l
}

fn burn_patches(ch: usize, grid_w: usize, grid_h: usize) -> Vec<FirePatch> {
    let (x, w, base_y, peak_h) = (ch, 8 * ch, grid_h - ch / 2, (3 * ch) as u16);
    let x0 = x.min(grid_w);
    let x1 = (x + w).min(grid_w);
    let reach = (peak_h as usize) * 6 / 5 + 2;
    let y1 = (base_y + 1).min(grid_h);
    let mut out = Vec::new();
    let mut y = base_y.saturating_sub(reach);
    while y < y1 {
        let row = y / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(FirePatch {
            row: row as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            base_y: base_y as u16,
            peak_h,
            phase: 99_999,
            temp: 230,
            strength: 240,
            lean: -48,
            cov_cap: 200,
            cell_h: ch as u16,
            mode: FireMode::Add,
        });
        y = band_end;
    }
    out
}

fn glow(row: usize, x: usize, y: usize, w: usize, h: usize, color: u32) -> GlowQuad {
    GlowQuad {
        row: row as u16,
        x: x as u16,
        y: y as u16,
        w: w as u16,
        h: h as u16,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    }
}

fn raining(i: &RenderInput, c: &Ctx) -> RainScene {
    let mut scene = RainScene::new(c.rows, c.cols, (c.cw, c.ch), i);
    scene.drive_until_raining();
    scene
}

const CHANNELS: &[Channel] = &[
    Channel {
        name: "companion sprite layers",
        rows: 3,
        cols: 10,
        text: b"\x1b[?25lkitty",
        fontdue: false,
        cpu_too: false,
        atlas_only: Some(|i, c| {
            let layer = clear_knock_layer(c);
            i.fx_prefetch.push(cut(0, 0, 24, 24));
            i.fx_layers = vec![layer];
        }),
        populate: |i, c| {
            let h = (c.ch as u16).min(32);
            put(i, &[under(0, c.ch as i32, cut(0, 0, 24, h))]);
        },
        loaded: |i| !published(&i.fx_layers).is_empty(),
        drain: None,
        is_empty: |i| i.fx_layers.is_empty() && i.fx_prefetch.is_empty(),
    },
    Channel {
        name: "fire_patch",
        rows: 6,
        cols: 20,
        text: b"\x1b[?25l$ embers off",
        fontdue: false,
        cpu_too: false,
        atlas_only: None,
        populate: |i, c| i.fire_patch = burn_patches(c.ch, c.cols * c.cw, c.rows * c.ch),
        loaded: |i| !i.fire_patch.is_empty(),
        drain: None,
        is_empty: |i| i.fire_patch.is_empty(),
    },
    Channel {
        name: "glow_halo",
        rows: 6,
        cols: 20,
        text: b"\x1b[?25l$ embers off",
        fontdue: false,
        cpu_too: false,
        atlas_only: None,
        populate: |i, c| {
            i.glow_halo.push(RainHalo {
                row: 2,
                x: (3 * c.cw) as u16,
                y: (2 * c.ch) as u16,
                w: (2 * c.cw) as u16,
                h: c.ch as u16,
                color: 0x0080_FF80,
                cx: (4 * c.cw) as u16,
                cy: (2 * c.ch + c.ch / 2) as u16,
                rx: c.cw as u16,
                ry: (c.ch / 2).max(1) as u16,
                mode: HaloMode::Add,
            });
        },
        loaded: |i| !i.glow_halo.is_empty(),
        drain: None,
        is_empty: |i| i.glow_halo.is_empty(),
    },
    Channel {
        name: "cursor_glow_add",
        rows: 4,
        cols: 16,
        text: b"hello aterm",
        fontdue: true,
        cpu_too: true,
        atlas_only: None,
        populate: |i, c| {
            let green = premul_rgb(0x0050_FA7B, 255);
            i.cursor_glow_add
                .push(glow(1, c.cw, c.ch, c.cw, c.ch, green));
        },
        loaded: |i| !i.cursor_glow_add.is_empty(),
        drain: Some(|i| i.cursor_glow_add.clear()),
        is_empty: |i| i.cursor_glow_add.is_empty(),
    },
    Channel {
        name: "glow_under + char_fg",
        rows: 6,
        cols: 20,
        text: b"\x1b[?25l$ embers off",
        fontdue: false,
        cpu_too: false,
        atlas_only: None,
        populate: |i, c| {
            i.glow_under
                .push(glow(0, 0, 0, 10 * c.cw, c.ch, 0x0060_3010));
            i.char_fg.push(CharFg {
                row: 0,
                col: 2,
                fg: 0x0010_0804,
            });
        },
        loaded: |i| !i.glow_under.is_empty() && !i.char_fg.is_empty(),
        drain: None,
        is_empty: |i| i.glow_under.is_empty() && i.char_fg.is_empty(),
    },
    Channel {
        name: "nova_add",
        rows: 4,
        cols: 16,
        text: b"hello aterm",
        fontdue: true,
        cpu_too: true,
        atlas_only: None,
        populate: |i, c| {
            let (cw, ch) = (c.cw, c.ch);
            let core = premul_rgb(0x00FF_F2C8, 200);
            for r in 0..3 {
                i.nova_add.push(glow(r, 6 * cw, r * ch, 2 * cw, ch, core));
            }
            let fringe = premul_rgb(0x00FF_9A3C, 120);
            for r in 1..4 {
                for col in [3, 9] {
                    let y = r * ch + ch / 4;
                    i.nova_add.push(glow(r, col * cw, y, cw, ch / 2, fringe));
                }
            }
        },
        loaded: |i| !i.nova_add.is_empty(),
        drain: Some(|i| i.nova_add.clear()),
        is_empty: |i| i.nova_add.is_empty(),
    },
    Channel {
        name: "rain layers + rain_add",
        rows: 6,
        cols: 20,
        text: b"\x1b[?25l$ matrix off",
        fontdue: false,
        cpu_too: false,
        atlas_only: Some(|i, c| {
            let scene = raining(i, c);
            let tiles: Vec<_> = (0..64)
                .filter_map(|g| scene.engine.tile(g).cloned())
                .collect();
            assert!(tiles.len() == 64, "the engine must have baked its tiles");
            i.fx_prefetch = tiles;
        }),
        populate: |i, c| raining(i, c).apply(i),
        loaded: |i| !i.fx_layers.is_empty() && !i.rain_add.is_empty(),
        drain: None,
        is_empty: |i| i.fx_layers.is_empty() && i.fx_prefetch.is_empty() && i.rain_add.is_empty(),
    },
];

fn legs(
    c: &Channel,
    backend: &str,
    exact: bool,
    ctx: &Ctx,
    render: &mut dyn FnMut(&RenderInput) -> Vec<u32>,
) {
    let name = c.name;
    let same = |a: &[u32], b: &[u32]| {
        if exact {
            a == b
        } else {
            a.len() == b.len() && max_channel_delta(a, b) == 0
        }
    };
    let mut term = Terminal::new(c.rows as u16, c.cols as u16);
    term.process(c.text);
    let mut input = term.cell_frame(c.rows, c.cols);
    assert!((c.is_empty)(&input), "{name}: a fresh snapshot carries it");
    let base = render(&input);

    if let Some(set) = c.atlas_only {
        let mut atlas_only = term.cell_frame(c.rows, c.cols);
        set(&mut atlas_only, ctx);
        assert!(
            same(&base, &render(&atlas_only)),
            "{name} on {backend}: resident or published arts with no draws must be byte-identical"
        );
    }

    (c.populate)(&mut input, ctx);
    assert!((c.loaded)(&input), "{name}: the payload must be loaded");
    let painted = render(&input);
    assert_ne!(
        base, painted,
        "NON-VACUITY: {name} on {backend}: a live frame must paint"
    );

    if let Some(drain) = c.drain {
        drain(&mut input);
        assert!(
            same(&base, &render(&input)),
            "{name} on {backend}: the emptied path is not a no-op"
        );
        (c.populate)(&mut input, ctx);
        let _ = render(&input);
    }

    input.clear_overlays();
    assert!((c.is_empty)(&input), "{name}: clear_overlays must strip it");
    assert!(
        same(&base, &render(&input)),
        "{name} on {backend}: clear_overlays must restore the bare frame"
    );
}

#[test]
fn every_empty_channel_is_byte_identical_on_the_gpu_and_clear_overlays_restores_it() {
    let theme = Theme::default();
    let mut default = backends(18.0, theme);
    let mut fontdue = backends_fontdue(18.0, theme);
    for c in CHANNELS {
        let (cpu, gpu): (&mut Renderer, &mut aterm_gpu::GpuRenderer) = if c.fontdue {
            (&mut fontdue.0, &mut fontdue.1)
        } else {
            (&mut default.0, &mut default.1)
        };
        let (cw, ch) = cpu.cell_size();
        let ctx = Ctx {
            rows: c.rows,
            cols: c.cols,
            cw,
            ch,
        };
        let exact = !c.cpu_too;
        if c.cpu_too {
            legs(c, "the CPU", exact, &ctx, &mut |i| {
                cpu.render_input(i).pixels.clone()
            });
        }
        let mut win = aterm_gpu::WindowGpu::new();
        legs(c, "the GPU", exact, &ctx, &mut |i| {
            gpu.render_input(&mut win, i, None).pixels
        });
    }
}
