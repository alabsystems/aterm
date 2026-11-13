// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxTexels,
    HostRowPixels, PxRect, RenderInput,
};
use crate::terminal::RenderCell;

fn content() -> Arc<FxContent> {
    Arc::new(FxContent::new(
        vec![FxItem::Rect(FxRect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
            ink: 0,
        })],
        vec![FxInk::add(0x0010_2030); 2],
    ))
}

fn art() -> Arc<FxArt> {
    Arc::new(FxArt::new(1, 1, FxTexels::Index8, vec![1]).expect("sized"))
}

fn frame(c: &Arc<FxContent>) -> RenderInput {
    let mut window = FxLayer::new(1, c.clone(), FxSlot::Over, FxSpace::Window, FxBlend::Exact);
    window.clip = PxRect::new(-3, 2, 60, 25);
    let mut grid = FxLayer::new(2, c.clone(), FxSlot::Under, FxSpace::Grid, FxBlend::Exact);
    grid.offset = (5, 7);
    grid.clip = PxRect::new(0, 0, 40, 30);
    grid.chrome = Some(PxRect::new(0, 10, 50, 20));
    let unclipped = FxLayer::new(3, c.clone(), FxSlot::Glow, FxSpace::Window, FxBlend::Exact);
    RenderInput {
        rows: 4,
        cols: 3,
        cells: vec![vec![RenderCell::default(); 3]; 4],
        fx_layers: vec![window, grid, unclipped],
        fx_occluders: vec![PxRect::new(0, 10, 50, 20)],
        fx_prefetch: vec![art()],
        ..Default::default()
    }
}

#[test]
fn fx_channels_compare_by_identity_and_prefetch_draws_nothing() {
    let c = content();
    let a = frame(&c);
    let mut b = frame(&c);
    b.fx_prefetch.clear();
    assert!(a == b, "same Arcs, prefetch ignored");
    let mut twin = frame(&content());
    twin.fx_prefetch.clone_from(&a.fx_prefetch);
    assert!(
        a != twin,
        "equal bytes behind another Arc are another layer"
    );
    let mut moved = frame(&c);
    moved.fx_layers[0].offset.0 += 1;
    assert!(a != moved);
    let mut slot = frame(&c);
    slot.fx_layers[0].slot = FxSlot::Glow;
    assert!(a != slot);
    let mut occ = frame(&c);
    occ.fx_occluders[0] = PxRect::new(0, 11, 50, 20);
    assert!(a != occ);
}

#[test]
fn the_damage_cache_keeps_the_layer_arcs_and_drops_the_prefetch() {
    let c = content();
    let src = frame(&c);
    let mut cache = RenderInput::default();
    cache.clone_damage_cache_from(&src);
    assert!(cache == src);
    assert!(Arc::ptr_eq(&cache.fx_layers[1].content, &c));
    assert_eq!(cache.fx_occluders, src.fx_occluders);
    assert!(cache.fx_prefetch.is_empty(), "prefetch is not cached");
    assert_eq!(
        Arc::strong_count(&c),
        7,
        "frame (3 layers), cache (3) and the local hold it"
    );
    let full = src.clone();
    assert!(Arc::ptr_eq(&full.fx_prefetch[0], &src.fx_prefetch[0]));
    let mut plain = src.clone();
    plain.clear_overlays();
    assert!(plain.fx_layers.is_empty() && plain.fx_occluders.is_empty());
    assert!(plain.fx_prefetch.is_empty());
}

#[test]
fn a_prepend_moves_each_layer_in_its_own_space() {
    let (strip, ch, top) = (2, 10, 6);
    let dy = i32::try_from(strip * ch).expect("small");
    let rows = vec![vec![RenderCell::default(); 3]; strip];
    let c = content();
    let prepend = |pixels| {
        let mut f = frame(&c);
        f.prepend_host_rows(
            strip,
            rows.iter().map(Vec::as_slice),
            ch,
            top,
            &mut Vec::new(),
            pixels,
        );
        f
    };
    let before = frame(&c);
    for (pixels, wdy) in [(HostRowPixels::Translate, dy), (HostRowPixels::Placed, 0)] {
        let f = prepend(pixels);
        assert_eq!(f.fx_layers[0].offset, (0, wdy), "{pixels:?}: Window offset");
        assert_eq!(
            f.fx_layers[0].clip,
            PxRect::new(-3, 2 + wdy, 60, 25 + wdy),
            "{pixels:?}: Window clip"
        );
        assert_eq!(f.fx_layers[2].offset, (0, wdy), "{pixels:?}: unclipped");
        assert_eq!(f.fx_layers[2].clip, PxRect::EVERYWHERE, "{pixels:?}");
        assert_eq!(
            f.fx_layers[1].offset,
            (5, 7 + dy),
            "{pixels:?}: Grid offset"
        );
        assert_eq!(
            f.fx_layers[1].clip,
            before.fx_layers[1].clip.translate(0, dy),
            "{pixels:?}: Grid clip"
        );
        assert_eq!(
            f.fx_occluders,
            vec![PxRect::new(0, 10 + wdy, 50, 20 + wdy)],
            "{pixels:?}: occluders"
        );
        assert_eq!(
            f.fx_layers[1].chrome.as_slice(),
            f.fx_occluders.as_slice(),
            "{pixels:?}: a layer's chrome moves with its occluder"
        );
        assert!(
            Arc::ptr_eq(&f.fx_layers[0].content, &c),
            "content untouched"
        );
    }
}
