// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn easings_hit_their_ends_and_rise_monotonically() {
    for ease in [ease_out_quad, ease_out_cubic, ease_in_out_cubic] {
        assert_eq!(ease(0), 0);
        assert_eq!(ease(ONE), ONE);
        assert_eq!(ease(ONE * 2), ONE, "clamped past the end");
        let mut prev = 0;
        for u in (0..=ONE).step_by(256) {
            let v = ease(u);
            assert!(v >= prev, "{u}: {v} < {prev}");
            prev = v;
        }
    }
    assert_eq!(
        ease_out_quad(ONE / 2),
        49_152,
        "three quarters at half time"
    );
    assert_eq!(
        ease_out_cubic(ONE / 2),
        57_344,
        "seven eighths at half time"
    );
    assert_eq!(ease_in_out_cubic(ONE / 2), ONE / 2, "half at half time");
    assert_eq!(ease_in_out_cubic(ONE / 4), ONE / 16, "4u³ on the way in");
    assert_eq!(
        ease_in_out_cubic(ONE - ONE / 4),
        ONE - ONE / 16,
        "and its mirror on the way out"
    );
}

#[test]
fn a_glide_spans_its_floor_per_point_and_ceiling() {
    assert_eq!(glide_span(500, 500), GLIDE);
    assert_eq!(
        glide_span(100, 900),
        GLIDE_MAX,
        "80 points: 180 + 560 ms, capped"
    );
    assert_eq!(
        glide_span(0, 100),
        Duration::from_millis(250),
        "10 points: 180 + 70"
    );
    assert_eq!(glide_span(600, 100), glide_span(0, 500), "both ways");
    let span = glide_span(100, 900);
    assert_eq!(glide(100, 900, Duration::ZERO), 100);
    assert_eq!(glide(100, 900, span), 900);
    assert!(glide(100, 900, span / 2) > 500, "eased out");
    assert_eq!(glide(900, 100, span), 100, "down too");
    assert_eq!(
        frac(Duration::from_millis(50), Duration::from_millis(100)),
        ONE / 2
    );
    assert_eq!(
        frac_fine(Duration::from_micros(8_333), Duration::from_micros(16_666)),
        ONE / 2
    );
    assert_eq!(
        frac(Duration::from_secs(9), Duration::ZERO),
        ONE,
        "a zero span is done"
    );
}

#[test]
fn a_critical_spring_arrives_without_passing_its_rest() {
    let spring = Spring::critical(14 * ONE);
    assert_eq!(spring.at(0), 0);
    let mut prev = 0;
    for t in 1..1_500 {
        let x = spring.at(t);
        assert!(x >= prev, "{t}: {x} fell from {prev}");
        assert!(x <= ONE as i32, "{t}: {x} passed its rest");
        prev = x;
    }
    // 1 − (1 + ωt)e^(−ωt) at ω = 14, t = 0.2 s is 0.769.
    let at_200 = f64::from(spring.at(200)) / f64::from(ONE);
    assert!((at_200 - 0.769).abs() < 0.01, "{at_200}");
    let settle = spring.settle_ms();
    assert!((600..1_000).contains(&settle), "{settle}");
    assert_eq!(spring.at(SPRING_MAX_MS), ONE as i32);
}

/// NEGATIVE CONTROL: extreme `omega = u32::MAX` springs saturate intermediate Q32 steps
/// without overflowing `i64` in `at` or `settle_ms` and settle to `ONE` at `SPRING_MAX_MS`.
#[test]
fn an_underdamped_spring_overshoots_once_and_settles() {
    let spring = Spring::underdamped(14 * ONE, 39_322);
    assert_eq!(spring.omega(), 14 * ONE);
    let peak = (0..1_000).map(|t| spring.at(t)).max().unwrap_or(0);
    // ζ = 0.6 overshoots by e^(−πζ/√(1−ζ²)) ≈ 9.5 %.
    let over = f64::from(peak - ONE as i32) / f64::from(ONE);
    assert!((0.085..0.105).contains(&over), "{over}");
    let settle = spring.settle_ms();
    assert!(
        settle > Spring::critical(14 * ONE).settle_ms() / 2,
        "{settle}"
    );
    assert!(settle < SPRING_MAX_MS, "it comes to rest");
    assert_eq!(
        Spring::underdamped(14 * ONE, ONE * 2).zeta(),
        ONE - 1,
        "never critical"
    );
    assert_eq!(spring.at(321), spring.at(321), "one integer every time");

    for extreme in [Spring::critical(u32::MAX), Spring::underdamped(u32::MAX, 0)] {
        let _ = extreme.at(1);
        let _ = extreme.at(SPRING_MAX_MS - 1);
        assert_eq!(extreme.at(SPRING_MAX_MS), ONE as i32);
        assert!(extreme.settle_ms() <= SPRING_MAX_MS);
    }
}

/// NEGATIVE CONTROL: high-rate cadences (`hz > 1000`) at large timestamps saturate
/// `frame_at` at `u64::MAX` instead of truncating `u128 as u64` and keep `next_frame > t_ms`
/// for all `t_ms < u64::MAX` without overflowing `frame_at + 1`.
#[test]
fn a_cadence_keeps_its_grid() {
    let spin = Cadence::SPINNER;
    assert_eq!(spin.frame_at(1_000, 0), 0, "before its start");
    assert_eq!(spin.frame_at(1_000, 1_000), 0);
    assert_eq!(spin.frame_at(1_000, 1_083), 0);
    assert_eq!(spin.frame_at(1_000, 1_084), 1);
    assert_eq!(spin.next_frame(1_000, 1_000), 1_084);
    assert_eq!(spin.next_frame(1_000, 1_084), 1_167);
    assert_eq!(spin.next_frame(1_000, 10), 1_084, "a motion yet to start");
    assert_eq!(
        spin.frame_at(0, 3_600_000),
        43_200,
        "an hour on, the 12 Hz grid has not drifted"
    );
    for t in [0, 1, 83, 84, 999, 1_000, 123_456] {
        let next = spin.next_frame(0, t);
        assert!(next > t);
        assert_eq!(spin.frame_at(0, next), spin.frame_at(0, t) + 1, "{t}");
    }
    assert_eq!(Cadence::hz(0).per_second(), 1);

    let fast = Cadence::hz(2_000);
    let far = 1_u64 << 63;
    assert_eq!(fast.frame_at(0, far), u64::MAX);
    assert_eq!(fast.next_frame(0, far), far + 1);
    assert_eq!(fast.frame_at(0, u64::MAX), u64::MAX);
    assert_eq!(fast.next_frame(0, u64::MAX), u64::MAX);
}
