// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_first_sample_seeds_both_averages_so_a_new_series_is_steady() {
    let mut t = Trend::new();
    t.push(0.8, 1.0);
    let v = t.view();
    assert_eq!((v.level, v.slow, v.raw), (0.8, 0.8, 0.8));
    assert_eq!(v.motion(0.02), Motion::Steady);
    assert_eq!(v.glow_step(0.02), 0);
}

#[test]
fn a_step_up_reads_rising_and_glows_then_settles_steady() {
    let mut t = Trend::new();
    for _ in 0..60 {
        t.push(0.1, 1.0);
    }
    t.push(0.9, 1.0);
    t.push(0.9, 1.0);
    let v = t.view();
    assert!(v.level > v.slow, "{v:?}");
    assert_eq!(v.motion(0.04), Motion::Rising);
    assert!(v.glow_step(0.04) >= 2, "{v:?}");
    for _ in 0..200 {
        t.push(0.9, 1.0);
    }
    let v = t.view();
    assert_eq!(v.motion(0.04), Motion::Steady, "{v:?}");
    assert_eq!(v.glow_step(0.04), 0);
}

#[test]
fn a_step_down_reads_falling_and_never_glows() {
    let mut t = Trend::new();
    for _ in 0..60 {
        t.push(0.9, 1.0);
    }
    for _ in 0..3 {
        t.push(0.1, 1.0);
    }
    let v = t.view();
    assert_eq!(v.motion(0.04), Motion::Falling);
    assert_eq!(v.glow_step(0.04), 0);
    assert_eq!(Motion::Falling.word(), Some("easing"));
}

#[test]
fn the_fast_average_follows_its_time_constant() {
    let mut t = Trend::new();
    t.push(0.0, 1.0);
    t.push(1.0, FAST_TAU_S);
    let want = 1.0 - (-1.0f32).exp();
    assert!((t.view().level - want).abs() < 1e-5, "{:?}", t.view());
}

#[test]
fn a_skipped_sample_is_folded_in_at_its_true_age() {
    let mut a = Trend::new();
    let mut b = Trend::new();
    a.push(0.0, 1.0);
    b.push(0.0, 1.0);
    a.push(1.0, 4.0);
    b.push(1.0, 1.0);
    assert!(a.view().level > b.view().level);
}

#[test]
fn variance_is_the_median_absolute_deviation_of_the_window() {
    assert_eq!(mad(&[]), 0.0);
    assert_eq!(mad(&[5.0]), 0.0);
    assert_eq!(mad(&[1.0, 1.0, 1.0, 1.0]), 0.0);
    assert_eq!(mad(&[1.0, 2.0, 3.0, 4.0, 5.0]), 1.0);
    let mut t = Trend::new();
    for i in 0..30 {
        t.push(if i % 2 == 0 { 0.2 } else { 0.8 }, 1.0);
    }
    assert!((t.view().mad - 0.3).abs() < 1e-6, "{:?}", t.view());
}

#[test]
fn a_jittery_series_needs_a_larger_move_to_read_rising() {
    let calm = TrendView {
        raw: 0.5,
        level: 0.55,
        slow: 0.5,
        mad: 0.0,
    };
    let jittery = TrendView { mad: 0.3, ..calm };
    assert_eq!(calm.motion(0.04), Motion::Rising);
    assert_eq!(jittery.motion(0.04), Motion::Steady);
}

#[test]
fn non_finite_samples_are_ignored() {
    let mut t = Trend::new();
    t.push(f32::NAN, 1.0);
    t.push(0.5, f32::NAN);
    t.push(f32::INFINITY, 1.0);
    assert_eq!(t.view().level, 0.5);
}

const S: u64 = 1_000_000_000;

#[test]
fn a_timed_history_keeps_one_entry_a_second_at_any_cadence() {
    let mut h = History::with_capacity(60);
    for i in 0..120u64 {
        h.push_at(Some(if i % 2 == 0 { 0.2 } else { 0.4 }), 10 * S + i * S / 2);
    }
    assert_eq!(h.len(), 60, "120 readings at 2 Hz are the last minute");
    assert!(
        h.to_vec()
            .iter()
            .all(|v| v.is_some_and(|x| (x - 0.3).abs() < 1e-6))
    );
    let mut h = History::with_capacity(60);
    for i in 0..30u64 {
        h.push_at(Some(0.5), 10 * S + i * S);
    }
    assert_eq!(h.len(), 30);
}

#[test]
fn a_timed_history_fills_a_short_silence_and_gaps_a_long_one() {
    let mut h = History::with_capacity(60);
    h.push_at(Some(0.1), 10 * S);
    h.push_at(Some(0.7), 15 * S);
    assert_eq!(
        h.to_vec(),
        vec![
            Some(0.1),
            Some(0.7),
            Some(0.7),
            Some(0.7),
            Some(0.7),
            Some(0.7)
        ]
    );
    let mut h = History::with_capacity(60);
    h.push_at(Some(0.1), 10 * S);
    h.push_at(Some(0.7), 100 * S);
    let v = h.to_vec();
    assert_eq!(
        v.len(),
        60,
        "the gap fills the whole minute, then the reading"
    );
    assert_eq!(v.last(), Some(&Some(0.7)));
    assert!(v[..59].iter().all(Option::is_none));
}
