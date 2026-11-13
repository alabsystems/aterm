// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/gvar.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use super::*;

struct NewControl {
    deltas_are_words: bool,
    run_count: u8,
}

fn gen_control(control: NewControl) -> u8 {
    assert!(control.run_count > 0, "run count cannot be zero");

    let mut n = 0;
    if control.deltas_are_words {
        n |= 0x80;
    }
    n |= (control.run_count - 1) & 0x7F;
    n
}

#[test]
fn empty() {
    let mut s = Stream::new(&[]);
    assert!(PackedPointsIter::new(&mut s).is_none());
}

#[test]
fn single_zero_control() {
    let mut s = Stream::new(&[0]);
    assert!(PackedPointsIter::new(&mut s).unwrap().is_none());
}

#[test]
fn single_point() {
    let data = vec![
        1,
        gen_control(NewControl {
            deltas_are_words: false,
            run_count: 1,
        }),
        1,
    ];

    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn set_0_and_2() {
    let data = vec![
        2,
        gen_control(NewControl {
            deltas_are_words: false,
            run_count: 2,
        }),
        0,
        2,
    ];

    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn set_1_and_2() {
    let data = vec![
        2,
        gen_control(NewControl {
            deltas_are_words: false,
            run_count: 2,
        }),
        1,
        1,
    ];

    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn set_1_and_3() {
    let data = vec![
        2,
        gen_control(NewControl {
            deltas_are_words: false,
            run_count: 2,
        }),
        1,
        2,
    ];

    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn set_2_5_7() {
    let data = vec![
        3,
        gen_control(NewControl {
            deltas_are_words: false,
            run_count: 3,
        }),
        2,
        3,
        2,
    ];

    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn more_than_127_points() {
    let mut data = vec![];
    data.push(Control::POINTS_ARE_WORDS_FLAG);
    data.push(150);

    data.push(gen_control(NewControl {
        deltas_are_words: false,
        run_count: 100,
    }));
    for _ in 0..100 {
        data.push(2);
    }
    data.push(gen_control(NewControl {
        deltas_are_words: false,
        run_count: 50,
    }));
    for _ in 0..50 {
        data.push(2);
    }
    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), false);
    for _ in 0..150 {
        assert_eq!(iter.next().unwrap(), false);
        assert_eq!(iter.next().unwrap(), true);
    }
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn long_points() {
    let data = vec![
        2,
        gen_control(NewControl {
            deltas_are_words: true,
            run_count: 2,
        }),
        0,
        2,
        0,
        3,
    ];

    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn multiple_runs() {
    let data = vec![
        5,
        gen_control(NewControl {
            deltas_are_words: true,
            run_count: 2,
        }),
        0,
        2,
        0,
        3,
        gen_control(NewControl {
            deltas_are_words: false,
            run_count: 3,
        }),
        2,
        3,
        2,
    ];

    let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
        .unwrap()
        .unwrap();
    let mut iter = SetPointsIter::new(points_iter);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), false);
    assert_eq!(iter.next().unwrap(), true);
    assert_eq!(iter.next().unwrap(), true);
}

#[test]
fn runs_overflow() {
    let data = vec![0xFF; 0xFFFF * 2];
    assert!(PackedPointsIter::new(&mut Stream::new(&data)).is_none());
}
