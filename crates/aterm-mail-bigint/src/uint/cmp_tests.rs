// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Integer, U128, Zero};
use core::cmp::Ordering;
use subtle::{ConstantTimeEq, ConstantTimeGreater, ConstantTimeLess};

#[test]
fn is_zero() {
    assert!(bool::from(U128::ZERO.is_zero()));
    assert!(!bool::from(U128::ONE.is_zero()));
    assert!(!bool::from(U128::MAX.is_zero()));
}

#[test]
fn is_odd() {
    assert!(!bool::from(U128::ZERO.is_odd()));
    assert!(bool::from(U128::ONE.is_odd()));
    assert!(bool::from(U128::MAX.is_odd()));
}

#[test]
fn ct_eq() {
    let a = U128::ZERO;
    let b = U128::MAX;

    assert!(bool::from(a.ct_eq(&a)));
    assert!(!bool::from(a.ct_eq(&b)));
    assert!(!bool::from(b.ct_eq(&a)));
    assert!(bool::from(b.ct_eq(&b)));
}

#[test]
fn ct_gt() {
    let a = U128::ZERO;
    let b = U128::ONE;
    let c = U128::MAX;

    assert!(bool::from(b.ct_gt(&a)));
    assert!(bool::from(c.ct_gt(&a)));
    assert!(bool::from(c.ct_gt(&b)));

    assert!(!bool::from(a.ct_gt(&a)));
    assert!(!bool::from(b.ct_gt(&b)));
    assert!(!bool::from(c.ct_gt(&c)));

    assert!(!bool::from(a.ct_gt(&b)));
    assert!(!bool::from(a.ct_gt(&c)));
    assert!(!bool::from(b.ct_gt(&c)));
}

#[test]
fn ct_lt() {
    let a = U128::ZERO;
    let b = U128::ONE;
    let c = U128::MAX;

    assert!(bool::from(a.ct_lt(&b)));
    assert!(bool::from(a.ct_lt(&c)));
    assert!(bool::from(b.ct_lt(&c)));

    assert!(!bool::from(a.ct_lt(&a)));
    assert!(!bool::from(b.ct_lt(&b)));
    assert!(!bool::from(c.ct_lt(&c)));

    assert!(!bool::from(b.ct_lt(&a)));
    assert!(!bool::from(c.ct_lt(&a)));
    assert!(!bool::from(c.ct_lt(&b)));
}

#[test]
fn cmp() {
    let a = U128::ZERO;
    let b = U128::ONE;
    let c = U128::MAX;

    assert_eq!(a.cmp(&b), Ordering::Less);
    assert_eq!(a.cmp(&c), Ordering::Less);
    assert_eq!(b.cmp(&c), Ordering::Less);

    assert_eq!(a.cmp(&a), Ordering::Equal);
    assert_eq!(b.cmp(&b), Ordering::Equal);
    assert_eq!(c.cmp(&c), Ordering::Equal);

    assert_eq!(b.cmp(&a), Ordering::Greater);
    assert_eq!(c.cmp(&a), Ordering::Greater);
    assert_eq!(c.cmp(&b), Ordering::Greater);
}

#[test]
fn cmp_vartime() {
    let a = U128::ZERO;
    let b = U128::ONE;
    let c = U128::MAX;

    assert_eq!(a.cmp_vartime(&b), Ordering::Less);
    assert_eq!(a.cmp_vartime(&c), Ordering::Less);
    assert_eq!(b.cmp_vartime(&c), Ordering::Less);

    assert_eq!(a.cmp_vartime(&a), Ordering::Equal);
    assert_eq!(b.cmp_vartime(&b), Ordering::Equal);
    assert_eq!(c.cmp_vartime(&c), Ordering::Equal);

    assert_eq!(b.cmp_vartime(&a), Ordering::Greater);
    assert_eq!(c.cmp_vartime(&a), Ordering::Greater);
    assert_eq!(c.cmp_vartime(&b), Ordering::Greater);
}
