// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::*;
use crate::{Theme, WindowCpu};
use aterm_core::render::{FxArt, FxContent, FxItem, FxRamps, FxRect, FxSprite, FxTexels};
use aterm_core::terminal::Terminal;

#[test]
fn solver_reproduces_band_bytes_exactly_across_the_lattice() {
    for b in (0..=255u32).step_by(3) {
        for t in (0..=255u32).step_by(3) {
            for a0 in [1u8, 37, 128, 255] {
                let base = (b << 16) | (b << 8) | b;
                let c = (t << 16) | (t << 8) | t;
                let out = solve_spill_px(base, c, a0);
                let composed = over_rgb(
                    base,
                    (u32::from(out[0]) << 16) | (u32::from(out[1]) << 8) | u32::from(out[2]),
                    out[3],
                );
                assert_eq!(
                    composed, c,
                    "solve must be over_rgb-exact for b={b} t={t} a0={a0}"
                );
            }
        }
    }
    for (base, c) in [
        (0x0010_2030u32, 0x00FF_E0B0u32),
        (0x00F0_F0F0, 0x0010_2030),
        (0x0080_8080, 0x0080_FF00),
        (0x0011_1318, 0x0011_1318),
    ] {
        for a0 in [1u8, 90, 255] {
            let out = solve_spill_px(base, c, a0);
            let composed = over_rgb(
                base,
                (u32::from(out[0]) << 16) | (u32::from(out[1]) << 8) | u32::from(out[2]),
                out[3],
            );
            assert_eq!(composed, c, "mixed-channel solve for {base:06x}->{c:06x}");
        }
    }
}

fn assert_band_parity(
    r: &mut Renderer,
    input: &RenderInput,
    spill: &SpillBand,
    base: u32,
) -> [usize; 4] {
    let (lit, seams) = band_parity(r, input, spill, base, true);
    assert_eq!(seams, 0);
    lit
}

fn band_parity(
    r: &mut Renderer,
    input: &RenderInput,
    spill: &SpillBand,
    base: u32,
    strict: bool,
) -> ([usize; 4], usize) {
    let mut win = WindowCpu::new();
    let view = r.render_input_cached(&mut win, input);
    let (w, h) = (view.width(), view.height());
    let pixels = view.pixels();
    let geom = SpillGeom {
        pad: r.pad(),
        pad_top: r.pad_top(),
        head: r.head(),
        frame_w: w,
        frame_h: h,
    };
    let buf = spill.rgba();
    assert_eq!(buf.len(), geom.band_px() * 4, "buffer sized to the band");
    let mut lit = [0usize; 4];
    let mut seams = 0usize;
    let mut off = 0usize;
    for (si, s) in geom.strips().iter().enumerate() {
        for yy in 0..s.h {
            for xx in 0..s.w {
                let px = &buf[(off + yy * s.w + xx) * 4..][..4];
                let (x, y) = (s.x + xx, s.y + yy);
                let frame = pixels[y * w + x] & 0x00FF_FFFF;
                let composed = if px[3] == 0 {
                    base
                } else {
                    lit[si] += 1;
                    over_rgb(
                        base,
                        (u32::from(px[0]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[2]),
                        px[3],
                    )
                };
                if strict {
                    assert_eq!(
                        composed, frame,
                        "spill ∘ bg must equal the frame band at ({x},{y}) in strip {si}"
                    );
                }
                seams += usize::from(composed != frame);
            }
        }
        off += s.w * s.h;
    }
    (lit, seams)
}

fn fx_layer(
    id: u64,
    item: FxItem,
    palette: Vec<FxInk>,
    slot: FxSlot,
    space: FxSpace,
    blend: FxBlend,
) -> FxLayer {
    FxLayer::new(
        id,
        Arc::new(FxContent::new(vec![item], palette)),
        slot,
        space,
        blend,
    )
}

fn fx_rect(x: i32, y: i32, w: usize, h: usize) -> FxItem {
    FxItem::Rect(FxRect {
        x,
        y,
        w: u16::try_from(w).expect("small"),
        h: u16::try_from(h).expect("small"),
        ink: 0,
    })
}

fn fx_sprite(x: i32, y: i32, dw: u16, dh: u16, art: Arc<FxArt>) -> FxItem {
    FxItem::Sprite(FxSprite {
        x,
        y,
        dw,
        dh,
        flip_x: false,
        art,
        ink: 0,
        clip: PxRect::EVERYWHERE,
    })
}

fn straight_art(w: u16, h: u16) -> Arc<FxArt> {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = [0u8, 255, 140, 60][usize::from((x + y) % 4)];
            data.extend_from_slice(&[(x * 41 + 20) as u8, (y * 23 + 90) as u8, 180, a]);
        }
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, data).expect("sized"))
}

fn band_layers(fw: usize, fh: usize, ch: usize) -> Vec<FxLayer> {
    let (fwi, fhi) = (fw as i32, fh as i32);
    let border = FxLayer::new(
        1,
        Arc::new(FxContent::new(
            vec![
                fx_rect(2, 6, fw - 4, 3),
                fx_rect(2, 6, 3, fh - 12),
                fx_rect(fwi - 5, 6, 3, fh - 12),
                fx_rect(2, fhi - 6, fw - 4, 3),
            ],
            vec![
                FxInk::over(0x0030_2010, 0x60),
                FxInk::over(0x0010_2030, 0x30),
            ],
        )),
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let grid_over = FxLayer::new(
        2,
        Arc::new(FxContent::new(
            vec![
                fx_rect(-8, -10, 60, 14),
                FxItem::Ramps(FxRamps {
                    x: -6,
                    y: 2 * ch as i32,
                    w: 9,
                    h: 3,
                    ink: 0,
                }),
            ],
            vec![
                FxInk::add(0x0028_1810),
                FxInk::add(0x0008_1828),
                FxInk::over(0x0040_2000, 0x80),
                FxInk::over(0x0000_2040, 0x20),
                FxInk::add(0x0030_3030),
                FxInk::add(0x0003_0303),
            ],
        )),
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    let glow = fx_layer(
        3,
        fx_sprite(
            fwi - 9,
            38,
            8,
            20,
            Arc::new(FxArt::new(3, 2, FxTexels::Index8, vec![1, 0, 2, 2, 1, 0]).expect("sized")),
        ),
        vec![FxInk::add(0x0040_3020), FxInk::over(0x0020_3040, 0xa0)],
        FxSlot::Glow,
        FxSpace::Window,
        FxBlend::Exact,
    );
    let linear_under = fx_layer(
        4,
        fx_sprite(fwi - 12, 30, 12, 30, straight_art(4, 5)),
        vec![FxInk::over(0x00ff_e0c0, 200)],
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Linear,
    );
    let linear_over = fx_layer(
        5,
        fx_sprite(30, 6 * ch as i32 - 4, 20, 14, straight_art(5, 3)),
        vec![FxInk::over(0x00c0_e0ff, 230)],
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Linear,
    );
    let mut premul = Vec::new();
    for k in 0..12u8 {
        let a = [0u8, 255, 120, 0][usize::from(k % 4)];
        let c = if a == 0 { 0x18 } else { a / 2 };
        premul.extend_from_slice(&[c, c / 2, c / 3, a]);
    }
    let head = fx_layer(
        6,
        fx_sprite(
            10,
            2,
            40,
            16,
            Arc::new(FxArt::new(4, 3, FxTexels::Rgba8Premul, premul).expect("sized")),
        ),
        vec![],
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    vec![border, grid_over, glow, linear_under, linear_over, head]
}

#[test]
fn fx_layers_spill_byte_identical_over_the_frame_band() {
    let dark = Theme::default();
    let light = Theme {
        fg: 0x001A_1A1A,
        bg: 0x00F5_F5F0,
        cursor: 0x0033_3333,
        selection: 0x00B0_C4DE,
    };
    for theme in [dark, light] {
        let mut r = Renderer::from_system(16.0, theme).expect(crate::NO_FONT);
        r.set_pad(10);
        r.set_pad_top(3);
        r.set_head(24);
        let base = theme.bg & 0x00FF_FFFF;
        let ch = r.cell_size().1;
        let mut term = Terminal::new(6, 20);
        let (fw, fh) = r.frame_size(6, 20);
        let mut input = term.cell_frame(6, 20);
        input.fx_layers = band_layers(fw, fh, ch);
        input.fx_occluders = vec![
            PxRect::new(0, 8, 50, 14),
            PxRect::new(fw as i32 - 6, 44, fw as i32, 52),
        ];
        let spill_of = |r: &Renderer, i: &RenderInput| {
            let mut s = SpillBand::new();
            s.update(r, i);
            s
        };
        let spill = spill_of(&r, &input);
        let lit = assert_band_parity(&mut r, &input, &spill, base);
        for (i, n) in lit.iter().enumerate() {
            assert!(*n > 0, "strip {i} carries fx light (non-vacuous)");
        }
        input.glow_under = vec![
            GlowQuad::gradient(0, 2, 4, 60, 14, 0x0030_0810, 0x30, 0x0008_1030, 0x50),
            GlowQuad::gradient(
                1,
                (fw - 12) as u16,
                32,
                12,
                20,
                0x0020_3010,
                0x40,
                0x0010_2030,
                0x60,
            ),
        ];
        input.fire_patch = vec![FirePatch {
            row: 0,
            x: 0,
            y: 2,
            w: 70,
            h: 26,
            base_y: 40,
            peak_h: 44,
            phase: 977_331,
            temp: 210,
            strength: 230,
            lean: 6,
            cov_cap: 210,
            cell_h: 18,
            mode: FireMode::Add,
        }];
        input.cursor_glow_add = vec![GlowQuad {
            row: 0,
            x: (fw - 10) as u16,
            y: 36,
            w: 10,
            h: 20,
            color: 0x0018_3040,
            alpha: 0x40,
            color2: 0x0018_3040,
            alpha2: 0x40,
        }];
        let veil = |row, x: usize, y: usize, w: usize, h: usize| RainHalo {
            row,
            x: x as u16,
            y: y as u16,
            w: w as u16,
            h: h as u16,
            color: 0x0040_3830,
            cx: (x + w / 2) as u16,
            cy: (y + h / 2) as u16,
            rx: (w / 2 + 2) as u16,
            ry: (h / 2 + 2) as u16,
            mode: HaloMode::Over,
        };
        input.glow_halo = vec![
            veil(5, 20, fh - 12, 50, 12),
            veil(1, fw - 12, 34, 12, 26),
            veil(0, 0, 10, 40, 20),
        ];
        let spill = spill_of(&r, &input);
        assert_band_parity(&mut r, &input, &spill, base);

        let mut bare = input.clone();
        bare.fx_layers.clear();
        let partial = spill_of(&r, &bare);
        let (_, seams) = band_parity(&mut r, &input, &partial, base, false);
        assert!(seams > 0, "CONTROL: a spill without the fx shows a seam");
        let mut uncut = input.clone();
        uncut.fx_occluders.clear();
        let partial = spill_of(&r, &uncut);
        let (_, seams) = band_parity(&mut r, &input, &partial, base, false);
        assert!(
            seams > 0,
            "CONTROL: a spill that ignores the occluders shows a seam"
        );
    }
}

#[test]
fn fx_layers_tick_the_revision_only_when_the_band_changes() {
    let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
    r.set_pad(8);
    r.set_head(20);
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    let (fw, fh) = r.frame_size(6, 20);
    let (cw, ch) = r.cell_size();
    let mut spill = SpillBand::new();
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 0);

    let inner = fx_layer(
        1,
        fx_rect(2 * cw as i32, ch as i32, 4 * cw, ch),
        vec![FxInk::add(0x0020_2020); 2],
        FxSlot::Under,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    input.fx_layers = vec![inner.clone()];
    spill.update(&r, &input);
    let mut moved_inner = inner.clone();
    moved_inner.offset = (cw as i32, ch as i32);
    input.fx_layers = vec![moved_inner];
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 0, "grid-interior layers never tick");
    assert!(spill.rects().is_empty());

    let border = fx_layer(
        2,
        fx_rect(4, 4, 40, 6),
        vec![FxInk::add(0x0030_2010); 2],
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    input.fx_layers = vec![inner.clone(), border.clone()];
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1);
    assert!(!spill.rgba().iter().all(|&b| b == 0), "the border is lit");

    input.fx_layers = vec![inner.clone(), border.clone()];
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1, "settled layers never tick");
    assert!(spill.rects().is_empty());

    let mut moved = border.clone();
    moved.offset.0 = 30;
    input.fx_layers = vec![inner.clone(), moved.clone()];
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 2);
    let rects = spill.rects().to_vec();
    let covers = |x: i32, y: i32| {
        rects
            .as_chunks::<4>()
            .0
            .iter()
            .any(|q| (q[0]..q[0] + q[2]).contains(&x) && (q[1]..q[1] + q[3]).contains(&y))
    };
    assert!(
        covers(4, 4) && covers(73, 9),
        "prev ∪ cur extents: {rects:?}"
    );
    for q in rects.as_chunks::<4>().0 {
        assert!(q[0] >= 0 && q[1] >= 0 && q[0] + q[2] <= fw as i32 && q[1] + q[3] <= fh as i32);
    }

    input.fx_occluders = vec![PxRect::new(0, 0, 50, 8)];
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 3, "an occluder cutting band light ticks");
    input.fx_occluders.push(PxRect::new(
        3 * cw as i32 + 8,
        30 + ch as i32,
        5 * cw as i32 + 8,
        30 + 2 * ch as i32,
    ));
    spill.update(&r, &input);
    assert_eq!(
        spill.rev(),
        3,
        "an occluder off the band is not band content"
    );

    input.fx_layers = vec![inner];
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 4);
    assert!(!spill.rects().is_empty());
    assert!(spill.rgba().iter().all(|&b| b == 0));
}

#[test]
fn spill_band_composes_byte_identical_over_the_frame_band() {
    let dark = Theme::default();
    let light = Theme {
        fg: 0x001A_1A1A,
        bg: 0x00F5_F5F0,
        cursor: 0x0033_3333,
        selection: 0x00B0_C4DE,
    };
    for theme in [dark, light] {
        let mut r = Renderer::from_system(16.0, theme).expect(crate::NO_FONT);
        r.set_pad(10);
        r.set_pad_top(3);
        r.set_head(24);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        let (fw, fh) = r.frame_size(6, 20);
        input.glow_under = vec![
            GlowQuad {
                row: 0,
                x: 2,
                y: 4,
                w: 40,
                h: 14,
                color: 0x0020_1008,
                alpha: 0,
                color2: 0x0020_1008,
                alpha2: 0,
            },
            GlowQuad {
                row: 0,
                x: 0,
                y: 20,
                w: 18,
                h: 20,
                color: 0x0008_1020,
                alpha: 0,
                color2: 0x0008_1020,
                alpha2: 0,
            },
            GlowQuad {
                row: 0,
                x: 6,
                y: 22,
                w: 24,
                h: 12,
                color: 0x0009_1216,
                alpha: 0x16,
                color2: 0x0009_1216,
                alpha2: 0x16,
            },
            GlowQuad::gradient(0, 20, 6, 100, 8, 0x0060_0000, 0, 0x0000_0060, 0),
            GlowQuad::gradient(0, 4, 18, 60, 14, 0x0030_0810, 0x30, 0x0008_1030, 0x50),
        ];
        input.fire_patch = vec![
            FirePatch {
                row: 0,
                x: 0,
                y: 4,
                w: 80,
                h: 30,
                base_y: 44,
                peak_h: 48,
                phase: 977_331,
                temp: 210,
                strength: 230,
                lean: 6,
                cov_cap: 210,
                cell_h: 18,
                mode: FireMode::Add,
            },
            FirePatch {
                row: 0,
                x: 30,
                y: 0,
                w: 60,
                h: 30,
                base_y: 40,
                peak_h: 40,
                phase: 512_101,
                temp: 180,
                strength: 200,
                lean: -4,
                cov_cap: 190,
                cell_h: 18,
                mode: FireMode::Over,
            },
        ];
        input.cursor_glow_add = vec![
            GlowQuad {
                row: 0,
                x: (fw - 8) as u16,
                y: 40,
                w: 8,
                h: 12,
                color: 0x0018_3040,
                alpha: 0,
                color2: 0x0018_3040,
                alpha2: 0,
            },
            GlowQuad {
                row: 5,
                x: 20,
                y: (fh - 8) as u16,
                w: 30,
                h: 8,
                color: 0x0030_1010,
                alpha: 0,
                color2: 0x0030_1010,
                alpha2: 0,
            },
        ];
        input.glow_halo = vec![
            RainHalo {
                row: 1,
                x: 0,
                y: 40,
                w: 12,
                h: 16,
                color: 0x0060_80FF,
                cx: 5,
                cy: 48,
                rx: 9,
                ry: 10,
                mode: HaloMode::Add,
            },
            RainHalo {
                row: 5,
                x: 24,
                y: (fh - 10) as u16,
                w: 40,
                h: 10,
                color: 0x0040_3830,
                cx: 44,
                cy: (fh - 4) as u16,
                rx: 22,
                ry: 9,
                mode: HaloMode::Over,
            },
        ];
        let mut spill = SpillBand::new();
        spill.update(&r, &input);
        assert_eq!(spill.rev(), 1, "band content must tick the revision once");
        let strips = spill.geom.strips();
        assert_eq!(spill.geom.grid_top(), 3 + 24);
        assert_eq!(strips[1].y, fh - (2 * 10 - 3));
        assert_eq!(strips[1].h, 2 * 10 - 3);
        assert_eq!(strips[2].h, 6 * r.cell_size().1);
        let lit = assert_band_parity(&mut r, &input, &spill, theme.bg & 0x00FF_FFFF);
        for (i, n) in lit.iter().enumerate() {
            assert!(
                *n > 0,
                "strip {i} must carry lit spill pixels (non-vacuous)"
            );
        }
    }
}

#[test]
fn asymmetric_top_pad_invalidates_spill_geometry_at_constant_frame_size() {
    let theme = Theme::default();
    let mut r = Renderer::from_system(16.0, theme).expect(crate::NO_FONT);
    const PAD: usize = 10;
    const HEAD: usize = 17;
    const TOP_A: usize = 3;
    const TOP_B: usize = 8;
    let (rows, cols) = (6usize, 20usize);
    r.set_pad(PAD);
    r.set_pad_top(TOP_A);
    r.set_head(HEAD);
    let frame_size = r.frame_size(rows, cols);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let mut input = term.cell_frame(rows, cols);
    input.cursor_glow_add = vec![GlowQuad {
        row: 0,
        x: 4,
        y: 4,
        w: 24,
        h: 10,
        color: 0x0020_3040,
        alpha: 0,
        color2: 0x0020_3040,
        alpha2: 0,
    }];
    let mut spill = SpillBand::new();
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1);
    assert_eq!(spill.geom.pad_top, TOP_A);
    let len = spill.rgba().len();
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1, "unchanged geometry/content stays cached");
    assert!(spill.rects().is_empty());

    r.set_pad_top(TOP_B);
    assert_eq!(r.frame_size(rows, cols), frame_size);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 2, "grid-origin move must re-raster live spill");
    assert_eq!(spill.geom.pad_top, TOP_B);
    assert_eq!(
        spill.rgba().len(),
        len,
        "vertical padding extent is conserved"
    );
    let expected_rects: Vec<i32> = spill
        .geom
        .strips()
        .iter()
        .flat_map(|s| [s.x as i32, s.y as i32, s.w as i32, s.h as i32])
        .collect();
    assert_eq!(
        spill.rects(),
        expected_rects,
        "origin move dirties the full band"
    );
    assert_band_parity(&mut r, &input, &spill, theme.bg & 0x00FF_FFFF);
}

/// A glow stream's ramps spill as the quads they stand for: of each, the
/// rows whose lights reach the band (by its columns, above the grid's top,
/// at and below its foot; a run whole where any of its rows does), drawn
/// in the stream's order, frame after frame, byte for byte, with the same
/// dirty rects and revisions. A ramp inside the grid box spills nothing.
#[test]
fn glow_ramps_spill_as_their_quads() {
    let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
    r.set_pad(10);
    r.set_pad_top(3);
    r.set_head(24);
    let mut term = Terminal::new(6, 20);
    let base = term.cell_frame(6, 20);
    let (fw, fh) = r.frame_size(6, 20);
    let (gt, gb) = (27u16, (fh - 17) as u16);
    let fw16 = fw as u16;
    let end = |c: u32, k: u16, a: u8| {
        crate::glow_end(
            premul_rgb(c.wrapping_add(u32::from(k) * 0x0003_0507), 0xC0),
            a,
        )
    };
    // The stream at `shift`: quads between ramps reaching the band by its
    // left and right columns, across the grid's top (dark rows above its
    // first lit one, its run across the top), at its foot, and one inside
    // the grid box; the left one's over light drawn under a quad.
    let stream = |shift: u16| {
        let mut ramps = GlowRamps::default();
        let quads = vec![
            GlowQuad::gradient(0, 2, gt + 2, 30, 6, 0x0030_0810, 0x30, 0x0008_1030, 0x50),
            GlowQuad::flat(1, 40, gt + 30, 12, 3, 0x0010_2030, 0),
            GlowQuad::gradient(2, fw16 - 9, gt + 8, 8, 5, 0x0020_3010, 0, 0x0010_2030, 0),
        ];
        ramps.open(
            0,
            0,
            (2, gt + 4, 30),
            (end(0x0040_2010, 0, 0x80), end(0x0010_4020, 0, 0x40)),
        );
        for k in 1..12u16 {
            let lit = if k == 7 {
                (0, 0)
            } else {
                (end(0x0040_2010, k, 0x80), end(0x0010_4020, k, 0x40))
            };
            if k == 3 {
                assert!(ramps.extend_run(0, 4, lit));
            } else if !(3..=6).contains(&k) {
                ramps.extend_last(0, lit);
            }
        }
        let y = gt - 6 + shift;
        ramps.open(1, 0, (40, y, 20), (0, 0));
        ramps.extend_last(0, (0, 0));
        for k in 2..4u16 {
            ramps.extend_last(0, (end(0x0050_3020, k, 0), end(0x0020_3050, k, 0)));
        }
        assert!(ramps.extend_run(0, 5, (end(0x0060_6020, 0, 0), end(0x0020_6060, 0, 0))));
        for k in 9..14u16 {
            ramps.extend_last(0, (end(0x0050_3020, k, 0), end(0x0020_3050, k, 0)));
        }
        ramps.open(
            2,
            5,
            (60, gb - 5, 16),
            (end(0x0030_5070, 0, 0), end(0x0070_5030, 0, 0)),
        );
        for k in 1..9u16 {
            ramps.extend_last(
                u16::from(k == 4),
                (end(0x0030_5070, k, 0), end(0x0070_5030, k, 0)),
            );
        }
        ramps.open(2, 2, (50, gt + 20, 10), (end(0x0050_5050, 0, 0), 0));
        for k in 1..6u16 {
            ramps.extend_last(0, (end(0x0050_5050, k, 0), 0));
        }
        ramps.open(
            3,
            3,
            (fw16 - 14, gt + 40, 12),
            (end(0x0010_5030, 0, 0), end(0x0030_1050, 0, 0)),
        );
        for k in 1..7u16 {
            ramps.extend_last(0, (end(0x0010_5030, k, 0), end(0x0030_1050, k, 0)));
        }
        ramps.lit = ramps.count_lit();
        (quads, ramps)
    };
    // The ramp across the grid's top alone, after a clear frame, beside a
    // light in the left column ending above its run's foot: the run's rows
    // inside the grid box spill nothing, but its whole rect widens the
    // dirty rect down the left column.
    let alone = || {
        let (_, all) = stream(0);
        let mut ramps = GlowRamps::default();
        ramps.copy_rows(&all.items[1], &all.ends, 0, (0, all.items[1].h));
        (
            vec![GlowQuad::flat(0, 2, gt + 1, 6, 1, 0x0030_2010, 0)],
            ramps,
        )
    };
    let (mut spill, mut legacy) = (SpillBand::new(), SpillBand::new());
    let mut carried = 0;
    for (frame, shift) in [Some(0), Some(3), Some(3), None, Some(99), None, Some(1)]
        .into_iter()
        .enumerate()
    {
        let (mut with, mut without) = (base.clone(), base.clone());
        if let Some(shift) = shift {
            let (quads, ramps) = if shift == 99 { alone() } else { stream(shift) };
            ramps.expand_into(&quads, &mut without.cursor_glow_add);
            ramps.expand_into(&quads, &mut without.glow_under);
            (with.glow_under, with.glow_under_ramps) = (quads.clone(), ramps.clone());
            (with.cursor_glow_add, with.cursor_glow_add_ramps) = (quads, ramps);
        }
        spill.update(&r, &with);
        legacy.update(&r, &without);
        carried = carried.max(spill.prev.glow.1.items.len());
        assert_eq!(
            spill.rects(),
            legacy.rects(),
            "frame {frame}: the dirty rects"
        );
        assert_eq!(spill.rev(), legacy.rev(), "frame {frame}: the revision");
        assert!(
            spill.rgba() == legacy.rgba(),
            "frame {frame}: the band's bytes"
        );
    }
    assert_eq!(
        carried, 4,
        "four ramps reach the band, the grid box's does not"
    );
    assert!(
        legacy.rev() == 6 && spill.rgba().iter().any(|&b| b != 0),
        "the band lit"
    );
}

#[test]
fn rev_advances_only_on_band_content_change() {
    let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
    r.set_pad(8);
    r.set_head(20);
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    let (fw, fh) = r.frame_size(6, 20);
    let mut spill = SpillBand::new();

    spill.update(&r, &input);
    assert_eq!(spill.rev(), 0, "no content → no revision");
    assert!(spill.rects().is_empty());
    assert!(
        !spill.rgba().is_empty(),
        "nonzero chrome keeps the band buffer allocated"
    );
    assert!(
        spill.rgba().iter().all(|&b| b == 0),
        "idle band transparent"
    );

    let (ix, iy) = ((fw / 2) as u16, (fh / 2) as u16);
    input.cursor_glow_add = vec![GlowQuad {
        row: 2,
        x: ix,
        y: iy,
        w: 8,
        h: 8,
        color: 0x0010_2030,
        alpha: 0,
        color2: 0x0010_2030,
        alpha2: 0,
    }];
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 0, "grid-interior emissions must not tick");

    input.cursor_glow_add[0].x = ix + 6;
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 0, "typing-only frame must not tick");
    assert!(spill.rects().is_empty());

    input.cursor_glow_add.push(GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: 20,
        h: 12,
        color: 0x0020_1008,
        alpha: 0,
        color2: 0x0020_1008,
        alpha2: 0,
    });
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1);
    let rects = spill.rects().to_vec();
    assert!(!rects.is_empty());
    for rect in rects.as_chunks::<4>().0 {
        let (x, y, w, h) = (rect[0], rect[1], rect[2], rect[3]);
        assert!(w > 0 && h > 0);
        assert!(x >= 0 && y >= 0 && x + w <= fw as i32 && y + h <= fh as i32);
    }

    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1, "settled content must not re-tick");
    assert!(spill.rects().is_empty());

    input.cursor_glow_add[0].x = ix;
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1);
    assert!(spill.rects().is_empty());

    input.cursor_glow_add.truncate(1);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 2);
    assert!(
        !spill.rects().is_empty(),
        "vanished light must report rects"
    );
    assert!(spill.rgba().iter().all(|&b| b == 0));
}

#[test]
fn spill_ptr_stays_stable_across_content_re_rasters() {
    let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
    r.set_pad(8);
    r.set_head(20);
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    input.cursor_glow_add = vec![GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: 16,
        h: 10,
        color: 0x0020_2020,
        alpha: 0,
        color2: 0x0020_2020,
        alpha2: 0,
    }];
    let mut spill = SpillBand::new();
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1);
    let ptr = spill.rgba().as_ptr();
    let len = spill.rgba().len();

    input.cursor_glow_add[0].x = 6;
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 2);
    assert_eq!(
        spill.rgba().as_ptr(),
        ptr,
        "content re-raster must not move the export"
    );
    assert_eq!(spill.rgba().len(), len);

    r.set_pad(12);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 3, "resize with content re-rasters");
    let geom = SpillGeom {
        pad: 12,
        pad_top: 12,
        head: 20,
        frame_w: r.frame_size(6, 20).0,
        frame_h: r.frame_size(6, 20).1,
    };
    assert_eq!(spill.rgba().len(), geom.band_px() * 4);
}

#[test]
fn zero_chrome_is_identity_and_transitions_clear() {
    let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    input.cursor_glow_add = vec![GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: 10,
        h: 10,
        color: 0x0020_2020,
        alpha: 0,
        color2: 0x0020_2020,
        alpha2: 0,
    }];
    let mut spill = SpillBand::new();
    spill.update(&r, &input);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 0, "0/0 chrome: the revision never advances");
    assert_eq!(spill.rgba().len(), 0, "0/0 chrome: empty buffer");
    assert!(spill.rects().is_empty());

    r.set_pad(8);
    r.set_head(20);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1);
    assert!(!spill.rgba().is_empty());

    r.set_pad(0);
    r.set_head(0);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 2, "chrome→0 with content clears with one tick");
    assert_eq!(spill.rgba().len(), 0);
    assert!(spill.rects().is_empty());
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 2, "steady 0/0 chrome stays still");
}

#[test]
fn include_veils_policy_gates_over_halos() {
    let mut r = Renderer::from_system(16.0, Theme::default()).expect(crate::NO_FONT);
    r.set_pad(8);
    r.set_head(20);
    let mut term = Terminal::new(6, 20);
    let mut input = term.cell_frame(6, 20);
    let (fw, _) = r.frame_size(6, 20);
    input.glow_halo = vec![
        RainHalo {
            row: 0,
            x: 4,
            y: 4,
            w: 10,
            h: 10,
            color: 0x0040_80C0,
            cx: 8,
            cy: 8,
            rx: 7,
            ry: 7,
            mode: HaloMode::Add,
        },
        RainHalo {
            row: 0,
            x: 40,
            y: 4,
            w: 12,
            h: 12,
            color: 0x0020_2020,
            cx: 46,
            cy: 10,
            rx: 8,
            ry: 8,
            mode: HaloMode::Over,
        },
    ];
    let mut spill = SpillBand::new();
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 1);
    let alpha_at = |spill: &SpillBand, x: usize, y: usize| spill.rgba()[(y * fw + x) * 4 + 3];
    assert!(alpha_at(&spill, 8, 8) > 0, "Add ember centre lit");
    assert!(alpha_at(&spill, 46, 10) > 0, "veil centre lit by default");

    spill.set_include_veils(false);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 2, "policy flip with veils present re-rasters");
    assert!(
        alpha_at(&spill, 8, 8) > 0,
        "Add light survives the veil gate"
    );
    assert_eq!(alpha_at(&spill, 46, 10), 0, "veil excluded from the spill");

    spill.set_include_veils(true);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 3);
    assert!(alpha_at(&spill, 46, 10) > 0, "veil restored");

    input.glow_halo.truncate(1);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 4, "removing the veil is a content change");
    spill.set_include_veils(false);
    spill.update(&r, &input);
    assert_eq!(spill.rev(), 4, "policy flip without veils present is free");
}

/// A ramp whose run crosses the frame's foot keeps only the rows above it: the run widens
/// the kept span to its own rows, never past the rows the frame holds.
#[test]
fn a_run_across_the_frame_foot_keeps_no_row_below_it() {
    let geom = SpillGeom {
        pad: 8,
        pad_top: 8,
        head: 0,
        frame_w: 100,
        frame_h: 50,
    };
    let ramp = |y| GlowRamp {
        x: 0,
        y,
        w: 4,
        h: 6,
        join: 0,
        joined: 5,
        ..GlowRamp::default()
    };
    let ends: [GlowEnd; 2] = [1, 1];
    assert_eq!(geom.band_rows(&ramp(48), &ends), Some((0, 2)));
    assert_eq!(
        geom.band_rows(&ramp(10), &ends),
        Some((0, 6)),
        "control: inside the frame"
    );
}
