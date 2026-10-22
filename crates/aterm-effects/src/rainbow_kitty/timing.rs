// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared numbers of Rainbow Kitty v2: every constant and curve that more
//! than one producer (or the glow and the synth) must agree on, defined once.
//! Both `rainbow_kitty::meteor` and `trail_sound::rainbow_kitty_v2` read the
//! flight clock and shared counts from here, so the landing and its bell cannot
//! drift apart. A second definition of any number below is a bug, even if
//! equal. Section references are to `docs/design/RAINBOW-KITTY-V2.md`.
//!
//! Not here:
//!
//! * The arrival edge `t₀ + T`: an `Instant` minted by [`super::meteor`] at
//!   spawn; [`flight_ms`] computes its offset so every reader derives the same
//!   instant.
//! * The caret's τ 220 ms cool: the host's `TypingCadence` energy
//!   (`CadenceParams::half_life` in `cursor_trail`), taken as
//!   `max(CaretSeam::paint, energy)`.
//! * The ≤ 3 live glints voice cap: `trail_sound::rainbow_kitty_v2::lane_cap`;
//!   the glow's half is the token bucket below.
//!
//! Durations are milliseconds (`f32`) unless a name ends in `_S`. Pixel figures
//! are at `ch = 18`; callers scale by `ch/18`. Coverages are additive `0..=255`
//! requests, pre-composite.

use std::time::Duration;

// 1. The flight clock (§8.1 no. 1, §6.2)

/// Fixed cost of a flight in ms, before distance — the part of `T` that is
/// "a jump happened at all" rather than "a jump went far" (§6.2).
pub const FLIGHT_BASE_MS: f32 = 50.0;

/// Milliseconds added per cell of path. At 1.0 ms/cell an 80-column fling is
/// 130 ms of *raw* flight before the ceiling bites — see [`FLIGHT_MAX_MS`].
pub const FLIGHT_PER_CELL_MS: f32 = 1.0;

/// Floor of the flight clock: shorter flights are about two frames at 60 Hz
/// and read as a teleport with a smear (`SHOULDER_FRAME_MS` has the frame-rate
/// details).
pub const FLIGHT_MIN_MS: f32 = 60.0;

/// Ceiling of the flight clock: past 120 ms the caret visibly arrives before
/// its own meteor.
pub const FLIGHT_MAX_MS: f32 = 120.0;

/// The distance→duration function (§8.1 no. 1):
/// `T = clamp(50 + 1.0·cells, 60, 120)` ms.
///
/// The visual flight (hence the arrival edge `t₀ + T`), the ribbon's jump, the
/// meteor bell's `Voice::delay` (§12.1) and the Enter cadence's resolution (D9)
/// all read it, so they land on the same frame; [`rain_spacing_ms`] is a
/// function of it.
///
/// `cells` is the path length in cell widths, diagonals included:
/// `cells = sqrt(dcol² + (drow·ch/cw)²)`. Non-finite or negative input resolves
/// to [`FLIGHT_MIN_MS`]: 8 → 60, 40 → 90, 200 → 120.
#[inline]
#[must_use]
pub const fn flight_ms(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return FLIGHT_MIN_MS;
    }
    (FLIGHT_BASE_MS + FLIGHT_PER_CELL_MS * cells).clamp(FLIGHT_MIN_MS, FLIGHT_MAX_MS)
}

/// [`flight_ms`] as a [`Duration`], for minting the arrival edge; one
/// conversion, so nobody rounds to whole milliseconds and lands a frame early.
#[inline]
#[must_use]
pub fn flight(cells: f32) -> Duration {
    Duration::from_secs_f32(flight_ms(cells) / 1000.0)
}

/// Exponent of the landing's impact curve: concave, so 16-40 cell jumps get
/// most of the headroom and a wide-terminal Ctrl-A does not climb forever.
pub const IMPACT_EXP: f32 = 0.7;

/// Where [`impact`] saturates: reached at ≈ 48 cells — a full line — so an
/// 80- or 200-cell jump lands on the cap instead of past it.
pub const IMPACT_MAX: f32 = 3.5;

/// How hard a jump lands: one magnitude read by every distance-graded landing
/// law (fan count, fan reach, ring radius, ring life).
///
/// `impact(cells) = clamp((cells / 8)^0.7, 1, 3.5)`: exactly `1.0` at the
/// 8-cell meteor floor ([`JUMP_MIN_CELLS`]), 1.62 at 16 cells, 2.16 at 24,
/// 3.09 at 40, capped from ≈ 48 on. A NaN, infinite or non-positive distance
/// is the floor.
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

/// Head position on the path at `t = 0`, the frame the caret is first seen at
/// its landing (D20). Frame 0 always draws; see [`enter_at_speed`].
pub const FLIGHT_P0: f32 = 0.28;

/// Exponent of the `enter-at-speed` easing (§2.5): the head is past 0.40 of the
/// path on the first ProMotion frame after spawn (+8.3 ms at `T = 100`).
pub const FLIGHT_ENTER_EXP: f32 = 2.2;

/// Milliseconds after the arrival edge by which every meteor pixel is off the
/// glass; the flight itself ([`flight_ms`]) is unaffected. `meteor.rs` alone
/// reads it: the pool's `end()` is `arrival + this`, and every impact mark is
/// asserted inside it at compile time there.
pub const FLIGHT_OFF_GLASS_MS: f32 = 600.0;

/// The retire-previous finish (§6.9): a new meteor within 1 `ch` of a live one
/// sends that one to its post-arrival fade with this `R`, never a pop.
pub const FLIGHT_RETIRE_MS: f32 = 60.0;

/// At most two live meteors (§6.9). A third evicts the oldest through the
/// [`FLIGHT_RETIRE_MS`] finish.
pub const FLIGHT_MAX_LIVE: usize = 2;

/// The same-row jump floor in cells (§6.1): scrubbing stays calm. At or above
/// it a move flies the full meteor; below it see [`MINI_FAN_MAX_CELLS`].
pub const JUMP_MIN_CELLS: u16 = 8;

/// Smallest same-row hop that draws a mini-fan (§6.12 / D17). A one-cell move
/// is typing or an arrow, and owns the ribbon head cell instead.
pub const MINI_FAN_MIN_CELLS: u16 = 2;

/// Largest same-row hop that draws a mini-fan, one below [`JUMP_MIN_CELLS`], so
/// the two ranges tile with no gap (§6.12: no flight, no train, 1 m2 + 2 m3,
/// gone by 245 ms).
pub const MINI_FAN_MAX_CELLS: u16 = JUMP_MIN_CELLS - 1;

// 1b. The phrase rest: one rest for the melody and the ribbon (law S5)

/// A typing gap this long is a rest: it resolves the melody's line and ends the
/// phrase the ribbon holds. The melody re-exports it and the ribbon's grace
/// derives from it, so light and song let go on the same instant (S5).
///
/// 900 ms is longer than a word-finding pause and shorter than a real stop.
pub const PHRASE_PAUSE_MS: u32 = 900;

/// Tempo multiple of the phrase rest (D-6):
/// `phrase_rest_ms(ioi) = max(PHRASE_PAUSE_MS, 2.5 · ioi)`. At ≥ 2.8 cps
/// (`ioi ≤ 360 ms`) the rest is the 900 ms floor; a slower hand gets a longer
/// rest in both halves, so a 2 cps line does not start its exit swoosh between
/// its own keys.
pub const PHRASE_REST_IOI_MUL: f32 = 2.5;

/// A gap longer than this is not typing: the IOI estimator restarts at
/// [`IOI_DEFAULT_MS`].
pub const IOI_RESET_MS: u32 = 2_000;
/// IOI clamp floor / ceiling (ms), the melody's §9.6. The ceiling bounds the
/// phrase rest too: `phrase_rest_ms` tops out at `2.5 · 600 = 1500 ms`.
pub const IOI_MIN_MS: f32 = 30.0;
/// See [`IOI_MIN_MS`].
pub const IOI_MAX_MS: f32 = 600.0;
/// The IOI a fresh session assumes — 4 cps, which the melody's §9.6 table
/// uses as the 0 dB reference for the whole loudness arc.
pub const IOI_DEFAULT_MS: f32 = 250.0;
/// EMA weight on the newest gap (§9.6): two-gap memory, fast enough to hear a
/// burst, slow enough that one stumbled key does not brighten the next note.
pub const IOI_EMA_ALPHA: f32 = 0.5;

/// The phrase rest in ms for a hand at `ioi_ms`: the longer of
/// [`PHRASE_PAUSE_MS`] and [`PHRASE_REST_IOI_MUL`] tempos; non-finite input
/// reads as the default tempo. Read by the melody (`MelodyV2::on_typed`) and
/// the ribbon's grace (`Ribbon::rest_s`).
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

/// The tempo update: the inter-onset EMA (§9.6), shared by melody and ribbon.
/// The first key of a session, or a gap of [`IOI_RESET_MS`] or more, restarts
/// at [`IOI_DEFAULT_MS`]; a rest holds the tempo (so the same text at two
/// speeds plays the same degrees after a pause); otherwise the EMA on the
/// clamped gap.
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

// 2. The shared counts (§8.1)

/// Fewest shed fragments a meteor sputters, whatever the distance — a jump
/// that sheds nothing has no sense of *burning* (§6.5 layer 5).
pub const SHED_MIN: u16 = 3;

/// Most shed fragments a meteor sputters. Past six the path reads as a dotted
/// line rather than as debris.
pub const SHED_MAX: u16 = 6;

/// Cells of path per shed fragment.
pub const SHED_CELLS_PER: f32 = 12.0;

/// `SHED_N(cells) = clamp(cells/12, 3, 6)` (§8.1, §5.6, §6.5 layer 5): m3
/// grains a meteor sheds along its path. Silent: the rain glints pair with the
/// landing fan (D6). Shared because §6.5 sizes the quad budget from it.
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

/// `FAN_HERO_N = 5` (§8.1, D6): the landing fan always carries 1 gold m1 + 4
/// m2, and the meteor's five rain glints pair 1:1 with them in throw order, so
/// the visual census (`the_landing_fan_is_one_hero_four_m2_and_grains`) and the
/// audio run length agree.
pub const FAN_HERO_N: usize = 5;

/// Fan stars in total, ceiling included:
/// `n = min(19 + 3.6·(impact − 1), 28) + party·8` never exceeds this. Shared
/// with the sky, which sizes its throw from it.
pub const FAN_MAX_N: usize = 36;

/// Rain-glint spacing floor in ms (D19).
pub const RAIN_SPACING_MIN_MS: f32 = 18.0;

/// Rain-glint spacing ceiling in ms (D19). The whole gesture is −40 dB by
/// `T + 250`, so five glints cannot be allowed to outlive it.
pub const RAIN_SPACING_MAX_MS: f32 = 24.0;

/// Spacing between the meteor's five rain glints, `s = clamp(T/5, 18, 24)` ms
/// (D19): a function of the flight clock, so retuning [`flight_ms`] retunes it.
#[inline]
#[must_use]
pub const fn rain_spacing_ms(flight_ms: f32) -> f32 {
    if !flight_ms.is_finite() {
        return RAIN_SPACING_MIN_MS;
    }
    (flight_ms / FAN_HERO_N as f32).clamp(RAIN_SPACING_MIN_MS, RAIN_SPACING_MAX_MS)
}

// 2b. The kill's retract schedule (§8.2, §27)

/// `12·n + 240` ms: the kill's retract span (§8.2). A kill of `n` cells drains
/// its suffix farthest-first over this long; a Backspace (`n = 0`) over the
/// base alone. Shared by the ribbon (retracting cells), the insert's rewrite
/// (§27) and the sky (finishing those cells' field stars, which cannot read
/// the cell per frame).
pub const KILL_RETRACT_BASE_MS: f32 = 240.0;

/// Per-cell term of [`KILL_RETRACT_BASE_MS`]'s schedule.
pub const KILL_RETRACT_PER_CELL_MS: f32 = 12.0;

/// The base in seconds — the ribbon's clock.
const KILL_RETRACT_BASE_S: f32 = KILL_RETRACT_BASE_MS / 1000.0;

/// The per-cell term in seconds — the ribbon's clock.
const KILL_RETRACT_PER_CELL_S: f32 = KILL_RETRACT_PER_CELL_MS / 1000.0;

/// The kill's retract span in ms, `240 + 12·cells` — the sky's clock.
#[inline]
#[must_use]
pub const fn kill_span_ms(cells: u16) -> f32 {
    KILL_RETRACT_BASE_MS + KILL_RETRACT_PER_CELL_MS * (cells as f32)
}

/// The kill's retract span in seconds, `0.012·cells + 0.240` — the ribbon's
/// clock, in the operation order its retract was baked with.
#[inline]
#[must_use]
pub const fn kill_span_s(cells: u16) -> f32 {
    KILL_RETRACT_PER_CELL_S * (cells as f32) + KILL_RETRACT_BASE_S
}

// 3. The token bucket (§8.1 no. 2, §13, D5)

/// Tokens the stardust bucket regains per second, hence ≤ 4 glints/s (§13);
/// refilled once per `Engine::tick`.
pub const GLINT_REFILL_PER_S: f32 = 4.0;

/// Bucket depth: three banked tokens let a burst of capitals chime (three
/// glints in 250 ms) without a long idle banking an arpeggio.
///
/// The bucket's law (D5), enforced at its one spend site (`stardust.rs`'s
/// `deal_star`): only an m1 spends a token, cueing one glint at its column; an
/// earned hero without a token is born silent; a dealt hero without one
/// degrades to m2; m2/m3 never touch the bucket; meteor-lane stars are exempt,
/// and a meteor empties the bucket on its move edge so nothing chimes over the
/// rain. Voice concurrency is the synth's lane cap.
pub const GLINT_CAP: f32 = 3.0;

// 4. The seven named curves (§2.5)
//
// The theme's only easing vocabulary; no linear fades except reduced motion's
// `REDUCED_MOTION_FADE_MS`, which is deliberately not a curve function.

/// Duration of the ribbon head cell's one ramp-in, seconds. Nothing else eases
/// in (T3).
pub const EDGE_IN_S: f32 = 0.018;

/// `edge-in`: smoothstep over 18 ms, the theme's one attack (§2.5, §4): the
/// ribbon head cell coming up at the hand. Takes an age in seconds, since its
/// span is fixed by [`EDGE_IN_S`].
#[inline]
#[must_use]
pub const fn edge_in(age_s: f32) -> f32 {
    smoothstep01(age_s / EDGE_IN_S)
}

/// `enter-at-speed`: `p(u) = 1 − (1 − p₀)·(1 − u)^2.2`, `p₀ = 0.28` (§2.5, §6.2,
/// D20), the meteor head's position at `u = t/T`. `p(0) = p₀`, so the gesture
/// is never seen starting from rest; `p(1) = 1` exactly.
#[inline]
#[must_use]
pub fn enter_at_speed(u: f32) -> f32 {
    let u = clamp01(u);
    1.0 - (1.0 - FLIGHT_P0) * (1.0 - u).powf(FLIGHT_ENTER_EXP)
}

/// `suck-in`: `u^1.8` (§2.5). Position law of any root or tail retracting
/// toward the caret: the meteor train's colour root over its 300 ms `R` (§6.8),
/// the ribbon's backspace retract, the pin's arms. Leaving marks move toward
/// the caret rather than fading in place.
#[inline]
#[must_use]
pub fn suck_in(u: f32) -> f32 {
    clamp01(u).powf(1.8)
}

/// `spend`: `(1 − u)²` (§2.5). Alpha law of a retracting mark and the focus-lost
/// ember (§8.2); paired with [`suck_in`] for position.
#[inline]
#[must_use]
pub const fn spend(u: f32) -> f32 {
    let v = 1.0 - clamp01(u);
    v * v
}

/// `half-life`: `1` for a hold `H`, then `2^(−(age − H)/τ)` (§2.5, §5.6).
///
/// The envelope of every star and of the caret's landing re-light
/// (`Engine::caret_paint`); `hold_s` and `tau_s` are per population.
/// A non-positive `tau_s` is a step: full through the hold, then nothing.
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

/// Star cull floor (D3): a star is dropped once [`half_life`] falls below this,
/// so grains wink out instead of greying.
pub const STAR_CULL_ALPHA: f32 = 0.20;

/// Chroma cull floor (C3, D3): coloured meteor-train pixels are culled below
/// this. Lower than [`STAR_CULL_ALPHA`] because the train's dim tail is still
/// chromatic; a star's is nothing.
pub const CHROMA_CULL_ALPHA: f32 = 0.12;

/// `H + 2.32·τ`: the actual life of a [`half_life`] envelope given
/// [`STAR_CULL_ALPHA`] (`2.32 = log₂(1/0.20) = log₂(5) = log₂(10) − 1`).
/// The class lives (225 / 340 / 460 ms) follow from their `H`/`τ` pairs.
#[inline]
#[must_use]
pub const fn half_life_life_s(hold_s: f32, tau_s: f32) -> f32 {
    hold_s + tau_s * (std::f32::consts::LOG2_10 - 1.0)
}

/// `spring-snap`'s response time in seconds (§2.5, §7.1), read as the natural
/// period: `ω = 2π/response`, so the mark is settled (residual under 1.5%) at
/// exactly `response`.
pub const SPRING_SNAP_RESPONSE_S: f32 = 0.18;

/// `spring-snap`'s natural frequency, rad/s (≈ 34.9) — see
/// [`SPRING_SNAP_RESPONSE_S`].
pub const SPRING_SNAP_OMEGA: f32 = 2.0 * std::f32::consts::PI / SPRING_SNAP_RESPONSE_S;

/// `spring-snap`: a critically damped unit step (ζ = 1.0), response 0.18 s:
/// `x(t) = 1 − (1 + ωt)·e^(−ωt)` (§2.5). Used for the caret flare relaxing to
/// its field stop (§7.1) and the kitty's landing. No overshoot: a caret that
/// dips past its colour reads as not arrived.
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

/// `spring-whip`'s damping ratio (§2.5). Under 1, so it overshoots: the flying
/// kitty is yanked backwards and springs past her resting lead before settling.
pub const SPRING_WHIP_ZETA: f32 = 0.6;

const SPRING_WHIP_ROOT: f32 = 0.8;
const SPRING_WHIP_WD: f32 = SPRING_WHIP_OMEGA * SPRING_WHIP_ROOT;
const SPRING_WHIP_Z_OVER_ROOT: f32 = SPRING_WHIP_ZETA / SPRING_WHIP_ROOT;
const _: () = assert!(
    (1.0 - SPRING_WHIP_ZETA * SPRING_WHIP_ZETA - SPRING_WHIP_ROOT * SPRING_WHIP_ROOT).abs()
        < f32::EPSILON
);

/// `spring-whip`: the damped unit step (ω = 24 rad/s, ζ = 0.6) of §2.5:
/// `x(t) = 1 − e^(−ζωt)·(cos ω_d t + (ζ/√(1−ζ²))·sin ω_d t)`,
/// `ω_d = ω√(1−ζ²) = 19.2`.
///
/// The flying kitty's lead on a meteor (§7.2): `−0.30` cell at `t = 0`,
/// springing to `companion::FLYING_LEAD_MAX` (+0.22), settled by ≈ 180 ms
/// (`e^(−ζωt) ≈ 0.07`).
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

/// The theme's one linear fade (§2.5, §6.11): under reduced motion every mark is
/// static and its alpha runs linearly to zero over 120 ms at the end of its life.
pub const REDUCED_MOTION_FADE_MS: f32 = 120.0;

// 5. The two coverage ceilings (§5.2, D1)
//
// One star family, two prices: sky stars live above the ribbon for 225-460 ms
// under the sparkle law; transient stars fly over ink for ≤ 340 ms under the
// transient cap.

/// The additive coverage one sky mark may request; field and strike stars are
/// priced from it.
pub const FIELD_STAR_COV_REQUEST_MAX: f32 = 61.0;

/// `push_twinkle_star`'s composite multiplier: two crossing body bars plus the
/// nucleus (`STAR_CORE_ADD 0.35`). No class may add a duplicate core rect (D1).
pub const STAR_STACK_ADD: f32 = 2.35;

/// Ceiling 1, the field/strike lane (§5.2): a sky star's composited centre may
/// not exceed `STAR_STACK_ADD × 61 = 143`. Class centres: field m1 143, m2 130,
/// m3 98.
pub const FIELD_STAR_COV_CEIL: f32 = STAR_STACK_ADD * FIELD_STAR_COV_REQUEST_MAX;

/// Ceiling 2, the transient lane (§5.2, D1): fan, shed, erase-throw and
/// mini-fan stars fly over ink. Class centres: m1 118, m2 80, m3 61.
pub const TRANSIENT_STAR_COV_CEIL: f32 = 118.0;

/// L3's graded-jump roof: the meteor's `head_cov = min(118·bright, 160)` (§6.3)
/// tops out here.
pub const JUMP_COV_CEIL: f32 = 160.0;

// Compile-time laws: relations between the shared numbers, so a wrong value
// does not compile.

// §8.1 / D6: the fan's five paired stars fit inside the fan.
const _: () = assert!(FAN_HERO_N <= FAN_MAX_N);
// §6.1 / §6.12: the mini-fan band and the flight floor tile with no gap.
const _: () = assert!(MINI_FAN_MAX_CELLS + 1 == JUMP_MIN_CELLS);
// §6.12: a one-cell move is typing or an arrow, never a hop.
const _: () = assert!(MINI_FAN_MIN_CELLS >= 2);
const _: () = assert!(FLIGHT_MAX_LIVE == 2);
// D1: two ceilings are genuinely two numbers, and the field lane is the
// brighter one.
const _: () = assert!(FIELD_STAR_COV_CEIL > TRANSIENT_STAR_COV_CEIL);
// §6.3 / L3: a full-grade head (`118 × 1.5 = 177`) exceeds the jump roof, so
// the roof binds; the roof is above the transient ceiling, so an ungraded head
// is never clipped.
const _: () = assert!(TRANSIENT_STAR_COV_CEIL * 1.5 > JUMP_COV_CEIL);
const _: () = assert!(TRANSIENT_STAR_COV_CEIL < JUMP_COV_CEIL);
const _: () = assert!(EDGE_IN_S > 0.0);
// §2.6 / S5: both arms of `phrase_rest_ms`'s `max` are reachable. The floor
// binds up to an IOI of 360 ms (`2.5 × 360 = 900`); above that the tempo term
// binds, staying at or above the floor to the clamp's top (`2.5 × 600 = 1500`).
const _: () = assert!(PHRASE_REST_IOI_MUL * IOI_MAX_MS >= PHRASE_PAUSE_MS as f32);
const _: () = assert!(PHRASE_REST_IOI_MUL * 360.0 <= PHRASE_PAUSE_MS as f32);
// The seconds form is bit-equal to `0.012` and `0.240`, so the kill, the
// rewrite and the field-star finish share one clock.
const _: () = assert!(
    KILL_RETRACT_PER_CELL_S == 0.012 && KILL_RETRACT_BASE_S == 0.240,
    "the seconds form is the literal the ribbon's retract was baked with"
);

// Small shared shapes

/// `t` clamped to `[0, 1]`, NaN resolving to 0 — the guard every curve above
/// runs first, so a non-finite age can never propagate into a coverage byte.
#[inline]
#[must_use]
pub const fn clamp01(t: f32) -> f32 {
    if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) }
}

/// Hermite smoothstep on `[0, 1]`, clamped: `3u² − 2u³`.
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
        // A rest is reached at its own length, not a millisecond before.
        let rest = phrase_rest_ms(100.0) as u32;
        assert!(phrase_rest_reached(rest, 100.0) && !phrase_rest_reached(rest - 1, 100.0));
        // The tempo law: first / reset → default, a rest holds, else it moves.
        assert_eq!(ioi_next(123.0, 100, true, false), IOI_DEFAULT_MS);
        assert_eq!(ioi_next(123.0, IOI_RESET_MS, false, false), IOI_DEFAULT_MS);
        assert_eq!(ioi_next(123.0, 1000, false, true), 123.0);
        assert!(ioi_next(200.0, 100, false, false) < 200.0);
    }

    /// §2.5 curves pinned at both ends: frame 0 exists (T2) and the mark is gone
    /// by its published life (T6).
    #[test]
    fn the_seven_curves_hit_their_endpoints() {
        // edge-in: 0 at birth, 1 at 18 ms, monotone between.
        assert!((edge_in(0.0)).abs() < 1e-6);
        assert!((edge_in(EDGE_IN_S) - 1.0).abs() < 1e-6);
        assert!(
            (edge_in(1.0) - 1.0).abs() < 1e-6,
            "past the span it is done"
        );
        assert!(edge_in(EDGE_IN_S * 0.5) > 0.4 && edge_in(EDGE_IN_S * 0.5) < 0.6);

        // enter-at-speed: p(0) = p₀ exactly (the frame-0 law), p(1) = 1.
        assert!(
            (enter_at_speed(0.0) - FLIGHT_P0).abs() < 1e-6,
            "the head is never at rest on frame 0 (T2, D20)"
        );
        assert!((enter_at_speed(1.0) - 1.0).abs() < 1e-6);

        // suck-in / spend are a matched pair on the same u.
        assert!((suck_in(0.0)).abs() < 1e-6);
        assert!((suck_in(1.0) - 1.0).abs() < 1e-6);
        assert!(suck_in(0.5) < 0.5, "u^1.8 leaves late");
        assert!((spend(0.0) - 1.0).abs() < 1e-6);
        assert!((spend(1.0)).abs() < 1e-6);
        assert!((spend(0.5) - 0.25).abs() < 1e-6);

        // half-life: flat through the hold, exactly ½ one τ after it, and at
        // the cull floor at the published life.
        assert!((half_life(0.0, 0.12, 0.146) - 1.0).abs() < 1e-6);
        assert!((half_life(0.12, 0.12, 0.146) - 1.0).abs() < 1e-6);
        assert!((half_life(0.12 + 0.146, 0.12, 0.146) - 0.5).abs() < 1e-5);
        let life = half_life_life_s(0.12, 0.146);
        assert!((half_life(life, 0.12, 0.146) - STAR_CULL_ALPHA).abs() < 1e-5);
        // spring-snap: starts at 0, never overshoots, settled by its response.
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

        // spring-whip: starts at 0, overshoots, and is settled by ≈ 180 ms.
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
        // "Settled ≈ 180 ms" bounds the envelope: at t = 0.18 the decay is
        // e^(−2.592) ≈ 0.075 and the amplitude factor ≤ 1/√(1−ζ²) = 1.25, so
        // the residual is ≤ 0.094.
        let residual = (spring_whip(0.180) - 1.0).abs();
        let bound = (-SPRING_WHIP_ZETA * SPRING_WHIP_OMEGA * 0.180).exp()
            / (1.0 - SPRING_WHIP_ZETA * SPRING_WHIP_ZETA).sqrt();
        assert!(
            residual <= bound + 1e-4 && residual < 0.10,
            "settles ≈ 180 ms (§7.2): residual {residual}, envelope bound {bound}"
        );
    }
}
