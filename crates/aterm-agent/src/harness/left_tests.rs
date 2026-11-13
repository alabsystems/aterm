// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// What is said left and what the same reading said used add to 100, `0% left` is a reading
/// of 100 alone, and a bound never says less is left than may be.
///
/// NEGATIVE CONTROL: rounding what is left half up on its own (`(1000 − x + 5) / 10`) says
/// `38% left` of 62.5% used beside the vendor's `63% used`, and `0% left` of 99.6%.
#[test]
fn what_is_left_mirrors_what_was_said_used() {
    for x10 in 0..=1000u32 {
        assert_eq!(exact(x10) + crate::harness::decimal::whole_used(x10), 100);
        assert_eq!(exact(x10) == 0, x10 == 1000, "{x10}");
        assert!(at_most(x10) * 10 >= 1000 - x10, "{x10}: a true upper bound");
        assert!(
            at_most(x10) * 10 < 1000 - x10 + 10,
            "{x10}: the tightest whole one"
        );
        assert!(ahead(x10) >= 1, "{x10}");
    }
    assert_eq!((exact(625), at_most(625)), (37, 38));
    assert_eq!((exact(996), exact(1000), exact(4)), (1, 0, 100));
    assert_eq!((exact(5000), at_most(5000), of_whole(250)), (0, 0, 0));
    assert_eq!((ahead(999), ahead(1000)), (1, 1));
    assert_eq!(
        (words(630), bound_words(629)),
        ("37% left".into(), "\u{2264}38% left".into())
    );
    assert_eq!(
        (tenths(425), tenths(420), tenths(1000)),
        ("57.5".into(), "58".into(), "0".into())
    );
}
