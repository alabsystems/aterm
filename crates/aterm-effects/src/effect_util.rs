// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor emitters' shared render kit — ONE copy of the per-cell-row quad
//! pushers, the colour ramps, and the 4-point twinkle star that the `cursor_*`
//! modules each carried privately. Two rect pushers exist ON PURPOSE and are
//! NOT interchangeable: [`push_grid_rect`] takes GRID-RELATIVE px and clamps to
//! the grid interior with `y / ch` row tags, while [`push_fx_rect`] takes
//! WINDOW-ABSOLUTE px and clamps to the effects box with origin-anchored row
//! tags — routing an emitter through the wrong one moves its light and mistags
//! its damage rows.

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;

/// Clamp a GRID-RELATIVE pixel rect to the grid interior and split it into
/// per-cell-row [`GlowQuad`]s (the renderer row-gate + CPU/GPU parity
/// invariant). Callers pass grid px — no `geom.origin_*` applied — and rows tag
/// `y / ch`: the contract of the grid-anchored emitters (comet, fireball,
/// droplet). Window-absolute emitters use [`push_fx_rect`] instead.
pub(crate) fn push_grid_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
) {
    // ADDITIVE light — the historical contract of every grid-anchored emitter
    // (see [`GlowQuad::alpha`]); `push_grid_quad` carries the same splitting
    // logic for the one emitter that also needs source-over.
    push_grid_quad(out, geom, x, y, w, h, premul, 0);
}

/// [`push_grid_rect`] with the blend mode left to the caller: `alpha == 0` is
/// the additive light every legacy grid emitter pushes (and this function is
/// then byte-identical to it), while `alpha > 0` selects premultiplied
/// source-over (`src + dst·(1 − a)`) — the mode a light-theme emitter needs,
/// because additive light can only BRIGHTEN and so cannot darken a pale ground.
/// Split out rather than duplicated so the per-cell-row band walk (the renderer
/// row-gate + CPU/GPU parity invariant) keeps exactly ONE copy.
#[allow(
    clippy::too_many_arguments,
    reason = "the rect + blend mode IS the call; a struct would relocate the same list and cost \
              every existing caller a construction"
)]
pub(crate) fn push_grid_quad(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
    alpha: u8,
) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    let gw = (geom.cols * geom.cw) as i32;
    let gh = (geom.rows * geom.ch) as i32;
    let x0 = x.max(0);
    let x1 = (x + w).min(gw);
    let y0 = y.max(0);
    let y1 = (y + h).min(gh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let ch = geom.ch as i32;
    let mut yy = y0;
    while yy < y1 {
        let row = yy / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row as u16,
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

/// Push a pixel rect of premultiplied light in WINDOW px, CLAMPED to the
/// EFFECTS BOX (grid + head band — identity-exact at `head == 0`) and SPLIT
/// into per-cell-row [`GlowQuad`]s with origin-anchored row DAMAGE tags (the
/// renderer row-gate + CPU/GPU parity invariant). Callers pass window px
/// (`geom.origin_*` already applied); grid-relative emitters use
/// [`push_grid_rect`] instead.
pub(crate) fn push_fx_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    debug_assert!(
        geom.fx_right() <= i32::from(u16::MAX) && geom.fx_bot() <= i32::from(u16::MAX),
        "effects-box pixel extent exceeds u16 GlowQuad range"
    );
    // Clamp to the EFFECTS BOX (grid + head band) — identity-exact at head 0;
    // below-grid/side bands would only be skipped by the renderers' row gates.
    let x0 = x.max(geom.fx_left());
    let x1 = (x + w).min(geom.fx_right());
    let y0 = y.max(geom.fx_top());
    let y1 = (y + h).min(geom.fx_bot());
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = i32::from(geom.origin_y);
    let mut yy = y0;
    while yy < y1 {
        // Grid-row DAMAGE HINT, anchored at origin_y (above-grid bands tag row 0).
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
            // ADDITIVE light — this emitter has no other mode (see
            // [`GlowQuad::alpha`]).
            alpha: 0,
        });
        yy = band_end;
    }
}

/// Per-channel linear interpolation between two `0x00RRGGBB` colours, `t` 0..1.
#[inline]
pub(crate) fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |sh: u32| {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t) + 0.5) as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// The FIRE ramp's stops: deep red → orange → yellow → near-white core.
///
/// Module-level `const` ON PURPOSE (both stop tables were function locals):
/// the ramps run per particle per frame in the fire/water emit paths, and a
/// local table is re-materialized on the stack every call the moment the
/// inliner declines the body. [`ramp_5stop`] leans on this table's SHAPE —
/// stop positions strictly ascending, first exactly 0.0, last exactly 1.0 —
/// pinned by `ramp_stop_tables_hold_the_branchless_contract`.
const FIRE_STOPS: [(f32, u32); 5] = [
    (0.0, 0x002A_0000),
    (0.25, 0x008B_1A00),
    (0.5, 0x00E0_4A00),
    (0.75, 0x00FF_B020),
    (1.0, 0x00FF_F0C0),
];

/// The OCEAN ramp's stops: deep-sea abyss → open-ocean blue → turquoise →
/// vivid aqua crest → foam. Deliberately SATURATED and green-leaning through
/// the midband: the old pale sky-cyan stops read as ICE (live review: "WE ARE
/// NOT DOING ICE") — real water is rich blue-green, and foam-white appears
/// only at the very crest. Same shape contract as [`FIRE_STOPS`].
const WATER_STOPS: [(f32, u32); 5] = [
    (0.0, 0x0005_2C48),
    (0.35, 0x000E_66B4),
    (0.65, 0x0014_AAC8),
    (0.85, 0x0032_DCDE),
    (1.0, 0x00C2_F2F5),
];

/// The shared 5-stop ramp core: a BRANCHLESS window select feeding the exact
/// `(t - t0) / (t1 - t0)` + [`lerp_rgb`] arithmetic the old `windows(2)` walk
/// fed, on the same f32 stop values — so every output is bit-equal to the
/// walk. Not asserted, SWEPT: a verbatim copy of the old formulation is the
/// oracle in `ramp_rewrite_is_bit_identical_to_the_stop_walk`, and its
/// `--ignored` twin runs all 2^32 bit patterns for palette retunes.
///
/// WHY (driver-03): both ramps built their stop table as a function local and
/// linear-searched it PER CALL, while the fire/water emit paths call them per
/// particle per frame (the ember-shower loop, the water sparks, the
/// fireball/droplet nucleus bands). Hoisting one constant-argument call site
/// was measured as invisible (wave-2 `cg2-undertow-hoist`: no win), so the
/// fix is at the source instead: the window index IS the count of interior
/// stop positions strictly below `t` — three compares summed, no stack
/// table, no slice iterator, no early-exit compare chain — the exact
/// complement of the walk's "first window with `t <= t1`", boundaries
/// included.
// NOT `.clamp()`: `f32::clamp` PROPAGATES NaN, and the walk compared
// `NaN <= t1` false on every window, falling through to the RAW crest
// colour. `f32::min`/`max` return the OTHER operand for a NaN input, so
// clamping min-FIRST pins NaN to 1.0 — and the lerp at `local == 1.0` lands
// EXACTLY on the crest (channel bytes are exact in f32, and `crest + 0.5`
// truncates back to `crest`), so even a poisoned `t` keeps its old colour
// without buying a dedicated NaN branch. Same idiom as
// `aterm_render::sdr_glow_budget`.
#[allow(clippy::manual_clamp)]
#[inline]
fn ramp_5stop(stops: &[(f32, u32); 5], t: f32) -> u32 {
    let t = t.min(1.0).max(0.0);
    let idx =
        usize::from(t > stops[1].0) + usize::from(t > stops[2].0) + usize::from(t > stops[3].0);
    let (t0, c0) = stops[idx];
    let (t1, c1) = stops[idx + 1];
    // The walk's `if t1 > t0` divide guard is gone, not hidden: positions
    // strictly ascend by table contract (tested), so it was dead in every
    // window and its only effect was one more branch in a per-particle path.
    lerp_rgb(c0, c1, (t - t0) / (t1 - t0))
}

/// Black-body-ish FIRE ramp, `t` 0 (cool, deep red) → 1 (hot, white-yellow) —
/// the one palette behind the aurora's fire comet/curtain and the fireball
/// nucleus.
#[inline]
pub(crate) fn fire_ramp(t: f32) -> u32 {
    ramp_5stop(&FIRE_STOPS, t)
}

/// OCEAN ramp, `t` 0 (deep navy) → 1 (bright cyan crest, just shy of foam) —
/// the one water palette behind the aurora's fluid wake, the droplet nucleus,
/// and the word-decoration splash (ORCA_PALETTE).
#[inline]
pub(crate) fn water_ramp(t: f32) -> u32 {
    ramp_5stop(&WATER_STOPS, t)
}

/// THE TWINKLE'S OWN PALETTE — the white/gold pair the typing starfield and the
/// glide sparkles have always used. Hoisted out of [`push_twinkle_star`] when the
/// star became colour-agnostic (the rainbow kitty landing burst draws the SAME shape in a
/// rainbow band hue), so those two call sites stay byte-identical.
///
/// THE ONE GOLD. Four near-identical warm whites used to be strewn across the
/// star kit — `0x00FF_E9A8` here, `0x00FF_F2C0` on the flying shower's warm
/// grains and its shooting-star tail, `0x00FF_EED2` on the word-decoration glow
/// star, `0x00FF_F2C8` on the supernova ember star — each independently trying
/// to be "the gold" and each landing a few counts off the others. A sparkle's
/// warmth is a FAMILY property, so it lives here and nowhere else; a call site
/// that wants the warm half of the palette asks for `twinkle_rgb(true)`.
/// (The FIRE ramp's `0x00FF_F0C0` crest and `cursor_glow`'s `FRESH_INK_WARM`
/// are NOT in this family — one is a black-body stop, the other a veil tint.)
#[inline]
pub(crate) const fn twinkle_rgb(gold: bool) -> u32 {
    if gold { 0x00FF_E9A8 } else { 0x00FF_FFFF }
}

// ─── THE SPARKLE FAMILY ─────────────────────────────────────────────────────
//
// ONE SILHOUETTE, ONE ARM FAMILY, ONE PULSE LAW.
//
// Every 4-point twinkle this crate draws — additive on dark
// ([`push_twinkle_star`]), source-over on light (`cursor_glow`'s
// `push_twinkle_over`) — is the SAME MARK, and the numbers that define it live
// here rather than at the eleven call sites that used to each invent their own.
// The owner's report was that "sparkle icons don't seem like they were drawn by
// the same artist": the light star had a waist and a nucleus the dark star had
// never heard of, nine emitters passed arm ratios spanning 6x with nothing in
// common, and six different pulse frequencies ran under three different
// rectifications. A star's CONTEXT may still choose its colour, its size (from
// the named ratios below) and its phase — nothing else.

/// SILHOUETTE — the arms' HALF-THICKNESS as a fraction of the arm's half-length.
/// Both rasterizers take their arm mass from this one number: the source-over
/// star as the minor radius of each arm's falloff ellipse, the additive star as
/// the integer bar height/width (`2·arm·STAR_WAIST` rounded, floored at the 1 px
/// hairline the small grains have always been — so the mark stays proportional
/// and only the big hero grains actually fatten).
/// STARDUST, NOT A PLUS SIGN.
///
/// The waist is a fraction of the ARM LENGTH, so `waist_px = 2·arm·STAR_WAIST`.
/// At 0.34 that made every arm 68 % as thick as it was long, with a core square
/// 80 % of the arm on a side — a typographic `+` with a block in the middle,
/// which is exactly what the owner saw and named. The additive star had ALWAYS
/// been two 1-pixel hairlines; that hairline IS the stardust read.
///
/// 0.34 and 0.40 came from `push_twinkle_over`, the SOURCE-OVER helper, where
/// they were chosen because ink on white needs AREA to register at all. Pushing
/// a light-theme compromise onto the additive star — which never had that
/// problem, because every lit pixel is at full coverage — is how a delicate
/// glint became a fat cross. The unification was right; the number it unified
/// on was the wrong one of the two.
///
/// At 0.10 the waist rounds to the shipped 1 px hairline for every ordinary
/// arm and only widens on genuine hero grains, which is what the ladder is for.
///
/// THE WAIST IS NOW THE ARM'S *BASE*, NOT ITS PROFILE — see [`STAR_TIP_COV`].
/// A bar of this thickness held all the way to a square end is a `+`; the arm
/// starts here and thins to a point.
pub(crate) const STAR_WAIST: f32 = 0.10;

// ─── THE TAPER ──────────────────────────────────────────────────────────────
//
// A SPARKLE COMES TO A POINT (owner, 2026-08-10: the needle star "is better",
// and the marks the kit was drawing "read as PLUS SIGNS").
//
// Both rasterizers used to draw an arm as ONE primitive of constant thickness
// held to a square end — additive as a rect, source-over as one falloff
// ellipse — crossed with its twin over a square nucleus. That is the anatomy of
// a typographic `+`, and no amount of thinning fixes it: a 1 px hairline with
// hard ends is a thin plus, not a star.
//
// THE SHAPE ALREADY EXISTED IN THIS WORKSPACE. `aterm-render`'s `needle_star`
// (the word decorations' own sparkle) draws each point as a needle whose
// half-width shrinks to nothing along the arm — `hw = r·hw_frac·(1 − along/r)`
// — over a small central disc. The cursor kit converges on THAT silhouette.
//
// AT TERMINAL SIZE A NEEDLE IS A COVERAGE RAMP. `needle_star` is rasterized
// from a signed distance at word-decoration size, where an arm is 4-6 px wide
// at the base and has room to narrow geometrically. A cursor star's arm is
// `star_arm` ≈ 0.14 · cell_h ≈ 3-8 px LONG, and [`STAR_WAIST`] puts its base at
// the 1-2 px the family's own anti-fat law allows (`STAR_WAIST`'s doc, three
// separate owner reports). A needle that narrows from 1.5 px to 0.2 px does not
// cross a pixel boundary — it just covers less and less of the one row it sits
// in, and the anti-aliased truth of that is a BRIGHTNESS ramp along the arm.
// So the taper is drawn as a bright BODY at the crossing and a short run of
// dimmer, thinner spans running out to each point.
//
// This is the same statement on both grounds; only the arithmetic differs. The
// additive arm ramps its per-span coverage (and, once an arm is thick enough to
// have room, its span thickness); the source-over arm stacks a shorter BODY lay
// over a full-length TIP lay, so the ink is heaviest at the crossing and
// thinnest at the points.

/// THE TAPER — the fraction of an arm's half-length that is drawn at FULL
/// coverage, as one span through the crossing. Beyond it the arm is the point.
///
/// A bit under half. Less and the star loses the solid centre that makes it
/// read as a mark at all (at `arm` 3-4 the body would be a single pixel); more
/// and the point is too short a run to ramp over and the arm goes back to
/// reading as a bar with a slightly soft end. 0.5 was the first capture and it
/// tapered visibly; 0.42 gives the ramp one more pixel to work in at the sizes
/// the starfield actually draws, which is what turns "soft-ended cross" into
/// "needle".
pub(crate) const STAR_TAPER_BODY: f32 = 0.42;

/// THE TAPER — the coverage AT THE POINT, as a fraction of the coverage the
/// emitter asked for. The ramp is linear in distance from full at the body's
/// end to this at the tip.
///
/// 0.22, i.e. the point ends at about a fifth of the light its centre carries.
/// Deep enough that it visibly dissolves rather than stopping (which is the
/// whole complaint) — at 0.45 a single-span point sampled at its midpoint came
/// out at 72 %, which is a slightly soft end, not a taper — and shallow enough
/// that the arm's REACH survives: a star whose tips fade to nothing is just a
/// shorter star, and arm length is what the size ladder
/// ([`STAR_ARM_FINE`]..[`STAR_ARM_HERO`]) spends its range on. Captured at 0.30
/// first, where the point was unmistakably tapered but still legible as a bar
/// end at the landing's hero size; 0.22 is where the tip stops being an end.
pub(crate) const STAR_TIP_COV: f32 = 0.22;

/// THE TAPER — the most spans one POINT is rasterized with (per side, per arm).
///
/// The point is rasterized one span PER PIXEL of its run, so the ramp is as
/// smooth as the pixels allow, up to this bound. It is what keeps the taper
/// cheap: the additive star is a hot path (the ribbon starfield draws one per
/// starred cell, every frame) and each span costs 4 quads — two arms x two
/// sides — so an unbounded ramp would price a hero grain at 30-odd quads for
/// pixels the eye cannot separate anyway. At 3 a star costs at most 15.
pub(crate) const STAR_TAPER_STEPS: i32 = 3;

/// How many coincident source-over lays `cursor_glow`'s `push_twinkle_over`
/// stacks at the crossing: a TIP lay and a BODY lay per arm, plus the nucleus.
pub(crate) const STAR_OVER_LAYS: usize = 5;

/// …and how many the star's centre must still COMPOSITE to. The untapered plus
/// laid exactly three coincident marks, and `stacked_ink_alpha` prices the round
/// stardust grain against `1 − (1 − a)³` on that basis; the taper adds lays to
/// the ARMS, not light to the middle, so the light star solves for a per-lay
/// alpha that reproduces this stack rather than deepening it.
pub(crate) const STAR_OVER_CENTRE_LAYS: f32 = 3.0;

/// The taper's coverage multiplier at `u` — the fraction of the way from the
/// BODY's end to the point. One ramp, shared by both rasterizers so the two
/// grounds taper at the same rate.
#[inline]
pub(crate) fn star_taper_cov(u: f32) -> f32 {
    1.0 - (1.0 - STAR_TIP_COV) * u.clamp(0.0, 1.0)
}

/// SILHOUETTE — the NUCLEUS's half-extent as a fraction of the arm's half-length.
/// A sparkle has a bright centre; on source-over that has to be drawn, and on
/// additive the crossing supplies part of it and [`STAR_CORE_ADD`] completes it.
pub(crate) const STAR_CORE: f32 = 0.16;

/// The ADDITIVE nucleus's share of the star's coverage. The dark star's two arms
/// already stack to 2x coverage where they cross, so a full-coverage core rect on
/// top would put the centre at 3x and clip a gold star to white. At half the
/// arms' coverage the crossing and the core together read as the same nucleus the
/// light star draws outright, without turning every sparkle into a blown dot.
pub(crate) const STAR_CORE_ADD: f32 = 0.35;

/// SILHOUETTE — the diagonal GLINT dots' offset from the centre, as a fraction of
/// the arm's half-length (the classic four-point sparkle's secondary points).
pub(crate) const STAR_GLINT: f32 = 0.5;

/// The glints' share of the star's coverage — dim by design, so text stays
/// legible under a starfield.
pub(crate) const STAR_GLINT_COV: f32 = 1.0 / 3.0;

/// THE ONE ARM SCALE: a star's arm half-length as a fraction of the CELL HEIGHT.
/// Every emitter sizes its stars as `STAR_ARM` times one of the named ratios
/// below (see [`star_arm`]) — the family's sizes are a deliberate ladder, not
/// nine unrelated floats.
pub(crate) const STAR_ARM: f32 = 0.14;

/// Arm ratio — the STARFIELD GRAIN: the ribbon's per-cell stars at rest, the
/// landing sparkles, the flying shower's cold end. The smallest mark that still
/// reads as a star rather than as a lit pixel.
pub(crate) const STAR_ARM_FINE: f32 = 0.75;

/// Arm ratio — the FAMILY DEFAULT: shooting-star and glide heads, Beam's
/// stardust, the comet's glint, every star at full typing spine.
pub(crate) const STAR_ARM_STD: f32 = 1.0;

/// Arm ratio — a HERO grain: Sparkle's large pour grains and the erase poof's
/// one surviving plus (which multiplies this again by `erase_hero_arm`).
pub(crate) const STAR_ARM_HERO: f32 = 1.4;

/// Arm ratio — the INK TAX. Additive light buys presence with per-pixel
/// coverage; source-over ink has to buy it with AREA, so the same radius that
/// reads as a star on black reads as a speck on white. A LIGHT-theme star is its
/// dark twin's ratio times this one. (Was `RAINBOW_LAND_STAR_LIGHT_SCALE`, which
/// applied only to the landing's colour stars while the other four light stars
/// paid taxes of 1.06, 1.71, 1.86 and 1.0 — i.e. none at all.)
pub(crate) const STAR_ARM_INK: f32 = 1.55;

/// THE LADDER'S SHAPE, held where a violation cannot ship. Nine call sites used
/// to pass arm ratios spanning 6x with no shared constant; these four rungs
/// replaced them, and the point of a ladder is that it stays ordered and stays
/// short. All four terms are constants, so this is a build-time fact rather than
/// something a test run could have skipped. (What the tests still owe is the
/// part no constant can state: that [`star_arm`] realizes each rung — see
/// `the_star_arm_ladder_is_ordered_and_derived`.)
const _: () = {
    assert!(
        STAR_ARM_FINE < STAR_ARM_STD && STAR_ARM_STD < STAR_ARM_HERO,
        "the star arm ladder must stay ordered fine < std < hero"
    );
    // Before the erase HERO band's own documented `erase_hero_arm` multiplier.
    assert!(
        STAR_ARM_HERO / STAR_ARM_FINE <= 2.0,
        "the family's own star sizes must stay inside 2x end to end"
    );
    assert!(
        STAR_ARM_INK > 1.0,
        "a source-over star must out-size its additive twin"
    );
};

/// A star's arm half-length in px: the cell height, [`STAR_ARM`], and ONE of the
/// named ratios. The only way an emitter is allowed to pick a star's size.
#[inline]
pub(crate) fn star_arm(cell_h: f32, ratio: f32) -> f32 {
    cell_h * STAR_ARM * ratio
}

/// THE ONE FLOAT→INTEGER ARM CONVERSION for the additive star: TRUNCATE, then
/// floor at the 1 px arm [`push_twinkle_star`] needs to draw anything at all.
///
/// It has to be one rule, because a plus's MASS is derived from its integer arm
/// and NOT from the float behind it: [`star_waist_px`] and [`star_core_px`]
/// round `2·arm·ratio`, so one extra pixel of arm is what flips a 1 px hairline
/// to a 2 px bar. Every integer-arm site in the kit spelled this
/// `(star_arm(..) as i32).max(1)` — truncation, which is what the ladder's
/// "every ordinary arm rounds to the hairline" claim was measured against —
/// except ONE, which spelled it
/// `(r.round() as i32).max(1)`: the jump-landing burst star. At a 40 px cell
/// that lone rounding turned the family's own hero grain (7.84 px of arm) into
/// an 8 px arm, i.e. a 2 px bar on a 3 px nucleus — a fat cross, thrown a whole
/// fan at a time, reached without ever leaving the ladder. Truncating there
/// costs the star less than a pixel of reach and puts it back on the hairline.
#[inline]
pub(crate) fn star_arm_px(arm: f32) -> i32 {
    (arm as i32).max(1)
}

/// THE STARDUST LAW (owner, 2026-08-08: "make sure that the cursor sparkles
/// look like these cute small stardust like sparkles and fewer of the '+'
/// like sparkles" — reaffirming 2026-08-06's "sparks are the body, '+' is
/// the accent").
///
/// The star-kit unification gave every mark ONE silhouette — and thereby made
/// every typing-trail particle a 4-point plus, which is exactly the look the
/// owner rejected twice. The family therefore has TWO members with ONE law
/// between them: the population's BODY is round stardust (a tiny radial
/// mote), and the 4-point star is an ACCENT dealt to at most one particle in
/// [`STAR_ACCENT_DEN`] — decided from the particle's ALREADY-STORED seed so
/// no RNG stream shifts and a grain never changes shape mid-flight. Heroes
/// (the erase poof's one plus, the jump-landing starburst, the momentum
/// shower's one shooting-star tip) sit OUTSIDE the deal: they are singular
/// gestures, not population.
///
/// "SINGULAR GESTURE" IS A COUNT, NOT A NAME. Two marks were exempted on the
/// strength of reading like one object and were in fact subpopulations, which
/// is why the DEFAULT THEME never felt the first thinning:
///   * the ribbon starfield's recruited GOLD — 6.3 % of every starred cell
///     measured over a 60x200 field at full spine, i.e. the same order as the
///     whole accent budget. Gold is a COLOUR now; the silhouette is dealt.
///   * the fast-glide shooting star's HEAD — one tip per streak, but the
///     classifier fires a streak per qualifying frame, so one fast sweep of
///     the cursor laid a row of full-size crosses. Dealt.
///
/// Both are `cursor_glow`'s; both were 1-in-1 before 2026-08-09.
///
/// 8 → 16 (owner, 2026-08-09: "many fewer of those cross sparkles"). This is
/// the SECOND report against this law, and the first one is the reason the
/// number had to move rather than the call sites: 1-in-8 was already shipping
/// everywhere when the owner looked again and still counted too many crosses.
/// A deal of 1-in-8 puts a plus in roughly every eighth grain of a field whose
/// grains arrive several per keystroke, so on a busy ribbon a cross is never
/// more than a cell or two away — "most of them are round" and "I keep seeing
/// crosses" are both true at 12.5 %, which is precisely the complaint. Halving
/// the share to 6.25 % puts the nearest plus a short word away instead, which
/// is what makes it read as punctuation on the dust rather than as texture in
/// it, while keeping it comfortably above zero ("a few of the '+' are nice").
///
/// The DEFAULT THEME is what this number is aimed at. The 2026-08-09 round
/// before this one thinned arm LENGTHS across both themes but changed the
/// plus:round RATIO at exactly one site — the LIGHT theme's fresh-ink spark —
/// so on dark, where the owner actually types, the mix was untouched. This
/// constant is the only lever that reaches every dealt population on both
/// grounds at once: the dark ribbon starfield, the light ribbon starfield, the
/// Beam/Comet/Sparkle wakes, the erase poof on both grounds, and the fresh-ink
/// spark. Measured per theme by `cursor_glow`'s
/// `stardust_is_the_body_the_plus_is_the_accent` (dark) and
/// `the_fresh_ink_spark_is_stardust_with_a_dealt_plus` (light).
pub(crate) const STAR_ACCENT_DEN: u32 = 16;

/// Deal a stored particle seed into the accent (true → the 4-point star) or
/// the stardust body (false → a round mote). 1-in-16 = 6.25 %, half the share
/// the first pass of the sparkle rebalance left and well under the ≤15 % it
/// promised.
#[inline]
pub(crate) fn star_accent(seed: u32) -> bool {
    seed.is_multiple_of(STAR_ACCENT_DEN)
}

/// Stardust mote radius, px: seeded so no two motes match, floored so the
/// smallest is still a lit point. Slightly smaller than the erase poof's
/// round body — the trail is dust, the poof is debris.
#[inline]
pub(crate) fn dust_r(cell_h: f32, seed01: f32) -> f32 {
    (cell_h * (0.07 + 0.05 * seed01)).max(1.3)
}

/// THE PLUS'S COMPOSITED CENTRE, as a multiple of the coverage its emitter
/// asked for. [`push_twinkle_star`] is THREE coincident additive lays at the
/// crossing — the horizontal arm at `cov`, the vertical arm at `cov`, and the
/// nucleus at [`STAR_CORE_ADD`] of it — so the PIXEL the eye actually reads at
/// the centre of a plus carries `2.35 · cov` of light, not `cov`.
///
/// This number exists because the stardust law's shape swap was, on the
/// additive arm, a BRIGHTNESS CUT nobody could see in the quad stream: the
/// round body was one lay at the same `cov` the plus's arms were handed, which
/// looks like parity primitive-for-primitive and lands 2.35x darker in the
/// middle. The owner asked for fewer crosses AND more light (2026-08-09, "many
/// fewer of those cross sparkles", after 2026-08-08's "cute small stardust"),
/// so a mote has to be priced against what the plus PUT ON SCREEN. The light
/// arm already does exactly this with `1-(1-a)^3` (the fresh-ink mote's
/// source-over stack); this is the additive half of the same statement.
pub(crate) const STAR_STACK_ADD: f32 = 2.0 + STAR_CORE_ADD;

/// THE ONE STARDUST MOTE (additive) — the round BODY of every dealt population
/// on dark ground, drawn so its composited centre matches the plus it replaced.
///
/// Two lays, for the same reason the star has three: a `d x d` SKIRT at the
/// emitter's own `cov`, and a HOT CORE over it carrying the rest of
/// [`STAR_STACK_ADD`]. A single flat rect at `2.35 · cov` would match the peak
/// too, but a 3-5 px square of blown white is a blob, not dust — the skirt keeps
/// the mote's soft edge while the core gives it the bright middle a sparkle
/// needs. The core is `ceil(d/2)`, so the mote's mass rides its own seeded size
/// and the smallest grain still gets a lit centre.
///
/// The core's coverage is CLAMPED at the byte, and that clamp is never a
/// shortfall: it only binds above `cov` 188, and there the skirt plus a 255 core
/// already saturates the channel — which is exactly what the plus's own
/// `2.35 · cov` does anywhere above `cov` 109. Either side of the clamp the mote
/// reaches `min(255, STAR_STACK_ADD · cov)`, i.e. the plus's rendered centre.
///
/// Window-absolute: draws through [`push_fx_rect`]. Returns `false` when the
/// quad budget ran out, checked between pushes exactly like [`push_twinkle_star`].
#[allow(
    clippy::too_many_arguments,
    reason = "the mote's parameter set is [`push_twinkle_star`]'s with the arm/gold pair \
              replaced by one size — output + geometry + centre + size + coverage + \
              colour + budget. The two sides of the stardust deal are chosen between at \
              every call site, so they must read as the same call"
)]
pub(crate) fn push_dust_mote(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    cx: i32,
    cy: i32,
    d: i32,
    cov: u8,
    color: u32,
    max_quads: usize,
) -> bool {
    if cov == 0 || d < 1 {
        return true;
    }
    push_fx_rect(
        out,
        geom,
        cx - d / 2,
        cy - d / 2,
        d,
        d,
        premul_rgb(color, cov),
    );
    if out.len() >= max_quads {
        return false;
    }
    // The HOT CORE completes the plus's stack: skirt (1.0) + core (1.35) =
    // [`STAR_STACK_ADD`] at the centre pixel.
    let c = ((d + 1) / 2).max(1);
    let core = premul_rgb(
        color,
        (f32::from(cov) * (STAR_STACK_ADD - 1.0)).min(255.0) as u8,
    );
    push_fx_rect(out, geom, cx - c / 2, cy - c / 2, c, c, core);
    true
}

/// THE ONE PULSE LAW — angular frequency (rad/s) of every twinkling star.
///
/// WCAG 2.3.1: this crate certifies a 3.2 Hz general-flash bound (see
/// `cursor_rainbow`'s `twinkle_flash_rate_stays_under_the_photosensitivity_bound`).
/// The envelope is RECTIFIED (`|sin|`), which DOUBLES the flash rate — two
/// bright peaks per sine cycle — so the bound applies to `ω/π`, not `ω/2π`.
/// At 9 rad/s that is [`TWINKLE_FLASH_HZ`] = 2.86 Hz, inside the bound with
/// headroom, and it is the rate the ribbon starfield already ran at.
pub(crate) const TWINKLE_OMEGA: f32 = 9.0;

/// THE ONE PULSE LAW — the envelope's floor. A star DIMS, it never blinks out:
/// a sparkle that reaches zero reads as a dropped frame, not as a twinkle.
pub(crate) const TWINKLE_FLOOR: f32 = 0.55;

/// THE ONE PULSE LAW — the envelope's depth. `TWINKLE_FLOOR + TWINKLE_DEPTH`
/// is 1.0: the law scales a star's own coverage and never exceeds it.
pub(crate) const TWINKLE_DEPTH: f32 = 0.45;

/// Where in the envelope's range a star counts as being at its PEAK — the gate
/// the diffraction-glint accents fire on. 0.92 of the range puts the glint on
/// for ~26 % of each pulse, the duty the Comet and Sparkle grains were hand-tuned
/// to before the law was shared.
pub(crate) const TWINKLE_GLINT_FRAC: f32 = 0.92;

/// The family's FLASH rate in Hz — `ω/π`, because the rectified envelope peaks
/// twice per sine cycle. Held under the WCAG 2.3.1 general-flash bound by
/// [`TWINKLE_FLASH_BOUND_HZ`] below, and MEASURED against the envelope the
/// emitters actually run by `twinkle_law_stays_under_the_photosensitivity_bound`.
pub(crate) const TWINKLE_FLASH_HZ: f32 = TWINKLE_OMEGA / std::f32::consts::PI;

/// WCAG 2.3.1's general-flash threshold, in Hz. Three flashes in any one second
/// is the failing condition, so 3.2 is the bound this crate certifies against
/// with the margin the guideline's own examples use.
pub(crate) const TWINKLE_FLASH_BOUND_HZ: f32 = 3.2;

/// A PHOTOSENSITIVITY BOUND IS NOT A TEST CASE. Both terms are constants, so
/// the check belongs where a violation cannot ship at all: retune
/// [`TWINKLE_OMEGA`] past the bound and this crate stops building, rather than
/// failing a test somebody could have skipped. (The test beside it still earns
/// its keep — it MEASURES the rate off the real envelope, which catches a
/// rectification change that leaves both constants untouched.)
const _: () = assert!(
    TWINKLE_FLASH_HZ <= TWINKLE_FLASH_BOUND_HZ,
    "the unified twinkle flashes over the WCAG 2.3.1 general-flash bound"
);

/// THE ONE TWINKLE. A star's brightness multiplier at `age_s` seconds of life,
/// on its own `phase` (radians — seed it per star so a field blinks out of step).
///
/// Driven by AGE, never by a frame counter: an envelope stepped per frame runs
/// at whatever rate the display happens to refresh at, which made the
/// word-decoration sparkles blink at double speed on a 120 Hz panel (and put
/// them over the flash bound at 60). Pinned by
/// `sparkle_twinkle_is_frame_rate_independent`.
#[inline]
pub(crate) fn twinkle_env(age_s: f32, phase: f32) -> f32 {
    TWINKLE_FLOOR + TWINKLE_DEPTH * (age_s * TWINKLE_OMEGA + phase).sin().abs()
}

/// Is this twinkle at its PEAK — the instant a star throws its diffraction
/// glints? One gate, expressed as a fraction of the law's own range, so a
/// retuned depth cannot silently change the glint duty.
#[inline]
pub(crate) fn twinkle_peak(env: f32) -> bool {
    env >= TWINKLE_FLOOR + TWINKLE_DEPTH * TWINKLE_GLINT_FRAC
}

/// The additive star's integer arm THICKNESS (px) for an arm of half-length
/// `arm`, from the shared [`STAR_WAIST`]. Floored at the 1 px hairline the small
/// grains have always been, so the family's ordinary starfield sizes are
/// unchanged and only the hero grains carry visible mass.
#[inline]
pub(crate) fn star_waist_px(arm: i32) -> i32 {
    ((2.0 * arm as f32 * STAR_WAIST).round() as i32).max(1)
}

/// The additive star's integer NUCLEUS side (px), from the shared [`STAR_CORE`].
#[inline]
pub(crate) fn star_core_px(arm: i32) -> i32 {
    ((2.0 * arm as f32 * STAR_CORE).round() as i32).max(1)
}

/// The additive star's integer BODY half-length (px) — the full-coverage run of
/// an arm, from the shared [`STAR_TAPER_BODY`]. Floored at 1 and capped at the
/// arm, so the smallest grain (`arm == 1`) is all body and no point: there is no
/// room to ramp over a single pixel, and a 3 px cross is already a dot.
#[inline]
pub(crate) fn star_body_px(arm: i32) -> i32 {
    ((arm as f32 * STAR_TAPER_BODY).round() as i32).clamp(1, arm)
}

/// THE ONE STAR. A 4-point twinkle: a horizontal and a vertical NEEDLE crossing
/// at `(sx, sy)` on a small NUCLEUS, plus — when `gold` — four dim diagonal
/// glint dots. Each needle is a full-coverage BODY through the crossing
/// ([`STAR_TAPER_BODY`]) running on into POINTS that dim toward
/// [`STAR_TIP_COV`] and thin with them — see THE TAPER above for why a needle
/// at terminal size is a coverage ramp rather than a narrowing outline.
/// This is the only 4-point star shape any emitter draws: the rainbow kitty
/// typing starfield, glide stars, jump-landing burst and shooting-star heads,
/// Beam's stardust, the Comet's debris glint and Sparkle's star grains all come
/// through here, so a star is a star wherever it appears and only its COLOUR and
/// SIZE change with context (owner: "use the same star pattern as in the cursor
/// trail … unify on rainbows and sparkles").
///
/// ONE SILHOUETTE WITH ITS LIGHT TWIN. The arms' thickness ([`STAR_WAIST`]), the
/// nucleus ([`STAR_CORE`]) and the glint offset ([`STAR_GLINT`]) are the family's,
/// shared verbatim with `cursor_glow`'s `push_twinkle_over` — the source-over
/// star that draws this same mark on a white ground. Before that, the light star
/// had a waist and a core and the dark star had neither, which is exactly what
/// "drawn by a different artist" looks like. Only the COMPOSITING differs: this
/// one is hard-edged and additive, so its crossing already supplies part of the
/// nucleus and the core rect is drawn at [`STAR_CORE_ADD`] of the arms' coverage
/// to finish it without clipping the centre to white.
///
/// Window-absolute: draws through [`push_fx_rect`]. Returns `false` when the
/// quad budget ran out, checked between pushes exactly as the call sites'
/// inlined originals did.
#[allow(
    clippy::too_many_arguments,
    reason = "output + geometry + centre + arm + coverage + glint + colour + budget; \
              the call sites are the emitters' whole star kit and share this one shape"
)]
pub(crate) fn push_twinkle_star(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    sx: i32,
    sy: i32,
    arm: i32,
    cov: u8,
    gold: bool,
    color: u32,
    max_quads: usize,
) -> bool {
    if cov == 0 || arm < 1 {
        return true;
    }
    let star = premul_rgb(color, cov);
    // THE BODY at the shared waist — the arm's full-coverage run through the
    // crossing. An even thickness is biased UP-LEFT by the integer half so the
    // mark's centre stays on `(sx, sy)`, the pixel every caller placed it at;
    // at the 1 px floor this is the shipped hairline verbatim.
    let t = star_waist_px(arm);
    let half = (t - 1) / 2;
    let b = star_body_px(arm);
    push_fx_rect(out, geom, sx - b, sy - half, 2 * b + 1, t, star); // horizontal
    if out.len() >= max_quads {
        return false;
    }
    push_fx_rect(out, geom, sx - half, sy - b, t, 2 * b + 1, star); // vertical
    if out.len() >= max_quads {
        return false;
    }
    // NUCLEUS: the crossing supplies half of it, this completes it. Drawn
    // BEFORE the points, so a star truncated by the quad budget loses its
    // faintest extremities rather than its bright centre.
    let c = star_core_px(arm);
    let ch = (c - 1) / 2;
    let core = premul_rgb(color, (f32::from(cov) * STAR_CORE_ADD) as u8);
    push_fx_rect(out, geom, sx - ch, sy - ch, c, c, core);
    // THE POINTS. Each arm runs on from the body to the tip in equal spans,
    // every one dimmer and (once the arm has the thickness to spare) thinner
    // than the last — the anti-aliased reading of a needle whose half-width
    // shrinks to nothing. Two spans per step, one per side of the crossing.
    let run = arm - b;
    let steps = run.clamp(0, STAR_TAPER_STEPS);
    for k in 0..steps {
        let d0 = b + run * k / steps; // last px of the previous span
        let d1 = b + run * (k + 1) / steps; // last px of this one
        // The ramp runs over the POINT — full where the body ends, [`STAR_TIP_COV`]
        // at the tip — sampled at this span's midpoint.
        let ramp = star_taper_cov(((d0 + d1) as f32 * 0.5 - b as f32) / run as f32);
        let tip = premul_rgb(color, (f32::from(cov) * ramp) as u8);
        if tip == 0 {
            break;
        }
        let tt = ((t as f32 * ramp).round() as i32).max(1);
        let th = (tt - 1) / 2;
        let len = d1 - d0;
        // `d0 + 1` runs right/down from the body; `-d1` mirrors it.
        for off in [d0 + 1, -d1] {
            if out.len() >= max_quads {
                return false;
            }
            push_fx_rect(out, geom, sx + off, sy - th, len, tt, tip); // horizontal
            if out.len() >= max_quads {
                return false;
            }
            push_fx_rect(out, geom, sx - th, sy + off, tt, len, tip); // vertical
        }
    }
    if gold {
        let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
        let dim = premul_rgb(color, (f32::from(cov) * STAR_GLINT_COV) as u8);
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            if out.len() >= max_quads {
                return false;
            }
            push_fx_rect(out, geom, sx + ox, sy + oy, 1, 1, dim);
        }
    }
    true
}

/// Channel-weighted lit-pixel sum of a quad batch — the effect test suites'
/// shared brightness metric.
#[cfg(test)]
pub(crate) fn ink(out: &[GlowQuad]) -> u64 {
    out.iter()
        .map(|q| {
            let px = (q.w as u64) * (q.h as u64);
            px * (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        }
    }

    /// ONE PULSE LAW, and it is inside the flash bound this crate certifies.
    ///
    /// The family used to run SIX frequencies (8, 9, 11, 13, 7..13 rad/s and a
    /// per-frame step) under THREE rectifications (`|sin|`, raw `sin`,
    /// `sin.max(0)`), so "how fast does a sparkle blink" had no answer. It has
    /// one now, and the rectification is part of it: `|sin|` peaks TWICE per
    /// sine cycle, so the WCAG 2.3.1 general-flash budget is spent at `ω/π`.
    /// The raw-`sin` sites were being measured against the wrong number.
    #[test]
    fn twinkle_law_stays_under_the_photosensitivity_bound() {
        // The DECLARED rate is bounded at build time (`TWINKLE_FLASH_BOUND_HZ`),
        // where a violation cannot ship. What is left for a test is the part a
        // constant cannot state: that the declaration matches the envelope the
        // emitters actually run.
        //
        // MEASURED, not merely declared: count the envelope's bright peaks over
        // ten seconds and check the rate against the constant. A future retune
        // that drops the rectification would halve this and be caught.
        let (dt, span) = (1.0f32 / 240.0, 10.0f32);
        let n = (span / dt) as usize;
        let mut peaks = 0usize;
        let mut high = false;
        for i in 0..n {
            let hot = twinkle_env(i as f32 * dt, 0.0) >= TWINKLE_FLOOR + TWINKLE_DEPTH * 0.9;
            if hot && !high {
                peaks += 1;
            }
            high = hot;
        }
        let hz = peaks as f32 / span;
        assert!(
            (hz - TWINKLE_FLASH_HZ).abs() < 0.2,
            "measured {hz} Hz against a declared {TWINKLE_FLASH_HZ} Hz"
        );
    }

    /// A star DIMS, it never blinks out, and the law never brightens a star past
    /// the coverage its emitter asked for.
    #[test]
    fn twinkle_law_dims_between_the_floor_and_unity() {
        assert!((TWINKLE_FLOOR + TWINKLE_DEPTH - 1.0).abs() < 1e-6);
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for i in 0..4000 {
            let v = twinkle_env(i as f32 * 0.001, 0.37);
            assert!(
                (TWINKLE_FLOOR..=1.0).contains(&v),
                "envelope left its range: {v}"
            );
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(
            lo < TWINKLE_FLOOR + 0.01,
            "the envelope never reaches its floor"
        );
        assert!(hi > 0.99, "the envelope never reaches full");
    }

    /// The GLINT gate is a fraction of the law's own range, so retuning the
    /// depth cannot silently change how often a star throws its diffraction
    /// points. ~26 % duty — what the Comet and Sparkle grains were hand-tuned to
    /// with their private `> 0.82` / `> 0.85` thresholds.
    #[test]
    fn twinkle_glint_gate_holds_its_duty() {
        let n = 20_000;
        let hits = (0..n)
            .filter(|i| twinkle_peak(twinkle_env(*i as f32 * 0.0005, 0.0)))
            .count();
        let duty = hits as f32 / n as f32;
        assert!(
            (0.20..0.32).contains(&duty),
            "glint duty drifted to {duty} — the family's stars stopped agreeing on 'brightest instant'"
        );
    }

    /// THE ACCENT IS 1-IN-[`STAR_ACCENT_DEN`], measured over a uniform seed
    /// sweep rather than read off the constant — the deal is the one number the
    /// owner has now reported on twice ("fewer of the '+'", then "many fewer of
    /// those cross sparkles"), so its rate is worth a measurement.
    ///
    /// Also pins the DIRECTION of the 2026-08-09 change: whatever the
    /// denominator becomes, the plus must stay a small minority AND must not
    /// vanish ("a few of the '+' are nice").
    #[test]
    fn the_stardust_accent_is_a_small_dealt_minority() {
        let n = 16_000u32;
        let hits = (0..n).filter(|s| star_accent(*s)).count();
        let share = hits as f32 / n as f32;
        // Exact over a contiguous sweep: `seed % DEN == 0` partitions perfectly.
        assert!(
            (share - 1.0 / STAR_ACCENT_DEN as f32).abs() < 1e-6,
            "the deal measured {share}, not 1-in-{STAR_ACCENT_DEN}"
        );
        assert!(
            hits > 0,
            "the accent must SURVIVE — it is the family's grace note"
        );
        assert!(
            share <= 0.07,
            "the plus is back to texture at {:.1} % of the population",
            share * 100.0
        );
    }

    /// THE ONE ARM CONVERSION, and the pixel it is worth.
    ///
    /// [`star_arm_px`] truncates. That is not a stylistic choice: the family's
    /// claim that "every ordinary arm rounds to the 1 px hairline" was only ever
    /// measured against truncation, and the one emitter that ROUNDED instead
    /// (`cursor_glow`'s jump-landing burst) drew a 2 px bar on a 3 px nucleus at
    /// a 40 px cell while the audit certified it a hairline. So this pins BOTH
    /// halves: the rule, and the fact that the rule is load-bearing — at every
    /// shipping cell height the family's heaviest named grain is a hairline
    /// under truncation, and somewhere in that band it provably is NOT under
    /// rounding.
    #[test]
    fn the_one_arm_conversion_truncates_and_floors() {
        assert_eq!(
            star_arm_px(7.84),
            7,
            "the conversion must truncate, not round"
        );
        assert_eq!(star_arm_px(7.0), 7);
        // …and floors, so a tiny grain still draws a mark rather than nothing.
        assert_eq!(star_arm_px(0.2), 1);
        assert_eq!(star_arm_px(0.0), 1);
        let mut rounding_would_fatten = 0;
        for ch in [24.0_f32, 28.0, 32.0, 36.0, 40.0] {
            let f = star_arm(ch, STAR_ARM_HERO);
            let arm = star_arm_px(f);
            assert!(arm >= 2, "vacuous: no arm to measure at ch {ch}");
            assert_eq!(
                star_waist_px(arm),
                1,
                "the family's heaviest named grain is {} px thick at ch {ch}",
                star_waist_px(arm)
            );
            if star_waist_px((f.round() as i32).max(1)) > 1 {
                rounding_would_fatten += 1;
            }
        }
        // NON-VACUOUS: if rounding and truncating agreed everywhere in the
        // shipping band, this whole rule would be cosmetic and the burst star's
        // second spelling would have been harmless. It is not.
        assert!(
            rounding_would_fatten > 0,
            "no shipping cell height distinguishes truncation from rounding — \
             this rule would be pinning nothing"
        );
    }

    /// ONE ARM FAMILY. Nine call sites used to pass arm ratios spanning 6x with
    /// no shared constant; the sizes are a ladder now, and this pins its shape:
    /// ordered, derived from the one scale, and (before the erase HERO band's
    /// own documented multiplier) inside 2x end to end.
    #[test]
    fn the_star_arm_ladder_is_ordered_and_derived() {
        // The ladder's ORDER and SPAN are relations between constants and are
        // held at build time (see the `const _` block beside the ratios), where
        // a violation cannot ship. What only a test can state is that
        // `star_arm` — the function every emitter actually calls — really does
        // realize each rung as `cell · STAR_ARM · ratio`.
        for r in [STAR_ARM_FINE, STAR_ARM_STD, STAR_ARM_HERO, STAR_ARM_INK] {
            assert!((star_arm(16.0, r) - 16.0 * STAR_ARM * r).abs() < 1e-6);
        }
    }

    /// ONE SILHOUETTE, additive half. Every proportion of the mark comes from
    /// the shared constants: the arms' length and waist, the nucleus, the glint
    /// offset and its dimming. (The source-over half is checked against these
    /// same numbers by `cursor_glow`'s `a_sparkle_is_the_same_mark_on_both_grounds`.)
    #[test]
    fn the_additive_star_draws_the_family_silhouette() {
        let g = geom();
        let (sx, sy, arm, cov) = (100i32, 40i32, 10i32, 200u8);
        let mut out = Vec::new();
        assert!(push_twinkle_star(
            &mut out,
            g,
            sx,
            sy,
            arm,
            cov,
            true,
            0x00FF_FFFF,
            4096
        ));
        // THE BODY: the arm's full-coverage run through the crossing, at the
        // shared waist and the shared body fraction.
        let t = star_waist_px(arm);
        assert_eq!(t, (2.0 * arm as f32 * STAR_WAIST).round() as i32);
        let b = star_body_px(arm);
        assert_eq!(b, (arm as f32 * STAR_TAPER_BODY).round() as i32);
        let span = |q: &GlowQuad| (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        let full = premul_rgb(0x00FF_FFFF, cov);
        let body = out
            .iter()
            .find(|q| span(q) == (sx - b, sx + b + 1) && q.color == full)
            .expect("a horizontal body of 2*body+1 at full coverage");
        assert_eq!(
            i32::from(body.h),
            t,
            "the horizontal body left the shared waist"
        );
        // …and the arm still REACHES `arm` px either side of it: the taper
        // dims the point, it does not shorten the star. Measured as the union
        // of the row's lit columns, so body and points have to be contiguous.
        let full_span = 2 * arm + 1;
        let row: Vec<_> = out
            .iter()
            .filter(|q| i32::from(q.y) <= sy && sy < i32::from(q.y) + i32::from(q.h))
            .collect();
        let lit = |x: i32| row.iter().any(|q| span(q).0 <= x && x < span(q).1);
        assert!(
            (sx - arm..=sx + arm).all(lit) && !lit(sx - arm - 1) && !lit(sx + arm + 1),
            "the tapered arm does not span exactly {full_span} px"
        );
        // THE POINTS: every span past the body is dimmer than the body, they
        // dim MONOTONICALLY outward, and the outermost lands on the shared tip
        // coverage. (Read off the right-hand point, which the mirror repeats.)
        let mut point: Vec<_> = row
            .iter()
            .filter(|q| span(q).0 > sx + b)
            .map(|q| (span(q).0, (q.color >> 16) & 0xff))
            .collect();
        point.sort_unstable();
        assert!(!point.is_empty(), "the arm came to a square end");
        let mut prev = u32::from(cov);
        for (x, c) in &point {
            assert!(
                *c < prev,
                "the point does not taper: {c} at x+{} under a previous {prev}",
                x - sx
            );
            prev = *c;
        }
        // The ramp is sampled at each span's MIDPOINT, so the outermost span
        // never lands exactly on the tip coverage — but it must sit in the
        // ramp's bottom half, between the tip and the ramp's own midpoint.
        let tip = f32::from(cov) * star_taper_cov(1.0);
        let midway = f32::from(cov) * star_taper_cov(0.5);
        assert!(
            (tip..=midway).contains(&(prev as f32)),
            "the outermost span carries {prev}, outside the ramp's bottom half \
             ({tip:.0}..={midway:.0}) — the point is not reaching the shared tip"
        );
        // Nucleus, at the shared core extent and its additive share.
        let c = star_core_px(arm);
        assert_eq!(c, (2.0 * arm as f32 * STAR_CORE).round() as i32);
        let core = premul_rgb(0x00FF_FFFF, (f32::from(cov) * STAR_CORE_ADD) as u8);
        assert!(
            out.iter().any(|q| i32::from(q.w) == c && q.color == core),
            "no nucleus at the shared core extent"
        );
        // Glints: four, on the diagonals, at the shared offset and dimming.
        let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
        let dim = premul_rgb(0x00FF_FFFF, (f32::from(cov) * STAR_GLINT_COV) as u8);
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            assert!(
                out.iter().any(|q| i32::from(q.x) == sx + ox
                    && i32::from(q.y) == sy + oy
                    && q.w == 1
                    && q.h == 1
                    && q.color == dim),
                "no glint at {ox},{oy}"
            );
        }
        // A star with no gold half throws no glints.
        let mut plain = Vec::new();
        assert!(push_twinkle_star(
            &mut plain,
            g,
            sx,
            sy,
            arm,
            cov,
            false,
            0x00FF_FFFF,
            4096
        ));
        assert!(
            plain
                .iter()
                .all(|q| !(q.w == 1 && q.h == 1 && q.color == dim))
        );
    }

    /// THE BODY IS AS BRIGHT AS THE ACCENT IT REPLACED — the additive half of
    /// the stardust law, stated on the COMPOSITED PIXEL.
    ///
    /// The star lays two arms and a nucleus on its centre, so a plus renders
    /// [`STAR_STACK_ADD`] times the coverage its emitter asked for; the mote that
    /// replaced it used to be one flat rect at that same coverage, i.e. 2.35x
    /// darker in the middle for a shape swap that was supposed to change nothing
    /// but the silhouette. Both marks are rasterized here — additively,
    /// saturating at the byte, the way the renderer composites them — and their
    /// centres compared.
    #[test]
    fn the_dust_mote_composites_to_the_plus_s_own_centre() {
        let g = geom();
        let (cx, cy) = (100i32, 40i32);
        let cov = 90u8;
        // Additive rasterization of a small quad batch: `(peak, lit pixels)`.
        let raster = |qs: &[GlowQuad]| -> (u32, usize) {
            let mut px: std::collections::HashMap<(u16, u16), u32> =
                std::collections::HashMap::new();
            for q in qs {
                for yy in q.y..q.y + q.h {
                    for xx in q.x..q.x + q.w {
                        *px.entry((xx, yy)).or_insert(0) += (q.color >> 16) & 0xff;
                    }
                }
            }
            (
                px.values().copied().map(|v| v.min(255)).max().unwrap_or(0),
                px.len(),
            )
        };
        let mut star = Vec::new();
        assert!(push_twinkle_star(
            &mut star,
            g,
            cx,
            cy,
            4,
            cov,
            false,
            0x00FF_FFFF,
            4096
        ));
        let mut mote = Vec::new();
        assert!(push_dust_mote(
            &mut mote,
            g,
            cx,
            cy,
            5,
            cov,
            0x00FF_FFFF,
            4096
        ));
        let (star_peak, star_px) = raster(&star);
        let (mote_peak, mote_px) = raster(&mote);
        // NON-VACUOUS: the plus really does stack, so there is something to match.
        assert!(
            star_peak >= 2 * u32::from(cov),
            "the reference is not stacking: a plus centre of {star_peak} at cov {cov}"
        );
        assert!(
            mote_peak >= star_peak,
            "the mote's centre composites to {mote_peak} against the plus's \
             {star_peak} — the round body is the dimmer mark"
        );
        // …and it did NOT buy that with FOOTPRINT: the hot core lands inside the
        // skirt, so the mote lights exactly the `d x d` it always did and the
        // extra light is in the middle rather than at the edges. (A mote is a
        // filled grain and a plus is two hairlines, so their pixel counts were
        // never equal, which is why this is stated against the mote's own
        // silhouette — and the plus's own count is printed beside it so a
        // failure says which silhouette moved.)
        assert_eq!(
            mote_px, 25,
            "the compensation grew the mote's footprint past its own 5x5 skirt \
             (the plus beside it lights {star_px} px — a different silhouette, \
             never a shared budget)"
        );
        // BUDGET: the helper checks between its two lays, exactly like the star.
        let mut tight = Vec::new();
        assert!(
            !push_dust_mote(&mut tight, g, cx, cy, 5, cov, 0x00FF_FFFF, 1),
            "a mote that ran out of budget must report it"
        );
    }

    /// The BRANCHLESS SHAPE CONTRACT behind [`ramp_5stop`]: stop positions
    /// strictly ascending, running exactly 0.0 → 1.0, in BOTH tables. The
    /// core counts interior stops below `t` instead of walking windows, drops
    /// the walk's `t1 > t0` divide guard, and pins NaN to the 1.0 end — all
    /// three moves lean on this shape, so a future palette retune that breaks
    /// it must fail HERE with a table name, not as a colour glitch on a wake.
    #[test]
    fn ramp_stop_tables_hold_the_branchless_contract() {
        for (name, stops) in [("fire", &FIRE_STOPS), ("water", &WATER_STOPS)] {
            assert!(stops[0].0 == 0.0, "{name}: the ramp must start at 0.0");
            assert!(stops[4].0 == 1.0, "{name}: the ramp must end at 1.0");
            for w in stops.windows(2) {
                assert!(
                    w[1].0 > w[0].0,
                    "{name}: stop positions must strictly ascend ({} then {})",
                    w[0].0,
                    w[1].0
                );
            }
        }
    }

    /// The PRE-REWRITE ramp, VERBATIM (function-local stop table, `windows(2)`
    /// walk, divide guard, raw-crest fall-through): the byte-identity oracle
    /// for the branchless [`ramp_5stop`]. Do NOT "simplify" this to call the
    /// live code — its entire value is being the OLD arithmetic.
    fn walked_ramp(stops: &[(f32, u32); 5], t: f32) -> u32 {
        let t = t.clamp(0.0, 1.0);
        for w in stops.windows(2) {
            let (t0, c0) = w[0];
            let (t1, c1) = w[1];
            if t <= t1 {
                let local = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
                return lerp_rgb(c0, c1, local);
            }
        }
        stops[stops.len() - 1].1
    }

    /// One f32 bit pattern through BOTH live ramps against the walk oracle.
    fn assert_ramps_match_walk(bits: u32) {
        let t = f32::from_bits(bits);
        assert_eq!(
            fire_ramp(t),
            walked_ramp(&FIRE_STOPS, t),
            "fire_ramp diverged from the stop walk at bits {bits:#010x} (t = {t:?})"
        );
        assert_eq!(
            water_ramp(t),
            walked_ramp(&WATER_STOPS, t),
            "water_ramp diverged from the stop walk at bits {bits:#010x} (t = {t:?})"
        );
    }

    /// BYTE-IDENTITY of the driver-03 rewrite: same input, bit-equal u32 out.
    /// These two functions ARE the fire and water palettes, and the frame
    /// fingerprint / volume gates cannot see a one-count colour drift, so the
    /// pin lives at the function itself. Three passes: a prime-strided march
    /// over the ENTIRE f32 bit space (~1M probes — every exponent, both
    /// signs, both infinities, NaN payloads), a dense ±16Ki-ULP band around
    /// every stop position of both tables (where a `<=`-vs-`>` complement
    /// mistake in the window select would surface) plus each band's sign-bit
    /// mirror, and the named specials. The `--ignored` twin below is the
    /// exhaustive version for palette retunes.
    #[test]
    fn ramp_rewrite_is_bit_identical_to_the_stop_walk() {
        // 4099 is prime, so the probes never lock onto a mantissa stride.
        for bits in (0..=u32::MAX).step_by(4099) {
            assert_ramps_match_walk(bits);
        }
        for anchor in [0.0f32, 0.25, 0.35, 0.5, 0.65, 0.75, 0.85, 1.0] {
            let b = anchor.to_bits();
            for bits in b.saturating_sub(16_384)..=b.saturating_add(16_384) {
                assert_ramps_match_walk(bits);
                // The mirror: tiny negatives around -0.0 and every negated
                // anchor all clamp to 0.0, and must clamp IDENTICALLY.
                assert_ramps_match_walk(bits | 0x8000_0000);
            }
        }
        for t in [
            f32::NAN,
            -f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::EPSILON,
            f32::from_bits(1), // the smallest subnormal
            -0.0,
            2.0,
            f32::MAX,
            f32::MIN,
        ] {
            assert_ramps_match_walk(t.to_bits());
        }
    }

    /// The EXHAUSTIVE twin: all 2^32 f32 bit patterns through both ramps, new
    /// against old. Too slow for the default suite by design; run it in
    /// release whenever the ramps or their tables are touched:
    /// `cargo test -p aterm-effects --release -- --ignored bit_identical_exhaustively`
    #[test]
    #[ignore = "2^32-pattern sweep — run in release when touching the ramps"]
    fn ramp_rewrite_is_bit_identical_exhaustively() {
        for bits in 0..=u32::MAX {
            assert_ramps_match_walk(bits);
        }
    }
}
