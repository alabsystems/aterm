// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

#[cfg(target_pointer_width = "32")]
#[test]
fn nlimbs_for_bits_macro() {
    assert_eq!(nlimbs!(64), 2);
    assert_eq!(nlimbs!(128), 4);
    assert_eq!(nlimbs!(192), 6);
    assert_eq!(nlimbs!(256), 8);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn nlimbs_for_bits_macro() {
    assert_eq!(nlimbs!(64), 1);
    assert_eq!(nlimbs!(128), 2);
    assert_eq!(nlimbs!(192), 3);
    assert_eq!(nlimbs!(256), 4);
}
