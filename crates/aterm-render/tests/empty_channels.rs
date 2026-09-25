// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE NO-OP LAW, one row per effect channel of `RenderInput` (CPU path): an
// EMPTY channel — untouched, explicitly emptied, pushed-then-cleared, or an
// atlas with nothing drawn from it — is byte-identical to the frame rendered
// before the channel existed, and `clear_overlays` (the `image plain` capture)
// strips a populated channel back to that bare frame, clearing every Vec and
// nulling every atlas Arc.
//
// These were nine copy-pasted tests, one per channel file (`cat_sprites`,
// `free_overlay`, `glow_halo`, `glow_under` ×2, `ink`, `nova`, `rain_render`,
// `word_decorations`). Each row keeps its own screen text, its own empty
// states and its own populated payload.

use std::sync::Arc;

use aterm_core::render::{
    CharFg, FreeSampler, FreeSprite, FreeZ, GlowQuad, InkCell, RainHalo, RenderInput, SceneAtlas,
    SpriteQuad,
};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, premul_rgb};

/// Sets a channel state on a fresh 3x12 snapshot, given the cell size.
type Setter = fn(&mut RenderInput, usize, usize);

struct Channel {
    /// The `RenderInput` field(s) under test.
    name: &'static str,
    /// What the 3x12 terminal shows.
    text: &'static [u8],
    /// Every EMPTY state of the channel, labelled: each must render the bare
    /// frame byte for byte.
    empties: &'static [(&'static str, Setter)],
    /// A populated channel for the `clear_overlays` leg (`None`: no such leg).
    populate: Option<Setter>,
    /// Whether the populated channel must visibly paint before it is stripped.
    must_paint: bool,
    /// Every field of the channel is empty (Vecs cleared, atlas Arcs nulled).
    is_empty: fn(&RenderInput) -> bool,
}

fn patterned_atlas(w: u32, h: u32, version: u64) -> SceneAtlas {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                255,
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

/// Opaque untinted sprite quad: `dest = [x, y, w, h]`, `src = [ax, ay, aw, ah]`.
fn quad(row: u16, dest: [u16; 4], src: [u16; 4]) -> SpriteQuad {
    SpriteQuad {
        row,
        x: dest[0],
        y: dest[1],
        w: dest[2],
        h: dest[3],
        ax: src[0],
        ay: src[1],
        aw: src[2],
        ah: src[3],
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }
}

/// An additive (`HaloMode::Add`, the default) radial halo filling its rect.
fn halo(row: u16, x: u16, y: u16, w: u16, h: u16, color: u32) -> RainHalo {
    RainHalo {
        row,
        x,
        y,
        w,
        h,
        color,
        cx: x + w / 2,
        cy: y + h / 2,
        rx: (w / 2).max(1),
        ry: (h / 2).max(1),
        ..Default::default()
    }
}

/// An ADDITIVE glow quad (see `GlowQuad::alpha`).
fn glow(row: u16, x: u16, y: u16, w: u16, h: u16, color: u32) -> GlowQuad {
    GlowQuad {
        row,
        x,
        y,
        w,
        h,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    }
}

const CHANNELS: &[Channel] = &[
    Channel {
        name: "cat_quads + cat_atlas",
        text: b"\x1b[?25lkitty cat",
        empties: &[("an atlas with no quads draws nothing", |i, _, _| {
            i.cat_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
        })],
        populate: Some(|i, _, ch| {
            i.cat_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
            i.cat_quads = vec![quad(
                1,
                [0, ch as u16, 16, 16.min(ch as u16)],
                [0, 0, 16, 16.min(ch as u16)],
            )];
        }),
        must_paint: true,
        is_empty: |i| i.cat_quads.is_empty() && i.cat_atlas.is_none(),
    },
    Channel {
        name: "free_sprites + free_atlas",
        text: b"\x1b[?25lfree layer",
        empties: &[("an atlas with no sprites draws nothing", |i, _, _| {
            i.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
        })],
        populate: Some(|i, _, _| {
            i.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
            i.free_sprites = vec![FreeSprite {
                x: 2,
                y: 5,
                w: 16,
                h: 16,
                ax: 0,
                ay: 0,
                aw: 16,
                ah: 16,
                tint: 0x00FF_FFFF,
                alpha: 255,
                flip_x: false,
                z: FreeZ::UnderText,
                sampler: FreeSampler::Nearest,
            }];
        }),
        must_paint: false,
        is_empty: |i| i.free_sprites.is_empty() && i.free_atlas.is_none(),
    },
    Channel {
        name: "glow_halo",
        text: b"\x1b[?25lember forge",
        empties: &[
            ("explicitly empty", |i, _, _| i.glow_halo = Vec::new()),
            ("pushed then cleared", |i, cw, ch| {
                i.glow_halo
                    .push(halo(1, 0, ch as u16, cw as u16, ch as u16, 0x0040_8040));
                i.glow_halo.clear();
            }),
        ],
        populate: Some(|i, cw, ch| {
            i.glow_halo = vec![halo(
                1,
                0,
                ch as u16,
                (2 * cw) as u16,
                ch as u16,
                0x0040_8040,
            )];
        }),
        must_paint: true,
        is_empty: |i| i.glow_halo.is_empty(),
    },
    Channel {
        name: "glow_under",
        text: b"\x1b[?25lember forge",
        empties: &[
            ("explicitly empty", |i, _, _| i.glow_under = Vec::new()),
            ("pushed then cleared", |i, cw, ch| {
                i.glow_under
                    .push(glow(1, 0, ch as u16, cw as u16, ch as u16, 0x0040_2008));
                i.glow_under.clear();
            }),
        ],
        populate: Some(|i, cw, ch| {
            i.glow_under = vec![glow(
                1,
                0,
                ch as u16,
                (3 * cw) as u16,
                ch as u16,
                0x0060_3010,
            )];
        }),
        must_paint: true,
        is_empty: |i| i.glow_under.is_empty(),
    },
    Channel {
        name: "char_fg",
        text: b"\x1b[?25lember forge",
        empties: &[
            ("explicitly empty", |i, _, _| i.char_fg = Vec::new()),
            ("pushed then cleared", |i, _, _| {
                i.char_fg.push(CharFg {
                    row: 0,
                    col: 0,
                    fg: 0x0010_0804,
                });
                i.char_fg.clear();
            }),
        ],
        // A char_fg override recolours its glyph.
        populate: Some(|i, _, _| {
            i.char_fg = vec![CharFg {
                row: 0,
                col: 0,
                fg: 0x0010_0804,
            }];
        }),
        must_paint: true,
        is_empty: |i| i.char_fg.is_empty(),
    },
    Channel {
        name: "ink",
        text: b"\x1b[?25lultra think",
        // Feature on, nothing matched / everything truncated.
        empties: &[("explicitly cleared", |i, _, _| i.ink.clear())],
        populate: Some(|i, _, _| {
            i.ink = vec![InkCell {
                row: 0,
                col: 0,
                color: [0xFF, 0x00, 0xFF],
            }];
        }),
        must_paint: true,
        is_empty: |i| i.ink.is_empty(),
    },
    Channel {
        name: "nova_add",
        text: b"\x1b[?25lbuild: fuck",
        // Feature on, every nova Settled: the steady state emits nothing.
        empties: &[("explicitly cleared", |i, _, _| i.nova_add.clear())],
        populate: Some(|i, cw, ch| {
            i.nova_add.push(glow(
                1,
                (2 * cw) as u16,
                ch as u16,
                cw as u16,
                ch as u16,
                premul_rgb(0x00FF_9A3C, 220),
            ));
        }),
        must_paint: true,
        is_empty: |i| i.nova_add.is_empty(),
    },
    Channel {
        name: "rain_quads + rain_atlas + rain_add",
        text: b"\x1b[?25lrainy planet",
        empties: &[
            ("every rain channel explicitly empty", |i, _, _| {
                i.rain_quads = Vec::new();
                i.rain_atlas = None;
                i.rain_add = Vec::new();
            }),
            ("an atlas with no quads draws nothing", |i, _, _| {
                i.rain_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
            }),
        ],
        populate: Some(|i, cw, ch| {
            i.rain_atlas = Some(Arc::new(patterned_atlas(32, 32, 1)));
            let h = ch.min(32) as u16;
            // One cell of its row's band, sourced 1:1 (bake == dest).
            i.rain_quads = vec![quad(1, [0, h, cw as u16, h], [0, 0, cw as u16, h])];
            i.rain_add = vec![halo(1, 0, ch as u16, cw as u16, 2, 0x0020_4020)];
        }),
        must_paint: true,
        is_empty: |i| i.rain_quads.is_empty() && i.rain_atlas.is_none() && i.rain_add.is_empty(),
    },
    Channel {
        name: "word_decorations",
        text: b"i love cats",
        // Host feature on, no match.
        empties: &[("explicitly cleared", |i, _, _| i.word_decorations.clear())],
        populate: None,
        must_paint: false,
        is_empty: |i| i.word_decorations.is_empty(),
    },
];

#[test]
fn every_empty_channel_is_byte_identical_and_clear_overlays_restores_the_bare_frame() {
    let Some(mut rend) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Deterministic pixels: a lazy fallback parse landing between two renders
    // could recolour a "must not change" frame.
    rend.debug_block_on_lazy_fallbacks();
    let (cw, ch) = rend.cell_size();
    for c in CHANNELS {
        let name = c.name;
        let mut term = Terminal::new(3, 12);
        term.process(c.text);
        // The pre-feature frame: the snapshot as built, the channel never set.
        let as_built = term.cell_frame(3, 12);
        assert!(
            (c.is_empty)(&as_built),
            "{name}: a fresh snapshot must carry it empty"
        );
        let base = rend.render_input(&as_built).pixels.clone();

        for (label, set) in c.empties {
            let mut input = term.cell_frame(3, 12);
            set(&mut input, cw, ch);
            let got = rend.render_input(&input).pixels.clone();
            assert_eq!(base, got, "{name}, {label}: must not change any pixel");
        }

        if let Some(populate) = c.populate {
            let mut input = term.cell_frame(3, 12);
            populate(&mut input, cw, ch);
            assert!(!(c.is_empty)(&input), "{name}: the payload is non-empty");
            if c.must_paint {
                let painted = rend.render_input(&input).pixels.clone();
                assert_ne!(base, painted, "{name}: a populated channel must paint");
            }
            input.clear_overlays();
            assert!(
                (c.is_empty)(&input),
                "{name}: clear_overlays must strip it (Vecs cleared, atlas Arcs nulled)"
            );
            let stripped = rend.render_input(&input).pixels.clone();
            assert_eq!(
                base, stripped,
                "{name}: clear_overlays must restore the bare frame"
            );
        }
    }
}
