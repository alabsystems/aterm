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
use proptest::prelude::*;

// Bundled ligature font (same fixture the ligature gate uses). Embedded so the
// property test always runs — the metrics scaling assertion needs real font
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

proptest! {
    // PROPERTY (a): cell_width and cell_height scale (monotonically, roughly
    // proportionally) with px. A strictly LARGER px must yield cells at least as
    // large on BOTH axes and strictly larger on at least one — the metrics are
    // re-derived from the font at the new px, never frozen at construction. We use
    // a meaningful gap (>= 4 px) so the integer ceil/round of cell metrics is
    // guaranteed to move, avoiding flakiness from sub-pixel rounding at tiny deltas.
    #[test]
    fn set_px_scales_cell_metrics_monotonically(
        base in 10.0f32..40.0,
        delta in 4.0f32..40.0,
    ) {
        let small = base;
        let large = base + delta;

        let mut r = renderer_at(small);
        let (w0, h0) = r.cell_size();

        r.set_px(large);
        let (w1, h1) = r.cell_size();

        prop_assert!(w1 >= w0, "wider px must not shrink cell width: {w0} -> {w1}");
        prop_assert!(h1 >= h0, "wider px must not shrink cell height: {h0} -> {h1}");
        prop_assert!(
            w1 > w0 || h1 > h0,
            "a >=4px size increase must grow at least one cell axis: ({w0},{h0}) -> ({w1},{h1})"
        );
        // Ratio sanity: a near-doubling of px must NOT leave the cell roughly the
        // same size (that would mean metrics were not re-derived). Allow generous
        // slack for integer rounding, but reject "barely moved".
        let want = large / small;
        let got_h = h1 as f32 / h0 as f32;
        prop_assert!(
            got_h > 1.0 + (want - 1.0) * 0.5,
            "cell height must scale roughly with px: px x{want:.2} but height only x{got_h:.2}"
        );

        // Round-trip: shrinking back re-derives the ORIGINAL metrics (no drift).
        r.set_px(small);
        prop_assert_eq!(r.cell_size(), (w0, h0), "set_px round-trips cell metrics");
    }

    // PROPERTY (b): set_px(new_px) INVALIDATES the glyph cache, and a subsequent
    // render re-rasterizes at the new px. We rasterize a glyph (filling the cache),
    // assert the cache is non-empty, then set_px and assert the cache is EMPTY
    // (dropped), then re-rasterize and assert the new image is sized for the new px
    // (not the stale old-px bitmap).
    #[test]
    fn set_px_invalidates_glyph_cache_and_rerasterizes(
        base in 12.0f32..28.0,
        delta in 6.0f32..30.0,
    ) {
        let small = base;
        let large = base + delta;

        let mut r = renderer_at(small);

        // Warm the atlas at the small px.
        let (w_small, h_small) = glyph_dims(&mut r, 'M');
        prop_assert!(r.glyph_cache_len() > 0, "rasterizing must populate the glyph cache");
        prop_assert!(w_small > 0 && h_small > 0, "small-px 'M' must rasterize to a real bitmap");

        // Re-size: the cache must be DROPPED (stale bitmaps were rasterized at the
        // old px). This is the invariant the host's DPR fix depends on — without it
        // the new frame would blit upscaled, soft glyphs.
        r.set_px(large);
        prop_assert_eq!(
            r.glyph_cache_len(),
            0,
            "set_px must clear the glyph atlas so glyphs re-rasterize at the new px"
        );

        // The NEXT rasterization produces a glyph sized for the LARGER px (it was
        // re-rasterized, not upscaled from the dropped cache).
        let (_w_large, h_large) = glyph_dims(&mut r, 'M');
        prop_assert!(r.glyph_cache_len() > 0, "re-rasterization re-populates the cache");
        prop_assert!(
            h_large > h_small,
            "re-rasterized glyph must be taller at the larger px: {h_small} -> {h_large}"
        );
    }
}

// A focused (non-proptest) sanity case so the contract is also pinned at a fixed,
// human-readable size pair, and so the suite has a deterministic regression anchor
// even if proptest shrinking ever masks a corner.
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
