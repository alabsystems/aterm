// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from read-fonts 0.43.2 (fvar, avar) and skrifa 0.46.2 (variation, instance).
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
mod tests {
    use super::*;

    fn fvar_with(axes: &[(&[u8; 4], f64, f64, f64)]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        t.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset
        t.extend_from_slice(&2u16.to_be_bytes()); // reserved
        t.extend_from_slice(&(axes.len() as u16).to_be_bytes());
        t.extend_from_slice(&20u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        for (tag, min, def, max) in axes {
            t.extend_from_slice(*tag);
            for v in [min, def, max] {
                t.extend_from_slice(&Fixed::from_f64(*v).to_bits().to_be_bytes());
            }
            t.extend_from_slice(&0u16.to_be_bytes());
            t.extend_from_slice(&0u16.to_be_bytes());
        }
        t
    }

    const WGHT: u32 = u32::from_be_bytes(*b"wght");

    #[test]
    fn normalizes_in_fixed_point() {
        let fvar = fvar_with(&[(b"wght", 400.0, 400.0, 900.0)]);
        let coords = location(Some(&fvar), None, &[(WGHT, 700.0)]);
        assert_eq!(coords, [F2Dot14::from_bits(((0x999A + 2) >> 2) as i16)]);
        let fvar = fvar_with(&[(b"wght", 100.0, 400.0, 900.0)]);
        assert_eq!(
            location(Some(&fvar), None, &[(WGHT, 2000.0)]),
            [F2Dot14::from_bits(0x4000)]
        );
        assert_eq!(
            location(Some(&fvar), None, &[(WGHT, 100.0)]),
            [F2Dot14::from_bits(-0x4000)]
        );
    }

    #[test]
    fn default_is_empty() {
        let fvar = fvar_with(&[(b"wght", 400.0, 400.0, 900.0)]);
        assert!(location(Some(&fvar), None, &[]).is_empty());
        assert!(location(Some(&fvar), None, &[(WGHT, 400.0)]).is_empty());
        assert!(location(None, None, &[(WGHT, 700.0)]).is_empty());
        assert!(location(Some(&fvar), None, &[(u32::from_be_bytes(*b"wdth"), 50.0)]).is_empty());
    }

    #[test]
    fn segment_maps() {
        fn map(pairs: &[(f64, f64)]) -> Vec<u8> {
            let mut t = Vec::new();
            for (from, to) in pairs {
                let f = (Fixed::from_f64(*from).to_f2dot14()).to_bits();
                let t2 = (Fixed::from_f64(*to).to_f2dot14()).to_bits();
                t.extend_from_slice(&f.to_be_bytes());
                t.extend_from_slice(&t2.to_be_bytes());
            }
            t
        }
        let m = map(&[(-1.0, -1.0), (0.0, 0.0), (0.5, 0.8), (1.0, 1.0)]);
        let at = |c: f64| apply_segment_map(&m, Fixed::from_f64(c)).to_f2dot14();
        assert_eq!(at(0.5), Fixed::from_f64(0.8).to_f2dot14());
        assert_eq!(at(0.0), F2Dot14::ZERO);
        assert_eq!(at(-1.0), Fixed::from_f64(-1.0).to_f2dot14());
        assert_eq!(at(0.75), Fixed::from_f64(0.9).to_f2dot14());
        assert_eq!(
            apply_segment_map(&[], Fixed::from_f64(0.3)),
            Fixed::from_f64(0.3)
        );
        let one = map(&[(0.0, 0.25)]);
        assert_eq!(
            apply_segment_map(&one, Fixed::from_f64(0.5)),
            Fixed::from_f64(0.75)
        );
    }
}
