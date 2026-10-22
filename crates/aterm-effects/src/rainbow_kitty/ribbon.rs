// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ribbon: the laid rainbow under typed text, plus the per-row field index every other
//! rainbow-kitty producer reads.
//!
//! Design: `docs/design/RAINBOW-TRAIL-ONE-STORY.md` §3 (geometry and colour law) and
//! `docs/design/RAINBOW-KITTY-V2.md` §4 (the ribbon), §18 (field index, quad budget) and §29
//! onward (gap, scrub, torn-read and walk laws).
//!
//! ## Body and light
//!
//! * The tall body is one `aterm_render::ribbon_beam` call per run, carrying the wave, the
//!   edge-in, the expiry melt, the backspace retract, the wrap fold and the exit swoosh; light
//!   themes get a source-over twin. The comet body ([`Ribbon::comet`]) is fattest under the
//!   caret, the vivid rail ([`rail_ink`], dark themes only) runs below the row bottom in the
//!   full-value spectrum, and a new cell enters as a wipe from the caret side; all three are
//!   off under [`Config::ribbon_flat`].
//! * The bed ([`bed_ink`]) puts every stop on one composited luminance at the 5.25:1
//!   text-contrast bar, spending chroma before white. The bar holds over every cell, since text
//!   arrives cell by cell under the band; a warm stop at that luminance is an olive, so
//!   brighter warm ink rides the rail. [`UNDER_COV_CAP`] is the one ceiling; no host pass clips
//!   further.
//! * Light is priced once, at birth ([`Cell::cov0`]); the live spine moves only the body's
//!   shape, so nothing brightens after the last key. The hot edge ([`hot_edge_ink`]) is a 1-px
//!   additive hairline along the body's top edge behind the head, dark themes only.
//!
//! ## The walk
//!
//! `t` is a function of position ([`walk_t`]). A continuation (fold, composer relay, rebirth)
//! carries the walk's distance ([`Cohort::d0`]); a Return, a line kill, a curtain or a
//! navigated row change continues the colour at distance zero ([`Ribbon::fresh_line`]). A row
//! holds one walk: a run minted beside a live run takes its walk by position
//! ([`Ribbon::join_cohort`]).
//!
//! ## Lifetime
//!
//! * A key refreshes the un-abandoned cohorts of its row ([`Ribbon::place`]), so a mark ages
//!   only once the hand stops or leaves the row ([`live_since`]); the swoosh then retracts it
//!   into the caret, and a typed fold flows the row it left into its kept end
//!   ([`Cohort::flow`]).
//! * Abutting typed cohorts are folded into one each frame ([`Ribbon::heal_seams`]).
//! * Unlicensed program output at the caret lays nothing, so the next key past it mints a
//!   second cohort with the gap between (v3 §8 D-12). The phrase does not break on a gap in
//!   space: that closes the gap but takes the hand's own line dark mid-typing.
//! * A same-row keyed move over the hand's lit run is a scrub ([`Ribbon::scrub`]): it lays
//!   nothing and holds the band. A move that leaves the run is a jump: a wake corridor of new
//!   light on [`Layer::Over`] ([`Ribbon::wake`]) and the band abandoned beneath
//!   ([`Ribbon::abandon`]).
//! * Composer reflows keep their light ([`Relocate`], [`SoftWrap`], [`Lift`]); a torn read's
//!   partial release is held per part ([`Cell::released_at`]) and restored when its glyphs
//!   come back.
//!
//! ## The frame path
//!
//! [`Ribbon`]'s pool, plan and index are private behind read-only accessors, so nothing
//! mutates the pool behind the index the other producers read. With the resident scratch and
//! the index's spare lanes the frame path allocates nothing (§18).

use std::mem;

use aterm_time::Instant;

use aterm_render::{
    BeamClip, BeamVertex, GlowBlend, GlowQuad, RibbonVertex, comet_beam, over_premul, premul_rgb,
    ribbon_beam,
};

use crate::color_math::{hsv2rgb, oklab_distance, relative_luminance, rgb2hsv};
use crate::cursor_glow::{InkRole, band_pos, band_row};
use crate::effect_util::lerp_rgb;
use crate::spectrum::{spectrum, spectrum_with_min_saturation};

use super::meteor::tri;
use super::spine::{DISP_RELEASE_TAU, PHASE_RATE};
use super::timing::{
    EDGE_IN_S, FLIGHT_MIN_MS, IOI_DEFAULT_MS, JUMP_MIN_CELLS, REDUCED_MOTION_FADE_MS, clamp01,
    edge_in, flight_ms, ioi_next, kill_span_s, phrase_rest_ms, smoothstep01, spend, suck_in,
};
use super::{Cadence, Config, Ctx, Event, Frame, Licence, TypedClass, level_step_spend};

// ===========================================================================
// The budget, the walk, and the body's geometry
// ===========================================================================

/// Quads the ribbon may spend on one frame (RAINBOW-KITTY-V2.md §18: a
/// 76-cell hot line at retina). Shed from the tail, never the head: the head
/// cell is the per-key light ("one key, one cell, one light, one note").
pub const RIBBON_QUAD_BUDGET: usize = 10_240;

/// Cells the hot edge reaches back from the head (§4.1).
pub const HOT_EDGE_CELLS: f32 = 3.0;

/// The furthest back, in cells, the crisp edge may reach when a key is born on a change of
/// speed or under an open theme ([`edge_cells`]): the same alpha law over a longer `d`, so a
/// longer streak, never a brighter one.
pub const HOT_EDGE_CELLS_MAX: f32 = 5.0;

/// The hot edge's falloff shape, normalised at the head cell:
/// `alpha(d) = 0.38·(1 − d/3)²`, so the request is
/// `HOT_EDGE_COV_MAX · (1 − d/3)²` with `d` in cells behind the head.
pub const HOT_EDGE_ALPHA: f32 = 0.38;

/// The hot edge's coverage request ceiling: the transient cap every transient at the hand
/// shares (`timing::TRANSIENT_STAR_COV_CEIL`). The ledger holds it to the ink lift over the
/// row above's glyph cells. At this cap every stop reads as colour, 2.3x (blue) to 7x
/// (yellow) the bed's luminance.
pub const HOT_EDGE_COV_MAX: f32 = 118.0;

/// The relative luminance the hot edge's ink is lifted to when its stop is
/// darker: twice [`BED_LUMA_MAX`]. Orange, yellow and green are carried pure;
/// red, blue, indigo and violet are lifted through their own hue, then toward
/// white ([`hot_edge_ink`], [`onto_luma`]). A floor on the ink, not the
/// composite: the additive hairline lands `ink * cov / 255` on the bed.
pub const HOT_EDGE_LUMA_FLOOR: f32 = 2.0 * BED_LUMA_MAX;

/// The spine value from which the hot edge's momentum gain starts rising
/// (over [`HOT_EDGE_DISP_SPAN`]); `disp` 0.15 is the second key at 8 cps.
pub const HOT_EDGE_DISP_MIN: f32 = 0.15;

/// Width of the smoothstep that fades the hot edge in over
/// [`HOT_EDGE_DISP_MIN`] — `smoothstep((disp − 0.15)/0.30)`.
pub const HOT_EDGE_DISP_SPAN: f32 = 0.30;

/// The share of the hot edge's gain a cold hand gets (a word's first key,
/// momentum zero), so the mark attaching the band's top edge to the caret
/// exists on every typed key; the rest of the gain ramps with the hand over
/// [`HOT_EDGE_DISP_SPAN`]. Typed keys only: an arrow's wake keeps the
/// momentum-only gain (`hot_edge_gain`'s `typed`).
pub const HOT_EDGE_GAIN_FLOOR: f32 = 0.5;

/// How far, in `ch`, the hot edge's hairline may sit from the caret's cell
/// band before it becomes a highlighter on another row. It rides `spine - up`
/// (`0.10 ch` above the cell top when tall, `0.36 ch` below when underline);
/// `0.55` bounds both with room for the wave.
#[cfg(test)]
pub const HOT_EDGE_CARET_REACH_CH: f32 = 0.55;

/// Cells of the classic walk's first phase, which advances `d/16` per cell so
/// a short word already shows several stops. [`CROSS_PACE`] moves cells within
/// the arc without lengthening it, so the meteor's reach still covers a sweep.
pub const WALK_FAST_CELLS: f32 = 16.0;

/// The classic walk's steady lay rate, `t`-units per cell, after
/// [`WALK_FAST_CELLS`]. The meteor arc continues this exact rate backwards
/// from the caret's stop, so a change here changes the meteor.
pub const WALK_LAY_RATE: f32 = 1.0 / 36.0;

/// The birth floor: a laid cell is readable on the exact frame that echoes its
/// key. The 18 ms `edge-in` ramps from zero, and the host can present an echo
/// 0.2-0.4 ms after the key, where the new cell would quantise to nothing. The
/// floor rides only the cell's birth envelope; the focus-regain rearm keeps
/// its unfloored `edge_in` so regain never snaps.
pub const BIRTH_EDGE_FLOOR: f32 = 0.35;

/// The tall body's reach above the spine, in `ch`. The band covers the whole
/// cell, so star birth zones are defined against [`Band::top`] and land in
/// row - 1.
pub const TALL_UP_CH: f32 = 1.10;

/// The tall body's full-strength plateau above the spine, in `ch`. Under
/// `aterm_render::RIBBON_CORE_SHARE` of [`TALL_UP_CH`], so the rasterizer's
/// plateau clamp is a no-op and the melt has the rest of the cell to fall
/// through.
pub const TALL_CORE_CH: f32 = 0.40;

/// The downward melt's floor, in `ch`: the narrowest the transverse falloff
/// below the spine may get, at any momentum and wave phase.
pub const DN_FLOOR_CH: f32 = 0.25;

/// The downward reach at full momentum, in `ch`: [`DN_FLOOR_CH`] plus
/// [`WAVE_AMP_CELLS`], so the wave's room is inside the tile by construction.
pub const DN_TOP_CH: f32 = DN_FLOOR_CH + WAVE_AMP_CELLS;

/// The underline spelling's reach above the spine, in `ch`: the rest of the
/// cell once [`DN_TOP_CH`] is spent below it, so the mark's top edge lands
/// `0.36 ch` under the cell top.
pub const UNDERLINE_UP_CH: f32 = 1.0 - DN_TOP_CH;

/// The leading's half-thickness in `ch`: the underline spelling's plateau and
/// the baseline strip's span.
pub const LEAD_CH: f32 = 0.11;

/// The tall highlighter's shoulder: the full shoulder, so the letters stand
/// inside the light and the profile melts only at the cell edge.
///
/// `aterm_render::ribbon_profile` short-circuits at exactly `1.0`, so this
/// spelling is also the cheap one.
pub const SHOULDER_TALL: f32 = 1.0;

/// The underline spelling's quieter glyph-band wash: the value the profile
/// passes through once the core ends. The two spellings differ only in
/// [`BodyProfile`]; there is no second emitter.
pub const SHOULDER_UNDERLINE: f32 = 0.55;

/// The 0.75-cycle wave's amplitude in cells at full spine (about 2 px at
/// `ch 18`). The head is pinned ([`WAVE_HEAD_PIN`]) and the body stays inside
/// its rows (below [`TALL_UP_CH`], above [`DN_TOP_CH`]).
pub const WAVE_AMP_CELLS: f32 = 0.110;

/// Cycles of the wave across the whole mark, whatever its length: three
/// quarters, so the mark undulates once and does not read as a ripple.
pub const WAVE_CYCLES: f32 = 0.75;

/// The share of the mark over which the wave's amplitude smoothsteps up from
/// the head. The head is pinned so the cell under the hand never moves.
pub const WAVE_HEAD_PIN: f32 = 0.35;

/// Cells over which the body's downward wedge closes behind the head. The mark
/// is fatter under the hand and settles into the leading behind it — about a
/// word's worth of swell.
pub const BLOOM_REACH_CELLS: f32 = 8.0;

/// Vertices per cell along the major axis, at most. `ribbon_beam` interpolates
/// colour between vertices in sRGB, so colour is resolved from the arc at every
/// slab; three keeps the cyan dwell under the arc's own. A ceiling:
/// [`Ribbon::slabs_per_cell`] plans fewer when live cells x rows would not fit
/// [`RIBBON_QUAD_BUDGET`], so the tail is not cut.
pub const SLABS_PER_CELL: usize = 3;

/// The baseline strip's gain, as a share of the cell's ink coverage: the crisp accent under
/// the letters, riding `aterm_render::ribbon_lift_profile` over [`LEAD_CH`]. [`Ribbon::emit`]
/// clamps it to the frame top minus `cov` ([`BODY_FRAME_TOP`] on a dark theme), so the strip
/// stays an accent on the body and never the only bright part of the mark.
const STRIP_LIFT_GAIN: f32 = 0.35;

// ===========================================================================
// The comet body, the vivid rail and the from-the-hand attack
// (RAINBOW-KITTY-V2.md §30). `Config::ribbon_flat` turns all three off.
// ===========================================================================

/// The comet's reach below the spine at the hand, in `ch`, at full momentum:
/// the lower lobe under the caret that the rail fills with vivid ink. It
/// closes to [`DN_FLOOR_CH`] over [`BLOOM_REACH_CELLS`] behind the head, and
/// with the spine's release ([`Ribbon::settle_step_s`]) when the hand stops;
/// shape follows the live spine, light does not.
pub const COMET_DN_HAND_CH: f32 = 0.55;

/// The tall body's reach above the spine at the comet's tail, in `ch`: it
/// thins from [`TALL_UP_CH`] at the head to this over [`COMET_TAPER_CELLS`],
/// still covering the x-height and cap zone. Applied as a share of the
/// profile's `up`, so the underline spelling thins in proportion.
pub const COMET_UP_TAIL_CH: f32 = 0.80;

/// Cells behind the head over which the comet's `up` tapers from
/// [`TALL_UP_CH`] to [`COMET_UP_TAIL_CH`] (smoothstep). Longer than
/// [`BLOOM_REACH_CELLS`] so the head reads as a head, not a bulge.
pub const COMET_TAPER_CELLS: f32 = 12.0;

/// The rail's ink luminance floor: a stop whose full-value colour is darker
/// (blue, indigo, violet) is walked toward white to reach it ([`rail_ink`]
/// through [`onto_luma`]); every warm stop is carried pure. Below
/// [`HOT_EDGE_LUMA_FLOOR`] because the rail is a wide bed, not a transient:
/// blue stays blue rather than pale.
pub const RAIL_LUMA_FLOOR: f32 = 0.20;

/// The rail's luminance ceiling. The rail lives below the row bottom, never
/// under a letter, so it takes its own ceiling rather than the caret's
/// ([`crate::cursor_glow::RAINBOW_CARET_LIGHT_FLOOR`]). At `Y 0.54` yellow's
/// ink is `(200, 200, 0)`; brighter stops are scaled down through their own hue
/// ([`rail_ink`] via [`onto_luma`]) so the rail reads as one weight.
pub const RAIL_LUMA_CEIL: f32 = 0.54;

const _: () = assert!(
    RAIL_LUMA_FLOOR < RAIL_LUMA_CEIL,
    "the rail's floor must sit under its ceiling"
);

/// The rail's coverage as a share of the body's at the same slab. At `1.0` the
/// rail composites at the bed's [`UNDER_COV_CAP`] and goes out with the body
/// (edge-in, wipe, melt, retract, swoosh). Lower it to let more of the bed's
/// stop show through.
pub const RAIL_GAIN: f32 = 1.0;

/// The rail's full-strength plateau as a share of its reach: the top half
/// holds full value and the bottom half melts to zero
/// (`aterm_render::ribbon_profile`, shoulder 1.0). The most the rasterizer
/// allows, so the rail is a bar with a soft underside.
pub const RAIL_CORE_SHARE: f32 = aterm_render::RIBBON_CORE_SHARE;

/// The from-the-hand attack's duration, seconds: a new cell's light crosses
/// the cell from the caret side in this long. Longer than [`EDGE_IN_S`] so
/// the crossing reads as motion, shorter than a key at any human cadence so
/// no cell is still filling when the next key lands.
pub const ATTACK_WIPE_S: f32 = 0.040;

/// The wipe front's softness, in cells: the front is a smoothstep this wide,
/// so the light pours rather than sweeps. At 0.35 a three-slab cell shows
/// the front on every one of the wipe's frames.
pub const ATTACK_WIPE_FRONT: f32 = 0.35;

/// How far below the row bottom the vivid rail may reach, in cells:
/// [`DN_TOP_CH`], the ink-free reach (the row below's caps start about
/// `0.2 ch` under its cell top). The part of the comet's lower lobe below
/// this line is bed ink at the bar, never full-value colour under a letter.
pub const RAIL_REACH_MAX_CH: f32 = DN_TOP_CH;

// The vivid rail stays above the line a sub-script cell's ink starts at
// (`aterm_render::SUB_CEILING`): the Claude Code footer writes its facts
// `Script::Sub` into the rule right under the composer row this rail runs
// beneath. Only the comet's bed lobe at the hand reaches lower.
const _: () = assert!(RAIL_REACH_MAX_CH < aterm_render::SUB_CEILING);

// ===========================================================================
// The bed's one ceiling (RAINBOW-KITTY-V2.md §3.2, §3.4)
// ===========================================================================

/// The bed's byte ceiling: the source-over stream's cap. The 15 levels above it, up to the
/// ledger's frame top ([`BODY_FRAME_TOP`]), are the baseline strip's.
pub const UNDER_COV_CAP: f32 = 236.0;

/// The coverage at which a planned boundary counts as on the glass for
/// `trail status` ([`Ribbon::lit_segments`]): half the cap. The bed's dimmest
/// stop composites at a brightest channel of about 80 at full coverage, and
/// the paint scanner's colour floor is 60 over a ground near 20, so the
/// dimmest stop reads as ink from about half coverage up.
pub const STATUS_LIT_COV: u8 = (UNDER_COV_CAP / 2.0) as u8;

/// The paint scanner's colour floor: the glass census reads a pixel as band
/// ink when its brightest channel is at least [`SCAN_INK_MAX`] and its channel
/// spread at least [`SCAN_INK_SPREAD`]. [`STATUS_LIT_COV`] solves this once
/// for the dimmest stop; [`reads_as_ink`] applies it per stop.
pub const SCAN_INK_MAX: u32 = 60;

/// …and the spread half of it — see [`SCAN_INK_MAX`].
pub const SCAN_INK_SPREAD: u32 = 40;

/// Whether a composited `0x00RRGGBB` pixel reads as band ink to the glass census
/// ([`SCAN_INK_MAX`] / [`SCAN_INK_SPREAD`]): per stop, where [`STATUS_LIT_COV`] is the arc's
/// worst case. [`Ribbon::emit`]'s drawn count reads it.
#[inline]
#[must_use]
pub const fn reads_as_ink(rgb: u32) -> bool {
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    // Compares, not `Ord::max`/`min`: `Ribbon::emit` asks this of every quad
    // it lays, and an unoptimized build calls those out of line.
    let mx = if r > g { r } else { g };
    let mx = if mx > b { mx } else { b };
    let mn = if r < g { r } else { g };
    let mn = if mn < b { mn } else { b };
    mx >= SCAN_INK_MAX && mx - mn >= SCAN_INK_SPREAD
}

/// The ledger's frame top: the level a `glow_under` pixel may reach once every
/// stream on it is priced. The bed stops at [`UNDER_COV_CAP`]; the 15 levels
/// above are the baseline strip's. Safe because the bed's ink carries the bar
/// ([`bed_luma_budget`]), so a brighter alpha cannot break it.
const BODY_FRAME_TOP: f32 = 251.0;

// The contrast bar every legibility ceiling in the rainbow family is solved
// against (unlit ink over lit ground, 5.25:1, L3) and its rounding guard are
// the cursor family's one pair: `cursor_glow::BODY_CONTRAST_BAR` and
// `BODY_CONTRAST_GUARD`, which the light ground's bodies are solved against
// too. Here the guard is the margin [`solve_for_luma`] answers under and the
// LUT's lerp stays under (the EOTF is convex).
use crate::cursor_glow::{BODY_CONTRAST_BAR, BODY_CONTRAST_GUARD};

/// Floor on the bed's composited luminance budget. A theme whose foreground is
/// almost black would solve to a bed nobody can see; below this the ribbon
/// stops obeying the bar and simply takes the dimmest light that still reads.
const BED_LUMA_MIN: f32 = 0.020;

/// The ceiling: the bar's own answer at a white foreground (`(1.0 + 0.05) /
/// 5.25 - 0.05`), so under the bar this clamp binds nowhere. On the default
/// theme [`bed_luma_budget`] solves to `Y ~ 0.083` (about `(80, 80, 3)` for
/// yellow).
const BED_LUMA_MAX: f32 = 0.150;

/// The bed's chroma floor: the least HSV saturation a stop may carry into
/// [`bed_ink`] and [`hot_edge_ink`], applied before the luminance solve
/// ([`crate::spectrum::spectrum_with_min_saturation`]), so no stop reads as a
/// grey notch. The shipped arc is `S 1.00` throughout, so `1.0` is the identity
/// there; it stays because [`bed_ink`] is public.
pub const BED_SAT_FLOOR: f32 = 1.0;

/// The light bed's one weight: the OKLab distance from the page every stop's tint is
/// solved onto ([`paper_ink`]). At `0.24` on white the arc composites from a coral red
/// through apricot, a nearly pure yellow, lime and cyan to a sky blue and lilac: a
/// highlighter's rainbow, light enough that dark letters keep far more than the bar.
pub const PAPER_WEIGHT: f32 = 0.24;

/// The light bed's bar on a theme whose own text sits under the body's (Solarized Light's
/// `#586e75` on `#fdf6e3` is 4.97:1): this share of the theme's own contrast, so such a
/// theme keeps a fainter ribbon rather than none ([`paper_ink`]).
pub const PAPER_BAR_THEME_SHARE: f32 = 0.8;

/// The leading-only body's weight (the underline spelling, [`InkRole::Leading`]): no
/// letter stands on it, and a thin band needs more ink than a tall one to read.
pub const PAPER_WEIGHT_LEADING: f32 = 0.30;

/// The least contrast the caret block keeps against a light page ([`paper_caret_ink`]): the
/// glyph under a block cursor is cut out in the page's own colour, and 3:1 is WCAG's bar for
/// large text and marks.
pub const PAPER_CARET_CONTRAST: f32 = 3.0;

/// The least contrast a point mark keeps against a light page ([`paper_mark_ink`]): stars,
/// sparks, pin arms, the coma and the landing flash can cross a letter, so they stay a
/// step deeper than the caret.
pub const PAPER_MARK_CONTRAST: f32 = 4.5;

/// The vivid inks' hue bend on paper ([`paper_caret_ink`], [`paper_mark_ink`]), as
/// `(from, to)` HSV hue knots in degrees, linear between them. A yellow darkened to [`PAPER_CARET_CONTRAST`] at its own hue is olive, and no
/// continuous path from orange to green avoids the olive band (about `45°..85°`), so the
/// path crosses it in under twenty degrees of the arc instead of fifty, gently enough
/// that a palette sampled along it steps unseen: yellow and its greener neighbours land
/// on amber (a stop bent only to `50°` still darkened to a mustard olive on the caret),
/// chartreuse on leaf green.
const PAPER_HUE_BEND: [(f32, f32); 4] = [(30.0, 30.0), (72.0, 40.0), (90.0, 85.0), (110.0, 110.0)];

// No floor for the bed against the ground: it would lift the bed over the
// luminance the text can bear. The bar wins; a dark-foreground theme's dim bed
// (Solarized Dark at the `BED_LUMA_MIN` clamp) is the accepted exception.

/// Entries in the bed-ink lookup table. The recipe is a bisection over the
/// sRGB EOTF, too expensive per slab, but a pure function of arc position, so
/// it is solved once per theme into this table and read with one lerp. 129
/// entries keep the interpolation error under one level on the whole arc.
pub const BED_INK_LUT_LEN: usize = 129;

/// Share of the ceiling a cold key's cell takes (momentum buys the rest,
/// [`Cell::cov0`]), so a single keystroke lays a readable band; momentum shows
/// as length, life, the hot edge and the wave. Moves only inside the
/// legibility bar.
pub const BODY_COLD_SHARE: f32 = 0.88;

// ===========================================================================
// Life, the chain law, and the exit swoosh
// ===========================================================================

/// The host's duration knob, as a multiplier on the base cell life.
pub const LIFE_DURATION_GAIN: f32 = 1.2;

/// Floor of the base cell life, seconds.
pub const LIFE_BASE_MIN: f32 = 0.26;

/// Ceiling of the base cell life, seconds.
pub const LIFE_BASE_MAX: f32 = 0.55;

/// Linear momentum term of the cell life: `base · (1 + 2.6·d + 11·d²)`. A
/// visible wake from the very first keys.
pub const LIFE_SPINE_LINEAR: f32 = 2.6;

/// …and the superlinear term, so an earned hot run drags a rainbow across many
/// cells and wrapped lines instead of merely a longer wake.
pub const LIFE_SPINE_SQUARE: f32 = 11.0;

/// Four-letter guarantee: within a rhythm a cell lives at least this many
/// observed inter-key gaps, so the ribbon spans about the last four typed
/// cells at any human cadence (four cells need ~3 gaps; the extra half-gap is
/// the tail's fade room).
pub const CHAIN_KEYS: f32 = 4.5;

/// The ribbon chains across any plausible rhythm, even one key every five
/// seconds. Only an isolated keystroke (a gap beyond this) takes no floor and
/// fades crisply.
pub const CHAIN_GAP_MAX: f32 = 5.0;

/// Fade margin added to the chained life, seconds.
pub const CHAIN_MARGIN: f32 = 0.10;

/// …capped just above `CHAIN_KEYS × CHAIN_GAP_MAX` so the slowest chained
/// rhythm still earns its full four-letter span.
pub const CHAIN_LIFE_MAX: f32 = 23.0;

// ===========================================================================
// The wake: what a jump leaves behind
// ===========================================================================

/// New light a jump's corridor may lay, in cells back from the landing, so a Ctrl-A across
/// 200 columns does not paint the whole row; band light beyond it is covered, never cut. The
/// cap binds at the corridor's far edge: light the corridor keeps past it (on the glass,
/// either layer) moves the far edge out and the columns between are laid (`Ribbon::wake`).
pub const WAKE_MAX_CELLS: u16 = 32;

/// A wake's life: long enough to read as an after-effect, short enough that a Ctrl-A / Ctrl-E
/// ping-pong cannot accumulate. Fixed, not [`Ribbon::cell_life`]: a jump takes neither the
/// chain law nor the swoosh floor. A later jump over it re-owns it (`Ribbon::own_wake_cohort`);
/// a later scrub holds it like any cohort under the hand, unless it is past its melt opening.
pub const WAKE_LIFE_S: f32 = 1.10;

// The wake is born along the flight: corridor cell `j` of `n` from the origin is
// born at `t0 + T * j / n` with `T = flight_ms(cells)`, the arrival edge shared
// by the meteor head, bell, kitty `land_at` and pin. Until born a cell is dark
// ([`Ribbon::env_of`]). A hop (under [`JUMP_MIN_CELLS`]) takes
// [`FLIGHT_MIN_MS`].

/// A same-row hop under [`JUMP_MIN_CELLS`] (an arrow key) lays its wake at this
/// share of [`WAKE_LIFE_S`]. Its cells go into their own cohort
/// ([`Cohort::wake`]) on the row's walk, never a typed word's, so an arrow does
/// not change a word's grace, bounds or swoosh; cells a live cell owns are left
/// alone.
pub const WAKE_HOP_LIFE_SHARE: f32 = 0.5;

/// Cells the four-letter guarantee reaches for.
pub const FOUR_LETTER_CELLS: u16 = 4;

/// The phrase rest's floor, seconds: the least grace a cohort keeps after its
/// phrase's last key before the swoosh begins, derived from the melody's rest
/// ([`super::timing::PHRASE_PAUSE_MS`]). A cohort's actual grace is
/// [`Cohort::grace_s`] (`max(0.90, 2.5 * ioi)`); the totals below are at this
/// floor.
pub const PHRASE_REST_MIN_S: f32 = super::timing::PHRASE_PAUSE_MS as f32 / 1000.0;

/// One extension cell lights per this step while the swoosh finishes reaching
/// four letters.
pub const REACH_STEP_S: f32 = 0.05;

/// Beats of [`REACH_STEP_S`] the reach spends.
pub const REACH_BEATS: u16 = 3;

/// Cells the reach may stage in one tick, across every live cohort. Sized so
/// the staging array is a fixed local and the frame path allocates nothing; a
/// starved reach continues on the next beat.
pub const REACH_STAGE: usize = 8;

/// The retract drains the ribbon tail->head over this long, whatever its
/// length.
pub const RETRACT_DUR_S: f32 = 0.40;

/// How long a retracted cell takes to go out once the drain selects it, so
/// the vanish is continuous rather than whole-cell deletions ending in a
/// cliff.
pub const RETRACT_FADE_S: f32 = 0.24;

/// The fold flow's whole life. A cohort on a row the hand left downward by
/// typing ([`Cohort::flow`]: a shell fold, a composer re-wrap, or a run carried
/// up with its text) skips the swoosh's grace and reach and, from the fold:
///
/// * drifts as one rigid band toward the fold point ([`Ribbon::retract_x`]);
/// * settles, thinning toward the seam with the new line ([`Ribbon::sample`]);
/// * fades far end first to a common end ([`Cohort::flow_drain`]), each stop at
///   its own colour floor ([`ember_level`]), dark exactly this long after the
///   flow's clock.
///
/// Never brighter than it was, and shorter than [`SWOOSH_TOTAL_S`].
pub const FLOW_TOTAL_S: f32 = 1.50;
const _: () = assert!(FLOW_TOTAL_S < SWOOSH_TOTAL_S);

/// The flow's far-first lead: a flowing band's far end starts spending at the
/// fold and its fold end this much later (the swoosh's spread,
/// [`RETRACT_DUR_S`]); every cell reaches its floor at [`FLOW_TOTAL_S`]
/// ([`Cohort::flow_drain`]).
pub const FLOW_LEAD_S: f32 = RETRACT_DUR_S;

/// The flow's last breath: over its final seconds a flowing band thins into
/// its spine, every column together ([`Ribbon::sample`]), while each stop holds
/// its colour floor. (A shared alpha fade would take the low-floor stops dark
/// first, opening gaps.)
pub const FLOW_TAIL_S: f32 = RETRACT_FADE_S;

/// The drift of the coldest hand, cells.
pub const FLOW_DRIFT_MIN_CELLS: f32 = 2.0;

/// The drift of the hottest hand, cells: half a cell per reach beat over the
/// whole flow (`FLOW_TOTAL_S / (4 * REACH_STEP_S)` = 7.5). Priced once from
/// the fold-end cell's birth momentum on the hot edge's ramp
/// ([`HOT_EDGE_DISP_MIN`], [`HOT_EDGE_DISP_SPAN`]), so a hot hand's row
/// drifts farther and no drift grows after the last key.
pub const FLOW_DRIFT_MAX_CELLS: f32 = FLOW_TOTAL_S / (4.0 * REACH_STEP_S);

/// The drift's cap as a share of the band's kept length: at least 60 % of
/// every band is still on glass when its light is gone, so a short row never
/// collapses into its fold point.
pub const FLOW_DRIFT_MAX_SHARE: f32 = 0.40;

/// The settle: the share of the body's reach above the spine, and of the
/// wedge's opening below the floor, a flowing band gives up by the end of its
/// flow. Shape only; it can only shrink the covered area.
pub const FLOW_SETTLE_SHARE: f32 = 0.40;

/// The seam's level: the hot edge's own level one cell behind the head,
/// `(1 - 1/HOT_EDGE_CELLS)^2` = 4/9, held as a floor under the hot edge's
/// falloff along the new line's tail while the row above drifts out
/// ([`Ribbon::seam`]), and spent with it.
pub const SEAM_LEVEL: f32 = (1.0 - 1.0 / HOT_EDGE_CELLS) * (1.0 - 1.0 / HOT_EDGE_CELLS);
/// The re-anchor's floor, cells, matching `cursor_glow::classify_move` (which
/// admits a re-anchor only at `raw_dist > 2`): a one- or two-cell same-row
/// backward echo is plain typing. The guarded trigger (`cursor_glow::flush_park`)
/// is pinned in `cursor_glow/tests/park.rs`.
pub const RE_ANCHOR_MIN_CELLS: u16 = 3;

/// The exit swoosh past the grace: `3 * 0.05 reach + 0.40 retract + 0.24
/// fade` = 0.79 s. The whole swoosh is `grace + 0.79 s`, [`swoosh_total_s`]
/// of the cohort's own grace.
pub const SWOOSH_AFTER_GRACE_S: f32 =
    REACH_STEP_S * REACH_BEATS as f32 + RETRACT_DUR_S + RETRACT_FADE_S;

/// The whole exit swoosh at the floor grace: `0.90 grace + 0.79` = 1.69 s,
/// the least after-glow a phrase leaves. A cohort's own is
/// [`Cohort::swoosh_total`]; the grace is tempo-scaled above the floor, so a
/// slower hand gets a longer after-glow.
pub const SWOOSH_TOTAL_S: f32 = PHRASE_REST_MIN_S + SWOOSH_AFTER_GRACE_S;

/// The margin a cell's life keeps past its swoosh, so the swoosh always ends the ribbon
/// before the natural melt can. A cell's life floor is [`swoosh_life_s`] of the rest its
/// phrase was priced at when laid, and every hold of the phrase re-floors its cells
/// ([`Ribbon::hold`]), so a rest that lengthens under a slowing hand never outlives them.
pub const SWOOSH_LIFE_MARGIN_S: f32 = 0.01;

/// The life floor at the floor grace, 1.70 s.
pub const SWOOSH_LIFE_S: f32 = SWOOSH_TOTAL_S + SWOOSH_LIFE_MARGIN_S;

/// The swoosh's retract offset at the floor grace: grace plus three reach
/// beats (a cohort's own is [`Cohort::retract_start`]). A jump abandons the
/// band by rewinding its cohorts' `alive_at` this far, so the mark goes out
/// through retract + fade without a step (clamping `life` would step, since the
/// melt rides `age / life`).
pub const RETRACT_START_S: f32 = retract_start_s(PHRASE_REST_MIN_S);

/// The idle at which a cohort of grace `grace_s` starts its retract.
#[inline]
#[must_use]
pub const fn retract_start_s(grace_s: f32) -> f32 {
    grace_s + REACH_STEP_S * REACH_BEATS as f32
}

/// The whole swoosh of a cohort of grace `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_total_s(grace_s: f32) -> f32 {
    grace_s + SWOOSH_AFTER_GRACE_S
}

/// The life floor of a cell whose phrase rests `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_life_s(grace_s: f32) -> f32 {
    swoosh_total_s(grace_s) + SWOOSH_LIFE_MARGIN_S
}

/// Focus loss embers the ribbon out over this long on `spend` (§8.2). Nothing
/// sounds, and nothing is retracted — the mark simply stops being lit.
pub const FOCUS_EMBER_S: f32 = 0.30;

/// A stop is on the glass while its composite is a colour the eye can pick
/// from the ground: [`SEEN_CONTRAST`] against the background in its carrying
/// channel, and at least this much channel spread (the paint census's
/// `max - min >= 40`).
pub const SEEN_SPREAD: f32 = 40.0;

/// The level half of [`SEEN_SPREAD`]: the WCAG contrast the composite must
/// stand from the ground in its furthest channel (`channel_contrast`).
///
/// A ratio rather than a byte lift, since a fixed lift lands on different sides
/// of the census floor (`max(r, g, b) >= 60 && max - min >= 40`) on different
/// grounds. On `#111318` the census's 60 is about 1.71:1; `1.90` adds margin
/// for rounding and sampling. It cannot lift a stop the bed's bar leaves under
/// the census (most of Solarized Dark's); that stop holds until the pool
/// clears.
pub const SEEN_CONTRAST: f32 = 1.90;

/// The ember's per-stop floor, in that stop's extinction units: the focus
/// ember leaves a stop at [`BedInkLut::seen_floor_at`] times this, just over
/// the colour floor so rounding never takes one stop under while neighbours are
/// lit. Kept small: the band ends when `Ribbon::retire` clears the pool.
pub const EMBER_FLOOR_MARGIN: f32 = 1.05;

/// The curtain: at a coordinate-space seam the host cannot carry cells across
/// (alternate screen, tab or terminal switch, resize with no reflow map,
/// content invalidation), every cohort is drawn into the hand it last stood
/// under over this long, farthest first ([`CURTAIN_STAGGER_S`]), and nothing
/// is laid meanwhile. Equal to the retract's fade ([`RETRACT_FADE_S`]).
pub const CURTAIN_S: f32 = RETRACT_FADE_S;

/// The curtain's stagger: the window over which cells start spending, in
/// [`Cohort::drain_rank`] order (farthest from the hand at `0`). It takes the
/// retract's ratio of stagger to span of `CURTAIN_S`. A band loses about `n *
/// frame / CURTAIN_STAGGER_S` boundaries per frame (22 % at 30 fps); see
/// `CURTAIN_MAX_STEP_FRAC`.
pub const CURTAIN_STAGGER_S: f32 = CURTAIN_S * (RETRACT_DUR_S / (RETRACT_DUR_S + RETRACT_FADE_S));

/// What each cell of a falling curtain spends over once its
/// [`CURTAIN_STAGGER_S`] turn has come: the remainder, so the last cell to
/// start (under the hand) reaches exactly zero at `CURTAIN_S`.
pub const CURTAIN_SPEND_S: f32 = CURTAIN_S - CURTAIN_STAGGER_S;

/// The curtain's cliff bound: the most of the band one 33 ms frame may take,
/// as a fraction of what was lit, in `trail status` and census units
/// (`ribbon_segments`, [`Ribbon::ink_segments`]). A fraction because
/// `slabs_per_cell` scales the count; headroom over 22 % covers bunching.
#[cfg(test)]
pub const CURTAIN_MAX_STEP_FRAC: f32 = 0.34;

/// The other half of the law: a curtain must be seen to fall. At 30 fps at
/// least this many frames carry band ink between the seam and darkness.
#[cfg(test)]
pub const CURTAIN_MIN_FRAMES: usize = 5;

/// The content retirement's melt: a cell whose glyph was replaced goes out over
/// this long on the retract's `spend` law ([`Ribbon::retire_cells`]), so a box
/// relocation's old band is dark before the next key echoes. A glyph that
/// merely went (a submit, Ctrl-U) is released to the swoosh's retract instead
/// ([`Ribbon::release_cells`]).
pub const RETIRE_MELT_S: f32 = 0.12;

/// The share of its light a line keeps once a Return has left it printed
/// ([`Cohort::printed`]): the band dims to this over [`RETURN_DIM_S`] and goes
/// on out through its own swoosh, so the letters it stands under are clear
/// of it by the time the output lands.
pub const RETURN_PRINTED_SHARE: f32 = 0.3;

/// How long the dim to [`RETURN_PRINTED_SHARE`] takes after the Return.
pub const RETURN_DIM_S: f32 = 0.06;

/// The share of a cell's life over which the expiry melt runs; identity over the first 70 %.
pub const EXPIRY_MELT_SHARE: f32 = 0.30;

/// The expiry melt's exponent. Superlinear, because a cell out of time must
/// be at zero before it is removed or the removal is a visible step: at 1.6
/// the melt reaches 6e-4 of body on the last frame.
pub const EXPIRY_MELT_GAMMA: f32 = 1.6;

/// The coverage share the run's outer tail boundary keeps, ramping to full across the tail
/// cell, so the oldest edge feathers out. At every boundary the right side enters at this
/// share and the brighter wins, `max(left, right * ease)`, the left side owning the boundary
/// whichever way the run streams. Beside a leaving cell the feather grows as fast as that
/// cell's light goes, so no dark column opens.
pub const RUN_TAIL_EASE: f32 = 0.10;

// ===========================================================================
// The atoms
// ===========================================================================

/// The instant a cell's expiry is measured from: its birth or its cohort's last
/// live typing key, whichever is later, so a mark ages only once the hand
/// stops. A cell whose cohort is gone falls back to its birth. O(cohorts).
#[inline]
#[must_use]
pub fn live_since(cohorts: &[Cohort], cell: &Cell) -> Instant {
    cohorts
        .iter()
        .find(|c| c.id == cell.cohort)
        .map_or(cell.born, |c| cell.born.max(c.alive_at))
}

/// The freshest cohort (latest [`Cohort::alive_at`]), ties broken toward the
/// walk that has gone farther at its rightmost column ([`Cohort::walk_d`] at
/// `col1`), never by pool order. The one reader for "the walk the hand was
/// last on". The follow split makes such ties: the copy carried a row up and
/// the remnant on the caret row share one clock, and the remnant (ending one
/// past the eaten Space) is where the walk goes on.
#[inline]
fn freshest<'a>(cohorts: impl Iterator<Item = &'a Cohort>) -> Option<&'a Cohort> {
    cohorts.max_by(|a, b| {
        a.alive_at
            .cmp(&b.alive_at)
            .then_with(|| a.walk_d(a.col1).total_cmp(&b.walk_d(b.col1)))
    })
}

/// Which layer a cell is on. Typed and insert lays write `Base`; a wake writes
/// only `Over` and never touches a `Base` cohort. A born `Over` cell is drawn
/// where one exists ([`Ribbon::build`]), else the `Base` beneath. The wake
/// shares the band's walk origin ([`Ribbon::wake_origin`]), so both layers
/// agree on colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    /// A typed key's, an insert's, or the swoosh's reach's cell.
    #[default]
    Base,
    /// A wake's cell — a jump's corridor or a hop's trail.
    Over,
}

/// One laid ribbon cell, the atom of the field.
///
/// `Copy` and flat: the cell pool is a resident `Vec` reused every frame
/// (zero allocation), and a style-crossfade ghost snapshots it.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    /// Grid row.
    pub row: u16,
    /// Grid column.
    pub col: u16,
    /// Which [`Cohort`] laid it — the hue walk's identity, so a re-typed cell
    /// re-joins the run it belongs to instead of restarting at red.
    pub cohort: u32,
    /// The field at this cell: the classic walk's spectrum position, 0..1+.
    /// The caret, the ribbon head and a star's halo on this cell read this
    /// number, so they are the same colour on the same frame.
    pub t: f32,
    /// When this cell was laid (the echo frame). Its identity in the content
    /// witness and the newest-writer ordering; a fresh lay never inherits an
    /// earlier glyph's record. The expiry melt runs from [`live_since`] —
    /// this, or the cohort's last live key, whichever is later.
    pub born: Instant,
    /// Start of the visual 18 ms `edge-in`. A live same-kind re-wet keeps
    /// the earlier attack so an already lit glyph does not dip, while `born`
    /// still names the fresh cell. A leaving cell starts a fresh attack.
    /// Future wake cells remain dark until `born`, even with an older attack.
    pub attack_at: Instant,
    /// Total life in seconds, priced at birth from `Spine::birth_disp` and
    /// measured from the cohort's clock ([`live_since`]), so no cell runs out
    /// under a hand that is still typing.
    pub life_s: f32,
    /// The cell's light, priced once at birth from `Spine::birth_disp`: the share of
    /// [`UNDER_COV_CAP`] its body may take at peak, `0..1`. Never re-read from the live
    /// spine, which is still climbing when the hand stops: a body re-priced every frame would
    /// gain light with no keystroke behind it.
    pub cov0: f32,
    /// True for a cell laid by a real typing advance (not the exit swoosh's
    /// reach); stardust's field-star deal reads it.
    pub typing: bool,
    /// Set when the cell is retracting toward the caret (Backspace or kill):
    /// the `spend` alpha law runs from this stamp. `None` while the cell is
    /// simply living. The exit swoosh drives the whole cohort instead, because
    /// it retracts the mark as one object.
    pub retract_at: Option<Instant>,
    /// Set when the cell is retired by content ([`Ribbon::retire_cells`]): its
    /// glyph was replaced. The cell spends to zero over [`RETIRE_MELT_S`] and
    /// leaves the pool, its attack frozen at the stamp. `None` while living and
    /// for a glyph that merely went ([`Ribbon::release_cells`]).
    pub retire_at: Option<Instant>,
    /// The `Spine::birth_disp` this cell was priced at (the number behind
    /// [`Cell::cov0`]), so the hot edge is priced from the head cell's birth
    /// too rather than from the still-climbing live follower.
    pub birth_disp: f32,
    /// The crisp edge's reach, in cells, priced at birth by [`edge_cells`] from
    /// the surge and flow's heat. Read only by [`Ribbon::emit_hot_edge`] from
    /// the head cell, never from the live spine (as [`Cell::cov0`]).
    pub edge_cells: f32,
    /// Which layer this cell is on — see [`Layer`].
    pub layer: Layer,
    /// Re-armed from a level: `(when, level)`, set when a cell's light continues
    /// another's (a wake cell over a live band cell, or a corridor cell re-owned
    /// by a later jump). [`Ribbon::env_of`] reads it in place of the birth
    /// attack: from `level` at `when` up to full through `edge-in`. `None` for a
    /// cell born from dark.
    pub rearm: Option<(Instant, f32)>,
    /// A partial release's stamp: when a release naming only part of this
    /// cell's cohort put it on the retract ([`Ribbon::release_cells`]).
    /// `retract_at` carries the same instant and does the leaving; this says
    /// why, so an identical redraw inside the melt can lift exactly these
    /// stamps ([`Ribbon::restore_releases`]) and the content witness keeps
    /// reading the cell for that restore while it leaves.
    pub released_at: Option<Instant>,
}

impl Cell {
    /// True once the cell is on its way out (retracting after a Backspace or
    /// kill, or retired by content) and so no longer owns its column: a sweep
    /// may lay over it, a wake may not take it over, it is not the head, and a
    /// witness does not read it. The one "is this cell live" predicate.
    #[inline]
    #[must_use]
    pub fn leaving(&self) -> bool {
        self.retract_at.is_some() || self.retire_at.is_some()
    }
}

/// A cohort: one contiguous run of cells laid by one typing burst on one row,
/// sharing a hue walk. The unit the exit swoosh, the wrap fold and the kill
/// drain act on.
#[derive(Clone, Copy, Debug)]
pub struct Cohort {
    /// Identity, referenced by [`Cell::cohort`].
    pub id: u32,
    /// The row this cohort lives on. A cohort never spans rows; a wrap folds into a new cohort
    /// (§4's wrap fold).
    pub row: u16,
    /// Leftmost column the cohort covers.
    pub col0: u16,
    /// One past the rightmost column the cohort covers.
    pub col1: u16,
    /// The walk's origin: the column of the cohort's first typed cell,
    /// immutable. `t` is a function of position, `walk_t(d0 + col -
    /// anchor_col)` from the walk's zero ([`Cohort::t_at`]), so the swoosh's
    /// reach, edits and inserts ([`Ribbon::shift_run`]) never repaint a colour
    /// already read, and a cell backspaced and retyped takes back its stop.
    pub anchor_col: u16,
    /// The `t` at [`Cohort::anchor_col`]. A same-row rebirth or wrap fold
    /// within the freshness window continues the previous walk (with
    /// [`Cohort::d0`]); past it the walk re-anchors at red. A new line
    /// continues the colour on a new walk ([`Ribbon::fresh_line`]); a rebirth
    /// beside a lit run takes that run's walk ([`Ribbon::join_cohort`]).
    pub t0: f32,
    /// The walk's odometer: distance in cells at [`Cohort::anchor_col`] since
    /// the walk last started. `0` for a cohort that starts a walk (cold at red,
    /// a new line, a wake on a row with nothing fresh); for a continuation (a
    /// wrap fold, the composer's relay, a same-row rebirth) the distance the
    /// continued walk had reached there. Signed and integer-valued, so
    /// [`walk_t`]'s knee falls on a column. A continuation shares the walk's
    /// zero ([`Cohort::walk_zero`]), so a wrapped line walks as the same text
    /// unwrapped.
    pub d0: f32,
    /// When the cohort's first cell was laid.
    pub born: Instant,
    /// The exit swoosh's grace anchor: the last live typing key on this
    /// cohort's row. A key holds every un-abandoned cohort on its own row and
    /// none elsewhere ([`Ribbon::place`]), so a wrapped paragraph's earlier row
    /// leaves through its own swoosh while the hand types on. An
    /// [`Cohort::abandoned`] cohort keeps its own clock. Also the clock cell
    /// expiry runs from ([`live_since`]); monotone.
    pub alive_at: Instant,
    /// Set by a real jump ([`Ribbon::abandon`]): this cohort leaves on its own
    /// rewound clock and typing elsewhere no longer refreshes it, so the band
    /// under the hand's old position goes out while the new one is laid.
    pub abandoned: bool,
    /// Minted by a wake ([`Ribbon::wake`]) and never typed into. A hop's new
    /// cells join only such a cohort or mint one, so an arrow beside a live
    /// word touches neither its clock nor its bounds; a typed key laid into a
    /// wake cohort clears the flag.
    pub wake: bool,
    /// The caret column the retract pulls this cohort toward, captured the
    /// frame it entered its retract ([`Ribbon::advance_swoosh`]) and held until
    /// a key brings the cohort back to laying: toward the hand that let go of
    /// it, not the live caret. A flowing edge narrows with its remaining live
    /// cells, so an erased suffix cannot pull their light off the text.
    pub retract_col: Option<u16>,
    /// Which end drains first once the retract owns the mark, frozen beside
    /// [`Cohort::retract_col`]: `false` (left end first) for the caret's own
    /// row and rows above it, `true` (right end first) for a row below the
    /// caret. The band always drains away from the hand.
    pub drain_right_first: bool,
    /// Where the cohort is in the exit choreography.
    pub phase: Phase,
    /// An abandon the hand did not make (a program's caret round trip, or the
    /// hand leaving the row), which a typed key landing in or beside this
    /// cohort takes back ([`Ribbon::join_cohort`]). A jump the hand made (Nav,
    /// Return) is not rejoinable.
    pub rejoinable: bool,
    /// The phrase this cohort belongs to: opened by the first typed key after a
    /// rest, a Return or a line kill, closed by the next. Cohorts minted or
    /// joined by a typed lay (or a wrap fold) while `P` is open belong to `P`;
    /// `0` for a wake. A key refreshes only the open phrase's cohorts on the
    /// hand's row ([`Ribbon::place`]).
    pub phrase: u32,
    /// The grace this cohort's swoosh keeps, seconds: the phrase rest
    /// ([`super::timing::phrase_rest_ms`]) at the hand's tempo when the cohort
    /// was last held, [`PHRASE_REST_MIN_S`] at the floor. The reach begins
    /// this long after `alive_at`; see [`Cohort::retract_start`] and
    /// [`Cohort::swoosh_total`].
    pub grace_s: f32,
    /// The fold flow: `Some(at)` when the hand left this cohort's row downward by typing
    /// ([`Ribbon::leave_row`]) or the follow pass carried the run up with its text
    /// ([`Ribbon::translate_run`]). The row then flows at once instead of holding its grace
    /// (see [`FLOW_TOTAL_S`]), on a clock that is the later of `at` and its last key
    /// ([`Cohort::clock`]). Cleared when the hand is back on the row: a lay into the cohort, a
    /// hold on its row ([`Ribbon::hold`], [`Ribbon::take_row_back`]), or the follow pass
    /// carrying it back onto the caret's row.
    pub flow: Option<Instant>,
    /// The drift, cells: how far a flowing band slides toward its fold point.
    /// Priced once, the frame the flow's target is frozen
    /// ([`Ribbon::advance_swoosh`]), from the fold-end cell's birth momentum,
    /// between [`FLOW_DRIFT_MIN_CELLS`] and [`FLOW_DRIFT_MAX_CELLS`] and never
    /// past [`FLOW_DRIFT_MAX_SHARE`] of the kept length. `0` when not flowing.
    pub flow_drift: f32,
    /// A content-only release may be undone when the same complete cell
    /// identities and glyphs return; a later abandon/retirement revokes it.
    /// The whole release's clock ([`Ribbon::release_cells`]); partial ones are
    /// in [`Cohort::release_parts`].
    release_clock: Option<ReleaseClock>,
    /// The partial releases in custody, each with the stamp it laid on its
    /// cells ([`Cell::released_at`]) and how many it laid. A part is lifted
    /// when its own glyphs are back, whatever the other parts wait for
    /// (`Ribbon::restore_releases`); a later abandon/retirement revokes them.
    release_parts: [Option<ReleasePart>; RELEASE_PARTS],
    /// The instant the parts in custody were last revoked, until the next
    /// tick lets them go ([`Ribbon::settle_revoked_parts`]).
    parts_revoked_at: Option<Instant>,
    /// Drained whole by a line kill: a `^U` / `^K` left this cohort with no
    /// cell standing ([`Ribbon::mark_line_killed`]). Its retracting cells are
    /// dead to a key ([`Ribbon::stands`]), since the next line is a new line;
    /// a run Backspaced or `^W`-ed away is still its band.
    line_killed: bool,
    /// The Return that left this cohort's line printed text
    /// ([`Ribbon::print_row`]): from then its light is capped toward
    /// [`RETURN_PRINTED_SHARE`] while the swoosh keeps its clock. A key that
    /// rejoins the run lifts it.
    printed: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct ReleaseClock {
    alive_at: Instant,
    retract_col: Option<u16>,
    phase: Phase,
    rejoinable: bool,
    /// Every cell the cohort had at the release: the count a restore must find
    /// still resident (nothing joins an abandoned, unrejoinable cohort, so no
    /// cell born since can be among them).
    cells: usize,
}

/// Partial releases one cohort holds in custody at once
/// (`Cohort::release_parts`). A torn read releases one part per present and
/// a restore lifts it on the next; four is headroom for a burst of torn
/// presents under an edit. Past it the oldest part is let go and its cells
/// finish their retract.
pub const RELEASE_PARTS: usize = 4;

/// One partial release in custody: its stamp and the cells it stamped. A
/// restore compares the stamp's own cells by identity (`released_at ==
/// stamp`), never the cohort's count, which a key typed inside the melt
/// changes.
#[derive(Clone, Copy, Debug)]
struct ReleasePart {
    stamp: Instant,
    cells: usize,
}

impl Cohort {
    /// Revoke every release in custody at `at` (the whole release's clock and
    /// every partial part): the hand edited or left the mark, or a glyph under
    /// it was replaced. The whole release's clock goes at once; partial parts
    /// held at `at` go at the start of the next tick
    /// ([`Ribbon::settle_revoked_parts`]), so this frame's content read can
    /// still lift a part whose glyphs are back. A part released at `at` or
    /// later is not revoked.
    #[inline]
    fn revoke_releases(&mut self, at: Instant) {
        self.release_clock = None;
        self.parts_revoked_at = Some(self.parts_revoked_at.map_or(at, |t| t.max(at)));
    }

    /// Hold a partial release in custody; with every slot taken, the
    /// oldest part is let go.
    #[inline]
    fn hold_part(&mut self, part: ReleasePart) {
        let slot = self
            .release_parts
            .iter()
            .position(Option::is_none)
            .or_else(|| {
                self.release_parts
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, p)| p.map(|p| p.stamp))
                    .map(|(i, _)| i)
            });
        if let Some(i) = slot {
            self.release_parts[i] = Some(part);
        }
    }

    /// The partial release in custody that laid `stamp`.
    #[inline]
    fn part_mut(&mut self, stamp: Instant) -> Option<&mut ReleasePart> {
        self.release_parts
            .iter_mut()
            .flatten()
            .find(|p| p.stamp == stamp)
    }

    /// The band is leaving at `at`: abandoned by a jump, or inside its swoosh (retracting,
    /// fading, or over). A flowing cohort reads its own clock and schedule
    /// ([`Cohort::idle_s`], [`Cohort::phase_at`]): the fold flow has no grace and no reach, so
    /// from the fold onward it is leaving. The one predicate both the scrub (`Ribbon::scrub`)
    /// and the hop (`Ribbon::wake`) read, so they never disagree about one cell.
    #[inline]
    #[must_use]
    pub fn leaving_at(&self, at: Instant) -> bool {
        self.abandoned
            || self
                .phase_at(self.idle_s(at))
                .is_none_or(Phase::is_retracting)
    }

    /// The idle at which this cohort's retract begins: its grace plus the
    /// three reach beats.
    #[inline]
    #[must_use]
    pub fn retract_start(&self) -> f32 {
        if self.flow.is_some() {
            0.0
        } else {
            retract_start_s(self.grace_s)
        }
    }

    /// How long this cohort's retract's farthest-first stagger spans: the
    /// swoosh's [`RETRACT_DUR_S`], or the fold flow's [`FLOW_LEAD_S`]
    /// ([`Ribbon::env_of`], [`Cohort::flow_drain`]).
    #[inline]
    #[must_use]
    pub fn retract_span(&self) -> f32 {
        if self.flow.is_some() {
            FLOW_LEAD_S
        } else {
            RETRACT_DUR_S
        }
    }

    /// The drift's progress, `0..1` at `idle` seconds into the flow:
    /// `1 - spend(idle / FLOW_TOTAL_S)`, an ease-out fastest on the fold frame
    /// and at rest exactly as the band goes dark. Read by the slide
    /// ([`Ribbon::retract_x`]) and the settle ([`Ribbon::sample`]).
    #[inline]
    #[must_use]
    pub fn flow_progress(&self, idle: f32) -> f32 {
        1.0 - spend(clamp01(idle / FLOW_TOTAL_S))
    }

    /// The flow's fade at a column, `1 -> 0`: the far end (drain rank 1) spends
    /// from the fold over the whole [`FLOW_TOTAL_S`], the fold end from
    /// [`FLOW_LEAD_S`] later, and every cell reaches its floor at
    /// [`FLOW_TOTAL_S`], a common end, so the band never shortens into a
    /// crawling dim edge. Applied by [`Ribbon::env_of`] through each stop's
    /// floor ([`ember_level`]).
    #[inline]
    #[must_use]
    pub fn flow_drain(&self, col: u16, idle: f32) -> f32 {
        let s = FLOW_LEAD_S * (1.0 - self.drain_rank(col));
        spend(clamp01((idle - s) / (FLOW_TOTAL_S - s)))
    }

    /// The idle at which this cohort's swoosh — or its fold flow — is over.
    #[inline]
    #[must_use]
    pub fn swoosh_total(&self) -> f32 {
        if self.flow.is_some() {
            FLOW_TOTAL_S
        } else {
            swoosh_total_s(self.grace_s)
        }
    }

    /// The phase this cohort is in `idle_s` after its last live key:
    /// [`Phase::at`] on its own grace, or for a flowing cohort
    /// ([`Cohort::flow`]) the flow's schedule, `Retracting` from the first
    /// instant until [`FLOW_TOTAL_S`] and then over.
    #[inline]
    #[must_use]
    pub fn phase_at(&self, idle_s: f32) -> Option<Phase> {
        if self.flow.is_none() {
            return Phase::at(idle_s, self.grace_s);
        }
        if idle_s.is_nan() || idle_s <= 0.0 {
            Some(Phase::Laying)
        } else if idle_s < FLOW_TOTAL_S {
            Some(Phase::Retracting)
        } else {
            None
        }
    }

    /// The clock the exit runs from: the last live key on this cohort's row
    /// ([`Cohort::alive_at`]), or for a flowing cohort the later of that and
    /// the fold ([`Cohort::flow`]). Every phase, drain and slide reads idle
    /// from this.
    #[inline]
    #[must_use]
    pub fn clock(&self) -> Instant {
        self.flow.map_or(self.alive_at, |f| self.alive_at.max(f))
    }

    /// Seconds of idle at `now` on [`Cohort::clock`].
    #[inline]
    #[must_use]
    pub fn idle_s(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.clock()).as_secs_f32()
    }

    /// The drain rank of a column, `0..=1`: how early in the retract the cell
    /// at `col` starts spending, `1` first (farthest from the hand in logical
    /// order), `0` last. Read from the frozen [`Cohort::drain_right_first`],
    /// so the order cannot flip mid-drain.
    #[inline]
    #[must_use]
    pub fn drain_rank(&self, col: u16) -> f32 {
        let span = f32::from(self.col1.saturating_sub(self.col0)).max(1.0);
        if self.drain_right_first {
            f32::from(col.saturating_sub(self.col0)) / span
        } else {
            f32::from(self.col1.saturating_sub(1).saturating_sub(col)) / span
        }
    }

    /// The walk's distance at a column: [`Cohort::d0`] at the origin, plus one
    /// per cell right of it, minus one per cell left of it (a reach cell).
    /// Exact: every term is an integer.
    #[inline]
    #[must_use]
    pub fn walk_d(&self, col: u16) -> f32 {
        self.d0 + (f32::from(col) - f32::from(self.anchor_col))
    }

    /// The walk's zero: the `t` the walk has at distance `0`,
    /// `t0 - walk_t(d0)`, i.e. the colour the walk started on (`0.0` for a
    /// cold start, the continued colour for a new line
    /// ([`Ribbon::fresh_line`])). A cohort that starts a walk (`d0 = 0`) has
    /// exactly `t0` here; a continuation shares the zero of the walk it
    /// continues (exactly on a cold walk, to the ulp otherwise).
    #[inline]
    #[must_use]
    pub fn walk_zero(&self) -> f32 {
        self.t0 - walk_t(self.d0)
    }

    /// The field at a column of this cohort: the classic walk read from the
    /// immutable origin, the same however the cell was laid (a reach cell
    /// before the origin continues the walk backwards). Read as `walk_zero() +
    /// walk_t(walk_d(col))`, so a continued cohort keeps its walk's pace. Keep
    /// the operation order: `meteor::LandingWalk::at` performs the same two
    /// float operations on [`Ribbon::walk_origin_at`]'s pair, so colours match
    /// bit for bit.
    #[inline]
    #[must_use]
    pub fn t_at(&self, col: u16) -> f32 {
        self.walk_zero() + walk_t(self.walk_d(col))
    }

    /// The walk at `col` as a continuation reads it: the `t` and distance a
    /// cohort minted to continue this one starts from. The one reader every
    /// continuation uses (`join_cohort`, `wake_origin`, `relay_origin`,
    /// `retire`'s [`LastWalk`]), so no site carries the colour and forgets the
    /// pace; a site that starts a new walk drops the distance explicitly
    /// ([`WalkPos::restarted`]).
    #[inline]
    #[must_use]
    fn walk_at(&self, col: u16) -> WalkPos {
        WalkPos {
            t: self.t_at(col),
            d: self.walk_d(col),
        }
    }

    /// The walk at [`Cohort::anchor_col`] — `(t0, d0)`, the pair the cohort
    /// was minted on.
    #[inline]
    #[must_use]
    fn origin_walk(&self) -> WalkPos {
        WalkPos {
            t: self.t0,
            d: self.d0,
        }
    }
}

/// A point on the classic walk: its `t` and its distance in cells from where
/// the walk started, the pair a cohort is minted on ([`Cohort::t0`],
/// [`Cohort::d0`]). Along one input the two travel together; where the hand
/// starts a new line the distance is dropped on purpose
/// ([`WalkPos::restarted`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct WalkPos {
    /// The walk's `t` here.
    t: f32,
    /// The walk's distance here, cells from where the walk started.
    d: f32,
}

impl WalkPos {
    /// A cold start: red, on the first cell of the fast leg.
    const COLD: Self = Self { t: 0.0, d: 0.0 };

    /// The colour continues, the pace restarts: this walk's `t` at distance
    /// zero, for a cohort that starts a walk on a colour it continues (a new
    /// line, [`Ribbon::fresh_line`]; a wake on a row with nothing fresh,
    /// [`Ribbon::wake_origin`]). Its cells read `t + walk_t(col - anchor_col)`.
    #[inline]
    const fn restarted(self) -> Self {
        Self { t: self.t, d: 0.0 }
    }
}

/// A cohort's place in the exit swoosh (§4: grace, `3 × 0.05` reach, `0.40` retract, `0.24`
/// fade; the grace is the phrase rest, [`PHRASE_REST_MIN_S`] at the floor).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// Taking cells — the hand is on it.
    #[default]
    Laying,
    /// The grace after the last key (the phrase rest, [`Cohort::grace_s`]): nothing moves yet.
    Grace,
    /// The three 0.05 s reach beats.
    Reaching,
    /// The 0.40 s retract toward the caret on `suck-in`.
    Retracting,
    /// The 0.24 s fade to exactly zero.
    Fading,
}

impl Phase {
    /// The phase a cohort of grace `grace_s` ([`Cohort::grace_s`]) is in
    /// `idle_s` seconds after its last live key. `None` once the swoosh has
    /// finished and the cohort is over.
    #[inline]
    #[must_use]
    pub fn at(idle_s: f32, grace_s: f32) -> Option<Self> {
        let grace = if grace_s.is_finite() {
            grace_s.max(PHRASE_REST_MIN_S)
        } else {
            PHRASE_REST_MIN_S
        };
        let reach_end = retract_start_s(grace);
        let retract_end = reach_end + RETRACT_DUR_S;
        if idle_s.is_nan() || idle_s <= 0.0 {
            // NaN-refusing spelling: a non-finite idle takes the laying arm,
            // which draws the mark rather than dropping it.
            return Some(Self::Laying);
        }
        if idle_s < grace {
            Some(Self::Grace)
        } else if idle_s < reach_end {
            Some(Self::Reaching)
        } else if idle_s < retract_end {
            Some(Self::Retracting)
        } else if idle_s < swoosh_total_s(grace) {
            Some(Self::Fading)
        } else {
            None
        }
    }

    /// True once the swoosh owns the mark and the retract is moving it.
    #[inline]
    #[must_use]
    pub fn is_retracting(self) -> bool {
        matches!(self, Self::Retracting | Self::Fading)
    }
}

/// One planned sample of a cohort's spine at a cell boundary, what
/// `aterm_render::RibbonVertex` is built from.
///
/// Boundary-anchored, not centre-anchored (see `RibbonVertex`): a segment
/// runs boundary-to-boundary so every slab `ribbon_beam` tiles lands inside
/// one cell however the step divides the cell width.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Window-absolute X of the boundary, sub-pixel.
    pub x: f32,
    /// The body's full-strength centreline here, sub-pixel; the 0.75-cycle wave
    /// (`WAVE_AMP_CELLS · ch · disp`) rides on it.
    pub spine: f32,
    /// Reach above the spine before the profile is exactly zero.
    pub up: f32,
    /// …and below it.
    pub dn: f32,
    /// The field at this boundary (C2) — resolved to colour through
    /// `spectrum::spectrum(t)`.
    pub t: f32,
    /// Coverage at the spine, already folding time-fade, intensity, the
    /// `edge-in` and the expiry melt. Capped at [`UNDER_COV_CAP`].
    pub cov: u8,
}

/// The vertical extent of the ribbon's body at one cell, `[spine - up,
/// spine + dn]`. Stardust draws no mark inside it and places its sky band
/// (`[top - 0.30 ch, top - 0.04 ch]`) relative to `top`, since the tall body
/// covers the whole cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// Window-absolute Y of the body's top edge (`spine − up`).
    pub top: f32,
    /// Window-absolute Y of the body's centreline, wave included.
    pub spine: f32,
    /// Window-absolute Y of the body's bottom edge (`spine + dn`).
    pub bottom: f32,
}

impl Band {
    /// True when `y` lies inside the body — §4.2's whole claim, in one call, so
    /// stardust never has to re-derive the inequality.
    #[must_use]
    #[cfg(test)]
    pub fn contains(&self, y: f32) -> bool {
        (self.top..=self.bottom).contains(&y)
    }
}

/// The two profiles, one emitter: the tall/underline fork is these four
/// numbers and nothing else. Both spellings walk the same [`Ribbon::plan`],
/// are drawn by the same `aterm_render::ribbon_beam` call, and price their
/// light through the same [`Cell::cov0`] law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyProfile {
    /// Reach above the spine, in `ch`.
    pub up_ch: f32,
    /// Reach below the spine at full momentum, in `ch`.
    pub dn_ch: f32,
    /// Full-strength plateau above the spine, in `ch`.
    pub core_up_ch: f32,
    /// The shoulder `aterm_render::ribbon_profile` takes on the up side — the
    /// one parameter the two spellings really disagree about.
    pub shoulder: f32,
}

/// One row's dense `col → t` lane of the field index.
#[derive(Clone, Debug, Default)]
struct FieldLane {
    /// The grid row this lane indexes.
    row: u16,
    /// `t` by column, `f32::NAN` where the row carries no field. Sized to the
    /// grid's column count once and reused; cleared over
    /// [`FieldLane::touched`] only, never zero-filled wholesale.
    t: Vec<f32>,
    /// The columns written this frame, so the next frame's clear is O(marks)
    /// rather than O(columns).
    touched: Vec<u16>,
}

impl FieldLane {
    /// Clear only what the last frame wrote — the O(marks) reset §18 asks for.
    #[inline]
    fn clear(&mut self) {
        for &col in &self.touched {
            if let Some(slot) = self.t.get_mut(usize::from(col)) {
                *slot = f32::NAN;
            }
        }
        self.touched.clear();
    }

    /// Size the lane to `cols`, filling with the "no field here" sentinel.
    #[inline]
    fn resize(&mut self, cols: u16) {
        self.t.clear();
        self.t.resize(usize::from(cols), f32::NAN);
        self.touched.clear();
    }
}

/// The field index: `col -> t` for every row carrying live ribbon light, rebuilt once per
/// frame by [`Ribbon::plan`]. Lookups are O(1) in columns and linear in live rows (a handful:
/// the ribbon is a per-row mark).
#[derive(Clone, Debug, Default)]
pub struct FieldIndex {
    /// One lane per live row. Resident and reused; see [`FieldLane::touched`].
    lanes: Vec<FieldLane>,
    /// Lanes whose row went dark, kept at full width for the next row that
    /// lights (an Enter or a wrap re-lights a row every few seconds while
    /// typing), so no per-frame allocation. Bounded by the most rows ever live
    /// at once.
    spare: Vec<FieldLane>,
    /// The grid width the lanes were sized to — a resize rebuilds them.
    cols: u16,
}

impl FieldIndex {
    /// The field at a cell, or `None` where no ribbon light is laid.
    #[inline]
    #[must_use]
    pub fn at(&self, row: u16, col: u16) -> Option<f32> {
        let lane = self.lanes.iter().find(|l| l.row == row)?;
        let t = *lane.t.get(usize::from(col))?;
        if t.is_nan() { None } else { Some(t) }
    }

    /// Rows carrying field light this frame (the linear term of every
    /// lookup).
    #[must_use]
    #[cfg(test)]
    pub fn live_rows(&self) -> usize {
        self.lanes.len()
    }

    /// Begin a frame: retire lanes for rows that carried no light last frame
    /// into the spare pool, and clear the rest over their own `touched` list.
    ///
    /// The prune keeps lookups linear in live rows rather than in every row
    /// ever typed on. A grid resize drops every lane, live and spare, since
    /// none is the right width any more.
    #[inline]
    fn begin(&mut self, cols: u16) {
        if self.cols != cols {
            self.cols = cols;
            self.lanes.clear();
            self.spare.clear();
            return;
        }
        let mut i = 0;
        while i < self.lanes.len() {
            if self.lanes[i].touched.is_empty() {
                let dark = self.lanes.swap_remove(i);
                self.spare.push(dark);
            } else {
                i += 1;
            }
        }
        for lane in &mut self.lanes {
            lane.clear();
        }
    }

    /// The lane for `row`, from the spare pool if the row is newly lit and
    /// minted only when the pool is empty: paid once per "most rows ever live
    /// at once", never per frame.
    #[inline]
    fn lane_mut(&mut self, row: u16) -> &mut FieldLane {
        if let Some(i) = self.lanes.iter().position(|l| l.row == row) {
            return &mut self.lanes[i];
        }
        let mut lane = self.spare.pop().unwrap_or_default();
        lane.row = row;
        if lane.t.len() != usize::from(self.cols) {
            lane.resize(self.cols);
        }
        self.lanes.push(lane);
        self.lanes.last_mut().expect("just pushed")
    }

    /// Drop every lane, live and spare.
    #[inline]
    fn reset(&mut self) {
        self.lanes.clear();
        self.spare.clear();
    }

    /// Move every lane with a scroll of `rows`: a lane whose row leaves the
    /// grid retires into the spare pool, cleared over its `touched` list; the
    /// rest keep their columns and take the new row, so [`FieldIndex::at`]
    /// answers for the moved cell between the scroll and the next plan.
    #[inline]
    fn translate(&mut self, rows: u16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = self.lanes[i].row.checked_sub(rows) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    /// [`FieldIndex::translate`]'s row-band twin: a lane outside
    /// `top..=bottom` keeps its row, a lane inside moves by `delta`, and a lane
    /// that leaves the band retires into the spare pool, so
    /// [`FieldIndex::at`] answers for the moved cell until the next plan (the
    /// law [`crate::cursor_glow::band_row`] states; the edge row is a real cell
    /// that does not own this light).
    #[inline]
    fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = band_row(self.lanes[i].row, top, bottom, delta) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    /// Write one cell's field. First writer wins, and the plan feeds this
    /// newest-first: the newest cell at a column owns it, and an older cell it
    /// shadows is never seen.
    #[inline]
    fn set(&mut self, row: u16, col: u16, t: f32) {
        let lane = self.lane_mut(row);
        let Some(slot) = lane.t.get_mut(usize::from(col)) else {
            return;
        };
        if !slot.is_nan() {
            return;
        }
        lane.touched.push(col);
        *slot = t;
    }
}

// ===========================================================================
// The bed's ink
// ===========================================================================

/// The bed's one ceiling, in relative luminance: the brightest a source-over
/// ribbon may composite and still leave `theme_fg` legible over it at
/// [`BODY_CONTRAST_BAR`] plus [`BODY_CONTRAST_GUARD`]. A function of the
/// foreground alone, clamped to [`BED_LUMA_MIN`] and [`BED_LUMA_MAX`];
/// [`bed_ink`] spends it.
#[inline]
#[must_use]
pub fn bed_luma_budget(theme_fg: u32) -> f32 {
    let bar =
        (relative_luminance(theme_fg) + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05;
    // `clamp` is total here: both bounds are finite constants with
    // `BED_LUMA_MIN < BED_LUMA_MAX`, and `relative_luminance` of a byte
    // triple is finite.
    bar.clamp(BED_LUMA_MIN, BED_LUMA_MAX)
}

/// Every channel scaled by `k`: a uniform gamma-space scale, which leaves a
/// colour's channel ratios (hue and saturation) unchanged.
#[inline]
fn scale_rgb(rgb: u32, k: f32) -> u32 {
    let ch = |sh: u32| ((((rgb >> sh) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// A walk toward white by `w`: `c + w * (255 - c)`. Channel differences
/// survive unscaled, so the hue direction is kept and only saturation falls.
/// The last resort of [`onto_luma`], taken only from a stop already at full
/// value, because white buys light at the price of chroma.
#[inline]
fn toward_white(rgb: u32, w: f32) -> u32 {
    let ch = |sh: u32| {
        let c = ((rgb >> sh) & 0xff) as f32;
        ((c + w * (255.0 - c)) + 0.5).clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Bisect a monotone one-parameter colour family for the brightest member
/// under `target` relative luminance. 24 halvings resolve below 1e-7. The
/// answer is `family(lo)`, the last parameter whose bytes measured under the
/// target (never the midpoint, which can round over); `Y(family(lo)) <
/// target` holds throughout.
#[inline]
fn solve_for_luma(family: impl Fn(f32) -> u32, target: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if crate::color_math::relative_luminance_with(table, family(mid)) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    family(lo)
}

/// A stop put on one relative luminance at the most chroma that luminance
/// allows: the family the bed and the hot edge solve over.
///
/// One monotone family, bisected by [`solve_for_luma`]: from black, the stop
/// scaled through itself to its hue at full value (`k = 255 / max`), then for
/// a target the hue cannot reach, a walk toward white. A dark stop gets its
/// light from value, not chroma (indigo `#4B0082` at `Y 0.097` is `(127, 0,
/// 219)`). Blue alone is under the bar at full value (`Y 0.072`) and takes some
/// white; a stop brighter than the target scales down.
#[inline]
fn onto_luma(rgb: u32, target: f32) -> u32 {
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return solve_for_luma(|w| toward_white(rgb, w), target);
    }
    let k_full = 255.0 / mx as f32;
    let full = scale_rgb(rgb, k_full);
    if relative_luminance(full) >= target {
        solve_for_luma(|k| scale_rgb(rgb, k * k_full), target)
    } else {
        solve_for_luma(|w| toward_white(full, w), target)
    }
}

/// The bed's on-glass ink: an arc colour put on one composited luminance, `budget`
/// ([`bed_luma_budget`]), at the most chroma that luminance allows: [`BED_SAT_FLOOR`] first,
/// then [`onto_luma`] (a colour brighter than the budget is scaled down, hue exact; a darker
/// one is lifted through its own hue to full value and only then toward white). Every stop
/// therefore composites at one weight, so one coverage ceiling holds for the whole arc; on the
/// default theme a warm stop at that weight composites at `(80, 80, 3)`.
#[inline]
#[must_use]
pub fn bed_ink(rgb: u32, budget: f32) -> u32 {
    onto_luma(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR), budget)
}

/// The bed's light twin, its ink on paper. On a light page the body is ink, not light, and
/// darkening a stop onto one luminance (the dark bed's law) prints yellow as olive and orange
/// as brown. Here each stop keeps its own hue at full value and spends coverage instead: it
/// is tinted toward the page until the composite stands `role`'s weight
/// ([`PAPER_WEIGHT`], [`PAPER_WEIGHT_LEADING`]) from the page in OKLab, so the whole arc
/// reads at one weight (yellow nearly pure, blue a sky tint), and a body that letters stand
/// on ([`InkRole::OverText`]) stops short of the legibility bar against `fg` too (the body's
/// bar, or [`PAPER_BAR_THEME_SHARE`] of the theme's own contrast where that is lower).
///
/// Returned as the ink that composites there at the role's own alpha ceiling, so the
/// coverage law ([`cov_cap`]) is unchanged and a dimmer slab is only a fainter tint.
#[must_use]
pub(crate) fn paper_ink(rgb: u32, fg: u32, bg: u32, role: InkRole) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return bg;
    }
    let full = scale_rgb(rgb, 255.0 / mx as f32);
    let bg = bg & 0x00FF_FFFF;
    let cap = (role.alpha_cap() / 255.0).clamp(0.0, 1.0);
    let table = crate::color_math::relative_luminance_table();
    let y_fg = crate::color_math::relative_luminance_with(table, fg);
    let ratio = |y: f32| (y.max(y_fg) + 0.05) / (y.min(y_fg) + 0.05);
    let (weight, bar) = match role {
        InkRole::OverText => (
            PAPER_WEIGHT,
            Some((BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD).min(
                PAPER_BAR_THEME_SHARE
                    * ratio(crate::color_math::relative_luminance_with(table, bg)),
            )),
        ),
        InkRole::Leading => (PAPER_WEIGHT_LEADING, None),
    };
    // The emitter rounds the ink to bytes and composites it in integer source-over, each
    // within half a step per channel (`aterm_render::over_premul`), and on paper one step of
    // green under dark text is a whole guard's worth of contrast. So the bar is read on the
    // composite a full step darker in every channel: the worst the two roundings print.
    let worst = |ink: u32| {
        let ch = |sh: u32| {
            let (i, b) = (((ink >> sh) & 0xff) as f32, ((bg >> sh) & 0xff) as f32);
            (b + (i - b) * cap - 1.0).max(0.0) as u32
        };
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    };
    // Both halves are monotone in the share: the composite walks a straight line from the
    // page to the hue, away from the page and (on a light page) toward the dark text.
    let holds = |share: f32| {
        let on_glass = lerp_rgb(bg, full, share * cap);
        oklab_distance(on_glass, bg) <= weight
            && bar.is_none_or(|bar| {
                ratio(crate::color_math::relative_luminance_with(
                    table,
                    worst(lerp_rgb(bg, full, share)),
                )) >= bar
            })
    };
    let share = if holds(1.0) {
        1.0
    } else {
        // 24 halvings resolve below 1e-7; the answer is the last share that held.
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            if holds(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    lerp_rgb(bg, full, share)
}

/// The caret's ink on paper: the deepest mark of the trail, as the dark theme's caret is its
/// brightest. The stop at full value (yellow bent through [`PAPER_HUE_BEND`]), darkened
/// with its hue intact only as far as [`PAPER_CARET_CONTRAST`] against `bg` needs; red,
/// blue, indigo and violet already clear it and stay pure.
#[inline]
#[must_use]
pub fn paper_caret_ink(rgb: u32, bg: u32) -> u32 {
    paper_vivid_ink(rgb, bg, PAPER_CARET_CONTRAST)
}

/// The caret's colour for the walk's stop `arc`: the authored stop on a dark
/// ground (the consumer applies its own ink), that stop's caret ink on the
/// page on a light one ([`paper_caret_ink`]). The one spelling both the live
/// head ([`Ribbon::head_rgb`]) and a seated stop
/// (`CursorGlow::v2_caret_stop_rgb`) read, so the two agree to the byte.
#[inline]
#[must_use]
pub(crate) fn caret_ink(arc: u32, cfg: &Config) -> u32 {
    if cfg.dark_theme {
        arc
    } else {
        paper_caret_ink(arc, cfg.theme_bg)
    }
}

/// A point mark's ink on paper: [`paper_caret_ink`]'s law at [`PAPER_MARK_CONTRAST`], so a
/// star or spark keeps its stop's hue (a yellow one gold, never olive) and a letter it
/// crosses stays readable through it.
#[inline]
#[must_use]
pub fn paper_mark_ink(rgb: u32, bg: u32) -> u32 {
    paper_vivid_ink(rgb, bg, PAPER_MARK_CONTRAST)
}

/// The vivid inks' one law: the stop at full value, bent, darkened onto `bar` against `bg`.
fn paper_vivid_ink(rgb: u32, bg: u32, bar: f32) -> u32 {
    let (h, s, v) = rgb2hsv(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR));
    if v <= 0.0 {
        return 0;
    }
    let h = PAPER_HUE_BEND
        .windows(2)
        .find(|k| (k[0].0..k[1].0).contains(&h))
        .map_or(h, |k| {
            k[0].1 + (k[1].1 - k[0].1) * (h - k[0].0) / (k[1].0 - k[0].0)
        });
    let full = hsv2rgb(h, s, 1.0);
    let table = crate::color_math::relative_luminance_table();
    let y_bg = crate::color_math::relative_luminance_with(table, bg & 0x00FF_FFFF);
    let holds = |k: f32| {
        let y = crate::color_math::relative_luminance_with(table, scale_rgb(full, k));
        (y.max(y_bg) + 0.05) / (y.min(y_bg) + 0.05) >= bar
    };
    if holds(1.0) {
        return full;
    }
    // On a light page darker is further from the page: black holds, `1.0` does not.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if holds(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    scale_rgb(full, lo)
}

/// The WCAG contrast of one channel, both bytes read as a monochrome light:
/// the per-channel form of [`relative_luminance`]'s ratio, in which
/// [`SEEN_CONTRAST`] is stated.
///
/// Per channel because the bed equalises the arc's luminance ([`bed_ink`]), so
/// a whole-colour test cannot tell its stops apart: at one luminance violet is
/// `#0019FD` and a warm stop `#585000`. The hue-carrying channel is what the
/// eye picks out of the ground and what the census measures.
#[inline]
fn channel_contrast(linear: &[f32; 256], a: u8, b: u8) -> f32 {
    let (a, b) = (linear[usize::from(a)], linear[usize::from(b)]);
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether a composited pixel is still a stop: [`SEEN_CONTRAST`] against the
/// ground in its furthest channel ([`channel_contrast`]) and [`SEEN_SPREAD`]
/// bytes of channel spread. Contrast alone passes a grey smudge; spread alone
/// passes a tinted ground. Both halves are ground-independent.
#[inline]
#[must_use]
pub(crate) fn seen(rgb: u32, bg: u32) -> bool {
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xff) as u8;
    let (r, g, b) = (ch(rgb, 16), ch(rgb, 8), ch(rgb, 0));
    let spread = f32::from(r.max(g).max(b) - r.min(g).min(b));
    if spread < SEEN_SPREAD {
        return false;
    }
    let linear = &crate::color_math::relative_luminance_table().linear;
    let stand = channel_contrast(linear, r, ch(bg, 16))
        .max(channel_contrast(linear, g, ch(bg, 8)))
        .max(channel_contrast(linear, b, ch(bg, 0)));
    stand >= SEEN_CONTRAST
}

/// The coverage at which a stop goes out: the smallest alpha byte at which `ink` over `bg`
/// still passes [`seen`]; `256` for an ink no coverage can make read (which
/// [`Ribbon::ember_floor_of`] turns into "hold at full until the pool clears").
///
/// The bed puts every stop on one luminance, not one distance from the ground, so a shared
/// alpha fade takes the warm stops out while their neighbours are bright (a warm stop goes out
/// at several times a cool one's coverage): the arithmetic behind the ember's per-stop floor.
/// Solved once per theme into [`BedInkLut`] by a scan down from full alpha with the renderer's
/// own compositor, answering one over the highest alpha that fails.
#[inline]
#[must_use]
fn seen_floor(ink: u32, bg: u32) -> f32 {
    (1..=255u16)
        .rev()
        .map(|a| u8::try_from(a).unwrap_or(255))
        .find(|&a| !seen(over_premul(bg, premul_rgb(ink, a), a), bg))
        .map_or(1.0, |a| f32::from(a) + 1.0)
}

/// The crisp edge's reach, priced at birth: how many cells back the hairline
/// of a cell laid this tick may run ([`Cell::edge_cells`]); the longer of:
///
/// * the surge ([`super::spine::Spine::surge`]): a key priced above the
///   previous by [`super::spine::SURGE_MIN_RISE`] wears a longer streak, full
///   at [`super::spine::SURGE_FULL_RISE`]: `3 + 4·Δ/0.5` capped at 5;
/// * the open theme ([`super::Flow::heat`]): flow raises the floor from 3 to 5.
///
/// At `surge = 0` and `heat = 0` this is [`HOT_EDGE_CELLS`] exactly.
#[inline]
#[must_use]
pub fn edge_cells(surge: f32, heat: f32) -> f32 {
    let stretch = clamp01(surge).max(clamp01(heat));
    HOT_EDGE_CELLS + (HOT_EDGE_CELLS_MAX - HOT_EDGE_CELLS) * stretch
}

/// The hot edge's ink: the stop lifted to at least [`HOT_EDGE_LUMA_FLOOR`]
/// ([`onto_luma`]); a brighter stop is carried pure. The bed equalizes the arc
/// down onto the legibility budget; this equalizes it up, so the hairline is
/// visibly brighter than the bed on every stop. It takes [`BED_SAT_FLOOR`]
/// too. Additive, so not under the bed's bar: 1 px on the top edge, priced by
/// [`HOT_EDGE_COV_MAX`] inside the transient cap, ledger-held over the row
/// above's descenders (§4.1).
#[inline]
#[must_use]
pub fn hot_edge_ink(rgb: u32) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    if relative_luminance(rgb) >= HOT_EDGE_LUMA_FLOOR {
        rgb
    } else {
        onto_luma(rgb, HOT_EDGE_LUMA_FLOOR)
    }
}

/// The vivid rail's ink: the stop at full value, put inside `[RAIL_LUMA_FLOOR,
/// RAIL_LUMA_CEIL]`: lifted toward white to the floor (blue, indigo, violet), scaled down
/// through its hue to the ceiling (yellow, green), carried pure between (red, orange). The
/// rail lies where no letter can be, so it answers to its own ceiling, not the legibility
/// bar: yellow `(200, 200, 0)`, green `(0, 225, 0)`, orange `(255, 127, 0)`, red
/// `(255, 0, 0)`, blue `(103, 103, 255)`. The green→blue crossing, authored under full value,
/// is scaled up through its hue first and takes [`BED_SAT_FLOOR`]. Solved once per theme
/// into [`BedInkLut`]'s rail table.
#[inline]
#[must_use]
pub fn rail_ink(rgb: u32) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return onto_luma(rgb, RAIL_LUMA_FLOOR);
    }
    let full = scale_rgb(rgb, 255.0 / mx as f32);
    let y = relative_luminance(full);
    if y < RAIL_LUMA_FLOOR {
        onto_luma(full, RAIL_LUMA_FLOOR)
    } else if y > RAIL_LUMA_CEIL {
        onto_luma(full, RAIL_LUMA_CEIL)
    } else {
        full
    }
}

/// The light theme's compositing role for the body (§3.3, L6). The tall
/// spelling stands over letterforms and takes the conservative ink and shared
/// ceiling; the underline spelling is confined to the leading and may be vivid
/// up to the rail's ceiling. `pub(crate)` because `InkRole` is.
#[inline]
#[must_use]
pub(crate) fn light_role(cfg: &Config) -> InkRole {
    if cfg.ribbon_tall {
        InkRole::OverText
    } else {
        InkRole::Leading
    }
}

/// The body's published alpha ceiling for this theme — [`UNDER_COV_CAP`] on
/// a dark one, the compositing role's on a light one (§3.4). One spelling,
/// because [`Ribbon::cov_of`] and [`Ribbon::ember_floor`] must price the
/// same cell against the same ceiling.
#[inline]
#[must_use]
fn cov_cap(cfg: &Config) -> f32 {
    if cfg.dark_theme {
        UNDER_COV_CAP
    } else {
        light_role(cfg).alpha_cap()
    }
}

/// The bed's ink table for one theme, rebuilt only when the theme or spelling
/// changes: [`bed_ink`] is a bisection over `powf` and a pure function of arc
/// position, so it is solved once and read with one lerp (§18).
#[derive(Clone, Debug, Default)]
pub struct BedInkLut {
    /// `(theme_fg, theme_bg, dark_theme, ribbon_tall)` the table was solved
    /// for. A change in any of the four invalidates it.
    key: Option<(u32, u32, bool, bool)>,
    /// [`BED_INK_LUT_LEN`] entries over the folded arc `[0, 1]`.
    lut: Vec<u32>,
    /// The hot edge's twin table ([`hot_edge_ink`]), same positions — solved
    /// beside the bed's so the hairline costs one lerp per vertex too.
    hot: Vec<u32>,
    /// The vivid rail's table ([`rail_ink`]), same positions.
    rail: Vec<u32>,
    /// The colour floor's table ([`seen_floor`] of the bed's ink over this
    /// theme's ground, in alpha bytes), same positions. The focus ember reads
    /// it so each stop leaves on its own extinction.
    seen: Vec<f32>,
}

impl BedInkLut {
    /// Rebuild if the theme moved. Deterministic and clockless.
    fn sync(&mut self, cfg: &Config) {
        let key = (cfg.theme_fg, cfg.theme_bg, cfg.dark_theme, cfg.ribbon_tall);
        if self.key == Some(key) && self.lut.len() == BED_INK_LUT_LEN {
            return;
        }
        let budget = bed_luma_budget(cfg.theme_fg);
        let role = light_role(cfg);
        self.lut.clear();
        self.lut.reserve(BED_INK_LUT_LEN);
        self.hot.clear();
        self.hot.reserve(BED_INK_LUT_LEN);
        self.rail.clear();
        self.rail.reserve(BED_INK_LUT_LEN);
        self.seen.clear();
        self.seen.reserve(BED_INK_LUT_LEN);
        for i in 0..BED_INK_LUT_LEN {
            let x = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
            // Indexed by the walk, filled from the arc ([`walk_pace`]): readers keep
            // `sample`'s one lerp and the pace costs nothing per frame.
            let arc = spectrum(walk_pace(x));
            // A light ground: each stop keeps its own hue at full value and
            // spends coverage, not luminance ([`paper_ink`]): the tall body
            // stands under the letters, so it stops short of the body's bar
            // against the theme's text; the underline body lives in the
            // leading, under no glyph, at its own weight. (Darkening a stop
            // onto one luminance printed yellow as olive and orange as brown
            // under the letters.) On a theme whose own text sits under the
            // bar (Solarized Light) that tint can thin out of sight, and the
            // tall body falls back on the light body law, which reads there
            // (`cursor_glow::light_body_ink`).
            let bed = if cfg.dark_theme {
                bed_ink(arc, budget)
            } else {
                let paper = paper_ink(arc, cfg.theme_fg, cfg.theme_bg, role);
                let a = (cov_cap(cfg) + 16.0).min(255.0) as u8;
                let shows = seen(
                    over_premul(cfg.theme_bg, premul_rgb(paper, a), a) & 0x00FF_FFFF,
                    cfg.theme_bg,
                );
                if cfg.ribbon_tall && !shows {
                    crate::cursor_glow::light_body_ink(arc, cfg.theme_fg, cfg.theme_bg, a)
                } else {
                    paper
                }
            };
            self.lut.push(bed);
            self.hot.push(hot_edge_ink(arc));
            self.rail.push(rail_ink(arc));
            self.seen.push(seen_floor(bed, cfg.theme_bg));
        }
        self.key = Some(key);
    }

    /// The ink at walk position `t`, folded through `meteor::tri` (the arc's own reflection),
    /// so the station under the caret is the caret's stop by construction.
    #[inline]
    #[must_use]
    pub fn at(&self, t: f32) -> u32 {
        Self::sample(&self.lut, t)
    }

    /// The hot edge's ink at walk position `t` — the same fold, the hot
    /// table ([`hot_edge_ink`]).
    #[inline]
    #[must_use]
    pub fn hot_at(&self, t: f32) -> u32 {
        Self::sample(&self.hot, t)
    }

    /// The vivid rail's ink at walk position `t` — the same fold, the rail
    /// table ([`rail_ink`]).
    #[inline]
    #[must_use]
    pub fn rail_at(&self, t: f32) -> u32 {
        Self::sample(&self.rail, t)
    }

    /// The colour floor at walk position `t`: the alpha byte under which this
    /// stop's bed stops reading over the live theme's ground ([`seen_floor`]).
    /// `0` (no floor) on an unsynced table.
    ///
    /// The max of the bracketing entries, not a lerp: `seen_floor` is not
    /// linear in the ink, so a lerped floor could land under the true
    /// extinction, taking a stop dark under its neighbours. The floor moves
    /// about two bytes per table step, so the max costs at most that.
    #[inline]
    #[must_use]
    pub fn seen_floor_at(&self, t: f32) -> f32 {
        if self.seen.is_empty() {
            return 0.0;
        }
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let i = (x as usize).min(BED_INK_LUT_LEN - 1);
        let j = (i + 1).min(BED_INK_LUT_LEN - 1);
        self.seen[i].max(self.seen[j])
    }

    /// One lerp on `table` at the folded position of `t`; `0` on an unsynced
    /// table.
    #[inline]
    fn sample(table: &[u32], t: f32) -> u32 {
        if table.is_empty() {
            return 0;
        }
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let i = (x as usize).min(BED_INK_LUT_LEN - 1);
        let j = (i + 1).min(BED_INK_LUT_LEN - 1);
        lerp_rgb(table[i], table[j], x - i as f32)
    }
}

// ===========================================================================
// The walk (C2)
// ===========================================================================

/// The stretch of the arc the walk re-paces, in table entries: the green→blue
/// leg only, less a guard at each of its anchors ([`CROSS_GUARD`]). Outside it
/// the walk is the identity, so every other leg keeps its per-cell steps and
/// anchor residency bit for bit; the cells the crossing gains come from its
/// own leg's flanks.
const CROSS_LUT0: usize = crate::spectrum::SPECTRUM_ANCHOR_AT[3] + CROSS_GUARD;
/// The top of [`CROSS_LUT0`]'s window — blue's anchor, less the same guard.
const CROSS_LUT1: usize = crate::spectrum::SPECTRUM_ANCHOR_AT[4] - CROSS_GUARD;

/// The guard either side of the leg's anchors, in spectrum entries: a cell
/// within this of green or blue lands on the unpaced stop exactly. Wider than
/// the ten entries anchor residency is judged on and than half a cell's reach
/// (31.9 entries), so the neighbouring legs keep every step unchanged.
const CROSS_GUARD: usize = 24;
/// [`CROSS_LUT0`] as a share of the arc.
const CROSS_ARC0: f32 = CROSS_LUT0 as f32 / (crate::spectrum::SPECTRUM_LUT_LEN - 1) as f32;
/// [`CROSS_LUT1`] as a share of the arc.
const CROSS_ARC1: f32 = CROSS_LUT1 as f32 / (crate::spectrum::SPECTRUM_LUT_LEN - 1) as f32;

/// The walk's pace across the green→blue leg, `@generated`: where a walk position lands
/// between [`CROSS_ARC0`] and [`CROSS_ARC1`], as `u16` shares of the arc at 65 evenly spaced
/// walk positions. One pass is still [`WALK_FAST_CELLS`] cells; cells move only within this
/// leg.
///
/// A ribbon reads one arc entry per cell (`1/16` of `t` is 31.9 entries), so a handoff little
/// more than a cell wide shows as a step whose size depends on the walk origin. The table
/// spaces the leg by what the eye reads, the composited bed and the vivid rail in equal parts;
/// the other five legs are the identity, and further smoothing would need cells from the
/// neighbouring legs. `the_walk_s_pace_is_the_arc_s_own_perceptual_step` re-derives it from
/// `bed_ink`/`rail_ink`, so an ink change is caught; it is nearly ground-independent (the luma
/// budget scales the inks), so one table serves every theme.
const CROSS_PACE: [u16; CROSS_PACE_KNOTS] = [
    33539, 33818, 34076, 34375, 34679, 35026, 35233, 35530, 35828, 36087, 36381, 36642, 36900,
    37159, 37456, 37760, 38011, 38231, 38537, 38825, 39085, 39343, 39598, 39897, 40189, 40573,
    40831, 41141, 41399, 41696, 41916, 42169, 42470, 42683, 42939, 43158, 43336, 43542, 43795,
    43969, 44174, 44364, 44569, 44782, 44996, 45199, 45337, 45554, 45715, 45896, 46035, 46197,
    46409, 46552, 46712, 46840, 47051, 47224, 47523, 47738, 47952, 48208, 48423, 48553, 48830,
];

/// Knots in [`CROSS_PACE`].
pub const CROSS_PACE_KNOTS: usize = 65;

/// The arc position of a walk position: the `meteor::tri` fold through [`walk_pace`]. Every
/// reader of a walk `t` goes through this, so the caret, bed, rail, hot edge, meteor train
/// and star halos land on the same stop.
#[inline]
#[must_use]
pub fn walk_arc(t: f32) -> f32 {
    walk_pace(clamp01(tri(t)))
}

/// Where a walk position lands on the arc: the identity outside the middle,
/// [`CROSS_PACE`] inside it. Monotone, with `0 ↦ 0`, `1 ↦ 1` and both window
/// edges fixed.
#[inline]
#[must_use]
pub fn walk_pace(x: f32) -> f32 {
    let x = clamp01(x);
    if x <= CROSS_ARC0 || x >= CROSS_ARC1 {
        return x;
    }
    let f = (x - CROSS_ARC0) / (CROSS_ARC1 - CROSS_ARC0) * (CROSS_PACE_KNOTS - 1) as f32;
    let i = (f as usize).min(CROSS_PACE_KNOTS - 2);
    let (a, b) = (f32::from(CROSS_PACE[i]), f32::from(CROSS_PACE[i + 1]));
    (a + (b - a) * (f - i as f32)) / 65535.0
}

/// The classic walk: `t` at `d` cells from where the walk started. `d/16` over
/// the first [`WALK_FAST_CELLS`] cells (a short word shows several stops),
/// then [`WALK_LAY_RATE`] per cell. `d` is the walk's distance: a continuation
/// starts [`Cohort::d0`] cells in, so a wrapped row keeps its pace; the fast
/// leg belongs to a walk's start (cold, or a new line, [`Ribbon::fresh_line`]).
/// Below zero it is the fast line continued; the only knee is at 16.
#[inline]
#[must_use]
pub fn walk_t(d: f32) -> f32 {
    if !d.is_finite() {
        return 0.0;
    }
    if d <= WALK_FAST_CELLS {
        d / WALK_FAST_CELLS
    } else {
        1.0 + (d - WALK_FAST_CELLS) * WALK_LAY_RATE
    }
}

/// A cell's terminal approach to zero, read on its own clock: smoothstep over the last
/// [`EXPIRY_MELT_SHARE`] of life, raised to [`EXPIRY_MELT_GAMMA`]. Exactly `1.0` over the
/// first 70 % of the domain and exactly `0.0` at `u = 1`.
#[inline]
#[must_use]
pub fn expiry_melt(u: f32) -> f32 {
    let taper = smoothstep01((1.0 - clamp01(u)) / EXPIRY_MELT_SHARE);
    if taper >= 1.0 {
        1.0
    } else {
        taper.powf(EXPIRY_MELT_GAMMA)
    }
}

/// The focus ember's level, per stop: the band's common fade `f` (the `spend` curve, or one
/// linear fade under reduced motion) mapped into the stop's own range, from exactly `1.0` at
/// the blur to exactly `floor` when spent, `floor` being the coverage share at which this stop
/// stops being seen ([`seen_floor`]). A common multiplier would take the warm stops under the
/// colour floor while blue is still bright, splitting the band; here no stop crosses its floor
/// while the ember has level, and the ember's end ([`Ribbon::retire`]) takes every cell in one
/// frame. Non-increasing in `f`; a floor above the cell's coverage holds what it has.
#[inline]
#[must_use]
pub fn ember_level(floor: f32, f: f32) -> f32 {
    let floor = floor.max(0.0);
    clamp01(floor + (1.0 - floor) * clamp01(f))
}

/// The one linear fade of the theme (§2.5, §6.11): under reduced motion a
/// mark's alpha runs to zero linearly over the last
/// [`REDUCED_MOTION_FADE_MS`] of its life. `remaining_s` is how long the mark
/// has left; `1.0` until the window opens, exactly `0.0` at the end.
#[inline]
#[must_use]
pub fn reduced_fade(remaining_s: f32) -> f32 {
    clamp01(remaining_s / (REDUCED_MOTION_FADE_MS / 1000.0))
}

/// Seconds until [`expiry_melt`] next takes a mark of `peak` levels down one u8
/// level, `u` of the way through its `life_s`: closed form through the
/// smoothstep's inverse (`x = ½ − sin(asin(1 − 2y)/3)`). Before the melt opens
/// this is its first level drop. `None` once nothing is left to see.
#[must_use]
pub(crate) fn level_step_melt(peak: f32, u: f32, life_s: f32) -> Option<f32> {
    if !(peak.is_finite() && life_s.is_finite() && u.is_finite()) || peak <= 0.0 || life_s <= 0.0 {
        return None;
    }
    let u = clamp01(u);
    let level = (peak * expiry_melt(u)).round();
    if level < 1.0 {
        return None;
    }
    let taper = ((level - 0.5) / peak)
        .clamp(0.0, 1.0)
        .powf(1.0 / EXPIRY_MELT_GAMMA);
    let x = 0.5 - ((1.0 - 2.0 * taper).clamp(-1.0, 1.0).asin() / 3.0).sin();
    let u_next = 1.0 - EXPIRY_MELT_SHARE * x;
    Some(((u_next - u) * life_s).max(0.0))
}

// ===========================================================================
// The producer
// ===========================================================================

/// One boundary's planned sample before it becomes a [`Segment`]:
/// `(spine, up, dn, t, cov)`, in window px / walk units / coverage levels.
type Sample = (f32, f32, f32, f32, f32);

/// One contiguous planned run: a range of [`Ribbon::plan`] plus its columns, so
/// [`Ribbon::band`] answers a cell without a search and [`Ribbon::emit`] can
/// shed the run's tail under budget. A run belongs to one cohort
/// ([`Ribbon::build_runs`]), so one cohort's swoosh never moves a neighbour's
/// boundaries.
#[derive(Clone, Copy, Debug)]
struct Run {
    row: u16,
    col0: u16,
    col1: u16,
    lo: usize,
    hi: usize,
    /// The head cell's column (see [`Ribbon::head_col`]).
    head_col: u16,
    /// Index into [`Ribbon::plan`] of the head boundary: the head cell's right
    /// edge for a left-streaming run, the landing's left edge for a leftward
    /// wake ([`Run::stream_dir`]). The hot edge starts here, and on a capped
    /// frame so does the emitter, so light by the hand is never dropped for a
    /// far tail.
    head: usize,
    /// True when the head cell is real, un-retracting light — the hot edge's
    /// "still wet at the hand" condition (§4.1). A run whose every cell is
    /// retracting (the whole word backspaced) has nothing wet in it.
    wet: bool,
    /// Whether the caret stands in or beside this run — the run the hand is
    /// on is emitted first, so a saturated budget sheds the oldest row and
    /// never the head (§7.3: one key, one cell, one light).
    at_caret: bool,
    /// The newest typing cell's birth, the emit order's second key.
    born: Instant,
    /// The direction the stream runs from the head: `−1` toward lower columns
    /// (typed runs and rightward wakes) or `+1` toward higher (a leftward wake,
    /// see [`Ribbon::head_col`]). The hot edge reaches this way only, behind
    /// the hand; [`Run::head`] is the `lo` boundary when `+1`.
    stream_dir: i8,
}

/// The attach's level this frame, set by [`Ribbon::plan_run`] at the head
/// boundary of the run under the caret: the head boundary's coverage (`base`),
/// the head cell's uniform attack (`uniform`, [`BIRTH_EDGE_FLOOR`] to 1 over
/// the edge-in) and the wipe's probe ([`Ribbon::wipe_of`]).
/// [`AttachFloor::level`] at `dist` across the caret cell is the light the
/// attach puts there: the caret cell's own segments when nothing is laid
/// under the caret, and the floor under a leaving cell's light when one is (a
/// Backspaced cell draining, or a neighbour retiring by content), so the
/// block stands on the band while the cell spends and its retirement changes
/// no byte.
#[derive(Clone, Copy, Debug)]
struct AttachFloor {
    row: u16,
    /// The caret cell.
    col: u16,
    base: f32,
    uniform: f32,
    probe: Cell,
}

impl AttachFloor {
    /// The attach's light at `dist` across the caret cell.
    #[inline]
    fn level(&self, rib: &Ribbon, ctx: &Ctx<'_>, dist: f32) -> f32 {
        self.base * self.uniform * rib.wipe_of(ctx, &self.probe, dist)
    }
}

/// A short caret retreat proved to be the exact parked echo of a wrap whose
/// old row's text moved. The host's held park supplies the pair; the content
/// follow supplies the proof. One park, one use.
#[derive(Clone, Copy, Debug)]
struct ShortReanchorProof {
    from: (u16, u16),
    to: (u16, u16),
    at: Instant,
}

/// Sampled geometry of the next matching typed rewrap, including the last Space in
/// its separately paid suffix when the key sequence proved one.
#[derive(Clone, Copy, Debug)]
struct RewrapStart {
    from: (u16, u16),
    to: (u16, u16),
    start: u16,
    /// An exact content proof may override the legacy moved-word measure;
    /// zero means the old word stayed on its source row.
    width: Option<u16>,
    space: Option<u16>,
}

/// The ribbon producer: the laid cells, their cohorts, this frame's plan, and
/// the field index every other producer reads. The pool, plan and index are
/// private; tests and other producers read them through [`Ribbon::cells`],
/// [`Ribbon::cohorts`], [`Ribbon::plan_segments`] and `Ribbon::field`.
#[derive(Clone, Debug, Default)]
pub struct Ribbon {
    /// The live cells, resident and reused (§18: no allocation per frame).
    cells: Vec<Cell>,
    /// The live cohorts.
    cohorts: Vec<Cohort>,
    /// This frame's boundary samples, rebuilt by [`Ribbon::plan`] and consumed
    /// by [`Ribbon::emit`]. Resident scratch.
    plan: Vec<Segment>,
    /// This frame's `col → t` index.
    index: FieldIndex,
    /// Vertices per cell this frame planned at — [`SLABS_PER_CELL`] or fewer
    /// under budget pressure (see [`Ribbon::slabs_per_cell`]). Every index
    /// into [`Ribbon::plan`] is stated in it.
    slabs: usize,
    /// The next cohort id to mint.
    next_cohort: u32,
    /// The caret cell as last observed — the anchor `field()` resolves
    /// against (seam point 4).
    caret: Option<(u16, u16)>,
    /// This frame's runs into [`Ribbon::plan`], in emit order (the run at the
    /// caret first, then newest first). Resident scratch.
    runs: Vec<Run>,
    /// `(row, col, cell index)` sort scratch — the one sort per frame the
    /// index and the geometry share (§18).
    sorted: Vec<(u16, u16, u32)>,
    /// `RibbonVertex` scratch, taken and returned by `mem::take` so the emit
    /// path allocates nothing.
    verts: Vec<RibbonVertex>,
    /// The cohorts one `release_cells` call names, each once: resident
    /// scratch, cleared per call (§18), since every torn read releases on the
    /// frame path.
    released_cohorts: Vec<u32>,
    /// The bed's ink table for the live theme.
    ink: BedInkLut,
    /// The theme ground the last planned frame composited over, stamped
    /// beside [`Ribbon::ink`]'s sync so `Ribbon::ink_segments` can answer
    /// what a boundary actually reads as without a `Ctx`.
    ground: u32,
    /// How many of the quads the last emit appended to `under` composite to
    /// something the glass census counts as band ink ([`reads_as_ink`]),
    /// measured on the frame's own pixels (the strip and rail ride with the
    /// bed, so [`Segment::cov`] alone under-reports). `trail status` reports
    /// it as `ribbon_drawn=`.
    ink_quads: usize,
    /// Set while the window is unfocused: the ribbon embers out over
    /// [`FOCUS_EMBER_S`] on `spend` and nothing sounds (§8.2). Cleared by
    /// [`Ribbon::retire`] with the pool once the ember has burnt out, and by
    /// [`Ribbon::regain_focus`] (into a rearm if still burning).
    ember_at: Option<Instant>,
    /// Focus regained while the ember was burning: `(when, fade)`, the ember's
    /// common fade then, which [`ember_level`] maps per stop. The light returns
    /// through `edge-in` from that level rather than snapping to full.
    rearm: Option<(Instant, f32)>,
    /// The reduced-motion posture of the last frame planned, cached for the
    /// cadence law (seam point 9 is asked with a clock and nothing else).
    reduced: bool,
    /// The focused pane's `(first column, width)` — the edges [`Ribbon::lay`]
    /// wraps a fold at — or `None` for the whole grid. Handed down by the
    /// engine every tick ([`super::Engine::set_pane`]).
    pane: Option<(u16, u16)>,
    /// The focused pane's `(first row, rows)`, `None` for the whole grid; the
    /// companion to [`Ribbon::pane`]'s columns. The comet's lower lobe and the
    /// rail reach into the leading below the row, which exists only below the
    /// pane's last row, not the grid's, so a stacked split's divider stays
    /// clean.
    pane_rows: Option<(u16, u16)>,
    /// The attach of the last frame planned: the `(row, col)` of the caret cell
    /// the band continued under ([`Ribbon::plan_run`]), or `None`. A typed cell
    /// laid there inherits its light ([`Ribbon::lay_cell`]), so the key moves no
    /// byte down and the wipe belongs to the next attach. [`Ribbon::build`]
    /// restarts the retract of a leaving cell the attach floored that the
    /// caret has since left, so it drains from the level shown under the block.
    attach: Option<(u16, u16)>,
    /// The attach's level this frame ([`AttachFloor`]), or `None` while no
    /// run stands under the caret. Set at the head boundary as the runs are
    /// planned, read by the caret cell's slabs in the same run or the
    /// neighbour run to its right — which is planned after it (`sorted` is
    /// row-major) — and by the attach's own segments.
    attach_floor: Option<AttachFloor>,
    /// The cell height of the last frame planned, px.
    cell_h: f32,
    /// The wedge's full travel in px — `(dn_ch − DN_FLOOR_CH)·ch` of the
    /// last frame's profile — the distance the mark's lower edge closes as
    /// the spine releases ([`Ribbon::settle_step_s`]).
    wedge_px: f32,
    /// The band already followed its text this tick: set by
    /// [`Ribbon::translate_scroll`] and its band twin, cleared at the end of
    /// [`Ribbon::plan`]. A bottom-pinned composer growing up scrolls the screen
    /// in the same repaint that re-lays the box, so the repaint's backward
    /// caret a tick later is not a stale line to drain ([`Ribbon::re_anchor`]).
    scroll_followed: bool,
    /// The cells the follow pass placed this tick, by their new identity `(row,
    /// col, born)`, pushed by [`Ribbon::translate_run`] and cleared at the end
    /// of [`Ribbon::plan`]. The follow pass places cells in the batch's new
    /// layout, while the tick's replayed erases are addressed in the old one,
    /// so an erase's suffix retract ([`Ribbon::retract_suffix`]) leaves these
    /// cells alone.
    followed_now: Vec<(u16, u16, Instant)>,
    /// Exact held park whose source row's text was proved to have followed
    /// a composer wrap. Installed only when that park's licensed flush
    /// queues its move, and gone after the next replay even if unused.
    short_reanchor: Option<ShortReanchorProof>,
    /// The edits the shift pass read this tick, as `(row, lo, hi, dc)` of each
    /// [`super::witness::ShiftRun`] an insert moved (`dc > 0`), pushed by
    /// [`Ribbon::shift_run`] and cleared with [`Ribbon::followed_now`]. The new
    /// text is `lo..lo + dc` and the pushed tail stands on `lo + dc..=hi + dc`.
    /// A typed lay replayed inside the moved tail is laid at the insert instead
    /// ([`Ribbon::insert_landing`]): a composer reprinting the line reports the
    /// echo at the line's end.
    shifted_now: Vec<(u16, u16, u16, i16)>,
    /// Erases whose retreat the ribbon has not yet seen. An `Erase` replayed
    /// against a caret whose echo lands on a later tick finds nothing to
    /// retract, so the count is spent by the next typed-licensed same-row
    /// retreat, which retracts from its landing; a typed lay clears it.
    pending_erase: u16,
    /// The pen: `(row, col)` where the ribbon's own last hand event left the caret, what
    /// [`Ribbon::typed_landing`] calls "where the hand is". Not [`Ribbon::caret`], the app's
    /// mirror, which reads `(0, 0)` before any caret is seen and is planted by every `Move`, a
    /// repaint's parking pass included.
    ///
    /// The pen moves only on the hand: a typed lay (to its landing), a Backspace or kill (to
    /// the caret it was replayed against), and a nav/return/synthetic move; never on an echoed
    /// move. It rides a scroll and a band move as `last_space` does, and is dropped by
    /// [`Ribbon::curtain`] and [`Ribbon::reset`]. `None` until the hand has acted.
    pen: Option<(u16, u16)>,
    /// The cell of the last typed Space, `(row, col)` — what tells a
    /// re-anchor how long the word an app moved to the next row was.
    last_space: Option<(u16, u16)>,
    /// Where the hand arrived: `(row, col)` of its last deliberate relocation
    /// (the non-echo arm of `Event::Move`, as for [`Ribbon::pen`]). The typed
    /// run on that row begins there as surely as after a typed Space, so the
    /// word a re-wrap moved is measured from the later of the two
    /// ([`Ribbon::moved_word_len`]). Cleared with `last_space` by the moves that
    /// consume the measure; ridden, dropped and reset as the pen is.
    arrived: Option<(u16, u16)>,
    /// The `last_space` a newer Space replaced, kept so a Space held right
    /// after a wrap key (remembered at the stale pre-wrap mirror) cannot erase
    /// where the moved word began ([`Ribbon::word_space`]). Written, cleared
    /// and carried wherever `last_space` is.
    prev_space: Option<(u16, u16)>,
    /// A re-anchor's moved word, waiting for the landing that reveals where
    /// it went (see [`Relocate`]).
    relocate: Option<Relocate>,
    /// The seam's soft-wrap verdict for the `Move` being replayed — set only
    /// for the length of that one replay ([`Ribbon::soft_wrap_move`]) and
    /// read by [`Ribbon::leave_row`].
    soft_wrap: Option<SoftWrap>,
    /// A soft-wrapped word waiting for the key that reflows it
    /// ([`Ribbon::settle_carry`]), with the wrap's clock.
    carry: Option<(SoftWrap, Instant)>,
    /// The seam's verdict that the soft-wrapped word `carry` names left its
    /// row on the glass (`CursorGlow::carried_word_left`), for the `Move`
    /// being replayed — set only for the length of that one replay
    /// ([`Ribbon::reflow_move`]) and read by [`Ribbon::settle_carry`].
    reflow: Option<SoftWrap>,
    /// The seam's lift verdict for the `Move` being replayed — set only for
    /// the length of that one replay ([`Ribbon::lift_move`]) and read by the
    /// `Move` arm's same-row retreat.
    lift: Option<Lift>,
    /// Where the relaid text starts, as the row shows it, for the typed
    /// rewrap `from → to` about to be replayed ([`Ribbon::note_rewrap_start`]).
    /// Taken by that move's [`Ribbon::re_anchor`] or [`Ribbon::leave_row`].
    rewrap_start: Option<RewrapStart>,
    /// The revived run's own walk ([`Revive`]): set only for the one `Sweep`
    /// that re-lays a retired typed run ([`Ribbon::revive_sweep`]) and read by
    /// [`Ribbon::join_cohort`] and [`Ribbon::lay_cell`].
    revive: Option<Revive>,
    /// Where the walk of a split run went ([`SplitOff`]): the last run the
    /// follow pass split carrying its text off the caret's row, read by
    /// [`Ribbon::relay_origin`] once the moved word's cells are gone and spent
    /// by that relay.
    split_off: Option<SplitOff>,
    /// The walk a finished swoosh left behind, so a rebirth inside [`CHAIN_GAP_MAX`] continues
    /// the arc (its colour and pace, or its colour alone after a new line,
    /// [`Ribbon::fresh_line`]) instead of re-anchoring at red once `retire` has dropped the
    /// cohort with its cells.
    last_walk: Option<LastWalk>,
    /// The hand started a new line: when, and the row it left ([`FreshLine`]), or `None`. The
    /// walk's distance travels with its colour along one input ([`Cohort::d0`]) and restarts
    /// where the hand starts a new line, so a meteor landing on the new line
    /// (`meteor::landing_field`'s `d/16`) matches the band.
    ///
    /// Set by [`Event::Return`], a line kill ([`super::KillScope::Line`]), [`Ribbon::curtain`],
    /// a Return's or composer newline's own move ([`Licence::Return`], whatever its rows: a
    /// bottom-anchored box homes the caret on the same row), and a row change the hand
    /// navigated ([`Licence::Nav`]); not by a same-row hop, an echoed move, a
    /// [`Licence::Synthetic`] move or a program's jump. While set, the next typed cell that
    /// mints a cohort on either of [`Ribbon::join_cohort`]'s continuation arms mints on
    /// [`WalkPos::restarted`]; the first typed cell the gate takes ([`FreshLine::takes`])
    /// clears it. The composer's relay ([`Ribbon::relay_word`]) is never gated.
    ///
    /// An instant, not a flag, and the earliest one: glyphs are laid on their key's clock but
    /// a row change reaches the ribbon a repaint later, so the old line's late glyph (`…x⏎`
    /// with `x` held for its echo) lands on its own row and neither restarts nor spends the
    /// gate.
    fresh_line: Option<FreshLine>,
    /// The seam on the new line: when the input continues onto a new row (a fold, a composer
    /// re-wrap, the follow pass carrying text off the caret's row; [`Ribbon::arm_seam`]), the
    /// hot edge along the new line's tail holds a floor of [`SEAM_LEVEL`] back to the hand's
    /// first column on the row, spent over [`FLOW_TOTAL_S`] as the row above drifts out
    /// ([`Ribbon::emit_hot_edge`]). Never on a new line the hand started, a jump or a
    /// program's move; cleared by a curtain, a reset, a focus loss, an abandon of its row and a
    /// run carried back onto it; carried with its row by a scroll or band move.
    seam: Option<Seam>,
    /// The hand's tempo, ms: the inter-onset EMA the melody runs
    /// ([`super::timing::ioi_next`]) on the same keys, so the ribbon's grace is
    /// [`super::timing::phrase_rest_ms`] of the number the song rests on.
    /// `0.0` before the first key reads as the default tempo
    /// ([`Ribbon::rest_s`]).
    ioi_ms: f32,
    /// The clock of the last key that held a phrase — a typed key, an erase
    /// or a kill: the rest is measured from it, exactly as the melody's
    /// `last_key_ms` is any touch.
    last_key_at: Option<Instant>,
    /// The open phrase (§2.3), or `None` between phrases: after a rest has passed since
    /// [`Ribbon::last_key_at`], a Return or a line kill. The next typed lay opens the next one.
    phrase_open: Option<u32>,
    /// The next phrase id to mint; `0` is never minted (it is "no phrase",
    /// a wake cohort's).
    next_phrase: u32,
    /// When the curtain in progress fell, or `None`. The column each cohort is
    /// drawn toward is frozen on the cohort ([`Cohort::retract_col`],
    /// [`Cohort::drain_right_first`]) from the band's own hand, not
    /// [`Ribbon::caret`] (see [`Ribbon::curtain`]).
    curtain: Option<Instant>,
    /// The hand's floor on its row: `(row, col)`, the leftmost column the hand has held there
    /// (a glyph it typed, a deliberate relocation, or the caret an erase was replayed against).
    /// The swoosh's reach extends the band leftward with no other bound, so without it a
    /// prompt's `$` could light up. Also decides whether a same-row re-anchor's relay is a word
    /// the app moved or a shell's `^A` ([`Ribbon::re_anchor`]). Not applied to
    /// [`Ribbon::sweep`] or the relay, which lay the hand's own glyphs. One row: a row change
    /// replaces it.
    hand_floor: Option<(u16, u16)>,
}

/// A re-anchor's moved word. A TUI composer (Claude Code's Ink box, Codex) re-wraps its box
/// per keystroke: the row's last word moves down to the next visual row and the caret lands
/// after it, which the seam licenses as one typed re-anchor `Move`. The word's length comes
/// from the last typed Space (`w` cells before the origin), so it is relaid at
/// `[landing − 1 − w, landing − 1)`, the wrap key's glyph at `landing − 1`. When Ink lands the
/// caret at the box's inset first (and at the word's end in a move the seam refuses), the
/// relocation waits and the next same-row forward typed move on the row pays it.
#[derive(Clone, Copy, Debug)]
struct Relocate {
    /// The row the word moved to.
    row: u16,
    /// Cells the moved word has (without the wrap key's own glyph).
    w: u16,
    /// When the re-anchor was seen; a relocation older than
    /// [`RELOCATE_PATIENCE_S`] buys nothing.
    at: Instant,
    /// The walk the word's first letter had on the row it came off — its
    /// `t` and its distance ([`Ribbon::relay_origin`]) — read when the
    /// re-anchor was seen: the old cells may be gone by the time the
    /// landing arrives.
    walk: Option<WalkPos>,
}

/// How long a [`Relocate`] waits for the caret to reveal the moved word.
pub const RELOCATE_PATIENCE_S: f32 = 2.0;

/// The soft-wrapped caret (`CursorGlow::soft_wrapped_caret`): the seam's
/// verdict that a typed `Move` `origin → landing`, one row down, is a key whose
/// glyph was drawn at `origin` while the caret alone wrapped to the
/// continuation row's indent (Claude Code's insert at the fold). The next key
/// reflows the word the glyph ends, `word_col0..=origin.1`, down to the
/// indent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftWrap {
    /// Where the key's glyph stands, and where the caret was.
    pub origin: (u16, u16),
    /// Where the caret alone wrapped to.
    pub landing: (u16, u16),
    /// The first column of the word the key ends, on the origin row.
    pub word_col0: u16,
}

/// The lifted word (`CursorGlow::lifted_word`): the seam's verdict that a
/// typed `Move` `origin → landing`, backward on one row, is the composer
/// re-wrapping the word `landing.1..origin.1` up onto the end of the row above,
/// where it now stands from `dst` (a Space typed after a word glued to the
/// following text). The caret stays before the unmoved text, at the word's
/// old first column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lift {
    /// The caret before the key: one past the word's last column.
    pub origin: (u16, u16),
    /// The caret after it: the word's old first column.
    pub landing: (u16, u16),
    /// Where the word's first glyph stands now, on the row above.
    pub dst: (u16, u16),
}

/// The revive lays a line back on its own walk (`RAINBOW-KITTY-V2.md` §43).
/// The seam's typed-run revive (`CursorGlow::hand_v2_typed_sweep`) re-lays a
/// typed run whose cells the swoosh removed, as one [`super::Event::Sweep`]
/// `col0..col1` on `row`, carrying the run's walk origin as read off its cells
/// ([`RunOrigin`]). A cell of the re-lay that would mint a cohort mints on it
/// ([`Ribbon::join_cohort`]), so every revived column takes back its stop
/// (§29.4) regardless of `last_walk` or the freshest cohort elsewhere. A cell
/// that joins a live band is laid as usual.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Revive {
    /// The re-laid row.
    pub row: u16,
    /// The re-lay's first column.
    pub col0: u16,
    /// One past its last (exclusive): the key's own first column.
    pub col1: u16,
    /// The walk the run's cells had.
    pub origin: RunOrigin,
}

impl Revive {
    /// Whether `(row, col)` is a column this re-lay lays.
    #[inline]
    fn covers(&self, row: u16, col: u16) -> bool {
        row == self.row && (self.col0..self.col1).contains(&col)
    }
}

/// A typed run's walk origin, kept outside the ribbon ([`Revive`]): the three
/// numbers of a cohort's walk ([`Cohort::anchor_col`], [`Cohort::t0`],
/// [`Cohort::d0`]), read by the seam while the cells stand. A cohort minted on
/// it reads [`Cohort::t_at`] bit for bit as the original.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunOrigin {
    /// The walk's anchor column.
    pub anchor_col: u16,
    /// The walk's `t` there.
    pub t0: f32,
    /// The walk's distance there.
    pub d0: f32,
}

impl RunOrigin {
    /// `coh`'s walk.
    #[inline]
    #[must_use]
    pub fn of(coh: &Cohort) -> Self {
        Self {
            anchor_col: coh.anchor_col,
            t0: coh.t0,
            d0: coh.d0,
        }
    }

    /// The pair a cohort is minted on at [`RunOrigin::anchor_col`].
    #[inline]
    fn walk(self) -> WalkPos {
        WalkPos {
            t: self.t0,
            d: self.d0,
        }
    }
}

/// The run the follow pass split, and where its walk went. A composer's wrap
/// carries its line a row up minus the word it moves: [`Ribbon::translate_run`]
/// mints a copy of the line's cohort on the row the text went to (same
/// anchor, colour and distance) and leaves the moved word's old cells and the
/// eaten Space in the old cohort. The relay reads the moved word's walk off
/// those cells ([`Ribbon::relay_origin`]), but they may already have melted
/// ([`RETIRE_MELT_S`]) when the relay runs (up to 0.25 s later, at the held
/// park's flush). This keeps the walk for the flow's [`FLOW_TOTAL_S`] so the
/// relaid word continues from its own first letter's stop.
#[derive(Clone, Copy, Debug)]
struct SplitOff {
    /// The copy's id ([`Cohort::id`]).
    copy: u32,
    /// The rows it was carried: it was split off row `copy.row − dr`, which
    /// rides a scroll with the copy's own row.
    dr: i16,
    /// The columns the split left behind on that row, `col0..col1` — the
    /// old cohort's shrunk bounds.
    col0: u16,
    col1: u16,
}

/// The walk a retired cohort left on its row — see `Ribbon::last_walk`.
#[derive(Clone, Copy, Debug)]
struct LastWalk {
    /// The walk one past the retired cohort's rightmost column
    /// (`Cohort::walk_at(col1)`): where a rebirth continues it, colour and pace.
    end: WalkPos,
    alive_at: Instant,
}

/// The seam ([`Ribbon::seam`]): the row a continuation of the same input
/// carried the hand onto, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seam {
    /// The new line's row.
    row: u16,
    /// When the fold (or the follow pass) handed the input to it.
    at: Instant,
}

/// The new line's gate while armed ([`Ribbon::fresh_line`]): when the hand
/// started a new line, and the row it left. Armed by
/// [`Ribbon::arm_fresh_line`], read by [`FreshLine::takes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FreshLine {
    /// The earliest instant the hand started a new line since the gate was last
    /// spent: re-arming keeps the older, so a Return's cross-row move (observed
    /// a repaint after the key armed the gate) never dates it past a key
    /// pressed in between.
    since: Instant,
    /// A composer newline also stamps a typed hint at the same key instant.
    /// That stamp describes the chord, not the first character on its new
    /// line, so its replay cannot spend this gate even if the box repainted
    /// before the next frame. `None` for Return, navigation and line kills.
    chord_at: Option<Instant>,
    /// Until the composer actually moves the caret to its new inset, typed
    /// events replay at the old caret. On a bottom-anchored repaint that old
    /// row is now empty, so a type-ahead key would mint a phantom there and
    /// spend the gate before its real echo reaches the new line.
    awaiting_move: bool,
    /// The row the hand left, from the first cross-row move while armed.
    /// `None` for a gate armed on the row itself (a Return before its move, a
    /// line kill) or in a new coordinate space (a curtain). It rides a scroll
    /// and band move with its row, and is dropped (not clamped) when carried
    /// off, leaving the instant alone.
    left_row: Option<u16>,
}

impl FreshLine {
    /// Whether a typed cell laid on `row` on its key's clock `at` is the
    /// new line's: a key pressed at or after the new line started, or a
    /// glyph laid on any row but the one the hand left — the old line's
    /// late glyph lands on the row it was typed on.
    #[inline]
    fn takes(self, row: u16, at: Instant) -> bool {
        !self.awaiting_move
            && self.chord_at.is_none_or(|chord| at != chord)
            && (at >= self.since || self.left_row.is_some_and(|left| left != row))
    }
}

impl Ribbon {
    /// A ribbon with nothing laid.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The host has queued the licensed move of the exact held park whose
    /// source row's content followed. This one-shot proof lives for its
    /// replay frame only, so a later park cannot inherit it. The original
    /// park clock distinguishes its queued move from any other exact pair
    /// already buffered on that frame.
    #[inline]
    pub(crate) fn prove_short_reanchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        debug_assert!(from.0 == to.0 && (1..=2).contains(&from.1.saturating_sub(to.1)));
        self.short_reanchor = Some(ShortReanchorProof { from, to, at });
    }

    /// The live cells, in pool (append) order. Read-only.
    #[inline]
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The live cohorts. Read-only.
    #[inline]
    #[must_use]
    pub fn cohorts(&self) -> &[Cohort] {
        &self.cohorts
    }

    /// The instant `cell`'s expiry is measured from — see [`live_since`].
    #[must_use]
    #[cfg(test)]
    pub fn live_since(&self, cell: &Cell) -> Instant {
        live_since(&self.cohorts, cell)
    }

    /// The callback-relative swoosh as a fault: `RainbowExitSampling`'s
    /// `Buggy = 1`, for that model's negative control. Every cohort whose
    /// swoosh ran out while no frame observed it has its clock rewound so the
    /// swoosh starts at `now`, as a lifecycle advanced per callback rather than
    /// by time would do.
    #[cfg(test)]
    pub fn restart_unobserved_swooshes(&mut self, now: Instant) {
        for coh in &mut self.cohorts {
            let unobserved = matches!(coh.phase, Phase::Laying | Phase::Grace);
            if !unobserved || coh.phase_at(coh.idle_s(now)).is_some() {
                continue;
            }
            let lead_s = if coh.flow.is_some() {
                1e-3
            } else if coh.grace_s.is_finite() {
                coh.grace_s.max(PHRASE_REST_MIN_S)
            } else {
                PHRASE_REST_MIN_S
            };
            let restart = now
                .checked_sub(std::time::Duration::from_secs_f32(lead_s))
                .unwrap_or(now);
            coh.alive_at = restart;
            if coh.flow.is_some() {
                coh.flow = Some(restart);
            }
        }
    }

    /// This frame's planned boundary samples, valid after [`Ribbon::plan`].
    /// Read-only.
    #[inline]
    #[must_use]
    pub fn plan_segments(&self) -> &[Segment] {
        &self.plan
    }

    /// The frame's vertex density: how many [`Ribbon::plan_segments`] each
    /// cell of a run spans this frame, `1..=`[`SLABS_PER_CELL`]. Boundary `b`
    /// of a run starting at `col0` sits at `run.lo + (b − col0) ·
    /// slabs_per_cell()`. Chosen so `live cells × device rows × slabs` fits
    /// [`RIBBON_QUAD_BUDGET`] (§18); past one slab the budget sheds the oldest
    /// run's tail ([`Ribbon::emit`]). Valid after [`Ribbon::plan`].
    #[inline]
    #[must_use]
    pub fn slabs_per_cell(&self) -> usize {
        self.slabs.clamp(1, SLABS_PER_CELL)
    }

    /// The caret cell as last observed, or `None` before any.
    #[must_use]
    #[cfg(test)]
    pub fn caret(&self) -> Option<(u16, u16)> {
        self.caret
    }

    /// The two profiles (§4): the tall/underline fork as data. `dn_ch` is the
    /// reach below the spine at full momentum under the hand: the comet's
    /// [`COMET_DN_HAND_CH`], or [`DN_TOP_CH`] under the `… flat` spelling. The
    /// spellings differ only above the spine.
    #[inline]
    #[must_use]
    pub fn body_profile(cfg: &Config) -> BodyProfile {
        let dn_ch = if Self::comet(cfg) {
            COMET_DN_HAND_CH
        } else {
            DN_TOP_CH
        };
        if cfg.ribbon_tall {
            BodyProfile {
                up_ch: TALL_UP_CH,
                dn_ch,
                core_up_ch: TALL_CORE_CH,
                shoulder: SHOULDER_TALL,
            }
        } else {
            BodyProfile {
                up_ch: UNDERLINE_UP_CH,
                dn_ch,
                core_up_ch: LEAD_CH,
                shoulder: SHOULDER_UNDERLINE,
            }
        }
    }

    /// Whether the comet body and the from-the-hand attack apply
    /// (`RAINBOW-KITTY-V2.md` §30); the `… flat` spelling
    /// ([`Config::ribbon_flat`]) restores the flat body byte for byte.
    #[inline]
    #[must_use]
    pub fn comet(cfg: &Config) -> bool {
        !cfg.ribbon_flat
    }

    /// Whether the vivid rail is drawn: the comet body on a dark ground. The
    /// light fork composites source-over ink under the shared legibility
    /// ceiling (L6) and keeps the bed alone below the baseline.
    #[inline]
    #[must_use]
    pub fn rail_lit(cfg: &Config) -> bool {
        Self::comet(cfg) && cfg.dark_theme
    }

    // -- ingest ------------------------------------------------------------

    /// Ingest one engine event at its own edge, before this frame's plan.
    ///
    /// * [`Event::Typed`] lays its cells at the hand ([`Ribbon::typed_landing`]).
    /// * [`Event::Erase`] retracts the row's suffix from the caret, so the lit run stays
    ///   contiguous behind it; [`Event::Kill`] drains the same suffix, at least `cells` wide,
    ///   farthest first at `12·n + 240` ms.
    /// * [`Event::Move`] moves [`Ribbon::caret`]. An echoed move is inert but for the hand's
    ///   row change ([`Ribbon::leave_row`]), an erase's retreat and a composer's re-anchor. A
    ///   keyed same-row move over the hand's lit run is a scrub ([`Ribbon::scrub`]); a keyed
    ///   same-row jump of [`JUMP_MIN_CELLS`] or more lays its wake ([`Ribbon::wake`]) and
    ///   abandons every other cohort; a shorter arrow hop lays a short wake and abandons
    ///   nothing; any other jump or row change abandons without a wake.
    /// * [`Event::Focus`] takes the 300 ms ember, and on regain inside it the `edge-in` rearm.
    ///
    /// Every event applies against `ctx.caret`, the caret for this tick; a host buffering two
    /// typed echoes into one tick must coalesce them into one `Typed { cells: 2 }`.
    pub fn on_event(&mut self, ev: &super::Event, at: Instant, ctx: &Ctx<'_>) {
        // Under a curtain nothing is laid (§2.8): the cells on glass belong to
        // an ended coordinate space. Only the caret mirror is kept.
        if self.curtain.is_some() {
            if let Event::Move { to, .. } = *ev {
                self.caret = Some(to);
            }
            return;
        }
        // A key that explicitly edits or relocates the hand supersedes an
        // unobserved composer home. Key-time cancellation also arrives as a
        // separate event for a navigation/kill whose move or erase is swallowed.
        // Echoes, including type-ahead, must leave the composer gate intact.
        if matches!(
            *ev,
            Event::CancelComposerNewline
                | Event::Erase
                | Event::Kill { .. }
                | Event::Return
                | Event::Move {
                    licence: Licence::Nav | Licence::Synthetic,
                    ..
                }
        ) && self.fresh_line.is_some_and(|gate| gate.awaiting_move)
        {
            self.fresh_line = None;
        }
        // Once the hand explicitly edits or leaves a released mark, a later
        // matching redraw cannot undo that decision. This changes no cell or
        // lifetime; it only revokes the content-release restoration token.
        let revoke = match *ev {
            Event::Erase | Event::Kill { .. } | Event::Return | Event::ComposerNewline => {
                Some(ctx.caret.0)
            }
            Event::Rewrite { row, .. } => Some(row),
            Event::Move {
                from,
                licence: Licence::Nav | Licence::Return | Licence::Synthetic,
                ..
            } => Some(from.0),
            _ => None,
        };
        if let Some(row) = revoke {
            for cohort in &mut self.cohorts {
                if cohort.row == row {
                    cohort.revoke_releases(at);
                }
            }
        }
        match *ev {
            Event::Typed { cells, class, .. } => {
                // A key typed while the focus ember burns (e.g. an agent typing
                // into an unfocused window) clears the ember and comes back up
                // from the current level through `edge-in`, as a focus regain
                // does; otherwise cells laid under it would be born dark.
                if self.ember_at.is_some() {
                    self.regain_focus(at);
                }
                // The tempo and phrase move before anything is laid, so the lay
                // reads the grace this key priced.
                self.note_key(at);
                self.pending_erase = 0;
                // A key lays where the hand is, not where a repaint parked the
                // cursor ([`Ribbon::typed_landing`]).
                let landing = self.typed_landing(
                    ctx.caret,
                    cells,
                    u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX),
                );
                let landing = self.insert_landing(landing, cells);
                self.pen = Some(landing);
                let ctx = &Ctx {
                    caret: landing,
                    ..*ctx
                };
                if class == TypedClass::Space {
                    let (row, col_end) = landing;
                    if let Some(c) = col_end.checked_sub(1) {
                        self.note_space((row, c));
                    }
                }
                self.lay(cells, at, ctx);
            }
            // An erase or a kill holds the phrase (§2.3): the open phrase's
            // cohorts on the caret's row (`ribbon_row_hold_model`'s
            // `OnlyOwnerRenews` row clause plus the phrase clause). Both arms
            // go through [`Ribbon::erase`]; a Backspace-shaped kill retracts
            // exactly what a Backspace does.
            Event::Erase => {
                // A Backspace is the hand ([`Ribbon::pen`]): the caret it is
                // replayed against is where the next key is brought back to.
                self.pen = Some(ctx.caret);
                self.touch_hand(ctx.caret.0, ctx.caret.1);
                self.erase(ctx.caret, 1, RETRACT_FADE_S, at);
            }
            Event::Kill { cells, scope } => {
                self.pen = Some(ctx.caret);
                self.erase(ctx.caret, cells.max(1), kill_span_s(cells), at);
                // A line kill ends the phrase, as a Return does: what is
                // typed next is a new line — on a walk that starts again
                // from the colour it had ([`Ribbon::fresh_line`]).
                if scope == super::KillScope::Line {
                    self.phrase_open = None;
                    self.arm_fresh_line(at, None);
                    self.mark_line_killed(ctx.caret.0);
                }
            }
            Event::Move {
                from, to, licence, ..
            } => {
                self.caret = Some(to);
                // A deliberate relocation moves the pen; an echoed move does not
                // ([`Ribbon::pen`]), since a repaint's parking pass arrives under
                // an echo licence.
                if !licence.is_echo() {
                    self.pen = Some(to);
                    self.arrived = Some(to);
                    // ...and with it the hand's floor on the row it is on
                    // ([`Ribbon::hand_floor`]): an arrow or a ^A is the hand reaching a
                    // column, so it frees that column for the leftward reach a repaint
                    // never may.
                    self.touch_hand(to.0, to.1);
                }
                // The hand started a new line ([`Ribbon::fresh_line`]): a `Return`-licensed
                // move whatever its rows, or a navigation to another row. `at` is when the
                // host observed it; the gate keeps any earlier instant ([`FreshLine`]).
                let same_row = to.0 == from.0;
                if licence == Licence::Return || (!same_row && licence == Licence::Nav) {
                    self.arm_fresh_line(at, (!same_row).then_some(from.0));
                    if licence == Licence::Return
                        && let Some(gate) = &mut self.fresh_line
                    {
                        gate.awaiting_move = false;
                    }
                }
                if licence.is_echo() {
                    // An echo's motion is inert (its key or sweep laid the cells) except
                    // the hand leaving the row, an erase's retreat, and a re-anchor.
                    if !same_row {
                        // A repaint's home is not a gesture: only a row change the hand
                        // could have made ([`Ribbon::hand_changed_row`]) leaves the row;
                        // anything else under an echo licence moves only the caret mirror.
                        if Self::hand_changed_row(from, to) {
                            self.leave_row(from, to, at, licence);
                        }
                    } else if to.1 < from.1 {
                        if let Some(lift) =
                            self.lift.filter(|l| l.origin == from && l.landing == to)
                        {
                            // The word went up with its text
                            // ([`Ribbon::lift_word`]).
                            self.lift_word(lift, at);
                        } else if self.pending_erase > 0 {
                            // The erase's retreat: the erased cells are those at
                            // and past the landing, whichever tick the `Erase`
                            // was replayed on.
                            let erased = from.1 - to.1;
                            self.retract_suffix(to.0, to.1, erased, at, RETRACT_FADE_S);
                            // Later erases may already be queued while this frame
                            // observes an earlier key's echo. Only this retreat
                            // pays down the outstanding erase count.
                            self.pending_erase = self.pending_erase.saturating_sub(erased);
                        } else if licence == Licence::Typed
                            && !self.scroll_followed
                            && (from.1 - to.1 >= RE_ANCHOR_MIN_CELLS
                                || self.short_reanchor.is_some_and(|proof| {
                                    proof.from == from && proof.to == to && proof.at == at
                                })
                                || self.rewrap_start.is_some_and(|proof| {
                                    proof.from == from && proof.to == to && proof.width.is_some()
                                }))
                        {
                            // Below the host's three-cell floor only the
                            // old row's proved content move can distinguish
                            // a narrow wrap from a false one-cell park.
                            self.re_anchor(from, to, at);
                            self.short_reanchor = None;
                        }
                    } else if to.1 > from.1 {
                        self.settle_carry(to.0, from.1, to.1, at);
                        self.settle_relocate(to.0, from.1, at, ctx);
                    }
                } else {
                    // A keyed same-row move over the hand's own lit text is a scrub
                    // ([`Ribbon::scrub`]); everything below is for a move that leaves the run.
                    let scrub = same_row
                        && licence.is_keyed_nav()
                        && self.scrub(to.0, from.1, to.1, at, ctx);
                    let jump = !same_row || to.1.abs_diff(from.1) >= JUMP_MIN_CELLS;
                    if scrub {
                        // Held; nothing else to do.
                    } else if jump {
                        // The wake, then the abandon that spares it. A row change lays no
                        // corridor (an Up-arrow recall must not paint the line it lands
                        // on), and a PTY cascade or a Return earns none.
                        let wakes = same_row && licence.is_keyed_nav();
                        let keep = if wakes {
                            self.wake(to.0, from.1, to.1, at, ctx, WAKE_LIFE_S, true)
                        } else {
                            None
                        };
                        // A program's jump strands the band under a hand that never left
                        // it: the band goes back into the hand's last column and the next
                        // key may take it back. A same-row jump the hand made drains toward
                        // the landing, origin end first, following the meteor (§2.6/§2.7).
                        let program = licence == Licence::Pty;
                        // A same-row jump names its row and [`Ribbon::abandon`] stamps that
                        // row alone; other rows drain into their own end. The program arm
                        // names no row.
                        let toward = if program {
                            Some((None, from.1, false))
                        } else {
                            same_row.then_some((Some(to.0), to.1, to.1 < from.1))
                        };
                        // A Return leaves its line printed text: its light dims off it
                        // as `leave_row`'s does. (A program's round trip below and back
                        // keeps the band for the hand's next key.)
                        if to.0 > from.0 && licence == Licence::Return {
                            self.print_row(from.0, at);
                        }
                        self.abandon(at, keep, toward, program);
                    } else if same_row && licence == Licence::Nav && from.1 != to.1 {
                        // An arrow's hop: a short wake behind the caret, no
                        // abandon — the band under the hand keeps its light.
                        self.wake(
                            to.0,
                            from.1,
                            to.1,
                            at,
                            ctx,
                            WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE,
                            false,
                        );
                    }
                }
            }
            Event::Focus(on) => {
                if on {
                    self.regain_focus(at);
                } else {
                    self.ember_at = Some(at);
                    self.rearm = None;
                    self.seam = None;
                }
            }
            Event::Sweep { row, col0, col1 } => {
                let (row, end) = self.insert_landing((row, col1), col1.saturating_sub(col0));
                let col0 = end.saturating_sub(col1.saturating_sub(col0));
                self.sweep(row, col0, end, at, ctx);
            }
            // The insert's rewrite: the kill's drain from the given column (the
            // caret mirror has not moved yet in this frame's `ctx`), and the
            // ribbon's caret follows it.
            Event::Rewrite { row, col, cells } => {
                let span_s = kill_span_s(cells);
                self.retract_suffix(row, col, cells.max(1), at, span_s);
                self.caret = Some((row, col));
            }
            // A Return closes the phrase: the next typed key opens a new
            // one, and the line the hand left goes out at its own rest. The
            // next key's walk continues the colour and starts its pace
            // again ([`Ribbon::fresh_line`]).
            Event::Return => {
                self.phrase_open = None;
                self.arm_fresh_line(at, None);
            }
            Event::ComposerNewline => {
                self.phrase_open = None;
                self.arm_fresh_line(at, None);
                if let Some(gate) = &mut self.fresh_line {
                    gate.chord_at = Some(at);
                    gate.awaiting_move = true;
                }
            }
            Event::CancelComposerNewline => {}
            Event::ReducedMotion(_) => {}
        }
    }

    /// Arm the new line's gate ([`Ribbon::fresh_line`]) at `at`, with the row
    /// the hand left when a cross-row move armed it. An armed gate keeps its
    /// earlier instant and first recorded row ([`FreshLine`]).
    #[inline]
    fn arm_fresh_line(&mut self, at: Instant, left_row: Option<u16>) {
        // A line the hand started is not the input continued: no seam.
        self.seam = None;
        self.fresh_line = Some(match self.fresh_line {
            Some(gate) => FreshLine {
                since: gate.since.min(at),
                chord_at: gate.chord_at,
                awaiting_move: gate.awaiting_move,
                left_row: gate.left_row.or(left_row),
            },
            None => FreshLine {
                since: at,
                chord_at: None,
                awaiting_move: false,
                left_row,
            },
        });
    }

    /// Arm the seam on `row` at `at` ([`Ribbon::seam`]): the input continued onto that row. A
    /// newer continuation replaces it.
    #[inline]
    pub fn arm_seam(&mut self, row: u16, at: Instant) {
        self.seam = Some(Seam { row, at });
    }

    /// The focused pane's `(first column, width)`; `None` is the whole grid.
    #[inline]
    pub fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// The focused pane's `(first row, rows)`; `None` is the whole grid.
    /// See [`Ribbon::pane_rows`] for why the band needs it.
    #[inline]
    pub fn set_pane_rows(&mut self, pane_rows: Option<(u16, u16)>) {
        self.pane_rows = pane_rows;
    }

    /// The floor the band may reach to, in frame pixels: the focused pane's
    /// bottom edge, or the grid's when no pane is set, never past the grid's.
    /// Every "is there leading below this row?" question asks this, not
    /// [`Geom::fx_bot`] ([`Ribbon::pane_rows`]).
    #[inline]
    fn pane_bot(&self, ctx: &Ctx<'_>, chf: f32) -> f32 {
        let grid = ctx.geom.fx_bot() as f32;
        match self.pane_rows {
            Some((row0, rows)) => (f32::from(ctx.geom.origin_y)
                + f32::from(row0.saturating_add(rows)) * chf)
                .min(grid),
            None => grid,
        }
    }

    /// Drop the curtain ([`CURTAIN_S`]): every cohort on glass is drawn into the
    /// hand it last stood under ([`Cohort::retract_col`], by the retract's
    /// per-row rule against the band's own head, [`Ribbon::curtain_hand`]) and
    /// spent to zero over 0.24 s, farthest first on [`CURTAIN_STAGGER_S`], with
    /// nothing laid until it is over. A ribbon with nothing laid stays ready; a
    /// falling curtain keeps its clock. The phrase ends with the space.
    pub fn curtain(&mut self, at: Instant) {
        self.phrase_open = None;
        // What is typed in the new space is a new line
        // ([`Ribbon::fresh_line`]) — and the row the hand left, if a move
        // recorded one, is a row of the space that is ending, as the pen
        // below is: the gate keeps its instant and forgets the row. A
        // composer home still owed in the old space cannot arrive in the
        // new one, so this boundary also releases its pending-home latch.
        self.arm_fresh_line(at, None);
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = None;
            gate.chord_at = None;
            gate.awaiting_move = false;
        }
        // The pen and the hand's floor belong to the coordinate space that is
        // ending ([`Ribbon::hand_floor`]), as does a soft-wrapped word waiting
        // for its reflow ([`Ribbon::settle_carry`]).
        self.clear_space_transient_state();
        if self.cells.is_empty() || self.curtain.is_some() {
            return;
        }
        // The hand is the band's own head, not [`Ribbon::caret`]: by the time a
        // program like `less` takes the screen, the Return that launched it
        // has moved the caret to column 0 of the next line, and reading that
        // column would drag the band sideways as it fades. The caret sits one
        // past the last laid glyph, so the target is the head cohort's `col1`.
        let hand = self.curtain_hand();
        for i in 0..self.cohorts.len() {
            // A cohort already retracting froze its target when the hand let
            // go of it; that is the same fact, measured earlier, and it stays.
            if self.cohorts[i].retract_col.is_some() {
                continue;
            }
            let coh = self.cohorts[i];
            let (target, right_first) = Self::target_toward(&coh, hand);
            self.cohorts[i].retract_col = target;
            self.cohorts[i].drain_right_first = right_first;
        }
        self.curtain = Some(at);
    }

    /// Where the hand was when a curtain falls: `(row, column one past the
    /// band's head)`, read from the cohorts so the host's caret at the seam
    /// cannot move it. The newest cohort the hand typed into (`!wake`, newest
    /// [`Cohort::alive_at`]), else the newest cohort; `col1` is exclusive, so it
    /// is the column the caret stood in after the last lay.
    #[inline]
    fn curtain_hand(&self) -> Option<(u16, u16)> {
        let newest = |wake_ok: bool| {
            freshest(self.cohorts.iter().filter(|c| wake_ok || !c.wake)).map(|c| (c.row, c.col1))
        };
        newest(false).or_else(|| newest(true))
    }

    /// Whether a curtain is falling — the host's "nothing is laid" gate
    /// and the twins' witness.
    #[inline]
    #[must_use]
    pub fn curtained(&self) -> bool {
        self.curtain.is_some()
    }

    /// The phrase rest in force, seconds: [`phrase_rest_ms`] of the hand's
    /// tempo; the floor before any key.
    #[inline]
    #[must_use]
    pub fn rest_s(&self) -> f32 {
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        phrase_rest_ms(ioi) / 1000.0
    }

    /// The open phrase's id — read-only, for the twins.
    #[must_use]
    #[cfg(test)]
    pub fn phrase_open(&self) -> Option<u32> {
        self.phrase_open
    }

    /// A typed key on the score (§2.1, §2.3): measure the gap since the last key, resolve a
    /// rest against [`Ribbon::rest_s`] at the previous tempo
    /// ([`super::timing::phrase_rest_reached`]'s law), advance the tempo by the melody's law,
    /// and open a new phrase if the last one is closed. Once per `Typed` event.
    #[inline]
    fn note_key(&mut self, at: Instant) {
        let gap_ms = self
            .last_key_at
            .map(|t| (at.saturating_duration_since(t).as_secs_f32() * 1000.0).round());
        let first = gap_ms.is_none();
        let rest = gap_ms.is_none_or(|g| g >= self.rest_s() * 1000.0);
        if rest || self.phrase_open.is_none() {
            self.open_phrase();
        }
        let gap = gap_ms.map_or(0, |g| g.clamp(0.0, f32::from(u16::MAX) * 64.0) as u32);
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        self.ioi_ms = ioi_next(ioi, gap, first, rest);
        self.last_key_at = Some(at);
    }

    /// Mint the next phrase and open it.
    #[inline]
    fn open_phrase(&mut self) {
        let id = self.next_phrase.max(1);
        self.next_phrase = id.wrapping_add(1).max(1);
        self.phrase_open = Some(id);
    }

    /// The phrase a typed lay belongs to — the open one, opened here if a
    /// typed echo arrives with none open (a ledger-bridged sweep after a
    /// Return, say): a typed cell always has a phrase.
    #[inline]
    fn phrase_for_lay(&mut self) -> u32 {
        if self.phrase_open.is_none() {
            self.open_phrase();
        }
        self.phrase_open.unwrap_or(1)
    }

    /// The hand is on the phrase under it (an erase or a kill): every un-abandoned cohort of
    /// the open phrase on the caret's row is held ([`Ribbon::hold`]) and the rest is measured
    /// from this key (`ribbon_row_hold_model`'s `OnlyOwnerRenews`; the phrase narrows the hold
    /// within the row). An erase after the rest opens a new phrase and adopts the caret row's
    /// un-abandoned cohorts into it, so a Backspace run begun during the after-glow brings the
    /// band back.
    fn hold_phrase(&mut self, at: Instant, row: u16) {
        let rested = self
            .last_key_at
            .is_some_and(|t| at.saturating_duration_since(t).as_secs_f32() >= self.rest_s());
        let open = match self.phrase_open {
            Some(open) if !rested => open,
            _ => {
                self.open_phrase();
                let id = self.phrase_open.unwrap_or(1);
                for coh in &mut self.cohorts {
                    if !coh.abandoned && coh.row == row {
                        coh.phrase = id;
                    }
                }
                id
            }
        };
        self.last_key_at = Some(at);
        self.hold(open, row, None, at);
    }

    /// Refresh every un-abandoned cohort of phrase `open` on `hand_row` (and the one laid into)
    /// at `at`: the laying clock, the phase, the frozen retract target, the grace, and its
    /// cells' life floor, so a lengthened rest never outlives its cells. A held row is wet
    /// again: a flowing cohort loses its flow and drift, since a shell's Backspace back through
    /// a fold reaches its row only through a hold.
    fn hold(&mut self, open: u32, hand_row: u16, laid_into: Option<usize>, at: Instant) {
        let grace = self.rest_s();
        let floor = swoosh_life_s(grace);
        let held_by = |i: usize, c: &Cohort| {
            !c.abandoned && c.phrase == open && (c.row == hand_row || laid_into == Some(i))
        };
        let mut any_held = false;
        for (i, coh) in self.cohorts.iter_mut().enumerate() {
            if held_by(i, coh) {
                any_held = true;
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                coh.grace_s = grace;
                if coh.flow.take().is_some() {
                    coh.flow_drift = 0.0;
                }
            }
        }
        if !any_held {
            return;
        }
        let cohorts = &self.cohorts;
        for cell in &mut self.cells {
            if cell.life_s < floor
                && cohorts
                    .iter()
                    .enumerate()
                    .any(|(i, c)| c.id == cell.cohort && held_by(i, c))
            {
                cell.life_s = floor;
            }
        }
    }

    /// The hand came back up through the fold: an erase's retreat one row up lands it on the
    /// row it had folded off ([`Ribbon::leave_row`]). Every un-abandoned cohort there still
    /// flowing joins the open phrase `open`, and the row is held ([`Ribbon::hold`]) at the
    /// arrival. The erase alone cannot guarantee this: where it is replayed depends on when its
    /// echo lands, and a run begun after the rest mints its phrase on the row below.
    #[inline]
    fn take_row_back(&mut self, open: u32, row: u16, at: Instant) {
        for coh in &mut self.cohorts {
            if !coh.abandoned && coh.row == row && coh.flow.is_some() {
                coh.phrase = open;
            }
        }
        self.hold(open, row, None, at);
    }

    /// The retract target of a cohort (§2.6, per row): the caret's column when
    /// the caret is on its row; otherwise the cohort's end nearest the caret in
    /// logical order (its right end for a row above, its left end for a row
    /// below), so a row never drains toward another row's column. Returned with
    /// the drain order ([`Cohort::drain_right_first`]).
    #[inline]
    fn retract_target(&self, coh: &Cohort) -> (Option<u16>, bool) {
        Self::target_toward(coh, self.caret)
    }

    /// A flowing band's initial fold edge, drain order and drift
    /// ([`Ribbon::advance_swoosh`]). Later content loss narrows the edge and
    /// drift to the surviving live span in [`Ribbon::retire`]. The edge is the band's kept
    /// end nearest the hand in logical order (rightmost non-leaving cell above
    /// the caret, leftmost below), not the cohort's bounds, which may still
    /// include cells a re-wrap stamped to leave ([`Ribbon::leave_row`]). The
    /// drift is priced once from that cell's birth momentum on the hot edge's
    /// ramp, capped at [`FLOW_DRIFT_MAX_SHARE`] of the kept length.
    fn flow_target(&self, coh: &Cohort) -> (Option<u16>, bool, f32) {
        let below = self.caret.is_some_and(|(row, _)| coh.row > row);
        let (lo, hi) = self
            .cells
            .iter()
            .filter(|c| c.row == coh.row && c.cohort == coh.id && !c.leaving())
            .fold((u16::MAX, 0u16), |(lo, hi), c| {
                (lo.min(c.col), hi.max(c.col))
            });
        let (lo, hi) = if lo > hi {
            (coh.col0, coh.col1.saturating_sub(1).max(coh.col0))
        } else {
            (lo, hi)
        };
        let edge = if below { lo } else { hi };
        // A shift can leave an older live writer over a newer leaving one.
        // Resolve the kept edge first, then price every resident at it;
        // pool order is not birth order (reach cells inherit their tail's birth).
        let disp = self
            .cells
            .iter()
            .filter(|c| c.row == coh.row && c.cohort == coh.id && c.col == edge)
            .max_by(|a, b| a.born.cmp(&b.born))
            .map_or(0.0, |c| c.birth_disp);
        let heat = smoothstep01((disp - HOT_EDGE_DISP_MIN) / HOT_EDGE_DISP_SPAN);
        let kept = f32::from(hi - lo) + 1.0;
        let drift = (FLOW_DRIFT_MIN_CELLS + (FLOW_DRIFT_MAX_CELLS - FLOW_DRIFT_MIN_CELLS) * heat)
            .min(FLOW_DRIFT_MAX_SHARE * kept);
        (Some(edge), below, drift)
    }

    /// [`Ribbon::retract_target`]'s rule against an arbitrary hand, so a
    /// curtain can freeze the same targets against the band's own head
    /// ([`Ribbon::curtain_hand`]) instead of against a caret the seam has
    /// already moved.
    #[inline]
    fn target_toward(coh: &Cohort, hand: Option<(u16, u16)>) -> (Option<u16>, bool) {
        match hand {
            None => (None, false),
            Some((hrow, hcol)) if coh.row == hrow => (Some(hcol), false),
            Some((hrow, _)) if coh.row < hrow => {
                (Some(coh.col1.saturating_sub(1).max(coh.col0)), false)
            }
            Some(_) => (Some(coh.col0), true),
        }
    }

    /// An erase (a Backspace or a kill of `cells` at `caret`): retract the
    /// row's suffix from the caret farthest-first over `span_s`
    /// ([`Ribbon::retract_suffix`]), owe the retreat its echo has not yet made
    /// (`pending_erase`), and hold the row's light through
    /// [`Ribbon::hold_phrase`]. The Backspace's `2 × 0.24 s` and the kill's
    /// `12·n + 240 ms` are this one law with two spans.
    ///
    /// The suffix, not the erased cell with the rest slid left: a slide stacks a slid live
    /// cell on the retracting one for the retract's length.
    #[inline]
    fn erase(&mut self, caret: (u16, u16), cells: u16, span_s: f32, at: Instant) {
        // In a shell line the suffix is the one erased cell; in a wrapped
        // composer the caret sits inside the text, and the band follows its
        // text through the witness rather than leaving a cell lit over a
        // letter that slid away.
        self.retract_suffix(caret.0, caret.1, cells, at, span_s);
        self.pending_erase = self.pending_erase.saturating_add(cells);
        self.hold_phrase(at, caret.0);
    }

    /// Whether `cell` is still live at `at`: not leaving, and inside its
    /// life measured from the later of its birth and its cohort's revival
    /// ([`live_since`]).
    #[inline]
    fn alive_at(&self, cell: &Cell, at: Instant) -> bool {
        !cell.leaving()
            && at
                .saturating_duration_since(live_since(&self.cohorts, cell))
                .as_secs_f32()
                < cell.life_s
    }

    /// Whether a live cell owns `(row, col)` at `at` — the cell a sweep or
    /// a relay leaves exactly as it is.
    #[inline]
    fn owned(&self, row: u16, col: u16, at: Instant) -> bool {
        self.cells
            .iter()
            .any(|c| c.row == row && c.col == col && self.alive_at(c, at))
    }

    /// A key whose echo has not landed is held, not laid. `Event::Typed` is replayed at the
    /// engine's mirror and [`Ribbon::lay`] lays `caret.col − n`, the key's own cell only once
    /// its echo moved the mirror; a frame earlier it is the previous cell, a phantom. The
    /// engine calls this instead of [`Ribbon::on_event`] for a `Typed` with no move after it in
    /// the frame: the `Typed` arm minus the lay (erase count closed, a Space remembered, the
    /// phrase held). The key's cell comes with its echo, from the seam's sweep.
    ///
    /// `cell` is the cell the held key will lay, or `None` for a caret the engine never
    /// learned; a held Space is remembered there, since the composer's word move measures from
    /// the Space's own cell and the echo's sweep cannot correct it later.
    #[inline]
    pub(super) fn hold_typed(
        &mut self,
        ev: &super::Event,
        at: Instant,
        ctx: &Ctx<'_>,
        cell: Option<(u16, u16)>,
    ) {
        if let Event::Typed { class, .. } = *ev {
            self.pending_erase = 0;
            if class == TypedClass::Space
                && let Some(cell) = cell
            {
                self.note_space(cell);
            }
            // A key whose echo has not landed is still the hand on that row, so
            // it holds the phrase exactly as a laid one does.
            self.hold_phrase(at, ctx.caret.0);
        }
    }

    /// A typed echo's glyph cells ([`Event::Sweep`]): lay every cell in `col0..col1` on `row`
    /// as typing, born at the echo, unless a live cell born at or after `at` (this key's own)
    /// already owns it. The sweep lights what the keys could not: a batched echo's cells behind
    /// the landing and a late echo's glyph (the key was held, [`Ribbon::hold_typed`]). A live
    /// cell born before this key is an earlier glyph the hand typed over (a mid-line insert),
    /// re-laid as this key's own with a fresh identity and the light it had
    /// ([`Ribbon::place`]'s monotone re-wet); otherwise the witness would melt the column as
    /// replaced text.
    fn sweep(&mut self, row: u16, col0: u16, col1: u16, at: Instant, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        for col in col0..col1.min(cols) {
            let own = self
                .cells
                .iter()
                .any(|c| c.row == row && c.col == col && c.born >= at && self.alive_at(c, at));
            if !own {
                self.lay_cell(row, col, at, ctx, true);
            }
        }
    }

    /// Focus came back. If the ember was still burning, carry its fade into an
    /// `edge-in` rearm so the ribbon comes back up rather than snapping to
    /// full. A burned-out focus loss has nothing to rearm (the pool was
    /// cleared by [`Ribbon::retire`]). The stamp is the common fade, which
    /// [`ember_level`] maps to each cell's own level.
    #[inline]
    fn regain_focus(&mut self, at: Instant) {
        if let Some(ember) = self.ember_at.take() {
            let r = at.saturating_duration_since(ember).as_secs_f32() / FOCUS_EMBER_S;
            if r < 1.0 {
                self.rearm = Some((at, spend(clamp01(r))));
            }
        }
    }

    /// Lay `n` cells ending at the caret. The caret sits one past the last
    /// echoed glyph, so the cells are `[caret.col − n, caret.col)`, laid
    /// oldest first so the head is the newest cell in the pool (the field
    /// index's last-writer-wins then equals a newest-first scan).
    ///
    /// The wrap folds (§4): a cell running off the left edge lands at the right
    /// end of the row above, in its own cohort (a cohort never spans rows), and
    /// the new row's cohort continues the folded one's walk, colour and pace
    /// ([`Cohort::d0`]).
    fn lay(&mut self, n: u16, at: Instant, ctx: &Ctx<'_>) {
        if n == 0 || ctx.geom.cols == 0 {
            return;
        }
        let cols = i32::try_from(ctx.geom.cols).unwrap_or(i32::MAX);
        // A fold wraps at the focused pane's edges (`set_pane`): the cell before
        // its first column is the previous row's last pane cell, never the
        // neighbouring pane's.
        let (pane_col0, pane_cols) = self
            .pane
            .map_or((0, cols), |(c0, w)| (i32::from(c0), i32::from(w).max(1)));
        let pane_col1 = pane_col0.saturating_add(pane_cols).min(cols);
        let (row, col_end) = ctx.caret;
        for back in (1..=i32::from(n)).rev() {
            let mut c = i32::from(col_end) - back;
            let mut r = i32::from(row);
            while c < pane_col0 {
                r -= 1;
                c += pane_cols;
            }
            if r < 0 || c >= pane_col1 {
                continue;
            }
            let (Ok(r), Ok(c)) = (u16::try_from(r), u16::try_from(c)) else {
                continue;
            };
            self.lay_cell(r, c, at, ctx, true);
        }
    }

    /// The spine a birth is priced at this tick: [`Ctx::birth_disp`] (the
    /// resume-floored spine), floored again by a live mend ([`Ctx::mend`],
    /// §23): a key that fixes a typo within a breath is born at
    /// `max(birth_disp, mend.disp)`, so the fixed cell's `cov0`, life and hot
    /// edge match the run it rejoins. Live-spine consumers (wave, wedge,
    /// caret, stars) keep reading [`Ctx::disp`].
    #[inline]
    fn birth_price(ctx: &Ctx<'_>) -> f32 {
        clamp01(
            ctx.mend
                .map_or(ctx.birth_disp, |m| ctx.birth_disp.max(m.disp)),
        )
    }

    /// The life one cell earns, priced at birth (§4's chain law, verbatim)
    /// — and spent from the cohort's clock ([`live_since`]): it is how long
    /// the cell outlives the last key of its run.
    #[inline]
    fn cell_life(&self, at: Instant, ctx: &Ctx<'_>) -> f32 {
        let full = ctx.cfg.duration.as_secs_f32().max(0.001);
        let base = (full * LIFE_DURATION_GAIN).clamp(LIFE_BASE_MIN, LIFE_BASE_MAX);
        let d = Self::birth_price(ctx);
        let life = base * (1.0 + LIFE_SPINE_LINEAR * d + LIFE_SPINE_SQUARE * d * d);
        // The four-letter guarantee: the observed gap is the newest cohort's
        // idle time; a burst's first key has no gap, takes no floor, and so a
        // lone keystroke fades crisply.
        let gap = self
            .cohorts
            .iter()
            .map(|c| at.saturating_duration_since(c.alive_at).as_secs_f32())
            .fold(f32::INFINITY, f32::min);
        let life = if gap <= CHAIN_GAP_MAX {
            life.max((CHAIN_KEYS * gap + CHAIN_MARGIN).min(CHAIN_LIFE_MAX))
        } else {
            life
        };
        // The exit swoosh must get to finish before the natural melt steals the
        // ending — even for a lone keystroke, which earns a full mini-swoosh.
        life.max(swoosh_life_s(self.rest_s()))
    }

    /// Which row changes the hand could have made under an echo licence (a
    /// typed key's or a delivered insert's own motion).
    ///
    /// A key can carry the caret one row down (the fold, or a composer's
    /// continuation row), and a Backspace one row up onto that row's right end.
    /// Every other echoed row change is an app repainting: a home to `(row − 1,
    /// 0)`, a two-row hop, a clear-and-rewrite's parking pass. Stated on the
    /// move (up and to the right), not on a column threshold, since a
    /// repaint's home can be sampled anywhere along the row it writes.
    #[inline]
    #[must_use]
    fn hand_changed_row(from: (u16, u16), to: (u16, u16)) -> bool {
        if to.0 == from.0.saturating_add(1) {
            return true;
        }
        from.0 == to.0.saturating_add(1) && to.1 > from.1
    }

    /// A key lands at the insert it made, not inside the text it pushed
    /// ([`Ribbon::shifted_now`]). `landing` is where a typed lay of `cells`
    /// ends. When the shift pass read an insert on that row this tick and the
    /// lay would fall on the tail it pushed right, the landing is the insert's
    /// end, `lo + dc`. Otherwise `landing` is returned as it came.
    #[inline]
    #[must_use]
    fn insert_landing(&self, landing: (u16, u16), cells: u16) -> (u16, u16) {
        if cells == 0 {
            return landing;
        }
        let (row, end) = landing;
        let first = end.saturating_sub(cells);
        for &(r, lo, hi, dc) in &self.shifted_now {
            let Ok(dc) = u16::try_from(dc) else {
                continue;
            };
            if r != row || cells > dc {
                continue;
            }
            let (moved0, moved1) = (lo.saturating_add(dc), hi.saturating_add(dc));
            if first >= moved0 && end.saturating_sub(1) <= moved1 {
                return (row, lo.saturating_add(dc));
            }
        }
        landing
    }

    /// Where a typed key lands: the caret the tick reports, unless laying there would write the
    /// key's glyph on a row the key cannot have written on. That is a repaint's parking pass (a
    /// composer homes to its top-left, clears and rewrites under a `Typed` licence,
    /// `cursor_glow::v2_licence`), where [`Ribbon::lay`]'s fold arithmetic would walk the cell
    /// onto the end of the row above; the key lands at the pen instead, `cells` on.
    ///
    /// "Where the hand is" is [`Ribbon::pen`], not [`Ribbon::caret`] (the mirror every `Move`
    /// plants, a parking pass included); a cold session has no pen and brings no key back.
    ///
    /// The law is the row reached, not distance or column: a landing is the hand's when
    /// [`Ribbon::fold_top_row`] is the landing's own row (nothing folded) or the pen's (the
    /// hand's line wrapping). Upward, a key can only Backspace back through the fold
    /// ([`Ribbon::hand_changed_row`]); every other upward caret under an echo is an app homing.
    /// The re-home needs a band cell on the pen's row, leaving or not (a held Backspace can
    /// leave only retracting cells), so it never invents a row the ribbon sees nothing on.
    #[must_use]
    fn typed_landing(&self, caret: (u16, u16), cells: u16, cols: u16) -> (u16, u16) {
        let Some(pen) = self.pen else {
            return caret;
        };
        if cells == 0 {
            return caret;
        }
        // The pen's own row is not exempt: a same-row caret whose leftmost cell underflows the
        // pane's first column walks onto the end of the row above (readline's CR park: bash
        // reprints after a bare CR, so mid-redraw the caret passes through the prompt's columns
        // under a `Typed` licence). A same-row landing that folds nothing has
        // `fold_top_row == caret.0` and returns as is. Re-homing every same-row caret breaks
        // the seam and witness laws, and a guard inside [`Ribbon::lay`] would forbid a genuine
        // wrap back to the pen's row.
        if caret.0 >= pen.0 {
            let top = self.fold_top_row(caret, cells, cols);
            if top == i32::from(caret.0) || top == i32::from(pen.0) {
                return caret;
            }
        } else if Self::hand_changed_row(pen, caret) {
            // Upward only a Backspace back through the fold ([`Ribbon::hand_changed_row`]).
            return caret;
        }
        if !self.cells.iter().any(|c| c.row == pen.0) {
            return caret;
        }
        (pen.0, pen.1.saturating_add(cells.max(1)))
    }

    /// The pane the fold wraps at, as [`Ribbon::lay`] resolves it: the focused pane's first
    /// column and width, or the grid's when no pane is set. Read by [`Ribbon::fold_top_row`].
    #[inline]
    #[must_use]
    fn fold_pane(&self, cols: u16) -> (i32, i32) {
        let (c0, w) = self.pane.unwrap_or((0, cols));
        (i32::from(c0), i32::from(w).max(1))
    }

    /// The topmost row [`Ribbon::lay`] would write on for this landing:
    /// `caret.0` when nothing folds, one row higher per pane width the
    /// leftmost cell runs off the left edge. This is `lay`'s walk (`while c <
    /// pane_col0 { r -= 1; c += pane_cols }`) in one `div_euclid` (`−1 / 120`
    /// floors to `−1`, `−121` to `−2`). `i32` because it may run off the top
    /// of the grid, as `lay`'s `r < 0` arm expects.
    #[inline]
    #[must_use]
    fn fold_top_row(&self, caret: (u16, u16), cells: u16, cols: u16) -> i32 {
        let (pane_col0, pane_cols) = self.fold_pane(cols);
        let left = i32::from(caret.1) - i32::from(cells) - pane_col0;
        i32::from(caret.0) + left.div_euclid(pane_cols).min(0)
    }

    /// The hand's own fold: an echoed move exactly one row down — a wrap, or
    /// a composer's next continuation row. [`Ribbon::leave_row`]'s one
    /// non-abandoning shape.
    #[inline]
    #[must_use]
    fn folded_down(from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
        licence.is_echo() && to.0 == from.0.saturating_add(1)
    }

    /// The hand left the row: a typed or delivered move onto another row. The
    /// origin row's cohorts are abandoned into the point the hand left at
    /// (`from.1`, T5: a fold's right end, or where a re-wrapped word was typed
    /// up to), and a key that returns there may take them back; the new row's
    /// mark starts its own clock, so a key on the new row never holds the old
    /// one.
    ///
    /// A typed landing past the pane's first two columns is a composer's
    /// re-anchor: the app moved the row's last word down with the caret, so
    /// the word is relaid where the caret reveals it ([`Relocate`]); a
    /// landing at the pane's edge is a plain fold, whose cells the host's fold
    /// sweep lays.
    fn leave_row(&mut self, from: (u16, u16), to: (u16, u16), at: Instant, licence: Licence) {
        // A soft-wrapped caret moved no text ([`SoftWrap`]): the key's glyph
        // stands at `from` and only the caret wrapped. Nothing is retracted or
        // relaid; the row flows as a fold and the word waits for the key that
        // reflows it ([`Ribbon::settle_carry`]).
        let soft_wrap = self
            .soft_wrap
            .filter(|sw| sw.origin == from && sw.landing == to);
        self.carry = None;
        if let Some(sw) = soft_wrap
            && Self::folded_down(from, to, licence)
        {
            // The input continues onto the new row, so the seam is armed; it
            // draws from the first cell the hand lays there
            // ([`Ribbon::hand_floor_on`]), never over the reflowed word or the
            // indent.
            self.arm_seam(to.0, at);
            self.flow_row(from.0, at);
            self.pending_erase = 0;
            self.last_space = None;
            self.prev_space = None;
            self.arrived = None;
            self.relocate = None;
            self.carry = Some((sw, at));
            return;
        }
        // A fold is a flow, not a departure: the row it wrapped off is marked [`Cohort::flow`]
        // ([`Ribbon::flow_row`]) rather than abandoned, and no key on the new row renews it.
        // Every other row change (a Return, an arrow, a program's cascade, a Backspace back up)
        // abandons. A composer's re-wrap takes the row's last word and the eaten Space with it
        // ([`Ribbon::begin_relocate`]): those `w + 1` cells retract here and arrive on the walk
        // they had ([`Ribbon::relay_origin`]). A plain terminal fold lands on the pane's first
        // column and takes nothing.
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let rewrap = self
            .rewrap_start
            .filter(|rewrap| rewrap.from == from && rewrap.to == to);
        let w = rewrap
            .and_then(|rewrap| rewrap.width)
            .unwrap_or_else(|| self.moved_word_len(from, at));
        let held_after = self.space_held_after_the_wrap(from);
        let re_wrapped = licence == Licence::Typed && to.1 > pane_col0.saturating_add(1);
        // The origin is read before the retract stamps the word's cells.
        let origin = if re_wrapped {
            self.relay_origin(from, w)
        } else {
            None
        };
        if re_wrapped {
            // One relay per split: a later one reads its own.
            self.split_off = None;
        }
        if Self::folded_down(from, to, licence) {
            self.arm_seam(to.0, at);
            if re_wrapped && w > 0 {
                let moved = (w + 1).min(from.1.saturating_sub(pane_col0));
                self.retract_span(from.0, from.1 - moved, moved, at, RETRACT_FADE_S);
            }
            self.flow_row(from.0, at);
        } else {
            // A line the hand left downward by anything but a navigation (a
            // Return, the line feed after it) is printed text now: its light
            // dims to [`RETURN_PRINTED_SHARE`] over [`RETURN_DIM_S`], clear of
            // its letters by the time the output lands, and goes on out
            // through the swoosh's retract (at full light the band under the
            // line's last word took it under 4.5:1 on paper for longer than
            // a sixth of a second).
            if to.0 > from.0 && licence != Licence::Nav {
                self.print_row(from.0, at);
            }
            self.abandon_row(from.0, at, Some(from.1));
            // The hand is back on the row it folded off: an erase's retreat one
            // row up, inside the phrase (not a line kill, which ends it).
            if licence == Licence::Typed
                && self.pending_erase > 0
                && to.0.checked_add(1) == Some(from.0)
                && let Some(open) = self.phrase_open
            {
                self.take_row_back(open, to.0, at);
            }
        }
        self.pending_erase = 0;
        self.reseat_spaces(held_after, to);
        self.arrived = None;
        self.relocate = None;
        if re_wrapped {
            let rewrap = self.take_rewrap_start(from, to);
            if let Some(rewrap) = rewrap
                && rewrap.start.checked_add(w).is_some_and(|end| end < to.1)
            {
                // The exact-key sweep lays the new suffix separately. This
                // old prefix returns at its observed indent on its old walk.
                self.relay_word(to.0, rewrap.start, rewrap.start + w, at, origin);
                if let Some(space) = rewrap.space {
                    self.note_space((to.0, space));
                }
            } else {
                self.begin_relocate(to, w, at, origin);
            }
        }
    }

    /// The fold flow begins: the hand left `row` downward by typing (a shell
    /// fold, a composer re-wrap, [`Ribbon::leave_row`]), or the follow pass
    /// carried the run up with its text ([`Ribbon::translate_run`]). Every
    /// typed cohort on it still laying or in grace is marked [`Cohort::flow`]
    /// at `at`. A cohort past its grace stays on its swoosh (restarting from
    /// full would brighten it); an abandoned cohort keeps its rewound clock; a
    /// wake expires on its own life.
    #[inline]
    fn flow_row(&mut self, row: u16, at: Instant) {
        for coh in &mut self.cohorts {
            if coh.row == row {
                Self::flow_cohort(coh, at);
            }
        }
    }

    /// [`Ribbon::flow_row`]'s law on one cohort: a typed cohort still in
    /// its laying or grace, not abandoned and not already flowing, starts
    /// its flow at `at`.
    #[inline]
    fn flow_cohort(coh: &mut Cohort, at: Instant) {
        if coh.abandoned || coh.wake || coh.flow.is_some() {
            return;
        }
        if !matches!(
            Phase::at(
                at.saturating_duration_since(coh.alive_at).as_secs_f32(),
                coh.grace_s
            ),
            Some(Phase::Laying | Phase::Grace | Phase::Reaching)
        ) {
            return;
        }
        coh.flow = Some(at);
        coh.release_clock = None;
    }

    /// The walk the moved word had: the `t` the relaid word's first letter had
    /// on the row it came off, and the walk's distance there. Read from the
    /// cohort `O` owning the cell before the origin caret (`from.1 − 1`, the
    /// word's last letter, leaving or not) at the word's first column:
    /// `O.t_at(from.1 − w)` and `O.walk_d(from.1 − w)` (signed; the walk is
    /// linear through zero). The relay mints its cohort on that pair
    /// ([`Ribbon::relay_word`]), so each word cell continues the old row's
    /// walk and the wrap key's glyph takes what its column would have had.
    /// When no cell owns that column any more, the walk is read off the copy
    /// the follow pass carried away ([`SplitOff`]). `None` when neither
    /// exists; the relay then joins or mints as usual.
    #[inline]
    #[must_use]
    fn relay_origin(&self, from: (u16, u16), w: u16) -> Option<WalkPos> {
        if w == 0 {
            return None;
        }
        let last = from.1.checked_sub(1)?;
        let first = from.1.checked_sub(w)?;
        if let Some(owner) = self
            .cells
            .iter()
            .rev()
            .find(|c| c.row == from.0 && c.col == last)
        {
            let coh = self.cohorts.iter().find(|c| c.id == owner.cohort)?;
            return Some(coh.walk_at(first));
        }
        // The word's cells are gone but its walk is not: the follow pass's copy
        // ([`SplitOff`]) keeps the run's walk verbatim, colour and distance.
        let split = self.split_off?;
        if !(split.col0..split.col1).contains(&last) {
            return None;
        }
        let copy = self.cohorts.iter().find(|c| c.id == split.copy)?;
        (copy.row.checked_add_signed(split.dr.checked_neg()?) == Some(from.0))
            .then(|| copy.walk_at(first))
    }

    /// A composer's same-row re-anchor: a typed move landing at least [`RE_ANCHOR_MIN_CELLS`]
    /// left of its origin on the same row with no erase behind it (Claude Code's bottom-pinned
    /// box growing up, Codex's rewrite). The old line's cells at and past the landing drain into
    /// the caret farthest first within [`RETRACT_DUR_S`], and the moved word is relaid where
    /// the caret reveals it, on the walk it had ([`Ribbon::relay_origin`]). The follow pass
    /// ([`super::Engine::follow_rows`]) has already carried the run up with its glyphs, so what
    /// the retract finds past the landing is exactly the moved word's old cells.
    ///
    /// A home is not a relocation: readline's `^A` parks the caret at the input's first column
    /// under a `Typed` licence and moves nothing, so the relay must fall inside the hand's own
    /// ink on this row ([`Ribbon::hand_floor`]); a span starting left of it relays nothing.
    fn re_anchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        let n = from.1 - to.1;
        let rewrap = self.take_rewrap_start(from, to);
        let w = rewrap
            .and_then(|rewrap| rewrap.width)
            .unwrap_or_else(|| self.moved_word_len(from, at));
        let held_after = self.space_held_after_the_wrap(from);
        // The origin is read before the retract stamps the word's cells.
        let origin = self.relay_origin(from, w);
        // A redraw that carried more than its wrap key: a stalled wrap brings
        // every key pressed meanwhile in the same repaint, so the moved text can
        // start left of the echoed word (`w`). The landing row's sample says
        // where it starts ([`Ribbon::note_rewrap_start`]); if further left, the
        // whole span to the landing is relaid as one run on the word's walk.
        let start = rewrap.map(|rewrap| rewrap.start);
        // One relay per split ([`SplitOff`]): a later one reads its own.
        self.split_off = None;
        // The re-anchor drains the old line toward its landing on the retract's clock,
        // whatever the app rewrites there.
        self.retract_suffix(to.0, to.1, n, at, RETRACT_DUR_S);
        self.reseat_spaces(held_after, to);
        if let Some(space) = rewrap.and_then(|rewrap| rewrap.space) {
            self.note_space((to.0, space));
        }
        self.arrived = None;
        // The span `begin_relocate` would lay is `[landing − 1 − w, landing −
        // 1)`. An unknown floor bounds nothing; when `note_rewrap_start`
        // sampled a continuation indent `start` left of the first line's
        // prompt, the wrapped word at `start..to.1` lowers the floor to
        // `start`.
        let hand_floor = self
            .hand_floor_on(to.0)
            .map(|floor| start.map_or(floor, |s| floor.min(s)));
        let relays_off_the_hand_s_ink =
            hand_floor.is_some_and(|floor| to.1.saturating_sub(1).saturating_sub(w) < floor);
        if relays_off_the_hand_s_ink {
            return;
        }
        if let (Some(start), Some(glyph)) = (start, to.1.checked_sub(1))
            && start <= glyph.saturating_sub(w)
            && start < glyph
            && hand_floor.is_none_or(|floor| start >= floor)
        {
            self.relocate = None;
            self.relay_word(to.0, start, to.1, at, origin);
            return;
        }
        self.begin_relocate(to, w, at, origin);
    }

    /// The content witness says the text this typed rewrap relays starts at
    /// `start` on its landing row. Only the matching move consumes it.
    #[inline]
    pub(super) fn note_rewrap_start(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        start: u16,
        width: Option<u16>,
        space: Option<u16>,
    ) {
        // The follow pass may also sample an earlier buffered move. A legacy
        // sample must not replace a queued exact-key width/Space proof.
        if width.is_none()
            && self
                .rewrap_start
                .is_some_and(|rewrap| rewrap.width.is_some())
        {
            return;
        }
        self.rewrap_start = Some(RewrapStart {
            from,
            to,
            start,
            width,
            space,
        });
    }

    /// An earlier buffered move cannot consume a later move's exact geometry.
    #[inline]
    fn take_rewrap_start(&mut self, from: (u16, u16), to: (u16, u16)) -> Option<RewrapStart> {
        let rewrap = self
            .rewrap_start
            .filter(|rewrap| rewrap.from == from && rewrap.to == to)?;
        self.rewrap_start = None;
        Some(rewrap)
    }

    /// Correct a Space's cell from an exact ordered key batch and its licensed
    /// same-row sweep. The engine alone has both witnesses; a final frame
    /// caret cannot locate an earlier Space in a coalesced echo. Metadata
    /// only: this neither lays cells nor changes their births or clocks.
    #[inline]
    pub(super) fn observe_space_cell(&mut self, row: u16, col: u16) {
        self.note_space((row, col));
    }

    /// Remember a typed Space's cell, keeping the one it replaces
    /// ([`Ribbon::prev_space`]). A correction of the same Space's cell
    /// (the same row, the cell the engine's sweep says it really took)
    /// keeps the Space before it rather than becoming it.
    #[inline]
    fn note_space(&mut self, cell: (u16, u16)) {
        if self.last_space != Some(cell) {
            self.prev_space = self.last_space;
        }
        self.last_space = Some(cell);
    }

    /// The Space the moved word starts after. A Space remembered exactly at the
    /// move's origin caret was typed after the wrap key (one typed before it
    /// would have moved the mirror past its own cell): it was held at the
    /// stale mirror while the park held the wrap's move, so the word starts
    /// after the Space before it (`tests/composer_growth_follows.rs`).
    #[inline]
    fn word_space(&self, from: (u16, u16)) -> Option<(u16, u16)> {
        match self.last_space {
            Some(cell) if cell == from => self.prev_space,
            other => other,
        }
    }

    /// Whether the last Space is the one held after the wrap key at the
    /// stale mirror ([`Ribbon::word_space`]): its real cell is the landing
    /// the wrap's move reveals, where the app's repaint put the caret.
    #[inline]
    fn space_held_after_the_wrap(&self, from: (u16, u16)) -> bool {
        self.last_space == Some(from)
    }

    /// After a re-anchor or a fold consumed the Space bookkeeping: a Space
    /// held after the wrap key is re-seated at the landing `to`, the cell
    /// its echo will lay, so the next wrap measures its word from it; every
    /// other Space is spent with the row it was typed on.
    #[inline]
    fn reseat_spaces(&mut self, held_after: bool, to: (u16, u16)) {
        self.prev_space = None;
        self.last_space = held_after.then_some(to);
    }

    /// Replay the `Move` the seam judged a soft-wrapped caret ([`SoftWrap`]):
    /// the verdict stands for exactly this one event.
    #[inline]
    pub(super) fn soft_wrap_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.soft_wrap = Some(sw);
        self.on_event(ev, at, ctx);
        self.soft_wrap = None;
    }

    /// Replay the `Move` the seam judged a lifted word ([`Lift`]): the
    /// verdict stands for exactly this one event.
    #[inline]
    pub(super) fn lift_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, lift: Lift) {
        self.lift = Some(lift);
        self.on_event(ev, at, ctx);
        self.lift = None;
    }

    /// Replay the `Move` whose echo the seam saw carry the soft-wrapped word
    /// `sw` off its row ([`Ribbon::settle_carry`]): the verdict stands for
    /// exactly this one event.
    #[inline]
    pub(super) fn reflow_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.reflow = Some(sw);
        self.on_event(ev, at, ctx);
        self.reflow = None;
    }

    /// Replay the `Sweep` the seam named the revive of a retired typed run
    /// ([`Revive`]): the run's walk stands for exactly this one event.
    #[inline]
    pub(super) fn revive_sweep(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, rv: Revive) {
        self.revive = Some(rv);
        self.on_event(ev, at, ctx);
        self.revive = None;
    }

    /// The soft-wrapped word waiting for the key that reflows it, if any —
    /// what the seam reads the glass for (`CursorGlow::carried_word_left`).
    #[inline]
    #[must_use]
    pub(super) fn carry(&self) -> Option<SoftWrap> {
        self.carry.map(|(sw, _)| sw)
    }

    /// The lifted word goes up with its text: a Space typed after `already` in
    /// `already[Image #1]` at the start of the continuation row lets `already`
    /// fit the row above, and Ink rewrites it at that row's end while the caret
    /// stays before `[Image`. The word's lit cells leave its old row on the
    /// retract's fade and are relaid under its glyphs above on the walk they
    /// had ([`Ribbon::relay_origin`]); that row is one the hand left by typing,
    /// so it flows ([`Ribbon::flow_row`]). The word's measure is consumed.
    fn lift_word(&mut self, lift: Lift, at: Instant) {
        let (row, end) = lift.origin;
        let col0 = lift.landing.1;
        self.last_space = None;
        self.prev_space = None;
        self.arrived = None;
        self.relocate = None;
        let Some(lit0) = (col0..end)
            .rev()
            .take_while(|&col| self.owned(row, col, at))
            .last()
        else {
            return;
        };
        let lit = end - lit0;
        let t0 = self.relay_origin((row, end), lit);
        self.retract_span(row, lit0, lit, at, RETRACT_FADE_S);
        let (dst_row, dst0) = lift.dst;
        self.relay_word(
            dst_row,
            dst0.saturating_add(lit0 - col0),
            dst0.saturating_add(end - col0),
            at,
            t0,
        );
        self.flow_row(dst_row, at);
    }

    /// The soft-wrapped word comes down with its text. When the first same-row forward typed
    /// echo from a soft-wrapped caret's landing advances past the word the wrap key ended plus
    /// its own glyph, the app erased that partial word from the origin row and rewrote it from
    /// the indent. The press budget refuses that hop, so the word's lit cells leave the origin
    /// row and are relaid from the indent on the walk they had ([`Ribbon::relay_origin`]),
    /// the reflowing key's glyph beside them. A forward echo that did not carry the word, or
    /// one later than [`RELOCATE_PATIENCE_S`], drops the carry. A burst or paste after a short
    /// word looks the same, so the relay also needs the seam's verdict for this very `Move`
    /// ([`Ribbon::reflow_move`]: the word's columns are blank on the glass).
    fn settle_carry(&mut self, row: u16, from_col: u16, to_col: u16, at: Instant) {
        let Some((sw, wrapped_at)) = self.carry else {
            return;
        };
        let (landing_row, landing) = sw.landing;
        if landing_row != row || from_col != landing {
            return;
        }
        self.carry = None;
        let (src_row, src_end) = (sw.origin.0, sw.origin.1.saturating_add(1));
        let n = src_end.saturating_sub(sw.word_col0);
        if n == 0
            || self.reflow != Some(sw)
            || at.saturating_duration_since(wrapped_at).as_secs_f32() > RELOCATE_PATIENCE_S
            || to_col <= landing.saturating_add(n)
        {
            return;
        }
        // Only the hand's own light moves: the lit suffix of the word (a
        // word the hand finished rather than began keeps its unlit head).
        let Some(lit0) = (sw.word_col0..src_end)
            .rev()
            .take_while(|&col| self.owned(src_row, col, at))
            .last()
        else {
            return;
        };
        let lit = src_end - lit0;
        let t0 = self.relay_origin((src_row, src_end), lit);
        self.retract_span(src_row, lit0, lit, at, RETRACT_FADE_S);
        let dst0 = landing + (lit0 - sw.word_col0);
        self.relay_word(row, dst0, to_col.min(landing + n + 1), at, t0);
    }

    /// Cells of the word the app moved off `from`'s row: everything the hand typed on the row
    /// up to the origin column since the later of its last typed Space ([`Ribbon::word_space`])
    /// and where it last arrived ([`Ribbon::arrived`]); `0` with neither on the row (a one-word
    /// line wraps mid-word: the app moved nothing). An arrival inside the hand's own live typed
    /// run (an arrow back over the word, a typo fix) reaches back over that run, still bounded
    /// by the Space; an arrival into unlit program text stands (`tests/wrap_code_reflow.rs`).
    #[inline]
    pub(crate) fn moved_word_len(&self, from: (u16, u16), at: Instant) -> u16 {
        let after_space = match self.word_space(from) {
            Some((row, s)) if row == from.0 && s < from.1 => Some(s + 1),
            _ => None,
        };
        let arrived = self
            .arrived
            .filter(|&(row, col)| row == from.0 && col <= from.1)
            .map(|(row, col)| {
                (0..col)
                    .rev()
                    .take_while(|&c| self.typed_live(row, c, at))
                    .last()
                    .unwrap_or(col)
            });
        after_space.max(arrived).map_or(0, |start| from.1 - start)
    }

    /// Whether a live typed cell owns `(row, col)` at `at` — the hand's
    /// own light, as [`Ribbon::owned`] reads any light (a wake's included).
    #[inline]
    fn typed_live(&self, row: u16, col: u16, at: Instant) -> bool {
        self.cells
            .iter()
            .any(|c| c.row == row && c.col == col && c.typing && self.alive_at(c, at))
    }

    /// Resident typed suffix immediately before `from`, for a content proof
    /// whose preceding Space was program text rather than a typed event.
    /// This measures existing light; it grants no relocation by itself.
    #[inline]
    pub(crate) fn typed_suffix_len(&self, from: (u16, u16), at: Instant) -> u16 {
        (0..from.1)
            .rev()
            .take_while(|&col| self.typed_live(from.0, col, at))
            .last()
            .map_or(0, |start| from.1 - start)
    }

    /// Relay a re-anchor's moved word (`w` cells) before the wrap key's own
    /// glyph at `landing − 1`: now when the landing reveals it, else pending
    /// for the same-row forward move that will. Cells laid on the row this
    /// tick left of the landing (the wrap key's `Typed` replayed against a
    /// caret parked at the box's inset) are dropped: nothing was typed there.
    fn begin_relocate(&mut self, to: (u16, u16), w: u16, at: Instant, walk: Option<WalkPos>) {
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let (row, landing) = to;
        let Some(glyph) = landing.checked_sub(1) else {
            return;
        };
        if w == 0 {
            return;
        }
        if glyph >= w && glyph - w >= pane_col0 {
            self.relay_word(row, glyph - w, glyph, at, walk);
            return;
        }
        // The landing cannot hold the word: the caret is parked at the
        // inset and the real landing comes with a later move.
        self.cells
            .retain(|c| !(c.row == row && c.col < landing && c.born >= at && c.typing));
        self.relocate = Some(Relocate { row, w, at, walk });
    }

    /// The pending relocation's landing arrived: a same-row forward typed
    /// move on its row whose origin `from_col` is the moved word's end plus
    /// the wrap key's glyph. Lays `[from_col − 1 − w, from_col)`: the word
    /// and that glyph, which the refused half of the rewrite left dark.
    fn settle_relocate(&mut self, row: u16, from_col: u16, at: Instant, ctx: &Ctx<'_>) {
        let Some(rel) = self.relocate else {
            return;
        };
        if rel.row != row {
            return;
        }
        if at.saturating_duration_since(rel.at).as_secs_f32() > RELOCATE_PATIENCE_S {
            self.relocate = None;
            return;
        }
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let Some(glyph) = from_col.checked_sub(1) else {
            return;
        };
        if glyph < rel.w || glyph - rel.w < pane_col0 {
            return;
        }
        let _ = ctx;
        self.relay_word(row, glyph - rel.w, from_col, at, rel.walk);
        self.relocate = None;
    }

    /// Lay `col0..col1` on `row` as typing born at `at`, only where no live
    /// cell already is: the relocation's own sweep. With `walk` (the `t` and
    /// distance the word had on the row it came off, [`Ribbon::relay_origin`])
    /// the first cell mints the relay's cohort anchored at `col0`, so each
    /// cell is `walk_t(d + col − col0)` and the old row's walk continues; the
    /// wrap key's glyph beside it is folded onto the same origin by
    /// [`Ribbon::heal_seams`]. Without it the cells join the row's cohort.
    ///
    /// The word is one run: every cell after the first joins the cohort the
    /// first one minted, never an older cohort whose bounds still span the
    /// column.
    fn relay_word(&mut self, row: u16, col0: u16, col1: u16, at: Instant, walk: Option<WalkPos>) {
        let mut first = true;
        let mut minted: Option<u32> = None;
        let mut cached_like: Option<(u32, (f32, f32, f32, f32))> = None;
        for col in col0..col1 {
            if self.owned(row, col, at) {
                continue;
            }
            let own = minted.and_then(|id| self.cohorts.iter().position(|c| c.id == id));
            let idx = match (walk, own) {
                (_, Some(i)) => i,
                (Some(walk), None) if first => {
                    let i = self.mint_cohort(row, col, col0, walk, at, false);
                    minted = Some(self.cohorts[i].id);
                    i
                }
                _ => self.join_cohort(row, col, at, false),
            };
            first = false;
            let cohort = self.cohorts[idx];
            // Priced like its neighbour: the word is the hand's own glyphs
            // the app relocated, so it takes the light the row has.
            let (life_s, cov0, birth_disp, edge_cells) = match cached_like {
                Some((id, pricing)) if id == cohort.id => pricing,
                _ => {
                    let pricing = self
                        .cells
                        .iter()
                        .filter(|c| c.row == row && c.cohort == cohort.id && c.typing)
                        .max_by(|a, b| a.born.cmp(&b.born))
                        .map_or((SWOOSH_LIFE_S, BODY_COLD_SHARE, 0.0, HOT_EDGE_CELLS), |c| {
                            (c.life_s, c.cov0, c.birth_disp, c.edge_cells)
                        });
                    cached_like = Some((cohort.id, pricing));
                    pricing
                }
            };
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born: at,
                attack_at: at,
                life_s,
                cov0,
                typing: true,
                retract_at: None,
                retire_at: None,
                birth_disp,
                edge_cells,
                layer: Layer::Base,
                rearm: None,
                released_at: None,
            };
            self.place(idx, cell, at, row);
        }
    }

    /// The scrub, and the line between it and the jump. A same-row keyed move (`Nav`,
    /// `Synthetic`) on `row` is a scrub when the corridor it crossed, `[min, max)`, is lit end
    /// to end by live cells of cohorts not leaving: the hand moving over its own text. Nothing
    /// is laid, taken over or abandoned, and the cohorts owning the corridor are held. Returns
    /// whether the move was a scrub; anything else (onto dark ground, over a cell melting or
    /// leaving, to another row) is a jump ([`Ribbon::wake`], [`Ribbon::abandon`]).
    ///
    /// The hold renews the clock only, not the grace or [`Cell::life_s`] as [`Ribbon::hold`]
    /// does for a key: a navigation mints no note. Only cells not yet past their melt opening
    /// light the corridor, since renewing a melting stub would snap it to full (a typed band's
    /// life is at least [`swoosh_life_s`] of its grace, so it never trips this); and only the
    /// cohorts owning the corridor's columns are held, so a band shadowed beneath a wake keeps
    /// its clock.
    fn scrub(&mut self, row: u16, from: u16, to: u16, at: Instant, ctx: &Ctx<'_>) -> bool {
        let (lo, hi) = (from.min(to), from.max(to));
        if lo == hi {
            return false;
        }
        // The owner of a column: its newest cell that is not leaving (the
        // pool is append-ordered; `build` and `wake` read the same one).
        let owner = |cells: &[Cell], col: u16| {
            cells
                .iter()
                .rposition(|c| c.row == row && c.col == col && !c.leaving())
        };
        let reduced = ctx.cfg.reduced_motion;
        let mut last_coh: Option<(u32, Instant)> = None;
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                return false;
            };
            let cell = self.cells[i];
            if self.env_of(ctx, &cell) <= 0.0 {
                return false;
            }
            let alive_at = match last_coh {
                Some((id, alive_at)) if id == cell.cohort => alive_at,
                _ => {
                    let Some(coh) = self.cohorts.iter().find(|k| k.id == cell.cohort) else {
                        return false;
                    };
                    if coh.leaving_at(at) {
                        return false;
                    }
                    last_coh = Some((coh.id, coh.alive_at));
                    coh.alive_at
                }
            };
            // The melt guard: `spent` on the clock `env_of` reads
            // (`live_since`), against the instant the cell's own melt opens.
            let spent = at
                .saturating_duration_since(cell.born.max(alive_at))
                .as_secs_f32();
            let melt_opens = if reduced {
                (cell.life_s - REDUCED_MOTION_FADE_MS / 1000.0).max(0.0)
            } else {
                cell.life_s * (1.0 - EXPIRY_MELT_SHARE)
            };
            if spent > melt_opens {
                return false;
            }
        }
        // The hold: the owner of each corridor column, and only it. Holding
        // a cohort twice is the same hold.
        let mut last_held: Option<u32> = None;
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                continue;
            };
            let id = self.cells[i].cohort;
            if last_held == Some(id) {
                continue;
            }
            if let Some(coh) = self.cohorts.iter_mut().find(|k| k.id == id) {
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                last_held = Some(id);
            }
        }
        true
    }

    /// Abandon the cohorts of one row — see [`Ribbon::abandon`] — into the
    /// column the hand left the row at, taking them back if it returns.
    fn abandon_row(&mut self, row: u16, at: Instant, toward: Option<u16>) {
        if self.seam.is_some_and(|s| s.row == row) {
            self.seam = None;
        }
        for coh in &mut self.cohorts {
            if coh.row != row {
                continue;
            }
            coh.revoke_releases(at);
            if coh.abandoned {
                continue;
            }
            // Its own retract start (Rainbow Path v3 §2.6): a cohort whose
            // phrase rested longer is sent the same `0.40 + 0.24 s` out.
            let Some(rewound) =
                at.checked_sub(std::time::Duration::from_secs_f32(coh.retract_start()))
            else {
                continue;
            };
            if coh.retract_col.is_none() {
                coh.retract_col = toward;
            }
            coh.abandoned = true;
            coh.rejoinable = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
    }

    /// Every cohort on `row` is printed text from `at` ([`Cohort::printed`]):
    /// its light dims to [`RETURN_PRINTED_SHARE`] while the swoosh keeps its
    /// clock, nothing stamped as leaving. A cohort already printed keeps its
    /// clock.
    #[inline]
    fn print_row(&mut self, row: u16, at: Instant) {
        for coh in &mut self.cohorts {
            if coh.row == row && coh.printed.is_none() {
                coh.printed = Some(at);
            }
        }
    }

    /// The hand touched `(row, col)`: fold it into [`Ribbon::hand_floor`]. A
    /// new row replaces the floor; the same row keeps the leftmost. Callers
    /// are only places the hand acted (a typed glyph, a relocation it made,
    /// the caret an erase was replayed against), never program output.
    #[inline]
    fn touch_hand(&mut self, row: u16, col: u16) {
        self.hand_floor = Some(match self.hand_floor {
            Some((r, c)) if r == row => (row, c.min(col)),
            _ => (row, col),
        });
    }

    /// The leftmost column the reach may take on `row`, or `None` when the
    /// hand has not been seen there — an unknown floor bounds nothing.
    #[inline]
    fn hand_floor_on(&self, row: u16) -> Option<u16> {
        self.hand_floor.and_then(|(r, c)| (r == row).then_some(c))
    }

    /// The hand's floor ([`Ribbon::hand_floor`]) as `(row, col)`, or `None` when no row has
    /// seen the hand yet. The meteor's shower reads it (`meteor::Meteors::set_hand_floor`)
    /// under the reach's law: nothing the hand did not put there goes left of where the hand
    /// has been on that row.
    #[inline]
    pub(super) fn hand_floor_cell(&self) -> Option<(u16, u16)> {
        self.hand_floor
    }

    fn lay_cell(&mut self, row: u16, col: u16, at: Instant, ctx: &Ctx<'_>, typing: bool) {
        // The hand's own ink is the floor ([`Ribbon::hand_floor`]).
        if typing {
            self.touch_hand(row, col);
        }
        let life_s = self.cell_life(at, ctx);
        let birth_disp = Self::birth_price(ctx);
        let cov0 = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        // A new line's first cell ([`Ribbon::fresh_line`], [`FreshLine::takes`]) mints on
        // the walk's colour at distance zero and spends the gate, whatever cohort it lands
        // in. Not the old line's text: an older key's cell on the row the hand left, the old
        // line's last glyph replayed over a column its run already owns
        // (`Engine::replay_events` lays `caret − 1` before a Return's move; the gate stays
        // armed for the key's own glyph), or the revive ([`Revive`]) laying the old line back.
        let reviving = self.revive.is_some_and(|rv| rv.covers(row, col));
        let gate = self
            .fresh_line
            .filter(|gate| !reviving && gate.takes(row, at));
        let idx = self.join_cohort(row, col, at, gate.is_some());
        if let Some(gate) = gate {
            let joined = self.cohorts[idx];
            let re_laid = joined.born < gate.since
                && self
                    .cells
                    .iter()
                    .any(|c| c.row == row && c.col == col && c.cohort == joined.id);
            if !re_laid {
                self.fresh_line = None;
            }
        }
        let idx = if typing {
            self.fold_wakes_into_band(idx, row, col, at)
        } else {
            idx
        };
        let cohort = self.cohorts[idx];
        // A typed cell laid where the attach stood inherits its light: the
        // caret cell already carried the head's light under the block, so it
        // starts with its attack spent (edge-in and wipe alike); the pour is
        // the next attach's (`plan_run`). A cell laid elsewhere attacks.
        let inherited = typing && self.attach == Some((row, col));
        let attack_at = if inherited {
            at.checked_sub(std::time::Duration::from_secs_f32(
                EDGE_IN_S + ATTACK_WIPE_S,
            ))
            .unwrap_or(at)
        } else {
            at
        };
        let cell = Cell {
            row,
            col,
            cohort: cohort.id,
            t: cohort.t_at(col),
            born: at,
            attack_at,
            life_s,
            cov0,
            typing,
            retract_at: None,
            retire_at: None,
            birth_disp,
            edge_cells: edge_cells(ctx.surge, ctx.flow.heat),
            layer: Layer::Base,
            rearm: None,
            released_at: None,
        };
        self.place(idx, cell, at, ctx.caret.0);
        self.seat_fold_successor(idx, col, ctx);
    }

    /// The fold's last glyph seats the row below. A plain terminal fold's two glyphs arrive on
    /// one tick out of walk order: the key at the pane's last column is held under xterm's
    /// pending wrap, so its glyph comes with the fold's sweep after the next key's `Typed`
    /// already laid the new row's first cell one column short, and both sides of the fold
    /// would share a stop. So when a lay makes cohort `idx` reach the pane's last column, a
    /// cohort on the next row whose origin is exactly this one's `walk_at(col)` is re-seated at
    /// `walk_at(pane_col1 + k)` (`k` its anchor's offset into the row) and its cells
    /// re-stamped. On the fold's own tick this runs before any plan, so the stale stop is never
    /// drawn; a successor that continued from the true end is left alone.
    fn seat_fold_successor(&mut self, idx: usize, col: u16, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        let (pane_col0, pane_col1) = self
            .pane
            .map_or((0, cols), |(c0, w)| (c0, c0.saturating_add(w.max(1))));
        if col.saturating_add(1) != pane_col1 {
            return;
        }
        let a = self.cohorts[idx];
        let Some(below) = a.row.checked_add(1) else {
            return;
        };
        let stale = a.walk_at(col);
        for bi in 0..self.cohorts.len() {
            let b = self.cohorts[bi];
            if b.id == a.id
                || b.row != below
                || b.anchor_col < pane_col0
                || b.anchor_col >= pane_col1
                || b.origin_walk() != stale
            {
                continue;
            }
            let seat = a.walk_at(pane_col1.saturating_add(b.anchor_col - pane_col0));
            let coh = &mut self.cohorts[bi];
            coh.t0 = seat.t;
            coh.d0 = seat.d;
            let origin = *coh;
            for c in &mut self.cells {
                if c.cohort == origin.id {
                    c.t = origin.t_at(c.col);
                }
            }
        }
    }

    /// Put one built cell into the pool under the cohort at `idx`, laid at
    /// `at` while the hand is on `hand_row`: one owner per `(row, col,
    /// cohort)`, the cohort's bounds grown over it, and the cohort clocks
    /// that the lay refreshes.
    fn place(&mut self, idx: usize, mut cell: Cell, at: Instant, hand_row: u16) {
        let (row, col, typing) = (cell.row, cell.col, cell.typing);
        // One owner per cell: a retype replaces the light on that cell rather
        // than stacking an invisible second body under it.
        if let Some(slot) = self
            .cells
            .iter()
            .position(|c| c.row == row && c.col == col && c.cohort == cell.cohort)
        {
            let old = self.cells.remove(slot);
            // A cell a partial release took, laid again by the hand, is
            // accounted for ([`Ribbon::custody_replaced`]).
            self.custody_replaced(idx, old.released_at, 1);
            // Re-wetting is monotone: a live cell laid again (a key replayed
            // one tick before its echo re-lays the previous glyph's cell)
            // keeps its attack and the higher of the two prices, so a settled
            // cell does not dip. A leaving or out-of-time cell takes a fresh
            // attack. Identity is always the fresh birth, so the content
            // witness does not read the new key as the old glyph.
            let alive = self.alive_at(&old, at);
            if alive && old.typing == typing {
                cell.attack_at = old.attack_at.min(cell.attack_at);
                cell.cov0 = cell.cov0.max(old.cov0);
                cell.life_s = cell.life_s.max(old.life_s);
            }
        }
        self.cells.push(cell);
        let coh = &mut self.cohorts[idx];
        coh.col0 = coh.col0.min(col);
        coh.col1 = coh.col1.max(col + 1);
        if typing {
            // The hand is on this cohort now, whatever minted it; a flowing row the hand came
            // back to (a Backspace back through the fold) is re-wet.
            coh.wake = false;
            if coh.flow.take().is_some() {
                coh.retract_col = None;
                coh.flow_drift = 0.0;
            }
            // One finger, one phrase, on the hand's row: a live key holds every un-abandoned
            // cohort of the open phrase on the hand's row; a row the hand left leaves on its
            // own clock (`leave_row`), and a closed phrase's cohort keeps its own. Model-checked
            // (`aterm-spec` `ribbon_row_hold_model`, invariant `OnlyOwnerRenews`). The cohort the
            // cell was laid into is always refreshed: a fold's own sweep, dated at the key, lands
            // on the row the hand is leaving and is that row's last key.
            let open = self.phrase_for_lay();
            self.cohorts[idx].phrase = open;
            self.hold(open, hand_row, Some(idx), at);
        } else {
            // A wake cell holds only its own cohort, which is always a wake
            // cohort ([`Cohort::wake`]): a held arrow keeps its wake out of
            // the swoosh until it stops, and a jump lifts no other mark's
            // finger (`typing` is what the one-finger law reads).
            coh.alive_at = coh.alive_at.max(at);
            coh.phase = Phase::Laying;
            coh.retract_col = None;
        }
    }

    /// The wake (R5): lay the corridor a same-row move just crossed on `row`, from `from` to
    /// the landing `to`, newest at the landing (included, so the caret fill, the train's
    /// phase-lock and the bed under the caret read one walk). New cells are born along the
    /// flight ([`flight_ms`] of the span), live `life_s`, and number at most
    /// [`WAKE_MAX_CELLS`] back from the landing. The wake's cohort takes the band's own walk
    /// origin ([`Ribbon::wake_origin`]), so `t_at(col)` matches the band at every column (C2).
    ///
    /// The wake is an overlay (v3 §2.3/§2.7, K1–K4): every wake cell is [`Layer::Over`] and the
    /// band's [`Layer::Base`] cells are never taken over, re-priced or moved. With `takeover`
    /// (a jump) the corridor is its own cohort over every column it crosses; over band light a
    /// cell is born at the band's level ([`Cell::rearm`]) and at least its price, so the
    /// hand-off has no dip, and [`Ribbon::abandon`] drains the band beneath. A cell an earlier
    /// wake laid is re-owned in place (K4), so a Ctrl-A/Ctrl-E ping-pong holds one corridor and
    /// one wake cohort. The cap binds at the far edge (v3 §2.10), so a corridor is never two
    /// islands.
    ///
    /// Without `takeover` (a hop) a column any live light owns is left as it is (the hop-side
    /// half of [`Ribbon::scrub`]'s melt guard); light whose band is leaving
    /// ([`Cohort::leaving_at`]) is covered as a jump does. Its new cells join the row's adjacent
    /// wake cohort or mint one ([`Ribbon::join_wake_cohort`]), never a typed word's. Every wake
    /// cell is `typing: false`. Returns the minted cohort's id (the takeover arm) so the
    /// abandon can spare it.
    #[allow(clippy::too_many_arguments)]
    fn wake(
        &mut self,
        row: u16,
        from: u16,
        to: u16,
        at: Instant,
        ctx: &Ctx<'_>,
        life_s: f32,
        takeover: bool,
    ) -> Option<u32> {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        if cols == 0 || from == to || to >= cols {
            return None;
        }
        let leftward = to < from;
        let far = from.min(cols - 1);
        let span = far.abs_diff(to);
        // The flight clock (K3): a jump's `T = flight_ms(cells)`, the number the meteor head,
        // the bell and the pin fly on; a hop takes the flight floor.
        let flight_s = if takeover {
            flight_ms(f32::from(span))
        } else {
            FLIGHT_MIN_MS
        } / 1000.0;
        let n = f32::from(span).max(1.0);
        let birth_disp = clamp01(ctx.birth_disp);
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let (anchor_col, walk) = self.wake_origin(row, to, at);
        let minted = takeover.then(|| self.own_wake_cohort(row, to, from, anchor_col, walk, at));
        // `reach` is the farthest column past the cap where this corridor keeps light anyway
        // (an `Over` cell it re-owns, or band light it covers); new light is laid out to it,
        // so the corridor is one interval (K2), and `0` lets the cap bind as usual. Reach and
        // lay read the glass census's bar ([`Ribbon::corridor_keeps_glass_light`]), so an
        // invisible residue neither moves the edge nor is covered.
        let reach = (WAKE_MAX_CELLS..=span)
            .rev()
            .find(|&k| {
                let col = if leftward { to + k } else { to - k };
                self.corridor_keeps_glass_light(ctx, row, col, at)
            })
            .unwrap_or(0);

        // Far end first, landing last: the pool is append-ordered and the index is
        // last-writer-wins. The head is the landing by `head_col`'s wake clause, not by
        // birth order.
        for k in (0..=span).rev() {
            let col = if leftward { to + k } else { to - k };
            let j = span - k;
            let born_new = at
                .checked_add(std::time::Duration::from_secs_f32(
                    flight_s * f32::from(j) / n,
                ))
                .unwrap_or(at);
            // The `Over` owner at this column (an older wake's cell, whatever
            // its clock) and the `Base` cell beneath if it is live light; the
            // pool is append-ordered and `build` reads the same ones.
            let (over, base_cov, base_leaving) = self.corridor_column(ctx, row, col, at);
            // The `Over` owner while it is still light on the glass, for the
            // hop's guard alone. A jump re-owns whatever it finds there, live
            // or burnt out, because its corridor must be one interval.
            let over_live = over.filter(|&i| {
                let c = self.cells[i];
                !c.leaving() && self.env_of(ctx, &c) > 0.0
            });
            // Live means not leaving: band light whose band is leaving goes out on the band's
            // clock while the hop's cells stay lit, splitting the corridor, so the hop covers
            // it. No level floor can replace this: the drain empties neighbouring columns a few
            // ms apart.
            let base_live = base_cov > 0.0 && !base_leaving;
            if !takeover && (base_live || over_live.is_some()) {
                continue;
            }
            let (born, attack_at, cov0, rearm) = match over {
                Some(i) => {
                    // The reversal re-owns (K4, M1): an earlier wake's cell is taken in
                    // place, re-armed from its current level, on this cohort's clock; the
                    // old cohort keeps only uncovered cells and retires once emptied.
                    let old = self.cells[i];
                    let level = clamp01(self.env_of(ctx, &old));
                    self.cells.remove(i);
                    (old.born, old.attack_at, old.cov0, Some((at, level)))
                }
                None => {
                    if k >= WAKE_MAX_CELLS && k > reach && !Self::reads_as_glass_light(base_cov) {
                        // Beyond the cap, new light is not laid past `reach`;
                        // light the band shows there is covered and the
                        // drained columns up to it are laid, so the corridor
                        // stays whole. The same glass bar as the reach.
                        continue;
                    }
                    if base_cov > 0.0 {
                        // Over a live band cell (or, under a hop, a leaving
                        // one): born on the flight clock at the band's own
                        // current level (M1) and at least the band's price.
                        let cov0 = cov0_new.max(base_cov);
                        (
                            born_new,
                            born_new,
                            cov0,
                            Some((born_new, clamp01(base_cov / cov0))),
                        )
                    } else {
                        (born_new, born_new, cov0_new, None)
                    }
                }
            };
            let idx = match minted {
                Some(idx) => idx,
                None => self.join_wake_cohort(row, col, anchor_col, walk, at),
            };
            let cohort = self.cohorts[idx];
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born,
                attack_at,
                life_s,
                cov0,
                typing: false,
                retract_at: None,
                retire_at: None,
                birth_disp,
                // A wake cell is the swoosh's reach, not a key: no key, no
                // surge, and the crisp edge is the head's property anyway.
                edge_cells: HOT_EDGE_CELLS,
                layer: Layer::Over,
                rearm,
                released_at: None,
            };
            self.place(idx, cell, at, row);
        }
        minted.map(|idx| self.cohorts[idx].id)
    }

    /// What a corridor finds at `(row, col)` at `at`, as `(over, base_cov,
    /// base_leaving)`: the `Over` owner's pool index, whatever its clock; the
    /// newest `Base` cell's coverage share now (`cov0 · env`, `0` with no cell
    /// or a [`Cell::leaving`] one); and whether that cell's band is leaving
    /// ([`Cohort::leaving_at`]). One reading for [`Ribbon::wake`]'s reach and
    /// lay, so the two cannot disagree about a column.
    #[inline]
    fn corridor_column(
        &self,
        ctx: &Ctx<'_>,
        row: u16,
        col: u16,
        at: Instant,
    ) -> (Option<usize>, f32, bool) {
        let mut over = None;
        let mut base = None;
        for (i, c) in self.cells.iter().enumerate().rev() {
            if c.row != row || c.col != col {
                continue;
            }
            match c.layer {
                Layer::Over if over.is_none() => {
                    over = Some(i);
                    if base.is_some() {
                        break;
                    }
                }
                Layer::Base if base.is_none() => {
                    base = Some(c);
                    if over.is_some() {
                        break;
                    }
                }
                _ => {}
            }
        }
        let (base_cov, base_leaving) = base.map_or((0.0, false), |c| {
            let env = if c.leaving() {
                0.0
            } else {
                self.env_of(ctx, c)
            };
            let leaving = self
                .cohorts
                .iter()
                .find(|k| k.id == c.cohort)
                .is_none_or(|k| k.leaving_at(at));
            (c.cov0 * env, leaving)
        });
        (over, base_cov, base_leaving)
    }

    /// Whether `(row, col)` carries light on the glass at `at`: either layer
    /// composited at or above [`STATUS_LIT_COV`], the status census's bar.
    ///
    /// Used only by [`Ribbon::wake`]'s reach pre-pass: what the eye cannot
    /// see must not move the corridor's far edge, or [`WAKE_MAX_CELLS`] stops
    /// binding.
    #[inline]
    fn corridor_keeps_glass_light(&self, ctx: &Ctx<'_>, row: u16, col: u16, at: Instant) -> bool {
        let (over, base_cov, _) = self.corridor_column(ctx, row, col, at);
        // The lay re-owns an `Over` owner whatever its clock (K4), so the
        // corridor must reach it or leave a gap before an island.
        over.is_some() || Self::reads_as_glass_light(base_cov)
    }

    /// Whether a coverage share (`cov0 · env`, as [`Ribbon::corridor_column`]
    /// reports) composites to something the glass census reads as lit:
    /// [`STATUS_LIT_COV`] of [`UNDER_COV_CAP`].
    #[inline]
    fn reads_as_glass_light(share: f32) -> bool {
        UNDER_COV_CAP * share >= f32::from(STATUS_LIT_COV)
    }

    /// One live wake cohort per row (v3 §2.7, K4). A jump whose corridor
    /// overlaps a wake cohort still inside [`WAKE_LIFE_S`] of its arrival
    /// re-owns it (`alive_at := now`, the corridor's cells join it) instead of
    /// minting a second; otherwise a fresh one.
    ///
    /// [`WAKE_MAX_CELLS`] re-lays only the cells nearest the landing, so with
    /// two cohorts the rest would ride the previous clock and the row would
    /// tear where the clocks came due apart. One cohort makes the whole
    /// corridor leave together (K2).
    fn own_wake_cohort(
        &mut self,
        row: u16,
        to: u16,
        from: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
    ) -> usize {
        let (lo, hi) = (to.min(from), to.max(from));
        let live = self.cohorts.iter().enumerate().find(|(_, c)| {
            c.row == row
                && c.wake
                && !c.abandoned
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= WAKE_LIFE_S
                && c.col0 <= hi
                && lo < c.col1
        });
        if let Some((i, _)) = live {
            let coh = &mut self.cohorts[i];
            coh.alive_at = at;
            coh.phase = Phase::Laying;
            coh.retract_col = None;
            return i;
        }
        self.mint_cohort(row, to, anchor_col, walk, at, true)
    }

    /// The walk origin a wake takes on `row`, as `(anchor_col, walk)`. The row's freshest cohort
    /// inside the chain window (abandoned, retracting or not) shares its origin outright, so
    /// `t_at(col)` matches the band at every column and past both its ends
    /// (`tests/wake_and_landing_walk_origins.rs`). With no cohort on the row, the freshest
    /// anywhere is continued from one past its rightmost column (its colour only), anchored at
    /// `landing` at distance zero ([`WalkPos::restarted`], as `meteor::landing_field` walks
    /// there); red when nothing is laid.
    #[inline]
    fn wake_origin(&self, row: u16, landing: u16, at: Instant) -> (u16, WalkPos) {
        let fresh =
            |c: &&Cohort| at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX;
        if let Some(c) = freshest(self.cohorts.iter().filter(fresh).filter(|c| c.row == row)) {
            return (c.anchor_col, c.origin_walk());
        }
        let walk = freshest(self.cohorts.iter().filter(fresh))
            .map_or(WalkPos::COLD, |c| c.walk_at(c.col1));
        (landing, walk.restarted())
    }

    /// The cohort a hop's new cell at `col` joins: a wake cohort on the same
    /// row, not abandoned, inside the chain window, adjacent to or containing
    /// `col` (a held arrow's trail, or a jump's wake it runs into). Otherwise
    /// a new wake cohort on the row's origin `(anchor_col, walk)`. Never a
    /// typed cohort: the hop leaves the word's clock and bounds alone.
    #[inline]
    fn join_wake_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
    ) -> usize {
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && c.wake
                && !c.abandoned
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && col + 1 >= c.col0
                && col <= c.col1
        }) {
            return i;
        }
        self.mint_cohort(row, col, anchor_col, walk, at, true)
    }

    /// Push a new cohort covering `col` on `row` with the walk origin
    /// `anchor_col` on `walk` (its `t0` and odometer `d0`), born and alive at
    /// `at`; returns its index.
    #[inline]
    fn mint_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        walk: WalkPos,
        at: Instant,
        wake: bool,
    ) -> usize {
        let id = self.next_cohort;
        self.next_cohort = self.next_cohort.wrapping_add(1);
        // A wake keeps the floor grace: it is no phrase's, and its cells
        // expire on `WAKE_LIFE_S` before any longer rest could matter.
        let grace_s = if wake {
            PHRASE_REST_MIN_S
        } else {
            self.rest_s()
        };
        self.cohorts.push(Cohort {
            id,
            row,
            col0: col,
            col1: col + 1,
            anchor_col,
            t0: walk.t,
            d0: walk.d,
            born: at,
            alive_at: at,
            abandoned: false,
            wake,
            retract_col: None,
            drain_right_first: false,
            phase: Phase::Laying,
            rejoinable: false,
            phrase: 0,
            grace_s,
            flow: None,
            flow_drift: 0.0,
            release_clock: None,
            release_parts: [None; RELEASE_PARTS],
            parts_revoked_at: None,
            line_killed: false,
            printed: None,
        });
        self.cohorts.len() - 1
    }

    /// The cohort this cell joins: one on the same row, adjacent to or containing `col`, still
    /// inside the chain window. Otherwise a new one whose walk continues the freshest live
    /// cohort's from one past its rightmost column, colour and distance ([`Cohort::d0`]), so a
    /// wrap fold and a rebirth cross without a seam; a cold start re-anchors at red.
    ///
    /// One row, one walk (`RAINBOW-KITTY-V2.md` §42): when the freshest cohort is a run still
    /// standing on this row ([`Ribbon::stands`]) and no new line started, the new cohort takes
    /// that run's walk by position (`walk_at(col)`), so a corridor over both runs is seamless.
    /// `new_line` ([`Ribbon::fresh_line`]; the relay passes `false`) keeps the colour and drops
    /// the distance ([`WalkPos::restarted`]).
    ///
    /// Neither arm joins a dead run (no standing cell: melted by the witness, a line kill's
    /// drain, or a follow split whose moved word melted, `Witness::landed_goes_with_its_run`);
    /// the key mints instead, continuing its end, and a shared column has one owner per cohort
    /// until the dead cells go ([`Ribbon::place`]). A run Backspaced or `^W`-ed away is still
    /// its band while it drains, so a retype inside the fade rejoins it on its stops (C2). A
    /// revive's cell that would mint takes the run's own origin (§43, [`Revive`]).
    fn join_cohort(&mut self, row: u16, col: u16, at: Instant, new_line: bool) -> usize {
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && !c.abandoned
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && col + 1 >= c.col0
                && col <= c.col1
                && self.stands(c)
        }) {
            return i;
        }
        // The hand came back: a typed key landing in or beside a cohort this
        // row's program motion or a row change abandoned
        // (`Cohort::rejoinable`) takes it back and lifts the abandon (a TUI's
        // caret round trip, a Backspace across a wrap), so no dark word opens
        // inside a live line. A jump the hand made (Nav, Return) keeps its
        // abandon.
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && c.abandoned
                && c.rejoinable
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && col + 1 >= c.col0
                && col <= c.col1
                && self.stands(c)
        }) {
            let coh = &mut self.cohorts[i];
            coh.abandoned = false;
            coh.retract_col = None;
            coh.printed = None;
            return i;
        }
        // The revive mints on the run's own walk (`tests/revived_line_keeps_its_stops.rs`); a
        // new line's gate is neither read nor spent here ([`Ribbon::lay_cell`]).
        if let Some(rv) = self.revive.filter(|rv| rv.covers(row, col)) {
            return self.mint_cohort(row, col, rv.origin.anchor_col, rv.origin.walk(), at, false);
        }
        let fresh = |t: Instant| at.saturating_duration_since(t).as_secs_f32() <= CHAIN_GAP_MAX;
        // The freshest cohort, a tie to the walk's farther end ([`freshest`]), never pool
        // order. One row, one walk: a run still standing on this row lends its walk by
        // position unless a new line started (left of its start or past its end alike); a
        // dead run, another row's, a retired one (`last_walk`) and a new line read the end.
        let continued = freshest(self.cohorts.iter().filter(|c| fresh(c.alive_at)));
        let walk = continued
            .map(|c| {
                if c.row == row && !new_line && self.stands(c) {
                    c.walk_at(col)
                } else {
                    c.walk_at(c.col1)
                }
            })
            .or_else(|| {
                // A finished swoosh's walk continues too, inside the chain window (`retire`
                // drops the cohort with its cells).
                self.last_walk.filter(|w| fresh(w.alive_at)).map(|w| w.end)
            })
            .unwrap_or(WalkPos::COLD);
        // …unless the hand started a new line: the colour continues, the
        // pace restarts ([`Ribbon::fresh_line`]).
        let walk = if new_line { walk.restarted() } else { walk };
        self.mint_cohort(row, col, col, walk, at, false)
    }

    /// Whether `coh` is still a band a key can join ([`Ribbon::join_cohort`]):
    /// it has a cell on its row that is not dead. A cell not leaving
    /// ([`Cell::leaving`]) stands, and so does one on the retract of the
    /// hand's own erase (a Backspace or `^W`), whose run the next key takes
    /// back on its stops (C2, [`Cohort::anchor_col`]). Dead are a cell the
    /// content witness retired ([`Cell::retire_at`]) and one on the drain of
    /// a line kill ([`Cohort::line_killed`]: the next line is a new line).
    #[inline]
    fn stands(&self, coh: &Cohort) -> bool {
        self.cells.iter().any(|c| {
            c.cohort == coh.id
                && c.row == coh.row
                && c.retire_at.is_none()
                && (c.retract_at.is_none() || !coh.line_killed)
        })
    }

    /// A line kill drains its runs dead ([`Cohort::line_killed`]): every
    /// cohort on `row` the kill's retract left with no cell standing
    /// ([`Cell::leaving`]) is marked, so no key rejoins it while it drains
    /// ([`Ribbon::stands`]). A cohort with a cell still standing (a `^K`
    /// from mid-line) keeps its band.
    #[inline]
    fn mark_line_killed(&mut self, row: u16) {
        for k in 0..self.cohorts.len() {
            let coh = self.cohorts[k];
            if coh.row == row
                && !self
                    .cells
                    .iter()
                    .any(|c| c.cohort == coh.id && c.row == row && !c.leaving())
            {
                self.cohorts[k].line_killed = true;
            }
        }
    }

    /// The hand takes the wake it bridged. A word hop across dark ground lays a wake cohort that
    /// touches the typed band; a key at the landing would leave two cohorts under one band,
    /// each with its own swoosh, and a hole opens between them as they drain
    /// (`tests/codex_particle_replay.rs`). So a typed key folds every wake on its row that
    /// contains or abuts the cell it lays into the typed band it belongs to (the cohort it
    /// joined, else a typed cohort the wake touches): the wake's cells take the band's id and
    /// walk (unchanged, [`Ribbon::wake_origin`]), are priced like the band's newest glyph, and
    /// the band's bounds grow over them, so [`Ribbon::retract_suffix`]'s contiguity holds.
    /// Where the wake covered the band's spent cells the newer writer wins.
    ///
    /// Only into a typed cohort still laying or in its grace: the hold after every lay would
    /// bring a leaving band back to full in one frame. Such a key keeps its wake.
    fn fold_wakes_into_band(&mut self, idx: usize, row: u16, col: u16, at: Instant) -> usize {
        let fresh = |t: Instant| at.saturating_duration_since(t).as_secs_f32() <= CHAIN_GAP_MAX;
        let joined = self.cohorts[idx];
        let band_id = if joined.wake {
            let Some(j) = self.cohorts.iter().position(|c| {
                c.id != joined.id
                    && c.row == row
                    && !c.wake
                    && !c.abandoned
                    && c.retract_col.is_none()
                    && !matches!(c.phase, Phase::Reaching | Phase::Retracting | Phase::Fading)
                    && fresh(c.alive_at)
                    && c.col1 >= joined.col0
                    && joined.col1 >= c.col0
            }) else {
                return idx;
            };
            self.cohorts[j].id
        } else {
            joined.id
        };
        while let Some(w) = self.cohorts.iter().position(|c| {
            c.wake
                && c.id != band_id
                && c.row == row
                && !c.abandoned
                && fresh(c.alive_at)
                && col.saturating_add(1) >= c.col0
                && col <= c.col1
        }) {
            let wake = self.cohorts[w];
            let Some(b) = self.cohorts.iter().position(|c| c.id == band_id) else {
                break;
            };
            let band = self.cohorts[b];
            let like = self
                .cells
                .iter()
                .filter(|c| c.row == band.row && c.cohort == band.id && c.typing)
                .max_by(|a, b| a.born.cmp(&b.born))
                .copied();
            // The newer writer of a column wins: the band's own cells under
            // the wake's go, so no column carries two owners of one cohort.
            // A released band cell the wake shadows is replaced by a newer
            // writer, and the wake's own release custody goes with its cells
            // ([`Ribbon::custody_replaced`], [`Ribbon::carry_release`]).
            let mut ci = 0usize;
            while ci < self.cells.len() {
                let c = self.cells[ci];
                if c.cohort == band.id
                    && c.row == row
                    && self
                        .cells
                        .iter()
                        .any(|w| w.cohort == wake.id && w.col == c.col)
                {
                    let gone = self.cells.remove(ci);
                    self.custody_replaced(b, gone.released_at, 1);
                } else {
                    ci += 1;
                }
            }
            self.carry_release(wake.id, band.id);
            for cell in &mut self.cells {
                if cell.cohort == wake.id {
                    cell.cohort = band.id;
                    cell.t = band.t_at(cell.col);
                    if let Some(like) = like {
                        cell.life_s = cell.life_s.max(like.life_s);
                        cell.cov0 = cell.cov0.max(like.cov0);
                        cell.birth_disp = cell.birth_disp.max(like.birth_disp);
                        cell.edge_cells = like.edge_cells;
                    }
                }
            }
            self.cohorts[b].col0 = band.col0.min(wake.col0);
            self.cohorts[b].col1 = band.col1.max(wake.col1);
            self.cohorts.remove(w);
        }
        self.cohorts
            .iter()
            .position(|c| c.id == band_id)
            .unwrap_or(idx.min(self.cohorts.len().saturating_sub(1)))
    }

    /// Retract exactly what went: the `min_cells` columns from `col`,
    /// farthest-first inside `span_s`, and nothing past them.
    ///
    /// [`Ribbon::retract_suffix`]'s bounded twin, for a re-wrap's moved word
    /// ([`Ribbon::leave_row`]): `w + 1` cells off the old row's end and
    /// nothing else. The erase is still the suffix ([`Ribbon::erase`]).
    #[inline]
    fn retract_span(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        let last = col.saturating_add(min_cells.max(1) - 1);
        let n = f32::from(last - col + 1);
        let step = span_s / n;
        for cell in &mut self.cells {
            if cell.row != row || cell.col < col || cell.col > last || cell.leaving() {
                continue;
            }
            let rank = f32::from(last - cell.col);
            cell.retract_at = at.checked_add(std::time::Duration::from_secs_f32(rank * step));
        }
    }

    /// Retract the suffix of `row` from `col` on (every live cell at or right
    /// of the caret, and at least `min_cells` columns) farthest-first inside
    /// `span_s`, each cell then spending [`RETRACT_FADE_S`] to exactly zero.
    /// The backspace's `2 × 0.24 s` and the kill's `12·n + 240 ms` are the
    /// same law with two spans (§4): contiguous at every frame, nothing
    /// removed from the middle.
    fn retract_suffix(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        // A cell the follow pass placed this tick arrived in the batch's new
        // layout; this erase is addressed in the old one
        // ([`Ribbon::followed_now`]).
        let followed = &self.followed_now;
        let arrived = |c: &Cell| followed.contains(&(c.row, c.col, c.born));
        let far = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.col >= col && !c.leaving() && !arrived(c))
            .map(|c| c.col)
            .max();
        let last = far
            .unwrap_or(col)
            .max(col.saturating_add(min_cells.saturating_sub(1)));
        let n = f32::from(last - col + 1);
        let step = span_s / n;
        for cell in &mut self.cells {
            if cell.row != row
                || cell.col < col
                || cell.col > last
                || cell.leaving()
                || followed.contains(&(cell.row, cell.col, cell.born))
            {
                continue;
            }
            // Farthest from the caret goes first, so the mark visibly slurps
            // back toward the hand rather than blinking out.
            let rank = f32::from(last - cell.col);
            cell.retract_at = at.checked_add(std::time::Duration::from_secs_f32(rank * step));
        }
    }

    /// A real jump that leaves the run abandons the band (a same-row jump over a lit corridor
    /// is a [`Ribbon::scrub`] and never reaches here). Every cohort but the jump's own wake
    /// `keep` is sent into its retract by rewinding `alive_at` to its retract start, so the
    /// light leaves continuously from the level it has (`spend` starts at exactly `1.0`); one
    /// already retracting keeps its clock. Under the corridor the wake already covers the band
    /// ([`Ribbon::wake`]), so what retracts here is the light beyond it.
    ///
    /// `toward` pre-sets the retract's target and drain order: a program's jump takes the band
    /// back into the hand's last column, not where the program parked the cursor, and
    /// `rejoinable` lets the hand's next key take it back ([`Ribbon::join_cohort`]). A same-row
    /// jump the hand made passes `(row, landing, leftward)`: the landing is the frozen target
    /// (R1) and the drain follows the meteor, origin end first, so no band cell starts spending
    /// before the corridor cell above it is born. A row change passes `None` and the per-row
    /// law decides at the retract's first frame.
    fn abandon(
        &mut self,
        at: Instant,
        keep: Option<u32>,
        toward: Option<(Option<u16>, u16, bool)>,
        rejoinable: bool,
    ) {
        for coh in &mut self.cohorts {
            if keep == Some(coh.id) {
                continue;
            }
            coh.revoke_releases(at);
            if !coh.abandoned {
                coh.rejoinable = rejoinable;
            }
            // Rewound to its own retract start: a cohort whose phrase
            // rested longer is sent the same `0.40 + 0.24 s` out.
            let Some(rewound) =
                at.checked_sub(std::time::Duration::from_secs_f32(coh.retract_start()))
            else {
                continue;
            };
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
            // A column is only a place on its own row: a same-row jump's
            // landing column is stamped only into cohorts on that row, as
            // [`Ribbon::abandon_row`] does.
            if let Some((row, col, leftward)) = toward
                && row.is_none_or(|row| coh.row == row)
                && coh.retract_col.is_none()
            {
                coh.retract_col = Some(col);
                coh.drain_right_first = leftward;
            }
        }
    }

    /// Retire specific live cells on the fast melt: every live cell named in `cells` by
    /// identity `(row, col, born)` takes [`Cell::retire_at`] `= at`, spends to exactly zero over
    /// [`RETIRE_MELT_S`] on the retract's alpha law with its attack frozen ([`Ribbon::env_of`]),
    /// and leaves on the first plan past the melt. The content witness calls this for a cell
    /// whose glyph it saw replaced (`Engine::witness_rows`); a redraw that puts the same text
    /// back changes nothing (D2).
    ///
    /// By identity, because a position can hold more than one live cell (one owner per cohort,
    /// [`Ribbon::place`]); the birth is the one thing two cells at a position never share.
    /// Returns how many cells were newly stamped: a cell already leaving keeps its clock, so a
    /// repeated witness cannot restart a melt. `O(pool × cells)`; a retirement is an event.
    pub fn retire_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        let mut n = 0;
        for cell in &mut self.cells {
            if cell.leaving() || !cells.contains(&(cell.row, cell.col, cell.born)) {
                continue;
            }
            cell.retire_at = Some(at);
            if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cell.cohort) {
                coh.revoke_releases(at);
            }
            n += 1;
        }
        n
    }

    /// Release specific live cells to the swoosh: the content witness's other verdict, that the
    /// glyph under each of `cells` (by identity, as [`Ribbon::retire_cells`]) has gone with
    /// nothing in its place (a composer cleared on Enter, a Ctrl-U). Such light leaves as a row
    /// the hand left does, not on the [`RETIRE_MELT_S`] cut: each named cell's cohort is
    /// abandoned into its retract (`alive_at` rewound to [`RETRACT_START_S`] ago), drawn into
    /// the hand farthest-first, and not rejoinable. A cohort already abandoned keeps its clock.
    ///
    /// A release naming only part of a cohort's standing cells stamps exactly that part onto
    /// the retract (`retract_at` and [`Cell::released_at`]) and holds it in custody
    /// ([`ReleasePart`]), so the identical redraw a frame later can lift it
    /// ([`Ribbon::restore_releases`]); the cohort's clock is untouched.
    ///
    /// Returns how many named cells were resident and not already leaving (`ribbon_retired=`).
    /// The cells keep owning their columns for the retract's span, so the witness can still
    /// name them to `retire_cells`.
    pub fn release_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        let Some(rewound) = at.checked_sub(std::time::Duration::from_secs_f32(RETRACT_START_S))
        else {
            return 0;
        };
        let mut n = 0;
        // The cohorts something was named in, each once. Resident scratch,
        // taken and given back so this path allocates nothing (§18).
        let mut touched = std::mem::take(&mut self.released_cohorts);
        touched.clear();
        for cell in &self.cells {
            if !cell.leaving()
                && cells.contains(&(cell.row, cell.col, cell.born))
                && !touched.contains(&cell.cohort)
            {
                touched.push(cell.cohort);
            }
        }
        for &id in &touched {
            // A part leaves as an erase's suffix does, the cohort's clock untouched:
            // abandoning the whole cohort would drain every standing letter for one cell a
            // repaint blanked, and leaving the part standing would keep light over a cleared
            // suffix. A release naming every standing cell is the text gone whole.
            let whole = self
                .cells
                .iter()
                .filter(|c| c.cohort == id && !c.leaving())
                .all(|c| cells.contains(&(c.row, c.col, c.born)));
            let count = self.cells.iter().filter(|c| c.cohort == id).count();
            if !whole {
                let mut stamped = 0usize;
                for cell in &mut self.cells {
                    if cell.cohort == id
                        && !cell.leaving()
                        && cells.contains(&(cell.row, cell.col, cell.born))
                    {
                        cell.retract_at = Some(at);
                        cell.released_at = Some(at);
                        n += 1;
                        stamped += 1;
                    }
                }
                // Each part in its own custody, lifted when its glyphs are back
                // ([`Ribbon::restore_releases`]), so a torn read under an insert cannot leave
                // the newest letters with no clock to lift them. An abandoned cohort holds its
                // parts too: lifting a part hands the cells back to the retract the cohort is
                // already on, so a torn read of a leaving band does not take them out early.
                if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == id)
                    && stamped > 0
                {
                    coh.hold_part(ReleasePart {
                        stamp: at,
                        cells: stamped,
                    });
                }
                continue;
            }
            n += self
                .cells
                .iter()
                .filter(|c| c.cohort == id && !c.leaving())
                .count();
            let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == id) else {
                continue;
            };
            if !coh.abandoned {
                coh.release_clock = Some(ReleaseClock {
                    alive_at: coh.alive_at,
                    retract_col: coh.retract_col,
                    phase: coh.phase,
                    rejoinable: coh.rejoinable,
                    cells: count,
                });
            }
            coh.rejoinable = false;
            if coh.abandoned {
                continue;
            }
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
        self.released_cohorts = touched;
        n
    }

    /// Lift the releases whose text the witness found back. `candidates` (sorted) are
    /// `(cohort, None)` for a whole release and `(cohort, Some(stamp))` for one partial release
    /// ([`Cell::released_at`] `== stamp`); an explicit abandon or retirement never restores
    /// through this seam. `accepted` gets the cohorts something was lifted in, sorted.
    ///
    /// A restore counts only what was released: every released cell must still be resident (a
    /// pruned one would leave its column dark between the rest), while a cell laid after the
    /// release neither authorizes nor refuses. `CursorGlow::tick` lays the held key's cell into
    /// the same cohort first, dated at its press, so a partial release is counted by the cells
    /// still carrying its stamp. A whole release abandons its cohort unrejoinable, so its whole
    /// pool is the count.
    pub(super) fn restore_releases(
        &mut self,
        candidates: &[(u32, Option<Instant>)],
        accepted: &mut Vec<u32>,
    ) {
        accepted.clear();
        for &(id, stamp) in candidates {
            let Some(k) = self.cohorts.iter().position(|c| c.id == id) else {
                continue;
            };
            let Some(stamp) = stamp else {
                let Some(saved) = self.cohorts[k].release_clock else {
                    continue;
                };
                if self.cells.iter().filter(|c| c.cohort == id).count() != saved.cells {
                    continue;
                }
                // Recover the exact saved clock; no new life or birth is earned.
                let coh = &mut self.cohorts[k];
                coh.release_clock = None;
                coh.alive_at = saved.alive_at;
                coh.retract_col = saved.retract_col;
                coh.phase = saved.phase;
                coh.rejoinable = saved.rejoinable;
                coh.abandoned = false;
                accepted.push(id);
                continue;
            };
            let Some(part) = self.cohorts[k].part_mut(stamp).copied() else {
                continue;
            };
            let resident = self
                .cells
                .iter()
                .filter(|c| c.cohort == id && c.released_at == Some(stamp))
                .count();
            if resident != part.cells {
                continue;
            }
            // A partial release's restore lifts exactly the stamps it laid:
            // the cells are still resident inside their melt, and the
            // cohort's clock was never touched.
            for cell in &mut self.cells {
                if cell.cohort == id && cell.released_at == Some(stamp) {
                    cell.retract_at = None;
                    cell.released_at = None;
                }
            }
            for slot in &mut self.cohorts[k].release_parts {
                if slot.is_some_and(|p| p.stamp == stamp) {
                    *slot = None;
                }
            }
            accepted.push(id);
        }
        accepted.sort_unstable();
        accepted.dedup();
    }

    /// Account for `n` cells of cohort `idx` carrying release stamp `stamp`
    /// that left the pool because a newer cell took their column (an echo's
    /// sweep re-laying it, [`Ribbon::place`], or a fold's newer writer). The
    /// column is lit again by that identity, so the partial release's count
    /// ([`Ribbon::restore_releases`]) stops waiting for them and the other
    /// released cells can still be restored. A pruned cell (its retract ran
    /// out) is not replaced and still refuses.
    #[inline]
    fn custody_replaced(&mut self, idx: usize, stamp: Option<Instant>, n: usize) {
        let Some(stamp) = stamp else {
            return;
        };
        if n == 0 {
            return;
        }
        if let Some(part) = self.cohorts.get_mut(idx).and_then(|c| c.part_mut(stamp)) {
            part.cells = part.cells.saturating_sub(n);
        }
    }

    /// A fold carries its release custody: the cells of cohort `from` are
    /// being relabelled into `into` ([`Ribbon::fold_wakes_into_band`],
    /// [`Ribbon::heal_seams`]) and `from` goes, so its
    /// [`Cohort::release_parts`] and any pending revocation move with the
    /// cells, each part keeping its stamp and count. Otherwise a torn read's
    /// released cells could never be lifted. A whole release is never
    /// folded: both folds take only un-abandoned cohorts.
    #[inline]
    fn carry_release(&mut self, from: u32, into: u32) {
        let Some((parts, revoked)) = self
            .cohorts
            .iter()
            .find(|c| c.id == from)
            .map(|c| (c.release_parts, c.parts_revoked_at))
        else {
            return;
        };
        let Some(target) = self.cohorts.iter_mut().find(|c| c.id == into) else {
            return;
        };
        for part in parts.into_iter().flatten() {
            target.hold_part(part);
        }
        if let Some(t) = revoked {
            target.parts_revoked_at = Some(target.parts_revoked_at.map_or(t, |u| u.max(t)));
        }
    }

    /// Let go of the partial releases revoked since the last tick: every part
    /// released before its cohort's revocation instant
    /// ([`Cohort::revoke_releases`]); a part released at it or after stays in
    /// custody. Run at the top of the engine's tick, so a revocation gets
    /// exactly one content read in which a part's glyphs could come back.
    #[inline]
    pub(super) fn settle_revoked_parts(&mut self) {
        for coh in &mut self.cohorts {
            let Some(t) = coh.parts_revoked_at.take() else {
                continue;
            };
            for slot in &mut coh.release_parts {
                if slot.is_some_and(|p| p.stamp < t) {
                    *slot = None;
                }
            }
        }
    }

    /// No two typed cohorts abut on a row. A key echoed on the same tick as another is replayed
    /// against the tick's landing caret and lays one past the live cohort's end, so a second
    /// cohort is minted and the sweep fills the hole into the old one; `build_runs` would split
    /// the row there and `plan_run` feather the seam at [`RUN_TAIL_EASE`], a visible slit. So
    /// two live typed cohorts that abut are folded into one every frame, before the geometry.
    /// The walk is continuous there by construction ([`Cohort::d0`]), so the re-stamp is the
    /// identity on that shape (to the ulp).
    ///
    /// Both sides must still stand ([`Ribbon::stands`]): a melting remnant (the composer wrap's
    /// follow split: the eaten Space and the moved word's old cells) keeps its bounds until
    /// pruned, and merging a new line into it would re-stamp the line on the old walk
    /// (`tests/composer_growth_follows.rs`).
    fn heal_seams(&mut self) {
        let mut i = 0;
        while i < self.cohorts.len() {
            let a = self.cohorts[i];
            // A flowing cohort is leaving, whatever its target says on the
            // frame before its first plan: it never merges with a live one
            // (a merge keeps one clock, wrong for the other's cells).
            let hot =
                |c: &Cohort| !c.wake && !c.abandoned && c.retract_col.is_none() && c.flow.is_none();
            if !hot(&a) || !self.stands(&a) {
                i += 1;
                continue;
            }
            let next = self.cohorts.iter().position(|b| {
                b.id != a.id && b.row == a.row && b.col0 == a.col1 && hot(b) && self.stands(b)
            });
            let Some(j) = next else {
                i += 1;
                continue;
            };
            let b = self.cohorts[j];
            let (into, from) = if a.born <= b.born { (i, j) } else { (j, i) };
            let (into_id, from_id) = (self.cohorts[into].id, self.cohorts[from].id);
            // The newer writer of a column wins, as `build`'s sort would
            // have picked it; the shadowed older cell leaves the pool.
            let mut idx = 0;
            while idx < self.cells.len() {
                let c = self.cells[idx];
                let shadowed = c.cohort == into_id
                    && self.cells.iter().any(|d| {
                        d.cohort == from_id && d.row == c.row && d.col == c.col && d.born >= c.born
                    });
                if shadowed {
                    let gone = self.cells.remove(idx);
                    self.custody_replaced(into, gone.released_at, 1);
                } else {
                    idx += 1;
                }
            }
            self.carry_release(from_id, into_id);
            for c in &mut self.cells {
                if c.cohort == from_id {
                    c.cohort = into_id;
                }
            }
            let merged = self.cohorts[from];
            let coh = &mut self.cohorts[into];
            coh.col0 = coh.col0.min(merged.col0);
            coh.col1 = coh.col1.max(merged.col1);
            coh.alive_at = coh.alive_at.max(merged.alive_at);
            coh.phase = Phase::Laying;
            // One run, one origin: the survivor takes the leftmost origin (the smaller
            // anchor), all three numbers `(anchor_col, t0, d0)`, and every cell is re-stamped
            // `t = t_at(col)` from it, so the walk is one function of position across the seam
            // (C2). A mixed origin would put the zero off by the distances' difference
            // wherever the run crosses the knee.
            if merged.anchor_col < coh.anchor_col {
                coh.anchor_col = merged.anchor_col;
                coh.t0 = merged.t0;
                coh.d0 = merged.d0;
            }
            let origin = *coh;
            for c in &mut self.cells {
                if c.cohort == into_id {
                    c.t = origin.t_at(c.col);
                }
            }
            self.cohorts.remove(from);
            // Re-examine from the start: a merge can create a new abutment.
            i = 0;
        }
    }

    // -- the frame ---------------------------------------------------------

    /// Build this frame's [`Ribbon::plan`] and [`FieldIndex`]: the geometry
    /// pass, run once per tick before any producer emits and before stardust
    /// deals a birth, because §5.4's zones are relative to [`Band::top`].
    ///
    /// The order is fixed: sync the ink table for the live theme, advance the
    /// exit swoosh (which may lay the reach's extension cells), retire what
    /// is out of time, then sort once and fill the index and the geometry
    /// from that same pass (§18).
    pub fn plan(&mut self, ctx: &Ctx<'_>) {
        self.ink.sync(ctx.cfg);
        self.ground = ctx.cfg.theme_bg;
        self.caret = Some(ctx.caret);
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        self.wedge_px = (Self::body_profile(ctx.cfg).dn_ch - DN_FLOOR_CH).max(0.0) * self.cell_h;
        self.advance_swoosh(ctx);
        self.retire(ctx);
        if self
            .seam
            .is_some_and(|s| ctx.now.saturating_duration_since(s.at).as_secs_f32() >= FLOW_TOTAL_S)
        {
            self.seam = None;
        }
        self.heal_seams();
        self.build(ctx);
        // The scroll's tick is over: the next tick's backward caret is the
        // hand's own again ([`Ribbon::scroll_followed`]).
        self.scroll_followed = false;
        // …and the next tick's erases are addressed in the layout the
        // follow pass has already read ([`Ribbon::followed_now`]).
        self.followed_now.clear();
        self.short_reanchor = None;
        // Exact key proofs name a Move queued in this replay. Legacy sampled
        // starts may still belong to a held park waiting for its later flush.
        if self
            .rewrap_start
            .is_some_and(|rewrap| rewrap.width.is_some())
        {
            self.rewrap_start = None;
        }
        self.shifted_now.clear();
    }

    /// Drive the exit choreography and lay the reach's extension cells.
    ///
    /// The beats are staged in a fixed array, not a `Vec`: the reach wants at
    /// most [`REACH_BEATS`] cells per cohort and nothing on the frame path
    /// allocates (§18).
    fn advance_swoosh(&mut self, ctx: &Ctx<'_>) {
        let now = ctx.now;
        let mut reach = [(0u16, 0u16, 0u32); REACH_STAGE];
        let mut n_reach = 0usize;
        for i in 0..self.cohorts.len() {
            let coh = self.cohorts[i];
            let idle = coh.idle_s(now);
            let Some(phase) = coh.phase_at(idle) else {
                self.cohorts[i].phase = Phase::Fading;
                continue;
            };
            self.cohorts[i].phase = phase;
            // The retract's target and drain order are frozen the frame the retract begins
            // ([`Ribbon::retract_target`], per row, v3 §2.6): the mark is drawn back into the
            // hand that let go of it, never toward where the caret goes next.
            if phase.is_retracting() && self.cohorts[i].retract_col.is_none() {
                let coh = self.cohorts[i];
                if coh.flow.is_some() {
                    let (target, right_first, drift) = self.flow_target(&coh);
                    self.cohorts[i].retract_col = target;
                    self.cohorts[i].drain_right_first = right_first;
                    self.cohorts[i].flow_drift = drift;
                } else {
                    let (target, right_first) = self.retract_target(&coh);
                    self.cohorts[i].retract_col = target;
                    self.cohorts[i].drain_right_first = right_first;
                }
            }
            // Under reduced motion the mark is static (§6.11): no reach; and
            // under a curtain nothing is laid, the reach included.
            if phase != Phase::Reaching || ctx.cfg.reduced_motion || self.curtain.is_some() {
                continue;
            }
            // One extension cell per beat while the mark is short of the
            // four-letter span. They may overlap letters the burst never
            // typed; the ribbon is source-over and capped.
            let beats = (((idle - coh.grace_s) / REACH_STEP_S).floor() as i32 + 1)
                .clamp(0, i32::from(REACH_BEATS)) as u16;
            let span = coh.col1.saturating_sub(coh.col0);
            let want = FOUR_LETTER_CELLS.saturating_sub(span).min(beats);
            // The reach stops where the hand stopped ([`Ribbon::hand_floor`]):
            // it never extends past the first column the hand held on the row,
            // where only the shell's prompt lies.
            let reach_floor = self
                .hand_floor_on(coh.row)
                .unwrap_or(0)
                .max(self.pane.map_or(0, |(c0, _)| c0));
            for k in 0..want {
                let Some(c) = coh.col0.checked_sub(k + 1) else {
                    break;
                };
                if c < reach_floor {
                    break;
                }
                if n_reach == REACH_STAGE {
                    break;
                }
                reach[n_reach] = (coh.row, c, coh.id);
                n_reach += 1;
            }
        }
        for &(row, col, cohort) in &reach[..n_reach] {
            if self.cells.iter().any(|c| c.row == row && c.col == col) {
                continue;
            }
            self.lay_reach_cell(row, col, cohort);
        }
    }

    /// An extension cell of the exit swoosh's reach, for cohort `cohort`.
    ///
    /// It inherits the mark's own envelope (`born`, `life_s` and `cov0` from
    /// its cohort's tail cell), so it dies with the mark and can never be
    /// brighter than the light already on glass. Its stop is the cohort's
    /// walk read one cell further back.
    fn lay_reach_cell(&mut self, row: u16, col: u16, cohort: u32) {
        let Some(coh) = self.cohorts.iter().find(|c| c.id == cohort).copied() else {
            return;
        };
        let Some(&tail) = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.cohort == cohort)
            .min_by_key(|c| c.col)
        else {
            return;
        };
        self.cells.push(Cell {
            row,
            col,
            cohort,
            t: coh.t_at(col),
            born: tail.born,
            attack_at: tail.attack_at,
            life_s: tail.life_s,
            cov0: tail.cov0,
            typing: false,
            retract_at: tail.retract_at,
            retire_at: tail.retire_at,
            birth_disp: tail.birth_disp,
            edge_cells: tail.edge_cells,
            layer: tail.layer,
            rearm: tail.rearm,
            released_at: None,
        });
        if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cohort) {
            coh.col0 = coh.col0.min(col);
        }
    }

    /// Drop what is out of time. An expired cell is removed here, so
    /// [`Ribbon::at_rest`] can answer "is anything visible" without a clock.
    fn retire(&mut self, ctx: &Ctx<'_>) {
        let now = ctx.now;
        let ember_done = self
            .ember_at
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() >= FOCUS_EMBER_S);
        let curtain_done = self
            .curtain
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() >= CURTAIN_S);
        if ember_done || curtain_done {
            // The ember and curtain clocks go with the pool: left standing,
            // the deadline read would keep offering a past instant, a wake
            // with nothing on glass. The next key lays afresh.
            self.cells.clear();
            self.cohorts.clear();
            self.ember_at = None;
            self.curtain = None;
            return;
        }
        let cohorts = &self.cohorts;
        self.cells.retain(|cell| {
            // A cell leaves with its cohort.
            let Some(coh) = cohorts.iter().find(|c| c.id == cell.cohort) else {
                return false;
            };
            // A flowing band's expiry is frozen at the fold
            // ([`Ribbon::env_of`]): its cells leave with the flow's phase,
            // below, never on their own life. Otherwise expiry runs from the
            // cohort's clock ([`live_since`]).
            if coh.flow.is_none()
                && now
                    .saturating_duration_since(cell.born.max(coh.alive_at))
                    .as_secs_f32()
                    >= cell.life_s
            {
                return false;
            }
            if let Some(at) = cell.retract_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETRACT_FADE_S
            {
                return false;
            }
            if let Some(at) = cell.retire_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETIRE_MELT_S
            {
                return false;
            }
            coh.phase_at(coh.idle_s(now)).is_some()
        });
        let cells = &self.cells;
        // The walk a cohort leaves when it goes, so a same-row rebirth can
        // continue it (`join_cohort`).
        if let Some(gone) = freshest(
            self.cohorts
                .iter()
                .filter(|coh| !cells.iter().any(|c| c.cohort == coh.id)),
        ) {
            self.last_walk = Some(LastWalk {
                end: gone.walk_at(gone.col1),
                alive_at: gone.alive_at,
            });
        }
        self.cohorts.retain_mut(|coh| {
            if coh.flow.is_none() {
                return cells.iter().any(|cell| cell.cohort == coh.id);
            }
            let (resident, lo, hi) = cells.iter().filter(|cell| cell.cohort == coh.id).fold(
                (false, u16::MAX, 0u16),
                |(_, lo, hi), cell| {
                    if cell.leaving() {
                        (true, lo, hi)
                    } else {
                        (true, lo.min(cell.col), hi.max(cell.col))
                    }
                },
            );
            if lo > hi {
                // A wholly leaving cohort keeps its existing fade until its
                // last resident cell retires.
                return resident;
            }
            // A rewrap can remove the word after the flow froze its edge.
            // Keep the original drain direction, but never pull the surviving
            // text toward columns whose light is already leaving.
            if let Some(edge) = &mut coh.retract_col {
                *edge = (*edge).clamp(lo, hi);
                coh.flow_drift = coh
                    .flow_drift
                    .min(FLOW_DRIFT_MAX_SHARE * (f32::from(hi - lo) + 1.0));
            }
            true
        });
    }

    /// The plan and the index, from one sort.
    fn build(&mut self, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        self.plan.clear();
        self.runs.clear();
        // What the block leaves behind drains from the level it showed: a leaving cell the
        // attach floored last frame that is no longer under the caret (a held Backspace moves
        // the block left one cell per key) has been spending under the floor, so uncovering it
        // would drop it in one frame; its retract restarts at the uncovering. A retract not yet
        // begun, a content retirement and a cell a fresh lay shadows are left alone.
        if let Some((row, col)) = self.attach
            && ctx.caret != (row, col)
            && !self
                .cells
                .iter()
                .any(|c| c.row == row && c.col == col && !c.leaving())
        {
            for c in &mut self.cells {
                if c.row == row
                    && c.col == col
                    && let Some(at) = c.retract_at
                    && at < ctx.now
                {
                    c.retract_at = Some(ctx.now);
                }
            }
        }
        self.attach = None;
        self.attach_floor = None;
        self.index.begin(cols);
        if self.cells.is_empty() {
            return;
        }
        // One sort, newest first: `row` and `col` ascending, then pool index
        // descending. The pool is append-ordered, so the first entry of a
        // `(row, col)` group is its owner, and both the index (first writer
        // wins) and the geometry (`dedup_by` keeps the first) take it.
        let mut sorted = mem::take(&mut self.sorted);
        sorted.clear();
        sorted.reserve(self.cells.len());
        for (i, c) in self.cells.iter().enumerate() {
            sorted.push((c.row, c.col, u32::try_from(i).unwrap_or(u32::MAX)));
        }
        let now = ctx.now;
        let cells = &self.cells;
        // The position's owner, by rank (the layers, K1): a born `Over` cell, then the newest
        // `Base` cell, then an unborn `Over` cell (flight clock), which leaves the band
        // beneath on glass until it is born.
        let rank = |i: u32| -> u8 {
            match cells.get(i as usize) {
                Some(c) if c.layer == Layer::Over => u8::from(now < c.born) * 2,
                _ => 1,
            }
        };
        sorted.sort_unstable_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(rank(a.2).cmp(&rank(b.2)))
                .then(b.2.cmp(&a.2))
        });
        for &(row, col, idx) in &sorted {
            let Some(cell) = self.cells.get(idx as usize) else {
                continue;
            };
            self.index.set(row, col, cell.t);
        }
        // Keep only the owner of each cell for the geometry pass, so a run is
        // one polyline and not two stacked on the same columns.
        sorted.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        self.slabs = Self::slabs_for(ctx, sorted.len());
        self.build_runs(ctx, &sorted);
        self.sorted = sorted;
        // Emit order: the hand's run first, then newest first. The budget
        // sheds from the end of the emit, so a hot wrapped paragraph keeps its
        // current row lit and loses the oldest row's far end.
        self.runs
            .sort_unstable_by(|a, b| b.at_caret.cmp(&a.at_caret).then(b.born.cmp(&a.born)));
    }

    /// The vertex density the budget allows this frame (§18), from the
    /// frame's total live cells across every row. The profile costs one quad
    /// per device row of every slab and `ribbon_beam` tiles at least one slab
    /// per vertex pair, so the cost is `cells × slabs × rows` and `slabs` is
    /// the term that gives. Sized on the total so two hot rows fit the same
    /// budget as one.
    #[inline]
    fn slabs_for(ctx: &Ctx<'_>, cells: usize) -> usize {
        let prof = Self::body_profile(ctx.cfg);
        let ch = ctx.geom.ch as f32;
        // The estimate reads the settled reach below the spine (`DN_TOP_CH`), not the comet's
        // lobe at the hand: the lobe spans `BLOOM_REACH_CELLS` of one run, and
        // `ribbon_beam`'s own truncation is the backstop for the rows an estimate misses.
        let body = ((prof.up_ch + DN_TOP_CH) * ch).ceil().max(1.0) + 2.0;
        // The rail is a second polyline over the same slabs, one quad per
        // device row of its reach.
        let rail = if Self::rail_lit(ctx.cfg) {
            (DN_TOP_CH * ch).ceil() + 1.0
        } else {
            0.0
        };
        let rows = body + rail;
        // The attach: the run at the hand continues one cell under the caret
        // (`plan_run`), one more cell of slabs to price.
        let cells = cells + 1;
        let per_slab = (cells as f32 * rows).max(1.0);
        let fit = (RIBBON_QUAD_BUDGET as f32 / per_slab).floor();
        (fit.max(1.0) as usize).clamp(1, SLABS_PER_CELL)
    }

    /// Walk the deduped index into contiguous runs — contiguous in column and
    /// in cohort — and plan each one.
    fn build_runs(&mut self, ctx: &Ctx<'_>, sorted: &[(u16, u16, u32)]) {
        fn cohort_of(cells: &[Cell], e: &(u16, u16, u32)) -> Option<u32> {
            cells.get(e.2 as usize).map(|c| c.cohort)
        }
        let mut s = 0usize;
        while s < sorted.len() {
            let mut e = s;
            while e + 1 < sorted.len()
                && sorted[e + 1].0 == sorted[s].0
                && sorted[e + 1].1 == sorted[e].1 + 1
                && cohort_of(&self.cells, &sorted[e + 1]) == cohort_of(&self.cells, &sorted[e])
            {
                e += 1;
            }
            // The run's neighbours: where another run abuts this one on its
            // row, the two share their seam vertex. The left run was planned
            // first (`sorted` is row-major), so its last vertex becomes this
            // run's first, and the seam carries the older side's light and
            // shape rather than a fresh feather (`plan_run`).
            let row = sorted[s].0;
            let nb = |i: Option<usize>, col: Option<u16>| -> Option<Cell> {
                let (i, col) = (i?, col?);
                let e = sorted.get(i)?;
                (e.0 == row && e.1 == col).then(|| self.cells.get(e.2 as usize).copied())?
            };
            let abuts_left = nb(s.checked_sub(1), sorted[s].1.checked_sub(1)).is_some();
            let seam = if abuts_left {
                self.runs
                    .last()
                    .filter(|r| r.row == row && r.col1.checked_add(1) == Some(sorted[s].1))
                    .and_then(|r| self.plan.get(r.hi.checked_sub(1)?).copied())
            } else {
                None
            };
            let right = nb(Some(e + 1), sorted[e].1.checked_add(1));
            self.plan_run(ctx, &sorted[s..=e], seam, right);
            s = e + 1;
        }
    }

    /// The per-cell sample every boundary is the midpoint of: `(spine, up,
    /// dn, cov)`. The plateau is a constant of [`BodyProfile`] and is not
    /// sampled per cell (§18). `coh` is `cell`'s cohort, looked up once per
    /// run by [`Ribbon::plan_run`].
    fn sample(
        &self,
        ctx: &Ctx<'_>,
        cell: &Cell,
        coh: Option<&Cohort>,
        sp: f32,
    ) -> (f32, f32, f32, f32) {
        let chf = ctx.geom.ch as f32;
        let prof = Self::body_profile(ctx.cfg);
        let reduced = ctx.cfg.reduced_motion;
        // The wave rides the live spine and only settles: amplitude
        // `WAVE_AMP_CELLS · ch · disp`, so the mark flattens when the hand
        // lifts. The head is pinned (`WAVE_HEAD_PIN`) so the cell under the
        // hand never moves. No wave under reduced motion (§6.11).
        let room = (DN_TOP_CH - DN_FLOOR_CH) * chf;
        // The settle: a flowing band thins from its top toward the seam with
        // the new line and its wave calms, by the drift's progress
        // ([`Cohort::flow_progress`]). Shape only, it can only shrink the
        // covered area, and the body never leaves its row. Static under
        // reduced motion.
        let (settle, breath) = if reduced {
            (0.0, 0.0)
        } else {
            coh.filter(|c| c.flow.is_some() && c.phase.is_retracting())
                .map_or((0.0, 0.0), |c| {
                    let idle = c.idle_s(ctx.now);
                    (
                        c.flow_progress(idle),
                        smoothstep01((idle - (FLOW_TOTAL_S - FLOW_TAIL_S)) / FLOW_TAIL_S),
                    )
                })
        };
        let amp = if reduced {
            0.0
        } else {
            (WAVE_AMP_CELLS * chf * clamp01(ctx.disp)).min(room)
                * smoothstep01(sp / WAVE_HEAD_PIN)
                * (1.0 - settle)
        };
        let wave = if amp == 0.0 {
            0.0
        } else {
            ((sp * std::f32::consts::TAU * WAVE_CYCLES) - ctx.phase * std::f32::consts::TAU).sin()
                * amp
        };
        // The wedge: fatter under the hand, settling into the leading behind
        // it. Shape, not light. Under the comet the wedge opens to
        // `prof.dn_ch` (`COMET_DN_HAND_CH`) at the hand, the lower lobe the
        // rail fills. `sp · span` is how many cells behind the head this one
        // is, the wedge's coordinate.
        let span = coh.map_or(1.0, |c| f32::from(c.col1.saturating_sub(c.col0)).max(1.0));
        let behind = sp * span;
        let bloom = clamp01(ctx.disp) * (1.0 - smoothstep01(behind / BLOOM_REACH_CELLS));
        // The lobe needs the leading below the row. The grid's last row has
        // none (its spine is clamped up by `dn`), so it keeps the flat wedge.
        let pane_bot = self.pane_bot(ctx, chf);
        let row_bot = f32::from(ctx.geom.origin_y) + (f32::from(cell.row) + 1.0) * chf;
        let room_below = pane_bot - row_bot;
        let last_row = room_below < prof.dn_ch * chf;
        let dn_ch = if last_row { DN_TOP_CH } else { prof.dn_ch };
        let dn_open = chf * (DN_FLOOR_CH + (dn_ch - DN_FLOOR_CH) * clamp01(bloom));
        let dn = (dn_open - wave.max(0.0)).max(DN_FLOOR_CH * chf);
        let dn = (DN_FLOOR_CH * chf
            + (dn - DN_FLOOR_CH * chf) * (1.0 - FLOW_SETTLE_SHARE * settle))
            * (1.0 - breath);
        // The comet's taper: the reach above the spine thins from the head's
        // full `up` to `COMET_UP_TAIL_CH / TALL_UP_CH` of it over
        // `COMET_TAPER_CELLS`, so the hand reads as the origin of the light.
        // The head cell keeps the full body; shape, not light. The flat
        // spelling and the grid's last row (no lobe or rail there) keep the
        // same `up` at every cell.
        let up_ch = if Self::comet(ctx.cfg) && !last_row {
            prof.up_ch
                * (1.0
                    - (1.0 - COMET_UP_TAIL_CH / TALL_UP_CH)
                        * smoothstep01(behind / COMET_TAPER_CELLS))
        } else {
            prof.up_ch
        };
        let up = (up_ch * chf + wave.min(0.0)).max(0.0)
            * (1.0 - FLOW_SETTLE_SHARE * settle)
            * (1.0 - breath);
        let spine = (row_bot + wave)
            .min(pane_bot - dn)
            .max(ctx.geom.fx_top() as f32);
        (spine, up, dn, self.cov_of(ctx, cell, coh))
    }

    /// The cell's envelope, `0..1`: everything that opens and closes a cell's light after its
    /// birth price (the `edge-in`, the expiry melt, the retract's `spend`, the content melt,
    /// the curtain, the focus ember and its rearm, the swoosh's farthest-first drain). The one
    /// clock the body ([`Ribbon::cov_of`]) and the hot edge ([`Ribbon::emit_hot_edge`]) share.
    /// Except for the two sanctioned attacks (`edge-in` at birth and on focus regain) every
    /// term is non-increasing in `now`, so nothing brightens after the last keystroke.
    #[inline]
    fn env_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        self.env_with(ctx, cell, self.cohorts.iter().find(|c| c.id == cell.cohort))
    }

    /// [`Ribbon::env_of`] with `cell`'s cohort already looked up — the form
    /// the plan reads, which has it once per run.
    #[inline]
    fn env_with(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        let now = ctx.now;
        // A wake cell laid ahead of its birth (the flight clock) is dark until it is born:
        // the attack starts at `born`, never before it.
        if now < cell.born {
            return 0.0;
        }
        let reduced = ctx.cfg.reduced_motion;
        // The attack runs from its visual origin (a live re-wet keeps it across a fresh
        // identity), expiry from the cohort's clock; a retired cell's attack is frozen at
        // `retire_at`, so it never climbs under the melt.
        let attack_now = cell.retire_at.map_or(now, |at| now.min(at));
        let age = attack_now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        // A flowing band's expiry is frozen at the fold: the flow is its whole exit.
        let spent_to = coh.and_then(|c| c.flow).map_or(now, |f| now.min(f));
        let spent = spent_to
            .saturating_duration_since(coh.map_or(cell.born, |c| cell.born.max(c.alive_at)))
            .as_secs_f32();
        let u = (spent / cell.life_s.max(1e-3)).clamp(0.0, 1.0);
        // Under reduced motion every mark is static and takes the theme's one linear fade
        // over the last `REDUCED_MOTION_FADE_MS` of whatever ends it (§6.11); the attack
        // stays, being no motion. The attack: from dark through `edge-in` at birth, or for a
        // re-armed cell (`Cell::rearm`, M1) from exactly that level through the same ramp.
        let attack = match cell.rearm {
            Some((re_at, level)) => {
                let r = attack_now.saturating_duration_since(re_at).as_secs_f32();
                level + (1.0 - level) * edge_in(r)
            }
            None => BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age),
        };
        let mut env = attack
            * if reduced {
                reduced_fade(cell.life_s - spent)
            } else {
                expiry_melt(u)
            };
        if let Some(at) = cell.retract_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(RETRACT_FADE_S - r)
            } else {
                spend(clamp01(r / RETRACT_FADE_S))
            };
        }
        if let Some(at) = cell.retire_at {
            // The content retirement: the retract's `spend` over `RETIRE_MELT_S`.
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(RETIRE_MELT_S - r)
            } else {
                spend(clamp01(r / RETIRE_MELT_S))
            };
        }
        if let Some(at) = self.curtain {
            // The curtain: the retract's `spend`, farthest first ([`Cohort::drain_rank`]),
            // staggered over [`CURTAIN_STAGGER_S`] with each cell spending
            // [`CURTAIN_SPEND_S`], so the band gathers into the hand and is dark at
            // `CURTAIN_S`.
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(CURTAIN_S - r)
            } else {
                let rank = coh.map_or(1.0, |c| c.drain_rank(cell.col));
                let t0 = CURTAIN_STAGGER_S * (1.0 - rank);
                spend(clamp01((r - t0) / CURTAIN_SPEND_S))
            };
        }
        if let Some(at) = coh.and_then(|c| c.printed) {
            // A printed line ([`Cohort::printed`]): capped toward its share,
            // never brightening, its exit still the swoosh below.
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= 1.0 - (1.0 - RETURN_PRINTED_SHARE) * smoothstep01(r / RETURN_DIM_S);
        }
        if let Some(coh) = coh
            && coh.phase.is_retracting()
        {
            let idle = coh.idle_s(now);
            if reduced {
                env *= reduced_fade(coh.swoosh_total() - idle);
            } else {
                // Farthest from the hand first ([`Cohort::drain_rank`]), each cell
                // taking `RETRACT_FADE_S`, so the mark is drawn back into the hand.
                if coh.flow.is_some() {
                    // A flowing band fades far end first to a common end
                    // ([`Cohort::flow_drain`]), each stop through its own colour floor
                    // ([`ember_level`], §24), then thins to nothing ([`Ribbon::sample`]).
                    env *= ember_level(
                        self.ember_floor(ctx, cell, env),
                        coh.flow_drain(cell.col, idle),
                    );
                } else {
                    let t0 =
                        coh.retract_start() + coh.retract_span() * (1.0 - coh.drain_rank(cell.col));
                    env *= spend(clamp01((idle - t0) / RETRACT_FADE_S));
                }
            }
        }
        // The ember and its rearm run last: the floor is priced against the light the cell
        // has, or a partly melted cell would reach it before its neighbours.
        if let Some(at) = self.ember_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            let f = if reduced {
                reduced_fade(FOCUS_EMBER_S - r)
            } else {
                spend(clamp01(r / FOCUS_EMBER_S))
            };
            env *= ember_level(self.ember_floor(ctx, cell, env), f);
        }
        if let Some((at, fade)) = self.rearm {
            // Focus regained mid-ember: back up from each stop's own level on the sanctioned
            // attack (the stamp carries the common fade, not a level).
            let level = ember_level(self.ember_floor(ctx, cell, env), fade);
            let r = attack_now.saturating_duration_since(at).as_secs_f32();
            env *= level + (1.0 - level) * edge_in(r);
        }
        env
    }

    /// The cell's coverage: its birth-priced ceiling ([`Cell::cov0`]) closed by
    /// [`Ribbon::env_of`] and scaled by the host's intensity under the bed's one cap (§3.4).
    #[inline]
    fn cov_of(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        let cap = cov_cap(ctx.cfg);
        (cap * cell.cov0 * self.env_with(ctx, cell, coh) * clamp01(ctx.cfg.intensity))
            .clamp(0.0, cap)
    }

    /// The ember's floor for this cell, as a share of the ember's own factor: the stop's
    /// extinction alpha ([`BedInkLut::seen_floor_at`] × [`EMBER_FLOOR_MARGIN`]) over the
    /// coverage the cell has with every other factor spent (`carried`); `0` with nothing to
    /// spend or an unsynced table.
    #[inline]
    fn ember_floor(&self, ctx: &Ctx<'_>, cell: &Cell, carried: f32) -> f32 {
        self.ember_floor_of(
            cov_cap(ctx.cfg) * cell.cov0 * clamp01(ctx.cfg.intensity) * carried,
            cell.t,
        )
    }

    /// [`Ribbon::ember_floor`] against a coverage already priced: the form
    /// the cadence fold reads, which has a peak and a walk position and no
    /// [`Config`].
    #[inline]
    fn ember_floor_of(&self, full_cov: f32, t: f32) -> f32 {
        if !full_cov.is_finite() || full_cov <= 0.0 {
            return 0.0;
        }
        // Not clamped to 1: a floor over the cell's full coverage means "hold
        // full through the dim" and [`ember_level`]'s clamp says so; clamping
        // here would take the cell out of the common go-out.
        (self.ink.seen_floor_at(t) * EMBER_FLOOR_MARGIN / full_cov).max(0.0)
    }

    /// The from-the-hand attack (`RAINBOW-KITTY-V2.md` §30): the multiplier on a young cell's
    /// coverage at `dist` across it (`0.0` at its caret side, `1.0` at its far side), so its
    /// light enters as a wipe from the hand instead of fading in place. The cell's level
    /// already carries the uniform attack ([`Ribbon::env_of`]); this is the ratio of the wiped
    /// attack (the same ramp gated by a smoothstep front crossing the cell over
    /// [`ATTACK_WIPE_S`], [`ATTACK_WIPE_FRONT`] wide) to it, and [`Ribbon::plan_run`]
    /// multiplies it into the slab. The floor is never gated, so the echo frame shows the
    /// colour (T2). Exactly `1.0` past the wipe, for a wake cell, before the attack, for the
    /// flat spelling and under reduced motion (§6.11).
    #[inline]
    fn wipe_of(&self, ctx: &Ctx<'_>, cell: &Cell, dist: f32) -> f32 {
        // Typed cells only: a wake's cells share one birth instant, so
        // wiping each from its own edge would print a comb; its lay sweep is
        // its attack. The wipe runs on the cell's visual clock (`attack_at`),
        // not its identity (`born`), so a re-wet cell keeps its wipe.
        if !cell.typing
            || !Self::comet(ctx.cfg)
            || ctx.cfg.reduced_motion
            || ctx.now < cell.attack_at
        {
            return 1.0;
        }
        let age = ctx
            .now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        if age >= ATTACK_WIPE_S {
            return 1.0;
        }
        let ramp = edge_in(age);
        if ramp <= 0.0 {
            return 1.0;
        }
        let front = clamp01(age / ATTACK_WIPE_S) * (1.0 + ATTACK_WIPE_FRONT);
        let gate = smoothstep01((front - clamp01(dist)) / ATTACK_WIPE_FRONT);
        let uniform = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp;
        let wiped = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp * gate;
        wiped / uniform
    }

    /// The run's own retract: every boundary of a retracting cohort is pulled toward the caret
    /// the retract began under ([`Cohort::retract_col`]) on `suck-in`, so the mark shortens
    /// into the hand (T5) and a later jump does not lurch it. Per cohort, never per row; static
    /// under reduced motion (§6.11).
    #[inline]
    fn retract_x(&self, ctx: &Ctx<'_>, coh: Option<&Cohort>, x: f32) -> f32 {
        if ctx.cfg.reduced_motion {
            return x;
        }
        // The curtain draws every boundary into the column its cohort was frozen toward
        // ([`Ribbon::curtain`]), whatever its phase, never [`Ribbon::caret`]: at the
        // alt-screen seam the launching Return has already moved it.
        if let Some(at) = self.curtain {
            let u = clamp01(ctx.now.saturating_duration_since(at).as_secs_f32() / CURTAIN_S);
            let Some(col) = coh.and_then(|c| c.retract_col) else {
                return x;
            };
            let caret_x = f32::from(ctx.geom.origin_x) + f32::from(col) * ctx.geom.cw as f32;
            return caret_x + (x - caret_x) * (1.0 - suck_in(u));
        }
        let Some(coh) = coh.filter(|c| c.phase.is_retracting()) else {
            return x;
        };
        let idle = coh.idle_s(ctx.now);
        if coh.flow.is_some() {
            // The drift: a flowing band slides as one rigid object toward its frozen fold edge
            // ([`Ribbon::flow_target`]), every boundary by the same amount, so each stop keeps
            // its laid pitch (C2); boundaries reaching the edge collapse onto it. A boundary
            // beyond the kept end (cells a re-wrap stamped to leave) keeps its x and fades in
            // place.
            let Some(edge) = coh.retract_col else {
                return x;
            };
            let cw = ctx.geom.cw as f32;
            let ox = f32::from(ctx.geom.origin_x);
            let s = coh.flow_drift * cw * coh.flow_progress(idle);
            return if coh.drain_right_first {
                let start = ox + f32::from(edge) * cw;
                if x < start - 0.5 {
                    x
                } else {
                    (x - s).max(start)
                }
            } else {
                let end = ox + (f32::from(edge) + 1.0) * cw;
                if x > end + 0.5 { x } else { (x + s).min(end) }
            };
        }
        let u = clamp01((idle - coh.retract_start()) / (coh.retract_span() + RETRACT_FADE_S));
        let target = coh.retract_col.or_else(|| self.caret.map(|(_, col)| col));
        // The target is the caret cell's left edge, not its centre: the mark
        // stops at the block rather than creeping into the caret cell.
        let caret_x = target.map_or(x, |col| {
            f32::from(ctx.geom.origin_x) + f32::from(col) * ctx.geom.cw as f32
        });
        caret_x + (x - caret_x) * (1.0 - suck_in(u))
    }

    /// The head of a run (§4.1, §20.1): `(head column, at caret, wet, stream_dir)`.
    ///
    /// The head is the cell under the hand: the caret's own cell (`caret − 1`) when the caret
    /// stands in or just past the run and that cell is live; otherwise the run's newest cell
    /// that is not leaving. A run with every cell draining has its head at its right end and
    /// is not wet, so the hot edge stays off it. When the caret stands on the first cell of a
    /// wake cohort ([`Cohort::wake`], a leftward wake's landing) that cell is the head and the
    /// stream runs toward higher columns (`stream_dir` `+1`); every other branch is `−1`.
    ///
    /// `at caret` is the emit-order key (the run the hand is on goes first): the caret is in or
    /// beside the run. `coh` is the run's cohort.
    #[inline]
    fn head_col(
        &self,
        ctx: &Ctx<'_>,
        run: &[(u16, u16, u32)],
        coh: Option<&Cohort>,
    ) -> (u16, bool, bool, i8) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let (crow, ccol) = ctx.caret;
        let at_caret = crow == row && (col0..=col1 + 1).contains(&ccol);
        let live = |col: u16| -> bool {
            run.get(usize::from(col - col0))
                .and_then(|e| self.cells.get(e.2 as usize))
                .is_some_and(|c| c.col == col && !c.leaving())
        };
        if at_caret
            && let Some(own) = ccol.checked_sub(1)
            && (col0..=col1).contains(&own)
            && live(own)
        {
            return (own, true, true, -1);
        }
        if at_caret && ccol == col0 && live(col0) && coh.is_some_and(|k| k.wake) {
            return (col0, true, true, 1);
        }
        let newest = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| !c.leaving())
            .max_by(|a, b| a.born.cmp(&b.born))
            .map(|c| c.col);
        match newest {
            Some(col) => (col, at_caret, true, -1),
            None => (col1, at_caret, false, -1),
        }
    }

    /// Plan one contiguous run into [`Ribbon::plan`], boundary by boundary, with
    /// [`Ribbon::slabs_per_cell`] vertices per cell.
    ///
    /// `seam` is the last vertex of the run abutting this one on its left and `right_nb` the
    /// other cohort's cell abutting it on its right ([`Ribbon::build_runs`]): the first boundary
    /// is the seam vertex and the last prices the neighbour's light into the same tail-ease
    /// law, so two abutting runs meet at one vertex instead of a feather.
    ///
    /// The attach: a typed run whose head is its last cell, with a live caret one column right
    /// and nothing laid under it, continues one cell past its head boundary with the head's
    /// sample, so body, strip and rail run under the caret cell and the block stands on the
    /// band. It has no key or clock of its own: priced from the head boundary, pulled by its
    /// retract, gone with it. A leaving cell under the caret (an erased or retired one) keeps
    /// its own slabs, floored at the attach's level ([`AttachFloor`]), so the frame it retires
    /// changes no byte. The floor is applied inside the boundary loop, never as extra
    /// segments: two polylines over one x would composite the bed twice.
    fn plan_run(
        &mut self,
        ctx: &Ctx<'_>,
        run: &[(u16, u16, u32)],
        seam: Option<Segment>,
        right_nb: Option<Cell>,
    ) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let Some(cohort) = self.cells.get(run[0].2 as usize).map(|c| c.cohort) else {
            return;
        };
        // A run is contiguous in cohort ([`Ribbon::build_runs`]): its cohort
        // is looked up here, once, for every per-cell read below.
        let coh = self.cohorts.iter().find(|c| c.id == cohort);
        let slabs = self.slabs_per_cell();
        let (head_col, at_caret, wet, stream_dir) = self.head_col(ctx, run, coh);
        let born = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| c.typing)
            .map(|c| c.born)
            .max()
            .unwrap_or(ctx.now);
        let span = f32::from(col1 - col0).max(1.0);
        // The run is contiguous and column-sorted, so a cell is an index, not
        // a search (§18).
        let cell_at = |col: u16| -> Option<&Cell> {
            let i = usize::from(col.checked_sub(col0)?);
            let e = run.get(i)?;
            (e.1 == col).then(|| self.cells.get(e.2 as usize))?
        };
        let cw = ctx.geom.cw as f32;
        // The attach's condition (see the doc above): a typed run wet at the hand, the caret
        // one column right of the head inside the pane, and no live light under the caret.
        let pane_end = self.pane.map_or(
            u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX),
            |(c0, w)| c0.saturating_add(w),
        );
        let caret_cell = head_col.wrapping_add(1);
        let under_caret_leaving = match cell_at(caret_cell) {
            Some(c) => c.leaving(),
            None => right_nb.as_ref().is_some_and(Cell::leaving),
        };
        let caret_free = cell_at(caret_cell).is_none() && right_nb.is_none() || under_caret_leaving;
        let attach = stream_dir < 0
            && wet
            && at_caret
            && ctx.caret.1 == caret_cell
            && ctx.caret.1 < pane_end
            && caret_free;
        // The attach's own attack, on the head cell's birth clock whatever attack the head
        // inherited: the birth floor and edge-in (T2, T3) and the wipe (`wipe_of` is a ratio
        // to the uniform attack, put back here).
        let attack = attach
            .then(|| cell_at(head_col).copied())
            .flatten()
            .map(|head| {
                let probe = Cell {
                    attack_at: head.born,
                    ..head
                };
                let age = ctx.now.saturating_duration_since(head.born).as_secs_f32();
                (
                    BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age),
                    probe,
                )
            });
        if attack.is_some() {
            self.attach = Some((row, caret_cell));
        }
        let lo = self.plan.len();
        let mut prev: Option<(f32, Sample)> = None;
        // A sample depends only on the cell and the frame, so the previous boundary's right
        // sample is this one's left: one sample per live cell.
        let mut left_sample = None;
        // The boundary (index from `col0`) a hard edge doubled, if any (see
        // the block's right edge below); the head index accounts for it.
        let mut doubled: Option<usize> = None;
        let caret_side_right = stream_dir < 0;
        let ox = f32::from(ctx.geom.origin_x);
        let slabs_f = slabs as f32;
        for boundary in u32::from(col0)..=u32::from(col1) + 1 {
            let left = boundary
                .checked_sub(1)
                .and_then(|c| u16::try_from(c).ok())
                .and_then(cell_at);
            let right = u16::try_from(boundary).ok().and_then(cell_at);
            let (a, b) = match (left, right) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => {
                    left_sample = None;
                    continue;
                }
            };
            let sp = |c: &Cell| f32::from(head_col.abs_diff(c.col)) / span;
            debug_assert_eq!(left_sample.is_some(), left.is_some());
            let sa = left_sample.unwrap_or_else(|| self.sample(ctx, a, coh, sp(a)));
            let sb = if a.col == b.col {
                sa
            } else {
                self.sample(ctx, b, coh, sp(b))
            };
            left_sample = right.map(|_| sb);
            // The wipe runs from the cell's caret side: the right edge on a typed run, the left
            // on a leftward wake.
            let mid = |u: f32, v: f32| (u + v) * 0.5;
            // The brighter side owns the boundary: shape and colour are the two cells'
            // midpoint, coverage the brighter cell's, and the dimmer reaches its own level
            // inside itself; a midpoint coverage would sag a settled cell while a neighbour
            // spends. The right side enters at [`RUN_TAIL_EASE`], so an absent side leaves a
            // one-cell feather and a leaving neighbour hands it over as fast as its light goes.
            // At a seam the neighbour's vertex (left) or light (right) enters the same law.
            // The boundary is `a`'s right edge and `b`'s left edge.
            let mut la = left.map_or(0.0, |_| {
                sa.3 * self.wipe_of(ctx, a, if caret_side_right { 0.0 } else { 1.0 })
            });
            let mut lb = right.map_or(0.0, |_| {
                sb.3 * self.wipe_of(ctx, b, if caret_side_right { 1.0 } else { 0.0 })
            });
            let mut shape = (
                mid(sa.0, sb.0),
                mid(sa.1, sb.1),
                mid(sa.2, sb.2),
                mid(a.t, b.t),
            );
            if left.is_none()
                && let Some(seam) = seam
            {
                la = f32::from(seam.cov);
                shape = (seam.spine, seam.up, seam.dn, seam.t);
            }
            if right.is_none()
                && let Some(nb) = right_nb.as_ref()
            {
                // This boundary is `nb`'s left edge whichever way this run streams, so the
                // neighbour's wipe is read from its own left edge.
                let nb_coh = self.cohorts.iter().find(|c| c.id == nb.cohort);
                lb = self.cov_of(ctx, nb, nb_coh) * self.wipe_of(ctx, nb, 1.0);
            }
            // The same expression on every run, whichever way it streams: the stream
            // direction follows the caret every frame, so mirroring it for a leftward wake
            // would let a moving caret flip which end is soft and jump a settled boundary.
            let natural = la.max(lb * RUN_TAIL_EASE);
            // The block's right edge is a hard edge: a leaving caret cell's far vertex is
            // floored (`AttachFloor`), and where the band continues the same x carries a second
            // vertex at the natural light, so the next cell lerps from its own seam
            // (`ribbon_beam` skips the zero-length span between).
            let floored = left
                .and_then(|l| self.attach_floor_at(ctx, row, l, 1.0))
                .map(|floor| natural.max(floor));
            let cov = floored.unwrap_or(natural);
            let here = (shape.0, shape.1, shape.2, shape.3, cov);
            // The attach's level is the head boundary's: from here on the
            // caret cell's slabs (this run's or the neighbour's) and the
            // attach's own segments read it.
            if let Some((uniform, probe)) = attack
                && boundary == u32::from(caret_cell)
            {
                self.attach_floor = Some(AttachFloor {
                    row,
                    col: caret_cell,
                    base: cov,
                    uniform,
                    probe,
                });
            }
            let x = ox + boundary as f32 * cw;
            if let Some((px, p)) = prev {
                // The cell between `prev` and `here` is `a`: its interior never
                // exceeds its own coverage, so a transition between two levels
                // lives inside the dimmer cell, and inside a young cell that
                // level is the wipe's at the slab.
                for j in 1..slabs {
                    let f = j as f32 / slabs_f;
                    let l = |u: f32, v: f32| u + (v - u) * f;
                    let own =
                        sa.3 * self.wipe_of(ctx, a, if caret_side_right { 1.0 - f } else { f });
                    let mut cov = l(p.4, here.4).min(own);
                    // The floor inside a leaving caret cell (`AttachFloor`):
                    // `f` runs from the head's edge, the attach's own wipe
                    // coordinate.
                    if let Some(floor) = self.attach_floor_at(ctx, row, a, f) {
                        cov = cov.max(floor);
                    }
                    self.plan.push(Segment {
                        // Rounded to the pixel lattice: `ribbon_beam` tiles
                        // half-open at `ceil`, and a fractional interior vertex
                        // would let two segments co-own a column, composited
                        // twice by the source-over bed.
                        x: self.retract_x(ctx, coh, l(px, x)).round(),
                        spine: l(p.0, here.0),
                        up: l(p.1, here.1),
                        dn: l(p.2, here.2),
                        t: l(p.3, here.3),
                        cov: cov.clamp(0.0, 255.0) as u8,
                    });
                }
            }
            self.plan.push(Segment {
                x: self.retract_x(ctx, coh, x).round(),
                spine: here.0,
                up: here.1,
                dn: here.2,
                t: here.3,
                cov: here.4.clamp(0.0, 255.0) as u8,
            });
            let continues =
                right.is_some() || (boundary == u32::from(col1) + 1 && right_nb.is_some());
            let onward = if floored.is_some() && continues {
                doubled = Some((boundary - u32::from(col0)) as usize);
                let seam = (shape.0, shape.1, shape.2, shape.3, natural);
                self.plan.push(Segment {
                    x: self.retract_x(ctx, coh, x).round(),
                    spine: seam.0,
                    up: seam.1,
                    dn: seam.2,
                    t: seam.3,
                    cov: seam.4.clamp(0.0, 255.0) as u8,
                });
                seam
            } else {
                here
            };
            prev = Some((x, onward));
        }
        // The attach's own segments: one cell on under a live caret, the head
        // boundary's sample at every slab, only where nothing is laid under
        // the caret. A leaving cell there keeps its own slabs, floored above.
        if attach
            && !under_caret_leaving
            && let Some((px, p)) = prev
            && let Some(af) = self.attach_floor
        {
            let x = px + cw;
            for j in 1..=slabs {
                let f = j as f32 / slabs_f;
                let cov = af.level(self, ctx, f);
                self.plan.push(Segment {
                    x: self.retract_x(ctx, coh, px + (x - px) * f).round(),
                    spine: p.0,
                    up: p.1,
                    dn: p.2,
                    t: p.3,
                    cov: cov.clamp(0.0, 255.0) as u8,
                });
            }
        }
        let hi = self.plan.len();
        if hi > lo {
            // The head boundary is the head cell's right edge; boundary `b`
            // sits at `lo + (b − col0) · slabs`. A leftward wake's head is the
            // landing's left edge, the run's first boundary, and its stream
            // runs the other way (`Run::stream_dir`).
            let head_boundary = usize::from(head_col - col0) + 1;
            // A hard edge doubled before the head shifts every later
            // vertex by one (the head itself is live light, never the
            // doubled boundary).
            let shift = usize::from(doubled.is_some_and(|d| d < head_boundary));
            let head = if stream_dir > 0 {
                lo
            } else {
                (lo + head_boundary * slabs + shift).min(hi - 1)
            };
            self.runs.push(Run {
                row,
                col0,
                col1,
                lo,
                hi,
                head_col,
                head,
                wet,
                at_caret,
                born,
                stream_dir,
            });
        }
    }

    /// The attach's floor under `cell` at `dist` across it (`0` at the head's
    /// edge), or `None`. It applies only to a leaving cell at this frame's
    /// attach caret cell ([`AttachFloor`]); a live cell owns its light, and a
    /// leaving cell elsewhere drains normally.
    #[inline]
    fn attach_floor_at(&self, ctx: &Ctx<'_>, row: u16, cell: &Cell, dist: f32) -> Option<f32> {
        let af = self.attach_floor?;
        (af.row == row && af.col == cell.col && cell.leaving()).then(|| af.level(self, ctx, dist))
    }

    // -- emit --------------------------------------------------------------

    /// Draw the body into `frame.under` and the hot edge into `frame.out`; called after the
    /// meteor (§6.5) and before stardust, so a truncation sheds sky first. One `ribbon_beam`
    /// call per run, `GlowBlend::Over` on both grounds (the bed is source-over; the light fork
    /// changes only the ink, L6). On dark themes a second call draws the vivid rail (§30): the
    /// same slabs, its top at `max(spine, row bottom)`, in [`rail_ink`] at the body's coverage.
    /// If the budget cuts a focused-row body behind a rail, the frame is repainted bodies first.
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.ink_quads = 0;
        if self.plan.is_empty() || self.runs.is_empty() {
            return;
        }
        let ink_from = frame.under.len();
        let prof = Self::body_profile(ctx.cfg);
        let shoulder = prof.shoulder;
        let rail = Self::rail_lit(ctx.cfg);
        let chf = ctx.geom.ch as f32;
        let ceiling = if ctx.cfg.dark_theme {
            BODY_FRAME_TOP
        } else {
            light_role(ctx.cfg).alpha_cap()
        };
        let clip = ctx.geom.beam_clip();
        let budget = frame.under.len() + RIBBON_QUAD_BUDGET;
        let core_up = prof.core_up_ch * chf;
        let lift_span = LEAD_CH * chf;
        let mut verts = mem::take(&mut self.verts);
        let runs = mem::take(&mut self.runs);
        // One slab per vertex pair: the plan put a vertex on every slab
        // boundary (`slabs_per_cell`), so the stride is the slab's width.
        let stride = ctx.geom.cw.max(1).div_ceil(self.slabs_per_cell());
        // A row edited in its middle can hold adjacent runs from different
        // cohorts, and a vivid rail can spend quads a later run's body needs.
        // Keep the plain order when it fits; only a capped multi-run row gets
        // a body-first retry.
        let focused_multi_run_row = runs
            .iter()
            .find(|run| run.at_caret)
            .map(|run| run.row)
            .filter(|&row| runs.iter().filter(|run| run.row == row).take(2).count() > 1);
        let pane_bot = self.pane_bot(ctx, chf);
        let dn_floor = DN_FLOOR_CH * chf;
        let rail_reach_max = RAIL_REACH_MAX_CH * chf;
        let plan = &self.plan;
        let ink = &self.ink;
        {
            let mut paint = |out: &mut Vec<GlowQuad>, run: &Run, rail_layer: bool| -> bool {
                let row_bottom = f32::from(ctx.geom.origin_y) + (f32::from(run.row) + 1.0) * chf;
                if rail_layer && (!rail || row_bottom >= pane_bot) {
                    return true;
                }
                verts.clear();
                verts.reserve(run.hi - run.lo);
                // Laid head-first, so a capped run is repainted outward from its head and never
                // drops the light beside the hand.
                if rail_layer {
                    for seg in plan[run.lo..run.hi].iter().rev() {
                        // The vivid rail lies below the row's bottom and uses
                        // the body's own lower reach, capped at the leading.
                        let top = row_bottom.max(seg.spine);
                        let dn = (seg.spine + seg.dn - top).max(dn_floor).min(rail_reach_max);
                        verts.push(RibbonVertex {
                            x: seg.x,
                            spine: top,
                            up: 0.0,
                            dn,
                            core_up: 0.0,
                            core_dn: dn * RAIL_CORE_SHARE,
                            color: ink.rail_at(seg.t),
                            cov: f32::from(seg.cov) * RAIL_GAIN,
                            lift: 0.0,
                            lift_span: 0.0,
                        });
                    }
                } else {
                    for seg in plan[run.lo..run.hi].iter().rev() {
                        // The spine's strip is inside the body's one ceiling.
                        let cov = f32::from(seg.cov);
                        let strip = (cov * STRIP_LIFT_GAIN).min(ceiling - cov).max(0.0);
                        verts.push(RibbonVertex {
                            x: seg.x,
                            spine: seg.spine,
                            up: seg.up,
                            dn: seg.dn,
                            core_up,
                            core_dn: 0.0,
                            color: ink.at(seg.t),
                            cov,
                            lift: strip,
                            lift_span,
                        });
                    }
                }
                let start = out.len();
                let shoulder = if rail_layer { 1.0 } else { shoulder };
                let mut complete =
                    ribbon_beam(out, clip, &verts, shoulder, stride, budget, GlowBlend::Over);
                let head = run.head - run.lo;
                if !complete && head + 1 < verts.len() {
                    out.truncate(start);
                    verts.reverse();
                    complete = Self::repaint_capped_run_from_head(
                        out, clip, &mut verts, head, shoulder, stride, budget,
                    );
                }
                complete
            };
            let mut retry_body_first = false;
            let mut prior_rail_drawn = false;
            for (i, run) in runs.iter().enumerate() {
                if !paint(frame.under, run, false) {
                    retry_body_first =
                        prior_rail_drawn && focused_multi_run_row.is_some_and(|row| run.row == row);
                    break;
                }
                let rail_from = frame.under.len();
                if !paint(frame.under, run, true) {
                    retry_body_first = focused_multi_run_row
                        .is_some_and(|row| runs[i + 1..].iter().any(|later| later.row == row));
                    break;
                }
                prior_rail_drawn |= frame.under.len() > rail_from;
            }
            if retry_body_first {
                let row = focused_multi_run_row.expect("retry requires a multi-run focused row");
                frame.under.truncate(ink_from);
                // Spend body quads across the entire edited row before any of
                // its rail. Saturated frames trade accent length for a
                // continuous body; uncapped bytes never change.
                let bodies_complete = runs
                    .iter()
                    .filter(|run| run.row == row)
                    .all(|run| paint(frame.under, run, false));
                if bodies_complete {
                    let rails_complete = runs
                        .iter()
                        .filter(|run| run.row == row)
                        .all(|run| paint(frame.under, run, true));
                    if rails_complete {
                        for run in runs.iter().filter(|run| run.row != row) {
                            if !paint(frame.under, run, false) || !paint(frame.under, run, true) {
                                break;
                            }
                        }
                    }
                }
            }
        }
        self.runs = runs;
        self.verts = verts;
        self.emit_hot_edge(ctx, frame);
        // What this frame put on the glass, off the frame's own pixels
        // ([`Ribbon::ink_quads`]): one saturating compare per emitted quad,
        // no allocation.
        let bg = ctx.cfg.theme_bg;
        self.ink_quads = frame.under[ink_from..]
            .iter()
            .filter(|q| reads_as_ink(over_premul(bg, q.color, q.alpha)))
            .count();
    }

    /// Retry only a run that the quad budget actually cut, starting at its
    /// head and walking both sides without a synthetic segment between them.
    fn repaint_capped_run_from_head(
        out: &mut Vec<GlowQuad>,
        clip: BeamClip,
        verts: &mut [RibbonVertex],
        head: usize,
        shoulder: f32,
        stride: usize,
        budget: usize,
    ) -> bool {
        debug_assert!(head < verts.len());
        // Both halves share exactly one boundary vertex. They own disjoint
        // pixel slabs, so the head is drawn once while the two paint calls
        // walk outward from it without a bridge across the far ends.
        verts[..=head].reverse();
        let left_complete = ribbon_beam(
            out,
            clip,
            &verts[..=head],
            shoulder,
            stride,
            budget,
            GlowBlend::Over,
        );
        verts[..=head].reverse();
        left_complete
            && ribbon_beam(
                out,
                clip,
                &verts[head..],
                shoulder,
                stride,
                budget,
                GlowBlend::Over,
            )
    }

    /// How many of the last frame's ribbon quads read as band ink on the
    /// glass — `trail status`'s `ribbon_drawn=`. See [`Ribbon::ink_quads`].
    #[inline]
    #[must_use]
    pub fn ink_quad_count(&self) -> usize {
        self.ink_quads
    }

    /// The hot edge's gain, `0..1`, priced from the head cell's birth spine
    /// ([`Cell::birth_disp`]), never the live follower, which would brighten for ~100 ms after
    /// the last key; [`Ribbon::emit_hot_edge`] multiplies in the head cell's envelope. Zero on a
    /// light theme and under reduced motion. `typed` is whether the head cell is a key's: the
    /// floor ([`HOT_EDGE_GAIN_FLOOR`]) is a typed key's alone, so a cold arrow's wake has no
    /// hairline.
    #[inline]
    #[must_use]
    pub fn hot_edge_gain(cfg: &Config, disp: f32, typed: bool) -> f32 {
        if !cfg.dark_theme || cfg.reduced_motion {
            return 0.0;
        }
        let floor = if typed { HOT_EDGE_GAIN_FLOOR } else { 0.0 };
        floor
            + (1.0 - floor) * smoothstep01((clamp01(disp) - HOT_EDGE_DISP_MIN) / HOT_EDGE_DISP_SPAN)
    }

    /// The hot edge (§4.1): a 1-px additive hairline along the body's top edge (`spine − up`,
    /// so it follows the wave), one `comet_beam` vertex per cell boundary from the head back the
    /// head cell's [`Cell::edge_cells`], each the stop under it lifted to
    /// [`HOT_EDGE_LUMA_FLOOR`] ([`BedInkLut::hot_at`]). It has no clock of its own, so it never
    /// lags the head; its gain is the birth-priced [`Ribbon::hot_edge_gain`] times the head
    /// cell's envelope ([`Ribbon::env_of`]), so it never rises after the last key and reaches
    /// zero with the body.
    fn emit_hot_edge(&self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        // The run the hand is on: the caret's run, else the one carrying the
        // newest cell. `runs` is in emit order, which is exactly that order.
        let Some(run) = self.runs.first() else {
            return;
        };
        // Still wet at the hand: the head cell itself, only while it is live
        // light. The gain is priced from that cell's birth, not the run's
        // newest-born cell (after a backspace, the erased one).
        if !run.wet {
            return;
        }
        let Some(head_cell) = self
            .cells
            .iter()
            .filter(|c| c.row == run.row && c.col == run.head_col)
            .max_by(|a, b| a.born.cmp(&b.born))
        else {
            return;
        };
        let gain = Self::hot_edge_gain(ctx.cfg, head_cell.birth_disp, head_cell.typing)
            * self.env_of(ctx, head_cell);
        if gain <= 0.0 {
            return;
        }
        let mouth_x = self.plan[run.head].x;
        // Which way is behind: `−1` (a typed run, a rightward wake) puts the
        // tail at lower x, `+1` (a leftward wake, `Run::stream_dir`) at
        // higher x; `behind` is positive on the tail side either way.
        let dir = f32::from(run.stream_dir);
        // The head cell's own reach ([`edge_cells`]), priced when it was laid:
        // the key that bought a longer streak keeps it while it is the head.
        let cells = head_cell
            .edge_cells
            .clamp(HOT_EDGE_CELLS, HOT_EDGE_CELLS_MAX);
        let cw = ctx.geom.cw as f32;
        // The seam ([`Ribbon::seam`]): a floor of [`SEAM_LEVEL`] under the falloff back to
        // the hand's first column on the row, spent over [`FLOW_TOTAL_S`]; no known floor, no
        // seam.
        let seam = self
            .seam
            .filter(|s| s.row == run.row && run.at_caret && dir < 0.0)
            .and_then(|s| {
                let floor_x =
                    f32::from(ctx.geom.origin_x) + f32::from(self.hand_floor_on(run.row)?) * cw;
                let lvl = SEAM_LEVEL
                    * spend(clamp01(
                        ctx.now.saturating_duration_since(s.at).as_secs_f32() / FLOW_TOTAL_S,
                    ));
                (lvl > 0.0).then_some((floor_x, lvl))
            });
        let reach = cells * cw;
        let seam_lvl = seam.map_or(0.0, |(_, l)| l);
        let intensity = clamp01(ctx.cfg.intensity);
        frame.beams.clear();
        for seg in self.plan[run.lo..run.hi]
            .iter()
            .step_by(self.slabs_per_cell())
        {
            // Only the tail side of the head (see `Run::head`, `stream_dir`):
            // behind the hand, never over the cells it has erased.
            let behind = (mouth_x - seg.x) * (-dir);
            let in_seam = seam.is_some_and(|(floor_x, _)| behind >= 0.0 && seg.x >= floor_x);
            if !(0.0..=reach).contains(&behind) && !in_seam {
                continue;
            }
            let d = behind / cw;
            // `max`, never add: the head's own falloff is byte-identical, and
            // the seam only ever lifts the tail to its level.
            let a = HOT_EDGE_ALPHA * (1.0 - d / cells).max(0.0).powi(2);
            let a_rel = (a / HOT_EDGE_ALPHA).max(if in_seam { seam_lvl } else { 0.0 });
            let cov = (HOT_EDGE_COV_MAX * a_rel * gain * intensity).clamp(0.0, HOT_EDGE_COV_MAX);
            frame.beams.push(BeamVertex {
                x: seg.x,
                y: seg.spine - seg.up,
                color: self.ink.hot_at(seg.t),
                cov: cov as u8,
            });
        }
        if frame.beams.len() < 2 {
            frame.beams.clear();
            return;
        }
        comet_beam(frame.out, ctx.geom.beam_clip(), frame.beams, 1.0, 1, 0.0);
        frame.beams.clear();
    }

    // -- the reads ---------------------------------------------------------

    /// The field at a cell, or `None`.
    #[inline]
    #[must_use]
    pub fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.index.at(row, col)
    }

    /// The walk at a cell: `(d, t_zero)` of the cohort owning `(row, col)` (its newest cell in
    /// pool order), or `None`. `t_zero + walk_t(d + dx)` is the owner's number `dx` cells along,
    /// past the band's ends too: what the landing's jets (`meteor::LandingWalk`) wear. On a
    /// jump's tick the owner may be the corridor's unborn `Over` cell where the field index
    /// reads the `Base` beneath; both agree because a row holds one walk (§45).
    #[inline]
    #[must_use]
    pub fn walk_origin_at(&self, row: u16, col: u16) -> Option<(f32, f32)> {
        let owner = self
            .cells
            .iter()
            .rev()
            .find(|c| c.row == row && c.col == col)?;
        let cohort = self.cohorts.iter().find(|c| c.id == owner.cohort)?;
        Some((cohort.walk_d(col), cohort.walk_zero()))
    }

    /// The caret's position in the field (seam point 4): the caret's own cell when it owns
    /// light, else the newest typed cell on the caret's row, then on any row; red when nothing
    /// is laid. The meteor phase-locks its arc to it (§6.4's `t_land`), so the station under
    /// the caret is the caret's own stop.
    #[inline]
    #[must_use]
    pub fn field_at_caret(&self) -> f32 {
        if let Some(t) = self.caret.and_then(|(row, col)| self.field_at(row, col)) {
            return t;
        }
        let caret_row = self.caret.map(|(row, _)| row);
        let mut best_on_row: Option<(Instant, f32)> = None;
        let mut best_any: Option<(Instant, f32)> = None;
        for c in &self.cells {
            if !c.typing || c.leaving() {
                continue;
            }
            if caret_row == Some(c.row) && best_on_row.is_none_or(|(b, _)| c.born >= b) {
                best_on_row = Some((c.born, c.t));
            }
            if best_any.is_none_or(|(b, _)| c.born >= b) {
                best_any = Some((c.born, c.t));
            }
        }
        best_on_row.or(best_any).map_or(0.0, |(_, t)| t)
    }

    /// The source RGB a companion cursor inherits to meet the laid ribbon with no palette seam
    /// (seam point 5), or `None` before any cursor was observed: on dark themes the authored
    /// stop at the caret's field (the consumer applies its own ink, since [`bed_ink`] would
    /// paint a yellow caret olive), on light themes that stop's caret ink on the page
    /// ([`paper_caret_ink`]).
    #[inline]
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.caret?;
        Some(caret_ink(spectrum(walk_arc(self.field_at_caret())), cfg))
    }

    /// The body's vertical extent at a cell, or `None` where no cell is laid (§4.2, §5.4, D16);
    /// valid after [`Ribbon::plan`]. Stardust's sky band is measured from its `top`, since the
    /// tall body covers the whole cell.
    #[inline]
    #[must_use]
    pub fn band(&self, row: u16, col: u16) -> Option<Band> {
        let run = self
            .runs
            .iter()
            .find(|r| r.row == row && (r.col0..=r.col1).contains(&col))?;
        let slabs = self.slabs_per_cell();
        let idx = run.lo + usize::from(col - run.col0) * slabs + slabs / 2;
        let seg = self.plan.get(idx.min(run.hi.saturating_sub(1)))?;
        Some(Band {
            top: seg.spine - seg.up,
            spine: seg.spine,
            bottom: seg.spine + seg.dn,
        })
    }

    /// Live cells — `Status::cells` and one term of the idle test.
    #[inline]
    #[must_use]
    pub fn live_cells(&self) -> usize {
        self.cells.len()
    }

    /// The planned boundaries lit on the last frame, at or over [`STATUS_LIT_COV`]: what
    /// `trail status` claims as its ribbon (`ribbon_segments=`, `ribbon_hue_bands=`), so no claim
    /// is made for a resident cell melted below what the glass shows.
    #[inline]
    pub fn lit_segments(&self) -> impl Iterator<Item = &Segment> {
        self.plan.iter().filter(|s| s.cov >= STATUS_LIT_COV)
    }

    /// What is on the glass, by plan: every planned boundary whose composite over the theme
    /// ground reads as band ink ([`reads_as_ink`]), not only those over the claim floor
    /// [`Ribbon::lit_segments`] keeps. [`STATUS_LIT_COV`] bounds what the status verb may claim
    /// (it is solved for the dimmest stop), not what is drawn: during a curtain's fall much of
    /// the band is under it yet composited. The host's count is [`Ribbon::ink_quad_count`].
    #[cfg(test)]
    pub fn ink_segments(&self) -> impl Iterator<Item = &Segment> {
        let bg = self.ground;
        let ink = &self.ink;
        self.plan.iter().filter(move |s| {
            if s.cov == 0 {
                return false;
            }
            // The level the spine actually composites, which the census sees:
            // the body's `cov` plus the baseline strip's accent
            // ([`STRIP_LIFT_GAIN`]), capped at the same [`BODY_FRAME_TOP`] the
            // emitter uses.
            let a = (f32::from(s.cov) * (1.0 + STRIP_LIFT_GAIN)).min(BODY_FRAME_TOP) as u8;
            reads_as_ink(over_premul(bg, premul_rgb(ink.at(s.t), a), a))
        })
    }

    /// Milliseconds left in a falling curtain, or `None` when none is falling:
    /// `trail status`'s `ribbon_curtain_ms=`, saying why a band on the glass
    /// is under the claim floor.
    #[inline]
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        let at = self.curtain?;
        let r = now.saturating_duration_since(at).as_secs_f32();
        Some(((CURTAIN_S - r).max(0.0) * 1000.0).round() as u32)
    }

    /// True when nothing is laid. Clockless: an expired cell is removed by [`Ribbon::plan`], so
    /// "laid" and "visible" are one question (§18, seam point 8).
    #[inline]
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.cells.is_empty() && self.cohorts.is_empty()
    }

    /// The cadence law, brisk half: whether per-frame motion is on the mark (a cell or the
    /// focus rearm inside its 18 ms `edge-in`, T3; a cohort retracting into the caret, §4's
    /// 0.40 s; a falling curtain; a live seam). Grace, reach beats and fades are tails. Under
    /// reduced motion nothing moves (§6.11); the attack still ramps.
    #[inline]
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        let ramping = |at: Instant| now.saturating_duration_since(at).as_secs_f32() < EDGE_IN_S;
        // A falling curtain is the retract's own motion, on every cell.
        (!self.reduced && self.curtain.is_some() && !self.cells.is_empty())
            || (!self.reduced && self.seam.is_some() && !self.cells.is_empty())
            || self.cells.iter().any(|c| ramping(c.attack_at))
            || self.rearm.is_some_and(|(at, _)| ramping(at))
            || (!self.reduced
                && self
                    .cohorts
                    .iter()
                    .any(|c| c.phase_at(c.idle_s(now)) == Some(Phase::Retracting)))
    }

    /// Seconds until the mark's settle under a falling spine moves a pixel.
    /// The wave and the wedge read `Ctx::disp` (§4), which the ribbon cannot
    /// see on its own, so the engine asks. The head cell moves fastest: the
    /// wedge closes at `wedge_px · disp / DISP_RELEASE_TAU` px/s and the wave
    /// (amplitude `WAVE_AMP_CELLS·ch·disp`, `PHASE_RATE·disp` cycles/s) at its
    /// crest velocity; one pixel over their sum. `None` with nothing laid or
    /// the spine at rest.
    #[inline]
    #[must_use]
    pub fn settle_step_s(&self, disp: f32) -> Option<f32> {
        if self.cells.is_empty() || !disp.is_finite() || disp <= 0.0 {
            return None;
        }
        let wedge = self.wedge_px * disp / DISP_RELEASE_TAU;
        let wave = if self.reduced {
            0.0
        } else {
            WAVE_AMP_CELLS * self.cell_h * disp * std::f32::consts::TAU * PHASE_RATE * disp
        };
        let rate = wedge + wave;
        if rate > 0.0 { Some(1.0 / rate) } else { None }
    }

    /// The cadence law: the next instant this ribbon changes what is on glass, `None` at rest
    /// so the host can idle at 0 % (T6). Motion is brisk ([`Ribbon::brisk`]); the reach beats,
    /// the retract's first frame and the swoosh's end are edges; every fade is a tail at its
    /// next visible level step ([`level_step_melt`], [`level_step_spend`]) priced at the dark
    /// cap, so the reading is never late. Under reduced motion every fade is the 120 ms linear
    /// one (§6.11).
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
            // Cadence::take discards every edge and tail once the ribbon is
            // brisk, so the per-cell fade curves below are skipped.
            return cad.take();
        }
        let since = |at: Instant| now.saturating_duration_since(at).as_secs_f32();
        let dur = std::time::Duration::from_secs_f32;
        let reduced_fade_s = REDUCED_MOTION_FADE_MS / 1000.0;
        let ember_left = self.ember_at.map(|at| FOCUS_EMBER_S - since(at));
        let ember_u = self.ember_at.map(|at| clamp01(since(at) / FOCUS_EMBER_S));
        let ember_spend = ember_u.map(spend);
        for cell in &self.cells {
            let coh = self.cohorts.iter().find(|c| c.id == cell.cohort);
            // The expiry clock is the cohort's (`live_since`), as `env_of`
            // and `retire` read it.
            let age = since(coh.map_or(cell.born, |c| cell.born.max(c.alive_at)));
            let life = cell.life_s.max(1e-3);
            // A flowing band's expiry is frozen at the fold: its life offers
            // nothing, and the flow keeps the cadence brisk until it ends.
            let flowing = coh.is_some_and(|c| c.flow.is_some());
            if !flowing {
                cad.tail(cell.life_s - age);
            }
            if self.reduced {
                // A flowing cell's life is frozen here too (`env_of`,
                // `retire`): its end is no change on the glass.
                let ends = [
                    (!flowing).then_some(cell.life_s - age),
                    cell.retract_at.map(|at| RETRACT_FADE_S - since(at)),
                    cell.retire_at.map(|at| RETIRE_MELT_S - since(at)),
                    ember_left,
                    coh.map(|c| c.swoosh_total() - c.idle_s(now)),
                ];
                for left in ends.into_iter().flatten() {
                    cad.tail((left - reduced_fade_s).max(0.0));
                }
                continue;
            }
            let base = UNDER_COV_CAP * cell.cov0;
            let u = clamp01(age / life);
            let melt = expiry_melt(u);
            let retract_u = cell
                .retract_at
                .map(|at| clamp01(since(at) / RETRACT_FADE_S));
            let swoosh_u = coh.and_then(|c| {
                let idle = c.idle_s(now);
                if c.phase_at(idle) != Some(Phase::Fading) {
                    return None;
                }
                let t0 = c.retract_start() + c.retract_span() * (1.0 - c.drain_rank(cell.col));
                Some(clamp01((idle - t0) / RETRACT_FADE_S))
            });
            // The content retirement's melt is one more `spend` factor on
            // the same product, and one more ending.
            let retire_u = cell.retire_at.map(|at| clamp01(since(at) / RETIRE_MELT_S));
            let f_r = retract_u.map_or(1.0, spend);
            let f_s = swoosh_u.map_or(1.0, spend);
            let f_m = retire_u.map_or(1.0, spend);
            // The ember is priced against the light already spent, exactly as
            // `env_of` prices it.
            let carried = base * melt * f_r * f_s * f_m;
            let ember_floor = self.ember_floor_of(carried, cell.t);
            let f_e = ember_spend.map_or(1.0, |s| ember_level(ember_floor, s));
            if !flowing && let Some(step) = level_step_melt(base * f_r * f_e * f_s * f_m, u, life) {
                cad.tail(step);
            }
            if let Some(ur) = retract_u
                && let Some(step) =
                    level_step_spend(base * melt * f_e * f_s * f_m, ur, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(ue) = ember_u
                && let Some(step) = level_step_spend(
                    // Only `1 - floor` of the carried peak moves with the
                    // ember now: `ember_level` is affine in the common fade,
                    // so this is that fade's own step, exactly.
                    carried * (1.0 - ember_floor).max(0.0),
                    ue,
                    FOCUS_EMBER_S,
                )
            {
                cad.tail(step);
            }
            if let Some(us) = swoosh_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_m, us, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(um) = retire_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_s, um, RETIRE_MELT_S)
            {
                cad.tail(step);
            }
            if let Some(at) = cell.retract_at {
                cad.tail_at(at);
                cad.tail_at(at + dur(RETRACT_FADE_S));
            }
            if let Some(at) = cell.retire_at {
                cad.tail_at(at + dur(RETIRE_MELT_S));
            }
        }
        for coh in &self.cohorts {
            // Edges: the grace's end, the two beats after it, the retract's first frame and
            // the swoosh's end; the retract's end is a tail. A flowing cohort has no grace.
            if coh.flow.is_some() {
                cad.edge(coh.clock() + dur(coh.swoosh_total()));
                // The lead's end is a motion boundary, and under reduced
                // motion the flow has none: its one fade is the per-cell
                // reduced end.
                if !self.reduced {
                    cad.tail_at(coh.clock() + dur(coh.retract_span()));
                }
                continue;
            }
            let grace = coh.grace_s.max(PHRASE_REST_MIN_S);
            for beat in [
                grace,
                grace + REACH_STEP_S,
                grace + 2.0 * REACH_STEP_S,
                coh.retract_start(),
                coh.swoosh_total(),
            ] {
                cad.edge(coh.alive_at + dur(beat));
            }
            cad.tail_at(coh.alive_at + dur(coh.retract_start() + coh.retract_span()));
        }
        if let Some(at) = self.ember_at {
            cad.tail_at(at + dur(FOCUS_EMBER_S));
        }
        if let Some(at) = self.curtain {
            cad.tail_at(at + dur(CURTAIN_S));
        }
        cad.take()
    }

    /// The band follows its text along the row: the engine half of the shift pass
    /// ([`super::Engine::follow_rows`]). The witness found `cohort`'s cells within `lo..=hi`
    /// under their own glyphs `dc` columns away ([`super::witness::ShiftRun`]: a mid-line insert
    /// pushed the tail right). They move with every clock and price intact, each `t` re-read at
    /// the new column off the unmoved anchor ([`Cohort::t_at`], C2): the colour stays with the
    /// screen column, the light rides the glyph. Moved cells go onto `moved` by old identity and
    /// onto [`Ribbon::followed_now`] by new. Returns how many cells moved.
    pub fn shift_run(
        &mut self,
        run: super::witness::ShiftRun,
        moved: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        let super::witness::ShiftRun {
            row,
            cohort,
            lo,
            hi,
            dc,
        } = run;
        if dc == 0 || lo > hi {
            return 0;
        }
        let Some(ci) = self
            .cohorts
            .iter()
            .position(|c| c.id == cohort && c.row == row)
        else {
            return 0;
        };
        // One cell per column: where part of the run's tail was left behind (a wrap took it
        // off the row), the block lands on columns that tail still holds; those are the light
        // of glyphs that left, so they go. Leaving cells are never in the way: the shift pass
        // reads no shift over light going out.
        let mut i = 0usize;
        while i < self.cells.len() {
            let c = self.cells[i];
            let stale = c.row == row
                && c.cohort == cohort
                && !c.leaving()
                && !(lo..=hi).contains(&c.col)
                && c.col.checked_add_signed(-dc).is_some_and(|src| {
                    (lo..=hi).contains(&src)
                        && self.cells.iter().any(|d| {
                            d.row == row && d.cohort == cohort && !d.leaving() && d.col == src
                        })
                });
            if stale {
                self.cells.remove(i);
            } else {
                i += 1;
            }
        }
        // The stop is re-read at the new column (C2): the text slides under a rainbow that
        // stands still and the band grows by one stop at its right end
        // (`tests/midline_insert_colour_seam.rs`).
        let walk = self.cohorts[ci];
        let mut n = 0usize;
        let (mut c0, mut c1) = (u16::MAX, 0u16);
        for c in &mut self.cells {
            if c.row != row || c.cohort != cohort {
                continue;
            }
            if !c.leaving()
                && (lo..=hi).contains(&c.col)
                && let Some(col) = c.col.checked_add_signed(dc)
            {
                moved.push((c.row, c.col, c.born));
                c.col = col;
                c.t = walk.t_at(col);
                self.followed_now.push((row, col, c.born));
                n += 1;
            }
            c0 = c0.min(c.col);
            c1 = c1.max(c.col.saturating_add(1));
        }
        if n == 0 {
            return 0;
        }
        self.shifted_now.push((row, lo, hi, dc));
        let coh = &mut self.cohorts[ci];
        coh.col0 = c0;
        coh.col1 = c1;
        // A flowing band's frozen fold edge is a glyph's column, so it rides the shift; a
        // shift only lengthens the kept band, so the drift needs no new cap.
        if coh.flow.is_some()
            && let Some(edge) = coh.retract_col
            && (lo..=hi).contains(&edge)
            && let Some(edge) = edge.checked_add_signed(dc)
        {
            coh.retract_col = Some(edge);
        }
        self.index.reset();
        self.plan.clear();
        self.runs.clear();
        n
    }

    /// The band follows its text across rows: the engine half of the follow pass
    /// ([`super::Engine::follow_rows`]). The witness found the run's glyphs `dr` rows away at
    /// their own columns ([`super::witness::FollowRun`]: a box repainted a row higher without a
    /// scroll), so `cohort`'s cells within `lo..=hi` move to `row + dr` with every clock, price
    /// and `t` intact, under a copy of the cohort when only part of it moves. With `away` the
    /// moved run flows ([`Cohort::flow`]). The caret mirror, the pen, `last_space` and the
    /// relocation stay: they are the hand's. Each moved cell's old identity goes onto `moved` for
    /// the witness to re-key. The field index is dropped until the next plan, so
    /// [`Ribbon::field_at_caret`] answers from the newest typed cell meanwhile. Returns how many
    /// cells moved.
    pub fn translate_run(
        &mut self,
        run: super::witness::FollowRun,
        at: Instant,
        away: bool,
        moved: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        let super::witness::FollowRun {
            row,
            cohort,
            lo,
            hi,
            dr,
        } = run;
        let Some(ci) = self
            .cohorts
            .iter()
            .position(|c| c.id == cohort && c.row == row)
        else {
            return 0;
        };
        let Some(new_row) = row.checked_add_signed(dr) else {
            return 0;
        };
        if dr == 0 || lo > hi {
            return 0;
        }
        let mut n = 0usize;
        let mut all = 0usize;
        let (mut c0, mut c1) = (u16::MAX, 0u16);
        let (mut k0, mut k1) = (u16::MAX, 0u16);
        for c in self
            .cells
            .iter()
            .filter(|c| c.row == row && c.cohort == cohort)
        {
            all += 1;
            if (lo..=hi).contains(&c.col) {
                n += 1;
                c0 = c0.min(c.col);
                c1 = c1.max(c.col + 1);
            } else {
                k0 = k0.min(c.col);
                k1 = k1.max(c.col + 1);
            }
        }
        if n == 0 {
            return 0;
        }
        let id = if all == n {
            self.cohorts[ci].row = new_row;
            cohort
        } else {
            let mut copy = self.cohorts[ci];
            copy.id = self.next_cohort;
            self.next_cohort = self.next_cohort.wrapping_add(1);
            copy.row = new_row;
            copy.release_clock = None;
            copy.col0 = c0;
            copy.col1 = c1;
            // A split of a band already flowing keeps its frozen fold edge
            // inside the part it carries and a drift that part can bear
            // ([`FLOW_DRIFT_MAX_SHARE`]).
            if copy.flow.is_some() && c0 < c1 {
                copy.retract_col = copy.retract_col.map(|e| e.clamp(c0, c1 - 1));
                copy.flow_drift = copy
                    .flow_drift
                    .min(FLOW_DRIFT_MAX_SHARE * f32::from(c1 - c0));
            }
            let old = &mut self.cohorts[ci];
            old.col0 = k0;
            old.col1 = k1;
            old.release_clock = None;
            self.cohorts.push(copy);
            if away {
                // Where the split run's walk went ([`SplitOff`]): the
                // relay reads it there once the moved word's cells melt.
                self.split_off = Some(SplitOff {
                    copy: copy.id,
                    dr,
                    col0: k0,
                    col1: k1,
                });
            }
            copy.id
        };
        for c in &mut self.cells {
            if c.row == row && c.cohort == cohort && (lo..=hi).contains(&c.col) {
                moved.push((c.row, c.col, c.born));
                c.row = new_row;
                c.cohort = id;
                self.followed_now.push((new_row, c.col, c.born));
            }
        }
        if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == id) {
            if away {
                Self::flow_cohort(coh, at);
            } else if coh.flow.take().is_some() {
                // Carried back onto the caret's row (the box shrank back under a Backspace
                // through the fold): the row is the hand's again and re-wet.
                coh.retract_col = None;
                coh.flow_drift = 0.0;
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
            }
        }
        // A run carried back onto the seam's row (a composer's unwrap) is the line the
        // input started on, so the continuation is gone. The caret's row never changed, so
        // nothing else clears the seam; left armed, its floor would light line 1's whole
        // tail as the Backspace run reached it.
        if !away && self.seam.is_some_and(|s| s.row == new_row) {
            self.seam = None;
        }
        self.index.reset();
        self.plan.clear();
        self.runs.clear();
        n
    }

    /// A scroll carries the ribbon with its text (seam point 12): every laid cell and
    /// everything addressed by row moves up `rows`; what leaves the grid is dropped, not
    /// clamped (light on a line nobody typed). No cell height is taken: the pixel geometry is
    /// this frame's scratch, dropped here and rebuilt by the next plan. The index and the
    /// caret are translated, not dropped: [`Ribbon::field_at`], [`Ribbon::field_at_caret`] and
    /// [`Ribbon::head_rgb`] may be read between the host's `note_scroll` and its next plan. The
    /// caret is a position and saturates at row 0, as the engine's does.
    pub fn translate_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.cells.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        self.cohorts.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        let cells = &self.cells;
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
        self.cells
            .retain(|c| self.cohorts.iter().any(|coh| coh.id == c.cohort));
        self.caret = self.caret.map(|(row, col)| (row.saturating_sub(rows), col));
        // A pen carried off the top is dropped, not saturated: a saturated pen would let
        // [`Ribbon::typed_landing`] re-home a later key's cells to row 0.
        self.pen = self
            .pen
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.arrived = self
            .arrived
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        // The hand's floor rides with its text; scrolled off, it goes.
        self.hand_floor = self
            .hand_floor
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        // The re-anchor's bookkeeping rides too: a composer at the screen's bottom grows a row
        // up by scrolling in the very repaint that moves the word down.
        self.last_space = self
            .last_space
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.prev_space = self
            .prev_space
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.attach = self
            .attach
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.attach_floor = None;
        self.seam = self.seam.filter(|s| s.row >= rows).map(|s| Seam {
            row: s.row - rows,
            ..s
        });
        self.relocate = self.relocate.filter(|r| r.row >= rows).map(|r| Relocate {
            row: r.row - rows,
            ..r
        });
        // A soft-wrapped word waiting for its reflow is dropped: a scroll between the wrap
        // and its reflow is not a shape the carry handles.
        self.carry = None;
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = proof.from.0.saturating_sub(rows);
            proof.to.0 = proof.to.0.saturating_sub(rows);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = proof.from.0.saturating_sub(rows);
            proof.to.0 = proof.to.0.saturating_sub(rows);
        }
        // The gate's row rides with its text; carried off the top it is dropped.
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = gate.left_row.and_then(|row| row.checked_sub(rows));
        }
        self.index.translate(rows);
        self.plan.clear();
        self.runs.clear();
        // A backward caret from the same repaint is not a stale line
        // ([`Ribbon::scroll_followed`]).
        self.scroll_followed = true;
    }

    /// A row band carries its ribbon with its text (seam point 12): [`Ribbon::translate_scroll`]
    /// for screen rows `top..=bottom` moved by `delta` while every other row stands still (an
    /// inline viewport, a transcript archiving under a fixed composer, `RI×k`, a DECSTBM region
    /// scroll, IL/DL), under [`crate::cursor_glow::band_row`]'s law: what is carried past the
    /// band's edge is dropped, the caret saturates there ([`crate::cursor_glow::band_pos`]).
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.cells
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        self.cohorts
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        let cells = &self.cells;
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
        self.cells
            .retain(|c| self.cohorts.iter().any(|coh| coh.id == c.cohort));
        self.caret = self
            .caret
            .map(|(row, col)| (band_pos(row, top, bottom, delta), col));
        // The pen is the hand: carried past the band's edge it is dropped, as
        // `last_space` is, not saturated onto an edge row.
        self.pen = self
            .pen
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.arrived = self
            .arrived
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        // Left on the old row, the hand's floor would fall to column 0 and the seam would go
        // out for a frame.
        self.hand_floor = self
            .hand_floor
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.last_space = self
            .last_space
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.prev_space = self
            .prev_space
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.attach = self
            .attach
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.attach_floor = None;
        self.seam = self
            .seam
            .and_then(|s| band_row(s.row, top, bottom, delta).map(|row| Seam { row, ..s }));
        self.relocate = self
            .relocate
            .and_then(|r| band_row(r.row, top, bottom, delta).map(|row| Relocate { row, ..r }));
        // …and a soft-wrapped word is dropped, as the whole-grid scroll drops
        // it ([`Ribbon::translate_scroll`]).
        self.carry = None;
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = band_pos(proof.from.0, top, bottom, delta);
            proof.to.0 = band_pos(proof.to.0, top, bottom, delta);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = band_pos(proof.from.0, top, bottom, delta);
            proof.to.0 = band_pos(proof.to.0, top, bottom, delta);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = gate
                .left_row
                .and_then(|row| band_row(row, top, bottom, delta));
        }
        self.index.translate_band(top, bottom, delta);
        self.plan.clear();
        self.runs.clear();
        self.scroll_followed = true;
    }

    /// [`super::Engine::lift_hand`]'s ribbon half: the hand-addressed members on
    /// `row` that [`Ribbon::translate_band`] carries for that row, except the
    /// cells and cohorts (the follow pass carries them with their text) and
    /// the seam ([`Ribbon::lift_seam`]), move to `to`: the caret, the pen, the
    /// arrival, the hand's floor, the spaces, the relocation and the new
    /// line's gate's left row.
    pub(super) fn lift_hand(&mut self, row: u16, to: u16) {
        let map = |r: u16| if r == row { to } else { r };
        self.caret = self.caret.map(|(r, c)| (map(r), c));
        self.pen = self.pen.map(|(r, c)| (map(r), c));
        self.arrived = self.arrived.map(|(r, c)| (map(r), c));
        self.hand_floor = self.hand_floor.map(|(r, c)| (map(r), c));
        self.last_space = self.last_space.map(|(r, c)| (map(r), c));
        self.prev_space = self.prev_space.map(|(r, c)| (map(r), c));
        self.attach = self.attach.map(|(r, c)| (map(r), c));
        self.attach_floor = None;
        self.relocate = self.relocate.map(|r| Relocate {
            row: map(r.row),
            ..r
        });
        if let Some((sw, _)) = &mut self.carry {
            sw.origin.0 = map(sw.origin.0);
            sw.landing.0 = map(sw.landing.0);
        }
        if let Some(proof) = &mut self.short_reanchor {
            proof.from.0 = map(proof.from.0);
            proof.to.0 = map(proof.to.0);
        }
        if let Some(proof) = &mut self.rewrap_start {
            proof.from.0 = map(proof.from.0);
            proof.to.0 = map(proof.to.0);
        }
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = gate.left_row.map(map);
        }
    }

    /// The seam on the hand's lifted line ([`super::Engine::lift_hand`]) rides
    /// from `row` to `to` with it. Applied after the follow pass has carried
    /// the line: the unwrap law ([`Ribbon::translate_run`]) clears a seam
    /// whose row a run lands on with the caret, and the hand's own line is no
    /// unwrap.
    #[inline]
    pub(super) fn lift_seam(&mut self, row: u16, to: u16) {
        if let Some(seam) = &mut self.seam
            && seam.row == row
        {
            seam.row = to;
        }
    }

    /// The seam's row ([`Ribbon::seam`]), for the engine's tests.
    #[cfg(test)]
    pub(super) fn seam_row(&self) -> Option<u16> {
        self.seam.map(|s| s.row)
    }

    /// Drop everything — style switch, layout change, `Engine::reset`.
    #[inline]
    pub fn reset(&mut self) {
        self.cells.clear();
        self.cohorts.clear();
        self.plan.clear();
        self.runs.clear();
        self.sorted.clear();
        self.verts.clear();
        self.index.reset();
        self.caret = None;
        self.ember_at = None;
        self.rearm = None;
        self.clear_space_transient_state();
        self.ink_quads = 0;
        self.last_walk = None;
        self.fresh_line = None;
        self.seam = None;
        // The phrase went with its cells; the hand's tempo and its last key
        // are the hand's, not the coordinate space's, and stay.
        self.phrase_open = None;
        self.curtain = None;
    }

    /// Clear transient pen, carry, re-anchor, and follow state belonging to
    /// the ending coordinate space (shared by [`Ribbon::curtain`] and
    /// [`Ribbon::reset`]).
    #[inline]
    fn clear_space_transient_state(&mut self) {
        self.pending_erase = 0;
        self.pen = None;
        self.arrived = None;
        self.hand_floor = None;
        self.last_space = None;
        self.prev_space = None;
        self.relocate = None;
        self.carry = None;
        self.split_off = None;
        self.short_reanchor = None;
        self.rewrap_start = None;
        self.followed_now.clear();
        self.shifted_now.clear();
        self.scroll_followed = false;
        self.attach = None;
        self.attach_floor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::{Geom, SoundCue};
    use aterm_render::{GlowQuad, RainHalo, over_premul, premul_rgb};
    use std::time::Duration;

    use super::super::{CaretSeam, Dir, TypedClass};

    /// Tokyo Night's dark pair: the fixture's ground (`cfg`) and the default the legibility
    /// tests are stated on.
    const DEFAULT_BG: u32 = 0x001A_1B26;
    const DEFAULT_FG: u32 = 0x00C8_D3F5;
    /// The Nord theme's ground, a pair the gap tests are stated on.
    const NORD_BG: u32 = 0x002E_3440;
    const NORD_FG: u32 = 0x00D8_DEE9;

    /// The ground of `ColorScheme::default` (a config with no `theme` line), the darkest
    /// ground aterm ships.
    const SHIPPED_BG: u32 = 0x0011_1318;
    const SHIPPED_FG: u32 = 0x00D0_D0D0;

    /// The 1× fixture: `cw 9`, `ch 18`, 120 × 40 cells.
    fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 720,
            head: 0,
        }
    }

    /// The 2× (retina) fixture §18's worst case is stated at.
    fn geom2x() -> Geom {
        Geom {
            cw: 18,
            ch: 36,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 2160,
            win_h: 1440,
            head: 0,
        }
    }

    fn cfg(dark: bool, tall: bool) -> Config {
        Config {
            dark_theme: dark,
            intensity: 1.0,
            duration: Duration::from_millis(400),
            ribbon_tall: tall,
            ribbon_flat: false,
            theme_fg: if dark { DEFAULT_FG } else { 0x0016_161C },
            theme_bg: if dark { DEFAULT_BG } else { 0x00FF_FFFF },
            reduced_motion: false,
        }
    }

    /// The `… flat` spelling's config: the body without the comet and its
    /// rail (`Config::ribbon_flat`).
    fn flat(dark: bool, tall: bool) -> Config {
        let mut c = cfg(dark, tall);
        c.ribbon_flat = true;
        c
    }

    fn ctx_in<'a>(now: Instant, cfg: &'a Config, caret: (u16, u16), disp: f32, g: Geom) -> Ctx<'a> {
        Ctx {
            now,
            geom: g,
            cfg,
            disp,
            birth_disp: disp,
            phase: 0.0,
            caret,
            caret_t: 0.0,
            caret_walk: None,
            mend: None,
            surge: 0.0,
            flow: Default::default(),
        }
    }

    fn ctx<'a>(now: Instant, cfg: &'a Config, caret: (u16, u16), disp: f32) -> Ctx<'a> {
        ctx_in(now, cfg, caret, disp, geom())
    }

    #[derive(Default)]
    struct Sink {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Sink {
        fn frame(&mut self) -> Frame<'_> {
            self.under.clear();
            self.out.clear();
            self.halos.clear();
            self.cues.clear();
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
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0.checked_add(Duration::from_millis(ms)).expect("clock")
    }

    fn typed() -> Event {
        Event::Typed {
            cells: 1,
            shifted: false,
            class: TypedClass::Glyph,
        }
    }

    fn nav(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Nav,
            dir: Dir::Right,
        }
    }

    /// A program-driven move (no credit): abandons, lays no wake.
    fn pty(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Pty,
            dir: Dir::Right,
        }
    }

    /// A typing burst: `n` keys on `row` from `col0`, one per `period_ms`,
    /// at spine `disp`, in `g`.
    #[derive(Clone, Copy)]
    struct Keys {
        g: Geom,
        row: u16,
        col0: u16,
        n: u16,
        period_ms: u64,
        disp: f32,
    }

    /// Type `keys` from `t0`, planning after every key as the engine does.
    fn type_keys(rib: &mut Ribbon, t0: Instant, keys: Keys, c: &Config) {
        for i in 0..keys.n {
            let now = at(t0, u64::from(i) * keys.period_ms);
            let caret = (keys.row, keys.col0 + i + 1);
            let cx = ctx_in(now, c, caret, keys.disp, keys.g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
        }
    }

    /// Type `n` cells starting at column `col0` on row 2, one key per 60 ms.
    fn type_run(rib: &mut Ribbon, t0: Instant, col0: u16, n: u16, c: &Config, disp: f32) {
        let keys = Keys {
            g: geom(),
            row: 2,
            col0,
            n,
            period_ms: 60,
            disp,
        };
        type_keys(rib, t0, keys, c);
    }

    /// One Backspace landing the caret at `caret`, planned.
    fn erase_at(rib: &mut Ribbon, now: Instant, caret: (u16, u16), c: &Config) {
        let cx = ctx(now, c, caret, 0.7);
        rib.on_event(&Event::Erase, now, &cx);
        rib.plan(&cx);
    }

    /// True when a body quad lands in the upper half of cell `(row, col)`'s
    /// own band. The tall body covers the whole cell, so an unlit cell is a
    /// shed cell; the upper half, because the row below reaches `0.10 ch`
    /// into this row's bottom and would vouch for a cell that is not there.
    fn cell_lit(under: &[GlowQuad], g: Geom, row: u16, col: u16) -> bool {
        let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
        let x1 = x0 + g.cw as f32;
        let y0 = f32::from(g.origin_y) + f32::from(row) * g.ch as f32;
        let y1 = y0 + g.ch as f32 * 0.5;
        under.iter().any(|q| {
            let qx0 = f32::from(q.x);
            let qx1 = qx0 + f32::from(q.w);
            let qy = f32::from(q.y);
            qx0 < x1 && qx1 > x0 && (y0..y1).contains(&qy)
        })
    }

    /// The device-column census: one entry per device column of `xs`, in
    /// window-absolute px: the topmost device row carrying at least `floor`
    /// levels of light inside the device rows `ys`, the bottommost, and the
    /// greatest level in the column; `None` where no quad reaches the floor.
    /// A source-over quad's level is its opacity byte; an additive quad's
    /// (`alpha == 0`, the hot edge in `out`) is its brightest premultiplied
    /// channel. A per-cell census cannot see a step inside a cell.
    fn column_census(
        quads: &[GlowQuad],
        xs: std::ops::Range<i32>,
        ys: std::ops::Range<i32>,
        floor: u8,
    ) -> Vec<Option<(i32, i32, u8)>> {
        // The level at column `x`: a ribbon slab ramps opacity and colour
        // between its ends (`GlowQuad::color2`, `alpha2`), so the census
        // reads the rasterizer's per-column law (`aterm_render::glow_lerp`).
        let level = |q: &GlowQuad, x: i32| -> u8 {
            let i = (x - i32::from(q.x)) as u32;
            let w = u32::from(q.w);
            if q.alpha > 0 {
                aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, w) as u8
            } else {
                let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, w);
                ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) as u8
            }
        };
        xs.map(|x| {
            let mut top = i32::MAX;
            let mut bot = i32::MIN;
            let mut peak = 0u8;
            for q in quads {
                let qx0 = i32::from(q.x);
                if q.w == 0 || x < qx0 || x >= qx0 + i32::from(q.w) {
                    continue;
                }
                let lv = level(q, x);
                if lv < floor {
                    continue;
                }
                let y = i32::from(q.y);
                if !ys.contains(&y) {
                    continue;
                }
                top = top.min(y);
                bot = bot.max(y + i32::from(q.h) - 1);
                peak = peak.max(lv);
            }
            (top != i32::MAX).then_some((top, bot, peak))
        })
        .collect()
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn peak_channel(quads: &[GlowQuad]) -> u32 {
        quads
            .iter()
            .map(|q| {
                ((q.color >> 16) & 0xff)
                    .max((q.color >> 8) & 0xff)
                    .max(q.color & 0xff)
            })
            .max()
            .unwrap_or(0)
    }

    fn plan_peak(rib: &Ribbon) -> u8 {
        rib.plan_segments().iter().map(|s| s.cov).max().unwrap_or(0)
    }

    fn plan_sum(rib: &Ribbon) -> u32 {
        rib.plan_segments().iter().map(|s| u32::from(s.cov)).sum()
    }

    /// The planned light up to and including the boundary at column `col` on
    /// the 1× fixture: the band's own, without the attach under the caret
    /// cell beyond it (that light leaves with the caret).
    fn band_sum(rib: &Ribbon, col: u16) -> u32 {
        let x = f32::from(col) * geom().cw as f32;
        rib.plan_segments()
            .iter()
            .filter(|s| s.x <= x + 0.5)
            .map(|s| u32::from(s.cov))
            .sum()
    }

    // -- §2.2 L3, §3.2: the bed's one ceiling ------------------------------

    #[test]
    fn letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor() {
        // The net under the guard: 71 foregrounds (38 greys `#909090` to `#FFFFFF` in
        // steps of three, 27 tints from `{A0, D0, FF}³`, five shipped defaults and Nord's)
        // over four grounds, each only where the ground sits under the bed's ceiling (a
        // brighter ground is the theme's contrast, not the bed's), through the ledger's frame
        // top plus one Bayer level. Foregrounds the floor catches (`bed_luma_budget` clamped
        // to `BED_LUMA_MIN`, fg ≤ ≈ `#999999`) are the documented exception and are skipped.
        // `BODY_CONTRAST_GUARD` is the smallest guard this sweep holds at.
        let mut fgs: Vec<u32> = (0x90u32..=0xFF)
            .step_by(3)
            .map(|g| (g << 16) | (g << 8) | g)
            .collect();
        for r in [0xA0u32, 0xD0, 0xFF] {
            for g in [0xA0u32, 0xD0, 0xFF] {
                for b in [0xA0u32, 0xD0, 0xFF] {
                    fgs.push((r << 16) | (g << 8) | b);
                }
            }
        }
        fgs.extend([
            DEFAULT_FG,
            0x00E8_E8F0,
            0x00F8_F8F2,
            0x00AB_B2BF,
            0x00CD_D6F4,
        ]);
        // Nord's pair rides in the sweep too.
        fgs.push(NORD_FG);
        let grounds = [0x0000_0000u32, DEFAULT_BG, 0x0028_2C34, NORD_BG];
        let mut worst = f32::INFINITY;
        let mut at = String::new();
        let mut themes = 0;
        for fg in fgs {
            // Floor-clamped (`BED_LUMA_MIN`) foregrounds are the documented
            // exception and are skipped whole. This and the
            // ground-brighter-than-the-bed skip below are the only two skips.
            let budget = bed_luma_budget(fg);
            if budget <= BED_LUMA_MIN {
                continue;
            }
            themes += 1;
            for bg in grounds {
                if relative_luminance(bg) >= budget {
                    continue;
                }
                for i in 0..=128u32 {
                    let ink = bed_ink(spectrum(i as f32 / 128.0), budget);
                    for cov in 1..=(BODY_FRAME_TOP as u32 + 1) {
                        let cov = cov as u8;
                        let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                        let c = contrast(fg, lit);
                        if c < worst {
                            worst = c;
                            at = format!(
                                "fg #{fg:06X} over #{bg:06X}, t {:.3}, cov {cov}, #{lit:06X}",
                                i as f32 / 128.0
                            );
                        }
                    }
                }
            }
        }
        assert!(
            themes >= 60,
            "the sweep must cover the themes it claims ({themes})"
        );
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the bed composited to {worst:.4}:1 at {at}; the bar is {BODY_CONTRAST_BAR}:1 on every dark theme above the floor"
        );
    }

    /// The shipped light pairs the paper laws are read on: GitHub Light, Aterm Day,
    /// Default Light, Gruvbox Light, Catppuccin Latte and Solarized Light (whose own text
    /// sits under the body's bar).
    const PAPERS: [(u32, u32); 6] = [
        (0x001F_2328, 0x00FF_FFFF),
        (0x0020_2733, 0x00F8_FAFD),
        (0x002B_2D33, 0x00F6_F7F9),
        (0x003C_3836, 0x00FB_F1C7),
        (0x004C_4F69, 0x00EF_F1F5),
        (0x0058_6E75, 0x00FD_F6E3),
    ];

    #[test]
    fn the_paper_bed_keeps_every_stop_its_hue_and_every_letter_its_bar() {
        // At every coverage up to the role's ceiling the letters keep the bar the body
        // promises (the body's, or its share of the theme's own contrast), and at full
        // coverage every stop composites as its own hue: a yellow stays yellow (the
        // luminance law printed `(119, 119, 65)` there) and nothing goes out.
        let cap = InkRole::OverText.alpha_cap() as u32;
        for (fg, bg) in PAPERS {
            let bar = (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD)
                .min(PAPER_BAR_THEME_SHARE * contrast(fg, bg))
                - BODY_CONTRAST_GUARD;
            for i in 0..=128u32 {
                let stop = spectrum(i as f32 / 128.0);
                let ink = paper_ink(stop, fg, bg, InkRole::OverText);
                for cov in 1..=cap {
                    let cov = cov as u8;
                    let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                    let c = contrast(fg, lit);
                    assert!(
                        c >= bar,
                        "#{fg:06X} on #{bg:06X}: stop #{stop:06X} at cov {cov} composites #{lit:06X}, {c:.3}:1 under {bar:.3}:1"
                    );
                }
                let lit = over_premul(bg, premul_rgb(ink, cap as u8), cap as u8);
                assert_ne!(lit, bg, "#{bg:06X}: stop #{stop:06X} went out");
                if bg == 0x00FF_FFFF {
                    let (h, ..) = rgb2hsv(lit);
                    let (h0, ..) = rgb2hsv(stop);
                    let off = (h - h0).abs().min(360.0 - (h - h0).abs());
                    assert!(
                        off <= 4.0,
                        "on white stop #{stop:06X} (hue {h0:.1}) composites #{lit:06X} (hue {h:.1})"
                    );
                }
            }
        }
        let yellow = paper_ink(0x00FF_FF00, PAPERS[0].0, PAPERS[0].1, InkRole::OverText);
        let lit = over_premul(0x00FF_FFFF, premul_rgb(yellow, cap as u8), cap as u8);
        assert!(
            lit & 0x00FF_FF00 == 0x00FF_FF00,
            "yellow on white composites #{lit:06X}, not a yellow"
        );
    }

    #[test]
    fn the_paper_caret_is_its_stop_deepened_and_never_olive() {
        // Every stop's caret clears 3:1 on every paper (the glyph cut out of the block is
        // the page's colour) on a pure hue ray (HSV saturation 1), a stop that already
        // clears it stays pure, and yellow lands on amber, not on its own olive.
        for (_, bg) in PAPERS {
            for i in 0..=128u32 {
                let stop = spectrum(i as f32 / 128.0);
                let caret = paper_caret_ink(stop, bg);
                let c = contrast(caret, bg);
                assert!(
                    c >= PAPER_CARET_CONTRAST - 0.01,
                    "on #{bg:06X} stop #{stop:06X} carets #{caret:06X} at {c:.3}:1"
                );
                let (_, s, _) = rgb2hsv(caret);
                assert!(
                    s >= 0.99,
                    "stop #{stop:06X} carets #{caret:06X}, off its hue ray"
                );
            }
        }
        assert_eq!(paper_caret_ink(0x00FF_0000, 0x00FF_FFFF), 0x00FF_0000);
        assert_eq!(paper_caret_ink(0x0000_00FF, 0x00FF_FFFF), 0x0000_00FF);
        let (h, ..) = rgb2hsv(paper_caret_ink(0x00FF_FF00, 0x00FF_FFFF));
        assert!(
            (35.0..=50.0).contains(&h),
            "yellow's caret sits at hue {h:.1}, not amber"
        );
    }

    // -- §18: the field index ---------------------------------------------

    #[test]
    fn the_field_index_answers_what_a_newest_first_scan_would() {
        // Two rows, driven through the public API only, across frames in
        // which a cell is shadowed (a jump abandons a cohort and the hand
        // retypes over it while it drains), a row goes dark and is relit:
        // the lane prune and re-mint in `FieldIndex::begin`.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let scan_matches = |rib: &Ribbon, ms: u64| {
            for row in 0..6u16 {
                for col in 0..30u16 {
                    let scan = rib
                        .cells()
                        .iter()
                        .rev()
                        .find(|l| l.row == row && l.col == col)
                        .map(|l| l.t);
                    let idx = rib.field_at(row, col);
                    match (scan, idx) {
                        (Some(a), Some(b)) => assert!(
                            (a - b).abs() < 1e-6,
                            "+{ms} ms: index says {b} at ({row},{col}); a newest-first scan says {a}"
                        ),
                        (None, None) => {}
                        (a, b) => {
                            panic!(
                                "+{ms} ms: index and scan disagree at ({row},{col}): {a:?} vs {b:?}"
                            )
                        }
                    }
                }
            }
        };
        let mut script: Vec<(u64, (u16, u16), Event)> = Vec::new();
        // Row 2, cols 10..17.
        for i in 0..8u16 {
            script.push((u64::from(i) * 60, (2, 11 + i), typed()));
        }
        // A jump away and back abandons that cohort; retyping cols 13 and 14
        // lays a new cohort over cells that are still in the pool, draining.
        script.push((500, (2, 40), nav((2, 18), (2, 40))));
        script.push((500, (2, 14), nav((2, 40), (2, 14))));
        script.push((500, (2, 14), typed()));
        script.push((560, (2, 15), typed()));
        // A jump to row 3 abandons row 2 entirely; row 3 is laid.
        script.push((900, (3, 10), nav((2, 15), (3, 10))));
        for i in 0..6u16 {
            script.push((1000 + u64::from(i) * 60, (3, 11 + i), typed()));
        }
        // Row 2 has gone dark by now; relight it.
        script.push((1600, (2, 21), nav((3, 16), (2, 21))));
        for i in 0..3u16 {
            script.push((1600 + u64::from(i) * 60, (2, 21 + i), typed()));
        }
        let mut caret = (2u16, 10u16);
        let (mut saw_shadow, mut saw_dark_row_2, mut saw_relit_row_2) = (false, false, false);
        for ms in (0..=2400u64).step_by(20) {
            let now = at(t0, ms);
            for (_, c_at, ev) in script.iter().filter(|(t, _, _)| *t == ms) {
                caret = *c_at;
                let cx = ctx(now, &c, caret, 0.6);
                rib.on_event(ev, now, &cx);
            }
            let cx = ctx(now, &c, caret, 0.6);
            rib.plan(&cx);
            scan_matches(&rib, ms);
            let cells = rib.cells();
            saw_shadow |= cells.iter().filter(|l| (l.row, l.col) == (2, 13)).count() >= 2;
            let row_2_dark = !cells.is_empty() && cells.iter().all(|l| l.row != 2);
            saw_dark_row_2 |= row_2_dark;
            saw_relit_row_2 |= saw_dark_row_2 && cells.iter().any(|l| l.row == 2);
        }
        assert!(
            saw_shadow,
            "the script must put a genuinely shadowed cell in the pool"
        );
        assert!(saw_dark_row_2, "the script must take row 2 dark");
        assert!(saw_relit_row_2, "…and relight it");
    }

    // -- §4 / C2: the classic walk -----------------------------------------

    /// CIE-Lab distance between two sRGB colours, the unit of the per-cell
    /// pace.
    fn lab_de(a: u32, b: u32) -> f32 {
        let lab = |c: u32| {
            let ch = |sh: u32| ((c >> sh) & 0xff) as f32 / 255.0;
            let lin = |v: f32| {
                if v <= 0.040_45 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            let (r, g, bl) = (lin(ch(16)), lin(ch(8)), lin(ch(0)));
            let (x, y, z) = (
                0.4124 * r + 0.3576 * g + 0.1805 * bl,
                0.2126 * r + 0.7152 * g + 0.0722 * bl,
                0.0193 * r + 0.1192 * g + 0.9505 * bl,
            );
            let f = |t: f32| {
                if t > 0.008_856 {
                    t.cbrt()
                } else {
                    7.787 * t + 16.0 / 116.0
                }
            };
            let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
            (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
        };
        let (l1, a1, b1) = lab(a);
        let (l2, a2, b2) = lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    /// The step the pace equalises: the composited bed under the letters plus
    /// the vivid rail below the row, in equal parts.
    fn arc_step(a: f32, b: f32, budget: f32) -> (f32, f32) {
        let bed = |t: f32| bed_ink(spectrum(t), budget);
        (
            lab_de(bed(a), bed(b)),
            lab_de(rail_ink(spectrum(a)), rail_ink(spectrum(b))),
        )
    }

    /// [`CROSS_PACE`] is a measurement: re-derived here from `bed_ink` and
    /// `rail_ink` by the rule that generated it (equal composited step per
    /// knot across the window), so a change to either ink is caught.
    /// Tolerance is two knots' worth of the window.
    #[test]
    fn the_walk_s_pace_is_the_arc_s_own_perceptual_step() {
        let budget = bed_luma_budget(NORD_FG);
        const N: usize = 4096;
        let (w0, w1) = (CROSS_ARC0, CROSS_ARC1);
        let mut cum = [0.0f32; N + 1];
        for i in 1..=N {
            let a = w0 + (w1 - w0) * (i - 1) as f32 / N as f32;
            let b = w0 + (w1 - w0) * i as f32 / N as f32;
            let (db, dr) = arc_step(a, b, budget);
            cum[i] = cum[i - 1] + (db + dr).max(0.005);
        }
        let total = cum[N];
        let mut j = 0usize;
        let mut worst = 0.0f32;
        for (k, &baked) in CROSS_PACE.iter().enumerate() {
            let want = k as f32 / (CROSS_PACE_KNOTS - 1) as f32 * total;
            while j < N && cum[j + 1] < want {
                j += 1;
            }
            let span = (cum[j + 1] - cum[j]).max(1e-9);
            let f = ((want - cum[j]) / span).clamp(0.0, 1.0);
            let a = w0 + (w1 - w0) * (j as f32 + f) / N as f32;
            let got = f32::from(baked) / 65535.0;
            worst = worst.max((a - got).abs());
        }
        assert!(
            worst <= 2.0 * (CROSS_ARC1 - CROSS_ARC0) / (CROSS_PACE_KNOTS - 1) as f32,
            "the baked pace is {worst:.5} of the arc off the ink's own step — re-generate it"
        );
    }

    #[test]
    fn the_walk_is_a_function_of_position_so_a_retyped_cell_takes_back_its_stop() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Cols 2..41; the caret ends at 42; the cohort's origin is col 2.
        type_run(&mut rib, t0, 2, 40, &c, 0.7);
        let mut ms = 40 * 60;
        for caret in [41u16, 40, 39] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &c);
            ms += 60;
        }
        // §4's retract is a suffix: every cell at or right of the caret is
        // leaving, nothing left of it is, and nothing in the middle is gone.
        for cell in rib.cells() {
            assert_eq!(
                cell.retract_at.is_some(),
                cell.col >= 39,
                "col {} is on the wrong side of the backspace",
                cell.col
            );
        }
        assert!(
            (2..39u16).all(|col| rib.field_at(2, col).is_some()),
            "the lit run must stay contiguous from the caret backward"
        );
        // Retype the three, then one more in the middle of the word.
        for caret in [40u16, 41, 42, 21] {
            let now = at(t0, ms);
            let cx = ctx(now, &c, (2, caret), 0.7);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            ms += 60;
        }
        for cell in rib.cells().iter().filter(|l| l.retract_at.is_none()) {
            let want = walk_t(f32::from(cell.col) - 2.0);
            assert!(
                (cell.t - want).abs() < 1e-5,
                "col {} carries t = {} but the walk at its column is {want}",
                cell.col,
                cell.t
            );
        }
        for col in [20u16, 39, 40, 41] {
            let owners: Vec<&Cell> = rib.cells().iter().filter(|l| l.col == col).collect();
            assert_eq!(
                owners.len(),
                1,
                "col {col} must have exactly one owner after a retype"
            );
            assert!(owners[0].retract_at.is_none(), "…and it is live again");
        }
    }

    /// The reach stops where the hand stopped: a mark two cells short of the four-letter span
    /// wants `col0 - 1` and `col0 - 2`, which at the start of a shell line are the prompt.
    /// [`Ribbon::hand_floor`] bounds it.
    #[test]
    fn the_reach_stops_at_the_hand_s_own_first_column() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 28,
            n: 2,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, t0, keys, &c);
        // Past the rest and two reach beats.
        let now = at(t0, 60 + (PHRASE_REST_MIN_S * 1000.0) as u64 + 100);
        let cx = ctx(now, &c, (2, 30), 0.0);
        rib.plan(&cx);
        assert!(
            rib.field_at(2, 28).is_some(),
            "the hand's own first cell must still be lit, or this pins nothing"
        );
        for col in 0..28u16 {
            assert!(
                rib.field_at(2, col).is_none(),
                "the reach lit column {col}, left of the first column the hand \
                 ever held on the row — at a shell prompt that is the prompt"
            );
        }
    }

    /// A column is only a place on its own row: [`Ribbon::abandon`] stamps a jump's landing
    /// column only into cohorts on that row, so a wrapped line's earlier row is not slid toward
    /// a column of the row below.
    #[test]
    fn a_jump_s_landing_column_is_stamped_on_its_own_row_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A wrapped line: row 2 cols 28..59, row 3 cols 0..15.
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 28,
                n: 32,
                period_ms: 12,
                disp: 0.8,
            },
            &c,
        );
        type_keys(
            &mut rib,
            at(t0, 400),
            Keys {
                g: geom(),
                row: 3,
                col0: 0,
                n: 16,
                period_ms: 12,
                disp: 0.8,
            },
            &c,
        );
        assert!(
            rib.cohorts().iter().any(|coh| coh.row == 2),
            "row 2 must still carry a cohort, or this pins nothing"
        );
        // A keyed same-row jump on row 3, forward past the lit run's end so
        // the crossed cells are dark and `scrub` cannot claim it.
        let now = at(t0, 700);
        let cx = ctx(now, &c, (3, 40), 0.0);
        rib.on_event(
            &Event::Move {
                from: (3, 16),
                to: (3, 40),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
            now,
            &cx,
        );
        for coh in rib.cohorts().iter().filter(|coh| coh.row != 3) {
            assert!(
                coh.retract_col != Some(40),
                "a cohort on row {} was handed column 40, which is a place on \
                 row 3 — it must reach the per-row law instead",
                coh.row
            );
        }
    }

    #[test]
    fn the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows() {
        // The live-momentum wave stays inside the rows the body owns: the top
        // edge never rises above `TALL_UP_CH` and the bottom never falls past
        // the lower bound, at any phase. At full momentum the bottom is the
        // comet's lobe under the hand (`COMET_DN_HAND_CH` 0.55); the flat
        // spelling keeps 0.36.
        for (c, dn_ch) in [
            (cfg(true, true), COMET_DN_HAND_CH),
            (flat(true, true), DN_TOP_CH),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 24, &c, 1.0);
            let g = geom();
            let ch = g.ch as f32;
            let rest = f32::from(g.origin_y) + 3.0 * ch;
            let mut deepest = 0.0f32;
            for k in 0..16u32 {
                let mut cx = ctx(at(t0, 23 * 60 + 8), &c, (2, 34), 1.0);
                cx.phase = k as f32 / 16.0;
                rib.plan(&cx);
                for col in 10..34u16 {
                    let band = rib.band(2, col).expect("a laid cell has a band");
                    deepest = deepest.max((band.spine - rest).abs());
                    assert!(
                        band.top >= rest - TALL_UP_CH * ch - 0.5,
                        "the wave lifted the body's top past its own reach at col {col}, phase {k}/16 ({})",
                        band.top
                    );
                    assert!(
                        band.bottom <= rest + dn_ch * ch + 0.5,
                        "the wave dropped the body's bottom past its own reach at col {col}, phase {k}/16 ({}; flat {})",
                        band.bottom,
                        c.ribbon_flat
                    );
                }
            }
            assert!(
                deepest >= 0.10 * ch,
                "the wave's deepest swing at full momentum is {deepest:.2} px ({:.3} ch); the owner asked for a ribbon that visibly breathes with speed — `WAVE_AMP_CELLS` 0.110",
                deepest / ch
            );
        }
    }

    // -- §4.1: the hot edge ------------------------------------------------

    #[test]
    fn the_hot_edge_lives_only_at_the_head_on_a_dark_ground() {
        let dark = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &dark, 0.9);
        let g = geom();
        let caret_top = f32::from(g.origin_y) + 2.0 * g.ch as f32;
        let caret_bot = caret_top + g.ch as f32;
        let reach_y = HOT_EDGE_CARET_REACH_CH * g.ch as f32;
        let check = |out: &[GlowQuad], head_col: u16, label: &str| {
            assert!(!out.is_empty(), "{label}: a hot hand must leave a hot edge");
            let head_x = f32::from(g.origin_x) + f32::from(head_col) * g.cw as f32;
            for q in out {
                assert!(
                    u32::from(q.alpha) == 0,
                    "{label}: the hot edge is ADDITIVE light; it may not carry a source-over alpha"
                );
                let x = f32::from(q.x);
                assert!(
                    x <= head_x + 1.0,
                    "{label}: the hot edge ran PAST the head cell (x = {x}, head = {head_x})"
                );
                assert!(
                    head_x - x <= HOT_EDGE_CELLS_MAX * g.cw as f32 + f32::from(q.w) + 1.0,
                    "{label}: the hot edge reached further than {HOT_EDGE_CELLS_MAX} cells behind the head"
                );
                let y = f32::from(q.y);
                assert!(
                    (caret_top - reach_y..=caret_bot + reach_y).contains(&y),
                    "{label}: the hot edge left the caret's own row band (y = {y})"
                );
            }
            // The peak request is `HOT_EDGE_COV_MAX`; the beam's transverse AA
            // can only lower it, and the lifted stop's brightest channel is
            // 255, so its premultiplied peak is the coverage itself.
            assert!(
                peak_channel(out) as f32 <= HOT_EDGE_COV_MAX + 1.0,
                "{label}: hot-edge coverage is over the {HOT_EDGE_COV_MAX} request ceiling"
            );
        };
        let mut sink = Sink::default();
        let cx = ctx(at(t0, 8 * 60), &dark, (2, 18), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        check(&sink.out, 18, "typing");
        // A backspace frame (§4.1, §20.1): three erased cells still draining
        // right of the caret; the edge ends at the caret's own cell.
        let mut ms = 8 * 60;
        for caret in [17u16, 16, 15] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
            ms += 60;
        }
        let cx = ctx(at(t0, ms), &dark, (2, 15), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            rib.cells().iter().any(|l| l.col >= 15),
            "the erased cells must still be draining for this frame to test anything"
        );
        check(&sink.out, 15, "backspace");
        // …and once the whole word is erased nothing at the hand is wet.
        for caret in [14u16, 13, 12, 11, 10] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
            ms += 60;
        }
        let cx = ctx(at(t0, ms), &dark, (2, 10), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !rib.cells().is_empty() && !sink.under.is_empty(),
            "the draining cells must still be on glass for this frame to test anything"
        );
        assert!(
            sink.out.is_empty(),
            "a word that is entirely draining has no wet head; the hot edge sat on erased cells"
        );
    }

    // -- the mark never brightens after the last key ------------------------

    #[test]
    fn the_body_never_brightens_after_the_last_keystroke() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 10, &c, 0.9);
        let last = at(t0, 9 * 60);
        let mut sink = Sink::default();
        let mut peak_prev = 255u8;
        let mut edge_first: Option<u32> = None;
        // Past the head cell's 18 ms `edge-in` the mark may only spend light, even under a
        // climbing spine. The body is read on its request (the rasterized peak may move a
        // level as the wave settles), the additive hot edge on what it emits.
        for ms in (20..1400).step_by(8) {
            let now = at(last, ms);
            let disp = (0.4 + 0.6 * (ms as f32 / 700.0)).min(1.0);
            let cx = ctx(now, &c, (2, 20), disp);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let peak = plan_peak(&rib);
            assert!(
                peak <= peak_prev,
                "the body brightened at +{ms} ms with no keystroke behind it ({peak_prev} → {peak})"
            );
            peak_prev = peak;
            let lit = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            assert!(
                f32::from(lit) <= BODY_FRAME_TOP,
                "the composited body passed the ledger's frame top at +{ms} ms ({lit})"
            );
            // The head vertex is pinned (`WAVE_HEAD_PIN`), so the hairline's brightest slab
            // is byte-static; the vertex one cell behind rides a quarter of the wave's ~2 px,
            // which the AA can move by a few levels.
            let edge = peak_channel(&sink.out);
            let first = *edge_first.get_or_insert(edge);
            assert!(
                edge <= first + 4,
                "the hot edge brightened at +{ms} ms with no keystroke behind it ({first} → {edge})"
            );
        }
        assert!(
            edge_first.is_some_and(|e| e > 0),
            "the hot edge must have been lit for its law to have been tested"
        );
    }

    // -- §4: the exit swoosh -----------------------------------------------

    #[test]
    fn the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let caret = (2u16, 18u16);
        let mut sink = Sink::default();
        let mut left_prev = f32::NEG_INFINITY;
        let mut edge_prev = u32::MAX;
        let mut edge_first: Option<u32> = None;
        let block_right = 19.0 * geom().cw as f32;
        let mut reach_prev = f32::INFINITY;
        let retract_start = RETRACT_START_S * 1000.0;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        for ms in ((retract_start as u64)..total + 20).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            // The swoosh stops at the caret block: no slab reaches past the
            // block's right edge, and the reach never grows under a lifted hand.
            let reach = rib
                .plan_segments()
                .iter()
                .map(|s| s.x)
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                reach <= block_right + 0.5 && reach <= reach_prev + 0.5,
                "+{ms} ms: a slab at x {reach} (was {reach_prev}) is past the caret block's right \
                 edge {block_right} or grew"
            );
            reach_prev = reach;
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            // The hot edge goes out with its body: it takes the head cell's
            // envelope, so through the retract it may only fall (slab-mid
            // sampling of a collapsing segment can move the peak by a level)
            // and it is exactly zero before the cells retire.
            let edge = peak_channel(&sink.out);
            edge_first.get_or_insert(edge);
            assert!(
                edge <= edge_prev.saturating_add(2),
                "the hot edge brightened inside the swoosh at +{ms} ms ({edge_prev} → {edge})"
            );
            edge_prev = edge;
            if ms == total - 90 {
                let first = edge_first.unwrap_or(0);
                assert!(
                    !sink.under.is_empty() && edge * 4 < first,
                    "mid-fade the hot edge must be well under way with its body ({first} → {edge})"
                );
            }
            if ms == total - 10 {
                assert!(
                    !rib.cells().is_empty(),
                    "the cells must still be in the pool for the pin to mean anything"
                );
                assert_eq!(
                    edge, 0,
                    "10 ms before the pool clears the hot edge must already be at exactly zero"
                );
            }
            let Some(left) = sink.under.iter().map(|q| f32::from(q.x)).reduce(f32::min) else {
                continue;
            };
            assert!(
                left + 1.0 >= left_prev,
                "the retract moved AWAY from the caret at +{ms} ms ({left_prev} → {left})"
            );
            left_prev = left;
        }
        assert!(
            edge_first.is_some_and(|e| e > 0),
            "the hot edge must have been lit at the retract's start for its law to have been tested"
        );
        let now = at(last, (SWOOSH_TOTAL_S * 1000.0) as u64 + 40);
        let cx = ctx(now, &c, caret, 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            sink.under.is_empty() && sink.out.is_empty(),
            "the swoosh must reach EXACTLY zero, not a residue"
        );
        assert!(rib.at_rest(), "and the pools must be empty with it");
    }

    #[test]
    fn a_cohort_still_being_typed_keeps_its_boundaries_while_its_row_mate_retracts() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A: cols 10..17. A PTY-licensed jump on the same row (no credit)
        // abandons it into its retract and lays no wake, so A is a retracting
        // row-mate and B a cohort of its own (a Nav jump would take A into
        // its wake and B would join that, R5).
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let now = at(t0, 1000);
        let cx = ctx(now, &c, (2, 40), 0.8);
        rib.on_event(&pty((2, 18), (2, 40)), now, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 6,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, at(t0, 1000), keys, &c);
        let now = at(t0, 1320);
        let cx = ctx(now, &c, (2, 46), 0.8);
        rib.plan(&cx);
        let cw = geom().cw as f32;
        let (a, b): (Vec<&Segment>, Vec<&Segment>) =
            rib.plan_segments().iter().partition(|s| s.x < 30.0 * cw);
        assert!(
            !a.is_empty() && !b.is_empty(),
            "both cohorts must be planned"
        );
        let a_left = a.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        assert!(
            a_left > 10.0 * cw + 0.5,
            "the abandoned cohort must be moving toward the caret (left edge {a_left})"
        );
        let b_left = b.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        let b_right = b.iter().map(|s| s.x).fold(f32::NEG_INFINITY, f32::max);
        // The six typed cells end at 46 cw; the attach carries the head's
        // light one cell on under the caret at 46, to 47 cw.
        assert!(
            (b_left - 40.0 * cw).abs() < 0.5 && (b_right - 47.0 * cw).abs() < 0.5,
            "the cohort under the hand was squashed with its row-mate: [{b_left}, {b_right}]"
        );
    }

    /// The insert's rewrite ([`Event::Rewrite`]) retracts the row's suffix from the given
    /// column, not `ctx.caret`, on the kill's law, and moves the ribbon's caret there.
    #[test]
    fn a_rewrite_retracts_the_suffix_from_the_given_column_not_the_mirror() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.8);
        let now = at(t0, 1300);
        // The mirror still stands at the old end (2, 30).
        let cx = ctx(now, &c, (2, 30), 0.8);
        rib.on_event(
            &Event::Rewrite {
                row: 2,
                col: 18,
                cells: 12,
            },
            now,
            &cx,
        );
        let retract_at = |col: u16| {
            rib.cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .unwrap_or_else(|| panic!("no cell at (2, {col})"))
                .retract_at
        };
        assert!(
            (18..30u16).all(|col| retract_at(col).is_some()),
            "every cell at or right of the rewrite's caret retracts"
        );
        assert!(
            (10..18u16).all(|col| retract_at(col).is_none()),
            "the cells left of it are untouched"
        );
        let span = kill_span_s(12);
        let far = retract_at(29).unwrap();
        let near = retract_at(18).unwrap();
        assert!(far < near, "farthest from the caret goes first");
        let near_s = near.saturating_duration_since(now).as_secs_f32();
        assert!(
            (near_s - span * 11.0 / 12.0).abs() < 1e-3,
            "the near cell goes last, at the kill's 12·n + 240 stagger: {near_s}"
        );
        assert_eq!(
            rib.caret(),
            Some((2, 18)),
            "the ribbon's caret moves to the rewrite"
        );
    }

    #[test]
    fn under_reduced_motion_the_exit_swoosh_moves_nothing_and_only_fades() {
        let mut c = cfg(true, true);
        c.reduced_motion = true;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let cw = geom().cw as f32;
        let mut sink = Sink::default();
        let mut frames = 0u32;
        let mut peak_prev = u8::MAX;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        for ms in ((PHRASE_REST_MIN_S * 1000.0) as u64..total + 20).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, (2, 18), 0.0);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            if sink.under.is_empty() {
                continue;
            }
            frames += 1;
            let left = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            let right = sink
                .under
                .iter()
                .map(|q| f32::from(q.x) + f32::from(q.w))
                .fold(f32::NEG_INFINITY, f32::max);
            // Eight typed cells, 10..18 cw, plus the attach under the caret at
            // 18, static on every frame like the rest.
            assert!(
                (left - 10.0 * cw).abs() < 0.5 && (right - 19.0 * cw).abs() < 0.5,
                "a static mark moved at +{ms} ms: [{left}, {right}]"
            );
            let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            assert!(
                peak <= peak_prev,
                "the one linear fade may only fall (+{ms} ms: {peak_prev} → {peak})"
            );
            peak_prev = peak;
        }
        assert!(
            frames > 40,
            "the mark must be visible through most of the swoosh window ({frames})"
        );
        assert!(rib.at_rest(), "…and be gone at its end");
    }

    #[test]
    fn a_jump_takes_the_band_out_from_the_light_it_has_never_by_a_step() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Forty cells over 2.4 s: the oldest are past their melt opening.
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let before = band_sum(&rib, 42);
        let segs = rib.plan_segments().len() as u32;
        assert!(
            plan_peak(&rib) > 200,
            "the mark must be hot for a step to be measurable"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 80), 0.9);
        // A PTY-licensed jump: the abandon alone (no credit, no wake — R5's
        // wake is pinned on its own below).
        rib.on_event(&pty((2, 42), (2, 80)), now, &cx);
        rib.plan(&cx);
        let after = band_sum(&rib, 42);
        assert!(
            after + 2 * segs >= before,
            "the jump stepped the band down ({before} → {after} over {segs} boundaries) instead of retracting it from where it was"
        );
        // …and the band then leaves through the retract + fade it would have
        // taken anyway, farthest-first, and is gone at their end.
        let cx = ctx(at(last, 16 + 340), &c, (2, 80), 0.9);
        rib.plan(&cx);
        let mid = plan_sum(&rib);
        assert!(
            mid * 10 < before * 8,
            "the abandoned band must be well into its drain by mid-retract ({before} → {mid})"
        );
        let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the abandoned band must be out {RETRACT_DUR_S} + {RETRACT_FADE_S} s after the jump"
        );
    }

    // -- R5: the wake ------------------------------------------------------

    /// Cells on `row` as `(col, typing, life_s, born)`, column-sorted.
    fn row_cells(rib: &Ribbon, row: u16) -> Vec<(u16, bool, f32, Instant)> {
        let mut v: Vec<_> = rib
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.col, c.typing, c.life_s, c.born))
            .collect();
        v.sort_by_key(|c| c.0);
        v
    }

    #[test]
    fn a_jump_lays_its_wake_over_the_live_band_and_leaves_the_band_s_cells_untouched() {
        // Under the overlay (K1, M1) the band's cells are byte-identical after
        // a jump and the corridor lies over them on `Over` at the band's own
        // level with the same stops, so the eye sees no step. The band starts
        // at column 12 and the jump crosses ten dark cells first, so the move
        // leaves the run (v3 §2.7) rather than being a scrub.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Forty cells at cols 12..52, then a Ctrl-A to column 2: the band under the corridor
        // keeps every stop, loses no light on the jump frame, has one owner per cell, and
        // outlives the jump on the wake's clock instead of the abandon's 0.64 s retract.
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 12,
                n: 40,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 39 * 60);
        let cx = ctx(at(last, 8), &c, (2, 52), 0.9);
        rib.plan(&cx);
        // The band's own light, not the caret's: at the `before` frame the
        // plan carries an attach under the caret at col 52 that leaves with
        // the caret, so the band is read with `band_sum` at its right edge,
        // 51, on every frame (`before`, `after`, `landed`, `held` are one
        // quantity). The corridor's dark half (cols 2..12) is asserted below.
        let band_light = |rib: &Ribbon| band_sum(rib, 51);
        let before = band_light(&rib);
        let before_cells: Vec<Cell> = rib.cells().to_vec();
        let segs = rib.plan_segments().len() as u32;
        let stops: Vec<Option<f32>> = (12..52u16).map(|col| rib.field_at(2, col)).collect();
        assert!(
            stops.iter().all(Option::is_some),
            "the band is laid under cols 12..52"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 2), 0.9);
        // The band's envelope at the jump instant, which the corridor's cells
        // are born from.
        let envs_before: Vec<(u16, f32)> = before_cells
            .iter()
            .map(|l| (l.col, rib.env_of(&cx, l)))
            .collect();
        rib.on_event(&nav((2, 52), (2, 2)), now, &cx);
        rib.plan(&cx);
        for (col, want) in (12..52u16).zip(&stops) {
            let got = rib.field_at(2, col);
            assert!(
                matches!((got, want), (Some(g), Some(w)) if (g - w).abs() < 1e-5),
                "col {col}: the wake repainted a stop the eye had read ({want:?} → {got:?})"
            );
        }
        let wake = rib
            .cohorts()
            .iter()
            .map(|c| c.id)
            .max()
            .expect("the wake cohort");
        let band = rib.cohorts()[0];
        assert_ne!(band.id, wake);
        assert!(band.abandoned, "the band is abandoned beneath the corridor");
        assert_eq!(
            (band.retract_col, band.drain_right_first),
            (Some(2), true),
            "…toward the landing, draining from the origin end first (the drain follows the meteor)"
        );
        // The corridor's dark half: the ten cells before the band, which make
        // this move a jump and not a scrub. They carry the corridor and
        // nothing else.
        let flight = flight_ms(50.0) / 1000.0;
        for col in 2..12u16 {
            assert!(
                !rib.cells()
                    .iter()
                    .any(|c| c.row == 2 && c.col == col && c.layer == Layer::Base),
                "col {col}: dark ground carries no band cell"
            );
            let over: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
                .collect();
            assert_eq!(over.len(), 1, "col {col}: the corridor lit the dark ground");
            assert!(over[0].rearm.is_none(), "col {col}: nothing to re-arm from");
        }
        for col in 12..52u16 {
            let base: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Base)
                .collect();
            let over: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
                .collect();
            assert_eq!(
                base.len(),
                1,
                "col {col}: the band's own cell is still there"
            );
            assert_eq!(over.len(), 1, "col {col}: one wake cell over it");
            let (b, o) = (base[0], over[0]);
            let was = before_cells
                .iter()
                .find(|c| c.col == col)
                .expect("was laid");
            assert!(
                b.cohort == was.cohort
                    && b.born == was.born
                    && b.cov0 == was.cov0
                    && b.life_s == was.life_s,
                "col {col}: the wake touched the band's cell (K1)"
            );
            assert_eq!(o.cohort, wake, "col {col}: …in the wake cohort");
            assert!(!o.typing, "col {col}: a wake cell is never typing");
            assert!(
                o.cov0 >= b.cov0 - 1e-6,
                "col {col}: the corridor is never dimmer than the band it covers"
            );
            let (re_at, level) = o.rearm.expect("born at the band's level");
            assert_eq!(re_at, o.born);
            // The band's coverage share at the jump: `cov0 · env` — the
            // newest typed cell is still inside its 18 ms attack at +16 ms.
            let env_before = envs_before
                .iter()
                .find(|&&(c, _)| c == col)
                .map_or(0.0, |&(_, e)| e);
            assert!(
                env_before > 0.9 && (level * o.cov0 - b.cov0 * env_before).abs() < 1e-5,
                "col {col}: the level is the band's own coverage share (level {level}, env {env_before})"
            );
            let j = f32::from(52 - col);
            let want = now + Duration::from_secs_f32(flight * j / 50.0);
            assert!(
                o.born.saturating_duration_since(want) <= Duration::from_millis(1)
                    && want.saturating_duration_since(o.born) <= Duration::from_millis(1),
                "col {col}: born on the flight clock"
            );
        }
        // The origin cell (52) is unlit and beyond the cap counted back from
        // the landing, so it stays dark: the cap bounds new light only; the
        // ten dark cells before the band are inside it and laid, and the
        // band's forty are covered however far back they run.
        assert!(rib.field_at(2, 52).is_none());
        assert_eq!(
            rib.cells().len(),
            90,
            "the band's 40 cells and the corridor's 50 over them"
        );
        // No step on the jump frame (the corridor is unborn, the band is
        // whole) and none once the corridor has landed.
        let after = band_light(&rib);
        assert!(
            after + 2 * segs >= before,
            "the jump frame stepped the band down ({before} → {after} over {segs} boundaries)"
        );
        let cx = ctx(at(now, (flight * 1000.0) as u64 + 20), &c, (2, 2), 0.9);
        rib.plan(&cx);
        let landed = band_light(&rib);
        assert!(
            landed + 2 * segs >= before,
            "the landed corridor is dimmer than the band it covers ({before} → {landed})"
        );
        // Where the abandon's retract would have drained it to nothing, the
        // wake still holds the corridor (the design's ~1.1 s after-effect)…
        let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
        rib.plan(&cx);
        let held = band_light(&rib);
        assert!(
            held * 2 > before,
            "0.66 s after the jump the wake must still hold the corridor ({before} → {held})"
        );
        // …and it is out one wake life past the last birth.
        let cx = ctx(at(last, 16 + 1250), &c, (2, 80), 0.9);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the wake is out by +1.25 s");
    }

    #[test]
    fn a_ping_pong_at_speed_does_not_accumulate_and_empties_one_wake_life_after_its_last_key() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Ctrl-A / Ctrl-E across 58 cells every 200 ms for 3 s, planned at
        // 120 Hz. Each jump abandons the last wake and lays its own, so the
        // pool plateaus (bounded by WAKE_LIFE_S / period wakes plus the
        // abandoned ones' 0.64 s drains) instead of growing with the count.
        let mut caret = (2u16, 2u16);
        let mut peak_by_cycle = Vec::new();
        let mut peak = 0usize;
        // K2 / K4: from the second corridor's landing on, the lit cells on
        // the row are one interval holding both endpoints on every frame, and
        // the row holds exactly one live wake cohort after every jump.
        let landed = 200 + flight_ms(58.0) as u64 + (EDGE_IN_S * 1000.0) as u64 + 8;
        let mut interval_frames = 0u32;
        for ms in (0..=3000u64).step_by(8) {
            let now = at(t0, ms);
            if ms % 200 == 0 {
                let to = if caret.1 == 2 { (2, 60) } else { (2, 2) };
                let cx = ctx(now, &c, to, 0.0);
                rib.on_event(&nav(caret, to), now, &cx);
                caret = to;
                if ms > 0 {
                    peak_by_cycle.push(peak);
                    peak = 0;
                }
            }
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            peak = peak.max(rib.cells().len());
            if ms % 200 == 0 {
                let wakes = rib
                    .cohorts()
                    .iter()
                    .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
                    .count();
                assert_eq!(wakes, 1, "+{ms} ms: one live wake cohort on the row (K4)");
            }
            if ms >= landed {
                // A column is lit when any cell at it composites at or over
                // the status floor; the position's owner is the brightest
                // born cell (`build`'s rank), so "any" is what the glass shows.
                let lit: Vec<u16> = (2..=60u16)
                    .filter(|&col| {
                        rib.cells().iter().any(|l| {
                            l.row == 2
                                && l.col == col
                                && UNDER_COV_CAP * l.cov0 * rib.env_of(&cx, l)
                                    >= f32::from(STATUS_LIT_COV)
                        })
                    })
                    .collect();
                assert!(
                    lit.first() == Some(&2) && lit.last() == Some(&60),
                    "+{ms} ms: the lit set holds both endpoints ({lit:?})"
                );
                assert!(
                    lit.windows(2).all(|w| w[1] - w[0] <= 1),
                    "+{ms} ms: the lit set is one interval, no hole (K2): {lit:?}"
                );
                interval_frames += 1;
            }
        }
        assert!(
            interval_frames > 300,
            "the interval law was checked on {interval_frames} frames"
        );
        let bound = usize::from(WAKE_MAX_CELLS + 1) * (WAKE_LIFE_S / 0.2).ceil() as usize
            + 59 * (RETRACT_START_S / 0.2).ceil() as usize;
        let worst = *peak_by_cycle.iter().max().expect("cycles");
        assert!(
            worst <= bound,
            "the pool grew past its bound: {worst} > {bound} ({peak_by_cycle:?})"
        );
        let (early, late) = peak_by_cycle.split_at(peak_by_cycle.len() / 2);
        assert!(
            late.iter().max() <= early.iter().max(),
            "the second half of the ping-pong holds more than the first: {peak_by_cycle:?}"
        );
        let cx = ctx(at(t0, 3000 + 1250), &c, caret, 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "everything is out one wake life after the last jump"
        );
    }

    /// A partial reversal re-owns the corridor instead of abandoning it (K4): the uncovered far
    /// part stays in the re-owned cohort, so the corridor is one clock and one interval (K2).
    #[test]
    fn a_partial_reversal_keeps_the_whole_corridor_on_one_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A 43-cell run left to go out entirely, so the corridor is laid over
        // nothing: over a live band cell a corridor is laid without bound
        // (§2.10) and neither the cap nor the ownership would show.
        type_run(&mut rib, t0, 2, 43, &c, 0.9);
        let cold = at(t0, 6000);
        rib.plan(&ctx(cold, &c, (2, 43), 0.0));
        assert!(
            rib.at_rest(),
            "the typed band is gone before the first jump"
        );
        // Ctrl-A: 43 → 0, the whole row.
        let a = at(cold, 100);
        rib.on_event(&nav((2, 43), (2, 0)), a, &ctx(a, &c, (2, 0), 0.0));
        rib.plan(&ctx(at(a, 130), &c, (2, 0), 0.0));
        let first = rib
            .cohorts()
            .iter()
            .find(|k| k.wake)
            .map(|k| k.id)
            .expect("the first corridor");
        // …then a partial reversal: 0 → 20, half of it.
        let b = at(a, 300);
        rib.on_event(&nav((2, 0), (2, 20)), b, &ctx(b, &c, (2, 20), 0.0));
        rib.plan(&ctx(at(b, 130), &c, (2, 20), 0.0));
        let live: Vec<(u32, bool)> = rib
            .cohorts()
            .iter()
            .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
            .map(|k| (k.id, k.abandoned))
            .collect();
        assert_eq!(
            live,
            vec![(first, false)],
            "the reversal re-owns the corridor it overlaps: one live wake cohort, \
             not abandoned ({live:?})"
        );
        // One clock over the whole corridor — the covered half and the far
        // half the reversal never reached.
        let cx = ctx(at(b, 130), &c, (2, 20), 0.0);
        let clocks: Vec<Instant> = rib
            .cells()
            .iter()
            .filter(|l| l.layer == Layer::Over)
            .map(|l| rib.live_since(l))
            .collect();
        assert!(clocks.len() > 30, "the corridor is laid ({})", clocks.len());
        assert_eq!(
            clocks.iter().min(),
            clocks.iter().max(),
            "every corridor cell reads one clock, covered or not"
        );
        // …and it stays one interval for the rest of the wake's life.
        let mut frames = 0u32;
        for ms in (0..1000u64).step_by(8) {
            let now = at(b, 130 + ms);
            let cx = ctx(now, &c, (2, 20), 0.0);
            rib.plan(&cx);
            let live: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|l| l.layer == Layer::Over && rib.env_of(&cx, l) > 0.0)
                .map(|l| l.col)
                .collect();
            if live.is_empty() {
                break;
            }
            let (lo, hi) = (
                *live.iter().min().expect("min"),
                *live.iter().max().expect("max"),
            );
            assert_eq!(
                (lo..=hi).filter(|col| live.contains(col)).count(),
                usize::from(hi - lo + 1),
                "+{ms} ms: the corridor is one interval ({lo}..={hi})"
            );
            frames += 1;
        }
        let _ = cx;
        assert!(
            frames > 60,
            "the interval law was checked on {frames} frames"
        );
    }

    #[test]
    fn a_row_change_a_pty_jump_and_a_typed_move_lay_no_wake() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (3, 5), 0.0);
        rib.on_event(&nav((2, 5), (3, 5)), t0, &cx);
        assert!(rib.at_rest(), "an Up-arrow recall paints no line");
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&pty((2, 60), (2, 2)), t0, &cx);
        assert!(rib.at_rest(), "a PTY cascade earns no wake");
        rib.on_event(
            &Event::Move {
                from: (2, 60),
                to: (2, 2),
                licence: Licence::Typed,
                dir: Dir::Right,
            },
            t0,
            &cx,
        );
        assert!(rib.at_rest(), "a typed echo's own motion lays nothing");
        rib.on_event(
            &Event::Move {
                from: (2, 60),
                to: (2, 2),
                licence: Licence::Return,
                dir: Dir::Right,
            },
            t0,
            &cx,
        );
        assert!(rib.at_rest(), "a return-licensed move lays nothing");
    }

    #[test]
    fn an_arrow_hop_lays_a_short_wake_behind_the_caret_and_leaves_live_cells_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A word at cols 10..17, the caret at 18; Right 18 → 19 crosses the empty cell 18 (a
        // hop, not a scrub) and lays 18 and 19 in a wake cohort of its own on the word's walk,
        // leaving the word's clock, phase and bounds alone. (Left onto the word is a scrub.)
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let word = rib.cohorts()[0];
        assert!(!word.wake);
        let now = at(t0, 600);
        let cx = ctx(now, &c, (2, 19), 0.8);
        let t17 = rib.field_at(2, 17);
        rib.on_event(&nav((2, 18), (2, 19)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.field_at(2, 17),
            t17,
            "the live cell beside the hop is untouched"
        );
        let laid: Vec<&Cell> = rib
            .cells()
            .iter()
            .filter(|c| c.row == 2 && c.col == 18)
            .collect();
        assert_eq!(laid.len(), 1);
        assert_ne!(
            laid[0].cohort, word.id,
            "the hop's cell is in a cohort of its own, not the word's"
        );
        let hop = rib
            .cohorts()
            .iter()
            .find(|k| k.id == laid[0].cohort)
            .expect("the hop's cohort");
        assert!(hop.wake, "…a wake cohort");
        assert!(
            (laid[0].t - word.t_at(18)).abs() < 1e-6,
            "…on the word's own walk ({} vs {})",
            laid[0].t,
            word.t_at(18)
        );
        assert!(!laid[0].typing);
        assert!((laid[0].life_s - WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE).abs() < 1e-6);
        assert!(
            rib.cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == 17)
                .count()
                == 1
        );
        let word_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == word.id)
            .expect("the word");
        assert_eq!(
            (word_now.alive_at, word_now.col0, word_now.col1),
            (word.alive_at, word.col0, word.col1),
            "the hop touched the word's clock or bounds"
        );
        assert_eq!(
            word_now.phase,
            Phase::Grace,
            "180 ms after its last key the word is in its grace — a hop that refreshed it would read Laying"
        );
        // …so the word swooshes exactly when it would have without the arrow:
        // SWOOSH_TOTAL_S after its last key (t0 + 420 ms), not after the hop.
        let cx = ctx(
            at(t0, 420 + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            (2, 19),
            0.0,
        );
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|c| c.cohort != word.id),
            "the word must be out {SWOOSH_TOTAL_S} s after its own last key; the arrow lifted its finger"
        );
        // Right held down over cold ground: a trail behind the caret, one
        // cohort, gone WAKE_LIFE_S / 2 after the last step.
        let mut rib = Ribbon::new();
        for (k, col) in (40u16..46).enumerate() {
            let now = at(t0, 40 * k as u64);
            let cx = ctx(now, &c, (2, col + 1), 0.0);
            rib.on_event(&nav((2, col), (2, col + 1)), now, &cx);
            rib.plan(&cx);
        }
        let cols: Vec<u16> = row_cells(&rib, 2).iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (40..=46).collect::<Vec<_>>(),
            "the arrow painted behind itself"
        );
        assert_eq!(rib.cohorts().len(), 1, "one walk under the held arrow");
        let cx = ctx(at(t0, 200 + 100 + 560), &c, (2, 46), 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the arrow's trail is out half a wake life after its last step"
        );
    }

    /// The columns of `row` that carry an `Over` cell, sorted.
    fn over_cols(rib: &Ribbon, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row && l.layer == Layer::Over)
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The columns of `row` any cell lights at or over the status floor —
    /// the ping-pong test's reading of the glass.
    fn lit_cols(rib: &Ribbon, cx: &Ctx<'_>, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| {
                l.row == row
                    && UNDER_COV_CAP * l.cov0 * rib.env_of(cx, l) >= f32::from(STATUS_LIT_COV)
            })
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The cap binds at the corridor's far edge, never inside it (v3 §2.10, K2): a Ctrl-A over
    /// a band drained past the cap, and a Ctrl-E back after a cold Ctrl-A, lay out to the
    /// farthest light they keep; with nothing lit past the cap a Ctrl-A lays exactly the cap.
    #[test]
    fn a_jump_s_cap_binds_at_the_corridor_s_far_edge_never_inside_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        // (a) Ctrl-A over a band drained past the cap.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 100, &c, 0.9);
        let last = at(t0, 99 * 60);
        let idle_ms = ((RETRACT_START_S + RETRACT_FADE_S + 0.2) * 1000.0).round() as u64;
        let now = at(last, idle_ms);
        let cx = ctx(now, &c, (2, 102), 0.9);
        rib.plan(&cx);
        let lit_before = lit_cols(&rib, &cx, 2);
        assert!(
            lit_before.first().is_some_and(|&l| l > 2 + WAKE_MAX_CELLS)
                && lit_before.last() == Some(&101),
            "the setup: the drain frontier lies past the cap, the remnant by the origin \
             ({lit_before:?})"
        );
        let cx = ctx(now, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 102), (2, 2)), now, &cx);
        assert_eq!(
            over_cols(&rib, 2),
            (2..=101).collect::<Vec<_>>(),
            "the corridor runs from the landing out to the farthest light it keeps"
        );
        let landed = at(now, flight_ms(100.0) as u64 + 30);
        let cx = ctx(landed, &c, (2, 2), 0.9);
        rib.plan(&cx);
        let lit = lit_cols(&rib, &cx, 2);
        assert!(
            lit.first() == Some(&2)
                && lit.last() == Some(&101)
                && lit.windows(2).all(|w| w[1] - w[0] <= 1),
            "(a) the lit set is one interval holding both ends: {lit:?}"
        );
        // (b) Ctrl-E back over dark ground, 300 ms after a cold Ctrl-A.
        let mut rib = Ribbon::new();
        let a = at(t0, 0);
        rib.on_event(&nav((2, 110), (2, 2)), a, &ctx(a, &c, (2, 2), 0.0));
        rib.plan(&ctx(at(a, 130), &c, (2, 2), 0.0));
        assert_eq!(
            over_cols(&rib, 2),
            (2..2 + WAKE_MAX_CELLS).collect::<Vec<_>>(),
            "the setup: the cold Ctrl-A lays the cap"
        );
        let e = at(a, 300);
        rib.on_event(&nav((2, 2), (2, 110)), e, &ctx(e, &c, (2, 110), 0.0));
        assert_eq!(
            over_cols(&rib, 2),
            (2..=110).collect::<Vec<_>>(),
            "the End's corridor reaches the island it re-owns"
        );
        let wakes = rib
            .cohorts()
            .iter()
            .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
            .count();
        assert_eq!(wakes, 1, "one wake cohort on the row (K4)");
        for ms in (160..900u64).step_by(8) {
            let cx = ctx(at(e, ms), &c, (2, 110), 0.0);
            rib.plan(&cx);
            let lit = lit_cols(&rib, &cx, 2);
            assert!(
                lit.windows(2).all(|w| w[1] - w[0] <= 1),
                "(b) +{ms} ms: the lit set is one interval (K2): {lit:?}"
            );
            if ms < 500 {
                assert!(
                    lit.first() == Some(&2) && lit.last() == Some(&110),
                    "(b) +{ms} ms: the lit set holds both ends: {lit:?}"
                );
            }
        }
    }

    /// A hop covers a leaving band from its level and leaves a live one as it is: over a
    /// retracting band a Left ×5 lays `Over` cells born at the band's coverage (M1), the band
    /// byte-identical (K1); over a band in its grace it lays nothing.
    #[test]
    fn a_hop_covers_a_leaving_band_from_its_level_and_leaves_a_live_one_as_it_is() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * 0.9;
        // Leaving: nine cells at 2..11, the caret at 11, the hop inside the
        // retract.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 9, &c, 0.9);
        let last = at(t0, 8 * 60);
        let band = rib.cohorts()[0];
        let now = at(last, (RETRACT_START_S * 1000.0).round() as u64 + 100);
        let cx = ctx(now, &c, (2, 11), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cohorts()[0].leaving_at(now),
            "the setup: the band is inside its swoosh"
        );
        let before: Vec<Cell> = rib.cells().to_vec();
        let level: Vec<(u16, f32)> = before
            .iter()
            .map(|l| (l.col, l.cov0 * rib.env_of(&cx, l)))
            .collect();
        assert!(
            level
                .iter()
                .filter(|&&(col, _)| col >= 6)
                .all(|&(_, v)| v > 0.1),
            "the setup: the band is still lit under the hop ({level:?})"
        );
        let cx = ctx(now, &c, (2, 6), 0.9);
        rib.on_event(&nav((2, 11), (2, 6)), now, &cx);
        assert_eq!(
            over_cols(&rib, 2),
            (6..=11).collect::<Vec<_>>(),
            "the hop lays its whole corridor over the leaving band"
        );
        for w in rib.cells().iter().filter(|l| l.layer == Layer::Over) {
            match level.iter().find(|&&(col, _)| col == w.col) {
                Some(&(col, start)) => {
                    let (re_at, lv) = w
                        .rearm
                        .unwrap_or_else(|| panic!("col {col}: over band light is re-armed"));
                    assert_eq!(re_at, w.born, "col {col}: re-armed at its birth");
                    assert!(
                        (w.cov0 * lv - start).abs() < 1e-5,
                        "col {col}: born at the band's own coverage {start} (cov0 {} level {lv})",
                        w.cov0
                    );
                }
                None => assert!(
                    w.rearm.is_none() && (w.cov0 - cov0_new).abs() < 1e-6,
                    "col {}: over nothing, a cold-priced cell from dark",
                    w.col
                ),
            }
        }
        for was in &before {
            let is = rib
                .cells()
                .iter()
                .find(|l| l.layer == Layer::Base && l.col == was.col)
                .expect("the band's cells stay");
            assert!(
                is.cohort == was.cohort
                    && is.born == was.born
                    && is.cov0 == was.cov0
                    && is.retract_at == was.retract_at,
                "the hop touched the band's cell at col {} (K1)",
                was.col
            );
        }
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == band.id)
                .map(|k| k.alive_at),
            Some(band.alive_at),
            "the leaving band keeps its clock"
        );
        // Live: the same run in its grace, a hop from dark ground at 14 back
        // to 8 — not a scrub (its corridor is not lit end to end).
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 9, &c, 0.9);
        let now = at(last, 300);
        let cx = ctx(now, &c, (2, 11), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.cohorts()[0].leaving_at(now),
            "the setup: the band is live"
        );
        let before: Vec<Cell> = rib.cells().to_vec();
        let cx = ctx(now, &c, (2, 8), 0.9);
        rib.on_event(&nav((2, 14), (2, 8)), now, &cx);
        assert_eq!(
            over_cols(&rib, 2),
            (11..=14).collect::<Vec<_>>(),
            "the hop lays only the dark ground; the live band's cells are left as they are"
        );
        for was in &before {
            assert!(
                rib.cells().iter().any(|l| l.layer == Layer::Base
                    && l.col == was.col
                    && l.born == was.born
                    && l.cohort == was.cohort),
                "the live band's cell at col {} is untouched",
                was.col
            );
        }
    }

    // -- scrubbing never damages the band ------------------------------------

    /// One column of a [`census`]: `(col, owner, planned coverage)`, the
    /// owner as `(cohort, born, typing, cov0 bits)` or `None` where no live
    /// cell owns the column.
    type CensusColumn = (u16, Option<(u32, Instant, bool, u32)>, u8);

    /// The per-cell census of `row`'s cells `col0..col1`: each column's owner (`cohort`, `born`,
    /// `typing`, `cov0`) and the max planned `cov` of every boundary and slab whose `x` falls
    /// in it (a retract moves light with its boundary). Byte for byte, so "the scrub damaged
    /// nothing" is an equality.
    fn census(rib: &Ribbon, row: u16, col0: u16, col1: u16) -> Vec<CensusColumn> {
        let cw = geom().cw as f32;
        (col0..col1)
            .map(|col| {
                let owner = rib
                    .cells()
                    .iter()
                    .rposition(|c| c.row == row && c.col == col && !c.leaving())
                    .map(|i| {
                        let c = rib.cells()[i];
                        (c.cohort, c.born, c.typing, c.cov0.to_bits())
                    });
                let cov = rib
                    .plan_segments()
                    .iter()
                    .filter(|s| (s.x / cw).floor() as i64 == i64::from(col))
                    .map(|s| s.cov)
                    .max()
                    .unwrap_or(0);
                (col, owner, cov)
            })
            .collect()
    }

    /// The columns of `census` whose light is gone or is another cell's —
    /// the gaps, as `(col, cov)`.
    fn gaps(before: &[CensusColumn], now: &[CensusColumn]) -> Vec<(u16, u8)> {
        before
            .iter()
            .zip(now)
            .filter(|(b, n)| b != n)
            .map(|(_, n)| (n.0, n.2))
            .collect()
    }

    /// The line "the quick brown fox jumps over the lazy dogs", typed at 12
    /// cps on row 2 from column 0 (44 cells, the caret at 44), planned after
    /// every key. Returns the last key's clock.
    fn type_the_owner_s_line(rib: &mut Ribbon, t0: Instant, c: &Config) -> Instant {
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 0,
            n: 44,
            period_ms: 83,
            disp: 0.8,
        };
        type_keys(rib, t0, keys, c);
        at(t0, 43 * 83)
    }

    /// Scrubbing never damages the band: Option+Left/Right hops inside the typed run abandon,
    /// take over, lay and dim nothing (the band's census is byte-identical after every hop) and
    /// hold the row, so the band leaves `SWOOSH_TOTAL_S` after the last hop.
    #[test]
    fn a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let last = type_the_owner_s_line(&mut rib, t0, &c);
        let band = rib.cohorts()[0];
        // The census just before the first hop.
        let cx = ctx(at(last, 150), &c, (2, 44), 0.8);
        rib.plan(&cx);
        let before = census(&rib, 2, 0, 44);
        assert!(
            before
                .iter()
                .all(|(_, owner, cov)| owner.is_some() && *cov > 100),
            "the band is whole and hot under the line before the scrub"
        );
        // The words' starts: 40 (dogs), 35 (lazy), 31 (the), 26 (over) — and
        // back to the end.
        let stops = [40u16, 35, 31, 26, 31, 35, 40, 44, 40, 35];
        let mut caret = (2u16, 44u16);
        let mut last_hop = last;
        for (k, &col) in stops.iter().enumerate() {
            let now = at(last, 150 * (k as u64 + 1));
            let to = (2, col);
            let cx = ctx(now, &c, to, 0.8);
            rib.on_event(&nav(caret, to), now, &cx);
            rib.plan(&cx);
            caret = to;
            last_hop = now;
            let now_census = census(&rib, 2, 0, 44);
            let g = gaps(&before, &now_census);
            assert!(
                g.is_empty(),
                "hop {} ({}→{}, +{} ms): the scrub damaged the band at {} of 44 cells: {:?}",
                k + 1,
                if k == 0 { 44 } else { stops[k - 1] },
                col,
                150 * (k + 1),
                g.len(),
                g
            );
            assert!(
                rib.cohorts().iter().all(|k| !k.abandoned),
                "hop {}: a scrub abandons nothing",
                k + 1
            );
            assert!(
                rib.cells().iter().all(|c| !c.leaving()),
                "hop {}: a scrub sends no cell on its way out",
                k + 1
            );
        }
        // The scrubbing hand held the row: the band's clock is the last hop's.
        let held = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("the typed band is still the band");
        assert_eq!(
            held.alive_at, last_hop,
            "the scrubbing hand holds the row — the band's clock is the hop's"
        );
        assert_eq!(held.phase, Phase::Laying);
        // The band is whole at +0.7 s after the last hop (its grace)…
        let cx = ctx(at(last_hop, 700), &c, caret, 0.8);
        rib.plan(&cx);
        assert!(
            gaps(&before, &census(&rib, 2, 0, 44)).is_empty(),
            "0.7 s after the last hop the band is still whole: the hops renewed its clock"
        );
        // …well into its retract 1.4 s after it, on the hop's clock, not the key's…
        let cx = ctx(at(last_hop, 1400), &c, caret, 0.8);
        rib.plan(&cx);
        assert!(
            !rib.at_rest() && plan_sum(&rib) > 0,
            "1.4 s after the last hop the band is leaving on the hop's clock"
        );
        // …and out one swoosh after the hand's last act on the row.
        let cx = ctx(
            at(last_hop, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            caret,
            0.8,
        );
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the band leaves through its swoosh {SWOOSH_TOTAL_S} s after the last hop"
        );
    }

    /// A scrub into a leftward corridor does not move its soft end: a run's soft end is its left
    /// one whichever way its light streams (the boundary is one expression on every run), so
    /// col 0 is the feather and the rest flat, before and after. Host-seam twin: `scrub_gaps.rs`'s
    /// `a_scrub_over_holes_a_hop_filled_leaves_an_abandoned_band_alone_at_the_host_seam`.
    #[test]
    fn a_scrub_into_a_leftward_corridor_does_not_move_its_soft_end() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 0, 11, &c, 0.9);
        let last = at(t0, 10 * 60);
        for (from, to, ms) in [((2u16, 11u16), (2, 31), 200u64), ((2, 31), (2, 0), 550)] {
            let now = at(last, ms);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav(from, to), now, &cx);
            rib.plan(&cx);
        }
        let cx = ctx(at(last, 734), &c, (2, 0), 0.9);
        rib.plan(&cx);
        // The corridor's own extent, read off its run: the overlay reaches as
        // far as the jump did, capped at `WAKE_MAX_CELLS`. The census window
        // is its landing through the vertex one past its far cell.
        let corridor = |r: &Ribbon| {
            r.runs
                .iter()
                .find(|u| u.row == 2 && u.col0 == 0)
                .map(|u| (u.col0, u.col1, u.stream_dir))
                .expect("the corridor's own run")
        };
        let (c0, c1, dir_before) = corridor(&rib);
        assert!(
            c1 >= c0 + 4,
            "the setup: the corridor must be long enough to have a far end away from its landing (cols {c0}..={c1})"
        );
        let before = census(&rib, 2, c0, c1 + 2);
        let cohorts_before = rib.cohorts().len();
        let scrub_at = at(last, 750);
        let cx = ctx(scrub_at, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 0), (2, 2)), scrub_at, &cx);
        rib.plan(&cx);
        let after = census(&rib, 2, c0, c1 + 2);
        let (a0, a1, dir_after) = corridor(&rib);
        let show = |cen: &[CensusColumn]| cen.iter().map(|k| (k.0, k.2)).collect::<Vec<_>>();
        // The precondition: the two Rights are a scrub (nothing is minted)
        // and the head does flip across them, so the pin is not vacuous.
        assert_eq!(
            cohorts_before,
            rib.cohorts().len(),
            "the setup: the two Rights are a scrub over lit cells and mint nothing"
        );
        assert_eq!(
            (a0, a1),
            (c0, c1),
            "the scrub moved the corridor's own bounds, so the two censuses read different columns"
        );
        assert_ne!(
            dir_before, dir_after,
            "the setup: the caret move still flips the corridor run's stream, which is what the soft end must no longer follow"
        );
        // The soft end is the corridor's left one and it is whole from there
        // to its right-edge vertex on both frames. Read over every column of
        // the run, so a census dark past a short window cannot pass.
        for (name, cen) in [("before the scrub", &before), ("after it", &after)] {
            let whole = cen[1].2;
            assert!(
                cen[0].2 < whole && whole > 100 && cen[1..].iter().all(|k| k.2 == whole),
                "{name}: the corridor opens soft at its left and is whole to its right edge: {:?}",
                show(cen)
            );
        }
        for (b, a) in before.iter().zip(after.iter()) {
            assert!(
                i32::from(a.2) - i32::from(b.2) <= 2,
                "col {}: the scrub raised the corridor's light {} -> {} in one frame ({:?} -> {:?})",
                b.0,
                b.2,
                a.2,
                show(&before),
                show(&after)
            );
        }
    }

    /// S6, the other half: a phrase holds only its own. A closed phrase's cohort on the same row
    /// goes out on its own clock until a lay adjacent to it (the hand resuming the line)
    /// re-assigns it to the open phrase.
    #[test]
    fn a_new_phrase_holds_none_of_a_closed_one_s_cohorts_until_the_hand_resumes_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = |col0: u16, n: u16| Keys {
            g: geom(),
            row: 2,
            col0,
            n,
            period_ms: 100,
            disp: 0.9,
        };
        // "abc" at cols 2..5, the last key at +200 ms; a 1.2 s rest closes
        // the phrase (the floor is 0.90 s).
        type_keys(&mut rib, t0, keys(2, 3), &c);
        let first = rib.phrase_open().expect("open");
        let abc = rib.cohorts()[0];
        assert_eq!(abc.phrase, first);
        // A new phrase at cols 20.., not adjacent: the old cohort is left
        // with the clock it had.
        type_keys(&mut rib, at(t0, 1400), keys(20, 3), &c);
        let second = rib.phrase_open().expect("open");
        assert_ne!(second, first, "a rest closed the first phrase");
        let abc_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == abc.id)
            .expect("abc is still resident inside its swoosh");
        assert_eq!(
            abc_now.phrase, first,
            "the closed phrase's cohort is not re-assigned"
        );
        assert_eq!(
            abc_now.alive_at,
            at(t0, 200),
            "…and not renewed by the new phrase's keys"
        );
        assert_ne!(abc_now.phase, Phase::Laying, "it is on its way out");
        // The hand resumes the old line at col 5, adjacent to "abc", at
        // +1700 ms: still inside the chain window, so the lay joins the old
        // cohort, which re-joins the open phrase and is held from here.
        let now = at(t0, 1700);
        let cx = ctx(now, &c, (2, 6), 0.9);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        let abc_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == abc.id)
            .expect("the resumed cohort");
        assert_eq!(
            abc_now.phrase, second,
            "resuming a line re-joins the open phrase"
        );
        assert_eq!(abc_now.alive_at, now);
        assert_eq!(abc_now.phase, Phase::Laying);
    }

    /// An erase holds the open phrase: through eight Backspaces at 120 ms the band's left end
    /// stays lit and the swoosh begins one rest after the last erase. (An erase after the rest
    /// takes the row back:
    /// `a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it`.)
    #[test]
    fn a_backspace_run_holds_the_phrase_so_the_left_end_stays_lit_through_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let coh = rib.cohorts()[0];
        // Eight Backspaces from +100 ms, 120 ms apart: caret 22 → 14.
        let mut caret = 22u16;
        for i in 0..8u64 {
            caret -= 1;
            let now = at(last, 100 + i * 120);
            erase_at(&mut rib, now, (2, caret), &c);
            let k = rib
                .cohorts()
                .iter()
                .find(|k| k.id == coh.id)
                .expect("the band is held");
            assert_eq!(
                k.alive_at, now,
                "erase {i}: the phrase's clock follows the hand"
            );
            assert_eq!(k.phase, Phase::Laying);
            let cx = ctx(now, &c, (2, caret), 0.9);
            let left = rib
                .cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 2)
                .expect("the left end is in the pool");
            assert!(
                rib.env_of(&cx, left) > 0.99,
                "erase {i} (+{} ms after the last key): the left end is at full ({})",
                100 + i * 120,
                rib.env_of(&cx, left)
            );
        }
        let last_erase = at(last, 100 + 7 * 120);
        // The grace runs from the last erase: in its grace at +0.80 s after it, reaching
        // at +0.95 s.
        let cx = ctx(at(last_erase, 800), &c, (2, 14), 0.0);
        rib.plan(&cx);
        let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
        assert_eq!(k.phase, Phase::Grace);
        let cx = ctx(at(last_erase, 950), &c, (2, 14), 0.0);
        rib.plan(&cx);
        let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
        assert_eq!(
            k.phase,
            Phase::Reaching,
            "the rest is measured from the last erase"
        );
    }

    /// A Backspace run begun after the rest revives the band: the first erase mints a new phrase
    /// over the caret row's cohorts, later erases hold it, and the band outlives the last erase
    /// by a swoosh. Still row-scoped (`OnlyOwnerRenews`).
    #[test]
    fn a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Twenty keys at 10 cps on row 2, cols 2..22; last key at +1900 ms.
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 2,
                n: 20,
                period_ms: 100,
                disp: 0.9,
            },
            &c,
        );
        let id = rib.cohorts()[0].id;
        // A second row under the same hand, one key later and never touched
        // again: the control that the revival renews the caret's row and
        // nothing else (`OnlyOwnerRenews`).
        type_keys(
            &mut rib,
            at(t0, 2000),
            Keys {
                g: geom(),
                row: 5,
                col0: 2,
                n: 1,
                period_ms: 100,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 2000);
        let other = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 5)
            .expect("row 5")
            .alive_at;
        // +1.40 s past the hand's last key: the rest (0.90 s) is spent, row
        // 2's after-glow is under way, and both rows are still resident.
        let glow = at(last, 1400);
        let cx = ctx(glow, &c, (2, 22), 0.0);
        rib.plan(&cx);
        let leaving = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .expect("row 2 is still resident");
        assert!(
            leaving.phase != Phase::Laying && leaving.phase != Phase::Grace,
            "the fixture must erase into a band that is already leaving ({:?})",
            leaving.phase
        );
        let dim = lit_row(&rib, &cx, 2, 1.0).len();
        let full_env = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
            .count();
        // The snap: one Backspace, and the row is laying again at full.
        let bs0 = at(glow, 0);
        erase_at(&mut rib, bs0, (2, 21), &c);
        // Read at the erase's own instant — the same frame `dim` was read
        // on, so the two numbers are a before and an after of one gesture.
        let cx = ctx(bs0, &c, (2, 21), 0.0);
        let k = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .expect("the band is taken back");
        assert_eq!(k.alive_at, bs0, "the clock follows the hand that came back");
        assert_eq!(k.phase, Phase::Laying);
        let back = lit_row(&rib, &cx, 2, 1.0).len();
        let back_env = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
            .count();
        assert!(
            back > dim && back_env > full_env,
            "the erase must SNAP the band back ({dim} -> {back} lit, {full_env} -> {back_env} at full)"
        );
        let left = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 2)
            .expect("the left end is in the pool");
        assert!(
            rib.env_of(&cx, left) > 0.99,
            "…and the left end is back at full ({})",
            rib.env_of(&cx, left)
        );
        // The row control: row 5 kept exactly the clock it had.
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 5)
                .expect("row 5 is resident")
                .alive_at,
            other,
            "an erase on row 2 must not renew row 5"
        );
        // The rest of the run, 120 ms apart, then the tail from the last one.
        let mut caret = 21u16;
        for i in 1..8u64 {
            caret -= 1;
            erase_at(&mut rib, at(glow, i * 120), (2, caret), &c);
        }
        let last_erase = at(glow, 7 * 120);
        let mut tail_ms = 0u64;
        for ms in (0..=2400).step_by(20) {
            let cx = ctx(at(last_erase, ms), &c, (2, caret), 0.0);
            rib.plan(&cx);
            if rib.cells().iter().any(|l| l.row == 2) {
                tail_ms = ms;
            } else {
                break;
            }
        }
        assert!(
            tail_ms >= 1176,
            "the band must outlive the last erase by at least the 1175.8 ms v0.85.0 measured ({tail_ms} ms)"
        );
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        assert!(
            tail_ms <= total + 60,
            "…and by no more than its own swoosh ({tail_ms} ms against {total} ms)"
        );
    }

    /// §2.6, per row: a band on a row below the caret retracts toward its own left end and
    /// drains right-first; one on a row above toward its right end (the fold), left-first.
    /// Neither slides toward another row's column.
    #[test]
    fn a_row_the_caret_is_not_on_retracts_into_its_own_nearest_end_in_logical_order() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let below = |to: (u16, u16)| {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 4, 20, &c, 0.9);
            let last = at(t0, 19 * 60);
            let now = at(last, 100);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav((2, 24), to), now, &cx);
            rib.plan(&cx);
            let k = rib.cohorts()[0];
            let env = |col: u16, ms: u64| {
                let cx = ctx(at(now, ms), &c, to, 0.0);
                let cell = rib
                    .cells()
                    .iter()
                    .find(|l| l.row == 2 && l.col == col)
                    .expect("cell");
                rib.env_of(&cx, cell)
            };
            (k, env(4, 200), env(23, 200))
        };
        // Up: the caret to row 1, col 24 — the band is now below the hand.
        let (k, left, right) = below((1, 24));
        assert!(k.abandoned && k.phase.is_retracting());
        assert_eq!(
            k.retract_col,
            Some(4),
            "a row below the caret pulls into its left end"
        );
        assert!(k.drain_right_first, "…and drains from its right end");
        assert!(
            right < left,
            "200 ms into the retract the right end has spent more than the left ({right} vs {left})"
        );
        // Down: the caret to row 3, col 0 — the band is above the hand.
        let (k, left, right) = below((3, 0));
        assert_eq!(
            k.retract_col,
            Some(23),
            "a row above the caret pulls into its right end — the fold — not toward col 0 on another row"
        );
        assert!(!k.drain_right_first);
        assert!(
            left < right,
            "…and drains from its left end ({left} vs {right})"
        );
    }

    /// The brightest planned boundary strictly inside cell `col` (its interior slabs, which
    /// interpolate the cell's own two edges), `0` once the cell has no interior slab. Not the
    /// quads' per-row alpha: the strip's vertical profile bends with the head's position.
    fn cell_cov(rib: &Ribbon, g: Geom, col: u16) -> u8 {
        let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
        let x1 = x0 + g.cw as f32;
        rib.plan_segments()
            .iter()
            .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
            .map(|s| s.cov)
            .max()
            .unwrap_or(0)
    }

    /// A RETURN LEAVES ITS LINE PRINTED, NOT LIT: eight keys on row 2, then
    /// a Return down to the next line's home: by 100 ms after it the band on
    /// row 2 stands at no more than [`RETURN_PRINTED_SHARE`] of an arrow's
    /// (on paper the band under the line's last word, drawn in a word
    /// decoration's lighter ink, kept it under 4.5:1 through the swoosh's
    /// retract), still lit and with no cell stamped as leaving, so it goes
    /// out through its own swoosh. Negative control: an arrow down from the
    /// same line leaves the band its full light at 100 ms.
    #[test]
    fn a_return_dims_the_line_it_leaves() {
        let c = cfg(false, true);
        let t0 = Instant::now();
        let lit_after = |licence: Licence| -> (u8, bool) {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, 8, &c, 0.9);
            let leave = at(t0, 8 * 60 + 40);
            let cx = ctx(leave, &c, (3, 0), 0.9);
            rib.on_event(&row_change((2, 10), (3, 0), licence), leave, &cx);
            rib.plan(&cx);
            let late = at(leave, 100);
            let cx = ctx(late, &c, (3, 0), 0.9);
            rib.plan(&cx);
            let cov = rib.plan_segments().iter().map(|s| s.cov).max().unwrap_or(0);
            (cov, rib.cells().iter().any(|c| c.row == 2 && c.leaving()))
        };
        let (ret, ret_leaving) = lit_after(Licence::Return);
        let (nav, _) = lit_after(Licence::Nav);
        assert!(nav > 0, "control: an arrow down leaves the band lit");
        assert!(
            f32::from(ret) <= RETURN_PRINTED_SHARE * f32::from(nav) + 2.0,
            "the line a Return left still stands at {ret} of the arrow's {nav} at 100 ms"
        );
        assert!(
            ret > 0 && !ret_leaving,
            "…but dimmed, not cut: it goes out through its own swoosh"
        );
    }

    /// ON PAPER THE RIBBON'S YELLOW IS A YELLOW: the tall body's ink for the
    /// yellow stop, composited at its strongest opacity on GitHub Light and
    /// Default Light, is a clean light yellow (hue 48°..68°, value over 0.85),
    /// never the olive a stop darkened onto one luminance prints; and the
    /// paper caret for a stop just greener than yellow lands on amber.
    /// Negative control: the light body law's ink for the same stop
    /// composites olive.
    #[test]
    fn the_paper_ribbon_keeps_its_yellow_clean() {
        for (fg, bg) in [(0x001F_2328, 0x00FF_FFFF), (0x002B_2D33, 0x00F6_F7F9)] {
            let mut c = cfg(false, true);
            (c.theme_fg, c.theme_bg) = (fg, bg);
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            let a = (cov_cap(&c) + 16.0).min(255.0) as u8;
            let lay = |ink: u32| over_premul(bg, premul_rgb(ink, a), a) & 0x00FF_FFFF;
            // The walk position whose arc is the yellow stop.
            let yellow = (0..BED_INK_LUT_LEN)
                .map(|i| i as f32 / (BED_INK_LUT_LEN - 1) as f32)
                .min_by(|x, y| {
                    let d = |t: f32| {
                        let (h, ..) = rgb2hsv(spectrum(walk_pace(t)));
                        (h - 60.0).abs()
                    };
                    d(*x).total_cmp(&d(*y))
                })
                .expect("a walk");
            let (h, s, v) = rgb2hsv(lay(lut.at(yellow)));
            assert!(
                (48.0..=68.0).contains(&h) && v > 0.85 && s > 0.2,
                "on #{bg:06X} the yellow lays ({h:.0}°, {s:.2}, {v:.2})"
            );
            let law = crate::cursor_glow::light_body_ink(spectrum(walk_pace(yellow)), fg, bg, a);
            let (_, _, lv) = rgb2hsv(lay(law));
            assert!(
                lv < 0.8,
                "control: the body law's yellow is not olive ({lv:.2})"
            );
        }
        let (h, ..) = rgb2hsv(paper_caret_ink(
            crate::color_math::hsv2rgb(68.0, 1.0, 1.0),
            0x00FF_FFFF,
        ));
        assert!(
            h <= 45.0,
            "a greenish yellow's caret sits at hue {h:.1}, not amber"
        );
    }

    #[test]
    fn a_retired_cell_melts_to_exactly_nothing_inside_150_ms_and_never_brightens() {
        // `Ribbon::retire_cells` is the engine's half of the host's content
        // witness. Eight keys on row 2 (cells 10..=17); 40 ms after the last,
        // the two head cells are reported overwritten.
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let fired = at(last, 40);
        let cx = ctx(fired, &c, (2, 18), 0.9);
        rib.plan(&cx);
        assert_eq!(
            rib.slabs_per_cell(),
            SLABS_PER_CELL,
            "interior slabs exist to read"
        );
        let before = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
        assert!(
            before.0 > 0 && before.1 > 0,
            "the head cells are lit: {before:?}"
        );
        let mut control = rib.clone();
        let born_of = |rib: &Ribbon, col: u16| {
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == col)
                .map(|l| l.born)
                .expect("laid")
        };
        let (b16, b17) = (born_of(&rib, 16), born_of(&rib, 17));
        assert_eq!(
            rib.retire_cells(&[(2, 15, fired)], fired),
            0,
            "a cell at a laid position with ANOTHER birth is not it (identity, not position)"
        );
        assert_eq!(
            rib.retire_cells(
                &[(2, 16, b16), (2, 17, b17), (2, 99, fired), (5, 16, b16)],
                fired
            ),
            2,
            "two live cells stamped; a cell that is not laid is not counted"
        );
        assert_eq!(
            rib.retire_cells(&[(2, 16, b16)], at(fired, 8)),
            0,
            "a retired cell is not restamped"
        );
        let mut prev = before;
        let mut gone_by: Option<u64> = None;
        for k in 1..=20u64 {
            let ms = 8 * k;
            let now = at(fired, ms);
            let cx = ctx(now, &c, (2, 18), 0.9);
            rib.plan(&cx);
            control.plan(&cx);
            let here = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
            assert!(
                here.0 <= prev.0 && here.1 <= prev.1,
                "+{ms} ms: the melt is monotone, got {here:?} after {prev:?}"
            );
            assert!(
                here.0 <= before.0 && here.1 <= before.1,
                "+{ms} ms: never brighter than the cell was ({before:?}), got {here:?}"
            );
            prev = here;
            let resident = rib
                .cells()
                .iter()
                .any(|l| l.row == 2 && (l.col == 16 || l.col == 17));
            if here == (0, 0) && !resident && gone_by.is_none() {
                gone_by = Some(ms);
            }
            // Cells two or more columns from the retired pair share no
            // boundary with it: byte-identical to the control that never
            // retired anything.
            for col in [10u16, 11, 12, 13] {
                assert_eq!(
                    cell_cov(&rib, g, col),
                    cell_cov(&control, g, col),
                    "+{ms} ms: cell {col} is untouched by the retirement"
                );
            }
        }
        assert!(
            gone_by.is_some_and(|ms| ms <= 150),
            "off the glass and out of the pool inside 150 ms, got {gone_by:?}"
        );
        assert!(
            rib.cells().iter().any(|l| l.row == 2 && l.col == 15),
            "the un-retired band is still resident"
        );
    }

    /// A released run leaves through the swoosh's retract, unstamped: its cohort is abandoned
    /// (not rejoinable), a cell already retracting keeps its clock, and the row is out of the
    /// pool `RETRACT_DUR_S + RETRACT_FADE_S` later, never cut on `RETIRE_MELT_S`.
    #[test]
    fn a_released_run_is_abandoned_into_its_retract_and_a_cell_already_leaving_keeps_its_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let last = at(t0, 5 * 60);
        // Backspace: cell 15 retracts on its own 0.24 s clock.
        erase_at(&mut rib, at(last, 50), (2, 15), &c);
        let stamp_15 = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15)
            .and_then(|l| l.retract_at);
        assert!(stamp_15.is_some(), "the erased cell is retracting");
        let named: Vec<(u16, u16, Instant)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2)
            .map(|l| (l.row, l.col, l.born))
            .collect();
        let released = rib.release_cells(&named, at(last, 60));
        assert_eq!(
            released, 5,
            "the five live cells; the retracting one keeps its clock"
        );
        assert!(
            rib.cells()
                .iter()
                .all(|l| l.row != 2 || l.retire_at.is_none()),
            "not one cell is stamped on the melt"
        );
        assert_eq!(
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 15)
                .and_then(|l| l.retract_at),
            stamp_15,
            "the retracting cell keeps its own clock"
        );
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .copied()
            .expect("the row's cohort");
        assert!(coh.abandoned, "the cohort is abandoned");
        assert!(!coh.rejoinable, "…and not rejoinable: the text is gone");
        let cx = ctx(at(last, 76), &c, (2, 15), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 2)
                .is_some_and(|k| k.phase.is_retracting()),
            "on the next plan the cohort is in its retract"
        );
        assert!(
            rib.cells().iter().filter(|l| l.row == 2).count() >= 5,
            "…and the five cells are still resident, drawn in, not cut"
        );
        // A second release of the same cells restarts nothing: the clock
        // the cohort has is the clock it keeps.
        let alive_at = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .map(|k| k.alive_at);
        rib.release_cells(&named, at(last, 90));
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 2)
                .map(|k| k.alive_at),
            alive_at,
            "a repeat release does not rewind the clock again"
        );
        let cx = ctx(
            at(
                last,
                60 + ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 20,
            ),
            &c,
            (2, 15),
            0.9,
        );
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "the released run is out of the pool the retract's own span later, \
             not {RETIRE_MELT_S} s later"
        );
    }

    /// A partial release is restored by the stamp's own cells, not the cohort's count: with one
    /// of six cells released and a key joining the cohort inside the melt, the stamp lifts and
    /// the cohort's clock is untouched.
    #[test]
    fn a_partial_release_is_restored_by_its_stamped_cells_when_a_key_joined_the_cohort() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let last = at(t0, 5 * 60);
        let id = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .map(|k| k.id)
            .expect("the row's cohort");
        let born_15 = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15)
            .map(|l| l.born)
            .expect("cell 15");
        let cell_15 = |rib: &Ribbon| {
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 15 && l.born == born_15)
                .copied()
                .expect("cell 15 is resident")
        };
        let stamp = at(last, 60);
        assert_eq!(
            rib.release_cells(&[(2, 15, born_15)], stamp),
            1,
            "one cell stamped"
        );
        assert_eq!(cell_15(&rib).released_at, Some(stamp));
        assert!(cell_15(&rib).leaving(), "…and leaving");
        // A key typed inside the melt, past the caret: it joins the cohort,
        // which now has seven cells where the release counted six.
        let key = at(last, 80);
        let cx = ctx(key, &c, (2, 17), 0.9);
        rib.on_event(&typed(), key, &cx);
        rib.plan(&cx);
        let on_row: Vec<(u16, u32)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2)
            .map(|l| (l.col, l.cohort))
            .collect();
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == 2).count(),
            1,
            "one cohort on the row: {on_row:?}"
        );
        let count = rib.cells().iter().filter(|l| l.cohort == id).count();
        assert_eq!(count, 7, "the cohort grew by one: {on_row:?}");
        let alive_at = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .map(|k| k.alive_at);
        // The witness's candidate: the part, by the stamp it laid, while
        // the cohort around it has grown by a key.
        let mut accepted = Vec::new();
        rib.restore_releases(&[(id, Some(stamp))], &mut accepted);
        assert_eq!(
            accepted,
            vec![id],
            "the restore is accepted by the stamp's own cells, the cohort's count notwithstanding"
        );
        assert!(!cell_15(&rib).leaving(), "the stamp is lifted");
        assert_eq!(cell_15(&rib).released_at, None);
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .copied()
            .expect("the cohort");
        assert!(coh.release_clock.is_none(), "the clock is handed back");
        assert_eq!(Some(coh.alive_at), alive_at, "…and was never touched");
        assert!(!coh.abandoned);
        // A second restore of the same candidate has nothing to lift.
        rib.restore_releases(&[(id, Some(stamp))], &mut accepted);
        assert!(accepted.is_empty(), "nothing is released any more");
    }

    #[test]
    fn a_relay_over_a_content_retired_cell_has_a_fresh_witness_identity() {
        use super::super::witness::{RowSample, Witness};

        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 2, &c, 0.9);
        let born = |rib: &Ribbon, col| {
            rib.cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .expect("typed cell")
                .born
        };
        let old = born(&rib, 10);
        let neighbor = born(&rib, 11);
        let mut witness = Witness::new();
        let mut retired = Vec::new();
        let mut released = Vec::new();
        let original: Vec<char> = "          ab".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &original,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty());
        let fired = at(t0, 100);
        assert_eq!(rib.retire_cells(&[(2, 10, old)], fired), 1);
        let fresh = at(t0, 116);
        rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
        assert_eq!(born(&rib, 10), fresh, "a retired identity cannot be re-wet");
        assert_eq!(born(&rib, 11), neighbor, "the neighboring key is untouched");
        assert_eq!(rib.retire_cells(&[(2, 10, old)], fresh), 0);

        // Even an old witness still resident at this position must arm the
        // new glyph under its new birth, not retire it as the old overwrite.
        let replacement: Vec<char> = "          Xb".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &replacement,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty(), "the fresh key arms its own record");
        let changed: Vec<char> = "          Yb".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &changed,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert_eq!(
            retired,
            vec![(2, 10, fresh)],
            "later changes retire the new identity"
        );
    }

    #[test]
    fn a_relocation_pays_a_content_retired_destination_and_preserves_live_neighbors() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 3, &c, 0.9);
        let before: Vec<_> = rib
            .cells()
            .iter()
            .map(|cell| (cell.col, cell.born))
            .collect();
        let old = before
            .iter()
            .find(|&&(col, _)| col == 11)
            .expect("middle")
            .1;
        let fired = at(t0, 160);
        assert_eq!(rib.retire_cells(&[(2, 11, old)], fired), 1);
        let fresh = at(t0, 176);
        rib.relay_word(2, 10, 13, fresh, None);
        for col in 10..13 {
            let cell = rib
                .cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .expect("relocated word");
            assert!(
                !cell.leaving(),
                "column {col} belongs to the relocated word"
            );
            let expected = if col == 11 {
                fresh
            } else {
                before.iter().find(|&&(c, _)| c == col).expect("neighbor").1
            };
            assert_eq!(
                cell.born, expected,
                "only the retired destination is paid again"
            );
        }
        assert_eq!(
            rib.retire_cells(&[(2, 11, old)], fresh),
            0,
            "stale identity has no authority over the relocation"
        );
    }

    #[test]
    fn a_live_retype_with_a_changed_glyph_does_not_inherit_the_old_witness() {
        use super::super::witness::{RowSample, Witness};

        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 1, &c, 0.9);
        let mut witness = Witness::new();
        let mut retired = Vec::new();
        let mut released = Vec::new();
        let original: Vec<char> = "          a".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &original,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty());
        let before_ctx = ctx(at(t0, 99), &c, (2, 11), 0.9);
        rib.plan(&before_ctx);
        let before = cell_cov(&rib, geom(), 10);
        assert!(before > 0, "the old glyph is visibly lit");
        let old_attack = rib.cells()[0].attack_at;
        let fresh = at(t0, 100);
        rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
        let after_ctx = ctx(at(t0, 101), &c, (2, 11), 0.9);
        rib.plan(&after_ctx);
        assert_eq!(rib.cells()[0].born, fresh, "fresh key, fresh identity");
        assert_eq!(rib.cells()[0].attack_at, old_attack);
        assert!(
            cell_cov(&rib, geom(), 10) >= before,
            "re-wetting cannot dim the old light"
        );
        assert!(
            !rib.brisk(after_ctx.now),
            "a settled attack is not restarted for scheduling either"
        );
        let replacement: Vec<char> = "          X".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &replacement,
            }],
            &[],
            Instant::now(),
            &mut retired,
            &mut released,
        );
        assert!(
            retired.is_empty(),
            "a genuine retyped glyph must not be mistaken for an unlicensed overwrite: {retired:?}"
        );
    }

    #[test]
    fn focus_regained_inside_the_ember_comes_back_up_through_edge_in_not_a_snap() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let last = at(t0, 5 * 60);
        let cx = ctx(at(last, 100), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let full = plan_peak(&rib);
        let full_sum = plan_sum(&rib);
        rib.on_event(&Event::Focus(false), at(last, 100), &cx);
        let cx = ctx(at(last, 250), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let dimmed = plan_peak(&rib);
        let dim_sum = plan_sum(&rib);
        // The ember dims each stop into its own range (on the default ground a chartreuse
        // stop's bar and colour floor are close), so "well under way" is read in the band's
        // own range: a twin embered to its last frame is where this band's light is heading.
        let mut twin = Ribbon::new();
        type_run(&mut twin, t0, 10, 6, &c, 0.8);
        let blur_cx = ctx(at(last, 100), &c, (2, 16), 0.8);
        twin.on_event(&Event::Focus(false), at(last, 100), &blur_cx);
        let end_cx = ctx(at(last, 399), &c, (2, 16), 0.8);
        twin.plan(&end_cx);
        let floor_sum = plan_sum(&twin);
        let range = full_sum.saturating_sub(floor_sum);
        let burned = full_sum.saturating_sub(dim_sum);
        assert!(
            dimmed < full,
            "the ember must be burning ({full} → {dimmed})"
        );
        assert!(
            range > 0 && floor_sum > 0,
            "the fixture must leave the band SOME range to spend and some light at the floor \
             ({full_sum} → {floor_sum})"
        );
        // `spend` is `(1 − u)²`, so at the halfway instant exactly three
        // quarters of the band's range is gone. Within a twentieth, the
        // rounding of a coverage byte per cell.
        assert!(
            burned as f32 >= range as f32 * (1.0 - spend(0.5) - 0.05),
            "the ember must be well under way: {burned} of {range} spendable \
             ({full_sum} → {dim_sum}, floor {floor_sum})"
        );
        rib.on_event(&Event::Focus(true), at(last, 250), &cx);
        let cx = ctx(at(last, 251), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let back = plan_peak(&rib);
        assert!(
            back <= dimmed + 3,
            "focus regain snapped the ribbon from {dimmed} to {back} in one frame"
        );
        let cx = ctx(at(last, 290), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let rearmed = plan_peak(&rib);
        assert!(
            rearmed + 3 >= full,
            "after the edge-in the ribbon must be back at its level ({full} vs {rearmed})"
        );
    }

    // -- the focus ember leaves as one object -------------------------------

    /// The paint census's rule, independent of this module: a pixel is paint
    /// when `max(r, g, b) >= 60 && max - min >= 40`, and a cell is lit when a
    /// fifth of its bed pixels are paint. On a light theme the level half is
    /// read in the paper's polarity (`min <= 255 - 60`); the chroma half is
    /// unchanged.
    ///
    /// Stated in absolute bytes, not via [`seen`]: [`seen_floor`] inverts
    /// `seen`, so a test judged by it would pass by construction.
    fn census_paint(px: u32, light: bool) -> bool {
        let c = |sh: u32| f32::from(((px >> sh) & 0xff) as u8);
        let (r, g, b) = (c(16), c(8), c(0));
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let level = if light {
            mn <= 255.0 - 60.0
        } else {
            mx >= 60.0
        };
        level && mx - mn >= 40.0
    }

    /// Columns of `row` whose bed composites as paint over the theme's ground
    /// ([`census_paint`], read against the bytes the renderer will send) at
    /// `scale` times the coverage the ribbon reports on this frame.
    fn lit_row(rib: &Ribbon, cx: &Ctx<'_>, row: u16, scale: f32) -> Vec<u16> {
        let bg = cx.cfg.theme_bg;
        let light = !cx.cfg.dark_theme;
        let mut out: Vec<u16> = Vec::new();
        for cell in rib.cells().iter().filter(|c| c.row == row) {
            let coh = rib.cohorts.iter().find(|c| c.id == cell.cohort);
            let a = (rib.cov_of(cx, cell, coh) * scale)
                .round()
                .clamp(0.0, 255.0) as u8;
            if a > 0 && census_paint(over_premul(bg, premul_rgb(rib.ink.at(cell.t), a), a), light) {
                out.push(cell.col);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The longest dark run strictly inside `lit`'s own span, in cells.
    fn dark_run(lit: &[u16]) -> u16 {
        let (mut worst, mut run) = (0u16, 0u16);
        let (Some(&first), Some(&last)) = (lit.first(), lit.last()) else {
            return 0;
        };
        for col in first..=last {
            if lit.contains(&col) {
                run = 0;
            } else {
                run += 1;
                worst = worst.max(run);
            }
        }
        worst
    }

    /// The band leaves as one object: through every frame of the focus ember its painted cells
    /// are one interval, and it goes out whole on the ember's last frame. Judged by
    /// [`census_paint`], not [`seen`]; the falsifier, a bare `spend` over the same frames,
    /// splits the band.
    #[test]
    fn the_focus_ember_never_opens_a_dark_run_inside_the_band() {
        for (theme, fg, bg) in [
            ("default #111318", SHIPPED_FG, SHIPPED_BG),
            ("Nord", NORD_FG, NORD_BG),
        ] {
            let mut c = cfg(true, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            // Sixty cells, so the walk folds twice and the warm stops land in
            // the middle of the band as well as at its ends: a hole needs lit
            // cells on both sides.
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g: geom(),
                    row: 2,
                    col0: 2,
                    n: 60,
                    period_ms: 40,
                    disp: 0.9,
                },
                &c,
            );
            let last = at(t0, 59 * 40);
            let caret = (2u16, 62u16);
            let cx = ctx(at(last, 60), &c, caret, 0.9);
            rib.plan(&cx);
            let full = lit_row(&rib, &cx, 2, 1.0);
            assert!(
                full.len() >= 40,
                "{theme}: the fixture's band is lit before the blur ({} cells)",
                full.len()
            );
            assert_eq!(dark_run(&full), 0, "{theme}: the fixture starts whole");
            let blur = at(last, 60);
            rib.on_event(&Event::Focus(false), blur, &cx);
            let (mut worst_new, mut worst_bare, mut bare_split_frames) = (0u16, 0u16, 0u32);
            let mut shrunk_while_lit = Vec::new();
            for ms in (0..=300).step_by(4) {
                let now = at(blur, ms);
                let cx = ctx(now, &c, caret, 0.0);
                rib.plan(&cx);
                let burned = ms as f32 / 1000.0 / FOCUS_EMBER_S;
                let now_lit = lit_row(&rib, &cx, 2, 1.0);
                // The falsifier: the same frame with the ember lifted off the
                // envelope and scaled by a bare `spend` instead of the
                // per-stop floor.
                let burning = rib.ember_at.take();
                let bare_lit = lit_row(&rib, &cx, 2, spend(clamp01(burned)));
                rib.ember_at = burning;
                worst_new = worst_new.max(dark_run(&now_lit));
                let bd = dark_run(&bare_lit);
                worst_bare = worst_bare.max(bd);
                if bd >= 2 {
                    bare_split_frames += 1;
                }
                if !now_lit.is_empty() && now_lit != full {
                    shrunk_while_lit.push((ms, now_lit.len()));
                }
            }
            assert_eq!(
                worst_new, 0,
                "{theme}: the ember opened a dark run of {worst_new} cells inside the band"
            );
            assert!(
                shrunk_while_lit.is_empty(),
                "{theme}: cells left the band before the end of the ember: {shrunk_while_lit:?}"
            );
            let gone = ctx(at(blur, 301), &c, (2, 62), 0.0);
            rib.plan(&gone);
            assert!(
                lit_row(&rib, &gone, 2, 1.0).is_empty(),
                "{theme}: the ember's end takes every cell"
            );
            // The falsifier: a bare-spend ember splits this same band.
            assert!(
                worst_bare >= 2 && bare_split_frames >= 2,
                "{theme}: the fixture cannot show the defect it pins \
                 (worst bare-spend dark run {worst_bare} cells over {bare_split_frames} frames)"
            );
        }
    }

    #[test]
    fn the_hot_edge_embers_out_with_its_body_on_focus_loss() {
        // §8.2 "Focus lost | ember in 300 ms | spend": the hairline is the
        // head's property and takes the head cell's envelope, so it spends
        // with the body instead of holding over a spent body and vanishing
        // with the pool.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let lost = at(t0, 7 * 60 + 100);
        let mut sink = Sink::default();
        let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| {
            let cx = ctx(now, &c, (2, 18), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        };
        frame(&mut rib, &mut sink, lost);
        let full = peak_channel(&sink.out);
        assert!(
            full > 0,
            "a hot hand must leave a hot edge for the law to have been tested"
        );
        rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
        frame(&mut rib, &mut sink, at(lost, 150));
        let mid = peak_channel(&sink.out);
        assert!(
            mid < full,
            "halfway through the ember the hot edge must already be spending ({full} → {mid})"
        );
        frame(&mut rib, &mut sink, at(lost, 250));
        assert!(
            !sink.under.is_empty(),
            "the body must still be on glass for the pin to mean anything"
        );
        let low = peak_channel(&sink.out);
        // The hairline's share of its own start is exactly the head cell's
        // envelope; under the per-stop ember floor the head's stop keeps the
        // range its ink has, and the hairline keeps exactly that.
        let cx = ctx(at(lost, 250), &c, (2, 18), 0.9);
        let head = rib
            .cells()
            .iter()
            .max_by_key(|l| l.col)
            .copied()
            .expect("the head is in the pool");
        let env = rib.env_of(&cx, &head);
        let share = low as f32 / full as f32;
        assert!(
            (share - env).abs() < 0.06,
            "+250 ms into the ember the hot edge is at {low} of {full} (share {share:.3}); \
             it must ride the head cell's own envelope ({env:.3}), not a clock of its own"
        );
        // "Going out with its body" is a claim about the clock, not the head stop's range
        // (the theme's): `spend` is `(1 − u)²`, so at +250 ms of 300 the ember has spent 97 %
        // of the range and the envelope is within a few hundredths of its final value.
        let end_cx = ctx(at(lost, 299), &c, (2, 18), 0.9);
        let env_end = rib.env_of(&end_cx, &head);
        let law = env_end + (1.0 - env_end) * spend(250.0 / 300.0);
        assert!(
            low <= mid && (env - law).abs() < 0.03 && env_end < 1.0,
            "…and it must be going out with its body on the ember's own clock, not holding \
             until the pool clears ({full} → {mid} → {low}, envelope {env:.3}, the ember's \
             law at this instant {law:.3}, its floor {env_end:.3})"
        );
        frame(&mut rib, &mut sink, at(lost, 300));
        assert!(
            sink.out.is_empty()
                && rib.at_rest()
                && rib.next_change_deadline(at(lost, 300)).is_none(),
            "…and at the ember's end there is nothing, hairline included, and no wake asked for"
        );
    }

    /// The focus seam's wake clause (v3 step 1): the host forwards raw focus
    /// (`CursorGlow::note_focus`), and an unfocused window being typed into (a control-socket
    /// agent's) keeps its light, so a typed key under a burning ember clears it and re-lights
    /// from the level the light had, through `edge-in`.
    #[test]
    fn a_typed_key_under_a_burning_ember_clears_it_and_relights_from_the_level() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let lost = at(t0, 7 * 60 + 100);
        rib.plan(&ctx(lost, &c, (2, 18), 0.9));
        let pre = plan_peak(&rib);
        rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
        let cx = ctx(at(lost, 150), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mid = plan_peak(&rib);
        let full = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
        // Halfway through, the body is below its level at the blur, not necessarily below
        // the cold-body ceiling: the peak is the run's stop with the least range between the
        // bar and the colour floor (on a dark ground a warm stop's range is a fifth of it).
        assert!(
            mid > 0 && mid < pre,
            "halfway through the ember the body is spending ({pre} → {mid}, cold ceiling {full})"
        );
        // A key at +150 ms: the hand is on the window, focused or not.
        let key = at(lost, 150);
        let cx = ctx(key, &c, (2, 19), 0.9);
        rib.on_event(&typed(), key, &cx);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= mid,
            "the key re-arms the run from the ember's level, never below it ({} < {mid})",
            plan_peak(&rib)
        );
        let cx = ctx(at(key, 30), &c, (2, 19), 0.9);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= full - 2,
            "one edge-in after the key the run is back at its ceiling ({})",
            plan_peak(&rib)
        );
        let cx = ctx(at(lost, 320), &c, (2, 19), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.at_rest() && plan_peak(&rib) >= full - 2,
            "past the ember's end the ribbon is still on the glass: the key cleared the ember"
        );
    }

    /// The curtain (v3 §2.8, A2) gathers the band into its hand over `CURTAIN_S` with no
    /// cliff and lays nothing meanwhile. Read in the instruments' quantities
    /// ([`Ribbon::lit_segments`], [`Ribbon::ink_segments`]), since a geometric `cell_lit` cannot
    /// see a brightness cliff: every frame's step bounded by [`CURTAIN_MAX_STEP_FRAC`], ink on at
    /// least [`CURTAIN_MIN_FRAMES`] frames.
    #[test]
    fn a_curtain_gathers_the_band_into_its_hand_over_a_quarter_second_with_no_cliff_and_lays_nothing()
     {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        // The caret has already moved when the host notices the space ended (the alt-screen
        // path's Return put it at the next row's column 0): the curtain gathers into the
        // band's head, not 30 columns over to the caret.
        let caret = (3u16, 0u16);
        let mut sink = Sink::default();
        #[derive(Clone, Copy)]
        struct Sample {
            peak: u8,
            sum: u32,
            left: f32,
            cells: usize,
            segs: usize,
            ink: usize,
        }
        let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| -> Sample {
            let cx = ctx(now, &c, caret, 0.9);
            rib.plan(&cx);
            let segs = rib.lit_segments().count();
            let ink = rib.ink_segments().count();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            Sample {
                peak: sink.under.iter().map(|q| q.alpha).max().unwrap_or(0),
                sum: sink.under.iter().map(|q| u32::from(q.alpha)).sum(),
                left: sink
                    .under
                    .iter()
                    .map(|q| f32::from(q.x))
                    .fold(f32::INFINITY, f32::min),
                cells: (0..40u16)
                    .filter(|&col| cell_lit(&sink.under, g, 2, col))
                    .count(),
                segs,
                ink,
            }
        };
        let before = frame(&mut rib, &mut sink, at(last, 8));
        assert!(
            before.peak > 200 && before.cells == 20 && before.segs >= 20,
            "the band is whole before the seam (peak {}, {} cells, {} segments)",
            before.peak,
            before.cells,
            before.segs
        );
        let fell = at(last, 16);
        rib.curtain(fell);
        assert!(rib.curtained());
        assert!(rib.brisk(fell), "a falling curtain asks for every frame");
        assert!(
            rib.next_change_deadline(fell)
                .is_some_and(|d| d <= fell + Duration::from_secs_f32(CURTAIN_S)),
            "…and books its end"
        );
        // Nothing is laid under the curtain: a key, an echo, a jump.
        let cells = rib.cells().len();
        let cx = ctx(at(fell, 40), &c, (3, 1), 0.9);
        rib.on_event(&typed(), at(fell, 40), &cx);
        rib.on_event(
            &Event::Sweep {
                row: 3,
                col0: 0,
                col1: 1,
            },
            at(fell, 40),
            &cx,
        );
        rib.on_event(&nav((3, 1), (3, 40)), at(fell, 40), &cx);
        assert_eq!(rib.cells().len(), cells, "a curtain lays nothing");
        // Ten frames at 30 fps, past the end of the fall: the light falls every frame, the
        // left end is drawn toward the band's head, and neither segment count steps more than
        // `CURTAIN_MAX_STEP_FRAC` of what it fell on.
        let mut prev = before;
        let mut worst_step = 0usize;
        let mut worst_ink_step = 0usize;
        let mut ink_frames = 0usize;
        let mut dark_at = None;
        for ms in [33u64, 66, 99, 132, 165, 198, 231, 264, 297, 330] {
            let now = frame(&mut rib, &mut sink, at(fell, ms));
            assert!(
                now.peak <= prev.peak,
                "+{ms} ms: the curtain never brightens ({} → {})",
                prev.peak,
                now.peak
            );
            // Never brightens. The tolerance is geometric: the suck-in moves
            // every boundary, so the slab lattice under a shortening run
            // re-samples and the summed alpha can wobble by a few parts in ten
            // thousand while every cell's envelope is monotone.
            assert!(
                now.sum <= prev.sum + prev.sum / 500,
                "+{ms} ms: the band's total light rose ({} → {})",
                prev.sum,
                now.sum
            );
            if now.sum > 0 {
                assert!(
                    now.left >= prev.left - 0.5,
                    "+{ms} ms: the band is drawn toward its head, never away ({} → {})",
                    prev.left,
                    now.left
                );
            }
            let step = prev.segs.saturating_sub(now.segs);
            let ink_step = prev.ink.saturating_sub(now.ink);
            worst_step = worst_step.max(step);
            worst_ink_step = worst_ink_step.max(ink_step);
            let bar = CURTAIN_MAX_STEP_FRAC * before.segs.max(before.ink) as f32;
            assert!(
                step as f32 <= bar && ink_step as f32 <= bar,
                "+{ms} ms: the curtain stepped {step} claimed / {ink_step} inked segments in one \
                 frame ({} → {} / {} → {}); the bound is {bar:.1} \
                 ({CURTAIN_MAX_STEP_FRAC} of the {} it fell on)",
                prev.segs,
                now.segs,
                prev.ink,
                now.ink,
                before.segs.max(before.ink)
            );
            if now.ink > 0 {
                ink_frames += 1;
            }
            if now.ink == 0 && dark_at.is_none() {
                dark_at = Some(ms);
            }
            prev = now;
        }
        // Not vacuous: the fall must actually have been a fall — the counts
        // reached zero inside the sweep, and they did it in steps.
        assert!(
            dark_at.is_some_and(|ms| ms >= 132),
            "the band went dark at {dark_at:?} ms — a curtain lasts CURTAIN_S"
        );
        assert!(
            worst_ink_step > 0,
            "the sweep never saw the band leave: it was already dark"
        );
        assert!(
            prev.sum * 8 < before.sum,
            "the fall spent nothing: {} → {} over the whole sweep",
            before.sum,
            prev.sum
        );
        assert!(
            ink_frames >= CURTAIN_MIN_FRAMES,
            "the curtain drew ink on {ink_frames} of the swept frames; the law is \
             {CURTAIN_MIN_FRAMES} (a cut draws one, a one-clock curtain two)"
        );
        // Exactly zero at the end, the pool empty, the next key lays afresh.
        let end = frame(&mut rib, &mut sink, at(fell, 250));
        assert_eq!((end.peak, end.segs, end.ink), (0, 0, 0));
        assert!(rib.at_rest() && !rib.curtained());
        let after = at(fell, 300);
        let cx = ctx(after, &c, (4, 6), 0.9);
        rib.on_event(&typed(), after, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.cells().len(),
            1,
            "after the curtain a key lays its cell"
        );
        // The gather is into the band's own head (row 2's columns 10..29, so column 30's
        // left edge), never on the seam's caret. Two caret shapes: (3, 0), the next row (the
        // alt-screen path), and (2, 0), the band's own row (a carriage-return redraw or a
        // resize), which defeats a per-row read of the caret.
        for seam_caret in [(3u16, 0u16), (2, 0)] {
            let mut rib2 = Ribbon::new();
            type_run(&mut rib2, t0, 10, 20, &c, 0.9);
            // The host's last observed caret before the space ended.
            let cx = ctx(at(last, 8), &c, seam_caret, 0.9);
            rib2.plan(&cx);
            rib2.curtain(fell);
            let near_end = at(fell, 216);
            let cx = ctx(near_end, &c, seam_caret, 0.9);
            rib2.plan(&cx);
            {
                let mut f2 = sink.frame();
                rib2.emit(&cx, &mut f2);
            }
            let left_x = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            let head_x = f32::from(g.origin_x) + 30.0 * cw;
            assert!(
                !sink.under.is_empty(),
                "seam caret {seam_caret:?}: the band was gone before the gather could be read"
            );
            assert!(
                (left_x - head_x).abs() <= 3.0 * cw,
                "seam caret {seam_caret:?}: the band gathered to x {left_x:.1}, not to its own \
                 head at {head_x:.1} (column 0 is x {:.1}, {:.0} cells away)",
                f32::from(g.origin_x),
                (head_x - f32::from(g.origin_x)) / cw
            );
        }
    }

    // -- §4, D14: one emitter, two profiles --------------------------------

    /// The `… underline` spelling is the tall body cut above the spine: the
    /// same cell, age and spine give the same peak coverage under both
    /// profiles ([`Ribbon::body_profile`]), and only the tall body's top edge
    /// rises above the underline's.
    #[test]
    fn tall_and_underline_price_light_alike_and_differ_only_in_reach_above_the_spine() {
        let t0 = Instant::now();
        let mut peaks = Vec::new();
        let mut tops = Vec::new();
        for c in [cfg(true, true), cfg(true, false)] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 6, &c, 0.7);
            let cx = ctx(at(t0, 5 * 60), &c, (2, 16), 0.7);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            peaks.push(sink.under.iter().map(|q| q.alpha).max().unwrap_or(0));
            tops.push(sink.under.iter().map(|q| q.y).min().unwrap_or(0));
        }
        assert_eq!(
            peaks[0], peaks[1],
            "the tall and underline spellings must price light identically"
        );
        assert!(
            tops[0] < tops[1],
            "the tall body's top edge must sit above the underline's ({tops:?})"
        );
    }

    // -- §3.3: the light fork ---------------------------------------------

    /// THE LIGHT BED KEEPS THE THEME'S TEXT AT THE BAR OVER IT: the tall
    /// body on a light ground stands under the letters, so every entry of
    /// its ink table, composited at the body's strongest opacity (its role's
    /// ceiling plus one Bayer level) over the theme's paper, leaves the
    /// theme's text at [`BODY_CONTRAST_BAR`] — on GitHub Light and a sweep of
    /// dark foregrounds on white and cream — and on Solarized Light, whose
    /// own text cannot meet the bar, every stop still reads as a colour.
    /// Negative control: the old table (conservative ink, never solved
    /// against the text) put near-black text under 3:1 over its blue.
    #[test]
    fn the_light_bed_keeps_theme_ink_over_it_at_the_bar() {
        fn contrast(a: u32, b: u32) -> f32 {
            let (x, y) = (relative_luminance(a), relative_luminance(b));
            let (hi, lo) = if x > y { (x, y) } else { (y, x) };
            (hi + 0.05) / (lo + 0.05)
        }
        let blend_at = |c: &Config, ink: u32| {
            let a = (cov_cap(c) + 16.0).min(255.0) as u8;
            over_premul(c.theme_bg, premul_rgb(ink, a), a) & 0x00FF_FFFF
        };
        let mut themes = vec![(0x001F_2328, 0x00FF_FFFF)];
        for bg in [0x00FF_FFFF, 0x00FD_F6E3] {
            for grey in (0..=0x33u32).step_by(0x11) {
                themes.push(((grey << 16) | (grey << 8) | grey, bg));
            }
        }
        for (fg, bg) in themes {
            let mut c = cfg(false, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            for &ink in &lut.lut {
                let blend = blend_at(&c, ink);
                assert!(
                    contrast(fg, blend) >= BODY_CONTRAST_BAR,
                    "{ink:06x} under {fg:06x} on {bg:06x}: {:.2}:1",
                    contrast(fg, blend)
                );
            }
        }
        let mut sol = cfg(false, true);
        sol.theme_fg = 0x0065_7B83;
        sol.theme_bg = 0x00FD_F6E3;
        let mut lut = BedInkLut::default();
        lut.sync(&sol);
        for &ink in &lut.lut {
            assert!(seen(blend_at(&sol, ink), sol.theme_bg), "{ink:06x} reads");
        }
        let old = blend_at(&cfg(false, true), InkRole::OverText.ink(0x0000_00FF));
        assert!(contrast(0x001F_2328, old) < 3.0, "the old table's blue");
    }

    #[test]
    fn a_light_theme_ribbon_is_source_over_ink_and_never_additive() {
        let c = cfg(false, false);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let now = at(t0, 5 * 60);
        let cx = ctx(now, &c, (2, 16), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(!sink.under.is_empty(), "the light rail still draws");
        assert!(
            sink.out.is_empty(),
            "no additive light on white — L6, and the hot edge is dark-only"
        );
        let cap = InkRole::Leading.alpha_cap() as u8;
        for q in &sink.under {
            assert!(q.alpha > 0, "every light-theme quad composites SOURCE-OVER");
            assert!(q.alpha <= cap, "light alpha {} over the {cap} cap", q.alpha);
        }
    }

    // -- §30: the comet, the vivid rail, the attack, the gate ----------------

    /// The composited colour of `under` at device pixel `(x, y)`: every quad
    /// covering it, in emission order, over the default ground (Over for a
    /// source-over quad, additive for an additive one), the CPU reference's
    /// own blend.
    fn composite_at(under: &[GlowQuad], x: f32, y: f32) -> u32 {
        use aterm_render::add_sat;
        let (px, py) = (x.floor(), y.floor());
        under
            .iter()
            .filter(|q| {
                let qx = f32::from(q.x);
                (qx..qx + f32::from(q.w)).contains(&px) && f32::from(q.y) == py
            })
            .fold(DEFAULT_BG, |acc, q| {
                if q.alpha == 0 {
                    add_sat(acc, q.color)
                } else {
                    over_premul(acc, q.color, q.alpha)
                }
            })
    }

    /// The glyph box stays at the bar over the emitted frame: the vivid rail
    /// rides below the row bottom, and every device row a letter can touch
    /// (the typed row and the reach into the row above) composites the text
    /// at `BODY_CONTRAST_BAR` at every column of the run.
    #[test]
    fn the_glyph_box_stays_at_the_bar_over_the_emitted_rail() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let g = geom();
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let row_top = f32::from(g.origin_y) + 2.0 * ch;
        let row_bottom = row_top + ch;
        // The glyph box stays at the bar: every device row above the row
        // bottom (the typed row and the reach into the row above) composites
        // the text at 5.25:1 at every column of the run. The rail's top is
        // pinned at the row bottom, so the wave's crest cannot lift it into
        // the descenders.
        let mut worst = f32::INFINITY;
        let mut worst_at = (0.0, 0.0);
        let mut x = f32::from(g.origin_x) + 10.0 * cw;
        while x < f32::from(g.origin_x) + 18.0 * cw {
            let mut y = row_top - 0.2 * ch;
            while y < row_bottom {
                let lit = composite_at(&sink.under, x, y);
                let k = contrast(DEFAULT_FG, lit);
                if k < worst {
                    worst = k;
                    worst_at = (x, y - row_bottom);
                }
                y += 1.0;
            }
            x += 1.0;
        }
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the text composites at {worst:.3}:1 at (x {}, {:.0} px above the row bottom); the bar is {BODY_CONTRAST_BAR}:1 over every row a letter can touch",
            worst_at.0,
            -worst_at.1
        );
        // (A "no vivid quad above the row bottom" check on the max channel is
        // not a pin: the bed's own blue composites at 238 there. The bar
        // sweep above is the claim.)
    }

    /// The gate (§30): the flat spelling draws no rail: nothing below the flat
    /// body's own reach, nothing vivid anywhere in `under`.
    #[test]
    fn the_flat_spelling_draws_no_rail() {
        let c = flat(true, true);
        assert!(!Ribbon::comet(&c) && !Ribbon::rail_lit(&c));
        assert!(Ribbon::comet(&cfg(true, true)) && Ribbon::rail_lit(&cfg(true, true)));
        assert!(
            !Ribbon::rail_lit(&cfg(false, true)),
            "the light fork keeps the bed alone below the baseline (L6)"
        );
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let g = geom();
        let ch = g.ch as f32;
        let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
        for q in &sink.under {
            assert!(
                f32::from(q.y) < row_bottom + DN_TOP_CH * ch + 1.0,
                "the flat body reaches no deeper than {DN_TOP_CH} ch (y {} vs row bottom {row_bottom})",
                q.y
            );
            let lit = over_premul(DEFAULT_BG, q.color, q.alpha);
            assert!(
                contrast(DEFAULT_FG, lit) >= BODY_CONTRAST_BAR,
                "the flat body is the bed everywhere: #{lit:06X} at y {}",
                q.y
            );
        }
    }

    /// A stacked split's focused pane has its own floor: the comet's lower lobe and the vivid
    /// rail reach into the leading below the row, so they ask the pane's last row
    /// (`Ribbon::pane_rows`), not [`Geom::fx_bot`], and do not pour over the divider. The
    /// control, with no pane set, still draws its rail.
    #[test]
    fn the_band_stops_at_the_focused_pane_s_floor_not_the_grid_s() {
        let c = cfg(true, true);
        let g = geom();
        let ch = g.ch as f32;
        // The pane owns rows 0..=3, so row 3 is its last row — nowhere near
        // the 40-row grid's.
        let pane_last_row = 3u16;
        let deepest = |pane_rows: Option<(u16, u16)>| -> f32 {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            rib.set_pane_rows(pane_rows);
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g,
                    row: pane_last_row,
                    col0: 10,
                    n: 8,
                    period_ms: 60,
                    disp: 0.9,
                },
                &c,
            );
            let cx = ctx(at(t0, 8 * 60), &c, (pane_last_row, 18), 0.9);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            sink.under
                .iter()
                .map(|q| f32::from(q.y) + f32::from(q.h))
                .fold(0.0f32, f32::max)
        };
        let pane_floor = f32::from(g.origin_y) + f32::from(pane_last_row.saturating_add(1)) * ch;
        let bounded = deepest(Some((0, pane_last_row + 1)));
        let unbounded = deepest(None);
        assert!(
            unbounded > pane_floor + 0.5,
            "fixture: with no pane the band reaches past {pane_floor} \
             (deepest {unbounded}) — otherwise this pins nothing"
        );
        assert!(
            bounded <= pane_floor + 0.5,
            "the band reached {bounded}, past the focused pane's floor \
             {pane_floor} — that is the divider and the pane below"
        );
    }

    // -- §18: the budget ---------------------------------------------------

    #[test]
    fn a_hot_paragraph_at_retina_keeps_its_head_row_whole_and_sheds_only_the_oldest_tail() {
        let c = cfg(true, true);
        let g = geom2x();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut sink = Sink::default();
        let mut ms = 0u64;
        // 5 ms a key keeps all three rows live for the budget: a row the hand left keeps
        // only its own clock, so at 20 ms a key the first row would be in its swoosh (a
        // drain, not a shed); at 5 ms it is 0.76 s idle when the third finishes.
        for row in 2..5u16 {
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 76,
                period_ms: 5,
                disp: 1.0,
            };
            type_keys(&mut rib, at(t0, ms), keys, &c);
            ms += 76 * 5;
            let cx = ctx_in(at(t0, ms), &c, (row, 78), 1.0, g);
            rib.plan(&cx);
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let live_rows = row - 1;
            assert!(
                sink.under.len() <= RIBBON_QUAD_BUDGET,
                "{live_rows} retina rows spent {} quads against a {RIBBON_QUAD_BUDGET} budget",
                sink.under.len()
            );
            // The row under the hand is whole on every frame — one key, one
            // cell, one light (§7.3) — whatever else is live.
            for col in 2..78u16 {
                assert!(
                    cell_lit(&sink.under, g, row, col),
                    "row {row} col {col} is dark on its own frame with {live_rows} rows live"
                );
            }
            match live_rows {
                1 => assert!(
                    rib.slabs_per_cell() < SLABS_PER_CELL,
                    "a 76-cell retina line does not fit at three slabs per cell; the density must give"
                ),
                2 => {
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 2, col),
                            "two retina rows fit the budget; row 2 col {col} was shed anyway"
                        );
                    }
                }
                _ => {
                    // Three do not fit: the newer of the two older rows is whole, and what is
                    // shed comes off the oldest row's tail, never out of its middle.
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 3, col),
                            "row 3 col {col} was shed before row 2"
                        );
                    }
                    let lit: Vec<bool> = (2..78u16)
                        .map(|col| cell_lit(&sink.under, g, 2, col))
                        .collect();
                    // The density dial answers first (§18 `slabs_for`, which prices the
                    // rail's rows too), then shedding the oldest tail.
                    assert!(
                        lit.iter().any(|&l| !l) || rib.slabs_per_cell() == 1,
                        "three retina rows exceed the budget: the density must give or the oldest must lose its tail"
                    );
                    let first = lit.iter().position(|&l| l).unwrap_or(lit.len());
                    assert!(
                        lit[first..].iter().all(|&l| l),
                        "the shed must come off the oldest row's tail, not out of its middle"
                    );
                }
            }
        }
    }

    // -- §18 / §20.1: determinism -----------------------------------------

    /// A 57-row screen (`ESC[{vt};57r`), the fixture the band tests are
    /// stated on: the viewport tops and the pinned composer row 54 need rows
    /// the 40-row fixture does not have.
    fn geom_codex() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 57,
            cols: 151,
            origin_x: 0,
            origin_y: 0,
            win_w: 1359,
            win_h: 1026,
            head: 0,
        }
    }

    /// Type `n` cells on `row` from `col0`, one key per 60 ms, on the Codex
    /// geometry.
    fn type_row(rib: &mut Ribbon, t0: Instant, row: u16, col0: u16, n: u16, c: &Config) {
        type_keys(
            rib,
            t0,
            Keys {
                g: geom_codex(),
                row,
                col0,
                n,
                period_ms: 60,
                disp: 0.5,
            },
            c,
        );
    }

    /// A row band carries its ribbon with its text (seam point 12), phase A: the inline viewport
    /// `[vt..56]` slides down a row per streamed line under a still transcript. The composer's
    /// cells land a row down with column, stop, birth and cohort untouched, the transcript's
    /// stay, and before the next plan the field index answers for the moved cells (the engine
    /// seeds `caret_t` from it) and not for the vacated row.
    #[test]
    fn a_band_move_carries_the_composer_band_down_and_leaves_the_transcript_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 5, 10, 3, &c);
        type_row(&mut rib, at(t0, 500), 20, 10, 6, &c);
        let before: Vec<Cell> = rib.cells().to_vec();
        assert_eq!(before.iter().filter(|l| l.row == 20).count(), 6);
        assert_eq!(before.iter().filter(|l| l.row == 5).count(), 3);
        let composer: Vec<Option<f32>> = (10..16).map(|col| rib.field_at(20, col)).collect();
        assert!(
            composer.iter().all(Option::is_some),
            "fixture: the composer is lit"
        );
        let transcript: Vec<Option<f32>> = (10..13).map(|col| rib.field_at(5, col)).collect();
        assert!(
            transcript.iter().all(Option::is_some),
            "fixture: the transcript is lit"
        );
        assert_eq!(rib.caret(), Some((20, 16)));

        rib.translate_band(19, 56, 1);

        let after = rib.cells();
        assert_eq!(
            after.len(),
            before.len(),
            "a band move loses no cell inside its band"
        );
        for b in &before {
            let want = if b.row == 20 { 21 } else { b.row };
            assert!(
                after.iter().any(|a| a.row == want
                    && a.col == b.col
                    && a.t == b.t
                    && a.born == b.born
                    && a.cohort == b.cohort
                    && a.retract_at == b.retract_at),
                "cell ({}, {}) did not arrive on row {want} with its clocks and prices",
                b.row,
                b.col
            );
        }
        assert!(
            after.iter().all(|a| a.row != 20),
            "the vacated row keeps no light"
        );
        for (i, col) in (10..16).enumerate() {
            assert_eq!(
                rib.field_at(21, col),
                composer[i],
                "the field moved with its cell"
            );
            assert_eq!(
                rib.field_at(20, col),
                None,
                "the vacated cell answers nothing"
            );
        }
        for (i, col) in (10..13).enumerate() {
            assert_eq!(
                rib.field_at(5, col),
                transcript[i],
                "the transcript stood still"
            );
        }
        assert_eq!(rib.caret(), Some((21, 16)), "the caret rides its band");
    }

    /// Phase B: the viewport is pinned at the bottom and each streamed line archives the
    /// transcript `[0..51]` up a row under a fixed composer: row 3's light rides to row 2, the
    /// composer's row 54 and caret stay, and row 0's light is dropped.
    #[test]
    fn a_top_anchored_band_moves_the_transcript_up_and_pins_the_footer() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 0, 4, 2, &c);
        type_row(&mut rib, at(t0, 300), 3, 10, 4, &c);
        type_row(&mut rib, at(t0, 900), 54, 13, 5, &c);
        let footer: Vec<Option<f32>> = (13..18).map(|col| rib.field_at(54, col)).collect();
        let t3 = rib.field_at(3, 11).expect("fixture: row 3 is lit");
        assert_eq!(rib.caret(), Some((54, 18)));

        rib.translate_band(0, 51, -1);

        assert!(rib.cells().iter().all(|l| l.row != 0 && l.row != 3));
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 2).count(),
            4,
            "row 3 rode up to row 2"
        );
        assert_eq!(rib.field_at(2, 11), Some(t3));
        assert!(
            rib.cells().iter().all(|l| l.row != u16::MAX),
            "nothing wrapped around the top"
        );
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 54).count(),
            5,
            "the pinned composer is outside the band and untouched"
        );
        for (i, col) in (13..18).enumerate() {
            assert_eq!(rib.field_at(54, col), footer[i]);
        }
        assert_eq!(
            rib.caret(),
            Some((54, 18)),
            "a caret outside the band stays put"
        );
        assert_eq!(
            rib.cells().len(),
            9,
            "row 0's two cells left through the top; nothing else was lost"
        );
    }

    /// What leaves the band is gone: a ribbon on bottom row 56 pushed down by the slide retires
    /// its cells and cohort, the field index answers `None` there, and the ribbon is at rest;
    /// the caret saturates at the band's edge.
    #[test]
    fn a_band_move_drops_what_leaves_the_band_and_retires_its_cohort() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 56, 10, 6, &c);
        assert!(rib.live_cells() == 6 && !rib.cohorts.is_empty());
        assert_eq!(rib.index.live_rows(), 1);

        rib.translate_band(50, 56, 1);

        assert!(
            rib.at_rest(),
            "light carried off the band's edge is not kept"
        );
        assert!(
            rib.cohorts.is_empty(),
            "a cohort with no cells retires with them"
        );
        for col in 10..16 {
            assert_eq!(rib.index.at(56, col), None);
            assert_eq!(rib.index.at(57, col), None);
        }
        assert_eq!(
            rib.index.live_rows(),
            0,
            "the lane retired into the spare pool"
        );
        assert_eq!(
            rib.index.spare.len(),
            1,
            "…and is there for the next row that lights"
        );
        assert_eq!(
            rib.caret(),
            Some((56, 16)),
            "the caret saturates at the edge"
        );
    }

    /// A scroll carries the ribbon with its text (seam point 12): before the next plan the
    /// ribbon answers for the moved text, and the next frame is the pre-scroll frame one cell
    /// height up, clocks and prices untouched; a scroll past the text drops it. The 2× fixture
    /// because `ribbon_beam` phases its Bayer dither on absolute device y, so a translated frame
    /// is byte-identical only at a cell height that is a multiple of four.
    #[test]
    fn a_scroll_carries_the_ribbon_with_its_text() {
        let c = cfg(true, true);
        let g = geom2x();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Row 2, cols 10..14, the caret at (2, 14); then a Backspace puts
        // col 13 on its retract clock and the caret on it.
        type_run(&mut rib, t0, 10, 4, &c, 0.5);
        let now = at(t0, 4 * 60);
        erase_at(&mut rib, now, (2, 13), &c);
        let t13 = rib
            .field_at(2, 13)
            .expect("the retracting cell keeps its stop");
        let t12 = rib.field_at(2, 12).expect("col 12 is lit");
        assert_ne!(
            t13, t12,
            "the fixture needs a caret stop the newest key does not share"
        );
        assert_eq!(rib.caret(), Some((2, 13)));
        assert_eq!(rib.field_at_caret(), t13);
        assert!(
            rib.cells().iter().any(|l| l.retract_at.is_some()),
            "the fixture needs a cell on its retract clock"
        );
        let cx = ctx_in(now, &c, (2, 13), 0.7, g);
        rib.plan(&cx);
        let mut before = Sink::default();
        {
            let mut f = before.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !before.under.is_empty(),
            "the frame must draw for the law to bite"
        );
        let cells_before: Vec<Cell> = rib.cells().to_vec();

        rib.translate_scroll(1);

        // Between the scroll and the next plan: the ribbon answers for the
        // moved text already.
        assert_eq!(rib.caret(), Some((1, 13)), "the caret rides the scroll");
        assert_eq!(
            rib.field_at(1, 13),
            Some(t13),
            "the field moved with its cell"
        );
        assert_eq!(rib.field_at(2, 13), None, "nothing is left on the old row");
        assert_eq!(
            rib.field_at_caret(),
            t13,
            "the caret's stop is its own cell's, not the newest key's"
        );
        // Only the row moved: every clock and every price is untouched.
        let cells_after = rib.cells();
        assert_eq!(
            cells_after.len(),
            cells_before.len(),
            "a scroll drops nothing on the grid"
        );
        for (a, b) in cells_before.iter().zip(cells_after) {
            assert_eq!(b.row + 1, a.row);
            assert_eq!(
                (b.col, b.cohort, b.born, b.retract_at, b.typing),
                (a.col, a.cohort, a.born, a.retract_at, a.typing)
            );
            assert_eq!(
                (b.t, b.life_s, b.cov0, b.birth_disp),
                (a.t, a.life_s, a.cov0, a.birth_disp)
            );
        }
        // The next frame, planned at the same instant with the host's
        // translated caret, is the pre-scroll frame one cell height higher.
        let cx1 = ctx_in(now, &c, (1, 13), 0.7, g);
        rib.plan(&cx1);
        let mut after = Sink::default();
        {
            let mut f = after.frame();
            rib.emit(&cx1, &mut f);
        }
        let ch = u16::try_from(g.ch).expect("ch");
        assert_eq!(
            after.under.len(),
            before.under.len(),
            "the body kept every quad"
        );
        for (a, b) in before.under.iter().zip(&after.under) {
            assert_eq!(
                (b.x, b.w, b.h, b.color, b.alpha),
                (a.x, a.w, a.h, a.color, a.alpha)
            );
            assert_eq!(
                b.y + ch,
                a.y,
                "a body quad moved by {} px, not one cell height ({ch})",
                i32::from(a.y) - i32::from(b.y)
            );
            assert_eq!(b.row + 1, a.row, "the damage hint rides with the quad");
        }
        // A scroll past the text drops it: light on a line nobody typed.
        rib.translate_scroll(2);
        assert!(rib.at_rest());
        assert_eq!(rib.field_at(0, 13), None);
    }

    // -- §18 cadence: a fade's next level is solved, not polled -------------

    /// A fade's next u8 level is solved in closed form (§18's cadence law): for `spend` and the
    /// expiry melt, at every phase, [`level_step_spend`] / [`level_step_melt`] name the first
    /// instant `round(peak · curve)` drops (within 0.2 ms of a 10 µs poll), and `None` exactly
    /// at zero.
    #[test]
    fn a_fades_next_level_is_solved_not_polled() {
        let poll = |level_now: f32, curve: &dyn Fn(f32) -> f32, u: f32, span: f32| -> f32 {
            let mut step = 0u64;
            loop {
                step += 1;
                let t = (step as f64 * 1e-5) as f32;
                if (curve(u + t / span)).round() < level_now {
                    return t;
                }
                assert!(t < 5.0, "the polled level never dropped");
            }
        };
        let mut checked = 0usize;
        for peak in [UNDER_COV_CAP * 0.6, 30.0, 4.0] {
            for u in [0.0, 0.3, 0.7, 0.9, 0.97] {
                let span = RETRACT_FADE_S;
                let level = (peak * spend(u)).round();
                match level_step_spend(peak, u, span) {
                    None => assert!(
                        level < 1.0,
                        "spend peak {peak} u {u}: None while at {level}"
                    ),
                    Some(dt) => {
                        let got = poll(level, &|v| peak * spend(v), u, span);
                        assert!(
                            (dt - got).abs() < 2e-4,
                            "spend peak {peak} u {u}: solved {dt} s, polled {got} s"
                        );
                        checked += 1;
                    }
                }
                let life = 1.7;
                let level = (peak * expiry_melt(u)).round();
                match level_step_melt(peak, u, life) {
                    None => assert!(level < 1.0, "melt peak {peak} u {u}: None while at {level}"),
                    Some(dt) => {
                        let got = poll(level, &|v| peak * expiry_melt(v), u, life);
                        assert!(
                            (dt - got).abs() < 2e-4,
                            "melt peak {peak} u {u}: solved {dt} s, polled {got} s"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked >= 20, "only {checked} steps checked");
    }

    // -- the cohort's clock --------------------------------------------------

    /// A line still being typed keeps every cell from its first key to the caret: words at 9, 4
    /// and 8 cps on hot and dipped spines with a 700 ms pause keep every cell through the grace
    /// (a slow cell's short life runs on the cohort's clock), and the mark is off the glass
    /// [`SWOOSH_TOTAL_S`] after the last key.
    #[test]
    fn a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let row = 4u16;
        let col0 = 2u16;
        // `(word, ms per key, pause before, birth_disp)`.
        let words: [(&str, u64, u64, f32); 4] = [
            ("rainbow ", 110, 0, 0.9),
            ("theme", 250, 700, 0.45),
            (" truly ", 125, 0, 0.9),
            ("magical", 125, 0, 0.9),
        ];
        let mut key_ms = 0u64;
        let mut caret = col0;
        let mut now_ms = 0u64;
        let mut dark: Vec<String> = Vec::new();
        let mut keys = 0;
        for (word, per_key, pause, disp) in words {
            key_ms += pause;
            for _ in word.chars() {
                key_ms += per_key;
                while now_ms + 16 <= key_ms {
                    now_ms += 16;
                    let cx = ctx_in(at(t0, now_ms), &c, (row, caret), disp, g);
                    rib.plan(&cx);
                    let missing: Vec<u16> = (col0..caret)
                        .filter(|&col| rib.field_at(row, col).is_none())
                        .collect();
                    if !missing.is_empty() && dark.len() < 6 {
                        dark.push(format!(
                            "+{:.3}s keys={keys} caret={caret} dark={missing:?}",
                            now_ms as f32 / 1000.0
                        ));
                    }
                }
                caret += 1;
                keys += 1;
                let cx = ctx_in(at(t0, key_ms), &c, (row, caret), disp, g);
                rib.on_event(&typed(), at(t0, key_ms), &cx);
                rib.plan(&cx);
            }
        }
        assert_eq!(keys, 27);
        assert!(
            dark.is_empty(),
            "a cell went dark under a hand still typing:\n{}",
            dark.join("\n")
        );
        // The swoosh is still the ending: through the PHRASE_REST_MIN_S grace every cell
        // stands, and SWOOSH_TOTAL_S after the last key the mark is gone.
        let last = key_ms;
        let grace_ms = (PHRASE_REST_MIN_S * 1000.0) as u64;
        let cx = ctx_in(at(t0, last + grace_ms), &c, (row, caret), 0.9, g);
        rib.plan(&cx);
        assert_eq!(
            (col0..caret)
                .filter(|&col| rib.field_at(row, col).is_some())
                .count(),
            usize::from(caret - col0),
            "every cell stands through the {PHRASE_REST_MIN_S} s grace"
        );
        let gone = last + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20;
        let cx = ctx_in(at(t0, gone), &c, (row, caret), 0.9, g);
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(
            rib.at_rest() && sink.under.is_empty() && sink.out.is_empty(),
            "off the glass {SWOOSH_TOTAL_S} s after the last key"
        );
    }

    // -- edits: landings ----------------------------------------------------

    /// Lit (non-leaving) columns of `row`, for the two landing pins below.
    fn landing_lit(rib: &Ribbon, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row && !l.leaving())
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Every cell of `row`, leaving or not — a stray is a stray while it
    /// fades.
    fn landing_any(rib: &Ribbon, row: u16) -> Vec<u16> {
        let mut v: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == row)
            .map(|l| l.col)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// A band on its way out is still the hand's row: after sixteen Backspaces (every cell
    /// retracting) a parking pass at `(12, 0)` does not lay the key at `(11, 119)`, since
    /// [`Ribbon::typed_landing`] reads every cell, leaving or not. Also pins that an erase moves
    /// [`Ribbon::pen`] to its caret, and that a cold session's pen is `None`.
    #[test]
    fn a_parking_pass_after_the_row_was_backspaced_empty_still_finds_the_hand() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        let mut ms = 16 * 60;
        for i in 0..16u16 {
            ms += 40;
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (13, 17 - i), 0.7, g);
            rib.on_event(&Event::Erase, now, &cx);
            rib.plan(&cx);
        }
        assert_eq!(rib.caret(), Some((13, 2)));
        assert!(
            landing_lit(&rib, 13).is_empty() && !landing_any(&rib, 13).is_empty(),
            "fixture: the held Backspace left row 13 entirely in retract, \
             live {:?} / all {:?}",
            landing_lit(&rib, 13),
            landing_any(&rib, 13)
        );
        ms += 40;
        let now = at(t0, ms);
        let cx = ctx_in(now, &c, (12, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 13),
            vec![2],
            "the key after the Backspace run did not land on the hand's own row"
        );
        assert!(
            landing_any(&rib, 11).is_empty(),
            "a stray was laid on the row above the box home: {:?}",
            landing_any(&rib, 11)
        );
    }

    /// A parking pass is caught at every column it can be sampled at ([`Ribbon::typed_landing`]
    /// has no column clause): with the hand at `(13, 18)`, a park sampled anywhere on row 12
    /// lays no stray there, since a key's caret is never up from the pen except through the
    /// fold.
    #[test]
    fn a_parking_pass_is_caught_at_every_column_it_can_be_sampled_at() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 18)));
        let lit = |rib: &Ribbon, row: u16| {
            let mut v: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|l| l.row == row && !l.leaving())
                .map(|l| l.col)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(lit(&rib, 13), (2..=17).collect::<Vec<_>>());
        // The repaint homes to the box top-left and starts rewriting; the host
        // samples it at the home, and at several columns into the rewrite.
        // The key is the hand's either way.
        for park in [0u16, 1, 5, 9, 17] {
            let mut rib = rib.clone();
            let now = at(t0, 16 * 60);
            let cx = ctx_in(now, &c, (12, park), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            assert_eq!(
                lit(&rib, 13),
                (2..=18).collect::<Vec<_>>(),
                "a parking pass sampled at column {park} took the key off the hand's row"
            );
            assert!(
                lit(&rib, 12).is_empty() && lit(&rib, 11).is_empty(),
                "a stray was laid off the hand's row for a pass sampled at column {park}: \
                 row 12 {:?}, row 11 {:?}",
                lit(&rib, 12),
                lit(&rib, 11)
            );
        }
    }

    /// A repaint's `Typed` home moves the caret mirror to the park without leaving the row, so
    /// the key's echo at the parked caret is landed at the pen ([`Ribbon::pen`]), never the
    /// mirror: an echoed move never moves the pen.
    #[test]
    fn a_repaints_home_pass_poisons_the_mirror_and_the_pen_still_holds_the_hand() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 18)));
        // (1) the repaint's home, licensed `Typed` — the sibling law's own
        // event, which must not abandon the row…
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (12, 0), 0.7, g);
        rib.on_event(&typed_move((13, 18), (12, 0)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.caret(),
            Some((12, 0)),
            "the fixture's whole point: the MIRROR took the park"
        );
        assert!(
            rib.cohorts().iter().all(|k| !k.abandoned),
            "fixture: and the sibling law kept the hand's row"
        );
        // (2) …and now the key's own echo, on a tick still showing it.
        let now = at(t0, 17 * 60);
        let cx = ctx_in(now, &c, (12, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 13),
            (2..=18).collect::<Vec<_>>(),
            "the key did not land on the hand's own row"
        );
        assert!(
            landing_any(&rib, 12).is_empty() && landing_any(&rib, 11).is_empty(),
            "a stray was laid off the hand's row: 11={:?} 12={:?}",
            landing_any(&rib, 11),
            landing_any(&rib, 12)
        );
    }

    /// A nav is the hand, so it moves the pen; an echo never does. Else a fold from where the
    /// hand navigated would be re-homed onto the row it left: sixteen keys on row 13, a nav to
    /// row 20's last column, a key that wraps onto row 21; its glyph belongs on row 20.
    #[test]
    fn a_nav_moves_the_pen_so_a_fold_after_it_still_lays_where_it_folded() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        // The hand hops to the end of row 20 — a nav, not an echo.
        let last = u16::try_from(g.cols).expect("cols") - 1;
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (20, last), 0.7, g);
        rib.on_event(
            &Event::Move {
                from: (13, 18),
                to: (20, last),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
            now,
            &cx,
        );
        rib.plan(&cx);
        // …and types one key there, which wraps: the echo's caret is the next
        // row's first column, the same shape a downward park has.
        let now = at(t0, 17 * 60);
        let cx = ctx_in(now, &c, (21, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        // The wrap key's glyph is the last column of row 20 (`Ribbon::lay`'s fold
        // arithmetic): the landing is no park to walk back to the row the nav left.
        assert_eq!(
            landing_any(&rib, 20),
            vec![last],
            "the hand's own fold after a nav did not lay on the row it folded off"
        );
        assert!(
            !landing_any(&rib, 13).contains(&18),
            "the wrap key was dragged back onto the row the nav left: {:?}",
            landing_any(&rib, 13)
        );
    }

    /// The fold is the fact, not the distance ([`Ribbon::typed_landing`]'s downward clause):
    /// (a) a park at `(15, 0)` under a pen at `(13, 18)`, whose fold would write row 14, is
    /// caught; (b) the same distance three columns over folds nothing and is taken as it comes,
    /// the clause's price.
    #[test]
    fn a_downward_landing_is_questioned_only_where_lay_would_write_above_it() {
        let c = cfg(true, true);
        let g = geom();
        let seat = |t0: Instant| {
            let mut rib = Ribbon::new();
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g,
                    row: 13,
                    col0: 2,
                    n: 16,
                    period_ms: 60,
                    disp: 0.7,
                },
                &c,
            );
            assert_eq!(rib.caret(), Some((13, 18)));
            rib
        };
        // (a) The park the fold would betray: caught, and the band's own
        // next column is lit instead of a stray on row 14.
        let t0 = Instant::now();
        let mut rib = seat(t0);
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (15, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(landing_lit(&rib, 13), (2..=18).collect::<Vec<_>>());
        assert!(landing_any(&rib, 14).is_empty() && landing_any(&rib, 15).is_empty());
        // (b) The same distance and direction, three columns over: nothing folds, so the
        // key lays where the tick says (the clause's price).
        let t0 = Instant::now();
        let mut rib = seat(t0);
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (15, 3), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 15),
            vec![2],
            "a downward caret that does not fold is the hand's own, however far"
        );
        assert_eq!(landing_lit(&rib, 13), (2..=17).collect::<Vec<_>>());
    }

    /// The one fold that is real reaches the hand ([`Ribbon::fold_top_row`]): five glyphs
    /// committed with the hand at `(13, 117)` of a 120-column grid echo at `(14, 2)`; the fold
    /// reaches the pen's own row, so three of their cells stay at `117..120` on row 13.
    #[test]
    fn a_delivered_burst_that_wraps_keeps_the_cells_it_left_on_the_row_above() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 112,
                n: 5,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(
            rib.caret(),
            Some((13, 117)),
            "fixture: the hand is five from the margin"
        );
        let now = at(t0, 5 * 60);
        let cx = ctx_in(now, &c, (14, 2), 0.7, g);
        rib.on_event(
            &Event::Typed {
                cells: 5,
                shifted: false,
                class: TypedClass::Glyph,
            },
            now,
            &cx,
        );
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 13),
            (112..=119).collect::<Vec<_>>(),
            "the burst's first three cells finished the hand's own row"
        );
        assert_eq!(
            landing_lit(&rib, 14),
            vec![0, 1],
            "…and the other two went on over the wrap"
        );
    }

    /// A mirror is not a hand ([`Ribbon::typed_landing`]'s guard): [`Ribbon::caret`] is seeded
    /// by [`Ribbon::plan`] from the engine's caret mirror, which reads `(0, 0)` until a caret is
    /// observed (`Engine::caret_known`, which the ribbon is not given); read as a hand it would
    /// send the session's first key to row 0.
    #[test]
    fn a_cold_caret_mirror_is_not_a_hand_a_key_can_be_brought_back_to() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // The engine's first plan of a session runs on its caret mirror,
        // which is `(0, 0)` and names nothing.
        let cold = ctx_in(t0, &c, (0, 0), 0.7, g);
        rib.plan(&cold);
        assert_eq!(rib.caret(), Some((0, 0)));
        assert!(
            rib.cells().is_empty(),
            "a cold ribbon holds no cell — there is no hand's row yet"
        );
        // The first real key of the session, on row 2.
        let now = at(t0, 80);
        let cx = ctx_in(now, &c, (2, 5), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        let lit = |rib: &Ribbon, row: u16| {
            let mut v: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|l| l.row == row)
                .map(|l| l.col)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(
            lit(&rib, 2),
            vec![4],
            "the first key of the session did not land on the row the host reported"
        );
        assert!(
            lit(&rib, 0).is_empty(),
            "the cold mirror was read as a hand and took the key to row 0: {:?}",
            lit(&rib, 0)
        );
    }

    /// A key sampled at readline's CR park stays on the hand's own row instead of folding onto
    /// the previous row ([`Ribbon::typed_landing`]): (a) the full grid, (b) one pane in, and
    /// (c) a same-row caret that folds nothing, taken as it comes.
    #[test]
    fn a_key_sampled_at_readlines_cr_park_stays_on_the_hands_own_row() {
        let c = cfg(true, true);
        let g = geom();

        // (a) bash at the full grid, 120 columns. The hand has typed
        // sixteen cells at 2..=17 and sits at (13, 18).
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 18)), "fixture: the hand's caret");
        // readline's CR park: column 0 of the hand's own row, licensed
        // `Typed` by the key in flight.
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (13, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert!(
            landing_any(&rib, 12).is_empty(),
            "the key folded off the pane and laid on the row above — the \
             previous command's output: {:?}",
            landing_any(&rib, 12)
        );
        assert_eq!(
            landing_lit(&rib, 13),
            (2..=18).collect::<Vec<_>>(),
            "the key did not land on the hand's own row, next to its last cell"
        );

        // (b) The same park one pane in: a split pane at columns 40..79. The
        // hand has typed 42..=57 and sits at (13, 58); the park is the pane's
        // own first column.
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        rib.set_pane(Some((40, 40)));
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 42,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 58)), "fixture: the hand's caret");
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (13, 40), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert!(
            landing_any(&rib, 12).is_empty(),
            "the key folded off the PANE's first column onto the row above: {:?}",
            landing_any(&rib, 12)
        );
        assert_eq!(
            landing_lit(&rib, 13),
            (42..=58).collect::<Vec<_>>(),
            "the key did not land on the hand's own row inside the pane"
        );

        // (c) …and a same-row caret that folds nothing is taken as it comes,
        // however far left of the hand (readline reprinting mid-line, a ^A, a
        // shell's own cursor walk). The hand is at (13, 36) with cells
        // 20..=35; a key sampled at (13, 5) lays at column 4 and is not
        // dragged back to the band.
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 20,
                n: 16,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        assert_eq!(rib.caret(), Some((13, 36)), "fixture: the hand's caret");
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (13, 5), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert!(
            landing_lit(&rib, 13).contains(&4),
            "a same-row caret that folds nothing was re-homed to the pen: {:?}",
            landing_lit(&rib, 13)
        );
        assert!(
            landing_any(&rib, 12).is_empty(),
            "a caret that folds nothing reached the row above: {:?}",
            landing_any(&rib, 12)
        );
    }

    /// A repaint's home is not a gesture: a bottom-pinned composer repaints by homing to the
    /// box's top-left, licensed `Typed` by a fresh type hint. The hand cannot type its way up
    /// and to the left, so the ribbon does not read it as leaving the row.
    #[test]
    fn a_repaints_home_pass_does_not_abandon_the_row_the_hand_is_typing_on() {
        let c = cfg(true, true);
        let g = geom();
        for (excursion, what) in [
            ((12u16, 0u16), "the row above, column 0"),
            ((11, 0), "two rows above, column 0"),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g,
                    row: 13,
                    col0: 2,
                    n: 20,
                    period_ms: 60,
                    disp: 0.7,
                },
                &c,
            );
            let ms = 19 * 60 + 60;
            // The repaint's home, licensed `Typed` by the key in flight.
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, excursion, 0.7, g);
            rib.on_event(&typed_move((13, 22), excursion), now, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().all(|k| !k.abandoned),
                "{what}: the repaint's home abandoned the band"
            );
            assert_eq!(
                rib.cells()
                    .iter()
                    .filter(|l| l.row == 13 && !l.leaving())
                    .count(),
                20,
                "{what}: the band lost cells to a caret the hand never put there"
            );
            // …and the hand's real backspace back through a fold still is a
            // row change: up one row, to the row's right end.
            let now = at(t0, ms + 60);
            let cx = ctx_in(now, &c, (12, 79), 0.7, g);
            rib.on_event(&typed_move((13, 2), (12, 79)), now, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().any(|k| k.row == 13 && k.abandoned),
                "{what}: a backspace back through the fold stopped being a row change"
            );
        }
    }

    /// A re-anchor inside the scroll that moved the band drains nothing: `translate_scroll` has
    /// already carried the row up with its glyphs, so the same repaint's backward caret is not
    /// a stale line (the one-tick `scroll_followed` guard).
    #[test]
    fn a_re_anchor_inside_the_scroll_that_moved_the_band_drains_nothing() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: 13,
                col0: 2,
                n: 20,
                period_ms: 60,
                disp: 0.7,
            },
            &c,
        );
        // The box grows a row up: the screen scrolls and the band follows.
        rib.translate_scroll(1);
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 12).count(),
            20,
            "the band followed its text up"
        );
        // The same repaint's home pass, on the same tick.
        let now = at(t0, 19 * 60 + 60);
        let cx = ctx_in(now, &c, (13, 3), 0.7, g);
        rib.on_event(&typed_move((12, 22), (12, 2)), now, &cx);
        assert!(
            rib.cells().iter().all(|l| l.retract_at.is_none()),
            "the re-anchor drained the row the scroll had just carried: {:?}",
            rib.cells()
                .iter()
                .filter(|l| l.retract_at.is_some())
                .map(|l| l.col)
                .collect::<Vec<_>>()
        );
        rib.plan(&cx);
        // …and the guard is one tick wide: a backward typed move on the next
        // tick is the hand's own re-anchor again.
        let now = at(t0, 19 * 60 + 120);
        let cx = ctx_in(now, &c, (12, 2), 0.7, g);
        rib.on_event(&typed_move((12, 22), (12, 2)), now, &cx);
        assert!(
            rib.cells().iter().any(|l| l.retract_at.is_some()),
            "the scroll guard outlived its own tick"
        );
    }

    /// A backward typed echo under [`RE_ANCHOR_MIN_CELLS`] (one or two cells, no erase) is plain
    /// typing, matching the host's `classify_move`: nothing is stamped and the census is
    /// unchanged; at the floor (the control) the re-anchor drains. The trigger is pinned in
    /// `cursor_glow/tests/park.rs`.
    #[test]
    fn a_backward_typed_echo_under_the_re_anchor_floor_stamps_nothing() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let census = |rib: &Ribbon| {
            let mut v: Vec<(u16, u16, Instant, bool)> = rib
                .cells()
                .iter()
                .map(|l| (l.row, l.col, l.born, l.typing))
                .collect();
            v.sort_unstable();
            v
        };
        let stamped = |rib: &Ribbon| {
            rib.cells()
                .iter()
                .filter(|l| l.leaving())
                .map(|l| l.col)
                .collect::<Vec<_>>()
        };
        for back in 1..RE_ANCHOR_MIN_CELLS {
            let mut rib = Ribbon::new();
            // Six keys at 10..16; the caret stands at (2, 16).
            type_run(&mut rib, t0, 10, 6, &c, 0.8);
            let before = census(&rib);
            let now = at(t0, 6 * 60 + 200);
            let landing = (2, 16 - back);
            let cx = ctx(now, &c, landing, 0.8);
            rib.on_event(&typed_move((2, 16), landing), now, &cx);
            rib.plan(&cx);
            assert!(
                stamped(&rib).is_empty(),
                "a {back}-cell backward typed echo drained the row: stamped={:?}",
                stamped(&rib)
            );
            assert_eq!(census(&rib), before, "{back} back: the census is unchanged");
            assert_eq!(rib.caret(), Some(landing), "{back} back: the mirror moved");
        }
        // The control, at the floor: the composer's re-anchor as before.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let now = at(t0, 6 * 60 + 200);
        let landing = (2, 16 - RE_ANCHOR_MIN_CELLS);
        let cx = ctx(now, &c, landing, 0.8);
        rib.on_event(&typed_move((2, 16), landing), now, &cx);
        assert_eq!(
            stamped(&rib),
            vec![13, 14, 15],
            "at the floor the re-anchor drains the cells at and past the landing"
        );
    }

    /// A `^A` is a home, not a relocation: a same-row re-anchor relays nothing left of the
    /// hand's own ink on the row, so nothing is laid on the prompt and no relocation is left
    /// pending ([`Ribbon::re_anchor`]).
    #[test]
    fn a_shell_home_relays_no_word_onto_the_prompt() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // `hello there now` typed at columns 28..42 behind a 28-column PS1.
        let mut now = t0;
        for (i, ch) in "hello there now".chars().enumerate() {
            now = at(t0, i as u64 * 60);
            let col0 = 28 + i as u16;
            let cx = ctx_in(now, &c, (5, col0 + 1), 0.9, g);
            let ev = if ch == ' ' { typed_space() } else { typed() };
            rib.on_event(&ev, now, &cx);
            rib.plan(&cx);
        }
        // ^A: readline reprints the line and parks the caret at the input's
        // first column. The move carries a fresh type hint, so it is licensed
        // `Typed`: `re_anchor`'s arm.
        let home = at(now, 60);
        let cx = ctx_in(home, &c, (5, 28), 0.9, g);
        rib.on_event(&typed_move((5, 43), (5, 28)), home, &cx);
        rib.plan(&cx);
        let inside_prompt: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 5 && l.col < 28)
            .map(|l| l.col)
            .collect();
        assert!(
            inside_prompt.is_empty(),
            "the home relaid a word onto the prompt: columns {inside_prompt:?} are lit left of \
             the 28-column PS1"
        );
        // …and the next key does not settle one there either: the refused
        // relay left nothing pending for a forward move to land.
        let k = at(home, 60);
        let cx = ctx_in(k, &c, (5, 29), 0.9, g);
        rib.on_event(&typed(), k, &cx);
        rib.on_event(&typed_move((5, 28), (5, 29)), k, &cx);
        rib.plan(&cx);
        let inside_prompt: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 5 && l.col < 28)
            .map(|l| l.col)
            .collect();
        assert!(
            inside_prompt.is_empty(),
            "a pending relocation settled onto the prompt: columns {inside_prompt:?}"
        );
    }

    // -- slits and gaps -----------------------------------------------------

    /// A typed echo licensed as one move — `from` → `to` on the same row.
    fn typed_move(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Typed,
            dir: Dir::of(
                i32::from(to.1) - i32::from(from.1),
                i32::from(to.0) - i32::from(from.0),
            ),
        }
    }

    fn typed_space() -> Event {
        Event::Typed {
            cells: 1,
            shifted: false,
            class: TypedClass::Space,
        }
    }

    /// The planned coverage of the slabs inside cell `col` on row 2 (the
    /// interior vertices, boundaries excluded).
    fn interior_covs(rib: &Ribbon, col: u16) -> Vec<u8> {
        let cw = geom().cw as f32;
        let (x0, x1) = (f32::from(col) * cw, f32::from(col + 1) * cw);
        rib.plan_segments()
            .iter()
            .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
            .map(|s| s.cov)
            .collect()
    }

    /// The planned coverage at the boundary `col` (the vertex on its x).
    fn boundary_cov(rib: &Ribbon, col: u16) -> Option<u8> {
        let x = f32::from(col) * geom().cw as f32;
        rib.plan_segments()
            .iter()
            .find(|s| (s.x - x).abs() < 0.5)
            .map(|s| s.cov)
    }

    /// Slits: two keys echoed on one tick replay as `Typed, Typed, Sweep, Move` against the
    /// landing caret, and the first `Typed` mints a second cohort past the live one's end;
    /// [`Ribbon::heal_seams`] makes it one cohort, one run, with no interior boundary feathered
    /// at [`RUN_TAIL_EASE`].
    #[test]
    fn a_coalesced_echo_leaves_one_cohort_and_no_feather_inside_the_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 6, &c, 0.9);
        // The batch: keys at +360/+370 ms, both echoed on the tick at +380.
        let k1 = at(t0, 360);
        let k2 = at(t0, 370);
        let now = at(t0, 380);
        let cx = ctx(now, &c, (2, 10), 0.9);
        rib.on_event(&typed(), k1, &cx);
        rib.on_event(&typed(), k2, &cx);
        rib.on_event(
            &Event::Sweep {
                row: 2,
                col0: 8,
                col1: 10,
            },
            k1,
            &cx,
        );
        rib.on_event(&typed_move((2, 8), (2, 10)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == 2).count(),
            1,
            "the batch's last cell joined the row's one cohort: {:?}",
            rib.cohorts()
        );
        for col in 2..10u16 {
            assert!(
                rib.cells().iter().any(|l| l.row == 2 && l.col == col),
                "col {col} is laid"
            );
        }
        // Settled, every interior boundary carries the body's own level:
        // nothing at a tenth, nothing at a quarter.
        let cx = ctx(at(t0, 500), &c, (2, 10), 0.9);
        rib.plan(&cx);
        let body = boundary_cov(&rib, 6).expect("a boundary inside the band");
        for col in 3..10u16 {
            let b = boundary_cov(&rib, col).expect("boundary {col}");
            assert!(
                u32::from(b) * 10 >= u32::from(body) * 9,
                "boundary {col} is feathered: {b} against the body's {body}"
            );
        }
    }

    /// The brighter side owns a boundary: a new cell's attack lives inside the new light, and an
    /// erased neighbour's spend never sags the surviving head cell, so nothing pops when it
    /// retires. The new light is the attach under the caret cell: the cell the key lays there
    /// inherits the attach's level, and the attack is the next attach's, one cell on.
    #[test]
    fn a_boundary_takes_the_brighter_cell_s_coverage() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 4, &c, 0.9);
        let cx = ctx(at(t0, 400), &c, (2, 6), 0.9);
        rib.plan(&cx);
        let settled = interior_covs(&rib, 5);
        let edge = boundary_cov(&rib, 6).expect("the head's right edge");
        // A new key: its cell attacks over 18 ms; frame 2 ms in.
        let k = at(t0, 420);
        let cx = ctx(at(t0, 422), &c, (2, 7), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        assert_eq!(
            interior_covs(&rib, 5),
            settled,
            "cell 5 dipped under cell 6's attack"
        );
        assert!(
            boundary_cov(&rib, 6).expect("the shared boundary") >= edge - 1,
            "the shared boundary sagged toward the attacking cell"
        );
        let laid: Vec<u8> = interior_covs(&rib, 6);
        assert!(
            laid.iter().all(|&v| v + 1 >= edge),
            "the cell laid where the attach stood keeps the attach's level: {laid:?} under {edge}"
        );
        let young: Vec<u8> = interior_covs(&rib, 7);
        assert!(
            !young.is_empty() && young.iter().all(|&v| v < edge),
            "the attack lives in the attach under the caret cell: {young:?} under {edge}"
        );
        // Settle, then erase the head: the survivor's slabs hold while the
        // erased neighbour spends, and hold when it retires.
        let cx = ctx(at(t0, 700), &c, (2, 7), 0.9);
        rib.plan(&cx);
        let hold = interior_covs(&rib, 5);
        let hold_edge = boundary_cov(&rib, 6).expect("edge");
        erase_at(&mut rib, at(t0, 720), (2, 6), &c);
        for ms in [730u64, 800, 900, 940, 970, 1000] {
            let cx = ctx(at(t0, ms), &c, (2, 6), 0.9);
            rib.plan(&cx);
            assert_eq!(
                interior_covs(&rib, 5),
                hold,
                "+{ms} ms: the survivor sagged"
            );
            assert!(
                boundary_cov(&rib, 6).expect("edge") + 1 >= hold_edge,
                "+{ms} ms: the survivor's edge sagged"
            );
        }
        assert!(
            !rib.cells().iter().any(|l| l.col == 6),
            "the erased cell retired inside {RETRACT_FADE_S} s"
        );
    }

    /// A Backspace whose retreat echoes a tick late: the `Erase` was replayed
    /// against the pre-retreat caret and found nothing to its right; the
    /// typed retreat that follows retracts from its landing.
    #[test]
    fn an_erase_whose_echo_lands_a_tick_late_still_retracts_the_erased_cell() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        // The erase, replayed on a tick that precedes its echo.
        let k = at(last, 300);
        let cx = ctx(k, &c, (2, 10), 0.9);
        rib.on_event(&Event::Erase, k, &cx);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|l| l.retract_at.is_none()),
            "nothing right of the pre-retreat caret to retract"
        );
        // The retreat lands on the next tick.
        let now = at(last, 316);
        let cx = ctx(now, &c, (2, 9), 0.9);
        rib.on_event(&typed_move((2, 10), (2, 9)), now, &cx);
        rib.plan(&cx);
        let erased = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 9)
            .expect("the erased glyph's cell is still spending");
        assert!(
            erased.retract_at.is_some(),
            "the erased cell stayed lit under the blank"
        );
        assert!(
            rib.cells()
                .iter()
                .filter(|l| l.col < 9)
                .all(|l| l.retract_at.is_none()),
            "only the erased cell retracts"
        );
        // A typed key clears the pending erase: a later re-anchor of the
        // hand's own is not read as an erase's retreat.
        let now = at(last, 400);
        let cx = ctx(now, &c, (2, 10), 0.9);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(rib.pending_erase, 0);
    }

    /// A fold is not a departure: a typed move onto the next row is the same input
    /// continuing, so the row it wrapped off keeps its own clock and is not abandoned; keys on
    /// the new row do not hold it (`OnlyOwnerRenews`), so it is gone by the flow's end.
    #[test]
    fn a_typed_row_change_lets_the_old_row_leave_on_its_own_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 100,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let last = at(t0, 19 * 60);
        let now = at(last, 60);
        let cx = ctx(now, &c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 120), (3, 0)), now, &cx);
        rib.plan(&cx);
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the old row");
        let last_key_on_the_old_row = old.alive_at;
        assert!(
            !old.abandoned,
            "a fold abandoned the row the wrap came off: the band is killed at every wrap"
        );
        assert_eq!(
            old.retract_col, None,
            "nothing froze the old row's retract target: it has not entered one"
        );
        assert!(
            last_key_on_the_old_row >= at(t0, 19 * 60),
            "the old row keeps the life its last key gave it"
        );
        // Keys on the new row: the old row is not held.
        let keys = Keys {
            g: geom(),
            row: 3,
            col0: 0,
            n: 12,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(now, 60), keys, &c);
        // The band spans the fold and flows: half a second on, with the hand typing below,
        // the row it wrapped off is still lit and drawn toward the fold point, its own right
        // end (`Cohort::flow`, `leave_row`), its far end gone first.
        let mut sink = Sink::default();
        let cx = ctx(at(now, 500), &c, (3, 12), 0.9);
        rib.plan(&cx);
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the old row at +0.5 s");
        assert!(old.flow.is_some(), "the fold set the old row flowing");
        assert_eq!(old.phase, Phase::Retracting, "…and it is moving at +0.5 s");
        assert_eq!(
            old.retract_col,
            Some(119),
            "toward the fold point: the row's own right end"
        );
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 2).count(),
            20,
            "the row the wrap came off went dark under the hand: the band does not span the fold"
        );
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let (cov, _, left) = row_light(&sink, geom(), 2).expect("row 2 is lit at +0.5 s");
        assert!(cov > 0.0);
        assert!(
            left > 100.0 * geom().cw as f32 + 0.5,
            "the far (left) end drained first: the left edge is at {left} px"
        );
        // And it is not persistent: gone by the flow's own end,
        // `FLOW_TOTAL_S` from the wrap, while the hand still types on row 3
        // (the flow's clock is the row's own, not the hand's).
        let gone_ms = (FLOW_TOTAL_S * 1000.0) as u64 + 60;
        let cx = ctx(at(now, gone_ms), &c, (3, 12), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "the old row is still lit {FLOW_TOTAL_S} s + 60 ms after the fold"
        );
        assert!(rib.cells().iter().any(|l| l.row == 3), "the new row is lit");
    }

    /// Thirty keys on row 2 from column 2 (a cold walk, cells 2..=31, its end
    /// at distance 30, past the knee), and the colour a continuation of it
    /// takes one step past its end: `t_at(32)`, exactly
    /// `t0 + walk_t(32 − 2)`.
    fn a_line_of_thirty(rib: &mut Ribbon, t0: Instant, c: &Config) -> f32 {
        type_run(rib, t0, 2, 30, c, 0.9);
        let band = rib.cohorts()[0];
        let t_next = band.t_at(32);
        assert_eq!(
            t_next.to_bits(),
            (band.t0 + walk_t(f32::from(32 - band.anchor_col))).to_bits(),
            "the premise: a cold walk, read as it was before the odometer"
        );
        t_next
    }

    /// A cross-row move `from → to` under `licence`, the shape the host
    /// hands a Return's, a composer newline's or an arrow's row change over.
    fn row_change(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
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

    /// Tier-1 of `RainbowComposerNewlineGate`: the real ribbon follows the
    /// model's chord → pre-home type-ahead → observed home → first echo
    /// sequence. A plain modified key with no composer hint cannot borrow the
    /// gate even if a TUI later parks its caret at the same inset.
    #[test]
    fn a_composer_gate_waits_for_its_home_and_only_its_first_real_echo_spends_it() {
        let model = aterm_models_effects::rainbow_composer_newline_gate_model();
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let chord = at(t0, 29 * 60 + 100);
        let old = ctx(chord, &c, (2, 32), 0.9);

        rib.on_event(&Event::ComposerNewline, chord, &old);
        let armed = model.successors("ComposerKey", &model.init_state())[0].clone();
        assert_eq!(armed.get("moved"), Some(&0));
        assert_eq!(
            rib.fresh_line,
            Some(FreshLine {
                since: chord,
                chord_at: Some(chord),
                awaiting_move: true,
                left_row: None,
            })
        );

        rib.on_event(&typed(), chord, &old);
        let stamped = model.successors("ChordStamp", &armed)[0].clone();
        assert_eq!(stamped.get("spent"), Some(&0));
        assert!(rib.fresh_line.is_some_and(|gate| gate.awaiting_move));

        let key = at(chord, 10);
        rib.on_event(&typed(), key, &ctx(key, &c, (2, 32), 0.9));
        let raced = model.successors("TypeAheadBeforeHome", &stamped)[0].clone();
        assert_eq!(raced.get("spent"), Some(&0));
        assert!(rib.fresh_line.is_some_and(|gate| gate.awaiting_move));

        let seen = at(chord, 30);
        rib.on_event(
            &row_change((2, 32), (2, 2), Licence::Return),
            seen,
            &ctx(seen, &c, (2, 2), 0.9),
        );
        let homed = model.successors("ObserveHome", &raced)[0].clone();
        assert_eq!(homed.get("moved"), Some(&1));
        assert_eq!(
            rib.fresh_line.map(|gate| (gate.since, gate.awaiting_move)),
            Some((chord, false))
        );

        let echo = at(seen, 20);
        rib.on_event(
            &Event::Sweep {
                row: 2,
                col0: 2,
                col1: 3,
            },
            key,
            &ctx(echo, &c, (2, 3), 0.9),
        );
        let echoed = model.successors("FirstEcho", &homed)[0].clone();
        assert_eq!(echoed.get("spent"), Some(&1));
        assert!(rib.fresh_line.is_none());
        let line = rib
            .cohorts()
            .iter()
            .find(|q| q.row == 2 && q.anchor_col == 2 && q.born > chord)
            .expect("the first echo made the new line's cohort");
        assert_eq!((line.t0.to_bits(), line.d0), (t_next.to_bits(), 0.0));

        let mut plain = Ribbon::new();
        a_line_of_thirty(&mut plain, t0, &c);
        let plain_state = model.successors("PlainModifiedKey", &model.init_state())[0].clone();
        plain.on_event(&typed(), chord, &old);
        plain.on_event(
            &row_change((2, 32), (2, 2), Licence::Typed),
            seen,
            &ctx(seen, &c, (2, 2), 0.9),
        );
        assert!(!model.action_enabled("ObserveHome", &plain_state));
        assert!(
            plain.fresh_line.is_none(),
            "a non-composer park arms no line gate"
        );

        let mut interrupted = Ribbon::new();
        a_line_of_thirty(&mut interrupted, t0, &c);
        interrupted.on_event(&Event::ComposerNewline, chord, &old);
        assert!(
            interrupted
                .fresh_line
                .is_some_and(|gate| gate.awaiting_move)
        );
        interrupted.curtain(at(chord, 1));
        assert!(
            interrupted.fresh_line.is_some_and(|gate| {
                !gate.awaiting_move && gate.chord_at.is_none() && gate.left_row.is_none()
            }),
            "a new coordinate space cannot wait for the old composer's home"
        );

        let pending = model.successors("ComposerKey", &model.init_state())[0].clone();
        let cancelled = model.successors("CancelBeforeHome", &pending)[0].clone();
        assert_eq!(cancelled.get("composer"), Some(&0));
        for superseding in [
            Event::CancelComposerNewline,
            Event::Erase,
            Event::Kill {
                cells: 1,
                scope: super::super::KillScope::Word,
            },
            row_change((2, 32), (2, 31), Licence::Nav),
        ] {
            let mut interrupted = Ribbon::new();
            a_line_of_thirty(&mut interrupted, t0, &c);
            interrupted.on_event(&Event::ComposerNewline, chord, &old);
            interrupted.on_event(&superseding, at(chord, 1), &old);
            assert!(
                interrupted.fresh_line.is_none(),
                "{superseding:?} left the cancelled composer gate pending"
            );
        }
    }

    /// The light the bed's quads put on `row` this frame as `(coverage,
    /// centroid px, leftmost px)`: the quads whose vertical extent covers the
    /// row's centre, each weighted by width and source-over opacity. `None`
    /// with nothing lit.
    fn row_light(sink: &Sink, g: Geom, row: u16) -> Option<(f32, f32, f32)> {
        let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        let (mut cov, mut num, mut left) = (0.0f32, 0.0f32, f32::INFINITY);
        for q in &sink.under {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
                continue;
            }
            let w = f32::from(q.w) * f32::from(q.alpha) / 255.0;
            cov += w;
            num += (f32::from(q.x) + f32::from(q.w) * 0.5) * w;
            left = left.min(f32::from(q.x));
        }
        (cov > 0.0).then(|| (cov / (g.cols * g.cw) as f32, num / cov, left))
    }

    /// One frame's reading of `row` for the drift law: the quads whose
    /// vertical extent covers the row's centre (the body), as `(coverage
    /// share, leftmost px, rightmost px)`. `None` with nothing lit.
    fn drift_census(sink: &Sink, g: Geom, row: u16) -> Option<DriftCensus> {
        let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        let mut cov = 0.0f32;
        let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);
        for q in &sink.under {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
                continue;
            }
            cov += f32::from(q.w) * f32::from(q.alpha) / 255.0;
            left = left.min(f32::from(q.x));
            right = right.max(f32::from(q.x) + f32::from(q.w));
        }
        (cov > 0.0).then(|| DriftCensus {
            cov: cov / (g.cols * g.cw) as f32,
            left,
            right,
        })
    }

    struct DriftCensus {
        cov: f32,
        left: f32,
        right: f32,
    }

    /// The hot edge's far tail on `row` this frame as `(light, leftmost lit
    /// px)`: the additive quads riding the row's top more than
    /// [`HOT_EDGE_CELLS`] behind a head at `head_col`, where the head's own
    /// falloff is dark, so only the seam ([`Ribbon::seam`]) can light it.
    /// Light is the quad's width times its summed channels.
    fn seam_far_tail(sink: &Sink, g: Geom, row: u16, head_col: u16) -> (f32, f32) {
        let cw = g.cw as f32;
        let head_x = f32::from(g.origin_x) + f32::from(head_col) * cw;
        let (y_lo, y_hi) = (
            f32::from(g.origin_y) + (f32::from(row) - 0.5) * g.ch as f32,
            f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32,
        );
        let (mut far, mut left) = (0.0f32, f32::INFINITY);
        for q in &sink.out {
            let (x, y) = (f32::from(q.x), f32::from(q.y));
            if q.w == 0 || y < y_lo || y >= y_hi || x >= head_x - (HOT_EDGE_CELLS + 1.0) * cw {
                continue;
            }
            let lum = (q.color >> 16 & 0xFF) + (q.color >> 8 & 0xFF) + (q.color & 0xFF);
            far += f32::from(q.w) * lum as f32;
            if lum > 0 {
                left = left.min(x);
            }
        }
        (far, left)
    }

    /// The light the bed's quads put on `row` between `x0` and `x1` px this
    /// frame: [`row_light`]'s coverage, counted only where a quad overlaps
    /// the span.
    fn light_between(sink: &Sink, g: Geom, row: u16, x0: f32, x1: f32) -> f32 {
        let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        let mut cov = 0.0f32;
        for q in &sink.under {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
                continue;
            }
            let (qx0, qx1) = (f32::from(q.x), f32::from(q.x) + f32::from(q.w));
            let w = (qx1.min(x1) - qx0.max(x0)).max(0.0);
            cov += w * f32::from(q.alpha) / 255.0;
        }
        cov / (g.cols * g.cw) as f32
    }

    /// Where a Backspace's `Erase` is replayed against its echo's `Move`.
    /// The engine's replay (`Engine::replay_events`) hands every event but a
    /// `Typed` the tick's caret, which is the post-echo one when the echo
    /// landed in the key's own tick.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum BsEcho {
        /// The echo landed with the key: the erase is replayed at the caret
        /// the echo took the hand to (for the Backspace up through the fold,
        /// the folded row's last column).
        SameTick,
        /// The erase is replayed at the caret the key was pressed at (for the
        /// Backspace up through the fold, the new row's first column) and the
        /// echo's move `lag` ms later, on the first frame at or after it
        /// (`Late(0)`: the key's own frame; `Late(16)`: the next frame).
        Late(u64),
    }

    /// One frame of [`bs_back_through_a_fold`]: `(ms since the fold, row
    /// 2's light, a cohort on row 2 flowing, the phases of row 2's
    /// cohorts)`.
    type BsFrame = (u64, Option<(f32, f32, f32)>, bool, Vec<Phase>);

    /// Twenty keys 60 ms apart on row 2 at columns 100..119, then (when
    /// `folded`) the typed fold to `(3, 0)` one key's cadence after the last;
    /// then a Backspace at each of `bs_ms` (ms after the fold): the first
    /// from `(3, 0)` up to `(2, 119)` (the never-folded control's from
    /// `(2, 120)`), the rest along row 2, each echo shaped by `echo`; then
    /// the caret parked. Row 2 is read every 16 ms to +1520 ms; returned with
    /// the phrase rest in force after the keys.
    fn bs_back_through_a_fold(folded: bool, echo: BsEcho, bs_ms: &[u64]) -> (Vec<BsFrame>, f32) {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 100,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let rest = rib.rest_s();
        let fold = at(t0, 20 * 60);
        let mut caret = (2u16, 120u16);
        if folded {
            caret = (3, 0);
            let cx = ctx(fold, &c, caret, 0.9);
            rib.on_event(&typed_move((2, 120), caret), fold, &cx);
            rib.plan(&cx);
        }
        let mut sink = Sink::default();
        let mut out = Vec::new();
        let mut k = 0usize;
        // A late echo still to land, `(due ms, to)`: the caret stays where
        // the key was pressed until it does.
        let mut due: Option<(u64, (u16, u16))> = None;
        let mut ms = 16u64;
        while ms <= 1520 {
            if k < bs_ms.len() && ms >= bs_ms[k] && due.is_none() {
                let key = at(fold, bs_ms[k]);
                let to = if caret.0 == 3 {
                    (2, 119)
                } else {
                    (2, caret.1 - 1)
                };
                match echo {
                    BsEcho::SameTick => {
                        let cx = ctx(key, &c, to, 0.9);
                        rib.on_event(&Event::Erase, key, &cx);
                        rib.on_event(&typed_move(caret, to), key, &cx);
                        caret = to;
                    }
                    BsEcho::Late(lag) => {
                        let cx = ctx(key, &c, caret, 0.9);
                        rib.on_event(&Event::Erase, key, &cx);
                        due = Some((bs_ms[k] + lag, to));
                    }
                }
                k += 1;
            }
            if let Some((due_ms, to)) = due
                && ms >= due_ms
            {
                let echo_at = at(fold, due_ms);
                let cx = ctx(echo_at, &c, to, 0.9);
                rib.on_event(&typed_move(caret, to), echo_at, &cx);
                caret = to;
                due = None;
            }
            let now = at(fold, ms);
            let cx = ctx(now, &c, caret, 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let row2: Vec<Cohort> = rib
                .cohorts()
                .iter()
                .filter(|q| q.row == 2)
                .copied()
                .collect();
            out.push((
                ms,
                row_light(&sink, g, 2),
                row2.iter().any(|q| q.flow.is_some()),
                row2.iter().map(|q| q.phase).collect::<Vec<_>>(),
            ));
            ms += 16;
        }
        (out, rest)
    }

    /// [`bs_back_through_a_fold`]'s law, read on every frame from `from_ms`
    /// up to (not including) `until_ms`: nothing on row 2 flows, its
    /// cohorts hold in their laying or grace, its left edge stands where it
    /// stood on the first frame read, and its light keeps at least 85 % of
    /// the never-folded control's.
    fn assert_the_row_holds_under_the_hand(
        what: &str,
        folded: &[BsFrame],
        control: &[BsFrame],
        from_ms: u64,
        until_ms: u64,
    ) {
        let settled = folded
            .iter()
            .find(|s| s.0 >= from_ms)
            .and_then(|s| s.1)
            .unwrap_or_else(|| panic!("{what}: row 2 is lit at +{from_ms} ms"))
            .2;
        let mut read = 0u32;
        for (f, k) in folded.iter().zip(control) {
            let (ms, light, flowing, phases) = f;
            if *ms < from_ms || *ms >= until_ms {
                continue;
            }
            read += 1;
            assert!(
                !flowing,
                "{what}: +{ms} ms: row 2 still flows under the hand that came back to it"
            );
            assert!(
                phases
                    .iter()
                    .all(|p| matches!(p, Phase::Laying | Phase::Grace)),
                "{what}: +{ms} ms: the row left its hold under the hand: {phases:?}"
            );
            let (cov, _, left) =
                light.unwrap_or_else(|| panic!("{what}: +{ms} ms: row 2 is lit under the hand"));
            assert!(
                (left - settled).abs() <= 0.5,
                "{what}: +{ms} ms: the row's left edge moved from {settled} px to {left} px: the \
                 drift restarted"
            );
            let (want, _, _) = k.1.expect("the control is lit");
            assert!(
                cov >= 0.85 * want,
                "{what}: +{ms} ms: row 2 drained under the hand: {cov} against the never-folded \
                 control's {want}"
            );
        }
        assert!(
            read >= 10,
            "{what}: the window +{from_ms}..+{until_ms} ms read {read} frames"
        );
    }

    /// The hand arriving back through the fold takes the row back ([`Ribbon::hold`],
    /// [`Ribbon::take_row_back`]): a Backspace run inside the rest, one after it, and a lone
    /// Backspace. A shell's erase lays nothing, so it reaches the flowing row only through the
    /// hold, which must also stop the flow, or every Backspace would restart the drift. When the
    /// echo lands with the key the erase is replayed on the row and re-wets it at once; a late
    /// echo's erase is replayed at `(3, 0)` and holds only that row, after the rest it mints a
    /// phrase that adopts only that row, and a lone Backspace makes no erase on row 2 at all. Row
    /// 2 holds with the never-folded control from the echo's frame through the last hold's grace,
    /// for every echo shape ([`BsEcho`]).
    #[test]
    fn the_hand_arriving_back_through_a_shell_fold_takes_the_row_back_after_the_rest_and_alone() {
        let runs: [(&str, &[u64]); 3] = [
            ("a run inside the rest", &[416, 480, 540, 600, 660, 720]),
            ("a run after the rest", &[900, 960, 1020, 1080, 1140]),
            ("a lone Backspace", &[416]),
        ];
        for (name, bs_ms) in runs {
            for echo in [BsEcho::SameTick, BsEcho::Late(0), BsEcho::Late(16)] {
                let what = format!("{name}, {echo:?}");
                let (control, rest) = bs_back_through_a_fold(false, echo, bs_ms);
                let (folded, _) = bs_back_through_a_fold(true, echo, bs_ms);
                let lag = match echo {
                    BsEcho::SameTick => 0,
                    BsEcho::Late(lag) => lag,
                };
                let rest_ms = (rest * 1000.0) as u64;
                if name == "a run after the rest" {
                    // The fixture's point: the first Backspace comes one key's
                    // cadence plus its offset after the last key, at or past
                    // the rest in force.
                    assert!(
                        60 + bs_ms[0] >= rest_ms,
                        "{what}: the run starts {} ms after the last key, inside the {rest_ms} \
                         ms rest",
                        60 + bs_ms[0]
                    );
                }
                let from = bs_ms[0] + lag;
                let until = bs_ms[bs_ms.len() - 1] + lag + rest_ms;
                assert_the_row_holds_under_the_hand(&what, &folded, &control, from, until);
            }
        }
    }

    /// A composer re-wrap's moved word fades in place while the row it left drifts into its
    /// kept end: no frame takes more than 4 % of the light, the word keeps 80 % at +16 ms, and
    /// nothing is drawn past its old end, nor past the kept end once it is gone.
    #[test]
    fn a_composer_re_wrap_s_moved_word_fades_in_place_while_the_row_it_left_drifts() {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 40,
            n: 30,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        for i in 0..9u16 {
            let now = at(t0, u64::from(30 + i) * 60);
            let ev = if i == 0 { typed_space() } else { typed() };
            let cx = ctx(now, &c, (2, 71 + i), 0.9);
            rib.on_event(&ev, now, &cx);
            rib.plan(&cx);
        }
        // The wrap key: the app carries the word down, and the caret stands
        // after the key's glyph.
        let wrap = at(t0, 39 * 60);
        let cx = ctx(wrap, &c, (3, 11), 0.9);
        rib.on_event(&typed(), wrap, &cx);
        rib.on_event(&typed_move((2, 79), (3, 11)), wrap, &cx);
        rib.plan(&cx);
        let (word_x0, word_x1) = (70.0 * cw, 79.0 * cw);
        let kept_end = 70.0 * cw;
        let mut sink = Sink::default();
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let first = drift_census(&sink, g, 2).expect("row 2 is lit at the wrap");
        let word0 = light_between(&sink, g, 2, word_x0, word_x1);
        assert!(
            word0 > 0.0 && (first.right - word_x1).abs() <= 0.5,
            "the fixture's point: the moved word is lit on row 2 at the wrap ({word0}, right \
             edge {} px)",
            first.right
        );
        let mut prev = first.cov;
        let mut worst = 0.0f32;
        let mut clipped = 0u32;
        let mut key_col = 11u16;
        let mut ms = 16u64;
        while ms <= 1600 {
            let now = at(wrap, ms);
            if ms / 60 != (ms - 16) / 60 {
                key_col += 1;
                let kx = ctx(now, &c, (3, key_col), 0.9);
                rib.on_event(&typed(), now, &kx);
            }
            let cx = ctx(now, &c, (3, key_col), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let Some(r) = drift_census(&sink, g, 2) else {
                worst = worst.max(prev / first.cov);
                prev = 0.0;
                ms += 16;
                continue;
            };
            if ms == 16 {
                let word = light_between(&sink, g, 2, word_x0, word_x1);
                assert!(
                    word >= 0.8 * word0,
                    "the moved word was cut on the flow's first frame: {word} of its {word0}"
                );
            }
            worst = worst.max((prev - r.cov) / first.cov);
            assert!(
                r.right <= word_x1 + 0.5,
                "light at {} px, past the moved word's old end at {word_x1} px (+{ms} ms)",
                r.right
            );
            if !rib.cells().iter().any(|l| l.row == 2 && l.col >= 70) {
                clipped += 1;
                assert!(
                    r.right <= kept_end + 0.5,
                    "CLIP: light at {} px, past the row's kept end at {kept_end} px (+{ms} ms)",
                    r.right
                );
            }
            prev = r.cov;
            ms += 16;
        }
        assert!(
            worst <= 0.04,
            "NO CLIFF: a frame took {:.1} % of the wrap frame's light",
            100.0 * worst
        );
        assert!(
            clipped >= 20,
            "CLIP was read on the drifting band after the word had gone: {clipped} frames"
        );
    }

    /// A band carried down off the caret's row flows into its own left end
    /// ([`Ribbon::flow_target`]); its left edge never passes it while the right end drifts
    /// toward it (the `max` clamp in [`Ribbon::retract_x`]).
    #[test]
    fn a_band_carried_down_off_the_caret_s_row_drifts_into_its_kept_start_and_never_past_it() {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 5,
            col0: 20,
            n: 30,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let carry = at(t0, 30 * 60);
        let id = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 5)
            .map(|k| k.id)
            .expect("the band");
        let run = super::super::witness::FollowRun {
            row: 5,
            cohort: id,
            lo: 20,
            hi: 49,
            dr: 1,
        };
        let mut moved = Vec::new();
        assert_eq!(rib.translate_run(run, carry, true, &mut moved), 30);
        let cx = ctx(carry, &c, (5, 50), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        let first = drift_census(&sink, g, 6).expect("row 6 is lit at the carry");
        let start = f32::from(g.origin_x) + 20.0 * cw;
        assert!(
            (first.left - start).abs() <= 0.5,
            "the fixture's point: the band starts on the clip line: {} px",
            first.left
        );
        let mut drifted = false;
        let mut ms = 16u64;
        while ms <= 1600 {
            let now = at(carry, ms);
            let cx = ctx(now, &c, (5, 50), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            if ms == 16 {
                let coh = rib
                    .cohorts()
                    .iter()
                    .find(|k| k.id == id)
                    .copied()
                    .expect("the carried band");
                assert!(coh.flow.is_some(), "the carry set the band flowing");
                assert!(
                    coh.drain_right_first && coh.retract_col == Some(20),
                    "a row below the caret flows into its own left end: {:?}, right first {}",
                    coh.retract_col,
                    coh.drain_right_first
                );
            }
            if let Some(r) = drift_census(&sink, g, 6) {
                assert!(
                    r.left >= start - 0.5,
                    "CLIP: light at {} px, past the band's kept start at {start} px (+{ms} ms)",
                    r.left
                );
                drifted |= r.right < first.right - cw;
            }
            ms += 16;
        }
        assert!(drifted, "the band drifted toward its start");

        // A public shift can put an old reach cell onto a newer leaving
        // writer. Both residents price the kept edge, regardless of which
        // side the hand is on or where each sits in the append-ordered pool.
        for below in [false, true] {
            for edge_disp in [0.0, 1.0] {
                let mut rib = Ribbon::new();
                // Establish the hand's floor, then let that old band leave.
                type_run(&mut rib, t0, 0, 1, &c, 0.0);
                rib.plan(&ctx(at(t0, 5000), &c, (2, 1), 0.0));
                assert!(rib.at_rest());
                let born = at(t0, 6000);
                type_run(&mut rib, born, 10, 2, &c, 0.0);
                let edge_at = at(born, 120);
                let cx = ctx(edge_at, &c, (2, 13), edge_disp);
                rib.on_event(&typed(), edge_at, &cx);
                rib.plan(&cx);
                let edge = *rib
                    .cells()
                    .iter()
                    .find(|cell| cell.col == 12)
                    .expect("edge");
                let coh = *rib
                    .cohorts()
                    .iter()
                    .find(|coh| coh.id == edge.cohort)
                    .expect("band");
                let reach_at = edge_at + Duration::from_secs_f32(coh.grace_s + 0.001);
                assert_eq!(rib.retire_cells(&[(2, 12, edge.born)], reach_at), 1);
                // The real reach pass appends column 9 on the cold tail's clock.
                rib.plan(&ctx(reach_at, &c, (2, 13), 0.0));
                let reach = *rib
                    .cells()
                    .iter()
                    .find(|cell| cell.col == 9)
                    .expect("reach");
                assert!(!reach.leaving() && reach.born < edge.born);
                let mut moved = Vec::new();
                assert_eq!(
                    rib.shift_run(
                        super::super::witness::ShiftRun {
                            row: 2,
                            cohort: coh.id,
                            lo: 9,
                            hi: 9,
                            dc: 3,
                        },
                        &mut moved
                    ),
                    1
                );
                // Widen the kept span so its cap distinguishes cold and hot pricing.
                assert_eq!(
                    rib.shift_run(
                        super::super::witness::ShiftRun {
                            row: 2,
                            cohort: coh.id,
                            lo: 10,
                            hi: 11,
                            dc: -8,
                        },
                        &mut moved
                    ),
                    2
                );
                let (edge_col, kept) = if below {
                    assert_eq!(
                        rib.shift_run(
                            super::super::witness::ShiftRun {
                                row: 2,
                                cohort: coh.id,
                                lo: 2,
                                hi: 3,
                                dc: 18,
                            },
                            &mut moved
                        ),
                        2
                    );
                    (12, 10.0)
                } else {
                    (12, 11.0)
                };
                let residents: Vec<_> = rib
                    .cells()
                    .iter()
                    .filter(|cell| cell.col == edge_col)
                    .collect();
                assert_eq!(residents.len(), 2);
                assert!(residents[0].leaving() && residents[0].born == edge.born);
                assert!(!residents[1].leaving() && residents[1].born == reach.born);
                assert_eq!(residents[0].birth_disp, edge_disp);
                assert_eq!(residents[1].birth_disp, 0.0);
                assert_eq!(
                    rib.translate_run(
                        super::super::witness::FollowRun {
                            row: 2,
                            cohort: coh.id,
                            lo: 0,
                            hi: u16::MAX,
                            dr: 1,
                        },
                        reach_at,
                        true,
                        &mut moved
                    ),
                    4
                );
                let caret = (if below { 1 } else { 4 }, 13);
                rib.plan(&ctx(at(reach_at, 1), &c, caret, 0.0));
                let flowed = rib
                    .cohorts()
                    .iter()
                    .find(|coh| coh.id == edge.cohort)
                    .expect("flow");
                assert_eq!(flowed.retract_col, Some(edge_col));
                assert_eq!(flowed.drain_right_first, below);
                let priced = if edge_disp == 0.0 {
                    FLOW_DRIFT_MIN_CELLS
                } else {
                    FLOW_DRIFT_MAX_CELLS
                };
                assert_eq!(flowed.flow_drift, priced.min(FLOW_DRIFT_MAX_SHARE * kept));
            }
        }
    }

    /// A band move under the hand carries the seam and its floor together: the seam reaches
    /// back to the hand's first column on its row ([`Ribbon::hand_floor_on`]), so after a fold,
    /// twelve keys and the band moved down a row (an inline viewport sliding), the seam on row 4
    /// reads what it reads on row 3 unmoved and the floor is `(4, 0)`
    /// ([`Ribbon::translate_band`]).
    #[test]
    fn a_band_move_under_the_hand_carries_the_seam_and_its_floor_together() {
        let c = cfg(true, true);
        let g = geom();
        let read = |moved: bool| {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row: 2,
                col0: 100,
                n: 20,
                period_ms: 60,
                disp: 0.9,
            };
            type_keys(&mut rib, t0, keys, &c);
            let fold = at(t0, 20 * 60);
            let cx = ctx(fold, &c, (3, 0), 0.9);
            rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
            rib.plan(&cx);
            let keys = Keys {
                g,
                row: 3,
                col0: 0,
                n: 12,
                period_ms: 20,
                disp: 0.9,
            };
            type_keys(&mut rib, at(fold, 20), keys, &c);
            let row = if moved {
                rib.translate_band(0, 39, 1);
                4
            } else {
                3
            };
            let now = at(fold, 300);
            let cx = ctx(now, &c, (row, 12), 0.9);
            rib.plan(&cx);
            let mut sink = Sink::default();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            (seam_far_tail(&sink, g, row, 12).0, rib.hand_floor_cell())
        };
        let (still, floor_still) = read(false);
        let (moved, floor_moved) = read(true);
        assert!(still > 0.0, "the seam holds the new line's far tail");
        assert_eq!(floor_still, Some((3, 0)));
        assert_eq!(
            floor_moved,
            Some((4, 0)),
            "the hand's floor rides the band move with its text"
        );
        assert!(
            (moved - still).abs() <= 0.01 * still,
            "the seam went with its row: {moved} on row 4 after the move, {still} on row 3 \
             without it"
        );
    }

    /// Under reduced motion a flowing band asks for no wake before its fade: the flow moves
    /// nothing and its life is frozen at the fold, so the band is static until the one linear
    /// fade opens at `FLOW_TOTAL_S − REDUCED_MOTION_FADE_MS` (§6.11). Every read asks to be
    /// woken no sooner than that, one [`TAIL_FLOOR`] early at most.
    ///
    /// [`TAIL_FLOOR`]: super::super::TAIL_FLOOR
    #[test]
    fn under_reduced_motion_a_flowing_band_asks_for_no_wake_before_its_fade() {
        let mut c = cfg(true, true);
        c.reduced_motion = true;
        let g = geom();
        let opens_ms = (FLOW_TOTAL_S * 1000.0 - REDUCED_MOTION_FADE_MS).round() as u64;
        for pause_ms in [60u64, 960] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row: 2,
                col0: 100,
                n: 20,
                period_ms: 60,
                disp: 0.2,
            };
            type_keys(&mut rib, t0, keys, &c);
            let fold = at(t0, 19 * 60 + pause_ms);
            let cx = ctx(fold, &c, (3, 0), 0.2);
            rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().any(|k| k.row == 2 && k.flow.is_some()),
                "pause {pause_ms} ms: the fold set the row flowing"
            );
            let earliest = at(fold, opens_ms) - super::super::TAIL_FLOOR;
            let mut ms = 100u64;
            while ms <= 1300 {
                let now = at(fold, ms);
                let cx = ctx(now, &c, (3, 0), 0.2);
                rib.plan(&cx);
                let d = rib.next_change_deadline(now).expect("the band is live");
                assert!(
                    d >= earliest,
                    "pause {pause_ms} ms: the read at +{ms} ms asks for a wake at +{} ms, before \
                     the fade opens at +{opens_ms} ms — nothing on the glass changes",
                    d.saturating_duration_since(fold).as_millis()
                );
                ms += 25;
            }
        }
    }

    /// A shift of a flowing band carries its frozen fold edge ([`Ribbon::flow_target`]): an
    /// insert after a Nav back up pushes the tail right ([`Ribbon::shift_run`]); the edge is a
    /// glyph's column, so it rides the shift (the walk's anchor does not), and the pushed
    /// glyph's column stays lit through the flow.
    #[test]
    fn a_shift_of_a_flowing_band_carries_its_frozen_fold_edge() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 60,
            n: 40,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let fold = at(t0, 40 * 60);
        let cx = ctx(fold, &c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 100), (3, 0)), fold, &cx);
        rib.plan(&cx);
        let cx = ctx(at(fold, 16), &c, (3, 0), 0.9);
        rib.plan(&cx);
        let flow = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.flow.is_some())
            .copied()
            .expect("the folded row flows");
        assert_eq!(flow.retract_col, Some(99), "frozen at the row's kept end");
        let back = at(fold, 64);
        let cx = ctx(back, &c, (2, 80), 0.9);
        rib.on_event(&nav((3, 0), (2, 80)), back, &cx);
        rib.plan(&cx);
        let key = at(fold, 80);
        let cx = ctx(key, &c, (2, 81), 0.9);
        rib.on_event(&typed(), key, &cx);
        let shift = super::super::witness::ShiftRun {
            row: 2,
            cohort: flow.id,
            lo: 80,
            hi: 99,
            dc: 1,
        };
        let mut moved = Vec::new();
        assert_eq!(rib.shift_run(shift, &mut moved), 20, "the tail moved");
        rib.plan(&cx);
        let after = rib
            .cohorts()
            .iter()
            .find(|k| k.id == flow.id)
            .copied()
            .expect("the band");
        assert!(after.flow.is_some(), "the row is still flowing");
        assert_eq!(
            after.retract_col,
            Some(100),
            "the frozen fold edge rides the shift with the glyph it bounds"
        );
        let mut sink = Sink::default();
        for ms in [96u64, 160, 320, 640] {
            let now = at(fold, ms);
            let cx = ctx(now, &c, (2, 81), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            assert!(
                cell_lit(&sink.under, g, 2, 100),
                "+{ms} ms: the pushed glyph's column 100 is dark"
            );
        }
    }

    /// A slow hand's folded row flows whole, whatever it paused before the wrap: its cells live
    /// 1.70 s and a pause spends most of that before the flow, so expiry is frozen at the fold
    /// ([`Ribbon::env_of`], [`Ribbon::retire`]). After a pause of 60, 600 or 900 ms the flow
    /// keeps its own timeline: dark between `0.8 · FLOW_TOTAL_S` and `FLOW_TOTAL_S` plus a
    /// frame, no frame taking more than 4 % of the fold frame's light.
    #[test]
    fn a_slow_hand_s_folded_row_flows_whole_whatever_it_paused_before_the_wrap() {
        let c = cfg(true, true);
        let g = geom();
        // `(dark at, s; light at +1.0 s as a share of the fold frame's;
        // the worst frame's drop, the same share)`.
        let flow = |pause_ms: u64| {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row: 2,
                col0: 90,
                n: 30,
                period_ms: 150,
                disp: 0.2,
            };
            type_keys(&mut rib, t0, keys, &c);
            let fold = at(t0, 29 * 150 + pause_ms);
            let cx = ctx(fold, &c, (3, 0), 0.2);
            rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
            rib.plan(&cx);
            assert!(
                rib.cohorts().iter().any(|k| k.row == 2 && k.flow.is_some()),
                "pause {pause_ms} ms: the fold set the row flowing"
            );
            let mut sink = Sink::default();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let first = row_light(&sink, g, 2).expect("lit at the fold").0;
            let (mut prev, mut worst, mut share, mut dark) = (first, 0.0f32, None, None);
            let mut key_col = 0u16;
            let mut ms = 16u64;
            while ms <= 1700 {
                let now = at(fold, ms);
                if ms / 150 != (ms - 16) / 150 {
                    key_col += 1;
                    let kx = ctx(now, &c, (3, key_col), 0.2);
                    rib.on_event(&typed(), now, &kx);
                }
                let cx = ctx(now, &c, (3, key_col), 0.2);
                rib.plan(&cx);
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
                let cov = row_light(&sink, g, 2).map_or(0.0, |l| l.0);
                assert!(
                    dark.is_none() || cov == 0.0,
                    "pause {pause_ms} ms: lit again at +{ms} ms after going dark"
                );
                if cov == 0.0 && dark.is_none() {
                    dark = Some(ms as f32 / 1000.0);
                }
                worst = worst.max((prev - cov) / first);
                if ms == 1008 {
                    share = Some(cov / first);
                }
                prev = cov;
                ms += 16;
            }
            (
                dark.expect("the folded row goes dark"),
                share.expect("read at +1.0 s"),
                worst,
            )
        };
        let base = flow(60);
        let (dark0, share0, _) = base;
        for (pause_ms, (dark, share, worst)) in [(60u64, base), (600, flow(600)), (900, flow(900))]
        {
            assert!(
                (0.8 * FLOW_TOTAL_S..=FLOW_TOTAL_S + 0.017).contains(&dark),
                "pause {pause_ms} ms: LIFE: dark at +{dark} s"
            );
            assert!(
                worst <= 0.04,
                "pause {pause_ms} ms: NO CLIFF: a frame took {:.1} % of the fold frame's light",
                100.0 * worst
            );
            assert!(
                (dark - dark0).abs() <= 0.017 && (share - share0).abs() <= 0.01,
                "pause {pause_ms} ms: the flow's timeline depends on the pause: dark at +{dark} s \
                 and {share} of the light at +1.0 s, where the 60 ms pause reads +{dark0} s and \
                 {share0}"
            );
        }
    }

    /// A program's round trip does not strand the band: a TUI repaints a row
    /// below and parks the cursor back; the hand's next key takes the band
    /// back, and in between it retracts toward the hand's column, never the
    /// parked one.
    #[test]
    fn a_program_round_trip_does_not_strand_the_band_under_a_typing_hand() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let cx = ctx(at(last, 100), &c, (3, 0), 0.9);
        rib.on_event(&pty((2, 10), (3, 0)), at(last, 100), &cx);
        rib.plan(&cx);
        let coh = rib.cohorts().iter().find(|k| k.row == 2).expect("the band");
        assert!(coh.abandoned && coh.rejoinable);
        assert_eq!(
            coh.retract_col,
            Some(10),
            "retracts toward the hand, not the parked cursor"
        );
        let cx = ctx(at(last, 116), &c, (2, 10), 0.9);
        rib.on_event(&pty((3, 0), (2, 10)), at(last, 116), &cx);
        rib.plan(&cx);
        let k = at(last, 200);
        let cx = ctx(k, &c, (2, 11), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let cohorts: Vec<_> = rib.cohorts().iter().filter(|k| k.row == 2).collect();
        assert_eq!(cohorts.len(), 1, "one band, taken back: {cohorts:?}");
        assert!(!cohorts[0].abandoned);
        assert_eq!((cohorts[0].col0, cohorts[0].col1), (2, 11));
        assert!(
            rib.cells()
                .iter()
                .filter(|l| l.row == 2)
                .all(|l| l.retract_at.is_none()),
            "no cell is left draining under the hand"
        );
    }

    // -- the caret notch and slivers -----------------------------------------
    //
    // A composer line at 12 cps with a block caret right of the last word. The pins read a
    // per-device-column census of the emitted quads on the reference glass (`geom_owner`):
    // three slabs per cell for the composer line alone, one for a screenful of lit rows.

    /// The reference glass: 144 × 60 cells of 15 × 28 pt at scale 2, 30 × 56
    /// device px.
    fn geom_owner() -> Geom {
        Geom {
            cw: 30,
            ch: 56,
            rows: 60,
            cols: 144,
            origin_x: 0,
            origin_y: 0,
            win_w: 4320,
            win_h: 3360,
            head: 0,
        }
    }

    /// The reference config: the default dark theme at `intensity 0.70`.
    fn cfg_owner() -> Config {
        let mut c = cfg(true, true);
        c.intensity = 0.70;
        c
    }

    /// The reference row: the composer line, row 56 of 60.
    const OWNER_ROW: u16 = 56;

    /// Type the reference line on [`OWNER_ROW`] from column 2 at `period_ms`
    /// a key with a `pause_ms` thinking pause after `also ` (the first key of
    /// `seee` is a word's first key), planning after every key as the engine
    /// does. Returns the last key's offset from `t0` and the caret column
    /// after it.
    fn type_owner_line(
        rib: &mut Ribbon,
        t0: Instant,
        c: &Config,
        disp: f32,
        period_ms: u64,
        pause_ms: u64,
    ) -> (u64, u16) {
        let g = geom_owner();
        let mut ms = 0u64;
        let mut col = 2u16;
        let mut last = 0u64;
        for (i, _) in "and I also seee an ugly gap".chars().enumerate() {
            let now = at(t0, ms);
            let cx = ctx_in(now, c, (OWNER_ROW, col + 1), disp, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            last = ms;
            ms += period_ms;
            if i == 10 {
                ms += pause_ms;
            }
        }
        (last, col)
    }

    /// One frame on the reference glass: plan and emit at `now` with the caret
    /// at `caret`, into `sink`.
    fn owner_frame(
        rib: &mut Ribbon,
        sink: &mut Sink,
        c: &Config,
        now: Instant,
        caret: u16,
        disp: f32,
    ) {
        let cx = ctx_in(now, c, (OWNER_ROW, caret), disp, geom_owner());
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }

    /// The device rows the reference row's band can occupy: half a cell above
    /// its top (the crown and the sky's stars are above that) to a cell below
    /// its bottom (the rail).
    fn owner_rows() -> std::ops::Range<i32> {
        let g = geom_owner();
        let top = i32::from(OWNER_ROW) * g.ch as i32;
        top - g.ch as i32 / 2..top + 2 * g.ch as i32
    }

    /// The attach: under a live caret the band's top holds its plateau through the head cell
    /// and the body continues under the caret cell, so no column from the head cell to the
    /// block's right edge is ground-dark, tops out more than a row under the plateau, or has a
    /// rail or a peak short of the head's. On a cold hand and a hot one, the hairline over the
    /// head cell on both ([`HOT_EDGE_GAIN_FLOOR`]).
    #[test]
    fn the_caret_block_stands_on_the_band_under_a_cold_hand_and_a_hot_one() {
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            let c = cfg_owner();
            let g = geom_owner();
            let cw = g.cw as i32;
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let (last, caret) = type_owner_line(&mut rib, t0, &c, disp, 83, 700);
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, last + 100), caret, disp);
            assert_eq!(
                rib.slabs_per_cell(),
                3,
                "{hand}: the composer line alone plans at three slabs"
            );
            let head_x0 = i32::from(caret - 1) * cw;
            let block_x1 = i32::from(caret + 1) * cw;
            let x0 = head_x0 - 3 * cw;
            let cens = column_census(&sink.under, x0..block_x1, owner_rows(), 8);
            // The plateau: the highest top over the three cells before the head.
            let plateau = cens[..3 * cw as usize]
                .iter()
                .filter_map(|e| e.map(|(t, _, _)| t))
                .min()
                .expect("the cells before the head are lit");
            let head = &cens[3 * cw as usize..4 * cw as usize];
            let head_bot = head
                .iter()
                .filter_map(|e| e.map(|(_, b, _)| b))
                .max()
                .unwrap_or(0);
            let head_peak = head
                .iter()
                .filter_map(|e| e.map(|(_, _, p)| p))
                .max()
                .unwrap_or(0);
            let mut dark = Vec::new();
            for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
                let x = x0 + k as i32;
                let Some((top, bot, peak)) = *e else {
                    dark.push(x);
                    continue;
                };
                assert!(
                    top <= plateau + 1,
                    "{hand}: column {x} tops out at {top}, under the plateau's {plateau}"
                );
                if x >= head_x0 + cw {
                    assert!(
                        bot >= head_bot - 1,
                        "{hand}: the foot under the block at column {x} ends at {bot}, the head's rail at {head_bot}"
                    );
                    assert!(
                        peak + 2 >= head_peak,
                        "{hand}: the band under the block at column {x} peaks at {peak}, the head at {head_peak}"
                    );
                }
            }
            assert!(
                dark.is_empty(),
                "{hand}: ground-dark columns between the head cell and the block's right edge: {} of them, from x {}",
                dark.len(),
                dark[0]
            );
            // The hairline stands over the head cell, cold hand included.
            let hot = column_census(&sink.out, head_x0..head_x0 + cw, owner_rows(), 1);
            let unlit = hot.iter().filter(|e| e.is_none()).count();
            assert_eq!(
                unlit, 0,
                "{hand}: {unlit} of the head cell's columns carry no hot edge"
            );
        }
    }

    /// One frame's census on the reference row: `(ms, caret, column_census
    /// over the leg's columns at floor 8)`.
    type FrameCensus = (u64, u16, Vec<Option<(i32, i32, u8)>>);

    /// One frame of `column_census` per 16 ms on the reference row over `ms`:
    /// before each frame `step` applies whatever the leg has due at that
    /// instant and names the caret column the frame is planned at.
    fn owner_frames(
        rib: &mut Ribbon,
        t0: Instant,
        c: &Config,
        disp: f32,
        ms: std::ops::RangeInclusive<u64>,
        xs: std::ops::Range<i32>,
        mut step: impl FnMut(&mut Ribbon, u64) -> u16,
    ) -> Vec<FrameCensus> {
        let mut sink = Sink::default();
        let mut frames = Vec::new();
        let (mut ms, to_ms) = (*ms.start(), *ms.end());
        while ms <= to_ms {
            let caret = step(rib, ms);
            owner_frame(rib, &mut sink, c, at(t0, ms), caret, disp);
            frames.push((
                ms,
                caret,
                column_census(&sink.under, xs.clone(), owner_rows(), 8),
            ));
            ms += 16;
        }
        frames
    }

    /// The fade's own steepest per-frame slope, in levels: `spend` falls at
    /// most `2 / RETRACT_FADE_S` a second from a 215-level body, 29 levels a
    /// 16 ms frame, plus the wave's own anti-aliasing at the plateau.
    const FADE_STEP_MAX: i32 = 36;

    /// The block stands on whole light after a jump and the keys that follow: ten keys, Home
    /// (a corridor over dark cols 0..1), End (a scrub back), three keys; the head cell and the
    /// block's cell read the band's level across every device column.
    #[test]
    fn the_block_stands_on_whole_light_after_a_jump_and_the_keys_that_follow_it() {
        let g = geom_owner();
        let cw = g.cw as i32;
        let row = OWNER_ROW;
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 10,
                period_ms: 83,
                disp,
            };
            type_keys(&mut rib, t0, keys, &c);
            let mut ms = 9 * 83 + 120;
            for (from, to) in [((row, 12u16), (row, 0u16)), ((row, 0), (row, 12))] {
                let now = at(t0, ms);
                let cx = ctx_in(now, &c, to, disp, g);
                rib.on_event(&nav(from, to), now, &cx);
                rib.plan(&cx);
                ms += 120;
            }
            let mut caret = 12u16;
            for _ in 0..3 {
                ms += 83;
                caret += 1;
                let now = at(t0, ms);
                let cx = ctx_in(now, &c, (row, caret), disp, g);
                rib.on_event(&typed(), now, &cx);
                rib.plan(&cx);
            }
            ms += 300;
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, ms), caret, disp);
            // The plateau is read off the four settled cells before the head;
            // the head cell and the block's own cell must match it.
            let x0 = i32::from(caret - 5) * cw;
            let x1 = i32::from(caret + 1) * cw;
            let cens = column_census(&sink.under, x0..x1, owner_rows(), 8);
            let plateau = cens[..4 * cw as usize]
                .iter()
                .filter_map(|e| e.map(|(_, _, p)| p))
                .max()
                .expect("the band before the head is lit");
            let show = |lo: usize| {
                (0..3)
                    .map(|j| cens[lo + j * (cw as usize / 3)].map_or(0, |(_, _, p)| i32::from(p)))
                    .collect::<Vec<_>>()
            };
            for (k, e) in cens.iter().enumerate().skip(4 * cw as usize) {
                let x = x0 + k as i32;
                let peak = e.map_or(0, |(_, _, p)| p);
                assert!(
                    i32::from(plateau) - i32::from(peak) <= 2,
                    "{hand}: device column {x} reads {peak} against the band's {plateau} — the head cell is {:?} and the block's own {:?}",
                    show(4 * cw as usize),
                    show(5 * cw as usize)
                );
            }
        }
    }

    /// The block stands on the band through a leaving caret cell, over `frames`: every
    /// caret-cell column lit on every frame, no column rising more than a dither step in one
    /// frame, and none from the caret cell to `hold_x1` falling faster than the fade.
    fn assert_block_on_band(frames: &[FrameCensus], x0: i32, cw: i32, hold_x1: i32, leg: &str) {
        for (i, (ms, caret, cens)) in frames.iter().enumerate() {
            let cx0 = i32::from(*caret) * cw;
            let dark: Vec<i32> = (cx0..cx0 + cw)
                .filter(|x| cens[(x - x0) as usize].is_none())
                .collect();
            assert!(
                dark.is_empty(),
                "{leg}, +{ms} ms, caret {caret}: {} ground-dark columns under the block, from x {}",
                dark.len(),
                dark[0]
            );
            if i == 0 {
                continue;
            }
            let prev = &frames[i - 1].2;
            let level = |e: &Option<(i32, i32, u8)>| e.map_or(0, |(_, _, p)| i32::from(p));
            for (k, (now, was)) in cens.iter().zip(prev.iter()).enumerate() {
                let x = x0 + k as i32;
                let (now, was) = (level(now), level(was));
                assert!(
                    now - was <= 2,
                    "{leg}, +{ms} ms: column {x} rose {was} -> {now} in one frame"
                );
                if (cx0..hold_x1).contains(&x) {
                    assert!(
                        was - now <= FADE_STEP_MAX,
                        "{leg}, +{ms} ms: column {x} fell {was} -> {now} in one frame, faster than the fade"
                    );
                }
            }
        }
    }

    /// The attach survives an erase: a leaving cell under the caret is floored at the attach's
    /// level ([`Ribbon::plan_run`]), so the block stands on the band while it spends and its
    /// retirement changes no byte. Four legs per hand: one Backspace, four at 30/s, the caret's
    /// own cells retired by content, a neighbour wake's cell retired.
    #[test]
    fn the_block_stays_on_the_band_while_the_cell_under_it_leaves_and_when_it_is_gone() {
        let g = geom_owner();
        let cw = g.cw as i32;
        let row = OWNER_ROW;
        let fade_ms = (RETRACT_FADE_S * 1000.0) as u64;
        let melt_ms = (RETIRE_MELT_S * 1000.0) as u64;
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            // Legs 1 and 2: `erases` Backspaces `period` ms apart after an
            // eight-key word (cells 2..=9, the caret at 10).
            for (period, erases, leg) in [
                (0u64, 1u16, "one Backspace"),
                (33, 4, "Backspace held at 30/s"),
            ] {
                let leg = format!("{hand} hand, {leg}");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 8,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                let ms0 = 7 * 83 + 300;
                let last_erase = ms0 + u64::from(erases - 1) * period;
                let mut done = 0u16;
                let mut caret = 10u16;
                let x0 = i32::from(10 - erases) * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=last_erase + fade_ms + 200,
                    x0..11 * cw,
                    |rib, ms| {
                        while done < erases && ms >= ms0 + u64::from(done) * period {
                            caret -= 1;
                            let now = at(t0, ms);
                            let cx = ctx_in(now, &c, (row, caret), disp, g);
                            rib.on_event(&Event::Erase, now, &cx);
                            done += 1;
                        }
                        caret
                    },
                );
                assert_block_on_band(&frames, x0, cw, 10 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= caret),
                    "{leg}: the erased cells retired inside the window"
                );
                assert_eq!(caret, 10 - erases, "{leg}: every erase landed");
            }
            // Leg 3: the caret's own run's cells retired by content under the
            // caret: three arrows left into the word, a key typed there, and
            // the witness retiring the cells whose glyphs the insert shifted
            // (the caret cell first among them).
            {
                let leg = format!("{hand} hand, own cells retired under the caret");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 8,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                let mut ms = 7 * 83 + 300;
                let cx = ctx_in(at(t0, ms), &c, (row, 7), disp, g);
                rib.on_event(&nav((row, 10), (row, 7)), at(t0, ms), &cx);
                rib.plan(&cx);
                ms += 300;
                let cx = ctx_in(at(t0, ms), &c, (row, 8), disp, g);
                rib.on_event(&typed(), at(t0, ms), &cx);
                rib.plan(&cx);
                let ms0 = ms + 60;
                let ids: Vec<(u16, u16, Instant)> = rib
                    .cells()
                    .iter()
                    .filter(|l| l.row == row && l.col >= 8)
                    .map(|l| (l.row, l.col, l.born))
                    .collect();
                let mut retired = false;
                let x0 = 7 * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=ms0 + melt_ms + 200,
                    x0..11 * cw,
                    |rib, ms| {
                        if !retired && ms >= ms0 {
                            // Two, not three: the three arrows left over the
                            // word's own lit cells are a scrub, which lays no
                            // wake, and the key at col 8 re-lays the cell at
                            // col 7 in place, so the cells at and right of
                            // the caret are the typed word's own two.
                            assert_eq!(
                                rib.retire_cells(&ids, at(t0, ms)),
                                2,
                                "{leg}: the shifted cells retire"
                            );
                            retired = true;
                        }
                        8
                    },
                );
                assert_block_on_band(&frames, x0, cw, 9 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= 8),
                    "{leg}: the retired cells left the pool inside the window"
                );
            }
            // Leg 4: a neighbour cohort's cell under the caret: a three-key
            // word, an arrow hop right over empty cells (a wake of its own
            // cohort, 5..=9), the hop back onto the word's end, and the wake
            // retired by content under the caret.
            {
                let leg = format!("{hand} hand, a wake's cell retired under the caret");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 3,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                // The hop's wake lives `WAKE_LIFE_S · WAKE_HOP_LIFE_SHARE`
                // from the hop and is born 100 ms after it: back and
                // retired inside that.
                let mut ms = 2 * 83 + 200;
                let cx = ctx_in(at(t0, ms), &c, (row, 9), disp, g);
                rib.on_event(&nav((row, 5), (row, 9)), at(t0, ms), &cx);
                rib.plan(&cx);
                ms += 120;
                let cx = ctx_in(at(t0, ms), &c, (row, 5), disp, g);
                rib.on_event(&nav((row, 9), (row, 5)), at(t0, ms), &cx);
                rib.plan(&cx);
                let typed_cohort = rib
                    .cells()
                    .iter()
                    .find(|l| l.row == row && l.col == 4)
                    .expect("the word's last cell")
                    .cohort;
                let ids: Vec<(u16, u16, Instant)> = rib
                    .cells()
                    .iter()
                    .filter(|l| l.row == row && l.col >= 5)
                    .map(|l| (l.row, l.col, l.born))
                    .collect();
                assert!(
                    rib.cells()
                        .iter()
                        .any(|l| l.row == row && l.col == 5 && l.cohort != typed_cohort),
                    "{leg}: the wake is a cohort of its own"
                );
                let ms0 = ms + 80;
                let mut retired = false;
                // The span is the head, the caret cell and the wake's next
                // cell. The wake's far end is excluded: when its landing cell
                // starts leaving, `head_col` resolves the run's newest cell
                // instead and the tail feather moves to the other end.
                let x0 = 4 * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=ms0 + melt_ms + 200,
                    x0..7 * cw,
                    |rib, ms| {
                        if !retired && ms >= ms0 {
                            assert!(
                                rib.retire_cells(&ids, at(t0, ms)) >= 5,
                                "{leg}: the wake retires"
                            );
                            retired = true;
                        }
                        5
                    },
                );
                assert_block_on_band(&frames, x0, cw, 6 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= 5),
                    "{leg}: the retired wake left the pool inside the window"
                );
            }
        }
    }

    /// The peak level per device column over `xs` inside the reference row's
    /// window, `0` where nothing is lit.
    fn column_peaks(under: &[GlowQuad], xs: std::ops::Range<i32>) -> Vec<i32> {
        column_census(under, xs, owner_rows(), 1)
            .iter()
            .map(|e| e.map_or(0, |(_, _, p)| i32::from(p)))
            .collect()
    }

    /// Columns of `peaks` (offsets into it) darker than both sides by more
    /// than `tol` levels — the brightest column within a cell to the left
    /// and within a cell to the right — past the first `skip` columns (the
    /// run's oldest cell, whose feather is designed).
    fn sliver_columns(peaks: &[i32], cw: usize, skip: usize, tol: i32) -> Vec<usize> {
        (skip..peaks.len())
            .filter(|&i| {
                let left = peaks[i.saturating_sub(cw)..i]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                let right = peaks[i + 1..(i + 1 + cw).min(peaks.len())]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                peaks[i] + tol < left && peaks[i] + tol < right
            })
            .collect()
    }

    /// A word and an arrow hop's wake beside it (two cohorts: `heal_seams` folds typed cohorts
    /// only) print one band: no column darker than both sides by more than a dither step, and
    /// the top edge steps at most one device row between columns past the run's oldest cell.
    #[test]
    fn a_boundary_two_live_runs_share_carries_the_older_side_s_light() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: OWNER_ROW,
                col0: 2,
                n: 6,
                period_ms: 83,
                disp: 0.7,
            },
            &c,
        );
        // The hop: → three times, coalesced — a wake on 8, 9, 10 of its own.
        let hop = at(t0, 5 * 83 + 120);
        let cx = ctx_in(hop, &c, (OWNER_ROW, 11), 0.7, g);
        rib.on_event(&nav((OWNER_ROW, 8), (OWNER_ROW, 11)), hop, &cx);
        rib.plan(&cx);
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(hop, 300), 11, 0.7);
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == OWNER_ROW).count(),
            2,
            "the word and its wake are two cohorts: {:?}",
            rib.cohorts()
        );
        assert_eq!(rib.runs.len(), 2, "…and two runs abutting at column 8");
        let x0 = 2 * cw;
        let x1 = 12 * cw;
        let peaks = column_peaks(&sink.under, x0..x1);
        let dark: Vec<i32> = peaks
            .iter()
            .enumerate()
            .filter(|(_, p)| **p == 0)
            .map(|(i, _)| x0 + i as i32)
            .collect();
        assert!(dark.is_empty(), "dark columns inside the band: {dark:?}");
        let slivers: Vec<i32> = sliver_columns(&peaks, cw as usize, cw as usize, 2)
            .into_iter()
            .map(|i| x0 + i as i32)
            .collect();
        assert!(
            slivers.is_empty(),
            "columns darker than both sides by more than a dither step: {slivers:?} (peaks {:?})",
            &peaks[5 * cw as usize..7 * cw as usize]
        );
        let tops = column_census(&sink.under, x0 + cw..x1, owner_rows(), 8);
        for w in tops.windows(2) {
            let (Some(a), Some(b)) = (w[0], w[1]) else {
                continue;
            };
            assert!(
                (a.0 - b.0).abs() <= 1,
                "the top edge steps {} rows between adjacent columns ({} → {})",
                (a.0 - b.0).abs(),
                a.0,
                b.0
            );
        }
    }

    /// The composer re-anchor: a licensed park and return on one tick drains the old cells
    /// while the hand types on. Read every 16 ms through the grace, no column's peak falls
    /// more than the fade's steepest frame (36 levels: `spend` is `(1 − u)²` over 0.24 s), and
    /// on every settled frame no live column is a sliver.
    #[test]
    fn the_feather_grows_in_as_a_drained_neighbour_goes_never_in_one_frame() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let first = "and I also ";
        let mut ms = 0u64;
        let mut col = 2u16;
        for _ in first.chars() {
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            ms += 83;
        }
        // The park and its return, one tick.
        let park = at(t0, ms + 20);
        let cx = ctx_in(park, &c, (OWNER_ROW, col), 0.7, g);
        rib.on_event(&typed_move((OWNER_ROW, col), (OWNER_ROW, 2)), park, &cx);
        rib.on_event(&typed_move((OWNER_ROW, 2), (OWNER_ROW, col)), park, &cx);
        rib.plan(&cx);
        let mut keys: Vec<(u64, u16)> = Vec::new();
        let mut k = ms + 70;
        for _ in "seee an ugly gap".chars() {
            keys.push((k, col + 1));
            col += 1;
            k += 83;
        }
        let last_key = keys.last().map_or(k, |e| e.0);
        let mut sink = Sink::default();
        let x0 = 2 * cw;
        let x1 = 30 * cw;
        let mut prev: Option<Vec<i32>> = None;
        let mut next_key = 0usize;
        let mut caret = col - 16;
        let mut worst_drop = (0i32, 0i32, 0u64);
        let mut slivers_seen = Vec::new();
        let mut t = ms + 20;
        let mut last_key_at = 0u64;
        // Through the hand's grace and no further: past it the retract pulls the edge into
        // the caret and a passed column goes dark in one frame by design (`retract_x`). At
        // a steady 83 ms the grace (`Ribbon::rest_s`) is `PHRASE_REST_MIN_S`.
        let until = last_key + (PHRASE_REST_MIN_S * 1000.0) as u64;
        while t <= until {
            while next_key < keys.len() && keys[next_key].0 <= t {
                let (kt, kc) = keys[next_key];
                let now = at(t0, kt);
                let cx = ctx_in(now, &c, (OWNER_ROW, kc), 0.7, g);
                rib.on_event(&typed(), now, &cx);
                caret = kc;
                last_key_at = kt;
                next_key += 1;
            }
            owner_frame(&mut rib, &mut sink, &c, at(t0, t), caret, 0.7);
            let peaks = column_peaks(&sink.under, x0..x1);
            if let Some(p) = &prev {
                for (i, (&a, &b)) in p.iter().zip(&peaks).enumerate() {
                    if a - b > worst_drop.0 {
                        worst_drop = (a - b, x0 + i as i32, t);
                    }
                }
            }
            let settled = t.saturating_sub(last_key_at) >= 60;
            if settled {
                // The live band: the cells not leaving. The drained cells left of it go
                // out farthest from the park first (a valley there is the drain, not a
                // seam), and the live band's first cell may feather.
                let live_col = rib
                    .cells()
                    .iter()
                    .filter(|l| !l.leaving())
                    .map(|l| l.col)
                    .min()
                    .unwrap_or(2);
                let first = (usize::from(live_col) - 2) * cw as usize;
                let live = &peaks[first.min(peaks.len())..];
                for i in sliver_columns(live, cw as usize, cw as usize, 2) {
                    slivers_seen.push((t, x0 + (first + i) as i32, live[i]));
                }
            }
            prev = Some(peaks);
            t += 16;
        }
        assert!(
            rib.cells().iter().all(|l| l.col >= 13),
            "by the end of the grace the drained cells have retired"
        );
        assert!(
            worst_drop.0 <= 36,
            "a column's light fell {} levels in one 16 ms frame (x {}, +{} ms): the feather popped in",
            worst_drop.0,
            worst_drop.1,
            worst_drop.2
        );
        assert!(
            slivers_seen.is_empty(),
            "columns darker than both sides inside the live band: {:?}",
            &slivers_seen[..slivers_seen.len().min(8)]
        );
    }

    /// At one slab per cell (three hundred lit cells above the composer line, the frame at the
    /// budget's edge) the attach and the seam law still hold: the block stands on the band, no
    /// sliver, and the top edge steps at most the comet taper's per-cell lerp (three device rows).
    #[test]
    fn the_attach_and_the_seam_law_hold_at_one_slab_per_cell() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut ms = 0u64;
        for row in 50..53u16 {
            type_keys(
                &mut rib,
                at(t0, ms),
                Keys {
                    g,
                    row,
                    col0: 1,
                    n: 100,
                    period_ms: 1,
                    disp: 0.7,
                },
                &c,
            );
            ms += 100;
        }
        let mut col = 2u16;
        for _ in "and I also seee an ugly gap".chars() {
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            ms += 20;
        }
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(t0, ms - 20 + 100), col, 0.7);
        assert_eq!(rib.slabs_per_cell(), 1, "327 retina cells plan at one slab");
        assert!(
            sink.under.len() + 2 > RIBBON_QUAD_BUDGET - 200,
            "the frame is at the budget's edge: {} quads",
            sink.under.len()
        );
        let head_x0 = i32::from(col - 1) * cw;
        let block_x1 = i32::from(col + 1) * cw;
        let cens = column_census(&sink.under, head_x0 - 3 * cw..block_x1, owner_rows(), 8);
        let plateau = cens[..3 * cw as usize]
            .iter()
            .filter_map(|e| e.map(|(t, _, _)| t))
            .min()
            .expect("lit");
        for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
            let x = head_x0 - 3 * cw + k as i32;
            let (top, _, _) = e.unwrap_or_else(|| panic!("column {x} is ground-dark at one slab"));
            assert!(
                top <= plateau + 1,
                "column {x} tops out at {top} under the plateau {plateau}"
            );
        }
        let x0 = 2 * cw;
        let peaks = column_peaks(&sink.under, x0..block_x1);
        let slivers = sliver_columns(&peaks, cw as usize, cw as usize, 2);
        assert!(slivers.is_empty(), "slivers at one slab: {slivers:?}");
        let tops = column_census(&sink.under, x0 + cw..block_x1, owner_rows(), 8);
        let worst = tops
            .windows(2)
            .filter_map(|w| Some((w[0]?.0 - w[1]?.0).abs()))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 3,
            "the top edge steps {worst} rows at one slab per cell"
        );
    }

    /// A wide window at a retina cell size, with one planned run that exceeds
    /// the real `ribbon_beam` ceiling. This isolates emission order from the
    /// content and motion laws that built the plan; their `head` and
    /// `stream_dir` values are pinned by the move tests above.
    fn capped_wide_run(head_col: u16, head: usize, stream_dir: i8) -> (Vec<GlowQuad>, u16) {
        let c = cfg_owner();
        let mut g = geom_owner();
        g.cols = 180;
        g.win_w = 5400;
        let now = Instant::now();
        let cx = ctx_in(now, &c, (OWNER_ROW, head_col), 0.7, g);
        let mut rib = Ribbon::new();
        rib.plan(&cx); // Sync the real theme ink table.
        rib.slabs = 1;
        rib.plan = (0..=179)
            .map(|col| Segment {
                x: col as f32 * g.cw as f32,
                spine: (f32::from(OWNER_ROW) + 1.0) * g.ch as f32,
                up: g.ch as f32,
                dn: 0.30 * g.ch as f32,
                t: 0.4,
                cov: 220,
            })
            .collect();
        rib.runs.push(Run {
            row: OWNER_ROW,
            col0: 0,
            col1: 178,
            lo: 0,
            hi: rib.plan.len(),
            head_col,
            head,
            wet: true,
            at_caret: true,
            born: now,
            stream_dir,
        });

        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(
            sink.under.len() + 2 >= RIBBON_QUAD_BUDGET,
            "precondition: the real quad ceiling was reached ({})",
            sink.under.len()
        );
        (sink.under, g.cw as u16)
    }

    /// Two adjacent cohorts can remain separate while the caret edits a
    /// settled row. Their body is one continuous trail even when the quad
    /// budget is tight: the vivid rail is an accent and must not consume the
    /// older cohort's body budget.
    #[test]
    fn a_capped_rail_does_not_cut_the_body_of_an_adjacent_run() {
        let c = cfg_owner();
        let g = geom_owner();
        let now = Instant::now();
        let cx = ctx_in(now, &c, (OWNER_ROW, 130), 0.7, g);
        let mut rib = Ribbon::new();
        rib.plan(&cx); // Sync the real theme ink table.
        rib.slabs = 1;
        rib.plan = (0..=130)
            .map(|col| Segment {
                x: col as f32 * g.cw as f32,
                spine: (f32::from(OWNER_ROW) + 1.0) * g.ch as f32,
                up: g.ch as f32,
                dn: 0.30 * g.ch as f32,
                t: 0.4,
                cov: 220,
            })
            .collect();
        rib.runs.extend([
            Run {
                row: OWNER_ROW,
                col0: 20,
                col1: 129,
                lo: 20,
                hi: 131,
                head_col: 129,
                head: 130,
                wet: true,
                at_caret: true,
                born: now,
                stream_dir: -1,
            },
            Run {
                row: OWNER_ROW,
                col0: 0,
                col1: 19,
                lo: 0,
                hi: 21,
                head_col: 19,
                head: 20,
                wet: true,
                at_caret: false,
                born: now,
                stream_dir: -1,
            },
        ]);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(
            sink.under.len() + 2 >= RIBBON_QUAD_BUDGET,
            "precondition: body plus rail must fill the budget ({})",
            sink.under.len()
        );
        assert!(sink.under.len() <= RIBBON_QUAD_BUDGET);
        assert!(cell_lit(&sink.under, g, OWNER_ROW, 129));
        assert!(
            (0..130).all(|col| cell_lit(&sink.under, g, OWNER_ROW, col)),
            "the vivid rail consumed body quads and opened a dark gap inside the typed row"
        );

        // Below the cap the same two-run layout must emit the exact same
        // byte-ordered quads as its runs emitted one at a time, each in
        // body/rail order.
        let mut uncapped = rib.clone();
        uncapped.plan.truncate(61);
        uncapped.runs[0].col1 = 59;
        uncapped.runs[0].hi = 61;
        uncapped.runs[0].head_col = 59;
        uncapped.runs[0].head = 60;
        let short_cx = ctx_in(now, &c, (OWNER_ROW, 60), 0.7, g);
        let mut joint = Sink::default();
        uncapped.emit(&short_cx, &mut joint.frame());
        let mut current = uncapped.clone();
        current.runs.truncate(1);
        let mut current_sink = Sink::default();
        current.emit(&short_cx, &mut current_sink.frame());
        let mut older = uncapped;
        older.runs.remove(0);
        let mut older_sink = Sink::default();
        older.emit(&short_cx, &mut older_sink.frame());
        let seam_x = (20 * g.cw) as u16;
        assert!(current_sink.under.iter().all(|q| q.x >= seam_x));
        assert!(older_sink.under.iter().all(|q| q.x + q.w <= seam_x));
        let mut per_run = current_sink.under;
        per_run.extend(older_sink.under);
        assert_eq!(joint.under, per_run, "uncapped emission changed");
    }

    /// Saturation sheds the distant tail, never the light at the hand. A
    /// leftward wake starts at its landing (repeated leftward navigations can
    /// extend one wake cohort past the per-navigation 32-cell cap), and a
    /// backward edit places the typed run's head inside the row; a cap that
    /// spent quads high-X first would leave a hole beside the typing caret.
    #[test]
    fn a_capped_run_keeps_the_light_at_its_head_and_sheds_the_distant_tail() {
        // `(what, head_col, head, stream_dir)`.
        for (what, head_col, head, stream_dir) in [
            ("a leftward wake", 0u16, 0usize, 1i8),
            ("a midline edit", 30, 31, -1),
        ] {
            let (quads, cw) = capped_wide_run(head_col, head, stream_dir);
            assert!(
                quads
                    .iter()
                    .any(|q| q.x >= head_col * cw && q.x < (head_col + 1) * cw),
                "{what}: the cell at the caret went dark under the cap"
            );
            assert!(
                quads.iter().any(|q| q.x < cw),
                "{what}: the run's left side must connect to the head"
            );
            assert!(
                !quads.iter().any(|q| q.x >= 178 * cw),
                "{what}: the distant right tail should pay the budget, not the head"
            );
        }
    }

    #[test]
    fn attach_cell_follows_scroll_band_and_hand_lift() {
        let now = Instant::now();
        let c = cfg(true, true);
        let g = geom();
        let mut rib = Ribbon::new();

        // Lay a cell at (5, 4) with caret at (5, 5) and plan so `attach` is latched at (5, 5).
        let cx5 = ctx_in(now, &c, (5, 5), 0.7, g);
        rib.on_event(&typed(), now, &cx5);
        rib.plan(&cx5);
        assert_eq!(rib.attach, Some((5, 5)));

        rib.translate_scroll(2);
        assert_eq!(rib.attach, Some((3, 5)));
        assert!(rib.attach_floor.is_none());

        rib.translate_band(2, 6, 1);
        assert_eq!(rib.attach, Some((4, 5)));

        rib.lift_hand(4, 3);
        assert_eq!(rib.attach, Some((3, 5)));
        rib.carry = Some((
            SoftWrap {
                origin: (3, 79),
                landing: (4, 2),
                word_col0: 74,
            },
            now,
        ));
        rib.lift_hand(4, 5);
        assert_eq!(
            rib.carry.map(|(sw, _)| (sw.origin, sw.landing)),
            Some(((3, 79), (5, 2)))
        );

        // Typing at the lifted caret (3, 6) lays at (3, 5) and inherits the spent attack from the translated attach.
        let t_next = now + Duration::from_millis(20);
        let cx3 = ctx_in(t_next, &c, (3, 6), 0.7, g);
        rib.on_event(&typed(), t_next, &cx3);
        let laid = rib
            .cells()
            .iter()
            .find(|cell| cell.row == 3 && cell.col == 5)
            .expect("cell laid at lifted caret");
        assert!(
            laid.attack_at < laid.born,
            "typed cell at translated attach must inherit spent attack"
        );
    }
}
