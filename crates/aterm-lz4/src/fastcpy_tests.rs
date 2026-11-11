// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../LICENSE-MIT for the upstream MIT license.

use super::slice_copy;
use alloc::vec::Vec;

#[test]
fn test_fast_short_slice_copy_edge_cases() {
    for len in 0..(512 * 2) {
        let left = (0..len).map(|i| i as u8).collect::<Vec<_>>();
        let mut right = vec![0u8; len];
        slice_copy(&left, &mut right);
        assert_eq!(left, right);
    }
}
