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
