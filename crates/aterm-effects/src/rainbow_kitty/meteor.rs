// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE METEOR** — the one thing in v2 that streaks. White-hot head already a
//! quarter of the way across on the frame the caret lands, wearing a rainbow
//! corona; a train that IS the rainbow — the whole ROYGBIV on every frame,
//! painted onto the row as the head passes so the colours stream backwards —
//! that dies white-first; sputtering fragments; a terminal flash; and an
//! IMPACT: a rainbow shockwave, a splash along the landing row, a fan of
//! coloured stars and a shower of coloured sparks that fall and fade, all
//! off glass by `T + 600`.
//!
//! ## The 2026-09-08 ruling — bigger, more colour, more emphasis
//!
//! The owner, verbatim: "I feel like you are diminishing the specialness and
//! emphasis of this theme? why?", "I want the meteor to have rainbow! be a
//! bigger more special rainbow impact!", and earlier "make this rainbow theme
//! truly magical and special and dynamic and beautiful". The restraint of
//! 2026-09-05 — a coma lerped half to white, a two-stop bar on a short jump,
//! a 1.3 `ch` ring spent in 180 ms, five hero stars, everything off glass at
//! `T + 320` — is REJECTED. What replaced it, and where each law now lives:
//!
//! * **The train is the rainbow** ([`arc_gain`], [`Meteor::arc`]): the arc
//!   is scaled so a WHOLE sweep fits inside the frame-0 span (`0.28·L`),
//!   whatever the distance, and it is fixed to the ROW rather than to the
//!   head — the head lays the colours down and flies on, so relative to the
//!   head they stream backwards at the head's own speed. Wider at the head
//!   ([`W_BASE_CH`]), feathered at the tail ([`WIDTH_FLOOR_SHARE`]),
//!   saturated over its whole length ([`COLOUR_FALLOFF_SHARE`]).
//! * **The corona** ([`COMA_PETALS`]): the nucleus stays white-hot — that is
//!   what makes the colours read as fire — and its coma is seven petals of
//!   the seven named stops orbiting it, not a white-lerped blob.
//! * **The impact** (§6.5 layers 9-11, extended): the ring is a SHOCKWAVE,
//!   `3.0 ch` where it was `1.3`, 420 ms where it was 180, its circumference
//!   carrying the spectrum twice over and spinning ([`RING_R_CH`],
//!   [`RING_MS`], [`RING_SWEEPS`]); the fan is twice the count
//!   ([`FAN_N_BASE`]); a [`draw_splash`] of the bed's ink at the transient
//!   cap bursts along the landing row; and [`draw_sparks`] throws a shower
//!   of coloured sparks that fall under gravity and fade.
//! * **The impact, second round (2026-09-08, "BIGGER and MORE SPECIAL")** —
//!   at the owner's cell the first round's impact read as a thin rainbow ring
//!   with tiny coloured dots: polite. Now the arrival is a WHITE-HOT FLASH
//!   ([`draw_flash`]): the caret cell and its two neighbours burst white for
//!   [`FLASH_BURST_MS`] and die INTO the spectrum — white leaving, colour
//!   arriving, one way ([`FLASH_COLOUR_MS`]); over a blank cell the burst
//!   asks [`FLASH_FULL_COV`], over a glyph cell the transient cap, so the
//!   text under it stays legible. The shockwave's stroke is two and a half
//!   times the first round's ([`RING_THICK_SHARE`]) and holds near the cap
//!   for its first 120 ms ([`RING_COV_HOLD_U`]) at the same three-cell-height
//!   reach. The sparks are twice as many ([`SPARK_N_BASE`]) and each is a
//!   small HALOED STAR — the family's own four-point star at 5–9 px
//!   ([`SPARK_ARM_MIN_PX`]) with a tinted core and a saturated halo — that
//!   climbs, hangs and falls as before. The splash leaves the text: the
//!   under-ink splash is gone, and in its place a rainbow band runs through
//!   the SKY BAND above the row and its mirror below the row, six cells
//!   either side ([`draw_splash`]), at the transient ceiling, gated cell by
//!   cell on the sky's own glyph probe ([`SkyMask`]) so it never enters a
//!   glyph's rows. Under reduced motion the landing is the flash and the
//!   colours, static — no expansion, no sparks, no pin — on the theme's one
//!   linear fade.
//! * **The impact, GRADED BY DISTANCE (2026-09-08, the merge of the second
//!   round with the peer's landing grade)** — the owner, to the peer: *"a
//!   bigger impact splash that scales more with the distance traveled."*
//!   ONE magnitude, [`impact`] (`timing.rs`: `clamp((cells/8)^0.7, 1,
//!   3.5)`), scales the second round's look: the shockwave's full radius
//!   (`3 → 6 ch`, [`ring_full_radius`]) and life (`480 → 600 ms`,
//!   [`ring_ms`]), the fan's count (`19 → 28`, [`FAN_N_PER_IMPACT`]) and
//!   reach (`2.8 → 6.8 ch`, [`FAN_REACH_PER_IMPACT_CH`]). The 8-cell floor is
//!   byte-identical to the second round; a full line lands on the cap. The
//!   ring grows ALONG the line — its vertical semi-axis stays the floor
//!   ring's `1.5 ch` ([`RING_RISE_MAX_CH`]) — and its stroke keeps the second
//!   round's ceiling, so a bigger landing is wider and longer, never taller
//!   or fatter. Every classic style's landing ring takes the same grade
//!   (`cursor_glow::classic_ring_radius_factor`).
//! * **What did not move**: the flight clock (`timing::flight_ms`, §8.1 —
//!   responsiveness is untouched), frame-0, idle → zero, zero allocation
//!   per frame, the bed's legibility ceiling, the pin, and the other nine
//!   styles.
//!
//! Design of record: `RAINBOW-KITTY-V2.md` §6 in full — §6.1 (trigger and
//! axis), §6.2 (timing), §6.3 (width), §6.4 (the meteor's own arc, D4), §6.5
//! (the layer table), §6.6 (the frame-0 acceptance picture), §6.7 (the arrival
//! edge), §6.8 (train fade), §6.9 (retire-previous), §6.10 (the light fork),
//! §6.11 (Enter / vertical / reduced motion), §6.12 (sub-floor hops) — plus
//! D8, D13, D15, D17, D20 and §18's budget rules.
//!
//! ## The two laws that decide everything else
//!
//! * **The frame-0 law (T2).** `t = 0` is the frame the caret is first
//!   observed at its landing. On THAT frame the head is at
//!   [`super::timing::FLIGHT_P0`] of the path, the train is lit exponentially
//!   behind it back to the launch cell, the caret flares, the flying kitty is
//!   AT the landing with its impulse and whip, and every shed fragment with
//!   `s < 0.28·L` is born.
//!   **Light exists at both ends.** v1 drew *nothing* on this frame
//!   (`emit_rainbow_jumps`'s `len < 1.0 → continue`) and that is the lag tell
//!   v2 exists to delete.
//! * **The meteor does not read the field (C5, D4).** It walks [`arc_t`], the
//!   classic lay rate continued backwards from the caret's own stop,
//!   **reflected and never clamped**. v1's `rainbow_field_line_lit` continued
//!   the nearest laid cell's slope and CLAMPED it, so behind a short fresh run
//!   a Home/End read `t = 0` — one red bar across 28 cells, the measured #1
//!   visual defect. C2 still governs the ribbon; the meteor is a mark, not a
//!   field read.
//!
//! ## Where v1's blank frame actually was, and why the guard below is not it
//!
//! [`Meteors::emit`] refuses a path shorter than one device pixel. That is NOT
//! v1's `len < 1.0 → continue`: v1 tested the ANIMATED length — the head's
//! displacement from the launch, which its smoothstep-from-rest flight left at
//! literally zero on the spawn frame — and skipped the whole emit, halos
//! included. The guard here tests the STATIC path length `L`, which for any
//! flight the trigger admits (§6.1: eight cells, or a row change) is at least
//! `cw` px and never zero on any frame of the flight. The distinction is the
//! whole of T2 and is why it is written down here.
//!
//! ## What this file draws, and what it only mints
//!
//! It DRAWS §6.5's layers 1, 2, 3, 6, 7, 8, 9 and 10 — the two train layers and
//! the shoulder, the corona and nucleus, the terminal flash, the pin and the
//! shockwave — and the 2026-09-08 splash and sparks. It DECIDES, but neither
//! builds nor draws, layers 5 and 11 and §6.12's
//! mini-fan — the shed fragments, the landing fan and the hop's three stars —
//! because they are *stars*, and every star in v2 is built by ONE producer
//! ([`super::stardust`]: §5.4's glyph clearance, §5.6's motion laws, the one
//! star recipe of §5.7, D2). This file owns what only the meteor knows — WHERE
//! a fragment's station is and WHEN the head passes it, the landing's reach,
//! count and hero, the hop's landing cell — and hands each of those to the
//! sky's transient-lane entry points ([`Stardust::sow_shed`],
//! [`Stardust::sow_fan`], [`Stardust::sow_mini_fan`]) through
//! [`Meteors::sow_into`]. The star's velocity units, its throw window, its
//! sputter and its class envelope are stardust's, spelled once there.

use std::{fmt, mem};

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{
    BeamClip, BeamVertex, GlowBlend, GlowQuad, HaloMode, RainHalo, RibbonVertex, comet_beam,
    premul_rgb, ribbon_beam, ribbon_beam_v,
};

use crate::cursor_glow::Geom;
use crate::effect_util::{push_fx_rect, push_twinkle_star};
use crate::spectrum::{
    SPECTRUM_STOPS, spectrum, spectrum_snap, spectrum_snap_index, spectrum_stop,
};

use super::ribbon::{TALL_UP_CH, WALK_LAY_RATE};
use super::stardust::{AURORA_TALL_CH, FAN_RISE_MAX_CH, FanSow, GlyphProbe, ShedSow, Stardust};
use super::timing::{
    CHROMA_CULL_ALPHA, FLIGHT_ENTER_EXP, FLIGHT_MAX_LIVE, FLIGHT_OFF_GLASS_MS, FLIGHT_P0,
    FLIGHT_RETIRE_MS, JUMP_COV_CEIL, JUMP_MIN_CELLS, MINI_FAN_MAX_CELLS, MINI_FAN_MIN_CELLS,
    REDUCED_MOTION_FADE_MS, SHED_MAX, STAR_CULL_ALPHA, TRANSIENT_STAR_COV_CEIL, clamp01,
    enter_at_speed, flight, impact, shed_n, smoothstep01, spend, suck_in,
};
use super::{Cadence, Config, Ctx, Dir, Event, Frame, Licence};

// ---- §6.3 width -----------------------------------------------------------

/// Constant term of `w = ch·(0.44 + 0.34·mom)·(1 + 0.5·grade)` (§6.3, widened
/// 2026-09-08) — a cold short meteor is 7.9 px at `ch = 18`. §6.3 wrote 0.36
/// (6.5 px, "the ribbon's own thickness"); the owner's "bigger more special
/// rainbow impact" buys the head a fifth more body, and the tail is feathered
/// harder to pay for it ([`WIDTH_FLOOR_SHARE`]).
pub const W_BASE_CH: f32 = 0.44;

/// Momentum term of the width law (§6.3): hot is 12.6 px at `ch = 18`.
pub const W_MOM_CH: f32 = 0.34;

/// **DISTANCE BUYS LENGTH AND SPEED, NOT FAT** (§6.3). v1's grade gain was
/// 0.85; v2 caps the grade at ×1.5 with this 0.5. A hot 34-cell meteor is
/// 19 px — 1.05 `ch` — and no further.
pub const W_GRADE_GAIN: f32 = 0.5;

/// Cells at which `grade` saturates: `grade = clamp(cells/34, 0, 1)` (§6.3).
pub const GRADE_CELLS: f32 = 34.0;

/// Base head coverage before the brightness grade (§6.3); the graded value is
/// `min(118·(1 + 0.5·grade), 160)` and 160 is
/// [`super::timing::JUMP_COV_CEIL`] (L3). This is the WHITE heat — the
/// shoulder, the white train layer and the nucleus (§6.5 layers 2, 3, 7).
pub const HEAD_COV_BASE: f32 = TRANSIENT_STAR_COV_CEIL;

/// Brightness grade gain (§6.3). Brightness IS still graded by distance; only
/// the width grade was cut. **Dark theme only** — §6.10: "no brightness grade
/// on light (the arm is legibility-bounded)".
pub const BRIGHT_GRADE_GAIN: f32 = 0.5;

/// **THE COLOUR TRAIN'S HEAD COVERAGE** (§6.5 layer 1, §3.4, §6.6): the
/// transient ceiling, and NOT graded. §6.5 writes layer 1 as
/// `head_cov·exp(−s/(0.35·L))` "capped at 118 for `s ≤ 0.06·L`"; §6.6's
/// acceptance picture of a grade-1 flight has the colour train at
/// `118·exp(−s/252)`, "still 53 at the launch cell"; and §20.1's law is
/// `cov(0.61·L)/cov(0.06·L) = e⁻¹`, monotone beyond the shoulder. Only one
/// reading satisfies all three: the colour layer starts at 118 and decays
/// from there, the grade being spent on the white heat and the nucleus
/// ([`HEAD_COV_BASE`] × `bright`). A graded colour base clipped at 118 over
/// the shoulder would step from 118 to `160·e^(−0.06/0.35) = 135` one
/// shoulder-length behind the head, brightening the train exactly where the
/// eye is leaving the heat, and would break the e⁻¹ law on every graded
/// flight. With the base at the ceiling, §3.4's bargain — white wins where
/// the eye reads the head — holds at every `s`, not just inside `0.06·L`.
pub const COLOUR_TRAIN_COV: f32 = TRANSIENT_STAR_COV_CEIL;

// ---- §6.4 the arc ---------------------------------------------------------

/// Cells of path per `t`-unit of the meteor's own arc — the classic walk's lay
/// rate, continued (§6.4). Deliberately spelled as `1 / WALK_LAY_RATE` rather
/// than as a literal 36: the ribbon's rate and the meteor's rate are ONE
/// number, and a change to the walk must move the arc with it.
pub const ARC_CELLS_PER_T: f32 = 1.0 / WALK_LAY_RATE;

/// `tri(x) = 1 − |1 − (x mod 2)|` (§6.4) — the triangle wave that REFLECTS the
/// continued walk instead of clamping it.
///
/// This one line is D4. An 80-cell meteor spans `80/36 = 2.22` t-units — 2.2
/// full ROYGBIV sweeps — so it reads as a rainbow over a fresh run *or* a dead
/// prompt, in both directions.
#[inline]
#[must_use]
pub fn tri(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    1.0 - (1.0 - x.rem_euclid(2.0)).abs()
}

/// The meteor's own spectrum position at `cells_behind` cells behind the head,
/// phase-locked to `t_land` — the caret's own field `t` at the landing cell
/// (§6.4):
///
/// ```text
/// t_raw = t_land − cells_behind/36
/// t_m   = tri(t_raw)
/// ```
///
/// The station under the caret is the caret's own stop BY CONSTRUCTION
/// (`cells_behind = 0` ⇒ `tri(t_land) = t_land` for `t_land ∈ [0, 1]`), which
/// is what `an_80_cell_meteor_always_spans_the_arc` checks last — and for a
/// `t_land` past `1.0` (the walk's second leg, a long line) `tri(t_land)` is
/// exactly what the ribbon draws for that cell, because `t_land` is latched
/// RAW and the ribbon folds the same raw walk through the same `tri`. The one
/// `tri` here is the only reflection: locking a pre-reflected `t_land` would
/// fold twice and run the train the wrong way along the walk.
#[inline]
#[must_use]
pub fn arc_t(t_land: f32, cells_behind: f32) -> f32 {
    tri(t_land - cells_behind * WALK_LAY_RATE)
}

/// **THE TRAIN IS THE RAINBOW** (owner, 2026-09-08): how many `t`-units of
/// the reflected walk the train carries INSIDE ITS FRAME-0 SPAN
/// (`FLIGHT_P0·L`), whatever the distance. At the classic lay rate alone an
/// 8-cell hop's frame-0 train (2.2 cells) carried 0.06 of a unit — one colour
/// — and a 48-cell Ctrl-A's 0.37: "a bright bar with a short colour tail".
/// **1.5, not 1.0**: `tri` reflects, so one unit starting at `t = 0.58`
/// walks `0.58 → 1 → 0.58` and shows four stops; a unit and a half covers
/// the whole spectrum from every phase (`x → 1 → 0` spans it inside 1.5
/// units for any `x`), which is what makes every frame of every flight a
/// rainbow.
pub const ARC_FRAME0_SWEEPS: f32 = 1.5;

/// The multiplier on the classic lay rate that puts [`ARC_FRAME0_SWEEPS`]
/// inside the frame-0 span: `max(1, sweeps / (p₀·cells/36))`. Never below
/// one — a flight long enough to carry the sweep at the walk's own rate
/// (≥ 129 cells) keeps the walk's rate, so the ribbon's pacing and the
/// meteor's agree wherever they can.
#[inline]
#[must_use]
pub fn arc_gain(cells: f32) -> f32 {
    if !cells.is_finite() || cells <= 0.0 {
        return 1.0;
    }
    (ARC_FRAME0_SWEEPS / (FLIGHT_P0 * cells * WALK_LAY_RATE)).max(1.0)
}

// ---- §6.5 the train -------------------------------------------------------

/// Stations per train layer (§6.5, §18). The stride is
/// `max(STATION_STRIDE_MIN_PX, L/STATIONS_MAX)`.
pub const STATIONS_MAX: usize = 96;

/// Minimum station stride in px (§18 wrote 6). **2 since 2026-09-08:** the
/// frame-0 span of an 8-cell hop is 20 px at `cw = 9`, and a whole sweep
/// across it needs more than three slabs to show its stops
/// (`the_train_carries_the_whole_spectrum_on_every_frame_of_the_flight`).
/// Only a flight shorter than 192 px pays for it; a long one strides `L/96`.
pub const STATION_STRIDE_MIN_PX: f32 = 2.0;

/// **HARD CAP on one meteor's `under` share** (§6.5, §18): the colour layer
/// sheds STATIONS before it exceeds this, so a two-meteor ping-pong can never
/// shed the ribbon. `2 × 2304 + 2 × 768 + 10240 = 16384`, exactly
/// `MAX_QUADS` — the 2026-09-08 splash ([`SPLASH_QUAD_CAP`]) is paid for out
/// of the train's former 3 072, which an 80-cell train at the retina cell
/// (≈ 1 900 quads) never reached.
pub const UNDER_QUAD_CAP: usize = 2_304;

/// Cap on one landing's SPLASH (`under`, [`draw_splash`]): each half of the
/// row is rasterized head-first from the caret outward under half of this,
/// so a saturated budget sheds the splash's far ends, never its heart.
pub const SPLASH_QUAD_CAP: usize = 768;

/// Cap on one meteor's white (`out`) layer (§18).
pub const WHITE_QUAD_CAP: usize = 1_152;

/// Cap on the landing pin's quads (§18).
pub const PIN_QUAD_CAP: usize = 12;

/// Cap on the landing ring's quads (§18). §18 prices the ring at ONE QUAD PER
/// SEGMENT, which is what a segment-stepped rasterizer would emit;
/// `comet_beam` at step 1 emits one quad per device row of each slab, so the
/// ring's true `out` share is this times the stroke's row count (≤ 0.5 `ch`).
/// The cap is not enforced by truncation, deliberately: truncating a ring
/// leaves a notch, which is exactly the artefact
/// `the_ring_has_no_notch_on_the_steep_quadrants` forbids. The sanctioned way
/// to reach the §18 figure is the analytic annulus the spec names as the
/// preferred later refactor (a `RainHalo` ring mode with CPU/GPU parity) —
/// which is also where §6.5 layer 10's "de-cover at the four axis switches"
/// lives: `comet_beam` de-covers same-axis monotone joins and keeps the corner
/// overlap at an axis switch, and that overlap is the primitive's to remove,
/// not a caller's. **KNOWN OPEN**: until `aterm-render` grows either, a ring
/// double-adds a few pixels at each of its four diagonals.
pub const RING_QUAD_CAP: usize = 72;

/// Width falloff length as a share of `L`: `w(s) = w·(0.30 + 0.70·exp(−s/(0.5·L)))`
/// (§6.5 layer 1).
pub const WIDTH_FALLOFF_SHARE: f32 = 0.5;

/// The width's floor share at the tail (§6.5 layer 1 wrote 0.30; 0.22 since
/// 2026-09-08 — "wider at the head, feathered at the tail").
pub const WIDTH_FLOOR_SHARE: f32 = 0.22;

/// The width's exponentially-decaying share (§6.5 layer 1); floor + span = 1.
pub const WIDTH_SPAN_SHARE: f32 = 0.78;

/// The transverse profile's CORE SHARE (§6.5 layer 1): the plateau is half the
/// reach, which is also `aterm_render::RIBBON_CORE_SHARE`'s own ceiling, so the
/// rasterizer's clamp is a no-op and the emitted profile is exactly the one
/// asked for.
pub const TRAIN_CORE_SHARE: f32 = 0.5;

/// Colour-layer coverage falloff length as a share of `L`:
/// `cov(s) = head_cov·exp(−s/(0.55·L))` (§6.5 layer 1 wrote 0.35). At
/// `0.61·L` this is `e⁻¹` of its value at the shoulder — the ratio
/// `the_train_is_brightest_just_behind_the_head` measures. **0.55 since
/// 2026-09-08**: at 0.35 the launch cell of a graded flight sat at 53 and
/// the tail read as a fade, not a rainbow; at 0.55 the whole length is
/// saturated (the launch cell of an 80-cell flight at 19, still four
/// stops brighter than the chroma cull) and the head is still the
/// brightest station.
pub const COLOUR_FALLOFF_SHARE: f32 = 0.55;

/// White-layer coverage falloff length as a share of `L` (§6.5 layer 2) — much
/// shorter, so the white dies fast behind the shoulder. That is the ion
/// emission. In flight this is the length; after the arrival edge the length
/// itself closes into the landing on the pin's clock ([`WHITE_CLOSE_MS`]),
/// which is what makes the train's white → spectrum turn (§6.8) readable on
/// glass.
pub const WHITE_FALLOFF_SHARE: f32 = 0.12;

/// The white layer's width as a share of `w(s)` (§6.5 layer 2), once it is
/// past the shoulder.
pub const WHITE_WIDTH_SHARE: f32 = 0.45;

/// How many e-folds of [`WHITE_FALLOFF_SHARE`] the white polyline is built
/// over before it stops spending stations. `e⁻⁵ = 0.0067`, i.e. under one
/// level of a 118-cov head: everything past this point would truncate to zero
/// anyway, and stations that emit nothing are the "no dead work" rule of §18.
pub const WHITE_TAIL_EFOLDS: f32 = 5.0;

/// The shoulder's length as a share of `L` (§6.5 layer 3) — the ionisation
/// peak, brightest just BEHIND the head. Its true length is
/// `max(0.06·L, |Δ_frame| + 2 px)`, clamped to
/// `[SHOULDER_MIN_CELLS, SHOULDER_MAX_CELLS]`: spanning at least the
/// per-frame travel is what keeps the head spatially continuous on a 60 Hz
/// panel (T7).
///
/// **Which term the cell clamp binds** (§6.5 vs §6.6, reconciled here): the
/// clamp is applied to the FRAME-TRAVEL term, not to the `0.06·L` term. §6.6's
/// acceptance picture puts an 80-column flight's shoulder at 43 px — 4.8 cells
/// at `cw = 9` — which a 4-cell ceiling over the whole `max` would clip. The
/// reading that satisfies both sections is the one that matches what each term
/// is FOR: `0.06·L` is the shoulder's design length, and `|Δ_frame| + 2 px` is
/// a continuity FLOOR whose job is to survive a slow panel; it is the floor
/// that must not be allowed to run away.
pub const SHOULDER_SHARE: f32 = 0.06;

/// The shoulder's width as a share of `w` (§6.5 layer 3).
pub const SHOULDER_WIDTH_SHARE: f32 = 0.55;

/// Shoulder length floor, in cells (§6.5 layer 3).
pub const SHOULDER_MIN_CELLS: f32 = 1.0;

/// Shoulder length ceiling, in cells (§6.5 layer 3).
pub const SHOULDER_MAX_CELLS: f32 = 4.0;

/// The SLOWEST effect frame the lane produces, in ms — a 60 Hz panel at
/// `EFFECT_PRESENT_PANEL_PERIODS = 2` (T7, §18). The shoulder's continuity
/// floor is the head's travel over THIS long, so the head is spatially
/// continuous at every panel rate without the emit path keeping a
/// previous-frame position (which would make a frame a function of its
/// predecessor rather than of `now`).
pub const SHOULDER_FRAME_MS: f32 = 33.4;

/// The corona's reach as a multiple of the nucleus radius (§6.5 layer 6's
/// coma radius, kept as the corona's outer edge).
pub const COMA_R_SCALE: f32 = 2.4;

/// Each petal's coverage as a share of `head_cov` (§6.5 layer 6's coma share,
/// now spent seven times over — the petals overlap pairwise, so no pixel of
/// the corona asks more than two shares, under the nucleus's own request).
pub const COMA_COV_SHARE: f32 = 0.34;

/// **THE CORONA IS A RAINBOW** (owner, 2026-09-08: "I want the meteor to have
/// rainbow!"). §6.5 layer 6's coma was `spectrum(t_m(0))` lerped half to
/// white — one pale blob. It is now SEVEN petals, one per named stop
/// (`spectrum_stop(0..7)`, C1: point marks snap), orbiting the white nucleus
/// at [`COMA_PETAL_ORBIT`] of the corona's reach with [`COMA_PETAL_R`] of it
/// as their own radius, turning once per [`COMA_SPIN_MS`]. The nucleus stays
/// `#FFFFFF` — that is what makes the colours read as fire (§3.2).
pub const COMA_PETALS: usize = 7;

/// A petal's radius as a share of the corona's reach.
pub const COMA_PETAL_R: f32 = 0.62;

/// A petal's orbit (centre distance from the nucleus) as a share of the
/// corona's reach. `0.72 − 0.62 = 0.10` of the reach short of the outer
/// edge, and `0.72 + 0.62 > 1`: the petals overlap the nucleus's edge, so the
/// corona reads as one glow with a white heart and a coloured rim.
pub const COMA_PETAL_ORBIT: f32 = 0.72;

/// One turn of the corona, ms — static under reduced motion (§6.11).
pub const COMA_SPIN_MS: f32 = 240.0;

/// Nucleus core radius floor in px (§6.5 layer 7).
pub const NUCLEUS_MIN_PX: f32 = 2.5;

/// Nucleus core radius as a share of `w` (§6.5 layer 7).
pub const NUCLEUS_W_SHARE: f32 = 0.22;

/// Nucleus stretch gain: `stretch = 1 + 1.2·v/v_max`, ×2.2 at launch → ×1.0 at
/// rest (§6.5 layer 7). Axis-aligned ellipse; on a diagonal, stretch the
/// DOMINANT axis only — `RainHalo` has no rotation.
pub const NUCLEUS_STRETCH_GAIN: f32 = 1.2;

/// How far ahead of the head the nucleus centre sits, in px (§6.5 layer 7).
pub const NUCLEUS_LEAD_PX: f32 = 2.0;

// ---- §6.2 / §6.5 the arrival edge and its marks ---------------------------

/// Terminal-flash duration in ms — nucleus radius ×(1.0 → 1.6) on ease-out-back
/// (§6.5 layer 8). **AREA, not coverage**: the cap is a cap.
pub const FLASH_MS: f32 = 40.0;

/// Terminal-flash peak radius multiplier (§6.5 layer 8) — the value the ease
/// is aimed at, and the one it passes through on the way up.
pub const FLASH_PEAK: f32 = 1.6;

/// How far past [`FLASH_PEAK`] the BACK ease carries the radius before it
/// relaxes (§6.5 layer 8: "overshoot 1.7 at 40 ms").
pub const FLASH_BACK: f32 = 0.1;

/// Terminal-flash overshoot at [`FLASH_MS`] (§6.5 layer 8) — where the
/// ease-out actually stands when the 40 ms are spent: the target plus the
/// back. Spelled as the sum so the published ×1.6 is the number the curve is
/// built from, not a constant nothing reads.
pub const FLASH_OVERSHOOT: f32 = FLASH_PEAK + FLASH_BACK;

/// Milliseconds by which the flash has relaxed back to ×1.0 (§6.5 layer 8).
pub const FLASH_RELAX_MS: f32 = 120.0;

/// Pin life in ms — `u = t/150` (§6.5 layer 9, §6.2). The pin CLOSES INTO THE
/// CARET: the last pixel on glass is its nucleus, which *is* the caret.
pub const PIN_MS: f32 = 150.0;

/// Pin arm half-length at `u = 0`, in `ch`:
/// `A(u) = 0.6 ch·(1 − smoothstep(0.15, 1, u))^1.2` (§6.5 layer 9).
pub const PIN_ARM_CH: f32 = 0.6;

/// The `u` at which the pin's arm law starts to close (§6.5 layer 9's
/// `smoothstep(0.15, 1, u)`): the arms hold full length for the first 15 % of
/// the pin's life, so the mark is a PIN on the frame it is born and not a
/// smear already in retreat.
pub const PIN_ARM_HOLD_U: f32 = 0.15;

/// The exponent of the pin's arm law (§6.5 layer 9) — `^1.2`, so the arms whip
/// in slightly faster than the smoothstep alone.
pub const PIN_ARM_EXP: f32 = 1.2;

/// The pin's waist, in px — **exactly 1** (§6.5 layer 9). Drawn as explicit
/// `push_fx_rect` arms plus a 2×2 nucleus, NOT via `push_twinkle_star`, whose
/// `star_waist_px` would give a 4-px waist at this arm.
pub const PIN_WAIST_PX: i32 = 1;

/// The pin's nucleus, in px — 2×2, and it *is* the caret (§6.5 layer 9).
pub const PIN_NUCLEUS_PX: i32 = 2;

/// The `u` up to which the pin holds α = 1 before spending it (§6.5 layer 9).
pub const PIN_ALPHA_HOLD_U: f32 = 0.6;

// ---- the arrival flash (2026-09-08, second round) -------------------------

/// **THE WHITE-HOT FLASH** — how long the arrival's white burst lives, ms,
/// from the pin's edge. The caret cell and its two neighbours
/// ([`FLASH_CELLS`]) go white on the arrival frame and the white LEAVES over
/// this span (`1 − smoothstep`), while the colour layer ARRIVES over the same
/// span — white → spectrum, one way, never back ([`draw_flash`]). ATTACK:
/// inside the pin's own 150 ms, so the cadence law is unchanged.
pub const FLASH_BURST_MS: f32 = 80.0;

/// When the flash's COLOUR layer is spent, ms from the pin. It peaks where
/// the white is gone ([`FLASH_BURST_MS`]) and spends on `spend` over the
/// same span again — dying into the shockwave, which is still at its hold
/// ([`RING_COV_HOLD_U`]) when the last of it goes.
pub const FLASH_COLOUR_MS: f32 = 2.0 * FLASH_BURST_MS;

/// The flash's coverage over a cell the sky's probe proved BLANK: legibility
/// costs nothing there, so the burst asks nearly the whole channel. Not 255:
/// the caret cell beside it is `#FFFFFF` on the flare frame with the pin's
/// nucleus on top, and the caret stays the brightest pixel on the glass by a
/// margin the eye can see (31 levels). Over a glyph cell, or a cell the
/// probe has not seen, the burst asks the transient cap
/// ([`super::timing::TRANSIENT_STAR_COV_CEIL`]) — the ceiling every mark
/// that crosses text is held to (L3).
pub const FLASH_FULL_COV: f32 = 224.0;

/// How many cells EITHER SIDE of the caret the flash covers — one: the
/// caret cell and its two neighbours, three cells wide.
pub const FLASH_CELLS: i32 = 1;

/// The reduced-motion flash's white share of its cell's coverage: the
/// static form is a white heart over the colour, half and half, held for the
/// landing's life and taken off on the theme's one linear fade (§6.11).
pub const FLASH_STATIC_WHITE_SHARE: f32 = 0.5;

/// Ring life in ms — `u = t/480` (§6.5 layer 10 wrote 180). **THE SHOCKWAVE
/// (owner, 2026-09-08: "a bigger more special rainbow impact").** At 180 ms
/// and 1.3 `ch` the ring was "a small pop"; it now expands to [`RING_R_CH`]
/// over 480 ms — most of the way out inside 110 ms on its quartic, then
/// hovering and spending — and is off glass at `u ≈ 0.93` (446 ms), inside
/// [`super::timing::FLIGHT_OFF_GLASS_MS`].
pub const RING_MS: f32 = 480.0;

/// Ring segments (§6.5 layer 10 wrote 48; 72 since 2026-09-08 — a 3 `ch`
/// ring at the retina cell is 84 px across, and 48 chords showed as facets).
/// The de-cover at the four axis switches is what
/// `the_ring_has_no_notch_on_the_steep_quadrants` measures.
pub const RING_SEGMENTS: usize = 72;

// ---- the landing's GRADE (2026-09-08, "scales more with the distance") ----
//
// Owner: *"a bigger impact splash that scales more with the distance
// traveled."* ONE magnitude, [`impact`] (`timing.rs`: `clamp((cells/8)^0.7,
// 1, 3.5)`), is read by every distance-graded landing law here — the ring's
// radius and life, the fan's count and reach — so they cannot come apart.
// The 8-cell floor (impact 1.0) is BYTE-IDENTICAL to the second-round look
// above (the flash, the 3 `ch` shockwave over 480 ms, the doubled fan);
// distance buys on top of it, monotonically, and saturates at the 3.5 cap
// (≈ 48 cells, a full line).

/// Extra ring life per unit of impact above the floor, in ms. `480 + 48·2.5
/// = 600` at the cap — exactly the pool's horizon
/// ([`super::timing::FLIGHT_OFF_GLASS_MS`]), the ceiling [`ring_ms`] holds
/// it under: the ring is off glass at `u ≈ 0.93` (558 ms) even at the cap.
pub const RING_MS_PER_IMPACT: f32 = 48.0;

/// Ring life ceiling in ms — the pool's own horizon, so the const assert
/// "every impact mark must be off glass by FLIGHT_OFF_GLASS_MS" holds at
/// every grade, and the shockwave's hold (`0.25 · 600 = 150`) is still
/// inside the pin's attack ([`PIN_MS`]).
pub const RING_MS_MAX: f32 = FLIGHT_OFF_GLASS_MS;

/// Extra ring radius per unit of impact above the floor, in `ch`: a
/// full-line landing rings at `3.0 + 1.2·2.5 = 6.0 ch`, TWICE the floor's
/// reach — the same ×2 the classic landing ring takes at the cap
/// (`cursor_glow::classic_ring_radius_factor`).
pub const RING_R_PER_IMPACT_CH: f32 = 1.2;

/// The ring's vertical semi-axis cap, in `ch` — the floor ring's own
/// (`RING_R_CH · RING_SQUASH = 1.5 ch`, the number the const assert against
/// [`FAN_RISE_MAX_CH`] has always stated). A bigger ring grows ALONG the
/// line and flattens past this, exactly as the fan flattens under
/// [`FAN_RISE_MAX_CH`]: one law, and the ring never rises above where the
/// fan may, at any grade.
pub const RING_RISE_MAX_CH: f32 = RING_R_CH * RING_SQUASH;

/// The ring's life for a landing of `cells` — [`RING_MS`] at the floor,
/// [`RING_MS_PER_IMPACT`] more per unit of [`impact`], capped at
/// [`RING_MS_MAX`]: 480 / 510 / 536 / 580 / 600 ms at 8 / 16 / 24 / 40 /
/// ≥ 48 cells.
#[inline]
#[must_use]
pub fn ring_ms(cells: f32) -> f32 {
    (RING_MS + RING_MS_PER_IMPACT * (impact(cells) - 1.0)).min(RING_MS_MAX)
}

/// The ring's FULL radius (at `u = 1`) for a landing of impact `scale`, in
/// px — [`RING_R_CH`] at the floor, [`RING_R_PER_IMPACT_CH`] more per unit of
/// impact. One function, so the emit path and the pins share it.
#[inline]
#[must_use]
pub fn ring_full_radius(scale: f32, ch: f32) -> f32 {
    let scale = if scale.is_finite() {
        scale.clamp(1.0, super::timing::IMPACT_MAX)
    } else {
        1.0
    };
    (RING_R_CH + RING_R_PER_IMPACT_CH * (scale - 1.0)) * ch
}

const _: () = assert!(
    RING_MS + RING_MS_PER_IMPACT * (super::timing::IMPACT_MAX - 1.0) <= RING_MS_MAX + 1e-3,
    "the graded ring life reaches the horizon exactly at the cap and never past it"
);

/// The fan's star count for a big landing of `cells` (§6.5 layer 11):
/// `min(19 + 3.6·(impact − 1), 28) + party·8`, held under
/// [`super::timing::FAN_MAX_N`] — 19 at the 8-cell floor (impact 1, m15's
/// second-round count, byte-identical), 21 / 23 / 27 at 16 / 24 / 40 cells,
/// 28 from the 3.5 cap. ONE function, so the mint path and the pins share
/// the `(impact − 1)` form every other graded law uses: the first cut wrote
/// `FAN_N_BASE + FAN_N_PER_IMPACT * impact` here — the peer's un-shifted
/// expression under the re-fitted constants — which threw 23 stars at the
/// floor and saturated by impact 2.5 (≈ 30 cells); the const asserts on the
/// constants could not see it, which is why `fan_count_and_reach_are_the_
/// graded_law_at_floor_and_cap` reads the FUNCTION.
#[inline]
#[must_use]
pub fn fan_count(cells: f32, party: bool) -> usize {
    let base = (FAN_N_BASE + FAN_N_PER_IMPACT * (impact(cells) - 1.0)).min(FAN_N_CEIL);
    ((base.round() as usize) + usize::from(party) * FAN_PARTY_ADD).min(super::timing::FAN_MAX_N)
}

/// The fan's throw reach for a big landing of `cells`, in `ch` (§6.5 layer
/// 11): `(2.8 + 1.6·(impact − 1)).clamp(1.6, 6.8 + 1.2·grade)` — exactly
/// [`FAN_REACH_BASE_CH`] at the 8-cell floor (stardust's m2 clearance law is
/// derived there), 3.8 / 4.7 / 6.1 at 16 / 24 / 40 cells, 6.8 from the cap.
/// Shares its `(impact − 1)` form with [`fan_count`] and [`ring_full_radius`].
#[inline]
#[must_use]
pub fn fan_reach_ch(cells: f32, grade: f32) -> f32 {
    let ceil = FAN_REACH_MAX_CH + FAN_REACH_GRADE_CH * grade;
    (FAN_REACH_BASE_CH + FAN_REACH_PER_IMPACT_CH * (impact(cells) - 1.0))
        .clamp(FAN_REACH_MIN_CH, ceil)
}

/// Ring radius at `u = 1`, in `ch`: `r(u) = 3.0 ch·(1 − (1 − u)^4)` (§6.5
/// layer 10 wrote 1.3). **2.3× the reach, 2026-09-08** — a shockwave that
/// expands PAST the fan and the splash, hollow, its circumference carrying
/// the spectrum ([`RING_SWEEPS`]).
/// `the_landing_is_a_rainbow_shockwave_twice_the_old_reach` pins it at no
/// less than twice 1.3.
pub const RING_R_CH: f32 = 3.0;

/// The reach the 2026-09-05 ring had, in `ch` — kept only as the number the
/// shockwave law is measured against ("2–3× the current ring's reach").
pub const RING_R_CH_2026_09_05: f32 = 1.3;

/// How many times the spectrum is walked around the shockwave — TWO, through
/// `tri`, so the walk runs red → violet → red and the ring has no seam
/// (ROYGBIV is not cyclic: one walk would butt violet against red). A
/// linear mark samples `spectrum` (C1), so the ring's colour is continuous
/// around it, not seven snapped arcs.
pub const RING_SWEEPS: f32 = 2.0;

/// How far the shockwave's spectrum turns around the ring over its life, in
/// turns — the "dynamic" of the owner's brief: the colours rotate as the
/// ring expands. Static under reduced motion (no ring at all, §6.11).
pub const RING_SPIN_TURNS: f32 = 0.5;

/// The exponent of the ring's radius law (§6.5 layer 10) — `(1 − (1 − u)^4)`,
/// so the ring is already most of the way out on the frame it is born (the
/// arrival edge owes the eye an EVENT, not a slow bloom).
pub const RING_R_EXP: i32 = 4;

/// Ring squash (§6.5 layer 10 wrote 0.62, the fan's). **0.5 since
/// 2026-09-08**: at 3 `ch` the fan's aspect would put the ring's crown
/// `1.86 ch` above the landing row — past the fan's own rise cap. A flatter
/// ellipse is a shockwave seen on a plane: 3 `ch` along the line, 1.5 `ch`
/// up, still inside where the fan may rise ([`FAN_RISE_MAX_CH`], asserted
/// below). The fan keeps its own `stardust::FAN_SQUASH`; the two are no longer one
/// ellipse — the ring is now the outermost mark of the landing, and the
/// fan and the splash sit inside it.
pub const RING_SQUASH: f32 = 0.5;

// The ring's vertical semi-axis at full radius is `3.0 · 0.5 = 1.5 ch`
// (§6.5 layer 10: `r(u) = R·(1 − (1 − u)^4)`, so `r_y ≤ 1.5 ch` for every
// `u` by construction): under the fan's own rise cap ([`FAN_RISE_MAX_CH`]) —
// the ring never rises above where the fan may.
const _: () = assert!(
    RING_R_CH * RING_SQUASH < FAN_RISE_MAX_CH,
    "the ring's vertical semi-axis must stay inside the fan's rise"
);

/// Ring stroke thickness as a share of its own radius (§6.5 layer 10 wrote
/// 0.10; 0.16 in the 2026-09-08 bold round — 13 px at the retina cell,
/// which at the owner's cell read as "a thin rainbow ring around the caret";
/// **0.40 in the second round, two and a half times that**, 34 px at the
/// retina cell under a `1.2 ch` ceiling: a BAND of spectrum, not a line).
/// The reach ([`RING_R_CH`]) is the same three cell heights; the stroke is
/// what grew. `the_shockwave_stroke_is_two_and_a_half_times_the_bold_round_s`
/// measures it on glass.
pub const RING_THICK_SHARE: f32 = 0.40;

/// The bold round's stroke share (2026-09-08, first round) — kept only as
/// the number the second round's law is measured against.
pub const RING_THICK_SHARE_BOLD_ROUND: f32 = 0.16;

/// Ring stroke thickness floor, px (§6.5 layer 10 wrote 1.5).
///
/// **2.0, not 1.5 — measured on glass (offline judge, 2026-09-05, defect 3).**
/// `comet_beam` paints a slab as a flat interior between two anti-aliased
/// EDGE pixels, and a 1.5 px stroke has an interior row only when its centre
/// happens to straddle a pixel boundary — half the ring's columns carried two
/// edge pixels at ≈ 0.75 of the request and nothing at the request itself,
/// which is why the ring composited at ≤ 29/255 chroma on `B/frame_0064-0071`
/// while asking 35-47. At 2.0 px every column of the stroke owns one pixel at
/// the full request; the ceiling (`0.5 ch`) and the share are untouched.
pub const RING_THICK_MIN_PX: f32 = 2.0;

/// Ring stroke thickness ceiling, in `ch` (§6.5 layer 10 wrote 0.5; 1.2
/// since the second round, so the `0.40·r` share binds at every cell size
/// — `0.40 · 3.0 ch = 1.2 ch` exactly at full reach).
pub const RING_THICK_MAX_CH: f32 = RING_THICK_SHARE * RING_R_CH;

/// The stroke thickness from which the shockwave is rasterized at a 2 px
/// major-axis step instead of 1 (2026-09-08). A 3 `ch` ring at the retina
/// cell is ≈ 600 px of major axis, three quads a pixel at step 1 — the one
/// term that moved the ping-pong's p50 from 21 to 44 µs — and on a stroke
/// this thick the 2 px stair on the diagonals is under its own edge
/// anti-aliasing. A thin ring (small cell) keeps step 1.
pub const RING_STEP2_THICK_PX: f32 = 6.0;

/// The stroke thickness from which the shockwave is rasterized at a 3 px
/// major-axis step (second round): the stroke is up to 34 px at the retina
/// cell, its edge anti-aliasing swallows a 3 px stair, and the extra step
/// is what keeps the ping-pong's p50 under its budget with a stroke two and
/// a half times thicker (each step is one interior slab per row it crosses
/// plus two edge pixels, so a thick stroke costs more per step, not fewer).
pub const RING_STEP3_THICK_PX: f32 = 18.0;

/// The stroke thickness from which the shockwave is rasterized at a 4 px
/// major-axis step (second round, measured): at the bench cell (`ch` 40) the
/// stroke is 48 px, and a 4 px stair inside a 48 px band is under its own
/// edge anti-aliasing. The ring is 3.3 µs at step 3 and 2.5 at step 4 on
/// the release probe; the ping-pong row's budget is what asked.
pub const RING_STEP4_THICK_PX: f32 = 30.0;

/// Ring coverage as a share of [`TRANSIENT_STAR_COV_CEIL`], HELD for
/// [`RING_COV_HOLD_U`] and then spent on `spend`:
/// `0.85·118` for `u ≤ 0.22`, then `0.85·118·(1 − u')²` with
/// `u' = (u − 0.22)/0.78` (§6.5 layer 10 wrote `0.40·118·(1 − u)²`).
///
/// **0.85 and a hold, not 0.40 spent from the edge — measured on glass
/// (offline judge, 2026-09-05, defect 3).** The share used to be spent on
/// `(1 − u)²` from the arrival edge, but the ring has NO RADIUS on that edge
/// (`r(0) = 0`): on the first frame it was a ring at all (`u ≈ 0.14`,
/// `r ≈ 0.6 ch`) the 0.40 share had already fallen to 35, and by `u = 0.33`
/// (`r ≈ 0.8 ch`, where the eye finds it) to 21 — a coloured stroke at
/// ≤ 29/255 over a dark ground, "a whisper" beside a pin at 118. Raising the
/// share alone to 0.85 bought 74 and 45 on those frames, still a decay
/// spent while the ring was too small to read. The hold spends the
/// coverage where the ring IS: full (100) while it is small — the first 40
/// ms, out to `r ≈ 0.82 ch`, the frames the pin and the fan's hold share
/// with it — and then `(1 − u')²` over the remaining 140 ms as it expands
/// past the fan, spent by `u = 1` as before (§6.2: "ring expands and
/// spends"; off glass at `u ≈ 0.93`). The landing is the
/// payoff the owner asked for ("bring back the stars, more variance and
/// fun"), and the ring is the one landing mark this file draws that is not
/// a point. Its 100 sits under every white in §3.2 (pin nucleus 118,
/// transient m1 core 118) — an event, not a new peak.
///
/// **1.0 since 2026-09-08** — the shockwave asks the whole transient cap
/// (118): it crosses text, and 118 is the cap the pin, the sparks and every
/// transient star are held to over a glyph cell (L3), so the ring may be as
/// loud as the law allows and no louder.
pub const RING_COV_SHARE: f32 = 1.0;

/// The `u` up to which the ring holds its full coverage before spending it
/// — `0.25 · 480 ms = 120 ms` (the bold round held to 0.35, 168 ms): the
/// stroke is held near the cap for the first 120 ms, the flash's whole life
/// and forty milliseconds past it, then spent over the remaining 360 ms as it
/// expands to its reach — see [`RING_COV_SHARE`]. The pin's own alpha hold
/// is [`PIN_ALPHA_HOLD_U`].
pub const RING_COV_HOLD_U: f32 = 0.25;

/// The stroke's hold in ms at the FLOOR, [`RING_COV_HOLD_U`] of [`RING_MS`]
/// — the shockwave's ATTACK (inside the pin's 150 ms, so the cadence law's
/// attack window is the pin's as before). A graded ring holds the same
/// SHARE of its longer life: [`RING_HOLD_MAX_MS`] is the cap's.
pub const RING_HOLD_MS: f32 = RING_COV_HOLD_U * RING_MS;

/// The stroke's hold at the cap, ms — `0.25 · 600 = 150`, the pin's own
/// window exactly; the const assert below holds it there.
pub const RING_HOLD_MAX_MS: f32 = RING_COV_HOLD_U * RING_MS_MAX;

/// Light-theme ring dots (§6.10, §18's halo budget; 24 → 36 with the 2.3×
/// reach, so the dots still read as a ring and not a necklace). NOT graded:
/// at the cap the ring is `2π·√((36 + 2.25)/2) ≈ 27.5 ch` around, `0.76 ch`
/// per dot against a `1.2 ch` dot — still overlapping, still a ring — and
/// `stardust::STARDUST_LIGHT_HALO_BUDGET` is priced on this number.
pub const RING_LIGHT_DOTS: usize = 36;

/// Fan reach at the FLOOR landing, in `ch` — `1.6 + 0.15·8 = 2.8`, the reach
/// the second round's 8-cell landing has always thrown (§6.5 layer 11 wrote
/// `1.35 + 0.15·cells`, 2.55 at the floor; 1.6 since 2026-09-08). Since the
/// grade: `reach = 2.8 + 1.6·(impact − 1)`, clamped.
pub const FAN_REACH_BASE_CH: f32 = 2.8;

/// Fan reach per unit of [`impact`] above the floor, in `ch` (§6.5 layer 11,
/// distance-graded 2026-09-08). The floor landing throws EXACTLY
/// [`FAN_REACH_BASE_CH`] — the const assert below holds it there, because
/// stardust's m2 clearance law (`a_fan_star_is_born_at_full_and_clears_its_
/// cell_inside_its_hold`) was derived at the floor's reach (2.55 then, 2.8
/// now — a larger floor only clears farther); distance then buys 3.8 / 4.7 /
/// 6.1 / 6.8 ch at 16 / 24 / 40 / ≥ 48 cells where the un-graded law bought
/// 4.0 / 5.0 / 5.0 / 5.0 — saturating at 23 cells, which is what "scales
/// more with the distance" asked to change.
pub const FAN_REACH_PER_IMPACT_CH: f32 = 1.6;

const _: () = assert!(
    FAN_REACH_BASE_CH >= 2.8 - 1e-6 && FAN_REACH_BASE_CH <= 2.8 + 1e-6,
    "the floor landing's reach is 2.8 ch — stardust's m2 clearance law is derived at the floor"
);
const _: () = assert!(
    FAN_REACH_BASE_CH + FAN_REACH_PER_IMPACT_CH * (super::timing::IMPACT_MAX - 1.0)
        <= FAN_REACH_MAX_CH + 1e-6,
    "the capped landing's reach lands exactly on the un-graded ceiling"
);

/// Fan reach floor, in `ch` (§6.5 layer 11) — and the Enter landing's fixed
/// reach (D8).
pub const FAN_REACH_MIN_CH: f32 = 1.6;

/// Fan reach ceiling before the grade term, in `ch` (§6.5 layer 11 wrote
/// 4.0; 5.0 in the second round; **6.8 since the distance grade** — the
/// capped landing's own `2.8 + 1.6·2.5`, so the ceiling binds only through
/// `grade`; along the line only, the rise is the sky's [`FAN_RISE_MAX_CH`]).
pub const FAN_REACH_MAX_CH: f32 = 6.8;

/// How much `grade` lifts the fan's reach ceiling (§6.5 layer 11).
pub const FAN_REACH_GRADE_CH: f32 = 1.2;

/// Fan count at the FLOOR landing: `n = min(19 + 3.6·(impact − 1), 28) +
/// party·8` (§6.5 layer 11 wrote `min(5 + cells/1.8, 14) + party·4`;
/// **doubled 2026-09-08** — "a fan of coloured stars twice today's count" —
/// to `min(10 + cells/0.9, 28)`, which is 19 at the 8-cell floor and
/// saturated by 16 cells; **distance-graded the same day** so the count
/// keeps climbing to the cap: 19 / 21 / 23 / 27 / 28 at 8 / 16 / 24 / 40 /
/// ≥ 48 cells). The census is unchanged: 1 gold m1 + 4 m2 (D6, the rain's
/// five) and the rest grains; the hold is the sky's
/// (`stardust::HOLD_*_TRANSIENT_MS`) and is not this file's to lengthen.
pub const FAN_N_BASE: f32 = 19.0;

/// Extra fan stars per unit of [`impact`] above the floor (§6.5 layer 11):
/// `19 + 3.6·2.5 = 28`, the ceiling, exactly at the cap.
pub const FAN_N_PER_IMPACT: f32 = 3.6;

/// Fan count ceiling before the party bonus (§6.5 layer 11, doubled
/// 2026-09-08). With the shed's six and the sky's typical twenty this sits
/// under `stardust::STAR_CAP`; a party lands on the eviction finish, which
/// is a 40 ms fade and never a pop.
pub const FAN_N_CEIL: f32 = 28.0;

const _: () = assert!(
    FAN_N_BASE + FAN_N_PER_IMPACT * (super::timing::IMPACT_MAX - 1.0) <= FAN_N_CEIL + 1e-3,
    "the capped landing's fan count lands exactly on the ceiling"
);

/// What a party adds to the fan's count (§6.5 layer 11, doubled 2026-09-08).
pub const FAN_PARTY_ADD: usize = 8;

/// The eased spine at or above which a landing is a PARTY (§6.5 layer 11,
/// §7.2). This is the celebration arm's own signature: `Engine::celebrate`
/// pins the spine to a floor while a sing-along is armed, and that pinned
/// spine is the one thing the glow can see that says "this is a celebration"
/// without reaching across the seam into the synth.
pub const FAN_PARTY_DISP: f32 = 0.95;

/// Cells of landing size per row of a vertical / Enter flight (D8):
/// `cells_landing = 4·|dr|` → 5-7 m3 + 1 m2, no ring, no hero, no rain.
pub const ENTER_LANDING_CELLS_PER_ROW: f32 = 4.0;

/// Floor on an Enter / vertical landing's grain count (D8, §6.11).
pub const ENTER_LANDING_M3_MIN: usize = 5;

/// Ceiling on an Enter / vertical landing's grain count (D8, §6.11).
pub const ENTER_LANDING_M3_MAX: usize = 7;

/// Nearest station of a shed fragment, as a share of `L` (§6.5 layer 5).
pub const SHED_S_MIN: f32 = 0.10;

/// Farthest station of a shed fragment, as a share of `L` (§6.5 layer 5).
pub const SHED_S_MAX: f32 = 0.70;

// ---- §6.8 the fade --------------------------------------------------------

/// White layer's post-arrival τ, ms: `α_w = exp(−(t−T)/90)`, culled at
/// α < 0.12 → `T + 191` (§6.8).
pub const TRAIN_WHITE_TAU_MS: f32 = 90.0;

/// Colour layer's post-arrival τ, ms: `α_c = exp(−(t−T)/220)`, culled at
/// α < 0.12 → `T + 466` (§6.8 wrote 150 and `T + 318`; 220 since
/// 2026-09-08 so the rainbow stays on the row while the shockwave expands
/// past it, and leaves inside [`super::timing::FLIGHT_OFF_GLASS_MS`]).
pub const TRAIN_COLOUR_TAU_MS: f32 = 220.0;

/// The colour root's retract span, ms: `root(t) = x₀ + (x₁−x₀)·((t−T)/460)^1.8`
/// on `suck-in` (§6.8 wrote 300; 460 since 2026-09-08 — the root reaches the
/// landing on the frame the colour reaches its chroma cull, so the last
/// train pixel and the corona leave together). **No wind bend.**
pub const ROOT_SUCK_MS: f32 = 460.0;

/// The thickness the train thins to over the fade, as a share of `w` (§6.8).
pub const THICKNESS_END_SHARE: f32 = 0.4;

/// **THE WHITE LAYER CLOSES INTO THE CARET** — the span, ms, over which the
/// white layer's EXTENT (its shoulder and its falloff length) retracts to the
/// landing on `suck-in` after the arrival edge. It is the pin's own clock
/// ([`PIN_MS`]): the pin's arms whip inward over these 150 ms and the white
/// heat behind them closes with them, so everything white at the landing
/// leaves on ONE edge and the last mark on glass is the nucleus, which is the
/// caret (§6.5 layer 9, T5). The two taus of §6.8 are untouched — the white
/// still spends its alpha on τ 90 and the colour on τ 150 — this moves WHERE
/// the white is, not how bright.
///
/// **Why it exists — measured on glass (offline judge, 2026-09-05, defect
/// 4).** §6.8 promises that "the train appears to turn white → spectrum
/// because the white layer dies first — real physics, no chroma change". On
/// glass the turn did not happen: the lit train's chromaticity was FLAT,
/// 0.86 → 0.86 over `T + 0 → T + 183` (`B/frame_0061-0083`), and the pixel
/// arithmetic says why. The white layer is a short spatial stub
/// (`exp(−s/(0.12·L))` past the shoulder), so beyond ≈ 0.4·L the train was
/// pure colour from the first frame and had nothing to turn FROM; and where
/// the two layers do overlap, an alpha-only fade leaves white at 0.19 over a
/// colour layer at 0.37 of an already dimmer base (118 vs 160) — additive
/// over the ground, that composite never reads as spectrum before the colour
/// is gone too. What the eye needs is the white LEAVING the train while the
/// colour is still bright: leaving toward the caret (T5: "leaving marks move
/// toward the caret; they do not fade in place"), which is what the pin's
/// arms and the colour root already do. With the extent closing on this span
/// the mean saturation of the train beyond the landing rises 0.786 → 0.823 →
/// 0.911 → 1.000 over `T`, `T + 50`, `T + 100`, `T + 150`
/// (`the_train_turns_white_then_spectrum_as_it_dies`, measured); alpha alone
/// left it 0.786 → 0.781.
pub const WHITE_CLOSE_MS: f32 = PIN_MS;

/// **THE RETIRE DECAY** (§6.9): `ln(1/CHROMA_CULL_ALPHA) = ln(1/0.12)`.
///
/// A retired train's colour layer runs `exp(−u·RETIRE_DECAY)` over
/// `u = (t − retired_at)/FLIGHT_RETIRE_MS`, so its alpha reaches the chroma
/// cull EXACTLY at `R = 60 ms` and not a frame later. Spelled as a number
/// because `f32::ln` is not `const`; `the_retire_decay_lands_on_the_chroma_cull`
/// pins it against [`CHROMA_CULL_ALPHA`] so the two can never drift apart.
pub const RETIRE_DECAY: f32 = 2.1203;

/// Reduced motion's static train alpha (§6.11): head at the landing, train at
/// the exponential profile at this α, no flight, no flash, no pin, no ring, no
/// twinkle, static fan — then the theme's one linear fade.
pub const REDUCED_MOTION_ALPHA: f32 = 0.85;

// ---- §6.9 retire ----------------------------------------------------------

/// How close two corridors must come for the newer to retire the older, in
/// `ch` (§6.9).
pub const CORRIDOR_CH: f32 = 1.0;

/// How close a new launch must be to a live head, in CELLS, for the flight to
/// CHAIN rather than merely cross (§6.9).
pub const CHAIN_CELLS: f32 = 1.0;

/// **THE METEOR POOL** (§18: "fixed pools — 2 meteors"): [`FLIGHT_MAX_LIVE`]
/// heads in the air plus as many trains FINISHING on §6.9's 60 ms retire.
/// `Meteors::live` is reserved to this depth at construction and never grows
/// past it: when a spawn finds the pool full, the finishing train nearest its
/// own end is dropped to make the slot. That cut is invisible at any human
/// cadence — key repeat is ≥ 30 ms per jump, so the train evicted is one that
/// has been retiring for ≥ 60 ms and is already at the chroma cull — and it
/// is what makes the spawn path allocation-free under a mash (§18).
pub const METEOR_POOL: usize = 2 * FLIGHT_MAX_LIVE;

/// **THE LANDING POOL** (§18 wrote "2 landings"; 3 since 2026-09-08). A
/// landing now lives [`Landing::end`]'s 560 ms, so an 80-cell ping-pong at a
/// 200 ms cadence has three impacts finishing at once; a FOURTH drops the
/// oldest, whose sparks are at the chroma cull and whose ring is dark.
pub const LANDING_POOL: usize = FLIGHT_MAX_LIVE + 1;

/// Depth of the per-tick hand-off scratch ([`Meteors::sow_into`]): every live
/// meteor's whole shed plus a fan each, plus one mini-fan. Reserved once so
/// staging never allocates (§18).
pub const SOW_SCRATCH: usize = METEOR_POOL * (SHED_MAX as usize + 1) + 1;

// ---- the splash (2026-09-08; out from under the text in the second round)

/// Splash life, ms — "a brief rainbow splash". Held for [`SPLASH_HOLD_U`] of
/// it, then spent; its reach bursts outward on the ring's own quartic
/// ([`RING_R_EXP`]).
pub const SPLASH_MS: f32 = 260.0;

/// How far the splash reaches along the row, in cells, EACH side of the
/// caret ("six cells either side").
pub const SPLASH_CELLS: f32 = 6.0;

/// **THE SPLASH IS IN THE SKY** (second round). The bold round's splash was
/// the bed's ink under the text; the owner asked for the impact out from
/// under the letters, so the splash is now two RAINBOW BANDS in `out`: one
/// in the sky band above the row and its mirror below the row — never a
/// pixel of the landing row's own cell rows, never a cell of the neighbour
/// row the probe has not proved blank ([`SkyMask`]). This is the band's
/// height in `ch` — the aurora's own ([`AURORA_TALL_CH`]), because the
/// splash lies in the aurora's band and the two must agree on what the sky
/// band IS.
pub const SPLASH_BAND_CH: f32 = AURORA_TALL_CH;

/// Stations per cell of splash — a `spectrum` sample every half cell (C1: a
/// linear mark samples `spectrum`); the beam lerps colour slab by slab
/// between stations, so the sweep reads as a ramp at two per cell. Three per
/// cell (the first cut) tripled the segments for no visible gain: the band
/// costs per SEGMENT slab, not per station, and at a third of a cell every
/// segment was one slab — 466 quads and 5.9 µs for two bands of six cells at
/// the bench cell against ~300 and ~3 at two.
pub const SPLASH_STATIONS_PER_CELL: usize = 2;

/// The band's rasterization step along the row as a share of `cw` — half a
/// cell (7–9 px), one slab per station segment: the band is a flat ribbon
/// of constant height and the slab is what a frame pays for.
pub const SPLASH_STEP_CW_SHARE: f32 = 0.5;

/// The `u` up to which the splash holds its full coverage (the transient
/// ceiling, 118 — the level §3.2 gives every transient white, so a yellow
/// band at it is under the pin's nucleus and far under the caret) before
/// spending it on `spend`.
pub const SPLASH_HOLD_U: f32 = 0.35;

/// The share of the splash's reach over which it is at full coverage; past
/// it the band feathers to nothing at its far end on `smoothstep`, so the
/// six cells end as light leaving and not as a cut.
pub const SPLASH_FEATHER_SHARE: f32 = 0.6;

/// How far the band's spectrum walk slides outward over the splash's life,
/// in `t`-units — the "dynamic" of the brief: the colours run away from the
/// caret as the band spends. Static under reduced motion.
pub const SPLASH_DRIFT_T: f32 = 0.5;

// ---- the sparks (2026-09-08) ----------------------------------------------

/// Spark count constant term: `n = clamp(36 + cells/2, 36, 64)` — "a shower
/// of coloured sparks that fall and fade", **twice the bold round's
/// `18 + cells/4` (second round)**. Each is a small HALOED STAR of one named
/// stop (C1: point marks snap), walked ROYGBIV from the landing's stop,
/// thrown up and out and falling under [`SPARK_G_CH_PER_S2`]. Drawn by this
/// file rather than sown into the sky: a spark is ballistic, with no twinkle
/// and no class ladder, a pure function of the landing's seed and its age —
/// nothing to pool, nothing to allocate — and sixty of them would empty the
/// sky's own star pool for the fan and the shed. Its SHAPE is the family's
/// one star (`push_twinkle_star`, D2), so a spark is a star wherever it
/// appears; only its colour, size and flight are the meteor's.
pub const SPARK_N_BASE: f32 = 36.0;

/// Cells of path per extra spark (halved in the second round).
pub const SPARK_CELLS_PER: f32 = 2.0;

/// Spark count ceiling (doubled in the second round).
pub const SPARK_N_MAX: usize = 64;

/// The smallest spark's arm half-length, px — a 5 px star.
pub const SPARK_ARM_MIN_PX: i32 = 2;

/// The largest spark's arm half-length, px — a 9 px star. Hashed per spark
/// at the mint ([`mint_spark`]), so a shower is a mix of sizes.
pub const SPARK_ARM_MAX_PX: i32 = 4;

/// How far a spark's CORE is tinted from white toward its stop — the core
/// is coloured (the owner's "coloured cores"), not white-hot like the
/// nucleus and not the raw stop like the sparks of the bold round: a hot
/// centre inside a saturated halo is what makes a point read as a star.
pub const SPARK_CORE_TINT: f32 = 0.75;

/// A spark's halo radius, px, over its arm — `arm + this`: 4 px around the
/// smallest star, 6 around the largest.
pub const SPARK_HALO_R_ADD_PX: f32 = 2.0;

/// A spark's halo peak as a share of its core coverage — the halo is the
/// saturated stop itself, and it may not outshine the core (the sky's own
/// halo law).
pub const SPARK_HALO_SHARE: f32 = 0.55;

/// How many spark halos one frame may carry across every live landing —
/// the aurora's own pool depth, budgeted beside it (§18): a spark past the
/// budget keeps its star and loses its halo, never the other way round.
pub const SPARK_HALO_CAP: usize = 64;

/// Shortest spark life, ms.
pub const SPARK_LIFE_MIN_MS: f32 = 460.0;

/// Longest spark life, ms — the impact's last mark, ON the pool's horizon
/// [`super::timing::FLIGHT_OFF_GLASS_MS`] (asserted below); its alpha is
/// under the chroma cull from `u ≈ 0.86`, 516 ms.
pub const SPARK_LIFE_MAX_MS: f32 = 600.0;

/// Slowest launch speed, in `ch` per second.
pub const SPARK_V_MIN_CH_PER_S: f32 = 5.0;

/// Fastest launch speed, in `ch` per second. With [`SPARK_G_CH_PER_S2`] the
/// highest spark peaks `9²/(2·24) = 1.7 ch` above the row — the fan's own
/// rise, not the tab strip — and is back on the row by 0.75 s.
pub const SPARK_V_MAX_CH_PER_S: f32 = 9.0;

/// Gravity, in `ch` per second squared.
pub const SPARK_G_CH_PER_S2: f32 = 24.0;

/// The launch cone: angles from `π·this` to `π·(1 − this)`, measured from
/// the row toward straight up — every spark leaves upward, none along the
/// row.
pub const SPARK_CONE_SHARE: f32 = 0.08;

/// How much wider the shower is along the line than up it.
pub const SPARK_SPREAD: f32 = 1.4;

/// The `u` of a spark's life it holds full coverage for before spending it
/// — three fifths: a spark is a point of light that BURNS, then dies (on
/// `spend`, under the chroma cull from `u ≈ 0.86`).
pub const SPARK_HOLD_U: f32 = 0.6;

/// The widest a spark's CORE gets, px — the family's star core at the
/// largest arm ([`SPARK_ARM_MAX_PX`]). What a census reads as "a spark" is
/// a chromatic `out` quad no bigger than this on either axis.
pub const SPARK_PX: i32 = 3;

/// The alpha under which a spark loses its halo and a pixel of arm; under
/// half of it the star is a 3 px cross. A dying star dims halo-first.
pub const SPARK_SHRINK_ALPHA: f32 = 0.6;

// ---- §6.10 the light fork -------------------------------------------------

/// `RAINBOW_METEOR_LIGHT_SQUASH` (§6.10) — the nucleus and coma's vertical
/// squash on the light arm. L6: light buys "bigger" as AREA, never as
/// brightness, and a squashed ellipse is area spent along the path.
pub const LIGHT_SQUASH: f32 = 0.30;

/// Where the light-theme train's rail line sits below the row's centre, in
/// `ch` (§6.10). The rail is INTER-LINE LEADING: nothing it darkens is ever a
/// glyph, which is what licenses the vivid ceiling below.
pub const LIGHT_RAIL_CH: f32 = 0.62;

/// The light-theme train's squash on the rail (§6.10).
pub const LIGHT_RAIL_SQUASH: f32 = 0.34;

/// Coverage → ink-alpha gain on the light arm (§6.10's `min(head_cov·2.1,
/// 236)`): additive coverage and source-over opacity are not the same
/// quantity, and this is the one number that converts between them.
pub const LIGHT_INK_GAIN: f32 = 2.1;

/// `LIGHT_INK_ALPHA_CAP` (L6, §3.3) — the centre over-alpha ceiling for a mark
/// that can composite over a letterform at any moment.
pub const LIGHT_ALPHA_CAP: f32 = 190.0;

/// `RAINBOW_LIGHT_RAIL_ALPHA_CAP` (L6, §3.3) — the ceiling for a mark that is
/// PROVABLY confined to the leading, which the rail line is.
pub const LIGHT_RAIL_ALPHA_CAP: f32 = 236.0;

/// The light ink's luma bar (§3.3): every light-theme colour is scaled down —
/// hue and saturation intact — until it satisfies BOTH this and
/// [`LIGHT_INK_MAX_CHANNEL`].
pub const LIGHT_INK_MAX_LUMA: f32 = 68.0;

/// The light ink's per-channel bar (§3.3). Luma alone is not enough: pure red
/// has a luma of only 54, so a luma bar alone leaves it at a full-blast `FF`.
pub const LIGHT_INK_MAX_CHANNEL: f32 = 150.0;

/// **THE AXIS RULE** (§6.1, D15). `ribbon_beam` is x-major (`ceil(a.x)..
/// ceil(b.x)`, `continue` when `|Δx| < 1e-3`), so a vertical recall drawn
/// through it would emit ZERO train quads. `|dx| ≥ |dy|` takes `ribbon_beam`;
/// `|dy| > |dx|` takes `ribbon_beam_v`, the transposed twin (same
/// `ribbon_profile`, same Bayer, same `GlowBlend`, plus a `glow_under_parity`
/// pin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// `|dx| ≥ |dy|` — `aterm_render::ribbon_beam`.
    Horizontal,
    /// `|dy| > |dx|` — `aterm_render::ribbon_beam_v`.
    Vertical,
}

impl Axis {
    /// The axis of a pixel-space delta, per D15's rule exactly.
    #[must_use]
    pub fn of(dx: f32, dy: f32) -> Self {
        if dx.abs() >= dy.abs() {
            Self::Horizontal
        } else {
            Self::Vertical
        }
    }
}

/// ONE METEOR (§17.1's `Meteor{x0,y0,x1,y1,t0,T,w,grade,mom,retiring}`).
///
/// `Copy`: the pool is a fixed [`FLIGHT_MAX_LIVE`]-slot array (§18) and a
/// style-crossfade ghost snapshots it whole.
#[derive(Clone, Copy, Debug)]
pub struct Meteor {
    /// Window-absolute X of the LAUNCH, px.
    pub x0: f32,
    /// Window-absolute Y of the launch, px.
    pub y0: f32,
    /// Window-absolute X of the LANDING, px — where the caret was observed.
    pub x1: f32,
    /// Window-absolute Y of the landing, px.
    pub y1: f32,
    /// The spawn edge: the frame the caret was first observed at its landing
    /// (T2). Everything the meteor owns is measured from here.
    pub t0: Instant,
    /// §6.2's **`T`** — `timing::flight(cells)`. The ARRIVAL EDGE is
    /// `t0 + t_flight` and is read by the pin, the ring, the fan, the terminal
    /// flash, the kitty's `land_at` and the audio bell (§6.7). One `Instant`;
    /// nothing at the landing waits for a second observation and nothing fires
    /// early.
    pub t_flight: Duration,
    /// Body thickness in px, `ch·(0.36 + 0.34·mom)·(1 + 0.5·grade)` (§6.3).
    pub w: f32,
    /// `clamp(cells/34, 0, 1)` (§6.3) — buys brightness and the fan's reach
    /// ceiling; its width share is capped at ×1.5.
    pub grade: f32,
    /// `Spine::birth_disp()` at launch — §6.3's `mom`. Latched, never re-read:
    /// a meteor's width must not breathe with the typing that follows it.
    pub mom: f32,
    /// When a crossing meteor RETIRED this one (§6.9). Both train layers run
    /// [`RETIRE_DECAY`] from this edge and are culled at exactly
    /// [`FLIGHT_RETIRE_MS`]; the head's position FREEZES where it was and the
    /// train's root retracts toward it over the same 60 ms on `suck-in` — the
    /// post-arrival fade of §6.8 with `R = 60`. Never a pop, never a
    /// fade-in-place: the alpha it decays FROM is the alpha it had, and the
    /// light leaves toward the caret (T5), never dims where it lies. It also
    /// sheds nothing more: a fragment born behind a head that is no longer on
    /// glass would be light with no gesture behind it, the class §19.1
    /// deletes.
    ///
    /// What happens to the NUCLEUS AND COMA depends on whether the head was
    /// still in flight ([`Meteor::handed_over`]): retired mid-flight they stop
    /// on the edge, because "only one head is ever in the air per row";
    /// retired at or after the arrival edge the head was AT REST on its
    /// landing, nothing was in the air to stop, and the arrival's afterglow
    /// decays with the train over the same 60 ms from exactly the coverage it
    /// had — a 30-level coma vanishing on the edge would be the pop this law
    /// forbids.
    pub retired_at: Option<Instant>,
    /// True once a same-direction continuation CHAINED off this head (§6.9).
    /// The head was handed to the continuation: this train is cut at the chain
    /// point and finishes on its natural post-arrival law (§6.8) from there —
    /// no nucleus, no further shedding, no landing of its own, because the
    /// head kept going and it is the continuation's landing that celebrates.
    /// Never set on a train whose head is already at rest: a landed head is
    /// nobody's to hand over, and [`Meteor::landed`] stays false here — the
    /// landing mint refuses a chained train on its own.
    pub chained: bool,
    /// §6.4's `t_land` — the caret's own field `t` at the landing cell,
    /// LATCHED at the spawn edge. Latched rather than re-read because the arc
    /// is a phase lock: a train whose whole colour shifted as the ribbon
    /// walked on underneath it would be a second colour law.
    ///
    /// The RAW walk, never clamped (D4): the classic walk runs past `1.0` on
    /// a long line (`walk_t(47) = 1.861`), and the ribbon draws that cell
    /// through `tri` — orange. Every read here goes through [`arc_t`], which
    /// applies the same `tri` once, so `arc_t(t_land, 0)` IS the ribbon's
    /// stop under the caret; a clamp at the lock made it violet.
    pub t_land: f32,
    /// Path length in cells, `L / cw` (§6.2) — the input to `flight_ms`,
    /// `shed_n` and the fan's count and reach.
    pub cells: f32,
    /// Travel direction, dominant axis — the kitty's facing and the cue's
    /// `dir`, and the chain test of §6.9.
    pub dir: Dir,
    /// The landing cell, `(row, col)`, **as observed at the spawn edge** — the
    /// seed's input and the [`Spawn`]'s report, and read for nothing after
    /// that. The flight's truth is its pixel endpoint [`Meteor::x1`] /
    /// [`Meteor::y1`]: a scroll between spawn and arrival (seam point 12,
    /// [`Meteors::translate_scroll`]) moves the endpoint with the train and
    /// leaves this cell where it was, so every arrival mark is minted at the
    /// endpoint and never re-derived from here — re-deriving it put the pin,
    /// the ring and the fan `rows` rows below a train's end on every PTY
    /// line-feed cascade (D18).
    pub landing: (u16, u16),
    /// The deterministic seed every hashed choice this meteor makes is drawn
    /// from (§18): the shed stations, the fan's phase, size and throw.
    pub seed: u32,
    /// Which shed fragments have already been born — bit `i` for station `i`
    /// (§6.5 layer 5). A fragment is born on the frame the head passes its
    /// station and never twice, which is what makes the sowing a pure function
    /// of the frame sequence rather than of the frame rate.
    pub shed: u8,
    /// Whether the arrival edge's marks have been minted (§6.7). One `Instant`,
    /// one minting: nothing at the landing fires early and nothing waits for a
    /// second observation.
    pub landed: bool,
    /// Whether this landing RINGS — false for an Enter and for a PTY line
    /// feed (D8, §6.11): the ring is reserved for same-row nav and for
    /// history recall, whichever axis the recall flies on.
    pub ring: bool,
    /// Whether this landing cues the five rain glints (D6, D8) — the same
    /// verdict as [`Meteor::ring`], because the rain pairs with the hero and
    /// four m2 that only a full landing carries.
    pub rain: bool,
}

impl Meteor {
    /// **THE ARRIVAL EDGE** (§6.7, §8.1 no. 3) — the single `Instant` every
    /// landing mark and the audio bell share.
    #[must_use]
    pub fn arrival(&self) -> Instant {
        self.t0 + self.t_flight
    }

    /// The path vector in px.
    #[must_use]
    pub fn delta(&self) -> (f32, f32) {
        (self.x1 - self.x0, self.y1 - self.y0)
    }

    /// Path length `L` in px.
    #[must_use]
    pub fn length(&self) -> f32 {
        let (dx, dy) = self.delta();
        dx.hypot(dy)
    }

    /// **THE TRAIN'S OWN SPECTRUM POSITION** `cells_behind_landing` cells
    /// short of the landing, along the path — [`arc_t`] at [`arc_gain`]'s
    /// rate: `tri(t_land − cells·(1/36)·gain)`.
    ///
    /// Measured from the LANDING and not from the head, which is the whole
    /// of "the colours stream backwards" (owner, 2026-09-08): a station's
    /// colour is a function of WHERE ON THE ROW it is, laid there as the
    /// head passed and never recoloured, so relative to the flying head the
    /// colours run backwards at the head's own speed. At the landing cell
    /// it is `tri(t_land)` — the caret's own reflected stop (D4), which the
    /// pin, the fan and the splash all start from.
    #[inline]
    #[must_use]
    pub fn arc(&self, cells_behind_landing: f32) -> f32 {
        tri(self.t_land - cells_behind_landing * WALK_LAY_RATE * arc_gain(self.cells))
    }

    /// Which rasterizer this flight takes (D15).
    #[must_use]
    pub fn axis(&self) -> Axis {
        let (dx, dy) = self.delta();
        Axis::of(dx, dy)
    }

    /// True once a crossing meteor has retired this one (§6.9). The published
    /// name of the flag; the edge itself is [`Meteor::retired_at`], because a
    /// fade needs to know WHEN, not only THAT.
    #[must_use]
    pub fn retiring(&self) -> bool {
        self.retired_at.is_some()
    }

    /// **IS THE HEAD OFF THE GLASS?** (§6.9) True when the head was handed
    /// over while still in flight — to a same-direction continuation
    /// ([`Meteor::chained`]) or to a crossing flight's 60 ms finish
    /// ([`Meteor::retired_at`] before the arrival edge). The nucleus and the
    /// coma are not drawn for such a train, because only one head is ever in
    /// the air per row.
    ///
    /// False for a train retired AT OR AFTER its arrival edge: its head was
    /// at rest on the landing, nothing was in the air to stop, and the
    /// arrival's afterglow decays with the train on the retire law instead
    /// of vanishing — the coma of a landed recall must not pop off the glass
    /// on the frame the next recall launches from its landing. A landing
    /// that was minted ([`Meteor::landed`]) settles the question without a
    /// clock comparison; the instant comparison covers a train retired on
    /// the frame it arrived, before any emit could mint it.
    #[must_use]
    pub fn handed_over(&self) -> bool {
        self.chained
            || self
                .retired_at
                .is_some_and(|at| !self.landed && at < self.arrival())
    }

    /// Does this train have a head IN THE AIR at `at`? Un-retired, not
    /// chained, not landed, and short of its arrival edge. §6.9's cap ("cap
    /// 2 live meteors; a third evicts the oldest through the same 60 ms
    /// finish") evicts the trains that answer false first: a chained stub or
    /// a landed train is one streak's remnant finishing on §6.8, not a head,
    /// and it must never cost a flight on another row its head.
    #[must_use]
    pub fn in_flight_at(&self, at: Instant) -> bool {
        self.retired_at.is_none() && !self.chained && !self.landed && at < self.arrival()
    }

    /// **THE COLOUR LAYER'S OWN ALPHA** at `now` (§6.8, §6.9) — the two-tau
    /// fade times the retire decay, and nothing else. Public because it is the
    /// quantity §6.9's law is stated in ("the first train's colour α ≤ 0.12 by
    /// +90 ms, never increasing"), and a law wants an accessor, not a poke at
    /// private state.
    #[must_use]
    pub fn colour_alpha(&self, now: Instant) -> f32 {
        let age = ms_since(self.t0, now);
        let t = self.t_flight.as_secs_f32() * 1000.0;
        let base = if age <= t {
            1.0
        } else {
            (-(age - t) / TRAIN_COLOUR_TAU_MS).exp()
        };
        base * self.retire_alpha(now)
    }

    /// The retire decay alone (§6.9) — `1` while the meteor is live.
    fn retire_alpha(&self, now: Instant) -> f32 {
        match self.retired_at {
            None => 1.0,
            Some(at) => {
                let u = ms_since(at, now) / FLIGHT_RETIRE_MS;
                (-u.max(0.0) * RETIRE_DECAY).exp()
            }
        }
    }

    /// The instant this meteor's last pixel leaves the glass (§6.2 as
    /// re-ruled: `T + 600`, or §6.9's `R = 60 ms` for a retired one).
    #[must_use]
    pub fn end(&self) -> Instant {
        match self.retired_at {
            Some(at) => at + Duration::from_secs_f32(FLIGHT_RETIRE_MS / 1000.0),
            None => self.arrival() + Duration::from_secs_f32(FLIGHT_OFF_GLASS_MS / 1000.0),
        }
    }
}

/// The landing PIN (§6.5 layer 9) — explicit arms plus a 2×2 nucleus, whipping
/// inward until the only pixel left is the caret.
#[derive(Clone, Copy, Debug)]
pub struct Pin {
    /// The arrival edge this pin was born on.
    pub at: Instant,
    /// The arc position its arms take, `arc_t(t_land, 0)` — snapped through
    /// `spectrum_snap` (C1: point marks snap).
    pub t: f32,
}

/// The landing RING (§6.5 layer 10) — hollow, expanding past its own debris,
/// finished before the fan. Reserved for same-row nav and history recall: an
/// Enter or a PTY line feed has no ring (D8). Since 2026-09-08 it is graded
/// by distance like the fan: `r_full = (3.0 + 1.2·(scale − 1)) ch`
/// ([`ring_full_radius`]), life [`ring_ms`], where `scale` is the landing's
/// [`impact`] — the floor landing is byte-identical to the second round's
/// fixed shockwave, a full-line landing rings twice as wide for 600 ms. It
/// grows ALONG the line: the vertical semi-axis is capped at
/// [`RING_RISE_MAX_CH`], the same flattening the fan takes.
#[derive(Clone, Copy, Debug)]
pub struct Ring {
    /// The arrival edge.
    pub at: Instant,
    /// The landing's [`impact`] — 1.0 at the 8-cell floor.
    pub scale: f32,
    /// This ring's life in ms ([`ring_ms`]).
    pub ms: f32,
}

/// The landing FAN (§6.5 layer 11) — "every landing is its own party". What
/// the meteor DECIDES about the fan; the throw itself is the sky's
/// ([`Stardust::sow_fan`]).
#[derive(Clone, Copy, Debug)]
pub struct Fan {
    /// The arrival edge.
    pub at: Instant,
    /// Star count, `min(19 + 3.6·(impact − 1), 28) + party·8`,
    /// ≤ [`super::timing::FAN_MAX_N`].
    pub n: u8,
    /// Throw reach in px — how far ALONG THE LINE the fan is thrown over the
    /// sky's throw window; the sky jitters each star's share of it, and caps
    /// the rise on its own vertical reach law (`stardust::FAN_RISE_MAX_CH`,
    /// the ring's squash): distance buys length along the line, not height.
    pub reach: f32,
    /// The seed that varies count, phase, size and throw — the whole of the
    /// party's variance, and deterministic (§18).
    pub seed: u32,
    /// Whether this landing carries its gold m1 hero. False for an Enter (D8).
    pub hero: bool,
}

/// ONE SPARK'S THROW (2026-09-08), resolved at the arrival edge from the
/// landing's seed so the frame path does no hashing and no trigonometry:
/// its birth velocity in px/ms and its life in ms. Position at `age` is
/// `v·age + ½g·age²` ([`spark_at`]) — a pure function of the age, no
/// integration, no drift (§18).
#[derive(Clone, Copy, Debug, Default)]
pub struct Spark {
    /// Birth velocity along the row, px/ms.
    pub vx: f32,
    /// Birth velocity down the glass, px/ms (negative is UP).
    pub vy: f32,
    /// Life, ms.
    pub life: f32,
    /// The star's arm half-length while bright, px
    /// ([`SPARK_ARM_MIN_PX`]..=[`SPARK_ARM_MAX_PX`]).
    pub arm: u8,
}

/// **WHAT THE SKY'S PROBE SAID** about the cells the landing's flash and
/// splash may light, latched ONCE per landing on the frame after the mint
/// ([`Meteors::sow_into`], the meteor's one sight of the probe) and read for
/// its life. Bit `k` of [`SkyMask::above`] / [`SkyMask::below`] is the cell
/// `k − SPLASH_CELLS` columns from the caret on the row above / below,
/// set where that cell is PROVABLY blank (`Some(false)`, §5.4 / L4 — unknown
/// is not blank); bit `k` of [`SkyMask::flash`] is the same for the landing
/// row's own cell `k − FLASH_CELLS` columns from the caret. Latched rather
/// than read per frame because a frame is a function of `now` and the
/// landing (§18), and because the probe is the sky's: the meteor never holds
/// it, it asks once through the hand-off it already makes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkyMask {
    /// Blank cells of the row ABOVE the landing row, `2·SPLASH_CELLS + 1`
    /// bits centred on the caret's column.
    pub above: u16,
    /// Blank cells of the row BELOW the landing row.
    pub below: u16,
    /// Blank cells of the landing row itself under the flash,
    /// `2·FLASH_CELLS + 1` bits centred on the caret's column.
    pub flash: u8,
}

impl SkyMask {
    /// Ask the probe about every cell a landing at `(x, y)` may light.
    #[must_use]
    fn probe(x: f32, y: f32, probe: &GlyphProbe, geom: Geom) -> Self {
        let (cw, ch) = ((geom.cw as f32).max(1.0), geom.ch as f32);
        let blank = |dx: i32, dy: i32| {
            let px = (x + dx as f32 * cw).round() as i32;
            let py = (y + dy as f32 * ch).round() as i32;
            probe.at_px(px, py, geom) == Some(false)
        };
        let mut mask = Self::default();
        let reach = SPLASH_CELLS as i32;
        for k in -reach..=reach {
            let bit = 1_u16 << (k + reach);
            if blank(k, -1) {
                mask.above |= bit;
            }
            if blank(k, 1) {
                mask.below |= bit;
            }
        }
        for k in -FLASH_CELLS..=FLASH_CELLS {
            if blank(k, 0) {
                mask.flash |= 1_u8 << (k + FLASH_CELLS);
            }
        }
        mask
    }

    /// Is the cell `k` columns from the caret on the row above (`−1`) or
    /// below (`+1`) proven blank?
    #[must_use]
    fn sky_blank(self, side: i32, k: i32) -> bool {
        let bit = 1_u16 << (k + SPLASH_CELLS as i32);
        let row = if side < 0 { self.above } else { self.below };
        row & bit != 0
    }

    /// Is the landing row's cell `k` columns from the caret proven blank?
    #[must_use]
    fn flash_blank(self, k: i32) -> bool {
        self.flash & (1_u8 << (k + FLASH_CELLS)) != 0
    }
}

/// The marks the arrival edge mints together (§17.1's
/// `Landing{pin, ring, fan}`, plus the 2026-09-08 splash and sparks). They
/// share ONE `Instant`; nothing at the landing fires early and nothing waits
/// for a second observation (§6.7).
#[derive(Clone, Copy, Debug)]
pub struct Landing {
    /// Always present — the pin IS the caret (§6.5 layer 9, D8).
    pub pin: Pin,
    /// Absent on an Enter / vertical landing (D8).
    pub ring: Option<Ring>,
    /// Always present; its composition varies (D8).
    pub fan: Fan,
    /// Window-absolute X of the landing cell's centre, px. The pin closes onto
    /// THIS point, which is the caret's own; carried on the landing because a
    /// landing outlives the meteor that minted it.
    pub x: f32,
    /// Window-absolute Y of the landing cell's centre, px.
    pub y: f32,
    /// How many of [`Landing::spark`] this impact throws ([`SPARK_N_BASE`]);
    /// zero for the small landing (D8: an Enter lands small).
    pub sparks: u8,
    /// The sparks' throws, the first [`Landing::sparks`] of them live —
    /// hashed once at the mint from the fan's own seed, so one landing's
    /// whole party is one number (§18) and the frame path only adds.
    pub spark: [Spark; SPARK_N_MAX],
    /// Whether this landing splashes and flashes — the same verdict as the
    /// ring (D8): the splash and the flash are the big landing's.
    pub splash: bool,
    /// What the sky's probe said about the cells the flash and the splash
    /// may light — `None` until the hand-off after the mint latches it
    /// ([`Meteors::sow_into`]). On the mint frame itself the flash draws at
    /// the transient cap everywhere and the splash draws nothing (its reach
    /// is zero on that frame anyway, `r(0) = 0`).
    pub sky: Option<SkyMask>,
}

impl Landing {
    /// When the last landing pixel leaves the glass: the pin closes at
    /// [`PIN_MS`], the flash's colour at [`FLASH_COLOUR_MS`], the ring
    /// finishes at its own graded life ([`Ring::ms`], [`RING_MS`] at the
    /// floor), the splash at [`SPLASH_MS`], the last spark at
    /// [`SPARK_LIFE_MAX_MS`]. The fan is not counted because the fan is
    /// STARS, and stars are the sky's pool, not this one. Under reduced
    /// motion the static form lives the pool's whole horizon
    /// ([`super::timing::FLIGHT_OFF_GLASS_MS`]) and leaves on the theme's
    /// one linear fade.
    #[must_use]
    pub fn end(&self) -> Instant {
        let ring_ms = self.ring.map_or(0.0, |r| r.ms);
        let life = PIN_MS
            .max(FLASH_COLOUR_MS)
            .max(ring_ms)
            .max(SPLASH_MS)
            .max(SPARK_LIFE_MAX_MS);
        self.pin.at + Duration::from_secs_f32(life / 1000.0)
    }

    /// The landing's end under reduced motion: the pool's own horizon, so
    /// the static form is on glass exactly as long as the static train is.
    #[must_use]
    fn static_end(&self) -> Instant {
        self.pin.at + Duration::from_secs_f32(FLIGHT_OFF_GLASS_MS / 1000.0)
    }

    /// The reduced-motion form's alpha at `now`: `1`, then the theme's one
    /// linear fade over [`REDUCED_MOTION_FADE_MS`] to [`Landing::static_end`]
    /// (§6.11, §2.5's single exception).
    #[must_use]
    fn static_alpha(&self, now: Instant) -> f32 {
        let left = self
            .static_end()
            .saturating_duration_since(now)
            .as_secs_f32()
            * 1000.0;
        clamp01(left / REDUCED_MOTION_FADE_MS.max(1.0))
    }

    /// **WHEN THE LANDING STOPS MOVING** — the last instant any of its marks
    /// is in MOTION on glass: the pin's arms close through [`PIN_MS`], the
    /// flash turns white → spectrum through [`FLASH_COLOUR_MS`], the ring
    /// expands and spins through [`RING_MS`], the splash reaches through
    /// [`SPLASH_MS`], and every spark is ballistic until its own chroma cull
    /// ([`spark_cull_u`] of its life). Each mark is under its cull by then,
    /// so past this instant nothing of the landing draws and the pool only
    /// waits for [`Landing::end`]. The cadence law's motion bound
    /// ([`Meteors::brisk`]): the big landing is still by `T + 516`, an
    /// Enter's small landing (pin only) by `T + 150`. Reads the sparks it
    /// carries — no state, no allocation.
    #[must_use]
    pub fn moving_until(&self) -> Instant {
        let mut ms = PIN_MS;
        if let Some(r) = self.ring {
            ms = ms.max(r.ms);
        }
        if self.splash {
            ms = ms.max(SPLASH_MS).max(FLASH_COLOUR_MS);
        }
        let cull = spark_cull_u();
        for s in self.spark.iter().take(usize::from(self.sparks)) {
            ms = ms.max(s.life * cull);
        }
        self.pin.at + Duration::from_secs_f32(ms / 1000.0)
    }
}

/// The share of a spark's life at which it is under the chroma cull and
/// [`spark_at`] stops drawing it: its alpha is `spend((u − SPARK_HOLD_U) /
/// (1 − SPARK_HOLD_U))`, under [`CHROMA_CULL_ALPHA`] from
/// `u = SPARK_HOLD_U + (1 − SPARK_HOLD_U)·(1 − √CHROMA_CULL_ALPHA)` ≈ 0.861.
/// Solved here rather than spelled, so the cull the cadence law reads and
/// the cull the frame path applies can never drift apart
/// (`a_spark_is_culled_exactly_where_the_cadence_law_says_it_is`).
#[must_use]
fn spark_cull_u() -> f32 {
    SPARK_HOLD_U + (1.0 - SPARK_HOLD_U) * (1.0 - CHROMA_CULL_ALPHA.sqrt())
}

/// What a spawn minted, handed back to `Engine` so the seam wiring lives in
/// ONE place: the companion impulse (§7.2), the audio gesture (§12), and the
/// glint bucket's emptying (§13) are engine-level effects, not meteor-level
/// ones, and a producer that reached across to do them itself is how v1 grew
/// five momentum integrators.
#[derive(Clone, Copy, Debug)]
pub struct Spawn {
    /// The spawn edge.
    pub t0: Instant,
    /// §6.2's `T`, so the bell's `Voice::delay` and the kitty's `land_at` read
    /// the same number the pixels do.
    pub t_flight: Duration,
    /// Travel direction — the kitty's facing and the cue's `dir`.
    pub dir: Dir,
    /// Path length in cells (§6.2's `cells`), **as the integer the audio
    /// receives** (`SoundKind::Meteor { cells: u16 }`). The flight's `T` was
    /// computed from THIS number, so `timing::flight_ms(cells)` at the synth
    /// resolves to the same `Instant` the pin and the squash read (§8.1
    /// no. 3) — a fractional distance here would let the bell miss the
    /// landing frame by up to half a millisecond per cell of rounding.
    pub cells: u16,
    /// `clamp(cells/34, 0, 1)`.
    pub grade: f32,
    /// The landing cell.
    pub landing: (u16, u16),
    /// Whether this landing rings (false for an Enter or a PTY line feed,
    /// D8).
    pub ring: bool,
    /// Whether this landing cues the five rain glints (false for an Enter or
    /// a PTY line feed, D8).
    pub rain: bool,
}

/// ONE STAR BIRTH THE METEOR DECIDED, waiting for the sky (§6.5 layers 5 and
/// 11, §6.12). Everything a stardust entry point needs, resolved on the frame
/// the meteor decided it; the sky adds clearance, velocity, envelope and the
/// star itself.
#[derive(Clone, Copy, Debug)]
enum Sow {
    /// One shed fragment at its station, born the frame the head passed it.
    Shed {
        /// The birth edge — the frame the head passed the station.
        at: Instant,
        /// The station, the head's velocity there, the arc colour there.
        spec: ShedSow,
    },
    /// The landing fan, on the arrival edge.
    Fan {
        /// The arrival edge.
        at: Instant,
        /// Reach, count, hero, seed, landing pixel, landing colour.
        spec: FanSow,
    },
    /// A sub-floor hop's mini-fan (§6.12, D17).
    MiniFan {
        /// The hop's observed edge.
        at: Instant,
        /// The landing cell's centre, px.
        at_px: (f32, f32),
        /// The hop's seed.
        seed: u32,
    },
}

/// The frame a hand-off was decided on — every field of [`Ctx`] except its
/// borrow of the config, which is copied. [`Meteors::sow_into`] rebuilds the
/// `Ctx` the sky's entry points take from this, so the stars are cleared and
/// tinted against the SAME geometry, config and clock that minted them, and
/// the engine's one line of wiring stays one line.
#[derive(Clone, Copy)]
struct Minted {
    /// [`Ctx::now`].
    now: Instant,
    /// [`Ctx::geom`].
    geom: Geom,
    /// [`Ctx::cfg`], by value.
    cfg: Config,
    /// [`Ctx::disp`].
    disp: f32,
    /// [`Ctx::birth_disp`].
    birth_disp: f32,
    /// [`Ctx::phase`].
    phase: f32,
    /// [`Ctx::caret`].
    caret: (u16, u16),
    /// [`Ctx::caret_t`].
    caret_t: f32,
}

/// `Geom` carries no `Debug` of its own, so the frame's geometry is printed
/// as its cell and origin figures — enough to read a pool dump.
impl fmt::Debug for Minted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Minted")
            .field("now", &self.now)
            .field("cell", &(self.geom.cw, self.geom.ch))
            .field("origin", &(self.geom.origin_x, self.geom.origin_y))
            .field("cfg", &self.cfg)
            .field("disp", &self.disp)
            .field("birth_disp", &self.birth_disp)
            .field("phase", &self.phase)
            .field("caret", &self.caret)
            .field("caret_t", &self.caret_t)
            .finish()
    }
}

impl Minted {
    fn of(ctx: &Ctx<'_>) -> Self {
        Self {
            now: ctx.now,
            geom: ctx.geom,
            cfg: *ctx.cfg,
            disp: ctx.disp,
            birth_disp: ctx.birth_disp,
            phase: ctx.phase,
            caret: ctx.caret,
            caret_t: ctx.caret_t,
        }
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            now: self.now,
            geom: self.geom,
            cfg: &self.cfg,
            disp: self.disp,
            birth_disp: self.birth_disp,
            phase: self.phase,
            caret: self.caret,
            caret_t: self.caret_t,
            mend: None,
        }
    }
}

/// THE METEOR producer: the live pool, its landings, and the station scratch.
#[derive(Clone, Debug)]
pub struct Meteors {
    /// The meteor pool (§18): at most [`FLIGHT_MAX_LIVE`] heads in the air
    /// (§6.9; v1 allowed 4 and they read as rulers piling up — a third evicts
    /// the oldest through the same 60 ms finish), and at most [`METEOR_POOL`]
    /// entries in all, finishing trains included. Reserved once, never grown.
    pub live: Vec<Meteor>,
    /// The landing pool: one per arrived meteor, at most [`LANDING_POOL`].
    pub landings: Vec<Landing>,
    /// **STAR BIRTHS THIS PRODUCER DECIDED AND DOES NOT BUILD** — the shed
    /// fragments (§6.5 layer 5), the landing fan (layer 11) and the sub-floor
    /// mini-fan (§6.12), staged for the sky. Resident and drained, never
    /// rebuilt (§18). See [`Meteors::sow_into`].
    sown: Vec<Sow>,
    /// The frame the staged births were decided on — see [`Minted`].
    minted_on: Option<Minted>,
    /// Reused station scratch for `ribbon_beam` / `ribbon_beam_v` (§18's
    /// "a reused 96-station vertex scratch"). Taken with `mem::take` for the
    /// duration of an emit and put back, so the steady-state frame path
    /// allocates nothing.
    verts: Vec<RibbonVertex>,
    /// The spawn ORDINAL — the deterministic stand-in for `born` in §18's
    /// "hashed from `(row, col, born)`".
    ///
    /// `aterm_time::Instant` exposes no bits to hash (it is `std::time::
    /// Instant` on every target that has one), so the seed takes the count of
    /// spawns since the last [`Meteors::reset`] instead. It is a pure function
    /// of the EVENT sequence, not of any clock, which is exactly the property
    /// §18 wants — and it is what makes `every_landing_is_its_own_party` true
    /// for four A/E round-trips, whose `(row, col)` pairs are identical.
    ord: u32,
    /// The reduced-motion posture of the last frame drawn, cached for the
    /// cadence law: under it nothing flies and nothing moves (§6.11), so
    /// the pool is never brisk and its one change is the theme's linear
    /// fade at the end of the life.
    reduced: bool,
}

impl Default for Meteors {
    /// The same pool [`Meteors::new`] builds — `Engine::new` reaches this
    /// through `Default`, and a derived `Default` would hand it four empty
    /// `Vec`s whose first spawn and first emit allocate (§18).
    fn default() -> Self {
        Self::new()
    }
}

impl Meteors {
    /// An empty pool, with every resident buffer reserved to its published
    /// depth so neither the spawn path nor the frame path allocates (§18).
    #[must_use]
    pub fn new() -> Self {
        Self {
            live: Vec::with_capacity(METEOR_POOL),
            landings: Vec::with_capacity(LANDING_POOL),
            sown: Vec::with_capacity(SOW_SCRATCH),
            minted_on: None,
            verts: Vec::with_capacity(STATIONS_MAX),
            ord: 0,
            reduced: false,
        }
    }

    /// Ingest one engine event; mint a meteor if it earns one.
    ///
    /// Returns [`Spawn`] exactly when a flight was born, so the engine can mint
    /// the companion impulse, start the audio gesture and empty the glint
    /// bucket on the SAME edge.
    ///
    /// §6.1's trigger: a licensed same-row `|dc| ≥`
    /// [`JUMP_MIN_CELLS`], or any row change the arms license (nav-hinted
    /// `|dr| ≥ 1`, a `return_licensed` cold Enter, a synthetic preview, a PTY
    /// line feed). **Typed wraps never fly** and reflow licenses nothing — a
    /// [`Licence::Typed`] move mints nothing here, which is
    /// `ink_wrap_rainbow_keeps_the_ribbon_no_zoom`. A 2-7-cell same-row hop is
    /// a mini-fan and no flight at all (§6.12, D17). A meteor is **not** gated
    /// on a live ribbon: a cold `Home`/`End` on a dead prompt flies the full
    /// anatomy, shed fragments included.
    pub fn on_event(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>) -> Option<Spawn> {
        match *ev {
            Event::Move {
                from,
                to,
                licence,
                dir,
            } => self.on_move(from, to, licence, dir, at, ctx),
            // §8.2: focus loss embers everything out. The meteors take the
            // same 60 ms finish a crossing flight would have given them —
            // one retire law, not two.
            Event::Focus(false) => {
                for m in &mut self.live {
                    if m.retired_at.is_none() {
                        m.retired_at = Some(at);
                    }
                }
                None
            }
            // T1, restated where it would be easiest to break: a keypress is
            // not a move. `Event::Return` is seam point 2's `note_return` —
            // the KEY — and the caret's own observed motion arrives as the
            // `Event::Move` that carries `Licence::Return`. Minting a flight
            // here would draw a meteor for an Enter the shell swallowed.
            _ => None,
        }
    }

    /// Draw the meteors and their landings.
    ///
    /// **Called FIRST in the emit order** — the bolts-first law (§6.5): on a
    /// monster jump the train alone can approach the quad budget, and
    /// truncation sheds from the END, so the meteor must be in the buffer
    /// before the ribbon and the sky are.
    ///
    /// §6.5's layer table, bottom → top: layer 1 (colour train) into
    /// `frame.under` through `ribbon_beam` / `ribbon_beam_v` by [`Axis`],
    /// self-capped at [`UNDER_QUAD_CAP`] by SHEDDING STATIONS from the tail —
    /// never by starving the ribbon; layers 2 and 3 (white train, shoulder)
    /// into `frame.out` under [`WHITE_QUAD_CAP`]; layers 6 and 7 (coma,
    /// nucleus, as the corona) into `frame.halos`; the impact — the splash
    /// into `frame.under` under [`SPLASH_QUAD_CAP`], layers 9 and 10 (pin,
    /// shockwave) and the sparks into `frame.out`. Layers 5 and 11 (shed
    /// fragments, fan) are MINTED onto [`Meteors::sown`] rather than drawn —
    /// see [`Meteors::sow_into`].
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.reduced = ctx.cfg.reduced_motion;
        let now = ctx.now;
        // The frame the staged births belong to — captured before the idle
        // early-out, because a mini-fan staged by `on_event` on an otherwise
        // empty pool must still reach the sky on THIS frame (T2).
        self.minted_on = Some(Minted::of(ctx));
        self.live.retain(|m| now < m.end());
        self.landings.retain(|l| now < l.end());
        if self.live.is_empty() && self.landings.is_empty() {
            return;
        }

        // §18: the station scratch is resident. Taking it lends the vector out
        // for the emit and hands it straight back, so nothing allocates and
        // nothing borrows `self` twice.
        let mut verts = mem::take(&mut self.verts);
        let (live, landings, sown) = (&mut self.live, &mut self.landings, &mut self.sown);

        for m in live.iter_mut() {
            if let Some(f) = Flight::of(m, ctx) {
                draw_train(&mut verts, m, &f, ctx, frame);
                draw_head(m, &f, ctx, frame);
                // §6.9: a head that was handed over — to a crossing flight's
                // 60 ms finish or to a chained continuation — is no longer on
                // the glass, and a head that is not there sheds nothing. A
                // fragment born behind a phantom head would be light with no
                // gesture behind it, the class §19.1 deletes.
                if m.retired_at.is_none() && !m.chained {
                    shed(m, &f, ctx, sown);
                }
            }
            // §6.7: the arrival edge is ONE instant, and this is the one place
            // it is spent. A meteor retired before it arrived mints nothing:
            // the pin closes into THE CARET, and the caret has left. A chained
            // train mints nothing either — its head kept going, and it is the
            // continuation's landing that celebrates (§6.9).
            if !m.landed && !m.chained && now >= m.arrival() && m.retired_at.is_none() {
                m.landed = true;
                // Seam point 12: a scroll that carried the endpoint off the
                // glass took the landing with it. Nothing is minted — a pin
                // clamped onto the top row would be light on a line nobody
                // flew to, and a fan sown above the glass would sit in the
                // sky's pool for 460 ms drawing nothing.
                if !on_glass(ctx.geom, m.x1, m.y1) {
                    continue;
                }
                let landing = mint_landing(m, ctx);
                // §6.5 layer 11: the fan reads no field — its colour is
                // ROYGBIV in order around the fan, and the walk STARTS at
                // the landing cell's own stop (§6.4's `t_m(0)`, the pin's
                // colour), so the fan and the pin read as one landing.
                sown.push(Sow::Fan {
                    at: landing.fan.at,
                    spec: FanSow {
                        at_px: (landing.x, landing.y),
                        reach: landing.fan.reach,
                        tint_t: landing.pin.t,
                        seed: landing.fan.seed,
                        n: landing.fan.n,
                        hero: landing.fan.hero,
                    },
                });
                // §6.11: under reduced motion the landing is minted too —
                // its STATIC form (the flash and the colours, no expansion,
                // no pin, no sparks) is what each `draw_*` below draws when
                // the config says so.
                if landings.len() >= LANDING_POOL {
                    landings.remove(0);
                }
                landings.push(landing);
            }
        }
        // The impact, bottom → top, all `out`: the splash's two sky bands,
        // the flash, the pin, the shockwave and the sparks. The fan is the
        // sky's. Spark halos share one budget across the landings.
        let halo_cap = frame.halos.len() + SPARK_HALO_CAP;
        for l in landings.iter() {
            draw_splash(&mut verts, l, ctx, frame);
            draw_flash(l, ctx, frame);
            draw_pin(l, ctx, frame);
            draw_ring(l, ctx, frame);
            draw_sparks(l, ctx, frame, halo_cap);
        }

        self.verts = verts;
    }

    /// **HAND THE DECIDED BIRTHS TO THE SKY** — the shed fragments (§6.5
    /// layer 5), the landing fan (layer 11) and the sub-floor mini-fan
    /// (§6.12).
    ///
    /// Every star in v2 is built and drawn by [`Stardust`] from its ONE
    /// recipe (§5.7, D2). These are meteor-LANE stars — transient pricing,
    /// exempt from the typing glint bucket, silent — and the sky's
    /// transient-lane entry points are the ONLY way into the pool for them:
    /// [`Stardust::sow_shed`] adds §5.6's along-plus-sputter velocity,
    /// [`Stardust::sow_fan`] the radial throw over the sky's own window,
    /// [`Stardust::sow_mini_fan`] the 1-2 px hop; each applies §5.4's glyph
    /// clearance ("a core never lands on a probed glyph") before a star
    /// exists. The meteor never builds a `Star`, so no second throw window,
    /// no second hash, no second class ladder can drift from the sky's.
    ///
    /// The `Ctx` those entry points take is rebuilt from the frame the births
    /// were decided on ([`Minted`], captured by [`Meteors::emit`]); until an
    /// emit has run the births stay staged rather than being priced against
    /// a frame that does not exist yet.
    ///
    /// **THE ONE LINE OF ENGINE WIRING THIS NEEDS**, in `Engine::tick`,
    /// immediately after `self.meteor.emit(&ctx, out)`:
    ///
    /// ```text
    /// self.meteor.sow_into(&mut self.stardust);
    /// ```
    pub fn sow_into(&mut self, dust: &mut Stardust) {
        let Some(minted) = self.minted_on else {
            return;
        };
        // THE ONE SIGHT OF THE PROBE: a landing minted this frame asks the
        // sky which of the cells its flash and its splash may light are
        // provably blank, once, and keeps the answer (`SkyMask`). The probe
        // is the sky's and stays the sky's; the meteor reads it here because
        // this is the hand-off it already makes every tick.
        for l in &mut self.landings {
            if l.sky.is_none() {
                l.sky = Some(SkyMask::probe(l.x, l.y, dust.probe(), minted.geom));
            }
        }
        let ctx = minted.ctx();
        for sow in self.sown.drain(..) {
            match sow {
                Sow::Shed { at, spec } => dust.sow_shed(at, spec, &ctx),
                Sow::Fan { at, spec } => dust.sow_fan(at, spec, &ctx),
                Sow::MiniFan { at, at_px, seed } => dust.sow_mini_fan(at, at_px, seed, &ctx),
            }
        }
    }

    /// Meteors with a head in the air or a train on its natural finish —
    /// `trail status`'s `meteors=`, and ≤ [`FLIGHT_MAX_LIVE`] by §6.9's cap.
    /// A train on the 60 ms retire is a FINISH, not a meteor, and is not
    /// counted; [`Meteors::at_rest`] still waits for it. A landed train and
    /// a chained stub (cut at the chain point, finishing on §6.8's own taus)
    /// are both trains on their natural finish and both count — the cap in
    /// [`Meteors::on_event`] is what holds the count at two, and it retires
    /// such remnants before it retires a head in flight.
    #[must_use]
    pub fn live(&self) -> usize {
        self.live.iter().filter(|m| m.retired_at.is_none()).count()
    }

    /// True when nothing is in the air and no landing is finishing. Every
    /// meteor pixel is gone by `T + 600` (§6.2 as re-ruled 2026-09-08), so
    /// this latches within 720 ms of the last flight.
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.live.is_empty() && self.landings.is_empty()
    }

    /// **THE CADENCE LAW, brisk half** — whether per-frame MOTION is on
    /// glass: a head in flight, a train whose colour root is still
    /// retracting toward the landing (§6.8's `R = 460` — the last train
    /// pixel leaves with it, `Flight::of` is `None` past it), a retiring
    /// train slurping into its frozen head (§6.9's `R = 60`), or a landing
    /// still MOVING — its pin closing, its shockwave expanding and spinning,
    /// its splash reaching, a spark still falling
    /// ([`Landing::moving_until`]). This is seam point 8's word: the host's
    /// phase-locked train draws every frame of it, ONE wake per lane tick.
    ///
    /// It is NOT "the pool is non-empty": a landing's last spark is under
    /// its cull by `T + 516` and an Enter's small landing is still by
    /// `T + 150`, while the pool's horizon is `T + 600` — the difference is
    /// frames the train would draw nothing on. Under reduced motion nothing
    /// moves (§6.11), so the answer is `false` whatever is live.
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        if self.reduced {
            return false;
        }
        let root_suck = Duration::from_secs_f32(ROOT_SUCK_MS / 1000.0);
        self.live.iter().any(|m| match m.retired_at {
            Some(_) => now < m.end(),
            None => now < m.arrival() + root_suck,
        }) || self.landings.iter().any(|l| now < l.moving_until())
    }

    /// **THE ATTACK** — the part of a meteor's life the engine asks for
    /// EVERY frame at [`super::FRAME_CADENCE`] (`a_flight_asks_for_every_
    /// frame`): the flight, then the pin's arms whipping into the caret and
    /// the white layer closing with them — `T .. T + 150`, [`PIN_MS`] =
    /// [`WHITE_CLOSE_MS`], the one edge everything white leaves on — and a
    /// retiring train's 60 ms finish (§6.9). The shockwave is most of the
    /// way out and the splash has burst inside it (both on the quartic's
    /// first 110 ms); what moves after it is the RELEASE, which
    /// [`Meteors::brisk`] still owns but the fold does not (see
    /// [`Meteors::next_change_deadline`]).
    fn attack(&self, now: Instant) -> bool {
        if self.reduced {
            return false;
        }
        let close = Duration::from_secs_f32(PIN_MS / 1000.0);
        self.live.iter().any(|m| match m.retired_at {
            Some(_) => now < m.end(),
            None => now < m.arrival() + close,
        }) || self.landings.iter().any(|l| now < l.pin.at + close)
    }

    /// **THE CADENCE LAW** — the next instant a meteor changes what is on
    /// glass. `None` at rest (T6).
    ///
    /// Three phases, never two at once:
    ///
    /// * **the attack** ([`Meteors::attack`], `t₀ .. T + 150`) — the fold's
    ///   brisk: the next frame, whatever else is live;
    /// * **the release** (past it, while [`Meteors::brisk`] — the shockwave
    ///   to `T + 480`, the sparks to their cull at `T + 516`, the train's
    ///   root to `T + 460`) — brisk to seam point 8 ALONE. The fold names
    ///   only the [`super::TAIL_FLOOR`]: the host's train draws every frame
    ///   of the motion, and its lane tick is the earliest wake the meteor
    ///   asks for; an engine-only reader still samples the release at the
    ///   tail rate. Every u8 step of the release's own fades is under the
    ///   floor (a `spend` over ≥ 240 ms at 118 levels steps every < 12 ms),
    ///   so the floor IS its next visible step;
    /// * **the horizon** (nothing moves; the pool waits for `T + 600`) — a
    ///   disappearance, offered as a tail so it lands on the floor grid.
    ///
    /// **Why the release is not the fold's brisk — measured.** The fold's
    /// brisk answer is `now + FRAME_CADENCE`, 8.333 ms, and the host's seam
    /// takes the engine's deadline UNDER its own 16.667 ms lane tick
    /// (`CursorGlow::next_change_deadline` is the `min` of the two), so a
    /// producer that is the fold's brisk wakes the seam TWICE per lane tick
    /// for as long as it says so. At `T + 320` the meteor could afford that
    /// on slack: the seam census (`under_v2_the_seam_never_paces_the_frame_
    /// train_for_v1s_crown`, 16.667 ms lane) read 474 frame-cadence wakes
    /// against 502 ticks. With the 2026-09-08 impact the pool stayed the
    /// fold's brisk to `T + 600`, and the same census read **567 against
    /// 521** — 345 of the meteor's 353 wakes 8.333 ms apart, each pair one
    /// lane tick. The law is one brisk wake per lane tick, so the fold's
    /// brisk is the attack's alone (the engine's flight law needs no more)
    /// and the release rides the train the predicate already arms.
    ///
    /// Under reduced motion the mark is static until the theme's one linear
    /// fade opens ([`REDUCED_MOTION_FADE_MS`] before the end) — a regime
    /// change, on the floor grid — then a tail at the floor.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let mut cad = Cadence::at(now);
        if self.attack(now) {
            cad.brisk();
        } else if self.brisk(now) {
            cad.tail(0.0);
        }
        for m in &self.live {
            if self.reduced {
                let fade_from = m.end() - Duration::from_secs_f32(REDUCED_MOTION_FADE_MS / 1000.0);
                if m.retired_at.is_none() && now < fade_from {
                    cad.tail_at(fade_from);
                } else {
                    cad.tail(0.0);
                }
            }
            cad.tail_at(m.end());
        }
        for l in &self.landings {
            if self.reduced {
                // §6.11: the static landing changes once — when its linear
                // fade opens — and then steps at the floor to its end.
                let fade_from =
                    l.static_end() - Duration::from_secs_f32(REDUCED_MOTION_FADE_MS / 1000.0);
                if now < fade_from {
                    cad.tail_at(fade_from);
                } else {
                    cad.tail(0.0);
                }
                cad.tail_at(l.static_end());
            } else {
                cad.tail_at(l.end());
            }
        }
        cad.take()
    }

    /// Translate live geometry by a scroll (seam point 12).
    ///
    /// Marks that leave the grid are DROPPED rather than clamped: a train
    /// pinned to row 0 by a clamp is light on a line nobody flew across. The
    /// flight's pixel endpoints move; its spawn-time cell
    /// ([`Meteor::landing`]) does not, and nothing reads it after the spawn
    /// — the arrival mints at the endpoint, and an endpoint the scroll
    /// carried off the glass mints nothing ([`Meteors::emit`]).
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        let dy = f32::from(rows) * f32::from(cell_h);
        if dy == 0.0 {
            return;
        }
        for m in &mut self.live {
            m.y0 -= dy;
            m.y1 -= dy;
        }
        for l in &mut self.landings {
            l.y -= dy;
        }
        for s in &mut self.sown {
            match s {
                Sow::Shed { spec, .. } => spec.at_px.1 -= dy,
                Sow::Fan { spec, .. } => spec.at_px.1 -= dy,
                Sow::MiniFan { at_px, .. } => at_px.1 -= dy,
            }
        }
        self.live.retain(|m| m.y0.max(m.y1) >= 0.0);
        self.landings.retain(|l| l.y >= 0.0);
        self.sown.retain(|s| {
            let y = match s {
                Sow::Shed { spec, .. } => spec.at_px.1,
                Sow::Fan { spec, .. } => spec.at_px.1,
                Sow::MiniFan { at_px, .. } => at_px.1,
            };
            y >= 0.0
        });
    }

    /// Drop everything in the air.
    pub fn reset(&mut self) {
        self.live.clear();
        self.landings.clear();
        self.sown.clear();
        self.minted_on = None;
        self.verts.clear();
        self.ord = 0;
    }

    // -- §6.1 the trigger, §6.9 the retire law -----------------------------

    fn on_move(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        licence: Licence,
        dir: Dir,
        at: Instant,
        ctx: &Ctx<'_>,
    ) -> Option<Spawn> {
        // §6.1: typed wraps never fly, and reflow licenses nothing. A typed
        // echo lays ribbon; it does not streak.
        if matches!(licence, Licence::Typed) {
            return None;
        }
        let drow = i32::from(to.0) - i32::from(from.0);
        let dcol = i32::from(to.1) - i32::from(from.1);
        if drow == 0 {
            let n = dcol.unsigned_abs();
            if n < u32::from(JUMP_MIN_CELLS) {
                // §6.12 / D17: a 2-7-cell hop is a mini-fan and NOT a flight.
                // Below two cells it is scrubbing, and scrubbing stays calm.
                if (u32::from(MINI_FAN_MIN_CELLS)..=u32::from(MINI_FAN_MAX_CELLS)).contains(&n) {
                    self.ord = self.ord.wrapping_add(1);
                    self.sown.push(Sow::MiniFan {
                        at,
                        at_px: ctx.geom.cell_center(to.0, to.1),
                        seed: seed_of(to.0, to.1, self.ord),
                    });
                }
                return None;
            }
        }

        let geom = ctx.geom;
        let (x0, y0) = geom.cell_center(from.0, from.1);
        let (x1, y1) = geom.cell_center(to.0, to.1);
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let len = (x1 - x0).hypot(y1 - y0);
        if len < 1.0 {
            return None;
        }
        // §6.2's `cells = L / cw`, resolved to the ONE integer the audio
        // receives (`SoundKind::Meteor { cells: u16 }`), so the flight clock
        // and the bell's delay are `flight_ms` of the same number (§8.1).
        let cells_n = (len / cw.max(1.0)).round().clamp(1.0, f32::from(u16::MAX)) as u16;
        let cells = f32::from(cells_n);
        let grade = (cells / GRADE_CELLS).clamp(0.0, 1.0);
        let mom = clamp01(ctx.birth_disp);
        let w = ch * (W_BASE_CH + W_MOM_CH * mom) * (1.0 + W_GRADE_GAIN * grade);
        self.ord = self.ord.wrapping_add(1);
        let seed = seed_of(to.0, to.1, self.ord);

        // D8 / §6.11: an Enter lands SMALL — no ring, no hero, no rain — and
        // so does a PTY line feed, which is program output under the same
        // row-change arm and whose sound (the cascade's brrrring) carries no
        // rain either. A NAV-licensed row change — history recall — is a
        // meteor in full: D8 reserves the ring for "same-row nav AND history
        // recall", §12.3 gives vertical recall the meteor gesture with its
        // five rain glints, and D6 pairs those glints 1:1 with the fan's gold
        // m1 + 4 m2 — a recall that landed small would leave the rain with no
        // stars to ride. §6.11's "Enter / vertical" heading is read as the
        // LICENCE, not the axis: the axis decides the rasterizer (D15), the
        // licence decides the landing.
        let small = matches!(licence, Licence::Return | Licence::Pty);

        let m = Meteor {
            x0,
            y0,
            x1,
            y1,
            t0: at,
            t_flight: flight(cells),
            w,
            grade,
            mom,
            retired_at: None,
            chained: false,
            // D4, at the LOCK as well as at the read: the caret's field `t`
            // is the classic walk, which is unbounded (1.861 at the end of a
            // 48-cell line), and it is latched RAW. `arc_t` folds it through
            // `tri` exactly once, at every read, so the station under the
            // caret is `tri(t_land)` — the very number the ribbon draws for
            // that cell. A `clamp01` here locked every long-line Ctrl-A to
            // violet beside an orange ribbon head.
            t_land: if ctx.caret_t.is_finite() {
                ctx.caret_t
            } else {
                0.0
            },
            cells,
            dir,
            landing: to,
            seed,
            shed: 0,
            landed: false,
            ring: !small,
            rain: !small,
        };

        // §6.9. A same-direction continuation from within a cell of a live
        // head CHAINS: the old flight HANDS ITS HEAD OVER. Its train is cut
        // at the head's position on this edge and finishes on the natural
        // post-arrival law from there (§6.8) — no nucleus, no more shedding,
        // no landing of its own — while the new flight carries the head on
        // from its own launch, so the eye reads one streak that kept going.
        // The new flight's clock, width, shed and fan are its OWN segment's:
        // inheriting the old launch would put the continuation's head at
        // `p₀` of a path it had already flown, i.e. behind where it was.
        //
        // Only a head IN THE AIR can be handed over. A train that has landed
        // — or whose arrival edge has passed unminted — has its head at rest
        // on its landing: chaining it would mark it headless and pop its
        // still-visible coma off the glass, and it would go on counting as
        // a head against the cap. Such a train is left to §6.9's corridor
        // rule below, which retires it with its afterglow decaying
        // (`Meteor::handed_over`). The arrival edge itself still chains: a
        // continuation on the very frame the head arrives is a hand-over at
        // the landing, and `on_event` runs before `emit` could mint it.
        let mut chained: Option<usize> = None;
        for (i, old) in self.live.iter_mut().enumerate() {
            if old.retired_at.is_some()
                || old.chained
                || old.landed
                || old.dir != dir
                || at > old.arrival()
            {
                continue;
            }
            let (hx, hy) = head_at(old, at);
            if (x0 - hx).hypot(y0 - hy) <= CHAIN_CELLS * cw {
                if at < old.arrival() {
                    // Mid-flight: the train ends where the head is NOW, and
                    // its arrival edge is this edge. The arc is fixed to the
                    // ROW and measured from the landing (`Meteor::arc`), so
                    // moving the landing to the head would shift every
                    // station's colour by the path the head never flew; the
                    // lock is moved by exactly that much, and every station
                    // keeps the colour it had — no hue pop on the chain
                    // frame.
                    let t_ms = (old.t_flight.as_secs_f32() * 1000.0).max(1.0);
                    let flown = enter_at_speed(ms_since(old.t0, at) / t_ms);
                    old.t_land -=
                        (1.0 - flown) * old.length() / cw * WALK_LAY_RATE * arc_gain(old.cells);
                    old.x1 = hx;
                    old.y1 = hy;
                    old.t_flight = at.saturating_duration_since(old.t0);
                }
                old.chained = true;
                chained = Some(i);
                break;
            }
        }

        // A new meteor whose corridor comes within 1 `ch` of a live one
        // RETIRES it. `at` is the edge, so the older train decays from exactly
        // the alpha it had — never a pop, never a fade-in-place. The train
        // that just chained is finishing on its own law and is not retired
        // by the continuation it fed.
        //
        // §6.9's last sentence is its own rule: "only one head is ever in the
        // air per row". Two same-row flights whose segments never come near
        // each other (reachable only through synthetic or PTY moves) still
        // share the row, so the older HEAD is retired if it is still flying;
        // a train already finishing its post-arrival fade has no head in the
        // air and is left to its own law.
        let corridor = CORRIDOR_CH * ch;
        let same_row =
            |old: &Meteor| m.y0 == m.y1 && old.y0 == old.y1 && m.y0 == old.y0 && at < old.arrival();
        for (i, old) in self.live.iter_mut().enumerate() {
            if Some(i) == chained || old.retired_at.is_some() {
                continue;
            }
            let near = seg_gap(
                (m.x0, m.y0),
                (m.x1, m.y1),
                (old.x0, old.y0),
                (old.x1, old.y1),
            ) <= corridor;
            if near || same_row(old) {
                old.retired_at = Some(at);
            }
        }

        // §18: the pool is fixed. A full pool drops the finishing train
        // nearest its own end before the push — at a human cadence that
        // train is already at the chroma cull (see `METEOR_POOL`).
        if self.live.len() >= METEOR_POOL {
            let victim = self
                .live
                .iter()
                .enumerate()
                .filter(|(_, x)| x.retired_at.is_some())
                .min_by_key(|(_, x)| x.end())
                .map_or(0, |(i, _)| i);
            self.live.remove(victim);
        }
        self.live.push(m);

        // Cap 2 (v1: 4 — "four rulers piling"). A third evicts a train
        // through the same 60 ms finish, so an eviction and a crossing look
        // identical on glass — and it evicts a HEADLESS train first: a
        // chained stub or a landed train finishing on §6.8 is one streak's
        // remnant, not a third meteor, and it must not cost a flight on
        // another row its head in the air. Only when every train on glass
        // has a head does the oldest head go. Counting every un-retired
        // train, stubs included, is what keeps the pool's arithmetic honest
        // (`METEOR_POOL` = heads + trains on the 60 ms finish): a stub left
        // to its 300 ms law under a mash would fill the pool with nothing a
        // full pool could drop without a pop.
        while self.live.iter().filter(|x| x.retired_at.is_none()).count() > FLIGHT_MAX_LIVE {
            let victim = self
                .live
                .iter()
                .position(|x| x.retired_at.is_none() && !x.in_flight_at(at))
                .or_else(|| self.live.iter().position(|x| x.retired_at.is_none()));
            match victim {
                Some(i) => self.live[i].retired_at = Some(at),
                None => break,
            }
        }

        Some(Spawn {
            t0: at,
            t_flight: m.t_flight,
            dir,
            cells: cells_n,
            grade,
            landing: to,
            ring: m.ring,
            rain: m.rain,
        })
    }
}

// ===========================================================================
// The per-frame flight solution (§6.2, §6.3, §6.8, §6.11)
// ===========================================================================

/// Everything about ONE meteor on ONE frame, solved once so no two layers can
/// read the flight at two different instants (the §17.1 rule that made the
/// spine one integrator applies just as hard here).
struct Flight {
    /// Path length `L`, px.
    l: f32,
    /// Unit vector along the path.
    unit: (f32, f32),
    /// The head's window-absolute position, px.
    head: (f32, f32),
    /// How far behind the head the train's ROOT is, px — the whole visible
    /// span. During flight this is the head's own displacement; after arrival
    /// the root retracts toward the landing on `suck-in` (§6.8).
    span: f32,
    /// How far the head still has to fly to the landing, px — `(1 − p)·L`,
    /// zero from the arrival edge on and frozen with the head of a retired
    /// train. The train's colours are measured from the LANDING
    /// ([`Meteor::arc`]), so a station `s` behind the head is
    /// `(to_go + s)/cw` cells short of it.
    to_go: f32,
    /// `u = t/T`, clamped.
    u: f32,
    /// Body thickness NOW, px — `w` thinning to `0.4 w` across the fade (§6.8).
    w: f32,
    /// The WHITE heat: `min(118·(1 + 0.5·grade)·intensity, 160)` (§6.3, L3)
    /// on dark; ungraded on light (§6.10).
    head_cov: f32,
    /// The colour train's head coverage, `118·intensity` — see
    /// [`COLOUR_TRAIN_COV`].
    colour_cov: f32,
    /// The white layer's alpha (§6.8's τ 90 × §6.9's retire).
    fade_w: f32,
    /// The colour layer's alpha (§6.8's τ 150 × §6.9's retire).
    fade_c: f32,
    /// The shoulder's DESIGN length behind the head, px (§6.5 layer 3) —
    /// `max(0.06·L, |Δ_frame| + 2 px)`. The light arm's rail chain holds
    /// full ink-bold alpha over it for the train's whole life.
    shoulder: f32,
    /// The white layer's shoulder NOW, px: [`Flight::shoulder`] during the
    /// flight, closing into the landing over [`WHITE_CLOSE_MS`] after it.
    white_shoulder: f32,
    /// The white layer's falloff length NOW, px: `0.12·L` during the flight,
    /// closing with the shoulder after it. Floored at 1 px so the exponential
    /// is always well-formed.
    white_fall: f32,
    /// True once the white layer has closed all the way into the landing
    /// (`t ≥ T + WHITE_CLOSE_MS`): nothing white is built on the train any
    /// more, whatever its alpha still says.
    white_closed: bool,
    /// The nucleus's velocity stretch, ×2.2 at launch → ×1.0 at rest.
    stretch: f32,
    /// The terminal flash's radius multiplier (§6.5 layer 8).
    flash: f32,
    /// Station stride, px, and the rasterizer's slab step (§18).
    stride: f32,
    /// Which rasterizer (D15).
    axis: Axis,
    /// True once the head has been handed over while in flight (§6.9,
    /// [`Meteor::handed_over`]) — the nucleus and the coma stop, because only
    /// one head is ever in the air per row. The two train layers do NOT stop
    /// here: they finish on the retire decay (or, for a chained train, on
    /// §6.8's own taus from the chain edge), because a white layer that
    /// vanished on the edge would be the pop §6.9 forbids. A head retired AT
    /// REST is not headless: its afterglow rides `fade_w` / `fade_c`, which
    /// already carry the retire decay, and leaves on the same 60 ms.
    headless: bool,
}

impl Flight {
    /// Solve one meteor on one frame, or `None` when there is nothing to draw.
    ///
    /// The `l < 1.0` guard is on the STATIC path length, not on the animated
    /// one — see this module's header for why that distinction is the whole of
    /// T2.
    fn of(m: &Meteor, ctx: &Ctx<'_>) -> Option<Self> {
        let l = m.length();
        if !l.is_finite() || l < 1.0 {
            return None;
        }
        let (dx, dy) = m.delta();
        let unit = (dx / l, dy / l);
        let t_ms = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
        let age = ms_since(m.t0, ctx.now);
        let after = (age - t_ms).max(0.0);
        let reduced = ctx.cfg.reduced_motion;

        // §6.11: reduced motion is STATIC — the head is at the landing from
        // the first frame, there is no flight, and the theme's ONE linear fade
        // (the single exception in §2.5's vocabulary) takes it off at the end
        // of the normal life.
        let (u, alpha_w, alpha_c) = if reduced {
            let life = t_ms + FLIGHT_OFF_GLASS_MS;
            let a = REDUCED_MOTION_ALPHA * clamp01((life - age) / REDUCED_MOTION_FADE_MS.max(1.0));
            (1.0, a, a)
        } else {
            (
                clamp01(age / t_ms),
                (-after / TRAIN_WHITE_TAU_MS).exp(),
                (-after / TRAIN_COLOUR_TAU_MS).exp(),
            )
        };
        let retire = m.retire_alpha(ctx.now);
        // Where the head is and where the root is, as shares of the path.
        //
        // Live: the head flies `enter-at-speed`; after arrival the root
        // retracts from the launch toward the landing over `R = 300` (§6.8).
        //
        // Retired (§6.9): the head FROZE on the retire edge, and the train
        // "jumps to its post-arrival fade with R = 60" — the root leaves
        // wherever it was on that edge (the launch, or partway through its
        // 300 ms retract) and retracts toward the frozen head on `suck-in`,
        // reaching it on the same 60 ms the alpha reaches the chroma cull.
        // A train that only dimmed where it lay would be the fade-in-place
        // §6.9 forbids; one that kept lengthening behind a head no longer on
        // glass would be light with no gesture behind it.
        let (u, p, root) = if reduced {
            (1.0, 1.0, 0.0)
        } else if let Some(at) = m.retired_at {
            let age_at = ms_since(m.t0, at);
            let u_at = clamp01(age_at / t_ms);
            let p_at = enter_at_speed(u_at);
            let after_at = (age_at - t_ms).max(0.0);
            let root_at = if after_at <= 0.0 {
                0.0
            } else {
                suck_in(after_at / ROOT_SUCK_MS)
            };
            let r = suck_in(ms_since(at, ctx.now) / FLIGHT_RETIRE_MS);
            (u_at, p_at, root_at + (p_at - root_at) * r)
        } else {
            let root = if after <= 0.0 {
                0.0
            } else {
                suck_in(after / ROOT_SUCK_MS)
            };
            (u, enter_at_speed(u), root)
        };
        let span = ((p - root) * l).max(0.0);
        if span < 1.0 {
            return None;
        }
        let to_go = ((1.0 - p) * l).max(0.0);

        let head = (m.x0 + dx * p, m.y0 + dy * p);
        let thin = 1.0 - (1.0 - THICKNESS_END_SHARE) * clamp01(after / ROOT_SUCK_MS);
        let cw = (ctx.geom.cw as f32).max(1.0);

        // §6.5 layer 3 / T7: the shoulder is the design length OR the head's
        // travel across the slowest effect frame, whichever is longer — and it
        // is that CONTINUITY FLOOR that the 1..4-cell clamp binds.
        let travel = head_speed(l, t_ms, u) * SHOULDER_FRAME_MS + 2.0;
        let shoulder = (SHOULDER_SHARE * l)
            .max(travel.clamp(SHOULDER_MIN_CELLS * cw, SHOULDER_MAX_CELLS * cw));

        // §6.8 on glass: after the arrival edge the white heat CLOSES INTO THE
        // CARET on the pin's own clock — its shoulder and its falloff length
        // retract to the landing on `suck-in` (see `WHITE_CLOSE_MS`). Its
        // alpha still runs τ 90; this is where the white IS, not how bright.
        // Static under reduced motion, like every other motion of the theme.
        let close = if reduced {
            0.0
        } else {
            suck_in(after / WHITE_CLOSE_MS)
        };
        let open = 1.0 - close;
        let white_shoulder = shoulder * open;
        let white_fall = (WHITE_FALLOFF_SHARE * l * open).max(1.0);

        // §6.3's brightness grade, on the dark arm only: §6.10 spends the
        // whole distance budget on length and speed where the ink is
        // legibility-bounded.
        let bright = if ctx.cfg.dark_theme {
            1.0 + BRIGHT_GRADE_GAIN * m.grade
        } else {
            1.0
        };
        let intensity = clamp01(ctx.cfg.intensity);
        let head_cov = (HEAD_COV_BASE * bright * intensity).min(JUMP_COV_CEIL);
        let colour_cov = COLOUR_TRAIN_COV * intensity;

        Some(Self {
            l,
            unit,
            head,
            span,
            to_go,
            u,
            w: m.w * thin,
            head_cov,
            colour_cov,
            fade_w: alpha_w * retire,
            fade_c: alpha_c * retire,
            shoulder,
            white_shoulder,
            white_fall,
            white_closed: close >= 1.0,
            stretch: 1.0 + NUCLEUS_STRETCH_GAIN * (1.0 - u).powf(FLIGHT_ENTER_EXP - 1.0),
            flash: if reduced { 1.0 } else { flash_scale(after) },
            stride: (l / STATIONS_MAX as f32).max(STATION_STRIDE_MIN_PX),
            axis: m.axis(),
            headless: m.handed_over(),
        })
    }

    /// The station stride as the rasterizer's own slab step (§18).
    fn step(&self) -> usize {
        (self.stride.round().max(1.0) as usize).max(1)
    }

    /// How many stations this span earns, `≤ STATIONS_MAX` (§18).
    fn stations(&self, span: f32) -> usize {
        (((span / self.stride).ceil() as usize) + 1).clamp(2, STATIONS_MAX)
    }

    /// How far along the path the head has travelled, as a share of `L` — the
    /// quantity a shed station is compared against (§6.5 layer 5).
    fn head_travel_share(&self) -> f32 {
        clamp01(enter_at_speed(self.u))
    }

    /// §6.5 layer 1's `w(s) = w·(0.30 + 0.70·exp(−s/(0.5·L)))`.
    fn width_at(&self, s: f32) -> f32 {
        self.w
            * (WIDTH_FLOOR_SHARE + WIDTH_SPAN_SHARE * (-s / (WIDTH_FALLOFF_SHARE * self.l)).exp())
    }
}

// ===========================================================================
// §6.5 layers 1-3 — the two train layers and the shoulder
// ===========================================================================

/// Which of the two train layers is being built (§6.5 layers 1-3). They share
/// one station walk and differ in exactly three things: the stream they land
/// in, the falloff length, and whether the colour comes from the arc or is
/// simply white.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// Layer 1 — `spectrum(t_m(s))` into `under`, BELOW ink, never blooms.
    /// "Where I came from" reads beneath the letters without touching them.
    Colour,
    /// Layers 2 and 3 — `#FFFFFF` into `out`, above ink, blooms on GPU. This
    /// is the ion emission, and it dies fast behind the shoulder.
    White,
}

/// Emit §6.5's layers 1, 2 and 3.
fn draw_train(
    verts: &mut Vec<RibbonVertex>,
    m: &Meteor,
    f: &Flight,
    ctx: &Ctx<'_>,
    frame: &mut Frame<'_>,
) {
    let clip = ctx.geom.beam_clip();
    let step = f.step();
    let light = !ctx.cfg.dark_theme;

    build_stations(verts, m, f, ctx, Layer::Colour);
    let cap = frame.under.len() + UNDER_QUAD_CAP;
    let blend = if light {
        GlowBlend::Over
    } else {
        GlowBlend::Add
    };
    rasterize(frame.under, clip, verts, f.axis, step, cap, blend);

    // §6.10: the light arm drops the white layer and the shoulder entirely —
    // there is no such thing as "whiter than the page". Their job (the head
    // reading hotter than the trail) is done there by the rail chain's own
    // ink-bold alpha over the shoulder span, inside `build_stations`.
    if light {
        return;
    }
    build_stations(verts, m, f, ctx, Layer::White);
    let cap = frame.out.len() + WHITE_QUAD_CAP;
    rasterize(frame.out, clip, verts, f.axis, step, cap, GlowBlend::Add);
}

/// D15's axis rule, spent. Neither rasterizer is a fallback for the other:
/// each is degenerate on the axis its twin owns, and a producer that guesses
/// wrong draws NOTHING rather than drawing badly.
fn rasterize(
    out: &mut Vec<GlowQuad>,
    clip: BeamClip,
    verts: &[RibbonVertex],
    axis: Axis,
    step: usize,
    cap: usize,
    blend: GlowBlend,
) {
    match axis {
        Axis::Horizontal => ribbon_beam(out, clip, verts, 1.0, step, cap, blend),
        Axis::Vertical => ribbon_beam_v(out, clip, verts, 1.0, step, cap, blend),
    };
}

/// Walk the stations HEAD FIRST — which is what makes a saturated budget shed
/// the TAIL (`ribbon_beam` returns early having emitted what it could) rather
/// than punch a hole in the middle, and what makes [`UNDER_QUAD_CAP`] a
/// shedding of stations rather than a starving of the ribbon (§6.5, §18).
fn build_stations(
    verts: &mut Vec<RibbonVertex>,
    m: &Meteor,
    f: &Flight,
    ctx: &Ctx<'_>,
    layer: Layer,
) {
    verts.clear();
    let light = !ctx.cfg.dark_theme;
    let cw = (ctx.geom.cw as f32).max(1.0);
    let ch = ctx.geom.ch as f32;

    // The white layer stops where its own exponential has nothing left to say
    // (§18: no dead work) — and is not built at all once it has closed into
    // the caret (`WHITE_CLOSE_MS`). The colour layer runs the whole visible
    // span.
    let span = match layer {
        Layer::Colour => f.span,
        Layer::White => {
            if f.white_closed {
                return;
            }
            f.span
                .min(f.white_shoulder + f.white_fall * WHITE_TAIL_EFOLDS)
        }
    };
    let n = f.stations(span);
    let alpha = match layer {
        Layer::Colour => f.fade_c,
        Layer::White => f.fade_w,
    };
    // C3 / §6.2: FADE MOVES ALPHA, NEVER CHROMA — a coloured pixel below the
    // chroma cull is dropped rather than allowed to drift toward grey. This is
    // what puts the white layer off glass at `T + 191` and the colour layer at
    // `T + 466`, from the two taus alone and with no second schedule.
    if alpha < CHROMA_CULL_ALPHA {
        return;
    }
    // §6.10's rail: the light train leaves the body line and lies in the
    // inter-line leading, where nothing it darkens is ever a glyph. A VERTICAL
    // flight crosses rows, so it has no one leading to lie in and stays on its
    // own path — the rail is a same-row idea, and offsetting a vertical train
    // by 0.62 `ch` would only move it onto the next row's glyphs.
    let rail = if light && f.axis == Axis::Horizontal {
        LIGHT_RAIL_CH * ch
    } else {
        0.0
    };

    for k in 0..n {
        let s = span * (k as f32) / ((n - 1) as f32);
        let px = f.head.0 - f.unit.0 * s;
        let py = f.head.1 - f.unit.1 * s + rail;

        let (color, cov, half) = match layer {
            Layer::Colour => {
                // The station's colour is its place on the ROW, not its
                // distance from the head (`Meteor::arc`): `to_go + s` cells
                // short of the landing.
                let stop = spectrum(m.arc((f.to_go + s) / cw));
                let mut half = f.width_at(s) * 0.5;
                if light {
                    // §6.10 / L6: the light twin buys AREA, never brightness
                    // — the rail is squashed, and the coverage becomes an
                    // ink opacity `min(head_cov·2.1, cap)`: the leading's
                    // 236 on the rail, where nothing it darkens is a glyph;
                    // the over-text 190 for a vertical train that crosses
                    // rows on its own path. The dropped white layer's job —
                    // the head reading hotter than the trail — is done by
                    // the shoulder at FULL ink-bold alpha, joined by the
                    // exponential re-based at its end (no step, as the dark
                    // white layer joins its own).
                    half *= LIGHT_RAIL_SQUASH;
                    let cap = if rail > 0.0 {
                        LIGHT_RAIL_ALPHA_CAP
                    } else {
                        LIGHT_ALPHA_CAP
                    };
                    let peak = (f.colour_cov * LIGHT_INK_GAIN).min(cap);
                    let ink = if s <= f.shoulder {
                        peak
                    } else {
                        peak * (-(s - f.shoulder) / (COLOUR_FALLOFF_SHARE * f.l)).exp()
                    };
                    (light_ink(stop), ink * alpha, half)
                } else {
                    // §6.5 layer 1 / §3.4 / §6.6: `118·exp(−s/(0.35·L))` —
                    // the transient ceiling at the head, so the white heat
                    // wins over it wherever the two share a cell, and the
                    // e⁻¹ law between the shoulder and 0.61·L holds at every
                    // grade (see `COLOUR_TRAIN_COV`).
                    let c = f.colour_cov * (-s / (COLOUR_FALLOFF_SHARE * f.l)).exp();
                    (stop, c * alpha, half)
                }
            }
            Layer::White => {
                // The shoulder is flat at `head_cov` and the exponential is
                // RE-BASED at its end, so the two join with no step: §6.5
                // layer 3's "flat 1.0, then joins layer 2's exponential".
                // Both lengths are the white layer's OWN — the design values
                // in flight, closing into the landing after it (§6.8 on
                // glass, `WHITE_CLOSE_MS`).
                let tail = (s - f.white_shoulder).max(0.0);
                let c = f.head_cov * (-tail / f.white_fall).exp();
                let blend = smoothstep01((s - f.white_shoulder) / f.white_shoulder.max(1.0));
                let share =
                    SHOULDER_WIDTH_SHARE + (WHITE_WIDTH_SHARE - SHOULDER_WIDTH_SHARE) * blend;
                (0x00FF_FFFF, c * alpha, f.width_at(s) * 0.5 * share)
            }
        };
        if cov < 1.0 {
            continue;
        }
        let half = half.max(0.5);
        // `ribbon_beam`'s vertex fields are MAJOR/MINOR, not x/y: on the
        // vertical twin `x` is the sample's window Y and `spine` its window X
        // (D15). Transposing the polyline is the whole of the port.
        let (major, spine) = match f.axis {
            Axis::Horizontal => (px, py),
            Axis::Vertical => (py, px),
        };
        verts.push(RibbonVertex {
            x: major,
            spine,
            up: half,
            dn: half,
            core_up: half * TRAIN_CORE_SHARE,
            core_dn: half * TRAIN_CORE_SHARE,
            color,
            cov,
            lift: 0.0,
            lift_span: 0.0,
        });
    }
}

// ===========================================================================
// §6.5 layers 6-8 — the coma, the nucleus and the terminal flash
// ===========================================================================

/// Emit the coma, the nucleus and the terminal flash's radius pop.
///
/// The nucleus is the top of §3.2's luminance hierarchy — the brightest pixel
/// in the frame is the SMALLEST thing, and it leads the train by
/// [`NUCLEUS_LEAD_PX`]. On a diagonal only the DOMINANT axis is stretched:
/// `RainHalo` is an axis-aligned ellipse with no rotation, and a stretch on
/// the wrong axis would read as a smear rather than as speed.
fn draw_head(m: &Meteor, f: &Flight, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    if f.headless {
        return;
    }
    let geom = ctx.geom;
    let light = !ctx.cfg.dark_theme;
    let r_c = (NUCLEUS_MIN_PX).max(NUCLEUS_W_SHARE * f.w);
    let squash = if light { LIGHT_SQUASH } else { 1.0 };
    // The stretch is along the path; the flash (§6.5 layer 8: "NUCLEUS radius
    // ×(1.0 → 1.6)") pops the nucleus and only the nucleus — the coma is the
    // steady blended glow the flash happens inside of.
    let (rx, ry) = match f.axis {
        Axis::Horizontal => (r_c * f.stretch, r_c * squash),
        Axis::Vertical => (r_c * squash, r_c * f.stretch),
    };
    let (nx, ny) = (rx * f.flash, ry * f.flash);
    let cx = f.head.0 + f.unit.0 * NUCLEUS_LEAD_PX;
    let cy = f.head.1 + f.unit.1 * NUCLEUS_LEAD_PX;

    // §3.2, D3: "nothing coloured is drawn below α 0.12; a white core below
    // α 0.20 is culled, never left as a grey speck". The two halos are the
    // two cases, and each is culled on its own floor rather than allowed to
    // linger as a 3/255 dot until the pool's end.
    let coma_on = f.fade_c >= CHROMA_CULL_ALPHA;
    let core_on = f.fade_w >= STAR_CULL_ALPHA;

    // Layer 6 — the corona: seven petals of the seven named stops orbiting
    // the nucleus (`COMA_PETALS`), turning once per `COMA_SPIN_MS` — static
    // under reduced motion, like every other motion of the theme. Halos,
    // not marks (§3.1), and the flash pops the nucleus only: the petals ride
    // the head's stretch and nothing else.
    let coma_cov = (COMA_COV_SHARE * f.head_cov * f.fade_c).clamp(0.0, 255.0);
    let core_cov = (f.head_cov * f.fade_w).clamp(0.0, 255.0);
    let spin = if ctx.cfg.reduced_motion {
        0.0
    } else {
        std::f32::consts::TAU * ms_since(m.t0, ctx.now) / COMA_SPIN_MS
    };
    let (orbit_x, orbit_y) = (
        COMA_R_SCALE * COMA_PETAL_ORBIT * rx,
        COMA_R_SCALE * COMA_PETAL_ORBIT * ry,
    );
    let (petal_x, petal_y) = (
        COMA_R_SCALE * COMA_PETAL_R * rx,
        COMA_R_SCALE * COMA_PETAL_R * ry,
    );
    let petal_at = |i: usize| {
        let a = spin + std::f32::consts::TAU * (i as f32) / (COMA_PETALS as f32);
        (cx + orbit_x * a.cos(), cy + orbit_y * a.sin())
    };

    if light {
        if coma_on {
            let ink = (coma_cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            for i in 0..COMA_PETALS {
                push_halo_veil(
                    frame.halos,
                    geom,
                    petal_at(i),
                    (petal_x, petal_y),
                    light_ink(spectrum_stop(i)),
                    ink,
                );
            }
        }
        if core_on {
            push_halo_veil(
                frame.halos,
                geom,
                (cx, cy),
                (nx, ny),
                ctx.cfg.theme_fg & 0x00FF_FFFF,
                (core_cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8,
            );
        }
        return;
    }
    if coma_on {
        for i in 0..COMA_PETALS {
            let (px, py) = petal_at(i);
            push_halo_add(
                frame.halos,
                geom,
                px,
                py,
                petal_x,
                petal_y,
                premul_rgb(spectrum_stop(i), coma_cov as u8),
            );
        }
    }
    if core_on {
        push_halo_add(
            frame.halos,
            geom,
            cx,
            cy,
            nx,
            ny,
            premul_rgb(0x00FF_FFFF, core_cov as u8),
        );
    }
}

/// §6.5 layer 8 — the terminal flash, as AREA and never as coverage (the cap
/// is a cap).
///
/// The nucleus radius eases OUT to [`FLASH_OVERSHOOT`] over [`FLASH_MS`],
/// passing through its nominal [`FLASH_PEAK`] on the way, then relaxes to ×1.0
/// by [`FLASH_RELAX_MS`]. The rise borrows the theme's own deceleration
/// exponent ([`FLIGHT_ENTER_EXP`], `enter-at-speed`'s 2.2) rather than
/// inventing an eighth easing: §2.5's vocabulary is closed.
#[must_use]
fn flash_scale(after_ms: f32) -> f32 {
    if after_ms <= 0.0 {
        return 1.0;
    }
    let over = FLASH_OVERSHOOT - 1.0;
    if after_ms <= FLASH_MS {
        let u = clamp01(after_ms / FLASH_MS);
        return 1.0 + over * (1.0 - (1.0 - u).powf(FLIGHT_ENTER_EXP));
    }
    let v = clamp01((after_ms - FLASH_MS) / (FLASH_RELAX_MS - FLASH_MS));
    1.0 + over * (1.0 - smoothstep01(v))
}

// ===========================================================================
// §6.5 layers 9 and 10 — the pin and the ring
// ===========================================================================

/// §6.5 layer 9 — the landing PIN.
///
/// Explicit arms plus a 2×2 nucleus, and deliberately NOT `push_twinkle_star`:
/// at this arm length `star_waist_px` would give a 4-px waist, and the pin's
/// waist is exactly [`PIN_WAIST_PX`]. The arms whip inward; the last pixel on
/// glass is the nucleus, which *is* the caret.
fn draw_pin(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    // §6.11: the pin exists only as motion (its arms whip into the caret),
    // and under reduced motion nothing moves.
    if ctx.cfg.reduced_motion {
        return;
    }
    let u = clamp01(ms_since(l.pin.at, ctx.now) / PIN_MS);
    let ch = ctx.geom.ch as f32;
    let close = smoothstep01((u - PIN_ARM_HOLD_U) / (1.0 - PIN_ARM_HOLD_U));
    let arm = PIN_ARM_CH * ch * (1.0 - close).powf(PIN_ARM_EXP);
    let alpha = if u <= PIN_ALPHA_HOLD_U {
        1.0
    } else {
        1.0 - smoothstep01((u - PIN_ALPHA_HOLD_U) / (1.0 - PIN_ALPHA_HOLD_U))
    };
    let cov = TRANSIENT_STAR_COV_CEIL * alpha * clamp01(ctx.cfg.intensity);
    if cov < 1.0 {
        return;
    }
    let light = !ctx.cfg.dark_theme;
    let arms_rgb = spectrum_snap(l.pin.t);
    let (cx, cy) = (l.x.round() as i32, l.y.round() as i32);
    let a = arm.round() as i32;

    // The 2×2 nucleus sits on `(cx − 1 ..= cx, cy − 1 ..= cy)`; the four
    // 1-px arms run OUTWARD FROM ITS EDGES and never through it. D1: "no
    // class draws a duplicate core rect" — the caret cell is ledger-exempt,
    // so two arms crossing under the nucleus would request 3 × 118 at the
    // one pixel the ledger cannot hold to 118, and the pin would read as a
    // blob rather than a cross with a heart. Each arm is `a − 1` long past
    // the nucleus, so the mark's full extent is `2·a` — centred on the
    // nucleus's own centre, half a pixel off `(cx, cy)` as any 2×2 is.
    let reach = a - 1;
    let arms = [
        (cx - a, cy, reach, PIN_WAIST_PX),
        (cx + 1, cy, reach, PIN_WAIST_PX),
        (cx, cy - a, PIN_WAIST_PX, reach),
        (cx, cy + 1, PIN_WAIST_PX, reach),
    ];
    let nuc = (
        cx - PIN_NUCLEUS_PX / 2,
        cy - PIN_NUCLEUS_PX / 2,
        PIN_NUCLEUS_PX,
        PIN_NUCLEUS_PX,
    );
    if light {
        let ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
        if reach > 0 {
            for b in arms {
                push_ink_rect(frame.out, ctx.geom, b, light_ink(arms_rgb), ink);
            }
        }
        push_ink_rect(
            frame.out,
            ctx.geom,
            nuc,
            ctx.cfg.theme_fg & 0x00FF_FFFF,
            ink,
        );
        return;
    }
    let arm_premul = premul_rgb(arms_rgb, cov as u8);
    // The arms whip all the way in before the nucleus goes: past `u = 1` the
    // pin IS the caret, and a zero-length arm is not drawn at all.
    if reach > 0 {
        for (x, y, w, h) in arms {
            push_fx_rect(frame.out, ctx.geom, x, y, w, h, arm_premul);
        }
    }
    push_fx_rect(
        frame.out,
        ctx.geom,
        nuc.0,
        nuc.1,
        nuc.2,
        nuc.3,
        premul_rgb(0x00FF_FFFF, cov as u8),
    );
}

/// The shockwave's colour at `share` of the way around it, at `u` of its
/// life: the spectrum walked [`RING_SWEEPS`] times around through `tri` (so
/// the ring has no seam) and turned [`RING_SPIN_TURNS`] over the life. A
/// linear mark samples `spectrum` (C1).
#[inline]
#[must_use]
pub fn ring_colour(share: f32, u: f32) -> u32 {
    spectrum(tri(RING_SWEEPS * (share + RING_SPIN_TURNS * u)))
}

/// Per-channel lerp of two `0x00RRGGBB` colours, `t` in `0..=1` — the one
/// place this file mixes colours (a spark's tinted core).
#[inline]
#[must_use]
fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = clamp01(t);
    let ch = |sh: u32| -> u32 {
        let (x, y) = (((a >> sh) & 0xFF) as f32, ((b >> sh) & 0xFF) as f32);
        (x + (y - x) * t + 0.5) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// **THE WHITE-HOT FLASH** (second round, 2026-09-08). On the arrival edge
/// the caret cell and its two neighbours ([`FLASH_CELLS`]) burst white, and
/// the white DIES INTO THE RAINBOW: two layers over each cell, a `#FFFFFF`
/// one whose coverage leaves on `1 − smoothstep(t/80)` and a coloured one
/// whose coverage arrives on the same curve, peaks where the white is gone
/// and spends on `spend` over the next 80 ms ([`FLASH_BURST_MS`],
/// [`FLASH_COLOUR_MS`]) — white → spectrum, one way. The colour is the
/// landing's own stop on the caret cell and the next stops outward (the
/// fan's ROYGBIV walk), so the flash hands the eye straight to the pin, the
/// fan and the shockwave in one spectrum.
///
/// Over a cell the sky's probe proved BLANK ([`SkyMask::flash_blank`]) the
/// burst asks [`FLASH_FULL_COV`]; over a glyph cell, or before the probe has
/// been asked, the transient cap — the text under it stays legible (L3). The
/// caret cell is ledger-exempt and is white anyway on the flare frame.
///
/// Light: there is no whiter than the page, so the white layer is dropped
/// and the colour layer is source-over ink at the over-text cap (§6.10).
/// Reduced motion: the static form — white and colour at half each, held,
/// then the theme's one linear fade (§6.11).
fn draw_flash(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    if !l.splash {
        return;
    }
    let age = ms_since(l.pin.at, ctx.now);
    let reduced = ctx.cfg.reduced_motion;
    let (white_share, colour_share) = if reduced {
        let a = l.static_alpha(ctx.now);
        (
            FLASH_STATIC_WHITE_SHARE * a,
            (1.0 - FLASH_STATIC_WHITE_SHARE) * a,
        )
    } else {
        if age >= FLASH_COLOUR_MS {
            return;
        }
        let turn = smoothstep01(age / FLASH_BURST_MS);
        let spent = spend(clamp01(
            (age - FLASH_BURST_MS) / (FLASH_COLOUR_MS - FLASH_BURST_MS),
        ));
        (1.0 - turn, turn * spent)
    };
    let intensity = clamp01(ctx.cfg.intensity);
    let light = !ctx.cfg.dark_theme;
    let geom = ctx.geom;
    let (cw, ch) = (geom.cw as i32, geom.ch as i32);
    let stop0 = spectrum_snap_index(l.pin.t);
    // The landing cell's own box, from the caret's centre.
    let x0 = (l.x - geom.cw as f32 * 0.5).round() as i32;
    let y0 = (l.y - geom.ch as f32 * 0.5).round() as i32;
    for k in -FLASH_CELLS..=FLASH_CELLS {
        let blank = l.sky.is_some_and(|s| s.flash_blank(k));
        let cap = if blank {
            FLASH_FULL_COV
        } else {
            TRANSIENT_STAR_COV_CEIL
        };
        let x = x0 + k * cw;
        let stop = spectrum_stop((stop0 + k.unsigned_abs() as usize) % SPECTRUM_STOPS);
        let colour_cov = cap * colour_share * intensity;
        if light {
            let ink = (colour_cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            push_ink_rect(frame.out, geom, (x, y0, cw, ch), light_ink(stop), ink);
            continue;
        }
        let white_cov = cap * white_share * intensity;
        if white_cov >= 1.0 {
            push_fx_rect(
                frame.out,
                geom,
                x,
                y0,
                cw,
                ch,
                premul_rgb(0x00FF_FFFF, white_cov as u8),
            );
        }
        if colour_cov >= 1.0 {
            push_fx_rect(
                frame.out,
                geom,
                x,
                y0,
                cw,
                ch,
                premul_rgb(stop, colour_cov as u8),
            );
        }
    }
}

/// §6.5 layer 10 — the landing RING, since 2026-09-08 **THE SHOCKWAVE**.
///
/// Hollow, expanding past the fan and the splash to [`RING_R_CH`] over
/// [`RING_MS`], its circumference carrying the spectrum ([`ring_colour`]).
/// 72 segments where §6.5 wrote 48 and v1 used 32: the notch
/// `the_ring_has_no_notch_on_the_steep_quadrants` measures is what too few
/// leave on the steep quadrants, where an ellipse's arc-length per angle
/// step is longest.
fn draw_ring(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    let Some(ring) = l.ring else {
        return;
    };
    let ch = ctx.geom.ch as f32;
    // The landing's GRADE: the full radius grows ALONG the line with the
    // jump's impact ([`ring_full_radius`]); the vertical semi-axis is capped
    // at the floor ring's own ([`RING_RISE_MAX_CH`]), so a big ring flattens
    // into a wide ellipse under the row exactly as the fan flattens under
    // its rise cap (one law, one squash), and never rises above where the
    // fan may.
    let r_full = ring_full_radius(ring.scale, ch);
    // §6.11: under reduced motion the shockwave is STATIC at its full reach
    // — the colours without the expansion — and leaves on the linear fade.
    let (u, r, alpha) = if ctx.cfg.reduced_motion {
        (0.0, r_full, l.static_alpha(ctx.now))
    } else {
        let u = clamp01(ms_since(ring.at, ctx.now) / ring.ms);
        // Hold-then-fall (`RING_COV_SHARE`): full while the ring is small,
        // spent on `spend` over the rest of its life as it expands past the
        // fan.
        let u_spend = clamp01((u - RING_COV_HOLD_U) / (1.0 - RING_COV_HOLD_U));
        (
            u,
            r_full * (1.0 - (1.0 - u).powi(RING_R_EXP)),
            spend(u_spend),
        )
    };
    let (rx, ry) = (r, (r * RING_SQUASH).min(RING_RISE_MAX_CH * ch));
    let cov = RING_COV_SHARE * TRANSIENT_STAR_COV_CEIL * alpha * clamp01(ctx.cfg.intensity);
    if cov < 1.0 || r < 1.0 {
        return;
    }
    let light = !ctx.cfg.dark_theme;
    let step = std::f32::consts::TAU / RING_SEGMENTS as f32;

    // §6.10: the light ring is source-over dots, not a stroked polyline —
    // `comet_beam` emits additive light, and additive light is invisible on a
    // page (you cannot brighten white).
    if light {
        let ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
        let dot = (r * RING_THICK_SHARE).clamp(RING_THICK_MIN_PX, RING_THICK_MAX_CH * ch);
        for k in 0..RING_LIGHT_DOTS {
            let share = (k as f32) / (RING_LIGHT_DOTS as f32);
            let a = std::f32::consts::TAU * share;
            let c = (l.x + rx * a.cos(), l.y + ry * a.sin());
            let rgb = light_ink(ring_colour(share, u));
            push_halo_veil(frame.halos, ctx.geom, c, (dot, dot * RING_SQUASH), rgb, ink);
        }
        return;
    }

    // The unit circle walked by ONE rotation per vertex (a complex multiply
    // by `e^(i·step)`) — no trigonometry on the frame path beyond the step
    // itself; over 72 steps the drift is under 1e-4 px, and the last vertex
    // is pinned to the first so the ring closes exactly.
    frame.beams.clear();
    let (cos_step, sin_step) = (step.cos(), step.sin());
    let (mut c, mut s) = (1.0_f32, 0.0_f32);
    for k in 0..=RING_SEGMENTS {
        let (ux, uy) = if k == RING_SEGMENTS {
            (1.0, 0.0)
        } else {
            (c, s)
        };
        frame.beams.push(BeamVertex {
            x: l.x + rx * ux,
            y: l.y + ry * uy,
            color: ring_colour((k % RING_SEGMENTS) as f32 / RING_SEGMENTS as f32, u),
            cov: cov as u8,
        });
        (c, s) = (c * cos_step - s * sin_step, s * cos_step + c * sin_step);
    }
    let thick = (r * RING_THICK_SHARE).clamp(RING_THICK_MIN_PX, RING_THICK_MAX_CH * ch);
    let step = if thick >= RING_STEP4_THICK_PX {
        4
    } else if thick >= RING_STEP3_THICK_PX {
        3
    } else if thick >= RING_STEP2_THICK_PX {
        2
    } else {
        1
    };
    comet_beam(
        frame.out,
        ctx.geom.beam_clip(),
        frame.beams,
        thick,
        step,
        0.0,
    );
}

/// **THE SPLASH — TWO RAINBOW BANDS IN THE SKY** (second round, 2026-09-08).
/// [`SPLASH_CELLS`] each side of the caret, a full sweep of the spectrum
/// outward from the landing's own stop, mirrored, sliding outward over the
/// life ([`SPLASH_DRIFT_T`]), at the transient ceiling and feathered to
/// nothing past [`SPLASH_FEATHER_SHARE`] of its reach. Its reach bursts out
/// on the ring's quartic and its coverage is held then spent.
///
/// WHERE: one band in the sky band above the landing row — the aurora's
/// band, `[top − gap − SPLASH_BAND_CH·ch, top − gap)` with `gap` the tall
/// ribbon's rise above the cell (`TALL_UP_CH − 1`) or nothing under
/// `underline` — and its mirror below the row, about the row's centre. Both
/// lie STRICTLY OUTSIDE the landing row's own cell rows, so no pixel of the
/// splash is ever on the text it celebrates; and each station is gated on
/// the sky's probe ([`SkyMask::sky_blank`]) so a band is never laid over a
/// neighbour-row cell that carries a glyph or that the probe has not seen.
/// The bold round's under-ink splash on the row itself is gone: the owner
/// asked for the impact out from under the letters.
///
/// Four head-first polylines (two sides of two bands), each under a quarter
/// of [`SPLASH_QUAD_CAP`], so a saturated budget sheds the far ends and
/// never the heart; a station the probe refuses breaks the polyline there.
/// `out`: above ink, and it blooms. Light: source-over ink at the leading's
/// cap — nothing under a band is a glyph, by construction (§6.10). Reduced
/// motion: static at full reach, on the linear fade (§6.11).
fn draw_splash(verts: &mut Vec<RibbonVertex>, l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
    if !l.splash {
        return;
    }
    let Some(sky) = l.sky else {
        return;
    };
    let reduced = ctx.cfg.reduced_motion;
    let (u, alpha) = if reduced {
        (1.0, l.static_alpha(ctx.now))
    } else {
        let u = clamp01(ms_since(l.pin.at, ctx.now) / SPLASH_MS);
        let u_spend = clamp01((u - SPLASH_HOLD_U) / (1.0 - SPLASH_HOLD_U));
        (u, spend(u_spend))
    };
    let (cw, ch) = ((ctx.geom.cw as f32).max(1.0), ctx.geom.ch as f32);
    let reach = SPLASH_CELLS * cw * (1.0 - (1.0 - u).powi(RING_R_EXP));
    let peak = TRANSIENT_STAR_COV_CEIL * alpha * clamp01(ctx.cfg.intensity);
    if peak < 1.0 || reach < 1.0 {
        return;
    }
    let light = !ctx.cfg.dark_theme;
    let (blend, gain, cap_ink) = if light {
        (GlowBlend::Over, LIGHT_INK_GAIN, LIGHT_RAIL_ALPHA_CAP)
    } else {
        (GlowBlend::Add, 1.0, 255.0)
    };
    let drift = if reduced { 0.0 } else { SPLASH_DRIFT_T * u };
    let gap = if ctx.cfg.ribbon_tall {
        (TALL_UP_CH - 1.0) * ch
    } else {
        0.0
    };
    let half = SPLASH_BAND_CH * ch * 0.5;
    let n = SPLASH_CELLS as usize * SPLASH_STATIONS_PER_CELL + 1;
    let step = ((cw * SPLASH_STEP_CW_SHARE).round() as usize).max(2);
    let clip = ctx.geom.beam_clip();
    let cap_quarter = SPLASH_QUAD_CAP / 4;
    for side_y in [-1_i32, 1] {
        let spine = l.y + side_y as f32 * (ch * 0.5 + gap + half);
        for side_x in [-1.0_f32, 1.0] {
            verts.clear();
            let cap = frame.out.len() + cap_quarter;
            for k in 0..n {
                let share = (k as f32) / ((n - 1) as f32);
                let d = reach * share;
                // The cell this station is over, columns from the caret:
                // a station the probe has not proved blank breaks the band.
                let cell = ((side_x * d + cw * 0.5) / cw).floor() as i32;
                let cov = peak
                    * (1.0
                        - smoothstep01(
                            (share - SPLASH_FEATHER_SHARE) / (1.0 - SPLASH_FEATHER_SHARE),
                        ))
                    * gain;
                if !sky.sky_blank(side_y, cell) || cov < 1.0 {
                    rasterize(frame.out, clip, verts, Axis::Horizontal, step, cap, blend);
                    verts.clear();
                    continue;
                }
                // A full sweep over the reach, from the landing's own stop
                // out, sliding outward as the splash spends.
                let stop = spectrum(tri(l.pin.t + d / cw / SPLASH_CELLS + drift));
                verts.push(RibbonVertex {
                    x: l.x + side_x * d,
                    spine,
                    up: half,
                    dn: half,
                    core_up: half * TRAIN_CORE_SHARE,
                    core_dn: half * TRAIN_CORE_SHARE,
                    color: if light { light_ink(stop) } else { stop },
                    cov: cov.min(cap_ink),
                    lift: 0.0,
                    lift_span: 0.0,
                });
            }
            rasterize(frame.out, clip, verts, Axis::Horizontal, step, cap, blend);
        }
    }
}

/// One spark's throw, hashed from the landing's seed and the spark's index
/// at the mint ([`Spark`]): thrown upward inside the [`SPARK_CONE_SHARE`]
/// cone at [`SPARK_V_MIN_CH_PER_S`]..[`SPARK_V_MAX_CH_PER_S`], spread
/// [`SPARK_SPREAD`] wider along the line, for
/// [`SPARK_LIFE_MIN_MS`]..[`SPARK_LIFE_MAX_MS`].
#[must_use]
fn mint_spark(seed: u32, k: usize, ch: f32) -> Spark {
    let s = mix32(seed ^ (k as u32).wrapping_mul(0x27D4_EB2F) ^ 0x5AA5_5AA5);
    let life = SPARK_LIFE_MIN_MS + (SPARK_LIFE_MAX_MS - SPARK_LIFE_MIN_MS) * unit01(s, 1);
    let a =
        std::f32::consts::PI * (SPARK_CONE_SHARE + (1.0 - 2.0 * SPARK_CONE_SHARE) * unit01(s, 2));
    // ch per second → px per ms.
    let v = (SPARK_V_MIN_CH_PER_S + (SPARK_V_MAX_CH_PER_S - SPARK_V_MIN_CH_PER_S) * unit01(s, 3))
        * ch
        / 1000.0;
    let sizes = SPARK_ARM_MAX_PX - SPARK_ARM_MIN_PX + 1;
    let arm = SPARK_ARM_MIN_PX + ((sizes as f32 * unit01(s, 4)) as i32).min(sizes - 1);
    Spark {
        vx: v * a.cos() * SPARK_SPREAD,
        vy: -v * a.sin(),
        life,
        arm: arm as u8,
    }
}

/// One spark's displacement and alpha at `age_ms` — a pure function of its
/// throw and its age (§18: no per-frame integration, no drift, and no
/// trigonometry on the frame path). Falls under [`SPARK_G_CH_PER_S2`]; full
/// for [`SPARK_HOLD_U`] of its life, then spent. `None` once the spark is
/// spent or under the chroma cull.
#[must_use]
fn spark_at(spark: Spark, age_ms: f32, ch: f32) -> Option<((f32, f32), f32)> {
    let u = age_ms / spark.life.max(1.0);
    if !(0.0..1.0).contains(&u) {
        return None;
    }
    let alpha = if u <= SPARK_HOLD_U {
        1.0
    } else {
        spend((u - SPARK_HOLD_U) / (1.0 - SPARK_HOLD_U))
    };
    if alpha < CHROMA_CULL_ALPHA {
        return None;
    }
    let g = SPARK_G_CH_PER_S2 * ch / 1_000_000.0;
    let dx = spark.vx * age_ms;
    let dy = spark.vy * age_ms + 0.5 * g * age_ms * age_ms;
    Some(((dx, dy), alpha))
}

/// The most quads one spark's star may spend — the family star's two bars,
/// its core and its taper spans; a spark past it loses its faintest tips.
pub const SPARK_STAR_QUAD_CAP: usize = 24;

/// **THE SPARKS** (2026-09-08; HALOED STARS in the second round) — a shower
/// of small stars thrown from the landing, climbing, hanging and falling,
/// the impact's last light on glass. Each is the family's one four-point
/// star (`push_twinkle_star`, D2) at 5–9 px ([`Spark::arm`]), its core the
/// named stop walked ROYGBIV from the landing's own (C1: point marks snap)
/// tinted [`SPARK_CORE_TINT`] of the way from white, inside a saturated halo
/// of the stop itself at [`SPARK_HALO_SHARE`] of the core — a dying star
/// dims halo-first ([`SPARK_SHRINK_ALPHA`]), then loses a pixel of arm, then
/// is a 3 px cross. At the transient cap: over a glyph cell that is the pin
/// arm's own request (L3). The shower is drawn from the frame the flash's
/// white has left ([`FLASH_BURST_MS`]) — inside the burst the stars are
/// invisible under the white and only cost. `halo_cap` is the frame's
/// spark-halo budget ([`SPARK_HALO_CAP`]); a spark past it keeps its star.
/// Light: ink crosses, no halos (§6.10's `peak < 96` cull). Reduced motion:
/// no sparks — they exist only as motion (§6.11).
fn draw_sparks(l: &Landing, ctx: &Ctx<'_>, frame: &mut Frame<'_>, halo_cap: usize) {
    if l.sparks == 0 || ctx.cfg.reduced_motion {
        return;
    }
    let age = ms_since(l.pin.at, ctx.now);
    // THE SPARKS ARE WHAT THE FLASH THROWS. For the white burst's life they
    // are inside it: sixty stars under a cell of white at 224 are invisible
    // on an additive glass and cost every quad and halo they spend, and a
    // star bar on the caret's own row is what a census reads as a pin arm.
    // So a spark is first drawn on the frame the white has left
    // ([`FLASH_BURST_MS`]), 0.4–0.7 `ch` out and climbing — the shower opens
    // as the flash dies into the spectrum.
    if age < FLASH_BURST_MS {
        return;
    }
    let geom = ctx.geom;
    let ch = geom.ch as f32;
    let intensity = clamp01(ctx.cfg.intensity);
    let light = !ctx.cfg.dark_theme;
    let stop0 = spectrum_snap_index(l.pin.t);
    for (k, spark) in l.spark.iter().enumerate().take(usize::from(l.sparks)) {
        let Some(((dx, dy), alpha)) = spark_at(*spark, age, ch) else {
            continue;
        };
        let cov = TRANSIENT_STAR_COV_CEIL * alpha * intensity;
        if cov < 1.0 {
            continue;
        }
        let rgb = spectrum_stop((stop0 + k) % SPECTRUM_STOPS);
        let arm0 = i32::from(spark.arm).max(1);
        let arm = if alpha >= SPARK_SHRINK_ALPHA {
            arm0
        } else if alpha >= SPARK_SHRINK_ALPHA * 0.5 {
            (arm0 - 1).max(1)
        } else {
            1
        };
        let (x, y) = ((l.x + dx).round() as i32, (l.y + dy).round() as i32);
        if light {
            let ink = (cov * LIGHT_INK_GAIN).min(LIGHT_ALPHA_CAP) as u8;
            let c = light_ink(rgb);
            push_ink_rect(frame.out, geom, (x - arm, y, 2 * arm + 1, 1), c, ink);
            push_ink_rect(frame.out, geom, (x, y - arm, 1, 2 * arm + 1), c, ink);
            continue;
        }
        // The halo first, so a star truncated by the quad budget keeps its
        // centre over it; the halo goes before the arm does.
        if alpha >= SPARK_SHRINK_ALPHA && frame.halos.len() < halo_cap {
            let r = arm as f32 + SPARK_HALO_R_ADD_PX;
            push_halo_add(
                frame.halos,
                geom,
                x as f32,
                y as f32,
                r,
                r,
                premul_rgb(rgb, (cov * SPARK_HALO_SHARE) as u8),
            );
        }
        let core = mix_rgb(0x00FF_FFFF, rgb, SPARK_CORE_TINT);
        let cap = frame.out.len() + SPARK_STAR_QUAD_CAP;
        push_twinkle_star(frame.out, geom, x, y, arm, cov as u8, false, core, cap);
    }
}

// ===========================================================================
// §6.5 layers 5 and 11, §6.12 — the stars this producer mints
// ===========================================================================

/// §6.5 layer 5 — the SHED FRAGMENTS: the part the meteor decides.
///
/// `n = SHED_N(cells)` m3 grains at hashed stations `s_i ∈ [0.10, 0.70]·L`,
/// each born on the frame the head passes its station — so on frame 0 every
/// station inside `p₀ = 0.28` is already alight, which is the second half of
/// T2's "light exists at both ends". Each fragment is handed to
/// [`Stardust::sow_shed`] with the station, the head's OBSERVED velocity
/// there in px/ms (T4: inherited, never smoothed — the sky takes its 0.12
/// share along the path and adds §5.6's perpendicular sputter) and the arc
/// colour at the station; the sky prices, clears and builds it. Silent (D5:
/// meteor-lane stars are exempt from the glint bucket).
///
/// A meteor ALWAYS sheds — v1 gated shedding on a live ribbon, so a cold
/// Home/End had no tail at all (§19.1).
fn shed(m: &mut Meteor, f: &Flight, ctx: &Ctx<'_>, sown: &mut Vec<Sow>) {
    let n = usize::from(shed_n(m.cells)).min(8);
    let cw = (ctx.geom.cw as f32).max(1.0);
    let passed = f.head_travel_share();
    let t_ms = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
    let speed = head_speed(f.l, t_ms, f.u);
    for i in 0..n {
        let bit = 1_u8 << i;
        if m.shed & bit != 0 {
            continue;
        }
        let share = SHED_S_MIN + (SHED_S_MAX - SHED_S_MIN) * unit01(m.seed, 0x5ED0 ^ i as u32);
        if passed < share {
            continue;
        }
        m.shed |= bit;
        let s = share * f.l;
        let at_px = (m.x0 + f.unit.0 * s, m.y0 + f.unit.1 * s);
        // Seam point 12: a station a scroll has carried off the glass is
        // spent, not sown — a fragment born above the window would hold a
        // pool slot for its whole life and light nothing (dropped, never
        // clamped).
        if !on_glass(ctx.geom, at_px.0, at_px.1) {
            continue;
        }
        sown.push(Sow::Shed {
            at: ctx.now,
            spec: ShedSow {
                at_px,
                v_head: (f.unit.0 * speed, f.unit.1 * speed),
                // §6.5 layer 5: `t_m(s_i)`. The station's own place on the
                // path is `s` from the LAUNCH, so its distance behind the
                // head at the landing is `L − s` — the fragment therefore
                // wears exactly the colour the train wears where it was shed,
                // which is what makes it read as a piece that came OFF the
                // meteor rather than as a star that happened to be there.
                tint_t: m.arc((f.l - s) / cw),
                seed: mix32(m.seed ^ (i as u32).wrapping_mul(0x9E37_79B9)),
                n: 1,
            },
        });
    }
}

/// Mint the three marks the arrival edge owns (§6.7). ONE `Instant`, spent
/// once. The fan's composition — **always 1 gold m1 + 4 m2 + the rest m3**
/// (D6), or D8's 5-7 m3 + one m2 with no hero for an Enter's small landing —
/// is the sky's class ladder ([`Stardust::sow_fan`]); what is decided HERE is
/// the count, the reach, the hero flag and the seed.
fn mint_landing(m: &Meteor, ctx: &Ctx<'_>) -> Landing {
    let at = m.arrival();
    let ch = ctx.geom.ch as f32;
    // The landing is the train's END — the endpoint a scroll moves with the
    // rest of the flight (seam point 12), never the spawn-time cell
    // re-derived: see `Meteor::landing`.
    let (x, y) = (m.x1, m.y1);

    // D8: an Enter's landing is sized by ROWS, not by the path. The flag is
    // `!ring` rather than a second licence test on purpose — the ring and the
    // small landing are the SAME decision, taken once at the spawn edge
    // (§6.11), so they cannot come apart later.
    let small = !m.ring;
    let cells_landing = if small {
        let drow = (m.y1 - m.y0).abs() / ch.max(1.0);
        ENTER_LANDING_CELLS_PER_ROW * drow
    } else {
        m.cells
    };

    let n = if small {
        // 5-7 m3 + 1 m2, and nothing else (§6.11).
        let extra = (cells_landing / ENTER_LANDING_CELLS_PER_ROW).round() as usize;
        (ENTER_LANDING_M3_MIN + extra.min(ENTER_LANDING_M3_MAX - ENTER_LANDING_M3_MIN) + 1)
            .min(super::timing::FAN_MAX_N)
    } else {
        // The graded count and reach — `(impact − 1)` above the floor, the
        // same form as the ring's — through the ONE function each that the
        // pins read (`fan_count`, `fan_reach_ch`).
        fan_count(m.cells, ctx.disp >= FAN_PARTY_DISP)
    };

    let reach = if small {
        FAN_REACH_MIN_CH * ch
    } else {
        fan_reach_ch(m.cells, m.grade) * ch
    };

    // The shower: sized by the path, and the big landing's only (D8). The
    // throws are hashed HERE, once, so the frame path does no trigonometry.
    let sparks = if small {
        0
    } else {
        ((SPARK_N_BASE + m.cells / SPARK_CELLS_PER).round() as usize).min(SPARK_N_MAX)
    };
    let seed = mix32(m.seed ^ 0xFA5E);
    let mut spark = [Spark::default(); SPARK_N_MAX];
    for (k, sp) in spark.iter_mut().enumerate().take(sparks) {
        *sp = mint_spark(seed, k, ch);
    }

    Landing {
        pin: Pin { at, t: m.arc(0.0) },
        ring: m.ring.then_some(Ring {
            at,
            scale: impact(m.cells),
            ms: ring_ms(m.cells),
        }),
        fan: Fan {
            at,
            n: n as u8,
            reach,
            seed,
            hero: !small,
        },
        x,
        y,
        sparks: sparks as u8,
        spark,
        splash: m.ring,
        sky: None,
    }
}

// ===========================================================================
// Small shared arithmetic
// ===========================================================================

/// Milliseconds from `a` to `b`, saturating at zero — every age in this module
/// runs through it, so a clock that moved backwards can never produce a
/// negative age and a negative age can never reach a coverage byte.
#[inline]
fn ms_since(a: Instant, b: Instant) -> f32 {
    b.saturating_duration_since(a).as_secs_f32() * 1000.0
}

/// Is a window pixel inside the effects box (`Geom::fx_*`)? The one test every
/// mark this file MINTS at a pixel a scroll may have moved runs first: a
/// landing or a station outside it is dropped, never clamped (seam point 12).
fn on_glass(geom: Geom, x: f32, y: f32) -> bool {
    let (xi, yi) = (x.round() as i32, y.round() as i32);
    xi >= geom.fx_left() && xi < geom.fx_right() && yi >= geom.fx_top() && yi < geom.fx_bot()
}

/// The head's position at an arbitrary instant — §6.9's chain test needs it,
/// and it must be the SAME curve the emit draws.
fn head_at(m: &Meteor, now: Instant) -> (f32, f32) {
    let t = (m.t_flight.as_secs_f32() * 1000.0).max(1.0);
    let p = enter_at_speed(ms_since(m.t0, now) / t);
    let (dx, dy) = m.delta();
    (m.x0 + dx * p, m.y0 + dy * p)
}

/// The head's SPEED in px/ms at `u`, from `enter-at-speed`'s own derivative:
/// `p(u) = 1 − (1 − p₀)(1 − u)^2.2` ⇒ `dp/dt = (1 − p₀)·2.2·(1 − u)^1.2 / T`.
///
/// Analytic rather than differenced, so it needs no previous frame — which is
/// what keeps a frame a pure function of `now` (§18) while still letting the
/// shoulder span the true per-frame travel (T7).
fn head_speed(l: f32, t_ms: f32, u: f32) -> f32 {
    if t_ms <= 0.0 {
        return 0.0;
    }
    l * (1.0 - FLIGHT_P0) * FLIGHT_ENTER_EXP * (1.0 - clamp01(u)).powf(FLIGHT_ENTER_EXP - 1.0)
        / t_ms
}

/// The shortest distance between two segments (§6.9's corridor test).
///
/// Crossing segments are zero by definition; otherwise the minimum is attained
/// at one of the four endpoint-to-segment distances, which is exact and needs
/// no parallel-case special pleading.
fn seg_gap(a0: (f32, f32), a1: (f32, f32), b0: (f32, f32), b1: (f32, f32)) -> f32 {
    if segments_cross(a0, a1, b0, b1) {
        return 0.0;
    }
    point_seg(a0, b0, b1)
        .min(point_seg(a1, b0, b1))
        .min(point_seg(b0, a0, a1))
        .min(point_seg(b1, a0, a1))
}

/// Distance from a point to a segment.
fn point_seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0)
    };
    (p.0 - (a.0 + vx * t)).hypot(p.1 - (a.1 + vy * t))
}

/// Do two segments properly cross? Four orientation signs, no division.
fn segments_cross(a0: (f32, f32), a1: (f32, f32), b0: (f32, f32), b1: (f32, f32)) -> bool {
    let cross = |o: (f32, f32), p: (f32, f32), q: (f32, f32)| {
        (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0)
    };
    let (d1, d2) = (cross(b0, b1, a0), cross(b0, b1, a1));
    let (d3, d4) = (cross(a0, a1, b0), cross(a0, a1, b1));
    (d1 * d2 < 0.0) && (d3 * d4 < 0.0)
}

/// `lowbias32` — the crate's own integer mixer (`cursor_glow.rs`'s spawn
/// hashes and `stardust.rs`'s deals use this exact pair of constants; the
/// sky's copy is private to its module, so this is the meteor's spelling of
/// the same function, not a second hash). Deterministic, cheap, and with no
/// per-frame RNG anywhere near it (§18). This file hashes only what the
/// meteor itself decides — the spawn seed and the shed STATIONS; every
/// per-star draw (angle, jitter, phase, rate, tint deal) is the sky's, made
/// from the seed handed over.
#[inline]
fn mix32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// The spawn seed — §18's `(row, col, born)`, with the spawn ORDINAL standing
/// in for `born` (see [`Meteors::ord`]).
#[inline]
fn seed_of(row: u16, col: u16, ord: u32) -> u32 {
    mix32(u32::from(row) << 20 ^ u32::from(col) << 8 ^ ord.wrapping_mul(0x9E37_79B9))
}

/// A seeded value in `[0, 1)` — the one place in this file a hash becomes a
/// number (the shed stations), so no draw rolls its own bias.
#[inline]
fn unit01(seed: u32, salt: u32) -> f32 {
    (mix32(seed ^ salt) >> 8) as f32 / 16_777_216.0
}

/// **THE LIGHT INK** (§3.3, L6): `rgb` scaled down — hue and saturation intact,
/// every channel by the same factor — until it satisfies BOTH
/// [`LIGHT_INK_MAX_LUMA`] and [`LIGHT_INK_MAX_CHANNEL`]. A colour already under
/// both is returned unchanged.
///
/// Scaling uniformly rather than mixing toward black is what preserves the
/// hue: the mark is still recognisably its own stop, at ink weight. This is
/// the light arm's whole operator flip — additive light becomes source-over
/// ink and "bigger" is bought as AREA, never as brightness.
fn light_ink(rgb: u32) -> u32 {
    let rgb = rgb & 0x00FF_FFFF;
    let (r, g, b) = (
        ((rgb >> 16) & 0xFF) as f32,
        ((rgb >> 8) & 0xFF) as f32,
        (rgb & 0xFF) as f32,
    );
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let hi = r.max(g).max(b);
    let k = (LIGHT_INK_MAX_LUMA / y.max(1.0))
        .min(LIGHT_INK_MAX_CHANNEL / hi.max(1.0))
        .min(1.0);
    if k >= 1.0 {
        return rgb;
    }
    let ch = |c: f32| ((c * k) + 0.5) as u32;
    (ch(r) << 16) | (ch(g) << 8) | ch(b)
}

/// One RADIAL halo of premultiplied ADDITIVE light — the dark arm's coma and
/// nucleus (§6.5 layers 6 and 7).
///
/// The equivalent of `cursor_glow`'s private `push_halo`, written here rather
/// than widening that function's visibility: the bounding rect is clamped to
/// the EFFECTS BOX and split into per-cell-row quads that all share one centre
/// and one pair of falloff radii, and **the centre is never clamped into the
/// box** — clamping it moves an off-grid centre onto the edge and renders the
/// surviving sliver at a fraction of peak weight instead of at the falloff's
/// true ~0 (the 2026-09-01 bright-fringe audit). Radii are floored to 1, which
/// is the parity contract's divide-by-zero guard.
fn push_halo_add(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    peak: u32,
) {
    if peak == 0 {
        return;
    }
    let (rxi, ryi) = ((rx.round() as i32).max(1), (ry.round() as i32).max(1));
    let (cxi, cyi) = (cx.round() as i32, cy.round() as i32);
    let Some((x0, x1, y0, y1)) = halo_box(geom, cxi, cyi, rxi, ryi) else {
        return;
    };
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let mut yy = y0;
    while yy < y1 {
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: peak,
            cx: cxi as u16,
            cy: cyi as u16,
            rx: rxi as u16,
            ry: ryi as u16,
            mode: HaloMode::Add,
        });
        yy = band_end;
    }
}

/// One RADIAL halo as a source-over INK VEIL — the light arm's twin (§6.10).
///
/// `peak` is the centre opacity and it does BOTH jobs the light rasterizer
/// asks of one: it rides the colour's high byte as `aterm_render::
/// halo_over_cap`'s ceiling, and it sizes the veil (a dim veil is also a small
/// one). The colour itself rides through STRAIGHT — unpremultiplied — because
/// under `HaloMode::Over` the radial falloff scales the OPACITY rather than
/// the light. Below the light arm's visibility floor the veil is dropped
/// entirely rather than drawn as a smudge (§3.3's `peak < 96` cull, restated
/// for the marks this file owns).
fn push_halo_veil(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    c: (f32, f32),
    r: (f32, f32),
    color: u32,
    peak: u8,
) {
    if peak < LIGHT_HALO_MIN_PEAK {
        return;
    }
    // The light rasterizer sizes a veil by its own opacity, so a dim veil is
    // also a small one; mirroring that here keeps the two arms the same shape.
    let scale = (f32::from(peak) / 255.0).max(0.55);
    let (rxi, ryi) = (
        ((r.0 * scale).round() as i32).max(1),
        ((r.1 * scale).round() as i32).max(1),
    );
    let (cxi, cyi) = (c.0.round() as i32, c.1.round() as i32);
    let Some((x0, x1, y0, y1)) = halo_box(geom, cxi, cyi, rxi, ryi) else {
        return;
    };
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let mut yy = y0;
    while yy < y1 {
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: (u32::from(peak) << 24) | (color & 0x00FF_FFFF),
            cx: cxi as u16,
            cy: cyi as u16,
            rx: rxi as u16,
            ry: ryi as u16,
            mode: HaloMode::Over,
        });
        yy = band_end;
    }
}

/// The light arm's halo visibility floor (§3.3, §6.10) — a veil dimmer than
/// this is dropped, not drawn.
const LIGHT_HALO_MIN_PEAK: u8 = 96;

/// The effects-box intersection of a halo's bounding rect, or `None` when
/// nothing of it lands on glass.
fn halo_box(geom: Geom, cxi: i32, cyi: i32, rxi: i32, ryi: i32) -> Option<(i32, i32, i32, i32)> {
    let (bl, br) = (geom.fx_left(), geom.fx_right());
    let (bt, bb) = (geom.fx_top(), geom.fx_bot());
    let x0 = (cxi - rxi).max(bl);
    let x1 = (cxi + rxi).min(br);
    let y0 = (cyi - ryi).max(bt);
    let y1 = (cyi + ryi).min(bb);
    if x1 <= x0 || y1 <= y0 || cxi < 0 || cyi < 0 {
        return None;
    }
    Some((x0, x1, y0, y1))
}

/// A rect of source-over INK, clamped to the effects box and split into
/// per-cell-row quads with origin-anchored row damage tags.
///
/// `push_fx_rect`'s light twin, and the ONE difference is the operator: it
/// stamps the coverage that premultiplied the colour as the quad's own
/// opacity, so the destination is attenuated instead of added to. At
/// `alpha == 0` the two are the same emission.
fn push_ink_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    rect: (i32, i32, i32, i32),
    color: u32,
    alpha: u8,
) {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 || alpha == 0 {
        return;
    }
    let x0 = x.max(geom.fx_left());
    let x1 = (x + w).min(geom.fx_right());
    let y0 = y.max(geom.fx_top());
    let y1 = (y + h).min(geom.fx_bot());
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let premul = premul_rgb(color, alpha);
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let mut yy = y0;
    while yy < y1 {
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
            alpha,
        });
        yy = band_end;
    }
}

/// Compile-time restatements of the budget arithmetic §18 depends on, so a
/// later retune cannot silently break the "a ping-pong never blanks the
/// ribbon" law by editing one number.
const _: () = {
    assert!(
        FLIGHT_MAX_LIVE * (UNDER_QUAD_CAP + SPLASH_QUAD_CAP) + super::ribbon::RIBBON_QUAD_BUDGET
            == 16_384,
        "§18: two meteors, two splashes and the ribbon must land exactly on MAX_QUADS"
    );
    // The impact's last light — the longest spark, the shockwave, the
    // splash, the colour train's chroma cull (`τ·ln(1/0.12)`) and its root's
    // retract — is inside the pool's own horizon (§6.2 as re-ruled).
    assert!(
        SPARK_LIFE_MAX_MS <= FLIGHT_OFF_GLASS_MS
            && RING_MS_MAX <= FLIGHT_OFF_GLASS_MS
            && SPLASH_MS <= FLIGHT_OFF_GLASS_MS
            && TRAIN_COLOUR_TAU_MS * RETIRE_DECAY <= FLIGHT_OFF_GLASS_MS
            && ROOT_SUCK_MS <= FLIGHT_OFF_GLASS_MS,
        "every impact mark must be off glass by FLIGHT_OFF_GLASS_MS"
    );
    assert!(
        SPARK_N_MAX <= 255 && SPARK_CONE_SHARE > 0.0 && SPARK_CONE_SHARE < 0.5,
        "the spark count is a u8 and the cone opens upward"
    );
    assert!(
        SPARK_ARM_MIN_PX >= 1 && SPARK_ARM_MAX_PX >= SPARK_ARM_MIN_PX && SPARK_ARM_MAX_PX <= 255,
        "a spark's arm is a u8 of at least one pixel"
    );
    // The second round's impact: the flash's colour is inside the pool's
    // horizon; the flash and the shockwave's hold are ATTACK inside the pin's
    // own window, so the cadence law's attack is the pin's as before; the
    // stroke is two and a half times the bold round's; the two masks hold
    // every cell they are asked about.
    assert!(
        FLASH_COLOUR_MS <= FLIGHT_OFF_GLASS_MS
            && FLASH_BURST_MS <= PIN_MS
            && RING_HOLD_MS <= PIN_MS
            && RING_HOLD_MAX_MS <= PIN_MS
            && FLASH_COLOUR_MS <= RING_MS,
        "the flash and the shockwave's hold must sit inside the pin's attack"
    );
    assert!(
        RING_THICK_SHARE >= 2.5 * RING_THICK_SHARE_BOLD_ROUND,
        "the second round's stroke is two and a half times the bold round's"
    );
    assert!(
        2 * (SPLASH_CELLS as usize) < u16::BITS as usize
            && 2 * (FLASH_CELLS as usize) < u8::BITS as usize,
        "the sky masks must hold every cell the splash and the flash light"
    );
    assert!(
        FLASH_FULL_COV > TRANSIENT_STAR_COV_CEIL && FLASH_FULL_COV < 255.0,
        "the flash over a blank cell is louder than the cap and under the caret"
    );
    assert!(
        WHITE_QUAD_CAP < UNDER_QUAD_CAP,
        "§6.5: the white layer is the short one"
    );
    assert!(
        RING_QUAD_CAP == RING_SEGMENTS,
        "§6.5 layer 10: one quad per ring segment"
    );
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::cursor_glow::SoundCue;
    use crate::rainbow_kitty::ribbon::walk_t;
    use crate::rainbow_kitty::stardust::{
        FAN_RISE_MAX_CH, FAN_THROW_JITTER_MAX, FAN_THROW_JITTER_MIN, Star, StarClass, StarLane,
        TINT_GOLD_RGB,
    };
    use crate::rainbow_kitty::timing::FAN_HERO_N;
    use crate::rainbow_kitty::{CaretSeam, Config};
    use crate::spectrum::SPECTRUM_ANCHORS;

    fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1200,
            win_h: 800,
            head: 0,
        }
    }

    fn config() -> Config {
        Config {
            dark_theme: true,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            theme_fg: 0x00E8_E8F0,
            theme_bg: 0x0016_161C,
            reduced_motion: false,
        }
    }

    /// A frame's worth of host scratch, reused exactly as the host reuses
    /// `CursorGlow`'s.
    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Scratch {
        fn frame(&mut self) -> Frame<'_> {
            Frame {
                under: &mut self.under,
                out: &mut self.out,
                halos: &mut self.halos,
                beams: &mut self.beams,
                cues: &mut self.cues,
                caret: CaretSeam::default(),
                companion: None,
                fp: 0,
            }
        }
        fn clear(&mut self) {
            self.under.clear();
            self.out.clear();
            self.halos.clear();
            self.beams.clear();
            self.cues.clear();
        }
        /// One emit into a cleared scratch.
        fn emit(&mut self, m: &mut Meteors, ctx: &Ctx<'_>) {
            self.clear();
            let mut fr = self.frame();
            m.emit(ctx, &mut fr);
        }
    }

    fn ctx_with(
        now: Instant,
        cfg: &Config,
        caret: (u16, u16),
        caret_t: f32,
        disp: f32,
        birth_disp: f32,
    ) -> Ctx<'_> {
        Ctx {
            now,
            geom: geom(),
            cfg,
            disp,
            birth_disp,
            phase: 0.0,
            caret,
            caret_t,
            mend: None,
        }
    }

    fn ctx_at(now: Instant, cfg: &Config, caret: (u16, u16), caret_t: f32) -> Ctx<'_> {
        ctx_with(now, cfg, caret, caret_t, 0.5, 0.5)
    }

    fn mv_as(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
        Event::Move {
            from,
            to,
            licence,
            dir: Dir::of(
                i32::from(to.1) - i32::from(from.1),
                i32::from(to.0) - i32::from(from.0),
            ),
        }
    }

    fn mv(from: (u16, u16), to: (u16, u16)) -> Event {
        mv_as(from, to, Licence::Nav)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A sky whose glyph probe has LOOKED at rows 3-10 and found them blank
    /// — §5.4's gate, fed explicitly: a test that left the probe empty would
    /// be relying on the free-transient asymmetry rather than stating it.
    fn sky() -> Stardust {
        let mut dust = Stardust::new();
        for row in 3..=10 {
            dust.probe_mut().probe_row(row, &[false; 120]);
        }
        dust
    }

    /// The sky with the landing row and its two neighbours PROVED blank — the
    /// three rows the host's probe holds (`GlyphProbe::ROWS`), which is what
    /// the flash and the splash are priced on. `sky()` probes eight rows into
    /// a three-row probe and keeps the last three (8..=10), leaving the
    /// landing row's neighbours UNKNOWN — and unknown is not blank (L4).
    fn sky_around(row: i32) -> Stardust {
        let mut dust = Stardust::new();
        for r in row - 1..=row + 1 {
            dust.probe_mut().probe_row(r, &[false; 120]);
        }
        dust
    }

    fn lane(dust: &Stardust, lane: StarLane) -> Vec<&Star> {
        dust.live_iter().filter(|s| s.lane == lane).collect()
    }

    /// The premultiplied luminance of a quad, in levels — the only thing the
    /// compositor will do with it, so the only thing an oracle should read.
    fn lum(c: u32) -> f32 {
        0.2126 * ((c >> 16) & 0xFF) as f32
            + 0.7152 * ((c >> 8) & 0xFF) as f32
            + 0.0722 * (c & 0xFF) as f32
    }

    fn chan(c: u32) -> (u32, u32, u32) {
        ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
    }

    /// Achromatic and lit — the white heat, the nucleus, the pin's heart.
    fn is_white(c: u32) -> bool {
        let (r, g, b) = chan(c);
        r == g && g == b && r > 0
    }

    /// Lit and NOT achromatic — a spectrum mark.
    fn is_chromatic(c: u32) -> bool {
        let (r, g, b) = chan(c);
        r.max(g).max(b) > 0 && !(r == g && g == b)
    }

    /// Which of the seven named stops a premultiplied quad colour is nearest,
    /// after normalising away the coverage it was multiplied by. C1: the seven
    /// stops are the only colour law, so "which stop is this" is a total
    /// question with a total answer.
    fn stop_of(c: u32) -> usize {
        let (r, g, b) = chan(c);
        let hi = r.max(g).max(b).max(1) as f32;
        let n = |v: u32| v as f32 * 255.0 / hi;
        let (nr, ng, nb) = (n(r), n(g), n(b));
        let mut best = 0;
        let mut near = f32::INFINITY;
        for (i, &a) in SPECTRUM_ANCHORS.iter().enumerate() {
            let (ar, ag, ab) = chan(a);
            let d = (nr - ar as f32).powi(2) + (ng - ag as f32).powi(2) + (nb - ab as f32).powi(2);
            if d < near {
                near = d;
                best = i;
            }
        }
        best
    }

    /// The white nucleus's centre: the achromatic halo with the highest
    /// premultiplied peak (§3.2 rank 1 — the brightest pixel is the smallest
    /// thing).
    fn nucleus(halos: &[RainHalo]) -> Option<&RainHalo> {
        halos
            .iter()
            .filter(|h| is_white(h.color))
            .max_by(|a, b| lum(a.color).total_cmp(&lum(b.color)))
    }

    fn nucleus_cx(halos: &[RainHalo]) -> Option<(f32, f32, f32)> {
        nucleus(halos).map(|h| (f32::from(h.cx), f32::from(h.cy), lum(h.color)))
    }

    /// The coma: the brightest CHROMATIC halo — on a frame before the
    /// shower opens (`T + 80`), when the corona's petals are the only
    /// chromatic halos the meteor draws. Past it, [`coma_of`].
    fn coma(halos: &[RainHalo]) -> Option<&RainHalo> {
        halos
            .iter()
            .filter(|h| is_chromatic(h.color))
            .max_by(|a, b| lum(a.color).total_cmp(&lum(b.color)))
    }

    /// Is this halo a SPARK's (second round, 2026-09-08) — centred where one
    /// of `landings`' sparks is at `now`? A corona petal and a spark's halo
    /// are both 4–6 px at this geometry, so the two are told apart by
    /// POSITION, which `spark_at` names exactly for the frame's age.
    fn is_spark_halo(h: &RainHalo, landings: &[Landing], now: Instant) -> bool {
        let ch = geom().ch as f32;
        landings.iter().any(|l| {
            let age = ms_since(l.pin.at, now);
            l.spark.iter().take(usize::from(l.sparks)).any(|s| {
                spark_at(*s, age, ch).is_some_and(|((dx, dy), _)| {
                    (l.x + dx).round() as i32 == i32::from(h.cx)
                        && (l.y + dy).round() as i32 == i32::from(h.cy)
                })
            })
        })
    }

    /// The coma on any frame: the brightest CHROMATIC halo that is not a
    /// spark's ([`is_spark_halo`]).
    fn coma_of<'a>(
        halos: &'a [RainHalo],
        landings: &[Landing],
        now: Instant,
    ) -> Option<&'a RainHalo> {
        halos
            .iter()
            .filter(|h| is_chromatic(h.color) && !is_spark_halo(h, landings, now))
            .max_by(|a, b| lum(a.color).total_cmp(&lum(b.color)))
    }

    fn overlaps(a: &GlowQuad, b: &GlowQuad) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    // -- the arc (kept from the frozen contract) ---------------------------

    /// D4, the correction the redesign turns on: the arc REFLECTS instead of
    /// clamping, so a long meteor always spans the spectrum — and the station
    /// under the caret is always the caret's own stop.
    #[test]
    fn the_meteor_arc_reflects_and_never_clamps() {
        // tri is a triangle wave on [0, 1] with period 2.
        assert!((tri(0.0)).abs() < 1e-6);
        assert!((tri(1.0) - 1.0).abs() < 1e-6);
        assert!((tri(2.0)).abs() < 1e-6);
        assert!((tri(1.5) - 0.5).abs() < 1e-6);
        assert!(
            (tri(-0.5) - 0.5).abs() < 1e-6,
            "reflects on the negative side"
        );
        for k in 0_u16..400 {
            let x = -20.0 + f32::from(k) * 0.1;
            let v = tri(x);
            assert!(
                (0.0..=1.0).contains(&v),
                "tri({x}) = {v} left the unit range"
            );
        }

        // The station under the caret IS the caret's stop, at every stop.
        for i in 0_u8..=10 {
            let t_land = f32::from(i) / 10.0;
            assert!(
                (arc_t(t_land, 0.0) - t_land).abs() < 1e-6,
                "phase lock broken at t_land {t_land}"
            );
        }

        // An 80-cell meteor spans 80/36 = 2.22 t-units — 2.2 full sweeps — so
        // it visits every seventh of the range, over a fresh run OR a dead
        // prompt (t_land 0.0 is the "dead prompt reads red" case v1 failed).
        for t_land in [0.0_f32, 0.37, 0.91] {
            let mut seen = [false; 7];
            let mut c = 0.0;
            while c <= 80.0 {
                let idx = ((arc_t(t_land, c) * 6.0).round() as usize).min(6);
                seen[idx] = true;
                c += 0.25;
            }
            let stops = seen.iter().filter(|s| **s).count();
            assert!(
                stops >= 5,
                "an 80-cell meteor at t_land {t_land} showed only {stops} stops"
            );
        }
    }

    /// D15: the axis rule is decided on the pixel delta, and the tie goes to
    /// the horizontal rasterizer (`|dx| ≥ |dy|`).
    #[test]
    fn the_axis_rule_sends_verticals_to_the_transposed_twin() {
        assert_eq!(Axis::of(100.0, 0.0), Axis::Horizontal);
        assert_eq!(Axis::of(0.0, 100.0), Axis::Vertical);
        assert_eq!(Axis::of(-50.0, 10.0), Axis::Horizontal);
        assert_eq!(Axis::of(10.0, -50.0), Axis::Vertical);
        assert_eq!(Axis::of(7.0, 7.0), Axis::Horizontal, "the tie is x-major");
    }

    /// §6.3: distance buys length and speed, not fat — the width grade is
    /// capped at ×1.5 where v1's 0.85 gain reached ×1.85. Measured on the
    /// `w` a SPAWN latches, at equal `mom`, so a spawn that dropped the grade
    /// term or read `disp` instead of `birth_disp` would be caught here.
    #[test]
    fn the_width_grade_is_capped_at_one_and_a_half() {
        let cfg = config();
        let t0 = Instant::now();
        let spawn_w = |cells: u16, disp: f32, birth_disp: f32| -> f32 {
            let mut m = Meteors::new();
            let ctx = ctx_with(t0, &cfg, (5, cells), 0.25, disp, birth_disp);
            m.on_event(&mv((5, 0), (5, cells)), t0, &ctx)
                .expect("must fly");
            m.live[0].w
        };
        let ratio = spawn_w(80, 1.0, 1.0) / spawn_w(8, 1.0, 1.0);
        assert!(
            ratio <= 1.5 + 1e-3,
            "w(80)/w(8) = {ratio} at equal mom must not exceed 1.5"
        );
        assert!(ratio > 1.0, "…but distance still grades the width");
        // The published figure at ch = 18: hot 34-cell ≈ 21 px (§6.3 wrote
        // 19; re-pinned 2026-09-08 with `W_BASE_CH` 0.44 — the owner: "a
        // bigger more special rainbow impact"), and the grade saturates
        // there.
        let hot34 = spawn_w(34, 1.0, 1.0);
        assert!(
            (hot34 - 21.1).abs() < 0.4,
            "hot 34-cell ≈ 21 px, got {hot34}"
        );
        assert!(
            (spawn_w(80, 1.0, 1.0) - hot34).abs() < 1e-3,
            "grade saturates at 34"
        );
        // `mom` is `birth_disp`, latched at the spawn — never `disp`.
        let cold = spawn_w(34, 1.0, 0.0);
        assert!(
            (cold - 18.0 * W_BASE_CH * (1.0 + W_GRADE_GAIN)).abs() < 0.4,
            "a cold spine at full disp is still a cold meteor: {cold}"
        );

        // And the graded head coverage never passes L3's roof — measured on
        // the nucleus the frame draws: an 80-cell head asks for 118 × 1.5 =
        // 177 and is held to 160; an 8-cell head sits under it.
        let peak = |cells: u16| -> f32 {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, cells), 0.25);
            m.on_event(&mv((5, 0), (5, cells)), t0, &ctx)
                .expect("must fly");
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx);
            nucleus_cx(&sc.halos).expect("a nucleus on frame zero").2
        };
        let far = peak(80);
        assert!(
            (JUMP_COV_CEIL - 1.0..=JUMP_COV_CEIL + 0.5).contains(&far),
            "an 80-cell nucleus is at {far}, not at L3's roof {JUMP_COV_CEIL}"
        );
        assert!(
            peak(8) < JUMP_COV_CEIL - 10.0,
            "an 8-cell head is under the roof"
        );
    }

    /// §6.9: a retired train's colour reaches the chroma cull at EXACTLY
    /// `R = 60 ms`. [`RETIRE_DECAY`] is spelled as a number because `ln` is not
    /// `const`; this is the pin that stops it drifting away from
    /// [`CHROMA_CULL_ALPHA`].
    #[test]
    fn the_retire_decay_lands_on_the_chroma_cull() {
        let at_r = (-RETIRE_DECAY).exp();
        assert!(
            (at_r - CHROMA_CULL_ALPHA).abs() < 1e-3,
            "a retired train is at {at_r} after R, not the cull {CHROMA_CULL_ALPHA}"
        );
    }

    // -- T2, the frame-0 law ------------------------------------------------

    /// **T2 / §6.6, the acceptance image.** On the frame the caret is first
    /// observed at its landing the head is ALREADY at `p₀ = 0.28` of the path,
    /// moving, with the train lit behind it back to the launch cell. v1 drew
    /// literally nothing on this frame (`emit_rainbow_jumps`'s
    /// `len < 1.0 → continue` sat above both nucleus halos) and its head
    /// arrived 120 ms after the caret; the owner read that as lag, and this
    /// test is the pin that it is gone.
    #[test]
    fn the_head_is_already_moving_and_lit_on_frame_zero() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m
            .on_event(&mv((5, 0), (5, 80)), t0, &ctx)
            .expect("an 80-cell Home/End must fly");
        assert_eq!(spawn.t0, t0, "§6.7: the spawn edge IS the observed frame");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(
            !sc.under.is_empty(),
            "T2: the colour train must be lit on frame zero"
        );
        assert!(
            !sc.out.is_empty(),
            "T2: the white heat must be lit on frame zero"
        );

        let (cx, _, peak) = nucleus_cx(&sc.halos).expect("T2: the nucleus must exist at age 0");
        let (x0, _) = geom().cell_center(5, 0);
        let (x1, _) = geom().cell_center(5, 80);
        let l = x1 - x0;
        let want = x0 + FLIGHT_P0 * l;
        assert!(
            (cx - want).abs() <= 0.02 * l + NUCLEUS_LEAD_PX + 1.0,
            "the head is at {cx}, not at p0 = {want} (0.28 of the path)"
        );
        assert!(peak >= 100.0, "the nucleus is dim on frame zero: {peak}");

        // …and the shoulder is lit BEHIND it, within 0.06·L.
        let hot = sc
            .out
            .iter()
            .filter(|q| {
                let x = f32::from(q.x);
                (cx - SHOULDER_SHARE * l..=cx).contains(&x) && lum(q.color) >= 100.0
            })
            .count();
        assert!(
            hot > 0,
            "T2: no white ≥ 100 within 0.06·L behind the head on frame zero"
        );

        // T2's second half: it is already MOVING. The head's displacement over
        // the first 8 ms is at least a tenth of the path at every published T.
        for cells in [8.0_f32, 40.0, 200.0] {
            let t = super::super::timing::flight_ms(cells);
            let d = enter_at_speed(8.0 / t) - enter_at_speed(0.0);
            assert!(
                d >= 0.10,
                "T = {t}: the head moved {d} of the path in 8 ms, not 0.10"
            );
        }
    }

    // -- D4, the flat-red-bar regression pin --------------------------------

    /// **D4, the measured #1 visual defect.** v1's `rainbow_field_line_lit`
    /// continued the nearest laid cell's slope and CLAMPED it, so behind a
    /// short fresh run a Home/End read `t = 0` — one red bar across 28 cells.
    /// The meteor's own arc reflects instead, so an 80-cell train spans at
    /// least five of the seven stops over a DEAD PROMPT and behind a SHORT
    /// FRESH RUN, in BOTH directions.
    #[test]
    fn an_eighty_cell_meteor_spans_the_arc_in_both_directions() {
        let cfg = config();
        // t_land 0.0 is the dead prompt (v1's red bar); 9/36 is the field
        // nine typed cells into a fresh run.
        for t_land in [0.0_f32, 9.0 / 36.0] {
            for (from, to) in [((5_u16, 0_u16), (5_u16, 80_u16)), ((5, 80), (5, 0))] {
                let t0 = Instant::now();
                let mut m = Meteors::new();
                let ctx = ctx_at(t0, &cfg, to, t_land);
                let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("must fly");
                // At the arrival edge the whole path is lit.
                let land = ctx_at(t0 + spawn.t_flight, &cfg, to, t_land);
                let mut sc = Scratch::default();
                sc.emit(&mut m, &land);

                let mut seen = [false; 7];
                for q in &sc.under {
                    let (r, g, b) = chan(q.color);
                    if r.max(g).max(b) >= 16 {
                        seen[stop_of(q.color)] = true;
                    }
                }
                let n = seen.iter().filter(|s| **s).count();
                assert!(
                    n >= 5,
                    "t_land {t_land}, {from:?}->{to:?}: the train showed {n} stops, not 5 \
                     (the flat-red-bar regression)"
                );
            }
        }
    }

    /// **§6.4 / D4, the phase lock.** `t_land` is the caret's field at the
    /// landing, LATCHED on the spawn edge: the pin's arms, the coma and the
    /// station under the caret all wear that stop, and a later frame whose
    /// caret field has walked on does not recolour them.
    #[test]
    fn the_landing_phase_locks_to_the_field_latched_at_spawn() {
        let cfg = config();
        let t0 = Instant::now();
        let yellow = 1.0 / 3.0;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), yellow);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");

        // The caret's field on the arrival frame is somewhere else entirely.
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (5, 40), 5.0 / 6.0);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &land);

        let pin = m
            .landings
            .first()
            .expect("the arrival minted a landing")
            .pin;
        assert!(
            (pin.t - yellow).abs() < 1e-6,
            "the pin took {}, not the latched t_land",
            pin.t
        );
        let cy = land.geom.cell_center(5, 40).1.round() as i32;
        let arms: Vec<_> = sc
            .out
            .iter()
            .filter(|q| is_chromatic(q.color) && q.h == 1 && i32::from(q.y) == cy)
            .collect();
        assert!(
            !arms.is_empty(),
            "the pin's arms are missing on the arrival frame"
        );
        for q in arms {
            assert_eq!(stop_of(q.color), 2, "a pin arm is not the landing's yellow");
        }
        // The corona (2026-09-08): seven petals, the latched yellow among
        // them — the rainbow halo the owner asked for, not one yellow blob.
        assert!(coma(&sc.halos).is_some(), "the corona is drawn at T");
        assert!(
            sc.halos
                .iter()
                .any(|h| is_chromatic(h.color) && stop_of(h.color) == 2),
            "the corona left the latched stop"
        );
    }

    /// **§6.4 / D4, the lock is reflected too.** The caret's field `t` is the
    /// classic walk, and the walk is UNBOUNDED: `d/16` over the first sixteen
    /// cells, then `1/36` a cell, so the caret at the end of a 48-cell line
    /// sits at `walk_t(47) = 1.861` — which the ribbon draws through `tri` as
    /// orange. A `t_land` that CLAMPED that walk to `1.0` locked every
    /// long-line Ctrl-A meteor to violet, so the station under the caret was
    /// NOT the caret's own stop and the train beside an orange ribbon head
    /// flew blue-violet only (measured: `#170022 #1b0034 #020038`). The lock
    /// takes the raw walk and `arc_t` folds it through the SAME `tri` the
    /// ribbon uses, exactly once: the pin, the coma and the station under the
    /// caret all wear `spectrum(tri(caret_t))`, and the spawn frame's train
    /// is already a rainbow.
    #[test]
    fn the_meteor_locks_to_the_carets_reflected_stop_on_a_long_line() {
        let cfg = config();
        let t0 = Instant::now();
        // The caret at the end of a 48-cell line — the ribbon's own walk law,
        // not a number typed in: 16 fast cells, then 31 at the lay rate.
        let caret_t = walk_t(47.0);
        assert!(
            caret_t > 1.0 && caret_t < 2.0,
            "walk_t(47) = {caret_t} is not on the walk's second leg"
        );
        let want_t = tri(caret_t);
        let want = spectrum(want_t);
        let want_stop = stop_of(want);
        let clamped_stop = stop_of(spectrum(clamp01(caret_t)));
        assert_ne!(
            want_stop, clamped_stop,
            "the reflected stop and the clamped stop coincide, so this test cannot bite"
        );

        // Ctrl-A: from the end of the line home to column 0.
        let (from, to) = ((5_u16, 47_u16), (5_u16, 0_u16));
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, to, caret_t);
        let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("must fly");

        // The spawn frame: the head is already `FLIGHT_P0` of the way along
        // (§21.3), so 13 cells of train are on glass — and since 2026-09-08
        // ("I want the meteor to have rainbow!") that span carries a WHOLE
        // sweep (`ARC_FRAME0_SWEEPS`): the spawn frame is a rainbow, not
        // three stops. (Before the re-ruling this clause pinned the train to
        // the caret's stop and its neighbours and forbade the clamped stop;
        // with every stop on glass on every frame that distinction is
        // carried by the ARRIVAL clauses below, where the lock is read.)
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        let mut seen = [false; 7];
        for q in &sc.under {
            let (r, g, b) = chan(q.color);
            if r.max(g).max(b) >= 16 && is_chromatic(q.color) {
                seen[stop_of(q.color)] = true;
            }
        }
        let n = seen.iter().filter(|s| **s).count();
        assert!(
            n >= 6,
            "the spawn frame's train showed {n} stops, not a rainbow: {seen:?}"
        );
        // The corona is a rainbow (2026-09-08): seven petals, one per named
        // stop, the caret's own among them — no longer one blob lerped
        // half-way to white. Read on the spawn frame, where the head is
        // mid-glass: at the landing (column 0) the petals on the left are
        // off the glass and dropped, never clamped. Six, not seven, because
        // the nearest-anchor classifier folds indigo into violet.
        assert!(coma(&sc.halos).is_some(), "the corona is drawn at T");
        let mut petals = [false; 7];
        for h in sc.halos.iter().filter(|h| is_chromatic(h.color)) {
            petals[stop_of(h.color)] = true;
        }
        let n = petals.iter().filter(|p| **p).count();
        assert!(
            n >= 6,
            "the corona carries {n} stops, not the spectrum: {petals:?}"
        );
        assert!(petals[want_stop], "the corona lacks the caret's own stop");

        // The arrival frame: the whole path is lit and the landing is minted.
        let land = ctx_at(t0 + spawn.t_flight, &cfg, to, caret_t);
        sc.emit(&mut m, &land);

        // The pin is the caret's reflected stop — `arc_t(t_land, 0)`.
        let pin = m
            .landings
            .first()
            .expect("the arrival minted a landing")
            .pin;
        assert!(
            (pin.t - want_t).abs() < 1e-6,
            "the pin took {}, not tri(caret_t) = {want_t}",
            pin.t
        );

        // The station under the caret IS the caret's stop: the chromatic
        // train quad nearest the landing cell's centre wears
        // `spectrum(tri(caret_t))`.
        let (x_land, _) = land.geom.cell_center(to.0, to.1);
        let cw = land.geom.cw as f32;
        let (dist, at_caret) = sc
            .under
            .iter()
            .filter(|q| is_chromatic(q.color))
            .map(|q| {
                (
                    (f32::from(q.x) + f32::from(q.w) * 0.5 - x_land).abs(),
                    q.color,
                )
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("the arrival frame has a colour train");
        assert!(
            dist <= cw,
            "no train station within a cell of the landing (nearest is {dist} px away)"
        );
        assert_eq!(
            stop_of(at_caret),
            want_stop,
            "the station under the caret wears {:#08x}, not the caret's own stop {want:#08x}",
            at_caret & 0x00FF_FFFF
        );

        // And the arrival train — 48 cells, 1.33 t-units of the reflected
        // walk — is a whole rainbow, not a bar.
        let mut seen = [false; 7];
        for q in &sc.under {
            let (r, g, b) = chan(q.color);
            if r.max(g).max(b) >= 16 && is_chromatic(q.color) {
                seen[stop_of(q.color)] = true;
            }
        }
        let n = seen.iter().filter(|s| **s).count();
        assert!(
            n >= 5,
            "the arrival train showed {n} stops, not 5: {seen:?}"
        );
    }

    // -- §3.2, the luminance hierarchy --------------------------------------

    /// **§3.2 rank 1.** The brightest thing in the frame is the smallest
    /// thing: the white nucleus out-shines every train pixel, and it LEADS the
    /// train — the ionisation peak is just BEHIND the head, never in front of
    /// it.
    #[test]
    fn the_brightest_point_is_the_nucleus_and_it_leads_the_train() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m
            .on_event(&mv((5, 0), (5, 80)), t0, &ctx)
            .expect("must fly");
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);

        let (_, _, peak) = nucleus_cx(&sc.halos).expect("no nucleus");
        for q in sc.out.iter().chain(sc.under.iter()) {
            assert!(
                lum(q.color) <= peak + 1.0,
                "a train quad at {} out-shone the nucleus ({} > {peak})",
                q.x,
                lum(q.color)
            );
        }

        // The white layer's brightest column is BEHIND the head, inside the
        // shoulder, and is spent by 0.35·L. Measured on the ARRIVAL frame,
        // where the whole path is lit and 0.35·L is a real place on it.
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (5, 80), 0.25);
        sc.emit(&mut m, &land);
        let (x0, _) = geom().cell_center(5, 0);
        let (x1, _) = geom().cell_center(5, 80);
        let l = x1 - x0;
        let mut best_x = 0.0_f32;
        let mut best = 0.0_f32;
        let mut far = 0.0_f32;
        for q in &sc.out {
            let x = f32::from(q.x);
            let v = lum(q.color);
            if v > best {
                best = v;
                best_x = x;
            }
            // "…and is ≤ 0.10 of that peak BY 0.35·L": everything from 0.35·L
            // outward, measured on the slab's own CENTRE. Reading the slab's
            // left EDGE instead would sample a slab that mostly lies nearer
            // the head than 0.35·L and report the train brighter than it is
            // there — an oracle biased toward the answer it is checking.
            let centre = x + f32::from(q.w) * 0.5;
            if x1 - centre >= 0.35 * l {
                far = far.max(v);
            }
        }
        assert!(
            (x1 - SHOULDER_SHARE * l - 10.0..=x1 + 1.0).contains(&best_x),
            "the white peak is at {best_x}, not in the shoulder behind the head at {x1}"
        );
        assert!(far > 0.0, "nothing was sampled at 0.35·L");
        assert!(
            far <= 0.10 * best,
            "the white layer is still at {far} of {best} by 0.35·L — it must be ≤ 10 %"
        );
    }

    /// **§20.1 `the_train_is_brightest_just_behind_the_head`, the colour
    /// half.** `cov(0.61·L) / cov(0.06·L) = e⁻¹ ± 0.05`, and the colour layer
    /// is monotone non-increasing beyond the shoulder — no 118 → 135 step one
    /// shoulder-length behind the head on a graded flight. The request each
    /// station made is recovered from the premultiplied quad through the arc
    /// law (public, pinned above): `cov = max_ch(quad) · 255 / max_ch(stop)`.
    #[test]
    fn the_train_is_brightest_just_behind_the_head() {
        let cfg = config();
        let t0 = Instant::now();
        let t_land = 1.0 / 3.0;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), t_land);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (5, 80), t_land);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &land);

        let (x0, _) = geom().cell_center(5, 0);
        let (x1, _) = geom().cell_center(5, 80);
        let l = x1 - x0;
        // The request each STATION made, read off the polyline the frame
        // was built from (`build_stations`) rather than recovered from the
        // rasterized quads: since 2026-09-08 the arc runs `arc_gain` times
        // faster, and a slab lerped in RGB between two stations across the
        // green → blue crossing has a max channel a fifth under the LUT's
        // at the same `t`, which read as a "brightening" that no station
        // asked for. The rasterizer's own lerp of `cov` is linear and
        // monotone between monotone stations, so the stations are the law.
        let f = Flight::of(&m.live[0], &land).expect("the arrival frame flies");
        let mut verts = Vec::new();
        build_stations(&mut verts, &m.live[0], &f, &land, Layer::Colour);
        assert!(verts.len() > 8, "too few stations to read: {}", verts.len());
        // Station `s` behind the head → its request.
        let cols: BTreeMap<i32, f32> = verts
            .iter()
            .map(|v| ((x1 - v.x).round() as i32, v.cov))
            .collect();
        let at = |s: f32| -> f32 {
            let si = s.round() as i32;
            cols.range(si - 4..=si + 4)
                .map(|(_, c)| *c)
                .fold(0.0_f32, f32::max)
        };
        // One falloff length past the shoulder: `0.06·L + 0.55·L` since
        // 2026-09-08 (§6.5 wrote `0.41·L` at the 0.35 falloff).
        let far_share = SHOULDER_SHARE + COLOUR_FALLOFF_SHARE;
        let near = at(SHOULDER_SHARE * l);
        let far = at(far_share * l);
        assert!(
            near > 0.0 && far > 0.0,
            "nothing sampled at 0.06·L / {far_share}·L"
        );
        let ratio = far / near;
        let e1 = (-1.0_f32).exp();
        assert!(
            (ratio - e1).abs() <= 0.05,
            "cov({far_share}L)/cov(0.06L) = {far}/{near} = {ratio}, not e⁻¹ = {e1} ± 0.05"
        );
        // Beyond the shoulder the request never rises again (s ascending);
        // one level of rounding slack.
        let mut floor = f32::INFINITY;
        for (&si, &c) in &cols {
            let s = si as f32;
            if s < SHOULDER_SHARE * l {
                continue;
            }
            assert!(
                c <= floor + 1.0,
                "the colour train brightens at s = {s}: {c} after a floor of {floor}"
            );
            floor = floor.min(c);
        }
    }

    /// **§6.8 / C3.** Two taus, alpha only: the white dies FIRST (τ 90) and
    /// the colour outlives it (τ 150) — and on glass the white also LEAVES
    /// first, closing into the caret on the pin's clock (`WHITE_CLOSE_MS`),
    /// so the train turns white → spectrum with no chroma change at all. And
    /// no coloured pixel is ever grey — chroma is held constant and the alpha
    /// is culled instead.
    #[test]
    fn the_train_dies_white_first_and_is_never_grey() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m
            .on_event(&mv((5, 0), (5, 80)), t0, &ctx)
            .expect("must fly");
        let arrival = t0 + spawn.t_flight;

        // Probe a window 48-72 px BEHIND the landing: past the pin's 0.6 `ch`
        // arms and past the ring's whole radius, so what is measured is the
        // train and only the train. Both layers are sampled at the same `s`,
        // so the stop's own luma cancels out of the ratio.
        // Since 2026-09-08 the shockwave (3 `ch`) and the sparks cross this
        // window too: the white probe reads ACHROMATIC `out` quads only, and
        // the colour probe the train's own stream.
        let (x1, _) = geom().cell_center(5, 80);
        let probe = |v: &[GlowQuad], white: bool| {
            v.iter()
                .filter(|q| (x1 - 72.0..=x1 - 48.0).contains(&f32::from(q.x)))
                .filter(|q| !white || is_white(q.color))
                .map(|q| lum(q.color))
                .fold(0.0_f32, f32::max)
        };

        let mut ratios = Vec::new();
        for after in [0_u64, 100, 180] {
            let mut sc = Scratch::default();
            let at = ctx_at(arrival + ms(after), &cfg, (5, 80), 0.25);
            sc.emit(&mut m, &at);
            let white = probe(&sc.out, true);
            let colour = probe(&sc.under, false);
            assert!(colour > 0.0, "the colour train vanished at T + {after}");
            // The white is still in the window at T + 100 (its extent has
            // closed to ≈ half, its alpha to a third); by T + 180 it has left
            // the window for the caret while the colour is still there.
            if after < WHITE_CLOSE_MS as u64 {
                assert!(white > 0.0, "the white train vanished at T + {after}");
            } else {
                assert!(
                    white == 0.0,
                    "the white is still on the train at T + {after}, {white} — it must have \
                     closed into the caret with the pin"
                );
            }

            // C3: fade moves ALPHA, never chroma. The floor is the arc's own
            // LEAST saturated authored colour — the green→blue crossing roof
            // (`SPECTRUM_CROSSING_ROOF`, `S 0.53`); the seven named stops are
            // `S 1.0`. Grey is `S → 0`, so a bar at `S 0.35` sits below
            // everything the colour law can produce and above everything a
            // fade-to-grey would, with room for the premultiply's rounding.
            for q in &sc.under {
                let (r, g, b) = chan(q.color);
                let hi = r.max(g).max(b);
                let lo = r.min(g).min(b);
                if hi >= 24 {
                    let sat = (hi - lo) as f32 / hi as f32;
                    assert!(
                        sat >= 0.35,
                        "a train pixel went grey at T + {after}: {r},{g},{b} (S {sat})"
                    );
                }
            }
            ratios.push(white / colour);
        }
        assert!(
            ratios[0] > ratios[1] && ratios[1] > ratios[2],
            "the white must die FIRST — white/colour ratios were {ratios:?}"
        );
        assert!(
            ratios[2] < 0.55 * ratios[0],
            "by T + 180 the white is barely half the ratio it had on arrival: {ratios:?}"
        );
        assert!(
            ratios[1] < 0.75 * ratios[0],
            "by T + 100 the white has not visibly left the train: {ratios:?}"
        );
    }

    /// **§3.2, D3 / C3 for the halos.** "A white core below α 0.20 is culled,
    /// never left as a grey speck; nothing coloured is drawn below α 0.12."
    /// The nucleus goes on the white layer's floor (`T + 145`), the coma on
    /// the chroma floor (`T + 466`), and at no frame of the fade is either a
    /// speck under its floor.
    #[test]
    fn the_nucleus_and_coma_are_culled_on_their_own_floors() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        let at = |sc: &mut Scratch, m: &mut Meteors, after: u64| {
            let c = ctx_at(arrival + ms(after), &cfg, (5, 40), 0.25);
            sc.emit(m, &c);
        };

        at(&mut sc, &mut m, 0);
        // The landing's sparks are hashed at the mint: the copy names every
        // spark halo's centre on every later frame (`is_spark_halo`).
        let l0 = m.landings[0];
        let peak_w = nucleus(&sc.halos)
            .map(|h| lum(h.color))
            .expect("nucleus at T");
        let peak_c = coma(&sc.halos)
            .map(|h| {
                let (r, g, b) = chan(h.color);
                r.max(g).max(b) as f32
            })
            .expect("coma at T");

        // α_w = e^(−140/90) = 0.211 is drawn; e^(−150/90) = 0.189 is culled.
        at(&mut sc, &mut m, 140);
        assert!(
            nucleus(&sc.halos).is_some(),
            "the nucleus is gone before α 0.20"
        );
        at(&mut sc, &mut m, 150);
        assert!(
            nucleus(&sc.halos).is_none(),
            "a white speck survives under α 0.20"
        );
        // α_c = e^(−460/220) = 0.124 is drawn; e^(−470/220) = 0.118 is
        // culled (τ 220 since 2026-09-08; §6.8 wrote 290/319 at τ 150). The
        // corona leaves WITH the last train pixel — the colour root reaches
        // the landing at `T + 460` (§6.8's R) — which is on or before its
        // own chroma floor at `T + 466`, never after it.
        let cull = (TRAIN_COLOUR_TAU_MS * RETIRE_DECAY) as u64;
        at(&mut sc, &mut m, cull - 16);
        assert!(
            coma_of(&sc.halos, &[l0], arrival + ms(cull - 16)).is_some(),
            "the coma is gone before α 0.12"
        );
        at(&mut sc, &mut m, cull + 4);
        assert!(
            coma_of(&sc.halos, &[l0], arrival + ms(cull + 4)).is_none(),
            "a coloured smudge survives under α 0.12"
        );

        // And no frame of the fade shows a halo under its own floor. The
        // corona's seven petals share one alpha, so the chroma floor is
        // read on the brightest of them (an indigo petal's max channel is
        // half a red one's at the same alpha, by the stop's own colour).
        let mut after = 0;
        while after <= FLIGHT_OFF_GLASS_MS as u64 {
            at(&mut sc, &mut m, after);
            let mut best_c = None;
            for h in &sc.halos {
                // A spark's halo is on its own law — drawn only while the
                // spark holds ≥ 0.6, dimmed halo-first — not the corona's.
                if is_spark_halo(h, &[l0], arrival + ms(after)) {
                    continue;
                }
                if is_white(h.color) {
                    assert!(
                        lum(h.color) >= STAR_CULL_ALPHA * peak_w - 1.0,
                        "T + {after}: a white halo at {} is a speck under {}",
                        lum(h.color),
                        STAR_CULL_ALPHA * peak_w
                    );
                } else {
                    let (r, g, b) = chan(h.color);
                    let hi = r.max(g).max(b) as f32;
                    best_c = Some(best_c.map_or(hi, |b: f32| b.max(hi)));
                }
            }
            if let Some(hi) = best_c {
                assert!(
                    hi >= CHROMA_CULL_ALPHA * peak_c - 1.0,
                    "T + {after}: the corona is under the chroma cull ({hi} of {peak_c})"
                );
            }
            after += 8;
        }
    }

    /// **§6.5 layer 8.** The terminal flash pops the NUCLEUS radius to ×1.7 at
    /// 40 ms and relaxes it by 120 ms — as area, never coverage — and the coma
    /// is the steady glow the flash happens inside of: it does not pop.
    #[test]
    fn the_flash_pops_the_nucleus_and_not_the_coma() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        let mut radii = |sc: &mut Scratch, after: u64| -> (f32, f32, f32) {
            let c = ctx_at(arrival + ms(after), &cfg, (5, 40), 0.25);
            sc.emit(&mut m, &c);
            let n = nucleus(&sc.halos).expect("nucleus");
            let k = coma(&sc.halos).expect("coma");
            (f32::from(n.rx), lum(n.color), f32::from(k.rx))
        };
        let (n0, cov0, c0) = radii(&mut sc, 0);
        let (n40, cov40, c40) = radii(&mut sc, 40);
        let (n120, _, _) = radii(&mut sc, 120);
        assert!(
            n40 > n0 && n40 >= FLASH_PEAK * n0 - 1.0,
            "the nucleus did not pop: rx {n0} → {n40} at T + 40"
        );
        assert!(
            n120 < n40,
            "the flash did not relax by 120 ms: {n40} → {n120}"
        );
        assert!(c40 <= c0, "the coma popped with the flash: rx {c0} → {c40}");
        assert!(
            cov40 <= cov0,
            "the flash bought COVERAGE ({cov0} → {cov40}); it is area, and the cap is a cap"
        );
    }

    // -- §6.2 as re-ruled 2026-09-08, off the glass --------------------------

    /// **THE IMPACT'S HORIZON** (owner, 2026-09-08: "be a bigger more special
    /// rainbow impact!"). §6.2's `T + 320` was restraint; the impact now
    /// stays on glass PAST `T + 400` — the shockwave, the falling sparks and
    /// the rainbow's root still leaving — and is off, every stream, by
    /// `T + 600` at every published distance, with the pool at rest so the
    /// host may idle (T6). Before the re-ruling the first clause fails:
    /// nothing was on glass at `T + 400`.
    #[test]
    fn the_impact_is_off_glass_by_t_plus_600() {
        let cfg = config();
        for cols in [8_u16, 34, 80] {
            let t0 = Instant::now();
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, cols), 0.25);
            let spawn = m.on_event(&mv((5, 0), (5, cols)), t0, &ctx).expect("fly");
            let arrival = t0 + spawn.t_flight;
            let mut sc = Scratch::default();
            // Drive the whole life so every edge is spent on a real frame.
            let mut t = t0;
            while t <= arrival + ms(400) {
                let at = ctx_at(t, &cfg, (5, cols), 0.25);
                sc.emit(&mut m, &at);
                t += ms(8);
            }
            // Still an impact at T + 400: coloured points above the row (the
            // sparks) and the train's rainbow still on it.
            let at = ctx_at(arrival + ms(400), &cfg, (5, cols), 0.25);
            sc.emit(&mut m, &at);
            let (_, ly) = geom().cell_center(5, cols);
            let small = |q: &GlowQuad| i32::from(q.w) <= SPARK_PX && i32::from(q.h) <= SPARK_PX;
            let sparks = sc
                .out
                .iter()
                .filter(|q| is_chromatic(q.color) && small(q))
                .count();
            assert!(
                sparks > 0 && !sc.under.is_empty(),
                "{cols} cells: the impact is already gone at T + 400 ({sparks} sparks, {} under)",
                sc.under.len()
            );
            assert!(
                sc.out.iter().any(|q| is_chromatic(q.color)
                    && small(q)
                    && (f32::from(q.y) - ly).abs() >= 4.0),
                "{cols} cells: no spark has left the row by T + 400"
            );
            while t <= arrival + ms(600) {
                let at = ctx_at(t, &cfg, (5, cols), 0.25);
                sc.emit(&mut m, &at);
                t += ms(8);
            }
            for after in [600_u64, 650] {
                let at = ctx_at(arrival + ms(after), &cfg, (5, cols), 0.25);
                sc.emit(&mut m, &at);
                assert!(
                    sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty(),
                    "{cols} cells: meteor light survived to T + {after} ms"
                );
            }
            assert!(m.at_rest(), "{cols} cells: the pool never came to rest");
            assert!(
                m.next_change_deadline(arrival + ms(650)).is_none(),
                "{cols} cells: a resting pool still named a deadline (T6)"
            );
        }
    }

    /// **THE FLIGHT DID NOT MOVE; THE RELEASE DID** (owner, 2026-09-08). The
    /// attack is responsiveness and is pinned exactly where §8.1 put it —
    /// `flight_ms` 8 → 60, 40 → 90, 80 → 120, the head at `enter-at-speed`
    /// of it on the first frame — while the pool's horizon, `end − arrival`,
    /// is now ≥ 600 ms. Before the re-ruling the last clause fails at 320.
    #[test]
    fn the_flight_speed_is_unchanged() {
        use super::super::timing::flight_ms;
        assert!((flight_ms(8.0) - 60.0).abs() < 1e-4, "8 cells → 60 ms");
        assert!((flight_ms(40.0) - 90.0).abs() < 1e-4, "40 cells → 90 ms");
        assert!((flight_ms(80.0) - 120.0).abs() < 1e-4, "80 cells → 120 ms");
        let cfg = config();
        let t0 = Instant::now();
        for cols in [8_u16, 40, 80] {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, cols), 0.25);
            let spawn = m.on_event(&mv((5, 0), (5, cols)), t0, &ctx).expect("fly");
            let t_ms = spawn.t_flight.as_secs_f32() * 1000.0;
            assert!(
                (t_ms - flight_ms(f32::from(cols))).abs() < 1e-3,
                "{cols} cells: T = {t_ms}, not flight_ms"
            );
            let (x0, _) = geom().cell_center(5, 0);
            let (x1, _) = geom().cell_center(5, cols);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(t0 + ms(8), &cfg, (5, cols), 0.25));
            let (cx, _, _) = nucleus_cx(&sc.halos).expect("the head at +8 ms");
            let want = x0 + enter_at_speed(8.0 / t_ms) * (x1 - x0) + NUCLEUS_LEAD_PX;
            assert!(
                (cx - want).abs() <= 1.5,
                "{cols} cells: the head is at {cx} at +8 ms, not {want}"
            );
            let release = m.live[0]
                .end()
                .saturating_duration_since(m.live[0].arrival())
                .as_secs_f32()
                * 1000.0;
            assert!(
                (release - FLIGHT_OFF_GLASS_MS).abs() < 1.0 && release >= 600.0,
                "{cols} cells: the release is {release} ms — the impact was not enlarged"
            );
        }
    }

    /// **THE TRAIN IS THE RAINBOW** (owner, 2026-09-08: "I want the meteor
    /// to have rainbow!"). On every frame of the flight after the first the
    /// colour train on glass runs from red to violet — its colours reach
    /// within 8 % of both ends of the spectrum table and touch at least five
    /// named stops — and on frame 0 (an 8-cell hop's is 20 px, ten slabs)
    /// at least four stops; at 8, 40 and 80 cells, in both directions, over
    /// a dead prompt, behind a fresh run and at a late phase. Six of the
    /// seven stops by name is what the eye sees; the nearest-anchor
    /// classifier folds indigo into violet (its max-channel-normalised
    /// colour is nearer) and misses red unless a slab lands within 6 % of
    /// the reflection, which is why the law is spelled on the table's span.
    /// Before the re-ruling an 8-cell hop's train spanned 6 % of the table
    /// on every frame (two stops) and a 40-cell one's frame 0 spanned 37 %.
    #[test]
    fn the_train_carries_the_whole_spectrum_on_every_frame_of_the_flight() {
        use crate::spectrum::{SPECTRUM_LUT, SPECTRUM_LUT_LEN};
        let cfg = config();
        // The nearest table entry to a premultiplied quad colour.
        let lut_index = |c: u32| -> usize {
            let (r, g, b) = chan(c);
            let hi = r.max(g).max(b).max(1) as f32;
            let n = |v: u32| v as f32 * 255.0 / hi;
            let (nr, ng, nb) = (n(r), n(g), n(b));
            let mut best = 0;
            let mut near = f32::INFINITY;
            for (i, &a) in SPECTRUM_LUT.iter().enumerate() {
                let (ar, ag, ab) = chan(a);
                let d =
                    (nr - ar as f32).powi(2) + (ng - ag as f32).powi(2) + (nb - ab as f32).powi(2);
                if d < near {
                    near = d;
                    best = i;
                }
            }
            best
        };
        let (red_end, violet_end) = (
            (0.08 * (SPECTRUM_LUT_LEN - 1) as f32) as usize,
            (0.92 * (SPECTRUM_LUT_LEN - 1) as f32) as usize,
        );
        for t_land in [0.0_f32, 9.0 / 36.0, 0.6] {
            for cols in [8_u16, 40, 80] {
                for (from, to) in [((5_u16, 0_u16), (5_u16, cols)), ((5, cols), (5, 0))] {
                    let t0 = Instant::now();
                    let mut m = Meteors::new();
                    let ctx = ctx_at(t0, &cfg, to, t_land);
                    let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("fly");
                    let mut sc = Scratch::default();
                    let mut t = t0;
                    let mut frame = 0;
                    while t <= t0 + spawn.t_flight {
                        sc.emit(&mut m, &ctx_at(t, &cfg, to, t_land));
                        let mut seen = [false; 7];
                        let (mut lo, mut hi) = (usize::MAX, 0_usize);
                        for q in &sc.under {
                            let (r, g, b) = chan(q.color);
                            if r.max(g).max(b) >= 16 && is_chromatic(q.color) {
                                seen[stop_of(q.color)] = true;
                                let i = lut_index(q.color);
                                lo = lo.min(i);
                                hi = hi.max(i);
                            }
                        }
                        let n = seen.iter().filter(|s| **s).count();
                        // Frame 0 of an 8-cell hop is 20 px of train — ten
                        // slabs over a unit and a half, which from a late
                        // phase folds on the reflection: four stops on the
                        // worst frame 0 (measured), five and the whole span
                        // from frame 1.
                        let want = if frame == 0 { 4 } else { 5 };
                        assert!(
                            n >= want,
                            "t_land {t_land}, {from:?}->{to:?}, frame {frame}: the train showed \
                             {n} stops, not the rainbow: {seen:?}"
                        );
                        if frame > 0 {
                            assert!(
                                lo <= red_end && hi >= violet_end,
                                "t_land {t_land}, {from:?}->{to:?}, frame {frame}: the train \
                                 spans table entries {lo}..{hi}, not red to violet"
                            );
                        }
                        t += ms(8);
                        frame += 1;
                    }
                    assert!(frame >= 8, "{cols} cells: only {frame} frames were checked");
                }
            }
        }
    }

    /// **THE SHOCKWAVE** (owner, 2026-09-08: "a bigger more special rainbow
    /// impact"). The landing ring, drawn alone, reaches at least TWICE the
    /// 2026-09-05 ring's 1.3 `ch` along the row, and its circumference
    /// carries at least six of the seven stops — a rainbow shockwave, not
    /// a small pop. Before the re-ruling the reach is 1.3 `ch`.
    #[test]
    fn the_landing_is_a_rainbow_shockwave_twice_the_old_reach() {
        let cfg = config();
        let t0 = Instant::now();
        let ch = geom().ch as f32;
        for (from, to) in [((5_u16, 0_u16), (5_u16, 40_u16)), ((5, 40), (5, 0))] {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("fly");
            let arrival = t0 + spawn.t_flight;
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(arrival, &cfg, to, 0.25));
            let l = m.landings[0];
            // The ring at the top of its expansion, drawn by itself so the
            // sparks and the pin cannot stand in for it.
            let mut reach = 0.0_f32;
            let mut seen = [false; 7];
            let mut after = 0_u64;
            while after <= RING_MS as u64 {
                sc.clear();
                let at = ctx_at(arrival + ms(after), &cfg, to, 0.25);
                let mut fr = sc.frame();
                draw_ring(&l, &at, &mut fr);
                for q in sc.out.iter().filter(|q| is_chromatic(q.color)) {
                    let (r, g, b) = chan(q.color);
                    if r.max(g).max(b) >= 24 {
                        seen[stop_of(q.color)] = true;
                        let x = f32::from(q.x) + f32::from(q.w) * 0.5;
                        reach = reach.max((x - l.x).abs());
                    }
                }
                after += 8;
            }
            let want = 2.0 * RING_R_CH_2026_09_05 * ch;
            assert!(
                reach >= want,
                "{from:?}->{to:?}: the ring reaches {reach} px along the row, under twice the \
                 old {want} px"
            );
            let n = seen.iter().filter(|s| **s).count();
            assert!(
                n >= 6,
                "{from:?}->{to:?}: the ring carried {n} stops, not the spectrum: {seen:?}"
            );
        }
    }

    /// **THE FAN'S GRADE, READ OFF THE FUNCTION** (LAW 2, 2026-09-08: m15's
    /// look is what the grade scales). The const asserts on `FAN_N_*` and
    /// `FAN_REACH_*` constrain the CONSTANTS in the `(impact − 1)` form; this
    /// pin reads the EXPRESSION the mint path uses, which is how a mis-fit —
    /// `… * impact` under `(impact − 1)` constants, 23 stars and 4.4 `ch` at
    /// the floor, saturated by ≈ 30 cells — went unseen by 1671 greens.
    /// The 8-cell floor is m15's second-round landing byte-for-byte (19
    /// stars, 2.8 `ch`); the cap (impact 3.5, ≥ 48 cells) is 28 stars and
    /// 6.8 `ch`; the doc's table holds at 16 / 24 / 40 cells; both laws are
    /// monotone in distance; a party adds exactly eight under `FAN_MAX_N`.
    #[test]
    fn fan_count_and_reach_are_the_graded_law_at_floor_and_cap() {
        use crate::rainbow_kitty::timing::{FAN_MAX_N, IMPACT_MAX, JUMP_MIN_CELLS, impact};
        let floor = f32::from(JUMP_MIN_CELLS);
        assert!((impact(floor) - 1.0).abs() < 1e-6, "the floor is impact 1");
        assert!(
            (impact(64.0) - IMPACT_MAX).abs() < 1e-6,
            "64 cells is the cap"
        );

        // The floor: m15's second-round look, byte-identical.
        assert_eq!(fan_count(floor, false), 19, "19 stars at the 8-cell floor");
        assert!(
            (fan_reach_ch(floor, 0.0) - FAN_REACH_BASE_CH).abs() < 1e-6,
            "2.8 ch at the floor"
        );
        // Under the floor the grade clamps to 1 — the same landing.
        assert_eq!(fan_count(3.0, false), 19);
        assert!((fan_reach_ch(3.0, 0.0) - FAN_REACH_BASE_CH).abs() < 1e-6);

        // The cap: the ceilings, exactly.
        assert_eq!(fan_count(64.0, false), 28, "28 stars from the cap");
        assert!(
            (fan_reach_ch(64.0, 0.0) - FAN_REACH_MAX_CH).abs() < 1e-6,
            "6.8 ch from the cap"
        );
        assert_eq!(fan_count(1e6, false), 28);

        // The documented table (§6.5 layer 11; `RAINBOW-KITTY-V2.md` row 11).
        assert_eq!(fan_count(16.0, false), 21);
        assert_eq!(fan_count(24.0, false), 23);
        assert_eq!(fan_count(40.0, false), 27);
        for (cells, want) in [(16.0_f32, 3.8_f32), (24.0, 4.7), (40.0, 6.1)] {
            let got = fan_reach_ch(cells, 0.0);
            assert!(
                (got - want).abs() < 0.06,
                "{cells} cells: reach {got} ch, documented {want}"
            );
        }

        // Monotone in distance, both laws, over the whole graded range.
        let mut prev_n = 0;
        let mut prev_r = 0.0_f32;
        for cells in (1..=80).map(|c| c as f32) {
            let n = fan_count(cells, false);
            let r = fan_reach_ch(cells, 0.0);
            assert!(n >= prev_n, "{cells} cells: count {n} fell under {prev_n}");
            assert!(
                r >= prev_r - 1e-6,
                "{cells} cells: reach {r} fell under {prev_r}"
            );
            prev_n = n;
            prev_r = r;
        }

        // A party adds FAN_PARTY_ADD, under the sky's FAN_MAX_N; the grade
        // term lifts the reach ceiling and nothing else.
        assert_eq!(fan_count(floor, true), 19 + FAN_PARTY_ADD);
        assert_eq!(fan_count(64.0, true), (28 + FAN_PARTY_ADD).min(FAN_MAX_N));
        assert!(
            (fan_reach_ch(64.0, 1.0) - FAN_REACH_MAX_CH).abs() < 1e-6,
            "the ceiling binds only above the cap's own reach"
        );
        assert!((fan_reach_ch(floor, 1.0) - FAN_REACH_BASE_CH).abs() < 1e-6);
    }

    // -- D15, the vertical arm ----------------------------------------------

    /// **D15.** `ribbon_beam` is x-major and `continue`s any segment with
    /// `|Δx| < 1e-3`, so a vertical recall drawn through it emits ZERO quads —
    /// the mark is not thin, not dim, it is ABSENT. The transposed twin is the
    /// fix, and this is its pin. And a nav-licensed recall is a meteor in
    /// full: D8 reserves the ring for "same-row nav and history recall", and
    /// §12.3 gives vertical recall the rain, which pairs with the fan's hero.
    #[test]
    fn a_vertical_recall_meteor_draws_its_train() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (8, 40), 0.25);
        let spawn = m
            .on_event(&mv((5, 40), (8, 40)), t0, &ctx)
            .expect("a 3-row recall must fly");
        assert!(spawn.ring, "D8: a history recall rings");
        assert!(spawn.rain, "§12.3 / D6: a history recall rains on its hero");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(
            !sc.under.is_empty(),
            "D15: the vertical train drew NOTHING — the x-major arm was taken"
        );

        // By the arrival edge every crossed row carries train light, and the
        // landing rings.
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (8, 40), 0.25);
        sc.emit(&mut m, &land);
        for row in 5_u16..=8 {
            assert!(
                sc.under.iter().any(|q| q.row == row),
                "row {row} was crossed but carries no train quad"
            );
        }
        assert!(
            m.landings.iter().any(|l| l.ring.is_some()),
            "a recall's landing minted no ring"
        );
    }

    // -- §6.9, retire-previous ----------------------------------------------

    /// **§6.9.** A new meteor whose corridor comes within 1 `ch` of a live one
    /// RETIRES it: the older train's colour jumps to its post-arrival fade
    /// with `R = 60 ms` and reaches the chroma cull exactly there. Never a pop
    /// (the alpha it decays from is the alpha it had), never a fade in place.
    /// And the pool is capped at 2 heads under any mash, on a FIXED pool that
    /// never grows (§18).
    #[test]
    fn a_second_meteor_retires_the_first_without_a_pop() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 80), 0.25);
        m.on_event(&mv((5, 0), (5, 80)), t0, &a).expect("E flies");
        let before = m.live[0].colour_alpha(t0 + ms(30));

        let b = ctx_at(t0 + ms(30), &cfg, (5, 0), 0.25);
        m.on_event(&mv((5, 80), (5, 0)), t0 + ms(30), &b)
            .expect("A flies");
        let first = m.live[0];
        assert!(
            first.retiring(),
            "the crossing meteor did not retire the first"
        );
        assert!(
            (first.colour_alpha(t0 + ms(30)) - before).abs() < 1e-3,
            "the retire POPPED: alpha jumped at the edge"
        );

        let mut last = f32::INFINITY;
        for at in [30_u64, 45, 60, 75, 90] {
            let v = first.colour_alpha(t0 + ms(at));
            assert!(v <= last + 1e-4, "the retired train brightened at +{at} ms");
            last = v;
        }
        assert!(
            first.colour_alpha(t0 + ms(90)) <= CHROMA_CULL_ALPHA,
            "the retired train is still above the cull at +90 ms"
        );

        // Cap 2 under a mash of ten (v1 allowed four — "rulers piling"), on
        // a pool that was reserved once and never reallocates.
        let cap = m.live.capacity();
        for i in 0..10_u64 {
            let now = t0 + ms(100 + i * 5);
            let (from, to) = if i % 2 == 0 {
                ((5_u16, 0_u16), (5_u16, 80_u16))
            } else {
                ((5, 80), (5, 0))
            };
            let c = ctx_at(now, &cfg, to, 0.25);
            let _ = m.on_event(&mv(from, to), now, &c);
            let flying = m.live.iter().filter(|x| !x.retiring()).count();
            assert!(
                flying <= FLIGHT_MAX_LIVE,
                "{flying} heads in the air at once"
            );
            assert_eq!(m.live(), flying, "`live()` counts finishing trains");
            assert!(
                m.live.len() <= METEOR_POOL,
                "the pool grew to {} under a mash",
                m.live.len()
            );
        }
        assert_eq!(
            m.live.capacity(),
            cap,
            "the meteor pool reallocated under a mash"
        );
    }

    /// **§6.9, the shape of the finish.** A retired train "jumps to its
    /// post-arrival fade with `R = 60`": the head FREEZES where it was on the
    /// retire edge and the root RETRACTS toward it over the 60 ms, so the
    /// light leaves toward the caret (T5) — never a fade-in-place, never a
    /// phantom head still extending the span — and at `R` it is gone.
    #[test]
    fn a_retired_train_retracts_toward_its_frozen_head_and_is_gone_at_sixty() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &a).expect("E flies");
        // A short crossing flight whose own train lives at x ≥ 630 px, so the
        // retired train (x < 500) is measured alone.
        let retire = t0 + ms(30);
        let b = ctx_at(retire, &cfg, (5, 70), 0.25);
        m.on_event(&mv((5, 80), (5, 70)), retire, &b)
            .expect("the crossing flies");
        assert!(m.live[0].retiring());

        let (x0, _) = geom().cell_center(5, 0);
        let (x1, _) = geom().cell_center(5, 80);
        let t_ms = spawn.t_flight.as_secs_f32() * 1000.0;
        let frozen = x0 + enter_at_speed(30.0 / t_ms) * (x1 - x0);

        let mut sc = Scratch::default();
        let mut extent = |sc: &mut Scratch, after: u64| -> Option<(f32, f32)> {
            let c = ctx_at(retire + ms(after), &cfg, (5, 70), 0.25);
            sc.emit(&mut m, &c);
            assert!(
                sc.under.iter().any(|q| f32::from(q.x) >= 600.0),
                "the crossing flight went dark at +{after} ms"
            );
            let mine: Vec<_> = sc
                .under
                .iter()
                .filter(|q| f32::from(q.x) + f32::from(q.w) <= 500.0)
                .collect();
            let lo = mine
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            let hi = mine
                .iter()
                .map(|q| f32::from(q.x) + f32::from(q.w))
                .fold(0.0_f32, f32::max);
            (!mine.is_empty()).then_some((lo, hi))
        };

        let (lo0, hi0) = extent(&mut sc, 0).expect("the retired train is lit on the edge");
        let (lo1, hi1) = extent(&mut sc, 15).expect("lit at +15");
        let (lo2, hi2) = extent(&mut sc, 29).expect("lit at +29");
        assert!(
            (hi0 - frozen).abs() <= 12.0,
            "the head did not freeze on the retire edge: {hi0} vs {frozen}"
        );
        assert!(
            hi1 <= hi0 + 1.0 && hi2 <= hi0 + 1.0,
            "a phantom head kept extending the retired span: {hi0} → {hi1} → {hi2}"
        );
        assert!(
            lo0 < lo1 && lo1 < lo2,
            "the root did not retract toward the head: {lo0} → {lo1} → {lo2} (fade-in-place)"
        );
        assert!(
            extent(&mut sc, 60).is_none(),
            "the retired train is still on glass at R = 60 ms"
        );
    }

    /// **§6.9 / §19.1.** A head that was handed over sheds nothing more: no
    /// fragment is born behind a phantom head, because light with no gesture
    /// behind it is the class v2 deletes.
    #[test]
    fn a_retired_head_sheds_nothing_more() {
        let cfg = config();
        let t0 = Instant::now();
        let run = |retire: bool| -> (u8, u8) {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
            m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("E flies");
            let mut sc = Scratch::default();
            let edge = t0 + ms(8);
            sc.emit(&mut m, &ctx_at(edge, &cfg, (5, 80), 0.25));
            let early = m.live[0].shed;
            if retire {
                let b = ctx_at(edge, &cfg, (5, 70), 0.25);
                m.on_event(&mv((5, 80), (5, 70)), edge, &b)
                    .expect("the crossing flies");
                assert!(m.live[0].retiring());
            }
            for after in [12_u64, 30, 45, 59] {
                sc.emit(&mut m, &ctx_at(edge + ms(after), &cfg, (5, 70), 0.25));
            }
            (early, m.live[0].shed)
        };
        let all = (1_u8 << shed_n(80.0)) - 1;
        let (early, late) = run(false);
        assert_ne!(
            early, all,
            "every station was already born at +8 ms — nothing to test"
        );
        assert_eq!(late, all, "a live head passes every station by +67 ms");
        let (early_r, late_r) = run(true);
        assert_eq!(early_r, early, "the two runs diverged before the retire");
        assert_eq!(
            late_r, early_r,
            "a retired head shed {:#b} → {:#b}",
            early_r, late_r
        );
    }

    /// **§6.9, chaining.** A same-direction continuation from within a cell
    /// of a live head CHAINS: the old train is cut at its head and finishes
    /// on its own law, and the continuation flies its OWN segment — so the
    /// one head on glass is never behind where it just was. (Inheriting the
    /// old launch would put the new head at `p₀` of a path already flown:
    /// an 18-cell jump backwards on the chain frame.)
    #[test]
    fn a_chained_continuation_never_puts_the_head_behind_where_it_was() {
        let cfg = config();
        let t0 = Instant::now();
        let (x0, _) = geom().cell_center(5, 0);
        let (x40, _) = geom().cell_center(5, 40);

        // Chain on the arrival edge.
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 40), 0.25);
        let s1 = m.on_event(&mv((5, 0), (5, 40)), t0, &a).expect("flies");
        let at = t0 + s1.t_flight;
        let b = ctx_at(at, &cfg, (5, 80), 0.25);
        m.on_event(&mv((5, 40), (5, 80)), at, &b)
            .expect("the continuation flies");
        assert_eq!(m.live.len(), 2);
        assert!(m.live[0].chained && m.live[0].retired_at.is_none());
        assert!(!m.live[1].chained);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &b);
        let (cx, _, _) = nucleus_cx(&sc.halos).expect("one head");
        assert!(
            cx >= x40 - 1.0,
            "the chained head appeared at {cx}, behind the old head at {x40}"
        );
        let heads: std::collections::BTreeSet<u16> = sc
            .halos
            .iter()
            .filter(|h| is_white(h.color))
            .map(|h| h.cx)
            .collect();
        assert_eq!(
            heads.len(),
            1,
            "two heads in the air after a chain: {heads:?}"
        );
        assert!(
            m.landings.iter().all(|l| (l.x - x40).abs() > 1.0),
            "the handed-over head celebrated a landing the caret left"
        );

        // Chain mid-flight: the old train is cut at the head's position on
        // the chain edge, and its arrival edge becomes that edge.
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 40), 0.25);
        let s1 = m.on_event(&mv((5, 0), (5, 40)), t0, &a).expect("flies");
        let at = t0 + ms(30);
        let t_ms = s1.t_flight.as_secs_f32() * 1000.0;
        let hx = x0 + enter_at_speed(30.0 / t_ms) * (x40 - x0);
        let from = (5_u16, ((hx - x0) / geom().cw as f32).round() as u16);
        let b = ctx_at(at, &cfg, (5, 70), 0.25);
        m.on_event(&mv(from, (5, 70)), at, &b)
            .expect("the continuation flies");
        let old = m.live[0];
        assert!(
            old.chained,
            "a continuation within a cell of the head must chain"
        );
        assert!(
            (old.x1 - hx).abs() <= 1.0,
            "the old train was not cut at its head"
        );
        assert_eq!(
            old.t_flight,
            ms(30),
            "the old arrival edge is the chain edge"
        );
        sc.emit(&mut m, &b);
        let (cx, _, _) = nucleus_cx(&sc.halos).expect("one head");
        assert!(
            cx >= hx - 1.0,
            "the head jumped backwards on the chain frame"
        );
    }

    /// **§6.9, a head at rest.** A train that has LANDED has no head to hand
    /// over and nothing in the air to stop. A same-direction recall launched
    /// from its landing 100 ms later does not chain it — chaining marked it
    /// headless, and its still-visible coma popped off the glass on the
    /// spawn frame — it retires it on the corridor rule, and the arrival's
    /// afterglow, coma and nucleus, leaves with the train over the 60 ms
    /// from exactly the coverage it had: on glass on the spawn frame,
    /// dimmer at +30, gone by `R`.
    #[test]
    fn a_landed_train_keeps_its_afterglow_when_a_continuation_launches_from_it() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (8, 40), 0.25);
        let s1 = m
            .on_event(&mv((5, 40), (8, 40)), t0, &a)
            .expect("a three-row recall flies");
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(t0 + s1.t_flight, &cfg, (8, 40), 0.25));
        assert!(
            m.live[0].landed && !m.landings.is_empty(),
            "the fixture must land first"
        );
        let at = t0 + ms(100);
        assert!(
            at > t0 + s1.t_flight,
            "the continuation must launch AFTER the landing"
        );
        let l0 = m.landings[0];

        // The afterglow: the halos centred on the old landing (the nucleus
        // leads the head by 2 px along the path), as `(coma, nucleus)`
        // premultiplied peaks.
        // The corona's petals orbit the nucleus (2026-09-08), so "here" is
        // the corona's reach around it, not one pixel.
        let (lx, ly) = geom().cell_center(8, 40);
        // A spark thrown by the old landing can be inside that reach too
        // (second round) — the shower is not the afterglow.
        let glow = |sc: &Scratch, now: Instant| -> (Option<f32>, Option<f32>) {
            let here = |h: &&RainHalo| {
                (f32::from(h.cx) - lx).abs() <= 8.0
                    && (f32::from(h.cy) - (ly + NUCLEUS_LEAD_PX)).abs() <= 8.0
                    && !is_spark_halo(h, &[l0], now)
            };
            let peak = |chroma: bool| {
                sc.halos
                    .iter()
                    .filter(here)
                    .filter(|h| {
                        if chroma {
                            is_chromatic(h.color)
                        } else {
                            is_white(h.color)
                        }
                    })
                    .map(|h| lum(h.color))
                    .reduce(f32::max)
            };
            (peak(true), peak(false))
        };
        sc.emit(&mut m, &ctx_at(at, &cfg, (8, 40), 0.25));
        let (coma0, core0) = glow(&sc, at);
        let coma0 = coma0.expect("the landed train's coma is on glass 40 ms after T");
        let core0 = core0.expect("the landed train's nucleus is on glass 40 ms after T");

        // A twelve-row continuation: its own head is 60 px down the path on
        // the spawn frame, clear of the old landing's corona.
        let b = ctx_at(at, &cfg, (20, 40), 0.25);
        m.on_event(&mv((8, 40), (20, 40)), at, &b)
            .expect("the continuation flies");
        let old = m.live[0];
        assert!(
            !old.chained,
            "a landed train was chained: it had no head to hand over"
        );
        assert!(
            old.retiring(),
            "§6.9: the continuation's corridor retires the landed train"
        );
        assert!(!old.handed_over(), "a head at rest is not off the glass");

        sc.emit(&mut m, &b);
        let (coma1, core1) = glow(&sc, at);
        assert_eq!(
            coma1,
            Some(coma0),
            "the landed train's coma popped on the spawn frame"
        );
        assert_eq!(
            core1,
            Some(core0),
            "the landed train's nucleus popped on the spawn frame"
        );
        // The continuation's own head is the brightest thing, and ahead.
        let (_, ncy, _) = nucleus_cx(&sc.halos).expect("the continuation's head");
        assert!(
            ncy > ly + 10.0,
            "the continuation's head is not ahead of the old landing: {ncy} vs {ly}"
        );

        sc.emit(&mut m, &ctx_at(at + ms(30), &cfg, (20, 40), 0.25));
        let (coma2, _) = glow(&sc, at + ms(30));
        let coma2 = coma2.expect("the afterglow is still leaving at +30 ms");
        assert!(
            coma2 < coma0,
            "the afterglow did not decay: {coma2} vs {coma0}"
        );
        let r = ms(FLIGHT_RETIRE_MS as u64);
        sc.emit(&mut m, &ctx_at(at + r, &cfg, (20, 40), 0.25));
        let (coma3, core3) = glow(&sc, at + r);
        assert!(
            coma3.is_none() && core3.is_none(),
            "the afterglow outlived R = 60 ms"
        );
    }

    /// **§6.9's cap, with a chain.** A chained stub is one streak's remnant,
    /// not a third meteor: when a continuation makes three un-retired trains
    /// on glass, the stub takes the 60 ms finish — never the head in flight
    /// on another row, which a plain "oldest first" evicted whenever it
    /// happened to be older.
    #[test]
    fn a_chained_stub_never_costs_another_row_its_head() {
        let cfg = config();
        let t0 = Instant::now();
        let (x0, _) = geom().cell_center(5, 0);
        let (x40, _) = geom().cell_center(5, 40);
        let mut m = Meteors::new();
        // B, the OLDER flight, on row 7.
        let b = ctx_at(t0, &cfg, (7, 40), 0.25);
        m.on_event(&mv((7, 0), (7, 40)), t0, &b).expect("B flies");
        // A on row 5, 5 ms later …
        let a_at = t0 + ms(5);
        let a = ctx_at(a_at, &cfg, (5, 40), 0.25);
        let s_a = m.on_event(&mv((5, 0), (5, 40)), a_at, &a).expect("A flies");
        assert_eq!(m.live(), FLIGHT_MAX_LIVE);
        // … and A′ chains A from within a cell of its head, 30 ms into its
        // flight.
        let at = a_at + ms(30);
        let t_ms = s_a.t_flight.as_secs_f32() * 1000.0;
        let hx = x0 + enter_at_speed(30.0 / t_ms) * (x40 - x0);
        let from = (5_u16, ((hx - x0) / geom().cw as f32).round() as u16);
        let c = ctx_at(at, &cfg, (5, 70), 0.25);
        m.on_event(&mv(from, (5, 70)), at, &c).expect("A′ flies");
        let (old_b, old_a, new_a) = (m.live[0], m.live[1], m.live[2]);
        assert!(old_a.chained, "A′ did not chain A");
        assert!(
            old_a.retiring(),
            "the chained stub is the third train on glass and takes the 60 ms finish"
        );
        assert!(
            !old_b.retiring(),
            "B — a head in flight on another row — was evicted by A's own continuation"
        );
        assert!(!new_a.retiring(), "the continuation itself was retired");
        assert_eq!(
            m.live(),
            FLIGHT_MAX_LIVE,
            "two live meteors after the chain"
        );
        // And B's head is still drawn on that frame, on its own row.
        let mut sc = Scratch::default();
        sc.emit(&mut m, &c);
        let (_, y7) = geom().cell_center(7, 40);
        assert!(
            sc.halos
                .iter()
                .any(|h| is_white(h.color) && (f32::from(h.cy) - y7).abs() <= 1.0),
            "B's nucleus is gone from row 7"
        );
    }

    /// **§6.9's last sentence.** "Only one head is ever in the air per row":
    /// two same-row flights whose segments never come near each other still
    /// share the row, and the older head is retired — unless it has already
    /// landed, in which case there is no head to retire and its train
    /// finishes on its own law.
    #[test]
    fn only_one_head_is_in_the_air_per_row() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 10), 0.25);
        m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
        let b = ctx_at(t0 + ms(10), &cfg, (5, 70), 0.25);
        m.on_event(&mv((5, 60), (5, 70)), t0 + ms(10), &b)
            .expect("flies");
        assert!(m.live[0].retiring(), "two heads in the air on one row");
        assert_eq!(m.live(), 1);

        // Two rows apart is two heads: the corridor rule alone decides.
        let mut m = Meteors::new();
        m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
        let c = ctx_at(t0 + ms(10), &cfg, (7, 70), 0.25);
        m.on_event(&mv((7, 60), (7, 70)), t0 + ms(10), &c)
            .expect("flies");
        assert_eq!(m.live(), 2, "flights on different rows retired each other");

        // A train already landed has no head in the air.
        let mut m = Meteors::new();
        let s = m.on_event(&mv((5, 0), (5, 10)), t0, &a).expect("flies");
        let later = t0 + s.t_flight + ms(10);
        let d = ctx_at(later, &cfg, (5, 70), 0.25);
        m.on_event(&mv((5, 60), (5, 70)), later, &d).expect("flies");
        assert!(
            !m.live[0].retiring(),
            "a landed train was retired by a distant newcomer"
        );
    }

    /// **§18 / §20.1's `a_ping_pong_never_blanks_the_ribbon`.** Each meteor's
    /// `under` share is hard-capped at [`UNDER_QUAD_CAP`], so two live
    /// meteors plus a full ribbon land exactly on `MAX_QUADS` and the ribbon
    /// is never the thing that sheds.
    #[test]
    fn a_ping_pong_never_spends_more_than_its_under_share() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let a = ctx_at(t0, &cfg, (5, 119), 1.0);
        m.on_event(&mv((5, 0), (5, 119)), t0, &a).expect("E flies");
        let b = ctx_at(t0 + ms(20), &cfg, (5, 0), 1.0);
        m.on_event(&mv((5, 119), (5, 0)), t0 + ms(20), &b)
            .expect("A flies");

        let mut sc = Scratch::default();
        let mut t = t0 + ms(20);
        while t <= t0 + ms(200) {
            let at = ctx_at(t, &cfg, (5, 0), 1.0);
            sc.emit(&mut m, &at);
            // Two trains and, past their arrival edges, two splashes
            // (2026-09-08) — each under its own cap.
            let share = FLIGHT_MAX_LIVE * UNDER_QUAD_CAP + LANDING_POOL * SPLASH_QUAD_CAP;
            assert!(
                sc.under.len() <= share,
                "two meteors spent {} under quads — over their {share} share",
                sc.under.len()
            );
            t += ms(8);
        }
    }

    // -- §6.11, reduced motion; §6.12, the sub-floor hop ---------------------

    /// **§6.11.** Reduced motion is STATIC: the head is at the landing from
    /// the first frame, the train is the exponential profile at α 0.85, and
    /// the landing is the FLASH AND THE COLOURS with no expansion (second
    /// round, 2026-09-08): a landing IS minted, and what it draws at
    /// `T + 100` and `T + 300` is byte-identical — no pin arms (a hairline
    /// quad), no sparks (a halo away from the head), the shockwave and the
    /// bands at their full geometry from the first frame. The fan is still
    /// minted, and it stays where it was born. Before the second round the
    /// landing was not minted at all under reduced motion.
    #[test]
    fn reduced_motion_draws_the_static_form() {
        let mut cfg = config();
        cfg.reduced_motion = true;
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(!sc.under.is_empty(), "the static train must still be drawn");
        let (cx, _, _) = nucleus_cx(&sc.halos).expect("the static head must be drawn");
        let (x1, ly) = geom().cell_center(5, 80);
        assert!(
            (cx - x1).abs() <= NUCLEUS_LEAD_PX + 2.0,
            "reduced motion put the head at {cx}, not at the landing {x1} — it flew"
        );

        // Past the arrival edge: the static landing — flash and colours, no
        // motion — and the fan is static.
        let land = ctx_at(t0 + spawn.t_flight + ms(4), &cfg, (5, 80), 0.25);
        sc.emit(&mut m, &land);
        assert_eq!(
            m.landings.len(),
            1,
            "reduced motion did not mint the static landing (the flash and the colours)"
        );
        let mut dust = sky_around(5);
        m.sow_into(&mut dust);
        // The landing's OWN layers, drawn alone: the static train and head
        // beside them keep their own linear fades under reduced motion, so a
        // whole-frame comparison would read the train's spend as the
        // landing's motion.
        let l = m.landings[0];
        let snapshot = |sc: &mut Scratch, after: u64| {
            sc.clear();
            let at = ctx_at(t0 + spawn.t_flight + ms(after), &cfg, (5, 80), 0.25);
            let mut fr = sc.frame();
            let mut verts = Vec::new();
            draw_splash(&mut verts, &l, &at, &mut fr);
            draw_flash(&l, &at, &mut fr);
            draw_ring(&l, &at, &mut fr);
            draw_sparks(&l, &at, &mut fr, SPARK_HALO_CAP);
            (sc.under.clone(), sc.out.clone(), sc.halos.clone())
        };
        let a = snapshot(&mut sc, 100);
        let b = snapshot(&mut sc, 300);
        assert!(!a.1.is_empty(), "the static landing drew nothing");
        assert!(
            a.0 == b.0 && a.1 == b.1 && a.2 == b.2,
            "the reduced-motion landing MOVED between T + 100 and T + 300"
        );
        assert!(
            a.2.iter().all(|h| !is_chromatic(h.color)),
            "a spark's halo (motion) was drawn under reduced motion"
        );
        sc.clear();
        {
            let at = ctx_at(t0 + spawn.t_flight + ms(100), &cfg, (5, 80), 0.25);
            let mut fr = sc.frame();
            draw_pin(&l, &at, &mut fr);
        }
        assert!(
            sc.out.is_empty() && sc.halos.is_empty(),
            "the pin (motion) was drawn under reduced motion"
        );
        // The flash: cell-sized white and coloured quads on the landing
        // row, held; the bands in the sky, at full reach from the first.
        let g = geom();
        let cell = |q: &GlowQuad| usize::from(q.w) == g.cw && usize::from(q.h) == g.ch;
        assert!(
            a.1.iter().any(|q| cell(q) && is_white(q.color))
                && a.1.iter().any(|q| cell(q) && is_chromatic(q.color)),
            "the static flash is not white and colour"
        );
        let ch = g.ch as f32;
        assert!(
            a.1.iter()
                .any(|q| f32::from(q.y) + f32::from(q.h) <= ly - 0.5 * ch)
                && a.1.iter().any(|q| f32::from(q.y) >= ly + 0.5 * ch),
            "the static splash is not in the sky above and below the row"
        );
        let fan = lane(&dust, StarLane::Fan);
        assert!(!fan.is_empty(), "reduced motion dropped the (static) fan");
        let later = land.now + ms(400);
        for s in &fan {
            let birth = (s.x.round() as i32, s.y.round() as i32);
            assert_eq!(
                s.pos(later, land.cfg.reduced_motion),
                birth,
                "the reduced-motion fan is thrown, not static"
            );
        }
        assert!(
            fan.iter()
                .any(|s| s.pos(later, false) != (s.x.round() as i32, s.y.round() as i32)),
            "the oracle is vacuous: the same stars would not move under motion either"
        );
    }

    /// **§6.12 / D17.** A 2-7-cell same-row hop is a MINI-FAN and not a
    /// flight: 1 m2 + 2 m3 in the mini-fan lane at the landing, no train, no
    /// ring, no flight at all. A full fan would break scrubbing calm; one
    /// lonely wink would be a step down from "more variance and fun on the
    /// landing".
    #[test]
    fn a_word_hop_is_a_mini_fan_not_a_flight() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 25), 0.25);
        assert!(
            m.on_event(&mv((5, 20), (5, 25)), t0, &ctx).is_none(),
            "a 5-cell hop must not mint a flight"
        );
        assert_eq!(m.live(), 0, "a 5-cell hop put a meteor in the air");

        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx);
        assert!(sc.under.is_empty(), "a mini-fan drew a train");
        assert!(m.landings.is_empty(), "a mini-fan rang");

        let mut dust = sky();
        m.sow_into(&mut dust);
        let stars = lane(&dust, StarLane::MiniFan);
        let m2 = stars.iter().filter(|s| s.class == StarClass::M2).count();
        let m3 = stars.iter().filter(|s| s.class == StarClass::M3).count();
        assert_eq!((m2, m3), (1, 2), "the mini-fan is 1 m2 + 2 m3 (D17)");
        assert_eq!(dust.live(), 3, "a mini-fan star escaped its lane");
        let (lx, ly) = geom().cell_center(5, 25);
        for s in &stars {
            assert!(
                (s.x - lx).abs() <= 2.0 && (s.y - ly).abs() <= 2.0,
                "a mini-fan star was born away from the landing cell"
            );
        }

        // Scrubbing stays calm: one cell mints nothing at all.
        let mut dust = sky();
        assert!(m.on_event(&mv((5, 25), (5, 26)), t0, &ctx).is_none());
        sc.emit(&mut m, &ctx);
        m.sow_into(&mut dust);
        assert_eq!(dust.live(), 0, "a one-cell scrub minted light");
    }

    /// **D6 / §6.5 layer 11.** Every landing carries exactly one gold m1 and
    /// four m2 — the five the rain glints pair with, in throw order — and the
    /// rest are grains. D8's Enter landing is the one fork: sized by rows, one
    /// m2 + 5-7 m3, NO hero, no ring — and the same census from column 5 as
    /// from column 40.
    #[test]
    fn the_landing_fan_is_one_hero_four_m2_and_grains() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
        let mut sc = Scratch::default();
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (5, 40), 0.25);
        sc.emit(&mut m, &land);
        let mut dust = sky();
        m.sow_into(&mut dust);
        let fan = lane(&dust, StarLane::Fan);
        assert_eq!(
            fan.iter().filter(|s| s.class == StarClass::M1).count(),
            1,
            "a landing carries exactly one hero"
        );
        assert_eq!(
            fan.iter().filter(|s| s.class == StarClass::M2).count(),
            4,
            "a landing carries exactly four m2 — the rain's other four glints"
        );
        assert!(
            fan.iter().filter(|s| s.class == StarClass::M3).count() >= 1,
            "and the rest are grains"
        );
        assert!(
            fan.iter()
                .all(|s| s.class != StarClass::M1 || s.tint == TINT_GOLD_RGB),
            "the hero is gold"
        );
        assert_eq!(
            usize::from(m.landings[0].fan.n),
            fan.len(),
            "the count the meteor decided"
        );

        // D8: an Enter lands small, from column 40 and from column 5 alike.
        for col in [40_u16, 5] {
            let mut m2 = Meteors::new();
            let ctx2 = ctx_at(t0, &cfg, (6, 0), 0.25);
            let s2 = m2
                .on_event(&mv_as((5, col), (6, 0), Licence::Return), t0, &ctx2)
                .expect("an Enter flies");
            assert!(!s2.ring && !s2.rain, "D8: an Enter never rings or rains");
            let mut sc2 = Scratch::default();
            let land2 = ctx_at(t0 + s2.t_flight, &cfg, (6, 0), 0.25);
            sc2.emit(&mut m2, &land2);
            let mut dust2 = sky();
            m2.sow_into(&mut dust2);
            let fan2 = lane(&dust2, StarLane::Fan);
            assert!(
                fan2.iter().all(|s| s.class != StarClass::M1),
                "D8: an Enter landing has no hero (from column {col})"
            );
            assert_eq!(
                fan2.iter().filter(|s| s.class == StarClass::M2).count(),
                1,
                "D8: an Enter landing carries one m2 (from column {col})"
            );
            let grains = fan2.iter().filter(|s| s.class == StarClass::M3).count();
            assert!(
                (ENTER_LANDING_M3_MIN..=ENTER_LANDING_M3_MAX).contains(&grains),
                "D8: an Enter landing carries 5-7 grains, not {grains} (from column {col})"
            );
            assert!(
                m2.landings.iter().all(|l| l.ring.is_none()),
                "D8: an Enter landing must never ring"
            );
        }
    }

    /// **THE VERTICAL REACH LAW (§6.5 layer 11, measured on glass 2026-09-05,
    /// defect 6a).** The fan's reach is sized by PATH LENGTH — 5.2 ch on a
    /// 44-cell jump — and a radial throw of that size put stars 60-75 px
    /// above the top text row, in the tab strip, on every long jump from a
    /// mid-screen row. Distance buys length ALONG THE LINE (§6.3's spirit):
    /// every fan star comes to rest within `FAN_RISE_MAX_CH` (1.6 ch) of the
    /// landing row's centre on an 8-, 40- and 80-cell jump alike, the census
    /// (1 gold m1 + 4 m2, D6) is untouched — a ray that would exceed the cap
    /// is flattened, never dropped — and the long jumps still spread along
    /// the line past the cap.
    #[test]
    fn a_mid_screen_landing_fan_stays_within_two_rows_of_its_line() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let ch = g.ch as f32;
        let cap = (FAN_RISE_MAX_CH * ch).round() as i32;
        for cells in [8_u16, 40, 80] {
            let landing = (5_u16, cells);
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, landing, 0.25);
            let spawn = m.on_event(&mv((5, 0), landing), t0, &ctx).expect("fly");
            let arrival = t0 + spawn.t_flight;
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(arrival, &cfg, landing, 0.25));
            let mut dust = sky();
            m.sow_into(&mut dust);
            let fan = lane(&dust, StarLane::Fan);
            assert_eq!(
                fan.iter()
                    .filter(|s| s.class == StarClass::M1 && s.gold)
                    .count(),
                1,
                "{cells} cells: the gold hero"
            );
            assert_eq!(
                fan.iter().filter(|s| s.class == StarClass::M2).count(),
                4,
                "{cells} cells: its four m2"
            );
            let (cx, cy) = g.cell_center(5, cells);
            let (cx, cy) = (cx.round() as i32, cy.round() as i32);
            let mut along = 0;
            for s in &fan {
                let (rx, ry) = s.rest();
                let rise = (ry - cy).abs();
                along = along.max((rx - cx).abs());
                assert!(
                    rise <= cap,
                    "a {cells}-cell landing's {:?} fan star rests {rise} px off its line at \
                     ({rx}, {ry}) — the cap is {cap} px (1.6 ch)",
                    s.class
                );
            }
            if cells >= 40 {
                assert!(
                    along > cap,
                    "a {cells}-cell landing's fan spreads only {along} px along the line — \
                     distance no longer buys length"
                );
            }
        }
    }

    /// **§5.4 / THE VERTICAL REACH LAW ON ROW 0.** Row 0's sky is the
    /// `Geom.head` band the host opened for effects (the v0.40 effects-layer
    /// law): a fan landing on row 0 may rise into the band — and does — but
    /// no star may come to rest above `fx_top()`, the band's own top, where
    /// the tab strip is, AND none above the 1.6 ch ceiling every other row
    /// keeps: the band is the ribbon's sky, not room for a three-row fan
    /// (the confirmation capture's 61–86 px spill on the prompt row). Under
    /// a 40 px band (2.2 ch) a 40-cell landing on row 0 puts at least one
    /// star in the band, none above `fx_top()` and none above the ceiling;
    /// with NO band every star rests inside the grid — re-aimed, not born
    /// off the glass, so the census holds either way.
    #[test]
    fn a_row_zero_fan_may_use_the_head_band_but_never_the_tab_strip_beyond_it() {
        let cfg = config();
        let t0 = Instant::now();
        let landing = (0_u16, 40_u16);
        for head in [40_u16, 0] {
            let at = |now: Instant| {
                let mut c = ctx_at(now, &cfg, landing, 0.25);
                c.geom.head = head;
                c.geom.origin_y = head;
                c
            };
            let mut m = Meteors::new();
            let spawn = m.on_event(&mv((0, 0), landing), t0, &at(t0)).expect("fly");
            let land = at(t0 + spawn.t_flight);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &land);
            let mut dust = sky();
            m.sow_into(&mut dust);
            let fan = lane(&dust, StarLane::Fan);
            assert_eq!(
                fan.iter()
                    .filter(|s| s.class == StarClass::M1 && s.gold)
                    .count(),
                1,
                "head {head}: the gold hero"
            );
            assert_eq!(
                fan.iter().filter(|s| s.class == StarClass::M2).count(),
                4,
                "head {head}: its four m2"
            );
            let g = land.geom;
            let (top, grid_top) = (g.fx_top(), i32::from(g.origin_y));
            let cap = (FAN_RISE_MAX_CH * g.ch as f32).round() as i32;
            let (_, cy) = g.cell_center(0, 40);
            let cy = cy.round() as i32;
            let mut in_band = 0;
            for s in &fan {
                let (rx, ry) = s.rest();
                assert!(
                    ry >= top,
                    "head {head}: a {:?} fan star rests at ({rx}, {ry}), above the effects \
                     box's top {top} — in the tab strip",
                    s.class
                );
                let rise = (ry - cy).abs();
                assert!(
                    rise <= cap,
                    "head {head}: a {:?} fan star rests {rise} px off row 0's line at \
                     ({rx}, {ry}) — the cap is {cap} px (1.6 ch) on row 0 too",
                    s.class
                );
                in_band += usize::from(ry < grid_top);
            }
            if head > 0 {
                assert!(in_band >= 1, "head {head}: no fan star used the head band");
            } else {
                assert_eq!(in_band, 0, "head 0: a fan star rests above the grid");
            }
        }
    }

    /// **§6.5 layer 11 / §6.12, the throw delivered.** The meteor hands the
    /// sky a `reach` in px and the sky owns the window: once that window has
    /// elapsed every fan star sits at `reach·(0.55..1.05)` from the landing,
    /// and every mini-fan star has moved its 1-2 px. (The shipped v1 of this
    /// seam divided by one window and integrated over another, and the fan
    /// reached a quarter of its reach.)
    #[test]
    fn the_fan_reaches_its_reach_and_the_mini_fan_moves() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (5, 40), 0.25);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &land);
        let mut dust = sky();
        m.sow_into(&mut dust);
        let reach = m.landings[0].fan.reach;
        assert!(reach > 40.0, "a 40-cell landing reaches {reach} px?");
        let window = StarLane::Fan.throw_ms().expect("the fan is a throw");
        let done = land.now + Duration::from_secs_f32(window / 1000.0) + ms(1);
        let half = land.now + Duration::from_secs_f32(window / 2000.0);
        let fan = lane(&dust, StarLane::Fan);
        assert!(!fan.is_empty());
        for s in &fan {
            let birth = (s.x.round() as i32, s.y.round() as i32);
            let d =
                |p: (i32, i32)| (((p.0 - birth.0).pow(2) + (p.1 - birth.1).pow(2)) as f32).sqrt();
            let full = d(s.pos(done, false));
            assert!(
                (FAN_THROW_JITTER_MIN * reach - 1.0..=FAN_THROW_JITTER_MAX * reach + 1.0)
                    .contains(&full),
                "a {:?} fan star reached {full} px of a {reach} px reach",
                s.class
            );
            let mid = d(s.pos(half, false));
            assert!(
                mid < full && mid > 0.5 * full,
                "the throw is not an ease-out: {mid} of {full}"
            );
        }

        let mut m = Meteors::new();
        let hop = ctx_at(t0, &cfg, (5, 25), 0.25);
        assert!(m.on_event(&mv((5, 20), (5, 25)), t0, &hop).is_none());
        sc.emit(&mut m, &hop);
        let mut dust = sky();
        m.sow_into(&mut dust);
        let window = StarLane::MiniFan
            .throw_ms()
            .expect("the mini-fan is a throw");
        let done = t0 + Duration::from_secs_f32(window / 1000.0) + ms(1);
        let stars = lane(&dust, StarLane::MiniFan);
        assert_eq!(stars.len(), 3);
        for s in &stars {
            let (bx, by) = (s.x.round() as i32, s.y.round() as i32);
            let (px, py) = s.pos(done, false);
            let moved = (px - bx).abs().max((py - by).abs());
            assert!(
                (1..=3).contains(&moved),
                "a mini-fan star moved {moved} px, not its 1-2 (D17)"
            );
        }
    }

    /// **§5.4 / L4, at the seam.** Every star the meteor decides is BUILT by
    /// the sky through its clearance, and the clearance is on where a core
    /// comes to REST — the end of its throw — not on where it is born. A fan
    /// is born on the landing cell, which is the caret's own and
    /// ledger-exempt (§3.4, §6.5 layer 9): a Home/End onto a glyph still
    /// throws its hero and four m2 (D6 pairs the five rain glints with them,
    /// and a landing with no stars leaves the rain nothing to ride), and a
    /// core whose rest would be on a probed glyph is nudged ≤ 2 px or
    /// dropped. Fed explicitly — a glyph AT the landing cell, a glyph beside
    /// it, a row of ink around it, and no probe at all (free transients fly
    /// over unprobed rows: the asymmetry §5.4 draws between a ribbon cell's
    /// sky and open air) — and the same law for the mini-fan: Alt-B onto a
    /// word's first glyph still winks.
    #[test]
    fn a_fan_star_core_never_lands_on_a_probed_glyph() {
        let cfg = config();
        let t0 = Instant::now();
        let landing = (5_u16, 40_u16);
        let g = geom();
        let window = StarLane::Fan.throw_ms().expect("the fan is a throw");
        // One landing into `dust`: every fan core's REST cell is asserted
        // clear, and the census is returned.
        let census = |dust: &mut Stardust| -> usize {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, landing, 0.25);
            let spawn = m.on_event(&mv((5, 0), landing), t0, &ctx).expect("fly");
            let arrival = t0 + spawn.t_flight;
            let land = ctx_at(arrival, &cfg, landing, 0.25);
            let mut sc = Scratch::default();
            sc.emit(&mut m, &land);
            m.sow_into(dust);
            let done = arrival + Duration::from_secs_f32(window / 1000.0) + ms(1);
            for s in lane(dust, StarLane::Fan) {
                let (rx, ry) = s.pos(done, false);
                assert_ne!(
                    dust.probe().at_px(rx, ry, g),
                    Some(true),
                    "a {:?} fan core came to rest on a probed glyph at ({rx}, {ry})",
                    s.class
                );
            }
            assert!(!m.landings.is_empty(), "the pin and ring need no clearance");
            lane(dust, StarLane::Fan).len()
        };
        let probed = |ink: &[bool; 120]| {
            let mut dust = Stardust::new();
            dust.probe_mut().probe_row(i32::from(landing.0), ink);
            dust
        };

        // A glyph AT the landing cell: the birth pixel is the caret's own.
        let mut inked = [false; 120];
        inked[usize::from(landing.1)] = true;
        let on_ink = census(&mut probed(&inked));
        assert!(
            on_ink >= FAN_HERO_N,
            "a glyph AT the landing cut the fan to {on_ink} stars — the hero and its four m2 \
             are gone, and the five rain glints have nothing to pair with (D6)"
        );

        // A glyph BESIDE it.
        let mut beside = [false; 120];
        beside[usize::from(landing.1) + 1] = true;
        let full = census(&mut probed(&beside));
        assert!(
            full >= FAN_HERO_N,
            "a glyph BESIDE the landing thinned the fan"
        );
        assert_eq!(on_ink, full, "ink under the caret thinned the fan");

        // A row of ink AROUND the landing: every core that would come to rest
        // on it is nudged or dropped, and the rest of the fan is still thrown.
        let mut row = [true; 120];
        row[usize::from(landing.1)] = false;
        let boxed = census(&mut probed(&row));
        assert!(
            boxed >= 1 && boxed <= full,
            "a row of ink around the landing left {boxed} of {full} stars"
        );

        // No probe at all: open air.
        let mut dust = Stardust::new();
        assert!(dust.probe().is_empty());
        assert_eq!(
            census(&mut dust),
            full,
            "an unprobed row refused a free transient"
        );

        // The mini-fan obeys the same law: a hop onto a glyph still winks.
        let mut dust = probed(&inked);
        let mut m = Meteors::new();
        let hop = ctx_at(t0, &cfg, landing, 0.25);
        assert!(m.on_event(&mv((5, 45), landing), t0, &hop).is_none());
        let mut sc = Scratch::default();
        sc.emit(&mut m, &hop);
        m.sow_into(&mut dust);
        assert_eq!(
            lane(&dust, StarLane::MiniFan).len(),
            3,
            "Alt-B onto a word's first glyph minted no mini-fan (§6.12)"
        );
    }

    /// **§6.5 layer 5, and `ShedSow::at_px`'s own promise.** A shed fragment
    /// is born AT ITS STATION — the point on the path the head passed — and
    /// wears the train's own stop there, `spectrum_snap(t_m(L − s))`. The
    /// station's variance is the meteor's hashed `s_i ∈ [0.10, 0.70]·L` and
    /// nothing else: a second, speed-scaled offset on the sky's side put
    /// fragments up to eight cells behind their station — behind the LAUNCH
    /// cell on a short flight, light where the meteor never was — wearing a
    /// stop the train never wore there.
    #[test]
    fn a_shed_fragment_is_born_at_its_station_wearing_the_train_colour_there() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let cw = g.cw as f32;
        for cells in [80_u16, 8] {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, cells), 0.25);
            let spawn = m
                .on_event(&mv((5, 0), (5, cells)), t0, &ctx)
                .expect("flies");
            let mut sc = Scratch::default();
            let mut dust = sky();
            // Frame 0 — where the head is fastest, and a speed-scaled offset
            // would be largest — then a frame past which every station has
            // been passed.
            let t_ms = spawn.t_flight.as_secs_f32() * 1000.0;
            for at_ms in [0.0_f32, 0.35 * t_ms] {
                let now = t0 + Duration::from_secs_f32(at_ms / 1000.0);
                sc.emit(&mut m, &ctx_at(now, &cfg, (5, cells), 0.25));
                m.sow_into(&mut dust);
            }
            let m0 = m.live[0];
            let l = m0.length();
            let (x0, y0) = g.cell_center(5, 0);
            let n = usize::from(shed_n(m0.cells)).min(8);
            assert_eq!(
                m0.shed,
                (1_u8 << n) - 1,
                "{cells} cells: not every station was passed by 0.35·T"
            );
            let stations: Vec<f32> = (0..n)
                .map(|i| {
                    let share =
                        SHED_S_MIN + (SHED_S_MAX - SHED_S_MIN) * unit01(m0.seed, 0x5ED0 ^ i as u32);
                    x0 + share * l
                })
                .collect();
            let shed = lane(&dust, StarLane::Shed);
            assert_eq!(
                shed.len(),
                n,
                "{cells} cells: {n} stations, {} fragments",
                shed.len()
            );
            let mut claimed = vec![false; n];
            for s in &shed {
                let (i, sx) = stations
                    .iter()
                    .copied()
                    .enumerate()
                    .min_by(|a, b| (a.1 - s.x).abs().total_cmp(&(b.1 - s.x).abs()))
                    .expect("stations");
                assert!(
                    (s.x - sx).abs() <= 2.0 && (s.y - y0).abs() <= 2.0,
                    "{cells} cells: a fragment was born {:.1} px along and {:.1} px across from \
                     its station at x = {sx:.1} (the clearance nudge is 2 px)",
                    s.x - sx,
                    s.y - y0
                );
                assert!(
                    (x0 + SHED_S_MIN * l - 2.0..=x0 + SHED_S_MAX * l + 2.0).contains(&s.x),
                    "{cells} cells: a fragment at x = {} is outside the 0.10..0.70·L window",
                    s.x
                );
                assert!(
                    !claimed[i],
                    "{cells} cells: two fragments at station {i}; the meteor's law is one"
                );
                claimed[i] = true;
                let want = spectrum_snap(m0.arc((l - (sx - x0)) / cw));
                assert_eq!(
                    s.tint, want,
                    "{cells} cells: the fragment at station {i} wears {:#08x}; the train wears \
                     {want:#08x} there",
                    s.tint
                );
            }
        }
    }

    /// **Seam point 12, at the arrival edge.** A scroll between spawn and
    /// arrival moves the whole flight — and the landing is the flight's END:
    /// the pin, the ring and the fan are minted at the scrolled endpoint, on
    /// the row the train now ends on, never re-derived from the spawn-time
    /// cell `rows` rows below it (a PTY line-feed cascade, D18, is exactly a
    /// flight plus a scroll inside one 60-120 ms window). A scroll that
    /// carries the endpoint off the glass takes the landing with it: nothing
    /// is minted, and nothing is sown above the window.
    #[test]
    fn a_scroll_between_spawn_and_arrival_moves_the_landing_with_the_train() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let rows = 2_u16;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("flies");
        m.translate_scroll(rows, g.ch as u16);
        let (wx, wy) = g.cell_center(5 - rows, 40);
        let arrival = t0 + spawn.t_flight;
        let land = ctx_at(arrival, &cfg, (5 - rows, 40), 0.25);
        let mut sc = Scratch::default();
        sc.emit(&mut m, &land);
        let l = m
            .landings
            .first()
            .copied()
            .expect("the landing is minted at T");
        assert!(
            (l.x - wx).abs() < 0.5 && (l.y - wy).abs() < 0.5,
            "the landing was minted at ({}, {}), not at the scrolled endpoint ({wx}, {wy})",
            l.x,
            l.y
        );
        assert_eq!(
            (l.x, l.y),
            (m.live[0].x1, m.live[0].y1),
            "the landing is the train's end"
        );
        // The pin's 2×2 heart is on the scrolled row …
        let (cx, cy) = (wx.round() as i32, wy.round() as i32);
        assert!(
            sc.out.iter().any(|q| is_white(q.color)
                && q.w == 2
                && q.h == 2
                && i32::from(q.x) == cx - 1
                && i32::from(q.y) == cy - 1),
            "the pin's heart is not on the scrolled landing"
        );
        // … and so is every fan star's birth.
        let mut dust = sky();
        m.sow_into(&mut dust);
        let fan = lane(&dust, StarLane::Fan);
        assert!(fan.len() >= FAN_HERO_N);
        for s in &fan {
            assert!(
                (s.x - wx).abs() <= 2.0 && (s.y - wy).abs() <= 2.0,
                "a fan star was born at ({}, {}), not on the scrolled landing ({wx}, {wy})",
                s.x,
                s.y
            );
        }

        // Off the glass: an upward recall whose landing — and farthest
        // stations — the scroll carries above the window, launch still on it.
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (3, 40), 0.25);
        let spawn = m.on_event(&mv((9, 40), (3, 40)), t0, &ctx).expect("flies");
        m.translate_scroll(7, g.ch as u16);
        assert_eq!(
            m.live.len(),
            1,
            "a train whose launch is still on glass is kept"
        );
        assert!(m.live[0].y1 < 0.0 && m.live[0].y0 >= 0.0);
        let mut dust = sky();
        let land = ctx_at(t0 + spawn.t_flight, &cfg, (0, 40), 0.25);
        sc.emit(&mut m, &land);
        m.sow_into(&mut dust);
        assert!(m.live[0].landed, "the arrival edge is still spent");
        assert!(
            m.landings.is_empty(),
            "a landing above the glass was minted"
        );
        assert!(
            lane(&dust, StarLane::Fan).is_empty(),
            "a fan was sown above the glass"
        );
        assert!(
            !lane(&dust, StarLane::Shed).is_empty(),
            "the stations still on glass were not sown"
        );
        assert!(
            dust.live_iter().all(|s| s.y >= 0.0),
            "a fragment was sown above the glass"
        );
    }

    /// **§20.1 `the_pin_is_born_at_peak_and_closes_into_the_caret`.** At `T`
    /// the arms are `0.6 ch ± 1 px` with a waist of exactly 1 px, drawn as
    /// four half-arms that never cross the 2×2 heart (no two pin quads
    /// overlap — D1's "no duplicate core rect" at the one cell the ledger
    /// cannot hold); they whip inward, and the last mark on glass is the
    /// nucleus, on the caret cell.
    #[test]
    fn the_pin_is_born_at_peak_and_closes_into_the_caret() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let ch = geom().ch as f32;
        let mut sc = Scratch::default();

        // The landing is minted on the arrival frame; the pin is then drawn
        // BY ITSELF at each instant, so the shockwave, the splash and the
        // sparks (2026-09-08) cannot stand in for its arms.
        sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 40), 0.25));
        let l = m.landings[0];
        let pin_only = |sc: &mut Scratch, after: u64| {
            sc.clear();
            let at = ctx_at(arrival + ms(after), &cfg, (5, 40), 0.25);
            let mut fr = sc.frame();
            draw_pin(&l, &at, &mut fr);
        };
        pin_only(&mut sc, 0);
        let (cx, cy) = (l.x.round() as i32, l.y.round() as i32);
        let arms: Vec<GlowQuad> = sc
            .out
            .iter()
            .filter(|q| is_chromatic(q.color))
            .copied()
            .collect();
        assert!(!arms.is_empty(), "no arms at T");
        for q in &arms {
            assert!(
                q.w == 1 || q.h == 1,
                "an arm is not a 1-px hairline: {}×{}",
                q.w,
                q.h
            );
        }
        let row: Vec<_> = arms
            .iter()
            .filter(|q| q.h == 1 && i32::from(q.y) == cy)
            .collect();
        let col: Vec<_> = arms
            .iter()
            .filter(|q| q.w == 1 && i32::from(q.x) == cx)
            .collect();
        assert!(!row.is_empty() && !col.is_empty(), "the pin is not a cross");
        let extent = |qs: &[&GlowQuad], major: fn(&GlowQuad) -> (i32, i32)| -> i32 {
            let lo = qs.iter().map(|q| major(q).0).min().unwrap();
            let hi = qs.iter().map(|q| major(q).1).max().unwrap();
            hi - lo
        };
        let xs = |q: &GlowQuad| (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        let ys = |q: &GlowQuad| (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
        let want = 2.0 * PIN_ARM_CH * ch;
        let across = extent(&row, xs);
        let down = extent(&col, ys);
        assert!(
            (across as f32 - want).abs() <= 2.0 && (down as f32 - want).abs() <= 2.0,
            "arm span {across}×{down} px, not 2 × 0.6 ch = {want}"
        );
        let nucleus = sc
            .out
            .iter()
            .find(|q| is_white(q.color) && q.w == 2 && q.h == 2)
            .copied()
            .expect("the 2×2 heart");
        assert_eq!(
            (i32::from(nucleus.x), i32::from(nucleus.y)),
            (cx - 1, cy - 1)
        );
        let mut pin = arms.clone();
        pin.push(nucleus);
        for (i, a) in pin.iter().enumerate() {
            for b in &pin[i + 1..] {
                assert!(!overlaps(a, b), "two pin quads overlap: {a:?} / {b:?}");
            }
        }

        // Halfway: the arms have whipped in.
        pin_only(&mut sc, 75);
        let near: Vec<GlowQuad> = sc
            .out
            .iter()
            .filter(|q| {
                is_chromatic(q.color)
                    && (i32::from(q.x) - cx).abs() <= 9
                    && (i32::from(q.y) - cy).abs() <= 9
            })
            .copied()
            .collect();
        let row: Vec<_> = near
            .iter()
            .filter(|q| q.h == 1 && i32::from(q.y) == cy)
            .collect();
        assert!(
            !row.is_empty() && extent(&row, xs) < across,
            "the arms did not whip inward"
        );

        // The close: no arm within 9 px of the heart, and the heart is there.
        pin_only(&mut sc, 140);
        assert!(
            !sc.out.iter().any(|q| {
                is_chromatic(q.color)
                    && (i32::from(q.x) - cx).abs() <= 9
                    && (i32::from(q.y) - cy).abs() <= 9
            }),
            "an arm outlived the close"
        );
        assert!(
            sc.out.iter().any(|q| is_white(q.color)
                && q.w == 2
                && q.h == 2
                && (i32::from(q.x), i32::from(q.y)) == (cx - 1, cy - 1)),
            "the last mark on glass is not the nucleus on the caret cell"
        );
    }

    /// **§20.1 `the_ring_has_no_notch_on_the_steep_quadrants`.** Per-column
    /// continuity on all four quadrants at 72 segments: every column across
    /// the ring carries ring light with no gap in it, and every row carries
    /// it on both sides. (v1 notched 16 of its 32 segments on the steep
    /// quadrants.)
    #[test]
    fn the_ring_has_no_notch_on_the_steep_quadrants() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        // The ring is drawn BY ITSELF (the sparks and the splash of
        // 2026-09-08 share its streams) at T + 60: `u = 1/8`, r = 3.0 ch ·
        // (1 − (7/8)⁴) ≈ 22.4 px, ry ≈ 11.2 px.
        sc.emit(&mut m, &ctx_at(arrival + ms(60), &cfg, (5, 40), 0.25));
        let l = m.landings[0];
        sc.clear();
        {
            let at = ctx_at(arrival + ms(60), &cfg, (5, 40), 0.25);
            let mut fr = sc.frame();
            draw_ring(&l, &at, &mut fr);
        }
        let (cx, cy) = (l.x.round() as i32, l.y.round() as i32);
        let ring_law = l.ring.expect("a 40-cell nav landing rings");
        let ch = geom().ch as f32;
        let u = 60.0 / ring_law.ms;
        let r = ring_full_radius(ring_law.scale, ch) * (1.0 - (1.0 - u).powi(RING_R_EXP));
        let (rx, ry) = (
            r.round() as i32,
            (r * RING_SQUASH).min(RING_RISE_MAX_CH * ch).round() as i32,
        );
        let ring: Vec<&GlowQuad> = sc.out.iter().filter(|q| is_chromatic(q.color)).collect();
        assert!(
            ring.len() >= RING_SEGMENTS,
            "only {} ring quads",
            ring.len()
        );

        let covers_col =
            |q: &GlowQuad, x: i32| (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&x);
        let covers_row =
            |q: &GlowQuad, y: i32| (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y);
        // A hollow ring crosses a line in at most TWO runs (the two arcs);
        // a third run is a notch, and every run is contiguous.
        let runs = |mut px: Vec<i32>| -> usize {
            px.sort_unstable();
            px.dedup();
            1 + px.windows(2).filter(|w| w[1] - w[0] > 1).count()
        };
        for x in cx - rx + 2..=cx + rx - 2 {
            let rows: Vec<i32> = ring
                .iter()
                .filter(|q| covers_col(q, x))
                .flat_map(|q| i32::from(q.y)..i32::from(q.y) + i32::from(q.h))
                .collect();
            assert!(!rows.is_empty(), "column {x} of the ring is dark (a notch)");
            let n = runs(rows);
            assert!(
                n <= 2,
                "column {x} crosses the ring in {n} runs — it is notched"
            );
        }
        for y in cy - ry + 2..=cy + ry - 2 {
            let left = ring.iter().any(|q| covers_row(q, y) && i32::from(q.x) < cx);
            let right = ring
                .iter()
                .any(|q| covers_row(q, y) && i32::from(q.x) + i32::from(q.w) > cx);
            assert!(left && right, "row {y} of the ring is open on one side");
            let cols: Vec<i32> = ring
                .iter()
                .filter(|q| covers_row(q, y))
                .flat_map(|q| i32::from(q.x)..i32::from(q.x) + i32::from(q.w))
                .collect();
            let n = runs(cols);
            assert!(
                n <= 2,
                "row {y} crosses the ring in {n} runs — it is notched"
            );
        }
    }

    // -- T6, idle → zero ----------------------------------------------------

    /// **T6.** An idle pool writes NOTHING, is at rest, and names no
    /// deadline — the first half of "idle → zero work", and the reason the
    /// host can return to 0 % CPU.
    #[test]
    fn an_idle_meteor_pool_draws_nothing_and_names_no_deadline() {
        let cfg = config();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let mut sc = Scratch::default();
        for k in 0..8_u64 {
            let ctx = ctx_at(t0 + ms(k * 16), &cfg, (5, 10), 0.25);
            sc.emit(&mut m, &ctx);
            assert!(sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty());
        }
        assert!(m.at_rest());
        assert_eq!(m.live(), 0);
        assert!(m.next_change_deadline(t0).is_none());

        // And a typed echo is not a flight: T1 lives at the seam, and the one
        // move class it refuses here is the ink wrap.
        let ctx = ctx_at(t0, &cfg, (5, 0), 0.25);
        let wrap = Event::Move {
            from: (5, 119),
            to: (6, 0),
            licence: Licence::Typed,
            dir: Dir::Down,
        };
        assert!(
            m.on_event(&wrap, t0, &ctx).is_none(),
            "a typed wrap flew — `ink_wrap_rainbow_keeps_the_ribbon_no_zoom`"
        );
        assert!(m.at_rest(), "a typed wrap left something in the air");
    }

    // -- §18, the cadence law over a whole life ------------------------------

    /// **THE ATTACK ASKS FOR EVERY FRAME; THE RELEASE RIDES THE HOST'S TRAIN**
    /// (the cadence law re-stated 2026-09-08 for the bigger impact). Over a
    /// whole flight and its impact — 8, 40 and 80 cells, both directions,
    /// and an Enter's small landing — emitted every 8 ms:
    ///
    /// * from the spawn frame through the pin's close (`T + 150`) the pool
    ///   is brisk and names the next frame exactly (`FRAME_CADENCE`);
    /// * while the impact still MOVES past that (the shockwave, the splash,
    ///   the sparks, the train's root) it is still brisk — seam point 8, the
    ///   host's train — but names nothing sooner than the `TAIL_FLOOR`, so a
    ///   host that folds the deadline under its own lane tick wakes once per
    ///   tick, never twice;
    /// * once the last spark is under its cull nothing of the meteor draws,
    ///   it is not brisk, and it names only the pool's horizon, at the floor
    ///   or later;
    /// * at `T + 600` it is at rest and names nothing (T6).
    ///
    /// The release is real (≥ `RING_MS` past the pin on a big landing; the
    /// train's `ROOT_SUCK_MS` on an Enter's), and the count of sub-floor
    /// deadlines over the life is the attack's alone. Before this law the
    /// whole `T + 600` was the fold's brisk and the seam census read 567
    /// frame-cadence wakes against 521 lane ticks.
    #[test]
    fn the_attack_asks_for_every_frame_and_the_release_rides_the_host_s_train() {
        use crate::rainbow_kitty::{FRAME_CADENCE, TAIL_FLOOR};
        let cfg = config();
        let t0 = Instant::now();
        let close = Duration::from_secs_f32(PIN_MS / 1000.0);
        let root_suck = Duration::from_secs_f32(ROOT_SUCK_MS / 1000.0);
        let flights = [
            ((5, 0), (5, 8), Licence::Nav),
            ((5, 0), (5, 40), Licence::Nav),
            ((5, 80), (5, 0), Licence::Nav),
            ((5, 40), (5, 80), Licence::Nav),
            ((5, 20), (6, 0), Licence::Return),
        ];
        for (from, to, licence) in flights {
            let name = format!("{from:?} → {to:?} {licence:?}");
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            let spawn = m
                .on_event(&mv_as(from, to, licence), t0, &ctx)
                .expect("fly");
            let arrival = t0 + spawn.t_flight;
            let end = m.live[0].end();
            let mut sc = Scratch::default();
            let mut moving_until: Option<Instant> = None;
            let mut sub_floor = 0_u32;
            let mut release_wakes = 0_u32;
            let mut t = t0;
            while t < end + ms(50) {
                sc.emit(&mut m, &ctx_at(t, &cfg, to, 0.25));
                if moving_until.is_none()
                    && let Some(l) = m.landings.first()
                {
                    moving_until = Some(l.moving_until().max(arrival + root_suck));
                }
                let brisk = m.brisk(t);
                let due = m.next_change_deadline(t);
                let after = t.saturating_duration_since(arrival).as_millis();
                if let Some(d) = due
                    && d < t + TAIL_FLOOR
                {
                    sub_floor += 1;
                }
                if t < arrival + close {
                    assert!(brisk, "{name}: not brisk at T + {after} ms, in the attack");
                    assert_eq!(
                        due,
                        Some(t + FRAME_CADENCE),
                        "{name}: the attack did not ask for the next frame at T + {after} ms"
                    );
                } else if moving_until.is_some_and(|mu| t < mu) {
                    release_wakes += 1;
                    assert!(brisk, "{name}: the release is not brisk at T + {after} ms");
                    assert!(
                        due.is_some_and(|d| d >= t + TAIL_FLOOR),
                        "{name}: the release named {:?} at T + {after} ms — under the floor",
                        due.map(|d| d.saturating_duration_since(t))
                    );
                } else if t < end {
                    assert!(
                        !brisk,
                        "{name}: brisk at T + {after} ms with nothing moving"
                    );
                    assert!(
                        sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty(),
                        "{name}: the meteor drew at T + {after} ms, past its last motion"
                    );
                    assert!(
                        due.is_some_and(|d| d >= t + TAIL_FLOOR),
                        "{name}: the horizon named {:?} at T + {after} ms",
                        due.map(|d| d.saturating_duration_since(t))
                    );
                } else {
                    assert!(m.at_rest(), "{name}: not at rest at T + {after} ms");
                    assert!(due.is_none(), "{name}: a resting pool named a deadline");
                }
                t += ms(8);
            }
            let mu = moving_until.expect("the landing was minted");
            let release = mu.saturating_duration_since(arrival + close);
            let floor = if licence == Licence::Return {
                root_suck - close
            } else {
                Duration::from_secs_f32(RING_MS / 1000.0) - close
            };
            assert!(
                release >= floor && release_wakes > 0,
                "{name}: the release is {release:?} ({release_wakes} wakes), less than {floor:?}"
            );
            let attack = (spawn.t_flight + close).as_secs_f32() * 1000.0 / 8.0;
            assert!(
                (sub_floor as f32 - attack).abs() <= 1.0,
                "{name}: {sub_floor} sub-floor deadlines over the life; the attack is {attack:.0} \
                 frames"
            );
        }
    }

    /// **A SPARK IS CULLED EXACTLY WHERE THE CADENCE LAW SAYS IT IS.** The
    /// share of its life [`spark_cull_u`] solves is the frame path's own
    /// cull: a millisecond before it [`spark_at`] still draws the spark, a
    /// millisecond after it does not — for every spark of a 40-cell landing.
    /// So [`Landing::moving_until`] is never early (a spark still falling
    /// past it) and never idle-late by more than the rounding.
    #[test]
    fn a_spark_is_culled_exactly_where_the_cadence_law_says_it_is() {
        let ch = geom().ch as f32;
        let cull = spark_cull_u();
        assert!(
            (cull - 0.861).abs() < 0.002,
            "the cull share is {cull}, not ≈ 0.861"
        );
        let seed = mix32(0xBEEF ^ 0xFA5E);
        for k in 0..SPARK_N_MAX {
            let s = mint_spark(seed, k, ch);
            let at = s.life * cull;
            assert!(
                spark_at(s, at - 1.0, ch).is_some(),
                "spark {k}: already gone 1 ms before its cull at {at:.1} ms"
            );
            assert!(
                spark_at(s, at + 1.0, ch).is_none(),
                "spark {k}: still drawn 1 ms after its cull at {at:.1} ms"
            );
        }
    }

    // -- the impact, second round (owner, 2026-09-08: "BIGGER and MORE
    // SPECIAL") ---------------------------------------------------------------

    /// A cell-sized `out` quad — the flash's, and nothing else the landing
    /// draws (the pin is hairlines, the ring is slabs no wider than its
    /// step, the sparks are stars, the bands are a third of a cell tall).
    fn cell_sized(q: &GlowQuad) -> bool {
        usize::from(q.w) == geom().cw && usize::from(q.h) == geom().ch
    }

    /// A quad's brightest premultiplied channel, in levels.
    fn peak_of(q: &GlowQuad) -> u32 {
        let (r, g, b) = chan(q.color);
        r.max(g).max(b)
    }

    /// **THE ARRIVAL FLASHES WHITE AND DIES INTO THE RAINBOW.** The owner's
    /// 48-cell Ctrl-A: on the first frame after the pin the caret cell and
    /// its neighbour on the line are cell-sized WHITE `out` quads — two
    /// cells' worth of white pixels at ≥ 200 levels (column 0's left
    /// neighbour is off the glass) — and from there the white LEAVES and the
    /// colour ARRIVES: the ratio of white to coloured flash coverage falls
    /// monotonically frame by frame, the colour is on glass after the white
    /// is gone, and both are gone by `T + 200`. And the burst is priced by
    /// the sky's probe: on a Ctrl-E the blank cell past the line asks more
    /// than the transient cap and the glyph cell before the caret asks the
    /// cap. Before the second round: no cell-sized white quad on any frame.
    #[test]
    fn the_arrival_flashes_white_and_dies_into_the_rainbow() {
        let cfg = config();
        let g = geom();
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 0), 0.25);
        let spawn = m.on_event(&mv((5, 48), (5, 0)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 0), 0.25));
        let mut dust = sky_around(5);
        m.sow_into(&mut dust);

        let (lx, ly) = g.cell_center(5, 0);
        let mine = |q: &GlowQuad| {
            cell_sized(q)
                && (f32::from(q.y) + f32::from(q.h) * 0.5 - ly).abs() < 1.0
                && (f32::from(q.x) + f32::from(q.w) * 0.5 - lx).abs() <= 2.0 * g.cw as f32
        };
        sc.emit(&mut m, &ctx_at(arrival + ms(8), &cfg, (5, 0), 0.25));
        let white_px: usize = sc
            .out
            .iter()
            .filter(|q| mine(q) && is_white(q.color) && peak_of(q) >= 200)
            .map(|q| usize::from(q.w) * usize::from(q.h))
            .sum();
        assert!(
            white_px >= 2 * g.cw * g.ch,
            "T + 8: {white_px} white flash pixels, not two cells' worth ({})",
            2 * g.cw * g.ch
        );

        let mut prev = f32::INFINITY;
        let mut colour_after_white = false;
        let mut after = 8_u64;
        while after <= 200 {
            sc.emit(&mut m, &ctx_at(arrival + ms(after), &cfg, (5, 0), 0.25));
            let white: u32 = sc
                .out
                .iter()
                .filter(|q| mine(q) && is_white(q.color))
                .map(peak_of)
                .sum();
            let colour: u32 = sc
                .out
                .iter()
                .filter(|q| mine(q) && is_chromatic(q.color))
                .map(peak_of)
                .sum();
            let ratio = white as f32 / (colour.max(1)) as f32;
            assert!(
                ratio <= prev + 1e-6,
                "T + {after}: the white/colour ratio ROSE ({prev} → {ratio}) — the flash \
                 must die one way, white into spectrum"
            );
            if white == 0 && colour > 0 {
                colour_after_white = true;
            }
            prev = ratio;
            after += 8;
        }
        assert!(
            prev == 0.0,
            "T + 200: the white/colour ratio is {prev}, not zero"
        );
        assert!(
            colour_after_white,
            "the colour never stood on the glass after the white had gone"
        );
        assert!(
            !sc.out.iter().any(mine),
            "the flash is still on glass at T + 200"
        );

        // Priced by the probe: a Ctrl-E from column 0 to the end of a
        // 48-glyph line lands on a blank cell with the last glyph before it
        // and the sky's blank past it.
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 48), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 48)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 48), 0.25));
        let mut dust = sky_around(5);
        let mut ink = [false; 120];
        ink[..48].fill(true);
        dust.probe_mut().probe_row(5, &ink);
        m.sow_into(&mut dust);
        sc.emit(&mut m, &ctx_at(arrival + ms(8), &cfg, (5, 48), 0.25));
        let white_at = |col: u16| -> u32 {
            let (cx, _) = g.cell_center(5, col);
            sc.out
                .iter()
                .filter(|q| {
                    cell_sized(q)
                        && is_white(q.color)
                        && (f32::from(q.x) + f32::from(q.w) * 0.5 - cx).abs() < 1.0
                })
                .map(peak_of)
                .max()
                .unwrap_or(0)
        };
        let (glyph, caret, blank) = (white_at(47), white_at(48), white_at(49));
        assert!(
            blank as f32 > TRANSIENT_STAR_COV_CEIL && caret as f32 > TRANSIENT_STAR_COV_CEIL,
            "the burst over the blank cells asks {caret}/{blank}, not more than the cap"
        );
        assert!(
            glyph > 0 && glyph as f32 <= TRANSIENT_STAR_COV_CEIL,
            "the burst over the glyph cell asks {glyph} — over the transient cap"
        );
    }

    /// **THE SHOCKWAVE'S STROKE IS TWO AND A HALF TIMES THE BOLD ROUND'S**,
    /// measured across the top of the ring at the end of its hold
    /// (`T + 120`, `u = 0.25`, `r ≈ 2.05 ch`): the rows of ring light in the
    /// caret's own column above the row span at least `2.5 × 0.16 × r`
    /// pixels — and the stroke is still at the cap there (held), and spent
    /// by `T + 300`. Before the second round: `0.16 × r`, 6 px at `ch = 18`,
    /// and the hold ran to `T + 168`.
    #[test]
    fn the_shockwave_stroke_is_two_and_a_half_times_the_bold_round_s() {
        let cfg = config();
        let t0 = Instant::now();
        let ch = geom().ch as f32;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 40), 0.25));
        let l = m.landings[0];
        let ring_only = |sc: &mut Scratch, after: u64| {
            sc.clear();
            let at = ctx_at(arrival + ms(after), &cfg, (5, 40), 0.25);
            let mut fr = sc.frame();
            draw_ring(&l, &at, &mut fr);
        };
        let (cx, cy) = (l.x.round() as i32, l.y.round() as i32);
        // The ring's OWN life and radius (distance-graded: a 40-cell landing
        // is impact 3.09 — 580 ms, `r_full` 5.5 ch), so the hold is read
        // where this ring holds, not where the floor ring would.
        let ring_law = l.ring.expect("a 40-cell nav landing rings");
        let hold_ms = (RING_COV_HOLD_U * ring_law.ms) as u64;
        ring_only(&mut sc, hold_ms);
        let u = hold_ms as f32 / ring_law.ms;
        let r = ring_full_radius(ring_law.scale, ch) * (1.0 - (1.0 - u).powi(RING_R_EXP));
        let top: Vec<&GlowQuad> = sc
            .out
            .iter()
            .filter(|q| {
                is_chromatic(q.color)
                    && (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&cx)
                    && i32::from(q.y) + i32::from(q.h) <= cy - 2
            })
            .collect();
        assert!(
            !top.is_empty(),
            "no ring light above the caret at T + {hold_ms}"
        );
        let y_lo = top.iter().map(|q| i32::from(q.y)).min().unwrap();
        let y_hi = top
            .iter()
            .map(|q| i32::from(q.y) + i32::from(q.h))
            .max()
            .unwrap();
        let stroke = (y_hi - y_lo) as f32;
        // The law carries its own ceiling (`RING_THICK_MAX_CH`, 1.2 ch): on a
        // graded ring the share binds at the ceiling, and the ceiling is what
        // the stroke must reach.
        let want = (2.5 * RING_THICK_SHARE_BOLD_ROUND * r).min(RING_THICK_MAX_CH * ch);
        assert!(
            stroke + 1.0 >= want,
            "the stroke is {stroke} px across the top of a {r:.1} px ring — under two and a \
             half times the bold round's {want:.1}"
        );
        let peak = top.iter().map(|q| peak_of(q)).max().unwrap_or(0) as f32;
        assert!(
            peak >= 0.95 * TRANSIENT_STAR_COV_CEIL,
            "the stroke asks {peak} at the end of its hold, not the cap"
        );
        ring_only(&mut sc, 300);
        let spent = sc.out.iter().map(peak_of).max().unwrap_or(0) as f32;
        assert!(
            spent < 0.75 * TRANSIENT_STAR_COV_CEIL,
            "the stroke is still at {spent} at T + 300 — not spending"
        );
    }

    /// **THE SPLASH NEVER ENTERS A GLYPH ROW.** Under `tall` and under
    /// `underline`: every splash quad over the life lies OUTSIDE the landing
    /// row's own cell rows, inside the sky band above it or its mirror below
    /// (`0.45 ch` of the cell's edges), the two bands reach at least 4.5
    /// cells each side at `T + 120`, and both bands are lit. With the row
    /// above inked the upper band is not laid and the lower one still is;
    /// before the probe has been asked, nothing is laid. Before the second
    /// round the splash was bed ink ON the row.
    #[test]
    fn the_splash_never_enters_a_glyph_row() {
        let g = geom();
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let t0 = Instant::now();
        for tall in [true, false] {
            let mut cfg = config();
            cfg.ribbon_tall = tall;
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, (5, 40), 0.25);
            let spawn = m.on_event(&mv((5, 0), (5, 40)), t0, &ctx).expect("fly");
            let arrival = t0 + spawn.t_flight;
            let mut sc = Scratch::default();
            sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 40), 0.25));
            let mut dust = sky_around(5);
            m.sow_into(&mut dust);
            let l = m.landings[0];
            let (lx, ly) = g.cell_center(5, 40);
            let (row_top, row_bot) = (ly - ch * 0.5, ly + ch * 0.5);
            let splash_only = |sc: &mut Scratch, l: &Landing, after: u64| {
                sc.clear();
                let at = ctx_at(arrival + ms(after), &cfg, (5, 40), 0.25);
                let mut fr = sc.frame();
                let mut verts = Vec::new();
                draw_splash(&mut verts, l, &at, &mut fr);
            };
            let mut after = 8_u64;
            let mut lit = 0_usize;
            while after <= SPLASH_MS as u64 {
                splash_only(&mut sc, &l, after);
                for q in sc.under.iter().chain(sc.out.iter()) {
                    lit += 1;
                    let (y0, y1) = (f32::from(q.y), f32::from(q.y) + f32::from(q.h));
                    assert!(
                        y1 <= row_top || y0 >= row_bot,
                        "tall {tall}, T + {after}: a splash quad at y {y0}..{y1} is INSIDE the \
                         glyph row {row_top}..{row_bot}"
                    );
                    assert!(
                        y0 >= row_top - 0.45 * ch - 1.0 && y1 <= row_bot + 0.45 * ch + 1.0,
                        "tall {tall}, T + {after}: a splash quad at y {y0}..{y1} is outside \
                         the sky band"
                    );
                }
                after += 8;
            }
            assert!(lit > 0, "tall {tall}: the splash never lit");
            splash_only(&mut sc, &l, 120);
            let reach = |above: bool, left: bool| -> f32 {
                sc.out
                    .iter()
                    .filter(|q| {
                        let y = f32::from(q.y);
                        let x = f32::from(q.x) + f32::from(q.w) * 0.5;
                        (y < ly) == above && (x < lx) == left
                    })
                    .map(|q| (f32::from(q.x) + f32::from(q.w) * 0.5 - lx).abs())
                    .fold(0.0, f32::max)
            };
            for above in [true, false] {
                for left in [true, false] {
                    let r = reach(above, left);
                    assert!(
                        r >= 4.5 * cw,
                        "tall {tall}: the band {} the row reaches {r} px {} — under 4.5 cells",
                        if above { "above" } else { "below" },
                        if left { "leftward" } else { "rightward" }
                    );
                }
            }

            // The row above inked: no band above, the band below still.
            let mut boxed = sky_around(5);
            boxed.probe_mut().probe_row(4, &[true; 120]);
            let mut l2 = l;
            l2.sky = Some(SkyMask::probe(l.x, l.y, boxed.probe(), g));
            splash_only(&mut sc, &l2, 120);
            assert!(
                !sc.out.iter().any(|q| f32::from(q.y) < ly),
                "tall {tall}: a band was laid over an inked row above"
            );
            assert!(
                sc.out.iter().any(|q| f32::from(q.y) > ly),
                "tall {tall}: the band below went with the one above"
            );
            let mut l3 = l;
            l3.sky = None;
            splash_only(&mut sc, &l3, 120);
            assert!(
                sc.out.is_empty() && sc.under.is_empty(),
                "tall {tall}: the splash was laid before the probe was asked"
            );
        }
    }

    /// **THE SPARKS ARE HALOED STARS, TWICE AS MANY.** A 48-cell jump (the
    /// owner's Ctrl-A, landed ten cells in so the leftward half of the shower
    /// is on the glass) throws at least sixty sparks (the bold round threw
    /// thirty), and at `T + 150` — every spark in its hold, the flash's white
    /// gone — at least fifty chromatic halos are on the frame (the corona's
    /// seven petals are the only others) and at least forty sparks stand
    /// ≥ 5 px on both axes (the bold round's were 3 × 3 points). Before the
    /// second round: 30 sparks, 7 halos, 3 px.
    #[test]
    fn the_sparks_are_haloed_stars_twice_the_bold_round_s_count() {
        let cfg = config();
        let g = geom();
        let ch = g.ch as f32;
        let t0 = Instant::now();
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 10), 0.25);
        let spawn = m.on_event(&mv((5, 58), (5, 10)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let mut sc = Scratch::default();
        sc.emit(&mut m, &ctx_at(arrival, &cfg, (5, 10), 0.25));
        let l = m.landings[0];
        assert!(
            usize::from(l.sparks) >= 60,
            "a 48-cell landing throws {} sparks, not twice the bold round's thirty",
            l.sparks
        );
        sc.emit(&mut m, &ctx_at(arrival + ms(150), &cfg, (5, 10), 0.25));
        let halos = sc.halos.iter().filter(|h| is_chromatic(h.color)).count();
        assert!(
            halos >= 50,
            "{halos} chromatic halos at T + 150 — the sparks have none"
        );
        let mut big = 0_usize;
        for spark in l.spark.iter().take(usize::from(l.sparks)) {
            let Some(((dx, dy), _)) = spark_at(*spark, 150.0, ch) else {
                continue;
            };
            let (px, py) = ((l.x + dx).round() as i32, (l.y + dy).round() as i32);
            let mine: Vec<&GlowQuad> = sc
                .out
                .iter()
                .filter(|q| {
                    is_chromatic(q.color)
                        && !cell_sized(q)
                        && (i32::from(q.x) + i32::from(q.w) / 2 - px).abs() <= 4
                        && (i32::from(q.y) + i32::from(q.h) / 2 - py).abs() <= 4
                })
                .collect();
            if mine.is_empty() {
                continue;
            }
            let x_lo = mine.iter().map(|q| i32::from(q.x)).min().unwrap();
            let x_hi = mine
                .iter()
                .map(|q| i32::from(q.x) + i32::from(q.w))
                .max()
                .unwrap();
            let y_lo = mine.iter().map(|q| i32::from(q.y)).min().unwrap();
            let y_hi = mine
                .iter()
                .map(|q| i32::from(q.y) + i32::from(q.h))
                .max()
                .unwrap();
            if x_hi - x_lo >= 5 && y_hi - y_lo >= 5 {
                big += 1;
            }
        }
        assert!(
            big >= 40,
            "only {big} sparks stand ≥ 5 × 5 px at T + 150 — points, not stars"
        );
    }

    // -- the landing, as the owner asked for it ("bring back the stars, more
    // variance and fun"), measured the way the offline judge measured it ----

    /// The meteor's OWN light over a window — every quad in `layers`
    /// composited additively per pixel on a BLACK ground, so what is measured
    /// is the mark's chromaticity and not the theme's. `w × h` px from
    /// `(x0, y0)`. Halos are not included: they are the head's and the sky's,
    /// and every window this is asked of excludes both.
    fn composite(layers: &[&[GlowQuad]], x0: i32, y0: i32, w: usize, h: usize) -> Vec<[u16; 3]> {
        let mut px = vec![[0_u16; 3]; w * h];
        for q in layers.iter().flat_map(|v| v.iter()) {
            let (r, g, b) = chan(q.color);
            for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                    let (lx, ly) = (x - x0, y - y0);
                    if lx < 0 || ly < 0 || lx >= w as i32 || ly >= h as i32 {
                        continue;
                    }
                    let p = &mut px[ly as usize * w + lx as usize];
                    p[0] = (p[0] + r as u16).min(255);
                    p[1] = (p[1] + g as u16).min(255);
                    p[2] = (p[2] + b as u16).min(255);
                }
            }
        }
        px
    }

    /// **The landing is the payoff.** On the arrival frame the brightest mark
    /// within 1.5 `ch` of the landing is at least as bright as the brightest
    /// train quad beyond it; on the first frame the ring is a ring (`T + 30`,
    /// `r ≈ 0.67 ch`) its stroke asks at least HALF the pin's coverage — the
    /// offline judge measured the shipped `0.40·118·(1 − u)²` at ≤ 29/255 on
    /// glass, "a whisper" (`B/frame_0064-0071`); and once the fan's hold is
    /// spent (`T + 40`) at least three fan stars — the hero and the m2 —
    /// stand ≥ 5 × 5 px on glass, the transient-lane sizes of §5.2.
    #[test]
    fn the_landing_outshines_the_train_it_ends() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let ch = g.ch as f32;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let (lx, ly) = g.cell_center(5, 80);
        let near = |x: f32, y: f32| (x - lx).hypot(y - ly) <= 1.5 * ch;
        let centre = |q: &GlowQuad| {
            (
                f32::from(q.x) + f32::from(q.w) * 0.5,
                f32::from(q.y) + f32::from(q.h) * 0.5,
            )
        };

        // The arrival frame.
        let mut sc = Scratch::default();
        let land = ctx_at(arrival, &cfg, (5, 80), 0.25);
        sc.emit(&mut m, &land);
        let mut dust = sky();
        m.sow_into(&mut dust);
        let landing_peak = sc
            .out
            .iter()
            .filter(|q| {
                let (x, y) = centre(q);
                near(x, y)
            })
            .map(|q| lum(q.color))
            .chain(
                sc.halos
                    .iter()
                    .filter(|h| near(f32::from(h.cx), f32::from(h.cy)))
                    .map(|h| lum(h.color)),
            )
            .fold(0.0_f32, f32::max);
        let train_peak = sc
            .out
            .iter()
            .chain(sc.under.iter())
            .filter(|q| {
                let (x, y) = centre(q);
                !near(x, y)
            })
            .map(|q| lum(q.color))
            .fold(0.0_f32, f32::max);
        assert!(landing_peak > 0.0 && train_peak > 0.0, "nothing lit at T");
        assert!(
            landing_peak >= train_peak - 1.0,
            "the train ({train_peak}) out-shines the landing it ends ({landing_peak})"
        );

        // The ring, on its first frame as a ring. Its arcs off the pin's own
        // row and column are the ring and nothing else (the ring is the only
        // chromatic `out` mark the meteor draws there; the fan is the sky's).
        sc.emit(&mut m, &ctx_at(arrival + ms(30), &cfg, (5, 80), 0.25));
        let (cx, cy) = (lx.round() as i32, ly.round() as i32);
        let ring_peak = sc
            .out
            .iter()
            .filter(|q| {
                is_chromatic(q.color) && (i32::from(q.y) - cy).abs() >= 2 && i32::from(q.x) != cx
            })
            .map(|q| {
                let (r, g, b) = chan(q.color);
                r.max(g).max(b) as f32
            })
            .fold(0.0_f32, f32::max);
        assert!(ring_peak > 0.0, "no ring at T + 30");
        assert!(
            ring_peak >= 0.5 * TRANSIENT_STAR_COV_CEIL,
            "the ring's stroke asks {ring_peak}/255 on its first frame — a whisper beside a \
             pin at {TRANSIENT_STAR_COV_CEIL}"
        );

        // The fan, once its hold is spent: the sky's own quads for it.
        let at40 = ctx_at(arrival + ms(40), &cfg, (5, 80), 0.25);
        sc.emit(&mut m, &at40);
        let before = sc.out.len();
        {
            let mut fr = sc.frame();
            dust.emit(&at40, &mut fr);
        }
        let sky_quads = &sc.out[before..];
        let mut big = 0_usize;
        let mut sizes = Vec::new();
        for s in lane(&dust, StarLane::Fan) {
            let (px, py) = s.pos(at40.now, false);
            let mine: Vec<&GlowQuad> = sky_quads
                .iter()
                .filter(|q| {
                    (i32::from(q.x) + i32::from(q.w) / 2 - px).abs() <= 6
                        && (i32::from(q.y) + i32::from(q.h) / 2 - py).abs() <= 6
                })
                .collect();
            if mine.is_empty() {
                continue;
            }
            let x_lo = mine.iter().map(|q| i32::from(q.x)).min().unwrap();
            let x_hi = mine
                .iter()
                .map(|q| i32::from(q.x) + i32::from(q.w))
                .max()
                .unwrap();
            let y_lo = mine.iter().map(|q| i32::from(q.y)).min().unwrap();
            let y_hi = mine
                .iter()
                .map(|q| i32::from(q.y) + i32::from(q.h))
                .max()
                .unwrap();
            sizes.push((s.class, x_hi - x_lo, y_hi - y_lo));
            if x_hi - x_lo >= 5 && y_hi - y_lo >= 5 {
                big += 1;
            }
        }
        assert!(
            big >= 3,
            "only {big} fan stars stand ≥ 5×5 px at T + 40: {sizes:?}"
        );
    }

    /// **§6.8, as the eye reads it — the two-tau turn.** The white layer dies
    /// on τ 90 and the colour on τ 150, so the train must visibly turn white →
    /// spectrum after the arrival edge: the mean saturation of the meteor's
    /// own light on the train (beyond 1.5 `ch` of the landing, on a black
    /// ground) RISES from `T` to `T + 150` by at least 0.15, and never falls on
    /// the way. The offline judge measured the shipped fade FLAT (chromaticity
    /// 0.86 → 0.86 over `T + 0 → T + 183`, `B/frame_0061-0083`): an alpha-only
    /// two-tau leaves a white residue at 0.19 over a colour layer at 0.37 of an
    /// already dimmer base, and the additive composite never turns.
    #[test]
    fn the_train_turns_white_then_spectrum_as_it_dies() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let ch = g.ch as f32;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let (x0, _) = g.cell_center(5, 0);
        let (x1, y1) = g.cell_center(5, 80);
        let (wx, wy) = ((x0 - 20.0) as i32, (y1 - 20.0) as i32);
        let (w, h) = ((x1 - x0 + 40.0) as usize, 40_usize);
        let mut sc = Scratch::default();
        let mut sat_at = |sc: &mut Scratch, after: u64| -> (f32, usize) {
            sc.emit(&mut m, &ctx_at(arrival + ms(after), &cfg, (5, 80), 0.25));
            // The landing's own light is not the train's: the shockwave
            // crosses the train inside its reach, and where two hues add the
            // composite is less saturated than either. The exclusion is the
            // ring WHERE IT IS at this instant — its graded radius law
            // (`ring_full_radius`, 6 `ch` at full reach for this 80-cell
            // jump, which is the impact cap) on its quartic, plus its stroke
            // (`RING_THICK_SHARE` of the radius, half each side) — so at `T`
            // (`r(0) = 0`) the white layer next to the landing is COUNTED,
            // and at `T + 150` only the ring's own annulus is not. A fixed
            // 3 `ch` was enough while the ring was un-graded (2.3 `ch` at
            // `T + 150`); a fixed 6 `ch` would cut the very white this test
            // is about.
            let ring_excl = m.landings.first().and_then(|l| l.ring).map_or(0.0, |r| {
                let u = clamp01(after as f32 / r.ms);
                let radius = ring_full_radius(r.scale, ch) * (1.0 - (1.0 - u).powi(RING_R_EXP));
                radius * (1.0 + 0.5 * RING_THICK_SHARE) + 1.0
            });
            let px = composite(&[&sc.under, &sc.out], wx, wy, w, h);
            let (mut sum, mut n) = (0.0_f32, 0_usize);
            for (i, p) in px.iter().enumerate() {
                let (x, y) = ((i % w) as f32 + wx as f32, (i / w) as f32 + wy as f32);
                if (x - x1).hypot(y - y1) <= ring_excl {
                    continue;
                }
                let hi = p[0].max(p[1]).max(p[2]);
                if hi < 24 {
                    continue;
                }
                let lo = p[0].min(p[1]).min(p[2]);
                sum += f32::from(hi - lo) / f32::from(hi);
                n += 1;
            }
            (sum / n.max(1) as f32, n)
        };
        let (s0, n0) = sat_at(&mut sc, 0);
        let (s50, _) = sat_at(&mut sc, 50);
        let (s100, _) = sat_at(&mut sc, 100);
        let (s150, n150) = sat_at(&mut sc, 150);
        assert!(n0 > 0 && n150 > 0, "the train is dark at T or at T + 150");
        // ≥ 0.10 since 2026-09-08 (was 0.15): the colour train is brighter
        // over its whole length now (`COLOUR_FALLOFF_SHARE` 0.55), so the
        // white counts for less at T and the turn starts from a higher
        // saturation — measured 0.847 → 0.953 where the 0.35 falloff gave
        // 0.786 → 1.000. Still a turn, still monotone.
        assert!(
            s150 - s0 >= 0.10,
            "the train's mean saturation went {s0:.3} → {s150:.3} from T to T + 150 — the \
             white did not leave before the colour (judge defect 4, the flat turn)"
        );
        assert!(
            s50 >= s0 - 0.02 && s100 >= s50 - 0.02 && s150 >= s100 - 0.02,
            "the turn is not monotone: {s0:.3} → {s50:.3} → {s100:.3} → {s150:.3}"
        );
    }

    /// PROBE, not a pin (2026-09-09, the wake review's finding 1): how long
    /// the train's COLOUR outlives its WHITE, READ OFF A FRAME — the software
    /// composite of everything this producer emits for an 80-cell jump, the
    /// landing ring's annulus excluded exactly as
    /// `the_train_turns_white_then_spectrum_as_it_dies` excludes it. Two
    /// readings per offset after the arrival: the ENERGY of each class
    /// (chroma `Σ (hi − lo)` over every lit pixel; white `Σ lo` — the
    /// achromatic floor — over the same), threshold-free, with the offset at
    /// which each has fallen to 12 % of its own peak (`CHROMA_CULL_ALPHA`'s
    /// own criterion, the design's "551 ms" arithmetic), whole path and root
    /// zone (8 `ch` of the landing); and the glass scanner's px classes
    /// (white: peak ≥ 150, spread < 40; colour: peak ≥ 60, spread ≥ 40) for
    /// comparison with the captures — the composite is dimmer than the GPU's
    /// One/One glass, so those absolute counts read short here. Run with
    /// `--ignored --nocapture`.
    #[test]
    #[ignore = "census, prints; not a pin"]
    fn probe_train_colour_outlives_white_census() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        let ch = g.ch as f32;
        let mut m = Meteors::new();
        let ctx = ctx_at(t0, &cfg, (5, 80), 0.25);
        let spawn = m.on_event(&mv((5, 0), (5, 80)), t0, &ctx).expect("fly");
        let arrival = t0 + spawn.t_flight;
        let (x0, _) = g.cell_center(5, 0);
        let (x1, y1) = g.cell_center(5, 80);
        let (wx, wy) = ((x0 - 20.0) as i32, (y1 - 20.0) as i32);
        let (w, h) = ((x1 - x0 + 40.0) as usize, 40_usize);
        let mut sc = Scratch::default();
        let offsets: Vec<u64> = (0..=1000).step_by(25).collect();
        // (after, chroma, white, root chroma, root white, colour px, white px, peak)
        type Row = (u64, f64, f64, f64, f64, usize, usize, u32);
        let mut rows: Vec<Row> = Vec::new();
        for &after in &offsets {
            sc.emit(&mut m, &ctx_at(arrival + ms(after), &cfg, (5, 80), 0.25));
            let ring_excl = m.landings.first().and_then(|l| l.ring).map_or(0.0, |r| {
                let u = clamp01(after as f32 / r.ms);
                let radius = ring_full_radius(r.scale, ch) * (1.0 - (1.0 - u).powi(RING_R_EXP));
                radius * (1.0 + 0.5 * RING_THICK_SHARE) + 1.0
            });
            let px = composite(&[&sc.under, &sc.out], wx, wy, w, h);
            let (mut chroma, mut white, mut rch, mut rwh) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            let (mut cpx, mut wpx, mut peak) = (0usize, 0usize, 0u32);
            for (i, p) in px.iter().enumerate() {
                let (x, y) = ((i % w) as f32 + wx as f32, (i / w) as f32 + wy as f32);
                let d = (x - x1).hypot(y - y1);
                if d <= ring_excl {
                    continue;
                }
                let hi = u32::from(p[0].max(p[1]).max(p[2]).min(255));
                let lo = u32::from(p[0].min(p[1]).min(p[2]).min(255));
                if hi < 8 {
                    continue;
                }
                peak = peak.max(hi);
                let root = d <= 8.0 * ch;
                chroma += f64::from(hi - lo);
                white += f64::from(lo);
                if root {
                    rch += f64::from(hi - lo);
                    rwh += f64::from(lo);
                }
                if hi >= 60 && hi - lo >= 40 {
                    cpx += 1;
                }
                if hi >= 150 && hi - lo < 40 {
                    wpx += 1;
                }
            }
            rows.push((after, chroma, white, rch, rwh, cpx, wpx, peak));
        }
        println!(
            "after_ms   chroma_E    white_E | root: chroma_E white_E | colour_px white_px peak"
        );
        for r in &rows {
            println!(
                "{:>8} {:>10.0} {:>10.0} | {:>13.0} {:>8.0} | {:>9} {:>8} {:>4}",
                r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7
            );
        }
        let fall = |pick: &dyn Fn(&Row) -> f64| -> Option<u64> {
            let peak = rows.iter().map(pick).fold(0.0f64, f64::max);
            if peak <= 0.0 {
                return None;
            }
            rows.iter().find(|r| pick(r) < 0.12 * peak).map(|r| r.0)
        };
        let (fc, fw) = (fall(&|r| r.1), fall(&|r| r.2));
        let (frc, frw) = (fall(&|r| r.3), fall(&|r| r.4));
        println!(
            "FALLS TO 12 % OF ITS PEAK: whole path chroma {fc:?} ms, white {fw:?} ms; root zone chroma {frc:?} ms, white {frw:?} ms"
        );
        if let (Some(c), Some(w)) = (frc, frw) {
            println!(
                "ROOT colour/white 12 %-fall ratio {:.2} (design §5.1 target: about three)",
                c as f64 / w.max(1) as f64
            );
        }
        let last = |pick: &dyn Fn(&Row) -> usize| -> Option<u64> {
            rows.iter().filter(|r| pick(r) >= 12).map(|r| r.0).max()
        };
        println!(
            "GLASS-CLASS px last >= 12: colour {:?} ms, white {:?} ms",
            last(&|r| r.5),
            last(&|r| r.6)
        );
    }

    /// **§6.5 layer 5, on glass.** A shed fragment is born AT its station —
    /// under the train — and must come to rest BESIDE the path, never under
    /// it: at least 3 px from the centreline, and outside the train's own
    /// half-width there, so it resolves as a point outside the streak that
    /// shed it. Asked of both directions and of a vertical recall; the
    /// perpendicular sputter is the sky's, the station and the head velocity
    /// it is sputtered from are this file's.
    #[test]
    fn a_shed_fragment_rests_beside_the_train_not_under_it() {
        let cfg = config();
        let t0 = Instant::now();
        let g = geom();
        for (from, to) in [
            ((5_u16, 0_u16), (5_u16, 80_u16)),
            ((5, 80), (5, 0)),
            ((5, 40), (9, 40)),
        ] {
            let mut m = Meteors::new();
            let ctx = ctx_at(t0, &cfg, to, 0.25);
            let spawn = m.on_event(&mv(from, to), t0, &ctx).expect("flies");
            let mut sc = Scratch::default();
            let mut dust = sky();
            let mut t = t0;
            while t <= t0 + spawn.t_flight {
                sc.emit(&mut m, &ctx_at(t, &cfg, to, 0.25));
                m.sow_into(&mut dust);
                t += ms(8);
            }
            let m0 = m.live[0];
            let (x0, y0) = g.cell_center(from.0, from.1);
            let (x1, y1) = g.cell_center(to.0, to.1);
            let l = (x1 - x0).hypot(y1 - y0);
            let (ux, uy) = ((x1 - x0) / l, (y1 - y0) / l);
            let shed = lane(&dust, StarLane::Shed);
            assert_eq!(
                shed.len(),
                usize::from(shed_n(m0.cells)),
                "{from:?}->{to:?}: not every station shed"
            );
            for s in &shed {
                let (rx, ry) = s.rest();
                // Perpendicular distance from the rest to the path's line.
                let off = ((rx as f32 - x0) * uy - (ry as f32 - y0) * ux).abs();
                assert!(
                    off >= 3.0,
                    "{from:?}->{to:?}: a fragment born at ({}, {}) rests {off:.1} px from the \
                     centreline — under the train, not beside it",
                    s.x,
                    s.y
                );
                assert!(
                    off > m0.w * 0.5 + 1.0,
                    "{from:?}->{to:?}: a fragment rests {off:.1} px out, inside a {:.1} px train",
                    m0.w
                );
            }
        }
    }
}
