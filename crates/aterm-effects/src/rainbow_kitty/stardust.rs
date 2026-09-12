// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **STARDUST** — the sky above the ribbon: a few real stars, of graded
//! magnitude, scintillating out of phase, dying by alpha at a snapped stop.
//!
//! Design of record: `RAINBOW-KITTY-V2.md` §5 in full — §5.1 (what separates
//! stardust from dust), §5.2 (the classes and the two ceilings), §5.3 (tint),
//! §5.4 (birth zones), §5.5 (scintillation's two clocks, D12), §5.6
//! (populations and the 40 cap), §5.7 (the exact recipes), §5.8 (earned
//! heroes) — plus §13 (the token bucket) and D1/D2/D3/D5.
//!
//! ## The five lines that are the whole trick (§5.1)
//!
//! 1. A star is a **peak**: the brightest pixel is the smallest thing. Dust is
//!    flat coverage on a blob.
//! 2. A star field is a **magnitude distribution**: a rare hero with a halo and
//!    glints, some medium stars, a few faint grains. Dust is one size.
//! 3. A star **scintillates**, and visibly *opens and closes*, out of phase
//!    with its neighbours.
//! 4. A star lives in the **sky**, never on a stroke.
//! 5. Stars are **few and bright**, and die by alpha at a snapped colour.
//!
//! ## What v1 measured, and which line answers it
//!
//! A live capture of the shipped trail measured component size p50 = **1 px**,
//! peak luminance p50 **70/255**, saturation p50 **7-8** — i.e. 93-98 % GREY,
//! motionless, blinking in unison. That is four separate failures, and each
//! gets a named law here rather than a tuning pass:
//!
//! | Measured | Cause | The law that answers it |
//! |---|---|---|
//! | size p50 1 px | `push_dust_mote` is a `d×d` square plus a `ceil(d/2)` square — flat coverage, no falloff, ONE size | [`StarClass`]: three magnitudes from an [`M1_COV_SKY`]-priced hero with halo and glints down to an [`M3_COV_SKY`] grain |
//! | peak 70/255 | nothing stacked to a peak | §5.7's recipes: composited centres [`CENTRE_M1_SKY`] / [`CENTRE_M2_SKY`] / [`CENTRE_M3_SKY`], each ≥ 2× its own 8-neighbours |
//! | saturation 7-8 | white marks fading through grey | chroma is fixed at birth ([`Star::tint`]) and only COVERAGE moves; the cull is at [`STAR_CULL_ALPHA`], never a grey speck — and the tint is put where the eye can find it: full-request [`STUB_LEN_PX`]-px arm tips ([`STUB_COV_SHARE`]), a halo at §3.2's 14 % cap ([`M1_HALO_SHARE`]), the grain's own tinted disc ([`M3_CROSS_ADD`]) |
//! | motionless, uniform blink | ONE clock, one phase | TWO clocks (§5.5): luminance on the certified 2.86 Hz [`twinkle_env`], ARM SIZE on the seeded [`Star::f`] ∈ [8, 12] Hz integer-stepped — plus the φ + π flip that stops a warm and a cool neighbour blinking together |
//!
//! ## Everything is derived; nothing is stored
//!
//! A [`Star`] is nine flat `Copy` fields and there is no per-frame state at
//! all. Envelope, arm, coverage, phase, gold-ness, halo radius and position
//! are pure functions of `(born, seed, now)`. That is what §18's determinism
//! rule and its "no per-star allocation" line mean in practice, and it is why
//! a frame at the same `now` is byte-identical on CPU and GPU.
//!
//! ## The sky carries the rainbow (owner, 2026-09-08)
//!
//! The first ruling (A/B #15, 2026-09-05) chose white-hot stardust with the
//! tint held to a 14 % halo and the arm tips. The owner reversed it: *"I feel
//! like you are diminishing the specialness and emphasis of this theme?
//! why?"* — *"make this rainbow theme truly magical and special and dynamic
//! and beautiful"*. So the sky is coloured now, and the two laws that kept
//! it white are re-derived rather than bent:
//!
//! * **D2 is inverted for the grain and the m2.** An m3 is the pure stop of
//!   the cell it rose from, core and arms ([`Paint::draw_m3`]); an m2's body
//!   is [`M2_BODY_TINT`] of the way from white to its tint, its disc and
//!   stubs are the tint ([`Paint::draw_add`]); the hero keeps its white-hot
//!   centre under a halo that is now its whole request
//!   ([`M1_HALO_SHARE`]). The tint deal is [`TINT_FIELD_SHARE`] /
//!   [`TINT_WHITE_SHARE`] / [`TINT_GOLD_SHARE`] = 75 / 10 / 15 for m1 and
//!   m2, and a grain ALWAYS wears its cell's stop ([`class_tint`]).
//! * **§3.2 rank 8 is "the halo may not outshine the core"**
//!   ([`HALO_CAP_SHARE`]), not "14 %": what reads as colour on glass is a
//!   halo pixel with a channel spread ≥ 40 at a max channel ≥ 60 (the
//!   scanner's own floor), and at 14 % no halo pixel ever got there.
//! * **The halo breathes its hue** — [`HALO_HUE_SWING_ENTRIES`] table
//!   entries around the stop on the luminance clock ([`halo_rgb`]), never
//!   off the arc, never onto a neighbouring name.
//! * **A hot hand deals a denser sky** — every laid cell carries a field
//!   star at `disp ≥` [`FIELD_HOT_DISP`] ([`FIELD_DEAL_IN_HOT`]).
//! * **The aurora** (section 8): a rainbow veil in the sky band above the
//!   live ribbon, one [`Veil`] cell per laid cell, carrying the ribbon's own
//!   spectrum walk, priced by momentum at birth, riding the field lane's
//!   envelope so it leaves with the ribbon and is exactly nothing when the
//!   sky is.
//!
//! Every number below that changed for this says so at its definition, with
//! the measurement it came from; the pinned laws that changed are re-pinned
//! in section 9 with the owner's words as the reason.

use aterm_render::{GlowQuad, HaloMode, RainHalo, add_sat, halo_row_ny, halo_weight, premul_rgb};
use aterm_time::Instant;

use super::timing::{
    CHROMA_CULL_ALPHA, EDGE_IN_S, FIELD_STAR_COV_CEIL, GLINT_CAP, GLINT_REFILL_PER_S,
    REDUCED_MOTION_FADE_MS, STAR_CULL_ALPHA, STAR_STACK_ADD, TRANSIENT_STAR_COV_CEIL, clamp01,
    edge_in, half_life, half_life_life_s, smoothstep01, spend,
};
use super::{Cadence, Config, Ctx, Event, Frame, TypedClass};
use crate::color_math::relative_luminance;
use crate::cursor_glow::{
    BandPx, Geom, InkRole, RAINBOW_CARET_LIGHT_FLOOR, RAINBOW_SPARKLE_LIGHT_SHARE, SoundCue,
    band_row,
};
use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE, STAR_CORE_ADD, STAR_GLINT,
    STAR_GLINT_COV, STAR_OVER_CENTRE_LAYS, STAR_OVER_LAYS, STAR_TAPER_BODY, STAR_WAIST,
    TWINKLE_GLINT_FRAC, TWINKLE_OMEGA, push_fx_rect, push_twinkle_star, star_arm, star_body_px,
    twinkle_env, twinkle_peak,
};
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::ribbon::{
    BIRTH_EDGE_FLOOR, EXPIRY_MELT_SHARE, Ribbon, SWOOSH_TOTAL_S, expiry_melt, reduced_fade,
};
use crate::spectrum::{
    SPECTRUM_ANCHOR_AT, SPECTRUM_LUT, SPECTRUM_LUT_LEN, SPECTRUM_STOPS, spectrum, spectrum_snap,
    spectrum_snap_index, spectrum_stop,
};
use crate::trail_sound::SoundKind;

// ===========================================================================
// 1. The pool, the deal, the tint (§5.2, §5.3, §5.6)
// ===========================================================================

/// **THE CAP: 40 LIVE** (§5.6). Typical at 12 cps is ≈ 20. Over the cap the
/// OLDEST star is put on its [`EVICT_FINISH_MS`] finish and the newborn takes
/// the pool; **a newborn is never refused** — light is not rationed, only
/// retired. A star on its finish is still on glass, so the pool may briefly
/// hold a few more than this: see [`STAR_POOL_MAX`].
pub const STAR_CAP: usize = 40;

/// How long an evicted star has to finish (§5.6): its envelope is multiplied
/// by `spend(age_since_eviction / 40 ms)`, so it leaves on the theme's own
/// curve, compressed — it never pops (T5). `stars_over_the_cap_finish_oldest_first_within_forty_ms`.
pub const EVICT_FINISH_MS: f32 = 40.0;

/// **THE CATCH'S SHORTENED LIFE**, ms (panel #10(b), the pet and the sky): a
/// gold m1 the resident pet reached for is put on a
/// [`EVICT_FINISH_MS`]-length finish on the frame the paw lands, so the star
/// is gone inside a breath *on the theme's own curve* — the cat caught it, it
/// did not blink out (T5).
///
/// It is the SAME 40 ms the cap already spends, and deliberately so: the
/// theme has exactly one "gone now, but not popped" number, and a catch is
/// that event with a paw in front of it. `spend(u)` takes the star under
/// [`STAR_CULL_ALPHA`] at `u = 1 − √0.20`, so the last lit frame is ~22 ms
/// after the paw and the pool is clear at 40.
pub const CATCH_FINISH_MS: f32 = EVICT_FINISH_MS;

/// How many FINISHING stars the pool holds beyond [`STAR_CAP`] before the one
/// nearest the end of its finish is dropped outright. Eight evictions inside
/// one 40 ms window is > 200 births/s — nothing a hand or a landing produces —
/// so the bound exists to make the pool a fixed allocation (§18), not to
/// shape anything visible.
pub const STAR_FINISH_SLACK: usize = 8;

/// The pool's fixed capacity: [`STAR_CAP`] live plus [`STAR_FINISH_SLACK`]
/// finishing. Reserved once in [`Stardust::new`]; the steady frame path never
/// allocates (§18).
pub const STAR_POOL_MAX: usize = STAR_CAP + STAR_FINISH_SLACK;

/// The strike deal: one in this many typed cells gets an m3 grain (§5.6 —
/// "1-in-2 typed cells": the ribbon head cell is the per-key light, the grain
/// is the accent). The three strike rates are ONE draw per cell, bucketed
/// m1 / m2 / m3 / none in that order (see [`Stardust::deal_typed`]), so a
/// typed cell carries at most one strike star.
pub const DEAL_M3_IN: u32 = 2;

/// One in this many typed cells gets an m2 (§5.6).
pub const DEAL_M2_IN: u32 = 4;

/// One in this many typed cells gets a DEALT m1 hero (§5.6). A dealt hero with
/// no token degrades to m2 (D5); an EARNED one does not.
pub const DEAL_M1_IN: u32 = 12;

/// One in this many LAID ribbon cells carries a FIELD star — the ribbon's own
/// persistent starfield (§5.6; §4.2's refinement B: v1's per-cell stars,
/// lifted out of the body into the sky), hashed stably per cell so the sky
/// does not shimmer as the ribbon re-plans.
///
/// **Was 4; re-derived to 2 from §5.6's stated outcome.** The judge measured
/// max 9 / mean 5 live at 14.3 cps (`A/frame_0030`; the capture's
/// `stats.csv` pool column reads 3-9) against §5.6's "typical at 12 cps ≈ 20"
/// and §18's "~120 quads, 6 halos". The pool count is the deal × life
/// arithmetic and nothing else — every birth the table deals is born (the
/// probe passed, the cap never bound, no pixel was refused, the hashes are
/// uniform): `live = cps · Σ deal · life`, and §5.6's strike table
/// (1-in-12 · 460 + 1-in-4 · 340 + 1-in-2 · 225 ms, plus the second grain
/// at 225) tops out at 5.5 at 12 cps. The ≈ 20 can only be the FIELD — the
/// population that "rides the cell's own envelope" and "dies with its cell"
/// — and it was being born with the GRAIN's 225 ms class life
/// ([`FIELD_LIFE_S`] is the fix), which left 0.7 field stars live. With the
/// field on its swoosh-bounded life, 1-in-2 cells (half of them m2,
/// [`FIELD_M2_IN`]) gives `12 · ½ · 1.54 = 9` field + 5.5 strike ≈ 15 live
/// at 12 cps hot (12 cold), 41-50 % of them m2/m1 (§5.6's ≥ 40 %), ≈ 125
/// quads and 6 halos — §18's typical row, to the number. 1-in-4 at the same
/// life gives 10 live at 18 % bright, which fails §5.6's own magnitude law.
pub const FIELD_DEAL_IN: u32 = 2;

/// …and one in this many of THOSE is an m2 rather than an m3 (§5.6).
/// **Was 12; re-derived to 2** with [`FIELD_DEAL_IN`]: at 1-in-12 the field
/// is 92 % grains, and once the field lives its cell's span the live sky is
/// 18 % m2/m1 against §5.6's ≥ 40 %; at 1-in-2 it is 41 % hot and 50 % cold,
/// and the haloed count lands on §18's "6 halos" typical.
pub const FIELD_M2_IN: u32 = 2;

/// **THE HOT FIELD DEAL** (owner, 2026-09-08: "make this rainbow theme truly
/// magical and special and dynamic") — one in this many laid cells carries a
/// field star once the honest spine is at or over [`FIELD_HOT_DISP`]: every
/// cell. Momentum is bought as DENSITY, on the honest metric the second
/// grain already reads (`ctx.disp`, never the resume-floored birth spine —
/// a resume must not buy a denser sky it did not earn).
///
/// Rates per typed key, as dealt — the strike deal (`1/12 + 1/4 + 1/2 =
/// 0.83`), the second grain (`1.0` at `disp ≥ 0.5`) and the field: cold
/// `0.83 + 0.50 = 1.33`; `disp ≥ 0.5` `2.33`; `disp ≥ 0.7` `0.83 + 1.0 +
/// 1.0 = 2.83`. As BORN, measured on
/// `a_hot_hand_deals_every_cell_a_field_star` (sixty keys on a probed-blank
/// sky, stars born on the key's own edge): **cold 1.15, hot 2.73 per key**
/// (hot was 2.15 before this deal) — the gap to the dealt rate is the hero
/// spacing law and the one-pixel birth clearance, which stand. The 12 cps
/// pool (`a_twelve_cps_sky_holds_about_twenty_stars`) is measured there,
/// under the 40 cap and §18's quad budget.
pub const FIELD_DEAL_IN_HOT: u32 = 1;

/// The spine at or above which the field deal is [`FIELD_DEAL_IN_HOT`]
/// instead of [`FIELD_DEAL_IN`].
pub const FIELD_HOT_DISP: f32 = 0.7;

/// **THE SKY OPENS** — the share of laid cells that carry a field star at
/// flow's `heat == 1`: every one of them, exactly [`FIELD_DEAL_IN_HOT`] read
/// as a probability.
///
/// The hot deal above is a STEP at `disp ≥ 0.7`; flow's is a LERP from the
/// cold share ([`FIELD_SHARE_COLD`]) to this one over
/// [`super::Flow::heat`], and the two are folded with a `max`, so at
/// `heat == 0` the field deal is EXACTLY what it was (the same probability,
/// against the same hash, on the same cells — the byte-identity a cold frame
/// is pinned on) and flow can only ever open the sky further. What it buys
/// that the step could not: an open theme holds the sky open through the
/// dips — a hand at speed whose honest `disp` breathes under 0.7 between
/// words keeps its grain, because the RUN has not broken.
pub const FIELD_SHARE_FLOW: f32 = 1.0 / FIELD_DEAL_IN_HOT as f32;

/// The share of laid cells that carry a field star at `heat == 0` — the
/// other end of [`FIELD_SHARE_FLOW`]'s lerp, and [`FIELD_DEAL_IN`] read as a
/// probability.
pub const FIELD_SHARE_COLD: f32 = 1.0 / FIELD_DEAL_IN as f32;

/// **THE COMBO LADDER'S FIRST RUNG** — every 16th key of a run takes an m2
/// where the strike deal would have given it a grain or nothing (§23's
/// addendum "Flow state", 2026-09-09).
///
/// The ladder is flow's PAYOUT and it is deterministic: it is not a deal, it
/// is a debt the run has earned, so a rung never draws and never leaves its
/// key dark. It never LOWERS a magnitude either — a 16th key that already
/// drew an m1 keeps it — and every rung is dealt through
/// [`Stardust::deal_star`] like any other star, so the spacing law, the row
/// cap, the one-star-per-cell law, the live cap and the quad budget all hold
/// over it exactly as they hold over the deal it replaces.
pub const LADDER_M2_KEYS: u32 = 16;

/// **THE SECOND RUNG** — every 32nd key of a run takes a WHITE m2: the
/// A-type white the tint deal gives 10 % of stars ([`TINT_WHITE_RGB`]), here
/// by rule instead of by draw, so the second rung reads as a different mark
/// from the first and not merely a repeat of it.
pub const LADDER_WHITE_KEYS: u32 = 32;

/// **THE TOP RUNG** — every 64th key of a run takes a GOLD m1
/// ([`TINT_GOLD_RGB`]), EARNED: an earned hero is never demoted by the
/// spacing law or by a dry bucket (§5.8, D5), so the rung always pays. It
/// chimes only if a token is on hand, which is the glint law unchanged (D5)
/// — the light is not rationed, the sound is. Past the first one, every
/// 64th also fires the caret flare (`Engine::tick`).
pub const LADDER_GOLD_KEYS: u32 = 64;

/// **A FIELD STAR'S LIFE, s** (§5.6's Field row: "rides the cell's own edge
/// envelope × retract — alpha only"; "its field stars die with their
/// cells").
///
/// The field population is the ribbon's own starfield: a star parked in the
/// sky of a laid cell, pinned, lit while the cell is. It was born with the
/// GRAIN's 225 ms class life — `12 · ¼ · 0.225 = 0.7` live at 12 cps — which
/// is the whole of the judge's "sparse" (see [`FIELD_DEAL_IN`]). A star
/// cannot read its cell per frame (the emit seam carries no ribbon), so its
/// life is the cell's own SHAPE — `edge-in` × the ribbon's `expiry_melt`
/// — over the one horizon at which the cell is guaranteed gone without an
/// event: the exit swoosh's whole span after the star's own birth
/// (`ribbon::SWOOSH_TOTAL_S`, 1.54 s: grace + reach + retract + fade). A
/// hot run's cells outlive it (2-8 s), so under a moving hand the sky
/// trails the hand by ~1.5 s rather than carpeting the whole ribbon.
///
/// **This is the ceiling under a MOVING hand, not the fade after it stops.**
/// The melt over this span is exactly 1.0 for its first 70 %, and a star
/// that rode it alone held near-full brightness for ~1.2 s after the last
/// key while the ribbon under it spent its grace and was drawn back into
/// the caret (live capture, 2026-09-05, defect 4: 12 of 45 typing tracks
/// past 460 ms, max 1264 ms). The field's alpha after the hand stops is the
/// field FADE — [`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`], clocked from the last
/// key on the star's row ([`Star::lit`]) — which culls it 0.88 s after that
/// key, before the ribbon's retract moves; the retract half of §5.6's row
/// stays the erase edge's finish ([`Stardust::on_event`]), as before.
pub const FIELD_LIFE_S: f32 = SWOOSH_TOTAL_S;

/// **A FIELD STAR'S HOLD AFTER THE LAST KEY, ms** — the `H` of the field
/// lane's own `half-life` envelope (§5.6's table: `α = 1` for a hold `H`,
/// then τ), clocked from the LAST KEY ON THE STAR'S ROW ([`Star::lit`])
/// rather than from birth, because §5.6's Field row is the one population
/// whose envelope is the CELL's ("rides the cell's own edge envelope ×
/// retract — alpha only"): a cell is lit while the hand is on its row. The
/// ribbon's answer to "how long after the last key is the hand still here"
/// is its 0.75 s grace; a star that waited that long could not then die
/// inside §5.2's "few and bright, die by alpha", so the field's answer is
/// this hold — long enough to cover the gap between two keys of ordinary
/// typing (300 ms ≥ 3.3 cps: at 12 cps the sky never leaves it, and the
/// re-light on each key ([`Stardust::relight_field`]) is a hold EXTENDED,
/// not a re-brighten, so the field does not pump with the hand the way v1's
/// stars blinked in unison) — and no longer.
///
/// **Measured on glass (live capture, 2026-09-05, defect 4).** 12 of 45
/// typing star tracks lived past §5.2's longest class life (460 ms), max
/// 1264 ms: the crosses over "sparkles" held near-full brightness ~1.2 s
/// after the last key. The field's alpha was the cell's end-of-life melt
/// over the swoosh horizon ([`FIELD_LIFE_S`]) — exactly 1.0 for the first
/// 70 % of 1.54 s — while the ribbon under it had spent its grace and was
/// being drawn back into the caret. A star goes out from the hand, not from
/// the horizon.
pub const FIELD_HOLD_MS: f32 = 300.0;

/// **…and its half-life, ms.** With [`FIELD_HOLD_MS`]: `α = ½` at 550 ms
/// after the last key, 0.33 at 700 ms (the measurer's bound: no field star
/// over half its last-key brightness past 700 ms), and the cull
/// ([`STAR_CULL_ALPHA`], `H + 2.32·τ`) at 880 ms — before the ribbon's own
/// retract starts moving (`ribbon::RETRACT_START_S`, 900 ms), so the sky is
/// dark by the frame the mark is drawn back into the caret and nothing
/// hangs over the swoosh. Pinned by
/// `field_stars_dim_with_the_ribbon_they_were_born_over` and
/// `the_field_is_dark_before_the_retract_moves`.
pub const FIELD_TAU_MS: f32 = 250.0;

/// The field fade's whole span after the last key, seconds — `H + 2.32·τ`
/// of [`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`] through the one
/// [`half_life_life_s`] every class life is derived by (0.88 s): the instant
/// [`Star::dead`] drops a field star whose row the hand has left, and the
/// end of the theme's one linear fade under reduced motion.
#[inline]
#[must_use]
pub fn field_fade_life_s() -> f32 {
    half_life_life_s(FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0)
}

/// The spine above which a strike also throws a SECOND m3 from the caret's
/// leading edge (§5.6) — fast typing makes the sky denser, on the honest
/// metric.
pub const STRIKE_SECOND_M3_DISP: f32 = 0.5;

/// Minimum spacing between heroes, in cells (§5.2). Two heroes on adjacent
/// cells read as one fat mark, not as two stars.
pub const HERO_MIN_SPACING_CELLS: u16 = 3;

/// **ONE SKY STAR PER CELL — the strike's reach when its cell is taken**,
/// in cells toward the caret's leading edge: one, the lead cell, which is
/// the cell the second grain already reaches and the cell the next key
/// will lay. A strike dealt onto a cell that already carries a live sky
/// star (the previous key's leading-edge grain, under a hot hand: every
/// key) moves to the first free cell within this reach or is dropped; the
/// cell keeps the star it has. Two cells ahead would put a star over glass
/// the hand has not reached.
///
/// The bold round's open finding: the hot deal put two or three stars
/// within a pixel or two on ONE cell (M1/Strike + M2/Field + the previous
/// key's M3/Strike), each priced only over the sky laid BEFORE it, so a
/// later star's core on an earlier star's pixels was unpriced — worst
/// luminance 99.6–101.7 over the caret law's 72 on 8 of 40 frames of the
/// 12 cps fixture. Fixed by design, not by dimming: a cell has one star
/// ([`Stardust::sky_star_live`]), the field deal ADOPTS the star a cell
/// already has ([`Stardust::deal_typed`]) instead of stacking a second,
/// and the strike moves within this reach. Measured after:
/// `one_sky_star_per_cell_and_no_pile_outshines_the_caret`.
pub const STRIKE_REACH_CELLS: u16 = 1;

/// At most this many heroes live on one row at a time (§5.2).
pub const HERO_MAX_LIVE_PER_ROW: u8 = 2;

/// Share of m1/m2 stars whose tint is `spectrum_snap(field at the birth
/// cell)` (§5.3) — the sky inherits the spectrum's order along the line: warm
/// stars over the old end, violet over the new. **Was 0.60; now 0.75**
/// (owner, 2026-09-08: more colour) — the A-type white share is what it
/// gave up; gold keeps its 15 %, because gold is the glint class. A GRAIN is
/// not dealt at all: every m3 wears its cell's stop ([`class_tint`]).
pub const TINT_FIELD_SHARE: f32 = 0.75;

/// Share of m1/m2 that are `#FFFFFF` — an A-type star; halo and stubs white
/// (§5.3). **Was 0.25; now 0.10.**
pub const TINT_WHITE_SHARE: f32 = 0.10;

/// **AN m2'S BODY IS TINTED** — how far from white toward the star's tint
/// its `push_twinkle_star` body and nucleus are drawn (owner, 2026-09-08:
/// "m2 tinted cores (colour at ~60 % into the core)"). The stack still
/// composites to §5.2's 130 on the tint's own peak channel (the peak law is
/// measured on the brightest channel), and the centre now carries a channel
/// spread of `0.6 · 113 = 68` levels where it carried none. The hero keeps
/// its white-hot centre (its colour is its halo); the grain is pure tint.
pub const M2_BODY_TINT: f32 = 0.6;

/// Share that are `#FFFF00` gold — **the only class that carries diagonal
/// glints** (§5.3). The three shares sum to 1.
pub const TINT_GOLD_SHARE: f32 = 1.0 - TINT_FIELD_SHARE - TINT_WHITE_SHARE;

/// The gold tint itself (§5.3), and the only value of [`Star::tint`] for which
/// `push_twinkle_star`'s `gold` argument may be true.
pub const TINT_GOLD_RGB: u32 = 0x00FF_FF00;

/// The A-type white tint (§5.3), and the body/nucleus colour of EVERY class
/// (D2).
pub const TINT_WHITE_RGB: u32 = 0x00FF_FFFF;

/// How many of the seven ROYGBIV stops count as WARM for §5.5's φ + π flip:
/// red, orange, yellow. Green, blue, indigo and violet are cool, and a warm
/// star and a cool star on adjacent cells are therefore exactly antiphase —
/// which is the mechanism that stops a field blinking in unison.
pub const WARM_STOPS: usize = 3;

/// Top of the sky band above the ribbon, in `ch` above [`super::ribbon::Band`]'s
/// `top` (§5.4). Under the shipped `tall` spelling `[0.30, 0.04]` is the lower
/// third of row − 1; under `underline` it is exactly the old `[0.02, 0.30]·ch`
/// of the cell — one zone, two spellings (D16).
pub const SKY_TOP_CH: f32 = 0.30;

/// …and its bottom, in `ch` above the band's top (§5.4).
pub const SKY_BOTTOM_CH: f32 = 0.04;

/// Horizontal jitter of a birth about the cell centre, in `cw` (§5.4, hashed).
pub const SKY_JITTER_CW: f32 = 0.3;

/// **THE 1× CELL — the reference every device-pixel size in this file is
/// ruled at**, in `ch`: the owner's cell is 7×14 device px at 1× and 15×28
/// on the retina screen he runs. Every `*_PX` number below (the stub
/// length, the m2's arm floor and halo floor, the disc's shoulder, the
/// lift, the throw reaches, the shed's sputter) is stated AT THIS CELL and
/// scaled by [`px_scale`] where it is spent, exactly as `star_arm` scales
/// the arm — so the 1× picture is byte-identical to what was ruled, and the
/// 2× picture is the same picture twice the size (measured:
/// `the_sky_at_two_x_is_the_sky_at_one_x_twice_the_size`). Before this the
/// sizes were literal device px, and on the retina cell the sky was a
/// quarter of the intended AREA with stars a quarter of the intended size
/// (the halo radii rode the clocked INTEGER arm, 3 → 5 px from 1× to 2×,
/// so a hero's halo grew 9 → 15 where the picture asked 16).
pub const CELL_1X_CH: f32 = 14.0;

/// The device-pixel scale of a cell of height `ch` against the 1× cell:
/// `ch / CELL_1X_CH` — 1 at 1×, 2 on the retina cell, 9/7 at the test
/// fixture's `ch` 18.
#[inline]
#[must_use]
pub fn px_scale(ch: f32) -> f32 {
    ch / CELL_1X_CH
}

/// An INTEGER pixel size ruled at 1×, at cell height `ch`: truncated, as
/// `star_arm_px` truncates the arm (so a size never rounds UP into a bar
/// on a fractional scale), floored at the one pixel any mark needs. Exact
/// at 1× and at 2×.
#[inline]
#[must_use]
pub fn px_at(ch: f32, at_1x: i32) -> i32 {
    ((at_1x as f32 * px_scale(ch)) as i32).max(1)
}

/// Lower bound of the seeded ARM-SIZE scintillation rate, Hz (§5.5, D12).
pub const SCINT_F_MIN: f32 = 8.0;

/// Upper bound of the seeded arm-size rate, Hz (§5.5, D12).
pub const SCINT_F_MAX: f32 = 12.0;

/// The photosensitivity EXEMPTION bound, device px at 2× (§5.5): a mark whose
/// bbox is at or under this may scintillate in SIZE at 8-12 Hz, because a
/// general-flash threshold is an AREA threshold. LUMINANCE of every mark, at
/// every size, stays on `twinkle_env`'s 2.86 Hz clock — no exemption there.
pub const SCINT_BBOX_MAX_PX: i32 = 10;

/// The m2's INTEGER ARM FLOOR, px (§5.2: `star_arm(ch, 1.0) = 2.5 → 2 px`;
/// §5.5's table: "m2 … (2 ↔ 3 px)"). The arm clock's formula alone,
/// `round(2.52 · (0.55 + 0.45·sin²))`, reaches 1 at its trough for a fifth of
/// every cycle, and a 1-px arm is the GRAIN's silhouette — the m2 ↔ m3 step
/// would flicker. The published table is the law, so the floor is stated once
/// here and read by [`StarClass::arm_min_px`], which scales it by the cell
/// ([`px_at`]: 2 at 1× and at `ch` 18, 4 at 2× — where the grain's arm
/// reaches 3). m1 (`3.53 · 0.88 = 3.1 → 3`)
/// and m3 (`1.89 · 0.55 = 1.04 → 1`) already round onto their published
/// bands without one.
pub const M2_ARM_MIN_PX: i32 = 2;

/// Light-theme count scale (§5.2): **counts × 0.6 in every population** —
/// strike, field, erase, shed, fan and mini-fan — because area buys "bigger"
/// on light and area is the thing that must not accumulate (L6). The sky
/// deals scale their probability ([`deal`]); the thrown populations scale
/// their census ([`light_count`]), never below the slots a law fixes (the
/// fan's hero + 4 m2, D6; the mini-fan's m2, D17; the erase m2).
pub const LIGHT_COUNT_SCALE: f32 = 0.6;

/// Coverage divisor for a TRANSIENT star whose CURRENT pixel sits over a
/// probed-occupied or UNKNOWN cell (§5.2: "divided by 3 — v1's text-safe arm,
/// kept"): a fan star drifting over a glyph, a shed fragment on a row nobody
/// probed.
///
/// **A sky star never takes it** (`Paint::of` gates it on
/// [`StarLane::is_transient`]). Its cell was proven blank at birth — §5.4's
/// gate is the whole of its clearance — and it lifts ≤ 2 px in life, so the
/// probe's answer for it can only ever CHANGE by being forgotten: the host's
/// three-row probe evicts row − 1 the moment the caret steps down a line, and
/// a scroll clears it outright. Re-asking per frame would drop every live
/// star to a third on Enter and on every prompt scroll — a 3× luminance step
/// that breaks T5 ("never pops") and C3 (alpha-only fade). Pinned by
/// `a_sky_star_keeps_its_centre_when_the_probe_forgets_its_row`.
pub const TEXT_SAFE_DIV: f32 = 3.0;

// ---------------------------------------------------------------------------
// 1a. THE COVERAGE REQUESTS — §5.2's two ceilings, one floor
// ---------------------------------------------------------------------------
//
// `c` is the coverage request handed to the class's recipe. There are two of
// every number because the two lanes composite over different grounds: a sky
// star lives ABOVE the ribbon for 225-460 ms and is priced by the sparkle law
// ([`FIELD_STAR_COV_CEIL`]); a transient star flies OVER INK for ≤ 340 ms and
// is priced by [`TRANSIENT_STAR_COV_CEIL`]. One family, two prices.

/// The m1 hero's coverage request in the SKY lane (§5.2). `2.35 · 61 = 143`,
/// which IS [`FIELD_STAR_COV_CEIL`] — the hero is the class the ceiling was
/// written for, and nothing in the sky is allowed to be brighter.
pub const M1_COV_SKY: f32 = 61.0;

/// The m2's coverage request in the sky lane (§5.2). `2.70 · 48 = 130`.
pub const M2_COV_SKY: f32 = 48.0;

/// The m3 grain's coverage request in the sky lane (§5.2). `1.6 · 61 = 98`.
///
/// Equal to [`M1_COV_SKY`] and that is not a copy-paste: the grain has no
/// crossing bars and no nucleus to stack, so the SAME request buys it 98
/// where it buys the hero 143. The class's shape, not its request, is what
/// grades the sky.
pub const M3_COV_SKY: f32 = 61.0;

/// The m1's request in the TRANSIENT lane (§5.2). `2.35 · 50 = 118`, exactly
/// [`TRANSIENT_STAR_COV_CEIL`] — the pin-nucleus brightness class.
pub const M1_COV_TRANSIENT: f32 = 50.0;

/// The m2's request in the transient lane (§5.2).
pub const M2_COV_TRANSIENT: f32 = 34.0;

/// The m3's request in the transient lane (§5.2). `1.6 · 38 = 61`.
pub const M3_COV_TRANSIENT: f32 = 38.0;

/// The m2's 3×3 disc, as a share of `c` (§5.7). It is the ONE thing an m2 adds
/// on top of `push_twinkle_star`'s own stack, and it is a disc rather than a
/// second core rect because D1 forbids a duplicate core: the disc widens the
/// peak's SHOULDER by one pixel each way, which is what separates an m2 from a
/// small m1 at a glance.
pub const M2_DISC_ADD: f32 = 0.35;

/// The m3's four ARM rects, as a share of `c` — the grain's Airy disc, and
/// **the grain's tint** (§5.7, D2): a coloured disc around a white core is
/// the astronomer's own picture. Each arm runs `arm` px OUTBOARD of the 1-px
/// core (four quads, [`M3_QUADS`]), never through it, so the centre pixel is
/// the white core alone and D2's "the peak is white in every class" is exact
/// rather than approximate.
///
/// **Was 0.30 (white, drawn as two bars through the centre); now 0.45,
/// tinted.** The judge's threshold was > ground + 24, and every grain
/// registered as a 1×1 component (`A/frame_0030`: sizes 1,1,1,1,1): at
/// `0.30 · 61 = 18` levels the disc was under it, so a grain read as a speck,
/// not a star. At `0.45 · 61 = 27` (transient `0.45 · 38 = 17`) it clears the
/// threshold with its tint intact — 27 levels of a pure stop is a chroma of
/// 27 — and it rides ALPHA only, not the twinkle: the white core breathes on
/// `env` (full depth, §5.5) over a steady coloured disc, which is §5.5's
/// chromatic scintillation for the grain. The peak law holds at every phase:
/// 98 : 27 at the crest, 54 : 27 at the trough (≥ 1.3 either way).
pub const M3_CROSS_ADD: f32 = 0.45;

/// The m3's composited CENTRE as a multiple of `c` — §5.2's published `1.6·c`
/// (98 sky / 61 transient). With the disc outboard the core rect is priced at
/// this directly; nothing stacks on the centre pixel.
pub const M3_CENTRE_ADD: f32 = 1.6;

/// The four outboard TINT STUBS, as a share of `c` (§5.7) — each arm's
/// coloured tip, strictly outside `star_body_px(arm)`, because
/// `push_twinkle_star` draws body AND nucleus in ONE colour (D2) and the
/// stubs plus the halo are therefore the only places a star's temperature
/// can live.
///
/// **Was 0.6; now 1.0.** The judge measured the AFTER sky at saturation p50
/// 7, 89-100 % grey (`A/frame_0030`, `A/frame_0100`): a 1-px stub at
/// `0.6 · 61 = 37` under a white taper point, and `0.6 · 48 = 29` for an m2,
/// never cleared the eye on the (17, 19, 24) ground — one blue-stubbed m2 was
/// the only colour on glass. At the full request the tip is 61 / 48 levels of
/// pure tint (a red tip composites to `(86, 25, 25)` over the point's last
/// white pixel), and [`STUB_LEN_PX`] makes it a tip rather than a dot.
pub const STUB_COV_SHARE: f32 = 1.0;

/// How long each tint stub is, px AT THE 1× CELL ([`CELL_1X_CH`]), running
/// OUTBOARD from `star_body_px(arm) + 1` — two, so the arm visibly ENDS in
/// colour (the judge's "monochrome" is exactly one tinted pixel lost under
/// a white point). Still four quads per star. Spent through
/// [`stub_len_px`]: 2 at 1× and at `ch` 18, 4 on the retina cell.
pub const STUB_LEN_PX: i32 = 2;

/// [`STUB_LEN_PX`] at cell height `ch` ([`px_at`]).
#[inline]
#[must_use]
pub fn stub_len_px(ch: f32) -> i32 {
    px_at(ch, STUB_LEN_PX)
}

/// **§3.2 RANK 8, RE-DERIVED: THE HALO MAY NOT OUTSHINE THE CORE.** A halo's
/// peak byte stays at or under this share of the composited centre it
/// belongs to — ONE, i.e. the core is still the brightest pixel of its star
/// on every channel the halo lights. It was 0.14 ("≤ 14 % of the core"),
/// under which the owner measured the sky as white dust with a tint
/// (A/B #15, reversed 2026-09-08: "I want ... a bigger more special rainbow
/// impact"). The measurable reading of "colour on glass" is the scanner's:
/// a pixel with a channel spread ≥ 40 at a max channel ≥ 60. At 14 % of a
/// 143 core (19 levels, falling off from the centre) no halo pixel reached
/// it; at the shares below the ring of halo pixels just outside the body
/// does (`stardust_is_coloured_on_the_glass_not_white_with_a_tint`). The
/// build refuses a share over the cap (see the `const _` block below).
pub const HALO_CAP_SHARE: f32 = 1.0;

/// The m1 halo's peak, as a share of `c` (§5.7).
///
/// **Was 0.31; now 1.00** — the hero's whole request, `61` levels in the
/// sky and `50` transient, 43 % of the 143 core (the old 0.31 was 13 %, the
/// 14 % cap's ceiling). Derived from the scanner's floor: the falloff at
/// `d = 4` px on an 11 px halo weighs `0.65`, so a pure-stop halo needs a
/// peak of `40 / 0.65 = 62` levels to put a spread-40 pixel just outside the
/// hero's white body; the request is the nearest number the recipe already
/// has. Note that the `RainHalo` falloff weighs `256/256` at its centre —
/// the peak IS added on the core pixel in the composite — so the hero's
/// centre ASKS for `143 + 61` on the tint's channel and reads white-hot
/// with a coloured heart; the [`FIELD_STAR_COV_CEIL`] ceiling binds the
/// RECIPE's stack, as it always did (rank 8 was never a ceiling on the
/// composite), and the sky lives above the text, where nothing is priced
/// against a glyph. **What the centre GETS is priced by the caret's law**
/// ([`STAR_LIGHT_CEIL`], [`price_centre`]): the request is the whole of it
/// on a ground with the room, and on the shipped ground the white body
/// cedes luminance to the colour rather than the colour to the body.
pub const M1_HALO_SHARE: f32 = 1.0;

/// The m2 halo's peak, as a share of `c` (§5.7). **Was 0.30; now 0.80** —
/// `0.80 · 48 = 38` levels in the sky (29 % of the 130 core), `27`
/// transient (34 % of 80): a 6 px halo at 38 puts `0.56 · 38 = 21` levels of
/// tint three pixels out, on top of the body's own 60 % tint, where 14 put
/// four.
pub const M2_HALO_SHARE: f32 = 0.80;

/// The m1 halo's radius as a multiple of its NOMINAL arm — `star_arm(ch,
/// STAR_ARM_HERO)`, the float, not the clocked integer (§5.7). **Was 2.6
/// (9 px at `ch` 18); now 3.0 — 8 px at 1×, 11 at `ch` 18, 16 at the
/// retina cell** (owner: "bigger"). Not more: the halo's foot is the row
/// the caret is on, and the ledger prices what lands on a glyph cell
/// (§3.4).
///
/// **Off the integer arm, for two reasons.** The clocked arm truncates
/// differently at every cell (3 at 1×, 5 at 2×: a 9 → 15 halo where the
/// picture asks 8 → 16), and it steps at the seeded 8–12 Hz — which put a
/// 33-px mark's SIZE on the arm clock, outside D12's exemption (bbox ≤ 10
/// px at 2×). The halo's size is now the cell's; its luminance and its hue
/// still breathe on the 2.86 Hz clock ([`halo_rgb`]).
pub const M1_HALO_R_ARM: f32 = 3.0;

/// The m2 halo's radius as a multiple of its NOMINAL arm (`star_arm(ch,
/// STAR_ARM_STD)`, §5.7), floored at [`M2_HALO_R_MIN_PX`]. **Was 1.6; now
/// 2.4** — under the floor at every cell (4.7 px at 1×, 6 at `ch` 18, 9.4
/// at 2×), so the floor IS the m2's halo: 6 / 7.7 / 12 px.
pub const M2_HALO_R_ARM: f32 = 2.4;

/// The m2 halo's radius floor in px AT THE 1× CELL (§5.7's `max(6,
/// 2.4·arm)`) — below 6 px a radial falloff has no room to fall off around
/// a 5 px body and reads as a second, softer core. **Was 4.** Spent through
/// [`m2_halo_r_min_px`]: 6 at 1×, 12 on the retina cell.
pub const M2_HALO_R_MIN_PX: f32 = 6.0;

/// [`M2_HALO_R_MIN_PX`] at cell height `ch` ([`px_scale`]).
#[inline]
#[must_use]
pub fn m2_halo_r_min_px(ch: f32) -> f32 {
    M2_HALO_R_MIN_PX * px_scale(ch)
}

/// The m2's 3×3 disc, as a SHOULDER: how far the tinted disc reaches past
/// the centre pixel on each axis, px at the 1× cell — one, so the disc is
/// `(2·1 + 1)² = 3×3` at 1× and at `ch` 18, and `5×5` on the retina cell
/// ([`m2_disc_shoulder_px`]). §5.7's `push_fx_rect(sx−1, sy−1, 3, 3)`,
/// scaled.
pub const M2_DISC_SHOULDER_PX: i32 = 1;

/// [`M2_DISC_SHOULDER_PX`] at cell height `ch` ([`px_at`]).
#[inline]
#[must_use]
pub fn m2_disc_shoulder_px(ch: f32) -> i32 {
    px_at(ch, M2_DISC_SHOULDER_PX)
}

/// **THE HALO BREATHES ITS HUE** (owner, 2026-09-08: "the twinkle has more
/// colour in it") — how far, in `SPECTRUM_LUT` entries, a halo's colour
/// walks from its star's stop on the luminance clock: `+15` at the twinkle
/// peak, `−15` at the trough, on the arc itself ([`halo_rgb`]). The nearest
/// two anchors are 63 entries apart (`SPECTRUM_ANCHOR_AT`), so the halo
/// never wears a neighbouring name; `15/510 ≈ 0.03` of the walk is a hue
/// shift of a few degrees — a breath, not a change of colour. Gold and the
/// A-type white have no stop to breathe around and hold their colour.
pub const HALO_HUE_SWING_ENTRIES: i32 = 15;

/// **THE CARET LAW, ON THE STAR'S OWN CENTRE** (`cursor_glow` §8 d, *"the
/// brightest pixel of the frame is under the cursor — you must always be
/// able to find your cursor"*): the relative luminance NO pixel a sky star
/// composites may exceed, over the ground it actually sits on.
///
/// **DERIVED FROM THE CARET'S DIMMEST LAWFUL STATE, not chosen.** The
/// block's fill is lifted to a luminance floor of
/// [`RAINBOW_CARET_LIGHT_FLOOR`] (`80/255`) the moment the colour envelope
/// clears its quarter-range knee — that is the least light the caret ever
/// carries while a starfield is alive, at every energy, flare or no flare
/// (the flare and the rim-headroom law only ever lift it HIGHER) — and the
/// field is allowed [`RAINBOW_SPARKLE_LIGHT_SHARE`] of it: `Y = 72/255`,
/// eight levels of luminance under the dimmest caret, which the gate holds
/// less the two levels its own byte roundings can eat. Over black a white
/// pixel at this luminance is level `145` — `RAINBOW_FIELD_LEVEL`, the very
/// number `61 = 145 / 2.35` and so [`M1_COV_SKY`] were solved from. That
/// solve was over BLACK and it priced the STACK alone: over the shipped
/// ground (`#1A1B26`) the same luminance is a white add of `117`, and the
/// hero's full-share halo stacks another `M1_HALO_SHARE · c` on the very
/// centre pixel — measured, an A-type white hero at the default intensity
/// composited to `#ACABB9`, `Y = 106`, eleven levels OVER a caret at `95`.
///
/// **IT IS AN ORIENTATION LAW, NOT COLOUR RESTRAINT.** Saturation costs no
/// luminance: a pure stop at this `Y` has a far higher channel than a grey
/// at it, and reads as colour. So [`price_centre`] spends the ceiling
/// COLOUR FIRST — a tinted halo (and the m2's tinted disc) keeps its whole
/// request and its whole saturation, and the WHITE body takes the luminance
/// left; only an achromatic star (the A-type white, whose halo is more
/// white) prices its body first and lets its halo have the remainder. On a
/// ground already over the ceiling (not a dark theme in any sense the caret
/// floor was written for) the request stands untouched: the law is the
/// caret's to keep there, and the field ledger's.
///
/// Held per star, per frame: PRICED on the composited centre — the two
/// crossing bodies, the nucleus, the disc and the halo's peak, in the
/// rasterizer's own bytes ([`price_centre`]) — and then SETTLED on the
/// pixels its recipe stacks light on away from the centre, read back from
/// its own quads ([`settle_under_ceiling`]); both over [`sky_ground`], the
/// theme ground plus everything the sky has already laid under this star
/// this frame (the aurora, and the stars drawn before it).
pub const STAR_LIGHT_CEIL: f32 = RAINBOW_CARET_LIGHT_FLOOR * RAINBOW_SPARKLE_LIGHT_SHARE / 255.0;

// ---------------------------------------------------------------------------
// 1a′. THE AURORA — the veil above the live ribbon (owner, 2026-09-08)
// ---------------------------------------------------------------------------
//
// "make this rainbow theme truly magical and special and dynamic and
// beautiful" — a soft rainbow glow in the sky band above the live ribbon,
// carrying the ribbon's own spectrum walk so the two read as ONE rainbow
// rising from the line. One [`Veil`] cell per laid cell, born on the key
// that laid it, priced by momentum at birth, riding the FIELD lane's
// envelope ([`field_lane_envelope`]) so it holds while the hand is on the
// row, goes out from the hand, and is gone before the ribbon's retract
// moves — "the aurora leaves with the ribbon". It sits strictly ABOVE the
// glyph row (never a pixel of a glyph cell's own rows) inside the cell's own
// column, and only over a cell whose sky the probe proved blank, so it costs
// legibility nothing.

/// The veil pool's fixed capacity — reserved once, never grown (§18). At 12
/// cps a cell's veil lives the swoosh horizon (1.54 s), so ~19 are live under
/// a hot hand; a 40-cell paste is bounded here by evicting the cell lit
/// longest ago.
pub const AURORA_CAP: usize = 64;

/// How tall the veil is, in `ch`, above the line — the box is
/// `[bottom − 0.35 ch, bottom)` where `bottom` is the sky band's own anchor
/// (the ribbon's top under `tall`, the glyph row's top under `underline`,
/// never lower than the glyph row's top). The brief's bound, exactly.
pub const AURORA_TALL_CH: f32 = 0.35;

/// The veil's peak coverage byte at full momentum, at the line — its
/// brightest pixel, since the falloff is centred on the box's bottom edge
/// and each cell's veil is clipped to its own column (no two veils sum).
/// "≤ ~40 coverage at its brightest": 40.
pub const AURORA_COV_PEAK: f32 = 40.0;

/// Below this BIRTH spine a key lays no veil at all — "invisible cold". Its
/// gain is `smoothstep((birth_disp − 0.15) / 0.85)`: `0.37` (15 levels,
/// "plain") at 0.5, `0.71` (29) at 0.7, `1.0` (40, "full") at 1.0. Priced
/// ONCE at birth from `Ctx::birth_disp`, exactly as the ribbon prices a
/// cell's `cov0`: a veil never brightens without a keystroke behind it.
pub const AURORA_COLD_DISP: f32 = 0.15;

/// The veil's horizontal falloff radius in `cw`, centred on the cell: at the
/// cell's own edge (`dx = 0.5 cw`) the weight is `0.88`, so a row of clipped
/// per-cell veils reads as one band with a 12 % scallop, not as beads.
pub const AURORA_REACH_CW: f32 = 2.0;

/// Veil halos stardust may draw on one frame — the pool's own size, one
/// `RainHalo` per live cell (the box lies in one cell row by construction,
/// so a veil never splits). Counted beside [`STARDUST_HALO_BUDGET`], which
/// stays the STAR halo budget; on light both spend [`STARDUST_LIGHT_HALO_BUDGET`].
pub const AURORA_HALO_BUDGET: usize = AURORA_CAP;

/// The composited centre of a sky m1 — `STAR_STACK_ADD · c` (§5.2, §3.2 rank
/// 4). Rank 4 in the luminance hierarchy: brighter than the coma, dimmer than
/// the meteor nucleus.
pub const CENTRE_M1_SKY: f32 = STAR_STACK_ADD * M1_COV_SKY;

/// The composited centre of a sky m2 — stack plus the [`M2_DISC_ADD`] disc.
pub const CENTRE_M2_SKY: f32 = (STAR_STACK_ADD + M2_DISC_ADD) * M2_COV_SKY;

/// The composited centre of a sky m3 — the core rect alone, priced at
/// [`M3_CENTRE_ADD`] (the arms are outboard and stack nothing on it).
pub const CENTRE_M3_SKY: f32 = M3_CENTRE_ADD * M3_COV_SKY;

/// The composited centre of a transient m1.
pub const CENTRE_M1_TRANSIENT: f32 = STAR_STACK_ADD * M1_COV_TRANSIENT;

/// The composited centre of a transient m2 — `STAR_STACK_ADD · c = 80`, the
/// number §5.2's transient table, §3.2 rank 4 and §20.1's oracle all publish.
///
/// **The transient m2 carries NO disc.** §5.2's transient row lists "body +
/// tapers, tinted stubs" and nothing else, and `2.35 · 34 = 80` is only true
/// without the disc (`2.70 · 34 = 92` with it). §5.7's "→ 130 / 80"
/// annotation is the same arithmetic — the disc is the sky m2's shoulder
/// (`113 + 17 = 130`) and the transient m2, flying over ink under the 118 cap,
/// keeps the stack alone. D1: every class's composited centre is the one
/// published in §5.2, so the disc is lane-gated in [`Paint::draw_add`].
pub const CENTRE_M2_TRANSIENT: f32 = STAR_STACK_ADD * M2_COV_TRANSIENT;

/// The composited centre of a transient m3 — `1.6 · 38 = 60.8`, published as
/// **61**. [`Paint::draw_m3`] prices the centre byte as `round(1.6·c)` in one
/// conversion, so the composite lands on the published integer.
pub const CENTRE_M3_TRANSIENT: f32 = M3_CENTRE_ADD * M3_COV_TRANSIENT;

/// **THE TWO CEILINGS HOLD BY CONSTRUCTION, NOT BY CLAMP** (§5.2, D1).
///
/// Every term below is a constant, so a retuned request that would break a
/// ceiling stops the build rather than shipping a star that is brighter than
/// the sparkle law allows. The envelope and `intensity` only ever scale these
/// DOWN, so there is no runtime clamp anywhere on the emit path and no place
/// for one to be forgotten.
const _: () = {
    assert!(
        CENTRE_M1_SKY <= FIELD_STAR_COV_CEIL
            && CENTRE_M2_SKY <= FIELD_STAR_COV_CEIL
            && CENTRE_M3_SKY <= FIELD_STAR_COV_CEIL,
        "a sky star's composited centre exceeds the sparkle law's ceiling"
    );
    assert!(
        CENTRE_M1_TRANSIENT <= TRANSIENT_STAR_COV_CEIL
            && CENTRE_M2_TRANSIENT <= TRANSIENT_STAR_COV_CEIL
            && CENTRE_M3_TRANSIENT <= TRANSIENT_STAR_COV_CEIL,
        "a transient star's composited centre exceeds the over-ink ceiling"
    );
    // §5.1 no. 2: the classes must actually be a DISTRIBUTION. Equal centres
    // would be one size again under three names.
    assert!(
        CENTRE_M3_SKY < CENTRE_M2_SKY && CENTRE_M2_SKY < CENTRE_M1_SKY,
        "the sky magnitudes must stay ordered m3 < m2 < m1"
    );
    // §3.2 rank 8, re-derived: the halo may not OUTSHINE the core — at the
    // top of the twinkle the core is `STAR_STACK_ADD · c` and the halo
    // `share · c`, and both lanes price the same shares, so one ratio covers
    // all four halos. Strict: the core is the peak.
    assert!(
        M1_HALO_SHARE < HALO_CAP_SHARE * STAR_STACK_ADD
            && M2_HALO_SHARE < HALO_CAP_SHARE * STAR_STACK_ADD,
        "a star halo outshines the core it belongs to (§3.2 rank 8)"
    );
    // The m2's tinted body still stacks to a peak that is the tint's own
    // channel at full, so its composited centre is unchanged on that channel.
    assert!(
        M2_BODY_TINT > 0.0 && M2_BODY_TINT <= 1.0,
        "the m2 body tint is a share of the way from white to the tint"
    );
    // The halo's hue breath stays inside its own stop's name: the nearest two
    // anchors of `SPECTRUM_ANCHOR_AT` are 63 entries apart (0 → 63), and a
    // breath of half that or more would let a red halo wear orange.
    assert!(
        HALO_HUE_SWING_ENTRIES > 0 && HALO_HUE_SWING_ENTRIES * 2 < 63,
        "the halo's hue breath reaches a neighbouring stop"
    );
    // The aurora: a coverage byte under the sparkle ceiling by a wide margin
    // (it is a veil, not a mark), and a pool the halo budget can carry.
    assert!(
        AURORA_COV_PEAK > 0.0 && AURORA_COV_PEAK <= 40.0,
        "the aurora's brightest pixel is over the brief's ~40"
    );
    assert!(
        AURORA_TALL_CH > 0.0 && AURORA_TALL_CH <= 0.35 && AURORA_HALO_BUDGET >= AURORA_CAP,
        "the aurora's box is taller than the brief's 0.35 ch, or its pool outruns its budget"
    );
    // D1 for the grain: its tinted disc must stay well under its white core
    // at the twinkle TROUGH too (`core · 0.55` against a disc that rides
    // alpha only), or the grain's peak would leave the centre for a fifth of
    // every cycle.
    assert!(
        M3_CENTRE_ADD * 0.55 >= 1.3 * M3_CROSS_ADD,
        "the grain's disc outshines its core at the twinkle trough"
    );
    // §5.3's deal must be a partition.
    assert!(
        TINT_GOLD_SHARE > 0.0 && TINT_FIELD_SHARE + TINT_WHITE_SHARE + TINT_GOLD_SHARE == 1.0,
        "the 75/10/15 tint deal must sum to one"
    );
    // §5.6's strike deal is ALSO a partition — one draw per cell, bucketed
    // m1 / m2 / m3 / none — so a cell can never carry two strike stars on
    // one pixel. Its three published rates must therefore fit inside one.
    assert!(
        1.0 / DEAL_M1_IN as f32 + 1.0 / DEAL_M2_IN as f32 + 1.0 / DEAL_M3_IN as f32 <= 1.0,
        "the 1-in-12 / 1-in-4 / 1-in-2 strike deals must fit inside one draw"
    );
};

// ---------------------------------------------------------------------------
// 1b. THE ENVELOPES — §5.6's `H` / `τ` / life table
// ---------------------------------------------------------------------------
//
// `α = 1` for a hold `H`, then half-life `τ`; cull at `α < 0.20`, so
// `life = H + 2.32·τ` ([`half_life_life_s`]). The table is keyed on
// (lane kind, class) and NOT on the population, because both sky populations
// share one column and all four transient populations share the other — which
// is exactly what makes "a star's class can be read off its life" true.

/// Sky m1 hold, ms (§5.6 — strike m1 `120/146/460`).
pub const HOLD_M1_SKY_MS: f32 = 120.0;
/// Sky m1 half-life, ms. `120 + 2.32·146 = 459` — the published 460 ms life.
pub const TAU_M1_SKY_MS: f32 = 146.0;
/// Sky m2 hold, ms (§5.6 — `80/112/340`).
pub const HOLD_M2_SKY_MS: f32 = 80.0;
/// Sky m2 half-life, ms.
pub const TAU_M2_SKY_MS: f32 = 112.0;
/// Sky m3 hold, ms (§5.6 — `40/80/225`). A grain's whole life is 225 ms, which
/// at the seeded 8-12 Hz arm clock is ≈ 2.3 open-and-close cycles: the number
/// that makes a grain read as a STAR instead of as one slow breath.
pub const HOLD_M3_SKY_MS: f32 = 40.0;
/// Sky m3 half-life, ms.
pub const TAU_M3_SKY_MS: f32 = 80.0;

/// Transient m1 hold, ms (§5.6 — the landing fan's hero, `40/118/315`).
pub const HOLD_M1_TRANSIENT_MS: f32 = 40.0;
/// Transient m1 half-life, ms.
pub const TAU_M1_TRANSIENT_MS: f32 = 118.0;
/// Transient m2 hold, ms (§5.6 — erase / fan / mini-fan m2, `30/93/245`).
pub const HOLD_M2_TRANSIENT_MS: f32 = 30.0;
/// Transient m2 half-life, ms.
pub const TAU_M2_TRANSIENT_MS: f32 = 93.0;
/// Transient m3 hold, ms (§5.6 — erase / shed / fan / mini-fan m3,
/// `20/67/175`).
pub const HOLD_M3_TRANSIENT_MS: f32 = 20.0;
/// Transient m3 half-life, ms.
pub const TAU_M3_TRANSIENT_MS: f32 = 67.0;

// ---------------------------------------------------------------------------
// 1c. MOTION — three laws, one per population family (§5.6)
// ---------------------------------------------------------------------------

/// The sky lift for an m1/m2 strike star, px per second (§5.6: "lift −3 px/s,
/// integer-stepped, 1-2 px over life"). Negative is UP.
///
/// Small on purpose: at 460 ms it is 1.4 px, which the integer step renders as
/// a single one-pixel rise somewhere in the star's life. A star that visibly
/// TRAVELS is a particle; a star that has shifted by the time you look back is
/// a sky. Stated at the 1× cell and spent through [`sky_lift_px_per_s`]
/// (−6 px/s on the retina cell: the same rise in the same time).
pub const SKY_LIFT_PX_PER_S: f32 = -3.0;

/// [`SKY_LIFT_PX_PER_S`] at cell height `ch` ([`px_scale`]).
#[inline]
#[must_use]
pub fn sky_lift_px_per_s(ch: f32) -> f32 {
    SKY_LIFT_PX_PER_S * px_scale(ch)
}

// ---------------------------------------------------------------------------
// 1d. THE SLIPSTREAM — the sky streams past a hand at speed
// ---------------------------------------------------------------------------

/// **THE SLIPSTREAM'S SHARE** — a sky star born under a hand at speed
/// inherits a BACKWARD drift (toward the pane's first column) of
/// `STREAM_SHARE_CW · cw / IOI` px/s, where `IOI` is the interval between
/// the last two Typed keys ([`Stardust::note_cadence`]): 0.35 of a cell per
/// key interval. 10 cps at `cw` 9 → 31 px/s; at the owner's `cw` 15 → 52;
/// 12 cps → 63; 8 cps → 42.
///
/// The owner's criterion, verbatim: *"effects that somehow make typing feel
/// faster or gain momentum are the best."* At speed the whole sky streams
/// past the hand; at a stroll it stands still ([`STREAM_IOI_STROLL_MS`]);
/// one star-life after the hand stops it is at rest ([`STREAM_END_MS`]).
/// T4 holds: the velocity is the OBSERVED cadence — the IOI of the typed
/// events, never the spine — and the spine only GATES the birth
/// ([`STREAM_DISP`]). The field stars take it too; the aurora's veils do
/// not (they belong to their cells).
pub const STREAM_SHARE_CW: f32 = 0.35;
/// The drift's decay, ms: `v(age) = v₀ · e^(−age/τ)`, so the sky slows
/// from the instant it is born and a star's travel is bounded whatever the
/// hand does next.
pub const STREAM_TAU_MS: f32 = 300.0;
/// Where the drift ENDS, ms after birth — the longest sky class life (the
/// strike m1's published 460, `H + 2.32·τ` = 458.7;
/// `the_sky_is_at_rest_one_star_life_after_the_last_key` pins the two
/// within 1.5 ms), so "the sky is at rest one star-life after the last
/// key" is true of EVERY star, the long-lived field star included.
/// The path is the exponential's first `STREAM_END_MS` ([`stream_unit`]),
/// then still; the whole travel is `v₀ · τ · STREAM_SPENT`.
pub const STREAM_END_MS: f32 = 460.0;
/// `1 − e^(−STREAM_END_MS / STREAM_TAU_MS)` — the share of the
/// exponential's asymptote spent by the end of the drift, as a literal so
/// the frame path never evaluates it
/// (`the_stream_spent_share_is_the_exponential_s`).
pub const STREAM_SPENT: f32 = 0.784_11;
/// The IOI clamp's fast end, ms — the fastest priced hand (33 cps: 175
/// px/s on the owner's cell, 2.6 cells of travel).
pub const STREAM_IOI_MIN_MS: f32 = 30.0;
/// The IOI clamp's slow end, ms — and the IOI a session's FIRST key is
/// priced at, having no interval to read (under the stroll gate: nothing).
pub const STREAM_IOI_MAX_MS: f32 = 600.0;
/// The stroll gate, OPEN: an IOI at or under this (≥ 6.7 cps) streams at
/// the whole priced velocity.
pub const STREAM_IOI_RUN_MS: f32 = 150.0;
/// The stroll gate, CLOSED: an IOI at or over this (≤ 3 cps) streams
/// nothing; between the two the velocity is smoothstepped in. Without it
/// a 2 cps peck would drift 3 px on the owner's cell: §23 measured a
/// sustained 2 cps hand at a `disp` of 0.97, so the birth gate alone
/// cannot make a stroll stand still — the cadence itself has to.
pub const STREAM_IOI_STROLL_MS: f32 = 333.0;
/// The spine gate: only a star born at `birth_disp ≥ 0.5` inherits the
/// drift — the second grain's own threshold.
pub const STREAM_DISP: f32 = 0.5;

/// **THE SLIPSTREAM'S VELOCITY**, px/s backward, for a cell width `cw` and
/// an observed key interval `ioi_ms`: `STREAM_SHARE_CW · cw / IOI` under
/// the stroll gate, the IOI clamped to
/// `[STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS]`. Zero for a stroll, a
/// non-finite input, or no cell.
#[must_use]
pub fn stream_px_per_s(cw: f32, ioi_ms: f32) -> f32 {
    stream_px_per_s_with_share(STREAM_SHARE_CW, cw, ioi_ms)
}

/// Scale the sky's velocity by its cell share, with the same stroll gate
/// and interval clamp used by the cadence measurement.
#[must_use]
fn stream_px_per_s_with_share(share: f32, cw: f32, ioi_ms: f32) -> f32 {
    if !(share.is_finite() && cw.is_finite() && ioi_ms.is_finite()) || cw <= 0.0 || share <= 0.0 {
        return 0.0;
    }
    let ioi = ioi_ms.clamp(STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS);
    let gate =
        smoothstep01((STREAM_IOI_STROLL_MS - ioi) / (STREAM_IOI_STROLL_MS - STREAM_IOI_RUN_MS));
    share * cw * 1000.0 / ioi * gate
}

/// The whole travel, px, of a star born at `px_per_s` — the exponential's
/// integral over the drift's life, `v₀ · τ · STREAM_SPENT`: 14.8 px at 12
/// cps on the owner's cell, 9.9 at 8.
#[must_use]
pub fn stream_travel_px(px_per_s: f32) -> f32 {
    px_per_s / 1000.0 * STREAM_TAU_MS * STREAM_SPENT
}

/// The drift's unit progress at `age_ms`, `0..=1`: the exponential's rise
/// normalised to reach exactly 1 at [`STREAM_END_MS`], and 1 thereafter.
/// The one curve the sky position and the cadence solver share.
#[inline]
#[must_use]
fn stream_unit(age_ms: f32) -> f32 {
    if age_ms >= STREAM_END_MS {
        return 1.0;
    }
    ((1.0 - (-age_ms.max(0.0) / STREAM_TAU_MS).exp()) / STREAM_SPENT).min(1.0)
}

// ---------------------------------------------------------------------------
// 1e. THE MEND'S PULL — the fix grabs the erased cell's light and pulls it
//     into the caret's cell
// ---------------------------------------------------------------------------

/// **THE MEND'S PULL WINDOW**, ms. When a typed key MENDS a typo run
/// ([`super::Mend`], §23's addendum "The mend") its strike star is an m2
/// born at the ERASED cell's sky — the very star the Backspace was retiring,
/// if one is live — and pulled ONE CELL into the caret's over this window on
/// the throw's own ease-out (`1 − spend(age/T)`, the curve every radial
/// throw in the theme draws), then PINNED there for the rest of its life:
/// the fix visibly pulls the light back to where it belongs. The landing
/// fan's own window ([`FAN_THROW_MS`]); on the owner's 15 px cell a
/// one-cell pull peaks at ~270 px/s and is over inside the m2's hold plus
/// thirty ms, so the star is at full the whole way.
///
/// A per-star travel like the slipstream's ([`Star::pull`]), NOT a lane
/// motion, for three reasons measured on this tree: the pull must END —
/// the shed's [`Motion::Drag`] at τ 90 stands at 70 % of the cell here and
/// pins only at death; it must ask for exactly one brisk window
/// ([`Star::brisk`]) where a dragging lane holds the frame cadence for the
/// star's whole life; and it must survive the next key's field deal, whose
/// adoption zeroes `v0` ([`Stardust::adopt_field`]) and would snap a
/// lane-moved star to its home mid-pull.
pub const MEND_PULL_MS: f32 = 110.0;

/// The drag time constant of a SHED fragment, ms (§5.6: "drag τ 90 ms").
/// Displacement is the closed-form integral `v₀·τ·(1 − e^(−age/τ))`, so a
/// fragment's whole path is a pure function of its birth velocity and its age
/// — no per-frame integration, no drift.
pub const DRAG_TAU_MS: f32 = 90.0;

// THE THROW WINDOW, per lane. A radial throw is `d = reach·(1 − (1 − u)²)`
// with `u = age / T` (§5.6, §6.5 layer 11); `1 − (1 − u)²` is `1 − spend(u)`,
// so the theme's own `spend` curve draws it and there is no eighth easing.
// `T` is the lane's: **this module owns the integrator**, and a caller hands
// a throw over as `v0 = reach / T` in px/ms ([`Star::v0`]) — or, for the
// meteor lanes, simply hands [`Stardust::sow_fan`] / [`Stardust::sow_mini_fan`]
// the `reach` in px and lets them do it. Read the window back through
// [`StarLane::throw_ms`]. Every window is SHORTER than its population's
// life: a thrown star reaches its reach, then sits there PINNED AND
// TWINKLING for the rest of its life (§5.6's erase row, and now every
// throw) — the throw is the star's entrance, not its whole story.

/// The erase throw's window, ms (§5.6: "radial throw 2-4 px over 80 ms
/// ease-out, up and away from the caret, then pinned + twinkling").
pub const ERASE_THROW_MS: f32 = 80.0;

/// The landing fan's window, ms.
///
/// **110, not 315 — measured on glass (offline judge, 2026-09-05, defect
/// 3).** The window used to be the hero's whole life (§6.2: "the fan's hero
/// winks at T + 315"), so the outermost star was at its full reach exactly
/// as the hero died. On glass that made the landing a NON-EVENT: on
/// `B/frame_0061+` (a 15×28 cell) the throw had covered 5 % of the reach at
/// 8 ms and 10 % at 16 ms, so for the first two frames every one of the
/// fan's stars — the hero and the four m2 included — sat INSIDE the caret
/// cell, under the host's opaque caret (§6.5 layer 12 is above layer 11),
/// and the class hold `H` that should have been their brightest 30-40 ms was
/// spent invisible; the first star emerged at T + 17-25 already off its
/// hold. On 110 the same throw has covered 47 % of the reach at the m2's
/// hold (30 ms) and 60 % at the hero's (40 ms): every one of the five is
/// clear of a 15×28 cell before its hold is spent, and the party is on glass
/// at full while it is still a party. The star's LIFE is unchanged
/// (`H + 2.32·τ`, 315 for the hero): it reaches its reach at 110 and
/// twinkles there, pinned, for the remaining 205 ms — §5.6's own erase
/// shape. The mini-fan follows ([`MINI_FAN_THROW_MS`]).
pub const FAN_THROW_MS: f32 = 110.0;

/// The mini-fan's window, ms — 100, on the same measurement as
/// [`FAN_THROW_MS`] (§6.12: "1-2 px throw … gone by 245 ms" — the 245 is the
/// m2's life, which is untouched; the hop's one or two pixels are covered
/// well inside the m2's hold instead of being finished as the star dies).
pub const MINI_FAN_THROW_MS: f32 = 100.0;

// The five law-fixed fan stars (D6) must clear the caret cell INSIDE their
// hold. At the transient m2's hold the throw has covered `1 − (1 − H/T)²` of
// the reach; the meteor's floor jump (8 cells, §6.1) reaches
// `(1.35 + 0.15·8) ch = 2.55 ch`, so at the jitter floor 0.55 an m2 is
// `0.47 · 0.55 · 2.55 ch = 0.66 ch` from the cell's centre at 30 ms — and on
// a cell with `cw ≤ 0.6 ch` (9×18, 15×28) that is outside the cell on EVERY
// ray: a ray that keeps `|dx| < 0.3 ch` has `|cos| < 0.45`, hence
// `|dy| > 0.59 ch > 0.5 ch`. A 40 % share is the floor that keeps that true;
// `a_fan_star_is_born_at_full_and_clears_its_cell_inside_its_hold` measures
// it on the real geometry. (The Enter landing's fixed `1.6 ch` reach is the
// one fan whose worst m2 — jitter 0.55 on a 60° ray — is still on its cell
// at 30 ms; it clears at ≈ 46 ms, where 315 left it there until ≈ 150.)
const _: () = assert!(
    {
        let u = HOLD_M2_TRANSIENT_MS / FAN_THROW_MS;
        1.0 - (1.0 - u) * (1.0 - u) >= 0.40
    },
    "the fan's four m2 must be clear of the caret cell before their hold is spent"
);

/// Lower end of the fan's per-star throw jitter, `reach·(0.55..1.05)` (§6.5
/// layer 11 — "every landing is its own party" is bought as count, phase,
/// size and throw).
pub const FAN_THROW_JITTER_MIN: f32 = 0.55;

/// Upper end of the fan's per-star throw jitter (§6.5 layer 11).
pub const FAN_THROW_JITTER_MAX: f32 = 1.05;

// THE VERTICAL REACH LAW (§6.5 layer 11, measured on glass 2026-09-05, defect
// 6a). §6.5 sizes the fan's reach by PATH LENGTH — `(1.35 + 0.15·cells)`, 5.2
// ch on a 44-cell jump — and a radial throw of that size put stars 60-75 px
// above the top text row, in the tab strip, on every long jump from a
// mid-screen row (`capture-after`, bbox ymin 9-20 px against a grid top of
// 84). The reach has no relation to the vertical room: a row is ONE ch tall
// whatever the path length was. So the throw is an ellipse, not a disc —
// squashed to its own aspect ([`FAN_SQUASH`], §6.5 layer 10) and
// never taller than [`FAN_RISE_MAX_CH`] above or below the landing row's
// centre, while the reach along the line keeps §6.5's formula: distance buys
// length ALONG THE LINE (§6.3's spirit), not height. A ray that would exceed
// the cap is flattened toward the line, keeping its length, never dropped —
// the census (1 gold m1 + 4 m2, D6) is untouched. On row 0 the `Geom.head`
// band is row 0's sky (§5.4) and the fan may rise into it, to the band's own
// top and not a pixel beyond; the same glass bound holds below.

/// The fan's vertical squash: distance buys the fan LENGTH along the line,
/// not height, and this is the aspect it buys it at.
///
/// §6.5 layer 10 wrote it as the landing ring's own, so that the ring and the
/// fan read as ONE squashed landing. The ring took 0.5 in the 2026-09-08 bold
/// round and was retired entirely on 2026-09-10 (the landing is a STARBURST,
/// whose core is ISOTROPIC and has no squash at all — see
/// `meteor::BURST_CORE_CH`), so this is now the fan's own number and nothing
/// else's. What still binds the two marks is the RISE CAP below, which they
/// share.
pub const FAN_SQUASH: f32 = 0.62;

/// The fan's rise ceiling, in `ch`, above and below the landing row's centre
/// — a row and a half beyond the landing row's own half, and exactly the
/// smallest fan's reach (§6.5 layer 11's floor, the Enter landing's D8 reach
/// sized by rows): no fan rises higher than the smallest fan is thrown.
pub const FAN_RISE_MAX_CH: f32 = 1.6;

/// Lower end of the per-star jitter on the rise cap, `cap·(0.80..1.00)`,
/// seeded: the re-aimed stars scatter UNDER the ceiling instead of lining up
/// on it ("every landing is its own party", §6.5 layer 11). Never above the
/// cap.
pub const FAN_RISE_JITTER_MIN: f32 = 0.80;

/// The focus-lost ember, ms (§8.2: "Focus lost | ember in 300 ms; stars die |
/// 300 | spend"). Every live star is put on a 300 ms `spend` finish beside the
/// ribbon's own ember — a fade beside a fade, never a pop beside a fade.
pub const FOCUS_EMBER_MS: f32 = 300.0;

/// A retracted cell's field star finishes over the cell's own retract: `240`
/// ms for a Backspace and `12·n + 240` for a kill of `n` cells (§8.2, §5.6:
/// "its field stars die with their cells"). These mirror the ribbon's retract
/// schedule; the ribbon owns the cell, stardust owns the star, and the two
/// meet on the event edge because the star cannot read the cell per frame.
pub const FIELD_RETRACT_BASE_MS: f32 = 240.0;

/// Per-cell term of a kill's retract span (§8.2: "12·n + 240").
pub const FIELD_RETRACT_PER_CELL_MS: f32 = 12.0;

/// Minimum radial reach of an erase throw, px at the 1× cell (§5.6; scaled
/// by [`px_scale`] where thrown, like every reach and sputter below).
pub const ERASE_REACH_MIN_PX: f32 = 2.0;
/// Maximum radial reach of an erase throw, px at the 1× cell (§5.6).
pub const ERASE_REACH_MAX_PX: f32 = 4.0;
/// Minimum radial reach of a mini-fan throw, px at the 1× cell (§5.6: "1-2
/// px throw").
pub const MINI_FAN_REACH_MIN_PX: f32 = 1.0;
/// Maximum radial reach of a mini-fan throw, px at the 1× cell (§5.6).
pub const MINI_FAN_REACH_MAX_PX: f32 = 2.0;

/// Share of the meteor head's velocity a shed fragment inherits ALONG the path
/// (§5.6: `v₀ = 0.12·v_head`).
pub const SHED_ALONG_SHARE: f32 = 0.12;
/// Minimum PERPENDICULAR sputter of a shed fragment, px/ms at the 1× cell
/// (§5.6). The ALONG share needs no scale: the head's velocity is already
/// in the cell's own pixels.
pub const SHED_PERP_MIN: f32 = 0.4;
/// Maximum perpendicular sputter of a shed fragment, px/ms at the 1× cell
/// (§5.6).
pub const SHED_PERP_MAX: f32 = 1.1;

/// Erase: how many m3 grains one Backspace throws (§5.6's "3 m3 + 1 m2").
pub const ERASE_M3_N: u16 = 3;
/// Erase: how many m2 stars one Backspace throws (§5.6).
pub const ERASE_M2_N: u16 = 1;
/// A kill mints one star per this many erased cells (§5.6, and [`Event::Kill`]'s
/// own restatement: "1 star per 3 cells, cap 8").
pub const KILL_CELLS_PER_STAR: u16 = 3;
/// …capped here, because a clause going is one gesture and not a shower (§5.6).
pub const KILL_STAR_MAX: u16 = 8;
/// A mini-fan is exactly this many m2 stars (§5.6, D17).
pub const MINI_FAN_M2_N: u16 = 1;
/// …and this many m3 grains (§5.6, D17).
pub const MINI_FAN_M3_N: u16 = 2;

// ---------------------------------------------------------------------------
// 1d. THE BUDGETS §18 holds this producer to
// ---------------------------------------------------------------------------

/// Quads stardust may spend on one frame (§18: "Stardust | 40 live | ≤ 284").
/// The worst case is the cap's own census — `4·23 + 12·10 + 24·3` — and the
/// budget is spent star by star, so an over-budget frame loses the LAST star's
/// faintest extremities rather than a whole magnitude.
pub const STARDUST_QUAD_BUDGET: usize = 284;

/// HALOED STARS stardust may draw on one frame (§18: "≤ 16" — the cap's own
/// 4 m1 + 12 m2). Counted per STAR, not per `RainHalo`: one halo splits into
/// one quad per cell row it crosses, and a budget that counted the splits
/// would give the ninth m2 no atmosphere. §18 also says `halos.truncate`
/// sheds the LAST emitter, which is this one: the sky is precisely the layer
/// that may be thinned, and the meteor and ribbon are not.
pub const STARDUST_HALO_BUDGET: usize = 16;

/// Source-over `RainHalo`s stardust may spend on one LIGHT frame. §3.3 draws
/// every light star as ink lays (the `push_twinkle_over` silhouette spends up
/// to five per star, each split per crossed row) and §18 holds a light frame
/// to ≤ 400 of `MAX_HALOS 512` with the meteor's train chain at 96 ahead of
/// the sky, which is the LAST emitter and the one `halos.truncate` sheds.
///
/// Until 2026-09-10 the meteor's light RING took another 36 of it
/// (`RING_LIGHT_DOTS`) and this budget was exactly `400 − 96 − 36`. The
/// STARBURST that replaced the ring has a light arm made of source-over ink
/// rects in `out`, not halos — the silhouette is the law and the operator is
/// not — so it spends NONE, and those 36 are now slack. Left as slack rather
/// than handed to the sky: retiring a mark is not a retune of the sky's
/// density.
pub const STARDUST_LIGHT_HALO_BUDGET: usize = 268;

const _: () = assert!(
    STARDUST_LIGHT_HALO_BUDGET + 96 <= 400,
    "§18: the light frame's 400-halo law — train chain + sky"
);

/// The source-over ceiling of a light-theme star mark (§3.3: `light_ink_bold`
/// is `InkRole::Leading`, cap 236). Every light alpha this module emits is at
/// or under it (§20.1's light-theme law); each rect is capped at ITS ROLE's
/// own ceiling first (`InkRole::alpha_cap`: the needles are over-text ink at
/// 190), and this is the larger of the two.
pub const LIGHT_ALPHA_CAP: f32 = 236.0;

// ===========================================================================
// 2. Class, lane, and the star itself
// ===========================================================================

/// A star's MAGNITUDE — the distribution that makes a field a field (§5.1
/// no. 2, §5.2). Together with [`StarLane`] this replaces v1's `cov_scale`
/// sentinel encoding (0.5/0.7/0.8/0.95/1.0/1.6 meaning shape *and* brightness),
/// which §19 deletes as fragile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StarClass {
    /// The hero: white body + tapers, four tinted stubs, a tinted halo, and —
    /// gold only, at twinkle peak only — four diagonal glints. Composited
    /// centre 143 (sky) / 118 (transient).
    M1,
    /// The medium star: white body + tapers, a 3×3 disc (sky lane only), four
    /// tinted stubs, a small halo. Composited centre 130 (sky) / 80
    /// (transient).
    M2,
    /// The grain: a white 1-px core with four tinted arms at
    /// [`M3_CROSS_ADD`]·c outboard of it — the cross *is* its Airy disc, and
    /// it carries the tint. Composited centre 98 (sky) / 61 (transient). This is
    /// also "the round mote at arm 0" of §2.4: v1's `push_dust_mote` d×d
    /// square is not carried.
    M3,
}

impl StarClass {
    /// This class's coverage request `c` in `lane` (§5.2's two tables).
    #[must_use]
    pub fn cov(self, lane: StarLane) -> f32 {
        match (self, lane.is_sky()) {
            (Self::M1, true) => M1_COV_SKY,
            (Self::M1, false) => M1_COV_TRANSIENT,
            (Self::M2, true) => M2_COV_SKY,
            (Self::M2, false) => M2_COV_TRANSIENT,
            (Self::M3, true) => M3_COV_SKY,
            (Self::M3, false) => M3_COV_TRANSIENT,
        }
    }

    /// The composited centre this class reaches at full envelope and full
    /// twinkle (§5.2, §3.2 rank 4). The tests measure against §5.2's OWN
    /// literals, not against this — this is what the code claims, and a claim
    /// cannot be its own oracle.
    #[must_use]
    pub fn centre(self, lane: StarLane) -> f32 {
        match (self, lane.is_sky()) {
            (Self::M1, true) => CENTRE_M1_SKY,
            (Self::M1, false) => CENTRE_M1_TRANSIENT,
            (Self::M2, true) => CENTRE_M2_SKY,
            (Self::M2, false) => CENTRE_M2_TRANSIENT,
            (Self::M3, true) => CENTRE_M3_SKY,
            (Self::M3, false) => CENTRE_M3_TRANSIENT,
        }
    }

    /// This class's rung on the family's ONE arm ladder
    /// (`effect_util::STAR_ARM_*`), so a star's size can only ever be one of
    /// the named sizes (`star_arm`'s own law: "the only way an emitter is
    /// allowed to pick a star's size").
    ///
    /// The grain takes [`STAR_ARM_FINE`] and not "arm 0": at `ch` 18 the fine
    /// rung is 1.89 px, which the arm clock steps between 1 and 2 — exactly
    /// §5.5's "stubs 1 ↔ 2 px", and the reason a grain visibly OPENS.
    #[must_use]
    pub fn arm_ratio(self) -> f32 {
        match self {
            Self::M1 => STAR_ARM_HERO,
            Self::M2 => STAR_ARM_STD,
            Self::M3 => STAR_ARM_FINE,
        }
    }

    /// The arm clock's FLOOR, as a share of the nominal arm (§5.5's table):
    /// m2/m3 swing at full depth, m1 only ±12 %. Small stars twinkle most —
    /// that is the physical reading (a smaller apparent disc is scintillated
    /// harder by the same atmosphere) and it is also what keeps a hero from
    /// looking like it is being resized.
    #[must_use]
    pub fn arm_floor(self) -> f32 {
        match self {
            Self::M1 => 0.88,
            Self::M2 | Self::M3 => 0.55,
        }
    }

    /// The smallest INTEGER arm this class is ever drawn at, at cell height
    /// `ch` (§5.5's table at `ch` 18: m1 3 ↔ 4, m2 2 ↔ 3, m3 1 ↔ 2 px). See
    /// [`M2_ARM_MIN_PX`] for why only the m2 needs stating — and why it
    /// scales with the cell ([`px_at`]).
    #[must_use]
    pub fn arm_min_px(self, ch: f32) -> i32 {
        match self {
            Self::M2 => px_at(ch, M2_ARM_MIN_PX),
            Self::M1 | Self::M3 => 1,
        }
    }

    /// The core-brightness floor on the LUMINANCE clock (§5.5's table): m1
    /// `0.85 + 0.15·e`, m2 `0.70 + 0.30·e`, m3 `env` at full depth.
    #[must_use]
    pub fn core_floor(self) -> f32 {
        match self {
            Self::M1 => 0.85,
            Self::M2 => 0.70,
            Self::M3 => 0.0,
        }
    }

    /// This class's `(hold, τ)` in seconds for `lane` (§5.6's envelope table).
    /// The FIELD lane's ALPHA does not read it — a field star rides its
    /// cell's own shape over [`FIELD_LIFE_S`] and the field fade from its
    /// row's last key ([`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`],
    /// [`Star::envelope`]) — but its hold `H` is still the star's
    /// ([`Star::in_hold`]): every class is born at full and scintillates
    /// once its hold is spent.
    #[must_use]
    pub fn envelope_s(self, lane: StarLane) -> (f32, f32) {
        let (h, t) = match (self, lane.is_sky()) {
            (Self::M1, true) => (HOLD_M1_SKY_MS, TAU_M1_SKY_MS),
            (Self::M1, false) => (HOLD_M1_TRANSIENT_MS, TAU_M1_TRANSIENT_MS),
            (Self::M2, true) => (HOLD_M2_SKY_MS, TAU_M2_SKY_MS),
            (Self::M2, false) => (HOLD_M2_TRANSIENT_MS, TAU_M2_TRANSIENT_MS),
            (Self::M3, true) => (HOLD_M3_SKY_MS, TAU_M3_SKY_MS),
            (Self::M3, false) => (HOLD_M3_TRANSIENT_MS, TAU_M3_TRANSIENT_MS),
        };
        (h / 1000.0, t / 1000.0)
    }

    /// This class's whole life in seconds for `lane` — `H + 2.32·τ`, the one
    /// place the published 225 / 340 / 460 ms are derived rather than typed;
    /// a FIELD star's is its cell's swoosh-bounded [`FIELD_LIFE_S`] whatever
    /// its class (§5.6: it rides the cell, not the class table).
    #[must_use]
    pub fn life_s(self, lane: StarLane) -> f32 {
        if lane == StarLane::Field {
            return FIELD_LIFE_S;
        }
        let (h, t) = self.envelope_s(lane);
        half_life_life_s(h, t)
    }
}

/// Which POPULATION minted a star (§5.6) — and therefore how it is priced,
/// how long it lives, how it moves, and whether it may touch the glint
/// bucket.
///
/// One enum rather than a bare sky/transient bit, because §5.6's envelope
/// table is keyed on the population and not only on the lane: a fan m1 lives
/// 315 ms where a strike m1 lives 460, and both are "m1 in the transient/sky
/// sense" alone would lose that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StarLane {
    /// Born on every typed glyph, in the sky band of the new cell. Sky
    /// pricing. Spends the bucket (m1 only).
    Strike,
    /// Born once per laid ribbon cell, hash-dealt and stable per cell. Sky
    /// pricing. PINNED; rides the cell's own shape over [`FIELD_LIFE_S`]
    /// (`edge-in` × expiry melt), and finishes with the cell's retract
    /// ([`Stardust::on_event`]'s erase arms).
    Field,
    /// Born on Backspace (and per 3 cells of a kill, cap 8) around the erased
    /// cells. Transient pricing. **Never chimes** — Backspace is unpitched,
    /// ruled twice (§13, §19.2).
    Erase,
    /// The meteor's shed fragments (§6.5 layer 5). Transient pricing,
    /// meteor-lane, **silent**.
    Shed,
    /// The landing fan (§6.5 layer 11). Transient pricing, meteor-lane; its
    /// 1 gold m1 + 4 m2 pair 1:1 with the five rain glints (D6).
    Fan,
    /// A sub-floor nav hop's mini-fan (§6.12, D17). Transient pricing, 1 m2 +
    /// 2 m3, thrown 1-2 px, gone by 245 ms.
    MiniFan,
}

impl StarLane {
    /// True for the SKY lanes — priced by the sparkle law, composited centre
    /// ≤ [`super::timing::FIELD_STAR_COV_CEIL`] (§5.2 ceiling 1).
    #[must_use]
    pub fn is_sky(self) -> bool {
        matches!(self, Self::Strike | Self::Field)
    }

    /// True for the TRANSIENT lanes — they fly over ink, so they are priced by
    /// [`super::timing::TRANSIENT_STAR_COV_CEIL`] (§5.2 ceiling 2).
    #[must_use]
    pub fn is_transient(self) -> bool {
        !self.is_sky()
    }

    /// True for the METEOR lanes, which are **exempt from the typing glint
    /// bucket entirely** (§8.1 no. 2, §13).
    #[must_use]
    pub fn is_meteor(self) -> bool {
        matches!(self, Self::Shed | Self::Fan)
    }

    /// True where a star may cue a glint at all (§13): erase-born stardust
    /// never chimes, and meteor-lane stars ride the rain instead.
    #[must_use]
    pub fn may_chime(self) -> bool {
        matches!(self, Self::Strike | Self::Field)
    }

    /// **THE THROW WINDOW** `T` of a radial-throw lane, ms — `None` for the
    /// lanes that lift or drag. A thrown star's [`Star::v0`] is `reach / T`
    /// in px/ms and it sits at exactly `reach` from its origin once
    /// `age ≥ T`, on `reach·(1 − spend(age/T))`, pinned and twinkling there
    /// for the rest of its life; `T` is the lane's own window
    /// ([`ERASE_THROW_MS`], [`FAN_THROW_MS`], [`MINI_FAN_THROW_MS`]), shorter
    /// than any class life on the lane. The meteor hands over a `reach` in
    /// px and never needs this number, but it is published so a caller that
    /// must build a `v0` itself builds it in this module's units.
    #[must_use]
    pub fn throw_ms(self) -> Option<f32> {
        match self {
            Self::Erase => Some(ERASE_THROW_MS),
            Self::Fan => Some(FAN_THROW_MS),
            Self::MiniFan => Some(MINI_FAN_THROW_MS),
            Self::Strike | Self::Field | Self::Shed => None,
        }
    }

    /// WHICH MOTION LAW this population takes (§5.6's Motion column). Three
    /// laws, each named by the population that needs it — and no fourth.
    #[must_use]
    fn motion(self) -> Motion {
        match self {
            // A sky m1/m2 lifts; a sky m3 and every field star are pinned,
            // which the zero `v0` expresses without a fourth law.
            Self::Strike | Self::Field => Motion::Lift,
            // §5.6: the shed inherits the head's velocity under drag τ 90.
            Self::Shed => Motion::Drag,
            // §5.6: "radial throw … over 80 ms ease-out … then pinned" — the
            // erase is a THROW on the erase window, not a drag; the fan and
            // the mini-fan throw on their own windows.
            Self::Erase => Motion::Throw(ERASE_THROW_MS),
            Self::Fan => Motion::Throw(FAN_THROW_MS),
            Self::MiniFan => Motion::Throw(MINI_FAN_THROW_MS),
        }
    }
}

/// The three motion laws of §5.6, resolved from the population.
#[derive(Clone, Copy, Debug)]
enum Motion {
    /// `d = v₀ · age` — the sky's −3 px/s rise, integer-stepped.
    Lift,
    /// `d = v₀ · τ · (1 − e^(−age/τ))` — exponential drag, the closed-form
    /// integral so a fragment's path never depends on the frame rate.
    Drag,
    /// `d = v₀ · T · (1 − spend(age/T))` — the radial ease-out, i.e. §5.6's
    /// `reach·(1 − (1 − u)²)` written on the theme's own curve, over the
    /// lane's own window `T` in ms.
    Throw(f32),
}

impl Motion {
    /// The displacement factor at `age_ms`: `pos = origin + round(v₀ · k)`.
    /// ONE function for the whole path, so the birth clearance and every
    /// later frame read the same curve ([`Star::pos`], [`Star::rest`]).
    #[inline]
    fn k_at(self, age_ms: f32) -> f32 {
        match self {
            Self::Lift => age_ms,
            Self::Drag => DRAG_TAU_MS * (1.0 - (-age_ms / DRAG_TAU_MS).exp()),
            Self::Throw(window_ms) => window_ms * (1.0 - spend(age_ms / window_ms)),
        }
    }

    /// The age at which this motion ENDS, given the star's own life in ms:
    /// a throw at its window (the reach — a class that dies sooner stops
    /// short of it on the same ray), a drift or a lift at the star's death.
    /// [`Star::rest`] is the position here.
    #[inline]
    fn end_ms(self, life_ms: f32) -> f32 {
        match self {
            Self::Throw(window_ms) => window_ms,
            Self::Lift | Self::Drag => life_ms,
        }
    }
}

/// A STAR'S FINISH — the one way a star leaves before its own life is up
/// (§5.6's cap, §8.2's focus ember, §5.6's "its field stars die with their
/// cells"). Its envelope is multiplied by `spend((now − at) / span)`, so it
/// leaves on the theme's own retract curve, compressed — never a pop (T5),
/// and never a fade-in-place of a mark that was moving.
///
/// A star on a finish is culled the moment the finish alone would take it
/// under [`STAR_CULL_ALPHA`] (`u > 1 − √0.20 ≈ 0.55`), so "finishes within
/// `span`" is literally true and the pool never carries a finishing ghost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finish {
    /// The edge the finish started on.
    pub at: Instant,
    /// The finish's span in seconds — 0.040 for an eviction, 0.300 for the
    /// focus ember, the cell's own retract for a field star.
    pub span_s: f32,
}

impl Finish {
    /// The finish's unit progress at `now`, `0..=1`.
    #[must_use]
    pub fn u(&self, now: Instant) -> f32 {
        clamp01(
            now.saturating_duration_since(self.at).as_secs_f32() / self.span_s.max(f32::EPSILON),
        )
    }

    /// The envelope multiplier at `now` — `spend(u)`.
    #[must_use]
    pub fn gain(&self, now: Instant) -> f32 {
        spend(self.u(now))
    }

    /// The instant this finish ends.
    #[must_use]
    fn ends(&self) -> Instant {
        self.at + std::time::Duration::from_secs_f32(self.span_s.max(0.0))
    }

    /// The instant this finish alone takes the star under
    /// [`STAR_CULL_ALPHA`] — where [`Star::dead`] drops it (D3): `spend(u) <
    /// 0.20` at `u = 1 − √0.20`, well before the finish ends.
    #[must_use]
    fn cull_at(&self) -> Instant {
        self.at
            + std::time::Duration::from_secs_f32(
                self.span_s.max(0.0) * (1.0 - STAR_CULL_ALPHA.sqrt()),
            )
    }
}

/// ONE STAR (§17.1's `Star{class, lane, x, y, born, seed, tint, v0, f}`, plus
/// the dealt gold flag and the optional finish).
///
/// `Copy` and flat: the pool is a fixed [`STAR_POOL_MAX`]-slot resident `Vec`
/// (§18: no per-star allocation), and everything else about a star —
/// envelope, arm, coverage, phase, halo radius — is DERIVED from these by
/// pure functions of `now`. That is what "no per-star state, no per-frame RNG"
/// means in practice (§5.5).
#[derive(Clone, Copy, Debug)]
pub struct Star {
    /// Magnitude (§5.2).
    pub class: StarClass,
    /// Population (§5.6) — pricing, envelope, motion, budget behaviour.
    pub lane: StarLane,
    /// Window-absolute X of the centre. **Pinned to a device pixel at birth**
    /// (§5.4): sub-pixel drift is expressed as INTEGER steps of the centre,
    /// never as half-lit pixels.
    pub x: f32,
    /// Window-absolute Y of the centre, pinned the same way.
    pub y: f32,
    /// The glyph row that earned a sky star. Its sky can occupy the preceding
    /// pixel row, so regional scrolls follow this owner rather than `y`.
    /// Free transients have no glyph owner and follow their pixel position.
    pub home_row: Option<u16>,
    /// The birth edge. Every envelope and both scintillation clocks read
    /// `now − born`.
    pub born: Instant,
    /// The last key edge that LIT this star — the FIELD lane's fade clock
    /// ([`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`] run from here, not from
    /// `born`). Equal to `born` at birth; a typed key on the star's row
    /// re-stamps it ([`Stardust::relight_field`]), which is the one brighten
    /// besides the `edge-in` — and it has a keystroke behind it. Every other
    /// lane leaves it at `born` and never reads it.
    pub lit: Instant,
    /// Deterministic seed, hashed from `(row, col, mint)` at the spawn edge
    /// (§18). Deals the tint (§5.3), the birth jitter (§5.4), the
    /// scintillation phase `φ` and rate [`Star::f`], the throw angle, and the
    /// shed station.
    pub seed: u32,
    /// The star's CHROMA, constant for its whole life — one of the seven
    /// snapped stops, [`TINT_WHITE_RGB`] or [`TINT_GOLD_RGB`] (§5.3, C1).
    /// Only ALPHA moves (C3).
    pub tint: u32,
    /// TRUE for a GOLD star — §5.3's 15 % deal, or the fan's own hero (D6):
    /// **the only stars that carry diagonal glints** (§5.2). Dealt, never
    /// inferred from [`Star::tint`]: the arc's yellow stop is the same RGB as
    /// gold, and a field star wearing the arc's yellow is not a gold one.
    pub gold: bool,
    /// Birth velocity in px/ms, `(vx, vy)` — the throw for erase / fan /
    /// mini-fan, the inherited `0.12·v_head` for a shed fragment, and the
    /// −3 px/s lift for a sky m1/m2. Zero for a pinned grain and for every
    /// field star. T4: this is the OBSERVED frame delta where one exists,
    /// never the smoothed spine.
    pub v0: (f32, f32),
    /// The seeded ARM-SIZE scintillation rate, Hz, in
    /// `[SCINT_F_MIN, SCINT_F_MAX]` (§5.5, D12). The audio glint that rides
    /// this star twinkles at **this same `f`** (§13) — one number, both halves.
    pub f: f32,
    /// **THE SLIPSTREAM'S TRAVEL**, px ≥ 0 — how far this star drifts
    /// BACKWARD (toward the pane's first column) over [`STREAM_END_MS`],
    /// priced ONCE at birth from the hand's observed key interval
    /// ([`stream_travel_px`] of [`stream_px_per_s`], T4) and clamped to the
    /// blank frontier of its sky row ([`Stardust::stream_reach`], L4).
    /// Integer-stepped like the lift: `x − round(stream · stream_unit(age))`
    /// ([`Star::pos`]). Zero — the common case, answered before any `exp` —
    /// for a star born under a stroll or a cold hand, and for every
    /// transient. An adopted field star keeps the drift it was born with.
    pub stream: f32,
    /// **THE MEND'S PULL**, px ≥ 0 — how far LEFT of its home this star
    /// was born, and is drawn in from over [`MEND_PULL_MS`] on the throw's
    /// ease-out: `x − round(pull · spend(age / MEND_PULL_MS))`
    /// ([`Star::pos`]). `x`, `y` are the HOME — the caret cell's own dealt
    /// sky pixel, where the star is pinned from the window's end and what
    /// every cell law reads ([`star_over_cell`]), so one-star-per-cell sees
    /// the star where it lands. Zero for every star but a mend's
    /// ([`Stardust::deal_star`] with a [`Pull`]); an adopted field star keeps
    /// it, as it keeps its stream.
    pub pull: f32,
    /// The finish this star is on, if any ([`Finish`]).
    pub finish: Option<Finish>,
}

// A floored birth is never under the cull: the echo frame's field star is
// drawn, not dropped by `Paint::of`'s `alpha < STAR_CULL_ALPHA` on the frame
// that acknowledges its key (T2).
const _: () = assert!(
    BIRTH_EDGE_FLOOR > STAR_CULL_ALPHA,
    "the field star's birth floor sits under the star cull"
);

impl Star {
    /// Seconds since birth, saturating at zero (a star handed a `now` before
    /// its own edge is a newborn, never a negative age).
    #[must_use]
    pub fn age_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }

    /// Seconds since the last key that lit this star ([`Star::lit`]),
    /// saturating at zero — the field fade's clock.
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.lit).as_secs_f32()
    }

    /// This star's whole life in seconds (§5.6).
    #[must_use]
    pub fn life_s(&self) -> f32 {
        self.class.life_s(self.lane)
    }

    /// **BORN AT FULL — TRUE inside the class hold `H`** (§5.6's `α = 1 for
    /// a hold H`, read as a hold on EVERYTHING): for its first `H` ms a star
    /// is drawn at its nominal arm with its core at full (`e = 1`, `env = 1`),
    /// and both scintillation clocks (§5.5) take over at `age ≥ H`. The
    /// clocks themselves are not shifted — they still read `now − born`, so a
    /// star's whole trace stays a pure function of `(birth, seed, now)` and
    /// the warm/cool antiphase law is untouched; the hold only overrides
    /// what they SAY while it lasts.
    ///
    /// **Why — measured on glass (offline judge, 2026-09-05, defect 3).** A
    /// landing fan's m2 born at a luminance trough drew its bars at
    /// `c·(0.70 + 0.30·e) = 24` levels over a (17, 19, 24) ground, at or
    /// under the read threshold, and at a 1-px arm (`round(2.52·0.55)`,
    /// floored to 2) — "fan stars 1-2 px at peak 61-122": the party's
    /// brightest 30-40 ms were dealt to the clock's whim. A star's hold is
    /// the one span it is promised at full; now it is full in size and light
    /// too, and the eye is handed a 7×7 m2 / 9×9 hero on the frame it is
    /// born, whatever the phase. The field lane reads the sky hold of its
    /// class.
    #[must_use]
    pub fn in_hold(&self, now: Instant) -> bool {
        self.age_s(now) < self.class.envelope_s(self.lane).0
    }

    /// True once the star must leave the pool: past its life, past the
    /// point where its [`Finish`] alone takes it under [`STAR_CULL_ALPHA`],
    /// or — a FIELD star — past the field fade from its row's last key
    /// ([`field_fade_life_s`], where that fade alone reaches the cull). The
    /// envelope reaches the cull exactly here (D3): a star is dropped while
    /// it is still a visible point, never left as a grey speck.
    #[must_use]
    pub fn dead(&self, now: Instant) -> bool {
        self.age_s(now) >= self.life_s()
            || (self.lane == StarLane::Field && self.idle_s(now) >= field_fade_life_s())
            || self.finish.is_some_and(|f| f.gain(now) < STAR_CULL_ALPHA)
    }

    /// THE POPULATION ENVELOPE at `now` — `1` for the hold, then `half-life`
    /// τ (§5.6), times the [`Finish`] if the star is on one. This is the ONLY
    /// thing that fades: chroma is fixed at birth (C3), so "dying" is a
    /// coverage curve and nothing else.
    ///
    /// A FIELD star rides its CELL instead of the class table (§5.6's Field
    /// row: "rides the cell's own edge envelope × retract — alpha only"),
    /// as the product of three clocks, every one of them alpha:
    ///
    /// * **the birth** — it is dealt on the echo edge that laid the cell, so
    ///   its age IS the cell's, and the 18 ms `edge-in` the ribbon head cell
    ///   takes is the one it takes — **from the cell's own
    ///   [`BIRTH_EDGE_FLOOR`], not from zero**: the host can present a key's
    ///   echo 0.2-0.4 ms after the key, where a bare `edge_in` is 0.0008 and
    ///   the star is under the cull on the very frame that echoes its key,
    ///   to appear one panel period later (measured 2026-09-05, the ribbon's
    ///   defect exactly; §2.1 T2: "the frame that acknowledges the key
    ///   already shows the colour"). The floor rides the BIRTH only — no
    ///   hold, no finish and no focus path reads it;
    /// * **the horizon** — the ribbon's own `expiry_melt` over
    ///   [`FIELD_LIFE_S`], the swoosh span at which the cell is guaranteed
    ///   gone, which is what bounds the sky under a hand that keeps typing;
    /// * **the hand** — the field fade, `half-life` on [`FIELD_HOLD_MS`] /
    ///   [`FIELD_TAU_MS`] from the LAST KEY ON THE STAR'S ROW
    ///   ([`Star::lit`], re-stamped by [`Stardust::relight_field`]): full
    ///   while the hand is on the row, under half 550 ms after it leaves,
    ///   culled at 880 ms — before the ribbon's retract moves. Without it a
    ///   field star held near-full for ~1.2 s after the last key over a
    ///   ribbon already being drawn back into the caret (the live capture's
    ///   defect 4).
    ///
    /// The retract half of §5.6's row is the [`Finish`] the producer starts
    /// on the erase edge, because the star cannot read its cell per frame.
    ///
    /// Under reduced motion the theme's ONE linear fade replaces the
    /// half-life ([`REDUCED_MOTION_FADE_MS`], §2.5/§6.11): full through the
    /// life, then straight to zero over 120 ms at the end of it — and, for a
    /// field star, the same 120 ms fade at the end of the field fade's span
    /// ([`field_fade_life_s`]). A finish still applies — a static mark is
    /// still retired, not popped.
    #[must_use]
    pub fn envelope(&self, now: Instant, reduced_motion: bool) -> f32 {
        let age = self.age_s(now);
        let fin = self.finish.map_or(1.0, |f| f.gain(now));
        if self.lane == StarLane::Field {
            // The field lane's one envelope — shared with the aurora's veil,
            // which rides the same cell the same way ([`Veil::envelope`]).
            return field_lane_envelope(age, self.idle_s(now), fin, reduced_motion);
        }
        if reduced_motion {
            let life = self.life_s();
            let fade = REDUCED_MOTION_FADE_MS / 1000.0;
            return clamp01((life - age) / fade.max(f32::EPSILON)) * fin;
        }
        let (hold, tau) = self.class.envelope_s(self.lane);
        half_life(age, hold, tau) * fin
    }

    /// Put this star on a finish ending at `at + span_s` — unless it is
    /// already on one that ends SOONER, which stands: a star being evicted
    /// while it embers is not given more time by the eviction.
    pub fn finish_by(&mut self, at: Instant, span_s: f32) {
        let next = Finish { at, span_s };
        if self.finish.is_none_or(|f| next.ends() < f.ends()) {
            self.finish = Some(next);
        }
    }

    /// **THIS STAR'S GRID COLUMN** — its HOME pixel read back as a column
    /// (§5.4: `x` is pinned at birth and every drift is an integer step off
    /// it, so this is stable for the star's whole life). The reach law of the
    /// pet's catch (`companion::CATCH_REACH_CELLS`) is measured against it.
    #[must_use]
    pub fn grid_col(&self, geom: Geom) -> i32 {
        px_col(self.x, geom)
    }

    /// **IS THIS STAR IN THE SKY OF `row`?** — the band a star born over that
    /// row's cells occupies, which is "row or row − 1" whichever ribbon
    /// spelling the host runs ([`in_sky_of_row`]). The vertical half of the
    /// catch's reach: a paw reaches along its own line and the sky directly
    /// over it, never three lines up.
    #[must_use]
    pub fn in_sky_of(&self, row: u16, geom: Geom) -> bool {
        in_sky_of_row(self.y, row, geom)
    }

    /// TRUE for a WARM tint — R, O, Y of the seven stops, plus gold and plus
    /// the achromatic A-type white (§5.5).
    ///
    /// White belongs to neither half of the spectrum, and the φ + π flip
    /// exists to keep a warm and a COOL NEIGHBOUR from blinking together; an
    /// achromatic star has no neighbour it could beat against, so it takes the
    /// base phase rather than earning a fourth rule.
    #[must_use]
    pub fn is_warm(&self) -> bool {
        if self.tint == TINT_WHITE_RGB || self.tint == TINT_GOLD_RGB {
            return true;
        }
        // Asked of the canonical ROYGBIV anchors by INDEX, not guessed from
        // channels: R, O, Y are stops 0-2 and G, B, I, V are 3-6, and a tint
        // is one of those seven constants by construction (C1).
        (0..SPECTRUM_STOPS)
            .position(|i| spectrum_stop(i) == self.tint)
            .is_none_or(|i| i < WARM_STOPS)
    }

    /// **THE LUMINANCE PHASE**, radians (§5.5): `φ = hash(row, col, seed)·2π`,
    /// and **warm tints take `φ`, cool tints take `φ + π`** — so a warm and a
    /// cool neighbour never blink together. Pinned by
    /// `warm_and_cool_neighbours_never_blink_together`.
    #[must_use]
    pub fn lum_phase(&self) -> f32 {
        let base = hash01(mix32(self.seed ^ SALT_PHASE)) * std::f32::consts::TAU;
        if self.is_warm() {
            base
        } else {
            base + std::f32::consts::PI
        }
    }

    /// The certified 2.86 Hz luminance envelope at `now` (§5.5;
    /// `effect_util::twinkle_env`, `TWINKLE_OMEGA 9` rad/s, under the WCAG
    /// 2.3.1 general-flash bound). **Every mark, at every size, rides this
    /// clock** — the 8-12 Hz exemption is for SIZE only.
    #[must_use]
    pub fn twinkle(&self, now: Instant) -> f32 {
        twinkle_env(self.age_s(now), self.lum_phase())
    }

    /// `e = (env − 0.55)/0.45 ∈ [0, 1]` — the twinkle expressed as a unit
    /// depth, which is what §5.5's per-class core-brightness rules take.
    #[must_use]
    pub fn twinkle_unit(&self, now: Instant) -> f32 {
        clamp01((self.twinkle(now) - 0.55) / 0.45)
    }

    /// True where this star's body and nucleus may carry `push_twinkle_star`'s
    /// diagonal glints — **gold only, at twinkle peak only** (§5.2, §5.7).
    #[must_use]
    pub fn glinting(&self, now: Instant) -> bool {
        self.gold && twinkle_peak(self.twinkle(now))
    }

    // -- the cadence law (§18) ---------------------------------------------

    /// Whether this star MOVES or RAMPS on every frame: a throw still inside
    /// its window (§5.6's 80 / 110 / 100 ms), a shed fragment under drag
    /// (its displacement keeps growing measurably for its whole 175-245 ms
    /// life), or a field star inside its cell's 18 ms `edge-in`. A sky star
    /// lifts one pixel in its whole life — a step, not motion — and a star
    /// in its hold is drawn at full, static.
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        let age_s = self.age_s(now);
        if self.lane == StarLane::Field && age_s < EDGE_IN_S {
            return true;
        }
        // The mend's pull moves every frame of its window, like a throw (1e).
        if self.pull > 0.0 && age_s * 1000.0 < MEND_PULL_MS {
            return true;
        }
        if self.v0.0 == 0.0 && self.v0.1 == 0.0 {
            return false;
        }
        match self.lane.motion() {
            Motion::Throw(window_ms) => age_s * 1000.0 < window_ms,
            Motion::Drag => true,
            Motion::Lift => false,
        }
    }

    /// Seconds until this star's INTEGER ARM next steps on §5.5's size
    /// clock, `arm = round(A·(floor + (1 − floor)·sin²(2π·f·age + φ)))`
    /// at cell height `ch` — the twinkle's one visible step, solved in
    /// closed form: the rounded arm toggles between `k` and `k + 1` where
    /// `sin²` crosses `((k + 0.5)/A − floor)/(1 − floor)`, twice per
    /// `1/(2f)` period, so the answer is never more than 62.5 ms away
    /// (`f ≥ 8`). `None` when the swing rounds to one size and the arm
    /// never steps at all.
    #[must_use]
    pub fn next_arm_step_s(&self, ch: f32, now: Instant) -> Option<f32> {
        let nominal = star_arm(ch, self.class.arm_ratio());
        let floor = self.class.arm_floor();
        let depth = 1.0 - floor;
        if !(nominal.is_finite() && depth.is_finite() && self.f.is_finite())
            || nominal <= 0.0
            || depth <= 0.0
            || self.f <= 0.0
        {
            return None;
        }
        let min = self.class.arm_min_px(ch) as f32;
        let omega = std::f32::consts::TAU * self.f;
        let theta = omega * self.age_s(now) + self.lum_phase();
        let k0 = (nominal * floor - 0.5).ceil().max(min) as i32;
        let k1 = (nominal - 0.5).floor() as i32;
        let mut best: Option<f32> = None;
        for k in k0..=k1 {
            let c = ((k as f32 + 0.5) / nominal - floor) / depth;
            if c > 0.0
                && c < 1.0
                && let Some(dt) = next_sine_crossing_s(theta, c.sqrt().asin(), omega)
            {
                best = Some(best.map_or(dt, |b| b.min(dt)));
            }
        }
        best
    }

    /// Seconds until a GOLD star's diagonal glints next switch on or off —
    /// `twinkle_peak` is `|sin(ω·age + φ)| ≥ 0.92` (§5.2, §5.5), and the
    /// crossing is exact on the certified luminance clock. `None` for any
    /// other star: nothing else in the theme has a glint to toggle.
    #[must_use]
    pub fn next_glint_toggle_s(&self, now: Instant) -> Option<f32> {
        if !self.gold {
            return None;
        }
        let theta = TWINKLE_OMEGA * self.age_s(now) + self.lum_phase();
        next_sine_crossing_s(theta, TWINKLE_GLINT_FRAC.asin(), TWINKLE_OMEGA)
    }

    /// Seconds until a LIFTING star's integer position next steps (§5.6's
    /// `−3 px/s, integer-stepped`): `pos = origin + round(v₀·age)`, so the
    /// step is where `|v₀|·age` next reaches a half. `None` for a pinned
    /// star, or for a star whose motion is not the lift.
    #[must_use]
    pub fn next_lift_step_s(&self, now: Instant) -> Option<f32> {
        if !matches!(self.lane.motion(), Motion::Lift) {
            return None;
        }
        let age_ms = self.age_s(now) * 1000.0;
        let mut best: Option<f32> = None;
        for v in [self.v0.0.abs(), self.v0.1.abs()] {
            if !v.is_finite() || v <= 0.0 {
                continue;
            }
            let next_ms = ((v * age_ms).round() + 0.5) / v;
            let dt = (next_ms - age_ms) / 1000.0;
            if dt > 0.0 {
                best = Some(best.map_or(dt, |b| b.min(dt)));
            }
        }
        best
    }

    /// The slipstream's integer displacement at `age_ms`, px toward the
    /// pane's first column: `round(stream · stream_unit(age))`. Zero for a
    /// star with no drift — the common case, answered before any `exp`.
    #[inline]
    fn stream_px(&self, age_ms: f32) -> i32 {
        if self.stream <= 0.0 {
            return 0;
        }
        (self.stream * stream_unit(age_ms)).round() as i32
    }

    /// Seconds until a STREAMING star's integer position next steps (the
    /// slipstream, integer-stepped like the lift): `x = origin −
    /// round(stream · u(age))` steps where `stream · u(age)` next reaches a
    /// half, and `u` is the normalised exponential ([`stream_unit`]), so the
    /// step is solved in closed form — `age = −τ · ln(1 − u · STREAM_SPENT)`
    /// — and never polled. `None` for a star with no drift, past
    /// [`STREAM_END_MS`], or whose remaining travel rounds to nothing.
    /// Offered to the cadence fold as a TAIL, under the floor.
    #[must_use]
    pub fn next_stream_step_s(&self, now: Instant) -> Option<f32> {
        if self.stream <= 0.0 || !self.stream.is_finite() {
            return None;
        }
        let age_ms = self.age_s(now) * 1000.0;
        if age_ms >= STREAM_END_MS {
            return None;
        }
        let n = (self.stream * stream_unit(age_ms)).round();
        let u = (n + 0.5) / self.stream;
        if u >= 1.0 {
            return None;
        }
        let t_ms = -STREAM_TAU_MS * (1.0 - u * STREAM_SPENT).ln();
        Some(((t_ms.min(STREAM_END_MS) - age_ms) / 1000.0).max(0.0))
    }

    /// The mend's pull at `age_ms`, px this star still stands LEFT of its
    /// home: `round(pull · spend(age / MEND_PULL_MS))` — the whole pull at
    /// birth, nothing from the window's end (1e). Zero for a star with no
    /// pull, answered before any arithmetic.
    #[inline]
    fn pull_px(&self, age_ms: f32) -> i32 {
        if self.pull <= 0.0 || age_ms >= MEND_PULL_MS {
            return 0;
        }
        (self.pull * spend(age_ms / MEND_PULL_MS)).round() as i32
    }

    /// The centre at `now`, PINNED TO A DEVICE PIXEL (§5.4: sub-pixel drift is
    /// expressed as integer steps of the centre, never as half-lit pixels).
    ///
    /// One of the three laws of [`Motion`], picked by the population. Reduced
    /// motion freezes every mark at its birth pixel (§6.11) — for a mend's
    /// star, its home.
    #[must_use]
    pub fn pos(&self, now: Instant, reduced_motion: bool) -> (i32, i32) {
        if reduced_motion {
            return self.origin();
        }
        let age_ms = self.age_s(now) * 1000.0;
        let (x, y) = self.at_k(self.lane.motion().k_at(age_ms));
        // The slipstream (1d) and the mend's pull (1e) ride on top of the
        // lane's own law, on X only.
        (x - self.stream_px(age_ms) - self.pull_px(age_ms), y)
    }

    /// **WHERE THIS STAR COMES TO REST** — its position at the end of its
    /// motion: a throw's window (`origin + v₀·T`, the reach), a drift's or a
    /// lift's death. The pixel §5.4's transient clearance is asked of
    /// ([`Stardust::settle`]): a free transient "may be thrown anywhere, but a
    /// core never LANDS on a probed glyph", and this is where it lands.
    /// Under reduced motion every mark is pinned at its origin, which is
    /// already clear by the same law.
    #[must_use]
    pub fn rest(&self) -> (i32, i32) {
        let motion = self.lane.motion();
        let end_ms = motion.end_ms(self.life_s() * 1000.0);
        let (x, y) = self.at_k(motion.k_at(end_ms));
        (x - self.stream_px(end_ms), y)
    }

    /// The birth pixel — `x`, `y` are integers by construction (§5.4).
    #[inline]
    fn origin(&self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }

    /// `origin + round(v₀ · k)`, the one integer step every position is.
    #[inline]
    fn at_k(&self, k: f32) -> (i32, i32) {
        let (bx, by) = self.origin();
        if self.v0.0 == 0.0 && self.v0.1 == 0.0 {
            return (bx, by);
        }
        (
            bx + (self.v0.0 * k).round() as i32,
            by + (self.v0.1 * k).round() as i32,
        )
    }

    /// Re-aim the motion so it ENDS exactly on `rest` — the clearance's
    /// nudge and walk-back ([`Stardust::settle`]). `k` is the motion's own
    /// end factor, so `at_k(k)` returns `rest` to the pixel: both ends are
    /// integers and `round(rest − origin) = rest − origin`.
    fn aim(&mut self, rest: (i32, i32), k: f32) {
        let (bx, by) = self.origin();
        if k <= 0.0 {
            self.v0 = (0.0, 0.0);
            return;
        }
        self.v0 = ((rest.0 - bx) as f32 / k, (rest.1 - by) as f32 / k);
    }
}

// ===========================================================================
// 3. The glint bucket (§13)
// ===========================================================================

/// **THE SHARED TOKEN BUCKET** (§8.1 no. 2, §13, D5) — one instance, owned by
/// [`Stardust`], refilled on the frame clock inside `Engine::tick`.
///
/// `tokens = min(GLINT_CAP, tokens + GLINT_REFILL_PER_S · dt)`.
///
/// The full law, restated where it is enforced: **only an m1 spends a token**,
/// and a spent token cues exactly one glint at that star's column on that
/// frame. m2 and m3 never touch the bucket. An *earned* hero (capital, `!`,
/// kitty Delight, the landing fan's own slot) with no token is **born anyway
/// and is silent** — light is never rationed. A *dealt* hero with no token
/// degrades to m2. Meteor-lane stars are exempt, and a meteor **empties** the
/// bucket on its move edge, so typing heroes for the ~250 ms after a meteor
/// are born as m2 — deliberately: nothing chimes on top of the rain.
///
/// Consequences, by construction rather than by clamp: ≤ 4 glints/s, and at
/// 10 cps prose ~0.8-1.5 glints/s — each an event, not a texture. The "≤ 3
/// live" of §13 is the SYNTH's rule (§14: the GLINT lane is capped at 3 and
/// steals its oldest); the bucket knows nothing about voices, because a
/// visual mirror of a voice count that nothing releases is a bucket that shuts
/// for good after three chimes. `the_bucket_keeps_chiming_as_it_refills`.
#[derive(Clone, Copy, Debug)]
pub struct StarBudget {
    /// Available glints, bounded by `GLINT_CAP`.
    tokens: f32,
    /// The last refill stamp; `None` ⇒ never refilled (a cold bucket starts
    /// FULL, so the first capital of a session chimes).
    at: Option<Instant>,
}

impl Default for StarBudget {
    /// The same FULL bucket [`StarBudget::new`] gives — a derived default
    /// would start a session mute.
    fn default() -> Self {
        Self::new()
    }
}

impl StarBudget {
    /// A full bucket (see [`StarBudget::at`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            tokens: GLINT_CAP,
            at: None,
        }
    }

    /// Refill on the frame clock — called once per `Engine::tick`, before any
    /// birth is dealt.
    pub fn refill(&mut self, now: Instant) {
        let dt = self
            .at
            .replace(now)
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        self.tokens = (self.tokens + GLINT_REFILL_PER_S * dt).min(GLINT_CAP);
    }

    /// Try to spend one token for an m1's glint. `true` ⇒ the caller cues
    /// exactly ONE glint at that star's column on this frame. Refuses only
    /// when the bucket is dry: the tokens are the whole law (§13).
    pub fn try_spend(&mut self) -> bool {
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    /// **A METEOR EMPTIES THE BUCKET** on its move edge (§8.1 no. 2, §13).
    /// Not a clamp and not a duck: the tokens are gone, so the ~250 ms of
    /// typing under the rain is silent by construction.
    pub fn empty(&mut self) {
        self.tokens = 0.0;
    }

    /// Tokens on hand — `trail status` and the budget tests.
    #[must_use]
    pub fn tokens(&self) -> f32 {
        self.tokens
    }
}

/// ONE GLINT the sky has EARNED and PAID FOR (§13): one sine at a lattice
/// degree, octave-folded into [2800, 5600) Hz, amplitude-twinkled at
/// [`Glint::f`] — the star's own scintillation rate, so the ear's wink and the
/// eye's wink are the same number.
///
/// **Why the spend is recorded here and not pushed straight at `Frame::cues`.**
/// A token is spent at the BIRTH EDGE, inside [`Stardust::on_event`], which has
/// no frame to write into; [`Stardust::emit`] drains this queue into
/// `frame.cues` as `SoundKind::Stardust` on the SAME tick, so §13's "one token
/// = one hero = one glint, same column, same frame" is a property of the code
/// path rather than of a convention. It is also what makes the budget law
/// measurable without a synth (`hero_births_and_glints_share_one_budget`).
///
/// It carries no instant of its own: the glint's `t = 0` is the tick that
/// drains it, which is the tick the hero was born on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glint {
    /// The hero's column, which the host maps to stereo pan (§13: "pan = the
    /// star's column ± 0.03").
    pub col: u16,
    /// The star's seeded scintillation rate, Hz — the glint's amplitude
    /// twinkle rides it at depth 0.5, so the sound opens and closes with the
    /// light it belongs to.
    pub f: f32,
}

// ===========================================================================
// 4. The glyph probe (§5.4's hard gate, L4, D16)
// ===========================================================================

/// **WHAT THIS FRAME KNOWS ABOUT THE GLYPHS** — the snapshot §5.4's birth
/// requirement is asked of, and the reason a star is never drawn over a
/// stroke (L4).
///
/// Mirrors `CursorGlow::probed_cell_glyph` exactly, including the two answers
/// that are easy to get wrong:
///
/// * an **off-grid** row (above row 0, below the last) is `Some(false)` —
///   provably blank, because the effects box clips those bands into padding
///   and no glyph can live there. This is what §5.4 means by "on row 0 the
///   `Geom.head` band above the grid is used";
/// * a row the probe never covered is `None` — *unknown*, and §5.4 is
///   emphatic that unknown means **no star is born**. There is no in-cell
///   fallback and no descender-gap fallback for ribbon cells; both are
///   deleted (L4, D16);
/// * a cell holding a **light horizontal rule** ([`CellInk::Rule`] — the
///   `─` a TUI draws its input box with) is ink ONLY inside the rule's
///   stroke band ([`RULE_INK_TOP_CH`]`..`[`RULE_INK_BOT_CH`]): the
///   cell-level [`Self::at`] still says `Some(true)` (there is ink in the
///   cell), the pixel-level [`Self::at_px`] says `Some(false)` above and
///   below the stroke — which is where the sky band lies, so a sky star
///   may be born there and its core never touches the stroke. Without this
///   class every input line drawn under a full-width rule (Claude Code's,
///   and every other bordered TUI prompt) probed as a wall of glyphs and
///   the sky above it was dark for the life of the session.
///
/// The host probes the caret's row and its two neighbours, so this holds at
/// most three rows and evicts round-robin. Resident and cleared, never
/// rebuilt (§18). The seam writes it through `Engine::probe_mut` before
/// `tick`; a producer test writes it through [`Stardust::probe_mut`].
#[derive(Clone, Debug, Default)]
pub struct GlyphProbe {
    /// The row storage: the first [`Self::live`] entries are the HELD rows,
    /// most recent last; any entry past them is retired storage kept for its
    /// bitset capacity. At most [`Self::ROWS`] entries ever exist.
    rows: Vec<ProbedRow>,
    /// How many of [`Self::rows`] are held. [`Self::clear`] resets this and
    /// nothing else, so the clear the sky performs on EVERY scroll and band
    /// move — under Codex one per streamed line, 9–233 ms apart — and the
    /// host's re-probe of the caret's rows right after it allocate nothing
    /// (§18: zero allocation on the tick; measured 6 × 32 B per band move
    /// before this count existed, from `rows.clear()` dropping the bitsets).
    live: usize,
    /// Round-robin write cursor into [`Self::rows`].
    at: usize,
}

/// **WHAT ONE PROBED CELL HOLDS**, for §5.4's gate — the host's per-column
/// char reduced to the one distinction the sky cares about.
///
/// The gate is CELL-granular by design (a bitset, §18), and for text that is
/// the right grain: a glyph's ink reaches the baseline and below, so the
/// lower third of its cell — the sky band of the row beneath — is never
/// provably clear. A light horizontal rule is the one glyph a TUI puts in
/// EVERY column of the row above an input line, and its ink is a stroke
/// through the cell's vertical centre and nothing else. [`cell_ink`] names
/// that shape so the probe can answer for it per PIXEL rather than per cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CellInk {
    /// A blank (`' '`): provably clear, `Some(false)` everywhere.
    #[default]
    Blank,
    /// Any other glyph, a wide continuation (`'\0'`) included: `Some(true)`
    /// at every pixel of the cell (L4's law, unchanged).
    Glyph,
    /// A LIGHT horizontal box-drawing rule (`─` U+2500 and its dashed and
    /// half-length siblings): `Some(true)` only inside
    /// [`RULE_INK_TOP_CH`]`..`[`RULE_INK_BOT_CH`], `Some(false)` above and
    /// below it. Heavy (`━`) and double (`═`) rules are NOT this class —
    /// their strokes are up to three times thicker and, on a square cell,
    /// reach into the sky band — so they stay [`Self::Glyph`].
    Rule,
}

/// Classify one probed char for the glyph probe (the host's
/// `Terminal::row_cols_into` convention: the resolved lead char at its
/// column, `'\0'` at a wide continuation, `' '` for a blank).
///
/// The rule set is exactly the LIGHT horizontal members of the Box Drawing
/// block — solid, triple-dash, quadruple-dash, double-dash, left half and
/// right half. Everything with a vertical arm, a heavy or double stroke, or
/// any other ink is a glyph.
#[must_use]
pub const fn cell_ink(ch: char) -> CellInk {
    match ch {
        ' ' => CellInk::Blank,
        '\u{2500}' | '\u{2504}' | '\u{2508}' | '\u{254C}' | '\u{2574}' | '\u{2576}' => {
            CellInk::Rule
        }
        _ => CellInk::Glyph,
    }
}

/// Where a light horizontal rule's stroke BEGINS, as a fraction of the cell
/// height from the cell's top (§5.4, the rule clause). The renderer draws
/// the light stroke `round(min(cw, ch) / 8)` px thick (never under 1 px),
/// centred in the cell (`aterm_render::procedural`'s one rounding rule), so
/// its top edge sits at `(ch − stroke) / 2` — at or above `0.40 · ch` on any
/// cell 8 px or taller, and at `0.46 · ch` on the 15 × 28 cell the owner
/// types on. `0.35` leaves a margin for a font-drawn `─` where procedural
/// synthesis is switched off.
pub const RULE_INK_TOP_CH: f32 = 0.35;

/// …and where it ENDS — the sky zone's own top edge under the shipped `tall`
/// spelling, derived rather than chosen: the ribbon's top is
/// [`super::ribbon::TALL_UP_CH`]` − 1 = 0.10 ch` above the typed cell, and
/// the sky band reaches [`SKY_TOP_CH`] above that, so the zone's highest
/// pixel is `1 − 0.10 − 0.30 = 0.60` of the way down row − 1. The stroke's
/// bottom edge is `(ch + stroke) / 2 ≤ 0.57 · ch` on any cell 8 px or
/// taller, so the band contains the stroke with a margin and is disjoint
/// from the sky by construction — the assertion beneath pins both
/// spellings.
pub const RULE_INK_BOT_CH: f32 = 1.0 - (super::ribbon::TALL_UP_CH - 1.0) - SKY_TOP_CH;

// THE RULE CLAUSE'S WHOLE CLAIM: a sky star born by §5.4's zone never has its
// core on a rule's stroke, in either spelling. Under `tall` the zone is the
// lower `[0.60, 0.86]` of row − 1 and the stroke band ends at 0.60; under
// `underline` the zone is `[0.04, 0.30]` of the typed cell and the stroke
// band starts at 0.35.
const _: () = assert!(
    RULE_INK_BOT_CH <= 1.0 - (super::ribbon::TALL_UP_CH - 1.0) - SKY_TOP_CH,
    "the rule's stroke band must end at or above the tall sky zone's top edge"
);
const _: () = assert!(
    SKY_TOP_CH <= RULE_INK_TOP_CH,
    "the underline sky zone must end above the rule's stroke band"
);
const _: () = assert!(
    RULE_INK_TOP_CH < 0.5 && 0.5 < RULE_INK_BOT_CH,
    "the rule's stroke band must contain the cell's vertical centre"
);

/// One probed row of the grid: which columns carry ink.
#[derive(Clone, Debug, Default)]
struct ProbedRow {
    /// The grid row, signed so an off-grid probe is representable.
    row: i32,
    /// One bit per column, LSB-first — resident, cleared, never rebuilt.
    /// Set for [`CellInk::Glyph`] AND [`CellInk::Rule`]: the cell has ink.
    ink: Vec<u64>,
    /// One bit per column, set where that ink is a [`CellInk::Rule`] — the
    /// pixel-level answer then depends on the row band asked.
    rule: Vec<u64>,
    /// Columns actually captured; a column past this holds nothing to
    /// overprint (the same convention v1's own-cell gate always used).
    width: u16,
}

impl GlyphProbe {
    /// How many rows a probe holds — the caret's and its two neighbours,
    /// which is exactly what `CursorGlow` captures.
    pub const ROWS: usize = 3;

    /// Nothing probed: every in-grid cell answers `None`, so no sky star is
    /// born until the host has looked. That is §5.4's law and not a
    /// conservative default.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that grid `row` was probed, with `occupied[col]` true where a
    /// glyph stands. Replaces any earlier record of the same row. The bool
    /// view knows only [`CellInk::Blank`] and [`CellInk::Glyph`]; a host
    /// with the chars in hand writes [`Self::probe_row_ink`] instead.
    pub fn probe_row(&mut self, row: i32, occupied: &[bool]) {
        let slot = self.slot_for(row, occupied.len());
        let r = &mut self.rows[slot];
        for (col, &on) in occupied.iter().enumerate() {
            if on {
                r.ink[col / 64] |= 1u64 << (col % 64);
            }
        }
    }

    /// [`Self::probe_row`] with the per-cell class the host reduced its
    /// chars to ([`cell_ink`]): a [`CellInk::Rule`] is recorded as ink
    /// (the cell-level [`Self::at`] answer) AND as a rule (the pixel-level
    /// [`Self::at_px`] answer inside/outside the stroke band).
    pub fn probe_row_ink(&mut self, row: i32, cells: &[CellInk]) {
        let slot = self.slot_for(row, cells.len());
        let r = &mut self.rows[slot];
        for (col, &cell) in cells.iter().enumerate() {
            let bit = 1u64 << (col % 64);
            match cell {
                CellInk::Blank => {}
                CellInk::Glyph => r.ink[col / 64] |= bit,
                CellInk::Rule => {
                    r.ink[col / 64] |= bit;
                    r.rule[col / 64] |= bit;
                }
            }
        }
    }

    /// The slot `row` is written into — its own if it is already held,
    /// a fresh one while there is room, else the round-robin victim — with
    /// both bitsets cleared and sized for `width` columns.
    fn slot_for(&mut self, row: i32, width: usize) -> usize {
        let words = width.div_ceil(64);
        let slot = if let Some(i) = self.rows[..self.live].iter().position(|r| r.row == row) {
            i
        } else if self.live < Self::ROWS {
            if self.rows.len() == self.live {
                self.rows.push(ProbedRow::default());
            }
            self.live += 1;
            self.live - 1
        } else {
            let i = self.at % Self::ROWS;
            self.at = (self.at + 1) % Self::ROWS;
            i
        };
        let r = &mut self.rows[slot];
        r.row = row;
        r.width = width.min(usize::from(u16::MAX)) as u16;
        r.ink.clear();
        r.ink.resize(words, 0);
        r.rule.clear();
        r.rule.resize(words, 0);
        slot
    }

    /// Forget everything — a reset, a layout change, a style switch, and
    /// every scroll or band move (the host re-probes before the next deal).
    /// The rows' bitsets are KEPT as retired storage ([`Self::live`]), so
    /// the re-probe that follows allocates nothing.
    pub fn clear(&mut self) {
        self.live = 0;
        self.at = 0;
    }

    /// **THE GATE** (§5.4): `Some(true)` a glyph is there, `Some(false)`
    /// provably blank, `None` outside this probe's knowledge.
    #[must_use]
    pub fn at(&self, row: i32, col: u16, rows: usize) -> Option<bool> {
        if row < 0 || row as usize >= rows {
            // Off-grid bands hold no glyphs by construction.
            return Some(false);
        }
        let r = self.rows[..self.live].iter().find(|r| r.row == row)?;
        if col >= r.width {
            return Some(false);
        }
        let i = usize::from(col);
        Some(r.ink.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1))
    }

    /// [`Self::at`] asked of a WINDOW PIXEL: the cell under `(x, y)`. A
    /// pixel left or right of the grid is off-grid, hence `Some(false)`.
    ///
    /// Over a [`CellInk::Rule`] cell the answer is the STROKE's, not the
    /// cell's: `Some(true)` for a pixel inside
    /// [`RULE_INK_TOP_CH`]`..`[`RULE_INK_BOT_CH`] of the cell height,
    /// `Some(false)` above or below it. This is the one place the probe
    /// answers finer than a cell, and it is what lets §5.4's sky zone —
    /// disjoint from that band by construction — be born over the rule a
    /// TUI draws above its input line.
    #[must_use]
    pub fn at_px(&self, x: i32, y: i32, geom: Geom) -> Option<bool> {
        let col = px_col(x as f32, geom);
        if col < 0 || col as usize >= geom.cols {
            return Some(false);
        }
        let row = px_row(y as f32, geom);
        let inked = self.at(row, col as u16, geom.rows)?;
        if inked && self.rule_at(row, col as u16) {
            // The fraction of the cell height this pixel sits at, `0` at the
            // cell's top edge.
            let fy = (y as f32 - f32::from(geom.origin_y)) / geom.ch.max(1) as f32 - row as f32;
            return Some((RULE_INK_TOP_CH..RULE_INK_BOT_CH).contains(&fy));
        }
        Some(inked)
    }

    /// [`Self::at`] for §5.4's SKY question: does this `row − 1` cell carry
    /// ink the zone `[top − 0.30 ch, top − 0.04 ch]` could touch? A light
    /// rule ([`CellInk::Rule`]) answers `Some(false)` here — its stroke band
    /// ends where the zone begins (the assertions beside
    /// [`RULE_INK_BOT_CH`]) — while [`Self::at`] keeps saying `Some(true)`
    /// for it: the cell has ink, the sky above the ribbon does not. Every
    /// other answer is [`Self::at`]'s, byte for byte.
    #[must_use]
    pub fn sky_at(&self, row: i32, col: u16, rows: usize) -> Option<bool> {
        let inked = self.at(row, col, rows)?;
        Some(inked && !self.rule_at(row, col))
    }

    /// Is the (in-grid, held) cell's ink a [`CellInk::Rule`]? Asked only
    /// after [`Self::at`] answered `Some(true)`, so a miss is simply "no".
    fn rule_at(&self, row: i32, col: u16) -> bool {
        let Some(r) = self.rows[..self.live].iter().find(|r| r.row == row) else {
            return false;
        };
        let i = usize::from(col);
        r.rule.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
    }

    /// True when the probe has no rows at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
}

// ===========================================================================
// 5. Sow specs — the transient-lane entry points meteor.rs needs (§6.5)
// ===========================================================================

/// What the meteor hands stardust to sputter ONE SHED STATION (§6.5 layer 5,
/// §5.6): the m3 grain(s) born on the frame the head passes a hashed station
/// `s_i`, transient-priced, meteor-lane and **silent**.
///
/// The meteor owns the STATIONS (`s_i ∈ [0.10, 0.70]·L`, `SHED_N(cells)` of
/// them) and their timing — it calls [`Stardust::sow_shed`] once per station
/// as the head passes it, with `n = 1`. Stardust owns what a fragment IS: its
/// inherited velocity, its perpendicular sputter, its drag, its pricing. A
/// record rather than five arguments because every field is part of one
/// physical statement: where the piece came off, how fast the head was going,
/// what colour the train was there, and which piece it is.
///
/// **Units.** `at_px` and `v_head` are window px and px/ms; `tint_t` is a
/// position on the meteor's own arc (`t_m(s_i)`, §6.4); `seed` is per station.
#[derive(Clone, Copy, Debug)]
pub struct ShedSow {
    /// Window-absolute px of the STATION the head just passed — the fragment
    /// is born exactly here.
    pub at_px: (f32, f32),
    /// The head's OWN velocity in px/ms, unscaled. The fragment inherits
    /// [`SHED_ALONG_SHARE`] of it along the path (this producer applies the
    /// share) and sputters [`SHED_PERP_MIN`]..[`SHED_PERP_MAX`] px/ms
    /// perpendicular to it, under [`DRAG_TAU_MS`].
    pub v_head: (f32, f32),
    /// The meteor's own arc position at this station. §6.5 layer 5: the
    /// fragment wears `spectrum_snap(t_m(s_i))` — exactly the colour the
    /// train wears where it was shed, so it reads as a piece that came OFF the
    /// meteor (C1: point marks snap). Not the 60/25/15 deal.
    pub tint_t: f32,
    /// This station's seed, hashed by the meteor from the flight's seed and
    /// the station index — the same flight sheds the same debris every time
    /// (§18).
    pub seed: u32,
    /// How many grains at this station; the meteor's law is one.
    pub n: u16,
}

/// What the meteor hands stardust for its LANDING FAN (§6.5 layer 11, §5.6) —
/// "every landing is its own party".
///
/// The composition is fixed by D6 and not by taste: **1 gold m1 + 4 m2 + rest
/// m3**, in throw order, so the fan's heroes pair 1:1 with the five rain
/// glints. An Enter lands small and carries no hero ([`Self::hero`] false,
/// D8: one m2 + 5-7 m3). Colour is **ROYGBIV in order around the fan** (§6.5
/// layer 11), walked from the landing's own stop ([`Self::tint_t`], the pin's
/// colour) so the fan and the pin read as one landing, the hero gold; the fan
/// reads no field.
///
/// **Units.** `at_px` is window px; `reach` is px — the length of the
/// outermost star's throw once [`FAN_THROW_MS`] has elapsed (each star at
/// `reach·(0.55..1.05)`, seeded), ALONG THE LINE: the vertical is the sky's
/// own law ([`fan_rise`] — the ring's squash, capped at [`FAN_RISE_MAX_CH`]
/// and at the glass), and a ray that would rise past it is flattened, never
/// dropped. The caller hands the reach and this producer builds every `v0`;
/// it never needs the window.
#[derive(Clone, Copy, Debug)]
pub struct FanSow {
    /// Window-absolute px of the landing cell's centre.
    pub at_px: (f32, f32),
    /// Throw reach in px — the length of the outermost star's throw on
    /// [`Motion::Throw`]'s ease-out over [`FAN_THROW_MS`], along the line;
    /// its rise is the sky's ([`fan_rise`]).
    pub reach: f32,
    /// The landing cell's own arc position — §6.4's `t_m(0)`, the pin's
    /// colour. The fan's ROYGBIV walk STARTS at this stop, so the fan and the
    /// pin read as one landing (§6.5 layer 11: "ROYGBIV in order around the
    /// fan"). An arc position, not a field read: the meteor resolves it on
    /// its own arc (D4).
    pub tint_t: f32,
    /// The landing's seed — the whole of the party's variance (§6.5).
    pub seed: u32,
    /// How many stars, `min(5 + cells/1.8, 14) + party·4` capped at
    /// `timing::FAN_MAX_N`. Thinned by [`LIGHT_COUNT_SCALE`] on a light
    /// theme, never below the hero slots.
    pub n: u8,
    /// Whether this landing carries its gold m1 hero. False for an Enter (D8).
    pub hero: bool,
}

/// ONE SKY BIRTH, as [`Stardust::deal_star`] takes it (§5.6's Strike and Field
/// populations). A record rather than four positional arguments because
/// `earned` is the field that changes the OUTCOME of a dry bucket (D5), and a
/// bare `bool` at a call site is how that ruling gets inverted by accident.
#[derive(Clone, Copy, Debug)]
struct Deal {
    /// The magnitude asked for; the budget and the spacing law may lower it.
    class: StarClass,
    /// Strike or Field (§5.6).
    lane: StarLane,
    /// The glyph cell — the sky band is resolved against ITS ribbon band top.
    cell: (u16, u16),
    /// EARNED (§5.8: a shifted capital, `!`, kitty Delight, the fan's own
    /// slot) rather than dealt. An earned hero is ALWAYS born as an m1 — no
    /// spacing law and no dry bucket demotes it; only a dealt one is spaced,
    /// row-capped, and degraded to m2 for want of a token (D5).
    earned: bool,
    /// **THE MEND'S PULL** (1e): `Some` for the star of a key that mends a
    /// typo run — born over the erased cell and pulled into `cell`, the
    /// caret's.
    pull: Option<Pull>,
    /// A tint fixed BY RULE rather than dealt — `(rgb, gold)`, exactly
    /// [`deal_tint`]'s pair. `None` everywhere except the combo ladder's two
    /// top rungs, whose colour is the payout ([`LADDER_WHITE_KEYS`],
    /// [`LADDER_GOLD_KEYS`]).
    tint: Option<(u32, bool)>,
}

/// Where a mend's star is pulled FROM, as [`Stardust::deal_star`] takes it
/// ([`MEND_PULL_MS`]).
#[derive(Clone, Copy, Debug)]
struct Pull {
    /// The erased cell ([`super::Mend::row`], [`super::Mend::col`]).
    from: (u16, u16),
    /// The momentum the deleting interrupted ([`super::Mend::disp`]): the
    /// birth spine the star's slipstream is gated on, beside the live one —
    /// a star born at the momentum the fix resumes.
    disp: f32,
}

/// ONE FREE TRANSIENT, as [`Stardust::throw`] takes it (§5.6's Erase, Shed,
/// Fan and MiniFan populations). Its tint is already resolved, because the
/// fan's own hero is gold by rule and not by deal (D6).
#[derive(Clone, Copy, Debug)]
struct Throw {
    /// The magnitude — transients are never demoted; they never touch the
    /// bucket (§13).
    class: StarClass,
    /// Which transient population, for pricing, envelope and motion.
    lane: StarLane,
    /// Window-absolute px of the throw's origin.
    at_px: (f32, f32),
    /// Birth velocity, px/ms.
    v0: (f32, f32),
    /// Chroma, constant for life (C3).
    tint: u32,
    /// Gold — the fan's hero slot (D6).
    gold: bool,
    /// The deterministic seed (§18).
    seed: u32,
    /// LAW-FIXED: a slot the theme promises — the fan's gold m1 and four m2
    /// (D6: the five rain glints pair 1:1 with them), the mini-fan's three
    /// (D17: "exactly 1 m2 + 2 m3"). Such a star is NEVER dropped by the
    /// clearance: a rest it cannot clear within 2 px is walked back along
    /// its own throw to the first clear pixel ([`Stardust::settle`]). A grain
    /// is not fixed and is dropped rather than drawn on ink (§5.4).
    fixed: bool,
}

// ===========================================================================
// 6. The producer
// ===========================================================================

/// THE STARDUST producer: the star pool, the shared bucket, and the hero
/// spacing memory.
#[derive(Clone, Debug)]
pub struct Stardust {
    /// The live pool, resident and reused, [`STAR_CAP`] live plus
    /// [`STAR_FINISH_SLACK`] finishing (§18: fixed pools, zero allocation per
    /// frame). PRIVATE: read through [`Stardust::live_iter`], written only by
    /// [`Stardust::sow`] — the pool has exactly two lawful entries,
    /// [`Stardust::deal_star`] (the sky's probe gate) and [`Stardust::throw`]
    /// (the transients' clearance), and a field a caller could `push` onto
    /// would be a third that skips the cap, the gate and `settle`. The
    /// scroll seam it was once opened for goes through
    /// [`Stardust::translate_scroll`].
    stars: Vec<Star>,
    /// The one glint bucket (§13).
    pub budget: StarBudget,
    /// The last hero's `(row, col)` — a diagnostic, translated by the scroll
    /// seam. §5.2's spacing and row cap are read off the LIVE POOL
    /// ([`Stardust::hero_allowed`]), not off this: a memory that nothing
    /// cleared when the hero died would ration heroes a minute later.
    last_hero: Option<(u16, u16)>,
    /// What this frame knows about the glyphs (§5.4's hard gate). Written by
    /// the host through `Engine::probe_mut` → [`Stardust::probe_mut`] before
    /// the deal pass.
    probe: GlyphProbe,
    /// Glints minted and PAID FOR this frame, drained by the engine (§13).
    /// Resident; `drain` keeps capacity.
    glints: Vec<Glint>,
    /// How many stars this producer has ever minted. Folded into every seed
    /// so two stars born on the same cell at the same `now` still differ,
    /// without reading a clock or an RNG (§18's determinism rule: the counter
    /// is a pure function of the event sequence).
    minted: u32,
    /// The reduced-motion posture of the last frame drawn, cached for the
    /// cadence law (seam point 9 is asked with a clock and nothing else).
    reduced: bool,
    /// The cell height of the last frame drawn, px — what the size clock's
    /// nominal arm is read at ([`Star::next_arm_step_s`]).
    cell_h: f32,
    /// **THE AURORA** — one [`Veil`] per laid cell whose sky the probe proved
    /// blank, [`AURORA_CAP`] at most, resident and reused (§18). Written by
    /// [`Stardust::deal_veil`] on the key that laid the cell; re-lit,
    /// finished, embered, scrolled and culled beside the field stars, whose
    /// envelope it rides.
    veil: Vec<Veil>,
    /// The instant of the last Typed key this sky saw — the slipstream's
    /// cadence memory ([`Stardust::note_cadence`], T4). `None` until the
    /// session's first key; a reset forgets it.
    last_typed: Option<Instant>,
    /// **THE OBSERVED KEY INTERVAL**, ms, clamped to
    /// `[STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS]` — the interval between the
    /// last two Typed keys, the one number every slipstream birth is priced
    /// from ([`stream_px_per_s`]). The slow end until two keys have been
    /// seen: a first key streams nothing.
    ioi_ms: f32,
}

impl Default for Stardust {
    /// The same sky [`Stardust::new`] builds — one constructor contract, so a
    /// caller reaching this through `Default` gets the full bucket and the
    /// reserved pool.
    fn default() -> Self {
        Self::new()
    }
}

impl Stardust {
    /// An empty sky with a full bucket and the pool reserved once (§18).
    #[must_use]
    pub fn new() -> Self {
        Self {
            stars: Vec::with_capacity(STAR_POOL_MAX),
            budget: StarBudget::new(),
            last_hero: None,
            probe: GlyphProbe::new(),
            glints: Vec::with_capacity(usize::from(HERO_MAX_LIVE_PER_ROW) * 2),
            minted: 0,
            reduced: false,
            cell_h: 0.0,
            veil: Vec::with_capacity(AURORA_CAP),
            last_typed: None,
            ioi_ms: STREAM_IOI_MAX_MS,
        }
    }

    /// The aurora's live cells — what is in the veil pool, finishing ones
    /// included (they are still on glass).
    pub fn veil_iter(&self) -> impl Iterator<Item = &Veil> {
        self.veil.iter()
    }

    /// The frame's glyph truth, for the host to write before the deal pass
    /// (§5.4). Until it is written every in-grid cell is UNKNOWN and no sky
    /// star is born — the law, stated as a default.
    pub fn probe_mut(&mut self) -> &mut GlyphProbe {
        &mut self.probe
    }

    /// Read-only view of the frame's glyph truth.
    #[must_use]
    pub fn probe(&self) -> &GlyphProbe {
        &self.probe
    }

    /// Every star in the pool, finishing ones included — what is on glass.
    pub fn live_iter(&self) -> impl Iterator<Item = &Star> {
        self.stars.iter()
    }

    /// Drain the glints minted since the last [`Stardust::emit`] (§13) — one
    /// per token spent, at the hero's own column.
    ///
    /// A ticking host will always find this EMPTY, because `emit` writes them
    /// into `Frame::cues`. It exists for the seam's non-ticking callers (a
    /// style switch, a shutdown), so a glint minted between frames is never
    /// silently dropped — the same contract `Engine::drain_sound_cues`
    /// carries one level up. The one thing that does drop them is
    /// [`Stardust::reset`], which is a new session.
    pub fn take_glints(&mut self) -> std::vec::Drain<'_, Glint> {
        self.glints.drain(..)
    }

    /// Deal this event's births, against the ribbon's PLANNED bands.
    ///
    /// Called after [`Ribbon::plan`] and before any emit, because §5.4's zones
    /// are relative to [`super::ribbon::Band::top`] and the cell the star sits
    /// over may have been laid on this very frame (T2: everything the event
    /// owns exists on frame 0, at full brightness).
    ///
    /// §5.6's population table, in order: **Strike** on every typed glyph
    /// (cold keys included), **Field** once per laid ribbon cell, **Erase** on
    /// every Backspace (3 m3 + 1 m2 at the erased cell) and per 3 cells of a
    /// kill (one star per 3 cells, cap 8, spread along the drained span). The
    /// meteor's own populations (Shed, Fan, MiniFan) arrive through
    /// [`Stardust::sow_shed`] / [`Stardust::sow_fan`] /
    /// [`Stardust::sow_mini_fan`] instead, because the meteor owns their
    /// geometry and their trigger classification — a second classifier here
    /// is how a gesture ends up speaking twice.
    ///
    /// A typed key first RE-LIGHTS the field of the caret's row
    /// ([`Stardust::relight_field`]) — the field fade runs from the last key
    /// on the row — and only then deals; the two erase arms put the FIELD
    /// stars of the retracted cells on the ribbon's own retract span (§5.6:
    /// "its field stars die with their cells"; §8.2: 240 ms, or `12·n + 240`
    /// for a kill); and a focus loss puts every star on the 300 ms ember
    /// beside the ribbon's (§8.2) — a fade beside a fade, never a pop beside
    /// a fade.
    pub fn on_event(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        match *ev {
            Event::Typed { cells, class, .. } => {
                self.note_cadence(at, cells);
                self.relight_field(ctx.caret, at, ctx.geom);
                self.deal_typed(cells, class, at, ctx, ribbon);
            }
            Event::Erase => {
                self.finish_field(ctx.caret, FIELD_RETRACT_BASE_MS, at, ctx.geom);
                self.deal_backspace(at, ctx);
            }
            // Both kill scales throw the SAME population: §5.6 prices it per
            // 3 cells either way and the spine drains identically for both
            // (§19.1 — erase weight is a counter, not a sixth integrator).
            // `KillScope` separates the two at the synth (`KillWord`'s poof vs
            // `Kill`'s swoosh), not here.
            Event::Kill { cells, .. } => {
                let span = FIELD_RETRACT_BASE_MS + FIELD_RETRACT_PER_CELL_MS * f32::from(cells);
                self.finish_field(ctx.caret, span, at, ctx.geom);
                self.deal_kill(cells, at, ctx);
            }
            // A move mints no star here: §6.12's mini-fan and §6.5's fan and
            // shed are the meteor's, and T1 keeps geometry born in one place.
            Event::Move { .. } | Event::Return | Event::Sweep { .. } => {}
            Event::Focus(false) => self.ember(at),
            Event::Focus(true) | Event::ReducedMotion(_) => {}
        }
    }

    /// **AN EARNED HERO** (§5.8) — a shifted capital, `!`, kitty Delight, or
    /// the landing fan's own slot. Always born as an m1; it cues a glint only
    /// if a token is available (D5), and it is never demoted for want of one:
    /// light is not rationed, only sound is.
    pub fn earn_hero(&mut self, at: Instant, cell: (u16, u16), ctx: &Ctx<'_>, ribbon: &Ribbon) {
        let spec = Deal {
            class: StarClass::M1,
            lane: StarLane::Strike,
            cell,
            earned: true,
            pull: None,
            tint: None,
        };
        self.deal_star(spec, at, ctx, ribbon);
    }

    /// The one entry point through which a fully-formed star enters the pool:
    /// it applies the 40-cap (§5.6) and nothing else. PRIVATE, because every
    /// lawful birth goes through [`Stardust::deal_star`] (the sky's probe gate)
    /// or [`Stardust::throw`] (the transients' clearance); a public raw sow was
    /// how a fan once landed its cores on ink.
    fn sow(&mut self, star: Star) {
        self.make_room(star.born);
        self.stars.push(star);
    }

    /// **THE CAT CATCHES A STAR** (panel #10(b)) — shorten the life of the
    /// ONE live star whose home pixel and birth edge are `(x, y, born)`, on
    /// the frame the paw lands. Returns whether a star was there to catch.
    ///
    /// **Exactly one life moves, structurally.** The triple is a star's
    /// identity: `x`/`y` are pinned device pixels at birth (§5.4) and `born`
    /// is the edge, so the search is a `find` — it stops at the first match
    /// and touches nothing else in the pool. A star that already died, or a
    /// second paw landing on an offer already spent, finds nothing (or finds
    /// the same star and re-asserts a finish [`Star::finish_by`] refuses to
    /// lengthen), so the catch is idempotent and can never shorten a
    /// bystander.
    ///
    /// **Only a gold m1 of the sky is catchable**, the class the offer names
    /// — the reach law is the router's ([`super::companion::PetOffer`]), but
    /// the CLASS law is asserted here so a mis-wired receiver cannot spend a
    /// grain's life. And nothing is BORN here: the catch spends a star a
    /// keystroke already made (T1) and mints no light of its own — the pet's
    /// own motion is not light.
    pub fn catch(&mut self, x: f32, y: f32, born: Instant, at: Instant) -> bool {
        let Some(star) = self.stars.iter_mut().find(|s| {
            s.gold
                && s.class == StarClass::M1
                && s.lane.is_sky()
                && s.born == born
                && s.x == x
                && s.y == y
        }) else {
            return false;
        };
        star.finish_by(at, CATCH_FINISH_MS / 1000.0);
        true
    }

    /// **SHED FRAGMENTS** (§6.5 layer 5): `spec.n` m3 grains born **AT THE
    /// STATION** `spec.at_px`, each inheriting [`SHED_ALONG_SHARE`] of the
    /// head's velocity plus a seeded perpendicular sputter, on
    /// [`Motion::Drag`] at [`DRAG_TAU_MS`]. Transient-priced, meteor-lane,
    /// silent. On a light theme each grain is dealt at [`LIGHT_COUNT_SCALE`]
    /// by its own seed (§5.2: counts × 0.6 in every population).
    ///
    /// **The station arithmetic is the meteor's, and only the meteor's.** It
    /// knows the path, hashes `s_i ∈ [0.10, 0.70]·L`, and hands over the
    /// finished window-absolute point and the arc colour there; this side
    /// adds nothing along the path. (A second, speed-scaled offset once
    /// applied here put fragments up to eight cells behind their station —
    /// behind the launch cell on a short flight — wearing a stop the train
    /// never wore there.) The clearance may still move the REST ≤ 2 px
    /// ([`Stardust::settle`]); the birth pixel is the station, rounded.
    pub fn sow_shed(&mut self, at: Instant, spec: ShedSow, ctx: &Ctx<'_>) {
        let (vx, vy) = spec.v_head;
        let speed = vx.hypot(vy).max(f32::EPSILON);
        let (ux, uy) = (vx / speed, vy / speed);
        for k in 0..spec.n {
            let seed = mix32(spec.seed ^ u32::from(k).wrapping_mul(0x9E37_79B9));
            if !light_keeps(seed, ctx.cfg) {
                continue;
            }
            let side = if seed & 1 == 0 { 1.0 } else { -1.0 };
            let perp = side
                * (SHED_PERP_MIN + (SHED_PERP_MAX - SHED_PERP_MIN) * hash01(mix32(seed ^ 1)))
                * px_scale(ctx.geom.ch as f32);
            let v0 = (
                ux * speed * SHED_ALONG_SHARE - uy * perp,
                uy * speed * SHED_ALONG_SHARE + ux * perp,
            );
            self.throw(
                Throw {
                    class: StarClass::M3,
                    lane: StarLane::Shed,
                    at_px: spec.at_px,
                    v0,
                    // §6.5 layer 5: the train's own colour where it was shed.
                    tint: spectrum_snap(spec.tint_t),
                    gold: false,
                    seed,
                    // A grain, and silent: nudged or dropped like any grain.
                    fixed: false,
                },
                at,
                ctx,
            );
        }
    }

    /// **THE LANDING FAN** (§6.5 layer 11, D6): **1 gold m1 + 4 m2 + rest m3**
    /// in throw order, thrown radially on [`Motion::Throw`]'s ease-out over
    /// [`FAN_THROW_MS`] to `reach·(0.55..1.05)`, coloured ROYGBIV in order
    /// around the fan from the landing's own stop. The hero slot is one of
    /// §5.8's five EARNED heroes, so it is always an m1 when `spec.hero` — but
    /// it is meteor-lane and therefore never touches the typing bucket (§13):
    /// the rain is its sound. On a light theme the m3 slots are dealt at
    /// [`LIGHT_COUNT_SCALE`]; the hero and its four m2 are law-fixed (D6).
    ///
    /// **The five are unconditional.** They are born on the landing cell —
    /// the caret's own, ledger-exempt (L2, §3.4) — and the clearance is on
    /// where each comes to REST ([`Stardust::settle`]): a Home/End onto a
    /// glyph still throws its hero and four m2, and a hero whose rest cannot
    /// be cleared within 2 px is walked back along its own throw rather than
    /// dropped, because the five rain glints are counted against these five
    /// and a landing with no stars would leave the rain nothing to ride.
    /// The grains are dropped where they cannot clear (§5.4).
    pub fn sow_fan(&mut self, at: Instant, spec: FanSow, ctx: &Ctx<'_>) {
        let n = usize::from(spec.n).max(1);
        let stop0 = spectrum_snap_index(spec.tint_t);
        // THE VERTICAL REACH LAW: the landing's rise caps, once per fan.
        let (rise_up, rise_down) = fan_rise(spec.reach, spec.at_px, ctx.geom);
        for k in 0..n {
            let seed = mix32(spec.seed ^ (k as u32).wrapping_mul(0x85EB_CA6B));
            let class = fan_class(k, spec.hero);
            if class == StarClass::M3 && !light_keeps(seed, ctx.cfg) {
                continue;
            }
            // Even radial spread with a seeded stagger, so four landings of
            // the same size differ in position as well as in count (§20.1's
            // `every_landing_is_its_own_party`).
            let a = (k as f32 / n as f32 + hash01(seed) / n as f32) * std::f32::consts::TAU;
            let reach = spec.reach
                * (FAN_THROW_JITTER_MIN
                    + (FAN_THROW_JITTER_MAX - FAN_THROW_JITTER_MIN) * hash01(mix32(seed ^ 3)));
            // The ray, flattened toward the line where it would rise past
            // its cap — the same length, spent along the line; never dropped.
            let (dx, dy) = fan_ray(a, reach, rise_up, rise_down, hash01(mix32(seed ^ 5)));
            let v0 = (dx / FAN_THROW_MS, dy / FAN_THROW_MS);
            let gold = class == StarClass::M1 && spec.hero;
            let tint = if gold {
                // D6: the fan's own hero is gold, always — it is the mark the
                // five rain glints are counted against.
                TINT_GOLD_RGB
            } else {
                spectrum_stop((stop0 + k) % SPECTRUM_STOPS)
            };
            self.throw(
                Throw {
                    class,
                    lane: StarLane::Fan,
                    at_px: spec.at_px,
                    v0,
                    tint,
                    gold,
                    seed,
                    // D6 / D8: the hero and the m2 slots are the landing's
                    // promise; only a grain may be dropped on ink.
                    fixed: class != StarClass::M3,
                },
                at,
                ctx,
            );
        }
    }

    /// **THE MINI-FAN** (§6.12, D17) — a same-row nav hop of 2-7 cells, under
    /// the meteor's own floor. Exactly 1 m2 + 2 m3, thrown 1-2 px over
    /// [`MINI_FAN_THROW_MS`], gone by 245 ms: a hop is acknowledged, not
    /// celebrated. On a light theme the two grains are dealt at
    /// [`LIGHT_COUNT_SCALE`]; the m2 is law-fixed (D17).
    ///
    /// `at_px` is the landing cell's centre in window px; `seed` is the hop's.
    pub fn sow_mini_fan(&mut self, at: Instant, at_px: (f32, f32), seed: u32, ctx: &Ctx<'_>) {
        let n = MINI_FAN_M2_N + MINI_FAN_M3_N;
        for k in 0..n {
            let s = mix32(seed ^ u32::from(k).wrapping_mul(0xC2B2_AE35));
            let class = if k < MINI_FAN_M2_N {
                StarClass::M2
            } else {
                StarClass::M3
            };
            if class == StarClass::M3 && !light_keeps(s, ctx.cfg) {
                continue;
            }
            let a =
                (f32::from(k) / f32::from(n) + hash01(s) / f32::from(n)) * std::f32::consts::TAU;
            let reach = (MINI_FAN_REACH_MIN_PX
                + (MINI_FAN_REACH_MAX_PX - MINI_FAN_REACH_MIN_PX) * hash01(mix32(s ^ 5)))
                * px_scale(ctx.geom.ch as f32);
            let v0 = (
                a.cos() * reach / MINI_FAN_THROW_MS,
                a.sin() * reach / MINI_FAN_THROW_MS,
            );
            let (tint, gold) = class_tint(class, s, ctx.caret_t);
            self.throw(
                Throw {
                    class,
                    lane: StarLane::MiniFan,
                    at_px,
                    v0,
                    tint,
                    gold,
                    seed: s,
                    // D17: "exactly 1 m2 + 2 m3" — a hop onto a word's first
                    // glyph still winks; the 1-2 px throw rests on the
                    // landing cell, the caret's own.
                    fixed: true,
                },
                at,
                ctx,
            );
        }
    }

    /// Draw the sky.
    ///
    /// Called LAST in the emit order (§6.5), which is also why §18's
    /// `halos.truncate` sheds stardust first: the sky is the layer that may be
    /// thinned, the meteor and the ribbon are not.
    ///
    /// §5.7's three recipes verbatim, on `effect_util::push_twinkle_star` (D2:
    /// body and nucleus are `#FFFFFF` in EVERY class) and a local
    /// premultiplied halo; the two scintillation clocks of §5.5; the
    /// light-theme twins of §3.3; and the cull at [`STAR_CULL_ALPHA`] with
    /// chroma held constant (C3).
    ///
    /// **Budgets are spent per star.** The quad budget is shared out as a
    /// fair share of what is left among the stars still to draw (floored at
    /// a grain's five quads while five remain — every recipe, the grain's
    /// included, checks its share between pushes), so an over-budget frame
    /// thins EVERY star's faintest extremities rather than dropping whole
    /// stars after a break, and the frame never exceeds
    /// [`STARDUST_QUAD_BUDGET`]; the halo budget counts haloed STARS, not the
    /// per-row splits a halo is cut into.
    ///
    /// **Brightest first: the heroes, then the m2s, then the grains.** A
    /// star is priced over the sky laid BEFORE it ([`sky_ground`]); what a
    /// star drawn later lays on its pixels is not priced. The only light
    /// that reaches a neighbouring cell's star is a HALO's tail (a hero's 11
    /// px at `ch` 18 against a 9 px cell; bodies and stubs stay inside
    /// their own cell), so drawing the haloed classes first puts every tail
    /// under every core it can reach — two heroes are three cells apart
    /// (§5.2) and out of each other's reach, and an m2's floored halo does
    /// not reach the next cell at any scale. Measured on the 12 cps fixture
    /// with one star per cell: worst Y 73.2 in pool order (12 of 40 frames
    /// over the caret law's 72 by a grain's core under a later hero's
    /// tail), under the ceiling on every frame brightest-first
    /// (`one_sky_star_per_cell_and_no_pile_outshines_the_caret`).
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        // §13: the token was spent at the birth edge; the cue rides out on the
        // same tick, at the hero's own column. `dir` is 0 — a star has no
        // travel direction — and `shifted` is false, because only the key-time
        // seam may set it (a birth is an echo, and an echo has no key).
        for g in self.glints.drain(..) {
            frame.cues.push(SoundCue {
                kind: SoundKind::Stardust {
                    twinkle_hz: g.f.round().clamp(0.0, 255.0) as u8,
                },
                col: g.col,
                heat: ctx.disp,
                hue: ctx.caret_t,
                dir: 0,
                shifted: false,
            });
        }
        self.cull(ctx.now);
        if self.stars.is_empty() && self.veil.is_empty() {
            return;
        }
        let quad_cap = frame.out.len() + STARDUST_QUAD_BUDGET;
        let halo_cap = frame.halos.len() + STARDUST_LIGHT_HALO_BUDGET;
        // The aurora first: it is the sky's ground, and on light the stars'
        // ink lays composite OVER it — and on dark the stars are PRICED over
        // it ([`sky_ground`]), so the slice the sky lays this frame is kept.
        let out_from = frame.out.len();
        let sky_from = frame.halos.len();
        self.draw_veil(ctx, frame.halos, halo_cap);
        let mut haloed = 0usize;
        let n = self.stars.len();
        // `i` counts every star handed a turn, drawn or culled, so the fair
        // share is what is left over the stars still to come.
        let mut i = 0usize;
        for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
            for star in self.stars.iter().filter(|s| s.class == class) {
                let turn = i;
                i += 1;
                let Some(paint) = Paint::of(star, &self.probe, ctx) else {
                    continue;
                };
                let left = quad_cap.saturating_sub(frame.out.len());
                if left == 0 {
                    return;
                }
                let share = (left / (n - turn)).max(M3_QUADS).min(left);
                let star_cap = frame.out.len() + share;
                if ctx.cfg.dark_theme {
                    paint.draw_add(ctx, frame, star_cap, &mut haloed, (out_from, sky_from));
                } else {
                    paint.draw_over(ctx.geom, frame, halo_cap);
                }
            }
        }
    }

    /// Stars on glass — `trail status`'s `v2_stars=` row (seam point 12).
    /// Finishing stars are still on glass, so they count; ≤ [`STAR_CAP`] of
    /// them are live (not finishing), and the pool never exceeds
    /// [`STAR_POOL_MAX`].
    #[must_use]
    pub fn live(&self) -> usize {
        self.stars.len()
    }

    /// True when the sky is empty — one of the three pools
    /// `Engine::next_change_deadline` folds (§18).
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.stars.is_empty() && self.veil.is_empty()
    }

    /// **THE CADENCE LAW, brisk half** — whether any star MOVES on every
    /// frame ([`Star::brisk`]). Under reduced motion every mark is pinned
    /// (§6.11), so never.
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        !self.reduced
            && (self.stars.iter().any(|s| s.brisk(now))
                || self.veil.iter().any(|v| v.age_s(now) < EDGE_IN_S))
    }

    /// **THE CADENCE LAW** — the next instant a star changes what is on
    /// glass. `None` when the sky is empty (T6).
    ///
    /// A star's life, read as offers to the [`Cadence`] fold — a throw, a
    /// drag or a mend's pull ([`MEND_PULL_MS`]) is BRISK (the next frame);
    /// everything else a star does is a
    /// TAIL, floored at the [`super::TAIL_FLOOR`] so a sky of many stars
    /// cannot interleave its steps into a finer cadence than the floor:
    ///
    /// * its HOLD — drawn at full and static, so its one change is the
    ///   hold's END, when the two scintillation clocks take over;
    /// * past its hold — the next integer arm step
    ///   ([`Star::next_arm_step_s`], ≤ 62.5 ms away). The luminance clock
    ///   changes the core's u8 by a level far more often, and that is
    ///   exactly the "present dedup hides the rest" class: the star is
    ///   sampled at 16-22 Hz while only tails are live, and at the frame
    ///   rate whenever anything is brisk;
    /// * a gold star's GLINTS switching on or off on the luminance clock
    ///   ([`Star::next_glint_toggle_s`]) — an 89 ms window on a ≤ 62 ms
    ///   cadence is never missed, only met up to a slot late;
    /// * a sky star's one-pixel LIFT ([`Star::next_lift_step_s`]);
    /// * its DEATH — the class life, or the instant a [`Finish`] alone
    ///   culls it — up to a slot late, which leaves the last frame drawn
    ///   (at the cull, D3) on glass that much longer and draws nothing
    ///   dimmer; a finish in progress is a `spend` fade;
    /// * under reduced motion: static until the theme's one linear fade
    ///   opens (`life − 120 ms`), then the floor.
    ///
    /// The sky once answered `now` for every live star, which had the host
    /// re-arm on the turn it asked — a spin.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
        }
        // The veil's clocks are the field star's: its hold's end, its fade's
        // end, its finish's cull, its horizon — and under reduced motion the
        // one linear fade's opening. Between those the arm clocks of the
        // stars over it already sample the sky; a veil alone is a tail.
        for v in &self.veil {
            let idle = v.idle_s(now);
            let age = v.age_s(now);
            let fade_life = field_fade_life_s();
            if let Some(f) = v.finish {
                cad.tail_at(f.cull_at());
                cad.tail(0.0);
            }
            if self.reduced {
                let fade_from = fade_life - REDUCED_MOTION_FADE_MS / 1000.0;
                cad.tail((fade_from - idle).max(0.0));
                cad.tail((FIELD_LIFE_S - REDUCED_MOTION_FADE_MS / 1000.0 - age).max(0.0));
            } else {
                if idle < FIELD_HOLD_MS / 1000.0 {
                    cad.tail(FIELD_HOLD_MS / 1000.0 - idle);
                } else {
                    cad.tail(0.0);
                }
                if age < EDGE_IN_S {
                    cad.tail(EDGE_IN_S - age);
                } else if age >= FIELD_LIFE_S * (1.0 - EXPIRY_MELT_SHARE) {
                    // The horizon's melt is moving under a hand that is
                    // still on the row.
                    cad.tail(0.0);
                }
            }
            cad.tail(fade_life - idle);
            cad.tail(FIELD_LIFE_S - age);
        }
        for s in &self.stars {
            let age = s.age_s(now);
            let life = s.life_s();
            cad.tail(life - age);
            if let Some(f) = s.finish {
                cad.tail_at(f.cull_at());
                cad.tail(0.0);
            }
            if s.lane == StarLane::Field {
                // The field fade from the row's last key ([`Star::lit`]):
                // its hold's END is where the alpha starts moving, its cull
                // is a disappearance, and under reduced motion the one
                // linear fade's opening. In between, the arm clock below
                // already samples the star at 16-22 Hz.
                let idle = s.idle_s(now);
                let fade_life = field_fade_life_s();
                if self.reduced {
                    let fade_from = fade_life - REDUCED_MOTION_FADE_MS / 1000.0;
                    cad.tail((fade_from - idle).max(0.0));
                } else if idle < FIELD_HOLD_MS / 1000.0 {
                    cad.tail(FIELD_HOLD_MS / 1000.0 - idle);
                }
                cad.tail(fade_life - idle);
            }
            if self.reduced {
                let fade_from = life - REDUCED_MOTION_FADE_MS / 1000.0;
                if age < fade_from {
                    cad.tail(fade_from - age);
                } else {
                    cad.tail(0.0);
                }
                continue;
            }
            let (hold, _) = s.class.envelope_s(s.lane);
            if age < hold {
                cad.tail(hold - age);
            } else if let Some(step) = s.next_arm_step_s(self.cell_h, now) {
                cad.tail(step);
            }
            if let Some(dt) = s.next_glint_toggle_s(now) {
                cad.tail(dt);
            }
            if let Some(dt) = s.next_lift_step_s(now) {
                cad.tail(dt);
            }
            // The slipstream's integer steps, solved like the lift's and
            // offered as tails: the sky streams at ≤ 22 Hz plus one step,
            // never at frame rate (1d).
            if let Some(dt) = s.next_stream_step_s(now) {
                cad.tail(dt);
            }
        }
        cad.take()
    }

    /// Drop the sky — style switch, layout change, `Engine::reset`. The bucket
    /// is refilled, not carried: a reset is a new session, and the first
    /// capital after it should chime.
    pub fn reset(&mut self) {
        self.stars.clear();
        self.veil.clear();
        self.budget = StarBudget::new();
        self.last_hero = None;
        self.probe.clear();
        self.glints.clear();
        self.minted = 0;
        self.last_typed = None;
        self.ioi_ms = STREAM_IOI_MAX_MS;
    }

    /// **SEAM POINT 12** (`translate_scroll_state`), the sky's half: move every
    /// star with the viewport by `rows` cell heights, drop the ones that leave
    /// the grid rather than clamping them to row 0 (light pinned to a line
    /// nobody typed), and move the hero memory by the same rows. The probe is
    /// NOT translated — the host re-probes the caret's rows before the next
    /// deal, and a stale row answered as blank would be a star over ink.
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if rows == 0 {
            return;
        }
        let dy = f32::from(rows) * f32::from(cell_h);
        self.stars.retain_mut(|s| {
            if let Some(row) = s.home_row {
                let Some(row) = row.checked_sub(rows) else {
                    return false;
                };
                s.home_row = Some(row);
            }
            s.y -= dy;
            s.y >= 0.0
        });
        // The veil moves with its row and leaves with it.
        for v in &mut self.veil {
            v.row = v.row.checked_sub(rows).unwrap_or(u16::MAX);
        }
        self.veil.retain(|v| v.row != u16::MAX);
        self.last_hero = self
            .last_hero
            .and_then(|(r, c)| r.checked_sub(rows).map(|r| (r, c)));
        self.probe.clear();
    }

    /// Move sky stars with the glyph rows that earned them. Under tall ribbon
    /// geometry a row's sky is in the previous pixel row, including the band
    /// above row zero; pixel membership alone would move a fixed footer's stars
    /// and leave the moving top row's stars behind. Free transients use
    /// [`BandPx`]'s point law. Veils and hero-spacing memory follow grid rows.
    /// The probe is cleared and re-read by the host before the next deal.
    /// Without cell height, stars fail closed while row ownership still moves.
    pub fn translate_band(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
        cell_h: u16,
        origin_y: u16,
    ) {
        if delta == 0 || top > bottom {
            return;
        }
        if cell_h == 0 {
            self.stars.clear();
        } else {
            let px = BandPx::new(top, bottom, delta, cell_h, origin_y);
            self.stars.retain_mut(|s| {
                if let Some(home) = s.home_row {
                    let Some(row) = band_row(home, top, bottom, delta) else {
                        return false;
                    };
                    s.y += (i32::from(row) - i32::from(home)) as f32 * f32::from(cell_h);
                    s.home_row = Some(row);
                    true
                } else {
                    px.point(&mut s.y)
                }
            });
        }
        self.veil
            .retain_mut(|v| match band_row(v.row, top, bottom, delta) {
                Some(row) => {
                    v.row = row;
                    true
                }
                None => false,
            });
        self.last_hero = self
            .last_hero
            .and_then(|(r, c)| band_row(r, top, bottom, delta).map(|r| (r, c)));
        self.probe.clear();
    }

    // -- the deal ----------------------------------------------------------

    /// §5.6's STRIKE and FIELD populations, dealt over the cells this echo
    /// laid.
    ///
    /// **The strike deal is ONE draw per cell, bucketed** — `u < 1/12` m1,
    /// then `1/4` m2, then `1/2` m3, else none — which keeps every published
    /// rate exactly and makes "a typed cell carries at most one strike star"
    /// true by construction; three independent draws put an m2 and an m3 on
    /// one cell one time in eight, and two stars on one cell composite past
    /// the sparkle law's ceiling. An earned hero (§5.8) takes the cell's one
    /// slot outright. The second grain at `disp ≥ 0.5` is born on the NEXT
    /// cell (the caret's leading edge), priced on the honest spine — a resume
    /// must not buy a denser sky it did not earn (`spine.rs`).
    ///
    /// **ONE SKY STAR PER CELL** ([`STRIKE_REACH_CELLS`]). Every sky-lane
    /// deal asks [`Stardust::sky_star_live`] first — the field's own guard,
    /// generalised to the strike and the second grain — and a cell that has
    /// a star keeps it:
    ///
    /// * the **strike** moves to the first free cell within its reach (the
    ///   lead cell) or is dropped — under a hot hand the typed cell is
    ///   always taken by the previous key's leading-edge grain, so the
    ///   partition's classes (the heroes, the m2s) land one cell ahead
    ///   rather than not at all;
    /// * the **second grain** is dropped when the lead cell is taken;
    /// * the **field** deal ADOPTS the cell's star into the field lane
    ///   (`lane ← Field`, `lit ← at`, pinned) instead of stacking a second:
    ///   the cell's star lives the cell's life from this key on, re-lit and
    ///   finished with its cell like any field star. That is what keeps the
    ///   hot sky DENSE ACROSS THE LINE — every laid cell carries one
    ///   long-lived star of graded magnitude — where refusing the field
    ///   would have left every cell a 225–460 ms wink (3–4 live at 12 cps
    ///   where the pile had 22). A brighten with a keystroke behind it: the
    ///   key that laid the cell;
    /// * the key that **mends** a typo run ([`Ctx::mend`]) takes its strike
    ///   as the PULLED m2 of 1e — born over the erased cell, grabbing the
    ///   star there, home in the caret's cell ([`MEND_PULL_MS`]) — so the
    ///   erased cell is free for the fix's own field star and the caret's
    ///   is taken for the leading grain.
    fn deal_typed(
        &mut self,
        cells: u16,
        class: TypedClass,
        at: Instant,
        ctx: &Ctx<'_>,
        ribbon: &Ribbon,
    ) {
        // THE AURORA rides the RIBBON, and the ribbon lays on a Space too:
        // every laid cell gets its veil before the star deal asks what the
        // key was.
        self.deal_veil(cells, at, ctx, ribbon);
        // Space lays ribbon but deals no star (`TypedClass::Space`, §8.2).
        if class == TypedClass::Space {
            return;
        }
        let scale = if ctx.cfg.dark_theme {
            1.0
        } else {
            LIGHT_COUNT_SCALE
        };
        // A hot hand deals every cell a field star (`FIELD_DEAL_IN_HOT`), on
        // the honest spine, like the second grain — and an OPEN THEME holds
        // that share through the dips, lerped over flow's heat so nothing
        // snaps and `heat == 0` is the step's own answer to the bit
        // (`FIELD_SHARE_FLOW`).
        let field_step = if ctx.disp >= FIELD_HOT_DISP {
            FIELD_SHARE_FLOW
        } else {
            FIELD_SHARE_COLD
        };
        let field_share = field_step
            .max(FIELD_SHARE_COLD + (FIELD_SHARE_FLOW - FIELD_SHARE_COLD) * clamp01(ctx.flow.heat));
        let m1 = scale / DEAL_M1_IN as f32;
        let m2 = m1 + scale / DEAL_M2_IN as f32;
        let m3 = m2 + scale / DEAL_M3_IN as f32;
        // §5.8: a shifted capital and `!` EARN their hero; the 1-in-12 is
        // the dealt one, and only the dealt one degrades for want of a
        // token (D5).
        let earned = matches!(class, TypedClass::Capital | TypedClass::Bang);
        let (row, head) = ctx.caret;
        for k in 0..cells.max(1) {
            let col = head.saturating_sub(cells.max(1) - k);
            let cell = (row, col);
            let h = cell_hash(row, col, 0);

            // -- the mend (1e; §23's addendum "The mend") --------------------
            // The key that mends a typo run takes its strike as a PULLED
            // star: born over the erased cell — the very star the Backspace
            // was retiring, if one is live — and drawn one cell into the
            // caret's over `MEND_PULL_MS`, then pinned there, where the
            // leading grain finds its cell taken and the next key's field
            // deal adopts it. An m2 by rule; an earned hero keeps its
            // magnitude (§5.8: light is not rationed). Refused — the erased
            // cell's sky inked, the caret's cell off the glass or taken to
            // the pixel — the key is dealt as any other.
            let mended = match ctx.mend {
                Some(m) if (m.row, m.col) == cell => self.deal_star(
                    Deal {
                        class: if earned { StarClass::M1 } else { StarClass::M2 },
                        lane: StarLane::Strike,
                        cell: (row, col.saturating_add(1)),
                        earned,
                        pull: Some(Pull {
                            from: cell,
                            disp: m.disp,
                        }),
                        tint: None,
                    },
                    at,
                    ctx,
                    ribbon,
                ),
                _ => false,
            };

            // -- strike -----------------------------------------------------
            let u = hash01(mix32(h ^ SALT_STRIKE));
            let dealt = if mended {
                None
            } else if earned || u < m1 {
                Some(StarClass::M1)
            } else if u < m2 {
                Some(StarClass::M2)
            } else if u < m3 {
                Some(StarClass::M3)
            } else {
                None
            };
            // **THE COMBO LADDER** — flow's payout, on the key's OWN cell (the
            // last of a coalesced echo, which is the cell the head sits on).
            // It rewrites the strike this key was going to take rather than
            // adding a star beside it: one key, one strike, and the ladder's
            // rung is what that strike IS.
            let rung = (k + 1 == cells.max(1) && !mended)
                .then(|| ladder_rung(ctx.flow.combo))
                .flatten();
            let (dealt, earned, tint) = match rung {
                Some(Rung::Promote) => (
                    Some(dealt.map_or(StarClass::M2, |c| c.min(StarClass::M2))),
                    earned,
                    None,
                ),
                Some(Rung::White) => (
                    Some(dealt.map_or(StarClass::M2, |c| c.min(StarClass::M2))),
                    earned,
                    Some((TINT_WHITE_RGB, false)),
                ),
                Some(Rung::Gold) => (Some(StarClass::M1), true, Some((TINT_GOLD_RGB, true))),
                None => (dealt, earned, None),
            };
            if let Some(class) = dealt
                && let Some(free) = self.free_sky_cell(cell, ctx.geom)
            {
                self.deal_star(
                    Deal {
                        class,
                        lane: StarLane::Strike,
                        cell: free,
                        earned,
                        pull: None,
                        tint,
                    },
                    at,
                    ctx,
                    ribbon,
                );
            }
            // A second grain from the caret's leading edge when the spine is
            // up: fast typing makes the sky denser, on the HONEST metric —
            // and only onto a free cell.
            let lead = (row, col.saturating_add(1));
            if ctx.disp >= STRIKE_SECOND_M3_DISP
                && deal(h, SALT_SECOND_M3, 1, scale)
                && !self.sky_star_live(lead, ctx.geom)
            {
                self.deal_star(
                    Deal {
                        class: StarClass::M3,
                        lane: StarLane::Strike,
                        cell: lead,
                        earned: false,
                        pull: None,
                        tint: None,
                    },
                    at,
                    ctx,
                    ribbon,
                );
            }

            // -- field ------------------------------------------------------
            // Stable per cell, so the sky does not shimmer as the ribbon
            // re-plans. A cell that already carries a sky star keeps it —
            // ADOPTED into the field lane, so it lives the cell's life —
            // and never a second birth on it. The mend's cell gives its
            // light to the caret's (1e): ONE light moves, and a fresh field
            // star born beside the pull's path would read as a copy left
            // behind and lay its halo on the pulled centre in transit
            // (measured: Y 85 over the caret law's 72 at +55 ms); the cell
            // keeps its veil.
            if mended {
                continue;
            }
            if deal_p(h, SALT_FIELD, scale * field_share) {
                if self.adopt_field(cell, at, ctx.geom) {
                    continue;
                }
                let fc = if deal(h, SALT_FIELD_M2, FIELD_M2_IN, 1.0) {
                    StarClass::M2
                } else {
                    StarClass::M3
                };
                self.deal_star(
                    Deal {
                        class: fc,
                        lane: StarLane::Field,
                        cell,
                        earned: false,
                        pull: None,
                        tint: None,
                    },
                    at,
                    ctx,
                    ribbon,
                );
            }
        }
    }

    /// §5.6's ERASE population for ONE Backspace: 3 m3 + 1 m2 thrown up and
    /// away from the erased cell (the caret's), transient-priced. **Never
    /// chimes** — Backspace is unpitched, ruled twice (§13, §19.2), which is
    /// why nothing here touches the bucket even for an m1 (and no m1 is
    /// dealt). On a light theme the grains are dealt at [`LIGHT_COUNT_SCALE`];
    /// the m2 is fixed.
    fn deal_backspace(&mut self, at: Instant, ctx: &Ctx<'_>) {
        let (row, col) = ctx.caret;
        for k in 0..(ERASE_M3_N + ERASE_M2_N) {
            let class = if k < ERASE_M3_N {
                StarClass::M3
            } else {
                StarClass::M2
            };
            self.throw_erase(class, (row, col), k, at, ctx);
        }
    }

    /// §5.6's ERASE population for a kill: **one star per 3 cells, cap 8**
    /// (§8.2), spread along the drained span — star `k` at the centre of the
    /// `k`-th group of three from the caret, in the direction the ribbon
    /// drains (`retract_suffix`: from the caret to the row's far end) — three
    /// grains to every m2. A clause going is one gesture, not a shower.
    fn deal_kill(&mut self, cells: u16, at: Instant, ctx: &Ctx<'_>) {
        let n = (cells / KILL_CELLS_PER_STAR).clamp(1, KILL_STAR_MAX);
        let (row, col) = ctx.caret;
        let last = ctx.geom.cols.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
        for k in 0..n {
            let class = if k % (ERASE_M3_N + ERASE_M2_N) == ERASE_M3_N {
                StarClass::M2
            } else {
                StarClass::M3
            };
            let at_col = col
                .saturating_add(k * KILL_CELLS_PER_STAR + KILL_CELLS_PER_STAR / 2)
                .min(last);
            self.throw_erase(class, (row, at_col), k, at, ctx);
        }
    }

    /// ONE erase-lane throw from `cell`: a radial throw of 2-4 px over
    /// [`ERASE_THROW_MS`] ease-out, UP AND AWAY (the angle spans the upper
    /// half-plane, so a throw never dives into the line the caret is still
    /// on), then pinned and twinkling. The seed folds the mint counter in
    /// (§18: `(row, col, born)`), so two Backspaces at one column throw
    /// different debris.
    fn throw_erase(
        &mut self,
        class: StarClass,
        cell: (u16, u16),
        k: u16,
        at: Instant,
        ctx: &Ctx<'_>,
    ) {
        let (row, col) = cell;
        let seed = mix32(self.mint_seed(row, col) ^ SALT_ERASE ^ u32::from(k));
        if class == StarClass::M3 && !light_keeps(seed, ctx.cfg) {
            return;
        }
        let (cx, cy) = ctx.geom.cell_center(row, col);
        let a = std::f32::consts::PI * (1.0 + hash01(seed));
        let reach = (ERASE_REACH_MIN_PX
            + (ERASE_REACH_MAX_PX - ERASE_REACH_MIN_PX) * hash01(mix32(seed ^ 7)))
            * px_scale(ctx.geom.ch as f32);
        let v0 = (
            a.cos() * reach / ERASE_THROW_MS,
            a.sin() * reach / ERASE_THROW_MS,
        );
        let (tint, gold) = deal_tint(seed, ctx.caret_t);
        self.throw(
            Throw {
                class,
                lane: StarLane::Erase,
                at_px: (cx, cy),
                v0,
                tint,
                gold,
                seed,
                // §5.6 promises no pairing for the erase: nudged or dropped.
                fixed: false,
            },
            at,
            ctx,
        );
    }

    /// Deal ONE sky star at `spec.cell`, subject to §5.4's zone and its hard
    /// probe gate, §5.2's hero spacing, and D5's budget interaction.
    ///
    /// The refusals are ordered so that only the MAGNITUDE is ever rationed
    /// and never the light: a DEALT hero too close to a live one, or the
    /// third live on its row, is born as an m2; a dealt hero with no token is
    /// born as an m2 (shown, silent); an EARNED hero is an m1 whatever the
    /// spacing and the bucket say, and simply says nothing without a token
    /// (§5.8, D5). Only §5.4's gates refuse a star outright — the probe,
    /// because a core on a stroke is the one thing L4 does not trade away,
    /// and the effects box, because a star clipped off the glass must not
    /// spend a token or a hero slot on a chime nobody can see the star of.
    ///
    /// **A mend's star** ([`Deal::pull`], 1e) is dealt at its HOME — the
    /// caret's cell, through every gate above — and born over the erased
    /// cell: the live sky star there, if one is, is GRABBED (re-minted in
    /// place as the pulled star, its seed kept so the tint stays the cell's
    /// deal and the finish the erase put it on pardoned), else the star is
    /// born one cell left of home. Its lift is zero — pinned once home — and
    /// its slipstream is gated on the momentum the fix resumes, clamped from
    /// where it is born. Returns whether a star was dealt.
    fn deal_star(&mut self, spec: Deal, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) -> bool {
        // The grab: `Some(slot)` names the star over the erased cell, if any.
        let mut grabbed: Option<Option<usize>> = None;
        if let Some(p) = spec.pull {
            let Some(g) = self.pull_origin(p.from, at, ctx) else {
                return false;
            };
            grabbed = Some(g);
        }
        let seed = match grabbed {
            Some(Some(i)) => self.stars[i].seed,
            _ => self.mint_seed(spec.cell.0, spec.cell.1),
        };
        let Some((x, y)) = self.sky_birth_px(spec.cell, seed, ctx, ribbon) else {
            return false;
        };
        if grabbed.is_some() {
            // THE CARET'S CELL IS THE MEND'S STAR'S: a live sky star already
            // there — the typo key's own strike, still lifting — goes on the
            // cap's finish ([`EVICT_FINISH_MS`]) as the pulled star sets out,
            // gone before it arrives. One star per cell, at home too.
            let g = ctx.geom;
            for (i, s) in self.stars.iter_mut().enumerate() {
                if grabbed != Some(Some(i)) && s.lane.is_sky() && star_over_cell(s, spec.cell, g) {
                    s.finish_by(at, EVICT_FINISH_MS / 1000.0);
                }
            }
        }
        let mut class = spec.class;
        if class == StarClass::M1 && !spec.earned && !self.hero_allowed(spec.cell, y, ctx.geom) {
            class = StarClass::M2;
        }
        let mut glint = false;
        if class == StarClass::M1 && spec.lane.may_chime() {
            if self.budget.try_spend() {
                glint = true;
            } else if !spec.earned {
                class = StarClass::M2;
            }
        }
        let (tint, gold) = spec
            .tint
            .unwrap_or_else(|| class_tint(class, seed, self.field_t(spec.cell, ctx, ribbon)));
        let v0 = match (class, spec.lane) {
            // A mend's star is pinned once home (1e: "then pinned").
            _ if spec.pull.is_some() => (0.0, 0.0),
            // §5.6: a strike m1/m2 lifts; a grain and every FIELD star are
            // pinned. The zero `v0` is what expresses "pinned" — there is no
            // fourth motion law for it.
            (StarClass::M1 | StarClass::M2, StarLane::Strike) => {
                (0.0, sky_lift_px_per_s(ctx.geom.ch as f32) / 1000.0)
            }
            _ => (0.0, 0.0),
        };
        // THE MEND'S PULL (1e): from the grabbed star's own pixel — the
        // light does not jump to be pulled — or one cell left of home.
        let from_x = match grabbed {
            Some(Some(i)) => self.stars[i].pos(at, self.reduced).0 as f32,
            Some(None) => x - ctx.geom.cw as f32,
            None => x,
        };
        let pull = (x - from_x).max(0.0);
        // THE SLIPSTREAM (1d): a star born at speed inherits the hand's own
        // backward drift — every sky lane, the field's included — priced by
        // the observed key interval (T4) and clamped at the blank frontier
        // (L4); only under a hand the spine calls hot — for a mend's star,
        // the momentum the fix resumes, and from where it is born.
        let birth_disp = spec
            .pull
            .map_or(ctx.birth_disp, |p| ctx.birth_disp.max(p.disp));
        let stream = if birth_disp >= STREAM_DISP {
            self.stream_reach(from_x, spec.cell.0, ctx)
        } else {
            0.0
        };
        if class == StarClass::M1 {
            self.last_hero = Some(spec.cell);
        }
        if glint {
            // ONE token = ONE hero = ONE glint, same column, same frame (§13).
            self.glints.push(Glint {
                col: spec.cell.1,
                f: scint_rate(seed),
            });
        }
        let star = Star {
            class,
            lane: spec.lane,
            x,
            y,
            home_row: Some(spec.cell.0),
            born: at,
            lit: at,
            seed,
            tint,
            gold,
            v0,
            f: scint_rate(seed),
            stream,
            pull,
            finish: None,
        };
        match grabbed {
            // The grab re-mints in place: one star, one slot, no churn.
            Some(Some(i)) => self.stars[i] = star,
            _ => self.sow(star),
        }
        true
    }

    /// Throw ONE free transient (§5.4: a free transient may be thrown
    /// anywhere, but a **core** never LANDS on a probed glyph). The birth
    /// pixel is exactly where the caller said — the landing cell's centre,
    /// the shed station — and the clearance is on where the star comes to
    /// rest ([`Stardust::settle`]).
    fn throw(&mut self, spec: Throw, at: Instant, ctx: &Ctx<'_>) {
        let mut star = Star {
            class: spec.class,
            lane: spec.lane,
            x: spec.at_px.0.round(),
            y: spec.at_px.1.round(),
            home_row: None,
            born: at,
            lit: at,
            seed: spec.seed,
            tint: spec.tint,
            gold: spec.gold,
            v0: spec.v0,
            f: scint_rate(spec.seed),
            // A transient is thrown, not dealt to a cell: no slipstream, no
            // pull.
            stream: 0.0,
            pull: 0.0,
            finish: None,
        };
        if self.settle(&mut star, spec.fixed, ctx) {
            self.sow(star);
        }
    }

    // -- birth zones (§5.4) ------------------------------------------------

    /// §5.4's SKY BAND for `cell`, or `None` where no star may be born.
    ///
    /// Four gates, in the order they can refuse:
    ///
    /// 1. the **probe**: `row − 1` must read `Some(false)`. `Some(true)` or
    ///    `None` and **no star is born** — there is no in-cell fallback and no
    ///    descender-gap fallback (L4, D16). Row 0 answers `Some(false)`
    ///    because the band above the grid is `Geom.head`, which holds no
    ///    glyphs;
    /// 2. the **zone**: `y ∈ [top − 0.30 ch, top − 0.04 ch]`, where `top` is
    ///    the cell's own ribbon band top — the reason §5.4 is stated against
    ///    the ribbon's top edge and not in cell coordinates (D16). With no
    ///    ribbon laid the spelling's own anchor stands in. The jitter inside
    ///    the zone is the STAR's seed, not the cell's: two stars dealt on one
    ///    cell must not be born on one pixel;
    /// 3. the **glass**: the pixel must lie inside the effects box
    ///    (`[fx_top, fx_bot)`), or the star would be clipped by `push_fx_rect`
    ///    while still spending a token and a hero slot — row 0 under a host
    ///    with no head band is the case;
    /// 4. the **pixel**: the centre is rounded to a device pixel at birth, so
    ///    every later step of it is an integer step — and if a live star was
    ///    born on that very pixel the birth is nudged to a clear neighbour
    ///    inside the zone, or refused, so two sky stars never stack
    ///    (`no_two_sky_stars_share_a_pixel`).
    fn sky_birth_px(
        &self,
        cell: (u16, u16),
        seed: u32,
        ctx: &Ctx<'_>,
        ribbon: &Ribbon,
    ) -> Option<(f32, f32)> {
        let (row, col) = cell;
        if usize::from(row) >= ctx.geom.rows || usize::from(col) >= ctx.geom.cols {
            return None;
        }
        // §5.4's requirement — `probed_cell_glyph(row − 1) == Some(false)` —
        // asked in its SKY form: a blank, an off-grid band and a light rule
        // (whose stroke the zone never reaches, `RULE_INK_BOT_CH`) all say
        // clear; a glyph says ink; an unprobed row says unknown. No star on
        // anything but clear.
        if self.probe.sky_at(i32::from(row) - 1, col, ctx.geom.rows) != Some(false) {
            return None;
        }
        let g = ctx.geom;
        let ch = g.ch as f32;
        let top = ribbon
            .band(row, col)
            .map_or_else(|| default_band_top(g, ctx.cfg, row), |b| b.top);
        let j = mix32(seed ^ SALT_JITTER);
        let y = top - ch * (SKY_BOTTOM_CH + (SKY_TOP_CH - SKY_BOTTOM_CH) * hash01(j));
        let (cx, _) = g.cell_center(row, col);
        let jx = (hash01(mix32(j ^ 11)) * 2.0 - 1.0) * SKY_JITTER_CW * g.cw as f32;
        let (x, y) = ((cx + jx).round(), y.round());
        // The zone's own box, for the nudge to stay inside.
        let x_lo = (cx - SKY_JITTER_CW * g.cw as f32).round();
        let x_hi = (cx + SKY_JITTER_CW * g.cw as f32).round();
        let y_lo = (top - SKY_TOP_CH * ch).round();
        let y_hi = (top - SKY_BOTTOM_CH * ch).round();
        let glass = |x: f32, y: f32| on_glass((x as i32, y as i32), g);
        if !glass(x, y) {
            return None;
        }
        if !self.pixel_taken(x, y) {
            return Some((x, y));
        }
        const NUDGE: [(f32, f32); 8] = [
            (0.0, -1.0),
            (0.0, 1.0),
            (-1.0, 0.0),
            (1.0, 0.0),
            (-1.0, -1.0),
            (1.0, -1.0),
            (-1.0, 1.0),
            (1.0, 1.0),
        ];
        NUDGE
            .iter()
            .map(|&(dx, dy)| (x + dx, y + dy))
            .find(|&(nx, ny)| {
                (x_lo..=x_hi).contains(&nx)
                    && (y_lo..=y_hi).contains(&ny)
                    && glass(nx, ny)
                    && !self.pixel_taken(nx, ny)
            })
    }

    /// Is a live star's BIRTH pixel exactly here? Asked of the birth pixel
    /// and not the current position, because a lifted neighbour one pixel up
    /// is a neighbour, not a stack.
    fn pixel_taken(&self, x: f32, y: f32) -> bool {
        self.stars.iter().any(|s| s.x == x && s.y == y)
    }

    /// **THE OBSERVED CADENCE** (T4): the interval between this Typed key
    /// and the last, clamped to `[STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS]` —
    /// the one number the slipstream is priced from, read off the typed
    /// events' own instants and never off the spine. A second event on the
    /// same instant (a coalesced burst delivered as two) keeps the interval
    /// already read: it is one keystroke's worth.
    ///
    /// A COALESCED ECHO IS PRICED PER KEY: a `Typed { cells: n }` is one
    /// event carrying `n` keystrokes the host delivered in one frame (a
    /// Codex burst, Claude Code's Sweep), so the gap since the last event
    /// is `n` key intervals, not one — it is divided by `cells` before the
    /// clamp. A 1-cell event is priced exactly as before (the divisor is
    /// 1). Without the division a 5-key burst 400 ms after the last key
    /// reads as a 2.5 cps stroll and the sky stands still under a hand
    /// that just typed at 12 cps. Event-edge work.
    fn note_cadence(&mut self, at: Instant, cells: u16) {
        if let Some(last) = self.last_typed {
            let gap_ms =
                at.saturating_duration_since(last).as_secs_f32() * 1000.0 / f32::from(cells.max(1));
            if gap_ms > 0.0 {
                self.ioi_ms = gap_ms.clamp(STREAM_IOI_MIN_MS, STREAM_IOI_MAX_MS);
            }
        }
        self.last_typed = Some(at);
    }

    /// **THE SLIPSTREAM'S TRAVEL FOR ONE BIRTH** (px ≥ 0): the priced travel
    /// ([`stream_travel_px`] of [`stream_px_per_s`] at this cell width and
    /// the observed interval), CLAMPED AT THE BLANK FRONTIER — L4 on a
    /// moving star: the centre may drift left only over cells of `row − 1`
    /// the probe proved blank, contiguously from the star's own, and never
    /// past the pane's first column. The frontier is read ONCE, here, at
    /// the birth edge — the probe forgets rows (an Enter, a scroll) and a
    /// later frame must not re-ask it; a scroll drops the star anyway.
    /// Event-edge work: a walk of at most one row's columns per birth,
    /// nothing per frame.
    fn stream_reach(&self, x: f32, row: u16, ctx: &Ctx<'_>) -> f32 {
        let want = stream_travel_px(stream_px_per_s(ctx.geom.cw as f32, self.ioi_ms));
        self.blank_frontier_px(x, row, want, ctx)
    }

    /// **THE BLANK FRONTIER** — how far, px ≥ 0, a thing at `x` on `row`
    /// may travel LEFT of the `want` it was priced: clamped at the first
    /// column of `row − 1` the probe has not proved blank (contiguously from
    /// the thing's own column) and at the pane's first column. The one
    /// clamp [`Stardust::stream_reach`] (the sky's travel) and the exhaust
    /// (the ribbon's stream at its own share, §27) share, so L4 is one
    /// walk with two callers and not two walks. Zero for a non-finite or
    /// sub-half-pixel `want` and off the grid's left edge. Event-edge work:
    /// at most one row's columns per birth.
    fn blank_frontier_px(&self, x: f32, row: u16, want: f32, ctx: &Ctx<'_>) -> f32 {
        let g = ctx.geom;
        if !want.is_finite() || want < 0.5 {
            return 0.0;
        }
        let col = px_col(x, g);
        if col < 0 {
            return 0.0;
        }
        let sky = i32::from(row) - 1;
        let mut frontier = col;
        while frontier > 0 {
            let left = frontier - 1;
            let blank =
                u16::try_from(left).is_ok_and(|c| self.probe.at(sky, c, g.rows) == Some(false));
            if !blank {
                break;
            }
            frontier = left;
        }
        let floor_x = (g.fx_left() + frontier * g.cw as i32) as f32;
        want.min(x - floor_x).max(0.0)
    }

    /// **WHERE A MEND'S STAR IS PULLED FROM** (1e): the erased cell `from`,
    /// on the grid with its sky proven blank — §5.4's gate, asked of the
    /// origin as it is of the home: a star is not born over a stroke to be
    /// pulled off it — or `None`: no pull, and the key is dealt as any
    /// other. `Some(slot)` names the live sky star over the erased cell, if
    /// one is — LIVE at `at`: a star past its cull is this tick's to drop,
    /// not to grab — the light the Backspace was retiring, which the fix
    /// GRABS rather than leaves beside a second star: one star per cell,
    /// the same law that has the field deal adopt. Event-edge work.
    fn pull_origin(&self, from: (u16, u16), at: Instant, ctx: &Ctx<'_>) -> Option<Option<usize>> {
        let g = ctx.geom;
        if usize::from(from.0) >= g.rows || usize::from(from.1) >= g.cols {
            return None;
        }
        if self.probe.at(i32::from(from.0) - 1, from.1, g.rows) != Some(false) {
            return None;
        }
        Some(
            self.stars
                .iter()
                .position(|s| s.lane.is_sky() && !s.dead(at) && star_over_cell(s, from, g)),
        )
    }

    /// **§5.4's FREE TRANSIENT CLEARANCE**, on where the core comes to
    /// REST. A transient is born wherever it was thrown from and flies over
    /// ink (its coverage there held to a third, §5.2); what L4 forbids is a
    /// core that LANDS on a probed glyph — so the pixel asked of the probe is
    /// [`Star::rest`], the end of the star's own motion. Returns whether the
    /// star may be sown; on `true` its `v0` may have been re-aimed.
    ///
    /// The GLASS comes first: a rest outside the effects box (`Geom::fx_*`
    /// — above `fx_top`, below `fx_bot`, past either side) is pulled onto
    /// the box's nearest pixel ([`clamp_to_glass`]) and the throw re-aimed
    /// to end there, so no transient is ever born off the glass to be clipped
    /// to nothing by `push_fx_rect` while it holds a pool slot; every
    /// candidate below is on the glass too. Then three answers, in order:
    ///
    /// 1. **clear** — the rest is not on a probed glyph: sown as thrown;
    /// 2. **nudged** — the nearest clear pixel **within 2 px** of the rest
    ///    (the four axis neighbours and the four diagonals at 1 px, then the
    ///    four axis neighbours at 2 px — a diagonal at 2 is 2.83 px, outside
    ///    the law), and the throw is re-aimed to end exactly there
    ///    ([`Star::aim`]): the birth pixel stays where the caller put it;
    /// 3. **walked back**, for a LAW-FIXED slot ([`Throw::fixed`] — D6's hero
    ///    and four m2, D17's three) — the rest walks back along its own
    ///    throw, one pixel at a time, to the first clear pixel; a grain is
    ///    **dropped** instead, which is §5.4's own last word.
    ///
    /// **The throw's origin cell is clear by exemption, for a radial throw.**
    /// Every radial throw in this theme leaves a cell the caret owns — the
    /// landing (the caret's own, ledger-exempt: L2, §3.4, §6.5 layer 9, where
    /// the pin's nucleus already stands on whatever glyph is there), a hop's
    /// landing, or a cell the erase just emptied — so a rest inside it is
    /// lawful whatever the probe says of the glyph the caret is on, and the
    /// walk-back always terminates: at worst a hero rests on the caret. A
    /// shed fragment drifts from a station on the path, which is nobody's
    /// cell, and takes no exemption.
    ///
    /// Unknown is not inked: a free transient may land on a row nobody
    /// probed, the asymmetry §5.4 draws between a ribbon cell's sky (where
    /// unknown means no star) and open air. Event-edge work only — a rest is
    /// asked once, at birth; nothing here runs per frame.
    fn settle(&self, star: &mut Star, fixed: bool, ctx: &Ctx<'_>) -> bool {
        const NUDGE: [(i32, i32); 12] = [
            (0, -1),
            (0, 1),
            (-1, 0),
            (1, 0),
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
            (0, -2),
            (0, 2),
            (-2, 0),
            (2, 0),
        ];
        let geom = ctx.geom;
        let origin = star.origin();
        let home = star
            .lane
            .throw_ms()
            .is_some()
            .then(|| (px_row(origin.1 as f32, geom), px_col(origin.0 as f32, geom)));
        let clear = |p: (i32, i32)| {
            on_glass(p, geom)
                && (home == Some((px_row(p.1 as f32, geom), px_col(p.0 as f32, geom)))
                    || self.probe.at_px(p.0, p.1, geom) != Some(true))
        };
        let motion = star.lane.motion();
        let k = motion.k_at(motion.end_ms(star.life_s() * 1000.0));
        // THE GLASS, first: a rest above `fx_top`, below `fx_bot` or past
        // either side is pulled onto the effects box — the ray flattens (or
        // shortens) to end on the box's own edge — before the probe is asked
        // of it. A transient is never born off the glass: `push_fx_rect`
        // would clip it to nothing while it held a pool slot and, for the
        // fan's five, one of the rain's glints. The fan's own rise caps
        // ([`fan_rise`]) keep this from ever biting on a fan; it is the
        // backstop for a pixel of rounding and the law for every other lane.
        let thrown = star.rest();
        let rest = clamp_to_glass(thrown, geom);
        if clear(rest) {
            if rest != thrown {
                star.aim(rest, k);
            }
            return true;
        }
        if let Some(p) = NUDGE
            .iter()
            .map(|&(dx, dy)| (rest.0 + dx, rest.1 + dy))
            .find(|&p| clear(p))
        {
            star.aim(p, k);
            return true;
        }
        if !fixed {
            return false;
        }
        // The walk back: from the rest toward the origin along the throw's
        // own ray, one pixel per step, the origin itself the last station.
        let (dx, dy) = ((rest.0 - origin.0) as f32, (rest.1 - origin.1) as f32);
        let steps = dx.hypot(dy).ceil().max(1.0) as i32;
        let p = (0..steps)
            .rev()
            .map(|i| {
                let t = i as f32 / steps as f32;
                (
                    origin.0 + (dx * t).round() as i32,
                    origin.1 + (dy * t).round() as i32,
                )
            })
            .find(|&p| clear(p))
            .unwrap_or(origin);
        star.aim(p, k);
        true
    }

    /// The FIELD POSITION a star inherits at its birth cell (§5.3's 60 %
    /// share). C2: the ribbon head, the caret and a star's halo on that cell
    /// resolve ONE spectrum position, because all three read this number. A
    /// cell with no ribbon light falls back to the caret's own `t`, which is
    /// where a free star over a dead prompt takes its colour from.
    fn field_t(&self, cell: (u16, u16), ctx: &Ctx<'_>, ribbon: &Ribbon) -> f32 {
        ribbon.field_at(cell.0, cell.1).unwrap_or(ctx.caret_t)
    }

    // -- the cap, the heroes, the finishes, the cull ------------------------

    /// §5.2's hero spacing, read off the LIVE pool: not within
    /// [`HERO_MIN_SPACING_CELLS`] of a live sky hero on the same band, and
    /// never a third live hero on one band. A hero that has died frees its
    /// spacing the moment it leaves the pool — nothing is remembered.
    fn hero_allowed(&self, cell: (u16, u16), y: f32, geom: Geom) -> bool {
        let band = px_row(y, geom);
        let mut on_row = 0usize;
        for s in &self.stars {
            if s.class != StarClass::M1 || !s.lane.is_sky() || px_row(s.y, geom) != band {
                continue;
            }
            on_row += 1;
            let dc = (i32::from(cell.1) - px_col(s.x, geom)).abs();
            if dc < i32::from(HERO_MIN_SPACING_CELLS) {
                return false;
            }
        }
        on_row < usize::from(HERO_MAX_LIVE_PER_ROW)
    }

    /// **Is a SKY star already live on this cell?** — the one-star-per-cell
    /// occupancy every sky-lane deal asks ([`STRIKE_REACH_CELLS`]). Was the
    /// field lane's alone (a re-lay must not stack a second field star on
    /// one pixel); now the strike's and the second grain's too, so no cell
    /// carries a pile. Transients (fan, shed, erase) are thrown, not dealt
    /// to a cell, and do not count.
    fn sky_star_live(&self, cell: (u16, u16), geom: Geom) -> bool {
        self.stars
            .iter()
            .any(|s| s.lane.is_sky() && star_over_cell(s, cell, geom))
    }

    /// The first cell without a live sky star from `cell` toward the
    /// caret's leading edge, within [`STRIKE_REACH_CELLS`] — where a strike
    /// dealt onto a taken cell is born — or `None`: dropped.
    fn free_sky_cell(&self, cell: (u16, u16), geom: Geom) -> Option<(u16, u16)> {
        (0..=STRIKE_REACH_CELLS)
            .map(|k| (cell.0, cell.1.saturating_add(k)))
            .find(|&c| !self.sky_star_live(c, geom))
    }

    /// **THE FIELD DEAL ADOPTS THE STAR A CELL ALREADY HAS** instead of
    /// stacking a second one on it: the star moves to the field lane —
    /// pinned (the zero `v0`), re-lit by this key — and from here lives the
    /// cell's own life ([`field_lane_envelope`]): held while the hand is on
    /// the row, re-lit by every key on it, finished with its cell. Its
    /// class, tint, seed and birth pixel are untouched, so a strike hero
    /// stays a hero and a leading-edge grain stays the cell's stop. A
    /// finish it is on stands. Returns whether a star was adopted; `false`
    /// leaves the deal to birth a field star. In place — nothing allocates.
    fn adopt_field(&mut self, cell: (u16, u16), at: Instant, geom: Geom) -> bool {
        let Some(s) = self
            .stars
            .iter_mut()
            .find(|s| s.lane.is_sky() && star_over_cell(s, cell, geom))
        else {
            return false;
        };
        s.lane = StarLane::Field;
        s.home_row = Some(cell.0);
        s.lit = at;
        s.v0 = (0.0, 0.0);
        true
    }

    /// Put the FIELD stars of the cells the ribbon is retracting — from
    /// `caret` to the row's far end, as `Ribbon::retract_suffix` drains — on
    /// the ribbon's own retract span, so they die with their cells (§5.6).
    fn finish_field(&mut self, caret: (u16, u16), span_ms: f32, at: Instant, geom: Geom) {
        let (row, col) = caret;
        for s in &mut self.stars {
            if s.lane != StarLane::Field {
                continue;
            }
            if px_col(s.x, geom) >= i32::from(col) && in_sky_of_row(s.y, row, geom) {
                s.finish_by(at, span_ms / 1000.0);
            }
        }
        // The veil over the retracted cells goes with them.
        for v in &mut self.veil {
            if v.row == row && v.col >= col {
                v.finish_by(at, span_ms / 1000.0);
            }
        }
    }

    /// **THE HAND IS STILL ON THIS ROW** — every field star in the caret
    /// row's sky is re-lit on the key ([`Star::lit`] ← `at`), so its fade
    /// ([`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`]) runs from the LAST key on its
    /// row, as the ribbon's own grace runs from its cohort's last live cell.
    /// Row-scoped, not global: a row the hand has left (Enter, a wrap) goes
    /// out on its own clock while the new row is typed — its ribbon is
    /// already in its swoosh. Inside the hold the re-stamp changes nothing
    /// on glass (α is 1 either side of it); past it — a slow hand — the
    /// row's sky comes back to full on the key, a brighten WITH a keystroke
    /// behind it, which is what the ribbon's cohort does when a key lands on
    /// it mid-swoosh. A star on a [`Finish`] keeps it: the re-light is a
    /// fade clock, not a pardon for an erased cell. Event path, in place —
    /// nothing here allocates.
    fn relight_field(&mut self, caret: (u16, u16), at: Instant, geom: Geom) {
        for s in &mut self.stars {
            if s.lane == StarLane::Field && in_sky_of_row(s.y, caret.0, geom) {
                s.lit = at;
            }
        }
        for v in &mut self.veil {
            if v.row == caret.0 {
                v.lit = at;
            }
        }
    }

    /// **FOCUS LOST** (§8.2): every star on the 300 ms `spend` ember beside
    /// the ribbon's own — the sky leaves on the same curve as the mark below
    /// it, never as a pop beside a fade.
    fn ember(&mut self, at: Instant) {
        for s in &mut self.stars {
            s.finish_by(at, FOCUS_EMBER_MS / 1000.0);
        }
        for v in &mut self.veil {
            v.finish_by(at, FOCUS_EMBER_MS / 1000.0);
        }
    }

    /// **A NEWBORN IS NEVER REFUSED** (§5.6). At [`STAR_CAP`] live the OLDEST
    /// live star is put on its [`EVICT_FINISH_MS`] finish and the newborn
    /// takes the pool; at [`STAR_POOL_MAX`] on glass the finishing star
    /// nearest the end of its finish is dropped outright, which keeps the pool
    /// a fixed allocation (§18).
    ///
    /// Oldest, not least-remaining-life: a 200 ms-old fan hero with 115 ms
    /// left is younger than a 300 ms-old strike hero with 160 ms left, and it
    /// is the strike hero that has been looked at longest. In practice the
    /// eviction is unreachable below ~30 cps sustained; the cap's own census
    /// (4 m1 + 12 m2 + 24 m3) is the state it describes.
    fn make_room(&mut self, now: Instant) {
        self.cull(now);
        let live = self.stars.iter().filter(|s| s.finish.is_none()).count();
        if live >= STAR_CAP
            && let Some(oldest) = self
                .stars
                .iter_mut()
                .filter(|s| s.finish.is_none())
                .min_by_key(|s| s.born)
        {
            oldest.finish_by(now, EVICT_FINISH_MS / 1000.0);
        }
        if self.stars.len() >= STAR_POOL_MAX {
            let nearest_end = self
                .stars
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.finish.map(|f| (i, f.u(now))))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map_or(0, |(i, _)| i);
            self.stars.remove(nearest_end);
        }
    }

    /// Drop every star past its life or its finish (D3: at
    /// [`STAR_CULL_ALPHA`] the star is still a visible point — it is never
    /// left as a grey speck).
    fn cull(&mut self, now: Instant) {
        self.stars.retain(|s| !s.dead(now));
        self.veil.retain(|v| !v.dead(now));
    }

    /// One fresh seed, hashed from `(row, col, mint)` (§18). The mint counter
    /// stands in for `born`, which has no absolute value to hash: it is a pure
    /// function of the event sequence, so the same events at the same `now`
    /// still produce byte-identical frames.
    fn mint_seed(&mut self, row: u16, col: u16) -> u32 {
        self.minted = self.minted.wrapping_add(1);
        cell_hash(row, col, self.minted)
    }

    // -- the aurora (section 1a′) -------------------------------------------

    /// **LAY THE VEIL** over the cells this echo laid — one [`Veil`] per
    /// cell, on the key (T2: it is on glass the frame that echoes the key,
    /// from the cell's own [`BIRTH_EDGE_FLOOR`]).
    ///
    /// Three gates: the cell is on the grid; the spine at birth is at or
    /// over [`AURORA_COLD_DISP`] (cold → nothing is laid, so an idle sky
    /// keeps nothing invisible in the pool); and `row − 1` probes
    /// `Some(false)` — §5.4's own requirement, because the veil lives in
    /// row − 1's lower band and a veil over a glyph is a mark over a stroke
    /// (L4). Its colour is the ribbon's own field at the cell (C2), folded
    /// as the bed folds it (`tri`), so the veil and the bed under it are one
    /// walk; its gain is priced ONCE from `Ctx::birth_disp`
    /// ([`veil_gain`]) — a re-typed cell is re-laid at the new key's own
    /// spine, with a keystroke behind it. At [`AURORA_CAP`] the cell lit
    /// longest ago gives up its slot; the newborn is never refused.
    fn deal_veil(&mut self, cells: u16, at: Instant, ctx: &Ctx<'_>, ribbon: &Ribbon) {
        let gain = veil_gain(ctx.birth_disp);
        if gain <= 0.0 {
            return;
        }
        let (row, head) = ctx.caret;
        let g = ctx.geom;
        for k in 0..cells.max(1) {
            let col = head.saturating_sub(cells.max(1) - k);
            if usize::from(row) >= g.rows || usize::from(col) >= g.cols {
                continue;
            }
            if self.probe.at(i32::from(row) - 1, col, g.rows) != Some(false) {
                continue;
            }
            let veil = Veil {
                row,
                col,
                t: self.field_t((row, col), ctx, ribbon),
                gain,
                born: at,
                lit: at,
                finish: None,
            };
            if let Some(v) = self.veil.iter_mut().find(|v| v.row == row && v.col == col) {
                *v = veil;
            } else if self.veil.len() < AURORA_CAP {
                self.veil.push(veil);
            } else if let Some(v) = self.veil.iter_mut().min_by_key(|v| v.lit) {
                *v = veil;
            }
        }
    }

    /// **DRAW THE AURORA** — one clipped [`RainHalo`] per live veil cell.
    ///
    /// The ellipse is centred on the box's BOTTOM edge with `ry` the box's
    /// own height, so the falloff is exactly "rising from the line": full
    /// at the line, nothing at the top of the band; `rx` is
    /// [`AURORA_REACH_CW`] so the cell's own edges sit at 0.88 of its
    /// centre. The box is the cell's own column by the sky band's anchor
    /// ([`Veil::bottom_px`]) less [`AURORA_TALL_CH`] — never a pixel of the
    /// glyph row, never a neighbouring column (whose sky the probe may not
    /// have proved), so a veil never splits a row and never sums with its
    /// neighbour: its brightest pixel is its own byte,
    /// `AURORA_COV_PEAK · gain · env · intensity`, at most 40.
    ///
    /// Dark: additive, `spectrum(tri(t))` premultiplied — the bed's own
    /// arc colour, unbudgeted by the bed's luma cap because nothing under
    /// the veil is text. Light: the same veil as source-over ink in the
    /// over-text role (`InkRole::OverText`, §3.3), its alpha the same byte.
    /// Culled below §3.2's chroma floor like every coloured mark.
    fn draw_veil(&self, ctx: &Ctx<'_>, halos: &mut Vec<RainHalo>, light_cap: usize) {
        let g = ctx.geom;
        let ch = g.ch as f32;
        let intensity = clamp01(ctx.cfg.intensity);
        let cap = if ctx.cfg.dark_theme {
            halos.len() + AURORA_HALO_BUDGET
        } else {
            light_cap
        };
        for v in &self.veil {
            let env = v.envelope(ctx.now, ctx.cfg.reduced_motion);
            if env < CHROMA_CULL_ALPHA {
                continue;
            }
            let byte = cov_byte(AURORA_COV_PEAK * v.gain * env * intensity);
            if byte == 0 {
                continue;
            }
            let bottom = v.bottom_px(g, ctx.cfg);
            let top = bottom - AURORA_TALL_CH * ch;
            let (y0, y1) = (top.round() as i32, bottom.round() as i32);
            if y1 <= y0 {
                continue;
            }
            let (cx, _) = g.cell_center(v.row, v.col);
            let x0 = i32::from(g.origin_x) + i32::from(v.col) * g.cw as i32;
            let x1 = x0 + g.cw as i32;
            let rgb = spectrum(clamp01(tri(v.t)));
            let (color, mode) = if ctx.cfg.dark_theme {
                (premul_rgb(rgb, byte), HaloMode::Add)
            } else {
                let a = u32::from(byte).clamp(1, InkRole::OverText.alpha_cap() as u32);
                (
                    (InkRole::OverText.ink(rgb) & 0x00FF_FFFF) | (a << 24),
                    HaloMode::Over,
                )
            };
            push_halo_quads_clipped(
                halos,
                g,
                cap,
                HaloSpec {
                    centre: (cx.round() as i32, y1),
                    radii: ((AURORA_REACH_CW * g.cw as f32).round() as i32, y1 - y0),
                    color,
                    mode,
                },
                (x0, y0, x1, y1),
            );
        }
    }
}

/// The fan's class at throw position `k` (§5.6, D6): **1 gold m1 + 4 m2 +
/// rest m3** in throw order — or, for a landing with no hero (an Enter, D8),
/// **1 m2 + rest m3**: the m2 takes the first slot and nothing takes the
/// hero's four.
fn fan_class(k: usize, hero: bool) -> StarClass {
    match (k, hero) {
        (0, true) => StarClass::M1,
        (0, false) | (1..=4, true) => StarClass::M2,
        _ => StarClass::M3,
    }
}

/// Does `star` sit over `cell` — this column, and in the cell's own row or
/// the one above it? The sky band is in row − 1 under the `tall` spelling
/// and in the cell's own row under `underline` (D16), so the match is "this
/// column, at most one row up" rather than an exact row.
fn star_over_cell(star: &Star, cell: (u16, u16), geom: Geom) -> bool {
    let up = i32::from(cell.0) - px_row(star.y, geom);
    px_col(star.x, geom) == i32::from(cell.1) && (0..=1).contains(&up)
}

/// §5.2's light-theme thinning for a THROWN star: on a light theme a grain is
/// dealt at [`LIGHT_COUNT_SCALE`] by its own seed; on dark every grain is
/// born. The sky deals scale their probability instead ([`deal`]).
fn light_keeps(seed: u32, cfg: &Config) -> bool {
    cfg.dark_theme || hash01(mix32(seed ^ SALT_LIGHT)) < LIGHT_COUNT_SCALE
}

// ===========================================================================
// 7. Paint — one star, resolved for one frame
// ===========================================================================

/// The quads a grain needs — one white core and four tinted arms
/// ([`M3_CROSS_ADD`]) — and the floor of a star's fair share of the quad
/// budget, so a truncated frame still draws every star's core.
const M3_QUADS: usize = 5;

// The floor is only a floor if the FULL pool can afford it. `emit`'s share is
// `floor(left / stars still to draw)` and every recipe consumes at most its
// share, so that ratio never falls across the loop: it starts at
// `STARDUST_QUAD_BUDGET / STAR_POOL_MAX` and every star of a full pool, the
// last grain included, is handed at least its cross. A budget cut or a pool
// growth that broke this would stop the build here rather than hand a tail
// grain a one-quad share.
const _: () = assert!(
    STARDUST_QUAD_BUDGET >= M3_QUADS * STAR_POOL_MAX,
    "the quad budget cannot hand every star of a full pool its five-quad floor"
);

/// ONE STAR RESOLVED FOR ONE FRAME: everything the recipes of §5.7 need, and
/// nothing that has to be recomputed inside them.
///
/// It exists so the three recipes read as the spec writes them — `m3(sx, sy,
/// c, env)`, `m2(sx, sy, c, tint, env, e, f)`, `m1(sx, sy, c, tint, gold, env,
/// e, f)` — instead of as an eight-argument call at each of six sites. Every
/// field is derived; none is stored between frames.
#[derive(Clone, Copy, Debug)]
struct Paint {
    /// Magnitude, which picks the recipe.
    class: StarClass,
    /// Population — the m2's disc is a SKY-lane addition only (§5.2).
    lane: StarLane,
    /// Device-pixel centre.
    x: i32,
    /// Device-pixel centre.
    y: i32,
    /// The integer arm at this instant — the ARM CLOCK's output (§5.5, D12).
    arm: i32,
    /// `c`, the class's coverage request scaled by the population envelope,
    /// by `intensity`, and — for a TRANSIENT over a probed-occupied or
    /// UNKNOWN cell — by `1/3` (§5.2's text-safe arm; a sky star's clearance
    /// is its birth gate, see [`TEXT_SAFE_DIV`]). The two ceilings are
    /// structural (see the `const _` assertion above), so this is never
    /// clamped.
    c: f32,
    /// The population envelope itself — the star's ALPHA, for the chroma cull.
    alpha: f32,
    /// The class's own core-brightness multiplier on the LUMINANCE clock:
    /// `0.85 + 0.15·e` (m1), `0.70 + 0.30·e` (m2), `env` (m3).
    core: f32,
    /// `twinkle_env` itself — the halo rides it undiluted.
    env: f32,
    /// The star's chroma, fixed at birth (C3).
    tint: u32,
    /// Gold AND at twinkle peak: the only condition under which any mark in
    /// this theme throws diagonal glints (§5.2).
    glints: bool,
    /// A dealt GOLD star — its halo holds gold rather than breathing a hue
    /// ([`halo_rgb`]).
    gold: bool,
}

impl Paint {
    /// Resolve one star for this frame, or `None` if it has nothing to draw.
    fn of(star: &Star, probe: &GlyphProbe, ctx: &Ctx<'_>) -> Option<Self> {
        let rm = ctx.cfg.reduced_motion;
        let alpha = star.envelope(ctx.now, rm);
        if alpha < STAR_CULL_ALPHA {
            // D3: below the cull a star is GONE, never a grey speck.
            return None;
        }
        // §5.6's hold is a hold on the light too ([`Star::in_hold`]): inside
        // it the core is at full and the halo at its peak; the luminance
        // clock speaks only once the hold is spent.
        let held = rm || star.in_hold(ctx.now);
        let env = if held { 1.0 } else { star.twinkle(ctx.now) };
        let e = if held {
            1.0
        } else {
            star.twinkle_unit(ctx.now)
        };
        let (x, y) = star.pos(ctx.now, rm);
        let floor = star.class.core_floor();
        let mut c = star.class.cov(star.lane) * alpha * clamp01(ctx.cfg.intensity);
        // §5.2: "coverage over probed-occupied or unknown cells is divided by
        // 3 — v1's text-safe arm, kept" — asked of the TRANSIENTS only. A sky
        // star was proven blank at birth, streams only over the blank
        // frontier it was clamped to there (`Stardust::stream_reach`) and is
        // pulled only between two cells both proven blank
        // (`Stardust::pull_origin`); the probe
        // forgets rows (Enter, a scroll) and, under `underline`, reports the
        // star's own just-echoed glyph, so re-asking it here is a 3× pop, not
        // a clearance (see `TEXT_SAFE_DIV`).
        if star.lane.is_transient() && probe.at_px(x, y, ctx.geom) != Some(false) {
            c /= TEXT_SAFE_DIV;
        }
        Some(Self {
            class: star.class,
            lane: star.lane,
            x,
            y,
            arm: arm_px(star, ctx.geom.ch as f32, rm, ctx.now),
            c,
            alpha,
            core: if star.class == StarClass::M3 {
                env
            } else {
                floor + (1.0 - floor) * e
            },
            env,
            tint: star.tint,
            glints: !rm && star.glinting(ctx.now),
            gold: star.gold,
        })
    }

    /// **THE DARK RECIPES** (§5.7), verbatim.
    ///
    /// D2 governs all three: `push_twinkle_star` draws body and nucleus in ONE
    /// colour, so the body is always `#FFFFFF` and the star's temperature is
    /// carried by the halo plus four [`STUB_LEN_PX`]-px stubs strictly
    /// outside `star_body_px(arm)` (the grain's is its four outboard arms).
    /// And D1 governs the centre: the crossing bars plus the nucleus already
    /// stack to `STAR_STACK_ADD 2.35`, so no class adds a duplicate core rect
    /// on top of it — and the m2's 3×3 disc is the SKY m2's shoulder only
    /// (`2.35·48 + 0.35·48 = 130`); the transient m2 keeps the stack alone,
    /// which is the `2.35·34 = 80` §5.2 publishes.
    ///
    /// `quad_cap` is this star's own share of the frame's quad budget;
    /// `haloed` counts the STARS given a halo so far this frame; `laid_from`
    /// is where this frame's sky begins in `frame.out` and `frame.halos` —
    /// the ground a star is priced over ([`sky_ground`]).
    ///
    /// **EVERY CLASS IS HELD UNDER THE CARET LAW** ([`STAR_LIGHT_CEIL`]),
    /// in two steps. The haloed classes are PRICED before they are drawn
    /// ([`price_centre`]): the body's coverage, the disc's and the halo's
    /// peak are what the law leaves of the recipe's request on this ground
    /// at the centre — the whole request where the ground has the room (a
    /// black ground at the published `143` has it), colour before white
    /// where it has not. Then every class is SETTLED after it is drawn
    /// ([`settle_under_ceiling`]): the pixels its recipe stacks light on
    /// away from the centre — the arm's first pixel, where a full-request
    /// tint stub, the taper's brightest span and the halo's ring meet — are
    /// read back from its own quads, and the whole star is dimmed by one
    /// factor until they fit. The second step is the one a yellow or green
    /// hero at full intensity needs: those stops are most of the eye's
    /// luminance, and `61 + 56` levels of them beside a white taper is over
    /// the ceiling where the same levels of red or violet are not.
    fn draw_add(
        &self,
        ctx: &Ctx<'_>,
        frame: &mut Frame<'_>,
        quad_cap: usize,
        haloed: &mut usize,
        laid_from: (usize, usize),
    ) {
        let geom = ctx.geom;
        let (out_from, sky_from) = laid_from;
        // The pixels this star's recipe stacks on, and the sky under each —
        // read BEFORE its own quads go out, since they would be in the slice.
        let d = if self.class == StarClass::M3 {
            1
        } else {
            star_body_px(self.arm) + 1
        };
        let mut watch = [((0i32, 0i32), 0u32); SETTLE_PIXELS];
        for (slot, (dx, dy)) in watch.iter_mut().zip([
            (0, 0),
            (d, 0),
            (-d, 0),
            (0, d),
            (0, -d),
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
        ]) {
            let p = (self.x + dx, self.y + dy);
            *slot = (
                p,
                sky_ground(
                    ctx.cfg.theme_bg,
                    &frame.out[out_from..],
                    &frame.halos[sky_from..],
                    p.0,
                    p.1,
                ),
            );
        }
        let ground = watch[0].1;
        let q0 = frame.out.len();
        let h0 = frame.halos.len();
        match self.class {
            StarClass::M3 => self.draw_m3(geom, frame.out, quad_cap),
            StarClass::M2 => {
                // The body is `M2_BODY_TINT` of the way from white to the
                // tint (owner, 2026-09-08: "m2 tinted cores"); the disc is
                // the tint itself, and the SKY m2's shoulder only (D1).
                let body = mix_rgb(TINT_WHITE_RGB, self.tint, M2_BODY_TINT);
                let disc = if self.lane.is_sky() {
                    cov_byte(self.c * M2_DISC_ADD)
                } else {
                    0
                };
                let r = self.halo_r(geom.ch as f32);
                let (halo_rgb, halo) = self.halo_request(M2_HALO_SHARE, *haloed);
                let (cov, disc, peak) = price_centre(
                    ground,
                    self.tint == TINT_WHITE_RGB,
                    (body, cov_byte(self.c * self.core)),
                    (self.tint, disc),
                    (halo_rgb, halo),
                );
                push_twinkle_star(
                    frame.out, geom, self.x, self.y, self.arm, cov, false, body, quad_cap,
                );
                if disc > 0 && frame.out.len() < quad_cap {
                    // The 3×3 disc (5×5 on the retina cell): an m2's one
                    // addition, and a SHOULDER rather than a second core (D1).
                    let sh = m2_disc_shoulder_px(geom.ch as f32);
                    push_fx_rect(
                        frame.out,
                        geom,
                        self.x - sh,
                        self.y - sh,
                        2 * sh + 1,
                        2 * sh + 1,
                        premul_rgb(self.tint, disc),
                    );
                }
                self.draw_stubs(geom, frame.out, quad_cap);
                self.draw_halo(geom, frame.halos, r, (halo_rgb, peak), haloed);
            }
            StarClass::M1 => {
                let r = self.halo_r(geom.ch as f32);
                let (halo_rgb, halo) = self.halo_request(M1_HALO_SHARE, *haloed);
                let (cov, _, peak) = price_centre(
                    ground,
                    self.tint == TINT_WHITE_RGB,
                    (TINT_WHITE_RGB, cov_byte(self.c * self.core)),
                    (self.tint, 0),
                    (halo_rgb, halo),
                );
                push_twinkle_star(
                    frame.out,
                    geom,
                    self.x,
                    self.y,
                    self.arm,
                    cov,
                    self.glints,
                    TINT_WHITE_RGB,
                    quad_cap,
                );
                self.draw_stubs(geom, frame.out, quad_cap);
                self.draw_halo(geom, frame.halos, r, (halo_rgb, peak), haloed);
            }
        }
        settle_under_ceiling(frame, q0, h0, &watch);
    }

    /// §5.7's m3: a white 1-px `core = 1.6·c·env`, plus four TINTED arms of
    /// `arm` px at [`M3_CROSS_ADD`]`·c` running outboard of it.
    ///
    /// The cross IS its Airy disc — the reason a grain is a star and v1's
    /// `d×d` mote was dust — and it is the grain's whole colour (D2: the core
    /// is white in every class, and a bar drawn THROUGH the centre would tint
    /// the peak, so the arms start one pixel out). Five quads, one peak at
    /// `1.6·c`, every 8-neighbour of it at `0.45·c`: 3.6:1 at the crest and
    /// 2:1 at the twinkle trough, because the core rides `env` and the disc
    /// rides alpha alone (§5.5's chromatic scintillation, on the grain).
    ///
    /// **The CENTRE byte is priced in one conversion** — `round(1.6·c·env)` —
    /// so the composite lands on the published integer (61 for a transient
    /// grain, 98 for a sky one).
    ///
    /// `quad_cap` is this grain's share of the frame's quad budget, checked
    /// between the pushes exactly as `push_twinkle_star` checks its own: the
    /// core goes first and always (the share is never empty — `emit` breaks
    /// at zero), and an arm is dropped rather than drawn over the cap, so the
    /// frame's total stays ≤ [`STARDUST_QUAD_BUDGET`] with a grain at the
    /// tail (`an_over_budget_frame_keeps_every_star_s_core`; the push-by-push
    /// law itself is `a_grain_s_share_is_honoured_push_by_push_core_first`).
    fn draw_m3(&self, geom: Geom, out: &mut Vec<GlowQuad>, quad_cap: usize) {
        // The grain is PURE TINT, core included (owner, 2026-09-08: "m3 ...
        // fully coloured at the spectrum position of the cell they rose
        // from"); the peak law holds on the tint's own brightest channel.
        let core = premul_rgb(self.tint, cov_byte(self.c * M3_CENTRE_ADD * self.core));
        let disc = premul_rgb(self.tint, cov_byte(self.c * M3_CROSS_ADD));
        let a = self.arm;
        // Core first, so a star truncated by the quad budget keeps its peak.
        push_fx_rect(out, geom, self.x, self.y, 1, 1, core);
        for (x, y, w, h) in [
            (self.x - a, self.y, a, 1),
            (self.x + 1, self.y, a, 1),
            (self.x, self.y - a, 1, a),
            (self.x, self.y + 1, 1, a),
        ] {
            if out.len() >= quad_cap {
                return;
            }
            push_fx_rect(out, geom, x, y, w, h, disc);
        }
    }

    /// The four TINT STUBS (§5.7): [`STUB_LEN_PX`] px each, running outboard
    /// from `±(star_body_px(arm) + 1)` on each axis at [`STUB_COV_SHARE`]`·c`
    /// — strictly outside the body, which is what makes "the body is white
    /// and only the halo and stubs are tinted" checkable per quad (D2), and
    /// long enough that every arm ENDS in its colour rather than hiding one
    /// tinted pixel under a white point. Four quads, as before.
    fn draw_stubs(&self, geom: Geom, out: &mut Vec<GlowQuad>, quad_cap: usize) {
        let d = star_body_px(self.arm) + 1;
        let l = stub_len_px(geom.ch as f32);
        let c = premul_rgb(self.tint, cov_byte(self.c * STUB_COV_SHARE));
        for (x, y, w, h) in [
            (self.x - d - (l - 1), self.y, l, 1),
            (self.x + d, self.y, l, 1),
            (self.x, self.y - d - (l - 1), 1, l),
            (self.x, self.y + d, 1, l),
        ] {
            if out.len() >= quad_cap {
                return;
            }
            push_fx_rect(out, geom, x, y, w, h, c);
        }
    }

    /// **THE HALO'S RADIUS IS THE CELL'S** — [`M1_HALO_R_ARM`] /
    /// [`M2_HALO_R_ARM`] times the class's NOMINAL arm (`star_arm`, the
    /// float the arm clock modulates), the m2's floored at
    /// [`m2_halo_r_min_px`]: 8 / 6 px at the 1× cell, 11 / 7.7 at `ch` 18,
    /// 16 / 12 on the retina cell — twice the 1× picture, exactly. Not the
    /// clocked integer arm: that truncates unevenly across cells and steps
    /// at 8–12 Hz, which is a size flicker D12 exempts only for marks under
    /// 10 px. `0` for the grain, which has no halo.
    fn halo_r(&self, ch: f32) -> f32 {
        let nominal = star_arm(ch, self.class.arm_ratio());
        match self.class {
            StarClass::M1 => M1_HALO_R_ARM * nominal,
            StarClass::M2 => (M2_HALO_R_ARM * nominal).max(m2_halo_r_min_px(ch)),
            StarClass::M3 => 0.0,
        }
    }

    /// The star's HALO REQUEST — its colour at this instant ([`halo_rgb`])
    /// and the peak the recipe asks for: the tint's atmosphere, riding `env`
    /// undiluted so the colour breathes while the white core holds (§5.5's
    /// "chromatic scintillation with no chroma fade"). Budgeted per STAR
    /// ([`STARDUST_HALO_BUDGET`]), and culled below §3.2's chroma floor
    /// (`α·env < 0.12`: nothing coloured is drawn below α 0.12) — a peak of
    /// `0` for either, so a halo the frame will not draw is never priced
    /// against the body it belongs to.
    fn halo_request(&self, share: f32, haloed: usize) -> (u32, u8) {
        let rgb = halo_rgb(self.tint, self.gold, self.env);
        if haloed >= STARDUST_HALO_BUDGET || self.alpha * self.env < CHROMA_CULL_ALPHA {
            return (rgb, 0);
        }
        (rgb, cov_byte(self.c * share * self.env))
    }

    /// Lay the halo [`Self::halo_request`] asked for, at the `peak` the
    /// caret law left it ([`price_centre`]) — nothing at `0`.
    fn draw_halo(
        &self,
        geom: Geom,
        halos: &mut Vec<RainHalo>,
        r: f32,
        (rgb, peak): (u32, u8),
        haloed: &mut usize,
    ) {
        if peak == 0 {
            return;
        }
        let ri = (r.round() as i32).max(1);
        let before = halos.len();
        push_halo_quads(
            halos,
            geom,
            usize::MAX,
            (self.x, self.y),
            (ri, ri),
            premul_rgb(rgb, peak),
            HaloMode::Add,
        );
        if halos.len() > before {
            *haloed += 1;
        }
    }

    /// **THE LIGHT FORK** (§3.3, §5.2's light twins) — one operator flip, same
    /// anatomy, same timings, same curves.
    ///
    /// Additive light is invisible on paper-white, so every mark becomes
    /// source-over INK: **m1/m2** are the family's light star — the
    /// `push_twinkle_over` silhouette at [`STAR_ARM_INK`], its core in the
    /// tint's deep ink (`InkRole::Leading`, cap 236) and its needles in the
    /// conservative over-text ink (`InkRole::OverText`, cap 190) — and **m3 is
    /// one round dot** priced by the stacked-ink law, so a grain on paper is
    /// the round mote it always was and not a hard cross. **No atmosphere
    /// halo at all** — §3.3 chooses none rather than fight the peak-96 cull;
    /// the tint is carried by the ink itself.
    ///
    /// The silhouette's numbers are the family's (`STAR_WAIST`,
    /// `STAR_TAPER_BODY`, `STAR_CORE`, `STAR_GLINT*`, the centre-lay stack),
    /// shared verbatim with `cursor_glow`'s private rasterizer; the lays are
    /// written here because that rasterizer is not exported, and the day it
    /// is, this fork collapses into one call.
    fn draw_over(&self, geom: Geom, frame: &mut Frame<'_>, halo_cap: usize) {
        if frame.halos.len() >= halo_cap {
            return;
        }
        let arm = (self.arm as f32 * STAR_ARM_INK).max(1.0);
        match self.class {
            StarClass::M3 => {
                let cov = cov_byte(self.c * self.core);
                let a = stacked_ink_alpha(cov, InkRole::OverText).min(LIGHT_ALPHA_CAP as u32);
                let ink = (InkRole::OverText.ink(self.tint) & 0x00FF_FFFF) | (a << 24);
                let r = arm.round() as i32;
                push_halo_quads(
                    frame.halos,
                    geom,
                    halo_cap,
                    (self.x, self.y),
                    (r.max(1), r.max(1)),
                    ink,
                    HaloMode::Over,
                );
            }
            StarClass::M1 | StarClass::M2 => {
                push_twinkle_over_twin(
                    frame.halos,
                    geom,
                    halo_cap,
                    LightStar {
                        centre: (self.x, self.y),
                        arm,
                        rgb: self.tint,
                        gold: self.glints,
                        cov: cov_byte(self.c * self.core),
                    },
                );
            }
        }
    }
}

/// The ARM CLOCK, evaluated with the frame's own `now` (§5.5, D12, T7).
///
/// `arm(t) = A·(floor + (1 − floor)·sin²(2π·f·age + φ))`, ROUNDED to an
/// integer so the star visibly opens 1 → 2 → 1 px, and floored at the
/// class's published band ([`StarClass::arm_min_px`]). `A` is the class's
/// rung on the family's arm ladder, so a star's size is still only ever one
/// of the named sizes, modulated. Inside the class hold the arm is the
/// nominal `round(A)` — the star is BORN at its full size ([`Star::in_hold`])
/// — and under reduced motion it stays there.
fn arm_px(star: &Star, ch: f32, reduced_motion: bool, now: Instant) -> i32 {
    let nominal = star_arm(ch, star.class.arm_ratio());
    let min = star.class.arm_min_px(ch);
    if reduced_motion || star.in_hold(now) {
        return (nominal.round() as i32).max(min);
    }
    let floor = star.class.arm_floor();
    let s = (std::f32::consts::TAU * star.f * star.age_s(now) + star.lum_phase()).sin();
    ((nominal * (floor + (1.0 - floor) * s * s)).round() as i32).max(min)
}

/// Seconds until `θ = θ_now + ω·t` next reaches `±a₀ (mod π)` — the
/// crossings of `sin²θ` (and of `|sin θ|`) with a level, for the two
/// scintillation clocks' closed-form steps. `a₀` is in `[0, π/2]`; a
/// crossing at `θ_now` itself does not count.
#[must_use]
fn next_sine_crossing_s(theta: f32, a0: f32, omega: f32) -> Option<f32> {
    use std::f32::consts::PI;
    if !(omega.is_finite() && theta.is_finite() && a0.is_finite()) || omega <= 0.0 {
        return None;
    }
    let base = theta.rem_euclid(PI);
    let mut gap = f32::INFINITY;
    for cand in [a0, PI - a0, PI + a0, 2.0 * PI - a0] {
        let g = cand - base;
        if g > 1e-5 && g < gap {
            gap = g;
        }
    }
    if gap.is_finite() {
        Some(gap / omega)
    } else {
        None
    }
}

// ===========================================================================
// 8. Small shared shapes
// ===========================================================================

/// Salt for the ONE strike draw (§5.6's m1 / m2 / m3 partition).
const SALT_STRIKE: u32 = 0x0000_0101;
/// Salt for the second grain's light-theme deal.
const SALT_SECOND_M3: u32 = 0x0000_0202;
/// Salt for the field deal.
const SALT_FIELD: u32 = 0x0000_0404;
/// Salt for the field's own m2 sub-deal.
const SALT_FIELD_M2: u32 = 0x0000_0505;
/// Salt for the tint deal (§5.3).
const SALT_TINT: u32 = 0x0000_0606;
/// Salt for the luminance phase φ (§5.5).
const SALT_PHASE: u32 = 0x0000_0707;
/// Salt for the arm-size rate `f` (§5.5).
const SALT_RATE: u32 = 0x0000_0808;
/// Salt for the birth jitter inside the sky band (§5.4).
const SALT_JITTER: u32 = 0x0000_0909;
/// Salt for the erase throw (§5.6).
const SALT_ERASE: u32 = 0x0000_0A0A;
/// Salt for the light-theme thinning of a thrown grain (§5.2).
const SALT_LIGHT: u32 = 0x0000_0B0B;

/// The family's avalanche mix — the same two rounds `cursor_glow` uses for its
/// per-cell deals, so a v2 star and a v1 spark hashed from the same cell do
/// not correlate.
#[inline]
fn mix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    h
}

/// `(row, col, salt)` → seed (§18: every seed is hashed at the spawn edge; no
/// per-frame RNG anywhere).
#[inline]
fn cell_hash(row: u16, col: u16, salt: u32) -> u32 {
    mix32(
        u32::from(row).wrapping_mul(73_856_093)
            ^ u32::from(col).wrapping_mul(19_349_663)
            ^ salt.wrapping_mul(83_492_791),
    )
}

/// A seed's uniform draw on `[0, 1)`.
#[inline]
fn hash01(h: u32) -> f32 {
    (h >> 8) as f32 / 16_777_216.0
}

/// ONE DEAL — "one in `den`", scaled by the light theme's count factor.
///
/// A probability test rather than a modulo, because §5.2's light fork is
/// "counts × 0.6" and a modulo cannot express 0.6 of a deal without changing
/// WHICH cells are starred; scaling the probability thins the same sky instead
/// of reshuffling it.
#[inline]
fn deal(cell: u32, salt: u32, den: u32, scale: f32) -> bool {
    hash01(mix32(cell ^ salt)) < scale / den as f32
}

/// The same deal against a SHARE rather than a denominator — what a rate that
/// is lerped (flow's field share) needs, and the same draw on the same cell
/// as [`deal`] with `share == scale / den`, to the bit.
#[inline]
fn deal_p(cell: u32, salt: u32, share: f32) -> bool {
    hash01(mix32(cell ^ salt)) < share
}

/// **THE COMBO LADDER'S RUNGS**, in the order a key claims them: the highest
/// rung a run length has reached wins, so the 64th key pays gold and not the
/// 16th's promotion three times over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rung {
    /// [`LADDER_M2_KEYS`] — the strike is an m2 at least.
    Promote,
    /// [`LADDER_WHITE_KEYS`] — an m2, white by rule.
    White,
    /// [`LADDER_GOLD_KEYS`] — an earned gold m1, glinting if a token is on
    /// hand.
    Gold,
}

/// Which rung a run of `combo` keys pays on its newest key, if any. Zero pays
/// nothing (a broken run owes nothing).
#[inline]
#[must_use]
fn ladder_rung(combo: u32) -> Option<Rung> {
    if combo == 0 {
        None
    } else if combo.is_multiple_of(LADDER_GOLD_KEYS) {
        Some(Rung::Gold)
    } else if combo.is_multiple_of(LADDER_WHITE_KEYS) {
        Some(Rung::White)
    } else if combo.is_multiple_of(LADDER_M2_KEYS) {
        Some(Rung::Promote)
    } else {
        None
    }
}

/// The seeded ARM-SIZE scintillation rate in Hz, `8 + 4·hash01` (§5.5). The
/// audio glint that rides a star twinkles at this same number (§13).
#[inline]
fn scint_rate(seed: u32) -> f32 {
    SCINT_F_MIN + (SCINT_F_MAX - SCINT_F_MIN) * hash01(mix32(seed ^ SALT_RATE))
}

/// **THE TINT BY CLASS** (§5.3 as re-ruled 2026-09-08): a GRAIN always wears
/// the stop of the cell it rose from — "m3 fully coloured at the spectrum
/// position of the cell they rose from" — and never gold (a grain has no
/// glints to carry); an m1 or m2 takes the [`deal_tint`] deal.
#[inline]
fn class_tint(class: StarClass, seed: u32, field_t: f32) -> (u32, bool) {
    if class == StarClass::M3 {
        (spectrum_snap(field_t), false)
    } else {
        deal_tint(seed, field_t)
    }
}

/// **THE 75/10/15 TINT DEAL** (§5.3; was 60/25/15 until the owner asked for
/// more colour, 2026-09-08), dealt by seed and CONSTANT FOR LIFE — `(tint,
/// gold)` — for an m1 or m2 ([`class_tint`] routes the grain past it).
///
/// 75 % take `spectrum_snap(field at the birth cell)` — the sky inherits the
/// spectrum's order along the line, warm stars over the old end and violet
/// over the new; 10 % are an A-type white; 15 % are gold, and gold is the only
/// class that carries diagonal glints. Gold-ness is the DEAL's answer, not the
/// colour's: the arc's yellow stop is the same RGB as gold, and a yellow field
/// star does not glint.
#[inline]
fn deal_tint(seed: u32, field_t: f32) -> (u32, bool) {
    let u = hash01(mix32(seed ^ SALT_TINT));
    if u < TINT_FIELD_SHARE {
        (spectrum_snap(field_t), false)
    } else if u < TINT_FIELD_SHARE + TINT_WHITE_SHARE {
        (TINT_WHITE_RGB, false)
    } else {
        (TINT_GOLD_RGB, true)
    }
}

/// A coverage request as a byte — **the one float→byte conversion on the emit
/// path**, ROUND-HALF and saturating.
///
/// Round-half rather than truncate, because a class's composited centre is the
/// sum of three or four independently converted coverage bytes: truncating
/// each one sheds up to a unit per rect, and a sky m2 then lands at 126 where
/// §5.2 publishes 130. Rounding holds every published centre to within one
/// byte of its own product, which is what makes `a_star_is_a_peak_not_a_blob`
/// an assertion about the design rather than about the conversion. It is also
/// `aterm_render::premul_rgb`'s own rule, so the two halves of one colour
/// round the same way.
#[inline]
fn cov_byte(v: f32) -> u8 {
    if v.is_nan() {
        0
    } else {
        (v + 0.5).clamp(0.0, 255.0) as u8
    }
}

/// The grid row a window-absolute Y falls in (signed: the sky band above row 0
/// is row − 1, and that is a real answer, not an error).
#[inline]
fn px_row(y: f32, geom: Geom) -> i32 {
    ((y - f32::from(geom.origin_y)) / geom.ch as f32).floor() as i32
}

/// The grid column a window-absolute X falls in (signed, like [`px_row`]).
#[inline]
fn px_col(x: f32, geom: Geom) -> i32 {
    ((x - f32::from(geom.origin_x)) / geom.cw as f32).floor() as i32
}

/// Is a star at window Y in the SKY of grid `row` — the band a field star
/// born over that row's cells occupies? Under the shipped `tall` spelling a
/// cell's sky is the lower third of the row above it, under `underline` the
/// top of its own row (§5.4), so the answer is "row or row − 1" whichever
/// the host runs — the one spelling [`Stardust::finish_field`] and
/// [`Stardust::relight_field`] share.
#[inline]
fn in_sky_of_row(y: f32, row: u16, geom: Geom) -> bool {
    (0..=1).contains(&(i32::from(row) - px_row(y, geom)))
}

/// Is a device pixel inside the effects box (`Geom::fx_*`: the grid plus the
/// `head` band above it — §5.4's row-0 sky)? The one glass test every birth
/// and every rest in this file runs.
#[inline]
fn on_glass(p: (i32, i32), geom: Geom) -> bool {
    p.0 >= geom.fx_left() && p.0 < geom.fx_right() && p.1 >= geom.fx_top() && p.1 < geom.fx_bot()
}

/// The nearest pixel of the effects box to `p` — `p` itself when it is on
/// the glass. Where a transient's REST is pulled to when its throw would end
/// off the glass ([`Stardust::settle`]): the ray keeps its origin and ends on
/// the box's edge instead, flattened toward the line.
#[inline]
fn clamp_to_glass(p: (i32, i32), geom: Geom) -> (i32, i32) {
    (
        p.0.clamp(geom.fx_left(), geom.fx_right() - 1),
        p.1.clamp(geom.fx_top(), geom.fx_bot() - 1),
    )
}

/// **THE FAN'S RISE CAPS** for one landing — `(up, down)` in px, the most a
/// fan star may come to rest above and below the landing's centre (the
/// vertical reach law, see [`FAN_SQUASH`]). The ellipse's own semi-minor
/// axis, `reach·FAN_SQUASH`, capped at [`FAN_RISE_MAX_CH`], and never past
/// the effects box — on EVERY row. On row 0 the `Geom.head` band is the
/// row's sky (§5.4) and the fan may rise into it, but the 1.6 ch ceiling
/// bounds the band exactly as it bounds a mid-screen row: the band is the
/// ribbon's row-0 sky, not a fan's. (This arm once read the band's full
/// room as the cap, and the confirmation capture found 10–11 px per flight
/// of lum 41–48 resting 61–86 px above the prompt row — 2.2–3.1 ch, in the
/// head strip — where every mid-screen row stayed inside ±1.6 ch.) Below,
/// the same glass bound. Event-edge work: once per landing, never per frame.
fn fan_rise(reach: f32, at_px: (f32, f32), geom: Geom) -> (f32, f32) {
    let squash = reach * FAN_SQUASH;
    let ceiling = FAN_RISE_MAX_CH * geom.ch as f32;
    let room_up = (at_px.1 - geom.fx_top() as f32).max(0.0);
    let room_down = ((geom.fx_bot() - 1) as f32 - at_px.1).max(0.0);
    (
        squash.min(ceiling).min(room_up),
        squash.min(ceiling).min(room_down),
    )
}

/// **ONE FAN RAY under the vertical reach law** — the throw's displacement at
/// rest, px. `a` is the seeded angle, `reach` the star's own radial length
/// (`reach·(0.55..1.05)`, §6.5 layer 11), `up`/`down` the landing's rise caps
/// ([`fan_rise`]) and `j` the star's cap jitter in `0..1`
/// ([`FAN_RISE_JITTER_MIN`]). A ray whose vertical component would exceed
/// its cap is FLATTENED toward the line, keeping its length — the throw is
/// still `reach` px long, spent along the line instead of into the row above
/// — and never dropped, so the census (D6) is untouched. Window y grows
/// downward: a negative `dy` rises.
fn fan_ray(a: f32, reach: f32, up: f32, down: f32, j: f32) -> (f32, f32) {
    let (dx, dy) = (a.cos() * reach, a.sin() * reach);
    let cap = if dy < 0.0 { up } else { down };
    let cap = cap * (FAN_RISE_JITTER_MIN + (1.0 - FAN_RISE_JITTER_MIN) * j);
    if dy.abs() <= cap {
        return (dx, dy);
    }
    let along = (reach * reach - cap * cap).max(0.0).sqrt();
    (along.copysign(dx), cap.copysign(dy))
}

/// D16's two spellings of the band top, for a cell the ribbon has laid nothing
/// on.
///
/// Under **`tall`** the body rises `RIBBON_TALL_UP 1.10 ch` from the row
/// bottom, so its top is 0.10 `ch` ABOVE the cell and the sky band is the
/// lower third of row − 1. Under **`underline`** the band sits at the cell's
/// own top, and §5.4's `[top − 0.30 ch, top − 0.04 ch]` then lands on the
/// `[0.04, 0.30]·ch` of the cell the spec names — the anchor is chosen to put
/// the band's LOWER edge exactly on the published 0.30.
fn default_band_top(geom: Geom, cfg: &Config, row: u16) -> f32 {
    let ch = geom.ch as f32;
    let cell_top = f32::from(geom.origin_y) + f32::from(row) * ch;
    if cfg.ribbon_tall {
        cell_top + ch - super::ribbon::TALL_UP_CH * ch
    } else {
        cell_top + (SKY_TOP_CH + SKY_BOTTOM_CH) * ch
    }
}

/// ONE HALO — a radial peak, split into per-cell-row [`RainHalo`] quads that
/// share one centre and one falloff. `Add` for the dark atmosphere (a
/// premultiplied colour), `Over` for the light ink lays (an ink RGB with the
/// centre over-alpha in the high byte). `cap` bounds `out.len()`.
///
/// The local twin of `cursor_glow`'s private `push_halo` / `push_halo_over`,
/// written here rather than widening a foreign file while four agents hold
/// the same checkout; §17.3's phase 7 (delete v1's rainbow arms) is where
/// the two collapse into one. The law it carries is the same one: **the
/// centre is the falloff's truth** — it is stored, never clamped into the box,
/// because clamping an off-edge centre onto the edge renders the surviving
/// sliver at 41-96 % of peak where the true falloff says ~0.
fn push_halo_quads(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cap: usize,
    centre: (i32, i32),
    radii: (i32, i32),
    color: u32,
    mode: HaloMode,
) {
    let glass = (
        geom.fx_left(),
        geom.fx_top(),
        geom.fx_right(),
        geom.fx_bot(),
    );
    let spec = HaloSpec {
        centre,
        radii,
        color,
        mode,
    };
    push_halo_quads_clipped(out, geom, cap, spec, glass);
}

/// One radial falloff as [`push_halo_quads_clipped`] takes it — the four
/// numbers of a halo that are not its box.
#[derive(Clone, Copy, Debug)]
struct HaloSpec {
    /// The falloff's centre, window px.
    centre: (i32, i32),
    /// Its radii, px.
    radii: (i32, i32),
    /// Premultiplied (`Add`) or ink-with-ceiling (`Over`).
    color: u32,
    /// The operator.
    mode: HaloMode,
}

/// [`push_halo_quads`] with an explicit CLIP BOX `(x0, y0, x1, y1)`
/// (half-open, window px) intersected with the glass — the aurora's veil is
/// a falloff whose box is the cell's own column above the line, not the
/// ellipse's whole extent ([`Stardust::draw_veil`]). Same law as the
/// unclipped twin: the centre is the falloff's truth and is stored, never
/// clamped into the box.
fn push_halo_quads_clipped(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cap: usize,
    spec: HaloSpec,
    clip: (i32, i32, i32, i32),
) {
    let HaloSpec {
        centre,
        radii,
        color,
        mode,
    } = spec;
    if color == 0 {
        return;
    }
    let (bl, bt) = (geom.fx_left().max(clip.0), geom.fx_top().max(clip.1));
    let (br, bb) = (geom.fx_right().min(clip.2), geom.fx_bot().min(clip.3));
    let (rx, ry) = (radii.0.max(1), radii.1.max(1));
    let (cx, cy) = centre;
    let x0 = (cx - rx).max(bl);
    let x1 = (cx + rx).min(br);
    let y0 = (cy - ry).max(bt);
    let y1 = (cy + ry).min(bb);
    if x1 <= x0 || y1 <= y0 || cx < 0 || cy < 0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let mut yy = y0;
    while yy < y1 {
        if out.len() >= cap {
            return;
        }
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color,
            cx: cx as u16,
            cy: cy as u16,
            rx: rx as u16,
            ry: ry as u16,
            mode,
        });
        yy = band_end;
    }
}

// ===========================================================================
// 8a. The aurora's cell, and the colour helpers the coloured sky needs
// ===========================================================================

/// ONE CELL OF THE AURORA — the veil above one laid ribbon cell (section
/// 1a′). `Copy` and flat like a [`Star`]: everything on glass is derived
/// from these fields and `now`.
#[derive(Clone, Copy, Debug)]
pub struct Veil {
    /// The laid cell's row.
    pub row: u16,
    /// The laid cell's column — the veil's box is this column, and only this
    /// column.
    pub col: u16,
    /// The ribbon's field at the cell (C2), unfolded — the bed folds it with
    /// `tri` when it draws, and so does the veil.
    pub t: f32,
    /// The veil's peak, as a share of [`AURORA_COV_PEAK`], priced ONCE at
    /// birth from `Ctx::birth_disp` ([`veil_gain`]).
    pub gain: f32,
    /// The key that laid the cell — the `edge-in` and the horizon read it.
    pub born: Instant,
    /// The last key on the row — the field fade reads it
    /// ([`Stardust::relight_field`]).
    pub lit: Instant,
    /// The finish this cell is on, if any: its retract, or the focus ember.
    pub finish: Option<Finish>,
}

impl Veil {
    /// Seconds since the key that laid the cell, saturating at zero.
    #[must_use]
    pub fn age_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }

    /// Seconds since the last key on the row, saturating at zero.
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.lit).as_secs_f32()
    }

    /// The veil's alpha at `now` — the FIELD lane's own envelope
    /// ([`field_lane_envelope`]): it is the sky of the cell it rides, and it
    /// leaves with the ribbon the way the field stars do.
    #[must_use]
    pub fn envelope(&self, now: Instant, reduced_motion: bool) -> f32 {
        let fin = self.finish.map_or(1.0, |f| f.gain(now));
        field_lane_envelope(self.age_s(now), self.idle_s(now), fin, reduced_motion)
    }

    /// True once the cell must leave the pool — the field star's own three
    /// exits ([`Star::dead`]).
    #[must_use]
    pub fn dead(&self, now: Instant) -> bool {
        self.age_s(now) >= FIELD_LIFE_S
            || self.idle_s(now) >= field_fade_life_s()
            || self.finish.is_some_and(|f| f.gain(now) < STAR_CULL_ALPHA)
    }

    /// Put this cell on a finish ending at `at + span_s`, unless one that
    /// ends sooner already stands ([`Star::finish_by`]'s rule).
    pub fn finish_by(&mut self, at: Instant, span_s: f32) {
        let next = Finish { at, span_s };
        if self.finish.is_none_or(|f| next.ends() < f.ends()) {
            self.finish = Some(next);
        }
    }

    /// The window-absolute Y the veil's box ENDS at (exclusive) and its
    /// falloff is centred on: the sky band's own anchor for the row —
    /// [`default_band_top`], the ribbon's top under `tall` — and never below
    /// the glyph row's own top, so under `underline` (whose band anchor is
    /// inside the cell) the veil still sits wholly above the text.
    #[must_use]
    pub fn bottom_px(&self, geom: Geom, cfg: &Config) -> f32 {
        let cell_top = f32::from(geom.origin_y) + f32::from(self.row) * geom.ch as f32;
        default_band_top(geom, cfg, self.row).min(cell_top)
    }
}

/// **THE FIELD LANE'S ONE ENVELOPE** — shared by the field star
/// ([`Star::envelope`]) and the aurora's veil ([`Veil::envelope`]), because
/// both ARE the cell's sky (§5.6's Field row: "rides the cell's own edge
/// envelope × retract — alpha only"), as the product of three alpha clocks:
///
/// * **the birth** — the cell's 18 ms `edge-in` from [`BIRTH_EDGE_FLOOR`]
///   (T2: on glass, above the cull, on the frame that echoes the key);
/// * **the horizon** — the ribbon's own `expiry_melt` over [`FIELD_LIFE_S`],
///   the swoosh span at which the cell is guaranteed gone;
/// * **the hand** — [`FIELD_HOLD_MS`] / [`FIELD_TAU_MS`] from the last key
///   on the row: full while the hand is there, half at 550 ms after it
///   leaves, culled at 880 ms — before the ribbon's retract moves.
///
/// Times `fin`, the finish if one is on. Under reduced motion the theme's
/// one linear fade replaces both curves at each span's end.
#[must_use]
pub fn field_lane_envelope(age_s: f32, idle_s: f32, fin: f32, reduced_motion: bool) -> f32 {
    if reduced_motion {
        let fade = REDUCED_MOTION_FADE_MS / 1000.0;
        return clamp01((FIELD_LIFE_S - age_s) / fade.max(f32::EPSILON))
            * reduced_fade(field_fade_life_s() - idle_s)
            * fin;
    }
    let birth = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age_s);
    let hand = half_life(idle_s, FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0);
    birth * expiry_melt(age_s / FIELD_LIFE_S.max(f32::EPSILON)) * hand * fin
}

/// **THE AURORA'S GAIN** at a birth spine — `0` below [`AURORA_COLD_DISP`]
/// ("invisible cold"), `smoothstep` up to `1` at a spine of one: 0.37 at
/// 0.5 ("plain"), 0.71 at 0.7, 1.0 at 1.0 ("full").
#[must_use]
pub fn veil_gain(birth_disp: f32) -> f32 {
    if !birth_disp.is_finite() || birth_disp < AURORA_COLD_DISP {
        return 0.0;
    }
    smoothstep01((birth_disp - AURORA_COLD_DISP) / (1.0 - AURORA_COLD_DISP))
}

/// `a` moved `k` of the way toward `b`, per channel, rounded — the m2's
/// body colour ([`M2_BODY_TINT`]).
#[inline]
#[must_use]
fn mix_rgb(a: u32, b: u32, k: f32) -> u32 {
    let k = clamp01(k);
    let ch = |sh: u32| -> u32 {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        (x + (y - x) * k).round().clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// **THE HALO'S COLOUR AT THIS INSTANT** — the star's stop, breathed
/// [`HALO_HUE_SWING_ENTRIES`] table entries along the arc on the luminance
/// clock: `env` runs `0.55..=1`, and the halo walks `−15` at the trough to
/// `+15` at the peak from its stop's own anchor (`SPECTRUM_ANCHOR_AT`),
/// landing on a whole `SPECTRUM_LUT` entry so the colour is one the arc
/// itself holds — never off the spectrum, never onto a neighbouring name.
/// The A-type white and a dealt gold have no stop and hold their colour;
/// so does any tint that is not one of the seven names (C1 says there is
/// none, and the fallback is the tint itself).
#[must_use]
fn halo_rgb(tint: u32, gold: bool, env: f32) -> u32 {
    if gold || tint == TINT_WHITE_RGB {
        return tint;
    }
    let Some(stop) = (0..SPECTRUM_STOPS).find(|&i| spectrum_stop(i) == tint) else {
        return tint;
    };
    // `env ∈ [0.55, 1]` → `[−1, 1]`.
    let breath = clamp01((env - 0.55) / 0.45) * 2.0 - 1.0;
    let step = (breath * HALO_HUE_SWING_ENTRIES as f32).round() as i32;
    let at = (SPECTRUM_ANCHOR_AT[stop] as i32 + step).clamp(0, SPECTRUM_LUT_LEN as i32 - 1);
    SPECTRUM_LUT[at as usize]
}

/// **THE GROUND UNDER A STAR** — the theme ground plus everything the sky
/// has already laid on this frame at `(x, y)`: the additive quads of the
/// stars drawn before this one (`add_sat`, as `draw_flat_add` lays them)
/// and every `Add` halo — the aurora's veils and the earlier stars'
/// atmospheres — through the renderers' own falloff bytes (`halo_row_ny` /
/// `halo_weight`, the halo-parity contract). It is what [`price_centre`]
/// prices against, so a hero standing in a full-momentum veil, or dealt
/// onto the cell another star already holds, is held to the SAME ceiling
/// as one on bare glass. `laid` and `sky` are the slices from this frame's
/// first sky quad and halo — bounded by [`STARDUST_QUAD_BUDGET`] and
/// [`AURORA_HALO_BUDGET`] + [`STARDUST_HALO_BUDGET`].
///
/// What a star drawn LATER lays on this one's pixels is not here — its
/// arms and its halo's tail land after this star is priced and settled —
/// so the ceiling is exact on each star's own watched pixels over the sky
/// under it, and a pair the deal puts within a halo's reach of each other
/// is bounded by the later star's tail on the earlier one's pixels.
#[must_use]
fn sky_ground(bg: u32, laid: &[GlowQuad], sky: &[RainHalo], x: i32, y: i32) -> u32 {
    let mut ground = bg & 0x00FF_FFFF;
    for q in laid {
        if quad_covers(q, x, y) {
            ground = add_sat(ground, q.color);
        }
    }
    for h in sky {
        if h.mode == HaloMode::Add {
            ground = add_sat(ground, halo_add_at(h, x, y));
        }
    }
    ground
}

/// What one `Add` halo lays on `(x, y)` — its premultiplied colour through
/// the renderers' own elliptical falloff (`halo_row_ny` / `halo_weight`,
/// the halo-parity contract), `0` outside its box or its reach.
#[must_use]
fn halo_add_at(h: &RainHalo, x: i32, y: i32) -> u32 {
    if !(i32::from(h.x)..i32::from(h.x) + i32::from(h.w)).contains(&x)
        || !(i32::from(h.y)..i32::from(h.y) + i32::from(h.h)).contains(&y)
    {
        return 0;
    }
    let ny = halo_row_ny(y - i32::from(h.cy), i32::from(h.ry) * i32::from(h.ry));
    if ny >= 256 {
        return 0;
    }
    let wt = halo_weight(x - i32::from(h.cx), ny, i32::from(h.rx) * i32::from(h.rx));
    if wt <= 0 {
        return 0;
    }
    premul_rgb(h.color, wt.min(255) as u8)
}

/// Does this additive quad cover `(x, y)`?
#[inline]
#[must_use]
fn quad_covers(q: &GlowQuad, x: i32, y: i32) -> bool {
    q.alpha == 0
        && (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&x)
        && (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y)
}

/// The pixels [`settle_under_ceiling`] watches on one star: its centre, the
/// first pixel of each arm past the body (the stub, the taper's brightest
/// span and the halo's ring meet there), and its four orthogonal
/// neighbours (the disc's and the nucleus's pixels).
pub const SETTLE_PIXELS: usize = 9;

/// **THE CLOSER** ([`STAR_LIGHT_CEIL`]) — one star, just drawn, read back
/// from its own quads `frame.out[q0..]` and its own halo `frame.halos[h0..]`
/// (one `RainHalo` per text row it spans — `push_halo_quads` splits on the
/// row, so a pixel is under exactly one piece) at each of the `watch`ed
/// pixels (each carried with the sky already under it), and
/// DIMMED BY ONE FACTOR until every one of them composites under the
/// ceiling. The factor is found on the bytes themselves: a probe scales
/// every own lay by `k/255` through `premul_rgb` — the very rounding the
/// scaled quads are then written with — so what is checked is what is
/// drawn. Nothing moves when the star already fits, which on the shipped
/// ground at the default intensity is every star but a yellow or green
/// hero at its twinkle peak; a star that does not is the same star at a
/// lower light — body, disc, stubs and halo in their designed ratio,
/// colour and saturation intact.
///
/// Bounded: ≤ 8 probes × [`SETTLE_PIXELS`] × the star's own quads (≤ 15)
/// and halo pieces (one per row spanned); no allocation.
fn settle_under_ceiling(
    frame: &mut Frame<'_>,
    q0: usize,
    h0: usize,
    watch: &[((i32, i32), u32); SETTLE_PIXELS],
) {
    let own_at = |k: u8, (x, y): (i32, i32)| -> u32 {
        let mut own = 0u32;
        for q in &frame.out[q0..] {
            if quad_covers(q, x, y) {
                own = add_sat(own, premul_rgb(q.color, k));
            }
        }
        for h in &frame.halos[h0..] {
            let piece = RainHalo {
                color: premul_rgb(h.color, k),
                ..*h
            };
            own = add_sat(own, halo_add_at(&piece, x, y));
        }
        own
    };
    let fits = |k: u8| {
        watch
            .iter()
            .all(|&(p, ground)| under_ceil(add_sat(ground, own_at(k, p))))
    };
    let k = fit_byte(u8::MAX, fits);
    if k == u8::MAX {
        return;
    }
    for q in &mut frame.out[q0..] {
        q.color = premul_rgb(q.color, k);
    }
    for h in &mut frame.halos[h0..] {
        h.color = premul_rgb(h.color, k);
    }
}

/// Under [`STAR_LIGHT_CEIL`]?
#[inline]
#[must_use]
fn under_ceil(px: u32) -> bool {
    relative_luminance(px) <= STAR_LIGHT_CEIL
}

/// What `push_twinkle_star` stacks on the crossing pixel at coverage `cov`
/// in `rgb` — the two crossing bodies and the nucleus, in its own bytes
/// (`premul_rgb(rgb, cov)` twice and `premul_rgb(rgb, (cov · 0.35) as u8)`
/// once), over `base`. The recipe's `STAR_STACK_ADD 2.35`, integer-exact.
#[must_use]
fn stacked_centre(base: u32, rgb: u32, cov: u8) -> u32 {
    let lay = premul_rgb(rgb, cov);
    let core = premul_rgb(rgb, (f32::from(cov) * STAR_CORE_ADD) as u8);
    add_sat(add_sat(add_sat(base, lay), lay), core)
}

/// The largest `k ≤ want` for which `under(k)` holds — `under` is monotone
/// (more light is never less luminance), so this is a bisection over the
/// byte, at most eight probes, none of them an allocation. `0` when even
/// nothing passes.
#[must_use]
fn fit_byte(want: u8, under: impl Fn(u8) -> bool) -> u8 {
    if under(want) {
        return want;
    }
    if !under(0) {
        return 0;
    }
    let (mut lo, mut hi) = (0u8, want);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if under(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// **THE CARET LAW, SPENT ON ONE STAR'S CENTRE** ([`STAR_LIGHT_CEIL`]).
///
/// `body` is the crossing's colour and requested coverage (stacked as
/// `push_twinkle_star` stacks it), `disc` the m2's 3×3 shoulder (`0` for
/// the hero), `halo` the atmosphere's colour and peak — the peak IS added
/// on the centre pixel, the falloff weighing `256/256` there. Returns what
/// each may be so that the composited centre over `ground` stays under the
/// ceiling:
///
/// * **a COLOURED star spends colour first** — the halo's whole request,
///   then the disc's, and the white body takes the luminance left. That is
///   "saturation costs no luminance" made exact: the centre lands at the
///   same `Y` it may have anyway, and reads as the tint rather than as
///   white. On the shipped ground at the default intensity the hero's
///   red, blue, indigo and violet halos fit beside the whole body; a
///   yellow or green one costs the body about a quarter, because those
///   stops are most of the eye's luminance, and at the full intensity —
///   where even the bare white `143` is over the ceiling on that ground —
///   the body is what the colour leaves (measured, per stop, by
///   `the_caret_law_spends_colour_first_and_white_last_on_a_star_s_centre`);
/// * **an ACHROMATIC star spends its body first** — the A-type white's
///   halo is more white, exactly the light the law is about, so it has the
///   remainder: `16` levels on the shipped ground at the default intensity
///   where it asked `43`, and one level over black at the published `143`;
/// * **a ground already over the ceiling leaves the request as it is** —
///   there is no answer on it, and the law there is the caret's to keep.
///
/// Every probe composites in the rasterizers' own integer arithmetic, so
/// the ceiling holds on the byte the eye gets, not on a model of it.
#[must_use]
fn price_centre(
    ground: u32,
    achromatic: bool,
    body: (u32, u8),
    disc: (u32, u8),
    halo: (u32, u8),
) -> (u8, u8, u8) {
    if !under_ceil(ground) {
        return (body.1, disc.1, halo.1);
    }
    let lay = |base: u32, rgb: u32, k: u8| add_sat(base, premul_rgb(rgb, k));
    if achromatic {
        let cov = fit_byte(body.1, |k| under_ceil(stacked_centre(ground, body.0, k)));
        let centre = stacked_centre(ground, body.0, cov);
        let d = fit_byte(disc.1, |k| under_ceil(lay(centre, disc.0, k)));
        let centre = lay(centre, disc.0, d);
        let peak = fit_byte(halo.1, |k| under_ceil(lay(centre, halo.0, k)));
        (cov, d, peak)
    } else {
        let peak = fit_byte(halo.1, |k| under_ceil(lay(ground, halo.0, k)));
        let lit = lay(ground, halo.0, peak);
        let d = fit_byte(disc.1, |k| under_ceil(lay(lit, disc.0, k)));
        let lit = lay(lit, disc.0, d);
        let cov = fit_byte(body.1, |k| under_ceil(stacked_centre(lit, body.0, k)));
        (cov, d, peak)
    }
}

/// ONE LIGHT-THEME STAR, as [`push_twinkle_over_twin`] takes it — the
/// star's own five numbers, folded into a record the way [`Deal`] and
/// [`Throw`] fold a birth: `push_twinkle_star`'s parameter set minus the quad
/// budget the ink twin does not spend, so the two halves of one silhouette
/// still read as one call without an eight-argument signature.
#[derive(Clone, Copy, Debug)]
struct LightStar {
    /// Device-pixel centre.
    centre: (i32, i32),
    /// The arm half-length in px, already taxed by `STAR_ARM_INK` (§3.3).
    arm: f32,
    /// The star's tint — the ink roles derive their deep and over-text inks
    /// from it.
    rgb: u32,
    /// Whether to lay the four diagonal glint dots (gold, at twinkle peak).
    gold: bool,
    /// The star's additive coverage request, as a byte.
    cov: u8,
}

/// The light-theme STAR — the `push_twinkle_over` silhouette (§3.3, §5.2's
/// light twins): a horizontal and a vertical arm, each a full-length TIP lay
/// and a shorter BODY lay, on a round nucleus, plus the four diagonal glint
/// dots when gold — every one a source-over falloff ellipse. The nucleus is
/// laid FIRST and in the tint's deep ink (`InkRole::Leading`); the arms take
/// the over-text ink (`InkRole::OverText`), because they can composite over a
/// letterform at any moment and 190 is the legibility budget.
///
/// The per-lay alpha solves for the family's THREE-lay centre stack
/// (`STAR_OVER_CENTRE_LAYS` of `STAR_OVER_LAYS`), so the crossing is
/// byte-for-byte what the plus put on screen and the taper shows up as the
/// points. The star itself arrives as one [`LightStar`] record.
fn push_twinkle_over_twin(halos: &mut Vec<RainHalo>, geom: Geom, cap: usize, star: LightStar) {
    let LightStar {
        centre,
        arm,
        rgb,
        gold,
        cov,
    } = star;
    if cov == 0 || arm < 0.75 {
        return;
    }
    let lay_of = |role: InkRole| -> u32 {
        let cap_a = u32::from(cov).clamp(1, role.alpha_cap() as u32);
        let a = cap_a as f32 / 255.0;
        let lay = (1.0 - (1.0 - a).powf(STAR_OVER_CENTRE_LAYS / STAR_OVER_LAYS as f32)) * 255.0;
        (role.ink(rgb) & 0x00FF_FFFF) | ((lay.round() as u32).clamp(1, cap_a) << 24)
    };
    let core_ink = lay_of(InkRole::Leading);
    let arm_ink = lay_of(InkRole::OverText);
    let px = |v: f32| (v.round() as i32).max(1);
    let waist = px((arm * STAR_WAIST).max(1.0));
    let body = px((arm * STAR_TAPER_BODY).max(1.0));
    let core = px((arm * STAR_CORE).max(1.0));
    let arm_px = px(arm);
    // Nucleus first, so a star truncated by the halo budget keeps its centre.
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (core, core),
        core_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (arm_px, waist),
        arm_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (waist, arm_px),
        arm_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (body, waist),
        arm_ink,
        HaloMode::Over,
    );
    push_halo_quads(
        halos,
        geom,
        cap,
        centre,
        (waist, body),
        arm_ink,
        HaloMode::Over,
    );
    if gold {
        // The classic four-point sparkle's secondary points, dim — the same
        // accent the additive gold star throws, so the WARM half of the
        // palette is the same mark on both grounds.
        let d = px(arm * STAR_GLINT);
        let gcap = ((f32::from(cov) * STAR_GLINT_COV) as u32)
            .clamp(1, InkRole::OverText.alpha_cap() as u32);
        let glint = (InkRole::OverText.ink(rgb) & 0x00FF_FFFF) | (gcap << 24);
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            push_halo_quads(
                halos,
                geom,
                cap,
                (centre.0 + ox, centre.1 + oy),
                (1, 1),
                glint,
                HaloMode::Over,
            );
        }
    }
}

/// The over-alpha a ROUND light grain is priced at so it composites to the
/// three-lay plus it replaced (`cursor_glow`'s `stacked_ink_alpha`, restated
/// on the family's `STAR_OVER_CENTRE_LAYS`): `1 − (1 − a)³`, bounded by the
/// same stack of the role's own cap.
fn stacked_ink_alpha(cov: u8, role: InkRole) -> u32 {
    let lays = STAR_OVER_CENTRE_LAYS;
    let a1 = f32::from(cov.min(role.alpha_cap() as u8)) / 255.0;
    let cap1 = role.alpha_cap() / 255.0;
    let ceiling = (1.0 - (1.0 - cap1).powf(lays)) * 255.0;
    ((1.0 - (1.0 - a1).powf(lays)) * 255.0).clamp(1.0, ceiling) as u32
}

// ===========================================================================
// 9. The laws, measured (§20.1)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::super::Flow;
    use super::super::spine::FLOW_ENTRY_KEYS;
    use super::*;
    use crate::rainbow_kitty::ribbon::RETRACT_START_S;
    use crate::rainbow_kitty::{KillScope, Mend};
    use aterm_render::add_sat;
    use std::time::Duration;

    /// A 120×40 grid of 9×18 cells with a 40 px chrome band above it, so the
    /// sky band of ROW 0 is inside the effects box and the off-grid arm of
    /// §5.4's probe gate is exercisable.
    fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 40,
            win_w: 1080,
            win_h: 760,
            head: 40,
        }
    }

    fn cfg(dark: bool) -> Config {
        Config {
            dark_theme: dark,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            theme_fg: 0x00FF_FFFF,
            theme_bg: 0x0000_0000,
            reduced_motion: false,
        }
    }

    /// [`geom`] at another cell — the same 120×40 grid under the same 40 px
    /// chrome band, at `cw × ch` device px: the owner's 7×14 (1×) and 15×28
    /// (retina) cells for the optical-size law.
    fn geom_cell(cw: usize, ch: usize) -> Geom {
        Geom {
            cw,
            ch,
            win_w: (120 * cw) as u16,
            win_h: (40 + 40 * ch) as u16,
            ..geom()
        }
    }

    fn ctx(now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
        ctx_in(geom(), now, cfg, caret)
    }

    /// [`ctx`] over a chosen geometry.
    fn ctx_in(geom: Geom, now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
        Ctx {
            now,
            geom,
            cfg,
            disp: 0.4,
            birth_disp: 0.4,
            phase: 0.0,
            caret,
            caret_t: 0.5,
            mend: None,
            surge: 0.0,
            flow: Default::default(),
        }
    }

    /// The borrowed host scratch a [`Frame`] is built over.
    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<aterm_render::BeamVertex>,
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
                caret: super::super::CaretSeam::default(),
                companion: None,
                fp: 0,
            }
        }
    }

    /// The COMPOSITE at one window pixel — the renderers' own arithmetic, so a
    /// test measures the pixel the eye gets and not the request that made it.
    fn px(quads: &[GlowQuad], x: i32, y: i32) -> u32 {
        let mut dst = 0u32;
        for q in quads {
            let inside = x >= i32::from(q.x)
                && x < i32::from(q.x) + i32::from(q.w)
                && y >= i32::from(q.y)
                && y < i32::from(q.y) + i32::from(q.h);
            if !inside {
                continue;
            }
            if q.alpha == 0 {
                dst = add_sat(dst, q.color);
            } else {
                let a = u32::from(q.alpha);
                let ch = |sh: u32| {
                    let d = (dst >> sh) & 0xff;
                    (((q.color >> sh) & 0xff) + d * (255 - a) / 255).min(255)
                };
                dst = (ch(16) << 16) | (ch(8) << 8) | ch(0);
            }
        }
        dst
    }

    /// The brightest channel — the "peak luminance" the capture measured.
    fn lum(p: u32) -> u32 {
        ((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff)
    }

    /// A seed whose luminance phase is AT ITS PEAK on frame 0.
    ///
    /// The rectified envelope peaks every `π/9` s ≈ 349 ms and an m3 lives
    /// 225 ms, so a sweep of a grain's whole life can MISS its own peak on an
    /// unlucky φ. Choosing the seed is not stacking the deck: the published
    /// centres of §5.2 are the values at `env = 1`, and the fixture has to be
    /// able to reach them for the peak law to say anything.
    fn peak_seed() -> u32 {
        (0u32..)
            .find(|&k| {
                (hash01(mix32(k ^ SALT_PHASE)) * std::f32::consts::TAU)
                    .sin()
                    .abs()
                    > 0.9995
            })
            .expect("some seed puts the luminance clock at its peak")
    }

    /// One star, hand-built at a known pixel with a known tint — the fixture
    /// every per-class law drives. Gold iff the tint is gold, as the deal
    /// would have it.
    fn star(class: StarClass, lane: StarLane, born: Instant, tint: u32) -> Star {
        Star {
            class,
            lane,
            x: 200.0,
            y: 100.0,
            home_row: None,
            born,
            lit: born,
            seed: peak_seed(),
            tint,
            gold: tint == TINT_GOLD_RGB,
            v0: (0.0, 0.0),
            f: 10.0,
            stream: 0.0,
            pull: 0.0,
            finish: None,
        }
    }

    /// A sky whose rows 2-4 are probed BLANK — the rows the fixture star at
    /// `y = 100` (row 3) and its neighbours live in — so §5.2's text-safe
    /// third does not bite what the per-class laws measure.
    fn sky() -> Stardust {
        let mut sky = Stardust::new();
        for row in 2..=4 {
            sky.probe_mut().probe_row(row, &[false; 120]);
        }
        sky
    }

    /// Emit one frame of a pool at `now` and hand back the scratch.
    fn frame_at(sky: &mut Stardust, now: Instant, cfg: &Config) -> Scratch {
        let mut sc = Scratch::default();
        let mut f = sc.frame();
        sky.emit(&ctx(now, cfg, (3, 10)), &mut f);
        sc
    }

    /// §5.2's OWN composited centres, as literals — the oracle the code's
    /// `centre()` is measured against, not the other way round.
    fn every_class() -> [(StarClass, StarLane, f32); 6] {
        [
            (StarClass::M1, StarLane::Strike, 143.0),
            (StarClass::M2, StarLane::Strike, 130.0),
            (StarClass::M3, StarLane::Strike, 98.0),
            (StarClass::M1, StarLane::Fan, 118.0),
            (StarClass::M2, StarLane::Fan, 80.0),
            (StarClass::M3, StarLane::Fan, 61.0),
        ]
    }

    fn typed(class: TypedClass) -> Event {
        Event::Typed {
            cells: 1,
            shifted: matches!(class, TypedClass::Capital),
            class,
        }
    }

    fn col_of(s: &Star) -> i32 {
        px_col(s.x, geom())
    }

    fn dist(a: (i32, i32), b: (i32, i32)) -> f32 {
        ((a.0 - b.0) as f32).hypot((a.1 - b.1) as f32)
    }

    /// The key instant of key `k` at exactly 12 cps.
    fn key_at(t0: Instant, k: u16) -> Instant {
        t0 + Duration::from_micros(u64::from(k) * 83_333)
    }

    /// The spine the engine's own integrator has reached by key `k` of a
    /// sustained 12 cps run — `TypingMomentum`'s published build curve,
    /// `RATE · τ · (1 − e^(−t/τ))` = `1.3 · (1 − e^(−t/2))` clamped at 1
    /// (0.45 after one second, 0.75 at ~1.7 s, 1.0 at ~2.9 s) — so a
    /// producer test prices its births on the number the seam would hand it
    /// rather than on a constant that never crosses the second grain's 0.5.
    fn disp_at_12cps(k: u16) -> f32 {
        let t = f32::from(k) / 12.0;
        (1.3 * (1.0 - (-t / 2.0).exp())).min(1.0)
    }

    /// The max − min channel gap of a premultiplied colour — the judge's
    /// "chroma", read off one quad or halo.
    fn chroma(p: u32) -> u32 {
        let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
        r.max(g).max(b) - r.min(g).min(b)
    }

    /// **D1 — A STAR IS A PEAK, NOT A BLOB** (§5.1 no. 1, §20.1).
    ///
    /// The whole of what separates stardust from dust, measured on the
    /// composited pixel: the centre exceeds every one of its eight neighbours
    /// by ≥ 30 %, and it reaches §5.2's PUBLISHED centre — 143/130/98 in the
    /// sky, 118/80/61 in the transient lane — two-sided, because the ceiling
    /// is a law too. v1's `push_dust_mote` was a `d×d` square on a
    /// `ceil(d/2)` square — flat coverage, a centre-to-neighbour ratio of 1.0
    /// over the whole skirt, and a measured peak of 70/255.
    ///
    /// Re-derived for the art polish: the grain's disc is now tinted and
    /// outboard at `0.45·c` (98 : 27 at the crest, 54 : 27 at the trough) —
    /// and again on 2026-09-08, when the grain's core and the m2's body took
    /// the tint: the centre is measured on its BRIGHTEST channel, which is
    /// the tint's own at full, so the two-sided centre and the 30 % margin
    /// over every 8-neighbour hold unchanged; §3.2 rank 8 is now "the halo
    /// may not outshine the core" (61 under 143, 38 under 130) and is
    /// measured here as such.
    #[test]
    fn a_star_is_a_peak_not_a_blob() {
        let c = cfg(true);
        let t0 = Instant::now();
        for (class, lane, want) in every_class() {
            let mut sky = sky();
            sky.sow(star(class, lane, t0, TINT_WHITE_RGB));
            // Sweep the life: the luminance clock is seeded, so the peak frame
            // has to be found rather than assumed.
            let life_ms = (class.life_s(lane) * 1000.0) as u64;
            let mut best = (0u32, 0u64);
            for ms in 0..life_ms {
                let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
                let v = lum(px(&sc.out, 200, 100));
                if v > best.0 {
                    best = (v, ms);
                }
            }
            assert!(
                (best.0 as f32 - want).abs() <= 1.0,
                "{class:?}/{lane:?} centre {} is not §5.2's {want}",
                best.0
            );
            // §20.1's floor: every class ≥ 80/255, the transient grain ≥ 61 —
            // with the same one byte of coverage-grid slack the centre has:
            // the family's `push_twinkle_star` truncates its nucleus byte
            // (`(34 · 0.35) as u8 = 11`), so the transient m2's `2.35 · 34`
            // lands on 79 and no request reaches 80 or 81 exactly.
            let floor = if class == StarClass::M3 && lane.is_transient() {
                61.0
            } else {
                80.0
            };
            assert!(
                best.0 as f32 >= floor - 1.0,
                "{class:?}/{lane:?} peak {} under the {floor}/255 floor",
                best.0
            );
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(best.1), &c);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let n = lum(px(&sc.out, 200 + dx, 100 + dy));
                    assert!(
                        best.0 as f32 >= 1.3 * n as f32,
                        "{class:?}/{lane:?} neighbour ({dx},{dy}) at {n} is not 30 % under {}",
                        best.0
                    );
                }
            }
            // §3.2 rank 8, as re-ruled 2026-09-08 (owner: "I want ... a
            // bigger more special rainbow impact"): the halo may not
            // OUTSHINE the core it belongs to — under the old 14 % no halo
            // pixel ever read as colour on glass.
            for h in &sc.halos {
                assert!(
                    lum(h.color) < best.0,
                    "{class:?}/{lane:?} halo peak {} outshines the core {}",
                    lum(h.color),
                    best.0
                );
            }
        }
    }

    /// **THE STARS CARRY THE RAINBOW — THE GRAIN IS PURE TINT, THE m2 IS
    /// TINTED TO ITS CORE, THE HERO KEEPS A WHITE-HOT CENTRE UNDER A RAINBOW
    /// HALO** (§5.2 / §5.3 as re-ruled 2026-09-08, §20.1).
    ///
    /// Re-pinned from `the_star_body_is_white_and_only_the_halo_and_stubs_are_tinted`
    /// (D2: "the peak is white in every class") on the owner's words: *"I
    /// feel like you are diminishing the specialness and emphasis of this
    /// theme? why?"* — *"make this rainbow theme truly magical and special
    /// and dynamic and beautiful"*. D2 was the law that kept 93 % of the
    /// sky's pixels grey under a tint no eye found. Now, on the composited
    /// pixel: an m3's centre and every one of its quads is the pure stop
    /// (`spread == max channel`); an m2's centre carries `M2_BODY_TINT` of
    /// the tint (spread `≥ 0.5·max` — the body, the disc and the stubs are
    /// all tinted, no quad of it is grey); an m1's centre stays white in
    /// its quads (`r == g == b`) with exactly its four tinted stubs outside
    /// the body — its colour is the halo, measured by
    /// `stardust_is_coloured_on_the_glass_not_white_with_a_tint`.
    #[test]
    fn the_grain_and_the_m2_wear_their_tint_to_the_core_and_the_hero_stays_white_hot() {
        let c = cfg(true);
        let t0 = Instant::now();
        for (class, lane, _) in every_class() {
            let mut sky = sky();
            sky.sow(star(class, lane, t0, 0x0000_00FF));
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(1), &c);
            let centre = px(&sc.out, 200, 100);
            let (r, g, b) = ((centre >> 16) & 0xff, (centre >> 8) & 0xff, centre & 0xff);
            let grey = sc.out.iter().filter(|q| chroma(q.color) == 0).count();
            match class {
                StarClass::M3 => {
                    assert!(
                        r == 0 && g == 0 && b > 0,
                        "{class:?}/{lane:?} centre {centre:#08x} is not the pure stop"
                    );
                    assert_eq!(grey, 0, "{class:?}/{lane:?} carries a grey quad");
                }
                StarClass::M2 => {
                    assert!(
                        chroma(centre) * 2 >= lum(centre) && b > r && r == g,
                        "{class:?}/{lane:?} centre {centre:#08x} is not tinted to its core"
                    );
                    assert_eq!(grey, 0, "{class:?}/{lane:?} carries a grey quad");
                }
                StarClass::M1 => {
                    assert!(
                        r == g && g == b && r > 0,
                        "{class:?}/{lane:?} centre {centre:#08x} is not white-hot"
                    );
                    let mut tinted = 0usize;
                    for q in sc.out.iter().filter(|q| chroma(q.color) > 0) {
                        tinted += 1;
                        // Chebyshev distance from the centre to the quad's
                        // NEAREST pixel — the stubs lie outside the body.
                        let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w) - 1);
                        let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h) - 1);
                        let dx = (x0 - 200).max(200 - x1).max(0);
                        let dy = (y0 - 100).max(100 - y1).max(0);
                        assert!(
                            dx.max(dy) >= 2,
                            "{class:?}/{lane:?} tinted {}x{} quad at ({dx},{dy}) is inside the body",
                            q.w,
                            q.h
                        );
                    }
                    assert_eq!(
                        tinted, 4,
                        "{class:?}/{lane:?} carries {tinted} stubs, not four"
                    );
                }
            }
        }
    }

    /// **D12 — A GRAIN OPENS AND CLOSES** (§5.1 no. 3, §5.5, §20.1).
    ///
    /// The second clock, and the one v1 did not have. An m3 lives 225 ms; at
    /// its seeded 8-12 Hz that is ≈ 2.3 open-and-close cycles, and the arm is
    /// INTEGER-STEPPED so the opening is a visible 1 → 2 → 1 px rather than a
    /// continuous breath the eye integrates away.
    ///
    /// Measured off the drawn bar, not off any internal: the cross's own width
    /// is the arm, and it must change at least four times in the grain's life.
    #[test]
    fn a_grain_opens_and_closes_at_least_twice_in_its_life() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        sky.sow(star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB));
        let life_ms = (StarClass::M3.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut widths = Vec::new();
        for ms in 0..life_ms {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            let w = sc.out.iter().map(|q| q.w).max().unwrap_or(0);
            if widths.last() != Some(&w) {
                widths.push(w);
            }
        }
        assert!(
            widths.len() >= 5,
            "the grain changed size {} times in {life_ms} ms: {widths:?}",
            widths.len() - 1
        );
        // §5.5's photosensitivity exemption is an AREA bound: a mark that
        // scintillates in SIZE must stay inside 10×10 device px at 2×.
        let widest = widths.iter().copied().max().unwrap_or(0);
        assert!(
            i32::from(widest) * 2 <= SCINT_BBOX_MAX_PX,
            "the grain's bbox {widest} px is outside the size-scintillation exemption at 2x"
        );
    }

    /// **§5.5 — AN m2 NEVER COLLAPSES TO THE GRAIN'S ARM**. The published
    /// band is 2 ↔ 3 px; the clock's formula alone rounds to 1 at its trough
    /// for a fifth of every cycle, and a 1-px arm is an m3's silhouette, so
    /// the m2 ↔ m3 step would flicker. Measured off the drawn extent.
    #[test]
    fn an_m2_arm_never_collapses_to_the_grain() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        // A coloured tint, so the stubs and the disc (pure tint) are not
        // counted into the BODY's extent below — the body is
        // `M2_BODY_TINT` of the way to the tint, a colour of its own.
        let tint = spectrum_snap(0.2);
        let body = mix_rgb(TINT_WHITE_RGB, tint, M2_BODY_TINT);
        sky.sow(star(StarClass::M2, StarLane::Strike, t0, tint));
        let life_ms = (StarClass::M2.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut extents = std::collections::BTreeSet::new();
        for ms in 0..life_ms {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            // The body's horizontal extent on the centre row.
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for q in sc.out.iter().filter(|q| {
                i32::from(q.y) == 100 && q.color == premul_rgb(body, lum(q.color) as u8)
            }) {
                lo = lo.min(i32::from(q.x));
                hi = hi.max(i32::from(q.x) + i32::from(q.w));
            }
            let extent = hi - lo;
            assert!(
                extent > 2 * StarClass::M2.arm_min_px(geom().ch as f32),
                "at {ms} ms the m2's arm collapsed to a {extent}-px extent"
            );
            extents.insert(extent);
        }
        assert!(
            extents.len() >= 2,
            "the m2 arm never scintillated: {extents:?}"
        );
    }

    /// **C3 — A STAR NEVER GREYS** (§5.1 no. 5, §3.2, §20.1).
    ///
    /// Chroma is fixed at birth and only coverage moves, so every colour a
    /// star ever emits is its own tint premultiplied by SOME coverage — never
    /// a blend toward white or toward the ground. The measured saturation p50
    /// of 7-8 was the sound of v1 doing the opposite.
    #[test]
    fn stars_fade_by_alpha_at_a_snapped_stop() {
        let c = cfg(true);
        let t0 = Instant::now();
        let tint = spectrum_snap(0.62);
        let mut sky = sky();
        sky.sow(star(StarClass::M1, StarLane::Strike, t0, tint));
        let life_ms = (StarClass::M1.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut last_lit = 0u64;
        for ms in 0..life_ms {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            if !sc.out.is_empty() {
                last_lit = ms;
            }
            for q in &sc.out {
                let ok = (0..=255u16).any(|k| premul_rgb(tint, k as u8) == q.color)
                    || (0..=255u16).any(|k| premul_rgb(TINT_WHITE_RGB, k as u8) == q.color);
                assert!(
                    ok,
                    "quad colour {:#08x} is not {tint:#08x} or white at some coverage",
                    q.color
                );
            }
            // The halo BREATHES its hue (owner, 2026-09-08: "the twinkle
            // has more colour in it"): its colour is a whole entry of the
            // arc within `HALO_HUE_SWING_ENTRIES` of the stop's own anchor —
            // on the spectrum, and never a neighbouring name.
            let anchor = SPECTRUM_ANCHOR_AT[spectrum_snap_index(0.62)] as i32;
            for h in &sc.halos {
                let ok =
                    (anchor - HALO_HUE_SWING_ENTRIES..=anchor + HALO_HUE_SWING_ENTRIES).any(|j| {
                        let rgb = SPECTRUM_LUT[j as usize];
                        (0..=255u16).any(|k| premul_rgb(rgb, k as u8) == h.color)
                    });
                assert!(ok, "halo colour {:#08x} left the stop's breath", h.color);
            }
            // …and both ends of the breath still snap to the star's own name.
            let name = |j: i32| spectrum_snap_index(j as f32 / (SPECTRUM_LUT_LEN - 1) as f32);
            assert_eq!(name(anchor - HALO_HUE_SWING_ENTRIES), name(anchor));
            assert_eq!(name(anchor + HALO_HUE_SWING_ENTRIES), name(anchor));
        }
        // D3: the last frame a star draws is still a visible point.
        let sc = frame_at(&mut sky, t0 + Duration::from_millis(last_lit), &c);
        assert!(
            lum(px(&sc.out, 200, 100)) >= 20,
            "the star greyed out before it was culled"
        );
        // …and one frame past the life it is gone, not left as a speck.
        let sc = frame_at(&mut sky, t0 + Duration::from_millis(life_ms + 1), &c);
        assert!(sc.out.is_empty() && sky.at_rest());
    }

    /// **§3.2 — NOTHING COLOURED IS DRAWN BELOW α 0.12**. The halo rides the
    /// star's alpha times its twinkle; at a twinkle trough near the cull a
    /// star at α 0.21 would put an 0.11 halo on glass. It is culled instead,
    /// while the white core (its own floor is 0.20) still draws.
    #[test]
    fn a_halo_is_culled_below_the_chroma_floor() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        let mut s = star(StarClass::M1, StarLane::Strike, t0, spectrum_snap(0.2));
        // A seed whose twinkle TROUGH meets the end of the life, so the band
        // `α ∈ [0.20, 0.218)` where the core still draws and the halo must
        // not is actually swept.
        let life = StarClass::M1.life_s(StarLane::Strike);
        s.seed = (0u32..)
            .find(|&k| {
                s.seed = k;
                s.twinkle(t0 + Duration::from_secs_f32(life - 0.004)) < 0.58
            })
            .expect("some seed puts the trough at the life's end");
        sky.sow(s);
        let life_ms = (life * 1000.0) as u64;
        let mut culled_while_core_drawn = false;
        for ms in 0..life_ms {
            let now = t0 + Duration::from_millis(ms);
            let sc = frame_at(&mut sky, now, &c);
            let s = sky.live_iter().next().expect("the hero is live");
            let a = s.envelope(now, false) * s.twinkle(now);
            if a < CHROMA_CULL_ALPHA {
                assert!(
                    sc.halos.is_empty(),
                    "a halo at α·env {a} is under the chroma floor"
                );
                if !sc.out.is_empty() {
                    culled_while_core_drawn = true;
                }
            }
        }
        assert!(
            culled_while_core_drawn,
            "the sweep never reached the band where the halo culls before the core"
        );
    }

    /// **§5.5 — WARM AND COOL NEIGHBOURS NEVER BLINK TOGETHER**.
    ///
    /// `φ = hash(row, col, seed)·2π`, and the cool half of the spectrum takes
    /// `φ + π`. Two stars on the same seed and opposite temperatures are
    /// therefore exactly antiphase, which is what makes a field read as a
    /// scatter of independent lights rather than as one blinking texture.
    #[test]
    fn warm_and_cool_neighbours_never_blink_together() {
        let t0 = Instant::now();
        for (warm_i, cool_i) in [(0usize, 3usize), (1, 4), (2, 6)] {
            let w = star(StarClass::M2, StarLane::Strike, t0, spectrum_stop(warm_i));
            let cl = star(StarClass::M2, StarLane::Strike, t0, spectrum_stop(cool_i));
            assert!(
                w.is_warm() && !cl.is_warm(),
                "stops {warm_i}/{cool_i} mis-classified"
            );
            let d = (cl.lum_phase() - w.lum_phase()).abs();
            assert!(
                (d - std::f32::consts::PI).abs() < 0.05,
                "phase difference {d} is not π"
            );
        }
        // The A-type white and the gold belong to neither half and take the
        // base phase — they have no neighbour they could beat against.
        let a = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
        let g = star(StarClass::M1, StarLane::Strike, t0, TINT_GOLD_RGB);
        assert!(a.is_warm() && g.is_warm());
        assert!((a.lum_phase() - g.lum_phase()).abs() < f32::EPSILON);
    }

    /// **§5.3 — GOLD IS DEALT, NOT INFERRED FROM YELLOW**. The arc's yellow
    /// stop is the same RGB as gold; a field star wearing it is not a gold
    /// star and never throws glints, while a dealt gold one does at its
    /// twinkle peak.
    #[test]
    fn gold_is_dealt_not_inferred_from_yellow() {
        let t0 = Instant::now();
        let yellow = spectrum_stop(2);
        assert_eq!(
            yellow, TINT_GOLD_RGB,
            "the fixture relies on yellow == gold RGB"
        );
        let mut field = star(StarClass::M1, StarLane::Strike, t0, yellow);
        field.gold = false;
        let gold = star(StarClass::M1, StarLane::Strike, t0, yellow);
        assert!(gold.gold);
        let life_ms = (StarClass::M1.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut gold_glinted = false;
        for ms in 0..life_ms {
            let now = t0 + Duration::from_millis(ms);
            assert!(
                !field.glinting(now),
                "a yellow field star glinted at {ms} ms"
            );
            gold_glinted |= gold.glinting(now);
        }
        assert!(gold_glinted, "a gold hero never glinted in its life");
        // The deal itself says which: over many seeds, the 15 % branch alone
        // returns gold, and every gold answer wears the gold RGB.
        let mut golds = 0;
        for seed in 0..1000u32 {
            let (tint, gold) = deal_tint(seed, 0.33);
            if gold {
                golds += 1;
                assert_eq!(tint, TINT_GOLD_RGB);
            } else if tint == TINT_GOLD_RGB {
                // yellow from the arc's own stop
                assert_eq!(spectrum_snap(0.33), yellow);
            }
        }
        assert!(
            (100..=200).contains(&golds),
            "the gold deal is not ~15 %: {golds}/1000"
        );
    }

    /// **§5.4, THE RULE CLAUSE — A LIGHT HORIZONTAL RULE ABOVE THE INPUT
    /// LINE IS NOT A DARK SKY.** The owner's report (2026-09-08): *"I don't
    /// see cursor sparkles when I'm using claude code, but I see them for
    /// codex and normal terminal use."* Claude Code draws its input line
    /// directly under a full-width `─` rule; the host probed that row as
    /// 120 glyphs, the cell-level gate refused every sky birth, and the
    /// ribbon typed on with no star over it for the life of the session —
    /// measured live: `licensed=45 ribbon_active=true momentum=0.70
    /// v2_stars=0` against the shell's `v2_stars=5` for the same keys.
    ///
    /// The rule's ink is a stroke through the cell's centre; the sky zone
    /// is the lower `[0.60, 0.86]` of that cell. Disjoint — so the probe
    /// records the rule as a class of its own and the sky is born over it,
    /// with no core on the stroke. A row of TEXT above the line still
    /// darkens the sky (L4 stands), and so does a HEAVY rule, whose stroke
    /// can reach the zone on a square cell.
    #[test]
    fn a_light_rule_over_the_typing_row_keeps_the_sky_alive() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let ch = g.ch as f32;
        // Forty keys on row 5 under a probed row 4, as the host feeds it.
        let type_under = |row_4: &[CellInk]| -> Stardust {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row_ink(4, row_4);
            sky.probe_mut().probe_row(5, &[false; 120]);
            for k in 0..40u16 {
                sky.on_event(
                    &typed(TypedClass::Glyph),
                    t0,
                    &ctx(t0, &c, (5, 10 + k)),
                    &ribbon,
                );
            }
            sky
        };
        let row_of = |s: &str| -> Vec<CellInk> { s.repeat(120).chars().map(cell_ink).collect() };

        // 1. The TUI input box: a full-width light rule above the line.
        let rule = row_of("─");
        assert!(rule.iter().all(|&cell| cell == CellInk::Rule));
        let sky = type_under(&rule);
        assert!(
            sky.live() > 0,
            "a light rule above the input line darkened the sky"
        );
        for s in sky.live_iter() {
            assert_eq!(
                px_row(s.y, g),
                4,
                "a sky star was born outside row − 1, at ({}, {})",
                s.x,
                s.y
            );
            let fy = (s.y - f32::from(g.origin_y)) / ch - 4.0;
            assert!(
                fy >= RULE_INK_BOT_CH,
                "a star core sits in the rule's stroke band: fy = {fy}"
            );
            assert_eq!(
                sky.probe().at_px(s.x as i32, s.y as i32, g),
                Some(false),
                "the pixel probe called a sky pixel over the rule inked at ({}, {})",
                s.x,
                s.y
            );
        }
        // …and the same census as a BLANK row: the rule costs the sky nothing.
        let blank = type_under(&row_of(" "));
        assert_eq!(
            sky.live(),
            blank.live(),
            "the sky over a light rule is not the sky over a blank row"
        );

        // 2. A row of text above the line: L4, unchanged.
        assert_eq!(
            type_under(&row_of("x")).live(),
            0,
            "a row of text above the input line minted stars"
        );

        // 3. A heavy rule is text for this purpose.
        assert_eq!(
            type_under(&row_of("━")).live(),
            0,
            "a heavy rule above the input line minted stars"
        );
    }

    /// The probe's two grains, stated: over a [`CellInk::Rule`] the CELL is
    /// inked ([`GlyphProbe::at`]), the SKY is not ([`GlyphProbe::sky_at`]),
    /// and a PIXEL is inked exactly inside the stroke band
    /// ([`GlyphProbe::at_px`]). A glyph is inked at every grain, a blank at
    /// none, and an unprobed row is unknown at every grain.
    #[test]
    fn the_pixel_probe_answers_ink_only_inside_a_rules_stroke_band() {
        let g = geom();
        let mut probe = GlyphProbe::new();
        let mut row = vec![CellInk::Blank; 120];
        row[10] = CellInk::Rule;
        row[11] = CellInk::Glyph;
        probe.probe_row_ink(4, &row);
        // A window pixel `f` of the way down row 4.
        let y_at = |f: f32| (f32::from(g.origin_y) + (4.0 + f) * g.ch as f32) as i32;
        let (rx, _) = g.cell_center(4, 10);
        let (gx, _) = g.cell_center(4, 11);
        let (bx, _) = g.cell_center(4, 12);

        assert_eq!(probe.at(4, 10, g.rows), Some(true), "the rule cell has ink");
        assert_eq!(
            probe.sky_at(4, 10, g.rows),
            Some(false),
            "the rule's sky is clear"
        );
        assert_eq!(
            probe.at_px(rx as i32, y_at(0.50), g),
            Some(true),
            "the stroke"
        );
        assert_eq!(
            probe.at_px(rx as i32, y_at(0.10), g),
            Some(false),
            "above the stroke"
        );
        assert_eq!(
            probe.at_px(rx as i32, y_at(0.80), g),
            Some(false),
            "the sky band"
        );

        assert_eq!(probe.at(4, 11, g.rows), Some(true));
        assert_eq!(
            probe.sky_at(4, 11, g.rows),
            Some(true),
            "a glyph darkens the sky"
        );
        for f in [0.10, 0.50, 0.80] {
            assert_eq!(
                probe.at_px(gx as i32, y_at(f), g),
                Some(true),
                "a glyph is ink at {f}"
            );
        }

        assert_eq!(probe.at(4, 12, g.rows), Some(false));
        assert_eq!(probe.sky_at(4, 12, g.rows), Some(false));
        assert_eq!(probe.at_px(bx as i32, y_at(0.50), g), Some(false));

        assert_eq!(probe.at(3, 10, g.rows), None, "an unprobed row is unknown");
        assert_eq!(probe.sky_at(3, 10, g.rows), None);
        assert_eq!(probe.at_px(rx as i32, y_at(-0.50), g), None);

        // The bool view is the class view with no rules in it.
        let mut bools = GlyphProbe::new();
        let mut occupied = [false; 120];
        occupied[11] = true;
        bools.probe_row(4, &occupied);
        for col in [10u16, 11, 12] {
            assert_eq!(bools.at(4, col, g.rows), Some(col == 11));
            assert_eq!(bools.sky_at(4, col, g.rows), Some(col == 11));
        }
        // Re-probing a row as bools forgets its rules (the slot is rebuilt).
        probe.probe_row(4, &occupied);
        assert_eq!(probe.at(4, 10, g.rows), Some(false));
        assert_eq!(probe.at_px(rx as i32, y_at(0.50), g), Some(false));
    }

    /// The rule class is exactly the LIGHT horizontal members of the Box
    /// Drawing block; a vertical arm, a heavy or double stroke, a block
    /// element, a dash or an underscore is a glyph, a space is a blank, and
    /// a wide continuation is a glyph (its lead is content).
    #[test]
    fn cell_ink_classifies_the_light_horizontal_rules_and_nothing_else() {
        for ch in ['─', '┄', '┈', '╌', '╴', '╶'] {
            assert_eq!(cell_ink(ch), CellInk::Rule, "{ch:?} (U+{:04X})", ch as u32);
        }
        for ch in [
            '━', '┅', '┉', '╍', '╸', '╺', '╼', '╾', '═', '│', '┃', '┼', '╭', '╮', '▁', '▔', '█',
            '-', '_', '—', '=', '~', 'a', 'A', '0', '\0', '\u{2588}',
        ] {
            assert_eq!(cell_ink(ch), CellInk::Glyph, "{ch:?} (U+{:04X})", ch as u32);
        }
        assert_eq!(cell_ink(' '), CellInk::Blank);
    }

    /// **L4 / D16 — NO MARK IS EVER DRAWN OVER A PROBED GLYPH** (§5.4, §20.1).
    ///
    /// Three statements in one: an occupied `row − 1` mints nothing, an
    /// UNPROBED `row − 1` mints nothing either (there is no in-cell fallback
    /// and no descender-gap fallback — both are deleted), and a free transient
    /// thrown at an inked cell is nudged clear or dropped rather than drawn.
    #[test]
    fn no_star_core_is_ever_born_over_a_probed_glyph() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();

        // 1. row − 1 UNKNOWN: nothing is born.
        let mut sky = Stardust::new();
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert_eq!(sky.live(), 0, "an unprobed sky band minted stars");

        // 2. row − 1 OCCUPIED: still nothing.
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[true; 120]);
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert_eq!(sky.live(), 0, "an inked sky band minted stars");

        // 3. row − 1 PROVABLY BLANK: the sky fills, and no core lands on ink.
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let inked: Vec<bool> = (0..120).map(|i| i % 2 == 0).collect();
        sky.probe_mut().probe_row(5, &inked);
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert!(sky.live() > 0, "a provably blank sky band minted nothing");
        for s in sky.live_iter() {
            assert_ne!(
                sky.probe().at_px(s.x as i32, s.y as i32, geom()),
                Some(true),
                "a star core landed on a probed glyph at ({}, {})",
                s.x,
                s.y
            );
        }

        // 4. A FREE TRANSIENT is cleared where it comes to REST, and the
        //    cell it is thrown from — the caret's own, ledger-exempt — is
        //    clear by exemption: a hop onto a word's first glyph still winks
        //    (§6.12, D17), its three born on the landing pixel and resting
        //    on the landing cell, never on the inked row around it. §5.4
        //    lets a transient fly anywhere; it does not let a CORE land on a
        //    stroke that is not the caret's own.
        let mut sky = Stardust::new();
        let mut row6 = [true; 120];
        row6[41] = false;
        sky.probe_mut().probe_row(6, &row6);
        let (ix, iy) = geom().cell_center(6, 40);
        sky.sow_mini_fan(t0, (ix, iy), 3, &ctx(t0, &c, (6, 40)));
        assert_eq!(sky.live(), 3, "a hop onto a glyph minted no mini-fan");
        for s in sky.live_iter() {
            assert_eq!(
                (s.x, s.y),
                (ix.round(), iy.round()),
                "a mini-fan star was not born on the landing pixel"
            );
            let (rx, ry) = s.rest();
            assert_eq!(
                (px_row(ry as f32, geom()), px_col(rx as f32, geom())),
                (6, 40),
                "a mini-fan core came to rest off the landing cell, at ({rx}, {ry})"
            );
        }
    }

    /// **§5.4 / D6 / D17 — A THROWN CORE IS CLEARED WHERE IT COMES TO REST:
    /// NUDGED WITHIN 2 PX, WALKED BACK IF THE SLOT IS LAW-FIXED, DROPPED
    /// OTHERWISE.** One landing cell, its neighbours inked; three throws
    /// aimed by hand so each answer of `settle` is exercised on known
    /// pixels, and then a whole fan boxed in by ink, which keeps exactly its
    /// hero and four m2 — the five the rain glints pair with — and drops
    /// every grain. In every case the birth pixel is where the throw was
    /// thrown from and the motion ENDS on the cleared pixel.
    #[test]
    fn a_thrown_core_is_cleared_where_it_rests_nudged_walked_back_or_dropped() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom();
        // Cell (6, 40) spans x 360..369, y 148..166; its centre rounds to
        // (365, 157). Row 6 is inked wall to wall except that cell; rows 5
        // and 7 are unprobed — open air.
        let mut sky = Stardust::new();
        let mut row6 = [true; 120];
        row6[40] = false;
        sky.probe_mut().probe_row(6, &row6);
        let cx = ctx(t0, &c, (6, 40));
        let window = FAN_THROW_MS;
        let thrown = |dx: i32, dy: i32| {
            let mut s = star(StarClass::M2, StarLane::Fan, t0, TINT_WHITE_RGB);
            s.x = 365.0;
            s.y = 157.0;
            s.v0 = (dx as f32 / window, dy as f32 / window);
            assert_eq!(s.rest(), (365 + dx, 157 + dy), "the fixture's own rest");
            s
        };
        let done = t0 + Duration::from_secs_f32(window / 1000.0) + Duration::from_millis(1);

        // Clear: a rest in open air (row 7) is sown as thrown.
        let mut s = thrown(7, 12);
        assert!(sky.settle(&mut s, false, &cx));
        assert_eq!(s.rest(), (372, 169), "a clear rest was moved");

        // Nudged: a rest one pixel inside the inked row, with air one pixel
        // below — the throw ends on that pixel, the birth stays put.
        let mut s = thrown(7, 8);
        assert!(sky.settle(&mut s, false, &cx));
        assert_eq!((s.x, s.y), (365.0, 157.0), "a nudge moved the birth");
        assert_eq!(
            s.rest(),
            (372, 166),
            "the rest was not nudged 1 px into the air"
        );
        assert_eq!(
            s.pos(done, false),
            s.rest(),
            "the throw does not end on its rest"
        );

        // Deep in ink: a grain is dropped …
        let mut s = thrown(12, 0);
        assert!(
            !sky.settle(&mut s, false, &cx),
            "a grain whose rest is 12 px into ink was kept"
        );
        // … and a law-fixed slot walks back along its own throw to the first
        // clear pixel: the landing cell's edge, the caret's own.
        let mut s = thrown(12, 0);
        assert!(sky.settle(&mut s, true, &cx));
        assert_eq!((s.x, s.y), (365.0, 157.0), "the walk-back moved the birth");
        assert_eq!(
            s.rest(),
            (368, 157),
            "the walk-back did not stop at the first clear pixel"
        );
        assert_eq!(s.pos(done, false), s.rest());

        // A shed fragment takes no exemption: its station is nobody's cell.
        let mut s = thrown(0, 0);
        s.lane = StarLane::Shed;
        s.class = StarClass::M3;
        s.x = 374.0;
        assert!(
            !sky.settle(&mut s, false, &cx),
            "a fragment resting on the inked cell it was shed over was kept"
        );

        // A whole fan boxed in by ink on rows 5-7. A 23 px reach puts every
        // rest at 12.65..24.15 px (`reach·(0.55..1.05)`): no 2 px nudge can
        // reach back into the landing cell (its far corner is 10.06 px out)
        // nor out past row 5 or 7 (27 px) into unprobed air — so every rest
        // is ink, the grains go, and the five stay, each walked back to the
        // landing cell, each still thrown (the rest is at the cell's edge,
        // not its centre).
        let mut boxed = Stardust::new();
        boxed.probe_mut().probe_row(5, &[true; 120]);
        boxed.probe_mut().probe_row(6, &row6);
        boxed.probe_mut().probe_row(7, &[true; 120]);
        let (ox, oy) = g.cell_center(6, 40);
        boxed.sow_fan(
            t0,
            FanSow {
                at_px: (ox, oy),
                reach: 23.0,
                tint_t: 0.25,
                seed: 7,
                n: 14,
                hero: true,
            },
            &cx,
        );
        let fan: Vec<&Star> = boxed
            .live_iter()
            .filter(|s| s.lane == StarLane::Fan)
            .collect();
        assert_eq!(
            fan.len(),
            5,
            "boxed in by ink, a landing keeps exactly its hero and four m2, not {}",
            fan.len()
        );
        assert_eq!(
            fan.iter()
                .filter(|s| s.class == StarClass::M1 && s.gold)
                .count(),
            1,
            "the gold hero"
        );
        assert_eq!(
            fan.iter().filter(|s| s.class == StarClass::M2).count(),
            4,
            "its four m2"
        );
        for s in &fan {
            assert_eq!((s.x, s.y), (ox.round(), oy.round()), "born on the landing");
            let (rx, ry) = s.rest();
            assert_eq!(
                (px_row(ry as f32, g), px_col(rx as f32, g)),
                (6, 40),
                "a {:?} came to rest off the landing cell, at ({rx}, {ry})",
                s.class
            );
            assert_ne!(
                (rx, ry),
                (ox.round() as i32, oy.round() as i32),
                "a {:?} was pinned at the centre instead of thrown to the cell's edge",
                s.class
            );
        }
    }

    /// **§5.2 — COVERAGE OVER A PROBED-OCCUPIED OR UNKNOWN CELL IS A THIRD**
    /// (v1's text-safe arm, kept). The same transient star, drawn over a
    /// probed-blank cell, over an inked one, and over a row nobody probed:
    /// the second and third peak at a third of the first.
    #[test]
    fn a_transient_over_ink_or_unknown_is_held_to_a_third() {
        let c = cfg(true);
        let t0 = Instant::now();
        let peak = |probe: Option<bool>| {
            let mut sky = Stardust::new();
            if let Some(inked) = probe {
                sky.probe_mut().probe_row(3, &[inked; 120]);
            }
            let mut s = star(StarClass::M2, StarLane::Fan, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            sky.sow(s);
            let sc = frame_at(&mut sky, t0, &c);
            lum(px(&sc.out, 200, 100))
        };
        let clear = peak(Some(false));
        let inked = peak(Some(true));
        let unknown = peak(None);
        assert!(
            clear >= 79,
            "the clear-row fan m2 did not reach its 80: {clear}"
        );
        assert!(
            inked * 3 <= clear + 3 && inked > 0,
            "over ink the star is {inked}, not a third of {clear}"
        );
        assert!(
            unknown * 3 <= clear + 3 && unknown > 0,
            "over an unprobed row the star is {unknown}, not a third of {clear}"
        );
    }

    /// The luminance trace of ONE strike m1 at its centre pixel over its whole
    /// life, with `after_sow` applied to the probe between the birth and the
    /// first frame — the shape both "the probe's answer changed after birth"
    /// laws share.
    fn strike_trace(c: &Config, t0: Instant, after_sow: impl FnOnce(&mut GlyphProbe)) -> Vec<u32> {
        let mut sky = sky();
        sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
        after_sow(sky.probe_mut());
        let life_ms = (StarClass::M1.life_s(StarLane::Strike) * 1000.0) as u64;
        (0..life_ms)
            .map(|ms| {
                let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), c);
                lum(px(&sc.out, 200, 100))
            })
            .collect()
    }

    /// **§5.2 / T5 / C3 — A SKY STAR KEEPS ITS CENTRE WHEN THE PROBE FORGETS
    /// ITS ROW.** The text-safe third is the TRANSIENTS' clearance; a sky
    /// star's clearance is its birth gate. The host's three-row probe evicts
    /// row − 1 the moment the caret steps down a line (Enter: rows r, r+1,
    /// r+2), so a strike star's own row becomes UNKNOWN mid-life while the
    /// star has not moved. Its whole luminance trace is byte-identical
    /// whether the probe remembers the row or has forgotten it — a per-frame
    /// re-read would hold every frame after the eviction to a third
    /// (143 → 48), a 3× step on every Enter and every prompt scroll.
    #[test]
    fn a_sky_star_keeps_its_centre_when_the_probe_forgets_its_row() {
        let c = cfg(true);
        let t0 = Instant::now();
        let remembered = strike_trace(&c, t0, |_| {});
        let forgotten = strike_trace(&c, t0, |probe| {
            // Three fresh rows evict rows 2-4 round-robin — the star's own
            // row 3 included — exactly as an Enter down at row 21 would.
            for row in 20..=22 {
                probe.probe_row(row, &[false; 120]);
            }
            assert!(
                probe.at_px(200, 100, geom()).is_none(),
                "the fixture did not forget the star's row"
            );
        });
        let peak = remembered.iter().copied().max().unwrap_or(0);
        assert!(
            (142..=144).contains(&peak),
            "the strike m1 did not reach §5.2's 143: {peak}"
        );
        assert_eq!(
            remembered, forgotten,
            "a sky star's trace changed when the probe forgot its row"
        );
    }

    /// **§5.2 / D16 — UNDER `underline` A SKY STAR SITS OVER ITS OWN ECHOED
    /// GLYPH AND IS NOT A THIRD FOR IT.** With `ribbon_tall = false` the sky
    /// band is `[0.04, 0.30]·ch` INSIDE the typed cell, and the caret-row
    /// probe reports that cell occupied by the very glyph the star was born
    /// for. The band was blank when the star was dealt — that gate is its
    /// whole clearance — so the trace over the inked cell is byte-identical
    /// to the trace over a blank one; a per-frame re-read would price every
    /// underline strike star at a third from frame 0.
    #[test]
    fn a_sky_star_over_its_own_echoed_glyph_keeps_its_centre_under_underline() {
        let mut c = cfg(true);
        c.ribbon_tall = false;
        let t0 = Instant::now();
        let blank = strike_trace(&c, t0, |_| {});
        let inked = strike_trace(&c, t0, |probe| {
            let mut row = [false; 120];
            row[px_col(200.0, geom()) as usize] = true;
            probe.probe_row(3, &row);
            assert_eq!(
                probe.at_px(200, 100, geom()),
                Some(true),
                "the fixture did not ink the star's own cell"
            );
        });
        let peak = blank.iter().copied().max().unwrap_or(0);
        assert!(
            (142..=144).contains(&peak),
            "the strike m1 did not reach §5.2's 143: {peak}"
        );
        assert_eq!(
            blank, inked,
            "a sky star's trace changed when its own cell was probed inked"
        );
    }

    /// **§5.6 — THE CAP NEVER REFUSES A NEWBORN**.
    ///
    /// 40 live is a ceiling on the SKY, not a rationing of light: over the cap
    /// the oldest live star is put on its finish and the newborn is always in
    /// the pool; the pool itself never exceeds its fixed allocation. The
    /// alternative — refusing the birth — is what makes a burst of typing look
    /// like it dropped frames.
    #[test]
    fn the_population_cap_never_refuses_a_newborn() {
        let t0 = Instant::now();
        let mut sky = Stardust::new();
        for k in 0..400u32 {
            let at = t0 + Duration::from_millis(u64::from(k));
            let mut s = star(StarClass::M2, StarLane::Fan, at, TINT_WHITE_RGB);
            s.seed = k;
            sky.sow(s);
            let live = sky.live_iter().filter(|s| s.finish.is_none()).count();
            assert!(live <= STAR_CAP, "{live} live stars over the cap at {k}");
            assert!(
                sky.live() <= STAR_POOL_MAX,
                "the pool overran its allocation at {k}: {}",
                sky.live()
            );
            assert!(
                sky.live_iter().any(|p| p.seed == k),
                "birth {k} was refused at the cap"
            );
        }
    }

    /// **§5.6 / §20.1 — OVER THE CAP THE OLDEST STAR FINISHES WITHIN 40 ms**,
    /// and it is the OLDEST, not the one with the least life left: a young
    /// grain with 190 ms left outlives an old hero with 420 ms left, because
    /// the hero is the one that has been looked at longest. The finish is a
    /// compressed retract on `spend`, never a pop — the star is still on
    /// glass 10 ms after the eviction and gone by 40.
    #[test]
    fn stars_over_the_cap_finish_oldest_first_within_forty_ms() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        // 40 stars, one per ms, each on its own pixel; #5 is a grain with the
        // least remaining life of the lot.
        for k in 0..STAR_CAP as u32 {
            let class = if k == 5 { StarClass::M3 } else { StarClass::M1 };
            let mut s = star(
                class,
                StarLane::Strike,
                t0 + Duration::from_millis(u64::from(k)),
                TINT_WHITE_RGB,
            );
            s.seed = k;
            s.x = 100.0 + 5.0 * k as f32;
            sky.sow(s);
        }
        let evict_at = t0 + Duration::from_millis(41);
        let mut newborn = star(StarClass::M1, StarLane::Strike, evict_at, TINT_WHITE_RGB);
        newborn.seed = 1000;
        newborn.x = 400.0;
        sky.sow(newborn);
        let finishing: Vec<u32> = sky
            .live_iter()
            .filter(|s| s.finish.is_some())
            .map(|s| s.seed)
            .collect();
        assert_eq!(
            finishing,
            vec![0],
            "the star put on its finish is not the oldest"
        );
        assert!(
            sky.live_iter().any(|s| s.seed == 1000),
            "the newborn was refused"
        );

        let lit = |sky: &mut Stardust, ms: u64, x: i32| {
            let sc = frame_at(sky, evict_at + Duration::from_millis(ms), &c);
            lum(px(&sc.out, x, 100)) > 0
        };
        assert!(
            lit(&mut sky, 10, 100),
            "the evicted star popped instead of finishing"
        );
        assert!(
            !lit(&mut sky, 40, 100),
            "the evicted star outlived its 40 ms finish"
        );
        assert!(
            lit(&mut sky, 40, 125),
            "the young grain (#5) was evicted instead of the oldest"
        );
    }

    /// **§5.8 / D5 — HEROES ARE EARNED, AND EARNED DETERMINISTICALLY**.
    ///
    /// A shifted capital and a `!` always mint an m1; an ordinary glyph mints
    /// one only on the 1-in-12 deal. And the whole producer is a pure function
    /// of its event sequence: the same keys at the same instants build the
    /// same sky, byte for byte, because every seed is hashed and nothing reads
    /// a clock or an RNG (§18).
    #[test]
    fn heroes_are_earned_deterministically() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let build = |class: TypedClass| {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            for k in 0..6u16 {
                sky.on_event(&typed(class), t0, &ctx(t0, &c, (5, 10 + k * 4)), &ribbon);
            }
            sky
        };
        let a = build(TypedClass::Capital);
        let b = build(TypedClass::Capital);
        let key = |s: &Stardust| -> Vec<(StarClass, u32, i32, i32)> {
            s.live_iter()
                .map(|p| (p.class, p.tint, p.x as i32, p.y as i32))
                .collect()
        };
        assert_eq!(key(&a), key(&b), "the same keys built two different skies");

        // Every capital is a hero: six typed, six m1.
        let heroes = a.live_iter().filter(|p| p.class == StarClass::M1).count();
        assert_eq!(heroes, 6, "six capitals earned {heroes} heroes");
        let bangs = build(TypedClass::Bang)
            .live_iter()
            .filter(|p| p.class == StarClass::M1)
            .count();
        assert_eq!(bangs, 6, "six `!` earned {bangs} heroes");

        // …and an ordinary glyph is nowhere near that rate.
        let plain_heroes = build(TypedClass::Glyph)
            .live_iter()
            .filter(|p| p.class == StarClass::M1)
            .count();
        assert!(
            plain_heroes < heroes,
            "an ordinary glyph earned as many heroes ({plain_heroes}) as a capital"
        );
    }

    /// **§5.2 / §5.8 — SPACING AND THE ROW CAP RATION DEALT HEROES, NEVER
    /// EARNED ONES**. Three capitals on three adjacent cells are three heroes
    /// (a capital is an m1, full stop); a row of plain glyphs carries at most
    /// two live dealt heroes, never closer than three cells — and the
    /// spacing is read off the LIVE pool, so a dead hero frees it.
    #[test]
    fn an_earned_hero_is_never_demoted_by_spacing_but_a_dealt_one_is() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();

        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        for col in 10..13u16 {
            sky.on_event(
                &typed(TypedClass::Capital),
                t0,
                &ctx(t0, &c, (5, col + 1)),
                &ribbon,
            );
        }
        assert_eq!(
            sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
            3,
            "adjacent capitals were demoted by the spacing law"
        );

        // A row whose first twenty cells deal at least three heroes, at least
        // two of them three cells apart — so the row cap has something to
        // bind on — inside one tick, under the pool cap (twenty, not thirty:
        // with the field dealt 1-in-2 a thirty-cell tick is forty births).
        let m1_draw = |row: u16, col: u16| {
            hash01(mix32(cell_hash(row, col, 0) ^ SALT_STRIKE)) < 1.0 / DEAL_M1_IN as f32
        };
        let row = (1..40u16)
            .find(|&row| {
                let draws: Vec<u16> = (0..20u16).filter(|&col| m1_draw(row, col)).collect();
                draws.len() >= 3
                    && draws
                        .windows(2)
                        .any(|w| w[1] - w[0] >= HERO_MIN_SPACING_CELLS)
            })
            .expect("some row deals three heroes in twenty cells");
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(i32::from(row) - 1, &[false; 120]);
        for col in 0..20u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (row, col + 1)),
                &ribbon,
            );
        }
        assert!(sky.live() < STAR_CAP, "the fixture ran into the pool cap");
        let heroes: Vec<i32> = sky
            .live_iter()
            .filter(|p| p.class == StarClass::M1)
            .map(col_of)
            .collect();
        assert_eq!(
            heroes.len(),
            usize::from(HERO_MAX_LIVE_PER_ROW),
            "three or more dealt heroes on one row left {} live",
            heroes.len()
        );
        for (i, a) in heroes.iter().enumerate() {
            for b in &heroes[i + 1..] {
                assert!(
                    (a - b).abs() >= i32::from(HERO_MIN_SPACING_CELLS),
                    "dealt heroes at columns {a} and {b} are closer than 3 cells"
                );
            }
        }

        // The spacing is the POOL's: once every hero has died, a capital on
        // the same row is a hero again, with no memory to ration it.
        let later = t0 + Duration::from_secs(2);
        let mut sc = Scratch::default();
        sky.emit(&ctx(later, &c, (row, 1)), &mut sc.frame());
        assert!(sky.at_rest());
        sky.on_event(
            &typed(TypedClass::Capital),
            later,
            &ctx(later, &c, (row, 1)),
            &ribbon,
        );
        assert_eq!(
            sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
            1
        );
    }

    /// **§5.6 — THE STRIKE DEAL IS A PARTITION, AND NO TWO SKY STARS SHARE A
    /// PIXEL**. A typed cell carries at most one strike star; two sky stars
    /// dealt near one cell (the leading-edge grain, the next key's own deal,
    /// the field star) are born on distinct device pixels, because two stars
    /// on one pixel composite past the sparkle law's ceiling.
    #[test]
    fn no_two_sky_stars_share_a_pixel() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut cx = ctx(t0, &c, (5, 1));
        cx.disp = 0.6;
        cx.birth_disp = 0.6;
        for col in 0..119u16 {
            cx.caret = (5, col + 1);
            sky.on_event(&typed(TypedClass::Glyph), t0, &cx, &ribbon);
        }
        let mut strike_per_cell = std::collections::BTreeMap::new();
        let mut pixels = std::collections::BTreeSet::new();
        for s in sky.live_iter() {
            assert!(
                pixels.insert((s.x as i32, s.y as i32)),
                "two sky stars share pixel ({}, {})",
                s.x,
                s.y
            );
            if s.lane == StarLane::Strike && s.v0 != (0.0, 0.0) {
                // an m1/m2 strike — the partition's own births (grains may be
                // the leading-edge second)
                *strike_per_cell.entry(col_of(s)).or_insert(0) += 1;
            }
        }
        assert!(
            strike_per_cell.values().all(|&n| n == 1),
            "a cell carries two m1/m2 strike stars: {strike_per_cell:?}"
        );
        // The cap binds long before the row ends: what is on glass is the
        // newest forty-odd, every one on its own pixel.
        assert!(
            sky.live() >= STAR_CAP,
            "the dense sky holds only {} stars",
            sky.live()
        );
    }

    /// **§5.6 / `spine.rs` — THE SECOND GRAIN IS PRICED ON THE HONEST SPINE**.
    /// A resumed hand has `birth_disp` floored high while the honest `disp`
    /// is cold; it must not buy the dense sky. Same key, same cell: no
    /// leading-edge grain at `disp 0.1 / birth 0.9`, one at `disp 0.6`.
    #[test]
    fn the_second_grain_is_priced_on_the_honest_spine() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let lead_grains = |disp: f32, birth: f32| {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut cx = ctx(t0, &c, (5, 10));
            cx.disp = disp;
            cx.birth_disp = birth;
            sky.on_event(&typed(TypedClass::Space), t0, &cx, &ribbon);
            assert_eq!(sky.live(), 0, "a space dealt a star");
            sky.on_event(&typed(TypedClass::Glyph), t0, &cx, &ribbon);
            sky.live_iter().filter(|s| col_of(s) == 10).count()
        };
        assert_eq!(lead_grains(0.1, 0.9), 0, "a resume bought the dense sky");
        assert_eq!(
            lead_grains(0.6, 0.6),
            1,
            "a hot spine did not throw its second grain"
        );
    }

    /// **§13 / D5 — ONE TOKEN, ONE HERO, ONE GLINT**.
    ///
    /// Only an m1 spends; m2 and m3 never touch the bucket; a DEALT hero with
    /// no token becomes an m2 (shown, silent) while an EARNED one is still an
    /// m1 and simply says nothing; and a meteor empties the bucket, so nothing
    /// chimes on top of the rain.
    #[test]
    fn hero_births_and_glints_share_one_budget() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.budget.refill(t0);

        // Twelve capitals inside one tick: twelve heroes, three glints — the
        // bucket's depth — and every hero past the third born silent.
        let mut glints = 0usize;
        for k in 0..12u16 {
            sky.on_event(
                &typed(TypedClass::Capital),
                t0,
                &ctx(t0, &c, (5, 10 + k * 4)),
                &ribbon,
            );
            glints += sky.take_glints().count();
        }
        let heroes = sky.live_iter().filter(|p| p.class == StarClass::M1).count();
        assert_eq!(heroes, 12, "an earned hero was demoted for want of a token");
        assert_eq!(
            glints, GLINT_CAP as usize,
            "{glints} glints from a bucket of {GLINT_CAP}"
        );

        // m2 and m3 never touch the bucket (§13).
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let before = sky.budget.tokens();
        for k in 0..20u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        assert!(sky.live_iter().any(|p| p.class == StarClass::M3));
        let spent = before - sky.budget.tokens();
        let dealt_heroes = sky.live_iter().filter(|p| p.class == StarClass::M1).count() as f32;
        assert!(
            spent <= dealt_heroes,
            "a grain or a medium star spent {spent} tokens"
        );

        // A DEALT hero with a dry bucket is born as an m2: over a whole row of
        // plain glyphs with the bucket empty, no m1 is born at all.
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.budget.empty();
        for col in 0..120u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, col + 1)),
                &ribbon,
            );
        }
        assert_eq!(
            sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
            0,
            "a dealt hero was born on a dry bucket"
        );

        // The meteor's edge empties the bucket; the next EARNED hero is still
        // an m1 and is silent.
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.budget.empty();
        sky.on_event(
            &typed(TypedClass::Capital),
            t0,
            &ctx(t0, &c, (5, 30)),
            &ribbon,
        );
        assert_eq!(
            sky.take_glints().count(),
            0,
            "a hero chimed on top of the rain"
        );
        assert!(
            sky.live_iter().any(|p| p.class == StarClass::M1),
            "an EARNED hero was demoted for want of a token"
        );
    }

    /// **§13 — THE BUCKET KEEPS CHIMING AS IT REFILLS**. Six capitals 400 ms
    /// apart on six rows: every one chimes, because the tokens are the whole
    /// law and the bucket regains 1.6 between them. A "live voices" gate that
    /// nothing releases would go mute after the third for the rest of the
    /// session — §13's ≤ 3 live is the SYNTH's steal rule, not a spend
    /// refusal.
    #[test]
    fn the_bucket_keeps_chiming_as_it_refills() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        let mut glints = 0usize;
        for k in 0..6u16 {
            let row = 5 + 3 * k;
            let at = t0 + Duration::from_millis(400 * u64::from(k));
            sky.probe_mut().probe_row(i32::from(row) - 1, &[false; 120]);
            sky.budget.refill(at);
            sky.on_event(
                &typed(TypedClass::Capital),
                at,
                &ctx(at, &c, (row, 10)),
                &ribbon,
            );
            glints += sky.take_glints().count();
        }
        assert_eq!(glints, 6, "the bucket shut after {glints} of 6 heroes");
    }

    /// **§13 — THE SPEND AND THE CUE RIDE THE SAME FRAME**.
    ///
    /// One token buys one hero AND one `SoundKind::Stardust` at that hero's
    /// own column, on the tick the hero was born — and the cue carries the
    /// star's own scintillation rate, so what you see winking and what you
    /// hear winking are one number.
    #[test]
    fn a_paid_hero_cues_one_glint_at_its_own_column() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.on_event(
            &typed(TypedClass::Capital),
            t0,
            &ctx(t0, &c, (5, 21)),
            &ribbon,
        );
        let sc = frame_at(&mut sky, t0, &c);
        assert_eq!(
            sc.cues.len(),
            1,
            "one paid hero did not cue exactly one glint"
        );
        assert_eq!(sc.cues[0].col, 20, "the glint is not at the hero's column");
        match sc.cues[0].kind {
            SoundKind::Stardust { twinkle_hz } => assert!(
                (SCINT_F_MIN as u8..=SCINT_F_MAX as u8).contains(&twinkle_hz),
                "twinkle_hz {twinkle_hz} is outside the seeded 8-12 Hz band"
            ),
            other => panic!("a hero cued {other:?}"),
        }
    }

    /// **§5.4 — A HERO CLIPPED OFF THE GLASS SPENDS NOTHING**. On row 0 with
    /// no head band the sky band lies above the effects box: no star can be
    /// drawn there, so none is born, no token is spent and no glint is cued.
    /// The same capital under a 40 px head band is a hero that chimes.
    #[test]
    fn a_row_zero_hero_clipped_off_the_glass_spends_nothing() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        for (head, want) in [(0u16, 0usize), (40, 1)] {
            let mut sky = Stardust::new();
            let before = sky.budget.tokens();
            let mut cx = ctx(t0, &c, (0, 5));
            cx.geom.head = head;
            cx.geom.origin_y = head;
            sky.on_event(&typed(TypedClass::Capital), t0, &cx, &ribbon);
            let heroes = sky.live_iter().filter(|s| s.class == StarClass::M1).count();
            assert_eq!(heroes, want, "head {head}: {heroes} heroes");
            assert_eq!(sky.take_glints().count(), want, "head {head}: glint count");
            assert_eq!(
                (before - sky.budget.tokens()) as usize,
                want,
                "head {head}: a clipped hero spent a token"
            );
        }
    }

    /// **§5.2 / §5.6 — STARDUST IS A MAGNITUDE DISTRIBUTION** (§20.1's
    /// oracle: 12 cps for 4 s, ≤ 40 live every frame, ≥ 40 % of LIVE stars
    /// m2/m1, all three magnitudes present). Measured on the live pool frame
    /// by frame after the first second, not on the births — the sky the eye
    /// sees is weighted by the classes' lives.
    #[test]
    fn stardust_is_a_magnitude_distribution_under_the_cap() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut seen = std::collections::BTreeSet::new();
        let (mut bright, mut total) = (0usize, 0usize);
        for k in 0..48u16 {
            let at = t0 + Duration::from_millis(u64::from(k) * 83);
            sky.budget.refill(at);
            sky.on_event(
                &typed(TypedClass::Glyph),
                at,
                &ctx(at, &c, (5, 10 + k)),
                &ribbon,
            );
            let mut sc = Scratch::default();
            sky.emit(&ctx(at, &c, (5, 10 + k)), &mut sc.frame());
            assert!(sky.live() <= STAR_CAP, "the pool overran the cap at {k}");
            for s in sky.live_iter() {
                seen.insert(s.class);
                if k >= 12 {
                    total += 1;
                    bright += usize::from(s.class != StarClass::M3);
                }
            }
        }
        assert_eq!(seen.len(), 3, "one magnitude never appeared: {seen:?}");
        assert!(
            bright * 100 >= total * 40,
            "only {}% of the live sky is m2 or m1",
            bright * 100 / total.max(1)
        );
    }

    /// **§5.6 — A FIELD STAR RIDES ITS CELL'S ENVELOPE, NOT THE GRAIN'S
    /// LIFE.** The judge's "sparse" (max 9 live at 14.3 cps against the
    /// design's ≈ 20) was this: the ribbon's own starfield was being born
    /// with the grain's 225 ms class life. A field grain is at FULL alpha at
    /// 500 ms and at one second (where a strike grain has been gone for 775
    /// ms), melts on the ribbon's own `expiry_melt` from 70 % of its life, is
    /// still a lit point at 1.3 s, and is gone by the exit swoosh's horizon —
    /// never a star over glass the ribbon has left.
    /// **T2 — THE FRAME THAT ECHOES THE KEY ALREADY SHOWS ITS FIELD STAR.**
    /// A field star dealt on a sub-millisecond echo frame is drawn on THAT
    /// frame: its birth envelope opens from the cell's [`BIRTH_EDGE_FLOOR`],
    /// above the cull, and reaches exactly full when the 18 ms `edge-in`
    /// does. Before the floor `edge_in(0.3 ms)` was 0.0008, the star was
    /// under [`STAR_CULL_ALPHA`] and the sky was empty on the echo frame.
    #[test]
    fn the_frame_that_echoes_the_key_already_shows_its_field_star() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        let mut field = star(StarClass::M3, StarLane::Field, t0, TINT_WHITE_RGB);
        field.x = 150.0;
        sky.sow(field);
        let echo = t0 + Duration::from_micros(300);
        let a = field.envelope(echo, false);
        assert!(
            a >= BIRTH_EDGE_FLOOR,
            "a field star's birth envelope is {a} on the echo frame"
        );
        let sc = frame_at(&mut sky, echo, &c);
        assert!(
            lum(px(&sc.out, 150, 100)) > 0,
            "the echo frame shows no field star"
        );
        let full = t0 + Duration::from_secs_f32(super::super::timing::EDGE_IN_S);
        assert!(
            (field.envelope(full, false) - 1.0).abs() < 1e-4,
            "the edge-in does not reach full: {}",
            field.envelope(full, false)
        );
        // The floor is the BIRTH's: a strike star never reads it (born at
        // full inside its hold) and a finish is unfloored.
        let strike = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
        assert!((strike.envelope(echo, false) - 1.0).abs() < 1e-6);
        let mut finishing = field;
        finishing.finish_by(t0, 0.040);
        let late = t0 + Duration::from_millis(30);
        assert!(
            finishing.envelope(late, false) < STAR_CULL_ALPHA,
            "a finish read the birth floor"
        );
    }

    /// **THE SIZE CLOCK'S NEXT STEP IS SOLVED, NOT POLLED** (§18's cadence
    /// law): for every class at 8, 9.7 and 12 Hz, [`Star::next_arm_step_s`]
    /// names the instant `arm_px` next changes to within 0.1 ms of a 20 µs
    /// scan — and it is never more than half a `sin²` period (≤ 62.5 ms)
    /// away, which is what bounds a twinkling sky's cadence from below.
    #[test]
    fn the_arm_clocks_next_step_is_solved_not_polled() {
        let t0 = Instant::now();
        let ch = geom().ch as f32;
        for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
            for f in [8.0f32, 9.7, 12.0] {
                let mut s = star(class, StarLane::Strike, t0, TINT_WHITE_RGB);
                s.f = f;
                let hold = class.envelope_s(StarLane::Strike).0;
                for lead_ms in [5u64, 17, 41] {
                    let from = t0 + Duration::from_secs_f32(hold) + Duration::from_millis(lead_ms);
                    let dt = s
                        .next_arm_step_s(ch, from)
                        .expect("every class swings across an integer arm");
                    assert!(
                        dt > 0.0 && dt <= 0.5 / f + 1e-4,
                        "{class:?} at {f} Hz: next step {dt} s"
                    );
                    let a0 = arm_px(&s, ch, false, from);
                    let mut t = 0.0f32;
                    let scanned = loop {
                        t += 2e-5;
                        assert!(t < 0.2, "{class:?} at {f} Hz: the arm never stepped");
                        if arm_px(&s, ch, false, from + Duration::from_secs_f32(t)) != a0 {
                            break t;
                        }
                    };
                    assert!(
                        (scanned - dt).abs() < 1e-4,
                        "{class:?} at {f} Hz, +{lead_ms} ms: solved {dt} s, scanned {scanned} s"
                    );
                }
            }
        }
    }

    /// **§5.6 — A FIELD STAR RIDES ITS CELL, NOT THE GRAIN'S LIFE.** Under a
    /// hand that keeps typing on its row — a Space every 83 ms, which lays
    /// ribbon, deals no star and re-lights the field
    /// ([`Stardust::relight_field`]) — a field grain is at full alpha at
    /// 500 ms and 1 s (the strike grain beside it is gone at 225), melts on
    /// the cell's own curve from 1.1 to 1.3 s, and is gone exactly at the
    /// swoosh horizon ([`FIELD_LIFE_S`], §4's 1.54 s). The same grain with
    /// NO key after its birth goes out on the field fade instead: full
    /// through [`FIELD_HOLD_MS`], under half by 700 ms, off glass and out
    /// of the pool at [`field_fade_life_s`] — long before the horizon.
    #[test]
    fn a_field_star_rides_its_cell_s_envelope_not_the_grain_s_life() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(
            (StarClass::M3.life_s(StarLane::Strike) * 1000.0) as u64,
            225,
            "the strike grain's published life"
        );
        let fixture = || {
            let mut sky = sky();
            let mut field = star(StarClass::M3, StarLane::Field, t0, TINT_WHITE_RGB);
            field.x = 150.0;
            sky.sow(field);
            sky.sow(star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB));
            sky
        };
        let field_alpha = |sky: &Stardust, ms: u64| {
            sky.live_iter()
                .find(|s| s.lane == StarLane::Field)
                .map_or(0.0, |s| s.envelope(at(ms), false))
        };

        // -- the hand keeps typing on row 3 (the fixture's sky) -----------
        let mut sky = fixture();
        let mut next_key = 83u64;
        let mut type_until = |sky: &mut Stardust, ms: u64| {
            while next_key <= ms {
                let cx = ctx(at(next_key), &c, (3, 10));
                sky.on_event(&typed(TypedClass::Space), at(next_key), &cx, &ribbon);
                next_key += 83;
            }
        };
        for ms in [500u64, 1000] {
            type_until(&mut sky, ms);
            let a = field_alpha(&sky, ms);
            assert!(
                (a - 1.0).abs() < 1e-3,
                "a field grain under a typing hand is at alpha {a} at {ms} ms, not full"
            );
        }
        let sc = frame_at(&mut sky, at(1000), &c);
        assert!(
            lum(px(&sc.out, 150, 100)) > 0,
            "the field grain died with the grain's life"
        );
        assert_eq!(
            lum(px(&sc.out, 200, 100)),
            0,
            "a strike grain outlived its 225 ms"
        );
        type_until(&mut sky, 1100);
        let a1100 = field_alpha(&sky, 1100);
        type_until(&mut sky, 1300);
        let a1300 = field_alpha(&sky, 1300);
        assert!(
            a1100 < 1.0 && a1300 < a1100 && a1300 > STAR_CULL_ALPHA,
            "the field grain does not melt on the cell's curve: {a1100} → {a1300}"
        );
        let sc = frame_at(&mut sky, at(1300), &c);
        assert!(lum(px(&sc.out, 150, 100)) > 0, "gone before its melt");
        let horizon = (FIELD_LIFE_S * 1000.0).round() as u64;
        assert_eq!(horizon, 1540, "the swoosh horizon is §4's 1.54 s");
        type_until(&mut sky, horizon);
        let sc = frame_at(&mut sky, at(horizon + 1), &c);
        // The STARS are gone; the aurora the same keys laid is the ribbon's
        // own light and is still under a hand that typed 46 ms ago — it
        // leaves with the ribbon (`the_aurora_leaves_with_the_ribbon`), not
        // with the grain.
        assert!(
            sc.out.is_empty() && sky.live() == 0,
            "a field star outlived the exit swoosh"
        );

        // -- the hand stops at the grain's birth --------------------------
        let mut sky = fixture();
        let hold = (FIELD_HOLD_MS as u64).saturating_sub(1);
        let a = field_alpha(&sky, hold);
        assert!(
            (a - 1.0).abs() < 1e-3,
            "a field grain is at alpha {a} inside its hold"
        );
        let (a500, a700) = (field_alpha(&sky, 500), field_alpha(&sky, 700));
        assert!(
            a500 < 1.0 && a700 <= 0.5 && a700 > STAR_CULL_ALPHA,
            "the field fade is not a half-life from the last key: {a500} → {a700}"
        );
        let cull = (field_fade_life_s() * 1000.0).round() as u64;
        let sc = frame_at(&mut sky, at(cull - 10), &c);
        assert!(
            lum(px(&sc.out, 150, 100)) > 0,
            "the field grain went out before its fade's cull"
        );
        let sc = frame_at(&mut sky, at(cull + 1), &c);
        assert!(
            sc.out.is_empty() && sky.at_rest(),
            "a field star outlived the field fade after the hand stopped"
        );
    }

    /// **THE FIELD IS DARK BEFORE THE RETRACT MOVES** — the one coupling
    /// the field fade's constants carry: `H + 2.32·τ` of [`FIELD_HOLD_MS`]
    /// / [`FIELD_TAU_MS`] ends at or before the ribbon's own retract begins
    /// ([`RETRACT_START_S`], §4: grace + the three reach beats), so nothing
    /// hangs over the mark as it is drawn back into the caret; the
    /// measurer's bound — under half by 700 ms after the last key — holds
    /// on the envelope itself, twinkle aside, and 700 ms is inside the fade
    /// (the star is dying by alpha there, not already gone).
    #[test]
    fn the_field_is_dark_before_the_retract_moves() {
        let life = field_fade_life_s();
        assert!(
            life <= RETRACT_START_S && life > 0.7,
            "the field fade ends at {life} s against a retract that starts at {RETRACT_START_S} s"
        );
        let (hold, tau) = (FIELD_HOLD_MS / 1000.0, FIELD_TAU_MS / 1000.0);
        let a = |s: f32| half_life(s, hold, tau);
        assert!(
            (a(hold) - 1.0).abs() < 1e-6,
            "the hold does not reach its own end at full"
        );
        assert!(
            a(0.7) <= 0.5,
            "a field star is at {} of its peak 700 ms after the last key",
            a(0.7)
        );
        assert!(
            a(life) < STAR_CULL_ALPHA + 1e-3 && a(life - 0.01) >= STAR_CULL_ALPHA,
            "the fade's cull ({}) is not where its life says ({life} s)",
            a(life)
        );
    }

    /// **§5.3 — A TYPING SKY IS NOT MONOCHROME.** The judge measured the
    /// AFTER sky at saturation p50 7, 89-100 % grey (`A/frame_0030`,
    /// `A/frame_0100`): the tint the deal assigns never cleared the eye.
    /// Forty keys at 12 cps over a blank sky, on the engine's own spine, the
    /// field walking all seven stops; each live star is rendered alone and
    /// its CHROMA read as the largest max − min channel gap among its tinted
    /// quads and halos (the stubs, the grain's disc, the atmosphere). Two
    /// clauses:
    ///
    /// 1. **birth** — ≥ 60 % of the stars born show chroma ≥ 24 on their
    ///    first FULL frame (20 ms in, past the `edge-in` a field star takes
    ///    with its cell). The 60/25/15 deal is 75 % coloured; what the
    ///    coloured 75 % lose is only the grain wearing indigo or violet,
    ///    whose disc at `27 · 130/255` cannot reach 24 (the stops are dim by
    ///    name — an m2's stubs at 48 still clear it on every stop);
    /// 2. **steady state** (keys ≥ 20, the field filled) — the live stars
    ///    chromatic at ≥ 24 average ≥ 40 % of the sky and never fall under
    ///    a quarter of it on any key frame. The arithmetic: 75 % coloured ×
    ///    the share of each class's life its tint stays over the threshold
    ///    under the alpha-only fade (a stub at `61·α` for the first two
    ///    thirds of an m1's life, a field disc at `27·α` for three quarters
    ///    of its melt, a strike grain's only through its hold) ≈ 46 %; the
    ///    judge's sky was 0 %.
    #[test]
    fn a_typing_sky_is_not_monochrome() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let chroma_of = |s: &Star, now: Instant| -> u32 {
            let mut one = Stardust::new();
            one.sow(*s);
            let sc = frame_at(&mut one, now, &c);
            sc.out
                .iter()
                .map(|q| chroma(q.color))
                .chain(sc.halos.iter().map(|h| chroma(h.color)))
                .max()
                .unwrap_or(0)
        };
        let (mut born, mut born_chromatic) = (0usize, 0usize);
        let (mut steady, mut steady_chromatic, mut steady_floor) = (0usize, 0usize, 1.0f32);
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            // The field walks the whole arc over the run, so every stop —
            // the dim indigo and violet included — is dealt.
            cx.caret_t = f32::from(k) / 40.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            let (mut live, mut live_chromatic) = (0usize, 0usize);
            for s in sky.live_iter() {
                if s.born == at {
                    let full = chroma_of(s, at + Duration::from_millis(20));
                    born += 1;
                    born_chromatic += usize::from(full >= 24);
                }
                live += 1;
                live_chromatic += usize::from(chroma_of(s, at) >= 24);
            }
            if k >= 20 && live > 0 {
                steady += live;
                steady_chromatic += live_chromatic;
                steady_floor = steady_floor.min(live_chromatic as f32 / live as f32);
            }
        }
        assert!(born >= 40, "forty keys bore only {born} stars");
        assert!(
            born_chromatic * 100 >= born * 60,
            "only {born_chromatic} of {born} stars were born visibly tinted"
        );
        assert!(
            steady_chromatic * 100 >= steady * 40,
            "only {steady_chromatic} of {steady} live star-frames are chromatic — the sky reads grey"
        );
        assert!(
            steady_floor >= 0.25,
            "a steady-state frame fell to {steady_floor} chromatic — the sky reads grey"
        );
    }

    /// **§5.6 — A 12 cps SKY HOLDS ABOUT TWENTY STARS** ("typical at 12 cps
    /// ≈ 20", cap 40; §18: "~120 quads, 6 halos"). The judge measured max 9
    /// / mean 5 live at 14.3 cps, and the capture's own pool column read
    /// 3-9: every dealt star was born, so the count was the deal × life
    /// arithmetic — 5.5 of strike at 12 cps, and a field starfield born on
    /// the grain's 225 ms life instead of its cell's ([`FIELD_LIFE_S`],
    /// [`FIELD_DEAL_IN`]). Forty-eight keys at exactly 12 cps on the spine
    /// the engine's own integrator reaches; steady state is read from key 24
    /// (t ≥ 2 s: the 1.54 s field horizon has passed and the spine is ≥ 0.8).
    ///
    /// The bound is the arithmetic's, not the prose's: `12 · (½ · 1.54)` of
    /// field plus 5.5 of strike is 14.7 expected, and the field's 1-in-2 over
    /// an 18-cell window is a binomial with σ ≈ 2 — this run's own hash
    /// realization sat at 13 (min 10, max 16), 104 quads, 5 haloed stars.
    /// **With the hot field deal (2026-09-08, `FIELD_DEAL_IN_HOT`: every
    /// cell at `disp ≥ 0.7`) the same run measures 22 live (min 16, max
    /// 26), 179 quads, 9 haloed stars** — the owner's denser sky, still
    /// under the cap and both budgets. So the steady mean must lie in
    /// `12..=40` (12.1 is the same arithmetic on a cold spine with no second
    /// grain — the floor a 12 cps sky may never fall through), no frame is
    /// under eight or over the cap, and a typical frame's quads and haloed
    /// STARS (the aurora's veils are ellipses, budgeted apart) are inside
    /// the budgets so nothing is thinned. The judge's 5 sat at a third of
    /// the floor.
    #[test]
    fn a_twelve_cps_sky_holds_about_twenty_stars() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let (mut live, mut quads, mut haloed) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..48u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            let n = sky.live_iter().filter(|s| s.finish.is_none()).count();
            assert!(n <= STAR_CAP, "{n} live at key {k} is over the cap");
            if k >= 24 {
                live.push(n);
                quads.push(sc.out.len());
                // Star halos are ROUND; the aurora's veils are the wide
                // ellipses (`rx != ry`) and are budgeted on their own.
                let stars: std::collections::BTreeSet<(u16, u16)> = sc
                    .halos
                    .iter()
                    .filter(|h| h.rx == h.ry)
                    .map(|h| (h.cx, h.cy))
                    .collect();
                haloed.push(stars.len());
            }
        }
        let mean = |v: &[usize]| v.iter().sum::<usize>() / v.len().max(1);
        let (m, lo, hi) = (
            mean(&live),
            live.iter().copied().min().unwrap_or(0),
            live.iter().copied().max().unwrap_or(0),
        );
        assert!(
            (12..=40).contains(&m),
            "a 12 cps sky holds {m} stars on average (min {lo}, max {hi}), not about twenty"
        );
        assert!(lo >= 8, "a 12 cps sky thinned to {lo} stars");
        let (q, h) = (mean(&quads), mean(&haloed));
        eprintln!("12 cps sky: {m} live (min {lo}, max {hi}), {q} quads, {h} haloed stars");
        assert!(
            (60..=STARDUST_QUAD_BUDGET).contains(&q) && (3..=STARDUST_HALO_BUDGET).contains(&h),
            "a typical 12 cps frame is {q} quads / {h} haloed, not §18's ~120 / 6"
        );
    }

    /// **§5.6 / §5.1 — FIELD STARS DIM WITH THE RIBBON THEY WERE BORN
    /// OVER.** The live capture of 2026-09-05 (defect 4) measured 12 of 45
    /// typing star tracks living past §5.2's longest class life, max 1264
    /// ms: the crosses over "sparkles" held near-full brightness ~1.2 s after
    /// the last key. A field star's alpha was the CELL's own end-of-life melt
    /// over the 1.54 s swoosh horizon — flat until its last 460 ms — while
    /// the ribbon under it had already spent its grace and was being drawn
    /// back into the caret. §5.6's Field row rides "the cell's own edge
    /// envelope × retract — alpha only", and §5.1's stars are "few and
    /// bright, and die by alpha": the sky goes out FROM THE HAND.
    ///
    /// Twenty keys at 12 cps on the engine's own spine, then the hand stops.
    /// The brightest field star's composited centre is read at +0, +350,
    /// +700 and +1000 ms after the last key — one luminance-clock period
    /// (`π/9` s = 349 ms) apart, so the twinkle reads the same phase at every
    /// sample and only the population envelope can move the number: monotone
    /// non-increasing, at or under HALF of +0 by +700 (the envelope itself
    /// under 0.50 there, twinkle aside), and the whole field culled — out of
    /// the pool and off glass — by +1100.
    #[test]
    fn field_stars_dim_with_the_ribbon_they_were_born_over() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let keys = 20u16;
        for k in 0..keys {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
        }
        let last = key_at(t0, keys - 1);
        let caret = (5, 10 + keys);
        let mut sc = Scratch::default();
        sky.emit(&ctx(last, &c, caret), &mut sc.frame());
        let (star, a0) = sky
            .live_iter()
            .filter(|s| s.lane == StarLane::Field)
            .map(|s| {
                let (x, y) = s.pos(last, false);
                (*s, lum(px(&sc.out, x, y)))
            })
            .max_by_key(|&(_, l)| l)
            .expect("twenty keys at 12 cps lay a field");
        assert!(
            a0 > 0,
            "the brightest field star is dark on the last key's frame"
        );
        let at_px = star.pos(last, false);
        let centre_at = |sky: &mut Stardust, ms: u64| -> u32 {
            let now = last + Duration::from_millis(ms);
            let mut sc = Scratch::default();
            sky.emit(&ctx(now, &c, caret), &mut sc.frame());
            lum(px(&sc.out, at_px.0, at_px.1))
        };
        let (a350, a700, a1000) = (
            centre_at(&mut sky, 350),
            centre_at(&mut sky, 700),
            centre_at(&mut sky, 1000),
        );
        assert!(
            a0 >= a350 && a350 >= a700 && a700 >= a1000,
            "the field star brightened after the hand stopped: {a0} → {a350} → {a700} → {a1000}"
        );
        assert!(
            a700 * 2 <= a0,
            "the field star is still at {a700}/{a0} of its last-key brightness 700 ms after the last key"
        );
        let env = |ms: u64| star.envelope(last + Duration::from_millis(ms), false);
        assert!(
            env(700) <= 0.5 * env(0),
            "the field envelope is {} of its last-key value at +700 ms",
            env(700) / env(0)
        );
        let gone = last + Duration::from_millis(1100);
        let mut sc = Scratch::default();
        sky.emit(&ctx(gone, &c, caret), &mut sc.frame());
        let left = sky
            .live_iter()
            .filter(|s| s.lane == StarLane::Field)
            .count();
        assert_eq!(
            left, 0,
            "{left} field stars are still in the pool 1.1 s after the last key"
        );
        assert_eq!(
            lum(px(&sc.out, at_px.0, at_px.1)),
            0,
            "the field star is still on glass 1.1 s after the last key"
        );
    }

    /// **§5.6 — A FIELD STAR IS PINNED AND FINISHES WITH ITS CELL**. It is
    /// born with no lift; when its cell is retracted by a Backspace it is put
    /// on the cell's own 240 ms retract instead of twinkling on over the
    /// empty space for the rest of its class life.
    #[test]
    fn a_field_star_is_pinned_and_finishes_with_its_cell() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        // A cell whose hash deals a FIELD m2 (the longer-lived one, so the
        // retract finish is what ends it, not its own life).
        let col = (0..120u16)
            .find(|&col| {
                let h = cell_hash(5, col, 0);
                deal(h, SALT_FIELD, FIELD_DEAL_IN, 1.0) && deal(h, SALT_FIELD_M2, FIELD_M2_IN, 1.0)
            })
            .expect("some column deals a field m2");
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (5, col + 1)),
            &ribbon,
        );
        let field: Vec<&Star> = sky
            .live_iter()
            .filter(|s| s.lane == StarLane::Field)
            .collect();
        assert_eq!(
            field.len(),
            1,
            "the dealt cell carries {} field stars",
            field.len()
        );
        assert_eq!(field[0].class, StarClass::M2);
        assert_eq!(field[0].v0, (0.0, 0.0), "a field star lifted");

        // Backspace over it at +50 ms: on the retract by +200 ms it is gone,
        // 1.3 s before its own swoosh-bounded field life would have ended.
        let erase_at = t0 + Duration::from_millis(50);
        sky.on_event(
            &Event::Erase,
            erase_at,
            &ctx(erase_at, &c, (5, col)),
            &ribbon,
        );
        assert!(
            sky.live_iter()
                .any(|s| s.lane == StarLane::Field && s.finish.is_some()),
            "the retracted cell's field star was not put on the retract"
        );
        let later = t0 + Duration::from_millis(200);
        let mut sc = Scratch::default();
        sky.emit(&ctx(later, &c, (5, col)), &mut sc.frame());
        assert!(
            !sky.live_iter().any(|s| s.lane == StarLane::Field),
            "the field star outlived its cell's retract"
        );
    }

    /// **§8.2 — FOCUS LOSS EMBERS THE SKY BESIDE THE RIBBON, IT DOES NOT POP
    /// IT**: every star is still on glass a frame after the blur, dimmer at
    /// 100 ms, and gone by 300 ms.
    #[test]
    fn focus_loss_embers_the_sky_instead_of_popping_it() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = sky();
        for k in 0..3u32 {
            let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = k;
            s.x = 100.0 + 20.0 * k as f32;
            sky.sow(s);
        }
        let blur = t0 + Duration::from_millis(10);
        sky.on_event(&Event::Focus(false), blur, &ctx(blur, &c, (3, 10)), &ribbon);
        let peak = |sky: &mut Stardust, ms: u64| {
            let sc = frame_at(sky, blur + Duration::from_millis(ms), &c);
            (0..3)
                .map(|k| lum(px(&sc.out, 100 + 20 * k, 100)))
                .max()
                .unwrap_or(0)
        };
        let at_1 = peak(&mut sky, 1);
        let at_100 = peak(&mut sky, 100);
        assert!(at_1 > 0, "the sky popped on the blur frame");
        assert!(
            at_100 > 0 && at_100 < at_1,
            "the ember is not a fade: {at_1} → {at_100}"
        );
        assert_eq!(peak(&mut sky, 300), 0, "a star outlived the 300 ms ember");
        assert!(sky.at_rest());
    }

    /// **T6 — IDLE → ZERO** (§18, §20.1).
    ///
    /// No events, no light, no cadence, no deadline — and after the longest
    /// class life the pool empties itself without anyone sweeping it.
    #[test]
    fn an_idle_sky_draws_exactly_nothing() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        let sc = frame_at(&mut sky, t0, &c);
        assert!(sc.out.is_empty() && sc.halos.is_empty() && sc.under.is_empty());
        assert!(sky.at_rest());
        assert!(sky.next_change_deadline(t0).is_none());

        sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
        assert!(!sky.at_rest() && sky.next_change_deadline(t0).is_some());
        let life = Duration::from_secs_f32(StarClass::M1.life_s(StarLane::Strike));
        let sc = frame_at(&mut sky, t0 + life + Duration::from_millis(1), &c);
        assert!(sc.out.is_empty() && sc.halos.is_empty());
        assert!(sky.at_rest() && sky.next_change_deadline(t0).is_none());
    }

    /// **§3.3 / §5.2 — EVERY MARK FORKS TO SOURCE-OVER ON LIGHT** (§20.1).
    ///
    /// One operator flip and nothing else: no additive quad and no additive
    /// halo survives; every light alpha is inside the 236 ceiling and every
    /// ARM lay (the ellipses that are not round) inside the over-text 190;
    /// no lay is emitted below peak 96; and the grain is ONE round dot, not
    /// a cross.
    #[test]
    fn every_v2_mark_forks_to_source_over_on_light() {
        let light = cfg(false);
        let t0 = Instant::now();
        for (class, lane, _) in every_class() {
            let mut sky = sky();
            sky.sow(star(class, lane, t0, spectrum_snap(0.3)));
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(1), &light);
            assert!(
                sc.out.is_empty(),
                "{class:?}/{lane:?} left an additive quad on light"
            );
            assert!(
                !sc.halos.is_empty(),
                "{class:?}/{lane:?} drew nothing on light"
            );
            for h in &sc.halos {
                assert_eq!(
                    h.mode,
                    HaloMode::Over,
                    "{class:?}/{lane:?} emitted an additive halo"
                );
                let a = h.color >> 24;
                assert!(
                    a >= 1 && a as f32 <= LIGHT_ALPHA_CAP,
                    "{class:?}/{lane:?} alpha {a}"
                );
                if h.rx != h.ry {
                    assert!(
                        a as f32 <= InkRole::OverText.alpha_cap(),
                        "{class:?}/{lane:?} needle alpha {a} over the over-text cap"
                    );
                }
            }
            let round = sc.halos.iter().filter(|h| h.rx == h.ry).count();
            if class == StarClass::M3 {
                assert_eq!(sc.halos.len(), round, "a light grain is not one round dot");
                let dots: std::collections::BTreeSet<_> =
                    sc.halos.iter().map(|h| (h.cx, h.cy)).collect();
                assert_eq!(dots.len(), 1);
            } else {
                assert!(sc.halos.len() > round, "a light {class:?} has no arms");
            }
        }
    }

    /// **§5.2 — COUNTS × 0.6 ON LIGHT, IN EVERY POPULATION**. The thrown
    /// populations are thinned too, never below the slots a law fixes: a
    /// fan keeps its hero and four m2 and loses grains; a mini-fan keeps its
    /// m2; a Backspace keeps its m2.
    #[test]
    fn light_thins_every_thrown_population_but_never_a_law_fixed_slot() {
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let census = |dark: bool| -> (usize, usize, usize, usize) {
            let c = cfg(dark);
            let cx = ctx(t0, &c, (5, 40));
            let mut sky = Stardust::new();
            sky.sow_fan(
                t0,
                FanSow {
                    at_px: (300.0, 120.0),
                    reach: 12.0,
                    tint_t: 0.4,
                    seed: 11,
                    n: 18,
                    hero: true,
                },
                &cx,
            );
            let fan_m3 = sky.live_iter().filter(|s| s.class == StarClass::M3).count();
            let fan_fixed = sky.live_iter().filter(|s| s.class != StarClass::M3).count();
            let mut sky = Stardust::new();
            for k in 0..8u32 {
                sky.sow_mini_fan(t0, (300.0 + 20.0 * k as f32, 120.0), k, &cx);
            }
            let mini = sky.live();
            let mut sky = Stardust::new();
            for k in 0..8u16 {
                sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10 + k)), &ribbon);
            }
            (fan_m3, fan_fixed, mini, sky.live())
        };
        let dark = census(true);
        let light = census(false);
        assert_eq!(dark.1, 5, "the dark fan is not hero + 4 m2");
        assert_eq!(light.1, 5, "light thinned a law-fixed fan slot");
        assert!(
            light.0 < dark.0,
            "light did not thin the fan's grains: {light:?} vs {dark:?}"
        );
        assert_eq!(dark.2, 24, "eight dark mini-fans are 24 stars");
        assert!((8..24).contains(&light.2), "light mini-fans: {}", light.2);
        assert_eq!(dark.3, 32, "eight dark Backspaces are 32 stars");
        assert!((8..32).contains(&light.3), "light Backspaces: {}", light.3);
    }

    /// **§5.6 / §6.5 — THE METEOR'S OWN POPULATIONS**.
    ///
    /// The transient entry points meteor.rs mints through: a shed that
    /// sputters `n` grains in the train's own colour, a fan that is one gold
    /// m1 with four m2 and grains in throw order, walked ROYGBIV from the
    /// landing's stop, an Enter landing that is one m2 with grains and no hero
    /// (D8), and a mini-fan that is exactly one m2 with two m3. None of them
    /// touches the glint bucket (§13: the rain is their sound).
    #[test]
    fn the_meteor_lanes_sow_their_published_class_census() {
        let c = cfg(true);
        let t0 = Instant::now();
        let cx = ctx(t0, &c, (5, 40));

        let mut sky = Stardust::new();
        sky.sow_shed(
            t0,
            ShedSow {
                at_px: (300.0, 120.0),
                v_head: (2.0, 0.0),
                tint_t: 0.4,
                seed: 7,
                n: 5,
            },
            &cx,
        );
        assert_eq!(sky.live(), 5);
        assert!(sky.live_iter().all(|s| s.class == StarClass::M3
            && s.lane == StarLane::Shed
            && s.tint == spectrum_snap(0.4)));
        assert_eq!(sky.take_glints().count(), 0, "a shed fragment chimed");

        let fan = |hero: bool, n: u8| {
            let mut sky = Stardust::new();
            sky.sow_fan(
                t0,
                FanSow {
                    at_px: (300.0, 120.0),
                    reach: 12.0,
                    tint_t: 0.4,
                    seed: 11,
                    n,
                    hero,
                },
                &cx,
            );
            sky
        };
        let sky = fan(true, 12);
        let m1 = sky.live_iter().filter(|s| s.class == StarClass::M1).count();
        let m2 = sky.live_iter().filter(|s| s.class == StarClass::M2).count();
        assert_eq!((m1, m2), (1, 4), "the fan is not 1 hero + 4 m2");
        assert!(
            sky.live_iter()
                .any(|s| s.class == StarClass::M1 && s.tint == TINT_GOLD_RGB && s.gold),
            "the fan's own hero is not gold"
        );
        // ROYGBIV in order: the non-hero tints walk the seven stops from the
        // landing's own stop, one stop per throw slot, in the arc's order.
        let start = spectrum_snap_index(0.4);
        let tints: Vec<u32> = sky
            .live_iter()
            .filter(|s| !s.gold)
            .map(|s| s.tint)
            .collect();
        for (k, t) in tints.iter().enumerate() {
            assert_eq!(
                *t,
                spectrum_stop((start + 1 + k) % SPECTRUM_STOPS),
                "slot {k} is off the walk"
            );
        }
        assert!(tints.len() >= 6, "the fan does not walk the arc: {tints:?}");

        let mut sky = fan(true, 12);
        assert_eq!(sky.take_glints().count(), 0, "the fan spent a typing token");

        let enter = fan(false, 7);
        assert_eq!(
            enter
                .live_iter()
                .filter(|s| s.class == StarClass::M1)
                .count(),
            0,
            "an Enter landing has a hero"
        );
        assert_eq!(
            enter
                .live_iter()
                .filter(|s| s.class == StarClass::M2)
                .count(),
            1,
            "an Enter landing is not ONE m2"
        );
        assert_eq!(
            enter
                .live_iter()
                .filter(|s| s.class == StarClass::M3)
                .count(),
            6
        );

        let mut sky = Stardust::new();
        sky.sow_mini_fan(t0, (300.0, 120.0), 5, &cx);
        assert_eq!(sky.live(), 3);
        assert_eq!(
            sky.live_iter().filter(|s| s.class == StarClass::M2).count(),
            1
        );
        assert_eq!(
            sky.live_iter().filter(|s| s.class == StarClass::M3).count(),
            2
        );
    }

    /// **§6.5 layer 11 / §6.12 — A FAN REACHES ITS REACH, A MINI-FAN HOPS ONE
    /// OR TWO PIXELS**, each on its own window. At `FAN_THROW_MS` every fan
    /// star sits at `reach·(0.55..1.05)` from the landing (three-quarters of
    /// the way at half the window — the ease-out); at `MINI_FAN_THROW_MS`
    /// every mini-fan star is 1-2 px out. An integrator on the wrong window
    /// leaves the fan at a quarter of its radius and the mini-fan on its
    /// birth pixel.
    #[test]
    fn a_fan_reaches_its_reach_and_a_mini_fan_hops_one_or_two_pixels() {
        let c = cfg(true);
        let t0 = Instant::now();
        let cx = ctx(t0, &c, (5, 40));
        let origin = (300, 120);
        let mut sky = Stardust::new();
        sky.sow_fan(
            t0,
            FanSow {
                at_px: (300.0, 120.0),
                reach: 12.0,
                tint_t: 0.4,
                seed: 11,
                n: 12,
                hero: true,
            },
            &cx,
        );
        let t_full = t0 + Duration::from_millis(FAN_THROW_MS as u64);
        let t_half = t0 + Duration::from_millis((FAN_THROW_MS / 2.0) as u64);
        for s in sky.live_iter() {
            let d = dist(s.pos(t_full, false), origin);
            assert!(
                (12.0 * FAN_THROW_JITTER_MIN - 1.0..=12.0 * FAN_THROW_JITTER_MAX + 1.0)
                    .contains(&d),
                "a fan star sits {d} px out at the end of its window"
            );
            let h = dist(s.pos(t_half, false), origin);
            assert!(
                (h - 0.75 * d).abs() <= 1.5,
                "the throw is not an ease-out: {h} at T/2 vs {d}"
            );
            // …and pinned afterwards.
            assert_eq!(
                s.pos(t_full, false),
                s.pos(t_full + Duration::from_millis(100), false)
            );
        }
        let mut sky = Stardust::new();
        sky.sow_mini_fan(t0, (300.0, 120.0), 5, &cx);
        let t_full = t0 + Duration::from_millis(MINI_FAN_THROW_MS as u64);
        for s in sky.live_iter() {
            let d = dist(s.pos(t_full, false), origin);
            assert!(
                (1.0..=3.0).contains(&d),
                "a mini-fan star sits {d} px out — not 1-2 px"
            );
        }
    }

    /// A seed whose luminance phase is AT ITS TROUGH on frame 0 — and, since
    /// the arm clock shares `φ`, whose arm is at its floor there too: the
    /// birth the offline judge measured as "fan stars 1-2 px at peak 61-122".
    fn trough_seed() -> u32 {
        (0u32..)
            .find(|&k| {
                (hash01(mix32(k ^ SALT_PHASE)) * std::f32::consts::TAU)
                    .sin()
                    .abs()
                    < 0.05
            })
            .expect("some seed puts the luminance clock at its trough")
    }

    /// **§5.6 / §5.5 — A STAR IS BORN AT FULL AND SCINTILLATES ONCE ITS HOLD
    /// IS SPENT** ([`Star::in_hold`]). A fan m2 born at the clocks' trough
    /// still draws its published 80 at the centre and its nominal 7-px body
    /// (arm 3) on frame 0 and through its 30 ms hold; one frame past the
    /// hold the two clocks own it — the centre has dropped toward the
    /// trough's `0.70·c` stack and the arm to the band's 2. Before the hold
    /// the same star was born at 56 and 5 px: an m2 that the ground ate.
    #[test]
    fn a_star_is_born_at_full_and_scintillates_once_its_hold_is_spent() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut first = sky();
        // A WARM coloured tint: the base phase (so `trough_seed` is the
        // star's own trough), and stubs that are not white, so the white
        // body's extent below is the body's alone.
        let tint = spectrum_stop(1);
        let mut s = star(StarClass::M2, StarLane::Fan, t0, tint);
        s.seed = trough_seed();
        assert!(s.is_warm(), "the fixture's tint must take the base phase");
        assert!(s.twinkle(t0) < 0.58, "the fixture is not at its trough");
        first.sow(s);
        let hold_ms = HOLD_M2_TRANSIENT_MS as u64;
        // The body's extent on the centre row (the body is `M2_BODY_TINT`
        // of the way to the tint — its own colour, not the stubs'), and the
        // centre itself.
        let body = mix_rgb(TINT_WHITE_RGB, tint, M2_BODY_TINT);
        let read = |sc: &Scratch| -> (u32, i32) {
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for q in sc.out.iter().filter(|q| {
                i32::from(q.y) == 100 && q.color == premul_rgb(body, lum(q.color) as u8)
            }) {
                lo = lo.min(i32::from(q.x));
                hi = hi.max(i32::from(q.x) + i32::from(q.w));
            }
            (lum(px(&sc.out, 200, 100)), hi - lo)
        };
        let nominal = star_arm(geom().ch as f32, StarClass::M2.arm_ratio()).round() as i32;
        assert_eq!(nominal, 3, "the fixture's own ladder: m2 arm 2.52 → 3");
        for ms in [0, hold_ms / 2, hold_ms - 1] {
            let sc = frame_at(&mut first, t0 + Duration::from_millis(ms), &c);
            let (centre, extent) = read(&sc);
            assert!(
                (79..=81).contains(&centre),
                "at {ms} ms, inside the hold, the m2's centre is {centre}, not its published 80"
            );
            assert_eq!(
                extent,
                2 * nominal + 1,
                "at {ms} ms, inside the hold, the m2 is {extent} px across, not its nominal"
            );
        }
        let sc = frame_at(&mut first, t0 + Duration::from_millis(hold_ms + 1), &c);
        let (centre, _) = read(&sc);
        assert!(
            centre <= 70,
            "one frame past the hold the centre is still {centre}: the luminance clock never \
             took over"
        );
        // The arm clock is seeded at 8-12 Hz, so one frame past the hold it
        // may legitimately sit at its own peak; within one of its cycles
        // (≤ 63 ms) the arm must have dipped below the nominal it held.
        let dipped = (hold_ms + 1..=hold_ms + 63).any(|ms| {
            read(&frame_at(&mut first, t0 + Duration::from_millis(ms), &c)).1 < 2 * nominal + 1
        });
        assert!(
            dipped,
            "the arm stayed at its nominal for a whole cycle past the hold: the arm clock never \
             took over"
        );
        // And the whole trace is a pure function of `(birth, seed, now)`: a
        // second sowing of the same star reads byte-identical at every ms.
        let mut again = sky();
        let mut s2 = star(StarClass::M2, StarLane::Fan, t0, tint);
        s2.seed = trough_seed();
        again.sow(s2);
        for ms in (0..(StarClass::M2.life_s(StarLane::Fan) * 1000.0) as u64).step_by(7) {
            let now = t0 + Duration::from_millis(ms);
            assert_eq!(
                frame_at(&mut first, now, &c).out,
                frame_at(&mut again, now, &c).out,
                "the hold made the star's trace non-deterministic at {ms} ms"
            );
        }
    }

    /// **§6.5 layer 11, on glass — THE FIVE ARE CLEAR OF THE CARET CELL
    /// BEFORE THEIR HOLD IS SPENT** ([`FAN_THROW_MS`]). The hero and the four
    /// m2 are law-fixed and born on the landing cell, which the host paints
    /// its opaque caret over (layer 12 above 11): a throw that has not
    /// carried them off the cell by the end of their hold has spent the
    /// star's one promised full-bright span invisible. Asked of the meteor's
    /// floor jump (8 cells, `reach = 2.55 ch`) at the m2's hold, on every
    /// seed of a dozen landings — the throw angle is seeded, so this sweeps
    /// the steep rays too.
    #[test]
    fn a_fan_star_is_born_at_full_and_clears_its_cell_inside_its_hold() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom();
        let cx = ctx(t0, &c, (5, 40));
        let (ox, oy) = g.cell_center(5, 40);
        let origin = (ox.round() as i32, oy.round() as i32);
        let reach = 2.55 * g.ch as f32;
        let half = (g.cw as i32 / 2, g.ch as i32 / 2);
        let at = t0 + Duration::from_millis(HOLD_M2_TRANSIENT_MS as u64);
        for seed in 0..12u32 {
            let mut sky = Stardust::new();
            sky.sow_fan(
                t0,
                FanSow {
                    at_px: (ox, oy),
                    reach,
                    tint_t: 0.3,
                    seed: seed.wrapping_mul(0x9E37_79B9),
                    n: 9,
                    hero: true,
                },
                &cx,
            );
            for s in sky.live_iter().filter(|s| s.class != StarClass::M3) {
                assert!(
                    s.in_hold(at - Duration::from_millis(1)),
                    "the fixture's own hold"
                );
                let (x, y) = s.pos(at, false);
                let (dx, dy) = ((x - origin.0).abs(), (y - origin.1).abs());
                assert!(
                    dx > half.0 || dy > half.1,
                    "seed {seed}: a {:?} fan star is still on the caret cell ({dx}, {dy} px \
                     from its centre) at the end of its hold",
                    s.class
                );
            }
        }
    }

    /// **§5.6 — AN ERASE IS A THROW OVER 80 ms, THEN PINNED**: 2-4 px out by
    /// `ERASE_THROW_MS`, and not a pixel further after it. A drag on τ 90
    /// would leave it at 63 % of its reach and still creeping.
    #[test]
    fn an_erase_throw_reaches_its_reach_by_eighty_ms_then_pins() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(5, &[false; 120]);
        sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10)), &ribbon);
        assert_eq!(sky.live(), 4, "a Backspace is 3 m3 + 1 m2");
        let (ox, oy) = geom().cell_center(5, 10);
        let origin = (ox.round() as i32, oy.round() as i32);
        let t_full = t0 + Duration::from_millis(ERASE_THROW_MS as u64);
        let scale = px_scale(geom().ch as f32);
        for s in sky.live_iter() {
            let d = dist(s.pos(t_full, false), origin);
            assert!(
                (ERASE_REACH_MIN_PX * scale - 1.0..=ERASE_REACH_MAX_PX * scale + 1.0).contains(&d),
                "an erase star sits {d} px out at 80 ms"
            );
            assert_eq!(
                s.pos(t_full, false),
                s.pos(t_full + Duration::from_millis(60), false)
            );
            assert!(
                s.pos(t_full, false).1 <= origin.1,
                "an erase star dived into the line"
            );
        }
    }

    /// **§5.6 / §8.2 — A KILL THROWS ONE STAR PER THREE CELLS, CAP EIGHT,
    /// ALONG THE DRAINED SPAN** — a swoosh, not a 32-star shower from one
    /// cell. A Backspace stays 3 m3 + 1 m2.
    #[test]
    fn a_kill_throws_one_star_per_three_cells_capped_at_eight() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let kill = |cells: u16| {
            let mut sky = Stardust::new();
            sky.on_event(
                &Event::Kill {
                    cells,
                    scope: KillScope::Line,
                },
                t0,
                &ctx(t0, &c, (5, 10)),
                &ribbon,
            );
            sky
        };
        assert_eq!(kill(2).live(), 1);
        assert_eq!(kill(6).live(), 2);
        assert_eq!(kill(24).live(), 8);
        let big = kill(60);
        assert_eq!(big.live(), 8, "^U on 60 cells threw {} stars", big.live());
        let cols: std::collections::BTreeSet<i32> = big.live_iter().map(col_of).collect();
        assert!(cols.len() >= 6, "the kill's stars are clumped on {cols:?}");
        assert!(
            cols.iter().all(|&col| (10..=34).contains(&col)),
            "a kill star left the drained span: {cols:?}"
        );
        assert_eq!(
            big.live_iter().filter(|s| s.class == StarClass::M2).count(),
            2,
            "3 m3 : 1 m2"
        );
    }

    /// **§18 — TWO BACKSPACES AT ONE CELL THROW DIFFERENT DEBRIS**. Every
    /// seed is hashed from `(row, col, born)`; an erase seed with no per-birth
    /// term repeats the same four stars, tints, angles and phases every time.
    #[test]
    fn two_backspaces_at_one_cell_throw_different_debris() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10)), &ribbon);
        let first: std::collections::BTreeSet<u32> = sky.live_iter().map(|s| s.seed).collect();
        sky.on_event(&Event::Erase, t0, &ctx(t0, &c, (5, 10)), &ribbon);
        let both: std::collections::BTreeSet<u32> = sky.live_iter().map(|s| s.seed).collect();
        assert_eq!(first.len(), 4);
        assert_eq!(
            both.len(),
            8,
            "the second Backspace reused the first's seeds"
        );
    }

    /// **§18 — HALOS ARE BUDGETED PER STAR, NOT PER SPLIT**. Seventeen m2s
    /// straddling a row boundary: sixteen carry an atmosphere (two `RainHalo`
    /// splits each — 32 in the stream), the seventeenth none. A budget that
    /// counted splits would give the ninth m2 no atmosphere.
    #[test]
    fn halos_are_budgeted_per_star_not_per_split() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        for k in 0..17u32 {
            let mut s = star(StarClass::M2, StarLane::Strike, t0, spectrum_snap(0.1));
            s.seed = peak_seed();
            s.x = 60.0 + 20.0 * k as f32;
            s.y = 93.0; // rows 2 | 3 meet at y = 94
            sky.sow(s);
        }
        let sc = frame_at(&mut sky, t0, &c);
        let haloed: std::collections::BTreeSet<u16> = sc.halos.iter().map(|h| h.cx).collect();
        assert_eq!(
            haloed.len(),
            STARDUST_HALO_BUDGET,
            "{} stars carry a halo",
            haloed.len()
        );
        assert!(
            sc.halos.len() > STARDUST_HALO_BUDGET,
            "the halos did not split across the row boundary"
        );
    }

    /// **§18 — AN OVER-BUDGET FRAME THINS EVERY STAR'S EXTREMITIES, NOT WHOLE
    /// STARS**. A FULL pool — forty heroes and, at the tail, the eight grains
    /// the finish slack admits — asks for ~900 quads against a 284 budget:
    /// every one of the forty-eight still has its centre pixel lit, and the
    /// frame lands at or under the budget with the grains drawn last, where a
    /// recipe that ignored its share would overrun it.
    #[test]
    fn an_over_budget_frame_keeps_every_star_s_core() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut sky = sky();
        for k in 0..STAR_CAP as u32 {
            let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            s.x = 20.0 + 24.0 * k as f32;
            sky.sow(s);
        }
        for k in 0..STAR_FINISH_SLACK as u32 {
            let mut s = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.seed = peak_seed();
            s.x = 20.0 + 24.0 * k as f32;
            s.y = 160.0; // row 6, clear of the heroes' arms and stubs
            sky.sow(s);
        }
        assert_eq!(sky.live(), STAR_POOL_MAX, "the fixture is not a full pool");
        let sc = frame_at(&mut sky, t0, &c);
        assert!(
            sc.out.len() <= STARDUST_QUAD_BUDGET,
            "{} quads over the budget",
            sc.out.len()
        );
        for k in 0..STAR_CAP as i32 {
            assert!(
                lum(px(&sc.out, 20 + 24 * k, 100)) > 0,
                "hero {k} lost its core to the budget"
            );
        }
        for k in 0..STAR_FINISH_SLACK as i32 {
            assert!(
                lum(px(&sc.out, 20 + 24 * k, 160)) > 0,
                "grain {k} lost its core to the budget"
            );
        }
    }

    /// **§18 — A GRAIN'S SHARE IS HONOURED PUSH BY PUSH, CORE FIRST.** `emit`
    /// hands `draw_m3` the grain's own share of the frame's quad budget: a
    /// share of one draws the core alone, two the core and one arm, five the
    /// whole cross — never a sixth quad, and never an arm before the core. A
    /// recipe that pushed its five rects unconditionally would overrun the
    /// frame's budget by two quads on a tight tail.
    #[test]
    fn a_grain_s_share_is_honoured_push_by_push_core_first() {
        let c = cfg(true);
        let t0 = Instant::now();
        let sky = sky();
        let grain = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
        let paint = Paint::of(&grain, sky.probe(), &ctx(t0, &c, (3, 10)))
            .expect("a newborn grain has something to draw");
        for share in 1..=M3_QUADS + 1 {
            let mut out = Vec::new();
            paint.draw_m3(geom(), &mut out, share);
            assert_eq!(
                out.len(),
                share.min(M3_QUADS),
                "a share of {share} drew {} quads",
                out.len()
            );
            let core = &out[0];
            assert_eq!(
                (
                    i32::from(core.x),
                    i32::from(core.y),
                    i32::from(core.w),
                    i32::from(core.h)
                ),
                (200, 100, 1, 1),
                "the first quad of a share of {share} is not the 1×1 core"
            );
        }
    }

    /// **§6.11 — REDUCED MOTION IS STATIC AND TAKES THE ONE LINEAR FADE**.
    ///
    /// Both clocks stop, the star does not move, and its alpha runs to zero
    /// linearly over the last [`REDUCED_MOTION_FADE_MS`] of its life. It is
    /// the theme's ONE exception to the seven named curves, and it stays one.
    #[test]
    fn reduced_motion_freezes_both_clocks_and_takes_the_linear_fade() {
        let mut c = cfg(true);
        c.reduced_motion = true;
        let t0 = Instant::now();
        let mut sky = sky();
        let mut s = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        s.v0 = (0.0, sky_lift_px_per_s(geom().ch as f32) / 1000.0);
        sky.sow(s);
        let life_ms = (StarClass::M2.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut widths = std::collections::BTreeSet::new();
        for ms in (0..life_ms - 130).step_by(7) {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
            widths.insert(sc.out.iter().map(|q| q.w).max().unwrap_or(0));
            // Static: the centre pixel never moves.
            assert!(
                lum(px(&sc.out, 200, 100)) > 0,
                "the mark drifted at {ms} ms"
            );
        }
        assert_eq!(
            widths.len(),
            1,
            "the arm clock ran under reduced motion: {widths:?}"
        );
    }

    /// Seam point 12: a scroll moves every star with the viewport and drops
    /// the ones that leave the grid; the probe is forgotten, because a stale
    /// row answered as blank would be a star over ink.
    #[test]
    fn a_scroll_moves_the_sky_and_forgets_the_probe() {
        let t0 = Instant::now();
        let mut sky = sky();
        let mut high = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        high.y = 50.0;
        sky.sow(high);
        sky.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
        sky.last_hero = Some((3, 4));
        sky.translate_scroll(3, 18);
        assert_eq!(sky.live(), 1, "a star that left the grid was kept");
        assert_eq!(sky.live_iter().next().map(|s| s.y), Some(46.0));
        assert_eq!(sky.last_hero, Some((0, 4)));
        assert!(sky.probe().is_empty(), "the probe survived a scroll");
        sky.translate_scroll(1, 18);
        assert_eq!(sky.last_hero, None);
    }

    /// Seam point 12, the band path: Codex's viewport `[4..10]` sliding
    /// down one row carries the star on row 5 to row 6, the veil on row 5
    /// to row 6 and the hero memory with it, drops the veil on row 10 that
    /// the slide pushed past the band's edge, leaves the star on row 2
    /// (outside the band) exactly where it was, and forgets the probe — the
    /// host re-probes before the next deal, and a stale row answered as
    /// blank would be a star over ink. Then the pinned phase: the transcript
    /// `[0..3]` archives up and only the row-2 star rides; two more rows and
    /// it leaves through the top, gone rather than pinned to row 0.
    #[test]
    fn a_band_move_moves_stars_in_the_band_and_clears_the_probe() {
        let t0 = Instant::now();
        let mut sky = sky();
        assert!(!sky.probe().is_empty(), "fixture: rows 2-4 are probed");
        let mut high = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        high.y = 50.0;
        sky.sow(high);
        sky.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
        for row in [5u16, 10] {
            sky.veil.push(Veil {
                row,
                col: 4,
                t: 0.3,
                gain: 1.0,
                born: t0,
                lit: t0,
                finish: None,
            });
        }
        sky.last_hero = Some((5, 4));

        sky.translate_band(4, 10, 1, 18, 0);

        assert_eq!(sky.live(), 2, "no star inside or outside the band was lost");
        let ys: Vec<f32> = sky.live_iter().map(|s| s.y).collect();
        assert!(
            ys.contains(&50.0),
            "the row-2 star is outside the band: {ys:?}"
        );
        assert!(ys.contains(&118.0), "the row-5 star rode to row 6: {ys:?}");
        assert_eq!(
            sky.veil.iter().map(|v| v.row).collect::<Vec<_>>(),
            vec![6],
            "the row-5 veil rode to row 6 and the row-10 veil left the band"
        );
        assert_eq!(sky.last_hero, Some((6, 4)));
        assert!(sky.probe().is_empty(), "the probe survived a band move");

        sky.translate_band(0, 3, -1, 18, 0);
        let ys: Vec<f32> = sky.live_iter().map(|s| s.y).collect();
        assert!(ys.contains(&32.0) && ys.contains(&118.0), "{ys:?}");
        assert_eq!(
            sky.last_hero,
            Some((6, 4)),
            "a hero outside the band is untouched"
        );
        sky.translate_band(0, 3, -2, 18, 0);
        assert_eq!(sky.live(), 1, "a star that left the band was kept");
        assert_eq!(sky.live_iter().next().map(|s| s.y), Some(118.0));

        // No geometry yet: stars fail closed, rows still translate.
        let mut cold = Stardust::new();
        cold.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
        cold.last_hero = Some((5, 4));
        cold.translate_band(4, 10, 1, 0, 0);
        assert_eq!(cold.live(), 0);
        assert_eq!(cold.last_hero, Some((6, 4)));
    }

    #[test]
    fn band_moves_follow_the_earning_glyph_at_sky_and_footer_edges() {
        let now = Instant::now();
        let ribbon = Ribbon::new();
        let mut caught_pixel_only = 0;
        for tall in [false, true] {
            let mut config = cfg(true);
            config.ribbon_tall = tall;
            for top in [0u16, 4] {
                let bottom = top + 6;
                for home in [top, top + 1, bottom, bottom + 1] {
                    for delta in [-1i16, 1] {
                        let mut sky = Stardust::new();
                        if let Some(above) = home.checked_sub(1) {
                            sky.probe_mut().probe_row(i32::from(above), &[false; 120]);
                        }
                        sky.probe_mut().probe_row(i32::from(home), &[false; 120]);
                        let context = ctx(now, &config, (home, 10));
                        assert!(sky.deal_star(
                            Deal {
                                class: StarClass::M2,
                                lane: StarLane::Field,
                                cell: (home, 10),
                                earned: false,
                                pull: None,
                                tint: None,
                            },
                            now,
                            &context,
                            &ribbon
                        ));
                        let before = *sky.live_iter().next().unwrap();
                        assert_eq!(before.home_row, Some(home));
                        let moved = i32::from(home) + i32::from(delta);
                        let expected = if home > bottom {
                            Some(home)
                        } else if moved < i32::from(top) || moved > i32::from(bottom) {
                            None
                        } else {
                            Some(moved as u16)
                        };
                        sky.translate_band(top, bottom, delta, geom().ch as u16, geom().origin_y);
                        if let Some(row) = expected {
                            let after = sky.live_iter().next().unwrap();
                            assert_eq!(after.home_row, Some(row));
                            let wanted_y = before.y
                                + (i32::from(row) - i32::from(home)) as f32 * geom().ch as f32;
                            assert_eq!(after.y, wanted_y);
                            assert_eq!(
                                (after.born, after.lit, after.seed),
                                (before.born, before.lit, before.seed)
                            );
                            // The rejected implementation uses the star's sky
                            // pixel as though it were the earning glyph row.
                            let mut wrong_y = before.y;
                            let retained =
                                BandPx::new(top, bottom, delta, geom().ch as u16, geom().origin_y)
                                    .point(&mut wrong_y);
                            if !retained || wrong_y != wanted_y {
                                caught_pixel_only += 1;
                            }
                        } else {
                            assert_eq!(sky.live(), 0, "a star outlived its ejected glyph row");
                        }
                    }
                }
            }
        }
        assert!(
            caught_pixel_only >= 4,
            "the fixture must catch pixel-row ownership at both band edges"
        );
    }

    #[test]
    fn sky_ownership_survives_uniform_scroll_before_a_band_move() {
        let now = Instant::now();
        let config = cfg(true);
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(6, &[false; 120]);
        sky.probe_mut().probe_row(7, &[false; 120]);
        assert!(sky.deal_star(
            Deal {
                class: StarClass::M2,
                lane: StarLane::Field,
                cell: (7, 10),
                earned: false,
                pull: None,
                tint: None,
            },
            now,
            &ctx(now, &config, (7, 10)),
            &Ribbon::new()
        ));
        let before = *sky.live_iter().next().unwrap();
        sky.translate_scroll(1, geom().ch as u16);
        assert_eq!(sky.live_iter().next().unwrap().home_row, Some(6));
        sky.translate_band(6, 10, 1, geom().ch as u16, geom().origin_y);
        let after = sky.live_iter().next().unwrap();
        assert_eq!((after.home_row, after.y), (Some(7), before.y));
        sky.translate_scroll(8, geom().ch as u16);
        assert_eq!(sky.live(), 0, "an ejected owner retires its sky star");
    }

    // -- §18 cadence: the sky's clocks are solved, not polled ---------------

    /// **THE SIZE CLOCK'S NEXT STEP IS SOLVED IN CLOSED FORM** (§18's
    /// cadence law, §5.5's arm clock): for every class, several seeded
    /// rates and phases, and several instants past the hold, the instant
    /// [`Star::next_arm_step_s`] names is the first at which [`arm_px`]
    /// actually changes, to within 0.1 ms — and it is never more than one
    /// `1/(2f)` period (62.5 ms at `f = 8`) away.
    #[test]
    fn the_size_clocks_next_step_is_solved_not_polled() {
        let t0 = Instant::now();
        let ch = geom().ch as f32;
        let mut checked = 0usize;
        for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
            for (f, seed) in [(8.0, 0x11u32), (10.3, 0x2222), (12.0, 0x3333_3333)] {
                let mut s = star(class, StarLane::Strike, t0, TINT_WHITE_RGB);
                s.f = f;
                s.seed = seed;
                let hold = class.envelope_s(StarLane::Strike).0;
                for k in 0..7 {
                    let now = t0 + Duration::from_secs_f32(hold + 0.013 * k as f32);
                    let Some(dt) = s.next_arm_step_s(ch, now) else {
                        continue;
                    };
                    assert!(
                        dt > 0.0 && dt <= 0.5 / SCINT_F_MIN + 1e-4,
                        "{class:?} f={f}: step {dt} s outside (0, 1/(2f)]"
                    );
                    let here = arm_px(&s, ch, false, now);
                    let mut polled = None;
                    let mut t = 0.0f32;
                    while t < 0.2 {
                        t += 0.000_05;
                        if arm_px(&s, ch, false, now + Duration::from_secs_f32(t)) != here {
                            polled = Some(t);
                            break;
                        }
                    }
                    let polled = polled.expect("the polled arm never stepped");
                    assert!(
                        (dt - polled).abs() < 1e-4,
                        "{class:?} f={f} k={k}: solved {dt} s, polled {polled} s"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked >= 40, "only {checked} steps checked");
    }

    /// **A GOLD STAR'S GLINT TOGGLE IS SOLVED IN CLOSED FORM** (§5.2's
    /// "gold, at twinkle peak only"): the instant [`Star::next_glint_toggle_s`]
    /// names is the first at which [`Star::glinting`] flips, to within
    /// 0.1 ms; a star that is not gold names none.
    #[test]
    fn the_glint_toggle_is_solved_not_polled() {
        let t0 = Instant::now();
        let mut gold = star(StarClass::M1, StarLane::Strike, t0, TINT_GOLD_RGB);
        for (k, seed) in [0x5u32, 0x77, 0xABCD].into_iter().enumerate() {
            gold.seed = seed;
            let now = t0 + Duration::from_millis(30 * k as u64 + 1);
            let dt = gold.next_glint_toggle_s(now).expect("gold toggles");
            let here = gold.glinting(now);
            let mut t = 0.0f32;
            let polled = loop {
                t += 0.000_05;
                if gold.glinting(now + Duration::from_secs_f32(t)) != here {
                    break t;
                }
                assert!(t < 1.0, "the glint never toggled");
            };
            assert!(
                (dt - polled).abs() < 1e-4,
                "seed {seed:#x}: solved {dt} s, polled {polled} s"
            );
        }
        let white = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
        assert!(white.next_glint_toggle_s(t0).is_none());
    }

    // -- the coloured sky and the aurora (owner, 2026-09-08) ----------------

    /// The example renderer's dark ground, `(17, 19, 24)` — the default dark
    /// theme the scanner reads.
    const GROUND: u32 = 0x0011_1318;

    /// The scanner's own floor for "this pixel is COLOUR": a channel spread
    /// of at least this…
    const SCAN_SPREAD_FLOOR: u32 = 40;

    /// …at a max channel of at least this.
    const SCAN_LIT_FLOOR: u32 = 60;

    /// Composite one frame's `out` quads and halos over the dark ground on a
    /// `w × h` canvas whose top-left is `(x0, y0)` — the CPU reference's own
    /// arithmetic (`add_sat` for additive quads and `Add` halos through the
    /// shared `halo_row_ny` / `halo_weight` falloff), so a test measures the
    /// pixel the eye gets.
    fn composite(sc: &Scratch, x0: i32, y0: i32, w: usize, h: usize) -> Vec<u32> {
        composite_over(sc, GROUND, x0, y0, w, h)
    }

    /// The shipped dark theme's ground, `#1A1B26` — the one `cursor_glow`'s
    /// whole-frame gates composite over, and the one the A-type hero's
    /// `#ACABB9` was measured on.
    const SHIPPED_GROUND: u32 = 0x001A_1B26;

    /// [`composite`] over a chosen ground.
    fn composite_over(sc: &Scratch, ground: u32, x0: i32, y0: i32, w: usize, h: usize) -> Vec<u32> {
        let mut canvas = vec![ground; w * h];
        for q in &sc.out {
            for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                    let (cx, cy) = (x - x0, y - y0);
                    if cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                        continue;
                    }
                    let p = &mut canvas[cy as usize * w + cx as usize];
                    *p = add_sat(*p, q.color);
                }
            }
        }
        for a in &sc.halos {
            assert_eq!(a.mode, HaloMode::Add, "a dark frame emitted an Over halo");
            let (rx2, ry2) = (
                i32::from(a.rx) * i32::from(a.rx),
                i32::from(a.ry) * i32::from(a.ry),
            );
            for y in i32::from(a.y)..i32::from(a.y) + i32::from(a.h) {
                let ny = halo_row_ny(y - i32::from(a.cy), ry2);
                if ny >= 256 {
                    continue;
                }
                for x in i32::from(a.x)..i32::from(a.x) + i32::from(a.w) {
                    let wt = halo_weight(x - i32::from(a.cx), ny, rx2).min(255);
                    let (cx, cy) = (x - x0, y - y0);
                    if wt == 0 || cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                        continue;
                    }
                    let p = &mut canvas[cy as usize * w + cx as usize];
                    *p = add_sat(*p, premul_rgb(a.color, wt as u8));
                }
            }
        }
        canvas
    }

    /// The aurora's halos on a dark frame — the wide ellipses; a star's halo
    /// is round.
    fn veils(sc: &Scratch) -> Vec<RainHalo> {
        sc.halos.iter().filter(|h| h.rx != h.ry).copied().collect()
    }

    /// **STARDUST IS COLOURED ON THE GLASS, NOT WHITE WITH A TINT** (owner,
    /// 2026-09-08: "I feel like you are diminishing the specialness and
    /// emphasis of this theme? why?").
    ///
    /// Forty keys at 12 cps on the engine's own spine, the field walking
    /// the whole arc; from key 20 every frame is composited over the dark
    /// ground and read by the scanner's rule — a STAR pixel is one at a max
    /// channel ≥ 60, and it is COLOUR if its channel spread is ≥ 40. At
    /// least 60 % of the star pixels are colour. Under the 14 % halo cap
    /// with white cores the sky measured **30 %** (457 of 1 486 star pixels
    /// — this test, run before the change); the coloured sky measures the
    /// share this test prints.
    #[test]
    fn stardust_is_coloured_on_the_glass_not_white_with_a_tint() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let (mut lit, mut coloured) = (0usize, 0usize);
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            cx.caret_t = f32::from(k) / 40.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            if k < 20 {
                continue;
            }
            // Rows 3-5 of the fixture: the sky band, the line, and below.
            let canvas = composite(&sc, 0, 94, 1080, 54);
            for &p in &canvas {
                if lum(p) >= SCAN_LIT_FLOOR {
                    lit += 1;
                    coloured += usize::from(chroma(p) >= SCAN_SPREAD_FLOOR);
                }
            }
        }
        eprintln!(
            "scanner: {coloured} of {lit} star pixels are colour ({}%)",
            coloured * 100 / lit.max(1)
        );
        assert!(lit >= 200, "only {lit} star pixels over twenty frames");
        assert!(
            coloured * 100 >= lit * 60,
            "only {coloured} of {lit} star pixels ({}%) are colour by the scanner's rule — \
             the sky reads as white dust with a tint",
            coloured * 100 / lit.max(1)
        );
    }

    /// **NO STAR OUTSHINES THE CARET'S FLOOR SHARE ON THE SHIPPED GROUND**
    /// (`cursor_glow` §8 d, *"you must always be able to find your
    /// cursor"*; [`STAR_LIGHT_CEIL`]).
    ///
    /// Every class in both lanes, every tint the deal can hand out — the
    /// seven stops, gold and the A-type white — at the default intensity
    /// and at full, one star at a time over `#1A1B26`: its whole life
    /// composited with its halo in the renderers' bytes and read in
    /// relative luminance, and no pixel of it reaches past `72/255`, the
    /// share of the caret's `80` floor the field is allowed. Non-vacuous:
    /// the white hero at full intensity stands within one level of the
    /// ceiling, so the law is measured binding — before the pricing that
    /// star's bare `143` body composited to `Y = 100` on this ground, and
    /// with its halo the 18 ms burst's A-type hero to `106`.
    ///
    /// One star at a time on purpose: what the deal stacks on ONE cell —
    /// a strike star, its second grain and a hot-hand field star can land
    /// within two pixels of each other — is priced star by star over the
    /// sky laid before it ([`sky_ground`]) and is not held here; see the
    /// note on [`sky_ground`].
    #[test]
    fn no_star_outshines_the_caret_s_floor_share_on_the_shipped_ground() {
        let t0 = Instant::now();
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let tints: Vec<u32> = (0..SPECTRUM_STOPS)
            .map(spectrum_stop)
            .chain([TINT_GOLD_RGB, TINT_WHITE_RGB])
            .collect();
        let mut worst = (0.0f32, String::new());
        let mut white_hero_at_full = 0.0f32;
        for intensity in [0.7f32, 1.0] {
            let mut c = cfg(true);
            c.theme_bg = SHIPPED_GROUND;
            c.intensity = intensity;
            for (class, lane, _) in every_class() {
                for &tint in &tints {
                    let mut sky = sky();
                    sky.sow(star(class, lane, t0, tint));
                    let life_ms = (class.life_s(lane) * 1000.0) as u64;
                    let mut peak = 0.0f32;
                    for ms in (0..life_ms).step_by(5) {
                        let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
                        // A 60 px window on the star at (200, 100).
                        for &p in &composite_over(&sc, SHIPPED_GROUND, 170, 70, 60, 60) {
                            peak = peak.max(relative_luminance(p) * 255.0);
                        }
                    }
                    if peak > worst.0 {
                        worst = (
                            peak,
                            format!("{class:?}/{lane:?} {tint:#08x} at intensity {intensity}"),
                        );
                    }
                    if class == StarClass::M1
                        && lane == StarLane::Strike
                        && tint == TINT_WHITE_RGB
                        && intensity == 1.0
                    {
                        white_hero_at_full = peak;
                    }
                }
            }
        }
        eprintln!(
            "the brightest star pixel over the shipped ground: Y {:.1} of the ceiling {ceil:.1} — {}",
            worst.0, worst.1
        );
        assert!(
            worst.0 <= ceil,
            "{} reached Y {:.1} over the caret law's {ceil:.1}",
            worst.1,
            worst.0
        );
        assert!(
            white_hero_at_full >= ceil - 1.0,
            "NON-VACUOUS: the white hero at full intensity stands at Y {white_hero_at_full:.1}, \
             not against the ceiling {ceil:.1}"
        );
    }

    /// **THE CLOSER DIMS A STAR THAT STACKS OVER THE CEILING AWAY FROM ITS
    /// CENTRE, BY ONE FACTOR, AND LEAVES ONE THAT FITS ALONE**
    /// ([`settle_under_ceiling`]).
    ///
    /// A synthetic star: a white centre lay under the ceiling and a
    /// full-request yellow stub beside a yellow halo's ring — the pixel a
    /// yellow hero's arm stacks — over the shipped ground. The centre is
    /// lawful and the stub pixel is not; after the closer every watched
    /// pixel is under the ceiling, the stub pixel stands within two levels
    /// of it (the factor is the largest that fits, on the bytes), every own
    /// lay carries the SAME factor to within the byte's rounding, the halo's
    /// colour is still the pure stop, and a quad laid before the star is
    /// not touched. A star that already fits is byte-identical afterwards.
    #[test]
    fn the_closer_dims_a_star_by_one_factor_until_its_arms_fit_and_leaves_one_that_fits() {
        let g = geom();
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let y = |p: u32| relative_luminance(p) * 255.0;
        let yellow = spectrum_stop(2);
        let mut sc = Scratch::default();
        // An earlier star's quad, off to the side: the closer's slice starts
        // after it.
        push_fx_rect(&mut sc.out, g, 300, 100, 1, 1, 0x0030_3030);
        let q0 = sc.out.len();
        // A priced centre (white 40 under a 61 yellow halo is Y 52), and the
        // arm's first pixel as the hero lays it: the taper's first span at
        // `0.87 · 40`, the tint stub at the full 61, the halo's ring at 0.92.
        push_fx_rect(
            &mut sc.out,
            g,
            200,
            100,
            1,
            1,
            premul_rgb(TINT_WHITE_RGB, 40),
        );
        push_fx_rect(&mut sc.out, g, 203, 100, 2, 1, premul_rgb(yellow, 61));
        push_fx_rect(
            &mut sc.out,
            g,
            203,
            100,
            1,
            1,
            premul_rgb(TINT_WHITE_RGB, 35),
        );
        let h0 = sc.halos.len();
        push_halo_quads(
            &mut sc.halos,
            g,
            usize::MAX,
            (200, 100),
            (15, 15),
            premul_rgb(yellow, 61),
            HaloMode::Add,
        );
        // One piece per text row the 15 px halo spans.
        assert!(sc.halos.len() > h0, "no halo was laid");
        assert!(
            sc.halos[h0..].iter().all(|h| h.color == sc.halos[h0].color),
            "the pieces of one halo carry one colour"
        );
        let before: Vec<u32> = sc.out[q0..].iter().map(|q| q.color).collect();
        let halo_before = sc.halos[h0].color;
        let watch: [((i32, i32), u32); SETTLE_PIXELS] = [
            ((200, 100), SHIPPED_GROUND),
            ((203, 100), SHIPPED_GROUND),
            ((197, 100), SHIPPED_GROUND),
            ((200, 103), SHIPPED_GROUND),
            ((200, 97), SHIPPED_GROUND),
            ((201, 100), SHIPPED_GROUND),
            ((199, 100), SHIPPED_GROUND),
            ((200, 101), SHIPPED_GROUND),
            ((200, 99), SHIPPED_GROUND),
        ];
        let at = |sc: &Scratch, p: (i32, i32)| {
            let mut px = SHIPPED_GROUND;
            for q in &sc.out[q0..] {
                if quad_covers(q, p.0, p.1) {
                    px = add_sat(px, q.color);
                }
            }
            for h in &sc.halos[h0..] {
                px = add_sat(px, halo_add_at(h, p.0, p.1));
            }
            px
        };
        assert!(
            y(at(&sc, (200, 100))) <= ceil,
            "the centre is lawful as laid"
        );
        assert!(
            y(at(&sc, (203, 100))) > ceil,
            "the stub pixel is over as laid"
        );
        {
            let mut f = sc.frame();
            settle_under_ceiling(&mut f, q0, h0, &watch);
        }
        for &(p, _) in &watch {
            assert!(
                y(at(&sc, p)) <= ceil,
                "({}, {}) is at Y {:.1} after the closer",
                p.0,
                p.1,
                y(at(&sc, p))
            );
        }
        assert!(
            y(at(&sc, (203, 100))) >= ceil - 2.0,
            "the closer dimmed past the ceiling: the stub pixel is at Y {:.1}",
            y(at(&sc, (203, 100)))
        );
        let k = lum(sc.halos[h0].color) as f32 / lum(halo_before) as f32;
        assert!(k > 0.3 && k < 1.0, "the factor {k} is not a dimming");
        for (q, &was) in sc.out[q0..].iter().zip(&before) {
            for sh in [16, 8, 0] {
                let (now, then) = (((q.color >> sh) & 0xff) as f32, ((was >> sh) & 0xff) as f32);
                assert!(
                    (now - then * k).abs() <= 1.0,
                    "a lay carries {now} where the factor {k} says {}",
                    then * k
                );
            }
        }
        assert!(chroma(sc.halos[h0].color) > 0, "the halo lost its colour");
        for h in &sc.halos[h0..] {
            assert_eq!(
                premul_rgb(yellow, lum(h.color) as u8),
                h.color,
                "the halo is no longer the pure stop"
            );
        }
        assert_eq!(sc.out[0].color, 0x0030_3030, "the earlier quad moved");

        // A star that fits is left byte-identical.
        let mut sc = Scratch::default();
        push_fx_rect(
            &mut sc.out,
            g,
            200,
            100,
            1,
            1,
            premul_rgb(TINT_WHITE_RGB, 40),
        );
        push_fx_rect(&mut sc.out, g, 203, 100, 2, 1, premul_rgb(yellow, 30));
        push_halo_quads(
            &mut sc.halos,
            g,
            usize::MAX,
            (200, 100),
            (15, 15),
            premul_rgb(yellow, 20),
            HaloMode::Add,
        );
        let (out, halos) = (sc.out.clone(), sc.halos.clone());
        {
            let mut f = sc.frame();
            settle_under_ceiling(&mut f, 0, 0, &watch);
        }
        assert!(sc.out.iter().zip(&out).all(|(a, b)| a.color == b.color));
        assert!(sc.halos.iter().zip(&halos).all(|(a, b)| a.color == b.color));
    }

    /// **THE CARET LAW SPENDS COLOUR FIRST AND WHITE LAST ON A STAR'S
    /// CENTRE** ([`price_centre`]) — the numbers, so the derivation is
    /// pinned and not merely described:
    ///
    /// * the A-type hero of the failing frame (`c = 61 · 0.7`, cov 43, halo
    ///   asked 43) keeps its whole body and gets 16 levels of halo, landing
    ///   at `Y 71.8` where it composited to `#ACABB9`, `Y 106`;
    /// * over black at the published `143` the white hero keeps its centre
    ///   to the byte and its halo has the one level the ceiling leaves;
    /// * every one of the seven stops, at the full intensity over the
    ///   shipped ground, keeps its halo's WHOLE request — 61, the pure stop
    ///   — and a body the ceiling can carry beside it; the centre lands at
    ///   or under `Y 72` on a max channel the grey at that `Y` (145) could
    ///   never reach, which is what "saturation costs no luminance" means
    ///   on the byte;
    /// * a ground already over the ceiling leaves the request as it is.
    #[test]
    fn the_caret_law_spends_colour_first_and_white_last_on_a_star_s_centre() {
        let y = |p: u32| relative_luminance(p) * 255.0;
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let white = TINT_WHITE_RGB;
        assert_eq!(fit_byte(255, |k| k <= 100), 100);
        assert_eq!(fit_byte(50, |_| true), 50);
        assert_eq!(fit_byte(50, |k| k == 0), 0);
        assert_eq!(fit_byte(50, |_| false), 0);

        let (cov, _, peak) =
            price_centre(SHIPPED_GROUND, true, (white, 43), (white, 0), (white, 43));
        assert_eq!((cov, peak), (43, 16), "the failing frame's A-type hero");
        let centre = add_sat(
            stacked_centre(SHIPPED_GROUND, white, 43),
            premul_rgb(white, 16),
        );
        assert!(
            y(centre) <= ceil && y(centre) > ceil - 1.0,
            "the A-type centre lands at Y {:.1}, not just under {ceil:.1}",
            y(centre)
        );
        let (cov, _, peak) = price_centre(0, true, (white, 61), (white, 0), (white, 61));
        assert_eq!((cov, peak), (61, 1), "the white hero over black");
        assert_eq!(lum(stacked_centre(0, white, 61)), 143);

        let grey_at_ceiling = 145;
        for i in 0..SPECTRUM_STOPS {
            let tint = spectrum_stop(i);
            let (cov, _, peak) =
                price_centre(SHIPPED_GROUND, false, (white, 61), (tint, 0), (tint, 61));
            assert_eq!(peak, 61, "stop {i} lost its halo's request");
            let centre = stacked_centre(add_sat(SHIPPED_GROUND, premul_rgb(tint, 61)), white, cov);
            eprintln!(
                "stop {i} {tint:#08x}: body cov {cov} of 61, centre #{centre:06X} — max channel {}, Y {:.1}",
                lum(centre),
                y(centre)
            );
            assert!(
                y(centre) <= ceil,
                "stop {i}: centre Y {:.1} over {ceil:.1}",
                y(centre)
            );
            assert!(
                cov >= 20 && lum(centre) > grey_at_ceiling,
                "stop {i}: body {cov}, max channel {} — the colour bought nothing",
                lum(centre)
            );
        }
        let bright = 0x00A0_A0A0;
        assert!(!under_ceil(bright));
        assert_eq!(
            price_centre(
                bright,
                false,
                (white, 61),
                (spectrum_stop(0), 17),
                (spectrum_stop(0), 61)
            ),
            (61, 17, 61),
            "a ground over the ceiling leaves the request as it is"
        );
    }

    /// **A STAR IS PRICED OVER THE SKY UNDER IT** ([`sky_ground`]): the
    /// theme ground plus the halos already laid — at a veil's centre the
    /// veil's whole colour, off its box nothing, and never an `Over` halo.
    #[test]
    fn a_star_is_priced_over_the_veils_and_halos_already_under_it() {
        let veil = RainHalo {
            row: 4,
            x: 90,
            y: 100,
            w: 9,
            h: 6,
            color: 0x0020_0008,
            cx: 94,
            cy: 106,
            rx: 18,
            ry: 6,
            mode: HaloMode::Add,
        };
        let over = RainHalo {
            mode: HaloMode::Over,
            ..veil
        };
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[], 94, 105),
            SHIPPED_GROUND
        );
        // One row above the veil's bottom edge on its axis: `ny = 256/36 = 7`,
        // `wt = (256 − 7)² >> 8 = 242`, `premul(#200008, 242) = #1E0008`.
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[veil], 94, 105),
            add_sat(SHIPPED_GROUND, 0x001E_0008)
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[veil], 94, 99),
            SHIPPED_GROUND,
            "above the box"
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[veil], 89, 105),
            SHIPPED_GROUND,
            "off the column"
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[], &[over], 94, 105),
            SHIPPED_GROUND,
            "an Over halo"
        );
        let stacked = sky_ground(SHIPPED_GROUND, &[], &[veil, veil], 94, 105);
        assert!(lum(stacked) > lum(sky_ground(SHIPPED_GROUND, &[], &[veil], 94, 105)));
        assert_eq!(
            sky_ground(0xFF1A_1B26, &[], &[], 0, 0),
            SHIPPED_GROUND,
            "the top byte is not a colour"
        );
        let quad = GlowQuad {
            row: 4,
            x: 93,
            y: 104,
            w: 3,
            h: 3,
            color: 0x0010_2030,
            alpha: 0,
        };
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[quad], &[], 94, 105),
            add_sat(SHIPPED_GROUND, 0x0010_2030),
            "an earlier star's quad"
        );
        assert_eq!(
            sky_ground(SHIPPED_GROUND, &[quad], &[], 96, 105),
            SHIPPED_GROUND,
            "off the quad"
        );
        assert_eq!(
            sky_ground(
                SHIPPED_GROUND,
                &[GlowQuad { alpha: 255, ..quad }],
                &[],
                94,
                105
            ),
            SHIPPED_GROUND,
            "a source-over quad is not the sky's"
        );
    }

    /// **THE AURORA RISES WITH MOMENTUM AND NEVER ENTERS A GLYPH ROW**
    /// (owner, 2026-09-08: "make this rainbow theme truly magical and
    /// special and dynamic and beautiful").
    ///
    /// Forty keys at 12 cps on the engine's own spine, red at the caret so
    /// a veil's byte is its red channel. Cold (key 0, spine 0) lays no veil;
    /// at one second (spine 0.51, "plain") the newest veil is 12-18; at the
    /// last key (spine 1.0, "full") it is 38-40 and brighter than at one
    /// second. Every veil on every frame lies wholly inside row 4 — the sky
    /// above the line, never a pixel of row 5's own rows — inside its own
    /// column, and never over a column whose row-4 cell the probe found
    /// inked (cols 25-30 here). And the veil walks the spectrum: with the
    /// field walking the arc, the last frame carries at least four hues.
    #[test]
    fn the_aurora_rises_with_momentum_and_never_enters_a_glyph_row() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let run = |walk: bool| -> Vec<(u16, f32, Vec<RainHalo>)> {
            let mut sky = Stardust::new();
            let mut above = [false; 120];
            above[25..=30].fill(true);
            sky.probe_mut().probe_row(4, &above);
            let mut frames = Vec::new();
            for k in 0..40u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp_at_12cps(k);
                cx.birth_disp = cx.disp;
                cx.caret_t = if walk { f32::from(k) / 40.0 } else { 0.0 };
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let mut sc = Scratch::default();
                sky.emit(&cx, &mut sc.frame());
                frames.push((k, cx.disp, veils(&sc)));
            }
            frames
        };
        let row5_top = i32::from(g.origin_y) + 5 * g.ch as i32;
        let inked =
            (i32::from(g.origin_x) + 25 * g.cw as i32)..(i32::from(g.origin_x) + 31 * g.cw as i32);
        let red = run(false);
        assert!(red[0].2.is_empty(), "a cold key laid a veil");
        let peak = |vs: &[RainHalo]| vs.iter().map(|h| lum(h.color)).max().unwrap_or(0);
        let plain = peak(&red[12].2);
        let full = peak(&red[39].2);
        eprintln!(
            "aurora: {plain} at spine {:.2}, {full} at spine {:.2}; {} veils on the last frame",
            red[12].1,
            red[39].1,
            red[39].2.len()
        );
        assert!(
            (12..=18).contains(&plain),
            "at spine {} the veil is {plain}, not plain",
            red[12].1
        );
        assert!(
            (38..=40).contains(&full) && full > plain,
            "at spine {} the veil is {full} (plain was {plain}), not full",
            red[39].1
        );
        for (k, _, vs) in &red {
            for h in vs {
                let (y0, y1) = (i32::from(h.y), i32::from(h.y) + i32::from(h.h));
                assert!(
                    y1 <= row5_top && y0 >= row5_top - g.ch as i32 && h.row == 4,
                    "key {k}: a veil at y {y0}..{y1} enters a glyph row (row 5 top {row5_top})"
                );
                assert!(
                    i32::from(h.h) <= (AURORA_TALL_CH * g.ch as f32).ceil() as i32,
                    "key {k}: a veil {} px tall is over 0.35 ch",
                    h.h
                );
                let (x0, x1) = (i32::from(h.x), i32::from(h.x) + i32::from(h.w));
                assert!(
                    x1 - x0 <= g.cw as i32 && (x0 - i32::from(g.origin_x)) % g.cw as i32 == 0,
                    "key {k}: a veil at x {x0}..{x1} is not one cell's own column"
                );
                assert!(
                    x1 <= inked.start || x0 >= inked.end,
                    "key {k}: a veil at x {x0}..{x1} sits over an inked sky cell"
                );
            }
        }
        let walked = run(true);
        let hues: std::collections::BTreeSet<u32> = walked[39].2.iter().map(|h| h.color).collect();
        assert!(
            hues.len() >= 4,
            "the veil wears {} colours over the walked field, not a rainbow",
            hues.len()
        );
    }

    /// **THE AURORA LEAVES WITH THE RIBBON** (owner, 2026-09-08). Twenty hot
    /// keys, then the hand stops: the veil holds its 38-40 through the
    /// field hold (300 ms), is at half by 550, and is gone — off glass, out
    /// of the pool, no deadline — by 900 ms, before the ribbon's retract
    /// moves (`RETRACT_START_S`). A Backspace puts the erased cells' veil
    /// on the ribbon's own 240 ms retract while the cells before the caret
    /// keep theirs.
    #[test]
    fn the_aurora_leaves_with_the_ribbon() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let g = geom();
        let hot = |sky: &mut Stardust, keys: u16| -> Instant {
            let mut last = t0;
            for k in 0..keys {
                last = key_at(t0, k);
                let mut cx = ctx(last, &c, (5, 10 + k));
                cx.disp = 1.0;
                cx.birth_disp = 1.0;
                cx.caret_t = 0.0;
                sky.budget.refill(last);
                sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
            }
            last
        };
        let peak_at = |sky: &mut Stardust, now: Instant| -> u32 {
            let mut sc = Scratch::default();
            sky.emit(&ctx(now, &c, (5, 29)), &mut sc.frame());
            veils(&sc).iter().map(|h| lum(h.color)).max().unwrap_or(0)
        };
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let last = hot(&mut sky, 20);
        let at = |ms: u64| last + Duration::from_millis(ms);
        let (p0, p300, p550, p900) = (
            peak_at(&mut sky, at(0)),
            peak_at(&mut sky, at(300)),
            peak_at(&mut sky, at(550)),
            peak_at(&mut sky, at(900)),
        );
        eprintln!(
            "aurora after the last key: {p0} at +0, {p300} at +300, {p550} at +550, {p900} at +900 ms"
        );
        assert!((38..=40).contains(&p0), "the veil is {p0} on the last key");
        assert!(
            (38..=40).contains(&p300),
            "the veil is {p300} at the end of the hold"
        );
        assert!(
            (15..=25).contains(&p550),
            "the veil is {p550} at 550 ms, not about half"
        );
        assert_eq!(p900, 0, "the veil outlived the ribbon's grace");
        assert!(
            (RETRACT_START_S * 1000.0) as u64 >= 900,
            "the fixture's 900 ms is not before the retract"
        );
        assert!(sky.at_rest(), "the veil is still in the pool");
        assert!(sky.next_change_deadline(at(900)).is_none());

        // -- the erase arm --------------------------------------------------
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let last = hot(&mut sky, 10);
        let erase = last + Duration::from_millis(20);
        sky.on_event(&Event::Erase, erase, &ctx(erase, &c, (5, 19)), &ribbon);
        let mut sc = Scratch::default();
        let done = erase + Duration::from_millis(240);
        sky.emit(&ctx(done, &c, (5, 19)), &mut sc.frame());
        let col_of = |h: &RainHalo| (i32::from(h.x) - i32::from(g.origin_x)) / g.cw as i32;
        let cols: std::collections::BTreeSet<i32> = veils(&sc).iter().map(col_of).collect();
        assert!(
            !cols.contains(&19),
            "the erased cell's veil survived the ribbon's retract: {cols:?}"
        );
        assert!(
            cols.contains(&10),
            "a cell before the caret lost its veil to the Backspace: {cols:?}"
        );
    }

    /// **A HOT HAND DEALS EVERY CELL A FIELD STAR** ([`FIELD_DEAL_IN_HOT`],
    /// owner 2026-09-08: "dynamic"). Sixty keys on a probed-blank sky at a
    /// fixed spine: cold (0.4) the births per key are the strike deal plus
    /// half a field star, `1.33`; hot (0.8) the second grain and a field star
    /// on every cell, `2.83` — measured as stars born on the key's own edge.
    #[test]
    fn a_hot_hand_deals_every_cell_a_field_star() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        // Births per key, and the share of the last twenty laid cells that
        // carry a live FIELD-lane star (dealt or adopted) on the last key.
        let census = |disp: f32| -> (f32, f32, usize) {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut born = 0usize;
            for k in 0..60u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp;
                cx.birth_disp = disp;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                born += sky.live_iter().filter(|s| s.born == at).count();
            }
            let field: std::collections::BTreeSet<i32> = sky
                .live_iter()
                .filter(|s| s.lane == StarLane::Field)
                .map(col_of)
                .collect();
            // The last SIXTEEN laid cells — 1.33 s at 12 cps, inside the
            // 1.54 s field horizon (`FIELD_LIFE_S`) an adopted star's own
            // birth still runs from.
            let held = (53..69).filter(|col| field.contains(col)).count();
            (born as f32 / 60.0, held as f32 / 16.0, sky.live())
        };
        let (cold, hot) = (census(0.4), census(0.8));
        eprintln!(
            "births per key: cold {:.2}, hot {:.2}; field-held cells of the last sixteen: cold {:.2}, hot {:.2}; live: cold {}, hot {}",
            cold.0, hot.0, cold.1, hot.1, cold.2, hot.2
        );
        assert!(
            (0.8..=1.0).contains(&cold.0),
            "a cold hand bears {} stars per key, not about 0.92",
            cold.0
        );
        assert!(
            (0.95..=1.05).contains(&hot.0),
            "a hot hand bears {} stars per key, not one per cell",
            hot.0
        );
        assert!(
            hot.1 >= 0.999,
            "a hot hand left {} of the last sixteen cells without a field star",
            1.0 - hot.1
        );
        assert!(
            cold.1 <= 0.75 && hot.1 > cold.1 + 0.2,
            "momentum bought no density: field-held {} → {}",
            cold.1,
            hot.1
        );
    }

    /// **THE COMBO LADDER** (§23's addendum "Flow state", 2026-09-09) — flow's
    /// payout, deterministic and keystroke-born: every
    /// [`LADDER_M2_KEYS`]th key of a run promotes its strike star to an m2,
    /// every [`LADDER_WHITE_KEYS`]th takes a WHITE m2, and every
    /// [`LADDER_GOLD_KEYS`]th takes a GOLD m1 — earned, so no spacing law and
    /// no dry bucket may demote it.
    ///
    /// Deterministic is the whole point: seventy keys of one run, and the
    /// rungs are the rungs on every one of them, while the keys between them
    /// still take the ordinary strike DEAL (some of them a grain, some of
    /// them nothing) — which is the control that says the ladder is a payout
    /// and not the deal getting brighter.
    #[test]
    fn the_combo_ladder_pays_at_sixteen_thirty_two_and_sixty_four() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        // The brightest STRIKE star born on each key's own edge, by combo.
        /// `(class, tint, gold)` of the brightest strike star one key drew.
        type Paid = (StarClass, u32, bool);
        let mut rung: Vec<(u32, Option<Paid>)> = Vec::new();
        for k in 0..70u16 {
            let at = key_at(t0, k);
            let combo = u32::from(k) + 1;
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            cx.flow = Flow {
                heat: 1.0,
                combo,
                best: combo,
            };
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let best = sky
                .live_iter()
                .filter(|s| s.born == at && s.lane == StarLane::Strike)
                .min_by_key(|s| s.class)
                .map(|s| (s.class, s.tint, s.gold));
            rung.push((combo, best));
        }
        let paid = |combo: u32| -> Paid {
            rung.iter()
                .find(|(n, _)| *n == combo)
                .and_then(|(_, s)| *s)
                .unwrap_or_else(|| panic!("key {combo} of an open run drew NO strike star at all"))
        };
        for n in [LADDER_M2_KEYS, LADDER_M2_KEYS * 3] {
            let (class, _, _) = paid(n);
            assert!(
                class <= StarClass::M2,
                "key {n} paid a {class:?}, not the ladder's m2 or better"
            );
        }
        let (class, tint, gold) = paid(LADDER_WHITE_KEYS);
        assert_eq!(class, StarClass::M2, "the 32nd key pays an m2");
        assert_eq!(
            tint, TINT_WHITE_RGB,
            "…and it is WHITE by rule, not by deal"
        );
        assert!(!gold, "a white rung carries no glints");
        let (class, tint, gold) = paid(LADDER_GOLD_KEYS);
        assert_eq!(class, StarClass::M1, "the 64th key pays a hero");
        assert_eq!(tint, TINT_GOLD_RGB, "…gold by rule");
        assert!(gold, "…and gold-ness is the class that carries glints");

        // THE CONTROL: the keys between the rungs are still the DEAL — some
        // of them a grain, some of them nothing at all.
        let plain = rung
            .iter()
            .filter(|(n, _)| !n.is_multiple_of(LADDER_M2_KEYS))
            .filter(|(_, s)| s.is_none_or(|(c, _, _)| c == StarClass::M3))
            .count();
        assert!(
            plain > 20,
            "only {plain} of the non-rung keys took a grain or nothing — the ladder is not a payout, the deal moved"
        );
        eprintln!(
            "ladder: 16 {:?}, 32 {:?}, 64 {:?}; {plain} of the 65 non-rung keys took a grain or nothing",
            paid(LADDER_M2_KEYS).0,
            paid(LADDER_WHITE_KEYS).0,
            paid(LADDER_GOLD_KEYS).0
        );
    }

    /// **THE SKY OPENS** ([`FIELD_SHARE_FLOW`]) — an open theme deals every
    /// laid cell its grain, and holds that share through a dip in the honest
    /// spine that would close the STEP deal ([`FIELD_HOT_DISP`]). Measured
    /// cold, dipping and flowing on the same sixty keys.
    #[test]
    fn an_open_theme_holds_the_sky_open_through_a_dip() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        // Births per key, the share of the last sixteen laid cells holding a
        // FIELD-lane star (dealt or adopted — the field is what "every laid
        // cell gets its grain" means), and the live pool.
        let census = |disp: f32, heat: f32| -> (f32, f32, usize) {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let mut born = 0usize;
            for k in 0..60u16 {
                let at = key_at(t0, k);
                let mut cx = ctx(at, &c, (5, 10 + k));
                cx.disp = disp;
                cx.birth_disp = disp;
                cx.flow = Flow {
                    heat,
                    combo: if heat > 0.0 { FLOW_ENTRY_KEYS } else { 0 },
                    best: FLOW_ENTRY_KEYS,
                };
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                born += sky.live_iter().filter(|s| s.born == at).count();
            }
            let field: std::collections::BTreeSet<i32> = sky
                .live_iter()
                .filter(|s| s.lane == StarLane::Field)
                .map(col_of)
                .collect();
            let held = (53..69).filter(|col| field.contains(col)).count();
            (born as f32 / 60.0, held as f32 / 16.0, sky.live())
        };
        // The dip: a hand whose honest spine has breathed under the step's
        // 0.7 but whose RUN has not broken.
        let dip = FIELD_HOT_DISP - 0.05;
        let cold = census(dip, 0.0);
        let flowing = census(dip, 1.0);
        let hot = census(0.9, 0.0);
        eprintln!(
            "at disp {dip}: cold {:.2} stars/key, field-held {:.2}, live {}; flowing {:.2}, {:.2}, live {}. The step at disp 0.9: {:.2}, {:.2}, live {}",
            cold.0, cold.1, cold.2, flowing.0, flowing.1, flowing.2, hot.0, hot.1, hot.2
        );
        assert!(
            flowing.1 >= 0.999,
            "an open theme left {:.2} of the last sixteen cells without a field star",
            1.0 - flowing.1
        );
        assert!(
            flowing.1 > cold.1 + 0.2 && flowing.2 > cold.2,
            "an open theme bought no density in the dip: field-held {:.2} → {:.2}, live {} → {}",
            cold.1,
            flowing.1,
            cold.2,
            flowing.2
        );
        assert!(
            (flowing.1 - hot.1).abs() < 0.01 && flowing.2 == hot.2,
            "an open theme's sky at a dip (field-held {:.2}, live {}) is not the hot step's ({:.2}, {})",
            flowing.1,
            flowing.2,
            hot.1,
            hot.2
        );
        assert!(
            flowing.2 <= STAR_CAP,
            "the open sky ran past the live cap: {} of {STAR_CAP}",
            flowing.2
        );
    }

    /// **ONE SKY STAR PER CELL, AND NO PILE OUTSHINES THE CARET**
    /// ([`STRIKE_REACH_CELLS`], [`Stardust::sky_star_live`],
    /// [`Stardust::adopt_field`]).
    ///
    /// Forty keys at 12 cps on the engine's own spine over the shipped
    /// ground, the glass composited in the renderers' bytes on every key's
    /// echo frame. No two live sky stars share a cell — and no pixel of the
    /// sky rows reaches past the caret law's `72/255`. The bold round left
    /// the hot deal stacking a strike star, a field star and the previous
    /// key's leading-edge grain within a pixel or two on one cell, each
    /// priced only over the sky laid before it: this test read Y 99.6–101.7
    /// on 8 of the 40 frames there (the FAIL pasted at
    /// [`STRIKE_REACH_CELLS`]).
    #[test]
    fn one_sky_star_per_cell_and_no_pile_outshines_the_caret() {
        let mut c = cfg(true);
        c.theme_bg = SHIPPED_GROUND;
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let (mut worst, mut over) = (0.0f32, 0usize);
        let mut piles = Vec::new();
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            cx.caret_t = f32::from(k) / 40.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            let mut cells = std::collections::BTreeMap::new();
            for s in sky.live_iter().filter(|s| s.lane.is_sky()) {
                *cells.entry(col_of(s)).or_insert(0usize) += 1;
            }
            piles.extend(
                cells
                    .iter()
                    .filter(|&(_, &n)| n > 1)
                    .map(|(&col, &n)| (k, col, n)),
            );
            // Rows 3-5 of the fixture: the sky band, the line, and below.
            let peak = composite_over(&sc, SHIPPED_GROUND, 0, 94, 1080, 54)
                .iter()
                .map(|&p| relative_luminance(p) * 255.0)
                .fold(0.0, f32::max);
            over += usize::from(peak > ceil);
            worst = worst.max(peak);
        }
        eprintln!(
            "pile census: {over} of 40 frames over the ceiling {ceil:.1}, worst Y {worst:.1}; piles (key, col, stars): {piles:?}"
        );
        assert!(
            piles.is_empty(),
            "a cell carries more than one sky star: {piles:?}"
        );
        assert!(
            over == 0,
            "{over} of 40 frames reach past the caret law's {ceil:.1} — worst Y {worst:.1}"
        );
    }

    /// **THE SKY AT 2× IS THE SKY AT 1× TWICE THE SIZE** ([`CELL_1X_CH`],
    /// [`px_scale`]). The owner runs a retina cell (15×28 device px); the
    /// stardust's pixel sizes were ruled at the 1× cell (7×14) and spent as
    /// literal device px, so on his screen the sky was a quarter of the
    /// intended AREA with stars a quarter of the intended size.
    ///
    /// The 12 cps fixture (forty keys on the engine's own spine) at 7×14
    /// and at 15×28, its last twenty echo frames composited over the dark
    /// ground: the STAR-PIXEL AREA the halos light (pixels at the scanner's
    /// lit floor) is four times larger at 2× to within the bound — the whole
    /// lit area 3.3×, short of four by the family's 1 px hairlines, which
    /// double in length and not in width — every star halo's radius is
    /// twice its 1× twin's, and the aurora's veil box is twice as
    /// tall and as wide as the cell (`15/7` across — the owner's cell is not
    /// exactly twice as wide). Before: area 1.87×, halo radii 1.41× (the
    /// clocked integer arm, and the m2's 6 px floor at both cells), the veil
    /// already 2.14×2.00 — measured on this fixture.
    #[test]
    fn the_sky_at_two_x_is_the_sky_at_one_x_twice_the_size() {
        let c = cfg(true);
        let t0 = Instant::now();
        let ribbon = Ribbon::new();
        struct Sky {
            lit: usize,
            lit_halo: usize,
            halo_r: f32,
            veil_rx: f32,
            veil_ry: f32,
        }
        let measure = |g: Geom| -> Sky {
            let mut sky = Stardust::new();
            sky.probe_mut().probe_row(4, &[false; 120]);
            let (mut lit, mut lit_halo) = (0usize, 0usize);
            let (mut halo_r, mut veil_rx, mut veil_ry) = (Vec::new(), Vec::new(), Vec::new());
            for k in 0..40u16 {
                let at = key_at(t0, k);
                let mut cx = ctx_in(g, at, &c, (5, 10 + k));
                cx.disp = disp_at_12cps(k);
                cx.birth_disp = cx.disp;
                cx.caret_t = f32::from(k) / 40.0;
                sky.budget.refill(at);
                sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
                let mut sc = Scratch::default();
                sky.emit(&cx, &mut sc.frame());
                if k < 20 {
                    continue;
                }
                let y0 = i32::from(g.origin_y) + 3 * g.ch as i32;
                let count = |sc: &Scratch| {
                    composite(sc, 0, y0, g.cols * g.cw, 3 * g.ch)
                        .iter()
                        .filter(|&&p| lum(p) >= SCAN_LIT_FLOOR)
                        .count()
                };
                lit += count(&sc);
                // The halos alone — the sky's AREA, apart from the hairlines.
                let halos_only = Scratch {
                    halos: sc.halos.clone(),
                    ..Scratch::default()
                };
                lit_halo += count(&halos_only);
                for h in &sc.halos {
                    if h.rx == h.ry {
                        halo_r.push(f32::from(h.rx));
                    } else {
                        veil_rx.push(f32::from(h.rx));
                        veil_ry.push(f32::from(h.ry));
                    }
                }
            }
            let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
            assert!(
                !halo_r.is_empty() && !veil_rx.is_empty(),
                "no halos at {}x{}",
                g.cw,
                g.ch
            );
            Sky {
                lit,
                lit_halo,
                halo_r: mean(&halo_r),
                veil_rx: mean(&veil_rx),
                veil_ry: mean(&veil_ry),
            }
        };
        let one = measure(geom_cell(7, 14));
        let two = measure(geom_cell(15, 28));
        let area = two.lit as f32 / one.lit.max(1) as f32;
        let area_halo = two.lit_halo as f32 / one.lit_halo.max(1) as f32;
        let halo = two.halo_r / one.halo_r;
        let (vx, vy) = (two.veil_rx / one.veil_rx, two.veil_ry / one.veil_ry);
        eprintln!(
            "1x: {} star px ({} halo-lit), halo r {:.2}, veil {:.1}x{:.1}; 2x: {} star px ({} halo-lit), halo r {:.2}, veil {:.1}x{:.1}; ratios: area {area:.2} (halos alone {area_halo:.2}), halo {halo:.2}, veil {vx:.2}x{vy:.2}",
            one.lit,
            one.lit_halo,
            one.halo_r,
            one.veil_rx,
            one.veil_ry,
            two.lit,
            two.lit_halo,
            two.halo_r,
            two.veil_rx,
            two.veil_ry
        );
        // The halos' area — the sky's, apart from the hairlines — is four
        // times; the whole lit area falls short of four by exactly the
        // needles, stubs and grain cores the family keeps ONE pixel wide at
        // every cell (`STAR_WAIST`, `star_waist_px`: a 1 px hairline until
        // the arm reaches 8 px), which double in length and not in width.
        assert!(
            (3.4..=4.6).contains(&area_halo),
            "the 2x sky's halos light {area_halo:.2}x the pixels of the 1x sky's, not about four"
        );
        assert!(
            area >= 3.2,
            "the 2x sky has {area:.2}x the star pixels of the 1x sky — less than the hairlines alone can explain"
        );
        assert!(
            (1.8..=2.2).contains(&halo),
            "the 2x halos are {halo:.2}x the 1x radii, not twice"
        );
        assert!(
            (1.8..=2.2).contains(&vy) && (vx - 15.0 / 7.0).abs() <= 0.2,
            "the 2x veil box is {vx:.2}x wide and {vy:.2}x tall, not the cell's own 15/7 x 2"
        );
    }

    // -- 1d. the slipstream ---------------------------------------------------

    /// A hand at `cps` on the owner's retina cell (15×28): `keys` typed
    /// glyphs from column `col0` on row 5, the sky row (4) probed blank
    /// past its first `ink_to` columns, the spine held HOT (0.9) — on
    /// purpose: §23 measured a sustained 2 cps hand at a `disp` of 0.97, so
    /// the birth gate is not what makes a stroll stand still, and the
    /// fixture must not let it stand in for the cadence. Returns the sky
    /// and the last key's instant.
    fn stream_sky(
        cps: f32,
        keys: u16,
        col0: u16,
        ink_to: usize,
        c: &Config,
        t0: Instant,
    ) -> (Stardust, Instant) {
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        let mut occupied = [false; 120];
        for slot in occupied.iter_mut().take(ink_to) {
            *slot = true;
        }
        sky.probe_mut().probe_row(4, &occupied);
        let mut last = t0;
        for k in 0..keys {
            last = t0 + Duration::from_secs_f32(f32::from(k) / cps);
            let mut cx = ctx_in(g, last, c, (5, col0 + k));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(last);
            sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
        }
        (sky, last)
    }

    /// **1d — STARS BORN AT SPEED STREAM BACKWARDS AT THE HAND'S OWN
    /// CADENCE** (T4). Twenty-four keys at 8, 12 and 2 cps on the owner's
    /// cell, the spine hot at every cadence. Every sky star the last key
    /// bore is read at its birth and 200 ms on: at 8 and 12 cps its
    /// backward travel, as px/s over the window, is within 10 % of the
    /// priced curve's (`0.35·cw/IOI` at birth — 42 and 63 px/s — decaying
    /// on τ 300); at 2 cps it is exactly zero, by the cadence and not by
    /// the gate. Fails before: every star measured 0 px/s against 30.7 and
    /// 46.0.
    #[test]
    fn stars_born_at_speed_stream_backwards_at_the_hand_s_own_cadence() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let window_ms = 200.0f32;
        let window = Duration::from_secs_f32(window_ms / 1000.0);
        for (cps, priced_v0) in [(8.0f32, 42.0f32), (12.0, 63.0), (2.0, 0.0)] {
            let v0 = stream_px_per_s(g.cw as f32, 1000.0 / cps);
            assert!(
                (v0 - priced_v0).abs() <= 0.01 * priced_v0.max(1.0),
                "{cps} cps on the owner's cell prices {v0:.2} px/s, not {priced_v0}"
            );
            let (sky, last) = stream_sky(cps, 24, 30, 0, &c, t0);
            let newborn: Vec<&Star> = sky
                .live_iter()
                .filter(|s| s.born == last && s.lane.is_sky())
                .collect();
            assert!(
                !newborn.is_empty(),
                "the last key at {cps} cps bore no sky star"
            );
            let priced_mean = stream_travel_px(v0) * stream_unit(window_ms) / (window_ms / 1000.0);
            for s in &newborn {
                let (x0, y0) = s.pos(last, false);
                let (x1, _) = s.pos(last + window, false);
                let back = x0 - x1;
                let measured = back as f32 / (window_ms / 1000.0);
                eprintln!(
                    "{cps} cps: {:?} at ({x0}, {y0}) travels {back} px back in {window_ms} ms = {measured:.1} px/s (priced v0 {v0:.1} px/s, {window_ms} ms mean {priced_mean:.1})",
                    s.class
                );
                if cps < 3.0 {
                    assert_eq!(
                        back, 0,
                        "a 2 cps stroll streams {back} px in {window_ms} ms; a stroll stands still"
                    );
                } else {
                    assert!(
                        (measured - priced_mean).abs() <= 0.10 * priced_mean,
                        "{cps} cps: {measured:.1} px/s measured against {priced_mean:.1} priced — not within 10 %"
                    );
                }
            }
        }
    }

    /// **1d / L4 — A STREAMING STAR NEVER CROSSES INK OR THE PANE EDGE.**
    /// (a) The sky row inked up to column 18 and a 12 cps hand typing from
    /// column 18: every star's centre, swept 5 ms at a time through the
    /// whole drift and beyond, stays over a cell the probe proved blank and
    /// never left of column 18 — and at least one star DID stream, so the
    /// clamp was a clamp and not an absence. (b) The hand typing from
    /// column 0: a star born inside its own travel of the pane's first
    /// column stops at the effects box's left edge. Fails before: nothing
    /// streams, so the "streamed at all" clause is what fails.
    #[test]
    fn a_streaming_star_never_crosses_ink_or_the_pane_edge() {
        let c = cfg(true);
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let sweep = |sky: &Stardust, min_col: i32| -> (usize, usize) {
            let (mut streamed, mut clamped) = (0usize, 0usize);
            for s in sky.live_iter().filter(|s| s.lane.is_sky()) {
                let (x0, _) = s.pos(s.born, false);
                let travel = stream_travel_px(stream_px_per_s(g.cw as f32, 1000.0 / 12.0));
                let mut x_end = x0;
                for ms in (0..=(STREAM_END_MS as u64 + 200)).step_by(5) {
                    let (x, y) = s.pos(s.born + Duration::from_millis(ms), false);
                    assert_eq!(
                        sky.probe().at_px(x, y, g),
                        Some(false),
                        "a star born at x {x0} is over ink or the unknown at ({x}, {y}) {ms} ms on"
                    );
                    assert!(
                        px_col(x as f32, g) >= min_col,
                        "a star born at x {x0} crossed to column {} at {ms} ms",
                        px_col(x as f32, g)
                    );
                    assert!(
                        x >= g.fx_left(),
                        "a star born at x {x0} left the glass at x {x} ({ms} ms)"
                    );
                    x_end = x;
                }
                if x_end < x0 {
                    streamed += 1;
                }
                // Would the unclamped travel have crossed the floor?
                let floor = i32::from(g.origin_x) + min_col * g.cw as i32;
                if (x0 as f32 - travel) < floor as f32 {
                    clamped += 1;
                    assert!(
                        x_end >= floor,
                        "a star born at x {x0} rests at {x_end}, past the floor {floor}"
                    );
                }
            }
            (streamed, clamped)
        };
        let (sky, _) = stream_sky(12.0, 8, 18, 18, &c, t0);
        let (streamed, clamped) = sweep(&sky, 18);
        eprintln!("ink at column 18: {streamed} stars streamed, {clamped} met the ink frontier");
        assert!(
            streamed > 0,
            "no star streamed over the blank sky — nothing was clamped"
        );
        assert!(
            clamped > 0,
            "no star was born within its travel of the ink — the clamp was not exercised"
        );
        let (sky, _) = stream_sky(12.0, 8, 0, 0, &c, t0);
        let (streamed, clamped) = sweep(&sky, 0);
        eprintln!("the pane's first column: {streamed} stars streamed, {clamped} met the edge");
        assert!(
            streamed > 0,
            "no star streamed from the pane's first columns"
        );
        assert!(
            clamped > 0,
            "no star was born within its travel of the pane's edge"
        );
    }

    /// **1d — THE SKY IS AT REST ONE STAR-LIFE AFTER THE LAST KEY.** The
    /// drift ends at [`STREAM_END_MS`], which is pinned here to the longest
    /// sky class life (the strike m1's 460 ms, `H + 2.32·τ`). Twenty-four
    /// keys at 12 cps, the spine hot; 100 ms after the last key the sky is
    /// still streaming (some star moves between +100 and +200 ms), and from
    /// one star-life on NO star's X moves again — read at +1 ms, +50, +200,
    /// +1 s and +3 s past the rest — and no star offers a stream step to the
    /// cadence fold. Fails before: nothing streams at +100 ms.
    #[test]
    fn the_sky_is_at_rest_one_star_life_after_the_last_key() {
        let c = cfg(true);
        let t0 = Instant::now();
        // §5.6 publishes the m1's life as 460; `H + 2.32·τ` is 458.7.
        let life_ms = StarClass::M1.life_s(StarLane::Strike) * 1000.0;
        assert!(
            (STREAM_END_MS - life_ms).abs() <= 1.5,
            "the drift ends at {STREAM_END_MS} ms, not the longest sky life {life_ms:.1}"
        );
        let (sky, last) = stream_sky(12.0, 24, 30, 0, &c, t0);
        let early = last + Duration::from_millis(100);
        let later = last + Duration::from_millis(200);
        let streaming = sky
            .live_iter()
            .filter(|s| s.pos(early, false).0 != s.pos(later, false).0)
            .count();
        assert!(
            streaming > 0,
            "nothing in the sky is streaming 100 ms after the last key"
        );
        let rest = last + Duration::from_secs_f32(STREAM_END_MS / 1000.0);
        let mut travelled = Vec::new();
        for s in sky.live_iter() {
            let at_rest = s.pos(rest, false);
            travelled.push(s.pos(s.born, false).0 - at_rest.0);
            for ms in [1u64, 50, 200, 1_000, 3_000] {
                let p = s.pos(rest + Duration::from_millis(ms), false);
                assert_eq!(
                    p.0, at_rest.0,
                    "a star still moves {ms} ms after the sky should be at rest ({:?} → {:?})",
                    at_rest, p
                );
            }
            assert!(
                s.next_stream_step_s(rest).is_none(),
                "a star at rest still offers a stream step"
            );
        }
        eprintln!(
            "{streaming} stars streaming at +100 ms; at rest at +{STREAM_END_MS} ms, travelled {travelled:?} px"
        );
    }

    /// **1d — THE STREAM STEP IS SOLVED, NOT POLLED** (§18's cadence law).
    /// A star with a 14.8 px travel (12 cps on the owner's cell): from
    /// every age on a 1 ms grid, the solved next step is where the integer
    /// X actually changes — unchanged 1 ms before it, changed 1 ms after —
    /// and every step is under the tail floor's reach of the previous one
    /// or later, never a spin.
    #[test]
    fn the_stream_step_is_solved_not_polled() {
        let t0 = Instant::now();
        let mut s = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
        s.stream = stream_travel_px(stream_px_per_s(15.0, 1000.0 / 12.0));
        let mut steps = 0usize;
        let mut age_ms = 0u64;
        while age_ms < STREAM_END_MS as u64 + 100 {
            let now = t0 + Duration::from_millis(age_ms);
            let Some(dt) = s.next_stream_step_s(now) else {
                break;
            };
            let at = now + Duration::from_secs_f32(dt);
            let before = s.pos(at - Duration::from_millis(1), false).0;
            let after = s.pos(at + Duration::from_millis(1), false).0;
            assert!(
                before != after,
                "the solved step at +{dt:.4} s from {age_ms} ms moves nothing ({before} → {after})"
            );
            assert!(dt >= 0.0, "a negative step {dt} from {age_ms} ms");
            steps += 1;
            age_ms = (age_ms as f32 + dt * 1000.0).floor() as u64 + 2;
        }
        eprintln!("{steps} solved steps over a {:.1} px travel", s.stream);
        assert!(
            steps == s.stream.round() as usize,
            "{steps} solved steps for a {:.1} px travel — one per pixel expected",
            s.stream
        );
        let done = t0 + Duration::from_secs_f32(STREAM_END_MS / 1000.0);
        assert!(
            s.next_stream_step_s(done).is_none(),
            "a step offered past the drift's end"
        );
        assert_eq!(
            s.pos(done, false).0,
            200 - s.stream.round() as i32,
            "the star did not travel its whole priced distance"
        );
    }

    /// `STREAM_SPENT` is the literal of `1 − e^(−STREAM_END_MS/STREAM_TAU_MS)`.
    #[test]
    fn the_stream_spent_share_is_the_exponential_s() {
        let want = 1.0 - (-STREAM_END_MS / STREAM_TAU_MS).exp();
        assert!(
            (STREAM_SPENT - want).abs() < 1e-4,
            "STREAM_SPENT {STREAM_SPENT} is not 1 − e^(−{STREAM_END_MS}/{STREAM_TAU_MS}) = {want:.5}"
        );
    }

    // -- the mend's star (1e) ----------------------------------------------

    /// One event on a HOT sky (`disp` 0.9 either way), with the frame the
    /// engine's tick would emit on the same instant — so the cull is as
    /// fresh here as it is there.
    fn hot_event(
        sky: &mut Stardust,
        ribbon: &Ribbon,
        world: (Geom, &Config),
        ev: &Event,
        at: Instant,
        caret: (u16, u16),
        mend: Option<Mend>,
    ) {
        let (g, c) = world;
        let mut cx = ctx_in(g, at, c, caret);
        cx.disp = 0.9;
        cx.birth_disp = 0.9;
        cx.mend = mend;
        sky.budget.refill(at);
        sky.on_event(ev, at, &cx, ribbon);
        let mut sc = Scratch::default();
        let mut f = sc.frame();
        sky.emit(&cx, &mut f);
    }

    /// THE TYPO SCRIPT on the sky alone (§23's addendum "The mend"): eight
    /// glyphs at 8 cps from column 4 of row 5 on the owner's cell, the sky
    /// row probed blank, the spine held hot; `deletes` Backspaces one slot
    /// after the last glyph and one slot apart; the fix `fix_after_ms` after
    /// the last delete, carrying the mark the engine publishes on that tick
    /// ([`Mend`]) when `mended` — else none, the tree before. Returns the
    /// sky, the fix's instant, the erased cell, the sky star live over it
    /// on the fix's own instant BEFORE the fix key — the light the
    /// Backspace was retiring — as `(position, class)`, and the classes of
    /// the sky stars live over the caret's cell then.
    #[expect(clippy::type_complexity, reason = "a fixture's five readings")]
    fn mend_sky(
        typo: TypedClass,
        deletes: u16,
        fix_after_ms: u64,
        mended: bool,
        c: &Config,
        t0: Instant,
    ) -> (
        Stardust,
        Instant,
        (u16, u16),
        Option<((i32, i32), StarClass)>,
        Vec<StarClass>,
    ) {
        const SLOT_MS: u64 = 125;
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let glyph = typed(TypedClass::Glyph);
        let mut last = t0;
        for k in 0..8u16 {
            last = t0 + Duration::from_millis(SLOT_MS * u64::from(k));
            let key = if k == 7 { typed(typo) } else { glyph };
            hot_event(&mut sky, &ribbon, (g, c), &key, last, (5, 5 + k), None);
        }
        // Glyphs on columns 4..=11; the caret at 12.
        let mut caret = 12u16;
        for _ in 0..deletes {
            last += Duration::from_millis(SLOT_MS);
            caret -= 1;
            hot_event(
                &mut sky,
                &ribbon,
                (g, c),
                &Event::Erase,
                last,
                (5, caret),
                None,
            );
        }
        let erased = (5u16, caret);
        let fix = last + Duration::from_millis(fix_after_ms);
        let retiring = sky
            .live_iter()
            .find(|s| s.lane.is_sky() && !s.dead(fix) && star_over_cell(s, erased, g))
            .map(|s| (s.pos(fix, false), s.class));
        let at_home = sky
            .live_iter()
            .filter(|s| s.lane.is_sky() && !s.dead(fix) && star_over_cell(s, (5, caret + 1), g))
            .map(|s| s.class)
            .collect();
        let mend = mended.then_some(Mend {
            at: last,
            disp: 0.9,
            row: erased.0,
            col: erased.1,
            deletes: u8::try_from(deletes).expect("a mend counts at most two"),
        });
        hot_event(&mut sky, &ribbon, (g, c), &glyph, fix, (5, caret + 1), mend);
        (sky, fix, erased, retiring, at_home)
    }

    /// **1e — A MEND'S STAR IS BORN WHERE THE TYPO WAS AND DRAGGED TO THE
    /// FIX** (§23's addendum "The mend"; [`MEND_PULL_MS`]).
    ///
    /// The typo script four ways — the fix one slot after one Backspace,
    /// 500 ms after it, one slot after two, and one slot after a Backspace
    /// over a CAPITAL typo, whose earned hero still lifts in the caret's
    /// cell when the fix lands — on the owner's cell. On
    /// the fix key's own instant one of its stars, an m2, stands where the
    /// typo was — at the very pixel of the star the Backspace was retiring
    /// where one is still live, else over the erased cell's sky — and 110
    /// ms on it stands over
    /// the CARET's cell: its own travel (the lane's position, the
    /// slipstream's steps read back) never goes backward inside the window,
    /// is exactly the pull from where it was born to its home, and does not
    /// move again after the window; the retiring star is gone from the
    /// erased cell (grabbed, not left beside a second) and the erased cell
    /// bears no fresh star of the fix's (its light moved); the fix bears ONE
    /// sky star over the caret's cell, and at +110 ms it is the ONLY sky
    /// star there — the typo's hero evicted on the cap's own finish, gone
    /// before the pull arrives; the sky is brisk inside the window
    /// and not one millisecond after it; and the star's centre, composited
    /// over the shipped ground at +0, +55 and +110 ms, is under the caret
    /// law's ceiling. Fails before: the fix's strike is dealt at the
    /// erased cell's free neighbour and lifts where it was born — "no star
    /// of the fix's stands where the typo was … at +0 and over the caret's
    /// (5, 12) at +110 ms".
    #[test]
    fn a_mend_s_star_is_born_where_the_typo_was_and_dragged_to_the_fix() {
        let mut c = cfg(true);
        c.theme_bg = SHIPPED_GROUND;
        let g = geom_cell(15, 28);
        let window_ms = MEND_PULL_MS as u64;
        let ceil = STAR_LIGHT_CEIL * 255.0;
        let over = |p: (i32, i32), cell: (u16, u16)| {
            px_col(p.0 as f32, g) == i32::from(cell.1) && in_sky_of_row(p.1 as f32, cell.0, g)
        };
        for (label, typo, deletes, fix_after_ms) in [
            ("one-slot fix", TypedClass::Glyph, 1u16, 125u64),
            ("500 ms fix", TypedClass::Glyph, 1, 500),
            ("two deletes", TypedClass::Glyph, 2, 125),
            ("capital typo", TypedClass::Capital, 1, 125),
        ] {
            let t0 = Instant::now();
            let (mut sky, fix, erased, retiring, home_before) =
                mend_sky(typo, deletes, fix_after_ms, true, &c, t0);
            let home = (erased.0, erased.1 + 1);
            eprintln!(
                "{label}: over the caret's cell {home:?} when the fix landed: {home_before:?}"
            );
            if typo == TypedClass::Capital {
                assert!(
                    home_before.contains(&StarClass::M1),
                    "NON-VACUOUS: the capital typo's earned hero is not over the caret's cell"
                );
            }
            let at = |ms: u64| fix + Duration::from_millis(ms);
            let fix_born: Vec<Star> = sky
                .live_iter()
                .filter(|s| s.born == fix && s.lane.is_sky())
                .copied()
                .collect();
            // Where the typo was: the retiring star's own pixel (a streamed
            // grain may stand a few px into the cell before), else the
            // erased cell's sky.
            let typo_at = |p: (i32, i32)| retiring.is_some_and(|(r, _)| r == p) || over(p, erased);
            let Some(star) = fix_born
                .iter()
                .find(|s| typo_at(s.pos(fix, false)) && over(s.pos(at(window_ms), false), home))
            else {
                panic!(
                    "{label}: no star of the fix's stands where the typo was ({erased:?}, retiring {retiring:?}) at +0 and over the caret's {home:?} at +{window_ms} ms — the fix bore {:?}",
                    fix_born
                        .iter()
                        .map(|s| (s.class, s.pos(fix, false), s.pos(at(window_ms), false)))
                        .collect::<Vec<_>>()
                );
            };
            assert_eq!(
                star.class,
                StarClass::M2,
                "{label}: the mend's star is an m2"
            );

            // Its own travel: the lane's position with the slipstream's
            // steps read back, ms by ms.
            let own = |ms: u64| {
                let p = star.pos(at(ms), false);
                p.0 + star.stream_px(ms as f32)
            };
            let (x0, y0) = star.pos(fix, false);
            let mut prev = own(0);
            for ms in 1..=window_ms {
                let now = own(ms);
                assert!(
                    now >= prev,
                    "{label}: the pull went backward at +{ms} ms ({prev} → {now})"
                );
                prev = now;
            }
            let travelled = own(window_ms) - own(0);
            eprintln!(
                "{label}: born at {:?} over {erased:?}, home {:?} over {home:?}: pulled {travelled} px in {window_ms} ms (priced {:.0}), at +55 ms {} px in",
                (x0, y0),
                star.pos(at(window_ms), false),
                star.pull,
                own(55) - own(0)
            );
            assert_eq!(
                travelled,
                star.pull.round() as i32,
                "{label}: pulled {travelled} px, priced {:.1}",
                star.pull
            );
            assert!(
                travelled >= g.cw as i32,
                "{label}: pulled {travelled} px — less than the {} px cell",
                g.cw
            );
            for ms in window_ms + 1..=400 {
                assert_eq!(
                    own(ms),
                    own(window_ms),
                    "{label}: the star moved at +{ms} ms, after its window"
                );
            }

            // The grab: the light does not jump to be pulled.
            match retiring {
                Some((p, class)) => {
                    assert_eq!(
                        x0, p.0,
                        "{label}: born at x {x0}, the retiring {class:?} stood at {}",
                        p.0
                    );
                    assert!(
                        (y0 - p.1).abs() <= 2,
                        "{label}: born at y {y0}, the retiring {class:?} stood at {}",
                        p.1
                    );
                    eprintln!("{label}: grabbed the retiring {class:?} at {p:?}");
                }
                None => eprintln!("{label}: nothing was retiring over {erased:?}"),
            }
            assert!(
                !sky.live_iter()
                    .any(|s| s.born < fix && s.lane.is_sky() && star_over_cell(s, erased, g)),
                "{label}: a star born before the fix is still over the erased cell {erased:?}"
            );
            assert!(
                !fix_born
                    .iter()
                    .any(|s| s.pull == 0.0 && over(s.pos(fix, false), erased)),
                "{label}: the fix bore a fresh star over the erased cell {erased:?} beside the one it pulled away"
            );
            assert_eq!(
                fix_born
                    .iter()
                    .filter(|s| over(s.pos(at(window_ms), false), home))
                    .count(),
                1,
                "{label}: the fix bore more than one sky star over the caret's cell"
            );
            let at_home: Vec<(StarClass, StarLane, (i32, i32))> = {
                let now = at(window_ms);
                let mut cx = ctx_in(g, now, &c, home);
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                let mut sc = Scratch::default();
                let mut f = sc.frame();
                sky.emit(&cx, &mut f);
                sky.live_iter()
                    .filter(|s| s.lane.is_sky() && over(s.pos(now, false), home))
                    .map(|s| (s.class, s.lane, s.pos(now, false)))
                    .collect()
            };
            assert_eq!(
                at_home.len(),
                1,
                "{label}: the caret's cell holds {at_home:?} at +{window_ms} ms — one star per cell"
            );

            // The cadence: one brisk window, then tails.
            assert!(sky.brisk(at(55)), "{label}: not brisk mid-pull");
            assert!(
                !sky.brisk(at(window_ms + 1)),
                "{label}: still brisk one millisecond after the pull"
            );

            // The caret stays brightest: the centre under the caret law.
            for ms in [0u64, 55, window_ms] {
                let now = at(ms);
                let mut cx = ctx_in(g, now, &c, home);
                cx.disp = 0.9;
                cx.birth_disp = 0.9;
                let mut sc = Scratch::default();
                let mut f = sc.frame();
                sky.emit(&cx, &mut f);
                let (x, y) = star.pos(now, false);
                let centre = composite_over(&sc, SHIPPED_GROUND, x, y, 1, 1)[0];
                let lum = relative_luminance(centre) * 255.0;
                assert!(
                    lum <= ceil,
                    "{label}: the mend's star centre at +{ms} ms reads Y {lum:.1} over the caret law's {ceil:.1}"
                );
            }
        }
    }

    // =======================================================================
    // THE END-TO-END STAR CATCH (panel #10(b)) — the seam walked WHOLE
    // =======================================================================
    //
    // `mod.rs` proves the sky's half (`Engine::pet_offer` / `catch_star`)
    // against a hand-built `PetOnGlass`, and `kitty_pet.rs` proves the
    // receiver's half against a hand-built `PetOffer`. What neither proves is
    // that a REAL star born on a REAL keystroke reaches a REAL cat's paw and
    // comes back to the sky: the two halves agree on a type, and a type is
    // not a walk. The harness below is that walk — one engine and one brain
    // on one clock, the host's two lines between them, and a twin of each
    // that never hears of the seam, so every "nothing else moved" below is a
    // diff against an un-offered world rather than a belief about one.

    use crate::kitty_pet::{PetAction, PetBrain, PetFrame, PetSense};
    use crate::rainbow_kitty::companion::{CATCH_REACH_CELLS, PetOffer, PetOnGlass, StarCatch};
    use crate::rainbow_kitty::{Dir, Engine, Licence};

    /// The paw's grid: the pet's own fixture (100×30 cells of 10×20) as the
    /// sky sees it, at origin 0 so the pet's grid px and the sky's window px
    /// are one coordinate.
    fn paw_geom() -> Geom {
        Geom {
            cw: 10,
            ch: 20,
            rows: 30,
            cols: 100,
            origin_x: 0,
            origin_y: 0,
            win_w: 1000,
            win_h: 600,
            head: 0,
        }
    }

    fn paw_sense(now: Instant, caret: (u16, u16)) -> PetSense {
        PetSense {
            now,
            caret: Some(caret),
            wrapped: false,
            rows: 30,
            cols: 100,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        }
    }

    /// The host's frame cadence while the pet or the sky is live.
    const FRAME_MS: u64 = 16;

    /// The column past which the typist's line wraps to the next row.
    const WRAP_COL: u16 = 90;

    /// What one lockstep frame reported, for the tests to read.
    #[derive(Clone, Copy, Debug)]
    struct Step {
        at: Instant,
        pet: PetFrame,
        twin_pet: PetFrame,
        offer: PetOffer,
        /// The star the paw handed back this frame, and whether the sky still
        /// had it (`Engine::catch_star`).
        caught: Option<(StarCatch, bool)>,
        /// `(quads, halos, cues)` the offered engine emitted this frame …
        emitted: (usize, usize, usize),
        /// … and the un-offered twin.
        twin_emitted: (usize, usize, usize),
    }

    /// ONE WORLD, TWICE: an engine and a brain wired through the host's two
    /// lines, and a twin pair fed byte-identical events on the same clock
    /// that never hears of panel #10.
    struct Lockstep {
        eng: Engine,
        pet: PetBrain,
        twin_eng: Engine,
        twin_pet: PetBrain,
        sc: Scratch,
        tsc: Scratch,
        cfg: Config,
        t: Instant,
        caret: (u16, u16),
        /// Tick the un-offered twin pair too (the walk's diff), or leave
        /// them still (the census, which only counts).
        twins: bool,
        last: Option<Step>,
    }

    impl Lockstep {
        fn new(t0: Instant, caret: (u16, u16)) -> Self {
            let mut eng = Engine::new();
            eng.set_engaged(true);
            let mut twin_eng = Engine::new();
            twin_eng.set_engaged(true);
            Self {
                eng,
                pet: PetBrain::default(),
                twin_eng,
                twin_pet: PetBrain::default(),
                sc: Scratch::default(),
                tsc: Scratch::default(),
                cfg: cfg(true),
                t: t0,
                caret,
                twins: true,
                last: None,
            }
        }

        /// The offered pair alone.
        fn solo(t0: Instant, caret: (u16, u16)) -> Self {
            Self {
                twins: false,
                ..Self::new(t0, caret)
            }
        }

        /// One frame at `self.t`: both engines tick, both brains tick, then
        /// the host's two lines — `app_render::route_v2_pet_offer`'s order,
        /// "immediately after the pet's OWN tick, with the frame that tick
        /// just published".
        fn frame(&mut self) -> Step {
            let g = paw_geom();
            let t = self.t;
            clear(&mut self.sc);
            clear(&mut self.tsc);
            {
                let mut fr = self.sc.frame();
                self.eng.tick(t, g, &self.cfg, &mut fr);
            }
            if self.twins {
                let mut fr = self.tsc.frame();
                self.twin_eng.tick(t, g, &self.cfg, &mut fr);
            }
            let pet = self.pet.tick(paw_sense(t, self.caret));
            let twin_pet = if self.twins {
                self.twin_pet.tick(paw_sense(t, self.caret))
            } else {
                pet
            };
            // THE SEAM — the host's two lines, verbatim from `pet_offer`'s
            // contract.
            let offer = self.eng.pet_offer(g, PetOnGlass::of(&pet, g));
            let caught = self
                .pet
                .note_v2_offer(&offer)
                .map(|star| (star, self.eng.catch_star(star, t)));
            let step = Step {
                at: t,
                pet,
                twin_pet,
                offer,
                caught,
                emitted: emitted(&self.sc),
                twin_emitted: emitted(&self.tsc),
            };
            self.last = Some(step);
            step
        }

        /// Advance the clock one frame and run it.
        fn next_frame(&mut self) -> Step {
            self.t += Duration::from_millis(FRAME_MS);
            self.frame()
        }

        /// Run frames up to (not at) `self.t + gap`, and set the clock there
        /// — the next key's instant, with nothing yet dealt on it, so a test
        /// may read the seed knob before it presses.
        fn advance(&mut self, gap: Duration) {
            let due = self.t + gap;
            while self.t + Duration::from_millis(FRAME_MS) < due {
                self.next_frame();
            }
            self.t = due;
        }

        /// One keystroke at `self.t`, in both worlds: the echo's one-cell
        /// move under the Typed licence and the key itself, on a row the
        /// probe has just proved blank (§5.4), then the frame that echoes it.
        fn key(&mut self, class: TypedClass) -> Step {
            let (r, c) = self.caret;
            let t = self.t;
            let to = (r, c + 1);
            for e in self.engines() {
                e.probe_mut().probe_row(i32::from(r) - 1, &[false; 100]);
                e.probe_mut().probe_row(i32::from(r), &[false; 100]);
                e.on_event(
                    Event::Move {
                        from: (r, c),
                        to,
                        licence: Licence::Typed,
                        dir: Dir::of(1, 0),
                    },
                    t,
                );
                e.on_event(
                    Event::Typed {
                        cells: 1,
                        shifted: matches!(class, TypedClass::Capital),
                        class,
                    },
                    t,
                );
            }
            self.caret = to;
            self.frame()
        }

        /// One Backspace at `self.t`, in both worlds: the retreat's one-cell
        /// move and the erase.
        fn backspace(&mut self) -> Step {
            let (r, c) = self.caret;
            let t = self.t;
            let to = (r, c.saturating_sub(1));
            for e in self.engines() {
                e.on_event(
                    Event::Move {
                        from: (r, c),
                        to,
                        licence: Licence::Typed,
                        dir: Dir::of(-1, 0),
                    },
                    t,
                );
                e.on_event(Event::Erase, t);
            }
            self.caret = to;
            self.frame()
        }

        /// Enter at `self.t`: the caret to the start of the next row, under
        /// the Return licence, in both worlds.
        fn newline(&mut self) -> Step {
            let (r, c) = self.caret;
            let t = self.t;
            let to = (r + 1, 0);
            for e in self.engines() {
                e.on_event(
                    Event::Move {
                        from: (r, c),
                        to,
                        licence: Licence::Return,
                        dir: Dir::of(-i32::from(c), 1),
                    },
                    t,
                );
                e.on_event(Event::Return, t);
            }
            self.caret = to;
            self.frame()
        }

        /// `gap` later, the next key — an Enter first when the line is full.
        fn key_after(&mut self, gap: Duration, class: TypedClass) -> Step {
            self.advance(gap);
            if self.caret.1 >= WRAP_COL {
                self.newline();
                self.advance(gap);
            }
            self.key(class)
        }

        /// The engines an event is reported to.
        fn engines(&mut self) -> impl Iterator<Item = &mut Engine> {
            std::iter::once(&mut self.eng).chain(self.twins.then_some(&mut self.twin_eng))
        }

        /// Idle for `secs` at frame cadence.
        fn idle(&mut self, secs: f32) -> Step {
            let end = self.t + Duration::from_secs_f32(secs);
            let mut step = self.frame();
            while self.t < end {
                step = self.next_frame();
            }
            step
        }

        /// The cat as the sky will read it on the NEXT frame's offer — the
        /// last published frame, which is what the host hands `pet_offer`.
        fn cat(&self) -> Option<PetOnGlass> {
            self.last.and_then(|s| PetOnGlass::of(&s.pet, paw_geom()))
        }

        /// The offered engine's copy of `star`, by identity.
        fn star(&self, star: StarCatch) -> Option<Star> {
            find_star(&self.eng, star)
        }

        /// The un-offered twin's copy of `star`, by identity.
        fn twin_star(&self, star: StarCatch) -> Option<Star> {
            find_star(&self.twin_eng, star)
        }
    }

    fn clear(sc: &mut Scratch) {
        sc.under.clear();
        sc.out.clear();
        sc.halos.clear();
        sc.beams.clear();
        sc.cues.clear();
    }

    fn emitted(sc: &Scratch) -> (usize, usize, usize) {
        (sc.under.len() + sc.out.len(), sc.halos.len(), sc.cues.len())
    }

    fn find_star(eng: &Engine, star: StarCatch) -> Option<Star> {
        eng.stardust
            .live_iter()
            .find(|s| s.born == star.born && s.x == star.x && s.y == star.y)
            .copied()
    }

    /// The gold m1 born on THIS instant in `eng`'s sky, if the key dealt one.
    fn gold_born(eng: &Engine, at: Instant) -> Option<Star> {
        eng.stardust
            .live_iter()
            .find(|s| s.gold && s.class == StarClass::M1 && s.lane.is_sky() && s.born == at)
            .copied()
    }

    /// **THE SEED KNOB, READ AHEAD** — whether a shifted capital pressed NOW
    /// on `cell` deals a GOLD m1, and where. Two laws, both the sky's own:
    ///
    /// * the CLASS is by rule: a capital EARNS its hero (§5.8,
    ///   `TypedClass::Capital` → `Deal::earned`), and an earned hero is never
    ///   demoted by the spacing or the bucket ([`Stardust::deal_star`]);
    /// * the TINT is by seed: `deal_tint` on the strike's seed,
    ///   `cell_hash(row, col, minted + 1)` — [`Stardust::mint_seed`] folds the
    ///   pool's mint count, and the strike is the first star a key mints — at
    ///   the cell the strike is BORN on, which is [`Stardust::free_sky_cell`]
    ///   of the typed cell (a taken cell births one cell ahead).
    ///
    /// So a test drives keys until the knob reads gold and presses a capital
    /// there: a fixed search over the event sequence (§18), not a die roll.
    fn gold_capital_at(eng: &Engine, cell: (u16, u16)) -> Option<(u16, u16)> {
        let free = eng.stardust.free_sky_cell(cell, paw_geom())?;
        let seed = cell_hash(free.0, free.1, eng.stardust.minted.wrapping_add(1));
        deal_tint(seed, 0.0).1.then_some(free)
    }

    /// The typo rally that SEATS a cat beside the caret: `fwd` keys forward
    /// and `back` Backspaces, repeated at `gap`. Three reversals inside
    /// `FROLIC_WINDOW` earn a frolic, a second frolic inside `TENNIS_AFTER`
    /// sits the cat down to WATCH the rally (`kitty_pet.rs`, the tennis
    /// watch) — and that seat is the one settled, purring posture a cat holds
    /// while the caret keeps moving on its row. Returns once the pet's own
    /// frame says `Sit` with the purr tell up, or panics after `rounds`.
    fn rally_until_seated(w: &mut Lockstep, gap: Duration, rounds: u32) -> u32 {
        for round in 0..rounds {
            for _ in 0..3 {
                w.key_after(gap, TypedClass::Glyph);
            }
            for _ in 0..2 {
                w.advance(gap);
                w.backspace();
            }
            if w.cat().is_some_and(|c| {
                c.contented() && w.last.is_some_and(|s| s.pet.action == PetAction::Sit)
            }) {
                return round + 1;
            }
        }
        panic!("fixture: the rally did not seat the cat in {rounds} rounds");
    }

    /// The one honest residual of the wiring round — "no single test walks a
    /// real star from `deal_typed` through the paw to `Stardust::catch`" —
    /// written: a real `Engine` typing at 12 cps and a real `PetBrain` on
    /// the same clock, ticked in lockstep with the host's two lines between
    /// them; a gold m1 dealt WITHIN REACH by the engine's own seed knob (a
    /// capital on the cell whose strike seed `deal_tint`s gold —
    /// [`gold_capital_at`]); and the whole of what may change: ONE star's
    /// life, on ONE frame, to the catch horizon, in a world otherwise
    /// bit-identical to its un-offered twin.
    ///
    /// **RED ON MAIN, AND IGNORED FOR THAT REASON (measured 2026-09-10).**
    /// The seated cat is the RALLY seat ([`rally_until_seated`]) because it
    /// is the only settled, purring posture a keystroke finds: a seat stands
    /// on the first key of a resume (`Sit → Stand`, the purr tell down, on
    /// the key's own tick), and no star outlives the 1.4 s it takes to sit
    /// again. But `kitty_pet.rs`'s tennis arm (`PetAction::Sit if
    /// self.tennis`) returns before the arrived branch that calls
    /// `consume_v2_offer`, so the rally seat LATCHES the star and never
    /// reaches for it, and the 0.25 s TTL retires it. With that one arm
    /// consuming the offer (a two-line experiment, not landed — the file is
    /// another agent's) every assertion here passes: paw 16 ms after birth,
    /// the sky at zero 1.5 s after the catch. Until the pet lands the paw,
    /// [`a_keystroke_s_star_finds_no_seat_that_can_reach_for_it`] pins the
    /// truth as it stands; when THAT goes red, un-ignore this.
    #[test]
    fn a_gold_star_walks_from_the_deal_through_the_paw_to_the_catch() {
        let t0 = Instant::now();
        let g = paw_geom();
        let gap = Duration::from_micros(83_333); // 12 cps
        let mut w = Lockstep::new(t0, (4, 0));

        // A sentence of running earns the cat its contentment and the engine
        // its heat; then the rally seats the cat.
        for _ in 0..30 {
            w.key_after(gap, TypedClass::Glyph);
        }
        let rounds = rally_until_seated(&mut w, gap, 12);
        let seat = w.cat().expect("fixture: the cat is on glass");
        assert!(seat.contented(), "fixture: a contented seat");

        // THE DEAL. Keep the rally going so the caret stays inside the paw's
        // reach, and on the third forward key of each round — the mend of
        // the two Backspaces already spent on the first — read the seed knob
        // at the key's own instant; a capital where it says gold.
        let mut dealt: Option<(Step, (u16, u16))> = None;
        'rounds: for _ in 0..rounds + 40 {
            for k in 0..3 {
                w.advance(gap);
                let cell = w.caret;
                let cat = w.cat().expect("fixture: the cat stays on glass");
                let in_reach =
                    cat.contented() && cat.columns_to(i32::from(cell.1)) <= CATCH_REACH_CELLS;
                if k == 2
                    && in_reach
                    && let Some(free) = gold_capital_at(&w.eng, cell)
                {
                    let s = w.key(TypedClass::Capital);
                    dealt = Some((s, free));
                    break 'rounds;
                }
                w.key(TypedClass::Glyph);
            }
            for _ in 0..2 {
                w.advance(gap);
                w.backspace();
            }
        }
        let (birth, free) = dealt.expect("fixture: the seed knob must read gold within 40 rounds");

        // The knob was right: the capital's strike is a gold m1 at the cell
        // the knob named, in BOTH worlds (the deal is the keystroke's, not
        // the seam's).
        let star = gold_born(&w.eng, birth.at).expect("the capital dealt a gold m1");
        assert_eq!(
            star.grid_col(g),
            i32::from(free.1),
            "born on the knob's cell"
        );
        assert!(star.in_sky_of(4, g), "born in the sky of the cat's row");
        assert_eq!(
            gold_born(&w.twin_eng, birth.at).map(|s| (s.x, s.y, s.born)),
            Some((star.x, star.y, star.born)),
            "the twin dealt the same star: the seam mints nothing"
        );
        assert!(
            star.finish.is_none(),
            "born with its whole life ahead of it"
        );

        // THE OFFER, on the birth frame: the sky names that star to the
        // seated, purring cat — and NOTHING is acted on yet (latch, never
        // act: the paw lands on a tick, not on a note).
        let catch = StarCatch {
            x: star.x,
            y: star.y,
            born: star.born,
            col: star.grid_col(g),
        };
        assert!(
            birth.pet.action.settled() && birth.pet.purr > 0.0,
            "a settled, purring cat"
        );
        assert_eq!(
            birth.offer.catch,
            Some(catch),
            "the birth frame's offer names the star"
        );
        assert_eq!(birth.caught, None, "noting is not catching");
        assert!(
            w.pet.pending_v2_offer().1,
            "the star is latched in the brain"
        );

        // THE PAW. Frame by frame until the brain hands the star back; the
        // star's life must not move on any frame before that one.
        let mut contact = None;
        for _ in 0..20 {
            let before = w.star(catch).expect("the star lives until the paw");
            assert_eq!(before.finish, None, "the life moved before the paw landed");
            let s = w.next_frame();
            if let Some((back, hit)) = s.caught {
                assert_eq!(back, catch, "the star handed back is the star offered");
                assert!(hit, "the sky still had the star to catch");
                contact = Some(s);
                break;
            }
        }
        let contact = contact.expect("the paw lands inside the star's TTL");
        let horizon = contact.at + Duration::from_secs_f32(CATCH_FINISH_MS / 1000.0);

        // EXACTLY ONE LIFE MOVED, to the catch horizon, on this frame …
        let caught = w
            .star(catch)
            .expect("the caught star is still on glass on the paw's frame");
        assert_eq!(
            caught.finish,
            Some(Finish {
                at: contact.at,
                span_s: CATCH_FINISH_MS / 1000.0,
            }),
            "the catch spends the star on the sky's own finish"
        );
        assert_eq!(
            w.twin_star(catch).map(|s| s.finish),
            Some(None),
            "the un-offered twin's copy keeps its whole life"
        );
        // … and no other star's did: every other star in the twin's pool is
        // in the offered pool with the same life, and vice versa.
        let others = |e: &Engine| -> Vec<(u32, u32, Instant, Option<Finish>)> {
            let mut v: Vec<_> = e
                .stardust
                .live_iter()
                .filter(|s| !(s.born == catch.born && s.x == catch.x && s.y == catch.y))
                .map(|s| (s.x.to_bits(), s.y.to_bits(), s.born, s.finish))
                .collect();
            v.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
            v
        };
        assert_eq!(
            others(&w.eng),
            others(&w.twin_eng),
            "a bystander's life moved"
        );
        assert!(
            !others(&w.eng).is_empty(),
            "fixture: there were bystanders to protect"
        );

        // THE PET DREW NO LIGHT OF ITS OWN: the reach spawned no mote the
        // twin did not, and the sky emitted exactly what the twin's did on
        // the contact frame — quads, halos and cues — because a finish
        // that starts at `now` has not yet taken a level off the star.
        assert_eq!(
            contact.pet.motes.iter().flatten().count(),
            contact.twin_pet.motes.iter().flatten().count(),
            "the reach spawned a mote"
        );
        assert_eq!(
            contact.emitted, contact.twin_emitted,
            "the catch drew or sounded something"
        );
        assert_eq!(
            w.eng.fingerprint(),
            w.twin_eng.fingerprint(),
            "the contact frame's light is byte-identical to the un-offered world's"
        );

        // NO TELEPORT: the reaching cat is where the un-offered twin is, to
        // the bit, on the contact frame.
        assert_eq!(
            (
                contact.pet.col.to_bits(),
                contact.pet.row.to_bits(),
                contact.pet.lift.to_bits()
            ),
            (
                contact.twin_pet.col.to_bits(),
                contact.twin_pet.row.to_bits(),
                contact.twin_pet.lift.to_bits()
            ),
            "the offered cat is at ({}, {}) and the un-offered twin at ({}, {})",
            contact.pet.col,
            contact.pet.row,
            contact.twin_pet.col,
            contact.twin_pet.row
        );

        // GONE BY THE HORIZON — and only in the offered world.
        let mut catches = 1;
        while w.t < horizon {
            let s = w.next_frame();
            catches += usize::from(s.caught.is_some());
            if let Some(s) = w.star(catch) {
                assert_eq!(
                    s.finish.map(|f| f.at),
                    Some(contact.at),
                    "the finish was re-stamped after the paw"
                );
            }
        }
        let s = w.next_frame();
        catches += usize::from(s.caught.is_some());
        assert!(w.t >= horizon);
        assert!(
            w.star(catch).is_none(),
            "the caught star is still in the sky past the horizon"
        );
        assert!(
            w.twin_star(catch).is_some_and(|s| !s.dead(w.t)),
            "fixture: the un-offered twin's star lives on past the horizon (m1 sky life 460 ms)"
        );

        // ONE CATCH. The offer stood for one star; nothing hands it back
        // twice, and nothing else is offered while the rally's seat lasts.
        for _ in 0..90 {
            let s = w.next_frame();
            catches += usize::from(s.caught.is_some());
        }
        assert_eq!(catches, 1, "the paw handed a star back {catches} times");

        // IDLE → EXACTLY ZERO, afterwards: the sky to its exact nothing …
        let mut t_zero = None;
        for _ in 0..(20 * 1000 / FRAME_MS) {
            w.next_frame();
            if w.eng.fingerprint() == 0
                && !w.eng.needs_frame_cadence()
                && w.eng.next_change_deadline(w.t).is_none()
            {
                t_zero = Some(w.t);
                break;
            }
        }
        let t_zero = t_zero.expect("the sky idles to exactly zero within 20 s of the catch");
        assert_eq!(w.eng.status().stars, 0);
        assert_eq!(
            w.eng.pet_offer(g, w.cat()),
            PetOffer::default(),
            "an idle sky offers the resident nothing"
        );
        // … and the pet's own frame train stands down (its settle ladder is
        // its own law; what the seam owes is that it left nothing armed).
        for _ in 0..(40 * 1000 / FRAME_MS) {
            if !w.pet.needs_frames() {
                break;
            }
            w.next_frame();
        }
        assert!(
            !w.pet.needs_frames(),
            "the pet's frame train did not stand down"
        );
        assert_eq!(
            w.pet.pending_v2_offer(),
            (false, false),
            "a latch outlived the catch"
        );
        assert_eq!(w.eng.fingerprint(), 0, "the sky lit again while idle");
        eprintln!(
            "walk: seat after {rounds} rally rounds at col {:.2}; gold dealt at {:?} (free cell {:?}) \
             {:.0} ms after t0; paw {:?} after birth; horizon {:?} after the paw; sky at zero {:.1} s \
             after the catch",
            seat.col,
            w.caret,
            free,
            (birth.at - t0).as_secs_f32() * 1000.0,
            contact.at - birth.at,
            horizon - contact.at,
            (t_zero - contact.at).as_secs_f32()
        );
    }

    /// The conjunction, tallied per key: where a keystroke's chance of a
    /// catch died.
    #[derive(Clone, Copy, Debug, Default)]
    struct Census {
        keys: u32,
        /// A gold m1 was dealt on the key (the sky's ~1-in-80, plus the
        /// combo ladder's every-64th).
        gold: u32,
        /// … within `CATCH_REACH_CELLS` of the cat, in the sky of its row.
        in_reach: u32,
        /// … with the cat settled …
        settled: u32,
        /// … and purring — the offer gate, so this is `offered`.
        purring: u32,
        /// The paw landed and the sky still had the star.
        caught: u32,
    }

    /// The typist's shape, for the census.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Shape {
        /// Forward only: the cat follows.
        Prose,
        /// Three forward, two back: the tennis watch seats the cat.
        Rally,
        /// A 1.7 s pause (the seat's `SIT_AFTER` plus a breath), then five
        /// keys: the hot resume, where the first key finds a seated cat.
        Resume,
    }

    /// Type `keys` keystrokes at `gap` in `shape` from a cat that starts
    /// contented on the row, and tally the conjunction on every GLYPH key
    /// (the rally's Backspaces deal no strike and are not keys of the
    /// census).
    fn census(w: &mut Lockstep, gap: Duration, keys: u32, shape: Shape) -> Census {
        let g = paw_geom();
        let mut c = Census::default();
        let mut k = 0u32;
        while c.keys < keys {
            match shape {
                Shape::Rally if k % 5 >= 3 => {
                    w.advance(gap);
                    w.backspace();
                    k += 1;
                    continue;
                }
                Shape::Resume if k.is_multiple_of(5) => {
                    w.idle(1.7);
                }
                _ => {}
            }
            let s = w.key_after(gap, TypedClass::Glyph);
            k += 1;
            c.keys += 1;
            let Some(star) = gold_born(&w.eng, s.at) else {
                continue;
            };
            c.gold += 1;
            let Some(cat) = PetOnGlass::of(&s.pet, g) else {
                continue;
            };
            if !(star.in_sky_of(cat.row, g)
                && cat.columns_to(star.grid_col(g)) <= CATCH_REACH_CELLS)
            {
                continue;
            }
            c.in_reach += 1;
            if !cat.settled {
                continue;
            }
            c.settled += 1;
            if cat.purr <= 0.0 {
                continue;
            }
            c.purring += 1;
            assert_eq!(
                s.offer.catch.map(|o| o.born),
                Some(star.born),
                "the gate and the offer disagree"
            );
            // The paw, if it lands, lands inside the star's TTL — before the
            // next key is due.
            while w.t + Duration::from_millis(FRAME_MS) < s.at + gap {
                let f = w.next_frame();
                if f.caught.is_some_and(|(_, hit)| hit) {
                    c.caught += 1;
                    break;
                }
            }
        }
        c
    }

    /// **THE CATCH'S RARITY ON THE REAL ENGINE** (§23's addendum "The star
    /// catch, walked end to end") — the owner's "will I ever see it", as a
    /// number. 2 000 glyph keys at 8 and 12 cps from a contented cat on the
    /// row, in three shapes: prose (forward only — the cat follows), the
    /// typo rally (3 forward, 2 back — the tennis watch seats the cat beside
    /// the caret) and the hot resume (a 1.7 s pause, then five keys — the
    /// first key finds a seated cat). The table is printed; the law pinned
    /// is the accounting: every catch is an offered gold m1 in reach of a
    /// settled, purring cat, and the census is exactly the keys asked for.
    #[test]
    fn the_catch_s_rarity_on_the_real_engine() {
        let t0 = Instant::now();
        let mut rows = Vec::new();
        for &(cps, shape, keys) in &[
            (8u32, Shape::Prose, 2000u32),
            (12, Shape::Prose, 2000),
            (8, Shape::Rally, 2000),
            (12, Shape::Rally, 2000),
            (8, Shape::Resume, 1000),
            (12, Shape::Resume, 1000),
        ] {
            let gap = Duration::from_micros(1_000_000 / u64::from(cps));
            let mut w = Lockstep::solo(t0, (2, 0));
            // A contented cat on the row: a sentence, then the rally's seat.
            for _ in 0..30 {
                w.key_after(gap, TypedClass::Glyph);
            }
            let seated = rally_until_seated(&mut w, gap, 12);
            let c = census(&mut w, gap, keys, shape);
            assert_eq!(c.keys, keys);
            assert!(
                c.caught <= c.purring
                    && c.purring <= c.settled
                    && c.settled <= c.in_reach
                    && c.in_reach <= c.gold
                    && c.gold <= c.keys,
                "the conjunction's ledger does not nest: {c:?}"
            );
            rows.push((cps, shape, seated, c));
        }
        eprintln!(
            "| shape | cps | keys | gold m1 dealt | in reach | cat settled | purring (= offered) | caught |"
        );
        eprintln!("|---|---|---|---|---|---|---|---|");
        for (cps, shape, seated, c) in &rows {
            eprintln!(
                "| {shape:?} (seated after {seated} rally rounds) | {cps} | {} | {} | {} | {} | {} | {} |",
                c.keys, c.gold, c.in_reach, c.settled, c.purring, c.caught
            );
        }
    }
}
