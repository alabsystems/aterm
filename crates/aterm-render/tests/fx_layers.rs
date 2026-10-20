// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The fx layers on the CPU renderer (`docs/DESIGN-card-engine-2026-10-19.md` §4,
// §7 "B, the primitive"), end to end through `Renderer`: the damaged path
// restamps only a dirty band's slice of a layer spanning three rows; several
// occluders compose; an fx rect under the glyphs is the glow_under quad stack
// it replaces; each slot sits where the GPU will draw it; Grid space is Window
// space at the grid origin; a Straight sprite is the FreeSprite stamp; and over
// random frame sequences the damaged path equals a full render. The rasterizer's
// own laws (run lists, slicing, the nearest map) are `fx_raster`'s unit tests;
// the empty-channel no-op is a row of `tests/empty_channels.rs`.

use std::sync::Arc;

use aterm_core::render::{
    FirePatch, FreeSampler, FreeSprite, FreeZ, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer,
    FxRamps, FxRect, FxSlot, FxSpace, FxSprite, FxTexels, GlowQuad, PxRect, RainHalo, RenderInput,
    SceneAtlas,
};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu, add_sat};

fn renderer() -> Renderer {
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    // Deterministic pixels: a lazy fallback parse landing between two renders
    // could recolour a frame under comparison.
    r.debug_block_on_lazy_fallbacks();
    r
}

fn rect(x: i32, y: i32, w: usize, h: usize, ink: u32) -> FxItem {
    FxItem::Rect(FxRect {
        x,
        y,
        w: u16::try_from(w).expect("small"),
        h: u16::try_from(h).expect("small"),
        ink,
    })
}

fn layer(
    id: u64,
    items: Vec<FxItem>,
    palette: Vec<FxInk>,
    slot: FxSlot,
    space: FxSpace,
    blend: FxBlend,
) -> FxLayer {
    FxLayer::new(
        id,
        Arc::new(FxContent::new(items, palette)),
        slot,
        space,
        blend,
    )
}

/// A terminal with text on every row and the cursor hidden.
fn term(rows: u16, cols: u16) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    t.process(b"\x1b[?25l");
    for r in 0..rows {
        t.process(format!("\x1b[{};1Hrow {r} of text", r + 1).as_bytes());
    }
    t
}

fn with_layers(t: &mut Terminal, rows: u16, cols: u16, layers: &[FxLayer]) -> RenderInput {
    let mut i = t.cell_frame(usize::from(rows), usize::from(cols));
    i.fx_layers = layers.to_vec();
    i
}

/// A straight-alpha art with opaque, translucent and clear texels.
fn straight_art(w: u16, h: u16) -> Arc<FxArt> {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = match (x + y) % 4 {
                0 => 0,
                1 => 255,
                _ => 90 + 20 * (x % 3) as u8,
            };
            data.extend_from_slice(&[(x * 37 + y * 11) as u8, (x * 5 + 90) as u8, 200, a]);
        }
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, data).expect("sized"))
}

/// THE DIRTY-BAND LAW, through the renderer: frame A draws layers of every
/// slot spanning rows 1..=3; frame B changes only row 2's text with the same
/// layer `Arc`s. B is a `Rows` frame repainting row 2 alone, and its pixels
/// equal a full render of B: each layer's row-2 slice was restamped over the
/// rebuilt band and rows 1 and 3 kept their light, taken once.
///
/// The negative control is a mutation (run by hand, recorded in the lane
/// report): passing `None` as the damaged path's gate double-adds the light
/// on rows 1 and 3 and fails this test.
#[test]
fn a_dirty_band_restamps_only_its_slice_of_a_spanning_layer() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (6u16, 14u16);
    let mut t = term(rows, cols);
    let y = i32::try_from(ch + ch / 2).expect("small");
    let ramp = vec![FxInk::add(0x0030_2010), FxInk::add(0x0008_1830)];
    let veil = vec![
        FxInk::over(0x0020_1008, 0x40),
        FxInk::over(0x0008_1020, 0x30),
    ];
    let layers = vec![
        layer(
            1,
            vec![rect(2, y, 6 * cw, 2 * ch, 0)],
            ramp.clone(),
            FxSlot::Under,
            FxSpace::Grid,
            FxBlend::Exact,
        ),
        layer(
            2,
            vec![rect(cw as i32, y + 2, 5 * cw, 2 * ch, 0)],
            veil,
            FxSlot::Glow,
            FxSpace::Grid,
            FxBlend::Exact,
        ),
        layer(
            3,
            vec![rect(4 * cw as i32, y - 1, 4 * cw, 2 * ch + 2, 0)],
            ramp,
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Exact,
        ),
        layer(
            4,
            vec![FxItem::Sprite(FxSprite {
                x: 8 * cw as i32,
                y,
                dw: 9,
                dh: u16::try_from(2 * ch).expect("small"),
                flip_x: true,
                art: straight_art(3, 5),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00ff_e0c0, 230)],
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Linear,
        ),
    ];
    let in_a = with_layers(&mut t, rows, cols, &layers);
    let mut wc = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc, &in_a);
    t.process(b"\x1b[3;1Hchanged");
    let in_b = with_layers(&mut t, rows, cols, &layers);
    let cached = rend.render_input_cached(&mut wc, &in_b).pixels().to_vec();
    assert_eq!(
        wc.last_damage(),
        DamageOutcome::Rows,
        "a text change on row 2"
    );
    let dirty = wc.dirty_rows().to_vec();
    assert!(
        dirty[2] && !dirty[1] && !dirty[3],
        "only row 2 repaints: {dirty:?}"
    );
    let fresh = rend.render_input(&in_b).pixels;
    assert!(cached == fresh, "the damaged frame equals a full render");
}

/// OCCLUDERS COMPOSE, through the renderer: three overlapping chrome rects
/// (a band, a menu card, a tab menu) each keep the bare frame's pixels, and
/// every other pixel takes the layer's light exactly once.
///
/// CONTROL: with only the first occluder, light lands inside the other two.
#[test]
fn occluders_compose_over_a_window_layer() {
    let mut rend = renderer();
    let (rows, cols) = (5u16, 14u16);
    let mut t = term(rows, cols);
    let bare = t.cell_frame(usize::from(rows), usize::from(cols));
    let plain = rend.render_input(&bare);
    let (w, h) = (plain.width, plain.height);
    let light = 0x0020_2020;
    let layers = vec![layer(
        7,
        vec![rect(0, 0, w, h, 0)],
        vec![FxInk::add(light); 2],
        FxSlot::Over,
        FxSpace::Window,
        FxBlend::Exact,
    )];
    let (wi, hi) = (
        i32::try_from(w).expect("small"),
        i32::try_from(h).expect("small"),
    );
    let occluders = [
        PxRect::new(0, hi / 5, wi, hi / 3),
        PxRect::new(wi / 2, hi / 4, wi - 3, 3 * hi / 4),
        PxRect::new(4, 2 * hi / 3, wi / 2 + 9, hi - 2),
    ];
    let draw = |rend: &mut Renderer, t: &mut Terminal, occ: &[PxRect]| {
        let mut i = with_layers(t, rows, cols, &layers);
        i.fx_occluders = occ.to_vec();
        rend.render_input(&i).pixels
    };
    let composed = draw(&mut rend, &mut t, &occluders);
    let single = draw(&mut rend, &mut t, &occluders[..1]);
    let hit = |o: &PxRect, x: usize, y: usize| o.overlaps(PxRect::at(x as i32, y as i32, 1, 1));
    let mut leaked = 0;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if occluders.iter().any(|o| hit(o, x, y)) {
                assert_eq!(composed[i], plain.pixels[i], "({x}, {y}) is occluded");
                if single[i] != plain.pixels[i] {
                    leaked += 1;
                }
            } else {
                assert_eq!(composed[i], add_sat(plain.pixels[i], light), "({x}, {y})");
            }
        }
    }
    assert!(
        leaked > 0,
        "CONTROL: one cut must leave light on the other menus"
    );
}

/// An fx rect under the glyphs IS the glow_under quad stack the card draws
/// today: one rect spanning three rows renders byte for byte as the producer's
/// per-row split, gradient and opacity included.
///
/// CONTROL: the same rect at the Over slot (above the glyph ink) differs.
#[test]
fn an_under_fx_rect_is_the_glow_under_stack_it_replaces() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let top = rend.grid_top();
    let (rows, cols) = (5u16, 14u16);
    let mut t = term(rows, cols);
    let (x, y0, w) = (rend.pad() + 3, top + ch / 2, 9 * cw);
    let y1 = y0 + 2 * ch;
    let (c0, c1) = (
        FxInk::over(0x0030_2010, 0x60),
        FxInk::over(0x0010_1820, 0x20),
    );
    let mut quads = Vec::new();
    for r in 0..usize::from(rows) {
        let (b0, b1) = (top + r * ch, top + (r + 1) * ch);
        let (q0, q1) = (y0.max(b0), y1.min(b1));
        if q0 < q1 {
            quads.push(GlowQuad {
                row: r as u16,
                x: x as u16,
                y: q0 as u16,
                w: w as u16,
                h: (q1 - q0) as u16,
                color: c0.color,
                alpha: c0.alpha,
                color2: c1.color,
                alpha2: c1.alpha,
            });
        }
    }
    assert_eq!(quads.len(), 3, "the rect spans three row bands");
    let mut stack = t.cell_frame(usize::from(rows), usize::from(cols));
    stack.glow_under = quads;
    let item = rect(x as i32, y0 as i32, w, 2 * ch, 0);
    let under = layer(
        1,
        vec![item.clone()],
        vec![c0, c1],
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let over = layer(
        1,
        vec![item],
        vec![c0, c1],
        FxSlot::Over,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let want = rend.render_input(&stack).pixels;
    let got = rend
        .render_input(&with_layers(&mut t, rows, cols, &[under]))
        .pixels;
    assert!(got == want, "the fx rect is the quad stack, byte for byte");
    let above = rend
        .render_input(&with_layers(&mut t, rows, cols, &[over]))
        .pixels;
    assert!(above != want, "CONTROL: above the ink is another frame");
}

/// A stream added to a frame under test.
type AddStream = Box<dyn Fn(&mut RenderInput)>;

/// The pixels of the `(x, y, w, h)` rect of a frame `fw` wide.
fn rect_px(
    px: &[u32],
    fw: usize,
    (x, y, w, h): (usize, usize, usize, usize),
) -> impl Iterator<Item = u32> + '_ {
    (y..y + h).flat_map(move |yy| px[yy * fw + x..yy * fw + x + w].iter().copied())
}

/// A frame of `t` with `layers` plus whatever `extra` adds.
fn frame_of(
    rend: &mut Renderer,
    t: &mut Terminal,
    layers: &[FxLayer],
    extra: impl FnOnce(&mut RenderInput),
) -> Vec<u32> {
    let mut i = with_layers(t, 4, 14, layers);
    extra(&mut i);
    rend.render_input(&i).pixels
}

/// EACH SLOT SITS WHERE THE GPU DRAWS IT. An opaque fx rect is a store, so
/// whatever a stream drew BEFORE it in the rect vanishes and whatever it
/// draws AFTER shows:
///
/// * Under: below the glyph ink (ink shows through an opaque Under rect,
///   never through an Over one), after glow_under, before the fire body.
/// * Glow: after the aurora (`cursor_glow_add`), before the glow halo.
/// * Over: after the nova, before the rain halos.
/// * Linear Under / Over: right after the free sprites of their z.
///
/// Each "after" leg is also its own control: the stream under test changes
/// the frame when drawn alone, so an order swap would show.
#[test]
fn each_slot_sits_where_the_gpu_draws_it() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (pad, top) = (rend.pad(), rend.grid_top());
    let mut t = term(4, 14);
    // The rect: rows 1..=2, columns 1..=8, in window pixels.
    let (x, y, w, h) = (pad + cw, top + ch, 8 * cw, 2 * ch);
    let solid = 0x0040_5060;
    let store = |slot| {
        layer(
            9,
            vec![rect(x as i32, y as i32, w, h, 0)],
            vec![FxInk::over(solid, 255); 2],
            slot,
            FxSpace::Window,
            FxBlend::Exact,
        )
    };
    let fw = rend.render_input(&t.cell_frame(4, 14)).width;
    // A light quad strictly inside the rect, window pixels, on row 1.
    let light = GlowQuad {
        row: 1,
        x: (x + 2) as u16,
        y: (y + 1) as u16,
        w: (w - 4) as u16,
        h: (ch - 2) as u16,
        color: 0x0010_1010,
        color2: 0x0010_1010,
        ..Default::default()
    };
    let halo = RainHalo {
        row: 1,
        x: (x + 2) as u16,
        y: (y + 1) as u16,
        w: (w - 4) as u16,
        h: (ch - 2) as u16,
        color: 0x0030_3030,
        cx: (x + w / 2) as u16,
        cy: (y + ch / 2) as u16,
        rx: (w / 2) as u16,
        ry: ch as u16,
        ..Default::default()
    };
    // A flame body on the same rows, rooted at the rect's row-1 bottom.
    let fire = FirePatch {
        row: 1,
        x: (x + 2) as u16,
        y: (y + 1) as u16,
        w: (w - 4) as u16,
        h: (ch - 2) as u16,
        base_y: (y + ch - 1) as u16,
        peak_h: (ch - 2) as u16,
        temp: 255,
        strength: 255,
        cov_cap: 255,
        cell_h: ch as u16,
        ..Default::default()
    };
    // Grid-interior twins for the nova and rain streams.
    let grid_light = GlowQuad {
        x: light.x - pad as u16,
        y: light.y - top as u16,
        ..light
    };
    let grid_halo = RainHalo {
        x: halo.x - pad as u16,
        y: halo.y - top as u16,
        cx: halo.cx - pad as u16,
        cy: halo.cy - top as u16,
        ..halo
    };

    // Under: the ink shows through; Over: the rect is solid.
    let under = frame_of(&mut rend, &mut t, &[store(FxSlot::Under)], |_| {});
    let over = frame_of(&mut rend, &mut t, &[store(FxSlot::Over)], |_| {});
    let area = (x, y, w, h);
    assert!(
        rect_px(&over, fw, area).all(|p| p & 0x00FF_FFFF == solid),
        "Over is solid"
    );
    assert!(
        rect_px(&under, fw, area).any(|p| p & 0x00FF_FFFF != solid),
        "Under lies below the glyph ink"
    );
    let plain = frame_of(&mut rend, &mut t, &[], |_| {});
    let checks: [(&str, FxSlot, AddStream, bool); 6] = [
        (
            "glow_under before Under",
            FxSlot::Under,
            Box::new(move |i| i.glow_under = vec![light]),
            false,
        ),
        (
            "the fire body after Under",
            FxSlot::Under,
            Box::new(move |i| i.fire_patch = vec![fire]),
            true,
        ),
        (
            "the aurora before Glow",
            FxSlot::Glow,
            Box::new(move |i| i.cursor_glow_add = vec![light]),
            false,
        ),
        (
            "the glow halo after Glow",
            FxSlot::Glow,
            Box::new(move |i| i.glow_halo = vec![halo]),
            true,
        ),
        (
            "the nova before Over",
            FxSlot::Over,
            Box::new(move |i| i.nova_add = vec![grid_light]),
            false,
        ),
        (
            "the rain halos after Over",
            FxSlot::Over,
            Box::new(move |i| i.rain_add = vec![grid_halo]),
            true,
        ),
    ];
    for (name, slot, add, after) in checks {
        let alone = frame_of(&mut rend, &mut t, &[], |i| add(i));
        assert!(alone != plain, "{name}: the stream draws inside the rect");
        let stored = frame_of(&mut rend, &mut t, &[store(slot)], |_| {});
        let both = frame_of(&mut rend, &mut t, &[store(slot)], |i| add(i));
        if after {
            assert!(both != stored, "{name}: drawn over the fx store");
        } else {
            assert!(both == stored, "{name}: hidden under the fx store");
        }
    }

    // Linear: an opaque free sprite under an opaque Straight fx sprite at the
    // same place shows the fx sprite, at both z.
    let art = Arc::new(
        FxArt::new(
            2,
            2,
            FxTexels::Rgba8Straight,
            [[200u8, 40, 10, 255]; 4].concat(),
        )
        .expect("sized"),
    );
    let atlas = Arc::new(SceneAtlas {
        width: 2,
        height: 2,
        rgba: [[10u8, 220, 90, 255]; 4].concat(),
        version: 1,
    });
    for (slot, z) in [
        (FxSlot::Under, FreeZ::UnderText),
        (FxSlot::Over, FreeZ::OverText),
    ] {
        let free = FreeSprite {
            x: cw as i32,
            y: ch as i32,
            w: 8 * cw as u16,
            h: 2 * ch as u16,
            ax: 0,
            ay: 0,
            aw: 2,
            ah: 2,
            tint: 0x00FF_FFFF,
            alpha: 255,
            flip_x: false,
            z,
            sampler: FreeSampler::Nearest,
        };
        let fx = layer(
            5,
            vec![FxItem::Sprite(FxSprite {
                x: cw as i32,
                y: ch as i32,
                dw: 8 * cw as u16,
                dh: 2 * ch as u16,
                flip_x: false,
                art: art.clone(),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00FF_FFFF, 255)],
            slot,
            FxSpace::Grid,
            FxBlend::Linear,
        );
        let add_free = |i: &mut RenderInput| {
            i.free_sprites = vec![free];
            i.free_atlas = Some(atlas.clone());
        };
        let alone = frame_of(&mut rend, &mut t, &[], add_free);
        let fx_only = frame_of(&mut rend, &mut t, std::slice::from_ref(&fx), |_| {});
        let both = frame_of(&mut rend, &mut t, std::slice::from_ref(&fx), add_free);
        assert!(alone != fx_only, "{z:?}: the free sprite and the fx differ");
        assert!(
            both == fx_only,
            "{z:?}: Linear fx draws right after the free sprites"
        );
    }
}

/// GRID SPACE IS WINDOW SPACE AT THE GRID ORIGIN: a Grid layer (offset, clip
/// and items) renders as the Window layer shifted by `(pad, grid_top)`, under
/// a padded grid with a head band.
///
/// CONTROL: the Window layer left unshifted is another frame.
#[test]
fn grid_space_is_window_space_at_the_grid_origin() {
    let mut rend = renderer();
    rend.set_pad(5);
    rend.set_head(7);
    let (cw, ch) = rend.cell_size();
    let (pad, top) = (
        i32::try_from(rend.pad()).expect("small"),
        i32::try_from(rend.grid_top()).expect("small"),
    );
    let (rows, cols) = (4u16, 14u16);
    let mut t = term(rows, cols);
    let items = vec![
        rect(-3, 2, 5 * cw, ch + 3, 0),
        FxItem::Ramps(FxRamps {
            x: 2 * cw as i32,
            y: ch as i32 - 2,
            w: 30,
            h: 2,
            ink: 2,
        }),
        FxItem::Sprite(FxSprite {
            x: 6 * cw as i32,
            y: -4,
            dw: 13,
            dh: u16::try_from(3 * ch).expect("small"),
            flip_x: false,
            art: Arc::new(
                FxArt::new(3, 2, FxTexels::Index8, vec![1, 0, 2, 3, 3, 0]).expect("sized"),
            ),
            ink: 1,
            clip: PxRect::new(0, -2, 1000, 2 * ch as i32),
        }),
    ];
    let palette = vec![
        FxInk::add(0x0020_1008),
        FxInk::add(0x0008_1020),
        FxInk::over(0x0030_2010, 0x80),
        FxInk::over(0x0010_2030, 0x40),
        FxInk::over(0x0040_4040, 255),
        FxInk::over(0x0000_2000, 0x20),
    ];
    let content = Arc::new(FxContent::new(items, palette));
    let mut grid = FxLayer::new(
        1,
        content.clone(),
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    grid.offset = (3, 5);
    grid.clip = PxRect::new(-2, -2, 9 * cw as i32, 3 * ch as i32);
    let mut window = FxLayer::new(1, content, FxSlot::Over, FxSpace::Window, FxBlend::Exact);
    window.offset = (3 + pad, 5 + top);
    window.clip = grid.clip.translate(pad, top);
    let render = |rend: &mut Renderer, t: &mut Terminal, l: &FxLayer| {
        rend.render_input(&with_layers(t, rows, cols, std::slice::from_ref(l)))
            .pixels
    };
    let g = render(&mut rend, &mut t, &grid);
    assert!(
        g == render(&mut rend, &mut t, &window),
        "Grid == Window at the grid origin"
    );
    let mut unshifted = window.clone();
    unshifted.offset = grid.offset;
    unshifted.clip = grid.clip;
    assert!(
        g != render(&mut rend, &mut t, &unshifted),
        "CONTROL: unshifted differs"
    );
}

/// THE LINEAR LAW IS FREESPRITE'S, through the renderer: a Grid-space
/// Straight sprite at a 1:1 and an integer scale renders byte for byte as the
/// FreeSprite of the same texels, tint and opacity at the matching z.
///
/// CONTROL: the fx sprite at the other z is another frame.
#[test]
fn a_linear_fx_sprite_is_the_free_sprite_stamp() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (4u16, 14u16);
    let mut t = term(rows, cols);
    let art = straight_art(7, 9);
    let atlas = Arc::new(SceneAtlas {
        width: 7,
        height: 9,
        rgba: art.data().to_vec(),
        version: 1,
    });
    let (tint, alpha) = (0x00e0_c0ff, 200);
    for scale in [1u16, 3] {
        for (slot, z, other) in [
            (FxSlot::Under, FreeZ::UnderText, FxSlot::Over),
            (FxSlot::Over, FreeZ::OverText, FxSlot::Under),
        ] {
            let (x, y) = (cw as i32 + 1, ch as i32 / 2);
            let free = FreeSprite {
                x,
                y,
                w: 7 * scale,
                h: 9 * scale,
                ax: 0,
                ay: 0,
                aw: 7,
                ah: 9,
                tint,
                alpha,
                flip_x: scale == 3,
                z,
                sampler: FreeSampler::Nearest,
            };
            let fx = |slot| {
                layer(
                    3,
                    vec![FxItem::Sprite(FxSprite {
                        x,
                        y,
                        dw: 7 * scale,
                        dh: 9 * scale,
                        flip_x: scale == 3,
                        art: art.clone(),
                        ink: 0,
                        clip: PxRect::EVERYWHERE,
                    })],
                    vec![FxInk::over(tint, alpha)],
                    slot,
                    FxSpace::Grid,
                    FxBlend::Linear,
                )
            };
            let mut want = t.cell_frame(usize::from(rows), usize::from(cols));
            want.free_sprites = vec![free];
            want.free_atlas = Some(atlas.clone());
            let want = rend.render_input(&want).pixels;
            let got = rend
                .render_input(&with_layers(&mut t, rows, cols, &[fx(slot)]))
                .pixels;
            assert!(got == want, "scale {scale}, {z:?}");
            let swapped = rend
                .render_input(&with_layers(&mut t, rows, cols, &[fx(other)]))
                .pixels;
            assert!(
                swapped != want,
                "CONTROL: scale {scale}, {z:?} at the other z"
            );
        }
    }
}

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

/// A random premultiplied art: additive texels (`a = 0` with colour), clear,
/// opaque and translucent ones.
fn premul_art(rng: &mut Rng) -> Arc<FxArt> {
    let (w, h) = (rng.range(1, 5) as u16, rng.range(1, 5) as u16);
    let mut data = Vec::new();
    for _ in 0..w * h {
        let a = [0u8, 0, 255, rng.range(1, 254) as u8][rng.below(4) as usize];
        let top = if a == 0 { 0x40 } else { u64::from(a) };
        let clear = rng.below(5) == 0;
        for _ in 0..3 {
            data.push(if clear { 0 } else { rng.below(top + 1) as u8 });
        }
        data.push(if clear { 0 } else { a });
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Premul, data).expect("sized"))
}

/// A sprite clip: none, or a box around the sprite's own origin (content
/// space, so it moves with the layer).
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

/// A random well-formed layer: an all-additive or all-over palette (so every
/// rect is one kind at both ends), rects, ramps, Index8 and Premul sprites
/// for an Exact layer, Straight sprites for a Linear one, items anywhere
/// around the grid, sprites sometimes clipped, the layer sometimes clipped.
fn random_layer(rng: &mut Rng, id: u64, cw: usize, ch: usize) -> FxLayer {
    let linear = rng.below(4) == 0;
    let additive = rng.below(2) == 0;
    let palette: Vec<FxInk> = (0..10)
        .map(|_| {
            let a = if additive { 0 } else { rng.range(1, 255) as u8 };
            let top = if additive { 0x30 } else { u64::from(a) };
            let ch = |rng: &mut Rng| rng.below(top + 1) as u32;
            FxInk {
                color: ch(rng) << 16 | ch(rng) << 8 | ch(rng),
                alpha: a,
            }
        })
        .collect();
    let (gw, gh) = (14 * cw as i64, 6 * ch as i64);
    let items = (0..rng.range(1, 4))
        .map(|_| {
            let (x, y) = (rng.range(-20, gw) as i32, rng.range(-20, gh) as i32);
            if linear {
                return FxItem::Sprite(FxSprite {
                    x,
                    y,
                    dw: rng.range(1, 40) as u16,
                    dh: rng.range(1, 3 * ch as i64) as u16,
                    flip_x: rng.below(2) == 0,
                    art: straight_art(rng.range(1, 6) as u16, rng.range(1, 6) as u16),
                    ink: rng.below(10) as u32,
                    clip: sprite_clip(rng, x, y),
                });
            }
            match rng.below(4) {
                0 => rect(
                    x,
                    y,
                    rng.range(1, 80) as usize,
                    rng.range(1, 3 * ch as i64) as usize,
                    rng.below(9) as u32,
                ),
                1 => FxItem::Ramps(FxRamps {
                    x,
                    y,
                    w: rng.range(1, 60) as u16,
                    h: rng.range(1, 5) as u16,
                    ink: 0,
                }),
                2 => FxItem::Sprite(FxSprite {
                    x,
                    y,
                    dw: rng.range(1, 50) as u16,
                    dh: rng.range(1, 3 * ch as i64) as u16,
                    flip_x: rng.below(2) == 0,
                    art: premul_art(rng),
                    ink: 0,
                    clip: sprite_clip(rng, x, y),
                }),
                _ => {
                    let (w, h) = (rng.range(1, 5) as u16, rng.range(1, 5) as u16);
                    let data = (0..w * h).map(|_| rng.below(4) as u8).collect::<Vec<_>>();
                    FxItem::Sprite(FxSprite {
                        x,
                        y,
                        dw: rng.range(1, 50) as u16,
                        dh: rng.range(1, 3 * ch as i64) as u16,
                        flip_x: rng.below(2) == 0,
                        art: Arc::new(FxArt::new(w, h, FxTexels::Index8, data).expect("sized")),
                        ink: rng.below(8) as u32,
                        clip: sprite_clip(rng, x, y),
                    })
                }
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
        Arc::new(FxContent::new(items, palette)),
        slot,
        space,
        blend,
    );
    l.offset = (rng.range(-10, 10) as i32, rng.range(-10, 10) as i32);
    if rng.below(3) == 0 {
        l.clip = PxRect::new(
            rng.range(-10, 40) as i32,
            rng.range(-10, 40) as i32,
            rng.range(30, 14 * cw as i64) as i32,
            rng.range(30, 6 * ch as i64) as i32,
        );
    }
    l
}

/// THE DAMAGED PATH EQUALS A FULL RENDER over random frame sequences: layers
/// kept (same `Arc`s), replaced, moved, added and dropped, occluders toggled,
/// and text changed on random rows, every frame of the cached path is
/// byte-equal to a fresh full render, and the sequence reaches `Rows` frames
/// that slice live layers.
///
/// The negative control is a mutation (run by hand, recorded in the lane
/// report): dropping the fx clause from `compute_dirty_rows`' reuse test lets
/// a moved layer keep its stale light and fails this test.
#[test]
fn the_damaged_path_equals_a_full_render_over_fx_sequences() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (6u16, 14u16);
    let mut rng = Rng(0x0dd5_eed5_f00d_cafe);
    let mut sliced_rows_frames = 0;
    for seq in 0..6 {
        let mut t = term(rows, cols);
        let mut wc = WindowCpu::new();
        let mut layers: Vec<FxLayer> = (0..3).map(|i| random_layer(&mut rng, i, cw, ch)).collect();
        let mut occluders: Vec<PxRect> = Vec::new();
        for step in 0..30 {
            match rng.below(8) {
                0 => {
                    let i = rng.below(layers.len() as u64) as usize;
                    layers[i] = random_layer(&mut rng, i as u64, cw, ch);
                }
                1 => {
                    let i = rng.below(layers.len() as u64) as usize;
                    layers[i].offset.1 += rng.range(-9, 9) as i32;
                }
                2 if layers.len() < 5 => {
                    layers.push(random_layer(&mut rng, layers.len() as u64, cw, ch));
                }
                3 if layers.len() > 1 => {
                    layers.pop();
                }
                4 => {
                    occluders = if occluders.is_empty() {
                        vec![PxRect::at(
                            rng.range(0, 100) as i32,
                            rng.range(0, 60) as i32,
                            rng.range(10, 120) as u32,
                            rng.range(5, 40) as u32,
                        )]
                    } else {
                        Vec::new()
                    };
                }
                _ => {
                    let r = rng.below(u64::from(rows)) + 1;
                    let word = ["fire", "ember", "card", "x", "      "][rng.below(5) as usize];
                    t.process(format!("\x1b[{r};{}H{word}", rng.range(1, 8)).as_bytes());
                }
            }
            let mut input = with_layers(&mut t, rows, cols, &layers);
            input.fx_occluders.clone_from(&occluders);
            let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
            if wc.last_damage() == DamageOutcome::Rows {
                sliced_rows_frames += 1;
            }
            let fresh = rend.render_input(&input).pixels;
            assert!(
                cached == fresh,
                "sequence {seq}, step {step}: the damaged frame diverged ({:?})",
                wc.last_damage()
            );
        }
    }
    assert!(
        sliced_rows_frames > 20,
        "the corpus reaches the damaged path with live layers ({sliced_rows_frames})"
    );
}

/// A HISTORY SCROLL NEVER SLIDES A LAYER'S LIGHT: with layers held still (the
/// same `Arc`s) over a scrolling viewport, every cached frame equals a full
/// render. Until the fx damage arm marks a layer's rows over the slid pixels
/// (design §4.4, lane B2), a frame with layers refuses the whole-row blit and
/// repaints instead.
///
/// The negative control is a mutation (run by hand, recorded in the lane
/// report): dropping the fx clause from `scroll_blittable_content` blits the
/// light along with the text and fails this test.
#[test]
fn a_history_scroll_never_slides_a_layers_light() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (5u16, 16u16);
    let mut t = Terminal::new(rows, cols);
    for i in 0..50 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    let layers = vec![
        layer(
            1,
            vec![rect(cw as i32, ch as i32 / 2, 6 * cw, 3 * ch, 0)],
            vec![FxInk::add(0x0030_2010), FxInk::add(0x0010_2030)],
            FxSlot::Under,
            FxSpace::Grid,
            FxBlend::Exact,
        ),
        layer(
            2,
            vec![rect(9 * cw as i32, ch as i32, 4 * cw, 2 * ch, 0)],
            vec![FxInk::over(0x0020_1010, 0x50); 2],
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Exact,
        ),
    ];
    let mut wc = WindowCpu::new();
    for step in 0..12 {
        if step > 0 {
            t.scroll_display(if step < 8 { 1 } else { -1 });
        }
        let input = with_layers(&mut t, rows, cols, &layers);
        let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
        let fresh = rend.render_input(&input).pixels;
        assert!(
            cached == fresh,
            "step {step}: the scrolled frame diverged ({:?})",
            wc.last_damage()
        );
    }
}
