// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

#[test]
fn accrual_is_rate_capped_so_key_repeat_cannot_outrun_typing() {
    let t0 = Instant::now();
    let run = |gap_ms: u64, steps: u32| {
        let mut m = TypingMomentum::default();
        m.set_value(t0, 0.0);
        let dt = Duration::from_millis(gap_ms);
        let mut t = t0;
        for _ in 0..steps {
            t += dt;
            m.advance(t);
        }
        m.value(t)
    };
    let geometric = |dt_s: f32, total_s: f32| {
        let num = 1.0 - crate::color_math::exp_f32(-total_s / TYPING_MOMENTUM_TAU);
        let den = 1.0 - crate::color_math::exp_f32(-dt_s / TYPING_MOMENTUM_TAU);
        (TYPING_MOMENTUM_RATE * dt_s * (num / den)).min(1.0)
    };
    let flood = run(5, 200);
    let sprint = run(25, 40);
    assert!(
        (flood - geometric(0.005, 1.0)).abs() < 1e-4,
        "flood {flood} disagrees with closed-form geometric series"
    );
    assert!(
        (sprint - geometric(0.025, 1.0)).abs() < 1e-4,
        "sprint {sprint} disagrees with closed-form geometric series"
    );
    assert!(
        flood <= sprint,
        "key-repeat flood {flood} must never outrun human sprint {sprint} over 1.0 s"
    );
    let continuous_floor = TYPING_MOMENTUM_RATE
        * TYPING_MOMENTUM_TAU
        * (1.0 - crate::color_math::exp_f32(-1.0 / TYPING_MOMENTUM_TAU));
    let discrete_ceiling = geometric(TYPING_MOMENTUM_CREDIT_CAP_S, 1.0);
    assert!(
        flood >= continuous_floor && sprint <= discrete_ceiling,
        "flood {flood} / sprint {sprint} outside [{continuous_floor}, {discrete_ceiling}]"
    );
}

#[test]
fn credit_cannot_be_banked_across_a_pause() {
    let t0 = Instant::now();
    let mut m = TypingMomentum::default();
    m.advance(t0);
    let first = m.value(t0);
    m.advance(t0 + Duration::from_secs(10));
    let after_pause = m.value(t0 + Duration::from_secs(10));
    let cap = TYPING_MOMENTUM_RATE * TYPING_MOMENTUM_CREDIT_CAP_S;
    assert!(first <= cap + 1e-4, "a cold first key is one capped credit");
    assert!(
        after_pause <= cap + 0.01,
        "10 s of silence buys no banked credit: {after_pause}"
    );
}

#[test]
fn low_crossing_predicts_the_analytic_decay_instant() {
    let t0 = Instant::now();
    let mut m = TypingMomentum::default();
    m.set_value(t0, 0.9);
    let cross = m.low_crossing(0.14).expect("stamped metric has a crossing");
    assert!(
        (m.value(cross) - 0.14).abs() < 0.005,
        "the metric reads the threshold at its predicted crossing"
    );
    m.set_value(t0, 0.1);
    assert_eq!(m.low_crossing(0.14), Some(t0));
    assert_eq!(TypingMomentum::default().low_crossing(0.14), None);
    m.set_value(t0, 0.9);
    assert_eq!(m.low_crossing(0.0), None);
    assert_eq!(m.low_crossing(-0.5), None);
    assert_eq!(m.low_crossing(f32::NAN), None);
    assert_eq!(m.low_crossing(f32::INFINITY), None);
    let _ = m.low_crossing(f32::from_bits(1));
}

#[test]
fn reading_crossing_reaches_the_actual_floor_including_the_zero_snap() {
    let at = Instant::now();
    for (value, floor) in [
        (0.000_75, 0.0),
        (0.000_75, 0.000_1),
        (0.000_75, 0.000_49),
        (0.000_75, SNAP_ZERO),
        (0.000_75, 0.000_7),
        (SNAP_ZERO, 0.0),
        (0.4, 0.3),
        (1.0, 0.0),
    ] {
        let mut metric = TypingMomentum::default();
        metric.set_value(at, value);
        let crossing = metric.reading_low_crossing(floor).unwrap();
        assert!(metric.value(at) > floor);
        assert!(
            crossing > at,
            "an unmet floor must not re-arm the same instant"
        );
        assert!(metric.value(crossing) <= floor, "{value} -> {floor}");
        let analytic = metric.low_crossing(floor.max(SNAP_ZERO)).unwrap();
        assert_eq!(crossing - analytic, Duration::from_millis(1));
    }

    let mut metric = TypingMomentum::default();
    metric.set_value(at, 0.000_75);
    let retired = metric.low_crossing(0.001).unwrap();
    assert_eq!(retired, at);
    assert!(
        metric.value(retired) > 0.0,
        "the old timer fires before dark"
    );

    metric.set_value(at, 0.0);
    assert_eq!(metric.reading_low_crossing(0.0), Some(at));
    metric.set_value(at, 0.000_4);
    assert_eq!(metric.reading_low_crossing(0.0), Some(at));
    metric.set_value(at, 0.3);
    assert_eq!(metric.reading_low_crossing(0.3), Some(at));
    assert_eq!(TypingMomentum::default().reading_low_crossing(0.0), None);
}

#[test]
fn delete_flood_walks_an_earned_run_to_zero() {
    let t0 = Instant::now();
    let mut m = TypingMomentum::default();
    m.set_value(t0, 1.0);
    let mut t = t0;
    for _ in 0..12 {
        t += Duration::from_millis(60);
        m.delete(t);
    }
    assert_eq!(m.value(t), 0.0, "a held delete run fully un-earns");
    t += Duration::from_millis(60);
    m.advance(t);
    assert!(
        m.value(t) <= TYPING_MOMENTUM_RATE * 0.06 + 1e-4,
        "the delete stamped the clock; no banked credit survives"
    );
}
