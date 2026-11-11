// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::CtChoice;
use crate::Word;

#[test]
fn select() {
    let a: Word = 1;
    let b: Word = 2;
    assert_eq!(CtChoice::TRUE.select(a, b), b);
    assert_eq!(CtChoice::FALSE.select(a, b), a);
}
