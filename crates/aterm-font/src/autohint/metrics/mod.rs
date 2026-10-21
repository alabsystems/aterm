// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/metrics/mod.rs and modified by the
// aterm project in 2026: metrics computed lazily only, the advance-only digit width dropped.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Autohinting specific metrics.

mod blues;
mod scale;
mod widths;

use super::{
    GlyphId, Target,
    fixed::{F2Dot14, Fixed},
    font::HintFont,
    shape::Shaper,
    style::{GlyphStyleMap, ScriptGroup, StyleClass},
    topo::Dimension,
};
use std::sync::{Arc, RwLock};

pub(crate) use blues::{BlueZones, ScaledBlue, ScaledBlues, UnscaledBlue, UnscaledBlues};
pub(crate) use scale::{compute_unscaled_style_metrics, scale_style_metrics};

/// Maximum number of widths, same for Latin and CJK.
///
/// See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aflatin.h#L65>
/// and <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afcjk.h#L55>
pub(crate) const MAX_WIDTHS: usize = 16;

/// Unscaled metrics for a single axis.
///
/// This is the union of the Latin and CJK axis records.
///
/// See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aflatin.h#L88>
/// and <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afcjk.h#L73>
#[derive(Clone, Default, Debug)]
pub struct UnscaledAxisMetrics {
    /// The dimension of this axis.
    pub dim: Dimension,
    pub(crate) widths: UnscaledWidths,
    /// The unscaled width metrics for the axis.
    pub width_metrics: WidthMetrics,
    pub(crate) blues: UnscaledBlues,
}

impl UnscaledAxisMetrics {
    /// Returns the largest width, if any.
    pub fn max_width(&self) -> Option<i32> {
        self.widths.last().copied()
    }
}

/// Scaled metrics for a single axis.
#[derive(Clone, Default, Debug)]
pub struct ScaledAxisMetrics {
    /// The dimension of this axis.
    pub dim: Dimension,
    /// Font unit to 26.6 scale in the axis direction.
    pub scale: i32,
    /// 1/64 pixel delta in the axis direction.
    pub delta: i32,
    pub(crate) widths: ScaledWidths,
    /// The scaled width metrics.
    pub width_metrics: WidthMetrics,
    pub(crate) blues: ScaledBlues,
}

/// Unscaled metrics for a single style and script.
//
// This is the union of the root, Latin and CJK style metrics but
// the latter two are actually identical.
//
// See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aftypes.h#L413>
// and <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aflatin.h#L109>
// and <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afcjk.h#L95>
#[derive(Clone, Default, Debug)]
pub struct UnscaledStyleMetrics {
    /// Index of style class.
    pub(crate) class_ix: u16,
    /// Per-dimension unscaled metrics.
    pub(crate) axes: [UnscaledAxisMetrics; 2],
}

impl UnscaledStyleMetrics {
    /// Returns the associated style class.
    pub fn style_class(&self) -> &'static StyleClass {
        &super::style::STYLE_CLASSES[self.class_ix as usize]
    }
}

/// The set of unscaled style metrics for a single font, each computed the
/// first time a glyph of its style is drawn.
///
/// For a variable font, this is dependent on the location in variation space.
#[derive(Clone, Debug)]
pub(crate) struct UnscaledStyleMetricsSet(Arc<RwLock<Vec<Option<UnscaledStyleMetrics>>>>);

impl UnscaledStyleMetricsSet {
    /// Creates an unscaled style metrics set where each entry will be
    /// initialized as needed.
    pub fn lazy(style_map: &GlyphStyleMap) -> Self {
        let vec = vec![None; style_map.metrics_count()];
        Self(Arc::new(RwLock::new(vec)))
    }

    /// Returns the unscaled style metrics for the given style map and glyph
    /// identifier.
    pub fn get(
        &self,
        font: &HintFont,
        coords: &[F2Dot14],
        style_map: &GlyphStyleMap,
        glyph_id: GlyphId,
    ) -> Option<UnscaledStyleMetrics> {
        let style = style_map.style(glyph_id)?;
        let index = style_map.metrics_index(style)?;
        let read = self.0.read().ok()?;
        let entry = read.get(index)?;
        if let Some(metrics) = &entry {
            return Some(metrics.clone());
        }
        core::mem::drop(read);
        // The std RwLock doesn't support upgrading and contention is
        // expected to be low, so let's just race to compute the new
        // metrics.
        let shaper = Shaper::new(font);
        let style_class = style.style_class()?;
        let metrics = compute_unscaled_style_metrics(&shaper, coords, style_class);
        let mut entry = self.0.write().ok()?;
        *entry.get_mut(index)? = Some(metrics.clone());
        Some(metrics)
    }
}

/// Scaled metrics for a single style and script.
#[derive(Clone, Default, Debug)]
pub struct ScaledStyleMetrics {
    /// Multidimensional scaling factors and deltas.
    pub scale: Scale,
    /// Per-dimension scaled metrics.
    pub(crate) axes: [ScaledAxisMetrics; 2],
}

/// Metrics for the set of stems along a single axis.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct WidthMetrics {
    /// Used for creating edges.
    pub edge_distance_threshold: i32,
    /// Default stem thickness.
    pub standard_width: i32,
    /// Is standard width very light?
    pub is_extra_light: bool,
}

pub(crate) type UnscaledWidths = Vec<i32>;

/// A scaled stem width.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct ScaledWidth {
    /// Width after applying scale.
    pub scaled: i32,
    /// Grid-fitted width.
    pub fitted: i32,
}

pub(crate) type ScaledWidths = Vec<ScaledWidth>;

/// Flags that define how scaling and hinting is applied.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct ScaleFlags(pub(crate) u32);

// See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aftypes.h#L115>
// and <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aflatin.h#L143>
impl ScaleFlags {
    /// Stem width snapping.
    pub const HORIZONTAL_SNAP: Self = Self(1 << 0);
    /// Stem height snapping.
    pub const VERTICAL_SNAP: Self = Self(1 << 1);
    /// Stem width/height adjustment.
    pub const STEM_ADJUST: Self = Self(1 << 2);
    /// Monochrome rendering.
    pub const MONO: Self = Self(1 << 3);
    /// Disable horizontal hinting.
    pub const NO_HORIZONTAL: Self = Self(1 << 4);
    /// Disable vertical hinting.
    pub const NO_VERTICAL: Self = Self(1 << 5);
    /// Disable advance hinting.
    pub const NO_ADVANCE: Self = Self(1 << 6);
}

impl ScaleFlags {
    /// Returns true if `self` contains all flags in `other`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::Not for ScaleFlags {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

impl core::ops::BitOr for ScaleFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for ScaleFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::ops::BitAnd for ScaleFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl core::ops::BitAndAssign for ScaleFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

/// The largest scale the hinter fits at, in 26.6 units per font unit: 128
/// pixels per unit. An unscaled point is at most 32768 units from the origin
/// (the loaders truncate to 16 bits), so below it every scaled coordinate is
/// within 2^28 and the sums of a few that the fitting adds stay inside an
/// `i32`. [`Scale::new`]'s 16.16 scale wraps at 32768 (512 pixels per unit),
/// and past that the fitting overflowed on outlines near the 16-bit edge
/// (`tests/hint_extremes.rs`).
pub(crate) const MAX_SCALE_26_6: f32 = 8192.0;

/// Whether `size` pixels per em at `units_per_em` is a scale the hinter fits
/// at ([`MAX_SCALE_26_6`]); `false` for a size that is not finite and
/// positive, or a face with no units.
pub(crate) fn scale_fits(size: f32, units_per_em: i32) -> bool {
    units_per_em > 0
        && size.is_finite()
        && size > 0.0
        && size * 64.0 <= MAX_SCALE_26_6 * units_per_em as f32
}

/// Captures scaling parameters which may be modified during metrics
/// computation.
#[derive(Copy, Clone, Default, Debug)]
pub struct Scale {
    /// Flags that determine hinting functionality.
    pub flags: ScaleFlags,
    /// Font unit to 26.6 scale in the X direction.
    pub x_scale: i32,
    /// Font unit to 26.6 scale in the Y direction.
    pub y_scale: i32,
    /// In 1/64 device pixels.
    pub x_delta: i32,
    /// In 1/64 device pixels.
    pub y_delta: i32,
    /// Font size in pixels per em.
    pub size: f32,
    /// From the source font.
    pub units_per_em: i32,
}

impl Scale {
    /// Create initial scaling parameters from metrics and hinting target.
    pub fn new(
        size: f32,
        units_per_em: i32,
        is_italic: bool,
        target: Target,
        group: ScriptGroup,
    ) -> Self {
        let scale =
            (Fixed::from_bits((size * 64.0) as i32) / Fixed::from_bits(units_per_em)).to_bits();
        let mut flags = ScaleFlags::default();
        let is_light = target.is_light();
        // Snap vertical stems for horizontal LCD rendering (and monochrome,
        // a target aterm does not render).
        if target.is_lcd() {
            flags |= ScaleFlags::HORIZONTAL_SNAP;
        }
        // Adjust stems to full pixels unless in LCD or light modes.
        if !(target.is_lcd() || is_light) {
            flags |= ScaleFlags::STEM_ADJUST;
        }
        if group == ScriptGroup::Default {
            // Disable horizontal hinting completely for LCD, light hinting
            // and italic fonts
            // See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/aflatin.c#L2674>
            if target.is_lcd() || is_light || is_italic {
                flags |= ScaleFlags::NO_HORIZONTAL;
            }
        } else {
            // CJK doesn't hint advances
            // See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afcjk.c#L1432>
            flags |= ScaleFlags::NO_ADVANCE;
        }
        Self {
            x_scale: scale,
            y_scale: scale,
            x_delta: 0,
            y_delta: 0,
            size,
            units_per_em,
            flags,
        }
    }
}

// <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afhints.c#L59>
pub(crate) fn sort_and_quantize_widths(widths: &mut UnscaledWidths, threshold: i32) {
    if widths.len() <= 1 {
        return;
    }
    widths.sort_unstable();
    let table = widths.as_mut_slice();
    let mut cur_ix = 0;
    let mut cur_val = table[cur_ix];
    let last_ix = table.len() - 1;
    let mut ix = 1;
    // Compute and use mean values for clusters not larger than
    // `threshold`.
    while ix < table.len() {
        if (table[ix] - cur_val) > threshold || ix == last_ix {
            let mut sum = 0;
            // Fix loop for end of array?
            if (table[ix] - cur_val <= threshold) && ix == last_ix {
                ix += 1;
            }
            for val in &mut table[cur_ix..ix] {
                sum += *val;
                *val = 0;
            }
            table[cur_ix] = sum / ix as i32;
            if ix < last_ix {
                cur_ix = ix + 1;
                cur_val = table[cur_ix];
            }
        }
        ix += 1;
    }
    cur_ix = 1;
    // Compress array to remove zero values
    for ix in 1..table.len() {
        if table[ix] != 0 {
            table[cur_ix] = table[ix];
            cur_ix += 1;
        }
    }
    widths.truncate(cur_ix);
}

// Fixed point helpers
//
// Note: lots of bit fiddling based fixed point math in the autohinter
// so we're opting out of using the strongly typed variants because they
// just add noise and reduce clarity.

pub(crate) fn fixed_mul(a: i32, b: i32) -> i32 {
    (Fixed::from_bits(a) * Fixed::from_bits(b)).to_bits()
}

pub(crate) fn fixed_div(a: i32, b: i32) -> i32 {
    (Fixed::from_bits(a) / Fixed::from_bits(b)).to_bits()
}

pub(crate) fn fixed_mul_div(a: i32, b: i32, c: i32) -> i32 {
    Fixed::from_bits(a)
        .mul_div(Fixed::from_bits(b), Fixed::from_bits(c))
        .to_bits()
}

/// The nearest whole pixel of a 26.6 value (ties up, wrapping at the top).
#[cfg_attr(trust_verify, trust::ensures(result % 64 == 0))]
pub(crate) fn pix_round(a: i32) -> i32 {
    (a.wrapping_add(32)) & !63
}

/// The whole pixel at or below a 26.6 value.
#[cfg_attr(trust_verify, trust::ensures(result % 64 == 0 && result <= a))]
pub(crate) fn pix_floor(a: i32) -> i32 {
    a & !63
}

#[cfg(test)]
mod tests {
    // skrifa 0.46.2 src/outline/autohint/metrics/mod.rs `sort_widths` and
    // `pix_round_extremes_do_not_panic` (its style-set tests need
    // fontations' test fonts).
    use super::*;

    #[test]
    fn sort_widths() {
        // We use 10 and 20 as thresholds because the computation used
        // is units_per_em / 100
        assert_eq!(sort_widths_helper(&[1], 10), &[1]);
        assert_eq!(sort_widths_helper(&[1], 20), &[1]);
        assert_eq!(sort_widths_helper(&[60, 20, 40, 35], 10), &[20, 35, 13, 60]);
        assert_eq!(sort_widths_helper(&[60, 20, 40, 35], 20), &[31, 60]);
    }

    fn sort_widths_helper(widths: &[i32], threshold: i32) -> Vec<i32> {
        let mut widths2 = UnscaledWidths::new();
        for width in widths {
            widths2.push(*width);
        }
        sort_and_quantize_widths(&mut widths2, threshold);
        widths2
    }

    #[test]
    fn pix_round_extremes_do_not_panic() {
        // Just don't panic with overflow
        let _ = pix_round(i32::MIN);
        let _ = pix_round(i32::MAX);
    }
}
