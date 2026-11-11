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
pub const fn flight(cells: f32) -> Duration {
    Duration::from_nanos(
        ((flight_ms(cells) * (1.0 / 1000.0)) as f64 * 1_000_000_000.0).round() as u64,
    )
}

/// Exponent of the landing's impact curve.
pub const IMPACT_EXP: f32 = 0.7;

/// Where [`impact`] saturates.
pub const IMPACT_MAX: f32 = 3.5;

/// How hard a jump lands.
#[inline]
#[must_use]
pub const fn impact(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return 1.0;
    }
    crate::color_math::powf_pos_f32(cells * (1.0 / JUMP_MIN_CELLS as f32), IMPACT_EXP)
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
    (flight_ms * (1.0 / FAN_HERO_N as f32)).clamp(RAIN_SPACING_MIN_MS, RAIN_SPACING_MAX_MS)
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
    smoothstep01(age_s * (1.0 / EDGE_IN_S))
}

/// `enter-at-speed`.
#[inline]
#[must_use]
pub const fn enter_at_speed(u: f32) -> f32 {
    let u = clamp01(u);
    1.0 - (1.0 - FLIGHT_P0) * crate::color_math::powf_pos_f32(1.0 - u, FLIGHT_ENTER_EXP)
}

/// `suck-in`: `u^1.8` (§2.5).
#[inline]
#[must_use]
pub const fn suck_in(u: f32) -> f32 {
    crate::color_math::powf_pos_f32(clamp01(u), 1.8)
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
pub const fn half_life(age_s: f32, hold_s: f32, tau_s: f32) -> f32 {
    if !age_s.is_finite() || age_s <= hold_s {
        return 1.0;
    }
    if tau_s <= 0.0 {
        return 0.0;
    }
    crate::color_math::exp_f32(-(age_s - hold_s) / tau_s * std::f32::consts::LN_2)
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
pub const fn spring_snap(age_s: f32) -> f32 {
    if !age_s.is_finite() || age_s <= 0.0 {
        return 0.0;
    }
    let wt = SPRING_SNAP_OMEGA * age_s;
    if wt >= 20.0 {
        return 1.0;
    }
    1.0 - (1.0 + wt) * crate::color_math::exp_f32(-wt)
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
pub const fn spring_whip(age_s: f32) -> f32 {
    if !age_s.is_finite() || age_s <= 0.0 {
        return 0.0;
    }
    let damp = SPRING_WHIP_ZETA * SPRING_WHIP_OMEGA * age_s;
    if damp >= 20.0 {
        return 1.0;
    }
    let (s, c) = crate::supernova::sin_cos_f32(SPRING_WHIP_WD * age_s);
    let env = crate::color_math::exp_f32(-damp);
    1.0 - env * (c + SPRING_WHIP_Z_OVER_ROOT * s)
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
#[path = "timing_tests.rs"]
mod tests;
