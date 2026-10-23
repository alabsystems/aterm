// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Saturation, clamping, and capacity tests at `u32::MAX`.

use crate::{Rle, Run};

#[test]
fn extend_and_push_clamp_to_the_remaining_capacity() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX);
    rle.extend_with(2, 1);
    rle.push(1);
    rle.push(2);
    assert_eq!(
        rle.runs(),
        &[Run {
            value: 1,
            length: u32::MAX
        }]
    );
    assert_eq!(rle.remaining_capacity(), 0);

    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 10);
    rle.extend_with(1, 20);
    assert_eq!(rle.runs().len(), 1);
    assert_eq!(rle.len(), u32::MAX);
    assert_eq!(rle.get(u32::MAX - 1), Some(1));
}

#[test]
fn try_forms_take_a_whole_request_or_change_nothing() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 5);
    let err = rle.try_extend_with(2, 10).unwrap_err();
    assert_eq!((err.requested, err.available), (10, 5));
    assert_eq!((rle.len(), rle.run_count()), (u32::MAX - 5, 1));

    assert!(
        rle.try_extend_with(1, 4).is_ok(),
        "the same-value merge path"
    );
    assert!(rle.try_push(2).is_ok(), "the new-run path, exactly full");
    assert_eq!((rle.len(), rle.run_count()), (u32::MAX, 2));

    for refused in [rle.try_push(2), rle.try_extend_with(1, 1)] {
        let err = refused.unwrap_err();
        assert_eq!((err.requested, err.available), (1, 0));
    }
    assert_eq!((rle.len(), rle.run_count()), (u32::MAX, 2));
}

#[test]
fn post_clamp_mutations_preserve_invariant() {
    let mut rle = Rle::<u8>::new();
    rle.extend_with(1, u32::MAX - 1);
    rle.extend_with(2, 10); // Clamp to a single trailing cell.

    assert_eq!(
        rle.runs(),
        &[
            Run {
                value: 1,
                length: u32::MAX - 1,
            },
            Run {
                value: 2,
                length: 1,
            },
        ]
    );
    assert_eq!(rle.get(u32::MAX - 2), Some(1));
    assert!(rle.set(u32::MAX - 1, 3));
    assert_eq!(rle.get(u32::MAX - 1), Some(3));

    rle.set_range(u32::MAX - 2, u32::MAX, 4);
    assert_eq!(rle.get(u32::MAX - 2), Some(4));
    assert_eq!(rle.get(u32::MAX - 1), Some(4));

    rle.resize(u32::MAX - 1);
    assert_eq!(rle.len(), u32::MAX - 1);
    assert_eq!(rle.get(u32::MAX - 1), None);

    let run_total: u32 = rle.runs().iter().map(|run| run.length).sum();
    assert_eq!(
        run_total,
        rle.len(),
        "post-clamp mutations must keep total_length exact"
    );
}
