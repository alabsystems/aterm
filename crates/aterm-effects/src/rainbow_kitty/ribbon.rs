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
//! (head floor 1.0, crest gain 0.0), the 0.75-cycle wave (at
//! `0.110 ch · disp` since 2026-09-08 — twice v1's `0.055`, the owner's
//! "bigger"; [`WAVE_AMP_CELLS`]), the 18 ms `edge-in` and the exact-zero
//! expiry melt, the four-letter
//! guarantee and its chain law, the staggered backspace retract, the wrap
//! fold, the exit swoosh (`0.75 grace + 3 × 0.05 reach + 0.40 retract +
//! 0.24 fade`), and the source-over twin on light themes.
//!
//! ## What is genuinely new, and why
//!
//! 1. **The hot edge** (§4.1) — a 1-px additive hairline IN THE SPECTRUM
//!    along the ribbon's TOP edge, from the head cell back [`HOT_EDGE_CELLS`]
//!    cells: each vertex carries the stop under it, lifted toward white to
//!    [`HOT_EDGE_LUMA_FLOOR`] ([`hot_edge_ink`]) so it out-shines any bed it
//!    can ride, at [`HOT_EDGE_COV_MAX`] 118 — the transient cap. It was a
//!    `#FFFFFF` hairline at 38 — "a third white idiom" — until 2026-09-08,
//!    when the owner struck the
//!    restraint ("I feel like you are diminishing the specialness and
//!    emphasis of this theme? why?"). Drawn as a `comet_beam` hairline
//!    sampled at the same `spine − up` the body used (the top edge carries
//!    the wave, so three axis-aligned rects cannot follow it). Dark themes
//!    only, into `out`, SPATIAL rather than temporal so it can never lag the
//!    head; its gain is priced at the head cell's birth and scaled by the
//!    head cell's envelope ([`Ribbon::env_of`]), so it goes out WITH the
//!    body — ember, melt and swoosh — and never holds over a spent one. It is
//!    the first item on the owner A/B sheet (§22), so it lives behind one
//!    gate — [`Ribbon::hot_edge_gain`] — that is one line to flip off.
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
//!   below the ground's own reach; [`bed_ink`] lifts them until they sit on
//!   the same composited luminance as every other stop — through their own
//!   hue at full value first, and toward white only for the light a hue
//!   cannot carry ([`onto_luma`], 2026-09-09; below) — so the arc is EVEN —
//!   indigo weighs what red weighs — and the 33-entry
//!   per-position coverage table v1 priced that against is not carried
//!   (§19.1: *"two ceilings for one bed"*). The bottom-only brightness was
//!   the baseline strip out-shining a body that had been capped below it; the
//!   strip is now priced out of the SAME ceiling the body is
//!   ([`BODY_FRAME_TOP`] − `cov`, ~15 levels on a hot mark) instead of beside
//!   it, so it is a crisp accent on a bright body rather than the only lit
//!   thing in the mark. The OLIVE mids are NOT fixed here. The "one
//!   composited luminance" is L3's 5.25:1 bar against the theme's foreground
//!   — `Y ≈ 0.083` on the default theme — and a warm stop at that luminance
//!   IS an olive: a full-coverage yellow composites at `(80, 80, 3)`, max
//!   channel 80, at or below the ≈ 84 v1's table produced. The warm mids sit
//!   at the bar's ceiling BY CONSTRUCTION — and since 2026-09-08 AT the bar
//!   rather than a guard under it: the owner kept the bar as the one
//!   restraint ("text under the bed stays legible") and struck every other
//!   one ("I feel like you are diminishing the specialness and emphasis of
//!   this theme? why?"), so the 0.15 guard became the 0.05 the composite's
//!   rounding actually needs ([`BODY_CONTRAST_GUARD`]), the solver answers
//!   from under its target ([`solve_for_luma`]), and the luminance clamp
//!   became the bar's own answer at a white foreground ([`BED_LUMA_MAX`]
//!   0.150, from 0.100 — which had bound on every foreground brighter than
//!   ≈ `#D8D8D8`). On the default theme that is one level (79 → 80); on a
//!   white foreground it is sixteen (87 → 103). Relaxing the bar for the bed
//!   over BLANK cells stays REJECTED (below). The number is pinned by
//!   `the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar`,
//!   so "dim" has a figure to move when a ruling lands.
//! * **"There are GAPS, BLACK GAPS in the rainbow a few characters back
//!   from the cursor"** (the owner, 2026-09-08, on Nord). Diagnosed twice
//!   on glass — the shipped v0.76.0 and m15's main — with the same
//!   instruments (`blackgaps/`, `blackgaps-main/`): NOT holes. Every "gap"
//!   cell held the SAME relative luminance as the vivid cells beside it
//!   (`Y 0.062–0.083`, ≈ 1.9 × the ground); what read as black was CHROMA.
//!   Two stops lost it two different ways. Indigo `#4B0082` was walked
//!   TOWARD WHITE onto the bar and arrived as `(82, 60, 136)` composited —
//!   `S 0.61`, a grey lavender next to a blue at 210–230 — and the
//!   green→blue crossing carried the arc's authored `S 0.53`
//!   ([`crate::spectrum::SPECTRUM_CROSSING_ROOF`], sized for the cyan
//!   census the owner retired on 2026-09-01) and composited as
//!   `(45, 92, 93)`, the greyest cell on the line. The owner's standing
//!   rulings cover the class — "bright not dim", and "the anti-cyan laws
//!   were what greyed the arc" — so the recipe now spends CHROMA before it
//!   spends white: [`onto_luma`] takes a dark stop up through its own hue to
//!   full value and only then toward white (indigo on Nord: `(127, 0, 219)`,
//!   `S 0.98`, the same `Y 0.097`, the same 5.3:1 under the text), and
//!   [`BED_SAT_FLOOR`] gives the crossing its neighbours' chroma back at the
//!   bed's own read (`(3, 95, 95)`). The luminance budget itself is still
//!   the bar's alone: a floor against the GROUND was tried here and taken
//!   out again, because wherever it exceeds the bar it lifts the bed over
//!   the luminance the text can bear (One Dark's `#ABB2BF` over `#282C34`
//!   went 5.29:1 → 4.69:1), and that trade is the owner's, not this
//!   module's — `the_warm_stops_frontier_table` prints what each theme's
//!   bed reads at over its own ground beside what a lift would cost. What
//!   this does NOT do: brighten yellow, green or the crossing's green half.
//!   At a fixed
//!   `Y` the sRGB transfer caps their peak channel (yellow `(90, 90, 0)`,
//!   green `(0, 102, 0)`, cyan `(0, 98, 97)` at Nord's `0.097`), and they
//!   are already at full chroma — the only lever left is the bar itself,
//!   and that is the owner's ruling, not this module's;
//!   `the_warm_stops_frontier_table` prints what each step up would cost.
//!   Pinned by `the_bed_s_dark_stops_arrive_at_full_chroma_not_walked_grey`,
//!   `the_crossing_composites_no_greyer_than_its_flanks`,
//!   `the_ground_never_lifts_the_bed_over_the_bar` and
//!   `the_visibility_floor_costs_the_bar_nothing_on_nord`.
//!
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
//! * **"The ribbon under a line I am still typing is BROKEN INTO PIECES"**
//!   (the owner's screenshot, 2026-09-08 — "rainbow" lit, "theme" dark,
//!   "truly" half lit, "magical" lit, "and" dark, "specai" lit at the caret).
//!   Traced on the shipped binary under real Claude Code and under the paint
//!   probe's fake-claude shape (771 and 624 planned ticks): every echo was
//!   licensed, no cell was re-laid or retired by the TUI's redraw, and the
//!   engine's cell set was contiguous on every tick — the darkness was the
//!   EXPIRY. A cell's life was priced once at birth from the spine
//!   ([`Ribbon::cell_life`]: 1.70 s for the first five keys of a take,
//!   4.55 s once the spine was hot) and ran on the cell's own clock, so the
//!   oldest cells died under a hand that had not lifted (the take's "wha"
//!   went dark 1.9 s into a line still being typed), and a word typed at a
//!   dip — after a thinking pause, or slowly — was born with a SHORTER life
//!   than the hotter word before it (`birth_disp` 0.8·peak after a 700 ms
//!   pause: 3.1 s against 4.4 s at peak 0.75) and died first, a dark word
//!   inside the live span. v2's law now: **cells of a cohort share the
//!   cohort's clock** — a cell's expiry is measured from
//!   `max(cell.born, cohort.alive_at)` ([`live_since`]), and every typing
//!   key refreshes `alive_at` on every un-abandoned cohort, so a mark only
//!   ages once the hand stops, and then the exit swoosh retracts it from the
//!   tail exactly as before (off-glass [`SWOOSH_TOTAL_S`] after the last
//!   key, idle → zero unchanged). This REVERSES the 2026-09-06 ruling that
//!   v1's "while the rhythm lives the whole mark lives" was not a v2 gap
//!   (`RAINBOW-KITTY-V2.md` §23's addendum). Pinned by
//!   `a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret`
//!   here and `a_line_still_being_typed_has_no_dark_cell_inside_its_live_span`
//!   at the seam.
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
//! and leaves the warm mids where v1 left them, ≈ 80 max channel composited.
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
//! second key (`HOT_EDGE_DISP_MIN` 0.15), both under the same bar (§23) —
//! and on 2026-09-08 the bar was taken to its edge (the guard, the clamp,
//! the solver) and everything ABOVE the text was made louder: the hot edge
//! in the spectrum at the transient cap, the wave at twice its swing. The
//! max-channel pin
//! beside `letters_stay_legible_under_the_ribbon_on_the_default_dark_theme`
//! holds the number so a future change to the bar has something to move,
//! and `the_bed_sits_at_the_bar_not_under_it_on_the_default_dark_theme`
//! holds the other side: light left under the bar is diminished light.
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
use crate::spectrum::{spectrum, spectrum_with_min_saturation};

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

/// The hot edge's falloff shape, normalised at the head cell:
/// `alpha(d) = 0.38·(1 − d/3)²`, so the request is
/// `HOT_EDGE_COV_MAX · (1 − d/3)²` with `d` in cells behind the head.
pub const HOT_EDGE_ALPHA: f32 = 0.38;

/// The hot edge's coverage request ceiling — THE TRANSIENT CAP itself (§3.2,
/// `RAINBOW_TRANSIENT_COV_CAP` 118: the meteor's shoulder, the pin's
/// nucleus, every transient at the hand), which is what keeps a 1-px additive
/// hairline legibility-safe: the ledger holds it to the ink lift over the
/// row above's probed glyph cells (L2), and it never touches the row it
/// underlines.
///
/// **118, from 38 (2026-09-08, the owner: "I feel like you are diminishing
/// the specialness and emphasis of this theme? why?" — "everything above
/// and around the text may be as loud as you like").** At 38 the white
/// hairline over the ground composited at max 76 / spread 12 — a grey
/// lightening no scanner could call colour (the paint scanner's rule: max
/// ≥ 60 AND spread ≥ 40) — and over its own bed it was 1.7× (blue) to 2.3×
/// (yellow) the bed's luminance. At the cap the lifted stop is colour on
/// every one of the seven (over the ground: red 144 / 74, orange 144 / 106,
/// yellow 145 / 107, green 145 / 119, blue 156 / 68, indigo 129 / 40,
/// violet 145 / 64) and over its bed the head's request is 2.3× (blue) to
/// 7.0× (yellow) the bed's luminance — before the hairline's 1-px
/// anti-aliasing, which lands 30-70 % of a request on its brightest row.
/// Measured on the default theme in
/// `the_hot_edge_is_the_stop_under_it_lifted_hot_and_brighter_than_the_bed`.
pub const HOT_EDGE_COV_MAX: f32 = 118.0;

/// The relative luminance the hot edge's ink is LIFTED to when its stop is
/// darker — twice [`BED_LUMA_MAX`], so the hairline's ink is at least twice
/// as luminous as any bed it can ride, on every theme and every stop, by
/// construction. A stop already brighter than this (orange, yellow, green)
/// is carried pure; red, blue, indigo and violet are lifted to it
/// ([`hot_edge_ink`] through [`onto_luma`]) — through their own hue at full
/// value first, then toward white: the hue stays, the heat is white.
///
/// It is a floor on the INK, not on the composite: the hairline is additive
/// and priced by [`HOT_EDGE_COV_MAX`], so what lands is `ink · cov / 255` on
/// top of the bed — the blue stop's `(135, 135, 255)` at 118 puts
/// `(62, 62, 118)` on a bed whose blue is already saturated, which is the
/// one way a blue hairline can be brighter than a blue bed.
pub const HOT_EDGE_LUMA_FLOOR: f32 = 2.0 * BED_LUMA_MAX;

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
/// the underline spelling is `0.36 ch` below it. `0.55` bounds both with room
/// for the wave (`0.11 ch` at full momentum), and the pin
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
/// exactly `0.36 ch` under the cell top (`0.305` until the wave doubled on
/// 2026-09-08). An underline however the shoulder is spelled.
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

/// The 0.75-cycle wave's amplitude in cells, at full spine.
///
/// **0.110, from v1's `RAINBOW_WAVE_AMP_CELLS` 0.055 (2026-09-08, the owner:
/// "make this rainbow theme truly magical and special and dynamic and
/// beautiful").** At 0.055 the live-momentum wave was ≤ 1 px at `ch 18` and
/// 1.5 px at retina — a settle the eye could not read as motion. Doubled,
/// the mark breathes ±2 px (3 px at retina) with speed, still inside the
/// rows the body owns: the head is pinned ([`WAVE_HEAD_PIN`]), the top edge
/// never rises above [`TALL_UP_CH`] and the bottom never falls past
/// [`DN_TOP_CH`], which grows with this number by construction (the wedge's
/// full travel is `0.36 ch` now, was `0.305`). Pinned by
/// `the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows`
/// (0.99 px at `ch 18` before, 1.98 after).
pub const WAVE_AMP_CELLS: f32 = 0.110;

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
/// bed's dimmest stop composites at a brightest channel of ≈ 80 at full
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
/// be able to cross — and NO MORE than that, because light left under the
/// bar is diminished light (the owner, 2026-09-08).
///
/// **0.05, from 0.15.** The 0.15 was sized as "~4 levels of composited luma
/// at the bar, more than all three [rounding costs] can spend together";
/// measured, two of the three spend nothing: [`solve_for_luma`] answers from
/// UNDER its target, so the ink's own byte rounding cannot cross the bar,
/// and the LUT's lerp of two on-bar colours is under the bar (the EOTF is
/// convex). What is left is the partial composite's one level of rounding
/// (`premul_rgb` + `over_premul`), and one Bayer level over the ledger's
/// frame top — worth 0.05 of a ratio point at the dimmest theme above the
/// floor (fg `#A0A0A0`, budget 0.026: 5.2019:1 at a guard of 0), less
/// everywhere brighter. Sized by
/// `letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor`,
/// which is the first step of a `0 / 0.005 / 0.01 / 0.015 / 0.02 / 0.03 /
/// 0.05` sweep that holds the bar on all 67 of its themes.
pub const BODY_CONTRAST_GUARD: f32 = 0.05;

/// Floor on the bed's composited luminance budget. A theme whose foreground is
/// almost black would solve to a bed nobody can see; below this the ribbon
/// stops obeying the bar and simply takes the dimmest light that still reads.
pub const BED_LUMA_MIN: f32 = 0.020;

/// …and the ceiling: the bar's own answer at a WHITE foreground
/// (`(1.0 + 0.05) / 5.25 − 0.05`), which no foreground can exceed — so under
/// the bar this clamp binds nowhere, and the bed on every theme is exactly
/// as bright as its text allows.
///
/// **0.150, from 0.100 (2026-09-08).** The 0.100 was written against L5
/// (the caret is the brightest PERSISTENT thing on glass), but it bound on
/// every foreground brighter than ≈ `#D8D8D8` — the offline renderer's
/// `#E8E8F0` solved to 0.114 and took 0.100, a white foreground to 0.150 and
/// took 0.100: a third of the bar's light, withheld on the brightest themes
/// for no law the text could name. The owner's ruling is that the bar is the
/// one restraint ("text under the bed stays legible") and the bed is not to
/// sit under it. L5 is untouched: the caret block is an opaque authored stop
/// and out-shines a bed at `Y 0.15` on every stop of the arc.
///
/// L3's "field luminance ceiling 72" is this budget read in v1's unit on the
/// default theme, where [`bed_luma_budget`] solves to `Y ≈ 0.083` (the bar
/// solves it, not this clamp) and a warm stop composited at full coverage
/// carries a Rec.709 gamma-luma of ≈ 74/255 (`(80, 80, 3)` at yellow). The
/// two numbers are one ceiling in two coordinates; this module states it in
/// the one the 5.25:1 bar is written in. Pinned by
/// `a_bright_foreground_s_bed_takes_the_bar_s_whole_budget_not_a_tenth`.
pub const BED_LUMA_MAX: f32 = 0.150;

/// **THE BED'S CHROMA FLOOR** — the least HSV saturation a stop of the arc
/// may carry into [`bed_ink`] and [`hot_edge_ink`], applied before the
/// luminance solve ([`crate::spectrum::spectrum_with_min_saturation`]).
///
/// THE OWNER, 2026-09-08: *"there are gaps black gaps in the rainbow a few
/// characters back from the cursor."* Diagnosed twice on glass (v0.76.0 and
/// m15's main): not holes — every "gap" cell held the SAME relative
/// luminance as the vivid cells beside it; what read as black was chroma.
/// This floor is the crossing's half of that. The green→blue crossing is
/// authored at `S 0.53` ([`crate::spectrum::SPECTRUM_CROSSING_ROOF`],
/// tapering in from `0.65` / `0.72` knots) for the cyan true-peak bound of
/// the census the owner retired on 2026-09-01, and once the bed scales it
/// onto the bar it composites as `(45, 92, 93)` — `S 0.52`, the greyest cell
/// on the line, a teal-grey notch between an `S 0.97` green and an `S 0.81`
/// blue. **`1.0`**: the seven anchors all carry `S 1.0` (each has a zero
/// channel) and every table entry outside the crossing's flanks sits at
/// `S ≥ 0.98`, so the floor asks nothing of any stop but the crossing, and
/// there it asks for exactly what its neighbours have. Hue and value are
/// untouched, so the crossing's hue PACING — the thing the roof still owns
/// — is exactly as authored. Measured: the crossing composites `(3, 95, 95)`
/// at the same `Y`. Pinned by
/// `the_crossing_composites_no_greyer_than_its_flanks`.
pub const BED_SAT_FLOOR: f32 = 1.0;

// There is deliberately NO floor for the bed against the GROUND. One was
// tried (2026-09-09, `BED_GROUND_CONTRAST_MIN` 1.4:1, `bar.max(ground)`): it
// reads as harmless on the shipped defaults, whose bars answer far above it
// (Nord `0.097` against `0.068`), but on any theme whose foreground solves
// the bar under `1.4 ×` its ground it OVERRIDES the bar — One Dark's
// `#ABB2BF` over `#282C34` went from 5.29:1 to 4.69:1 under the text, and 54
// (fg, ground) pairs of the 67-theme sweep were bar-violating by
// construction. A floor that must never exceed the bar collapses to the bar,
// so it cannot exist as a law beside it; how dark a bed a dark-foreground
// theme is handed is a ruling for the owner, and
// `the_warm_stops_frontier_table` prints it (Solarized Dark's bed sits 1.00:1
// over its page at the `BED_LUMA_MIN` clamp, its text at 4.70:1 — the
// documented exception; lifting it to 1.4:1 would cost that text 3.39:1, and
// One Dark's 5.29:1 → 4.69:1). Pinned by
// `the_ground_never_lifts_the_bed_over_the_bar`.

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

// ===========================================================================
// The wake — what a jump leaves behind (R5, 2026-09-08)
// ===========================================================================

/// **A JUMP LAYS ITS CORRIDOR** (the owner, 2026-09-08: "I want more of a
/// trailing cursor rainbow effect", "I like the streak effect, but leave more
/// a rainbow after effect"). Measured on 379ab6159, before this law, on
/// glass against a trail-OFF control: behind a Ctrl-E into unlit ground the
/// corridor held colour for under 100 ms — the train crosses 25 cells in
/// ~60 ms and its root then sucks INTO the landing (T5), so nothing of it
/// lingers where it flew — and a band live under the hand was gone 707 ms
/// after a Ctrl-A (the abandon's 0.40 + 0.24 s retract, sliding toward the
/// new caret). Nothing trailed. The wake is the ribbon the jump itself lays
/// behind the caret, newest at the landing, on the walk the band already
/// had (C2: `t` is a function of position, so the corridor continues the
/// live cohort's spectrum instead of re-anchoring at red).
///
/// A phrase, not a line: a Ctrl-A across 200 columns must not paint the
/// whole row. Counted back from the LANDING, so the rainbow lies beside the
/// caret, where the eye is. Cells beyond the cap that the live band already
/// lights are TAKEN OVER, never cut — light that was on the glass stays on
/// it and goes out on the wake's clock. A band ALREADY LEAVING (its cohort
/// retracting, or abandoned by an earlier jump) is not taken over and has
/// nothing laid under it while it is lit: it finishes on its own clock, and
/// the wake lights only the cells its drain has already emptied — a half-
/// drained cell handed to a fresh cohort would freeze at the level it had,
/// and that frozen ramp between two bright runs is the owner's "black gaps a
/// few characters back from the cursor" (R6), reproduced on a Ctrl-A at
/// 1.0–1.4 s idle before this clause.
pub const WAKE_MAX_CELLS: u16 = 32;

/// Long enough to read as an after-effect and short enough that a Ctrl-A /
/// Ctrl-E ping-pong cannot accumulate. FIXED, not [`Ribbon::cell_life`] — a
/// jump must not inherit the four-letter chain law, nor its 1.70 s swoosh
/// floor ([`SWOOSH_LIFE_S`]). The wake's cohort is never held by a later
/// jump (each jump abandons what came before it), so the pool holds at most
/// `WAKE_LIFE_S / jump period` wakes, and the pinned ping-pong empties
/// within one wake life of its last key.
pub const WAKE_LIFE_S: f32 = 1.10;

/// A NEW wake cell is born this far past the landing, so it fades up UNDER a
/// train that is still bright: the eye sees ONE gesture leaving light
/// behind, not two marks racing. Until it is born the cell is DARK
/// ([`Ribbon::env_of`]'s first line) — the attack starts at `born`, never
/// before it. A cell taken over from the live band keeps its own birth: a
/// hand-off has no attack, and no dip.
pub const WAKE_BORN_LAG_S: f32 = 0.10;

/// A same-row hop under [`JUMP_MIN_CELLS`] — an arrow key — lays its wake at
/// this share of [`WAKE_LIFE_S`]: holding an arrow paints rainbow behind the
/// caret, a single tap leaves a short one, and a cell a live cell already
/// owns is left exactly as it is (the hop does not abandon). The hop's new
/// cells go into a cohort of their own ([`Cohort::wake`]) on the row's own
/// walk, never into a typed word's: an arrow beside the word you just typed
/// does not restart the word's grace, move its bounds, or change when it
/// swooshes — a hop lifts no other mark's finger.
pub const WAKE_HOP_LIFE_SHARE: f32 = 0.5;

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

/// **THE COHORT'S CLOCK** (2026-09-08): the instant a cell's expiry is
/// measured from — its own birth or its cohort's last live typing key,
/// whichever is later. Every key in a live run refreshes the whole run, so a
/// cell born with 1.70 s of life keeps 1.70 s of life AFTER THE LAST KEY of
/// its run rather than after its own; a mark only ages once the hand stops.
/// A cell whose cohort is gone (never on the frame path — `retire` drops the
/// cells with the cohort) falls back to its birth.
///
/// Zero-alloc and O(cohorts) per call; a row has one cohort per typing burst,
/// so this is a handful of compares.
#[must_use]
pub fn live_since(cohorts: &[Cohort], cell: &Cell) -> Instant {
    cohorts
        .iter()
        .find(|c| c.id == cell.cohort)
        .map_or(cell.born, |c| cell.born.max(c.alive_at))
}

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
    /// When the cell was laid (the echo frame). Drives the 18 ms `edge-in`;
    /// the expiry melt runs from [`live_since`] — this, or the cohort's last
    /// live key, whichever is later.
    pub born: Instant,
    /// Total life in seconds, priced at birth from `Spine::birth_disp` — and
    /// measured from the COHORT'S clock, not this cell's (2026-09-08): the
    /// life a cell was born with is the life it has left after the LAST key
    /// of its run, so no cell can run out under a hand that is still typing.
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
    ///
    /// It is also the clock every cell's EXPIRY runs from ([`live_since`],
    /// 2026-09-08): a key refreshes the whole run's life, not only the cell
    /// it lays. Monotone — a sweep laid at an older key's stamp never
    /// rewinds it.
    pub alive_at: Instant,
    /// Set by a real jump ([`Ribbon::abandon`]): this cohort is leaving on its
    /// own rewound clock, and a typing key elsewhere no longer refreshes it —
    /// the band under the hand's OLD position must go out while the new one
    /// is being laid, not wait for the next finger-lift.
    pub abandoned: bool,
    /// Minted by a wake ([`Ribbon::wake`]) and never typed into. A HOP's
    /// new cells join only such a cohort, or mint one — so an arrow beside a
    /// live word touches neither the word's clock nor its bounds (its cells
    /// it leaves exactly as they are) — and a typed key laid into a wake
    /// cohort clears the flag: the hand is on it now.
    pub wake: bool,
    /// The caret column the retract pulls this cohort toward, captured the
    /// frame it entered its retract ([`Ribbon::advance_swoosh`]) and held
    /// until a key brings the cohort back to laying. T5 says a leaving mark
    /// moves TOWARD THE CARET, and it does — toward the hand that let go of
    /// it: a mark read against the LIVE caret lurched across the row the
    /// frame a Ctrl-A landed while it was half-way into its old spot (13
    /// cells at 1.25 s idle), which is the step "smooth transitions" forbids.
    pub retract_col: Option<u16>,
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

    /// Move every lane with a scroll of `rows` (seam point 12): a lane whose
    /// row leaves the grid retires into the spare pool, cleared over its own
    /// `touched` list — the O(marks) reset, and no lane is minted again on
    /// the next plan; the rest keep their columns and take the new row, so
    /// [`FieldIndex::at`] answers for the MOVED cell between the scroll and
    /// the next plan, exactly as a newest-first scan of the translated pool
    /// would.
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
///
/// **A FUNCTION OF THE FOREGROUND ALONE.** [`BED_LUMA_MIN`] is the one
/// documented exception under it ("below this the ribbon stops obeying the
/// bar and simply takes the dimmest light that still reads") and
/// [`BED_LUMA_MAX`] the bar's own answer at a white foreground. The theme's
/// GROUND is not an input: a floor against it was tried and removed (see the
/// note above `BED_LUMA_MIN`'s neighbours) because wherever it exceeded the
/// bar it put the bed over the luminance the text can bear.
#[must_use]
pub fn bed_luma_budget(theme_fg: u32) -> f32 {
    let bar =
        (relative_luminance(theme_fg) + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05;
    // `clamp` is total here: both bounds are finite constants with
    // `BED_LUMA_MIN < BED_LUMA_MAX`, and `relative_luminance` of a byte
    // triple is finite.
    bar.clamp(BED_LUMA_MIN, BED_LUMA_MAX)
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
/// Since 2026-09-09 it is the LAST resort of [`onto_luma`], taken only from a
/// stop already at full value: white is the one currency that buys light at
/// the price of chroma, and the owner's "black gaps" were that price paid
/// where it did not have to be.
fn toward_white(rgb: u32, w: f32) -> u32 {
    let ch = |sh: u32| {
        let c = ((rgb >> sh) & 0xff) as f32;
        ((c + w * (255.0 - c)) + 0.5).clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Bisect a monotone one-parameter colour family for the brightest member
/// UNDER `target` relative luminance. 24 halvings resolve the parameter to
/// under 1e-7, which is finer than the byte quantization it feeds — and the
/// answer is `family(lo)`, the last parameter whose bytes measured under the
/// target, never the midpoint: the family is a step function of bytes, so
/// the midpoint's bytes can land a level OVER the target, which is exactly
/// the rounding the old 0.15 [`BODY_CONTRAST_GUARD`] was paying for. The
/// invariant `Y(family(lo)) < target` holds from `lo = 0` (black for a scale,
/// the colour itself for a walk toward white from under the target) and is
/// kept by every halving.
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

/// **A STOP PUT ON ONE RELATIVE LUMINANCE AT THE MOST CHROMA THAT LUMINANCE
/// ALLOWS** — the one family the bed and the hot edge both solve over.
///
/// One monotone family, bisected by [`solve_for_luma`]: from black, the stop
/// scaled UP through itself to its own hue at FULL VALUE (`k = 255 / max`,
/// the brightest colour with exactly this hue and this saturation), and only
/// past that point — only for a target the hue cannot reach at full value —
/// a walk toward white. The recipe this replaces (2026-09-09) walked toward
/// white from the stop ITSELF, so a dark stop paid in chroma for light it
/// could have had in value: indigo `#4B0082` at the bar's `Y 0.097` came out
/// `(118, 60, 160)`, `S 0.61`, a grey lavender — the owner's "black gap".
/// Through this family it is `(127, 0, 219)`, `S 0.98`: the same hue, the
/// same luminance, the same contrast under any text, and every bit of the
/// chroma the hue owns. The fg-over-ribbon bar is a function of `Y` alone,
/// so this costs it nothing, on any theme.
///
/// Blue `#0000FF` is the one anchor whose full value is still under the bar
/// (`Y 0.072`), so it alone still takes some white — `(45, 45, 255)`,
/// `S 0.82`, exactly what it took before, because it was already at full
/// value. A stop BRIGHTER than the target scales DOWN, byte for byte as
/// before (its `k` lands under `1`): red, orange, yellow, green and every
/// warm mid are untouched by this change, and the bar-side pins beside them
/// (`the_dimmest_stop_…`, `a_bright_foreground_s_bed_…`) read the same
/// numbers. A black (no hue to spend) walks toward white as it always did;
/// the arc has none.
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

/// **THE BED'S ON-GLASS INK** — an arc colour put on ONE composited luminance,
/// `budget` (see [`bed_luma_budget`]), at the most chroma that luminance
/// allows.
///
/// Two moves, one bar:
///
/// * [`BED_SAT_FLOOR`] first — the crossing's authored chroma dip is given
///   back at the bed's own read, hue and value untouched;
/// * then [`onto_luma`] — a colour BRIGHTER than the budget (red, yellow,
///   green) is scaled down uniformly, so its hue is bit-exact and only its
///   value moves; a colour DARKER than it (blue, indigo, violet) is lifted
///   through its own hue to full value and only then toward white, so it
///   arrives as a vivid stop instead of the navy smudge (v1) or the grey
///   lavender (v2 before 2026-09-09) the two measured defects report.
///
/// The consequence is that **every stop of the arc composites at the same
/// weight**, and one coverage ceiling is legal for the whole arc — which is
/// why the 33-entry table §19.1 deletes is not needed and is not carried. That
/// is the NAVY half of the measured "dim and muddy" defect (a dark stop as a
/// smudge) and not the OLIVE half: the weight is the bar's own, and on the
/// default theme a warm stop at that weight is `(80, 80, 3)` composited — the
/// luminance v1's table produced, with more chroma. A brighter warm mid is a
/// ruling on the bar (module doc: the bar STANDS, and the bed sits AT it
/// since 2026-09-08), not on this recipe.
///
/// It also makes the per-hue "equal-ledge" core law v1 needed unnecessary: the
/// per-row melt step is proportional to the position's peak premultiplied
/// level, and after this recipe every position has the same one.
#[must_use]
pub fn bed_ink(rgb: u32, budget: f32) -> u32 {
    onto_luma(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR), budget)
}

/// **THE HOT EDGE'S INK** — the stop, lifted until it is at least
/// [`HOT_EDGE_LUMA_FLOOR`] luminous ([`onto_luma`]: through its own hue at
/// full value, then toward white); a stop already brighter is carried pure.
/// The bed's recipe equalizes the arc DOWN onto one legibility budget; this
/// one equalizes it UP onto one heat, and for the same reason in the other
/// direction — a blue hairline at blue's own `Y 0.072` over a blue bed whose
/// blue channel is already saturated adds nothing the eye can see, and
/// "brighter than the bed" has to be true of every stop or the edge reads as
/// speed only on the warm half of the arc. It takes [`BED_SAT_FLOOR`] too, so
/// the hairline over the seam is a teal line over a teal bed, not a grey one.
///
/// Additive light, so it is not under the bed's bar: the hairline is 1 px on
/// the ribbon's top edge, priced by [`HOT_EDGE_COV_MAX`] inside the
/// transient cap, and ledger-held over the row above's descenders (§4.1).
#[must_use]
pub fn hot_edge_ink(rgb: u32) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    if relative_luminance(rgb) >= HOT_EDGE_LUMA_FLOOR {
        rgb
    } else {
        onto_luma(rgb, HOT_EDGE_LUMA_FLOOR)
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
    /// The hot edge's twin table ([`hot_edge_ink`]), same positions — solved
    /// beside the bed's so the hairline costs one lerp per vertex too.
    hot: Vec<u32>,
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
        for i in 0..BED_INK_LUT_LEN {
            let x = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
            let arc = spectrum(x);
            self.lut.push(if cfg.dark_theme {
                bed_ink(arc, budget)
            } else {
                role.ink(arc)
            });
            self.hot.push(hot_edge_ink(arc));
        }
        self.key = Some(key);
    }

    /// The ink at walk position `t`. The walk is unbounded, so it is folded
    /// through `meteor::tri` — the SAME reflection §6.4's arc uses, which is
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

    /// The instant `cell`'s expiry is measured from — see [`live_since`].
    #[must_use]
    pub fn live_since(&self, cell: &Cell) -> Instant {
        live_since(&self.cohorts, cell)
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
    /// row), lays the WAKE of a same-row navigation or synthetic jump
    /// ([`Ribbon::wake`], R5 — the corridor it crossed, newest at the
    /// landing, taking over what the live band already lit) and then
    /// ABANDONS every other cohort straight into its retract (see
    /// [`RETRACT_START_S`]); a same-row hop under the jump floor with a
    /// navigation witness (an arrow) lays a shorter wake in a cohort of its
    /// own and abandons nothing — a click inside the word is an edit, and
    /// the swoosh's own grace handles it; a band already leaving is left on
    /// its own clock by either arm; a PTY-driven or return-licensed jump still only abandons
    /// (no credit, no wake); [`Event::Focus`] takes the 300 ms ember and, on
    /// regain inside it, the `edge-in` rearm; [`Event::ReducedMotion`] and
    /// [`Event::Return`] lay nothing. T1 holds: the wake is born only on an
    /// observed, licensed move whose witness was a key.
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
                // A typed echo's own motion is inert here: the key laid its
                // cell, and the caret mirror is all that moves.
                if licence != Licence::Typed {
                    let same_row = to.0 == from.0;
                    let jump = !same_row || to.1.abs_diff(from.1) >= JUMP_MIN_CELLS;
                    if jump {
                        // THE WAKE, then the abandon that spares it. A row
                        // change has no corridor on one row (an Up-arrow
                        // recall must not paint the line it lands on), and a
                        // PTY cascade or a Return earns no credit — they keep
                        // the old law: abandon, lay nothing.
                        let wakes =
                            same_row && matches!(licence, Licence::Nav | Licence::Synthetic);
                        let keep = if wakes {
                            self.wake(to.0, from.1, to.1, at, ctx, WAKE_LIFE_S, true)
                        } else {
                            None
                        };
                        self.abandon(at, keep);
                    } else if same_row && licence == Licence::Nav && from.1 != to.1 {
                        // AN ARROW'S HOP: a short wake behind the caret, no
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
                    && at
                        .saturating_duration_since(live_since(&self.cohorts, c))
                        .as_secs_f32()
                        < c.life_s
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

    /// **THE SPINE A BIRTH IS PRICED AT** on this tick: [`Ctx::birth_disp`]
    /// (the resume-floored spine), floored again by a live mend
    /// ([`Ctx::mend`] — §23's addendum "The mend", 2026-09-08). A typed key
    /// that fixes a typo within a breath is born at `max(birth_disp,
    /// mend.disp)`, the momentum its Backspace interrupted, so the fixed
    /// cell's `cov0`, life and hot-edge price match the run it re-joins and
    /// the bed shows no dip where the fix went in. A v2-local birth price,
    /// like `METRIC_GAIN`: the spine's metric and follower are untouched, and
    /// every live-spine consumer (the wave, the wedge, the caret, the stars'
    /// envelope) keeps reading the honest [`Ctx::disp`].
    #[inline]
    fn birth_price(ctx: &Ctx<'_>) -> f32 {
        clamp01(
            ctx.mend
                .map_or(ctx.birth_disp, |m| ctx.birth_disp.max(m.disp)),
        )
    }

    /// The life one cell earns, priced at BIRTH (§4's chain law, verbatim)
    /// — and spent from the cohort's clock ([`live_since`]): it is how long
    /// the cell outlives the LAST key of its run.
    fn cell_life(&self, at: Instant, ctx: &Ctx<'_>) -> f32 {
        let full = ctx.cfg.duration.as_secs_f32().max(0.001);
        let base = (full * LIFE_DURATION_GAIN).clamp(LIFE_BASE_MIN, LIFE_BASE_MAX);
        let d = Self::birth_price(ctx);
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
        let birth_disp = Self::birth_price(ctx);
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
        self.place(idx, cell, at);
    }

    /// Put ONE built cell into the pool under the cohort at `idx`, laid at
    /// `at`: one owner per `(row, col, cohort)`, the cohort's bounds grown
    /// over it, and the cohort clocks that the lay refreshes.
    fn place(&mut self, idx: usize, cell: Cell, at: Instant) {
        let (row, col, typing) = (cell.row, cell.col, cell.typing);
        // ONE OWNER PER CELL. A retype replaces the light on that cell rather
        // than stacking a second body under it — the shadowed cell could never
        // be seen, and leaving it in the pool is how v1's field scan grew.
        if let Some(slot) = self
            .cells
            .iter()
            .position(|c| c.row == row && c.col == col && c.cohort == cell.cohort)
        {
            self.cells.remove(slot);
        }
        self.cells.push(cell);
        let coh = &mut self.cohorts[idx];
        coh.col0 = coh.col0.min(col);
        coh.col1 = coh.col1.max(col + 1);
        if typing {
            // The hand is on this cohort now, whatever minted it.
            coh.wake = false;
            // ONE FINGER: a live key holds EVERY cohort that has not been
            // abandoned in its laying phase, so the mark's earlier rows do
            // not start their exit swoosh while the hand is still typing on
            // the current one (see `Cohort::alive_at`).
            for held in &mut self.cohorts {
                if !held.abandoned {
                    held.alive_at = held.alive_at.max(at);
                    held.phase = Phase::Laying;
                    held.retract_col = None;
                }
            }
        } else {
            // A WAKE CELL holds only its OWN cohort — and that is always a
            // wake cohort ([`Cohort::wake`]: a jump mints one, a hop joins
            // or mints one, never a typed word's): an arrow held down keeps
            // the wake it is laying out of the swoosh until the arrow stops,
            // and nothing else — a jump builds no momentum and lifts no other
            // mark's finger (`typing` is what the one-finger law reads).
            coh.alive_at = coh.alive_at.max(at);
            coh.phase = Phase::Laying;
            coh.retract_col = None;
        }
    }

    /// **THE WAKE** (R5) — lay the corridor a same-row move just crossed on
    /// `row`, from column `from` to the landing `to`, newest at the landing.
    /// The landing cell is included, so the caret block's fill, the train's
    /// phase-lock (`field_at(landing)`, §6.4) and the bed under the caret read
    /// ONE walk; the origin cell is the far end. New cells are born
    /// [`WAKE_BORN_LAG_S`] past the move and live `life_s`; at most
    /// [`WAKE_MAX_CELLS`] of them, counted back from the landing.
    ///
    /// The wake's cohort takes THE BAND'S OWN WALK ORIGIN
    /// ([`Ribbon::wake_origin`]: the row's freshest cohort's `anchor_col`
    /// and `t0`, not its `t` read at the landing) — `walk_t` is `d/16` for
    /// sixteen cells and `1/36` a cell after, so only a cohort anchored where
    /// the band is anchored gives `t_at(col)` the band's number at EVERY
    /// column (C2); a wake anchored at the landing agreed with the band on a
    /// Ctrl-A to its origin and repainted stops past the kink on a Ctrl-E
    /// across it.
    ///
    /// `takeover` is the JUMP's arm: the corridor is minted as a cohort of
    /// its own and every cell the live band already lights is HANDED OVER,
    /// not re-lit: same stop, the old cell's own birth (no second attack),
    /// priced at exactly the light it has now, and the old cell leaves the
    /// pool. The abandon that follows then finds nothing under the corridor
    /// to retract, and the eye sees no step — the band simply outlives the
    /// jump on the wake's clock. Lit cells beyond the cap are taken over too:
    /// the cap bounds NEW light, never light already on the glass.
    ///
    /// A band ALREADY LEAVING is not an owner to take over: a cell whose
    /// cohort is retracting or abandoned stays on that clock, and while it is
    /// still lit nothing is laid under it — the wake lights only the cells
    /// the drain has emptied (and, past the drain's end, all of them). Taking
    /// a half-drained cell over froze it at its instantaneous level for a
    /// whole wake life beside cells re-lit at full: the frozen partial ramp
    /// the reviewer reproduced at 1.0 / 1.25 / 1.4 s idle. So a Ctrl-A a
    /// second after the last key shows the band finishing its exit and a
    /// fresh wake rising under the cells it has already left, the two never
    /// sharing a cell; the wake's width grows with the idle from nothing at
    /// 1.0 s to the full cap once the band is gone.
    ///
    /// Without `takeover` (a hop) a cell a live cell owns is left as it is,
    /// and the new cells join the row's adjacent WAKE cohort or mint one on
    /// the same origin ([`Ribbon::join_wake_cohort`]) — never a typed
    /// word's, whose clock and bounds a hop must not touch.
    ///
    /// Every wake cell is `typing: false` — load-bearing: the one-finger hold
    /// in [`Ribbon::place`] and the momentum spine read that flag, and
    /// `mod.rs`'s "a jump builds no momentum" depends on it (pinned by
    /// `a_wake_is_never_typing_and_holds_no_other_cohort`).
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
        let born_new = at
            .checked_add(std::time::Duration::from_secs_f32(WAKE_BORN_LAG_S))
            .unwrap_or(at);
        let birth_disp = clamp01(ctx.birth_disp);
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let (anchor_col, t0) = self.wake_origin(row, to, at);
        let minted = takeover.then(|| self.mint_cohort(row, to, anchor_col, t0, at, true));
        // Far end first, landing LAST: the landing is the newest cell, so it
        // is the run's head (`head_col`) and the hot edge sits by the caret.
        for k in (0..=span).rev() {
            let col = if leftward { to + k } else { to - k };
            // The cell that owns this column now: its newest un-retracting
            // cell (the pool is append-ordered; `build` reads the same one).
            let owner = self
                .cells
                .iter()
                .rposition(|c| c.row == row && c.col == col && c.retract_at.is_none());
            let (born, cov0) = match owner {
                Some(i) => {
                    let cell = self.cells[i];
                    let env = self.env_of(ctx, &cell);
                    let leaving = self
                        .cohorts
                        .iter()
                        .find(|c| c.id == cell.cohort)
                        .is_none_or(|c| c.abandoned || c.phase.is_retracting());
                    if env <= 0.0 {
                        // Melted to nothing since the last plan, or drained
                        // by its cohort's retract: a hole, not an owner.
                        if k >= WAKE_MAX_CELLS {
                            continue;
                        }
                        (born_new, cov0_new)
                    } else if leaving || !takeover {
                        // On its own clock (a band already leaving), or
                        // under a hop (a live cell is left as it is): lay
                        // nothing here.
                        continue;
                    } else {
                        let old = self.cells.remove(i);
                        (old.born, old.cov0 * env)
                    }
                }
                None if k >= WAKE_MAX_CELLS => continue,
                None => (born_new, cov0_new),
            };
            let idx = match minted {
                Some(idx) => idx,
                None => self.join_wake_cohort(row, col, anchor_col, t0, at),
            };
            let cohort = self.cohorts[idx];
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born,
                life_s,
                cov0,
                typing: false,
                retract_at: None,
                birth_disp,
            };
            self.place(idx, cell, at);
        }
        minted.map(|idx| self.cohorts[idx].id)
    }

    /// The walk origin a wake takes on `row`, as `(anchor_col, t0)`: the
    /// row's freshest cohort inside the chain window — abandoned, retracting
    /// or not: the band it continues may be leaving — shares its origin
    /// outright, so `t_at(col)` is the very number the band has at every
    /// column and continues past both its ends on the same walk. With no
    /// cohort on the row, the freshest anywhere is continued from one past
    /// its rightmost column, anchored at `landing`, as
    /// [`Ribbon::join_cohort`] does; red at the landing when nothing is laid.
    fn wake_origin(&self, row: u16, landing: u16, at: Instant) -> (u16, f32) {
        let fresh =
            |c: &&Cohort| at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX;
        if let Some(c) = self
            .cohorts
            .iter()
            .filter(fresh)
            .filter(|c| c.row == row)
            .max_by_key(|c| c.alive_at)
        {
            return (c.anchor_col, c.t0);
        }
        let t0 = self
            .cohorts
            .iter()
            .filter(fresh)
            .max_by_key(|c| c.alive_at)
            .map_or(0.0, |c| c.t_at(c.col1));
        (landing, t0)
    }

    /// The cohort a HOP's new cell at `col` joins: a WAKE cohort on the same
    /// row, not abandoned, inside the chain window, adjacent to or containing
    /// `col` — a held arrow's own trail, or a jump's wake it runs into.
    /// Otherwise a new wake cohort on the row's origin `(anchor_col, t0)`.
    /// Never a typed cohort: the hop leaves the word's clock and bounds alone.
    fn join_wake_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        t0: f32,
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
        self.mint_cohort(row, col, anchor_col, t0, at, true)
    }

    /// Push a new cohort covering `col` on `row` with the walk origin
    /// `(anchor_col, t0)`, born and alive at `at`; returns its index.
    fn mint_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        t0: f32,
        at: Instant,
        wake: bool,
    ) -> usize {
        let id = self.next_cohort;
        self.next_cohort = self.next_cohort.wrapping_add(1);
        self.cohorts.push(Cohort {
            id,
            row,
            col0: col,
            col1: col + 1,
            anchor_col,
            t0,
            born: at,
            alive_at: at,
            abandoned: false,
            wake,
            retract_col: None,
            phase: Phase::Laying,
        });
        self.cohorts.len() - 1
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
        self.mint_cohort(row, col, col, t0, at, false)
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

    /// A real jump ABANDONS the band: every cohort still in its grace or
    /// reach — except the jump's own wake, `keep` — is sent straight into
    /// its retract by rewinding `alive_at` to [`RETRACT_START_S`] ago. The
    /// light leaves CONTINUOUSLY from the level it has (the retract's `spend`
    /// starts at exactly `1.0`), where v1's life clamp stepped the melt on
    /// the jump frame; and a cohort already retracting is left on its own
    /// clock. Under the corridor the wake has already taken the band's cells
    /// over ([`Ribbon::wake`]), so what retracts here is only the light the
    /// jump left behind beyond it — and a band that was already leaving
    /// when the jump came, which the wake did not touch.
    fn abandon(&mut self, at: Instant, keep: Option<u32>) {
        let Some(rewound) = at.checked_sub(std::time::Duration::from_secs_f32(RETRACT_START_S))
        else {
            return;
        };
        for coh in &mut self.cohorts {
            if keep == Some(coh.id) {
                continue;
            }
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
            // THE RETRACT'S TARGET is the caret's column the frame the
            // retract begins, and it stays: the mark is drawn back into the
            // hand that let go of it, not toward wherever the caret goes
            // next (see `Cohort::retract_col`).
            if phase.is_retracting() && self.cohorts[i].retract_col.is_none() {
                self.cohorts[i].retract_col = self.caret.map(|(_, col)| col);
            }
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
            if now
                .saturating_duration_since(live_since(cohorts, cell))
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
        // is `WAVE_AMP_CELLS · ch · disp` (0.110 ch — twice v1's, 2026-09-08),
        // so when the hand lifts the mark flattens. The head is pinned
        // (`WAVE_HEAD_PIN`) so the cell under the hand never moves. Under
        // reduced motion there is no wave at all (§6.11).
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
        // A wake cell laid AHEAD of its birth (`WAKE_BORN_LAG_S`) is dark
        // until it is born: the attack starts at `born`, never before it.
        if now < cell.born {
            return 0.0;
        }
        let reduced = ctx.cfg.reduced_motion;
        let coh = self.cohorts.iter().find(|c| c.id == cell.cohort);
        // The attack runs from the cell's own birth; the expiry runs from the
        // COHORT's clock (`live_since`): a key refreshes the whole run.
        let age = now.saturating_duration_since(cell.born).as_secs_f32();
        let spent = now
            .saturating_duration_since(coh.map_or(cell.born, |c| cell.born.max(c.alive_at)))
            .as_secs_f32();
        let u = (spent / cell.life_s.max(1e-3)).clamp(0.0, 1.0);
        // UNDER REDUCED MOTION every mark is static and takes the theme's ONE
        // linear fade over the last `REDUCED_MOTION_FADE_MS` of whatever ends
        // it (§6.11) — the natural life, the retract stamp, the ember and the
        // swoosh all resolve through `reduced_fade`; the 18 ms `edge-in`
        // stays, because it is the one sanctioned attack and not a motion.
        let mut env = (BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age))
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
        if let Some(coh) = coh
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
    /// nothing. The caret it pulls toward is the one the retract BEGAN
    /// under ([`Cohort::retract_col`]), so a jump landing mid-retract does
    /// not lurch the leaving mark across the row. Per COHORT, never per row:
    /// a cohort still being typed beside one that is swooshing keeps every
    /// boundary where its letters are. Under reduced motion nothing moves
    /// (§6.11).
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
        let target = coh.retract_col.or_else(|| self.caret.map(|(_, col)| col));
        let caret_x = target.map_or(x, |col| {
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

    /// **REFINEMENT A** (§4.1) — a 1-px additive hairline IN THE SPECTRUM
    /// along the ribbon's TOP edge, from the head cell back
    /// [`HOT_EDGE_CELLS`] cells: every vertex carries the stop under it,
    /// lifted to [`HOT_EDGE_LUMA_FLOOR`] ([`BedInkLut::hot_at`]), so at the
    /// hand it IS the head's stop, hot — brighter than the bed on every stop
    /// of the arc. (`#FFFFFF` at 38 until 2026-09-08; the owner: "I want the
    /// meteor to have rainbow! be a bigger more special rainbow impact!".)
    ///
    /// It is drawn as a 2-vertex-per-cell `comet_beam` hairline (one vertex
    /// per cell BOUNDARY) sampled at the same `spine − up` the body used,
    /// because the top edge carries the 0.75-cycle wave (≈ 2 px at `ch 18`,
    /// 3 px at retina) and three axis-aligned rects cannot follow it. It is
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
        // hairline that ignored it rode the retract at full coverage over a
        // body that had spent to zero, then snapped off with the cells.
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
            // The expiry clock is the cohort's (`live_since`), as `env_of`
            // and `retire` read it.
            let age = since(live_since(&self.cohorts, cell));
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

    /// **A SCROLL CARRIES THE RIBBON WITH ITS TEXT** (seam point 12): move
    /// every laid cell by a scroll of `rows` rows, with everything that is
    /// addressed by a cell's row — the cohorts, the field index the other
    /// producers read, and the caret the field is resolved against. What
    /// leaves the grid is dropped rather than clamped: a ribbon pinned to row
    /// 0 by a clamp is light on a line nobody typed.
    ///
    /// **There is no pixel to move, so no cell height is taken.** Every piece
    /// of ribbon STATE is addressed by grid row and clock: a cell is `(row,
    /// col)` plus its instants and its prices, a cohort is a row and a column
    /// span, the retract clocks and the focus ember are instants, and the
    /// wave's phase is the spine's ([`Ctx::phase`]), not the mark's. The pixel
    /// geometry — [`Ribbon::plan`], the runs, the hot edge's hairline, the
    /// bands stardust reads — is this frame's scratch, rebuilt from the cells
    /// by the next tick's [`Ribbon::plan`] before any producer reads it, so it
    /// is dropped here. The seam's `cell_h` belongs to the meteor's and the
    /// sky's halves, whose marks are window-absolute px; a parameter this
    /// half accepted and ignored read as a pixel path that did not exist.
    ///
    /// The index and the caret are TRANSLATED, not dropped. Between this call
    /// and the next plan the ribbon is still asked — the engine seeds the
    /// tick's `caret_t` from [`Ribbon::field_at_caret`] BEFORE it plans, and
    /// [`Ribbon::field_at`], [`Ribbon::head_rgb`] and [`Ribbon::caret`] are
    /// public reads the host may make between its `note_scroll` and its tick
    /// — and the honest answer is the MOVED cell's, which a reset index and a
    /// caret left on the pre-scroll row could not give (the index answered
    /// `None` for the moved cell, and the caret fell through to the newest
    /// key's stop). On glass the old spelling never showed: the same tick's
    /// plan rebuilt both before anything drew. The caret is a position, not
    /// a mark, so it saturates at row 0 as the engine's does; the next plan
    /// re-observes it.
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
        self.index.translate(rows);
        self.plan.clear();
        self.runs.clear();
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
    /// The owner's theme (`theme = "Nord"`): the pair the black-gaps report
    /// was made on and re-measured against.
    const NORD_BG: u32 = 0x002E_3440;
    const NORD_FG: u32 = 0x00D8_DEE9;

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
            mend: None,
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
    fn the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar() {
        // "Dim and muddy" gets a NUMBER. Under L3's 5.25:1 bar every stop is
        // put on `bed_luma_budget(fg)`, and a warm stop at that luminance IS
        // an olive: on the default theme a full-coverage yellow composites at
        // `(80, 80, 3)`, at or below the ≈ 84 v1's coverage table produced.
        // The module does not fix that half of the measured defect, and this
        // pin is what keeps the module doc from claiming it does.
        //
        // RE-PINNED 2026-09-08, 78 → 80 (measured 79 → 80): the owner struck
        // the restraint under the bar ("I feel like you are diminishing the
        // specialness and emphasis of this theme? why?") and kept the bar —
        // the 0.15 guard is the 0.05 the rounding needs, the solver answers
        // from under its target, and the clamp is the bar's own answer at a
        // white foreground. On the default theme that is one level; the
        // bright-foreground gain is pinned beside this in
        // `a_bright_foreground_s_bed_takes_the_bar_s_whole_budget_not_a_tenth`.
        //
        // TWO-SIDED on purpose. A ruling that relaxes the bar for the bed
        // over blank cells (REJECTED 2026-09-06, module doc) would land the
        // warm mids above 84 and must re-pin this line; a change that dims the
        // bed trips the floor. Either way the number moves HERE first, not in
        // a screenshot.
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

    /// The worst contrast the bed can composite against `fg` over `bg` —
    /// every position of the arc, every level the emitter can reach (the
    /// body's request and the strip's accent, to the ledger's frame top).
    fn worst_bed_contrast(fg: u32, bg: u32) -> f32 {
        worst_bed_contrast_at(fg, bg, bed_luma_budget(fg))
    }

    /// The same, with the bed put on a stated luminance instead of the
    /// theme's own budget — what a different budget WOULD read at.
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
        // THE OWNER, 2026-09-08: "I feel like you are diminishing the
        // specialness and emphasis of this theme? why?" — and the one
        // restraint that stays is the bar itself, not a margin under it. The
        // bed carried a 0.15 guard ("more than all three [rounding costs]
        // can spend together") and composited at 5.3729:1 at its worst.
        // Measured: `solve_for_luma` answering from UNDER its target makes
        // the ink's own rounding cost nothing, and the composite's one level
        // of rounding costs 0.05 of a ratio point at the dimmest theme above
        // the floor (`letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor`)
        // — so that is the guard, and the bed sits within a tenth of the bar.
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
        // THE NET UNDER THE GUARD. 67 foregrounds — the greys `#909090` to
        // `#FFFFFF` in steps of three, 27 tints from `{A0, D0, FF}³`, the
        // shipped defaults — over four grounds, each only where the ground
        // sits under the bed's own ceiling (a ground brighter than the bed is
        // the theme's contrast, not the bed's), through the ledger's frame
        // top plus one Bayer level. Foregrounds the floor catches
        // (`bed_luma_budget` clamped up to `BED_LUMA_MIN`, fg ≤ ≈ `#999999`)
        // are the documented exception — "below this the ribbon stops obeying
        // the bar" — and are skipped. This sweep is what sized
        // `BODY_CONTRAST_GUARD`: at 0 the worst was 5.2019:1 (fg `#A0A0A0`,
        // one level of composite rounding at a 0.026 budget); 0.05 is the
        // first step of the sweep that holds — and it is a law the 0.15 guard
        // did NOT hold, because its solver could answer a level over its
        // target (5.2039:1 at fg `#999999` before).
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
            // Floor-clamped (`BED_LUMA_MIN`) is the documented exception:
            // those foregrounds are skipped whole. This and the
            // ground-brighter-than-the-bed skip below are the ONLY two skips
            // — a third was added on 2026-09-09 to hide the pairs a ground
            // floor lifted over the bar, and it was taken out with the floor.
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
        // MORE COLOUR where the bar allows it (the owner, 2026-09-08: "make
        // this rainbow theme truly magical and special and dynamic and
        // beautiful"). `BED_LUMA_MAX` was 0.100 — a clamp that bound on every
        // foreground brighter than ≈ `#D8D8D8`: the offline renderer's
        // `#E8E8F0` and a white foreground both solved to the clamp, under
        // the bar's own answers (0.114 and 0.150). The ceiling is now the
        // bar's answer at a white foreground, so under the bar the clamp
        // binds nowhere.
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

    // -- the black gaps (2026-09-08): chroma, not luminance ------------------

    #[test]
    fn the_bed_s_dark_stops_arrive_at_full_chroma_not_walked_grey() {
        // THE OWNER, 2026-09-08: "there are gaps black gaps in the rainbow a
        // few characters back from the cursor" — measured on Nord as indigo
        // composited `(82, 60, 136)`, S 0.61, next to a blue at 210–230. At
        // the bar's own luminance indigo's hue owns `(127, 0, 219)`; the old
        // walk toward white spent that chroma for nothing the bar wanted.
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
        // The other "gap": the green→blue crossing carried the arc's authored
        // S 0.53 (`SPECTRUM_CROSSING_ROOF`, the retired cyan census's bound)
        // and composited `(45, 92, 93)` on Nord — the greyest cell on a typed
        // line. `BED_SAT_FLOOR` gives the seam its neighbours' chroma back at
        // the bed's own read; the arc's hue pacing is untouched.
        use crate::spectrum::{spectrum_crossing_position, spectrum_crossing_width};
        let mid = spectrum_crossing_position();
        let half = spectrum_crossing_width();
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
                "{name}: the crossing's least saturated composite is S {worst:.3} (#{px:06X}) — the roof's S 0.53 is showing through the bed"
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
        // A floor against the ground (`BED_GROUND_CONTRAST_MIN` 1.4:1,
        // 2026-09-09) was taken out: wherever it exceeded the bar it lifted
        // the bed over the luminance the text can bear. The budget is the
        // bar's answer alone, clamped to [`BED_LUMA_MIN`, `BED_LUMA_MAX`] —
        // on the themes the floor would have moved as much as on the ones it
        // would not.
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
            // The floor that was removed would have bound here: prove the
            // control — on One Dark and Solarized it would have lifted the
            // bed over the bar, and the text over it under 5.25:1.
            let floor = (relative_luminance(bg) + 0.05) * 1.4 - 0.05;
            if floor > budget {
                assert!(
                    worst_bed_contrast_at(fg, bg, floor) < BODY_CONTRAST_BAR,
                    "{name}: the control — a 1.4:1 ground floor ({floor:.4}) would not have broken the bar here"
                );
            }
            // …and the bar's own budget holds the bar wherever the bar is
            // obeyed at all: a foreground clamped up to `BED_LUMA_MIN`
            // (Solarized Dark's `#839496` solves to 0.012) is the documented
            // exception, and is under 5.25:1 by design.
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
        // and the crossing LOOK like; it must not change what the text over
        // them reads at — the bar is a function of Y alone and the solver
        // still answers from under it. Nord, every position, every level.
        let worst = worst_bed_contrast(NORD_FG, NORD_BG);
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "Nord: the bed composited to {worst:.4}:1; the bar is {BODY_CONTRAST_BAR}:1"
        );
        assert!(
            worst - BODY_CONTRAST_BAR <= 0.10,
            "Nord: the bed's worst stop composites at {worst:.4}:1 — light left under the bar"
        );
        // …and the scanner's own colour floor (paint-conformance `SAT_MIN`
        // 60 / `SAT_SPREAD` 40) is cleared by every stop at the cap and at
        // the cold key's share of it.
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

    /// **THE INK TABLE** — not a pin: every anchor and the crossing's roof,
    /// the bed's ink before (the walk toward white from the stop itself) and
    /// after (`onto_luma` + `BED_SAT_FLOOR`) on Nord and the default theme,
    /// and the hot edge's ink the same way — the numbers the module doc and
    /// `RAINBOW-KITTY-V2.md` quote, printed from the code that makes them.
    ///
    /// `targo --unverified test -p aterm-effects --lib -- --ignored --nocapture the_bed_and_hot_edge_ink_table`
    #[test]
    #[ignore = "a table for the record, not a pin"]
    fn the_bed_and_hot_edge_ink_table() {
        use crate::spectrum::{SPECTRUM_ANCHORS, SPECTRUM_CROSSING_ROOF};
        let old_bed = |rgb: u32, budget: f32| {
            if relative_luminance(rgb) > budget {
                solve_for_luma(|k| scale_rgb(rgb, k), budget)
            } else {
                solve_for_luma(|w| toward_white(rgb, w), budget)
            }
        };
        let old_hot = |rgb: u32| {
            if relative_luminance(rgb) >= HOT_EDGE_LUMA_FLOOR {
                rgb
            } else {
                solve_for_luma(|w| toward_white(rgb, w), HOT_EDGE_LUMA_FLOOR)
            }
        };
        let rgb = |c: u32| {
            format!(
                "({:3},{:3},{:3})",
                (c >> 16) & 0xff,
                (c >> 8) & 0xff,
                c & 0xff
            )
        };
        let names = [
            "red", "orange", "yellow", "green", "blue", "indigo", "violet",
        ];
        let mut stops: Vec<(String, u32)> = names
            .iter()
            .zip(SPECTRUM_ANCHORS)
            .map(|(n, c)| ((*n).to_string(), c))
            .collect();
        for (i, &c) in SPECTRUM_CROSSING_ROOF.iter().enumerate() {
            stops.push((format!("roof[{i}]"), c));
        }
        println!();
        for (theme, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ] {
            let budget = bed_luma_budget(fg);
            let cov = UNDER_COV_CAP as u8;
            println!("bed on {theme} (budget Y {budget:.4}):");
            println!(
                "  stop      arc                 old ink          S     @236 (peak)          new ink          S     @236 (peak)          Y new  fg/bed@236"
            );
            for (name, c) in &stops {
                let (o, n) = (old_bed(*c, budget), bed_ink(*c, budget));
                let (oc, nc) = (
                    over_premul(bg, premul_rgb(o, cov), cov),
                    over_premul(bg, premul_rgb(n, cov), cov),
                );
                println!(
                    "  {name:9} {} #{c:06X}  {} {:.2}  {} ({:3})  {} {:.2}  {} ({:3})  {:.4} {:.3}",
                    rgb(*c),
                    rgb(o),
                    sat(o),
                    rgb(oc),
                    max_channel(oc),
                    rgb(n),
                    sat(n),
                    rgb(nc),
                    max_channel(nc),
                    relative_luminance(n),
                    contrast(fg, nc)
                );
            }
        }
        println!("hot edge (floor Y {HOT_EDGE_LUMA_FLOOR:.2}):");
        println!("  stop      old ink   S     new ink   S");
        for (name, c) in &stops {
            let (o, n) = (old_hot(*c), hot_edge_ink(*c));
            println!(
                "  {name:9} #{o:06X}   {:.2}  #{n:06X}   {:.2}",
                sat(o),
                sat(n)
            );
        }
    }

    /// **THE WARM STOPS' FRONTIER** — not a pin: a table for the owner.
    ///
    /// Yellow, green and the crossing's cyan are at full chroma already; at
    /// the bar's luminance the sRGB transfer sets their peak channel (Nord:
    /// 90 / 102 / 98 ink, 87 / 98 / 95 composited). The only lever left is
    /// the bar, and that is a ruling. This prints, per stop and per step of
    /// the peak channel, what the foreground reads at over the composited
    /// bed at the body's cap and at the ledger's frame top, on Nord and the
    /// default theme, and names the pins each step would move.
    ///
    /// `targo --unverified test -p aterm-effects --lib -- --ignored --nocapture the_warm_stops_frontier_table`
    #[test]
    #[ignore = "a table for the owner's ruling, not a pin"]
    fn the_warm_stops_frontier_table() {
        type InkOf = fn(u32) -> u32;
        let stops: [(&str, InkOf); 3] = [
            ("yellow", |p| (p << 16) | (p << 8)),
            ("green", |p| p << 8),
            ("cyan", |p| (p << 8) | p),
        ];
        let themes = [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ];
        println!();
        println!(
            "bar {BODY_CONTRAST_BAR}:1 (+{BODY_CONTRAST_GUARD} guard); fg over the composited bed, at cov {} (the body's cap) and {} (the frame top)",
            UNDER_COV_CAP as u32, BODY_FRAME_TOP as u32
        );
        for (name, ink_of) in stops {
            for (theme, fg, bg) in themes {
                let budget = bed_luma_budget(fg);
                let at_bar = match name {
                    "yellow" => bed_ink(0x00FF_FF00, budget),
                    "green" => bed_ink(0x0000_FF00, budget),
                    _ => bed_ink(0x0000_FFFF, budget),
                };
                let p0 = max_channel(at_bar);
                println!();
                println!(
                    "{name} on {theme}: the bar's own ink is #{at_bar:06X} (peak {p0}); one level per row for the first six, then five"
                );
                println!(
                    "  ink peak | composited@236 (peak) | fg/bed @236 | fg/bed @251 | Y ink  | pins that move"
                );
                let mut p = p0;
                while p <= p0 + 45 && p <= 255 {
                    let ink = ink_of(p);
                    let cap = UNDER_COV_CAP as u8;
                    let top = BODY_FRAME_TOP as u8;
                    let c236 = over_premul(bg, premul_rgb(ink, cap), cap);
                    let c251 = over_premul(bg, premul_rgb(ink, top), top);
                    let (r236, r251) = (contrast(fg, c236), contrast(fg, c251));
                    let mut moves = Vec::new();
                    if r251 < BODY_CONTRAST_BAR {
                        moves.push("letters_stay_legible_* (5.25:1 at the frame top)");
                    }
                    if r236 < BODY_CONTRAST_BAR {
                        moves.push("…and at the body's cap");
                    }
                    if theme == "default"
                        && name == "yellow"
                        && !(79..=81).contains(&max_channel(c236))
                    {
                        moves.push("the_dimmest_stop_… (79..=81)");
                    }
                    if relative_luminance(ink) - budget > 0.004 {
                        moves.push("every_stop_of_the_arc_composites_at_one_weight (±0.004 Y)");
                    }
                    println!(
                        "  {p:8} | #{c236:06X} ({:3})        | {r236:8.3}    | {r251:8.3}    | {:.4} | {}",
                        max_channel(c236),
                        relative_luminance(ink),
                        if moves.is_empty() {
                            "none".to_string()
                        } else {
                            moves.join("; ")
                        }
                    );
                    p += if p < p0 + 6 { 1 } else { 5 };
                }
            }
        }
        // THE GROUND. The bed's budget is the bar's answer against the
        // foreground; how far it sits over the theme's own page is not a
        // law (a 1.4:1 floor was tried on 2026-09-09 and removed — it
        // overrode the bar wherever it exceeded it). Per theme: the bar's
        // bed over the ground, and what lifting the bed to 1.4:1 over the
        // ground would cost the text over it.
        println!();
        println!(
            "the bed over its ground (no law — a ruling): bar's bed / ground, and the cost of a 1.4:1 lift"
        );
        println!(
            "  theme                     fg      bg      budget  bed/ground  fg/bed worst | lifted to  fg/bed worst"
        );
        let one_dark = (0x00AB_B2BFu32, 0x0028_2C34u32);
        for (theme, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
            ("One Dark", one_dark.0, one_dark.1),
            ("One Dark (alt ground)", one_dark.0, 0x0032_3844),
            ("Solarized Dark", 0x0083_9496, 0x0000_2B36),
            ("Gruvbox Dark", 0x00EB_DBB2, 0x0028_2828),
            ("Dracula", 0x00F8_F8F2, 0x0028_2A36),
            ("Tokyo Night", 0x00C0_CAF5, 0x001A_1B26),
        ] {
            let budget = bed_luma_budget(fg);
            let over = (budget + 0.05) / (relative_luminance(bg) + 0.05);
            let lifted = ((relative_luminance(bg) + 0.05) * 1.4 - 0.05).max(budget);
            println!(
                "  {theme:24} #{fg:06X} #{bg:06X} {budget:.4}  {over:6.3}:1  {:8.3}:1 | {lifted:.4}     {:8.3}:1{}",
                worst_bed_contrast_at(fg, bg, budget),
                worst_bed_contrast_at(fg, bg, lifted),
                if lifted > budget + 1e-6 {
                    "  (lift would bind)"
                } else {
                    "  (bar already above 1.4:1)"
                }
            );
        }
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

    #[test]
    fn the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows() {
        // THE OWNER, 2026-09-08: "make this rainbow theme truly magical and
        // special and dynamic and beautiful" — the live-momentum wave was
        // v1's `0.055 ch · disp`, at most 1.5 px at retina; it is doubled so
        // the ribbon visibly breathes with speed, still inside the rows the
        // body already owns: the top edge never rises above `TALL_UP_CH` and
        // the bottom never falls past `DN_TOP_CH`, at any phase.
        let c = cfg(true, true);
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
                    band.bottom <= rest + DN_TOP_CH * ch + 0.5,
                    "the wave dropped the body's bottom past its own reach at col {col}, phase {k}/16 ({})",
                    band.bottom
                );
            }
        }
        assert!(
            deepest >= 0.10 * ch,
            "the wave's deepest swing at full momentum is {deepest:.2} px ({:.3} ch); the owner asked for a ribbon that visibly breathes with speed — `WAVE_AMP_CELLS` 0.110",
            deepest / ch
        );
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

    #[test]
    fn the_hot_edge_is_the_stop_under_it_lifted_hot_and_brighter_than_the_bed() {
        // THE OWNER, 2026-09-08: "I want the meteor to have rainbow! be a
        // bigger more special rainbow impact!" — "make this rainbow theme
        // truly magical and special and dynamic and beautiful". The hairline
        // at the hand was `#FFFFFF`, "a third white idiom". It is now the
        // spectrum: the stop under each vertex, lifted toward white to
        // `HOT_EDGE_LUMA_FLOOR` (`hot_edge_ink`) so it out-shines any bed it
        // can sit on, at `HOT_EDGE_COV_MAX` 118 — the transient cap (was 38).
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
        let stop = spectrum(clamp01(tri(t)));
        assert_eq!(
            dominant(head.color),
            dominant(stop),
            "the hairline's hue (#{:06X}) is not the head's stop (#{stop:06X})",
            head.color
        );
        // BRIGHTER THAN THE BED it rides: the head's stop at the bed's
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
        // …and priced for EMPHASIS: past the 38 the white hairline was held
        // to. The hairline's 1-px anti-aliasing puts two thirds of a request
        // on its brighter row at this fixture (25 of 38 before), so the pin
        // is on the emitted peak, past the old ceiling.
        let peak = peak_channel(&sink.out);
        assert!(
            peak > 39,
            "the hairline's brightest emitted level is {peak}; it was capped at 38 until 2026-09-08 and the owner asked for more"
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
        // A PTY-licensed jump (no credit): it abandons and lays no wake, so
        // A is a retracting row-mate and B a cohort of its own — a Nav jump
        // would take A over into its wake and B would join that (R5).
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
        // A PTY-licensed jump: the abandon alone (no credit, no wake — R5's
        // wake is pinned on its own below).
        rib.on_event(&pty((2, 42), (2, 80)), now, &cx);
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
        // the landing included, every one born WAKE_BORN_LAG_S later, with
        // the fixed life — never the four-letter chain law's.
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&nav((2, 110), (2, 2)), t0, &cx);
        let cells = row_cells(&rib, 2);
        let cols: Vec<u16> = cells.iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (2..2 + WAKE_MAX_CELLS).collect::<Vec<_>>(),
            "a leftward jump's wake runs from the landing rightward, capped"
        );
        let lag = t0 + Duration::from_secs_f32(WAKE_BORN_LAG_S);
        for &(col, typing, life, born) in &cells {
            assert!(!typing, "col {col}: a wake cell is never typing");
            assert!(
                (life - WAKE_LIFE_S).abs() < 1e-6,
                "col {col}: fixed life, got {life}"
            );
            assert_eq!(born, lag, "col {col}: born one lag past the landing");
        }
        assert_eq!(rib.cohorts().len(), 1, "one cohort, the wake's own");
        // A cold Ctrl-E the other way, once the first wake is long gone: the
        // wake runs from the landing LEFTWARD, and the origin cell at col 2
        // is beyond the cap, so it is not painted.
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
        // The landing is the run's HEAD (newest cell): the hot edge sits by
        // the caret, not at the far end.
        let newest = rib
            .cells()
            .iter()
            .max_by_key(|c| (c.born, c.col))
            .map(|c| c.col);
        assert_eq!(newest, Some(110));
    }

    #[test]
    fn a_wake_cell_is_dark_until_it_is_born_then_takes_the_one_attack() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&nav((2, 60), (2, 2)), t0, &cx);
        let lag_ms = (WAKE_BORN_LAG_S * 1000.0) as u64;
        let edge_ms = (EDGE_IN_S * 1000.0) as u64;
        for ms in [0u64, lag_ms / 2, lag_ms - 1] {
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
        let cx = ctx(at(t0, lag_ms + edge_ms + 2), &c, (2, 2), 0.0);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= (UNDER_COV_CAP * BODY_COLD_SHARE) as u8 - 2,
            "one attack past its birth the wake is at its cold ceiling ({})",
            plan_peak(&rib)
        );
        // …and it is out one life after that birth, through its own melt.
        let cx = ctx(
            at(t0, lag_ms + (WAKE_LIFE_S * 1000.0) as u64 + 20),
            &c,
            (2, 2),
            0.0,
        );
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the wake is gone one WAKE_LIFE_S after its birth"
        );
    }

    #[test]
    fn a_jump_hands_the_live_band_to_its_wake_with_the_stops_it_had_and_no_step() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Forty cells over 2.4 s, then a Ctrl-A: the band under the hand IS
        // the corridor. It keeps every stop it had (C2), loses no light on
        // the jump frame, has ONE owner per cell, and then outlives the jump
        // on the wake's clock instead of the abandon's 0.64 s retract. (A
        // jump AWAY from the band — Alt-F past its end — leaves the band
        // behind its origin, outside the corridor: that light still goes out
        // through the abandon, pinned above under the PTY licence.)
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let before = plan_sum(&rib);
        let segs = rib.plan_segments().len() as u32;
        let stops: Vec<Option<f32>> = (2..42u16).map(|col| rib.field_at(2, col)).collect();
        assert!(
            stops.iter().all(Option::is_some),
            "the band is laid under cols 2..42"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 42), (2, 2)), now, &cx);
        rib.plan(&cx);
        for (col, want) in (2..42u16).zip(&stops) {
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
        for col in 2..42u16 {
            let owners: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col)
                .collect();
            assert_eq!(owners.len(), 1, "col {col}: one owner, the wake's cell");
            assert_eq!(owners[0].cohort, wake, "col {col}: …in the wake cohort");
            assert!(
                !owners[0].typing,
                "col {col}: a taken-over cell is a wake cell"
            );
        }
        // The origin cell (42) is unlit and beyond the cap counted back from
        // the landing, so it stays dark: the cap bounds NEW light only, and
        // the band's 40 cells were all taken over — nothing else was laid.
        assert!(rib.field_at(2, 42).is_none());
        assert_eq!(rib.cells().len(), 40, "the band's cells, and no more");
        let after = plan_sum(&rib);
        assert!(
            after + 2 * segs >= before,
            "the hand-off stepped the band down ({before} → {after} over {segs} boundaries)"
        );
        // Where the abandon's retract would have drained it to nothing, the
        // wake still holds the band (the design's ~1.1 s after-effect)…
        let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
        rib.plan(&cx);
        let held = plan_sum(&rib);
        assert!(
            held * 2 > before,
            "0.66 s after the jump the wake must still hold the band ({before} → {held})"
        );
        // …and it is out one wake life (plus the new cells' lag) later.
        let cx = ctx(at(last, 16 + 1250), &c, (2, 80), 0.9);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the wake is out by +1.25 s");
    }

    #[test]
    fn a_wake_is_never_typing_and_holds_no_other_cohort() {
        // THE FLAG PIN. `typing: false` is what keeps "a jump builds no
        // momentum" (mod.rs) true and keeps a wake from lifting another
        // mark's finger: a later flip to `true` would break both silently.
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
        // A jump's wake: not typing either, and `field_at_caret`'s fallback
        // still reads REAL TYPING — the jump wrote no hand colour.
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
        }
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
        // A word at cols 10..17; Left from 18 to 17 crosses its own live
        // cell (left alone) and the caret's empty cell 18 (laid, in a WAKE
        // cohort of its own on the word's walk — never in the word's cohort,
        // whose clock, phase and bounds the arrow must not touch: an arrow
        // beside the word you just typed does not restart its grace).
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let word = rib.cohorts()[0];
        assert!(!word.wake);
        let now = at(t0, 600);
        let cx = ctx(now, &c, (2, 17), 0.8);
        let t17 = rib.field_at(2, 17);
        rib.on_event(&nav((2, 18), (2, 17)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.field_at(2, 17),
            t17,
            "the live cell under the hop is untouched"
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
        // SWOOSH_TOTAL_S after ITS last key (t0 + 420 ms), not after the hop.
        let cx = ctx(
            at(t0, 420 + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            (2, 17),
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

    #[test]
    fn a_jump_into_a_band_already_leaving_lays_nothing_under_it_and_lights_only_what_has_gone_out()
    {
        // The reviewer's reproduction (2026-09-09): forty cells, a plan at
        // `now` — the host plans every tick, so the cohort's phase is current
        // when the Move arrives — then a Ctrl-A at 1.0 / 1.25 / 1.4 s idle,
        // inside the swoosh window (retract from 0.90 s, over at 1.54 s), and
        // at 1.6 s as the cold endpoint. Before this clause the takeover
        // handed every half-drained cell to a fresh Laying cohort at its
        // instantaneous level and re-lit the emptied ones at full: a ramp
        // frozen between two bright runs for a whole wake life — R6's gaps.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let cw = geom().cw as f32;
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * 0.9;
        let vis = |rib: &Ribbon| -> String {
            let mut cells: Vec<(i32, u8)> = Vec::new();
            for s in rib.plan_segments() {
                let col = (s.x / cw).floor() as i32;
                match cells.iter_mut().find(|e| e.0 == col) {
                    Some(e) => e.1 = e.1.max(s.cov),
                    None => cells.push((col, s.cov)),
                }
            }
            cells.sort_unstable();
            cells
                .iter()
                .map(|(c0, v)| format!("{c0}:{v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut laid = Vec::new();
        for idle_ms in [1000u64, 1250, 1400, 1600] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, 40, &c, 0.9);
            let last = at(t0, 39 * 60);
            let now = at(last, idle_ms);
            let cx = ctx(now, &c, (2, 42), 0.9);
            rib.plan(&cx);
            let band = rib.cohorts().first().copied();
            let before: Vec<Cell> = rib.cells().to_vec();
            let lit: Vec<u16> = before
                .iter()
                .filter(|l| rib.env_of(&cx, l) > 0.0)
                .map(|l| l.col)
                .collect();
            let mut empty: Vec<u16> = (2..42u16).filter(|col| !lit.contains(col)).collect();
            empty.sort_unstable();
            println!("idle {idle_ms} before  {}", vis(&rib));
            let lit_xs = |rib: &Ribbon| -> Vec<(i32, u8)> {
                rib.plan_segments()
                    .iter()
                    .filter(|s| s.cov > 0)
                    .map(|s| ((s.x * 16.0).round() as i32, s.cov))
                    .collect()
            };
            let xs_before = lit_xs(&rib);
            let cx = ctx(now, &c, (2, 2), 0.9);
            rib.on_event(&nav((2, 42), (2, 2)), now, &cx);
            rib.plan(&cx);
            println!("idle {idle_ms} +0 ms   {}", vis(&rib));
            // NO LURCH: every boundary the leaving band had is exactly where
            // it was on the jump frame — its retract keeps pulling toward
            // the caret it began under, not toward the landing.
            let xs_after = lit_xs(&rib);
            assert_eq!(
                xs_before, xs_after,
                "idle {idle_ms}: the leaving band moved on the jump frame"
            );
            let band_id = band.map(|b| b.id);
            // Every cell the band still lights is still the band's: same
            // cohort, same birth, same price, ONE owner — nothing was taken
            // over and nothing was laid under it.
            for col in &lit {
                let owners: Vec<&Cell> = rib
                    .cells()
                    .iter()
                    .filter(|l| l.row == 2 && l.col == *col)
                    .collect();
                assert_eq!(owners.len(), 1, "idle {idle_ms}: col {col} keeps one owner");
                let b = before
                    .iter()
                    .find(|l| l.col == *col)
                    .expect("was in the pool");
                let o = owners[0];
                assert!(
                    Some(o.cohort) == band_id && o.born == b.born && o.cov0 == b.cov0,
                    "idle {idle_ms}: the lit band cell at col {col} was taken over"
                );
            }
            // …the leaving band's clock is not touched (it is already past
            // the abandon's rewind point)…
            if let Some(b) = band {
                let b_now = rib
                    .cohorts()
                    .iter()
                    .find(|k| k.id == b.id)
                    .expect("the band stays in the pool");
                assert_eq!(
                    b_now.alive_at, b.alive_at,
                    "idle {idle_ms}: the leaving band keeps its clock"
                );
                assert!(b_now.phase.is_retracting() || idle_ms < 900);
            }
            // …and the wake's NEW cells sit exactly where the drain had
            // already emptied a cell (within the cap counted back from the
            // landing: cols 2..34), one cohort, born a lag late, cold-priced.
            let wake: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|l| Some(l.cohort) != band_id)
                .collect();
            let mut wake_cols: Vec<u16> = wake.iter().map(|l| l.col).collect();
            wake_cols.sort_unstable();
            let want: Vec<u16> = empty
                .iter()
                .copied()
                .filter(|col| col - 2 < WAKE_MAX_CELLS)
                .collect();
            assert_eq!(
                wake_cols, want,
                "idle {idle_ms}: the wake lights the emptied cells and only them"
            );
            let wake_id = wake.first().map(|l| l.cohort);
            for w in &wake {
                assert!(Some(w.cohort) == wake_id, "idle {idle_ms}: one wake cohort");
                let lag_ok = w.born >= at(now, 99) && w.born <= at(now, 101);
                assert!(
                    !w.typing && (w.cov0 - cov0_new).abs() < 1e-6 && lag_ok,
                    "idle {idle_ms}: col {} is a new wake cell (cold-priced, born WAKE_BORN_LAG_S late)",
                    w.col
                );
            }
            laid.push(wake.len());
            // Through the wake's life the pool holds only the two kinds — the
            // band's cells draining on their own clock and the wake's — and
            // no band cell remains once its swoosh is over (1.54 s idle).
            for dt in [50u64, 150, 400, 800] {
                let cx = ctx(at(now, dt), &c, (2, 2), 0.9);
                rib.plan(&cx);
                println!("idle {idle_ms} +{dt:>3} ms {}", vis(&rib));
                for l in rib.cells() {
                    assert!(
                        Some(l.cohort) == band_id || Some(l.cohort) == wake_id,
                        "idle {idle_ms} +{dt}: a cell of a third cohort at col {}",
                        l.col
                    );
                }
                if idle_ms + dt > (SWOOSH_TOTAL_S * 1000.0) as u64 {
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
        // The wake's width grows with the idle, continuously from nothing
        // while the band is still whole to the full cap once it is gone.
        assert_eq!(
            laid[0], 0,
            "at 1.0 s idle every band cell is still lit: no wake"
        );
        assert!(
            laid.windows(2).all(|w| w[0] < w[1]),
            "monotone in idle: {laid:?}"
        );
        assert_eq!(
            laid[3],
            usize::from(WAKE_MAX_CELLS),
            "past the swoosh the cold Ctrl-A lays the cap"
        );
    }

    #[test]
    fn a_wake_continues_the_band_s_own_walk_across_the_kink_whichever_way_it_jumps() {
        // `walk_t` is `d/16` for sixteen cells and `1/36` a cell after, so a
        // wake anchored at the LANDING reproduces the band's `t_at` only when
        // the landing is the band's origin (a Ctrl-A) and repaints every stop
        // past the kink on a jump into the band's middle or a Ctrl-E across
        // it. The wake shares the band's origin instead (`wake_origin`).
        assert!(
            (walk_t(39.0) - walk_t(18.0) - walk_t(21.0)).abs() > 0.1,
            "the kink this pin is about"
        );
        let c = cfg(true, true);
        let t0 = Instant::now();
        let last = at(t0, 39 * 60);
        let now = at(last, 16);
        // (a) Into the band's middle, 42 → 20: cells 20..42 are taken over on
        // the stops they had; cells 2..20 stay the band's, on theirs.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let stops: Vec<f32> = (2..42u16)
            .map(|col| rib.field_at(2, col).expect("laid"))
            .collect();
        let cx = ctx(now, &c, (2, 20), 0.9);
        rib.on_event(&nav((2, 42), (2, 20)), now, &cx);
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
            (wake.anchor_col, wake.t0),
            (rib.cohorts()[0].anchor_col, rib.cohorts()[0].t0),
            "the wake shares the band's origin"
        );
        // (b) Past its end, 42 → 60: the new cells continue the band's walk
        // at the lay rate — no seam at 41/42, and none where a landing-
        // anchored walk would have re-entered the fast sixteen.
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
        rib.translate_scroll(1);
        assert!(rib.cells().iter().all(|l| l.row == 1));
        rib.translate_scroll(4);
        assert!(rib.at_rest(), "light on a line nobody typed is not kept");
    }

    /// **A SCROLL CARRIES THE RIBBON WITH ITS TEXT** (seam point 12 — the
    /// PTY's one-row scroll on every Enter at the foot of the screen): the
    /// hand types four cells and backspaces the last, so the caret stands ON
    /// a retracting cell whose stop is not the newest key's; then the screen
    /// scrolls one row. BETWEEN the scroll and the next plan the ribbon must
    /// already answer for the moved text — the caret one row up, the field
    /// at the caret the SAME cell's stop, nothing left on the old row (the
    /// engine reads these for a jump that shares the tick with the scroll) —
    /// and the NEXT FRAME, planned at the same instant with the host's
    /// translated caret, must be the pre-scroll frame moved up exactly one
    /// cell height: every body quad and every hot-edge quad the same colour
    /// and width, one `ch` higher, and every cell's clocks and prices
    /// untouched. A scroll past the text drops it.
    ///
    /// The frame clause is stated at the 2× fixture (`ch 36`) on purpose:
    /// `ribbon_beam` phases its 4×4 Bayer dither on ABSOLUTE device y, so a
    /// translated frame is byte-identical only when the cell height is a
    /// multiple of four — true of the 2× fixture and of the owner's retina
    /// cell (`ch 28`), false of the 1× fixture's `ch 18`, where the same
    /// move re-phases the dither by two rows and every quad's byte shifts
    /// one level (the rasterizer's dither, not the ribbon's light).
    ///
    /// FAILED before the fix on the first clause after the scroll: the
    /// caret stayed on row 2 and the index was dropped, so `field_at(1, 13)`
    /// was `None` until the next plan and `field_at_caret` fell through to
    /// the newest typing cell's stop (col 12's, not col 13's).
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

    // -- the cohort's clock (2026-09-08, the owner's screenshot) -------------

    /// **A LINE STILL BEING TYPED KEEPS EVERY CELL FROM ITS FIRST KEY TO THE
    /// CARET.** The owner's screenshot ("rainbow" lit, "theme" DARK, "truly"
    /// half lit, "magical" lit, "and" DARK, "specai" lit at the caret):
    /// "rainbow" at 9 cps on a hot spine (`birth_disp` 0.9, `cell_life`
    /// 5.88 s), a 700 ms thinking pause, "theme" slowly at 4 cps on the
    /// dipped spine (0.45, 2.11 s), then " truly magical" at 8 cps hot again.
    /// Planned every 16 ms until the last key, every glyph cell from the
    /// first key to the caret's neighbour is in the pool. FAILED before the
    /// cohort's clock (measured): at +4.192 s "theme"'s `h` — born at
    /// +2.08 s with 2.11 s of life (its `t`, the first key after the pause,
    /// took the chain floor `4.5 × 0.95 s + 0.1 = 4.38 s`) — ran out on its
    /// own clock while "rainbow" (5.88 s) on its left and " truly" on its
    /// right stood lit: a dark cell inside the live span, under a hand that
    /// was still typing (key 23 of 27). Also pinned here, UNCHANGED: after the last key
    /// nothing is dropped through the 0.75 s grace, and the mark is off the
    /// glass — pool empty, at rest — [`SWOOSH_TOTAL_S`] after it.
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
        // UNCHANGED: the swoosh is still the ending. Through the grace every
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
}
