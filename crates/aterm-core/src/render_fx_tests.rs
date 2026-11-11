// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn index_art(w: u16, h: u16, data: &[u8]) -> Arc<FxArt> {
    Arc::new(FxArt::new(w, h, FxTexels::Index8, data.to_vec()).expect("sized"))
}

#[test]
fn an_art_is_addressed_by_its_kind_size_and_bytes() {
    let a = FxArt::new(2, 2, FxTexels::Index8, vec![0, 1, 2, 3]).unwrap();
    let same = FxArt::new(2, 2, FxTexels::Index8, vec![0, 1, 2, 3]).unwrap();
    assert_eq!(a.id(), same.id());
    let byte = FxArt::new(2, 2, FxTexels::Index8, vec![0, 1, 2, 4]).unwrap();
    let shape = FxArt::new(4, 1, FxTexels::Index8, vec![0, 1, 2, 3]).unwrap();
    let kind = FxArt::new(1, 1, FxTexels::Rgba8Premul, vec![0, 1, 2, 3]).unwrap();
    let kind2 = FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![0, 1, 2, 3]).unwrap();
    for other in [&byte, &shape, &kind, &kind2] {
        assert_ne!(a.id(), other.id(), "{other:?}");
    }
    assert_ne!(kind.id(), kind2.id());
    assert_eq!(a.max_index(), 3);
    assert_eq!(kind.max_index(), 0);
    assert!(FxArt::new(2, 2, FxTexels::Index8, vec![0; 3]).is_none());
    assert!(FxArt::new(1, 1, FxTexels::Rgba8Premul, vec![0; 3]).is_none());
    assert!(FxArt::new(0, 5, FxTexels::Index8, Vec::new()).is_some_and(|a| a.is_empty()));
    let max = FX_ART_MAX_SIDE;
    assert!(FxArt::new(max, max, FxTexels::Rgba8Premul, Vec::new()).is_none());
    assert!(FxArt::new(max, max, FxTexels::Rgba8Straight, Vec::new()).is_none());
    assert!(
        FxArt::new(max, 0, FxTexels::Rgba8Straight, Vec::new()).is_some_and(|art| art.is_empty())
    );
    let side = usize::from(max);
    assert!(FxArt::new(max, 1, FxTexels::Index8, vec![1; side]).is_some());
    assert!(FxArt::new(max + 1, 1, FxTexels::Index8, vec![1; side + 1]).is_none());
    assert!(FxArt::new(1, max + 1, FxTexels::Index8, vec![1; side + 1]).is_none());
    assert!(FxArt::new(max + 1, 0, FxTexels::Index8, Vec::new()).is_none());
    assert!(FxArt::new(u16::MAX, u16::MAX, FxTexels::Rgba8Premul, Vec::new()).is_none());
}

/// Every non-clear texel lies in one run of its own value and no clear texel in any; runs
/// are maximal and ordered.
#[test]
fn run_lists_expand_to_exactly_the_drawing_texels() {
    let empty = FxArt::new(0, 0, FxTexels::Index8, Vec::new()).unwrap();
    assert!(empty.runs().row(0).is_empty());
    assert!(empty.runs().row(usize::MAX).is_empty());
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for case in 0..300 {
        let (w, h) = ((next() % 17) as u16, (next() % 9) as u16);
        let texels = [
            FxTexels::Index8,
            FxTexels::Rgba8Premul,
            FxTexels::Rgba8Straight,
        ][case % 3];
        let n = usize::from(w) * usize::from(h) * texels.bytes_per_texel();
        let data: Vec<u8> = (0..n)
            .map(|_| [0u8, 0, 1, 2, 200][(next() % 5) as usize])
            .collect();
        let art = FxArt::new(w, h, texels, data).unwrap();
        let runs = art.runs();
        for y in 0..usize::from(h) {
            let mut covered = vec![None; usize::from(w)];
            let mut last_end = 0usize;
            for (k, run) in runs.row(y).iter().enumerate() {
                let (x0, x1) = (usize::from(run.x), usize::from(run.x + run.len));
                assert!(run.len >= 1 && x0 >= last_end && x1 <= usize::from(w));
                assert!(!texels.is_clear(run.value), "case {case}: a clear run");
                if x0 > 0 {
                    assert_ne!(art.texel(x0 - 1, y), run.value, "case {case} run {k}");
                }
                if x1 < usize::from(w) {
                    assert_ne!(art.texel(x1, y), run.value, "case {case} run {k}");
                }
                for c in &mut covered[x0..x1] {
                    *c = Some(run.value);
                }
                last_end = x1;
            }
            for (x, c) in covered.iter().enumerate() {
                let t = art.texel(x, y);
                if texels.is_clear(t) {
                    assert_eq!(*c, None, "case {case}: clear texel ({x}, {y}) in a run");
                } else {
                    assert_eq!(*c, Some(t), "case {case}: texel ({x}, {y})");
                }
            }
        }
        assert!(runs.row(usize::from(h)).is_empty());
    }
}

#[test]
fn seal_takes_the_hull_of_every_item_and_a_sprite_within_its_clip() {
    let art = index_art(2, 2, &[1, 1, 1, 1]);
    let content = FxContent::new(
        vec![
            FxItem::Rect(FxRect {
                x: -5,
                y: 3,
                w: 4,
                h: 2,
                ink: 0,
            }),
            FxItem::Sprite(FxSprite {
                x: 10,
                y: -7,
                dw: 20,
                dh: 20,
                flip_x: false,
                art: art.clone(),
                ink: 0,
                clip: PxRect::new(12, -100, 15, 100),
            }),
            FxItem::Rect(FxRect {
                x: 1000,
                y: 1000,
                w: 0,
                h: 9,
                ink: 0,
            }),
            FxItem::Ramps(FxRamps {
                x: 0,
                y: 30,
                w: 1,
                h: 1,
                ink: 0,
            }),
        ],
        vec![FxInk::add(0x10_2030); 2],
    );
    assert_eq!(content.extent(), PxRect::new(-5, -7, 15, 31));
    assert!(content.accepts(FxBlend::Exact));
    assert!(!content.accepts(FxBlend::Linear));
    assert_eq!(FxContent::default().extent(), PxRect::EMPTY);
    assert!(FxContent::default().accepts(FxBlend::Linear));
}

#[test]
fn edit_reseals_and_keeps_capacity() {
    let mut content = FxContent::new(Vec::with_capacity(16), vec![FxInk::add(1); 2]);
    content.edit(|items, _| {
        items.push(FxItem::Rect(FxRect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
            ink: 0,
        }));
    });
    assert_eq!(content.extent(), PxRect::new(1, 2, 4, 6));
    let cap = content.items().as_ptr();
    content.edit(|items, _| {
        items.clear();
        items.push(FxItem::Rect(FxRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            ink: 0,
        }));
    });
    assert_eq!(content.extent(), PxRect::new(0, 0, 1, 1));
    assert_eq!(content.items().as_ptr(), cap, "the refill reuses the Vec");

    let straight = Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![1, 2, 3, 255]).unwrap());
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        content.edit(|items, _| {
            items.clear();
            items.push(FxItem::Sprite(FxSprite {
                x: -4,
                y: 9,
                dw: 6,
                dh: 2,
                flip_x: false,
                art: straight,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            }));
            panic!("captured edit interruption");
        });
    }));
    assert!(interrupted.is_err());
    assert_eq!(content.items().len(), 1, "completed edits remain");
    assert_eq!(content.extent(), PxRect::new(-4, 9, 2, 11));
    assert!(
        content.accepts(FxBlend::Linear),
        "the new items set the blend"
    );
    assert!(!content.accepts(FxBlend::Exact));
    assert_eq!(content.items().as_ptr(), cap, "resealing keeps the Vec");

    content.edit(|items, _| items.clear());
    assert!(content.items().is_empty());
    assert_eq!(content.extent(), PxRect::EMPTY);
    assert!(content.accepts(FxBlend::Exact) && content.accepts(FxBlend::Linear));
}

/// The seal decides opacity again on every edit.
#[test]
fn a_content_draws_opaque_only_over_an_opaque_palette_without_rgba_texels() {
    let rect = FxItem::Rect(FxRect {
        x: 0,
        y: 0,
        w: 4,
        h: 2,
        ink: 0,
    });
    let ramps = FxItem::Ramps(FxRamps {
        x: 0,
        y: 2,
        w: 4,
        h: 1,
        ink: 0,
    });
    let sprite = |art: Arc<FxArt>| {
        FxItem::Sprite(FxSprite {
            x: 0,
            y: 0,
            dw: 2,
            dh: 2,
            flip_x: false,
            art,
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
    };
    let index = sprite(index_art(2, 1, &[1, 2]));
    let premul = sprite(Arc::new(
        FxArt::new(1, 1, FxTexels::Rgba8Premul, vec![9, 9, 9, 255]).unwrap(),
    ));
    let opaque = vec![FxInk::over(0x0010_2030, 255); 2];
    assert!(FxContent::default().draws_opaque(), "an empty content");
    for items in [vec![rect], vec![ramps], vec![index.clone()]] {
        assert!(FxContent::new(items.clone(), opaque.clone()).draws_opaque());
        for (k, ink) in [FxInk::over(0x0010_2030, 254), FxInk::add(0x0010_2030)]
            .into_iter()
            .enumerate()
        {
            let mut palette = opaque.clone();
            palette[k] = ink;
            assert!(
                !FxContent::new_unchecked(items.clone(), palette).draws_opaque(),
                "{items:?} over entry {k} = {ink:?}"
            );
        }
    }
    assert!(
        !FxContent::new(vec![premul], opaque.clone()).draws_opaque(),
        "an RGBA sprite's texels are not read"
    );
    let mut content = FxContent::new(vec![index], opaque);
    content.edit(|_, palette| palette[1].alpha = 128);
    assert!(!content.draws_opaque(), "the edit reseals");
    content.edit(|_, palette| palette[1].alpha = 255);
    assert!(content.draws_opaque());
}

#[test]
#[should_panic(expected = "mixes Exact and Linear")]
fn seal_refuses_a_mixture_of_blends() {
    let straight = Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![1; 4]).unwrap());
    let _ = FxContent::new(
        vec![
            FxItem::Rect(FxRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                ink: 0,
            }),
            FxItem::Sprite(FxSprite {
                x: 0,
                y: 0,
                dw: 1,
                dh: 1,
                flip_x: false,
                art: straight,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            }),
        ],
        vec![FxInk::add(1); 2],
    );
}

#[test]
#[should_panic(expected = "zero-size art")]
fn seal_refuses_a_zero_size_art() {
    let blank = Arc::new(FxArt::new(0, 3, FxTexels::Index8, Vec::new()).unwrap());
    let _ = FxContent::new(
        vec![FxItem::Sprite(FxSprite {
            x: 0,
            y: 0,
            dw: 4,
            dh: 4,
            flip_x: false,
            art: blank,
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })],
        Vec::new(),
    );
}

#[test]
#[should_panic(expected = "past a 3-entry palette")]
fn seal_refuses_an_index_past_the_palette() {
    let _ = FxContent::new(
        vec![FxItem::Sprite(FxSprite {
            x: 0,
            y: 0,
            dw: 2,
            dh: 1,
            flip_x: false,
            art: index_art(2, 1, &[1, 3]),
            ink: 1,
            clip: PxRect::EVERYWHERE,
        })],
        vec![FxInk::add(1); 3],
    );
}

#[test]
#[should_panic(expected = "past a 3-entry palette")]
fn seal_refuses_a_ramps_stack_past_the_palette() {
    let _ = FxContent::new(
        vec![FxItem::Ramps(FxRamps {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
            ink: 0,
        })],
        vec![FxInk::add(1); 3],
    );
}

#[test]
#[should_panic(expected = "additive at both ends")]
fn seal_refuses_a_rect_additive_at_one_end() {
    let _ = FxContent::new(
        vec![FxItem::Rect(FxRect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
            ink: 0,
        })],
        vec![FxInk::add(1), FxInk::over(1, 9)],
    );
}

#[test]
#[should_panic(expected = "ramps row 1 is additive at one end only")]
fn seal_refuses_a_ramps_row_additive_at_one_end() {
    let _ = FxContent::new(
        vec![FxItem::Ramps(FxRamps {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
            ink: 0,
        })],
        vec![
            FxInk::add(1),
            FxInk::add(2),
            FxInk::add(3),
            FxInk::over(1, 9),
        ],
    );
}

#[test]
#[should_panic(expected = "tint entry 2 past a 2-entry palette")]
fn seal_refuses_a_straight_tint_past_the_palette() {
    let straight = Arc::new(FxArt::new(1, 1, FxTexels::Rgba8Straight, vec![9; 4]).unwrap());
    let _ = FxContent::new(
        vec![FxItem::Sprite(FxSprite {
            x: 0,
            y: 0,
            dw: 1,
            dh: 1,
            flip_x: false,
            art: straight,
            ink: 2,
            clip: PxRect::EVERYWHERE,
        })],
        vec![FxInk::over(0x00FF_FFFF, 255); 2],
    );
}

#[test]
#[should_panic(expected = "fx layer 4: its content is not drawn under Linear")]
fn a_layer_refuses_content_its_blend_does_not_draw() {
    let rects = Arc::new(FxContent::new(
        vec![FxItem::Rect(FxRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            ink: 0,
        })],
        vec![FxInk::add(1); 2],
    ));
    let _ = FxLayer::new(4, rects, FxSlot::Over, FxSpace::Window, FxBlend::Linear);
}

#[test]
#[should_panic(expected = "fx layer 5: a Linear layer draws at Under or Over")]
fn a_layer_refuses_linear_at_glow() {
    let _ = FxLayer::new(
        5,
        Arc::new(FxContent::default()),
        FxSlot::Glow,
        FxSpace::Window,
        FxBlend::Linear,
    );
}

/// The unchecked seam seals the hull and the class but refuses nothing,
/// so the renderers' fail-closed law can be tested.
#[test]
fn the_unchecked_seam_seals_without_refusing() {
    let content = FxContent::new_unchecked(
        vec![FxItem::Rect(FxRect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
            ink: 7,
        })],
        Vec::new(),
    );
    assert_eq!(content.extent(), PxRect::new(0, 0, 2, 2));
}

#[test]
fn layer_equality_is_identity() {
    let content = Arc::new(FxContent::default());
    let base = FxLayer::new(
        7,
        content.clone(),
        FxSlot::Under,
        FxSpace::Window,
        FxBlend::Exact,
    );
    assert_eq!(base, base.clone());
    let twin = FxLayer {
        content: Arc::new(FxContent::default()),
        ..base.clone()
    };
    assert_ne!(base, twin, "equal bytes in a new Arc are a change");
    let variants: [fn(&mut FxLayer); 9] = [
        |l| l.id += 1,
        |l| l.slot = FxSlot::Over,
        |l| l.space = FxSpace::Grid,
        |l| l.blend = FxBlend::Linear,
        |l| l.bloom = true,
        |l| l.knock = true,
        |l| l.offset.1 += 1,
        |l| l.clip = PxRect::new(0, 0, 1, 1),
        |l| l.chrome = Some(PxRect::new(0, 0, 1, 1)),
    ];
    for (i, change) in variants.iter().enumerate() {
        let mut other = base.clone();
        change(&mut other);
        assert_ne!(base, other, "field change {i} must compare unequal");
    }
}

#[test]
fn rect_algebra() {
    let a = PxRect::new(0, 0, 10, 10);
    let b = PxRect::new(5, -5, 20, 5);
    assert_eq!(a.intersect(b), PxRect::new(5, 0, 10, 5));
    assert!(a.overlaps(b));
    assert!(!a.overlaps(PxRect::new(10, 0, 11, 1)), "half-open");
    assert_eq!(a.hull(b), PxRect::new(0, -5, 20, 10));
    assert_eq!(a.hull(PxRect::EMPTY), a);
    assert_eq!(PxRect::EMPTY.hull(PxRect::new(3, 3, 3, 9)), PxRect::EMPTY);
    assert_eq!(a.translate(-3, 4), PxRect::new(-3, 4, 7, 14));
    assert_eq!(PxRect::EVERYWHERE.translate(5, -5), PxRect::EVERYWHERE);
    let half = PxRect::new(i32::MIN, 0, 10, i32::MAX).translate(1, 1);
    assert_eq!(half, PxRect::new(i32::MIN, 1, 11, i32::MAX));
    assert_eq!(PxRect::at(i32::MAX - 1, 0, 5, 5).x1, i32::MAX);
    let layer = FxLayer {
        offset: (100, 50),
        clip: PxRect::new(0, 0, 103, 1000),
        ..FxLayer::new(
            1,
            Arc::new(FxContent::new(
                vec![FxItem::Rect(FxRect {
                    x: 1,
                    y: 2,
                    w: 5,
                    h: 5,
                    ink: 0,
                })],
                vec![FxInk::add(1); 2],
            )),
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Exact,
        )
    };
    assert_eq!(layer.extent(), PxRect::new(101, 52, 103, 57));
}
