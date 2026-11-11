// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from font-types 0.12.4 src/fixed.rs and src/point.rs and modified by
// the aterm project in 2026: the 16.16, 26.6 and 2.14 types and the point
// operations the autohinter and its outline loader use, nothing else.
// See ../../LICENSE-FONTATIONS for the upstream license.

use super::*;

#[test]
fn round_fixed() {
    assert_eq!(Fixed(0x0001_7FFE).round(), Fixed(0x0001_0000));
    assert_eq!(Fixed(0x0001_7FFF).round(), Fixed(0x0001_0000));
    assert_eq!(Fixed(0x0001_8000).round(), Fixed(0x0002_0000));
}

#[test]
fn fixed_floats() {
    assert_eq!(Fixed(0x7fff_0000), Fixed::from_f64(32767.));
    assert_eq!(Fixed(0x7000_0001), Fixed::from_f64(28672.00001525879));
    assert_eq!(Fixed(0x0001_0000), Fixed::from_f64(1.0));
    assert_eq!(Fixed(0x0000_0000), Fixed::from_f64(0.0));
    assert_eq!(
        Fixed(i32::from_be_bytes([0xff; 4])),
        Fixed::from_f64(-0.000015259)
    );
    assert_eq!(Fixed(0x7fff_ffff), Fixed::from_f64(32768.0));
}

#[test]
fn fixed_floats_rounding() {
    fn with_round_intrinsic(x: f64) -> Fixed {
        Fixed((x * 65536.0).round() as i32)
    }
    let inputs = [0.05, 0.6, 0.2, 0.4, 0.67755];
    for input in inputs {
        assert_eq!(Fixed::from_f64(input), with_round_intrinsic(input));
        assert_eq!(Fixed::from_f64(-input), with_round_intrinsic(-input));
    }
}

#[test]
fn fixed_to_int() {
    assert_eq!(Fixed::from_f64(1.0).to_i32(), 1);
    assert_eq!(Fixed::from_f64(1.5).to_i32(), 2);
}

#[test]
fn fixed_from_int() {
    assert_eq!(Fixed::from_i32(1000).to_bits(), 1000 << 16);
    assert_eq!(F26Dot6::from_i32(1000).to_bits(), 1000 << 6);
}

#[test]
fn fixed_muldiv() {
    assert_eq!(
        Fixed::from_f64(0.5) * Fixed::from_f64(2.0),
        Fixed::from_f64(1.0)
    );
    assert_eq!(
        Fixed::from_f64(0.5) / Fixed::from_f64(2.0),
        Fixed::from_f64(0.25)
    );
}

#[test]
fn fixed_mul_div_rounds_once_and_keeps_the_sign() {
    for (value, multiplier, divisor, expected) in [
        (1, 1, 1, 1),
        (1, 1, 2, 1),
        (-1, 1, 2, -1),
        (1, -1, 2, -1),
        (1, 1, -2, -1),
        (-1, -1, -2, -1),
        (2, 3, 4, 2),
        (6, -3, -2, 9),
    ] {
        assert_eq!(
            Fixed::from_bits(value)
                .mul_div(Fixed::from_bits(multiplier), Fixed::from_bits(divisor))
                .to_bits(),
            expected,
            "{value} * {multiplier} / {divisor}"
        );
    }
}

#[test]
fn fixed_mul_div_handles_signed_extremes_and_zero_divisors() {
    for (value, multiplier, divisor, expected) in [
        (i32::MIN, 1, 1, i32::MIN),
        (i32::MIN, 1, -1, i32::MIN),
        (i32::MIN, -1, 2, 1 << 30),
        (i32::MAX, i32::MAX, i32::MAX, i32::MAX),
        (i32::MIN, i32::MIN, i32::MIN, i32::MIN),
        (i32::MAX, i32::MAX, 1, 1),
        (1, 1, 0, i32::MAX),
        (-1, 1, 0, -i32::MAX),
        (0, 0, 0, i32::MAX),
    ] {
        assert_eq!(
            Fixed::from_bits(value)
                .mul_div(Fixed::from_bits(multiplier), Fixed::from_bits(divisor))
                .to_bits(),
            expected,
            "{value} * {multiplier} / {divisor}"
        );
    }
}

#[test]
fn fixed_div_neg_overflow() {
    let a = Fixed::from_f64(-92.5);
    let b = Fixed::from_f64(0.0028228759765625);
    let _ = a / b;
}

#[test]
fn fixed_div_min_value() {
    let min = Fixed(i32::MIN);
    let _ = min / Fixed::ONE;
    let _ = min / Fixed(-Fixed::ONE.0);
}

#[test]
fn fixed_abs_min_value() {
    assert_eq!(Fixed(i32::MIN).abs(), Fixed(i32::MIN));
}

#[test]
fn fixed_neg_min_value() {
    assert_eq!(-Fixed(i32::MIN), Fixed(i32::MIN));
}
