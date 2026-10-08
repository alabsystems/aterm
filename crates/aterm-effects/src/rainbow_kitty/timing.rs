// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared numbers of Rainbow Kitty v2: every constant and curve that more
//! than one producer (or the glow and the synth) must agree on, each defined
//! once here with its law beside it.
//!
//! Section references are to `docs/design/RAINBOW-KITTY-V2.md`: §8.1 (shared
//! numbers), §2.5 (the seven named curves), §5.2 (coverage ceilings), §6.1-6.2
//! (the meteor's trigger and clock), §13 (the token bucket).
//!
//! Both `rainbow_kitty::meteor` and `trail_sound::rainbow_kitty_v2` read the
//! distance→duration function, glint budget, shed count and fan hero count from
//! here, so the landing and its bell cannot drift apart. A second definition of
//! any number below is a bug, even if equal.
//!
//! ## Not here
//!
//! * **The arrival edge `t₀ + T`** is an `Instant` minted by [`super::meteor`]
//!   at spawn and read by the pin, ring, fan, flash, the kitty's `land_at` and
//!   the bell; [`flight_ms`] computes its offset so every reader derives the
//!   same instant.
//! * **The caret's τ 220 ms cool** belongs to the host: `TypingCadence`'s
//!   `energy` (`CadenceParams::half_life` in `cursor_trail`). The host takes
//!   `max(CaretSeam::paint, energy)`.
//! * **"≤ 3 live glints, a fourth steals the oldest"** is a voice cap owned by
//!   `trail_sound::rainbow_kitty_v2::lane_cap`; the glow's half is the token
//!   bucket below.
//!
//! ## Units
//!
//! Durations are milliseconds (`f32`) unless a name ends in `_S`. Pixel figures
//! are at `ch = 18`; callers scale by `ch/18`. Coverages are additive `0..=255`
//! requests, pre-composite.

use std::time::Duration;

// ---------------------------------------------------------------------------
// 1. The flight clock (§8.1 no. 1, §6.2)
// ---------------------------------------------------------------------------

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
/// Read by exactly four couplings:
///
/// 1. the visual flight (`meteor.rs`'s `T`, hence the arrival edge `t₀ + T`);
/// 2. the audio rain spacing ([`rain_spacing_ms`] is a function of this value);
/// 3. the meteor bell's `Voice::delay` (§12.1), so it strikes on the landing
///    frame;
/// 4. the Enter cadence's resolution delay (D9).
///
/// `cells` is the path length in cell widths, diagonals included:
/// `cells = sqrt(dcol² + (drow·ch/cw)²)`. Non-finite or negative input resolves
/// to [`FLIGHT_MIN_MS`].
///
/// Pinned by `flight_ms_is_shared_and_clamped`: 8 → 60, 40 → 90, 200 → 120.
#[inline]
#[must_use]
pub fn flight_ms(cells: f32) -> f32 {
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
/// law (fan count, fan reach, ring radius, ring life), so the splash scales
/// with the distance travelled (owner, 2026-09-08).
///
/// `impact(cells) = clamp((cells / 8)^0.7, 1, 3.5)`: exactly `1.0` at the
/// 8-cell meteor floor ([`JUMP_MIN_CELLS`]), 1.62 at 16 cells, 2.16 at 24,
/// 3.09 at 40, capped from ≈ 48 on.
///
/// A NaN, infinite or non-positive distance is the floor.
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
/// glass. The impact carries a 420 ms shockwave, a 260 ms splash and sparks
/// living up to 560 ms; the flight itself ([`flight_ms`]) is unaffected. Read
/// by `meteor.rs` alone: the pool's `end()` is `arrival + this`, and every
/// impact mark is asserted inside it at compile time there.
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

// ---------------------------------------------------------------------------
// 1b. The phrase rest: one rest for the melody and the ribbon (law S5)
// ---------------------------------------------------------------------------

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
/// the ribbon's grace (`Ribbon::place`).
#[inline]
#[must_use]
pub fn phrase_rest_ms(ioi_ms: f32) -> f32 {
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
pub fn phrase_rest_reached(gap_ms: u32, ioi_ms: f32) -> bool {
    gap_ms as f32 >= phrase_rest_ms(ioi_ms)
}

/// The tempo update: the inter-onset EMA (§9.6), shared by melody and ribbon.
/// The first key of a session, or a gap of [`IOI_RESET_MS`] or more, restarts
/// at [`IOI_DEFAULT_MS`]; a rest holds the tempo (so the same text at two
/// speeds plays the same degrees after a pause); otherwise the EMA on the
/// clamped gap.
#[inline]
#[must_use]
pub fn ioi_next(ioi_ms: f32, gap_ms: u32, first: bool, rest: bool) -> f32 {
    if first || gap_ms >= IOI_RESET_MS {
        IOI_DEFAULT_MS
    } else if rest {
        ioi_ms
    } else {
        (1.0 - IOI_EMA_ALPHA) * ioi_ms
            + IOI_EMA_ALPHA * (gap_ms as f32).clamp(IOI_MIN_MS, IOI_MAX_MS)
    }
}

// ---------------------------------------------------------------------------
// 2. The shared counts (§8.1)
// ---------------------------------------------------------------------------

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
pub fn shed_n(cells: f32) -> u16 {
    if !cells.is_finite() || cells <= 0.0 {
        return SHED_MIN;
    }
    ((cells / SHED_CELLS_PER) as u16).clamp(SHED_MIN, SHED_MAX)
}

/// `FAN_HERO_N = 5` (§8.1, D6): the landing fan always carries 1 gold m1 + 4
/// m2, and the meteor's five rain glints pair 1:1 with them in throw order, so
/// the visual census (`the_fan_is_one_hero_four_m2_and_grains`) and the audio
/// run length agree.
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
pub fn rain_spacing_ms(flight_ms: f32) -> f32 {
    if !flight_ms.is_finite() {
        return RAIN_SPACING_MIN_MS;
    }
    (flight_ms / FAN_HERO_N as f32).clamp(RAIN_SPACING_MIN_MS, RAIN_SPACING_MAX_MS)
}

// ---------------------------------------------------------------------------
// 2b. The kill's retract schedule (§8.2, §27)
// ---------------------------------------------------------------------------

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
pub fn kill_span_ms(cells: u16) -> f32 {
    KILL_RETRACT_BASE_MS + KILL_RETRACT_PER_CELL_MS * f32::from(cells)
}

/// The kill's retract span in seconds, `0.012·cells + 0.240` — the ribbon's
/// clock, in the operation order its retract was baked with.
#[inline]
#[must_use]
pub fn kill_span_s(cells: u16) -> f32 {
    KILL_RETRACT_PER_CELL_S * f32::from(cells) + KILL_RETRACT_BASE_S
}

// ---------------------------------------------------------------------------
// 3. The token bucket (§8.1 no. 2, §13, D5)
// ---------------------------------------------------------------------------

/// Tokens the stardust bucket regains per second, hence ≤ 4 glints/s (§13).
/// Refilled in `tick()`: `tokens = min(GLINT_CAP, tokens + GLINT_REFILL_PER_S · dt)`.
pub const GLINT_REFILL_PER_S: f32 = 4.0;

/// Bucket depth: three banked tokens let a burst of capitals chime (three
/// glints in 250 ms) without a long idle banking an arpeggio.
///
/// The bucket's law (D5), enforced at its one spend site (`stardust.rs`'s
/// `deal_star`): only an m1 spends a token, cueing one glint at its column; an
/// earned hero (capital, `!`, kitty Delight, the fan's slot) without a token is
/// born silent; a dealt hero without one degrades to m2; m2/m3 never touch the
/// bucket; meteor-lane stars are exempt, and a meteor empties the bucket on its
/// move edge so nothing chimes over the rain. Voice concurrency is the synth's
/// lane cap.
pub const GLINT_CAP: f32 = 3.0;

// ---------------------------------------------------------------------------
// 4. The seven named curves (§2.5)
// ---------------------------------------------------------------------------
//
// The theme's only easing vocabulary; no linear fades except reduced motion's
// `REDUCED_MOTION_FADE_MS`, which is deliberately not a curve function.

/// Duration of the ribbon head cell's one ramp-in, seconds (v1's
/// `RAINBOW_EDGE_IN_S`, kept verbatim). T3: *nothing else eases in.*
pub const EDGE_IN_S: f32 = 0.018;

/// `edge-in`: smoothstep over 18 ms, the theme's one attack (§2.5, §4): the
/// ribbon head cell coming up at the hand. Takes an age in seconds, since its
/// span is fixed by [`EDGE_IN_S`].
#[inline]
#[must_use]
pub fn edge_in(age_s: f32) -> f32 {
    smoothstep01(age_s / EDGE_IN_S)
}

/// `enter-at-speed`: `p(u) = 1 − (1 − p₀)·(1 − u)^2.2`, `p₀ = 0.28` (§2.5, §6.2,
/// D20), the meteor head's position at `u = t/T`. `p(0) = p₀`, so the gesture
/// is never seen starting from rest; `p(1) = 1` exactly.
///
/// Pinned by `meteor_enters_at_speed_never_from_rest`: displacement over the
/// first 8 ms is ≥ `0.10·L` at every `T ∈ {60, 90, 120}`.
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
pub fn spend(u: f32) -> f32 {
    let v = 1.0 - clamp01(u);
    v * v
}

/// `half-life`: `1` for a hold `H`, then `2^(−(age − H)/τ)` (§2.5, §5.6).
///
/// The envelope of every star and of the caret's landing re-light
/// (`Engine::caret_paint`, run on [`SPRING_SNAP_RESPONSE_S`] so re-light and
/// flare share an edge). `hold_s` and `tau_s` are per population; the shape is
/// shared. The caret's 220 ms cool is the host's and does not use this.
///
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
/// [`STAR_CULL_ALPHA`] (`2.32 = log₂(1/0.20)`). The class lives (225 / 340 /
/// 460 ms) follow from their `H`/`τ` pairs.
#[inline]
#[must_use]
pub fn half_life_life_s(hold_s: f32, tau_s: f32) -> f32 {
    hold_s + tau_s * -STAR_CULL_ALPHA.log2()
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

/// `spring-whip`'s damping ratio (§2.5). Under 1, so it OVERSHOOTS — which is
/// the point: the flying kitty is yanked backwards and springs past her
/// resting lead before settling.
pub const SPRING_WHIP_ZETA: f32 = 0.6;

/// `spring-whip`: the damped unit step (ω = 24 rad/s, ζ = 0.6) of §2.5:
/// `x(t) = 1 − e^(−ζωt)·(cos ω_d t + (ζ/√(1−ζ²))·sin ω_d t)`,
/// `ω_d = ω√(1−ζ²) = 19.2`.
///
/// The flying kitty's lead on a meteor (§7.2): `−0.30` cell at `t = 0`,
/// springing to `+LEAD_MAX 0.22`, settled by ≈ 180 ms (`e^(−ζωt) ≈ 0.07`).
#[inline]
#[must_use]
pub fn spring_whip(age_s: f32) -> f32 {
    if !age_s.is_finite() || age_s <= 0.0 {
        return 0.0;
    }
    let z = SPRING_WHIP_ZETA;
    let root = (1.0 - z * z).sqrt();
    let wd = SPRING_WHIP_OMEGA * root;
    let env = (-z * SPRING_WHIP_OMEGA * age_s).exp();
    1.0 - env * ((wd * age_s).cos() + (z / root) * (wd * age_s).sin())
}

/// The theme's one linear fade (§2.5, §6.11): under reduced motion every mark is
/// static and its alpha runs linearly to zero over 120 ms at the end of its life.
pub const REDUCED_MOTION_FADE_MS: f32 = 120.0;

// ---------------------------------------------------------------------------
// 5. The two coverage ceilings (§5.2, D1)
// ---------------------------------------------------------------------------
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
/// m3 98. Pinned by `a_star_is_a_peak_not_a_blob` with the ≥ 80/255 peak floor.
pub const FIELD_STAR_COV_CEIL: f32 = STAR_STACK_ADD * FIELD_STAR_COV_REQUEST_MAX;

/// Ceiling 2, the transient lane (§5.2, D1): fan, shed, erase-throw and
/// mini-fan stars fly over ink. Class centres: m1 118, m2 80, m3 61.
pub const TRANSIENT_STAR_COV_CEIL: f32 = 118.0;

/// L3's graded-jump roof (v1's `RAINBOW_JUMP_COV_CEIL`): the meteor's
/// `head_cov = min(118·bright, 160)` (§6.3) tops out here.
pub const JUMP_COV_CEIL: f32 = 160.0;

// ---------------------------------------------------------------------------
// Compile-time laws
// ---------------------------------------------------------------------------
//
// Relations between the shared numbers are const assertions, so a wrong value
// does not compile.

// §8.1 / D6: the fan's five paired stars fit inside the fan.
const _: () = assert!(FAN_HERO_N <= FAN_MAX_N);
// §6.1 / §6.12: the mini-fan band and the flight floor tile with no gap.
const _: () = assert!(MINI_FAN_MAX_CELLS + 1 == JUMP_MIN_CELLS);
// §6.12: a one-cell move is typing or an arrow, never a hop.
const _: () = assert!(MINI_FAN_MIN_CELLS >= 2);
// §6.9: cap 2 live meteors.
const _: () = assert!(FLIGHT_MAX_LIVE == 2);
// D1: two ceilings are genuinely two numbers, and the field lane is the
// brighter one.
const _: () = assert!(FIELD_STAR_COV_CEIL > TRANSIENT_STAR_COV_CEIL);
// §6.3 / L3: a full-grade head (`118 × 1.5 = 177`) exceeds the jump roof, so
// the roof binds; the roof is above the transient ceiling, so an ungraded head
// is never clipped.
const _: () = assert!(TRANSIENT_STAR_COV_CEIL * 1.5 > JUMP_COV_CEIL);
const _: () = assert!(TRANSIENT_STAR_COV_CEIL < JUMP_COV_CEIL);
// §2.5: the one sanctioned attack has a positive span.
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

// ---------------------------------------------------------------------------
// Small shared shapes
// ---------------------------------------------------------------------------

/// `t` clamped to `[0, 1]`, NaN resolving to 0 — the guard every curve above
/// runs first, so a non-finite age can never propagate into a coverage byte.
#[inline]
#[must_use]
pub fn clamp01(t: f32) -> f32 {
    if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) }
}

/// Hermite smoothstep on `[0, 1]`, clamped: `3u² − 2u³`.
#[inline]
#[must_use]
pub fn smoothstep01(t: f32) -> f32 {
    let u = clamp01(t);
    u * u * (3.0 - 2.0 * u)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `flight_ms`: the three published points, both clamps, and monotonicity.
    /// `phrase_rest_ms` (S5): the floor at 2.8 cps or faster, `2.5 · ioi`
    /// beyond, capped by the IOI ceiling; the tempo update matches the melody's
    /// law exactly.
    #[test]
    fn the_phrase_rest_is_the_floor_at_speed_and_the_tempo_multiple_below_it() {
        for ioi in [30.0, 100.0, 250.0, 360.0] {
            assert_eq!(phrase_rest_ms(ioi), 900.0, "ioi {ioi}: the floor");
        }
        assert_eq!(phrase_rest_ms(400.0), 1000.0);
        assert_eq!(phrase_rest_ms(500.0), 1250.0);
        assert_eq!(phrase_rest_ms(600.0), 1500.0);
        assert_eq!(
            phrase_rest_ms(5000.0),
            1500.0,
            "the IOI ceiling caps the rest"
        );
        assert_eq!(phrase_rest_ms(f32::NAN), phrase_rest_ms(IOI_DEFAULT_MS));
        assert!(phrase_rest_reached(900, 100.0));
        assert!(!phrase_rest_reached(899, 100.0));
        assert!(!phrase_rest_reached(1000, 500.0));
        assert!(phrase_rest_reached(1250, 500.0));
        // The tempo law: first / reset → default, a rest holds, else the EMA.
        assert_eq!(ioi_next(123.0, 100, true, false), IOI_DEFAULT_MS);
        assert_eq!(ioi_next(123.0, 2000, false, false), IOI_DEFAULT_MS);
        assert_eq!(ioi_next(123.0, 1000, false, true), 123.0);
        assert_eq!(ioi_next(200.0, 100, false, false), 150.0);
        assert_eq!(
            ioi_next(200.0, 10, false, false),
            115.0,
            "the gap is clamped at 30"
        );
    }

    /// `impact`: exactly 1 at the 8-cell floor, non-decreasing, saturating at
    /// the cap, and total on garbage.
    #[test]
    fn impact_is_monotone_floored_and_capped() {
        assert!(
            (impact(8.0) - 1.0).abs() < 1e-6,
            "8 cells is the floor: exactly 1"
        );
        for c in [
            0.0,
            1.0,
            4.0,
            7.9,
            -3.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert!(
                (impact(c) - 1.0).abs() < 1e-6,
                "{c}: below the floor (or garbage) is 1"
            );
        }
        let mut last = 0.0f32;
        for c in 1..=400 {
            let v = impact(c as f32);
            assert!(
                v.is_finite() && v >= last,
                "impact must be monotone: {c} → {v} < {last}"
            );
            assert!(v <= IMPACT_MAX);
            last = v;
        }
        assert!((impact(16.0) - 1.62).abs() < 0.01, "{}", impact(16.0));
        assert!((impact(40.0) - 3.09).abs() < 0.01, "{}", impact(40.0));
        assert!(
            (impact(48.0) - IMPACT_MAX).abs() < 1e-3,
            "a full line is the cap"
        );
        assert!(
            (impact(400.0) - IMPACT_MAX).abs() < 1e-6,
            "and it stays there"
        );
    }

    #[test]
    fn flight_ms_is_shared_and_clamped() {
        assert!((flight_ms(8.0) - 60.0).abs() < 1e-4, "8 cells → 60 ms");
        assert!((flight_ms(40.0) - 90.0).abs() < 1e-4, "40 cells → 90 ms");
        assert!((flight_ms(200.0) - 120.0).abs() < 1e-4, "clamped at 120 ms");
        assert!(
            (flight_ms(70.0) - 120.0).abs() < 1e-4,
            "≥ 70 cells → 120 ms"
        );
        // The floor binds below 10 cells (50 + 1·cells < 60).
        assert!((flight_ms(0.5) - 60.0).abs() < 1e-4);
        assert!(
            (flight_ms(-3.0) - 60.0).abs() < 1e-4,
            "negative → the floor"
        );
        assert!((flight_ms(f32::NAN) - 60.0).abs() < 1e-4, "NaN → the floor");
        // Monotone non-decreasing, and inside the band everywhere.
        let mut prev = 0.0;
        let mut c = 0.0;
        while c <= 240.0 {
            let t = flight_ms(c);
            assert!(t >= prev, "flight_ms must never decrease with distance");
            assert!(
                (FLIGHT_MIN_MS..=FLIGHT_MAX_MS).contains(&t),
                "flight_ms out of its band at {c} cells: {t}"
            );
            prev = t;
            c += 0.25;
        }
        // The Duration twin agrees with the millisecond one.
        assert!((flight(40.0).as_secs_f32() - 0.090).abs() < 1e-6);
    }

    /// §8.1's two shared counts, at their clamps and across the range.
    #[test]
    fn shed_and_fan_counts_are_clamped() {
        assert_eq!(shed_n(0.0), SHED_MIN);
        assert_eq!(shed_n(8.0), SHED_MIN, "the jump floor still sheds three");
        assert_eq!(shed_n(36.0), 3);
        assert_eq!(shed_n(48.0), 4);
        assert_eq!(shed_n(80.0), SHED_MAX);
        assert_eq!(shed_n(4000.0), SHED_MAX);
        assert_eq!(shed_n(f32::NAN), SHED_MIN);
    }

    /// D19: rain spacing is a function of the SHARED clock and stays inside
    /// [18, 24] ms for every flight the clock can produce.
    #[test]
    fn rain_spacing_rides_the_flight_clock() {
        assert!(
            (rain_spacing_ms(flight_ms(8.0)) - 18.0).abs() < 1e-4,
            "T/5 = 12 → floor"
        );
        assert!(
            (rain_spacing_ms(flight_ms(120.0)) - 24.0).abs() < 1e-4,
            "T/5 = 24"
        );
        let mut c = 0.0;
        while c <= 200.0 {
            let s = rain_spacing_ms(flight_ms(c));
            assert!((RAIN_SPACING_MIN_MS..=RAIN_SPACING_MAX_MS).contains(&s));
            // Five glints must fit inside the gesture's own −40 dB deadline.
            assert!(
                s * FAN_HERO_N as f32 <= 250.0,
                "the rain outlives §12's gesture"
            );
            c += 1.0;
        }
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

        // enter-at-speed: p(0) = p₀ EXACTLY (the frame-0 law), p(1) = 1.
        assert!(
            (enter_at_speed(0.0) - FLIGHT_P0).abs() < 1e-6,
            "the head is never at rest on frame 0 (T2, D20)"
        );
        assert!((enter_at_speed(1.0) - 1.0).abs() < 1e-6);
        // §6.2's worked frame: T = 100 ms, +8.3 ms → p ≈ 0.40.
        let p = enter_at_speed(8.3 / 100.0);
        assert!((0.38..0.42).contains(&p), "first ProMotion frame p = {p}");

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
        // …and that life is the m1 hero's published 460 ms (§5.6).
        assert!((life - 0.460).abs() < 2e-3, "m1 life {life} ≠ 460 ms");
        assert!(
            (half_life_life_s(0.080, 0.112) - 0.340).abs() < 2e-3,
            "m2 → 340 ms"
        );
        assert!(
            (half_life_life_s(0.040, 0.080) - 0.225).abs() < 2e-3,
            "m3 → 225 ms"
        );
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

        // spring-whip: starts at 0, DOES overshoot, and is inside 7 % of
        // unity by 180 ms.
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

    /// §5.2 / D1: two ceilings, one floor — and the published class centres
    /// all fit under the ceiling of their own lane.
    #[test]
    fn the_two_coverage_ceilings_hold_every_class() {
        assert!((FIELD_STAR_COV_CEIL - 143.35).abs() < 0.1, "2.35 × 61");
        for centre in [143.0_f32, 130.0, 98.0] {
            assert!(
                centre <= FIELD_STAR_COV_CEIL,
                "field class {centre} over ceiling 1"
            );
            assert!(centre >= 80.0, "field class {centre} under the peak floor");
        }
        for centre in [118.0_f32, 80.0, 61.0] {
            assert!(
                centre <= TRANSIENT_STAR_COV_CEIL,
                "transient class {centre} over ceiling 2"
            );
            assert!(
                centre >= 61.0,
                "transient class {centre} under the grain floor"
            );
        }
        // Relations between the ceilings (D1, L3) are compile-time assertions
        // beside the constants.
    }
}
