// Copyright 2023 The AccessKit Authors.
// Copyright 2018 The kurbo Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit 0.24.1 (MIT OR Apache-2.0, used under Apache-2.0)
// src/geometry.rs, itself derived from kurbo (Apache-2.0 OR MIT), and cut by
// the aterm project in 2026 to the operations aterm's tree and publisher use.

//! The geometry of an accessibility tree: points, sizes and rectangles in a
//! node's coordinate space, and the affine transforms between spaces.

use std::ops::{Add, Mul};

/// A 2D affine transform: `[a, b, c, d, e, f]` maps `(x, y)` to
/// `(a x + c y + e, b x + d y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine([f64; 6]);

impl Affine {
    /// The identity transform.
    pub const IDENTITY: Affine = Affine::scale(1.0);

    /// A transform from its six coefficients.
    #[inline]
    pub const fn new(c: [f64; 6]) -> Affine {
        Affine(c)
    }

    /// A uniform scale by `s`.
    #[inline]
    pub const fn scale(s: f64) -> Affine {
        Affine([s, 0.0, 0.0, s, 0.0, 0.0])
    }

    /// A translation by `p`.
    #[inline]
    pub fn translate<V: Into<Vec2>>(p: V) -> Affine {
        let p = p.into();
        Affine([1.0, 0.0, 0.0, 1.0, p.x, p.y])
    }

    /// The six coefficients.
    #[inline]
    pub fn as_coeffs(self) -> [f64; 6] {
        self.0
    }

    /// The determinant of the linear part.
    pub fn determinant(self) -> f64 {
        self.0[0] * self.0[3] - self.0[1] * self.0[2]
    }

    /// The inverse transform. A singular transform's has non-finite
    /// coefficients.
    pub fn inverse(self) -> Affine {
        let inv_det = self.determinant().recip();
        Affine([
            inv_det * self.0[3],
            -inv_det * self.0[1],
            -inv_det * self.0[2],
            inv_det * self.0[0],
            inv_det * (self.0[2] * self.0[5] - self.0[3] * self.0[4]),
            inv_det * (self.0[1] * self.0[4] - self.0[0] * self.0[5]),
        ])
    }

    /// The bounding box of `rect` transformed.
    pub fn transform_rect_bbox(self, rect: Rect) -> Rect {
        let p00 = self * Point::new(rect.x0, rect.y0);
        let p01 = self * Point::new(rect.x0, rect.y1);
        let p10 = self * Point::new(rect.x1, rect.y0);
        let p11 = self * Point::new(rect.x1, rect.y1);
        Rect::from_points(p00, p01).union(Rect::from_points(p10, p11))
    }

    /// Whether every coefficient is finite.
    #[inline]
    pub fn is_finite(&self) -> bool {
        self.0.iter().all(|c| c.is_finite())
    }
}

impl Default for Affine {
    #[inline]
    fn default() -> Affine {
        Affine::IDENTITY
    }
}

impl Mul<Point> for Affine {
    type Output = Point;

    #[inline]
    fn mul(self, other: Point) -> Point {
        Point::new(
            self.0[0] * other.x + self.0[2] * other.y + self.0[4],
            self.0[1] * other.x + self.0[3] * other.y + self.0[5],
        )
    }
}

impl Mul for Affine {
    type Output = Affine;

    #[inline]
    fn mul(self, other: Affine) -> Affine {
        Affine([
            self.0[0] * other.0[0] + self.0[2] * other.0[1],
            self.0[1] * other.0[0] + self.0[3] * other.0[1],
            self.0[0] * other.0[2] + self.0[2] * other.0[3],
            self.0[1] * other.0[2] + self.0[3] * other.0[3],
            self.0[0] * other.0[4] + self.0[2] * other.0[5] + self.0[4],
            self.0[1] * other.0[4] + self.0[3] * other.0[5] + self.0[5],
        ])
    }
}

/// A point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    /// The origin.
    pub const ZERO: Point = Point::new(0., 0.);

    #[inline]
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }

    #[inline]
    pub const fn to_vec2(self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }
}

impl From<(f64, f64)> for Point {
    #[inline]
    fn from(v: (f64, f64)) -> Point {
        Point { x: v.0, y: v.1 }
    }
}

impl Add<Vec2> for Point {
    type Output = Point;

    #[inline]
    fn add(self, other: Vec2) -> Self {
        Point::new(self.x + other.x, self.y + other.y)
    }
}

/// A rectangle by its two corners: `(x0, y0)` the top left, `(x1, y1)` the
/// bottom right, y growing down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    /// The empty rectangle at the origin.
    pub const ZERO: Rect = Rect::new(0., 0., 0., 0.);

    #[inline]
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    /// The rectangle with corners `p0` and `p1`, in either order.
    #[inline]
    pub fn from_points(p0: impl Into<Point>, p1: impl Into<Point>) -> Rect {
        let p0 = p0.into();
        let p1 = p1.into();
        Rect::new(p0.x, p0.y, p1.x, p1.y).abs()
    }

    #[inline]
    pub fn from_origin_size(origin: impl Into<Point>, size: impl Into<Size>) -> Rect {
        let origin = origin.into();
        Rect::from_points(origin, origin + size.into().to_vec2())
    }

    /// This rectangle moved to `origin`, its size kept.
    #[inline]
    pub fn with_origin(self, origin: impl Into<Point>) -> Rect {
        Rect::from_origin_size(origin, self.size())
    }

    #[inline]
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    #[inline]
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    #[inline]
    pub fn origin(&self) -> Point {
        Point::new(self.x0, self.y0)
    }

    #[inline]
    pub fn size(&self) -> Size {
        Size::new(self.width(), self.height())
    }

    /// The same rectangle with its corners ordered.
    #[inline]
    pub fn abs(&self) -> Rect {
        let Rect { x0, y0, x1, y1 } = *self;
        Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }

    #[inline]
    pub fn area(&self) -> f64 {
        self.width() * self.height()
    }

    /// Whether the area is zero.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.area() == 0.0
    }

    /// Whether `point` is inside: the top and left edges are, the bottom and
    /// right ones are not.
    #[inline]
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x0 && point.x < self.x1 && point.y >= self.y0 && point.y < self.y1
    }

    /// The smallest rectangle holding both.
    #[inline]
    pub fn union(&self, other: Rect) -> Rect {
        Rect::new(
            self.x0.min(other.x0),
            self.y0.min(other.y0),
            self.x1.max(other.x1),
            self.y1.max(other.y1),
        )
    }

    /// The overlap of the two, empty (and ordered) when there is none.
    #[inline]
    pub fn intersect(&self, other: Rect) -> Rect {
        let x0 = self.x0.max(other.x0);
        let y0 = self.y0.max(other.y0);
        let x1 = self.x1.min(other.x1);
        let y1 = self.y1.min(other.y1);
        Rect::new(x0, y0, x1.max(x0), y1.max(y0))
    }
}

/// A size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    pub const ZERO: Size = Size::new(0., 0.);

    #[inline]
    pub const fn new(width: f64, height: f64) -> Self {
        Size { width, height }
    }

    #[inline]
    pub const fn to_vec2(self) -> Vec2 {
        Vec2::new(self.width, self.height)
    }
}

impl From<(f64, f64)> for Size {
    #[inline]
    fn from(v: (f64, f64)) -> Size {
        Size {
            width: v.0,
            height: v.1,
        }
    }
}

/// A displacement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2::new(0., 0.);

    #[inline]
    pub const fn new(x: f64, y: f64) -> Vec2 {
        Vec2 { x, y }
    }
}

impl From<(f64, f64)> for Vec2 {
    #[inline]
    fn from(v: (f64, f64)) -> Vec2 {
        Vec2 { x: v.0, y: v.1 }
    }
}
