// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pane placement and coordinate translators for a composed (split) frame.

/// Translate one pane's rain emission into window-content coords: a cell outside the pane's
/// grid-interior box is dropped (cells are cell-aligned, so none is ever trimmed) and a halo is
/// clipped to it.
#[allow(
    clippy::too_many_arguments,
    reason = "the pane placement (row/col offset + rows/cols) and cell metrics are one flat geometry tuple; a wrapper struct would relocate the list, not simplify it"
)]
#[inline]
pub fn translate_rain_into_pane(
    quads: &mut Vec<crate::matrix_rain::RainCell>,
    add: &mut Vec<aterm_render::RainHalo>,
    row_off: u16,
    col_off: u16,
    pane_rows: u16,
    pane_cols: u16,
    cell_w: u32,
    cell_h: u32,
) {
    if quads.is_empty() && add.is_empty() {
        return;
    }
    let dx = u32::from(col_off) * cell_w;
    let dy = u32::from(row_off) * cell_h;
    let x1 = (u32::from(col_off) + u32::from(pane_cols)) * cell_w;
    let y1 = (u32::from(row_off) + u32::from(pane_rows)) * cell_h;
    #[inline]
    const fn clip_rect(
        (x, y, w, h): (u32, u32, u32, u32),
        (dx, dy, x1, y1): (u32, u32, u32, u32),
    ) -> Option<(u16, u16, u16, u16)> {
        let (nx0, ny0) = (x + dx, y + dy);
        let nx1_raw = nx0.saturating_add(w);
        let ny1_raw = ny0.saturating_add(h);
        let nx1 = if nx1_raw < x1 { nx1_raw } else { x1 };
        let ny1 = if ny1_raw < y1 { ny1_raw } else { y1 };
        if nx1 > nx0 && ny1 > ny0 && nx1 <= u16::MAX as u32 && ny1 <= u16::MAX as u32 {
            Some((
                nx0 as u16,
                ny0 as u16,
                (nx1 - nx0) as u16,
                (ny1 - ny0) as u16,
            ))
        } else {
            None
        }
    }
    let bounds = (dx, dy, x1, y1);
    if !quads.is_empty() {
        quads.retain_mut(|q| {
            let Some((x, y, w, h)) = clip_rect(
                (
                    u32::from(q.x),
                    u32::from(q.y),
                    u32::from(q.w),
                    u32::from(q.h),
                ),
                bounds,
            ) else {
                return false;
            };
            if (w, h) != (q.w, q.h) {
                return false;
            }
            (q.x, q.y) = (x, y);
            q.row = q.row.saturating_add(row_off);
            true
        });
    }
    if !add.is_empty() {
        add.retain_mut(|q| {
            let Some((x, y, w, h)) = clip_rect(
                (
                    u32::from(q.x),
                    u32::from(q.y),
                    u32::from(q.w),
                    u32::from(q.h),
                ),
                bounds,
            ) else {
                return false;
            };
            (q.x, q.y, q.w, q.h) = (x, y, w, h);
            q.row = q.row.saturating_add(row_off);
            q.cx = (u32::from(q.cx) + dx).min(u32::from(u16::MAX)) as u16;
            q.cy = (u32::from(q.cy) + dy).min(u32::from(u16::MAX)) as u16;
            true
        });
    }
}

/// Where one pane sits inside the composed window grid.
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
    /// The pane's grid-interior pixel box `(x0, y0, x1, y1)`. An edge on the window's own edge
    /// stays open (a sprite may overhang the window pad); every interior edge is a divider and
    /// clips hard.
    #[inline]
    const fn px_box(self) -> (i32, i32, i32, i32) {
        let (cw, ch) = (self.cell_w as i32, self.cell_h as i32);
        let open_lo = i32::MIN / 2;
        let open_hi = i32::MAX / 2;
        let x0 = if self.col_off == 0 {
            open_lo
        } else {
            self.col_off as i32 * cw
        };
        let y0 = if self.row_off == 0 {
            open_lo
        } else {
            self.row_off as i32 * ch
        };
        let x1 = if self.col_off.saturating_add(self.cols) >= self.win_cols {
            open_hi
        } else {
            (self.col_off as i32 + self.cols as i32) * cw
        };
        let y1 = if self.row_off.saturating_add(self.rows) >= self.win_rows {
            open_hi
        } else {
            (self.row_off as i32 + self.rows as i32) * ch
        };
        (x0, y0, x1, y1)
    }

    /// Grid-interior pixel shift from pane-local to window-content coords.
    #[inline]
    const fn px_shift(self) -> (i32, i32) {
        (
            self.col_off as i32 * self.cell_w as i32,
            self.row_off as i32 * self.cell_h as i32,
        )
    }
}

/// Move one pane's sparkle-word decorations into the composed window grid.
#[inline]
pub fn translate_decos_into_pane(decos: &mut Vec<aterm_render::WordDecoration>, place: PanePlace) {
    if decos.is_empty() {
        return;
    }
    decos.retain_mut(|d| {
        if d.row >= place.rows || d.col >= place.cols {
            return false;
        }
        d.row += place.row_off;
        d.col += place.col_off;
        true
    });
}

/// Move one pane's animated-ink fg overrides into the composed window grid.
///
/// The caller must re-sort the merged stream by `(row, col)`.
#[inline]
pub fn translate_ink_into_pane(ink: &mut Vec<aterm_render::InkCell>, place: PanePlace) {
    if ink.is_empty() {
        return;
    }
    ink.retain_mut(|c| {
        if c.row >= place.rows || c.col >= place.cols {
            return false;
        }
        c.row += place.row_off;
        c.col += place.col_off;
        true
    });
}

/// Move one pane's companion sprites into the composed window grid and clip them to [`PanePlace`].
#[inline]
pub fn translate_free_into_pane(
    free: &mut Vec<aterm_render::sprites::FreeSprite>,
    place: PanePlace,
) {
    if free.is_empty() {
        return;
    }
    let px_box = place.px_box();
    let (dx, dy) = place.px_shift();
    free.retain_mut(|s| {
        s.x = s.x.saturating_add(dx);
        s.y = s.y.saturating_add(dy);
        s.clip = s.clip.translate(dx, dy);
        crop_free_sprite(s, px_box)
    });
}

/// Clip one companion sprite to the pixel box `(x0, y0, x1, y1)`: whether any
/// of it is left to draw. The art and its destination stay whole, so the
/// surviving pixels sample exactly the texels they sampled uncut.
#[inline]
pub fn crop_free_sprite(
    s: &mut aterm_render::sprites::FreeSprite,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
) -> bool {
    s.clip = s
        .clip
        .intersect(aterm_core::render::PxRect::new(x0, y0, x1, y1));
    !s.visible().is_empty()
}

/// Move one pane's supernova additive light into the composed window grid and clip to the pane box.
#[inline]
pub fn translate_nova_into_pane(nova: &mut Vec<aterm_render::GlowQuad>, place: PanePlace) {
    if nova.is_empty() {
        return;
    }
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
    use aterm_core::render::{FxTexels, PxRect};

    use super::{PanePlace, translate_free_into_pane};
    use aterm_render::sprites::FreeSprite;

    fn sprite(x: i32, w: u16, flip_x: bool) -> FreeSprite {
        let art = aterm_render::fx::bitmap(FxTexels::Rgba8Straight, 8, 4, vec![9; 8 * 4 * 4])
            .expect("art");
        FreeSprite {
            w,
            flip_x,
            ..FreeSprite::whole(x, 0, art)
        }
    }

    const fn place(cols: u16) -> PanePlace {
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

    #[test]
    fn a_pane_clips_a_sprite_and_keeps_its_art_and_box_whole() {
        for flip in [false, true] {
            let full = sprite(-2, 8, flip);
            let mut v = vec![full.clone()];
            translate_free_into_pane(&mut v, place(6));
            let s = &v[0];
            assert_eq!(
                (s.x, s.y, s.w, s.h),
                (2, 1, 8, 4),
                "flip_x={flip}: shifted, not shrunk"
            );
            assert_eq!(s.art.id(), full.art.id());
            assert_eq!(
                s.visible(),
                PxRect::new(4, 1, 10, 5),
                "flip_x={flip}: cut at the pane's interior edges, so the surviving pixels sample the texels they sampled uncut"
            );
        }
        let mut gone = vec![sprite(-20, 8, false)];
        translate_free_into_pane(&mut gone, place(6));
        assert!(
            gone.is_empty(),
            "a sprite wholly outside its pane is dropped"
        );
    }
}
