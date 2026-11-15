// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::due;

/// Counts are reported at the 1st, 2nd, 4th, up to the 256th event and then at each multiple of
/// 256, and at no other.
///
/// Negative control: `n % 255 == 0` for the multiple (due at 255, 510, 765, 1020).
#[test]
fn counts_are_due_at_each_doubling_and_then_each_256th() {
    let due_to_1024: Vec<u64> = (0..=1024).filter(|n| due(*n)).collect();
    assert_eq!(
        due_to_1024,
        [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 768, 1024]
    );
    assert!(due(256 * 1000) && !due(256 * 1000 + 1) && !due(255 * 4));
}
