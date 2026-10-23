// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/metrics/blues.rs and modified by the
// aterm project in 2026: reading outlines through aterm-font; its tests need
// fontations' test fonts and are not carried.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Unscaled and scaled Latin/CJK alignment blue zones.

use super::{
    super::{
        fixed::F2Dot14,
        shape::{ShapedCluster, Shaper},
        style::{ScriptGroup, StyleClass},
        unscaled::UnscaledOutlineBuf,
    },
    ScaledWidth,
};

/// Maximum number of blue values.
const MAX_BLUES: usize = 8;

const BLUE_STRING_MAX_LEN: usize = 51;

/// Alignment zone flags associated with a blue value.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
#[repr(transparent)]
pub struct BlueZones(u16);

impl BlueZones {
    pub const NONE: Self = Self(0);
    pub const TOP: Self = Self(1 << 1);
    pub const SUB_TOP: Self = Self(1 << 2);
    pub const NEUTRAL: Self = Self(1 << 3);
    pub const ADJUSTMENT: Self = Self(1 << 4);
    pub const X_HEIGHT: Self = Self(1 << 5);
    pub const LONG: Self = Self(1 << 6);
    pub const HORIZONTAL: Self = Self(1 << 2);
    pub const RIGHT: Self = Self::TOP;

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) fn is_top_like(self) -> bool {
        self & (Self::TOP | Self::SUB_TOP) != Self::NONE
    }

    pub fn is_top(self) -> bool {
        self.contains(Self::TOP)
    }

    pub fn is_sub_top(self) -> bool {
        self.contains(Self::SUB_TOP)
    }

    pub fn is_neutral(self) -> bool {
        self.contains(Self::NEUTRAL)
    }

    pub fn is_x_height(self) -> bool {
        self.contains(Self::X_HEIGHT)
    }

    pub(crate) fn is_long(self) -> bool {
        self.contains(Self::LONG)
    }

    pub(crate) fn is_horizontal(self) -> bool {
        self.contains(Self::HORIZONTAL)
    }

    pub(crate) fn is_right(self) -> bool {
        self.contains(Self::RIGHT)
    }

    #[must_use]
    pub fn retain_top_like_or_neutral(self) -> Self {
        self & (Self::TOP | Self::SUB_TOP | Self::NEUTRAL)
    }
}

impl core::ops::Not for BlueZones {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

impl core::ops::BitOr for BlueZones {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for BlueZones {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::ops::BitAnd for BlueZones {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl core::ops::BitAndAssign for BlueZones {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

/// Unscaled alignment zone.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct UnscaledBlue {
    /// Reference position.
    pub position: i32,
    /// Overshoot position.
    pub overshoot: i32,
    /// Maximum outline extent.
    pub ascender: i32,
    /// Minimum outline extent.
    pub descender: i32,
    /// Active zone flags.
    pub zones: BlueZones,
}

pub(crate) type UnscaledBlues = Vec<UnscaledBlue>;

/// Scaled alignment zone.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct ScaledBlue {
    /// Scaled reference position.
    pub position: ScaledWidth,
    /// Scaled overshoot position.
    pub overshoot: ScaledWidth,
    /// Active zone flags.
    pub zones: BlueZones,
    /// Whether this blue zone is active at the current scale.
    pub is_active: bool,
}

pub(crate) type ScaledBlues = Vec<ScaledBlue>;

/// Computes unscaled blue values for each axis of `style`.
pub(crate) fn compute_unscaled_blues(
    shaper: &Shaper,
    coords: &[F2Dot14],
    style: &StyleClass,
) -> [UnscaledBlues; 2] {
    match style.script.group {
        ScriptGroup::Default => [
            Default::default(),
            compute_default_blues(shaper, coords, style),
        ],
        ScriptGroup::Cjk => compute_cjk_blues(shaper, coords, style),
        ScriptGroup::Indic => Default::default(),
    }
}

/// Compute unscaled blue values for the default script set.
fn compute_default_blues(shaper: &Shaper, coords: &[F2Dot14], style: &StyleClass) -> UnscaledBlues {
    let mut blues = UnscaledBlues::new();
    let (mut outline_buf, mut flats, mut rounds) = buffers();
    let font = shaper.font();
    let units_per_em = font.units_per_em();
    let flat_threshold = units_per_em / 14;
    let mut cluster_shaper = shaper.cluster_shaper(style);
    let mut shaped_cluster = ShapedCluster::default();
    for (blue_str, blue_zones) in style.script.blues {
        let mut ascender = i32::MIN;
        let mut descender = i32::MAX;
        let mut n_flats = 0;
        let mut n_rounds = 0;
        if blues.len() >= MAX_BLUES {
            continue;
        }
        for cluster in blue_str.split(' ') {
            let mut best_y_extremum = if blue_zones.is_top() {
                i32::MIN
            } else {
                i32::MAX
            };
            let mut best_is_round = false;
            cluster_shaper.shape(cluster, &mut shaped_cluster);
            for (glyph, y_offset) in shaped_cluster
                .iter()
                .filter(|g| g.id.to_u32() != 0)
                .filter_map(|g| Some((font.outline(g.id)?, g.y_offset)))
            {
                outline_buf.clear();
                if font
                    .draw_unscaled(&glyph, coords, &mut outline_buf)
                    .is_err()
                {
                    continue;
                }
                let outline = outline_buf.as_ref();
                if outline.points.len() <= 2 {
                    continue;
                }
                let mut best_y: Option<i16> = None;
                let best_contour_and_point = if blue_zones.is_top_like() {
                    outline.find_last_contour(|point| {
                        if best_y.is_none() || Some(point.y) > best_y {
                            best_y = Some(point.y);
                            ascender = ascender.max(point.y as i32 + y_offset);
                            true
                        } else {
                            descender = descender.min(point.y as i32 + y_offset);
                            false
                        }
                    })
                } else {
                    outline.find_last_contour(|point| {
                        if best_y.is_none() || Some(point.y) < best_y {
                            best_y = Some(point.y);
                            descender = descender.min(point.y as i32 + y_offset);
                            true
                        } else {
                            ascender = ascender.max(point.y as i32 + y_offset);
                            false
                        }
                    })
                };
                let Some((best_contour_range, best_point_ix)) = best_contour_and_point else {
                    continue;
                };
                let best_contour = &outline.points[best_contour_range];
                let Some(mut best_y) = best_y.map(i32::from) else {
                    continue;
                };
                let best_x = best_contour[best_point_ix].x as i32;
                let [mut on_point_first, mut on_point_last] =
                    if best_contour[best_point_ix].is_on_curve() {
                        [Some(best_point_ix); 2]
                    } else {
                        [None; 2]
                    };
                let mut segment_first = best_point_ix;
                let mut segment_last = best_point_ix;
                for (ix, prev) in cycle_backward(best_contour, best_point_ix) {
                    let dist = (prev.y as i32 - best_y).abs();
                    if dist > 5 && ((prev.x as i32 - best_x).abs() <= (20 * dist)) {
                        break;
                    }
                    segment_first = ix;
                    if prev.is_on_curve() {
                        on_point_first = Some(ix);
                        if on_point_last.is_none() {
                            on_point_last = Some(ix);
                        }
                    }
                }
                let mut next_ix = 0;
                for (ix, next) in cycle_forward(best_contour, best_point_ix) {
                    next_ix = ix;
                    let dist = (next.y as i32 - best_y).abs();
                    if dist > 5 && ((next.x as i32 - best_x).abs() <= (20 * dist)) {
                        break;
                    }
                    segment_last = ix;
                    if next.is_on_curve() {
                        on_point_last = Some(ix);
                        if on_point_first.is_none() {
                            on_point_first = Some(ix);
                        }
                    }
                }
                if blue_zones.is_long() {
                    let length_threshold = units_per_em / 25;
                    let dist = (best_contour[segment_last].x as i32
                        - best_contour[segment_first].x as i32)
                        .abs();
                    if dist < length_threshold
                        && satisfies_min_long_segment_len(
                            segment_first,
                            segment_last,
                            best_contour.len() - 1,
                        )
                    {
                        let height_threshold = units_per_em / 4;
                        let mut prev_ix = best_point_ix;
                        for (ix, prev) in cycle_backward(best_contour, best_point_ix) {
                            if prev.x as i32 != best_x {
                                prev_ix = ix;
                                break;
                            }
                        }
                        if prev_ix == best_point_ix {
                            continue;
                        }
                        let is_ltr = (best_contour[prev_ix].x as i32) < best_x;
                        let mut first = segment_last;
                        let mut last = first;
                        let mut p_first = None;
                        let mut p_last = None;
                        let mut hit = false;
                        loop {
                            if !hit {
                                first = last;
                                if best_contour[first].is_on_curve() {
                                    p_first = Some(first);
                                    p_last = Some(first);
                                } else {
                                    p_first = None;
                                    p_last = None;
                                }
                                hit = true;
                            }
                            if last < best_contour.len() - 1 {
                                last += 1;
                            } else {
                                last = 0;
                            }
                            if (best_y - best_contour[first].y as i32).abs() > height_threshold {
                                hit = false;
                                if last == segment_first {
                                    break;
                                }
                                continue;
                            }
                            let dist =
                                (best_contour[last].y as i32 - best_contour[first].y as i32).abs();
                            if dist > 5
                                && (best_contour[last].x as i32 - best_contour[first].x as i32)
                                    .abs()
                                    <= 20 * dist
                            {
                                hit = false;
                                if last == segment_first {
                                    break;
                                }
                                continue;
                            }
                            if best_contour[last].is_on_curve() {
                                p_last = Some(last);
                                if p_first.is_none() {
                                    p_first = Some(last);
                                }
                            }
                            let first_x = best_contour[first].x as i32;
                            let last_x = best_contour[last].x as i32;
                            let is_cur_ltr = first_x < last_x;
                            let dx = (last_x - first_x).abs();
                            if is_cur_ltr == is_ltr && dx >= length_threshold {
                                loop {
                                    if last < best_contour.len() - 1 {
                                        last += 1;
                                    } else {
                                        last = 0;
                                    }
                                    let dy = (best_contour[last].y as i32
                                        - best_contour[first].y as i32)
                                        .abs();
                                    if dy > 5
                                        && (best_contour[next_ix].x as i32
                                            - best_contour[first].x as i32)
                                            .abs()
                                            <= 20 * dist
                                    {
                                        if last > 0 {
                                            last -= 1;
                                        } else {
                                            last = best_contour.len() - 1;
                                        }
                                        break;
                                    }
                                    p_last = Some(last);
                                    if best_contour[last].is_on_curve() {
                                        p_last = Some(last);
                                        if p_first.is_none() {
                                            p_first = Some(last);
                                        }
                                    }
                                    if last == segment_first {
                                        break;
                                    }
                                }
                                best_y = best_contour[first].y as i32;
                                segment_first = first;
                                segment_last = last;
                                on_point_first = p_first;
                                on_point_last = p_last;
                                break;
                            }
                            if last == segment_first {
                                break;
                            }
                        }
                    }
                }
                best_y += y_offset;
                let is_round = match (on_point_first, on_point_last) {
                    (Some(first), Some(last))
                        if (best_contour[last].x as i32 - best_contour[first].x as i32).abs()
                            > flat_threshold =>
                    {
                        false
                    }
                    _ => {
                        !best_contour[segment_first].is_on_curve()
                            || !best_contour[segment_last].is_on_curve()
                    }
                };
                if is_round && blue_zones.is_neutral() {
                    // Ignore round segments for neutral zone.
                    continue;
                }
                if blue_zones.is_top() {
                    if best_y > best_y_extremum {
                        best_y_extremum = best_y;
                        best_is_round = is_round;
                    }
                } else if best_y < best_y_extremum {
                    best_y_extremum = best_y;
                    best_is_round = is_round;
                }
            }
            if best_y_extremum != i32::MIN && best_y_extremum != i32::MAX {
                if best_is_round {
                    if let Some(round) = rounds.get_mut(n_rounds) {
                        *round = best_y_extremum;
                        n_rounds += 1;
                    }
                } else if let Some(flat) = flats.get_mut(n_flats) {
                    *flat = best_y_extremum;
                    n_flats += 1;
                }
            }
        }
        if n_flats == 0 && n_rounds == 0 {
            continue;
        }
        rounds[..n_rounds].sort_unstable();
        flats[..n_flats].sort_unstable();
        let (mut blue_ref, mut blue_shoot) = if n_flats == 0 {
            let val = rounds[n_rounds / 2];
            (val, val)
        } else if n_rounds == 0 {
            let val = flats[n_flats / 2];
            (val, val)
        } else {
            (flats[n_flats / 2], rounds[n_rounds / 2])
        };
        if blue_shoot != blue_ref {
            let over_ref = blue_shoot > blue_ref;
            if blue_zones.is_top_like() ^ over_ref {
                let val = (blue_shoot + blue_ref) / 2;
                blue_ref = val;
                blue_shoot = val;
            }
        }
        let mut blue = UnscaledBlue {
            position: blue_ref,
            overshoot: blue_shoot,
            ascender,
            descender,
            zones: blue_zones.retain_top_like_or_neutral(),
        };
        if blue_zones.is_x_height() {
            blue.zones |= BlueZones::ADJUSTMENT;
        }
        blues.push(blue);
    }
    let mut sorted_indices: [usize; MAX_BLUES] = core::array::from_fn(|ix| ix);
    let blue_values = blues.as_mut_slice();
    let len = blue_values.len();
    if len == 0 {
        return blues;
    }
    for i in 1..len {
        for j in (1..=i).rev() {
            let first = &blue_values[sorted_indices[j - 1]];
            let second = &blue_values[sorted_indices[j]];
            let a = if first.zones.is_top_like() {
                first.position
            } else {
                first.overshoot
            };
            let b = if second.zones.is_top_like() {
                second.position
            } else {
                second.overshoot
            };
            if b >= a {
                break;
            }
            sorted_indices.swap(j, j - 1);
        }
    }
    for i in 0..len - 1 {
        let index1 = sorted_indices[i];
        let index2 = sorted_indices[i + 1];
        let first = &blue_values[index1];
        let second = &blue_values[index2];
        let a = if first.zones.is_top_like() {
            first.overshoot
        } else {
            first.position
        };
        let b = if second.zones.is_top_like() {
            second.overshoot
        } else {
            second.position
        };
        if a > b {
            if first.zones.is_top_like() {
                blue_values[index1].overshoot = b;
            } else {
                blue_values[index1].position = b;
            }
        }
    }
    blues
}

/// Given inclusive indices and a contour length, returns true if the segment
/// is of sufficient size to test for bumps when detecting "long" Hebrew
/// alignment zones.
fn satisfies_min_long_segment_len(first_ix: usize, last_ix: usize, contour_last: usize) -> bool {
    let inclusive_diff = if first_ix <= last_ix {
        last_ix - first_ix
    } else {
        // last_ix == 0, then we want the result to be 1
        contour_last - first_ix + 1 + last_ix
    };
    inclusive_diff + 2 <= contour_last
}

/// Compute unscaled blue values for the CJK script set.
fn compute_cjk_blues(
    shaper: &Shaper,
    coords: &[F2Dot14],
    style: &StyleClass,
) -> [UnscaledBlues; 2] {
    let mut blues = [UnscaledBlues::new(), UnscaledBlues::new()];
    let (mut outline_buf, mut flats, mut fills) = buffers();
    let font = shaper.font();
    let mut cluster_shaper = shaper.cluster_shaper(style);
    let mut shaped_cluster = ShapedCluster::default();
    for (blue_str, blue_zones) in style.script.blues {
        let is_horizontal = blue_zones.is_horizontal();
        if is_horizontal {
            continue;
        }
        let is_right = blue_zones.is_right();
        let is_top = blue_zones.is_top();
        let blues = &mut blues[!is_horizontal as usize];
        if blues.len() >= MAX_BLUES {
            continue;
        }
        let mut n_flats = 0;
        let mut n_fills = 0;
        let mut is_fill = true;
        for cluster in blue_str.split(' ') {
            // <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afcjk.c#L380>
            if cluster == "|" {
                is_fill = false;
                continue;
            }
            cluster_shaper.shape(cluster, &mut shaped_cluster);
            for glyph in shaped_cluster
                .iter()
                .filter(|g| g.id.to_u32() != 0)
                .filter_map(|g| font.outline(g.id))
            {
                outline_buf.clear();
                if font
                    .draw_unscaled(&glyph, coords, &mut outline_buf)
                    .is_err()
                {
                    continue;
                }
                let outline = outline_buf.as_ref();
                if outline.points.len() <= 2 {
                    continue;
                }
                let best_pos = outline
                    .points
                    .iter()
                    .map(|p| if is_horizontal { p.x } else { p.y })
                    .reduce(
                        if (is_horizontal && is_right) || (!is_horizontal && is_top) {
                            |a: i16, c: i16| a.max(c)
                        } else {
                            |a: i16, c: i16| a.min(c)
                        },
                    );
                let Some(best_pos) = best_pos else {
                    continue;
                };
                if is_fill {
                    if let Some(fill) = fills.get_mut(n_fills) {
                        *fill = best_pos;
                        n_fills += 1;
                    }
                } else if let Some(flat) = flats.get_mut(n_flats) {
                    *flat = best_pos;
                    n_flats += 1;
                }
            }
        }
        if n_flats == 0 && n_fills == 0 {
            continue;
        }
        fills[..n_fills].sort_unstable();
        flats[..n_flats].sort_unstable();
        let (mut blue_ref, mut blue_shoot) = if n_flats == 0 {
            let value = fills[n_fills / 2] as i32;
            (value, value)
        } else if n_fills == 0 {
            let value = flats[n_flats / 2] as i32;
            (value, value)
        } else {
            (fills[n_fills / 2] as i32, flats[n_flats / 2] as i32)
        };
        if blue_shoot != blue_ref {
            let under_ref = blue_shoot < blue_ref;
            if blue_zones.is_top() ^ under_ref {
                blue_ref = (blue_shoot + blue_ref) / 2;
                blue_shoot = blue_ref;
            }
        }
        blues.push(UnscaledBlue {
            position: blue_ref,
            overshoot: blue_shoot,
            ascender: 0,
            descender: 0,
            zones: *blue_zones & BlueZones::TOP,
        });
    }
    blues
}

#[inline(always)]
fn buffers<T: Copy + Default>() -> (
    UnscaledOutlineBuf,
    [T; BLUE_STRING_MAX_LEN],
    [T; BLUE_STRING_MAX_LEN],
) {
    (
        UnscaledOutlineBuf::default(),
        [T::default(); BLUE_STRING_MAX_LEN],
        [T::default(); BLUE_STRING_MAX_LEN],
    )
}

/// Iterates forward cyclically through `items` starting at `start + 1` and ending at `start`.
pub(super) fn cycle_forward<T>(items: &[T], start: usize) -> impl Iterator<Item = (usize, &T)> {
    let len = items.len();
    let start = start + 1;
    (0..len).map(move |ix| {
        let real_ix = (ix + start) % len;
        (real_ix, &items[real_ix])
    })
}

/// Iterates backward cyclically through `items` starting at `start - 1` and ending at `start`.
pub(super) fn cycle_backward<T>(items: &[T], start: usize) -> impl Iterator<Item = (usize, &T)> {
    let len = items.len();
    (0..len).rev().map(move |ix| {
        let real_ix = (ix + start) % len;
        (real_ix, &items[real_ix])
    })
}
