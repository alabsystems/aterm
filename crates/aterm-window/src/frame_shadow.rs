// Copyright (c) 2022 Bartłomiej Maryńczak
// SPDX-License-Identifier: MIT
//
// Derived from sctk-adwaita 0.10.1 src/shadow.rs (the shadow, and its rendering kept by scale
// and focus), src/theme.rs (the frame's sizes
// and its border colours), src/parts.rs (where the border parts sit and their input regions)
// and src/lib.rs (`precise_location`, and the visible border's rectangles in `redraw_inner`),
// and modified by the aterm project in 2026. See ../LICENSE-MIT-sctk-adwaita for the upstream
// MIT license.

//! The shadow margin and visible border outside a client-decorated Wayland window.

use std::collections::BTreeMap;

#[cfg(not(macos_platform))]
use crate::window::ResizeDirection;

/// The shadow's depth beyond the visible border, logical px.
pub(crate) const SHADOW_SIZE: u32 = 43;
/// The visible border's width, logical px.
pub(crate) const VISIBLE_BORDER_SIZE: u32 = 1;
/// A border part's depth: the visible border and the shadow.
pub(crate) const BORDER_SIZE: u32 = SHADOW_SIZE + VISIBLE_BORDER_SIZE;
/// The radius of the corners the shadow is rounded around.
pub(crate) const CORNER_RADIUS: u32 = 10;
/// How far outside the window a part takes the pointer.
pub(crate) const RESIZE_HANDLE_SIZE: u32 = 12;
/// How far along an edge from a corner a press resizes by the corner.
pub(crate) const RESIZE_HANDLE_CORNER_SIZE: u32 = 24;

const SHADOW_PARAMS_ACTIVE: (f32, f32, f32) = (0.206_505_5, 0.104_617_53, -0.000_542_446_2);
const SHADOW_PARAMS_INACTIVE: (f32, f32, f32) = (0.168_297_29, 0.204_299_8, 0.001_769_798_6);

/// The visible border's colour (opaque, so premultiplied as it is): sctk-adwaita's light
/// theme's and its dark theme's.
const BORDER_LIGHT: [u8; 4] = [220, 220, 220, 255];
const BORDER_DARK: [u8; 4] = [58, 58, 58, 255];

/// One of the four border parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Part {
    Top,
    Left,
    Right,
    Bottom,
}

impl Part {
    /// Every part, in sctk-adwaita's order.
    pub(crate) const ALL: [Part; 4] = [Part::Top, Part::Left, Part::Right, Part::Bottom];

    /// Its position in [`Part::ALL`].
    #[inline]
    pub(crate) const fn index(self) -> usize {
        match self {
            Part::Top => 0,
            Part::Left => 1,
            Part::Right => 2,
            Part::Bottom => 3,
        }
    }
}

/// A rectangle in logical px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// `v` as an `i32`, saturating (a window is never that large).
#[inline]
const fn signed(v: u32) -> i32 {
    if v > i32::MAX as u32 {
        i32::MAX
    } else {
        v as i32
    }
}

/// Where `part` sits beside a `width`×`height` window, relative to the window's surface
/// (sctk-adwaita's `DecorationParts::new` and `resize`, without a header of its own).
#[inline]
pub(crate) const fn surface_rect(part: Part, width: u32, height: u32) -> Rect {
    let b = BORDER_SIZE;
    let across = width.saturating_add(2 * b);
    match part {
        Part::Top => Rect { x: -signed(b), y: -signed(b), width: across, height: b },
        Part::Left => Rect { x: -signed(b), y: 0, width: b, height },
        Part::Right => Rect { x: signed(width), y: 0, width: b, height },
        Part::Bottom => Rect { x: -signed(b), y: signed(height), width: across, height: b },
    }
}

/// The part of `part` that takes the pointer, relative to the part: the [`RESIZE_HANDLE_SIZE`]
/// px against the window, the top and bottom ones reaching as far past the sides.
#[inline]
pub(crate) const fn input_rect(part: Part, width: u32, height: u32) -> Rect {
    let (b, r) = (BORDER_SIZE, RESIZE_HANDLE_SIZE);
    let inset = signed(b - r);
    let across = width.saturating_add(2 * r);
    match part {
        Part::Top => Rect { x: inset, y: inset, width: across, height: r },
        Part::Left => Rect { x: inset, y: 0, width: r, height },
        Part::Right => Rect { x: 0, y: 0, width: r, height },
        Part::Bottom => Rect { x: inset, y: 0, width: across, height: r },
    }
}

/// Which way a press at (`x`, `y`) on `part` (logical px, relative to the part) resizes a
/// `width`×`height` window: by a corner within [`RESIZE_HANDLE_CORNER_SIZE`] px of it along the
/// edge, else by the edge (sctk-adwaita's `precise_location`).
#[cfg(not(macos_platform))]
#[inline]
pub(crate) fn direction(part: Part, x: f64, y: f64, width: u32, height: u32) -> ResizeDirection {
    let left_corner_x = f64::from(BORDER_SIZE + RESIZE_HANDLE_CORNER_SIZE);
    let right_corner_x = f64::from((width + BORDER_SIZE).saturating_sub(RESIZE_HANDLE_CORNER_SIZE));
    let top_corner_y = f64::from(RESIZE_HANDLE_CORNER_SIZE);
    let bottom_corner_y = f64::from(height.saturating_sub(RESIZE_HANDLE_CORNER_SIZE));
    match part {
        Part::Top if x <= left_corner_x => ResizeDirection::NorthWest,
        Part::Top if x >= right_corner_x => ResizeDirection::NorthEast,
        Part::Top => ResizeDirection::North,
        Part::Bottom if x <= left_corner_x => ResizeDirection::SouthWest,
        Part::Bottom if x >= right_corner_x => ResizeDirection::SouthEast,
        Part::Bottom => ResizeDirection::South,
        Part::Left if y <= top_corner_y => ResizeDirection::NorthWest,
        Part::Left if y >= bottom_corner_y => ResizeDirection::SouthWest,
        Part::Left => ResizeDirection::West,
        Part::Right if y <= top_corner_y => ResizeDirection::NorthEast,
        Part::Right if y >= bottom_corner_y => ResizeDirection::SouthEast,
        Part::Right => ResizeDirection::East,
    }
}

/// A part's pixels, row by row: premultiplied RGBA, as tiny-skia lays out the pixmap
/// sctk-adwaita painted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pixmap {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) pixels: Vec<[u8; 4]>,
}

impl Pixmap {
    fn new(width: usize, height: usize) -> Pixmap {
        Pixmap { width, height, pixels: vec![[0; 4]; width * height] }
    }

    /// The bytes of a `wl_shm` ARGB8888 buffer: each pixel little-endian `0xAARRGGBB`.
    pub(crate) fn argb8888(&self) -> Vec<u8> {
        self.pixels.iter().flat_map(|&[r, g, b, a]| [b, g, r, a]).collect()
    }

    /// Fill `[x0, x1)`×`[y0, y1)` (clipped) with `color`.
    fn fill(&mut self, (x0, y0, x1, y1): (usize, usize, usize, usize), color: [u8; 4]) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        for y in y0.min(y1)..y1 {
            self.pixels[y * self.width + x0.min(x1)..y * self.width + x1].fill(color);
        }
    }
}

#[inline]
fn shadow(pixel_dist: f32, scale: u32, active: bool) -> f32 {
    let (a, b, c) = if active { SHADOW_PARAMS_ACTIVE } else { SHADOW_PARAMS_INACTIVE };

    a * (-b * (pixel_dist / scale as f32)).exp() + c
}

/// The shadow's alpha at `dist` px from the window, as a premultiplied black pixel.
#[inline]
fn shadow_pixel(dist: f32, scale: u32, active: bool) -> [u8; 4] {
    let alpha = (shadow(dist, scale, active) * f32::from(u8::MAX)).round() as u8;
    [0, 0, 0, alpha]
}

/// A tiny-skia `Point::distance` (its `length`: f32, widened only where the square overflows).
#[inline]
fn distance(x: f32, y: f32) -> f32 {
    let mag2 = x * x + y * y;
    if mag2.is_finite() {
        mag2.sqrt()
    } else {
        let (xx, yy) = (f64::from(x), f64::from(y));
        (xx * xx + yy * yy).sqrt() as f32
    }
}

/// One scale's and focus's shadow: a run of it outward from an edge, and the square of its
/// four rounded corners.
#[derive(Debug)]
struct RenderedShadow {
    side: Vec<[u8; 4]>,
    edges: Pixmap,
}

impl RenderedShadow {
    fn new(scale: u32, active: bool) -> RenderedShadow {
        let shadow_size = SHADOW_SIZE * scale;
        let corner_radius = CORNER_RADIUS * scale;

        let side = (0..shadow_size).map(|x| shadow_pixel(x as f32 + 0.5, scale, active)).collect();

        let edges_size = ((corner_radius + shadow_size) * 2) as usize;
        let mut edges = Pixmap::new(edges_size, edges_size);
        let middle = edges_size as f32 / 2.0;
        for y in 0..edges_size {
            let y_pos = y as f32 + 0.5;
            for x in 0..edges_size {
                let dist =
                    distance(middle - (x as f32 + 0.5), middle - y_pos) - corner_radius as f32;
                edges.pixels[y * edges_size + x] = shadow_pixel(dist, scale, active);
            }
        }

        RenderedShadow { side, edges }
    }

    fn side_draw(
        &self,
        flipped: bool,
        rotated: bool,
        stack: usize,
        dst: &mut Pixmap,
        dst_left: usize,
        dst_top: usize,
    ) {
        fn iter_copy<'a>(
            src: impl Iterator<Item = &'a [u8; 4]>,
            dst: impl Iterator<Item = &'a mut [u8; 4]>,
        ) {
            src.zip(dst).for_each(|(src, dst)| *dst = *src)
        }

        let dst_width = dst.width;
        let dst_pixels = &mut dst.pixels;
        match (flipped, rotated) {
            (false, false) => (0..stack).for_each(|i| {
                let dst = dst_pixels.iter_mut().skip((dst_top + i) * dst_width + dst_left);
                iter_copy(self.side.iter(), dst);
            }),
            (false, true) => (0..stack).for_each(|i| {
                let dst = dst_pixels
                    .iter_mut()
                    .skip(dst_top * dst_width + dst_left + i)
                    .step_by(dst_width);
                iter_copy(self.side.iter(), dst);
            }),
            (true, false) => (0..stack).for_each(|i| {
                let dst = dst_pixels.iter_mut().skip((dst_top + i) * dst_width + dst_left);
                iter_copy(self.side.iter().rev(), dst);
            }),
            (true, true) => (0..stack).for_each(|i| {
                let dst = dst_pixels
                    .iter_mut()
                    .skip(dst_top * dst_width + dst_left + i)
                    .step_by(dst_width);
                iter_copy(self.side.iter().rev(), dst);
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn edges_draw(
        &self,
        src_x_offset: isize,
        src_y_offset: isize,
        dst: &mut Pixmap,
        dst_rect_left: usize,
        dst_rect_top: usize,
        dst_rect_width: usize,
        dst_rect_height: usize,
    ) {
        let src_width = self.edges.width;
        let src_pixels = &self.edges.pixels;
        let dst_width = dst.width;
        let dst_pixels = &mut dst.pixels;
        for y in 0..dst_rect_height {
            let dst_y = dst_rect_top + y;
            let src_y = y as isize + src_y_offset;
            if src_y < 0 {
                continue;
            }

            let src_y = src_y as usize;
            for x in 0..dst_rect_width {
                let dst_x = dst_rect_left + x;
                let src_x = x as isize + src_x_offset;
                if src_x < 0 {
                    continue;
                }

                let src = src_pixels.get(src_y * src_width + src_x as usize);
                let dst = dst_pixels.get_mut(dst_y * dst_width + dst_x);
                if let (Some(src), Some(dst)) = (src, dst) {
                    *dst = *src;
                }
            }
        }
    }

    fn draw(&self, dst: &mut Pixmap, scale: u32, part: Part) {
        let shadow_size = (SHADOW_SIZE * scale) as usize;
        let visible_border_size = (VISIBLE_BORDER_SIZE * scale) as usize;
        let corner_radius = (CORNER_RADIUS * scale) as usize;

        let dst_width = dst.width;
        let dst_height = dst.height;
        let edges_half = self.edges.width / 2;
        match part {
            Part::Top => {
                let left_edge_width = edges_half;
                let right_edge_width = edges_half;
                let side_width =
                    dst_width.saturating_sub(left_edge_width).saturating_sub(right_edge_width);

                self.edges_draw(
                    0,
                    -(visible_border_size as isize),
                    dst,
                    0,
                    0,
                    left_edge_width,
                    dst_height,
                );

                self.side_draw(true, true, side_width, dst, left_edge_width, visible_border_size);

                self.edges_draw(
                    edges_half as isize,
                    -(visible_border_size as isize),
                    dst,
                    left_edge_width + side_width,
                    0,
                    right_edge_width,
                    dst_height,
                );
            },
            Part::Left => {
                let top_edge_height = corner_radius;
                let bottom_edge_height = corner_radius - visible_border_size;
                let side_height =
                    dst_height.saturating_sub(top_edge_height).saturating_sub(bottom_edge_height);

                self.edges_draw(
                    0,
                    shadow_size as isize,
                    dst,
                    0,
                    0,
                    dst_width.saturating_sub(visible_border_size),
                    top_edge_height,
                );

                self.side_draw(true, false, side_height, dst, 0, top_edge_height);

                self.edges_draw(
                    0,
                    edges_half as isize,
                    dst,
                    0,
                    top_edge_height + side_height,
                    dst_width.saturating_sub(visible_border_size),
                    bottom_edge_height,
                );
            },
            Part::Right => {
                let top_edge_height = corner_radius;
                let bottom_edge_height = corner_radius - visible_border_size;
                let side_height =
                    dst_height.saturating_sub(top_edge_height).saturating_sub(bottom_edge_height);

                self.edges_draw(
                    edges_half as isize + corner_radius as isize,
                    shadow_size as isize,
                    dst,
                    visible_border_size,
                    0,
                    dst_width.saturating_sub(visible_border_size),
                    top_edge_height,
                );

                self.side_draw(
                    false,
                    false,
                    side_height,
                    dst,
                    visible_border_size,
                    top_edge_height,
                );

                self.edges_draw(
                    edges_half as isize + corner_radius as isize,
                    edges_half as isize,
                    dst,
                    visible_border_size,
                    top_edge_height + side_height,
                    dst_width.saturating_sub(visible_border_size),
                    bottom_edge_height,
                );
            },
            Part::Bottom => {
                let left_edge_width = edges_half;
                let right_edge_width = edges_half;
                let side_width =
                    dst_width.saturating_sub(left_edge_width).saturating_sub(right_edge_width);

                self.edges_draw(
                    0,
                    edges_half as isize + (corner_radius - visible_border_size) as isize,
                    dst,
                    0,
                    0,
                    left_edge_width,
                    dst_height,
                );

                self.side_draw(false, true, side_width, dst, left_edge_width, visible_border_size);

                self.edges_draw(
                    edges_half as isize,
                    edges_half as isize + (corner_radius - visible_border_size) as isize,
                    dst,
                    left_edge_width + side_width,
                    0,
                    right_edge_width,
                    dst_height,
                );
            },
        }
    }
}

/// `part`'s buffer size in px beside a `width`×`height` window at integer `scale`.
#[inline]
pub(crate) const fn buffer_size(part: Part, width: u32, height: u32, scale: u32) -> (usize, usize) {
    let rect = surface_rect(part, width, height);
    ((rect.width * scale) as usize, (rect.height * scale) as usize)
}

/// The shadow sctk-adwaita's `Shadow::draw` paints into `part`'s buffer beside a
/// `width`×`height` window at `scale`.
#[cfg(test)]
pub(crate) fn shadow_plane(
    part: Part,
    width: u32,
    height: u32,
    scale: u32,
    active: bool,
) -> Pixmap {
    let (w, h) = buffer_size(part, width, height, scale);
    let mut pixmap = Pixmap::new(w, h);
    RenderedShadow::new(scale, active).draw(&mut pixmap, scale, part);
    pixmap
}

/// The shadows a frame has rendered, keyed by scale and focus for reuse across redraws.
#[derive(Debug, Default)]
pub(crate) struct Shadows {
    rendered: BTreeMap<(u32, bool), RenderedShadow>,
}

impl Shadows {
    /// `part`'s buffer as the frame shows it: the shadow (none while tiled), then the visible
    /// border over it.
    pub(crate) fn paint(&mut self, part: Part, look: Look) -> Pixmap {
        let (w, h) = buffer_size(part, look.width, look.height, look.scale);
        let mut pixmap = Pixmap::new(w, h);
        if !look.tiled {
            self.rendered
                .entry((look.scale, look.active))
                .or_insert_with(|| RenderedShadow::new(look.scale, look.active))
                .draw(&mut pixmap, look.scale, part);
        }
        let color = if look.dark { BORDER_DARK } else { BORDER_LIGHT };
        pixmap.fill(border_span(part, look.width, look.height, look.scale), color);
        pixmap
    }
}

/// How a frame looks: what a redraw of the parts depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Look {
    /// The window's size, logical px.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The buffers' integer scale (the window's scale, rounded up, as sctk-adwaita's).
    pub(crate) scale: u32,
    /// Focused: the darker, wider shadow.
    pub(crate) active: bool,
    /// The dark theme's border colour.
    pub(crate) dark: bool,
    /// Tiled against another window or a screen edge: the border only, no shadow.
    pub(crate) tiled: bool,
}

/// `[x0, y0, x1, y1)` of the visible border on `part`'s buffer, px: the line against the
/// window, reaching across the corners on the top and bottom parts.
#[inline]
pub(crate) const fn border_span(
    part: Part,
    width: u32,
    height: u32,
    scale: u32,
) -> (usize, usize, usize, usize) {
    let (b, vb) = ((BORDER_SIZE * scale) as usize, (VISIBLE_BORDER_SIZE * scale) as usize);
    let (w, h) = ((width * scale) as usize, (height * scale) as usize);
    match part {
        Part::Top => (b - vb, b - vb, b + w + vb, b),
        Part::Left => (b - vb, 0, b, h),
        Part::Right => (0, 0, vb, h),
        Part::Bottom => (b - vb, 0, b + w + vb, vb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A part painted afresh, nothing kept.
    fn paint(part: Part, look: Look) -> Pixmap {
        Shadows::default().paint(part, look)
    }

    /// Verifies that a cached shadow rendering paints the same pixels as a fresh rendering.
    #[test]
    fn a_kept_rendering_paints_what_a_fresh_one_does() {
        let mut shadows = Shadows::default();
        let base =
            Look { width: 744, height: 528, scale: 1, active: true, dark: false, tiled: false };
        let looks = [
            base,
            Look { width: 900, height: 600, ..base },
            Look { scale: 2, ..base },
            Look { active: false, ..base },
            Look { active: true, dark: true, ..base },
            Look { scale: 2, active: false, ..base },
            Look { tiled: true, ..base },
            base,
        ];
        for look in looks {
            for part in Part::ALL {
                let kept = shadows.paint(part, look);
                assert_eq!(kept.pixels, paint(part, look).pixels, "{part:?} {look:?}");
                if !look.tiled {
                    let mut plane =
                        shadow_plane(part, look.width, look.height, look.scale, look.active);
                    let color = if look.dark { BORDER_DARK } else { BORDER_LIGHT };
                    plane.fill(border_span(part, look.width, look.height, look.scale), color);
                    assert_eq!(kept.pixels, plane.pixels, "{part:?} {look:?}");
                }
            }
        }
        assert_eq!(shadows.rendered.len(), 4, "one rendering a scale and focus");
    }

    /// sctk-adwaita 0.10.1's own `Shadow::draw`, recorded (`record planes`).
    const RECORDED: &str = include_str!("../tests/recorded/sctk-adwaita-0.10.1-shadow.tsv");

    fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
        bytes.fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    }

    fn field<'a>(row: &[&'a str], name: &str) -> &'a str {
        row.iter()
            .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("no {name} in {row:?}"))
    }

    fn pair(v: &str) -> (u32, u32) {
        let (a, b) = v.split_once('x').expect("WxH");
        (a.parse().expect("a width"), b.parse().expect("a height"))
    }

    fn part_named(name: &str) -> Part {
        match name {
            "top" => Part::Top,
            "left" => Part::Left,
            "right" => Part::Right,
            "bottom" => Part::Bottom,
            other => panic!("no part {other}"),
        }
    }

    /// Every recorded plane, drawn here: the same buffer size, alpha and bytes.
    #[test]
    fn the_shadow_is_sctk_adwaita_s_to_the_byte() {
        let mut rows = 0;
        for line in RECORDED.lines().filter(|l| l.starts_with("plane\t")) {
            let row: Vec<&str> = line.split('\t').collect();
            let part = part_named(field(&row, "part"));
            let (w, h) = pair(field(&row, "window"));
            let scale: u32 = field(&row, "scale").parse().expect("a scale");
            let active = field(&row, "active") == "1";
            let plane = shadow_plane(part, w, h, scale, active);
            let (bw, bh) = pair(field(&row, "buffer"));
            assert_eq!((plane.width, plane.height), (bw as usize, bh as usize), "{line}");
            let alpha: u64 = plane.pixels.iter().map(|p| u64::from(p[3])).sum();
            assert_eq!(alpha.to_string(), field(&row, "alpha_sum"), "{line}");
            let hash = fnv(plane.pixels.iter().flatten().copied());
            assert_eq!(format!("{hash:016x}"), field(&row, "fnv"), "{line}");
            rows += 1;
        }
        assert_eq!(rows, 120, "every recorded plane was drawn");
    }

    /// NEGATIVE CONTROL for the oracle: the inactive curve, or a corner radius off by one,
    /// draws other bytes; the hash tells them apart.
    #[test]
    fn a_shadow_off_its_curve_is_not_the_recorded_one() {
        let active = shadow_plane(Part::Right, 744, 528, 1, true);
        let inactive = shadow_plane(Part::Right, 744, 528, 1, false);
        assert_ne!(active, inactive);
        let row = &active.pixels[active.width * (active.height / 2)..][..active.width];
        let alpha: Vec<u8> = row.iter().map(|p| p[3]).collect();
        assert_eq!(alpha, [
            0, 50, 45, 40, 36, 33, 29, 27, 24, 22, 19, 17, 16, 14, 13, 11, 10, 9, 8, 7, 7, 6, 5, 5,
            4, 4, 4, 3, 3, 3, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0
        ]);
    }

    #[test]
    fn the_parts_sit_around_the_window_and_take_the_pointer_next_to_it() {
        let (w, h) = (744, 528);
        assert_eq!(surface_rect(Part::Top, w, h), Rect { x: -44, y: -44, width: 832, height: 44 });
        assert_eq!(surface_rect(Part::Left, w, h), Rect { x: -44, y: 0, width: 44, height: 528 });
        assert_eq!(surface_rect(Part::Right, w, h), Rect { x: 744, y: 0, width: 44, height: 528 });
        assert_eq!(surface_rect(Part::Bottom, w, h), Rect {
            x: -44,
            y: 528,
            width: 832,
            height: 44
        });
        assert_eq!(input_rect(Part::Top, w, h), Rect { x: 32, y: 32, width: 768, height: 12 });
        assert_eq!(input_rect(Part::Left, w, h), Rect { x: 32, y: 0, width: 12, height: 528 });
        assert_eq!(input_rect(Part::Right, w, h), Rect { x: 0, y: 0, width: 12, height: 528 });
        assert_eq!(input_rect(Part::Bottom, w, h), Rect { x: 32, y: 0, width: 768, height: 12 });
        for part in Part::ALL {
            assert_eq!(Part::ALL[part.index()], part);
        }
    }

    #[cfg(not(macos_platform))]
    #[test]
    fn a_press_resizes_by_the_edge_and_by_the_corner_near_it() {
        use ResizeDirection::*;
        let (w, h) = (744, 528);
        assert_eq!(direction(Part::Right, 6.0, 264.0, w, h), East);
        assert_eq!(direction(Part::Right, 6.0, 24.0, w, h), NorthEast);
        assert_eq!(direction(Part::Right, 6.0, 25.0, w, h), East);
        assert_eq!(direction(Part::Right, 6.0, 504.0, w, h), SouthEast);
        assert_eq!(direction(Part::Left, 38.0, 3.0, w, h), NorthWest);
        assert_eq!(direction(Part::Left, 38.0, 527.0, w, h), SouthWest);
        assert_eq!(direction(Part::Left, 38.0, 300.0, w, h), West);
        assert_eq!(direction(Part::Top, 68.0, 38.0, w, h), NorthWest);
        assert_eq!(direction(Part::Top, 69.0, 38.0, w, h), North);
        assert_eq!(direction(Part::Top, 764.0, 38.0, w, h), NorthEast);
        assert_eq!(direction(Part::Bottom, 400.0, 3.0, w, h), South);
        assert_eq!(direction(Part::Bottom, 10.0, 3.0, w, h), SouthWest);
        assert_eq!(direction(Part::Bottom, 800.0, 3.0, w, h), SouthEast);
    }

    #[test]
    fn the_border_runs_around_the_window_in_the_theme_s_colour() {
        let look =
            Look { width: 100, height: 50, scale: 2, active: true, dark: true, tiled: false };
        let top = paint(Part::Top, look);
        let at = |p: &Pixmap, x: usize, y: usize| p.pixels[y * p.width + x];
        assert_eq!(at(&top, 86, 87), BORDER_DARK);
        assert_eq!(at(&top, 86, 86), BORDER_DARK);
        assert_ne!(at(&top, 85, 87), BORDER_DARK, "the shadow beyond the border's end");
        assert_eq!(at(&top, 88 + 200 + 1, 87), BORDER_DARK);
        assert_ne!(at(&top, 88 + 200 + 2, 87), BORDER_DARK);
        assert_ne!(at(&top, 100, 85), BORDER_DARK, "the shadow above the border");
        let right = paint(Part::Right, Look { dark: false, ..look });
        assert_eq!(at(&right, 0, 50), BORDER_LIGHT);
        assert_eq!(at(&right, 1, 99), BORDER_LIGHT);
        assert_ne!(at(&right, 2, 50), BORDER_LIGHT);
        let tiled = paint(Part::Bottom, Look { tiled: true, ..look });
        assert_eq!(at(&tiled, 100, 0), BORDER_DARK);
        assert!(tiled.pixels[2 * tiled.width..].iter().all(|p| *p == [0; 4]));
    }

    #[test]
    fn a_buffer_is_argb8888_little_endian() {
        let pixmap = Pixmap { width: 1, height: 1, pixels: vec![[1, 2, 3, 4]] };
        assert_eq!(pixmap.argb8888(), [3, 2, 1, 4]);
    }

    #[test]
    fn a_window_smaller_than_the_corners_draws_without_a_panic() {
        for part in Part::ALL {
            for (w, h) in [(1, 1), (2, 1), (30, 20)] {
                let look = Look {
                    width: w,
                    height: h,
                    scale: 3,
                    active: false,
                    dark: false,
                    tiled: false,
                };
                let pixmap = paint(part, look);
                assert_eq!((pixmap.width, pixmap.height), buffer_size(part, w, h, 3));
            }
        }
    }
}
