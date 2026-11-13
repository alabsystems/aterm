// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ComposedWitnessRows, captured_witness_neighbor, needs_fresh_witness_read};

#[test]
fn composed_witness_rows_come_from_the_capture_or_a_fresh_read() {
    let above = ['═', 'A'];
    let below = [' ', 'B'];
    let none = ComposedWitnessRows::default();
    let (a, b) = (Some(&above[..]), Some(&below[..]));
    assert_eq!(captured_witness_neighbor(4, 5, a, b), a);
    assert_eq!(captured_witness_neighbor(6, 5, a, b), b);
    assert_eq!(captured_witness_neighbor(8, 5, a, b), None);
    assert_eq!(captured_witness_neighbor(4, 5, None, b), None);
    assert!(!needs_fresh_witness_read(&[4, 5, 6], 5, 4, 4, a, b, &none));
    assert!(needs_fresh_witness_read(
        &[4, 5, 6, 7],
        5,
        4,
        4,
        a,
        b,
        &none
    ));
    assert!(needs_fresh_witness_read(
        &[4, 5, 6],
        5,
        4,
        4,
        None,
        b,
        &none
    ));
}
