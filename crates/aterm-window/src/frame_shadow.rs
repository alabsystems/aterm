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

pub(crate) const SHADOW_SIZE: u32 = 43;
pub(crate) const VISIBLE_BORDER_SIZE: u32 = 1;
pub(crate) const BORDER_SIZE: u32 = SHADOW_SIZE + VISIBLE_BORDER_SIZE;
pub(crate) const CORNER_RADIUS: u32 = 10;
pub(crate) const RESIZE_HANDLE_SIZE: u32 = 12;
pub(crate) const RESIZE_HANDLE_CORNER_SIZE: u32 = 24;

const SHADOW_PARAMS_ACTIVE: (f32, f32, f32) = (0.206_505_5, 0.104_617_53, -0.000_542_446_2);
const SHADOW_PARAMS_INACTIVE: (f32, f32, f32) = (0.168_297_29, 0.204_299_8, 0.001_769_798_6);

const BORDER_LIGHT: [u8; 4] = [220, 220, 220, 255];
const BORDER_DARK: [u8; 4] = [58, 58, 58, 255];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Part {
    Top,
    Left,
    Right,
    Bottom,
}

impl Part {
    pub(crate) const ALL: [Part; 4] = [Part::Top, Part::Left, Part::Right, Part::Bottom];

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[inline]
const fn signed(v: u32) -> i32 {
    if v > i32::MAX as u32 {
        i32::MAX
    } else {
        v as i32
    }
}

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

    pub(crate) fn argb8888(&self) -> Vec<u8> {
        self.pixels.iter().flat_map(|&[r, g, b, a]| [b, g, r, a]).collect()
    }

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

#[inline]
fn shadow_pixel(dist: f32, scale: u32, active: bool) -> [u8; 4] {
    let alpha = (shadow(dist, scale, active) * f32::from(u8::MAX)).round() as u8;
    [0, 0, 0, alpha]
}

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

#[inline]
pub(crate) const fn buffer_size(part: Part, width: u32, height: u32, scale: u32) -> (usize, usize) {
    let rect = surface_rect(part, width, height);
    ((rect.width * scale) as usize, (rect.height * scale) as usize)
}

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

#[derive(Debug, Default)]
pub(crate) struct Shadows {
    rendered: BTreeMap<(u32, bool), RenderedShadow>,
}

impl Shadows {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Look {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) scale: u32,
    pub(crate) active: bool,
    pub(crate) dark: bool,
    pub(crate) tiled: bool,
}

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
#[path = "frame_shadow_tests.rs"]
mod tests;
