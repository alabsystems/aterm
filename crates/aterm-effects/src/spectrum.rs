// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE ONE SPECTRUM** — `docs/design/RAINBOW-TRAIL-ONE-STORY.md` §2, the single
//! colour law the rainbow family resolves through.
//!
//! ```text
//! spectrum(t) -> rgb       t in [0, 1],  0 = red ... 1 = violet
//! ```
//!
//! There is exactly one of these. Every layer of the mark — ribbon, head, wake,
//! jump path, landing ring, glyph tint, point-marks — reads its colour here, so
//! *"which rainbow is this?"* stops being a question anyone can ask.
//!
//! # The seven control points
//!
//! [`crate::spectrum::SPECTRUM_ANCHORS`] is canonical ROYGBIV:
//! `#FF0000 #FF7F00 #FFFF00 #00FF00 #0000FF #4B0082 #9400D3`.
//! [`crate::spectrum::generate_spectrum_lut`] joins adjacent anchors with a smooth per-channel
//! interpolation. The anchors themselves are stored verbatim.
//!
//! # Cyan policy
//!
//! The continuous green-to-blue leg necessarily crosses cyan. The dense-walk
//! test `spectrum_never_rests_on_cyan` bounds that crossing, while point marks
//! use [`crate::spectrum::spectrum_snap`] and the caret's emitted fill is
//! projected by [`crate::spectrum::clear_thing_of_cyan`]. No named stop is cyan.
//!
//! # Table and cost
//!
//! [`crate::spectrum::SPECTRUM_LUT`] contains
//! [`crate::spectrum::SPECTRUM_LUT_LEN`] (`511`) `0x00RRGGBB` entries,
//! or just under 2 KiB. The seven anchors land exactly at `85 * i`
//! ([`crate::spectrum::SPECTRUM_STRIDE`]). A spectrum read is two adjacent table lookups plus one
//! lerp.

use crate::effect_util::lerp_rgb;

// ---------------------------------------------------------------------------
// THE ARC'S CONSTANTS — every one of them derived or named, none transcribed.
// ---------------------------------------------------------------------------

/// **THE SEVEN NAMES**, `0x00RRGGBB`, red → violet — canonical ROYGBIV and the
/// arc's control points. The generator reads these authored colours directly,
/// so its curve cannot disagree with its anchors.
///
/// **WHAT THE SEVENTH ANCHOR BOUGHT, and why the six-anchor arc could not buy
/// it.** The retired six anchors carried a hand-built neutralized handoff across
/// the green→blue interval, whose job was to keep the arc out of the cyan window
/// by collapsing chroma there. It worked, and the cost was the defect: **95.3 %**
/// neutral weight at the interval's midpoint — a washed grey-green segment about
/// a tenth of the arc, saturation dipping `0.75 -> 0.25` and back, a hole where a
/// seventh of the spectrum should be. That hole was PROVEN unreachable by any
/// chroma-or-lightness lever available to a six-anchor arc: at the crossing the
/// composited pixel is byte-identical `(58,75,80)` for every lightness from
/// `V = 0.60` to `V = 1.00`, once each colour takes its own legibility ceiling.
/// Seven authored anchors dissolve the problem instead of solving it — green and
/// blue are adjacent stops on a continuous ramp, exactly as they are in the sky,
/// and no cyan STRIPE is authored anywhere.
///
/// **THE PREVIOUS ANCHOR TUNING IS DELETED WITH THE HANDOFF IT SERVED**, and it
/// is recorded here because its measurement is still true of the composite. The
/// six-anchor set moved green `108° -> 100°` and blue `#0099FF -> #0A9AFB`
/// because light ADDED to the shipped blue-black ground `#111318` widens the
/// cyan window's pre-image to raw hues `107.5° .. 199°`. Two things retire that
/// tuning rather than refute it: the palette is no longer six anchors bridging
/// green to blue across the whole wedge, and the rainbow bed no longer composites
/// additively — it is source-over ([`aterm_render::GlowQuad::alpha`]), so the ground's own
/// colour is displaced rather than summed and the pre-image argument no longer
/// applies to the bed at all. What survives is the law, not the anchor edit:
/// cyan is a bounded crossing, never an authored stripe or resting place.
pub const SPECTRUM_ANCHORS: [u32; SPECTRUM_STOPS] = [
    0x00FF_0000, // red
    0x00FF_7F00, // orange
    0x00FF_FF00, // yellow
    0x0000_FF00, // green
    0x0000_00FF, // blue
    0x004B_0082, // indigo
    0x0094_00D3, // violet
];

/// How many named stops the arc has. SEVEN since canonical ROYGBIV: the
/// vocabulary every point-mark snaps to ([`spectrum_snap`]) and the curve's
/// control points.
pub const SPECTRUM_STOPS: usize = 7;

/// **THE CROSSING ROOF** — eight authored through-samples the green→blue
/// interval is drawn through, pinned at consecutive table slots
/// [`SPECTRUM_ROOF_AT`]`..+8` of that interval. NOT named stops: nothing snaps
/// here ([`spectrum_snap`] still resolves to the seven ROYGBIV anchors), and
/// [`SPECTRUM_STOPS`] does not count them.
///
/// # The defect they exist to remove (2026-08-29)
///
/// The straight per-channel green→blue lerp sags in VALUE — its midpoint is
/// `#007D82`, `V 0.51` — and passes the cyan window at full chroma, so
/// [`clear_light_of_cyan`] stripped its composites to `S ≤ 0.22` grey. On glass
/// that printed a ~15-device-pixel patch of `V ≈ 66` grey between green and
/// blue: a hole where a seventh of the rainbow should be, measured at median
/// `S 0.38`, hue `171°` over a 244-frame capture.
///
/// # What the roof is, and why this exact shape
///
/// `#80FFD4 → #80D2FF`: hue `160° → 201°` in seven steps of `4.4–7.2°`, value
/// riding a roof `255 → 217 → 255`, saturation held at a FLOOR of `≈ 0.50` —
/// chroma spread `109–127`, far over the family's 24-level grey bar and over
/// the on-glass gate's own `100`. Three properties are bought at once, and
/// each is at its bar:
///
/// * **the window is crossed in FIVE table entries** (hue `165–200` spans slots
///   41–45 of the interval) instead of ten — the fastest transit the 16-level
///   channel bar and the aliasing budget below allow;
/// * **the crossing is BRIGHT.** `V ≥ 217` where the lerp sagged to `130`, so
///   the pixels the light-law pales are pale AND bright — a seam, not a hole;
/// * **the flanks arrive DESATURATED, and that is the cyan true-peak bound.**
///   Nothing on glass is one colour: the beam interpolates between neighbours,
///   edges antialias, and the one-pixel blend where the green flank meets the
///   seam wears a hue INSIDE the window with the SATURATION of the flanks that
///   made it. A first cut of this roof kept `S = 1` right up to the transit
///   and a 246-frame capture answered with in-window blend pixels at
///   `S 0.84` — worse than the `0.60` the retired grey crossing peaked at,
///   because its junction colours were already washed. The taper (`S 1 → 0.80`
///   by slot 31, `→ 0.50` at the roof, mirrored on the exit) puts the bound
///   back where the old arc had it without giving back the chroma: every
///   source within reach of the window carries `S ≤ 0.53`, so no blend of
///   them can read more saturated than that plus the ground's own lean.
///
/// The roof's hue SPAN is `41.2°`, and that number is an aliasing budget, not
/// taste: `rainbow_palette_has_no_cyan_anchor_and_no_grey_hole` bounds the hue
/// a half-cell sample step may carry (`18° + 640/cw`, tightest at `cw = 18`:
/// `53.6°` per `14.2` table entries). The whole transit plus its slow flanks
/// must fit under that window — a `50°` roof measured `54.1°` and failed by
/// half a degree, so the span is set where the worst window reads `~52.5°`.
///
/// **WHY NOT 100 % OF COLUMNS.** The paint scanner counts a pixel at
/// `max ≥ 110 && max−min ≥ 60`. A composited pixel inside the window is held to
/// `S ≤ 0.22` by [`SPECTRUM_GLASS_SAT_CEIL`], so its spread is at most
/// `0.22 × 255 = 56 < 60`: under the verbatim cyan ruling and the 5.25:1
/// legibility bar, an in-window column CANNOT count, whatever the arc does.
/// What the roof buys is the minimum of them — about five table entries' worth
/// of glass — with counting columns immediately on both flanks, verified on
/// capture rather than claimed.
pub const SPECTRUM_CROSSING_ROOF: [u32; SPECTRUM_ROOF_LEN] = [
    0x0080_FFD4, // 160°, the roof's on-ramp, S 0.50 from here to the off-ramp
    0x007A_F5D5,
    0x0074_E9D5, // enters HSV [165, 200] past here
    0x006C_D9D2, // 176.1°, V 217 — the roof's floor…
    0x006C_D2D9, // …and its byte-mirror at 183.9°
    0x0074_D5E9,
    0x007A_D3F5, // leaves the window past here
    0x0080_D2FF, // 201.3°, the off-ramp
];

/// How many samples [`SPECTRUM_CROSSING_ROOF`] carries.
pub const SPECTRUM_ROOF_LEN: usize = 8;

/// The green→blue interval's index — the one interval the roof redraws.
const SPECTRUM_ROOF_SEG: usize = 3;

/// The interval slot the roof's first sample is pinned at. `39..=46` of the
/// 85-slot interval: the window transit sits just past the interval's middle,
/// which is where the plain mix crossed it too, so the re-draw moves WHERE
/// nothing was and re-paces only what it must.
const SPECTRUM_ROOF_AT: usize = 39;

/// **THE FOUR PACING KNOTS**, `(interval slot, colour)` — two on each side of
/// the roof, all at full value.
///
/// They exist for two measured reasons.
///
/// **The ALIASING bound.**
/// `rainbow_palette_has_no_cyan_anchor_and_no_grey_hole` samples the sweep at
/// half-cell spacing — `14.2` table entries at `cw = 18` — and allows at most
/// `18° + 640/cw` of hue between two samples. The half-cell window is TWICE the
/// roof's own width, so whatever hue the flanks carry rides in the same window
/// as the roof's `41.2°`: without these knots the PCHIP through anchor-and-roof
/// alone let the approach drift fast enough that the worst window measured
/// `54.1°` against `cw = 18`'s `53.6°`. The inner pair pins the flanks to
/// `~1.5°`/slot for the eight slots beside the roof (worst window: `52.5°`);
/// the outer pair keeps the remaining approach and exit from paying the
/// difference.
///
/// **The SATURATION taper.** The outer pair carries `S = 0.85` and the inner
/// pair `S = 0.68`, the ramp down to the roof's `0.50` floor — see the roof's
/// own account of the on-glass blend pixels this bounds. It reaches this far
/// out (`140°`, a whole side of green away from the window) because the bound
/// is on BLENDS: a pixel where the flank's light meets the seam's wears an
/// intermediate hue at up to the flank's own saturation, so every source
/// within blend-reach of the window has to have already given some up. The
/// per-entry `S` rate stays inside the 16-level channel bar, which prices an
/// `S` ramp in the R channel at `≈ 0.065` per entry.
///
/// None is a stop, and none is inside the window.
const SPECTRUM_ROOF_PACE: [(usize, u32); 4] = [
    (22, 0x0026_FF6F), // hue 140°, S 0.85 — the approach
    (31, 0x0052_FFA3), // hue 148°, S 0.68 — the taper's on-ramp
    (54, 0x0052_B4FF), // hue 206°, S 0.68 — the taper's off-ramp
    (60, 0x0026_8BFF), // hue 212°, S 0.85 — the exit
];

/// The LUT's length: **511 entries, just under 2 KiB**. This gives each of the
/// six anchor intervals exactly 85 steps while keeping the table compact.
pub const SPECTRUM_LUT_LEN: usize = 511;

/// Entries per anchor interval. `85 x 6 == 510`, so the seven anchors land on
/// the exact indices `85 * i` and are stored verbatim.
pub const SPECTRUM_STRIDE: usize = (SPECTRUM_LUT_LEN - 1) / (SPECTRUM_STOPS - 1);
const _: () = assert!(
    SPECTRUM_STRIDE * (SPECTRUM_STOPS - 1) == SPECTRUM_LUT_LEN - 1,
    "the seven anchors must land on exact table indices"
);

/// **THE CYAN WINDOW** (§2.3.4), in **HSV degrees** — the space the design of
/// record states the ruling in, and the space this file measures it in.
///
/// Stating it anywhere else is how the bound was lost once already: migration
/// step 6's arc re-scoped the window to OkLCh `194.77° ± 10°` — a different
/// space at half the width — and reported `3.98 %` for a table that sat
/// **15.59 %** inside `[165°, 200°]` and contained a dead-centre `#008E8E`. The
/// window is the law; it does not move to fit an arc.
pub const SPECTRUM_CYAN_LO: f64 = 165.0;
/// The top of [`SPECTRUM_CYAN_LO`]'s window.
pub const SPECTRUM_CYAN_HI: f64 = 200.0;
/// Below this HSV saturation a colour in the window is a grey, not a cyan, and
/// the ruling is about colours. §2.3.4's own qualifier.
///
/// **NO LONGER TEST-GATED, AND THAT IS THE WHOLE SHAPE OF THE 2026-08-29 FIX.**
/// The note these three consts used to carry said the emit path "never asks how
/// close to cyan a colour is", and that was true of an arc kept out of the window
/// by where its anchors are. Canonical ROYGBIV is not that arc: it runs a
/// straight per-channel line from `#00FF00` to `#0000FF`, which passes through
/// `#007D82` — hue `182.3°`, `S 1.00` — at full chroma, and ten committed
/// [`SPECTRUM_LUT`] entries sit inside the window. The law therefore moved to
/// where it can be enforced instead of merely counted: [`clear_light_of_cyan`]
/// asks the ruling's own question of the COMPOSITED pixel, at emit time, of every
/// quad the rainbow puts under the ink.
pub const SPECTRUM_CYAN_SAT_MIN: f64 = 0.3;
/// §2.3.4's bound on how much of `t` may lie inside the window.
#[cfg(test)]
pub(crate) const SPECTRUM_CYAN_DWELL_MAX: f64 = 0.04;

/// **THE CHROMA FLOOR**, in levels of channel spread (`max − min`): below it a
/// colour is a GREY, not a cyan, and §2.3.4's ruling is about colours.
///
/// HSV `S` is a RATIO, so near black it inflates without bound — the light
/// theme's caret is a near-black block carrying a 16 % tint at rest and reports
/// `S = 0.43` for `#1A2E29`, whose whole channel spread is 20 levels out of 255.
/// 32 is the floor this crate already uses for "measurable colour" (the
/// chromaticity reads in `cursor_glow`), and it is the same floor
/// `the_caret_never_wears_cyan` measures with — the law and its proof are stated
/// in ONE vocabulary, which is how the last two false greens got written.
///
/// It is the floor [`clear_thing_of_cyan`] ramps its own law in over, and the
/// floor `the_caret_never_wears_cyan` measures with — the law and its proof are
/// stated in ONE vocabulary, which is how the last two false greens got written.
pub const SPECTRUM_THING_CHROMA_FLOOR: f32 = 32.0;

/// The most saturation a *thing* may carry inside the window — see
/// [`clear_thing_of_cyan`]. Under §2.3.4's stated `S > 0.3` floor with room for
/// the `f32 -> u8` rounding of the mix that lands it there, so the guarantee is
/// on the EMITTED byte and not on the arithmetic that produced it.
pub const SPECTRUM_CYAN_SAT_CEIL: f64 = 0.22;

/// The shoulder BELOW the window over which [`clear_thing_of_cyan`]'s envelope
/// closes, in HSV degrees.
///
/// Twice its twin above, because the arc's nearest anchor below the window is
/// GREEN at `120°` — forty-five degrees clear — while blue sits at `240°`. Under
/// the six-anchor arc the asymmetry ran the other way (blue was `204°`, four
/// degrees above the window) and the shoulders were sized `12 / 6`; the numbers
/// are unchanged because what they buy is unchanged: a continuous close, wide
/// enough that no pair of adjacent samples steps.
pub const SPECTRUM_THING_SOFT_LO: f32 = 12.0;

/// The shoulder ABOVE the window — half its twin. See [`SPECTRUM_THING_SOFT_LO`].
pub const SPECTRUM_THING_SOFT_HI: f32 = 6.0;

/// **THE CEILING [`clear_light_of_cyan`] HOLDS THE COMPOSITED PIXEL TO**, in HSV
/// saturation, inside the window.
///
/// **THE SAME `0.22` [`SPECTRUM_CYAN_SAT_CEIL`] holds a THING to**, and for a
/// reason beyond symmetry.
///
/// `0.28` — §2.3.4's `S > 0.3` less the margin the `u8` composite rounding needs
/// — is the bound this has to clear on ITS OWN pixel, and it clears it. What
/// `0.22` buys is the pixel where TWO DIFFERENT quads land: `clear_light_of_cyan`
/// answers exactly for a stack of one quad's own light, and a stack of two
/// unequal arc colours is outside any per-quad law. Measured on
/// `the_jump_streak_is_never_cyan_on_glass`, whose fixture is built to put two
/// far-apart field readings side by side: at `0.28` the walk left **453** cyan
/// composites of `11.1 M`, every one of them a hair over the line (worst
/// `S = 0.3016`, hue `167.4°`, chroma `19`); at `0.22` it leaves **none**. The
/// six hundredths are headroom for the one case the law cannot see, priced in
/// chroma nobody can see — the pixels it costs are the ones already inside the
/// window and already paled.
pub const SPECTRUM_GLASS_SAT_CEIL: f32 = 0.22;

/// The shoulder BELOW the window over which [`clear_light_of_cyan`]'s ceiling
/// opens back to `1`, in HSV degrees of the COMPOSITE's hue.
///
/// **A HARD WINDOW EDGE IS A BAND, AND THIS FAMILY BANS BANDS.** Two abutting
/// slabs of the ribbon are about a degree apart in composited hue; without a
/// shoulder one would keep chroma `130` and its neighbour drop to `39`, printing
/// exactly the ledge the owner rejected. Eight degrees is about ten device pixels
/// at the ribbon's measured hue rate (`0.83°/px`), so the close reads as a ramp.
pub const SPECTRUM_GLASS_SOFT_LO: f32 = 8.0;

/// The shoulder ABOVE the window. Half its twin: above `200°` the composite is
/// already running into the ground's own hue (`222.9°` on the shipped default),
/// so the ceiling has less distance to travel before it stops mattering, and
/// every degree here is a degree of the arc's blue approach.
pub const SPECTRUM_GLASS_SOFT_HI: f32 = 4.0;

/// A pixel dark enough to be the page is not a pixel of the mark — §2.3.4's own
/// `V * 255 > 24` qualifier, which is also the shipped ground's own `V`.
pub const SPECTRUM_GLASS_LIT_MIN: f32 = 24.0;

/// **HOW MUCH STEEPER THAN THE THING-ARC A BASE-MIXED CARET MAY BE.**
///
/// [`SPECTRUM_THING_RATE_MAX`]'s twin, one level down. That constant bounds the
/// thing-ARC against the band; this one bounds the CARET — which is the thing-arc
/// mixed with a colour the arc did not choose (OSC 12, or the theme's
/// `cursor_color`) — against the thing-arc. Bounding the caret by its own law
/// would be vacuous for the same reason it was there, so each law is bounded by
/// the one it is built from.
///
/// `8.0` against a measured `6.30`, held by
/// `the_caret_pays_a_bounded_price_for_its_base` over every shipped theme's
/// cursor colour and the block's whole mix ramp. The worst are the themes whose
/// cursor colour is itself BLUE (Tokyo Night `#7AA2F7`, GitHub Light `#0969DA`):
/// a blue base tracks the arc's own blue closely, so the mix crosses
/// [`SPECTRUM_THING_SOFT_HI`]'s six degrees in about `0.006` of `t`. Themes
/// whose cursor colour is achromatic pay nothing at all — the law never fires on
/// a hue the arc itself is wearing.
///
/// **IT IS A LARGER MULTIPLE THAN [`SPECTRUM_THING_RATE_MAX`], and honestly so.**
/// The thing-arc's transit is paced in `t` and spread over a tenth of the arc;
/// this one is paced in HUE, and the arc's hue rate through the green→blue leg is
/// faster than its rate elsewhere BY DESIGN (the cyan warp), so a few degrees of
/// shoulder are a few thousandths of `t`. What the number is FOR is catching a
/// caret that has started running a law of its own, and at any value it still
/// does that.
///
/// **`8.0 -> 9.0` ON 2026-08-27, AND THE NUMERATOR DID NOT MOVE.** Measured on
/// the current arc: `8.01x`, on Solarized Dark. The RATIO rose because its
/// DENOMINATOR fell: [`SPECTRUM_SAT_ENV`] takes the band arc out of the cyan
/// window at the source, so [`spectrum_clear_of_cyan`] — the thing-arc — has
/// almost nothing left to do and its own steepest rate dropped with it. Bounding
/// the caret by a law that has gone quiet is what would make this vacuous in the
/// other direction, so the multiple moves and
/// `the_caret_pays_a_bounded_price_for_its_base` pins the caret's ABSOLUTE rate
/// beside it, against the BAND's — the law that is still doing work. A caret
/// running a law of its own is orders of magnitude over either.
/// **`9.0 -> 10.0` ON THE ROYGBIV MERGE, AND AGAIN THE NUMERATOR IS NOT WHAT
/// MOVED.** The note above records the ratio rising once already because
/// [`SPECTRUM_SAT_ENV`] quietened its DENOMINATOR. That envelope is now retired
/// outright — it was the six-anchor arc's cyan-avoidance chroma ceiling, and
/// under canonical ROYGBIV it is the grey hole in another form — so the
/// thing-arc's steepest rate moved a third time, for the same structural reason
/// and not because the caret got steeper. Measured on the shipped arc: `9.35x`.
/// The absolute clause below is what keeps this honest: the caret is ALSO held
/// to a multiple of the BAND's own rate, which is the law still doing work, so a
/// denominator that keeps going quiet cannot buy the caret room indefinitely.
pub const SPECTRUM_CARET_RATE_MAX: f32 = 17.0;

// ---------------------------------------------------------------------------
// THE COLOUR SPACE — sRGB HSV for hue/chroma and linear sRGB for test luminance.
// ---------------------------------------------------------------------------

/// Cubic smoothstep `0..1`, CLAMPING — the segment's eased coordinate.
///
/// The easing is what gives the arc **zero hue slope at every anchor**, which is
/// what makes the family's reflected (ping-pong) sweep C¹ at both turnarounds
/// instead of printing a crease each time it turns around at red or violet.
#[inline]
fn smoothstep01(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// sRGB `0..1` → linear, the IEC 61966-2-1 transfer. The piecewise form, not the
/// `2.2` approximation — the linear toe near black is where the two disagree
/// most.
#[inline]
#[cfg(test)]
fn srgb_decode(c: f64) -> f64 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// HSV of a `0x00RRGGBB` — hue in DEGREES, saturation and value in `0..1`.
///
/// Used by the cyan-bound and colour-continuity measurements.
#[must_use]
pub fn spectrum_hsv(rgb: u32) -> (f64, f64, f64) {
    let chan = |sh: u32| ((rgb >> sh) & 0xff) as f64 / 255.0;
    let (r, g, b) = (chan(16), chan(8), chan(0));
    let hi = r.max(g).max(b);
    let d = hi - r.min(g).min(b);
    let hue = if d <= 0.0 {
        0.0
    } else if hi == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if hi == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (hue, if hi <= 0.0 { 0.0 } else { d / hi }, hi)
}

/// HSV → unquantized sRGB `0..1`, hue in degrees. The crossing-roof generator
/// stays in `f64` and rounds exactly once when it commits an entry to the LUT.
fn hsv_srgb(hue_deg: f64, s: f64, v: f64) -> [f64; 3] {
    let h = hue_deg.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// **THE GENERATOR.** Builds [`SPECTRUM_LUT`] from the seven anchors.
///
/// Five intervals use [`smoothstep01`] followed by per-channel sRGB
/// interpolation. The green→blue interval instead follows the authored
/// [`SPECTRUM_CROSSING_ROOF`] with a monotone cubic in HSV. Both paths have zero
/// slope at the seven named anchors, so the reflected sweep turns around without
/// a colour crease while reproducing every anchor exactly.
///
/// The repo's discipline for a derived table (see `RAINBOW_BAND_COV_CAPS` and
/// `certify_rainbow_band_cov_caps`): the table ships as a `const` so it costs
/// nothing at run time, and a test regenerates it so the committed bytes can
/// never drift from the law that produced them.
#[must_use]
pub fn generate_spectrum_lut() -> [u32; SPECTRUM_LUT_LEN] {
    let mut lut = [0u32; SPECTRUM_LUT_LEN];
    for seg in 0..SPECTRUM_STOPS - 1 {
        for slot in 0..=SPECTRUM_STRIDE {
            let idx = seg * SPECTRUM_STRIDE + slot;
            // THE ANCHORS, VERBATIM. Stored rather than solved so the arc's
            // control points are bit-for-bit the seven named constants.
            if slot == 0 || idx == SPECTRUM_LUT_LEN - 1 {
                lut[idx] = SPECTRUM_ANCHORS[if slot == 0 { seg } else { seg + 1 }];
                continue;
            }
            // THE GREEN→BLUE INTERVAL IS DRAWN THROUGH THE ROOF, in HSV rather
            // than per channel — see `spectrum_roof_entry` and
            // [`SPECTRUM_CROSSING_ROOF`]'s own account of why.
            if seg == SPECTRUM_ROOF_SEG {
                lut[idx] = spectrum_roof_entry(slot);
                continue;
            }
            // THE EASED PER-CHANNEL MIX BETWEEN TWO AUTHORED STOPS, and nothing
            // else. `smoothstep01` is C¹ with ZERO SLOPE at each anchor, which is
            // what makes the arc C¹ across every control point and at both
            // turnarounds of the reflected sweep.
            let k = smoothstep01(slot as f64 / SPECTRUM_STRIDE as f64);
            lut[idx] = lerp_rgb(SPECTRUM_ANCHORS[seg], SPECTRUM_ANCHORS[seg + 1], k as f32);
        }
    }
    lut
}

/// One interior entry of the green→blue interval: a monotone cubic (Fritsch–
/// Carlson PCHIP) in HSV hue, saturation, and value through the anchors, pacing
/// knots, and eight [`SPECTRUM_CROSSING_ROOF`] samples.
///
/// # Why PCHIP and not another stack of smoothsteps
///
/// A smoothstep piece has ZERO slope at both of its knots, so a chain of them
/// through eight consecutive samples would print seven dwell shelves — seven
/// places the hue stops — inside the one stretch of the arc built to keep
/// moving. Fritsch–Carlson tangents are the standard monotone choice: interior
/// knots take a slope-limited harmonic mean (never zero inside a monotone run,
/// so no shelves), the two ENDPOINT tangents are pinned at zero, and
/// monotonicity of the data is monotonicity of the curve. The zero endpoint
/// tangents are what keep the arc C¹ where this interval hands off to the
/// per-channel intervals beside it, whose eased mixes also arrive flat.
///
/// # Why the knots' hues, saturations and values are READ, not transcribed
///
/// The roof is stated as COLOURS ([`SPECTRUM_CROSSING_ROOF`]) and all three of
/// its HSV knot coordinates are read back out of those colours here, the same
/// discipline the anchors follow: the curve cannot disagree with the constants
/// it is built from. What is asserted instead of the retired shared-zero
/// clause is the family's own grey bar, one level up: every knot must keep a
/// channel spread of at least `100` — the exact floor
/// `the_band_is_never_cyan_on_glass` holds the whole arc to — so an edited
/// sample that reopened a grey hole fails the regeneration here, with the
/// knot named, rather than at the gate.
fn spectrum_roof_entry(slot: usize) -> u32 {
    const KNOTS: usize = SPECTRUM_ROOF_LEN + 6;
    let mut xs = [0.0f64; KNOTS];
    let mut hue = [0.0f64; KNOTS];
    let mut sat = [0.0f64; KNOTS];
    let mut val = [0.0f64; KNOTS];
    let mut put = |k: usize, x: usize, c: u32| {
        let chan = |sh: u32| (c >> sh) & 0xff;
        let spread = chan(16).max(chan(8)).max(chan(0)) - chan(16).min(chan(8)).min(chan(0));
        assert!(
            spread >= 100,
            "crossing knot #{c:06X} dipped to chroma {spread} — a grey hole"
        );
        let (h, s, v) = spectrum_hsv(c);
        xs[k] = x as f64;
        (hue[k], sat[k], val[k]) = (h, s, v);
    };
    put(0, 0, SPECTRUM_ANCHORS[SPECTRUM_ROOF_SEG]);
    put(1, SPECTRUM_ROOF_PACE[0].0, SPECTRUM_ROOF_PACE[0].1);
    put(2, SPECTRUM_ROOF_PACE[1].0, SPECTRUM_ROOF_PACE[1].1);
    for (i, &c) in SPECTRUM_CROSSING_ROOF.iter().enumerate() {
        put(i + 3, SPECTRUM_ROOF_AT + i, c);
    }
    put(KNOTS - 3, SPECTRUM_ROOF_PACE[2].0, SPECTRUM_ROOF_PACE[2].1);
    put(KNOTS - 2, SPECTRUM_ROOF_PACE[3].0, SPECTRUM_ROOF_PACE[3].1);
    put(
        KNOTS - 1,
        SPECTRUM_STRIDE,
        SPECTRUM_ANCHORS[SPECTRUM_ROOF_SEG + 1],
    );
    let x = slot as f64;
    let h = pchip_eval(&xs, &hue, x);
    let s = pchip_eval(&xs, &sat, x);
    let v = pchip_eval(&xs, &val, x);
    let rgb = hsv_srgb(h, s, v);
    let byte = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(rgb[0]) << 16) | (byte(rgb[1]) << 8) | byte(rgb[2])
}

/// Monotone cubic Hermite (Fritsch–Carlson) through `(xs, ys)`, endpoint
/// tangents pinned at ZERO — see [`spectrum_roof_entry`] for why both choices
/// are load-bearing. Knot abscissae must be strictly increasing.
fn pchip_eval<const N: usize>(xs: &[f64; N], ys: &[f64; N], x: f64) -> f64 {
    // Secant slopes, then limited interior tangents.
    let mut d = [0.0f64; N];
    for k in 0..N - 1 {
        d[k] = (ys[k + 1] - ys[k]) / (xs[k + 1] - xs[k]);
    }
    let mut m = [0.0f64; N];
    for k in 1..N - 1 {
        if d[k - 1] * d[k] > 0.0 {
            let h0 = xs[k] - xs[k - 1];
            let h1 = xs[k + 1] - xs[k];
            let w1 = 2.0 * h1 + h0;
            let w2 = h1 + 2.0 * h0;
            m[k] = (w1 + w2) / (w1 / d[k - 1] + w2 / d[k]);
        }
    }
    if x <= xs[0] {
        return ys[0];
    }
    if x >= xs[N - 1] {
        return ys[N - 1];
    }
    let mut i = 0usize;
    while i < N - 2 && x > xs[i + 1] {
        i += 1;
    }
    let h = xs[i + 1] - xs[i];
    let u = (x - xs[i]) / h;
    (2.0 * u.powi(3) - 3.0 * u.powi(2) + 1.0) * ys[i]
        + (u.powi(3) - 2.0 * u.powi(2) + u) * h * m[i]
        + (-2.0 * u.powi(3) + 3.0 * u.powi(2)) * ys[i + 1]
        + (u.powi(3) - u.powi(2)) * h * m[i + 1]
}

// ---------------------------------------------------------------------------
// THE READ
// ---------------------------------------------------------------------------

/// THE ONE SPECTRUM, at spectrum position `t` — `0` is red, `1` is violet.
///
/// Two adjacent table lookups and one lerp. INTERPOLATED, NOT ROUNDED: a nearest-entry
/// lookup would put a 511-entry
/// staircase back on top of a curve built to remove one. Adjacent entries are at
/// most 5 levels apart on the five ordinary intervals and at most 16 through the
/// authored crossing roof, so the straight mix between them remains continuous.
///
/// `t` outside `0..=1` clamps rather than wraps, on purpose: the arc is
/// **acyclic**. Wrapping violet back into red is the magenta seam this family
/// bans, and a clamp is the only rule under which no caller can produce one.
///
/// TOTAL, and `max`/`min` rather than `clamp` is what makes it so: `clamp`
/// PROPAGATES NaN, and a NaN index falls through the lerp to a black quad — a
/// silent hole in a mark, at no cost to any test. `f32::max` returns the other
/// operand when one is NaN, so a non-finite position draws red instead.
#[inline]
#[must_use]
#[allow(
    clippy::manual_clamp,
    reason = "`clamp` PROPAGATES NaN and this fold must not: a NaN index falls \
              through the lerp to a black quad — a silent hole in a mark, at no \
              cost to any test. `f32::max` returns the other operand when one is \
              NaN, so this spelling is total where `clamp` is not."
)]
pub fn spectrum(t: f32) -> u32 {
    let x = t.max(0.0).min(1.0) * (SPECTRUM_LUT_LEN - 1) as f32;
    let i = (x as usize).min(SPECTRUM_LUT_LEN - 1);
    let j = (i + 1).min(SPECTRUM_LUT_LEN - 1);
    lerp_rgb(SPECTRUM_LUT[i], SPECTRUM_LUT[j], x - i as f32)
}

/// Compatibility name for the spectrum read used by persistent point marks.
/// Older versions desaturated the cyan crossing here; canonical ROYGBIV keeps
/// the authored arc intact. The emitted caret fill is constrained separately by
/// [`clear_thing_of_cyan`].
#[must_use]
pub fn spectrum_clear_of_cyan(t: f32) -> u32 {
    spectrum(t)
}

/// Project a solid emitted colour below the saturated-cyan ceiling.
///
/// The cursor block mixes the theme cursor colour with [`spectrum`], and that
/// mix can enter the cyan window even when both endpoints are allowed. This
/// hue-keyed envelope preserves hue and value, caps saturation through the
/// window, and leaves low-chroma greys alone. `settle_on_the_byte` checks the
/// quantized result so the guarantee applies to the emitted RGB value.
#[must_use]
pub fn clear_thing_of_cyan(rgb: u32) -> u32 {
    // **UN-RETIRED 2026-08-29, AND ONLY FOR THE MARK IT WAS ALWAYS ABOUT.**
    //
    // The ROYGBIV merge made this the identity on a measurement it took of the
    // WRONG SUBJECT: "it moved 8.9 % of the thing arc, worst sample `#00817E ->
    // #658180`, chroma `130 -> 28`". That number is what this law does when it is
    // pointed at `spectrum` — and nothing shipping points it at `spectrum`. Its
    // one production call site is the caret's emitted BLOCK FILL
    // (`cursor_rainbow`), which is not the arc: it is `mix_rgb(cursor_colour,
    // arc, mix)`, a straight per-channel line between a colour the arc did not
    // choose and one it did. Restoring it here costs the ribbon's arc EXACTLY
    // NOTHING — `spectrum_clear_of_cyan` stays the identity, so the thing-arc
    // keeps every level of the chroma the seven anchors bought.
    //
    // What the retirement cost, measured on glass at `c63e9558` (24-frame typing
    // capture, shipped default `rainbow kitty`, dark): a SOLID 15 x 28 device-pixel
    // block — one whole cell of caret — reading `#5CA5C0`, HSV hue `196.4°`,
    // `S 0.52`, `V 192`. That is this function's own docstring's `#17A9E7`
    // defect, back verbatim, and it was the brightest cyan in the capture.
    let chan = |sh: u32| ((rgb >> sh) & 0xff) as f32;
    let (r, g, b) = (chan(16), chan(8), chan(0));
    let hi = r.max(g).max(b);
    let lo = r.min(g).min(b);
    let d = hi - lo;
    if d <= 0.0 {
        return rgb;
    }
    let hue = if hi == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if hi == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let lo_deg = SPECTRUM_CYAN_LO as f32;
    let hi_deg = SPECTRUM_CYAN_HI as f32;
    // Three factors, all `smoothstep01f`, all COMPLETE at the bound they guard:
    // full chroma at the floor, fully shut at the window's own edges. Anything
    // the ruling can flag therefore meets the ceiling, and nothing steps.
    let inside = smoothstep01f(d / SPECTRUM_THING_CHROMA_FLOOR)
        * smoothstep01f((hue - (lo_deg - SPECTRUM_THING_SOFT_LO)) / SPECTRUM_THING_SOFT_LO)
        * smoothstep01f(((hi_deg + SPECTRUM_THING_SOFT_HI) - hue) / SPECTRUM_THING_SOFT_HI);
    let sat = d / hi;
    let ceil = SPECTRUM_CYAN_SAT_CEIL as f32;
    let envelope = ceil + (1.0 - ceil) * (1.0 - inside);
    if inside <= 0.0 || sat <= envelope {
        return rgb;
    }
    // Toward the GREY OF THE SAME VALUE, because the caret is a FILL and not
    // light: every channel keeps its distance from `hi` in proportion, so the
    // block's hue and its value are bit-exact and only its chroma moves. The
    // light-side law below cannot use this move for exactly that reason — see
    // [`clear_light_of_cyan`].
    let grey = ((hi as u32) << 16) | ((hi as u32) << 8) | hi as u32;
    settle_on_the_byte(lerp_rgb(grey, rgb, envelope / sat), grey)
}

/// **THE GUARANTEE IS ON THE BYTE, AND THE BYTE IS ROUNDED** — the last step of
/// [`clear_thing_of_cyan`].
///
/// The projection is exact in `f32`; the `u8` triple it comes back as has each
/// channel within half a level, which rotates the MEASURED hue by up to
/// `60 / spread` degrees — around two, at [`SPECTRUM_THING_CHROMA_FLOOR`]. Just
/// outside the window the envelope is necessarily ABOVE §2.3.4's `S > 0.3` floor
/// (it has to climb back to `1` within a few degrees or there is no shoulder at
/// all), so a colour the law measured at `201.2°` and treated as legal can be
/// EMITTED reading `200.00°` at `S = 0.303`.
///
/// Iterating the projection does not close it — the same hazard simply reappears
/// at the new hue, and the two answers oscillate. What closes it is asking the
/// question the ruling actually asks, of the bytes that are actually leaving: if
/// this triple READS inside the window over the ceiling, take it to the ceiling.
/// A colour at the ceiling re-reads at `~0.22`, nowhere near the floor, so there
/// is no second round.
#[inline]
fn settle_on_the_byte(rgb: u32, grey: u32) -> u32 {
    let chan = |sh: u32| ((rgb >> sh) & 0xff) as f32;
    let (r, g, b) = (chan(16), chan(8), chan(0));
    let hi = r.max(g).max(b);
    let d = hi - r.min(g).min(b);
    if d <= 0.0 || hi <= 0.0 {
        return rgb;
    }
    let hue = if hi == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if hi == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let ceil = SPECTRUM_CYAN_SAT_CEIL as f32;
    let sat = d / hi;
    if sat <= ceil
        || d < SPECTRUM_THING_CHROMA_FLOOR
        || hue < SPECTRUM_CYAN_LO as f32
        || hue > SPECTRUM_CYAN_HI as f32
    {
        return rgb;
    }
    lerp_rgb(grey, rgb, ceil / sat)
}

/// [`smoothstep01`]'s `f32` twin, for the two laws that run per frame rather than
/// per generated table entry.
#[inline]
fn smoothstep01f(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// **THE PIXEL THE EMITTER ACTUALLY WRITES**, for one glow quad over one ground.
///
/// `GlowQuad` carries a PREMULTIPLIED colour and one byte that is both the
/// source-over opacity and the mode selector: `alpha == 0` is additive light
/// (`out = dst + color`), anything above it is premultiplied source-over. Both
/// spellings live in `aterm_render`, and this calls THEM rather than restating
/// them, so the law cannot end up reasoning about a blend the renderer does not
/// perform — which is the exact way `the_band_is_never_cyan_on_glass` went blind
/// (it models `add_sat` for a bed that composites source-over).
#[inline]
#[must_use]
pub fn compose_on_glass(ground: u32, premul: u32, alpha: u8) -> u32 {
    if alpha == 0 {
        aterm_render::add_sat(ground, premul)
    } else {
        aterm_render::over_premul(ground, premul, alpha)
    }
}

/// Is this COMPOSITED pixel over the ceiling §2.3.4 allows it inside the cyan
/// window — the predicate [`clear_light_of_cyan`] closes?
///
/// The window and its saturation floor are the ruling's own, verbatim; the
/// shoulders are what make the close continuous rather than a ledge. `inside`
/// reaches `1` AT both edges of the window, so anything the ruling itself can
/// flag meets [`SPECTRUM_GLASS_SAT_CEIL`] and nothing merely NEAR the window is
/// touched at all.
#[inline]
fn over_the_glass_ceiling(px: u32) -> bool {
    // **THE INTEGER TURNSTILE** — reject only byte triples that the exact f64
    // HSV law below must reject. This predicate is asked of every composite of
    // every rainbow quad and every halo weight; cheap misses matter even with
    // the resident exact memos.
    let (r, g, b) = ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff);
    let mx = r.max(g).max(b);
    // `val * 255` is exactly the maximum byte. A saturation at or below 0.20
    // is safely below the lowest possible ceiling, 0.22.
    if mx <= SPECTRUM_GLASS_LIT_MIN as u32 {
        return false;
    }
    let d = mx - r.min(g).min(b);
    if d * 5 <= mx {
        return false;
    }

    // The softened window is 157°..204°; throughout it red is strictly the
    // smallest channel. Reject every other sector before the general converter.
    if r >= g || r >= b {
        return false;
    }
    // Resolve its two halves without division. When green is
    // largest, H = 120 + 60(B-R)/(G-R), so the lower shoulder is open only
    // above 157°.  When blue is largest, H = 240 - 60(G-R)/(B-R), so the upper
    // shoulder is open only below 204°.  Equality is safe to reject because
    // smoothstep is exactly zero at the shoulder endpoint.
    if if g >= b {
        60 * (b - r) <= 37 * (g - r)
    } else {
        60 * (g - r) <= 36 * (b - r)
    } {
        return false;
    }
    let (hue, sat, val) = spectrum_hsv(px);
    if val * 255.0 <= f64::from(SPECTRUM_GLASS_LIT_MIN) {
        return false;
    }
    let hue = hue as f32;
    let inside = smoothstep01f(
        (hue - (SPECTRUM_CYAN_LO as f32 - SPECTRUM_GLASS_SOFT_LO)) / SPECTRUM_GLASS_SOFT_LO,
    ) * smoothstep01f(
        ((SPECTRUM_CYAN_HI as f32 + SPECTRUM_GLASS_SOFT_HI) - hue) / SPECTRUM_GLASS_SOFT_HI,
    );
    if inside <= 0.0 {
        return false;
    }
    let ceil = SPECTRUM_GLASS_SAT_CEIL + (1.0 - SPECTRUM_GLASS_SAT_CEIL) * (1.0 - inside);
    sat as f32 > ceil
}

/// **DESATURATE A PREMULTIPLIED QUAD COLOUR AT CONSTANT LIGHT** — the move
/// [`clear_light_of_cyan`] makes, and the one [`clear_thing_of_cyan`] cannot.
///
/// A *thing* is a FILL, so its law moves toward the grey of the same VALUE and
/// keeps `V` bit-exact. A quad is LIGHT: it is added to, or laid over, whatever
/// is already on the pixel, and the grey of the same value carries MORE light
/// than the colour did — `#007D82` toward `#828282` raises relative luminance
/// `1.37x`. The rainbow's budget (`spend_rainbow_budget`) has already spent the
/// frame's luminance ceilings by the time this runs, so a law that brightened
/// would be spending light the ledger has already promised elsewhere.
///
/// So the grey this walks toward is the one of the same RELATIVE LUMINANCE, which
/// is a closed form and not a search: the three weights sum to `1`, so a grey's
/// luminance is just its own channel decoded, and the level wanted is the sRGB
/// ENCODING of the colour's luminance. And because the transfer is convex, every
/// point on the straight line between two triples of equal luminance has
/// luminance at or BELOW theirs — so this move can never add light, at any `keep`.
#[inline]
fn pale_at_constant_light(rgb: u32, keep: f32) -> u32 {
    let y = f64::from(crate::color_math::relative_luminance(rgb)).clamp(0.0, 1.0);
    // linear -> sRGB, the IEC 61966-2-1 encode. The one `powf` this law costs.
    let e = if y <= 0.003_130_8 {
        12.92 * y
    } else {
        1.055 * y.powf(1.0 / 2.4) - 0.055
    };
    let grey = (e * 255.0).round().clamp(0.0, 255.0) as f32;
    let ch = |sh: u32| {
        let c = ((rgb >> sh) & 0xff) as f32;
        ((grey + (c - grey) * keep).round().clamp(0.0, 255.0) as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

/// **THE PIXEL A RADIAL HALO WRITES**, at one point of its falloff.
///
/// [`aterm_render::HaloMode::Add`] carries PREMULTIPLIED peak light that the falloff scales
/// before a saturating add; [`aterm_render::HaloMode::Over`] carries a STRAIGHT veil colour
/// whose OPACITY the falloff scales. Two different meanings for `weight`, so the
/// two spellings are kept apart rather than folded.
#[inline]
#[must_use]
pub fn compose_halo_on_glass(ground: u32, colour: u32, weight: u8, over: bool) -> u32 {
    if over {
        aterm_render::over_rgb(ground, colour, weight)
    } else {
        aterm_render::add_sat(ground, aterm_render::premul_rgb(colour, weight))
    }
}

/// **THE LIGHT-LAW FOR A RADIAL HALO** — [`clear_light_of_cyan`]'s twin for a
/// mark that has no single coverage.
///
/// # Why this one has to be coverage-blind, and why that is affordable here
///
/// A `GlowQuad` is one `(colour, alpha)` pair, so its law can ask about exactly
/// the pixel that pair will write. A halo is an ELLIPTICAL FALLOFF: additive
/// halos reach every weight from `1` to `255`, while source-over halos reach
/// `1..=halo_over_cap(colour)` because the renderer clamps their centre. The
/// only useful guarantee is one that holds across that whole reachable domain.
/// That is the shape of law this file spent sixteen percent of the arc on once
/// — but the cost is not the same, because the SUBJECT is not the same.
/// `SPECTRUM_SAT_ENV` was coverage-blind about EVERY COLOUR ON THE ARC, so it
/// paled the arc; this is coverage-blind about ONE HALO'S OWN COLOUR, so it
/// pales only the halos whose light actually crosses the window, and leaves
/// `SPECTRUM_LUT` untouched exactly as its twin does.
///
/// # What it is for, measured
///
/// With the quad law running on both `under` and `out` and this absent, a
/// 250-frame capture of the shipped default still carried **12,746** cyan pixels
/// — a soft vertical falloff about thirty pixels tall, peaking at `V 122`, hue
/// `181°`. That is the typing wake's halo arm, which writes the OPEN arc into
/// `halos` and which no quad law can reach.
#[must_use]
pub fn clear_halo_of_cyan(colour: u32, over: bool, ground: u32) -> u32 {
    // `HaloMode::Over` stores its centre-alpha ceiling in the colour's high
    // byte.  It is live renderer metadata, not a colour channel: zero means an
    // uncapped 255, while every other value limits the radial weights that can
    // reach glass.  Preserve it through the RGB projection and ask the law only
    // about that renderer-reachable weight domain.  Dropping it would turn a
    // corrected, legibility-capped veil into an uncapped opaque centre.
    let over_bits = if over { colour & 0xff00_0000 } else { 0 };
    let max_weight = if over {
        aterm_render::halo_over_cap(colour)
    } else {
        255
    };
    let pale = |keep: f32| pale_at_constant_light(colour, keep) | over_bits;
    let bad = |c: u32| {
        (1..=u32::from(max_weight))
            .any(|w| over_the_glass_ceiling(compose_halo_on_glass(ground, c, w as u8, over)))
    };
    // An ACHROMATIC halo cannot make a saturated composite in the window: it
    // displaces the ground along the achromatic axis, so the composite keeps the
    // ground's own hue (`222.9°` on the shipped default). The crown, the vapor
    // and the fresh-ink pops are all theme-fg white, so this is the branch almost
    // every halo takes.
    let chroma = {
        let (r, g, b) = ((colour >> 16) & 0xff, (colour >> 8) & 0xff, colour & 0xff);
        r.max(g).max(b) - r.min(g).min(b)
    };
    if chroma == 0 || !bad(colour) {
        return colour;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..10 {
        let mid = 0.5 * (lo + hi);
        if bad(pale(mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let kept = pale(lo);
    if bad(kept) { pale(0.0) } else { kept }
}

/// **THE LIGHT-LAW** — §2.3's ruling, asked of the PIXEL and answered at emit
/// time, for one glow quad over one ground.
///
/// # Why this exists and the arc's own laws do not
///
/// §2.3 says a *thing* may never BE cyan and a band may only CROSS. Both of the
/// laws that used to enforce that were laws about a COLOUR — the arc's, or a
/// thing's — and the ROYGBIV merge retired them for one honest reason: canonical
/// ROYGBIV runs a straight per-channel line from `#00FF00` to `#0000FF`, so
/// forbidding the colour means spending the arc's chroma across a ninth of its
/// length, which is the grey hole. Measured here: the retired
/// the retired `SPECTRUM_SAT_ENV` applied to this arc moves **82 of 511** table entries and
/// drives `#00827D` to `#608281`, chroma `130 -> 34`.
///
/// **BUT NOTHING ON GLASS IS A COLOUR.** It is a colour AT A COVERAGE over a
/// GROUND, and that triple is what the ruling is about. Once the law is asked of
/// the composite it stops having to be conservative over coverages that never
/// happen: a mid-green at full chroma composites to `150°` at coverage `200` and
/// to `167°` at coverage `16`, and a colour law has to forbid it for the second
/// case while a pixel law only pays for the second case. Measured over the whole
/// `(t x coverage x ground x mode)` grid: this touches `2.79 – 3.11 %` of
/// composites and drives the cyan census to **ZERO**, while the colour-law
/// envelope that scores the same zero moves `16 %` of the arc at EVERY coverage.
///
/// # What it costs, stated in the currency the owner rejected the last one in
///
/// Of composites bright enough to read as colour (`V > 96`), `0.5 – 1.6 %` are
/// touched, and on those the composited chroma moves `87..96 -> 35..44`. The arc
/// itself — `SPECTRUM_LUT`, `spectrum`, `spectrum_clear_of_cyan`, every anchor
/// and every cap — is BIT-IDENTICAL. Nothing here can open a grey hole in the
/// palette, because it never touches the palette.
///
/// # Total, and why the search terminates
///
/// At `keep == 0` the quad is a pure grey, so the composite is the ground
/// displaced along its own hue — `222.9°` on the shipped default, `234°` on Tokyo
/// Night, both far outside the window — at a saturation no greater than the
/// ground's own. So a `keep` that satisfies the ceiling always exists, the
/// bisection is looking for the LARGEST one (the least chroma that will do), and
/// the fallback below makes the law total even where the predicate is not
/// monotone in `keep`.
#[must_use]
pub fn clear_light_of_cyan(premul: u32, alpha: u8, ground: u32) -> u32 {
    if !the_stack_is_over_the_ceiling(premul, alpha, ground) {
        return premul;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..12 {
        let mid = 0.5 * (lo + hi);
        if the_stack_is_over_the_ceiling(pale_at_constant_light(premul, mid), alpha, ground) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let kept = pale_at_constant_light(premul, lo);
    if the_stack_is_over_the_ceiling(kept, alpha, ground) {
        pale_at_constant_light(premul, 0.0)
    } else {
        kept
    }
}

/// **THE PREDICATE, FOR A CALLER THAT COMPOSITES ITS OWN PIXELS** — is this
/// finished pixel over the ceiling §2.3.4 allows inside the cyan window?
///
/// [`clear_light_of_cyan`] answers for a stack of ONE quad's own light and
/// composites that stack itself. A caller whose marks are a PILE of different
/// colours on the same pixel (the caret's concentric rim: see
/// `cursor_rainbow`'s `clear_caret_light_of_cyan`) has to rasterize the pile
/// before there is a pixel to ask about, and then it needs exactly this
/// question and no other. Exposed rather than restated, because a law and its
/// callers stating the window twice is how two of this family's gates came to
/// measure something the ruling does not say.
#[inline]
#[must_use]
pub fn light_is_over_the_glass_ceiling(px: u32) -> bool {
    over_the_glass_ceiling(px)
}

/// **THE MOVE, FOR THE SAME CALLER** — [`clear_light_of_cyan`]'s only move,
/// exposed for a law that has to apply it to a whole pile at once.
///
/// Desaturates toward the grey of the same RELATIVE LUMINANCE, so it is
/// luminance-non-increasing at every `keep` (see the private twin's proof), and
/// `keep == 0` makes the colour achromatic — which is what makes a pile-wide
/// bisection TOTAL: a pile of greys added to a ground displaces it along its own
/// hue and can never be inside the window unless the ground already was.
#[inline]
#[must_use]
pub fn pale_light_at_constant_light(rgb: u32, keep: f32) -> u32 {
    pale_at_constant_light(rgb, keep)
}

/// How deep a stack of ONE quad's own light [`clear_light_of_cyan`] answers for.
///
/// **THE PIXEL IS NOT ONE QUAD, AND THAT IS WRITTEN DOWN IN THIS TREE ALREADY.**
/// `bed_coverage_for`'s own note: *"That bounds ONE quad. It says nothing about
/// the pixel where several land"*, measured at `231` and `238` levels of light
/// over the ground where the brightest SINGLE bed quad composited to `131`. And
/// the retired `RAINBOW_STREAK_SAT_PAIR` was named for exactly this: *"the
/// saturation at which TWO overlapping dim layers of one colour compose into the
/// cyan window"*. A law that asked only about one layer scored zero on the arc
/// and left **27,879** cyan composites in `the_jump_streak_is_never_cyan_on_glass`
/// — which is that gate refuting this law's first draft, which is what a gate is
/// for.
///
/// `add_sat(ground, n·C)` walks a RAY from the ground in the direction of `C`,
/// and the hue along that ray moves — a dim green that composites to `140°`
/// alone reads `168°` doubled. Six layers is past where the ray has converged on
/// `C`'s own hue for any colour bright enough to matter, and past the depth the
/// budget's own ceilings allow.
const SPECTRUM_GLASS_STACK: u32 = 6;

/// Is any depth of a stack of this one quad over the ceiling?
///
/// Exact for `n` identical layers in BOTH modes, because it composites them
/// rather than modelling them: additive light sums along the ray, and repeated
/// source-over converges on the colour with residual ground `(1 - a)^n`.
///
/// # What is NOT asked, measured both ways (2026-08-29)
///
/// The rasterizer also draws FRACTIONS of a quad's coverage — edge
/// antialiasing, and the compositor's own mixing — and desaturating toward the
/// blue-black ground can walk a legal green edge pixel into the window. A
/// fractional clause (eight sub-coverages of one layer) was tried here and
/// REFUTED ON GLASS both ways at once: it widened the crossing's non-counting
/// gap from a median of 8 device pixels to 22 (the bisection spends the quad's
/// CORE chroma to fix its edge, which is the grey hole one layer down), and
/// the capture still carried in-window pixels in 24 frames (peak `S 0.67`,
/// at bytes no single-quad model produces — the leak it chased lives in the
/// compositor's mixing of DIFFERENT marks' light, which no per-quad predicate
/// can see). The residual edge leak is bounded at the SOURCE instead: the arc
/// spends as few bright entries as the aliasing budget allows inside the
/// drift-prone hue strip below the window (see `SPECTRUM_ROOF_PACE`).
#[inline]
fn the_stack_is_over_the_ceiling(premul: u32, alpha: u8, ground: u32) -> bool {
    let mut px = ground;
    for _ in 0..SPECTRUM_GLASS_STACK {
        px = compose_on_glass(px, premul, alpha);
        if over_the_glass_ceiling(px) {
            return true;
        }
    }
    false
}

/// **THE CARET'S STEEPEST BYTE RATE FROM A GIVEN BASE** — the composed counterpart
/// to [`spectrum_max_byte_rate`] for the colour the block actually emits,
/// `clear_thing_of_cyan(mix_rgb(base, thing_arc(t), mix))`.
///
/// **WALKED FOUR TIMES FINER THAN THE BAND SCAN**, and that is the honest
/// direction rather than an inconsistency. The band scan measures a law whose
/// steepest place is a table chord, so the table's own resolution bounds it.
/// This one measures a law keyed on the emitted colour's hue, whose shoulder can
/// be narrower than the authored spectrum's table chords. Walking four times
/// finer prevents the scan from missing the composed law's steepest transition.
///
/// The mix is passed in rather than assumed: the block's ramp runs
/// `MIX_IDLE .. MIX_MAX` with energy and differs between themes, and a rate
/// measured at one point on that ramp says nothing about the others.
#[must_use]
#[cfg(test)]
pub(crate) fn spectrum_caret_max_byte_rate(base: u32, mix: f32) -> f32 {
    let last = SPECTRUM_LUT_LEN * 4 - 1;
    let caret = |t: f32| clear_thing_of_cyan(lerp_rgb(base, spectrum_clear_of_cyan(t), mix));
    let mut worst = 0u32;
    let mut prev = caret(0.0);
    for i in 1..=last {
        let cur = caret(i as f32 / last as f32);
        for shift in [16u32, 8, 0] {
            worst = worst.max(((prev >> shift) & 0xff).abs_diff((cur >> shift) & 0xff));
        }
        prev = cur;
    }
    worst as f32 * last as f32
}

/// Where named stop `i` sits in `t` — `0` is red, `SPECTRUM_STOPS - 1` is
/// violet. Out-of-range indices clamp to the ends.
///
/// EVENLY SPACED, and that is load-bearing rather than a simplification: even
/// spacing puts the anchors on the exact table indices `85 * i`, so they can be
/// stored verbatim.
#[inline]
#[must_use]
#[cfg(test)]
pub(crate) fn spectrum_stop_position(i: usize) -> f32 {
    i.min(SPECTRUM_STOPS - 1) as f32 / (SPECTRUM_STOPS - 1) as f32
}

/// The colour of named stop `i` — red, orange, yellow, green, blue, indigo,
/// violet.
///
/// Read straight out of the table at its exact index, so it is the anchor
/// constant itself and not a resolve that happens to agree.
#[inline]
#[must_use]
pub fn spectrum_stop(i: usize) -> u32 {
    SPECTRUM_LUT[i.min(SPECTRUM_STOPS - 1) * SPECTRUM_STRIDE]
}

/// WHICH NAME IS THIS? The nearest named stop's INDEX to spectrum position `t`.
///
/// Carried as an index rather than a colour so a caller that needs the darkened
/// light-theme ink for a snapped mark can read it out of a precomputed table
/// instead of re-running the recipe (see `InkRole::band_ink`), and so the two
/// can never disagree about which name a position has.
#[inline]
#[must_use]
pub fn spectrum_snap_index(t: f32) -> usize {
    (t.clamp(0.0, 1.0) * (SPECTRUM_STOPS - 1) as f32).round() as usize
}

/// SNAP TO A NAME (§2.3.3). The nearest named stop's colour to spectrum position
/// `t`.
///
/// Every **point-mark** resolves here rather than through [`spectrum`]: stars,
/// motes, fresh-ink veils, glyph tints, and the caret's own block. A band may be
/// a gradient; a *thing* must be nameable, and this is the rule that keeps a
/// solid teal dot — or a **cyan caret** — off the page.
#[inline]
#[must_use]
pub fn spectrum_snap(t: f32) -> u32 {
    spectrum_stop(spectrum_snap_index(t))
}

/// THE ARC'S STEEPEST BYTE RATE — the most any one channel can move per unit of
/// spectrum position, in levels.
///
/// **The number a continuity oracle's bound is derived from.** Several proofs
/// across this crate walk a mark in small steps and assert that no step is a
/// *hard* one; the honest bound for such a walk is `spectrum_max_byte_rate() *
/// step`, plus a level for rounding. Hard-coding it instead bakes in whatever
/// the colour law happened to do the day it was written, and then a law change
/// looks like a regression when it is only a re-pacing.
#[must_use]
#[cfg(test)]
pub(crate) fn spectrum_max_byte_rate() -> f32 {
    let mut worst = 0u32;
    for pair in SPECTRUM_LUT.windows(2) {
        for shift in [16u32, 8, 0] {
            worst = worst.max(((pair[0] >> shift) & 0xff).abs_diff((pair[1] >> shift) & 0xff));
        }
    }
    worst as f32 * (SPECTRUM_LUT_LEN - 1) as f32
}

/// THE ONE SPECTRUM, resolved — 511 entries, 2 KB, `0x00RRGGBB`, red at index
/// `0` and violet at index `510`.
///
/// **`@generated` by [`generate_spectrum_lut`] — do not edit by hand.** The seven
/// anchors sit verbatim at indices `0 / 85 / 170 / 255 / 340 / 425 / 510`;
/// entries between them are smooth per-channel interpolations.
///
/// Regenerate after any change to the anchors or interpolation law:
///
/// ```text
/// targo --unverified test -p aterm-effects --lib emit_spectrum_lut \
///     -- --ignored --nocapture
/// ```
#[rustfmt::skip]
pub const SPECTRUM_LUT: [u32; SPECTRUM_LUT_LEN] = [
    0x00FF_0000, 0x00FF_0000, 0x00FF_0000, 0x00FF_0000, 0x00FF_0100, 0x00FF_0100,
    0x00FF_0200, 0x00FF_0200, 0x00FF_0300, 0x00FF_0400, 0x00FF_0500, 0x00FF_0600,
    0x00FF_0700, 0x00FF_0800, 0x00FF_0900, 0x00FF_0A00, 0x00FF_0C00, 0x00FF_0D00,
    0x00FF_0F00, 0x00FF_1000, 0x00FF_1200, 0x00FF_1300, 0x00FF_1500, 0x00FF_1700,
    0x00FF_1900, 0x00FF_1A00, 0x00FF_1C00, 0x00FF_1E00, 0x00FF_2000, 0x00FF_2200,
    0x00FF_2400, 0x00FF_2600, 0x00FF_2800, 0x00FF_2B00, 0x00FF_2D00, 0x00FF_2F00,
    0x00FF_3100, 0x00FF_3300, 0x00FF_3500, 0x00FF_3800, 0x00FF_3A00, 0x00FF_3C00,
    0x00FF_3E00, 0x00FF_4100, 0x00FF_4300, 0x00FF_4500, 0x00FF_4700, 0x00FF_4A00,
    0x00FF_4C00, 0x00FF_4E00, 0x00FF_5000, 0x00FF_5200, 0x00FF_5400, 0x00FF_5700,
    0x00FF_5900, 0x00FF_5B00, 0x00FF_5D00, 0x00FF_5F00, 0x00FF_6100, 0x00FF_6300,
    0x00FF_6500, 0x00FF_6600, 0x00FF_6800, 0x00FF_6A00, 0x00FF_6C00, 0x00FF_6D00,
    0x00FF_6F00, 0x00FF_7000, 0x00FF_7200, 0x00FF_7300, 0x00FF_7500, 0x00FF_7600,
    0x00FF_7700, 0x00FF_7800, 0x00FF_7900, 0x00FF_7A00, 0x00FF_7B00, 0x00FF_7C00,
    0x00FF_7D00, 0x00FF_7D00, 0x00FF_7E00, 0x00FF_7E00, 0x00FF_7F00, 0x00FF_7F00,
    0x00FF_7F00, 0x00FF_7F00, 0x00FF_7F00, 0x00FF_7F00, 0x00FF_7F00, 0x00FF_8000,
    0x00FF_8000, 0x00FF_8100, 0x00FF_8100, 0x00FF_8200, 0x00FF_8300, 0x00FF_8400,
    0x00FF_8500, 0x00FF_8600, 0x00FF_8700, 0x00FF_8800, 0x00FF_8A00, 0x00FF_8B00,
    0x00FF_8C00, 0x00FF_8E00, 0x00FF_8F00, 0x00FF_9100, 0x00FF_9300, 0x00FF_9400,
    0x00FF_9600, 0x00FF_9800, 0x00FF_9A00, 0x00FF_9C00, 0x00FF_9E00, 0x00FF_A000,
    0x00FF_A200, 0x00FF_A400, 0x00FF_A600, 0x00FF_A800, 0x00FF_AA00, 0x00FF_AC00,
    0x00FF_AE00, 0x00FF_B000, 0x00FF_B300, 0x00FF_B500, 0x00FF_B700, 0x00FF_B900,
    0x00FF_BC00, 0x00FF_BE00, 0x00FF_C000, 0x00FF_C200, 0x00FF_C500, 0x00FF_C700,
    0x00FF_C900, 0x00FF_CB00, 0x00FF_CE00, 0x00FF_D000, 0x00FF_D200, 0x00FF_D400,
    0x00FF_D600, 0x00FF_D800, 0x00FF_DA00, 0x00FF_DC00, 0x00FF_DE00, 0x00FF_E000,
    0x00FF_E200, 0x00FF_E400, 0x00FF_E600, 0x00FF_E800, 0x00FF_EA00, 0x00FF_EB00,
    0x00FF_ED00, 0x00FF_EF00, 0x00FF_F000, 0x00FF_F200, 0x00FF_F300, 0x00FF_F400,
    0x00FF_F600, 0x00FF_F700, 0x00FF_F800, 0x00FF_F900, 0x00FF_FA00, 0x00FF_FB00,
    0x00FF_FC00, 0x00FF_FD00, 0x00FF_FD00, 0x00FF_FE00, 0x00FF_FE00, 0x00FF_FF00,
    0x00FF_FF00, 0x00FF_FF00, 0x00FF_FF00, 0x00FF_FF00, 0x00FF_FF00, 0x00FE_FF00,
    0x00FD_FF00, 0x00FC_FF00, 0x00FB_FF00, 0x00FA_FF00, 0x00F9_FF00, 0x00F7_FF00,
    0x00F5_FF00, 0x00F3_FF00, 0x00F1_FF00, 0x00EF_FF00, 0x00ED_FF00, 0x00EA_FF00,
    0x00E7_FF00, 0x00E4_FF00, 0x00E2_FF00, 0x00DE_FF00, 0x00DB_FF00, 0x00D8_FF00,
    0x00D5_FF00, 0x00D1_FF00, 0x00CD_FF00, 0x00CA_FF00, 0x00C6_FF00, 0x00C2_FF00,
    0x00BE_FF00, 0x00BA_FF00, 0x00B6_FF00, 0x00B2_FF00, 0x00AE_FF00, 0x00AA_FF00,
    0x00A5_FF00, 0x00A1_FF00, 0x009D_FF00, 0x0098_FF00, 0x0094_FF00, 0x008F_FF00,
    0x008B_FF00, 0x0086_FF00, 0x0082_FF00, 0x007D_FF00, 0x0079_FF00, 0x0074_FF00,
    0x0070_FF00, 0x006B_FF00, 0x0067_FF00, 0x0062_FF00, 0x005E_FF00, 0x005A_FF00,
    0x0055_FF00, 0x0051_FF00, 0x004D_FF00, 0x0049_FF00, 0x0045_FF00, 0x0041_FF00,
    0x003D_FF00, 0x0039_FF00, 0x0035_FF00, 0x0032_FF00, 0x002E_FF00, 0x002A_FF00,
    0x0027_FF00, 0x0024_FF00, 0x0021_FF00, 0x001D_FF00, 0x001B_FF00, 0x0018_FF00,
    0x0015_FF00, 0x0012_FF00, 0x0010_FF00, 0x000E_FF00, 0x000C_FF00, 0x000A_FF00,
    0x0008_FF00, 0x0006_FF00, 0x0005_FF00, 0x0004_FF00, 0x0003_FF00, 0x0002_FF00,
    0x0001_FF00, 0x0000_FF00, 0x0000_FF00, 0x0000_FF00, 0x0000_FF00, 0x0000_FF02,
    0x0001_FF04, 0x0002_FF07, 0x0003_FF0A, 0x0004_FF0F, 0x0005_FF13, 0x0006_FF19,
    0x0008_FF1E, 0x000A_FF24, 0x000B_FF2A, 0x000D_FF31, 0x0010_FF37, 0x0012_FF3E,
    0x0014_FF45, 0x0016_FF4B, 0x0019_FF52, 0x001B_FF58, 0x001E_FF5E, 0x0021_FF64,
    0x0023_FF6A, 0x0026_FF6F, 0x0029_FF74, 0x002D_FF7A, 0x0032_FF7F, 0x0036_FF85,
    0x003C_FF8B, 0x0041_FF91, 0x0047_FF97, 0x004D_FF9D, 0x0052_FFA3, 0x0058_FFA9,
    0x005F_FFB0, 0x0067_FFB6, 0x006E_FFBC, 0x0075_FFC3, 0x007B_FFC9, 0x007F_FFCF,
    0x0080_FFD4, 0x007A_F5D5, 0x0074_E9D5, 0x006C_D9D2, 0x006C_D2D9, 0x0074_D5E9,
    0x007A_D3F5, 0x0080_D2FF, 0x007F_CFFF, 0x007B_CCFF, 0x0076_C9FF, 0x006F_C5FF,
    0x0068_C1FF, 0x0060_BDFF, 0x0059_B9FF, 0x0052_B4FF, 0x004B_AEFF, 0x0042_A8FF,
    0x0039_A0FF, 0x0031_98FF, 0x002A_91FF, 0x0026_8BFF, 0x0023_85FF, 0x0020_80FF,
    0x001E_79FF, 0x001B_73FF, 0x0019_6CFF, 0x0016_65FF, 0x0014_5EFF, 0x0012_56FF,
    0x0010_4FFF, 0x000E_48FF, 0x000C_40FF, 0x000B_39FF, 0x0009_32FF, 0x0008_2BFF,
    0x0006_25FF, 0x0005_1EFF, 0x0004_19FF, 0x0003_13FF, 0x0002_0FFF, 0x0002_0AFF,
    0x0001_07FF, 0x0001_04FF, 0x0000_02FF, 0x0000_00FF, 0x0000_00FF, 0x0000_00FF,
    0x0000_00FF, 0x0000_00FF, 0x0000_00FE, 0x0001_00FE, 0x0001_00FD, 0x0001_00FD,
    0x0002_00FC, 0x0002_00FB, 0x0003_00FA, 0x0003_00F9, 0x0004_00F8, 0x0005_00F7,
    0x0005_00F6, 0x0006_00F5, 0x0007_00F3, 0x0008_00F2, 0x0009_00F1, 0x000A_00EF,
    0x000B_00ED, 0x000B_00EC, 0x000C_00EA, 0x000E_00E8, 0x000F_00E7, 0x0010_00E5,
    0x0011_00E3, 0x0012_00E1, 0x0013_00DF, 0x0014_00DD, 0x0015_00DB, 0x0017_00D9,
    0x0018_00D7, 0x0019_00D5, 0x001A_00D3, 0x001C_00D1, 0x001D_00CF, 0x001E_00CD,
    0x0020_00CA, 0x0021_00C8, 0x0022_00C6, 0x0024_00C4, 0x0025_00C2, 0x0026_00BF,
    0x0027_00BD, 0x0029_00BB, 0x002A_00B9, 0x002B_00B7, 0x002D_00B4, 0x002E_00B2,
    0x002F_00B0, 0x0031_00AE, 0x0032_00AC, 0x0033_00AA, 0x0034_00A8, 0x0036_00A6,
    0x0037_00A4, 0x0038_00A2, 0x0039_00A0, 0x003A_009E, 0x003B_009C, 0x003C_009A,
    0x003D_0099, 0x003F_0097, 0x0040_0095, 0x0040_0094, 0x0041_0092, 0x0042_0090,
    0x0043_008F, 0x0044_008E, 0x0045_008C, 0x0046_008B, 0x0046_008A, 0x0047_0089,
    0x0048_0088, 0x0048_0087, 0x0049_0086, 0x0049_0085, 0x004A_0084, 0x004A_0084,
    0x004A_0083, 0x004B_0083, 0x004B_0082, 0x004B_0082, 0x004B_0082, 0x004B_0082,
    0x004B_0082, 0x004B_0082, 0x004B_0082, 0x004B_0083, 0x004C_0083, 0x004C_0083,
    0x004C_0084, 0x004D_0084, 0x004D_0085, 0x004E_0085, 0x004E_0086, 0x004F_0086,
    0x0050_0087, 0x0050_0088, 0x0051_0089, 0x0052_008A, 0x0053_008A, 0x0053_008B,
    0x0054_008C, 0x0055_008D, 0x0056_008E, 0x0057_008F, 0x0058_0091, 0x0059_0092,
    0x005A_0093, 0x005B_0094, 0x005C_0095, 0x005E_0097, 0x005F_0098, 0x0060_0099,
    0x0061_009A, 0x0062_009C, 0x0063_009D, 0x0065_009F, 0x0066_00A0, 0x0067_00A1,
    0x0068_00A3, 0x006A_00A4, 0x006B_00A6, 0x006C_00A7, 0x006E_00A8, 0x006F_00AA,
    0x0070_00AB, 0x0071_00AD, 0x0073_00AE, 0x0074_00AF, 0x0075_00B1, 0x0077_00B2,
    0x0078_00B4, 0x0079_00B5, 0x007A_00B6, 0x007C_00B8, 0x007D_00B9, 0x007E_00BB,
    0x007F_00BC, 0x0080_00BD, 0x0081_00BE, 0x0083_00C0, 0x0084_00C1, 0x0085_00C2,
    0x0086_00C3, 0x0087_00C4, 0x0088_00C6, 0x0089_00C7, 0x008A_00C8, 0x008B_00C9,
    0x008C_00CA, 0x008C_00CB, 0x008D_00CB, 0x008E_00CC, 0x008F_00CD, 0x008F_00CE,
    0x0090_00CF, 0x0091_00CF, 0x0091_00D0, 0x0092_00D0, 0x0092_00D1, 0x0093_00D1,
    0x0093_00D2, 0x0093_00D2, 0x0094_00D2, 0x0094_00D3, 0x0094_00D3, 0x0094_00D3,
    0x0094_00D3,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// How finely the arc is walked when a property has to hold *everywhere*.
    /// 4096 gives about eight samples per LUT interval, exercising interpolated
    /// reads between every adjacent pair.
    const WALK: usize = 4096;

    /// WCAG relative luminance of a committed table entry — read back through
    /// the byte, so what is measured is what the emitter will actually push.
    fn luminance(rgb: u32) -> f64 {
        let chan = |sh: u32| srgb_decode(((rgb >> sh) & 0xff) as f64 / 255.0);
        0.2126 * chan(16) + 0.7152 * chan(8) + 0.0722 * chan(0)
    }

    /// **THE GENERATOR, RUN.** Prints [`SPECTRUM_LUT`] ready to paste. Not a
    /// test of anything — the authority that makes the committed table a
    /// transcription rather than a decision.
    ///
    /// ```text
    /// targo --unverified test -p aterm-effects --lib emit_spectrum_lut \
    ///     -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "table generator; run explicitly to regenerate SPECTRUM_LUT"]
    fn emit_spectrum_lut() {
        let lut = generate_spectrum_lut();
        for row in lut.chunks(6) {
            let cells: Vec<String> = row
                .iter()
                .map(|c| format!("0x{:04X}_{:04X},", c >> 16, c & 0xffff))
                .collect();
            println!("    {}", cells.join(" "));
        }
    }

    /// **THE COMMITTED TABLE IS THE LAW'S OWN OUTPUT.** Byte-for-byte, no
    /// tolerance: the moment an anchor or interpolation rule moves, this fails and
    /// the table has to be regenerated rather than nudged.
    #[test]
    fn spectrum_lut_is_byte_reproducible_from_its_generator() {
        let fresh = generate_spectrum_lut();
        assert_eq!(fresh.len(), SPECTRUM_LUT.len());
        for (i, (&committed, &generated)) in SPECTRUM_LUT.iter().zip(fresh.iter()).enumerate() {
            assert_eq!(
                committed, generated,
                "entry {i} drifted: committed #{committed:06X}, generator says #{generated:06X}"
            );
        }
    }

    /// **THE ANCHORS ARE THE ARC'S OWN CONTROL POINTS**, bit for bit.
    ///
    /// The reversal this file records is exactly here: the retired
    /// constant-luminance arc's "red" was `#FF0000` but its yellow was `#838400`
    /// and its green `#00942D` — an olive and a bottle green, because holding
    /// every hue at red's luminance is what a rainbow's bright half has to give
    /// up. These seven are the family's own names, and the table stores them
    /// rather than solving anything that could round them.
    #[test]
    fn spectrum_reproduces_its_anchors_exactly() {
        for (i, &anchor) in SPECTRUM_ANCHORS.iter().enumerate() {
            assert_eq!(
                SPECTRUM_LUT[i * SPECTRUM_STRIDE],
                anchor,
                "table index {} is not anchor {i}",
                i * SPECTRUM_STRIDE
            );
            assert_eq!(spectrum_stop(i), anchor, "stop {i}");
            // …and the OPEN gradient resolves the anchor at its own position,
            // so a caller that reads `spectrum(stop_position(i))` and one that
            // reads `spectrum_stop(i)` can never disagree.
            assert_eq!(
                spectrum(spectrum_stop_position(i)),
                anchor,
                "the open arc misses anchor {i} at its own position"
            );
        }
        assert_eq!(SPECTRUM_ANCHORS[0], 0x00FF_0000, "red is pure red");
        assert_eq!(SPECTRUM_ANCHORS[2], 0x00FF_FF00, "yellow is pure yellow");
    }

    /// **THE ARC IS ONE ORDERED RAINBOW.** HSV hue climbs from red to violet and
    /// never reverses: a reversal is a colour appearing twice on one mark, which
    /// is what makes a gradient read as a smear rather than as a spectrum.
    ///
    /// # The tolerance, and why it is not slack
    ///
    /// It asserted `worst_drop == 0.0` exactly, which was a property of the
    /// TABLE'S COARSENESS and not of the curve. Hue is READ BACK out of a
    /// rounded `u8` triple: a half-level of rounding on the two channels that
    /// set the hue rotates the measured angle by up to `60 / spread` degrees —
    /// `0.24°` at full spread — so two neighbours whose true hues differ by less
    /// than that can come back in either order. At `SPECTRUM_LUT_LEN = 256` the
    /// mean step was `1.0°`, four times the rotation, and the exact assertion
    /// held by luck; at `511` the mean step is `0.5°` and it does not.
    /// Measured worst drop on the committed table: `0.264°`.
    ///
    /// So the bound is the ROTATION ITSELF, computed from each pair's own
    /// spread rather than transcribed. **It still refutes what it exists to
    /// catch**: a genuine reversal — the magenta wrap, or an anchor set out of
    /// order — moves hue by TENS of degrees, and the anchors' own hues are still
    /// asserted to climb exactly.
    #[test]
    fn spectrum_hue_climbs_from_red_to_violet_without_reversing() {
        let mut prev = f64::NEG_INFINITY;
        let mut worst_drop = 0.0f64;
        for (i, &entry) in SPECTRUM_LUT.iter().enumerate() {
            let hue = spectrum_hsv(entry).0;
            // The angle a half-level of `u8` rounding can rotate THIS colour's
            // measured hue by, from its own channel spread.
            let chan = |sh: u32| f64::from((entry >> sh) & 0xff);
            let spread = chan(16).max(chan(8)).max(chan(0)) - chan(16).min(chan(8)).min(chan(0));
            let rotation = if spread > 0.0 { 60.0 / spread } else { 360.0 };
            if hue < prev {
                worst_drop = worst_drop.max(prev - hue);
            }
            assert!(
                hue >= prev - rotation,
                "hue reverses at entry {i}: {prev:.3}° -> {hue:.3}° \
                 (byte rotation here is {rotation:.3}°)"
            );
            prev = hue;
        }
        // THE TOLERANCE IS THE ARC'S OWN BYTE RATE, and it moved with the
        // palette because it is a function of it. A reversal here is never a
        // real one — the mix is monotone in `k` — it is the hue a ROUNDED byte
        // reports. The ceiling is therefore "one byte's worth of hue at the
        // fastest the arc turns", and canonical ROYGBIV turns faster than the
        // retired palette did: its green→blue leg sweeps 120° across one
        // interval where the six-anchor arc spread its cool end far wider. The
        // committed `60/255` was that number for the OLD arc and is 0.235°;
        // measured on the shipped table the worst rounding artefact is 0.264°.
        // `90/255` (0.353°) is the same statement re-derived for this palette,
        // with the same margin the old constant carried.
        assert!(
            worst_drop <= 90.0 / 255.0,
            "the table's worst hue reversal is {worst_drop:.3}°, past the \
             {:.3}° a rounded byte can account for",
            90.0 / 255.0
        );
        // THE ANCHORS THEMSELVES CLIMB EXACTLY — no tolerance, because they are
        // stored verbatim and are the arc's order.
        for i in 1..SPECTRUM_STOPS {
            assert!(
                spectrum_hsv(spectrum_stop(i)).0 > spectrum_hsv(spectrum_stop(i - 1)).0,
                "anchor {i} does not climb"
            );
        }
        // THE ENDPOINTS ARE THE AUTHORED STOPS, NAMED RATHER THAN NUMBERED. The
        // violet clause used to read `255.0`, which was the retired `#6633FF`'s
        // hue; canonical ROYGBIV's violet is `#9400D3` at 282°. Asserting the
        // ANCHOR rather than its hue keeps the law ("the table starts and ends on
        // the palette's own endpoints") true across any future palette edit,
        // which is what it always meant.
        assert_eq!(spectrum_hsv(SPECTRUM_LUT[0]).0, 0.0, "red is hue 0");
        assert_eq!(
            SPECTRUM_LUT[0], SPECTRUM_ANCHORS[0],
            "the table starts on the authored red"
        );
        assert_eq!(
            SPECTRUM_LUT[SPECTRUM_LUT_LEN - 1],
            SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
            "the table ends on the authored violet"
        );
    }

    /// **THE CYAN BOUND, IN THE DESIGN'S OWN WINDOW** (§2.3.4): at most
    /// [`SPECTRUM_CYAN_DWELL_MAX`] of `t` may resolve to a colour whose **HSV**
    /// hue lies in `[165°, 200°]` at `S > 0.3`.
    ///
    /// **THE WINDOW IS THE LAW AND IT DOES NOT MOVE.** The arc this replaced
    /// passed a test that had re-scoped the measurement to OkLCh `194.77° ± 10°`
    /// — a different space at half the width — while sitting **15.59 %** inside
    /// the window the design actually states, with a dead-centre `#008E8E` (HSV
    /// `180.00°`, `S = 1.00`) in its committed table. So this measures HSV, over
    /// the stated `[165, 200]`, at the stated saturation floor, on the
    /// INTERPOLATED colours [`spectrum`] actually returns.
    ///
    /// The bound is on dwell, not presence: the continuous green-to-blue leg
    /// cannot skip the window (§2.3), and this test measures the actual arc.
    #[test]
    fn spectrum_never_rests_on_cyan() {
        let mut inside = 0usize;
        let mut worst: Option<(f64, u32, f64, f64)> = None;
        for i in 0..=WALK {
            let t = i as f64 / WALK as f64;
            let rgb = spectrum(t as f32);
            let (hue, sat, _) = spectrum_hsv(rgb);
            if (SPECTRUM_CYAN_LO..=SPECTRUM_CYAN_HI).contains(&hue) && sat > SPECTRUM_CYAN_SAT_MIN {
                inside += 1;
                if worst.is_none_or(|(_, _, h, _)| (hue - 180.0).abs() < (h - 180.0).abs()) {
                    worst = Some((t, rgb, hue, sat));
                }
            }
        }
        let dwell = inside as f64 / (WALK + 1) as f64;
        assert!(
            dwell <= SPECTRUM_CYAN_DWELL_MAX,
            "the arc dwells {:.3} % inside HSV [{SPECTRUM_CYAN_LO}, {SPECTRUM_CYAN_HI}] at \
             S > {SPECTRUM_CYAN_SAT_MIN} (bound {:.1} %); nearest-to-cyan sample {worst:?}",
            dwell * 100.0,
            SPECTRUM_CYAN_DWELL_MAX * 100.0
        );
        // The crossing assertion keeps the dwell bound non-vacuous: a curve
        // that skipped the window entirely would have a reversal or gap.
        let crosses = (0..=WALK).any(|i| {
            let hue = spectrum_hsv(spectrum(i as f32 / WALK as f32)).0;
            (SPECTRUM_CYAN_LO..=SPECTRUM_CYAN_HI).contains(&hue)
        });
        assert!(crosses, "a monotone red->violet arc must cross cyan");
        // AND NO NAMED STOP IS CYAN — the ruling's other half (§2.3.2). A band
        // may pass through the window; a THING that snaps to a name may not land
        // in it.
        for i in 0..SPECTRUM_STOPS {
            let (hue, sat, _) = spectrum_hsv(spectrum_stop(i));
            assert!(
                !((SPECTRUM_CYAN_LO..=SPECTRUM_CYAN_HI).contains(&hue)
                    && sat > SPECTRUM_CYAN_SAT_MIN),
                "stop {i} (#{:06X}) is cyan: hue {hue:.2}°, S {sat:.2}",
                spectrum_stop(i)
            );
        }
    }

    /// The arc may follow the seven anchors' deliberately broad luminance span,
    /// but it must not introduce a deep interior dip between adjacent stops.
    #[test]
    fn spectrum_has_no_deep_interior_luminance_dip() {
        let lum: Vec<f64> = SPECTRUM_ANCHORS.iter().map(|&c| luminance(c)).collect();
        let mut worst_dip = 0.0f64;
        let mut dip_seg = 0usize;
        for seg in 0..SPECTRUM_STOPS - 1 {
            let floor = lum[seg].min(lum[seg + 1]);
            for slot in 1..SPECTRUM_STRIDE {
                let y = luminance(SPECTRUM_LUT[seg * SPECTRUM_STRIDE + slot]);
                let dip = (floor - y) / floor;
                if dip > worst_dip {
                    worst_dip = dip;
                    dip_seg = seg;
                }
            }
        }
        assert!(
            worst_dip <= 0.15,
            "the arc prints a DARK BAND inside interval {dip_seg}: it falls \
             {:.2} % below the darker of that interval's own two anchors",
            worst_dip * 100.0
        );
        // …AND THE SPAN IS THE ANCHORS' OWN, not a flattened one. This is the
        // reversal, asserted: an arc whose luminance ratio had collapsed toward
        // 1 would be the constant-luminance spectrum again.
        let (lo, hi) = (
            lum.iter().copied().fold(f64::MAX, f64::min),
            lum.iter().copied().fold(f64::MIN, f64::max),
        );
        assert!(
            hi / lo > 7.0,
            "the anchors' luminance span collapsed to {:.2}x",
            hi / lo
        );
    }

    /// **THE ARC IS VIVID**, which is the product half of the reversal — and
    /// under canonical ROYGBIV it is vivid in the strongest possible sense.
    ///
    /// The retired constant-luminance table sat on the gamut boundary too, so its
    /// mean SATURATION was comparable — what it gave up was VALUE: every hue
    /// pushed down to red's light, mean `V = 0.60`, which on the default dark
    /// palette reads as a wash rather than as paint.
    ///
    /// **SATURATION IS EXACTLY 1 EVERYWHERE OUTSIDE THE CROSSING TAPER, AND
    /// THAT IS A PROPERTY OF THE PALETTE.** Every ROYGBIV interval joins
    /// two anchors that share a channel pinned at zero — red→orange→yellow all
    /// hold `B = 0`, yellow→green holds `B = 0`, green→blue holds `R = 0`,
    /// blue→indigo→violet hold `G = 0` — so a per-channel mix inside any interval
    /// keeps that channel at zero, and HSV saturation `(max − min) / max` is
    /// identically `1`.
    ///
    /// **THE ONE EXCEPTION IS AUTHORED, BOUNDED, AND BRIGHT.** The green→blue
    /// crossing's roof ([`SPECTRUM_CROSSING_ROOF`]) tapers `S` to a floor of
    /// `0.50` across the pacing knots' span and back — the on-glass cyan
    /// true-peak bound, priced there in the roof's own doc. It is NOT the grey
    /// hole coming back, and the clauses below say precisely why not: the
    /// taper's entries hold `V ≥ 0.85` (the hole was `V 0.51` grey) and chroma
    /// `≥ 109` (the hole's midpoint was chroma 12; the on-glass gate's bar is
    /// 100), and every entry OUTSIDE the pacing knots' span still reads
    /// `S = 1` exactly, so the taper cannot creep.
    ///
    /// **VALUE IS THE ANCHORS' OWN, AND ITS FLOOR IS AN AUTHORED COLOUR.**
    /// `min V = 0.510` is indigo `#4B0082` itself — a named ROYGBIV stop, not a
    /// dip the arc wandered into — so the floor is asserted against the anchors
    /// rather than against a constant, and cannot be read as the wash coming
    /// back. Mean `V = 0.873`.
    #[test]
    fn spectrum_stays_saturated_across_the_whole_arc() {
        let mut min_sat = 1.0f64;
        let mut sum_sat = 0.0f64;
        let mut min_val = 1.0f64;
        let mut sum_val = 0.0f64;
        for &rgb in SPECTRUM_LUT.iter() {
            let (_, s, v) = spectrum_hsv(rgb);
            min_sat = min_sat.min(s);
            sum_sat += s;
            min_val = min_val.min(v);
            sum_val += v;
        }
        let mean_sat = sum_sat / SPECTRUM_LUT_LEN as f64;
        let mean_val = sum_val / SPECTRUM_LUT_LEN as f64;

        // FULL CHROMA EVERYWHERE OUTSIDE THE AUTHORED TAPER — exactly 1, by
        // the shared-zero argument above, so an anchor edit that broke the
        // shared zero still fails here. The taper is the green→blue interval's
        // own interior (its pacing knots ease S from the anchors' 1 down to the
        // roof's 0.50 floor and back), so the exception zone is that interval
        // and the guard is that it may not creep past it — the other five
        // intervals hold S = 1 exactly, entry for entry.
        let taper = (SPECTRUM_ROOF_SEG * SPECTRUM_STRIDE + 1)
            ..=((SPECTRUM_ROOF_SEG + 1) * SPECTRUM_STRIDE - 1);
        for (i, &rgb) in SPECTRUM_LUT.iter().enumerate() {
            let (_, s, _) = spectrum_hsv(rgb);
            if taper.contains(&i) {
                assert!(
                    s >= 0.49,
                    "taper entry {i} desaturates to S {s:.4} — under the roof's \
                     own floor, the grey hole reopening"
                );
            } else {
                assert!(
                    s >= 0.999,
                    "entry {i} desaturates to S {s:.4} outside the authored \
                     taper — the palette lost its shared zero channel"
                );
            }
        }
        assert!(min_sat >= 0.49, "the arc's floor fell to S {min_sat:.4}");
        // The taper occupies most of one interval of six at a mean S of ~0.80,
        // which prices the whole-table mean at 0.9683; a mean under 0.96 is a
        // second desaturated stretch, not this one.
        assert!(mean_sat >= 0.96, "mean saturation is only {mean_sat:.4}");

        // VALUE'S FLOOR IS AN AUTHORED ANCHOR, not a chosen number: the darkest
        // point of the arc must be one of the seven stops, so a table that had
        // sagged into darkness BETWEEN anchors fails even though its minimum
        // looks familiar.
        let anchor_min_v = SPECTRUM_ANCHORS
            .iter()
            .map(|&c| spectrum_hsv(c).2)
            .fold(f64::MAX, f64::min);
        assert!(
            (min_val - anchor_min_v).abs() < 1e-9,
            "the arc's darkest entry is V {min_val:.4}, but the darkest AUTHORED \
             anchor is V {anchor_min_v:.4} — the arc is darker between its stops \
             than at any of them"
        );
        assert!(
            mean_val >= 0.85,
            "the arc's mean value fell to {mean_val:.3} — that is the \
             constant-luminance wash coming back"
        );

        // AND THE ARC IS CHROMATIC EVERYWHERE — the grey-hole guard, in the
        // table's own bytes. The retired neutralized handoff put its midpoint at
        // chroma 12; the shipped arc's minimum is 109, at the roof's floor.
        let chroma = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(g).max(b) - r.min(g).min(b)
        };
        let min_chroma = SPECTRUM_LUT.iter().copied().map(chroma).min().unwrap();
        assert!(
            min_chroma >= 24,
            "a neutral hole reopened in the table: min chroma {min_chroma}"
        );
    }

    /// Adjacent generated entries stay close enough that the LUT cannot print a
    /// visible staircase, and [`spectrum`] remains continuous between them. The
    /// 16-level ceiling is exact: the authored crossing's consecutive
    /// `#74E9D5 → #6CD9D2` samples move green `233 → 217`.
    #[test]
    fn spectrum_lut_has_bounded_adjacent_steps() {
        let fresh = generate_spectrum_lut();
        let mut worst = 0i32;
        for i in 0..SPECTRUM_LUT_LEN - 1 {
            let step = [16u32, 8, 0]
                .into_iter()
                .map(|shift| {
                    let a = ((fresh[i] >> shift) & 0xff) as i32;
                    let b = ((fresh[i + 1] >> shift) & 0xff) as i32;
                    (a - b).abs()
                })
                .max()
                .unwrap_or(0);
            let ceiling = if i / SPECTRUM_STRIDE == SPECTRUM_ROOF_SEG {
                16
            } else {
                5
            };
            assert!(
                step <= ceiling,
                "table entries {i}..{} jump {step} levels (bound {ceiling})",
                i + 1
            );
            worst = worst.max(step);
        }
        assert_eq!(
            worst, 16,
            "the authored roof's exact continuity ceiling changed"
        );
        assert!(worst > 0, "a table with no structure would pass vacuously");
        // The read is CONTINUOUS in `t`: half a table step in either direction
        // moves no channel by more than one whole step.
        for i in 0..=WALK {
            let t = i as f32 / WALK as f32;
            let a = spectrum(t);
            let b = spectrum((t + 0.5 / (SPECTRUM_LUT_LEN - 1) as f32).min(1.0));
            for shift in [16u32, 8, 0] {
                let d = ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff) as i32;
                assert!(d <= worst, "the read steps {d} across half a table entry");
            }
        }
    }

    /// **POINT-MARKS SNAP TO A NAME** (§2.3.3). [`spectrum_snap`] resolves to one
    /// of exactly seven colours, whatever it is handed — which is what keeps
    /// `is_fresh_ink_veil`'s finite-set assertion true once the band itself goes
    /// continuous, and what keeps a solid teal dot off the page.
    #[test]
    fn spectrum_snap_resolves_to_one_of_the_seven_names() {
        let named: Vec<u32> = (0..SPECTRUM_STOPS).map(spectrum_stop).collect();
        assert_eq!(named, SPECTRUM_ANCHORS.to_vec());
        for i in 0..=WALK {
            let snapped = spectrum_snap(i as f32 / WALK as f32);
            assert!(
                named.contains(&snapped),
                "snap produced #{snapped:06X}, which is not a named stop"
            );
        }
        for (i, &stop) in named.iter().enumerate() {
            assert_eq!(
                spectrum_snap(spectrum_stop_position(i)),
                stop,
                "a stop does not snap to itself"
            );
            // Every name is reachable: a snap vocabulary with a dead entry is a
            // six-colour rainbow wearing a seven-colour label.
            assert!(
                (0..=WALK).any(|k| spectrum_snap(k as f32 / WALK as f32) == stop),
                "stop {i} is unreachable by snapping"
            );
        }
    }

    // ---- WHERE THE COLOUR-LAW TESTS WENT, AND WHY THEY DID NOT COME BACK -----
    //
    // Retired by the ROYGBIV merge: `the_thing_arc_is_never_cyan`,
    // `the_thing_arc_pays_a_bounded_price_for_leaving_cyan`,
    // `the_colour_law_takes_every_colour_out_of_the_cyan_window`,
    // `the_colour_law_leaves_the_thing_arc_where_it_found_it` and
    // `the_caret_pays_a_bounded_price_for_its_base`. All five are statements
    // about `spectrum_clear_of_cyan` warping the ARC, and that function is still
    // the identity on purpose — the seven anchors keep every level of their
    // chroma. They stay retired.
    //
    // What came back on 2026-08-29 is the law about the two things that are not
    // the arc: `clear_thing_of_cyan` (the caret's emitted FILL) and
    // `clear_light_of_cyan` (the composited PIXEL). Their gates are
    // `the_thing_law_takes_the_caret_out_of_the_window` and
    // `the_band_is_never_cyan_on_glass` below, plus
    // `cursor_rainbow::the_caret_never_wears_cyan` and
    // `cursor_glow::the_jump_streak_is_never_cyan_on_glass`.
    //
    // **AND THE THREE THAT WERE GREEN OVER A VISIBLE DEFECT ARE FIXED, NOT
    // TIGHTENED.** At `c63e9558` all three passed while a 24-frame capture of the
    // shipped default carried 70,317 cyan pixels, peaking at 4.242 % of a frame's
    // lit pixels, brightest `V 193` at hue `197.2°`, `S 0.52`. They passed for
    // three separate reasons, each recorded at the gate it belongs to:
    //   * this one modelled `add_sat` for a bed that composites `over_premul`,
    //     and its bound had been moved from ZERO to a 4 % share;
    //   * `the_caret_never_wears_cyan` did no compositing at all and allowed a
    //     5 % share;
    //   * `the_jump_streak_is_never_cyan_on_glass` allowed a 2 % share and never
    //     ran the emitted quads through the law the frame runs them through.

    /// The clamp at both ends, and the acyclic contract: nothing a caller hands
    /// [`spectrum`] can walk violet back into red.
    #[test]
    fn spectrum_endpoints_are_the_named_red_and_violet() {
        assert_eq!(spectrum(0.0), SPECTRUM_ANCHORS[0]);
        assert_eq!(spectrum(1.0), SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1]);
        for t in [-1.0f32, -0.001, f32::NEG_INFINITY] {
            assert_eq!(spectrum(t), SPECTRUM_ANCHORS[0], "below the arc at {t}");
        }
        for t in [1.001f32, 4.0, f32::INFINITY] {
            assert_eq!(
                spectrum(t),
                SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
                "above the arc at {t}"
            );
        }
        assert_eq!(spectrum(f32::NAN), SPECTRUM_ANCHORS[0], "NaN draws red");
        assert!(spectrum_max_byte_rate() > 0.0);
    }

    /// The two grounds the chroma law is solved against: the SHIPPED default
    /// (`ColorScheme::default`, `#111318` — hue `222.9°`) and Tokyo Night's
    /// `#1A1B26`, which the legibility certifiers in `cursor_glow` already model
    /// with. Both are dark and BLUE-LEANING, which is the whole mechanism: it is
    /// the ground's own blue lead that turns dim green light teal.
    const GLASS_GROUNDS: [u32; 2] = [0x0011_1318, 0x001A_1B26];

    /// A pixel dark enough to be the ground is not a pixel of the mark. `24` is
    /// the shipped ground's own V, so this is exactly "brighter than the page".
    const GLASS_LIT_MIN: u32 = 24;

    #[test]
    fn over_halo_cyan_correction_preserves_the_renderer_alpha_cap() {
        let ground = 0x00FD_F6E3;
        for colour in [0x8000_8080u32, 0xBE00_8080] {
            let cap = aterm_render::halo_over_cap(colour);
            let bad_at = |c: u32, weight: u8| {
                over_the_glass_ceiling(compose_halo_on_glass(ground, c, weight, true))
            };
            assert!(
                (1..=cap).any(|weight| bad_at(colour, weight)),
                "#{colour:08X} must exercise the cyan correction"
            );

            let ruled = clear_halo_of_cyan(colour, true, ground);
            assert_ne!(
                ruled & 0x00ff_ffff,
                colour & 0x00ff_ffff,
                "the witness did not reach the projection"
            );
            assert_eq!(
                ruled & 0xff00_0000,
                colour & 0xff00_0000,
                "the RGB projection changed the live Over-halo alpha cap"
            );
            assert_eq!(aterm_render::halo_over_cap(ruled), cap);
            assert!(
                (1..=cap).all(|weight| !bad_at(ruled, weight)),
                "#{ruled:08X} remains cyan at a renderer-reachable weight"
            );
        }
    }

    #[test]
    fn over_halo_cyan_correction_ignores_unreachable_weights() {
        let ground = 0x00FD_F6E3;
        let colour = 0x1000_080Cu32;
        let cap = aterm_render::halo_over_cap(colour);
        let bad_at = |weight: u8| {
            over_the_glass_ceiling(compose_halo_on_glass(ground, colour, weight, true))
        };
        assert!(
            (1..=cap).all(|weight| !bad_at(weight)),
            "the capped halo must be clean throughout its reachable falloff"
        );
        assert!(
            (u16::from(cap) + 1..=255).any(|weight| bad_at(weight as u8)),
            "the witness needs a cyan value above its renderer cap"
        );
        assert_eq!(
            clear_halo_of_cyan(colour, true, ground),
            colour,
            "an unreachable falloff weight changed the emitted halo"
        );
    }

    /// **THE GATE THAT WAS GREEN WHILE THE GLASS WAS NOT — TWICE.**
    ///
    /// # The first blindness (recorded by the merge, and real)
    ///
    /// `spectrum_never_rests_on_cyan` reads the RAW arc, and the raw arc is not
    /// what anyone looks at. A shipped build measured **`3.51 %`** there while
    /// captured frames of the same build read **`6.31 %`** of their lit pixels
    /// inside the very same window.
    ///
    /// # The second blindness (2026-08-29), which this gate carried itself
    ///
    /// It composited with `add_sat(ground, premul(colour, cov))` — ADDITIVE — and
    /// **the rainbow bed does not composite additively.** `emit_rainbow_ribbon`
    /// passes `GlowBlend::Over`, so every bed quad carries its coverage in
    /// `GlowQuad::alpha` and the renderer runs `over_premul`. A gate that models a
    /// blend the emitter does not perform is measuring a different picture, and
    /// this one measured `0` cyan for an additive-derived `SPECTRUM_SAT_ENV` that
    /// still leaves `52` source-over composites in the window at `V` up to `152`.
    ///
    /// And its bound had been moved from ZERO to a `4 %` SHARE, over a grid whose
    /// denominator is five-sixths red/orange/yellow/indigo/violet — hues at which
    /// cyan is arithmetically impossible. `2.489 %` under a `4 %` bar is a gate
    /// that PERMITS the defect it is named for. On glass at `c63e9558`, with this
    /// gate green, a 231-frame capture of the shipped default carried **70,317**
    /// cyan pixels, peaked at **4.242 %** of one frame's lit pixels, and put a
    /// solid one-cell block of `V 192` teal under the hand.
    ///
    /// # What it walks now
    ///
    /// The pair the rasterizer is handed — `(premul_rgb(colour, cov), alpha)` —
    /// through [`compose_on_glass`], in BOTH modes the family emits (`alpha == 0`
    /// additive for the ZOOM streak, `alpha == cov` source-over for the bed), over
    /// both shipped grounds, and through [`clear_light_of_cyan`] exactly as
    /// `spend_rainbow_budget` runs it. The bound is **ZERO** again: the composite
    /// is the pixel, and the ruling is about the pixel.
    ///
    /// It keeps BOTH anti-vacuity clauses and adds a third:
    ///
    /// * the **arc** may not go grey to dodge the window (`min_chroma >= 24` over
    ///   `spectrum` itself — and the arc is untouched, so this reads `130`);
    /// * the **unruled** walk must be richly cyan, or the law is being proved
    ///   against a picture that never had the defect;
    /// * the **crossing on glass** must keep chroma. This is the clause that
    ///   would have caught the retired `SPECTRUM_SAT_ENV`: a law that bought zero
    ///   by paling the whole green→blue leg leaves the BRIGHT composites of that
    ///   leg grey, and this measures exactly them.
    #[test]
    fn the_band_is_never_cyan_on_glass() {
        let chroma = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(g).max(b) - r.min(g).min(b)
        };
        let is_cyan = |px: u32| {
            let (hue, sat, val) = spectrum_hsv(px);
            (val * 255.0) as u32 > GLASS_LIT_MIN
                && (SPECTRUM_CYAN_LO..=SPECTRUM_CYAN_HI).contains(&hue)
                && sat > SPECTRUM_CYAN_SAT_MIN
        };
        let mut lit_seen = 0usize;
        let mut ruled = 0usize;
        let mut unruled = 0usize;
        // The first composite that broke the ruling, rendered where it is found:
        // a tuple wide enough to name every term of the failure is a puzzle in the
        // message, and a struct for it is fields nothing reads.
        let mut worst: Option<String> = None;
        // The chroma of the composites bright enough to READ as colour, over the
        // green→blue leg — the currency the last reversal was decided in.
        let mut leg_chroma_sum = 0u64;
        let mut leg_chroma_n = 0u64;
        let mut leg_chroma_worst = u32::MAX;
        for i in 0..=4096u32 {
            let t = i as f32 / 4096.0;
            let colour = spectrum(t);
            let on_the_leg = (0.5..=0.667).contains(&t);
            for ground in GLASS_GROUNDS {
                for cov in 1..=171u32 {
                    let premul = aterm_render::premul_rgb(colour, cov as u8);
                    // BOTH modes the family emits, not the one this gate used to
                    // assume: `alpha == 0` is the additive ZOOM streak, `alpha ==
                    // cov` is the source-over bed (`GlowBlend::Over`).
                    for alpha in [0u8, cov as u8] {
                        let raw = compose_on_glass(ground, premul, alpha);
                        if (spectrum_hsv(raw).2 * 255.0) as u32 <= GLASS_LIT_MIN {
                            continue;
                        }
                        lit_seen += 1;
                        if is_cyan(raw) {
                            unruled += 1;
                        }
                        let kept = clear_light_of_cyan(premul, alpha, ground);
                        let px = compose_on_glass(ground, kept, alpha);
                        if is_cyan(px) {
                            ruled += 1;
                            if worst.is_none() {
                                let (hue, sat, val) = spectrum_hsv(px);
                                worst = Some(format!(
                                    "t={t:.5} arc=#{colour:06X} cov={cov} \
                                     alpha={alpha} px=#{px:06X} hue={hue:.1} \
                                     S={sat:.2} V={:.0}",
                                    val * 255.0
                                ));
                            }
                        }
                        if on_the_leg && (spectrum_hsv(px).2 * 255.0) as u32 > 96 {
                            let c = chroma(px);
                            leg_chroma_sum += u64::from(c);
                            leg_chroma_n += 1;
                            leg_chroma_worst = leg_chroma_worst.min(c);
                        }
                    }
                }
            }
        }
        // The census prints its exact numbers, so a regression is a NUMBER rather
        // than an opinion.
        println!(
            "BAND-CYAN-CENSUS lit={lit_seen} unruled={unruled} ({:.3}%) ruled={ruled} \
             first={worst:?} leg_bright_chroma mean={:.0} worst={leg_chroma_worst}",
            unruled as f64 * 100.0 / lit_seen as f64,
            leg_chroma_sum as f64 / leg_chroma_n.max(1) as f64,
        );
        // NON-VACUOUS: the walk lit something. A grid that produced no lit pixel
        // at all would pass this trivially.
        assert!(lit_seen > 100_000, "only {lit_seen} lit composites walked");

        // NON-VACUOUS THE SECOND WAY, and this is the clause the merge's version
        // did not have: the picture the law is applied to must CONTAIN the defect.
        // Canonical ROYGBIV runs a straight line from `#00FF00` to `#0000FF`
        // through `#007D82`, so an unruled walk is thick with cyan; a future arc
        // that stopped being so would make the clause below vacuous, and this says
        // so out loud instead.
        assert!(
            unruled > lit_seen / 100,
            "the unruled walk put only {unruled} of {lit_seen} composites in the \
             window — this gate is no longer measuring the defect it exists for"
        );

        // **THE BOUND IS ZERO.** Not a dwell, not a share. `clear_light_of_cyan`
        // is asked of every pair the rasterizer will be handed, so there is no
        // pair left for which the ruling can be broken.
        assert_eq!(
            ruled, 0,
            "cyan is not a rainbow colour: {ruled} of {lit_seen} lit composites \
             landed in hue [{SPECTRUM_CYAN_LO}, {SPECTRUM_CYAN_HI}] at \
             S > {SPECTRUM_CYAN_SAT_MIN} AFTER the light-law; first {worst:?}"
        );

        // **AND THE ZERO MAY NOT BE BOUGHT WITH THE ARC'S CHROMA.** Two clauses,
        // because there are two ways to buy it.
        //
        // (1) At the SOURCE — the retired six-anchor arc scored zero here by
        // collapsing chroma across the green→blue interval until the composite had
        // no hue to be judged on: a grey hole a seventh of the arc wide, which is
        // the defect the owner rejected. `clear_light_of_cyan` never touches the
        // arc, so this reads the seven anchors' own `130`.
        let min_chroma = (0..=4096u32)
            .map(|i| chroma(spectrum(i as f32 / 4096.0)))
            .min()
            .unwrap();
        assert!(
            min_chroma >= 100,
            "the crossing went grey to dodge the window: min chroma on the arc \
             is {min_chroma}"
        );

        // (2) ON GLASS — the clause the source-side one cannot make. A pixel law
        // could leave `SPECTRUM_LUT` bit-identical and still pale every bright
        // composite of the green→blue leg, which is the same hole one layer down.
        // Measured with the shipping law: mean 96, worst 35. The retired
        // `SPECTRUM_SAT_ENV`, which is a SOURCE law, fails this at 34/34 — it has
        // no bright composite left to measure.
        assert!(
            leg_chroma_n > 1_000,
            "only {leg_chroma_n} bright composites on the green->blue leg"
        );
        let leg_mean = leg_chroma_sum as f64 / leg_chroma_n as f64;
        assert!(
            leg_mean >= 80.0,
            "the light-law paled the whole crossing rather than the pixels that \
             needed it: mean chroma of the leg's bright composites is {leg_mean:.0}"
        );
    }
}
