// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{Limb, Zero};
use core::cmp::Ordering;
use subtle::{ConstantTimeEq, ConstantTimeGreater, ConstantTimeLess};

#[test]
fn is_zero() {
    assert!(bool::from(Limb::ZERO.is_zero()));
    assert!(!bool::from(Limb::ONE.is_zero()));
    assert!(!bool::from(Limb::MAX.is_zero()));
}

#[test]
fn is_odd() {
    assert!(!bool::from(Limb::ZERO.is_odd()));
    assert!(bool::from(Limb::ONE.is_odd()));
    assert!(bool::from(Limb::MAX.is_odd()));
}

#[test]
fn ct_eq() {
    let a = Limb::ZERO;
    let b = Limb::MAX;

    assert!(bool::from(a.ct_eq(&a)));
    assert!(!bool::from(a.ct_eq(&b)));
    assert!(!bool::from(b.ct_eq(&a)));
    assert!(bool::from(b.ct_eq(&b)));
}

#[test]
fn ct_gt() {
    let a = Limb::ZERO;
    let b = Limb::ONE;
    let c = Limb::MAX;

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
    let a = Limb::ZERO;
    let b = Limb::ONE;
    let c = Limb::MAX;

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
    assert_eq!(Limb::ZERO.cmp(&Limb::ONE), Ordering::Less);
    assert_eq!(Limb::ONE.cmp(&Limb::ONE), Ordering::Equal);
    assert_eq!(Limb::MAX.cmp(&Limb::ONE), Ordering::Greater);
}
