// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cff/index.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use super::*;

#[test]
fn parse_offset_size() {
    assert_eq!(core::mem::size_of::<OffsetSize>(), 1);

    assert_eq!(Stream::new(&[0x00]).read::<OffsetSize>(), None);
    assert_eq!(
        Stream::new(&[0x01]).read::<OffsetSize>(),
        Some(OffsetSize::Size1)
    );
    assert_eq!(Stream::new(&[0x05]).read::<OffsetSize>(), None);
}
