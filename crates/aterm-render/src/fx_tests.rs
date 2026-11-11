// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::fx_raster::fill_flat;
use aterm_core::render::{FxContent, FxInk, FxItem, FxSpace, FxSprite, GlowQuad};

fn frame(w: usize, h: usize) -> FxFrame {
    FxFrame {
        w,
        h,
        pad: 0,
        grid_top: 0,
        cell_h: h.max(1),
        rows: 1,
    }
}

/// A deterministic stream of `u32`s.
fn rng(seed: u64) -> impl FnMut() -> u32 {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 16) as u32
    }
}

/// Lay `laid` piece by piece onto a `w × h` buffer at `(x, y)`, each in
/// its piece's ink: the order a quad stream lays them in.
fn lay_in_order(
    px: &mut [u32],
    w: usize,
    laid: &[(PxRect, u8)],
    inks: &[FxInk],
    (x, y): (i32, i32),
) {
    for &(r, id) in laid {
        let ink = inks[usize::from(id)];
        for py in r.y0..r.y1 {
            let row = (py + y) as usize * w;
            let (a, b) = ((r.x0 + x) as usize, (r.x1 + x) as usize);
            fill_flat(&mut px[row + a..row + b], ink.color, ink.alpha);
        }
    }
}

/// The layer of one sprite per art in `arts` at `(x, y)` over `palette`,
/// each at palette base `base`.
fn sprites(arts: &[(Arc<FxArt>, u32)], (x, y): (i32, i32), palette: Vec<FxInk>) -> FxLayer {
    let items = arts
        .iter()
        .map(|(art, base)| {
            FxItem::Sprite(FxSprite {
                x,
                y,
                dw: art.w(),
                dh: art.h(),
                flip_x: false,
                art: Arc::clone(art),
                ink: *base,
                clip: PxRect::EVERYWHERE,
            })
        })
        .collect();
    FxLayer::new(
        7,
        Arc::new(FxContent::new(items, palette)),
        FxSlot::Over,
        FxSpace::Window,
        FxBlend::Exact,
    )
}

/// AN ART IS INTERNED ONCE: the same picture built twice, while either is
/// held, is one `Arc`; dropped by every producer, it is built afresh.
#[test]
fn an_art_is_interned_once() {
    let a = indexed(&["ab.", ".ba"], b"ab").expect("an art");
    let b = indexed(&["ab.", ".ba"], b"ab").expect("an art");
    assert!(Arc::ptr_eq(&a, &b), "one picture, one Arc");
    let other = indexed(&["ab.", ".bb"], b"ab").expect("an art");
    assert!(!Arc::ptr_eq(&a, &other), "CONTROL: another picture");
    let id = a.id();
    let weak = Arc::downgrade(&a);
    drop((a, b));
    assert!(weak.upgrade().is_none(), "the registry holds arts weakly");
    let again = indexed(&["ab.", ".ba"], b"ab").expect("an art");
    assert_eq!(again.id(), id, "the same picture, the same id");
    let glow: [u8; 8] = [0x40, 0x20, 0x10, 0x00, 0x80, 0x40, 0x20, 0x80];
    let _ = crate::test_counters::take_art_builds();
    let a = bitmap(FxTexels::Rgba8Premul, 2, 1, glow).expect("an art");
    let b = bitmap(FxTexels::Rgba8Premul, 2, 1, glow).expect("an art");
    assert!(Arc::ptr_eq(&a, &b), "one premultiplied picture, one Arc");
    assert_eq!(
        crate::test_counters::take_art_builds(),
        2,
        "each build is counted"
    );
    let straight = bitmap(FxTexels::Rgba8Straight, 2, 1, glow).expect("an art");
    assert!(!Arc::ptr_eq(&a, &straight), "CONTROL: another kind");
    let index = bitmap(FxTexels::Index8, 3, 2, [1u8, 0, 2, 2, 3, 1]).expect("an art");
    let named = indexed(&["o.c", "cko"], b"ock").expect("an art");
    assert!(
        Arc::ptr_eq(&index, &named),
        "a bitmap is the picture its bytes name"
    );
    assert!(
        bitmap(FxTexels::Rgba8Premul, 2, 1, [0u8; 4]).is_none(),
        "short bytes"
    );
    assert!(
        bitmap(FxTexels::Index8, 0, 0, [0u8; 0]).is_none(),
        "no texels"
    );
}

/// A registry of its own, so a test reads the table no other test's arts reach.
fn private_registry() -> FxRegistry {
    FxRegistry {
        arts: Mutex::new(Interned::default()),
    }
}

/// An 8 × 1 Index8 picture named by `name`: one picture a name.
fn named_art(name: u64) -> FxArt {
    FxArt::new(8, 1, FxTexels::Index8, name.to_le_bytes()).expect("an art")
}

/// ONE PICTURE, ONE ARC, WHATEVER THE THREADS: eight threads let loose
/// together on a picture no earlier round made all get the one `Arc`,
/// round after round, while each also interns a picture of its own and
/// drops it, so the table sweeps under them.
#[test]
fn concurrent_interning_gives_one_arc_a_picture() {
    const THREADS: u64 = 8;
    const ROUNDS: u64 = 400;
    let reg = private_registry();
    let start = std::sync::Barrier::new(THREADS as usize);
    let rounds: Vec<Vec<Arc<FxArt>>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..THREADS)
            .map(|t| {
                let (reg, start) = (&reg, &start);
                s.spawn(move || {
                    (0..ROUNDS)
                        .map(|round| {
                            start.wait();
                            let shared = reg.intern(named_art(round));
                            drop(reg.intern(named_art((t + 1) << 32 | round)));
                            shared
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().expect("an interning thread"))
            .collect()
    });
    let split = (0..ROUNDS as usize)
        .filter(|&r| !rounds.iter().all(|got| Arc::ptr_eq(&got[r], &rounds[0][r])))
        .count();
    assert_eq!(
        split, 0,
        "{split} of {ROUNDS} rounds gave one picture two Arcs"
    );
    let t = reg.arts.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(
        t.by_id.len() < (ROUNDS * (THREADS + 1)) as usize,
        "CONTROL: the dropped pictures were swept ({} entries)",
        t.by_id.len()
    );
}

/// THE REGISTRY NEVER GROWS WITH PICTURES SEEN ONCE (§4.7): a thousand
/// pictures interned and dropped, at most three held at once, leave a
/// table of at most `SWEEP_FLOOR` entries; a held picture is never swept
/// (interning it again is its `Arc`).
#[test]
fn the_registry_sweeps_what_nobody_holds() {
    let reg = private_registry();
    let kept = reg.intern(named_art(u64::MAX));
    let mut held = std::collections::VecDeque::new();
    let mut most = 0;
    for name in 0..1000u64 {
        held.push_back(reg.intern(named_art(name)));
        if held.len() > 2 {
            held.pop_front();
        }
        most = most.max(
            reg.arts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .by_id
                .len(),
        );
    }
    assert!(
        most <= SWEEP_FLOOR,
        "a table of {most} entries for at most 3 pictures held"
    );
    assert!(
        Arc::ptr_eq(&reg.intern(named_art(u64::MAX)), &kept),
        "a held picture is never swept"
    );
    let back = reg.intern(named_art(999));
    assert!(Arc::ptr_eq(&back, &held[1]), "nor is the newest");
}

/// `indexed` reads `'.'` as transparent and `chars[k]` as texel `k + 1`,
/// and refuses a ragged row or a byte it does not know.
#[test]
fn indexed_names_each_character_by_its_place() {
    let art = indexed(&["o.c", "cko"], b"ock").expect("an art");
    assert_eq!((art.w(), art.h()), (3, 2));
    assert_eq!(art.data(), &[1, 0, 2, 2, 3, 1]);
    assert!(indexed(&["o.c", "ck"], b"ock").is_none(), "a ragged row");
    assert!(indexed(&["o.x"], b"ock").is_none(), "an unknown byte");
    assert!(indexed(&[], b"ock").is_none(), "no rows");
    assert!(indexed(&["", ""], b"ock").is_none(), "empty rows");
    let max = usize::from(aterm_core::render::FX_ART_MAX_SIDE);
    let edge = ".".repeat(max);
    assert!(indexed(&[&edge], b"o").is_some(), "the longest legal row");
    let too_wide = ".".repeat(max + 1);
    assert!(indexed(&[&too_wide], b"o").is_none());
    assert!(indexed(&vec!["o"; max + 1], b"o").is_none());
}

/// Random pieces (overlapping needles and dots) over a random ground.
fn random_pieces(next: &mut impl FnMut() -> u32, ids: u8) -> Vec<(PxRect, u8)> {
    let n = 2 + next() % 14;
    (0..n)
        .map(|_| {
            let (x, y) = ((next() % 24) as i32 - 12, (next() % 24) as i32 - 12);
            let (w, h) = (1 + next() % 12, 1 + next() % 12);
            (PxRect::at(x, y, w, h), (next() % u32::from(ids)) as u8)
        })
        .collect()
}

/// THE FOLD AND THE PEEL ARE EXACT: random pieces laid in order onto a
/// random ground equal, byte for byte, the folded art under each class's
/// `add_sat` sum where every ink is additive, and the peeled arts drawn
/// in order under the pieces' inks for translucent and opaque inks.
/// Controls: a fold of over inks, and the peel drawn in reverse, differ.
#[test]
fn the_fold_and_the_peel_are_exact() {
    let (w, h) = (64usize, 64usize);
    let at = (24, 24);
    let (mut fold_breaks, mut reverse_breaks) = (0, 0);
    for seed in 0..200u64 {
        let mut next = rng(seed);
        let ids = 6u8;
        let laid = random_pieces(&mut next, ids);
        let ground: Vec<u32> = (0..w * h).map(|_| next() & 0x00FF_FFFF).collect();
        let art = pieces(&laid).expect("pieces");
        let origin = art.origin();
        let place = (at.0 + origin.0, at.1 + origin.1);
        for additive in [true, false] {
            let inks: Vec<FxInk> = (0..ids)
                .map(|_| {
                    let a = if additive {
                        0
                    } else {
                        [255u8, 1 + (next() % 254) as u8][(next() % 2) as usize]
                    };
                    let rgb = next() & 0x00FF_FFFF;
                    FxInk::over(crate::premul_rgb(rgb, if a == 0 { 255 } else { a }), a)
                })
                .collect();
            let mut want = ground.clone();
            lay_in_order(&mut want, w, &laid, &inks, at);
            let sums: Vec<FxInk> = art
                .classes()
                .iter()
                .map(|m| {
                    if additive {
                        FxInk::add(m.iter().fold(0, |s, &id| {
                            crate::add_sat(s, inks[usize::from(id)].color) & 0x00FF_FFFF
                        }))
                    } else {
                        inks[usize::from(*m.last().expect("a member"))]
                    }
                })
                .collect();
            let mut folded = ground.clone();
            paint_layers(
                &mut folded,
                &frame(w, h),
                &[sprites(&[(Arc::clone(art.folded()), 0)], place, sums)],
                &[],
            );
            let peel = art
                .peeled()
                .iter()
                .map(|a| (Arc::clone(a), 0))
                .collect::<Vec<_>>();
            let mut peeled = ground.clone();
            paint_layers(
                &mut peeled,
                &frame(w, h),
                &[sprites(&peel, place, inks.clone())],
                &[],
            );
            assert_eq!(peeled, want, "seed {seed}: the peel, additive {additive}");
            if additive {
                assert_eq!(folded, want, "seed {seed}: the fold");
            } else {
                fold_breaks += usize::from(folded != want);
                let mut reversed = ground.clone();
                let back: Vec<_> = peel.iter().rev().cloned().collect();
                paint_layers(
                    &mut reversed,
                    &frame(w, h),
                    &[sprites(&back, place, inks.clone())],
                    &[],
                );
                reverse_breaks += usize::from(reversed != want);
            }
        }
    }
    assert!(fold_breaks > 0, "CONTROL: a fold of over inks is not exact");
    assert!(
        reverse_breaks > 0,
        "CONTROL: the peel drawn in reverse is not exact"
    );
}

/// A peeled art is as deep as the most pieces over one pixel, and a
/// class lists its members in the order laid, once per covering piece.
#[test]
fn a_class_keeps_its_members_in_order() {
    let laid = [
        (PxRect::at(0, 1, 3, 1), 0),
        (PxRect::at(1, 0, 1, 3), 0),
        (PxRect::at(1, 1, 1, 1), 1),
    ];
    let art = pieces(&laid).expect("pieces");
    assert_eq!(art.origin(), (0, 0));
    assert_eq!(art.depth(), 3);
    let centre = art.folded().texel(1, 1) as usize;
    assert_eq!(&*art.classes()[centre - 1], &[0, 0, 1]);
    assert_eq!(art.peeled()[2].texel(1, 1), 2, "the nucleus is laid third");
    assert_eq!(art.folded().texel(0, 0), 0, "a corner no piece covers");
    let max = u32::from(aterm_core::render::FX_ART_MAX_SIDE);
    assert!(pieces(&[(PxRect::at(0, 0, max, 1), 0)]).is_some());
    assert!(pieces(&[(PxRect::at(0, 0, max + 1, 1), 0)]).is_none());
    assert!(pieces(&[(PxRect::at(0, 0, 1, max + 1), 0)]).is_none());
    assert!(
        pieces(&[(
            PxRect::at(0, 0, u32::from(u16::MAX), u32::from(u16::MAX)),
            0
        )])
        .is_none(),
        "reject the hull before allocating a pixel table"
    );
}

/// A random art of `kind`: a filled blob with holes and stray texels, so
/// it has runs to merge and gaps to fill.
fn random_art(next: &mut impl FnMut() -> u32, kind: FxTexels) -> Arc<FxArt> {
    let (w, h) = (1 + next() % 24, 1 + next() % 24);
    let (cx, cy, r) = (next() % w, next() % h, 1 + next() % 12);
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x.abs_diff(cx), y.abs_diff(cy));
            let ring = (dx * dx + dy * dy).abs_diff(r * r) <= r;
            let on = ring || next().is_multiple_of(11);
            match kind {
                FxTexels::Index8 => data.push(if on { 1 + (next() % 4) as u8 } else { 0 }),
                FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => {
                    let a = (next() % 256) as u8;
                    let c = if on { [a / 2, a / 3, a, a] } else { [0; 4] };
                    let c = if on && next().is_multiple_of(5) {
                        [40, 0, 9, 0]
                    } else {
                        c
                    };
                    data.extend(c);
                }
            }
        }
    }
    Arc::new(FxArt::new(w as u16, h as u16, kind, data).expect("an art"))
}

/// One frame of `art` at `(x, y)` scaled to `dw × dh` (mirrored under
/// `flip`) on a random ground: the whole sprite once, then one item per
/// rectangle of `clips` (item-local pixels, each an item's clip).
fn whole_and_covered(
    art: &Arc<FxArt>,
    clips: &[PxRect],
    (x, y, dw, dh, flip): (i32, i32, u16, u16, bool),
    ground: &[u32],
    palette: &[FxInk],
) -> (Vec<u32>, Vec<u32>) {
    let blend = if art.texels() == FxTexels::Rgba8Straight {
        FxBlend::Linear
    } else {
        FxBlend::Exact
    };
    let item = |clip: PxRect| {
        FxItem::Sprite(FxSprite {
            x,
            y,
            dw,
            dh,
            flip_x: flip,
            art: Arc::clone(art),
            ink: 0,
            clip,
        })
    };
    let paint = |items: Vec<FxItem>| {
        let layer = FxLayer::new(
            7,
            Arc::new(FxContent::new(items, palette.to_vec())),
            FxSlot::Over,
            FxSpace::Window,
            blend,
        );
        let mut px = ground.to_vec();
        paint_layers(&mut px, &frame(48, 48), &[layer], &[]);
        px
    };
    let whole = paint(vec![item(PxRect::EVERYWHERE)]);
    let covered = paint(clips.iter().map(|l| item(l.translate(x, y))).collect());
    (whole, covered)
}

/// A cover draws its sprite's bytes identically to the whole sprite.
#[test]
fn a_cover_draws_its_sprites_bytes() {
    let mut next = rng(0xC0_7E2);
    let (mut short_breaks, mut floor_breaks, mut drawn) = (0, 0, 0);
    for round in 0..300 {
        let kind = [
            FxTexels::Index8,
            FxTexels::Rgba8Premul,
            FxTexels::Rgba8Straight,
        ][round % 3];
        let art = random_art(&mut next, kind);
        let palette: Vec<FxInk> = (0..5)
            .map(|_| FxInk::over(next() & 0x00FF_FFFF, (next() % 3 * 127) as u8))
            .collect();
        let ground: Vec<u32> = (0..48 * 48).map(|_| next() & 0x00FF_FFFF).collect();
        let at = (
            (next() % 20) as i32 - 4,
            (next() % 20) as i32 - 4,
            (1 + next() % 40) as u16,
            (1 + next() % 40) as u16,
            next().is_multiple_of(2),
        );
        let (aw, ah) = (art.w(), art.h());
        let (w, h) = (usize::from(aw), usize::from(ah));
        for max in [1, 2, 3, 5, 8, 64] {
            let rects = cover(&art, max);
            assert!(rects.len() <= max, "{} rectangles past {max}", rects.len());
            let mut held = vec![0u8; w * h];
            for r in &rects {
                assert!(!r.is_empty() && r.x0 >= 0 && r.y0 >= 0);
                assert!(
                    r.x1 as usize <= w && r.y1 as usize <= h,
                    "{r:?} outside the art"
                );
                for ty in r.y0..r.y1 {
                    for tx in r.x0..r.x1 {
                        held[ty as usize * w + tx as usize] += 1;
                    }
                }
            }
            for (i, &n) in held.iter().enumerate() {
                assert!(n <= 1, "two rectangles hold texel {i}");
                assert!(!draws(&art, i) || n == 1, "texel {i} draws, uncovered");
            }
            let px: Vec<PxRect> = rects
                .iter()
                .map(|&t| cover_px(t, (aw, ah), (at.2, at.3), at.4))
                .collect();
            let (whole, covered) = whole_and_covered(&art, &px, at, &ground, &palette);
            assert_eq!(
                whole, covered,
                "round {round}, max {max}: the cover's bytes"
            );
            drawn += usize::from(whole != ground);
            if let Some((_, short)) = px.split_last() {
                let (whole, covered) = whole_and_covered(&art, short, at, &ground, &palette);
                short_breaks += usize::from(whole != covered);
            }
            let down = |v: i32, n: u16, d: u16| v * i32::from(d) / i32::from(n);
            let floor: Vec<PxRect> = rects
                .iter()
                .map(|t| {
                    let (tx0, tx1) = if at.4 {
                        (i32::from(aw) - t.x1, i32::from(aw) - t.x0)
                    } else {
                        (t.x0, t.x1)
                    };
                    PxRect::new(
                        down(tx0, aw, at.2),
                        down(t.y0, ah, at.3),
                        down(tx1, aw, at.2),
                        down(t.y1, ah, at.3),
                    )
                })
                .collect();
            let (whole, by_floor) = whole_and_covered(&art, &floor, at, &ground, &palette);
            floor_breaks += usize::from(whole != by_floor);
        }
    }
    assert!(drawn > 1000, "the frames draw: {drawn}");
    assert!(
        short_breaks > 0,
        "CONTROL: a cover short of a rectangle loses bytes"
    );
    assert!(floor_breaks > 0, "CONTROL: the floor's mapping loses bytes");

    let art = Arc::new(FxArt::new(4, 3, FxTexels::Index8, vec![1; 12]).unwrap());
    let ground = vec![0x12_34_56; 48 * 48];
    let palette = [FxInk::over(0xAA_BB_CC, 255)];
    for flip in [false, true] {
        for (dw, dh) in [(3, 2), (17, 11)] {
            for rect in [
                PxRect::EVERYWHERE,
                PxRect::new(i32::MIN, -1, i32::MAX, i32::MAX),
            ] {
                let clip = cover_px(rect, (4, 3), (dw, dh), flip);
                assert_eq!(clip, PxRect::new(0, 0, i32::from(dw), i32::from(dh)));
                let (whole, covered) =
                    whole_and_covered(&art, &[clip], (2, 3, dw, dh, flip), &ground, &palette);
                assert_eq!(whole, covered);
                assert_ne!(whole, ground, "CONTROL: the unbounded clip draws");
            }
        }
        for rect in [
            PxRect::new(i32::MIN, 0, -1, 3),
            PxRect::new(5, 0, i32::MAX, 3),
            PxRect::new(3, 0, 1, 3),
            PxRect::EMPTY,
        ] {
            assert!(cover_px(rect, (4, 3), (17, 11), flip).is_empty());
        }
        for (art_size, drawn_size) in [
            ((0, 3), (17, 11)),
            ((4, 0), (17, 11)),
            ((4, 3), (0, 11)),
            ((4, 3), (17, 0)),
        ] {
            assert!(cover_px(PxRect::EVERYWHERE, art_size, drawn_size, flip).is_empty());
        }
        assert_eq!(
            cover_px(PxRect::EVERYWHERE, (4, 3), (u16::MAX, u16::MAX), flip),
            PxRect::new(0, 0, i32::from(u16::MAX), i32::from(u16::MAX))
        );
    }
}

/// A cover of an outline is a sliver of its box, and a blank art has no
/// rectangle.
#[test]
fn a_cover_of_an_outline_is_a_sliver_of_its_box() {
    let (w, h) = (60usize, 40usize);
    let mut data = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let edge = x < 2 || y < 2 || x >= w - 2 || y >= h - 2;
            data[y * w + x] = u8::from(edge);
        }
    }
    let art = FxArt::new(w as u16, h as u16, FxTexels::Index8, data).expect("an art");
    let rects = cover(&art, 8);
    let area: i64 = rects
        .iter()
        .map(|r| i64::from(r.x1 - r.x0) * i64::from(r.y1 - r.y0))
        .sum();
    assert_eq!(
        area,
        (2 * w + 2 * (h - 4)) as i64 * 2,
        "exactly the frame: {rects:?}"
    );
    assert_eq!(cover(&art, 1).len(), 1, "one rectangle: the box");
    let blank = FxArt::new(4, 4, FxTexels::Index8, vec![0; 16]).expect("an art");
    assert!(cover(&blank, 8).is_empty());
    let checks = |w: usize, h: usize| {
        let data: Vec<u8> = (0..w * h)
            .map(|i| u8::from((i % w + i / w).is_multiple_of(2)))
            .collect();
        FxArt::new(w as u16, h as u16, FxTexels::Index8, data).expect("an art")
    };
    let past = checks(128, 64);
    assert_eq!(
        &cover(&past, 4096)[..],
        &[PxRect::new(0, 0, 128, 64)],
        "4096 runs: the box"
    );
    assert_eq!(
        cover(&checks(64, 64), 4096).len(),
        COVER_EXACT_MAX,
        "CONTROL: 2048 runs are each a rectangle"
    );
    let mut late = vec![0u8; 128 * 70];
    for (i, byte) in late[..128 * 64].iter_mut().enumerate() {
        *byte = u8::from((i % 128 + i / 128).is_multiple_of(2));
    }
    late[128 * 70 - 1] = 1;
    let late = FxArt::new(128, 70, FxTexels::Index8, late).unwrap();
    assert_eq!(&cover(&late, 4096)[..], &[PxRect::new(0, 0, 128, 70)]);
}

/// `paint_quads` is the glow law: a gradient quad's columns ramp from its
/// left edge, and a quad past the buffer's edge is cut there.
#[test]
fn paint_quads_ramps_from_the_quads_left_edge() {
    let (w, h) = (8usize, 2usize);
    let q = GlowQuad::gradient(0, 4, 0, 8, 1, 0x0000_0000, 0, 0x00FF_0000, 0);
    let mut px = vec![0u32; w * h];
    paint_quads(&mut px, w, h, &[q], (0, 1));
    assert_eq!(px[..w], [0; 8], "the first row is untouched");
    let row = &px[w..];
    assert_eq!(row[..4], [0; 4]);
    assert_eq!(row[4], crate::glow_lerp_rgb(0, 0x00FF_0000, 0, 8));
    assert_eq!(row[7], crate::glow_lerp_rgb(0, 0x00FF_0000, 3, 8));
    let before = px.clone();
    for off in [(usize::MAX, 0), (0, usize::MAX)] {
        paint_quads(&mut px, w, h, &[q], off);
        assert_eq!(px, before, "an offscreen offset is clipped");
    }
    for (w, h) in [(usize::MAX, 2), (8, 3)] {
        paint_quads(&mut px, w, h, &[q], (0, 0));
        paint_layers(&mut px, &frame(w, h), &[], &[]);
        assert_eq!(px, before, "an invalid framebuffer is left untouched");
    }
}
