// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// `OutlineSink` is ttf-parser 0.25.1's `OutlineBuilder` (src/lib.rs, MIT OR
// Apache-2.0; see LICENSE-MIT and LICENSE-APACHE), renamed by the aterm
// project in 2026. The rest of this file is aterm's.

//! Where an outline goes: segments in font units ([`OutlineSink`]), the same
//! segments mapped into a raster's pixel space ([`ScaledSink`]), or the
//! unscaled points a grid-fitter works on ([`PointSink`]).

/// Receives a glyph outline as segments, in font units, y up.
///
/// Every contour starts with `move_to`. TrueType outlines arrive as
/// quadratic segments, CFF outlines as cubic ones, and each contour ends with
/// `close` except a CFF2 glyph's last one, which ends when the outline call
/// returns (CFF2 has no `endchar`; ttf-parser draws it so, and the corpus
/// pins it). A sink that needs every contour closed closes the open one
/// itself, as aterm-render's rasterizer sink does.
pub trait OutlineSink {
    /// Appends a MoveTo segment: the start of a contour.
    fn move_to(&mut self, x: f32, y: f32);

    /// Appends a LineTo segment.
    fn line_to(&mut self, x: f32, y: f32);

    /// Appends a QuadTo segment.
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32);

    /// Appends a CurveTo segment.
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32);

    /// Appends a ClosePath segment: the end of a contour.
    fn close(&mut self);
}

/// Discards the outline: what a bounding-box query outlines into.
pub(crate) struct NullSink;

impl OutlineSink for NullSink {
    #[inline]
    fn move_to(&mut self, _: f32, _: f32) {}
    #[inline]
    fn line_to(&mut self, _: f32, _: f32) {}
    #[inline]
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    #[inline]
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    #[inline]
    fn close(&mut self) {}
}

/// Forwards an outline to `inner` with every point mapped through
/// `(x, y) -> (x * sx + dx, y * sy + dy)`: font units to a raster's pixels.
///
/// `sx = sy = px / units_per_em` with no offset is the y-up pixel outline;
/// `sy = -sx`, `dx = -left`, `dy = top` puts it in a y-down ink box whose
/// top-left corner is `(left, top)`. Each coordinate is one multiply and one
/// add, in that order, so a caller that wrote `x * s - left` (or
/// `top - y * s`) gets the same bits back. "No offset" is `-0.0`, the one
/// value whose addition changes no bits (`-0.0 + 0.0` is `+0.0`).
pub struct ScaledSink<'a, S: OutlineSink + ?Sized> {
    inner: &'a mut S,
    sx: f32,
    sy: f32,
    dx: f32,
    dy: f32,
}

impl<'a, S: OutlineSink + ?Sized> ScaledSink<'a, S> {
    /// A uniform scale about the origin.
    #[inline]
    #[must_use]
    pub const fn new(inner: &'a mut S, scale: f32) -> Self {
        Self::affine(inner, scale, scale, -0.0, -0.0)
    }

    /// Scale `(sx, sy)`, then translate by `(dx, dy)`.
    #[inline]
    #[must_use]
    pub const fn affine(inner: &'a mut S, sx: f32, sy: f32, dx: f32, dy: f32) -> Self {
        Self {
            inner,
            sx,
            sy,
            dx,
            dy,
        }
    }

    #[inline]
    const fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.sx + self.dx, y * self.sy + self.dy)
    }
}

impl<S: OutlineSink + ?Sized> OutlineSink for ScaledSink<'_, S> {
    #[inline]
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        self.inner.move_to(x, y);
    }

    #[inline]
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.map(x, y);
        self.inner.line_to(x, y);
    }

    #[inline]
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = self.map(x1, y1);
        let (x, y) = self.map(x, y);
        self.inner.quad_to(x1, y1, x, y);
    }

    #[inline]
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = self.map(x1, y1);
        let (x2, y2) = self.map(x2, y2);
        let (x, y) = self.map(x, y);
        self.inner.curve_to(x1, y1, x2, y2, x, y);
    }

    #[inline]
    fn close(&mut self) {
        self.inner.close();
    }
}

/// What an outline point is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointKind {
    /// On the curve: a contour start, a line end or a curve end.
    OnCurve,
    /// A quadratic (TrueType) control point.
    OffCurveQuad,
    /// A cubic (CFF) control point.
    OffCurveCubic,
}

/// Receives a glyph outline as points, in font units, y up: each contour's
/// points in order, then `end_contour`.
///
/// For `glyf` these are the points the font stores (two adjacent quadratic
/// control points keep their implied on-curve midpoint implicit), after the
/// `gvar` deltas and a composite's component transforms. For CFF they are
/// each segment's control points and end point, with a contour's last point
/// dropped when it repeats the contour's start (the implicit close).
pub trait PointSink {
    /// The next point of the current contour.
    fn point(&mut self, x: f32, y: f32, kind: PointKind);

    /// The current contour is complete.
    fn end_contour(&mut self);
}

/// A segment the [`SegmentPoints`] adapter holds until it knows whether the
/// contour closes on it.
#[derive(Clone, Copy)]
enum Pending {
    Line(f32, f32),
    Cubic([f32; 6]),
}

/// Turns a segment stream (CFF) into points for a [`PointSink`].
pub(crate) struct SegmentPoints<'a> {
    sink: &'a mut dyn PointSink,
    start: Option<(f32, f32)>,
    pending: Option<Pending>,
}

impl<'a> SegmentPoints<'a> {
    #[inline]
    pub(crate) fn new(sink: &'a mut dyn PointSink) -> Self {
        Self {
            sink,
            start: None,
            pending: None,
        }
    }

    #[inline]
    fn flush(&mut self, closing: bool) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let (x, y) = match pending {
            Pending::Line(x, y) => (x, y),
            Pending::Cubic([x1, y1, x2, y2, x, y]) => {
                self.sink.point(x1, y1, PointKind::OffCurveCubic);
                self.sink.point(x2, y2, PointKind::OffCurveCubic);
                (x, y)
            }
        };
        if !closing || self.start != Some((x, y)) {
            self.sink.point(x, y, PointKind::OnCurve);
        }
    }

    #[inline]
    fn end(&mut self) {
        if self.start.is_some() {
            self.flush(true);
            self.sink.end_contour();
            self.start = None;
        }
    }

    /// Ends a contour the stream left open.
    #[inline]
    pub(crate) fn finish(&mut self) {
        self.end();
    }
}

impl OutlineSink for SegmentPoints<'_> {
    #[inline]
    fn move_to(&mut self, x: f32, y: f32) {
        self.end();
        self.sink.point(x, y, PointKind::OnCurve);
        self.start = Some((x, y));
    }

    #[inline]
    fn line_to(&mut self, x: f32, y: f32) {
        self.flush(false);
        self.pending = Some(Pending::Line(x, y));
    }

    #[inline]
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.flush(false);
        self.sink.point(x1, y1, PointKind::OffCurveQuad);
        self.sink.point(x, y, PointKind::OnCurve);
    }

    #[inline]
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.flush(false);
        self.pending = Some(Pending::Cubic([x1, y1, x2, y2, x, y]));
    }

    #[inline]
    fn close(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<(f32, f32, PointKind)>, usize);

    impl PointSink for Log {
        fn point(&mut self, x: f32, y: f32, kind: PointKind) {
            self.0.push((x, y, kind));
        }
        fn end_contour(&mut self) {
            self.1 += 1;
        }
    }

    use PointKind::{OffCurveCubic as Cubic, OnCurve as On};

    /// A closing line back to the start is the implicit close: dropped. A
    /// closing curve keeps its controls and drops only its end point.
    #[test]
    fn a_contour_closing_on_its_start_drops_the_repeat() {
        let mut log = Log::default();
        let mut s = SegmentPoints::new(&mut log);
        s.move_to(0.0, 0.0);
        s.line_to(10.0, 0.0);
        s.line_to(0.0, 0.0);
        s.close();
        s.move_to(0.0, 0.0);
        s.curve_to(1.0, 2.0, 3.0, 4.0, 5.0, 6.0);
        s.curve_to(7.0, 8.0, 9.0, 10.0, 0.0, 0.0);
        s.close();
        s.finish();
        assert_eq!(
            log.0,
            vec![
                (0.0, 0.0, On),
                (10.0, 0.0, On),
                (0.0, 0.0, On),
                (1.0, 2.0, Cubic),
                (3.0, 4.0, Cubic),
                (5.0, 6.0, On),
                (7.0, 8.0, Cubic),
                (9.0, 10.0, Cubic),
            ]
        );
        assert_eq!(log.1, 2);
    }

    /// An end point elsewhere stays, and a contour the stream leaves open is
    /// ended by `finish`.
    #[test]
    fn an_open_contour_keeps_its_last_point() {
        let mut log = Log::default();
        let mut s = SegmentPoints::new(&mut log);
        s.move_to(0.0, 0.0);
        s.line_to(10.0, 0.0);
        s.line_to(10.0, 10.0);
        s.finish();
        assert_eq!(
            log.0,
            vec![(0.0, 0.0, On), (10.0, 0.0, On), (10.0, 10.0, On)]
        );
        assert_eq!(log.1, 1);
    }
}
