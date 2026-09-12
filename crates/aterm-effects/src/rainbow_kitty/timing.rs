// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE SHARED NUMBERS** of Rainbow Kitty v2 — every constant and curve that
//! more than one producer, or the glow *and* the synth, must agree on. Each is
//! defined EXACTLY ONCE, here, with its law written beside it.
//!
//! Design of record: `RAINBOW-KITTY-V2.md` §8.1 ("three shared numbers, each
//! defined once"), §2.5 (the seven named curves), §5.2 (the two coverage
//! ceilings), §6.1-6.2 (the meteor's trigger and clock), §13 (the token
//! bucket). Section references in this file are to that document.
//!
//! ## Why one file
//!
//! v1's measured #1 *coupling* defect was two clocks for one gesture: the
//! visual flight ran on `RAINBOW_METEOR_FLIGHT_S` (a fixed 0.12 s) while the
//! audio run ran on `CURSOR_SWEEP_STEP_S`, so the bell and the landing drifted
//! apart with distance and neither could be retuned without silently breaking
//! the other. §8.1's ruling is structural, not stylistic: the distance→duration
//! function, the glint budget, the shed count and the fan's hero count live in
//! ONE module that both `rainbow_kitty::meteor` and
//! `trail_sound::rainbow_kitty_v2` read. A second definition of any number
//! below is a bug, however arithmetically equal it happens to be today.
//!
//! ## What is NOT here
//!
//! The third shared number of §8.1 — **the arrival edge `t₀ + T`** — is an
//! `Instant`, not a constant: it is minted by [`super::meteor`] at the spawn
//! edge and read by the pin, the ring, the fan, the terminal flash, the kitty's
//! `land_at` and the audio bell. It cannot live in a constants module; what
//! lives here is the [`flight_ms`] that computes its offset, so that every
//! reader derives the SAME instant from the SAME distance.
//!
//! Two numbers the design names beside these are deliberately NOT restated,
//! because each already has exactly one owner and a copy here would be the
//! drift this file exists to prevent:
//!
//! * **The caret's τ 220 ms cool** (§2.5, §7.1). The caret block is ticked by
//!   the host, and its ignition heat — `TypingCadence`'s `energy`,
//!   `CadenceParams::half_life` 220 ms in `cursor_trail` — IS that cool. v2
//!   hands the host the display spine as `CaretSeam::paint` and the host
//!   takes `max(paint, energy)`; nothing in this tree runs a 220 ms clock.
//! * **"≤ 3 live glints, a fourth steals the oldest"** (§13, §14). That is a
//!   VOICE cap — the GLINT row of §14's lane table — and the lane table
//!   (`trail_sound::rainbow_kitty_v2::lane_cap`) owns every lane's cap. The
//!   glow's half of the glint law is the token bucket below, which knows
//!   nothing about voices.
//!
//! ## Units
//!
//! Durations are **milliseconds** (`f32`) unless a name ends in `_S`. Pixel
//! figures are at `ch = 18`; callers scale by `ch/18`. Coverages are additive
//! `0..=255` requests, pre-composite.

use std::time::Duration;

// ---------------------------------------------------------------------------
// 1. THE FLIGHT CLOCK (§8.1 no. 1, §6.2)
// ---------------------------------------------------------------------------

/// Fixed cost of a flight in ms, before distance — the part of `T` that is
/// "a jump happened at all" rather than "a jump went far" (§6.2).
pub const FLIGHT_BASE_MS: f32 = 50.0;

/// Milliseconds added per cell of path. At 1.0 ms/cell an 80-column fling is
/// 130 ms of *raw* flight before the ceiling bites — see [`FLIGHT_MAX_MS`].
pub const FLIGHT_PER_CELL_MS: f32 = 1.0;

/// Floor of the flight clock. Below this the head cannot be seen to travel at
/// all on a 60 Hz panel (T7: a 60 ms flight is 2-4 effect frames there), and a
/// shorter gesture would read as a teleport with a smear.
pub const FLIGHT_MIN_MS: f32 = 60.0;

/// Ceiling of the flight clock. §2.1's T3 says release is the art but the
/// *attack* is not: past 120 ms the caret has visibly arrived before its own
/// meteor does, which is exactly the lag tell v2 exists to delete.
pub const FLIGHT_MAX_MS: f32 = 120.0;

/// **THE ONE DISTANCE→DURATION FUNCTION** (§8.1 no. 1):
/// `T = clamp(50 + 1.0·cells, 60, 120)` ms.
///
/// Read by ALL FOUR of the couplings §8.1 enumerates, and by nothing else:
///
/// 1. the visual flight — `meteor.rs`'s `T`, hence the arrival edge `t₀ + T`;
/// 2. the audio rain spacing — see [`rain_spacing_ms`], which is a function of
///    this value and not of distance directly;
/// 3. the meteor bell's `Voice::delay` (§12.1), so the bell strikes on the
///    landing frame rather than near it;
/// 4. the Enter cadence's resolution delay (D9) — the resolution C, the tonic
///    dyad and the faraway bell all fire at `flight_ms(cells)`, not at the
///    fixed +60/+70 ms v1 used.
///
/// `cells` is the path length in CELL WIDTHS, diagonals included:
/// `cells = sqrt(dcol² + (drow·ch/cw)²)` (§6.2). Non-finite and negative input
/// resolves to [`FLIGHT_MIN_MS`] — a NaN path is a bug upstream, and the floor
/// is the answer that draws a correct short flight rather than a hang.
///
/// Pinned by `flight_ms_is_shared_and_clamped` (§20.1): 8 → 60, 40 → 90,
/// 200 → 120.
#[inline]
#[must_use]
pub fn flight_ms(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return FLIGHT_MIN_MS;
    }
    (FLIGHT_BASE_MS + FLIGHT_PER_CELL_MS * cells).clamp(FLIGHT_MIN_MS, FLIGHT_MAX_MS)
}

/// [`flight_ms`] as a [`Duration`], for the callers that add it to an
/// `Instant` to mint the arrival edge. One conversion, so nobody rounds
/// milliseconds to whole units on the way and lands a frame early.
#[inline]
#[must_use]
pub fn flight(cells: f32) -> Duration {
    Duration::from_secs_f32(flight_ms(cells) / 1000.0)
}

/// Exponent of the landing's IMPACT curve — concave, so the middle band of
/// jumps (16-40 cells) gets most of the new headroom and a wide-terminal
/// Ctrl-A does not climb forever.
pub const IMPACT_EXP: f32 = 0.7;

/// Where [`impact`] saturates: reached at ≈ 48 cells — a full line — so an
/// 80- or 200-cell jump lands on the cap instead of past it.
pub const IMPACT_MAX: f32 = 3.5;

/// How HARD a jump lands — ONE shared magnitude read by every distance-graded
/// landing law (fan count, fan reach, ring radius, ring life), so they cannot
/// come apart.
///
/// `impact(cells) = clamp((cells / 8)^0.7, 1, 3.5)`: exactly `1.0` at the
/// 8-cell meteor floor ([`JUMP_MIN_CELLS`]), so the floor landing is
/// byte-identical to what it was before distance bought anything — 1.62 at
/// 16 cells, 2.16 at 24, 3.09 at 40, and the cap from ≈ 48 on.
///
/// The owner's ask (2026-09-08): "a bigger impact splash that scales more
/// with the distance traveled." Before this, count saturated at ~16 cells
/// and reach at ~34; from 40 to 200 cells nothing grew at all.
///
/// Guards mirror [`flight_ms`]: a NaN, infinite or non-positive distance is
/// the floor, never a panic and never a runaway.
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

/// Head position on the path at `t = 0` — the frame the caret is first
/// observed at its landing (D20: `p₀ = 0.28`, *everywhere*, in both halves).
///
/// This single number is the frame-0 law (T2) made arithmetic: v1 drew nothing
/// on the spawn frame (`emit_rainbow_jumps`'s `len < 1.0 → continue`) and the
/// eye read the whole gesture as late. See [`enter_at_speed`].
pub const FLIGHT_P0: f32 = 0.28;

/// Exponent of the `enter-at-speed` easing (§2.5). Chosen so the head is
/// already past 0.40 of the path on the first ProMotion frame after spawn
/// (+8.3 ms at `T = 100`) — see §6.2's frame table.
pub const FLIGHT_ENTER_EXP: f32 = 2.2;

/// Milliseconds AFTER the arrival edge by which every meteor pixel is off the
/// glass. §6.2 wrote 320 ("any meteor light after `T + 350` ms is a bug");
/// **600 since 2026-09-08**, the owner's ruling: "I want the meteor to have
/// rainbow! be a bigger more special rainbow impact!" — the 320 was restraint,
/// and the impact now carries a 420 ms shockwave, a 260 ms splash and a
/// shower of sparks whose longest lives 560 ms. The FLIGHT is untouched
/// ([`flight_ms`]): responsiveness is the attack, and the attack did not
/// move — only the release grew. Read by `meteor.rs` alone; the pool's
/// `end()` is `arrival + this`, and every impact mark is asserted inside it
/// at compile time there.
pub const FLIGHT_OFF_GLASS_MS: f32 = 600.0;

/// The retire-previous finish (§6.9): a new meteor whose corridor comes within
/// 1 `ch` of a live one jumps that one to its post-arrival fade with this `R`.
/// Never a pop, never a fade-in-place.
pub const FLIGHT_RETIRE_MS: f32 = 60.0;

/// **Cap 2 live meteors** (§6.9; v1 allowed 4 and they read as rulers piling
/// up). A third evicts the oldest through the same [`FLIGHT_RETIRE_MS`]
/// finish, so an eviction is a gesture and not a disappearance.
pub const FLIGHT_MAX_LIVE: usize = 2;

/// The same-row jump floor in cells (§6.1; v1's `RAINBOW_JUMP_MIN`, kept —
/// "scrubbing must stay calm"). At or above this a move flies the full meteor
/// anatomy; below it, see [`MINI_FAN_MAX_CELLS`].
pub const JUMP_MIN_CELLS: u16 = 8;

/// Smallest same-row hop that draws a mini-fan (§6.12 / D17). A one-cell move
/// is typing or an arrow, and owns the ribbon head cell instead.
pub const MINI_FAN_MIN_CELLS: u16 = 2;

/// Largest same-row hop that draws a mini-fan — one below [`JUMP_MIN_CELLS`],
/// so the two populations tile the whole range with no gap and no overlap
/// (§6.12: no flight, no train, 1 m2 + 2 m3, gone by 245 ms).
pub const MINI_FAN_MAX_CELLS: u16 = JUMP_MIN_CELLS - 1;

/// **THE ECHO PATIENCE**, seconds — how long a typed press stays on the
/// engine's echo ledger (`super::EchoLedger`) waiting for the caret advance
/// that is its echo, before it is forgotten as swallowed.
///
/// The host's licence window is `0.25 s` (`CursorGlow::TYPE_HINT_FRESH`), and
/// an echo judged later than that is refused at the seam — correctly, for
/// program output, and at the cost of the glyph cell when the echo was merely
/// LATE: a TUI whose render loop stalled. Measured on the owner's machine
/// (2026-09-10): a scripted typist on an idle Claude Code prompt saw echoes of
/// 2-31 ms, but the instance's own echo ledger held a `1373 ms` worst case from
/// real use, and `input_p99 = 268 ms`. Two seconds covers that worst case with
/// margin and stays well inside the ribbon's own chain window
/// (`ribbon::CHAIN_GAP_MAX`, 5 s), the rhythm the mark already treats as one
/// burst; a press older than this has no echo coming that the eye would still
/// pair with the key. It is the same two seconds the host gives an unpaid
/// press credit (`CursorGlow::RAINBOW_COALESCE_CREDIT_LIFE`): one patience
/// for one press, whichever layer is asked.
pub const ECHO_PATIENCE_S: f32 = 2.0;

/// The most cells one observed move may lay from the echo ledger — the host's
/// own coalesced-sweep cap (`CursorGlow::RAINBOW_TYPED_SWEEP_MAX`, which is
/// `TYPED_STAMP_DEPTH`, 32 since the host's own ledger fix of 2026-09-09):
/// more presses than that cannot be paired with any one move, so the ledger
/// never holds more either.
pub const ECHO_LEDGER_DEPTH: usize = crate::cursor_glow::TYPED_STAMP_DEPTH;

// ---------------------------------------------------------------------------
// 2. THE SHARED COUNTS (§8.1, "plus two shared counts")
// ---------------------------------------------------------------------------

/// Fewest shed fragments a meteor sputters, whatever the distance — a jump
/// that sheds nothing has no sense of *burning* (§6.5 layer 5).
pub const SHED_MIN: u16 = 3;

/// Most shed fragments a meteor sputters. Past six the path reads as a dotted
/// line rather than as debris.
pub const SHED_MAX: u16 = 6;

/// Cells of path per shed fragment.
pub const SHED_CELLS_PER: f32 = 12.0;

/// **`SHED_N(cells) = clamp(cells/12, 3, 6)`** (§8.1, §5.6, §6.5 layer 5) —
/// how many m3 grains a meteor sheds along its path as the head passes their
/// hashed stations. Transient-lane and **silent**: the rain glints pair with
/// the landing fan, not with these (D6).
///
/// Shared because §6.5's layer table sizes the quad budget from it and §12's
/// gesture must not chime once per fragment.
#[inline]
#[must_use]
pub fn shed_n(cells: f32) -> u16 {
    if !cells.is_finite() || cells <= 0.0 {
        return SHED_MIN;
    }
    ((cells / SHED_CELLS_PER) as u16).clamp(SHED_MIN, SHED_MAX)
}

/// **`FAN_HERO_N = 5`** (§8.1, D6): the landing fan ALWAYS carries 1 gold m1 +
/// 4 m2, and the meteor's five rain glints pair 1:1 with those five in throw
/// order. The count is shared so the visual census
/// (`the_fan_is_one_hero_four_m2_and_grains`) and the audio run length can
/// never disagree — a sixth glint with no star to ride is the bug this
/// constant exists to make impossible.
pub const FAN_HERO_N: usize = 5;

/// Fan stars in total, ceiling included:
/// `n = min(19 + 3.6·(impact − 1), 28) + party·8` never exceeds this (§6.5
/// layer 11 wrote `min(5 + cells/1.8, 14) + party·4` under 18; **doubled
/// 2026-09-08** — "a fan of coloured stars twice today's count" — and
/// distance-graded the same day through [`impact`], which moves the count
/// from the floor's 19 to the ceiling's 28 at the cap instead of saturating
/// at 16 cells). Shared with the sky, which sizes its throw from it.
pub const FAN_MAX_N: usize = 36;

/// Rain-glint spacing floor in ms (D19).
pub const RAIN_SPACING_MIN_MS: f32 = 18.0;

/// Rain-glint spacing ceiling in ms (D19). The whole gesture is −40 dB by
/// `T + 250`, so five glints cannot be allowed to outlive it.
pub const RAIN_SPACING_MAX_MS: f32 = 24.0;

/// Spacing between the meteor's five rain glints, `s = clamp(T/5, 18, 24)` ms
/// (D19) — a function of the SHARED flight clock, never of distance directly,
/// so retuning [`flight_ms`] retunes the rain with it.
#[inline]
#[must_use]
pub fn rain_spacing_ms(flight_ms: f32) -> f32 {
    if !flight_ms.is_finite() {
        return RAIN_SPACING_MIN_MS;
    }
    (flight_ms / FAN_HERO_N as f32).clamp(RAIN_SPACING_MIN_MS, RAIN_SPACING_MAX_MS)
}

// ---------------------------------------------------------------------------
// 3. THE TOKEN BUCKET (§8.1 no. 2, §13, D5)
// ---------------------------------------------------------------------------

/// Tokens the stardust bucket regains per second — hence "≤ 4 glints/s by
/// construction" (§13). The bucket is refilled on the frame clock inside
/// `tick()`: `tokens = min(GLINT_CAP, tokens + GLINT_REFILL_PER_S · dt)`.
pub const GLINT_REFILL_PER_S: f32 = 4.0;

/// Bucket depth. Three banked tokens let a burst of capitals chime as a burst
/// (three glints inside one 250 ms window) without letting a long idle bank a
/// whole arpeggio.
///
/// The bucket's full law (D5), enforced at the ONE spend site —
/// `stardust.rs`'s `deal_star` — and restated here beside its numbers: **only
/// an m1 spends a token**, and a spent token cues exactly one glint at that
/// star's column on that frame; an *earned* hero (capital, `!`, kitty Delight,
/// the landing fan's own slot) with no token is **born anyway and is silent**
/// (light is never rationed); a *dealt* hero with no token degrades to m2;
/// m2/m3 never touch the bucket; meteor-lane stars are exempt entirely, and a
/// meteor **empties** the bucket on its move edge, so typing heroes for the
/// ~250 ms after a meteor are born as m2 — deliberately, because nothing may
/// chime on top of the rain. The tokens are the WHOLE of the glow's law: how
/// many glint voices may sound at once is the synth's lane cap (see the
/// module doc's "What is NOT here").
pub const GLINT_CAP: f32 = 3.0;

// ---------------------------------------------------------------------------
// 4. THE SEVEN NAMED CURVES (§2.5)
// ---------------------------------------------------------------------------
//
// "The theme's only easing vocabulary." No linear fades; the single exception
// is reduced motion (§6.11), whose 120 ms linear alpha is
// `REDUCED_MOTION_FADE_MS` below and is deliberately NOT given a curve
// function — it is a fade, not a member of the vocabulary.

/// Duration of the ribbon head cell's one ramp-in, seconds (v1's
/// `RAINBOW_EDGE_IN_S`, kept verbatim). T3: *nothing else eases in.*
pub const EDGE_IN_S: f32 = 0.018;

/// `edge-in` — smoothstep over 18 ms. The one sanctioned attack in the theme
/// (§2.5, §4): the ribbon head cell coming up to full at the hand.
///
/// Takes an AGE in seconds, not a normalized `u`, because its span is fixed by
/// [`EDGE_IN_S`] and every call site has an age.
#[inline]
#[must_use]
pub fn edge_in(age_s: f32) -> f32 {
    smoothstep01(age_s / EDGE_IN_S)
}

/// `enter-at-speed` — `p(u) = 1 − (1 − p₀)·(1 − u)^2.2`, `p₀ = 0.28`
/// (§2.5, §6.2, D20). The meteor head's position along its path at `u = t/T`.
///
/// The whole point is `p(0) = p₀`, not `p(0) = 0`: the head is ALREADY 28 % of
/// the way across on the frame the caret is observed at its landing, so the
/// gesture is never seen starting from rest. `p(1) = 1` exactly.
///
/// Pinned by `meteor_enters_at_speed_never_from_rest` (§20.1): the head's
/// displacement between spawn and spawn + 8 ms is ≥ `0.10·L` at every
/// `T ∈ {60, 90, 120}`.
#[inline]
#[must_use]
pub fn enter_at_speed(u: f32) -> f32 {
    let u = clamp01(u);
    1.0 - (1.0 - FLIGHT_P0) * (1.0 - u).powf(FLIGHT_ENTER_EXP)
}

/// `suck-in` — `u^1.8` (§2.5). The position law of any root or tail
/// RETRACTING TOWARD THE CARET: the meteor train's colour root over its 300 ms
/// `R` (§6.8), the ribbon's backspace retract, the pin's arms whipping inward.
///
/// T5: leaving marks move toward the caret; they do not fade in place.
#[inline]
#[must_use]
pub fn suck_in(u: f32) -> f32 {
    clamp01(u).powf(1.8)
}

/// `spend` — `(1 − u)²` (§2.5). The ALPHA law of a retracting mark, and of the
/// focus-lost ember (§8.2). Paired with [`suck_in`]: `suck_in` says where the
/// mark went, `spend` says what it cost to get there.
#[inline]
#[must_use]
pub fn spend(u: f32) -> f32 {
    let v = 1.0 - clamp01(u);
    v * v
}

/// `half-life` — `1` for a hold `H`, then `2^(−(age − H)/τ)` (§2.5, §5.6).
///
/// The envelope of every star, and of the caret's landing re-light
/// (`Engine::caret_paint`, which runs it on [`SPRING_SNAP_RESPONSE_S`] so the
/// re-light and the flare share one edge). `hold_s` and `tau_s` are
/// per-population (§5.6's table); the SHAPE is shared so a star's class can be
/// read off its life without knowing which population minted it. §2.5's other
/// half-life, the caret's τ 220 ms cool, is the HOST's (module doc, "What is
/// NOT here") and is not run through this function.
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

/// Star cull floor (D3): a star is dropped once [`half_life`] falls below
/// this. `0.12` would leave a 36-cov grain vanishing at ~10/255 — invisible
/// long before the arithmetic said so, which is how v1 shipped grains that
/// "greyed out" instead of winking.
pub const STAR_CULL_ALPHA: f32 = 0.20;

/// Chroma cull floor (C3, D3): coloured METEOR-TRAIN pixels are culled below
/// this rather than allowed to go grey. Distinct from [`STAR_CULL_ALPHA`] on
/// purpose — the train is a broad mark whose dim tail is still chromatic, a
/// star is a point whose dim tail is nothing.
pub const CHROMA_CULL_ALPHA: f32 = 0.12;

/// `H + 2.32·τ` — the life a [`half_life`] envelope actually has, given
/// [`STAR_CULL_ALPHA`] (§2.5, §5.6). `2.32 = log₂(1/0.20)`.
///
/// The published class lives (225 / 340 / 460 ms) are this function of the
/// published `H`/`τ` pairs; a τ that does not reproduce its published life is
/// a mis-transcription, not a taste call.
#[inline]
#[must_use]
pub fn half_life_life_s(hold_s: f32, tau_s: f32) -> f32 {
    hold_s + tau_s * -STAR_CULL_ALPHA.log2()
}

/// `spring-snap`'s response time, seconds (§2.5, §7.1). Read as the spring's
/// NATURAL PERIOD: `ω = 2π/response`, so the mark is visually settled (residual
/// under 1.5 %) at exactly `response`. Fixing the reading here, once, is what
/// stops two producers from picking two different `ω` off the same "0.18 s".
pub const SPRING_SNAP_RESPONSE_S: f32 = 0.18;

/// `spring-snap`'s natural frequency, rad/s (≈ 34.9) — see
/// [`SPRING_SNAP_RESPONSE_S`].
pub const SPRING_SNAP_OMEGA: f32 = 2.0 * std::f32::consts::PI / SPRING_SNAP_RESPONSE_S;

/// `spring-snap` — a CRITICALLY DAMPED unit step (ζ = 1.0), response 0.18 s:
/// `x(t) = 1 − (1 + ωt)·e^(−ωt)` (§2.5).
///
/// Used for the caret flare relaxing toward its field stop (§7.1) and for the
/// kitty's landing. Critically damped means **no overshoot**: the caret must
/// never dip past its own colour on the way back, because a caret that
/// undershoots reads as "not arrived" (§7.1's second bullet).
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

/// `spring-whip` — the DAMPED unit step (ω = 24 rad/s, ζ = 0.6) of §2.5:
/// `x(t) = 1 − e^(−ζωt)·(cos ω_d t + (ζ/√(1−ζ²))·sin ω_d t)`,
/// `ω_d = ω√(1−ζ²) = 19.2`.
///
/// The flying kitty's lead on a meteor (§7.2): `lead = −0.30` cell at `t = 0`,
/// springing to `+LEAD_MAX 0.22`, settled by ≈ 180 ms — which is
/// `e^(−ζωt) = e^(−2.6) ≈ 0.07` at `t = 0.18`, the number the spec quotes.
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

/// The ONE linear fade in the theme (§2.5, §6.11): under reduced motion every
/// mark is static and its alpha runs to zero linearly over 120 ms at the end
/// of its normal life. Deliberately a constant and not a curve function — it
/// is the exception, and giving it an eighth entry in the vocabulary would
/// invite an eighth easing.
pub const REDUCED_MOTION_FADE_MS: f32 = 120.0;

// ---------------------------------------------------------------------------
// 5. THE TWO COVERAGE CEILINGS (§5.2, D1)
// ---------------------------------------------------------------------------
//
// "Two ceilings, one floor." One star FAMILY, two PRICES, because the two
// lanes composite over different grounds: sky stars live above the ribbon for
// 225-460 ms and are priced by the sparkle law; transient stars fly OVER INK
// for ≤ 340 ms and are priced by the jump/transient cap. v1 carried a 33-entry
// `RAINBOW_BAND_COV_CAPS` table *beside* `RAINBOW_UNDER_COV_CAP` — two ceilings
// for one bed — and that is exactly what §19 deletes.

/// The additive coverage ONE sky mark may request (v1's
/// `RAINBOW_SPARKLE_COV_MAX`, unchanged). Field and strike stars are priced
/// from this.
pub const FIELD_STAR_COV_REQUEST_MAX: f32 = 61.0;

/// `push_twinkle_star`'s own composite multiplier — its two crossing body bars
/// plus its nucleus (`STAR_CORE_ADD 0.35`) stack to exactly this (v1's
/// `STAR_STACK_ADD`). No class may add a duplicate core rect on top (D1).
pub const STAR_STACK_ADD: f32 = 2.35;

/// **CEILING 1 — the field / strike lane** (§5.2): a sky star's COMPOSITED
/// centre may not exceed `STAR_STACK_ADD × 61 = 143`.
///
/// Every published class centre is at or under it: field m1 143, m2 130,
/// m3 98. Pinned by `a_star_is_a_peak_not_a_blob` (§20.1) together with the
/// ≥ 80/255 floor that makes a star a *peak*.
pub const FIELD_STAR_COV_CEIL: f32 = STAR_STACK_ADD * FIELD_STAR_COV_REQUEST_MAX;

/// **CEILING 2 — the transient lane** (§5.2, D1): fan, shed, erase-throw and
/// mini-fan stars fly OVER INK, so they are priced by v1's
/// `RAINBOW_TRANSIENT_COV_CAP` instead. Class centres: m1 118, m2 80, m3 61.
pub const TRANSIENT_STAR_COV_CEIL: f32 = 118.0;

/// L2's over-ink ledger cap (v1's `OVER_INK_COV_CAP`), restated here because
/// the transient lane's whole justification is that it is ledger-held over
/// probed glyph cells and the caret cell is exempt.
pub const OVER_INK_COV_CAP: f32 = 47.0;

/// L3's graded-jump roof (v1's `RAINBOW_JUMP_COV_CEIL`): the meteor's
/// `head_cov = min(118·bright, 160)` (§6.3) tops out here.
pub const JUMP_COV_CEIL: f32 = 160.0;

// ---------------------------------------------------------------------------
// The laws that hold at compile time
// ---------------------------------------------------------------------------
//
// Relations between the shared numbers that a mis-transcription would break
// are pinned as CONST assertions, so the module does not compile with them
// wrong — a test that merely re-evaluated a constant would be a tautology.

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
// §6.3 / L3: a full-grade head (`118 × 1.5 = 177`) asks for MORE than the
// jump roof, so the roof genuinely binds — and the roof sits above the
// transient ceiling, so an ungraded head is never clipped by it.
const _: () = assert!(TRANSIENT_STAR_COV_CEIL * 1.5 > JUMP_COV_CEIL);
const _: () = assert!(TRANSIENT_STAR_COV_CEIL < JUMP_COV_CEIL);
// §2.5: the one sanctioned attack has a positive span.
const _: () = assert!(EDGE_IN_S > 0.0);

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

    /// §20.1 `flight_ms_is_shared_and_clamped`: the three published points, the
    /// two clamps, and monotonicity across the whole open range. This is the
    /// number the synth resolves through the SAME function, so an off-by-one
    /// here desynchronises the bell from the pin.
    /// `impact` is the ONE magnitude every distance-graded landing law reads,
    /// so its shape is pinned here once: floor exactly 1 at the 8-cell meteor
    /// floor (the floor landing must not change), non-decreasing over the whole
    /// range, saturating at the cap, and total on garbage.
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

    /// §2.5, endpoint by endpoint. Every curve is pinned at both ends because
    /// the ends are what the laws talk about: frame 0 exists (T2) and the mark
    /// is gone by its published life (T6).
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
        // "Settled ≈ 180 ms" is the ENVELOPE's statement: at t = 0.18 the
        // decay is e^(−ζωt) = e^(−2.592) ≈ 0.075, and the step response's
        // amplitude factor is at most 1/√(1−ζ²) = 1.25, so the residual is
        // bounded by 0.094 — never further out than a tenth.
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
        // The relations BETWEEN the ceilings (D1, L3) are compile-time
        // assertions beside the constants; a graded head's roof is pinned
        // there, not here.
    }
}
