// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Property and negative-control tests for the fx rasterizer.

use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, FxSprite,
    FxTexels, PxRect,
};

use super::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo).max(0) as u64 + 1) as i64
    }
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

fn ground(w: usize, h: usize, rng: &mut Rng) -> Vec<u32> {
    (0..w * h)
        .map(|_| {
            let t = if rng.chance(1, 4) {
                (rng.below(256) as u32) << 24
            } else {
                0
            };
            t | (rng.next() as u32 & 0x00FF_FFFF)
        })
        .collect()
}

fn ink(rng: &mut Rng) -> FxInk {
    let alpha = match rng.below(4) {
        0 => 0,
        1 => 255,
        _ => rng.below(254) as u8 + 1,
    };
    let ch = |rng: &mut Rng| {
        if alpha == 0 {
            rng.below(256) as u32
        } else {
            rng.below(u64::from(alpha) + 1) as u32
        }
    };
    let color = ch(rng) << 16 | ch(rng) << 8 | ch(rng);
    FxInk { color, alpha }
}

fn art(rng: &mut Rng, texels: FxTexels, max_index: u8) -> Arc<FxArt> {
    let (w, h) = (rng.range(1, 12) as u16, rng.range(1, 10) as u16);
    let n = usize::from(w) * usize::from(h);
    let data: Vec<u8> = match texels {
        FxTexels::Index8 => (0..n)
            .map(|_| {
                if rng.chance(1, 4) {
                    0
                } else {
                    rng.range(1, i64::from(max_index)) as u8
                }
            })
            .collect(),
        FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => {
            let pool: Vec<[u8; 4]> = (0..4)
                .map(|k| {
                    if k == 0 {
                        [0, 0, 0, 0]
                    } else {
                        let a = [0u8, 255, rng.below(256) as u8][rng.below(3) as usize];
                        let c = |rng: &mut Rng| {
                            if texels == FxTexels::Rgba8Premul && a > 0 {
                                rng.below(u64::from(a) + 1) as u8
                            } else {
                                rng.below(256) as u8
                            }
                        };
                        [c(rng), c(rng), c(rng), a]
                    }
                })
                .collect();
            (0..n).flat_map(|_| pool[rng.below(4) as usize]).collect()
        }
    };
    Arc::new(FxArt::new(w, h, texels, data).expect("sized"))
}

fn dest(rng: &mut Rng, src: u16) -> u16 {
    let s = u16::try_from(rng.range(1, 13)).unwrap();
    if rng.chance(1, 2) {
        src * s
    } else {
        u16::try_from(rng.range(1, i64::from(src) * 13)).unwrap()
    }
}

fn sprite(rng: &mut Rng, texels: FxTexels, palette_len: u32, x: i32, y: i32) -> FxSprite {
    let max_index = (palette_len + 3).min(255) as u8;
    let art = art(rng, texels, max_index.max(1));
    FxSprite {
        x,
        y,
        dw: dest(rng, art.w()),
        dh: dest(rng, art.h()),
        flip_x: rng.chance(1, 2),
        ink: rng.below(u64::from(palette_len) + 2) as u32,
        clip: if rng.chance(1, 3) {
            PxRect::new(
                x + rng.range(-4, 20) as i32,
                y + rng.range(-4, 20) as i32,
                x + rng.range(10, 200) as i32,
                y + rng.range(10, 200) as i32,
            )
        } else {
            PxRect::EVERYWHERE
        },
        art,
    }
}

fn item(rng: &mut Rng, palette_len: u32, w: usize, h: usize) -> FxItem {
    let x = rng.range(-40, w as i64) as i32;
    let y = rng.range(-40, h as i64) as i32;
    match rng.below(5) {
        0 => FxItem::Rect(FxRect {
            x,
            y,
            w: rng.range(0, 60) as u16,
            h: rng.range(0, 60) as u16,
            ink: rng.below(u64::from(palette_len) + 1) as u32,
        }),
        1 => FxItem::Ramps(FxRamps {
            x,
            y,
            w: rng.range(0, 60) as u16,
            h: rng.range(0, 40) as u16,
            ink: rng.below(u64::from(palette_len)) as u32,
        }),
        2 => FxItem::Sprite(sprite(rng, FxTexels::Rgba8Premul, palette_len, x, y)),
        _ => FxItem::Sprite(sprite(rng, FxTexels::Index8, palette_len, x, y)),
    }
}

fn palette(rng: &mut Rng, n: usize) -> Vec<FxInk> {
    (0..n).map(|_| ink(rng)).collect()
}

fn frame(rng: &mut Rng) -> FxFrame {
    let cell_h = rng.range(3, 21) as usize;
    let rows = rng.range(1, 7) as usize;
    let pad = rng.range(0, 9) as usize;
    let grid_top = pad + rng.range(0, 12) as usize;
    FxFrame {
        w: rng.range(20, 90) as usize,
        h: grid_top + rows * cell_h + rng.range(0, 9) as usize,
        pad,
        grid_top,
        cell_h,
        rows,
    }
}

fn stamp_own(px: &mut [u32], w: usize, s: FxSlice, item: &FxItem, o: (i64, i64), pal: &[FxInk]) {
    let mut cols = Vec::new();
    if let FxItem::Sprite(sp) = item {
        sprite_cols(sp, &mut cols);
    }
    stamp(&mut FramePx::new(px, w), s, item, o, pal, &cols);
}

fn stamp_sprite_own(
    px: &mut [u32],
    w: usize,
    s: FxSlice,
    o: (i64, i64),
    sp: &FxSprite,
    pal: &[FxInk],
) {
    let mut cols = Vec::new();
    sprite_cols(sp, &mut cols);
    stamp_sprite(&mut FramePx::new(px, w), s, o, sp, pal, &cols);
}

fn whole(area: Area) -> FxSlice {
    FxSlice {
        x0: area.x0 as usize,
        x1: area.x1 as usize,
        y0: area.y0 as usize,
        y1: area.y1 as usize,
    }
}

type Drawn<'a> = (&'a FxItem, (i64, i64), Area, &'a [FxInk]);

fn draw_rects<'a>(
    f: &FxFrame,
    layers: &'a [FxLayer],
    slot: FxSlot,
    blend: FxBlend,
) -> Vec<Drawn<'a>> {
    let mut out = Vec::new();
    for layer in layers {
        if layer.slot != slot || layer.blend != blend {
            continue;
        }
        let Some((shift, clip)) = layer_place(f, layer) else {
            continue;
        };
        for it in layer.content.items() {
            if it.blend() != blend {
                continue;
            }
            let (o, area) = item_place(it, shift);
            let d = area.intersect(clip);
            if !d.is_empty() {
                out.push((it, o, d, layer.content.palette()));
            }
        }
    }
    out
}

fn random_layers(rng: &mut Rng, f: &FxFrame, slot: FxSlot) -> Vec<FxLayer> {
    (0..rng.range(1, 3))
        .map(|id| {
            let n = rng.range(2, 12) as usize;
            let pal = palette(rng, n);
            let items = (0..rng.range(1, 6))
                .map(|_| item(rng, n as u32, f.w, f.h))
                .collect();
            let mut layer = FxLayer::new(
                id as u64,
                Arc::new(FxContent::new_unchecked(items, pal)),
                slot,
                if rng.chance(1, 2) {
                    FxSpace::Window
                } else {
                    FxSpace::Grid
                },
                FxBlend::Exact,
            );
            layer.offset = (rng.range(-20, 20) as i32, rng.range(-20, 20) as i32);
            if rng.chance(1, 3) {
                layer.clip = PxRect::new(
                    rng.range(-10, 30) as i32,
                    rng.range(-10, 30) as i32,
                    rng.range(20, 120) as i32,
                    rng.range(20, 120) as i32,
                );
            }
            layer
        })
        .collect()
}

#[test]
fn fx_run_lists_are_law_f() {
    let mut rng = Rng(0x5eed_f00d_1234_5678);
    let (mut out_of_range, mut control_differs) = (0, 0);
    for case in 0..4000 {
        let (w, h) = (96usize, 80usize);
        let texels = [
            FxTexels::Index8,
            FxTexels::Rgba8Premul,
            FxTexels::Rgba8Straight,
        ][case % 3];
        let n = rng.range(1, 10) as usize;
        let pal = palette(&mut rng, n);
        let (x, y) = (rng.range(-60, 90) as i32, rng.range(-60, 70) as i32);
        let sp = sprite(&mut rng, texels, n as u32, x, y);
        if texels == FxTexels::Index8
            && u64::from(sp.ink) + u64::from(sp.art.max_index()) > n as u64
        {
            out_of_range += 1;
        }
        let o = (i64::from(sp.x), i64::from(sp.y));
        let frame_area = Area {
            x0: 0,
            y0: 0,
            x1: w as i64,
            y1: h as i64,
        };
        let area = Area::at(o.0, o.1, sp.dw, sp.dh)
            .intersect(Area::of(sp.clip, 0, 0))
            .intersect(frame_area);
        if area.is_empty() {
            continue;
        }
        let x0 = rng.range(area.x0, area.x1 - 1);
        let y0 = rng.range(area.y0, area.y1 - 1);
        let s = FxSlice {
            x0: x0 as usize,
            x1: rng.range(x0 + 1, area.x1) as usize,
            y0: y0 as usize,
            y1: rng.range(y0 + 1, area.y1) as usize,
        };
        let base = ground(w, h, &mut rng);
        let mut runs = base.clone();
        stamp_sprite_own(&mut runs, w, s, o, &sp, &pal);
        let mut oracle = base.clone();
        stamp_sprite_per_pixel(&mut oracle, w, s, o, &sp, &pal, 0);
        assert!(
            runs == oracle,
            "case {case}: run lists diverge from law F ({texels:?}, {}x{} → {}x{}, flip {})",
            sp.art.w(),
            sp.art.h(),
            sp.dw,
            sp.dh,
            sp.flip_x
        );
        let mut shifted = base;
        stamp_sprite_per_pixel(&mut shifted, w, s, o, &sp, &pal, 1);
        if shifted != runs {
            control_differs += 1;
        }
    }
    assert!(
        out_of_range > 100,
        "the corpus reaches past the palette ({out_of_range})"
    );
    assert!(
        control_differs > 1000,
        "CONTROL: an off-by-one texel must differ ({control_differs})"
    );
}

#[test]
fn fx_slicing_changes_no_byte() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let mut scratch = FxScratch::default();
    let mut control_differs = 0;
    let mut sliced_items = 0;
    for case in 0..1500 {
        let f = frame(&mut rng);
        let layers = random_layers(&mut rng, &f, FxSlot::Over);
        let base = ground(f.w, f.h, &mut rng);

        let mut engine = base.clone();
        draw_layers(
            &mut engine,
            &f,
            &layers,
            &[],
            FxSlot::Over,
            FxBlend::Exact,
            None,
            &mut scratch,
        );

        let rects = draw_rects(&f, &layers, FxSlot::Over, FxBlend::Exact);
        let mut uncut = base.clone();
        let mut cut = base.clone();
        let mut from_slice = base.clone();
        for &(it, o, d, pal) in &rects {
            stamp_own(&mut uncut, f.w, whole(d), it, o, pal);
            let xs = cuts(&mut rng, d.x0, d.x1);
            let ys = cuts(&mut rng, d.y0, d.y1);
            for wy in ys.windows(2) {
                for wx in xs.windows(2) {
                    let s = FxSlice {
                        x0: wx[0] as usize,
                        x1: wx[1] as usize,
                        y0: wy[0] as usize,
                        y1: wy[1] as usize,
                    };
                    stamp_own(&mut cut, f.w, s, it, o, pal);
                }
            }
            if f.band_of(d.y0 as usize) != f.band_of(d.y1 as usize - 1) {
                sliced_items += 1;
            }
            for_each_slice(&f, d, &|_| true, |s, _| {
                stamp_own(&mut from_slice, f.w, s, it, (s.x0 as i64, s.y0 as i64), pal);
            });
        }
        assert!(engine == uncut, "case {case}: band slicing changed a byte");
        assert!(cut == uncut, "case {case}: an arbitrary cut changed a byte");
        if from_slice != uncut {
            control_differs += 1;
        }

        let occluders: Vec<PxRect> = (0..rng.range(1, 4))
            .map(|_| {
                let x = rng.range(-10, f.w as i64) as i32;
                let y = rng.range(-10, f.h as i64) as i32;
                PxRect::at(x, y, rng.range(1, 50) as u32, rng.range(1, 40) as u32)
            })
            .collect();
        let mut occluded = base.clone();
        draw_layers(
            &mut occluded,
            &f,
            &layers,
            &occluders,
            FxSlot::Over,
            FxBlend::Exact,
            None,
            &mut scratch,
        );
        for y in 0..f.h {
            for x in 0..f.w {
                let inside = occluders
                    .iter()
                    .any(|o| o.overlaps(PxRect::at(x as i32, y as i32, 1, 1)));
                let want = if inside {
                    base[y * f.w + x]
                } else {
                    uncut[y * f.w + x]
                };
                assert_eq!(
                    occluded[y * f.w + x],
                    want,
                    "case {case}: pixel ({x}, {y}) inside={inside}"
                );
            }
        }
    }
    assert!(
        sliced_items > 500,
        "the corpus crosses band edges ({sliced_items})"
    );
    assert!(
        control_differs > 300,
        "CONTROL: slice-origin addressing must diverge ({control_differs})"
    );
}

fn cuts(rng: &mut Rng, lo: i64, hi: i64) -> Vec<i64> {
    let mut v = vec![lo, hi];
    for _ in 0..rng.below(4) {
        v.push(rng.range(lo, hi));
    }
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn fx_nearest_map_is_integer() {
    let pal: Vec<FxInk> = (0..16u32)
        .map(|k| FxInk::over((0x00_10_00 * (k + 1)) | 0x40, 255))
        .collect();
    let mut centre_differs = 0;
    for aw in 1..=16u16 {
        let row = Arc::new(
            FxArt::new(aw, 1, FxTexels::Index8, (1..=aw as u8).collect::<Vec<_>>()).unwrap(),
        );
        let col = Arc::new(
            FxArt::new(1, aw, FxTexels::Index8, (1..=aw as u8).collect::<Vec<_>>()).unwrap(),
        );
        for dw in 1..=13 * aw {
            for flip in [false, true] {
                let w = usize::from(dw);
                let mut px = vec![0u32; w];
                let sp = FxSprite {
                    x: 0,
                    y: 0,
                    dw,
                    dh: 1,
                    flip_x: flip,
                    art: row.clone(),
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                };
                let s = FxSlice {
                    x0: 0,
                    x1: w,
                    y0: 0,
                    y1: 1,
                };
                stamp_sprite_own(&mut px, w, s, (0, 0), &sp, &pal);
                for (l, &p) in px.iter().enumerate() {
                    let t = l * usize::from(aw) / w;
                    let t = if flip { usize::from(aw) - 1 - t } else { t };
                    assert_eq!(p, pal[t].color, "w {aw} → dw {dw}, flip {flip}, column {l}");
                    let centre = ((2 * l + 1) * usize::from(aw) / (2 * w)).min(usize::from(aw) - 1);
                    let centre = if flip {
                        usize::from(aw) - 1 - centre
                    } else {
                        centre
                    };
                    if centre != t {
                        centre_differs += 1;
                    }
                }
            }
            let h = usize::from(dw);
            let mut px = vec![0u32; h];
            let sp = FxSprite {
                x: 0,
                y: 0,
                dw: 1,
                dh: dw,
                flip_x: false,
                art: col.clone(),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            };
            let s = FxSlice {
                x0: 0,
                x1: 1,
                y0: 0,
                y1: h,
            };
            stamp_sprite_own(&mut px, 1, s, (0, 0), &sp, &pal);
            for (m, &p) in px.iter().enumerate() {
                assert_eq!(
                    p,
                    pal[m * usize::from(aw) / h].color,
                    "h {aw} → dh {dw}, row {m}"
                );
            }
        }
    }
    assert!(
        centre_differs > 1000,
        "CONTROL: the pixel-centre map must name other texels ({centre_differs})"
    );
}

#[test]
fn fx_rects_and_ramps_are_the_glow_quad_law() {
    let mut rng = Rng(0xfeed_beef_0000_0001);
    for case in 0..2000 {
        let (w, h) = (64usize, 24usize);
        let n = 8;
        let pal = palette(&mut rng, n);
        let base = ground(w, h, &mut rng);
        let (ox, oy) = (rng.range(-20, 50), rng.range(-6, 20));
        let (rw, rh) = (rng.range(1, 40) as u16, rng.range(1, 10) as u16);
        let area = Area::at(ox, oy, rw, rh).intersect(Area {
            x0: 0,
            y0: 0,
            x1: w as i64,
            y1: h as i64,
        });
        if area.is_empty() {
            continue;
        }
        let x0 = rng.range(area.x0, area.x1 - 1);
        let s = FxSlice {
            x0: x0 as usize,
            x1: rng.range(x0 + 1, area.x1) as usize,
            y0: area.y0 as usize,
            y1: area.y1 as usize,
        };
        let law = |dst: u32, c0: FxInk, c1: FxInk, x: usize| {
            let i = (x as i64 - ox) as u32;
            let a = glow_lerp(u32::from(c0.alpha), u32::from(c1.alpha), i, u32::from(rw)) as u8;
            over_premul(dst, glow_lerp_rgb(c0.color, c1.color, i, u32::from(rw)), a)
        };
        let ink0 = rng.below(n as u64 - 1) as u32;
        let mut pal_r = pal.clone();
        if rng.chance(1, 3) {
            pal_r[ink0 as usize + 1] = pal_r[ink0 as usize];
        }
        let rect = FxItem::Rect(FxRect {
            x: ox as i32,
            y: oy as i32,
            w: rw,
            h: rh,
            ink: ink0,
        });
        let mut got = base.clone();
        stamp_own(&mut got, w, s, &rect, (ox, oy), &pal_r);
        for y in 0..h {
            for x in 0..w {
                let inside = (s.x0..s.x1).contains(&x) && (s.y0..s.y1).contains(&y);
                let want = if inside {
                    law(
                        base[y * w + x],
                        pal_r[ink0 as usize],
                        pal_r[ink0 as usize + 1],
                        x,
                    )
                } else {
                    base[y * w + x]
                };
                assert_eq!(got[y * w + x], want, "case {case}: rect pixel ({x}, {y})");
            }
        }
        let ramps = FxItem::Ramps(FxRamps {
            x: ox as i32,
            y: oy as i32,
            w: rw,
            h: rh,
            ink: 0,
        });
        let mut got = base.clone();
        stamp_own(&mut got, w, s, &ramps, (ox, oy), &pal);
        for y in s.y0..s.y1 {
            let r = (y as i64 - oy) as usize;
            for x in s.x0..s.x1 {
                let want = match (pal.get(2 * r), pal.get(2 * r + 1)) {
                    (Some(&c0), Some(&c1)) => law(base[y * w + x], c0, c1, x),
                    _ => base[y * w + x],
                };
                assert_eq!(got[y * w + x], want, "case {case}: ramps pixel ({x}, {y})");
            }
        }
    }
}

#[test]
fn fx_out_of_range_writes_nothing() {
    let (w, h) = (8usize, 4usize);
    let base: Vec<u32> = (0..(w * h) as u32).map(|i| 0x0101_0101 * (i % 7)).collect();
    let pal = vec![FxInk::over(0x00_80_40_20, 255); 2];
    let all = FxSlice {
        x0: 0,
        x1: w,
        y0: 0,
        y1: h,
    };
    let rect = FxItem::Rect(FxRect {
        x: 0,
        y: 0,
        w: 8,
        h: 4,
        ink: 1,
    });
    let mut px = base.clone();
    stamp_own(&mut px, w, all, &rect, (0, 0), &pal);
    assert_eq!(px, base, "a rect reaching past the palette");
    let art = Arc::new(FxArt::new(4, 1, FxTexels::Index8, vec![1, 2, 3, 0]).unwrap());
    let sp = FxItem::Sprite(FxSprite {
        x: 0,
        y: 0,
        dw: 8,
        dh: 4,
        flip_x: false,
        art,
        ink: 0,
        clip: PxRect::EVERYWHERE,
    });
    let mut px = base.clone();
    stamp_own(&mut px, w, all, &sp, (0, 0), &pal);
    for y in 0..h {
        for x in 0..w {
            let b = base[y * w + x];
            let want = if x < 4 {
                (b & 0xFF00_0000) | pal[0].color
            } else {
                b
            };
            assert_eq!(px[y * w + x], want, "pixel ({x}, {y})");
        }
    }
    let straight = Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![9, 9, 9, 255]).unwrap());
    let sp = FxItem::Sprite(FxSprite {
        x: 0,
        y: 0,
        dw: 8,
        dh: 4,
        flip_x: false,
        art: straight,
        ink: 2,
        clip: PxRect::EVERYWHERE,
    });
    let mut px = base.clone();
    stamp_own(&mut px, w, all, &sp, (0, 0), &pal);
    assert_eq!(px, base, "a Straight sprite without its tint entry");
}

#[test]
fn the_opaque_store_is_over_premul_at_255() {
    for dst in (0..=255u32).map(|v| v << 24 | v << 16 | (255 - v) << 8 | ((v * 7) & 0xff)) {
        for c in [0u32, 0x00FF_FFFF, 0x0012_3456, 0xAB12_3456] {
            let mut span = [dst];
            fill_flat(&mut span, c, 255);
            assert_eq!(span[0], over_premul(dst, c, 255), "dst {dst:08x} c {c:08x}");
            let mut span = [dst];
            fill_flat(&mut span, c & 0x00FF_FFFF, 0);
            assert_eq!(span[0], add_sat(dst, c & 0x00FF_FFFF));
        }
    }
}

#[test]
fn fx_linear_is_the_free_sprite_law() {
    let mut rng = Rng(0x7777_1111_2222_3333);
    for case in 0..400 {
        let art = art(&mut rng, FxTexels::Rgba8Straight, 1);
        let s = rng.range(1, 4) as u16;
        let tint = ink(&mut rng);
        let tint = FxInk {
            color: rng.next() as u32 & 0x00FF_FFFF,
            alpha: tint.alpha.max(1),
        };
        let (w, h) = (48usize, 40usize);
        let base = ground(w, h, &mut rng);
        let (x, y) = (rng.range(-8, 30) as i32, rng.range(-8, 30) as i32);
        let sp = FxSprite {
            x,
            y,
            dw: art.w() * s,
            dh: art.h() * s,
            flip_x: rng.chance(1, 2),
            art: art.clone(),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        };
        let area = Area::at(i64::from(x), i64::from(y), sp.dw, sp.dh).intersect(Area {
            x0: 0,
            y0: 0,
            x1: w as i64,
            y1: h as i64,
        });
        let mut got = base.clone();
        if !area.is_empty() {
            stamp_sprite_own(
                &mut got,
                w,
                whole(area),
                (i64::from(x), i64::from(y)),
                &sp,
                &[tint],
            );
        }
        let old = crate::legacy_sprite_oracle::LegacySprite {
            x,
            y,
            w: sp.dw,
            h: sp.dh,
            src: [0, 0, art.w(), art.h()],
            art: art.clone(),
            tint: tint.color,
            alpha: tint.alpha,
            flip_x: sp.flip_x,
            under: true,
        };
        let mut want = base;
        crate::legacy_sprite_oracle::stamp(&mut want, w, h, 0, 0, &old, 0, h, None);
        assert!(got == want, "case {case}: scale {s}");
    }
}

#[test]
fn fx_dirty_band_restamps_only_its_band() {
    let f = FxFrame {
        w: 40,
        h: 6 + 5 * 10 + 3,
        pad: 3,
        grid_top: 6,
        cell_h: 10,
        rows: 5,
    };
    let r = 2;
    let pal = vec![FxInk::add(0x00_20_30_40), FxInk::add(0x00_40_10_08)];
    let content = Arc::new(FxContent::new(
        vec![FxItem::Rect(FxRect {
            x: 2,
            y: 13,
            w: 30,
            h: 25,
            ink: 0,
        })],
        pal,
    ));
    let layers = vec![FxLayer::new(
        1,
        content,
        FxSlot::Under,
        FxSpace::Grid,
        FxBlend::Exact,
    )];
    let base: Vec<u32> = (0..f.w * f.h).map(|i| 0x0010_1010 + i as u32 % 9).collect();
    let mut scratch = FxScratch::default();
    let draw = |px: &mut Vec<u32>, dirty: Option<&[bool]>, scratch: &mut FxScratch| {
        draw_layers(
            px,
            &f,
            &layers,
            &[],
            FxSlot::Under,
            FxBlend::Exact,
            dirty,
            scratch,
        );
    };
    let mut full = base.clone();
    draw(&mut full, None, &mut scratch);
    let (y0, y1) = f.band(r);
    let relaid =
        |px: &mut Vec<u32>| px[y0 * f.w..y1 * f.w].copy_from_slice(&base[y0 * f.w..y1 * f.w]);
    let mut dirty = vec![false; f.rows];
    dirty[r] = true;
    let mut damaged = full.clone();
    relaid(&mut damaged);
    draw(&mut damaged, Some(&dirty), &mut scratch);
    assert!(damaged == full, "the damaged frame must equal a full draw");
    let mut ungated = full.clone();
    relaid(&mut ungated);
    draw(&mut ungated, None, &mut scratch);
    let (a0, _) = f.band(r - 1);
    let (_, b1) = f.band(r + 1);
    let doubled = (a0..b1)
        .filter(|&y| !(y0..y1).contains(&y))
        .any(|y| ungated[y * f.w..(y + 1) * f.w] != full[y * f.w..(y + 1) * f.w]);
    assert!(doubled, "CONTROL: an ungated slice build must double-add");
}

#[test]
fn a_column_table_is_built_per_size_and_only_when_drawn() {
    let f = FxFrame {
        w: 160,
        h: 100,
        pad: 0,
        grid_top: 0,
        cell_h: 10,
        rows: 10,
    };
    let art = |w: u16, seed: u8| {
        let data: Vec<u8> = (0..w * 5)
            .map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed) % 4)
            .collect();
        Arc::new(FxArt::new(w, 5, FxTexels::Index8, data).expect("sized"))
    };
    let (a, b, c) = (art(7, 3), art(7, 5), art(9, 7));
    let sprite = |art: &Arc<FxArt>, x: i32, dw: u16, clip: PxRect| {
        FxItem::Sprite(FxSprite {
            x,
            y: 2,
            dw,
            dh: 15,
            flip_x: false,
            ink: 0,
            clip,
            art: Arc::clone(art),
        })
    };
    let items = vec![
        sprite(&a, 2, 21, PxRect::new(2, 2, 12, 17)),
        sprite(&a, 2, 21, PxRect::new(12, 2, 23, 17)),
        sprite(&b, 30, 21, PxRect::EVERYWHERE),
        sprite(&a, 60, 17, PxRect::EVERYWHERE),
        sprite(&c, 90, 21, PxRect::EVERYWHERE),
    ];
    let pal = vec![
        FxInk::over(0x00C0_4020, 255),
        FxInk::over(0x0020_C040, 160),
        FxInk::add(0x0030_3090),
    ];
    let layers = vec![FxLayer::new(
        1,
        Arc::new(FxContent::new(items, pal)),
        FxSlot::Over,
        FxSpace::Window,
        FxBlend::Exact,
    )];
    let base: Vec<u32> = (0..f.w * f.h).map(|i| 0x0008_1018 + i as u32 % 7).collect();
    let mut scratch = FxScratch::default();
    let mut band = |only: Option<usize>| {
        let dirty: Option<Vec<bool>> = only.map(|r| (0..f.rows).map(|b| b == r).collect());
        let mut px = base.clone();
        crate::test_counters::take_sprite_cols_builds();
        draw_layers(
            &mut px,
            &f,
            &layers,
            &[],
            FxSlot::Over,
            FxBlend::Exact,
            dirty.as_deref(),
            &mut scratch,
        );
        let builds = crate::test_counters::take_sprite_cols_builds();
        let want = placed_by_spec(
            &f,
            &layers,
            &[],
            FxSlot::Over,
            FxBlend::Exact,
            dirty.as_deref(),
            &base,
            true,
        );
        if let Some(i) = (0..px.len()).find(|&i| px[i] != want[i]) {
            panic!(
                "band {only:?}: pixel ({}, {}) is {:08x}, the spec says {:08x}",
                i % f.w,
                i / f.w,
                px[i],
                want[i]
            );
        }
        (builds, px != base)
    };
    assert_eq!(band(None), (3, true), "a whole frame: one table a size");
    assert_eq!(band(None), (3, true), "and again on the next frame");
    assert_eq!(band(Some(1)), (3, true), "every sprite reaches band 1");
    assert_eq!(band(Some(6)), (0, false), "no sprite reaches band 6");
}

#[test]
fn a_layer_away_from_every_open_band_is_not_read() {
    struct Count(Vec<u64>);
    impl<'a> FxSliceSink<'a> for Count {
        fn item(&mut self, layer: &'a FxLayer, _: &'a FxItem, _: (i64, i64)) -> bool {
            self.0.push(layer.id);
            true
        }
        fn slice(&mut self, _: FxSlice, _: usize) {}
    }
    let f = FxFrame {
        w: 120,
        h: 6 + 8 * 10,
        pad: 4,
        grid_top: 6,
        cell_h: 10,
        rows: 8,
    };
    let rect = |y: i32| {
        FxItem::Rect(FxRect {
            x: 3,
            y,
            w: 20,
            h: 12,
            ink: 0,
        })
    };
    let layer = |id: u64, space: FxSpace, offset: (i32, i32), ys: &[i32]| {
        let pal = vec![FxInk::add(0x0010_2030), FxInk::add(0x0030_2010)];
        let mut l = FxLayer::new(
            id,
            Arc::new(FxContent::new(ys.iter().map(|&y| rect(y)).collect(), pal)),
            FxSlot::Over,
            space,
            FxBlend::Exact,
        );
        l.offset = offset;
        l
    };
    let layers = vec![
        layer(1, FxSpace::Window, (0, 0), &[10, 30]),
        layer(2, FxSpace::Grid, (0, -20), &[50]),
        layer(3, FxSpace::Window, (5, 0), &[70]),
    ];
    let handed = |open: &[usize]| {
        let mut sink = Count(Vec::new());
        slice_layers(
            &f,
            &layers,
            &[],
            |_| true,
            |b| open.contains(&b),
            &mut FxSlicer::default(),
            &mut sink,
        );
        sink.0
    };
    assert_eq!(handed(&[5]), Vec::<u64>::new(), "no layer reaches band 5");
    assert_eq!(handed(&[0]), vec![1, 1], "only layer 1 reaches band 0");
    assert_eq!(handed(&[4]), vec![2], "layer 2, moved, reaches band 4");
    assert_eq!(handed(&[3]), vec![1, 1, 2], "both reach band 3");
    assert_eq!(handed(&[7]), vec![3], "layer 3 reaches the last band");
    assert_eq!(
        handed(&[0, 1, 2, 3, 4, 5, 6, 7]),
        vec![1, 1, 2, 3],
        "every band open hands over every item"
    );
}

#[test]
fn fx_occluders_compose() {
    let f = FxFrame {
        w: 60,
        h: 50,
        pad: 2,
        grid_top: 4,
        cell_h: 9,
        rows: 5,
    };
    let content = Arc::new(FxContent::new(
        vec![FxItem::Rect(FxRect {
            x: 0,
            y: 0,
            w: 60,
            h: 50,
            ink: 0,
        })],
        vec![FxInk::add(0x00_30_30_30); 2],
    ));
    let layers = vec![FxLayer::new(
        9,
        content,
        FxSlot::Over,
        FxSpace::Window,
        FxBlend::Exact,
    )];
    let occluders = [
        PxRect::new(0, 4, 60, 13),
        PxRect::new(30, 10, 55, 40),
        PxRect::new(5, 30, 35, 45),
    ];
    let base = vec![0x0001_0203u32; f.w * f.h];
    let mut scratch = FxScratch::default();
    let run = |occ: &[PxRect], scratch: &mut FxScratch| {
        let mut px = base.clone();
        draw_layers(
            &mut px,
            &f,
            &layers,
            occ,
            FxSlot::Over,
            FxBlend::Exact,
            None,
            scratch,
        );
        px
    };
    let composed = run(&occluders, &mut scratch);
    let single = run(&occluders[..1], &mut scratch);
    let mut leaked = 0;
    for y in 0..f.h {
        for x in 0..f.w {
            let hit = |o: &PxRect| o.overlaps(PxRect::at(x as i32, y as i32, 1, 1));
            let i = y * f.w + x;
            if occluders.iter().any(hit) {
                assert_eq!(composed[i], base[i], "({x}, {y}) under an occluder");
                if single[i] != base[i] {
                    leaked += 1;
                }
            } else {
                assert_eq!(composed[i], add_sat(base[i], 0x00_30_30_30), "({x}, {y})");
            }
        }
    }
    assert!(
        leaked > 0,
        "CONTROL: a single cut must leave light on the other menus"
    );
}

#[test]
fn fx_extremes_never_panic() {
    let mut rng = Rng(0x1357_9bdf_2468_ace0);
    let f = FxFrame {
        w: 30,
        h: 30,
        pad: 1,
        grid_top: 3,
        cell_h: 7,
        rows: 4,
    };
    let mut scratch = FxScratch::default();
    let extremes = [
        i32::MIN,
        i32::MIN + 1,
        -70_000,
        -1,
        0,
        29,
        70_000,
        i32::MAX - 1,
        i32::MAX,
    ];
    let blank = Arc::new(FxArt::new(0, 0, FxTexels::Index8, Vec::new()).unwrap());
    for _ in 0..3000 {
        let pick = |rng: &mut Rng| extremes[rng.below(extremes.len() as u64) as usize];
        let mut it = item(&mut rng, 4, f.w, f.h);
        match &mut it {
            FxItem::Rect(r) => {
                r.x = pick(&mut rng);
                r.y = pick(&mut rng);
                r.w = u16::MAX;
                r.ink = u32::MAX - rng.below(2) as u32;
            }
            FxItem::Ramps(r) => {
                r.x = pick(&mut rng);
                r.h = u16::MAX;
            }
            FxItem::Sprite(s) => {
                s.x = pick(&mut rng);
                s.y = pick(&mut rng);
                s.ink = u32::MAX;
                if rng.chance(1, 4) {
                    s.art = blank.clone();
                }
                if rng.chance(1, 4) {
                    s.dw = 0;
                }
                s.clip = PxRect::new(
                    pick(&mut rng),
                    pick(&mut rng),
                    pick(&mut rng),
                    pick(&mut rng),
                );
            }
        }
        let mut layer = FxLayer::new(
            0,
            Arc::new(FxContent::new_unchecked(vec![it], palette(&mut rng, 4))),
            FxSlot::Glow,
            FxSpace::Grid,
            FxBlend::Exact,
        );
        layer.offset = (pick(&mut rng), pick(&mut rng));
        layer.clip = PxRect::new(
            pick(&mut rng),
            pick(&mut rng),
            pick(&mut rng),
            pick(&mut rng),
        );
        let occ = [PxRect::new(
            pick(&mut rng),
            pick(&mut rng),
            pick(&mut rng),
            pick(&mut rng),
        )];
        let mut px = vec![0u32; f.w * f.h];
        draw_layers(
            &mut px,
            &f,
            std::slice::from_ref(&layer),
            &occ,
            FxSlot::Glow,
            FxBlend::Exact,
            Some(&[true, false, true, true]),
            &mut scratch,
        );
    }
}

#[test]
fn fx_premul_additive_texel_adds_light() {
    let f = FxFrame {
        w: 12,
        h: 4,
        pad: 0,
        grid_top: 0,
        cell_h: 2,
        rows: 2,
    };
    let texels: [[u8; 4]; 4] = [
        [0x20, 0x10, 0x08, 0],
        [0, 0, 0, 0],
        [0x40, 0x50, 0x60, 255],
        [0, 0, 0, 0x80],
    ];
    let art = Arc::new(FxArt::new(4, 1, FxTexels::Rgba8Premul, texels.concat()).expect("sized"));
    let content = Arc::new(FxContent::new(
        vec![FxItem::Sprite(FxSprite {
            x: 1,
            y: 1,
            dw: 8,
            dh: 2,
            flip_x: false,
            art,
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })],
        Vec::new(),
    ));
    let layer = FxLayer::new(1, content, FxSlot::Under, FxSpace::Window, FxBlend::Exact);
    let base: Vec<u32> = (0..(f.w * f.h) as u32)
        .map(|i| ((i % 3) << 24) | (0x0030_4050 + i))
        .collect();
    let mut px = base.clone();
    draw_layers(
        &mut px,
        &f,
        std::slice::from_ref(&layer),
        &[],
        FxSlot::Under,
        FxBlend::Exact,
        None,
        &mut FxScratch::default(),
    );
    for y in 0..f.h {
        for x in 0..f.w {
            let b = base[y * f.w + x];
            let want = if (1..3).contains(&y) && (1..9).contains(&x) {
                match (x - 1) / 2 {
                    0 => add_sat(b, 0x0020_1008),
                    1 => b,
                    2 => (b & 0xFF00_0000) | 0x0040_5060,
                    _ => over_premul(b, 0, 0x80),
                }
            } else {
                b
            };
            assert_eq!(px[y * f.w + x], want, "pixel ({x}, {y})");
        }
    }
    assert_ne!(
        px[f.w + 1],
        base[f.w + 1],
        "the additive texel must change its pixel"
    );
}

fn holds(r: PxRect, x: i64, y: i64) -> bool {
    i64::from(r.x0) <= x && x < i64::from(r.x1) && i64::from(r.y0) <= y && y < i64::from(r.y1)
}

/// THE PLACEMENT ORACLE: the frame `layers` draw over `base`, computed per
/// pixel from the spec text alone, sharing no placement code with the engine
/// (`layer_place`, `item_place`, `Area`, the slicer).
#[allow(clippy::too_many_arguments)]
fn placed_by_spec(
    f: &FxFrame,
    layers: &[FxLayer],
    occluders: &[PxRect],
    slot: FxSlot,
    blend: FxBlend,
    dirty: Option<&[bool]>,
    base: &[u32],
    sprite_clip_moves: bool,
) -> Vec<u32> {
    let band_of = |y: usize| {
        (0..f.rows)
            .find(|&b| {
                let (y0, y1) = f.band(b);
                (y0..y1).contains(&y)
            })
            .expect("every row is in a band")
    };
    let mut out = base.to_vec();
    for layer in layers {
        if layer.slot != slot || layer.blend != blend || !layer.content.accepts(blend) {
            continue;
        }
        let (gx, gy) = match layer.space {
            FxSpace::Window => (0i64, 0i64),
            FxSpace::Grid => (f.pad as i64, f.grid_top as i64),
        };
        let (ox, oy) = (i64::from(layer.offset.0), i64::from(layer.offset.1));
        let extent = layer.extent();
        for item in layer.content.items() {
            if item.blend() != blend {
                continue;
            }
            let (ix, iy, iw, ih, sprite_clip) = match item {
                FxItem::Rect(r) => (r.x, r.y, r.w, r.h, None),
                FxItem::Ramps(r) => (r.x, r.y, r.w, r.h, None),
                FxItem::Sprite(s) => (s.x, s.y, s.dw, s.dh, Some(s.clip)),
            };
            let (ix, iy) = (i64::from(ix), i64::from(iy));
            let mut cols = Vec::new();
            if let FxItem::Sprite(sp) = item {
                sprite_cols(sp, &mut cols);
            }
            let o = (ix + ox + gx, iy + oy + gy);
            let xs = o.0.max(0)..(o.0 + i64::from(iw)).min(f.w as i64);
            let ys = o.1.max(0)..(o.1 + i64::from(ih)).min(f.h as i64);
            for py in ys {
                for px in xs.clone() {
                    let (lx, ly) = (px - gx, py - gy);
                    if !holds(layer.clip, lx, ly) {
                        continue;
                    }
                    let (cx, cy) = (lx - ox, ly - oy);
                    if let Some(clip) = sprite_clip {
                        let (sx, sy) = if sprite_clip_moves {
                            (cx, cy)
                        } else {
                            (px, py)
                        };
                        if !holds(clip, sx, sy) {
                            continue;
                        }
                    }
                    if occluders.iter().any(|&r| holds(r, px, py)) {
                        continue;
                    }
                    let (x, y) = (px as usize, py as usize);
                    if dirty.is_some_and(|d| !d[band_of(y)]) {
                        continue;
                    }
                    if sprite_clip_moves {
                        assert!(
                            holds(extent, lx, ly),
                            "pixel ({px}, {py}) of layer {} lies outside its extent {extent:?}",
                            layer.id
                        );
                    }
                    let s = FxSlice {
                        x0: x,
                        x1: x + 1,
                        y0: y,
                        y1: y + 1,
                    };
                    stamp(
                        &mut FramePx::new(&mut out, f.w),
                        s,
                        item,
                        o,
                        layer.content.palette(),
                        &cols,
                    );
                }
            }
        }
    }
    out
}

fn random_linear_layer(rng: &mut Rng, f: &FxFrame, id: u64) -> FxLayer {
    let n = rng.range(1, 6) as usize;
    let pal = palette(rng, n);
    let items = (0..rng.range(1, 4))
        .map(|_| {
            let x = rng.range(-40, f.w as i64) as i32;
            let y = rng.range(-40, f.h as i64) as i32;
            FxItem::Sprite(sprite(rng, FxTexels::Rgba8Straight, n as u32, x, y))
        })
        .collect();
    let mut layer = FxLayer::new(
        id,
        Arc::new(FxContent::new_unchecked(items, pal)),
        FxSlot::Over,
        if rng.chance(1, 2) {
            FxSpace::Window
        } else {
            FxSpace::Grid
        },
        FxBlend::Linear,
    );
    layer.offset = (rng.range(-20, 20) as i32, rng.range(-20, 20) as i32);
    if rng.chance(1, 2) {
        layer.clip = PxRect::new(
            rng.range(-10, 30) as i32,
            rng.range(-10, 30) as i32,
            rng.range(20, 120) as i32,
            rng.range(20, 120) as i32,
        );
    }
    layer
}

#[test]
fn fx_placement_is_the_spec() {
    let mut rng = Rng(0x0b5e_55ed_c0ff_ee00);
    let mut scratch = FxScratch::default();
    let (mut control_differs, mut clipped_moved) = (0, 0);
    for case in 0..1500 {
        let f = frame(&mut rng);
        let blend = if rng.chance(1, 4) {
            FxBlend::Linear
        } else {
            FxBlend::Exact
        };
        let layers = match blend {
            FxBlend::Exact => random_layers(&mut rng, &f, FxSlot::Over),
            FxBlend::Linear => (0..rng.range(1, 2))
                .map(|id| random_linear_layer(&mut rng, &f, id as u64))
                .collect(),
        };
        clipped_moved += layers
            .iter()
            .filter(|l| l.offset != (0, 0) || l.space == FxSpace::Grid)
            .flat_map(|l| l.content.items())
            .filter(|it| matches!(it, FxItem::Sprite(s) if s.clip != PxRect::EVERYWHERE))
            .count();
        let occluders: Vec<PxRect> = (0..rng.range(0, 2))
            .map(|_| {
                let x = rng.range(-10, f.w as i64) as i32;
                let y = rng.range(-10, f.h as i64) as i32;
                PxRect::at(x, y, rng.range(1, 50) as u32, rng.range(1, 40) as u32)
            })
            .collect();
        let dirty: Option<Vec<bool>> = rng
            .chance(1, 3)
            .then(|| (0..f.rows).map(|_| rng.chance(1, 2)).collect());
        let base = ground(f.w, f.h, &mut rng);
        let mut engine = base.clone();
        draw_layers(
            &mut engine,
            &f,
            &layers,
            &occluders,
            FxSlot::Over,
            blend,
            dirty.as_deref(),
            &mut scratch,
        );
        let want = placed_by_spec(
            &f,
            &layers,
            &occluders,
            FxSlot::Over,
            blend,
            dirty.as_deref(),
            &base,
            true,
        );
        if let Some(i) = (0..engine.len()).find(|&i| engine[i] != want[i]) {
            panic!(
                "case {case} ({blend:?}): pixel ({}, {}) is {:08x}, the spec says {:08x}",
                i % f.w,
                i / f.w,
                engine[i],
                want[i]
            );
        }
        let unmoved = placed_by_spec(
            &f,
            &layers,
            &occluders,
            FxSlot::Over,
            blend,
            dirty.as_deref(),
            &base,
            false,
        );
        if unmoved != engine {
            control_differs += 1;
        }
    }
    assert!(
        clipped_moved > 500,
        "the corpus moves clipped sprites ({clipped_moved})"
    );
    assert!(
        control_differs > 100,
        "CONTROL: a sprite clip read in window pixels must diverge ({control_differs})"
    );
}

#[test]
fn fx_mixed_content_draws_nothing() {
    let f = FxFrame {
        w: 16,
        h: 8,
        pad: 0,
        grid_top: 0,
        cell_h: 4,
        rows: 2,
    };
    let rect = FxItem::Rect(FxRect {
        x: 1,
        y: 1,
        w: 6,
        h: 5,
        ink: 0,
    });
    let straight = FxItem::Sprite(FxSprite {
        x: 8,
        y: 2,
        dw: 6,
        dh: 4,
        flip_x: false,
        art: Arc::new(FxArt::new(2, 2, FxTexels::Rgba8Straight, vec![0xC0; 16]).expect("sized")),
        ink: 2,
        clip: PxRect::EVERYWHERE,
    });
    let pal = vec![
        FxInk::add(0x0030_2010),
        FxInk::add(0x0010_2030),
        FxInk::over(0x00FF_FFFF, 200),
    ];
    let base: Vec<u32> = (0..(f.w * f.h) as u32).map(|i| 0x0010_1010 + i).collect();
    let draw = |content: Arc<FxContent>, blend: FxBlend| {
        let layer = FxLayer {
            id: 1,
            content,
            slot: FxSlot::Over,
            space: FxSpace::Window,
            blend,
            bloom: false,
            knock: false,
            offset: (0, 0),
            clip: PxRect::EVERYWHERE,
        };
        let mut px = base.clone();
        draw_layers(
            &mut px,
            &f,
            std::slice::from_ref(&layer),
            &[],
            FxSlot::Over,
            blend,
            None,
            &mut FxScratch::default(),
        );
        px
    };
    let mixed = Arc::new(FxContent::new_unchecked(
        vec![rect.clone(), straight.clone()],
        pal.clone(),
    ));
    assert!(!mixed.accepts(FxBlend::Exact) && !mixed.accepts(FxBlend::Linear));
    for blend in [FxBlend::Exact, FxBlend::Linear] {
        assert!(
            draw(mixed.clone(), blend) == base,
            "mixed content drew under {blend:?}"
        );
    }
    let exact = Arc::new(FxContent::new(vec![rect], pal.clone()));
    let linear = Arc::new(FxContent::new(vec![straight], pal));
    assert!(
        draw(exact, FxBlend::Exact) != base,
        "CONTROL: the rect alone draws"
    );
    assert!(
        draw(linear, FxBlend::Linear) != base,
        "CONTROL: the sprite alone draws"
    );
}

#[test]
fn fx_bloom_quads_are_law_f() {
    let mut rng = Rng(0xb100_3c0d_5eed_0001);
    let mut scratch = FxScratch::default();
    let (mut slicer, mut cols, mut quads) =
        (FxSlicer::default(), SpriteCols::default(), Vec::new());
    let (mut columns, mut spans) = (0usize, 0usize);
    for case in 0..1500 {
        let f = frame(&mut rng);
        let mut layers = random_layers(&mut rng, &f, FxSlot::Glow);
        for layer in &mut layers {
            layer.bloom = true;
        }
        let occluders: Vec<PxRect> = (0..rng.range(0, 3))
            .map(|_| {
                let x = rng.range(-10, f.w as i64) as i32;
                let y = rng.range(-10, f.h as i64) as i32;
                PxRect::at(x, y, rng.range(1, 40) as u32, rng.range(1, 30) as u32)
            })
            .collect();
        let base = ground(f.w, f.h, &mut rng);
        let mut engine = base.clone();
        draw_layers(
            &mut engine,
            &f,
            &layers,
            &occluders,
            FxSlot::Glow,
            FxBlend::Exact,
            None,
            &mut scratch,
        );
        quads.clear();
        fx_bloom_quads(&f, &layers, &occluders, &mut slicer, &mut cols, &mut quads);
        let mut drawn = base.clone();
        for q in &quads {
            let (x, y, w, h) = (
                usize::from(q.x),
                usize::from(q.y),
                usize::from(q.w),
                usize::from(q.h),
            );
            let (by0, by1) = f.band(usize::from(q.row));
            assert!(
                x + w <= f.w && y >= by0 && y + h <= by1 && w > 0 && h > 0,
                "case {case}: quad {q:?} leaves the frame or its band [{by0}, {by1})"
            );
            if w == 1 && q.color == q.color2 && q.alpha == q.alpha2 {
                columns += 1;
            } else {
                spans += 1;
            }
            let (c0, c1) = (
                FxInk {
                    color: q.color,
                    alpha: q.alpha,
                },
                FxInk {
                    color: q.color2,
                    alpha: q.alpha2,
                },
            );
            for r in y..y + h {
                fill_glow_span(
                    &mut drawn[r * f.w + x..r * f.w + x + w],
                    0,
                    q.w.into(),
                    c0,
                    c1,
                );
            }
        }
        assert!(
            drawn == engine,
            "case {case}: the bloom quads paint other bytes than law F"
        );
        for layer in &mut layers {
            layer.bloom = false;
        }
        quads.clear();
        fx_bloom_quads(&f, &layers, &occluders, &mut slicer, &mut cols, &mut quads);
        assert!(
            quads.is_empty(),
            "case {case}: a layer without bloom fed it"
        );
    }
    assert!(
        columns > 500 && spans > 1000,
        "the corpus cuts gradients ({columns} columns) and keeps spans ({spans})"
    );
}

#[test]
fn fx_art_budget_refuses_past_the_frame_total() {
    use aterm_core::render::{FX_ART_MAX_SIDE, FX_FRAME_ART_TEXELS};
    let (aw, ah) = (FX_ART_MAX_SIDE, FX_ART_MAX_SIDE / 2);
    let half = u64::from(aw) * u64::from(ah);
    assert_eq!(2 * half, FX_FRAME_ART_TEXELS, "two arts fill the budget");
    let index = |k: u8| {
        let n = usize::from(aw) * usize::from(ah);
        Arc::new(FxArt::new(aw, ah, FxTexels::Index8, vec![k; n]).expect("sized"))
    };
    let (a, b, c) = (index(1), index(2), index(3));
    let rgba = Arc::new(
        FxArt::new(
            4,
            4,
            FxTexels::Rgba8Premul,
            [0x40, 0x50, 0x60, 0xff].repeat(16),
        )
        .expect("sized"),
    );
    let sprite = |x: i32, art: &Arc<FxArt>| {
        FxItem::Sprite(FxSprite {
            x,
            y: 2,
            dw: 6,
            dh: 6,
            flip_x: false,
            art: Arc::clone(art),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
    };
    let palette = vec![
        FxInk::over(0x11_22_33, 255),
        FxInk::over(0x44_55_66, 255),
        FxInk::over(0x77_88_99, 255),
    ];
    let layer = |id, items| {
        FxLayer::new(
            id,
            Arc::new(FxContent::new(items, palette.clone())),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        )
    };
    let f = FxFrame {
        w: 64,
        h: 16,
        pad: 0,
        grid_top: 0,
        cell_h: 8,
        rows: 2,
    };
    let layers = vec![
        layer(1, vec![sprite(0, &a), sprite(8, &b)]),
        layer(2, vec![sprite(16, &c), sprite(24, &a), sprite(32, &rgba)]),
    ];
    let ground = vec![0x0101_0101u32; f.w * f.h];
    let mut scratch = FxScratch::default();
    let mut px = ground.clone();
    draw_layers(
        &mut px,
        &f,
        &layers,
        &[],
        FxSlot::Under,
        FxBlend::Exact,
        None,
        &mut scratch,
    );
    let painted = |px: &[u32], x: usize| px[4 * f.w + x + 2] != ground[4 * f.w + x + 2];
    assert!(painted(&px, 0) && painted(&px, 8), "the budget's arts draw");
    assert!(!painted(&px, 16), "the art past the budget draws nothing");
    assert!(painted(&px, 24), "a repeated art is counted once and draws");
    assert!(painted(&px, 32), "the four-byte arts keep their own total");
    assert_eq!(scratch.slicer.budget().refused_items(), 1);
    let alone = vec![layer(3, vec![sprite(16, &c)])];
    let mut px = ground.clone();
    draw_layers(
        &mut px,
        &f,
        &alone,
        &[],
        FxSlot::Under,
        FxBlend::Exact,
        None,
        &mut scratch,
    );
    assert!(painted(&px, 16), "CONTROL: the refused art draws alone");
    assert_eq!(scratch.slicer.budget().refused_items(), 0);
}

#[test]
fn the_budget_rules_once_per_set_of_contents() {
    use aterm_core::render::{FX_ART_MAX_SIDE, FX_FRAME_ART_TEXELS};
    let (aw, ah) = (FX_ART_MAX_SIDE, FX_ART_MAX_SIDE / 2);
    assert_eq!(
        3 * u64::from(aw) * u64::from(ah),
        FX_FRAME_ART_TEXELS + FX_FRAME_ART_TEXELS / 2,
        "two arts fill the budget, and a third passes it"
    );
    let index = |k: u8| {
        let n = usize::from(aw) * usize::from(ah);
        Arc::new(FxArt::new(aw, ah, FxTexels::Index8, vec![k; n]).expect("sized"))
    };
    let (a, b, c) = (index(1), index(2), index(3));
    let content = |arts: &[&Arc<FxArt>]| {
        let items = arts
            .iter()
            .enumerate()
            .map(|(i, art)| {
                FxItem::Sprite(FxSprite {
                    x: 8 * i as i32,
                    y: 0,
                    dw: 6,
                    dh: 6,
                    flip_x: false,
                    art: Arc::clone(art),
                    ink: 0,
                    clip: PxRect::EVERYWHERE,
                })
            })
            .collect();
        let ink = FxInk::over(0x11_22_33, 255);
        Arc::new(FxContent::new(items, vec![ink; 3]))
    };
    let layer = |content: &Arc<FxContent>, slot: FxSlot| {
        FxLayer::new(
            1,
            Arc::clone(content),
            slot,
            FxSpace::Window,
            FxBlend::Exact,
        )
    };
    let (over, fits) = (content(&[&a, &b, &c]), content(&[&a, &b]));
    let mut budget = FxBudget::default();
    let mut rule = |layers: &[FxLayer]| {
        crate::test_counters::take_budget_rulings();
        let afresh = budget.rule(layers);
        let walked = crate::test_counters::take_budget_rulings();
        assert_eq!(afresh, walked == 1, "a ruling says whether it walked");
        (walked, budget.refused_items())
    };
    let under = |c: &Arc<FxContent>| vec![layer(c, FxSlot::Under)];
    assert_eq!(rule(&under(&over)), (1, 1), "the third art is refused");
    assert_eq!(rule(&under(&over)), (0, 1), "the same content keeps it");
    assert_eq!(rule(&under(&fits)), (1, 0), "ruled afresh: nothing refused");
    assert_eq!(rule(&under(&over)), (1, 1), "and back");
    assert_eq!(
        rule(&[layer(&over, FxSlot::Over)]),
        (1, 1),
        "a slot's change is ruled afresh"
    );
    let mut linear = layer(&over, FxSlot::Over);
    linear.blend = FxBlend::Linear;
    assert_eq!(rule(&[linear]), (1, 0), "a blend's change is ruled afresh");
    let copy = Arc::new(FxContent::new(
        over.items().to_vec(),
        over.palette().to_vec(),
    ));
    assert_eq!(rule(&under(&copy)), (1, 1), "an equal copy is ruled afresh");
    assert_eq!(rule(&[]), (1, 0), "no layers: nothing refused");
    assert_eq!(rule(&[]), (0, 0), "and kept");
}

/// THE BAND LAW'S INVERSE: every pixel row of a frame lies in the band [`band_px`] gives for the
/// band [`band_at`] names, read through the slicer ([`FxFrame`]'s `band_of`) and through fx damage
/// ([`crate::RowBands`]) alike, so damage marks exactly the rows the slicer draws into. Grids
/// shorter than, as tall as and taller than the frame, with and without a top pad, each row checked.
#[test]
fn every_pixel_row_lies_in_the_band_named_for_it() {
    fn first_miss(band_of: impl Fn(&FxFrame, usize) -> usize) -> Option<String> {
        for grid_top in [0, 1, 7, 40] {
            for cell_h in [1, 2, 17, 40] {
                for rows in [1, 2, 3, 26] {
                    let grid = grid_top + rows * cell_h;
                    for h in [1, grid_top + 1, grid / 2 + 1, grid, grid + 13] {
                        let f = FxFrame {
                            w: 1,
                            h,
                            pad: 0,
                            grid_top,
                            cell_h,
                            rows,
                        };
                        for y in 0..h {
                            let b = band_of(&f, y);
                            let (y0, y1) = f.band(b.min(rows - 1));
                            if b >= rows || y < y0 || y >= y1 {
                                return Some(format!(
                                    "top {grid_top}, pitch {cell_h}, {rows} rows, {h} px: \
                                     row {y} named band {b} ({y0}..{y1})"
                                ));
                            }
                        }
                    }
                }
            }
        }
        None
    }
    assert_eq!(first_miss(|f, y| f.band_of(y)), None, "the slicer's");
    assert_eq!(
        first_miss(|f, y| crate::RowBands::new(f.grid_top, f.cell_h).band_of(y as i128, f.rows)),
        None,
        "fx damage's"
    );
    let off_by_one = |f: &FxFrame, y: usize| {
        if y < f.grid_top {
            0
        } else {
            ((y - f.grid_top) / (f.cell_h + 1)).min(f.rows - 1)
        }
    };
    assert!(
        first_miss(off_by_one).is_some(),
        "CONTROL: an off-by-one pitch names a band that misses the row"
    );
}
