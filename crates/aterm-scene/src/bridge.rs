// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The renderer **bridge** (feature `bridge`): translate a scene's local-pixel
//! [`SceneFrame`] into the renderer-facing `aterm_core::render::SpriteQuad`s, plus copy
//! the baked [`Atlas`] into a shareable `SceneAtlas`. Two responsibilities the renderer
//! must NOT do itself (it's a dumb consumer):
//!
//! 1. **Place** the scene's local box at `(band_x, band_y)` in grid-interior pixels.
//! 2. **Row-slice** every sprite to the single-cell-row-band invariant the dirty gate +
//!    GPU scissor rely on — a tall cat spanning 7 cell rows becomes 7 quads, each with its
//!    source rect cropped proportionally so the seams are continuous. Sprites are also
//!    cropped (dest + source together) against the grid edges, so a friend cat half-off the
//!    left never produces an out-of-range quad.
//!
//! Keeping this in `aterm-scene` (behind a feature) means the GUI and the render/GPU parity
//! tests share ONE placement+slicing implementation — they can't drift.

use crate::atlas::Atlas;
use crate::scene::{LocalSprite, SceneFrame};
use aterm_core::render::{SceneAtlas, SpriteQuad};

/// Copy a baked [`Atlas`] into a shareable [`SceneAtlas`] for `RenderInput`.
#[must_use]
pub fn scene_atlas(atlas: &Atlas) -> SceneAtlas {
    SceneAtlas {
        width: atlas.width,
        height: atlas.height,
        rgba: atlas.rgba.clone(),
        version: atlas.version,
    }
}

/// Where the scene's local box lives in the frame, in grid-interior pixels, plus the grid
/// extent and cell height for row-slicing.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// Panel column left edge in grid-interior px.
    pub band_x: u32,
    /// Band top-left Y in grid-interior px.
    pub band_y: u32,
    /// This panel's COLUMN width in px — sprites are cropped to `[band_x, band_x+col_w]`
    /// so a scene never bleeds into its neighbour in a horizontal stack.
    pub col_w: u32,
    /// Grid interior width in px (`cols * cell_w`) — the overall clamp bound.
    pub grid_w: u32,
    /// Grid interior height in px (`rows * cell_h`) — the vertical clamp bound.
    pub grid_h: u32,
    /// Cell height in px — the row-band size for slicing.
    pub cell_h: u32,
}

/// Translate a [`SceneFrame`] into `over`/`add` [`SpriteQuad`] lists (cleared first).
pub fn to_render(
    frame: &SceneFrame,
    atlas: &Atlas,
    p: Placement,
    over: &mut Vec<SpriteQuad>,
    add: &mut Vec<SpriteQuad>,
) {
    over.clear();
    add.clear();
    if p.cell_h == 0 || p.grid_w == 0 || p.grid_h == 0 {
        return;
    }
    emit(&frame.over, atlas, p, over);
    emit(&frame.add, atlas, p, add);
}

fn emit(sprites: &[LocalSprite], atlas: &Atlas, p: Placement, out: &mut Vec<SpriteQuad>) {
    let gw = p.grid_w as f32;
    let gh = p.grid_h as f32;
    let cell_h = p.cell_h as f32;
    for s in sprites {
        let (ax, ay, aw, ah) = Atlas::rect(atlas, s.sprite);
        if aw == 0 || ah == 0 || s.dst.w <= 0.0 || s.dst.h <= 0.0 || s.alpha <= 0.0 {
            continue;
        }
        // Global dest rect (grid-interior px) before clamping.
        let mut gx = p.band_x as f32 + s.dst.x;
        let gy = p.band_y as f32 + s.dst.y;
        let mut dw = s.dst.w;
        let dh = s.dst.h;

        // Horizontal crop against THIS PANEL'S COLUMN [left, right], cropping the SOURCE u
        // proportionally so the visible part is the correct slice (no squish, no bleed into
        // the neighbouring scene in a horizontal stack).
        let left = p.band_x as f32;
        let right = (p.band_x + p.col_w.min(p.grid_w.saturating_sub(p.band_x))) as f32;
        let (mut su0, mut su1) = (0.0f32, 1.0f32);
        if gx < left {
            let cut = ((left - gx) / dw).min(1.0);
            su0 = cut;
            dw -= left - gx;
            gx = left;
        }
        if gx + dw > right {
            let over_px = gx + dw - right;
            su1 = 1.0 - (over_px / dw).min(1.0);
            dw = right - gx;
        }
        if dw <= 0.5 || su1 <= su0 {
            continue;
        }
        // flip mirrors the source-u crop window.
        let (fu0, fu1) = if s.flip_x {
            (1.0 - su1, 1.0 - su0)
        } else {
            (su0, su1)
        };
        let src_ax = ax as f32 + fu0 * aw as f32;
        let src_aw = ((fu1 - fu0) * aw as f32).max(1.0);

        let alpha = (s.alpha * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        if alpha == 0 {
            continue;
        }
        let tint = s.tint & 0x00FF_FFFF;

        // Vertical row-slice: one quad per cell-row band the sprite spans.
        let r0 = (gy / cell_h).floor().max(0.0) as u32;
        let r_last = ((gy + dh - 0.001) / cell_h).floor();
        let r1 = if r_last < 0.0 {
            0
        } else {
            (r_last as u32 + 1).min((p.grid_h / p.cell_h).max(1))
        };
        for r in r0..r1 {
            let row_top = r as f32 * cell_h;
            let sy0 = gy.max(row_top);
            let sy1 = (gy + dh).min(row_top + cell_h).min(gh);
            if sy1 - sy0 <= 0.5 {
                continue;
            }
            let v0 = (sy0 - gy) / dh;
            let v1 = (sy1 - gy) / dh;
            // Integer source sub-rect that TILES EXACTLY across the per-row slices with a
            // SHARED boundary texel (no gap, no overlap). The GPU maps each slice's UV to the
            // sub-rect EDGES and samples LINEAR, so the boundary between two slices must map
            // to the SAME source texel on both sides or the sampler jumps ~1 texel there — a
            // visible horizontal seam every cell row. Rounding both edges guarantees that: a
            // boundary at global y = r*cell_h feeds the identical `(sy - gy)/dh` to `round`
            // as the bottom of row r-1 and the top of row r, so both resolve to one texel.
            // (An earlier floor-top/ceil-bottom scheme OVERLAPPED by a texel, which the CPU
            // sampler hid but the GPU rendered as a seam — see the
            // `slices_share_dest_and_source_boundaries` test.)
            let top = (ay as f32 + v0 * ah as f32).round();
            let bot = (ay as f32 + v1 * ah as f32).round();
            let src_ay = top.clamp(ay as f32, (ay + ah) as f32);
            let src_ah = (bot - top).clamp(1.0, ah as f32);
            out.push(SpriteQuad {
                row: r.min(u16::MAX as u32) as u16,
                x: gx.clamp(0.0, gw) as u16,
                y: sy0.clamp(0.0, gh) as u16,
                w: dw.clamp(1.0, gw) as u16,
                h: (sy1 - sy0).clamp(1.0, cell_h) as u16,
                ax: src_ax.clamp(0.0, f32::from(u16::MAX)) as u16,
                ay: src_ay.clamp(0.0, f32::from(u16::MAX)) as u16,
                aw: src_aw.clamp(1.0, f32::from(u16::MAX)) as u16,
                ah: src_ah.clamp(1.0, f32::from(u16::MAX)) as u16,
                tint,
                alpha,
                flip_x: s.flip_x,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalSprite, Rect, SceneFrame, Sprite};

    /// A tall sprite sliced into per-row quads must TILE EXACTLY in both dest and source:
    /// adjacent slices share their boundary (row r's bottom == row r+1's top) with NO gap and
    /// NO overlap. A gap shows the HUD background as a seam; an overlap duplicates a boundary
    /// texel, which the GPU's linear sampler renders as a seam (the CPU sampler hid it). This
    /// is the invariant the shared-edge rounding in `emit` guarantees.
    #[test]
    fn slices_share_dest_and_source_boundaries() {
        use crate::{LocalSprite, Rect, Sprite};
        let cell_h = 20u32;
        let grid_h = 400u32;
        // One opaque sprite spanning the whole 400px band → 20 full interior row slices.
        let mut frame = SceneFrame::new();
        let mut s = LocalSprite::new(Sprite::House, Rect::new(0.0, 0.0, 120.0, grid_h as f32));
        s.tint = 0x00FF_FFFF;
        s.alpha = 1.0;
        frame.push_over(s);

        let p = Placement {
            band_x: 0,
            band_y: 0,
            col_w: 640,
            grid_w: 640,
            grid_h,
            cell_h,
        };
        let atlas = crate::atlas::Atlas::bake(1);
        let mut over = Vec::new();
        let mut add = Vec::new();
        to_render(&frame, &atlas, p, &mut over, &mut add);

        // All quads belong to the one sprite; order by row and check adjacency.
        over.sort_by_key(|q| q.row);
        assert!(
            over.len() >= 2,
            "sprite sliced into multiple rows: {}",
            over.len()
        );
        for pair in over.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if b.row != a.row + 1 {
                continue;
            }
            // Dest tiles exactly (no gap, no overlap).
            assert_eq!(
                a.y + a.h,
                b.y,
                "dest slices abut: row {} ends {} but row {} starts {}",
                a.row,
                a.y + a.h,
                b.row,
                b.y
            );
            // Source shares the boundary texel exactly (round-to-nearest on both edges).
            assert_eq!(
                a.ay + a.ah,
                b.ay,
                "source slices share the boundary texel: row {} src-bottom {} vs row {} src-top {}",
                a.row,
                a.ay + a.ah,
                b.row,
                b.ay
            );
        }
    }

    #[test]
    fn quads_are_row_sliced_and_in_bounds() {
        // A frame of a few tall sprites spanning multiple rows — enough to exercise the
        // row-slicer + column crop without any concrete scene.
        let mut frame = SceneFrame::new();
        frame.push_over(LocalSprite::new(
            Sprite::Pixel,
            Rect::new(10.0, 0.0, 240.0, 200.0),
        ));
        frame.push_over(LocalSprite::new(
            Sprite::Glow,
            Rect::new(300.0, 40.0, 140.0, 150.0),
        ));
        frame.push_add(LocalSprite::new(
            Sprite::Glow,
            Rect::new(520.0, 10.0, 90.0, 180.0),
        ));

        // Band is the bottom 200px of a 640x400 grid, cells 8x20 → 10 band rows at row 10.
        let cell_h = 20u32;
        let grid_h = 400u32;
        let p = Placement {
            band_x: 0,
            band_y: grid_h - 200,
            col_w: 640,
            grid_w: 640,
            grid_h,
            cell_h,
        };
        let mut over = Vec::new();
        let mut add = Vec::new();
        let atlas = crate::atlas::Atlas::bake(1);
        to_render(&frame, &atlas, p, &mut over, &mut add);
        assert!(!over.is_empty(), "produced sprite quads");
        for q in over.iter().chain(&add) {
            // single row band
            let band_top = q.row as u32 * cell_h;
            assert!(
                (q.y as u32) >= band_top && (q.y as u32 + q.h as u32) <= band_top + cell_h,
                "quad spans one row band: row={} y={} h={}",
                q.row,
                q.y,
                q.h
            );
            // inside grid
            assert!(q.x as u32 + q.w as u32 <= p.grid_w, "x in grid");
            assert!((q.row as u32) < p.grid_h / cell_h, "row in grid");
            // source rect inside atlas
            assert!(q.ax as u32 + q.aw as u32 <= atlas.width, "src x in atlas");
            assert!(q.ay as u32 + q.ah as u32 <= atlas.height, "src y in atlas");
        }
    }
}
