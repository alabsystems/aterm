// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn easing_keeps_timing_nan_policy_and_curve_endpoints() {
    assert_eq!(smoothstep01(f32::NAN), 0.0);
    assert_eq!(smoothstep01(f32::NEG_INFINITY), 0.0);
    assert_eq!(smoothstep01(f32::INFINITY), 1.0);
    for (progress, eased) in [
        (0.0, 0.0),
        (0.25, 0.15625),
        (0.5, 0.5),
        (0.75, 0.84375),
        (1.0, 1.0),
    ] {
        assert_eq!(smoothstep01(progress), eased);
    }
    assert!(aterm_scene::smoothstep(f32::NAN).is_nan());
}

/// The shared clocks are total, monotone and held to their bands: garbage
/// in is the floor, more distance (or a slower tempo) never shortens a
/// clock, and the rain fits inside its gesture for every flight.
#[test]
fn the_shared_clocks_are_total_monotone_and_banded() {
    for garbage in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 0.0] {
        assert_eq!(flight_ms(garbage), FLIGHT_MIN_MS, "flight_ms({garbage})");
        assert_eq!(impact(garbage), 1.0, "impact({garbage})");
    }
    assert_eq!(phrase_rest_ms(f32::NAN), phrase_rest_ms(IOI_DEFAULT_MS));
    let (mut flight_prev, mut impact_prev, mut rest_prev) = (0.0f32, 0.0f32, 0.0f32);
    for k in 0..=960 {
        let c = k as f32 * 0.25;
        let (f, i, r) = (flight_ms(c), impact(c), phrase_rest_ms(c * 25.0));
        assert!(
            f >= flight_prev && (FLIGHT_MIN_MS..=FLIGHT_MAX_MS).contains(&f),
            "flight_ms({c}) = {f}"
        );
        assert!(
            i >= impact_prev && (1.0..=IMPACT_MAX).contains(&i),
            "impact({c}) = {i}"
        );
        assert!(r >= rest_prev, "phrase_rest_ms falls at ioi {}", c * 25.0);
        let rain = rain_spacing_ms(f);
        assert!((RAIN_SPACING_MIN_MS..=RAIN_SPACING_MAX_MS).contains(&rain));
        assert!(
            rain * FAN_HERO_N as f32 <= 250.0,
            "the rain outlives §12's gesture"
        );
        (flight_prev, impact_prev, rest_prev) = (f, i, r);
    }
    assert!((flight(40.0).as_secs_f32() * 1000.0 - flight_ms(40.0)).abs() < 1e-3);
    let rest = phrase_rest_ms(100.0) as u32;
    assert!(phrase_rest_reached(rest, 100.0) && !phrase_rest_reached(rest - 1, 100.0));
    assert_eq!(ioi_next(123.0, 100, true, false), IOI_DEFAULT_MS);
    assert_eq!(ioi_next(123.0, IOI_RESET_MS, false, false), IOI_DEFAULT_MS);
    assert_eq!(ioi_next(123.0, 1000, false, true), 123.0);
    assert!(ioi_next(200.0, 100, false, false) < 200.0);
}

/// §2.5 curves pinned at both ends: frame 0 exists (T2) and the mark is gone
/// by its published life (T6).
#[test]
fn the_seven_curves_hit_their_endpoints() {
    assert!((edge_in(0.0)).abs() < 1e-6);
    assert!((edge_in(EDGE_IN_S) - 1.0).abs() < 1e-6);
    assert!(
        (edge_in(1.0) - 1.0).abs() < 1e-6,
        "past the span it is done"
    );
    assert!(edge_in(EDGE_IN_S * 0.5) > 0.4 && edge_in(EDGE_IN_S * 0.5) < 0.6);

    assert!(
        (enter_at_speed(0.0) - FLIGHT_P0).abs() < 1e-6,
        "the head is never at rest on frame 0 (T2, D20)"
    );
    assert!((enter_at_speed(1.0) - 1.0).abs() < 1e-6);

    assert!((suck_in(0.0)).abs() < 1e-6);
    assert!((suck_in(1.0) - 1.0).abs() < 1e-6);
    assert!(suck_in(0.5) < 0.5, "u^1.8 leaves late");
    assert!((spend(0.0) - 1.0).abs() < 1e-6);
    assert!((spend(1.0)).abs() < 1e-6);
    assert!((spend(0.5) - 0.25).abs() < 1e-6);

    assert!((half_life(0.0, 0.12, 0.146) - 1.0).abs() < 1e-6);
    assert!((half_life(0.12, 0.12, 0.146) - 1.0).abs() < 1e-6);
    assert!((half_life(0.12 + 0.146, 0.12, 0.146) - 0.5).abs() < 1e-5);
    let life = half_life_life_s(0.12, 0.146);
    assert!((half_life(life, 0.12, 0.146) - STAR_CULL_ALPHA).abs() < 1e-5);
    assert!((spring_snap(0.0)).abs() < 1e-6);
    assert!((spring_snap(-1.0)).abs() < 1e-6);
    let mut t = 0.0;
    while t <= 1.0 {
        assert!(spring_snap(t) <= 1.0 + 1e-6, "spring-snap overshot at {t}");
        t += 0.001;
    }
    assert!(
        spring_snap(SPRING_SNAP_RESPONSE_S) > 0.98,
        "settled by its own response time"
    );

    assert!((spring_whip(0.0)).abs() < 1e-6);
    let mut peak = 0.0f32;
    let mut t = 0.0;
    while t <= 0.5 {
        peak = peak.max(spring_whip(t));
        t += 0.0005;
    }
    assert!(
        peak > 1.02,
        "spring-whip must overshoot (ζ = 0.6), peak {peak}"
    );
    let residual = (spring_whip(0.180) - 1.0).abs();
    let bound = (-SPRING_WHIP_ZETA * SPRING_WHIP_OMEGA * 0.180).exp()
        / (1.0 - SPRING_WHIP_ZETA * SPRING_WHIP_ZETA).sqrt();
    assert!(
        residual <= bound + 1e-4 && residual < 0.10,
        "settles ≈ 180 ms (§7.2): residual {residual}, envelope bound {bound}"
    );
}
