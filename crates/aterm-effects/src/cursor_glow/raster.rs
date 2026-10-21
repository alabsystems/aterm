// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure raster emission: the colour rules (the rainbow spectrum, light-theme
//! ink, the fresh-ink glyph palette) and the quad/halo pushers. Every item is
//! a function of its arguments: no [`CursorGlow`] state, no clock.

use super::*;

/// The comet debris glint's coverage as a fraction of the grain's own (a
/// highlight on a grain of ice is dimmer than the grain): the dealt grain's
/// plus at its twinkle peak, laid over its round body on either ground (the
/// comet arm of `emit_particles`).
pub(super) const COMET_GLINT_COV: f32 = 0.5;

/// The seven names every point-mark of this family snaps to, red → violet
/// (`0x00RRGGBB`): canonical ROYGBIV in spectral order.
///
/// Each entry is [`crate::spectrum::spectrum_stop`]`(i)`, read from the same
/// table at compile time so it can stay a `const`.
///
/// Cyan is not a band (owner, 2026-08-27). Green and blue are adjacent stops on
/// a continuous ramp, so the arc crosses the cyan wedge between them at full
/// chroma and value (owner, 2026-09-15) without any authored cyan stripe or
/// neutralized handoff.
pub(super) const RAINBOW_BANDS: [u32; SPECTRUM_STOPS] = {
    let mut bands = [0u32; SPECTRUM_STOPS];
    let mut i = 0;
    while i < SPECTRUM_STOPS {
        bands[i] = crate::spectrum::SPECTRUM_LUT[crate::spectrum::SPECTRUM_ANCHOR_AT[i]];
        i += 1;
    }
    bands
};

// Additive-light budget over text (the legibility bound).
//
// The bed and the transients compose along different streams:
//
//   * the continuous bed rides `glow_under` (beneath the glyph pass), so it
//     lifts only the ground: contrast is (unlit ink) vs (lit ground);
//   * the transients ride `glow_halo` / `cursor_glow_add` on top, lifting ink
//     and ground together; once the ink saturates (`#C8D3F5` has 55/44/10
//     per-channel headroom) the ground keeps climbing and contrast collapses.
//
// Each cap is derived against the 5.25:1 bar for its own stream, so caps are
// not interchangeable across streams. The beam family's streak on a dark
// ground lies under the letters as its bed (`CursorGlow::emit_dark_bed`,
// solved to the 4.5:1 bar), and the lights that still land on a letter there
// spend one over-ink account per frame (`InkLedger`).

/// The transient over-ink coverage cap: the fresh-ink pop, crown, starfield,
/// and the rainbow cursor's halo and twinkle. Solved, with the bed at 34 under
/// it, against the arc's brightest over-ink colour (the mint `#80FFD3` at
/// t 0.576) to hold the 5.25:1 bar.
pub(crate) const OVER_INK_COV_CAP: f32 = 47.0;

/// The caret is the brightest thing in the effect: the least WCAG relative
/// luminance the block cursor's fill may carry while the effect is live on a
/// dark page, on a `0..=255` scale. A light page has no such floor: the page
/// is the brightest thing there, so the caret keeps its saturated hue.
///
/// A saturated violet is `Y = 32/255` and a white dot `255`, so neither side
/// alone can win: the caret takes a floor and the sparkle field a ceiling
/// derived from it. `80` is the least floor that leaves the field alive: it
/// lifts 23-30 % of the arc (the red/orange and violet ends) at a cost of at
/// most 0.34 saturation. A higher floor would pale most of the arc and turn
/// the caret's red salmon.
pub(crate) const RAINBOW_CARET_LIGHT_FLOOR: f32 = 80.0;

/// The share of [`RAINBOW_CARET_LIGHT_FLOOR`] the sparkle field is allowed.
///
/// `0.9` leaves eight luminance levels between the brightest sparkle and the
/// dimmest caret, more than either's `f32 -> u8` can move.
pub(crate) const RAINBOW_SPARKLE_LIGHT_SHARE: f32 = 0.9;

/// The sparkle field's own ceiling in additive coverage: what a starfield
/// point-mark may ask for so its composited centre stays under
/// [`RAINBOW_CARET_LIGHT_FLOOR`].
///
/// A mote's composited centre carries [`crate::effect_util::STAR_STACK_ADD`]
/// `· cov`, which must sit at or under the sRGB level whose relative luminance
/// is `RAINBOW_CARET_LIGHT_FLOOR · RAINBOW_SPARKLE_LIGHT_SHARE` (`Y = 72`,
/// level `145`): `145 / 2.35 = 61`. Tighter than the `OVER_INK_COV_CAP · 1.5`
/// legibility bound, which also applies.
pub(super) const RAINBOW_SPARKLE_COV_MAX: f32 = 61.0;

/// The sparkle ceiling must be the tighter of the two bounds; a ceiling above
/// what the emitter can already ask for would be dead code.
const _: () = assert!(
    RAINBOW_SPARKLE_COV_MAX < OVER_INK_COV_CAP * 1.5,
    "the sparkle ceiling must bind before the legibility clamp"
);

// The composition rule: every ceiling above bounds one layer, but the renderer
// composites every stream with `add_sat` onto the same pixels, so individually
// legal layers can sum past 255 and clip. Clipping is a shape bug: the mark
// grows a flat plateau, loses its falloff, and a field at 255 cannot sit under
// a caret. `RAINBOW_FIELD_LEVEL` is the ceiling on the sum.

/// The field's ceiling: the most light every over-ink stream together may put
/// on one pixel outside the caret, in levels of one channel, measured over the
/// ground. The same level [`RAINBOW_SPARKLE_COV_MAX`] solves for one mote
/// (`145`), held on the sum, since the brightest pixel is where several land.
pub(crate) const RAINBOW_FIELD_LEVEL: f32 = 145.0;

/// The field level is the level [`RAINBOW_SPARKLE_COV_MAX`] was solved against.
const _: () = assert!(RAINBOW_FIELD_LEVEL > 144.0 && RAINBOW_FIELD_LEVEL < 146.0);

/// Drift floor of the jump streak's chroma fold, in `t` (the arc's parameter):
/// a third of the cycle, so it tracks the lay rate. The streak's stack is about
/// 1.6 cells thick and the field has a slope, so same-row taps always read some
/// drift; a tighter floor greys the streak along a row. Only the build check
/// below reads it.
pub(super) const RAINBOW_STREAK_DRIFT_FLOOR: f32 = RAINBOW_CYCLE / 3.0;
/// Where the fold is complete: an uncorrelated reading, which across a row
/// boundary averages `0.5` of the arc.
pub(super) const RAINBOW_STREAK_DRIFT_MAX: f32 = 0.5;

// The floor admits the stack's own thickness and leaves the fold room to run;
// a floor at or above the max would switch it off.
const _: () = assert!(
    RAINBOW_STREAK_DRIFT_FLOOR >= 1.0 / RAINBOW_TRAVERSE_SETTLED
        && RAINBOW_STREAK_DRIFT_FLOOR < RAINBOW_STREAK_DRIFT_MAX
        && RAINBOW_STREAK_DRIFT_MAX <= RAINBOW_CYCLE * 0.5,
    "the streak's drift floor must admit one cell's lay and leave the fold room, \
     and the fold must complete no later than the farthest two readings can be"
);

// The ribbon's transverse profile lives in the renderer
// (`aterm_render::ribbon_profile`): the quads are rasterized host-side, so
// CPU/GPU parity is structural.

// Light-theme ink: every source-over mark the rainbow kitty draws on a white
// ground darkens through the constants below.
/// Ceiling on the luma (0..255, Rec. 709) of any hue composited source-over on
/// a white ground.
///
/// A flat mix toward black is the wrong operator: pure yellow's luma is 226 and
/// pure blue's 32, so one ratio leaves yellow tinting the paper while violet
/// stains it. Normalizing to a luma target darkens each band by what it needs.
/// The bar is set by the worst band (the warm middle), or the rainbow thins out
/// in its middle on white.
pub(super) const LIGHT_INK_MAX_LUMA: f32 = 68.0;
/// Companion ceiling on any single channel. Pure red's luma is only 54, so a
/// luma bar alone leaves it at `FF`, and the fresh-ink veil (composited over
/// the glyph) would tint near-black ink dark red.
pub(super) const LIGHT_INK_MAX_CHANNEL: f32 = 150.0;
/// Legibility ceiling on any light-theme mark's per-pixel centre over-alpha
/// (`aterm_render::halo_over_cap`): `over_rgb(glyph, ink, cap)` keeps
/// `(255−cap)/255` of the glyph, so near-black ink stays near-black and a white
/// counter stays light grey. Pinned by `light_ink_cap_keeps_glyphs_legible`.
pub(crate) const LIGHT_INK_ALPHA_CAP: f32 = 190.0;
/// Ceiling on the opacity of any body drawn under the letters on a light
/// ground (the beam family's bodies, every crown, the comet's coma, the
/// rod's sleeve, the typing glow): the full byte. Legibility is the ink's,
/// never this cap's: a body's ink is solved so that even laid opaque, and so
/// however many bodies stack on one pixel, the theme's text keeps the bar
/// over it (`light_body_ink`).
pub(crate) const LIGHT_BODY_ALPHA_CAP: f32 = 255.0;

/// Rec. 709 luma of a `0x00RRGGBB` colour, 0..255. The one luminance the light
/// arms measure with.
#[inline]
pub(super) fn luma709(rgb: u32) -> f32 {
    0.2126 * ((rgb >> 16) & 0xff) as f32
        + 0.7152 * ((rgb >> 8) & 0xff) as f32
        + 0.0722 * (rgb & 0xff) as f32
}

// The one light-ink rule: a light ground has two darkening policies, chosen by
// a mark's compositing role ([`InkRole`]), never by which emitter draws it, and
// never mixed inside one event (bold ink is about 1.8x the weight of
// conservative ink, so a mixed landing reads as half washed out):
//
//   * [`light_ink_bold`], leading-only: marks that land in the inter-line
//     leading and never composite over a glyph (the rail, the rail-riding
//     streak, the meteor nucleus, the landing bloom, the fresh-ink bar's
//     tint), and marks drawn beneath the glyph ink (water's wake, light
//     lightning, the beam family's light bodies and Lumen's light glow).
//     Vivid, because nothing they darken is ink.
//   * [`light_ink`], may-touch-text: every star, sparkle and dot
//     ([`push_twinkle_over`] applies it internally), the erase-poof grains and
//     the cursor cat's exit flourish. Conservative.
//
// Bold ink is about 1.8x the weight of conservative ink, so mixing them inside
// one landing reads as half the event washed out. Each call site names its
// role (`InkRole`); no test pins the split itself (the test this comment once
// named was never written).
//
// The rule is for marks that can composite over a glyph. On a light ground
// the harness card's marks never do: its trail, rim, stars, sparkles and
// burst lie on cells the host read blank (the stars are clipped to that
// region, `star_region`, which on a light ground stops short of the border's
// column, where a glyph may stand), and its border is drawn under glyph ink.
// So on a light ground they are opaque card art in `harness_card`'s print
// inks (its "the light ground: print inks" and "one printed piece"), as its
// kitty is; on dark they are added light, which keeps any letter under it
// readable. Its line is text: its letters take the paper law's point-mark
// ink (`rainbow_kitty::ribbon`'s `paper_mark_ink`, WCAG AA against the page)
// rather than this rule, as does the composer rule's wordmark under the card
// (`claude_footer.rs`'s `brand_letter_fg`). Every other mark keeps the rule.

/// Leading-only ink ([`light_ink_bold`]): one ink weight for every hue, so a
/// sweep does not pulse light and dark, at the most saturation that weight
/// allows. Hues whose pure luma is above the bar are scaled down; red, blue and
/// violet reach it only by letting some white back in.
pub(super) const LIGHT_INK_BOLD_MAX_LUMA: f32 = 120.0;
pub(super) const LIGHT_INK_BOLD_MAX_CHANNEL: f32 = 215.0;

/// [`light_ink`]'s vivid twin (see the one light-ink rule above). Normalizes
/// `rgb` to its pure hue (achromatic floor removed, peak renormalized to 1),
/// then re-mixes it at the `(S, V)` that lands exactly on
/// [`LIGHT_INK_BOLD_MAX_LUMA`] with as much saturation as
/// [`LIGHT_INK_BOLD_MAX_CHANNEL`] leaves. Channel order is preserved. Pinned by
/// `light_ink_bold_holds_one_ink_weight_across_the_spectrum`.
#[inline]
pub(super) fn light_ink_bold(rgb: u32) -> u32 {
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as f32,
        ((rgb >> 8) & 0xff) as f32,
        (rgb & 0xff) as f32,
    );
    let hi = r.max(g).max(b);
    let lo = r.min(g).min(b);
    if hi <= 0.5 {
        return 0; // a black input carries no hue to weigh
    }
    // The pure hue: `lo` is the white this colour carries; removing it and
    // renormalizing gives the same hue at S = 1, V = 1. An achromatic input
    // has zero span and no hue to saturate, so it keeps its ratios (all 1)
    // and takes the bar as a neutral grey rather than turning black.
    let span = hi - lo;
    let pure = |c: f32| {
        if span <= 1e-3 { 1.0 } else { (c - lo) / span }
    };
    let (pr, pg, pb) = (pure(r), pure(g), pure(b));
    // The pure hue's own luma, 0..1: why one scale factor cannot reach the
    // bar for red (0.21) or violet (0.13).
    let y0 = 0.2126 * pr + 0.7152 * pg + 0.0722 * pb;
    let yt = LIGHT_INK_BOLD_MAX_LUMA / 255.0;
    let vmax = LIGHT_INK_BOLD_MAX_CHANNEL / 255.0;
    // At value `v` and saturation `s` a channel is `v·(1 − s + s·c)` and the
    // luma `v·(1 − s + s·y0)`. Luma falls with `s` (for `y0 < 1`), so the most
    // saturated solution takes `v` as high as the channel bar allows and then
    // solves for `s`; `s ≥ 1` means scaling alone reaches the bar.
    let s = if y0 >= 1.0 - 1e-4 {
        0.0 // achromatic: nothing to desaturate
    } else {
        ((1.0 - yt / vmax) / (1.0 - y0)).clamp(0.0, 1.0)
    };
    let v = (yt / (1.0 - s + s * y0)).min(vmax);
    let ch = |c: f32| ((255.0 * v * (1.0 - s + s * c)) + 0.5).clamp(0.0, 255.0) as u32;
    (ch(pr) << 16) | (ch(pg) << 8) | ch(pb)
}

/// Light-theme ink: `rgb` scaled uniformly (hue and saturation intact) until
/// it satisfies both [`LIGHT_INK_MAX_LUMA`] and [`LIGHT_INK_MAX_CHANNEL`]. A
/// colour already under both is returned unchanged. Pinned by
/// `light_ink_darkens_every_band_to_one_bar`.
#[inline]
pub(super) fn light_ink(rgb: u32) -> u32 {
    let rgb = rgb & 0x00FF_FFFF;
    let y = luma709(rgb);
    let hi = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff) as f32;
    let k = (LIGHT_INK_MAX_LUMA / y.max(1.0))
        .min(LIGHT_INK_MAX_CHANNEL / hi.max(1.0))
        .min(1.0);
    if k >= 1.0 {
        return rgb;
    }
    let ch = |sh: u32| ((((rgb >> sh) & 0xff) as f32 * k) + 0.5) as u32;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The compositing role of one light-theme mark (see the one light-ink rule).
/// It picks both halves of the recipe, the darkening policy and the centre
/// over-alpha ceiling, so the two cannot disagree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InkRole {
    /// Provably confined to the inter-line leading (the rail line), or drawn
    /// beneath the glyph ink (the under-ink stream: water's light wake, light
    /// lightning, the beam family's light bodies and Lumen's light glow):
    /// nothing it darkens is ever a glyph's stroke, so it may be
    /// vivid. Its centre ceiling is the rail's; an under-ink emitter caps its
    /// own opacity lower where its ground must keep a dark letter legible.
    Leading,
    /// Can composite over a letterform at any moment: the conservative ink and
    /// the shared legibility ceiling.
    OverText,
}

impl InkRole {
    /// This role's darkening policy.
    #[inline]
    pub(crate) fn ink(self, rgb: u32) -> u32 {
        match self {
            Self::Leading => light_ink_bold(rgb),
            Self::OverText => light_ink(rgb),
        }
    }

    /// This role's per-pixel centre over-alpha ceiling.
    #[inline]
    pub(crate) fn alpha_cap(self) -> f32 {
        match self {
            Self::Leading => RAINBOW_LIGHT_RAIL_ALPHA_CAP,
            Self::OverText => LIGHT_INK_ALPHA_CAP,
        }
    }
}

// ---- rainbow kitty light-theme rails ----------------------------------------
/// The one sweep: where a cell sits on the spectrum given its column and the
/// ribbon's phase clock. Every mark on the light rail line reads it (rail,
/// fresh-ink bar, glyph tint, and the [`crate::cursor_rainbow`] block cursor at
/// its own column), so they are the same colour.
///
/// It ping-pongs: the seven bands are an acyclic ramp, so wrapping would print
/// a violet-to-red seam every `1 / SPREAD` ≈ 22 columns. A triangle wave runs
/// red→violet→red with no discontinuity.
#[inline]
pub(crate) fn rainbow_sweep_at(col: u16, phase: f32) -> f32 {
    rainbow_sweep_reflect(col as f32 * RAINBOW_LIGHT_RAIL_SPREAD + rainbow_sweep_phase(phase))
}

/// Fold an unbounded spectrum position onto the sweep's ping-pong `0..1`, for
/// callers that offset a resolved position (the caret's halo rings step
/// outward along the wheel).
#[inline]
pub(crate) fn rainbow_sweep_reflect(x: f32) -> f32 {
    let raw = x.rem_euclid(2.0);
    if raw <= 1.0 { raw } else { 2.0 - raw }
}

/// Resolve a reflected spectrum position for a persistent mark — the caret's
/// read of the same field the ribbon paints, past the cycle's turnaround.
#[inline]
pub(crate) fn rainbow_thing_of(sweep: f32) -> u32 {
    // The caret wears the cycle, not the open arc: the field walks past violet,
    // and clamping would park the caret while the band behind it advances.
    // Identity on `[0, 1]`.
    let phase = rainbow_cycle_phase(sweep);
    if phase > 1.0 {
        return rainbow_cycle_ink(phase);
    }
    spectrum(phase)
}

/// Re-pacing of the sweep's ends, off (`0.0`): the fold is the pure triangle,
/// so every keystroke advances the arc at the mark's own rate. A positive `a`
/// would apply `x − a·sin(2πx)/2π`, lingering at the arc's ends (which reads as
/// monochrome); the bounds below are parametric on it.
pub(super) const RAINBOW_END_DWELL: f32 = 0.0;

// ---- the one visible field --------------------------------------------------
//
// Every layer reads a spark's cached, run-local classic position, so a run
// keeps one red→violet geometry across its live cells. The rolling hue,
// absolute-column sweep and hashes are ancillary clocks, never colour rules
// for a live kitty mark.

/// The named stop nearest a sweep position ([`spectrum_snap`]), the quantized
/// spectrum every point-mark of this family snaps to, so red and violet stay
/// nameable on a thin underline.
#[inline]
#[cfg(test)]
pub(crate) fn rainbow_band_of(sweep: f32) -> u32 {
    spectrum_snap(sweep)
}

/// [`rainbow_band_of`] at a column and phase (test-only: live kitty marks read
/// the cached classic field).
#[cfg(test)]
#[inline]
pub(crate) fn rainbow_band_at(col: u16, phase: f32) -> u32 {
    rainbow_band_of(rainbow_sweep_at(col, phase))
}

/// Legibility ceiling on a rail's per-pixel centre over-alpha. The rail lives
/// wholly in the leading below the row (`DY − RY` = 0.49, clear of any
/// descender), so this bounds how dark the leading gets (and how two rails
/// compound), not the ink; hence it may exceed [`LIGHT_INK_ALPHA_CAP`].
pub(super) const RAINBOW_LIGHT_RAIL_ALPHA_CAP: f32 = 236.0;

/// How fast the band ramp sweeps along the rails, per column and per unit of
/// the ribbon's phase clock. Phase-driven, so it freezes when the ribbon does.
pub(super) const RAINBOW_LIGHT_RAIL_SPREAD: f32 = 0.045;
/// The integrated rainbow clock lives on this exact ring. Every consumer must
/// complete a whole number of its own cycles across the ring so wrapping can
/// never change a visible frame.
pub(crate) const RAINBOW_PHASE_RING: f32 = 1024.0;
/// The sweep's rate per unit of phase: 179 whole reflected (period-two) sweeps
/// per [`RAINBOW_PHASE_RING`], so the ring's wrap is exact.
pub(super) const RAINBOW_LIGHT_RAIL_FLOW: f32 = 179.0 / 512.0;

/// Spectrum-clock position on the reflected sweep's period-two domain.
#[inline]
pub(super) fn rainbow_sweep_phase(phase: f32) -> f32 {
    (phase.rem_euclid(RAINBOW_PHASE_RING) * RAINBOW_LIGHT_RAIL_FLOW).rem_euclid(2.0)
}

/// Lift a standalone unit-turn animation clock onto the rainbow phase ring so
/// one `0..1` turn traverses one whole reflected spectrum cycle, turns 0 and 1
/// both at sweep position zero. Passed unchanged, a unit clock would cover only
/// [`RAINBOW_LIGHT_RAIL_FLOW`] of the sweep and jump back on wrap.
#[inline]
pub(crate) fn rainbow_phase_from_unit_turn(turn: f32) -> f32 {
    ((turn.rem_euclid(1.0) * 2.0) / RAINBOW_LIGHT_RAIL_FLOW).rem_euclid(RAINBOW_PHASE_RING)
}

/// Adjacency bound for the ancillary laid-hue lattice, kept for its
/// compile-time checks below; the live kitty field is bounded by the classic
/// geometry.
pub(super) const RAINBOW_UNDERLINE_SWEEP_MAX: f32 = 0.22;

/// Reference step for the ancillary rainbow-kitty rolling hue clock. It
/// sequences per-spawn hue data, not the visible ribbon's spatial rate
/// (classic geometry supplies that).
pub(super) const RAINBOW_KITTY_HUE_STEP: f32 = 0.0125;

/// The shortest traverse, in cells. The per-cell hue step is `1 / T` of the
/// arc, so a tiny `T` would print band edges. At `26` the worst per-cell step
/// is `55°`, about `3.6°` per device pixel; [`RAINBOW_UNDERLINE_SWEEP_MAX`] is
/// checked against it below.
pub(super) const RAINBOW_TRAVERSE_MIN_CELLS: f32 = 26.0;

/// The short mark's traverse, in cells: `1/16` of the arc per cell, so a
/// five-char word's last cell reaches yellow (`4/16` → 45°) at `17.6°` per key,
/// the per-cell lay stays under the adjacency ceiling (checked below), and a
/// 4-key line's head (`3/16`) stays clear of the green-blue crossing.
pub(super) const RAINBOW_SHORT_TRAVERSE_CELLS: f32 = 16.0;

// The short walk's step obeys the same adjacency rule: one typed cell never
// lays more than [`RAINBOW_UNDERLINE_SWEEP_MAX`] of the spectrum, end
// re-pacing included.
const _: () = assert!(
    (1.0 + RAINBOW_END_DWELL) / RAINBOW_SHORT_TRAVERSE_CELLS + RAINBOW_LAID_END_PULL
        <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "the short walk's step must stay under the adjacency ceiling"
);

/// The settled traverse, in cells: what one red→violet pass costs once a line
/// is long. A fixed length, not a function of the mark, so a long mark's head
/// never pins at violet. `36` gives `7.83°` of hue per keystroke: a named colour
/// every 5.1 keys, a full pass in 3.2-4.0 s at 90-110 ms cadence.
pub(super) const RAINBOW_TRAVERSE_SETTLED: f32 = 36.0;

/// The wrap in cells: violet back to red, the price of a head that keeps
/// advancing. The short way from violet (`282°`) to red runs through magenta
/// (`78°`, `G = 0`, never near cyan); 2 cells keeps it no faster than the
/// green→blue crossing (`1420/36 = 39.4°` per cell).
pub(super) const RAINBOW_WRAP_CELLS: f32 = 2.0;

/// The wrap as a share of the arc (the unit the walk is stated in), so it
/// keeps the crossing's rate at every local rate the walk takes.
pub(super) const RAINBOW_WRAP_SHARE: f32 = RAINBOW_WRAP_CELLS / RAINBOW_TRAVERSE_SETTLED;

/// One full turn of the spectrum in arc units: the red→violet arc (`1.0`) plus
/// the wrap that closes it. [`rainbow_cycle_phase`] folds by this.
pub(super) const RAINBOW_CYCLE: f32 = 1.0 + RAINBOW_WRAP_SHARE;

// The walk never parks: its rate is bounded below by the settled traverse,
// so one more typed cell always moves the head.
const _: () = assert!(
    RAINBOW_TRAVERSE_SETTLED > 0.0 && RAINBOW_TRAVERSE_SETTLED.is_finite(),
    "the settled traverse is the walk's lower bound on advance; it may not be zero"
);
// The wrap is no faster than the crossing: the crossing spends `1420/T` per
// cell, the wrap `78/W`.
const _: () = assert!(
    78.0 / RAINBOW_WRAP_CELLS <= 1420.0 / RAINBOW_TRAVERSE_SETTLED,
    "the wrap may not move faster than the green-blue crossing it is paced against"
);

/// The walk folded into one turn of the spectrum: `[0, 1]` is the red→violet
/// arc, `(1, `[`RAINBOW_CYCLE`]`)` is the wrap that closes it.
#[inline]
pub(super) fn rainbow_cycle_phase(p: f32) -> f32 {
    // Fast path: most callers pass a value already inside one turn, and the
    // hot ribbon calls this per slab per vertex, so avoid `rem_euclid` (a
    // library `fmod`) there.
    if (0.0..RAINBOW_CYCLE).contains(&p) {
        return p;
    }
    if !p.is_finite() {
        return 0.0;
    }
    let folded = p - RAINBOW_CYCLE * (p * RAINBOW_CYCLE_INV).floor();
    // Rounding can land exactly on the period (the domain is half-open), and
    // an overflow to `-inf` lands outside it; both fold to zero.
    if !(0.0..RAINBOW_CYCLE).contains(&folded) {
        return 0.0;
    }
    folded
}

/// Reciprocal of [`RAINBOW_CYCLE`], so the fold is a multiply.
pub(super) const RAINBOW_CYCLE_INV: f32 = 1.0 / RAINBOW_CYCLE;

/// The cycle's colour: `spectrum` on the arc, and past it the wrap leg, violet
/// to red the short way through magenta. Both joins are exact (the anchors are
/// stored verbatim, [`crate::spectrum::SPECTRUM_ANCHOR_AT`]); the leg holds
/// `G = 0`, so it is fully saturated and never enters the cyan window.
#[inline]
pub(super) fn rainbow_cycle_ink(p: f32) -> u32 {
    let phase = rainbow_cycle_phase(p);
    if phase <= 1.0 {
        spectrum(phase)
    } else {
        crate::effect_util::lerp_rgb(
            crate::spectrum::SPECTRUM_ANCHORS[SPECTRUM_STOPS - 1],
            crate::spectrum::SPECTRUM_ANCHORS[0],
            ((phase - 1.0) / RAINBOW_WRAP_SHARE).clamp(0.0, 1.0),
        )
    }
}

/// The longest traverse, in cells: past this a long mark carries more than one
/// traverse rather than an arc so thin that a word is one colour. Sets the
/// slowest lay ([`RAINBOW_LAID_SWEEP_MIN_PER_CELL`]).
pub(super) const RAINBOW_TRAVERSE_MAX_CELLS: f32 = 144.0;

/// Phaser's hue step, in turns per typed cell: fast enough that the fat beam
/// visibly sweeps the spectrum.
pub(super) const RAINBOW_PHASER_HUE_STEP: f32 = 0.14;

/// The rolling hue step for the non-kitty styles (sparkle, lumen, fire, laser,
/// beam, water, comet): how fast the colour a grain or wake is dealt turns as
/// you type.
pub(super) const RAINBOW_ROLL_HUE_STEP: f32 = 0.07;

/// The lay rate of the ancillary lattice, in fractions of one red→violet
/// traverse per typed cell, at the reference 40-cell traverse. Bounds are
/// taken against [`RAINBOW_LAID_SWEEP_MAX_PER_CELL`] (fastest lay) and
/// [`RAINBOW_LAID_SWEEP_MIN_PER_CELL`] (slowest).
pub(super) const RAINBOW_LAID_SWEEP_PER_CELL: f32 = RAINBOW_KITTY_HUE_STEP * RAINBOW_LAID_HUE_SWEEP;

/// The fastest one keystroke can lay, in fractions of a traverse: the
/// shortest traverse ([`RAINBOW_TRAVERSE_MIN_CELLS`]) at the steepest point of
/// the end re-pacing. Every adjacency bound is taken against this.
pub(super) const RAINBOW_LAID_SWEEP_MAX_PER_CELL: f32 =
    (1.0 + RAINBOW_END_DWELL) / RAINBOW_TRAVERSE_MIN_CELLS;

/// The slowest lay: the longest traverse at the re-pacing's shallowest point
/// (the arc's ends).
pub(super) const RAINBOW_LAID_SWEEP_MIN_PER_CELL: f32 =
    (1.0 - RAINBOW_END_DWELL) / RAINBOW_TRAVERSE_MAX_CELLS;

/// Width of the end-pull's landing, strictly inside
/// [`RAINBOW_LAID_SWEEP_MIN_PER_CELL`], so at most one cell per turnaround
/// resolves to the exact anchor. Checked at compile time below.
pub(super) const RAINBOW_LAID_END_PULL: f32 = 0.003;

/// How close to an end the lattice can land without being on it: half of one
/// cell's lay at the fastest lay ([`RAINBOW_TRAVERSE_MIN_CELLS`]), compressed
/// by `1 - a` under end dwell. The lattice straddles the turnaround at `t = 1`,
/// so this is what any claim about landing on an anchor may be off by.
pub(super) const RAINBOW_LAID_END_REACH: f32 =
    (1.0 - RAINBOW_END_DWELL) * 0.5 / RAINBOW_TRAVERSE_MIN_CELLS;

/// Turns of laid hue → spectrum position: one turn of the rolling hue paints
/// one complete red→violet→red ping-pong.
///
/// Must be an even integer: [`rainbow_sweep_reflect`] has period 2 and the hue
/// is kept in `0..1`, so only an even gain agrees across the wrap (0.99 and
/// 0.01 both resolve near red instead of printing a magenta seam). At 4 the
/// per-cell step would break [`RAINBOW_UNDERLINE_SWEEP_MAX`], so 2 is the only
/// value that satisfies both.
pub(super) const RAINBOW_LAID_HUE_SWEEP: f32 = 2.0;

/// The adjacency bound: no retune of step, gain, clamp or dwell may push
/// neighbouring cells further apart than [`RAINBOW_UNDERLINE_SWEEP_MAX`] (red
/// beside violet), taken at the fastest lay.
const _: () = assert!(
    RAINBOW_LAID_SWEEP_MAX_PER_CELL + RAINBOW_LAID_END_PULL <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "one typed cell must step at most RAINBOW_UNDERLINE_SWEEP_MAX of the spectrum"
);

/// The end-pull's landing is narrower than the slowest lay
/// ([`RAINBOW_LAID_SWEEP_MIN_PER_CELL`]), or two consecutive keystrokes could
/// lay the same field.
const _: () = assert!(
    RAINBOW_LAID_END_PULL < RAINBOW_LAID_SWEEP_MIN_PER_CELL,
    "the end-pull's landing must be narrower than one lay at the slowest rate"
);

/// The reference pace [`RAINBOW_LAID_SWEEP_PER_CELL`] must be a pace the rate
/// rule can resolve to for some mark length, and the landing error plus one
/// fastest lay must stay inside the adjacency ceiling.
const _: () = assert!(
    RAINBOW_LAID_SWEEP_MIN_PER_CELL <= RAINBOW_LAID_SWEEP_PER_CELL
        && RAINBOW_LAID_SWEEP_PER_CELL <= RAINBOW_LAID_SWEEP_MAX_PER_CELL
        && RAINBOW_LAID_END_REACH + RAINBOW_LAID_SWEEP_MAX_PER_CELL <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "the reference pace must be reachable and its landing error inside the ceiling"
);

// ---- the fresh-ink glyph tint -----------------------------------------------

/// The contrast ratio the tint aims for against the theme's own ground: WCAG
/// AA for small text.
pub(super) const FRESH_INK_GLYPH_TARGET_RATIO: f32 = 4.5;
/// Below this ratio the theme has no usable side — an equiluminant or
/// near-invisible fg/bg pair — and the tint suppresses itself entirely rather
/// than inventing a side the theme does not have.
pub(super) const FRESH_INK_GLYPH_MIN_THEME_RATIO: f32 = 1.5;
/// The ink must be a hue (max channel − min channel) and must differ visibly
/// from the foreground it replaces, or the substitution buys nothing.
pub(super) const FRESH_INK_GLYPH_MIN_SPREAD: i32 = 24;
pub(super) const FRESH_INK_GLYPH_MIN_DELTA: i32 = 24;
/// Bisection depth on the one-parameter ink family. 24 halvings resolve `k` far
/// below the 1/255 quantization step, so the result is exact in u8 terms.
pub(super) const FRESH_INK_GLYPH_BISECT: usize = 24;

/// WCAG relative luminance of a `0x00RRGGBB` colour, with the piecewise sRGB
/// transfer (linear toe below 0.03928, 2.4 power above); a plain power law
/// differs enough near black to move a ratio across the AA line.
#[inline]
pub(super) fn rel_luminance(c: u32) -> f32 {
    crate::color_math::relative_luminance(c)
}

/// WCAG contrast ratio between two `0x00RRGGBB` colours.
#[inline]
pub(super) fn wcag_ratio(a: u32, b: u32) -> f32 {
    let (x, y) = (rel_luminance(a), rel_luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Per-channel max / min of two colours, repacked: the corners of the box the
/// whole tint path lives in (see [`glyph_ink_bound`]).
#[inline]
pub(super) fn chan_max(a: u32, b: u32) -> u32 {
    let m = |sh: u32| ((a >> sh) & 0xff).max((b >> sh) & 0xff);
    (m(16) << 16) | (m(8) << 8) | m(0)
}
#[inline]
pub(super) fn chan_min(a: u32, b: u32) -> u32 {
    let m = |sh: u32| ((a >> sh) & 0xff).min((b >> sh) & 0xff);
    (m(16) << 16) | (m(8) << 8) | m(0)
}
/// Chroma presence: how far apart this colour's extreme channels are.
#[inline]
pub(super) fn chan_spread(c: u32) -> i32 {
    let (r, g, b) = (
        ((c >> 16) & 0xff) as i32,
        ((c >> 8) & 0xff) as i32,
        (c & 0xff) as i32,
    );
    r.max(g).max(b) - r.min(g).min(b)
}
/// Largest per-channel difference — "is this visibly a different colour".
#[inline]
pub(super) fn chan_dist(a: u32, b: u32) -> i32 {
    let d = |sh: u32| (((a >> sh) & 0xff) as i32 - ((b >> sh) & 0xff) as i32).abs();
    d(16).max(d(8)).max(d(0))
}

/// Which side of the ground the theme's own ink sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum GlyphInkSide {
    /// Dark ink on a light ground: the tint mixes down toward black.
    Dark,
    /// Light ink on a dark ground: the tint mixes up toward white.
    Light,
}

/// The side the theme's ink is on and the luminance every point of the tint
/// path must satisfy, or `None` when this theme gets no tint.
///
/// A luminance, not a per-point ratio: the tint is a per-channel lerp from the
/// foreground to the band ink and `rel_luminance` is monotone per channel, so
/// the box corner ([`chan_max`] dark side, [`chan_min`] light side) bounds the
/// whole path. The bound is `max(cap_AA, L(fg))`, not a ratio round-trip: when
/// the theme's own text is below AA (Solarized Light's 4.13:1) the round-trip
/// lands an ulp below `L(fg)` and makes every candidate infeasible.
#[inline]
pub(super) fn glyph_ink_bound(fg: u32, bg: u32) -> Option<(GlyphInkSide, f32)> {
    // A sentinel (or any set high byte) is not a colour: masking it would read
    // as pure black and flip the side.
    if fg > 0x00FF_FFFF || bg > 0x00FF_FFFF || fg == bg {
        return None;
    }
    let theme = wcag_ratio(fg, bg);
    if !theme.is_finite() || theme < FRESH_INK_GLYPH_MIN_THEME_RATIO {
        return None;
    }
    let (lfg, lbg) = (rel_luminance(fg), rel_luminance(bg));
    if lfg < lbg {
        let aa = (lbg + 0.05) / FRESH_INK_GLYPH_TARGET_RATIO - 0.05;
        Some((GlyphInkSide::Dark, aa.max(lfg)))
    } else {
        let aa = FRESH_INK_GLYPH_TARGET_RATIO * (lbg + 0.05) - 0.05;
        Some((GlyphInkSide::Light, aa.min(lfg)))
    }
}

/// One band as glyph ink for this theme: saturated, then bisected along a
/// one-parameter family toward black (dark side) or white (light side) until
/// the box corner against the foreground satisfies the bound. The family is
/// per-channel monotone in `k` even after quantization, so bisection on the
/// quantized candidate is exact. `None` when the band is achromatic or even
/// the family's extreme fails.
#[inline]
pub(super) fn fresh_ink_glyph_ink(band: u32, fg: u32, side: GlyphInkSide, cap: f32) -> Option<u32> {
    let (r, g, b) = (
        ((band >> 16) & 0xff) as f32,
        ((band >> 8) & 0xff) as f32,
        (band & 0xff) as f32,
    );
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    if hi <= 0.5 {
        return None;
    }
    let span = (hi - lo).max(1e-3);
    let sat = |c: f32| ((c - lo) / span * hi).clamp(0.0, 255.0);
    let s = [sat(r), sat(g), sat(b)];
    // k = 0 is the safe extreme (black / white), k = 1 the most chromatic.
    let cand = |k: f32| -> u32 {
        let ch = |i: usize| -> u32 {
            let v = match side {
                GlyphInkSide::Dark => s[i] * k,
                GlyphInkSide::Light => s[i] + (255.0 - s[i]) * (1.0 - k),
            };
            ((v + 0.5) as u32).min(255)
        };
        (ch(0) << 16) | (ch(1) << 8) | ch(2)
    };
    let ok = |ink: u32| -> bool {
        let corner = match side {
            GlyphInkSide::Dark => chan_max(ink, fg),
            GlyphInkSide::Light => chan_min(ink, fg),
        };
        let l = rel_luminance(corner);
        match side {
            GlyphInkSide::Dark => l <= cap,
            GlyphInkSide::Light => l >= cap,
        }
    };
    let c1 = cand(1.0);
    if ok(c1) {
        return Some(c1);
    }
    if !ok(cand(0.0)) {
        // Even the extreme fails: the foreground alone breaches the bound.
        // Fail closed.
        return None;
    }
    let (mut lo_k, mut hi_k) = (0.0f32, 1.0f32);
    for _ in 0..FRESH_INK_GLYPH_BISECT {
        let mid = 0.5 * (lo_k + hi_k);
        if ok(cand(mid)) {
            lo_k = mid
        } else {
            hi_k = mid
        }
    }
    Some(cand(lo_k))
}

/// One memoized fresh-ink glyph palette: the `(theme_fg, theme_bg)` key and
/// the seven band inks, or `None` for a memoized refusal (see
/// [`fresh_ink_glyph_palette`]).
pub(super) type GlyphPaletteMemo = ((u32, u32), Option<[u32; RAINBOW_BANDS.len()]>);

/// The seven band inks for one theme, or `None` when this theme gets no tint.
///
/// All seven or none: a per-band suppression would draw a rainbow with holes,
/// so if any band fails the chroma gates the whole layer stands down and the
/// text keeps its own colour.
pub(super) fn fresh_ink_glyph_palette(fg: u32, bg: u32) -> Option<[u32; RAINBOW_BANDS.len()]> {
    let (side, cap) = glyph_ink_bound(fg, bg)?;
    let mut out = [0u32; RAINBOW_BANDS.len()];
    for (slot, &band) in out.iter_mut().zip(RAINBOW_BANDS.iter()) {
        let ink = fresh_ink_glyph_ink(band, fg, side, cap)?;
        if chan_spread(ink) < FRESH_INK_GLYPH_MIN_SPREAD
            || chan_dist(ink, fg) < FRESH_INK_GLYPH_MIN_DELTA
        {
            return None;
        }
        *slot = ink;
    }
    Some(out)
}

// ---- the classic landing ring's grade ---------------------------------------
//
// The landing splash scales with distance travelled, for every non-kitty
// style (fire flash, water ripple, light-theme veil, dark round bloom)
// (owner, 2026-09-08):
// `classic_ring_radius_factor` widens and `classic_ring_life` lengthens with
// impact. An 8-cell hop (impact 1.0) is ungraded; a full-line jump (impact
// 3.5) rings twice as wide for 300 ms, matching the rainbow kitty's ring.

/// Radius growth per unit of impact above 1: ×2.0 at the 3.5 cap
/// (`1 + 0.4 · 2.5`). Rainbow Kitty's `RING_R_PER_IMPACT_CH` doubles its ring
/// over the same range.
pub(super) const CLASSIC_RING_RADIUS_PER_IMPACT: f32 = 0.4;
/// Life growth per unit of impact above 1: the built-in 180 ms reaches 300 ms
/// at the cap (`0.18 · (1 + 0.267 · 2.5)`).
pub(super) const CLASSIC_RING_LIFE_PER_IMPACT: f32 = 0.267;

/// How much wider a ring of impact `scale` expands: `1.0` at impact 1 (and for
/// anything non-finite or below), `2.0` at the 3.5 cap.
pub(super) fn classic_ring_radius_factor(scale: f32) -> f32 {
    if !scale.is_finite() {
        return 1.0;
    }
    let impact = scale.clamp(1.0, crate::rainbow_kitty::timing::IMPACT_MAX);
    1.0 + CLASSIC_RING_RADIUS_PER_IMPACT * (impact - 1.0)
}

/// A ring's life in seconds: `base` graded by impact `scale` — `base` itself at
/// impact 1, `base · 1.667` at the cap (0.18 s → 0.30 s).
pub(super) fn classic_ring_life(base: f32, scale: f32) -> f32 {
    if !scale.is_finite() {
        return base;
    }
    let impact = scale.clamp(1.0, crate::rainbow_kitty::timing::IMPACT_MAX);
    base * (1.0 + CLASSIC_RING_LIFE_PER_IMPACT * (impact - 1.0))
}

/// The comet colour for `style` at path position `pos` (0 tail .. 1 head):
/// the single ramp shared by the live animator ([`CursorGlow::comet_color`])
/// and the settings-card demo. `hue` is the rolling rainbow phase in turns
/// (rainbow/sparkle only).
pub fn style_comet_color(style: GlowStyle, color: u32, accent: u32, hue: f32, pos: f32) -> u32 {
    match style {
        GlowStyle::Lumen => lerp_rgb(accent, color, pos),
        // Comet: an icy dust tail. Far back the accent dims toward a dark haze,
        // the body brightens through the base hue, and the last quarter
        // whitens toward ice at the head. The cap stays below pure white so
        // the nucleus cursor's coma owns the brightest point.
        GlowStyle::Comet => {
            let dusty = lerp_rgb(accent, 0x0000_0000, 0.45);
            let body = lerp_rgb(dusty, color, pos);
            if pos > 0.75 {
                lerp_rgb(body, 0x00F0_FAFF, (pos - 0.75) * 4.0 * 0.80)
            } else {
                body
            }
        }
        // Phaser: `pos` is a hue offset in turns from the live sweep phase.
        // The live animator passes each cell's laid-hue offset (see
        // `emit_comet`); the settings preview passes its tail→head position.
        GlowStyle::Phaser => hsv2rgb((hue + pos).fract(), 0.95, 1.0),
        // Classic keeps a half-turn sweep (the phaser lays a full turn). No
        // live path reaches this arm: `CursorGlow::tick` hands the frame to
        // the classic engine first, and the settings preview drives that same
        // engine. It answers with the engine's own colour rule so the two
        // cannot drift, in the spectrum face: the mono face is chosen by the
        // style's spelling (`GlowConfig::classic_mono`), which this
        // signature does not carry, so a caller wanting it calls
        // `crate::classic_wake::comet_color` with the flag.
        GlowStyle::Classic => crate::classic_wake::comet_color(false, color, accent, hue, pos),
        GlowStyle::Sparkle => hsv2rgb((hue + pos * 0.5).fract(), 0.95, 1.0),
        // The ribbon and cursor share this same continuous seven-anchor field.
        // This arm serves the lone-cell fallback and settings preview.
        GlowStyle::RainbowKitty => rainbow_cycle_ink(pos),
        // Head capped short of white (0.83 ≈ FIRE_IDX_MAX/1023) so the bloomed
        // streak is amber and letters under it stay readable.
        GlowStyle::Fire => fire_ramp((pos).min(0.83)),
        // Monochrome beam: the hue stays saturated almost to the tail tip,
        // rolling off slightly. No white lerp; the filament layer supplies the
        // only (capped) highlight.
        GlowStyle::Laser => lerp_rgb(0x0000_0000, color, 0.70 + 0.30 * pos),
        // Coherent tube: near-uniform hue with a gentle brightening toward the
        // head; the beam layers' specular axis supplies the highlight.
        GlowStyle::Beam => lerp_rgb(0x0000_0000, color, 0.60 + 0.40 * pos),
        GlowStyle::Water => water_ramp(pos), // deep-blue tail → bright-cyan crest
    }
}

/// The particle colour for `style` at remaining life `fade` (1 fresh → 0 dead)
/// with per-particle seed `hue` — shared by the live ember/droplet/spark emitters
/// and the settings-card effect demo. `color` is the style's base hue
/// (`GlowConfig::color`), used only by the monochrome Laser.
pub fn style_particle_color(style: GlowStyle, color: u32, hue: f32, fade: f32) -> u32 {
    match style {
        GlowStyle::Fire => fire_ramp(fade.min(0.83)), // hot amber → cool red (capped short of white)
        // Droplet: bright crest-cyan darkening toward deep blue as it falls and
        // dies (the hue seed nudges each droplet's depth).
        GlowStyle::Water => water_ramp(0.35 + 0.55 * fade + 0.1 * hue),
        // Ablation spark: white-hot off the cut, cooling into the pure beam hue.
        // The 0.55 white ceiling keeps every spark inside `laser_is_monochrome`'s
        // 0.75 hue bound.
        GlowStyle::Laser => lerp_rgb(color, 0x00FF_FFFF, 0.55 * fade),
        // Stardust mote: near-white when fresh, settling into the beam's
        // ice-blue. (The live emitter draws its own star tints; this keeps the
        // settings preview in the same family.)
        GlowStyle::Beam => lerp_rgb(color, 0x00FF_FFFF, 0.35 + 0.45 * fade),
        // Comet debris: a glint of shed ice, near-white while young, cooling
        // into the tail's own hue.
        GlowStyle::Comet => lerp_rgb(color, 0x00FF_FFFF, 0.65 * fade),
        // Rainbow kitty debris snaps to a named stop, never the open wheel
        // (`docs/design/RAINBOW-KITTY-V2.md` §2.3 C1: point marks snap via
        // `spectrum_snap`). The uniform random seed still
        // decorrelates grains but selects a band index, so a jump sheds only
        // the ribbon's seven anchors. The white core is a glint, not a hue.
        GlowStyle::RainbowKitty => {
            lerp_rgb(spectrum_snap(hue.rem_euclid(1.0)), 0x00FF_FFFF, 0.35 * fade)
        }
        // Sparkle: slightly white-cored rainbow. This style's identity is the
        // open wheel; it is not bound by the trail's anchor rule.
        _ => lerp_rgb(hsv2rgb(hue, 0.85, 1.0), 0x00FF_FFFF, 0.35 * fade),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "centre + per-axis radii + colour + peak; a param struct would obscure the geometry at the many emitter call sites"
)]
pub(crate) fn push_halo_over(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    color: u32,
    peak: u8,
) {
    if peak < 96 {
        return;
    }
    let scale = (peak as f32 / 255.0).max(0.55);
    push_halo_rows(
        out,
        geom,
        (cx, cy),
        (rx * scale, ry * scale),
        color,
        HaloMode::Over,
    );
}

/// A round glow's centre and radii fitted inside the effects box, so its
/// radial falloff fades to nothing before the box's edge instead of being cut
/// there (a landing at the grid's left column once left a bloom sliced by a
/// hard vertical edge): the centre moves inward by at most a cell on each
/// axis, then both radii shrink together until the ellipse fits.
pub(crate) fn fit_glow_in_box(
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    let fit = |c: f32, r: f32, lo: f32, hi: f32, step: f32| {
        let mut c = c;
        if c - r < lo {
            c = (c + step).min(lo + r).min(hi);
        }
        if c + r > hi {
            c = (c - step).max(hi - r).max(lo);
        }
        (c, (c - lo).min(hi - c).max(0.0))
    };
    let (cx, room_x) = fit(
        cx,
        rx,
        geom.fx_left() as f32,
        geom.fx_right() as f32,
        geom.cw as f32,
    );
    let (cy, room_y) = fit(
        cy,
        ry,
        geom.fx_top() as f32,
        geom.fx_bot() as f32,
        geom.ch as f32,
    );
    let k = (room_x / rx.max(1e-3))
        .min(room_y / ry.max(1e-3))
        .clamp(0.0, 1.0);
    ((cx, cy), (rx * k, ry * k))
}

/// How many nested veils a soft veil stacks ([`push_soft_veil`]).
pub(crate) const SOFT_VEIL_LAYERS: usize = 10;

/// A source-over veil that falls off radially from its centre to nothing at
/// its rim, in `ink` with at most `cap` of opacity at the centre. One veil
/// held under a ceiling is a flat disc out to half its radius (the renderer
/// lays `min(falloff, cap)`), which reads on paper as a stain; this stacks
/// [`SOFT_VEIL_LAYERS`] nested veils, each a share of the ceiling and a
/// step smaller, so the opacity climbs toward the centre in soft steps. It
/// is fitted inside the effects box first ([`fit_glow_in_box`]).
pub(crate) fn push_soft_veil(
    halos: &mut Vec<RainHalo>,
    geom: Geom,
    centre: (f32, f32),
    radii: (f32, f32),
    ink: u32,
    cap: u8,
) {
    let ((cx, cy), (rx, ry)) = fit_glow_in_box(geom, centre, radii);
    if rx < 1.0 || ry < 1.0 || cap == 0 {
        return;
    }
    // Each layer's share of the centre's opacity, so the stack composites
    // (source-over) to about `cap` there.
    let a = f32::from(cap) / 255.0;
    let layer = 1.0 - (1.0 - a).powf(1.0 / SOFT_VEIL_LAYERS as f32);
    let layer_cap = ((layer * 255.0).round() as u32).clamp(1, u32::from(cap));
    for k in 0..SOFT_VEIL_LAYERS {
        let shrink = 1.0 - k as f32 / SOFT_VEIL_LAYERS as f32;
        push_halo_rows(
            halos,
            geom,
            (cx, cy),
            (rx * shrink, ry * shrink),
            (ink & 0x00FF_FFFF) | (layer_cap << 24),
            HaloMode::Over,
        );
    }
}

// ---- light-theme bands over wide glyphs -----------------------------------
//
// The beam family's light bands (`CursorGlow::emit_comet_light`) are ink
// under the letters, 0.8 of the row tall. Over a Latin letter that leaves
// paper above and below the x-height; over a wide glyph (CJK, emoji) the band
// fills the glyph's whole box, the gaps between its strokes included, so at
// the Latin band's strength it took the newest CJK glyph on GitHub Light to
// 6.5:1. Over wide glyphs the band carries on in the same shape and hue, its
// soft falloff and all, at only the opacity that leaves the theme's text
// [`LIGHT_WIDE_BED_KEEP`] of its contrast on the paper
// ([`light_alpha_keeping`]). (Rails along the row's edges, which once stood
// in for it, read as a hard-edged taupe frame boxing the glyphs.)

/// The share of the theme's text contrast on its paper a light band keeps
/// under a wide glyph: a wide glyph's counters lie wholly on the band, where
/// a Latin letter's share its ground with the paper above and below it, so
/// at 0.6 the newest CJK glyph measured 6.8:1 on GitHub Light against the
/// Latin letter's 9; at this share the band still reads in its hue.
pub(crate) const LIGHT_WIDE_BED_KEEP: f32 = 0.75;

/// The one dash: a velocity-aligned two-vertex AA beam trailing `trail_s`
/// seconds of a particle's live ballistic motion behind `(x, y)`, shared by
/// the laser ablation spark, the rainbow kitty shooting-star body and the
/// sparkle mini-comet. `tail` colours the trailing vertex at quarter
/// coverage, `head` the tip at full.
#[allow(
    clippy::too_many_arguments,
    reason = "output + geometry + position + live velocity + trail/thickness knobs + \
              the two tints + coverage; the dash IS this parameter set"
)]
pub(super) fn push_velocity_dash(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    trail_s: f32,
    thick: f32,
    tail: u32,
    head: u32,
    cov: u8,
) {
    let verts = dash_verts(x, y, vx, vy, trail_s, tail, head, cov);
    comet_beam(out, geom.beam_clip(), &verts, thick, 1, 0.0);
}

/// [`push_velocity_dash`] on a light ground: the same dash painted
/// source-over ([`comet_beam_over`]) in `ink` at opacity `alpha` (its tail at
/// a quarter of it), capped at the may-touch-text ceiling
/// ([`LIGHT_INK_ALPHA_CAP`]). `ink` is already darkened by the caller.
#[allow(
    clippy::too_many_arguments,
    reason = "the dash's parameter set, with one ink in place of two tints"
)]
pub(super) fn push_velocity_dash_over(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    trail_s: f32,
    thick: f32,
    ink: u32,
    alpha: u8,
) {
    let alpha = alpha.min(LIGHT_INK_ALPHA_CAP as u8);
    let verts = dash_verts(x, y, vx, vy, trail_s, ink, ink, alpha);
    comet_beam_over(out, geom.beam_clip(), &verts, thick, 1, 0.0);
}

/// The dash's two vertices: the tail `trail_s` seconds back along the live
/// velocity at a quarter of `cov`, the head at `(x, y)` at full.
#[allow(
    clippy::too_many_arguments,
    reason = "position + live velocity + trail + the two tints + coverage"
)]
fn dash_verts(
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    trail_s: f32,
    tail: u32,
    head: u32,
    cov: u8,
) -> [BeamVertex; 2] {
    [
        BeamVertex {
            x: x - vx * trail_s,
            y: y - vy * trail_s,
            color: tail,
            cov: cov / 4,
        },
        BeamVertex {
            x,
            y,
            color: head,
            cov,
        },
    ]
}

/// A light-ground grain: one source-over mote of radius `r` at `(cx, cy)` in
/// the may-touch-text ink ([`InkRole::OverText`] of `rgb`), its opacity
/// `alpha` capped at that role's ceiling ([`LIGHT_INK_ALPHA_CAP`]) through
/// the colour's high byte, so the falloff's opaque centre never reads above
/// it. The light twin of [`push_dust_halo`] for every non-Fire particle arm
/// (and any emitter that sheds round grains on white): source-over buys
/// presence with area where additive light uses coverage, so callers pass
/// the dark radius times [`crate::effect_util::STAR_ARM_INK`].
pub(super) fn push_light_mote(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    (cx, cy): (f32, f32),
    r: f32,
    rgb: u32,
    alpha: u8,
) {
    let role = InkRole::OverText;
    let cap = u32::from(alpha).min(role.alpha_cap() as u32);
    // Under a few levels of opacity a mote changes no pixel by more than its
    // rounding.
    if cap < 4 {
        return;
    }
    let color = (role.ink(rgb) & 0x00FF_FFFF) | (cap << 24);
    push_halo_rows(out, geom, (cx, cy), (r, r), color, HaloMode::Over);
}

/// A bead of ink on paper at `(cx, cy)`: a round disc of radius `r` in the
/// `rim` ink with a disc of the `core` ink inside it (three fifths of the
/// radius), both anti-aliased at their edges and laid source-over at
/// opacity `alpha`, one pixel row at a time. A soft halo of a bright ink at
/// a may-touch-text opacity composites on white to a pale, greyed fleck; the
/// deep rim gives the mark its edge on the page and the core its colour.
/// Only for marks never laid over a letter.
pub(super) fn push_paper_bead(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    r: f32,
    (rim, core): (u32, u32),
    alpha: u8,
) {
    if alpha < 4 || r < 0.75 {
        return;
    }
    let mut disc = |rr: f32, rgb: u32| {
        let rr2 = rr * rr;
        let (y0, y1) = ((cy - rr).floor() as i32, (cy + rr).ceil() as i32);
        for y in y0..y1 {
            // The row's chord through the disc, its ends anti-aliased by the
            // pixel's share of the chord (and the row's share of the disc at
            // its top and bottom).
            let dy = y as f32 + 0.5 - cy;
            let rest = rr2 - dy * dy;
            if rest <= 0.0 {
                continue;
            }
            let half = rest.sqrt();
            let rows = (rr - dy.abs() + 0.5).clamp(0.0, 1.0);
            let (xl, xr) = (cx - half, cx + half);
            let mut lay = |x: i32, w: i32, f: f32| {
                let a = (f32::from(alpha) * (f * rows).clamp(0.0, 1.0)) as u8;
                if a > 0 && w > 0 {
                    push_fx_quad(out, geom, x, y, w, 1, premul_rgb(rgb, a), a);
                }
            };
            let (pl, pr) = (xl.floor() as i32, (xr.ceil() as i32) - 1);
            if pl == pr {
                lay(pl, 1, xr - xl);
                continue;
            }
            lay(pl, 1, (pl + 1) as f32 - xl);
            if pr > pl + 1 {
                lay(pl + 1, pr - pl - 1, 1.0);
            }
            lay(pr, 1, xr - pr as f32);
        }
    };
    disc(r, rim);
    disc(r * 0.6, core);
}

/// Where the crescent's shadow disc sits, as a share of the moon's radius
/// from its centre: east and a little north, so the lit limb bulges to the
/// lower left and the horns point up and to the right, the way a crescent
/// moon is drawn (an upright, east-opening crescent is the letter `c`).
pub(super) const CRESCENT_SHADOW_OFFSET: (f32, f32) = (0.42, -0.30);
/// The crescent's shadow disc radius, as a share of the moon's.
pub(super) const CRESCENT_SHADOW_R: f32 = 0.84;

/// A crescent moon of radius `r` at `(cx, cy)`: the lit disc minus a shadow
/// disc ([`CRESCENT_SHADOW_R`] of its radius, at [`CRESCENT_SHADOW_OFFSET`]
/// from its centre), so a thick limb tapers into two horns. Laid one pixel
/// row at a time: each row's lit chord minus the shadow's, so one or two
/// spans, each with anti-aliased ends (at most six quads a row). `over`
/// paints it source-over at opacity `cov` (a light ground; `rgb` already
/// inked), else it is additive light of `rgb` at `cov`.
pub(super) fn push_crescent(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    r: f32,
    rgb: u32,
    cov: u8,
    over: bool,
) {
    if cov == 0 || r < 1.0 {
        return;
    }
    let (sx, sy) = (
        cx + CRESCENT_SHADOW_OFFSET.0 * r,
        cy + CRESCENT_SHADOW_OFFSET.1 * r,
    );
    let sr = CRESCENT_SHADOW_R * r;
    let mut span = |y: i32, xl: f32, xr: f32| {
        if xr - xl < 0.05 {
            return;
        }
        let mut lay = |x: i32, w: i32, f: f32| {
            let a = (f32::from(cov) * f.clamp(0.0, 1.0)) as u8;
            if a > 0 {
                let alpha = if over { a } else { 0 };
                push_fx_quad(out, geom, x, y, w, 1, premul_rgb(rgb, a), alpha);
            }
        };
        let (pl, pr) = (xl.floor() as i32, (xr.ceil() as i32) - 1);
        if pl == pr {
            lay(pl, 1, xr - xl);
            return;
        }
        lay(pl, 1, (pl + 1) as f32 - xl);
        if pr > pl + 1 {
            lay(pl + 1, pr - pl - 1, 1.0);
        }
        lay(pr, 1, xr - pr as f32);
    };
    let (r2, sr2) = (r * r, sr * sr);
    let (y0, y1) = ((cy - r).floor() as i32, (cy + r).ceil() as i32);
    for y in y0..y1 {
        let yc = y as f32 + 0.5;
        let dy = yc - cy;
        if dy.abs() >= r {
            continue;
        }
        let half = (r2 - dy * dy).sqrt();
        let (xl, xr) = (cx - half, cx + half);
        let ds = yc - sy;
        if ds.abs() >= sr {
            span(y, xl, xr);
            continue;
        }
        let shalf = (sr2 - ds * ds).sqrt();
        let (sl, sr_x) = (sx - shalf, sx + shalf);
        span(y, xl, xr.min(sl));
        span(y, xl.max(sr_x), xr);
    }
}

/// Push one radial halo of premultiplied peak light: its rect is clamped to
/// the window interior and split into per-cell-row [`RainHalo`] quads sharing
/// one centre and falloff radii. Radii are floored to 1.
pub(super) fn push_halo(
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
    push_halo_rows(out, geom, (cx, cy), (rx, ry), peak, HaloMode::Add);
}

/// The radial-halo band walk behind [`push_halo`], [`push_halo_over`] and the
/// kitty cursor's exit veils: the halo at `(cx, cy)` with radii `(rx, ry)`
/// (rounded, floored to 1) is clamped to the effects box (grid + head band)
/// and split into per-cell-row [`RainHalo`]s in `mode`, sharing centre and
/// radii.
pub(crate) fn push_halo_rows(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
    color: u32,
    mode: HaloMode,
) {
    let rxi = (rx.round() as i32).max(1);
    let ryi = (ry.round() as i32).max(1);
    let (cxi, cyi) = (cx.round() as i32, cy.round() as i32);
    let x0 = (cxi - rxi).max(geom.fx_left());
    let x1 = (cxi + rxi).min(geom.fx_right());
    let y0 = (cyi - ryi).max(geom.fx_top());
    let y1 = (cyi + ryi).min(geom.fx_bot());
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // Store the true centre, never clamped into the box: a clamped off-edge
    // centre would draw the surviving sliver near peak weight as a bright
    // fringe. A positive overshoot fits u16; a negative centre cannot be
    // represented, so the halo is culled (only its dim tail was visible).
    if cxi < 0 || cyi < 0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = geom.origin_y as i32;
    let x0_u = x0 as u16;
    let w_u = (x1 - x0) as u16;
    let cx_u = cxi as u16;
    let cy_u = cyi as u16;
    let rx_u = rxi as u16;
    let ry_u = ryi as u16;
    let mut yy = y0;
    while yy < y1 {
        // Grid-row damage hint anchored at origin_y: an above-grid band tags
        // row 0, never a wrapped u16.
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0_u,
            y: yy as u16,
            w: w_u,
            h: (band_end - yy) as u16,
            color,
            cx: cx_u,
            cy: cy_u,
            rx: rx_u,
            ry: ry_u,
            mode,
        });
        yy = band_end;
    }
}

/// A round stardust grain on dark ground: a radial mote drawn at the
/// composited light of the four-point plus it stands in for.
///
/// [`push_twinkle_star`] lays three coincident marks at the crossing (two arms
/// at `cov`, the nucleus at [`crate::effect_util::STAR_CORE_ADD`]), so a plus
/// centre carries [`crate::effect_util::STAR_STACK_ADD`] (2.35x) of `cov`. One
/// `push_halo` at `cov` would be 2.35x darker. So: a skirt at `cov` plus a
/// small hot core at [`STAR_CORE`] of the radius carrying the rest. That keeps
/// the total light where the plus had it and keeps the soft edge. Every
/// additive stardust site calls this; [`push_light_mote`] is its light twin.
pub(super) fn push_dust_halo(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    r: f32,
    rgb: u32,
    cov: u8,
) {
    if cov == 0 {
        return;
    }
    push_halo(out, geom, cx, cy, r, r, premul_rgb(rgb, cov));
    // Floored at 1 px so the smallest grain still has a lit centre.
    let core = (r * STAR_CORE).max(1.0);
    // Clamped at the byte; above `cov` 188 the skirt plus a 255 core already
    // saturates, as the plus does above `cov` 109.
    let hot = (f32::from(cov) * (crate::effect_util::STAR_STACK_ADD - 1.0)).min(255.0) as u8;
    push_halo(out, geom, cx, cy, core, core, premul_rgb(rgb, hot));
}

/// The water ripple ring (the water landing and the droplet cursor's
/// waterline): a flattened elliptical annulus at `(cx, cy)` with outer radii
/// `(rx, ry)` and a `thick` px rim, laid as left/right additive slabs one
/// pixel row tall, so its curve steps a pixel at a time (slabs half `ry`
/// tall stood its sides in visible stairs).
pub(crate) fn push_ripple_ring(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    (cx, cy): (f32, f32),
    (rx, ry): (f32, f32),
    thick: f32,
    premul: u32,
) {
    let rslab = 1;
    let inner_rx = (rx - thick).max(0.0);
    let inner_ry = ry * (inner_rx / rx);
    let (cxi, cyi) = (cx as i32, cy as i32);
    let ry_i = ry as i32;
    let mut dy = -ry_i;
    while dy < ry_i {
        let h = rslab.min(ry_i - dy);
        let ym = (dy as f32 + h as f32 * 0.5) / ry;
        let outer = rx * (1.0 - ym * ym).max(0.0).sqrt();
        let inner = if inner_ry > 0.5 {
            let yin = (dy as f32 + h as f32 * 0.5) / inner_ry;
            inner_rx * (1.0 - yin * yin).max(0.0).sqrt()
        } else {
            0.0
        };
        let (o, i) = (outer as i32, inner as i32);
        if o > i {
            push_rect(out, geom, cxi - o, cyi + dy, o - i, h, premul);
            push_rect(out, geom, cxi + i, cyi + dy, o - i, h, premul);
        }
        dy += h;
    }
}

/// Fractional part `x − ⌊x⌋`: unlike `f32::fract`, never negative.
pub(super) fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// A lightning bolt's brightness at `u` (0 born → 1 dead) of its life: one
/// flash. Born at full strength (the strike), held for the first
/// [`BOLT_HOLD`] of its life, then a monotone ease down to
/// `1 − BOLT_FADE`; the channel's spatial burn-out (`emit_bolts`, from
/// [`BOLT_BURN_START`]) extinguishes it. Never rising again is the point: a
/// pixel a bolt lights rises once and falls once, so lightning's flash count
/// is its onset count (`CursorGlow::admit_bolt`).
pub(super) fn bolt_flash(u: f32) -> f32 {
    if u < BOLT_HOLD {
        1.0
    } else {
        1.0 - BOLT_FADE * smoothstep01((u - BOLT_HOLD) / (1.0 - BOLT_HOLD))
    }
}

/// How long a bolt holds its full strike, as a share of its life.
pub(super) const BOLT_HOLD: f32 = 0.15;
/// How far a bolt's brightness eases down across the rest of its life.
pub(super) const BOLT_FADE: f32 = 0.60;
/// Where in a bolt's life its channel starts retracting from its root.
pub(super) const BOLT_BURN_START: f32 = 0.55;

/// The beam power envelope: full power for the first 40% of a spark's life,
/// then one cosine down to zero. Shared by the tube's brightness and its
/// thickness collapse (`core_thick`), so the rod dims and thins as one object.
pub(super) fn beam_power(frac: f32) -> f32 {
    if frac < 0.40 {
        1.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * (frac - 0.40) / 0.60).cos())
    }
}

/// HSV (all `0..1`; `h` in turns, any real, wrapped) → `0x00RRGGBB`: the colour
/// wheel the classic wake and the phaser turn.
pub(crate) fn hsv2rgb(h: f32, s: f32, v: f32) -> u32 {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let u = |c: f32| ((c.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32;
    (u(r) << 16) | (u(g) << 8) | u(b)
}

/// Cubic smoothstep `0..1` (Hermite): the classic AA/feather easing.
pub(super) fn smoothstep01(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}
