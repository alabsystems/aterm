// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/unscaled.rs and modified by the
// aterm project in 2026: the buffer is a `Vec`.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! A compact unscaled, unhinted outline: integer points with their curve
//! flags and contour starts, what the hinter measures and fits.

use super::DrawError;
use super::fixed::{F26Dot6, Point};
use super::glyf::PointFlags;
use core::ops::Range;

#[derive(Copy, Clone, Default, Debug)]
pub(crate) struct UnscaledPoint {
    pub x: i16,
    pub y: i16,
    pub flags: PointFlags,
    pub is_contour_start: bool,
}

impl UnscaledPoint {
    /// A `glyf` point from the unscaled loader: whole units in 26.6,
    /// shifted down and truncated to 16 bits as skrifa does.
    pub(crate) fn from_glyf_point(
        point: Point<F26Dot6>,
        flags: PointFlags,
        is_contour_start: bool,
    ) -> Self {
        let point = point.map(|x| (x.to_bits() >> 6) as i16);
        Self {
            x: point.x,
            y: point.y,
            flags: flags.without_markers(),
            is_contour_start,
        }
    }

    pub(crate) fn is_on_curve(self) -> bool {
        self.flags.is_on_curve()
    }
}

/// Where unscaled points go.
pub(crate) trait UnscaledOutlineSink {
    fn reserve(&mut self, additional: usize);
    fn push(&mut self, point: UnscaledPoint) -> Result<(), DrawError>;
    fn extend(&mut self, points: impl IntoIterator<Item = UnscaledPoint>) -> Result<(), DrawError> {
        for point in points.into_iter() {
            self.push(point)?;
        }
        Ok(())
    }
}

/// An unscaled outline in a buffer.
#[derive(Default)]
pub(crate) struct UnscaledOutlineBuf(Vec<UnscaledPoint>);

impl UnscaledOutlineBuf {
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    pub(crate) fn as_ref(&self) -> UnscaledOutlineRef<'_> {
        UnscaledOutlineRef {
            points: self.0.as_slice(),
        }
    }
}

impl UnscaledOutlineSink for UnscaledOutlineBuf {
    fn reserve(&mut self, additional: usize) {
        self.0.reserve(additional);
    }

    fn push(&mut self, point: UnscaledPoint) -> Result<(), DrawError> {
        self.0.push(point);
        Ok(())
    }
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct UnscaledOutlineRef<'a> {
    pub points: &'a [UnscaledPoint],
}

impl UnscaledOutlineRef<'_> {
    /// The point range of the contour holding the last point where `f`
    /// answers true, and that point's index in it. Single-point contours
    /// are skipped. (The extremum search of blue zone computation, FreeType
    /// aflatin.c.)
    pub(crate) fn find_last_contour(
        &self,
        mut f: impl FnMut(&UnscaledPoint) -> bool,
    ) -> Option<(Range<usize>, usize)> {
        if self.points.is_empty() {
            return None;
        }
        let mut best_contour = 0..0;
        // Index of the best point relative to the start of the best contour.
        let mut best_point = 0;
        let mut cur_contour = 0..0;
        let mut found_best_in_cur_contour = false;
        for (point_ix, point) in self.points.iter().enumerate() {
            if point.is_contour_start {
                if found_best_in_cur_contour {
                    best_contour = cur_contour;
                }
                cur_contour = point_ix..point_ix;
                found_best_in_cur_contour = false;
                // Ignore single point contours.
                match self.points.get(point_ix + 1) {
                    Some(next_point) if next_point.is_contour_start => continue,
                    None => continue,
                    _ => {}
                }
            }
            cur_contour.end += 1;
            if f(point) {
                best_point = point_ix - cur_contour.start;
                found_best_in_cur_contour = true;
            }
        }
        if found_best_in_cur_contour {
            best_contour = cur_contour;
        }
        if !best_contour.is_empty() {
            Some((best_contour, best_point))
        } else {
            None
        }
    }
}

#[derive(Copy, Clone)]
enum PendingElement {
    Line([f32; 2]),
    Cubic([f32; 6]),
}

/// Feeds an [`UnscaledOutlineSink`] from segments (the CFF path), holding
/// each segment until it knows whether the contour closes on it.
pub(crate) struct UnscaledPenAdapter<'a, T> {
    sink: &'a mut T,
    failed: bool,
    last_start: Option<(f32, f32)>,
    pending: Option<PendingElement>,
}

impl<'a, T: UnscaledOutlineSink> UnscaledPenAdapter<'a, T> {
    pub(crate) fn new(sink: &'a mut T) -> Self {
        Self {
            sink,
            failed: false,
            last_start: None,
            pending: None,
        }
    }

    fn push(&mut self, x: f32, y: f32, flags: PointFlags, is_contour_start: bool) {
        if self
            .sink
            .push(UnscaledPoint {
                x: x as i16,
                y: y as i16,
                flags,
                is_contour_start,
            })
            .is_err()
        {
            self.failed = true;
        }
    }

    fn flush_pending(&mut self, for_close: bool) {
        if let Some(element) = self.pending.take() {
            let [x, y] = match element {
                PendingElement::Line([x, y]) => [x, y],
                PendingElement::Cubic([x0, y0, x1, y1, x, y]) => {
                    self.push(x0, y0, PointFlags::off_curve_cubic(), false);
                    self.push(x1, y1, PointFlags::off_curve_cubic(), false);
                    [x, y]
                }
            };
            if !for_close || self.last_start != Some((x, y)) {
                self.push(x, y, PointFlags::on_curve(), false);
            }
        }
    }

    pub(crate) fn finish(mut self) -> Result<(), DrawError> {
        self.flush_pending(true);
        if self.failed {
            Err(DrawError::InsufficientMemory)
        } else {
            Ok(())
        }
    }

    pub(crate) fn move_to(&mut self, x: f32, y: f32) {
        self.push(x, y, PointFlags::on_curve(), true);
        self.last_start = Some((x, y));
    }

    pub(crate) fn line_to(&mut self, x: f32, y: f32) {
        self.flush_pending(false);
        self.pending = Some(PendingElement::Line([x, y]));
    }

    pub(crate) fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.flush_pending(false);
        self.pending = Some(PendingElement::Cubic([cx0, cy0, cx1, cy1, x, y]));
    }

    pub(crate) fn close(&mut self) {
        self.flush_pending(true);
    }
}
