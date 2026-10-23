// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/glyf/mod.rs, src/outline/glyf/outline.rs
// and src/outline/glyf/memory.rs, and from read-fonts 0.43.2
// src/tables/glyf.rs, src/tables/loca.rs, src/tables/hmtx.rs and
// generated/generated_glyf.rs, and modified by the aterm project in 2026:
// FreeType's unscaled `glyf` loader (the path skrifa's autohinter loads its
// outlines through), without the TrueType interpreter, the scaled paths or the
// metrics it does not reach, every read checked.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Unscaled TrueType `glyf` outline loader for the autohinter.

use super::DrawError;
use super::deltas;
use super::fixed::{F2Dot14, F26Dot6, Fixed, Point};
use super::tuple::{Gvar, ReadError};

/// Composite recursion limit (skrifa's `GLYF_COMPOSITE_RECURSION_LIMIT`).
const GLYF_COMPOSITE_RECURSION_LIMIT: usize = 32;
/// Most points a glyph may have (skrifa's `MAX_GLYF_POINTS`).
const MAX_GLYF_POINTS: usize = u16::MAX as usize;
/// Most component edges a composite graph may have (skrifa's `MAX_GRAPH_EDGES`).
const MAX_GRAPH_EDGES: usize = 2048;

/// Number of phantom points generated at the end of an outline.
pub(crate) const PHANTOM_POINT_COUNT: usize = 4;

const ON_CURVE_POINT: u8 = 0x01;
const X_SHORT_VECTOR: u8 = 0x02;
const Y_SHORT_VECTOR: u8 = 0x04;
const REPEAT_FLAG: u8 = 0x08;
const X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR: u8 = 0x10;
const Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR: u8 = 0x20;

const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const ARGS_ARE_XY_VALUES: u16 = 0x0002;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
pub(crate) const USE_MY_METRICS: u16 = 0x0200;
const SCALED_COMPONENT_OFFSET: u16 = 0x0800;
const UNSCALED_COMPONENT_OFFSET: u16 = 0x1000;

/// Marker bits for point flags set during variation delta processing and hinting.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub(crate) struct PointMarker(u8);

impl core::ops::BitOr for PointMarker {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl PointMarker {
    /// The point has an explicit delta in a glyph variation tuple.
    pub const HAS_DELTA: Self = Self(0x4);

    /// The point is a candidate for weak interpolation (autohinter).
    pub const WEAK_INTERPOLATION: Self = Self(0x2);

    /// The distance to the next point is very small (autohinter).
    pub const NEAR: Self = Self(0x8);

    /// The x coordinate was touched by an IUP-style interpolation.
    pub const TOUCHED_X: Self = Self(0x10);

    /// The y coordinate was touched by an IUP-style interpolation.
    pub const TOUCHED_Y: Self = Self(0x20);
}

/// Flags describing a point.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
#[repr(transparent)]
pub(crate) struct PointFlags(u8);

impl PointFlags {
    const ON_CURVE: u8 = ON_CURVE_POINT;
    const OFF_CURVE_CUBIC: u8 = 0x80;
    const CURVE_MASK: u8 = Self::ON_CURVE | Self::OFF_CURVE_CUBIC;

    /// An on curve point.
    pub const fn on_curve() -> Self {
        Self(Self::ON_CURVE)
    }

    /// An off curve quadratic point.
    #[cfg(test)]
    pub const fn off_curve_quad() -> Self {
        Self(0)
    }

    /// An off curve cubic point.
    pub const fn off_curve_cubic() -> Self {
        Self(Self::OFF_CURVE_CUBIC)
    }

    /// True for an on curve point.
    #[inline]
    pub const fn is_on_curve(self) -> bool {
        self.0 & Self::ON_CURVE != 0
    }

    /// True for an off curve quadratic point.
    #[inline]
    pub const fn is_off_curve_quad(self) -> bool {
        self.0 & Self::CURVE_MASK == 0
    }

    /// True for an off curve cubic point.
    #[inline]
    pub const fn is_off_curve_cubic(self) -> bool {
        self.0 & Self::OFF_CURVE_CUBIC != 0
    }

    /// True if the given marker is set.
    pub fn has_marker(self, marker: PointMarker) -> bool {
        self.0 & marker.0 != 0
    }

    /// Sets the given marker.
    pub fn set_marker(&mut self, marker: PointMarker) {
        self.0 |= marker.0;
    }

    /// Clears the given marker.
    pub fn clear_marker(&mut self, marker: PointMarker) {
        self.0 &= !marker.0
    }

    /// A copy with every marker cleared.
    pub const fn without_markers(self) -> Self {
        Self(self.0 & Self::CURVE_MASK)
    }
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, ReadError> {
    let end = offset.checked_add(2).ok_or(ReadError)?;
    let b = data.get(offset..end).ok_or(ReadError)?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

fn read_i16(data: &[u8], offset: usize) -> Result<i16, ReadError> {
    read_u16(data, offset).map(|v| v as i16)
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, ReadError> {
    let end = offset.checked_add(4).ok_or(ReadError)?;
    let b = data.get(offset..end).ok_or(ReadError)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The `loca` + `glyf` pair.
#[derive(Clone, Copy)]
pub(crate) struct Glyf<'a> {
    loca: &'a [u8],
    long: bool,
    glyf: &'a [u8],
}

impl<'a> Glyf<'a> {
    /// `loca` must hold whole offsets (2 bytes each, 4 when long), as read-fonts requires.
    fn new(loca: &'a [u8], long: bool, glyf: &'a [u8]) -> Option<Self> {
        let size = if long { 4 } else { 2 };
        (loca.len() % size == 0).then_some(Self { loca, long, glyf })
    }

    fn loca_offset(&self, idx: usize) -> Option<u32> {
        if self.long {
            read_u32(self.loca, idx.checked_mul(4)?).ok()
        } else {
            read_u16(self.loca, idx.checked_mul(2)?)
                .ok()
                .map(|v| u32::from(v) * 2)
        }
    }

    /// The glyph, `None` for an empty one, or an error.
    pub(crate) fn get(&self, gid: u16) -> Result<Option<Glyph<'a>>, ReadError> {
        let idx = usize::from(gid);
        let start = self.loca_offset(idx).ok_or(ReadError)?;
        let end = self.loca_offset(idx + 1).ok_or(ReadError)?;
        if start == end {
            return Ok(None);
        }
        let data = self
            .glyf
            .get(start as usize..end as usize)
            .ok_or(ReadError)?;
        Glyph::read(data).map(Some)
    }
}

/// A simple or composite glyph.
#[derive(Clone, Copy)]
pub(crate) enum Glyph<'a> {
    Simple(SimpleGlyph<'a>),
    Composite(CompositeGlyph<'a>),
}

impl<'a> Glyph<'a> {
    fn read(data: &'a [u8]) -> Result<Self, ReadError> {
        let number_of_contours = read_i16(data, 0)?;
        if number_of_contours >= 0 {
            // numberOfContours, the box and instructionLength.
            if data.len() < 12 {
                return Err(ReadError);
            }
            Ok(Self::Simple(SimpleGlyph { data }))
        } else {
            if data.len() < 10 {
                return Err(ReadError);
            }
            Ok(Self::Composite(CompositeGlyph { data }))
        }
    }

    /// `[x_min, x_max, y_min, y_max]` from the glyph header.
    fn bounds(&self) -> [i16; 4] {
        let data = match self {
            Self::Simple(g) => g.data,
            Self::Composite(g) => g.data,
        };
        let at = |o| read_i16(data, o).unwrap_or_default();
        [at(2), at(6), at(4), at(8)]
    }
}

/// A simple glyph: contours of on- and off-curve points.
#[derive(Clone, Copy)]
pub(crate) struct SimpleGlyph<'a> {
    data: &'a [u8],
}

impl<'a> SimpleGlyph<'a> {
    fn number_of_contours(&self) -> usize {
        read_i16(self.data, 0).unwrap_or_default().max(0) as usize
    }

    /// The contour end points, or none when the array runs past the glyph.
    fn end_pts_of_contours(&self) -> &'a [u8] {
        let len = self.number_of_contours().saturating_mul(2);
        self.data
            .get(10..)
            .and_then(|d| d.get(..len))
            .unwrap_or_default()
    }

    fn end_pt(&self, i: usize) -> u16 {
        read_u16(self.end_pts_of_contours(), i * 2).unwrap_or_default()
    }

    fn contour_count(&self) -> usize {
        self.end_pts_of_contours().len() / 2
    }

    fn validate_contours(&self) -> Result<(), ReadError> {
        let endpoints = self.end_pts_of_contours();
        if endpoints.len() / 2 != self.number_of_contours() {
            return Err(ReadError);
        }
        let mut previous = None;
        for bytes in endpoints.as_chunks::<2>().0 {
            let endpoint = u16::from_be_bytes(*bytes);
            if previous.is_some_and(|previous| endpoint <= previous) {
                return Err(ReadError);
            }
            previous = Some(endpoint);
        }
        Ok(())
    }

    /// The number of points: the last contour end point, plus one.
    pub(crate) fn num_points(&self) -> usize {
        match self.contour_count() {
            0 => 0,
            n => usize::from(self.end_pt(n - 1)) + 1,
        }
    }

    /// The flags and coordinates after the instructions, or none.
    fn glyph_data(&self) -> &'a [u8] {
        let length_at = 10 + self.number_of_contours().saturating_mul(2);
        let instruction_length = read_u16(self.data, length_at).unwrap_or_default();
        let start = length_at + 2 + usize::from(instruction_length);
        self.data.get(start..).unwrap_or_default()
    }

    /// Reads the points and their on-curve flags (every other flag bit is dropped).
    fn read_points_fast(
        &self,
        points: &mut [Point<i32>],
        flags: &mut [PointFlags],
    ) -> Result<(), ReadError> {
        let n_points = self.num_points();
        if points.len() != n_points || flags.len() != n_points {
            return Err(ReadError);
        }
        if n_points == 0 {
            return Ok(());
        }
        let data = self.glyph_data();
        let mut flags_iter = data.iter().copied();
        let mut read_flags_bytes = 0;
        let mut i = 0;
        while let Some(flag_bits) = flags_iter.next() {
            read_flags_bytes += 1;
            if flag_bits & REPEAT_FLAG != 0 {
                let count = (flags_iter.next().ok_or(ReadError)? as usize + 1).min(n_points - i);
                read_flags_bytes += 1;
                for f in flags.get_mut(i..i + count).ok_or(ReadError)? {
                    f.0 = flag_bits;
                }
                i += count;
            } else {
                flags.get_mut(i).ok_or(ReadError)?.0 = flag_bits;
                i += 1;
            }
            if i == n_points {
                break;
            }
        }
        let mut pos = read_flags_bytes;
        let read_u8 = |pos: &mut usize| -> Result<u8, ReadError> {
            let v = *data.get(*pos).ok_or(ReadError)?;
            *pos += 1;
            Ok(v)
        };
        let mut x = 0i32;
        for (&point_flags, point) in flags.iter().zip(points.iter_mut()) {
            let mut delta = 0i32;
            let flag = point_flags.0;
            if flag & X_SHORT_VECTOR != 0 {
                delta = i32::from(read_u8(&mut pos)?);
                if flag & X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR == 0 {
                    delta = -delta;
                }
            } else if flag & X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR == 0 {
                delta = i32::from(read_i16(data, pos)?);
                pos += 2;
            }
            x = x.wrapping_add(delta);
            point.x = x;
        }
        let mut y = 0i32;
        for (point_flags, point) in flags.iter_mut().zip(points.iter_mut()) {
            let mut delta = 0i32;
            let flag = point_flags.0;
            if flag & Y_SHORT_VECTOR != 0 {
                delta = i32::from(read_u8(&mut pos)?);
                if flag & Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR == 0 {
                    delta = -delta;
                }
            } else if flag & Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR == 0 {
                delta = i32::from(read_i16(data, pos)?);
                pos += 2;
            }
            y = y.wrapping_add(delta);
            point.y = y;
            point_flags.0 &= PointFlags::ON_CURVE;
        }
        Ok(())
    }
}

/// A composite glyph: transformed references to other glyphs.
#[derive(Clone, Copy)]
pub(crate) struct CompositeGlyph<'a> {
    data: &'a [u8],
}

/// How a component is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    Offset { x: i16, y: i16 },
    Point { base: u16, component: u16 },
}

/// A component's 2x2 transform, in 2.14.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Transform {
    xx: F2Dot14,
    yx: F2Dot14,
    xy: F2Dot14,
    yy: F2Dot14,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            xx: F2Dot14::ONE,
            yx: F2Dot14::ZERO,
            xy: F2Dot14::ZERO,
            yy: F2Dot14::ONE,
        }
    }
}

/// One component of a composite glyph.
#[derive(Clone, Copy, Debug)]
struct Component {
    flags: u16,
    glyph: u16,
    anchor: Anchor,
    transform: Transform,
}

impl<'a> CompositeGlyph<'a> {
    fn component_data(&self) -> &'a [u8] {
        self.data.get(10..).unwrap_or_default()
    }

    /// Every component, fully read; stops at the first that runs past the data.
    fn components(&self) -> impl Iterator<Item = Component> + 'a {
        let data = self.component_data();
        let mut pos = 0usize;
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            let u16_at = |pos: &mut usize| -> Option<u16> {
                let v = read_u16(data, *pos).ok()?;
                *pos += 2;
                Some(v)
            };
            let flags = u16_at(&mut pos)?;
            let glyph = u16_at(&mut pos)?;
            let words = flags & ARG_1_AND_2_ARE_WORDS != 0;
            let xy_values = flags & ARGS_ARE_XY_VALUES != 0;
            let u8_at = |pos: &mut usize| -> Option<u8> {
                let v = *data.get(*pos)?;
                *pos += 1;
                Some(v)
            };
            let anchor = match (xy_values, words) {
                (true, true) => Anchor::Offset {
                    x: u16_at(&mut pos)? as i16,
                    y: u16_at(&mut pos)? as i16,
                },
                (true, false) => Anchor::Offset {
                    x: i16::from(u8_at(&mut pos)? as i8),
                    y: i16::from(u8_at(&mut pos)? as i8),
                },
                (false, true) => Anchor::Point {
                    base: u16_at(&mut pos)?,
                    component: u16_at(&mut pos)?,
                },
                (false, false) => Anchor::Point {
                    base: u16::from(u8_at(&mut pos)?),
                    component: u16::from(u8_at(&mut pos)?),
                },
            };
            let f2dot14 = |pos: &mut usize| -> Option<F2Dot14> {
                Some(F2Dot14::from_bits(u16_at(pos)? as i16))
            };
            let mut transform = Transform::default();
            if flags & WE_HAVE_A_SCALE != 0 {
                transform.xx = f2dot14(&mut pos)?;
                transform.yy = transform.xx;
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                transform.xx = f2dot14(&mut pos)?;
                transform.yy = f2dot14(&mut pos)?;
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                transform.xx = f2dot14(&mut pos)?;
                transform.yx = f2dot14(&mut pos)?;
                transform.xy = f2dot14(&mut pos)?;
                transform.yy = f2dot14(&mut pos)?;
            }
            done = flags & MORE_COMPONENTS == 0;
            Some(Component {
                flags,
                glyph,
                anchor,
                transform,
            })
        })
    }

    /// Each component's glyph id and flags.
    pub(crate) fn component_glyphs_and_flags(&self) -> impl Iterator<Item = (u16, u16)> + 'a {
        let data = self.component_data();
        let mut pos = 0usize;
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            let flags = read_u16(data, pos).ok()?;
            pos = pos.saturating_add(2);
            let glyph = read_u16(data, pos).ok()?;
            pos = pos.saturating_add(2);
            pos = pos.saturating_add(if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                4
            } else {
                2
            });
            if flags & WE_HAVE_A_SCALE != 0 {
                pos = pos.saturating_add(2);
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                pos = pos.saturating_add(4);
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                pos = pos.saturating_add(8);
            }
            done = flags & MORE_COMPONENTS == 0;
            Some((glyph, flags))
        })
    }
}

/// The horizontal metrics (`hhea` + `hmtx`), at the default instance.
#[derive(Clone, Copy)]
struct HMetrics<'a> {
    h_metrics: &'a [u8],
    side_bearings: &'a [u8],
}

impl<'a> HMetrics<'a> {
    fn new(hhea: &'a [u8], hmtx: &'a [u8]) -> Option<Self> {
        if hhea.len() < 36 {
            return None;
        }
        let count = usize::from(read_u16(hhea, 34).ok()?);
        let len = count * 4;
        let h_metrics = hmtx.get(..len).unwrap_or_default();
        let side_bearings = hmtx
            .get(len..)
            .map(|rest| &rest[..rest.len() / 2 * 2])
            .unwrap_or_default();
        Some(Self {
            h_metrics,
            side_bearings,
        })
    }

    fn count(&self) -> usize {
        self.h_metrics.len() / 4
    }

    fn advance(&self, gid: u16) -> i32 {
        let ix = usize::from(gid);
        let ix = if ix < self.count() {
            ix
        } else if let Some(last) = self.count().checked_sub(1) {
            last
        } else {
            return 0;
        };
        i32::from(read_u16(self.h_metrics, ix * 4).unwrap_or_default())
    }

    fn lsb(&self, gid: u16) -> i32 {
        let ix = usize::from(gid);
        let lsb = if ix < self.count() {
            read_i16(self.h_metrics, ix * 4 + 2).ok()
        } else {
            read_i16(self.side_bearings, ix.saturating_sub(self.count()) * 2).ok()
        };
        i32::from(lsb.unwrap_or_default())
    }
}

/// The font's TrueType outline tables (skrifa's `glyf::Outlines`).
#[derive(Clone, Copy)]
pub(crate) struct Outlines<'a> {
    pub(crate) glyf: Glyf<'a>,
    gvar: Option<Gvar<'a>>,
    metrics: HMetrics<'a>,
    os2_vmetrics: [i16; 2],
}

/// The raw tables an [`Outlines`] reads.
pub(crate) struct Tables<'a> {
    pub head: &'a [u8],
    pub loca: Option<&'a [u8]>,
    pub glyf: Option<&'a [u8]>,
    pub gvar: Option<&'a [u8]>,
    pub hhea: Option<&'a [u8]>,
    pub hmtx: Option<&'a [u8]>,
    pub os2: Option<&'a [u8]>,
}

impl<'a> Outlines<'a> {
    /// `None` without `loca`, `glyf`, `hhea` or `hmtx`, or with a short `head` or `hhea` or a `loca` of partial offsets.
    pub(crate) fn new(tables: &Tables<'a>) -> Option<Self> {
        if tables.head.len() < 54 {
            return None;
        }
        let long = read_i16(tables.head, 50).ok()? == 1;
        let glyf = Glyf::new(tables.loca?, long, tables.glyf?)?;
        let metrics = HMetrics::new(tables.hhea?, tables.hmtx?)?;
        let os2_vmetrics = tables
            .os2
            .filter(|os2| os2.len() >= 78)
            .map(|os2| {
                [
                    read_i16(os2, 68).unwrap_or_default(),
                    read_i16(os2, 70).unwrap_or_default(),
                ]
            })
            .unwrap_or_default();
        Some(Self {
            glyf,
            gvar: tables.gvar.and_then(Gvar::new),
            metrics,
            os2_vmetrics,
        })
    }

    /// The glyph's outline description.
    pub(crate) fn outline(&self, glyph_id: u16) -> Result<Outline<'a>, DrawError> {
        let mut outline = Outline {
            glyph_id,
            has_variations: self.gvar.is_some(),
            ..Default::default()
        };
        let glyph = self.glyf.get(glyph_id)?;
        if let Some(glyph) = glyph.as_ref() {
            self.outline_rec(glyph, &mut outline, 0, 0, &mut 0)?;
        }
        outline.points += PHANTOM_POINT_COUNT;
        outline.glyph = glyph;
        Ok(outline)
    }

    fn outline_rec(
        &self,
        glyph: &Glyph,
        outline: &mut Outline,
        component_depth: usize,
        recurse_depth: usize,
        total_components: &mut usize,
    ) -> Result<(), DrawError> {
        if recurse_depth > GLYF_COMPOSITE_RECURSION_LIMIT {
            return Err(DrawError::RecursionLimitExceeded);
        }
        match glyph {
            Glyph::Simple(simple) => {
                simple.validate_contours()?;
                let num_points = simple.num_points();
                let num_points_with_phantom = num_points + PHANTOM_POINT_COUNT;
                outline.max_simple_points = outline.max_simple_points.max(num_points_with_phantom);
                outline.points += num_points;
                if outline.points > MAX_GLYF_POINTS {
                    return Err(DrawError::TooManyPoints);
                }
                outline.contours += simple.contour_count();
                outline.max_other_points = outline.max_other_points.max(num_points_with_phantom);
            }
            Glyph::Composite(composite) => {
                let mut count = composite.component_glyphs_and_flags().count();
                count += PHANTOM_POINT_COUNT;
                for (component, _flags) in composite.component_glyphs_and_flags() {
                    *total_components += 1;
                    if *total_components > MAX_GRAPH_EDGES {
                        return Err(DrawError::RecursionLimitExceeded);
                    }
                    let component_glyph = self.glyf.get(component)?;
                    let Some(component_glyph) = component_glyph else {
                        continue;
                    };
                    self.outline_rec(
                        &component_glyph,
                        outline,
                        component_depth + count,
                        recurse_depth + 1,
                        total_components,
                    )?;
                }
                outline.max_component_delta_stack = outline
                    .max_component_delta_stack
                    .max(component_depth + count);
            }
        }
        Ok(())
    }
}

/// What loading one glyph takes (skrifa's `glyf::Outline`, the unhinted fields).
#[derive(Clone, Copy, Default)]
pub(crate) struct Outline<'a> {
    pub glyph_id: u16,
    /// The top-level glyph.
    pub glyph: Option<Glyph<'a>>,
    /// Sum of the point counts of every simple glyph, plus the phantoms.
    pub points: usize,
    /// Sum of the contour counts of every simple glyph.
    pub contours: usize,
    /// Most points in one simple glyph, phantoms included.
    pub max_simple_points: usize,
    /// The unscaled point buffer's size.
    pub max_other_points: usize,
    /// The component delta stack's depth.
    pub max_component_delta_stack: usize,
    /// True if the font has `gvar`.
    pub has_variations: bool,
}

/// The loader's unhinted buffers (skrifa's `FreeTypeOutlineMemory`), zeroed per glyph.
struct Memory {
    unscaled: Vec<Point<i32>>,
    scaled: Vec<Point<F26Dot6>>,
    contours: Vec<u16>,
    flags: Vec<PointFlags>,
    deltas: Vec<Point<Fixed>>,
    iup_buffer: Vec<Point<Fixed>>,
    composite_deltas: Vec<Point<Fixed>>,
}

impl Memory {
    fn new(outline: &Outline) -> Self {
        let (deltas, iup, composite) = if outline.has_variations {
            (
                outline.max_simple_points,
                outline.max_simple_points,
                outline.max_component_delta_stack,
            )
        } else {
            (0, 0, 0)
        };
        Self {
            unscaled: vec![Point::default(); outline.max_other_points],
            scaled: vec![Point::default(); outline.points],
            contours: vec![0; outline.contours],
            flags: vec![PointFlags::default(); outline.points],
            deltas: vec![Point::default(); deltas],
            iup_buffer: vec![Point::default(); iup],
            composite_deltas: vec![Point::default(); composite],
        }
    }
}

/// An outline as FreeType loads it unscaled.
pub(crate) struct UnscaledOutline {
    pub points: Vec<Point<F26Dot6>>,
    pub flags: Vec<PointFlags>,
    pub contours: Vec<u16>,
}

/// FreeType's loader at no scale.
pub(crate) struct UnscaledScaler<'a> {
    outlines: &'a Outlines<'a>,
    memory: Memory,
    coords: &'a [F2Dot14],
    point_count: usize,
    contour_count: usize,
    component_delta_count: usize,
    /// The four phantom points.
    phantom: [Point<F26Dot6>; PHANTOM_POINT_COUNT],
}

impl<'a> UnscaledScaler<'a> {
    pub(crate) fn new(
        outlines: &'a Outlines<'a>,
        outline: &Outline,
        coords: &'a [F2Dot14],
    ) -> Result<Self, DrawError> {
        if outline.points > MAX_GLYF_POINTS {
            return Err(DrawError::TooManyPoints);
        }
        Ok(Self {
            outlines,
            memory: Memory::new(outline),
            coords,
            point_count: 0,
            contour_count: 0,
            component_delta_count: 0,
            phantom: Default::default(),
        })
    }

    /// Loads the glyph and shifts it so its left phantom point is at x = 0.
    pub(crate) fn load_outline(
        mut self,
        glyph: &Option<Glyph>,
        glyph_id: u16,
    ) -> Result<UnscaledOutline, DrawError> {
        self.load(glyph, glyph_id, 0)?;
        let mut points = self.memory.scaled;
        points.truncate(self.point_count);
        let mut flags = self.memory.flags;
        flags.truncate(self.point_count);
        let mut contours = self.memory.contours;
        contours.truncate(self.contour_count);
        let x_shift = self.phantom[0].x;
        if x_shift != F26Dot6::ZERO {
            for point in points.iter_mut() {
                point.x -= x_shift;
            }
        }
        Ok(UnscaledOutline {
            points,
            flags,
            contours,
        })
    }

    fn load(
        &mut self,
        glyph: &Option<Glyph>,
        glyph_id: u16,
        recurse_depth: usize,
    ) -> Result<(), DrawError> {
        if recurse_depth > GLYF_COMPOSITE_RECURSION_LIMIT {
            return Err(DrawError::RecursionLimitExceeded);
        }
        let bounds = match &glyph {
            Some(glyph) => glyph.bounds(),
            _ => [0; 4],
        };
        let lsb = self.outlines.metrics.lsb(glyph_id);
        let advance = self.outlines.metrics.advance(glyph_id);
        let [ascent, descent] = self.outlines.os2_vmetrics.map(i32::from);
        let tsb = ascent - i32::from(bounds[3]);
        let vadvance = ascent - descent;
        self.setup_phantom_points(bounds, lsb, advance, tsb, vadvance);
        match glyph {
            Some(Glyph::Simple(simple)) => self.load_simple(simple, glyph_id),
            Some(Glyph::Composite(composite)) => {
                self.load_composite(composite, glyph_id, recurse_depth)
            }
            None => self.load_empty(glyph_id),
        }
    }

    fn setup_phantom_points(
        &mut self,
        bounds: [i16; 4],
        lsb: i32,
        advance: i32,
        tsb: i32,
        vadvance: i32,
    ) {
        self.phantom[0].x = F26Dot6::from_bits(i32::from(bounds[0]) - lsb);
        self.phantom[0].y = F26Dot6::ZERO;
        self.phantom[1].x = self.phantom[0].x + F26Dot6::from_bits(advance);
        self.phantom[1].y = F26Dot6::ZERO;
        self.phantom[2].x = F26Dot6::ZERO;
        self.phantom[2].y = F26Dot6::from_bits(i32::from(bounds[3]) + tsb);
        self.phantom[3].x = F26Dot6::ZERO;
        self.phantom[3].y = self.phantom[2].y - F26Dot6::from_bits(vadvance);
    }

    fn load_empty(&mut self, glyph_id: u16) -> Result<(), DrawError> {
        let mut unscaled = self.phantom.map(|point| point.map(|x| x.to_bits()));
        if let (Some(gvar), false) = (self.outlines.gvar.as_ref(), self.coords.is_empty()) {
            if let Ok(Some(deltas)) =
                gvar.phantom_point_deltas(&self.outlines.glyf, self.coords, glyph_id)
            {
                unscaled[0] += deltas[0].map(Fixed::to_i32);
                unscaled[1] += deltas[1].map(Fixed::to_i32);
            }
        }
        for (phantom, unscaled) in self.phantom.iter_mut().zip(&unscaled) {
            *phantom = unscaled.map(F26Dot6::from_i32);
        }
        Ok(())
    }

    fn load_simple(&mut self, glyph: &SimpleGlyph, glyph_id: u16) -> Result<(), DrawError> {
        use DrawError::InsufficientMemory;
        let points_start = self.point_count;
        let point_count = glyph.num_points();
        let phantom_start = point_count;
        let points_end = points_start + point_count + PHANTOM_POINT_COUNT;
        let point_range = points_start..points_end;
        let other_points_end = point_count + PHANTOM_POINT_COUNT;
        let Memory {
            unscaled,
            scaled,
            contours,
            flags,
            deltas,
            iup_buffer,
            ..
        } = &mut self.memory;
        let scaled = scaled
            .get_mut(point_range.clone())
            .ok_or(InsufficientMemory)?;
        let flags = flags.get_mut(point_range).ok_or(InsufficientMemory)?;
        let unscaled = unscaled
            .get_mut(..other_points_end)
            .ok_or(InsufficientMemory)?;
        glyph.read_points_fast(&mut unscaled[..point_count], &mut flags[..point_count])?;
        let contours_start = self.contour_count;
        let contour_count = glyph.contour_count();
        let contours_end = contours_start + contour_count;
        let contours = contours
            .get_mut(contours_start..contours_end)
            .ok_or(InsufficientMemory)?;
        let mut last_end_pt = 0;
        for (i, contour) in contours.iter_mut().enumerate() {
            let end_pt = glyph.end_pt(i);
            if end_pt < last_end_pt {
                return Err(DrawError::Read);
            }
            last_end_pt = end_pt;
            *contour = end_pt;
        }
        self.point_count += point_count;
        self.contour_count += contour_count;
        for (i, phantom) in self.phantom.iter().enumerate() {
            unscaled[phantom_start + i] = phantom.map(|x| x.to_bits());
            flags[phantom_start + i] = Default::default();
        }
        let mut have_deltas = false;
        if let (Some(gvar), false) = (self.outlines.gvar.as_ref(), self.coords.is_empty()) {
            let glyph = deltas::SimpleGlyph {
                points: &unscaled[..],
                flags: &mut flags[..],
                contours,
            };
            let deltas = deltas
                .get_mut(..point_count + PHANTOM_POINT_COUNT)
                .ok_or(InsufficientMemory)?;
            let iup_buffer = iup_buffer
                .get_mut(..point_count + PHANTOM_POINT_COUNT)
                .ok_or(InsufficientMemory)?;
            if deltas::simple_glyph(gvar, glyph_id, self.coords, glyph, iup_buffer, deltas).is_ok()
            {
                have_deltas = true;
            }
        }
        if have_deltas {
            for (unscaled, delta) in unscaled.iter_mut().zip(deltas.iter()) {
                *unscaled += delta.map(Fixed::to_i32);
            }
        }
        for (point, unscaled) in scaled.iter_mut().zip(unscaled.iter()) {
            *point = unscaled.map(F26Dot6::from_i32);
        }
        self.phantom.copy_from_slice(&scaled[phantom_start..]);
        if points_start != 0 {
            for contour_end in contours.iter_mut() {
                *contour_end += points_start as u16;
            }
        }
        Ok(())
    }

    fn load_composite(
        &mut self,
        glyph: &CompositeGlyph,
        glyph_id: u16,
        recurse_depth: usize,
    ) -> Result<(), DrawError> {
        use DrawError::InsufficientMemory;
        let point_base = self.point_count;
        let mut have_deltas = false;
        let delta_base = self.component_delta_count;
        if let (Some(gvar), false) = (self.outlines.gvar.as_ref(), self.coords.is_empty()) {
            let count = glyph.components().count() + PHANTOM_POINT_COUNT;
            let deltas = self
                .memory
                .composite_deltas
                .get_mut(delta_base..delta_base + count)
                .ok_or(InsufficientMemory)?;
            if deltas::composite_glyph(gvar, glyph_id, self.coords, &mut deltas[..]).is_ok() {
                for (phantom, delta) in self
                    .phantom
                    .iter_mut()
                    .zip(&deltas[deltas.len() - PHANTOM_POINT_COUNT..])
                {
                    *phantom += delta.map(Fixed::to_i32).map(F26Dot6::from_bits);
                }
                have_deltas = true;
                self.component_delta_count += count;
            }
        }
        for point in self.phantom.iter_mut() {
            *point = point.map(|x| F26Dot6::from_i32(x.to_bits()));
        }
        for (i, component) in glyph.components().enumerate() {
            let phantom = self.phantom;
            let start_point = self.point_count;
            let component_glyph = self.outlines.glyf.get(component.glyph)?;
            self.load(&component_glyph, component.glyph, recurse_depth + 1)?;
            let end_point = self.point_count;
            if component.flags & USE_MY_METRICS == 0 {
                self.phantom = phantom;
            }
            fn scale_component(x: F2Dot14) -> Fixed {
                Fixed::from_bits(x.to_bits() as i32 * 4)
            }
            let xform = &component.transform;
            let xx = scale_component(xform.xx);
            let yx = scale_component(xform.yx);
            let xy = scale_component(xform.xy);
            let yy = scale_component(xform.yy);
            let have_xform = component.flags
                & (WE_HAVE_A_SCALE | WE_HAVE_AN_X_AND_Y_SCALE | WE_HAVE_A_TWO_BY_TWO)
                != 0;
            if have_xform {
                let scaled = self
                    .memory
                    .scaled
                    .get_mut(start_point..end_point)
                    .ok_or(InsufficientMemory)?;
                for point in scaled {
                    let unscaled = point.map(|c| Fixed::from_bits(c.to_i32()));
                    let x = unscaled.x * xx + unscaled.y * xy;
                    let y = unscaled.x * yx + unscaled.y * yy;
                    *point = Point::new(x, y).map(|c| F26Dot6::from_i32(c.to_bits()));
                }
            }
            let anchor_offset = match component.anchor {
                Anchor::Offset { x, y } => {
                    let (mut x, mut y) = (i32::from(x), i32::from(y));
                    if have_xform
                        && component.flags & (SCALED_COMPONENT_OFFSET | UNSCALED_COMPONENT_OFFSET)
                            == SCALED_COMPONENT_OFFSET
                    {
                        fn hypot(a: Fixed, b: Fixed) -> Fixed {
                            let a = a.to_bits().abs();
                            let b = b.to_bits().abs();
                            Fixed::from_bits(if a > b {
                                a + ((3 * b) >> 3)
                            } else {
                                b + ((3 * a) >> 3)
                            })
                        }
                        x = (Fixed::from_bits(x) * hypot(xx, xy)).to_bits();
                        y = (Fixed::from_bits(y) * hypot(yy, yx)).to_bits();
                    }
                    if have_deltas {
                        let delta = self
                            .memory
                            .composite_deltas
                            .get(delta_base + i)
                            .copied()
                            .unwrap_or_default();
                        x += delta.x.to_i32();
                        y += delta.y.to_i32();
                    }
                    Point::new(x, y).map(F26Dot6::from_i32)
                }
                Anchor::Point { base, component } => {
                    let (base_offset, component_offset) = (base as usize, component as usize);
                    let base_point = self
                        .memory
                        .scaled
                        .get(point_base + base_offset)
                        .ok_or(DrawError::InvalidAnchorPoint)?;
                    let component_point = self
                        .memory
                        .scaled
                        .get(start_point + component_offset)
                        .ok_or(DrawError::InvalidAnchorPoint)?;
                    *base_point - *component_point
                }
            };
            if anchor_offset.x != F26Dot6::ZERO || anchor_offset.y != F26Dot6::ZERO {
                for point in self
                    .memory
                    .scaled
                    .get_mut(start_point..end_point)
                    .ok_or(InsufficientMemory)?
                {
                    *point += anchor_offset;
                }
            }
        }
        if have_deltas {
            self.component_delta_count = delta_base;
        }
        Ok(())
    }
}

impl From<ReadError> for DrawError {
    fn from(_: ReadError) -> Self {
        DrawError::Read
    }
}

#[cfg(test)]
#[path = "glyf_bounds_tests.rs"]
mod bounds_tests;
