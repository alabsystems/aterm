// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PURE RASTER EMISSION — the colour laws (the rainbow spectrum, light-theme
//! ink, the fresh-ink glyph palette) and the quad/halo pushers. Every item
//! here is a function of its arguments: no [`CursorGlow`] state, no clock.

use super::*;

/// The Comet debris GLINT's coverage, as a fraction of the grain's own — the
/// diffraction spike is a highlight ON a grain of ice, so it is drawn dimmer
/// than the grain that throws it.
///
/// It is a named constant because TWO sites must agree on it: the dealt grain
/// that still draws the 4-point glint, and the UNDEALT grain that no longer
/// does and is paid the glint's light instead (see the Comet arm of
/// `emit_particles`). Split across two literals they would drift, and the drift
/// would land as a brightness step between two grains of the same tail.
pub(super) const COMET_GLINT_COV: f32 = 0.5;

/// THE SEVEN NAMES — the vocabulary every point-mark of this family snaps to,
/// red → violet (`0x00RRGGBB`).
///
/// **`@generated` from [`crate::spectrum`] — do not edit by hand.** Each entry
/// is `spectrum_stop(i)`, transcribed so it can stay a `const` (array lengths
/// and const contexts across this file depend on it); `rainbow_bands_are_the_
/// spectrum_stops` is what stops the transcription from drifting.
///
/// THE SEVEN CONSTANT-LUMINANCE STOPS THIS REPLACES — `#FF0000 #B76E00 #838400
/// #00942D #007CF8 #8066FF #CF1AFF`, every hue solved to carry red's relative
/// luminance — were reversed on glass. Two measured reasons, both properties of
/// holding the luminance flat rather than of the machinery: yellow at red's
/// light is the olive `#838400` and green the bottle `#00942D`, so the trail
/// read pale; and the arc that connected them sat **15.59 %** inside the
/// design's cyan window (HSV `[165°, 200°]` at `S > 0.3`) with a dead-centre
/// `#008E8E` in its table. See [`crate::spectrum`] for the whole reversal.
///
/// THE VALUES ARE CANONICAL ROYGBIV, and they are the owner's ruling
/// (2026-08-27: *"a rainbow. like in the sky"*, *"do you know how to make a
/// regular ROYGBIV rainbow color pallet? do that"*). Seven authored anchors —
/// red, orange, yellow, green, blue, INDIGO, violet — in spectral order.
///
/// CYAN IS NOT A BAND, also the owner's ruling in the same breath (*"cyan isn't
/// a color in the rainbow!"*), and it is simply true of the seven names. It does
/// NOT follow that the green→blue interval must be destroyed to get there. The
/// six-anchor arc this file used to transcribe carried a hand-built neutralized
/// handoff over that one segment which drove the colour **95.3 %** of the way to
/// flat grey at its midpoint — a hole where a seventh of the spectrum should be,
/// which is the shape of the complaint that produced this palette, and which was
/// separately PROVEN unreachable by any chroma-or-lightness lever: at the
/// crossing the composited pixel was byte-identical `(58,75,80)` for every
/// lightness from `V = 0.60` to `V = 1.00` once each colour took its own
/// legibility ceiling. Seven authored anchors need no such special case — green
/// and blue are adjacent stops on a continuous ramp, exactly as they are in the
/// sky — so the handoff is deleted rather than retuned, and no cyan STRIPE is
/// authored anywhere. What the arc does is CROSS the wedge on its way from green
/// to blue, at full chroma and full value since 2026-09-15 (the owner, retiring
/// the last of the anti-cyan machinery: *"this is legacy cruft. delete it"*).
pub(super) const RAINBOW_BANDS: [u32; SPECTRUM_STOPS] = [
    0x00FF_0000, // red
    0x00FF_7F00, // orange
    0x00FF_FF00, // yellow
    0x0000_FF00, // green
    0x0000_00FF, // blue
    0x004B_0082, // indigo
    0x0094_00D3, // violet
];

// ---- ADDITIVE-LIGHT BUDGET OVER TEXT (the legibility bound) ---------------
//
// The bed and the transients compose along DIFFERENT streams and their contrast
// models differ:
//
//   * the continuous bed rides `glow_under` (beneath the glyph pass), so it
//     lifts only the GROUND — contrast is (unlit ink) vs (lit ground);
//   * the transients ride `glow_halo` / `cursor_glow_add` ON TOP, lifting the
//     INK AND the ground together — and once the ink saturates (`#C8D3F5` has
//     only 55/44/10 per-channel headroom) the ground keeps climbing while the
//     glyph is pinned at white, collapsing contrast.
//
// A cap is derived against the 5.25:1 bar for its own stream, so a cap from one
// stream is NOT interchangeable with a cap from the other.

/// The TRANSIENT over-ink share (fresh-ink pop, crown, starfield, and — via
/// `crate::cursor_glow::OVER_INK_COV_CAP` — the rainbow cursor's halo and
/// twinkle). Worst case is the bed at 34 (ground only) plus 48 (ground AND
/// ink), measured at 5.44:1 — above the 5.25 bar this crate certifies.
// 47, DOWN FROM 48 ON THE CROSSING ROOF (2026-08-30). The ceiling is solved
// against the arc's brightest over-ink colour, and the crossing's lift gave
// the arc a brighter one: the roof's mint #80FFD3 at t 0.576. At 48 the
// guarded contrast bar read 5.2692 against the 5.27 guard (the raw 5.25:1
// still held) — a 0.0008 miss, but the guard exists so the raw bar is never
// the thing on trial. One level restores the margin without touching the
// guard, which is the direction the discipline allows.
pub(crate) const OVER_INK_COV_CAP: f32 = 47.0;

/// **§8 (d): THE CARET IS THE BRIGHTEST THING IN THE EFFECT** — the least WCAG
/// relative luminance the block cursor's fill may carry while the effect is
/// live, on a `0..=255` scale.
///
/// **THE CLAUSE FAILED 24 TRIALS OUT OF 24.** A pure-white sparkle composited to
/// luminance `255` every time, while the cursor cell peaked at `111..237` — and
/// as low as `111..134` with the arc at the blue/violet end, where a saturated
/// spectrum colour simply does not carry much light. In four trials of ten the
/// winner sat two to eighteen cells back in the sparkle field; restricting the
/// search to CHROMATIC pixels, the brightest was still somewhere else ten times
/// out of ten. The eye lands on the brightest thing, and the brightest thing was
/// never the cursor.
///
/// **BOTH SIDES HAVE TO MOVE, AND THE ARITHMETIC SAYS SO.** A saturated violet
/// is `Y = 32/255`; a white dot is `255`. No cap on the sparkles that leaves
/// them visible can get under a violet caret, and no lift of the caret that
/// leaves it violet can get over a white dot. So the caret takes a FLOOR (it is
/// a cursor; being findable is its job) and the sparkle field takes a CEILING
/// derived from that floor.
///
/// `80` is chosen as the least floor that leaves the sparkle field alive: it
/// lifts `23..30 %` of the arc on the shipped themes (the red/orange end and the
/// violet end, where the caret's own colour is dark), costs at most `0.34` of
/// saturation there, and leaves the field's own ceiling high enough that a
/// sky sparkle keeps `62` of its `72` coverage. Raising it to `120` — enough to
/// leave the sparkles entirely alone — pales `39..68 %` of the arc and turns the
/// caret's red into a salmon, which is a different mark.
pub(crate) const RAINBOW_CARET_LIGHT_FLOOR: f32 = 80.0;

/// The share of [`RAINBOW_CARET_LIGHT_FLOOR`] the sparkle field is allowed.
///
/// A strict inequality is the whole of the clause, so the margin only has to be
/// real; `0.9` leaves eight levels of luminance between the brightest sparkle
/// and the dimmest caret, which is more than the `f32 -> u8` of either can move.
///
/// **IT IS A TERM IN THE LAW NOW, not only the statement of it.** It used to be
/// test-gated, for the reason `SPECTRUM_CYAN_SAT_MIN` is: what the emitter read
/// was [`RAINBOW_SPARKLE_COV_MAX`], a coverage, and this was the luminance that
/// coverage had been solved from. §4's budget spends the FIELD's ceiling
/// directly ([`rainbow_field_luminance`]), so the number is live and the two
/// readings of it cannot drift.
pub(crate) const RAINBOW_SPARKLE_LIGHT_SHARE: f32 = 0.9;

/// **THE SPARKLE FIELD'S OWN CEILING**, in additive coverage — what a starfield
/// point-mark may ask for so its composited centre stays under
/// [`RAINBOW_CARET_LIGHT_FLOOR`].
///
/// DERIVED, and `certify_rainbow_sparkle_cov_max` re-derives it: a mote lays a
/// skirt at `cov` and a core over it, so the pixel the eye reads carries
/// [`crate::effect_util::STAR_STACK_ADD`] `· cov`; that pixel must sit at or
/// under the sRGB level whose relative luminance is
/// `RAINBOW_CARET_LIGHT_FLOOR · RAINBOW_SPARKLE_LIGHT_SHARE` (`Y = 72`, which is
/// level `145`). `145 / 2.35 = 61`.
///
/// It REPLACES a clamp at `OVER_INK_COV_CAP · 1.5` (`72`), which was a
/// legibility bound — how much light may land on the ink — and had nothing to
/// say about which pixel on the screen is the brightest. Both bounds now apply;
/// this is the tighter one.
pub(super) const RAINBOW_SPARKLE_COV_MAX: f32 = 61.0;

/// **THE SPARKLE CEILING MUST BE THE TIGHTER OF THE TWO BOUNDS**, checked by the
/// compiler. A ceiling above what the emitter can already ask for is dead code
/// dressed as a safety property — which is the mistake [`RAINBOW_STAR_COV`]'s own
/// history records, in the other direction — and it would fail silently, at some
/// momentum nobody happened to capture.
const _: () = assert!(
    RAINBOW_SPARKLE_COV_MAX < OVER_INK_COV_CAP * 1.5,
    "the sparkle ceiling must bind before the legibility clamp"
);

// ---- §4, THE COMPOSITION RULE: ONE BUDGET, SPENT ONCE -------------------------------
//
// **THE DEFECT.** Every ceiling above bounds ONE LAYER. Nothing bounded the
// SUM, and light adds: the renderer composites every one of these streams with
// `add_sat` onto the same pixels, so a frame in which each layer is
// individually legal composites to `255` and STOPS. Measured on the shared test
// compositor, at the shipped intensity, on one pixel of the leading:
//
// ```text
//   the bed          74     (its own ceiling is 108)
//   the plume core  106     (its own ceiling is 228 with its underglow)
//   the halo tiers  105     (each halo's own peak is at most 46)
//   ------------------------
//   asked           285  ->  delivered 255, and the 30 levels over are not
//                            merely lost: they are lost UNEVENLY, so the
//                            mark grows a flat plateau with a cliff at each
//                            end of it. 3.4 % of lit columns carried one.
// ```
//
// Clipping is not a brightness bug, it is a SHAPE bug — the same argument
// [`RAINBOW_WAKE_COMPOSITE_CAP`] already makes about the plume's own ramp,
// applied to the frame instead of to one emitter. And it destroys the two
// properties §8 asks for by name: a saturated pixel has no falloff left to be
// smooth (the ledge), and a field that reaches `255` cannot be under a caret.
//
// **THE RULE.** One ledger, per row band and per device column, charged by
// every additive layer the family emits in the order the emitter emits them.
// The mark claims first and a companion takes what the mark left — which is
// [`RAINBOW_WAKE_COMPOSITE_CAP`]'s own "one shared headroom" idiom, promoted
// from the pair inside one emitter to the whole frame.

/// **THE FIELD'S OWN CEILING** — the most light every OVER-INK stream together
/// may put on one pixel outside the caret, in levels of one channel.
///
/// **DERIVED, and from the constant that already owns this claim.**
/// [`RAINBOW_SPARKLE_COV_MAX`] solves for what ONE mote may ask so its
/// composited centre stays under the sRGB level whose relative luminance is
/// `RAINBOW_CARET_LIGHT_FLOOR · RAINBOW_SPARKLE_LIGHT_SHARE` — level `145`. It
/// is the right solve and it was applied to the wrong thing: a ceiling on one
/// mote says nothing about a FIELD of them, and §8's clause is about the
/// brightest pixel of the frame, which is where several of them land together.
/// This is that same level, held on the SUM.
///
/// **AND IT IS SPENT IN LIGHT, NOT IN CHANNELS.** The clause is about
/// luminance — *"the eye lands on the brightest thing"* — and a saturated
/// spectrum colour carries a fraction of the light a white dot does at the same
/// channel value: red at level `112` is `Y = 9` while white at `112` is
/// `Y = 46`. So each lay spends what it is WORTH in light
/// ([`white_equiv_level`]: the grey of the same relative luminance), and a
/// chromatic plume is not charged a white star's price for the same byte. A
/// ceiling in raw channel levels would be sound and useless — measured, it took
/// the plume's nozzle from `112` to `12` to buy headroom for a luminance it was
/// never spending.
///
/// **OVER THE GROUND, which is the correction that makes it bite.**
/// `RAINBOW_SPARKLE_COV_MAX`'s own solve reads *"a mote's composited centre is
/// `STAR_STACK_ADD · cov`, and that pixel must sit under level 145"* — the
/// composited centre is `ground + STAR_STACK_ADD · cov`, and on the shipped
/// dark ground that `38` is the difference between a field at luminance `72`
/// and a field at `106`. Measured with the ceiling taken over black, the
/// brightest thing in the frame was still a sparkle, at `106` against a caret
/// at its `80` floor.
pub(crate) const RAINBOW_FIELD_LEVEL: f32 = 145.0;

/// The two readings of the field's ceiling — the LEVEL
/// [`RAINBOW_SPARKLE_COV_MAX`] was solved against and the LUMINANCE §4's budget
/// spends — must be the same bound, or a mote could pass one and fail the
/// other.
const _: () = assert!(RAINBOW_FIELD_LEVEL > 144.0 && RAINBOW_FIELD_LEVEL < 146.0);

/// **HOW FAR THE FIELD MAY DISAGREE UNDER THE JUMP STREAK BEFORE THE MARK STOPS
/// SPENDING CHROMA** — in `t`, the arc's own parameter.
///
/// See [`rainbow_streak_chroma`] for what it bounds. `0.02`
/// is a fortieth of the arc, which is **1.6 keystrokes** at the shipped lay rate
/// ([`RAINBOW_KITTY_HUE_STEP`]) — so the law is silent along a row (neighbouring
/// columns are `0.0125` apart and the mark keeps every level of its chroma) and
/// complete across a row BOUNDARY, where two readings are uncorrelated and
/// their difference is `0.5` on average. Swept on the emitter's own corpus:
/// `0.05` leaves 61 cyan px, `0.03` leaves 9, `0.02` leaves 0, and the cost at
/// `0.02` is 0.6 % of the streak's coloured area.
///
/// **IT IS DERIVED NOW, IN KEYSTROKES, BECAUSE A LITERAL COULD NOT SURVIVE THE
/// LAY RATE MOVING** (2026-08-31). The paragraph above states the bound in
/// keystrokes and then transcribes the product; the transcription had since
/// been tightened to `0.004` — a fifth of what the same paragraph argues for —
/// and nothing tied the two together. When
/// [`RAINBOW_TRAVERSE_SETTLED`] made one keystroke `1/36 = 0.0278` of the arc,
/// that stale literal was **a seventh of a single cell's lay**: every station of
/// a streak over a live mark saw its own transverse taps disagree by more than
/// the whole budget and folded to grey. Measured on the emitter's own corpus at
/// that moment: mean saturation loss against the arc **49.7 %** — a grey bar,
/// which is a worse answer to *"the cursor meteor streak isn't a single colour,
/// that's a rainbow!"* than a single colour would have been. Stated as a
/// keystroke count times the live rate it cannot fall out of step again.
///
/// **AND IT HAS A FLOOR NOW, WHICH IS WHAT MAKES IT SILENT ALONG A ROW.** The
/// paragraph above promises the law is *"silent along a row … and complete
/// across a row BOUNDARY"*, but a single threshold on a `smoothstep` cannot be:
/// the curve starts spending chroma at the first non-zero drift, and the
/// transverse taps ALWAYS read a non-zero drift, because the stack is about
/// `1.6` cells thick ([`RAINBOW_JUMP_FEATHER_WIDEN`]) and the field has a slope.
/// Measured at the settled traverse: same-row drift `0.032`, which folded the
/// streak to `keep = 0.09` — **5,520 of its ~10,800 coloured pixels rendered at
/// mean saturation `0.094`, a grey bar**, while every other hue bucket in the
/// same take sat at `0.82`-`0.97`. The floor is the drift a stack of the
/// streak's own thickness legitimately reads; below it the mark keeps every
/// level of its chroma, and the fold runs from there to a reading that can only
/// be two different runs mixing.
///
/// **AND THE FLOOR IS A THIRD OF THE SPECTRUM, WHICH THE CYAN CENSUS PAID FOR.**
/// The number is not a taste: with the fold switched off ENTIRELY, every cyan
/// census that existed because of it measured zero on the emitter's own corpus
/// (`lit=1,678,001 unruled=24,224 cyan=0` on the jump streak's own walk), and
/// the zoom streak's on-arc census returned `0.000` loss on a population that is
/// `S = 1.000` pure. Those censuses are all retired now — the anti-cyan rulings
/// they enforced were what greyed the arc — so what is recorded here is the
/// MEASUREMENT, which still stands: the seven-anchor palette retired the cyan
/// hazard this fold was insurance against, and the fold is kept ONLY for the
/// case the corpus cannot stage — a streak whose taps
/// straddle two different runs on adjacent rows, where two readings are
/// genuinely uncorrelated and average half the spectrum apart. A third of the
/// cycle is past anything one mark's own slope can produce at any traverse and
/// still well inside that. Anything tighter is measured harm: at `6` cells it
/// still folded 1,702 px to `S = 0.66`, and at the retired `0.004` literal it
/// folded 5,520 px to `S = 0.09`.
pub(super) const RAINBOW_STREAK_DRIFT_FLOOR: f32 = RAINBOW_CYCLE / 3.0;
/// Where the fold is COMPLETE: a reading uncorrelated with the station's own,
/// which across a row boundary averages `0.5` of the arc. A quarter of the arc
/// is already far past anything one mark's own slope can produce.
pub(super) const RAINBOW_STREAK_DRIFT_MAX: f32 = 0.5;

// THE FLOOR ADMITS THE STACK'S OWN THICKNESS, and the fold still has room to
// run past it — a floor at or above the max would switch the law off.
const _: () = assert!(
    RAINBOW_STREAK_DRIFT_FLOOR >= 1.0 / RAINBOW_TRAVERSE_SETTLED
        && RAINBOW_STREAK_DRIFT_FLOOR < RAINBOW_STREAK_DRIFT_MAX
        && RAINBOW_STREAK_DRIFT_MAX <= RAINBOW_CYCLE * 0.5,
    "the streak's drift floor must admit one cell's lay and leave the fold room, \
     and the fold must complete no later than the farthest two readings can be"
);

// **`rainbow_streak_chroma(drift)` bounds how far two adjacent samples may
// drift before their SUM leaves the arc** — a statement about mixing two
// different colours, not about which colours the palette authors.
// **THE ONE TRANSVERSE PROFILE LIVES IN THE RENDERER** (step 11).
// `rainbow_ribbon_across` stood here through step 10 and is now
// [`aterm_render::ribbon_profile`], beside [`aterm_render::ribbon_beam`], which
// is the only thing that evaluates it in production. That is where a
// rasterization law belongs: the quads are emitted HOST-SIDE, so CPU/GPU parity
// is structural rather than tested for, and a profile that lived in the producer
// would have to be handed across that boundary to stay one law.
//
// `RAINBOW_RIBBON_CORE_SHARE` went with it as [`aterm_render::RIBBON_CORE_SHARE`],
// for the same reason: the clamp that guarantees at least half of every reach is
// falloff is the rasterizer's own invariant, not the caller's promise.

// ---- rainbow kitty HEAD CLEARANCE ---------------------------------------------------
//
// [`rainbow_ribbon_profile`] already ramps brightness with a spark's normalized
// AGE, but age is only a proxy for distance-from-cursor, and a poor one: a
// spark's life CHAINS to the observed cadence (up to
// [`CursorGlow::RAINBOW_CHAIN_LIFE_MAX`]), so at a fast rhythm the profile's onset
// stretches over dozens of cells and its crest sits seconds behind the hand,
// while at a slow one the same law compresses into three. The ribbon is read in
// SPACE — how many letters back the light gets bright — so the
// clearance below is measured in CELLS and is therefore cadence-invariant: the
// letters under and immediately behind the cursor keep a deliberately quiet
// wash and the ribbon reaches its full body a few cells back, at every typing
// speed. It MULTIPLIES the age profile (never replaces it), so the documented
// onset/crest/feather law and every one of its endpoints survive untouched.
//
// Only same-row cells are dimmed: a wrapped or scrolled ribbon on another row
// is nowhere near the letters being typed, so it keeps its full body (and a
// hidden cursor — no known head — dims nothing at all). The emitter scales the
// whole clearance by the momentum spine and never lets it cull a cell — see
// the application site in `emit_rainbow` for both bounds.
// ---- THE LIGHT-THEME INK RECIPE ---------------------------------------------
//
// One pair of numbers behind every source-over mark the rainbow kitty draws on
// a white ground — the rails, the light twinkle, the meteor streak, the landing
// stars, the delete sparkles. They were spelled inline at each site as a bare
// `0.28` and a bare `150`, and a white-ground capture showed the result: every
// transient read as PALE PASTEL CONFETTI, because a saturated hue only 28 % of
// the way to black, composited at ~40 % centre alpha and then run through
// `push_halo_over`'s radial falloff, lands a handful of counts off white
// everywhere but the exact centre pixel.
//
// Naming them makes the light arms one system and lets the whole family gain
// contrast in one place.
/// Ceiling on the LUMA (0..255, Rec. 709) of any hue composited source-over on
/// a white ground.
///
/// A flat "mix `x` toward black" — which is what every light arm used to spell
/// inline — is the wrong operator here, because the seven bands do not start
/// anywhere near the same brightness: pure yellow's luma is 226 and pure blue's
/// is 32, so ONE mix ratio leaves yellow and green tinting the paper while red
/// and violet stain it. Normalizing to a luma target instead darkens each band
/// by whatever IT needs, so a rainbow of light-theme marks reads as one
/// material. 92/255 keeps every band a clear contrast INCREASE against white
/// while leaving enough headroom that the hue is still obviously the hue.
/// LOWERED 92 -> 68 after a white-ground review: at 92 the warm middle of the
/// spectrum (yellow, tan, yellow-green) still composited to within a few counts
/// of the paper, so a rainbow drawn on white visibly THINNED OUT in its middle
/// while its violet/blue end stayed strong — "the ribbon looks like it has a
/// hole in it". The bar has to be set by the WORST band, not an average one.
pub(super) const LIGHT_INK_MAX_LUMA: f32 = 68.0;
/// Companion ceiling on any SINGLE channel. Luma alone is not enough: pure red
/// has a luma of only 54, so a luma bar leaves it at a full-blast `FF` in the
/// red channel — and the fresh-ink veil, which composites OVER the glyph, would
/// then tint near-black ink to a dark red instead of merely greying its
/// surround. Bounding the channel too keeps every band's darken honest against
/// the INK as well as against the ground.
pub(super) const LIGHT_INK_MAX_CHANNEL: f32 = 150.0;
/// LEGIBILITY CEILING on any light-theme mark's per-pixel CENTRE over-alpha
/// (`aterm_render::halo_over_cap`). Raised 150 → 190: at 150 the transients
/// were bounded well under what legibility actually requires, and the bound
/// that matters is the one this constant now states — `over_rgb(glyph, ink,
/// cap)` keeps `(255−cap)/255` of the glyph, so near-black ink stays near-black
/// and a white counter stays a light grey. Pinned by
/// `light_ink_cap_keeps_glyphs_legible`.
pub(super) const LIGHT_INK_ALPHA_CAP: f32 = 190.0;

/// Rec. 709 luma of a `0x00RRGGBB` colour, 0..255. The one luminance the light
/// arms measure with.
#[inline]
pub(super) fn luma709(rgb: u32) -> f32 {
    0.2126 * ((rgb >> 16) & 0xff) as f32
        + 0.7152 * ((rgb >> 8) & 0xff) as f32
        + 0.0722 * (rgb & 0xff) as f32
}

// ---- THE ONE LIGHT-INK RULE ------------------------------------------------
//
// There are exactly TWO darkening policies on a light ground, and which one a
// mark takes is decided by its COMPOSITING ROLE — never by which emitter
// happens to draw it, and never twice inside one event:
//
//   * [`light_ink_bold`] — the LEADING-ONLY role. Marks that provably land in
//     the inter-line leading and therefore never composite over a glyph: the
//     rail, the rail-riding streak, the meteor nucleus, the landing bloom, and
//     the fresh-ink bar's tint. Vivid, because nothing they darken is ink.
//   * [`light_ink`] — the MAY-TOUCH-TEXT role. Everything else: every star,
//     sparkle and dot ([`push_twinkle_over`] applies it INTERNALLY, so a star
//     caller cannot get this wrong), the erase-poof grains, and the cursor
//     cat's exit flourish. Conservative, because any of them can land on a
//     letterform at any moment.
//
// The rule has to be stated because the two are NOT interchangeable: bold sits
// at [`LIGHT_INK_BOLD_MAX_LUMA`] and conservative at [`LIGHT_INK_MAX_LUMA`], a
// ~1.8x difference in ink weight. Composite both inside ONE landing and half
// the event reads visibly washier than the other half, which is precisely what
// a white-ground capture of a landing burst showed. Pinned by
// `one_light_ink_policy_per_compositing_role`.

/// THE LEADING-ONLY INK — vivid, for marks that provably never touch a glyph.
///
/// [`light_ink`] is deliberately conservative because its marks CAN land on
/// text: it bounds the channel hard, which on white leaves the low channels
/// showing enough paper through to read as grey. Three white-ground reviews
/// converged on the same sentence — "the palette collapses on white",
/// "desaturated olive/gray smear" — and this is the arithmetic behind it.
///
/// The marks that live wholly in the inter-line leading are under no such
/// constraint, and they are the ones that carry the rainbow. They get:
///
///   * A SINGLE INK WEIGHT — every hue lands ON [`LIGHT_INK_BOLD_MAX_LUMA`], so
///     a sweep that rolls the whole spectrum under a moving cursor keeps ONE
///     weight instead of pulsing light and dark as the band changes.
///   * THE MOST SATURATION THAT WEIGHT ALLOWS, so the low channels go as close
///     to zero as the bar permits instead of letting paper through them. This
///     is what separates "green" from "grey-green" once composited.
///
/// THE BAR NOW BINDS FOR ALL SEVEN BANDS. The retired recipe saturated to S = 1
/// and then only ever SCALED DOWN — `k = min(luma_bar/y, channel_bar/hi, 1)` —
/// so the bar could bind only for a hue whose own luma was already ABOVE it.
/// At S = 1 that is true of orange, yellow and green and false of red, blue and
/// violet, whose pure luma sits under the bar and can never be scaled UP to it
/// without clipping past `FF`. The light underline therefore changed apparent
/// weight with hue, as though drawn with different pens.
///
/// A hue below the bar can only be lifted to it by letting some white back in,
/// so the recipe now solves for the pair `(S, V)` that hits the bar with the
/// MAXIMUM saturation — scaling alone where that suffices (orange, yellow and
/// green are byte-identical to the retired recipe) and backing saturation off
/// only as far as the bar demands (red, blue, violet). That is the honest
/// generalization of "one bar": the bar governs the weight, and saturation gets
/// everything the bar has left over.
pub(super) const LIGHT_INK_BOLD_MAX_LUMA: f32 = 120.0;
pub(super) const LIGHT_INK_BOLD_MAX_CHANNEL: f32 = 215.0;

/// [`light_ink`]'s vivid twin — see the constants above and THE ONE LIGHT-INK
/// RULE. Normalizes `rgb` to its PURE hue (achromatic floor removed, peak
/// renormalized to 1), then re-mixes it at the `(S, V)` that lands exactly on
/// [`LIGHT_INK_BOLD_MAX_LUMA`] with as much saturation as
/// [`LIGHT_INK_BOLD_MAX_CHANNEL`] leaves available. Channel ORDER is preserved,
/// so every band is still recognisably its own colour. Pinned by
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
    // THE PURE HUE: `lo` is the white this colour was carrying; removing it and
    // renormalizing gives the same hue at S = 1, V = 1 (peak channel exactly 1).
    //
    // AN ACHROMATIC INPUT IS NOT BLACK. `r == g == b` leaves a zero span, and
    // the retired recipe divided by `max(span, 1e-3)` and multiplied by `hi` —
    // which drove every channel of a WHITE input to 0, i.e. `light_ink_bold`
    // silently returned BLACK for the single most common "invisible on paper"
    // hue there is. A grey has no hue to saturate, so it simply keeps its
    // ratios (all 1) and takes the bar as a neutral grey.
    let span = hi - lo;
    let pure = |c: f32| {
        if span <= 1e-3 { 1.0 } else { (c - lo) / span }
    };
    let (pr, pg, pb) = (pure(r), pure(g), pure(b));
    // The pure hue's OWN luma, 0..1 — the whole reason a single scale factor
    // could never reach the bar for red (0.21) or violet (0.13).
    let y0 = 0.2126 * pr + 0.7152 * pg + 0.0722 * pb;
    let yt = LIGHT_INK_BOLD_MAX_LUMA / 255.0;
    let vmax = LIGHT_INK_BOLD_MAX_CHANNEL / 255.0;
    // At value `v` and saturation `s` a channel is `v·(1 − s + s·c)` and the
    // luma is `v·(1 − s + s·y0)`. Luma falls with `s` (for `y0 < 1`), so the
    // most saturated solution takes `v` as high as the channel bar allows and
    // then solves for `s`. `s ≥ 1` means scaling alone reaches the bar — the
    // retired behaviour, kept byte-for-byte for the warm middle.
    let s = if y0 >= 1.0 - 1e-4 {
        0.0 // achromatic: nothing to desaturate
    } else {
        ((1.0 - yt / vmax) / (1.0 - y0)).clamp(0.0, 1.0)
    };
    let v = (yt / (1.0 - s + s * y0)).min(vmax);
    let ch = |c: f32| ((255.0 * v * (1.0 - s + s * c)) + 0.5).clamp(0.0, 255.0) as u32;
    (ch(pr) << 16) | (ch(pg) << 8) | ch(pb)
}

/// THE LIGHT-THEME INK: `rgb` scaled down (hue and saturation intact — every
/// channel by the SAME factor) until it satisfies BOTH bars —
/// [`LIGHT_INK_MAX_LUMA`] and [`LIGHT_INK_MAX_CHANNEL`]. A colour already under
/// both is returned UNCHANGED, so the dark bands are not needlessly crushed.
/// Scaling uniformly (rather than mixing toward black per-channel) is what
/// preserves the hue: the band is still recognisably its own colour, just at
/// ink weight. Pinned by `light_ink_darkens_every_band_to_one_bar`.
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

/// THE COMPOSITING ROLE of one light-theme mark — see THE ONE LIGHT-INK RULE
/// above. It is the ONE thing a light arm declares, and it picks BOTH halves of
/// the recipe: the darkening policy and the centre over-alpha ceiling. Those
/// two always agreed in intent and did not always agree in code — the flying
/// shower's streak asked for the conservative CEILING while its ink came out of
/// the leading-only policy, so a mark that can cross a glyph at any moment was
/// drawn in the vivid ink reserved for marks that provably cannot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InkRole {
    /// Provably confined to the inter-line leading (the rail line): nothing it
    /// darkens is ever a glyph, so it may be vivid and go to the rail's ceiling.
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

    /// This role's per-pixel CENTRE over-alpha ceiling.
    #[inline]
    pub(crate) fn alpha_cap(self) -> f32 {
        match self {
            Self::Leading => RAINBOW_LIGHT_RAIL_ALPHA_CAP,
            Self::OverText => LIGHT_INK_ALPHA_CAP,
        }
    }
}

// ---- rainbow kitty LIGHT-THEME RAILS ------------------------------------------------
/// THE ONE SWEEP. Where a cell sits on the spectrum, given its column and the
/// ribbon's own phase clock — the single source every mark that lands on the
/// light rail line reads.
///
/// The rail, fresh-ink bar, and glyph tint once hand-rolled this expression and
/// could disagree. Centralizing the sweep keeps each caller on the same
/// reflected coordinate law.
///
/// IT PING-PONGS. The seven bands are an ACYCLIC ramp (red at one end, violet at
/// the other), so wrapping the position with `rem_euclid` walks violet
/// straight back into red and prints a hard seam every `1 / SPREAD` ≈ 22
/// columns. Reflecting instead — a triangle wave — runs red→violet→red with no
/// discontinuity anywhere, which is what a spectrum drawn on a line should do.
///
/// THE CARET READS IT TOO. [`crate::cursor_rainbow`]'s block cursor used to run
/// its own HSV wheel on its own clock, so the caret and the ribbon leaving it
/// were literally two different rainbows meeting at one cell. It now resolves
/// its hue through this sweep at its OWN column, so the block and the underline
/// under it are the same colour.
#[inline]
pub(crate) fn rainbow_sweep_at(col: u16, phase: f32) -> f32 {
    rainbow_sweep_reflect(col as f32 * RAINBOW_LIGHT_RAIL_SPREAD + rainbow_sweep_phase(phase))
}

/// Fold an unbounded spectrum position onto the sweep's PING-PONG `0..1` —
/// the reflection [`rainbow_sweep_at`] is built from, exposed so a caller that
/// wants a spectrum position OFFSET from a resolved one (the caret's halo rings
/// step outward along the wheel) folds it the one way this family folds.
#[inline]
pub(crate) fn rainbow_sweep_reflect(x: f32) -> f32 {
    let raw = x.rem_euclid(2.0);
    if raw <= 1.0 { raw } else { 2.0 - raw }
}

// THE CONTINUOUS DOOR IS GONE. `rainbow_spectrum_of` was a one-line wrapper on
// [`spectrum`], and once the caret moved to the thing-arc its last caller was
// `cursor_rainbow`, which is the one module that must NOT resolve the raw
// gradient. Streaming BANDS — the ribbon, the plume, the jump path — call
// `spectrum` directly, in this file, where the position they read it at is
// resolved. A door that only a thing walked through is a door in the wrong wall.

/// Resolve a reflected spectrum position for a persistent mark — the caret's
/// read of the same field the ribbon paints, past the cycle's turnaround.
#[inline]
pub(crate) fn rainbow_thing_of(sweep: f32) -> u32 {
    // THE CARET WEARS THE CYCLE, NOT THE OPEN ARC. This is the caret's and the
    // lone cell's read of the same field the ribbon paints, and the field walks
    // past violet now; clamping here would park the caret on violet while the
    // band behind it kept advancing — the very split the "one field, one cell"
    // law exists to forbid. Identity on `[0, 1]`, so every arc reading is the
    // byte it always was.
    let phase = rainbow_cycle_phase(sweep);
    if phase > 1.0 {
        return rainbow_cycle_ink(phase);
    }
    spectrum(phase)
}

/// **THE DWELL IS OFF — the owner, 2026-08-30: "NO we don't want 'dwell' at
/// the end … make it a fucking rainbow."** The sine re-pacing below parked
/// 8-12 cells within ~20° of red or violet at every turnaround, and a resumed
/// mark picked the walk up AT an end — the "flat red bar" the A/B judges
/// ranked first. At `0.0` the fold is the pure triangle: every keystroke
/// advances the arc at the mark's own solved rate, everywhere.
///
/// Blue and violet are still REACHED without it (the 2026-08-29 "yes we need
/// blue and violet" ask): [`RAINBOW_TRAVERSE_PER_MARK`] `< 1` puts a
/// turnaround inside every mark, and [`RAINBOW_LAID_END_PULL`] lands the
/// nearest lay ON the anchor. What the dwell bought on top of that was
/// LINGERING at the ends, and lingering is what read as monochrome.
///
/// (The original derivation, kept for the record: a monotone re-pacing
/// `x − a·sin(2πx)/2π` hands more cells to the arc's two ends and fewer to
/// its middle; every derived bound below is parametric on the constant, so
/// `0.0` degrades each to the bare-triangle reading.)
pub(super) const RAINBOW_END_DWELL: f32 = 0.0;

// ---- THE ONE VISIBLE FIELD (§2.1) -----------------------------------------
//
// Every layer reads a spark's cached, run-local classic position. Reflowing a
// run keeps one red→violet geometry across its live cells; disjoint old marks
// retain independent fields. The rolling hue, absolute-column sweep and hashes
// remain only as ancillary clocks or fallbacks, never competing colour laws for
// a live kitty mark.
//
// After this there is one field and everyone reads it, so "which rainbow is
// this?" is not a question anyone can ask.

// THE FIELD THE CARET IS ABOUT TO LAY — the position the next keystroke will
// stamp into the cell it echoes into.
//
// `rainbow_field_next` — the rolling-hue forecast of the caret's next lay —
// is GONE: the classic field re-anchored the caret's read to the mark's own
// geometry (owner, 2026-08-30), the navigation gate now pins the fresh caret
// to the arc's start directly, and with its last test caller rewritten the
// forecast had no reader left. The `+ 0.5` stamp identity it documented lives
// on in the emitter's own `(self.hue + pos * 0.5).fract()` and its tests.

/// THE ONE BAND. The named stop nearest a sweep position — the quantized
/// spectrum every point-mark of this family snaps to (§2.3.3).
///
/// Quantized rather than continuous, deliberately: a discrete band is what
/// makes red and violet nameable on a thin underline, and `is_fresh_ink_veil`
/// needs the pop's colour to come from a finite set to stay independently
/// assertable. What changed is *which* finite set — the seven stops of the one
/// spectrum, resolved through [`spectrum_snap`], rather than six anchors this
/// file owned.
#[inline]
#[cfg(test)]
pub(crate) fn rainbow_band_of(sweep: f32) -> u32 {
    spectrum_snap(sweep)
}

/// [`rainbow_band_of`] at a column and phase. Live kitty marks read the cached
/// classic field; this composition remains only for compatibility proofs.
#[cfg(test)]
#[inline]
pub(crate) fn rainbow_band_at(col: u16, phase: f32) -> u32 {
    rainbow_band_of(rainbow_sweep_at(col, phase))
}

/// LEGIBILITY CEILING on a rail's per-pixel centre over-alpha. The rails live
/// outside the glyph band, so this bounds how dark the LEADING can get (and how
/// much two overlapping rails can compound), not the ink.
/// RAISED 150 -> 200 across two white-ground reviews. The rail is the ONE
/// continuous mark of this family on white — the "underline cursor trail" — and
/// at the old ceiling it read as a pastel wash rather than a rainbow. It may go
/// higher than [`LIGHT_INK_ALPHA_CAP`] precisely because it is NOT a transient
/// over text: it lives wholly in the leading below the row (`DY − RY` = 0.49,
/// clear of any descender), so nothing it darkens is ever a glyph.
pub(super) const RAINBOW_LIGHT_RAIL_ALPHA_CAP: f32 = 236.0;

/// How fast the band ramp sweeps ALONG the rails: per column, and per unit of
/// the ribbon's own shared phase clock. Slow — the spectrum should flow, not
/// strobe — and phase-driven, so it freezes exactly when the ribbon does.
pub(super) const RAINBOW_LIGHT_RAIL_SPREAD: f32 = 0.045;
/// The integrated rainbow clock lives on this exact ring. Every consumer must
/// complete a whole number of its own cycles across the ring so wrapping can
/// never change a visible frame.
pub(crate) const RAINBOW_PHASE_RING: f32 = 1024.0;
/// 179 complete reflected (period-two) spectrum sweeps per phase ring. This is
/// only 0.11 % slower than the former 0.35, but makes the shared wrap exact.
pub(super) const RAINBOW_LIGHT_RAIL_FLOW: f32 = 179.0 / 512.0;

/// Spectrum-clock position on the reflected sweep's period-two domain.
#[inline]
pub(super) fn rainbow_sweep_phase(phase: f32) -> f32 {
    (phase.rem_euclid(RAINBOW_PHASE_RING) * RAINBOW_LIGHT_RAIL_FLOW).rem_euclid(2.0)
}

/// Lift a standalone unit-turn animation clock onto the rainbow family's phase
/// ring so one `0..1` turn traverses one WHOLE reflected spectrum cycle.
///
/// [`rainbow_sweep_at`] consumes the ribbon's `0..1024` phase domain, not unit
/// turns. Passing a unit clock through unchanged advances only
/// [`RAINBOW_LIGHT_RAIL_FLOW`] (about 0.35) of the period-two sweep and then
/// jumps backward when that clock wraps. Dividing the complete period-two
/// excursion by the family's flow preserves the one resolver while making the
/// standalone wrap exact: turn `0` and turn `1` both resolve to sweep position
/// zero, approached continuously from either side.
#[inline]
pub(crate) fn rainbow_phase_from_unit_turn(turn: f32) -> f32 {
    ((turn.rem_euclid(1.0) * 2.0) / RAINBOW_LIGHT_RAIL_FLOW).rem_euclid(RAINBOW_PHASE_RING)
}

// THE WAKE'S OWN PERSISTENCE IS DELETED TOO (`RAINBOW_WAKE_PERSIST = 0.30`,
// retired 2026-09-16 with `GlowConfig::wake_persist_s`). It was the default
// that dial carried — "the plume shows the last ~0.3 s of travel" — and after
// §17.3 phase 7 deleted the walk that replayed a travel window it was a
// default for nothing: every construction site in the tree wrote it into a
// field no frame read. What decides how much recent typing you see today is
// `Ribbon::cell_life` (priced from `cfg.duration`, floored at the phrase rest)
// — see the tombstone on `GlowConfig` above.
// THE WAKE'S OWN SPATIAL RATE IS DELETED (§2.1). `RAINBOW_WAKE_SWEEP_SPREAD`,
// `RAINBOW_WAKE_SWEEP_TRAVEL`, `RAINBOW_WAKE_SWEEP_MIN` and
// `rainbow_wake_sweep_spread` gave the plume a spectrum of its own, resolved at
// a rate DERIVED PER FRAME from the plume's reach — `2.25 / reach_cells`, a 25x
// range, and therefore a function of typing momentum.
//
// The whole apparatus was solving a problem the field does not have. Its own
// doc records both horns: at a fixed 0.50 a hot 27.5-cell plume ping-ponged the
// spectrum 6.9 times (owner: "it's more of just arbitrary colors?"), and at
// v0.43's 0.15 a resting plume was nearly monochrome. Both are symptoms of the
// plume owning a second spectrum: it had to pick a rate, and no rate serves both
// ends of its own length range.
//
// Reading the FIELD removes the choice. The plume's colour at a column is the
// colour of the cell it covers — the same cell the ribbon above it is drawing —
// so a long plume shows exactly the spectrum the text under it was typed in,
// never a repeat and never a smear, at any cadence, with nothing to tune.

// THE LEADING IS NOT BEHIND LETTERFORMS, AND IT STOPS PAYING AS IF IT
// WERE. `RAINBOW_BAND_COV_CAPS` is a ceiling on the light INK can be read
// over — solved for the tall body's six slabs under the typed glyphs — and the
// hybrid inherited it by sharing `rainbow_band_coverage`, so the strip in the
// leading, the one part of the mark with no ink on it, was held to yellow's
// `57` and composited to olive (`#4B4C12`, V `0.30`) between a brighter red
// and blue. That is the dark middle third of every ribbon the owner reported
// as *"black gaps in the underline"* and *"weird cutoffs in brightness"*
// (2026-08-29), and it is what §4's zone table already forbids: *"Row
// boundary ± the leading: the ribbon body — full strength."*
//
// So the strip carries a LIFT above its ink ceiling
// (`aterm_render::RibbonVertex::lift`) — since 2026-08-30 a RATIO of the
// cell's own equalized ink rather than a flat level, and no longer starved
// by the falloff ledge budget: see `RAINBOW_STRIP_GAIN` and
// `rainbow_spine_lift`. (The retired flat target `RAINBOW_SPINE_LEVEL =
// 168` and its `RAINBOW_LEDGE_STEP_BUDGET = 34` pricing are recorded there.)

/// Compatibility bound for the retired laid-hue lattice. The live kitty field
/// is instead bounded by [`RAINBOW_CLASSIC_UNROLL`] and cached in
/// [`Spark::classic_t`]; this constant remains for the ancillary lattice's
/// compile-time checks and negative controls.
pub(super) const RAINBOW_UNDERLINE_SWEEP_MAX: f32 = 0.22;

/// Reference step for the ancillary rainbow-kitty rolling hue clock. It still
/// sequences per-spawn hue data and compatibility state, but it is not the
/// visible ribbon's spatial rate; classic geometry supplies that rate.
pub(super) const RAINBOW_KITTY_HUE_STEP: f32 = 0.0125;

/// **THE SHORTEST TRAVERSE, IN CELLS.** A mark only a few cells long must not
/// be handed a few-cell traverse: the per-cell hue step is `1 / T` of the arc
/// and the arc's steepest stretch spends `1420°` per unit of traverse, so a tiny
/// `T` walks the green→blue crossing in one cell and prints a band edge. At `26`
/// the worst per-cell step is `55°`, about `3.6°` per device pixel at the shipped
/// cell width — inside the per-pixel ceiling §8's acceptance measures, and the
/// adjacency ceiling [`RAINBOW_UNDERLINE_SWEEP_MAX`] is checked against it below
/// at compile time.
///
/// It is also the floor a GROWING mark lays at: the first cells of a burst are
/// laid while the mark is two or three cells long, and they should carry the
/// spectrum of the mark they are about to be part of.
pub(super) const RAINBOW_TRAVERSE_MIN_CELLS: f32 = 26.0;

/// The short mark's traverse, in cells — `1/16` of the arc per cell, the
/// value [`RAINBOW_CLASSIC_UNROLL`] resolves to. Sized against four gates at
/// once: a FIVE-char word's last cell reaches yellow (`4/16 = 0.25` of the
/// arc → hue 45°, five degrees past the yellow bin's edge — the sentinel's
/// red→yellow short-word bar runs on "hello", and the five-key law pins the
/// traverse at `<= ~17.3` from above, NOT the `~18.5` a six-char reading
/// once claimed: at 18 "hello" topped at 37.8° and the bar read 4/5); the
/// per-cell lay stays well under the adjacency ceiling (checked at compile
/// time below); the crossing transit moves `1420°/16 ≈ 89°` per cell ≈
/// `5.9°`/device px — inside §8's ~10°/px smoothness ceiling; and a 4-key
/// line's head (`3/16 ≈ 0.19`) stays clear of the crossing.
///
/// **`14 -> 18 -> 16` (owner, 2026-08-30: "I don't like that the rainbow is
/// advancing as fast as I'm typing. I'd like to advance slower").** The
/// unroll IS the advance rate the eye reads at the caret while a line is
/// young: one key moves the head `282°/T` of hue at any cadence. At `14`
/// that was `20.1°` per keystroke — at 90 ms the head visibly churned
/// through a named colour every three keys. `18` was calmer still
/// (`15.7°`) but broke the five-key word's red→yellow floor; `16` is the
/// calmest traverse the five-key law admits — `17.6°` per key, violet at 17
/// cells, and past the unroll the walk keeps slowing as the span grows (the
/// classic stretch).
pub(super) const RAINBOW_SHORT_TRAVERSE_CELLS: f32 = 16.0;

// THE SHORT WALK'S STEP obeys the same adjacency law as the clamp's: one
// typed cell may never lay more than [`RAINBOW_UNDERLINE_SWEEP_MAX`] of the
// spectrum, dwell re-pacing included.
const _: () = assert!(
    (1.0 + RAINBOW_END_DWELL) / RAINBOW_SHORT_TRAVERSE_CELLS + RAINBOW_LAID_END_PULL
        <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "the short walk's step must stay under the adjacency ceiling"
);

/// **THE SETTLED TRAVERSE, IN CELLS** — what one red→violet pass costs once a
/// line is long, and the whole of the owner's 2026-08-31 ruling: *"I think we
/// need to slowly advance the lead color as typing we dont' want to be stuck on
/// purple do you understand?"*
///
/// **THE DEFECT THIS REPLACES, MEASURED ON GLASS.** The classic field used to
/// STRETCH one arc to fit the mark — [`CursorGlow::rainbow_classic_t`] divided
/// by `span.max(`[`RAINBOW_CLASSIC_UNROLL`]`)` — so the head's own position was
/// `span / span` the instant the mark outgrew the unroll: pinned at `1.0`,
/// violet, forever. Filmed on an isolated 24x100 Nord instance typing 100 keys
/// at 85 ms (`ctl image` per key, head cell read as composited HSV): head hue
/// `277.3°, 277.8°, 277.6°, 278.4°, 282.4°` at keys 20/40/60/80/100 — **`5.1°`
/// of travel across eighty keystrokes, `0.06°` per key.** Every one of those
/// eighty keys laid the same purple. That is what the owner is seeing, and no
/// amount of re-pacing a stretched arc can fix it: a traverse that is defined
/// as the mark's own length cannot advance relative to the mark.
///
/// So the traverse stops being a function of the mark and becomes a LENGTH. The
/// head then walks red→…→violet→red→… for as long as the typing lasts and never
/// parks, because the walk's rate is bounded below by `1 /` this number.
///
/// **WHY `36` AND NOT THE UNROLL'S `16`.** The unroll's rate is `282°/16 =
/// 17.6°` per key, and the owner has already ruled on it once from the other
/// side (2026-08-30: *"I don't like that the rainbow is advancing as fast as
/// I'm typing. I'd like to advance slower"*). The eye reads the arc in NAMES —
/// seven of them over `282°`, so a named colour is `40.3°` wide. At `17.6°/key`
/// a name changes every `2.3` keys, which at 90-110 ms cadence is a new colour
/// four times a second: churn, which is the complaint. At `36` the rate is
/// **`7.83°` per keystroke**, a name every `5.1` keys — about **half a second
/// per named colour**, and a full red→violet pass in `36` keys, i.e. **`3.2 s`
/// at 90 ms and `4.0 s` at 110 ms**. That is the "slow, deliberate sweep, order
/// several seconds" the ruling asks for, and it is `2.25x` slower than the rate
/// the owner called too fast.
///
/// **AND WHY IT MAY NOT GO MUCH HIGHER.** The bound was the green→blue
/// crossing: on the retired arc it was far and away the steepest leg — `1420°`
/// per unit of traverse, spending `1420/T` per cell — and a census kept its
/// desaturated seam to ONE ribbon slab for traverses `26..=72`, two at `80` and
/// beyond. `36` sits in the middle of that range with room on both sides.
/// **THAT SEAM NO LONGER EXISTS** (2026-09-15): the crossing's authored roof,
/// its saturation taper and its exemption from the spectrum's perceptual pace
/// were the retired no-cyan ruling's machinery and are deleted, so the crossing
/// is drawn at full chroma and spent at the same pace as every other leg — it
/// is no longer the arc's steepest, and there is no pale seam for a traverse to
/// widen. The number stands on the ruling above it (the rate the owner asked
/// for) rather than on a crossing bound that has gone quiet.
pub(super) const RAINBOW_TRAVERSE_SETTLED: f32 = 36.0;

/// **THE WRAP, IN CELLS** — violet back to red, the one adjacency the arc has
/// never had, and the price of a head that keeps advancing.
///
/// A periodic hue MUST close somewhere. The arc's own ends are violet
/// (`#9400D3`, `282°`) and red (`#FF0000`, `0°`), and the short way between them
/// runs UP through magenta and rose — `282° → 328° → 360°` — which is `78°` of
/// hue that never approaches the cyan window `[165°, 200°]` and never loses
/// saturation (both ends have `G = 0`, and so does every point of the lerp
/// between them, so `S = 1.0` across the whole leg).
///
/// **THE LENGTH IS DERIVED, NOT CHOSEN: the wrap moves at exactly the crossing's
/// own rate.** The green→blue crossing is the fastest transit the arc already
/// contains and the only one the design already accepts and gates — `1420°` per
/// unit of traverse, i.e. `1420/36 = 39.4°` per cell at the settled traverse.
/// Asking the wrap to move no faster than that gives `78° / 39.4° = 1.98` cells.
/// `2` is that number. The wrap is therefore not a cliff and not a new kind of
/// event: it is a second crossing, the same speed as the first, in a part of the
/// circle with no cyan in it.
pub(super) const RAINBOW_WRAP_CELLS: f32 = 2.0;

/// The wrap as a SHARE of the arc, which is the unit the walk is stated in.
///
/// Expressed as a share rather than a cell count it stays at the crossing's rate
/// at EVERY local rate the walk takes: at the unroll's `1/16` the wrap is `0.89`
/// cells and spends `88°` there, against the crossing's own `1420/16 = 89°` — the
/// same equality that sized it, holding at the other end of the walk.
pub(super) const RAINBOW_WRAP_SHARE: f32 = RAINBOW_WRAP_CELLS / RAINBOW_TRAVERSE_SETTLED;

/// One full turn of the spectrum in arc units: the red→violet arc (`1.0`) plus
/// the wrap that closes it. [`rainbow_cycle_phase`] folds by this.
pub(super) const RAINBOW_CYCLE: f32 = 1.0 + RAINBOW_WRAP_SHARE;

// THE WALK NEVER PARKS: its rate is bounded below by the settled traverse, so
// one more typed cell always moves the head. A zero here would be the defect
// this law replaces, spelled as a constant.
const _: () = assert!(
    RAINBOW_TRAVERSE_SETTLED > 0.0 && RAINBOW_TRAVERSE_SETTLED.is_finite(),
    "the settled traverse is the walk's lower bound on advance; it may not be zero"
);
// THE WRAP IS NO FASTER THAN THE CROSSING, which is the derivation above stated
// as an inequality the compiler checks: the crossing spends `1420/T` per cell,
// the wrap `78/W`, and the wrap may not be the steeper of the two.
const _: () = assert!(
    78.0 / RAINBOW_WRAP_CELLS <= 1420.0 / RAINBOW_TRAVERSE_SETTLED,
    "the wrap may not move faster than the green-blue crossing it is paced against"
);

/// The walk folded into one turn of the spectrum: `[0, 1]` is the red→violet
/// arc, `(1, `[`RAINBOW_CYCLE`]`)` is the wrap that closes it.
#[inline]
pub(super) fn rainbow_cycle_phase(p: f32) -> f32 {
    // THE FAST PATH IS THE COMMON PATH. Every short mark, every caret read at
    // rest and every arc-domain caller hands this a number already inside one
    // turn, and the hot ribbon calls it per slab per vertex — `rem_euclid` is a
    // library `fmod` and it showed up as tens of microseconds a frame on the
    // worst-case bench. One compare answers the common case; the general case is
    // a multiply, a floor and a fused subtract rather than a call.
    if (0.0..RAINBOW_CYCLE).contains(&p) {
        return p;
    }
    if !p.is_finite() {
        return 0.0;
    }
    let folded = p - RAINBOW_CYCLE * (p * RAINBOW_CYCLE_INV).floor();
    // The subtraction can land exactly on the period through rounding; the
    // domain is half-open, so fold that one value down rather than out. Spelled
    // as the same `contains` the fast path above uses, which is exact here
    // rather than merely tidy: the two forms are De Morgan duals only when NaN
    // is absent, and it is — `p` was proved finite three lines up, and the only
    // way this expression reaches NaN is `inf - inf`, which needs an infinite
    // `p`. An overflow to `-inf` lands outside the range either way.
    if !(0.0..RAINBOW_CYCLE).contains(&folded) {
        return 0.0;
    }
    folded
}

/// Reciprocal of [`RAINBOW_CYCLE`], so the fold is a multiply.
pub(super) const RAINBOW_CYCLE_INV: f32 = 1.0 / RAINBOW_CYCLE;

/// **THE CYCLE'S COLOUR.** `spectrum` on the arc, and the wrap leg — violet to
/// red the short way, through magenta — past it.
///
/// The join is exact at both ends and needs no easing: `spectrum(1.0)` IS the
/// violet anchor `#9400D3` and `spectrum(0.0)` IS the red anchor `#FF0000`
/// (the seven anchors are stored verbatim, wherever the pace put them —
/// [`crate::spectrum::SPECTRUM_ANCHOR_AT`]), so this lerp starts and
/// finishes on the same bytes the arc does. The leg holds `G = 0` throughout,
/// so it is fully saturated at every point and cannot enter the cyan window.
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

/// **THE LONGEST TRAVERSE, IN CELLS.** A screen-crossing run at full tilt can
/// hold a mark of two hundred cells; letting the traverse follow it that far
/// would spread one arc so thin that a whole word is one colour. Beyond this the
/// mark simply carries more than one traverse again — which at that length is
/// what a rainbow trail dragged across two wrapped lines should look like.
///
/// It is ALSO the number [`RAINBOW_LAID_END_REACH`] is sized against, because
/// the slowest lay is the one whose cells sit closest together.
pub(super) const RAINBOW_TRAVERSE_MAX_CELLS: f32 = 144.0;

/// PHASER's own step, in turns per typed cell. `0.14` — the rate every style's
/// rolling hue used to turn at, doubled, so the fat beam visibly sweeps the
/// spectrum instead of dwelling on one hue. Named beside its sibling for the
/// same reason.
pub(super) const RAINBOW_PHASER_HUE_STEP: f32 = 0.14;

/// **THE ROLLING HUE STEP FOR EVERY OTHER STYLE** — `0.07`, unchanged since it
/// was the one shared number.
///
/// It exists as its own constant because [`RAINBOW_KITTY_HUE_STEP`] stopped
/// being a shared one. For the kitty that number is now a SPATIAL rate solved
/// against the ribbon's measured length; for sparkle, lumen, fire, laser, beam,
/// water and comet it is what it always was — how fast the colour a grain or a
/// wake is dealt turns as you type — and those styles have no ribbon whose
/// length could solve it. Slowing them by the kitty's factor would be a change
/// to seven styles nobody asked to change.
pub(super) const RAINBOW_ROLL_HUE_STEP: f32 = 0.07;

/// **THE LAY RATE**, in fractions of one red→violet traverse per typed cell —
/// what the spectrum's spatial pace along the trail is, in the units every claim
/// about the mark's colour is stated in.
///
/// The two constants above are a HUE step; this is what the fold makes of it, and
/// it is the number the adjacency ceiling and every per-cell pin are about.
/// `0.14`, unchanged: the laid fold's end-pull touches only the
/// outermost [`RAINBOW_LAID_END_REACH`] of the arc, so the pace between cells
/// anywhere else is exactly what it always was.
///
/// **IT IS THE PACE AT THE REFERENCE MARK, NOT AT EVERY MARK.** Since
/// [`RAINBOW_TRAVERSE_PER_MARK`] the lay rate is solved per keystroke against
/// the mark's own live length, so the pace on glass is `1 / T` for that mark's
/// `T` — this number is what that resolves to at the `40`-cell traverse the
/// fixed rate assumed, and it stays the unit every claim about the arc is
/// stated in. The two numbers a bound has to be taken against are
/// [`RAINBOW_LAID_SWEEP_MAX_PER_CELL`] (the fastest lay, through the dwell) and
/// [`RAINBOW_LAID_END_PULL`] (the slowest).
pub(super) const RAINBOW_LAID_SWEEP_PER_CELL: f32 = RAINBOW_KITTY_HUE_STEP * RAINBOW_LAID_HUE_SWEEP;

/// **THE FASTEST ONE KEYSTROKE CAN EVER LAY**, in fractions of a traverse — the
/// shortest traverse ([`RAINBOW_TRAVERSE_MIN_CELLS`]) walked at the steepest
/// point of the dwell's re-pacing. Every adjacency bound is taken against THIS,
/// not against the reference pace above, because the rate is now solved per
/// mark.
pub(super) const RAINBOW_LAID_SWEEP_MAX_PER_CELL: f32 =
    (1.0 + RAINBOW_END_DWELL) / RAINBOW_TRAVERSE_MIN_CELLS;

/// **THE SLOWEST**, likewise — the longest traverse walked at the dwell's
/// shallowest point, which is the arc's two ends. It is the gap between the two
/// cells nearest a turnaround in the worst case, so the end-pull's flat landing
/// zone must be narrower than this or two keystrokes could park on the same
/// anchor — the parked-lay defect that killed the zero-slope turnaround.
pub(super) const RAINBOW_LAID_SWEEP_MIN_PER_CELL: f32 =
    (1.0 - RAINBOW_END_DWELL) / RAINBOW_TRAVERSE_MAX_CELLS;

/// **HOW WIDE THE END-PULL'S LANDING IS** — strictly inside
/// [`RAINBOW_LAID_SWEEP_MIN_PER_CELL`], so at most ONE cell per turnaround can
/// resolve to the exact anchor however slowly the mark is being laid. The
/// compile-time clause below is what holds that.
pub(super) const RAINBOW_LAID_END_PULL: f32 = 0.003;

/// **HOW CLOSE TO AN END THE LATTICE CAN LAND WITHOUT BEING ON IT** — half of
/// one cell's lay, which is the whole of the violet defect.
///
/// A rolling hue advances [`RAINBOW_KITTY_HUE_STEP`] per typed cell and
/// [`rainbow_sweep_reflect`] turns around at `t = 1`. The lattice therefore
/// STRADDLES the turnaround rather than standing on it: the nearest cell to the
/// turn sits at most half a step away in hue, which is exactly this much of the
/// arc short of the end — and no nearer, which is what makes the pull below a
/// GUARANTEE rather than a nudge. Measured on glass, 52 traverse tops over 30
/// frames: median top hue `239.1°`, range `233.6..250.7`, and **not one reached
/// the violet anchor's `255°`** — a median `0.07` of the arc short, which is half
/// a lay, exactly as a uniform offset predicts.
///
/// The arc was never the problem: `spectrum(1.0)` is `#6633FF` and the table
/// stores it verbatim. The SAMPLING never landed on it.
///
/// **AND THE HALF-STEP IS NOW TAKEN THROUGH THE DWELL.** The lattice still
/// straddles the turnaround by half a lay, but that half-lay is measured in the
/// RAW triangle and the end-dwell re-pacing compresses it by `1 - a` on the way
/// out, so the shortfall on the arc is that much smaller. This is the worst case
/// over the whole clamp range — the FASTEST lay
/// ([`RAINBOW_TRAVERSE_MIN_CELLS`]), because a fast lay is the one whose cells
/// sit furthest apart — and it is still what every claim about landing on an
/// anchor is allowed to be wrong by.
pub(super) const RAINBOW_LAID_END_REACH: f32 =
    (1.0 - RAINBOW_END_DWELL) * 0.5 / RAINBOW_TRAVERSE_MIN_CELLS;

/// TURNS OF LAID HUE → SPECTRUM POSITION, the gain the laid fold applies.
/// One turn of the engine's rolling hue therefore paints one complete
/// red→violet→red ping-pong.
///
/// IT MUST BE AN EVEN INTEGER, and that is the whole reason it is a named
/// constant rather than a literal 2: [`rainbow_sweep_reflect`] has period 2, and
/// the engine's hue is kept `.fract()`ed to `0..1`, so only an even gain makes
/// the fold agree across the wrap. At 2 the hues 0.99 and 0.01 resolve to sweep
/// 0.02 and 0.02 — the same near-red — instead of stepping violet→red and
/// printing the magenta seam. At 4 the per-cell step (0.28) would also break
/// [`RAINBOW_UNDERLINE_SWEEP_MAX`]; 2 is the only value that satisfies both.
pub(super) const RAINBOW_LAID_HUE_SWEEP: f32 = 2.0;

// THE SUB-CELL RASTER GRAIN IS GONE WITH THE PER-CELL EMITTER.
// `RAINBOW_UNDERLINE_SAMPLE_PX` fixed the mark's horizontal spectrum at two
// device pixels, which is what a per-cell rasterizer needs to stop painting
// cell-wide colour blocks. The mark is one polyline now and its major-axis
// resolution is SOLVED against the quad budget by
// [`CursorGlow::rainbow_ribbon_stride`] (floored at
// [`CursorGlow::RAINBOW_RIBBON_SAMPLE_MIN`]), so a word gets four samples per
// cell and a screen-crossing ribbon gets one — a fixed grain can only be too
// coarse at one end of that range or too expensive at the other.

/// THE ADJACENCY BOUND, CHECKED BY THE COMPILER. A retune of the hue step or of
/// the fold's gain that pushed neighbouring cells further apart than
/// [`RAINBOW_UNDERLINE_SWEEP_MAX`] would let the mark lay red beside violet —
/// the one adjacency this family bans — and it would do so silently, at some
/// trail length nobody happened to capture. So it fails the BUILD instead: the
/// three constants are compile-time, and so is the relation between them.
/// **AND IT IS TAKEN AGAINST THE FASTEST LAY THE RATE LAW CAN SOLVE TO**, not
/// against the reference pace — since [`RAINBOW_TRAVERSE_PER_MARK`] the rate is
/// a function of the mark, so a retune of the CLAMP or of the DWELL is the
/// retune that could reach the banned adjacency, and both are in this clause.
const _: () = assert!(
    RAINBOW_LAID_SWEEP_MAX_PER_CELL + RAINBOW_LAID_END_PULL <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "one typed cell must step at most RAINBOW_UNDERLINE_SWEEP_MAX of the spectrum"
);

/// **THE LANDING IS NARROWER THAN THE SLOWEST LAY, CHECKED BY THE COMPILER.**
/// The end-pull collapses the outermost [`RAINBOW_LAID_END_PULL`] of the fold
/// onto the anchor. If two cells could ever fall inside that width, two
/// consecutive keystrokes would lay the SAME field — the parked lay that
/// `rainbow_ribbon_hue_advances_with_the_typed_text` bans and that killed the
/// zero-slope turnaround built before this. The cells sit closest together at
/// the slowest lay, at the dwell's shallowest point, which is exactly
/// [`RAINBOW_LAID_SWEEP_MIN_PER_CELL`] — so the relation is arithmetic and
/// belongs to the build.
const _: () = assert!(
    RAINBOW_LAID_END_PULL < RAINBOW_LAID_SWEEP_MIN_PER_CELL,
    "the end-pull's landing must be narrower than one lay at the slowest rate"
);

/// **THE REFERENCE PACE IS A PACE THE LAW CAN ACTUALLY RESOLVE TO.** Every claim
/// about this mark's colour — in the tests, in §8, in the notes above — is
/// stated in [`RAINBOW_LAID_SWEEP_PER_CELL`], and that only means anything while
/// the rate law can still produce it for some mark length. A retune of the clamp
/// that put `0.025` outside the reachable range would leave the whole vocabulary
/// describing a rate the engine never lays at. And the worst the fold's landing
/// can be wrong by, plus one lay at the fastest rate, still has to sit inside the
/// banned-adjacency ceiling.
const _: () = assert!(
    RAINBOW_LAID_SWEEP_MIN_PER_CELL <= RAINBOW_LAID_SWEEP_PER_CELL
        && RAINBOW_LAID_SWEEP_PER_CELL <= RAINBOW_LAID_SWEEP_MAX_PER_CELL
        && RAINBOW_LAID_END_REACH + RAINBOW_LAID_SWEEP_MAX_PER_CELL <= RAINBOW_UNDERLINE_SWEEP_MAX,
    "the reference pace must be reachable and its landing error inside the ceiling"
);

// `RAINBOW_BODY_CENTRE` IS DELETED. It named the tall body's vertical MIDPOINT —
// "the row of the red→violet stack a companion mark inherits its colour from" —
// so the emitter and the head-colour seam could agree on the colour there. A
// constant whose whole job was keeping two answers to "what colour is this cell"
// in step. A cell has one position now, read from one field, so there is no
// second answer to keep in step with, and no stack for a midpoint to be in.

// THE SHIMMER/RESERVATION TIE STOOD HERE AND IS MOOT ON THIS BRANCH.
//
// It was a `const _: () = assert!(MAGIC_ROOM * (1.0 + RAINBOW_IRID_AMP) <= 1.0)`,
// checked at compile time because the two constants lived 3 000 lines apart and
// the failure it prevented was silent: past `1 / (1 + IRID_AMP)` the SHIMMER's
// own crest clamped against the uniform ceiling before the glint had added
// anything, so the strip's brightest sub-sample pinned at the ceiling at every
// phase and both magic channels went arithmetically dead at the top of their
// range.
//
// The iridescence field is DELETED here — it was a ~2.7-cell lattice swinging
// ±30 % of each cell's coverage, i.e. the loudest per-cell brightness term in
// the emitter and a bead by construction — so there is no second magic channel
// left to reserve against. `RAINBOW_IRID_AMP` and `rainbow_irid_noise` do not
// exist in this crate, which is what makes the deletion safe rather than
// convenient: a tie has nothing to bind. The property it protected — that the
// surviving glint is live at the top of its range and still capped — is
// measured directly by `rainbow_underline_magic_channels_are_live_and_capped`.

// THE FILAMENT IS DELETED (step 4). A thin near-white nucleus was laid on the
// spine inside the coloured body — `lerp_rgb(band, white, 0.25)` — as a SIXTH
// treatment of the same six anchors: desaturated, off-anchor, drawn on top of an
// already-off-anchor lerp. §3 rules it out on the coherence count, and its own
// history is the corroboration: it was retuned twice (0.55 → 0.30 → 0.25)
// chasing the pink it manufactured over the red band, because desaturating a
// saturated hue toward white is how you make the colour the owner banned. The
// mark's one bright-above-body channel is the specular glint, and §4 ranks it
// alone at that job.
//
// THE SHOCK RINGS ARE DELETED (step 4). One expanding elliptical puff per live
// keystroke, timed a beat late. §3: invisible at typing speed and read as noise
// when you look for them — and every one of them was a full-strength band colour
// at a keystroke position, which is the "lots of colors" half of the owner's
// complaint. The cadence still reaches the plume: `beat` rides the same pulse
// ring into the core's weight AND its thickness swell, which is the "bigger
// impacts" the rings were added beside.

// ---- the WAKE's LIGHT-THEME arm ----

/// The contrast ratio the tint aims for against the theme's own ground: WCAG
/// AA for small text.
pub(super) const FRESH_INK_GLYPH_TARGET_RATIO: f32 = 4.5;
/// Below this ratio the theme has no usable side — an equiluminant or
/// near-invisible fg/bg pair — and the tint suppresses itself entirely rather
/// than inventing a side the theme does not have.
pub(super) const FRESH_INK_GLYPH_MIN_THEME_RATIO: f32 = 1.5;
/// The ink must BE a hue (max channel − min channel) and must visibly DIFFER
/// from the foreground it replaces, or the substitution buys nothing.
pub(super) const FRESH_INK_GLYPH_MIN_SPREAD: i32 = 24;
pub(super) const FRESH_INK_GLYPH_MIN_DELTA: i32 = 24;
/// Bisection depth on the one-parameter ink family. 24 halvings resolve `k` far
/// below the 1/255 quantization step, so the result is exact in u8 terms.
pub(super) const FRESH_INK_GLYPH_BISECT: usize = 24;

/// WCAG relative luminance of a `0x00RRGGBB` colour. The transfer is the
/// piecewise sRGB one (a linear toe below 0.03928, a 2.4 power above), NOT a
/// plain power law — the two disagree by enough near black to move a ratio
/// across the AA line.
#[inline]
pub(super) fn rel_luminance(c: u32) -> f32 {
    let ch = |sh: u32| {
        let v = ((c >> sh) & 0xff) as f32 / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

/// WCAG contrast ratio between two `0x00RRGGBB` colours. Named apart from
/// the test module's own long-standing `contrast_ratio` helper, which returns
/// `f64` and would silently shadow this one inside `mod tests`.
#[inline]
pub(super) fn wcag_ratio(a: u32, b: u32) -> f32 {
    let (x, y) = (rel_luminance(a), rel_luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Per-channel max / min of two colours, repacked. These are the corners of the
/// BOX the whole tint path lives in — see [`glyph_ink_bound`].
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
    /// Dark ink on a light ground — the tint mixes DOWN toward black.
    Dark,
    /// Light ink on a dark ground — the tint mixes UP toward white.
    Light,
}

/// THE BOUND. The side the theme's ink is on, and the luminance every point of
/// the tint path must satisfy — or `None` when this theme gets no tint at all.
///
/// WHY A LUMINANCE AND NOT A RATIO PER POINT. The tint is a per-channel lerp
/// from the foreground to the band ink, so every channel of every intermediate
/// colour lies in the BOX between the two endpoints' channels. `rel_luminance`
/// is monotone non-decreasing in each channel, so the worst luminance anywhere
/// on the path is bounded by the box CORNER — [`chan_max`] on the dark side,
/// [`chan_min`] on the light side. Bounding that one corner bounds the whole
/// path, with no sampling and no convexity argument (the sRGB toe makes the
/// transfer slightly non-convex at its join, so a convexity argument would not
/// actually hold).
///
/// WHY `max(cap_AA, L(fg))` AND NOT THE RATIO ROUND-TRIP. When a theme's own
/// body text sits BELOW AA — Solarized Light's `#657B83` on `#FDF6E3` is
/// 4.13:1 — demanding AA of the tint would demand more than the theme delivers,
/// and the honest bound is the theme's own ratio. Writing that as
/// `(L(bg)+0.05)/min(4.5, contrast(fg,bg)) - 0.05` round-trips `L(fg)` through
/// two divisions and lands an ulp BELOW it, which makes every candidate
/// infeasible and collapses the palette. The `max` form is feasible at the
/// foreground itself by identity, because both sides evaluate the same
/// `rel_luminance(fg)`.
#[inline]
pub(super) fn glyph_ink_bound(fg: u32, bg: u32) -> Option<(GlyphInkSide, f32)> {
    // A sentinel (or any set high byte) is NOT a colour: masking it would read
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

/// One band as GLYPH INK for this theme: saturated about zero, then walked along
/// a one-parameter family toward black (dark side) or white (light side) until
/// the BOX CORNER against the foreground satisfies the bound.
///
/// The family is per-channel MONOTONE in `k` even after quantization, which is
/// what makes bisection exact rather than approximate: the predicate is
/// evaluated on the quantized candidate, so the answer that comes back is the
/// answer that ships.
///
/// `None` when the band is achromatic (not a hue), or when even the extreme of
/// the family cannot satisfy the bound.
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
    // k = 0 is the SAFE extreme (black / white), k = 1 the most chromatic.
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
    if ok(cand(1.0)) {
        return Some(cand(1.0));
    }
    if !ok(cand(0.0)) {
        // Even the extreme fails — the foreground alone already breaches the
        // bound, which only happens on a theme this function should not have
        // been asked about. Fail closed.
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

/// ONE MEMOIZED FRESH-INK GLYPH PALETTE: the `(theme_fg, theme_bg)` key it was
/// derived from, paired with the seven band inks — or with `None`, which is a
/// memoized REFUSAL (a theme that gets no tint at all; see
/// [`fresh_ink_glyph_palette`]). Named because the nesting is three deep at the
/// field that holds it and reads as noise inline.
pub(super) type GlyphPaletteMemo = ((u32, u32), Option<[u32; RAINBOW_BANDS.len()]>);

/// The seven band inks for one theme, or `None` when this theme gets no tint.
///
/// ALL SEVEN OR NONE, deliberately. A per-band suppression would draw a rainbow
/// with holes — the exact defect the [`LIGHT_INK_MAX_LUMA`] note records from a
/// white-ground review — so if any band fails the chroma gates the whole layer
/// stands down and the text simply keeps its own colour.
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

// ---- THE CLASSIC LANDING RING's GRADE -------------------------------------
//
// Owner, 2026-09-08: "a bigger impact splash that scales more with the distance
// traveled". Rainbow Kitty's landing took the grade first (`timing::impact`,
// `meteor::ring_ms`); this is the SAME magnitude applied to every other style's
// ring — fire flash, water ripple, the light-theme veil and the dark square
// outline all expand `classic_ring_radius_factor` wider and last
// `classic_ring_life` longer. An 8-cell hop (impact 1.0) is byte-for-byte what
// it was; a full-line Ctrl-A (impact 3.5) rings TWICE as wide for 300 ms —
// the same numbers the kitty's ring reaches, so the two arts agree on how big
// a jump felt.

/// Radius growth per unit of impact above 1: ×2.0 at the 3.5 cap
/// (`1 + 0.4 · 2.5`). Rainbow Kitty's `RING_R_PER_IMPACT_CH` doubles its ring
/// over the same range.
pub(super) const CLASSIC_RING_RADIUS_PER_IMPACT: f32 = 0.4;
/// Life growth per unit of impact above 1: the built-in 180 ms reaches 300 ms
/// at the cap (`0.18 · (1 + 0.267 · 2.5)`), Rainbow Kitty's `RING_MS_MAX`.
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
/// impact 1, `base · 1.667` at the cap (0.18 s → 0.30 s for the built-ins; a
/// Trail Pack's own `ring.life_ms` grades the same way from its own base).
pub(super) fn classic_ring_life(base: f32, scale: f32) -> f32 {
    if !scale.is_finite() {
        return base;
    }
    let impact = scale.clamp(1.0, crate::rainbow_kitty::timing::IMPACT_MAX);
    base * (1.0 + CLASSIC_RING_LIFE_PER_IMPACT * (impact - 1.0))
}

/// The comet colour for `style` at path position `pos` (0 tail .. 1 head) — the
/// SINGLE ramp shared by the live animator ([`CursorGlow::comet_color`]) and the
/// settings-card effect demo, so the preview can never drift from the real art.
/// `hue` is the rolling rainbow phase in turns (rainbow/sparkle only).
pub fn style_comet_color(style: GlowStyle, color: u32, accent: u32, hue: f32, pos: f32) -> u32 {
    match style {
        GlowStyle::Lumen => lerp_rgb(accent, color, pos),
        // COMET: an icy DUST tail. Far back the dust has dispersed — the accent
        // dimmed toward a dark haze; the body brightens through the base hue;
        // and the last quarter flash-freezes toward white-hot ice at the head
        // (real comets whiten at the nucleus — sunlit fresh ice — while the
        // tail is older, darker dust). The white cap stays BELOW pure white so
        // the nucleus cursor's additive coma owns the brightest point on screen.
        GlowStyle::Comet => {
            let dusty = lerp_rgb(accent, 0x0000_0000, 0.45);
            let body = lerp_rgb(dusty, color, pos);
            if pos > 0.75 {
                lerp_rgb(body, 0x00F0_FAFF, (pos - 0.75) * 4.0 * 0.80)
            } else {
                body
            }
        }
        // Phaser: `pos` is a HUE OFFSET in turns from the live sweep phase.
        // The live animator passes each cell's laid-hue offset (see
        // `emit_comet` — cells keep the colour they were laid in, so the band
        // is a real spatial rainbow); the settings preview passes its tail→head
        // path position, which renders the same laid-spectrum look one full
        // turn across the preview band.
        GlowStyle::Phaser => hsv2rgb((hue + pos).fract(), 0.95, 1.0),
        // CLASSIC keeps v0.28's HALF-turn sweep. The modern phaser above lays a
        // FULL turn across the band — which is precisely the change that turned
        // the old fine tracer into today's saturated rainbow bar — so the
        // salvage must not share that arm. (The live classic engine resolves its
        // own colour in `classic_wake`; this arm serves the settings preview,
        // and the two must agree.)
        GlowStyle::Classic => hsv2rgb((hue + pos * 0.5).fract(), 0.95, 1.0),
        GlowStyle::Sparkle => hsv2rgb((hue + pos * 0.5).fract(), 0.95, 1.0),
        // The ribbon and cursor share this same continuous seven-anchor field.
        // This arm serves the lone-cell fallback and settings preview.
        GlowStyle::RainbowKitty => rainbow_cycle_ink(pos),
        // Head capped just short of white (0.83 == the field's FIRE_IDX_MAX/1023)
        // so the bloomed comet streak is a hot AMBER glow, not a white wash that
        // swallows the letters under it (owner: "cannot read if the flame is too
        // bright"). Tail still dies to deep red.
        GlowStyle::Fire => fire_ramp((pos).min(0.83)),
        // Monochrome beam: the hue stays SATURATED almost to the tail tip (a
        // laser is coherent light — it doesn't fade to smoke), rolling off only
        // slightly. No white lerp — the filament layer supplies the only
        // (capped) highlight, so the beam never flashes out of its own hue.
        GlowStyle::Laser => lerp_rgb(0x0000_0000, color, 0.70 + 0.30 * pos),
        // Coherent tube: near-uniform hue along the whole rod with only a
        // gentle brightening toward the head — a beam of steady light has no
        // tail-to-head drama; the BEAM_LAYERS' specular axis supplies the
        // (capped) highlight.
        GlowStyle::Beam => lerp_rgb(0x0000_0000, color, 0.60 + 0.40 * pos),
        GlowStyle::Water => water_ramp(pos), // deep-blue tail → bright-cyan crest
        // Exhaustiveness only: the custom interpreter resolves colour through its
        // OWN ramp closure (`custom_ramp_color`) and never calls this shared ramp
        // for a pack, so this mono fallback is never emitted live (risk #3).
        GlowStyle::Custom => lerp_rgb(accent, color, pos),
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
        // Ablation spark: white-hot off the cut, cooling into the pure beam hue
        // as it dies. The 0.55 white ceiling keeps every spark inside the 0.75
        // monochrome-hue law the laser test pins.
        GlowStyle::Laser => lerp_rgb(color, 0x00FF_FFFF, 0.55 * fade),
        // Stardust mote: starlight — near-white when fresh, settling into the
        // beam's ice-blue as it dims. (The live emitter draws its own white /
        // ice / violet star tints; this shared ramp keeps the settings-preview
        // dots in the same family.)
        GlowStyle::Beam => lerp_rgb(color, 0x00FF_FFFF, 0.35 + 0.45 * fade),
        // Comet debris: a GLINT of fresh-shed ice — near-white while young,
        // cooling back into the tail's own hue as it dies, so the glitter reads
        // as pieces OF the comet, never confetti in a foreign colour.
        GlowStyle::Comet => lerp_rgb(color, 0x00FF_FFFF, 0.65 * fade),
        // rainbow kitty debris: A NAMED STOP, never the open wheel.
        //
        // THE OWNER'S COMPLAINT, and this arm is where it was manufactured: *"I
        // see lots of colors but it still doesn't feel coherent"* and *"Cyan
        // isn't a rainbow color."* Every rainbow particle — the jump's shed
        // glitter above all — carries a `hue` that is a UNIFORM RANDOM 0..1
        // seed, and it used to fall through to the Sparkle arm below and be
        // resolved as `hsv2rgb(hue, 0.85, 1.0)`: the FULL hue wheel, cyan and
        // magenta included, at full saturation, thrown a dozen at a time across
        // a screen-crossing jump.
        //
        // §2.3.3 of `docs/design/RAINBOW-TRAIL-ONE-STORY.md` is the rule and it
        // is absolute: no point-mark ever samples the open gradient — stars,
        // motes, veils and tints SNAP TO A NAMED STOP. So the seed is kept (it
        // is what decorrelates one grain from the next) and it now selects a
        // band index instead of an angle: if a jump sheds, it sheds the
        // ribbon's own seven anchors and nothing else. The white core stays — it
        // is a glint on the grain, not a hue.
        GlowStyle::RainbowKitty => {
            lerp_rgb(spectrum_snap(hue.rem_euclid(1.0)), 0x00FF_FFFF, 0.35 * fade)
        }
        // Sparkle: bright, slightly white-cored rainbow. This style's identity
        // IS the open wheel; it is not the rainbow trail and it is not bound by
        // the trail's anchor law.
        _ => lerp_rgb(hsv2rgb(hue, 0.85, 1.0), 0x00FF_FFFF, 0.35 * fade),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "centre + per-axis radii + colour + peak; a param struct would obscure the geometry at the many emitter call sites"
)]
pub(super) fn push_halo_over(
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
    // EFFECTS BOX (grid + head band): identity-exact at head 0; keeps every
    // emitted band inside a row the renderers actually draw.
    let (bl, br) = (geom.fx_left(), geom.fx_right());
    let (bt, bb) = (geom.fx_top(), geom.fx_bot());
    let scale = (peak as f32 / 255.0).max(0.55);
    let rxi = ((rx * scale).round() as i32).max(1);
    let ryi = ((ry * scale).round() as i32).max(1);
    let (cxi, cyi) = (cx.round() as i32, cy.round() as i32);
    let x0 = (cxi - rxi).max(bl);
    let x1 = (cxi + rxi).min(br);
    let y0 = (cyi - ryi).max(bt);
    let y1 = (cyi + ryi).min(bb);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // The falloff centre is stored TRUE, or the halo is culled — the same law
    // as [`push_halo`] above: a centre clamped into the box relocated the
    // radial peak onto the edge for the sliver frames of anything sliding off
    // the grid (2026-09-01 audit).
    if cxi < 0 || cyi < 0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = geom.origin_y as i32;
    let mut yy = y0;
    while yy < y1 {
        // Grid-row DAMAGE HINT, anchored at origin_y: an above-grid band tags
        // row 0 (which opens the top scissor band), never a wrapped u16.
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color,
            cx: cxi as u16,
            cy: cyi as u16,
            rx: rxi as u16,
            ry: ryi as u16,
            mode: HaloMode::Over,
        });
        yy = band_end;
    }
}

/// THE ONE DASH. A velocity-aligned two-vertex AA beam trailing `trail_s`
/// seconds of a particle's LIVE ballistic motion behind its position `(x, y)`
/// — the shared renderer behind the Laser ablation spark, the rainbow kitty
/// shooting-star body, and the Sparkle mini-comet. `tail` colours the trailing
/// vertex at quarter coverage, `head` the tip at full; thickness and trail
/// seconds are the knobs the three sites disagree on.
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
    let verts = [
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
    ];
    comet_beam(out, geom.beam_clip(), &verts, thick, 1, 0.0);
}

/// Push one RADIAL halo of premultiplied peak light: its bounding rect is
/// clamped to the WINDOW interior and SPLIT into per-cell-row [`RainHalo`] quads
/// (the dirty gate + scissor stay exact) that all share the same centre and
/// falloff radii — the renderer's shared radial rasterizer does the rest.
/// Radii are floored to 1 (the parity contract's divide-by-zero guard).
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
    // EFFECTS BOX (grid + head band): identity-exact at head 0.
    let (bl, br) = (geom.fx_left(), geom.fx_right());
    let (bt, bb) = (geom.fx_top(), geom.fx_bot());
    let rxi = (rx.round() as i32).max(1);
    let ryi = (ry.round() as i32).max(1);
    let (cxi, cyi) = (cx.round() as i32, cy.round() as i32);
    let x0 = (cxi - rxi).max(bl);
    let x1 = (cxi + rxi).min(br);
    let y0 = (cyi - ryi).max(bt);
    let y1 = (cyi + ryi).min(bb);
    if x1 <= x0 || y1 <= y0 || cxi < bl - rxi || cyi < bt - ryi || cxi > br + rxi || cyi > bb + ryi
    {
        return;
    }
    // THE CENTRE IS THE FALLOFF'S TRUTH — store it, never clamp it into the
    // box. Clamping `cxi` to `br` moved an off-right centre ONTO the edge, so
    // the surviving 1..r-px sliver of a particle sliding off the grid rendered
    // at 41-96% of peak radial weight where the true falloff says ~0 — a
    // bright fringe pinned against the boundary that then popped off
    // (2026-09-01 audit; both backends read this same field). A positive
    // overshoot fits u16 (`br + rxi` is far under 65536); a NEGATIVE centre
    // cannot be represented, so cull the halo instead — reachable only when
    // the box starts at 0, and the surviving band is at most r px of the
    // falloff's own dim tail.
    if cxi < 0 || cyi < 0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = geom.origin_y as i32;
    let mut yy = y0;
    while yy < y1 {
        // Grid-row DAMAGE HINT, anchored at origin_y (above-grid bands tag row 0).
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
            // Defaulted `mode: HaloMode::Add` — the historical light.
            ..Default::default()
        });
        yy = band_end;
    }
}

/// THE ONE ROUND STARDUST GRAIN ON DARK GROUND — a radial mote drawn at the
/// COMPOSITED LIGHT of the four-point plus it stands in for.
///
/// The stardust law swapped a population's silhouette, not its brightness, and
/// on the additive arm those are not the same statement.
/// [`push_twinkle_star`] lays THREE coincident marks on the crossing — the
/// horizontal arm at `cov`, the vertical arm at `cov`, the nucleus at
/// [`crate::effect_util::STAR_CORE_ADD`] — so the pixel the eye reads at a plus's centre
/// carries [`crate::effect_util::STAR_STACK_ADD`] (2.35x) of the coverage its
/// emitter asked for. One `push_halo` at that same `cov` peaks at `cov`: parity
/// primitive-for-primitive, 2.35x darker in the middle. The owner asked for
/// fewer crosses AND more light (2026-08-09, "many fewer of those cross
/// sparkles", after 2026-08-08's "cute small stardust"), so every dealt grain
/// that LOST its plus has to be priced against what that plus put on screen.
///
/// Two halos, exactly as [`crate::effect_util::push_dust_mote`] does it on the
/// additive rect: the emitter's own SKIRT at `cov`, and a small HOT CORE at
/// [`STAR_CORE`] of the radius carrying the rest of the stack. Paying the
/// difference into a core rather than into the whole disc keeps the mark's TOTAL
/// light where the plus had it — brightening the skirt to 2.35x would make the
/// round grain 2.35x the light of the cross it replaced, which is a different
/// wrong answer — and it keeps the mote's soft edge, which a flat 2.35x disc
/// would blow out into a blob.
///
/// This is the helper the shooting-star head wrote out by hand first; every
/// other additive stardust site now calls it, so the law has ONE spelling and a
/// new emitter cannot quietly ship the 1x version again.
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
    // Floored at 1 px so the smallest grain still has a lit centre — the same
    // floor `push_dust_mote`'s `ceil(d/2).max(1)` core carries.
    let core = (r * STAR_CORE).max(1.0);
    // Clamped at the byte, and the clamp is never a shortfall: above `cov` 188
    // the skirt plus a 255 core already saturates the channel, which is what the
    // plus's own 2.35x does anywhere above `cov` 109.
    let hot = (f32::from(cov) * (crate::effect_util::STAR_STACK_ADD - 1.0)).min(255.0) as u8;
    push_halo(out, geom, cx, cy, core, core, premul_rgb(rgb, hot));
}

/// Fractional part (non-negative for non-negative input).
pub(super) fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// The BEAM power envelope: full power for the first 40% of a spark's life,
/// then ONE smooth cosine down to zero — the switch-off curve shared by the
/// tube's brightness (both typing and jump sparks in `emit_comet`'s fade) and
/// its thickness collapse (`core_thick`), so the rod dims and thins as a single
/// object. Deterministic in `frac`, so tests and CPU/GPU parity see identical
/// quads.
pub(super) fn beam_power(frac: f32) -> f32 {
    if frac < 0.40 {
        1.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * (frac - 0.40) / 0.60).cos())
    }
}

/// HSV (h,s,v all 0..1) → `0x00RRGGBB`.
pub(super) fn hsv2rgb(h: f32, s: f32, v: f32) -> u32 {
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
