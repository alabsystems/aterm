// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CtChoice, U256};

fn uint_with_bits_at(positions: &[usize]) -> U256 {
    let mut result = U256::ZERO;
    for pos in positions {
        result |= U256::ONE << *pos;
    }
    result
}

#[test]
fn bit_vartime() {
    let u = uint_with_bits_at(&[16, 48, 112, 127, 255]);
    assert!(!u.bit_vartime(0));
    assert!(!u.bit_vartime(1));
    assert!(u.bit_vartime(16));
    assert!(u.bit_vartime(127));
    assert!(u.bit_vartime(255));
    assert!(!u.bit_vartime(256));
    assert!(!u.bit_vartime(260));
}

#[test]
fn bit() {
    let u = uint_with_bits_at(&[16, 48, 112, 127, 255]);
    assert!(!u.bit(0).is_true_vartime());
    assert!(!u.bit(1).is_true_vartime());
    assert!(u.bit(16).is_true_vartime());
    assert!(u.bit(127).is_true_vartime());
    assert!(u.bit(255).is_true_vartime());
    assert!(!u.bit(256).is_true_vartime());
    assert!(!u.bit(260).is_true_vartime());
}

#[test]
fn leading_zeros() {
    let u = uint_with_bits_at(&[256 - 16, 256 - 79, 256 - 207]);
    assert_eq!(u.leading_zeros(), 15);

    let u = uint_with_bits_at(&[256 - 79, 256 - 207]);
    assert_eq!(u.leading_zeros(), 78);

    let u = uint_with_bits_at(&[256 - 207]);
    assert_eq!(u.leading_zeros(), 206);

    let u = uint_with_bits_at(&[256 - 1, 256 - 75, 256 - 150]);
    assert_eq!(u.leading_zeros(), 0);

    let u = U256::ZERO;
    assert_eq!(u.leading_zeros(), 256);
}

#[test]
fn leading_zeros_vartime() {
    let u = uint_with_bits_at(&[256 - 16, 256 - 79, 256 - 207]);
    assert_eq!(u.leading_zeros_vartime(), 15);

    let u = uint_with_bits_at(&[256 - 79, 256 - 207]);
    assert_eq!(u.leading_zeros_vartime(), 78);

    let u = uint_with_bits_at(&[256 - 207]);
    assert_eq!(u.leading_zeros_vartime(), 206);

    let u = uint_with_bits_at(&[256 - 1, 256 - 75, 256 - 150]);
    assert_eq!(u.leading_zeros_vartime(), 0);

    let u = U256::ZERO;
    assert_eq!(u.leading_zeros_vartime(), 256);
}

#[test]
fn trailing_zeros() {
    let u = uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(u.trailing_zeros(), 16);

    let u = uint_with_bits_at(&[79, 150]);
    assert_eq!(u.trailing_zeros(), 79);

    let u = uint_with_bits_at(&[150, 207]);
    assert_eq!(u.trailing_zeros(), 150);

    let u = uint_with_bits_at(&[0, 150, 207]);
    assert_eq!(u.trailing_zeros(), 0);

    let u = U256::ZERO;
    assert_eq!(u.trailing_zeros(), 256);
}

#[test]
fn trailing_zeros_vartime() {
    let u = uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(u.trailing_zeros_vartime(), 16);

    let u = uint_with_bits_at(&[79, 150]);
    assert_eq!(u.trailing_zeros_vartime(), 79);

    let u = uint_with_bits_at(&[150, 207]);
    assert_eq!(u.trailing_zeros_vartime(), 150);

    let u = uint_with_bits_at(&[0, 150, 207]);
    assert_eq!(u.trailing_zeros_vartime(), 0);

    let u = U256::ZERO;
    assert_eq!(u.trailing_zeros_vartime(), 256);
}

#[test]
fn trailing_ones() {
    let u = !uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(u.trailing_ones(), 16);

    let u = !uint_with_bits_at(&[79, 150]);
    assert_eq!(u.trailing_ones(), 79);

    let u = !uint_with_bits_at(&[150, 207]);
    assert_eq!(u.trailing_ones(), 150);

    let u = !uint_with_bits_at(&[0, 150, 207]);
    assert_eq!(u.trailing_ones(), 0);

    let u = U256::MAX;
    assert_eq!(u.trailing_ones(), 256);
}

#[test]
fn trailing_ones_vartime() {
    let u = !uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(u.trailing_ones_vartime(), 16);

    let u = !uint_with_bits_at(&[79, 150]);
    assert_eq!(u.trailing_ones_vartime(), 79);

    let u = !uint_with_bits_at(&[150, 207]);
    assert_eq!(u.trailing_ones_vartime(), 150);

    let u = !uint_with_bits_at(&[0, 150, 207]);
    assert_eq!(u.trailing_ones_vartime(), 0);

    let u = U256::MAX;
    assert_eq!(u.trailing_ones_vartime(), 256);
}

#[test]
fn set_bit() {
    let u = uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(
        u.set_bit(127, CtChoice::TRUE),
        uint_with_bits_at(&[16, 79, 127, 150])
    );

    let u = uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(
        u.set_bit(150, CtChoice::TRUE),
        uint_with_bits_at(&[16, 79, 150])
    );

    let u = uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(
        u.set_bit(127, CtChoice::FALSE),
        uint_with_bits_at(&[16, 79, 150])
    );

    let u = uint_with_bits_at(&[16, 79, 150]);
    assert_eq!(
        u.set_bit(150, CtChoice::FALSE),
        uint_with_bits_at(&[16, 79])
    );
}
