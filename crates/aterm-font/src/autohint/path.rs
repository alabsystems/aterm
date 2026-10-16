// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/path.rs and read-fonts 0.43.2
// src/tables/glyf.rs (`PointCoord for F26Dot6`), and modified by the aterm
// project in 2026: FreeType's decomposition alone, on 26.6 points, into a
// recording that replays to an `OutlineSink`.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! TrueType-style points to path segments, as `FT_Outline_Decompose` does
//! it: a contour that starts off the curve starts at its last point if that
//! one is on it, else at the implied midpoint of the two.

use super::fixed::F26Dot6;
use super::glyf::PointFlags;
use crate::OutlineSink;

/// A point the path conversion reads: 26.6 coordinates and curve flags.
#[derive(Copy, Clone, Default, Debug)]
pub(crate) struct ContourPoint {
    pub x: F26Dot6,
    pub y: F26Dot6,
    pub flags: PointFlags,
}

impl ContourPoint {
    fn point_f32(&self) -> (f32, f32) {
        (self.x.to_f32(), self.y.to_f32())
    }

    /// The midpoint, by FreeType's integer division on 26.6 (wrapping, as
    /// only a broken font can overflow it); it takes `other`'s flags.
    fn midpoint(&self, other: Self) -> Self {
        let mid =
            |a: F26Dot6, b: F26Dot6| F26Dot6::from_bits(a.to_bits().wrapping_add(b.to_bits()) / 2);
        Self {
            x: mid(self.x, other.x),
            y: mid(self.y, other.y),
            flags: other.flags,
        }
    }
}

/// Converts one contour to path segments. The last point is passed apart
/// (and also yielded by `points`) for the FreeType start-point rule.
pub(crate) fn contour_to_path(
    points: impl ExactSizeIterator<Item = ContourPoint>,
    first_point: ContourPoint,
    last_point: ContourPoint,
    pen: &mut impl OutlineSink,
) -> Result<(), ()> {
    // An off curve cubic cannot start a contour.
    if first_point.flags.is_off_curve_cubic() {
        return Err(());
    }
    let mut points = points.enumerate();
    // The last point is omitted when the contour starts there.
    let mut omit_last = false;
    let start_point = if first_point.flags.is_off_curve_quad() {
        if last_point.flags.is_on_curve() {
            // The last point is on the curve: start there.
            omit_last = true;
            last_point
        } else {
            // Also off the curve: start at the implied midpoint.
            last_point.midpoint(first_point)
        }
    } else {
        // On the curve: start here, consuming the point.
        points.next();
        first_point
    };
    let (x, y) = start_point.point_f32();
    pen.move_to(x, y);
    let mut state = PendingState::Empty;
    if omit_last {
        let end_ix = points.len() - 1;
        for (ix, point) in points {
            if ix == end_ix {
                break;
            }
            state.emit(point, pen)?;
        }
    } else {
        for (_, point) in points {
            state.emit(point, pen)?;
        }
    }
    state.finish(start_point, pen)?;
    Ok(())
}

#[derive(Copy, Clone)]
enum PendingState {
    /// No pending points.
    Empty,
    /// A pending quadratic control point.
    PendingQuad(ContourPoint),
    /// One pending cubic control point.
    PendingCubic(ContourPoint),
    /// Two pending cubic control points.
    TwoPendingCubics(ContourPoint, ContourPoint),
}

impl PendingState {
    #[inline(always)]
    fn emit(&mut self, point: ContourPoint, pen: &mut impl OutlineSink) -> Result<(), ()> {
        let flags = point.flags;
        match *self {
            Self::Empty => {
                if flags.is_off_curve_quad() {
                    *self = Self::PendingQuad(point);
                } else if flags.is_off_curve_cubic() {
                    *self = Self::PendingCubic(point);
                } else {
                    let (x, y) = point.point_f32();
                    pen.line_to(x, y);
                }
            }
            Self::PendingQuad(quad) => {
                if flags.is_off_curve_quad() {
                    let (cx, cy) = quad.point_f32();
                    let (x, y) = quad.midpoint(point).point_f32();
                    pen.quad_to(cx, cy, x, y);
                    *self = Self::PendingQuad(point);
                } else if flags.is_off_curve_cubic() {
                    return Err(());
                } else {
                    let (cx, cy) = quad.point_f32();
                    let (x, y) = point.point_f32();
                    pen.quad_to(cx, cy, x, y);
                    *self = Self::Empty;
                }
            }
            Self::PendingCubic(cubic) => {
                if flags.is_off_curve_cubic() {
                    *self = Self::TwoPendingCubics(cubic, point);
                } else {
                    return Err(());
                }
            }
            Self::TwoPendingCubics(cubic0, cubic1) => {
                if flags.is_off_curve_quad() {
                    return Err(());
                } else if flags.is_off_curve_cubic() {
                    let (c0x, c0y) = cubic0.point_f32();
                    let (c1x, c1y) = cubic1.point_f32();
                    let (x, y) = cubic1.midpoint(point).point_f32();
                    pen.curve_to(c0x, c0y, c1x, c1y, x, y);
                    *self = Self::PendingCubic(point);
                } else {
                    let (c0x, c0y) = cubic0.point_f32();
                    let (c1x, c1y) = cubic1.point_f32();
                    let (x, y) = point.point_f32();
                    pen.curve_to(c0x, c0y, c1x, c1y, x, y);
                    *self = Self::Empty;
                }
            }
        }
        Ok(())
    }

    fn finish(
        mut self,
        mut start_point: ContourPoint,
        pen: &mut impl OutlineSink,
    ) -> Result<(), ()> {
        match self {
            Self::Empty => {}
            _ => {
                // Always end on an explicit on curve point.
                start_point.flags = PointFlags::on_curve();
                self.emit(start_point, pen)?;
            }
        }
        pen.close();
        Ok(())
    }
}

/// One recorded path segment, in pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Segment {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Close,
}

/// A path held until the whole glyph has converted, so a failed glyph
/// draws nothing.
#[derive(Default)]
pub(crate) struct Recorded(Vec<Segment>);

impl Recorded {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn replay(&self, sink: &mut dyn OutlineSink) {
        for segment in &self.0 {
            match *segment {
                Segment::Move(x, y) => sink.move_to(x, y),
                Segment::Line(x, y) => sink.line_to(x, y),
                Segment::Quad(cx, cy, x, y) => sink.quad_to(cx, cy, x, y),
                Segment::Curve(c0x, c0y, c1x, c1y, x, y) => sink.curve_to(c0x, c0y, c1x, c1y, x, y),
                Segment::Close => sink.close(),
            }
        }
    }
}

impl OutlineSink for Recorded {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Segment::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Segment::Line(x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.push(Segment::Quad(x1, y1, x, y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.push(Segment::Curve(x1, y1, x2, y2, x, y));
    }
    fn close(&mut self) {
        self.0.push(Segment::Close);
    }
}

#[cfg(test)]
mod tests {
    // skrifa 0.46.2 src/outline/path.rs `all_off_curve_to_path_scan_backward`
    // and `start_off_curve_to_path_scan_backward` (the FreeType style).
    use super::*;

    fn path_of(all_off_curve: bool) -> Vec<Segment> {
        let pt = |x: i32, y: i32, on: bool| ContourPoint {
            x: F26Dot6::from_bits(x),
            y: F26Dot6::from_bits(y),
            flags: if on {
                PointFlags::on_curve()
            } else {
                PointFlags::off_curve_quad()
            },
        };
        // A contour of all off curve points starts at the midpoint of its
        // first and last: [(640, 128) + (128, 128)] / 2 = (384, 128) in
        // 26.6, (6.0, 2.0) in pixels.
        let points = [
            pt(640, 128, false),
            pt(256, 64, !all_off_curve),
            pt(640, 64, false),
            pt(128, 128, false),
        ];
        let mut pen = Recorded::default();
        contour_to_path(points.iter().copied(), points[0], points[3], &mut pen).unwrap();
        pen.0
    }

    #[test]
    fn all_off_curve_to_path_scan_backward() {
        use Segment::*;
        assert_eq!(
            path_of(true),
            [
                Move(6.0, 2.0),
                Quad(10.0, 2.0, 7.0, 1.5),
                Quad(4.0, 1.0, 7.0, 1.0),
                Quad(10.0, 1.0, 6.0, 1.5),
                Quad(2.0, 2.0, 6.0, 2.0),
                Close
            ]
        );
    }

    #[test]
    fn start_off_curve_to_path_scan_backward() {
        use Segment::*;
        assert_eq!(
            path_of(false),
            [
                Move(6.0, 2.0),
                Quad(10.0, 2.0, 4.0, 1.0),
                Quad(10.0, 1.0, 6.0, 1.5),
                Quad(2.0, 2.0, 6.0, 2.0),
                Close
            ]
        );
    }
}
