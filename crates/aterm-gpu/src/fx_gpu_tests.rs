// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::render::{FxRect, FxSpace, FxSprite, PxRect};

/// looks7: a frame whose text holds the bloom ([`RenderInput::bloom_gate`],
/// a dim theme's) feeds the bloom nothing, its over-text light and the
/// crown over it kept; the same frame unheld feeds it (the negative
/// control).
#[test]
fn a_held_frame_feeds_the_bloom_nothing() {
    let mut input = RenderInput {
        rows: 4,
        ..RenderInput::default()
    };
    input
        .cursor_glow_add
        .push(GlowQuad::flat(1, 30, 16, 8, 8, 0x00E0_2020, 0));
    assert!(
        bloom_source_live(&input),
        "control: unheld, the light feeds the bloom"
    );
    input.bloom_gate = aterm_core::render::BloomGate::Held;
    assert!(!bloom_source_live(&input), "the held frame feeds the bloom");
    assert!(crown_glow_present(&input), "the crown is the light's own");
}

fn art(w: u16, h: u16, seed: u8) -> Arc<FxArt> {
    let data: Vec<u8> = (0..usize::from(w) * usize::from(h))
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) | 1)
        .collect();
    Arc::new(FxArt::new(w, h, FxTexels::Index8, data).expect("sized"))
}

fn sprite_layer(arts: &[Arc<FxArt>]) -> FxLayer {
    let items = arts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            FxItem::Sprite(FxSprite {
                x: i as i32 * 3,
                y: 0,
                dw: a.w(),
                dh: a.h(),
                flip_x: false,
                art: Arc::clone(a),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })
        })
        .collect();
    let palette = (0..255).map(|i| FxInk::over(i, 255)).collect();
    FxLayer::new(
        1,
        Arc::new(FxContent::new(items, palette)),
        FxSlot::Over,
        FxSpace::Window,
        FxBlend::Exact,
    )
}

/// Regions are disjoint, inside the side, and stay put while the
/// generation lasts; a frame whose arts overflow mints a generation
/// holding exactly those arts.
#[test]
fn the_atlas_appends_in_place_and_compacts_on_overflow() {
    let mut fx = FxGpu::default();
    let small: Vec<_> = (0..40).map(|i| art(60, 50, i)).collect();
    fx.admit(&[sprite_layer(&small)], &[], FX_ATLAS_CAP);
    let side = fx.atlas.side(FxAtlasKind::R8);
    assert_eq!(
        (side.side(), side.generation(), side.generation()),
        (512, 1, 1)
    );
    let first: Vec<_> = small.iter().map(|a| side.region(a.id()).unwrap()).collect();
    for (i, a) in first.iter().enumerate() {
        assert!(u32::from(a[0] + a[2]) <= 512 && u32::from(a[1] + a[3]) <= 512);
        for b in &first[..i] {
            let apart = a[0] + a[2] <= b[0]
                || b[0] + b[2] <= a[0]
                || a[1] + a[3] <= b[1]
                || b[1] + b[3] <= a[1];
            assert!(apart, "{a:?} overlaps {b:?}");
        }
    }
    let more: Vec<_> = small.iter().cloned().chain([art(30, 30, 99)]).collect();
    fx.admit(&[sprite_layer(&more)], &[], FX_ATLAS_CAP);
    let side = fx.atlas.side(FxAtlasKind::R8);
    assert_eq!(side.generation(), 1, "a fitting art appends");
    for (a, r) in small.iter().zip(&first) {
        assert_eq!(
            side.region(a.id()),
            Some(*r),
            "a resident region never moves"
        );
    }
    let big: Vec<_> = (0..12).map(|i| art(200, 150, 100 + i)).collect();
    fx.admit(&[sprite_layer(&big)], &[], FX_ATLAS_CAP);
    let side = fx.atlas.side(FxAtlasKind::R8);
    assert_eq!(side.generation(), 2);
    assert_eq!(side.side(), 1024);
    assert_eq!(
        side.len(),
        big.len(),
        "compaction keeps only the frame's arts"
    );
    assert!(small.iter().all(|a| side.region(a.id()).is_none()));
    let layers = [sprite_layer(&big)];
    fx.admit(&layers, &[], FX_ATLAS_CAP);
    let mints = fx.atlas.side(FxAtlasKind::R8).generation();
    fx.admit(&layers, &[], FX_ATLAS_CAP);
    assert_eq!(fx.atlas.side(FxAtlasKind::R8).generation(), mints);
}

/// On a device whose texture limit holds the cap below an art (here 512
/// against a 600-wide art), the art is unfit: refused and counted, its
/// items draw nothing, and the frame does not re-mint for it again.
#[test]
fn an_art_past_the_cap_fails_closed_without_thrashing() {
    let mut fx = FxGpu::default();
    let arts = vec![art(40, 40, 1), art(600, 10, 2)];
    let layers = [sprite_layer(&arts)];
    fx.admit(&layers, &[], 512);
    let side = fx.atlas.side(FxAtlasKind::R8);
    assert!(side.region(arts[0].id()).is_some());
    assert!(side.region(arts[1].id()).is_none());
    let mints = side.generation();
    fx.admit(&[sprite_layer(&arts)], &[], 512);
    assert_eq!(fx.atlas.side(FxAtlasKind::R8).generation(), mints);
    let f = FxFrame {
        w: 800,
        h: 64,
        pad: 0,
        grid_top: 0,
        cell_h: 16,
        rows: 4,
    };
    let mut out: [Vec<FxInstance>; FX_STREAM_COUNT] = Default::default();
    fx.build(&f, &layers, &[], |_| true, &mut out);
    assert_eq!(fx.refused, 1, "the unfit art's item is refused");
    assert!(out[FxStream::Over as usize].iter().all(|i| i.src[2] == 40));
}

/// A Linear Under knock layer (an under-text companion) and a blooming
/// Exact Over layer beside `arts`' layer.
fn knock_and_bloom(arts: &[Arc<FxArt>]) -> [FxLayer; 3] {
    let straight =
        Arc::new(FxArt::new(4, 4, FxTexels::Rgba8Straight, vec![200; 4 * 4 * 4]).expect("sized"));
    let mut knock = FxLayer::new(
        2,
        Arc::new(FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x: 0,
                y: 20,
                dw: 8,
                dh: 8,
                flip_x: false,
                art: straight,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(0x00FF_FFFF, 255)],
        )),
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Linear,
    );
    knock.knock = true;
    let mut bloom = sprite_layer(&arts[..1]);
    bloom.id = 3;
    bloom.bloom = true;
    [sprite_layer(arts), knock, bloom]
}

/// Every reader of a frame's layers (the knock-out, the atlas admission, the slice, the
/// bloom source) reads one frame art budget, ruled by whichever reads first when the
/// layers change and not at all while they stand.
#[test]
fn a_frame_rules_its_art_budget_once() {
    struct Rig {
        fx: FxGpu,
        out: [Vec<FxInstance>; FX_STREAM_COUNT],
    }
    let mut rig = Rig {
        fx: FxGpu::default(),
        out: Default::default(),
    };
    let f = FxFrame {
        w: 800,
        h: 64,
        pad: 0,
        grid_top: 0,
        cell_h: 16,
        rows: 4,
    };
    // In the renderer's order: the admission, the build, then the bloom source.
    let frame = |rig: &mut Rig, layers: &[FxLayer]| {
        let _ = aterm_render::test_counters::take_budget_rulings();
        rig.fx.admit(layers, &[], FX_ATLAS_CAP);
        rig.fx.build(&f, layers, &[], |_| true, &mut rig.out);
        let input = RenderInput {
            fx_layers: layers.to_vec(),
            ..RenderInput::default()
        };
        let _ = rig.fx.bloom_source(&f, &input);
        aterm_render::test_counters::take_budget_rulings()
    };
    let arts = vec![art(40, 40, 1), art(20, 10, 2)];
    let layers = [sprite_layer(&arts)];
    assert_eq!(frame(&mut rig, &layers), 1, "a new frame is ruled once");
    assert_eq!(frame(&mut rig, &layers), 0, "a standing frame is not ruled");
    assert_eq!(
        frame(&mut rig, &[sprite_layer(&arts)]),
        1,
        "a new content is ruled once"
    );
    assert_eq!(
        rig.out[FxStream::Over as usize].len(),
        3 + 1,
        "CONTROL: both sprites drawn, the 40-tall one in its three bands"
    );
    let all = knock_and_bloom(&arts);
    assert_eq!(
        frame(&mut rig, &all),
        1,
        "a frame with a knock and a bloom layer is ruled once"
    );
    assert!(
        !rig.out[FxStream::KnockUnder as usize].is_empty(),
        "the knock layer drew from the ruling"
    );
    assert_eq!(frame(&mut rig, &all), 0, "and not at all while it stands");
    let fresh = knock_and_bloom(&[art(12, 12, 9)]);
    assert_eq!(frame(&mut rig, &fresh), 1);
    assert!(
        rig.fx
            .atlas
            .region(
                &fresh[0]
                    .content
                    .items()
                    .first()
                    .and_then(|i| match i {
                        FxItem::Sprite(sp) => Some(Arc::clone(&sp.art)),
                        _ => None,
                    })
                    .expect("a sprite")
            )
            .is_some(),
        "a new frame's art is admitted though the knock-out ruled the frame first"
    );
}

/// At the cap, every frame whose drawn arts keep the budget's law (each side at most half
/// the cap, at most a quarter of the cap's area in all) is wholly resident after admission,
/// in disjoint regions, whatever the generation held before and the prefetch asks beside it.
#[test]
fn every_frame_within_the_art_budget_is_resident() {
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u32| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % u64::from(n.max(1))) as u32
    };
    let mut serial = 0u32;
    let mut fresh = |w: u32, h: u32| {
        serial += 1;
        let mut data = vec![1u8; (w * h) as usize];
        for (b, s) in data.iter_mut().zip(serial.to_le_bytes()) {
            *b = s;
        }
        Arc::new(FxArt::new(w as u16, h as u16, FxTexels::Index8, data).expect("sized"))
    };
    let disjoint_and_inside = |side: &FxAtlasSide, arts: &[Arc<FxArt>], frame: usize| {
        for a in arts {
            assert!(
                side.region(a.id()).is_some(),
                "frame {frame}: a drawn art is not resident"
            );
        }
        let regions: Vec<[u16; 4]> = side.resident.values().map(|r| r.region).collect();
        for (i, a) in regions.iter().enumerate() {
            let (ax1, ay1) = (
                u32::from(a[0]) + u32::from(a[2]),
                u32::from(a[1]) + u32::from(a[3]),
            );
            assert!(
                ax1 <= side.side() && ay1 <= side.side(),
                "frame {frame}: {a:?}"
            );
            for b in &regions[..i] {
                let apart = ax1 <= u32::from(b[0])
                    || u32::from(b[0]) + u32::from(b[2]) <= u32::from(a[0])
                    || ay1 <= u32::from(b[1])
                    || u32::from(b[1]) + u32::from(b[3]) <= u32::from(a[1]);
                assert!(apart, "frame {frame}: {a:?} overlaps {b:?}");
            }
        }
    };
    let cap = 256u32;
    let (half, budget) = (cap / 2, u64::from(cap) * u64::from(cap) / 4);
    let mut side = FxAtlasSide::new(FxAtlasKind::R8);
    let mut pool: Vec<Arc<FxArt>> = Vec::new();
    let (mut mints, mut appends) = (0, 0);
    for frame in 0..400 {
        let shape = |next: &mut dyn FnMut(u32) -> u32| match next(4) {
            0 => (1 + next(4), half / 2 + next(half / 2 + 1)),
            1 => (half / 2 + next(half / 2 + 1), 1 + next(4)),
            2 => (1 + next(half), 1 + next(half)),
            _ => (1 + next(16), 1 + next(16)),
        };
        let mut pick = |total: u64, next: &mut dyn FnMut(u32) -> u32, pool: &mut Vec<_>| {
            let (mut arts, mut used, mut seen) = (Vec::new(), 0u64, FxHashSet::default());
            for _ in 0..1 + next(24) {
                let art: Arc<FxArt> = if !pool.is_empty() && next(2) == 0 {
                    Arc::clone(&pool[next(pool.len() as u32) as usize])
                } else {
                    let (w, h) = shape(next);
                    let art = fresh(w, h);
                    pool.push(Arc::clone(&art));
                    art
                };
                let a = u64::from(art.w()) * u64::from(art.h());
                if used + a <= total && seen.insert(art.id()) {
                    used += a;
                    arts.push(art);
                }
            }
            (arts, seen)
        };
        let (draw, drawn) = pick(budget >> next(3), &mut next, &mut pool);
        let hinted = if next(4) == 0 { 4 * budget } else { 0 };
        let (hint, _) = pick(hinted, &mut next, &mut pool);
        let hint: Vec<_> = hint
            .into_iter()
            .filter(|a| !drawn.contains(&a.id()))
            .collect();
        let (generation, written) = (side.generation(), side.seq());
        side.admit(&draw, &hint, cap);
        if side.generation() != generation {
            mints += 1;
        } else if side.seq() > written {
            appends += 1;
        }
        disjoint_and_inside(&side, &draw, frame);
    }
    assert!(
        mints > 40 && appends > 40,
        "the history compacts ({mints} mints) and appends ({appends})"
    );
    let max = u32::from(aterm_core::render::FX_ART_MAX_SIDE);
    let extremes = [
        vec![fresh(max, max)],
        vec![fresh(max, max / 2), fresh(max, max / 2)],
        (0..max).map(|_| fresh(1, max)).collect::<Vec<_>>(),
    ];
    let hint: Vec<_> = (0..3).map(|_| fresh(max, max)).collect();
    for (k, draw) in extremes.iter().enumerate() {
        let texels: u64 = draw
            .iter()
            .map(|a| u64::from(a.w()) * u64::from(a.h()))
            .sum();
        assert_eq!(texels, aterm_core::render::FX_FRAME_ART_TEXELS);
        let mut side = FxAtlasSide::new(FxAtlasKind::R8);
        side.admit(&hint[..1], &[], FX_ATLAS_CAP);
        side.admit(draw, &hint, FX_ATLAS_CAP);
        disjoint_and_inside(&side, draw, 1000 + k);
    }
}

fn tile(w: u16, h: u16, seed: u32) -> Arc<FxArt> {
    let mut data = vec![0x80u8; usize::from(w) * usize::from(h) * 4];
    for (b, s) in data.iter_mut().zip(seed.to_le_bytes()) {
        *b = s;
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, data).expect("sized"))
}

/// An eviction gives back the one run of neighbours whose newest use is oldest, where plain
/// LRU would give back more arts, and an art taller than every shelf takes the bottom shelf.
#[test]
fn an_eviction_gives_back_one_run_not_the_cache() {
    // A 512 square of 100 × 100 tiles: five shelves of five, the first
    // shelf's tiles used in the order t0, t2, t4, t1, t3, every other
    // tile since.
    let mut side = FxAtlasSide::new(FxAtlasKind::Rgba);
    let tiles: Vec<Arc<FxArt>> = (0..25).map(|i| tile(100, 100, 7000 + i)).collect();
    side.admit(&tiles, &[], 512);
    assert_eq!((side.generation(), side.len()), (1, 25));
    let mut first: Vec<&Arc<FxArt>> = tiles
        .iter()
        .filter(|t| side.region(t.id()).is_some_and(|r| r[1] == 0))
        .collect();
    first.sort_unstable_by_key(|t| side.region(t.id()).map(|r| r[0]));
    assert_eq!(first.len(), 5, "one shelf of five");
    for k in [2, 4, 1, 3] {
        side.admit(&[Arc::clone(first[k])], &[], 512);
    }
    let rest: Vec<Arc<FxArt>> = tiles
        .iter()
        .filter(|t| !first.iter().any(|f| f.id() == t.id()))
        .cloned()
        .collect();
    side.admit(&rest, &[], 512);
    // A 120-wide art fits no single tile's columns. Plain LRU would give
    // back t0, t2 and t4 (spans of 100, 100 and 112, none 120 wide) and
    // then t1: four tiles where two neighbours do.
    let wide = tile(120, 100, 9000);
    side.admit(std::slice::from_ref(&wide), &[], 512);
    assert_eq!(side.generation(), 1, "no mint");
    assert_eq!(side.evictions(), 2, "one run of two neighbours goes");
    let kept: Vec<bool> = first
        .iter()
        .map(|t| side.region(t.id()).is_some())
        .collect();
    assert_eq!(
        kept,
        [false, false, true, true, true],
        "the run whose newest use is oldest: t0 and t1"
    );
    assert_eq!(side.region(wide.id()).map(|r| [r[0], r[1]]), Some([0, 0]));
    // An art taller than every shelf needs rows under the last kept
    // shelf: the bottom shelf's five go whole, and nothing above it.
    let tall = tile(200, 112, 9001);
    side.admit(std::slice::from_ref(&tall), &[], 512);
    assert_eq!(side.generation(), 1, "no mint");
    assert_eq!(side.evictions(), 2 + 5, "the bottom shelf's five");
    assert_eq!(side.region(tall.id()).map(|r| [r[0], r[1]]), Some([0, 400]));
    assert!(
        first[2..].iter().all(|t| side.region(t.id()).is_some())
            && side.region(wide.id()).is_some(),
        "the first shelf keeps t2, t3, t4 and the wide art"
    );
    assert_eq!(side.len(), 25 - 2 + 1 - 5 + 1);
}

/// Baked tiles live by LRU: a companion that bakes two new tiles a frame and holds one
/// runs through a full atlas without a mint, never evicting the held tile or a frame's own
/// tiles, keeping the newest resident, and an arm's sync uploads only the tiles written
/// since it last synced.
#[test]
fn baked_tiles_cycle_through_a_full_atlas_by_lru() {
    let mut side = FxAtlasSide::new(FxAtlasKind::Rgba);
    let held = tile(100, 100, 0);
    let mut baked: Vec<Arc<FxArt>> = Vec::new();
    let capacity = (512 / 100) * (512 / 100);
    for frame in 0..200u32 {
        let fresh = [tile(100, 100, 2 * frame + 1), tile(100, 100, 2 * frame + 2)];
        baked.extend(fresh.iter().cloned());
        let synced = side.seq();
        let draw: Vec<_> = [Arc::clone(&held)].into_iter().chain(fresh).collect();
        side.admit(&draw, &[], 512);
        assert_eq!(
            (side.generation(), side.side()),
            (1, 512),
            "frame {frame}: no mint"
        );
        let mut written: Vec<u128> = side
            .written_after(synced)
            .iter()
            .map(|(a, _)| a.id())
            .collect();
        let mut want: Vec<u128> = draw
            .iter()
            .skip(usize::from(frame > 0))
            .map(|a| a.id())
            .collect();
        if frame == 0 {
            // The first admission mints, placing tallest first.
            written.sort_unstable();
            want.sort_unstable();
        }
        assert_eq!(written, want, "frame {frame}: a sync uploads the new tiles");
        let newest = baked.len().min(capacity - 1);
        let mut resident: Vec<u128> = side.resident.keys().copied().collect();
        let mut expect: Vec<u128> = baked[baked.len() - newest..]
            .iter()
            .chain([&held])
            .map(|a| a.id())
            .collect();
        resident.sort_unstable();
        expect.sort_unstable();
        assert_eq!(
            resident, expect,
            "frame {frame}: the resident set is the held tile and the newest"
        );
        let regions: Vec<[u16; 4]> = side.resident.values().map(|r| r.region).collect();
        for (i, a) in regions.iter().enumerate() {
            for b in &regions[..i] {
                let apart = a[0] + a[2] <= b[0]
                    || b[0] + b[2] <= a[0]
                    || a[1] + a[3] <= b[1]
                    || b[1] + b[3] <= a[1];
                assert!(apart, "frame {frame}: {a:?} overlaps {b:?}");
            }
        }
    }
    assert_eq!(side.evictions(), 400 + 1 - capacity as u64);
    let crowd: Vec<_> = (0..=capacity as u32)
        .map(|i| tile(100, 100, 1000 + i))
        .collect();
    side.admit(&crowd, &[], 512);
    for art in &crowd {
        assert!(
            side.region(art.id()).is_some() || side.unfit_drawn.contains(&art.id()),
            "a frame's own tile is never evicted for another of its tiles"
        );
    }
    assert_eq!(
        (side.generation(), side.len(), side.unfit_drawn.len()),
        (2, capacity, 1),
        "a frame past the cap mints, and its one unfit tile fails closed"
    );
}

/// A released region's columns return to its shelf, merged with their
/// free neighbours, and a trailing shelf left wholly free returns to the
/// square, so a taller art can take its rows.
#[test]
fn released_columns_merge_and_free_trailing_shelves_return() {
    let mut pack = ShelfPack::new(100);
    let a = pack.place(30, 10).expect("a");
    let b = pack.place(30, 10).expect("b");
    let c = pack.place(30, 10).expect("c");
    assert_eq!((a, b, c), ((0, 0), (30, 0), (60, 0)));
    pack.release(30, 0, 30);
    assert_eq!(
        pack.shelves[0].free,
        vec![(30, 60), (90, 100)],
        "a hole and the tail"
    );
    pack.release(0, 0, 30);
    assert_eq!(pack.shelves[0].free, vec![(0, 60), (90, 100)]);
    assert_eq!(pack.place(60, 5), Some((0, 0)), "the merged hole");
    pack.release(0, 0, 60);
    pack.release(60, 0, 30);
    assert!(pack.shelves.is_empty() && pack.bottom == 0);
    assert_eq!(pack.place(100, 100), Some((0, 0)), "the whole square");
}

/// Instances carry the origin mod 2¹⁶ and the absolute palette base; a
/// content's palette is appended once however many items draw from it.
#[test]
fn instances_name_their_item_and_palettes_append_once() {
    let mut fx = FxGpu::default();
    let palette = vec![
        FxInk::add(0x10_20_30),
        FxInk::add(0x11_22_33),
        FxInk::over(0x40_50_60, 7),
        FxInk::over(1, 9),
    ];
    let content = Arc::new(FxContent::new(
        vec![
            FxItem::Rect(FxRect {
                x: -65_000,
                y: 6,
                w: 65_535,
                h: 5,
                ink: 2,
            }),
            FxItem::Rect(FxRect {
                x: 2,
                y: 6,
                w: 4,
                h: 5,
                ink: 0,
            }),
        ],
        palette,
    ));
    let layer = |id| {
        FxLayer::new(
            id,
            Arc::clone(&content),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        )
    };
    let layers = [layer(1), layer(2)];
    let f = FxFrame {
        w: 64,
        h: 32,
        pad: 0,
        grid_top: 0,
        cell_h: 8,
        rows: 4,
    };
    let mut out: [Vec<FxInstance>; FX_STREAM_COUNT] = Default::default();
    fx.admit(&layers, &[], FX_ATLAS_CAP);
    fx.build(&f, &layers, &[], |_| true, &mut out);
    assert_eq!(fx.palette.len(), 4, "one content, one palette");
    assert_eq!(fx.palette[2], 0x07_60_50_40, "RGBA bytes, little-endian");
    let under = &out[FxStream::Under as usize];
    assert_eq!(
        under.len(),
        2 * 2 * 2,
        "two layers of two rects, each cut at y = 8"
    );
    let wide: Vec<_> = under.iter().filter(|i| i.src[0] == 65_535).collect();
    assert_eq!(wide.len(), 4);
    assert!(wide.iter().all(|i| i.origin == [536, 6] && i.pal == 2));
    assert!(wide.iter().all(|i| i.rect[0] == 0 && i.rect[2] == 64));
    let narrow: Vec<_> = under.iter().filter(|i| i.src[0] == 4).collect();
    assert!(narrow.iter().all(|i| i.origin == [2, 6] && i.pal == 0));
    assert!(under.iter().all(|i| i.flags == FX_KIND_RECT));
    fx.build(&f, &layers, &[], |b| b == 1, &mut out);
    let under = &out[FxStream::Under as usize];
    assert_eq!(under.len(), 4);
    assert!(under.iter().all(|i| i.rect[1] == 8 && i.rect[3] == 3));

    let art =
        Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![10, 20, 30, 255]).expect("sized"));
    let linear = Arc::new(FxContent::new(
        vec![FxItem::Sprite(FxSprite {
            x: 0,
            y: 0,
            dw: 1,
            dh: 1,
            flip_x: false,
            art: Arc::clone(&art),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })],
        vec![FxInk::over(0x00ff_ffff, 255)],
    ));
    for (why, slot, blend) in [
        ("slot", FxSlot::Glow, FxBlend::Linear),
        ("blend", FxSlot::Under, FxBlend::Exact),
    ] {
        let mut layer = FxLayer::new(
            3,
            Arc::clone(&linear),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Linear,
        );
        layer.slot = slot;
        layer.blend = blend;
        let mut recovery = FxGpu::default();
        recovery.admit(std::slice::from_ref(&layer), &[], FX_ATLAS_CAP);
        recovery.build(&f, std::slice::from_ref(&layer), &[], |_| true, &mut out);
        assert!(
            out.iter().all(Vec::is_empty),
            "{why}: invalid draws nothing"
        );
        assert!(recovery.atlas.region(&art).is_none(), "{why}: not admitted");

        layer.slot = FxSlot::Under;
        layer.blend = FxBlend::Linear;
        assert!(Arc::ptr_eq(&linear, &layer.content), "the same content Arc");
        recovery.admit(std::slice::from_ref(&layer), &[], FX_ATLAS_CAP);
        recovery.build(&f, std::slice::from_ref(&layer), &[], |_| true, &mut out);
        assert!(
            recovery.atlas.region(&art).is_some(),
            "{why}: valid art admitted"
        );
        assert_eq!(
            out[FxStream::LinearUnder as usize].len(),
            1,
            "{why}: restored"
        );
        assert!(
            out.iter().enumerate().all(|(i, instances)| {
                i == FxStream::LinearUnder as usize || instances.is_empty()
            }),
            "{why}: only the valid stream draws"
        );
        assert_eq!(recovery.refused, 0, "{why}: no missing art");

        let generation = recovery.atlas.side(FxAtlasKind::Rgba).generation();
        recovery.admit(std::slice::from_ref(&layer), &[], FX_ATLAS_CAP);
        recovery.build(&f, std::slice::from_ref(&layer), &[], |_| true, &mut out);
        assert_eq!(
            recovery.atlas.side(FxAtlasKind::Rgba).generation(),
            generation,
            "{why}: unchanged valid admission keeps residency"
        );
        assert_eq!(
            out[FxStream::LinearUnder as usize].len(),
            1,
            "{why}: unchanged valid frame still draws"
        );
    }
}

/// A frame's ramps open its fx stream on the Metal arm only where the
/// glow instances their lit rows spare, both streams' together, pay for
/// opening it; in an fx stream the frame draws its art in anyway they
/// ride only where a lit row costs less there than its glow instance,
/// which on Metal none does; a frame with no lit ramp row opens nothing;
/// and an arm no cost was measured on never opens it.
#[test]
fn ramps_open_the_fx_stream_only_where_their_lit_rows_pay() {
    let lit = |lit: usize| GlowRamps {
        lit,
        ..GlowRamps::default()
    };
    let cost = ramp_cost(RampArm::Metal).expect("Metal's costs were measured");
    assert_eq!(cost, METAL_RAMP_COST);
    let even = usize::try_from(cost.open / -cost.light).expect("small");
    let (half, rest) = (even / 2, even - even / 2);
    for (under, add) in [(0, 0), (even, 0), (0, even), (half, rest)] {
        let streams = [&lit(under), &lit(add)];
        assert!(
            !ramps_open_fx(false, streams, Some(cost)),
            "{under} + {add}"
        );
    }
    for (under, add) in [(even + 1, 0), (0, even + 1), (half + 1, rest)] {
        let streams = [&lit(under), &lit(add)];
        assert!(ramps_open_fx(false, streams, Some(cost)), "{under} + {add}");
    }
    // Beside art, a lit row costs Metal more in fx than as its glow
    // instance, however many there are; an arm where it costs less
    // rides from one lit row.
    assert!(cost.art > 0, "{cost:?}");
    let cheaper = FxRampCost { art: -1, ..cost };
    for n in [1, even, 10 * even] {
        let streams = [&lit(n / 2), &lit(n - n / 2)];
        assert!(!ramps_open_fx(true, streams, Some(cost)), "{n} beside art");
        assert!(
            ramps_open_fx(true, streams, Some(cheaper)),
            "{n} beside art"
        );
    }
    assert!(!ramps_open_fx(true, [&lit(0), &lit(0)], Some(cheaper)));
    for arm in [RampArm::Gl, RampArm::D3d11, RampArm::Unarmed] {
        assert_eq!(ramp_cost(arm), None, "{arm:?}: no cost was measured");
        for n in [1, even + 1, 10 * even] {
            for art in [false, true] {
                let streams = [&lit(n), &lit(n)];
                assert!(!ramps_open_fx(art, streams, ramp_cost(arm)), "{arm:?}");
            }
        }
    }
}
