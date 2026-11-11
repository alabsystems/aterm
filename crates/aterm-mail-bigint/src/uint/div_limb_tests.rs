// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{Reciprocal, div2by1};
use crate::{Limb, Word};
#[test]
fn div2by1_overflow() {
    // A regression test for a situation when in div2by1() an operation (`q1 + 1`)
    // that is protected from overflowing by a condition in the original paper (`r >= d`)
    // still overflows because we're calculating the results for both branches.
    let r = Reciprocal::new(Limb(Word::MAX - 1)).unwrap();
    assert_eq!(
        div2by1(Word::MAX - 2, Word::MAX - 63, &r),
        (Word::MAX, Word::MAX - 65)
    );
}
