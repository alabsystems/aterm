// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_core::render::{
    FirePatch, FreeSampler, FreeSprite, FreeZ, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer,
    FxRamps, FxRect, FxSlot, FxSpace, FxSprite, FxTexels, GlowQuad, PxRect, RainHalo, RenderInput,
    SceneAtlas,
};
use aterm_core::terminal::Terminal;
use aterm_render::{
    DamageOutcome, DirtyDecision, Renderer, Theme, WindowCpu, add_sat, compute_dirty_rows,
};

fn renderer() -> Renderer {
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
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
#[test]
fn each_slot_sits_where_the_gpu_draws_it() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let (pad, top) = (rend.pad(), rend.grid_top());
    let mut t = term(4, 14);
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

/// The rows of a `rows`-row frame `h` pixels tall whose bands (the renderer's
fn rows_reached(rend: &Renderer, rows: usize, h: usize, (y0, y1): (i32, i32)) -> Vec<usize> {
    (0..rows)
        .filter(|&r| {
            let (b0, b1) = rend.row_pixel_band(r, rows, h);
            i32::try_from(b0).expect("small") < y1 && y0 < i32::try_from(b1).expect("small")
        })
        .collect()
}

/// The rows a layer's extent reaches in a frame (Grid space at the grid origin).
fn layer_rows(rend: &Renderer, rows: usize, h: usize, l: &FxLayer) -> Vec<usize> {
    let e = l.extent();
    let dy = match l.space {
        FxSpace::Window => 0,
        FxSpace::Grid => i32::try_from(rend.grid_top()).expect("small"),
    };
    rows_reached(rend, rows, h, (e.y0 + dy, e.y1 + dy))
}

/// The sorted union of row sets.
fn union(sets: &[&[usize]]) -> Vec<usize> {
    let mut all: Vec<usize> = sets.iter().flat_map(|s| s.iter().copied()).collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// THE FX ARM OF THE SHARED DAMAGE SEAM (design §4.4), under a padded grid
#[test]
fn compute_dirty_rows_tracks_fx_layers() {
    let mut rend = renderer();
    rend.set_pad(5);
    rend.set_head(7);
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (10u16, 14u16);
    let (nr, nc) = (usize::from(rows), usize::from(cols));
    let mut t = term(rows, cols);
    let h = rend.render_input(&t.cell_frame(nr, nc)).height;
    let (top, chi) = (
        i32::try_from(rend.grid_top()).expect("small"),
        i32::try_from(ch).expect("small"),
    );
    let glow = |id: u64, y: i32, ink: u32| {
        layer(
            id,
            vec![rect(8, y, 7 * cw, ch, 0)],
            vec![FxInk::add(ink), FxInk::add(ink >> 1)],
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        )
    };
    let a = glow(1, top + 2 * chi + chi / 2, 0x0030_2010);
    let b = layer(
        2,
        vec![rect(cw as i32, 6 * chi + 2, 4 * cw, ch - 4, 0)],
        vec![FxInk::over(0x0040_2030, 0x80); 2],
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    let c = layer(
        3,
        vec![rect(0, 1, 9 * cw, 4, 0)],
        vec![FxInk::add(0x0020_3040); 2],
        FxSlot::Glow,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let (ra, rb, rc) = (
        layer_rows(&rend, nr, h, &a),
        layer_rows(&rend, nr, h, &b),
        layer_rows(&rend, nr, h, &c),
    );
    assert_eq!(ra, vec![2, 3], "the Window layer's bands");
    assert_eq!(rb, vec![6], "the Grid layer's band, through grid_top");
    assert_eq!(rc, vec![0], "the head band is row 0's");
    let mut moved = a.clone();
    moved.offset.1 += 3 * chi;
    let rmoved = layer_rows(&rend, nr, h, &moved);
    assert_eq!(rmoved, vec![5, 6]);
    let changed = glow(1, top + 2 * chi + chi / 2, 0x0010_3020);
    let mut clipped = a.clone();
    clipped.clip = PxRect::new(i32::MIN, i32::MIN, i32::MAX, top + 3 * chi);
    let occ = PxRect::new(i32::MIN, top + 3 * chi, i32::MAX, top + 5 * chi);

    type Frame<'a> = (&'a [FxLayer], &'a [PxRect]);
    let cases: [(&str, Frame, Frame, Vec<usize>); 12] = [
        (
            "settled",
            (&[a.clone(), b.clone(), c.clone()], &[]),
            (&[a.clone(), b.clone(), c.clone()], &[]),
            vec![],
        ),
        (
            "settled under a settled occluder",
            (std::slice::from_ref(&a), &[occ]),
            (std::slice::from_ref(&a), &[occ]),
            vec![],
        ),
        (
            "moved",
            (std::slice::from_ref(&a), &[]),
            (std::slice::from_ref(&moved), &[]),
            union(&[&ra, &rmoved]),
        ),
        (
            "moved back",
            (std::slice::from_ref(&moved), &[]),
            (std::slice::from_ref(&a), &[]),
            union(&[&ra, &rmoved]),
        ),
        (
            "changed",
            (std::slice::from_ref(&a), &[]),
            (&[changed], &[]),
            ra.clone(),
        ),
        (
            "re-clipped",
            (std::slice::from_ref(&a), &[]),
            (&[clipped], &[]),
            ra.clone(),
        ),
        (
            "appeared",
            (std::slice::from_ref(&a), &[]),
            (&[a.clone(), b.clone()], &[]),
            rb.clone(),
        ),
        (
            "vanished",
            (&[a.clone(), b.clone()], &[]),
            (std::slice::from_ref(&a), &[]),
            rb.clone(),
        ),
        (
            "head band",
            (std::slice::from_ref(&c), &[]),
            (&[], &[]),
            rc.clone(),
        ),
        (
            "reordered",
            (&[a.clone(), b.clone()], &[]),
            (&[b.clone(), a.clone()], &[]),
            union(&[&ra, &rb]),
        ),
        (
            "occluder",
            (std::slice::from_ref(&a), &[]),
            (std::slice::from_ref(&a), &[occ]),
            vec![3],
        ),
        ("occluder over no layer", (&[], &[]), (&[], &[occ]), vec![]),
    ];
    for (name, (pl, po), (cl, co), want) in cases {
        let frame = |t: &mut Terminal, l: &[FxLayer], o: &[PxRect]| {
            let mut i = with_layers(t, rows, cols, l);
            i.fx_occluders = o.to_vec();
            i
        };
        let (prev, cur) = (frame(&mut t, pl, po), frame(&mut t, cl, co));
        let mut dirty = Vec::new();
        let d = compute_dirty_rows(
            &prev,
            &cur,
            false,
            None,
            false,
            None,
            rend.row_bands(),
            &mut dirty,
        );
        let DirtyDecision::Rows(v) = d else {
            panic!("{name}: the fx arm never forces a full repaint");
        };
        let mut wc = WindowCpu::new();
        let _ = rend.render_input_cached(&mut wc, &prev);
        let cached = rend.render_input_cached(&mut wc, &cur).pixels().to_vec();
        let damage = if want.is_empty() {
            DamageOutcome::GateHit
        } else {
            DamageOutcome::Rows
        };
        assert_eq!(wc.last_damage(), damage, "{name}");
        let fresh = rend.render_input(&cur).pixels;
        assert!(
            cached == fresh,
            "{name}: the damaged frame equals a full render"
        );
        let marked: Vec<usize> = (0..nr).filter(|&r| dirty[r]).collect();
        assert_eq!(marked, want, "{name}: the marked bands");
        assert_eq!(v.is_gate_hit(), want.is_empty(), "{name}: the gate");
        assert_eq!(v.fx_changed, !want.is_empty(), "{name}: fx_changed");
    }
}

/// THE LAYERS NEVER REFUSE THE SCROLL BLIT (design §4.4): with settled layers
#[test]
fn fx_layers_never_refuse_the_blit_and_mark_their_rows() {
    let mut rend = renderer();
    rend.set_pad(5);
    rend.set_head(7);
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (20u16, 16u16);
    let nr = usize::from(rows);
    let mut t = Terminal::new(rows, cols);
    t.process(b"\x1b[?25l");
    for i in 0..60 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    let h = rend
        .render_input(&t.cell_frame(nr, usize::from(cols)))
        .height;
    let (top, chi) = (
        i32::try_from(rend.grid_top()).expect("small"),
        i32::try_from(ch).expect("small"),
    );
    let bottom = top + i32::from(rows) * chi;
    let layers = vec![
        layer(
            1,
            vec![rect(cw as i32, chi / 2, 6 * cw, 3 * ch, 0)],
            vec![FxInk::add(0x0030_2010), FxInk::add(0x0010_2030)],
            FxSlot::Under,
            FxSpace::Grid,
            FxBlend::Exact,
        ),
        layer(
            2,
            vec![rect(9 * cw as i32, 2, 4 * cw, (top + chi + 3) as usize, 0)],
            vec![FxInk::over(0x0020_1010, 0x50); 2],
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Exact,
        ),
        layer(
            3,
            vec![rect(2 * cw as i32, 6 * chi + 3, 9 * cw, ch, 0)],
            vec![FxInk::add(0x0018_2830), FxInk::add(0x0030_1808)],
            FxSlot::Glow,
            FxSpace::Grid,
            FxBlend::Exact,
        ),
        layer(
            4,
            vec![FxItem::Sprite(FxSprite {
                x: 5 * cw as i32,
                y: bottom - chi / 2,
                dw: 3 * cw as u16,
                dh: u16::try_from(h).expect("small") - (bottom - chi / 2) as u16,
                flip_x: false,
                art: straight_art(4, 3),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00ff_e0c0, 220)],
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Linear,
        ),
    ];
    let reach: Vec<usize> = union(
        &layers
            .iter()
            .map(|l| layer_rows(&rend, nr, h, l))
            .collect::<Vec<_>>()
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>(),
    );
    assert!(
        reach.contains(&0) && reach.contains(&(nr - 1)),
        "the layers reach both edge rows: {reach:?}"
    );
    let mut wc = WindowCpu::new();
    let first = with_layers(&mut t, rows, cols, &layers);
    let _ = rend.render_input_cached(&mut wc, &first);
    /// A named move of the frame under the settled layers.
    type Step = (&'static str, fn(&mut Terminal));
    let steps: [Step; 3] = [
        ("wheel back 3", |t| t.scroll_display(3)),
        ("wheel forward 3", |t| t.scroll_display(-3)),
        ("an inline stream of 3 lines", |t| {
            t.process(b"more 1\r\nmore 2\r\nmore 3\r\n")
        }),
    ];
    for (name, step) in steps {
        step(&mut t);
        let input = with_layers(&mut t, rows, cols, &layers);
        let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
        assert!(
            matches!(wc.last_damage(), DamageOutcome::Scroll { .. }),
            "{name}: the layers do not refuse the blit ({:?})",
            wc.last_damage()
        );
        let fresh = rend.render_input(&input).pixels;
        assert!(
            cached == fresh,
            "{name}: the blitted frame equals a full render"
        );
        let dirty = wc.dirty_rows().to_vec();
        for r in &reach {
            assert!(dirty[*r], "{name}: row {r} the layers draw is marked");
        }
        assert!(
            dirty.iter().filter(|&&d| d).count() < nr,
            "{name}: the blit repaints fewer rows than the frame: {dirty:?}"
        );
    }
}

/// A renderer at an explicit font px: the upward-overshoot seams of the
fn renderer_px(px: f32) -> Renderer {
    let mut r = Renderer::from_system(px, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.debug_block_on_lazy_fallbacks();
    r
}

/// Line `i` of a glyph class (newline included): `accent` is accented
fn overshoot_line(class: &str, i: usize) -> String {
    let accent = format!("\u{c9}\u{c1}\u{d1}\u{c5}\u{ca}\u{cd}\u{c9}\u{c1}\u{d1}\u{c5} r{i}\r\n");
    match class {
        "accent" => accent,
        "mixed" if i.is_multiple_of(2) => accent,
        "mixed" => format!("plain line {i}\r\n"),
        other => panic!("unknown class {other}"),
    }
}

/// The pixels of `cached` that differ from `fresh`.
fn diverging(cached: &[u32], fresh: &[u32]) -> usize {
    cached.iter().zip(fresh).filter(|(a, b)| a != b).count()
}

/// THE BLIT DRAWS AN OVERSHOOT UNDER THE LIGHT ABOVE IT (design §4.4): a
#[test]
fn the_blit_draws_an_overshoot_under_the_light_above_it() {
    let mut blits = 0usize;
    for px in [11.0f32, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0] {
        let mut warm = renderer_px(px);
        let mut fresh = renderer_px(px);
        let (cw, ch) = warm.cell_size();
        let (rows, cols) = (16u16, 16u16);
        let nc = usize::from(cols);
        let (y, w, h) = (
            2 * i32::try_from(ch).expect("small"),
            u16::try_from(nc * cw).expect("small"),
            u16::try_from(3 * ch).expect("small"),
        );
        let sheet = |slot: FxSlot| {
            layer(
                1,
                vec![rect(0, y, usize::from(w), usize::from(h), 0)],
                vec![FxInk::over(0x0060_2010, 0xa0); 2],
                slot,
                FxSpace::Grid,
                FxBlend::Exact,
            )
        };
        let sprite = layer(
            1,
            vec![FxItem::Sprite(FxSprite {
                x: 0,
                y,
                dw: w,
                dh: h,
                flip_x: false,
                art: straight_art(5, 3),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00ff_e0c0, 220)],
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Linear,
        );
        let cases = [
            ("Under", sheet(FxSlot::Under)),
            ("Glow", sheet(FxSlot::Glow)),
            ("Over Exact", sheet(FxSlot::Over)),
            ("Over Linear", sprite),
        ];
        for class in ["accent", "mixed"] {
            for (slot, l) in &cases {
                let mut t = Terminal::new(rows, cols);
                t.process(b"\x1b[?25l");
                for i in 0..60 {
                    t.process(overshoot_line(class, i).as_bytes());
                }
                let mut wc = WindowCpu::new();
                let first = with_layers(&mut t, rows, cols, std::slice::from_ref(l));
                let _ = warm.render_input_cached(&mut wc, &first);
                let mut moves: Vec<String> = (60..66).map(|i| overshoot_line(class, i)).collect();
                moves.extend(["back", "back", "back", "fwd", "fwd", "fwd"].map(String::from));
                for (step, m) in moves.iter().enumerate() {
                    match m.as_str() {
                        "back" => t.scroll_display(1),
                        "fwd" => t.scroll_display(-1),
                        line => t.process(line.as_bytes()),
                    }
                    let input = with_layers(&mut t, rows, cols, std::slice::from_ref(l));
                    let cached = warm.render_input_cached(&mut wc, &input).pixels().to_vec();
                    let damage = wc.last_damage();
                    assert!(
                        matches!(damage, DamageOutcome::Scroll { .. }),
                        "px {px} {class} {slot} step {step}: the layer keeps the blit ({damage:?})"
                    );
                    blits += 1;
                    let full = fresh.render_input(&input).pixels;
                    assert!(
                        cached == full,
                        "px {px} {class} {slot} step {step}: the blitted frame equals a full \
                         render ({} px differ)",
                        diverging(&cached, &full)
                    );
                }
            }
        }
    }
    assert_eq!(
        blits,
        8 * 2 * 4 * 12,
        "every frame of the sweep took the blit"
    );
}

/// LAYERS THAT CHANGE ON A SCROLL FRAME (design §4.4, §4.10): the blit's fx
#[test]
fn layers_that_change_on_a_scroll_frame_equal_a_full_render() {
    let mut rend = renderer();
    rend.set_pad(5);
    rend.set_head(12);
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (20u16, 16u16);
    let mut t = Terminal::new(rows, cols);
    t.process(b"\x1b[?25l");
    for i in 0..60 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    let (top, chi) = (
        i32::try_from(rend.grid_top()).expect("small"),
        i32::try_from(ch).expect("small"),
    );
    let head = layer(
        1,
        vec![rect(
            cw as i32,
            2,
            9 * cw,
            usize::try_from(top - 4).expect("small"),
            0,
        )],
        vec![FxInk::add(0x0030_4050), FxInk::add(0x0050_3010)],
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let card = layer(
        2,
        vec![rect(2 * cw as i32, 6 * chi + 3, 8 * cw, 2 * ch, 0)],
        vec![FxInk::over(0x0040_2030, 0x90); 2],
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    let occ = PxRect::new(i32::MIN, top + 6 * chi, i32::MAX, top + 7 * chi);
    let mut moved = card.clone();
    moved.offset.1 += 5 * chi;

    let mut wc = WindowCpu::new();
    let mut frame = |t: &mut Terminal, layers: &[FxLayer], occluders: &[PxRect], what: &str| {
        let mut input = with_layers(t, rows, cols, layers);
        input.fx_occluders = occluders.to_vec();
        let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
        let damage = wc.last_damage();
        let fresh = rend.render_input(&input).pixels;
        (damage, diverging(&cached, &fresh), what.to_owned())
    };
    let check = |(damage, bad, what): (DamageOutcome, usize, String)| {
        assert!(
            matches!(damage, DamageOutcome::Scroll { .. }),
            "{what}: the frame takes the blit ({damage:?})"
        );
        assert_eq!(bad, 0, "{what}: the blitted frame equals a full render");
    };

    t.scroll_display(4);
    let _ = frame(&mut t, &[head.clone(), card.clone()], &[], "warm");
    t.scroll_display(-1);
    check(frame(
        &mut t,
        std::slice::from_ref(&card),
        &[],
        "the head-band layer vanishes",
    ));
    t.scroll_display(-1);
    check(frame(
        &mut t,
        std::slice::from_ref(&moved),
        &[],
        "a layer moves",
    ));
    t.scroll_display(1);
    check(frame(
        &mut t,
        std::slice::from_ref(&moved),
        &[occ],
        "an occluder appears",
    ));
    t.scroll_display(-1);
    check(frame(
        &mut t,
        &[moved.clone(), head.clone()],
        &[],
        "it goes as the head lights",
    ));
    t.scroll_display(-1);
    check(frame(
        &mut t,
        std::slice::from_ref(&card),
        &[],
        "the layer moves back",
    ));

    t.scroll_display(-100);
    let mut riding = card.clone();
    riding.offset.1 += 8 * chi;
    let _ = frame(&mut t, std::slice::from_ref(&riding), &[], "the bottom");
    for i in 0..9 {
        t.process(format!("streamed {i}\r\n").as_bytes());
        riding.offset.1 -= chi;
        check(frame(
            &mut t,
            std::slice::from_ref(&riding),
            &[],
            &format!("the card rides line {i}"),
        ));
    }
}

/// A LAYER THAT RIDES THE BLIT IS SLID, NOT REBUILT (design §4.4 as built,
#[test]
fn a_layer_that_rides_the_blit_is_slid_not_rebuilt() {
    let mut rend = renderer();
    rend.set_pad(5);
    rend.set_head(12);
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (20u16, 16u16);
    let nr = usize::from(rows);
    let (top, chi) = (
        i32::try_from(rend.grid_top()).expect("small"),
        i32::try_from(ch).expect("small"),
    );
    let under = layer(
        1,
        vec![rect(cw as i32, top + 9 * chi + 2, 9 * cw, 2 * ch - 4, 0)],
        vec![FxInk::add(0x0030_2010), FxInk::add(0x0010_2030)],
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let over = layer(
        2,
        vec![FxItem::Sprite(FxSprite {
            x: 2 * cw as i32,
            y: 10 * chi + 1,
            dw: u16::try_from(5 * cw).expect("small"),
            dh: u16::try_from(ch - 2).expect("small"),
            flip_x: false,
            art: straight_art(5, 3),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })],
        vec![FxInk::over(0x00ff_e0c0, 220)],
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Linear,
    );
    let cut = PxRect::new(i32::MIN, i32::MIN, i32::MAX, top + 9 * chi + chi / 2);
    let shelf = PxRect::new(i32::MIN, top + 9 * chi, i32::MAX, top + 11 * chi);
    let head = layer(
        1,
        vec![rect(cw as i32, top - 4, 9 * cw, 3 * ch, 0)],
        vec![FxInk::add(0x0030_2010), FxInk::add(0x0010_2030)],
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let cases = [
        ("rides", 5, true),
        ("held", 3, false),
        ("clipped", 1, false),
        ("occluded", 2, false),
        ("into the head band", 2, false),
        ("two rows a line", 3, false),
        ("swapped", 3, false),
    ];
    let mut slid_frames = 0usize;
    for (name, lines, slides) in cases {
        let card: [FxLayer; 2] = match name {
            "clipped" => {
                let (mut u, mut o) = (under.clone(), over.clone());
                u.clip = cut;
                o.clip = cut.translate(-5, -top);
                [u, o]
            }
            "into the head band" => [head.clone(), over.clone()],
            _ => [under.clone(), over.clone()],
        };
        let lay = |k: i32| -> Vec<FxLayer> {
            let step = match name {
                "held" => 0,
                "two rows a line" => 2 * chi,
                _ => chi,
            };
            let mut v: Vec<FxLayer> = card.iter().map(|l| moved(l, k * step)).collect();
            if name == "swapped" && k % 2 == 1 {
                v.reverse();
            }
            v
        };
        let occluders = if name == "occluded" {
            vec![shelf]
        } else {
            vec![]
        };
        let mut t = Terminal::new(rows, cols);
        t.process(b"\x1b[?25l");
        for i in 0..60 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        let mut wc = WindowCpu::new();
        let mut input = with_layers(&mut t, rows, cols, &lay(0));
        input.fx_occluders.clone_from(&occluders);
        let _ = rend.render_input_cached(&mut wc, &input);
        for k in 1..=lines {
            t.process(format!("streamed {k}\r\n").as_bytes());
            let layers = lay(k);
            let mut input = with_layers(&mut t, rows, cols, &layers);
            input.fx_occluders.clone_from(&occluders);
            let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
            let damage = wc.last_damage();
            let what = format!("{name}, line {k}");
            assert!(
                matches!(damage, DamageOutcome::Scroll { delta_rows: 1 }),
                "{what}: the frame takes the blit ({damage:?})"
            );
            let full = rend.render_input(&input);
            assert_eq!(
                diverging(&cached, &full.pixels),
                0,
                "{what}: the blitted frame equals a full render"
            );
            let reach: Vec<Vec<usize>> = layers
                .iter()
                .map(|l| layer_rows(&rend, nr, full.height, l))
                .collect();
            let card_rows = union(&reach.iter().map(Vec::as_slice).collect::<Vec<_>>());
            let dirty = wc.dirty_rows().to_vec();
            let rebuilt = card_rows.iter().filter(|&&r| dirty[r]).count();
            if slides {
                assert_eq!(rebuilt, 0, "{what}: no card row is rebuilt: {dirty:?}");
                slid_frames += 1;
            } else {
                assert!(
                    rebuilt > 0,
                    "{what}: the card's rows are rebuilt: {dirty:?}"
                );
            }
        }
    }
    assert_eq!(slid_frames, 5, "every riding frame slid the card");
}

/// `l` moved `dy` pixels up.
fn moved(l: &FxLayer, dy: i32) -> FxLayer {
    let mut l = l.clone();
    l.offset.1 -= dy;
    l
}

/// LAYERS RIDING RANDOM SCROLLS EQUAL A FULL RENDER: over random sequences on
#[test]
fn layers_riding_random_scrolls_equal_a_full_render() {
    let mut rend = renderer();
    rend.set_pad(3);
    rend.set_head(9);
    let (cw, ch) = rend.cell_size();
    let chi = i32::try_from(ch).expect("small");
    let (rows, cols) = (16u16, 14u16);
    let nr = usize::from(rows);
    let mut rng = Rng(0x5eed_0b3c_0de5_7a11);
    let (mut blits, mut slid) = (0usize, 0usize);
    for seq in 0..24 {
        let mut t = Terminal::new(rows, cols);
        t.process(b"\x1b[?25l");
        for i in 0..80 {
            t.process(format!("s{seq} line {i}\r\n").as_bytes());
        }
        let mut layers: Vec<FxLayer> = (0..rng.range(1, 3))
            .map(|id| {
                let mut l = random_layer(&mut rng, id as u64, cw, ch);
                l.offset.1 += rng.range(0, 8) as i32 * chi;
                l
            })
            .collect();
        let mut occluders: Vec<PxRect> = Vec::new();
        let frame = |t: &mut Terminal, layers: &[FxLayer], occ: &[PxRect]| {
            let mut i = with_layers(t, rows, cols, layers);
            i.fx_occluders = occ.to_vec();
            i
        };
        let mut wc = WindowCpu::new();
        let _ = rend.render_input_cached(&mut wc, &frame(&mut t, &layers, &occluders));
        let mut back = 0i32;
        for step in 0..10 {
            let up = match rng.below(3) {
                0 if back < 30 => {
                    let n = rng.range(1, 2) as i32;
                    t.scroll_display(n);
                    back += n;
                    -n
                }
                1 if back > 0 => {
                    let n = (rng.range(1, 2) as i32).min(back);
                    t.scroll_display(-n);
                    back -= n;
                    n
                }
                _ if back == 0 => {
                    let n = rng.range(1, 3) as i32;
                    for k in 0..n {
                        t.process(format!("s{seq} step {step}.{k}\r\n").as_bytes());
                    }
                    n
                }
                _ => {
                    t.scroll_display(-back);
                    let n = back;
                    back = 0;
                    n
                }
            };
            let mut riding = Vec::new();
            for l in &mut layers {
                match rng.below(5) {
                    0 => riding.push(false),
                    1 => {
                        l.offset.1 -= (up + [-1, 1][rng.below(2) as usize]) * chi;
                        riding.push(false);
                    }
                    _ => {
                        l.offset.1 -= up * chi;
                        riding.push(true);
                    }
                }
            }
            if rng.below(4) == 0 {
                occluders = if occluders.is_empty() {
                    let y = i32::try_from(rend.grid_top()).expect("small")
                        + rng.range(0, 15) as i32 * chi;
                    vec![PxRect::new(i32::MIN, y, i32::MAX, y + chi)]
                } else {
                    Vec::new()
                };
            }
            let input = frame(&mut t, &layers, &occluders);
            let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
            let damage = wc.last_damage();
            let full = rend.render_input(&input);
            assert_eq!(
                diverging(&cached, &full.pixels),
                0,
                "sequence {seq}, step {step} ({up} up, {damage:?}): the cached frame equals a \
                 full render"
            );
            if matches!(damage, DamageOutcome::Scroll { .. }) {
                blits += 1;
                let dirty = wc.dirty_rows();
                slid += usize::from(layers.iter().zip(&riding).any(|(l, &r)| {
                    let reach = layer_rows(&rend, nr, full.height, l);
                    r && !reach.is_empty() && reach.iter().all(|&row| !dirty[row])
                }));
            }
        }
    }
    eprintln!("riding random scrolls: {blits} blitted frames, {slid} slid a riding layer");
    assert!(blits >= 60, "the corpus reaches the blit ({blits})");
    assert!(slid >= 10, "the corpus slides riding layers ({slid})");
}
