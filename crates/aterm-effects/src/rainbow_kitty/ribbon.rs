// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE RIBBON** — the laid rainbow the owner's design keeps unchanged, plus
//! the two refinements v2 adds and the per-row field index every other
//! producer reads.
//!
//! Design of record: `RAINBOW-KITTY-V2.md` §4 (the ribbon, kept), §4.1 (the
//! hot edge), §4.2 (no mark inside the band), §2.3 C2 (the ribbon's `t` IS the
//! laid field), §3.2 (the luminance hierarchy), §3.3 (the light fork), §3.4
//! (requests vs the ledger), §18 (the O(1) field index and the quad budget).
//!
//! ## What is carried verbatim in law
//!
//! The tall body (`TALL_UP_CH 1.10 ch`, spine at the row bottom, `dn`
//! `0.25 → 0.305 ch`), the classic walk (`d/16` for the first
//! [`WALK_FAST_CELLS`] cells, then [`WALK_LAY_RATE`] per cell), one
//! `aterm_render::ribbon_beam` call over `RibbonVertex`s with the C¹
//! `ribbon_profile` (core share 0.5) and its Bayer dither, the flat body
//! (head floor 1.0, crest gain 0.0), the 0.75-cycle wave at `0.055 ch · disp`,
//! the 18 ms `edge-in` and the exact-zero expiry melt, the four-letter
//! guarantee and its chain law, the staggered backspace retract, the wrap
//! fold, the exit swoosh (`0.75 grace + 3 × 0.05 reach + 0.40 retract +
//! 0.24 fade`), and the source-over twin on light themes.
//!
//! ## What is genuinely new, and why
//!
//! 1. **The hot edge** (§4.1) — a 1-px `#FFFFFF` additive hairline along the
//!    ribbon's TOP edge, from the head cell back [`HOT_EDGE_CELLS`] cells,
//!    drawn as a `comet_beam` hairline sampled at the same `spine − up` the
//!    body used (the top edge carries the wave, so three axis-aligned rects
//!    cannot follow it). Dark themes only, into `out`, SPATIAL rather than
//!    temporal so it can never lag the head; its gain is priced at the head
//!    cell's birth and scaled by the head cell's envelope
//!    ([`Ribbon::env_of`]), so it goes out WITH the body — ember, melt and
//!    swoosh — and never holds over a spent one. It is the first item on the
//!    owner A/B sheet (§22), so it lives behind one gate —
//!    [`Ribbon::hot_edge_gain`] — that is one line to flip off.
//! 2. **The field index** (§18) — a per-row `col → t` lane built ONCE per tick
//!    by [`Ribbon::plan`] and answering [`Ribbon::field_at`] in O(1). v1's
//!    `rainbow_field_at` was a LINEAR SCAN of every spark, called per station
//!    per frame: a measured ~1.7 M spark visits on one jump frame. The meteor,
//!    the caret and stardust all read this index instead.
//! 3. **No mark inside the band** (§4.2) — the ribbon publishes its [`Band`]
//!    (`top` / `spine` / `bottom`) so stardust can be born in the SKY (§5.4)
//!    instead of on a stroke. v1's in-cell starfield placement is deleted.
//!
//! ## The two measured defects this module is written against
//!
//! * **"The body reads dim and muddy"** — a mid-row peak of 100–130/255 with
//!   olive and navy mids, and only the bottom 2–3 px bright. Three parts, and
//!   this module answers TWO of them; it does not claim the third. The NAVY
//!   mids were v1's dark stops (blue, indigo, violet) composited as smudges
//!   below the ground's own reach; [`bed_ink`] walks them toward white until
//!   they sit on the same composited luminance as every other stop, so the
//!   arc is EVEN — indigo weighs what red weighs — and the 33-entry
//!   per-position coverage table v1 priced that against is not carried
//!   (§19.1: *"two ceilings for one bed"*). The bottom-only brightness was
//!   the baseline strip out-shining a body that had been capped below it; the
//!   strip is now priced out of the SAME ceiling the body is
//!   ([`BODY_FRAME_TOP`] − `cov`, ~15 levels on a hot mark) instead of beside
//!   it, so it is a crisp accent on a bright body rather than the only lit
//!   thing in the mark. The OLIVE mids are NOT fixed here. The "one
//!   composited luminance" is L3's 5.25:1 bar against the theme's foreground
//!   — `Y ≈ 0.080` on the default theme — and a warm stop at that luminance
//!   IS an olive: a full-coverage yellow composites at `(78, 78, 2)`, max
//!   channel 78, at or below the ≈ 84 v1's table produced. The warm mids sit
//!   at the bar's ceiling BY CONSTRUCTION. Whether the bar is relaxed for the
//!   bed over BLANK cells (e.g. 3:1, with the ledger still holding 5.25:1
//!   over probed glyph cells per §3.4) is an **owner ruling, OPEN** — it
//!   needs a §22 A/B row and has none — not a recipe this module may choose.
//!   The number is pinned either way by
//!   `the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_78_under_the_bar`,
//!   so "dim" has a figure to move when the ruling lands.
//! * **"It keeps BRIGHTENING for ~660 ms after the last key"** — light with no
//!   keystroke behind it, which is the anti-stray law's own complaint. The
//!   cause is a body whose coverage re-read the LIVE spine every frame while
//!   the follower was still climbing. v2's law: **a cell's light is priced
//!   ONCE, at birth, from [`super::spine::Spine::birth_disp`]**
//!   ([`Cell::cov0`]). The live spine may still move the body's SHAPE — the
//!   wave amplitude and the wedge follow the follower, which keeps climbing
//!   for ~100 ms after the last key and then settles — but it may never move
//!   its LIGHT: the body's coverage and the hot edge's gain are both priced
//!   from the head cell's own [`Cell::birth_disp`], and both are then closed
//!   by the same envelope ([`Ribbon::env_of`]) — the hairline has no fade of
//!   its own to get out of step with the body's. Pinned by
//!   `the_body_never_brightens_after_the_last_keystroke`, on the body's
//!   request AND on the additive light the hot edge actually emits; the
//!   "goes out with its body" half is pinned on the swoosh by
//!   `the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero`
//!   and on the ember by `the_hot_edge_embers_out_with_its_body_on_focus_loss`.
//!
//! ## The ledger, and the one ceiling this producer applies
//!
//! §3.4: *requests are clipped by the ledger, never by ad-hoc caps.* This
//! module applies exactly ONE ceiling of its own — the bed's published
//! [`UNDER_COV_CAP`], with the strip's accent inside [`BODY_FRAME_TOP`] — and
//! the host's `spend_rainbow_budget` does the rest over probed glyph cells.
//! The per-position table v1 carried beside that cap is not reproduced;
//! [`bed_ink`] is where the arc's per-hue light is equalized, and it is
//! equalized in the coordinate the 5.25:1 bar is actually written in.
//!
//! **Why the bed ink is not v1's `rainbow_bed_ink` byte for byte**, although
//! §3.1 says "kept verbatim": v1's recipe (`rainbow_bed_glass` + the hue
//! give-back) leaves a bright hue such as yellow at `#FFFF00` and reaches
//! L3's 5.25:1 bar through the 33-entry COVERAGE table (yellow's entries sit
//! at 55–65 of 212, so yellow composited at a max channel of ≈ 84 over the
//! default ground — that is the measured "olive mid"). §19.1 deletes that
//! table, and under the bar the composited bed luminance is bounded at
//! `Y ≤ 0.081` for EVERY hue against the default foreground, so a bed with one
//! coverage ceiling has to carry the bar in its ink. [`bed_ink`] composites
//! yellow at the same luminance v1's table did (`Y ≈ 0.080` vs `≈ 0.086`)
//! with more chroma — which is to say it equalizes the arc AT L3's ceiling
//! and leaves the warm mids where v1 left them, ≈ 78 max channel composited.
//! v1's ink at a flat 236 would composite yellow at `(243, 243, 33)` —
//! ≈ 1.25:1 — and a "max channel ≥ 120 at yellow" floor is arithmetically
//! incompatible with 5.25:1 (`(120, 120, ·)` is 3.1:1). Any brighter warm
//! mid is therefore a ruling on the bar, not a recipe — and that ruling was
//! made on 2026-09-06 under the owner's delegation of taste: **THE BAR
//! STANDS, over every cell.** L3's 5.25:1 / luma-72 ceiling (yellow at
//! `(78, 78, 2)`, olive by construction) is kept, and the alternative on the
//! table — the bar relaxed to ≈ 3:1 for the bed over BLANK cells with the
//! ledger holding 5.25:1 over probed glyph cells (§3.4) — is REJECTED: the
//! ribbon lies under text that arrives cell by cell, so a bed whose
//! brightness depended on whether a glyph is there would dim each cell the
//! moment its letter lands and flicker along the line as a word is typed,
//! the opposite of the "light on the frame the caret lands" law. The measured
//! dim-and-muddy complaint was closed on the other axis — the cold key's
//! share of the ceiling (`BODY_COLD_SHARE` 0.88) and the hot edge from the
//! second key (`HOT_EDGE_DISP_MIN` 0.15), both under the same bar (§23). The
//! max-channel pin beside
//! `letters_stay_legible_under_the_ribbon_on_the_default_dark_theme` holds
//! the number so a future change to the bar has something to move.
//!
//! ## Contract notes (stage 2)
//!
//! Changes to the stage-1 shapes, each flagged rather than smuggled:
//! [`Cell`] gains [`Cell::cov0`] and [`Cell::birth_disp`] (the birth-priced
//! light and the spine it was priced at; without them the "keeps brightening"
//! defect cannot be fixed) and loses its write-only `seed` (stardust deals its
//! own seeds; a field nothing reads is a determinism story that is not
//! wired); [`Cohort`] gains [`Cohort::anchor_col`], the immutable origin the
//! classic walk is a function of, and [`Cohort::abandoned`], because the
//! exit swoosh is the FINGER-LIFT arc and there is one finger — a live key
//! holds every un-abandoned cohort, so a wrapped paragraph keeps its earlier
//! rows until the hand lifts; and [`Ribbon`]'s pool, plan and index are
//! PRIVATE behind read-only accessors, so nothing can mutate the pool behind
//! the index the other producers read. The resident scratch (`runs`,
//! `sorted`, `verts`, `ink`, `ember_at`, `rearm`) is private too, and the
//! index keeps its dark lanes in a spare pool, so the frame path allocates
//! nothing — including on the Enter / wrap that lights a row that was dark.
//! The vertex density is a per-frame number ([`Ribbon::slabs_per_cell`]),
//! because `ribbon_beam` tiles at least one slab per vertex and §18's budget
//! is otherwise unreachable at retina. [`Ribbon::head_rgb`] keeps v1's
//! contract byte for byte: the AUTHORED stop on dark, the rail's ink on
//! light.

use std::mem;

use aterm_time::Instant;

use aterm_render::{BeamVertex, GlowBlend, RibbonVertex, comet_beam, ribbon_beam};

use crate::cursor_glow::InkRole;
use crate::effect_util::lerp_rgb;
use crate::spectrum::spectrum;

use super::meteor::tri;
use super::spine::{DISP_RELEASE_TAU, PHASE_RATE};
use super::timing::{
    EDGE_IN_S, JUMP_MIN_CELLS, REDUCED_MOTION_FADE_MS, clamp01, edge_in, smoothstep01, spend,
    suck_in,
};
use super::{Cadence, Config, Ctx, Event, Frame, Licence, level_step_spend};

// ===========================================================================
// The budget, the walk, and the body's geometry
// ===========================================================================

/// Quads the ribbon may spend on one frame (§18's `RAINBOW_RIBBON_QUAD_BUDGET`
/// — a 76-cell hot line at retina). Shed from the TAIL, never from the head:
/// the head cell is the per-key light and losing it would break the coupling
/// contract (§7.3, "one key, one cell, one light, one note").
pub const RIBBON_QUAD_BUDGET: usize = 10_240;

/// Cells the hot edge reaches back from the head (§4.1).
pub const HOT_EDGE_CELLS: f32 = 3.0;

/// Peak alpha of the hot edge at the head cell: `alpha(d) = 0.38·(1 − d/3)²`.
pub const HOT_EDGE_ALPHA: f32 = 0.38;

/// The hot edge's coverage request ceiling — inside the transient cap (§4.1),
/// which is what keeps a third white idiom legibility-safe.
pub const HOT_EDGE_COV_MAX: f32 = 38.0;

/// The spine value below which there is no hot edge at all (§4.1): it is the
/// "still wet at the hand" mark, and a cold hand is not wet.
///
/// **0.15, from 0.35 (2026-09-06, the paint-conformance finding in §23).**
/// At 0.35 the crisp edge arrived at key 4-5; a hand is wet from its second
/// key, and the edge is the one mark that reads as SPEED, so it now rises
/// from `disp` 0.15 (key 2 at 8 cps) over the same 0.30 span.
pub const HOT_EDGE_DISP_MIN: f32 = 0.15;

/// Width of the smoothstep that fades the hot edge in over
/// [`HOT_EDGE_DISP_MIN`] — `smoothstep((disp − 0.15)/0.30)`.
pub const HOT_EDGE_DISP_SPAN: f32 = 0.30;

/// How far, in `ch`, the hot edge's hairline may sit from the caret's own cell
/// band before it stops being "the mark at the hand" and becomes a highlighter
/// on somebody else's row.
///
/// It is a CONSEQUENCE, not a knob: the hairline rides `spine − up`, which
/// under the tall spelling is `0.10 ch` above the caret cell's top and under
/// the underline spelling is `0.305 ch` below it. `0.55` bounds both with room
/// for the wave, and the pin
/// (`the_hot_edge_lives_only_at_the_head_on_a_dark_ground`) is what stops a
/// later geometry change from quietly moving a white line onto another row.
pub const HOT_EDGE_CARET_REACH_CH: f32 = 0.55;

/// Cells of the classic walk's FIRST phase, which advances `d/16` per cell so
/// a short word already shows several stops (§4, v1 verbatim).
pub const WALK_FAST_CELLS: f32 = 16.0;

/// The classic walk's steady lay rate, `t`-units per cell, after
/// [`WALK_FAST_CELLS`]. §6.4's meteor arc CONTINUES this exact rate backwards
/// from the caret's stop — the two numbers are the same number, and a change
/// here is a change to the meteor.
pub const WALK_LAY_RATE: f32 = 1.0 / 36.0;

/// THE BIRTH FLOOR — v1's `RAINBOW_BIRTH_EDGE_FLOOR`, verbatim: a laid cell is
/// READABLE on the exact frame that echoes its key. The 18 ms `edge-in` (§2.5,
/// T3) is the one sanctioned attack, but it ramps from ZERO, and the host can
/// present a keystroke's echo 0.2–0.4 ms after the key: at that age `edge_in`
/// is 0.0008 and the new cell quantises to nothing, so the glyph presents
/// without its colour and the ribbon arrives one panel period later (the live
/// capture of 2026-09-05 measured 6/255 on the echo frame vs 71 at +17 ms, on
/// 4 of 44 keys). v1 had this floor and lit the cell at 75 on that frame; v2's
/// port dropped it. The floor rides ONLY the cell's birth envelope — the
/// focus-regain rearm keeps its unfloored `edge_in` so regain never snaps.
/// Owner law (§2.1 T2): "the frame that acknowledges the key already shows
/// the colour."
pub const BIRTH_EDGE_FLOOR: f32 = 0.35;

/// The shipped `tall` body's reach ABOVE the spine, in `ch` (v1's
/// `RAINBOW_TALL_UP`). D16's whole point: under `tall` the band covers the
/// entire cell, so star birth zones are defined against [`Band::top`] and land
/// in row − 1.
pub const TALL_UP_CH: f32 = 1.10;

/// The tall body's full-strength plateau above the spine, in `ch` (v1's
/// `RAINBOW_TALL_CORE`). Under `aterm_render::RIBBON_CORE_SHARE` of
/// [`TALL_UP_CH`], so the rasterizer's own plateau clamp is a no-op and the
/// melt keeps the whole of the rest of the cell to fall through.
pub const TALL_CORE_CH: f32 = 0.40;

/// The DOWNWARD melt's floor, in `ch` (v1's `RAINBOW_RIBBON_DN_FLOOR`) — the
/// narrowest the transverse falloff below the spine may ever get, at any
/// momentum and any phase of the wave.
pub const DN_FLOOR_CH: f32 = 0.25;

/// The downward reach at full momentum, in `ch` (v1's `RAINBOW_RIBBON_TOP`) —
/// [`DN_FLOOR_CH`] plus exactly [`WAVE_AMP_CELLS`], so the room the wave needs
/// is inside the tile BY CONSTRUCTION and the clamp is a bound on the wave
/// rather than a tax on the profile.
pub const DN_TOP_CH: f32 = DN_FLOOR_CH + WAVE_AMP_CELLS;

/// The UNDERLINE spelling's reach above the spine, in `ch`: the rest of the
/// cell once [`DN_TOP_CH`] is spent below it, so the mark's top edge lands
/// exactly `0.305 ch` under the cell top. An underline however the shoulder is
/// spelled.
pub const UNDERLINE_UP_CH: f32 = 1.0 - DN_TOP_CH;

/// The leading's own half-thickness in `ch` (v1's `RAINBOW_RIBBON_LEAD`,
/// `RAINBOW_UNDERLINE_H / 2`): the underline spelling's plateau, and the
/// baseline strip's span. It is the same number the resting strip used to be
/// the HEIGHT of; it is now the width of a plateau, so what used to cut melts.
pub const LEAD_CH: f32 = 0.11;

/// **THE ONE FORK** (§4, D14's "as a parameter rather than a fork"): the tall
/// highlighter holds the FULL shoulder, so the letters stand inside the light
/// and the profile melts only at the cell edge.
///
/// `aterm_render::ribbon_profile` short-circuits at exactly `1.0`, so this
/// spelling is also the cheap one.
pub const SHOULDER_TALL: f32 = 1.0;

/// …and the underline spelling's quieter glyph-band wash (v1's
/// `RAINBOW_RIBBON_SHOULDER`). It is A POINT ON THE CURVE — the value the
/// profile passes through once the core ends — and there is no second emitter
/// for it to be the gain of. The tall/underline fork flip-flopped six times in
/// five days in v1 precisely because it was spelled as two bodies; here the
/// only difference between the two spellings is [`BodyProfile`].
pub const SHOULDER_UNDERLINE: f32 = 0.55;

/// The 0.75-cycle wave's amplitude in cells, at full spine (v1's
/// `RAINBOW_WAVE_AMP_CELLS`).
pub const WAVE_AMP_CELLS: f32 = 0.055;

/// Cycles of the wave across the WHOLE mark, whatever its length (v1's
/// `RAINBOW_WAVE_CYCLES`): three quarters, so the mark undulates once and does
/// not read as a ripple.
pub const WAVE_CYCLES: f32 = 0.75;

/// The share of the mark over which the wave's amplitude smoothsteps up from
/// the head (v1's `RAINBOW_WAVE_HEAD_PIN`). The head is PINNED so the cell
/// under the hand never moves.
pub const WAVE_HEAD_PIN: f32 = 0.35;

/// Cells over which the body's downward wedge closes behind the head. The mark
/// is fatter under the hand and settles into the leading behind it — about a
/// word's worth of swell.
pub const BLOOM_REACH_CELLS: f32 = 8.0;

/// Vertices per cell along the major axis, AT MOST. ONE VERTEX PER SLAB,
/// because `ribbon_beam` interpolates a vertex's COLOUR to its neighbour's in
/// sRGB — a CHORD across the arc — and the arc's fastest leg is the one a
/// chord walks straight back across. Three thirds keep the emitted mark's
/// cyan dwell under the arc's own; the POSITION is what ramps, and the colour
/// is resolved from the arc at every slab.
///
/// It is a CEILING, not the frame's number: `ribbon_beam` tiles at least one
/// slab per segment whatever stride it is handed, so the vertex density is
/// the only knob the quad budget has. [`Ribbon::slabs_per_cell`] is what a
/// frame actually plans — this, or fewer when the live cells × rows would
/// not fit [`RIBBON_QUAD_BUDGET`] at it (a 76-cell hot line at retina is
/// ~12 k quads at three; at one it is ~4 k, and the tail is not cut).
pub const SLABS_PER_CELL: usize = 3;

/// The baseline strip's own gain, as a share of the cell's ink coverage (v1's
/// `RAINBOW_STRIP_GAIN`): the crisp accent under the letters, riding
/// `aterm_render::ribbon_lift_profile` over [`LEAD_CH`] in the leading.
///
/// It is CLAMPED to `255 − cov` by [`Ribbon::plan`], which is why v2's strip
/// can never reproduce the measured "only the bottom 2–3 px bright" defect:
/// the body now rides its own ceiling, so the strip has ~19 levels of headroom
/// to be crisp in and no more.
pub const STRIP_LIFT_GAIN: f32 = 0.35;

// ===========================================================================
// The bed's ONE ceiling (§3.2, §3.4, L3)
// ===========================================================================

/// The bed's published byte ceiling (L3's `RAINBOW_UNDER_COV_CAP 236`). It is
/// the SOURCE-OVER stream's cap, and the ledger's frame top (251) leaves the
/// companions their remainder above it (§3.4).
pub const UNDER_COV_CAP: f32 = 236.0;

/// The coverage at which a planned boundary counts as ON THE GLASS for
/// `trail status` ([`Ribbon::lit_segments`]): half the cap. Derivation: the
/// bed's dimmest stop composites at a brightest channel of ≈ 78 at full
/// coverage (the pin below), and the paint scanner's colour floor is 60 on
/// that channel over a ground near 20 — so the dimmest stop reads as ink from
/// about half coverage up, and a claim made under that would be a claim the
/// pixels cannot honour.
pub const STATUS_LIT_COV: u8 = (UNDER_COV_CAP / 2.0) as u8;

/// The LEDGER'S FRAME TOP (§3.4): the level a `glow_under` pixel may reach
/// once every stream on it has been priced. The bed's own request stops at
/// [`UNDER_COV_CAP`]; the 15 levels between the two are the baseline strip's
/// accent, and there is nothing else in `under` on a ribbon row.
///
/// It is safe to spend them because the bed's ceiling is carried by the INK
/// (see [`bed_luma_budget`]): at full opacity the composite IS the ink, whose
/// relative luminance is the bar's own budget, so a brighter ALPHA cannot
/// break a bar that a brighter COLOUR would have.
pub const BODY_FRAME_TOP: f32 = 251.0;

/// The contrast bar every legibility ceiling in the rainbow family is solved
/// against: unlit ink over lit ground, 5.25:1 (L3).
pub const BODY_CONTRAST_BAR: f32 = 5.25;

/// The margin under [`BODY_CONTRAST_BAR`] the emitter's own rounding may not
/// be able to cross. Quantization, the Bayer offset and the LUT's own
/// interpolation each cost a fraction of a level; 0.15 of a ratio point is
/// ~4 levels of composited luma at the bar, which is more than all three can
/// spend together.
pub const BODY_CONTRAST_GUARD: f32 = 0.15;

/// Floor on the bed's composited luminance budget. A theme whose foreground is
/// almost black would solve to a bed nobody can see; below this the ribbon
/// stops obeying the bar and simply takes the dimmest light that still reads.
pub const BED_LUMA_MIN: f32 = 0.020;

/// …and the ceiling. L5: the caret is the brightest PERSISTENT thing on glass,
/// and the ribbon is a persistent thing. A white-on-white theme would solve to
/// an unbounded budget; the ribbon takes this and no more.
///
/// L3's "field luminance ceiling 72" is this budget read in v1's unit: on the
/// default theme [`bed_luma_budget`] solves to `Y ≈ 0.080`, and a warm stop
/// composited at full coverage on that budget carries a Rec.709 gamma-luma of
/// ≈ 72/255 (`(78, 78, 2)` at yellow). The two numbers are one ceiling in two
/// coordinates; this module states it in the one the 5.25:1 bar is written in.
pub const BED_LUMA_MAX: f32 = 0.100;

/// Entries in the bed-ink lookup table. The recipe below is a bisection over
/// the sRGB EOTF — far too expensive per slab — but it is a pure function of
/// the arc POSITION, so it is solved once per theme into this table and read
/// with one lerp. 129 entries put the interpolation error under one level
/// everywhere on the arc.
pub const BED_INK_LUT_LEN: usize = 129;

/// Share of the ceiling a COLD key's cell takes (the rest is bought by
/// momentum, [`Cell::cov0`]). A single keystroke on a cold hand must still lay
/// a band a person can read — the ribbon is the per-key light, and a per-key
/// light that needs a rhythm before it appears is a lag tell. The last eighth
/// is what a sustained run earns.
///
/// **0.88, from 0.72 (2026-09-06, measured by the paint-conformance gate —
/// §23).** At 0.72 of a `Y ≈ 0.08` ceiling the cold bed's brightest channel
/// sat at 40-110/255 on the default theme for the first second of a 7 cps
/// take (frames 40-97 of a kept take: colourful pixels 415-687 on the typed
/// row, none over 110), under the house pixel floor for "effect ink" that v1
/// cleared from its first key — the exact lag tell this constant exists to
/// forbid. The bed's LIGHT now starts near its ceiling; momentum still shows
/// as length, life, the hot edge and the wave, which is where the eye reads
/// speed anyway. The legibility bar (`BED_LUMA_MAX`) is untouched: the share
/// only moves inside it.
pub const BODY_COLD_SHARE: f32 = 0.88;

// ===========================================================================
// Life, the chain law, and the exit swoosh (§4, kept)
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

/// …and the SUPERLINEAR term, so an earned hot run drags a rainbow across many
/// cells and wrapped lines instead of merely a longer wake.
pub const LIFE_SPINE_SQUARE: f32 = 11.0;

/// FOUR-LETTER GUARANTEE: within a rhythm a cell lives at least this many
/// observed inter-key gaps, so the ribbon spans about the last four typed
/// cells at ANY human cadence (4 cells lit needs ~3 gaps; the extra half-gap
/// buys the tail's fade room).
pub const CHAIN_KEYS: f32 = 4.5;

/// The ribbon chains across ANY plausible rhythm — one key every five seconds
/// still counts. Only a truly isolated keystroke (a gap beyond this) takes no
/// floor and fades crisply.
pub const CHAIN_GAP_MAX: f32 = 5.0;

/// Fade margin added to the chained life, seconds.
pub const CHAIN_MARGIN: f32 = 0.10;

/// …capped just above `CHAIN_KEYS × CHAIN_GAP_MAX` so the slowest chained
/// rhythm still earns its full four-letter span.
pub const CHAIN_LIFE_MAX: f32 = 23.0;

/// Cells the four-letter guarantee reaches for.
pub const FOUR_LETTER_CELLS: u16 = 4;

/// FINGER-LIFT ARC: no typing key for this long and the ribbon begins its exit
/// swoosh — finish reaching four letters, then retract into the caret.
pub const LIFT_GRACE_S: f32 = 0.75;

/// One extension cell lights per this step while the swoosh finishes REACHING
/// four letters.
pub const REACH_STEP_S: f32 = 0.05;

/// Beats of [`REACH_STEP_S`] the reach spends.
pub const REACH_BEATS: u16 = 3;

/// Cells the reach may stage in one tick, across every live cohort. Sized so
/// the staging array is a fixed local and the frame path allocates nothing
/// (§18); a mark whose reach is starved by it simply reaches on the next beat.
pub const REACH_STAGE: usize = 8;

/// The retract then drains the ribbon tail→head over this long, whatever its
/// length — a short hop and a hot multi-line comet both slurp cleanly back
/// into the caret.
pub const RETRACT_DUR_S: f32 = 0.40;

/// How long a RETRACTED cell takes to go out once the drain has selected it,
/// so the vanish is continuous instead of a staircase of whole-cell deletions
/// ending in a cliff.
pub const RETRACT_FADE_S: f32 = 0.24;

/// The whole exit swoosh: `0.75 grace + 3 × 0.05 reach + 0.40 retract +
/// 0.24 fade` = 1.54 s (§4, verbatim).
pub const SWOOSH_TOTAL_S: f32 =
    LIFT_GRACE_S + REACH_STEP_S * REACH_BEATS as f32 + RETRACT_DUR_S + RETRACT_FADE_S;

/// Floor on a cell's life so the exit swoosh always terminates the ribbon
/// before the natural melt can — **the swoosh IS the ending, never a passive
/// dim-out.**
pub const SWOOSH_LIFE_S: f32 = 1.70;

/// The swoosh's own offset of its RETRACT: grace plus the three reach beats,
/// the idle at which a cohort starts moving into the caret. A real jump
/// ABANDONS the band by rewinding its cohorts' `alive_at` to exactly this far
/// back, so the mark goes out through the retract + fade it would have taken
/// anyway (`0.40 + 0.24 s`), starting from the light it has NOW — never a
/// step (v1's own note: clamping `life` steps too, because the melt rides
/// `age / life`).
pub const RETRACT_START_S: f32 = LIFT_GRACE_S + REACH_STEP_S * REACH_BEATS as f32;

/// Focus loss embers the ribbon out over this long on `spend` (§8.2). Nothing
/// sounds, and nothing is retracted — the mark simply stops being lit.
pub const FOCUS_EMBER_S: f32 = 0.30;

/// The share of a cell's life over which the expiry melt runs (v1's
/// `RAINBOW_EDGE_OUT`). Identity over the first 70 %.
pub const EXPIRY_MELT_SHARE: f32 = 0.30;

/// The expiry melt's exponent (v1's `RAINBOW_EXPIRY_MELT_GAMMA`). SUPERLINEAR,
/// because a cell that is out of time must be at zero BEFORE it is removed or
/// its removal is a visible step: at γ = 1.6 the melt reaches 6e-4 of body on
/// the last frame, where a sub-linear taper still holds ~6 %.
pub const EXPIRY_MELT_GAMMA: f32 = 1.6;

/// The coverage share the run's outer TAIL boundary keeps, ramping linearly to
/// full across the tail cell (v1's `RAINBOW_RUN_TAIL_EASE`): the band's oldest
/// edge reads as a full slab easing out through a feather, never as a chopped
/// stub.
pub const RUN_TAIL_EASE: f32 = 0.10;

// ===========================================================================
// The atoms
// ===========================================================================

/// One LAID RIBBON CELL — the atom of the field (v1's `spark`, ported).
///
/// `Copy` and flat: the cell pool is a resident `Vec` reused every frame
/// (§18's zero-allocation rule), and a style-crossfade ghost snapshots it.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    /// Grid row.
    pub row: u16,
    /// Grid column.
    pub col: u16,
    /// Which [`Cohort`] laid it — the hue walk's identity, so a re-typed cell
    /// re-joins the run it belongs to instead of restarting at red.
    pub cohort: u32,
    /// THE FIELD at this cell: the classic walk's spectrum position, 0..1+.
    /// C2 — the caret, the ribbon head and a star's halo on this cell are the
    /// same colour on the same frame because they all read THIS number.
    pub t: f32,
    /// When the cell was laid (the echo frame). Drives the 18 ms `edge-in` and
    /// the expiry melt.
    pub born: Instant,
    /// Total life in seconds, priced at birth from `Spine::birth_disp`.
    pub life_s: f32,
    /// **THE CELL'S LIGHT, PRICED ONCE.** The share of [`UNDER_COV_CAP`] this
    /// cell's body may take at its peak, `0..1`, resolved at BIRTH from
    /// `Spine::birth_disp` and never re-read from the live spine again.
    ///
    /// This field is the whole of the fix for the measured *"it keeps
    /// brightening for ~660 ms after the last key"* defect: the follower is
    /// still climbing when the hand stops, so a body that re-prices itself
    /// every frame gains light with no keystroke behind it — which is exactly
    /// what the anti-stray law forbids. Shape may follow the live spine (the
    /// wave, the wedge; both only SETTLE); light may not.
    pub cov0: f32,
    /// True for a cell laid by a real typing advance (as opposed to the exit
    /// swoosh's reach) — the "earned by real typing only" gate v1 spells in
    /// `spawn`, and the gate stardust's field-star deal reads.
    pub typing: bool,
    /// Set when the cell is retracting toward the caret (Backspace or kill):
    /// the `spend` alpha law runs from this stamp. `None` while the cell is
    /// simply living. The exit swoosh drives the whole COHORT instead, because
    /// it retracts the mark as one object.
    pub retract_at: Option<Instant>,
    /// The `Spine::birth_disp` this cell was priced at — the number behind
    /// [`Cell::cov0`], kept so the HOT EDGE can be priced from the head cell's
    /// birth as well: a hairline that re-read the live follower would brighten
    /// for ~100 ms after the hand stopped, which is the "light with no
    /// keystroke behind it" class this module is written against.
    pub birth_disp: f32,
}

/// A COHORT — one contiguous run of cells laid by one typing burst on one row,
/// sharing a hue walk (v1's `classic_run`). The unit the exit swoosh, the wrap
/// fold and the kill drain all act on.
#[derive(Clone, Copy, Debug)]
pub struct Cohort {
    /// Identity, referenced by [`Cell::cohort`].
    pub id: u32,
    /// The row this cohort lives on. A cohort never spans rows; a wrap FOLDS
    /// into a new cohort (§4's "wrap fold", kept).
    pub row: u16,
    /// Leftmost column the cohort covers.
    pub col0: u16,
    /// One past the rightmost column the cohort covers.
    pub col1: u16,
    /// **THE WALK'S ORIGIN** — the column of the cohort's FIRST typed cell,
    /// and immutable: the swoosh's reach moves [`Cohort::col0`] under the
    /// mark, an edit inside the run moves the head, and neither may repaint a
    /// colour the eye has already read. C2 / §4: `t` is a function of
    /// POSITION, `t0 + walk_t(col − anchor_col)` (see [`Cohort::t_at`]), so a
    /// cell backspaced and retyped takes back exactly the stop it had.
    pub anchor_col: u16,
    /// The walk's anchor value — the `t` the cell at [`Cohort::anchor_col`]
    /// took. A same-row rebirth or a wrap fold within v1's freshness window
    /// CONTINUES the previous cohort's walk here; past it, the walk re-anchors
    /// at red.
    pub t0: f32,
    /// When the cohort's first cell was laid.
    pub born: Instant,
    /// The exit swoosh's grace anchor (0.75 s of grace before the drain
    /// begins): the ribbon's last live typing key — on ANY cohort, because the
    /// swoosh is the FINGER-LIFT arc (§4: "no typing key for this long"), and
    /// there is one finger. A hot paragraph typed across three wraps is one
    /// mark: its earlier rows stay lit while the hand is still on the last
    /// one and slurp back into the caret together when it lifts. Only an
    /// [`Cohort::abandoned`] cohort keeps a clock of its own.
    pub alive_at: Instant,
    /// Set by a real jump ([`Ribbon::abandon`]): this cohort is leaving on its
    /// own rewound clock, and a typing key elsewhere no longer refreshes it —
    /// the band under the hand's OLD position must go out while the new one
    /// is being laid, not wait for the next finger-lift.
    pub abandoned: bool,
    /// Where the cohort is in the exit choreography.
    pub phase: Phase,
}

impl Cohort {
    /// **THE FIELD AT A COLUMN OF THIS COHORT** (C2): the classic walk read
    /// from the immutable origin, so it is the same number however the cell
    /// came to be laid — typed, retyped after a backspace, or reached by the
    /// swoosh (a reach cell sits BEFORE the origin and continues the walk
    /// backwards at the same rate).
    #[must_use]
    pub fn t_at(&self, col: u16) -> f32 {
        self.t0 + walk_t(f32::from(col) - f32::from(self.anchor_col))
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
    /// The phase a cohort is in `idle` seconds after its last live cell.
    /// `None` once the swoosh has finished and the cohort is over.
    #[must_use]
    pub fn at(idle_s: f32) -> Option<Self> {
        const REACH_END: f32 = LIFT_GRACE_S + REACH_STEP_S * REACH_BEATS as f32;
        const RETRACT_END: f32 = REACH_END + RETRACT_DUR_S;
        if idle_s.is_nan() || idle_s <= 0.0 {
            // NaN-refusing spelling: a non-finite idle takes the laying arm,
            // which draws the mark rather than dropping it.
            return Some(Self::Laying);
        }
        if idle_s < LIFT_GRACE_S {
            Some(Self::Grace)
        } else if idle_s < REACH_END {
            Some(Self::Reaching)
        } else if idle_s < RETRACT_END {
            Some(Self::Retracting)
        } else if idle_s < SWOOSH_TOTAL_S {
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

/// One planned sample of a cohort's spine at a CELL BOUNDARY — what
/// `aterm_render::RibbonVertex` is built from.
///
/// Boundary-anchored, not centre-anchored, for the reason `RibbonVertex`'s own
/// doc gives: a segment must run boundary-to-boundary so every major-axis slab
/// `ribbon_beam` tiles lands inside ONE cell however the step divides the cell
/// width.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Window-absolute X of the boundary, sub-pixel.
    pub x: f32,
    /// The body's full-strength centreline here, sub-pixel — the 0.75-cycle
    /// wave at `0.055 ch · disp` rides ON this value.
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

/// The vertical extent of the ribbon's body at one cell — `[spine − up,
/// spine + dn]`.
///
/// Published so stardust can obey §4.2 ("no mark is drawn inside the ribbon
/// band") and §5.4 (the sky band is `[top − 0.30 ch, top − 0.04 ch]`, relative
/// to THIS `top`, which is D16's entire resolution: under the shipped `tall`
/// spelling the band covers the whole cell, so a zone stated in cell
/// coordinates would put every strike star inside the body).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// Window-absolute Y of the body's TOP edge (`spine − up`).
    pub top: f32,
    /// Window-absolute Y of the body's centreline, wave included.
    pub spine: f32,
    /// Window-absolute Y of the body's BOTTOM edge (`spine + dn`).
    pub bottom: f32,
}

impl Band {
    /// True when `y` lies inside the body — §4.2's whole claim, in one call, so
    /// stardust never has to re-derive the inequality.
    #[must_use]
    pub fn contains(&self, y: f32) -> bool {
        (self.top..=self.bottom).contains(&y)
    }
}

/// **THE TWO PROFILES, ONE EMITTER** (§4, D14).
///
/// The tall/underline fork is these four numbers and nothing else: both
/// spellings walk the same [`Ribbon::plan`], are drawn by the same single
/// `aterm_render::ribbon_beam` call, and price their light through the same
/// [`Cell::cov0`] law. In v1 the fork was two bodies, and it flip-flopped six
/// times in five days; a body one `if` can produce cannot flip-flop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyProfile {
    /// Reach above the spine, in `ch`.
    pub up_ch: f32,
    /// Reach below the spine at full momentum, in `ch`.
    pub dn_ch: f32,
    /// Full-strength plateau above the spine, in `ch`.
    pub core_up_ch: f32,
    /// The shoulder `aterm_render::ribbon_profile` takes on the UP side — the
    /// one parameter the two spellings really disagree about.
    pub shoulder: f32,
}

/// One row's dense `col → t` lane of the field index.
#[derive(Clone, Debug, Default)]
struct FieldLane {
    /// The grid row this lane indexes.
    row: u16,
    /// `t` by column, `f32::NAN` where the row carries no field. Sized to the
    /// grid's column count once and REUSED — the lane is cleared over
    /// [`FieldLane::touched`] only, never zero-filled wholesale (§18 deletes
    /// v1's 8 KB `owner_starts` zero-init per frame for exactly this reason).
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

/// **THE FIELD INDEX** (§18) — `col → t` for every row carrying live ribbon
/// light, rebuilt once per frame by [`Ribbon::plan`].
///
/// Lookups are O(1) in COLUMNS and linear in LIVE ROWS (a handful: the ribbon
/// is a per-row mark and the cohort cap bounds it). That is what replaces
/// v1's three per-station reverse scans of `sparks`, which is the single
/// largest CPU line item §18 deletes.
#[derive(Clone, Debug, Default)]
pub struct FieldIndex {
    /// One lane per live row. Resident and reused; see [`FieldLane::touched`].
    lanes: Vec<FieldLane>,
    /// Lanes whose row went dark, kept at full width for the next row that
    /// lights: an Enter or a wrap re-lights a row every few seconds while
    /// typing, and minting a fresh `cols`-wide `Vec` for each one is exactly
    /// the per-frame allocation §18 forbids. Bounded by the most rows that
    /// were ever live at once.
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
    pub fn is_empty(&self) -> bool {
        self.lanes.iter().all(|l| l.touched.is_empty())
    }

    /// Rows carrying field light this frame — the "linear in LIVE ROWS" term
    /// of every lookup.
    #[must_use]
    pub fn live_rows(&self) -> usize {
        self.lanes.len()
    }

    /// Begin a frame: RETIRE the lanes for rows that carried no light last
    /// frame into the spare pool, and clear the rest over their own `touched`
    /// list.
    ///
    /// The prune is what keeps "linear in LIVE ROWS" true: without it a
    /// session that has typed on a thousand rows carries a thousand lanes and
    /// every `field_at` walks them all — v1's linear scan wearing a different
    /// hat. A grid resize drops every lane, live and spare, since none of them
    /// is the right width any more.
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

    /// The lane for `row`, taken from the spare pool if this row is newly lit
    /// and minted only when the pool is empty — the one allocation, paid once
    /// per "most rows ever live at once", never per frame.
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

    /// Write one cell's field. **FIRST WRITER WINS**, and the plan feeds this
    /// NEWEST-FIRST — which is the index's whole contract, stated in exactly
    /// the terms v1's reverse scan of `sparks` answered it in: the newest cell
    /// at a column owns that column, and an older cell it shadows can never be
    /// seen and is never asked about.
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
// The bed's ink (§3.1, §3.2, §3.3)
// ===========================================================================

/// One sRGB channel, 0..1, through the EOTF to LINEAR light.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The WCAG relative luminance of an `0x00RRGGBB` colour — the coordinate the
/// 5.25:1 bar is stated in, and therefore the only honest coordinate for a
/// ceiling that claims to keep it.
#[must_use]
pub fn relative_luminance(rgb: u32) -> f32 {
    let chan = |sh: u32| srgb_to_linear(((rgb >> sh) & 0xff) as f32 / 255.0);
    0.2126 * chan(16) + 0.7152 * chan(8) + 0.0722 * chan(0)
}

/// **THE BED'S ONE CEILING**, in relative luminance: the brightest a
/// source-over ribbon may composite and still leave `theme_fg` legible over it
/// at [`BODY_CONTRAST_BAR`] (plus [`BODY_CONTRAST_GUARD`]).
///
/// This is §19.1's "one ceiling + the ledger" made arithmetic. v1 carried a
/// 33-entry per-position coverage table BESIDE a byte cap — two ceilings for
/// one bed, solved by two different emitters, and they disagreed. There is one
/// bar here, it is stated in the coordinate the bar is actually written in,
/// and [`bed_ink`] is what spends it.
#[must_use]
pub fn bed_luma_budget(theme_fg: u32) -> f32 {
    let y = relative_luminance(theme_fg);
    ((y + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05)
        .clamp(BED_LUMA_MIN, BED_LUMA_MAX)
}

/// Every channel scaled by `k` — a uniform gamma-space scale, which is the one
/// operation that leaves a colour's channel RATIOS (its hue and saturation)
/// exactly where they were.
fn scale_rgb(rgb: u32, k: f32) -> u32 {
    let ch = |sh: u32| ((((rgb >> sh) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// A walk toward WHITE by `w`: `c + w·(255 − c)`. Every channel DIFFERENCE
/// survives unscaled, so the hue direction is untouched and only the
/// saturation falls — the same move v1's bed glass floor made, for the same
/// reason: a dark arc colour over a dark ground is a navy smudge, not a stop.
fn toward_white(rgb: u32, w: f32) -> u32 {
    let ch = |sh: u32| {
        let c = ((rgb >> sh) & 0xff) as f32;
        ((c + w * (255.0 - c)) + 0.5).clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Bisect a monotone one-parameter colour family for the parameter whose
/// relative luminance is `target`. 24 halvings resolve the parameter to under
/// 1e-7, which is finer than the byte quantization it feeds.
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
    family(0.5 * (lo + hi))
}

/// **THE BED'S ON-GLASS INK** — an arc colour put on ONE composited luminance,
/// `budget` (see [`bed_luma_budget`]).
///
/// Two arms, one bar:
///
/// * A colour BRIGHTER than the budget (red, yellow, green) is scaled down
///   uniformly, so its hue is bit-exact and only its value moves.
/// * A colour DARKER than it (blue, indigo, violet) is walked toward white
///   until it reaches the budget, so it arrives as a vivid stop instead of the
///   navy smudge the measured defect reports.
///
/// The consequence is that **every stop of the arc composites at the same
/// weight**, and one coverage ceiling is legal for the whole arc — which is
/// why the 33-entry table §19.1 deletes is not needed and is not carried. That
/// is the NAVY half of the measured "dim and muddy" defect (a dark stop as a
/// smudge) and not the OLIVE half: the weight is the bar's own, and on the
/// default theme a warm stop at that weight is `(78, 78, 2)` composited — the
/// luminance v1's table produced, with more chroma. A brighter warm mid is a
/// ruling on the bar (module doc; OPEN on the §22 sheet), not on this recipe.
///
/// It also makes the per-hue "equal-ledge" core law v1 needed unnecessary: the
/// per-row melt step is proportional to the position's peak premultiplied
/// level, and after this recipe every position has the same one.
#[must_use]
pub fn bed_ink(rgb: u32, budget: f32) -> u32 {
    let y = relative_luminance(rgb);
    if y > budget {
        solve_for_luma(|k| scale_rgb(rgb, k), budget)
    } else {
        solve_for_luma(|w| toward_white(rgb, w), budget)
    }
}

/// The light theme's compositing ROLE for the body (§3.3, L6). The tall
/// spelling stands over letterforms, so it takes the conservative ink and the
/// shared legibility ceiling; the underline spelling is provably confined to
/// the leading, so it may be vivid and go to the rail's own ceiling.
///
/// `pub(crate)` because `InkRole` is: the light-ink recipe is the family's,
/// not this module's, and a producer outside the crate has no business naming
/// a compositing role it cannot spell.
#[must_use]
pub(crate) fn light_role(cfg: &Config) -> InkRole {
    if cfg.ribbon_tall {
        InkRole::OverText
    } else {
        InkRole::Leading
    }
}

/// The bed's ink table for ONE theme — resident, rebuilt only when the theme
/// or the spelling changes.
///
/// [`bed_ink`] is a bisection over `powf`; there are up to `SLABS_PER_CELL`
/// vertices per cell per frame and it is a pure function of the arc position,
/// so it is solved once into this table and read with one lerp (§18: no dead
/// work, and nothing expensive on the steady frame path).
#[derive(Clone, Debug, Default)]
pub struct BedInkLut {
    /// `(theme_fg, theme_bg, dark_theme, ribbon_tall)` the table was solved
    /// for. A change in any of the four invalidates it.
    key: Option<(u32, u32, bool, bool)>,
    /// [`BED_INK_LUT_LEN`] entries over the folded arc `[0, 1]`.
    lut: Vec<u32>,
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
        for i in 0..BED_INK_LUT_LEN {
            let x = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
            let arc = spectrum(x);
            self.lut.push(if cfg.dark_theme {
                bed_ink(arc, budget)
            } else {
                role.ink(arc)
            });
        }
        self.key = Some(key);
    }

    /// The ink at walk position `t`. The walk is unbounded, so it is folded
    /// through `meteor::tri` — the SAME reflection §6.4's arc uses, which is
    /// what makes "the station under the caret equals the caret's stop" true
    /// by construction rather than by coincidence.
    #[must_use]
    pub fn at(&self, t: f32) -> u32 {
        if self.lut.is_empty() {
            return 0;
        }
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let i = (x as usize).min(BED_INK_LUT_LEN - 1);
        let j = (i + 1).min(BED_INK_LUT_LEN - 1);
        lerp_rgb(self.lut[i], self.lut[j], x - i as f32)
    }
}

// ===========================================================================
// The walk (C2)
// ===========================================================================

/// **THE CLASSIC WALK**, `t` at `d` cells from the mark's first cell: `d/16`
/// over the first [`WALK_FAST_CELLS`] cells so a short word already shows
/// several stops, then [`WALK_LAY_RATE`] per cell so a long line does not
/// spin through the arc.
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

/// The ONE linear fade of the theme (§2.5, §6.11): under reduced motion a
/// mark's alpha runs to zero linearly over the last
/// [`REDUCED_MOTION_FADE_MS`] of its life. `remaining_s` is how long the mark
/// has left; `1.0` until the window opens, exactly `0.0` at the end.
#[must_use]
pub fn reduced_fade(remaining_s: f32) -> f32 {
    clamp01(remaining_s / (REDUCED_MOTION_FADE_MS / 1000.0))
}

/// Seconds until [`expiry_melt`] next takes a mark of `peak` levels down
/// one u8 level, `u` of the way through its `life_s` — the cadence law's
/// "next visible step" for the melt, in closed form through the
/// smoothstep's own inverse (`x = ½ − sin(asin(1 − 2y)/3)`). Before the
/// melt opens this is the instant of its first level drop, so a static
/// cell asks for exactly the frame its light first moves. `None` once
/// nothing is left to see.
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

/// One contiguous planned run — a range of [`Ribbon::plan`] plus the columns
/// it covers, so [`Ribbon::band`] can answer a cell without a search and
/// [`Ribbon::emit`] can shed the run's TAIL when the budget runs out.
///
/// A run belongs to ONE cohort: [`Ribbon::build_runs`] breaks a run where the
/// cohort changes, so the exit swoosh of one cohort can never move the
/// boundaries of a neighbour that is still being typed.
#[derive(Clone, Copy, Debug)]
struct Run {
    row: u16,
    col0: u16,
    col1: u16,
    lo: usize,
    hi: usize,
    /// The head cell's column (see [`Ribbon::head_col`]).
    head_col: u16,
    /// Index into [`Ribbon::plan`] of the HEAD boundary — the head cell's
    /// RIGHT edge, where the hot edge ends.
    ///
    /// The head is always the run's right-hand side and the tail its left:
    /// a run is one cohort, and a cohort's walk is a monotone function of the
    /// column ([`Cohort::t_at`]), so the oldest light — the reach, the first
    /// typed cells — is always the left end. There is no "which end is the
    /// head" heuristic: one resolved from the head's POSITION flipped the tail
    /// onto the erased cells whenever a backspace carried the caret past the
    /// run's midpoint, and drew the hot edge over them.
    head: usize,
    /// True when the head cell is real, un-retracting light — the hot edge's
    /// "still wet at the hand" condition (§4.1). A run whose every cell is
    /// retracting (the whole word backspaced) has nothing wet in it.
    wet: bool,
    /// Whether the caret stands in or beside this run — the run the hand is
    /// on is emitted FIRST, so a saturated budget sheds the oldest row and
    /// never the head (§7.3: one key, one cell, one light).
    at_caret: bool,
    /// The newest typing cell's birth, the emit order's second key.
    born: Instant,
}

/// THE RIBBON producer: the laid cells, their cohorts, this frame's plan, and
/// the field index every other producer reads.
///
/// The pool, the plan and the index are PRIVATE: the index answers "what a
/// newest-first scan of the pool would" only while nothing edits the pool
/// behind it, and the read-only accessors ([`Ribbon::cells`],
/// [`Ribbon::cohorts`], [`Ribbon::plan_segments`], [`Ribbon::field`]) are
/// what tests and the other producers read.
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
    /// Vertices per cell THIS frame planned at — [`SLABS_PER_CELL`] or fewer
    /// under budget pressure (see [`Ribbon::slabs_per_cell`]). Every index
    /// into [`Ribbon::plan`] is stated in it.
    slabs: usize,
    /// The next cohort id to mint.
    next_cohort: u32,
    /// The caret cell as last observed — the anchor `field()` resolves
    /// against (seam point 4).
    caret: Option<(u16, u16)>,
    /// This frame's runs into [`Ribbon::plan`], in EMIT order (the run at the
    /// caret first, then newest first). Resident scratch.
    runs: Vec<Run>,
    /// `(row, col, cell index)` sort scratch — the ONE sort per frame the
    /// index and the geometry share (§18).
    sorted: Vec<(u16, u16, u32)>,
    /// `RibbonVertex` scratch, taken and returned by `mem::take` so the emit
    /// path allocates nothing.
    verts: Vec<RibbonVertex>,
    /// The bed's ink table for the live theme.
    ink: BedInkLut,
    /// Set while the window is unfocused: the ribbon embers out over
    /// [`FOCUS_EMBER_S`] on `spend` and nothing sounds (§8.2).
    ember_at: Option<Instant>,
    /// Focus REGAINED while the ember was still burning: `(when, level)`, the
    /// spend level the ribbon had at that instant. The light comes back up
    /// from that level through the one sanctioned attack, `edge-in`, rather
    /// than snapping to full — v1's `rearm`.
    rearm: Option<(Instant, f32)>,
    /// The reduced-motion posture of the last frame planned, cached for the
    /// cadence law (seam point 9 is asked with a clock and nothing else).
    reduced: bool,
    /// The focused pane's `(first column, width)` — the edges [`Ribbon::lay`]
    /// wraps a fold at — or `None` for the whole grid. Handed down by the
    /// engine every tick ([`super::Engine::set_pane`]).
    pane: Option<(u16, u16)>,
    /// The cell height of the last frame planned, px.
    cell_h: f32,
    /// The WEDGE's full travel in px — `(dn_ch − DN_FLOOR_CH)·ch` of the
    /// last frame's profile — the distance the mark's lower edge closes as
    /// the spine releases ([`Ribbon::settle_step_s`]).
    wedge_px: f32,
}

impl Ribbon {
    /// A ribbon with nothing laid.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// This frame's planned boundary samples, valid after [`Ribbon::plan`].
    /// Read-only.
    #[must_use]
    pub fn plan_segments(&self) -> &[Segment] {
        &self.plan
    }

    /// **THE FRAME'S VERTEX DENSITY** — how many [`Ribbon::plan_segments`]
    /// each cell of a run spans this frame, `1..=`[`SLABS_PER_CELL`]. Boundary
    /// `b` of a run whose first column is `col0` sits at
    /// `run.lo + (b − col0) · slabs_per_cell()`.
    ///
    /// The law (§18): the frame's `live cells × device rows × slabs` must fit
    /// [`RIBBON_QUAD_BUDGET`], and since `ribbon_beam` tiles at least one
    /// slab per vertex pair the density is the only thing that can give. It
    /// falls from three toward one as the live mark grows — a 76-cell hot line
    /// at retina plans at one — and the budget's truncation is reached only
    /// past that, where it sheds the OLDEST run's tail (see
    /// [`Ribbon::emit`]). Valid after [`Ribbon::plan`].
    #[must_use]
    pub fn slabs_per_cell(&self) -> usize {
        self.slabs.clamp(1, SLABS_PER_CELL)
    }

    /// The caret cell as last observed, or `None` before any.
    #[must_use]
    pub fn caret(&self) -> Option<(u16, u16)> {
        self.caret
    }

    /// **THE TWO PROFILES** (§4). The whole tall/underline fork, as data.
    #[must_use]
    pub fn body_profile(cfg: &Config) -> BodyProfile {
        if cfg.ribbon_tall {
            BodyProfile {
                up_ch: TALL_UP_CH,
                dn_ch: DN_TOP_CH,
                core_up_ch: TALL_CORE_CH,
                shoulder: SHOULDER_TALL,
            }
        } else {
            BodyProfile {
                up_ch: UNDERLINE_UP_CH,
                dn_ch: DN_TOP_CH,
                core_up_ch: LEAD_CH,
                shoulder: SHOULDER_UNDERLINE,
            }
        }
    }

    // -- ingest ------------------------------------------------------------

    /// Ingest one engine event at its own edge, BEFORE this frame's plan.
    ///
    /// [`Event::Typed`] extends the head cohort (or folds a wrap into a new
    /// one); [`Event::Erase`] retracts the SUFFIX of the row from the caret on
    /// — v1's law, kept: the lit run stays contiguous from the caret backward
    /// and nothing is ever removed from the middle, so no cell holds light
    /// over a glyph that has shifted; [`Event::Kill`] drains the same suffix,
    /// at least `cells` wide, farthest-first at `12·n + 240` ms (the Word /
    /// Line scope is the synth's, §8.2 — the drain is one law);
    /// [`Event::Move`] moves [`Ribbon::caret`] and, on a REAL jump under a
    /// non-typed licence (a row change, or [`JUMP_MIN_CELLS`] or more on the
    /// row), ABANDONS the live band straight into its retract (see
    /// [`RETRACT_START_S`]) — a one-cell arrow or a click inside the word is an
    /// edit, and the swoosh's own grace handles it; [`Event::Focus`] takes the
    /// 300 ms ember and, on regain inside it, the `edge-in` rearm;
    /// [`Event::ReducedMotion`] and [`Event::Return`] lay nothing — a jump lays
    /// nothing, which is T1.
    ///
    /// Every event is applied against `ctx.caret`, the caret as the host
    /// observed it for THIS tick; a host that buffers two typed echoes into
    /// one tick must coalesce them into one `Typed { cells: 2 }`, because the
    /// caret each of them landed at is not carried on the event.
    pub fn on_event(&mut self, ev: &super::Event, at: Instant, ctx: &Ctx<'_>) {
        match *ev {
            Event::Typed { cells, .. } => self.lay(cells, at, ctx),
            Event::Erase => self.retract_suffix(ctx.caret.0, ctx.caret.1, 1, at, RETRACT_FADE_S),
            Event::Kill { cells, .. } => {
                let span_s = 0.012 * f32::from(cells) + 0.240;
                self.retract_suffix(ctx.caret.0, ctx.caret.1, cells, at, span_s);
            }
            Event::Move {
                from, to, licence, ..
            } => {
                self.caret = Some(to);
                let jump = to.0 != from.0 || to.1.abs_diff(from.1) >= JUMP_MIN_CELLS;
                if licence != Licence::Typed && jump {
                    self.abandon(at);
                }
            }
            Event::Focus(on) => {
                if on {
                    self.regain_focus(at);
                } else {
                    self.ember_at = Some(at);
                    self.rearm = None;
                }
            }
            Event::Sweep { row, col0, col1 } => self.sweep(row, col0, col1, at, ctx),
            Event::Return | Event::ReducedMotion(_) => {}
        }
    }

    /// The focused pane's `(first column, width)`; `None` is the whole grid.
    pub fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// **A TYPED ECHO'S GLYPH CELLS** ([`Event::Sweep`]): lay every cell in
    /// `col0..col1` on `row` that no LIVE cell owns, as typing, born at the
    /// echo. A key lays at the caret the tick replays with, so a key whose
    /// echo shares its tick already owns its glyph cell and is left exactly
    /// as it was — per-key typing is byte-identical (the deletion goldens
    /// pin it). What the sweep lights is what the keys could not: the cells
    /// of a batched echo behind the landing's own, and the glyph of a key
    /// whose echo landed a frame late (the key laid the PREVIOUS glyph's
    /// cell, the caret's neighbour at key time).
    fn sweep(&mut self, row: u16, col0: u16, col1: u16, at: Instant, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        for col in col0..col1.min(cols) {
            let owned = self.cells.iter().any(|c| {
                c.row == row
                    && c.col == col
                    && c.retract_at.is_none()
                    && at.saturating_duration_since(c.born).as_secs_f32() < c.life_s
            });
            if !owned {
                self.lay_cell(row, col, at, ctx, true);
            }
        }
    }

    /// Focus came back. If the ember was still burning, carry the level it
    /// had reached into an `edge-in` rearm so the ribbon comes back UP through
    /// the one sanctioned attack instead of snapping to full in one frame
    /// (v1's `rearm`). A focus loss that already burned out has nothing to
    /// rearm — the pool was cleared by [`Ribbon::retire`].
    fn regain_focus(&mut self, at: Instant) {
        if let Some(ember) = self.ember_at.take() {
            let r = at.saturating_duration_since(ember).as_secs_f32() / FOCUS_EMBER_S;
            if r < 1.0 {
                self.rearm = Some((at, spend(clamp01(r))));
            }
        }
    }

    /// Lay `n` cells ending at the caret. The caret sits one PAST the last
    /// echoed glyph, so the cells are `[caret.col − n, caret.col)` — laid
    /// oldest-first so the head is the newest cell in the pool, which is what
    /// makes the field index's last-writer-wins rule equal to v1's newest-first
    /// scan.
    ///
    /// **THE WRAP FOLDS** (§4, kept): a cell whose column runs off the left
    /// edge lands at the right end of the row above, in a cohort of its own —
    /// a cohort never spans rows — and the new row's cohort CONTINUES the
    /// folded one's walk, so the arc crosses the fold without a seam.
    fn lay(&mut self, n: u16, at: Instant, ctx: &Ctx<'_>) {
        if n == 0 || ctx.geom.cols == 0 {
            return;
        }
        let cols = i32::try_from(ctx.geom.cols).unwrap_or(i32::MAX);
        // A fold wraps at the FOCUSED PANE's edges (`set_pane`), not the
        // grid's: a split pane's line ends at its own right margin, and the
        // cell before its first column is the previous row's LAST pane cell
        // — never the neighbouring pane's.
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

    /// The life one cell earns, priced at BIRTH (§4's chain law, verbatim).
    fn cell_life(&self, at: Instant, ctx: &Ctx<'_>) -> f32 {
        let full = ctx.cfg.duration.as_secs_f32().max(0.001);
        let base = (full * LIFE_DURATION_GAIN).clamp(LIFE_BASE_MIN, LIFE_BASE_MAX);
        let d = clamp01(ctx.birth_disp);
        let life = base * (1.0 + LIFE_SPINE_LINEAR * d + LIFE_SPINE_SQUARE * d * d);
        // THE FOUR-LETTER GUARANTEE. The observed inter-key gap is the newest
        // cohort's own idle time; the FIRST key of a burst has no gap and so
        // takes no floor, which is what lets a lone keystroke fade crisply.
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
        life.max(SWOOSH_LIFE_S)
    }

    /// Lay ONE cell, joining or minting its cohort.
    fn lay_cell(&mut self, row: u16, col: u16, at: Instant, ctx: &Ctx<'_>, typing: bool) {
        let life_s = self.cell_life(at, ctx);
        let birth_disp = clamp01(ctx.birth_disp);
        let cov0 = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let idx = self.join_cohort(row, col, at);
        let cohort = self.cohorts[idx];
        // THE WALK IS A FUNCTION OF POSITION (C2, §4): the cell takes the stop
        // its column has in its cohort, from the cohort's immutable origin.
        // Chaining from "the highest t in the pool" instead would hand a cell
        // retyped after a backspace the stop of the erased cell to its right
        // plus one, and leave a colour seam when the erased cells fade.
        let cell = Cell {
            row,
            col,
            cohort: cohort.id,
            t: cohort.t_at(col),
            born: at,
            life_s,
            cov0,
            typing,
            retract_at: None,
            birth_disp,
        };
        // ONE OWNER PER CELL. A retype replaces the light on that cell rather
        // than stacking a second body under it — the shadowed cell could never
        // be seen, and leaving it in the pool is how v1's field scan grew.
        if let Some(slot) = self
            .cells
            .iter()
            .position(|c| c.row == row && c.col == col && c.cohort == cohort.id)
        {
            self.cells.remove(slot);
        }
        self.cells.push(cell);
        let coh = &mut self.cohorts[idx];
        coh.col0 = coh.col0.min(col);
        coh.col1 = coh.col1.max(col + 1);
        if typing {
            // ONE FINGER: a live key holds EVERY cohort that has not been
            // abandoned in its laying phase, so the mark's earlier rows do
            // not start their exit swoosh while the hand is still typing on
            // the current one (see `Cohort::alive_at`).
            for held in &mut self.cohorts {
                if !held.abandoned {
                    held.alive_at = at;
                    held.phase = Phase::Laying;
                }
            }
        }
    }

    /// The cohort this cell joins: one on the same row, adjacent to or
    /// containing `col`, still inside the chain window. Otherwise a new one —
    /// whose walk CONTINUES the freshest live cohort's from one past its
    /// rightmost column, so a wrap fold and a same-row rebirth both cross
    /// without a seam, and a genuinely cold start re-anchors at red.
    fn join_cohort(&mut self, row: u16, col: u16, at: Instant) -> usize {
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && !c.abandoned
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && col + 1 >= c.col0
                && col <= c.col1
        }) {
            return i;
        }
        let t0 = self
            .cohorts
            .iter()
            .filter(|c| at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX)
            .max_by_key(|c| c.alive_at)
            .map_or(0.0, |c| c.t_at(c.col1));
        let id = self.next_cohort;
        self.next_cohort = self.next_cohort.wrapping_add(1);
        self.cohorts.push(Cohort {
            id,
            row,
            col0: col,
            col1: col + 1,
            anchor_col: col,
            t0,
            born: at,
            alive_at: at,
            abandoned: false,
            phase: Phase::Laying,
        });
        self.cohorts.len() - 1
    }

    /// Retract the SUFFIX of `row` from `col` on — every live cell at or right
    /// of the caret, and at least `min_cells` columns — FARTHEST-FIRST inside
    /// `span_s`, each cell then spending [`RETRACT_FADE_S`] to reach exactly
    /// zero. The backspace's `2 × 0.24 s` and the kill's `12·n + 240 ms` are
    /// the same law with two spans (§4, v1 verbatim: "every live ribbon cell on
    /// the landing row at or beyond the new caret is stamped … CONTIGUOUS at
    /// every frame … nothing is ever removed from the MIDDLE").
    fn retract_suffix(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        let far = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.col >= col && c.retract_at.is_none())
            .map(|c| c.col)
            .max();
        let last = far
            .unwrap_or(col)
            .max(col.saturating_add(min_cells.saturating_sub(1)));
        let n = f32::from(last - col + 1);
        let step = span_s / n;
        for cell in &mut self.cells {
            if cell.row != row || cell.col < col || cell.col > last || cell.retract_at.is_some() {
                continue;
            }
            // Farthest from the caret goes first, so the mark visibly slurps
            // back toward the hand rather than blinking out.
            let rank = f32::from(last - cell.col);
            cell.retract_at = at.checked_add(std::time::Duration::from_secs_f32(rank * step));
        }
    }

    /// A real jump ABANDONS the band: nothing lays here, and every cohort
    /// still in its grace or reach is sent straight into its retract by
    /// rewinding `alive_at` to [`RETRACT_START_S`] ago. The light leaves
    /// CONTINUOUSLY from the level it has (the retract's `spend` starts at
    /// exactly `1.0`), where v1's life clamp stepped the melt on the jump
    /// frame; and a cohort already retracting is left on its own clock.
    fn abandon(&mut self, at: Instant) {
        let Some(rewound) = at.checked_sub(std::time::Duration::from_secs_f32(RETRACT_START_S))
        else {
            return;
        };
        for coh in &mut self.cohorts {
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
    }

    // -- the frame ---------------------------------------------------------

    /// Build this frame's [`Ribbon::plan`] and [`FieldIndex`] — the geometry
    /// pass, run once per tick BEFORE any producer emits and before stardust
    /// deals a birth, because §5.4's zones are relative to [`Band::top`].
    ///
    /// The order is fixed and each step depends on the one before it: sync the
    /// ink table for the live theme, advance the exit swoosh (which may lay
    /// the reach's extension cells), retire what is out of time, then sort
    /// ONCE and fill the index and the geometry from that same pass (§18).
    pub fn plan(&mut self, ctx: &Ctx<'_>) {
        self.ink.sync(ctx.cfg);
        self.caret = Some(ctx.caret);
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        self.wedge_px = (Self::body_profile(ctx.cfg).dn_ch - DN_FLOOR_CH).max(0.0) * self.cell_h;
        self.advance_swoosh(ctx);
        self.retire(ctx);
        self.build(ctx);
    }

    /// Drive the exit choreography and lay the reach's extension cells.
    ///
    /// The beats are staged in a FIXED array rather than a `Vec`: the reach
    /// can only ever want [`REACH_BEATS`] cells per cohort and §18's rule is
    /// that nothing on the frame path allocates, not that nothing on the
    /// STEADY frame path does.
    fn advance_swoosh(&mut self, ctx: &Ctx<'_>) {
        let now = ctx.now;
        let mut reach = [(0u16, 0u16, 0u32); REACH_STAGE];
        let mut n_reach = 0usize;
        for i in 0..self.cohorts.len() {
            let coh = self.cohorts[i];
            let idle = now.saturating_duration_since(coh.alive_at).as_secs_f32();
            let Some(phase) = Phase::at(idle) else {
                self.cohorts[i].phase = Phase::Fading;
                continue;
            };
            self.cohorts[i].phase = phase;
            // Under reduced motion the mark is STATIC (§6.11): no reach.
            if phase != Phase::Reaching || ctx.cfg.reduced_motion {
                continue;
            }
            // ONE EXTENSION CELL PER BEAT while the mark is still short of the
            // four-letter span. They may overlap letters the burst never typed
            // — accepted by design; the ribbon is source-over and capped.
            let beats = (((idle - LIFT_GRACE_S) / REACH_STEP_S).floor() as i32 + 1)
                .clamp(0, i32::from(REACH_BEATS)) as u16;
            let span = coh.col1.saturating_sub(coh.col0);
            let want = FOUR_LETTER_CELLS.saturating_sub(span).min(beats);
            for k in 0..want {
                let Some(c) = coh.col0.checked_sub(k + 1) else {
                    break;
                };
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
    /// It INHERITS the mark's own envelope — `born`, `life_s` and `cov0` come
    /// from ITS OWN cohort's tail cell — for two reasons. It is one object
    /// with the mark, so it must die with it; and because its light can
    /// therefore never exceed the light already on glass, the reach cannot
    /// brighten a ribbon whose hand has already left (the `never brightens`
    /// law). Its stop is the cohort's walk read one cell further back.
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
            life_s: tail.life_s,
            cov0: tail.cov0,
            typing: false,
            retract_at: tail.retract_at,
            birth_disp: tail.birth_disp,
        });
        if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cohort) {
            coh.col0 = coh.col0.min(col);
        }
    }

    /// Drop what is out of time. A cell that has expired is REMOVED here, not
    /// left in place for a deadline read to notice, which is what lets
    /// [`Ribbon::at_rest`] answer "is anything visible" without a clock.
    fn retire(&mut self, ctx: &Ctx<'_>) {
        let now = ctx.now;
        let ember_done = self
            .ember_at
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() >= FOCUS_EMBER_S);
        if ember_done {
            self.cells.clear();
            self.cohorts.clear();
            return;
        }
        let cohorts = &self.cohorts;
        self.cells.retain(|cell| {
            if now.saturating_duration_since(cell.born).as_secs_f32() >= cell.life_s {
                return false;
            }
            if let Some(at) = cell.retract_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETRACT_FADE_S
            {
                return false;
            }
            cohorts.iter().any(|c| {
                c.id == cell.cohort
                    && Phase::at(now.saturating_duration_since(c.alive_at).as_secs_f32()).is_some()
            })
        });
        let cells = &self.cells;
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
    }

    /// The plan and the index, from ONE sort.
    fn build(&mut self, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        self.plan.clear();
        self.runs.clear();
        self.index.begin(cols);
        if self.cells.is_empty() {
            return;
        }
        // ONE SORT, and it is a NEWEST-FIRST sort: `row` and `col` ascending,
        // then the pool index DESCENDING. The pool is append-ordered, so the
        // first entry of a `(row, col)` group is that cell's newest — its
        // owner — and both the index (first writer wins) and the geometry
        // (`dedup_by` keeps the first of a group) take the same one.
        //
        // Sorting the other way and letting the LAST writer win would give the
        // index the right answer and hand the GEOMETRY the shadowed cell,
        // which is a bug that draws one colour and reports another.
        let mut sorted = mem::take(&mut self.sorted);
        sorted.clear();
        sorted.reserve(self.cells.len());
        for (i, c) in self.cells.iter().enumerate() {
            sorted.push((c.row, c.col, u32::try_from(i).unwrap_or(u32::MAX)));
        }
        sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)));
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
        // EMIT ORDER: the run the hand is on first, then newest first. The
        // budget sheds from the END of the emit, so this is what makes "shed
        // from the tail, never from the head" true ACROSS rows and not only
        // within one: a hot wrapped paragraph keeps its current row lit and
        // loses the oldest row's far end, never the other way round.
        self.runs
            .sort_unstable_by(|a, b| b.at_caret.cmp(&a.at_caret).then(b.born.cmp(&a.born)));
    }

    /// The vertex density the budget allows this frame (§18), from the
    /// frame's TOTAL live cells across every row: the transverse profile
    /// costs one quad per device row of every slab, and `ribbon_beam` tiles
    /// at least one slab per vertex pair, so the frame's cost is
    /// `cells × slabs × rows` and `slabs` is the one term that can give.
    /// Sized against the total rather than per run, so two hot rows fit the
    /// same budget one does instead of each spending it in full.
    fn slabs_for(ctx: &Ctx<'_>, cells: usize) -> usize {
        let prof = Self::body_profile(ctx.cfg);
        let rows = ((prof.up_ch + prof.dn_ch) * ctx.geom.ch as f32)
            .ceil()
            .max(1.0)
            + 2.0;
        let per_slab = (cells as f32 * rows).max(1.0);
        let fit = (RIBBON_QUAD_BUDGET as f32 / per_slab).floor();
        (fit.max(1.0) as usize).clamp(1, SLABS_PER_CELL)
    }

    /// Walk the deduped index into contiguous runs — contiguous in COLUMN and
    /// in COHORT — and plan each one.
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
            self.plan_run(ctx, &sorted[s..=e]);
            s = e + 1;
        }
    }

    /// The per-cell sample every boundary is the midpoint of: `(spine, up,
    /// dn, cov)`. The plateau is NOT sampled here — it is a constant of
    /// [`BodyProfile`], and computing it per cell would be exactly the dead
    /// per-station work §18 deletes.
    fn sample(&self, ctx: &Ctx<'_>, cell: &Cell, sp: f32) -> (f32, f32, f32, f32) {
        let chf = ctx.geom.ch as f32;
        let prof = Self::body_profile(ctx.cfg);
        let reduced = ctx.cfg.reduced_motion;
        // THE WAVE rides the LIVE spine, and only ever settles: its amplitude
        // is `0.055 ch · disp`, so when the hand lifts the mark flattens. The
        // head is pinned (`WAVE_HEAD_PIN`) so the cell under the hand never
        // moves. Under reduced motion there is no wave at all (§6.11).
        let room = (DN_TOP_CH - DN_FLOOR_CH) * chf;
        let amp = if reduced {
            0.0
        } else {
            (WAVE_AMP_CELLS * chf * clamp01(ctx.disp)).min(room) * smoothstep01(sp / WAVE_HEAD_PIN)
        };
        let wave = ((sp * std::f32::consts::TAU * WAVE_CYCLES) - ctx.phase * std::f32::consts::TAU)
            .sin()
            * amp;
        // THE WEDGE: fatter under the hand, settling into the leading behind
        // it. Shape, not light.
        let behind = sp * self.mark_span(cell);
        let bloom = clamp01(ctx.disp) * (1.0 - smoothstep01(behind / BLOOM_REACH_CELLS));
        let dn_open = chf * (DN_FLOOR_CH + (prof.dn_ch - DN_FLOOR_CH) * clamp01(bloom));
        let dn = (dn_open - wave.max(0.0)).max(DN_FLOOR_CH * chf);
        let up = (prof.up_ch * chf + wave.min(0.0)).max(0.0);
        let spine = (f32::from(ctx.geom.origin_y) + (f32::from(cell.row) + 1.0) * chf + wave)
            .min(ctx.geom.fx_bot() as f32 - dn)
            .max(ctx.geom.fx_top() as f32);
        (spine, up, dn, self.cov_of(ctx, cell))
    }

    /// The mark's own length in cells — the denominator `sp` was normalized
    /// by, so `sp · span` is "how many cells behind the head this one is",
    /// which is the coordinate the wedge is stated in.
    fn mark_span(&self, cell: &Cell) -> f32 {
        self.cohorts
            .iter()
            .find(|c| c.id == cell.cohort)
            .map_or(1.0, |c| f32::from(c.col1.saturating_sub(c.col0)).max(1.0))
    }

    /// **THE CELL'S ENVELOPE**, `0..1` — everything that opens and closes a
    /// cell's light AFTER its birth price: the 18 ms `edge-in`, the expiry
    /// melt, the retract's `spend`, the focus ember and its `edge-in` rearm,
    /// and the exit swoosh's farthest-first drain. It is the ONE clock the
    /// body and the hot edge share: [`Ribbon::cov_of`] multiplies it into the
    /// body's request, and [`Ribbon::emit_hot_edge`] multiplies the HEAD
    /// cell's into the hairline's gain, so the white line embers, spends,
    /// melts and swooshes WITH the body under it and reaches exactly zero
    /// with it. A hairline priced from `birth_disp` alone rode the retract at
    /// full coverage over a body that had spent to nothing and then snapped
    /// off when the cells retired — a white line with no keystroke behind it,
    /// which is the anti-stray law's own complaint.
    ///
    /// Except for the two sanctioned attacks (`edge-in` at birth, and the
    /// rearm's `edge-in` on focus regain) every term is non-increasing in
    /// `now`, which is what lets "never brightens after the last keystroke"
    /// hold for the hairline by construction.
    fn env_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        let now = ctx.now;
        let reduced = ctx.cfg.reduced_motion;
        let age = now.saturating_duration_since(cell.born).as_secs_f32();
        let u = (age / cell.life_s.max(1e-3)).clamp(0.0, 1.0);
        // UNDER REDUCED MOTION every mark is static and takes the theme's ONE
        // linear fade over the last `REDUCED_MOTION_FADE_MS` of whatever ends
        // it (§6.11) — the natural life, the retract stamp, the ember and the
        // swoosh all resolve through `reduced_fade`; the 18 ms `edge-in`
        // stays, because it is the one sanctioned attack and not a motion.
        let mut env = (BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age))
            * if reduced {
                reduced_fade(cell.life_s - age)
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
        if let Some(at) = self.ember_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(FOCUS_EMBER_S - r)
            } else {
                spend(clamp01(r / FOCUS_EMBER_S))
            };
        }
        if let Some((at, level)) = self.rearm {
            // Focus regained mid-ember: back up from the level it had, on the
            // one sanctioned attack, so the return is continuous.
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= level + (1.0 - level) * edge_in(r);
        }
        if let Some(coh) = self.cohorts.iter().find(|c| c.id == cell.cohort)
            && coh.phase.is_retracting()
        {
            let idle = now.saturating_duration_since(coh.alive_at).as_secs_f32();
            if reduced {
                env *= reduced_fade(SWOOSH_TOTAL_S - idle);
            } else {
                let span = f32::from(coh.col1.saturating_sub(coh.col0)).max(1.0);
                // FARTHEST-FIRST, on the retract's own schedule: the cell
                // FURTHEST FROM THE HEAD starts spending first and each takes
                // `RETRACT_FADE_S` to reach exactly zero, so the mark is drawn
                // back INTO the hand. Draining head-first would be the same
                // arithmetic reading the mark backwards, and it looks like the
                // ribbon abandoning the caret.
                let from_head =
                    f32::from(coh.col1.saturating_sub(1).saturating_sub(cell.col)) / span;
                let t0 = RETRACT_START_S + RETRACT_DUR_S * (1.0 - from_head);
                env *= spend(clamp01((idle - t0) / RETRACT_FADE_S));
            }
        }
        env
    }

    /// **THE CELL'S COVERAGE** — the flat body (head floor 1.0, crest gain
    /// 0.0) at its birth-priced ceiling ([`Cell::cov0`]), closed by
    /// [`Ribbon::env_of`], and scaled by the host's intensity BEFORE the cap
    /// (§3.4: requests are clipped by the ledger, never by ad-hoc caps — this
    /// is the bed's own published ceiling, and the only one this producer
    /// applies).
    fn cov_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        let cap = if ctx.cfg.dark_theme {
            UNDER_COV_CAP
        } else {
            light_role(ctx.cfg).alpha_cap()
        };
        (cap * cell.cov0 * self.env_of(ctx, cell) * clamp01(ctx.cfg.intensity)).clamp(0.0, cap)
    }

    /// The run's own retract: every boundary of a RETRACTING cohort is pulled
    /// toward the caret on `suck-in`, so the mark shortens INTO the hand (T5)
    /// rather than fading in place, and at `u = 1` every boundary is the
    /// caret's own x — a run of exactly zero length, which draws exactly
    /// nothing. Per COHORT, never per row: a cohort still being typed beside
    /// one that is swooshing keeps every boundary where its letters are.
    /// Under reduced motion nothing moves (§6.11).
    fn retract_x(&self, ctx: &Ctx<'_>, cohort: u32, x: f32) -> f32 {
        if ctx.cfg.reduced_motion {
            return x;
        }
        let Some(coh) = self
            .cohorts
            .iter()
            .find(|c| c.id == cohort && c.phase.is_retracting())
        else {
            return x;
        };
        let idle = ctx
            .now
            .saturating_duration_since(coh.alive_at)
            .as_secs_f32();
        let u = clamp01((idle - RETRACT_START_S) / (RETRACT_DUR_S + RETRACT_FADE_S));
        let caret_x = self.caret.map_or(x, |(_, col)| {
            f32::from(ctx.geom.origin_x) + (f32::from(col) + 0.5) * ctx.geom.cw as f32
        });
        caret_x + (x - caret_x) * (1.0 - suck_in(u))
    }

    /// **THE HEAD OF A RUN** (§4.1 "a property of the head's position", §20.1
    /// "ends at the head cell every frame"): `(head column, at caret, wet)`.
    ///
    /// The head is the cell under the hand — the caret's own cell (`caret −
    /// 1`) when the caret stands in or just past the run and that cell is
    /// live, un-retracting light, whatever was typed and erased to its right.
    /// Otherwise it is the run's newest cell that is not retracting. A run
    /// with no such cell (the whole word backspaced, every cell draining) has
    /// its head at its right end and is NOT wet: nothing at the hand is still
    /// being laid, so nothing there is still wet, and the hot edge stays off
    /// it. Resolving the head from the highest `t` parked the hot edge on the
    /// fading erased cells for up to half a second after a backspace and then
    /// jumped it left.
    ///
    /// `at caret` is the emit-order key (the run the hand is on goes first)
    /// and is true whenever the caret is in or beside the run, wet or not.
    fn head_col(&self, ctx: &Ctx<'_>, run: &[(u16, u16, u32)]) -> (u16, bool, bool) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let (crow, ccol) = ctx.caret;
        let at_caret = crow == row && (col0..=col1 + 1).contains(&ccol);
        let live = |col: u16| -> bool {
            run.get(usize::from(col - col0))
                .and_then(|e| self.cells.get(e.2 as usize))
                .is_some_and(|c| c.col == col && c.retract_at.is_none())
        };
        if at_caret
            && let Some(own) = ccol.checked_sub(1)
            && (col0..=col1).contains(&own)
            && live(own)
        {
            return (own, true, true);
        }
        let newest = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| c.retract_at.is_none())
            .max_by(|a, b| a.born.cmp(&b.born))
            .map(|c| c.col);
        match newest {
            Some(col) => (col, at_caret, true),
            None => (col1, at_caret, false),
        }
    }

    /// Plan ONE contiguous run into [`Ribbon::plan`], boundary by boundary,
    /// with [`Ribbon::slabs_per_cell`] vertices per cell.
    fn plan_run(&mut self, ctx: &Ctx<'_>, run: &[(u16, u16, u32)]) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let Some(cohort) = self.cells.get(run[0].2 as usize).map(|c| c.cohort) else {
            return;
        };
        let slabs = self.slabs_per_cell();
        let (head_col, at_caret, wet) = self.head_col(ctx, run);
        let born = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| c.typing)
            .map(|c| c.born)
            .max()
            .unwrap_or(ctx.now);
        let span = f32::from(col1 - col0).max(1.0);
        // The run is contiguous and column-sorted, so a cell is an INDEX, not
        // a search: v1's per-boundary scan is exactly the shape of work §18
        // deletes, and it is the same shape twice per boundary here.
        let cell_at = |col: u16| -> Option<&Cell> {
            let i = usize::from(col.checked_sub(col0)?);
            let e = run.get(i)?;
            (e.1 == col).then(|| self.cells.get(e.2 as usize))?
        };
        let cw = ctx.geom.cw as f32;
        let lo = self.plan.len();
        let mut prev: Option<(f32, Sample)> = None;
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
                (None, None) => continue,
            };
            let sp = |c: &Cell| f32::from(head_col.abs_diff(c.col)) / span;
            let sa = self.sample(ctx, a, sp(a));
            let sb = self.sample(ctx, b, sp(b));
            let mid = |u: f32, v: f32| (u + v) * 0.5;
            // THE TAIL EASE: the mark's oldest outer boundary — the LEFT end,
            // always (see `Run::head`) — keeps a tenth of its coverage and the
            // first cell's slabs ramp it back to full, so the band ends
            // through a one-cell feather instead of a cliff.
            let ease = if left.is_none() { RUN_TAIL_EASE } else { 1.0 };
            let here = (
                mid(sa.0, sb.0),
                mid(sa.1, sb.1),
                mid(sa.2, sb.2),
                mid(a.t, b.t),
                mid(sa.3, sb.3) * ease,
            );
            let x = f32::from(ctx.geom.origin_x) + boundary as f32 * cw;
            if let Some((px, p)) = prev {
                for j in 1..slabs {
                    let f = j as f32 / slabs as f32;
                    let l = |u: f32, v: f32| u + (v - u) * f;
                    self.plan.push(Segment {
                        // ROUNDED TO THE PIXEL LATTICE: `ribbon_beam` tiles
                        // half-open at `ceil`, and a fractional interior vertex
                        // made the two segments either side of it co-own a
                        // column — which a source-over bed composites twice.
                        x: self.retract_x(ctx, cohort, l(px, x)).round(),
                        spine: l(p.0, here.0),
                        up: l(p.1, here.1),
                        dn: l(p.2, here.2),
                        t: l(p.3, here.3),
                        cov: l(p.4, here.4).clamp(0.0, 255.0) as u8,
                    });
                }
            }
            self.plan.push(Segment {
                x: self.retract_x(ctx, cohort, x).round(),
                spine: here.0,
                up: here.1,
                dn: here.2,
                t: here.3,
                cov: here.4.clamp(0.0, 255.0) as u8,
            });
            prev = Some((x, here));
        }
        let hi = self.plan.len();
        if hi > lo {
            // The head BOUNDARY: the head cell's RIGHT edge. Boundary `b` of
            // the run sits at `lo + (b − col0) · slabs`.
            let head_boundary = usize::from(head_col - col0) + 1;
            let head = (lo + head_boundary * slabs).min(hi - 1);
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
            });
        }
    }

    // -- emit --------------------------------------------------------------

    /// Draw the ribbon body into `frame.under` and the hot edge into
    /// `frame.out`.
    ///
    /// Called THIRD in the emit order — after the meteor (§6.5's bolts-first
    /// law) and before stardust — so a truncation sheds sky before it sheds
    /// the laid rainbow.
    ///
    /// ONE `aterm_render::ribbon_beam` CALL PER RUN, with the shoulder taken
    /// from [`BodyProfile`] and `GlowBlend::Over` on both grounds: the bed is
    /// the one stream in the family that composites SOURCE-OVER, which is why
    /// its ceiling is a luminance rather than an additive budget, and why the
    /// light theme's fork is a change of INK and of nothing else (§3.3, L6:
    /// never additive light on white).
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        if self.plan.is_empty() || self.runs.is_empty() {
            return;
        }
        let shoulder = Self::body_profile(ctx.cfg).shoulder;
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
        // ONE SLAB PER VERTEX PAIR: the plan already put a vertex on every
        // slab boundary (`slabs_per_cell`), so the stride is the slab's own
        // width and the density the budget chose is the density that prints.
        let stride = ctx.geom.cw.max(1).div_ceil(self.slabs_per_cell());
        // `runs` is in EMIT ORDER — the run at the caret, then newest first —
        // so when the budget runs out it is the OLDEST row's far end that is
        // shed (see `build`).
        for run in &runs {
            verts.clear();
            verts.reserve(run.hi - run.lo);
            for seg in &self.plan[run.lo..run.hi] {
                // THE STRIP IS PRICED OUT OF THE SAME CEILING THE BODY IS,
                // never beside it: `cov + lift` is what the spine actually
                // composites, so a strip clamped against the byte instead of
                // against the frame top is a second, higher ceiling — which is
                // exactly the "two ceilings for one bed" §19.1 deletes.
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
                    color: self.ink.at(seg.t),
                    cov: f32::from(seg.cov),
                    lift: strip,
                    lift_span,
                });
            }
            // HEAD FIRST: `ribbon_beam` walks the polyline in order and stops
            // at the budget, so starting from the run's RIGHT end — the head's
            // side, always (see `Run::head`) — sheds the left, older light
            // when it runs out.
            verts.reverse();
            if !ribbon_beam(
                frame.under,
                clip,
                &verts,
                shoulder,
                stride,
                budget,
                GlowBlend::Over,
            ) {
                break;
            }
        }
        self.runs = runs;
        self.verts = verts;
        self.emit_hot_edge(ctx, frame);
    }

    /// The gain of §4.1's hot edge, `0..1`, from the spine the head cell was
    /// BORN at. **THE A/B GATE**: the owner has struck a "highlighter" line
    /// twice and this is the first item on the A/B sheet (§22), so the whole
    /// refinement is one `0.0` away from gone — dark themes only, and only
    /// while the hand is on the mark.
    ///
    /// Priced from the head cell's [`Cell::birth_disp`], never the live
    /// follower: a hairline read off the follower brightens for ~100 ms after
    /// the last key while the attack finishes, which is light with no
    /// keystroke behind it. This is the PRICE only; [`Ribbon::emit_hot_edge`]
    /// multiplies in the head cell's envelope ([`Ribbon::env_of`]) so the
    /// hairline goes out with its body.
    #[must_use]
    pub fn hot_edge_gain(cfg: &Config, disp: f32) -> f32 {
        if !cfg.dark_theme || cfg.reduced_motion {
            return 0.0;
        }
        smoothstep01((clamp01(disp) - HOT_EDGE_DISP_MIN) / HOT_EDGE_DISP_SPAN)
    }

    /// **REFINEMENT A** (§4.1) — a 1-px `#FFFFFF` additive hairline along the
    /// ribbon's TOP edge, from the head cell back [`HOT_EDGE_CELLS`] cells.
    ///
    /// It is drawn as a 2-vertex-per-cell `comet_beam` hairline (one vertex
    /// per cell BOUNDARY) sampled at the same `spine − up` the body used,
    /// because the top edge carries the 0.75-cycle wave (≈ 1 px at `ch 18`,
    /// 2 px at retina) and three axis-aligned rects cannot follow it. It is
    /// SPATIAL rather than temporal — a property of where the head IS, with no
    /// clock of its own — so it can never lag the head: it ends at the head
    /// boundary [`Run::head`] resolves and reaches back only toward the tail.
    /// The one clock it does ride is the head cell's own
    /// ([`Ribbon::env_of`]): its gain is the birth-priced
    /// [`Ribbon::hot_edge_gain`] times the head cell's envelope, so it can
    /// never rise after the last key and it reaches exactly zero WITH the
    /// body — through the focus ember, the expiry melt and the exit swoosh
    /// alike — instead of holding at full coverage over a spent body and
    /// snapping off when the cells retire.
    fn emit_hot_edge(&self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        // THE RUN THE HAND IS ON, not merely the last one planned: the hot
        // edge is the head's own property, so it belongs to the run the caret
        // stands in, and failing that to the run carrying the newest cell.
        // `runs` is in emit order, which is exactly that order.
        let Some(run) = self.runs.first() else {
            return;
        };
        // "STILL WET AT THE HAND": the head cell itself, and only while it is
        // live light — a run whose every cell is draining has nothing wet.
        // The gain is priced from THAT cell's birth, not from the run's
        // newest-born cell, which after a backspace is the erased one.
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
        // BIRTH-PRICED so it can never rise; ENVELOPE-SCALED so it goes out
        // with its body. The head cell's envelope is the mark's own clock at
        // the hand — the ember, the expiry melt, the swoosh's drain — and a
        // hairline that ignored it rode the retract at full 38 over a body
        // that had spent to zero, then snapped off with the cells.
        let gain = Self::hot_edge_gain(ctx.cfg, head_cell.birth_disp) * self.env_of(ctx, head_cell);
        if gain <= 0.0 {
            return;
        }
        let head_x = self.plan[run.head].x;
        let reach = HOT_EDGE_CELLS * ctx.geom.cw as f32;
        frame.beams.clear();
        for seg in self.plan[run.lo..run.hi]
            .iter()
            .step_by(self.slabs_per_cell())
        {
            // Only the TAIL side of the head — its left (see `Run::head`):
            // behind the hand, never over the cells it has erased.
            let behind = head_x - seg.x;
            if !(0.0..=reach).contains(&behind) {
                continue;
            }
            let d = behind / ctx.geom.cw as f32;
            let a = HOT_EDGE_ALPHA * (1.0 - d / HOT_EDGE_CELLS).powi(2);
            let cov = (HOT_EDGE_COV_MAX * (a / HOT_EDGE_ALPHA) * gain * clamp01(ctx.cfg.intensity))
                .clamp(0.0, HOT_EDGE_COV_MAX);
            frame.beams.push(BeamVertex {
                x: seg.x,
                y: seg.spine - seg.up,
                color: 0x00FF_FFFF,
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
    pub fn field(&self) -> &FieldIndex {
        &self.index
    }

    /// The field at a cell, or `None`.
    #[must_use]
    pub fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.index.at(row, col)
    }

    /// **THE CARET'S POSITION IN THE FIELD** (v1's `rainbow_field`, seam point
    /// 4). The caret's own cell when it owns light; otherwise the newest cell
    /// REAL TYPING laid on the caret's row, then on any row (v1's "newest
    /// typing spark" — a reach cell or a retracting cell of an older, higher-t
    /// row is not the hand's colour); red when nothing is laid.
    ///
    /// Also §6.4's `t_land`: the meteor phase-locks its arc to THIS value at
    /// its spawn edge, which is why "the station under the caret is the
    /// caret's own stop by construction".
    #[must_use]
    pub fn field_at_caret(&self) -> f32 {
        if let Some(t) = self.caret.and_then(|(row, col)| self.field_at(row, col)) {
            return t;
        }
        let newest_typing = |row: Option<u16>| {
            self.cells
                .iter()
                .filter(|c| c.typing && c.retract_at.is_none() && row.is_none_or(|r| c.row == r))
                .max_by(|a, b| a.born.cmp(&b.born))
                .map(|c| c.t)
        };
        newest_typing(self.caret.map(|(row, _)| row))
            .or_else(|| newest_typing(None))
            .unwrap_or(0.0)
    }

    /// The source RGB of the ribbon HEAD's body, before premultiplication —
    /// what a companion cursor inherits to meet the laid ribbon with no
    /// palette seam (v1's `rainbow_head_rgb`, seam point 5). `None` before any
    /// cursor has been observed.
    ///
    /// **Dark themes return the AUTHORED spectrum colour** at the caret's
    /// field stop, exactly as v1 did — the consumer applies its own ink: the
    /// caret block lifts the stop to its light floor (§3.2 rank 3), and a
    /// companion inherits the stop, not the bed's dimmed on-glass ink. Handing
    /// out [`bed_ink`] here would paint a yellow caret olive. Light themes
    /// apply the body's own leading-ink recipe to that same position, so the
    /// caret meets the rail with no seam. Both arms resolve
    /// [`Ribbon::field_at_caret`] once, folded through the same `tri` the bed
    /// uses (C2).
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.caret?;
        let arc = spectrum(clamp01(tri(self.field_at_caret())));
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
    /// **REFINEMENT B'S PUBLICATION.** Stardust's sky band is
    /// `[top − 0.30 ch, top − 0.04 ch]` measured from THIS `top`, which is the
    /// whole of D16: under the shipped tall spelling the body covers the entire
    /// cell, so a birth zone stated in cell coordinates would put every strike
    /// star inside the stroke.
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

    /// The planned boundaries that are LIT on the last frame — coverage at
    /// or over [`STATUS_LIT_COV`] after the time-fade, the edge-in and the
    /// expiry melt. This is what `trail status` claims as its ribbon
    /// (`ribbon_segments=` / `ribbon_hue_bands=`), so a claim is never made
    /// for a cell that is resident but has melted under what the glass shows:
    /// the paint-conformance bind pairs every status read with its frame and
    /// charges a claimed-but-dark frame as `ribbon_dark` (measured 2-3 such
    /// frames per image row on the first matrix run, all in the retract's
    /// fade, when the count was `live_cells`).
    pub fn lit_segments(&self) -> impl Iterator<Item = &Segment> {
        self.plan.iter().filter(|s| s.cov >= STATUS_LIT_COV)
    }

    /// True when nothing is laid and no transient is finishing — one of the
    /// three pools `Engine::next_change_deadline` folds (§18).
    ///
    /// Clockless on purpose: the POOLS are the state. A cell that has expired
    /// is removed by [`Ribbon::plan`], not left in place for a deadline read
    /// to notice, so "is anything laid" and "is anything visible" are the same
    /// question — which is what lets `needs_frame_cadence()` keep v1's
    /// argument-free signature at seam point 8.
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.cells.is_empty() && self.cohorts.is_empty()
    }

    /// **THE CADENCE LAW, brisk half** — whether per-frame MOTION is on the
    /// mark: a cell inside its 18 ms `edge-in` (the one attack, T3), the
    /// focus rearm's `edge-in`, or a cohort RETRACTING into the caret on
    /// `suck-in` — §4's 0.40 s, the swoosh's one motion; its grace, its
    /// reach beats and its fade are tails. Under reduced motion nothing
    /// moves (§6.11); the attack still ramps.
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        let ramping = |at: Instant| now.saturating_duration_since(at).as_secs_f32() < EDGE_IN_S;
        self.cells.iter().any(|c| ramping(c.born))
            || self.rearm.is_some_and(|(at, _)| ramping(at))
            || (!self.reduced
                && self.cohorts.iter().any(|c| {
                    Phase::at(now.saturating_duration_since(c.alive_at).as_secs_f32())
                        == Some(Phase::Retracting)
                }))
    }

    /// Seconds until the mark's SETTLE under a falling spine moves a pixel
    /// — the engine's own tail, because the wave and the wedge read
    /// `Ctx::disp` (§4: "both only SETTLE") and the ribbon cannot see the
    /// spine on its own. The head cell moves fastest: the wedge closes at
    /// `wedge_px · disp / DISP_RELEASE_TAU` px/s and the wave (amplitude
    /// `WAVE_AMP_CELLS·ch·disp`, travelling at `PHASE_RATE·disp` cycles/s)
    /// at its crest velocity; one pixel over their sum. `None` with nothing
    /// laid or the spine at rest.
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

    /// **THE CADENCE LAW** — the next instant this ribbon changes what is
    /// on glass. `None` when at rest, which is what lets the host go to 0 %
    /// idle (T6).
    ///
    /// Read as offers to the [`Cadence`] fold: the `edge-in`s and the
    /// retract are brisk ([`Ribbon::brisk`]); the reach beats (a cell
    /// appears on each), the retract's first frame and the swoosh's end are
    /// exact EDGES; and everything else is a TAIL — every fade (the expiry
    /// melt, a retracted cell's `spend`, the focus ember, the swoosh's
    /// farthest-first fade) at the next visible step solved from its own
    /// curve ([`level_step_melt`], [`level_step_spend`]) with the cell's
    /// other factors frozen into the peak, and every disappearance or
    /// fade-start (a cell's expiry, a kill's staggered retract stamps, the
    /// ember's end) at its instant — all floored. The peak is the dark
    /// cap's ([`UNDER_COV_CAP`]): the light role's is lower, and a lower
    /// peak only puts the step later, so the dark reading is never late.
    /// Under reduced motion every fade is the theme's one linear 120 ms
    /// fade (§6.11): its opening, then the floor inside it.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
        }
        let since = |at: Instant| now.saturating_duration_since(at).as_secs_f32();
        let dur = std::time::Duration::from_secs_f32;
        let reduced_fade_s = REDUCED_MOTION_FADE_MS / 1000.0;
        for cell in &self.cells {
            let age = since(cell.born);
            let life = cell.life_s.max(1e-3);
            cad.tail(cell.life_s - age);
            let coh = self.cohorts.iter().find(|c| c.id == cell.cohort);
            if self.reduced {
                let ends = [
                    Some(cell.life_s - age),
                    cell.retract_at.map(|at| RETRACT_FADE_S - since(at)),
                    self.ember_at.map(|at| FOCUS_EMBER_S - since(at)),
                    coh.map(|c| SWOOSH_TOTAL_S - since(c.alive_at)),
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
                let idle = since(c.alive_at);
                if Phase::at(idle) != Some(Phase::Fading) {
                    return None;
                }
                let span = f32::from(c.col1.saturating_sub(c.col0)).max(1.0);
                let from_head = f32::from(c.col1.saturating_sub(1).saturating_sub(cell.col)) / span;
                let t0 = RETRACT_START_S + RETRACT_DUR_S * (1.0 - from_head);
                Some(clamp01((idle - t0) / RETRACT_FADE_S))
            });
            let f_r = retract_u.map_or(1.0, spend);
            let f_e = ember_u.map_or(1.0, spend);
            let f_s = swoosh_u.map_or(1.0, spend);
            if let Some(step) = level_step_melt(base * f_r * f_e * f_s, u, life) {
                cad.tail(step);
            }
            if let Some(ur) = retract_u
                && let Some(step) = level_step_spend(base * melt * f_e * f_s, ur, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(ue) = ember_u
                && let Some(step) = level_step_spend(base * melt * f_r * f_s, ue, FOCUS_EMBER_S)
            {
                cad.tail(step);
            }
            if let Some(us) = swoosh_u
                && let Some(step) = level_step_spend(base * melt * f_r * f_e, us, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(at) = cell.retract_at {
                cad.tail_at(at);
                cad.tail_at(at + dur(RETRACT_FADE_S));
            }
        }
        for coh in &self.cohorts {
            // Appearances and the one motion's start are EDGES: the grace
            // ending on the first reach beat, the two beats after it, the
            // retract's first frame, and the swoosh's end (the cohort
            // retires — the idle instant). The retract's end is a tail: the
            // retract is brisk until then, and what follows is a fade.
            for beat in [
                LIFT_GRACE_S,
                LIFT_GRACE_S + REACH_STEP_S,
                LIFT_GRACE_S + 2.0 * REACH_STEP_S,
                RETRACT_START_S,
                SWOOSH_TOTAL_S,
            ] {
                cad.edge(coh.alive_at + dur(beat));
            }
            cad.tail_at(coh.alive_at + dur(RETRACT_START_S + RETRACT_DUR_S));
        }
        if let Some(at) = self.ember_at {
            cad.tail_at(at + dur(FOCUS_EMBER_S));
        }
        cad.take()
    }

    /// Translate every laid cell by a scroll of `rows` rows (seam point 12).
    /// Cells that leave the grid are dropped rather than clamped: a ribbon
    /// pinned to row 0 by a clamp is light on a line nobody typed.
    ///
    /// `_cell_h` is accepted and unused because every piece of ribbon state is
    /// GRID-anchored — the pixel geometry lives in [`Ribbon::plan`], which the
    /// next tick rebuilds from scratch.
    pub fn translate_scroll(&mut self, rows: u16, _cell_h: u16) {
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
        self.plan.clear();
        self.runs.clear();
        self.index.reset();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::{Geom, SoundCue};
    use aterm_render::{GlowQuad, RainHalo, over_premul, premul_rgb};
    use std::time::Duration;

    use super::super::{CaretSeam, Dir, TypedClass};

    /// The shipped dark theme, which every legibility number in the family is
    /// solved against.
    const DEFAULT_BG: u32 = 0x001A_1B26;
    const DEFAULT_FG: u32 = 0x00C8_D3F5;

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
            theme_fg: if dark { DEFAULT_FG } else { 0x0016_161C },
            theme_bg: if dark { DEFAULT_BG } else { 0x00FF_FFFF },
            reduced_motion: false,
        }
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

    /// True when a body quad lands in the UPPER HALF of cell `(row, col)`'s
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

    // -- §2.2 L3, §3.2: the bed's one ceiling ------------------------------

    #[test]
    fn letters_stay_legible_under_the_ribbon_on_the_default_dark_theme() {
        let budget = bed_luma_budget(DEFAULT_FG);
        let mut worst = f32::INFINITY;
        for i in 0..=512u32 {
            let t = i as f32 / 512.0;
            let ink = bed_ink(spectrum(t), budget);
            // Every level the emitter can composite — the body's own request
            // AND the strip's accent above it — not merely the bed's ceiling:
            // the bar must hold on the whole ramp, not only at the top of it.
            for cov in 1..=(BODY_FRAME_TOP as u32) {
                let cov = cov as u8;
                let lit = over_premul(DEFAULT_BG, premul_rgb(ink, cov), cov);
                worst = worst.min(contrast(DEFAULT_FG, lit));
            }
        }
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the bed composited to {worst:.3}:1 against the default foreground; the bar is {BODY_CONTRAST_BAR}:1"
        );
    }

    #[test]
    fn the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_78_under_the_bar() {
        // "Dim and muddy" gets a NUMBER. Under L3's 5.25:1 bar every stop is
        // put on `bed_luma_budget(fg)`, and a warm stop at that luminance IS
        // an olive: on the default theme a full-coverage yellow composites at
        // `(78, 78, 2)`, at or below the ≈ 84 v1's coverage table produced.
        // The module does not fix that half of the measured defect, and this
        // pin is what keeps the module doc from claiming it does.
        //
        // TWO-SIDED on purpose. An owner ruling that relaxes the bar for the
        // bed over blank cells (§22, OPEN) lands the warm mids above 84 and
        // must re-pin this line; a change that dims the bed further trips the
        // floor. Either way the number moves HERE first, not in a screenshot.
        let budget = bed_luma_budget(DEFAULT_FG);
        let cov = UNDER_COV_CAP as u8;
        let max_channel = |c: u32| ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff);
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
            (72..=84).contains(&max),
            "the dimmest stop (t = {t:.3}, composited #{lit:06X}) peaks at max channel {max}; the bar puts it at ≈ 78 and v1's table at ≈ 84 — a brighter warm mid is the OPEN §22 ruling, not a drift"
        );
    }

    #[test]
    fn every_stop_of_the_arc_composites_at_one_weight() {
        // The EVEN-ARC law — the "navy mids" half of the measured defect:
        // after the ink recipe, no two positions of the arc differ in
        // composited luminance by more than the byte quantization can
        // explain. It is NOT the "olive mids" half: the shared weight is the
        // bar's own, and the pin above says what that weight looks like.
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

    // -- §18: the field index ---------------------------------------------

    #[test]
    fn the_field_index_answers_what_a_newest_first_scan_would() {
        // Two rows, driven through the public API only, across the frames in
        // which a cell is genuinely SHADOWED (a jump abandons a cohort and the
        // hand retypes over it while it is still draining), a row goes DARK
        // and is RELIT — the lane prune and re-mint in `FieldIndex::begin`
        // that a one-row, one-frame pin never exercises.
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
        // lays a NEW cohort over cells that are still in the pool, draining.
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
        // §4's retract is a SUFFIX: every cell at or right of the caret is
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
        // Retype the three, then one more in the MIDDLE of the word.
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

    #[test]
    fn the_reach_extends_its_own_cohort_and_continues_its_own_walk() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A: cols 10..17. B: cols 40..41, two cells short of four letters, so
        // at its second reach beat it wants cols 39 and 38 — with A still on
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
        let now = at(t0, 1060 + 850);
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
                (cell.t - (t40 - back / WALK_FAST_CELLS)).abs() < 1e-5,
                "the reach at {col} must continue B's own walk backwards"
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
                    head_x - x <= HOT_EDGE_CELLS * g.cw as f32 + f32::from(q.w) + 1.0,
                    "{label}: the hot edge reached further than {HOT_EDGE_CELLS} cells behind the head"
                );
                let y = f32::from(q.y);
                assert!(
                    (caret_top - reach_y..=caret_bot + reach_y).contains(&y),
                    "{label}: the hot edge left the caret's own row band (y = {y})"
                );
            }
            // The peak request is 38; the beam's transverse AA can only lower
            // it, and premultiplied white carries the coverage in every channel.
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
        // A BACKSPACE FRAME (§4.1 "never lags the head", §20.1 "ends at the
        // head cell every frame"): three erased cells are still draining to
        // the right of the caret; the edge ends at the caret's own cell.
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

    #[test]
    fn a_cold_hand_and_a_light_ground_carry_no_hot_edge() {
        let dark = cfg(true, true);
        let light = cfg(false, true);
        assert!(Ribbon::hot_edge_gain(&dark, HOT_EDGE_DISP_MIN - 0.01) <= 0.0);
        assert!(Ribbon::hot_edge_gain(&dark, 0.9) > 0.0);
        assert!(
            Ribbon::hot_edge_gain(&light, 1.0) <= 0.0,
            "additive white on a paper ground is exactly what L6 forbids"
        );
    }

    // -- the measured "keeps brightening" defect ---------------------------

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
        // Start past the head cell's ONE sanctioned ramp-in (T3's 18 ms
        // `edge-in`); everything after it must only ever spend light. The
        // spine is handed a value that keeps CLIMBING, which is exactly the
        // condition that produced the measured 660 ms of after-glow.
        //
        // The body's law is stated on the REQUEST — the coverage the ribbon
        // asks for at each cell boundary — because that is what "the body's
        // light" means. The rasterized peak still moves by a level as the
        // wave's sub-pixel phase settles, and forbidding THAT would forbid the
        // wave; what may not move is the light the mark was priced at. The hot
        // edge's law is stated on what it EMITS, since it is additive and has
        // no request of its own on the plan.
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
            // levels. A gain read off the live spine climbs by a factor of
            // ten here, not by four levels.
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
        let retract_start = (LIFT_GRACE_S + REACH_STEP_S * f32::from(REACH_BEATS)) * 1000.0;
        for ms in ((retract_start as u64)..1560).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            // THE HOT EDGE GOES OUT WITH ITS BODY. It is the head's property
            // and takes the head cell's envelope, so through the retract it
            // may only fall — the slab-mid sampling of a collapsing segment
            // can move the rasterized peak by a level, and no more — and it
            // is at exactly zero before the cells retire. A hairline priced
            // from `birth_disp` alone held at 38 over a spent body here and
            // snapped off with the pool at 1.54 s.
            let edge = peak_channel(&sink.out);
            edge_first.get_or_insert(edge);
            assert!(
                edge <= edge_prev.saturating_add(2),
                "the hot edge brightened inside the swoosh at +{ms} ms ({edge_prev} → {edge})"
            );
            edge_prev = edge;
            if ms == 1450 {
                let first = edge_first.unwrap_or(0);
                assert!(
                    !sink.under.is_empty() && edge * 4 < first,
                    "mid-fade the hot edge must be well under way with its body ({first} → {edge})"
                );
            }
            if ms == 1530 {
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
        // §4's drain "picks cells farthest-from-the-head first", which is what
        // makes the ending read as the mark being drawn back INTO the caret.
        // Reading the mark the other way round is the same arithmetic and
        // looks like the ribbon walking away from the hand.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let start = LIFT_GRACE_S + REACH_STEP_S * f32::from(REACH_BEATS);
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
        // A: cols 10..17. A real jump on the same row abandons it into its
        // retract (§4); the hand then lays B beside it while A is moving.
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let now = at(t0, 1000);
        let cx = ctx(now, &c, (2, 40), 0.8);
        rib.on_event(&nav((2, 18), (2, 40)), now, &cx);
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
        assert!(
            (b_left - 40.0 * cw).abs() < 0.5 && (b_right - 46.0 * cw).abs() < 0.5,
            "the cohort under the hand was squashed with its row-mate: [{b_left}, {b_right}]"
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
        for ms in (900..1560u64).step_by(10) {
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
            assert!(
                (left - 10.0 * cw).abs() < 0.5 && (right - 18.0 * cw).abs() < 0.5,
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
        let before = plan_sum(&rib);
        let segs = rib.plan_segments().len() as u32;
        assert!(
            plan_peak(&rib) > 200,
            "the mark must be hot for a step to be measurable"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 80), 0.9);
        rib.on_event(&nav((2, 42), (2, 80)), now, &cx);
        rib.plan(&cx);
        let after = plan_sum(&rib);
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

    #[test]
    fn a_wrapped_paragraph_keeps_its_earlier_rows_until_the_finger_lifts() {
        // §4: the swoosh is the FINGER-LIFT arc — "no typing key for this
        // long". A cohort per row with a grace clock of its own would swoosh
        // the previous line away 1.54 s after every wrap while the hand was
        // still typing on the next.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let row = |r: u16| Keys {
            g: geom(),
            row: r,
            col0: 0,
            n: 20,
            period_ms: 100,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, row(2), &c);
        type_keys(&mut rib, at(t0, 2000), row(3), &c);
        let cx = ctx(at(t0, 3950), &c, (3, 20), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().any(|l| l.row == 2),
            "row 2 must still be lit 2 s after its wrap while the hand types on row 3"
        );
        let cx = ctx(at(t0, 3900 + 1600), &c, (3, 20), 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "…and the whole mark leaves together {SWOOSH_TOTAL_S} s after the LAST key"
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
        rib.on_event(&Event::Focus(false), at(last, 100), &cx);
        let cx = ctx(at(last, 250), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let dimmed = plan_peak(&rib);
        assert!(
            dimmed < full / 2,
            "the ember must be well under way ({full} → {dimmed})"
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

    #[test]
    fn the_hot_edge_embers_out_with_its_body_on_focus_loss() {
        // §8.2 "Focus lost | ember in 300 ms | spend": the hairline is the
        // head's property and takes the head cell's envelope, so it spends
        // WITH the body. A hot edge that held at 38 while the body under it
        // spent to nothing, then vanished with the pool at +300 ms, was a
        // white line with no keystroke behind it — the anti-stray class.
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
        assert!(
            low * 4 < full && low <= mid,
            "+250 ms into the ember the hot edge is at {low} against {full} at the loss; it must go out with its body, not hold until the pool clears"
        );
        frame(&mut rib, &mut sink, at(lost, 300));
        assert!(
            sink.out.is_empty() && rib.at_rest(),
            "…and at the ember's end there is nothing, hairline included"
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

        // …and the LIGHT law is one law: the same cell, the same age, the same
        // spine gives the same peak coverage under both profiles. Only the
        // band's extent moves.
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
        // THE STRIP'S ORACLE: on a hot mark the body sits at its own cap, so
        // the only thing that can take the spine past it is the baseline
        // strip's accent — if it does not, the strip is not there.
        assert!(
            peak > UNDER_COV_CAP as u8,
            "the baseline strip must show above the body's ceiling (peak {peak}, cap {UNDER_COV_CAP})"
        );
        assert!(
            f32::from(peak) <= BODY_FRAME_TOP,
            "and the strip's lift may never take the spine past the ledger's frame top"
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
            let arc = spectrum(clamp01(tri(t)));
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
        for row in 2..5u16 {
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 76,
                period_ms: 20,
                disp: 1.0,
            };
            type_keys(&mut rib, at(t0, ms), keys, &c);
            ms += 76 * 20;
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
                    // whole, and what is shed comes off the OLDEST row's tail
                    // — contiguous to its head, never out of its middle.
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 3, col),
                            "row 3 col {col} was shed before row 2"
                        );
                    }
                    let lit: Vec<bool> = (2..78u16)
                        .map(|col| cell_lit(&sink.under, g, 2, col))
                        .collect();
                    assert!(
                        lit.iter().any(|&l| !l),
                        "three retina rows exceed the budget; the oldest must have lost its tail"
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
        // …and the SAME ribbon re-planned at the same `now` is the same frame:
        // the plan is a function of `now`, not of how many times it ran.
        let mut again = Sink::default();
        frame(&mut a, &mut again);
        assert_eq!(
            sa.under, again.under,
            "re-planning at one `now` changed the frame"
        );
        assert_eq!(sa.out, again.out);
    }

    #[test]
    fn a_scroll_moves_the_ribbon_with_the_viewport_and_drops_what_leaves() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 4, &c, 0.5);
        assert!(rib.live_cells() > 0);
        rib.translate_scroll(1, 18);
        assert!(rib.cells().iter().all(|l| l.row == 1));
        rib.translate_scroll(4, 18);
        assert!(rib.at_rest(), "light on a line nobody typed is not kept");
    }

    // -- §18 cadence: a fade's next level is solved, not polled -------------

    /// **A FADE'S NEXT u8 LEVEL IS SOLVED IN CLOSED FORM** (§18's cadence
    /// law): for the theme's `spend` and the cell's expiry melt, over bright
    /// and dim marks and every phase of the fade, the instant
    /// [`level_step_spend`] / [`level_step_melt`] names is the first at
    /// which `round(peak · curve)` actually drops, to within 0.2 ms of a
    /// 10 µs poll — and `None` exactly when the mark is already at zero.
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
}
