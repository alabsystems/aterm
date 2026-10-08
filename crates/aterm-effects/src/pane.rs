// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pane placement for a composed (split) frame: where one pane sits in the
//! window grid, and translators that move a pane-local effect stream into
//! window-content coordinates and clip it at the pane's interior edges, so no
//! light or fur crosses a divider. The per-pane word pass, prism wake and rain
//! place their channels through these.

/// Translate one pane's pane-local rain emission into window-content coords
/// and clip it to the pane's grid-interior box. Rain streams are grid-interior
/// pixels plus a viewport row tag, so the shift is `row += row_off`,
/// `x += col_off·cell_w`, `y += row_off·cell_h`. The clip is defensive for halo
/// spill; a clipped halo renders the same pixels over the surviving area, since
/// its falloff depends only on the shifted centre and per-quad params. Quads
/// that would leave u16 pixel space are dropped.
#[allow(
    clippy::too_many_arguments,
    reason = "the pane placement (row/col offset + rows/cols) and cell metrics are one flat geometry tuple; a wrapper struct would relocate the list, not simplify it"
)]
pub fn translate_rain_into_pane(
    quads: &mut Vec<aterm_render::SpriteQuad>,
    add: &mut Vec<aterm_render::RainHalo>,
    row_off: u16,
    col_off: u16,
    pane_rows: u16,
    pane_cols: u16,
    cell_w: u32,
    cell_h: u32,
) {
    let dx = u32::from(col_off) * cell_w;
    let dy = u32::from(row_off) * cell_h;
    let x1 = (u32::from(col_off) + u32::from(pane_cols)) * cell_w;
    let y1 = (u32::from(row_off) + u32::from(pane_rows)) * cell_h;
    let clip = move |x: u32, y: u32, w: u32, h: u32| -> Option<(u16, u16, u16, u16)> {
        let (nx0, ny0) = (x + dx, y + dy);
        let nx1 = nx0.saturating_add(w).min(x1);
        let ny1 = ny0.saturating_add(h).min(y1);
        // LAZY `then` (the pane_clip closure's law): `then_some` would
        // evaluate the subtractions even when the guard is false.
        (nx1 > nx0 && ny1 > ny0 && nx1 <= u32::from(u16::MAX) && ny1 <= u32::from(u16::MAX)).then(
            || {
                (
                    nx0 as u16,
                    ny0 as u16,
                    (nx1 - nx0) as u16,
                    (ny1 - ny0) as u16,
                )
            },
        )
    };
    quads.retain_mut(|q| {
        let Some((x, y, w, h)) = clip(
            u32::from(q.x),
            u32::from(q.y),
            u32::from(q.w),
            u32::from(q.h),
        ) else {
            return false;
        };
        (q.x, q.y, q.w, q.h) = (x, y, w, h);
        q.row = q.row.saturating_add(row_off);
        true
    });
    add.retain_mut(|q| {
        let Some((x, y, w, h)) = clip(
            u32::from(q.x),
            u32::from(q.y),
            u32::from(q.w),
            u32::from(q.h),
        ) else {
            return false;
        };
        (q.x, q.y, q.w, q.h) = (x, y, w, h);
        q.row = q.row.saturating_add(row_off);
        // The falloff CENTRE shifts with the quad (it may lie outside the
        // clipped rect — the radial math is absolute, so light stays exact).
        q.cx = (u32::from(q.cx) + dx).min(u32::from(u16::MAX)) as u16;
        q.cy = (u32::from(q.cy) + dy).min(u32::from(u16::MAX)) as u16;
        true
    });
}

/// Where one pane sits inside the composed window grid — the geometry every
/// `translate_*_into_pane` helper below needs to move a pane-local effect
/// stream into window-content coordinates and clip it to its own pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanePlace {
    pub row_off: u16,
    pub col_off: u16,
    pub rows: u16,
    pub cols: u16,
    pub win_rows: u16,
    pub win_cols: u16,
    pub cell_w: u32,
    pub cell_h: u32,
}

impl PanePlace {
    /// The pane's grid-interior pixel box, as `(x0, y0, x1, y1)` in i32.
    ///
    /// An edge on the window's own edge stays open (a sprite may overhang the
    /// window pad, as `pane_clip` allows for cursor effects). Every interior
    /// edge is a divider and clips hard.
    fn px_box(self) -> (i32, i32, i32, i32) {
        let (cw, ch) = (self.cell_w as i32, self.cell_h as i32);
        // Half the range keeps `x0 + w` style arithmetic far from overflow
        // while still being unreachable for any real sprite coordinate.
        let open_lo = i32::MIN / 2;
        let open_hi = i32::MAX / 2;
        let x0 = if self.col_off == 0 {
            open_lo
        } else {
            i32::from(self.col_off) * cw
        };
        let y0 = if self.row_off == 0 {
            open_lo
        } else {
            i32::from(self.row_off) * ch
        };
        let x1 = if self.col_off.saturating_add(self.cols) >= self.win_cols {
            open_hi
        } else {
            (i32::from(self.col_off) + i32::from(self.cols)) * cw
        };
        let y1 = if self.row_off.saturating_add(self.rows) >= self.win_rows {
            open_hi
        } else {
            (i32::from(self.row_off) + i32::from(self.rows)) * ch
        };
        (x0, y0, x1, y1)
    }

    /// Grid-interior pixel shift from pane-local to window-content coords.
    fn px_shift(self) -> (i32, i32) {
        (
            i32::from(self.col_off) * self.cell_w as i32,
            i32::from(self.row_off) * self.cell_h as i32,
        )
    }
}

/// Move one pane's sparkle-word decorations into the composed window grid.
/// `row`/`col` are CELLS in the same viewport space as the cursor, so the shift
/// is the pane's cell offset; the sub-cell `dx`/`dy` jitter is pane-independent
/// and rides untouched.
pub fn translate_decos_into_pane(decos: &mut Vec<aterm_render::WordDecoration>, place: PanePlace) {
    decos.retain_mut(|d| {
        if d.row >= place.rows || d.col >= place.cols {
            return false;
        }
        d.row += place.row_off;
        d.col += place.col_off;
        true
    });
}

/// Move one pane's animated-ink fg overrides into the composed window grid
/// (the [`translate_decos_into_pane`] twin). The caller must re-sort the merged
/// stream by `(row, col)`: `ink_row_slice` binary-searches it and
/// `debug_assert!`s strict column order.
pub fn translate_ink_into_pane(ink: &mut Vec<aterm_render::InkCell>, place: PanePlace) {
    ink.retain_mut(|c| {
        if c.row >= place.rows || c.col >= place.cols {
            return false;
        }
        c.row += place.row_off;
        c.col += place.col_off;
        true
    });
}

/// Move one pane's free-overlay sprites (peeking cats, gaze dots, the cursor
/// companion) into the composed window grid.
///
/// `FreeSprite` dest origins are signed grid-interior pixels, so the shift is
/// `col_off·cell_w` / `row_off·cell_h`. A sprite straddling an interior edge
/// is cropped in its dest rect and, in lockstep, its atlas source rect.
///
/// The crop is proportional (scaled by `aw/w`): the cursor companion's dest
/// size is pose-scaled while `aw/ah` is the natural art size. At 1:1 it reduces
/// to texel arithmetic.
pub fn translate_free_into_pane(free: &mut Vec<aterm_core::render::FreeSprite>, place: PanePlace) {
    let px_box = place.px_box();
    let (dx, dy) = place.px_shift();
    free.retain_mut(|s| {
        s.x = s.x.saturating_add(dx);
        s.y = s.y.saturating_add(dy);
        crop_free_sprite(s, px_box)
    });
}

/// Crop one free sprite to the pixel box `(x0, y0, x1, y1)` — dest rect and,
/// in lockstep, its atlas source rect, by the sprite's own scale (see
/// [`translate_free_into_pane`]). `false`: nothing of it is left in the box.
pub fn crop_free_sprite(
    s: &mut aterm_core::render::FreeSprite,
    px_box: (i32, i32, i32, i32),
) -> bool {
    let (bx0, by0, bx1, by1) = px_box;
    let (x0, y0) = (s.x, s.y);
    let x1 = x0.saturating_add(i32::from(s.w));
    let y1 = y0.saturating_add(i32::from(s.h));
    let (cx0, cy0) = (x0.max(bx0), y0.max(by0));
    let (cx1, cy1) = (x1.min(bx1), y1.min(by1));
    if cx1 <= cx0 || cy1 <= cy0 {
        return false;
    }
    s.x = cx0;
    s.y = cy0;
    if (cx0, cy0, cx1, cy1) == (x0, y0, x1, y1) {
        return true; // wholly inside: a pure shift, no atlas math
    }
    // Map a dest-space span onto the atlas by the sprite's own scale.
    // Rounds to nearest so a one-pixel dest sliver still selects a texel
    // rather than collapsing to an empty source rect (which the backends
    // would treat as nothing to sample).
    let scale = |span: i32, dest: u16, atlas: u16| -> u16 {
        if dest == 0 {
            return 0;
        }
        let n = i64::from(span) * i64::from(atlas) + i64::from(dest) / 2;
        (n / i64::from(dest)).clamp(0, i64::from(u16::MAX)) as u16
    };
    let (aw0, ah0) = (s.aw, s.ah);
    let (w0, h0) = (s.w, s.h);
    // A MIRRORED sprite runs its atlas window the other way: both backends
    // start sampling at `ax + aw` and walk backwards (`aterm-render`'s
    // `aw - 1 - sx`, the GPU's negative `du`). So dest x = 0 is the atlas's
    // RIGHT edge, and trimming pixels off the LEFT of the destination must
    // trim them off the RIGHT of the source — leaving `ax` where it is. The
    // unmirrored formula would advance `ax` into the wrong half of the tile
    // and draw a cat sliced from its own middle. Only the pet mirrors today
    // (facing), and only a pet straddling a pane divider is cropped at all,
    // which is exactly the case a split makes ordinary.
    let left_trim = cx0 - x0;
    let right_trim = x1 - cx1;
    s.ax = s.ax.saturating_add(scale(
        if s.flip_x { right_trim } else { left_trim },
        w0,
        aw0,
    ));
    s.ay = s.ay.saturating_add(scale(cy0 - y0, h0, ah0));
    s.w = (cx1 - cx0) as u16;
    s.h = (cy1 - cy0) as u16;
    // A cropped source must keep at least one texel, or the sprite is
    // present in the stream with nothing to sample from.
    s.aw = scale(cx1 - cx0, w0, aw0).max(1);
    s.ah = scale(cy1 - cy0, h0, ah0).max(1);
    true
}

/// Move one pane's supernova additive light into the composed window grid.
/// `GlowQuad` in the `nova_add` stream is GRID-INTERIOR `u16` pixels plus a
/// `row` damage hint, so both shift; the pixel shift saturates (clamping, never
/// wrapping) and the quad is then clipped to the pane box.
pub fn translate_nova_into_pane(nova: &mut Vec<aterm_render::GlowQuad>, place: PanePlace) {
    let (bx0, by0, bx1, by1) = place.px_box();
    let (dx, dy) = place.px_shift();
    nova.retain_mut(|q| {
        let x0 = i32::from(q.x).saturating_add(dx);
        let y0 = i32::from(q.y).saturating_add(dy);
        let x1 = x0.saturating_add(i32::from(q.w));
        let y1 = y0.saturating_add(i32::from(q.h));
        let (cx0, cy0) = (x0.max(bx0).max(0), y0.max(by0).max(0));
        let (cx1, cy1) = (
            x1.min(bx1).min(i32::from(u16::MAX)),
            y1.min(by1).min(i32::from(u16::MAX)),
        );
        if cx1 <= cx0 || cy1 <= cy0 {
            return false;
        }
        q.x = cx0 as u16;
        q.y = cy0 as u16;
        q.w = (cx1 - cx0) as u16;
        q.h = (cy1 - cy0) as u16;
        q.row = q.row.saturating_add(place.row_off);
        true
    });
}

#[cfg(test)]
mod pane_free_crop_tests {
    use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};

    use super::{PanePlace, translate_free_into_pane};

    fn sprite(x: i32, w: u16, ax: u16, aw: u16, flip_x: bool) -> FreeSprite {
        FreeSprite {
            x,
            y: 0,
            w,
            h: 4,
            ax,
            ay: 0,
            aw,
            ah: 4,
            tint: 0x00FF_FFFF,
            alpha: 255,
            flip_x,
            z: FreeZ::OverText,
            sampler: FreeSampler::Nearest,
        }
    }

    /// The atlas column each destination column samples, by the CPU renderer's
    /// own rule (`aterm-render`: `sx = if flip_x { aw - 1 - sx0 } else { sx0 }`).
    /// 1:1 scale, so `sx0` is the destination column.
    fn sampled(s: &FreeSprite) -> Vec<u16> {
        (0..s.w)
            .map(|i| {
                let sx = if s.flip_x { s.aw - 1 - i } else { i };
                s.ax + sx
            })
            .collect()
    }

    /// An interior pane: `cols` cells from window column 4 in a 40-column
    /// window, 1x1 px cells. `px_box` leaves window edges open, so only an
    /// interior pane exercises divider crops.
    fn place(cols: u16) -> PanePlace {
        PanePlace {
            row_off: 1,
            col_off: 4,
            rows: 4,
            cols,
            win_rows: 40,
            win_cols: 40,
            cell_w: 1,
            cell_h: 1,
        }
    }

    /// The pixels that survive a crop are the same pixels as before, checked
    /// against the renderer's sampling rule rather than the crop's arithmetic.
    #[test]
    fn cropping_preserves_which_texels_the_surviving_pixels_sample() {
        for flip in [false, true] {
            // 8 px wide starting 2 px left of the pane: the left 2 are cut.
            let full = sprite(-2, 8, 100, 8, flip);
            let want: Vec<u16> = sampled(&full).into_iter().skip(2).collect();
            let mut v = vec![full];
            translate_free_into_pane(&mut v, place(6));
            let got = sampled(&v[0]);
            assert_eq!(
                got, want,
                "flip_x={flip}: a left crop must keep the same texels"
            );
        }
    }
}
