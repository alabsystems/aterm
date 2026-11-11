// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from read-fonts 0.43.2 src/tables/fvar.rs, src/tables/avar.rs,
// generated/generated_fvar.rs and generated/generated_avar.rs, and from
// skrifa 0.46.2 src/variation.rs and src/instance.rs, and modified by the
// aterm project in 2026: user coordinates to a normalized 2.14 location in
// 16.16 fixed point (`fvar`, then the `avar` segment maps), every read checked.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! A variation instance as the autohinter sees it.

use super::fixed::{F2Dot14, Fixed};

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let b = data.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let b = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

#[derive(Clone, Copy, Debug)]
struct Axis {
    tag: u32,
    min: Fixed,
    default: Fixed,
    max: Fixed,
}

impl Axis {
    fn normalize(&self, mut value: Fixed) -> Fixed {
        use core::cmp::Ordering::*;
        let min_value = self.min;
        let default_value = self.default;
        let max_value = self.max.max(min_value);
        value = value.clamp(min_value, max_value);
        value = match value.cmp(&default_value) {
            Less => {
                -((default_value.saturating_sub(value)) / (default_value.saturating_sub(min_value)))
            }
            Greater => {
                (value.saturating_sub(default_value)) / (max_value.saturating_sub(default_value))
            }
            Equal => Fixed::ZERO,
        };
        value.clamp(Fixed::NEG_ONE, Fixed::ONE)
    }
}

fn fvar_axes(fvar: &[u8]) -> Option<(usize, Vec<Axis>)> {
    if fvar.len() < 16 {
        return None;
    }
    let axis_count = usize::from(read_u16(fvar, 8)?);
    let offset = usize::from(read_u16(fvar, 4)?);
    let records = if offset == 0 {
        &[][..]
    } else {
        fvar.get(offset..)
            .and_then(|d| d.get(..axis_count * 20))
            .unwrap_or_default()
    };
    let fixed = |r: &[u8], o| Fixed::from_bits(read_u32(r, o).unwrap_or_default() as i32);
    let axes = records
        .as_chunks::<20>()
        .0
        .iter()
        .map(|r| Axis {
            tag: read_u32(r, 0).unwrap_or_default(),
            min: fixed(r, 4),
            default: fixed(r, 8),
            max: fixed(r, 12),
        })
        .collect();
    Some((axis_count, axes))
}

fn apply_segment_map(maps: &[u8], coord: Fixed) -> Fixed {
    let len = maps.len() / 4;
    let from =
        |i: usize| F2Dot14::from_bits(read_u16(maps, i * 4).unwrap_or_default() as i16).to_fixed();
    let to_ = |i: usize| {
        F2Dot14::from_bits(read_u16(maps, i * 4 + 2).unwrap_or_default() as i16).to_fixed()
    };
    if len < 2 {
        return if len == 0 {
            coord
        } else {
            coord - from(0) + to_(0)
        };
    }
    let neg1 = Fixed::from_i32(-1);
    let pos1 = Fixed::from_i32(1);
    let mut start = 0usize;
    let mut end = len;
    if from(start) == neg1 && to_(start) == neg1 && from(start + 1) == neg1 {
        start += 1;
    }
    if from(end - 1) == pos1 && to_(end - 1) == pos1 && from(end - 2) == pos1 {
        end -= 1;
    }
    let mut i = start;
    while i < end {
        if coord == from(i) {
            break;
        }
        i += 1;
    }
    if i < end {
        let mut j = i;
        while j + 1 < end && coord == from(j + 1) {
            j += 1;
        }
        if i == j {
            return to_(i);
        }
        if i + 2 == j {
            return to_(i + 1);
        }
        if coord < Fixed::ZERO {
            return to_(j);
        }
        if coord > Fixed::ZERO {
            return to_(i);
        }
        let ti = to_(i);
        let tj = to_(j);
        return if ti.abs() < tj.abs() { ti } else { tj };
    }
    let mut k = start;
    while k < end {
        if coord < from(k) {
            break;
        }
        k += 1;
    }
    if k == start {
        return coord - from(start) + to_(start);
    }
    if k == end {
        return coord - from(end - 1) + to_(end - 1);
    }
    let (bf, bt) = (from(k - 1), to_(k - 1));
    let (af, at) = (from(k), to_(k));
    let denom = af - bf;
    bt + (at - bt).mul_div(coord - bf, denom)
}

fn avar_segment_maps(avar: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut data = if avar.len() < 8 {
        &[][..]
    } else {
        avar.get(8..).unwrap_or_default()
    };
    core::iter::from_fn(move || {
        if data.is_empty() {
            return None;
        }
        let item_len = usize::from(read_u16(data, 0)?) * 4 + 2;
        let item = data.get(..item_len)?;
        data = data.get(item_len..)?;
        item.get(2..)
    })
}

/// The normalized location of user-space `(tag, value)` settings.
pub(crate) fn location(
    fvar: Option<&[u8]>,
    avar: Option<&[u8]>,
    settings: &[(u32, f32)],
) -> Vec<F2Dot14> {
    let Some((axis_count, axes)) = fvar.and_then(fvar_axes) else {
        return Vec::new();
    };
    let actual_len = axes.len().min(axis_count);
    let mut fixed_coords = vec![Fixed::ZERO; actual_len];
    for &(tag, value) in settings {
        let user = Fixed::from_f64(f64::from(value));
        for (axis, coord) in axes
            .iter()
            .zip(fixed_coords.iter_mut())
            .filter(|(axis, _)| axis.tag == tag)
        {
            *coord = axis.normalize(user);
        }
    }
    if let Some(avar) = avar.filter(|avar| avar.len() >= 8) {
        for (coord, map) in fixed_coords.iter_mut().zip(avar_segment_maps(avar)) {
            *coord = apply_segment_map(map, *coord);
        }
    }
    let mut coords = vec![F2Dot14::ZERO; axis_count];
    for (target, coord) in coords.iter_mut().zip(&fixed_coords) {
        *target = coord.to_f2dot14();
    }
    if coords.iter().all(|c| *c == F2Dot14::ZERO) {
        coords.clear();
    }
    coords
}

#[cfg(test)]
#[path = "location_tests.rs"]
mod tests;
