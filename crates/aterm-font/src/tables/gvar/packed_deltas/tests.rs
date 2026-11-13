// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/gvar.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use super::*;

struct NewControl {
    deltas_are_zero: bool,
    deltas_are_words: bool,
    run_count: u8,
}

fn gen_control(control: NewControl) -> u8 {
    assert!(control.run_count > 0, "run count cannot be zero");

    let mut n = 0;
    if control.deltas_are_zero {
        n |= 0x80;
    }
    if control.deltas_are_words {
        n |= 0x40;
    }
    n |= (control.run_count - 1) & 0x3F;
    n
}

#[test]
fn empty() {
    let mut iter = PackedDeltasIter::new(1.0, 1, &[]);
    assert!(iter.next().is_none());
}

#[test]
fn single_delta() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 2,
        }),
        2,
        3,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert_eq!(iter.next().unwrap(), (2.0, 3.0));
    assert!(iter.next().is_none());
}

#[test]
fn two_deltas() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 4,
        }),
        2,
        3,
        4,
        5,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 2, &data);
    assert_eq!(iter.next().unwrap(), (2.0, 4.0));
    assert_eq!(iter.next().unwrap(), (3.0, 5.0));
    assert!(iter.next().is_none());
}

#[test]
fn single_long_delta() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: true,
            run_count: 2,
        }),
        0,
        2,
        0,
        3,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert_eq!(iter.next().unwrap(), (2.0, 3.0));
    assert!(iter.next().is_none());
}

#[test]
fn zeros() {
    let data = vec![gen_control(NewControl {
        deltas_are_zero: true,
        deltas_are_words: false,
        run_count: 4,
    })];

    let mut iter = PackedDeltasIter::new(1.0, 2, &data);
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert!(iter.next().is_none());
}

#[test]
fn zero_words() {
    let data = vec![gen_control(NewControl {
        deltas_are_zero: true,
        deltas_are_words: true,
        run_count: 4,
    })];

    let mut iter = PackedDeltasIter::new(1.0, 2, &data);
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert!(iter.next().is_none());
}

#[test]
fn zero_runs() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: true,
            deltas_are_words: false,
            run_count: 2,
        }),
        gen_control(NewControl {
            deltas_are_zero: true,
            deltas_are_words: false,
            run_count: 4,
        }),
        gen_control(NewControl {
            deltas_are_zero: true,
            deltas_are_words: false,
            run_count: 6,
        }),
    ];

    let mut iter = PackedDeltasIter::new(1.0, 6, &data);
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
    assert!(iter.next().is_none());
}

#[test]
fn delta_after_zeros() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: true,
            deltas_are_words: false,
            run_count: 2,
        }),
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 2,
        }),
        2,
        3,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 2, &data);
    assert_eq!(iter.next().unwrap(), (0.0, 2.0));
    assert_eq!(iter.next().unwrap(), (0.0, 3.0));
    assert!(iter.next().is_none());
}

#[test]
fn unexpected_end_of_data_1() {
    let data = vec![gen_control(NewControl {
        deltas_are_zero: false,
        deltas_are_words: false,
        run_count: 2,
    })];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert!(iter.next().is_none());
}

#[test]
fn unexpected_end_of_data_2() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 2,
        }),
        1,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert!(iter.next().is_none());
}

#[test]
fn unexpected_end_of_data_3() {
    let data = vec![gen_control(NewControl {
        deltas_are_zero: false,
        deltas_are_words: true,
        run_count: 2,
    })];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert!(iter.next().is_none());
}

#[test]
fn unexpected_end_of_data_4() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: true,
            run_count: 2,
        }),
        1,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert!(iter.next().is_none());
}

#[test]
fn unexpected_end_of_data_6() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: true,
            run_count: 2,
        }),
        0,
        1,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert!(iter.next().is_none());
}

#[test]
fn unexpected_end_of_data_7() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: true,
            run_count: 2,
        }),
        0,
        1,
        0,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert!(iter.next().is_none());
}

#[test]
fn single_run() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 1,
        }),
        2,
        3,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 1, &data);
    assert!(iter.next().is_none());
}

#[test]
fn too_many_pairs() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 2,
        }),
        2,
        3,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 10, &data);
    assert!(iter.next().is_none());
}

#[test]
fn invalid_number_of_pairs() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 2,
        }),
        2,
        3,
        4,
        5,
        6,
        7,
    ];

    let mut iter = PackedDeltasIter::new(1.0, 4, &data);
    assert_eq!(iter.next().unwrap(), (2.0, 7.0));
    assert!(iter.next().is_none());
}

#[test]
fn mixed_runs() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 3,
        }),
        2,
        3,
        4,
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: true,
            run_count: 2,
        }),
        0,
        5,
        0,
        6,
        gen_control(NewControl {
            deltas_are_zero: true,
            deltas_are_words: false,
            run_count: 1,
        }),
    ];

    let mut iter = PackedDeltasIter::new(1.0, 3, &data);
    assert_eq!(iter.next().unwrap(), (2.0, 5.0));
    assert_eq!(iter.next().unwrap(), (3.0, 6.0));
    assert_eq!(iter.next().unwrap(), (4.0, 0.0));
    assert!(iter.next().is_none());
}

#[test]
fn non_default_scalar() {
    let data = vec![
        gen_control(NewControl {
            deltas_are_zero: false,
            deltas_are_words: false,
            run_count: 2,
        }),
        2,
        3,
    ];

    let mut iter = PackedDeltasIter::new(0.5, 1, &data);
    assert_eq!(iter.next().unwrap(), (1.0, 1.5));
    assert!(iter.next().is_none());
}

#[test]
fn runs_overflow() {
    let data = vec![0xFF; 0xFFFF];
    let mut iter = PackedDeltasIter::new(1.0, 0xFFFF, &data);
    assert_eq!(iter.next().unwrap(), (0.0, 0.0));
}
