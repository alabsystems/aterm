// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_core::render::{
    CharFg, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxSprite,
    FxTexels, GlowQuad, InkCell, PxRect, RainHalo, RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, premul_rgb};

type Setter = fn(&mut RenderInput, usize, usize);

struct Channel {
    /// The `RenderInput` field(s) under test.
    name: &'static str,
    /// What the 3x12 terminal shows.
    text: &'static [u8],
    /// Every empty state of the channel, labelled: each must render the bare
    /// frame byte for byte.
    empties: &'static [(&'static str, Setter)],
    /// A populated channel for the `clear_overlays` leg (`None`: no such leg).
    populate: Option<Setter>,
    /// Whether the populated channel must visibly paint before it is stripped.
    must_paint: bool,
    /// Every field of the channel is empty (Vecs cleared, atlas Arcs nulled).
    is_empty: fn(&RenderInput) -> bool,
}

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
        name: "rain_add",
        text: b"\x1b[?25lrainy planet",
        empties: &[("explicitly empty", |i, _, _| i.rain_add = Vec::new())],
        populate: Some(|i, cw, ch| {
            i.rain_add = vec![halo(1, 0, ch as u16, cw as u16, 2, 0x0020_4020)];
        }),
        must_paint: true,
        is_empty: |i| i.rain_add.is_empty(),
    },
    Channel {
        name: "fx_layers + fx_occluders + fx_prefetch",
        text: b"\x1b[?25lcard on a row",
        empties: &[
            ("every fx channel explicitly empty", |i, _, _| {
                i.fx_layers = Vec::new();
                i.fx_occluders = Vec::new();
                i.fx_prefetch = Vec::new();
            }),
            (
                "occluders and a prefetch with no layer draw nothing",
                |i, _, _| {
                    i.fx_occluders = vec![PxRect::new(0, 0, 40, 40)];
                    i.fx_prefetch = vec![Arc::new(
                        FxArt::new(1, 1, FxTexels::Index8, vec![1]).expect("sized"),
                    )];
                },
            ),
            ("a layer with no items draws nothing", |i, _, _| {
                i.fx_layers = vec![FxLayer::new(
                    1,
                    Arc::new(FxContent::default()),
                    FxSlot::Over,
                    FxSpace::Grid,
                    FxBlend::Exact,
                )];
            }),
            (
                "a knock-out sprite layer with a clear ink draws and knocks nothing",
                |i, cw, ch| {
                    let art =
                        FxArt::new(2, 2, FxTexels::Rgba8Straight, vec![255; 16]).expect("sized");
                    let mut l = FxLayer::new(
                        1,
                        Arc::new(FxContent::new(
                            vec![FxItem::Sprite(FxSprite {
                                x: 0,
                                y: 0,
                                dw: (4 * cw) as u16,
                                dh: (2 * ch) as u16,
                                flip_x: false,
                                art: Arc::new(art),
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
                    i.fx_layers = vec![l];
                },
            ),
        ],
        populate: Some(|i, cw, ch| {
            let content = FxContent::new(
                vec![FxItem::Rect(FxRect {
                    x: 0,
                    y: 0,
                    w: (2 * cw) as u16,
                    h: (2 * ch) as u16,
                    ink: 0,
                })],
                vec![FxInk::add(0x0030_2010); 2],
            );
            i.fx_layers = vec![FxLayer::new(
                1,
                Arc::new(content),
                FxSlot::Under,
                FxSpace::Grid,
                FxBlend::Exact,
            )];
            i.fx_occluders = vec![PxRect::new(0, 0, 1, 1)];
        }),
        must_paint: true,
        is_empty: |i| {
            i.fx_layers.is_empty() && i.fx_occluders.is_empty() && i.fx_prefetch.is_empty()
        },
    },
    Channel {
        name: "word_decorations",
        text: b"i love cats",
        empties: &[("explicitly cleared", |i, _, _| i.word_decorations.clear())],
        populate: None,
        must_paint: false,
        is_empty: |i| i.word_decorations.is_empty(),
    },
];

#[test]
fn every_empty_channel_is_byte_identical_and_clear_overlays_restores_the_bare_frame() {
    let mut rend = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    rend.debug_block_on_lazy_fallbacks();
    let (cw, ch) = rend.cell_size();
    for c in CHANNELS {
        let name = c.name;
        let mut term = Terminal::new(3, 12);
        term.process(c.text);
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
