// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const ROW: AosRow = AosRow {
    repairs: 4,
    keys: 8,
    max_p99_us: 8_000,
};

#[test]
fn aos_keeps_its_tail_obligation_and_a_decided_failure_cannot_recover() {
    let row = AosRow { repairs: 40, ..ROW };
    assert_eq!(decision(&row, &[1; 8], true), None);
    assert_eq!(decision(&row, &[1; 39], true), None);
    assert_eq!(decision(&row, &[1; 40], true), Some(ShapeVerdict::Pass));
    assert_eq!(decision(&row, &[8_001; 7], true), None);
    assert_eq!(decision(&row, &[8_001; 8], true), Some(ShapeVerdict::Fail));
    assert_eq!(decision(&row, &[1; 8], false), Some(ShapeVerdict::Fail));
    let mut slow_tail = vec![1; 39];
    slow_tail.push(8_001);
    assert_eq!(decision(&row, &slow_tail, true), Some(ShapeVerdict::Fail));
    let hundred = AosRow {
        repairs: 100,
        ..ROW
    };
    let mut recoverable = vec![1; 7];
    recoverable.push(8_001);
    assert_eq!(decision(&hundred, &recoverable, true), None);
    recoverable.push(8_001);
    assert_eq!(
        decision(&hundred, &recoverable, true),
        Some(ShapeVerdict::Fail)
    );
    recoverable.resize(100, 0);
    assert_eq!(judge(&hundred, &recoverable, true), ShapeVerdict::Fail);
    assert_eq!(decision(&AosRow { repairs: 0, ..ROW }, &[], true), None);
    let mut calls = 0;
    let (values, off) = run_plan(&row, |_| {
        calls += 1;
        Ok((8_001, true))
    })
    .unwrap();
    assert_eq!(calls, MIN_REPAIRS);
    assert_eq!(judge(&row, &values, off), ShapeVerdict::Fail);
    calls = 0;
    let (values, off) = run_plan(&row, |i| {
        calls += 1;
        Ok((if i == row.repairs - 1 { 8_001 } else { 1 }, true))
    })
    .unwrap();
    assert_eq!(calls, 40);
    assert_eq!(judge(&row, &values, off), ShapeVerdict::Fail);
    assert!(
        run_plan(&row, |_| super::super::instance::could_not_run(
            "missing timing"
        ))
        .is_err()
    );
}

#[test]
fn the_nearest_rank_p99_is_the_top_sample_below_a_hundred_and_ignores_order() {
    assert_eq!(p99(&[]), 0);
    assert_eq!(p99(&[7]), 7);
    assert_eq!(p99(&[5, 1, 9, 3]), 9);
    let hundred: Vec<u64> = (1..=100).rev().collect();
    assert_eq!(p99(&hundred), 99);
    let two_hundred: Vec<u64> = (1..=200).collect();
    assert_eq!(p99(&two_hundred), 198);
}

#[test]
fn a_fast_off_main_matrix_passes_and_each_witness_fails_it() {
    assert_eq!(
        judge(&ROW, &[900, 1200, 800, 1000], true),
        ShapeVerdict::Pass
    );
    assert_eq!(
        judge(&ROW, &[900, 1200, 800, 114_500], true),
        ShapeVerdict::Fail,
        "proven red: R0's main-thread repair (about 114.5 ms)"
    );
    assert_eq!(
        judge(&ROW, &[900, 1200, 800, 1000], false),
        ShapeVerdict::Fail,
        "a prepare on the main thread"
    );
    assert_eq!(
        judge(&ROW, &[900, 1200, 800], true),
        ShapeVerdict::Fail,
        "a repair not measured"
    );
    assert_eq!(
        judge(&AosRow { repairs: 0, ..ROW }, &[], true),
        ShapeVerdict::Fail
    );
}
