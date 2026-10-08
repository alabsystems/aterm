// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Re-rasterization property gate (P1): the renderer's `set_px(new_px)` is the
// engine half of the host's devicePixelRatio fix — when a pane moves to a
// different-density display the host re-derives the cell font px and calls
// `set_px`, expecting the engine to (a) re-size the cell grid proportionally and
// (b) drop the glyph atlas/caches so the next render RE-RASTERIZES at the new px
// instead of upscaling stale, soft bitmaps. These properties lock that contract
// so a future cache-clearing or metrics regression in `set_px` fails loudly.
//
// A real, deterministic font (the bundled JetBrains Mono fixture) is embedded so
// the test never skips: the proportional-metrics property needs true font line
// metrics, and a synthetic stub would not exercise the fontdue path the host
// relies on.

use aterm_render::{Renderer, Theme};

// Bundled ligature font (same fixture the ligature gate uses). Embedded so the
// test always runs — the metrics scaling assertion needs real font
// metrics, not a host-dependent system font that may be absent under CI/SSH.
const FONT: &[u8] = include_bytes!("fixtures/jetbrains-mono.ttf");

fn renderer_at(px: f32) -> Renderer {
    Renderer::from_bytes(FONT, px, Theme::default()).expect("fixture font builds a renderer")
}

fn glyph_dims(r: &mut Renderer, ch: char) -> (usize, usize) {
    let key = r.glyph_key(ch);
    let img = r.glyph_image(key);
    (img.width(), img.height())
}

// PROPERTY (a): cell_width and cell_height scale (monotonically, roughly
// proportionally) with px. A strictly LARGER px must yield cells at least as
// large on BOTH axes and strictly larger on at least one — the metrics are
// re-derived from the font at the new px, never frozen at construction. A
// meaningful gap (>= 4 px) guarantees the integer ceil/round of cell metrics
// moves, avoiding flakiness from sub-pixel rounding at tiny deltas.
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
            // Ratio sanity: a near-doubling of px must NOT leave the cell
            // roughly the same size (that would mean metrics were not
            // re-derived). Generous slack for integer rounding.
            let want = large / small;
            let got_h = h1 as f32 / h0 as f32;
            assert!(
                got_h > 1.0 + (want - 1.0) * 0.5,
                "cell height must scale roughly with px: px x{want:.2} but height only x{got_h:.2}"
            );

            // Round-trip: shrinking back re-derives the ORIGINAL metrics.
            r.set_px(small);
            assert_eq!(r.cell_size(), (w0, h0), "set_px round-trips cell metrics");
        }
    }
}

// PROPERTY (b): set_px(new_px) INVALIDATES the glyph cache, and a subsequent
// render re-rasterizes at the new px — never the stale old-px bitmap.
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

            // The cache must be DROPPED (stale bitmaps were rasterized at the
            // old px) — the invariant the host's DPR fix depends on.
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

// A focused sanity case so the contract is also pinned at a human-readable
// size pair.
#[test]
fn set_px_doubling_clears_cache_and_doubles_height_anchor() {
    let mut r = renderer_at(16.0);
    let (_w16, h16) = glyph_dims(&mut r, 'W');
    assert!(r.glyph_cache_len() > 0);

    r.set_px(32.0);
    assert_eq!(r.glyph_cache_len(), 0, "doubling px clears the glyph cache");

    let (_w32, h32) = glyph_dims(&mut r, 'W');
    assert!(
        h32 > h16,
        "32px glyph must be taller than 16px (re-rasterized, not cached): {h16} -> {h32}"
    );
}

// set_px is a NO-OP when the size is unchanged (within the 0.01 epsilon): a warm
// cache survives, so an idle pane re-applying its current px never pays a needless
// atlas rebuild. This pins the early-return in set_px.
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
