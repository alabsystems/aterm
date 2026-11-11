// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

#[cfg(feature = "alloc")]
use {super::Limb, alloc::format};

#[cfg(feature = "alloc")]
#[test]
fn debug() {
    #[cfg(target_pointer_width = "32")]
    assert_eq!(format!("{:?}", Limb(42)), "Limb(0x0000002A)");

    #[cfg(target_pointer_width = "64")]
    assert_eq!(format!("{:?}", Limb(42)), "Limb(0x000000000000002A)");
}
