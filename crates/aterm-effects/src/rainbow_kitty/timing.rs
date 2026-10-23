// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared numbers of Rainbow Kitty v2.

use std::time::Duration;

/// Fixed cost of a flight in ms, before distance.
pub const FLIGHT_BASE_MS: f32 = 50.0;

/// Milliseconds added per cell of path.
pub const FLIGHT_PER_CELL_MS: f32 = 1.0;

/// Floor of the flight clock.
pub const FLIGHT_MIN_MS: f32 = 60.0;

/// Ceiling of the flight clock.
pub const FLIGHT_MAX_MS: f32 = 120.0;

/// The distance→duration function (§8.1 no. 1).
#[inline]
#[must_use]
pub const fn flight_ms(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return FLIGHT_MIN_MS;
    }
    (FLIGHT_BASE_MS + FLIGHT_PER_CELL_MS * cells).clamp(FLIGHT_MIN_MS, FLIGHT_MAX_MS)
}

/// [`flight_ms`] as a [`Duration`], for minting the arrival edge.
#[inline]
#[must_use]
pub fn flight(cells: f32) -> Duration {
    Duration::from_secs_f32(flight_ms(cells) / 1000.0)
}

/// Exponent of the landing's impact curve.
pub const IMPACT_EXP: f32 = 0.7;

/// Where [`impact`] saturates.
pub const IMPACT_MAX: f32 = 3.5;

/// How hard a jump lands.
#[inline]
#[must_use]
pub fn impact(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return 1.0;
    }
    (cells / f32::from(JUMP_MIN_CELLS))
        .powf(IMPACT_EXP)
        .clamp(1.0, IMPACT_MAX)
}

const _: () = assert!(
    IMPACT_MAX >= 1.0,
    "the impact cap cannot be under the floor"
);

/// Head position on the path at `t = 0`, the frame the caret is first seen at its landing (D20).
pub const FLIGHT_P0: f32 = 0.28;

/// Exponent of the `enter-at-speed` easing (§2.5).
pub const FLIGHT_ENTER_EXP: f32 = 2.2;

/// Milliseconds after the arrival edge by which every meteor pixel is off the glass.
pub const FLIGHT_OFF_GLASS_MS: f32 = 600.0;

/// The retire-previous finish (§6.9).
pub const FLIGHT_RETIRE_MS: f32 = 60.0;

/// At most two live meteors (§6.9).
pub const FLIGHT_MAX_LIVE: usize = 2;

/// The same-row jump floor in cells (§6.1).
pub const JUMP_MIN_CELLS: u16 = 8;

/// Smallest same-row hop that draws a mini-fan (§6.12 / D17).
pub const MINI_FAN_MIN_CELLS: u16 = 2;

/// Largest same-row hop that draws a mini-fan, one cell below [`JUMP_MIN_CELLS`].
pub const MINI_FAN_MAX_CELLS: u16 = JUMP_MIN_CELLS - 1;

/// A typing gap this long is a rest.
pub const PHRASE_PAUSE_MS: u32 = 900;

/// Tempo multiple of the phrase rest (D-6).
pub const PHRASE_REST_IOI_MUL: f32 = 2.5;

/// A gap longer than this is not typing.
pub const IOI_RESET_MS: u32 = 2_000;
/// IOI clamp floor / ceiling (ms), the melody's §9.6.
pub const IOI_MIN_MS: f32 = 30.0;
/// See [`IOI_MIN_MS`].
pub const IOI_MAX_MS: f32 = 600.0;
/// The IOI a fresh session assumes.
pub const IOI_DEFAULT_MS: f32 = 250.0;
/// EMA weight on the newest gap (§9.6).
pub const IOI_EMA_ALPHA: f32 = 0.5;

/// The phrase rest in ms for a hand at `ioi_ms`.
#[inline]
#[must_use]
pub const fn phrase_rest_ms(ioi_ms: f32) -> f32 {
    let ioi = if ioi_ms.is_finite() {
        ioi_ms.clamp(IOI_MIN_MS, IOI_MAX_MS)
    } else {
        IOI_DEFAULT_MS
    };
    (PHRASE_REST_IOI_MUL * ioi).max(PHRASE_PAUSE_MS as f32)
}

/// Whether a gap of `gap_ms` since the last key is a rest at tempo `ioi_ms`.
#[inline]
#[must_use]
pub const fn phrase_rest_reached(gap_ms: u32, ioi_ms: f32) -> bool {
    gap_ms as f32 >= phrase_rest_ms(ioi_ms)
}

/// The tempo update.
#[inline]
#[must_use]
pub const fn ioi_next(ioi_ms: f32, gap_ms: u32, first: bool, rest: bool) -> f32 {
    if first || gap_ms >= IOI_RESET_MS {
        IOI_DEFAULT_MS
    } else if rest {
        ioi_ms
    } else {
        (1.0 - IOI_EMA_ALPHA) * ioi_ms
            + IOI_EMA_ALPHA * (gap_ms as f32).clamp(IOI_MIN_MS, IOI_MAX_MS)
    }
}

/// Fewest shed fragments a meteor sputters, whatever the distance.
pub const SHED_MIN: u16 = 3;

/// Most shed fragments a meteor sputters.
pub const SHED_MAX: u16 = 6;

/// Cells of path per shed fragment.
pub const SHED_CELLS_PER: f32 = 12.0;

/// `SHED_N(cells) = clamp(cells/12, 3, 6)` (§8.1, §5.6, §6.5 layer 5).
#[inline]
#[must_use]
pub const fn shed_n(cells: f32) -> u16 {
    if !cells.is_finite() || cells <= 0.0 {
        return SHED_MIN;
    }
    let n = (cells / SHED_CELLS_PER) as u16;
    if n < SHED_MIN {
        SHED_MIN
    } else if n > SHED_MAX {
        SHED_MAX
    } else {
        n
    }
}

/// `FAN_HERO_N = 5` (§8.1, D6).
pub const FAN_HERO_N: usize = 5;

/// Fan stars in total, ceiling included.
pub const FAN_MAX_N: usize = 36;

/// Rain-glint spacing floor in ms (D19).
pub const RAIN_SPACING_MIN_MS: f32 = 18.0;

/// Rain-glint spacing ceiling in ms (D19).
pub const RAIN_SPACING_MAX_MS: f32 = 24.0;

/// Spacing between the meteor's five rain glints, `s = clamp(T/5, 18, 24)` ms (D19).
#[inline]
#[must_use]
pub const fn rain_spacing_ms(flight_ms: f32) -> f32 {
    if !flight_ms.is_finite() {
        return RAIN_SPACING_MIN_MS;
    }
    (flight_ms / FAN_HERO_N as f32).clamp(RAIN_SPACING_MIN_MS, RAIN_SPACING_MAX_MS)
}

/// `12·n + 240` ms.
pub const KILL_RETRACT_BASE_MS: f32 = 240.0;

/// Per-cell term of [`KILL_RETRACT_BASE_MS`]'s schedule.
pub const KILL_RETRACT_PER_CELL_MS: f32 = 12.0;

/// The base in seconds — the ribbon's clock.
const KILL_RETRACT_BASE_S: f32 = KILL_RETRACT_BASE_MS / 1000.0;

/// The per-cell term in seconds — the ribbon's clock.
const KILL_RETRACT_PER_CELL_S: f32 = KILL_RETRACT_PER_CELL_MS / 1000.0;

/// The kill's retract span in ms, `240 + 12·cells`.
#[inline]
#[must_use]
pub const fn kill_span_ms(cells: u16) -> f32 {
    KILL_RETRACT_BASE_MS + KILL_RETRACT_PER_CELL_MS * (cells as f32)
}

/// The kill's retract span in seconds, `0.012·cells + 0.240`.
#[inline]
#[must_use]
pub const fn kill_span_s(cells: u16) -> f32 {
    KILL_RETRACT_PER_CELL_S * (cells as f32) + KILL_RETRACT_BASE_S
}

/// Tokens the stardust bucket regains per second, hence ≤ 4 glints/s (§13).
pub const GLINT_REFILL_PER_S: f32 = 4.0;

/// Bucket depth.
pub const GLINT_CAP: f32 = 3.0;

/// Duration of the ribbon head cell's one ramp-in, seconds.
pub const EDGE_IN_S: f32 = 0.018;

/// `edge-in`: smoothstep over 18 ms, the theme's one attack (§2.5, §4).
#[inline]
#[must_use]
pub const fn edge_in(age_s: f32) -> f32 {
    smoothstep01(age_s / EDGE_IN_S)
}

/// `enter-at-speed`.
#[inline]
#[must_use]
pub fn enter_at_speed(u: f32) -> f32 {
    let u = clamp01(u);
    1.0 - (1.0 - FLIGHT_P0) * (1.0 - u).powf(FLIGHT_ENTER_EXP)
}

/// `suck-in`: `u^1.8` (§2.5).
#[inline]
#[must_use]
pub fn suck_in(u: f32) -> f32 {
    clamp01(u).powf(1.8)
}

/// `spend`: `(1 − u)²` (§2.5).
#[inline]
#[must_use]
pub const fn spend(u: f32) -> f32 {
    let v = 1.0 - clamp01(u);
    v * v
}

/// `half-life`: `1` for a hold `H`, then `2^(−(age − H)/τ)` (§2.5, §5.6).
#[inline]
#[must_use]
pub fn half_life(age_s: f32, hold_s: f32, tau_s: f32) -> f32 {
    if !age_s.is_finite() || age_s <= hold_s {
        return 1.0;
    }
    if tau_s <= 0.0 {
        return 0.0;
    }
    (-(age_s - hold_s) / tau_s * std::f32::consts::LN_2).exp()
}

/// Star cull floor (D3).
pub const STAR_CULL_ALPHA: f32 = 0.20;

/// Chroma cull floor (C3, D3).
pub const CHROMA_CULL_ALPHA: f32 = 0.12;

/// `H + 2.32·τ`.
#[inline]
#[must_use]
pub const fn half_life_life_s(hold_s: f32, tau_s: f32) -> f32 {
    hold_s + tau_s * (std::f32::consts::LOG2_10 - 1.0)
}

/// `spring-snap`'s response time in seconds (§2.5, §7.1), read as the natural period.
pub const SPRING_SNAP_RESPONSE_S: f32 = 0.18;

/// `spring-snap`'s natural frequency, rad/s (≈ 34.9).
pub const SPRING_SNAP_OMEGA: f32 = 2.0 * std::f32::consts::PI / SPRING_SNAP_RESPONSE_S;

/// `spring-snap`.
#[inline]
#[must_use]
pub fn spring_snap(age_s: f32) -> f32 {
    if !age_s.is_finite() || age_s <= 0.0 {
        return 0.0;
    }
    let wt = SPRING_SNAP_OMEGA * age_s;
    1.0 - (1.0 + wt) * (-wt).exp()
}

/// `spring-whip`'s natural frequency, rad/s (§2.5).
pub const SPRING_WHIP_OMEGA: f32 = 24.0;

/// `spring-whip`'s damping ratio (§2.5).
pub const SPRING_WHIP_ZETA: f32 = 0.6;

const SPRING_WHIP_ROOT: f32 = 0.8;
const SPRING_WHIP_WD: f32 = SPRING_WHIP_OMEGA * SPRING_WHIP_ROOT;
const SPRING_WHIP_Z_OVER_ROOT: f32 = SPRING_WHIP_ZETA / SPRING_WHIP_ROOT;
const _: () = assert!(
    (1.0 - SPRING_WHIP_ZETA * SPRING_WHIP_ZETA - SPRING_WHIP_ROOT * SPRING_WHIP_ROOT).abs()
        < f32::EPSILON
);

/// `spring-whip`.
#[inline]
#[must_use]
pub fn spring_whip(age_s: f32) -> f32 {
    if !age_s.is_finite() || age_s <= 0.0 {
        return 0.0;
    }
    let phase = SPRING_WHIP_WD * age_s;
    let env = (-SPRING_WHIP_ZETA * SPRING_WHIP_OMEGA * age_s).exp();
    1.0 - env * (phase.cos() + SPRING_WHIP_Z_OVER_ROOT * phase.sin())
}

/// The theme's one linear fade (§2.5, §6.11).
pub const REDUCED_MOTION_FADE_MS: f32 = 120.0;

/// The additive coverage one sky mark may request.
pub const FIELD_STAR_COV_REQUEST_MAX: f32 = 61.0;

/// `push_twinkle_star`'s composite multiplier.
pub const STAR_STACK_ADD: f32 = 2.35;

/// Ceiling 1, the field/strike lane (§5.2).
pub const FIELD_STAR_COV_CEIL: f32 = STAR_STACK_ADD * FIELD_STAR_COV_REQUEST_MAX;

/// Ceiling 2, the transient lane (§5.2, D1).
pub const TRANSIENT_STAR_COV_CEIL: f32 = 118.0;

/// L3's graded-jump roof.
pub const JUMP_COV_CEIL: f32 = 160.0;

const _: () = assert!(FAN_HERO_N <= FAN_MAX_N);
const _: () = assert!(MINI_FAN_MAX_CELLS + 1 == JUMP_MIN_CELLS);
const _: () = assert!(MINI_FAN_MIN_CELLS >= 2);
const _: () = assert!(FLIGHT_MAX_LIVE == 2);
const _: () = assert!(FIELD_STAR_COV_CEIL > TRANSIENT_STAR_COV_CEIL);
const _: () = assert!(TRANSIENT_STAR_COV_CEIL * 1.5 > JUMP_COV_CEIL);
const _: () = assert!(TRANSIENT_STAR_COV_CEIL < JUMP_COV_CEIL);
const _: () = assert!(EDGE_IN_S > 0.0);
const _: () = assert!(PHRASE_REST_IOI_MUL * IOI_MAX_MS >= PHRASE_PAUSE_MS as f32);
const _: () = assert!(PHRASE_REST_IOI_MUL * 360.0 <= PHRASE_PAUSE_MS as f32);
const _: () = assert!(
    KILL_RETRACT_PER_CELL_S == 0.012 && KILL_RETRACT_BASE_S == 0.240,
    "the seconds form is the literal the ribbon's retract was baked with"
);

/// `t` clamped to `[0, 1]`, NaN resolving to 0.
#[inline]
#[must_use]
pub const fn clamp01(t: f32) -> f32 {
    if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) }
}

/// Hermite smoothstep on `[0, 1]`, clamped.
#[inline]
#[must_use]
pub const fn smoothstep01(t: f32) -> f32 {
    let u = clamp01(t);
    u * u * (3.0 - 2.0 * u)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
