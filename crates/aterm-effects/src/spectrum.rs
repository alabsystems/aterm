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
//! # The pace
//!
//! The arc is DRAWN in one coordinate and SAMPLED in another. The drawing
//! coordinate is the path ([`crate::spectrum::SPECTRUM_STRIDE`]), where every
//! authored position in this file lives; the table is then resampled from it so
//! that equal steps of `t` are equal steps of PERCEIVED colour
//! ([`crate::spectrum::SPECTRUM_PACE_HUE_SHARE`]), END TO END with no exempt
//! span. A ribbon reads one entry per cell, so this is what makes a typed line
//! a sweep instead of a set of vertical stripes.
//!
//! # Table and cost
//!
//! [`crate::spectrum::SPECTRUM_LUT`] contains
//! [`crate::spectrum::SPECTRUM_LUT_LEN`] (`511`) `0x00RRGGBB` entries,
//! or just under 2 KiB. The seven anchors land exactly on the indices
//! [`crate::spectrum::SPECTRUM_ANCHOR_AT`] names, and are stored verbatim
//! there. A spectrum read is two adjacent table lookups plus one lerp.

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

/// **THE GREEN→BLUE INTERVAL'S INDEX** — the one interval drawn in HSV rather
/// than per channel, because it is the one interval whose two anchors do NOT
/// share a bright channel.
///
/// # The defect the HSV draw removes, and the machinery it replaces (2026-09-15)
///
/// A straight per-channel line from `#00FF00` to `#0000FF` SAGS IN VALUE: its
/// midpoint is `#007D82`, `V 0.51`, the darkest point on the whole arc and
/// darker than any authored anchor. Every other interval is safe from this
/// because its two anchors share a channel pinned at `255` as well as one
/// pinned at `0` (red→orange→yellow hold `R = 255`, yellow→green hold
/// `G = 255`, blue→indigo→violet ride down from blue's own `B`), so a
/// per-channel mix inside them keeps full value as well as full chroma.
///
/// **THE RETIRED ANSWER WAS AN AUTHORED ROOF, AND IT WAS SHAPED BY A RULE THAT
/// IS GONE.** Until this commit the interval was drawn through eight authored
/// through-samples (`SPECTRUM_CROSSING_ROOF`) on a PCHIP, with four pacing
/// knots (`SPECTRUM_ROOF_PACE`) tapering saturation to a floor of `0.53` on
/// each side, and the whole span was LIFTED OUT OF THE PERCEPTUAL PACE so it
/// would be crossed fast. All three of those served the no-cyan ruling — the
/// roof dodged the `[165°, 200°]` window in as few entries as possible, the
/// taper bounded the saturation of on-glass blend pixels INSIDE that window,
/// and the pacing exemption kept the desaturated zone one ribbon slab wide.
/// That ruling was retired on 2026-09-01 (*"you can have cyan so long as it's
/// a rainbow"*; the anti-cyan laws *"were what greyed the arc"*), and on
/// 2026-09-15 the owner named what was left of it: *"this is legacy cruft.
/// delete it … this logic about cyan"*.
///
/// **SO THE INTERVAL IS DRAWN THE WAY THE OTHER SIX ARE PACED: BY WHAT THE EYE
/// SEES.** Both of its anchors are `S 1.00, V 1.00`; only their HUE differs
/// (`120° → 240°`). Interpolating all three HSV coordinates therefore holds
/// `S` and `V` at `1.00` across the entire leg and turns hue alone — which is
/// the maximum of both, so no other path through this interval can be brighter
/// or more chromatic. Its midpoint is `#00FFFF`, `V 1.000` against the retired
/// lerp's `V 0.51` and the retired roof's `V 0.92`: the "bright, not dim"
/// ruling answered at the one place on the arc that could not answer it.
///
/// The bright waypoint is not a stop: [`spectrum_snap`] still resolves to the
/// seven ROYGBIV anchors, and [`SPECTRUM_STOPS`] still counts seven.
const SPECTRUM_CROSSING_SEG: usize = 3;

/// The LUT's length: **511 entries, just under 2 KiB**, and the length of the
/// PATH the table is sampled from ([`SPECTRUM_STRIDE`]).
pub const SPECTRUM_LUT_LEN: usize = 511;

/// **THE PATH'S SLOTS PER ANCHOR INTERVAL** — the coordinate the arc is DRAWN
/// in, which since the 2026-08-31 re-pace is no longer the coordinate it is
/// SAMPLED in.
///
/// `85 * i` is anchor `i`, and the whole path is `510` units long. The
/// generator then RESAMPLES that path by perceptual pace
/// ([`generate_spectrum_lut`]), so a table index is a distance along the arc and
/// not a slot of it. Where the anchors ended up is [`SPECTRUM_ANCHOR_AT`],
/// which is committed and re-derived by the generator.
pub const SPECTRUM_STRIDE: usize = (SPECTRUM_LUT_LEN - 1) / (SPECTRUM_STOPS - 1);
const _: () = assert!(
    SPECTRUM_STRIDE * (SPECTRUM_STOPS - 1) == SPECTRUM_LUT_LEN - 1,
    "the seven anchors must land on exact path slots"
);

// ---------------------------------------------------------------------------
// THE PACE — how the path's 510 units are spent across the table's 510 steps.
// ---------------------------------------------------------------------------

/// **THE SHARE OF THE PACE THAT IS HUE**, against the share that is perceptual
/// distance — `0.60`.
///
/// # The defect (2026-08-31)
///
/// The owner: *"in 0.68, I'm seeing vertical stripes. That looks like a bug…
/// in the rainbow"*, and on the dev build after it, *"I still see this
/// problem"* — the fourth time in this campaign that the blending was called
/// not smooth.
///
/// It was the PARAMETERIZATION, and it was measurable without a camera. The
/// path above is paced by its own drawing law: [`smoothstep01`] has ZERO slope
/// at every anchor, so the arc DWELLS at each of the seven names and races
/// between them, and the seven anchor intervals carry wildly different amounts
/// of hue in the same 85 slots — `30°` from red to orange, `60°` from yellow to
/// green, `120°` across the crossing, and **`7.5°` from indigo to violet**. A
/// ribbon reads ONE table entry per cell, so equal cell steps sampled unequal
/// arc: measured on the shipped table at the traverse a 34-key line lays, the
/// per-cell hue steps ran `0.7° .. 34.9°` around a median of `10.8°` — a
/// fiftyfold spread. Where the hue stalls the eye sees a WIDE BLOCK of one
/// colour; where it leaps it sees a HARD EDGE. Those blocks and edges are the
/// vertical stripes.
///
/// # Why the pace is not perceptual distance alone
///
/// Re-pacing by cumulative ΔE in a uniform space (OKLab, [`spectrum_oklab`]) is
/// the right instrument and it is most of this constant — but ΔE alone answers
/// the wrong question at two places on THIS arc, and both were measured:
///
/// * **indigo → violet** is `7.5°` of hue and a large ΔE (`#4B0082` is dark,
///   `#9400D3` is bright), so pure-ΔE pacing spends `77` entries there and the
///   hue stalls at `1.8°`/cell — the violet block, back;
/// * **the greens**, hue `100° .. 150°`, are the opposite: a large hue span at
///   small ΔE, so pure-ΔE pacing crosses them in `46°` steps — the hard edge,
///   back.
///
/// So the pace is a BLEND of two currencies, each normalized by its own total
/// over the whole arc: perceptual distance, and HSV hue — the space this file
/// states every colour bound in. Measured over the whole blend, at the 16-cell
/// traverse:
///
/// ```text
///   hue share   per-cell hue step        per-cell ΔE
///               max/med   min/med        max/med   min/med
///   0.00 (ΔE)     3.53      0.23           1.30      0.45
///   0.50          1.42      0.35           1.77      0.27
///   0.60          1.33      0.40           2.10      0.28
///   0.70          1.24      0.49           2.20      0.24
///   1.00 (hue)    1.16      0.50           2.44      0.19
/// ```
///
/// `0.60` is where the hue spread is inside the owner-facing bar (`±40 %` of
/// its median) with the perceptual spread still under the shipped table's own
/// (`2.10` against `1.49`, and the green plateau it prices is the arc's, not
/// the pace's — the shipped table's own minimum there is `0.020`). Past `0.70`
/// the hue keeps improving by hundredths and the perceptual spread pays for all
/// of it.
const SPECTRUM_PACE_HUE_SHARE: f64 = 0.60;

/// **THE SHARE OF THE PACE THAT IS BYTES** — `0.30`, and it is what keeps the
/// table's own staircase bound.
///
/// A pace that answers only to the eye starves the places where the arc moves
/// far in BYTES for little perceived change: with this at zero the entry before
/// green read `#21FF00`, a `33`-level chord against
/// `spectrum_lut_has_bounded_adjacent_steps`' ceiling of `5`.
/// The third currency is the Chebyshev byte distance along the path, normalized
/// like the others, so a byte-fast stretch buys entries in proportion to how
/// fast it is. Measured: at `0.15` and above the worst adjacent step is `4`
/// levels — under the shipped ceiling, which therefore did not move. `0.30` is
/// the middle of the flat part of that curve (`0.15`
/// through `0.60` all measure `4`), and it costs the hue spread nothing
/// (`max/med 1.53 -> 1.58` between `0.0` and `0.30`).
const SPECTRUM_PACE_BYTE_SHARE: f64 = 0.30;

/// How finely the pacing integral is taken, in samples per path slot. `64` puts
/// about `32 600` samples on the path; the resulting entry positions move by
/// less than a thousandth of a slot if it is doubled, which is two orders under
/// the rounding that commits them.
const SPECTRUM_PACE_FINE: usize = 64;

/// **THE MOST OF THE TABLE THE GREEN→BLUE CROSSING MAY HAVE** — `167` of the
/// table's `510` steps, `32.75 %`, and the reason yellow survived this round.
///
/// # What it is for
///
/// [`SPECTRUM_CROSSING_SEG`] routed the crossing through a bright saturated
/// cyan and deleted the pacing exemption. Both were right and neither is in
/// question here. But a saturated route through `#00FFFF` is a LONGER route
/// than the pale one it replaced — more hue, more perceptual distance, more
/// bytes — and the pace pays for length. Uncapped it bought the crossing
/// `215` entries, `42.2 %` of the whole table, and every one of them came out
/// of the other five legs. Measured on the nearest-anchor regions, which is
/// the share of the arc that is nearest each name:
///
/// ```text
///                         shipped v0.86.0   uncapped   this cap
///   yellow                    19.1 %          15.1 %     17.5 %
///   red + orange + yellow     39.2 %          31.0 %     36.0 %
///   green + blue              45.6 %          57.1 %     50.1 %
/// ```
///
/// The owner's standing ruling is that he must SEE YELLOW (2026-09-13, *"I
/// don't see much yellow? that's confusing"*). Losing a fifth of yellow's arc
/// as the incidental price of fixing the crossing is not a trade he was
/// offered, and on the walk's first full pass it is the difference between
/// three yellow cells of seventeen and two. At this cap it is three again
/// (`cyancensus`, both walks, on the shipped arc and on this one).
///
/// # Why `167` and not a round number
///
/// `the_crossing_share_cap_trade` in `rainbow_kitty::ribbon` regenerates the
/// whole arc at every candidate cap, re-derives that arc's own `CROSS_PACE`,
/// and walks it. Two floors close on this value from opposite sides, and one
/// entry either way breaks one of them:
///
/// * **`168` entries and up, and the warm third falls under `36 %`.** The
///   warm third is `36.0 %` here and `35.9 %` at `168`. That is the
///   challenger's floor for the ruling above.
/// * **`166` entries and down, and the table's own staircase comes back.**
///   The worst adjacent byte chord in the whole table is `4` at `167` and
///   **`5`** at `166`, because entries taken off the crossing are entries its
///   own handoff no longer has. `4` is what routing through cyan bought —
///   `spectrum_lut_has_bounded_adjacent_steps` pins it exactly — and giving it
///   back would be re-buying the owner's own complaint one level at a time.
///
/// So the cap is the LEAST table the crossing can have while its own byte
/// continuity is still the `4` the cyan route bought, and that is also the
/// MOST it can have while the warm third holds `36 %`. Neither floor was
/// chosen to meet the other; they were measured separately and they meet.
///
/// # What the cap costs the crossing, which is little
///
/// The smoothing was bought by the cyan route and by deleting the roof, not by
/// the share. Over `8000` walk phases × a sixteen-cell pass, the worst step a
/// cell is asked to carry across the crossing, composited bed and vivid rail:
///
/// ```text
///                    shipped v0.86.0   uncapped 42.2 %   this cap 32.75 %
///   bed  worst            73.25            56.94              70.72
///   rail worst           129.19            44.22              56.49
/// ```
///
/// The rail — the vivid ink below the row, where the owner reads the arc —
/// keeps `86 %` of the whole fix at a third of the table's cost. The bed keeps
/// less, and [`SPECTRUM_CROSSING_SEG`]'s own note says why that number is a
/// tail rather than the body: the bed's MEDIAN over the same census is
/// `27.87` here against the shipped arc's `42.67`, a third off.
///
/// # How it is spent
///
/// A capped leg is spent EXACTLY and takes no part in the largest-remainder
/// round, so the crossing can never win a rounding entry back. What the cap
/// takes off it is returned to the other five legs IN PROPORTION TO THEIR OWN
/// COST, which restores the pace's own verdict on how the rest of the arc
/// should be spent rather than substituting a second opinion for it.
///
/// Stated with half an entry of margin either side of `167`, so that the
/// floor this resolves through cannot be moved by a rounding change.
const SPECTRUM_CROSSING_SHARE_CAP: f64 =
    (SPECTRUM_CROSSING_ENTRY_CAP as f64 + 0.5) / (SPECTRUM_LUT_LEN - 1) as f64;

/// [`SPECTRUM_CROSSING_SHARE_CAP`] in the unit it resolves to: table entries.
const SPECTRUM_CROSSING_ENTRY_CAP: usize = 167;

/// **WHERE THE SEVEN NAMES LANDED**, in table indices — `@generated` beside
/// [`SPECTRUM_LUT`] by the same pass, and pinned by
/// `spectrum_reproduces_its_anchors_exactly`.
///
/// Under the retired even parameterization this was `85 * i` and did not need
/// saying. The pace spends the table where the arc moves, so it is now a
/// measurement: red and violet still sit on the ends (the arc is a clamped
/// `[0, 1]` and its endpoints are the two turnarounds), and the five interior
/// names sit where their share of the pace put them. Every consumer that needs
/// a name — [`spectrum_stop`], [`spectrum_snap_index`] — reads it here rather
/// than multiplying, so a re-pace cannot leave a caller pointing at the wrong
/// colour.
pub const SPECTRUM_ANCHOR_AT: [usize; SPECTRUM_STOPS] = [0, 58, 130, 237, 404, 474, 510];

/// **THE CROSSING'S HUE WINDOW**, in **HSV degrees** — where the green→blue leg
/// reads as a cyan rather than as a green or a blue.
///
/// **IT IS A NAME FOR A STRETCH OF THE ARC, NOT A PROHIBITION.** It was the
/// no-cyan ruling's window (design §2.3.4), and every law that enforced that
/// ruling — the arc envelope, the thing-projection, the composited-pixel
/// ceiling, the halo ceiling — is deleted: the ruling was retired on 2026-09-01
/// (*"you can have cyan so long as it's a rainbow"*; the anti-cyan laws *"were
/// what greyed the arc"*) and its last machinery on 2026-09-15 (*"this is
/// legacy cruft. delete it"*). The arc now runs green→blue through `#00FFFF` at
/// full chroma and full value on purpose.
///
/// What still reads these is a LOCALITY census:
/// `rainbow_kitty::meteor::a_warm_landing_throws_no_cyan_and_the_seam_is_the_band_s_teal`
/// asks whether a burst that landed on the arc's warm third threw any light
/// from the arc's COOL third — "no cyan the band does not have". The window is
/// how "the crossing's colours" is spelled there. Nothing clamps to it.
pub const SPECTRUM_CYAN_LO: f64 = 165.0;
/// The top of [`SPECTRUM_CYAN_LO`]'s window.
pub const SPECTRUM_CYAN_HI: f64 = 200.0;
/// Below this HSV saturation a colour in the window is a GREY rather than a
/// crossing colour, so a census that is asking "did this mark carry the
/// crossing's light" must not count it. HSV `S` is a ratio and inflates without
/// bound near black, which is what this floor is for.
pub const SPECTRUM_CYAN_SAT_MIN: f64 = 0.3;

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

/// **THE PATH**, at any real position along it — the arc as it is DRAWN, before
/// the pace decides how much table to spend on each part of it.
///
/// `x` is in path slots, `0 ..= 510`: `85 * i` is anchor `i`. Five intervals are
/// [`smoothstep01`] followed by per-channel sRGB interpolation; the green→blue
/// interval ([`SPECTRUM_CROSSING_SEG`]) is the same easing applied to the two
/// anchors' HSV coordinates instead, which is what keeps it from sagging. Both
/// have zero slope at the seven named anchors, so the path reproduces every
/// anchor exactly and turns around without a colour crease.
///
/// It returns UNQUANTIZED sRGB. The pace integrates over tens of thousands of
/// samples of this and a `u8` staircase would make that integral measure the
/// rounding rather than the arc; rounding happens once, where an entry is
/// committed.
fn spectrum_path(x: f64) -> [f64; 3] {
    let x = x.clamp(0.0, (SPECTRUM_LUT_LEN - 1) as f64);
    let seg = ((x / SPECTRUM_STRIDE as f64).floor() as usize).min(SPECTRUM_STOPS - 2);
    let slot = x - (seg * SPECTRUM_STRIDE) as f64;
    let k = smoothstep01(slot / SPECTRUM_STRIDE as f64);
    // THE GREEN→BLUE INTERVAL IS DRAWN IN HSV, not per channel — see
    // [`SPECTRUM_CROSSING_SEG`]'s own account of why. All three coordinates are
    // READ OUT OF the two anchors, so the leg cannot disagree with them: both
    // are `S 1, V 1`, so this is a pure hue turn through `#00FFFF`.
    if seg == SPECTRUM_CROSSING_SEG {
        let (h0, s0, v0) = spectrum_hsv(SPECTRUM_ANCHORS[seg]);
        let (h1, s1, v1) = spectrum_hsv(SPECTRUM_ANCHORS[seg + 1]);
        return hsv_srgb(h0 + (h1 - h0) * k, s0 + (s1 - s0) * k, v0 + (v1 - v0) * k);
    }
    // THE EASED PER-CHANNEL MIX BETWEEN TWO AUTHORED STOPS, and nothing else.
    let chan = |c: u32, sh: u32| f64::from((c >> sh) & 0xff) / 255.0;
    let (a, b) = (SPECTRUM_ANCHORS[seg], SPECTRUM_ANCHORS[seg + 1]);
    [
        chan(a, 16) + (chan(b, 16) - chan(a, 16)) * k,
        chan(a, 8) + (chan(b, 8) - chan(a, 8)) * k,
        chan(a, 0) + (chan(b, 0) - chan(a, 0)) * k,
    ]
}

/// The path colour as the byte triple an entry would commit — one rounding, and
/// the same one [`spectrum_from_hsv`] performs.
fn spectrum_path_byte(x: f64) -> u32 {
    let c = spectrum_path(x);
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(c[0]) << 16) | (byte(c[1]) << 8) | byte(c[2])
}

/// **OKLab of an unquantized sRGB triple** — the uniform space the pace measures
/// perceived distance in.
///
/// Björn Ottosson's matrices, verbatim. It is used for ONE thing: the length of
/// a step along the path, so that equal steps of the table are equal steps of
/// the eye. Nothing that ships reads it — the arc's own colours are authored in
/// sRGB and its rulings are stated in HSV.
fn spectrum_oklab(c: [f64; 3]) -> [f64; 3] {
    let lin = |v: f64| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

/// The HSV hue of an unquantized triple, in degrees — [`spectrum_hsv`]'s hue,
/// asked of the path rather than of a committed byte.
fn spectrum_path_hue(c: [f64; 3]) -> f64 {
    let (r, g, b) = (c[0], c[1], c[2]);
    let hi = r.max(g).max(b);
    let d = hi - r.min(g).min(b);
    if d <= 0.0 {
        return 0.0;
    }
    if hi == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if hi == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    }
}

/// **THE GENERATOR.** Builds [`SPECTRUM_LUT`] and [`SPECTRUM_ANCHOR_AT`] by
/// resampling [`spectrum_path`] at an even PACE.
///
/// # What it does, in one paragraph
///
/// The path is cut into six legs at the seven anchors. EVERY leg spends entries
/// in proportion to its share of the pace — a blend of perceptual distance
/// ([`SPECTRUM_PACE_HUE_SHARE`]), HSV hue and byte distance
/// ([`SPECTRUM_PACE_BYTE_SHARE`]), each normalized by its own total — and places
/// them at EQUAL pace inside itself. The seven anchors fall on leg boundaries,
/// so they land on exact indices and are stored verbatim, as they always were.
///
/// **THERE IS NO EXEMPT SPAN, SINCE 2026-09-15.** The green→blue crossing used
/// to be lifted out of the pace and spent slot-for-slot, because the retired
/// no-cyan ruling wanted it crossed fast; what that bought on glass was the one
/// leg that LURCHED. See [`SPECTRUM_CROSSING_SEG`].
///
/// # Why the table and not the read
///
/// The pace could have lived in [`spectrum`], as a warp applied to `t`. It lives
/// in the TABLE instead so that everything that walks `SPECTRUM_LUT` — the
/// crossing census, the adjacent-step bound, the certifier's chord walk —
/// measures the arc that ships, at the spacing it ships at. A
/// warp behind the read would leave every one of those gates measuring a table
/// no eye ever sees.
///
/// The repo's discipline for a derived table (see `RAINBOW_BAND_COV_CAPS` and
/// `certify_rainbow_band_cov_caps`): the table ships as a `const` so it costs
/// nothing at run time, and a test regenerates it so the committed bytes can
/// never drift from the law that produced them.
#[must_use]
pub fn generate_spectrum_lut() -> [u32; SPECTRUM_LUT_LEN] {
    generate_spectrum_table().lut
}

/// **THE GENERATOR'S FULL RESULT** — the table, and the two things about it
/// that used to be arithmetic and are now measurements.
pub struct SpectrumTable {
    /// The committed [`SPECTRUM_LUT`].
    pub lut: [u32; SPECTRUM_LUT_LEN],
    /// The committed [`SPECTRUM_ANCHOR_AT`].
    pub anchor_at: [usize; SPECTRUM_STOPS],
    /// **WHERE EACH ENTRY CAME FROM ON THE PATH**, in path slots — the pace's
    /// own record, and the only way back from a table position to the drawing
    /// coordinate every authored position in this file is stated in. What reads
    /// it is `super::cursor_glow`'s legibility-ceiling table, which is solved
    /// per COLOUR and must therefore follow its colours through a re-pace.
    pub path_at: [f32; SPECTRUM_LUT_LEN],
}

/// [`generate_spectrum_lut`]'s full result — see [`SpectrumTable`].
#[must_use]
pub fn generate_spectrum_table() -> SpectrumTable {
    spectrum_table_at_cap(SPECTRUM_CROSSING_SHARE_CAP)
}

/// [`generate_spectrum_table`] with the crossing's share cap left OPEN, so the
/// cap can be swept rather than asserted — `the_crossing_share_cap_trade` in
/// `rainbow_kitty::ribbon` runs it across a range of caps and measures what
/// each one costs yellow and buys the crossing. The shipped table is this
/// function at [`SPECTRUM_CROSSING_SHARE_CAP`] and nothing else.
pub(crate) fn spectrum_table_at_cap(cap: f64) -> SpectrumTable {
    // THE SIX LEGS, in path slots: one per anchor interval, and nothing else.
    const LEGS: usize = SPECTRUM_STOPS - 1;
    let mut bounds = [0.0f64; LEGS + 1];
    for (i, b) in bounds.iter_mut().enumerate() {
        *b = (i * SPECTRUM_STRIDE) as f64;
    }

    // THE PACE, INTEGRATED. Three currencies, sampled together so they are the
    // same walk: perceptual distance, HSV hue, and Chebyshev bytes.
    let mut walk: Vec<Vec<(f64, [f64; 3])>> = Vec::with_capacity(LEGS);
    let mut totals = [0.0f64; 3];
    for leg in 0..LEGS {
        let (a, b) = (bounds[leg], bounds[leg + 1]);
        let steps = ((b - a) * SPECTRUM_PACE_FINE as f64) as usize;
        let mut cum = Vec::with_capacity(steps + 1);
        let mut acc = [0.0f64; 3];
        let mut prev = spectrum_path(a);
        cum.push((a, acc));
        for i in 1..=steps {
            let x = a + (b - a) * i as f64 / steps as f64;
            let cur = spectrum_path(x);
            let (pl, cl) = (spectrum_oklab(prev), spectrum_oklab(cur));
            acc[0] += ((pl[0] - cl[0]).powi(2) + (pl[1] - cl[1]).powi(2) + (pl[2] - cl[2]).powi(2))
                .sqrt();
            acc[1] += {
                let d = spectrum_path_hue(cur) - spectrum_path_hue(prev);
                (d - 360.0 * (d / 180.0).trunc()).abs()
            };
            acc[2] += (0..3)
                .map(|k| (prev[k] - cur[k]).abs())
                .fold(0.0f64, f64::max);
            cum.push((x, acc));
            prev = cur;
        }
        for (t, a) in totals.iter_mut().zip(acc) {
            *t += a;
        }
        walk.push(cum);
    }
    // ONE COST out of the three, each normalized by its own total, so the
    // blend's shares mean what they say whatever units the currencies are in.
    let cost = |c: [f64; 3]| {
        (1.0 - SPECTRUM_PACE_BYTE_SHARE)
            * ((1.0 - SPECTRUM_PACE_HUE_SHARE) * c[0] / totals[0]
                + SPECTRUM_PACE_HUE_SHARE * c[1] / totals[1])
            + SPECTRUM_PACE_BYTE_SHARE * c[2] / totals[2]
    };

    // THE BUDGETS. Every leg shares the table in proportion to cost, by largest
    // remainder so the table is exactly `SPECTRUM_LUT_LEN` long however the
    // fractions fall — EXCEPT that the crossing's share is capped
    // ([`SPECTRUM_CROSSING_SHARE_CAP`]) and the entries the cap takes off it go
    // back to the other five legs in proportion to their own cost.
    let mut budget = [0usize; LEGS];
    let mut want = [0.0f64; LEGS];
    let legcost: Vec<f64> = (0..LEGS)
        .map(|l| cost(walk[l].last().expect("a leg has samples").1))
        .collect();
    let free: f64 = legcost.iter().sum();
    let purse = SPECTRUM_LUT_LEN - 1;
    for leg in 0..LEGS {
        want[leg] = legcost[leg] / free * purse as f64;
    }

    // THE CAP, APPLIED. A capped leg is spent EXACTLY, so it takes no part in
    // the largest-remainder round below: the other five legs share the whole of
    // what is left, and the crossing cannot win a rounding entry back.
    let ceiling = (cap * purse as f64).floor();
    let capped = want[SPECTRUM_CROSSING_SEG] > ceiling;
    if capped {
        let surplus = want[SPECTRUM_CROSSING_SEG] - ceiling;
        want[SPECTRUM_CROSSING_SEG] = ceiling;
        let rest: f64 = (0..LEGS)
            .filter(|&l| l != SPECTRUM_CROSSING_SEG)
            .map(|l| legcost[l])
            .sum();
        for (leg, w) in want.iter_mut().enumerate() {
            if leg != SPECTRUM_CROSSING_SEG {
                *w += surplus * legcost[leg] / rest;
            }
        }
    }

    for leg in 0..LEGS {
        budget[leg] = want[leg].floor() as usize;
    }
    let mut order: Vec<usize> = (0..LEGS)
        .filter(|&l| !(capped && l == SPECTRUM_CROSSING_SEG))
        .collect();
    order.sort_by(|&a, &b| {
        (want[b] - budget[b] as f64)
            .partial_cmp(&(want[a] - budget[a] as f64))
            .expect("the pace is finite")
    });
    let mut short = SPECTRUM_LUT_LEN - 1 - budget.iter().sum::<usize>();
    for &leg in &order {
        if short == 0 {
            break;
        }
        budget[leg] += 1;
        short -= 1;
    }
    assert_eq!(short, 0, "the pace could not spend the table");

    // THE ENTRIES.
    let mut lut = [0u32; SPECTRUM_LUT_LEN];
    let mut path_at = [0.0f32; SPECTRUM_LUT_LEN];
    let mut anchor_at = [0usize; SPECTRUM_STOPS];
    let mut idx = 0usize;
    for leg in 0..LEGS {
        // WHICH NAME, IF ANY, THIS LEG STARTS ON — the anchors are leg
        // boundaries, so they land on exact indices and are stored VERBATIM,
        // bit-for-bit the seven named constants.
        let start = bounds[leg];
        if (start as usize).is_multiple_of(SPECTRUM_STRIDE) && (start as usize) < SPECTRUM_LUT_LEN {
            anchor_at[start as usize / SPECTRUM_STRIDE] = idx;
        }
        let total = cost(walk[leg].last().expect("a leg has samples").1);
        for j in 0..budget[leg] {
            let x = spectrum_pace_at(&walk[leg], &cost, total * j as f64 / budget[leg] as f64);
            lut[idx + j] = spectrum_path_byte(x);
            path_at[idx + j] = x as f32;
        }
        idx += budget[leg];
    }
    anchor_at[SPECTRUM_STOPS - 1] = SPECTRUM_LUT_LEN - 1;
    path_at[SPECTRUM_LUT_LEN - 1] = (SPECTRUM_LUT_LEN - 1) as f32;
    for (i, &at) in anchor_at.iter().enumerate() {
        lut[at] = SPECTRUM_ANCHORS[i];
        path_at[at] = (i * SPECTRUM_STRIDE) as f32;
    }
    SpectrumTable {
        lut,
        anchor_at,
        path_at,
    }
}

/// **WHERE THE GREEN→BLUE CROSSING SITS**, in `t` — the table entry whose hue
/// is midway between the green and blue anchors' OWN hues.
///
/// Several fixtures and non-vacuity controls in this crate need to point at the
/// crossing — "aim the sample window at the handoff" — and every one of them
/// used to transcribe a number (`0.5843`, `0.52`, `3.5/6`) that was true of
/// where the crossing sat in the path. The pace moves it, and a transcribed
/// position does not follow, so the question is answered from the table.
///
/// **DERIVED FROM THE ANCHORS, NOT FROM THE RETIRED WINDOW.** It used to aim at
/// the centre of the cyan window `[165°, 200°]`, which was the same `182.5°` by
/// coincidence of that window's own placement. The window is a retired law's
/// vocabulary; the two anchors it sits between are not, and they are what the
/// leg is drawn from.
#[cfg(test)]
#[must_use]
pub(crate) fn spectrum_crossing_position() -> f32 {
    let mid = 0.5
        * (spectrum_hsv(SPECTRUM_ANCHORS[SPECTRUM_CROSSING_SEG]).0
            + spectrum_hsv(SPECTRUM_ANCHORS[SPECTRUM_CROSSING_SEG + 1]).0);
    let (mut best, mut near) = (0usize, f64::MAX);
    for (i, &c) in SPECTRUM_LUT.iter().enumerate() {
        let d = (spectrum_hsv(c).0 - mid).abs();
        if d < near {
            near = d;
            best = i;
        }
    }
    best as f32 / (SPECTRUM_LUT_LEN - 1) as f32
}

/// **HOW WIDE THE GREEN→BLUE LEG IS IN `t`** — from the green anchor to the
/// blue one.
///
/// **THIS USED TO BE THE EXEMPT SPAN'S WIDTH** (`39` of `511` entries), and the
/// exempt span is deleted ([`SPECTRUM_CROSSING_SEG`]), so the honest unit for
/// "how far must a control step to be clear of the crossing" is the leg the
/// crossing IS. Callers take their own fraction of it and say which.
#[cfg(test)]
#[must_use]
pub(crate) fn spectrum_crossing_span() -> f32 {
    (SPECTRUM_ANCHOR_AT[SPECTRUM_CROSSING_SEG + 1] - SPECTRUM_ANCHOR_AT[SPECTRUM_CROSSING_SEG])
        as f32
        / (SPECTRUM_LUT_LEN - 1) as f32
}

/// Where along a leg the pace has spent `want` — the inverse of the cumulative
/// cost, by binary search and one linear step inside the bracket it lands in.
fn spectrum_pace_at(cum: &[(f64, [f64; 3])], cost: &impl Fn([f64; 3]) -> f64, want: f64) -> f64 {
    if want <= 0.0 {
        return cum[0].0;
    }
    let (mut lo, mut hi) = (0usize, cum.len() - 1);
    if want >= cost(cum[hi].1) {
        return cum[hi].0;
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if cost(cum[mid].1) <= want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (c0, c1) = (cost(cum[lo].1), cost(cum[hi].1));
    if c1 <= c0 {
        return cum[lo].0;
    }
    cum[lo].0 + (cum[hi].0 - cum[lo].0) * (want - c0) / (c1 - c0)
}

// ---------------------------------------------------------------------------
// THE READ
// ---------------------------------------------------------------------------

/// THE ONE SPECTRUM, at spectrum position `t` — `0` is red, `1` is violet.
///
/// Two adjacent table lookups and one lerp. INTERPOLATED, NOT ROUNDED: a nearest-entry
/// lookup would put a 511-entry
/// staircase back on top of a curve built to remove one. Adjacent entries are at
/// most 5 levels apart on the five ordinary intervals and at most 15 through the
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

/// **A PRODUCER'S CHROMA FLOOR** — `rgb` with its HSV saturation raised to at
/// least `s_min`; hue and value are exactly what they were, and a grey (no hue
/// to keep), a black, or a non-finite floor comes back untouched.
///
/// # What it was for, and what it is now
///
/// The arc used to carry ONE authored dip in chroma: the green→blue crossing's
/// retired roof held `S 0.53` with its pacing knots tapering `0.65` / `0.72`
/// into it, and that taper was the retired no-cyan census's blend bound. It is
/// what the owner reported on 2026-09-08 as "black gaps in the rainbow a few
/// characters back from the cursor", and this operation is the answer the
/// producers took: `rainbow_kitty::ribbon::bed_ink` through `BED_SAT_FLOOR`
/// gives the seam its chroma back at ITS OWN read. Measured where it was spent:
/// the bed's crossing cell went from composited `(45, 92, 93)` — `S 0.52`, the
/// greyest cell on a typed line — to `(3, 95, 95)`, `S 0.97`, at the same
/// relative luminance.
///
/// **THE DIP ITSELF IS GONE SINCE 2026-09-15** ([`SPECTRUM_CROSSING_SEG`]): the
/// crossing is drawn in HSV between two `S 1` anchors, so `SPECTRUM_LUT` is now
/// `S 1.00` end to end and this is the identity on every read of the arc. It is
/// kept because it is a FLOOR on a producer's own input and `bed_ink` is a
/// public function taking any colour — a guard that has nothing to do is not
/// the same thing as a guard that is wrong.
///
/// The move is HSV's own: every channel's distance below the maximum is
/// scaled by `s_min · max / (max − min)`, so the maximum channel (the value)
/// and the channel ORDER (the hue) are untouched and the minimum channel lands
/// at `max · (1 − s_min)`. A colour already at or over the floor is returned
/// bit for bit.
#[must_use]
pub fn spectrum_with_min_saturation(rgb: u32, s_min: f32) -> u32 {
    if !s_min.is_finite() {
        return rgb;
    }
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    if mx == 0 || mx == mn {
        return rgb;
    }
    let s_min = s_min.clamp(0.0, 1.0);
    let spread = (mx - mn) as f32;
    if spread >= s_min * mx as f32 {
        return rgb;
    }
    let k = s_min * mx as f32 / spread;
    let ch = |c: u32| (mx as f32 - (mx - c) as f32 * k + 0.5).clamp(0.0, 255.0) as u32;
    (ch(r) << 16) | (ch(g) << 8) | ch(b)
}

/// **THE PIXEL THE EMITTER ACTUALLY WRITES**, for one glow quad over one ground.
///
/// `GlowQuad` carries a PREMULTIPLIED colour and one byte that is both the
/// source-over opacity and the mode selector: `alpha == 0` is additive light
/// (`out = dst + color`), anything above it is premultiplied source-over. Both
/// spellings live in `aterm_render`, and this calls THEM rather than restating
/// them, so the law cannot end up reasoning about a blend the renderer does not
/// perform. That is not hypothetical: the retired on-glass cyan census modelled
/// `add_sat` for a bed that composites source-over, and went blind for two days
/// over a visible defect because of it.
#[inline]
#[must_use]
pub fn compose_on_glass(ground: u32, premul: u32, alpha: u8) -> u32 {
    if alpha == 0 {
        aterm_render::add_sat(ground, premul)
    } else {
        aterm_render::over_premul(ground, premul, alpha)
    }
}

/// Where named stop `i` sits in `t` — `0` is red, `SPECTRUM_STOPS - 1` is
/// violet. Out-of-range indices clamp to the ends.
///
/// **READ OUT OF [`SPECTRUM_ANCHOR_AT`], NOT DIVIDED.** The stops were evenly
/// spaced in `t` while the table was evenly spaced along the path; since the
/// 2026-08-31 re-pace the table is spaced by PACE, so a name's position is
/// wherever the pace put it. It is still exact — the anchors are leg boundaries,
/// so each lands on an integer index and `spectrum(stop_position(i))` still
/// resolves to the anchor's own bytes.
#[inline]
#[must_use]
pub(crate) fn spectrum_stop_position(i: usize) -> f32 {
    SPECTRUM_ANCHOR_AT[i.min(SPECTRUM_STOPS - 1)] as f32 / (SPECTRUM_LUT_LEN - 1) as f32
}

/// The colour of named stop `i` — red, orange, yellow, green, blue, indigo,
/// violet.
///
/// Read straight out of the table at its exact index, so it is the anchor
/// constant itself and not a resolve that happens to agree.
#[inline]
#[must_use]
pub fn spectrum_stop(i: usize) -> u32 {
    SPECTRUM_LUT[SPECTRUM_ANCHOR_AT[i.min(SPECTRUM_STOPS - 1)]]
}

/// WHICH NAME IS THIS? The nearest named stop's INDEX to spectrum position `t`.
///
/// Carried as an index rather than a colour so a caller that needs the darkened
/// light-theme ink for a snapped mark can read it out of a precomputed table
/// instead of re-running the recipe (see `InkRole::band_ink`), and so the two
/// can never disagree about which name a position has.
///
/// **NEAREST ON THE TABLE, NOT ON A DIVISION.** While the stops sat at `85 * i`
/// this was a multiply and a round. The pace moved them
/// ([`SPECTRUM_ANCHOR_AT`]), so the question is asked where the answer lives:
/// which committed anchor index is `t` nearest. Seven compares, none of them
/// able to disagree with [`spectrum_stop`].
#[inline]
#[must_use]
pub fn spectrum_snap_index(t: f32) -> usize {
    let x = t.clamp(0.0, 1.0) * (SPECTRUM_LUT_LEN - 1) as f32;
    let mut best = 0usize;
    let mut near = f32::INFINITY;
    for (i, &at) in SPECTRUM_ANCHOR_AT.iter().enumerate() {
        let d = (x - at as f32).abs();
        if d < near {
            near = d;
            best = i;
        }
    }
    best
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
/// anchors sit verbatim at the indices [`SPECTRUM_ANCHOR_AT`] names; entries
/// between them are the drawn path, resampled at an even PACE.
///
/// Regenerate after any change to the anchors or interpolation law:
///
/// ```text
/// targo --unverified test -p aterm-effects --lib emit_spectrum_lut \
///     -- --ignored --nocapture
/// ```
#[rustfmt::skip]
pub const SPECTRUM_LUT: [u32; SPECTRUM_LUT_LEN] = [
    0x00FF_0000, 0x00FF_0300, 0x00FF_0500, 0x00FF_0800, 0x00FF_0B00, 0x00FF_0D00,
    0x00FF_1000, 0x00FF_1300, 0x00FF_1500, 0x00FF_1800, 0x00FF_1A00, 0x00FF_1D00,
    0x00FF_1F00, 0x00FF_2200, 0x00FF_2400, 0x00FF_2600, 0x00FF_2900, 0x00FF_2B00,
    0x00FF_2D00, 0x00FF_3000, 0x00FF_3200, 0x00FF_3400, 0x00FF_3700, 0x00FF_3900,
    0x00FF_3B00, 0x00FF_3D00, 0x00FF_3F00, 0x00FF_4200, 0x00FF_4400, 0x00FF_4600,
    0x00FF_4800, 0x00FF_4A00, 0x00FF_4C00, 0x00FF_4E00, 0x00FF_5000, 0x00FF_5200,
    0x00FF_5400, 0x00FF_5600, 0x00FF_5800, 0x00FF_5A00, 0x00FF_5C00, 0x00FF_5E00,
    0x00FF_6000, 0x00FF_6200, 0x00FF_6400, 0x00FF_6600, 0x00FF_6800, 0x00FF_6A00,
    0x00FF_6C00, 0x00FF_6E00, 0x00FF_7000, 0x00FF_7200, 0x00FF_7400, 0x00FF_7600,
    0x00FF_7800, 0x00FF_7900, 0x00FF_7B00, 0x00FF_7D00, 0x00FF_7F00, 0x00FF_8100,
    0x00FF_8300, 0x00FF_8500, 0x00FF_8600, 0x00FF_8800, 0x00FF_8A00, 0x00FF_8C00,
    0x00FF_8E00, 0x00FF_9000, 0x00FF_9100, 0x00FF_9300, 0x00FF_9500, 0x00FF_9700,
    0x00FF_9900, 0x00FF_9A00, 0x00FF_9C00, 0x00FF_9E00, 0x00FF_A000, 0x00FF_A200,
    0x00FF_A300, 0x00FF_A500, 0x00FF_A700, 0x00FF_A900, 0x00FF_AB00, 0x00FF_AC00,
    0x00FF_AE00, 0x00FF_B000, 0x00FF_B200, 0x00FF_B300, 0x00FF_B500, 0x00FF_B700,
    0x00FF_B900, 0x00FF_BB00, 0x00FF_BC00, 0x00FF_BE00, 0x00FF_C000, 0x00FF_C200,
    0x00FF_C300, 0x00FF_C500, 0x00FF_C700, 0x00FF_C900, 0x00FF_CA00, 0x00FF_CC00,
    0x00FF_CE00, 0x00FF_D000, 0x00FF_D100, 0x00FF_D300, 0x00FF_D500, 0x00FF_D700,
    0x00FF_D800, 0x00FF_DA00, 0x00FF_DC00, 0x00FF_DE00, 0x00FF_DF00, 0x00FF_E100,
    0x00FF_E300, 0x00FF_E500, 0x00FF_E600, 0x00FF_E800, 0x00FF_EA00, 0x00FF_EC00,
    0x00FF_ED00, 0x00FF_EF00, 0x00FF_F100, 0x00FF_F300, 0x00FF_F400, 0x00FF_F600,
    0x00FF_F800, 0x00FF_FA00, 0x00FF_FB00, 0x00FF_FD00, 0x00FF_FF00, 0x00FD_FF00,
    0x00FB_FF00, 0x00F9_FF00, 0x00F7_FF00, 0x00F4_FF00, 0x00F2_FF00, 0x00F0_FF00,
    0x00EE_FF00, 0x00EC_FF00, 0x00EA_FF00, 0x00E8_FF00, 0x00E5_FF00, 0x00E3_FF00,
    0x00E1_FF00, 0x00DF_FF00, 0x00DD_FF00, 0x00DB_FF00, 0x00D9_FF00, 0x00D6_FF00,
    0x00D4_FF00, 0x00D2_FF00, 0x00D0_FF00, 0x00CE_FF00, 0x00CC_FF00, 0x00C9_FF00,
    0x00C7_FF00, 0x00C5_FF00, 0x00C3_FF00, 0x00C1_FF00, 0x00BE_FF00, 0x00BC_FF00,
    0x00BA_FF00, 0x00B8_FF00, 0x00B6_FF00, 0x00B3_FF00, 0x00B1_FF00, 0x00AF_FF00,
    0x00AD_FF00, 0x00AA_FF00, 0x00A8_FF00, 0x00A6_FF00, 0x00A4_FF00, 0x00A1_FF00,
    0x009F_FF00, 0x009D_FF00, 0x009A_FF00, 0x0098_FF00, 0x0096_FF00, 0x0094_FF00,
    0x0091_FF00, 0x008F_FF00, 0x008D_FF00, 0x008A_FF00, 0x0088_FF00, 0x0086_FF00,
    0x0083_FF00, 0x0081_FF00, 0x007F_FF00, 0x007C_FF00, 0x007A_FF00, 0x0077_FF00,
    0x0075_FF00, 0x0073_FF00, 0x0070_FF00, 0x006E_FF00, 0x006B_FF00, 0x0069_FF00,
    0x0067_FF00, 0x0064_FF00, 0x0062_FF00, 0x005F_FF00, 0x005D_FF00, 0x005A_FF00,
    0x0058_FF00, 0x0055_FF00, 0x0053_FF00, 0x0050_FF00, 0x004E_FF00, 0x004B_FF00,
    0x0049_FF00, 0x0046_FF00, 0x0043_FF00, 0x0041_FF00, 0x003E_FF00, 0x003C_FF00,
    0x0039_FF00, 0x0036_FF00, 0x0034_FF00, 0x0031_FF00, 0x002E_FF00, 0x002C_FF00,
    0x0029_FF00, 0x0026_FF00, 0x0024_FF00, 0x0021_FF00, 0x001E_FF00, 0x001C_FF00,
    0x0019_FF00, 0x0016_FF00, 0x0013_FF00, 0x0011_FF00, 0x000E_FF00, 0x000B_FF00,
    0x0008_FF00, 0x0006_FF00, 0x0003_FF00, 0x0000_FF00, 0x0000_FF04, 0x0000_FF08,
    0x0000_FF0C, 0x0000_FF10, 0x0000_FF14, 0x0000_FF18, 0x0000_FF1C, 0x0000_FF20,
    0x0000_FF24, 0x0000_FF28, 0x0000_FF2C, 0x0000_FF30, 0x0000_FF34, 0x0000_FF38,
    0x0000_FF3B, 0x0000_FF3F, 0x0000_FF43, 0x0000_FF46, 0x0000_FF4A, 0x0000_FF4E,
    0x0000_FF51, 0x0000_FF55, 0x0000_FF58, 0x0000_FF5C, 0x0000_FF5F, 0x0000_FF63,
    0x0000_FF66, 0x0000_FF6A, 0x0000_FF6D, 0x0000_FF71, 0x0000_FF74, 0x0000_FF77,
    0x0000_FF7B, 0x0000_FF7E, 0x0000_FF81, 0x0000_FF85, 0x0000_FF88, 0x0000_FF8B,
    0x0000_FF8F, 0x0000_FF92, 0x0000_FF95, 0x0000_FF98, 0x0000_FF9C, 0x0000_FF9F,
    0x0000_FFA2, 0x0000_FFA5, 0x0000_FFA9, 0x0000_FFAC, 0x0000_FFAF, 0x0000_FFB2,
    0x0000_FFB5, 0x0000_FFB9, 0x0000_FFBC, 0x0000_FFBF, 0x0000_FFC2, 0x0000_FFC5,
    0x0000_FFC8, 0x0000_FFCC, 0x0000_FFCF, 0x0000_FFD2, 0x0000_FFD5, 0x0000_FFD8,
    0x0000_FFDB, 0x0000_FFDF, 0x0000_FFE2, 0x0000_FFE5, 0x0000_FFE8, 0x0000_FFEB,
    0x0000_FFEE, 0x0000_FFF1, 0x0000_FFF4, 0x0000_FFF8, 0x0000_FFFB, 0x0000_FFFE,
    0x0000_FDFF, 0x0000_FBFF, 0x0000_F8FF, 0x0000_F6FF, 0x0000_F4FF, 0x0000_F1FF,
    0x0000_EFFF, 0x0000_ECFF, 0x0000_EAFF, 0x0000_E7FF, 0x0000_E5FF, 0x0000_E2FF,
    0x0000_E0FF, 0x0000_DDFF, 0x0000_DBFF, 0x0000_D8FF, 0x0000_D6FF, 0x0000_D3FF,
    0x0000_D1FF, 0x0000_CEFF, 0x0000_CCFF, 0x0000_C9FF, 0x0000_C7FF, 0x0000_C4FF,
    0x0000_C2FF, 0x0000_BFFF, 0x0000_BDFF, 0x0000_BAFF, 0x0000_B8FF, 0x0000_B5FF,
    0x0000_B3FF, 0x0000_B0FF, 0x0000_AEFF, 0x0000_ABFF, 0x0000_A9FF, 0x0000_A6FF,
    0x0000_A4FF, 0x0000_A1FF, 0x0000_9FFF, 0x0000_9CFF, 0x0000_9AFF, 0x0000_97FF,
    0x0000_95FF, 0x0000_92FF, 0x0000_90FF, 0x0000_8DFF, 0x0000_8AFF, 0x0000_88FF,
    0x0000_85FF, 0x0000_83FF, 0x0000_80FF, 0x0000_7EFF, 0x0000_7BFF, 0x0000_78FF,
    0x0000_76FF, 0x0000_73FF, 0x0000_70FF, 0x0000_6EFF, 0x0000_6BFF, 0x0000_69FF,
    0x0000_66FF, 0x0000_63FF, 0x0000_60FF, 0x0000_5EFF, 0x0000_5BFF, 0x0000_58FF,
    0x0000_55FF, 0x0000_53FF, 0x0000_50FF, 0x0000_4DFF, 0x0000_4AFF, 0x0000_47FF,
    0x0000_44FF, 0x0000_41FF, 0x0000_3EFF, 0x0000_3BFF, 0x0000_38FF, 0x0000_35FF,
    0x0000_32FF, 0x0000_2FFF, 0x0000_2BFF, 0x0000_28FF, 0x0000_25FF, 0x0000_21FF,
    0x0000_1EFF, 0x0000_1AFF, 0x0000_17FF, 0x0000_13FF, 0x0000_0FFF, 0x0000_0CFF,
    0x0000_08FF, 0x0000_04FF, 0x0000_00FF, 0x0001_00FD, 0x0003_00FA, 0x0004_00F8,
    0x0006_00F5, 0x0007_00F3, 0x0009_00F1, 0x000A_00EE, 0x000B_00EC, 0x000D_00EA,
    0x000E_00E8, 0x000F_00E5, 0x0011_00E3, 0x0012_00E1, 0x0013_00DF, 0x0015_00DC,
    0x0016_00DA, 0x0017_00D8, 0x0019_00D6, 0x001A_00D4, 0x001B_00D2, 0x001C_00D0,
    0x001E_00CE, 0x001F_00CC, 0x0020_00CA, 0x0021_00C8, 0x0022_00C6, 0x0023_00C4,
    0x0025_00C2, 0x0026_00C0, 0x0027_00BE, 0x0028_00BC, 0x0029_00BB, 0x002A_00B9,
    0x002B_00B7, 0x002C_00B5, 0x002D_00B3, 0x002E_00B2, 0x002F_00B0, 0x0030_00AE,
    0x0031_00AD, 0x0032_00AB, 0x0033_00A9, 0x0034_00A8, 0x0035_00A6, 0x0036_00A4,
    0x0037_00A3, 0x0038_00A1, 0x0039_00A0, 0x003A_009E, 0x003B_009D, 0x003C_009B,
    0x003D_009A, 0x003E_0098, 0x003E_0097, 0x003F_0096, 0x0040_0094, 0x0041_0093,
    0x0042_0091, 0x0043_0090, 0x0043_008F, 0x0044_008D, 0x0045_008C, 0x0046_008B,
    0x0047_0089, 0x0047_0088, 0x0048_0087, 0x0049_0086, 0x004A_0084, 0x004A_0083,
    0x004B_0082, 0x004D_0084, 0x004F_0086, 0x0050_0088, 0x0052_008A, 0x0054_008C,
    0x0056_008E, 0x0058_0090, 0x005A_0092, 0x005C_0094, 0x005E_0097, 0x005F_0099,
    0x0061_009B, 0x0063_009D, 0x0065_009F, 0x0067_00A1, 0x0069_00A4, 0x006B_00A6,
    0x006D_00A8, 0x006F_00AA, 0x0072_00AD, 0x0074_00AF, 0x0076_00B1, 0x0078_00B4,
    0x007A_00B6, 0x007C_00B8, 0x007E_00BB, 0x0080_00BD, 0x0082_00C0, 0x0085_00C2,
    0x0087_00C4, 0x0089_00C7, 0x008B_00C9, 0x008D_00CC, 0x0090_00CE, 0x0092_00D1,
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
        let table = generate_spectrum_table();
        let (lut, anchor_at) = (table.lut, table.anchor_at);
        println!("pub const SPECTRUM_ANCHOR_AT: [usize; SPECTRUM_STOPS] = {anchor_at:?};");
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
        // WHERE THE NAMES LANDED IS ITSELF GENERATED. The pace decides it, so
        // the committed index table is checked against the generator's before
        // anything is read through it.
        let anchor_at = generate_spectrum_table().anchor_at;
        assert_eq!(
            anchor_at, SPECTRUM_ANCHOR_AT,
            "SPECTRUM_ANCHOR_AT drifted from the pace that places the anchors"
        );
        assert_eq!(SPECTRUM_ANCHOR_AT[0], 0, "red is the arc's own start");
        assert_eq!(
            SPECTRUM_ANCHOR_AT[SPECTRUM_STOPS - 1],
            SPECTRUM_LUT_LEN - 1,
            "violet is the arc's own end"
        );
        assert!(
            SPECTRUM_ANCHOR_AT.windows(2).all(|w| w[0] < w[1]),
            "the seven names must land in ROYGBIV order: {SPECTRUM_ANCHOR_AT:?}"
        );
        for (i, &anchor) in SPECTRUM_ANCHORS.iter().enumerate() {
            assert_eq!(
                SPECTRUM_LUT[SPECTRUM_ANCHOR_AT[i]], anchor,
                "table index {} is not anchor {i}",
                SPECTRUM_ANCHOR_AT[i]
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

    /// The arc may follow the seven anchors' deliberately broad luminance span,
    /// but it must not introduce a deep interior dip between adjacent stops.
    #[test]
    fn spectrum_has_no_deep_interior_luminance_dip() {
        let lum: Vec<f64> = SPECTRUM_ANCHORS.iter().map(|&c| luminance(c)).collect();
        let mut worst_dip = 0.0f64;
        let mut dip_seg = 0usize;
        for seg in 0..SPECTRUM_STOPS - 1 {
            let floor = lum[seg].min(lum[seg + 1]);
            // THE INTERVAL IS THE ONE BETWEEN TWO NAMES, wherever the pace put
            // them — not a fixed count of entries.
            for &entry in SPECTRUM_LUT
                .iter()
                .take(SPECTRUM_ANCHOR_AT[seg + 1])
                .skip(SPECTRUM_ANCHOR_AT[seg] + 1)
            {
                let y = luminance(entry);
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
    /// **SATURATION IS EXACTLY 1, EVERYWHERE, AND THAT IS A PROPERTY OF THE
    /// PALETTE.** Every ROYGBIV interval joins two anchors that share a channel
    /// pinned at zero — red→orange→yellow all hold `B = 0`, yellow→green holds
    /// `B = 0`, green→blue holds `R = 0`, blue→indigo→violet hold `G = 0` — so a
    /// mix inside any interval keeps that channel at zero, and HSV saturation
    /// `(max − min) / max` is identically `1`.
    ///
    /// **THERE IS NO LONGER AN EXCEPTION, AND THAT IS THE 2026-09-15 DELETION.**
    /// The green→blue crossing's roof used to taper `S` to a floor of `0.53`
    /// across its pacing knots' span and back — the on-glass cyan true-peak
    /// bound of a census the owner retired on 2026-09-01. The roof, the taper
    /// and the crossing's exemption from the perceptual pace went together
    /// ([`SPECTRUM_CROSSING_SEG`]), and what replaced them is an HSV draw
    /// between the same two `S 1, V 1` anchors: the crossing's midpoint is
    /// `#00FFFF`, `S 1.000, V 1.000`, against the taper's `S 0.53, V 0.92` and
    /// the plain per-channel lerp's `V 0.51` grey-teal. The arc is now at FULL
    /// chroma end to end and at full value everywhere except the two anchors
    /// that author their own darkness.
    ///
    /// **VALUE IS THE ANCHORS' OWN, AND ITS FLOOR IS AN AUTHORED COLOUR.**
    /// `min V = 0.510` is indigo `#4B0082` itself — a named ROYGBIV stop, not a
    /// dip the arc wandered into — so the floor is asserted against the anchors
    /// rather than against a constant, and cannot be read as the wash coming
    /// back. Mean `V = 0.947` (it was `0.873` while the crossing sagged).
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

        // FULL CHROMA EVERYWHERE — exactly 1, entry for entry, with NO
        // exception zone since 2026-09-15. Five intervals get it from the
        // shared-zero argument above; the green→blue interval gets it from
        // being drawn in HSV between two anchors that are both `S 1`
        // ([`SPECTRUM_CROSSING_SEG`]), which is what retiring the crossing
        // roof's saturation taper bought. An anchor edit that broke the shared
        // zero, or a leg that reintroduced a taper, still fails here.
        for (i, &rgb) in SPECTRUM_LUT.iter().enumerate() {
            let (_, s, _) = spectrum_hsv(rgb);
            assert!(
                s >= 0.999,
                "entry {i} desaturates to S {s:.4} — the palette lost its \
                 shared zero channel, or a chroma taper came back"
            );
        }
        assert!(min_sat >= 0.999, "the arc's floor fell to S {min_sat:.4}");
        assert!(mean_sat >= 0.999, "mean saturation is only {mean_sat:.4}");

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
    /// visible staircase, and [`spectrum`] remains continuous between them.
    ///
    /// **ONE CEILING, EVERYWHERE, SINCE 2026-09-15.** This test used to allow a
    /// `16`-level chord across the crossing's exempt span and `5` everywhere
    /// else: the authored roof turned `48°` of hue in seven entries at a fixed
    /// `V` and `S`, and a wide turn at a fixed radius is a long chord. Deleting
    /// the roof and the pacing exemption ([`SPECTRUM_CROSSING_SEG`]) deletes the
    /// exception with them — the crossing is now spent by the same pace as every
    /// other leg, and the pace's byte currency
    /// ([`SPECTRUM_PACE_BYTE_SHARE`]) holds it to the ordinary bound. The
    /// exception zone was the last place the retired no-cyan ruling was still
    /// costing the table continuity.
    #[test]
    fn spectrum_lut_has_bounded_adjacent_steps() {
        // The one bound, for every adjacent pair of the table.
        const CEILING: i32 = 5;
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
            assert!(
                step <= CEILING,
                "table entries {i}..{} jump {step} levels (bound {CEILING})",
                i + 1
            );
            worst = worst.max(step);
        }
        assert_eq!(
            worst, 4,
            "the arc's exact steepest chord changed (the bound is {CEILING})"
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
}

#[cfg(test)]
mod min_saturation {
    //! The one chroma operation a producer may apply to its own read of the
    //! arc ([`spectrum_with_min_saturation`]) — hue and value are invariants,
    //! the floor is reached exactly, and nothing at or over it moves.
    use super::*;

    fn hsv(rgb: u32) -> (f64, f64, f64) {
        spectrum_hsv(rgb)
    }

    /// **WHAT IT DOES TO A COLOUR THAT IS UNDER THE FLOOR.**
    ///
    /// The witnesses used to be the eight `SPECTRUM_CROSSING_ROOF` samples,
    /// which were the only colours in this file under `S 0.60`. The roof is
    /// deleted and the arc no longer dips (see
    /// [`SPECTRUM_CROSSING_SEG`]), so the witnesses are stated directly: five
    /// desaturated colours spread round the hue circle, including the two the
    /// retired roof's own ends were.
    #[test]
    fn the_floor_is_reached_with_hue_and_value_untouched() {
        for (i, &under) in [
            0x006F_EBC2u32, // the retired roof's on-ramp, S 0.53
            0x006F_B1EB,    // …and its off-ramp, S 0.53
            0x00C0_8080,    // a washed red, S 0.33
            0x0080_C0A0,    // a washed green, S 0.33
            0x009A_9AD0,    // a washed indigo, S 0.26
        ]
        .iter()
        .enumerate()
        {
            let (h0, s0, v0) = hsv(under);
            assert!(
                s0 < 0.60,
                "witness {i} is not under S 0.60 ({s0:.3}) — this pin needs a colour the floor can move"
            );
            for floor in [0.65f32, 0.80, 0.90, 1.0] {
                let got = spectrum_with_min_saturation(under, floor);
                let (h1, s1, v1) = hsv(got);
                assert!(
                    (h1 - h0).abs() < 1.0,
                    "witness {i} at floor {floor}: hue moved {h0:.1}° → {h1:.1}° (#{got:06X})"
                );
                assert_eq!(
                    (v1 * 255.0).round() as u32,
                    (v0 * 255.0).round() as u32,
                    "witness {i} at floor {floor}: value moved"
                );
                assert!(
                    (s1 - f64::from(floor)).abs() <= 1.5 / 255.0 * 2.0,
                    "witness {i} at floor {floor}: S landed at {s1:.3}, not on the floor"
                );
            }
        }
        // One by hand: `#6FB1EB` = (111, 177, 235), spread 124, `k = 235 / 124`;
        // G lands at `235 − 58 · k = 125`, R at 0, B stays 235 — `(0, 125, 235)`,
        // still hue 208°.
        assert_eq!(spectrum_with_min_saturation(0x006F_B1EB, 1.0), 0x0000_7DEB);
    }

    #[test]
    fn a_colour_at_or_over_the_floor_and_every_grey_is_returned_bit_for_bit() {
        for &anchor in &SPECTRUM_ANCHORS {
            assert_eq!(
                spectrum_with_min_saturation(anchor, 1.0),
                anchor,
                "every anchor has a zero channel"
            );
        }
        for grey in [0u32, 0x0080_8080, 0x00FF_FFFF, 0x0011_1111] {
            assert_eq!(
                spectrum_with_min_saturation(grey, 1.0),
                grey,
                "a grey has no hue to keep"
            );
        }
        assert_eq!(
            spectrum_with_min_saturation(0x006F_EBC2, 0.50),
            0x006F_EBC2,
            "S 0.53 is over a 0.50 floor"
        );
        assert_eq!(
            spectrum_with_min_saturation(0x006F_EBC2, f32::NAN),
            0x006F_EBC2,
            "a non-finite floor is no floor"
        );
        assert_eq!(spectrum_with_min_saturation(0x006F_EBC2, 0.0), 0x006F_EBC2);
    }

    /// **THE FLOOR ASKS NOTHING OF THE ARC ANY MORE, AND THAT IS THE POINT.**
    ///
    /// This replaces `the_shipped_arc_dips_under_the_floor_only_inside_the_
    /// green_blue_interval`, whose control clause required the arc to DIP
    /// ("a table that no longer dips makes the floor a no-op and this pin
    /// vacuous"). The dip it was pinning was the retired crossing roof's
    /// saturation taper, which the 2026-09-01 ruling's deletion took with it
    /// ([`SPECTRUM_CROSSING_SEG`]), so the honest statement is the opposite
    /// one: a producer that applies the floor to its read of the arc gets the
    /// arc back, bit for bit, at every entry.
    #[test]
    fn the_floor_is_the_identity_on_every_entry_of_the_shipped_arc() {
        for (i, &c) in SPECTRUM_LUT.iter().enumerate() {
            assert_eq!(
                spectrum_with_min_saturation(c, 1.0),
                c,
                "entry {i} (#{c:06X}) is under the floor — the arc dips again"
            );
        }
    }
}
