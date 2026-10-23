// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/outline.rs and modified by
// the aterm project in 2026: filled from aterm's outline sources, the
// ahead-of-time (ttfautohint) corner rule and the advance dropped.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Outline representation and helpers for autohinting.

use super::{
    DrawError,
    fixed::{F2Dot14, F26Dot6},
    font::{HintFont, OutlineGlyph},
    glyf::{PointFlags, PointMarker},
    metrics::Scale,
    path,
    unscaled::{UnscaledOutlineSink, UnscaledPoint},
};
use crate::OutlineSink;
use core::ops::Range;

/// Hinting directions.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
#[repr(i8)]
pub enum Direction {
    /// Undetermined direction.
    #[default]
    None = 4,
    /// Toward the right.
    Right = 1,
    /// Toward the left.
    Left = -1,
    /// Toward the top.
    Up = 2,
    /// Toward the bottom.
    Down = -2,
}

impl Direction {
    /// Computes a direction from a vector.
    pub fn new(dx: i32, dy: i32) -> Self {
        let (dir, long_arm, short_arm) = if dy >= dx {
            if dy >= -dx {
                (Direction::Up, dy, dx)
            } else {
                (Direction::Left, -dx, dy)
            }
        } else if dy >= -dx {
            (Direction::Right, dx, dy)
        } else {
            (Direction::Down, -dy, dx)
        };
        if long_arm <= 14 * short_arm.abs() {
            Direction::None
        } else {
            dir
        }
    }

    pub fn is_opposite(self, other: Self) -> bool {
        self as i8 + other as i8 == 0
    }

    pub fn is_same_axis(self, other: Self) -> bool {
        (self as i8).abs() == (other as i8).abs()
    }

    pub(crate) fn normalize(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Down => Self::Up,
            _ => self,
        }
    }
}

/// The overall orientation of an outline.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Orientation {
    Clockwise,
    CounterClockwise,
}

/// Outline point with a lot of context for hinting.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub(crate) struct Point {
    /// Describes the type and hinting state of the point.
    pub flags: PointFlags,
    /// X coordinate in font units.
    pub fx: i32,
    /// Y coordinate in font units.
    pub fy: i32,
    /// Scaled X coordinate.
    pub ox: i32,
    /// Scaled Y coordinate.
    pub oy: i32,
    /// Hinted X coordinate.
    pub x: i32,
    /// Hinted Y coordinate.
    pub y: i32,
    /// Direction of inwards vector.
    pub in_dir: Direction,
    /// Direction of outwards vector.
    pub out_dir: Direction,
    /// Context dependent coordinate.
    pub u: i32,
    /// Context dependent coordinate.
    pub v: i32,
    /// Index of next point in contour.
    pub next_ix: u16,
    /// Index of previous point in contour.
    pub prev_ix: u16,
}

impl Point {
    pub fn is_on_curve(&self) -> bool {
        self.flags.is_on_curve()
    }

    /// Returns the index of the next point in the contour.
    pub fn next(&self) -> usize {
        self.next_ix as usize
    }

    /// Returns the index of the previous point in the contour.
    pub fn prev(&self) -> usize {
        self.prev_ix as usize
    }

    #[inline(always)]
    fn as_contour_point(&self) -> path::ContourPoint {
        path::ContourPoint {
            x: F26Dot6::from_bits(self.x),
            y: F26Dot6::from_bits(self.y),
            flags: self.flags,
        }
    }
}

#[derive(Default)]
pub(crate) struct Outline {
    pub units_per_em: i32,
    pub orientation: Option<Orientation>,
    pub points: Vec<Point>,
    pub contours: Vec<Contour>,
}

impl Outline {
    /// Fills the outline from the given glyph.
    pub fn fill(
        &mut self,
        font: &HintFont,
        glyph: &OutlineGlyph,
        coords: &[F2Dot14],
    ) -> Result<(), DrawError> {
        self.clear();
        self.units_per_em = font.units_per_em();
        font.draw_unscaled(glyph, coords, self)?;
        self.analyze_and_validate()
    }

    fn analyze_and_validate(&mut self) -> Result<(), DrawError> {
        const MAX_LEN: usize = u16::MAX as usize + 1;
        if self.points.len() > MAX_LEN || self.contours.len() > MAX_LEN {
            return Err(DrawError::TooManyPoints);
        }
        let near_limit = 20 * self.units_per_em / 2048;
        self.link_points();
        self.mark_near_points(near_limit);
        self.compute_directions(near_limit);
        self.simplify_topology();
        self.check_remaining_weak_points(is_corner_flat);
        self.compute_orientation();
        Ok(())
    }

    /// Applies dimension specific scaling factors and deltas to each point in the outline.
    pub fn scale(&mut self, scale: &Scale) {
        use super::metrics::fixed_mul;
        for point in &mut self.points {
            let x = fixed_mul(point.fx, scale.x_scale) + scale.x_delta;
            let y = fixed_mul(point.fy, scale.y_scale) + scale.y_delta;
            point.ox = x;
            point.x = x;
            point.oy = y;
            point.y = y;
        }
    }

    pub fn clear(&mut self) {
        self.units_per_em = 0;
        self.points.clear();
        self.contours.clear();
    }

    /// The fitted outline as path segments, FreeType style, in pixels.
    pub fn to_path(&self, pen: &mut impl OutlineSink) -> Result<(), DrawError> {
        for contour in &self.contours {
            let Some(points) = self.points.get(contour.range()) else {
                continue;
            };
            if let (Some(first_point), Some(last_point)) = (
                points.first().map(Point::as_contour_point),
                points.last().map(Point::as_contour_point),
            ) {
                path::contour_to_path(
                    points.iter().map(Point::as_contour_point),
                    first_point,
                    last_point,
                    pen,
                )
                .map_err(|()| DrawError::ToPath)?;
            }
        }
        Ok(())
    }
}

impl Outline {
    /// Sets next and previous indices for each point.
    fn link_points(&mut self) {
        let points = self.points.as_mut_slice();
        for contour in &self.contours {
            let Some(points) = points.get_mut(contour.range()) else {
                continue;
            };
            let first_ix = contour.first();
            let mut prev_ix = contour.last() as u16;
            for (ix, point) in points.iter_mut().enumerate() {
                let ix = (ix + first_ix) as u16;
                point.prev_ix = prev_ix;
                prev_ix = ix;
                point.next_ix = ix.wrapping_add(1);
            }
            points.last_mut().unwrap().next_ix = first_ix as u16;
        }
    }

    /// Computes the near flag for each contour.
    fn mark_near_points(&mut self, near_limit: i32) {
        let points = self.points.as_mut_slice();
        for contour in &self.contours {
            let mut prev_ix = contour.last();
            for ix in contour.range() {
                let point = points[ix];
                let prev = &mut points[prev_ix];
                let out_x = point.fx - prev.fx;
                let out_y = point.fy - prev.fy;
                if out_x.abs() + out_y.abs() < near_limit {
                    prev.flags.set_marker(PointMarker::NEAR);
                }
                prev_ix = ix;
            }
        }
    }

    /// Compute directions of in and out vectors.
    fn compute_directions(&mut self, near_limit: i32) {
        let near_limit2 = 2 * near_limit - 1;
        let points = self.points.as_mut_slice();
        for contour in &self.contours {
            let mut first_ix = contour.first();
            let mut ix = first_ix;
            let mut prev_ix = contour.prev(first_ix);
            let mut point = points[first_ix];
            while prev_ix != first_ix {
                let prev = points[prev_ix];
                let out_x = point.fx - prev.fx;
                let out_y = point.fy - prev.fy;
                if out_x.abs() + out_y.abs() >= near_limit2 {
                    break;
                }
                point = prev;
                ix = prev_ix;
                prev_ix = contour.prev(prev_ix);
            }
            first_ix = ix;
            let first = &mut points[first_ix];
            first.u = first_ix as _;
            first.v = first_ix as _;
            let mut next_ix = first_ix;
            let mut ix = first_ix;
            let mut out_x = 0;
            let mut out_y = 0;
            loop {
                let point_ix = next_ix;
                next_ix = contour.next(point_ix);
                let point = points[point_ix];
                let next = &mut points[next_ix];
                out_x += next.fx - point.fx;
                out_y += next.fy - point.fy;
                if out_x.abs() + out_y.abs() < near_limit {
                    next.flags.set_marker(PointMarker::WEAK_INTERPOLATION);
                    if next_ix == first_ix {
                        break;
                    }
                    continue;
                }
                let out_dir = Direction::new(out_x, out_y);
                next.in_dir = out_dir;
                next.v = ix as _;
                let cur = &mut points[ix];
                cur.u = next_ix as _;
                cur.out_dir = out_dir;
                let mut inter_ix = contour.next(ix);
                while inter_ix != next_ix {
                    let point = &mut points[inter_ix];
                    point.in_dir = out_dir;
                    point.out_dir = out_dir;
                    inter_ix = contour.next(inter_ix);
                }
                ix = next_ix;
                points[ix].u = first_ix as _;
                points[first_ix].v = ix as _;
                out_x = 0;
                out_y = 0;
                if next_ix == first_ix {
                    break;
                }
            }
        }
    }

    /// Simplify so that we can identify local extrema more reliably.
    fn simplify_topology(&mut self) {
        let points = self.points.as_mut_slice();
        for i in 0..points.len() {
            let point = points[i];
            if point.flags.has_marker(PointMarker::WEAK_INTERPOLATION) {
                continue;
            }
            if point.in_dir == Direction::None && point.out_dir == Direction::None {
                let u_index = point.u as usize;
                let v_index = point.v as usize;
                let next_u = points[u_index];
                let prev_v = points[v_index];
                let in_x = point.fx - prev_v.fx;
                let in_y = point.fy - prev_v.fy;
                let out_x = next_u.fx - point.fx;
                let out_y = next_u.fy - point.fy;
                if (in_x ^ out_x) >= 0 && (in_y ^ out_y) >= 0 {
                    points[i].flags.set_marker(PointMarker::WEAK_INTERPOLATION);
                    points[v_index].u = u_index as _;
                    points[u_index].v = v_index as _;
                }
            }
        }
    }

    /// Check for remaining weak points.
    fn check_remaining_weak_points(&mut self, is_corner_flat: impl Fn(i32, i32, i32, i32) -> bool) {
        let points = self.points.as_mut_slice();
        for i in 0..points.len() {
            let point = points[i];
            let mut make_weak = false;
            if point.flags.has_marker(PointMarker::WEAK_INTERPOLATION) {
                continue;
            }
            if !point.flags.is_on_curve() {
                make_weak = true;
            } else if point.out_dir == point.in_dir {
                if point.out_dir != Direction::None {
                    make_weak = true;
                } else {
                    let u_index = point.u as usize;
                    let v_index = point.v as usize;
                    let next_u = points[u_index];
                    let prev_v = points[v_index];
                    if is_corner_flat(
                        point.fx - prev_v.fx,
                        point.fy - prev_v.fy,
                        next_u.fx - point.fx,
                        next_u.fy - point.fy,
                    ) {
                        make_weak = true;
                        points[v_index].u = u_index as _;
                        points[u_index].v = v_index as _;
                    }
                }
            } else if point.in_dir.is_opposite(point.out_dir) {
                make_weak = true;
            }
            if make_weak {
                points[i].flags.set_marker(PointMarker::WEAK_INTERPOLATION);
            }
        }
    }

    /// Computes the overall winding order of the outline.
    fn compute_orientation(&mut self) {
        self.orientation = None;
        let points = self.points.as_slice();
        if points.is_empty() {
            return;
        }
        fn point_to_i64(point: &Point) -> (i64, i64) {
            (point.fx as i64, point.fy as i64)
        }
        let mut area = 0i64;
        for contour in &self.contours {
            let last_ix = contour.last();
            let first_ix = contour.first();
            let (mut prev_x, mut prev_y) = point_to_i64(&points[last_ix]);
            for point in &points[first_ix..=last_ix] {
                let (x, y) = point_to_i64(point);
                area += (y - prev_y) * (x + prev_x);
                (prev_x, prev_y) = (x, y);
            }
        }
        use core::cmp::Ordering;
        self.orientation = match area.cmp(&0) {
            Ordering::Less => Some(Orientation::CounterClockwise),
            Ordering::Greater => Some(Orientation::Clockwise),
            Ordering::Equal => None,
        };
    }
}

/// FreeType's corner test (ftcalc.c `ft_corner_is_flat`).
fn is_corner_flat(in_x: i32, in_y: i32, out_x: i32, out_y: i32) -> bool {
    let ax = in_x + out_x;
    let ay = in_y + out_y;
    fn hypot(x: i32, y: i32) -> i32 {
        let x = x.abs();
        let y = y.abs();
        if x > y {
            x + ((3 * y) >> 3)
        } else {
            y + ((3 * x) >> 3)
        }
    }
    let d_in = hypot(in_x, in_y);
    let d_out = hypot(out_x, out_y);
    let d_hypot = hypot(ax, ay);
    (d_in + d_out - d_hypot) < (d_hypot >> 4)
}

#[derive(Copy, Clone, Default, Debug)]
pub(crate) struct Contour {
    first_ix: u16,
    last_ix: u16,
}

impl Contour {
    pub fn first(self) -> usize {
        self.first_ix as usize
    }

    pub fn last(self) -> usize {
        self.last_ix as usize
    }

    pub fn next(self, index: usize) -> usize {
        if index >= self.last_ix as usize {
            self.first_ix as usize
        } else {
            index + 1
        }
    }

    pub fn prev(self, index: usize) -> usize {
        if index <= self.first_ix as usize {
            self.last_ix as usize
        } else {
            index - 1
        }
    }

    pub fn range(self) -> Range<usize> {
        self.first()..self.last() + 1
    }
}

impl UnscaledOutlineSink for Outline {
    fn reserve(&mut self, additional: usize) {
        self.points.reserve(additional);
    }

    fn push(&mut self, point: UnscaledPoint) -> Result<(), DrawError> {
        let new_point = Point {
            flags: point.flags,
            fx: point.x as i32,
            fy: point.y as i32,
            ..Default::default()
        };
        let new_point_ix: u16 = self
            .points
            .len()
            .try_into()
            .map_err(|_| DrawError::InsufficientMemory)?;
        if point.is_contour_start {
            self.contours.push(Contour {
                first_ix: new_point_ix,
                last_ix: new_point_ix,
            });
        } else if let Some(last_contour) = self.contours.last_mut() {
            last_contour.last_ix += 1;
        } else {
            self.contours.push(Contour {
                first_ix: new_point_ix,
                last_ix: new_point_ix,
            });
        }
        self.points.push(new_point);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_from_vectors() {
        assert_eq!(Direction::new(-100, 0), Direction::Left);
        assert_eq!(Direction::new(100, 0), Direction::Right);
        assert_eq!(Direction::new(0, -100), Direction::Down);
        assert_eq!(Direction::new(0, 100), Direction::Up);
        assert_eq!(Direction::new(7, 100), Direction::Up);
        assert_eq!(Direction::new(8, 100), Direction::None);
    }

    #[test]
    fn direction_axes() {
        use Direction::*;
        let hori = [Left, Right];
        let vert = [Up, Down];
        for h in hori {
            for h2 in hori {
                assert!(h.is_same_axis(h2));
                if h != h2 {
                    assert!(h.is_opposite(h2));
                } else {
                    assert!(!h.is_opposite(h2));
                }
            }
            for v in vert {
                assert!(!h.is_same_axis(v));
                assert!(!h.is_opposite(v));
            }
        }
        for v in vert {
            for v2 in vert {
                assert!(v.is_same_axis(v2));
                if v != v2 {
                    assert!(v.is_opposite(v2));
                } else {
                    assert!(!v.is_opposite(v2));
                }
            }
        }
    }

    #[test]
    fn fill_sanity_checks_u16_index_bounds() {
        let mut outline = Outline::default();
        outline
            .points
            .resize(u16::MAX as usize + 1, Point::default());
        outline
            .contours
            .resize(u16::MAX as usize + 1, Contour::default());
        assert!(outline.analyze_and_validate().is_ok());
        outline
            .points
            .resize(u16::MAX as usize + 2, Point::default());
        assert_eq!(
            outline.analyze_and_validate(),
            Err(DrawError::TooManyPoints)
        );
        outline.points.clear();
        outline
            .contours
            .resize(u16::MAX as usize + 2, Contour::default());
        assert_eq!(
            outline.analyze_and_validate(),
            Err(DrawError::TooManyPoints)
        );
    }

    #[test]
    fn link_points_handles_max_points_last_next_overflow() {
        let mut outline = Outline::default();
        outline
            .points
            .resize(u16::MAX as usize + 1, Point::default());
        outline.contours.push(Contour {
            first_ix: 0,
            last_ix: u16::MAX,
        });
        outline.analyze_and_validate().unwrap();
        assert_eq!(outline.points[0].prev(), u16::MAX as usize);
        assert_eq!(outline.points[0].next(), 1);
        assert_eq!(
            outline.points[u16::MAX as usize].prev(),
            u16::MAX as usize - 1
        );
        assert_eq!(outline.points[u16::MAX as usize].next(), 0);
    }
}
