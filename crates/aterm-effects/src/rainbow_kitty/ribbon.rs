// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ribbon: the laid rainbow under typed text, plus the per-row field index
//! every other rainbow-kitty producer reads.
//!
//! Design: `docs/design/RAINBOW-TRAIL-ONE-STORY.md` §3 (geometry and colour
//! law) and `docs/design/RAINBOW-KITTY-V2.md` §4 (the ribbon), §18 (field
//! index, quad budget) and §29 onward (gap, scrub, torn-read and walk laws).
//!
//! ## Body and light
//!
//! * The tall body (`TALL_UP_CH` above a spine at the row bottom) is one
//!   `aterm_render::ribbon_beam` call with `ribbon_profile`, a 0.75-cycle wave
//!   ([`WAVE_AMP_CELLS`]), an 18 ms edge-in, the expiry melt, the staggered
//!   backspace retract, the wrap fold and the exit swoosh ([`SWOOSH_TOTAL_S`]);
//!   light themes get a source-over twin.
//! * `t` is the laid field, a function of position ([`walk_t`]): `d/16` for
//!   the first [`WALK_FAST_CELLS`] cells, then [`WALK_LAY_RATE`] per cell. A
//!   continuation (fold, composer relay, rebirth) carries the walk's distance
//!   ([`Cohort::d0`]); a new line continues the colour at distance zero
//!   ([`Ribbon::fresh_line`]).
//! * The bed ([`bed_ink`], [`onto_luma`]) puts every stop on one composited
//!   luminance at the 5.25:1 text-contrast bar, spending chroma before white.
//!   The ribbon applies one ceiling ([`UNDER_COV_CAP`], the strip inside
//!   [`BODY_FRAME_TOP`]), and it is the only one: no host pass clips further.
//! * A cell's light is priced once, at birth ([`Cell::cov0`] from
//!   [`Cell::birth_disp`]); the live spine moves only the body's shape.
//! * The hot edge ([`hot_edge_ink`], gated by [`Ribbon::hot_edge_gain`]) is a
//!   1-px additive hairline along the body's top edge from the head back
//!   [`HOT_EDGE_CELLS`], priced at the head's birth. Dark themes only.
//! * The comet body ([`Ribbon::comet`], off under [`Config::ribbon_flat`]) is
//!   fattest under the caret; the vivid rail ([`rail_ink`]) runs below the row
//!   bottom in the full-value spectrum; a new cell enters with a 40 ms wipe
//!   from the caret side ([`Ribbon::wipe_of`]).
//!
//! ## Lifetime and ownership
//!
//! * Cells of a cohort share its clock ([`live_since`]): each typing key
//!   refreshes `alive_at` on the un-abandoned cohorts of its row
//!   ([`Ribbon::place`]), so a mark ages only once the hand stops or leaves the
//!   row ([`Ribbon::leave_row`]); the swoosh then retracts it toward the caret,
//!   or a fold flows it into its kept end (`Ribbon::flow_row`)
//!   ([`Ribbon::advance_swoosh`], [`Ribbon::retract_x`]).
//! * Abutting typed cohorts on a row are folded into one each frame
//!   ([`Ribbon::heal_seams`]); the brighter side owns a boundary.
//! * A same-row keyed move inside the hand's lit run is a scrub
//!   ([`Ribbon::scrub`]): it lays nothing and holds the band. A move that leaves
//!   the run is a jump: it lays a wake corridor of at most [`WAKE_MAX_CELLS`]
//!   new light and abandons the band ([`Cohort::leaving_at`]).
//! * The hand coming back takes its band back ([`Ribbon::join_cohort`]).
//! * Composer reflows keep their light ([`Relocate`], [`Ribbon::re_anchor`],
//!   [`Ribbon::moved_word_len`], [`SoftWrap`], `Ribbon::settle_carry`,
//!   [`Lift`], [`Ribbon::lift_word`], [`Ribbon::seat_fold_successor`]).
//! * A torn read's partial release is held per part ([`Cell::released_at`],
//!   [`RELEASE_PARTS`]) and restored when its glyphs come back.
//!
//! ## The bar (`RAINBOW-KITTY-V2.md` §19.1, §23, §24)
//!
//! * The bed sits AT the 5.25:1 text-contrast bar, not under it. The bar is
//!   a standing ruling (2026-09-06: "THE BAR STANDS, over every cell") and
//!   the one restraint kept ("text under the bed stays legible"), so the
//!   guard is only the composite's rounding ([`BODY_CONTRAST_GUARD`]), the
//!   solver answers from under its target ([`solve_for_luma`]) and the luma
//!   clamp is the bar's own answer at a white foreground ([`BED_LUMA_MAX`]).
//!   Relaxing the bar over BLANK cells is rejected: the ribbon lies under
//!   text that arrives cell by cell, so a glyph-dependent bed would dim each
//!   cell as its letter lands.
//! * At one luminance a warm stop IS an olive: yellow composites at about
//!   `(80, 80, 3)` on the default theme, already at full chroma. Brighter
//!   warm ink rides the rail below the row bottom ([`RAIL_LUMA_CEIL`],
//!   `RAINBOW-PATH-V3.md` D-1 option B), never a moved bar. Pinned by
//!   `the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar`
//!   and `the_bed_sits_at_the_bar_not_under_it_on_the_default_dark_theme`.
//! * Chroma is spent before white ([`onto_luma`]): a dark stop climbs its own
//!   hue to full value and only then toward white. The arc is S 1.00 end to
//!   end, so the green→blue crossing carries its neighbours' chroma;
//!   [`BED_SAT_FLOOR`] is the identity on the arc and guards a caller's
//!   off-arc colour. The "black gaps"
//!   seen on glass were lost chroma at the bar's luminance, not holes. There
//!   is no floor against the GROUND: wherever it exceeds the bar it costs the
//!   text its contrast, so a dark-foreground theme's dim bed (Solarized Dark)
//!   is the documented exception. Pinned by
//!   `the_bed_s_dark_stops_arrive_at_full_chroma_not_walked_grey`,
//!   `the_crossing_composites_no_greyer_than_its_flanks`,
//!   `the_ground_never_lifts_the_bed_over_the_bar` and
//!   `the_visibility_floor_costs_the_bar_nothing_on_nord`.
//! * §3.4: requests are clipped by the ledger, never by ad-hoc caps. This
//!   module applies one ceiling ([`UNDER_COV_CAP`], the strip inside
//!   [`BODY_FRAME_TOP`]); v1's 33-entry per-position coverage table is not
//!   carried ("two ceilings for one bed"), and [`bed_ink`] equalizes the arc
//!   in the coordinate the bar is written in.
//!
//! ## Light is priced once; a cohort shares one clock
//!
//! * The live spine moves the body's SHAPE (wave, wedge), never its LIGHT:
//!   coverage and hot-edge gain come from the head cell's
//!   [`Cell::birth_disp`] and close under one envelope ([`Ribbon::env_of`]),
//!   so nothing brightens after the last key. Pinned by
//!   `the_body_never_brightens_after_the_last_keystroke`,
//!   `the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero` and
//!   `the_hot_edge_embers_out_with_its_body_on_focus_loss`.
//! * A cell's expiry runs from `max(cell.born, cohort.alive_at)`
//!   ([`live_since`]), so a line still being typed never darkens from its
//!   oldest or slowest word (§23's addendum). Pinned by
//!   `a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret`
//!   and, at the seam, `a_line_still_being_typed_has_no_dark_cell_inside_its_live_span`.
//!
//! ## Ownership and the frame path
//!
//! [`Ribbon`]'s pool, plan and index are private behind read-only accessors,
//! so nothing mutates the pool behind the index the other producers read.
//! The resident scratch (`runs`, `sorted`, `verts`, `ink`, `ember_at`,
//! `rearm`, `released_cohorts`) is private and the index keeps dark lanes in
//! a spare pool, so the frame path allocates nothing (§18), release and the
//! Enter that lights a dark row included. [`Ribbon::slabs_per_cell`] is per
//! frame because `ribbon_beam` tiles at least one slab per vertex.
//! [`Ribbon::head_rgb`] keeps v1's contract: the authored stop on dark, the
//! rail's ink on light.
//!
//! ## Gaps (§29)
//!
//! * No two typed cohorts abut on a row ([`Ribbon::heal_seams`]): two keys
//!   echoed on one tick would otherwise split the run and feather a dark
//!   ramp at [`RUN_TAIL_EASE`].
//! * The brighter side owns a boundary (`plan_run`); a midpoint rule sagged
//!   a head whose neighbour was erased.
//! * Re-wetting is monotone ([`Ribbon::place`]): a live cell laid again
//!   keeps its attack age (with a fresh identity for the content witness);
//!   a leaving or expired one takes a fresh attack.
//! * One finger, one row (`place`, [`Ribbon::leave_row`]): no key on the new
//!   row holds the row the hand left. A typed fold one row down flows that row
//!   (`Ribbon::flow_row`, `Cohort::flow`): from its own last key it drifts as
//!   one rigid band into its kept fold end (`Ribbon::flow_target`;
//!   [`Ribbon::retract_x`] clamps it at that cell's outer edge), fades far end
//!   first and is dark by [`FLOW_TOTAL_S`]; a cohort already past its grace
//!   keeps its swoosh. Any other row change abandons the row toward the column
//!   the hand left (`Ribbon::abandon_row`, `retract_col = from.1`), and the
//!   retract sucks in to that column's left edge. Pinned by
//!   `the_row_the_hand_folded_off_drifts_into_the_fold_and_fades_as_one_band`.
//! * The editing hand holds its phrase ([`Ribbon::hold_phrase`], v3 §2.3);
//!   an erase whose echo lands a tick late still retracts from the landing
//!   (`pending_erase`).
//! * A composer's re-anchor relays the moved word where the caret reveals it
//!   ([`Relocate`], [`Ribbon::re_anchor`]).
//! * A program's caret round trip abandons the band toward the HAND's
//!   column, and the hand's next key takes it back ([`Ribbon::join_cohort`]).
//!   A rebirth after a finished swoosh continues the walk (`last_walk`),
//!   colour and pace, unless a new line started in between.
//!
//! ## The scrub (§31)
//!
//! A same-row keyed move whose corridor is lit end to end by a band that is
//! not leaving is a scrub ([`Ribbon::scrub`]): it lays nothing, abandons
//! nothing, and renews the crossed cohorts' clock as an erase does. Anything
//! else (past the band's oldest cell, onto dark ground, over light already
//! melting, to another row) is a jump and takes v3's overlay wake (§2.7); a
//! move inside the run has no dark ground to light, so the two never
//! disagree. The hold reaches only the cohorts that own the corridor's
//! columns. Measured in `docs/measured/scrub-gaps-2026-09-14.md` and
//! `tests/scrub_gaps.rs`.
//!
//! ## The comet, the rail and the attack (§30)
//!
//! * The comet body ([`Ribbon::sample`], [`COMET_DN_HAND_CH`],
//!   [`COMET_UP_TAIL_CH`], [`COMET_TAPER_CELLS`]) is fattest at the hand:
//!   the reach below the spine opens to 0.55 ch under the caret and settles
//!   to [`DN_FLOOR_CH`]; the reach above tapers 1.10 → 0.80 ch over twelve
//!   cells. Shape, not light.
//! * The vivid rail ([`Ribbon::emit`], [`rail_ink`], [`RAIL_LUMA_FLOOR`],
//!   [`RAIL_GAIN`]) is a second `ribbon_beam` below the row bottom in the
//!   full-value spectrum, Over-composited on the bed at the body's coverage;
//!   every device row a glyph can touch stays at the bar. Dark themes only.
//!   The strip ([`STRIP_LIFT_GAIN`]) stays as the bed-ink accent above it.
//! * A typed cell enters as a 40 ms wipe from the caret side
//!   ([`Ribbon::wipe_of`], [`ATTACK_WIPE_S`], [`ATTACK_WIPE_FRONT`]) on top
//!   of the 18 ms edge-in; [`BIRTH_EDGE_FLOOR`] is never gated, so T2 holds
//!   on the echo frame. Off under reduced motion.
//! * [`Config::ribbon_flat`] turns all three off ([`Ribbon::comet`]); the
//!   flat body is the A/B control, pinned by the deletion goldens. The
//!   rail's rows enter [`Ribbon::slabs_for`]'s estimate under
//!   [`RIBBON_QUAD_BUDGET`].
//!
//! ## Torn reads (`tests/torn_read_gaps.rs`)
//!
//! A composer redraw over 1 KiB arrives in 1024-byte PTY reads, and a
//! present between two shows the row cleared only up to the boundary. That
//! shape is driven at the host seam, not caught in the wild (the witness
//! module doc says what was witnessed). The content witness releases the
//! missing tail; the ribbon holds its custody:
//!
//! * A restore counts only what was released, by the release's own stamp
//!   ([`Cell::released_at`]), never by birth time: the held key's cell is
//!   laid dated at its press, before the torn present. A released cell a
//!   newer writer replaced is accounted for (`Ribbon::custody_replaced`); a
//!   pruned one refuses.
//! * Each part is in its own custody (`Cohort::release_parts`,
//!   [`RELEASE_PARTS`]), lifted when ITS glyphs are back, held on an
//!   abandoned cohort too, and carried through the wake and seam folds
//!   (`Ribbon::carry_release`).
//! * A part's revocation waits for the frame's read
//!   (`Ribbon::settle_revoked_parts`); the whole release's clock is revoked
//!   at once (`aterm-spec`'s `ribbon_release_restoration_model`, `Abandon`).
//!
//! ## A move over a fading band (`tests/jump_over_fading_band.rs`)
//!
//! * The wake cap binds at the corridor's far edge, never inside it (v3
//!   §2.10, K2): light the corridor keeps past the cap, covered or re-owned,
//!   moves that end out, and the drained columns between are laid. Reach and
//!   lay read the glass census's bar (`Ribbon::corridor_keeps_glass_light`,
//!   [`STATUS_LIT_COV`]), so an invisible residue never stretches a corridor
//!   across the row.
//! * A hop covers light that is leaving ([`Cohort::leaving_at`]); the scrub
//!   and the hop read one predicate. A visibility floor on that guard only
//!   moves the instant the corridor splits, so there is none.
//! * A hop leaves LIVE light as it is; a cold jump over dark ground lays the
//!   cap, never the row; no wake touches a `Base` cell or its clock (K1).
//!
//! ## Licensed inserts (2026-09-22, `tests/licensed_insert_band.rs`)
//!
//! A Tab completion or paste whose echo reaches the glass torn before its
//! glyphs (the read ends at the hide; the next present shows the caret
//! moved with the print) was refused by the hide bridge, and the licence
//! lapsed unspent. It is now bridged on the insert's own reach
//! (`CursorGlow::hidden_bridge_source`, judged by
//! `CursorGlow::visible_insert_echo`, the one predicate the insert arm lays
//! by) and joins the cohort as one sweep ([`Licence::Insert`]). Unlicensed
//! program output at the caret lays nothing, and the next key past it mints
//! a second cohort with the gap between: T1 and v3's X1 stay strict (v3 §8
//! D-12). Breaking the phrase on a gap in space (S7) closed that gap
//! 0.8–1.1 s after the key but took the hand's own line dark mid-typing.
//!
//! ## Composer wraps (`tests/claude_wrap_band.rs`, `tests/wrap_code_reflow.rs`)
//!
//! * A key that fills the row's last column is laid where its glyph is
//!   while the caret alone wraps (`CursorGlow::soft_wrapped_caret`,
//!   [`SoftWrap`]); the word comes down with its text once the seam reads
//!   its columns blank (`Ribbon::settle_carry`, [`Ribbon::reflow_move`]).
//! * A key that pushes the word at the caret down is judged at once, not
//!   parked (`CursorGlow::key_pushed_text_down`).
//! * The moved word is measured from where the hand's run began: the later
//!   of its last typed Space and its last deliberate arrival
//!   ([`Ribbon::arrived`], [`Ribbon::moved_word_len`]), reaching back over
//!   contiguous live typed cells so an arrow inside the word starts no run.
//! * A word lifted back to the row above goes up with its light ([`Lift`],
//!   [`Ribbon::lift_word`]).
//! * Every other re-wrap keeps the `landing − 1` law. A Space typed at the
//!   last text column (no glyph to read) stays on it, unmeasured, and the
//!   trail ring logs a reflow key `no-credits`: the seam's true verdict on
//!   its presses, not a fault.
//!
//! ## The walk's odometer (§35)
//!
//! * A continuation (fold, composer relay, rebirth) carries the walk's
//!   distance ([`Cohort::d0`]), so a wrapped line walks as the same text
//!   unwrapped; a program's jump or a rest is not a new line.
//! * A Return, a line kill, a curtain or a navigated row change restarts the
//!   pace at distance zero and continues the colour ([`Ribbon::fresh_line`]);
//!   a wake on a row with nothing fresh does the same
//!   ([`Ribbon::wake_origin`]). The composer's relay is never gated.
//! * The fold's pending-wrap glyph, laid after the next key's first cell,
//!   re-seats the row below ([`Ribbon::seat_fold_successor`]).

use std::mem;

use aterm_time::Instant;

use aterm_render::{
    BeamClip, BeamVertex, GlowBlend, GlowQuad, RibbonVertex, comet_beam, over_premul, premul_rgb,
    ribbon_beam,
};

use crate::color_math::relative_luminance;
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

/// The furthest back, in cells, the crisp edge may reach when a key is born on
/// a change of speed or under an open theme (`edge_cells = 3 + 4*delta/0.5`,
/// capped here). The same alpha law over a longer `d`, so the edge reads as a
/// longer speed streak, never a brighter one: [`HOT_EDGE_ALPHA`]'s falloff
/// still reaches exact zero at the reach and [`HOT_EDGE_COV_MAX`] is
/// untouched.
pub const HOT_EDGE_CELLS_MAX: f32 = 5.0;

/// The hot edge's falloff shape, normalised at the head cell:
/// `alpha(d) = 0.38·(1 − d/3)²`, so the request is
/// `HOT_EDGE_COV_MAX · (1 − d/3)²` with `d` in cells behind the head.
pub const HOT_EDGE_ALPHA: f32 = 0.38;

/// The hot edge's coverage request ceiling: the transient cap
/// (`RAINBOW_TRANSIENT_COV_CAP`, shared by every transient at the hand). The
/// ledger holds it to the ink lift over the row above's glyph cells. At this
/// cap every stop reads as colour, 2.3x (blue) to 7x (yellow) the bed's
/// luminance.
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

/// The baseline strip's gain, as a share of the cell's ink coverage: the crisp
/// accent under the letters, riding `aterm_render::ribbon_lift_profile` over
/// [`LEAD_CH`]. [`Ribbon::plan`] clamps it to `255 - cov`, so the strip stays
/// an accent on the body and never the only bright part of the mark.
const STRIP_LIFT_GAIN: f32 = 0.35;

// ===========================================================================
// The comet body, the vivid rail and the from-the-hand attack
// (RAINBOW-KITTY-V2.md §30). `Config::ribbon_flat` turns all three off.
// ===========================================================================
//
// * The comet body is fattest at the hand: the reach below the spine opens to
//   [`COMET_DN_HAND_CH`] under the caret and settles to [`DN_FLOOR_CH`] over
//   [`BLOOM_REACH_CELLS`]; the reach above tapers from [`TALL_UP_CH`] to
//   [`COMET_UP_TAIL_CH`] over [`COMET_TAPER_CELLS`].
// * The vivid rail: under the 5.25:1 bar yellow composites as olive, so
//   full-value colour can only live below the row bottom, where there is no
//   ink. The rail is a second `ribbon_beam` polyline there in the full-value
//   spectrum ([`rail_ink`]) at the body's coverage ([`RAIL_GAIN`]). Dark themes
//   only.
// * The attack: a new cell's light enters as a wipe from the caret side
//   ([`ATTACK_WIPE_S`], [`Ribbon::wipe_of`]) above [`BIRTH_EDGE_FLOOR`].

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

/// The bed's published byte ceiling (`RAINBOW_UNDER_COV_CAP`): the
/// source-over stream's cap. The ledger's frame top (251) leaves the
/// companions their remainder above it.
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

/// Whether a composited `0x00RRGGBB` pixel reads as band ink to the glass
/// census ([`SCAN_INK_MAX`] / [`SCAN_INK_SPREAD`]); `Ribbon::ink_segments`
/// uses it per stop instead of the arc's worst case.
#[must_use]
pub fn reads_as_ink(rgb: u32) -> bool {
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

/// The contrast bar every legibility ceiling in the rainbow family is solved
/// against: unlit ink over lit ground, 5.25:1 (L3).
const BODY_CONTRAST_BAR: f32 = 5.25;

/// The margin under [`BODY_CONTRAST_BAR`] for the emitter's own rounding:
/// [`solve_for_luma`] answers from under its target and the LUT's lerp stays
/// under the bar (the EOTF is convex), leaving one level of composite rounding
/// and one Bayer level over the frame top.
const BODY_CONTRAST_GUARD: f32 = 0.05;

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

/// A jump lays its corridor behind the caret, newest at the landing, on the
/// band's existing walk (`t` is a function of position). New light is capped at
/// this many cells back from the landing, so a Ctrl-A across 200 columns does
/// not paint the whole row. Cells beyond the cap that the live band already
/// lights are taken over, never cut.
///
/// The cap binds at the corridor's far edge: light the corridor keeps past the
/// cap (on the glass: [`STATUS_LIT_COV`] of [`UNDER_COV_CAP`], either layer)
/// moves the far edge out to it and the columns between are laid
/// (`Ribbon::wake`).
pub const WAKE_MAX_CELLS: u16 = 32;

/// A wake's life: long enough to read as an after-effect, short enough that a
/// Ctrl-A / Ctrl-E ping-pong cannot accumulate. Fixed, not
/// [`Ribbon::cell_life`]: a jump takes neither the chain law nor the swoosh
/// floor. A later jump never holds the wake; a later scrub over it holds it
/// like any cohort under the hand, but a stub past its melt opening is never
/// held.
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

// The kill's retract span is [`kill_span_s`] in `timing.rs`
// (`per_cell * cells + base`).

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

/// The margin a cell's life keeps past its swoosh, so the swoosh always ends
/// the ribbon before the natural melt can. A cell's life floor is
/// [`swoosh_life_s`] of the rest its phrase was priced at when laid, and every
/// key that holds the phrase re-floors its cells ([`Ribbon::place`]), so a
/// rest that lengthens under a slowing hand never outlives its cells.
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

/// The share of a cell's life over which the expiry melt runs (v1's
/// `RAINBOW_EDGE_OUT`). Identity over the first 70 %.
pub const EXPIRY_MELT_SHARE: f32 = 0.30;

/// The expiry melt's exponent. Superlinear, because a cell out of time must
/// be at zero before it is removed or the removal is a visible step: at 1.6
/// the melt reaches 6e-4 of body on the last frame.
pub const EXPIRY_MELT_GAMMA: f32 = 1.6;

/// The coverage share the run's outer tail boundary keeps, ramping to full
/// across the tail cell, so the oldest edge feathers out. At every boundary
/// the younger side enters at this share and the brighter side wins:
/// `max(older, younger * ease)`, older being the tail side
/// ([`Run::stream_dir`]). Beside a leaving cell the feather grows as fast as
/// that cell's light goes, so no dark column opens.
pub const RUN_TAIL_EASE: f32 = 0.10;

// ===========================================================================
// The atoms
// ===========================================================================

/// The instant a cell's expiry is measured from: its birth or its cohort's last
/// live typing key, whichever is later, so a mark ages only once the hand
/// stops. A cell whose cohort is gone falls back to its birth. O(cohorts).
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
    /// The cell's light, priced once: the share of [`UNDER_COV_CAP`] its body
    /// may take at peak, `0..1`, resolved at birth from `Spine::birth_disp`
    /// and never re-read from the live spine. The follower is still climbing
    /// when the hand stops, so a body re-priced every frame would gain light
    /// with no keystroke behind it. Shape may follow the live spine (the wave,
    /// the wedge; both only settle); light may not.
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
    /// The row this cohort lives on. A cohort never spans rows; a wrap folds
    /// into a new cohort (§4's "wrap fold", kept).
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
    /// wake cohort clears the flag. An arrow over the word (every crossed cell
    /// lit) is a scrub ([`Ribbon::scrub`]): no wake, and the word's clock is
    /// held.
    pub wake: bool,
    /// The caret column the retract pulls this cohort toward, captured the
    /// frame it entered its retract ([`Ribbon::advance_swoosh`]) and held until
    /// a key brings the cohort back to laying: toward the hand that let go of
    /// it, not the live caret, so a later jump cannot make it lurch.
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
    /// The fold flow: the hand left this cohort's row downward by typing
    /// ([`Ribbon::leave_row`]), or the follow pass carried the run up with its
    /// text ([`Ribbon::translate_run`]), and no key on the new row renews it.
    /// It then flows at once instead of holding its grace: drift into the fold
    /// point over [`FLOW_TOTAL_S`], settle, and far-first fade
    /// ([`Cohort::phase_at`], [`Cohort::flow_drain`], [`Ribbon::env_of`],
    /// [`Ribbon::retract_x`], [`Ribbon::sample`]).
    ///
    /// Cleared when the hand is back on the row: a typing lay into the cohort,
    /// a hold by a key or erase on its row ([`Ribbon::hold`]), the hand's
    /// return up through the fold ([`Ribbon::take_row_back`]), or the follow
    /// pass carrying the run back onto the caret's row.
    ///
    /// `Some(at)` is when the row was folded off; the flow's clock is the later
    /// of that and the row's last key ([`Cohort::clock`]). A cohort already
    /// past its grace at the fold stays on its swoosh ([`Ribbon::flow_row`]).
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
    fn revoke_releases(&mut self, at: Instant) {
        self.release_clock = None;
        self.parts_revoked_at = Some(self.parts_revoked_at.map_or(at, |t| t.max(at)));
    }

    /// Hold a partial release in custody; with every slot taken, the
    /// oldest part is let go.
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
    fn part_mut(&mut self, stamp: Instant) -> Option<&mut ReleasePart> {
        self.release_parts
            .iter_mut()
            .flatten()
            .find(|p| p.stamp == stamp)
    }

    /// The band is leaving at `at`: abandoned by a jump, or inside its swoosh
    /// (retracting, fading, or over). The one predicate both the scrub
    /// (`Ribbon::scrub`) and the hop (`Ribbon::wake`) read, so they never
    /// disagree about one cell.
    #[must_use]
    /// A flowing cohort reads its own clock and schedule ([`Cohort::idle_s`],
    /// [`Cohort::phase_at`]): the fold flow has no grace and no reach, so from
    /// the fold onward it is leaving.
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
    #[must_use]
    fn walk_at(&self, col: u16) -> WalkPos {
        WalkPos {
            t: self.t_at(col),
            d: self.walk_d(col),
        }
    }

    /// The walk at [`Cohort::anchor_col`] — `(t0, d0)`, the pair the cohort
    /// was minted on.
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
    const fn restarted(self) -> Self {
        Self { t: self.t, d: 0.0 }
    }
}

/// A cohort's place in the exit swoosh (§4: `0.75 grace + 3 × 0.05 reach +
/// 0.40 retract + 0.24 fade`), kept verbatim from v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// Taking cells — the hand is on it.
    #[default]
    Laying,
    /// The 0.75 s grace after the last key: nothing moves yet.
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
    /// The body's full-strength centreline here, sub-pixel — the 0.75-cycle
    /// wave at `0.055 ch · disp` rides on this value.
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
    fn clear(&mut self) {
        for &col in &self.touched {
            if let Some(slot) = self.t.get_mut(usize::from(col)) {
                *slot = f32::NAN;
            }
        }
        self.touched.clear();
    }

    /// Size the lane to `cols`, filling with the "no field here" sentinel.
    fn resize(&mut self, cols: u16) {
        self.t.clear();
        self.t.resize(usize::from(cols), f32::NAN);
        self.touched.clear();
    }
}

/// The field index: `col -> t` for every row carrying live ribbon light,
/// rebuilt once per frame by [`Ribbon::plan`]. Lookups are O(1) in columns and
/// linear in live rows (a handful; the ribbon is a per-row mark and the
/// cohort cap bounds it).
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
    #[must_use]
    pub fn at(&self, row: u16, col: u16) -> Option<f32> {
        let lane = self.lanes.iter().find(|l| l.row == row)?;
        let t = *lane.t.get(usize::from(col))?;
        if t.is_nan() { None } else { Some(t) }
    }

    /// True when no row carries field light — one of the three pools
    /// `needs_frame_cadence()` ORs (§18's idle → zero).
    #[must_use]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.lanes.iter().all(|l| l.touched.is_empty())
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
    fn reset(&mut self) {
        self.lanes.clear();
        self.spare.clear();
    }

    /// Move every lane with a scroll of `rows`: a lane whose row leaves the
    /// grid retires into the spare pool, cleared over its `touched` list; the
    /// rest keep their columns and take the new row, so [`FieldIndex::at`]
    /// answers for the moved cell between the scroll and the next plan.
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

/// One sRGB channel, 0..1, through the EOTF to linear light.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The bed's one ceiling, in relative luminance: the brightest a source-over
/// ribbon may composite and still leave `theme_fg` legible over it at
/// [`BODY_CONTRAST_BAR`] plus [`BODY_CONTRAST_GUARD`]. A function of the
/// foreground alone, clamped to [`BED_LUMA_MIN`] and [`BED_LUMA_MAX`];
/// [`bed_ink`] spends it.
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
fn scale_rgb(rgb: u32, k: f32) -> u32 {
    let ch = |sh: u32| ((((rgb >> sh) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// A walk toward white by `w`: `c + w * (255 - c)`. Channel differences
/// survive unscaled, so the hue direction is kept and only saturation falls.
/// The last resort of [`onto_luma`], taken only from a stop already at full
/// value, because white buys light at the price of chroma.
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
fn solve_for_luma(family: impl Fn(f32) -> u32, target: f32) -> u32 {
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if relative_luminance(family(mid)) < target {
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

/// The bed's on-glass ink: an arc colour put on one composited luminance,
/// `budget` ([`bed_luma_budget`]), at the most chroma that luminance allows.
///
/// * [`BED_SAT_FLOOR`] first, hue and value untouched;
/// * then [`onto_luma`]: a colour brighter than the budget (red, yellow,
///   green) is scaled down uniformly, hue exact; a darker one (blue, indigo,
///   violet) is lifted through its own hue to full value and only then toward
///   white, so it arrives as a vivid stop.
///
/// Every stop of the arc therefore composites at the same weight, so one
/// coverage ceiling holds for the whole arc and the per-row melt step is the
/// same at every position. On the default theme a warm stop at that weight is
/// `(80, 80, 3)` composited.
#[must_use]
pub fn bed_ink(rgb: u32, budget: f32) -> u32 {
    onto_luma(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR), budget)
}

/// The WCAG contrast of one channel, both bytes read as a monochrome light:
/// the per-channel form of [`relative_luminance`]'s ratio, in which
/// [`SEEN_CONTRAST`] is stated.
///
/// Per channel because the bed equalises the arc's luminance ([`bed_ink`]), so
/// a whole-colour test cannot tell its stops apart: at one luminance violet is
/// `#0019FD` and a warm stop `#585000`. The hue-carrying channel is what the
/// eye picks out of the ground and what the census measures.
fn channel_contrast(a: f32, b: f32) -> f32 {
    let (a, b) = (srgb_to_linear(a / 255.0), srgb_to_linear(b / 255.0));
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether a composited pixel is still a stop: [`SEEN_CONTRAST`] against the
/// ground in its furthest channel ([`channel_contrast`]) and [`SEEN_SPREAD`]
/// bytes of channel spread. Contrast alone passes a grey smudge; spread alone
/// passes a tinted ground. Both halves are ground-independent.
#[must_use]
fn seen(rgb: u32, bg: u32) -> bool {
    let ch = |c: u32, sh: u32| f32::from(((c >> sh) & 0xff) as u8);
    let (r, g, b) = (ch(rgb, 16), ch(rgb, 8), ch(rgb, 0));
    let stand = channel_contrast(r, ch(bg, 16))
        .max(channel_contrast(g, ch(bg, 8)))
        .max(channel_contrast(b, ch(bg, 0)));
    let spread = r.max(g).max(b) - r.min(g).min(b);
    stand >= SEEN_CONTRAST && spread >= SEEN_SPREAD
}

/// The coverage at which a stop goes out: the smallest alpha byte at which
/// `ink` over `bg` still passes [`seen`]; `256` for an ink no coverage can make
/// read (which [`Ribbon::ember_floor_of`] turns into "hold at full until the
/// pool clears").
///
/// The bed puts every stop on one luminance, not one distance from the
/// ground, so a shared alpha fade takes the warm stops out while their
/// neighbours are bright (on the default theme chartreuse goes out at 188 of
/// 236, blue and violet at 52). This is the arithmetic behind the ember's
/// per-stop floor.
///
/// Solved once per theme into [`BedInkLut::seen`] by a scan over the 256
/// alphas with the renderer's own compositor. The scan runs down from full
/// and answers one over the highest alpha that fails, so the stop reads at
/// the floor and every coverage above it.
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
/// RAIL_LUMA_CEIL]`: lifted toward white to the floor (blue, indigo, violet),
/// scaled down through its hue to the ceiling (yellow, green, orange), carried
/// pure between (red). The rail lies where no letter can be, so it answers to
/// the caret's light floor rather than the legibility bar: yellow `(149, 149,
/// 0)`, green `(0, 168, 0)`, orange `(227, 113, 0)`, red `(255, 0, 0)`, blue
/// `(103, 103, 255)`. The green→blue crossing, authored under full value, is
/// scaled up through its hue first (`scale_rgb`) and takes [`BED_SAT_FLOOR`].
///
/// Solved once per theme into [`BedInkLut`]'s third table.
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
            // Indexed by the walk, filled from the arc ([`WALK_PACE`]): readers
            // keep `sample`'s one lerp and the pace costs nothing per frame.
            let arc = spectrum(walk_pace(x));
            let bed = if cfg.dark_theme {
                bed_ink(arc, budget)
            } else {
                role.ink(arc)
            };
            self.lut.push(bed);
            self.hot.push(hot_edge_ink(arc));
            self.rail.push(rail_ink(arc));
            self.seen.push(seen_floor(bed, cfg.theme_bg));
        }
        self.key = Some(key);
    }

    /// The ink at walk position `t`. The walk is unbounded, so it is folded
    /// through `meteor::tri` — the same reflection §6.4's arc uses, which is
    /// what makes "the station under the caret equals the caret's stop" true
    /// by construction rather than by coincidence.
    #[must_use]
    pub fn at(&self, t: f32) -> u32 {
        Self::sample(&self.lut, t)
    }

    /// The hot edge's ink at walk position `t` — the same fold, the hot
    /// table ([`hot_edge_ink`]).
    #[must_use]
    pub fn hot_at(&self, t: f32) -> u32 {
        Self::sample(&self.hot, t)
    }

    /// The vivid rail's ink at walk position `t` — the same fold, the rail
    /// table ([`rail_ink`]).
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

/// The walk's pace across the green→blue leg, `@generated`: where a walk
/// position lands between [`CROSS_ARC0`] and [`CROSS_ARC1`], as `u16` shares
/// of the arc at 65 evenly spaced walk positions. The window's cell budget is
/// unchanged: one pass is still [`WALK_FAST_CELLS`] cells, and cells move only
/// within this leg.
///
/// A ribbon reads one arc entry per cell (`1/16` of `t` is 31.9 entries), so
/// the authored green→blue handoff was little more than one cell wide and
/// showed as a step whose size depended on the walk origin. This table spaces
/// the leg by what the eye reads, the composited bed and the vivid rail in
/// equal parts, taking the worst step on this leg from 109.5 to 73.3 (bed)
/// and 164.5 to 129.2 (rail); the other five legs are the identity. Further
/// smoothing would need cells from the neighbouring legs. The band stays
/// quantised into flat slabs, with the bed's bar and the rail's full-value
/// spectrum unchanged.
///
/// `the_walk_s_pace_is_the_arc_s_own_perceptual_step` re-derives it from
/// `bed_ink`/`rail_ink`, so an ink change is caught. It is nearly
/// ground-independent (the luma budget scales the inks), so one table serves
/// every theme. Per-leg numbers: `targo --unverified run -p aterm-effects
/// --example legpace`.
const CROSS_PACE: [u16; CROSS_PACE_KNOTS] = [
    33539, 33818, 34076, 34375, 34679, 35026, 35233, 35530, 35828, 36087, 36381, 36642, 36900,
    37159, 37456, 37760, 38011, 38231, 38537, 38825, 39085, 39343, 39598, 39897, 40189, 40573,
    40831, 41141, 41399, 41696, 41916, 42169, 42470, 42683, 42939, 43158, 43336, 43542, 43795,
    43969, 44174, 44364, 44569, 44782, 44996, 45199, 45337, 45554, 45715, 45896, 46035, 46197,
    46409, 46552, 46712, 46840, 47051, 47224, 47523, 47738, 47952, 48208, 48423, 48553, 48830,
];

/// Knots in [`CROSS_PACE`].
pub const CROSS_PACE_KNOTS: usize = 65;

/// The arc position of a walk position: [`meteor::tri`]'s fold through
/// [`walk_pace`]. Every reader of a walk `t` goes through this, so the caret,
/// bed, rail, hot edge, meteor train and star halos land on the same stop.
#[must_use]
pub fn walk_arc(t: f32) -> f32 {
    walk_pace(clamp01(tri(t)))
}

/// Where a walk position lands on the arc: the identity outside the middle,
/// [`CROSS_PACE`] inside it. Monotone, with `0 ↦ 0`, `1 ↦ 1` and both window
/// edges fixed.
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

/// The walk position that shows a given stop: [`walk_pace`]'s inverse, for
/// readers that start from a named colour (the rail's yellow, the landing's
/// anchors, the crossing's seam).
#[must_use]
#[cfg(test)]
pub fn walk_of_arc(a: f32) -> f32 {
    let a = clamp01(a);
    if a <= CROSS_ARC0 || a >= CROSS_ARC1 {
        return a;
    }
    let want = a * 65535.0;
    let mut f = (CROSS_PACE_KNOTS - 1) as f32;
    for i in 1..CROSS_PACE_KNOTS {
        let (lo, hi) = (f32::from(CROSS_PACE[i - 1]), f32::from(CROSS_PACE[i]));
        if want <= hi {
            f = i as f32 - 1.0
                + if hi > lo {
                    (want - lo) / (hi - lo)
                } else {
                    0.0
                };
            break;
        }
    }
    CROSS_ARC0 + f / (CROSS_PACE_KNOTS - 1) as f32 * (CROSS_ARC1 - CROSS_ARC0)
}

/// The classic walk: `t` at `d` cells from where the walk started. `d/16` over
/// the first [`WALK_FAST_CELLS`] cells (a short word shows several stops),
/// then [`WALK_LAY_RATE`] per cell. `d` is the walk's distance: a continuation
/// starts [`Cohort::d0`] cells in, so a wrapped row keeps its pace; the fast
/// leg belongs to a walk's start (cold, or a new line, [`Ribbon::fresh_line`]).
/// Below zero it is the fast line continued; the only knee is at 16.
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

/// A cell's terminal approach to zero, read on its own clock (v1's
/// `rainbow_expiry_melt`): smoothstep over the last [`EXPIRY_MELT_SHARE`] of
/// life, raised to [`EXPIRY_MELT_GAMMA`]. Exactly `1.0` over the first 70 % of
/// the domain and exactly `0.0` at `u = 1`.
#[must_use]
pub fn expiry_melt(u: f32) -> f32 {
    let taper = smoothstep01((1.0 - clamp01(u)) / EXPIRY_MELT_SHARE);
    if taper >= 1.0 {
        1.0
    } else {
        taper.powf(EXPIRY_MELT_GAMMA)
    }
}

/// The focus ember's level, per stop: the band's common fade `f` (the `spend`
/// curve, or one linear fade under reduced motion) mapped into the stop's own
/// range, from exactly `1.0` at the blur to exactly `floor` when spent, where
/// `floor` is the coverage at which this stop stops being seen ([`seen_floor`],
/// as a share of the cell's full coverage).
///
/// The band leaves as one object: a common multiplier would take the warm
/// stops under the colour floor while blue is still bright, splitting the band.
/// Here no stop crosses its floor while the ember has level, and the ember's
/// end ([`Ribbon::retire`]) takes every cell in the same frame.
/// Non-increasing in `f`. A cell whose floor is above its own coverage holds
/// what it has; the clamp is on the level, never the floor.
#[must_use]
pub fn ember_level(floor: f32, f: f32) -> f32 {
    let floor = floor.max(0.0);
    clamp01(floor + (1.0 - floor) * clamp01(f))
}

/// The one linear fade of the theme (§2.5, §6.11): under reduced motion a
/// mark's alpha runs to zero linearly over the last
/// [`REDUCED_MOTION_FADE_MS`] of its life. `remaining_s` is how long the mark
/// has left; `1.0` until the window opens, exactly `0.0` at the end.
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

/// The first text column a same-row re-anchor's landing row holds, keyed by
/// the move: `(from, to, start)` ([`Ribbon::note_rewrap_start`]).
type RewrapStart = ((u16, u16), (u16, u16), u16);

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
    /// The pen: `(row, col)` where the ribbon's own last hand event left the
    /// caret, what [`Ribbon::typed_landing`] calls "where the hand is". Not
    /// [`Ribbon::caret`], the app's mirror, which reads `(0, 0)` before any
    /// caret is seen and is planted by every `Move` before
    /// [`Ribbon::hand_changed_row`] rules on it (so a repaint's parking pass
    /// can write itself there,
    /// [`a_repaints_home_pass_poisons_the_mirror_and_the_pen_still_holds_the_hand`]).
    ///
    /// The pen moves only on the hand: a typed lay (to its landing), a
    /// Backspace or kill (to the caret it was replayed against), and a
    /// nav/return/synthetic move; never on an echoed move. It rides a scroll
    /// and a band move with its light, as `Ribbon::last_space` does, and is
    /// dropped by [`Ribbon::curtain`] and [`Ribbon::reset`]. `None` until the
    /// hand has acted. The `Kill` update and the four coordinate-space clauses
    /// have no pinning test; they follow `last_space`'s law.
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
    /// Where the relaid text starts, as the row shows it: for the same-row
    /// re-anchor `from → to` about to be replayed, the first column the landing
    /// row's sample holds text at ([`Ribbon::note_rewrap_start`]). Taken by that
    /// move's [`Ribbon::re_anchor`] alone.
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
    /// The walk a finished swoosh left behind on its row, so a same-row
    /// rebirth inside [`CHAIN_GAP_MAX`] continues the arc — its colour and
    /// its pace, or its colour alone after a new line
    /// ([`Ribbon::fresh_line`]) — instead of re-anchoring at red:
    /// `Cohort::t0`'s documented continuation, which had nothing to read
    /// once `retire` dropped the cohort with its cells.
    last_walk: Option<LastWalk>,
    /// The hand started a new line: when, and the row it left ([`FreshLine`]),
    /// or `None`. The walk's distance travels with its colour along the same
    /// input ([`Cohort::d0`]: a wrap fold, the composer's relay, a same-row
    /// rebirth) and restarts where the hand starts a new line.
    ///
    /// Set by [`Event::Return`], a line kill ([`super::KillScope::Line`]),
    /// [`Ribbon::curtain`], a Return's or composer newline's own move
    /// ([`Licence::Return`], whatever its rows: a bottom-anchored box homes the
    /// caret on the same row), and a row change the hand navigated
    /// ([`Licence::Nav`]: arrow, click, history recall). Not by a same-row hop,
    /// an echoed move (fold, relay), a completion or scripted preview
    /// ([`Licence::Synthetic`]) or a program's jump.
    ///
    /// While set, the next typed cell that mints a cohort (either of
    /// [`Ribbon::join_cohort`]'s continuation arms, the freshest cohort's end
    /// or a finished swoosh's [`LastWalk`]) mints on [`WalkPos::restarted`]:
    /// the colour reached, at distance zero. The first typed cell the gate
    /// takes ([`FreshLine::takes`]) clears it, whatever cohort it lands in. The
    /// composer's relay ([`Ribbon::relay_word`]) is never gated: the word it
    /// moves is the old row's text. The engine's pre-move replay of a key onto
    /// the old line's last cell does not spend it ([`Ribbon::lay_cell`]).
    ///
    /// An instant, not a flag, and the earliest one: typed glyphs are laid on
    /// their key's clock, but a row change reaches the ribbon when the host
    /// observed it, a repaint later. So re-arming never moves the instant
    /// later, and a cross-row move records the row the hand left: a typed cell
    /// laid on any other row is the new line's whatever its clock, while the
    /// old line's late glyph (`…x⏎` with `x` held for its echo,
    /// [`Ribbon::hold_typed`]) lands on its own row and is neither restarted
    /// nor allowed to spend the gate.
    ///
    /// Without the gate, the line typed after a Return would continue past the
    /// walk's knee (stepping `1/36`), and a meteor landing on that row (which
    /// walks `meteor::landing_field`'s `d/16`) would no longer match the band.
    fresh_line: Option<FreshLine>,
    /// The seam on the new line. When the input continues onto a new row (a
    /// shell fold, a composer re-wrap, [`Ribbon::leave_row`], or the follow
    /// pass carrying text off the caret's row, [`Ribbon::arm_seam`]), the hot
    /// edge along the new line's tail holds a floor of [`SEAM_LEVEL`] back to
    /// the first column the hand laid on the row ([`Ribbon::hand_floor_on`]),
    /// spent over [`FLOW_TOTAL_S`] as the row above drifts out
    /// ([`Ribbon::emit_hot_edge`]).
    ///
    /// Never on a new line the hand started (every [`Ribbon::arm_fresh_line`]
    /// setter clears it), a jump, or a program's move. Cleared by a curtain, a
    /// reset, a focus loss, an abandon of its row, and a run the follow pass
    /// carries back onto it ([`Ribbon::translate_run`]); carried with its row
    /// (and the hand's floor) by a scroll or band move, dropped when carried
    /// off. Dark themes only, off under reduced motion.
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
    /// **The open phrase** (§2.3), or `None` between phrases — after a rest
    /// has passed since [`Ribbon::last_key_at`], a Return or a line kill.
    /// The next typed lay opens the next one.
    phrase_open: Option<u32>,
    /// The next phrase id to mint; `0` is never minted (it is "no phrase",
    /// a wake cohort's).
    next_phrase: u32,
    /// When the curtain in progress fell, or `None`. The column each cohort is
    /// drawn toward is frozen on the cohort ([`Cohort::retract_col`],
    /// [`Cohort::drain_right_first`]) from the band's own hand, not
    /// [`Ribbon::caret`] (see [`Ribbon::curtain`]).
    curtain: Option<Instant>,
    /// The hand's floor on its row: `(row, col)`, the leftmost column the hand
    /// has held on that row (a glyph it typed, [`Ribbon::lay_cell`] with
    /// `typing`; a deliberate relocation, the non-echo arm of `Event::Move`; or
    /// the caret an erase was replayed against).
    ///
    /// The exit swoosh's reach extends the band leftward with no other bound,
    /// so without this a prompt's `$` could light up. The law: nothing the hand
    /// did not put there goes left of where the hand has been on that row.
    /// Typing and arrows lower the floor, so a caret reaching column 0 frees
    /// the row.
    ///
    /// It bounds the reach, and decides whether a same-row re-anchor's relay is
    /// a word the app moved or a shell's `^A` inside untouched text
    /// ([`Ribbon::re_anchor`]). It is not applied to [`Ribbon::sweep`] or the
    /// relay itself ([`Ribbon::relay_word`], [`Ribbon::begin_relocate`]),
    /// which lay the hand's own glyphs, often onto a new row where the hand
    /// has no floor yet.
    ///
    /// One row, not a map: a row the hand has left keeps its own `col0` as the
    /// reach's bound. A row change replaces it.
    hand_floor: Option<(u16, u16)>,
}

/// A re-anchor's moved word.
///
/// A TUI composer (Claude Code's Ink box, Codex) re-wraps its box per
/// keystroke: the row's last word moves down to the next visual row and the
/// caret lands after it. The seam licenses that as a typed re-anchor and the
/// ribbon sees one `Move`. The ribbon knows the word's length from the last
/// typed Space (`w` cells before the origin), so it relays the word where the
/// caret reveals it: `[landing − 1 − w, landing − 1)`, with the wrap key's
/// glyph at `landing − 1`. Ink lands the caret at the box's inset first and at
/// the word's end in a move the seam refuses, so the relocation waits and the
/// next same-row forward typed move on the row pays it from its origin.
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
    #[must_use]
    pub fn of(coh: &Cohort) -> Self {
        Self {
            anchor_col: coh.anchor_col,
            t0: coh.t0,
            d0: coh.d0,
        }
    }

    /// The pair a cohort is minted on at [`RunOrigin::anchor_col`].
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
    fn takes(self, row: u16, at: Instant) -> bool {
        !self.awaiting_move
            && self.chord_at.is_none_or(|chord| at != chord)
            && (at >= self.since || self.left_row.is_some_and(|left| left != row))
    }
}

impl Ribbon {
    /// A ribbon with nothing laid.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The host has queued the licensed move of the exact held park whose
    /// source row's content followed. This one-shot proof lives for its
    /// replay frame only, so a later park cannot inherit it. The original
    /// park clock distinguishes its queued move from any other exact pair
    /// already buffered on that frame.
    pub(crate) fn prove_short_reanchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        debug_assert!(from.0 == to.0 && (1..=2).contains(&from.1.saturating_sub(to.1)));
        self.short_reanchor = Some(ShortReanchorProof { from, to, at });
    }

    /// The live cells, in pool (append) order. Read-only.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The live cohorts. Read-only.
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
    /// * [`Event::Typed`] extends the head cohort (or folds a wrap into a new
    ///   one).
    /// * [`Event::Erase`] retracts the row's suffix from the caret on, so the
    ///   lit run stays contiguous behind the caret and no cell keeps light over
    ///   a shifted glyph; [`Event::Kill`] drains the same suffix, at least
    ///   `cells` wide, farthest first at `12·n + 240` ms.
    /// * [`Event::Move`] moves [`Ribbon::caret`], then asks [`Ribbon::scrub`]
    ///   about a keyed same-row move. A move inside the hand's lit run is a
    ///   scrub: it lays, takes over and abandons nothing, and holds the
    ///   cohorts it crossed. A move that leaves the run under a non-typed
    ///   licence (a row change, or [`JUMP_MIN_CELLS`] or more) is a real jump:
    ///   a navigation or synthetic jump lays its wake ([`Ribbon::wake`], on
    ///   [`Layer::Over`], each cell born on the meteor's flight clock) and
    ///   abandons every other cohort into its retract, draining from the
    ///   origin end. A shorter arrow hop lays a short wake in its own cohort and
    ///   abandons nothing. A band already leaving keeps its clock. A PTY or
    ///   return-licensed jump only abandons.
    /// * [`Event::Focus`] takes the 300 ms ember, and on regain inside it the
    ///   `edge-in` rearm; [`Event::ReducedMotion`] and [`Event::Return`] lay
    ///   nothing.
    ///
    /// Every event applies against `ctx.caret`, the caret for this tick; a host
    /// buffering two typed echoes into one tick must coalesce them into one
    /// `Typed { cells: 2 }`.
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
                // The pen follows the ribbon's own lay: where this key put its
                // glyph.
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
                    // ([`Ribbon::hand_floor`]): an arrow or a ^A is the hand
                    // Reaching a column, so it frees that column for the
                    // leftward reach a repaint never may.
                    self.touch_hand(to.0, to.1);
                }
                // The hand started a new line: a Return's or composer newline's
                // move ([`Licence::Return`]) whatever its rows, or a navigation
                // to another row. Not a same-row hop, an echoed move, a
                // completion or scripted preview ([`Licence::Synthetic`]), or a
                // program's jump. `at` is when the host observed the move, so
                // the gate keeps any earlier instant and a row change records
                // the row left ([`FreshLine`]).
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
                    // A typed echo's forward motion is inert here (the key laid
                    // its cell), as is a delivered insert's (its sweep laid the
                    // span). Not inert: the hand leaving the row, an erase's
                    // retreat, and a composer's re-anchor.
                    if !same_row {
                        // A repaint's home is not a gesture: a bottom-pinned
                        // composer repaints by homing to the box's top-left,
                        // and a fresh type hint licenses those carets `Typed`
                        // (`cursor_glow::v2_licence`). Only a move the hand
                        // could have made is a row change
                        // ([`Ribbon::hand_changed_row`]); anything else under an
                        // echo licence moves only the caret mirror.
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
                            self.retract_suffix(to.0, to.1, from.1 - to.1, at, RETRACT_FADE_S);
                            self.pending_erase = 0;
                        } else if licence == Licence::Typed
                            && !self.scroll_followed
                            && (from.1 - to.1 >= RE_ANCHOR_MIN_CELLS
                                || self.short_reanchor.is_some_and(|proof| {
                                    proof.from == from && proof.to == to && proof.at == at
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
                    // The scrub: a keyed same-row move whose every crossed cell
                    // is lit by a band that is not leaving is the hand moving
                    // over its own text, at any distance, either way. It lays,
                    // takes over and abandons nothing, and holds the band
                    // ([`Ribbon::scrub`]). Everything below is for a move that
                    // leaves the run.
                    let scrub = same_row
                        && licence.is_keyed_nav()
                        && self.scrub(to.0, from.1, to.1, at, ctx);
                    let jump = !same_row || to.1.abs_diff(from.1) >= JUMP_MIN_CELLS;
                    if scrub {
                        // Held; nothing else to do.
                    } else if jump {
                        // The wake, then the abandon that spares it. A row
                        // change has no corridor on one row (an Up-arrow
                        // recall must not paint the line it lands on), and a
                        // PTY cascade or a Return earns no credit — they keep
                        // the old law: abandon, lay nothing.
                        let wakes = same_row && licence.is_keyed_nav();
                        let keep = if wakes {
                            self.wake(to.0, from.1, to.1, at, ctx, WAKE_LIFE_S, true)
                        } else {
                            None
                        };
                        // A program's jump strands the band under a hand that
                        // never left it: the band goes back into the hand's last
                        // column, and the next key may take it back
                        // (`join_cohort`). A same-row jump the hand made drains
                        // toward the landing, origin end first, following the
                        // meteor (§2.6/§2.7).
                        let program = licence == Licence::Pty;
                        // The hand's target carries its row: a same-row jump names
                        // `to.0` and [`Ribbon::abandon`] stamps that row alone;
                        // other rows go through [`Ribbon::retract_target`] and
                        // drain into their own end. The program arm keeps its
                        // whole-ribbon law and names no row.
                        let toward = if program {
                            Some((None, from.1, false))
                        } else {
                            same_row.then_some((Some(to.0), to.1, to.1 < from.1))
                        };
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

    /// **Arm the seam** on `row` at `at` ([`Ribbon::seam`]): the input
    /// continued onto that row. A newer continuation replaces the seam.
    pub fn arm_seam(&mut self, row: u16, at: Instant) {
        self.seam = Some(Seam { row, at });
    }

    /// The focused pane's `(first column, width)`; `None` is the whole grid.
    pub fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// The focused pane's `(first row, rows)`; `None` is the whole grid.
    /// See [`Ribbon::pane_rows`] for why the band needs it.
    pub fn set_pane_rows(&mut self, pane_rows: Option<(u16, u16)>) {
        self.pane_rows = pane_rows;
    }

    /// The floor the band may reach to, in frame pixels: the focused pane's
    /// bottom edge, or the grid's when no pane is set, never past the grid's.
    /// Every "is there leading below this row?" question asks this, not
    /// [`Geom::fx_bot`] ([`Ribbon::pane_rows`]).
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
        // ending ([`Ribbon::hand_floor`]).
        self.pen = None;
        self.arrived = None;
        self.hand_floor = None;
        // …and a soft-wrapped word waiting for its reflow: the curtain is
        // the same coordinate-space cut ([`Ribbon::settle_carry`]).
        self.carry = None;
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
    fn curtain_hand(&self) -> Option<(u16, u16)> {
        let newest = |wake_ok: bool| {
            freshest(self.cohorts.iter().filter(|c| wake_ok || !c.wake)).map(|c| (c.row, c.col1))
        };
        newest(false).or_else(|| newest(true))
    }

    /// Whether a curtain is falling — the host's "nothing is laid" gate
    /// and the twins' witness.
    #[must_use]
    pub fn curtained(&self) -> bool {
        self.curtain.is_some()
    }

    /// The phrase rest in force, seconds: [`phrase_rest_ms`] of the hand's
    /// tempo; the floor before any key.
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

    /// A typed key on the score (§2.1, §2.3): measure the gap since the last
    /// key, resolve a rest ([`super::timing::phrase_rest_reached`] on the
    /// previous tempo), advance the tempo by the melody's law, and open a new
    /// phrase if the last one is closed. Once per `Typed` event.
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
    fn open_phrase(&mut self) {
        let id = self.next_phrase.max(1);
        self.next_phrase = id.wrapping_add(1).max(1);
        self.phrase_open = Some(id);
    }

    /// The phrase a typed lay belongs to — the open one, opened here if a
    /// typed echo arrives with none open (a ledger-bridged sweep after a
    /// Return, say): a typed cell always has a phrase.
    fn phrase_for_lay(&mut self) -> u32 {
        if self.phrase_open.is_none() {
            self.open_phrase();
        }
        self.phrase_open.unwrap_or(1)
    }

    /// The hand is on the phrase under it (an erase or a kill): every
    /// un-abandoned cohort of the open phrase on the caret's row keeps its
    /// laying clock, its grace is re-priced at the rest in force, and the rest
    /// is measured from this key. The row scope is `ribbon_row_hold_model`'s
    /// `OnlyOwnerRenews`; the phrase narrows the hold within the row.
    ///
    /// An erase after the rest opens a new phrase and adopts the caret row's
    /// un-abandoned cohorts into it, so a Backspace run begun during the
    /// after-glow brings the band back: a hand that comes back to a line is on
    /// that line. It never widens the hold across rows.
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

    /// Refresh every un-abandoned cohort of phrase `open` at `at`: the laying
    /// clock, the phase, the frozen retract target, the grace, and its cells'
    /// life floor, so a lengthened rest never outlives its cells.
    ///
    /// A held row is wet again: a flowing cohort the hold renews loses its flow
    /// and drift, as a typed lay ([`Ribbon::place`]) or the follow pass
    /// ([`Ribbon::translate_run`]) would do. A shell's Backspace back through a
    /// fold reaches its row only through a hold (its erase's, or the one the
    /// hand's arrival makes, [`Ribbon::take_row_back`]); left flowing, the row
    /// would drain away under the hand.
    fn hold(&mut self, open: u32, hand_row: u16, laid_into: Option<usize>, at: Instant) {
        let grace = self.rest_s();
        let floor = swoosh_life_s(grace);
        let held_by = |i: usize, c: &Cohort| {
            !c.abandoned && c.phrase == open && (c.row == hand_row || laid_into == Some(i))
        };
        for (i, coh) in self.cohorts.iter_mut().enumerate() {
            if held_by(i, coh) {
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                coh.grace_s = grace;
                if coh.flow.take().is_some() {
                    coh.flow_drift = 0.0;
                }
            }
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

    /// The hand came back up through the fold: an erase's retreat one row up (a
    /// shell's Backspace or word kill back through the fold,
    /// [`Ribbon::leave_row`]) lands the hand on the row it had folded off.
    /// Every un-abandoned cohort there still flowing ([`Ribbon::flow_row`]'s
    /// mark) joins the open phrase `open`, and the row is held
    /// ([`Ribbon::hold`], clearing flow and drift) at the arrival instant.
    ///
    /// The erase alone cannot guarantee this: where it is replayed depends on
    /// when its echo lands (on the row in the key's tick, or on the new row's
    /// first column a tick later), and a run begun after the rest mints a
    /// phrase on the row below, so the folded row would drain under the parked
    /// hand.
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
    fn retract_target(&self, coh: &Cohort) -> (Option<u16>, bool) {
        Self::target_toward(coh, self.caret)
    }

    /// A flowing band's fold edge, drain order and drift, frozen the frame its
    /// flow begins ([`Ribbon::advance_swoosh`]). The edge is the band's kept
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
    /// Still the suffix, not "retract the erased cell and slide the rest
    /// left": measured on a mid-word Backspace run in a wrapped composer the
    /// slide showed no gain within run-to-run scatter, and it stacks a slid
    /// live cell on the retracting cell under it for the retract's length.
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
    fn alive_at(&self, cell: &Cell, at: Instant) -> bool {
        !cell.leaving()
            && at
                .saturating_duration_since(live_since(&self.cohorts, cell))
                .as_secs_f32()
                < cell.life_s
    }

    /// Whether a live cell owns `(row, col)` at `at` — the cell a sweep or
    /// a relay leaves exactly as it is.
    fn owned(&self, row: u16, col: u16, at: Instant) -> bool {
        self.cells
            .iter()
            .any(|c| c.row == row && c.col == col && self.alive_at(c, at))
    }

    /// A key whose echo has not landed is held, not laid. `Event::Typed` is
    /// replayed at the engine's mirror and [`Ribbon::lay`] lays `caret.col −
    /// n`, which is the key's own cell only once its echo moved the mirror; a
    /// frame earlier it is the previous cell (the prompt's trailing space, or
    /// through the fold the previous row's last pane cell), a phantom. The
    /// engine calls this instead of [`Ribbon::on_event`] for a `Typed` with no
    /// move after it in the frame: the `Typed` arm minus the lay (erase count
    /// closed, a Space remembered, the caret row held). The key's cell comes
    /// with its echo, from the seam's sweep.
    ///
    /// `cell` is the cell the held key will lay (the mirror advanced past the
    /// keys held before it), or `None` for a caret the engine never learned. A
    /// held Space is remembered at that cell, since the composer's word move
    /// measures from the Space's own cell and the echo's sweep cannot correct
    /// it later.
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

    /// A typed echo's glyph cells ([`Event::Sweep`]): lay every cell in
    /// `col0..col1` on `row` that no live cell owns ([`Ribbon::owned`]), as
    /// typing, born at the echo. A key whose echo shares its tick already owns
    /// its cell and is left as is (per-key typing is byte-identical; the
    /// deletion goldens pin it). The sweep lights what the keys could not: a
    /// batched echo's cells behind the landing, and a late echo's glyph (the
    /// key was held, [`Ribbon::hold_typed`]).
    ///
    /// The hand types over old light: a live cell born before this key is an
    /// earlier key's glyph the hand has typed over (a mid-line insert), and is
    /// re-laid as this key's own, with a fresh identity for the witness and
    /// the light it had ([`Ribbon::place`]'s monotone re-wet); otherwise the
    /// witness would melt the column as replaced text
    /// (`tests/codex_particle_replay.rs`). A live cell born at or after `at` is
    /// this key's own and is left as is.
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

    /// Where a typed key lands: the caret the tick reports, unless laying there
    /// would write the key's glyph on a row the key cannot have written on.
    /// That is a repaint's parking pass, and the key lands at the pen instead,
    /// `cells` on.
    ///
    /// An input box (Claude Code's or Codex's composer) repaints by homing to
    /// its top-left, clearing and rewriting; the host licenses those
    /// intermediate carets `Typed` (`cursor_glow::v2_licence`), so a key's echo
    /// can be replayed at a parking pass. [`Ribbon::lay`]'s fold arithmetic
    /// would then walk the cell off the pane's first column onto the end of the
    /// row above: a stray on a row the hand never visited, and a hole where the
    /// key's own cell belonged.
    ///
    /// "Where the hand is" is [`Ribbon::pen`], not [`Ribbon::caret`]: the
    /// mirror reads `(0, 0)` before any caret is seen and is planted by every
    /// `Move` before [`Ribbon::hand_changed_row`] rules, so a parking pass
    /// sets it
    /// ([`a_repaints_home_pass_poisons_the_mirror_and_the_pen_still_holds_the_hand`]).
    /// The pen is `None` until the hand has acted, so a cold session brings no
    /// key back anywhere
    /// ([`a_cold_caret_mirror_is_not_a_hand_a_key_can_be_brought_back_to`]).
    ///
    /// The law concerns the row reached, not distance or column: a park two
    /// rows below the pen must be re-homed
    /// ([`a_repaint_that_parks_below_the_hand_does_not_take_the_key_with_it`]),
    /// while a run that simply begins three rows down is taken as it comes
    /// ([`a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it`]).
    /// [`Ribbon::fold_top_row`] is the topmost row `lay` would write on; a
    /// landing is the hand's when that row is the landing's own (nothing
    /// folded) or the pen's (the hand's line wrapping,
    /// [`a_delivered_burst_that_wraps_keeps_the_cells_it_left_on_the_row_above`]).
    /// A downward caret that does not fold is taken however far below.
    ///
    /// Upward, a key can only Backspace back through the fold
    /// ([`Ribbon::hand_changed_row`]); every other upward caret under an echo
    /// is an app homing, caught at every column
    /// ([`a_parking_pass_is_caught_at_every_column_it_can_be_sampled_at`]).
    ///
    /// The re-home needs a run to land on: the pen's row must still carry a
    /// band cell, leaving or not ([`Ribbon::erase`] stamps `retract_at` on
    /// the whole suffix, so a held Backspace can leave only retracting cells,
    /// [`a_parking_pass_after_the_row_was_backspaced_empty_still_finds_the_hand`]).
    /// No fixture reaches that clause's absence; it keeps the function from
    /// inventing a cell on a row the ribbon sees nothing on. The upward
    /// exemption is pinned by `ribbon_row_hold_conformance::
    /// typing_and_erase_renew_only_their_own_row_and_conform`.
    #[must_use]
    fn typed_landing(&self, caret: (u16, u16), cells: u16, cols: u16) -> (u16, u16) {
        // A pen the hand has never held names nothing.
        let Some(pen) = self.pen else {
            return caret;
        };
        // A key that lays nothing has no landing to argue about
        // ([`Ribbon::lay`] returns on `n == 0`).
        if cells == 0 {
            return caret;
        }
        // The pen's own row is not exempt from the fold question: `lay` does
        // not stop at the row, and a same-row caret whose leftmost cell
        // underflows the pane's first column walks onto the end of the row
        // above. That is readline's CR park: bash repositions with a bare CR
        // and reprints, so mid-redraw the caret passes through the prompt's
        // columns on the pen's own row under a `Typed` licence
        // ([`a_key_sampled_at_readlines_cr_park_stays_on_the_hands_own_row`]).
        //
        // So the question is asked uniformly, `caret.0 >= pen.0`. A same-row
        // landing that folds nothing has `fold_top_row == caret.0` and returns
        // as is; only the folding case changes. Re-homing every same-row caret
        // (`>`) instead breaks the seam and witness laws, and guarding inside
        // [`Ribbon::lay`] would forbid a genuine wrap back to the pen's row.
        if caret.0 >= pen.0 {
            // Downward is the hand's own direction, unless `lay`'s fold would put
            // the cells on a row that is neither the landing's nor the pen's
            // ([`Ribbon::fold_top_row`]).
            let top = self.fold_top_row(caret, cells, cols);
            if top == i32::from(caret.0) || top == i32::from(pen.0) {
                return caret;
            }
        } else if Self::hand_changed_row(pen, caret) {
            // Upward the hand can carry the caret exactly one row, onto that row's
            // right end (a Backspace back through the fold), which is the
            // only way up there is ([`Ribbon::hand_changed_row`]).
            return caret;
        }
        // The re-home needs a cell on the pen's row, leaving or not (see the
        // doc).
        if !self.cells.iter().any(|c| c.row == pen.0) {
            return caret;
        }
        (pen.0, pen.1.saturating_add(cells.max(1)))
    }

    /// The pane the fold wraps at, as [`Ribbon::lay`] resolves it: the
    /// Focused pane's own first column and width, or the grid's when no
    /// pane is set. One spelling, read by [`Ribbon::fold_top_row`] and by
    /// nothing else.
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
    #[must_use]
    fn fold_top_row(&self, caret: (u16, u16), cells: u16, cols: u16) -> i32 {
        let (pane_col0, pane_cols) = self.fold_pane(cols);
        let left = i32::from(caret.1) - i32::from(cells) - pane_col0;
        i32::from(caret.0) + left.div_euclid(pane_cols).min(0)
    }

    /// The hand's own fold: an echoed move exactly one row down — a wrap, or
    /// a composer's next continuation row. [`Ribbon::leave_row`]'s one
    /// non-abandoning shape.
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
        // A fold is not a departure but a flow. Wrapping is the same input
        // continuing, its glyphs still on glass one row up, so the row it
        // wrapped off is not abandoned. The row the hand left downward by
        // typing is marked [`Cohort::flow`] ([`Ribbon::flow_row`]): from its
        // own last key it drifts toward the fold point (the row's kept right
        // end) as one band, settling and fading far end first, gone by
        // [`FLOW_TOTAL_S`] and never brighter than it was.
        //
        // No key on the new row renews the old one (`Ribbon::place`'s
        // `OnlyOwnerRenews` row clause). A hand back on the row re-wets it: a
        // key laid into it, or for a shell's Backspace back through the fold a
        // hold ([`Ribbon::hold`]); the hand's arrival takes the row back
        // ([`Ribbon::take_row_back`]) whichever tick the echo lands on.
        //
        // A real departure still abandons: a Return, an arrow's row change, a
        // program's cascade, a Backspace back through the fold (the row it
        // leaves); every row change that is not one row down under the hand's
        // own echo.
        //
        // What the wrap moved goes with its text and nothing else does: a
        // composer's re-wrap takes the row's last word and the eaten space to
        // the new row, the span [`Ribbon::begin_relocate`] re-lays there. Those
        // `w + 1` cells retract here and arrive on the new row on the walk they
        // had ([`Ribbon::relay_origin`]), colour and distance. A plain terminal
        // fold lands on the pane's first column, moves no glyph, and takes
        // nothing.
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let w = self.moved_word_len(from, at);
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
            // The input continues onto the new row: arm the seam.
            self.arm_seam(to.0, at);
            if re_wrapped && w > 0 {
                let moved = (w + 1).min(from.1.saturating_sub(pane_col0));
                self.retract_span(from.0, from.1 - moved, moved, at, RETRACT_FADE_S);
            }
            self.flow_row(from.0, at);
        } else {
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
            self.begin_relocate(to, w, at, origin);
        }
    }

    /// The fold flow begins: the hand left `row` downward by typing (a shell
    /// fold, a composer re-wrap, [`Ribbon::leave_row`]), or the follow pass
    /// carried the run up with its text ([`Ribbon::translate_run`]). Every
    /// typed cohort on it still laying or in grace is marked [`Cohort::flow`]
    /// at `at`. A cohort past its grace stays on its swoosh (restarting from
    /// full would brighten it); an abandoned cohort keeps its rewound clock; a
    /// wake expires on its own life.
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

    /// A composer's same-row re-anchor: a typed move landing left of its origin
    /// on the same row with no erase behind it (Claude Code's bottom-pinned box
    /// growing up, Codex's rewrite). The old line's cells at and past the
    /// landing drain into the caret farthest first within [`RETRACT_DUR_S`],
    /// and the moved word is relaid where the caret reveals it, on the walk it
    /// had ([`Ribbon::relay_origin`]). Reached only from [`RE_ANCHOR_MIN_CELLS`]
    /// back.
    ///
    /// Claude Code's box growth repaints the composer one row higher without a
    /// scroll and moves the caret `(28,89) → (28,6)`. The follow pass
    /// ([`super::Engine::follow_rows`], run before this move is replayed) has
    /// already carried the run up with its glyphs ([`Ribbon::translate_run`]),
    /// so what this retract finds past the landing is exactly the moved word's
    /// old cells.
    ///
    /// A home is not a relocation (`a_shell_home_relays_no_word_onto_the_prompt`):
    /// readline's `^A` reprints the line and parks the caret at the input's
    /// first column under a `Typed` licence. Nothing moved, and
    /// [`Ribbon::moved_word_len`]'s two witnesses (the program's caret and the
    /// hand's last Space) would span into the prompt. So the relay must fall
    /// inside the hand's own ink on this row ([`Ribbon::hand_floor`]); a span
    /// starting left of the floor relays nothing and leaves nothing pending.
    /// The floor applies here only: a composer's wrap relays onto a new row
    /// where the hand has no floor yet.
    fn re_anchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        let n = from.1 - to.1;
        let w = self.moved_word_len(from, at);
        let held_after = self.space_held_after_the_wrap(from);
        // The origin is read before the retract stamps the word's cells.
        let origin = self.relay_origin(from, w);
        // A redraw that carried more than its wrap key: a stalled wrap brings
        // every key pressed meanwhile in the same repaint, so the moved text can
        // start left of the echoed word (`w`). The landing row's sample says
        // where it starts ([`Ribbon::note_rewrap_start`]); if further left, the
        // whole span to the landing is relaid as one run on the word's walk.
        let start = self
            .rewrap_start
            .take()
            .filter(|&(f, t, _)| f == from && t == to)
            .map(|(_, _, c)| c);
        // One relay per split ([`SplitOff`]): a later one reads its own.
        self.split_off = None;
        // The licensed backward re-anchor drains the old line toward its
        // landing
        // (`a_re_anchor_onto_the_pane_s_first_column_lights_nothing_in_the_pane_beside_it`):
        // the stale cells leave on the retract's clock whatever the app
        // rewrites there.
        self.retract_suffix(to.0, to.1, n, at, RETRACT_DUR_S);
        self.reseat_spaces(held_after, to);
        self.arrived = None;
        // The span `begin_relocate` would lay is `[landing − 1 − w, landing −
        // 1)`. An unknown floor bounds nothing.
        let relays_off_the_hand_s_ink = self
            .hand_floor_on(to.0)
            .is_some_and(|floor| to.1.saturating_sub(1).saturating_sub(w) < floor);
        if relays_off_the_hand_s_ink {
            return;
        }
        // …and what it lays takes the walk the word had (`relay_origin`,
        // read above, before the retract stamped the word's cells).
        if let (Some(start), Some(glyph)) = (start, to.1.checked_sub(1))
            && start < glyph.saturating_sub(w)
            && self.hand_floor_on(to.0).is_none_or(|floor| start >= floor)
        {
            self.relocate = None;
            self.relay_word(to.0, start, to.1, at, origin);
            return;
        }
        self.begin_relocate(to, w, at, origin);
    }

    /// The engine's row sample says the text the same-row re-anchor
    /// `from → to` relays starts at column `start` of `to`'s row. Read by that
    /// move's [`Ribbon::re_anchor`] only; any other clears it.
    pub(super) fn note_rewrap_start(&mut self, from: (u16, u16), to: (u16, u16), start: u16) {
        self.rewrap_start = Some((from, to, start));
    }

    /// Correct a Space's cell from an exact ordered key batch and its licensed
    /// same-row sweep. The engine alone has both witnesses; a final frame
    /// caret cannot locate an earlier Space in a coalesced echo. Metadata
    /// only: this neither lays cells nor changes their births or clocks.
    pub(super) fn observe_space_cell(&mut self, row: u16, col: u16) {
        self.note_space((row, col));
    }

    /// Remember a typed Space's cell, keeping the one it replaces
    /// ([`Ribbon::prev_space`]). A correction of the same Space's cell
    /// (the same row, the cell the engine's sweep says it really took)
    /// keeps the Space before it rather than becoming it.
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
    fn word_space(&self, from: (u16, u16)) -> Option<(u16, u16)> {
        match self.last_space {
            Some(cell) if cell == from => self.prev_space,
            other => other,
        }
    }

    /// Whether the last Space is the one held after the wrap key at the
    /// stale mirror ([`Ribbon::word_space`]): its real cell is the landing
    /// the wrap's move reveals, where the app's repaint put the caret.
    fn space_held_after_the_wrap(&self, from: (u16, u16)) -> bool {
        self.last_space == Some(from)
    }

    /// After a re-anchor or a fold consumed the Space bookkeeping: a Space
    /// held after the wrap key is re-seated at the landing `to`, the cell
    /// its echo will lay, so the next wrap measures its word from it; every
    /// other Space is spent with the row it was typed on.
    fn reseat_spaces(&mut self, held_after: bool, to: (u16, u16)) {
        self.prev_space = None;
        self.last_space = held_after.then_some(to);
    }

    /// Replay the `Move` the seam judged a soft-wrapped caret ([`SoftWrap`]):
    /// the verdict stands for exactly this one event.
    pub(super) fn soft_wrap_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.soft_wrap = Some(sw);
        self.on_event(ev, at, ctx);
        self.soft_wrap = None;
    }

    /// Replay the `Move` the seam judged a lifted word ([`Lift`]): the
    /// verdict stands for exactly this one event.
    pub(super) fn lift_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, lift: Lift) {
        self.lift = Some(lift);
        self.on_event(ev, at, ctx);
        self.lift = None;
    }

    /// Replay the `Move` whose echo the seam saw carry the soft-wrapped word
    /// `sw` off its row ([`Ribbon::settle_carry`]): the verdict stands for
    /// exactly this one event.
    pub(super) fn reflow_move(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, sw: SoftWrap) {
        self.reflow = Some(sw);
        self.on_event(ev, at, ctx);
        self.reflow = None;
    }

    /// Replay the `Sweep` the seam named the revive of a retired typed run
    /// ([`Revive`]): the run's walk stands for exactly this one event.
    pub(super) fn revive_sweep(&mut self, ev: &Event, at: Instant, ctx: &Ctx<'_>, rv: Revive) {
        self.revive = Some(rv);
        self.on_event(ev, at, ctx);
        self.revive = None;
    }

    /// The soft-wrapped word waiting for the key that reflows it, if any —
    /// what the seam reads the glass for (`CursorGlow::carried_word_left`).
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

    /// The soft-wrapped word comes down with its text. When the first
    /// same-row forward typed echo from a soft-wrapped caret's landing
    /// advances past the word the wrap key ended plus its own glyph, the app
    /// erased that partial word from the end of the origin row and rewrote it
    /// from the indent. The press budget refuses that hop, so the word's lit
    /// cells leave the origin row and are relaid from the indent on the walk
    /// they had ([`Ribbon::relay_origin`]), with the reflowing key's own glyph
    /// beside them. A forward echo that did not carry the word, or one later
    /// than [`RELOCATE_PATIENCE_S`], drops the carry.
    ///
    /// A hop past the word is also what a burst or paste after a short word
    /// looks like, so the relay needs the seam's verdict for this very `Move`
    /// ([`Ribbon::reflow_move`]): the word's columns on its row are blank on
    /// the glass (`CursorGlow::carried_word_left`).
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

    /// Cells of the word the app moved off `from`'s row: everything the hand
    /// typed on the row up to the origin column since the later of its last
    /// typed Space ([`Ribbon::word_space`]: not one held after the wrap key)
    /// and where it last arrived ([`Ribbon::arrived`]). `0` with neither on
    /// the row (a line that is one word wraps mid-word: the app moved
    /// nothing).
    ///
    /// An arrival inside the hand's own live run (an arrow back over the
    /// word being typed, a typo fix) does not start a new run: the arrival
    /// reaches back over the contiguous live typed cells left of it
    /// ([`Ribbon::typed_live`]; a wake's cells are no run), still bounded by
    /// the typed Space. An arrival into unlit program text stands
    /// (`tests/wrap_code_reflow.rs`).
    fn moved_word_len(&self, from: (u16, u16), at: Instant) -> u16 {
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
    fn typed_live(&self, row: u16, col: u16, at: Instant) -> bool {
        self.cells
            .iter()
            .any(|c| c.row == row && c.col == col && c.typing && self.alive_at(c, at))
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
            let like = self
                .cells
                .iter()
                .filter(|c| c.row == row && c.cohort == cohort.id && c.typing)
                .max_by(|a, b| a.born.cmp(&b.born))
                .copied();
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born: at,
                attack_at: at,
                life_s: like.map_or(SWOOSH_LIFE_S, |c| c.life_s),
                cov0: like.map_or(BODY_COLD_SHARE, |c| c.cov0),
                typing: true,
                retract_at: None,
                retire_at: None,
                birth_disp: like.map_or(0.0, |c| c.birth_disp),
                edge_cells: like.map_or(HOT_EDGE_CELLS, |c| c.edge_cells),
                layer: Layer::Base,
                rearm: None,
                released_at: None,
            };
            self.place(idx, cell, at, row);
        }
    }

    /// The scrub, and the line between it and the jump. A same-row keyed move
    /// (`Nav`, `Synthetic`) from `from` to `to` on `row` is a scrub when the
    /// corridor it crossed, `[min, max)`, is lit end to end by cells that are
    /// not leaving: each column owned by a live cell with light on the glass
    /// whose cohort is neither abandoned nor retracting. That is the hand
    /// moving over its own text: nothing is laid, taken over or abandoned, and
    /// the cohorts the corridor crosses are held (clock renewed to the hop,
    /// phase back to laying, retract target released), as
    /// [`Ribbon::hold_phrase`] holds an editing hand's phrase. The band leaves
    /// through its swoosh after the hand's last act on it, a hop included.
    /// Returns whether the move was a scrub.
    ///
    /// This predicate is the whole boundary between the two laws:
    ///
    /// * Inside the hand's own lit run: a scrub (Alt-B/Alt-F over the words
    ///   just typed, Ctrl-A/Ctrl-E between the ends of a band that reaches
    ///   both).
    /// * Leaving the run (past its oldest cell, off its end onto dark ground,
    ///   over a cell already melting or leaving, or to another row): a jump.
    ///   [`Ribbon::wake`] lays its corridor on [`Layer::Over`] and
    ///   [`Ribbon::abandon`] retracts the [`Layer::Base`] band beneath it.
    ///
    /// The laws cannot disagree: a move inside the run has no dark ground to
    /// light and no left band to abandon; a move that leaves has both.
    ///
    /// The hold renews the clock only (`alive_at`, `phase`, `retract_col`):
    /// it does not re-price `Cohort::grace_s` or raise [`Cell::life_s`] as
    /// [`Ribbon::hold`] does for a typed key, since a navigation mints no
    /// note. A key that later lands in or beside the band takes it back
    /// through [`Ribbon::join_cohort`] and [`Ribbon::place`].
    ///
    /// Two guards:
    ///
    /// * A cell past its melt opening is already going out. Renewing its
    ///   cohort would snap a melting stub back to full body in one frame, so
    ///   the corridor counts as lit only by cells not yet melting. The typed
    ///   band never trips this: its life is at least [`swoosh_life_s`] of its
    ///   phrase's grace.
    /// * The hold reaches exactly the cohorts that own the corridor's columns
    ///   (the newest un-leaving cell of each, on either layer). A cohort
    ///   shadowed beneath them, such as an abandoned band under a jump's
    ///   wake, stays on its clock.
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
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                return false;
            };
            let cell = self.cells[i];
            if self.env_of(ctx, &cell) <= 0.0 {
                return false;
            }
            let Some(coh) = self.cohorts.iter().find(|k| k.id == cell.cohort) else {
                return false;
            };
            if coh.leaving_at(at) {
                return false;
            }
            // The melt guard: `spent` on the clock `env_of` reads
            // (`live_since`), against the instant the cell's own melt opens.
            let spent = at
                .saturating_duration_since(cell.born.max(coh.alive_at))
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
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                continue;
            };
            let id = self.cells[i].cohort;
            if let Some(coh) = self.cohorts.iter_mut().find(|k| k.id == id) {
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
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

    /// The hand touched `(row, col)`: fold it into [`Ribbon::hand_floor`]. A
    /// new row replaces the floor; the same row keeps the leftmost. Callers
    /// are only places the hand acted (a typed glyph, a relocation it made,
    /// the caret an erase was replayed against), never program output.
    fn touch_hand(&mut self, row: u16, col: u16) {
        self.hand_floor = Some(match self.hand_floor {
            Some((r, c)) if r == row => (row, c.min(col)),
            _ => (row, col),
        });
    }

    /// The leftmost column the reach may take on `row`, or `None` when the
    /// hand has not been seen there — an unknown floor bounds nothing.
    fn hand_floor_on(&self, row: u16) -> Option<u16> {
        self.hand_floor.and_then(|(r, c)| (r == row).then_some(c))
    }

    /// The hand's floor ([`Ribbon::hand_floor`]) as `(row, col)`, or `None`
    /// when no row has seen the hand yet. The meteor's shower reads it
    /// (`meteor::Meteors::set_hand_floor`) under the same law as the reach:
    /// nothing the hand did not put there goes left of where the hand has
    /// been on that row.
    ///
    /// Named `_cell` so `[`Ribbon::hand_floor`]` stays an unambiguous link to
    /// the field.
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
        // A new line's first cell ([`Ribbon::fresh_line`]): the first cell
        // laid at or after the hand started a new line, or off the row the
        // hand left whatever its key's clock ([`FreshLine::takes`]), mints on
        // the walk's colour at distance zero and spends the gate whatever
        // cohort it lands in. An older key's cell on the row the hand left is
        // the old line's text: it neither restarts nor spends it.
        //
        // Nor does the old line re-laid: a cell joining a run born before the
        // gate, over a column that run already owns, is the old line's last
        // glyph replayed (`Engine::replay_events` lays `caret − 1` before a
        // Return's move). It leaves the gate armed for the key's own glyph.
        //
        // Nor the revive ([`Revive`]): its cells are the old line's unchanged
        // text laid back on its walk.
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
        // The walk is a function of position (C2, §4): the cell takes its
        // column's stop in its cohort, from the cohort's immutable origin, so
        // a cell retyped after a backspace leaves no colour seam.
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

    /// The fold's last glyph seats the row below. A plain terminal fold's two
    /// glyphs arrive on one tick out of walk order: the key printed at the
    /// pane's last column is held under xterm's pending wrap, so its glyph
    /// comes with the fold's sweep after the next key's `Typed` already laid
    /// the new row's first cell, continuing the row above from one column
    /// short. Both sides of the fold would then share a stop.
    ///
    /// So when a lay makes cohort `idx` reach the pane's last column, a
    /// cohort on the next row whose `(t0, d0)` is exactly `walk_at(col)` of
    /// this one is re-seated one step on and its cells re-stamped. A
    /// successor anchored `k` columns into the row (the fold key's and the
    /// next key's echoes landing on one frame) is re-seated at
    /// `walk_at(pane_col1 + k)` under the same exact match. On the fold's own
    /// tick this runs before any plan, so the stale stop is never drawn; a
    /// successor that continued from the true end is left alone
    /// (`tests/abandoned_ribbon.rs`).
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
            // The hand is on this cohort now, whatever minted it; a flowing
            // row the hand came back to (a Backspace back through the fold)
            // is re-wet: the flow is cleared with the frozen target, and the
            // hold below takes the clock.
            coh.wake = false;
            if coh.flow.take().is_some() {
                coh.retract_col = None;
                coh.flow_drift = 0.0;
            }
            // One finger, one phrase, on the hand's row. A live key holds
            // every un-abandoned cohort on the hand's row in its laying phase;
            // a row the hand left leaves on its own clock (`leave_row`). This
            // is model-checked (`aterm-spec` `ribbon_row_hold_model`,
            // invariant `OnlyOwnerRenews`).
            //
            // Within the row, the hold renews only cohorts of the open phrase
            // (minted into it, or re-assigned when the hand resumes a closed
            // phrase's line). A closed phrase's cohort on the same row keeps
            // its own clock. The cohort the cell was laid into is always
            // refreshed: a fold's own sweep, dated at the key, lands on the
            // row the hand is leaving and is that row's last key.
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

    /// The wake (R5): lay the corridor a same-row move just crossed on `row`,
    /// from `from` to the landing `to`, newest at the landing. The landing
    /// cell is included, so the caret fill, the train's phase-lock
    /// (`field_at(landing)`, §6.4) and the bed under the caret read one walk.
    /// New cells are born along the flight ([`flight_ms`] of the span: cell
    /// `j` from the origin at `T · j / n`), live `life_s`, and number at most
    /// [`WAKE_MAX_CELLS`] counted back from the landing.
    ///
    /// The wake's cohort takes the band's own walk origin
    /// ([`Ribbon::wake_origin`]: `anchor_col`, `t0`, `d0`), so `t_at(col)`
    /// matches the band at every column (C2).
    ///
    /// The wake is an overlay (v3 §2.3/§2.7, K1–K4): every wake cell is
    /// [`Layer::Over`] and the band's [`Layer::Base`] cells are never taken
    /// over, re-priced or moved. With `takeover` (a jump) the corridor is its
    /// own cohort laid over every column it crosses. Over a live or leaving
    /// band cell the wake cell is born on the flight clock at the band's own
    /// level ([`Cell::rearm`]) and at least its price, so the hand-off has no
    /// attack and no dip; [`Ribbon::abandon`] then drains the band beneath
    /// behind the meteor. A cell an earlier wake laid is re-owned in place
    /// (K4): same birth and price, re-armed from its current level, on this
    /// cohort's clock, so a Ctrl-A/Ctrl-E ping-pong holds one corridor and
    /// one wake cohort on the row.
    ///
    /// The cap binds at the far edge (v3 §2.10): light the corridor keeps
    /// past the cap moves the far edge out to it and the drained columns
    /// between are laid new, so a corridor is never two islands.
    ///
    /// Without `takeover` (a hop) a column any live light owns is left as it
    /// is, on either layer: a hop re-owns nothing, since renewing a melting
    /// stub would snap it to full (the same case [`Ribbon::scrub`]'s melt
    /// guard refuses). Light whose band is leaving ([`Cohort::leaving_at`])
    /// is not live, and the hop covers it from its level as a jump does. The
    /// hop's new cells join the row's adjacent wake cohort or mint one on the
    /// same origin ([`Ribbon::join_wake_cohort`]), never a typed word's.
    ///
    /// Every wake cell is `typing: false`: the one-finger hold in
    /// [`Ribbon::place`] and the momentum spine read that flag
    /// (`a_wake_is_never_typing_and_holds_no_other_cohort`).
    ///
    /// Returns the minted cohort's id (the takeover arm) so the abandon can
    /// spare it.
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
        // The flight clock (K3): a jump's `T = flight_ms(cells)`, the number
        // the meteor head, the bell and the pin fly on; a hop takes the
        // flight floor. Cell `j` from the origin is born at `t0 + T · j / n`.
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
        // The cap binds at the corridor's far edge, never inside it (v3
        // §2.10). `reach` is the farthest column past the cap where this
        // corridor keeps light anyway (an `Over` cell it re-owns, or band
        // light it covers); new light is laid from the cap out to it, so the
        // corridor is one interval (K2). With nothing kept past the cap,
        // `reach` is 0 and the cap binds as usual.
        //
        // Past the cap, reach and lay read one bar, the glass census's own
        // ([`Ribbon::corridor_keeps_glass_light`]): a residue the eye cannot
        // see neither moves the edge nor is covered, so a row reading as
        // dark is never painted end to end.
        let reach = (WAKE_MAX_CELLS..=span)
            .rev()
            .find(|&k| {
                let col = if leftward { to + k } else { to - k };
                self.corridor_keeps_glass_light(ctx, row, col, at)
            })
            .unwrap_or(0);

        // Far end first, landing last: the pool is append-ordered and the
        // index is last-writer-wins. The landing is the head by `head_col`'s
        // wake clause (the caret on a wake run's first cell), which puts the
        // hot edge by the caret after a Ctrl-A; birth order does not decide
        // it.
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
            // A hop leaves live light exactly as it is, on both layers:
            // renewing a cell inside its expiry melt is light no key bought.
            // This is the hop-side half of `Ribbon::scrub`'s melt guard.
            // Re-owning is the jump's law (K4).
            //
            // Live means not leaving: band light whose band is leaving
            // ([`Cohort::leaving_at`]) would go out on the band's clock while
            // the hop's cells stay lit, splitting the corridor, so the hop
            // covers it from its current level as a jump does. No level floor
            // can replace this: the drain empties neighbouring columns a few
            // ms apart, so any floor splits the corridor at some pause.
            let base_live = base_cov > 0.0 && !base_leaving;
            if !takeover && (base_live || over_live.is_some()) {
                continue;
            }
            let (born, attack_at, cov0, rearm) = match over {
                Some(i) => {
                    // The reversal re-owns (K4, M1): an earlier wake's cell is
                    // taken into this wake in place (same birth and price,
                    // envelope re-armed from its current level so it rises
                    // through `edge-in`), on this cohort's clock. The old
                    // cohort keeps only uncovered cells; emptied, it retires.
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
    fn corridor_column(
        &self,
        ctx: &Ctx<'_>,
        row: u16,
        col: u16,
        at: Instant,
    ) -> (Option<usize>, f32, bool) {
        let over = self
            .cells
            .iter()
            .rposition(|c| c.row == row && c.col == col && c.layer == Layer::Over);
        let (base_cov, base_leaving) = self
            .cells
            .iter()
            .rposition(|c| c.row == row && c.col == col && c.layer == Layer::Base)
            .map_or((0.0, false), |i| {
                let c = self.cells[i];
                let env = if c.leaving() {
                    0.0
                } else {
                    self.env_of(ctx, &c)
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
    fn corridor_keeps_glass_light(&self, ctx: &Ctx<'_>, row: u16, col: u16, at: Instant) -> bool {
        let (over, base_cov, _) = self.corridor_column(ctx, row, col, at);
        // The lay re-owns an `Over` owner whatever its clock (K4), so the
        // corridor must reach it or leave a gap before an island.
        over.is_some() || Self::reads_as_glass_light(base_cov)
    }

    /// Whether a coverage share (`cov0 · env`, as [`Ribbon::corridor_column`]
    /// reports) composites to something the glass census reads as lit:
    /// [`STATUS_LIT_COV`] of [`UNDER_COV_CAP`].
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

    /// The walk origin a wake takes on `row`, as `(anchor_col, walk)`. The
    /// row's freshest cohort inside the chain window (abandoned, retracting or
    /// not) shares its origin outright, `(anchor_col, t0, d0)`, so `t_at(col)`
    /// matches the band at every column and continues past both its ends.
    /// Runs on one row share one walk ([`Ribbon::join_cohort`]), so a corridor
    /// over several is seamless (`tests/wake_and_landing_walk_origins.rs`).
    ///
    /// With no cohort on the row, the freshest anywhere is continued from one
    /// past its rightmost column (its colour only), anchored at `landing` at
    /// distance zero ([`WalkPos::restarted`]); red when nothing is laid. The
    /// dark row restarts the pace, as [`Ribbon::fresh_line`] does for a typed
    /// key, which is also what `meteor::landing_field` walks there.
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
        });
        self.cohorts.len() - 1
    }

    /// The cohort this cell joins: one on the same row, adjacent to or
    /// containing `col`, still inside the chain window. Otherwise a new one
    /// whose walk continues the freshest live cohort's from one past its
    /// rightmost column, colour and distance ([`Cohort::d0`]), so a wrap fold
    /// and a rebirth cross without a seam; a cold start re-anchors at red on
    /// the fast leg.
    ///
    /// One row, one walk (`RAINBOW-KITTY-V2.md` §42): when the freshest cohort
    /// is a run still standing on this row ([`Ribbon::stands`]) and the hand
    /// has not started a new line, the new cohort takes that run's walk by
    /// position (`walk_at(col)`, anchored at `col`), so the whole row reads
    /// one function of position and a corridor over both runs
    /// ([`Ribbon::wake_origin`]) is seamless.
    ///
    /// `new_line` (the hand started a new line since the last typed cell,
    /// [`Ribbon::fresh_line`]; the relay passes `false`) keeps the colour and
    /// drops the distance ([`WalkPos::restarted`]) on both continuation arms.
    /// A join or rejoin is not a mint and is unaffected.
    ///
    /// A dead run is not a band: neither arm joins a cohort with no standing
    /// cell (every cell melted by the content witness, or draining after a
    /// line kill), whose bounds still span the row until pruned. The key
    /// mints instead, continuing the dead run's end. The dead cells keep
    /// their own clocks; a shared column has two owners of two cohorts until
    /// they go ([`Ribbon::place`] keeps one owner per cohort). A run the
    /// follow pass split off a composer's caret row is dead the same way once
    /// its moved word's old cells melt (`Witness::landed_goes_with_its_run`).
    ///
    /// The hand's own erase is not death: a run Backspaced or `^W`-ed away is
    /// still its band while it drains, so a retype inside the fade rejoins it
    /// on its stops (C2); once pruned, the next key continues the erased end
    /// from `last_walk`.
    ///
    /// The revive is not a retype (§43, [`Revive`]): a cell of the seam's
    /// re-lay of a retired run that would mint takes the run's own origin.
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
            return i;
        }
        // The revive mints on the run's own walk (§43, [`Revive`]): a cell of
        // the seam's re-lay that joins no live band takes the origin the
        // run's cells had, so each column gets back its old stop
        // (`tests/revived_line_keeps_its_stops.rs`). A key typed past the
        // fade still continues the erased end, and a new line's gate is
        // neither read nor spent here ([`Ribbon::lay_cell`]).
        if let Some(rv) = self.revive.filter(|rv| rv.covers(row, col)) {
            return self.mint_cohort(row, col, rv.origin.anchor_col, rv.origin.walk(), at, false);
        }
        // The continuation carries the walk's distance with its colour
        // ([`Cohort::d0`], through `walk_at`), so the new cohort walks on at
        // the pace already reached instead of re-running the fast leg.
        let fresh = |t: Instant| at.saturating_duration_since(t).as_secs_f32() <= CHAIN_GAP_MAX;
        // The freshest cohort, a tie to the walk's farther end ([`freshest`]),
        // never pool order.
        //
        // One row, one walk: when that cohort is a live run on this row (a
        // cell still standing, [`Ribbon::stands`], abandoned or not) and the
        // hand has not started a new line, the new cohort takes its walk by
        // position, `walk_at(col)` anchored at `col`, left of its start or
        // past its end alike (`tests/wake_and_landing_walk_origins.rs`). The
        // run typed after a program's jump therefore continues the gap's walk
        // from where the older run ended. A dead run, a run on another row, a
        // retired one (`last_walk`) and a new line keep the continuation
        // below.
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
                // A finished swoosh's walk continues too (`Cohort::t0`'s 5 s
                // continuation; `retire` drops the cohort with its cells).
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

    /// The hand takes the wake it bridged. A word hop across dark ground (the
    /// suffix a mid-line edit retired) lays a wake cohort that touches the
    /// typed band; without this fold a key at the landing would leave two
    /// cohorts under one contiguous band, each with its own swoosh, and a
    /// hole opens between them as they drain (`tests/codex_particle_replay.rs`).
    /// The same holds the other way round: a landing inside the typed
    /// cohort's bounds (which never shrink when the witness retires cells)
    /// would leave the wake at the landing un-held by the hand's keys.
    ///
    /// So a typed key folds every wake on its row that contains or abuts the
    /// cell it lays into the typed band that key belongs to (the cohort it
    /// joined when typed, else a typed cohort the wake touches). The wake's
    /// cells take the band's id and its walk at their column (unchanged, as
    /// the wake was minted on the band's origin, [`Ribbon::wake_origin`]),
    /// are priced like the band's newest glyph as [`Ribbon::relay_word`]
    /// prices a relocated word, the band's bounds grow over them, and the
    /// wake cohort is gone: one band, one drain, so
    /// [`Ribbon::retract_suffix`]'s contiguity holds through the swoosh.
    /// Where the wake covered the band's spent resident cells, the newer
    /// writer wins ([`Ribbon::heal_seams`]), keeping one owner per column.
    ///
    /// A band on its way out is not taken back by a key in a wake: the fold
    /// goes only into a typed cohort still laying or in its grace, since the
    /// hold that follows every lay would bring a reaching, retracting or
    /// fading band back to full in one frame. Such a key keeps its wake.
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
            let wake_cols: Vec<u16> = self
                .cells
                .iter()
                .filter(|c| c.cohort == wake.id)
                .map(|c| c.col)
                .collect();
            // A released band cell the wake shadows is replaced by a newer
            // writer, and the wake's own release custody goes with its cells
            // ([`Ribbon::custody_replaced`], [`Ribbon::carry_release`]).
            for part in band.release_parts.iter().flatten() {
                let shadowed = self
                    .cells
                    .iter()
                    .filter(|c| {
                        c.cohort == band.id
                            && c.row == row
                            && wake_cols.contains(&c.col)
                            && c.released_at == Some(part.stamp)
                    })
                    .count();
                self.custody_replaced(b, Some(part.stamp), shadowed);
            }
            self.cells
                .retain(|c| !(c.cohort == band.id && c.row == row && wake_cols.contains(&c.col)));
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

    /// A real jump that leaves the run abandons the band (a same-row jump
    /// whose corridor is lit end to end is a scrub and never reaches here,
    /// [`Ribbon::scrub`]). Every cohort still in its grace or reach, except
    /// the jump's own wake `keep`, is sent straight into its retract by
    /// rewinding `alive_at` to [`RETRACT_START_S`] ago, so the light leaves
    /// continuously from the level it has (`spend` starts at exactly `1.0`).
    /// A cohort already retracting keeps its clock. Under the corridor the
    /// wake already covers the band's cells ([`Ribbon::wake`]), so what
    /// retracts here is the light beyond it.
    ///
    /// `toward` pre-sets the retract's target and drain order. A program's
    /// jump takes the band back into the hand's last column rather than
    /// wherever the program parked the cursor, and `rejoinable` lets the
    /// hand's next key take the band back ([`Ribbon::join_cohort`]). A
    /// same-row jump the hand made passes `(landing, leftward)`: the landing
    /// is the frozen target (R1) and the drain follows the meteor, origin end
    /// first, so no band cell starts spending before the corridor cell above
    /// it is born (cell `j` is born at `T · j / n ≤ 0.12 · j / n` and starts
    /// spending at `0.40 · j / n`). A row change passes `None` and the per-row
    /// target law decides at the retract's first frame.
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

    /// Retire specific live cells on the fast melt. Every live cell that is
    /// one of `cells`, named by identity `(row, col, born)`, takes
    /// [`Cell::retire_at`] `= at`: it spends to exactly zero over
    /// [`RETIRE_MELT_S`] on the retract's alpha law ([`super::timing::spend`]),
    /// its attack frozen so it never brightens ([`Ribbon::env_of`]), and
    /// leaves the pool on the first plan past the melt ([`Ribbon::retire`]).
    /// The host's content witness calls this for a cell whose glyph it saw
    /// replaced (`Engine::witness_rows`), live or already retracting (the
    /// melt multiplies into the retract's envelope). A cell over ink that is
    /// still there is never touched, so a prompt redraw that puts the same
    /// text back changes nothing (D2).
    ///
    /// By identity, because a position can hold more than one live cell (one
    /// owner per cohort, [`Ribbon::place`]; a key typed over an abandoned
    /// cohort's cell mints a second). The birth is the one thing two cells
    /// at a position never share, so the key's own new cell survives.
    ///
    /// Returns how many cells were newly stamped. A cell already leaving
    /// keeps its clock, so a repeated witness cannot restart a melt.
    /// `O(pool × cells)`; a retirement is an event, not a frame.
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

    /// Release specific live cells to the swoosh: the content witness's other
    /// verdict, that the glyph under each of `cells` (by identity, as
    /// [`Ribbon::retire_cells`]) has gone and nothing replaced it (a composer
    /// cleared on Enter, a Ctrl-U, a cleared line). Such light leaves as a row
    /// the hand left does, not on the [`RETIRE_MELT_S`] cut: each named cell's
    /// cohort is abandoned into its retract ([`Ribbon::abandon_row`]'s law,
    /// `alive_at` rewound to [`RETRACT_START_S`] ago), drawn into the hand
    /// farthest-first over `RETRACT_DUR_S + RETRACT_FADE_S`, the target left
    /// for `advance_swoosh`. Not rejoinable: the text is gone. A cohort
    /// already abandoned keeps its clock and only loses its rejoin.
    ///
    /// Returns how many named cells were resident and not already leaving, so
    /// `ribbon_retired=` counts a band that went because its text went. The
    /// cells stay owners of their columns for the retract's span, so the
    /// witness can still name them to `retire_cells` later; it keeps their
    /// records and `Engine::witness_rows` counts each cell once.
    ///
    /// A release naming only part of a cohort's standing cells takes exactly
    /// that part: the named cells are stamped onto the retract at `at`
    /// (`retract_at` and [`Cell::released_at`]) and counted, the cohort's
    /// clock is untouched, and a [`ReleaseClock`] with `partial` set lets the
    /// identical redraw a frame later lift it again
    /// ([`Ribbon::restore_releases`]).
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
            // A release of part of a run takes exactly that part. Abandoning
            // the whole cohort would drain every standing letter for one cell
            // an app's repaint blanked; leaving the part standing would keep
            // light over a cleared suffix ahead of the hand. So the part
            // leaves as an erase's suffix does and the cohort's clock is
            // untouched; `released_at` lets the identical redraw a frame later
            // lift it ([`Ribbon::restore_releases`]). A release naming every
            // standing cell is the text gone whole, and the cohort leaves.
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
                // Each part in its own custody: a part released while an
                // earlier one is still out is held beside it, so a torn read
                // under an insert cannot leave the newest letters with no
                // clock to lift them. Each part is lifted when its glyphs are
                // back ([`Ribbon::restore_releases`]).
                //
                // An abandoned cohort holds its parts too: lifting a part's
                // stamps hands the cells back to the retract the cohort is
                // already on, so the swoosh is unchanged and a torn read of a
                // leaving band does not take the torn cells out early.
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

    /// Whether a release still owns what the witness found back. The witness
    /// checks the original glyph identities separately (`candidates`, sorted:
    /// `(cohort, None)` for a whole release whose text is back,
    /// `(cohort, Some(stamp))` for one partial release, [`Cell::released_at`]
    /// `== stamp`, whose glyphs are back); an explicit abandon or retirement
    /// never permits restoration through this seam. Returns the cohorts
    /// something was lifted in, sorted.
    ///
    /// A restore counts only what was released: every cell the release took
    /// must still be resident (a pruned one already left, and restoring the
    /// rest would leave its column dark between them), but a cell laid after
    /// the release neither authorizes nor refuses. `CursorGlow::tick` lays
    /// the held key's cell into the same cohort before this, and a birth time
    /// cannot tell it apart (it is dated at its press), so a partial release
    /// is counted by the cells still carrying its stamp. A whole release
    /// abandons its cohort unrejoinable, so its whole pool is the count.
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

    /// No two typed cohorts abut on a row. A key whose echo lands on the same
    /// tick as another's is replayed against the tick's landing caret and
    /// lays one past the live cohort's end; `join_cohort` refuses it, a
    /// second cohort is minted, and the sweep fills the hole into the old
    /// one. `build_runs` would then split the contiguous row at the cohort
    /// change and `plan_run` feather the seam at [`RUN_TAIL_EASE`], a visible
    /// slit. So two live typed cohorts that abut on a row are folded into
    /// one here, every frame, before the geometry is planned. The walk is
    /// continuous by construction in colour and pace ([`Cohort::d0`]), so the
    /// re-stamp below is the identity on that shape (to the ulp).
    ///
    /// A dead run is not a band to heal into: both sides must still stand
    /// ([`Ribbon::stands`]). A melting remnant (such as the composer wrap's
    /// follow split: the eaten Space and the moved word's old cells) keeps
    /// its bounds until pruned, and merging a new line into it would
    /// re-stamp the new line on the old walk
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
            let next = self.cohorts.iter().position(|b| {
                b.id != a.id
                    && b.row == a.row
                    && b.col0 == a.col1
                    && hot(b)
                    && hot(&a)
                    && self.stands(b)
                    && self.stands(&a)
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
            // One run, one origin: the survivor takes the leftmost origin
            // (the cohort with the smaller anchor) and every cell of the
            // merged run is re-stamped `t = t_at(col)` from it, so the walk
            // is one function of position across the seam (C2). The origin
            // is all three numbers, `(anchor_col, t0, d0)`: with a mixed
            // origin the computed zero would be off by the distances'
            // difference wherever the run crosses the walk's knee
            // (`a_heal_merge_takes_the_relaid_word_s_whole_origin_and_keeps_the_walk_s_pace`).
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
            // The retract's target is fixed the frame the retract begins: the
            // mark is drawn back into the hand that let go of it, or on
            // another row into its own end nearest the hand
            // ([`Ribbon::retract_target`]), never toward where the caret goes
            // next (`Cohort::retract_col`). The drain order is frozen with it.
            if phase.is_retracting() && self.cohorts[i].retract_col.is_none() {
                // Per row (v3 §2.6): a cohort on the caret's row is drawn
                // into the caret; one on a row above into its right end (the
                // fold), one below into its left end. Another row's caret
                // column never pulls a row's light sideways.
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
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
    }

    /// The plan and the index, from one sort.
    fn build(&mut self, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        self.plan.clear();
        self.runs.clear();
        // What the block leaves behind drains from the level it showed: a
        // leaving cell the attach floored last frame that is no longer under
        // the caret (a held Backspace moves the block left one cell per key)
        // has been spending under the floor, so uncovering it would drop it
        // in one frame. Its retract restarts at the uncovering and it drains
        // from full, one cell behind the block. A retract not yet begun keeps
        // its date, a content retirement keeps its clock, and a cell a fresh
        // lay shadows is left alone.
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
        // The layers (K1): a born `Over` cell owns its position over a `Base`
        // cell, whatever the band's clock; an unborn `Over` cell (flight
        // clock) leaves the band beneath on glass until it is born.
        let now = ctx.now;
        let cells = &self.cells;
        // The position's owner, by rank: a born `Over` cell, then the newest
        // `Base` cell, then an unborn `Over` cell (dark until born).
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
    fn slabs_for(ctx: &Ctx<'_>, cells: usize) -> usize {
        let prof = Self::body_profile(ctx.cfg);
        let ch = ctx.geom.ch as f32;
        // The estimate reads the settled reach below the spine (`DN_TOP_CH`),
        // not the comet's lobe at the hand: the lobe spans `BLOOM_REACH_CELLS`
        // of one run, and `ribbon_beam`'s own truncation is the backstop for
        // the rows an estimate misses. Under the flat spelling the profile's
        // `dn_ch` is `DN_TOP_CH`, so this is the number it always was.
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
        let wave = ((sp * std::f32::consts::TAU * WAVE_CYCLES) - ctx.phase * std::f32::consts::TAU)
            .sin()
            * amp;
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
        let room_below = self.pane_bot(ctx, chf)
            - (f32::from(ctx.geom.origin_y) + (f32::from(cell.row) + 1.0) * chf);
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
        let spine = (f32::from(ctx.geom.origin_y) + (f32::from(cell.row) + 1.0) * chf + wave)
            .min(self.pane_bot(ctx, chf) - dn)
            .max(ctx.geom.fx_top() as f32);
        (spine, up, dn, self.cov_of(ctx, cell, coh))
    }

    /// The cell's envelope, `0..1`: everything that opens and closes a cell's
    /// light after its birth price (the 18 ms `edge-in`, the expiry melt, the
    /// retract's `spend`, the focus ember and its rearm, and the exit
    /// swoosh's farthest-first drain). It is the one clock the body and the
    /// hot edge share: [`Ribbon::cov_of`] multiplies it into the body's
    /// request and [`Ribbon::emit_hot_edge`] multiplies the head cell's into
    /// the hairline's gain, so the hairline reaches zero with the body.
    ///
    /// Except for the two sanctioned attacks (`edge-in` at birth, and the
    /// rearm's `edge-in` on focus regain) every term is non-increasing in
    /// `now`, so "never brightens after the last keystroke" holds by
    /// construction.
    fn env_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        self.env_with(ctx, cell, self.cohorts.iter().find(|c| c.id == cell.cohort))
    }

    /// [`Ribbon::env_of`] with `cell`'s cohort already looked up — the form
    /// the plan reads, which has it once per run.
    fn env_with(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        let now = ctx.now;
        // A wake cell laid ahead of its birth (`WAKE_BORN_LAG_S`) is dark
        // until it is born: the attack starts at `born`, never before it.
        if now < cell.born {
            return 0.0;
        }
        let reduced = ctx.cfg.reduced_motion;
        // The attack runs from its visual origin, which a live re-wet may
        // keep across a fresh identity; expiry runs from the cohort's clock
        // (`live_since`), since a key refreshes the whole run. A retired
        // cell's attack is frozen at `retire_at`, so a cell retired inside
        // its `edge-in` never climbs under the melt.
        let attack_now = cell.retire_at.map_or(now, |at| now.min(at));
        let age = attack_now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        // A flowing band's expiry is frozen at the fold: the flow is its
        // whole exit, and a natural melt under it would trim the row while
        // the drift carries it.
        let spent_to = coh.and_then(|c| c.flow).map_or(now, |f| now.min(f));
        let spent = spent_to
            .saturating_duration_since(coh.map_or(cell.born, |c| cell.born.max(c.alive_at)))
            .as_secs_f32();
        let u = (spent / cell.life_s.max(1e-3)).clamp(0.0, 1.0);
        // Under reduced motion every mark is static and takes the theme's one
        // linear fade over the last `REDUCED_MOTION_FADE_MS` of whatever ends
        // it (§6.11); the 18 ms `edge-in` stays, being an attack and not a
        // motion.
        // The attack: from dark through `edge-in` at birth, or for a cell
        // re-armed from a level (`Cell::rearm`, M1) from exactly that level up
        // to full through the same `edge-in`.
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
            // The content retirement (`Ribbon::retire_cells`): the retract's
            // `spend` on the short `RETIRE_MELT_S` span, exactly 1.0 at the
            // stamp and 0 at its end, monotone between.
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(RETIRE_MELT_S - r)
            } else {
                spend(clamp01(r / RETIRE_MELT_S))
            };
        }
        if let Some(at) = self.curtain {
            // The curtain: the retract's `spend` in the retract's
            // farthest-first order ([`Cohort::drain_rank`], the frozen
            // [`Cohort::drain_right_first`]). The farthest cell starts
            // spending at the curtain's instant, the cell under the hand at
            // [`CURTAIN_STAGGER_S`], each taking [`CURTAIN_SPEND_S`], so the
            // last reaches zero at `CURTAIN_S` and the band gathers into the
            // hand instead of dropping whole.
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(CURTAIN_S - r)
            } else {
                let rank = coh.map_or(1.0, |c| c.drain_rank(cell.col));
                let t0 = CURTAIN_STAGGER_S * (1.0 - rank);
                spend(clamp01((r - t0) / CURTAIN_SPEND_S))
            };
        }
        if let Some(coh) = coh
            && coh.phase.is_retracting()
        {
            let idle = coh.idle_s(now);
            if reduced {
                env *= reduced_fade(coh.swoosh_total() - idle);
            } else {
                // Farthest-first on the retract's schedule: the cell furthest
                // from the hand in logical order starts spending first
                // ([`Cohort::drain_rank`]: the left end on the caret's row and
                // rows above, the right end on rows below), each taking
                // `RETRACT_FADE_S`, so the mark is drawn back into the hand.
                if coh.flow.is_some() {
                    // A flowing band fades far end first to a common end
                    // ([`Cohort::flow_drain`]), each stop through its own
                    // colour floor ([`ember_level`], §24), so dim stops do not
                    // go dark before their neighbours. It then leaves by its
                    // shape: over the last [`FLOW_TAIL_S`] its body thins to
                    // nothing ([`Ribbon::sample`]), every stop at its floor.
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
        // The focus ember and its rearm run last, because the ember's floor is
        // priced against the light the cell actually has (melt, retract and
        // drain included); priced against the birth ceiling, a partly melted
        // cell would reach its floor before its neighbours.
        if let Some(at) = self.ember_at {
            // The ember, per stop: the fall is read into this cell's own range
            // by [`ember_level`], and the go-out is one common multiplier over
            // every stop at its own floor, so the band leaves as one object.
            let r = now.saturating_duration_since(at).as_secs_f32();
            let f = if reduced {
                reduced_fade(FOCUS_EMBER_S - r)
            } else {
                spend(clamp01(r / FOCUS_EMBER_S))
            };
            env *= ember_level(self.ember_floor(ctx, cell, env), f);
        }
        if let Some((at, fade)) = self.rearm {
            // Focus regained mid-ember: back up from the level it had, on the
            // sanctioned attack, so the return is continuous. The stamp
            // carries the ember's fade at the regain rather than one level,
            // since the level is per stop and each cell rearms from its own.
            let level = ember_level(self.ember_floor(ctx, cell, env), fade);
            let r = attack_now.saturating_duration_since(at).as_secs_f32();
            env *= level + (1.0 - level) * edge_in(r);
        }
        env
    }

    /// The cell's coverage: the flat body (head floor 1.0, crest gain 0.0) at
    /// its birth-priced ceiling ([`Cell::cov0`]), closed by
    /// [`Ribbon::env_of`], scaled by the host's intensity before the cap
    /// (§3.4: the bed's own published ceiling, the only cap this producer
    /// applies). `coh` is `cell`'s cohort.
    fn cov_of(&self, ctx: &Ctx<'_>, cell: &Cell, coh: Option<&Cohort>) -> f32 {
        let cap = cov_cap(ctx.cfg);
        (cap * cell.cov0 * self.env_with(ctx, cell, coh) * clamp01(ctx.cfg.intensity))
            .clamp(0.0, cap)
    }

    /// The ember's floor for this cell, as a share of the ember's own factor:
    /// the alpha byte at which its stop stops being seen over the live
    /// theme's ground ([`BedInkLut::seen_floor_at`], with
    /// [`EMBER_FLOOR_MARGIN`]) divided by the coverage the cell has with
    /// every other envelope factor already spent (`carried`). `0` (no floor)
    /// for a cell with no coverage to spend and for an unsynced ink table.
    fn ember_floor(&self, ctx: &Ctx<'_>, cell: &Cell, carried: f32) -> f32 {
        self.ember_floor_of(
            cov_cap(ctx.cfg) * cell.cov0 * clamp01(ctx.cfg.intensity) * carried,
            cell.t,
        )
    }

    /// [`Ribbon::ember_floor`] against a coverage already priced: the form
    /// the cadence fold reads, which has a peak and a walk position and no
    /// [`Config`].
    fn ember_floor_of(&self, full_cov: f32, t: f32) -> f32 {
        if !full_cov.is_finite() || full_cov <= 0.0 {
            return 0.0;
        }
        // Not clamped to 1: a floor over the cell's full coverage means "hold
        // full through the dim" and [`ember_level`]'s clamp says so; clamping
        // here would take the cell out of the common go-out.
        (self.ink.seen_floor_at(t) * EMBER_FLOOR_MARGIN / full_cov).max(0.0)
    }

    /// The from-the-hand attack (`RAINBOW-KITTY-V2.md` §30): the multiplier on
    /// a young cell's coverage at `dist` across the cell (`0.0` at its caret
    /// side, `1.0` at its far side), so its light enters as a wipe from the
    /// hand instead of fading in place.
    ///
    /// The cell's level already carries the uniform attack
    /// (`BIRTH_EDGE_FLOOR + (1 − floor) · edge_in(age)`, [`Ribbon::env_of`]);
    /// this returns the ratio of the wiped attack (the same ramp gated by a
    /// smoothstep front crossing the cell over [`ATTACK_WIPE_S`],
    /// [`ATTACK_WIPE_FRONT`] wide) to the uniform one, and
    /// [`Ribbon::plan_run`] multiplies it into the slab. The floor is never
    /// gated, so the echo frame shows the colour (T2). Exactly `1.0` past the
    /// wipe, for the flat spelling, under reduced motion (§6.11) and for an
    /// unborn wake cell.
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

    /// The run's own retract: every boundary of a retracting cohort is pulled
    /// toward the caret on `suck-in`, so the mark shortens into the hand (T5)
    /// and at `u = 1` collapses to the caret's x and draws nothing. The caret
    /// is the one the retract began under ([`Cohort::retract_col`]), so a
    /// jump mid-retract does not lurch the mark. Per cohort, never per row: a
    /// cohort still being typed beside a swooshing one keeps its boundaries.
    /// Nothing moves under reduced motion (§6.11). `coh` is the run's cohort.
    fn retract_x(&self, ctx: &Ctx<'_>, coh: Option<&Cohort>, x: f32) -> f32 {
        if ctx.cfg.reduced_motion {
            return x;
        }
        // The curtain draws every boundary into the column its cohort was
        // frozen toward when the curtain fell ([`Ribbon::curtain`], derived
        // from the band's head), on the retract's `suck-in`, whatever the
        // cohort's phase. [`Ribbon::caret`] is not read: the engine forgets
        // the caret with the space that ended, and at the alt-screen seam the
        // launching Return has already moved it.
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
            // The drift: a flowing band slides as one rigid object toward its
            // frozen fold edge ([`Ribbon::flow_target`]) by
            // `flow_drift · cw · flow_progress` px, every boundary by the same
            // amount, so each stop keeps its laid pitch (C2). Boundaries that
            // reach the edge collapse onto it and draw nothing; with the
            // far-first fade every pixel is only crossed by dimmer light.
            //
            // Only the kept band drifts: a boundary beyond the kept end (the
            // moved word's old cells and the eaten Space, stamped to leave by
            // [`Ribbon::leave_row`]) keeps its x and fades in place, so the
            // boundary order is kept.
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

    /// The head of a run (§4.1, §20.1): `(head column, at caret, wet,
    /// stream_dir)`.
    ///
    /// The head is the cell under the hand: the caret's own cell (`caret −
    /// 1`) when the caret stands in or just past the run and that cell is
    /// live, un-retracting light. Otherwise the run's newest cell that is not
    /// retracting. A run with no such cell (every cell draining) has its head
    /// at its right end and is not wet, so the hot edge stays off it.
    ///
    /// The head of a leftward wake is its landing: when the caret stands on
    /// the first cell of a wake cohort ([`Cohort::wake`]), the landing is the
    /// head and the stream runs toward higher columns (`stream_dir` `+1`), so
    /// the hot edge comes up at the hand and not at the corridor's far end. A
    /// typed cohort never reaches this clause, and a rightward wake keeps the
    /// own-cell head.
    ///
    /// `at caret` is the emit-order key (the run the hand is on goes first)
    /// and is true whenever the caret is in or beside the run. `stream_dir`
    /// is `−1` from every branch but the wake clause. `coh` is the run's
    /// cohort.
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

    /// Plan one contiguous run into [`Ribbon::plan`], boundary by boundary,
    /// with [`Ribbon::slabs_per_cell`] vertices per cell.
    ///
    /// `seam` is the last vertex of the run abutting this one on its left,
    /// and `right_nb` the owner cell abutting it on its right (another
    /// cohort's, [`Ribbon::build_runs`]): the run's first boundary is the seam
    /// vertex (its light the older side's under the tail-ease law below), and
    /// its last prices the right neighbour's light into the same law, so two
    /// abutting runs meet at one vertex instead of a feather.
    ///
    /// The attach: a typed run whose head is its last cell, with a live caret
    /// one column right and nothing laid under it, continues one cell past
    /// its head boundary with the head's own sample, so body, strip and rail
    /// run under the caret cell and the block stands on the band instead of
    /// a black notch. The extension has no key or clock of its own: priced
    /// from the head boundary, pulled by its retract, gone with it.
    ///
    /// The attach survives an erase: a leaving cell under the caret (the one
    /// a Backspace erased, or one the witness retired) does not own the
    /// caret cell either. Its slabs are planned with its own shape, colour
    /// and spend, floored at the attach's level ([`AttachFloor`]), so the
    /// frame it retires and the attach takes over changes no byte. The floor
    /// is applied inside the boundary loop, never as extra segments, since
    /// two polylines over one x would composite the bed twice.
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
        // The attach's condition (see the doc above): a typed run, wet at the
        // hand, the caret one column right of the head and inside the pane,
        // and no live light under the caret (nothing laid there, or a leaving
        // cell of either run).
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
        // The attach's own attack: the birth floor and the 18 ms edge-in (T2,
        // T3) and the from-the-hand wipe over `ATTACK_WIPE_S` from the head's
        // edge outward, all on the head cell's birth clock, whatever attack
        // the head cell itself inherited. `wipe_of` is a ratio to the uniform
        // attack, so the uniform attack is put back here.
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
        // Every interior boundary's left cell was the previous boundary's
        // right cell and a sample depends only on the cell and the frame, so
        // carry it across: one sample per live cell instead of two per
        // boundary.
        let mut left_sample = None;
        // The boundary (index from `col0`) a hard edge doubled, if any (see
        // the block's right edge below); the head index accounts for it.
        let mut doubled: Option<usize> = None;
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
            // The wipe's coordinate: `dist` runs from the cell's caret side to
            // its far side. A typed run's head is its right end (`stream_dir`
            // −1), so its cells' caret side is the right edge; a leftward
            // wake's is the left edge.
            let caret_side_right = stream_dir < 0;
            // The boundary is `a`'s right edge and `b`'s left edge.
            let wa = self.wipe_of(ctx, a, if caret_side_right { 0.0 } else { 1.0 });
            let wb = self.wipe_of(ctx, b, if caret_side_right { 1.0 } else { 0.0 });
            let mid = |u: f32, v: f32| (u + v) * 0.5;
            // The brighter side owns the boundary: shape and colour are the
            // midpoint of the two cells, coverage is the brighter cell's, and
            // the dimmer cell reaches its own level inside itself (its first
            // slab). A midpoint coverage would sag a settled cell while a
            // neighbour spends and pop it back when the neighbour retires.
            //
            // The younger side enters at the tail ease: the left side is the
            // older light on every run and the right side feathers in at
            // [`RUN_TAIL_EASE`], the boundary taking the brighter of the two.
            // An absent side gives no light, so the run's outer tail is a
            // one-cell feather; a live neighbour at full light owns the seam
            // and no feather shows; a leaving neighbour hands the feather over
            // as fast as its light goes, so the frame that retires it changes
            // no byte.
            //
            // At a seam with the run on the left, this run's first vertex is
            // that run's last (same spine, reach and stop) and carries its
            // light. At a seam on the right, the neighbour's light enters this
            // run's last boundary the same way.
            let mut la = left.map_or(0.0, |_| sa.3 * wa);
            let mut lb = right.map_or(0.0, |_| sb.3 * wb);
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
                // This boundary is `nb`'s left edge by construction, whatever
                // way this run streams, so the neighbour's wipe is read from
                // its own left edge. The term only matters where a leftward run
                // abuts a neighbour's cell typed within `ATTACK_WIPE_S`.
                let nb_coh = self.cohorts.iter().find(|c| c.id == nb.cohort);
                lb = self.cov_of(ctx, nb, nb_coh) * self.wipe_of(ctx, nb, 1.0);
            }
            // The same expression on every run: the left side owns the
            // boundary and the right enters at the ease, whichever way this
            // run streams. `la` is the neighbour's at a run's first boundary
            // (or nothing: the oldest end's one-cell feather) and the run's
            // own at its last, where the head, attach and block stand. The
            // stream direction follows the caret every frame, so mirroring
            // the expression for a leftward wake would let a moving caret
            // flip which end is soft and jump a settled boundary to full.
            let natural = la.max(lb * RUN_TAIL_EASE);
            // The block's right edge is a hard edge. At a leaving caret cell's
            // far edge the vertex is floored (`AttachFloor`); when the band
            // continues past it, the same x carries a second vertex at the
            // boundary's natural light, so the next cell lerps from its own
            // seam rather than the floor (`ribbon_beam` prices a slab once at
            // its centre). The zero-length segment between draws nothing (the
            // beam skips a span under 1e-3); the attach's own segments end at
            // the block's right edge the same way.
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
            let x = f32::from(ctx.geom.origin_x) + boundary as f32 * cw;
            if let Some((px, p)) = prev {
                // The cell between `prev` and `here` is `a`: its interior never
                // exceeds its own coverage, so a transition between two levels
                // lives inside the dimmer cell, and inside a young cell that
                // level is the wipe's at the slab.
                for j in 1..slabs {
                    let f = j as f32 / slabs as f32;
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
                let f = j as f32 / slabs as f32;
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
    fn attach_floor_at(&self, ctx: &Ctx<'_>, row: u16, cell: &Cell, dist: f32) -> Option<f32> {
        let af = self.attach_floor?;
        (af.row == row && af.col == cell.col && cell.leaving()).then(|| af.level(self, ctx, dist))
    }

    // -- emit --------------------------------------------------------------

    /// Draw the ribbon body into `frame.under` and the hot edge into
    /// `frame.out`.
    ///
    /// Called third in the emit order, after the meteor (§6.5) and before
    /// stardust, so a truncation sheds sky before the laid rainbow.
    ///
    /// The body is one `aterm_render::ribbon_beam` call per run, with the
    /// shoulder from [`BodyProfile`] and `GlowBlend::Over` on both grounds:
    /// the bed composites source-over, so its ceiling is a luminance rather
    /// than an additive budget, and the light theme's fork changes only the
    /// ink (§3.3, L6: never additive light on white).
    ///
    /// On dark themes a second call per run draws the vivid rail (§30,
    /// [`Ribbon::rail_lit`]): the same slabs and `x`, a polyline whose top is
    /// `max(spine, row bottom)` (so the wave never lifts it into descenders)
    /// reaching the body's `dn` below, in [`rail_ink`] at the body's coverage
    /// ([`RAIL_GAIN`]), Over-composited on the bed. `up` is `0.0`, so
    /// `ribbon_profile` is zero above the rail's top. It rides the same
    /// budget after its run's body; if that cuts another run's body on the
    /// focused row, the capped frame is repainted with every focused-row body
    /// ahead of its rails.
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.ink_quads = 0;
        if self.plan.is_empty() || self.runs.is_empty() {
            return;
        }
        let ink_from = frame.under.len();
        let shoulder = Self::body_profile(ctx.cfg).shoulder;
        let rail = Self::rail_lit(ctx.cfg);
        let chf = ctx.geom.ch as f32;
        let ceiling = if ctx.cfg.dark_theme {
            BODY_FRAME_TOP
        } else {
            light_role(ctx.cfg).alpha_cap()
        };
        let clip = ctx.geom.beam_clip();
        let budget = frame.under.len() + RIBBON_QUAD_BUDGET;
        let core_up = Self::body_profile(ctx.cfg).core_up_ch * ctx.geom.ch as f32;
        let lift_span = LEAD_CH * ctx.geom.ch as f32;
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
                // Laid head-first: a capped run whose head is inside it is
                // repainted outward from that head, never leaving the light
                // beside the hand behind. Walking the plan backwards is the
                // order a reversal of the forward list would give.
                for seg in plan[run.lo..run.hi].iter().rev() {
                    if rail_layer {
                        // The vivid rail lies below the row's bottom and uses
                        // the body's own lower reach, capped at the leading.
                        let top = row_bottom.max(seg.spine);
                        let dn = (seg.spine + seg.dn - top)
                            .max(DN_FLOOR_CH * chf)
                            .min(RAIL_REACH_MAX_CH * chf);
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
                    } else {
                        // The spine's strip is inside the body's one ceiling.
                        let strip = (f32::from(seg.cov) * STRIP_LIFT_GAIN)
                            .min(ceiling - f32::from(seg.cov))
                            .max(0.0);
                        verts.push(RibbonVertex {
                            x: seg.x,
                            spine: seg.spine,
                            up: seg.up,
                            dn: seg.dn,
                            core_up,
                            core_dn: 0.0,
                            color: ink.at(seg.t),
                            cov: f32::from(seg.cov),
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
    #[must_use]
    pub fn ink_quad_count(&self) -> usize {
        self.ink_quads
    }

    /// The gain of §4.1's hot edge, `0..1`, from the spine the head cell was
    /// born at. The A/B gate (§22): the whole refinement is one `0.0` away
    /// from gone. Dark themes only, and only while the hand is on the mark.
    ///
    /// Priced from the head cell's [`Cell::birth_disp`], never the live
    /// follower, which would brighten for ~100 ms after the last key. This is
    /// the price only; [`Ribbon::emit_hot_edge`] multiplies in the head
    /// cell's envelope ([`Ribbon::env_of`]).
    ///
    /// `typed` is whether the head cell is a key's ([`Cell::typing`]): the
    /// floor is a typed key's alone, so a cold arrow's wake has no hairline.
    #[must_use]
    pub fn hot_edge_gain(cfg: &Config, disp: f32, typed: bool) -> f32 {
        if !cfg.dark_theme || cfg.reduced_motion {
            return 0.0;
        }
        // The floor: the edge exists on a cold hand (a word's first key
        // attaches the band's top to the caret) and the rest ramps with the
        // hand. A wake's head is no key and gets no floor.
        let floor = if typed { HOT_EDGE_GAIN_FLOOR } else { 0.0 };
        floor
            + (1.0 - floor) * smoothstep01((clamp01(disp) - HOT_EDGE_DISP_MIN) / HOT_EDGE_DISP_SPAN)
    }

    /// Refinement A (§4.1): a 1-px additive hairline in the spectrum along the
    /// ribbon's top edge, from the head cell back [`HOT_EDGE_CELLS`] cells.
    /// Every vertex carries the stop under it lifted to
    /// [`HOT_EDGE_LUMA_FLOOR`] ([`BedInkLut::hot_at`]), so at the hand it is
    /// the head's stop, brighter than the bed on every stop of the arc.
    ///
    /// Drawn as a `comet_beam` hairline with one vertex per cell boundary,
    /// sampled at the body's `spine − up`, so it follows the wave. It is
    /// spatial, with no clock of its own, so it never lags the head: it ends
    /// at the head boundary [`Run::head`] resolves and reaches back toward the
    /// tail. Its gain is the birth-priced [`Ribbon::hot_edge_gain`] times the
    /// head cell's envelope ([`Ribbon::env_of`]), so it never rises after the
    /// last key and reaches zero with the body.
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
        // Birth-priced so it never rises; envelope-scaled so it goes out with
        // its body (the ember, the expiry melt, the swoosh's drain).
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
        // The seam ([`Ribbon::seam`]): on the new line of a continued input,
        // at the hand, a floor of [`SEAM_LEVEL`] under the falloff, spent over
        // the row above's drift and reaching back to the hand's first column
        // on the row, never past it (the prompt's columns carry no light). An
        // unknown floor draws no seam. After a composer's re-wrap the floor
        // is the wrap key's glyph, and the word the app relaid left of it
        // ([`Ribbon::relay_word`]) carries none
        // (`the_seam_marks_a_continued_line_s_tail_and_spends_with_the_row_above`).
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
            let a_rel = (a / HOT_EDGE_ALPHA).max(if in_seam {
                seam.map_or(0.0, |(_, l)| l)
            } else {
                0.0
            });
            let cov = (HOT_EDGE_COV_MAX * a_rel * gain * clamp01(ctx.cfg.intensity))
                .clamp(0.0, HOT_EDGE_COV_MAX);
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

    /// This frame's field index — read by stardust (tint deal, §5.3) and by
    /// `Engine::field_at` (seam point 4).
    #[must_use]
    #[cfg(test)]
    pub fn field(&self) -> &FieldIndex {
        &self.index
    }

    /// The field at a cell, or `None`.
    #[must_use]
    pub fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.index.at(row, col)
    }

    /// The walk at a cell: `(d, t_zero)` of the cohort owning `(row, col)`, or
    /// `None` where nothing is laid. `d` is the walk's distance at this cell
    /// ([`Cohort::walk_d`]) and `t_zero` its `t` at distance zero
    /// ([`Cohort::walk_zero`]), the two numbers [`Cohort::t_at`] adds, so
    /// `t_zero + walk_t(d + dx)` is exactly the owner's number `dx` cells
    /// along and continues past the band's ends at its own pace. A mark that
    /// must wear the band's colour where the band does not reach (the landing
    /// starburst's jets, `meteor::LandingWalk`) needs this; the field alone
    /// cannot give it.
    ///
    /// The owner is the newest cell in pool order at the position
    /// ([`Ribbon::build`]'s newest-first sort without its layer rank): on a
    /// jump's own tick, the corridor's unborn `Over` cell, where the field
    /// index still reads the `Base` cell beneath. Both readers agree because
    /// the row holds one walk (`RAINBOW-KITTY-V2.md` §45,
    /// `a_landing_across_a_yanked_word_wears_the_band_s_number_whichever_cell_each_reader_takes`).
    /// Read after [`Ribbon::plan`], like `field_at`.
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

    /// The caret's position in the field (seam point 4): the caret's own cell
    /// when it owns light; otherwise the newest cell real typing laid on the
    /// caret's row, then on any row (a reach cell or an older row's retracting
    /// cell is not the hand's colour); red when nothing is laid.
    ///
    /// Also §6.4's `t_land`: the meteor phase-locks its arc to this value at
    /// its spawn edge, so the station under the caret is the caret's own stop
    /// by construction.
    #[must_use]
    pub fn field_at_caret(&self) -> f32 {
        if let Some(t) = self.caret.and_then(|(row, col)| self.field_at(row, col)) {
            return t;
        }
        let newest_typing = |row: Option<u16>| {
            self.cells
                .iter()
                .filter(|c| c.typing && !c.leaving() && row.is_none_or(|r| c.row == r))
                .max_by(|a, b| a.born.cmp(&b.born))
                .map(|c| c.t)
        };
        newest_typing(self.caret.map(|(row, _)| row))
            .or_else(|| newest_typing(None))
            .unwrap_or(0.0)
    }

    /// The source RGB of the ribbon head's body, before premultiplication:
    /// what a companion cursor inherits to meet the laid ribbon with no
    /// palette seam (seam point 5). `None` before any cursor has been
    /// observed.
    ///
    /// Dark themes return the authored spectrum colour at the caret's field
    /// stop; the consumer applies its own ink (the caret block lifts it to
    /// its light floor, §3.2 rank 3), since [`bed_ink`] would paint a yellow
    /// caret olive. Light themes apply the body's leading-ink recipe at that
    /// position, so the caret meets the rail with no seam. Both arms resolve
    /// [`Ribbon::field_at_caret`] once, through the bed's `tri` (C2).
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.caret?;
        let arc = spectrum(walk_arc(self.field_at_caret()));
        Some(if cfg.dark_theme {
            arc
        } else {
            light_role(cfg).ink(arc)
        })
    }

    /// The body's vertical extent at a cell, or `None` where no cell is laid
    /// (§4.2, §5.4, D16). Valid only after [`Ribbon::plan`] has run for this
    /// frame.
    ///
    /// Stardust's sky band is `[top − 0.30 ch, top − 0.04 ch]` from this
    /// `top` (D16): the tall body covers the whole cell, so a birth zone in
    /// cell coordinates would put every strike star inside the stroke.
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
    #[must_use]
    pub fn live_cells(&self) -> usize {
        self.cells.len()
    }

    /// The planned boundaries lit on the last frame: coverage at or over
    /// [`STATUS_LIT_COV`] after the time-fade, the edge-in and the expiry
    /// melt. `trail status` claims this as its ribbon (`ribbon_segments=`,
    /// `ribbon_hue_bands=`), so no claim is made for a resident cell that has
    /// melted below what the glass shows; the paint-conformance bind charges
    /// a claimed-but-dark frame as `ribbon_dark`.
    pub fn lit_segments(&self) -> impl Iterator<Item = &Segment> {
        self.plan.iter().filter(|s| s.cov >= STATUS_LIT_COV)
    }

    /// What is actually on the glass: every planned boundary whose composite
    /// over the theme ground reads as band ink to the census
    /// ([`reads_as_ink`]), not only those over the claim floor
    /// [`Ribbon::lit_segments`] keeps.
    ///
    /// [`STATUS_LIT_COV`] bounds what the status verb may claim (it is solved
    /// for the dimmest stop), not what is drawn: during a curtain's fall much
    /// of the band is below it but still composited. `trail status` reports
    /// this beside the claim (`ribbon_drawn=`), and `ribbon_active` reads it
    /// while a curtain is falling.
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
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        let at = self.curtain?;
        let r = now.saturating_duration_since(at).as_secs_f32();
        Some(((CURTAIN_S - r).max(0.0) * 1000.0).round() as u32)
    }

    /// True when nothing is laid and no transient is finishing: one of the
    /// three pools `Engine::next_change_deadline` folds (§18).
    ///
    /// Clockless: an expired cell is removed by [`Ribbon::plan`], so "is
    /// anything laid" and "is anything visible" are the same question, and
    /// `needs_frame_cadence()` stays argument-free (seam point 8).
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.cells.is_empty() && self.cohorts.is_empty()
    }

    /// The cadence law, brisk half: whether per-frame motion is on the mark
    /// (a cell inside its 18 ms `edge-in` (T3), the focus rearm's `edge-in`,
    /// or a cohort retracting into the caret on `suck-in`, §4's 0.40 s). Its
    /// grace, reach beats and fade are tails. Under reduced motion nothing
    /// moves (§6.11); the attack still ramps.
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

    /// The cadence law: the next instant this ribbon changes what is on glass,
    /// `None` at rest so the host can idle at 0 % (T6).
    ///
    /// Offers to the [`Cadence`] fold: the `edge-in`s and the retract are
    /// brisk ([`Ribbon::brisk`]); the reach beats, the retract's first frame
    /// and the swoosh's end are exact edges; every fade (expiry melt, a
    /// retracted cell's `spend`, the focus ember, the swoosh's fade) is a tail
    /// at its next visible step ([`level_step_melt`], [`level_step_spend`])
    /// with the cell's other factors frozen into the peak, and every
    /// disappearance or fade start is a tail at its instant, all floored. The
    /// peak is the dark cap ([`UNDER_COV_CAP`]); a lower peak only moves the
    /// step later, so the dark reading is never late. Under reduced motion
    /// every fade is the theme's linear 120 ms fade (§6.11).
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
                    self.ember_at.map(|at| FOCUS_EMBER_S - since(at)),
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
            let ember_u = self.ember_at.map(|at| clamp01(since(at) / FOCUS_EMBER_S));
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
            let f_e = ember_u.map_or(1.0, |u| ember_level(ember_floor, spend(u)));
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
            // Appearances and the one motion's start are edges: the grace
            // ending on the first reach beat, the two beats after it, the
            // retract's first frame, and the swoosh's end (the idle instant).
            // The retract's end is a tail. A flowing cohort has no grace or
            // beats: its motion begins at its last key and ends with the flow.
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

    /// The band follows its text along the row: the engine half of the shift
    /// pass ([`super::Engine::follow_rows`]). The content witness found the
    /// cells of `cohort` on `row` within `lo..=hi` standing under their own
    /// glyphs `dc` columns away ([`super::witness::ShiftRun`]), the tail a
    /// mid-line insert pushed right. They move there with every clock and
    /// price intact; each `t` is re-read at the new column off the walk's
    /// anchor, which does not move ([`Cohort::t_at`], C2), so the colour stays
    /// with the screen column while the light rides the glyph. The cohort's
    /// bounds are re-read from its cells. A run carried to another row
    /// ([`Ribbon::translate_run`]) keeps its `t` instead.
    ///
    /// Each moved cell is pushed onto `moved` by its old identity (the witness
    /// re-keys it, [`super::witness::Witness::shift_cells`]) and onto
    /// [`Ribbon::followed_now`] by its new one, since the tick's erases are
    /// addressed in the old layout. A leaving cell keeps its column and
    /// clock. Allocates nothing past the scratch's growth; returns how many
    /// cells moved.
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
        // One cell per column: where part of the run's tail was left behind
        // (a wrap took it off the row), the block lands on columns that tail
        // still holds. Those are the light of glyphs that left, so they go;
        // keeping them would stack two cells of one cohort on one
        // `(row, col)`. Leaving cells are never in the way: the shift pass
        // refuses a run with light going out at or right of its edit point.
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
        // The colour stays with the column: the light (clock, price,
        // identity) rides the glyph and the stop is re-read at its new column
        // off the walk's anchor, which stays put (C2). Every screen column
        // keeps the colour it showed: the key takes its column's stop, the
        // text slides under a rainbow that stands still, and the band grows
        // by one stop at its right end (`tests/midline_insert_colour_seam.rs`,
        // `tests/keys_typed_at_the_line_start.rs`).
        let walk = self.cohorts[ci];
        let mut n = 0usize;
        for c in &mut self.cells {
            if c.row != row || c.cohort != cohort || c.leaving() || !(lo..=hi).contains(&c.col) {
                continue;
            }
            let Some(col) = c.col.checked_add_signed(dc) else {
                continue;
            };
            moved.push((c.row, c.col, c.born));
            c.col = col;
            c.t = walk.t_at(col);
            self.followed_now.push((row, col, c.born));
            n += 1;
        }
        if n == 0 {
            return 0;
        }
        self.shifted_now.push((row, lo, hi, dc));
        let (mut c0, mut c1) = (u16::MAX, 0u16);
        for c in self
            .cells
            .iter()
            .filter(|c| c.row == row && c.cohort == cohort)
        {
            c0 = c0.min(c.col);
            c1 = c1.max(c.col.saturating_add(1));
        }
        let coh = &mut self.cohorts[ci];
        coh.col0 = c0;
        coh.col1 = c1;
        // A flowing band's fold edge rides the shift: the edge the flow froze
        // ([`Ribbon::flow_target`]) is a glyph's column, and an insert typed
        // into the row after a Nav back up pushes that glyph right. The edge
        // moves with it while the walk's anchor stays. A shift only pushes
        // right (`dc > 0`), lengthening the kept band, so the drift needs no
        // new cap.
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

    /// The band follows its text across rows: the engine half of the follow
    /// pass ([`super::Engine::follow_rows`]). The content witness found the
    /// run's glyphs standing `dr` rows away at their own columns and gone
    /// from their own row (a bottom-anchored box repainted one row higher
    /// without a scroll, [`super::witness::FollowRun`]), so the cells of
    /// `cohort` on `row` within `lo..=hi` (the found cells and the run's
    /// never-armed blanks between) move to `row + dr` with every clock, price
    /// and `t` intact. If the whole cohort lies inside the span it moves
    /// itself; otherwise a copy (same origin, clocks, phase and drain fields)
    /// is minted on the new row for the moved cells and the old cohort keeps
    /// the rest with its bounds shrunk. With `flow_at` the moved run is set
    /// flowing ([`Cohort::flow`]): the text left the caret's row.
    ///
    /// The mirror, the pen, `last_space` and the relocation stay: they are
    /// the hand's. Each moved cell's old `(row, col, born)` is pushed onto
    /// `moved` for the witness to re-key
    /// ([`super::witness::Witness::translate_cells`]). The field index is
    /// dropped and rebuilt by the next [`Ribbon::plan`]; until then
    /// [`Ribbon::field_at_caret`] answers from the newest typed cell.
    ///
    /// Allocates nothing past the pool's growth. Returns how many cells
    /// moved.
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
        let inside = |c: &Cell| c.row == row && c.cohort == cohort && (lo..=hi).contains(&c.col);
        let n = self.cells.iter().filter(|c| inside(c)).count();
        if n == 0 {
            return 0;
        }
        let all = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.cohort == cohort)
            .count();
        let id = if all == n {
            self.cohorts[ci].row = new_row;
            cohort
        } else {
            let mut copy = self.cohorts[ci];
            copy.id = self.next_cohort;
            self.next_cohort = self.next_cohort.wrapping_add(1);
            copy.row = new_row;
            copy.release_clock = None;
            let (mut c0, mut c1) = (u16::MAX, 0u16);
            let (mut k0, mut k1) = (u16::MAX, 0u16);
            for c in self
                .cells
                .iter()
                .filter(|c| c.row == row && c.cohort == cohort)
            {
                if inside(c) {
                    c0 = c0.min(c.col);
                    c1 = c1.max(c.col + 1);
                } else {
                    k0 = k0.min(c.col);
                    k1 = k1.max(c.col + 1);
                }
            }
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
                // Carried back under the hand: a flowing run the follow pass
                // brings onto the caret's row (the box shrank back a row under
                // a Backspace through the fold) is the hand's row again and is
                // re-wet as a typed lay re-wets it ([`Ribbon::place`]): the
                // flow and frozen target cleared, the clock taken at the move.
                coh.retract_col = None;
                coh.flow_drift = 0.0;
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
            }
        }
        // A run carried back onto the seam's row takes the seam: the seam
        // marks the row the input continued onto, and a run brought back
        // there (a composer's unwrap) is the line the input started on, so
        // the continuation is gone. The caret's row never changed, so nothing
        // else clears it; left armed, the seam's floor would light line 1's
        // whole tail as the Backspace run reached it.
        if !away && self.seam.is_some_and(|s| s.row == new_row) {
            self.seam = None;
        }
        self.index.reset();
        self.plan.clear();
        self.runs.clear();
        n
    }

    /// A scroll carries the ribbon with its text (seam point 12): move every
    /// laid cell by a scroll of `rows` rows, with everything addressed by a
    /// cell's row (the cohorts, the field index the other producers read, and
    /// the caret the field resolves against). What leaves the grid is dropped
    /// rather than clamped: a ribbon pinned to row 0 is light on a line nobody
    /// typed.
    ///
    /// No cell height is taken: all ribbon state is grid rows and clocks, and
    /// the pixel geometry ([`Ribbon::plan`], the runs, the hot edge, the bands
    /// stardust reads) is this frame's scratch, dropped here and rebuilt by
    /// the next plan before any producer reads it.
    ///
    /// The index and the caret are translated, not dropped: the engine seeds
    /// the tick's `caret_t` from [`Ribbon::field_at_caret`] before it plans,
    /// and [`Ribbon::field_at`], [`Ribbon::head_rgb`] and [`Ribbon::caret`]
    /// may be read between the host's `note_scroll` and its tick, so they
    /// must answer for the moved cell. The caret is a position and saturates
    /// at row 0 as the engine's does; the next plan re-observes it.
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
        // A pen carried off the top is dropped, not saturated, as `last_space`
        // and `relocate` below are and as [`Ribbon::translate_band`] does: the
        // pen is the hand, and a saturated pen would let
        // [`Ribbon::typed_landing`] re-home a later key's cells to row 0.
        self.pen = self
            .pen
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.arrived = self
            .arrived
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        // The hand's floor rides with the text it bounds; a row scrolled off
        // the top takes it, because the hand was never on the row that
        // replaced it.
        self.hand_floor = self
            .hand_floor
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        // The re-anchor's bookkeeping rides the scroll with its text: a
        // composer at the screen's bottom grows a row up by scrolling the
        // screen in the very repaint that moves the word down.
        self.last_space = self
            .last_space
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.prev_space = self
            .prev_space
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.seam = self.seam.filter(|s| s.row >= rows).map(|s| Seam {
            row: s.row - rows,
            ..s
        });
        self.relocate = self.relocate.filter(|r| r.row >= rows).map(|r| Relocate {
            row: r.row - rows,
            ..r
        });
        // A soft-wrapped word waiting for its reflow is dropped, not carried:
        // the reflow is the next key's echo, and a scroll between the two is
        // not a shape the carry handles ([`Ribbon::settle_carry`]).
        self.carry = None;
        // The row the new line's gate says the hand left is the old line's
        // row and rides with its text; carried off the top it is dropped and
        // the gate reads its instant alone ([`FreshLine::left_row`]).
        if let Some(gate) = &mut self.fresh_line {
            gate.left_row = gate.left_row.and_then(|row| row.checked_sub(rows));
        }
        self.index.translate(rows);
        self.plan.clear();
        self.runs.clear();
        // The band has followed its text on this tick; a backward caret from
        // the same repaint is not a stale line ([`Ribbon::scroll_followed`]).
        self.scroll_followed = true;
    }

    /// A row band carries its ribbon with its text (seam point 12, the band
    /// path): [`Ribbon::translate_scroll`] for screen rows `top..=bottom`
    /// moved by `delta` rows while every other row stands still (an inline
    /// viewport sliding down per streamed line, a transcript archiving up
    /// under a fixed composer, `RI×k`, a DECSTBM region scroll, IL/DL).
    ///
    /// The law is [`crate::cursor_glow::band_row`]'s: a cell or cohort
    /// outside the band is untouched, one inside moves by `delta` with its
    /// clocks and prices, and one carried past the band's edge is dropped
    /// rather than clamped. The two orphan sweeps keep "a cohort has cells"
    /// and "a cell has a cohort" true. The caret is a position
    /// ([`crate::cursor_glow::band_pos`]), saturated at the band's edge. The
    /// field index is translated lane by lane so [`Ribbon::field_at`] and
    /// [`Ribbon::field_at_caret`] answer for the moved cell before the next
    /// plan. No pixel is taken, as in the scroll twin.
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
        // The hand's floor rides with the text it bounds, as in the scroll
        // twin; left on the old row, the reach's floor would fall to column 0
        // and the seam ([`Ribbon::emit_hot_edge`]) would go out for a frame.
        self.hand_floor = self
            .hand_floor
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.last_space = self
            .last_space
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.prev_space = self
            .prev_space
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.seam = self
            .seam
            .and_then(|s| band_row(s.row, top, bottom, delta).map(|row| Seam { row, ..s }));
        self.relocate = self
            .relocate
            .and_then(|r| band_row(r.row, top, bottom, delta).map(|row| Relocate { row, ..r }));
        // …and a soft-wrapped word is dropped, as the whole-grid scroll drops
        // it ([`Ribbon::translate_scroll`]).
        self.carry = None;
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

    /// Drop everything — style switch, layout change, `Engine::reset`.
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
        self.pending_erase = 0;
        self.pen = None;
        self.arrived = None;
        self.hand_floor = None;
        self.last_space = None;
        self.prev_space = None;
        self.relocate = None;
        self.carry = None;
        self.split_off = None;
        self.last_walk = None;
        self.fresh_line = None;
        self.seam = None;
        // The phrase went with its cells; the hand's tempo and its last key
        // are the hand's, not the coordinate space's, and stay.
        self.phrase_open = None;
        self.curtain = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::{Geom, SoundCue};
    use aterm_render::{GlowQuad, RainHalo, over_premul, premul_rgb};
    use std::time::Duration;

    use super::super::spine::{SURGE_FULL_RISE, SURGE_MIN_RISE};
    use super::super::{CaretSeam, Dir, TypedClass};

    /// The shipped dark theme, which every legibility number in the family is
    /// solved against.
    const DEFAULT_BG: u32 = 0x001A_1B26;
    const DEFAULT_FG: u32 = 0x00C8_D3F5;
    /// The Nord theme's ground, a pair the gap tests are stated on.
    const NORD_BG: u32 = 0x002E_3440;
    const NORD_FG: u32 = 0x00D8_DEE9;

    /// The ground of `ColorScheme::default` (a config with no `theme` line),
    /// the darkest ground aterm ships. `DEFAULT_BG` above is Tokyo Night's
    /// `#1A1B26`, a different theme.
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
    fn the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar() {
        // "Dim and muddy" as a number: under L3's 5.25:1 bar every stop is put
        // on `bed_luma_budget(fg)`, and a warm stop at that luminance is an
        // olive: on the default theme a full-coverage yellow composites at
        // `(80, 80, 3)`. The module does not claim to fix that.
        //
        // Two-sided: a change that relaxes the bar lands the warm mids higher
        // and a change that dims the bed trips the floor; either way the
        // number moves here first. The bright-foreground gain is pinned in
        // `a_bright_foreground_s_bed_takes_the_bar_s_whole_budget_not_a_tenth`.
        let budget = bed_luma_budget(DEFAULT_FG);
        let cov = UNDER_COV_CAP as u8;
        let dimmest = (0..=32u32)
            .map(|i| {
                let t = i as f32 / 32.0;
                let lit = over_premul(
                    DEFAULT_BG,
                    premul_rgb(bed_ink(spectrum(t), budget), cov),
                    cov,
                );
                (max_channel(lit), i, lit)
            })
            .min()
            .expect("33 stops");
        let (max, i, lit) = dimmest;
        let t = i as f32 / 32.0;
        assert!(
            (79..=81).contains(&max),
            "the dimmest stop (t = {t:.3}, composited #{lit:06X}) peaks at max channel {max}; the bar puts it at ≈ 80 and v1's table at ≈ 84 — a brighter warm mid is a ruling on the bar, a dimmer one is light left under it"
        );
    }

    /// The worst contrast the bed can composite against `fg` over `bg`: every
    /// position of the arc, every level the emitter can reach (the body's
    /// request and the strip's accent, to the ledger's frame top).
    fn worst_bed_contrast(fg: u32, bg: u32) -> f32 {
        worst_bed_contrast_at(fg, bg, bed_luma_budget(fg))
    }

    /// The same, with the bed put on a stated luminance instead of the
    /// theme's own budget — what a different budget would read at.
    fn worst_bed_contrast_at(fg: u32, bg: u32, budget: f32) -> f32 {
        let mut worst = f32::INFINITY;
        for i in 0..=512u32 {
            let ink = bed_ink(spectrum(i as f32 / 512.0), budget);
            for cov in 1..=(BODY_FRAME_TOP as u32) {
                let cov = cov as u8;
                worst = worst.min(contrast(fg, over_premul(bg, premul_rgb(ink, cov), cov)));
            }
        }
        worst
    }

    fn max_channel(c: u32) -> u32 {
        ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff)
    }

    fn min_channel(c: u32) -> u32 {
        ((c >> 16) & 0xff).min((c >> 8) & 0xff).min(c & 0xff)
    }

    #[test]
    fn the_bed_sits_at_the_bar_not_under_it_on_the_default_dark_theme() {
        // The bed sits within a tenth of the bar: `solve_for_luma` answers
        // from under its target, so the ink's rounding costs nothing, and the
        // composite's one level of rounding costs 0.05 of a ratio point at the
        // dimmest theme above the floor
        // (`letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor`).
        let worst = worst_bed_contrast(DEFAULT_FG, DEFAULT_BG);
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the bed composited to {worst:.4}:1; the bar is {BODY_CONTRAST_BAR}:1"
        );
        assert!(
            worst - BODY_CONTRAST_BAR <= 0.10,
            "the bed's worst stop composites at {worst:.4}:1 against a {BODY_CONTRAST_BAR}:1 bar — light left under the bar is diminished light; the guard under it is one level of rounding, not {:.4} of a ratio point",
            worst - BODY_CONTRAST_BAR
        );
    }

    #[test]
    fn letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor() {
        // The net under the guard: 67 foregrounds (greys `#909090` to
        // `#FFFFFF` in steps of three, 27 tints from `{A0, D0, FF}³`, the
        // shipped defaults) over four grounds, each only where the ground sits
        // under the bed's ceiling (a brighter ground is the theme's contrast,
        // not the bed's), through the ledger's frame top plus one Bayer level.
        // Foregrounds the floor catches (`bed_luma_budget` clamped to
        // `BED_LUMA_MIN`, fg ≤ ≈ `#999999`) are the documented exception and
        // are skipped. `BODY_CONTRAST_GUARD` is the smallest guard this sweep
        // holds at.
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
        // Nord's pair — the owner's own theme — rides in the sweep too.
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

    #[test]
    fn a_bright_foreground_s_bed_takes_the_bar_s_whole_budget_not_a_tenth() {
        // `BED_LUMA_MAX` is the bar's answer at a white foreground, so under
        // the bar the clamp binds nowhere (a lower clamp would bind on every
        // foreground brighter than ≈ `#D8D8D8`).
        let white = bed_luma_budget(0x00FF_FFFF);
        assert!(
            white > 0.14,
            "a white foreground's bed budget is {white:.4}; the bar itself allows ≈ 0.148 and the bed took 0.100"
        );
        let cov = UNDER_COV_CAP as u8;
        let yellow_at = |fg: u32, bg: u32| {
            max_channel(over_premul(
                bg,
                premul_rgb(bed_ink(0x00FF_FF00, bed_luma_budget(fg)), cov),
                cov,
            ))
        };
        let (white_bed, renderer_bed) = (
            yellow_at(0x00FF_FFFF, 0),
            yellow_at(0x00E8_E8F0, 0x0011_1318),
        );
        assert!(
            (102..=104).contains(&white_bed),
            "yellow at the bed's ceiling under a white foreground composites at max channel {white_bed}; the bar puts it at ≈ 103 (it was 87 under the 0.100 clamp)"
        );
        assert!(
            (91..=93).contains(&renderer_bed),
            "yellow at the bed's ceiling under the renderer's `#E8E8F0` composites at max channel {renderer_bed}; the bar puts it at ≈ 92 (it was 88 under the 0.100 clamp)"
        );
    }

    #[test]
    fn every_stop_of_the_arc_composites_at_one_weight() {
        // The even-arc law: after the ink recipe, no two positions of the arc
        // differ in composited luminance by more than byte quantization
        // explains. The shared weight is the bar's own (pinned above).
        let budget = bed_luma_budget(DEFAULT_FG);
        let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
        for i in 0..=256u32 {
            let y = relative_luminance(bed_ink(spectrum(i as f32 / 256.0), budget));
            lo = lo.min(y);
            hi = hi.max(y);
        }
        assert!(
            hi - lo <= 0.004,
            "the arc's composited weight spans {lo:.4}..{hi:.4}; the bed is supposed to be even"
        );
    }

    /// HSV saturation of a composited pixel, `0..=1`.
    fn sat(c: u32) -> f32 {
        let (mx, mn) = (max_channel(c), min_channel(c));
        if mx == 0 {
            0.0
        } else {
            (mx - mn) as f32 / mx as f32
        }
    }

    /// The bed's composite at the body's own cap on a theme.
    fn bed_at_cap(t: f32, fg: u32, bg: u32) -> u32 {
        let cov = UNDER_COV_CAP as u8;
        over_premul(
            bg,
            premul_rgb(bed_ink(spectrum(t), bed_luma_budget(fg)), cov),
            cov,
        )
    }

    // -- the black gaps: chroma, not luminance ------------------------------

    #[test]
    fn the_bed_s_dark_stops_arrive_at_full_chroma_not_walked_grey() {
        // At the bar's luminance a stop keeps its hue's chroma (indigo owns
        // `(127, 0, 219)`) rather than walking toward white, which would read
        // as a dark grey gap beside a bright blue.
        for (name, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ] {
            let budget = bed_luma_budget(fg);
            let indigo = bed_ink(0x004B_0082, budget);
            assert!(
                sat(indigo) >= 0.95 && max_channel(indigo) >= 195,
                "{name}: indigo's bed ink is #{indigo:06X} (S {:.2}, peak {}) — a grey lavender, not a violet",
                sat(indigo),
                max_channel(indigo)
            );
            // …at the bar's weight, not over it.
            assert!(
                relative_luminance(indigo) < budget && budget - relative_luminance(indigo) < 0.002,
                "{name}: indigo sits at Y {:.4} against a budget of {budget:.4}",
                relative_luminance(indigo)
            );
            // The whole arc, composited at the cap: no stop is walked greyer
            // than blue's own unavoidable share of white (blue's full value
            // is under the bar, so it alone takes white: S 0.81 on Nord).
            let (mut lo, mut lo_t) = (f32::INFINITY, 0.0f32);
            for i in 0..=512u32 {
                let t = i as f32 / 512.0;
                let s = sat(bed_at_cap(t, fg, bg));
                if s < lo {
                    lo = s;
                    lo_t = t;
                }
            }
            assert!(
                lo >= 0.78,
                "{name}: the bed's least saturated composite is S {lo:.3} at t {lo_t:.3} — the walk toward white is back"
            );
        }
        // The controls: the stops the bar itself prices are byte-identical to
        // the recipe this replaced (scale-down is scale-down).
        let budget = bed_luma_budget(DEFAULT_FG);
        for (stop, name) in [
            (0x00FF_FF00, "yellow"),
            (0x0000_FF00, "green"),
            (0x00FF_0000, "red"),
        ] {
            let old = solve_for_luma(|k| scale_rgb(stop, k), budget);
            assert_eq!(
                bed_ink(stop, budget),
                old,
                "{name} is priced by the bar alone and must not move"
            );
        }
        // Blue's full value is already blue: its white share is what it was.
        let old_blue = solve_for_luma(|w| toward_white(0x0000_00FF, w), budget);
        assert_eq!(
            bed_ink(0x0000_00FF, budget),
            old_blue,
            "blue was already at full value"
        );
    }

    #[test]
    fn the_crossing_composites_no_greyer_than_its_flanks() {
        // The green→blue crossing carries the arc's own saturation (S 1.00),
        // and `BED_SAT_FLOOR` holds any read that is not.
        use crate::spectrum::{spectrum_crossing_position, spectrum_crossing_span};
        let mid = spectrum_crossing_position();
        // A quarter of the green→blue leg: the scan covers `mid ± half` and
        // reads its flanks at `mid ± 2·half`, the leg's own two anchors.
        let half = 0.25 * spectrum_crossing_span();
        for (name, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ] {
            let green = sat(bed_at_cap(mid - 2.0 * half, fg, bg));
            let blue = sat(bed_at_cap(mid + 2.0 * half, fg, bg));
            let mut worst = f32::INFINITY;
            let mut at = 0.0f32;
            let mut px = 0u32;
            for i in 0..=64u32 {
                let t = mid - half + 2.0 * half * i as f32 / 64.0;
                let c = bed_at_cap(t, fg, bg);
                if sat(c) < worst {
                    worst = sat(c);
                    at = t;
                    px = c;
                }
            }
            assert!(
                worst >= green.min(blue) - 0.02,
                "{name}: the crossing composites #{px:06X} (S {worst:.3}) at t {at:.3}, greyer than its flanks (green S {green:.3}, blue S {blue:.3})"
            );
            assert!(
                worst >= 0.90,
                "{name}: the crossing's least saturated composite is S {worst:.3} (#{px:06X}) — a chroma dip is showing through the bed"
            );
            // And the arc's hue through the seam is the table's own: the
            // floor moves S, never H.
            for i in 0..=16u32 {
                let t = mid - half + 2.0 * half * i as f32 / 16.0;
                let (h_arc, _, _) = crate::spectrum::spectrum_hsv(spectrum(t));
                let (h_ink, _, _) =
                    crate::spectrum::spectrum_hsv(bed_ink(spectrum(t), bed_luma_budget(fg)));
                assert!(
                    (h_arc - h_ink).abs() <= 2.5,
                    "{name}: at t {t:.3} the bed's hue is {h_ink:.1}° against the arc's {h_arc:.1}°"
                );
            }
        }
    }

    #[test]
    fn the_ground_never_lifts_the_bed_over_the_bar() {
        // There is no floor against the ground: wherever one exceeded the bar
        // it would lift the bed over the luminance the text can bear. The
        // budget is the bar's answer alone, clamped to [`BED_LUMA_MIN`,
        // `BED_LUMA_MAX`].
        let bar = |fg: u32| {
            (relative_luminance(fg) + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05
        };
        let one_dark = (0x00AB_B2BFu32, 0x0028_2C34u32);
        let solarized = (0x0083_9496u32, 0x0000_2B36u32);
        for (name, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
            ("One Dark", one_dark.0, one_dark.1),
            ("One Dark on its alt ground", one_dark.0, 0x0032_3844),
            ("Solarized Dark", solarized.0, solarized.1),
            (
                "a #999999 foreground on Nord's ground",
                0x0099_9999,
                NORD_BG,
            ),
        ] {
            let budget = bed_luma_budget(fg);
            assert!(
                (budget - bar(fg).clamp(BED_LUMA_MIN, BED_LUMA_MAX)).abs() < 1e-6,
                "{name}: the budget {budget:.4} is not the bar's own answer {:.4}",
                bar(fg)
            );
            // Control: a 1.4:1 ground floor would bind here, lifting the bed
            // on One Dark and Solarized over the bar and the text under 5.25:1.
            let floor = (relative_luminance(bg) + 0.05) * 1.4 - 0.05;
            if floor > budget {
                assert!(
                    worst_bed_contrast_at(fg, bg, floor) < BODY_CONTRAST_BAR,
                    "{name}: the control — a 1.4:1 ground floor ({floor:.4}) would not have broken the bar here"
                );
            }
            // The bar's own budget holds the bar wherever it is obeyed at all:
            // a foreground clamped up to `BED_LUMA_MIN` (Solarized Dark's
            // `#839496` solves to 0.012) is the documented exception.
            if budget > BED_LUMA_MIN {
                assert!(
                    worst_bed_contrast_at(fg, bg, budget) >= BODY_CONTRAST_BAR,
                    "{name}: the bar's own budget composites under the bar"
                );
            }
        }
        // One Dark's pair is the one the floor cost the most: 5.29:1 → 4.69:1
        // (4.74:1 on the sweep pin's own coarser grid).
        let (fg, bg) = one_dark;
        let floor = (relative_luminance(bg) + 0.05) * 1.4 - 0.05;
        assert!(
            worst_bed_contrast_at(fg, bg, floor) < 4.8,
            "the control: One Dark under the removed floor read {:.3}:1",
            worst_bed_contrast_at(fg, bg, floor)
        );
    }

    #[test]
    fn the_visibility_floor_costs_the_bar_nothing_on_nord() {
        // The chroma-first recipe changes what indigo, violet, the blue mids
        // and the crossing look like, not what the text over them reads at:
        // the bar is a function of Y alone and the solver answers from under
        // it. Nord, every position, every level.
        let worst = worst_bed_contrast(NORD_FG, NORD_BG);
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "Nord: the bed composited to {worst:.4}:1; the bar is {BODY_CONTRAST_BAR}:1"
        );
        assert!(
            worst - BODY_CONTRAST_BAR <= 0.10,
            "Nord: the bed's worst stop composites at {worst:.4}:1 — light left under the bar"
        );
        // The scanner's colour floor (paint-conformance `SAT_MIN` 60 /
        // `SAT_SPREAD` 40) is cleared by every stop at the cap and at the
        // cold key's share of it.
        for cov in [UNDER_COV_CAP as u8, (UNDER_COV_CAP * BODY_COLD_SHARE) as u8] {
            for i in 0..=512u32 {
                let t = i as f32 / 512.0;
                let ink = bed_ink(spectrum(t), bed_luma_budget(NORD_FG));
                let lit = over_premul(NORD_BG, premul_rgb(ink, cov), cov);
                assert!(
                    max_channel(lit) >= 60 && max_channel(lit) - min_channel(lit) >= 40,
                    "Nord at cov {cov}, t {t:.3}: #{lit:06X} is under the scanner's colour floor"
                );
            }
        }
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

    /// The green→blue crossing is smoothed out of its own leg's cells.
    ///
    /// The band is read one arc entry per cell, so the eye judges the step
    /// from one flat slab to the next. Under the identity walk the green→blue
    /// handoff is about 1.2 cells wide and one cell can carry most of it, and
    /// whether a cell lands inside depends on the cohort's walk origin. So
    /// the bound is taken over every phase of the walk, against the same
    /// walk's identity.
    ///
    /// Nothing else moves: every step whose midpoint is outside green→blue is
    /// unchanged to the bit, and every named anchor keeps the phases that
    /// land within ten entries of it.
    ///
    /// Twins: make `walk_pace` the identity and the worst step comes back
    /// (the ratio bound); widen [`CROSS_GUARD`] past its leg and the
    /// neighbours move (the identity bound).
    #[test]
    fn the_crossing_is_smoothed_out_of_its_own_legs_cells_and_nothing_else_moves() {
        let budget = bed_luma_budget(NORD_FG);
        let span = (crate::spectrum::SPECTRUM_LUT_LEN - 1) as f32;
        let green = crate::spectrum::SPECTRUM_ANCHOR_AT[3] as f32 / span;
        let blue = crate::spectrum::SPECTRUM_ANCHOR_AT[4] as f32 / span;
        // Ten entries: the window an anchor's residency is judged on.
        let win = 10.0 / span;
        let (mut worst_bed, mut worst_rail) = (0.0f32, 0.0f32);
        let (mut was_bed, mut was_rail) = (0.0f32, 0.0f32);
        let (mut at_bed, mut moved_off_leg) = (0.0f32, 0usize);
        let mut res = [(0usize, 0usize); crate::spectrum::SPECTRUM_STOPS];
        let mut all = Vec::new();
        // 64 phases of the cohort's own walk origin × 16 cells of a pass.
        for p in 0..64 {
            let t0 = p as f32 / 64.0 / WALK_FAST_CELLS;
            let arc = |d: i32| walk_arc(t0 + walk_t(d as f32));
            let was = |d: i32| clamp01(tri(t0 + walk_t(d as f32)));
            for (i, &at) in crate::spectrum::SPECTRUM_ANCHOR_AT.iter().enumerate() {
                let a = at as f32 / span;
                res[i].0 += usize::from((0..=16).any(|d| (was(d) - a).abs() <= win));
                res[i].1 += usize::from((0..=16).any(|d| (arc(d) - a).abs() <= win));
            }
            for d in 0..16 {
                let (db, dr) = arc_step(arc(d), arc(d + 1), budget);
                let (wb, wr) = arc_step(was(d), was(d + 1), budget);
                all.push(db);
                if db > worst_bed {
                    worst_bed = db;
                    at_bed = 0.5 * (arc(d) + arc(d + 1));
                }
                worst_rail = worst_rail.max(dr);
                was_bed = was_bed.max(wb);
                was_rail = was_rail.max(wr);
                // Off the named leg: a step the shipped walk placed outside
                // green→blue must be the shipped step, to the bit.
                let mid = 0.5 * (was(d) + was(d + 1));
                if !(green..=blue).contains(&mid)
                    && ((db - wb).abs() > 1e-4 || (dr - wr).abs() > 1e-4)
                {
                    moved_off_leg += 1;
                }
            }
        }
        all.sort_by(f32::total_cmp);
        let median = all[all.len() / 2];
        println!(
            "walk step over 64 phases: median {median:.1}, worst bed {worst_bed:.1} (was {was_bed:.1}, arc {at_bed:.3}), worst rail {worst_rail:.1} (was {was_rail:.1})"
        );
        println!("anchor residency (shipped, paced) of 64: {res:?}");
        // The crossing arrives here already paced by `crate::spectrum`
        // (capped at `SPECTRUM_CROSSING_SHARE_CAP` so yellow keeps its share);
        // the walk-level re-pace buys only what the composited ink adds,
        // which the spectrum's pace of the raw table cannot see. The bounds
        // below are that residue, and the identity walk still fails them.
        assert!(
            worst_bed <= 0.95 * was_bed,
            "the crossing still carries {worst_bed:.1} of the bed at arc {at_bed:.3} against the identity walk's {was_bed:.1} — it is a block, not a step"
        );
        assert!(
            worst_rail <= 0.92 * was_rail,
            "the crossing still carries {worst_rail:.1} of the RAIL against the identity walk's {was_rail:.1} — it is a block below the row too"
        );
        // And the size, absolutely: a ratio alone would pass an arc that got
        // worse in both terms together, so the worst steps are also pinned
        // against fixed ceilings on this census.
        assert!(
            worst_bed <= 76.0,
            "the crossing carries {worst_bed:.1} of the bed at arc {at_bed:.3} — worse than the shipped v0.86.0 arc's own 73.3, so the cap has overpaid"
        );
        assert!(
            worst_rail <= 60.0,
            "the crossing carries {worst_rail:.1} of the RAIL — the shipped v0.86.0 arc read 129.2 here and the cyan route must keep most of that fall"
        );
        assert_eq!(
            moved_off_leg, 0,
            "the re-pace reached outside green→blue: {moved_off_leg} steps the shipped walk put on another leg moved"
        );
        for (i, (was, now)) in res.iter().enumerate() {
            assert_eq!(
                was, now,
                "anchor {i} lost its residency: {was} phases of 64 land within ten entries of it on the shipped walk, {now} on this one"
            );
        }
        // …and the warm end is untouched: red, orange and yellow sit on
        // exactly the cells they always did.
        for d in 0..=4u16 {
            let t = walk_t(f32::from(d));
            assert!(
                (walk_arc(t) - clamp01(tri(t))).abs() < 1e-6,
                "cell {d} moved off its shipped stop: the re-pace reached the warm end"
            );
        }
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
    fn the_walk_lays_a_sixteenth_per_cell_for_sixteen_cells_then_a_thirty_sixth() {
        assert!(walk_t(0.0).abs() < 1e-6);
        assert!((walk_t(1.0) - 1.0 / WALK_FAST_CELLS).abs() < 1e-6);
        assert!((walk_t(16.0) - 1.0).abs() < 1e-6);
        assert!((walk_t(17.0) - walk_t(16.0) - WALK_LAY_RATE).abs() < 1e-6);
        assert!((walk_t(52.0) - 2.0).abs() < 1e-6);
        assert!(
            (walk_t(-2.0) + 2.0 / WALK_FAST_CELLS).abs() < 1e-6,
            "a reach cell continues the fast phase backwards"
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

    /// The reach stops where the hand stopped: a mark two cells short of the
    /// four-letter span wants `col0 - 1` and `col0 - 2`, which at the start of
    /// a shell line are the prompt. [`Ribbon::hand_floor`] bounds it; without
    /// that floor this test fails at column 27.
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
        // Past the rest and two reach beats — where the glass census found
        // the stray cells.
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

    /// A column is only a place on its own row: [`Ribbon::abandon`] stamps the
    /// jump's landing column only into cohorts on that row, so a wrapped
    /// line's earlier row is not slid left to a column read off the row
    /// below.
    ///
    /// The jump lands outside the hand's lit run; one back into it would be
    /// a [`Ribbon::scrub`] and abandon nothing.
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

    /// One row, one walk (`RAINBOW-KITTY-V2.md` §42): B is minted beside A
    /// while A is still lit on the row, so it takes A's walk by position,
    /// thirty cells in at column 40, on the slow leg. The reach extends B's
    /// own cohort and continues B's own walk backwards at `1/36` a cell.
    #[test]
    fn the_reach_extends_its_own_cohort_and_continues_its_own_walk() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A: cols 10..17. B: cols 40..41, two cells short of four letters, so
        // at its second reach beat it wants cols 39 and 38, with A still on
        // the row to its left.
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 2,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, at(t0, 1000), keys, &c);
        // Two reach beats in: the rest (0.90 s at the floor) plus 0.10 s.
        let now = at(t0, 1060 + (PHRASE_REST_MIN_S * 1000.0) as u64 + 100);
        let cx = ctx(now, &c, (2, 42), 0.0);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().any(|l| l.row == 2 && l.col < 18),
            "A must still be on the row for the pin to mean anything"
        );
        let b_id = rib
            .cells()
            .iter()
            .find(|l| l.col == 40)
            .expect("B's first cell")
            .cohort;
        let t40 = rib.field_at(2, 40).expect("B's field");
        let a = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.col0 == 10)
            .expect("A's cohort");
        let b = *rib
            .cohorts()
            .iter()
            .find(|k| k.id == b_id)
            .expect("B's cohort");
        assert_eq!(
            (b.anchor_col, b.origin_walk()),
            (40, a.walk_at(40)),
            "the premise: B is on A's walk at its own column (one row, one walk), thirty \
             cells in"
        );
        for (col, back) in [(39u16, 1.0f32), (38, 2.0)] {
            let cell = rib
                .cells()
                .iter()
                .find(|l| l.row == 2 && l.col == col)
                .unwrap_or_else(|| panic!("the reach must have laid col {col}"));
            assert_eq!(
                cell.cohort, b_id,
                "the reach cell at {col} joined the wrong cohort"
            );
            assert!(!cell.typing, "a reach cell is not typing light");
            assert!(
                (cell.t - (t40 - back * WALK_LAY_RATE)).abs() < 1e-5
                    && (cell.t - b.t_at(col)).abs() < 1e-6,
                "the reach at {col} must continue B's own walk backwards: {} where B's walk \
                 is {} ({t40} at 40, one `1/36` step a cell)",
                cell.t,
                b.t_at(col)
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

    /// The surge (§23): a key born on a change of speed wears a longer crisp
    /// edge, and an open theme raises the floor to the same ceiling. The
    /// price ([`edge_cells`]) and then the pixels: a surged head's hairline
    /// reaches further behind the hand than a steady head's, and neither
    /// past [`HOT_EDGE_CELLS_MAX`].
    ///
    /// The alpha law and the transient cap are untouched: the same falloff
    /// over a longer `d`, so a surged edge is longer, never brighter; the
    /// peak coverage is `HOT_EDGE_COV_MAX` either way.
    #[test]
    fn the_surge_stretches_the_edge_on_a_change_of_speed() {
        // -- 1. the price: `3 + 4·Δ/0.5`, capped at 5 -----------------------
        assert!(
            (edge_cells(0.0, 0.0) - HOT_EDGE_CELLS).abs() < 1e-6,
            "no surge and no flow is the constant the theme had before flow"
        );
        // The spine hands the rise over normalised by `SURGE_FULL_RISE`; the
        // gate rise is 0.15 of it, and `3 + 4·0.15/0.5 = 4.2`.
        let gate = SURGE_MIN_RISE / SURGE_FULL_RISE;
        assert!(
            (edge_cells(gate, 0.0) - (HOT_EDGE_CELLS + 4.0 * SURGE_MIN_RISE / 0.5)).abs() < 1e-5,
            "the gate rise buys {} cells, want 3 + 4·0.15/0.5",
            edge_cells(gate, 0.0)
        );
        assert!(
            (edge_cells(1.0, 0.0) - HOT_EDGE_CELLS_MAX).abs() < 1e-6,
            "the full rise buys the whole stretch"
        );
        assert!(
            (edge_cells(0.0, 1.0) - HOT_EDGE_CELLS_MAX).abs() < 1e-6,
            "an OPEN theme raises the floor from 3 to 5 with no surge at all"
        );
        assert!(
            edge_cells(0.0, 0.5) > HOT_EDGE_CELLS && edge_cells(0.0, 0.5) < HOT_EDGE_CELLS_MAX,
            "the floor LERPS as the theme opens; it does not snap"
        );

        // -- 2. the pixels: how far the hairline actually reaches -----------
        let dark = cfg(true, true);
        let g = geom();
        // The furthest a hot-edge quad sits behind the head, in cells, for a
        // ten-key run whose last key was born with `surge`.
        let reach_of = |surge: f32, heat: f32| -> f32 {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            for i in 0..10u16 {
                let now = at(t0, u64::from(i) * 60);
                let mut cx = ctx_in(now, &dark, (2, 9 + i), 0.9, g);
                if i == 9 {
                    cx.surge = surge;
                    cx.flow.heat = heat;
                }
                rib.on_event(&typed(), now, &cx);
                rib.plan(&cx);
            }
            let cx = ctx_in(at(t0, 9 * 60), &dark, (2, 18), 0.9, g);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            assert!(!sink.out.is_empty(), "a hot hand must leave a hot edge");
            let head_x = f32::from(g.origin_x) + 18.0 * g.cw as f32;
            let far = sink
                .out
                .iter()
                .map(|q| head_x - f32::from(q.x))
                .fold(0.0f32, f32::max);
            far / g.cw as f32
        };
        let steady = reach_of(0.0, 0.0);
        let surged = reach_of(1.0, 0.0);
        let flowing = reach_of(0.0, 1.0);
        assert!(
            surged > steady + 0.9,
            "a surged head reaches {surged:.2} cells behind the hand, a steady one {steady:.2} — the stretch is not on the glass"
        );
        assert!(
            (flowing - surged).abs() < 0.01,
            "an open theme reaches the same ceiling as a full surge ({flowing:.2} vs {surged:.2})"
        );
        assert!(
            surged <= HOT_EDGE_CELLS_MAX,
            "the stretched edge ran past its own ceiling ({surged:.2} cells)"
        );
        assert!(
            steady <= HOT_EDGE_CELLS,
            "a steady head must reach exactly what it always did ({steady:.2} cells)"
        );
    }

    /// A cold hand carries the hot edge at its floor
    /// ([`HOT_EDGE_GAIN_FLOOR`]) and a hot one the whole of it; a light
    /// ground carries none.
    #[test]
    fn a_cold_hand_carries_the_edge_s_floor_and_a_light_ground_carries_no_hot_edge() {
        let dark = cfg(true, true);
        let light = cfg(false, true);
        let cold = Ribbon::hot_edge_gain(&dark, 0.0, true);
        assert!(
            (cold - HOT_EDGE_GAIN_FLOOR).abs() < 1e-6,
            "the first key of a word gets the attach: {cold} against the floor {HOT_EDGE_GAIN_FLOOR}"
        );
        // The floor's value, not only its arithmetic: with the floor at 0.0
        // every line above would still hold.
        assert!(
            HOT_EDGE_GAIN_FLOOR > 0.0 && cold > 0.0,
            "the edge EXISTS on a cold hand: floor {HOT_EDGE_GAIN_FLOOR}, cold gain {cold}"
        );
        assert!(
            (Ribbon::hot_edge_gain(&dark, HOT_EDGE_DISP_MIN - 0.01, true) - HOT_EDGE_GAIN_FLOOR)
                .abs()
                < 1e-6,
            "under the disp floor the gain is the floor and nothing more"
        );
        assert!(Ribbon::hot_edge_gain(&dark, 0.9, true) > HOT_EDGE_GAIN_FLOOR + 0.4);
        assert!((Ribbon::hot_edge_gain(&dark, 1.0, true) - 1.0).abs() < 1e-6);
        // A wake's head is no key: no floor, the ramp it always had.
        assert!(
            Ribbon::hot_edge_gain(&dark, 0.0, false) <= 0.0,
            "a cold arrow's wake carries no hairline"
        );
        assert!((Ribbon::hot_edge_gain(&dark, 1.0, false) - 1.0).abs() < 1e-6);
        assert!(
            Ribbon::hot_edge_gain(&light, 1.0, true) <= 0.0,
            "additive white on a paper ground is exactly what L6 forbids"
        );
    }

    #[test]
    fn the_hot_edge_is_the_stop_under_it_lifted_hot_and_brighter_than_the_bed() {
        // The hairline at the hand is the spectrum: the stop under each
        // vertex, lifted toward white to `HOT_EDGE_LUMA_FLOOR`
        // (`hot_edge_ink`) so it outshines any bed it sits on, at the
        // transient cap `HOT_EDGE_COV_MAX`.
        use aterm_render::add_sat;
        let dark = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &dark, 0.9);
        let cx = ctx(at(t0, 8 * 60), &dark, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let head = sink
            .out
            .iter()
            .max_by_key(|q| max_channel(q.color))
            .expect("a hot hand leaves a hot edge");
        assert!(
            max_channel(head.color) - min_channel(head.color) >= 8,
            "the hot edge is white (#{:06X}) — the owner asked for the rainbow at the hand",
            head.color
        );
        let dominant = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            if r >= g && r >= b {
                'r'
            } else if g >= b {
                'g'
            } else {
                'b'
            }
        };
        let t = rib.field_at(2, 17).expect("the head cell is laid");
        let stop = spectrum(walk_arc(t));
        assert_eq!(
            dominant(head.color),
            dominant(stop),
            "the hairline's hue (#{:06X}) is not the head's stop (#{stop:06X})",
            head.color
        );
        // Brighter than the bed it rides: the head's stop at the bed's
        // ceiling, plus the hairline's premultiplied light on top.
        let cov = UNDER_COV_CAP as u8;
        let bed = over_premul(
            DEFAULT_BG,
            premul_rgb(bed_ink(stop, bed_luma_budget(DEFAULT_FG)), cov),
            cov,
        );
        let lit = add_sat(bed, head.color);
        assert!(
            relative_luminance(lit) >= 1.5 * relative_luminance(bed),
            "the hot edge over its bed (#{lit:06X}, Y {:.3}) is not brighter than the bed (#{bed:06X}, Y {:.3})",
            relative_luminance(lit),
            relative_luminance(bed)
        );
        // Priced for emphasis: the hairline's 1-px anti-aliasing puts about
        // two thirds of the request on its brighter row at this fixture, so
        // the pin is on the emitted peak.
        let peak = peak_channel(&sink.out);
        assert!(
            peak > 39,
            "the hairline's brightest emitted level is {peak}, want above 39"
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
        // Start past the head cell's one sanctioned ramp-in (T3's 18 ms
        // `edge-in`); after it the mark may only spend light, even with a
        // spine that keeps climbing.
        //
        // The body's law is stated on the request (the coverage asked for at
        // each boundary): the rasterized peak may move a level as the wave's
        // sub-pixel phase settles. The hot edge's law is stated on what it
        // emits, since it is additive and has no request on the plan.
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
            // The head vertex is pinned (`WAVE_HEAD_PIN`), so the hairline's
            // brightest slab is byte-static; the vertex one cell behind rides
            // a quarter of the wave's ~1 px, which the AA can move by a few
            // levels. A gain read off the live spine would climb tenfold.
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
        let retract_start = RETRACT_START_S * 1000.0;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        for ms in ((retract_start as u64)..total + 20).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
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
    fn the_retract_drains_the_tail_before_the_head() {
        // §4's drain picks cells farthest from the head first, so the ending
        // reads as the mark drawn back into the caret.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let start = RETRACT_START_S;
        let now = at(last, ((start + RETRACT_DUR_S * 0.5) * 1000.0) as u64);
        let cx = ctx(now, &c, (2, 18), 0.0);
        rib.plan(&cx);
        let plan = rib.plan_segments();
        let slabs = rib.slabs_per_cell();
        let n = plan.len();
        assert!(n > 2 * slabs, "the mark must still be planned");
        let tail = plan[slabs].cov;
        let head = plan[n - 1 - slabs].cov;
        assert!(
            tail < head,
            "the retract drained the HEAD first (tail {tail}, head {head})"
        );
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

    /// The insert's rewrite ([`Event::Rewrite`]) retracts the row's suffix
    /// from the given column (not `ctx.caret`, which the host has not moved
    /// yet) farthest-first inside `12·n + 240` ms, the kill's law, and moves
    /// the ribbon's caret there. Cells left of the column are untouched.
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
        // Forty cells over 2.4 s: the oldest are past the 70 % of their life
        // where a life clamp would have stepped the melt.
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
    fn a_jump_lays_its_wake_counted_back_from_the_landing_and_never_the_whole_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A cold Ctrl-A across 108 columns: WAKE_MAX_CELLS beside the caret,
        // the landing included, each born on the flight clock (cell `j` from
        // the origin at `T · j / n`, `T = flight_ms(108)` = 120 ms) with the
        // fixed life, never the four-letter chain law's.
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&nav((2, 110), (2, 2)), t0, &cx);
        let cells = row_cells(&rib, 2);
        let cols: Vec<u16> = cells.iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (2..2 + WAKE_MAX_CELLS).collect::<Vec<_>>(),
            "a leftward jump's wake runs from the landing rightward, capped"
        );
        let flight = flight_ms(108.0) / 1000.0;
        assert!((flight - 0.12).abs() < 1e-6, "the clamp: 120 ms");
        for &(col, typing, life, born) in &cells {
            assert!(!typing, "col {col}: a wake cell is never typing");
            assert!(
                (life - WAKE_LIFE_S).abs() < 1e-6,
                "col {col}: fixed life, got {life}"
            );
            let j = f32::from(110 - col);
            let want = t0 + Duration::from_secs_f32(flight * j / 108.0);
            let off = born
                .saturating_duration_since(want)
                .max(want.saturating_duration_since(born));
            assert!(
                off <= Duration::from_millis(1),
                "col {col}: born on the flight clock at T·{j}/108 (off by {off:?})"
            );
        }
        let (first, last) = (cells[cells.len() - 1].3, cells[0].3);
        assert!(
            first < last,
            "the cell nearest the origin is born first, the landing last"
        );
        assert_eq!(rib.cohorts().len(), 1, "one cohort, the wake's own");
        // A cold Ctrl-E the other way, once the first wake is gone: the wake
        // runs from the landing leftward, and the origin cell at col 2 is
        // beyond the cap, so it is not painted.
        let t1 = at(t0, 3000);
        let cx = ctx(t1, &c, (2, 110), 0.0);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the first wake is out by +3 s");
        rib.on_event(&nav((2, 2), (2, 110)), t1, &cx);
        let cols: Vec<u16> = row_cells(&rib, 2).iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (111 - WAKE_MAX_CELLS..111).collect::<Vec<_>>(),
            "a rightward jump's wake ends AT the landing, capped"
        );
        // The landing is the run's head (newest cell): the hot edge sits by
        // the caret, not at the far end.
        let newest = rib
            .cells()
            .iter()
            .max_by_key(|c| (c.born, c.col))
            .map(|c| c.col);
        assert_eq!(newest, Some(110));
    }

    /// The head of a leftward wake is its landing: after a Ctrl-A the
    /// standing hot edge comes up where the caret is, over the landing cell
    /// and reaching rightward over the corridor, not at the corridor's far
    /// end. `head_col`'s wake clause makes the landing the head of a
    /// `Cohort::wake` run with `stream_dir` +1, and the edge is measured from
    /// the landing's left edge rightward. The typed-cohort rule (`Run::head`)
    /// is untouched, and the rightward mirror (a Ctrl-E) keeps the caret's
    /// own cell as its head.
    #[test]
    fn a_leftward_wake_s_hot_edge_sits_at_the_landing_not_where_the_caret_left() {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let jump = at(last, 8);
        let cx = ctx(jump, &c, (2, 0), 0.9);
        rib.on_event(&nav((2, 42), (2, 0)), jump, &cx);
        rib.plan(&cx);
        let mut sink = Sink::default();
        let now = at(jump, 150);
        let cx = ctx(now, &c, (2, 0), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !sink.out.is_empty(),
            "a hot hand's wake must carry a hot edge once its cells are born"
        );
        let landing_x = f32::from(g.origin_x);
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for q in &sink.out {
            let x = f32::from(q.x);
            let x1 = x + f32::from(q.w);
            lo = lo.min(x);
            hi = hi.max(x1);
            assert!(
                x >= landing_x - 1.0,
                "the hot edge lies BEHIND the landing after a Ctrl-A (x = {x})"
            );
            assert!(
                x <= landing_x + HOT_EDGE_CELLS_MAX * cw + f32::from(q.w) + 1.0,
                "the hot edge reached further than {HOT_EDGE_CELLS_MAX} cells from the landing (x = {x})"
            );
        }
        println!("leftward wake hot edge: x ∈ [{lo}, {hi}] (landing {landing_x}, cw {cw})");
        // It reaches over the corridor: past the landing cell's own right
        // edge and at least the base reach, so the eye finds it at the hand.
        assert!(
            hi >= landing_x + HOT_EDGE_CELLS * cw - 1.0,
            "the hot edge does not reach rightward from the landing (right end {hi}, want ≥ {})",
            landing_x + HOT_EDGE_CELLS * cw - 1.0
        );
        assert_eq!(
            rib.runs.first().map(|r| (r.head_col, r.stream_dir)),
            Some((0, 1)),
            "the landing is the head and the stream runs rightward"
        );
        // The mirror: a cold Ctrl-E lays its wake ending at the landing; the
        // caret stands on the landing cell, so the head is the caret's own
        // cell (`caret.col − 1`) and the edge ends at the caret's left edge,
        // within five cells left of col 43's left edge.
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (2, 42), 0.9);
        rib.on_event(&nav((2, 2), (2, 42)), t0, &cx);
        let now = at(t0, 150);
        let cx = ctx(now, &c, (2, 42), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !sink.out.is_empty(),
            "the Ctrl-E's wake must carry a hot edge"
        );
        let edge = 43.0 * cw;
        for q in &sink.out {
            let x = f32::from(q.x);
            assert!(
                x <= edge + 1.0 && edge - x <= HOT_EDGE_CELLS_MAX * cw + f32::from(q.w) + 1.0,
                "a rightward wake's hot edge left the five cells before the landing's right edge (x = {x})"
            );
        }
        assert_eq!(
            rib.runs.first().map(|r| (r.head_col, r.stream_dir)),
            Some((41, -1)),
            "a rightward wake keeps the own-cell head and a leftward stream"
        );
    }

    #[test]
    fn a_wake_cell_is_dark_until_it_is_born_then_takes_the_one_attack() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&nav((2, 60), (2, 2)), t0, &cx);
        // The flight clock (K3): 58 cells fly `T = flight_ms(58)` = 108 ms.
        // The corridor is capped at 32 cells back from the landing, so the
        // first cell laid is col 33 (`j = 27`), born at `T · 27/58` = 50 ms;
        // the landing (col 2, `j = 58`) is born at `T`.
        let flight_ms_ = flight_ms(58.0) as u64;
        let first_ms = (flight_ms(58.0) * 27.0 / 58.0) as u64;
        let edge_ms = (EDGE_IN_S * 1000.0) as u64;
        for ms in [0u64, first_ms / 2, first_ms - 1] {
            let cx = ctx(at(t0, ms), &c, (2, 2), 0.0);
            rib.plan(&cx);
            assert_eq!(
                plan_peak(&rib),
                0,
                "+{ms} ms: a wake cell is dark before its birth"
            );
            assert!(
                !rib.at_rest(),
                "+{ms} ms: …but it is laid, so the host keeps ticking"
            );
        }
        // Half-way through the flight the corridor is lit behind the head
        // and dark ahead of it: the landing's own cell is still unborn.
        let cx = ctx(at(t0, flight_ms_ / 2 + 10), &c, (2, 2), 0.0);
        rib.plan(&cx);
        assert!(plan_peak(&rib) > 0, "the corridor is lit behind the head");
        let landing = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 2)
            .expect("the landing cell is laid");
        assert_eq!(
            rib.env_of(&cx, landing),
            0.0,
            "…and dark ahead of it: the landing is unborn until T"
        );
        let cx = ctx(at(t0, flight_ms_ + edge_ms + 2), &c, (2, 2), 0.0);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= (UNDER_COV_CAP * BODY_COLD_SHARE) as u8 - 2,
            "one attack past the landing's birth the wake is at its cold ceiling ({})",
            plan_peak(&rib)
        );
        assert!(
            rib.env_of(
                &cx,
                rib.cells().iter().find(|l| l.col == 2).expect("landing")
            ) > 0.99,
            "the landing cell is at full one edge-in past T"
        );
        // …and it is out one life after the last birth, through its own melt.
        let cx = ctx(
            at(t0, flight_ms_ + (WAKE_LIFE_S * 1000.0) as u64 + 20),
            &c,
            (2, 2),
            0.0,
        );
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the wake is gone one WAKE_LIFE_S after its last birth"
        );
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
        // Forty cells at cols 12..52 over 2.4 s, then a Ctrl-A to column 2:
        // the corridor crosses ten dark cells before the band, so the jump
        // leaves the run and the band under the corridor is covered by the
        // wake. It keeps every stop (C2), loses no light on the jump frame,
        // has one owner per cell, and outlives the jump on the wake's clock
        // instead of the abandon's 0.64 s retract.
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
            let j = f32::from(52 - col);
            let want = now + Duration::from_secs_f32(flight * j / 50.0);
            assert!(
                over[0].born.saturating_duration_since(want) <= Duration::from_millis(1)
                    && want.saturating_duration_since(over[0].born) <= Duration::from_millis(1),
                "col {col}: born on the flight clock"
            );
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
    fn a_wake_is_never_typing_and_holds_no_other_cohort() {
        // The flag pin: `typing: false` keeps "a jump builds no momentum"
        // (mod.rs) true and keeps a wake from lifting another mark's finger.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A band on row 3, then a hop on row 2 (no abandon): row 3's clock is
        // untouched, and every row-2 cell is a non-typing wake cell.
        let keys = Keys {
            g: geom(),
            row: 3,
            col0: 10,
            n: 6,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, t0, keys, &c);
        let row3_alive = rib
            .cohorts()
            .iter()
            .find(|c| c.row == 3)
            .map(|c| c.alive_at);
        let now = at(t0, 400);
        let cx = ctx(now, &c, (2, 21), 0.8);
        rib.on_event(&nav((2, 20), (2, 21)), now, &cx);
        assert!(rib.cells().iter().filter(|c| c.row == 2).all(|c| !c.typing));
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|c| c.row == 3)
                .map(|c| c.alive_at),
            row3_alive,
            "a hop's wake holds only its own cohort"
        );
        // A jump's wake is not typing either, and `field_at_caret`'s fallback
        // still reads real typing: the jump wrote no hand colour.
        let now = at(t0, 800);
        let cx = ctx(now, &c, (2, 2), 0.8);
        rib.on_event(&nav((2, 60), (2, 2)), now, &cx);
        assert!(rib.cells().iter().filter(|c| c.row == 2).all(|c| !c.typing));
        assert!(rib.cells().iter().any(|c| c.row == 3 && c.typing));
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

    /// A partial reversal re-owns the corridor instead of abandoning it (K4:
    /// no second wake cohort on the row). A jump back that stops short of the
    /// corridor it reverses leaves the far part uncovered; that part stays in
    /// the re-owned cohort ([`Ribbon::own_wake_cohort`]) rather than draining
    /// at once on its own clock, so the lit set stays one interval (K2). A
    /// hop back over still-lit text is a scrub and never reaches this.
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
        // A word at cols 10..17, the caret at 18; Right from 18 to 19 crosses
        // the empty cell 18 (blank ground, so a hop and not a scrub) and lays
        // 18 and 19 in a wake cohort of its own on the word's walk, never in
        // the word's cohort, whose clock, phase and bounds an arrow over blank
        // ground must not touch. (Left onto the word is a scrub:
        // `a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row`.)
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

    /// A jump into a band already leaving (K1 / M1): forty cells, a Ctrl-A
    /// inside the swoosh window and past it. Nothing is taken over: the
    /// band's cells and clock are byte-identical after the jump, the corridor
    /// is laid over every column from the level the band has there, rising
    /// through `edge-in` with no step, and the corridor's width does not
    /// depend on how far the band has drained.
    #[test]
    fn a_jump_into_a_band_already_leaving_lays_its_wake_over_it_from_the_level_it_has() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * 0.9;
        let cw = geom().cw as f32;
        // The band's own boundaries end at the head cell's right edge (42 cw);
        // the attach under the caret cell is the caret's light and leaves with
        // the caret on the jump frame, so the no-lurch comparison below
        // filters it out.
        let lit_xs = |rib: &Ribbon| -> Vec<(i32, u8)> {
            rib.plan_segments()
                .iter()
                .filter(|s| s.cov > 0 && s.x <= 42.0 * cw + 0.5)
                .map(|s| ((s.x * 16.0).round() as i32, s.cov))
                .collect()
        };
        let rs = (RETRACT_START_S * 1000.0).round() as u64;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        let flight = flight_ms(40.0) / 1000.0;
        let mut laid = Vec::new();
        for idle_ms in [rs + 100, rs + 350, rs + 500, total + 60] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, 40, &c, 0.9);
            let last = at(t0, 39 * 60);
            let now = at(last, idle_ms);
            let cx = ctx(now, &c, (2, 42), 0.9);
            rib.plan(&cx);
            let band = rib.cohorts().first().copied();
            let before: Vec<Cell> = rib.cells().to_vec();
            // The level of every band cell at the jump instant: the overlay is
            // born from it (`rearm`), and `> 0.0` on it is the lit/empty split.
            let level_before: Vec<(u16, f32)> =
                before.iter().map(|l| (l.col, rib.env_of(&cx, l))).collect();
            // And what of it is on the glass: past the cap the corridor
            // covers only light the census reads as ink
            // (`Ribbon::reads_as_glass_light`). Within the cap every column is
            // laid, lit or dark.
            let shown_before: Vec<(u16, f32)> = before
                .iter()
                .map(|l| (l.col, l.cov0 * rib.env_of(&cx, l)))
                .collect();
            let on_glass = |col: u16| {
                shown_before
                    .iter()
                    .any(|&(c, share)| c == col && Ribbon::reads_as_glass_light(share))
            };
            let xs_before = lit_xs(&rib);
            let cx = ctx(now, &c, (2, 2), 0.9);
            rib.on_event(&nav((2, 42), (2, 2)), now, &cx);
            rib.plan(&cx);
            // No lurch: every boundary the leaving band had is exactly where
            // it was on the jump frame; the corridor is unborn, and the
            // band's retract keeps pulling toward the caret it began under.
            assert_eq!(
                xs_before,
                lit_xs(&rib),
                "idle {idle_ms}: the leaving band moved on the jump frame"
            );
            let band_id = band.map(|b| b.id);
            // K1: every band cell is byte-identical, and its clock untouched.
            for was in &before {
                let now_cell = rib
                    .cells()
                    .iter()
                    .find(|l| l.layer == Layer::Base && l.col == was.col)
                    .unwrap_or_else(|| {
                        panic!("idle {idle_ms}: the band's cell at {} is gone", was.col)
                    });
                assert!(
                    now_cell.cohort == was.cohort
                        && now_cell.born == was.born
                        && now_cell.cov0 == was.cov0
                        && now_cell.retract_at == was.retract_at,
                    "idle {idle_ms}: the wake touched the band's cell at col {}",
                    was.col
                );
            }
            if let Some(b) = band {
                let b_now = rib
                    .cohorts()
                    .iter()
                    .find(|k| k.id == b.id)
                    .expect("the band stays in the pool");
                assert_eq!(
                    b_now.alive_at, b.alive_at,
                    "idle {idle_ms}: the band keeps its clock"
                );
                assert!(b_now.phase.is_retracting() || idle_ms < rs);
            }
            // The corridor: one wake cohort on `Over`, over every column
            // within the cap and every lit column beyond it; from the band's
            // level where the band is lit, from dark where it is not.
            let wake: Vec<Cell> = rib
                .cells()
                .iter()
                .filter(|l| l.layer == Layer::Over)
                .copied()
                .collect();
            let wake_id = wake.first().map(|l| l.cohort);
            assert!(wake_id.is_some() && wake_id != band_id);
            let mut wake_cols: Vec<u16> = wake.iter().map(|l| l.col).collect();
            wake_cols.sort_unstable();
            let want: Vec<u16> = (2..42u16)
                .filter(|col| col - 2 < WAKE_MAX_CELLS || on_glass(*col))
                .collect();
            assert_eq!(
                wake_cols, want,
                "idle {idle_ms}: the corridor covers the cap and every cell the glass shows beyond it"
            );
            for w in &wake {
                assert!(Some(w.cohort) == wake_id, "idle {idle_ms}: one wake cohort");
                assert!(!w.typing);
                let j = f32::from(42 - w.col);
                let want = now + Duration::from_secs_f32(flight * j / 40.0);
                assert!(
                    w.born.saturating_duration_since(want) <= Duration::from_millis(1)
                        && want.saturating_duration_since(w.born) <= Duration::from_millis(1),
                    "idle {idle_ms}: col {} is born on the flight clock",
                    w.col
                );
                let under = before.iter().find(|l| l.col == w.col);
                let env = level_before
                    .iter()
                    .find(|&&(c, _)| c == w.col)
                    .map_or(0.0, |&(_, e)| e);
                match (under, env > 0.0) {
                    (Some(u), true) => {
                        let (re_at, level) = w.rearm.unwrap_or_else(|| {
                            panic!("idle {idle_ms}: col {} over lit light is re-armed", w.col)
                        });
                        assert_eq!(re_at, w.born);
                        let start = u.cov0 * env;
                        assert!(
                            (w.cov0 * level - start).abs() < 1e-5 && w.cov0 >= cov0_new - 1e-6,
                            "idle {idle_ms}: col {} starts at the band's coverage {start} (cov0 {} level {level})",
                            w.col,
                            w.cov0
                        );
                    }
                    _ => {
                        assert!(
                            w.rearm.is_none() && (w.cov0 - cov0_new).abs() < 1e-6,
                            "idle {idle_ms}: col {} over nothing is a cold-priced cell from dark",
                            w.col
                        );
                    }
                }
            }
            laid.push(wake.len());
            // M1 at one covered column: the light there does not step when the
            // corridor cell is born over the draining band, sampled one frame
            // either side of its birth.
            if let Some(w) = wake.iter().find(|l| l.col == 20)
                && level_before.iter().any(|&(c, e)| c == 20 && e > 0.3)
            {
                let cov_at = |rib: &mut Ribbon, t: Instant| -> u8 {
                    let cx = ctx(t, &c, (2, 2), 0.9);
                    rib.plan(&cx);
                    cell_cov(rib, geom(), 20)
                };
                let pre = cov_at(&mut rib, w.born - Duration::from_millis(1));
                let post = cov_at(&mut rib, w.born + Duration::from_millis(1));
                assert!(
                    post + 4 >= pre,
                    "idle {idle_ms}: col 20 stepped down at the corridor's birth ({pre} → {post})"
                );
            }
            // Through the wake's life the pool holds only the band's cells
            // draining on their own clock and the wake's, and no band cell
            // remains once its swoosh is over.
            for dt in [50u64, 150, 400, 800] {
                let cx = ctx(at(now, dt), &c, (2, 2), 0.9);
                rib.plan(&cx);
                for l in rib.cells() {
                    assert!(
                        Some(l.cohort) == band_id || Some(l.cohort) == wake_id,
                        "idle {idle_ms} +{dt}: a cell of a third cohort at col {}",
                        l.col
                    );
                }
                if idle_ms + dt > total {
                    assert!(
                        rib.cells().iter().all(|l| Some(l.cohort) != band_id),
                        "idle {idle_ms} +{dt}: the band must be out {SWOOSH_TOTAL_S} s after its last key"
                    );
                }
            }
            let cx = ctx(at(now, 1300), &c, (2, 2), 0.9);
            rib.plan(&cx);
            assert!(
                rib.at_rest(),
                "idle {idle_ms}: everything is out 1.3 s after the jump"
            );
        }
        // The corridor's width does not depend on how far the band has
        // drained: the cap plus whatever the band still lights beyond it,
        // 40 while any of cols 34..42 are lit, the bare cap once it is gone.
        assert_eq!(laid[0], 40, "inside the retract the whole band is covered");
        assert!(
            laid.windows(2).all(|w| w[0] >= w[1]),
            "non-increasing in idle: {laid:?}"
        );
        assert_eq!(
            laid[3],
            usize::from(WAKE_MAX_CELLS),
            "past the swoosh the cold Ctrl-A lays the cap"
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

    /// The cap binds at the corridor's far edge, never inside it (v3 §2.10,
    /// K2). Two jumps that would otherwise lay two islands with a dark
    /// stretch between:
    ///
    /// * a Ctrl-A across a 100-cell band whose swoosh has drained its far
    ///   half (the cap at the landing, and the remnant by the origin);
    /// * a Ctrl-E back 300 ms after a cold Ctrl-A across 108 dark columns
    ///   (the re-owned first corridor, and the cap at its own landing).
    ///
    /// Each corridor is laid from its landing out to the farthest light it
    /// keeps, and the lit set is one interval holding both ends. A cold
    /// Ctrl-A with nothing lit past the cap still lays exactly the cap.
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

    /// A hop covers a leaving band from its level and leaves a live one as it
    /// is. The scrub refuses a move over a leaving band
    /// ([`Cohort::leaving_at`]), and the hop that takes the move covers it:
    ///
    /// * over a retracting band a Left ×5 lays an `Over` cell at every column
    ///   of its span, each over band light born at exactly the light's
    ///   coverage (M1: no step), and the band's cells and clock stay
    ///   byte-identical (K1);
    /// * over a live band (in its grace) a hop from dark ground back into it
    ///   lays nothing over the band's cells.
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

    /// The per-cell census of `row`'s cells `col0..col1` on this frame: the
    /// identity of each column's owner (`cohort`, `born`, `typing`, `cov0`)
    /// and the coverage the plan puts inside that column (the max `cov` of
    /// every boundary and slab whose `x` falls in it; a retract that pulls a
    /// boundary out of its column moves its light with it). Byte for byte,
    /// so "the scrub damaged nothing" is an equality.
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

    /// Scrubbing never damages the band: Option+Left ×4, Option+Right ×4,
    /// Option+Left ×2 over the words just typed, 150 ms apart, starting
    /// 150 ms after the last key. Every hop stays inside the typed run, so
    /// it is a scrub: it abandons, takes over, lays and dims nothing (the
    /// census of the band's 44 cells is byte-identical after every hop), and
    /// it holds the row as an erase does (`hold_row`): the band leaves
    /// through its swoosh `SWOOSH_TOTAL_S` after the hand's last hop, not its
    /// last key.
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
        // …well into its retract SWOOSH_TOTAL_S − 0.2 s after it, on the
        // hop's clock and not the key's (which would have had it out long
        // ago)…
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

    /// A Home and an End inside the band are scrubs too: forty cells at cols
    /// 2..42, then Ctrl-A (42 → 2), Ctrl-E (2 → 42) and Ctrl-A again, 150 ms
    /// apart. The corridor is the band, so nothing is abandoned or handed
    /// over: every cell keeps its cohort, birth, price and `typing`, one
    /// owner per cell, the census byte-identical, the row held, and the band
    /// leaves one swoosh after the last jump. A jump that leaves the run
    /// still abandons (pinned below).
    #[test]
    fn a_home_and_an_end_inside_the_band_abandon_nothing_and_take_nothing_over() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let band = rib.cohorts()[0];
        let cx = ctx(at(last, 150), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let before = census(&rib, 2, 2, 42);
        let mut caret = (2u16, 42u16);
        let mut last_jump = last;
        for (k, &col) in [2u16, 42, 2].iter().enumerate() {
            let now = at(last, 150 * (k as u64 + 1));
            let to = (2, col);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav(caret, to), now, &cx);
            rib.plan(&cx);
            caret = to;
            last_jump = now;
            let g = gaps(&before, &census(&rib, 2, 2, 42));
            assert!(
                g.is_empty(),
                "jump {} (→{col}): the jump damaged the band at {} of 40 cells: {:?}",
                k + 1,
                g.len(),
                g
            );
            assert_eq!(rib.cohorts().len(), 1, "jump {}: no wake was minted", k + 1);
            assert_eq!(rib.cells().len(), 40, "jump {}: nothing was laid", k + 1);
            for cell in rib.cells() {
                assert!(
                    cell.cohort == band.id && cell.typing && !cell.leaving(),
                    "jump {}: col {} was taken over or sent away",
                    k + 1,
                    cell.col
                );
            }
            let held = rib.cohorts()[0];
            assert!(!held.abandoned, "jump {}: the band is not abandoned", k + 1);
            assert_eq!(held.alive_at, now, "jump {}: the hand holds the row", k + 1);
        }
        let cx = ctx(at(last_jump, 660), &c, caret, 0.9);
        rib.plan(&cx);
        assert!(
            gaps(&before, &census(&rib, 2, 2, 42)).is_empty(),
            "0.66 s after the last jump — where the abandon's retract would have drained it — the band is whole"
        );
        let cx = ctx(
            at(last_jump, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            caret,
            0.9,
        );
        rib.plan(&cx);
        assert!(rib.at_rest(), "…and out one swoosh after the last jump");
    }

    /// A jump that leaves the run keeps the abandon law (the scrub's
    /// control). (a) Past the band's oldest cell: forty cells at cols 10..50
    /// and a Ctrl-A to column 0; the corridor crosses ten dark cells, so it
    /// is a jump: the wake lays its corridor as an overlay (K1) and the band,
    /// untouched on `Base`, goes into its retract beneath it. (b) Off the
    /// row: an Up-arrow abandons the band and lays nothing. (c) Past the
    /// band's end: a Right ×6 over blank ground is a hop that leaves the
    /// band's clock alone; the band leaves one swoosh after its last key, and
    /// the hop's cold wake sits behind the caret.
    #[test]
    fn a_jump_past_the_band_s_oldest_cell_or_off_its_row_still_abandons_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        // (a)
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 10,
                n: 40,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 39 * 60);
        let band = rib.cohorts()[0];
        let now = at(last, 150);
        let cx = ctx(now, &c, (2, 0), 0.9);
        rib.on_event(&nav((2, 50), (2, 0)), now, &cx);
        rib.plan(&cx);
        let wake = rib
            .cohorts()
            .iter()
            .find(|k| k.wake)
            .expect("(a): the jump past the band's oldest cell lays its wake");
        assert!(
            (10..50u16).all(|col| rib
                .cells()
                .iter()
                .any(|c| c.col == col && c.cohort == wake.id && c.layer == Layer::Over)),
            "(a): the corridor lies over every cell of the band"
        );
        assert_eq!(
            rib.cells()
                .iter()
                .filter(|c| c.cohort == band.id && c.layer == Layer::Base)
                .count(),
            40,
            "(a): …and the band's own forty cells are still under it (K1)"
        );
        let leaving = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("(a): the band is still in the pool");
        assert!(
            leaving.abandoned,
            "(a): a jump PAST the band's oldest cell abandons it — this is the law"
        );
        assert!(
            (0..10u16).all(|col| rib
                .cells()
                .iter()
                .any(|c| c.col == col && c.cohort == wake.id)),
            "(a): the dark cells before the band are laid"
        );
        // …and it is gone one abandon later, where a scrub would have held
        // it one swoosh past the hand's last act.
        let cx = ctx(at(now, 660), &c, (2, 0), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|c| c.cohort != band.id),
            "(a): the abandoned band is out 0.66 s later"
        );
        // (b)
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let band = rib.cohorts()[0];
        let cx = ctx(now, &c, (1, 42), 0.9);
        rib.on_event(&nav((2, 42), (1, 42)), now, &cx);
        rib.plan(&cx);
        let gone = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("(b): the band is still in the pool");
        assert!(gone.abandoned, "(b): a row change abandons the band");
        assert_eq!(rib.cohorts().len(), 1, "(b): …and lays no wake");
        let cx = ctx(at(now, 660), &c, (1, 42), 0.9);
        rib.plan(&cx);
        assert!(rib.at_rest(), "(b): the abandoned band is out 0.64 s later");
        // (c)
        let mut rib = Ribbon::new();
        let last = type_the_owner_s_line(&mut rib, t0, &c);
        let band = rib.cohorts()[0];
        let now = at(last, 150);
        let cx = ctx(now, &c, (2, 50), 0.8);
        rib.on_event(&nav((2, 44), (2, 50)), now, &cx);
        rib.plan(&cx);
        let held = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("(c): the band");
        assert_eq!(
            held.alive_at, last,
            "(c): a hop off the band's end does not hold the row"
        );
        assert!(
            rib.cohorts().iter().any(|k| k.wake && k.id != band.id),
            "(c): the hop lays its cold wake behind the caret"
        );
        let cx = ctx(
            at(last, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            (2, 50),
            0.8,
        );
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|c| c.cohort != band.id),
            "(c): the band leaves one swoosh after ITS last key"
        );
    }

    /// A scrub holds only the cohorts that own its corridor. Type `hello
    /// world` (cols 0..11); +200 ms a Ctrl-E-shaped jump 11 → 31 (wake W1
    /// over dark ground, band A abandoned); +350 ms a Ctrl-A 31 → 0 (W2 takes
    /// W1 over and fills A's drained holes, A's drained cells staying
    /// resident beneath); +200 ms a Right ×2 0 → 2 over W2's live cells, a
    /// scrub.
    ///
    /// The scrub holds W2 and nothing else. A, shadowed under the corridor,
    /// keeps its clock, phase, abandon and every cell's coverage exactly as
    /// a twin that never saw the scrub, and is pruned when its swoosh ends;
    /// its drained cells never come back to light no key bought.
    #[test]
    fn a_scrub_over_holes_a_hop_filled_leaves_an_abandoned_cohort_exactly_as_it_was() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        // The scrubbed ribbon and its twin: identical up to the scrub, which
        // only `rib` receives; both planned at the same instant with the same
        // caret.
        let mut rib = Ribbon::new();
        let mut twin = Ribbon::new();
        for r in [&mut rib, &mut twin] {
            type_run(r, t0, 0, 11, &c, 0.9);
        }
        let last = at(t0, 10 * 60);
        let band = rib.cohorts()[0];
        for (from, to, ms) in [((2u16, 11u16), (2, 31), 200u64), ((2, 31), (2, 0), 550)] {
            let now = at(last, ms);
            for r in [&mut rib, &mut twin] {
                let cx = ctx(now, &c, to, 0.9);
                r.on_event(&nav(from, to), now, &cx);
                r.plan(&cx);
            }
        }
        let a_before = *rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("A is resident until its swoosh ends");
        assert!(
            a_before.abandoned && a_before.phase.is_retracting(),
            "the setup: A is abandoned and inside its swoosh ({:?})",
            a_before.phase
        );
        let w2 = *rib
            .cohorts()
            .iter()
            .filter(|k| k.wake && !k.abandoned)
            .max_by_key(|k| k.alive_at)
            .expect("W2 owns the corridor");
        assert!(
            rib.cells()
                .iter()
                .any(|l| l.cohort == band.id && (0..2).contains(&l.col) && !l.leaving()),
            "the setup: A's drained cells at cols 0..2 are still resident under W2's"
        );
        // The frame before the scrub, on both.
        let scrub_at = at(last, 750);
        let pre = at(scrub_at, 0)
            .checked_sub(Duration::from_millis(16))
            .expect("clock");
        let cx = ctx(pre, &c, (2, 0), 0.9);
        rib.plan(&cx);
        let cov_pre = census(&rib, 2, 0, 32);
        // The scrub: Right ×2 over W2's live cells.
        let cx = ctx(scrub_at, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 0), (2, 2)), scrub_at, &cx);
        rib.plan(&cx);
        twin.plan(&cx);
        let held = rib
            .cohorts()
            .iter()
            .find(|k| k.id == w2.id)
            .expect("W2 is still in the pool");
        assert_eq!(
            held.alive_at, scrub_at,
            "the scrub holds W2, the cohort that owns the corridor"
        );
        let a_after = *rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("A is still resident on the scrub frame");
        assert!(
            a_after.abandoned
                && a_after.alive_at == a_before.alive_at
                && a_after.retract_col == a_before.retract_col
                && a_after.phase.is_retracting(),
            "the scrub left A's clock, abandon and retract target exactly as they were, still leaving: {a_before:?} → {a_after:?}"
        );
        // A's phase moves only with the clock: it is the twin's, planned at
        // the same instant (Retracting → Fading between the jump and the
        // scrub, on A's own clock).
        let a_twin = twin
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("the twin's A");
        assert!(
            a_twin.alive_at == a_after.alive_at
                && a_twin.phase == a_after.phase
                && a_twin.retract_col == a_after.retract_col,
            "A on the scrubbed ribbon is A on the twin: {a_twin:?} vs {a_after:?}"
        );
        // A's own columns (its cells are the newest owners at cols 2..11; W2
        // filled only the holes at 0..2): byte-identical to the twin's, and
        // no column brighter than the frame before.
        let cov_now = census(&rib, 2, 0, 32);
        let cov_twin = census(&twin, 2, 0, 32);
        let a_cols: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.cohort == band.id && !l.leaving())
            .map(|l| l.col)
            .collect();
        assert!(!a_cols.is_empty(), "A still has resident cells");
        for &col in &a_cols {
            let i = usize::from(col);
            assert_eq!(
                cov_now[i], cov_twin[i],
                "col {col}: A's light after the scrub is the twin's"
            );
            assert!(
                cov_now[i].2 <= cov_pre[i].2,
                "col {col}: A's light did not come back ({} → {})",
                cov_pre[i].2,
                cov_now[i].2
            );
        }
        // A is pruned when its swoosh ends, not one swoosh after the hop.
        let cx = ctx(at(last, 950), &c, (2, 2), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cohorts().iter().all(|k| k.id != band.id),
            "A is gone when its own swoosh ends — the scrub did not renew it"
        );
    }

    /// A scrub into a corridor does not move its soft end. The tail-ease law
    /// puts a run's one-cell feather at one end, and [`Ribbon::head_col`]
    /// resolves the head from the caret. Type `hello world`, Ctrl-E to col 31
    /// (a hop over blank ground), Ctrl-A back to col 0 (a leftward corridor
    /// landing at col 0), then Right Right, a scrub over the corridor's own
    /// cells. `stream_dir` flips +1 → −1 across the scrub.
    ///
    /// The boundary is one expression on every run (the left side owns it,
    /// the right enters at the ease), so a run's soft end is its left one
    /// whichever way its light streams, and no caret move exchanges them.
    /// The window is read off the corridor's own run (its left cell through
    /// the vertex one past its last), and wholeness is checked over every
    /// column of it: col 0 is the left feather, the rest flat through the
    /// right-edge vertex, identical before and after the scrub. Letting the
    /// right side own the boundary (`lb.max(la * RUN_TAIL_EASE)`) fails it.
    /// The host-seam twin is `scrub_gaps.rs`'s
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

    /// A scrub never raises a cell's light. Type `the quick` (cols 0..9);
    /// +150 ms a Right ×6 9 → 15 over blank ground (a hop: a cold half-life
    /// wake at cols 9..15, 0.55 s); later a Left ×5 15 → 10 back over the
    /// stub while the stub is deep inside its expiry melt and its cohort
    /// still in grace.
    ///
    /// A cell past its melt opening is already going out: the move is not a
    /// scrub, it takes the hop arm, lays nothing over the stub, and the stub
    /// finishes melting; no column is brighter on the landing frame than the
    /// frame before. The typed band is never caught by this guard: its melt
    /// opens at 0.70 × 1.70 = 1.19 s, past the 0.90 s retract the phase guard
    /// already refuses.
    ///
    /// Control: the same Left ×5 at +200 ms, the stub at 0.1 s of life
    /// (un-melted), is a scrub: it holds the stub's cohort and no column
    /// moves a level.
    #[test]
    fn a_scrub_over_a_melting_wake_stub_raises_no_cell_s_coverage() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let hop_at = |last: Instant| at(last, 150);
        // (a) The melting stub.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 0, 9, &c, 0.9);
        let last = at(t0, 8 * 60);
        let band = rib.cohorts()[0];
        let cx = ctx(hop_at(last), &c, (2, 15), 0.9);
        rib.on_event(&nav((2, 9), (2, 15)), hop_at(last), &cx);
        rib.plan(&cx);
        let stub = *rib
            .cohorts()
            .iter()
            .find(|k| k.wake)
            .expect("the hop lays its cold wake");
        // The corridor is born on the flight clock (cell `j` of `n` at
        // `T·j/n`, `T` = `FLIGHT_MIN_MS` 60 ms for a hop). At +500 ms every
        // cell of cols 10..15 is past its melt opening (0.70 × 0.55 s =
        // 0.385 s of life; the youngest, col 15, is 0.44 s old) and every one
        // is still lit (the oldest, col 10, is 0.49 s of 0.55 s).
        let back_at = at(hop_at(last), 500);
        let pre = back_at
            .checked_sub(Duration::from_millis(16))
            .expect("clock");
        let cx = ctx(pre, &c, (2, 15), 0.9);
        rib.plan(&cx);
        let cov_pre = census(&rib, 2, 0, 16);
        // The setup, read off the engine: every cell of the corridor the move
        // is about to cross is still lit and already past its melt opening
        // (staggered 10 ms apart across the hop's 60 ms flight).
        let melting: Vec<(u16, f32, f32)> = (10..16u16)
            .filter_map(|col| {
                rib.cells().iter().find(|l| l.col == col).map(|l| {
                    (
                        col,
                        rib.env_of(&cx, l),
                        pre.saturating_duration_since(live_since(rib.cohorts(), l))
                            .as_secs_f32()
                            - l.life_s * (1.0 - EXPIRY_MELT_SHARE),
                    )
                })
            })
            .collect();
        assert!(
            melting.len() == 6
                && melting
                    .iter()
                    .all(|&(_, env, past)| env > 0.0 && past > 0.0),
            "the setup: every cell of cols 10..16 is lit and past its melt opening              (col, env, seconds past the opening) {melting:?}, coverage {:?}",
            cov_pre.iter().map(|k| k.2).collect::<Vec<_>>()
        );
        let cx = ctx(back_at, &c, (2, 10), 0.9);
        rib.on_event(&nav((2, 15), (2, 10)), back_at, &cx);
        rib.plan(&cx);
        let cov_now = census(&rib, 2, 0, 16);
        for col in 0..16usize {
            assert!(
                cov_now[col].2 <= cov_pre[col].2,
                "col {col}: the move over a melting stub raised its light ({} → {})",
                cov_pre[col].2,
                cov_now[col].2
            );
        }
        let stub_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == stub.id)
            .expect("the stub's cohort is still resident");
        assert_eq!(
            stub_now.alive_at, stub.alive_at,
            "a melting stub's clock is not renewed"
        );
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == band.id)
                .map(|k| k.alive_at),
            Some(last),
            "the typed band beside the corridor is left on its own clock"
        );
        // …and the stub is out within the 0.11 s its youngest cell had left
        // (col 9 holds the band's own right boundary, sampled at 9·cw).
        let cx = ctx(at(back_at, 130), &c, (2, 10), 0.9);
        rib.plan(&cx);
        let cov_out = census(&rib, 2, 0, 16);
        assert!(
            (10..16).all(|col| cov_out[col].2 == 0),
            "the stub finished melting under the hand: {:?}",
            cov_out.iter().map(|k| k.2).collect::<Vec<_>>()
        );
        // (b) The control: over an un-melted stub the same move is a scrub.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 0, 9, &c, 0.9);
        let cx = ctx(hop_at(last), &c, (2, 15), 0.9);
        rib.on_event(&nav((2, 9), (2, 15)), hop_at(last), &cx);
        rib.plan(&cx);
        let stub = *rib.cohorts().iter().find(|k| k.wake).expect("the stub");
        let back_at = at(hop_at(last), 200);
        let cx = ctx(back_at, &c, (2, 15), 0.9);
        rib.plan(&cx);
        let cov_pre = census(&rib, 2, 0, 16);
        let cx = ctx(back_at, &c, (2, 10), 0.9);
        rib.on_event(&nav((2, 15), (2, 10)), back_at, &cx);
        rib.plan(&cx);
        let cov_now = census(&rib, 2, 0, 16);
        assert_eq!(
            cov_now.iter().map(|k| k.2).collect::<Vec<_>>(),
            cov_pre.iter().map(|k| k.2).collect::<Vec<_>>(),
            "(b): a scrub over a live stub moves no column a level"
        );
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == stub.id)
                .map(|k| k.alive_at),
            Some(back_at),
            "(b): …and holds the stub's cohort, which owns the corridor"
        );
        assert_eq!(rib.cells().len(), 9 + 7, "(b): nothing was laid");
    }

    #[test]
    fn a_wake_continues_the_band_s_own_walk_across_the_kink_whichever_way_it_jumps() {
        // `walk_t` is `d/16` for sixteen cells and `1/36` a cell after, so a
        // wake anchored at the landing would match the band's `t_at` only when
        // the landing is the band's origin. The wake shares the band's origin
        // instead (`wake_origin`).
        assert!(
            (walk_t(39.0) - walk_t(18.0) - walk_t(21.0)).abs() > 0.1,
            "the kink this pin is about"
        );
        let c = cfg(true, true);
        let t0 = Instant::now();
        let last = at(t0, 39 * 60);
        let now = at(last, 16);
        // (a) Into the band's middle from beyond its end, 60 → 20 (a jump
        // that leaves the run: cells 42..60 are dark; from 42 it would be a
        // scrub): cells 20..42 are covered on the stops they had; cells 2..20
        // stay the band's, on theirs.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let stops: Vec<f32> = (2..42u16)
            .map(|col| rib.field_at(2, col).expect("laid"))
            .collect();
        let cx = ctx(now, &c, (2, 20), 0.9);
        rib.on_event(&nav((2, 60), (2, 20)), now, &cx);
        rib.plan(&cx);
        for (col, want) in (2..42u16).zip(&stops) {
            let got = rib.field_at(2, col).expect("still lit at +0");
            assert!(
                (got - want).abs() < 1e-5,
                "col {col}: the wake repainted {want} → {got}"
            );
        }
        let wake = rib.cohorts().iter().max_by_key(|k| k.id).expect("the wake");
        assert_eq!(
            (wake.anchor_col, wake.t0, wake.d0),
            (
                rib.cohorts()[0].anchor_col,
                rib.cohorts()[0].t0,
                rib.cohorts()[0].d0
            ),
            "the wake shares the band's origin (a cold band: `d0` is 0 on both sides, so \
             the odometer is pinned by (c), on a band that continues a walk)"
        );
        // (b) Past its end, 42 → 60: the new cells continue the band's walk
        // at the lay rate, with no seam at 41/42 and none where a
        // landing-anchored walk would re-enter the fast sixteen.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let band = rib.cohorts()[0];
        let cx = ctx(now, &c, (2, 60), 0.9);
        rib.on_event(&nav((2, 42), (2, 60)), now, &cx);
        let cx = ctx(at(now, 150), &c, (2, 60), 0.9);
        rib.plan(&cx);
        for col in 42..=60u16 {
            let got = rib.field_at(2, col).expect("the corridor is laid");
            let want = band.t_at(col);
            assert!((got - want).abs() < 1e-5, "col {col}: {want} → {got}");
        }
        // (c) The same row's wake shares a continued band's odometer
        // (`wake_origin`'s same-row arm). Thirty keys from column 2, a
        // program's jump to column 40 and twenty keys there (the rebirth, on
        // the first run's walk at its own column, `d0 = 38`, §42), then an
        // arrow's hop past its end, 60 → 76. The hop's wake takes the
        // rebirth's whole origin, so column 60 is one `1/36` step past column
        // 59. With the distance restarted, column 60 would read `walk_t(20)`
        // instead, a 0.56 seam.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 30, &c, 0.9);
        let hop = at(t0, 29 * 60 + 100);
        let cx = ctx(hop, &c, (2, 40), 0.9);
        rib.on_event(&pty((2, 32), (2, 40)), hop, &cx);
        rib.plan(&cx);
        let k0 = at(hop, 100);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, k0, keys, &c);
        let reborn = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.anchor_col == 40)
            .expect("the rebirth");
        assert_eq!(
            reborn.d0, 38.0,
            "(c): the premise, a band that continues a walk — the first run's, at column 40 \
             (one row, one walk)"
        );
        let h = at(k0, 19 * 60 + 100);
        let cx = ctx(h, &c, (2, 76), 0.9);
        rib.on_event(&nav((2, 60), (2, 76)), h, &cx);
        rib.plan(&ctx(at(h, 150), &c, (2, 76), 0.9));
        let wake = *rib
            .cohorts()
            .iter()
            .filter(|k| k.row == 2 && k.wake)
            .max_by_key(|k| k.id)
            .expect("(c): the hop laid a wake");
        assert_eq!(
            (wake.anchor_col, wake.origin_walk()),
            (reborn.anchor_col, reborn.origin_walk()),
            "(c): the wake shares the continued band's origin, its distance included"
        );
        let (f59, f60) = (
            rib.field_at(2, 59).expect("(c): the band's last glyph"),
            rib.field_at(2, 60).expect("(c): the wake's first cell"),
        );
        assert!(
            (f60 - reborn.t_at(60)).abs() < 1e-5 && (f60 - f59 - WALK_LAY_RATE).abs() < 1e-5,
            "(c): column 60 is {f60} after {f59}, where the band's walk is {} one `1/36` step on",
            reborn.t_at(60)
        );
    }

    #[test]
    fn a_wrapped_paragraph_is_one_phrase_and_each_row_keeps_its_own_clock() {
        // The row law and the phrase, each doing its own work. Renewal is
        // row-scoped (`Ribbon::place`, model-checked by `aterm-spec`'s
        // `ribbon_row_hold_model`, `OnlyOwnerRenews`): row 2's cohort stops
        // being renewed the moment the hand is on row 3, keeps its clock, and
        // leaves through its own swoosh after its last key.
        //
        // The phrase (v3 §2.3, S6) sits on top: a wrap fold's new-row cohort
        // is minted while the phrase is open, so both rows carry one phrase id
        // and one grace, and a closed phrase's cohort on the hand's row is not
        // renewed. The phrase narrows the hold; it never widens it across
        // rows.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let row = |r: u16, col0: u16, n: u16| Keys {
            g: geom(),
            row: r,
            col0,
            n,
            period_ms: 100,
            disp: 0.9,
        };
        // Twenty keys on row 2 (the last at +1900 ms), then the hand is on
        // row 3 from +2000 ms — 100 ms later, inside the phrase's rest.
        type_keys(&mut rib, t0, row(2, 0, 20), &c);
        let left_row_2 = at(t0, 1900);
        // The fold: the hand's echoed move onto row 3 is the wrap, and it sets
        // row 2 flowing (`Cohort::flow`): the row keeps its own clock (below)
        // but slides into the fold from its last key and is gone by
        // `FLOW_TOTAL_S`.
        let fold = at(t0, 2000);
        let cx = ctx(fold, &c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 20), (3, 0)), fold, &cx);
        rib.plan(&cx);
        type_keys(&mut rib, at(t0, 2000), row(3, 0, 5), &c);
        let open = rib
            .phrase_open()
            .expect("the phrase is open under the hand");
        let row2 = rib
            .cohorts()
            .iter()
            .find(|c| c.row == 2)
            .expect("row 2 is still resident inside its own flow at +2.4 s");
        assert!(row2.flow.is_some(), "the fold set row 2 flowing");
        let row3 = rib
            .cohorts()
            .iter()
            .find(|c| c.row == 3)
            .expect("row 3 cohort");
        // The phrase: one breath, one id, one grace.
        assert_eq!(row2.phrase, open, "the fold's first row is the phrase's");
        assert_eq!(row3.phrase, open, "…and so is the row the fold opened");
        assert_eq!(
            (row2.grace_s, row3.grace_s),
            (rib.rest_s(), rib.rest_s()),
            "both rows keep the rest the hand is playing"
        );
        // The row: each keeps its own clock.
        assert_eq!(
            row2.alive_at, left_row_2,
            "a key on row 3 must not renew row 2's clock"
        );
        assert_eq!(
            row3.alive_at,
            at(t0, 2400),
            "…and the key holds its own row"
        );
        assert!(
            rib.cells().iter().any(|l| l.row == 2),
            "row 2 keeps the life it had: lit through its own grace"
        );
        // Fifteen more keys on row 3, +2500 … +3900 ms.
        type_keys(&mut rib, at(t0, 2500), row(3, 5, 15), &c);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "row 2 left one swoosh after ITS last key, under a hand still typing on row 3"
        );
        assert!(
            rib.cells().iter().any(|l| l.row == 3),
            "…while row 3 is still lit under the hand"
        );
        // …and row 3 leaves one swoosh after its own last key, measured
        // against the phrase's own total.
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        let cx = ctx(at(t0, 3900 + total - 40), &c, (3, 20), 0.0);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().any(|l| l.row == 3),
            "row 3 is on glass 40 ms before its swoosh ends"
        );
        let cx = ctx(at(t0, 3900 + total + 40), &c, (3, 20), 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "…and leaves {SWOOSH_TOTAL_S} s after its own last key"
        );
    }

    /// S6, the other half: a phrase holds only its own. A cohort of a closed
    /// phrase on the same row (typed, then a rest) is not renewed by a new
    /// phrase's keys elsewhere on the row and goes out on its own clock; a
    /// lay adjacent to it (the hand resuming the line) re-assigns it to the
    /// open phrase, and it is held again.
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

    /// An erase holds the open phrase: through a run of eight Backspaces at
    /// 120 ms the band's left end stays lit and its clock follows the
    /// erases; the swoosh begins one rest after the last erase. An erase
    /// after the rest has passed mints a phrase and takes the row back.
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
        // The grace runs from the last erase: still laying at +0.80 s after
        // it, reaching at +0.95 s.
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
        // …and an erase past the rest mints a phrase and takes the row back
        // (`hold_phrase`).
        let before = rib.phrase_open().expect("open under the run");
        let late = at(last_erase, 1200);
        erase_at(&mut rib, late, (2, 13), &c);
        let k = rib
            .cohorts()
            .iter()
            .find(|k| k.id == coh.id)
            .expect("resident");
        assert_eq!(
            k.alive_at, late,
            "an erase after the rest takes the row's band back"
        );
        assert_eq!(k.phase, Phase::Laying);
        let now = rib.phrase_open().expect("a fresh phrase is open");
        assert_ne!(now, before, "it is a NEW phrase, not the one that rested");
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == coh.id)
                .expect("resident")
                .phrase,
            now,
            "…and the row's cohort was adopted into it"
        );
    }

    /// The hand comes back: a Backspace run begun after the rest revives the
    /// band. The first erase mints a new phrase over the caret row's cohorts
    /// and every later erase of the run holds it, so the band outlives the
    /// last erase by a swoosh. The row scope is untouched
    /// (`ribbon_row_hold_model`, `OnlyOwnerRenews`): this is not a global
    /// hold.
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
        println!("revival: lit {dim} -> {back} cells, at full {full_env} -> {back_env}");
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
        println!("revival: tail after the last erase {tail_ms} ms");
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

    /// S5: the grace is the melody's rest, tempo-scaled (D-6, k = 2.5). At
    /// 10 cps the rest is the 0.90 s floor; at 2 cps (ioi 500 ms) it is
    /// 1.25 s, so a slow hand's line is still in its grace at 1.0 s idle and
    /// leaves 0.79 s after its own rest.
    #[test]
    fn a_slow_hand_s_grace_is_the_tempo_multiple_and_a_fast_hand_s_is_the_floor() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let fast = {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, 12, &c, 0.9);
            rib.cohorts()[0].grace_s
        };
        assert!(
            (fast - PHRASE_REST_MIN_S).abs() < 1e-6,
            "10 cps: the floor ({fast})"
        );
        let mut rib = Ribbon::new();
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 12,
            period_ms: 500,
            disp: 0.6,
        };
        type_keys(&mut rib, t0, keys, &c);
        let last = at(t0, 11 * 500);
        let slow = rib.cohorts()[0].grace_s;
        assert!(
            (1.20..=1.25).contains(&slow),
            "2 cps: the EMA has the tempo near 500 ms, the rest near 1.25 s ({slow})"
        );
        let cx = ctx(at(last, 1000), &c, (2, 14), 0.0);
        rib.plan(&cx);
        assert_eq!(
            rib.cohorts()[0].phase,
            Phase::Grace,
            "at 1.0 s idle a 2 cps line is still in its grace"
        );
        let total = (swoosh_total_s(slow) * 1000.0) as u64;
        let cx = ctx(at(last, total - 40), &c, (2, 14), 0.0);
        rib.plan(&cx);
        assert!(!rib.at_rest(), "on glass 40 ms before its own swoosh ends");
        let cx = ctx(at(last, total + 40), &c, (2, 14), 0.0);
        rib.plan(&cx);
        assert!(rib.at_rest(), "…and gone 40 ms after");
    }

    /// §2.6, the retract target and drain order per row. A band on a row
    /// below the caret (an Up-arrow recall left it) retracts toward its own
    /// left end and drains right-first; one on a row above (the hand went
    /// down a line) retracts toward its right end, the fold, and drains
    /// left-first. Neither slides toward a column of another row.
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

    /// A3: the after-glow of a fast line ends at the floor rest plus the
    /// swoosh, 1.69 s after the last key.
    #[test]
    fn the_after_glow_of_a_fast_line_ends_at_the_rest_plus_the_swoosh() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        assert_eq!(total, 1690, "0.90 s rest + 0.79 s swoosh");
        let mut sink = Sink::default();
        let cx = ctx(at(last, total - 30), &c, (2, 22), 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            !sink.under.is_empty(),
            "light on glass 30 ms before the swoosh ends"
        );
        let cx = ctx(at(last, total + 30), &c, (2, 22), 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            sink.under.is_empty() && rib.at_rest(),
            "…and none 30 ms after"
        );
    }

    /// The brightest planned boundary strictly inside cell `col`: its
    /// interior slabs, which interpolate the cell's own two edges and read
    /// nothing of the run's shape (a boundary's `cov` takes the brighter
    /// envelope; the wave and wedge move `spine`/`up`/`dn`, never `cov`). The
    /// quads' per-row alpha is not this number: the strip's vertical profile
    /// bends with the head's position. `0` once the cell has no interior slab.
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

    /// A released run leaves through the swoosh's retract, unstamped. Six
    /// typed cells, the last backspaced (retracting on its own 0.24 s
    /// clock); the witness names all six as blanked. The five live cells are
    /// counted and none is stamped: their cohort is abandoned into its
    /// retract (not rejoinable) and the retracting cell keeps its clock. The
    /// retract's span later the row is out of the pool, drawn into the hand
    /// over `RETRACT_DUR_S + RETRACT_FADE_S`, never cut on `RETIRE_MELT_S`.
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

    /// A partial release is restored by the stamp's own cells, not the
    /// cohort's count. Six typed cells; one released (a partial stamp of one
    /// cell); then a key lands past the caret inside the melt and joins the
    /// cohort, seven cells resident. The restore compares the cells still
    /// carrying the stamp against the cells the stamp took (one and one) and
    /// is accepted: the stamp is lifted, the cell lives, the cohort's clock
    /// is untouched. The candidate is `(cohort, Some(stamp))`; the cohort's
    /// own count is read by nobody.
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
        // The ember dims each stop into its own range, which is a fact about
        // the theme: for a chartreuse stop on the default ground the bar and
        // the colour floor are close, so a band of warm stops loses little
        // light before one would leave the glass. "Well under way" is said in
        // units of the band's own range, measured here: a second ribbon laid
        // by the same fixture and embered to its last frame is where this
        // band's light is heading.
        let mut twin = Ribbon::new();
        type_run(&mut twin, t0, 10, 6, &c, 0.8);
        let blur_cx = ctx(at(last, 100), &c, (2, 16), 0.8);
        twin.on_event(&Event::Focus(false), at(last, 100), &blur_cx);
        let end_cx = ctx(at(last, 399), &c, (2, 16), 0.8);
        twin.plan(&end_cx);
        let floor_sum = plan_sum(&twin);
        let range = full_sum.saturating_sub(floor_sum);
        let burned = full_sum.saturating_sub(dim_sum);
        println!(
            "ember/regain: {full_sum} → {dim_sum} → {floor_sum} of light \
             (range {range}, burned {burned} = {:.0}% at u=0.5, spend says {:.0}%)",
            100.0 * burned as f32 / range.max(1) as f32,
            100.0 * (1.0 - spend(0.5))
        );
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

    /// The longest dark run strictly inside `lit`'s own span — the rig's
    /// `ribscan` verdict, in cells.
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

    /// The band leaves as one object: through every frame of the focus ember
    /// the painted cells of a band are one interval, with no dark run inside,
    /// and the whole band goes out in the frame the ember ends.
    ///
    /// [`bed_ink`] puts every stop on one composited luminance (the
    /// legibility bar), not one distance from the ground, so a single shared
    /// fade would take the warm stops off the glass while their neighbours
    /// are still bright and open a hole in the band. The per-stop floor
    /// prevents that.
    ///
    /// Judged by [`census_paint`], not [`seen`], so the test does not grade
    /// the ember's exit level against its own definition.
    ///
    /// Two falsifiers: a bare `spend` over the whole band on the same frames
    /// splits it, and a fixed 24-byte lift instead of the floor fails this
    /// arm on the default ground.
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
            println!(
                "ember/{theme}: shipped worst dark run {worst_new}; \
                 bare spend {worst_bare} cells over {bare_split_frames} frames"
            );
        }
    }

    /// Every ground aterm ships, read out of the product's own theme table:
    /// `ColorScheme::default` plus every built-in, dark and light, each with
    /// the `dark_theme` flag the GUI would give it
    /// (`aterm_render::theme_is_dark`). Read, not listed, so a new theme is
    /// covered automatically.
    fn shipped_grounds() -> Vec<(String, u32, u32, bool)> {
        let rgb =
            |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        let d = aterm_types::scheme::ColorScheme::default();
        let mut out = vec![("Default".to_string(), rgb(d.foreground), rgb(d.background))];
        out.extend(
            aterm_types::scheme::builtin_names()
                .into_iter()
                .filter_map(|n| {
                    aterm_types::scheme::builtin(n)
                        .map(|s| (n.to_string(), rgb(s.foreground), rgb(s.background)))
                }),
        );
        // `builtin_names` carries "Default" too; one ground, one row.
        let mut seen_bg: Vec<u32> = Vec::new();
        out.into_iter()
            .filter(|(_, _, bg)| {
                let first = !seen_bg.contains(bg);
                seen_bg.push(*bg);
                first
            })
            .map(|(n, fg, bg)| (n, fg, bg, aterm_render::theme_is_dark(bg)))
            .collect()
    }

    /// The floor means one thing on every theme that ships: for every ground
    /// in the product's table, every stop the bed's bar leaves as paint at
    /// full coverage ([`census_paint`], not [`seen`]) is still paint at the
    /// coverage the ember leaves it at. The ember may dim a stop; it may not
    /// take it off the glass.
    ///
    /// The second arm reconstructs a fixed 24-byte lift (strict on Nord,
    /// loose on the default `#111318`) and requires it to fail, so the sweep
    /// has a red arm and a green one on the same fixture.
    #[test]
    fn the_ember_never_takes_a_stop_under_the_census_on_any_shipped_ground() {
        /// The 24-byte lift this module shipped for one day, reconstructed.
        fn abs_lift_floor(ink: u32, bg: u32) -> f32 {
            let ch = |c: u32, sh: u32| f32::from(((c >> sh) & 0xff) as u8);
            let old_seen = |px: u32| {
                let (r, g, b) = (ch(px, 16), ch(px, 8), ch(px, 0));
                let lift = (r - ch(bg, 16))
                    .abs()
                    .max((g - ch(bg, 8)).abs())
                    .max((b - ch(bg, 0)).abs());
                lift >= 24.0 && r.max(g).max(b) - r.min(g).min(b) >= 40.0
            };
            (1..=255u16)
                .rev()
                .map(|a| u8::try_from(a).unwrap_or(255))
                .find(|&a| !old_seen(over_premul(bg, premul_rgb(ink, a), a)))
                .map_or(1.0, |a| f32::from(a) + 1.0)
        }
        let mut old_broke = Vec::new();
        let mut grounds = 0u32;
        for (name, fg, bg, dark) in shipped_grounds() {
            let mut c = cfg(dark, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            let cap = cov_cap(&c);
            let light = !dark;
            let exit = |ink: u32, floor: f32| {
                // `EMBER_FLOOR_MARGIN` over the floor, clipped to the bed's
                // own ceiling: a floor over the ceiling is "hold at full".
                let a = (floor * EMBER_FLOOR_MARGIN).clamp(0.0, cap) as u8;
                over_premul(bg, premul_rgb(ink, a), a)
            };
            let level = |px: u32| {
                let c = |sh: u32| f32::from(((px >> sh) & 0xff) as u8);
                if light {
                    c(16).min(c(8)).min(c(0))
                } else {
                    c(16).max(c(8)).max(c(0))
                }
            };
            let (mut lit_at_bar, mut lost_new, mut lost_old) = (0u32, Vec::new(), Vec::new());
            let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
            for i in 0..BED_INK_LUT_LEN {
                let t = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
                let ink = lut.at(t);
                let cov = cap as u8;
                if !census_paint(over_premul(bg, premul_rgb(ink, cov), cov), light) {
                    // The bar already leaves this stop under the instrument at
                    // full coverage (Solarized Dark's foreground does it to
                    // half the arc). Not the ember's to fix, and the floor
                    // lands over the whole coverage there, so the stop holds
                    // until the pool clears.
                    continue;
                }
                lit_at_bar += 1;
                let px = exit(ink, lut.seen_floor_at(t));
                lo = lo.min(level(px));
                hi = hi.max(level(px));
                if !census_paint(px, light) {
                    lost_new.push((t, px));
                }
                if !census_paint(exit(ink, abs_lift_floor(ink, bg)), light) {
                    lost_old.push(t);
                }
            }
            grounds += 1;
            println!(
                "ground/{name} bg=#{bg:06X} {} — {lit_at_bar} of {BED_INK_LUT_LEN} stops lit at the bar, \
                 exit level {lo:.0}..{hi:.0}, lost by the floor {} (by the 24-byte lift {})",
                if dark { "dark" } else { "light" },
                lost_new.len(),
                lost_old.len()
            );
            assert!(
                lost_new.is_empty(),
                "{name}: the ember took {} stops the bar left lit under the paint census — \
                 {:?}",
                lost_new.len(),
                lost_new
                    .iter()
                    .map(|(t, px)| format!("t={t:.2} #{px:06X}"))
                    .take(6)
                    .collect::<Vec<_>>()
            );
            if !lost_old.is_empty() {
                old_broke.push((name, lost_old.len(), lit_at_bar));
            }
        }
        assert!(
            grounds >= 12,
            "the product's theme table went missing ({grounds} grounds)"
        );
        // The red arm: a fixed lift is not a statement about visibility and
        // loses most of the arc on the default theme, which no fixture
        // stated in `seen` could show.
        println!(
            "the 24-byte lift loses stops on {} of {grounds} shipped grounds: {old_broke:?}",
            old_broke.len()
        );
        let default_lost = old_broke
            .iter()
            .find(|(n, _, _)| n == "Default")
            .map_or(0, |(_, lost, _)| *lost);
        assert!(
            default_lost >= 100,
            "the 24-byte lift must be REFUTED on the default ground for this sweep to have a red \
             arm; it lost {default_lost} stops"
        );
    }

    /// The arithmetic behind the floor, printed. The bed's bar equalises the
    /// arc's luminance, not its distance from the ground, so the alpha at
    /// which a stop stops being [`seen`] differs across the arc by a factor
    /// of four or more: how much of the fade a shared multiplier gets wrong.
    #[test]
    fn the_warm_stops_go_out_at_several_times_the_coverage_the_cool_ones_do() {
        for (theme, fg, bg) in [
            ("default #111318", SHIPPED_FG, SHIPPED_BG),
            ("Nord", NORD_FG, NORD_BG),
        ] {
            let mut c = cfg(true, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            let mut rows = Vec::new();
            // Twelve stops of the arc (`walk_of_arc`): the law is about the
            // arc's colours, and an even sweep of the walk is not an even
            // sweep of the arc.
            for i in 0..12u32 {
                let t = walk_of_arc(i as f32 / 12.0);
                rows.push((t, lut.at(t), lut.seen_floor_at(t)));
            }
            let lo = rows.iter().map(|r| r.2).fold(f32::INFINITY, f32::min);
            let hi = rows.iter().map(|r| r.2).fold(0.0f32, f32::max);
            for (t, ink, floor) in &rows {
                println!(
                    "seen_floor/{theme} t={t:.2} ink=#{ink:06X} peak={} floor={floor:.0}/236",
                    ((ink >> 16) & 0xff).max((ink >> 8) & 0xff).max(ink & 0xff)
                );
            }
            assert!(
                hi >= 3.0 * lo,
                "{theme}: the arc's colour floors must spread — {lo:.0} to {hi:.0}"
            );
            assert!(
                hi < UNDER_COV_CAP,
                "{theme}: every stop must be visible at the bed's own ceiling ({hi:.0})"
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
        // "Going out with its body" is a claim about the clock, not the head
        // stop's range (which belongs to the theme). `spend` is `(1 − u)²`,
        // so at +250 ms of 300 the ember has spent 97% of the head's range:
        // the envelope must be within a few parts in a hundred of its final
        // value, and the hairline falling with it.
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

    /// The focus seam's wake clause (v3 step 1). The host forwards raw focus
    /// (`CursorGlow::note_focus`), and an unfocused window being typed into
    /// (a control-socket agent's window) keeps drawing its light. So a typed
    /// key laid while the ember burns clears the ember and re-lights from the
    /// level the light had, through `edge-in`, and the pool is not swept at
    /// the ember's end.
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
        // Halfway through, the body is below its level at the blur. It need
        // not be below the cold-body ceiling: the peak belongs to whichever
        // stop in the run has the least range between the bed's bar and the
        // colour floor, and on a dark ground a warm stop's range is a fifth of
        // its light.
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

    /// The curtain (v3 §2.8, A2). At a coordinate-space seam the band is
    /// drawn into the hand it last stood under over `CURTAIN_S` and spent to
    /// exactly zero, farthest-first, so the count falls in steps and never
    /// off a cliff; nothing is laid until it is over (a key and a jump during
    /// the curtain leave the pool as it is, a key after it lays afresh). The
    /// host asks for frames throughout (the suck-in is motion).
    ///
    /// Stated in the quantities the instruments read:
    /// [`Ribbon::lit_segments`] (`trail status`'s `ribbon_segments=`) and
    /// [`Ribbon::ink_segments`] (the glass census). A geometric `cell_lit`
    /// scores alpha 1 like alpha 236 and cannot see a brightness cliff. The
    /// sweep runs past `CURTAIN_S` to +330 ms and measures every frame's
    /// step, bounded by [`CURTAIN_MAX_STEP_FRAC`] and [`CURTAIN_MIN_FRAMES`],
    /// as the staggered natural retract it imitates is.
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
        // The caret has already moved when the host notices the space ended:
        // on the alt-screen path the launching Return put the caret at column
        // 0 of the next row while the band still lies at columns 10..29 of
        // row 2. The curtain must gather into the band's head, not slide 30
        // columns to the caret.
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
        // Eleven frames at 30 fps, past the end of the fall: the band's total
        // light falls every frame, the left end is drawn toward the band's
        // own head, and neither segment count steps more than
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
            println!(
                "curtain +{ms} ms: peak {} sum {} left {:.1} cells {} segs {} ink {}",
                now.peak, now.sum, now.left, now.cells, now.segs, now.ink
            );
            if now.ink == 0 && dark_at.is_none() {
                dark_at = Some(ms);
            }
            prev = now;
        }
        println!("curtain worst step: {worst_step} claimed, {worst_ink_step} inked");
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
        // The gather is into the band's own head: the band lies at columns
        // 10..29 of row 2, its head's right edge is column 30's left edge,
        // and every boundary converges there, never on the seam's caret.
        //
        // Both caret shapes are swept because they fail differently: (3, 0),
        // the next row, is the alt-screen path; (2, 0), column 0 of the
        // band's own row (a carriage-return redraw or a resize), defeats a
        // per-row read of the caret.
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

    /// The curtain is drawn under letters that stay legible (v3 §2.8, B1).
    /// The band leaves under the incoming app's first frames, and the bed is
    /// source-over at the bar. This sweeps the real emitted frames: every
    /// device row a letter of the typed row (and the reach into the row
    /// above) can touch, at every column the band crosses, every 8 ms of the
    /// fall; the text composites at `BODY_CONTRAST_BAR` throughout.
    ///
    /// The frames are required to be a curtain's: the light spends away, the
    /// ink retreats toward the hand, and a key struck under it lays nothing.
    /// Each clause fails on its own if its mechanism is removed.
    ///
    /// The rail is outside the sweep, as in
    /// `the_rail_is_yellow_below_the_row_bottom_and_the_glyph_box_stays_at_the_bar`:
    /// its top is pinned at the row bottom and no glyph of the row reaches it.
    #[test]
    fn the_curtain_is_legible_under_the_app_s_first_frames_on_every_frame_it_falls() {
        let c = cfg(true, true);
        let g = geom();
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let caret = (2u16, 30u16);
        let fell = at(last, 16);
        // The cells the curtain falls on, so the lay-gate below has something
        // to refuse.
        let laid = rib.cells().len();
        rib.curtain(fell);
        assert!(rib.curtained(), "precondition: a curtain is falling");
        // The lay-gate, checked on the frames the sweep walks: a key struck
        // under the curtain lays nothing, so the letters composite against
        // the leaving band. The key lands clear of the band's columns, so a
        // lay that got through would be a new cell and not a re-wet.
        let cx = ctx(at(fell, 8), &c, (2, 41), 0.9);
        rib.on_event(&typed(), at(fell, 8), &cx);
        assert_eq!(
            rib.cells().len(),
            laid,
            "the sweep is not under a lay-gate: a key laid a cell during the curtain"
        );
        let row_top = f32::from(g.origin_y) + 2.0 * ch;
        let row_bottom = row_top + ch;
        let mut sink = Sink::default();
        let mut worst = f32::INFINITY;
        let mut worst_at = (0.0_f32, 0.0_f32, 0_u64);
        let mut lit_frames = 0;
        let mut peak_seen = 0_u8;
        let mut first: Option<(u32, f32)> = None;
        let mut last_lit: Option<(u64, u32, f32)> = None;
        for ms in (0..=248).step_by(8) {
            let cx = ctx(at(fell, ms), &c, caret, 0.9);
            rib.plan(&cx);
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            if peak > 0 {
                lit_frames += 1;
                peak_seen = peak_seen.max(peak);
            }
            // The three moving parts, sampled on the same frames as the
            // letters: the spend's envelope, the suck-in's leftmost boundary,
            // and the total light.
            let sum: u32 = sink.under.iter().map(|q| u32::from(q.alpha)).sum();
            let left = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            if ms == 0 {
                first = Some((sum, left));
            }
            if sum > 0 {
                last_lit = Some((ms, sum, left));
            }
            // The whole span the run could occupy plus a cell either side: the
            // suck-in moves the boundaries, so a fixed window would stop
            // sampling the band.
            let mut x = f32::from(g.origin_x) + 9.0 * cw;
            while x < f32::from(g.origin_x) + 32.0 * cw {
                let mut y = row_top - 0.2 * ch;
                while y < row_bottom {
                    let k = contrast(DEFAULT_FG, composite_at(&sink.under, x, y));
                    if k < worst {
                        worst = k;
                        worst_at = (x, y - row_bottom, ms);
                    }
                    y += 1.0;
                }
                x += 1.0;
            }
        }
        assert!(
            lit_frames >= 3 && peak_seen > 0,
            "precondition: the curtain actually drew ({lit_frames} lit frames, peak {peak_seen})"
        );
        // Sensitive to the curtain, not just to a lit ribbon: the frames swept
        // must be a curtain's (its light spends away, its boundaries are drawn
        // into the hand, nothing is laid under it). Each clause fails on its
        // own if its mechanism goes.
        let (sum0, left0) = first.expect("the sweep starts at +0 ms");
        let (end_ms, end_sum, end_left) = last_lit.expect("the curtain drew");
        assert!(
            end_sum * 8 < sum0,
            "the sweep was not a falling curtain: the band's light went {sum0} → {end_sum} \
             by +{end_ms} ms"
        );
        assert!(
            end_left > left0 + 3.0 * cw,
            "the sweep was not a gathering curtain: the band's ink retreated from {left0:.1} \
             only to {end_left:.1} px, under three cells — it is supposed to end up under the \
             hand at {:.1}",
            f32::from(g.origin_x) + 30.0 * cw
        );

        // Not vacuous: a sweep that missed the band would report the bare
        // ground's own contrast.
        let ground = contrast(DEFAULT_FG, DEFAULT_BG);
        println!(
            "curtain legibility: {lit_frames} lit frames, peak cov {peak_seen}, worst {worst:.3}:1 vs the bare ground's {ground:.3}:1"
        );
        assert!(
            worst < ground - 0.5,
            "the sweep never crossed the band: worst {worst:.3}:1 is the bare ground's {ground:.3}:1"
        );
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "a letter under the falling curtain composites at {worst:.3}:1 (x {}, {:.0} px above the row bottom, +{} ms into the fall); the bar is {BODY_CONTRAST_BAR}:1",
            worst_at.0,
            -worst_at.1,
            worst_at.2
        );
    }

    #[test]
    fn an_idle_ribbon_draws_nothing_and_asks_for_no_frame() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 4, &c, 0.5);
        let now = at(t0, 30_000);
        let cx = ctx(now, &c, (2, 14), 0.0);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(sink.under.is_empty() && sink.out.is_empty() && sink.halos.is_empty());
        assert!(rib.at_rest());
        assert!(rib.next_change_deadline(now).is_none());
        assert!(rib.field().is_empty());
    }

    #[test]
    fn a_busy_band_keeps_its_boundary_and_pixel_signature_across_its_exit() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 30, &c, 0.9);
        let last = at(t0, 29 * 60);
        let mut signatures = Vec::new();
        let mut sink = Sink::default();
        for (offset, erase, curtain) in [
            (8u64, false, false),
            (100, false, false),
            (120, true, false),
            (180, false, true),
        ] {
            let now = at(last, offset);
            let caret = if erase || curtain { (2, 39) } else { (2, 40) };
            if erase {
                erase_at(&mut rib, now, caret, &c);
            }
            if curtain {
                rib.curtain(at(last, 140));
            }
            let cx = ctx(now, &c, caret, 0.9);
            rib.plan(&cx);
            if curtain {
                assert!(rib.brisk(now), "at +{offset} ms");
                assert_eq!(
                    rib.next_change_deadline(now),
                    Some(now + super::super::FRAME_CADENCE),
                    "a moving band keeps its full frame cadence"
                );
            } else if offset == 100 {
                assert!(!rib.brisk(now));
                assert!(
                    rib.next_change_deadline(now)
                        .is_some_and(|due| due >= now + super::super::TAIL_FLOOR),
                    "the settled band uses the paced tail"
                );
            }
            let mut hash = 0xcbf2_9ce4_8422_2325u64;
            let mut feed = |part: u64| {
                hash = (hash ^ part).wrapping_mul(0x100_0000_01b3);
            };
            for seg in rib.plan_segments() {
                for part in [seg.x, seg.spine, seg.up, seg.dn, seg.t] {
                    feed(u64::from(part.to_bits()));
                }
                feed(u64::from(seg.cov));
            }
            let mut frame = sink.frame();
            rib.emit(&cx, &mut frame);
            for q in &sink.under {
                for part in [
                    u64::from(q.row),
                    u64::from(q.x),
                    u64::from(q.y),
                    u64::from(q.w),
                    u64::from(q.h),
                    u64::from(q.color),
                    u64::from(q.alpha),
                    u64::from(q.color2),
                    u64::from(q.alpha2),
                ] {
                    feed(part);
                }
            }
            signatures.push((rib.plan_segments().len(), sink.under.len(), hash));
        }
        // Golden values for every boundary bit and every emitted gradient
        // quad: one sample per cell must leave the paint byte-identical
        // through typing, rest, Backspace, and the curtain.
        assert_eq!(
            signatures,
            [
                (94, 2405, 4594527394971076470),
                (94, 2406, 14096293239580483835),
                (91, 2342, 317606280172771422),
                (91, 2336, 2582682657226917771),
            ]
        );
    }

    // -- §4, D14: one emitter, two profiles --------------------------------

    #[test]
    fn tall_and_underline_are_one_emitter_with_two_shoulders() {
        let tall = cfg(true, true);
        let under = cfg(true, false);
        let (a, b) = (Ribbon::body_profile(&tall), Ribbon::body_profile(&under));
        assert!((a.shoulder - SHOULDER_TALL).abs() < 1e-6);
        assert!((b.shoulder - SHOULDER_UNDERLINE).abs() < 1e-6);
        assert!(a.up_ch > b.up_ch, "the tall body reaches into row − 1");
        assert!(
            (a.dn_ch - b.dn_ch).abs() < 1e-6,
            "the two spellings differ ABOVE the spine and nowhere else"
        );

        // …and the light law is one law: the same cell, age and spine give
        // the same peak coverage under both profiles. Only the band's extent
        // moves.
        let t0 = Instant::now();
        let mut peaks = Vec::new();
        let mut rows = Vec::new();
        for c in [&tall, &under] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 6, c, 0.7);
            let now = at(t0, 5 * 60);
            let cx = ctx(now, c, (2, 16), 0.7);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            peaks.push(sink.under.iter().map(|q| q.alpha).max().unwrap_or(0));
            rows.push(
                sink.under
                    .iter()
                    .map(|q| f32::from(q.y))
                    .reduce(f32::min)
                    .unwrap_or(0.0),
            );
        }
        assert_eq!(
            peaks[0], peaks[1],
            "the tall and underline spellings must price light identically"
        );
        assert!(
            rows[0] < rows[1],
            "the tall body's top edge must sit above the underline's"
        );
    }

    // -- §4.2 / D16: the band is published so nothing is born inside it -----

    #[test]
    fn the_ribbon_publishes_the_band_stardust_must_stay_out_of() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let now = at(t0, 5 * 60);
        let cx = ctx(now, &c, (2, 16), 0.8);
        rib.plan(&cx);
        let g = geom();
        let band = rib.band(2, 13).expect("a laid cell publishes a band");
        let cell_top = f32::from(g.origin_y) + 2.0 * g.ch as f32;
        assert!(
            band.top < cell_top,
            "the tall band reaches into row − 1, which is D16's whole point"
        );
        assert!(band.contains(band.spine));
        assert!(!band.contains(band.top - 0.05 * g.ch as f32));
        // The sky band §5.4 hands stardust is entirely above the body.
        let sky_lo = band.top - 0.30 * g.ch as f32;
        let sky_hi = band.top - 0.04 * g.ch as f32;
        assert!(sky_lo < sky_hi && !band.contains(sky_hi));
        assert!(
            rib.band(2, 60).is_none(),
            "an unlaid cell publishes nothing"
        );
    }

    // -- §3.3: the light fork ---------------------------------------------

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

    #[test]
    fn a_dark_body_rides_its_ceiling_and_the_strip_shows_above_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 12, &c, 1.0);
        let now = at(t0, 11 * 60);
        let cx = ctx(now, &c, (2, 22), 1.0);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
        assert!(
            peak > (UNDER_COV_CAP * 0.9) as u8,
            "a hot ribbon must actually reach its ceiling (peak {peak}); a body that cannot is the 'dim and muddy' defect"
        );
        // The strip's oracle: on a hot mark the body sits at its own cap, so
        // only the baseline strip's accent can take the spine past it.
        assert!(
            peak > UNDER_COV_CAP as u8,
            "the baseline strip must show above the body's ceiling (peak {peak}, cap {UNDER_COV_CAP})"
        );
        assert!(
            f32::from(peak) <= BODY_FRAME_TOP,
            "and the strip's lift may never take the spine past the ledger's frame top"
        );
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

    /// The device rows of `under` at column `x`, as `(y, composited)`.
    fn column_at(under: &[GlowQuad], x: f32) -> Vec<(f32, u32)> {
        let mut ys: Vec<f32> = under
            .iter()
            .filter(|q| {
                let qx = f32::from(q.x);
                (qx..qx + f32::from(q.w)).contains(&x.floor())
            })
            .map(|q| f32::from(q.y))
            .collect();
        ys.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        ys.dedup();
        ys.into_iter()
            .map(|y| (y, composite_at(under, x, y)))
            .collect()
    }

    /// The vivid rail (§30): below the row bottom the rainbow is the full
    /// ROYGBIV (yellow composites as yellow at a max channel over 200), and
    /// above it every device row a letter can touch is still the bed, at the
    /// bar. The bed's own pin
    /// (`the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar`)
    /// still holds beside this one: the two zones are two inks.
    #[test]
    fn the_rail_is_yellow_below_the_row_bottom_and_the_glyph_box_stays_at_the_bar() {
        use crate::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN};
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
        // Yellow's anchor on the walk: the arc and the walk are perceptually
        // paced ([`WALK_PACE`]), so yellow is at `walk_of_arc` of
        // `SPECTRUM_ANCHOR_AT[2]`, not at `2/6`.
        let t_yellow = walk_of_arc(SPECTRUM_ANCHOR_AT[2] as f32 / (SPECTRUM_LUT_LEN - 1) as f32);
        let seg = rib
            .plan_segments()
            .iter()
            .min_by(|a, b| {
                (a.t - t_yellow)
                    .abs()
                    .partial_cmp(&(b.t - t_yellow).abs())
                    .expect("finite")
            })
            .expect("a planned run");
        assert!(
            (seg.t - t_yellow).abs() < 0.04,
            "the run must cross yellow's anchor (nearest slab t = {:.3}, yellow {t_yellow:.3})",
            seg.t
        );
        let x = seg.x + 0.5;
        // 0.11–0.17 ch below the row bottom: inside the leading, in the rail's
        // plateau, under no letter of the typed row.
        let y_rail = row_bottom + (0.12 * ch).round();
        let lit = composite_at(&sink.under, x, y_rail);
        println!(
            "the column at yellow (x {x:.1}): {:?}",
            column_at(&sink.under, x)
                .iter()
                .map(|(y, c)| format!("y{}=#{c:06X}", *y as i32 - row_bottom as i32))
                .collect::<Vec<_>>()
        );
        let (r, gr, b) = ((lit >> 16) & 0xff, (lit >> 8) & 0xff, lit & 0xff);
        // About 2.4× the bed's olive: the rail's ceiling (`RAIL_LUMA_CEIL`
        // 0.54, `(200, 200, 0)` ink, `≈ (190, 190, 3)` at the cap) gives a
        // yellow that reads as yellow below the baseline. It is brighter than
        // the caret's floor on purpose: the rail is never under a letter, and
        // L5 is the caret's law in the glyph rows. r within a sixteenth of g:
        // the sampled slab lerps a step past the anchor toward green.
        assert!(
            max_channel(lit) >= 185 && r.abs_diff(gr) <= 16 && b < 40,
            "0.15 ch below the row bottom at yellow's anchor the rail composites #{lit:06X}; the owner asked to SEE yellow (peak ≥ 185, r ≈ g, little blue)"
        );
        // …and the rail is the full arc: every stop the run lays composites
        // at a max channel over 130 there, warm and cold alike. (A vertex
        // owns the slab to its right, so the run's last vertex is not
        // sampled.)
        for w in rib.plan_segments().windows(2).step_by(3) {
            let seg = &w[0];
            let lit = composite_at(&sink.under, seg.x + 0.5, y_rail);
            if seg.cov < STATUS_LIT_COV {
                continue;
            }
            assert!(
                max_channel(lit) >= 130,
                "the rail at t = {:.3} composites #{lit:06X} (max {}); every stop is vivid on the rail",
                seg.t,
                max_channel(lit)
            );
        }
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

    /// The rail's ink, stop by stop: every anchor inside
    /// `[RAIL_LUMA_FLOOR, RAIL_LUMA_CEIL]` (the cold half lifted to the floor
    /// and no further, the warm half scaled down to the ceiling with its hue
    /// exact, red carried pure), each at least 130 on its brightest channel
    /// composited over the bed. Printed so a re-tune has a table to move.
    #[test]
    fn the_rail_ink_table() {
        use crate::spectrum::SPECTRUM_ANCHORS;
        let names = [
            "red", "orange", "yellow", "green", "blue", "indigo", "violet",
        ];
        let cov = UNDER_COV_CAP as u8;
        println!();
        println!(
            "band [{RAIL_LUMA_FLOOR:.3}, {RAIL_LUMA_CEIL:.3}] — the rail's own ceiling below the baseline (the caret's floor {} × the field's share {} rules the glyph rows)",
            crate::cursor_glow::RAINBOW_CARET_LIGHT_FLOOR,
            crate::cursor_glow::RAINBOW_SPARKLE_LIGHT_SHARE
        );
        println!("stop     arc      rail     Y rail  composited@236 (max, light)   bed@236");
        for (name, &arc) in names.iter().zip(SPECTRUM_ANCHORS.iter()) {
            let ink = rail_ink(arc);
            let y = relative_luminance(ink);
            let lit = over_premul(DEFAULT_BG, premul_rgb(ink, cov), cov);
            let bed = over_premul(
                DEFAULT_BG,
                premul_rgb(bed_ink(arc, bed_luma_budget(DEFAULT_FG)), cov),
                cov,
            );
            println!(
                "{name:7}  #{arc:06X}  #{ink:06X}  {y:.3}   #{lit:06X} ({:3}, {:3.0})          #{bed:06X}",
                max_channel(lit),
                relative_luminance(lit) * 255.0
            );
            assert!(
                (RAIL_LUMA_FLOOR - 0.003..=RAIL_LUMA_CEIL).contains(&y),
                "{name}: the rail's ink #{ink:06X} (Y {y:.3}) is outside the band"
            );
            let y_full = relative_luminance(arc);
            if (RAIL_LUMA_FLOOR..=RAIL_LUMA_CEIL).contains(&y_full) {
                assert_eq!(ink, arc, "{name}: a stop inside the band is carried PURE");
            } else if y_full > RAIL_LUMA_CEIL {
                // Scaled through its own hue: every channel in the same
                // ratio to the anchor's, to a level of rounding.
                let k = max_channel(ink) as f32 / max_channel(arc) as f32;
                for sh in [16, 8, 0] {
                    let (a, i) = (((arc >> sh) & 0xff) as f32, ((ink >> sh) & 0xff) as f32);
                    assert!(
                        (a * k - i).abs() <= 1.5,
                        "{name}: the rail changed the hue (#{arc:06X} → #{ink:06X})"
                    );
                }
                assert!(
                    y > RAIL_LUMA_CEIL - 0.01,
                    "{name}: scaled to the ceiling, not under it"
                );
            } else {
                assert!(
                    y < RAIL_LUMA_FLOOR + 0.01,
                    "{name}: a cold stop is lifted to the floor and no further"
                );
                assert!(
                    sat(ink) >= 0.5,
                    "{name}: still a colour on the rail (S {:.2})",
                    sat(ink)
                );
            }
            assert!(
                max_channel(lit) >= 130,
                "{name}: vivid on the rail (max {})",
                max_channel(lit)
            );
            assert!(
                relative_luminance(lit) * 255.0 <= RAIL_LUMA_CEIL * 255.0 + 0.5,
                "{name}: the rail composites over its own ceiling"
            );
        }
    }

    /// The comet (§30): fattest at the hand, thinner behind it. The reach
    /// below the spine is `COMET_DN_HAND_CH` under the caret and the floor at
    /// the tail; the reach above is `TALL_UP_CH` at the head and
    /// `COMET_UP_TAIL_CH` past the taper; both fall monotonically. The flat
    /// spelling is the same shape at every cell.
    #[test]
    fn the_comet_is_fattest_at_the_hand_and_thins_behind_it() {
        let g = geom();
        let ch = g.ch as f32;
        let dn = |b: Band| b.bottom - b.spine;
        let up = |b: Band| b.spine - b.top;
        // Reduced motion stills the wave, so the bands read the pure profile.
        let mut comet = cfg(true, true);
        comet.reduced_motion = true;
        let mut flat = flat(true, true);
        flat.reduced_motion = true;
        for c in [&comet, &flat] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 24, c, 1.0);
            let cx = ctx(at(t0, 23 * 60 + 8), c, (2, 34), 1.0);
            rib.plan(&cx);
            let head = rib.band(2, 33).expect("the head cell");
            let mid = rib.band(2, 27).expect("six cells behind");
            let tail = rib.band(2, 12).expect("twenty-one cells behind");
            println!(
                "flat {}: dn head/mid/tail = {:.2}/{:.2}/{:.2} ch, up = {:.2}/{:.2}/{:.2} ch",
                c.ribbon_flat,
                dn(head) / ch,
                dn(mid) / ch,
                dn(tail) / ch,
                up(head) / ch,
                up(mid) / ch,
                up(tail) / ch
            );
            if c.ribbon_flat {
                assert!(
                    (dn(head) - DN_TOP_CH * ch).abs() <= 1.0,
                    "the flat wedge at the hand"
                );
                assert!((up(head) - TALL_UP_CH * ch).abs() <= 0.5);
                assert!(
                    (up(tail) - TALL_UP_CH * ch).abs() <= 0.5,
                    "the flat body is one shape"
                );
                continue;
            }
            assert!(
                (dn(head) - COMET_DN_HAND_CH * ch).abs() <= 1.0,
                "the comet's lobe under the hand is {:.2} ch, not {COMET_DN_HAND_CH}",
                dn(head) / ch
            );
            assert!(
                (dn(tail) - DN_FLOOR_CH * ch).abs() <= 1.0,
                "the lobe settles to the floor behind the hand ({:.2} ch)",
                dn(tail) / ch
            );
            assert!(
                dn(head) > dn(mid) && dn(mid) > dn(tail),
                "the lobe closes monotonically"
            );
            assert!(
                (up(head) - TALL_UP_CH * ch).abs() <= 0.5,
                "the head cell keeps the full body ({:.2} ch)",
                up(head) / ch
            );
            assert!(
                (up(tail) - COMET_UP_TAIL_CH * ch).abs() <= 1.0,
                "the tail thins to {COMET_UP_TAIL_CH} ch ({:.2})",
                up(tail) / ch
            );
            assert!(
                up(head) > up(mid) && up(mid) > up(tail),
                "the body thins monotonically"
            );
        }
    }

    /// The from-the-hand attack (§30): new light enters from the hand. The new
    /// light is the attach under the caret cell (the cell the key lays where
    /// the last attach stood inherits its level), and it pours in from the
    /// head's edge outward: on the echo frame the whole attach is at the
    /// birth floor (T2), from the second frame the slab nearest the head
    /// leads the far slab, and by `ATTACK_WIPE_S` it is uniform. The flat
    /// spelling fades in place: every slab equal on every frame.
    #[test]
    fn a_new_cell_s_light_enters_from_the_caret_side() {
        let g = geom();
        let cw = g.cw as f32;
        // The attach: the caret cell, 18, from the head's right edge on.
        let x0 = f32::from(g.origin_x) + 18.0 * cw;
        let x1 = x0 + cw;
        for c in [cfg(true, true), flat(true, true)] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 8, &c, 0.9);
            let born = at(t0, 7 * 60);
            let mut frames = Vec::new();
            println!(
                "flat {}: the attach's slabs (cov), the head's edge first; the caret cell's right edge last",
                c.ribbon_flat
            );
            for ms in [0u64, 8, 16, 24, 32, 48] {
                let cx = ctx(at(born, ms), &c, (2, 18), 0.9);
                rib.plan(&cx);
                let slabs: Vec<u8> = rib
                    .plan_segments()
                    .iter()
                    .filter(|s| s.x >= x0 && s.x <= x1)
                    .map(|s| s.cov)
                    .collect();
                println!("  +{ms:2} ms: {slabs:?}");
                assert!(slabs.len() >= 3, "three slabs per cell at this fixture");
                frames.push(slabs);
            }
            // The first vertex is the head's own right edge (the brighter
            // side owns it); the attach's own slabs are the rest.
            let own = |f: &Vec<u8>| f[1..].to_vec();
            let floor = (BIRTH_EDGE_FLOOR * UNDER_COV_CAP * 0.8) as u8;
            for &s in &own(&frames[0]) {
                assert!(
                    s >= floor,
                    "the echo frame is on glass at the floor (T2): {s} < {floor}"
                );
                assert!(
                    s <= (BIRTH_EDGE_FLOOR * UNDER_COV_CAP) as u8 + 2,
                    "the echo frame is AT the floor, not above it: {s}"
                );
            }
            let f16 = own(&frames[2]);
            let f24 = own(&frames[3]);
            let last = own(&frames[5]);
            if c.ribbon_flat {
                for f in [&f16, &f24, &last] {
                    let (lo, hi) = (f.iter().min().unwrap(), f.iter().max().unwrap());
                    assert!(hi - lo <= 2, "the flat body fades in IN PLACE: {f:?}");
                }
                continue;
            }
            // +16 ms and +24 ms: the head's side (the first own slab, a
            // third of the cell from the head's edge) leads the far side
            // (the last slab) by a visible margin.
            for (ms, f) in [(16, &f16), (24, &f24)] {
                let (near, far) = (f[0], f[f.len() - 1]);
                assert!(
                    near >= far + 30,
                    "+{ms} ms: the slab at the head's edge ({near}) must lead the far slab ({far}); the light enters from the hand"
                );
            }
            // …and by the wipe's end the cell is uniform.
            let (lo, hi) = (last.iter().min().unwrap(), last.iter().max().unwrap());
            assert!(
                hi - lo <= 2,
                "past ATTACK_WIPE_S the cell is uniform: {last:?}"
            );
        }
    }

    /// The gate (§30): the flat spelling draws no rail: nothing below the flat
    /// body's own reach, nothing vivid anywhere in `under`. (The byte-for-byte
    /// pin is `cursor_glow`'s
    /// `the_flat_spelling_collapses_every_comet_branch_byte_for_byte`.)
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

    /// A stacked split's focused pane has its own floor. The comet's lower
    /// lobe and the vivid rail reach into the leading below the row; they ask
    /// the pane's last row ([`Ribbon::pane_rows`]), not [`Geom::fx_bot`], so
    /// on a pane above another they do not pour over the divider.
    ///
    /// The control is the same band with no pane set, which must still draw
    /// its rail.
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

    // -- seam point 5 ------------------------------------------------------

    #[test]
    fn head_rgb_is_the_authored_stop_on_dark_and_the_rail_ink_on_light() {
        let dark = cfg(true, true);
        let light = cfg(false, false);
        let t0 = Instant::now();
        for c in [&dark, &light] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 6, c, 0.8);
            let cx = ctx(at(t0, 5 * 60), c, (2, 16), 0.8);
            rib.plan(&cx);
            let t = rib.field_at_caret();
            assert!(
                (t - walk_t(5.0)).abs() < 1e-5,
                "the caret's own cell is the field"
            );
            let arc = spectrum(walk_arc(t));
            let got = rib.head_rgb(c).expect("a cursor has been observed");
            if c.dark_theme {
                assert_eq!(got, arc, "dark themes hand the companion the AUTHORED stop");
                assert_ne!(
                    got,
                    bed_ink(arc, bed_luma_budget(c.theme_fg)),
                    "…and this stop is one the bed dims, so the pin is not vacuous"
                );
            } else {
                assert_eq!(
                    got,
                    light_role(c).ink(arc),
                    "light themes hand it the rail's ink"
                );
            }
        }
        assert!(
            Ribbon::new().head_rgb(&dark).is_none(),
            "no cursor observed, no colour"
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
        // 5 ms a key: all three rows must still be live when the third is done
        // for the budget to be under test. A row the hand left keeps only its
        // own clock, so at 20 ms a key the first row would already be in its
        // swoosh (a drain, not a shed). At 5 ms it is 0.76 s idle when the
        // third finishes: still reaching, every cell at full envelope.
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
                    // Three do not fit: the newer of the two older rows is
                    // whole, and what is shed comes off the oldest row's tail,
                    // contiguous to its head, never out of its middle.
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 3, col),
                            "row 3 col {col} was shed before row 2"
                        );
                    }
                    let lit: Vec<bool> = (2..78u16)
                        .map(|col| cell_lit(&sink.under, g, 2, col))
                        .collect();
                    // Three rows exceed the budget at full density. The
                    // budget answers first with the density dial (§18
                    // `slabs_for`, which prices the rail's rows too, so three
                    // retina rows fit at one slab per cell) and only then by
                    // shedding the oldest tail. A shed is never out of the
                    // middle.
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

    #[test]
    fn a_frame_is_a_pure_function_of_its_events_and_now() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let drive = |rib: &mut Ribbon| {
            type_run(rib, t0, 10, 12, &c, 0.85);
            let now = at(t0, 12 * 60);
            let cx = ctx(now, &c, (2, 21), 0.85);
            rib.on_event(&Event::Erase, now, &cx);
            rib.plan(&cx);
            let now = at(t0, 12 * 60 + 200);
            let cx = ctx(now, &c, (2, 50), 0.85);
            rib.on_event(&nav((2, 21), (2, 50)), now, &cx);
            rib.plan(&cx);
            let keys = Keys {
                g: geom(),
                row: 2,
                col0: 50,
                n: 3,
                period_ms: 60,
                disp: 0.85,
            };
            type_keys(rib, at(t0, 12 * 60 + 200), keys, &c);
        };
        let frame = |rib: &mut Ribbon, sink: &mut Sink| {
            let now = at(t0, 12 * 60 + 900);
            let mut cx = ctx(now, &c, (2, 53), 0.5);
            cx.phase = 0.3;
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        };
        let (mut a, mut b) = (Ribbon::new(), Ribbon::new());
        drive(&mut a);
        drive(&mut b);
        let (mut sa, mut sb) = (Sink::default(), Sink::default());
        frame(&mut a, &mut sa);
        frame(&mut b, &mut sb);
        assert!(
            !sa.under.is_empty(),
            "the frame must draw for the pin to mean anything"
        );
        assert_eq!(
            sa.under, sb.under,
            "two ribbons fed the same events differ in `under`"
        );
        assert_eq!(sa.out, sb.out, "…or in `out`");
        // …and the same ribbon re-planned at the same `now` is the same frame:
        // the plan is a function of `now`, not of how many times it ran.
        let mut again = Sink::default();
        frame(&mut a, &mut again);
        assert_eq!(
            sa.under, again.under,
            "re-planning at one `now` changed the frame"
        );
        assert_eq!(sa.out, again.out);
    }

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

    /// A row band carries its ribbon with its text (seam point 12, the band
    /// path), phase A: the inline viewport `[vt..56]` slides down one row per
    /// streamed line with the composer inside it, while the transcript above
    /// `vt` stands still. Six cells typed on composer row 20 land on row 21
    /// with column, field stop, birth and cohort untouched; three cells on
    /// transcript row 5 do not move; and between the band move and the next
    /// plan the field index already answers for the moved cells (the engine
    /// seeds `caret_t` from the caret's field before it plans) and for
    /// nothing on the vacated row.
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

    /// Phase B: the viewport is pinned at the bottom and every streamed line
    /// archives the transcript `[0..51]` up one row under a fixed composer.
    /// Light on transcript row 3 rides to row 2; the composer's light on row
    /// 54 and its caret do not move; light on row 0 leaves through the top of
    /// the band, dropped rather than parked on row 0.
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

    /// What leaves the band is gone: a ribbon on bottom row 56 pushed down by
    /// the viewport's slide has no row to land on, so its cells and cohort
    /// retire, the field index answers `None` on the old row and past the
    /// edge, and the ribbon is at rest. The caret is a position and
    /// saturates at the band's edge instead.
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

    /// A scroll carries the ribbon with its text (seam point 12, the PTY's
    /// one-row scroll on Enter at the foot of the screen). The hand types four
    /// cells and backspaces the last, so the caret stands on a retracting
    /// cell whose stop is not the newest key's; then the screen scrolls one
    /// row. Between the scroll and the next plan the ribbon already answers
    /// for the moved text (the caret one row up, the field at the caret the
    /// same cell's stop, nothing on the old row), and the next frame, planned
    /// at the same instant with the host's translated caret, is the
    /// pre-scroll frame moved up exactly one cell height, with every cell's
    /// clocks and prices untouched. A scroll past the text drops it.
    ///
    /// The frame clause uses the 2× fixture (`ch 36`): `ribbon_beam` phases
    /// its 4×4 Bayer dither on absolute device y, so a translated frame is
    /// byte-identical only when the cell height is a multiple of four (not
    /// the 1× fixture's `ch 18`).
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
        assert!(
            !before.out.is_empty(),
            "the hot edge must draw for the law to bite"
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
        assert_eq!(
            after.out.len(),
            before.out.len(),
            "the hot edge kept every quad"
        );
        for (a, b) in before.out.iter().zip(&after.out) {
            assert_eq!(
                (b.x, b.w, b.h, b.color, b.alpha),
                (a.x, a.w, a.h, a.color, a.alpha)
            );
            assert_eq!(
                b.y + ch,
                a.y,
                "a hot-edge quad moved by {} px, not one cell height ({ch})",
                i32::from(a.y) - i32::from(b.y)
            );
        }
        // A scroll past the text drops it: light on a line nobody typed.
        rib.translate_scroll(2);
        assert!(rib.at_rest());
        assert_eq!(rib.field_at(0, 13), None);
    }

    // -- §18 cadence: a fade's next level is solved, not polled -------------

    /// A fade's next u8 level is solved in closed form (§18's cadence law):
    /// for the theme's `spend` and the cell's expiry melt, over bright and dim
    /// marks and every phase of the fade, the instant [`level_step_spend`] /
    /// [`level_step_melt`] names is the first at which `round(peak · curve)`
    /// drops, to within 0.2 ms of a 10 µs poll, and `None` exactly when the
    /// mark is already at zero.
    #[test]
    fn a_fades_next_level_is_solved_not_polled() {
        let poll = |level_now: f32, curve: &dyn Fn(f32) -> f32, u: f32, span: f32| -> f32 {
            let mut t = 0.0f32;
            loop {
                t += 0.000_01;
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

    /// A line still being typed keeps every cell from its first key to the
    /// caret. "rainbow" at 9 cps on a hot spine (`birth_disp` 0.9), a 700 ms
    /// thinking pause, "theme" slowly at 4 cps on the dipped spine (0.45),
    /// then " truly magical" at 8 cps hot again. Planned every 16 ms until
    /// the last key, every glyph cell from the first key to the caret's
    /// neighbour is in the pool: a slow cell's short life runs on the
    /// cohort's clock, not its own. After the last key nothing is dropped
    /// through the 0.75 s grace, and the mark is off the glass (pool empty,
    /// at rest) [`SWOOSH_TOTAL_S`] after it.
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
        // Unchanged: the swoosh is still the ending. Through the grace every
        // cell stands; SWOOSH_TOTAL_S after the last key the mark is gone.
        let last = key_ms;
        let cx = ctx_in(at(t0, last + 700), &c, (row, caret), 0.9, g);
        rib.plan(&cx);
        assert_eq!(
            (col0..caret)
                .filter(|&col| rib.field_at(row, col).is_some())
                .count(),
            usize::from(caret - col0),
            "every cell stands through the 0.75 s grace"
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

    /// A parking pass is a parking pass downward too
    /// ([`Ribbon::typed_landing`]'s direction clause).
    ///
    /// [`Ribbon::lay`]'s fold arithmetic walks a cell at `pane_col0` back onto
    /// the end of the row above whichever row it was parked on, so a repaint
    /// that homes down to the pane's first column (a box's third line, the
    /// hint row under a composer, a `\r\n` sampled between the newline and
    /// the writes) would lay a stray on a row the hand was never on and leave
    /// a one-cell hole at the head of the band: with the hand at `(13, 18)`
    /// and a park at `(15, 0)`, the cell would land at `(14, 119)`.
    ///
    /// A run that simply begins lower must still lay where the tick says, and
    /// no bound on distance separates the two cases
    /// ([`a_downward_landing_is_questioned_only_where_lay_would_write_above_it`]
    /// shows the pair). The clause is stated on the fold
    /// ([`Ribbon::fold_top_row`]): a landing is the hand's own when the
    /// topmost row `lay` would write on is the landing's own row or the pen's.
    ///
    /// Twins: without the downward clause (`if caret.0 > pen.0 { return
    /// caret; }`) the first assertion fails with column 18 dark and the stray
    /// at `(14, 119)`; dropping the `top == i32::from(caret.0)` disjunct (every
    /// downward fold refused) fails the `(14, 1)` leg.
    #[test]
    fn a_repaint_that_parks_below_the_hand_does_not_take_the_key_with_it() {
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
        assert_eq!(
            landing_lit(&rib, 13),
            (2..=17).collect::<Vec<_>>(),
            "fixture: the hand's band is standing on its own row"
        );
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (15, 0), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 13),
            (2..=18).collect::<Vec<_>>(),
            "the key did not land on the hand's own row, next to its last cell"
        );
        assert!(
            landing_any(&rib, 14).is_empty(),
            "a stray was laid on the row above the parking pass: {:?}",
            landing_any(&rib, 14)
        );
        // …and the key's own reach is still taken as it comes: a fold onto
        // the next row lays there.
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
        let now = at(t0, 16 * 60);
        let cx = ctx_in(now, &c, (14, 1), 0.7, g);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            landing_lit(&rib, 14),
            vec![0],
            "the hand's own fold stopped laying on the row it folded onto"
        );
    }

    /// A band on its way out is still the hand's row
    /// ([`Ribbon::typed_landing`]'s evidence clause). The clause that keeps a
    /// key from being re-homed onto a row the ribbon holds nothing on reads
    /// every cell, leaving or not: after a held Backspace every cell on the
    /// hand's row is retracting, which is the strongest evidence the hand is
    /// there. Here the hand is at `(13, 2)` after sixteen Backspaces
    /// (retracting cells `[2..=8]`) and a parking pass at `(12, 0)` must not
    /// lay the key at `(11, 119)`. A cold session still names nothing:
    /// [`Ribbon::pen`] is `None` until the hand acts
    /// ([`a_cold_caret_mirror_is_not_a_hand_a_key_can_be_brought_back_to`]).
    ///
    /// This also pins the Backspace half of the pen: an erase moves
    /// [`Ribbon::pen`] to the caret it was replayed against.
    ///
    /// Twins: put `&& !c.leaving()` back on the evidence clause and the key
    /// lands at `(11, 119)`; drop `self.pen = Some(ctx.caret)` from the
    /// `Erase` arm and the key goes back to column 18, where typing stopped.
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

    /// A parking pass is not sampled at one column
    /// ([`Ribbon::typed_landing`] has no column clause). The host samples the
    /// caret on its own clock, so a repaint that homes and rewrites is caught
    /// mid-rewrite as often as at the home ([`Ribbon::hand_changed_row`]). A
    /// park sampled at `(12, 5)` with the hand at `(13, 18)` must not lay a
    /// stray on row 12 and leave column 18 dark. The upward clause already
    /// excludes every typed echo (a key's caret is never up from the pen
    /// except through the fold) at every column.
    ///
    /// Twin: let every upward caret through (`else if true` in place of the
    /// upward clause) and column 0 of this loop fails first, with the stray
    /// behind it at each of the five columns; this test,
    /// [`a_parking_pass_after_the_row_was_backspaced_empty_still_finds_the_hand`]
    /// and
    /// [`a_repaints_home_pass_poisons_the_mirror_and_the_pen_still_holds_the_hand`]
    /// fail together.
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

    /// The mirror is poisoned by the very pass the sibling law refuses, which
    /// is why [`Ribbon::typed_landing`] asks [`Ribbon::pen`] and not
    /// [`Ribbon::caret`].
    ///
    /// A repaint's home reaches the ribbon as a `Move` licensed `Typed`;
    /// [`Ribbon::hand_changed_row`] correctly refuses to abandon the hand's
    /// row on it
    /// ([`a_repaints_home_pass_does_not_abandon_the_row_the_hand_is_typing_on`]),
    /// but the `Move` arm sets `self.caret = Some(to)` unconditionally. The
    /// key's echo then arrives with the same parked caret, and a predicate
    /// asking the mirror would find `hand == caret` and lay the cell at the
    /// park, which [`Ribbon::lay`]'s fold walks onto the end of the row
    /// above. The pen carries the law because an echoed move never moves it;
    /// only a typed lay, an erase, or a nav the hand made does.
    ///
    /// Twin: read `self.caret` instead of `self.pen` in
    /// [`Ribbon::typed_landing`] and this test alone fails, the cell laid at
    /// `(11, 119)` with column 18 of the band dark.
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

    /// A nav is the hand, so it moves the pen. The downward fold clause is
    /// only as good as the pen: if the hand could get somewhere the pen does
    /// not follow, a legitimate fold from there would be re-homed onto a row
    /// the hand left. The one such way is moving the caret without typing
    /// (arrow, word hop, click), a `Move` under a nav licence; so a nav moves
    /// the pen and an echo never does. Here: sixteen keys on row 13, a nav to
    /// the last column of row 20, then one key that wraps onto row 21 at
    /// column 0; its glyph belongs at the last column of row 20.
    ///
    /// Twin: drop the `!licence.is_echo()` pen update in the `Move` arm and
    /// the wrap key is re-homed to `(13, 19)`, laid at `(13, 18)`, and row 20
    /// is left empty.
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
        // The wrap key's glyph is the last column of row 20, which is what
        // `Ribbon::lay`'s fold arithmetic is for: the caret really did wrap
        // off that cell. The landing must not be taken for a park and walked
        // back to the row the nav left.
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

    /// The fold is the fact, not the distance ([`Ribbon::typed_landing`]'s
    /// downward clause). No positional predicate separates this pair:
    ///
    /// * pen `(13, 18)`, pass at `(15, 0)`, two rows down: a repaint's park,
    ///   to be caught
    ///   ([`a_repaint_that_parks_below_the_hand_does_not_take_the_key_with_it`]);
    /// * pen `(2, 22)`, key at `(5, 3)`, three rows down: a run that simply
    ///   begins lower, to be taken as it comes
    ///   ([`a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it`]).
    ///
    /// The legitimate one is further away and at a higher column, and both
    /// fall inside the open phrase. What decides is what [`Ribbon::lay`]
    /// would do: at `(15, 0)` the fold walks the cell onto the end of row 14,
    /// where the hand never was; at `(5, 3)` nothing folds. So a typed key
    /// never writes above its own landing row unless the row it reaches is
    /// the pen's. The price, pinned in leg (b): a park sampled at `(15, 3)`
    /// is not caught, being identical to a run beginning at `(5, 3)`.
    ///
    /// Twin: remove the downward clause and leg (a) fails with column 18 dark
    /// and the stray on row 14. The other side of the clause (a downward
    /// fold that reaches the pen) is held by
    /// [`a_delivered_burst_that_wraps_keeps_the_cells_it_left_on_the_row_above`].
    /// Leg (b) asserts the landing is taken, so any widening of the refusal
    /// fails it.
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
        // (b) The same distance and direction, three columns over: nothing
        // folds, so the key lays where the tick says (the C2 shape, stated as
        // a price).
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

    /// The one fold that is real: the one that reaches the hand
    /// ([`Ribbon::fold_top_row`]). Five glyphs committed with the hand at
    /// `(13, 117)` of a 120-column grid echo at `(14, 2)`, and three of their
    /// cells belong at `117..120` on row 13, through the same fold
    /// arithmetic the downward clause questions. This fold reaches row 13,
    /// the pen's own, so it is the hand's line continuing across the wrap; a
    /// park's fold from `(15, 0)` under a pen at `(13, 18)` reaches row 14,
    /// where nobody is.
    ///
    /// Twin: drop the `top == i32::from(pen.0)` disjunct and this test alone
    /// fails: the burst is re-homed and row 14 is empty. Dropping the other
    /// disjunct fails
    /// [`a_repaint_that_parks_below_the_hand_does_not_take_the_key_with_it`]
    /// and leaves this one green: each half is pinned separately.
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

    /// A mirror is not a hand ([`Ribbon::typed_landing`]'s guard).
    /// [`Ribbon::caret`] is seeded by [`Ribbon::plan`] from the engine's caret
    /// mirror, which reads `(0, 0)` until a caret is observed
    /// (`Engine::caret_known`, which the ribbon is not given). Read as a hand
    /// it would make row 0 the hand's row and send the session's first key
    /// there (with `caret=(2, 5)`, the cell would land at `(0, 1)`).
    ///
    /// Twin: drop the `self.cells` guard and the key goes to row 0.
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

    /// A key sampled at readline's CR park stays on the hand's own row. GNU
    /// readline repositions with a bare CR and reprints, so mid-redraw the
    /// caret passes through columns `0..prompt_width` on the row the hand is
    /// typing on, licensed `Typed`. With the pen's own row exempt from the
    /// fold question, such a key would reach [`Ribbon::lay`] at a column
    /// under the pane's first, and `lay`'s fold walk (`while c < pane_col0 {
    /// r -= 1; c += pane_cols }`) would put its cells at the end of the
    /// previous row (the previous command's output) while the hand's column
    /// stayed dark. The pane subcase is the same walk one pane in.
    ///
    /// Twin: restore the exemption (`if caret.0 == pen.0 { return caret }`
    /// ahead of the fold question) and the first two subcases fail at the
    /// stray. The third is the other side: a same-row caret that folds
    /// nothing is taken exactly as it comes.
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

    /// A repaint's home is not a gesture. A bottom-pinned composer repaints by
    /// homing to the box's top-left and rewriting; the host observes that
    /// caret and a fresh type hint licenses it `Typed`. The hand cannot type
    /// its way up and to the left, so the ribbon must not read it as leaving
    /// the row (e.g. `(13,46)->(12,0)`, `(13,13)->(11,0)`).
    ///
    /// Twin: call `leave_row` unconditionally and the first assertion fails:
    /// the band the hand is typing into is abandoned.
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

    /// A scroll has already told the ribbon. A composer at the screen's
    /// bottom grows a row up by scrolling inside the repaint that re-lays the
    /// box, so `translate_scroll` has already carried every cell of the row
    /// up with its glyphs, and the backward caret the same repaint shows a
    /// moment later is not a stale line to drain.
    ///
    /// Twin: drop the `scroll_followed` guard and every cell of the row comes
    /// back retracting.
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

    /// A one-cell backward typed echo is not a re-anchor, matching the host's
    /// `classify_move`, which admits a re-anchor only at `raw_dist > 2`. A
    /// same-row backward `Typed` move of one or two cells with no erase
    /// behind it is plain typing: under [`RE_ANCHOR_MIN_CELLS`] nothing is
    /// stamped and the census is unchanged; at the floor (the control) the
    /// re-anchor still drains. The trigger (a flushed one-cell park) is
    /// pinned at unit level in `cursor_glow/tests/park.rs`.
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

    /// What the floor buys: five keys and a Space (a cell the content witness
    /// never arms), then the caret observed one cell left as a same-row
    /// backward `Typed` echo with no erase (the flushed one-cell park,
    /// `cursor_glow/tests/park.rs`). Under [`RE_ANCHOR_MIN_CELLS`] the ribbon
    /// drains nothing: the Space past the caret stands (not leaving, no
    /// retract stamp), the cohort's clock is still the last key's, and the
    /// cell stays lit on that clock.
    #[test]
    fn a_never_armed_space_past_a_one_cell_backward_typed_echo_keeps_its_light() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Five glyphs at 10..15, then the space at 15; the caret at 16.
        type_run(&mut rib, t0, 10, 5, &c, 0.8);
        let last = at(t0, 5 * 60);
        let cx = ctx(last, &c, (2, 16), 0.8);
        rib.on_event(&typed_space(), last, &cx);
        rib.plan(&cx);
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .copied()
            .expect("the row's cohort");
        assert!(cell_cov(&rib, g, 15) > 0, "the space is lit at its key");
        // The echo: the caret observed one cell left, nothing erased.
        let now = at(last, 200);
        let cx = ctx(now, &c, (2, 15), 0.8);
        rib.on_event(&typed_move((2, 16), (2, 15)), now, &cx);
        rib.plan(&cx);
        let space = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15)
            .copied()
            .expect("the space cell is resident");
        assert!(
            !space.leaving() && space.retract_at.is_none(),
            "the space past the caret stands, unstamped"
        );
        assert_eq!(rib.caret(), Some((2, 15)), "the mirror moved");
        let after = rib
            .cohorts()
            .iter()
            .find(|k| k.id == coh.id)
            .copied()
            .expect("the cohort stands");
        assert_eq!(
            after.alive_at, coh.alive_at,
            "the cohort's clock is the last key's"
        );
        assert!(!after.abandoned && after.retract_col.is_none());
        for ms in [216u64, 300, 500] {
            let now = at(last, ms);
            let cx = ctx(now, &c, (2, 15), 0.8);
            rib.plan(&cx);
            assert!(
                cell_cov(&rib, g, 15) > 0,
                "+{ms} ms: the space keeps its light on its cohort's clock"
            );
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == 2 && l.col == 15 && !l.leaving()),
                "+{ms} ms: …and stands"
            );
        }
    }

    /// A ^A is a home, not a relocation. readline's `^A` reprints the line and
    /// parks the caret at the input's first column; the host sees a same-row
    /// backward move with a fresh type hint, licensed `Typed` with no erase,
    /// which is [`Ribbon::re_anchor`]'s arm. The span
    /// [`Ribbon::moved_word_len`] measures there (the program's caret to the
    /// hand's last Space) is no word the app moved, and relaying it would
    /// light the prompt.
    ///
    /// The law: a same-row re-anchor relays nothing left of the hand's own
    /// ink on that row ([`Ribbon::hand_floor`]). No cells are laid and no
    /// relocation is left pending.
    ///
    /// The clamp is not inside [`Ribbon::relay_word`] or
    /// [`Ribbon::begin_relocate`]: a composer's wrap relays onto a new row
    /// where the hand has no floor yet
    /// (`a_composer_re_anchor_relays_the_moved_word_on_the_new_row`,
    /// `a_held_space_measures_the_word_the_composer_moves_from_its_own_cell`).
    /// The row change is the discriminator.
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

    /// Slits: two keys whose echoes land on one tick replay as `Typed, Typed,
    /// Sweep, Move` against the landing caret; the first `Typed` lays one past
    /// the live cohort's end and mints a second cohort, which would split
    /// into two runs feathered at [`RUN_TAIL_EASE`], a dark ramp at the last
    /// cell of the batch. [`Ribbon::heal_seams`] makes it one cohort, one
    /// run, no interior boundary under the body's level.
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
        // …and a second batch on the same tick shape does not seam either.
        let k3 = at(t0, 560);
        let k4 = at(t0, 570);
        let now = at(t0, 580);
        let cx = ctx(now, &c, (2, 12), 0.9);
        rib.on_event(&typed(), k3, &cx);
        rib.on_event(&typed(), k4, &cx);
        rib.on_event(
            &Event::Sweep {
                row: 2,
                col0: 10,
                col1: 12,
            },
            k3,
            &cx,
        );
        rib.on_event(&typed_move((2, 10), (2, 12)), now, &cx);
        rib.plan(&cx);
        assert_eq!(rib.cohorts().iter().filter(|k| k.row == 2).count(), 1);
    }

    /// A key replayed against a caret whose cell is already lit lays that cell
    /// again and keeps its attack age instead of restarting the 18 ms edge-in
    /// on a settled cell. The engine holds a key whose echo has not landed,
    /// so the shape that reaches this re-lay is a glyph typed before a
    /// one-shot's move, replayed at the caret before that move (its run's own
    /// last cell).
    #[test]
    fn a_key_replayed_before_its_echo_keeps_the_previous_cell_s_attack() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 4, &c, 0.9);
        let cx = ctx(at(t0, 400), &c, (2, 6), 0.9);
        rib.plan(&cx);
        let attack_at = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 5)
            .expect("the last glyph's cell")
            .attack_at;
        let before = interior_covs(&rib, 5);
        // The next key, replayed on a tick that precedes its echo: the
        // caret still stands at 6, so the lay lands on cell 5 again.
        let k = at(t0, 420);
        let cx = ctx(at(t0, 421), &c, (2, 6), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let cell = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 5)
            .expect("still one cell");
        assert_eq!(cell.attack_at, attack_at, "the attack was restarted");
        assert_eq!(cell.born, k, "a fresh key must not inherit the old witness");
        assert_eq!(
            interior_covs(&rib, 5),
            before,
            "cell 5 dipped on the re-lay"
        );
    }

    /// The brighter side owns a boundary: a new cell's attack lives inside the
    /// new light (the settled cell beside it keeps every slab), and an erased
    /// neighbour's spend never sags the surviving head cell, so nothing pops
    /// when the erased cell retires. The new light is the attach under the
    /// caret cell: the cell the key lays where the attach stood inherits the
    /// attach's level (no byte moves down on the key), and the attack is the
    /// next attach's, one cell on.
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

    /// The editing hand holds the row: a Backspace run refreshes the cohort
    /// like a typed key, so the survivors do not drain under a hand still
    /// editing and the fix key finds them whole.
    #[test]
    fn an_erase_holds_the_row_s_cohort_through_the_edit() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        erase_at(&mut rib, at(last, 400), (2, 9), &c);
        erase_at(&mut rib, at(last, 800), (2, 8), &c);
        erase_at(&mut rib, at(last, 1200), (2, 7), &c);
        let cx = ctx(at(last, 1500), &c, (2, 7), 0.9);
        rib.plan(&cx);
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the cohort");
        assert!(
            matches!(coh.phase, Phase::Laying | Phase::Grace),
            "the cohort went into its exit under the editing hand: {:?}",
            coh.phase
        );
        for col in 2..7u16 {
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == 2 && l.col == col && l.retract_at.is_none()),
                "survivor {col} was drained under the hand"
            );
        }
        assert!(
            boundary_cov(&rib, 4).expect("inside the survivors") >= STATUS_LIT_COV,
            "the survivors are lit"
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

    /// A fold is not a departure: a typed move onto the next row is the same
    /// input continuing, so the row it wrapped off keeps its own clock and is
    /// not abandoned; keys on the new row still do not hold it
    /// (`OnlyOwnerRenews`), so it is gone well inside the swoosh.
    ///
    /// Twin: put `abandon_row` back on the fold and the first assertion fails
    /// immediately, and the "still lit 1 s later" one with it.
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
        // The band spans the fold, and flows: half a second after the wrap,
        // with the hand typing below, the row it wrapped off is still lit
        // (every cell resident) and moving: the fold set it flowing
        // (`Cohort::flow`, `leave_row`), so it is drawn toward the fold point
        // (its own right end), its leftmost light gone first.
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
    /// vertical extent covers the row's centre (the body), as
    /// `(coverage share, centroid px, leftmost px, rightmost px, lit px,
    /// body top px, their x positions)` — and `top` is the least `y` of any
    /// lit quad above the row's centre (the body reaches up into the row
    /// above's band). `None` with nothing lit.
    fn drift_census(sink: &Sink, g: Geom, row: u16) -> Option<DriftCensus> {
        let centre = f32::from(g.origin_y) + (f32::from(row) + 0.5) * g.ch as f32;
        let (mut cov, mut num, mut lit) = (0.0f32, 0.0f32, 0.0f32);
        let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);
        let mut xs = Vec::new();
        for q in &sink.under {
            let y0 = f32::from(q.y);
            let y1 = y0 + f32::from(q.h);
            if q.w == 0 || q.alpha == 0 || y0 > centre || centre >= y1 {
                continue;
            }
            let w = f32::from(q.w) * f32::from(q.alpha) / 255.0;
            cov += w;
            num += (f32::from(q.x) + f32::from(q.w) * 0.5) * w;
            lit += f32::from(q.w);
            left = left.min(f32::from(q.x));
            right = right.max(f32::from(q.x) + f32::from(q.w));
            xs.push(q.x);
        }
        if cov <= 0.0 {
            return None;
        }
        let top = sink
            .under
            .iter()
            .filter(|q| q.w > 0 && q.alpha > 0 && f32::from(q.y) < centre)
            .filter(|q| f32::from(q.x) >= left && f32::from(q.x) < right)
            .map(|q| f32::from(q.y))
            .fold(f32::INFINITY, f32::min);
        xs.sort_unstable();
        xs.dedup();
        Some(DriftCensus {
            cov: cov / (g.cols * g.cw) as f32,
            cen: num / cov,
            left,
            right,
            lit,
            top,
            xs,
        })
    }

    struct DriftCensus {
        cov: f32,
        cen: f32,
        left: f32,
        right: f32,
        lit: f32,
        top: f32,
        xs: Vec<u16>,
    }

    /// The row the hand folded off drifts into the fold and fades as one
    /// band. Sixty keys on row 2, the fold, then a hand typing on row 3 for
    /// the whole flow; row 2 is read every 16 ms frame:
    ///
    /// * monotone: its light never rises, its centroid never steps back from
    ///   the fold, and once dark it stays dark;
    /// * no cliff: no frame takes more than 4 % of the fold frame's light;
    /// * pitch: the body's slabs keep their laid stride, ±1 px, so the band
    ///   moves rigidly and its colours are never compressed;
    /// * clip: nothing is drawn right of the band's kept end, at a full-width
    ///   fold and at a fold at a 100-column pane's edge (where the
    ///   rasterizer's own clip does not coincide);
    /// * drift: by +0.304 s the far edge has moved at least half a cell, and
    ///   at +0.448 s the band still covers at least 90 % of its width;
    /// * fade: at +0.704 s the light per lit column is at most 60 % of the
    ///   fold frame's;
    /// * settle: by +1.0 s the body's top has come down by at least a quarter
    ///   of its reach above the spine;
    /// * life: still lit at +0.9 s, dark by `FLOW_TOTAL_S` plus a frame, and
    ///   not before `0.8 · FLOW_TOTAL_S`.
    ///
    /// Under reduced motion nothing moves and the row takes the one linear
    /// fade at the end: its edges, centroid and top stay put, and its light
    /// is whole until the fade's window opens. Without the drift's clamp in
    /// [`Ribbon::retract_x`] the pane fold's clip law fails.
    #[test]
    fn the_row_the_hand_folded_off_drifts_into_the_fold_and_fades_as_one_band() {
        // `(pane, first column, keys)`: sixty keys to the grid's last
        // column, and sixty to a 100-column pane's edge, whose kept end is
        // strictly inside the grid.
        for (pane, col0, n) in [(None, 60u16, 60u16), (Some((0u16, 100u16)), 40, 60)] {
            for reduced in [false, true] {
                let mut c = cfg(true, true);
                c.reduced_motion = reduced;
                let g = geom();
                let cw = g.cw as f32;
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                rib.set_pane(pane);
                let keys = Keys {
                    g,
                    row: 2,
                    col0,
                    n,
                    period_ms: 60,
                    disp: 0.9,
                };
                type_keys(&mut rib, t0, keys, &c);
                let end = col0 + n;
                let last = at(t0, u64::from(n - 1) * 60);
                let fold = at(last, 60);
                let cx = ctx(fold, &c, (3, 0), 0.9);
                rib.on_event(&typed_move((2, end), (3, 0)), fold, &cx);
                rib.plan(&cx);
                let mut sink = Sink::default();
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
                let first = drift_census(&sink, g, 2).expect("lit at the fold");
                let clip = f32::from(g.origin_x) + f32::from(end) * cw;
                // Not vacuous: the band stands on the clip line at the fold.
                assert!(
                    (first.right - clip).abs() <= 0.5,
                    "pane={pane:?}: the band's kept end is its last glyph's right edge at the \
                 fold: {} px against {clip} px",
                    first.right
                );
                let stride = first
                    .xs
                    .windows(2)
                    .map(|w| w[1] - w[0])
                    .min()
                    .expect("a band has slabs");
                let body_top = f32::from(g.origin_y) + 3.0 * g.ch as f32 - first.top;
                let mut prev = (first.cov, first.cen);
                let mut dark_at: Option<u64> = None;
                let (mut at_304, mut at_448, mut at_704, mut at_900, mut at_1008) =
                    (None, None, None, None, None);
                let mut worst_drop = 0.0f32;
                let mut key_col = 0u16;
                let mut ms = 16u64;
                while ms <= 1700 {
                    let now = at(fold, ms);
                    if ms / 60 != (ms - 16) / 60 {
                        key_col += 1;
                        let kx = ctx(now, &c, (3, key_col), 0.9);
                        rib.on_event(&typed(), now, &kx);
                    }
                    let cx = ctx(now, &c, (3, key_col), 0.9);
                    rib.plan(&cx);
                    let mut f = sink.frame();
                    rib.emit(&cx, &mut f);
                    let s = ms as f32 / 1000.0;
                    let Some(r) = drift_census(&sink, g, 2) else {
                        if dark_at.is_none() {
                            dark_at = Some(ms);
                        }
                        ms += 16;
                        continue;
                    };
                    assert!(
                        dark_at.is_none(),
                        "pane={pane:?} reduced={reduced}: dark at +{} ms and lit again at +{s:.3} s",
                        dark_at.unwrap_or(0)
                    );
                    assert!(
                        r.cov <= prev.0 + 2e-3,
                        "pane={pane:?} reduced={reduced}: the light rose from {} to {} at +{s:.3} s",
                        prev.0,
                        r.cov
                    );
                    worst_drop = worst_drop.max((prev.0 - r.cov) / first.cov);
                    assert!(
                        r.right <= clip + 0.5,
                        "pane={pane:?} reduced={reduced}: light at {} px, past the band's end at {clip} \
                     px (+{s:.3} s)",
                        r.right
                    );
                    if reduced {
                        assert!(
                            (r.cen - first.cen).abs() <= 0.5
                                && (r.left - first.left).abs() <= 0.5
                                && (r.top - first.top).abs() <= 0.5,
                            "pane={pane:?} reduced motion: the band moved at +{s:.3} s: centroid {} -> {}, left \
                         {} -> {}, top {} -> {}",
                            first.cen,
                            r.cen,
                            first.left,
                            r.left,
                            first.top,
                            r.top
                        );
                        if s < FLOW_TOTAL_S - REDUCED_MOTION_FADE_MS / 1000.0 - 0.02 {
                            assert!(
                                (r.cov - first.cov).abs() <= 2e-3,
                                "pane={pane:?} reduced motion: the light is whole until the fade: {} -> {} at \
                             +{s:.3} s",
                                first.cov,
                                r.cov
                            );
                        }
                    } else {
                        // The light-weighted centroid is read while the band
                        // still holds 5 % of its light: below that the alpha
                        // bytes' rounding moves it by sub-pixel amounts.
                        assert!(
                            r.cov < 0.05 * first.cov || r.cen >= prev.1 - 0.5,
                            "pane={pane:?} the centroid stepped back from the fold, {} -> {} at +{s:.3} s",
                            prev.1,
                            r.cen
                        );
                        // Pitch, away from the fold edge where slabs collapse.
                        for w in r.xs.windows(2) {
                            if f32::from(w[1]) + 2.0 * cw >= clip {
                                break;
                            }
                            // Compression is a stride shorter than laid; a
                            // longer one is a slab whose alpha byte rounded to
                            // zero in the last few per cent of the light.
                            let d = w[1] - w[0];
                            assert!(
                                d + 1 >= stride,
                                "pane={pane:?} the band was compressed at +{s:.3} s: slabs {} px apart where \
                             they were laid {stride} px apart",
                                d
                            );
                        }
                    }
                    match ms {
                        304 => at_304 = Some((r.left, r.lit)),
                        448 => at_448 = Some(r.lit),
                        704 => at_704 = Some(r.cov / r.lit),
                        896 | 912 => at_900 = Some(()),
                        1008 => at_1008 = Some(r.top),
                        _ => {}
                    }
                    prev = (r.cov, r.cen);
                    ms += 16;
                }
                let dark = dark_at.expect("the folded row goes dark") as f32 / 1000.0;
                assert!(
                    dark <= FLOW_TOTAL_S + 0.017,
                    "pane={pane:?} reduced={reduced}: dark by FLOW_TOTAL_S plus a frame: {dark}"
                );
                assert!(
                    dark >= 0.8 * FLOW_TOTAL_S,
                    "pane={pane:?} reduced={reduced}: …and not before 0.8 of the flow: {dark}"
                );
                assert!(
                    at_900.is_some(),
                    "pane={pane:?} reduced={reduced}: still lit at +0.9 s"
                );
                if reduced {
                    continue;
                }
                assert!(
                    worst_drop <= 0.04,
                    "pane={pane:?} NO CLIFF: a frame took {:.1} % of the fold frame's light",
                    100.0 * worst_drop
                );
                let (left, _) = at_304.expect("lit at +0.304 s");
                assert!(
                    left >= first.left + 0.5 * cw,
                    "pane={pane:?} DRIFT: the far edge moved from {} px to {left} px by +0.304 s",
                    first.left
                );
                let lit = at_448.expect("lit at +0.448 s");
                assert!(
                    lit >= 0.9 * first.lit,
                    "pane={pane:?} DRIFT: the band covers {lit} px of its {} px at +0.448 s",
                    first.lit
                );
                let per = at_704.expect("lit at +0.704 s");
                assert!(
                    per <= 0.6 * (first.cov / first.lit),
                    "pane={pane:?} FADE: {per} per lit px at +0.704 s against {} at the fold",
                    first.cov / first.lit
                );
                let top = at_1008.expect("lit at +1.008 s");
                assert!(
                    top >= first.top + 0.25 * body_top,
                    "pane={pane:?} SETTLE: the body's top came down from {} to {top} px by +1.0 s (reach {body_top} \
                 px)",
                    first.top
                );
                assert!(
                    rib.cells().iter().any(|l| l.row == 3),
                    "pane={pane:?} the new row is lit under the hand throughout"
                );
            }
        }
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

    /// The seam marks the continued line, and only it. Keys on row 2, then a
    /// wrap (a shell's typed fold to `(3, 0)`), a composer's re-wrap (the
    /// row's last word carried down to `(3, 2..7)` with the wrap key's glyph
    /// at 7), a soft-wrapped caret, or a Return, then twelve keys on row 3.
    /// Along row 3's tail more than [`HOT_EDGE_CELLS`] behind the head, after
    /// any continuation the hairline holds the seam's floor, spent (dimmer at
    /// +0.7 s than at +0.3 s, gone by [`FLOW_TOTAL_S`]); after the Return, and
    /// under reduced motion, it is dark throughout.
    ///
    /// Back to the hand's first column on the row and no further: in the
    /// re-wrap the relay lays the moved word as the app's text
    /// ([`Ribbon::relay_word`] touches no floor), so the first cell the hand
    /// laid is the wrap key's glyph and the word lies left of it in the same
    /// run. In the soft wrap ([`SoftWrap`]) the next key reflows `abcde` down
    /// from the indent ([`Ribbon::reflow_move`], [`Ribbon::settle_carry`]),
    /// and the hand's first cell is the reflow key's glyph at 7, or the key
    /// after it at 8 when the reflow key was held for a late echo. The seam
    /// covers neither the relaid word nor the indent; without
    /// `seg.x >= floor_x` in [`Ribbon::emit_hot_edge`] it runs over the word
    /// from column 2.
    #[test]
    fn the_seam_marks_a_continued_line_s_tail_and_spends_with_the_row_above() {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        enum Line {
            Wrap,
            ReWrap,
            /// `held`: the reflow key's echo lands a tick after it (the key
            /// held at the landing, its glyph relaid with the word), or in
            /// its own tick (the key laid at the frame's caret).
            SoftWrap {
                held: bool,
            },
            Return,
        }
        // `(far-tail light, its leftmost px, the hand's floor on row 3, the
        // first column of the run the hand is on)`.
        let tail_light = |line: Line, reduced: bool, ms: u64| {
            let mut c = cfg(true, true);
            c.reduced_motion = reduced;
            let g = geom();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let soft = SoftWrap {
                origin: (2, 119),
                landing: (3, 2),
                word_col0: 115,
            };
            let fold = if matches!(line, Line::ReWrap | Line::SoftWrap { .. }) {
                // Fourteen glyphs, a Space at 114 and `abcde` at 115..119; for
                // the soft wrap the `e` is the wrap key, its glyph laid at the
                // origin as the engine's replay lays it.
                let keys = Keys {
                    g,
                    row: 2,
                    col0: 100,
                    n: 14,
                    period_ms: 60,
                    disp: 0.9,
                };
                type_keys(&mut rib, t0, keys, &c);
                for i in 0..6u16 {
                    let now = at(t0, u64::from(14 + i) * 60);
                    let ev = if i == 0 { typed_space() } else { typed() };
                    let cx = ctx(now, &c, (2, 115 + i), 0.9);
                    rib.on_event(&ev, now, &cx);
                    rib.plan(&cx);
                }
                at(t0, 20 * 60)
            } else {
                let keys = Keys {
                    g,
                    row: 2,
                    col0: 100,
                    n: 20,
                    period_ms: 60,
                    disp: 0.9,
                };
                type_keys(&mut rib, t0, keys, &c);
                at(t0, 20 * 60)
            };
            // Where the row-3 keys start: after the relaid word and the
            // key's glyph beside it for the re-wrap and the soft wrap.
            let landing = if matches!(line, Line::ReWrap | Line::SoftWrap { .. }) {
                8
            } else {
                0
            };
            let fold_caret = match line {
                Line::SoftWrap { .. } => soft.landing,
                _ => (3, landing),
            };
            let cx = ctx(fold, &c, fold_caret, 0.9);
            match line {
                Line::Wrap => rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx),
                Line::ReWrap => {
                    // The wrap key, then its echo: the app moved `abcde`
                    // down with the caret, which stands after the key's
                    // glyph.
                    rib.on_event(&typed(), fold, &cx);
                    rib.on_event(&typed_move((2, 120), (3, landing)), fold, &cx);
                }
                Line::SoftWrap { .. } => {
                    // The wrap key's echo: only the caret wrapped.
                    rib.soft_wrap_move(&typed_move(soft.origin, soft.landing), fold, &cx, soft);
                }
                Line::Return => {
                    rib.on_event(&Event::Return, fold, &cx);
                    let ret = Event::Move {
                        from: (2, 120),
                        to: (3, 0),
                        licence: Licence::Return,
                        dir: Dir::of(-120, 1),
                    };
                    rib.on_event(&ret, fold, &cx);
                }
            }
            rib.plan(&cx);
            if let Line::SoftWrap { held } = line {
                // The reflow key, and at +10 ms its echo: the app erased
                // `abcde` off row 2 and rewrote it from the indent, the key's
                // glyph after it and the caret at 8, the hop the seam's content
                // witness hands the ribbon as a reflow.
                let (press, echo) = if held {
                    (at(fold, 4), at(fold, 10))
                } else {
                    (at(fold, 10), at(fold, 10))
                };
                let echo_cx = ctx(echo, &c, (3, landing), 0.9);
                if held {
                    let cx = ctx(press, &c, soft.landing, 0.9);
                    rib.hold_typed(&typed(), press, &cx, Some(soft.landing));
                    rib.plan(&cx);
                } else {
                    rib.on_event(&typed(), press, &echo_cx);
                }
                rib.reflow_move(
                    &typed_move(soft.landing, (3, landing)),
                    echo,
                    &echo_cx,
                    soft,
                );
                rib.plan(&echo_cx);
            }
            // Twelve keys on the new row, done by +0.26 s; the reads come
            // after them.
            let keys = Keys {
                g,
                row: 3,
                col0: landing,
                n: 12,
                period_ms: 20,
                disp: 0.9,
            };
            type_keys(&mut rib, at(fold, 20), keys, &c);
            let head = landing + 12;
            let now = at(fold, ms);
            let cx = ctx(now, &c, (3, head), 0.9);
            rib.plan(&cx);
            let mut sink = Sink::default();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let (far, left) = seam_far_tail(&sink, g, 3, head);
            let run_col0 = rib.runs.first().map(|r| r.col0);
            (far, left, rib.hand_floor_on(3), run_col0)
        };
        let cw = geom().cw as f32;
        for line in [
            Line::Wrap,
            Line::ReWrap,
            Line::SoftWrap { held: false },
            Line::SoftWrap { held: true },
        ] {
            let (early, left, floor, run_col0) = tail_light(line, false, 300);
            assert!(
                early > 0.0,
                "{line:?}: the new line's far tail carries the seam at +0.3 s"
            );
            let floor = floor.expect("the hand has a floor on the new row");
            let floor_x = f32::from(geom().origin_x) + f32::from(floor) * cw;
            assert!(
                (left - floor_x).abs() <= 0.5,
                "{line:?}: the seam reaches back to the hand's first column on the row \
                 ({floor}, {floor_x} px) and no further: its far tail starts at {left} px"
            );
            let (later, _, _, _) = tail_light(line, false, 700);
            assert!(
                later < early,
                "{line:?}: …and it spends with the row above: {early} at +0.3 s, {later} at \
                 +0.7 s"
            );
            let gone_ms = (FLOW_TOTAL_S * 1000.0) as u64 + 20;
            assert_eq!(
                tail_light(line, false, gone_ms).0,
                0.0,
                "{line:?}: …gone by the flow's end"
            );
            assert_eq!(
                tail_light(line, true, 300).0,
                0.0,
                "{line:?}: reduced motion: no seam"
            );
            if let Line::SoftWrap { held } = line {
                // The reflowed word lies at 2..7 and the reflow key's glyph at
                // 7; the first cell the hand laid is that glyph or, when held,
                // the key after it at 8. Never the word, never the indent.
                let hand_first = if held { 8 } else { 7 };
                assert_eq!(
                    floor, hand_first,
                    "{line:?}: the seam's floor is the first cell the hand laid on the row"
                );
                // Not vacuous: the hand's run starts at the indent's end —
                // the relaid word is in it, left of the floor.
                assert_eq!(
                    run_col0,
                    Some(2),
                    "{line:?}: the reflow relaid `abcde` from the indent's end into the \
                     hand's run"
                );
            } else if line == Line::ReWrap {
                // Not vacuous: the hand's run starts left of the floor (the
                // relaid word), so a seam without its floor clause would run
                // over it.
                let run_col0 = run_col0.expect("the hand's run");
                assert!(
                    run_col0 < floor,
                    "the fixture's point: the hand's run starts at {run_col0}, left of the \
                     hand's first column {floor} (the relaid word)"
                );
            } else {
                assert_eq!(
                    floor, 0,
                    "a shell fold's new line starts at the pane's edge"
                );
            }
        }
        assert_eq!(
            tail_light(Line::Return, false, 300).0,
            0.0,
            "a line the hand STARTED (a Return) carries no seam"
        );
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

    /// A Backspace run back through a shell fold re-wets the row, and the row
    /// holds under the hand. A shell's erase lays nothing, so it reaches the
    /// flowing row only through [`Ribbon::hold`], which must also stop the
    /// flow, or every Backspace would restart the drift and the row would
    /// drain away under the parked caret. Twenty keys on row 2, the fold to
    /// `(3, 0)`, then six Backspaces 60 ms apart (the first up through the
    /// fold, its erase replayed at `(3, 0)` ([`BsEcho::Late`]`(0)`) and its
    /// echo taking the caret to `(2, 119)`, the rest along row 2), and the
    /// caret parked. From the first Backspace ([`Ribbon::take_row_back`])
    /// nothing on the row flows, its cohort holds in its laying or grace,
    /// its left edge stands still and its light keeps with a never-folded
    /// control until the grace ends.
    #[test]
    fn a_backspace_run_back_through_a_shell_fold_re_wets_the_row_and_holds_it() {
        let bs_ms = [416u64, 480, 540, 600, 660, 720];
        let (control, _) = bs_back_through_a_fold(false, BsEcho::Late(0), &bs_ms);
        let (folded, _) = bs_back_through_a_fold(true, BsEcho::Late(0), &bs_ms);
        assert_the_row_holds_under_the_hand("late(0)", &folded, &control, bs_ms[0], u64::MAX);
    }

    /// …and when the echo lands with the key, from the first Backspace. The
    /// engine replays an erase at the tick's caret, so on this common path
    /// the erase that takes the caret up through the fold is replayed at
    /// `(2, 119)`, on the row, and its hold re-wets the row immediately. The
    /// same run with every erase and echo in one tick ([`BsEcho::SameTick`]).
    /// Fails without the hold's re-wet, and also if the re-wet is gated on
    /// the ribbon's caret mirror already being on the held row.
    #[test]
    fn a_backspace_back_through_a_shell_fold_whose_echo_lands_with_the_key_re_wets_the_row_at_once()
    {
        let bs_ms = [416u64, 480, 540, 600, 660, 720];
        let (control, _) = bs_back_through_a_fold(false, BsEcho::SameTick, &bs_ms);
        let (folded, _) = bs_back_through_a_fold(true, BsEcho::SameTick, &bs_ms);
        assert_the_row_holds_under_the_hand("same tick", &folded, &control, bs_ms[0], u64::MAX);
    }

    /// The hand arriving back through the fold takes the row back, after the
    /// rest and for a lone Backspace. When the echo lands after its key's
    /// tick, the erase that takes the caret up through the fold is replayed
    /// at `(3, 0)` and holds only that row; after the rest it also mints a
    /// phrase that adopts only that row, so row 2's flowing cohort (of the
    /// old phrase) would never be held. A lone Backspace makes no erase on
    /// row 2 at all. Both must hold with the never-folded control from the
    /// frame the echo lands on, through the grace of the last hold, for every
    /// echo shape ([`BsEcho`]). Without [`Ribbon::take_row_back`] row 2 is
    /// still flowing at the echo's frame for both late shapes.
    #[test]
    fn the_hand_arriving_back_through_a_shell_fold_takes_the_row_back_after_the_rest_and_alone() {
        let runs: [(&str, &[u64]); 2] = [
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

    /// A composer re-wrap's moved word fades in place while the row it left
    /// drifts. The re-wrap stamps the moved word's old cells (and the eaten
    /// Space) to leave over [`RETRACT_FADE_S`] ([`Ribbon::leave_row`]), and
    /// the row flows into its kept end, left of them
    /// ([`Ribbon::flow_target`]). Thirty glyphs at 40..69, a Space at 70, an
    /// eight-letter word at 71..78, the wrap key carrying the word to
    /// `(3, 2..10)`, and the hand typing on; row 2 read every 16 ms: no cliff
    /// (no frame takes more than 4 % of the wrap frame's light), the word
    /// keeps at least 80 % of its light at +16 ms, nothing is drawn right of
    /// the word's old end, and once the word's cells are gone nothing right
    /// of the kept end (clip, with a kept end strictly inside the grid).
    /// Fails with the drift clamp applied to every boundary (the word cut at
    /// +16 ms), and with it removed (clip).
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

    /// A band drifting left pours into its kept start and never past it: the
    /// drift's mirror. A run the follow pass carries down, away from the
    /// caret's row, flows into its own left end ([`Ribbon::flow_target`]'s
    /// row below the caret). Thirty keys on row 5 at 20..49, carried to row
    /// 6 with the caret staying on row 5: the band's left edge never passes
    /// column 20 while its right end drifts toward it. Fails without the
    /// `max` clamp in [`Ribbon::retract_x`].
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
    }

    /// A band move under the hand carries the seam and its floor together.
    /// The seam reaches back to the hand's first column on its row
    /// ([`Ribbon::hand_floor_on`]) and draws nothing where that floor is
    /// unknown. A fold, twelve keys on the new row, then the band `0..=39`
    /// moved down one row (an inline viewport sliding per streamed line):
    /// the seam's far tail on row 4 reads what it reads on row 3 with no
    /// move, and the floor is `(4, 0)`, so the meteor's floor follows the
    /// text too. Fails if [`Ribbon::translate_band`] leaves the floor behind.
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

    /// Under reduced motion a flowing band asks for no wake before its fade.
    /// The flow moves nothing and its life is frozen at the fold, so the band
    /// is static at full light until the one linear fade opens at
    /// `FLOW_TOTAL_S − REDUCED_MOTION_FADE_MS` (§6.11); the cadence must not
    /// offer the flow's `FLOW_LEAD_S` motion boundary or any cell's frozen
    /// life end. Twenty keys at a slow spine (life 1.70 s), a pause, the
    /// fold; every read from +0.10 s to +1.30 s asks to be woken no sooner
    /// than the fade's opening, one [`TAIL_FLOOR`] early at most.
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

    /// A shift of a flowing band carries its frozen fold edge. The flow
    /// freezes its fold edge on its first frame ([`Ribbon::flow_target`]); an
    /// insert typed into the row after a Nav back up (the key mints its own
    /// cohort and the row keeps flowing) pushes the tail right
    /// ([`Ribbon::shift_run`]). The edge is a glyph's column, so it rides the
    /// shift with that glyph (the walk's anchor does not: the pushed cells
    /// take their new columns' stops), and the pushed glyph's column stays
    /// lit through the flow.
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

    /// A slow hand's folded row flows whole, whatever it paused before the
    /// wrap. A slow hand's cells live only 1.70 s (`disp` 0.2), and a pause
    /// before the wrapping key spends most of that before the flow begins, so
    /// expiry must be frozen at the fold. Thirty keys 150 ms apart, a pause
    /// of 60, 600 or 900 ms, the fold, and the hand typing on: every pause
    /// gives the flow's own timeline (dark no earlier than
    /// `0.8 · FLOW_TOTAL_S` and by `FLOW_TOTAL_S` plus a frame; no frame
    /// takes more than 4 % of the fold frame's light). Fails with either
    /// freeze reverted ([`Ribbon::env_of`]'s `spent_to`, [`Ribbon::retire`]'s
    /// `flowing`).
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
        let (dark0, share0, _) = flow(60);
        for pause_ms in [60u64, 600, 900] {
            let (dark, share, worst) = flow(pause_ms);
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

    /// A composer's re-anchor relays the moved word: Ink moves the row's last
    /// word down with the wrap key and the caret lands after it; the word's
    /// cells are laid on the new row from the last Space's measure, and its
    /// old cells leave with the old row. Both observed shapes: one move to
    /// the word's end, and the two-move rewrite whose first half parks at the
    /// box's inset.
    ///
    /// On the walk it had: on both shapes, and on the key after the one move,
    /// every new-row cell's `t` (`Ribbon::field_at`) is the old row's walk
    /// continued in colour and pace ([`Cohort::d0`]), and the relaid word's
    /// first cell has exactly the `t` its column had on the old row, so no
    /// hard colour edge appears a few cells into the new row.
    #[test]
    fn a_composer_re_anchor_relays_the_moved_word_on_the_new_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let script = |rib: &mut Ribbon| {
            // "ab cd" typed at cols 70..75 on row 2; the caret stands at 75.
            let mut now = t0;
            for (i, ev) in [typed(), typed(), typed_space(), typed(), typed()]
                .iter()
                .enumerate()
            {
                now = at(t0, i as u64 * 60);
                let cx = ctx(now, &c, (2, 71 + i as u16), 0.9);
                rib.on_event(ev, now, &cx);
                rib.plan(&cx);
            }
            now
        };
        // The old row's walk continued: `c` stood at distance 3 on the old
        // row's cold walk, so the new row's column `col` is that walk
        // `col − 2` cells on.
        let walk_check = |rib: &Ribbon, last: u16, t_c_old: f32, shape: &str| {
            let ts: Vec<(u16, f32)> = (2..=last)
                .map(|col| {
                    (
                        col,
                        rib.field_at(3, col).unwrap_or_else(|| {
                            panic!("{shape}: column {col} of the new row is laid")
                        }),
                    )
                })
                .collect();
            for &(col, t) in &ts {
                let want = walk_t(3.0 + f32::from(col - 2));
                assert!(
                    (t - want).abs() < 1e-6,
                    "{shape}: column {col} is {t}, the old row's walk continued is {want}: {ts:?}"
                );
            }
            assert!(
                (ts[0].1 - t_c_old).abs() < 1e-5,
                "{shape}: the relaid word's first cell has the t its column had on the old row: \
                 {} against {t_c_old}: {ts:?}",
                ts[0].1
            );
        };
        // One move: 'e' wraps "cd" down; "cde" lands at 2..5, caret 5.
        let mut rib = Ribbon::new();
        let last = script(&mut rib);
        let t_c_old = rib.field_at(2, 73).expect("`c` on the old row");
        assert!((t_c_old - 3.0 / 16.0).abs() < 1e-5, "the old row's walk");
        let k = at(last, 60);
        let cx = ctx(k, &c, (3, 5), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.on_event(&typed_move((2, 75), (3, 5)), k, &cx);
        rib.plan(&cx);
        for col in 2..5u16 {
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == 3 && l.col == col && l.typing),
                "one move: the moved word's cell {col} is lit on the new row"
            );
        }
        assert!(
            !rib.cells().iter().any(|l| l.row == 3 && l.col < 2),
            "one move: nothing left of the inset"
        );
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the old row");
        // The old row is not abandoned by its own wrap; what leaves it is
        // exactly the cells the wrap moved (the Space at 72 and `cd` at
        // 73..74), which are relaid on the new row.
        assert!(!old.abandoned, "the wrap abandoned the row it came off");
        for col in 72..75u16 {
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == 2 && l.col == col && l.retract_at.is_some()),
                "the moved word's cell {col} did not leave the old row with its glyph"
            );
        }
        assert!(
            rib.cells()
                .iter()
                .filter(|l| l.row == 2 && l.col < 72)
                .all(|l| l.retract_at.is_none()),
            "the wrap took light the app did not move"
        );
        walk_check(&rib, 4, t_c_old, "one move");
        // …and the next key continues the same walk.
        let k2 = at(k, 60);
        let cx = ctx(k2, &c, (3, 6), 0.9);
        rib.on_event(&typed(), k2, &cx);
        rib.plan(&cx);
        walk_check(&rib, 5, t_c_old, "one move, next key");
        assert_eq!(rib.cohorts().iter().filter(|k| k.row == 3).count(), 1);
        // Two moves: the caret parks at the inset first; the seam refuses
        // the second half; the next key's licensed echo settles it.
        let mut rib = Ribbon::new();
        let last = script(&mut rib);
        let k = at(last, 60);
        let cx = ctx(k, &c, (3, 2), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.on_event(&typed_move((2, 75), (3, 2)), k, &cx);
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 3),
            "the parked caret lays nothing on the new row (the marker's cell stays dark): {:?}",
            rib.cells()
                .iter()
                .filter(|l| l.row == 3)
                .map(|l| l.col)
                .collect::<Vec<_>>()
        );
        let k2 = at(last, 130);
        let cx = ctx(k2, &c, (3, 6), 0.9);
        rib.on_event(&typed(), k2, &cx);
        rib.on_event(&typed_move((3, 5), (3, 6)), k2, &cx);
        rib.plan(&cx);
        // The flow's kept end: on this frame the old row's flow freezes its
        // fold edge while the moved word's old cells (72..74) are still
        // leaving in the same cohort, and the edge is `b` at 71, the row's
        // rightmost kept cell, not 74; the drift is priced against the two
        // kept cells ([`Ribbon::flow_target`]). Without `!c.leaving()` in its
        // filter the edge would be 74 with a 2.0-cell drift.
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.col0 == 70)
            .copied()
            .expect("the old row");
        assert!(old.flow.is_some(), "two moves: the old row flows");
        assert!(
            rib.cells()
                .iter()
                .any(|l| l.row == 2 && l.cohort == old.id && l.col == 74 && l.leaving()),
            "the fixture's point: the moved word's old cells are still in the flowing cohort"
        );
        assert_eq!(
            old.retract_col,
            Some(71),
            "two moves: the old row flows into its kept end, not the vacated one"
        );
        assert!(
            !old.drain_right_first,
            "a row above the caret drains left first"
        );
        assert!(
            old.flow_drift <= FLOW_DRIFT_MAX_SHARE * 2.0 + 1e-6,
            "two moves: the drift is priced on the two kept cells: {}",
            old.flow_drift
        );
        for col in 2..6u16 {
            assert!(
                rib.cells().iter().any(|l| l.row == 3 && l.col == col),
                "two moves: cell {col} is lit once the landing is known"
            );
        }
        assert!(!rib.cells().iter().any(|l| l.row == 3 && l.col < 2));
        assert_eq!(rib.cohorts().iter().filter(|k| k.row == 3).count(), 1);
        walk_check(&rib, 5, t_c_old, "two moves");
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

    /// A rebirth on the row after a finished swoosh continues the walk from
    /// where the retired band ended, inside the chain window.
    #[test]
    fn a_rebirth_after_a_finished_swoosh_continues_the_walk() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 10, &c, 0.9);
        let want = rib.cohorts()[0].t_at(12);
        let last = at(t0, 9 * 60);
        let cx = ctx(at(last, 3000), &c, (2, 12), 0.0);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the swoosh has finished");
        let k = at(last, 3100);
        let cx = ctx(k, &c, (2, 13), 0.3);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let reborn = rib.cohorts().iter().find(|k| k.row == 2).expect("reborn");
        assert!(
            (reborn.t0 - want).abs() < 1e-5,
            "the walk restarted at {} instead of continuing at {want}",
            reborn.t0
        );
        // …and its distance: the run was a cold walk from column 2, so column
        // 12 is ten cells on.
        assert_eq!(
            reborn.d0, 10.0,
            "the rebirth continues the walk's distance, not only its colour"
        );
        // Past the chain window, red again.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 10, &c, 0.9);
        let cx = ctx(at(last, 6000), &c, (2, 12), 0.0);
        rib.plan(&cx);
        let k = at(last, 6100);
        let cx = ctx(k, &c, (2, 13), 0.3);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let cold = rib.cohorts().iter().find(|k| k.row == 2).expect("reborn");
        assert_eq!(
            (cold.t0, cold.d0),
            (0.0, 0.0),
            "a cold start: red, on the fast leg's first cell"
        );
    }

    /// The walk's steps on `row` over `cols`, read from the field: `(col,
    /// t)` for each laid column, panicking on a dark one.
    fn walk_steps(rib: &Ribbon, row: u16, cols: std::ops::RangeInclusive<u16>) -> Vec<(u16, f32)> {
        cols.map(|col| {
            let t = rib
                .field_at(row, col)
                .unwrap_or_else(|| panic!("column {col} of row {row} is laid"));
            (col, t)
        })
        .collect()
    }

    /// Assert every adjacent step of `ts` is `pace`, to 1e-4.
    fn assert_pace(ts: &[(u16, f32)], pace: f32, what: &str) {
        for w in ts.windows(2) {
            let ((c0, t0), (c1, t1)) = (w[0], w[1]);
            assert!(
                (t1 - t0 - pace).abs() < 1e-4,
                "{what}: the step from column {c0} to {c1} is {} — the walk it continues \
                 laid at {pace} a cell; the continuation restarted the walk's pace: {ts:?}",
                t1 - t0
            );
        }
    }

    /// The run both rebirth pace laws continue: 30 keys on row 2 from column 2
    /// (cells 2..=31) at 60 ms, and the walk's pace at its end, asserted to
    /// be [`WALK_LAY_RATE`] (30 cells is past the walk's `d/16` fast phase).
    fn a_run_past_the_fast_phase(rib: &mut Ribbon, t0: Instant, c: &Config) -> (Cohort, f32) {
        type_run(rib, t0, 2, 30, c, 0.9);
        let band = rib.cohorts()[0];
        let pace = band.t_at(31) - band.t_at(30);
        assert!(
            (pace - WALK_LAY_RATE).abs() < 1e-6,
            "the premise: the run lays at WALK_LAY_RATE at its end ({pace})"
        );
        (band, pace)
    }

    /// A rebirth continues the walk's pace, not only its colour: (a) the
    /// freshest cohort's walk. The engine-level sibling of the host-seam fold
    /// laws (`tests/abandoned_ribbon.rs`
    /// `a_shell_fold_continues_the_walk_at_the_pace_the_row_above_had_reached`,
    /// `tests/composer_box_growth_wrap.rs`
    /// `the_new_row_continues_the_old_rows_walk_at_the_pace_it_had_reached`),
    /// on `join_cohort`'s freshest-cohort arm on the same row inside
    /// [`CHAIN_GAP_MAX`]: [`a_run_past_the_fast_phase`], a program jump to
    /// column 40 that abandons it, and 24 keys typed there. Every step of the
    /// rebirth is the run's own [`WALK_LAY_RATE`]; a restarted distance would
    /// step `1/16` for sixteen cells.
    ///
    /// One row, one walk (§42): the run is abandoned but still lit when the
    /// first key lands, so the rebirth takes its walk by position: its first
    /// cell is `band.t_at(40)` (`walk_t(38)`, the dark gap's walk), and a
    /// corridor across the row cannot repaint one run in the other's colours
    /// (`tests/wake_and_landing_walk_origins.rs`).
    #[test]
    fn a_same_row_rebirth_after_a_program_jump_continues_the_walk_s_pace() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let last = at(t0, 29 * 60);
        let mut rib = Ribbon::new();
        let (band, pace) = a_run_past_the_fast_phase(&mut rib, t0, &c);
        let hop = at(last, 100);
        let cx = ctx(hop, &c, (2, 40), 0.9);
        rib.on_event(&pty((2, 32), (2, 40)), hop, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 24,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(last, 200), keys, &c);
        let ts = walk_steps(&rib, 2, 40..=63);
        let reborn = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.col0 == 40)
            .expect("the rebirth is a cohort of its own");
        assert!(
            (ts[0].1 - band.t_at(40)).abs() < 1e-5 && reborn.id != band.id,
            "the rebirth takes the run's walk at its own column (one row, one walk), not \
             the run's end continued one step: {} against {} (one step past the end: {}): \
             {ts:?}",
            ts[0].1,
            band.t_at(40),
            band.t_at(32)
        );
        assert_eq!(
            reborn.origin_walk(),
            band.walk_at(40),
            "the rebirth's whole origin is the run's walk at column 40, its distance included"
        );
        assert_pace(&ts, pace, "a same-row rebirth after a program jump");
    }

    /// One row, one walk, left of the run too (§42). Twenty keys on row 2 from
    /// column 30 (cells 30..=49, a cold walk), a program's jump back left to
    /// column 4 that abandons the still-lit run (a TUI moving to an earlier
    /// field), and eight keys typed there. The rebirth takes the run's walk
    /// at its own column (origin `band.walk_at(4)`, `d0 = −26`: the fast line
    /// continued through zero, as a reach cell reads it), so every cell on
    /// the row carries the one walk's number at its column, bit for bit.
    /// Continued from one past the run's end instead, column 4 would read
    /// `walk_t(20)` where the row's walk is `walk_t(−26)`.
    #[test]
    fn a_same_row_rebirth_left_of_a_live_run_takes_its_walk_by_position() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let last = at(t0, 19 * 60);
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 30, 20, &c, 0.9);
        let band = rib.cohorts()[0];
        assert_eq!(
            (band.anchor_col, band.t0, band.d0),
            (30, 0.0, 0.0),
            "the premise: a cold walk from column 30"
        );
        let hop = at(last, 100);
        let cx = ctx(hop, &c, (2, 4), 0.9);
        let back = Event::Move {
            from: (2, 50),
            to: (2, 4),
            licence: Licence::Pty,
            dir: Dir::Left,
        };
        rib.on_event(&back, hop, &cx);
        rib.plan(&cx);
        let first = Keys {
            g: geom(),
            row: 2,
            col0: 4,
            n: 1,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(last, 200), first, &c);
        assert!(
            rib.cells()
                .iter()
                .any(|k| k.cohort == band.id && k.row == 2),
            "the premise: the run is still lit when the rebirth mints"
        );
        let rest = Keys {
            col0: 5,
            n: 7,
            ..first
        };
        type_keys(&mut rib, at(last, 260), rest, &c);
        let reborn = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.col0 == 4)
            .expect("the rebirth is a cohort of its own");
        assert!(reborn.id != band.id, "the rebirth is not the run");
        assert_eq!(
            reborn.origin_walk(),
            band.walk_at(4),
            "left of the run, the rebirth takes the run's walk at its own column (one row, \
             one walk), not the run's end continued ({:?})",
            band.walk_at(band.col1)
        );
        let laid = rib
            .cells()
            .iter()
            .filter(|k| k.row == 2 && k.cohort == reborn.id)
            .count();
        assert_eq!(laid, 8, "every key laid its cell on the rebirth");
        for cell in rib.cells().iter().filter(|k| k.row == 2) {
            assert_eq!(
                cell.t.to_bits(),
                band.t_at(cell.col).to_bits(),
                "column {}: {} where the row's one walk is {}",
                cell.col,
                cell.t,
                band.t_at(cell.col)
            );
        }
    }

    /// (b) The walk a finished swoosh left (`last_walk`): the same law as
    /// [`a_same_row_rebirth_after_a_program_jump_continues_the_walk_s_pace`]
    /// on `join_cohort`'s other continuation arm. [`a_run_past_the_fast_phase`]
    /// rests past its swoosh (only `last_walk` left) and 24 keys are typed on
    /// from column 32, 3.1 s after its last key. The first reborn cell is the
    /// run's walk continued one step; every step after it is the run's
    /// [`WALK_LAY_RATE`] (`last_walk` carries the distance, so the rebirth
    /// does not re-run the `1/16` leg).
    #[test]
    fn a_rebirth_after_a_finished_swoosh_continues_the_walk_s_pace() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let last = at(t0, 29 * 60);
        let mut rib = Ribbon::new();
        let (band, pace) = a_run_past_the_fast_phase(&mut rib, t0, &c);
        let cx = ctx(at(last, 3000), &c, (2, 32), 0.0);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the swoosh has finished");
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 32,
            n: 24,
            period_ms: 60,
            disp: 0.3,
        };
        type_keys(&mut rib, at(last, 3100), keys, &c);
        let ts = walk_steps(&rib, 2, 32..=55);
        assert!(
            (ts[0].1 - band.t_at(32)).abs() < 1e-5,
            "the rebirth continues the colour one step past the run's last cell: \
             {} against {}: {ts:?}",
            ts[0].1,
            band.t_at(32)
        );
        assert_pace(&ts, pace, "a rebirth after a finished swoosh");
    }

    /// The revive lays the retired run back on its own walk (§43,
    /// [`Revive`]). [`a_run_past_the_fast_phase`] rests past its swoosh (only
    /// `last_walk` left); then the seam's re-lay of its columns `2..32` and
    /// the key's own cell at 32, in either order. Named with the run's
    /// origin, every revived column is the run's own stop bit for bit and
    /// the key the walk one step past its last cell, whichever lays first.
    /// The control is the same re-lay unnamed and laid first (a key's echo a
    /// frame late), which continues `last_walk` from the run's end anchored
    /// at its first column. The revive neither restarts the pace nor spends
    /// an armed new line's gate ([`Ribbon::fresh_line`]): the key's own glyph
    /// spends it.
    #[test]
    fn a_revive_lays_the_retired_run_back_on_its_own_walk_in_either_order() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let last = at(t0, 29 * 60);
        let now = at(last, 3000);
        let relay = Event::Sweep {
            row: 2,
            col0: 2,
            col1: 32,
        };
        let key = Event::Sweep {
            row: 2,
            col0: 32,
            col1: 33,
        };
        // `(named, key first, gate armed)`.
        for (named, key_first, gated) in [
            (true, false, false),
            (true, true, false),
            (true, false, true),
            (false, false, false),
        ] {
            let what = format!("named {named}, key first {key_first}, gate armed {gated}");
            let mut rib = Ribbon::new();
            let (band, _) = a_run_past_the_fast_phase(&mut rib, t0, &c);
            rib.plan(&ctx(now, &c, (2, 32), 0.0));
            assert!(
                rib.at_rest() && rib.cohorts().is_empty() && rib.last_walk.is_some(),
                "{what}: the premise — the swoosh has finished, only `last_walk` is left"
            );
            if gated {
                rib.arm_fresh_line(at(last, 2000), None);
            }
            let cx = ctx(now, &c, (2, 33), 0.3);
            let rv = Revive {
                row: 2,
                col0: 2,
                col1: 32,
                origin: RunOrigin::of(&band),
            };
            let relay_it = |rib: &mut Ribbon| {
                if named {
                    rib.revive_sweep(&relay, now, &cx, rv);
                } else {
                    rib.on_event(&relay, now, &cx);
                }
            };
            if key_first {
                rib.on_event(&key, now, &cx);
                relay_it(&mut rib);
            } else {
                relay_it(&mut rib);
                if gated {
                    assert!(
                        rib.fresh_line.is_some(),
                        "{what}: the revive left the new line's gate armed"
                    );
                }
                rib.on_event(&key, now, &cx);
            }
            rib.plan(&cx);
            if gated {
                assert!(
                    rib.fresh_line.is_none(),
                    "{what}: the key's own glyph spent the gate"
                );
            }
            let t_of = |col: u16| {
                rib.cells()
                    .iter()
                    .find(|k| k.row == 2 && k.col == col)
                    .map(|k| k.t)
                    .unwrap_or_else(|| panic!("{what}: column {col} is laid"))
            };
            if !named {
                assert_eq!(
                    t_of(2).to_bits(),
                    band.t_at(32).to_bits(),
                    "{what}: the control — the unnamed re-lay laid first continues the run's END"
                );
                continue;
            }
            for col in 2..=32u16 {
                assert_eq!(
                    t_of(col).to_bits(),
                    band.t_at(col).to_bits(),
                    "{what}: column {col} reads {} where the run's own walk is {}",
                    t_of(col),
                    band.t_at(col)
                );
            }
        }
    }

    /// The odometer's law, for a session whose every walk started cold or at
    /// a new line: each cohort either starts a walk (`d0 == 0`, on whatever
    /// colour it continued) or continues the same input from a cold walk, so
    /// its origin is on that walk: `t0 == walk_t(d0)` and its zero is exactly
    /// `0.0`. Every resident cell carries its cohort's `t_at` at its column.
    /// A continuation of a walk a new line started shares that line's zero
    /// instead, pinned in
    /// [`a_line_after_a_return_that_wraps_carries_its_own_distance_across_the_fold`].
    fn assert_every_cohort_on_its_walk(rib: &Ribbon, what: &str) {
        for coh in rib.cohorts() {
            assert!(
                coh.d0 == 0.0 || (coh.t0 == walk_t(coh.d0) && coh.walk_zero() == 0.0),
                "{what}: cohort {} (row {}, anchor {}, wake {}) neither starts a walk nor \
                 continues a cold one: t0 {} at d0 {} (walk_t {})",
                coh.id,
                coh.row,
                coh.anchor_col,
                coh.wake,
                coh.t0,
                coh.d0,
                walk_t(coh.d0)
            );
        }
        for cell in rib.cells() {
            let coh = rib
                .cohorts()
                .iter()
                .find(|k| k.id == cell.cohort)
                .expect("every cell has its cohort");
            assert!(
                cell.t == coh.t_at(cell.col),
                "{what}: ({}, {}) carries {} but its cohort's walk there is {}",
                cell.row,
                cell.col,
                cell.t,
                coh.t_at(cell.col)
            );
        }
    }

    /// A cold walk is exact. Forty keys from column 2 with nothing to
    /// continue: the cohort is minted at `d0 = 0`, and every cell,
    /// [`Cohort::t_at`] and [`Ribbon::walk_origin_at`] read
    /// `t0 + walk_t(col − anchor_col)` and `(col − anchor_col, t0)` across
    /// both legs of the walk (`walk_t(0.0)` is `0.0`).
    #[test]
    fn a_cold_walk_reads_bit_for_bit_what_it_read_before_the_odometer() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let band = rib.cohorts()[0];
        assert_eq!(
            (band.anchor_col, band.t0, band.d0),
            (2, 0.0, 0.0),
            "a cold start: red, at distance zero"
        );
        for col in 2..42u16 {
            let before = band.t0 + walk_t(f32::from(col) - f32::from(band.anchor_col));
            let t = rib.field_at(2, col).expect("laid");
            assert_eq!(
                (t.to_bits(), band.t_at(col).to_bits()),
                (before.to_bits(), before.to_bits()),
                "col {col}: {t} / {} against the pre-odometer {before}",
                band.t_at(col)
            );
            assert_eq!(
                rib.walk_origin_at(2, col),
                Some((f32::from(col - 2), 0.0)),
                "col {col}: the landing pair is `(col − anchor_col, t0)`, as it was"
            );
        }
        assert_every_cohort_on_its_walk(&rib, "a cold run");
    }

    /// Ten keys at the right edge of row 2 (columns 110..=119 of the
    /// 120-column fixture: a cold walk, distances 0..=9), the typed fold onto
    /// row 3, and `n` keys typed there from column 0.
    fn folded_after_ten(rib: &mut Ribbon, c: &Config, n: u16) {
        let g = geom();
        let t0 = Instant::now();
        let row = |r: u16, col0: u16, n: u16| Keys {
            g,
            row: r,
            col0,
            n,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(rib, t0, row(2, 110, 10), c);
        let fold = at(t0, 10 * 60);
        let cx = ctx(fold, c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 120), (3, 0)), fold, &cx);
        rib.plan(&cx);
        type_keys(rib, fold, row(3, 0, n), c);
    }

    /// A wrap after fewer than sixteen cells continues the fast leg for the
    /// distance it has left. A phrase starting ten cells from the pane's edge
    /// wraps while its walk is still on `d/16`: the new row starts at
    /// distance 10, so its first six cells step `1/16`, the knee falls on
    /// column 6, and `1/36` follows, exactly as the same text unwrapped.
    #[test]
    fn a_wrap_after_fewer_than_sixteen_cells_continues_the_fast_leg_for_the_distance_it_has_left() {
        let c = cfg(true, true);
        let mut rib = Ribbon::new();
        folded_after_ten(&mut rib, &c, 20);
        let above = rib.field_at(2, 119).expect("row 2's last glyph");
        assert_eq!(
            above,
            walk_t(9.0),
            "row 2 is a cold walk: distance 9 at 119"
        );
        let row3 = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 3)
            .expect("row 3's cohort");
        assert_eq!(
            (row3.anchor_col, row3.d0),
            (0, 10.0),
            "row 3 continues the walk ten cells in"
        );
        let ts = walk_steps(&rib, 3, 0..=19);
        for &(col, t) in &ts {
            let want = walk_t(10.0 + f32::from(col));
            assert!(
                (t - want).abs() < 1e-6,
                "col {col} of the new row is {t}, the walk unwrapped is {want}: {ts:?}"
            );
        }
        let step = |a: f32, b: f32| b - a;
        assert!(
            (step(above, ts[0].1) - 1.0 / WALK_FAST_CELLS).abs() < 1e-6,
            "the fold is one fast-leg step"
        );
        assert_pace(
            &ts[..=6],
            1.0 / WALK_FAST_CELLS,
            "the fast leg's last six cells",
        );
        assert_pace(&ts[6..], WALK_LAY_RATE, "past the knee at column 6");
        assert_every_cohort_on_its_walk(&rib, "a short fold");
    }

    /// A landing on a continued band reads the band's own number, bit for bit:
    /// [`Ribbon::walk_origin_at`]'s pair on a band that continues a walk (the
    /// short fold above, whose knee sits six cells into the row) is
    /// `(d, 0.0)`, and `meteor::LandingWalk::at` over it is `tri` of the
    /// band's own field at every column around the landing, on both legs and
    /// across the knee.
    #[test]
    fn a_landing_walk_on_a_continued_band_is_the_band_s_field_bit_for_bit() {
        let c = cfg(true, true);
        let mut rib = Ribbon::new();
        folded_after_ten(&mut rib, &c, 20);
        for landing in [2u16, 5, 6, 7, 12] {
            let pair = rib.walk_origin_at(3, landing).expect("the landing is laid");
            assert_eq!(
                pair,
                (10.0 + f32::from(landing), 0.0),
                "landing {landing}: the walk's distance there, from its zero"
            );
            let lw = super::super::meteor::LandingWalk {
                t_land: rib.field_at(3, landing).expect("laid"),
                band: Some(pair),
            };
            for k in -3..=3_i32 {
                let Some(col) = landing.checked_add_signed(k as i16) else {
                    continue;
                };
                let Some(t) = rib.field_at(3, col) else {
                    continue;
                };
                assert_eq!(
                    lw.at(k as f32).to_bits(),
                    tri(t).to_bits(),
                    "landing {landing}, {k:+}: the landing walks {} where the band is {}",
                    lw.at(k as f32),
                    tri(t)
                );
            }
        }
    }

    /// How a jump's two landing readers pick their cells on the jump's tick.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Readers {
        /// A scrub over lit band lays no corridor: both take the band's cell.
        OneCell,
        /// The corridor's landing cell is `Over` and still unborn, so the field
        /// index takes the band's `Base` cell beneath it while
        /// [`Ribbon::walk_origin_at`] takes the corridor's: two cells of two
        /// cohorts.
        Split,
    }

    /// The cell one landing reader takes: `(cohort, layer, born)`, `None`
    /// where nothing is laid.
    type ReaderPick = Option<(u32, Layer, bool)>;

    /// The [`ReaderPick`] of each landing reader at `(row, col)` at `now`: the
    /// field index's owner ([`Ribbon::build`]'s rank: a born `Over`, then the
    /// newest `Base`, an unborn `Over` last) and [`Ribbon::walk_origin_at`]'s
    /// (the newest in pool order).
    fn landing_readers(rib: &Ribbon, row: u16, col: u16, now: Instant) -> (ReaderPick, ReaderPick) {
        let describe = |c: &Cell| (c.cohort, c.layer, now >= c.born);
        let rank = |c: &Cell| {
            if c.layer == Layer::Over {
                u8::from(now < c.born) * 2
            } else {
                1
            }
        };
        let field = rib
            .cells()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.row == row && c.col == col)
            .min_by(|&(i, a), &(j, b)| rank(a).cmp(&rank(b)).then(j.cmp(&i)))
            .map(|(_, c)| describe(c));
        let walk = rib
            .cells()
            .iter()
            .rfind(|c| c.row == row && c.col == col)
            .map(describe);
        (field, walk)
    }

    /// At every laid column of `row`, the walk [`Ribbon::walk_origin_at`]
    /// answers (`t_zero + walk_t(d)`, what a landing latches for its burst,
    /// jets and fan) is the field there (what it latches for its pin and
    /// train), bit for bit.
    fn assert_the_walk_is_the_field_on_every_column(rib: &Ribbon, row: u16, what: &str) {
        let cols = u16::try_from(geom().cols).expect("the fixture's width");
        let mut read = 0usize;
        for col in 0..cols {
            let Some(t) = rib.field_at(row, col) else {
                continue;
            };
            let (d, zero) = rib
                .walk_origin_at(row, col)
                .unwrap_or_else(|| panic!("{what}: column {col} is laid, so it has a walk"));
            assert_eq!(
                (zero + walk_t(d)).to_bits(),
                t.to_bits(),
                "{what}: column {col}: the landing's walk reads {} (d {d}, zero {zero}) where \
                 the field reads {t}",
                zero + walk_t(d)
            );
            read += 1;
        }
        assert!(read > 0, "{what}: the row is lit");
    }

    /// A keyed jump along `row` from `from` to `to` at `now`, its landing read
    /// as `Engine::tick` reads it on the jump's own tick (`caret_t =
    /// field_at(to)`, `caret_walk = walk_origin_at(to)`, the pair
    /// `meteor::LandingWalk` latches), then again once the flight is over:
    /// the pin and train (`tri(t_land)`) and the burst, jets and fan
    /// (`LandingWalk::at(dx)`) against the band drawn under them, `−3..=3`
    /// cells, bit for bit (as `tests/wake_and_landing_walk_origins.rs` reads
    /// it), and on both reads the walk against the field at every laid column
    /// of the row.
    fn jump_and_read_the_landing(
        rib: &mut Ribbon,
        c: &Config,
        now: Instant,
        (row, from, to): (u16, u16, u16),
        readers: Readers,
        what: &str,
    ) {
        let jump = Event::Move {
            from: (row, from),
            to: (row, to),
            licence: Licence::Nav,
            dir: if to < from { Dir::Left } else { Dir::Right },
        };
        let cx = ctx(now, c, (row, to), 0.9);
        rib.on_event(&jump, now, &cx);
        rib.plan(&cx);
        let (field, walk) = landing_readers(rib, row, to, now);
        match readers {
            Readers::OneCell => assert!(
                field.is_some() && field == walk,
                "{what}: the premise — a scrub, both readers take one cell: {field:?} / {walk:?}"
            ),
            Readers::Split => assert!(
                matches!(
                    (field, walk),
                    (Some((f, Layer::Base, true)), Some((w, Layer::Over, false))) if f != w
                ),
                "{what}: the premise — on the jump's tick the field takes the band's Base cell \
                 and walk_origin_at the corridor's unborn Over cell: {field:?} / {walk:?}"
            ),
        }
        let lw = super::super::meteor::LandingWalk {
            t_land: rib.field_at(row, to).expect("the landing is on laid band"),
            band: rib.walk_origin_at(row, to),
        };
        assert_the_walk_is_the_field_on_every_column(
            rib,
            row,
            &format!("{what}, on the jump's tick"),
        );
        let arrival = at(now, 150);
        rib.plan(&ctx(arrival, c, (row, to), 0.9));
        assert!(
            rib.cells()
                .iter()
                .all(|k| k.row != row || k.layer != Layer::Over || k.born <= arrival),
            "{what}: the flight is over — every corridor cell is born"
        );
        let here = rib
            .field_at(row, to)
            .unwrap_or_else(|| panic!("{what}: the landing is lit at arrival"));
        assert_eq!(
            tri(lw.t_land).to_bits(),
            tri(here).to_bits(),
            "{what}: the pin and the train land at {} on a band drawn at {}",
            tri(lw.t_land),
            tri(here)
        );
        for dx in -3..=3_i16 {
            let Some(col) = to.checked_add_signed(dx) else {
                continue;
            };
            let Some(t) = rib.field_at(row, col) else {
                continue;
            };
            assert_eq!(
                lw.at(f32::from(dx)).to_bits(),
                tri(t).to_bits(),
                "{what}, {dx:+}: the burst wears {} on a band drawn at {}",
                lw.at(f32::from(dx)),
                tri(t)
            );
        }
        assert_the_walk_is_the_field_on_every_column(rib, row, &format!("{what}, at arrival"));
        assert_every_cohort_on_its_walk(rib, what);
    }

    /// Every landing wears the band's number, whichever cell each reader
    /// takes: (a) Ctrl-A across a yanked word (§45). A jump's landing is read
    /// twice on its own tick: the field index ([`Ribbon::build`]'s owner)
    /// gives the meteor's pin and train their `t_land`, and
    /// [`Ribbon::walk_origin_at`] (the newest pool cell's cohort's walk)
    /// gives the burst, jets and fan theirs (`meteor::LandingWalk`). On a
    /// jump's tick the two take different cells (the corridor's landing cell
    /// is an unborn `Over`), which is harmless exactly when every cell stands
    /// on its cohort's walk and the row holds one walk. These three tests pin
    /// those two conditions at the ribbon, on the shapes that break them,
    /// each read on the jump's tick and at arrival
    /// ([`jump_and_read_the_landing`]). Every walk here is cold, so "bit for
    /// bit" is exact ([`Cohort::walk_zero`]).
    ///
    /// This one: `cp ` typed (columns 2..=4), a yanked word the program
    /// writes (5..=21), ` /tmp` typed (22..=26), then Ctrl-A across the line.
    /// Without §42's position arm in `join_cohort`, ` /tmp` continues `cp `'s
    /// end, and on the jump's tick column 2's walk reads `−1.0625` where the
    /// field reads `0`.
    #[test]
    fn a_landing_across_a_yanked_word_wears_the_band_s_number_whichever_cell_each_reader_takes() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 3, &c, 0.9);
        let yank = at(t0, 2 * 60 + 200);
        let cx = ctx(yank, &c, (2, 22), 0.9);
        rib.on_event(&pty((2, 5), (2, 22)), yank, &cx);
        rib.plan(&cx);
        let tmp = Keys {
            g: geom(),
            row: 2,
            col0: 22,
            n: 5,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(yank, 100), tmp, &c);
        jump_and_read_the_landing(
            &mut rib,
            &c,
            at(yank, 100 + 4 * 60 + 200),
            (2, 27, 2),
            Readers::Split,
            "Ctrl-A across a yanked word",
        );
    }

    /// Thirty keys on row 2 (columns 2..=31, a cold walk), a scrub back to
    /// column 12, and `k` keys typed there, each pushing the tail one column
    /// on ([`Ribbon::shift_run`], before the key's replay, as the shift pass
    /// runs it); returns the ribbon and the last key's instant.
    fn a_line_with_an_insert(c: &Config, t0: Instant, k: u16) -> (Ribbon, Instant) {
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 30, c, 0.9);
        let band = rib.cohorts()[0];
        let back = at(t0, 29 * 60 + 150);
        let cx = ctx(back, c, (2, 12), 0.9);
        let scrub = Event::Move {
            from: (2, 32),
            to: (2, 12),
            licence: Licence::Nav,
            dir: Dir::Left,
        };
        rib.on_event(&scrub, back, &cx);
        rib.plan(&cx);
        let mut now = back;
        for i in 0..k {
            now = at(back, 300 + u64::from(i) * 83);
            let push = super::super::witness::ShiftRun {
                row: 2,
                cohort: band.id,
                lo: 12 + i,
                hi: 31 + i,
                dc: 1,
            };
            let mut moved = Vec::new();
            assert_eq!(
                rib.shift_run(push, &mut moved),
                20,
                "an insert of {k}, key {i}: the tail is pushed on"
            );
            let cx = ctx(now, c, (2, 13 + i), 0.9);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
        }
        (rib, now)
    }

    /// (b) ⌥F onto a pushed tail: the landing law of
    /// [`a_landing_across_a_yanked_word_wears_the_band_s_number_whichever_cell_each_reader_takes`]
    /// after [`a_line_with_an_insert`] of 1, 4 and 10 keys, then ⌥F from the
    /// insert point nine columns on: a scrub, so both readers take one cell,
    /// and the walk its cohort answers is the stop the cell shows only while
    /// pushed cells stand on their cohort's walk (§41). With pushed cells
    /// keeping their old `t`, column 13 would answer `0.6875` while showing
    /// `0.625`.
    #[test]
    fn a_scrub_s_landing_on_a_pushed_tail_wears_the_band_s_number() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        for k in [1u16, 4, 10] {
            let (mut rib, last) = a_line_with_an_insert(&c, t0, k);
            jump_and_read_the_landing(
                &mut rib,
                &c,
                at(last, 150),
                (2, 12 + k, 21 + k),
                Readers::OneCell,
                &format!("⌥F onto the tail {k} keys pushed"),
            );
        }
    }

    /// (c) A corridor over a pushed tail: the landing law of
    /// [`a_landing_across_a_yanked_word_wears_the_band_s_number_whichever_cell_each_reader_takes`]
    /// after [`a_line_with_an_insert`] of 1, 4 and 10 keys, then a jump back
    /// onto the pushed tail from dark ground eight columns past the line's
    /// end (a corridor over the tail's last cells): the readers split at the
    /// landing. With pushed cells keeping their old `t` (§41 off), column
    /// 13's walk would read `0.6875` where the field reads `0.625`.
    #[test]
    fn a_corridor_s_landing_on_a_pushed_tail_wears_the_band_s_number() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        for k in [1u16, 4, 10] {
            let (mut rib, last) = a_line_with_an_insert(&c, t0, k);
            jump_and_read_the_landing(
                &mut rib,
                &c,
                at(last, 150),
                (2, 39 + k, 18 + k),
                Readers::Split,
                &format!("a jump back onto the tail {k} keys pushed"),
            );
        }
    }

    /// A heal merge takes the relaid word's whole origin. A composer's relay
    /// mints the moved word left of the wrap key's glyph, which `join_cohort`
    /// minted a beat earlier; [`Ribbon::heal_seams`] folds the two into one
    /// run on the leftmost origin and re-stamps every cell from it. The
    /// origin is `anchor_col`, `t0` and `d0`, and the merge takes all three.
    /// Here row 2's walk ends on the knee (seventeen keys, distances
    /// 0..=16), the relaid word stands at distances 14..=16 and the glyph at
    /// 17, so the merged run straddles the knee: without the odometer the
    /// word would be re-stamped `1/36` a cell where its walk steps `1/16`.
    #[test]
    fn a_heal_merge_takes_the_relaid_word_s_whole_origin_and_keeps_the_walk_s_pace() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 17, &c, 0.9);
        let old = rib.cohorts()[0];
        // The wrap key's own glyph at (3, 5), continued from row 2's end
        // (column 19, distance 17)…
        let k = at(t0, 17 * 60);
        let cx = ctx(k, &c, (3, 6), 0.9);
        rib.on_event(&typed(), k, &cx);
        let glyph = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 3)
            .expect("the key's cohort");
        assert_eq!(
            (glyph.anchor_col, glyph.d0),
            (5, 17.0),
            "the premise: the key continued the old row's walk at its end"
        );
        // …then the move's relay of the word `row 2 [16, 19)` to `[2, 5)`, on
        // the walk it had there (distance 14).
        let r = at(k, 8);
        rib.relay_word(3, 2, 5, r, Some(old.walk_at(16)));
        let cx = ctx(r, &c, (3, 6), 0.9);
        rib.plan(&cx);
        let row3: Vec<Cohort> = rib
            .cohorts()
            .iter()
            .filter(|k| k.row == 3)
            .copied()
            .collect();
        assert_eq!(row3.len(), 1, "the heal made one run: {row3:?}");
        assert_eq!(
            (row3[0].id, row3[0].anchor_col, row3[0].t0, row3[0].d0),
            (glyph.id, 2, walk_t(14.0), 14.0),
            "the older survivor takes the relaid word's whole origin, odometer and all"
        );
        let ts = walk_steps(&rib, 3, 2..=5);
        for &(col, t) in &ts {
            let want = old.t_at(col + 14);
            assert_eq!(
                t.to_bits(),
                want.to_bits(),
                "col {col} is {t}, the old row's walk continued is {want}: {ts:?}"
            );
        }
        assert_pace(
            &ts[..=2],
            1.0 / WALK_FAST_CELLS,
            "the word, on the fast leg",
        );
        assert_pace(&ts[2..], WALK_LAY_RATE, "the glyph, past the knee");
        assert_every_cohort_on_its_walk(&rib, "after the heal");
    }

    /// The fold's last glyph seats the row below
    /// ([`Ribbon::seat_fold_successor`]), in the order the engine replays a
    /// plain terminal fold: the key printed at the pane's last column was held
    /// (xterm's pending wrap), so on the fold's tick the next key's `Typed`
    /// lays `(3, 0)` first, continuing row 2 one column short, and only then
    /// does the fold's sweep lay the held glyph at `(2, 119)`. Row 3's first
    /// cell must be that glyph's walk continued one step, its origin
    /// re-seated on the glyph's far side.
    ///
    /// Control: the glyph's sweep replayed one key later finds a successor
    /// already continuing from the true end and leaves it as it is. That
    /// control holds with or without the stale-origin guard, so the guard is
    /// pinned separately in
    /// [`a_fold_leaves_a_row_below_that_did_not_continue_it_exactly_as_it_is`].
    #[test]
    fn a_fold_s_first_cell_laid_before_the_glyph_above_it_is_re_seated_one_step_on() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 2,
            col0: 89,
            n: 30,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let above = rib.cohorts()[0];
        // The 31st key prints at 119 and parks the caret there: HELD.
        let k31 = at(t0, 30 * 60);
        let cx = ctx(k31, &c, (2, 119), 0.9);
        rib.hold_typed(&typed(), k31, &cx, Some((2, 119)));
        rib.plan(&cx);
        // The fold's tick: the 32nd key's `Typed` at the landing first…
        let k32 = at(k31, 60);
        let cx = ctx(k32, &c, (3, 1), 0.9);
        rib.on_event(&typed(), k32, &cx);
        let early = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 3)
            .expect("row 3 is minted");
        assert_eq!(
            early.origin_walk(),
            above.walk_at(119),
            "the premise: row 3 was minted one column short of the glyph to come"
        );
        // …then the fold's sweeps (the held glyph, the landing's head), then
        // the move.
        let sweep = |row: u16, col0: u16| Event::Sweep {
            row,
            col0,
            col1: col0 + 1,
        };
        rib.on_event(&sweep(2, 119), k31, &cx);
        rib.on_event(&sweep(3, 0), k32, &cx);
        rib.on_event(&typed_move((2, 119), (3, 1)), k32, &cx);
        rib.plan(&cx);
        let t_above = rib.field_at(2, 119).expect("the held glyph is laid");
        let t_first = rib.field_at(3, 0).expect("row 3's first cell");
        assert_eq!(t_above, walk_t(30.0), "the held glyph joined row 2's walk");
        assert!(
            (t_first - t_above - WALK_LAY_RATE).abs() < 1e-6,
            "the colour crosses the fold one step at a time: {t_above} above, {t_first} below"
        );
        let seated = *rib.cohorts().iter().find(|k| k.row == 3).expect("row 3");
        assert_eq!(
            (seated.id, seated.origin_walk()),
            (early.id, above.walk_at(120)),
            "the same cohort, re-seated on the glyph's far side"
        );
        assert_every_cohort_on_its_walk(&rib, "after the fold's tick");
        // Negative control.
        let k33 = at(k32, 60);
        let cx = ctx(k33, &c, (3, 2), 0.9);
        rib.on_event(&typed(), k33, &cx);
        rib.plan(&cx);
        let before: Vec<(u16, u32)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 3)
            .map(|l| (l.col, l.t.to_bits()))
            .collect();
        let k34 = at(k33, 60);
        let cx = ctx(k34, &c, (3, 2), 0.9);
        rib.on_event(&sweep(2, 119), k34, &cx);
        rib.plan(&cx);
        let after: Vec<(u16, u32)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 3)
            .map(|l| (l.col, l.t.to_bits()))
            .collect();
        assert_eq!(
            (
                before,
                rib.cohorts()
                    .iter()
                    .find(|k| k.row == 3)
                    .map(Cohort::origin_walk)
            ),
            (after, Some(above.walk_at(120))),
            "a successor that continued from the true end is left exactly as it is"
        );
        assert_every_cohort_on_its_walk(&rib, "after the glyph above was laid again");
    }

    /// The fold re-seat leaves a band that did not continue it exactly as it
    /// is. [`Ribbon::seat_fold_successor`] writes an absolute origin,
    /// `walk_at(pane_col1)`, onto a cohort on the row below anchored at the
    /// pane's first column; only the guard that the cohort's origin is the
    /// stale `walk_at(col)` of the glyph just laid keeps it off an unrelated
    /// band there. An editor's shape: a cold line typed on row 3 from column
    /// 0, the hand arrows up to row 2's last column, and a glyph lands there.
    /// Row 3's band must not move; without the guard its origin `(t 0, d 0)`
    /// would be re-seated to `(t 0.6875, d 1)` and every cell repainted.
    #[test]
    fn a_fold_leaves_a_row_below_that_did_not_continue_it_exactly_as_it_is() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g,
            row: 3,
            col0: 0,
            n: 10,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let cold = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 3)
            .expect("row 3's line");
        assert_eq!(
            (cold.anchor_col, cold.origin_walk()),
            (0, WalkPos::COLD),
            "the premise: a cold line anchored at the pane's first column"
        );
        let row3 = |rib: &Ribbon| -> Vec<(u16, u32)> {
            rib.cells()
                .iter()
                .filter(|l| l.row == 3)
                .map(|l| (l.col, l.t.to_bits()))
                .collect()
        };
        let before = row3(&rib);
        let up = at(t0, 10 * 60);
        let cx = ctx(up, &c, (2, 119), 0.9);
        rib.on_event(&nav((3, 10), (2, 119)), up, &cx);
        rib.plan(&cx);
        let lay = at(up, 60);
        let cx = ctx(lay, &c, (2, 119), 0.9);
        let sweep = Event::Sweep {
            row: 2,
            col0: 119,
            col1: 120,
        };
        rib.on_event(&sweep, lay, &cx);
        rib.plan(&cx);
        // The premise: a glyph laid at the pane's last column on a walk that
        // is not the one row 3 started from (the seat's trigger), with a
        // cohort below it at the pane's first column.
        let above = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the glyph at the pane's last column is laid");
        assert!(rib.field_at(2, 119).is_some(), "the glyph is lit");
        assert_ne!(
            above.walk_at(119),
            cold.origin_walk(),
            "the premise: row 3 did not continue the glyph"
        );
        let after = rib
            .cohorts()
            .iter()
            .find(|k| k.id == cold.id)
            .map(Cohort::origin_walk);
        assert_eq!(
            (row3(&rib), after),
            (before, Some(WalkPos::COLD)),
            "a band on the row below that did not continue the glyph keeps its origin and \
             every colour on the glass"
        );
        assert_every_cohort_on_its_walk(&rib, "after the glyph at the pane's last column");
    }

    /// The walk's distance travels along one input and restarts at a new
    /// line, over a mixed session: a cold run, a program jump and its same-row
    /// rebirth (the same input, carrying the distance), a keyed jump back
    /// across the band (a wake sharing the band's origin, odometer and all),
    /// a jump onto a dark row and a hop along it (a wake continuing the
    /// freshest walk's colour, anchored at its landing at distance zero),
    /// and a fold. After every step every cohort either starts a walk
    /// (`d0 == 0`) or continues a cold one (`t0 == walk_t(d0)`), and every
    /// cell carries its cohort's `t_at`.
    ///
    /// One row, one walk (§42): the run is still lit when the rebirth mints,
    /// so the rebirth takes the run's walk by position, `d0 = 38` at column
    /// 40 on the run's zero, and the jump back's wake shares that origin.
    #[test]
    fn the_walk_s_distance_travels_along_one_input_and_restarts_at_a_new_line() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 30, &c, 0.9);
        assert_every_cohort_on_its_walk(&rib, "the cold run");
        let band = rib.cohorts()[0];
        let last = at(t0, 29 * 60);
        // A program's jump and the hand typing on from there: the same input,
        // so the rebirth carries the run's own distance at column 40, 38 (one
        // row, one walk).
        let hop = at(last, 100);
        let cx = ctx(hop, &c, (2, 40), 0.9);
        rib.on_event(&pty((2, 32), (2, 40)), hop, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 10,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(last, 200), keys, &c);
        let reborn = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.anchor_col == 40)
            .expect("the rebirth");
        assert_eq!(
            reborn.d0, 38.0,
            "a program's jump is not a new line: the rebirth carries the distance — the \
             run's own at column 40 (one row, one walk)"
        );
        assert_eq!(
            (reborn.origin_walk(), reborn.walk_zero()),
            (band.walk_at(40), band.walk_zero()),
            "the rebirth is on the run's walk: its origin is the run's walk at its column, \
             and the two share one zero"
        );
        assert_every_cohort_on_its_walk(&rib, "the rebirth");
        // A keyed jump back across the band: a wake on the row's freshest
        // origin, the rebirth's, thirty-eight cells in. The wake's whole
        // origin is pinned here, odometer included, since
        // `assert_every_cohort_on_its_walk` alone accepts `d0 == 0`.
        let j = at(last, 900);
        let cx = ctx(j, &c, (2, 10), 0.9);
        rib.on_event(&nav((2, 50), (2, 10)), j, &cx);
        rib.plan(&cx);
        let back = *rib
            .cohorts()
            .iter()
            .filter(|k| k.row == 2 && k.wake)
            .max_by_key(|k| k.id)
            .expect("the jump back laid a wake");
        assert_eq!(
            (back.anchor_col, back.origin_walk()),
            (reborn.anchor_col, reborn.origin_walk()),
            "the jump back's wake shares the rebirth's origin, its distance included"
        );
        assert_every_cohort_on_its_walk(&rib, "the jump back");
        // A jump onto a dark row, and a hop along it.
        let d = at(j, 200);
        let cx = ctx(d, &c, (6, 10), 0.9);
        rib.on_event(&nav((2, 10), (6, 10)), d, &cx);
        rib.plan(&cx);
        let freshest = rib
            .cohorts()
            .iter()
            .filter(|k| k.row != 6)
            .max_by_key(|k| k.alive_at)
            .map(|k| k.walk_at(k.col1))
            .expect("a fresh walk to continue");
        let h = at(d, 100);
        let cx = ctx(h, &c, (6, 16), 0.9);
        rib.on_event(&nav((6, 10), (6, 16)), h, &cx);
        rib.plan(&cx);
        let wake = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 6 && k.wake)
            .expect("the hop laid a wake on the dark row");
        assert_eq!(
            (wake.anchor_col, wake.origin_walk()),
            (16, freshest.restarted()),
            "the dark row's wake continues the freshest walk's colour at distance zero"
        );
        assert_every_cohort_on_its_walk(&rib, "the dark row's wake");
        // And a fold.
        let mut rib = Ribbon::new();
        folded_after_ten(&mut rib, &c, 20);
        assert_every_cohort_on_its_walk(&rib, "the fold");
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

    /// The new line's law, read off `rib`: the cohort anchored at `(row,
    /// col0)` continues the colour `t_next` and starts its walk (`d0 = 0`),
    /// and its `n` cells are exactly `t_next + walk_t(col − col0)`: `1/16` a
    /// cell for the fast leg's sixteen, then `1/36`.
    fn assert_a_new_line(rib: &Ribbon, (row, col0): (u16, u16), n: u16, t_next: f32, what: &str) {
        let line = *rib
            .cohorts()
            .iter()
            .filter(|k| k.row == row && k.anchor_col == col0 && !k.wake)
            .max_by_key(|k| k.born)
            .unwrap_or_else(|| panic!("{what}: the new line is a cohort of its own"));
        assert_eq!(
            (line.t0.to_bits(), line.d0),
            (t_next.to_bits(), 0.0),
            "{what}: the colour continues ({} against {t_next}) and the walk starts again \
             (d0 {})",
            line.t0,
            line.d0
        );
        let ts = walk_steps(rib, row, col0..=col0 + n - 1);
        for &(col, t) in &ts {
            let head = t_next + walk_t(f32::from(col - col0));
            assert_eq!(
                t.to_bits(),
                head.to_bits(),
                "{what}: col {col} is {t}, the mint before the odometer laid {head}: {ts:?}"
            );
        }
        assert_pace(&ts[..=16], 1.0 / WALK_FAST_CELLS, what);
        assert_pace(&ts[16..], WALK_LAY_RATE, what);
    }

    /// A new line continues the colour on the fast leg
    /// ([`Ribbon::fresh_line`]). Thirty keys on row 2, a gesture that starts
    /// a new line, and twenty keys after it: the line starts on the colour
    /// the line above reached, one step past its last glyph, and walks the
    /// cold fast leg from there (`1/16` a cell for sixteen cells, then
    /// `1/36`); its first key spends the gate. A wrap carries the pace; a
    /// line the hand starts shows its first word's stops. The gestures:
    ///
    /// * a Return: the key, then its move to column 0 of row 3;
    /// * a composer's newline, which sends no Return: Shift+Enter's move to
    ///   the box's inset `(3, 2)` under `Licence::Return` (a move-only
    ///   fallback without the key-time [`Event::ComposerNewline`], in a
    ///   top-anchored box; the bottom-anchored same-row shape is
    ///   [`a_new_line_after_a_same_row_composer_newline_continues_the_colour_on_the_fast_leg`]);
    /// * an arrow to `(4, 10)`: a keyed `Nav` whose rows differ.
    ///
    /// Control: the arrow's move under a program's licence ([`Licence::Pty`],
    /// a TUI parking the caret) is not the hand leaving the row, and the
    /// line typed there carries the walk's distance (`d0 = 30`): the gate
    /// reads the licence, not the geometry. Without the gate every line
    /// would carry `d0 = 30`; without the `licence == Licence::Return` clause
    /// the composer's newline fails.
    #[test]
    fn a_new_line_after_a_return_a_newline_or_an_arrow_continues_the_colour_on_the_fast_leg() {
        let c = cfg(true, true);
        let ret = Event::Move {
            from: (2, 32),
            to: (3, 0),
            licence: Licence::Return,
            dir: Dir::Left,
        };
        // `(what, the Return key first, the move, where the keys start, a
        // new line)`.
        for (what, return_key, moved, landing, new_line) in [
            ("after a Return", true, ret, (3, 0), true),
            (
                "after a composer's newline",
                false,
                row_change((2, 32), (3, 2), Licence::Return),
                (3, 2),
                true,
            ),
            (
                "after an arrow to another row",
                false,
                nav((2, 32), (4, 10)),
                (4, 10),
                true,
            ),
            (
                "after a program's move to another row",
                false,
                pty((2, 32), (4, 10)),
                (4, 10),
                false,
            ),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let t_next = a_line_of_thirty(&mut rib, t0, &c);
            let mut gesture = at(t0, 29 * 60 + 100);
            if return_key {
                rib.on_event(&Event::Return, gesture, &ctx(gesture, &c, (2, 32), 0.9));
                gesture = at(gesture, 20);
            }
            let cx = ctx(gesture, &c, landing, 0.9);
            rib.on_event(&moved, gesture, &cx);
            rib.plan(&cx);
            let keys = Keys {
                g: geom(),
                row: landing.0,
                col0: landing.1,
                n: 20,
                period_ms: 60,
                disp: 0.9,
            };
            type_keys(&mut rib, at(gesture, 100), keys, &c);
            if new_line {
                assert_a_new_line(&rib, landing, 20, t_next, what);
                assert!(
                    rib.fresh_line.is_none(),
                    "{what}: the new line's first key spent the gate"
                );
                continue;
            }
            let line = *rib
                .cohorts()
                .iter()
                .find(|k| k.row == 4 && k.anchor_col == 10)
                .expect("the line typed after the program's move");
            assert_eq!(
                (line.t0.to_bits(), line.d0),
                (t_next.to_bits(), 30.0),
                "a program's move to another row is not a new line: the walk carries on"
            );
            assert_pace(&walk_steps(&rib, 4, 10..=29), WALK_LAY_RATE, what);
        }
    }

    /// The old line's late glyph does not spend the new line's gate
    /// ([`Ribbon::fresh_line`] is an instant). A composer's submit, `…x⏎` in
    /// a one-row input box: the `x` is pressed at column 32 and held (no echo
    /// yet, [`Ribbon::hold_typed`]) and the Return 30 ms later. On the next
    /// tick the `x`'s glyph arrives by its sweep on its own key's clock, then
    /// its echo, then the box's repaint homes the caret to the input's first
    /// column on the same row (a Return-licensed move that re-arms nothing).
    /// The `x` is the old line's text and joins its walk; the next prompt
    /// typed from column 2 is a new line. With a plain flag (any typed cell
    /// spending the gate) the next prompt would carry `d0 = 31`.
    #[test]
    fn a_key_pressed_before_the_return_whose_glyph_lands_after_it_does_not_spend_the_gate() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        a_line_of_thirty(&mut rib, t0, &c);
        let kx = at(t0, 30 * 60);
        let cx = ctx(kx, &c, (2, 32), 0.9);
        rib.hold_typed(&typed(), kx, &cx, Some((2, 32)));
        rib.plan(&cx);
        let enter = at(kx, 30);
        rib.on_event(&Event::Return, enter, &ctx(enter, &c, (2, 32), 0.9));
        // The next tick: the `x`'s echo (its sweep on the key's clock and
        // its move), then the box's repaint homing the caret.
        let echo = at(kx, 50);
        let cx = ctx(echo, &c, (2, 33), 0.9);
        let sweep = Event::Sweep {
            row: 2,
            col0: 32,
            col1: 33,
        };
        rib.on_event(&sweep, kx, &cx);
        rib.on_event(&typed_move((2, 32), (2, 33)), echo, &cx);
        let home = at(echo, 20);
        let cx = ctx(home, &c, (2, 2), 0.9);
        let submit = Event::Move {
            from: (2, 33),
            to: (2, 2),
            licence: Licence::Return,
            dir: Dir::Left,
        };
        rib.on_event(&submit, home, &cx);
        rib.plan(&cx);
        let band = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2 && k.anchor_col == 2)
            .expect("row 2's line");
        assert_eq!(
            (rib.field_at(2, 32).map(f32::to_bits), band.col1),
            (Some(walk_t(30.0).to_bits()), 33),
            "the premise: the late glyph joined the old line's walk where it stands"
        );
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(home, 100), keys, &c);
        assert_a_new_line(
            &rib,
            (2, 2),
            20,
            band.t_at(33),
            "the next prompt, after a glyph of the old line landed late",
        );
    }

    /// …and after a line kill. Thirty keys on row 2, a `^U` from their end
    /// back to column 2 (a `Kill` of scope `Line`), the drain left to finish
    /// (only [`LastWalk`] remains), and twenty keys retyped from column 2
    /// inside [`CHAIN_GAP_MAX`]: a new line on the same row, continuing the
    /// killed walk's colour on the fast leg rather than its distance.
    #[test]
    fn a_new_line_after_a_line_kill_continues_the_colour_on_the_fast_leg() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let kill = at(t0, 29 * 60 + 100);
        let cx = ctx(kill, &c, (2, 2), 0.9);
        rib.on_event(
            &Event::Kill {
                cells: 30,
                scope: super::super::KillScope::Line,
            },
            kill,
            &cx,
        );
        rib.plan(&cx);
        let drained = at(kill, 1500);
        rib.plan(&ctx(drained, &c, (2, 2), 0.0));
        assert!(
            rib.cohorts().is_empty() && rib.last_walk.is_some(),
            "the premise: the killed line has drained and retired, leaving its walk: {:?}",
            rib.cohorts()
        );
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(drained, 100), keys, &c);
        assert_a_new_line(&rib, (2, 2), 20, t_next, "after a line kill");
    }

    /// A new line that wraps carries its own distance across the fold: the two
    /// laws together. Thirty keys on row 2, a Return to row 3, the shell's
    /// 110-column prompt (a program's move to column 110, which is not a new
    /// line of its own; the gate waits for the first key), ten keys to the
    /// pane's edge and the fold onto row 4, and twenty keys there. Row 3 is a
    /// new line (colour continued, walk restarted at column 110) and row 4
    /// continues that line: ten cells into its walk, so its first six cells
    /// finish the fast leg and the knee falls on column 6, as the same text
    /// unwrapped has it.
    #[test]
    fn a_line_after_a_return_that_wraps_carries_its_own_distance_across_the_fold() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let enter = at(t0, 29 * 60 + 100);
        let cx = ctx(enter, &c, (2, 32), 0.9);
        rib.on_event(&Event::Return, enter, &cx);
        let down = at(enter, 20);
        let cx = ctx(down, &c, (3, 0), 0.9);
        let ret = Event::Move {
            from: (2, 32),
            to: (3, 0),
            licence: Licence::Return,
            dir: Dir::Left,
        };
        rib.on_event(&ret, down, &cx);
        rib.plan(&cx);
        let prompt = at(down, 30);
        let cx = ctx(prompt, &c, (3, 110), 0.9);
        rib.on_event(&pty((3, 0), (3, 110)), prompt, &cx);
        rib.plan(&cx);
        let row = |r: u16, col0: u16, n: u16| Keys {
            g: geom(),
            row: r,
            col0,
            n,
            period_ms: 60,
            disp: 0.9,
        };
        let k0 = at(prompt, 100);
        type_keys(&mut rib, k0, row(3, 110, 10), &c);
        let fold = at(k0, 10 * 60);
        let cx = ctx(fold, &c, (4, 0), 0.9);
        rib.on_event(&typed_move((3, 120), (4, 0)), fold, &cx);
        rib.plan(&cx);
        type_keys(&mut rib, fold, row(4, 0, 20), &c);
        let line = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 3 && k.anchor_col == 110)
            .expect("row 3's new line");
        assert_eq!(
            (line.t0.to_bits(), line.d0),
            (t_next.to_bits(), 0.0),
            "row 3 is a new line: the colour continued, the walk started again"
        );
        let wrapped = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 4 && k.anchor_col == 0)
            .expect("row 4's continuation");
        assert!(
            wrapped.d0 == 10.0 && (wrapped.walk_zero() - t_next).abs() < 1e-6,
            "row 4 continues the NEW line's walk ten cells in, from the colour it started on: \
             d0 {}, zero {} against {t_next}",
            wrapped.d0,
            wrapped.walk_zero()
        );
        let above = rib.field_at(3, 119).expect("row 3's last glyph");
        let ts = walk_steps(&rib, 4, 0..=19);
        for &(col, t) in &ts {
            let want = t_next + walk_t(10.0 + f32::from(col));
            assert!(
                (t - want).abs() < 1e-5,
                "col {col} of the wrapped row is {t}, the new line unwrapped is {want}: {ts:?}"
            );
        }
        assert!(
            (ts[0].1 - above - 1.0 / WALK_FAST_CELLS).abs() < 1e-5,
            "the fold is one fast-leg step: {above} above, {} below",
            ts[0].1
        );
        assert_pace(
            &ts[..=6],
            1.0 / WALK_FAST_CELLS,
            "the new line's fast leg, finished across the fold",
        );
        assert_pace(
            &ts[6..],
            WALK_LAY_RATE,
            "past the new line's knee at column 6",
        );
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

    /// The new line's first key, pressed at `key` and held for want of an
    /// echo, arrives: its glyph at `landing` by its echo's sweep, dated at its
    /// own press (the host's `typed_at`), and the echo's one-cell move at
    /// `echo`; then nineteen more keys along the row, one per 60 ms.
    fn echo_the_held_key_then_type_on(
        rib: &mut Ribbon,
        c: &Config,
        landing: (u16, u16),
        key: Instant,
        echo: Instant,
    ) {
        let next = (landing.0, landing.1 + 1);
        let cx = ctx(echo, c, next, 0.9);
        let sweep = Event::Sweep {
            row: landing.0,
            col0: landing.1,
            col1: next.1,
        };
        rib.on_event(&sweep, key, &cx);
        rib.on_event(&typed_move(landing, next), echo, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: landing.0,
            col0: next.1,
            n: 19,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(rib, at(echo, 60), keys, c);
    }

    /// The new line's first key, pressed before its row change was seen,
    /// still starts the walk again ([`FreshLine`]). Thirty keys on row 2,
    /// then a gesture that starts a new line (a Return, a composer's newline
    /// as a move-only fallback, or an arrow to another row), and the new
    /// line's first key pressed 10 ms after the gesture, before the host
    /// observed the row change 30 ms after it. The key is held
    /// ([`Ribbon::hold_typed`]) and its glyph arrives on its own press's
    /// clock, earlier than the move's. That window is the app's repaint
    /// latency, so without this whether a line restarted would depend on
    /// typing speed. Every gesture's new line continues the colour on the
    /// cold fast leg, exactly as keys typed after the move do, and the
    /// Return's move keeps the Return key's instant.
    ///
    /// Both witnesses are needed: with only the earliest instant kept, the
    /// newline and the arrow fail (no press-dated event arms them); with only
    /// the row the hand left, the Return's instant assertion fails.
    #[test]
    fn a_new_line_s_first_key_pressed_before_its_row_change_was_seen_still_starts_the_walk_again() {
        let c = cfg(true, true);
        for (what, return_key, licence, landing) in [
            ("a Return", true, Licence::Return, (3, 0)),
            ("a composer's newline", false, Licence::Return, (3, 2)),
            ("an arrow to another row", false, Licence::Nav, (4, 10)),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let t_next = a_line_of_thirty(&mut rib, t0, &c);
            let gesture = at(t0, 29 * 60 + 100);
            if return_key {
                rib.on_event(&Event::Return, gesture, &ctx(gesture, &c, (2, 32), 0.9));
            }
            // The new line's first key: pressed, no echo yet — held.
            let key = at(gesture, 10);
            let cx = ctx(key, &c, (2, 32), 0.9);
            rib.hold_typed(&typed(), key, &cx, Some((2, 32)));
            rib.plan(&cx);
            // The row change, on the clock the host observed it.
            let seen = at(gesture, 30);
            let cx = ctx(seen, &c, landing, 0.9);
            rib.on_event(&row_change((2, 32), landing, licence), seen, &cx);
            rib.plan(&cx);
            let since = rib.fresh_line.map(|gate| gate.since);
            echo_the_held_key_then_type_on(&mut rib, &c, landing, key, at(seen, 18));
            assert_a_new_line(&rib, landing, 20, t_next, what);
            assert!(
                rib.fresh_line.is_none(),
                "{what}: the held key's glyph spent the gate"
            );
            assert_eq!(
                since,
                Some(if return_key { gesture } else { seen }),
                "{what}: the gate is dated at the earliest instant it was given"
            );
        }
    }

    /// …and when the engine replayed that key on the old row first
    /// ([`FreshLine::left_row`]). The Return of the test above with the new
    /// line's first key sharing a tick with the Return's move: the engine
    /// replays a key pressed before the frame's first one-shot move at the
    /// caret before that move (`Engine::replay_events`), so the key re-lays
    /// the old row's last glyph cell `(2, 31)` into the old line's run on a
    /// clock past the Return. That is the old line re-laid, not the new
    /// line's first cell, and leaves the gate armed ([`Ribbon::lay_cell`]).
    /// The glyph then arrives by its echo's sweep, dated at its press, and
    /// the new line restarts its walk. Two homes:
    ///
    /// * cross-row, to `(3, 0)`: two witnesses, the gate left armed at the
    ///   Return's instant and the row the hand left;
    /// * same-row, to `(2, 2)` (a plain Enter in a composer, the box's home
    ///   observed at `T + 30` in the key's frame): only the first witness.
    #[test]
    fn a_new_line_s_first_key_replayed_on_the_old_row_before_the_return_s_move_still_starts_the_walk_again()
     {
        let c = cfg(true, true);
        for (landing, what) in [
            (
                (3u16, 0u16),
                "the new line's first key, replayed on the old row first",
            ),
            (
                (2, 2),
                "the new line's first key, replayed on the old line before the same-row home",
            ),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let t_next = a_line_of_thirty(&mut rib, t0, &c);
            let enter = at(t0, 29 * 60 + 100);
            rib.on_event(&Event::Return, enter, &ctx(enter, &c, (2, 32), 0.9));
            // The tick that sees the move replays the key at the pre-move
            // caret.
            let key = at(enter, 10);
            rib.on_event(&typed(), key, &ctx(key, &c, (2, 32), 0.9));
            assert_eq!(
                rib.fresh_line.map(|gate| gate.since),
                Some(enter),
                "{what}: the premise — the phantom re-lay of the old line's last cell left the \
                 Return's gate armed"
            );
            let seen = at(enter, 30);
            let cx = ctx(seen, &c, landing, 0.9);
            rib.on_event(&row_change((2, 32), landing, Licence::Return), seen, &cx);
            rib.plan(&cx);
            echo_the_held_key_then_type_on(&mut rib, &c, landing, key, at(seen, 18));
            assert_a_new_line(&rib, landing, 20, t_next, what);
        }
    }

    /// The gate's two witnesses ([`FreshLine::takes`]): a typed cell is the
    /// new line's when its key was pressed at or after the new line started,
    /// or when it is laid on any row but the one the hand left, and not when
    /// an older key's glyph lands on the row the hand left (the old line's
    /// text, the `…x⏎` law). No host shape lays an old line's glyph on its
    /// row after a cross-row move (the seam sweeps only a same-row echo), so
    /// the predicate is pinned directly; it fails with the row clause widened
    /// to "any recorded row".
    #[test]
    fn the_new_line_s_gate_takes_a_late_key_off_the_row_the_hand_left_and_never_on_it() {
        let t0 = Instant::now();
        let since = at(t0, 30);
        let moved = FreshLine {
            since,
            chord_at: None,
            awaiting_move: false,
            left_row: Some(2),
        };
        let homed = FreshLine {
            since,
            chord_at: None,
            awaiting_move: false,
            left_row: None,
        };
        assert!(moved.takes(2, since) && moved.takes(7, since) && homed.takes(2, since));
        assert!(
            moved.takes(3, t0),
            "an earlier key laid on the new row is the new line's"
        );
        assert!(
            !moved.takes(2, t0),
            "an earlier key laid on the row the hand left is the old line's"
        );
        assert!(
            !homed.takes(3, t0),
            "with no row recorded the gate reads its instant alone"
        );
    }

    /// Tier-1 of `RainbowComposerNewlineGate`: the real ribbon follows the
    /// model's chord → pre-home type-ahead → observed home → first echo
    /// sequence. A plain modified key with no composer hint cannot borrow the
    /// gate even if a TUI later parks its caret at the same inset.
    #[test]
    fn a_composer_gate_waits_for_its_home_and_only_its_first_real_echo_spends_it() {
        let model = aterm_spec::derive::rainbow_composer_newline_gate_model();
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

    /// …and after a newline that keeps the caret on its row. A bottom-anchored
    /// composer's Shift+Enter is modelled as the box repainting one row
    /// higher, the follow pass carrying the line above and the caret homing
    /// to the inset on the same row: `(2, 32) → (2, 2)` under
    /// `Licence::Return`, with no key-time event in this fallback. The
    /// newline is a new line whatever its rows; only a Return key or a
    /// composer's newline hint produces `Licence::Return`. Thirty keys on row
    /// 2, that move, and twenty keys after it from column 2: a new line on
    /// the fast leg (an arm requiring `to.0 != from.0` would leave `d0 = 30`).
    #[test]
    fn a_new_line_after_a_same_row_composer_newline_continues_the_colour_on_the_fast_leg() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let newline = at(t0, 29 * 60 + 100);
        let cx = ctx(newline, &c, (2, 2), 0.9);
        rib.on_event(&row_change((2, 32), (2, 2), Licence::Return), newline, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(newline, 100), keys, &c);
        assert_a_new_line(
            &rib,
            (2, 2),
            20,
            t_next,
            "after a same-row composer newline",
        );
    }

    /// A completion that crosses the fold is the same line. A Tab completion
    /// (or a ⌃V gesture) whose inserted text runs over the pane's edge
    /// reaches the ribbon as one cross-row move under [`Licence::Synthetic`]
    /// (`bash` completing a long name from column 70 onto `(3, 12)`): the
    /// command line continuing. Only a hand navigation ([`Licence::Nav`]) or
    /// a Return arms the gate on a row change. Thirty keys on row 2, the
    /// completion's move, and twenty keys after it carry the walk's distance
    /// (`d0 = 30`, stepping [`WALK_LAY_RATE`]) as a plain fold does. Reading
    /// [`Licence::is_keyed_nav`] (`Nav | Synthetic`) would restart the fast
    /// leg here.
    #[test]
    fn a_completion_that_crosses_the_fold_keeps_the_walk_s_distance() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let tab = at(t0, 29 * 60 + 100);
        let cx = ctx(tab, &c, (3, 12), 0.9);
        rib.on_event(&row_change((2, 32), (3, 12), Licence::Synthetic), tab, &cx);
        rib.plan(&cx);
        assert!(
            rib.fresh_line.is_none(),
            "a completion's row change is not a new line"
        );
        let keys = Keys {
            g: geom(),
            row: 3,
            col0: 12,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(tab, 100), keys, &c);
        let line = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 3 && k.anchor_col == 12)
            .expect("the keys typed after the completion");
        assert_eq!(
            (line.t0.to_bits(), line.d0),
            (t_next.to_bits(), 30.0),
            "the completion continued the line: the walk carries its distance"
        );
        assert_pace(
            &walk_steps(&rib, 3, 12..=31),
            WALK_LAY_RATE,
            "after a completion across the fold",
        );
    }

    /// A line kill retyped at once is a new line too. The sibling of
    /// [`a_new_line_after_a_line_kill_continues_the_colour_on_the_fast_leg`],
    /// retyping 150 ms after the `^U` while every killed cell is still
    /// resident on its drain and its bounds still cover the prompt's
    /// columns. A cohort with no standing cell is not a band the key can
    /// join ([`Ribbon::join_cohort`]), so the retyped line mints, continuing
    /// the killed walk's colour at distance zero, exactly as after the drain.
    #[test]
    fn a_line_kill_retyped_at_once_continues_the_colour_on_the_fast_leg() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let kill = at(t0, 29 * 60 + 100);
        let cx = ctx(kill, &c, (2, 2), 0.9);
        rib.on_event(
            &Event::Kill {
                cells: 30,
                scope: super::super::KillScope::Line,
            },
            kill,
            &cx,
        );
        rib.plan(&cx);
        let retype = at(kill, 150);
        rib.plan(&ctx(retype, &c, (2, 2), 0.9));
        assert!(
            rib.cells().iter().filter(|l| l.row == 2).count() >= 20
                && rib.cells().iter().filter(|l| l.row == 2).all(Cell::leaving),
            "the premise: the killed line is still resident, every cell on its drain"
        );
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, retype, keys, &c);
        assert_a_new_line(&rib, (2, 2), 20, t_next, "a line kill retyped at once");
    }

    /// Row 2's standing cells, `(col, t, cohort)` by column, and every
    /// column's owner count, leaving cells included ([`laid_on_row_2`]).
    type RowTwo = (Vec<(u16, f32, u32)>, Vec<(u16, usize)>);

    /// The row-2 cells' `(col, t, cohort)`, the NON-leaving ones, by column —
    /// and the number of owners every column has, leaving ones included.
    fn laid_on_row_2(rib: &Ribbon) -> RowTwo {
        let mut live: Vec<(u16, f32, u32)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && !l.leaving())
            .map(|l| (l.col, l.t, l.cohort))
            .collect();
        live.sort_unstable_by_key(|l| l.0);
        let mut owners: Vec<(u16, usize)> = Vec::new();
        for l in rib.cells().iter().filter(|l| l.row == 2) {
            match owners.iter_mut().find(|o| o.0 == l.col) {
                Some(o) => o.1 += 1,
                None => owners.push((l.col, 1)),
            }
        }
        owners.sort_unstable();
        (live, owners)
    }

    /// The hand's own erase leaves its run a band (C2, [`Cohort::anchor_col`]:
    /// a cell backspaced and retyped takes back exactly the stop it had). A
    /// run the hand Backspaced whole is on its retract, every cell `leaving`;
    /// only a witness melt and a line kill's drain are dead
    /// ([`Ribbon::stands`]), so a retype inside the fade rejoins the run and
    /// takes back its stops, one owner a column ([`Ribbon::place`] replaces
    /// the draining one). `gti`, three Backspaces, `git` 60 ms later lays
    /// `0 / 0.0625 / 0.125`; thirty cells backspaced at 35 ms (a held key
    /// down to the prompt) and retyped at 60 ms read the same.
    ///
    /// Controls: the run erased to its first glyph rejoins on the stops (a
    /// glyph still stands); and a retype after the fade, every erased cell
    /// pruned, continues the erased end on the slow leg (`t0 = walk_t(30)`,
    /// `d0 = 30`, the finished swoosh's `last_walk`). With every `leaving`
    /// cell treated as dead, the retypes would mint on `walk_at(col1)`.
    #[test]
    fn a_run_the_hand_backspaced_whole_is_rejoined_on_its_stops_inside_its_fade() {
        let c = cfg(true, true);
        // `(cells typed, cells erased, ms between Backspaces, retype after)`.
        for (n, erased, bs_ms, pause_ms) in [
            (3u16, 3u16, 60u64, 60u64),
            (20, 20, 60, 80),
            (30, 30, 35, 60),
            (30, 29, 35, 60),
        ] {
            let what =
                format!("{n} typed, {erased} erased at {bs_ms} ms, retyped {pause_ms} ms on");
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, n, &c, 0.9);
            let run = rib.cohorts()[0].id;
            let mut ms = u64::from(n) * 60;
            for k in 0..erased {
                erase_at(&mut rib, at(t0, ms), (2, 2 + n - 1 - k), &c);
                ms += bs_ms;
            }
            let retype = at(t0, ms - bs_ms + pause_ms);
            let from = 2 + n - erased;
            if erased == n {
                assert!(
                    rib.cells().iter().filter(|l| l.row == 2).all(Cell::leaving),
                    "{what}: the premise — every resident cell is on its retract"
                );
            }
            let keys = Keys {
                g: geom(),
                row: 2,
                col0: from,
                n: erased,
                period_ms: 60,
                disp: 0.9,
            };
            type_keys(&mut rib, retype, keys, &c);
            let (live, owners) = laid_on_row_2(&rib);
            for &(col, t, cohort) in &live {
                let want = walk_t(f32::from(col - 2));
                assert!(
                    cohort == run && (t - want).abs() < 1e-5,
                    "{what}: col {col} is cohort {cohort} at t = {t}, its stop in run {run} is \
                     {want}: {live:?}"
                );
            }
            assert!(
                owners.iter().all(|&(_, k)| k == 1),
                "{what}: one owner a column: {owners:?}"
            );
        }
        // Past the fade: the erased cells are pruned and the retype
        // continues the walk from the erased end.
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 30, &c, 0.9);
        let mut ms = 30 * 60;
        for k in 0..30u16 {
            erase_at(&mut rib, at(t0, ms), (2, 31 - k), &c);
            ms += 35;
        }
        let retype = at(t0, ms - 35 + 1200);
        rib.plan(&ctx(retype, &c, (2, 2), 0.9));
        assert!(
            rib.cells().is_empty(),
            "the premise: the erased cells are pruned"
        );
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 6,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, retype, keys, &c);
        let line = rib.cohorts()[0];
        assert_eq!(
            (line.t0.to_bits(), line.d0),
            (walk_t(30.0).to_bits(), 30.0),
            "a retype past the fade continues the erased end"
        );
    }

    /// …and so does a `^W`. A word killed by `^W`
    /// ([`super::super::KillScope::Word`]) is on its farthest-first drain,
    /// every cell `leaving`; two keys typed 120 ms later, while the killed
    /// cells at columns 4..6 still show their stops, take back the word's own
    /// stops at 2 and 3, continuous with the cells still draining beside
    /// them rather than a quarter-arc step backward at the `3|4` seam.
    #[test]
    fn a_word_killed_and_retyped_inside_its_drain_takes_back_its_stops() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 5, &c, 0.9);
        let run = rib.cohorts()[0].id;
        let kill = at(t0, 5 * 60 + 50);
        let cx = ctx(kill, &c, (2, 2), 0.9);
        rib.on_event(
            &Event::Kill {
                cells: 5,
                scope: super::super::KillScope::Word,
            },
            kill,
            &cx,
        );
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 2,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(kill, 120), keys, &c);
        let (live, owners) = laid_on_row_2(&rib);
        assert_eq!(
            live.iter().map(|l| (l.0, l.2)).collect::<Vec<_>>(),
            vec![(2, run), (3, run)],
            "the retyped keys rejoin the killed word's run: {live:?}"
        );
        for &(col, t, _) in &live {
            assert!(
                (t - walk_t(f32::from(col - 2))).abs() < 1e-5,
                "col {col} takes back its stop: {live:?}"
            );
        }
        let draining: Vec<(u16, f32)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && l.leaving())
            .map(|l| (l.col, l.t))
            .collect();
        assert!(
            !draining.is_empty()
                && draining
                    .iter()
                    .all(|&(col, t)| col >= 4 && (t - walk_t(f32::from(col - 2))).abs() < 1e-5),
            "the killed cells still draining sit on the same walk past the retype: {draining:?}"
        );
        assert!(
            owners.iter().all(|&(_, k)| k == 1),
            "one owner a column: {owners:?}"
        );
    }

    /// The hand's run begins where it arrived (`tests/wrap_code_reflow.rs`, B1
    /// and B2): the hand walks into program text at column 72 of row 2 and
    /// types `ab`; the wrap key `c` moves `abc` down, the caret landing after
    /// it at (3, 5). The moved word is measured from the arrival: `ab` is
    /// relaid at 2..4 beside the wrap key's cell, nothing lands on the indent,
    /// and the old cells leave row 2 with their glyphs. Twice: with no typed
    /// Space on the row, and with a Space typed at 60 before the hand
    /// navigated.
    #[test]
    fn a_word_begun_where_the_hand_arrived_is_measured_from_the_arrival() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        for stale_space in [false, true] {
            let mut rib = Ribbon::new();
            let mut now = t0;
            if stale_space {
                // `x ` typed at 59..61: the Space's cell is 60.
                for (i, ev) in [typed(), typed_space()].iter().enumerate() {
                    now = at(t0, i as u64 * 60);
                    let cx = ctx(now, &c, (2, 60 + i as u16), 0.9);
                    rib.on_event(ev, now, &cx);
                    rib.plan(&cx);
                }
            }
            now = at(now, 300);
            let cx = ctx(now, &c, (2, 72), 0.9);
            rib.on_event(&nav((2, 61), (2, 72)), now, &cx);
            rib.plan(&cx);
            for i in 0..2u16 {
                now = at(now, 60);
                let cx = ctx(now, &c, (2, 73 + i), 0.9);
                rib.on_event(&typed(), now, &cx);
                rib.on_event(&typed_move((2, 72 + i), (2, 73 + i)), now, &cx);
                rib.plan(&cx);
            }
            let k = at(now, 60);
            let cx = ctx(k, &c, (3, 5), 0.9);
            rib.on_event(&typed(), k, &cx);
            rib.on_event(&typed_move((2, 74), (3, 5)), k, &cx);
            rib.plan(&cx);
            for col in 2..5u16 {
                assert!(
                    rib.cells()
                        .iter()
                        .any(|l| l.row == 3 && l.col == col && l.typing && !l.leaving()),
                    "stale space {stale_space}: the moved word's cell {col} is lit on the new row"
                );
            }
            assert!(
                !rib.cells().iter().any(|l| l.row == 3 && l.col < 2),
                "stale space {stale_space}: nothing on the indent"
            );
            for col in 72..74u16 {
                assert!(
                    rib.cells()
                        .iter()
                        .filter(|l| l.row == 2 && l.col == col)
                        .all(|l| l.retract_at.is_some()),
                    "stale space {stale_space}: the moved cell {col} stayed on the old row"
                );
            }
        }
    }

    /// An arrow inside the hand's own live word does not cut its measure
    /// (`tests/wrap_code_reflow.rs`, B3): `x ` typed at 59..61 (the Space at
    /// 60) and `abcd` at 61..65; Left ×2 over the lit `cd` to (2, 63); the `X`
    /// inserted there makes `abXcd` too long for the row, and it goes down
    /// whole, the caret after the `X` at (3, 5). The arrival reaches back over
    /// the live typed `ab` to the typed Space, so the moved word measures 2:
    /// `ab` relaid at 2..4 beside the key's cell, nothing on the indent, the
    /// old `ab` cells leaving row 2.
    #[test]
    fn an_arrow_inside_the_hands_live_word_does_not_cut_its_measure() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut now = t0;
        for (i, ev) in [typed(), typed_space()].iter().enumerate() {
            now = at(t0, i as u64 * 60);
            let cx = ctx(now, &c, (2, 60 + i as u16), 0.9);
            rib.on_event(ev, now, &cx);
            rib.plan(&cx);
        }
        for i in 0..4u16 {
            now = at(now, 60);
            let cx = ctx(now, &c, (2, 62 + i), 0.9);
            rib.on_event(&typed(), now, &cx);
            rib.on_event(&typed_move((2, 61 + i), (2, 62 + i)), now, &cx);
            rib.plan(&cx);
        }
        now = at(now, 120);
        let cx = ctx(now, &c, (2, 63), 0.9);
        rib.on_event(&nav((2, 65), (2, 63)), now, &cx);
        rib.plan(&cx);
        let k = at(now, 120);
        let cx = ctx(k, &c, (3, 5), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.on_event(&typed_move((2, 63), (3, 5)), k, &cx);
        rib.plan(&cx);
        for col in 2..5u16 {
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == 3 && l.col == col && l.typing && !l.leaving()),
                "the moved word's cell {col} is lit on the new row"
            );
        }
        assert!(
            !rib.cells().iter().any(|l| l.row == 3 && l.col < 2),
            "nothing on the indent"
        );
        for col in 61..63u16 {
            assert!(
                rib.cells()
                    .iter()
                    .filter(|l| l.row == 2 && l.col == col)
                    .all(|l| l.retract_at.is_some()),
                "the moved cell {col} stayed on the old row"
            );
        }
    }

    /// The lifted word goes up with its light (`tests/wrap_code_reflow.rs`,
    /// C): `abc` typed at 2..5 on row 3; the Space after it lets the word fit
    /// row 2 again, where the composer rewrites it at 70..73, the caret
    /// staying at (3, 2). With the seam's verdict ([`Lift`]) the word's cells
    /// leave row 3 and are relaid under its glyphs on row 2 on the walk they
    /// had, flowing; replayed without it (the same-row re-anchor) nothing
    /// reaches row 2 (the control).
    #[test]
    fn a_lifted_word_goes_up_with_its_light_on_the_walk_it_had() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        for verdict in [true, false] {
            let mut rib = Ribbon::new();
            let mut now = t0;
            for i in 0..3u16 {
                now = at(t0, u64::from(i) * 60);
                let cx = ctx(now, &c, (3, 3 + i), 0.9);
                rib.on_event(&typed(), now, &cx);
                rib.plan(&cx);
            }
            let walk: Vec<f32> = (2..5u16)
                .map(|col| rib.field_at(3, col).expect("the typed word's walk"))
                .collect();
            let k = at(now, 60);
            let cx = ctx(k, &c, (3, 2), 0.9);
            let lift = typed_move((3, 5), (3, 2));
            if verdict {
                rib.lift_move(
                    &lift,
                    k,
                    &cx,
                    Lift {
                        origin: (3, 5),
                        landing: (3, 2),
                        dst: (2, 70),
                    },
                );
            } else {
                rib.on_event(&lift, k, &cx);
            }
            rib.plan(&cx);
            let up: Vec<u16> = (70..73u16)
                .filter(|&col| {
                    rib.cells()
                        .iter()
                        .any(|l| l.row == 2 && l.col == col && l.typing && !l.leaving())
                })
                .collect();
            assert!(
                rib.cells().iter().filter(|l| l.row == 3).all(Cell::leaving),
                "verdict {verdict}: the word's old cells stayed live under the caret"
            );
            if !verdict {
                assert!(up.is_empty(), "the control lifted {up:?} with no verdict");
                continue;
            }
            assert_eq!(up, [70, 71, 72], "the lifted word's cells on row 2");
            for (i, t) in walk.iter().enumerate() {
                let col = 70 + i as u16;
                let now_t = rib.field_at(2, col).expect("the relaid walk");
                assert!(
                    (now_t - t).abs() < 1e-4,
                    "cell {col} took t {now_t}, the word had {t}"
                );
            }
            assert!(
                rib.cohorts()
                    .iter()
                    .filter(|k| k.row == 2)
                    .all(|k| k.flow.is_some()),
                "the row the word went up to flows into the fold"
            );
        }
    }

    /// A relaid word is one run ([`Ribbon::relay_word`]). A composer's relay
    /// settles on a row where the old line's run still stands past the word:
    /// thirty keys on row 2, the cells at columns 2 and 3 retired, and the
    /// moved word relaid at `2..4` on the walk it had (`t = 3`, ninety cells
    /// in). Its second letter joins the run its first letter minted, one
    /// step on, never the old run whose bounds still span the column (which
    /// [`Ribbon::join_cohort`] would find first by pool order).
    #[test]
    fn a_relaid_word_s_letters_join_the_run_its_first_letter_minted() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 30, &c, 0.9);
        let old = rib.cohorts()[0];
        let now = at(t0, 30 * 60 + 50);
        let under: Vec<(u16, u16, Instant)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && (l.col == 2 || l.col == 3))
            .map(|l| (l.row, l.col, l.born))
            .collect();
        assert_eq!(
            rib.retire_cells(&under, now),
            2,
            "the premise: two cells melt"
        );
        let walk = WalkPos { t: 3.0, d: 90.0 };
        rib.relay_word(2, 2, 4, now, Some(walk));
        let word: Vec<(u16, u32, f32)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && (l.col == 2 || l.col == 3) && !l.leaving())
            .map(|l| (l.col, l.cohort, l.t))
            .collect();
        assert_eq!(word.len(), 2, "the word is relaid: {word:?}");
        let (first, second) = (word[0], word[1]);
        assert!(
            first.1 != old.id && second.1 == first.1,
            "the word is one run of its own, not the old line's ({}): {word:?}",
            old.id
        );
        assert!(
            (second.2 - (walk.t + walk_t(91.0) - walk_t(90.0))).abs() < 1e-5,
            "the second letter is the word's walk one step on: {word:?}"
        );
    }

    /// …and after a curtain ([`Ribbon::curtain`]). Thirty keys on row 2 left
    /// to finish their swoosh (only [`LastWalk`] kept, inside
    /// [`CHAIN_GAP_MAX`]), then a full-screen program takes the glass (a
    /// curtain on a ribbon with nothing laid, which arms the gate and nothing
    /// else) and the hand types twenty keys at `(5, 4)` in the new space: a
    /// new line, continuing the retired walk's colour on the fast leg. Without
    /// the curtain's gate setter the first key continues `last_walk`'s
    /// distance, `d0 = 30`.
    #[test]
    fn a_new_line_after_a_curtain_continues_the_colour_on_the_fast_leg() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let mut now = at(t0, 29 * 60);
        for _ in 0..40 {
            if rib.cohorts().is_empty() {
                break;
            }
            now = at(now, 100);
            rib.plan(&ctx(now, &c, (2, 32), 0.0));
        }
        assert!(
            rib.cohorts().is_empty() && rib.last_walk.is_some(),
            "the premise: the line retired, leaving its walk: {:?}",
            rib.cohorts()
        );
        rib.curtain(now);
        let keys = Keys {
            g: geom(),
            row: 5,
            col0: 4,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(now, 100), keys, &c);
        assert_a_new_line(&rib, (5, 4), 20, t_next, "after a curtain");
    }

    /// The row the hand left rides the screen with its text
    /// ([`FreshLine::left_row`]). A composer at the screen's bottom grows a
    /// row up by scrolling the screen, and an inline viewport slides by a
    /// band move. The raced composer newline of the test above, with the
    /// screen moved one row up between the newline's move and the held key's
    /// echo (once by a scroll, once by a band move): the old line is on row 1
    /// now, the new line's first glyph lands at `(2, 2)`, and it still starts
    /// the walk again. With the gate's row left standing, the glyph would
    /// read as the old line's text and the new line carry `d0 = 30`.
    #[test]
    fn the_row_the_hand_left_rides_a_scroll_and_a_band_move_with_its_text() {
        let c = cfg(true, true);
        for (what, band) in [("a scroll", false), ("a band move", true)] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let t_next = a_line_of_thirty(&mut rib, t0, &c);
            let chord = at(t0, 29 * 60 + 100);
            let key = at(chord, 10);
            let cx = ctx(key, &c, (2, 32), 0.9);
            rib.hold_typed(&typed(), key, &cx, Some((2, 32)));
            rib.plan(&cx);
            let seen = at(chord, 30);
            let cx = ctx(seen, &c, (3, 2), 0.9);
            rib.on_event(&row_change((2, 32), (3, 2), Licence::Return), seen, &cx);
            rib.plan(&cx);
            if band {
                rib.translate_band(0, 10, -1);
            } else {
                rib.translate_scroll(1);
            }
            let left = rib.fresh_line.and_then(|gate| gate.left_row);
            echo_the_held_key_then_type_on(&mut rib, &c, (2, 2), key, at(seen, 18));
            assert_a_new_line(&rib, (2, 2), 20, t_next, what);
            assert_eq!(
                left,
                Some(1),
                "{what}: the row the hand left moved with the old line"
            );
        }
    }

    /// A wake on a dark row starts its walk at the landing, the walk a meteor
    /// landing there paints: thirty keys on row 2, an arrow to the dark row
    /// 6, and a hop along it to column 16. The hop's wake continues the
    /// freshest walk's colour (row 2's, one step past its end) anchored at
    /// its landing at distance zero, so its cells walk `d/16` from the
    /// landing, and `meteor::landing_field` is that wake's own number at
    /// every one of its columns, bit for bit.
    #[test]
    fn a_wake_on_a_dark_row_starts_its_walk_at_the_landing_as_the_meteor_paints_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let t_next = a_line_of_thirty(&mut rib, t0, &c);
        let d = at(t0, 29 * 60 + 100);
        let cx = ctx(d, &c, (6, 10), 0.9);
        rib.on_event(&nav((2, 32), (6, 10)), d, &cx);
        rib.plan(&cx);
        let h = at(d, 100);
        let cx = ctx(h, &c, (6, 16), 0.9);
        rib.on_event(&nav((6, 10), (6, 16)), h, &cx);
        rib.plan(&cx);
        let wake = *rib
            .cohorts()
            .iter()
            .find(|k| k.row == 6 && k.wake)
            .expect("the hop laid a wake on the dark row");
        assert_eq!(
            (wake.anchor_col, wake.t0.to_bits(), wake.d0),
            (16, t_next.to_bits(), 0.0),
            "the wake continues the colour, anchored at its landing at distance zero"
        );
        let cells: Vec<(u16, f32)> = rib
            .cells()
            .iter()
            .filter(|l| l.cohort == wake.id)
            .map(|l| (l.col, l.t))
            .collect();
        assert!(cells.len() >= 4, "the wake has a corridor: {cells:?}");
        for &(col, t) in &cells {
            let dx = f32::from(col) - 16.0;
            assert_eq!(
                tri(t).to_bits(),
                super::super::meteor::landing_field(wake.t0, dx).to_bits(),
                "col {col}: the wake is {t} where a landing on the dark row paints {}",
                super::super::meteor::landing_field(wake.t0, dx)
            );
        }
    }

    /// The swoosh shortens the mark into the hand and stops at the caret
    /// block: no column lights past the block's right edge, and the attach
    /// under the caret cell (lit since the key, the head's own light) only
    /// shrinks toward the block's left edge with the rest of the mark.
    #[test]
    fn the_swoosh_stops_at_the_caret_block_s_left_edge() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let caret_x = 10.0 * geom().cw as f32;
        let block_right = caret_x + geom().cw as f32;
        let mut reach_prev = f32::INFINITY;
        for ms in [1000u64, 1100, 1200, 1300, 1400] {
            let cx = ctx(at(last, ms), &c, (2, 10), 0.0);
            rib.plan(&cx);
            let reach = rib
                .plan_segments()
                .iter()
                .map(|s| s.x)
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                reach <= block_right + 0.5,
                "+{ms} ms: a slab at x {reach} is past the caret block (right edge {block_right})"
            );
            assert!(
                reach <= reach_prev + 0.5,
                "+{ms} ms: the mark's reach grew from {reach_prev} to {reach} under a lifted hand"
            );
            reach_prev = reach;
            for s in rib.plan_segments() {
                assert!(
                    s.x <= block_right + 0.5,
                    "+{ms} ms: a slab at x {} is past the caret block (right edge {block_right})",
                    s.x
                );
            }
        }
    }

    // -- the caret notch and slivers -----------------------------------------
    //
    // A composer line typed at 12 cps with a block caret right of the last
    // word. Without the attach the band ends at the block's left edge and the
    // ground shows as a black foot under the block, and a run's first
    // boundary is feathered whatever stands to its left. The pins below are
    // stated on a per-device-column census of the emitted quads at a retina
    // geometry (`geom_owner`, 30 × 56 device px), at three slabs per cell for
    // the composer line alone and one for a screenful of lit rows.

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

    /// One frame on the owner's glass: plan and emit at `now` with the caret
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

    /// The attach. Under a live caret the band's top edge holds its plateau
    /// height through the head cell up to the block's left edge, and the body
    /// (rail foot and crown, not only the strip) continues under the caret
    /// cell, so the block stands on the band: no device column from the head
    /// cell's left edge to the block's right edge is ground-dark, none has a
    /// top lit row under the plateau by more than one row, none a rail
    /// shallower than the head's by more than one row, and none a peak more
    /// than a dither step under the head's. On a cold hand (a word's first
    /// key, momentum 0) as on a hot one, and the hot edge's hairline stands
    /// over the head cell on both ([`HOT_EDGE_GAIN_FLOOR`]).
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

    /// One frame's census on the owner's row: `(ms, caret, column_census
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

    /// The block stands on whole light after a jump, and on the keys that
    /// follow it. Ten keys, Home (cols 0..1 are dark, so the move lays a
    /// corridor), End (back over the corridor's own live cells, a scrub),
    /// then three more keys: the head cell and the block's own cell read the
    /// band's own level across every device column of the 30 × 56 cell. A
    /// rule that feathered the typing end while the run carried a corridor
    /// fails this.
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

    /// The block stands on the band through a leaving caret cell, read off
    /// `frames` (`owner_frames` over device columns from `x0`, `cw` px a
    /// cell): on every frame every column of the caret cell is lit; no column
    /// of the span rises by more than a dither step (2 levels) in one frame,
    /// including the frame the leaving cell retires and the attach takes over;
    /// and no column from the caret cell's left edge to `hold_x1` (cells the
    /// block has stood on) falls faster than the fade's own slope.
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

    /// The attach survives an erase. A Backspace leaves the caret over the
    /// cell it erased, which drains under the block (`Cell::retract_at`) and
    /// retires 240 ms on; a content retirement (`Ribbon::retire_cells`) puts
    /// a melting cell under the caret the same way. A leaving cell under the
    /// caret is floored at the attach's level ([`Ribbon::plan_run`]): the
    /// block stands on the band while the cell spends, the frame that retires
    /// it changes no byte, and a cell the block leaves behind (a held
    /// Backspace at 30/s) drains from the level it showed, its retract
    /// restarted when the block leaves it ([`Ribbon::build`]). Four legs,
    /// each on a cold and a hot hand, per 16 ms frame through the
    /// retirement: one Backspace; four at 30/s; the caret's own run's cells
    /// retired by content under the caret (an insert shifted the glyphs); and
    /// a neighbour cohort's cell (an arrow's wake) retiring under the caret.
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

    /// The floor is a typed key's: the hot edge's gain floor lets the first
    /// key of a word attach the band's top to the caret; a cold arrow hop's
    /// wake is no key, and its head carries no hairline on a cold hand, while
    /// a hot hand's wake keeps the hairline it always had.
    #[test]
    fn a_cold_arrow_s_wake_carries_no_hairline_and_a_hot_one_keeps_its_own() {
        let g = geom_owner();
        let row = OWNER_ROW;
        for (disp, hand, lit) in [(0.0f32, "cold", false), (0.7, "hot", true)] {
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let cx = ctx_in(t0, &c, (row, 8), disp, g);
            rib.on_event(&nav((row, 2), (row, 8)), t0, &cx);
            rib.plan(&cx);
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, 200), 8, disp);
            assert!(
                !sink.under.is_empty(),
                "{hand}: the hop's wake is laid and lit"
            );
            assert_eq!(
                !sink.out.is_empty(),
                lit,
                "{hand} hand: the wake's hairline ({} out quads) — a floor is a typed key's",
                sink.out.len()
            );
        }
    }

    /// The peak level per device column over `xs` inside the owner's row
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

    /// No slivers: a boundary two live runs share carries the older side's
    /// light. A word typed on the reference glass and an arrow hop beside it
    /// (the hop's wake is its own cohort, abutting the word; `heal_seams`
    /// folds typed cohorts only) print one continuous band: no device column
    /// inside it is darker than both its sides by more than a dither step,
    /// and the top edge steps no more than one device row between adjacent
    /// columns, except at the run's oldest cell.
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

    /// The feather grows in as its neighbour goes: the composer re-anchor.
    /// `and I also ` typed, then the composer's licensed park
    /// (`(56,13) → (56,2)`: `re_anchor` drains the row's cells toward the
    /// landing over the retract's 0.4 s) and its return on the same tick,
    /// then `seee an ugly gap` typed on at 12 cps while the old cells fade and
    /// retire one by one under the new word. Read at 16 ms from the park
    /// through the hand's grace: no device column's peak falls by more than
    /// the fade's own steepest frame between two frames (36 levels: `spend`
    /// is `(1 − u)²` over 0.24 s, so its first 16 ms take about 34 levels off
    /// the strip's peak), and on every settled frame (past the newest cell's
    /// 40 ms wipe) no column inside the live band is darker than both its
    /// sides by more than a dither step, including the frame an old cell
    /// retires and the run's first boundary moves.
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
        // Through the hand's grace and no further: past it the swoosh's
        // retract pulls the mark's edge into the caret, and a column the edge
        // passes goes dark in a frame by design (`retract_x`). The grace in
        // force is `Cohort::rest_s`, `max(900 ms, 2.5 · ioi)`; at a steady
        // 83 ms that is `PHRASE_REST_MIN_S`.
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
                // The live band: the cells that are not leaving, to the block.
                // The drained cells to its left go out farthest from the park
                // first (a valley there is the re-anchor's own drain, not a
                // seam), and the live band's first cell, whose left edge the
                // feather grows into as its neighbour goes, may feather.
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

    /// One slab per cell: a screenful of lit rows on the reference glass
    /// (three hundred cells on three rows above the composer line) drives
    /// `slabs_per_cell` to one and the frame to the budget's edge, and the
    /// attach and the seam law hold there too: the block stands on the band,
    /// no column inside the composer line's band is a sliver, and the top
    /// edge steps no more than the comet taper's own per-cell lerp (three
    /// device rows, from 0.30 ch over twelve cells resolved once per cell).
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
}
