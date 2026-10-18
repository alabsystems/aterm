// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// `set_px` re-rasterizes: when a pane moves to a display of another density the
// host re-derives the cell px and calls `set_px`, which must re-size the cell
// grid proportionally and drop the glyph caches so the next render rasterizes
// at the new px instead of upscaling stale bitmaps. The bundled JetBrains Mono
// fixture gives real line metrics on every host.

use aterm_render::{Renderer, Theme};

// The ligature gate's fixture, embedded so the test never depends on a
// system font.
const FONT: &[u8] = include_bytes!("fixtures/jetbrains-mono.ttf");

fn renderer_at(px: f32) -> Renderer {
    Renderer::from_bytes(FONT, px, Theme::default()).expect("fixture font builds a renderer")
}

fn glyph_dims(r: &mut Renderer, ch: char) -> (usize, usize) {
    let key = r.glyph_key(ch);
    let img = r.glyph_image(key);
    (img.width(), img.height())
}

// Cell metrics are re-derived at the new px: a larger px gives cells at least
// as large on both axes and strictly larger on one. Deltas are >= 4 px so the
// integer cell metrics always move.
#[test]
fn set_px_scales_cell_metrics_monotonically() {
    for small in [10.0f32, 13.5, 17.25, 24.0, 31.5, 39.75] {
        for delta in [4.0f32, 7.5, 16.0, 27.25, 39.5] {
            let large = small + delta;
            let mut r = renderer_at(small);
            let (w0, h0) = r.cell_size();

            r.set_px(large);
            let (w1, h1) = r.cell_size();

            assert!(
                w1 >= w0,
                "{small}->{large}: cell width shrank: {w0} -> {w1}"
            );
            assert!(
                h1 >= h0,
                "{small}->{large}: cell height shrank: {h0} -> {h1}"
            );
            assert!(
                w1 > w0 || h1 > h0,
                "{small}->{large}: a >=4px size increase must grow a cell axis"
            );
            // Height scales roughly with px (slack for integer rounding).
            let want = large / small;
            let got_h = h1 as f32 / h0 as f32;
            assert!(
                got_h > 1.0 + (want - 1.0) * 0.5,
                "cell height must scale roughly with px: px x{want:.2} but height only x{got_h:.2}"
            );

            // Round-trip: shrinking back re-derives the original metrics.
            r.set_px(small);
            assert_eq!(r.cell_size(), (w0, h0), "set_px round-trips cell metrics");
        }
    }
}

// `set_px` invalidates the glyph cache; the next render re-rasterizes at the
// new px.
#[test]
fn set_px_invalidates_glyph_cache_and_rerasterizes() {
    for small in [12.0f32, 15.5, 19.25, 23.0, 27.75] {
        for delta in [6.0f32, 11.5, 20.0, 29.75] {
            let large = small + delta;
            let mut r = renderer_at(small);

            // Warm the atlas at the small px.
            let (w_small, h_small) = glyph_dims(&mut r, 'M');
            assert!(
                r.glyph_cache_len() > 0,
                "rasterizing must populate the glyph cache"
            );
            assert!(w_small > 0 && h_small > 0, "small-px 'M' must rasterize");

            // The old-px bitmaps must be dropped.
            r.set_px(large);
            assert_eq!(
                r.glyph_cache_len(),
                0,
                "set_px must clear the glyph atlas so glyphs re-rasterize at the new px"
            );

            let (_w_large, h_large) = glyph_dims(&mut r, 'M');
            assert!(
                r.glyph_cache_len() > 0,
                "re-rasterization re-populates the cache"
            );
            assert!(
                h_large > h_small,
                "{small}->{large}: re-rasterized glyph must be taller: {h_small} -> {h_large}"
            );
        }
    }
}

// An unchanged size (within `set_px`'s 0.01 epsilon) keeps the warm cache, so
// re-applying the current px never rebuilds the atlas.
#[test]
fn set_px_same_size_keeps_the_warm_cache() {
    let mut r = renderer_at(18.0);
    let _ = glyph_dims(&mut r, 'A');
    let warm = r.glyph_cache_len();
    assert!(warm > 0);
    r.set_px(18.0);
    assert_eq!(
        r.glyph_cache_len(),
        warm,
        "set_px to the same size must not drop the warm cache"
    );
}
