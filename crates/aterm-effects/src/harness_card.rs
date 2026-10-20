// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness card (`docs/DESIGN-harness-autopilot-2026-10-15.md` §3): the
//! decoration Claude Code's launch card wears while the aterm harness has the
//! session on Autopilot. Owner, 2026-10-15: "a rainbow border and a similar
//! pixel art with an aterm kitty and fancy rainbows and star power to show off
//! the power of the harness".
//!
//! This module is the card's pure emitter. The host finds the card
//! (`aterm_phase::launch_card_place`), resolves its geometry into a
//! [`CardScene`] in the target stream's pixel space, and calls
//! [`HarnessCard::tick`] each frame; the emitter appends [`GlowQuad`]s to two
//! streams and returns a fingerprint. It reads no clock (time comes in as
//! `now`), no terminal and no config, so the same inputs give the same bytes.
//!
//! # The five marks
//!
//! * **Border** (into `under`, drawn beneath glyph ink): a thin rectangle round
//!   the boxed rows, standing half a row clear of them over and under where
//!   the host found the row there blank across the box
//!   ([`CardScene::pad_above`], [`CardScene::pad_below`]: the box's AIR), else
//!   closing on them. Its hue runs along the perimeter, `palette(tri(2s + φ))`
//!   at arc fraction `s` (the doubled `s` spends one whole `tri` period on the
//!   loop, red → violet → red, so the closed perimeter has no seam where it
//!   starts). Top and bottom edges are horizontal gradient segments about a
//!   cell wide; the sides are flat slices, one per row band they cross.
//! * **Rainbow trail** (into `under`): six stripes from the text's end to the
//!   kitty's back, as tall as the kitty's BODY (§10.6: the cat leads, the
//!   rainbow trails), split at the row bands it crosses. It runs on under the
//!   art to the haunch's outline ([`KittyArt::rear`]), so the rainbow meets
//!   the cat flush on every row it spans. Settled, a stripe is one quad end
//!   to end; during an episode it steps and shimmers in segments, but for
//!   the stretch that reaches under the cat, which holds still.
//! * **Kitty** (into `over`): authored pixel art, a chibi kitten in the
//!   spirit of Claude's block mascot — a big round head on a small body,
//!   glossy eyes (a dark pupil, a white catch-light, the iris a band at its
//!   foot), pink ears, nose and blush, an `ω` mouth on a chin of coat, and
//!   white socks on the front paws under it — [`KITTY_W`] pixels wide with
//!   square pixels, each rectangle of one colour (a run of a pixel row,
//!   carried down the rows that repeat it) one opaque source-over quad. Two
//!   heights: [`KITTY_H`] pixels laid two cell rows tall, the cat lying with
//!   its paws tucked, and — where the host found the card's third row blank,
//!   [`CardScene::kitty_rows`] — [`KITTY_TALL_H`] pixels laid three rows
//!   tall, the height of Claude's mascot, the same face under a taller head
//!   on longer legs. Three frames each: eyes open, a happy blink (the eyes
//!   closed in `^ ^` arcs), a tail flick.
//! * **Rim** (into `over`): a one-pixel outline of the border's spectrum just
//!   outside the kitty's silhouette ([`rim_px`]), so a black or grey coat reads
//!   on a dark ground: additive light on dark, opaque print ink on light, its
//!   hue running round the cat as the border's runs round the box. It is cut
//!   where it would lie on the trail, and runs under each paw but not under
//!   the toes' gaps, which open onto the ground.
//! * **Stars** (into `over`): five to nine four-point twinkles at seeded spots
//!   round the kitty and the trail, sparkles the kitty sheds on its ride, and
//!   a burst of [`BURST_STARS`] thrown out from the kitty where it lands.
//!
//! # Episodes, then stillness
//!
//! [`HarnessCard::arm`] starts an episode ([`EPISODE_MS`]): the border's hue
//! drifts (one trip through the palette per [`TRIP_MS`] at first, easing to a
//! stop) and a white-hot head laps the perimeter twice; on the card's first
//! showing, for the episode's first [`RIDE_MS`], the kitty RIDES in (§10.6)
//! — from the trail's start to its seat, easing out, the trail laid behind
//! it to its back, the field's stars coming up as it passes them and
//! sparkles shed as it goes — and lands with the burst; once the glass has
//! shown it seated it plays every later episode in its seat, the burst at
//! the start ([`Ride`]); the trail steps and shimmers; the kitty blinks and flicks its
//! tail; the stars twinkle.
//! When it ends the frame is the settled one: constant bytes, a constant
//! fingerprint and no deadline ([`HarnessCard::deadline`] asks for the
//! settled frame — `now`, until a tick draws it or [`SETTLE_GRACE_MS`] past
//! the episode's end — then is `None`), so the host idles at zero (the
//! upgrade rim's `level_up.rs` lifecycle). Every
//! animated quantity converges on its settled value, so the last episode frame
//! runs into the settled frame without a jump.
//!
//! `animate == false` (reduced motion, an unfocused window, the load-shed
//! latch) gives the settled frame from the first tick and wants no frames,
//! but does not throw the episode away: one not yet drawn animating is OWED
//! and plays from the first tick that may animate, so a latch that forces
//! Reduced just as the card is first found delays the show rather than
//! cancelling it. A tick with nothing to draw (an empty clip, no rows or
//! columns: a pane mid-layout) holds the episode back the same way. The debt
//! lapses [`OWED_MAX_MS`] after the arm, a margin past the latch's dwells: an
//! episode never plays minutes later on an unrelated redraw, and a latch held
//! longer by sustained load forfeits the show. Serious mode ends any
//! episode and also drops the kitty, the trail and the stars, leaving a
//! static border ([`crate::motion::SeriousEffect::HarnessCard`]).
//!
//! # Laws the host can rely on
//!
//! * **One band per quad.** Every quad lies inside exactly one cell-row band,
//!   `[origin_y + r·ch, origin_y + (r + 1)·ch)` for grid row `r`, and is tagged
//!   `row = row_base + r` (the renderers' row gate and the CPU/GPU parity
//!   invariant of [`GlowQuad`]). Side slices, stripes, kitty pixel rows and
//!   star arms are split or laid so none straddles a row boundary.
//! * **Inside the box.** Every quad lies inside the border's outer rectangle
//!   (its air included) and inside the scene's [`clip`](CardScene::clip) (the
//!   pane); the kitty lies inside the border's stroke as it stands with no
//!   air, wherever its ride has it; the stars, the sparkles and the burst
//!   stay in the trail/kitty region, never over the card's text columns (left
//!   of the trail's start). AIR MOVES THE BORDER AND NOTHING ELSE: the trail,
//!   the kitty, its rim and the stars are laid inside the boxed rows' stroke,
//!   so a card that gains or loses its air (a notice drawn under it, a launch
//!   line over it) keeps every one of them where it was.
//! * **Budget.** At most [`MAX_CARD_QUADS`] quads per tick across both streams,
//!   spent border → trail → kitty → rim → burst → stars → sparkles (the
//!   burst before the field: a frame over the cap loses a twinkling field
//!   star, never one of the burst's ring); a star is drawn whole or not at
//!   all, to the quad (one the budget cannot finish is taken back).
//! * **Themes.** On a dark theme the border, trail, rim and stars are additive
//!   light (`alpha == 0`); on a light theme every quad is source-over
//!   (`alpha > 0`), since additive light is invisible on white. There the
//!   border, trail and rim are opaque card art in print inks
//!   ([`PRINT_CRUISE`], [`PRINT_LAND`]), as a printer lays a rainbow on
//!   paper: none of them ever lies on a glyph, so they leave the light-ink law
//!   (`cursor_glow`'s `InkRole`), which washes a hue out to keep text
//!   legible under it. The stars, the sparkles and the burst join them (one
//!   printed piece, §10.6.10): they are clipped to cells the host read blank,
//!   so they too never touch text; on the paper they are opaque print ink,
//!   on the trail the paper's white. The line IS text: each letter is its
//!   print ink in the paper law's point-mark ink (`rainbow_kitty::ribbon`'s
//!   `paper_mark_ink`), WCAG AA on its ground. The kitty is opaque art
//!   (`alpha == 255`) on both.
//! * **Photosensitivity.** Every twinkle runs on `effect_util::twinkle_env`,
//!   under the crate's certified 3.2 Hz bound; the trail's step wave cycles
//!   below it too (both const-asserted here). The burst only fades, and the
//!   blink comes once per 2.5 s.

use aterm_render::{GlowQuad, InkCell, premul_rgb};
use aterm_time::Instant;
use std::time::Duration;

use crate::color_math::relative_luminance;
use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE_ADD, STAR_GLINT,
    STAR_GLINT_COV, STAR_TAPER_STEPS, TWINKLE_FLASH_BOUND_HZ, TWINKLE_FLASH_HZ, mix_rgb, star_arm,
    star_arm_px, star_body_px, star_core_px, star_taper_cov, star_waist_px, twinkle_env,
    twinkle_peak, twinkle_rgb,
};
use crate::genome::mix;
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::ribbon::paper_mark_ink;
use crate::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN, spectrum, spectrum_stop};

// ─── the clock ──────────────────────────────────────────────────────────────

/// One episode's length (ms): the card animates this long after
/// [`HarnessCard::arm`], then holds its settled frame.
pub const EPISODE_MS: u64 = 6000;

/// The animation cadence (ms, ~30 fps): [`HarnessCard::deadline`]'s step and
/// the fingerprint's frame quantum while an episode runs.
pub const FRAME_MS: u64 = 33;

/// How long past an episode's end (ms) [`HarnessCard::deadline`] keeps
/// asking for the settled frame, `now`, when no tick has drawn it. A host
/// that ticks on its deadline draws the settled frame a frame after the end,
/// well inside this; one that stopped ticking (the frame scrolled back, the
/// card's version no longer the process's, the pane hidden) but still asks
/// each loop is asked to wake at once for at most this long, then is let
/// idle — never a spin for as long as the card goes unticked. A host later
/// than this draws the settled frame on its next wake.
pub const SETTLE_GRACE_MS: u64 = 500;

/// How long after its arm (ms) an OWED episode — held back before any of it
/// was drawn — may still start. It covers the load-shed latch's dwells: the
/// GUI's latch (`app_render.rs`) holds Reduced AT LEAST its dwell, backed off
/// up to `PERF_SHED_DWELL_MAX` (30 s) when it flaps, and restores only after
/// three qualifying frames past it — so a ceiling-long shed clears a little
/// AFTER 30 s, and the margin here (15 s) lets a latch that forced Reduced as
/// the card was first found still pay the debt when it clears. A latch held
/// longer than that by sustained load forfeits the show: the debt lapses,
/// and the next tick that may animate draws the settled frame, so the show
/// never plays minutes or hours after the moment it marked (the card found,
/// the Autopilot changed) on some unrelated redraw. A fresh
/// [`HarnessCard::arm`] (focus regained, the Autopilot changed) owes a fresh
/// one.
pub const OWED_MAX_MS: u64 = 45_000;

/// The most quads one [`HarnessCard::tick`] appends across both streams.
///
/// The parts: the border ≤ 96 edge segments plus two side slices per row
/// band it crosses (the boxed rows, and one either side in each half row of
/// air over and under them: four more at most); the trail six stripes
/// settled, ≤ 16 segments of six (each split at the row bands it crosses)
/// during an episode; the kitty's rectangles (about 95 two rows tall, 110 to
/// 120 three: the chibi face's detail repeats down few rows) and its rim
/// (about 55); and up to seventeen stars (the field's nine and the burst's
/// eight: the ride's five sparkles are out before the burst opens). Each
/// quad is a GPU instance (a rectangle on the CPU raster).
///
/// Measured (2026-10-19, launch cards on an 8- and a 20-cell trail, two rows
/// tall then three): the settled frame — what a card at rest replays into
/// every frame, typing included — is 316 to 335 quads at 9 × 18 and 356 to
/// 384 at 16 × 33 and 17 × 35, 37 to 56 more than the kitty before (279 to
/// 286 and 319 to 327), all of it the art's rectangles and rim; its air
/// over and under the box adds the four side slices (320 to 339, 360 to
/// 388). The busiest frame (the burst opening as the ride ends, beside the
/// field and the stepping trail) wants at most 552 at 9 × 18, 634 at 16 ×
/// 33 and 645 at 17 × 35 over 300 seeds, three rows on a 20-cell trail in
/// air (548, 630 and 641 without), so the cap leaves a whole star (34
/// quads) to spare at the shipping cell sizes (`the_quad_budget_holds`); at
/// any cell size (8 to 96 px tall) no launch card wants more than 674, in
/// air at 25 × 53 (670 without), so the cap binds on none and no field star
/// waits on it. It is the backstop for a pathological scene (hundreds of
/// boxed rows), spent in the order border → trail → kitty → rim → burst →
/// stars → sparkles, each star whole or not at all: a frame over it loses a
/// field star, never one of the burst's ring
/// (`the_burst_stays_whole_under_the_cap`).
pub const MAX_CARD_QUADS: usize = 680;

/// The border's hue drift at an episode's start: one trip through the palette
/// (`φ` moves by 1) per this many ms, easing to a stop at the episode's end.
pub const TRIP_MS: u64 = 3000;

/// How far `φ` drifts over the episode: the integral of a speed that starts at
/// one trip per [`TRIP_MS`] and eases linearly to zero, `EPISODE / (2·TRIP)`.
const PHI_DRIFT: f32 = EPISODE_MS as f32 / (2.0 * TRIP_MS as f32);

/// The settled border phase. Red sits at the top-left corner and the loop
/// reaches violet halfway round.
const PHI_SETTLED: f32 = 0.0;

// ─── the border ─────────────────────────────────────────────────────────────

/// The specular head's laps of the perimeter per episode.
const HEAD_LAPS: f32 = 2.0;

/// The head's gaussian width as a fraction of the perimeter: a few cells on a
/// launch card's border.
const HEAD_SIGMA: f32 = 0.03;

/// How far the head pulls the border toward white at its centre (dark), or
/// toward its own deeper shade (light: a white head would be a hole in white).
const HEAD_GAIN: f32 = 0.85;

/// The light theme's head: the share by which the ink deepens toward black.
const LIGHT_HEAD_DEEPEN: f32 = 0.3;

/// Most gradient segments per horizontal edge; a wide pane widens the segments
/// rather than spending the budget on more of them.
const MAX_EDGE_SEGMENTS: i64 = 48;

/// The dark border's coverage (additive light).
const BORDER_COV: u8 = 230;

// ─── the trail ──────────────────────────────────────────────────────────────

/// The trail's six stripes as named spectrum stops: red, orange, yellow,
/// green, blue, violet (indigo skipped, as on the flag the rainbow kitty
/// flies).
const TRAIL_STOPS: [usize; 6] = [0, 1, 2, 3, 4, 6];

/// The trail's step-wave segment, in cells (wider when the trail is long, see
/// [`MAX_TRAIL_SEGMENTS`]).
const TRAIL_SEG_CELLS: i64 = 2;

/// Most step-wave segments on one trail.
const MAX_TRAIL_SEGMENTS: i64 = 16;

/// The step wave's beat (ms): segments alternate up and down and swap each
/// beat, the rainbow kitty's flag in miniature.
const WAVE_STEP_MS: u64 = 200;

/// The wave stops this long before the episode ends, so the stripes are
/// already at rest when the frame settles.
const WAVE_END_MS: u64 = EPISODE_MS - 600;

/// The trail's shimmer: one bright sweep along it per this many ms.
const SHIMMER_MS: f32 = 1500.0;

/// The shimmer's gaussian width as a fraction of the trail.
const SHIMMER_SIGMA: f32 = 0.12;

/// How far the shimmer lifts a stripe toward white at its peak, on both
/// grounds: light on dark, a gloss on a light ground's print ink.
const SHIMMER_GAIN: f32 = 0.45;

/// The dark trail's coverage (additive light).
const TRAIL_COV: u8 = 200;

// ─── the light ground: print inks ───────────────────────────────────────────
//
// On a light theme the border, the trail and the rim are CARD ART, laid as a
// printer lays a rainbow on white paper: opaque process-style inks at full
// chroma for their lightness, crisp-edged, the shimmer a gloss. They leave
// the shared light-ink law (`cursor_glow`'s `InkRole`) on purpose. That law
// is for marks that can composite over a letterform: it re-mixes every hue
// to one luma (`light_ink_bold`), which lets white back into red, blue and
// violet (salmon, periwinkle, lavender) and darkens yellow and green toward
// olive, and its translucent alpha lets the paper through again — legible
// over text, washed out as art. None of these three ever lies on a glyph:
// the trail fills cells the host read blank beside the card's text
// (`claude_card.rs`'s layout), the rim rings the kitty on those same cells,
// and the border is drawn under glyph ink (`under`) on the box's edge. So
// they are opaque like the kitty. The stars, the sparkles, the burst and the
// line join them below ("one printed piece").
//
// Three refinements keep the print crisp: the border and the rim run a ramp
// with two knots between the stripes' inks ([`PRINT_CRUISE_RAMP`]), so the
// gold-to-green and green-to-blue passages stay bright instead of going olive
// and grey; the one-pixel border alone is darkened to a luma bar
// ([`LIGHT_LINE_MAX_LUMA`]) so its gold reads on white; and the gloss is
// halved on the band's top and bottom stripes ([`LIGHT_GLINT_EDGE`]) so the
// silhouette holds while it passes. Each is a table read or a few multiplies
// per paint, cheaper than the `light_ink_bold` call each paint made before.

/// Cruise's six stripes on a light ground (red, orange, golden yellow, green,
/// blue, violet): each within a few degrees of OKLCh hue of its dark twin
/// ([`TRAIL_STOPS`]' spectrum stop) but the yellow, which leans gold —
/// lemon vanishes on white and cream, and lemon darkened goes mustard. The
/// green and the violet are lifted toward the dark card's luminous green and
/// violet (OKLCh L 0.69 and 0.49), so the two read as one rainbow; the
/// violet still holds the band's bottom edge at 6:1 or more on every light
/// ground.
const PRINT_CRUISE: [u32; 6] = [
    0x00E3_141B, // red
    0x00FF_7A00, // orange
    0x00FF_C20E, // golden yellow
    0x0022_B848, // green
    0x0010_60D8, // blue
    0x0082_26C0, // violet
];

/// The light border's and rim's knot between Cruise's gold and green: a
/// peridot. Mixed straight, the gold and the green meet in an olive.
const PRINT_PERIDOT: u32 = 0x0080_C800;

/// The light border's and rim's knot between Cruise's green and blue: a teal
/// at about the most chroma sRGB holds for its hue and lightness. Mixed
/// straight, the green and the blue meet in a greyish teal (OKLCh chroma
/// 0.09); through this knot the passage keeps 0.11 or more.
const PRINT_TEAL: u32 = 0x0000_A6C4;

/// Cruise's light ramp for the border and the rim, `(spectrum table index,
/// colour)`: each [`PRINT_CRUISE`] ink at its stripe's spectrum stop
/// ([`SPECTRUM_ANCHOR_AT`]), so a hue sits on the box and round the cat where
/// it does on dark, with [`PRINT_PERIDOT`] and [`PRINT_TEAL`] halfway between
/// their neighbours. Read as [`spectrum`] is: two knots and one mix.
const PRINT_CRUISE_RAMP: [(usize, u32); 8] = [
    (stripe_at(0), PRINT_CRUISE[0]),
    (stripe_at(1), PRINT_CRUISE[1]),
    (stripe_at(2), PRINT_CRUISE[2]),
    ((stripe_at(2) + stripe_at(3)) / 2, PRINT_PERIDOT),
    (stripe_at(3), PRINT_CRUISE[3]),
    ((stripe_at(3) + stripe_at(4)) / 2, PRINT_TEAL),
    (stripe_at(4), PRINT_CRUISE[4]),
    (stripe_at(5), PRINT_CRUISE[5]),
];

/// Where Cruise's trail stripe `k` sits in the spectrum table: its stop's
/// index ([`SPECTRUM_ANCHOR_AT`]).
const fn stripe_at(k: usize) -> usize {
    SPECTRUM_ANCHOR_AT[TRAIL_STOPS[k]]
}

// The ramp spans the whole table, its knots in order: `knot_ramp` reads it
// so.
const _: () = {
    let r = &PRINT_CRUISE_RAMP;
    assert!(r[0].0 == 0 && r[r.len() - 1].0 == SPECTRUM_LUT_LEN - 1);
    let mut i = 1;
    while i < r.len() {
        assert!(r[i - 1].0 < r[i].0, "the light ramp's knots run in order");
        i += 1;
    }
};

/// Land's six stripes on a light ground, the sunset in print: Cruise's red,
/// a vermilion, an orange, the gold of the horizon, a coral and a rose — a
/// tonal arc that peaks at the gold. Six inks rather than [`SUNSET`]'s
/// ramp: its fifth stripe, two fifths of the way from gold to rose, is a pale
/// apricot as light on dark but nearly the third's orange in opaque ink
/// (OKLab ΔE 0.044), so on paper it is the coral that lies further on toward
/// the rose.
const PRINT_LAND: [u32; 6] = [
    0x00E3_141B, // red
    0x00F7_591C, // vermilion
    0x00FF_8C00, // orange
    0x00FF_B81C, // gold
    0x00FF_6F59, // coral
    0x00E8_336D, // rose
];

/// Print art's opacity: opaque, as the kitty is.
const PRINT_ALPHA: f32 = 255.0;

// ─── the light ground: one printed piece ────────────────────────────────────
//
// The stars, the sparkles, the burst and the line join the print rainbow on
// a light ground: one family with the trail, the border and the rim, where
// the shared light-ink law (`InkRole::OverText`: every hue to luma 68, alpha
// at most 190) made them brownish and see-through beside it.
//
// THE MARKS never touch a glyph, so they leave the law as the trail did. Each
// is clipped to the star region ([`star_region`]): on a light ground, the
// trail's start to the border's column on the card's first two rows, cells
// the host lays the card on only where every one read blank
// (`claude_card.rs`'s `layout`: the kitty and its trail need `cols <= limit`,
// the blank run on both rows, so the border's column itself may hold a
// glyph and the region stops short of it) and lays again when a cell it read
// changes, so no text is ever under one.
// On the paper a mark is opaque print ink, its stripe's ink in turn
// ([`CardPalette::star_ink`]), held to the border's luma bar
// ([`print_line`]) because it is as thin as the border; on the trail's ink
// it is the paper's white, a glint cut out of the print (as a star on dark
// adds white light to the trail), so an ink star never vanishes into its own
// stripe. The tone is the pixel's, not the star's ([`lay_toned`], the
// trail's ink segment by segment, [`Emit::trail_ink`]): a star that crosses
// the trail's edge, as the burst's do as they fly out over the kitty, is
// whole, the paper's white where it lies on the print and ink where it lies
// off it, never cut into half a star where print meets paper. A steady star
// keeps one tone: the field is placed on one side of the edge or the other
// ([`field_spots`]), and a sparkle the ride sheds on the fresh trail is moved
// wholly onto its ink where the band has room ([`Emit::sparkles`]). A
// glinting star's four diagonal dots are its own ink at the
// body's strength ([`lay_star`]): at a third of it, as added light takes them
// on dark, they let the paper through, a pale pink or tan halo. They do not take
// the paper law's point-mark ink, as the line does: its 4.5:1 is for a mark
// a letter may lie under, and none does here, while at 4.5:1 the gold star
// prints bronze (`#A56500` on Aterm Day) beside the gold stripe it echoes.
//
// THE LINE IS TEXT, and its letters take aterm's one paper law for a mark
// a reader must read through (`rainbow_kitty::ribbon`'s `paper_mark_ink`,
// the cursor kitty's stars and sparks): the print ink at the letter's place
// on the ramp ([`CardPalette::art_at`]) at full value, yellow bent to amber,
// darkened along its own hue only as far as 4.5:1 against the pane's ground
// needs, opaque. So each letter is the trail's hue at text weight, WCAG AA
// on every light ground, a custom one too (the law reads the ground it is
// given); red, blue and violet already clear the bar and stay pure. The law
// is solved once per palette and ground, on a light tick, and kept on the
// card ([`LineInks`]): a light frame's line is a lookup a letter.

/// The opacity a light mark reaches at a settled star's coverage
/// ([`STAR_COV`] × [`STAR_STEADY_ENV`]): from there up it is opaque print,
/// below it (a twinkle's dip, a burst's fade, a point's taper) it lets the
/// ground through in proportion.
const STAR_PRINT_FULL: f32 = STAR_COV * STAR_STEADY_ENV;

/// The share of the gloss the trail's top and bottom stripes take on a light
/// ground: a full gloss turns the red top pink and the bottom lavender as it
/// passes, softening the band's edge on paper, so the edges shine half as
/// much and the silhouette stays crisp while the centre shines.
const LIGHT_GLINT_EDGE: f32 = 0.5;

/// The light border's ceiling on Rec. 709 luma (0..255): its ramp colour
/// darkened toward black, never whitened, to at most this. A one-pixel gold
/// line is about 1.6:1 on white and fades; at this bar it reads as gold. Only
/// the border's warm middle is above it; the trail and the rim, wider marks,
/// keep the full ink.
const LIGHT_LINE_MAX_LUMA: f32 = 180.0;

// ─── the kitty ──────────────────────────────────────────────────────────────

/// The kitty bitmap's width in pixels, at both heights.
pub const KITTY_W: usize = 24;

/// The two-row kitty's height in pixels: two cell rows of [`KITTY_HALF`].
pub const KITTY_H: usize = 14;

/// The tall kitty's height in pixels (§10.6): three cell rows of
/// [`KITTY_HALF`], the height of Claude's three-row mascot.
pub const KITTY_TALL_H: usize = 21;

/// Pixel rows per cell row, at both heights: the kitty's square pixel is the
/// largest that lays this many inside one band less the border's stroke
/// ([`kitty_px`]). Both kitties' first this many pixel rows end on the line
/// between the card's first two rows ([`kitty_place`]): the two-row kitty's
/// other half lies in the second row's band, so no pixel row of it straddles
/// a boundary; the tall one's rest hangs below, and the sink splits a
/// rectangle at the bands it crosses.
const KITTY_HALF: usize = KITTY_H / 2;

/// One two-row kitty frame: [`KITTY_H`] rows of [`KITTY_W`] palette
/// characters.
type KittyFrame = [&'static str; KITTY_H];

/// One tall kitty frame: [`KITTY_TALL_H`] rows of [`KITTY_W`] palette
/// characters.
type KittyTallFrame = [&'static str; KITTY_TALL_H];

/// The kitty's palette characters: `.` is transparent, the rest are its
/// colours (see [`KittyInk::of`]).
const KITTY_PALETTE: &[u8] = b".ocbkenw";

/// The kitty at rest, eyes open, tail up: a chibi kitten facing right, riding
/// the rainbow — a big round head over a small body, its haunch on the
/// trail. `o` outline, `c` coat, `k` pupil and eye ring, `w` catch-light,
/// `e` iris, `n` inner ears, nose and blush, `b` the front paws' socks. Each
/// eye is mostly pupil, a white catch-light at its top left and the iris a
/// band near its foot, so at three or four device pixels an art pixel it
/// reads as a glossy eye rather than a ring (goggles); the `ω` mouth sits
/// under the nose on the chin, a row of coat between it and the front paws,
/// which peek out below in their socks — the cat lies on the rainbow, its
/// paws tucked under its chin, since a sock right under the mouth reads as a
/// fang ([`mouth_on_coat`]); the tail is two pixels of coat wide, its tip
/// hooked back toward the trail.
const KITTY_OPEN: KittyFrame = [
    ".........o...........o..",
    ".........oo.........oo..",
    "oo.......ono.......ono..",
    "oco......onnooooooonno..",
    "occo....occccccccccccco.",
    "occo...occckkccccckkccco",
    ".occoooocckwkkccckwkkcco",
    ".occcccocckkkkccckkkkcco",
    ".occcccocckeekccckeekcco",
    ".occcccoccckkccncckkccco",
    ".occcccocnnccocococcnnco",
    ".occccccooccccococcccoo.",
    "..occccccooccccccccco...",
    "...oo..oo...obbo.obbo...",
];

/// The blink: [`KITTY_OPEN`] with its eyes closed in happy arcs, `^ ^`, a
/// row of coat (the nose's) between them and the mouth.
const KITTY_BLINK: KittyFrame = [
    ".........o...........o..",
    ".........oo.........oo..",
    "oo.......ono.......ono..",
    "oco......onnooooooonno..",
    "occo....occccccccccccco.",
    "occo...occccccccccccccco",
    ".occooooccccccccccccccco",
    ".occcccocccoocccccooccco",
    ".occcccoccoccocccoccocco",
    ".occcccocccccccnccccccco",
    ".occcccocnnccocococcnnco",
    ".occccccooccccococcccoo.",
    "..occccccooccccccccco...",
    "...oo..oo...obbo.obbo...",
];

/// The tail flick: [`KITTY_OPEN`] with the tail swept flat back into the
/// trail.
const KITTY_FLICK: KittyFrame = [
    ".........o...........o..",
    ".........oo.........oo..",
    ".........ono.......ono..",
    ".........onnooooooonno..",
    "........occccccccccccco.",
    ".......occckkccccckkccco",
    "oooooooocckwkkccckwkkcco",
    "cccccccocckkkkccckkkkcco",
    "cccccccocckeekccckeekcco",
    "oocccccoccckkccncckkccco",
    ".occcccocnnccocococcnnco",
    ".occccccooccccococcccoo.",
    "..occccccooccccccccco...",
    "...oo..oo...obbo.obbo...",
];

/// The tall kitty at rest (§10.6): [`KITTY_OPEN`]'s cat stood three rows
/// tall — the same ears, the head four rows taller above the same face (its
/// eyes, nose, blush and mouth row for row the two-row cat's), and a body
/// that stands: the haunch under the head, longer legs, white socks on the
/// front paws alone (the hind paws plain coat, quieter on a black coat over
/// a dark ground), and between the legs the ground.
const KITTY_TALL_OPEN: KittyTallFrame = [
    ".........o...........o..",
    ".........oo.........oo..",
    "oo.......ono.......ono..",
    "oco......onnooooooonno..",
    "occo....occccccccccccco.",
    "occo....occccccccccccco.",
    "occo....occccccccccccco.",
    "occo...occccccccccccccco",
    "occo...occccccccccccccco",
    "occo...occckkccccckkccco",
    ".occoooocckwkkccckwkkcco",
    ".occcccocckkkkccckkkkcco",
    ".occcccocckeekccckeekcco",
    ".occcccoccckkccncckkccco",
    ".occcccocnnccocococcnnco",
    ".occccccooccccococcccoo.",
    ".occcccccooccccccccco...",
    "..occcccccccocco.occo...",
    "...occcoccccobbo.obbo...",
    "...occo.occo.oo...oo....",
    "....oo...oo.............",
];

/// The tall blink: [`KITTY_TALL_OPEN`] with its eyes closed in happy arcs,
/// as [`KITTY_BLINK`] closes them.
const KITTY_TALL_BLINK: KittyTallFrame = [
    ".........o...........o..",
    ".........oo.........oo..",
    "oo.......ono.......ono..",
    "oco......onnooooooonno..",
    "occo....occccccccccccco.",
    "occo....occccccccccccco.",
    "occo....occccccccccccco.",
    "occo...occccccccccccccco",
    "occo...occccccccccccccco",
    "occo...occccccccccccccco",
    ".occooooccccccccccccccco",
    ".occcccocccoocccccooccco",
    ".occcccoccoccocccoccocco",
    ".occcccocccccccnccccccco",
    ".occcccocnnccocococcnnco",
    ".occccccooccccococcccoo.",
    ".occcccccooccccccccco...",
    "..occcccccccocco.occo...",
    "...occcoccccobbo.obbo...",
    "...occo.occo.oo...oo....",
    "....oo...oo.............",
];

/// The tall tail flick: [`KITTY_TALL_OPEN`] with the tail swept flat back
/// into the trail, as [`KITTY_FLICK`] sweeps it.
const KITTY_TALL_FLICK: KittyTallFrame = [
    ".........o...........o..",
    ".........oo.........oo..",
    ".........ono.......ono..",
    ".........onnooooooonno..",
    "........occccccccccccco.",
    "........occccccccccccco.",
    "........occccccccccccco.",
    ".......occccccccccccccco",
    ".......occccccccccccccco",
    ".......occckkccccckkccco",
    "oooooooocckwkkccckwkkcco",
    "cccccccocckkkkccckkkkcco",
    "cccccccocckeekccckeekcco",
    "oocccccoccckkccncckkccco",
    ".occcccocnnccocococcnnco",
    ".occccccooccccococcccoo.",
    ".occcccccooccccccccco...",
    "..occcccccccocco.occo...",
    "...occcoccccobbo.obbo...",
    "...occo.occo.oo...oo....",
    "....oo...oo.............",
];

/// One height of the kitty: its three frames — open, blink, flick — its eye
/// rows, `[first, end)` (the only rows the blink changes), and its body band,
/// `(first pixel row, pixel rows)` from its back's top outline down: the band
/// the trail's six stripes span (§10.6: the cat leads, the rainbow trails).
#[derive(Clone, Copy, Debug)]
struct KittyArt {
    open: &'static [&'static str],
    blink: &'static [&'static str],
    flick: &'static [&'static str],
    eyes: (usize, usize),
    body: (usize, usize),
}

impl KittyArt {
    /// The art's height in pixels.
    const fn h(&self) -> usize {
        self.open.len()
    }

    /// How many cell rows it is laid over.
    const fn rows(&self) -> usize {
        self.h() / KITTY_HALF
    }

    /// The face's last pixel row, the `ω`'s feet: the face is the
    /// [`KITTY_HALF`] rows from the eyes' top, at both heights the same.
    const fn mouth_row(&self) -> usize {
        self.eyes.0 + KITTY_HALF - 1
    }

    /// The column the trail runs to under the art ([`KittyAt::rear`]): the
    /// furthest in of the body band's rows' first pixels in the open frame,
    /// so the rainbow meets the haunch flush on every row it spans. The
    /// art's own opaque pixels hide the stripes left of it on a row that
    /// starts sooner ([`trail_meets_flush`]).
    const fn rear(&self) -> usize {
        let mut rear = 0;
        let mut r = self.body.0;
        while r < self.body.0 + self.body.1 {
            let first = first_art(self.open[r].as_bytes());
            if first > rear {
                rear = first;
            }
            r += 1;
        }
        rear
    }
}

/// The two-row kitty.
const KITTY_SHORT: KittyArt = KittyArt {
    open: &KITTY_OPEN,
    blink: &KITTY_BLINK,
    flick: &KITTY_FLICK,
    eyes: (5, 10),
    body: (6, 7),
};

/// The tall kitty (§10.6).
const KITTY_TALL: KittyArt = KittyArt {
    open: &KITTY_TALL_OPEN,
    blink: &KITTY_TALL_BLINK,
    flick: &KITTY_TALL_FLICK,
    eyes: (9, 14),
    body: (10, 8),
};

/// The flick changes only the columns left of this one: the tail.
const KITTY_TAIL_COLS: usize = 4;

/// Whether every row of `frame` is [`KITTY_W`] palette characters, and there
/// are `h` of them. A `const fn` so a mis-drawn row stops the build.
const fn frame_is_well_formed(frame: &[&str], h: usize) -> bool {
    if frame.len() != h {
        return false;
    }
    let mut r = 0;
    while r < h {
        let row = frame[r].as_bytes();
        if row.len() != KITTY_W {
            return false;
        }
        let mut c = 0;
        while c < KITTY_W {
            let mut known = false;
            let mut k = 0;
            while k < KITTY_PALETTE.len() {
                known |= row[c] == KITTY_PALETTE[k];
                k += 1;
            }
            if !known {
                return false;
            }
            c += 1;
        }
        r += 1;
    }
    true
}

/// Whether `a` and `b` (well formed, the same height) differ only inside
/// rows `rows` and columns `cols`.
const fn differ_only_in(
    a: &[&str],
    b: &[&str],
    rows: (usize, usize),
    cols: (usize, usize),
) -> bool {
    let mut r = 0;
    while r < a.len() {
        let (x, y) = (a[r].as_bytes(), b[r].as_bytes());
        let mut c = 0;
        while c < KITTY_W {
            let inside = r >= rows.0 && r < rows.1 && c >= cols.0 && c < cols.1;
            if !inside && x[c] != y[c] {
                return false;
            }
            c += 1;
        }
        r += 1;
    }
    true
}

/// The first art (not transparent) column of a pixel row, [`KITTY_W`] where
/// it has none.
const fn first_art(row: &[u8]) -> usize {
    let mut c = 0;
    while c < KITTY_W && row[c] == b'.' {
        c += 1;
    }
    c
}

/// Whether, on every row of `body` in `frame`, the pixels from the row's
/// first to column `rear` are all art: the trail laid to `rear` under the
/// cat shows left of its silhouette only, never through a hole inside it.
const fn trail_meets_flush(frame: &[&str], body: (usize, usize), rear: usize) -> bool {
    let mut r = body.0;
    while r < body.0 + body.1 {
        let row = frame[r].as_bytes();
        let mut c = first_art(row);
        while c < rear {
            if row[c] == b'.' {
                return false;
            }
            c += 1;
        }
        r += 1;
    }
    true
}

/// Whether the mouth on `frame`'s row `r` stands on coat: under each of its
/// pixels — the row's outline pixels with coat either side, the `ω`'s feet —
/// and under the coat either side of it, row `r + 1` is coat, neither a sock
/// nor an outline, so nothing under the mouth reads as a tooth. False where
/// the row holds no mouth, so a check pointed at the wrong row fails.
const fn mouth_on_coat(frame: &[&str], r: usize) -> bool {
    let (row, under) = (frame[r].as_bytes(), frame[r + 1].as_bytes());
    let mut mouth = 0;
    let mut c = 1;
    while c + 1 < KITTY_W {
        if row[c] == b'o' && row[c - 1] == b'c' && row[c + 1] == b'c' {
            if under[c - 1] != b'c' || under[c] != b'c' || under[c + 1] != b'c' {
                return false;
            }
            mouth += 1;
        }
        c += 1;
    }
    mouth > 0
}

/// How many times palette character `ch` appears in `frame`'s rows `rows`.
const fn count_in(frame: &[&str], rows: (usize, usize), ch: u8) -> usize {
    let mut n = 0;
    let mut r = rows.0;
    while r < rows.1 {
        let row = frame[r].as_bytes();
        let mut c = 0;
        while c < KITTY_W {
            n += (row[c] == ch) as usize;
            c += 1;
        }
        r += 1;
    }
    n
}

/// THE SHAPE CHECKS, at both heights: every frame [`KITTY_W`] palette
/// characters by its art's height, a whole number of cell rows; the blink is
/// the open frame with its eyes closed (the eye rows alone change, and no
/// pixel of an eye — pupil, catch-light or iris — is left); the flick is the
/// open frame with its tail moved (the columns left of [`KITTY_TAIL_COLS`]
/// alone change); the open cat has eyes, pupils and irises; the mouth
/// stands on a row of coat in every frame ([`mouth_on_coat`]), so the front
/// paws never stand right under it; the body band lies inside the art and
/// holds the trail's six stripes; and the trail laid to the art's
/// [`rear`](KittyArt::rear) meets the cat flush in every frame, within the
/// tail's columns.
const _: () = {
    let arts = [KITTY_SHORT, KITTY_TALL];
    let heights = [KITTY_H, KITTY_TALL_H];
    let mut i = 0;
    while i < arts.len() {
        let art = arts[i];
        let h = heights[i];
        assert!(
            frame_is_well_formed(art.open, h)
                && frame_is_well_formed(art.blink, h)
                && frame_is_well_formed(art.flick, h),
            "every kitty row is KITTY_W palette characters, the art's height of them"
        );
        assert!(
            art.h().is_multiple_of(KITTY_HALF) && art.rows() == 2 + i,
            "the kitty is a whole number of cell rows: two, then three"
        );
        assert!(
            differ_only_in(art.open, art.blink, art.eyes, (0, KITTY_W))
                && count_in(art.blink, art.eyes, b'e') == 0
                && count_in(art.blink, art.eyes, b'k') == 0
                && count_in(art.blink, art.eyes, b'w') == 0
                && count_in(art.open, art.eyes, b'e') > 0
                && count_in(art.open, art.eyes, b'k') > 0,
            "the blink is the open frame with its eyes closed"
        );
        assert!(
            differ_only_in(art.open, art.flick, (0, h), (0, KITTY_TAIL_COLS)),
            "the flick is the open frame with its tail moved"
        );
        let mouth = art.mouth_row();
        assert!(
            mouth + 1 < h
                && mouth_on_coat(art.open, mouth)
                && mouth_on_coat(art.blink, mouth)
                && mouth_on_coat(art.flick, mouth),
            "the mouth stands on a row of coat, the front paws a row below"
        );
        assert!(
            art.body.0 + art.body.1 <= h && art.body.1 >= 6,
            "the body band lies in the art and holds six stripes"
        );
        let rear = art.rear();
        assert!(
            rear < KITTY_TAIL_COLS
                && trail_meets_flush(art.open, art.body, rear)
                && trail_meets_flush(art.blink, art.body, rear)
                && trail_meets_flush(art.flick, art.body, rear),
            "the trail meets the cat's rear flush, no hole in it"
        );
        i += 1;
    }
    assert!(
        2 * KITTY_HALF == KITTY_H && 3 * KITTY_HALF == KITTY_TALL_H,
        "the kitty splits evenly across its rows"
    );
};

/// The first blink (ms into an episode) and the gap to the next: one every
/// [`BLINK_PERIOD_MS`].
const BLINK_AT_MS: u64 = 1250;
const BLINK_PERIOD_MS: u64 = 2500;
/// How long the eyes stay shut.
const BLINK_MS: u64 = 160;

/// The first tail flick (ms into an episode) and the gap to the next; the
/// flick windows sit between the blinks, so the two never share a frame.
const FLICK_AT_MS: u64 = 400;
const FLICK_PERIOD_MS: u64 = 2500;
/// How long the tail stays swept back.
const FLICK_MS: u64 = 240;

const _: () = assert!(
    FLICK_AT_MS + FLICK_MS <= BLINK_AT_MS
        && BLINK_AT_MS + BLINK_MS <= FLICK_AT_MS + FLICK_PERIOD_MS
        && BLINK_PERIOD_MS == FLICK_PERIOD_MS,
    "a blink and a flick never share a frame"
);

/// The kitty's nose, inner ears and blush.
const NOSE_PINK: u32 = 0x00FF_8FAB;

/// The kitty's pupils and eye rings (`k`), on every coat and both grounds:
/// a near-black a shade under `cat_baker::COAT_RAMP`'s blackest coat, so
/// the eye is a dark gloss on a grey or an orange coat (the grey coat's
/// iris is about as light as its fur, and without a dark pupil the eye
/// goes), and not the very fur of the blackest one, whose eyes the
/// catch-light and the iris band carry. Not the outline's colour: on a
/// near-black coat over a dark ground the outline turns light grey, which
/// would ring each eye like goggles.
const PUPIL: u32 = 0x000A_0A0E;

/// The dark rim's coverage (additive light), the border's own.
const RIM_COV: u8 = BORDER_COV;

// ─── the stars ──────────────────────────────────────────────────────────────

/// The steady field's star count is `STARS_MIN + seed % STARS_SPAN` (five to
/// nine), fewer only where the region has no room.
const STARS_MIN: u64 = 5;
const STARS_SPAN: u64 = 5;
/// The field's largest possible count.
const STARS_MAX: usize = (STARS_MIN + STARS_SPAN - 1) as usize;

/// Placement attempts per field star before it is given up.
const STAR_TRIES: u64 = 24;

/// Each field star's size, cycled by index: the family's named ratios.
const STAR_RATIOS: [f32; 4] = [STAR_ARM_STD, STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_FINE];

/// A field star's coverage at the peak of its twinkle (0..255).
const STAR_COV: f32 = 235.0;

/// Where a settled star holds on the twinkle's range: steady, a little under
/// its peak.
const STAR_STEADY_ENV: f32 = 0.8;

/// The share of the episode's tail over which the twinkle eases onto the
/// steady value.
const STAR_SETTLE_SPAN: f32 = 0.2;

/// The burst: this many stars thrown out from the kitty at an episode's start.
pub const BURST_STARS: usize = 8;

/// How long the burst flies (ms), from the ride's end ([`RIDE_MS`]).
const BURST_MS: u64 = 900;

/// THE RIDE (§10.6.1): for this long from an episode's start the kitty flies
/// in from the trail's start to its seat ([`ride`]), the trail laid behind
/// it; the burst opens where it lands. Its first tail flick falls in flight.
pub const RIDE_MS: u64 = 1200;

/// The sparkles the kitty sheds on its ride: one each time it has come
/// another of `SHED_SPARKLES + 1` equal parts of the way…
const SHED_SPARKLES: usize = 5;
/// …each fading out over this long (ms)…
const SHED_MS: u64 = 450;
/// …from this coverage.
const SHED_COV: f32 = 220.0;

/// How long a field star takes to come up once the kitty has passed it (ms).
const STAR_REVEAL_MS: f32 = 240.0;

const _: () = assert!(
    RIDE_MS + BURST_MS < EPISODE_MS && RIDE_MS + SHED_MS < EPISODE_MS,
    "the ride, its sparkles and its burst end inside the episode"
);

/// A burst star's coverage at launch.
const BURST_COV: f32 = 255.0;

/// An upper bound on one star's quads: two body bars, the nucleus and up to
/// [`STAR_TAPER_STEPS`] spans on each of four points, every one of them
/// possibly split at the row boundary (a star is shorter than a cell, so it
/// crosses at most one), plus four one-pixel glints. The headroom a frame at
/// shipping sizes keeps (`the_quad_budget_holds`); a star itself is laid
/// whole or taken back, to the quad ([`push_star`]).
#[cfg(test)]
const STAR_QUADS_MAX: usize = 2 * (3 + 4 * STAR_TAPER_STEPS as usize) + 4;

// ─── the line ───────────────────────────────────────────────────────────────

/// The line's palette drift over an episode (one trip, easing to a stop).
const LINE_DRIFT: f32 = 1.0;
/// The line's sweeps per episode, a bright band running left to right.
const LINE_SWEEPS: f32 = 2.0;
/// The sweep's gaussian width as a fraction of the line.
const LINE_SWEEP_SIGMA: f32 = 0.08;
/// How far the sweep pulls a letter toward white on a dark ground.
const LINE_SWEEP_GAIN: f32 = 0.55;
/// How far the sweep pulls a letter toward black on a light ground: a shade
/// deeper in its own hue. Lighter would take a letter under AA; at the dark
/// ground's 0.55 a vivid letter passes through a near-black brown, the mud
/// the paper law keeps out of the line.
const LINE_SWEEP_GAIN_LIGHT: f32 = 0.3;
/// A dark theme's line ink is lifted to at least this WCAG relative luminance
/// (about 5:1 on a near-black ground), so blue and violet letters read.
const LINE_DARK_LUMA_FLOOR: f32 = 0.30;
/// The light line's palette positions, `0..=1` in this many even steps: a
/// letter wears the ink of the nearest one ([`LineInks`]). Neighbouring
/// steps differ by at most 0.017 in OKLab (Cruise on Aterm Day and Gruvbox
/// Light; Land by 0.007), under the eye's least step of about 0.02, and a
/// letter sits at most half a step from its place — far finer than the
/// letters themselves, an 80-letter line's neighbours about 1/40 of the
/// palette apart. Each step's ink is the paper law's own answer there, so a
/// letter is still a pure hue at 4.5:1 or more.
const LINE_INK_STEPS: usize = 256;

/// The light line's inks for one palette on one ground ([`CardPalette::text_at`]
/// at each of [`LINE_INK_STEPS`] even steps), solved together on the first
/// light tick that meets them and kept on the card ([`HarnessCard`]'s
/// `inks`), so a light episode frame solves nothing: the law is a 24-step
/// bisection, and solving every letter afresh cost an 80-letter line about
/// 37 µs a frame through an episode (release), ten times the dark line's
/// lift. A reader asking for another palette or ground than the kept one
/// (a line laid before its card's first light tick) gets the law's own
/// answer at the same step, the same bytes ([`Self::at`]).
#[derive(Clone, Copy, Debug)]
struct LineInks {
    /// The palette and the ground these were solved for; `None` before a
    /// light tick.
    key: Option<(CardPalette, u32)>,
    /// The ink at each step.
    ink: [u32; LINE_INK_STEPS + 1],
}

impl Default for LineInks {
    fn default() -> Self {
        Self {
            key: None,
            ink: [0; LINE_INK_STEPS + 1],
        }
    }
}

impl LineInks {
    /// Solve every step for `palette` on `ground`, unless they are kept.
    fn solve(&mut self, palette: CardPalette, ground: u32) {
        if self.key == Some((palette, ground)) {
            return;
        }
        for (i, ink) in self.ink.iter_mut().enumerate() {
            *ink = palette.text_at(i as f32 / LINE_INK_STEPS as f32, ground);
        }
        self.key = Some((palette, ground));
    }

    /// The ink at palette position `at` (`0..=1`; clamps, and a NaN reads as
    /// `0`) on `ground`: the nearest step's.
    fn at(&self, palette: CardPalette, ground: u32, at: f32) -> u32 {
        // `as` saturates, and a NaN reads as step 0.
        let step =
            ((at.clamp(0.0, 1.0) * LINE_INK_STEPS as f32).round() as usize).min(LINE_INK_STEPS);
        if self.key == Some((palette, ground)) {
            self.ink[step]
        } else {
            #[cfg(any(test, debug_assertions))]
            LINE_INK_SOLVES.with(|n| n.set(n.get() + 1));
            palette.text_at(step as f32 / LINE_INK_STEPS as f32, ground)
        }
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Light letters solved on their own ([`LineInks::at`] asked for another
    /// palette or ground than the card keeps), counted for the host's tests.
    static LINE_INK_SOLVES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test seam: the light letters this thread solved on their own since the
/// last call ([`LineInks::at`]: a line inked against another palette or
/// ground than its card's last light tick), and the count reset. A host
/// that ticks its card on the pane's ground and inks the line against the
/// same ground solves none: a light frame's line is a lookup a letter.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn take_line_ink_solves() -> usize {
    LINE_INK_SOLVES.with(|n| n.replace(0))
}

// ─── the photosensitivity bound ─────────────────────────────────────────────

const _: () = {
    assert!(
        TWINKLE_FLASH_HZ <= TWINKLE_FLASH_BOUND_HZ,
        "the card's stars twinkle on the family law, under the bound"
    );
    // A wave cycle is two beats (up, then down).
    assert!(
        1000.0 / (2.0 * WAVE_STEP_MS as f32) <= TWINKLE_FLASH_BOUND_HZ,
        "the trail's step wave cycles under the photosensitivity bound"
    );
};

/// The sunset ramp for [`CardPalette::Land`]: red → orange → gold → rose.
const SUNSET: [u32; 4] = [0x00FF_3B30, 0x00FF_8A1F, 0x00FF_C83D, 0x00FF_6F91];

const WHITE: u32 = 0x00FF_FFFF;

/// Which colours the card wears: the Autopilot it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardPalette {
    /// Cruise: the full spectrum (the rainbow family's one arc).
    #[default]
    Cruise,
    /// Land: a sunset, red → orange → gold → rose.
    Land,
}

impl CardPalette {
    /// The palette at position `t` (`0..=1`; clamps).
    fn at(self, t: f32) -> u32 {
        match self {
            Self::Cruise => spectrum(t),
            Self::Land => sunset(t),
        }
    }

    /// The card art's colour at position `t` on a dark or a light ground:
    /// [`at`](Self::at) on dark, its print twin on light
    /// ([`PRINT_CRUISE_RAMP`], so a hue sits where it does on dark;
    /// [`PRINT_LAND`] evenly, each stripe its own ink).
    fn art_at(self, t: f32, dark: bool) -> u32 {
        match (self, dark) {
            (_, true) => self.at(t),
            (Self::Cruise, false) => knot_ramp(&PRINT_CRUISE_RAMP, t),
            (Self::Land, false) => even_ramp(&PRINT_LAND, t),
        }
    }

    /// Trail stripe `k` (0 top … 5 bottom) on a dark or a light ground.
    fn stripe(self, k: usize, dark: bool) -> u32 {
        let k = k % TRAIL_STOPS.len();
        match (self, dark) {
            (Self::Cruise, true) => spectrum_stop(TRAIL_STOPS[k]),
            (Self::Land, true) => sunset(k as f32 / (TRAIL_STOPS.len() - 1) as f32),
            (Self::Cruise, false) => PRINT_CRUISE[k],
            (Self::Land, false) => PRINT_LAND[k],
        }
    }

    /// A point mark's colour on a dark ground (a burst star, a sparkle): a
    /// named stop, never a between colour.
    fn mark(self, k: usize) -> u32 {
        match self {
            Self::Cruise => spectrum_stop(k % 7),
            Self::Land => SUNSET[k % SUNSET.len()],
        }
    }

    /// A point mark's print ink on a light ground's paper: the trail's
    /// stripe inks in turn, held to the border's luma bar ([`print_line`]),
    /// a star's arms being as thin as the border's line.
    fn star_ink(self, k: usize) -> u32 {
        print_line(self.stripe(k, false))
    }

    /// The line's letter at position `t` (`0..=1`; clamps) on the light
    /// `ground`: the print ramp's ink there as the paper law's point-mark
    /// ink ([`paper_mark_ink`]), text at 4.5:1 or more against `ground`.
    fn text_at(self, t: f32, ground: u32) -> u32 {
        paper_mark_ink(self.art_at(t, false), ground)
    }
}

/// The sunset ramp at `t` (`0..=1`; clamps, and a NaN reads as `0`).
fn sunset(t: f32) -> u32 {
    even_ramp(&SUNSET, t)
}

/// The ramp through evenly spaced `stops` at `t` (`0..=1`; clamps, and a NaN
/// reads as `0`).
fn even_ramp(stops: &[u32], t: f32) -> u32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = t * (stops.len() - 1) as f32;
    let i = (x as usize).min(stops.len() - 2);
    mix_rgb(stops[i], stops[i + 1], x - i as f32)
}

/// The ramp through `knots` (`(spectrum table index, colour)`, in order from
/// index `0` to the table's last) at `t` (`0..=1`; clamps, and a NaN reads as
/// `0`): the two knots about `t`'s table index, mixed straight.
fn knot_ramp(knots: &[(usize, u32)], t: f32) -> u32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = t * (SPECTRUM_LUT_LEN - 1) as f32;
    let i = (1..knots.len())
        .find(|&i| x <= knots[i].0 as f32)
        .unwrap_or(knots.len() - 1);
    let ((a, ca), (b, cb)) = (knots[i - 1], knots[i]);
    mix_rgb(ca, cb, (x - a as f32) / (b - a) as f32)
}

/// A light ground's one-pixel line in `rgb`: darkened toward black (hue and
/// saturation kept) until its Rec. 709 luma is at most
/// [`LIGHT_LINE_MAX_LUMA`]; a colour under the bar is unchanged.
fn print_line(rgb: u32) -> u32 {
    let ch = |sh: u32| ((rgb >> sh) & 0xff) as f32;
    let y = 0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0);
    if y <= LIGHT_LINE_MAX_LUMA {
        rgb
    } else {
        mix_rgb(rgb, 0, 1.0 - LIGHT_LINE_MAX_LUMA / y)
    }
}

/// Where the card is, in the target stream's pixel space. The host supplies
/// every number; the emitter derives the box and the bands from them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardScene {
    /// Pixel X of grid cell `(0, 0)`'s left edge.
    pub origin_x: i32,
    /// Pixel Y of grid cell `(0, 0)`'s top edge: grid row `r`'s band is
    /// `[origin_y + r·ch, origin_y + (r + 1)·ch)`.
    pub origin_y: i32,
    /// Cell width in pixels.
    pub cw: u16,
    /// Cell height in pixels.
    pub ch: u16,
    /// The stream's row index of grid row 0: a quad in grid row `r` is tagged
    /// `row_base + r` (the [`GlowQuad::row`] damage hint).
    pub row_base: u16,
    /// Pixels available left of column 0 (the pane's padding): the border's
    /// left side sits up to half a cell into it.
    pub pad_left: u16,
    /// Pixels of blank pane above the banner row, the box's AIR over it: the
    /// border's top edge stands up to half a cell into them, clear of the
    /// mascot's head and the banner's caps (the left side stands about as
    /// far from the mascot's nearest ink, its model row's `▝` in column 0's
    /// right half). The host passes the row's height where it read the pane
    /// row above blank across the box's columns, else `0`. Ignored at
    /// `top == 0`: the pane has no row above its first, and the window's
    /// padding and chrome there are not the card's.
    pub pad_above: u16,
    /// Pixels of blank pane under the last boxed row, the box's air under
    /// it: the border's bottom edge stands up to half a cell into them, clear
    /// of the line's descenders (or the working directory's). The host
    /// passes the row's height where it read the pane row under the box
    /// blank across the box's columns, else `0`.
    pub pad_below: u16,
    /// The first boxed grid row (the card's banner row).
    pub top: u16,
    /// How many rows the box holds: the card's rows plus the line's row when
    /// the line is drawn.
    pub rows: u16,
    /// The box's inner width in cells from column 0. The border's right side
    /// is DRAWN IN column `cols`, ending at that column's right edge
    /// (`origin_x + (cols + 1)·cw`), so the box spans `cols + 1` columns: a
    /// host that passes the pane's width in columns puts the right border
    /// one column past the pane, where [`clip`](Self::clip) cuts it away.
    /// Pass at most the pane's last column index.
    pub cols: u16,
    /// The pixels the card may touch, `(x0, y0, x1, y1)` half-open in the
    /// stream's pixel space: the pane's rect, so a box larger than the pane
    /// (a `cols` past its edge, padding that belongs to a neighbour) never
    /// draws over another pane or the chrome. Every quad is clipped to it
    /// (a horizontal ramp keeps its colours where it is cut) and one clipped
    /// to nothing is dropped; the kitty, art that reads wrong cut, is drawn
    /// whole inside it or not at all. An empty clip draws nothing.
    pub clip: (i32, i32, i32, i32),
    /// `[start_col, end_col)` of the rainbow trail: from the text's end to the
    /// kitty's back (`end_col` is normally `kitty_col`), as tall as the
    /// kitty's body — on the card's first two rows beside the two-row
    /// kitty, reaching into the third beside the tall one, whose host has
    /// read those cells blank. A trail that reaches the kitty runs on under
    /// its art, inside its columns, to the haunch's outline, so the rainbow
    /// meets the cat with no ground between them.
    pub trail: Option<(u16, u16)>,
    /// The kitty's left column on rows `top..top + kitty_rows`; it needs
    /// [`kitty_cols`] columns and is left out when they do not fit in the box.
    pub kitty_col: Option<u16>,
    /// How many of the card's rows the kitty stands on: `3` for the tall
    /// kitty ([`KITTY_TALL_H`] pixels, the height of Claude's mascot), where
    /// the host found the card's third row blank across the trail, the kitty
    /// and its margin; anything else is the two-row kitty ([`KITTY_H`]).
    /// The tall kitty needs a box of three rows or more, and is drawn
    /// two rows tall in a smaller one.
    pub kitty_rows: u8,
    /// The Autopilot's colours.
    pub palette: CardPalette,
    /// The theme's polarity: additive light on dark, source-over ink on light.
    pub dark: bool,
    /// The pane's ground, `0x00RRGGBB`: its blank cell's background, which
    /// the host reads `dark` from ([`dark_ground`]). On a light ground a tick
    /// solves the line's letters against it once and keeps them on the card
    /// ([`HarnessCard::line_ink`]); unread on a dark one.
    pub ground: u32,
    /// Serious mode: a static border only.
    pub serious: bool,
    /// The kitty's coat, `0x00RRGGBB` (the launch kitty's look).
    pub coat: u32,
    /// The kitty's eyes, `0x00RRGGBB`.
    pub eye: u32,
}

/// The border's stroke at cell height `ch`: a fourteenth of the cell, at
/// least one pixel.
fn stroke_px(ch: u16) -> u16 {
    (ch / 14).max(1)
}

/// The kitty's pixel size at cell height `ch`: the largest square pixel that
/// lays [`KITTY_HALF`] pixel rows inside one band LESS the border's stroke
/// ([`stroke_px`]). The kitty sits on the box's first row, right under the
/// top border as it stands with no air (the air never moves the kitty:
/// [`Geo::inner`]), its top half bottom-aligned in that row's band: sized
/// from the whole band (`ch / 7`) it would reach into the stroke whenever
/// `ch % 7` is under the stroke (14, 21, 28, 29, 35, 36, 42 to 44 px …).
/// `0` when the cell is too short to hold the art.
fn kitty_px(ch: u16) -> u16 {
    ch.saturating_sub(stroke_px(ch)) / KITTY_HALF as u16
}

/// How many cells wide the kitty is at cell size `cw × ch`: its
/// [`KITTY_W`] square pixels ([`kitty_px`], the band less the border's
/// stroke), rounded up to whole cells. `0` when the cells are too short for
/// the art (under [`KITTY_HALF`] px plus the stroke), or `cw` is `0`.
#[must_use]
pub fn kitty_cols(cw: u16, ch: u16) -> u16 {
    let p = u32::from(kitty_px(ch));
    if p == 0 || cw == 0 {
        return 0;
    }
    let w = KITTY_W as u32 * p;
    u16::try_from(w.div_ceil(u32::from(cw))).unwrap_or(u16::MAX)
}

/// The card's episode clock and seed. `Default` is an unarmed card: it draws
/// the settled frame and wants no frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct HarnessCard {
    /// Where the card's episode stands.
    episode: Episode,
    /// The episode's seed: where the stars sit and how they twinkle.
    seed: u64,
    /// The palette of the last tick, which [`Self::line_ink`] wears.
    palette: CardPalette,
    /// The light line's inks for the last light tick's palette and ground
    /// ([`LineInks`]), which [`Self::line_ink`] reads.
    inks: LineInks,
    /// The last tick's motion verdict was `animate == false`, the scene was
    /// serious, or it had nothing to draw: [`Self::deadline`] then wants no
    /// frames, whatever the episode's clock says. `false` before any tick
    /// and after an arm, so an armed card asks for the frame that will draw
    /// its episode.
    still: bool,
    /// The last tick drew an EPISODE frame (`Clock::t_ms` was `Some`): the
    /// settled frame is still to be drawn once the episode runs out, and
    /// [`Self::deadline`] asks for it — for [`SETTLE_GRACE_MS`] past the
    /// episode's end at most.
    drew_episode: bool,
    /// Where the kitty's ride stands ([`Ride`]): once, on the card's first
    /// showing.
    ride: Ride,
}

/// Where the kitty's ride stands (§10.6.5). It rides in ONCE, on the episode
/// that first puts the card on the glass. A card the glass has shown at rest
/// — its kitty seated, its trail whole: any settled frame drawn — keeps the
/// kitty in its seat: an episode armed again (the window regained focus, the
/// Autopilot changed) or an owed one played plays there, the burst at its
/// start, rather than snapping the kitty back to the trail's start and
/// taking the trail away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Ride {
    /// Nothing drawn yet: the first episode drawn animating rides.
    #[default]
    Ahead,
    /// Riding, on a clock started at this instant: an episode armed again
    /// while the kitty is still on its way carries the ride on from where it
    /// is (the kitty never steps back).
    Since(Instant),
    /// The glass has shown the kitty seated, or the ride had run out when an
    /// episode was armed again: no more rides for this card.
    Done,
}

/// One card's last SETTLED frame — its scene and its quads — kept by the
/// host beside the card so that a card at rest is replayed rather than drawn
/// again ([`HarnessCard::tick_or_replay`], [`HarnessCard::replay`]); its
/// fingerprint is the quads' ([`HarnessCard::tick`]), made only when asked.
/// `Default` holds nothing. Its buffers are reused, so a host that keeps one
/// per card allocates nothing for it once the first settled frame is in.
#[derive(Clone, Debug, Default)]
pub struct SettledFrame {
    /// The scene the frame was drawn for; `None` while it holds no frame.
    scene: Option<CardScene>,
    under: Vec<GlowQuad>,
    over: Vec<GlowQuad>,
}

impl SettledFrame {
    /// The settled frame's border and trail ([`HarnessCard::tick`]'s
    /// `under`); empty while it holds no frame.
    #[must_use]
    pub fn under(&self) -> &[GlowQuad] {
        &self.under
    }

    /// The settled frame's kitty and stars ([`HarnessCard::tick`]'s `over`);
    /// empty while it holds no frame.
    #[must_use]
    pub fn over(&self) -> &[GlowQuad] {
        &self.over
    }
}

/// Where the card's episode stands ([`HarnessCard`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Episode {
    /// No episode: unarmed, played out, or ended by serious mode.
    #[default]
    Idle,
    /// Armed at this instant and not yet drawn animating: a tick that may
    /// animate draws it on this clock.
    Armed(Instant),
    /// Armed at this instant, then ticked with `animate == false` before any
    /// of it was drawn (reduced motion, an unfocused window, the load-shed
    /// latch forcing Reduced just as the card was first found), or with
    /// nothing to draw (a pane mid-layout): OWED, not thrown away. The first
    /// tick that may animate and draw starts it over from its own `now` —
    /// one that ran out while held back is owed too — until
    /// [`OWED_MAX_MS`] after the arm, when the debt lapses to [`Self::Idle`].
    Owed(Instant),
    /// Drawn animating at least once, on the clock started at this instant.
    /// A tick with `animate == false` keeps the clock (and draws the settled
    /// frame); motion allowed again inside the episode resumes it there.
    Playing(Instant),
}

/// Where in an episode a frame falls: `Some(ms)` while it runs, `None` for
/// the settled frame; and how far into the kitty's ride ([`Ride`]), `None`
/// where this episode does not ride (or settled).
#[derive(Clone, Copy, Debug)]
struct Clock {
    t_ms: Option<u64>,
    ride_ms: Option<u64>,
}

impl Clock {
    const SETTLED: Self = Self {
        t_ms: None,
        ride_ms: None,
    };

    /// `t` ms into a card's first episode, which rides from its start.
    #[cfg(test)]
    const fn riding(t: u64) -> Self {
        Self {
            t_ms: Some(t),
            ride_ms: Some(t),
        }
    }

    /// `t` ms into an episode that does not ride: the kitty seated
    /// throughout, the burst at the start.
    #[cfg(test)]
    const fn seated(t: u64) -> Self {
        Self {
            t_ms: Some(t),
            ride_ms: None,
        }
    }

    /// The episode's progress, `0..1`; `1` when settled.
    fn u(self) -> f32 {
        self.t_ms
            .map_or(1.0, |t| (t as f32 / EPISODE_MS as f32).min(1.0))
    }

    /// The border's (and line's) drift phase offset: `−drift·(1 − u)²`, which
    /// starts at one trip per [`TRIP_MS`] and eases to zero at the end.
    fn drift(self, drift: f32) -> f32 {
        let v = 1.0 - self.u();
        -drift * v * v
    }

    /// The head's strength: ramps in over the first 4 % and out over the last
    /// 12 % of the episode; `0` when settled.
    fn head_env(self) -> f32 {
        if self.t_ms.is_none() {
            return 0.0;
        }
        let u = self.u();
        smooth(u / 0.04) * smooth((1.0 - u) / 0.12)
    }
}

/// `smoothstep` on `0..=1`, clamped.
fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A pixel rectangle `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Rect {
    fn meet(self, o: Self) -> Self {
        Self {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }

    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    fn contains(self, x: i64, y: i64) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
    }
}

/// The scene resolved to pixels (`i64`, so no product of `u16`s overflows).
#[derive(Clone, Copy, Debug)]
struct Geo {
    cw: i64,
    ch: i64,
    ox: i64,
    oy: i64,
    top: i64,
    rows: i64,
    row_base: i64,
    /// The boxed rows' rectangle: the border's outer rectangle as it stands
    /// with no air, from the banner row's band to the last boxed row's, its
    /// left side up to half a cell into the padding. The marks inside the
    /// border keep to its stroke ([`Self::inner`]).
    core: Rect,
    /// The border's outer rectangle, unclipped: [`Self::core`] with up to
    /// half a row of air over and under it ([`CardScene::pad_above`],
    /// [`CardScene::pad_below`]). Only the border is laid on it.
    outer: Rect,
    /// The border's thickness.
    t: i64,
    /// The outer rectangle clipped to the scene's [`clip`](CardScene::clip)
    /// and to the `u16` pixel space: no quad leaves it.
    clip: Rect,
}

impl Geo {
    fn new(s: &CardScene) -> Option<Self> {
        if s.cw == 0 || s.ch == 0 || s.rows == 0 || s.cols == 0 {
            return None;
        }
        let (cw, ch) = (i64::from(s.cw), i64::from(s.ch));
        let (ox, oy) = (i64::from(s.origin_x), i64::from(s.origin_y));
        let top = i64::from(s.top);
        let rows = i64::from(s.rows);
        let pad = i64::from(s.pad_left).min(cw / 2);
        let core = Rect {
            x0: ox - pad,
            y0: oy + top * ch,
            x1: ox + (i64::from(s.cols) + 1) * cw,
            y1: oy + (top + rows) * ch,
        };
        // Half a row of air at most: the edge (`t ≤ ch / 2` thick) lies in
        // the neighbour row's band, and the row past it is never reached.
        let air = |pad: u16| i64::from(pad).min(ch / 2);
        let outer = Rect {
            y0: core.y0 - if top > 0 { air(s.pad_above) } else { 0 },
            y1: core.y1 + air(s.pad_below),
            ..core
        };
        let space = Rect {
            x0: 0,
            y0: 0,
            x1: i64::from(u16::MAX),
            y1: i64::from(u16::MAX),
        };
        let (cx0, cy0, cx1, cy1) = s.clip;
        let pane = Rect {
            x0: i64::from(cx0),
            y0: i64::from(cy0),
            x1: i64::from(cx1),
            y1: i64::from(cy1),
        };
        let clip = outer.meet(space).meet(pane);
        if clip.is_empty() {
            return None;
        }
        Some(Self {
            cw,
            ch,
            ox,
            oy,
            top,
            rows,
            row_base: i64::from(s.row_base),
            core,
            outer,
            t: i64::from(stroke_px(s.ch)),
            clip,
        })
    }

    /// The top pixel of grid row `r`'s band.
    fn band(&self, r: i64) -> i64 {
        self.oy + r * self.ch
    }

    /// The left pixel of grid column `c`.
    fn col(&self, c: u16) -> i64 {
        self.ox + i64::from(c) * self.cw
    }

    /// The box inside the border's stroke as it stands with no air (inside
    /// [`Self::core`]), where the trail, the kitty, its rim and the stars
    /// are laid: the air moves the border and nothing else.
    fn inner(&self) -> Rect {
        Rect {
            x0: self.core.x0 + self.t,
            y0: self.core.y0 + self.t,
            x1: self.core.x1 - self.t,
            y1: self.core.y1 - self.t,
        }
    }
}

/// One end of a quad: its premultiplied colour and its opacity (`0` additive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Paint {
    color: u32,
    alpha: u8,
}

/// The one way a quad leaves this module: clipped, split at every row band,
/// tagged, and charged against the tick's budget.
struct Sink {
    geo: Geo,
    left: usize,
}

impl Sink {
    /// Push `r` (clipped to `clip` and the box) as a horizontal ramp from `l`
    /// at its left edge to `rt` at its right; split at every row band.
    /// Returns `false` where the budget could not take a quad `r` needs: a
    /// rectangle that spends the last of it is laid whole, and the next one
    /// that needs a quad is refused.
    fn push(&mut self, out: &mut Vec<GlowQuad>, clip: Rect, r: Rect, l: Paint, rt: Paint) -> bool {
        // Additive zero light adds nothing; a source-over quad still darkens.
        if l.alpha == 0 && l.color == 0 && rt.color == 0 {
            return true;
        }
        let c = r.meet(clip).meet(self.geo.clip);
        if c.is_empty() {
            return true;
        }
        // A horizontal clip moves a ramp's ends: read them off the ramp.
        let (l, rt) = if (c.x0, c.x1) == (r.x0, r.x1) {
            (l, rt)
        } else {
            let span = (r.x1 - r.x0) as f32;
            let at = |x: i64| ramp(l, rt, (x - r.x0) as f32 / span);
            (at(c.x0), at(c.x1))
        };
        let g = self.geo;
        let mut y = c.y0;
        while y < c.y1 {
            let band = (y - g.oy).div_euclid(g.ch);
            let end = g.band(band + 1).min(c.y1);
            if let Ok(row) = u16::try_from(g.row_base + band) {
                if self.left == 0 {
                    return false;
                }
                self.left -= 1;
                // In range: `c` lies inside the u16 pixel space.
                let q = GlowQuad::gradient(
                    row,
                    c.x0 as u16,
                    y as u16,
                    (c.x1 - c.x0) as u16,
                    (end - y) as u16,
                    l.color,
                    l.alpha,
                    rt.color,
                    rt.alpha,
                );
                out.push(q);
            }
            y = end;
        }
        true
    }

    /// [`Self::push`] for one flat colour.
    fn flat(&mut self, out: &mut Vec<GlowQuad>, clip: Rect, r: Rect, p: Paint) -> bool {
        self.push(out, clip, r, p, p)
    }
}

/// A ramp's value at fraction `f` between its ends (per channel, rounded).
/// The mode is per quad: two additive ends stay additive, two source-over ends
/// stay at least 1.
fn ramp(a: Paint, b: Paint, f: f32) -> Paint {
    let f = f.clamp(0.0, 1.0);
    let ch = |x: u32, y: u32| ((x as f32 + (y as f32 - x as f32) * f).round() as u32).min(255);
    let rgb = |x: u32, y: u32| {
        (ch((x >> 16) & 0xff, (y >> 16) & 0xff) << 16)
            | (ch((x >> 8) & 0xff, (y >> 8) & 0xff) << 8)
            | ch(x & 0xff, y & 0xff)
    };
    let alpha = if a.alpha == 0 {
        0
    } else {
        ch(u32::from(a.alpha), u32::from(b.alpha)).max(1) as u8
    };
    Paint {
        color: rgb(a.color, b.color),
        alpha,
    }
}

/// Light that adds `rgb` at coverage `cov` (dark theme).
fn light(rgb: u32, cov: f32) -> Paint {
    Paint {
        color: premul_rgb(rgb, cov.clamp(0.0, 255.0) as u8),
        alpha: 0,
    }
}

/// Ink laid source-over at opacity `alpha` (light theme), floored at 1 so the
/// quad stays source-over.
fn ink(rgb: u32, alpha: f32) -> Paint {
    let a = (alpha.clamp(1.0, 255.0)) as u8;
    Paint {
        color: premul_rgb(rgb, a),
        alpha: a,
    }
}

/// The kitty's resolved colours for one theme.
#[derive(Clone, Copy, Debug)]
struct KittyInk {
    outline: u32,
    coat: u32,
    /// The front paws' socks: the coat lifted toward white.
    socks: u32,
    /// The pupils and eye rings ([`PUPIL`]).
    pupil: u32,
    /// The iris.
    eye: u32,
    nose: u32,
    /// The eyes' catch-light.
    glint: u32,
}

impl KittyInk {
    fn new(coat: u32, eye: u32, dark: bool) -> Self {
        let coat = coat & 0x00FF_FFFF;
        // The outline has to part the cat from the ground: a deeper coat on
        // light, and on dark the same unless the coat is itself near black,
        // when a lighter rim draws the silhouette instead.
        let outline = if dark && relative_luminance(coat) < 0.06 {
            mix_rgb(coat, WHITE, 0.45)
        } else {
            mix_rgb(coat, 0, 0.65)
        };
        Self {
            outline,
            coat,
            socks: mix_rgb(coat, WHITE, 0.55),
            pupil: PUPIL,
            eye: eye & 0x00FF_FFFF,
            nose: NOSE_PINK,
            glint: WHITE,
        }
    }

    /// The colour of palette character `c`; `None` is transparent.
    fn of(self, c: u8) -> Option<u32> {
        match c {
            b'o' => Some(self.outline),
            b'c' => Some(self.coat),
            b'b' => Some(self.socks),
            b'k' => Some(self.pupil),
            b'e' => Some(self.eye),
            b'n' => Some(self.nose),
            b'w' => Some(self.glint),
            _ => None,
        }
    }
}

/// The frame of `art` shown `t_ms` into an episode (the open frame when
/// settled): a blink every [`BLINK_PERIOD_MS`], a tail flick between them.
fn kitty_frame(art: &KittyArt, clock: Clock) -> &'static [&'static str] {
    let Some(t) = clock.t_ms else {
        return art.open;
    };
    let in_window = |at: u64, period: u64, len: u64| t >= at && (t - at) % period < len;
    if in_window(BLINK_AT_MS, BLINK_PERIOD_MS, BLINK_MS) {
        art.blink
    } else if in_window(FLICK_AT_MS, FLICK_PERIOD_MS, FLICK_MS) {
        art.flick
    } else {
        art.open
    }
}

/// A deterministic fraction in `0..1` from `(seed, salt)`.
fn unit(seed: u64, salt: u64) -> f32 {
    (mix(seed ^ mix(salt)) >> 40) as f32 / (1u64 << 24) as f32
}

/// One field star: its centre, arm, twinkle phase and whether it wears
/// glints when settled.
#[derive(Clone, Copy, Debug, PartialEq)]
struct StarSpot {
    x: i64,
    y: i64,
    ratio: f32,
    phase: f32,
    gold: bool,
}

/// The region stars may occupy: the trail and kitty rows, from the trail's
/// start (or the kitty's, with no trail) to the border's inner right edge.
/// Never left of it, where the card's text is. On a light ground it stops
/// short of the border's column (`cols`), at the last column the host read
/// blank on both rows (`claude_card.rs`'s `layout`: `cols <= limit`, and
/// column `limit` may hold a glyph), so no opaque mark ever lies on a
/// letter; on dark, added light, it runs to the stroke.
fn star_region(g: &Geo, s: &CardScene) -> Option<Rect> {
    if g.rows < 2 {
        return None;
    }
    let start = match (s.trail, s.kitty_col) {
        (Some((t0, _)), Some(k)) => t0.min(k),
        (Some((t0, _)), None) => t0,
        (None, Some(k)) => k,
        (None, None) => return None,
    };
    let inner = g.inner();
    let r = Rect {
        x0: g.col(start),
        y0: g.band(g.top),
        x1: if s.dark {
            inner.x1
        } else {
            inner.x1.min(g.col(s.cols))
        },
        y1: g.band(g.top + 2),
    }
    .meet(inner)
    .meet(g.clip);
    (!r.is_empty()).then_some(r)
}

/// Where the kitty sits in a frame: its pixel rectangle, its square pixel
/// and its art (two rows or three).
#[derive(Clone, Copy, Debug)]
struct KittyAt {
    rect: Rect,
    p: i64,
    art: KittyArt,
}

impl KittyAt {
    /// The pixel rows of the art's body band, the trail's stripes' span.
    fn body(&self) -> (i64, i64) {
        let (first, rows) = self.art.body;
        let y0 = self.rect.y0 + first as i64 * self.p;
        (y0, y0 + rows as i64 * self.p)
    }

    /// Where the trail that reaches the kitty ends: the left edge of its
    /// art's [`rear`](KittyArt::rear) column, under the cat — the haunch's
    /// outline, where the rainbow meets it on every row of the body band.
    /// Ended at the art's left edge instead, the trail would stop short of
    /// the haunch by the art's transparent columns, a strip of ground with
    /// the rim's line in it between the rainbow and the cat.
    fn rear(&self) -> i64 {
        self.rect.x0 + self.art.rear() as i64 * self.p
    }
}

/// The rim's width at kitty pixel `p` under a border stroke of `t` pixels: the
/// stroke, at most half a kitty pixel — so a rim strip never reaches another
/// across a transparent pixel ([`Emit::rim`]) — and `0`, no rim, for art of
/// one-pixel pixels.
fn rim_px(p: i64, t: i64) -> i64 {
    t.min(p / 2)
}

/// Whether pixel `(r, c)` of `frame` is art (not transparent, and inside it).
fn art_at(frame: &[&str], r: i64, c: i64) -> bool {
    usize::try_from(r)
        .ok()
        .and_then(|r| frame.get(r))
        .zip(usize::try_from(c).ok())
        .and_then(|(row, c)| row.as_bytes().get(c))
        .is_some_and(|&b| b != b'.')
}

/// Whether pixel `(r, c)` of `frame` is inside the silhouette the rim rings:
/// art, or a one-pixel notch between art on its left and its right (the gaps
/// between the paws). The rim lines neither side of a notch, and runs under
/// art alone, not under the notch ([`Emit::rim`]): a toe's gap stays the
/// ground and opens onto it, so the paws read as paws on either ground
/// rather than as holes boxed in by a line beneath them.
fn rim_solid(frame: &[&str], r: i64, c: i64) -> bool {
    art_at(frame, r, c) || (art_at(frame, r, c - 1) && art_at(frame, r, c + 1))
}

/// Which art a scene's kitty wears: the tall kitty where the host asked for
/// three rows and the box holds them, else the two-row kitty.
fn kitty_art(g: &Geo, s: &CardScene) -> KittyArt {
    if s.kitty_rows >= 3 && g.rows >= 3 {
        KITTY_TALL
    } else {
        KITTY_SHORT
    }
}

/// Where the kitty's art sits at pixel size `p` with its left edge at `x0`,
/// whether or not it fits: its first [`KITTY_HALF`] pixel rows bottom-aligned
/// in the first row's band, the rest from the second's top down — so no pixel
/// row straddles that boundary, and the tall kitty's head sits where the
/// two-row kitty's does (a card that gains or loses its third row's room
/// stands the cat up or down; its head does not move).
fn kitty_place(g: &Geo, art: KittyArt, p: i64, x0: i64) -> KittyAt {
    let y0 = g.band(g.top + 1) - KITTY_HALF as i64 * p;
    KittyAt {
        rect: Rect {
            x0,
            y0,
            x1: x0 + KITTY_W as i64 * p,
            y1: y0 + art.h() as i64 * p,
        },
        p,
        art,
    }
}

/// The kitty's seat when it is drawn: [`KITTY_W`] square pixels wide at its
/// column, two or three rows tall ([`kitty_art`], [`kitty_place`]), wholly
/// inside the border's stroke ([`Geo::inner`]) and wholly inside the clip.
/// Art cut by the scene's [`clip`](CardScene::clip) or by the box would read
/// as a broken cat, so the kitty is drawn whole or not at all; the border,
/// trail and stars stay either way.
fn kitty_rect(g: &Geo, s: &CardScene) -> Option<KittyAt> {
    let col = s.kitty_col?;
    let p = i64::from(kitty_px(s.ch));
    if g.rows < 2 || p == 0 {
        return None;
    }
    let at = kitty_place(g, kitty_art(g, s), p, g.col(col));
    (at.rect.meet(g.inner()).meet(g.clip) == at.rect).then_some(at)
}

/// How far along its ride the kitty is at `clock`: `1 − (1 − u)³` of the way
/// at `u` of [`RIDE_MS`] — quick off the line, settling into its seat — and
/// `1`, seated, from [`RIDE_MS`] on, when settled, and all through an
/// episode that does not ride ([`Ride`]).
fn ride(clock: Clock) -> f32 {
    match clock.ride_ms {
        Some(t) if t < RIDE_MS => {
            let v = 1.0 - t as f32 / RIDE_MS as f32;
            1.0 - v * v * v
        }
        _ => 1.0,
    }
}

/// When, in ms into an episode, the ride has come `f` of the way: [`ride`]
/// inverted.
fn ride_at(f: f32) -> f32 {
    RIDE_MS as f32 * (1.0 - (1.0 - f.clamp(0.0, 1.0)).cbrt())
}

/// Where the ride starts and how far it runs, in pixels: from the trail's
/// first pixel inside the box and the clip to the seat's left edge. `None`
/// with no trail, or no way to go.
fn ride_span(g: &Geo, s: &CardScene, seat: &KittyAt) -> Option<(i64, i64)> {
    let (c0, _) = s.trail?;
    let from = g.col(c0).max(g.inner().x0).max(g.clip.x0);
    let dist = seat.rect.x0 - from;
    (dist > 0).then_some((from, dist))
}

/// The kitty at `clock`, seated at `seat`: on its ride ([`ride`]) the
/// seat's rows and art that far from the ride's start ([`ride_span`]) —
/// every pixel of the way inside the box and the clip, which hold the trail
/// and the seat — and the seat itself from [`RIDE_MS`] on.
fn ridden(g: &Geo, s: &CardScene, seat: KittyAt, clock: Clock) -> KittyAt {
    let f = ride(clock);
    match ride_span(g, s, &seat) {
        Some((from, dist)) if f < 1.0 => {
            kitty_place(g, seat.art, seat.p, from + (dist as f32 * f).round() as i64)
        }
        _ => seat,
    }
}

/// The trail's span, top to bottom: the body band of the kitty the scene
/// seats ([`KittyAt::body`]) — where it is drawn, and where it would sit when
/// it is not (left out of a box too small, or cells too short for the art,
/// at a pixel of one) — kept inside the rows the kitty stands on.
fn trail_band(g: &Geo, s: &CardScene) -> (i64, i64) {
    let art = kitty_art(g, s);
    let p = i64::from(kitty_px(s.ch)).max(1);
    let (y0, y1) = kitty_place(g, art, p, 0).body();
    let (lo, hi) = (g.band(g.top), g.band(g.top + art.rows() as i64));
    (y0.clamp(lo, hi), y1.clamp(lo, hi))
}

/// The whole trail at rest, `[tx0, tx1) × [ya, yb)` (from its start to the
/// kitty's rear under the cat where it reaches the seat, inside the stroke,
/// as tall as the kitty's body), and the
/// step wave's amplitude, by which an episode moves its top and bottom
/// edges; `None` where no trail is drawn (no trail, no room, a band too short
/// for its stripes). `seat` is the kitty's ([`kitty_rect`]).
fn trail_rect(g: &Geo, s: &CardScene, seat: Option<KittyAt>) -> Option<(Rect, i64)> {
    let (c0, c1) = s.trail?;
    if g.rows < 2 {
        return None;
    }
    let inner = g.inner();
    let (tx0, mut tx1) = (g.col(c0).max(inner.x0), g.col(c1).min(inner.x1));
    // A trail that reaches the kitty's seat runs on under the cat to its
    // rear ([`KittyAt::rear`]), its stripes hidden under the art's opaque
    // pixels, so it meets the haunch flush rather than a strip of ground
    // short of it.
    if let Some(seat) = seat.filter(|seat| tx1 >= seat.rect.x0) {
        tx1 = tx1.max(seat.rear());
    }
    if tx1 <= tx0 {
        return None;
    }
    let stripes = TRAIL_STOPS.len() as i64;
    let (ya, yb) = trail_band(g, s);
    let h = yb - ya;
    if h < stripes {
        return None;
    }
    let a = (g.ch / 16).max(1);
    let amp = if h / stripes > a { a } else { 0 };
    let r = Rect {
        x0: tx0,
        y0: ya,
        x1: tx1,
        y1: yb,
    };
    Some((r, amp))
}

/// Whether a light star whose reach is the box `b` lies across the trail's
/// edge: in reach of its ink as the wave moves it (`t` grown by `amp`) yet
/// not wholly inside the band it covers in every segment (`t` shrunk by
/// `amp`). Such a star would wear two tones, the paper's white on the print
/// and ink off it ([`lay_toned`]), and with the wave its edge between them
/// would step through the star; a steady star keeps one tone, so the field
/// is placed off it.
fn across_trail(b: Rect, (t, amp): (Rect, i64)) -> bool {
    let outer = Rect {
        y0: t.y0 - amp,
        y1: t.y1 + amp,
        ..t
    };
    let inside = b.x0 >= t.x0 && b.x1 <= t.x1 && b.y0 >= t.y0 + amp && b.y1 <= t.y1 - amp;
    !b.meet(outer).is_empty() && !inside
}

/// The field's spots for this seed and geometry: up to [`STARS_MAX`], one per
/// slice of the region's width where there is room, clear of the kitty and of
/// each other, and on a light ground clear of the trail's edge
/// ([`across_trail`]). A pure function of its inputs, recomputed each tick.
fn field_spots(
    g: &Geo,
    s: &CardScene,
    seed: u64,
    light_theme: bool,
) -> ([StarSpot; STARS_MAX], usize) {
    let mut spots = [StarSpot {
        x: 0,
        y: 0,
        ratio: 0.0,
        phase: 0.0,
        gold: false,
    }; STARS_MAX];
    let Some(region) = star_region(g, s) else {
        return (spots, 0);
    };
    let seat = kitty_rect(g, s);
    let kitty = seat.map(|at| at.rect);
    let trail = if light_theme {
        trail_rect(g, s, seat)
    } else {
        None
    };
    let want = (STARS_MIN + mix(seed ^ 0x5354_4152) % STARS_SPAN) as usize;
    let gap = g.cw.max(g.ch / 2);
    let mut n = 0;
    for i in 0..want {
        let ratio =
            STAR_RATIOS[i % STAR_RATIOS.len()] * if light_theme { STAR_ARM_INK } else { 1.0 };
        let arm = i64::from(star_arm_px(star_arm(g.ch as f32, ratio)));
        let m = arm + 1;
        let room = Rect {
            x0: region.x0 + m,
            y0: region.y0 + m,
            x1: region.x1 - m,
            y1: region.y1 - m,
        };
        if room.is_empty() {
            continue;
        }
        // Stratified: star `i` first tries its own slice of the region's
        // width, so the field spreads along the trail, round the kitty and
        // past it; then anywhere, if its slice is all kitty or crowded.
        let span = room.x1 - room.x0;
        let slice = Rect {
            x0: room.x0 + span * i as i64 / want as i64,
            x1: room.x0 + span * (i as i64 + 1) / want as i64,
            ..room
        };
        for k in 0..STAR_TRIES {
            let salt = ((i as u64) << 8) | k;
            let within = if k < STAR_TRIES / 2 { slice } else { room };
            let x = within.x0 + (unit(seed, salt * 2) * (within.x1 - within.x0) as f32) as i64;
            let y = room.y0 + (unit(seed, salt * 2 + 1) * (room.y1 - room.y0) as f32) as i64;
            let on_kitty = kitty.is_some_and(|r| {
                Rect {
                    x0: r.x0 - m,
                    y0: r.y0 - m,
                    x1: r.x1 + m,
                    y1: r.y1 + m,
                }
                .contains(x, y)
            });
            let crowded = spots[..n]
                .iter()
                .any(|o| (o.x - x).abs() < gap && (o.y - y).abs() < gap);
            let reach = Rect {
                x0: x - m,
                y0: y - m,
                x1: x + m + 1,
                y1: y + m + 1,
            };
            let across = trail.is_some_and(|t| across_trail(reach, t));
            if !on_kitty && !crowded && !across {
                spots[n] = StarSpot {
                    x,
                    y,
                    ratio,
                    phase: unit(seed, 0x5048_0000 | i as u64) * std::f32::consts::PI,
                    gold: i % 3 == 0,
                };
                n += 1;
                break;
            }
        }
    }
    (spots, n)
}

/// One four-point star ([`lay_star`]), WHOLE OR NOT AT ALL, to the quad: a
/// star the budget cannot finish is taken back — its quads and its spend —
/// and `false` returned, so a frame near the cap loses a star only when the
/// star truly does not fit, and never shows part of one.
#[allow(
    clippy::too_many_arguments,
    reason = "output + sink + clip + centre + arm + coverage + glint + colour + the \
              trail's ink + theme: the push_twinkle_star call with its clip, tones and theme"
)]
fn push_star(
    out: &mut Vec<GlowQuad>,
    sink: &mut Sink,
    clip: Rect,
    centre: (i64, i64),
    arm: i64,
    cov: f32,
    gold: bool,
    color: u32,
    on_trail: &[Rect],
    dark: bool,
) -> bool {
    let (n, left) = (out.len(), sink.left);
    if lay_star(
        out, sink, clip, centre, arm, cov, gold, color, on_trail, dark,
    ) {
        return true;
    }
    out.truncate(n);
    sink.left = left;
    false
}

/// Lay the flat piece `r`, clipped to `clip`, in `paint`, but where it lies
/// on `on_trail` (a light frame's trail ink, segment by segment:
/// [`Emit::trail_ink`]) in `glint`: so a light mark that crosses the trail's
/// edge is whole, the paper's white on the print and its ink on the paper,
/// never cut where print meets paper (§10.6.10). On the trail one quad a run
/// of segments that share the piece's rows, off it the piece less every
/// segment ([`lay_cut`]): a piece clear of the trail is laid as one quad, as
/// with no trail at all. `on_trail` runs left to right, each segment
/// starting where the last ends, so only the segments under the piece's
/// columns are visited. `false` once the budget is spent.
fn lay_toned(
    out: &mut Vec<GlowQuad>,
    sink: &mut Sink,
    clip: Rect,
    r: Rect,
    (paint, glint): (Paint, Paint),
    on_trail: &[Rect],
) -> bool {
    if on_trail.is_empty() {
        return sink.flat(out, clip, r, paint);
    }
    let q = r.meet(clip);
    if q.is_empty() {
        return true;
    }
    let lo = on_trail.partition_point(|c| c.x1 <= q.x0);
    let span = on_trail[lo..].partition_point(|c| c.x0 < q.x1);
    let on_trail = &on_trail[lo..lo + span];
    let Some(last) = on_trail.len().checked_sub(1) else {
        return sink.flat(out, clip, q, paint);
    };
    let mut run: Option<Rect> = None;
    for c in on_trail.iter().map(|c| c.meet(q)).filter(|c| !c.is_empty()) {
        run = match run {
            Some(p) if (p.y0, p.y1, p.x1) == (c.y0, c.y1, c.x0) => Some(Rect { x1: c.x1, ..p }),
            Some(p) => {
                if !sink.flat(out, clip, p, glint) {
                    return false;
                }
                Some(c)
            }
            None => Some(c),
        };
    }
    if let Some(p) = run
        && !sink.flat(out, clip, p, glint)
    {
        return false;
    }
    lay_cut(q, 0, last as i64, &|k| on_trail[k as usize], &mut |piece| {
        sink.flat(out, clip, piece, paint)
    })
}

/// One four-point star: the crate's one silhouette (`effect_util`'s
/// `push_twinkle_star`: a body through the crossing at the shared waist, the
/// nucleus, tapered points, glints when `gold`), clipped to `clip`. Additive
/// light on dark; on light `color` is the print ink it is laid in, every
/// piece source-over, opaque from a settled star's coverage up
/// ([`STAR_PRINT_FULL`]) and the nucleus with the body, and the paper's white
/// where a piece lies on the trail's ink (`on_trail`, [`lay_toned`]; empty
/// on dark). Returns `false` once the budget is spent, with the star in part
/// ([`push_star`] takes it back).
#[allow(
    clippy::too_many_arguments,
    reason = "output + sink + clip + centre + arm + coverage + glint + colour + the \
              trail's ink + theme: the push_twinkle_star call with its clip, tones and theme"
)]
fn lay_star(
    out: &mut Vec<GlowQuad>,
    sink: &mut Sink,
    clip: Rect,
    (sx, sy): (i64, i64),
    arm: i64,
    cov: f32,
    gold: bool,
    color: u32,
    on_trail: &[Rect],
    dark: bool,
) -> bool {
    if cov < 1.0 || arm < 1 {
        return true;
    }
    // One piece of the star at coverage `c` (0..255): in its colour, and in
    // the paper's white where it lies on the trail's ink.
    let tone = |rgb: u32, c: f32| {
        if dark {
            light(rgb, c)
        } else {
            ink(rgb, c * 255.0 / STAR_PRINT_FULL)
        }
    };
    let paint = |c: f32| (tone(color, c), tone(WHITE, c));
    let rect = |x: i64, y: i64, w: i64, h: i64| Rect {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    };
    let mut lay =
        |sink: &mut Sink, r: Rect, p: (Paint, Paint)| lay_toned(out, sink, clip, r, p, on_trail);
    let arm32 = i32::try_from(arm).unwrap_or(i32::MAX);
    let t = i64::from(star_waist_px(arm32));
    let half = (t - 1) / 2;
    let b = i64::from(star_body_px(arm32));
    let full = paint(cov.floor());
    if !lay(sink, rect(sx - b, sy - half, 2 * b + 1, t), full) {
        return false;
    }
    if !lay(sink, rect(sx - half, sy - b, t, 2 * b + 1), full) {
        return false;
    }
    let c = i64::from(star_core_px(arm32));
    let cc = (c - 1) / 2;
    // Additive, the crossing supplies part of the nucleus; ink lays it whole.
    let core = if dark {
        paint((cov * STAR_CORE_ADD).floor())
    } else {
        full
    };
    if !lay(sink, rect(sx - cc, sy - cc, c, c), core) {
        return false;
    }
    let run = arm - b;
    let steps = run.clamp(0, i64::from(STAR_TAPER_STEPS));
    for k in 0..steps {
        let d0 = b + run * k / steps;
        let d1 = b + run * (k + 1) / steps;
        let taper = star_taper_cov(((d0 + d1) as f32 * 0.5 - b as f32) / run as f32);
        let tip_cov = (cov * taper).floor();
        if dark && premul_rgb(color, tip_cov as u8) == 0 {
            break;
        }
        let tip = paint(tip_cov);
        let tt = ((t as f32 * taper).round() as i64).max(1);
        let th = (tt - 1) / 2;
        let len = d1 - d0;
        for off in [d0 + 1, -d1] {
            if !lay(sink, rect(sx + off, sy - th, len, tt), tip) {
                return false;
            }
            if !lay(sink, rect(sx - th, sy + off, tt, len), tip) {
                return false;
            }
        }
    }
    if gold {
        let d = ((arm as f32 * STAR_GLINT).round() as i64).max(1);
        // Dim light on dark. On paper the star's own ink at the body's
        // strength: a third of it let the paper through, a pale halo.
        let dim = if dark {
            paint((cov * STAR_GLINT_COV).floor())
        } else {
            full
        };
        for (gx, gy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            if !lay(sink, rect(sx + gx, sy + gy, 1, 1), dim) {
                return false;
            }
        }
    }
    true
}

impl HarnessCard {
    /// Start an episode at `now` (the card first found for a process, the
    /// Autopilot changed, the window regained focus). `seed` places and phases
    /// the stars; the host passes something stable per card (its process), so
    /// a re-arm keeps the same constellation.
    ///
    /// Arming forgets the last tick's motion verdict, so [`Self::deadline`]
    /// asks for the frame whose tick learns the new one: a card armed under
    /// reduced motion costs that one frame, and one armed as motion came back
    /// (focus regained) is never left waiting on an unrelated wake.
    pub fn arm(&mut self, now: Instant, seed: u64) {
        self.episode = Episode::Armed(now);
        self.seed = seed;
        self.still = false;
        // A ride run out stays out: the new episode bursts at its start.
        if let Ride::Since(from) = self.ride
            && now.saturating_duration_since(from) >= Duration::from_millis(RIDE_MS)
        {
            self.ride = Ride::Done;
        }
    }

    /// Whether [`Self::deadline`] is `None` at EVERY instant — known without
    /// a clock: the last tick held the card still (`animate == false`, a
    /// serious scene, nothing to draw), or no episode's clock is armed or
    /// playing (none armed, one owed, one played out and let go). A host
    /// asks this first and reads its clock only for a card that is not at
    /// rest, so a settled card costs its host's repaint key and wake judge
    /// no clock read.
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.still || !matches!(self.episode, Episode::Armed(_) | Episode::Playing(_))
    }

    /// Whether the card holds STILL at its settled frame: no episode's clock
    /// is armed or playing (none armed, one owed, one played out). Its
    /// [`Self::line_ink`] is then the settled gradient at every instant.
    #[must_use]
    pub fn is_still(&self) -> bool {
        !matches!(self.episode, Episode::Armed(_) | Episode::Playing(_))
    }

    /// Whether a tick on `scene` with motion verdict `animate` draws the
    /// SETTLED frame and leaves the episode where it is, at any instant: the
    /// card has no episode, or owes or plays one that this tick holds back
    /// (it may not animate — the window lost focus, Reduce Motion, the load
    /// latch — or has nothing to draw, and the scene is not serious, which
    /// would end it). Such a tick draws a function of the scene and
    /// the seed alone (`the_card_is_deterministic`), so its host may replay
    /// the last one ([`Self::tick_or_replay`]). An owed episode whose debt
    /// lapsed meanwhile ([`OWED_MAX_MS`]) is let go by the next tick that
    /// draws, which starts nothing either way.
    fn holds(&self, scene: &CardScene, animate: bool) -> bool {
        match self.episode {
            Episode::Idle => true,
            Episode::Owed(_) | Episode::Playing(_) => {
                !scene.serious && !(animate && Geo::new(scene).is_some())
            }
            Episode::Armed(_) => false,
        }
    }

    /// [`Self::tick`], or — where the tick would draw the same settled frame
    /// as the last one `kept` holds for this very `scene` ([`Self::holds`]) —
    /// that frame replayed from `kept`: the same quads appended, the same
    /// fingerprint returned and the card left as the tick would leave it,
    /// with no frame computed and NO CLOCK READ (`now` is called only to
    /// tick). The settled frame of a card at rest is then a copy, frame
    /// after frame: what a host's present costs while the card sits still.
    ///
    /// `kept` serves ONE card: every way into a state that replays (no
    /// episode, or one owed) is a tick — an arm leaves both — and every tick
    /// that draws the settled frame refills `kept`, so what it holds is this
    /// card's settled frame at its current seed whenever it is replayed. An
    /// episode frame empties it (its buffers kept).
    ///
    /// Returns the frame's fingerprint and the instant it was ticked at
    /// (`None`: replayed).
    pub fn tick_or_replay(
        &mut self,
        now: impl FnOnce() -> Instant,
        scene: &CardScene,
        animate: bool,
        kept: &mut SettledFrame,
        under: &mut Vec<GlowQuad>,
        over: &mut Vec<GlowQuad>,
    ) -> (u64, Option<Instant>) {
        if self.replay(scene, animate, kept) {
            under.extend_from_slice(&kept.under);
            over.extend_from_slice(&kept.over);
            return (frame_fp(&kept.under, &kept.over, Clock::SETTLED), None);
        }
        let now = now();
        let (u0, o0) = (under.len(), over.len());
        let fp = self
            .draw_keeping(now, scene, animate, kept, under, over)
            .map_or(0, |clock| frame_fp(&under[u0..], &over[o0..], clock));
        (fp, Some(now))
    }

    /// [`Self::tick_or_replay`]'s tick, with no fingerprint: the frame
    /// appended to `under` and `over` ([`Self::tick`]'s), and `kept`
    /// refilled with it when it is the settled frame (emptied by an episode
    /// frame). What a host that never reads the fingerprint calls where
    /// [`Self::replay`] said no.
    pub fn draw_kept(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        kept: &mut SettledFrame,
        under: &mut Vec<GlowQuad>,
        over: &mut Vec<GlowQuad>,
    ) {
        let _ = self.draw_keeping(now, scene, animate, kept, under, over);
    }

    /// [`Self::draw_kept`], and the frame's clock — `None` when the scene had
    /// nothing to draw.
    fn draw_keeping(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        kept: &mut SettledFrame,
        under: &mut Vec<GlowQuad>,
        over: &mut Vec<GlowQuad>,
    ) -> Option<Clock> {
        let (u0, o0) = (under.len(), over.len());
        let clock = self.draw(now, scene, animate, under, over);
        kept.under.clear();
        kept.over.clear();
        if self.drew_episode {
            kept.scene = None;
        } else {
            kept.scene = Some(*scene);
            kept.under.extend_from_slice(&under[u0..]);
            kept.over.extend_from_slice(&over[o0..]);
        }
        clock
    }

    /// [`Self::tick_or_replay`]'s replay, with the quads left where they
    /// are: where a tick would draw the settled frame `kept` holds for this
    /// very `scene`, the card is left as that tick would leave it and `true`
    /// is returned — the frame's quads are `kept`'s ([`SettledFrame::under`],
    /// [`SettledFrame::over`]), for a host that puts them on the glass
    /// itself; `false`, the card untouched, where a tick must draw
    /// ([`Self::draw_kept`]). No clock is read.
    pub fn replay(&mut self, scene: &CardScene, animate: bool, kept: &SettledFrame) -> bool {
        if kept.scene.as_ref() != Some(scene) || !self.holds(scene, animate) {
            return false;
        }
        // The state change of the tick it replays: `tick`'s own, with
        // `advance` leaving the episode where it is.
        self.palette = scene.palette;
        self.still = !(animate && Geo::new(scene).is_some()) || scene.serious;
        self.drew_episode = false;
        true
    }

    /// How far into a running episode `now` falls, in ms; `None` when no
    /// episode's clock is running at `now` (none armed, one owed, one played
    /// out).
    fn running(&self, now: Instant) -> Option<u64> {
        let (Episode::Armed(start) | Episode::Playing(start)) = self.episode else {
            return None;
        };
        let ms =
            u64::try_from(now.saturating_duration_since(start).as_millis()).unwrap_or(u64::MAX);
        (ms < EPISODE_MS).then_some(ms)
    }

    /// The next wake: one [`FRAME_MS`] ahead while an episode animates;
    /// `now` itself when the last tick drew an episode frame and the episode
    /// has run out since — so the settled frame is drawn promptly rather
    /// than on the host's next unrelated wake — but only until
    /// [`SETTLE_GRACE_MS`] past the playing episode's end: the tick that
    /// draws the settled frame ends the asking, and a host that stopped
    /// ticking yet asks each loop is let idle after the grace instead of
    /// spinning; `None` otherwise (settled, unarmed, an episode owed, after
    /// a tick with `animate == false`, a serious scene or nothing to draw),
    /// so a settled card idles at zero.
    ///
    /// THE HOST'S CONTRACT: ask this only of a card the host TICKED THIS
    /// FRAME. The answer describes the card as its last tick left it, and
    /// only the host knows whether that tick still stands on the glass: a
    /// card the host did not tick — a frame scrolled back, the pane hidden,
    /// the card shown bare (Off, the master switch, `card = false`) or its
    /// version not the process's — registers no deadline, else the last
    /// episode's frames would be asked for after the card left the screen.
    /// [`SETTLE_GRACE_MS`] is the backstop for a host that breaks this, not
    /// the rule.
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        if self.at_rest() {
            return None;
        }
        if self.running(now).is_some() {
            return now.checked_add(Duration::from_millis(FRAME_MS));
        }
        let Episode::Playing(start) = self.episode else {
            return None;
        };
        let ask_until = start.checked_add(Duration::from_millis(EPISODE_MS + SETTLE_GRACE_MS));
        (self.drew_episode && ask_until.is_some_and(|end| now < end)).then_some(now)
    }

    /// Append this frame's quads: the border and the trail to `under` (drawn
    /// beneath glyph ink), the kitty and the stars to `over`. `animate` is the
    /// motion policy's verdict ([`crate::motion::MotionEffect::HarnessCard`]).
    ///
    /// `false` gives the settled frame and holds the episode: one not yet
    /// drawn animating is OWED ([`Episode::Owed`]) and plays from the first
    /// tick with `animate == true`, from that tick's `now` (within
    /// [`OWED_MAX_MS`] of the arm); one already playing keeps its clock.
    /// A scene with nothing to draw (an empty or inverted clip, no rows,
    /// columns or cell size: a pane mid-layout) is held back the same way,
    /// whatever `animate` says, so an episode is never spent on frames
    /// nobody saw. `scene.serious` gives the settled frame and ends any
    /// episode.
    ///
    /// Returns the frame's fingerprint: `0` when nothing was drawn; otherwise
    /// a hash of every quad appended, folded with the frame index while an
    /// episode animates, so it changes every frame then and is constant once
    /// the card has settled (for the same scene and seed).
    pub fn tick(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        under: &mut Vec<GlowQuad>,
        over: &mut Vec<GlowQuad>,
    ) -> u64 {
        let (u0, o0) = (under.len(), over.len());
        self.draw(now, scene, animate, under, over)
            .map_or(0, |clock| frame_fp(&under[u0..], &over[o0..], clock))
    }

    /// [`Self::tick`] without its fingerprint: the frame appended, the card
    /// stepped, and the frame's clock returned — `None` when the scene had
    /// nothing to draw.
    fn draw(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        under: &mut Vec<GlowQuad>,
        over: &mut Vec<GlowQuad>,
    ) -> Option<Clock> {
        self.palette = scene.palette;
        if !scene.dark {
            self.inks.solve(scene.palette, scene.ground & 0x00FF_FFFF);
        }
        // The geometry first: a tick with nothing to draw holds the episode
        // back rather than starting its clock.
        let geo = Geo::new(scene);
        let animate = animate && geo.is_some();
        self.still = !animate || scene.serious;
        let clock = self.advance(now, animate, scene.serious);
        self.drew_episode = clock.t_ms.is_some();
        let geo = geo?;
        if clock.t_ms.is_none() {
            // The settled frame goes on the glass: the kitty is seated there,
            // and stays seated through every episode after ([`Ride`]).
            self.ride = Ride::Done;
        }
        let mut sink = Sink {
            geo,
            left: MAX_CARD_QUADS,
        };
        let cx = Emit::new(scene, geo, clock, self.seed);
        // The spend order: each mark runs only while the budget lasts. The
        // burst goes before the field: where a frame wants more than the cap
        // (the burst opening on a long trail at a large cell, three rows
        // tall), a field star — which twinkles anyway — waits a frame or two,
        // never a star of the burst's ring.
        if cx.border(&mut sink, under)
            && !scene.serious
            && cx.trail(&mut sink, under)
            && cx.kitty(&mut sink, over)
            && cx.rim(&mut sink, over)
            && cx.burst(&mut sink, over)
            && cx.stars(&mut sink, over)
        {
            cx.sparkles(&mut sink, over);
        }
        Some(clock)
    }

    /// Step the episode for a tick at `now` and say where in it the frame
    /// falls ([`Episode`]'s transitions): serious mode ends it; a debt
    /// older than [`OWED_MAX_MS`] lapses; a held-back tick owes an episode
    /// not yet drawn and keeps a playing one's clock; a tick that may
    /// animate starts an owed one from `now`, draws a running one, and lets
    /// one that ran out go.
    fn advance(&mut self, now: Instant, animate: bool, serious: bool) -> Clock {
        if serious {
            self.episode = Episode::Idle;
            return Clock::SETTLED;
        }
        if let Episode::Owed(armed) = self.episode
            && now.saturating_duration_since(armed) >= Duration::from_millis(OWED_MAX_MS)
        {
            self.episode = Episode::Idle;
        }
        if !animate {
            if let Episode::Armed(armed) = self.episode {
                self.episode = Episode::Owed(armed);
            }
            return Clock::SETTLED;
        }
        if let Episode::Owed(_) = self.episode {
            self.episode = Episode::Armed(now);
        }
        match (self.episode, self.running(now)) {
            (Episode::Armed(start) | Episode::Playing(start), Some(ms)) => {
                self.episode = Episode::Playing(start);
                let ride_ms = match self.ride {
                    Ride::Ahead => {
                        self.ride = Ride::Since(start);
                        Some(ms)
                    }
                    Ride::Since(from) => Some(
                        u64::try_from(now.saturating_duration_since(from).as_millis())
                            .unwrap_or(u64::MAX),
                    ),
                    Ride::Done => None,
                };
                Clock {
                    t_ms: Some(ms),
                    ride_ms,
                }
            }
            _ => {
                self.episode = Episode::Idle;
                Clock::SETTLED
            }
        }
    }

    /// The colours of the line under the card ("enhanced by the aterm
    /// harness …"), one [`InkCell`] per cell of `row`, columns `col..col + len`,
    /// sorted and unique. During an episode (and `animate`) the palette drifts
    /// and a bright band sweeps the line twice; otherwise it is the settled
    /// gradient, the palette end to end, constant bytes. The line wears the
    /// last tick's palette.
    ///
    /// `ground` is the pane's background, `0x00RRGGBB`: dark or light by
    /// [`dark_ground`]. Ink stays legible: on dark every letter is lifted to
    /// a relative luminance floor; on light it is the print ramp's ink in
    /// the paper law's point-mark ink ([`paper_mark_ink`], 4.5:1 or more
    /// against `ground`), opaque, and the sweep only deepens it.
    #[must_use]
    pub fn line_ink(
        &self,
        now: Instant,
        row: u16,
        col: u16,
        len: u16,
        ground: u32,
        animate: bool,
    ) -> Vec<InkCell> {
        let mut out = Vec::with_capacity(usize::from(len));
        self.line_ink_into(Some(now), row, col, len, ground, animate, &mut out);
        out
    }

    /// [`Self::line_ink`] appended to `out`: a host that keeps the line's
    /// buffer allocates nothing for an episode's frames once it is grown.
    /// `now` is `None` for a card the host knows is still
    /// ([`Self::is_still`], a frame it replayed): a still card's line is the
    /// settled gradient at every instant, and no clock need be read for it.
    #[allow(
        clippy::too_many_arguments,
        reason = "line_ink's own inputs and the buffer it fills"
    )]
    pub fn line_ink_into(
        &self,
        now: Option<Instant>,
        row: u16,
        col: u16,
        len: u16,
        ground: u32,
        animate: bool,
        out: &mut Vec<InkCell>,
    ) {
        let ground = ground & 0x00FF_FFFF;
        let dark = dark_ground(ground);
        let running = now.and_then(|now| self.running(now));
        debug_assert!(
            now.is_some() || self.is_still() || !animate,
            "a card whose episode runs needs the clock for its line"
        );
        let clock = match (animate, running) {
            (true, Some(ms)) => Clock {
                t_ms: Some(ms),
                ride_ms: None,
            },
            _ => Clock::SETTLED,
        };
        let phi = clock.drift(LINE_DRIFT);
        let env = clock.head_env();
        let pos = (clock.u() * LINE_SWEEPS).fract() * 1.4 - 0.2;
        let span = f32::from(len.saturating_sub(1).max(1));
        let (palette, inks) = (self.palette, &self.inks);
        let gain = if dark {
            LINE_SWEEP_GAIN
        } else {
            LINE_SWEEP_GAIN_LIGHT
        };
        out.extend(
            (0..len)
                .map_while(|i| Some((i, col.checked_add(i)?)))
                .map(|(i, c)| {
                    let x = f32::from(i) / span;
                    let at = tri(x + phi);
                    let d = (x - pos) / LINE_SWEEP_SIGMA;
                    let w = env * (-d * d).exp() * gain;
                    let rgb = if dark {
                        let base = palette.at(at);
                        mix_rgb(lift_to_luminance(base, LINE_DARK_LUMA_FLOOR), WHITE, w)
                    } else {
                        // Toward black: on a light ground darker only reads
                        // further from it.
                        mix_rgb(inks.at(palette, ground, at), 0, w)
                    };
                    InkCell {
                        row,
                        col: c,
                        color: [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
                    }
                }),
        );
    }
}

/// `rgb` mixed toward white just far enough to reach WCAG relative luminance
/// `floor`: by the least mix `t` on the grid of `2^-14` steps whose colour
/// reaches it (deterministic); a colour already there is returned unchanged.
///
/// Found four ways a round, seven rounds: the three points of a round are
/// tested independently, so they run side by side where a bisection's
/// fourteen tests each wait on the last (the card's line lifts a cell this
/// way on every episode frame). The same `t` as that bisection, exactly:
/// the colour's luminance never falls as `t` grows (each channel is a
/// rounded `c + (255 − c)·t`, the luminance a sum of increasing table
/// entries), so both find the grid's first `t` that reaches the floor, and
/// every point either tests is a multiple of `2^-14` at most 1, exact in an
/// `f32` (`a_lift_finds_the_bisections_mix`).
fn lift_to_luminance(rgb: u32, floor: f32) -> u32 {
    if relative_luminance(rgb) >= floor {
        return rgb;
    }
    let reaches = |t: f32| relative_luminance(mix_rgb(rgb, WHITE, t)) >= floor;
    // The last grid point known short of the floor, and the round's step.
    let (mut short, mut step) = (0.0f32, 0.25f32);
    for _ in 0..7 {
        let (a, b, c) = (
            reaches(short + step),
            reaches(short + 2.0 * step),
            reaches(short + 3.0 * step),
        );
        short += step * f32::from(u8::from(!a) + u8::from(!b) + u8::from(!c));
        step *= 0.25;
    }
    mix_rgb(rgb, WHITE, short + 4.0 * step)
}

/// Whether a pane's background `bg` (`0x00RRGGBB`) is a dark ground: WCAG
/// relative luminance under one half. The card's polarity, for the host's
/// scene and the line alike.
#[must_use]
pub fn dark_ground(bg: u32) -> bool {
    relative_luminance(bg) < 0.5
}

/// A frame's fingerprint ([`HarnessCard::tick`]): `0` when it holds no
/// quad; otherwise a hash of every quad, `under`'s then `over`'s, folded
/// with the frame index while an episode animates (`clock`).
fn frame_fp(under: &[GlowQuad], over: &[GlowQuad], clock: Clock) -> u64 {
    #[cfg(test)]
    FRAME_FPS.with(|n| n.set(n.get() + 1));
    if under.is_empty() && over.is_empty() {
        return 0;
    }
    let mut h = mix(0x4841_524E_4553_5321 ^ under.len() as u64);
    for q in under.iter().chain(over) {
        for w in q.damage_words() {
            h = mix(h ^ w);
        }
    }
    if let Some(t) = clock.t_ms {
        h = mix(h ^ (1 + t / FRAME_MS));
    }
    h
}

#[cfg(test)]
thread_local! {
    /// Frame fingerprints made ([`frame_fp`]): the work a host that never
    /// reads one is spared.
    static FRAME_FPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How a frame lays the trail ([`Emit::trail_lay`]): from `tx0` to where it
/// is drawn to (`drawn`, the kitty's rear on its ride) of its whole `len`, in
/// segments of `seg` pixels from `tx0`, its stripes spanning `[ya, yb)` and
/// stepped by up to `amp` ([`Emit::wave`]) — but for the segments reaching
/// past `held`, the kitty's left edge where the trail runs under it
/// (`i64::MAX` where it does not), which hold still.
#[derive(Clone, Copy, Debug)]
struct TrailLay {
    tx0: i64,
    drawn: i64,
    len: i64,
    seg: i64,
    ya: i64,
    yb: i64,
    amp: i64,
    held: i64,
}

/// Lay `q` less every rectangle `cover(k)` for `k` in `k..=last`, piece by
/// piece (above each cut, below it, then left and right of it on its rows)
/// through `lay`; `false` once `lay` says the budget is spent.
fn lay_cut(
    q: Rect,
    k: i64,
    last: i64,
    cover: &impl Fn(i64) -> Rect,
    lay: &mut impl FnMut(Rect) -> bool,
) -> bool {
    if q.is_empty() {
        return true;
    }
    if k > last {
        return lay(q);
    }
    let c = cover(k).meet(q);
    if c.is_empty() {
        return lay_cut(q, k + 1, last, cover, lay);
    }
    [
        Rect { y1: c.y0, ..q },
        Rect { y0: c.y1, ..q },
        Rect {
            x1: c.x0,
            y0: c.y0,
            y1: c.y1,
            ..q
        },
        Rect {
            x0: c.x1,
            y0: c.y0,
            y1: c.y1,
            ..q
        },
    ]
    .into_iter()
    .all(|piece| lay_cut(piece, k + 1, last, cover, lay))
}

/// One frame's emitter: the scene, its pixels, its clock, and the kitty it
/// seats with the frame of its art the clock shows.
struct Emit<'a> {
    scene: &'a CardScene,
    geo: Geo,
    clock: Clock,
    seed: u64,
    /// The kitty where it is this frame ([`ridden`]) and the frame of its
    /// art it wears.
    kitty: Option<(KittyAt, &'static [&'static str])>,
    /// Where it sits once its ride is done ([`kitty_rect`]).
    seat: Option<KittyAt>,
}

impl<'a> Emit<'a> {
    /// The marks of `scene` at `clock`: the kitty seated, and where its
    /// ride has it.
    fn new(scene: &'a CardScene, geo: Geo, clock: Clock, seed: u64) -> Self {
        let seat = kitty_rect(&geo, scene);
        Self {
            scene,
            geo,
            clock,
            seed,
            kitty: seat.map(|at| (ridden(&geo, scene, at, clock), kitty_frame(&at.art, clock))),
            seat,
        }
    }

    /// Whether the kitty rides in this episode: it is drawn, and there is a
    /// trail to ride.
    fn rides(&self) -> bool {
        self.seat
            .is_some_and(|seat| ride_span(&self.geo, self.scene, &seat).is_some())
    }

    /// The border's paint at arc fraction `s`.
    fn border_paint(&self, s: f32) -> Paint {
        let phi = PHI_SETTLED + self.clock.drift(PHI_DRIFT);
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + phi), self.scene.dark);
        let env = self.clock.head_env();
        let w = if env > 0.0 {
            let head = (self.clock.u() * HEAD_LAPS).fract();
            let d = (s - head).rem_euclid(1.0);
            let d = d.min(1.0 - d) / HEAD_SIGMA;
            env * (-d * d).exp()
        } else {
            0.0
        };
        if self.scene.dark {
            light(mix_rgb(base, WHITE, w * HEAD_GAIN), f32::from(BORDER_COV))
        } else {
            ink(
                mix_rgb(print_line(base), 0, w * LIGHT_HEAD_DEEPEN),
                PRINT_ALPHA,
            )
        }
    }

    /// The border: top edge left → right, right side down, bottom edge right
    /// → left, left side up; arc fraction measured from the top-left corner.
    /// Laid on the OUTER rectangle, its air included: a top edge with air
    /// lies in the band of the row over the box and is tagged with it, a
    /// bottom edge in the band of the row under it, and each side gains a
    /// slice in each of those bands — one band a quad, as everywhere.
    fn border(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let g = self.geo;
        let o = g.outer;
        let (w, h, t) = (o.x1 - o.x0, o.y1 - o.y0, g.t);
        let per = (2 * (w + h)) as f32;
        let all = g.clip;
        let n = ((w + g.cw - 1) / g.cw).clamp(1, MAX_EDGE_SEGMENTS);
        // Horizontal edges: gradient segments whose ends read the arc (the top
        // runs left → right from the corner, the bottom right → left). A
        // segment's left end is the last one's right end, the same `x` read
        // the same way, so its paint is that one's, not painted again.
        for bottom in [false, true] {
            let y0 = if bottom { o.y1 - t } else { o.y0 };
            let arc = |x: i64| {
                if bottom {
                    (w + h + (o.x1 - x)) as f32
                } else {
                    (x - o.x0) as f32
                }
            };
            let mut pa = self.border_paint(arc(o.x0) / per);
            for k in 0..n {
                let xa = o.x0 + w * k / n;
                let xb = o.x0 + w * (k + 1) / n;
                let r = Rect {
                    x0: xa,
                    y0,
                    x1: xb,
                    y1: y0 + t,
                };
                let pb = self.border_paint(arc(xb) / per);
                if !sink.push(out, all, r, pa, pb) {
                    return false;
                }
                pa = pb;
            }
        }
        // Sides: one flat slice per row band the outer rectangle crosses (the
        // boxed rows', and the half rows of air over and under them),
        // coloured at its midpoint (the right side runs down, the left side
        // up).
        let bands = (o.y0 - g.oy).div_euclid(g.ch)..=(o.y1 - 1 - g.oy).div_euclid(g.ch);
        for left in [false, true] {
            let x0 = if left { o.x0 } else { o.x1 - t };
            let arc = |y: i64| {
                if left {
                    (2 * w + h + (o.y1 - y)) as f32
                } else {
                    (w + (y - o.y0)) as f32
                }
            };
            for r in bands.clone() {
                let ya = g.band(r).max(o.y0 + t);
                let yb = g.band(r + 1).min(o.y1 - t);
                if yb <= ya {
                    continue;
                }
                let p = self.border_paint(arc((ya + yb) / 2) / per);
                let slice = Rect {
                    x0,
                    y0: ya,
                    x1: x0 + t,
                    y1: yb,
                };
                if !sink.flat(out, all, slice, p) {
                    return false;
                }
            }
        }
        true
    }

    /// The step wave's displacement (px, `+` up) for segment `k` of the
    /// trail `lay`: none for a segment that reaches under the kitty (past
    /// [`TrailLay::held`]). The cat holds the ribbon's end still and the wave
    /// runs along the free stretch behind it, so no stepped stripe shows past
    /// the cat's outline where its art leaves the ground open beside the
    /// trail's band (above the tail swept flat in the flick, under the
    /// haunch).
    fn wave(&self, lay: &TrailLay, k: i64) -> i64 {
        let amp = lay.amp;
        if lay.tx0 + (k + 1) * lay.seg > lay.held {
            return 0;
        }
        match self.clock.t_ms {
            Some(t) if t < WAVE_END_MS && amp > 0 => {
                if (k + (t / WAVE_STEP_MS) as i64).rem_euclid(2) == 0 {
                    amp
                } else {
                    -amp
                }
            }
            _ => 0,
        }
    }

    /// The shimmer's weight at trail fraction `x`.
    fn shimmer(&self, x: f32) -> f32 {
        let Some(t) = self.clock.t_ms else {
            return 0.0;
        };
        let pos = (t as f32 / SHIMMER_MS).fract() * 1.4 - 0.2;
        let d = (x - pos) / SHIMMER_SIGMA;
        self.clock.head_env() * (-d * d).exp() * SHIMMER_GAIN
    }

    /// One stripe's paint under shimmer weight `w`: lifted toward white by
    /// `w` on both grounds, light on dark and a gloss on a light ground's
    /// print ink, where the top and bottom stripes take
    /// [`LIGHT_GLINT_EDGE`] of it.
    fn stripe_paint(&self, k: usize, w: f32) -> Paint {
        let base = self.scene.palette.stripe(k, self.scene.dark);
        if self.scene.dark {
            light(mix_rgb(base, WHITE, w), f32::from(TRAIL_COV))
        } else {
            let edge = k == 0 || k + 1 == TRAIL_STOPS.len();
            let w = if edge { w * LIGHT_GLINT_EDGE } else { w };
            ink(mix_rgb(base, WHITE, w), PRINT_ALPHA)
        }
    }

    /// The rainbow trail: six stripes from the text's end to the kitty's
    /// rear, under its art to the haunch ([`KittyAt::rear`]), as tall as the
    /// kitty's body ([`trail_band`]: the cat leads, the rainbow trails),
    /// split at the row bands they cross. During an episode
    /// it steps and shimmers in segments; settled, every segment of a stripe
    /// would wear the same paint, so each stripe is one quad end to end — a
    /// card at rest replays six quads of trail into every frame, not ninety
    /// six.
    ///
    /// The step wave moves the ribbon's outline, not its stripes: a segment
    /// stepped up `d` px grows the top stripe up by `d` and shrinks the bottom
    /// stripe by as much (and the reverse stepped down), so the silhouette
    /// steps while the stripes between keep their places.
    fn trail(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let Some(lay) = self.trail_lay() else {
            return true;
        };
        let TrailLay {
            tx0,
            drawn,
            len,
            seg,
            ya,
            yb,
            ..
        } = lay;
        let inner = self.geo.inner();
        let stripes = TRAIL_STOPS.len() as i64;
        let h = yb - ya;
        let mut k = 0;
        let mut xa = tx0;
        // A segment's left end is the last one's right end: its shimmer and
        // its stripes' paints are that one's, not computed again.
        let wa = self.shimmer(0.0);
        let mut ends: [Paint; TRAIL_STOPS.len()] =
            std::array::from_fn(|s| self.stripe_paint(s, wa));
        while xa < drawn {
            let xb = (xa + seg).min(drawn);
            let d = self.wave(&lay, k);
            let wb = self.shimmer((xb - tx0) as f32 / len as f32);
            for (s, end) in ends.iter_mut().enumerate() {
                let si = s as i64;
                let mut y0 = ya + h * si / stripes;
                let mut y1 = ya + h * (si + 1) / stripes;
                if s == 0 {
                    y0 -= d;
                }
                if s == TRAIL_STOPS.len() - 1 {
                    y1 -= d;
                }
                let r = Rect {
                    x0: xa,
                    y0,
                    x1: xb,
                    y1,
                };
                // Inside the stroke: light stacked on the border would double.
                let pb = self.stripe_paint(s, wb);
                if !sink.push(out, inner, r, *end, pb) {
                    return false;
                }
                *end = pb;
            }
            xa = xb;
            k += 1;
        }
        true
    }

    /// How this frame lays the trail ([`TrailLay`]); `None` where it draws
    /// none (no trail, no room, a band too short for its stripes).
    fn trail_lay(&self) -> Option<TrailLay> {
        let g = self.geo;
        let (
            Rect {
                x0: tx0,
                y0: ya,
                x1: tx1,
                y1: yb,
            },
            amp,
        ) = trail_rect(&g, self.scene, self.seat)?;
        let len = tx1 - tx0;
        // On the ride the trail is laid behind the kitty, to its rear: its
        // segments fall where the whole trail's do, the last cut there.
        let kitty = self.kitty.map(|(at, _)| at);
        let drawn = match kitty {
            Some(at) if ride(self.clock) < 1.0 => tx1.min(at.rear()),
            _ => tx1,
        };
        let held = kitty
            .filter(|at| drawn > at.rect.x0)
            .map_or(i64::MAX, |at| at.rect.x0);
        let seg = if self.clock.t_ms.is_some() {
            (TRAIL_SEG_CELLS * g.cw).max((len + MAX_TRAIL_SEGMENTS - 1) / MAX_TRAIL_SEGMENTS)
        } else {
            len
        };
        Some(TrailLay {
            tx0,
            drawn,
            len,
            seg,
            ya,
            yb,
            amp,
            held,
        })
    }

    /// The pixels segment `k` of the trail `lay` covers: its stripes, the
    /// top one's top and the bottom one's bottom stepped by the wave — one
    /// rectangle, `[xa, xb) × [ya − d, yb − d)`.
    fn trail_cover(&self, lay: &TrailLay, k: i64) -> Rect {
        let d = self.wave(lay, k);
        let xa = lay.tx0 + k * lay.seg;
        Rect {
            x0: xa,
            y0: lay.ya - d,
            x1: (xa + lay.seg).min(lay.drawn),
            y1: lay.yb - d,
        }
    }

    /// The kitty: its frame's art as opaque source-over rectangles — each run
    /// of one colour along a pixel row, carried down the rows under it that
    /// repeat it exactly (the same columns and colour, neither neighbour
    /// that colour) — split at the row bands they cross. About ninety-five
    /// quads two rows tall and a hundred and ten three, where a quad a run
    /// would be a hundred and eighteen and a hundred and sixty-two: the
    /// face's detail (glossy eyes, blush, the `ω` mouth) repeats down few
    /// rows.
    fn kitty(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let Some((at, frame)) = self.kitty else {
            return true;
        };
        let inks = KittyInk::new(self.scene.coat, self.scene.eye, self.scene.dark);
        let p = at.p;
        for (r, row) in frame.iter().enumerate() {
            let px = row.as_bytes();
            let mut c = 0;
            while c < px.len() {
                let ch = px[c];
                let run = px[c..].iter().take_while(|&&b| b == ch).count();
                // Row `other` holds this very run.
                let repeats = |other: &str| {
                    let o = other.as_bytes();
                    o[c..c + run].iter().all(|&b| b == ch)
                        && (c == 0 || o[c - 1] != ch)
                        && o.get(c + run).is_none_or(|&b| b != ch)
                };
                let carried = r > 0 && repeats(frame[r - 1]);
                if let (false, Some(rgb)) = (carried, inks.of(ch)) {
                    let down = frame[r + 1..]
                        .iter()
                        .take_while(|other| repeats(other))
                        .count();
                    let q = Rect {
                        x0: at.rect.x0 + c as i64 * p,
                        y0: at.rect.y0 + r as i64 * p,
                        x1: at.rect.x0 + (c + run) as i64 * p,
                        y1: at.rect.y0 + (r + 1 + down) as i64 * p,
                    };
                    let paint = Paint {
                        color: rgb,
                        alpha: 255,
                    };
                    if !sink.flat(out, self.geo.clip, q, paint) {
                        return false;
                    }
                }
                c += run;
            }
        }
        true
    }

    /// The rim's paint for a strip centred at `(x, y)`: the border's spectrum
    /// at the strip's angle round the kitty's centre `(cx, cy)`, measured
    /// from straight up — `palette(tri(2s + φ))`, the border's own sweep and
    /// drift, so the hue runs round the cat once with no seam and settles
    /// with the border's.
    fn rim_paint(&self, (x, y): (f32, f32), (cx, cy): (f32, f32)) -> Paint {
        let s = ((x - cx).atan2(cy - y) / std::f32::consts::TAU).rem_euclid(1.0);
        let phi = PHI_SETTLED + self.clock.drift(PHI_DRIFT);
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + phi), self.scene.dark);
        if self.scene.dark {
            light(base, f32::from(RIM_COV))
        } else {
            ink(base, PRINT_ALPHA)
        }
    }

    /// The rim (§10.6): a [`rim_px`]-wide outline just outside the kitty's
    /// silhouette ([`rim_solid`]: the art, its toes' one-pixel gaps closed),
    /// along every pixel edge with the silhouette on one side and nothing on
    /// the other, in the border's spectrum ([`Self::rim_paint`]),
    /// so a black or grey coat reads on a dark ground. Horizontal strips run
    /// along the art's rows and reach a rim's width past an outer corner to
    /// close it; vertical strips run down its columns and stop a rim's width
    /// short where a horizontal strip turns an inner corner — so no two
    /// strips overlap (a rim is at most half a kitty pixel, so none reaches
    /// another across one transparent pixel) and no light is doubled.
    /// Clipped to the box inside the border's stroke, and cut where it would
    /// lie on the trail (where the tail meets the rainbow: the rainbow is no
    /// dark ground to set the coat off, and light on light doubles to white).
    fn rim(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let Some((at, frame)) = self.kitty else {
            return true;
        };
        let (p, rim) = (at.p, rim_px(at.p, self.geo.t));
        if rim == 0 {
            return true;
        }
        let (h, w) = (frame.len() as i64, KITTY_W as i64);
        let solid = |r: i64, c: i64| rim_solid(frame, r, c);
        let r0 = at.rect;
        let centre = ((r0.x0 + r0.x1) as f32 * 0.5, (r0.y0 + r0.y1) as f32 * 0.5);
        let clip = self.geo.inner();
        let trail = self.trail_lay();
        let mut strip = |sink: &mut Sink, q: Rect| {
            let mid = ((q.x0 + q.x1) as f32 * 0.5, (q.y0 + q.y1) as f32 * 0.5);
            let paint = self.rim_paint(mid, centre);
            let mut lay = |piece: Rect| sink.flat(out, clip, piece, paint);
            // The trail's segments under the strip, if any.
            match trail {
                Some(t) if q.x0 < t.drawn && q.x1 > t.tx0 => {
                    let first = (q.x0 - t.tx0).max(0) / t.seg;
                    let last = (q.x1.min(t.drawn) - 1 - t.tx0) / t.seg;
                    lay_cut(q, first, last, &|k| self.trail_cover(&t, k), &mut lay)
                }
                _ => lay(q),
            }
        };
        // Horizontal strips at row boundary `b`: over the art (art at row
        // `b`, nothing `solid` at `b - 1`), then under it (art at `b - 1`,
        // nothing at `b`) — over and under art itself, never a toe's gap.
        for b in 0..=h {
            for (art_row, open_row, y0) in
                [(b, b - 1, r0.y0 + b * p - rim), (b - 1, b, r0.y0 + b * p)]
            {
                let edge = |c: i64| art_at(frame, art_row, c) && !solid(open_row, c);
                let mut c = 0;
                while c < w {
                    if !edge(c) {
                        c += 1;
                        continue;
                    }
                    let c0 = c;
                    while c < w && edge(c) {
                        c += 1;
                    }
                    // An outer corner at either end: nothing beside the run on
                    // either row, so the strip reaches round it.
                    let outer = |e: i64| !solid(art_row, e) && !solid(open_row, e);
                    let q = Rect {
                        x0: r0.x0 + c0 * p - if outer(c0 - 1) { rim } else { 0 },
                        y0,
                        x1: r0.x0 + c * p + if outer(c) { rim } else { 0 },
                        y1: y0 + rim,
                    };
                    if !strip(sink, q) {
                        return false;
                    }
                }
            }
        }
        // Vertical strips at column boundary `v`: left of the art (`solid` at
        // column `v`, not at `v - 1`), then right of it.
        for v in 0..=w {
            for (art_col, open_col, x0) in
                [(v, v - 1, r0.x0 + v * p - rim), (v - 1, v, r0.x0 + v * p)]
            {
                let edge = |r: i64| solid(r, art_col) && !solid(r, open_col);
                let mut r = 0;
                while r < h {
                    if !edge(r) {
                        r += 1;
                        continue;
                    }
                    let top = r;
                    while r < h && edge(r) {
                        r += 1;
                    }
                    // An inner corner at either end: the open column's pixel
                    // past the run is art, whose own strip turns the corner.
                    let q = Rect {
                        x0,
                        y0: r0.y0 + top * p + if solid(top - 1, open_col) { rim } else { 0 },
                        x1: x0 + rim,
                        y1: r0.y0 + r * p - if solid(r, open_col) { rim } else { 0 },
                    };
                    if q.y1 > q.y0 && !strip(sink, q) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// The trail's ink this frame on a light ground, segment by segment as
    /// [`Self::trail`] lays it ([`Self::trail_cover`]): where a light mark is
    /// the paper's white ([`lay_toned`]). None on dark, and where no trail
    /// is drawn.
    fn trail_ink(&self) -> TrailInk {
        let mut ink = TrailInk::default();
        if self.scene.dark {
            return ink;
        }
        let Some(lay) = self.trail_lay() else {
            return ink;
        };
        while ink.n < ink.covers.len() && lay.tx0 + ink.n as i64 * lay.seg < lay.drawn {
            ink.covers[ink.n] = self.trail_cover(&lay, ink.n as i64);
            ink.n += 1;
        }
        ink
    }

    /// The colour the `k`th mark of its set is laid in: `dark` on a dark
    /// ground (added light), on a light one its print ink
    /// ([`CardPalette::star_ink`]), which [`lay_toned`] turns the paper's
    /// white where it lies on the trail.
    fn mark_ink(&self, k: usize, dark: u32) -> u32 {
        if self.scene.dark {
            dark
        } else {
            self.scene.palette.star_ink(k)
        }
    }

    /// The steady field: twinkling during an episode (easing onto a steady
    /// brightness over its last fifth), fixed once settled.
    fn stars(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let Some(region) = star_region(&self.geo, self.scene) else {
            return true;
        };
        let dark = self.scene.dark;
        let trail = self.trail_ink();
        let (spots, n) = field_spots(&self.geo, self.scene, self.seed, !dark);
        let u = self.clock.u();
        let settle = smooth((u - (1.0 - STAR_SETTLE_SPAN)) / STAR_SETTLE_SPAN);
        // The ride: a star left of the seat is still to come until the
        // kitty's back has passed it by its reach, then comes up.
        let riding = self
            .seat
            .and_then(|seat| Some((seat, ride_span(&self.geo, self.scene, &seat)?)));
        for (i, s) in spots[..n].iter().enumerate() {
            let arm = i64::from(star_arm_px(star_arm(self.geo.ch as f32, s.ratio)));
            let up = match (self.clock.ride_ms, riding) {
                (Some(t), Some((seat, (from, dist)))) if s.x < seat.rect.x0 => {
                    let passed = ride_at((s.x + arm + 1 - from) as f32 / dist as f32);
                    smooth((t as f32 - passed) / STAR_REVEAL_MS)
                }
                _ => 1.0,
            };
            if up <= 0.0 {
                continue;
            }
            let (env, gold) = match self.clock.t_ms {
                Some(t) => {
                    let e = twinkle_env(t as f32 / 1000.0, s.phase);
                    let e = (e + (STAR_STEADY_ENV - e) * settle) * up;
                    (e, s.gold || (i % 2 == 0 && twinkle_peak(e)))
                }
                None => (STAR_STEADY_ENV, s.gold),
            };
            let light_rgb = match self.scene.palette {
                CardPalette::Cruise => twinkle_rgb(i % 2 == 0),
                CardPalette::Land => {
                    if i % 2 == 0 {
                        twinkle_rgb(true)
                    } else {
                        mix_rgb(SUNSET[3], WHITE, 0.5)
                    }
                }
            };
            if !push_star(
                out,
                sink,
                region,
                (s.x, s.y),
                arm,
                STAR_COV * env,
                gold,
                self.mark_ink(i, light_rgb),
                trail.covers(),
                dark,
            ) {
                return false;
            }
        }
        true
    }

    /// The sparkles the kitty sheds on its ride: the `k`th where its back
    /// was when it had come `(k + 1) / (SHED_SPARKLES + 1)` of the way, at a
    /// height in its body band the seed picks, fading over [`SHED_MS`] as its
    /// arm draws in. Each sits whole in the star region; none outlives the
    /// ride (the last is shed at 0.54 s and out by 0.99 s).
    fn sparkles(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let (Some(t), Some(seat)) = (self.clock.ride_ms, self.seat) else {
            return true;
        };
        if t >= RIDE_MS {
            return true;
        }
        let (Some((from, dist)), Some(region)) = (
            ride_span(&self.geo, self.scene, &seat),
            star_region(&self.geo, self.scene),
        ) else {
            return true;
        };
        let dark = self.scene.dark;
        let trail = self.trail_ink();
        // On a light ground the trail's ink at every step of the wave, so far
        // laid: a sparkle shed on it lies wholly there, one white glint on the
        // print ([`lay_toned`]), never across the band's edge in two tones.
        let band = if dark {
            None
        } else {
            self.trail_lay().map(|lay| {
                Rect {
                    x0: lay.tx0,
                    y0: lay.ya + lay.amp,
                    x1: lay.drawn,
                    y1: lay.yb - lay.amp,
                }
                .meet(region)
            })
        };
        let (y0, y1) = seat.body();
        for k in 0..SHED_SPARKLES {
            let f = (k + 1) as f32 / (SHED_SPARKLES + 1) as f32;
            let age = t as f32 - ride_at(f);
            if !(0.0..SHED_MS as f32).contains(&age) {
                continue;
            }
            let tau = age / SHED_MS as f32;
            let ratio = STAR_ARM_FINE * (1.0 - 0.4 * tau) * if dark { 1.0 } else { STAR_ARM_INK };
            let arm = i64::from(star_arm_px(star_arm(self.geo.ch as f32, ratio)));
            let m = arm + 1;
            // A star's reach behind the kitty's back as it was then.
            let x = from + (dist as f32 * f).round() as i64 - m;
            let y = y0
                + ((y1 - y0) as f32 * (0.15 + 0.7 * unit(self.seed, 0x5348_4544 ^ k as u64)))
                    as i64;
            let (lo, hi) = (
                (region.x0 + m, region.y0 + m),
                (region.x1 - m, region.y1 - m),
            );
            if hi.0 <= lo.0 || hi.1 <= lo.1 {
                return true;
            }
            let mut centre = (x.clamp(lo.0, hi.0 - 1), y.clamp(lo.1, hi.1 - 1));
            // Wholly on the trail's ink, where the band has room.
            if let Some(b) = band.filter(|b| b.x1 - b.x0 > 2 * m && b.y1 - b.y0 > 2 * m) {
                centre = (
                    centre.0.clamp(b.x0 + m, b.x1 - m - 1),
                    centre.1.clamp(b.y0 + m, b.y1 - m - 1),
                );
            }
            if !push_star(
                out,
                sink,
                region,
                centre,
                arm,
                SHED_COV * (1.0 - tau).powf(1.5),
                false,
                self.mark_ink(k, self.scene.palette.mark(k)),
                trail.covers(),
                dark,
            ) {
                return false;
            }
        }
        true
    }

    /// The burst: [`BURST_STARS`] stars on a ring thrown out from the kitty
    /// (or the trail's end, with no kitty) over [`BURST_MS`], shrinking and
    /// fading as they fly ([`Self::burst_ring`]). Only during an episode's
    /// first [`BURST_MS`]. On a light ground a star that crosses the trail's
    /// edge, or reaches past its end onto the kitty, is whole in two tones
    /// ([`lay_toned`]).
    fn burst(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let Some(ring) = self.burst_ring() else {
            return true;
        };
        let trail = self.trail_ink();
        for (k, &centre) in ring.centres.iter().enumerate() {
            if !push_star(
                out,
                sink,
                ring.region,
                centre,
                ring.arm,
                ring.cov,
                true,
                self.mark_ink(k, self.scene.palette.mark(k)),
                trail.covers(),
                self.scene.dark,
            ) {
                return false;
            }
        }
        true
    }

    /// Where this frame's burst lies ([`BurstRing`]): `None` outside its
    /// [`BURST_MS`] (from where the kitty lands, as it lands; from the
    /// episode's start where it does not ride), with no region, or with
    /// neither a kitty nor a trail to throw it from.
    fn burst_ring(&self) -> Option<BurstRing> {
        let age = match self.clock.ride_ms {
            Some(r) if self.rides() => r.checked_sub(RIDE_MS),
            _ => self.clock.t_ms,
        };
        let t = age.filter(|&t| t < BURST_MS)?;
        let region = star_region(&self.geo, self.scene)?;
        let g = self.geo;
        let ((cx, mid), half_w) = match self.seat {
            Some(at) => {
                let r = at.rect;
                (((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2), (r.x1 - r.x0) / 2)
            }
            None => {
                let (_, end) = self.scene.trail?;
                ((g.col(end).min(region.x1), g.band(g.top + 1)), 0)
            }
        };
        let tau = t as f32 / BURST_MS as f32;
        let fly = 1.0 - (1.0 - tau).powi(3);
        let rx = (half_w as f32 + 2.0 * g.cw as f32).max(3.0 * g.cw as f32);
        let ry = 0.85 * g.ch as f32;
        let reach = 0.35 + 0.65 * fly;
        let ratio = STAR_ARM_HERO + (STAR_ARM_FINE - STAR_ARM_HERO) * tau;
        let ratio = ratio * if self.scene.dark { 1.0 } else { STAR_ARM_INK };
        let turn = unit(self.seed, 0x4255_5253) * std::f32::consts::TAU;
        Some(BurstRing {
            region,
            centres: std::array::from_fn(|k| {
                let theta = turn + std::f32::consts::TAU * k as f32 / BURST_STARS as f32;
                (
                    cx + (theta.cos() * rx * reach).round() as i64,
                    mid + (theta.sin() * ry * reach).round() as i64,
                )
            }),
            arm: i64::from(star_arm_px(star_arm(g.ch as f32, ratio))),
            cov: BURST_COV * (1.0 - tau).powf(1.2),
        })
    }
}

/// One frame's burst ([`Emit::burst_ring`]): its stars' centres, their arm
/// and coverage, and the region they are clipped to.
#[derive(Clone, Copy, Debug)]
struct BurstRing {
    region: Rect,
    centres: [(i64, i64); BURST_STARS],
    arm: i64,
    cov: f32,
}

/// A light frame's trail ink, segment by segment ([`Emit::trail_ink`]): at
/// most [`MAX_TRAIL_SEGMENTS`] (a segment is at least the trail's length
/// over that), none on dark.
#[derive(Clone, Copy, Debug)]
struct TrailInk {
    covers: [Rect; MAX_TRAIL_SEGMENTS as usize],
    n: usize,
}

impl Default for TrailInk {
    fn default() -> Self {
        Self {
            covers: [Rect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            }; MAX_TRAIL_SEGMENTS as usize],
            n: 0,
        }
    }
}

impl TrailInk {
    /// The segments' covers, left to right.
    fn covers(&self) -> &[Rect] {
        &self.covers[..self.n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::Geom;
    use crate::cursor_glow::InkRole;
    use crate::effect_util::push_twinkle_star;

    /// A launch card at a 9 × 18 cell: text to column 31, the trail to the
    /// kitty at column 40, the box 60 columns wide, four boxed rows (the card's
    /// three and the line's), in a pane whose grid starts below a tab strip.
    fn scene() -> CardScene {
        CardScene {
            origin_x: 10,
            origin_y: 46,
            cw: 9,
            ch: 18,
            row_base: 2,
            pad_left: 10,
            pad_above: 0,
            pad_below: 0,
            top: 3,
            rows: 4,
            cols: 60,
            trail: Some((32, 40)),
            kitty_col: Some(40),
            kitty_rows: 2,
            clip: WHOLE_STREAM,
            palette: CardPalette::Cruise,
            dark: true,
            ground: NIGHT,
            serious: false,
            coat: 0x00E8_A060,
            eye: 0x0040_C080,
        }
    }

    /// Aterm Night's and Aterm Day's backgrounds: a dark and a light ground
    /// for the line.
    const NIGHT: u32 = 0x0012_1721;
    const DAY: u32 = 0x00F8_FAFD;

    /// WCAG 2 contrast of `a` against `b`.
    fn contrast(a: u32, b: u32) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// A clip that cuts nothing: the whole stream.
    const WHOLE_STREAM: (i32, i32, i32, i32) = (i32::MIN, i32::MIN, i32::MAX, i32::MAX);

    /// [`scene`]'s pane, cut short of the box: the right border's column and
    /// the left padding fall outside it, and the line's row is cut through
    /// its middle. The kitty (columns 40 to 46) still fits.
    fn clipped() -> CardScene {
        let s = scene();
        let (cw, ch) = (i32::from(s.cw), i32::from(s.ch));
        CardScene {
            clip: (
                s.origin_x,
                0,
                s.origin_x + 50 * cw,
                s.origin_y + i32::from(s.top + 3) * ch + ch / 2,
            ),
            ..s
        }
    }

    /// The scenes every law is checked on: both themes and palettes, odd cell
    /// sizes, a grid origin inside the padding, no padding, a negative origin
    /// (the box clipped at the stream's left edge), a card with no line row,
    /// a cell height whose kitty would reach into the top border sized from
    /// the whole band (35 px: `35 % 7 == 0` under a 2 px stroke), a box
    /// larger than its pane's clip, and the box's air — over and under it on
    /// both themes, over a three-row box only, and at the pane's first row,
    /// where the air over it is ignored — each at BOTH HEIGHTS (§10.6): the
    /// two-row kitty ([`scenes_two_rows`]), then the same scenes with the
    /// tall kitty.
    fn scenes() -> Vec<CardScene> {
        let two = scenes_two_rows();
        let tall = two.iter().map(|s| CardScene {
            kitty_rows: 3,
            ..*s
        });
        two.iter().copied().chain(tall).collect()
    }

    /// [`scenes`] with the two-row kitty.
    fn scenes_two_rows() -> Vec<CardScene> {
        let base = scene();
        let mut v = vec![base];
        v.push(CardScene {
            dark: false,
            ..base
        });
        v.push(CardScene {
            palette: CardPalette::Land,
            ..base
        });
        v.push(CardScene {
            palette: CardPalette::Land,
            dark: false,
            ..base
        });
        v.push(CardScene {
            cw: 8,
            ch: 16,
            pad_left: 0,
            pad_above: 16,
            origin_x: 0,
            origin_y: 0,
            row_base: 0,
            top: 0,
            ..base
        });
        v.push(CardScene {
            cw: 16,
            ch: 33,
            kitty_col: Some(44),
            trail: Some((32, 44)),
            ..base
        });
        v.push(CardScene {
            origin_x: -3,
            pad_left: 2,
            rows: 3,
            ..base
        });
        v.push(CardScene {
            cw: 7,
            ch: 13,
            rows: 3,
            dark: false,
            trail: None,
            ..base
        });
        v.push(CardScene {
            cw: 17,
            ch: 35,
            kitty_col: Some(44),
            trail: Some((32, 44)),
            ..base
        });
        v.push(clipped());
        v.push(CardScene {
            dark: false,
            palette: CardPalette::Land,
            ..clipped()
        });
        v.push(aired(base));
        v.push(aired(CardScene {
            dark: false,
            palette: CardPalette::Land,
            ..base
        }));
        v.push(CardScene {
            cw: 16,
            ch: 33,
            pad_above: 33,
            rows: 3,
            kitty_col: Some(44),
            trail: Some((32, 44)),
            ..base
        });
        v
    }

    /// `s` with a row of blank pane over and under its box: the border
    /// stands half a row clear of the boxed rows on both sides.
    fn aired(s: CardScene) -> CardScene {
        CardScene {
            pad_above: s.ch,
            pad_below: s.ch,
            ..s
        }
    }

    /// One tick's output on a fresh pair of streams.
    fn frame(
        card: &mut HarnessCard,
        now: Instant,
        s: &CardScene,
        animate: bool,
    ) -> (Vec<GlowQuad>, Vec<GlowQuad>, u64) {
        let (mut under, mut over) = (Vec::new(), Vec::new());
        let fp = card.tick(now, s, animate, &mut under, &mut over);
        (under, over, fp)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The left edge of the kitty's opaque art among `over`'s quads.
    fn kitty_left(over: &[GlowQuad]) -> Option<i64> {
        over.iter()
            .filter(|q| q.alpha == 255)
            .map(|q| i64::from(q.x))
            .min()
    }

    /// Where `s`'s ride starts: the trail's first pixel in the box
    /// ([`ride_span`]).
    fn ride_from(s: &CardScene) -> i64 {
        let g = Geo::new(s).expect("a scene");
        let seat = kitty_rect(&g, s).expect("a kitty");
        ride_span(&g, s, &seat).expect("a ride").0
    }

    /// A card whose settled frame of `s` went on the glass at `at` (a tick
    /// held back, unarmed): its kitty seen seated.
    fn shown_at_rest(s: &CardScene, at: Instant) -> HarnessCard {
        let mut card = HarnessCard::default();
        frame(&mut card, at, s, false);
        card
    }

    /// The settled frame for `s` with the test seed.
    fn settled(s: &CardScene) -> (Vec<GlowQuad>, Vec<GlowQuad>, u64) {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0 + ms(EPISODE_MS + 1), s, true)
    }

    /// One frame's marks, each drawn alone into its own stream (the budget
    /// never binding): what [`HarnessCard::tick`] lays, mark by mark.
    #[derive(Debug, Default)]
    struct Parts {
        border: Vec<GlowQuad>,
        trail: Vec<GlowQuad>,
        kitty: Vec<GlowQuad>,
        rim: Vec<GlowQuad>,
        stars: Vec<GlowQuad>,
        sparkles: Vec<GlowQuad>,
        burst: Vec<GlowQuad>,
    }

    /// [`Parts`] of the frame of `s` at `clock`, with the test seed.
    fn parts(s: &CardScene, clock: Clock) -> Parts {
        parts_seeded(s, clock, 7)
    }

    /// [`Parts`] of the frame of `s` at `clock` for `seed`, each mark laid
    /// with no budget.
    fn parts_seeded(s: &CardScene, clock: Clock, seed: u64) -> Parts {
        let geo = Geo::new(s).expect("a scene");
        let cx = Emit::new(s, geo, clock, seed);
        let mut out = Parts::default();
        let sink = || Sink {
            geo,
            left: usize::MAX,
        };
        assert!(cx.border(&mut sink(), &mut out.border));
        assert!(cx.trail(&mut sink(), &mut out.trail));
        assert!(cx.kitty(&mut sink(), &mut out.kitty));
        assert!(cx.rim(&mut sink(), &mut out.rim));
        assert!(cx.stars(&mut sink(), &mut out.stars));
        assert!(cx.sparkles(&mut sink(), &mut out.sparkles));
        assert!(cx.burst(&mut sink(), &mut out.burst));
        out
    }

    /// The box and band law for one quad: the box its air included, up to
    /// half a row over it (none at the pane's first row) and under it.
    fn assert_in_band_and_box(q: &GlowQuad, s: &CardScene, what: &str) {
        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
        assert!(q.w > 0 && q.h > 0, "{what}: an empty quad {q:?}");
        let r = i64::from(q.row) - i64::from(s.row_base);
        let band0 = i64::from(s.origin_y) + r * i64::from(s.ch);
        assert!(
            band0 <= y0 && y1 <= band0 + i64::from(s.ch),
            "{what}: {q:?} leaves its band [{band0}, {})",
            band0 + i64::from(s.ch)
        );
        let cw = i64::from(s.cw);
        let bx0 = (i64::from(s.origin_x) - i64::from(s.pad_left).min(cw / 2)).max(0);
        let bx1 = i64::from(s.origin_x) + (i64::from(s.cols) + 1) * cw;
        let ch = i64::from(s.ch);
        let air = |pad: u16| i64::from(pad).min(ch / 2);
        let top = i64::from(s.origin_y) + i64::from(s.top) * ch;
        let by0 = top - if s.top > 0 { air(s.pad_above) } else { 0 };
        let by1 = top + i64::from(s.rows) * ch + air(s.pad_below);
        assert!(
            bx0 <= x0 && x1 <= bx1 && by0 <= y0 && y1 <= by1,
            "{what}: {q:?} leaves the box x {bx0}..{bx1} y {by0}..{by1}"
        );
        let (cx0, cy0, cx1, cy1) = s.clip;
        assert!(
            i64::from(cx0) <= x0
                && x1 <= i64::from(cx1)
                && i64::from(cy0) <= y0
                && y1 <= i64::from(cy1),
            "{what}: {q:?} leaves the clip {:?}",
            s.clip
        );
    }

    /// The bitmaps are the art the module documents, at both heights: every
    /// frame [`KITTY_W`] palette characters by its art's height, the blink
    /// touching only the eye rows (and closing every eye: no pupil,
    /// catch-light or iris left), the flick only the tail's columns, the
    /// mouth on a row of coat (the review of the cuter kitty: the two-row
    /// cat's white socks stood right under its `ω` and read as two fangs),
    /// and the body band's rows solid from their first pixel to the art's
    /// rear, where the trail ends under it; the tall cat is the two-row cat
    /// stood taller — the same ears, and the same face row for row (its eyes,
    /// nose, blush and mouth) under a taller head. The const shape checks
    /// hold this at build time; this test reads them back, so a check
    /// weakened to pass reads wrong here. NEGATIVE CONTROL: the shape checks
    /// refuse a blink that moves a pixel off the eye rows, a flick that moves
    /// one right of the tail, a ragged row, a hole in the haunch the trail
    /// would show through, the socks under the mouth as first drawn, and a
    /// mouth looked for on a row that has none.
    #[test]
    fn kitty_frames_are_rectangular_and_drawn_in_the_palette() {
        for (art, h) in [(KITTY_SHORT, KITTY_H), (KITTY_TALL, KITTY_TALL_H)] {
            for f in [art.open, art.blink, art.flick] {
                assert_eq!(f.len(), h);
                for row in f.iter() {
                    assert_eq!(row.len(), KITTY_W, "a ragged row {row:?}");
                    assert!(
                        row.bytes().all(|b| KITTY_PALETTE.contains(&b)),
                        "an unknown pixel in {row:?}"
                    );
                }
            }
            for (r, (open, blink)) in art.open.iter().zip(art.blink.iter()).enumerate() {
                assert_eq!(
                    open == blink,
                    !(art.eyes.0..art.eyes.1).contains(&r),
                    "the blink is the eyes alone (row {r})"
                );
                if open != blink {
                    assert!(
                        !blink.contains(['e', 'k', 'w']),
                        "the eyes closed (row {r})"
                    );
                }
            }
            for (open, flick) in art.open.iter().zip(art.flick.iter()) {
                assert_eq!(
                    open[KITTY_TAIL_COLS..],
                    flick[KITTY_TAIL_COLS..],
                    "the flick is the tail alone"
                );
            }
            assert!(art.open.iter().any(|r| r.contains('e')), "the cat has eyes");
            for f in [art.open, art.blink, art.flick] {
                assert!(
                    mouth_on_coat(f, art.mouth_row()),
                    "the mouth stands on coat: {:?} under {:?}",
                    f[art.mouth_row() + 1],
                    f[art.mouth_row()]
                );
            }
            let rear = art.rear();
            for f in [art.open, art.blink, art.flick] {
                for row in &f[art.body.0..art.body.0 + art.body.1] {
                    let first = row.find(|c| c != '.').expect("a body row");
                    assert!(
                        first <= rear && !row[first..rear].contains('.'),
                        "{row:?} meets the trail flush at column {rear}"
                    );
                }
            }
        }
        // The same cat: the tall head's columns hold the two-row head's
        // features — the ears, the eyes, the blush and the nose on the same
        // columns — and its face, a cell row of pixels from the eyes' top, is
        // the two-row cat's row for row, open and blinking.
        let cols_of = |f: &[&str], ch: char| -> std::collections::BTreeSet<usize> {
            f.iter()
                .flat_map(|r| {
                    r.char_indices()
                        .filter(move |&(_, c)| c == ch)
                        .map(|(i, _)| i)
                })
                .collect()
        };
        for ch in ['e', 'k', 'n'] {
            assert_eq!(
                cols_of(KITTY_SHORT.open, ch),
                cols_of(KITTY_TALL.open, ch),
                "{ch:?} on the same columns"
            );
        }
        assert_eq!(KITTY_SHORT.open[0], KITTY_TALL.open[0], "the ears' tips");
        let face =
            |art: KittyArt, f: &'static [&'static str]| &f[art.eyes.0..art.eyes.0 + KITTY_HALF];
        for (short, tall) in [
            (KITTY_SHORT.open, KITTY_TALL.open),
            (KITTY_SHORT.blink, KITTY_TALL.blink),
        ] {
            assert_eq!(
                face(KITTY_SHORT, short),
                face(KITTY_TALL, tall),
                "the same face"
            );
        }
        // NEGATIVE CONTROL: the const checks see a mis-drawn frame.
        let mut stray: Vec<&str> = KITTY_TALL.blink.to_vec();
        stray[0] = "..............o......oo.";
        assert!(!differ_only_in(
            KITTY_TALL.open,
            &stray,
            KITTY_TALL.eyes,
            (0, KITTY_W)
        ));
        let mut wag: Vec<&str> = KITTY_TALL.flick.to_vec();
        wag[1] = "....o........ono....ono.";
        assert!(!differ_only_in(
            KITTY_TALL.open,
            &wag,
            (0, KITTY_TALL_H),
            (0, KITTY_TAIL_COLS)
        ));
        let mut ragged: Vec<&str> = KITTY_TALL.open.to_vec();
        ragged[3] = "oco.........onnoooooonn";
        assert!(!frame_is_well_formed(&ragged, KITTY_TALL_H));
        let rear = KITTY_TALL.rear();
        assert!(trail_meets_flush(KITTY_TALL.open, KITTY_TALL.body, rear));
        let mut holed: Vec<&str> = KITTY_TALL.open.to_vec();
        holed[KITTY_TALL.body.0 + 1] = "o.cccccocckkkkccckkkkcco";
        assert!(!trail_meets_flush(&holed, KITTY_TALL.body, rear));
        let mouth = KITTY_SHORT.mouth_row();
        let mut fangs: Vec<&str> = KITTY_SHORT.open.to_vec();
        fangs[mouth + 1] = "..occoocccooobbooobbo...";
        assert!(!mouth_on_coat(&fangs, mouth), "socks under the mouth");
        assert!(
            !mouth_on_coat(KITTY_SHORT.open, mouth + 1),
            "no mouth there"
        );
        assert!(frame_is_well_formed(KITTY_TALL.open, KITTY_TALL_H));
        assert!(
            !frame_is_well_formed(KITTY_TALL.open, KITTY_H),
            "the wrong height"
        );
    }

    /// The kitty needs whole cells at its square pixel, sized from the band
    /// less the border's stroke, and none when a cell is shorter than its
    /// half plus the stroke.
    #[test]
    fn kitty_cols_rounds_the_art_up_to_whole_cells() {
        assert_eq!(kitty_cols(8, 16), 6, "2 px pixels: 48 px in 8 px cells");
        assert_eq!(kitty_cols(9, 18), 6, "2 px pixels: 48 px in 9 px cells");
        assert_eq!(kitty_cols(16, 33), 6, "4 px pixels: 96 px in 16 px cells");
        // 21 px less a 1 px stroke holds 2 px pixels, not 3.
        assert_eq!(kitty_cols(7, 21), 7, "2 px pixels: 48 px in 7 px cells");
        // 35 px less a 2 px stroke holds 4 px pixels, not 5.
        assert_eq!(kitty_cols(17, 35), 6, "4 px pixels: 96 px in 17 px cells");
        assert_eq!(kitty_cols(8, 14), 3, "1 px pixels: 24 px in 8 px cells");
        assert_eq!(kitty_cols(8, 8), 3, "7 pixel rows under a 1 px stroke");
        assert_eq!(
            kitty_cols(8, 7),
            0,
            "7 px less the stroke cannot hold 7 rows"
        );
        assert_eq!(kitty_cols(8, 6), 0, "a 6 px cell cannot hold 7 pixel rows");
        assert_eq!(kitty_cols(0, 16), 0);
        assert_eq!(kitty_cols(8, 0), 0);
    }

    /// The kitty clears the border's stroke at every cell height that holds
    /// it — the heights where `ch % 7` is under the stroke (14, 21, 28, 29,
    /// 35, 36, 42 to 44 …) among them — at both heights, in a box of the
    /// card's three rows (the line on the banner row: the bottom border
    /// under the third): the two-row kitty with its first pixel row in the
    /// top row's band and its last in the second's, the tall one inside the
    /// three rows, each spanning no more than the [`kitty_cols`] it claims;
    /// and its rim stays inside the stroke too.
    #[test]
    fn the_kitty_clears_the_border_at_every_cell_height() {
        let mut drawn = [0; 2];
        for (k, kitty_rows) in [2u8, 3].into_iter().enumerate() {
            for ch in 1..=96u16 {
                let cw = (ch / 2).max(1);
                let cols = kitty_cols(cw, ch);
                let s = CardScene {
                    cw,
                    ch,
                    origin_x: 4,
                    origin_y: 4,
                    top: 0,
                    row_base: 0,
                    rows: 3,
                    trail: Some((32, 40)),
                    kitty_col: Some(40),
                    kitty_rows,
                    cols: 40 + cols + 1,
                    ..scene()
                };
                let g = Geo::new(&s).expect("a scene");
                let Some(at) = kitty_rect(&g, &s) else {
                    assert_eq!(
                        cols, 0,
                        "{ch} px, {kitty_rows} rows: the kitty fits its columns but was dropped"
                    );
                    continue;
                };
                let (r, p) = (at.rect, at.p);
                let inner = g.inner();
                assert_eq!(
                    r.meet(inner),
                    r,
                    "{ch} px: the kitty {r:?} meets the stroke {inner:?}"
                );
                assert_eq!(at.art.rows(), usize::from(kitty_rows));
                assert!(
                    r.y0 >= g.band(0) && r.y1 <= g.band(i64::from(kitty_rows)),
                    "{ch} px: {r:?}"
                );
                if kitty_rows == 2 {
                    assert_eq!(
                        r.y0 + KITTY_HALF as i64 * p,
                        g.band(1),
                        "{ch} px: split at the boundary"
                    );
                }
                assert!(
                    r.x1 - r.x0 <= i64::from(cols) * i64::from(cw),
                    "{ch} px: wider than its {cols} columns"
                );
                assert_eq!(p, i64::from(kitty_px(ch)));
                let settled = parts(&s, Clock::SETTLED);
                for q in settled.kitty.iter().chain(&settled.rim) {
                    let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                    let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                    assert!(
                        inner.x0 <= x0 && x1 <= inner.x1 && inner.y0 <= y0 && y1 <= inner.y1,
                        "{ch} px: a kitty or rim pixel {q:?} on the stroke"
                    );
                }
                drawn[k] += 1;
            }
        }
        assert!(
            drawn.iter().all(|&n| n >= 80),
            "the sweep must draw the kitty: {drawn:?}"
        );
    }

    /// Idle to zero: after the episode every tick is the same bytes and the
    /// same fingerprint, and the card wants no frames.
    #[test]
    fn the_settled_frame_is_static_and_wants_no_frames() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let first = frame(&mut card, t0 + ms(EPISODE_MS), &s, true);
            assert_ne!(first.2, 0, "a settled card still draws");
            for k in 1..6 {
                let now = t0 + ms(EPISODE_MS + k * 977);
                assert_eq!(
                    frame(&mut card, now, &s, true),
                    first,
                    "settled frame moved at {k}"
                );
                assert_eq!(card.deadline(now), None, "a settled card wants no frames");
            }
        }
        assert_eq!(
            HarnessCard::default().deadline(Instant::now()),
            None,
            "an unarmed card idles"
        );
    }

    /// While an episode runs every frame has a new fingerprint and a deadline
    /// one frame out, and the quads themselves move (for the first five
    /// seconds at every frame; the last second eases onto the settled frame).
    #[test]
    fn episode_frames_move_and_keep_a_deadline() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 11);
            let mut prev: Option<(Vec<GlowQuad>, Vec<GlowQuad>, u64)> = None;
            let mut at = 0;
            while at < EPISODE_MS {
                let now = t0 + ms(at);
                let f = frame(&mut card, now, &s, true);
                assert_eq!(card.deadline(now), Some(now + ms(FRAME_MS)), "{at} ms");
                if let Some(p) = &prev {
                    assert_ne!(p.2, f.2, "the fingerprint held at {at} ms");
                    if at < 5000 {
                        assert!(p.0 != f.0 || p.1 != f.1, "the quads held at {at} ms");
                    }
                }
                prev = Some(f);
                at += FRAME_MS;
            }
            // One last wake for the settled frame, then none.
            let end = t0 + ms(EPISODE_MS);
            assert_eq!(card.deadline(end), Some(end));
            frame(&mut card, end, &s, true);
            assert_eq!(card.deadline(end), None);
        }
    }

    /// Every quad of every frame lies in exactly one row band, tagged with it,
    /// and inside the box.
    #[test]
    fn every_quad_lies_in_one_band_inside_the_box() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 3);
            let mut at = 0;
            while at <= EPISODE_MS + 100 {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                for q in &under {
                    assert_in_band_and_box(q, &s, &format!("under at {at} ms"));
                }
                for q in &over {
                    assert_in_band_and_box(q, &s, &format!("over at {at} ms"));
                }
                at += 17;
            }
        }
    }

    /// AIR MOVES THE BORDER AND NOTHING ELSE: a box with a row of blank pane
    /// over and under it ([`aired`]) stands its border's top edge half a row
    /// up, in the band of the row over the box and tagged with it, and its
    /// bottom edge half a row down, in the band of the row under it; each
    /// side gains one slice in each of those bands (four quads in all), and
    /// the left and right edges stay where they were. The trail, the kitty,
    /// its rim, the field, the sparkles and the burst are the same quads,
    /// settled, riding and seated, at both heights and on both grounds. At
    /// the pane's first row the air over the box is ignored: the border is
    /// the airless one. CONTROL: with no air the border's edges lie on the
    /// boxed rows' bands, and no quad is tagged with a row over or under them.
    #[test]
    fn air_moves_the_border_and_nothing_else() {
        let extent = |quads: &[GlowQuad]| {
            let y0 = quads.iter().map(|q| i64::from(q.y)).min();
            let y1 = quads.iter().map(|q| i64::from(q.y) + i64::from(q.h)).max();
            let x0 = quads.iter().map(|q| i64::from(q.x)).min();
            let x1 = quads.iter().map(|q| i64::from(q.x) + i64::from(q.w)).max();
            (y0.unwrap_or(0), y1.unwrap_or(0), x0, x1)
        };
        let tagged = |quads: &[GlowQuad], row: u16| quads.iter().filter(|q| q.row == row).count();
        for bare in [
            scene(),
            CardScene {
                kitty_rows: 3,
                dark: false,
                palette: CardPalette::Land,
                ..scene()
            },
        ] {
            let g = Geo::new(&bare).expect("a scene");
            let (top, bottom) = (g.band(g.top), g.band(g.top + g.rows));
            let half = g.ch / 2;
            let (over, under) = (
                bare.row_base + bare.top - 1,
                bare.row_base + bare.top + bare.rows,
            );
            for clock in [
                Clock::SETTLED,
                Clock::riding(150),
                Clock::riding(1300),
                Clock::seated(150),
            ] {
                let (a, b) = (parts(&bare, clock), parts(&aired(bare), clock));
                let what = format!("{} rows, {clock:?}", bare.kitty_rows);
                assert!(!a.kitty.is_empty() && !a.trail.is_empty(), "{what}");
                assert_eq!(a.trail, b.trail, "{what}: the trail");
                assert_eq!(a.kitty, b.kitty, "{what}: the kitty");
                assert_eq!(a.rim, b.rim, "{what}: the rim");
                assert_eq!(a.stars, b.stars, "{what}: the field");
                assert_eq!(a.sparkles, b.sparkles, "{what}: the sparkles");
                assert_eq!(a.burst, b.burst, "{what}: the burst");
                let (ya0, ya1, xa0, xa1) = extent(&a.border);
                assert_eq!(
                    (ya0, ya1, tagged(&a.border, over), tagged(&a.border, under)),
                    (top, bottom, 0, 0),
                    "{what}: CONTROL, no air: the border closes on the boxed rows"
                );
                let (yb0, yb1, xb0, xb1) = extent(&b.border);
                assert_eq!((yb0, yb1), (top - half, bottom + half), "{what}: the air");
                assert_eq!((xb0, xb1), (xa0, xa1), "{what}: the sides stay");
                // The top edge's segments and a slice each side over the box,
                // the bottom edge's and a slice each side under it.
                let n = tagged(&a.border, bare.row_base + bare.top);
                assert!(n > 2, "{what}: the top edge");
                assert_eq!(tagged(&b.border, over), n, "{what}: over the box");
                assert_eq!(tagged(&b.border, under), n, "{what}: under the box");
                assert_eq!(b.border.len(), a.border.len() + 4, "{what}: four slices");
                for q in &b.border {
                    assert_in_band_and_box(q, &aired(bare), &what);
                }
            }
        }
        // The pane's first row: no row over it, and no air.
        let first = CardScene { top: 0, ..scene() };
        let lifted = CardScene {
            pad_above: first.ch,
            ..first
        };
        let border = parts(&lifted, Clock::SETTLED).border;
        assert_eq!(border, parts(&first, Clock::SETTLED).border);
        assert_eq!(
            extent(&border).0,
            i64::from(first.origin_y),
            "nothing over band 0"
        );
    }

    /// How many runs of one colour the frame's pixel rows hold: the quads a
    /// kitty drawn a run a quad would cost.
    fn runs_of(frame: &[&str]) -> usize {
        frame
            .iter()
            .map(|r| {
                let b = r.as_bytes();
                (0..b.len())
                    .filter(|&i| b[i] != b'.' && (i == 0 || b[i - 1] != b[i]))
                    .count()
            })
            .sum()
    }

    /// THE KITTY IS ITS ART IN SQUARE PIXELS, at both heights, opaque on both
    /// themes: read back pixel by pixel from the quads a settled tick draws
    /// it is the open frame, and the quads cover exactly its pixels (their
    /// area is the art's, so none overlaps another or reaches past a pixel);
    /// the two-row kitty's halves meet at the row boundary; and it is drawn
    /// in rectangles carried down the rows, a sixth or more under a quad a
    /// run (the face's detail repeats down few rows). Every
    /// other mark is additive light on dark. CONTROL: a quad nudged a pixel
    /// over reads back wrong.
    #[test]
    fn the_kitty_is_opaque_square_pixels_and_dark_marks_are_light() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let at = kitty_rect(&g, &s).expect("a kitty");
            let inks = KittyInk::new(s.coat, s.eye, s.dark);
            let (under, over, _) = settled(&s);
            assert!(
                under.iter().all(|q| q.alpha == 0 && q.alpha2 == 0),
                "dark border and trail are light"
            );
            assert!(
                over.iter().all(|q| q.alpha == 0 || q.alpha == 255),
                "the rim and the stars are light on dark"
            );
            let kitty: Vec<GlowQuad> = over.iter().filter(|q| q.alpha == 255).copied().collect();
            let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
            assert_eq!(
                drawn_kitty(&kitty, &at, inks),
                art(at.art.open),
                "{kitty_rows} rows"
            );
            let area: i64 = kitty.iter().map(|q| i64::from(q.w) * i64::from(q.h)).sum();
            let solid = at
                .art
                .open
                .iter()
                .map(|r| r.bytes().filter(|&b| b != b'.').count())
                .sum::<usize>() as i64;
            assert_eq!(
                area,
                solid * at.p * at.p,
                "{kitty_rows} rows: no overlap, no spill"
            );
            let runs = runs_of(at.art.open);
            assert!(
                kitty.len() * 6 < runs * 5,
                "{kitty_rows} rows: {} quads for {runs} runs",
                kitty.len()
            );
            if kitty_rows == 2 {
                let mid = s.origin_y + i32::from(s.top + 1) * i32::from(s.ch);
                assert!(
                    kitty.iter().all(|q| {
                        let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
                        y1 <= mid || y0 >= mid
                    }) && kitty.iter().any(|q| i32::from(q.y) + i32::from(q.h) == mid),
                    "the halves meet at the boundary"
                );
            }
            let light = CardScene { dark: false, ..s };
            let lit = parts(&light, Clock::SETTLED);
            assert_eq!(lit.kitty.len(), kitty.len(), "{kitty_rows} rows");
            assert!(
                lit.kitty
                    .iter()
                    .chain(&lit.rim)
                    .all(|q| (q.alpha, q.alpha2) == (255, 255)),
                "the kitty and its rim are opaque on light too (print art)"
            );
            // CONTROL: one quad nudged a pixel right reads back otherwise.
            let mut nudged = kitty.clone();
            nudged[0].x += u16::try_from(at.p).expect("a pixel");
            assert_ne!(drawn_kitty(&nudged, &at, inks), art(at.art.open));
        }
    }

    /// The inks a light ground's point marks are laid in: the paper's white
    /// and `palette`'s six star inks ([`CardPalette::star_ink`]).
    fn mark_inks(palette: CardPalette) -> Vec<u32> {
        (0..TRAIL_STOPS.len())
            .map(|k| palette.star_ink(k))
            .chain([WHITE])
            .collect()
    }

    /// On a light theme every quad is source-over, through an episode, and
    /// the card is one printed piece: the border, the trail, the rim and the
    /// kitty are opaque, and a mark that lets the ground through (a star's
    /// taper, a twinkle's dip, a fading burst or sparkle) is exactly one of
    /// the print inks or the paper's white at its opacity — no washed-out
    /// light ink anywhere. CONTROL: the light-ink law's red, laid as a star
    /// was before, is none of them.
    #[test]
    fn a_light_theme_is_source_over_ink_throughout() {
        for s in scenes().into_iter().filter(|s| !s.dark) {
            let inks = mark_inks(s.palette);
            let printed = |c: u32, a: u8| inks.iter().any(|&k| premul_rgb(k, a) == c);
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 5);
            for at in (0..=EPISODE_MS).step_by(30) {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                for q in under.iter().chain(&over) {
                    assert!(
                        q.alpha > 0 && q.alpha2 > 0,
                        "additive on white at {at} ms: {q:?}"
                    );
                    if q.alpha < 255 || q.alpha2 < 255 {
                        assert!(
                            printed(q.color, q.alpha) && printed(q.color2, q.alpha2),
                            "a translucent mark off the print at {at} ms: {q:?}"
                        );
                    }
                }
            }
            let law = InkRole::OverText.ink(spectrum_stop(0));
            assert!(!printed(premul_rgb(law, 176), 176), "CONTROL");
        }
    }

    /// Whether pixel `(x, y)` lies on any of `quads`.
    fn on_any(quads: &[GlowQuad], x: i64, y: i64) -> bool {
        quads.iter().any(|q| {
            let (qx, qy) = (i64::from(q.x), i64::from(q.y));
            (qx..qx + i64::from(q.w)).contains(&x) && (qy..qy + i64::from(q.h)).contains(&y)
        })
    }

    /// The breaches of the one-printed-piece law in one light frame's point
    /// marks (`marks`) over its trail (`trail`): a white pixel off the
    /// trail's ink, or an ink pixel on it. Empty when it holds.
    fn print_breaches(marks: &[GlowQuad], trail: &[GlowQuad]) -> Vec<String> {
        let mut out = Vec::new();
        for q in marks {
            let white = q.color == premul_rgb(WHITE, q.alpha);
            for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
                for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                    if white != on_any(trail, x, y) {
                        out.push(format!("({x}, {y}) white {white}: {q:?}"));
                    }
                }
            }
        }
        out
    }

    /// ONE PRINTED PIECE (§10.6.10): on a light ground, at both heights, both
    /// palettes, through the ride, the burst and the twinkle, and settled,
    /// every pixel of a star, a sparkle or the burst that lies on the
    /// trail's ink is the paper's white (a glint cut out of the print) and
    /// every other is print ink: no ink star ever lies on a stripe, where it
    /// would vanish into its own hue, nor white on the paper. Both kinds are
    /// drawn. NEGATIVE CONTROL: an ink star laid at a trail star's centre
    /// with the region alone for its clip (the old placement) prints ink on
    /// the trail.
    #[test]
    fn light_marks_print_on_the_paper_and_glint_on_the_trail() {
        let (mut glints, mut inked) = (0usize, 0usize);
        for s in scenes()
            .into_iter()
            .filter(|s| !s.dark && s.trail.is_some())
        {
            for t in (0..=EPISODE_MS).step_by(30).map(Some).chain([None]) {
                for ride_ms in [t, None] {
                    let clock = Clock { t_ms: t, ride_ms };
                    let p = parts(&s, clock);
                    let marks: Vec<GlowQuad> = p
                        .stars
                        .iter()
                        .chain(&p.sparkles)
                        .chain(&p.burst)
                        .copied()
                        .collect();
                    let bad = print_breaches(&marks, &p.trail);
                    assert!(
                        bad.is_empty(),
                        "{t:?}/{ride_ms:?}: {:?}",
                        &bad[..bad.len().min(4)]
                    );
                    for q in &marks {
                        if q.color == premul_rgb(WHITE, q.alpha) {
                            glints += 1;
                        } else {
                            inked += 1;
                        }
                    }
                }
            }
        }
        assert!(glints > 0 && inked > 0, "glints {glints}, ink {inked}");
        // CONTROL: the old placement, ink clipped to the region alone.
        let s = CardScene {
            dark: false,
            ..scene()
        };
        let geo = Geo::new(&s).expect("a scene");
        let region = star_region(&geo, &s).expect("a region");
        let p = parts(&s, Clock::SETTLED);
        let (ya, yb) = trail_band(&geo, &s);
        let centre = (geo.col(34), (ya + yb) / 2);
        let mut old = Vec::new();
        let mut sink = Sink {
            geo,
            left: usize::MAX,
        };
        assert!(push_star(
            &mut old,
            &mut sink,
            region,
            centre,
            4,
            STAR_COV,
            false,
            s.palette.star_ink(0),
            &[],
            false
        ));
        assert!(!print_breaches(&old, &p.trail).is_empty(), "CONTROL");
    }

    /// THE LIGHT MARKS LIE ONLY ON CELLS THE HOST READ BLANK: on a light
    /// ground, at both heights, both palettes, through the ride, the burst
    /// and the twinkle, every star, sparkle and burst quad lies in the
    /// card's first two rows between the trail's start and the border's
    /// column (`claude_card.rs`'s `layout` reads `[start, cols)` blank on
    /// both, and column `cols` may hold a glyph). CONTROL: the dark region,
    /// where the pane does not cut the box, runs on into the border's column.
    #[test]
    fn light_marks_lie_only_on_cells_read_blank() {
        let (mut marks_seen, mut dark_past) = (0usize, 0usize);
        for s in scenes().into_iter().filter(|s| !s.dark) {
            let g = Geo::new(&s).expect("a scene");
            let start = match (s.trail, s.kitty_col) {
                (Some((t0, _)), Some(k)) => t0.min(k),
                (Some((t0, _)), None) => t0,
                (None, Some(k)) => k,
                (None, None) => continue,
            };
            let blank = Rect {
                x0: g.col(start),
                y0: g.band(g.top),
                x1: g.col(s.cols),
                y1: g.band(g.top + 2),
            };
            for t in (0..=EPISODE_MS).step_by(30).map(Some).chain([None]) {
                for ride_ms in [t, None] {
                    let p = parts(&s, Clock { t_ms: t, ride_ms });
                    for q in p.stars.iter().chain(&p.sparkles).chain(&p.burst) {
                        marks_seen += 1;
                        let (x, y) = (i64::from(q.x), i64::from(q.y));
                        let (w, h) = (i64::from(q.w), i64::from(q.h));
                        assert!(
                            x >= blank.x0
                                && y >= blank.y0
                                && x + w <= blank.x1
                                && y + h <= blank.y1,
                            "{t:?}/{ride_ms:?}: {q:?} outside {blank:?}"
                        );
                    }
                }
            }
            let dark = CardScene { dark: true, ..s };
            let region = star_region(&g, &dark).expect("a region");
            dark_past += usize::from(region.x1 > blank.x1);
        }
        assert!(marks_seen > 0);
        assert!(dark_past > 0, "CONTROL");
    }

    /// The light field keeps off the trail's edge: at both heights, both
    /// palettes and many seeds, each field star's reach lies wholly on the
    /// band the stripes cover in every segment or wholly clear of the
    /// trail's reach, so none is cut where the print meets the paper.
    /// CONTROL: the dark field, placed without the rule, lies across it.
    #[test]
    fn the_light_field_keeps_off_the_trails_edge() {
        let mut across_dark = 0;
        for s in scenes().into_iter().filter(|s| s.trail.is_some()) {
            let g = Geo::new(&s).expect("a scene");
            let Some(t) = trail_rect(&g, &s, kitty_rect(&g, &s)) else {
                continue;
            };
            for seed in 0..64 {
                let (spots, n) = field_spots(&g, &s, seed, !s.dark);
                for sp in &spots[..n] {
                    let arm = i64::from(star_arm_px(star_arm(g.ch as f32, sp.ratio)));
                    let m = arm + 1;
                    let reach = Rect {
                        x0: sp.x - m,
                        y0: sp.y - m,
                        x1: sp.x + m + 1,
                        y1: sp.y + m + 1,
                    };
                    if s.dark {
                        across_dark += usize::from(across_trail(reach, t));
                    } else {
                        assert!(!across_trail(reach, t), "seed {seed}: {sp:?}");
                    }
                }
            }
        }
        assert!(across_dark > 0, "CONTROL");
    }

    /// THE KEPT LINE INKS ARE THE LAW: for each palette, a table solved on
    /// white gives every step's [`CardPalette::text_at`] there, on white and,
    /// asked for Gruvbox Light's cream, on the cream — a reader asking for
    /// another ground than the kept one gets the law's answer. A light tick
    /// keeps its palette's and ground's inks, and solves again for another
    /// palette or ground; a dark one solves none.
    /// CONTROL: the law differs between the two grounds at some step, so a
    /// table read for the wrong ground would fail.
    #[test]
    fn the_kept_line_inks_are_the_law() {
        let cream = 0x00FB_F1C7;
        let step = |i: usize| i as f32 / LINE_INK_STEPS as f32;
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let mut inks = LineInks::default();
            inks.solve(palette, WHITE);
            for i in 0..=LINE_INK_STEPS {
                for ground in [WHITE, cream] {
                    assert_eq!(
                        inks.at(palette, ground, step(i)),
                        palette.text_at(step(i), ground),
                        "{palette:?} step {i} on {ground:06x}"
                    );
                }
            }
            // Between steps a letter takes the nearer one; out of range clamps.
            let half = 0.4 / LINE_INK_STEPS as f32;
            assert_eq!(
                inks.at(palette, WHITE, step(7) + half),
                inks.at(palette, WHITE, step(7))
            );
            assert_eq!(inks.at(palette, WHITE, -1.0), inks.at(palette, WHITE, 0.0));
            assert_eq!(inks.at(palette, WHITE, 2.0), inks.at(palette, WHITE, 1.0));
            assert_eq!(
                inks.at(palette, WHITE, f32::NAN),
                inks.at(palette, WHITE, 0.0)
            );
            assert!(
                (0..=LINE_INK_STEPS)
                    .any(|i| palette.text_at(step(i), WHITE) != palette.text_at(step(i), cream)),
                "{palette:?}: CONTROL, the grounds' inks differ"
            );
        }
        let t0 = Instant::now();
        let mut light = HarnessCard::default();
        light.arm(t0, 7);
        let page = CardScene {
            dark: false,
            ground: cream,
            palette: CardPalette::Land,
            ..scene()
        };
        light.tick(t0, &page, true, &mut Vec::new(), &mut Vec::new());
        assert_eq!(light.inks.key, Some((CardPalette::Land, cream)));
        // Another palette or ground on the same card solves for it.
        let white = CardScene {
            ground: WHITE,
            palette: CardPalette::Cruise,
            ..page
        };
        light.tick(
            t0 + ms(FRAME_MS),
            &white,
            true,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert_eq!(light.inks.key, Some((CardPalette::Cruise, WHITE)));
        let mut dark = HarnessCard::default();
        dark.arm(t0, 7);
        dark.tick(t0, &scene(), true, &mut Vec::new(), &mut Vec::new());
        assert_eq!(dark.inks.key, None);
    }

    /// `quads` gathered into the stars they make: quads that overlap or
    /// touch are one star's (its body, nucleus and points meet edge to edge),
    /// each star's bounding box.
    fn star_boxes(quads: &[GlowQuad]) -> Vec<Rect> {
        let rect = |q: &GlowQuad| Rect {
            x0: i64::from(q.x),
            y0: i64::from(q.y),
            x1: i64::from(q.x) + i64::from(q.w),
            y1: i64::from(q.y) + i64::from(q.h),
        };
        let mut boxes: Vec<Rect> = Vec::new();
        for r in quads.iter().map(rect) {
            let grown = Rect {
                x0: r.x0 - 1,
                y0: r.y0 - 1,
                x1: r.x1 + 1,
                y1: r.y1 + 1,
            };
            let mut merged = r;
            boxes.retain(|b| {
                if b.meet(grown).is_empty() {
                    true
                } else {
                    merged = Rect {
                        x0: merged.x0.min(b.x0),
                        y0: merged.y0.min(b.y0),
                        x1: merged.x1.max(b.x1),
                        y1: merged.y1.max(b.y1),
                    };
                    false
                }
            });
            boxes.push(merged);
        }
        boxes
    }

    /// THE RIDE'S SPARKLES LIE WHOLE ON THE TRAIL'S INK on a light ground: at
    /// every scene's cell turned light, both palettes and heights, through
    /// the ride, each sparkle shed on the fresh trail is the paper's white
    /// (a glint cut out of the print) and whole — its box as tall as it is
    /// wide, never cut at the band's edge into half a star — wherever the
    /// band has room for it. NEGATIVE CONTROL: a sparkle laid where the shed
    /// puts it at the band's top edge, unclamped, wears two tones there (the
    /// paper's white on the print, ink on the paper), where the law wants one
    /// white glint.
    #[test]
    fn light_sparkles_lie_whole_on_the_trails_ink() {
        let mut seen = 0usize;
        for s in scenes()
            .into_iter()
            .filter(|s| s.trail.is_some())
            .map(|s| CardScene { dark: false, ..s })
        {
            for t in (0..RIDE_MS).step_by(20) {
                let p = parts(
                    &s,
                    Clock {
                        t_ms: Some(t),
                        ride_ms: Some(t),
                    },
                );
                for b in star_boxes(&p.sparkles) {
                    seen += 1;
                    assert_eq!(
                        b.x1 - b.x0,
                        b.y1 - b.y0,
                        "{}x{} at {t} ms: a cut sparkle {b:?}",
                        s.cw,
                        s.ch
                    );
                }
                assert!(
                    p.sparkles
                        .iter()
                        .all(|q| q.color == premul_rgb(WHITE, q.alpha)),
                    "{}x{} at {t} ms: a sparkle off the trail's ink",
                    s.cw,
                    s.ch
                );
            }
        }
        assert!(seen > 0);
        // CONTROL: the unclamped placement, on the band's top edge.
        let s = CardScene {
            dark: false,
            ..scene()
        };
        let geo = Geo::new(&s).expect("a scene");
        let clock = Clock {
            t_ms: Some(300),
            ride_ms: Some(300),
        };
        let cx = Emit::new(&s, geo, clock, 7);
        let region = star_region(&geo, &s).expect("a region");
        let lay = cx.trail_lay().expect("a trail");
        let x = (lay.tx0 + lay.drawn) / 2;
        let trail = cx.trail_ink();
        // On the band's top edge as the wave has it here.
        let edge = trail
            .covers()
            .iter()
            .find(|c| (c.x0..c.x1).contains(&x))
            .expect("a segment");
        let centre = (x, edge.y0);
        let mut two = Vec::new();
        let mut sink = Sink {
            geo,
            left: usize::MAX,
        };
        let arm = i64::from(star_arm_px(star_arm(
            geo.ch as f32,
            STAR_ARM_FINE * STAR_ARM_INK,
        )));
        assert!(push_star(
            &mut two,
            &mut sink,
            region,
            centre,
            arm,
            SHED_COV,
            false,
            s.palette.star_ink(0),
            trail.covers(),
            false
        ));
        assert!(
            two.iter().any(|q| q.color == premul_rgb(WHITE, q.alpha))
                && two.iter().any(|q| q.color != premul_rgb(WHITE, q.alpha)),
            "CONTROL: two tones {two:?}"
        );
    }

    /// A LIGHT STAR'S NUCLEUS AND GLINTS PRINT AT ITS STRENGTH: on a light
    /// ground a settled star lays its nucleus (whose corners stand out past
    /// the arms' waist) opaque with its body, and a glinting star (a hero, a
    /// burst star) its four diagonal dots in its own print ink at the body's
    /// opacity — where on dark the nucleus is a share of the light the
    /// crossing adds to and the dots a third of it. NEGATIVE CONTROLS: the
    /// dots as laid before, a third of the star's strength, and the nucleus
    /// at the additive share, let the paper through (a pale pink or tan
    /// halo, paler corners).
    #[test]
    fn a_light_stars_nucleus_and_glints_print_at_its_strength() {
        let s = CardScene {
            dark: false,
            ..scene()
        };
        let geo = Geo::new(&s).expect("a scene");
        let region = star_region(&geo, &s).expect("a region");
        let centre = (region.x0 + 3 * geo.cw, region.y0 + geo.ch);
        let arm = 8;
        let d = ((arm as f32 * STAR_GLINT).round() as i64).max(1);
        let cov = STAR_COV * STAR_STEADY_ENV;
        for k in 0..TRAIL_STOPS.len() {
            let ink_rgb = s.palette.star_ink(k);
            let mut out = Vec::new();
            let mut sink = Sink {
                geo,
                left: usize::MAX,
            };
            assert!(push_star(
                &mut out,
                &mut sink,
                region,
                centre,
                arm,
                cov,
                true,
                ink_rgb,
                &[],
                false
            ));
            // Every quad that touches the nucleus is opaque ink.
            let arm32 = i32::try_from(arm).expect("a small arm");
            let (c, t) = (
                i64::from(star_core_px(arm32)),
                i64::from(star_waist_px(arm32)),
            );
            assert!(c > t, "the nucleus stands out past the waist");
            let cc = (c - 1) / 2;
            let core = Rect {
                x0: centre.0 - cc,
                y0: centre.1 - cc,
                x1: centre.0 - cc + c,
                y1: centre.1 - cc + c,
            };
            let touching: Vec<&GlowQuad> = out
                .iter()
                .filter(|q| {
                    !core
                        .meet(Rect {
                            x0: i64::from(q.x),
                            y0: i64::from(q.y),
                            x1: i64::from(q.x) + i64::from(q.w),
                            y1: i64::from(q.y) + i64::from(q.h),
                        })
                        .is_empty()
                })
                .collect();
            assert!(!touching.is_empty());
            for q in touching {
                assert_eq!(
                    (q.color, q.alpha),
                    (premul_rgb(ink_rgb, 255), 255),
                    "ink {k}: the nucleus {q:?}"
                );
            }
            for (gx, gy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
                let (x, y) = (centre.0 + gx, centre.1 + gy);
                let dot = out
                    .iter()
                    .find(|q| (i64::from(q.x), i64::from(q.y), q.w, q.h) == (x, y, 1, 1))
                    .unwrap_or_else(|| panic!("ink {k}: no glint at ({x}, {y})"));
                assert_eq!(
                    (dot.color, dot.alpha),
                    (premul_rgb(ink_rgb, 255), 255),
                    "ink {k}: {dot:?}"
                );
            }
            // CONTROL: a third of the star's strength, as before.
            let before = ink(
                ink_rgb,
                (cov * STAR_GLINT_COV).floor() * 255.0 / STAR_PRINT_FULL,
            );
            assert!(before.alpha < 128, "CONTROL {before:?}");
            let additive = ink(
                ink_rgb,
                (cov * STAR_CORE_ADD).floor() * 255.0 / STAR_PRINT_FULL,
            );
            assert!(additive.alpha < 128, "CONTROL {additive:?}");
        }
    }

    /// The pixels `quads` cover.
    fn pixels(quads: &[GlowQuad]) -> std::collections::BTreeSet<(i64, i64)> {
        let mut px = std::collections::BTreeSet::new();
        for q in quads {
            for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
                for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                    px.insert((x, y));
                }
            }
        }
        px
    }

    /// The light burst's placement before (the CONTROL of
    /// [`light_burst_stars_lie_whole`]): a star centred at `(x, y)` in one
    /// tone, clipped to the band where its centre lies on the trail's reach,
    /// else to the side of the reach its centre is on that cuts least.
    fn one_sided(cx: &Emit, region: Rect, (x, y): (i64, i64)) -> Rect {
        let Some(lay) = cx.trail_lay() else {
            return region;
        };
        let w = cx.wave(&lay, 0).abs();
        let t = Rect {
            x0: lay.tx0,
            y0: lay.ya - w,
            x1: lay.drawn,
            y1: lay.yb + w,
        };
        if t.contains(x, y) {
            return region.meet(Rect {
                y0: lay.ya + w,
                y1: lay.yb - w,
                ..t
            });
        }
        [
            (t.x0 - 1 - x, Rect { x1: t.x0, ..region }),
            (x - t.x1, Rect { x0: t.x1, ..region }),
            (t.y0 - 1 - y, Rect { y1: t.y0, ..region }),
            (y - t.y1, Rect { y0: t.y1, ..region }),
        ]
        .into_iter()
        .filter(|(d, _)| *d >= 0)
        .max_by_key(|(d, _)| *d)
        .map_or(region, |(_, r)| region.meet(r))
    }

    /// THE BURST'S STARS LIE WHOLE (§10.6.10): on a light ground, in every
    /// scene with a trail, both palettes and heights, five seeds, on the
    /// ride and re-armed, through the burst, the burst covers exactly the
    /// pixels its stars reach in the region, each star laid alone in one
    /// tone: a star that crosses the trail's edge, or reaches past its end
    /// onto the kitty, is whole, the paper's white on the print and its ink
    /// off it ([`light_marks_print_on_the_paper_and_glint_on_the_trail`]
    /// holds the tones), never cut where print meets paper. NEGATIVE
    /// CONTROL: the placement before, each star clipped to the one side of
    /// the trail's edge its centre is on ([`one_sided`]), loses pixels in
    /// most burst frames.
    #[test]
    fn light_burst_stars_lie_whole() {
        let (mut frames, mut crossed, mut cut_before) = (0usize, 0usize, 0usize);
        for s in scenes()
            .into_iter()
            .filter(|s| !s.dark && s.trail.is_some())
        {
            let geo = Geo::new(&s).expect("a scene");
            for seed in [3u64, 7, 11, 19, 23] {
                for ride in [true, false] {
                    for t in (0..RIDE_MS + BURST_MS).step_by(20) {
                        let clock = Clock {
                            t_ms: Some(t),
                            ride_ms: ride.then_some(t),
                        };
                        let cx = Emit::new(&s, geo, clock, seed);
                        let Some(ring) = cx.burst_ring() else {
                            continue;
                        };
                        frames += 1;
                        let mut sink = Sink {
                            geo,
                            left: usize::MAX,
                        };
                        let mut burst = Vec::new();
                        assert!(cx.burst(&mut sink, &mut burst));
                        let (mut whole, mut before) = (Vec::new(), Vec::new());
                        for (k, &c) in ring.centres.iter().enumerate() {
                            let rgb = s.palette.star_ink(k);
                            for (out, clip) in [
                                (&mut whole, ring.region),
                                (&mut before, one_sided(&cx, ring.region, c)),
                            ] {
                                assert!(push_star(
                                    out,
                                    &mut sink,
                                    clip,
                                    c,
                                    ring.arm,
                                    ring.cov,
                                    true,
                                    rgb,
                                    &[],
                                    false
                                ));
                            }
                        }
                        let want = pixels(&whole);
                        let what = format!("{}x{} seed {seed} ride {ride} {t} ms", s.cw, s.ch);
                        assert_eq!(pixels(&burst), want, "{what}");
                        let tones = burst
                            .iter()
                            .map(|q| q.color == premul_rgb(WHITE, q.alpha))
                            .collect::<std::collections::BTreeSet<bool>>();
                        crossed += usize::from(tones.len() == 2);
                        cut_before += usize::from(pixels(&before) != want);
                    }
                }
            }
        }
        assert!(
            frames > 0 && crossed > 0,
            "{frames} frames, {crossed} in two tones"
        );
        assert!(cut_before * 2 > frames, "CONTROL: {cut_before} of {frames}");
    }

    /// THE LIGHT SWEEP ONLY DEEPENS A SHADE: through a light episode, every
    /// letter of the line keeps at least 0.69 of the value (its brightest
    /// channel) of its unswept ink — the paper law's ink at its place — on
    /// the brightest and the dimmest light built-in, both palettes; at the
    /// dark ground's gain a vivid letter would pass through a near-black
    /// brown. The sweep does deepen: its darkest letter falls under 0.8.
    /// NEGATIVE CONTROL: the dark ground's gain takes a letter under 0.69.
    #[test]
    fn the_light_sweep_only_deepens_a_shade() {
        let grounds = light_builtins();
        let lum = |g: &&(&str, u32)| relative_luminance(g.1);
        let brightest = grounds.iter().max_by(|a, b| lum(a).total_cmp(&lum(b)));
        let dimmest = grounds.iter().min_by(|a, b| lum(a).total_cmp(&lum(b)));
        let value = |rgb: u32| (rgb >> 16).max((rgb >> 8) & 0xff).max(rgb & 0xff) as f32;
        for &(name, g) in brightest.into_iter().chain(dimmest) {
            for palette in [CardPalette::Cruise, CardPalette::Land] {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                let page = CardScene {
                    palette,
                    dark: false,
                    ground: g,
                    ..scene()
                };
                card.tick(t0, &page, false, &mut Vec::new(), &mut Vec::new());
                card.arm(t0, 9);
                let len = 80u16;
                let mut least = f32::MAX;
                for at in (0..=EPISODE_MS).step_by(10) {
                    let now = t0 + ms(at);
                    let Some(run) = card.running(now) else {
                        continue;
                    };
                    let clock = Clock {
                        t_ms: Some(run),
                        ride_ms: None,
                    };
                    let phi = clock.drift(LINE_DRIFT);
                    for (i, c) in card.line_ink(now, 0, 0, len, g, true).iter().enumerate() {
                        let rgb = (u32::from(c.color[0]) << 16)
                            | (u32::from(c.color[1]) << 8)
                            | u32::from(c.color[2]);
                        let x = i as f32 / f32::from(len - 1);
                        let plain = card.inks.at(palette, g, tri(x + phi));
                        least = least.min(value(rgb) / value(plain));
                    }
                }
                assert!(
                    (0.69..0.8).contains(&least),
                    "{name} {palette:?}: the darkest swept letter keeps {least:.3}"
                );
            }
        }
        let red = PRINT_CRUISE[0];
        let mud = mix_rgb(red, 0, LINE_SWEEP_GAIN);
        assert!(value(mud) / value(red) < 0.69, "CONTROL");
    }

    /// THE KEPT LINE INKS STEP UNSEEN: neighbouring steps of the light
    /// line's ink table ([`LineInks`]) are at most 0.02 apart in OKLab, under
    /// the eye's least step, for both palettes on the brightest and the
    /// dimmest light built-in, so a letter on the nearest step reads as its
    /// own place. NEGATIVE CONTROL: a table a sixteenth as fine steps
    /// visibly.
    #[test]
    fn the_kept_line_inks_step_unseen() {
        let grounds = light_builtins();
        let lum = |g: &&(&str, u32)| relative_luminance(g.1);
        let brightest = grounds.iter().max_by(|a, b| lum(a).total_cmp(&lum(b)));
        let dimmest = grounds.iter().min_by(|a, b| lum(a).total_cmp(&lum(b)));
        let gap = |palette: CardPalette, g: u32, steps: usize| {
            (0..steps)
                .map(|i| {
                    let at = |i: usize| palette.text_at(i as f32 / steps as f32, g);
                    crate::color_math::oklab_distance(at(i), at(i + 1))
                })
                .fold(0.0f32, f32::max)
        };
        for &(name, g) in brightest.into_iter().chain(dimmest) {
            for palette in [CardPalette::Cruise, CardPalette::Land] {
                let mut inks = LineInks::default();
                inks.solve(palette, g);
                let widest = (0..LINE_INK_STEPS)
                    .map(|i| crate::color_math::oklab_distance(inks.ink[i], inks.ink[i + 1]))
                    .fold(0.0f32, f32::max);
                assert!(widest <= 0.02, "{name} {palette:?}: a step of {widest:.4}");
                assert!(
                    gap(palette, g, LINE_INK_STEPS / 16) > 0.02,
                    "{name} {palette:?}: CONTROL"
                );
            }
        }
    }

    /// The light grounds of every built-in theme.
    fn light_builtins() -> Vec<(&'static str, u32)> {
        use aterm_types::scheme::{Appearance, builtin, builtin_names};
        builtin_names()
            .into_iter()
            .filter_map(|n| Some((n, builtin(n)?)))
            .filter(|(_, s)| s.appearance == Appearance::Light)
            .map(|(n, s)| {
                let c = s.background;
                (
                    n,
                    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
                )
            })
            .collect()
    }

    /// The settled light line's letters, both palettes, on `ground`, as
    /// `0x00RRGGBB`: read from the inks a light tick on that ground kept.
    fn light_letters(palette: CardPalette, ground: u32) -> Vec<u32> {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.tick(
            t0,
            &CardScene {
                palette,
                dark: false,
                ground,
                ..scene()
            },
            false,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        card.line_ink(t0, 0, 0, 80, ground, false)
            .iter()
            .map(|c| {
                (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
            })
            .collect()
    }

    /// THE LINE IS THE PRINT AT TEXT WEIGHT (§10.6.10): on every light
    /// built-in, both palettes, each settled letter is a pure hue (one
    /// channel at zero: no white, no grey mixed in), reads at 4.5:1 or more
    /// against the ground, and is no deeper than that needs — a hue at full
    /// value, or within a few hundredths of the bar. NEGATIVE CONTROL: the
    /// light-ink law the line wore before (`InkRole::OverText`) is far
    /// deeper than the text needs on the brightest light built-in.
    #[test]
    fn the_light_line_is_the_print_at_text_weight() {
        let grounds = light_builtins();
        assert!(grounds.len() >= 7, "{grounds:?}");
        for &(name, g) in &grounds {
            for palette in [CardPalette::Cruise, CardPalette::Land] {
                for rgb in light_letters(palette, g) {
                    let ch = [rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff];
                    let c = contrast(rgb, g);
                    assert!(
                        ch.contains(&0),
                        "{name} {palette:?}: {rgb:06x} is not a pure hue"
                    );
                    assert!(c >= 4.5, "{name} {palette:?}: {rgb:06x} at {c:.2}:1");
                    assert!(
                        ch.contains(&0xff) || c < 4.6,
                        "{name} {palette:?}: {rgb:06x} is deeper than the text needs, {c:.2}:1"
                    );
                }
            }
        }
        let white = 0x00FF_FFFF;
        for &print in &PRINT_CRUISE[..4] {
            let law = InkRole::OverText.ink(print);
            assert!(contrast(law, white) > 6.0, "CONTROL {law:06x}");
        }
    }

    /// THE LINE READS ON EVERY LIGHT GROUND: through an episode and
    /// settled, both palettes, every letter at 4.5:1 or more against its
    /// ground — every light built-in, and a light ground dimmer than any of
    /// them, which the law reads. NEGATIVE CONTROL: the letters solved for
    /// white paper, laid on that dim ground, fall under AA.
    #[test]
    fn the_light_line_reads_on_every_light_ground() {
        let dim = 0x00C8_C8C8;
        assert!(!dark_ground(dim));
        let mut grounds: Vec<u32> = light_builtins().into_iter().map(|(_, g)| g).collect();
        grounds.push(dim);
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            for &g in &grounds {
                // The card ticked on the ground, as the host does, so its
                // kept inks are read.
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.tick(
                    t0,
                    &CardScene {
                        palette,
                        dark: false,
                        ground: g,
                        ..scene()
                    },
                    false,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                card.arm(t0, 9);
                for at in (0..=EPISODE_MS + 100).step_by(50) {
                    for c in card.line_ink(t0 + ms(at), 0, 0, 80, g, true) {
                        let rgb = (u32::from(c.color[0]) << 16)
                            | (u32::from(c.color[1]) << 8)
                            | u32::from(c.color[2]);
                        assert!(
                            contrast(rgb, g) >= 4.5,
                            "{palette:?} {rgb:06x} on {g:06x} at {at} ms: {:.2}:1",
                            contrast(rgb, g)
                        );
                    }
                }
            }
            // CONTROL: white paper's letters on the dim ground.
            let worst = light_letters(palette, 0x00FF_FFFF)
                .into_iter()
                .map(|rgb| contrast(rgb, dim))
                .fold(f32::MAX, f32::min);
            assert!(worst < 4.5, "CONTROL {palette:?}: {worst:.2}");
        }
    }

    /// Serious mode keeps a static border and nothing else.
    #[test]
    fn serious_mode_is_a_static_border() {
        for s in scenes() {
            let s = CardScene { serious: true, ..s };
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 9);
            let first = frame(&mut card, t0 + ms(40), &s, true);
            assert_eq!(
                card.deadline(t0 + ms(40)),
                None,
                "serious mode ends the episode"
            );
            assert!(first.1.is_empty(), "no kitty, no stars");
            let t = i64::from((s.ch / 14).max(1));
            let cw = i64::from(s.cw);
            let bx0 = (i64::from(s.origin_x) - i64::from(s.pad_left).min(cw / 2)).max(0);
            let bx1 = i64::from(s.origin_x) + (i64::from(s.cols) + 1) * cw;
            let by0 = i64::from(s.origin_y) + i64::from(s.top) * i64::from(s.ch);
            let by1 = by0 + i64::from(s.rows) * i64::from(s.ch);
            for q in &first.0 {
                let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                let on_rim = y1 <= by0 + t || y0 >= by1 - t || x1 <= bx0 + t || x0 >= bx1 - t;
                assert!(on_rim, "a serious quad off the rim: {q:?}");
            }
            assert!(!first.0.is_empty(), "the border stays");
            assert_eq!(frame(&mut card, t0 + ms(900), &s, true), first, "static");
        }
    }

    /// Reduced motion (or an unfocused window) shows the settled frame from
    /// the first tick: no burst, no head, no twinkle, and no deadline, for
    /// as long as motion is withheld.
    #[test]
    fn reduced_motion_is_the_settled_frame_from_the_start() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            assert!(
                card.deadline(t0).is_some(),
                "an armed card asks for its frame"
            );
            let still = frame(&mut card, t0 + ms(30), &s, false);
            assert_eq!(still, settled(&s), "animate=false is the settled frame");
            assert_eq!(card.deadline(t0 + ms(30)), None);
            for at in [400, 2000, EPISODE_MS + 500] {
                assert_eq!(frame(&mut card, t0 + ms(at), &s, false), still, "{at} ms");
                assert_eq!(card.deadline(t0 + ms(at)), None, "{at} ms");
            }
        }
    }

    /// An episode held back before it was ever drawn animating is OWED, not
    /// thrown away: the load-shed latch (or reduced motion, an unfocused
    /// window) forcing `animate == false` just as the card is first found
    /// delays the show. The first tick that may animate plays it from that
    /// tick — the burst flies, a deadline is wanted — even when the held-back
    /// stretch outlasted the episode; it then settles a whole episode later.
    /// The held-back ticks put the settled frame on the glass, so the kitty
    /// plays the owed episode in its seat ([`Ride`]): no ride.
    #[test]
    fn an_episode_held_back_before_it_played_is_owed() {
        for held in [2000, EPISODE_MS + 3000] {
            let s = scene();
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut at = 0;
            while at <= held {
                let still = frame(&mut card, t0 + ms(at), &s, false);
                assert_eq!(still, settled(&s), "held {held}: {at} ms");
                at += 100;
            }
            let go = t0 + ms(held + 50);
            let (_, first, fp) = frame(&mut card, go, &s, true);
            assert_eq!(card.deadline(go), Some(go + ms(FRAME_MS)), "held {held}");
            let (_, rest, rest_fp) = settled(&s);
            assert_ne!(fp, rest_fp, "held {held}: the episode plays");
            assert_eq!(
                kitty_left(&first),
                kitty_left(&rest),
                "held {held}: the kitty stays in the seat the glass showed"
            );
            assert!(first.len() > rest.len(), "held {held}: the burst flies");
            // The same frame an episode armed at `go` on a card shown at
            // rest draws at its start.
            let mut shown = shown_at_rest(&s, go);
            shown.arm(go, 7);
            assert_eq!(frame(&mut shown, go, &s, true).1, first, "held {held}");
            // A whole episode from `go`, not from the arm.
            let late = go + ms(EPISODE_MS - FRAME_MS);
            assert_ne!(frame(&mut card, late, &s, true).2, rest_fp, "held {held}");
            let done = go + ms(EPISODE_MS);
            assert_eq!(frame(&mut card, done, &s, true).2, rest_fp, "held {held}");
        }
    }

    /// An episode already drawn animating keeps its clock while motion is
    /// withheld (the settled frame, no deadline) and resumes on that clock
    /// when motion comes back inside it; past its end it is simply settled.
    /// It is not owed again.
    #[test]
    fn a_playing_episode_held_back_keeps_its_clock() {
        let s = scene();
        let t0 = Instant::now();
        let (_, rest, rest_fp) = settled(&s);
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let held = frame(&mut card, t0 + ms(1000), &s, false);
        assert_eq!(held.2, rest_fp, "held back: the settled frame");
        assert_eq!(card.deadline(t0 + ms(1000)), None);
        // The settled frame seated the kitty on the glass: the twin plays
        // seated too ([`Ride`]).
        let mut twin = shown_at_rest(&s, t0);
        twin.arm(t0, 7);
        assert_eq!(
            frame(&mut card, t0 + ms(2000), &s, true),
            frame(&mut twin, t0 + ms(2000), &s, true),
            "resumed on the arm's clock"
        );
        frame(&mut card, t0 + ms(2100), &s, false);
        let (_, after, fp) = frame(&mut card, t0 + ms(EPISODE_MS + 100), &s, true);
        assert_eq!((after, fp), (rest, rest_fp), "past its end it is settled");
        assert_eq!(card.deadline(t0 + ms(EPISODE_MS + 100)), None);
    }

    /// The deadline is not a function of `now` alone: once the episode runs
    /// out after a tick that drew one of its frames, the card asks for
    /// `now`, so the settled frame is drawn promptly; the tick that draws it
    /// ends the asking.
    #[test]
    fn the_settled_frame_is_asked_for_until_it_is_drawn() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let last = t0 + ms(EPISODE_MS - 10);
        frame(&mut card, last, &s, true);
        assert_eq!(card.deadline(last), Some(last + ms(FRAME_MS)));
        let past = t0 + ms(EPISODE_MS + 250);
        assert_eq!(card.deadline(past), Some(past), "the settled frame is owed");
        assert_eq!(card.deadline(past), Some(past), "asked until drawn");
        let (_, _, fp) = frame(&mut card, past, &s, true);
        assert_eq!(fp, settled(&s).2);
        assert_eq!(card.deadline(past), None, "drawn: idle at zero");
        assert_eq!(card.deadline(past + ms(5000)), None);
        // A held-back tick draws the settled frame itself: nothing is owed.
        let mut held = HarnessCard::default();
        held.arm(t0, 7);
        frame(&mut held, t0, &s, true);
        frame(&mut held, t0 + ms(100), &s, false);
        assert_eq!(held.deadline(past), None);
    }

    /// A host that stops ticking after the last episode frame (the frame
    /// scrolled back, the card's version no longer the process's, the pane
    /// hidden) but asks for the deadline every loop is asked to wake at
    /// once only until [`SETTLE_GRACE_MS`] past the episode's end, then let
    /// idle: never a spin for as long as the card goes unticked. A re-armed
    /// card that is never ticked asks for frames while its episode would
    /// run, then nothing — not the settled frame of the episode before it.
    #[test]
    fn an_unticked_card_stops_asking_after_the_grace() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let mut at = 0;
        while at < EPISODE_MS {
            frame(&mut card, t0 + ms(at), &s, true);
            at += FRAME_MS;
        }
        let end = t0 + ms(EPISODE_MS);
        assert_eq!(card.deadline(end), Some(end), "the settled frame is owed");
        // The host asks every millisecond for ten seconds and never ticks.
        let asked: Vec<u64> = (0..10_000)
            .filter(|&k| card.deadline(end + ms(k)).is_some())
            .collect();
        assert_eq!(
            asked,
            (0..SETTLE_GRACE_MS).collect::<Vec<_>>(),
            "asked only inside the grace"
        );
        let after = end + ms(SETTLE_GRACE_MS);
        assert_eq!(card.deadline(after), None);
        // The tick that comes late still draws the settled frame.
        let late = end + ms(60_000);
        assert_eq!(frame(&mut card, late, &s, true).2, settled(&s).2);
        assert_eq!(card.deadline(late), None);

        // Re-armed after an episode frame, then never ticked.
        let mut rearmed = HarnessCard::default();
        rearmed.arm(t0, 7);
        frame(&mut rearmed, t0, &s, true);
        let t1 = t0 + ms(1000);
        rearmed.arm(t1, 7);
        assert_eq!(rearmed.deadline(t1), Some(t1 + ms(FRAME_MS)));
        let gone = t1 + ms(EPISODE_MS);
        assert_eq!(rearmed.deadline(gone), None, "no frame of its own drawn");
    }

    /// A tick with nothing to draw — an inverted or empty clip, no columns,
    /// no rows, no cell size: a pane mid-layout — draws nothing and holds
    /// the episode back like `animate == false`: an episode not yet drawn is
    /// owed (no deadline meanwhile) and plays whole from the first tick that
    /// draws, even past the episode's length; a playing one keeps its clock.
    #[test]
    fn nothing_to_draw_holds_the_episode_back() {
        let s = scene();
        let (_, rest, rest_fp) = settled(&s);
        for (what, blank) in [
            (
                "an inverted clip",
                CardScene {
                    clip: (4000, 0, 0, 4000),
                    ..s
                },
            ),
            (
                "an empty clip",
                CardScene {
                    clip: (0, 0, 0, 0),
                    ..s
                },
            ),
            ("no columns", CardScene { cols: 0, ..s }),
            ("no rows", CardScene { rows: 0, ..s }),
            ("no cell size", CardScene { cw: 0, ..s }),
        ] {
            for later in [2000, EPISODE_MS + 1000] {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.arm(t0, 7);
                for at in [0, 500, 1000] {
                    let now = t0 + ms(at);
                    assert_eq!(
                        frame(&mut card, now, &blank, true),
                        (Vec::new(), Vec::new(), 0),
                        "{what}: {at} ms"
                    );
                    assert_eq!(card.deadline(now), None, "{what}: owed, no wake");
                }
                let go = t0 + ms(later);
                let drawn = frame(&mut card, go, &s, true);
                let mut fresh = HarnessCard::default();
                fresh.arm(go, 7);
                assert_eq!(drawn, frame(&mut fresh, go, &s, true), "{what}: {later}");
                assert_eq!(
                    kitty_left(&drawn.1),
                    Some(ride_from(&s)),
                    "{what}: {later}: the ride starts"
                );
                assert_ne!(kitty_left(&drawn.1), kitty_left(&rest), "{what}: {later}");
                assert_eq!(card.deadline(go), Some(go + ms(FRAME_MS)), "{what}");
            }
            // A playing episode keeps its clock across a tick with nothing
            // to draw, and resumes on it.
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            assert_eq!(frame(&mut card, t0 + ms(1000), &blank, true).2, 0, "{what}");
            assert_eq!(card.deadline(t0 + ms(1000)), None, "{what}");
            let mut twin = HarnessCard::default();
            twin.arm(t0, 7);
            assert_eq!(
                frame(&mut card, t0 + ms(2000), &s, true),
                frame(&mut twin, t0 + ms(2000), &s, true),
                "{what}: resumed on the arm's clock"
            );
            let done = t0 + ms(EPISODE_MS + 100);
            assert_eq!(frame(&mut card, done, &s, true).2, rest_fp, "{what}");
        }
    }

    /// An owed episode's debt lapses [`OWED_MAX_MS`] after the arm: a tick
    /// that may animate before then plays it, one at or after it draws the
    /// settled frame and wants no frames, so the show never plays minutes
    /// later on an unrelated redraw. Held back by `animate == false` or by
    /// nothing to draw alike; a fresh arm owes a fresh episode.
    #[test]
    fn an_owed_episode_lapses_after_its_dwell() {
        let s = scene();
        let (_, _, rest_fp) = settled(&s);
        let blank = CardScene { cols: 0, ..s };
        for (held_by, animate, by) in [("reduced", false, &s), ("nothing to draw", true, &blank)] {
            for (at, plays) in [
                (OWED_MAX_MS - 1, true),
                (OWED_MAX_MS, false),
                (OWED_MAX_MS + 3_600_000, false),
            ] {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.arm(t0, 7);
                frame(&mut card, t0 + ms(10), by, animate);
                let now = t0 + ms(at);
                let (_, _, fp) = frame(&mut card, now, &s, true);
                let what = format!("{held_by}: paid at {at} ms");
                assert_eq!(fp != rest_fp, plays, "{what}");
                let want = plays.then(|| now + ms(FRAME_MS));
                assert_eq!(card.deadline(now), want, "{what}");
                if !plays {
                    // Lapsed for good: a later tick is settled too…
                    let after = now + ms(1000);
                    assert_eq!(frame(&mut card, after, &s, true).2, rest_fp, "{what}");
                    // …until an arm owes a fresh episode.
                    card.arm(after, 7);
                    assert_ne!(frame(&mut card, after, &s, true).2, rest_fp, "{what}");
                }
            }
        }
    }

    /// AN EPISODE OWED THROUGH THE LATCH'S LONGEST SHED STILL PLAYS (the
    /// review of d209dd683): the GUI's load-shed latch holds Reduced at
    /// least its dwell — 30 s at its ceiling (`PERF_SHED_DWELL_MAX`,
    /// `app_render.rs`) — and restores only on the three qualifying frames
    /// past it, so the first tick that may animate after a ceiling-long shed
    /// comes a few frames after 30 s from the arm, and pays the debt then.
    /// (Past [`OWED_MAX_MS`] it lapses: `an_owed_episode_lapses_after_its_dwell`.)
    #[test]
    fn an_episode_owed_through_the_latchs_longest_shed_still_plays() {
        const LATCH_DWELL_MAX_MS: u64 = 30_000;
        let s = scene();
        let (_, rest, rest_fp) = settled(&s);
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        // Reduced from the card's first frame, a frame a tick, for the
        // whole dwell.
        let mut at = 0;
        while at < LATCH_DWELL_MAX_MS {
            assert_eq!(frame(&mut card, t0 + ms(at), &s, false).2, rest_fp);
            at += 1000;
        }
        let restored = t0 + ms(LATCH_DWELL_MAX_MS + 3 * FRAME_MS);
        let (_, over, fp) = frame(&mut card, restored, &s, true);
        assert_ne!(fp, rest_fp, "the show plays");
        assert_eq!(
            kitty_left(&over),
            kitty_left(&rest),
            "in the seat it was shown in"
        );
        assert!(over.len() > rest.len(), "the burst flies");
        assert_eq!(card.deadline(restored), Some(restored + ms(FRAME_MS)));
    }

    /// The span the trail's quads in `under` cover, left to right: the quads
    /// inside the border's stroke (the border's own lie on it).
    fn trail_reach(under: &[GlowQuad], s: &CardScene) -> Option<(i64, i64)> {
        let inner = Geo::new(s).expect("a scene").inner();
        let inside = under.iter().filter(|q| {
            let (x, y) = (i64::from(q.x), i64::from(q.y));
            x >= inner.x0
                && y >= inner.y0
                && x + i64::from(q.w) <= inner.x1
                && y + i64::from(q.h) <= inner.y1
        });
        let x0 = inside.clone().map(|q| i64::from(q.x)).min()?;
        let x1 = inside.map(|q| i64::from(q.x) + i64::from(q.w)).max()?;
        Some((x0, x1))
    }

    /// The frame `parts` lays at `clock`, as a tick hands it over (in the
    /// spend order).
    fn whole(s: &CardScene, clock: Clock) -> (Vec<GlowQuad>, Vec<GlowQuad>) {
        let p = parts(s, clock);
        let under = [p.border, p.trail].concat();
        let over = [p.kitty, p.rim, p.burst, p.stars, p.sparkles].concat();
        (under, over)
    }

    /// A KITTY THE GLASS SHOWED SEATED RIDES NO MORE (review of slice 5b): a
    /// card shown at rest — its episode played out, or held back mid-ride
    /// by a lost focus — and armed again (the window regained focus, the
    /// Autopilot changed) plays its episode in the seat: the kitty never
    /// snaps back to the trail's start, the trail stays whole, and the burst
    /// opens at the episode's start, as before the ride. At both heights.
    /// NEGATIVE CONTROL: the card's first episode rides (the kitty at the
    /// trail's start, the trail not yet laid).
    #[test]
    fn a_kitty_shown_seated_rides_no_more() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let what = format!("{kitty_rows} rows");
            let (rest_under, rest, _) = settled(&s);
            let (seat, reach) = (kitty_left(&rest), trail_reach(&rest_under, &s));
            assert!(seat.is_some() && reach.is_some(), "{what}");
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let (under, over, _) = frame(&mut card, t0, &s, true);
            assert_eq!(kitty_left(&over), Some(ride_from(&s)), "{what}: control");
            assert_ne!(trail_reach(&under, &s), reach, "{what}: control");
            // Played out and settled; then armed again.
            frame(&mut card, t0 + ms(EPISODE_MS + 10), &s, true);
            let t1 = t0 + ms(EPISODE_MS + 5000);
            card.arm(t1, 7);
            let first = frame(&mut card, t1, &s, true);
            assert_eq!(
                (first.0.clone(), first.1.clone()),
                whole(&s, Clock::seated(0)),
                "{what}: the seated episode's first frame"
            );
            assert!(
                !parts(&s, Clock::seated(0)).burst.is_empty(),
                "{what}: burst"
            );
            for t in [FRAME_MS, RIDE_MS / 2, RIDE_MS, 3000] {
                let (under, over, _) = frame(&mut card, t1 + ms(t), &s, true);
                assert_eq!(kitty_left(&over), seat, "{what}: {t} ms, seated");
                assert_eq!(trail_reach(&under, &s), reach, "{what}: {t} ms, whole");
            }
            // Held back mid-ride: the settled frame seats the kitty, and
            // the episode resumes with it there.
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            frame(&mut card, t0 + ms(300), &s, false);
            card.arm(t0 + ms(400), 7);
            for t in [400, 500, 900] {
                let (under, over, _) = frame(&mut card, t0 + ms(t), &s, true);
                assert_eq!(kitty_left(&over), seat, "{what}: resumed at {t} ms");
                assert_eq!(trail_reach(&under, &s), reach, "{what}: resumed at {t} ms");
            }
        }
    }

    /// An episode armed again while the kitty is still on its way (the
    /// Autopilot changed in the ride's first second) carries the ride on
    /// from where it is, on the ride's own clock: the kitty never steps back
    /// nor jumps to its seat, and the burst opens where the ride ends. One
    /// armed again after the ride ran out bursts at its own start.
    /// NEGATIVE CONTROL: the ride's clock and the new episode's differ (the
    /// frame is not the new episode's own first episode frame).
    #[test]
    fn a_ride_armed_again_on_its_way_carries_on() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let what = format!("{kitty_rows} rows");
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            let re = 500;
            frame(&mut card, t0 + ms(re), &s, true);
            card.arm(t0 + ms(re), 7);
            let mut last = i64::MIN;
            for t in (re..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
                let (under, over) = {
                    let f = frame(&mut card, t0 + ms(t), &s, true);
                    (f.0, f.1)
                };
                let clock = Clock {
                    t_ms: Some(t - re),
                    ride_ms: Some(t),
                };
                assert_eq!((under, over.clone()), whole(&s, clock), "{what}: {t} ms");
                let x0 = kitty_left(&over).expect("a kitty");
                assert!(x0 >= last, "{what}: {t} ms, back from {last} to {x0}");
                last = x0;
            }
            assert_ne!(
                whole(&s, Clock::riding(0)).1,
                whole(&s, Clock::riding(re)).1,
                "{what}: control"
            );
            assert!(
                !parts(
                    &s,
                    Clock {
                        t_ms: Some(RIDE_MS + 10 - re),
                        ride_ms: Some(RIDE_MS + 10),
                    }
                )
                .burst
                .is_empty(),
                "{what}: the burst where the ride ends"
            );
            // Armed again once the ride ran out: the burst at the start.
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            frame(&mut card, t0 + ms(2500), &s, true);
            let t1 = t0 + ms(2500);
            card.arm(t1, 7);
            let (under, over, _) = frame(&mut card, t1, &s, true);
            assert_eq!(
                (under, over),
                whole(&s, Clock::seated(0)),
                "{what}: run out"
            );
        }
    }

    /// The ride (§10.7): the kitty starts at the trail's start and comes in
    /// along it to its seat by [`RIDE_MS`], never stepping back, quick off
    /// the line and slow into the seat; drawn whole wherever it is (its art
    /// at the ridden spot, inside the box and its bands), with the trail laid
    /// to its rear (under it, to the haunch: [`KittyAt::rear`]) and no
    /// further; seated from [`RIDE_MS`] on. At both
    /// heights and on both grounds. NEGATIVE CONTROL: settled, a scene with
    /// no trail, and a trail that starts under the kitty are seated at once.
    #[test]
    fn the_kitty_rides_in_along_the_trail() {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    kitty_rows,
                    dark,
                    ..scene()
                };
                let what = format!("{kitty_rows} rows, dark {dark}");
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let (from, dist) = ride_span(&g, &s, &seat).expect("a ride");
                assert_eq!(from + dist, seat.rect.x0, "{what}");
                // The trail ends at the kitty's column, so it runs on under
                // the seated cat to its rear.
                assert_eq!(s.trail.expect("a trail").1, s.kitty_col.expect("a kitty"));
                let trail_end = seat.rear();
                let inks = KittyInk::new(s.coat, s.eye, s.dark);
                let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
                let mut last = i64::MIN;
                for t in (0..RIDE_MS + 4 * FRAME_MS).step_by(10) {
                    let clock = Clock::riding(t);
                    let p = parts(&s, clock);
                    let x0 = kitty_left(&p.kitty).expect("a kitty drawn");
                    assert!(x0 >= last, "{what}: {t} ms, back from {last} to {x0}");
                    last = x0;
                    let at = ridden(&g, &s, seat, clock);
                    assert_eq!(at.rect.y0, seat.rect.y0, "{what}: on the seat's rows");
                    assert_eq!(
                        drawn_kitty(&p.kitty, &at, inks),
                        art(kitty_frame(&at.art, clock)),
                        "{what}: {t} ms, whole where the ride has it"
                    );
                    for q in p.kitty.iter().chain(&p.rim).chain(&p.trail) {
                        assert_in_band_and_box(q, &s, &what);
                    }
                    let laid = p
                        .trail
                        .iter()
                        .map(|q| i64::from(q.x) + i64::from(q.w))
                        .max()
                        .unwrap_or(from);
                    if t >= RIDE_MS {
                        assert_eq!(x0, seat.rect.x0, "{what}: {t} ms, seated");
                        assert_eq!(laid, trail_end, "{what}: {t} ms, the whole trail");
                    } else {
                        assert_eq!(
                            laid,
                            trail_end.min(at.rear()),
                            "{what}: {t} ms, to its rear"
                        );
                    }
                }
                let at = |t: u64| kitty_left(&parts(&s, Clock::riding(t)).kitty);
                assert_eq!(at(0), Some(from), "{what}: from the trail's start");
                // Eased out: seven eighths of the way at half the time.
                let half = at(RIDE_MS / 2).expect("a kitty") - from;
                assert!(
                    half * 8 >= dist * 7 - 8,
                    "{what}: {half} of {dist} at half time"
                );
                assert!(at(RIDE_MS / 4).expect("a kitty") < seat.rect.x0, "{what}");
                assert_eq!(at(RIDE_MS), Some(seat.rect.x0), "{what}");
                assert_eq!(
                    kitty_left(&parts(&s, Clock::SETTLED).kitty),
                    Some(seat.rect.x0),
                    "{what}: settled, seated"
                );
            }
        }
        // CONTROLS: nothing to ride.
        let base = scene();
        let seat = kitty_rect(&Geo::new(&base).expect("a scene"), &base)
            .expect("a kitty")
            .rect
            .x0;
        for (name, s) in [
            (
                "no trail",
                CardScene {
                    trail: None,
                    ..base
                },
            ),
            (
                "a trail from the kitty",
                CardScene {
                    trail: Some((40, 44)),
                    ..base
                },
            ),
        ] {
            let g = Geo::new(&s).expect("a scene");
            assert!(
                ride_span(&g, &s, &kitty_rect(&g, &s).expect("a kitty")).is_none(),
                "{name}"
            );
            assert_eq!(
                kitty_left(&parts(&s, Clock::riding(0)).kitty),
                Some(seat),
                "{name}: seated at once"
            );
        }
    }

    /// On the ride the field comes up behind the kitty: no star is drawn on
    /// the kitty where the ride has it, nor ahead of it on the way to its
    /// seat, and every star is up a [`STAR_REVEAL_MS`] after the ride. The
    /// sparkles it sheds sit behind its back, in the star region and their
    /// bands, and are gone by the ride's end, before the burst opens; none
    /// when settled or with nothing to ride. NEGATIVE CONTROL: before the kitty starts, stars
    /// the field holds left of the seat are not drawn, and are once it is
    /// seated.
    #[test]
    fn the_field_and_the_sparkles_come_up_behind_the_kitty() {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    kitty_rows,
                    dark,
                    ..scene()
                };
                let what = format!("{kitty_rows} rows, dark {dark}");
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let region = star_region(&g, &s).expect("a star region");
                let reach = STAR_RATIOS
                    .iter()
                    .map(|r| i64::from(star_arm_px(star_arm(s.ch as f32, *r))))
                    .max()
                    .unwrap_or(0)
                    + 1;
                let mut shed = 0;
                for t in (0..RIDE_MS + SHED_MS + 4 * FRAME_MS).step_by(10) {
                    let clock = Clock::riding(t);
                    let p = parts(&s, clock);
                    let at = ridden(&g, &s, seat, clock).rect;
                    for q in &p.stars {
                        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                        let on = x0 < at.x1 && at.x0 < x1 && y0 < at.y1 && at.y0 < y1;
                        assert!(!on, "{what}: {t} ms, a star on the kitty {q:?}");
                        let ahead = x0 < seat.rect.x0 - reach && x1 > at.x0;
                        assert!(!ahead, "{what}: {t} ms, a star ahead of the kitty {q:?}");
                    }
                    if t >= RIDE_MS {
                        // Out before the burst opens (`MAX_CARD_QUADS`' count).
                        assert!(p.sparkles.is_empty(), "{what}: {t} ms, the sparkles gone");
                    }
                    shed += p.sparkles.len();
                    for q in &p.sparkles {
                        assert_in_band_and_box(q, &s, &what);
                        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                        assert!(x1 <= at.x0, "{what}: {t} ms, behind the kitty {q:?}");
                        assert!(
                            region.x0 <= x0
                                && x1 <= region.x1
                                && region.y0 <= y0
                                && y1 <= region.y1,
                            "{what}: {t} ms, in the star region {q:?}"
                        );
                    }
                }
                assert!(shed > 0, "{what}: sparkles shed");
                // CONTROL: the field left of the seat is held back, then up.
                let left_of_seat = |p: &Parts| {
                    p.stars
                        .iter()
                        .filter(|q| i64::from(q.x) + i64::from(q.w) < seat.rect.x0 - reach)
                        .count()
                };
                let up = parts(&s, Clock::riding(RIDE_MS + STAR_REVEAL_MS as u64));
                let settled = parts(&s, Clock::SETTLED);
                assert!(
                    left_of_seat(&settled) > 0,
                    "{what}: a field left of the seat"
                );
                assert_eq!(left_of_seat(&parts(&s, Clock::riding(0))), 0, "{what}");
                assert!(left_of_seat(&up) > 0, "{what}: up after the ride");
                assert!(settled.sparkles.is_empty(), "{what}: settled, none shed");
            }
        }
        let still = CardScene {
            trail: None,
            ..scene()
        };
        for t in (0..RIDE_MS + SHED_MS).step_by(10) {
            assert!(
                parts(&still, Clock::riding(t)).sparkles.is_empty(),
                "{t} ms"
            );
        }
    }

    /// The ride converges on the settled frame: from [`RIDE_MS`] the kitty's
    /// quads are the settled frame's (until the first blink), its rim the
    /// settled ring's pixels off the trail (which steps with the wave), the
    /// trail the settled trail's span; once the wave is still (and no blink
    /// or flick shows) the rim lights the settled rim's very pixels.
    #[test]
    fn the_ride_ends_on_the_settled_frame() {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    kitty_rows,
                    dark,
                    ..scene()
                };
                let what = format!("{kitty_rows} rows, dark {dark}");
                let rest = parts(&s, Clock::SETTLED);
                let rects = |qs: &[GlowQuad]| {
                    let mut v: Vec<_> = qs.iter().map(|q| (q.x, q.y, q.w, q.h)).collect();
                    v.sort_unstable();
                    v
                };
                let span = |qs: &[GlowQuad]| {
                    (
                        qs.iter().map(|q| q.x).min(),
                        qs.iter().map(|q| i64::from(q.x) + i64::from(q.w)).max(),
                    )
                };
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let ring = rim_ring(
                    &seat,
                    seat.art.open,
                    rim_px(seat.p, g.t),
                    g.inner().meet(g.clip),
                );
                let lit = |qs: &[GlowQuad]| {
                    coverage(qs)
                        .into_keys()
                        .collect::<std::collections::BTreeSet<_>>()
                };
                for t in (RIDE_MS..BLINK_AT_MS).step_by(10) {
                    let p = parts(&s, Clock::riding(t));
                    assert_eq!(p.kitty, rest.kitty, "{what}: {t} ms");
                    let trail = lit(&p.trail);
                    let off: std::collections::BTreeSet<_> =
                        ring.difference(&trail).copied().collect();
                    assert_eq!(lit(&p.rim), off, "{what}: {t} ms");
                    assert_eq!(span(&p.trail), span(&rest.trail), "{what}: {t} ms");
                }
                let still = parts(&s, Clock::riding(EPISODE_MS - 100));
                assert_eq!(lit(&still.rim), lit(&rest.rim), "{what}: the wave still");
                assert_eq!(rects(&still.kitty), rects(&rest.kitty), "{what}");
                // And not before: the kitty is still on its way at the start.
                let start = parts(&s, Clock::riding(0));
                assert_ne!(start.kitty, rest.kitty, "{what}");
            }
        }
    }

    /// The burst flies where the ride ends, round the seated kitty, for
    /// [`BURST_MS`] from [`RIDE_MS`], at both heights; none on the ride, none
    /// settled. NEGATIVE CONTROL: a kitty with no trail to ride bursts at the
    /// episode's start.
    #[test]
    fn the_burst_opens_where_the_ride_ends_and_ends() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let seat = kitty_rect(&g, &s).expect("a kitty").rect;
            let burst = |t: u64| parts(&s, Clock::riding(t)).burst;
            for t in (0..RIDE_MS).step_by(FRAME_MS as usize) {
                assert!(
                    burst(t).is_empty(),
                    "{kitty_rows} rows: none on the ride, {t} ms"
                );
            }
            for t in (RIDE_MS..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
                let b = burst(t);
                // Whole for its first half; fading to nothing after.
                if t < RIDE_MS + BURST_MS / 2 {
                    assert!(b.len() >= BURST_STARS * 3, "{kitty_rows} rows: {t} ms");
                } else if b.is_empty() {
                    continue;
                }
                // Round the seat: its centre within the stars' spread.
                let (x0, x1) = (
                    b.iter().map(|q| i64::from(q.x)).min().unwrap_or(0),
                    b.iter()
                        .map(|q| i64::from(q.x) + i64::from(q.w))
                        .max()
                        .unwrap_or(0),
                );
                let cx = (seat.x0 + seat.x1) / 2;
                assert!(
                    x0 < cx && cx < x1,
                    "{kitty_rows} rows: {t} ms round the seat"
                );
            }
            assert!(
                burst(RIDE_MS + BURST_MS).is_empty(),
                "{kitty_rows} rows: gone"
            );
            assert!(parts(&s, Clock::SETTLED).burst.is_empty(), "settled: none");
            let (_, after, _) = {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.arm(t0, 7);
                frame(&mut card, t0 + ms(RIDE_MS + BURST_MS + SHED_MS), &s, true)
            };
            let (_, rest, _) = settled(&s);
            assert!(
                after.len() <= rest.len() + STARS_MAX * 4,
                "after the burst only the field is left"
            );
        }
        // CONTROL: a kitty with no trail to ride bursts at the start.
        let still = CardScene {
            trail: None,
            ..scene()
        };
        assert!(!parts(&still, Clock::riding(0)).burst.is_empty());
        assert!(parts(&still, Clock::riding(BURST_MS)).burst.is_empty());
    }

    /// The field is five to nine stars, inside the trail and kitty region (so
    /// never over the card's text), clear of the kitty.
    #[test]
    fn the_field_is_five_to_nine_stars_clear_of_text_and_kitty() {
        let mut counts = std::collections::BTreeSet::new();
        for seed in 0..64 {
            for s in [
                scene(),
                CardScene {
                    dark: false,
                    ..scene()
                },
            ] {
                let g = Geo::new(&s).expect("a scene");
                let (spots, n) = field_spots(&g, &s, seed, !s.dark);
                counts.insert(n);
                let region = star_region(&g, &s).expect("a region");
                let kitty = kitty_rect(&g, &s).expect("a kitty").rect;
                let text_end = g.col(s.trail.expect("trail").0);
                for p in &spots[..n] {
                    assert!(
                        region.contains(p.x, p.y),
                        "a star outside its region: {p:?}"
                    );
                    assert!(p.x >= text_end, "a star over the card's text: {p:?}");
                    assert!(!kitty.contains(p.x, p.y), "a star on the kitty: {p:?}");
                }
            }
        }
        assert!(
            counts.iter().all(|n| (5..=9).contains(n)),
            "counts {counts:?}"
        );
        assert!(counts.len() > 1, "the seed varies the count: {counts:?}");
    }

    /// The card's dark star is the crate's one star: the same quads
    /// `push_twinkle_star` lays, given the same centre, arm and coverage.
    #[test]
    fn the_dark_star_is_the_family_star() {
        let s = scene();
        let g = Geo::new(&s).expect("a scene");
        let geom = Geom {
            cw: 1,
            ch: usize::from(s.ch),
            rows: 64,
            cols: 4000,
            origin_x: 0,
            origin_y: s.origin_y as u16,
            win_w: 4000,
            win_h: 2000,
            head: 0,
        };
        for (arm, cov, gold) in [(2i64, 235.0f32, false), (3, 200.0, true), (6, 255.0, true)] {
            let (sx, sy) = (g.col(45), g.band(g.top + 1) + 1);
            let mut ours = Vec::new();
            let mut sink = Sink {
                geo: g,
                left: MAX_CARD_QUADS,
            };
            assert!(push_star(
                &mut ours,
                &mut sink,
                g.clip,
                (sx, sy),
                arm,
                cov,
                gold,
                WHITE,
                &[],
                true
            ));
            let mut family = Vec::new();
            assert!(push_twinkle_star(
                &mut family,
                geom,
                sx as i32,
                sy as i32,
                arm as i32,
                cov as u8,
                gold,
                WHITE,
                4096
            ));
            for q in &mut family {
                q.row += s.row_base;
            }
            assert_eq!(ours, family, "arm {arm} cov {cov} gold {gold}");
        }
    }

    /// The budget holds on a pathological scene: hundreds of columns, many
    /// boxed rows, a long trail and tall cells, through the burst; and at the
    /// shipping cell sizes it never binds, with a whole star to spare.
    #[test]
    fn the_quad_budget_holds() {
        let s = CardScene {
            cw: 20,
            ch: 44,
            cols: 3000,
            rows: 40,
            trail: Some((12, 2900)),
            kitty_col: Some(2900),
            origin_y: 0,
            origin_x: 0,
            top: 0,
            ..scene()
        };
        for dark in [true, false] {
            let s = CardScene { dark, ..s };
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 1);
            for at in [0, 100, 450, RIDE_MS + 100, 2000, EPISODE_MS + 1] {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                assert!(
                    under.len() + over.len() <= MAX_CARD_QUADS,
                    "{} quads at {at} ms",
                    under.len() + over.len()
                );
            }
        }
        // At shipping sizes the cap never binds: the busiest frame (the burst
        // at its widest) leaves room for one more whole star.
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 1);
            let worst = (0..=EPISODE_MS)
                .step_by(11)
                .map(|at| {
                    let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                    under.len() + over.len()
                })
                .max()
                .unwrap_or(0);
            assert!(
                worst + STAR_QUADS_MAX < MAX_CARD_QUADS,
                "{worst} quads at {} × {}",
                s.cw,
                s.ch
            );
        }
        // The busiest frame a launch card draws — the burst opening as the
        // ride ends beside the field, the rim and the stepping trail — on its
        // longest trail (20 cells), its border standing in air over and under
        // the box, at both heights, at the shipping cell sizes and over many
        // seeds: the frame it wants is under the cap with a whole star to
        // spare.
        for (cw, ch) in [(9u16, 18u16), (16, 33), (17, 35)] {
            for kitty_rows in [2u8, 3] {
                let s = aired(CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    trail: Some((32, 52)),
                    kitty_col: Some(52),
                    cols: 63,
                    ..scene()
                });
                for seed in 0..40u64 {
                    for t in (RIDE_MS..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
                        let p = parts_seeded(&s, Clock::riding(t), seed);
                        let want = [
                            &p.border,
                            &p.trail,
                            &p.kitty,
                            &p.rim,
                            &p.burst,
                            &p.stars,
                            &p.sparkles,
                        ]
                        .iter()
                        .map(|v| v.len())
                        .sum::<usize>();
                        assert!(
                            want + STAR_QUADS_MAX < MAX_CARD_QUADS,
                            "{cw}x{ch}, {kitty_rows} rows, seed {seed}, {t} ms: {want} quads"
                        );
                    }
                }
            }
        }
    }

    /// A STAR IS LAID WHOLE OR TAKEN BACK, TO THE QUAD: with the budget
    /// one quad short of a star, [`push_star`] lays nothing, spends nothing
    /// and says the budget is spent; with exactly enough it lays the whole
    /// star and spends it all — a light star the trail's edge crosses, laid
    /// in two tones ([`lay_toned`]), too. NEGATIVE CONTROL: [`lay_star`]
    /// alone, one short, leaves part of a star behind.
    #[test]
    fn a_star_is_laid_whole_or_taken_back() {
        let s = scene();
        let geo = Geo::new(&s).expect("a scene");
        let region = star_region(&geo, &s).expect("a region");
        let centre = ((region.x0 + region.x1) / 2, (region.y0 + region.y1) / 2);
        // A trail's ink whose top edge runs through the star's middle, in
        // two segments stepped a pixel apart.
        let edge = [
            Rect {
                x0: centre.0 - 40,
                y0: centre.1,
                x1: centre.0,
                y1: centre.1 + 40,
            },
            Rect {
                x0: centre.0,
                y0: centre.1 + 1,
                x1: centre.0 + 40,
                y1: centre.1 + 40,
            },
        ];
        for (arm, gold, dark, on_trail) in [
            (3, false, true, &[][..]),
            (6, true, true, &[][..]),
            (5, true, false, &[][..]),
            (5, true, false, &edge[..]),
        ] {
            let star = |left: usize, whole: bool| {
                let mut sink = Sink { geo, left };
                let mut out = Vec::new();
                let laid = if whole {
                    push_star(
                        &mut out,
                        &mut sink,
                        region,
                        centre,
                        arm,
                        200.0,
                        gold,
                        0x00E3_141B,
                        on_trail,
                        dark,
                    )
                } else {
                    lay_star(
                        &mut out,
                        &mut sink,
                        region,
                        centre,
                        arm,
                        200.0,
                        gold,
                        0x00E3_141B,
                        on_trail,
                        dark,
                    )
                };
                (laid, out.len(), sink.left)
            };
            let (laid, need, _) = star(usize::MAX, true);
            assert!(laid && need > 4, "arm {arm}: {need} quads");
            assert_eq!(
                star(need, true),
                (true, need, 0),
                "arm {arm}: exactly enough"
            );
            assert_eq!(
                star(need - 1, true),
                (false, 0, need - 1),
                "arm {arm}: one short"
            );
            let (laid, part, _) = star(need - 1, false);
            assert!(!laid && part > 0, "arm {arm}: CONTROL, part of a star");
        }
    }

    /// THE BURST STAYS WHOLE UNDER THE CAP: where the burst opens at the
    /// ride's end on a 20-cell trail at a large cell, three rows tall, in a
    /// box of two dozen rows (a pathological scene: the border's sides cost
    /// two quads a row), a frame can want more than [`MAX_CARD_QUADS`],
    /// which no launch card does; it is held to the cap,
    /// the burst's ring is drawn whole in every such frame, and what the
    /// frame drops is whole field stars only, no more of them than the
    /// overflow needs (a star's worth of quads). NEGATIVE CONTROL: some of
    /// these frames do want more than the cap.
    #[test]
    fn the_burst_stays_whole_under_the_cap() {
        let mut over_cap = 0;
        for (cw, ch) in [(20u16, 44u16), (22, 48)] {
            let s = CardScene {
                cw,
                ch,
                kitty_rows: 3,
                rows: 24,
                trail: Some((32, 52)),
                kitty_col: Some(52),
                cols: 63,
                ..scene()
            };
            for seed in 0..40u64 {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.arm(t0, seed);
                for t in (RIDE_MS..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
                    let p = parts_seeded(&s, Clock::riding(t), seed);
                    let want: usize = [
                        &p.border,
                        &p.trail,
                        &p.kitty,
                        &p.rim,
                        &p.burst,
                        &p.stars,
                        &p.sparkles,
                    ]
                    .iter()
                    .map(|v| v.len())
                    .sum();
                    let (under, over, _) = frame(&mut card, t0 + ms(t), &s, true);
                    let got = under.len() + over.len();
                    let what = format!("{cw}x{ch}, seed {seed}, {t} ms: {got} of {want}");
                    assert!(got <= MAX_CARD_QUADS, "{what}");
                    let head = p.kitty.len() + p.rim.len();
                    assert_eq!(over[head..head + p.burst.len()], p.burst[..], "{what}");
                    if want > MAX_CARD_QUADS {
                        over_cap += 1;
                        let field = &over[head + p.burst.len()..];
                        assert!(p.stars.starts_with(field), "{what}: whole field stars");
                        assert!(got + STAR_QUADS_MAX > MAX_CARD_QUADS, "{what}: no waste");
                    } else {
                        assert_eq!(got, want, "{what}: all of it");
                    }
                }
            }
        }
        assert!(over_cap > 0, "CONTROL: frames that want more than the cap");
    }

    /// Equal inputs give equal bytes, and the seed moves the stars.
    #[test]
    fn the_card_is_deterministic() {
        for s in scenes() {
            let t0 = Instant::now();
            let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
            a.arm(t0, 42);
            b.arm(t0, 42);
            for at in [0, 33, 700, 2600, EPISODE_MS + 5] {
                assert_eq!(
                    frame(&mut a, t0 + ms(at), &s, true),
                    frame(&mut b, t0 + ms(at), &s, true)
                );
            }
        }
        let s = scene();
        let t0 = Instant::now();
        let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
        a.arm(t0, 1);
        b.arm(t0, 2);
        assert_ne!(
            frame(&mut a, t0 + ms(EPISODE_MS), &s, true),
            frame(&mut b, t0 + ms(EPISODE_MS), &s, true)
        );
    }

    /// What a card shows its host after a step: when it next wants a frame,
    /// at several instants, whether it says it is at rest and still, and its
    /// line's ink.
    fn observed(card: &HarnessCard, at: Instant) -> Vec<String> {
        let mut seen = Vec::new();
        for dt in [
            0,
            1,
            FRAME_MS,
            400,
            EPISODE_MS,
            EPISODE_MS + SETTLE_GRACE_MS + 1,
        ] {
            let t = at + ms(dt);
            seen.push(format!("deadline {dt}: {:?}", card.deadline(t)));
            seen.push(format!(
                "ink {dt}: {:?}",
                card.line_ink(t, 4, 2, 9, NIGHT, true)
            ));
        }
        seen.push(format!(
            "at_rest {} still {}",
            card.at_rest(),
            card.is_still()
        ));
        seen
    }

    /// A CARD AT REST IS REPLAYED, BIT FOR BIT (`tick_or_replay`): beside a
    /// twin that ticks every step, a card that ticks through its kept
    /// settled frame appends the same quads, returns the same fingerprint
    /// and shows its host the same card (its deadlines, its line, at rest
    /// and still) after every step of a life — an episode, its end, still
    /// frames, a scene that changes, serious mode, a window that loses
    /// motion and an episode owed until motion comes back, and an owed
    /// debt that lapses. Every step that replays reads no clock. So does a
    /// card driven the host's way (`replay`, else `draw_kept`, the quads read
    /// off `kept` where it replays), which makes no fingerprint at all.
    /// CONTROL: every episode frame, every first frame of a new scene and
    /// every step out of an owed episode is ticked (the clock read), the
    /// replays happen — on every still step after the first — and
    /// `tick_or_replay` makes the fingerprint it returns (the counter sees
    /// the work the host's way skips).
    #[test]
    fn a_card_at_rest_is_replayed_bit_for_bit() {
        for s in scenes() {
            // The other Autopilot's palette: another scene.
            let land = CardScene {
                palette: match s.palette {
                    CardPalette::Land => CardPalette::Cruise,
                    CardPalette::Cruise => CardPalette::Land,
                },
                ..s
            };
            let serious = CardScene { serious: true, ..s };
            let t0 = Instant::now();
            // (ms after t0, scene, animate, arm, whether the step replays)
            let steps: Vec<(u64, CardScene, bool, bool, bool)> = vec![
                (0, s, true, true, false),
                (33, s, true, false, false),
                // Motion lost mid-episode (focus, Reduce Motion, the load
                // latch): the first held frame is ticked, the rest
                // replayed; motion back resumes the clock.
                (100, s, false, false, false),
                (140, s, false, false, true),
                (180, s, false, false, true),
                (220, s, true, false, false),
                (2000, s, true, false, false),
                (EPISODE_MS + 1, s, true, false, false),
                (EPISODE_MS + 40, s, true, false, true),
                (EPISODE_MS + 90, s, false, false, true),
                (EPISODE_MS + 95, land, true, false, false),
                (EPISODE_MS + 99, land, true, false, true),
                (EPISODE_MS + 120, serious, true, false, false),
                (EPISODE_MS + 130, serious, true, false, true),
                (EPISODE_MS + 140, s, true, false, false),
                // The window loses motion and the card is armed: owed.
                (EPISODE_MS + 200, s, false, true, false),
                (EPISODE_MS + 300, s, false, false, true),
                (EPISODE_MS + 400, s, false, false, true),
                // Motion back: the owed episode plays from here.
                (EPISODE_MS + 500, s, true, false, false),
                (EPISODE_MS + 533, s, true, false, false),
                // Armed unfocused again, held past the debt's lapse.
                (3 * EPISODE_MS, s, false, true, false),
                (3 * EPISODE_MS + OWED_MAX_MS + 10, s, false, false, true),
                (3 * EPISODE_MS + OWED_MAX_MS + 20, s, true, false, false),
                (3 * EPISODE_MS + OWED_MAX_MS + 30, s, true, false, true),
            ];
            let (mut twin, mut card) = (HarnessCard::default(), HarnessCard::default());
            let mut kept = SettledFrame::default();
            // The host's way (`claude_card`): `replay`, else `draw_kept`.
            let (mut host, mut host_kept) = (HarnessCard::default(), SettledFrame::default());
            for (at, scene, animate, arm, replays) in steps {
                let now = t0 + ms(at);
                if arm {
                    twin.arm(now, 9);
                    card.arm(now, 9);
                    host.arm(now, 9);
                }
                let want = frame(&mut twin, now, &scene, animate);
                let _ = FRAME_FPS.with(|n| n.replace(0));
                let hosted = if host.replay(&scene, animate, &host_kept) {
                    (host_kept.under().to_vec(), host_kept.over().to_vec(), true)
                } else {
                    let (mut under, mut over) = (Vec::new(), Vec::new());
                    host.draw_kept(now, &scene, animate, &mut host_kept, &mut under, &mut over);
                    (under, over, false)
                };
                assert_eq!(
                    FRAME_FPS.with(|n| n.replace(0)),
                    0,
                    "{at} ms: the host's way makes no fingerprint"
                );
                assert_eq!(
                    (&hosted.0, &hosted.1, hosted.2),
                    (&want.0, &want.1, replays),
                    "{at} ms: the host's frame"
                );
                assert_eq!(observed(&host, now), observed(&twin, now), "{at} ms: host");
                let (mut under, mut over) = (Vec::new(), Vec::new());
                let mut read = 0;
                let (fp, ticked) = card.tick_or_replay(
                    || {
                        read += 1;
                        now
                    },
                    &scene,
                    animate,
                    &mut kept,
                    &mut under,
                    &mut over,
                );
                assert_eq!(
                    FRAME_FPS.with(|n| n.replace(0)),
                    1,
                    "{at} ms: CONTROL, the fingerprint is made where it is asked for"
                );
                assert_eq!((under, over, fp), want, "{at} ms: the same frame");
                assert_eq!(ticked.is_none(), replays, "{at} ms: replayed?");
                assert_eq!(read, usize::from(!replays), "{at} ms: clock reads");
                assert_eq!(observed(&card, now), observed(&twin, now), "{at} ms");
            }
        }
    }

    /// The fourteen-step bisection [`lift_to_luminance`] replaced: the
    /// reference its four-way search is held to.
    fn lift_by_bisection(rgb: u32, floor: f32) -> u32 {
        if relative_luminance(rgb) >= floor {
            return rgb;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..14 {
            let mid = 0.5 * (lo + hi);
            if relative_luminance(mix_rgb(rgb, WHITE, mid)) >= floor {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        mix_rgb(rgb, WHITE, hi)
    }

    /// THE LINE'S LIFT IS THE BISECTION'S, BIT FOR BIT
    /// (`lift_to_luminance`): over a lattice of colours — every channel in
    /// steps of 5 at the line's own floor, of 15 at floors from none to past
    /// white's — the four-way search returns the colour the fourteen-step
    /// bisection did. NEGATIVE CONTROL: the same search a round short (a
    /// `2^-12` grid) returns another colour for some of them, so the
    /// comparison sees a search that stops early.
    /// Abutting gradient quads in one band whose ends do not meet: each
    /// quad whose left edge is another's right edge, at the same `y` and
    /// height, must start in the colour (and opacity) that one ends in.
    /// Returns the mismatches and how many abutting pairs were checked.
    fn seams(quads: &[GlowQuad]) -> (Vec<String>, usize) {
        let mut ends = std::collections::HashMap::new();
        for q in quads {
            ends.insert((q.y, q.h, q.x + q.w), (q.color2, q.alpha2));
        }
        let (mut bad, mut checked) = (Vec::new(), 0);
        for q in quads {
            if let Some(&end) = ends.get(&(q.y, q.h, q.x)) {
                checked += 1;
                if end != (q.color, q.alpha) {
                    bad.push(format!("{q:?} starts off {end:?}"));
                }
            }
        }
        (bad, checked)
    }

    /// THE BORDER'S AND THE TRAIL'S SEGMENTS MEET: a horizontal border
    /// segment's left end is the last one's right end, and so is each trail
    /// stripe's, in every frame — settled, mid-episode, both themes, Land's
    /// ramp. The emitter reuses the last segment's right-end paint instead of
    /// painting the same arc point again; a reuse that kept the wrong paint
    /// (the first segment's, or a stripe's first) would break every seam.
    /// CONTROL: the check sees seams (the scenes' borders and trails have
    /// several segments), and one quad's start nudged off its neighbour's end
    /// is caught.
    #[test]
    fn the_border_and_trail_segments_meet() {
        let mut checked = 0;
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            for at in [0, 40, 450, 1300, 2600, 4100, EPISODE_MS + 1] {
                let (under, _, _) = frame(&mut card, t0 + ms(at), &s, true);
                let (bad, n) = seams(&under);
                assert!(bad.is_empty(), "at {at} ms: {bad:#?}");
                checked += n;
            }
        }
        assert!(checked > 100, "seams checked: {checked}");
        let (mut under, _, _) = settled(&scene());
        let target = (0..under.len())
            .find(|&i| {
                let q = under[i];
                under
                    .iter()
                    .any(|p| (p.y, p.h, p.x + p.w) == (q.y, q.h, q.x))
            })
            .expect("the settled frame has a seam");
        under[target].color ^= 0x0001_0101;
        assert_eq!(seams(&under).0.len(), 1, "a nudged start is caught");
    }

    #[test]
    fn a_lift_finds_the_bisections_mix() {
        let coarse = |rgb: u32, floor: f32| {
            if relative_luminance(rgb) >= floor {
                return rgb;
            }
            let reaches = |t: f32| relative_luminance(mix_rgb(rgb, WHITE, t)) >= floor;
            let (mut short, mut step) = (0.0f32, 0.25f32);
            for _ in 0..6 {
                let n = [1.0, 2.0, 3.0].map(|j| !reaches(short + j * step));
                short += step * f32::from(n.iter().map(|&s| u8::from(s)).sum::<u8>());
                step *= 0.25;
            }
            mix_rgb(rgb, WHITE, short + 4.0 * step)
        };
        let mut seen_short = 0;
        let mut checked = 0;
        for (floor, every) in [
            (LINE_DARK_LUMA_FLOOR, 5),
            (0.0, 15),
            (0.02, 15),
            (0.18, 15),
            (0.5, 15),
            (0.9, 15),
            (1.0, 15),
            (1.2, 15),
        ] {
            for r in (0..=255u32).step_by(every) {
                for g in (0..=255u32).step_by(every) {
                    for b in (0..=255u32).step_by(every) {
                        let rgb = r << 16 | g << 8 | b;
                        let want = lift_by_bisection(rgb, floor);
                        assert_eq!(lift_to_luminance(rgb, floor), want, "{rgb:06x} to {floor}");
                        seen_short += usize::from(coarse(rgb, floor) != want);
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 140_000, "{checked} colours");
        assert!(seen_short > 0, "CONTROL: a search a round short is seen");
    }

    /// AT REST IS KNOWN WITHOUT A CLOCK (`HarnessCard::at_rest`): wherever
    /// it says so, the card wants no frame at any instant; and a card it
    /// does not call at rest is one whose episode is armed or playing.
    /// CONTROL: an episode's frames are not at rest.
    #[test]
    fn at_rest_means_no_deadline_at_any_instant() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        assert!(card.at_rest(), "unarmed");
        card.arm(t0, 5);
        assert!(!card.at_rest(), "CONTROL: armed");
        for (at, animate) in [
            (0, true),
            (500, true),
            (EPISODE_MS + 1, true),
            (EPISODE_MS + 2, true),
        ] {
            let now = t0 + ms(at);
            let _ = frame(&mut card, now, &s, animate);
            if card.at_rest() {
                for dt in [0, 1, FRAME_MS, EPISODE_MS, 10 * EPISODE_MS] {
                    assert_eq!(card.deadline(now + ms(dt)), None, "{at} ms + {dt}");
                }
            } else {
                assert!(card.deadline(now).is_some(), "CONTROL: {at} ms plays");
            }
        }
        assert!(card.at_rest(), "settled");
    }

    /// An empty scene draws nothing and says so.
    #[test]
    fn an_empty_scene_draws_nothing() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 1);
        for s in [
            CardScene { rows: 0, ..scene() },
            CardScene { cols: 0, ..scene() },
            CardScene { ch: 0, ..scene() },
        ] {
            assert_eq!(frame(&mut card, t0, &s, true), (Vec::new(), Vec::new(), 0));
        }
    }

    /// The line's ink: one cell per column, sorted and unique; a sweep during
    /// the episode; the settled gradient (constant bytes) after it and under
    /// reduced motion; legible on both grounds (on light, WCAG AA against
    /// the ground, frame by frame).
    #[test]
    fn the_line_sweeps_then_settles() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 3);
        let (row, col, len) = (7u16, 11u16, 64u16);
        for ground in [NIGHT, DAY] {
            let dark = ground == NIGHT;
            let rest = card.line_ink(t0 + ms(EPISODE_MS), row, col, len, ground, true);
            assert_eq!(rest.len(), usize::from(len));
            for (i, c) in rest.iter().enumerate() {
                assert_eq!(
                    (c.row, c.col),
                    (row, col + i as u16),
                    "sorted, unique, contiguous"
                );
            }
            assert_eq!(
                rest,
                card.line_ink(t0 + ms(EPISODE_MS + 5000), row, col, len, ground, true),
                "settled is constant"
            );
            assert_eq!(
                rest,
                card.line_ink(t0 + ms(900), row, col, len, ground, false),
                "reduced motion is settled"
            );
            let mid = card.line_ink(t0 + ms(900), row, col, len, ground, true);
            let next = card.line_ink(t0 + ms(900 + FRAME_MS), row, col, len, ground, true);
            assert_ne!(mid, rest, "the episode moves the line");
            assert_ne!(mid, next, "frame to frame");
            assert_ne!(
                rest[0].color,
                rest[rest.len() - 1].color,
                "the settled line is a gradient"
            );
            for c in rest.iter().chain(&mid) {
                let rgb = (u32::from(c.color[0]) << 16)
                    | (u32::from(c.color[1]) << 8)
                    | u32::from(c.color[2]);
                if dark {
                    assert!(
                        relative_luminance(rgb) >= LINE_DARK_LUMA_FLOOR - 0.01,
                        "{rgb:06x} too dark"
                    );
                } else {
                    assert!(
                        contrast(rgb, ground) >= 4.5,
                        "{rgb:06x} under AA on {ground:06x}: {:.2}:1",
                        contrast(rgb, ground)
                    );
                }
            }
        }
        // The line stops at the last column rather than wrapping.
        assert_eq!(card.line_ink(t0, 0, u16::MAX - 1, 5, NIGHT, true).len(), 2);
    }

    /// The line wears the last tick's palette.
    #[test]
    fn the_line_wears_the_cards_palette() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        let cruise = card.line_ink(t0, 0, 0, 30, NIGHT, false);
        let mut under = Vec::new();
        let mut over = Vec::new();
        card.tick(
            t0,
            &CardScene {
                palette: CardPalette::Land,
                ..scene()
            },
            false,
            &mut under,
            &mut over,
        );
        assert_ne!(card.line_ink(t0, 0, 0, 30, NIGHT, false), cruise);
    }

    /// The sunset ramp runs red → orange → gold → rose and is total.
    #[test]
    fn the_sunset_ramp_hits_its_stops() {
        assert_eq!(sunset(0.0), SUNSET[0]);
        assert_eq!(sunset(1.0), SUNSET[3]);
        assert_eq!(sunset(1.0 / 3.0), SUNSET[1]);
        assert_eq!(sunset(f32::NAN), SUNSET[0]);
        assert_eq!(sunset(7.0), SUNSET[3]);
    }

    /// Whether quad `q` lies on the border's rim (within the stroke of the
    /// box's outer edge) rather than inside it.
    fn on_rim(q: &GlowQuad, s: &CardScene) -> bool {
        let g = Geo::new(s).expect("a scene");
        let inner = g.inner();
        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
        y1 <= inner.y0 || y0 >= inner.y1 || x1 <= inner.x0 || x0 >= inner.x1
    }

    /// The pixel art the opaque quads in `over` draw where the kitty `at`
    /// sits, read back into palette characters through `inks` — each art
    /// pixel the colour of the quad over its centre, `.` where none is: the
    /// inverse of [`Emit::kitty`], so a frame's art can be compared to its
    /// bitmap whatever rectangles it was drawn in.
    fn drawn_kitty(over: &[GlowQuad], at: &KittyAt, inks: KittyInk) -> Vec<String> {
        let (r0, p) = (at.rect, at.p);
        (0..at.art.h() as i64)
            .map(|r| {
                (0..KITTY_W as i64)
                    .map(|c| {
                        let (x, y) = (r0.x0 + c * p + p / 2, r0.y0 + r * p + p / 2);
                        over.iter()
                            .filter(|q| q.alpha == 255)
                            .find(|q| {
                                let (qx, qy) = (i64::from(q.x), i64::from(q.y));
                                (qx..qx + i64::from(q.w)).contains(&x)
                                    && (qy..qy + i64::from(q.h)).contains(&y)
                            })
                            .map_or('.', |q| {
                                char::from(
                                    *KITTY_PALETTE
                                        .iter()
                                        .find(|&&b| inks.of(b) == Some(q.color))
                                        .expect("a palette colour"),
                                )
                            })
                    })
                    .collect()
            })
            .collect()
    }

    /// The blink and the tail flick are drawn in their windows and only
    /// there: the eyes shut for [`BLINK_MS`] from [`BLINK_AT_MS`] (and every
    /// [`BLINK_PERIOD_MS`] after), the tail swept back for [`FLICK_MS`] from
    /// [`FLICK_AT_MS`], the open frame between them and once settled. Read
    /// back from the quads a tick draws, not from the frame picker alone.
    #[test]
    fn the_blink_and_the_flick_come_in_their_windows() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let at = kitty_rect(&g, &s).expect("a kitty");
            let inks = KittyInk::new(s.coat, s.eye, s.dark);
            let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut seen = [0usize; 3];
            for t in (0..EPISODE_MS).step_by(10) {
                let (_, over, _) = frame(&mut card, t0 + ms(t), &s, true);
                // The first flick falls on the ride: read the kitty where the
                // ride has it.
                let at = ridden(&g, &s, at, Clock::riding(t));
                let in_window =
                    |first: u64, period: u64, len: u64| t >= first && (t - first) % period < len;
                let (want, k) = if in_window(BLINK_AT_MS, BLINK_PERIOD_MS, BLINK_MS) {
                    (at.art.blink, 1)
                } else if in_window(FLICK_AT_MS, FLICK_PERIOD_MS, FLICK_MS) {
                    (at.art.flick, 2)
                } else {
                    (at.art.open, 0)
                };
                assert_eq!(drawn_kitty(&over, &at, inks), art(want), "{t} ms");
                seen[k] += 1;
            }
            assert!(seen.iter().all(|&n| n > 0), "every frame drawn: {seen:?}");
            // Twice each in a 6 s episode: at 1.25 s and 3.75 s, at 0.4 s and
            // 2.9 s (and 5.4 s for the flick).
            assert_eq!(seen[1], 2 * (BLINK_MS as usize / 10), "two blinks");
            assert_eq!(seen[2], 3 * (FLICK_MS as usize / 10), "three flicks");
            let (_, rest, _) = settled(&s);
            assert_eq!(drawn_kitty(&rest, &at, inks), art(at.art.open), "settled");
        }
    }

    /// The kitty is left out when it does not fit the box (its columns run
    /// into the right border's stroke), and the border, the trail and the
    /// stars stay: the card loses its cat, not its decoration.
    #[test]
    fn a_kitty_that_does_not_fit_is_dropped_and_the_rest_stays() {
        let fits = scene();
        // The kitty needs six columns from 40; the box ends at column 44.
        let tight = CardScene { cols: 44, ..fits };
        let g = Geo::new(&tight).expect("a scene");
        assert!(kitty_rect(&g, &tight).is_none(), "the kitty is dropped");
        assert!(kitty_rect(&Geo::new(&fits).expect("a scene"), &fits).is_some());
        for dark in [true, false] {
            let s = CardScene { dark, ..tight };
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            for at in [100, EPISODE_MS + 1] {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                // On light the stars are opaque print too: a kitty pixel is
                // an opaque quad in none of their inks.
                let marks = mark_inks(s.palette);
                assert!(
                    !over
                        .iter()
                        .any(|q| q.alpha == 255 && (dark || !marks.contains(&q.color))),
                    "dark {dark}, {at} ms: no kitty pixel"
                );
                assert!(
                    under.iter().any(|q| on_rim(q, &s)),
                    "dark {dark}: the border"
                );
                assert!(
                    under.iter().any(|q| !on_rim(q, &s)),
                    "dark {dark}: the trail"
                );
                assert!(!over.is_empty(), "dark {dark}, {at} ms: the stars");
                for q in under.iter().chain(&over) {
                    assert_in_band_and_box(q, &s, &format!("dark {dark}, {at} ms"));
                }
            }
        }
    }

    /// The nearest colour on `ramp` (sampled finely, each sample through
    /// `paint`) to `color`, as the largest per-channel gap.
    fn gap_to_ramp(color: u32, ramp: impl Fn(f32) -> u32) -> u32 {
        (0..=2000)
            .map(|i| {
                let on = ramp(i as f32 / 2000.0);
                [16, 8, 0]
                    .iter()
                    .map(|sh| ((on >> sh) & 0xff).abs_diff((color >> sh) & 0xff))
                    .max()
                    .unwrap_or(0)
            })
            .min()
            .unwrap_or(u32::MAX)
    }

    /// Land's border and trail wear the sunset: every settled border colour
    /// is a point on its ramp at the border's coverage, every trail stripe
    /// its stop at the trail's (red at the top through gold to rose at the
    /// bottom) — [`SUNSET`] as light on dark, [`PRINT_LAND`]'s opaque inks on
    /// light (the border's held under its line bar, [`print_line`]) — and
    /// Cruise's are not on it (the negative control: the spectrum's greens
    /// and blues are off the ramp).
    #[test]
    fn land_wears_the_sunset_ramp() {
        for dark in [true, false] {
            let land = CardScene {
                palette: CardPalette::Land,
                dark,
                ..scene()
            };
            // The colour a settled quad end carries for ramp colour `rgb`.
            let border = |t: f32| {
                if dark {
                    light(sunset(t), f32::from(BORDER_COV)).color
                } else {
                    ink(print_line(even_ramp(&PRINT_LAND, t)), PRINT_ALPHA).color
                }
            };
            let stripe = |rgb: u32| {
                if dark {
                    light(rgb, f32::from(TRAIL_COV)).color
                } else {
                    ink(rgb, PRINT_ALPHA).color
                }
            };
            let (under, _, _) = settled(&land);
            let (rim, trail): (Vec<&GlowQuad>, Vec<&GlowQuad>) =
                under.iter().partition(|q| on_rim(q, &land));
            assert!(!rim.is_empty() && !trail.is_empty(), "dark {dark}");
            for q in &rim {
                for c in [q.color, q.color2] {
                    let gap = gap_to_ramp(c, border);
                    assert!(
                        gap <= 2,
                        "dark {dark}: border {c:06x} off the sunset by {gap}"
                    );
                }
            }
            let stops: Vec<u32> = (0..TRAIL_STOPS.len())
                .map(|k| {
                    stripe(if dark {
                        sunset(k as f32 / (TRAIL_STOPS.len() - 1) as f32)
                    } else {
                        PRINT_LAND[k]
                    })
                })
                .collect();
            let worn: std::collections::BTreeSet<u32> =
                trail.iter().flat_map(|q| [q.color, q.color2]).collect();
            assert_eq!(
                worn,
                stops.iter().copied().collect(),
                "dark {dark}: the trail is the ramp's six stops"
            );
            let (red, rose) = if dark {
                (SUNSET[0], SUNSET[3])
            } else {
                (PRINT_LAND[0], PRINT_LAND[5])
            };
            assert_eq!(stops[0], stripe(red), "red on top");
            assert_eq!(stops[5], stripe(rose), "rose at the bottom");
            // The negative control: Cruise's border leaves the ramp.
            let cruise = CardScene {
                palette: CardPalette::Cruise,
                ..land
            };
            let (under, _, _) = settled(&cruise);
            let off = under
                .iter()
                .filter(|q| on_rim(q, &cruise))
                .map(|q| gap_to_ramp(q.color, border))
                .max()
                .unwrap_or(0);
            assert!(
                off > 40,
                "dark {dark}: Cruise's border is not a sunset ({off})"
            );
        }
    }

    /// A LIGHT GROUND PRINTS THE RAINBOW: on a light theme the settled trail
    /// is Cruise's six print inks top to bottom, one colour a stripe, and the
    /// trail, the border and the rim are opaque — card art, off the shared
    /// light-ink law; mid-shimmer every stripe is its ink lifted toward white
    /// (a gloss) and still opaque, never the paper showing through, the top
    /// and bottom stripes by at most [`LIGHT_GLINT_EDGE`] of the gloss.
    /// CONTROL: the leading ink's red, which washed the trail out, is not a
    /// red it prints.
    #[test]
    fn a_light_ground_prints_the_rainbow() {
        let mid = Clock {
            t_ms: Some(1500 + 750),
            ride_ms: None,
        };
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                dark: false,
                kitty_rows,
                ..scene()
            };
            let rest = parts(&s, Clock::SETTLED);
            assert_eq!(
                stripes_top_down(&rest.trail),
                PRINT_CRUISE.to_vec(),
                "{kitty_rows} rows: red on top"
            );
            for q in rest.trail.iter().chain(&rest.border).chain(&rest.rim) {
                assert_eq!((q.alpha, q.alpha2), (255, 255), "{kitty_rows} rows: {q:?}");
            }
            assert!(
                !PRINT_CRUISE.contains(&InkRole::Leading.ink(spectrum_stop(0))),
                "the light-ink law's red is not printed"
            );
            let gloss = parts(&s, mid).trail;
            let mut lifted = [0.0f32; 6];
            for q in &gloss {
                assert_eq!((q.alpha, q.alpha2), (255, 255), "{kitty_rows} rows: {q:?}");
                for c in [q.color, q.color2] {
                    // The ink `c` is a gloss of, and how far toward white.
                    let (gap, k, t) = (0..PRINT_CRUISE.len())
                        .flat_map(|k| (0..=100).map(move |i| (k, i as f32 / 100.0)))
                        .map(|(k, t)| (channel_gap(c, mix_rgb(PRINT_CRUISE[k], WHITE, t)), k, t))
                        .min_by(|a, b| a.0.cmp(&b.0).then(a.2.total_cmp(&b.2)))
                        .expect("inks");
                    assert!(gap <= 2, "{kitty_rows} rows: {c:06x} not an ink's gloss");
                    lifted[k] = lifted[k].max(t);
                }
            }
            let middle = lifted[1..5].iter().copied().fold(0.0, f32::max);
            assert!(
                middle > 0.3,
                "{kitty_rows} rows: the gloss runs ({lifted:?})"
            );
            for edge in [lifted[0], lifted[5]] {
                assert!(
                    edge <= SHIMMER_GAIN * LIGHT_GLINT_EDGE + 0.02 && edge > 0.0,
                    "{kitty_rows} rows: an edge stripe glossed {edge} ({lifted:?})"
                );
            }
        }
    }

    /// The largest per-channel gap between two colours.
    fn channel_gap(a: u32, b: u32) -> u32 {
        [16, 8, 0]
            .iter()
            .map(|sh| ((a >> sh) & 0xff).abs_diff((b >> sh) & 0xff))
            .max()
            .unwrap_or(0)
    }

    /// A settled trail's stripe colours top to bottom, one a stripe.
    fn stripes_top_down(trail: &[GlowQuad]) -> Vec<u32> {
        let mut trail = trail.to_vec();
        trail.sort_by_key(|q| (q.y, q.x));
        let mut worn: Vec<u32> = trail.iter().map(|q| q.color).collect();
        worn.dedup();
        worn
    }

    /// The vividness floor the light trail's stripes are held to
    /// (§10.6.6): HSV saturation, as composited on white paper.
    const PRINT_MIN_SATURATION: f64 = 0.6;

    /// ... and OKLCh chroma.
    const PRINT_MIN_CHROMA: f64 = 0.16;

    /// The least OKLab distance between any two of a palette's stripes.
    const PRINT_MIN_STRIPE_DE: f64 = 0.05;

    /// The least WCAG contrast a stripe keeps against the white paper it is
    /// composited on, so no stripe vanishes into the page: the gold is
    /// 1.62:1, lemon 1.07:1 (§10.6.6).
    const PRINT_MIN_PAPER_CONTRAST: f32 = 1.4;

    /// The light trail's vividness law for one palette's six stripes, each a
    /// quad end's `(premultiplied colour, alpha)` top to bottom: every stripe
    /// opaque, and on white paper at least [`PRINT_MIN_SATURATION`],
    /// [`PRINT_MIN_CHROMA`] and [`PRINT_MIN_PAPER_CONTRAST`] against the
    /// paper; no two stripes within [`PRINT_MIN_STRIPE_DE`];
    /// and the hues in the palette's order — Cruise's OKLCh hue rising red to
    /// violet, Land's rising red to the gold horizon (the fourth stripe), then
    /// falling through the coral to the rose. Every breach, empty when it
    /// holds.
    fn vivid_on_paper(palette: CardPalette, stripes: &[(u32, u8)]) -> Vec<String> {
        use aterm_types::Rgb;
        use aterm_types::scheme_signature::{chroma_hue, oklab_distance};
        if stripes.len() != TRAIL_STOPS.len() {
            return vec![format!("{} stripes", stripes.len())];
        }
        let mut breaches = Vec::new();
        // On white: premultiplied ink plus the paper it lets through.
        let paper = |(c, a): (u32, u8)| {
            let ch = |sh: u32| (((c >> sh) & 0xff) + 255 - u32::from(a)).min(255) as u8;
            Rgb::new(ch(16), ch(8), ch(0))
        };
        let on: Vec<Rgb> = stripes.iter().map(|&s| paper(s)).collect();
        let mut hues = Vec::new();
        for (k, (&(c, a), rgb)) in stripes.iter().zip(&on).enumerate() {
            if a != 255 {
                breaches.push(format!("stripe {k} ({c:06x}) is not opaque: alpha {a}"));
            }
            let px = (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b);
            let (_, sat, _) = crate::spectrum::spectrum_hsv(px);
            let (chroma, hue) = chroma_hue(*rgb);
            if sat < PRINT_MIN_SATURATION || chroma < PRINT_MIN_CHROMA {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) is washed out: saturation {sat:.2}, chroma {chroma:.3}"
                ));
            }
            let contrast = 1.05 / (relative_luminance(px) + 0.05);
            if contrast < PRINT_MIN_PAPER_CONTRAST {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) vanishes on the paper: {contrast:.2}:1"
                ));
            }
            hues.push(hue);
        }
        for i in 0..on.len() {
            for j in i + 1..on.len() {
                let d = oklab_distance(on[i], on[j]);
                if d < PRINT_MIN_STRIPE_DE {
                    breaches.push(format!("stripes {i} and {j} are one colour (dE {d:.3})"));
                }
            }
        }
        // Hue steps, unwrapped to [-180, 180).
        let step = |a: f64, b: f64| (b - a + 540.0).rem_euclid(360.0) - 180.0;
        let peak = match palette {
            CardPalette::Cruise => hues.len() - 1,
            CardPalette::Land => 3,
        };
        for k in 1..hues.len() {
            if (k <= peak) != (step(hues[k - 1], hues[k]) > 0.0) {
                breaches.push(format!("stripe {k}'s hue is out of order: {hues:.1?}"));
            }
        }
        breaches
    }

    /// Whether `breaches` holds one that says `what`.
    fn breached(breaches: &[String], what: &str) -> bool {
        breaches.iter().any(|b| b.contains(what))
    }

    /// THE LIGHT TRAIL IS VIVID: at both heights, for both palettes, the
    /// settled light trail passes [`vivid_on_paper`] — opaque, saturated,
    /// distinct, in order. NEGATIVE CONTROLS, each caught for its own
    /// reason: main's washed-out recipe (the leading light ink at 212/255,
    /// for both palettes) is translucent and washed out, and the leading ink
    /// alone, laid opaque, is still washed out; the print inks at main's
    /// 212/255 let the paper through; the sunset ramp laid in opaque ink
    /// repeats its orange; Cruise's inks upside down are out of order; and
    /// lemon in the gold's place, saturated and in order, vanishes on the
    /// paper.
    #[test]
    fn the_light_trail_is_vivid_print() {
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            for kitty_rows in [2u8, 3] {
                let s = CardScene {
                    dark: false,
                    palette,
                    kitty_rows,
                    ..scene()
                };
                let trail = parts(&s, Clock::SETTLED).trail;
                let mut ends: Vec<(u16, u32, u8)> =
                    trail.iter().map(|q| (q.y, q.color, q.alpha)).collect();
                ends.sort_unstable();
                ends.dedup_by_key(|e| (e.1, e.2));
                let stripes: Vec<(u32, u8)> = ends.iter().map(|e| (e.1, e.2)).collect();
                assert_eq!(
                    vivid_on_paper(palette, &stripes),
                    Vec::<String>::new(),
                    "{palette:?}, {kitty_rows} rows"
                );
            }
        }
        // The controls.
        const MAIN_TRAIL_LIGHT_ALPHA: f32 = 212.0;
        let lay = |rgb: u32, alpha: f32| {
            let p = ink(rgb, alpha);
            (p.color, p.alpha)
        };
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let leading = |alpha: f32| -> Vec<(u32, u8)> {
                (0..TRAIL_STOPS.len())
                    .map(|k| lay(InkRole::Leading.ink(palette.stripe(k, true)), alpha))
                    .collect()
            };
            let main = vivid_on_paper(palette, &leading(MAIN_TRAIL_LIGHT_ALPHA));
            assert!(breached(&main, "not opaque"), "{palette:?}: {main:?}");
            assert!(breached(&main, "washed out"), "{palette:?}: {main:?}");
            let opaque = vivid_on_paper(palette, &leading(PRINT_ALPHA));
            assert!(breached(&opaque, "washed out"), "{palette:?}: {opaque:?}");
            assert!(!breached(&opaque, "not opaque"), "{palette:?}: {opaque:?}");
        }
        let thin: Vec<(u32, u8)> = PRINT_CRUISE
            .iter()
            .map(|&c| lay(c, MAIN_TRAIL_LIGHT_ALPHA))
            .collect();
        let thin = vivid_on_paper(CardPalette::Cruise, &thin);
        assert!(breached(&thin, "not opaque"), "{thin:?}");
        let ramp: Vec<(u32, u8)> = (0..TRAIL_STOPS.len())
            .map(|k| lay(sunset(k as f32 / 5.0), PRINT_ALPHA))
            .collect();
        let ramp = vivid_on_paper(CardPalette::Land, &ramp);
        assert!(breached(&ramp, "2 and 4 are one colour"), "{ramp:?}");
        let flipped: Vec<(u32, u8)> = PRINT_CRUISE
            .iter()
            .rev()
            .map(|&c| lay(c, PRINT_ALPHA))
            .collect();
        let flipped = vivid_on_paper(CardPalette::Cruise, &flipped);
        assert_eq!(
            flipped
                .iter()
                .map(|b| b.contains("out of order"))
                .collect::<Vec<_>>(),
            vec![true; 5],
            "{flipped:?}"
        );
        const LEMON: u32 = 0x00FF_FF00;
        let lemon: Vec<(u32, u8)> = (0..PRINT_CRUISE.len())
            .map(|k| lay(if k == 2 { LEMON } else { PRINT_CRUISE[k] }, PRINT_ALPHA))
            .collect();
        let lemon = vivid_on_paper(CardPalette::Cruise, &lemon);
        assert!(
            lemon.len() == 1 && lemon[0].starts_with("stripe 2 (ffff00) vanishes"),
            "{lemon:?}"
        );
    }

    /// The light border and rim stay bright all the way round, and the
    /// one-pixel border reads on white: every point of Cruise's light ramp
    /// ([`PRINT_CRUISE_RAMP`]) keeps OKLCh chroma 0.11 or more; every settled
    /// light border quad end, both palettes, both heights, is a point on its
    /// palette's print ramp ([`CardPalette::art_at`]) held under the line
    /// bar ([`print_line`]), and every border quad end, settled or with the
    /// head passing, is under the bar ([`LIGHT_LINE_MAX_LUMA`]) while the rim
    /// keeps the full ink, above it at the gold; and the bar only darkens (a
    /// colour under it is untouched, the gold keeps its hue). NEGATIVE
    /// CONTROLS: the six inks mixed straight, without the peridot and the
    /// teal, dip under the chroma floor between green and blue; and main's
    /// border colour, the leading light ink of the spectrum, laid opaque, is
    /// off Cruise's print ramp (it sits under the bar, so only the ramp check
    /// catches it).
    #[test]
    fn the_light_border_keeps_its_chroma_and_reads_on_white() {
        use aterm_types::Rgb;
        use aterm_types::scheme_signature::chroma_hue;
        const RAMP_MIN_CHROMA: f64 = 0.11;
        let chroma = |c: u32| chroma_hue(Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8)).0;
        let n = SPECTRUM_LUT_LEN - 1;
        let low = (0..=n)
            .map(|i| chroma(CardPalette::Cruise.art_at(i as f32 / n as f32, false)))
            .fold(f64::MAX, f64::min);
        assert!(
            low >= RAMP_MIN_CHROMA,
            "the light ramp dips to chroma {low:.3}"
        );
        let plain: Vec<(usize, u32)> = (0..PRINT_CRUISE.len())
            .map(|k| (stripe_at(k), PRINT_CRUISE[k]))
            .collect();
        let plain_low = (0..=n)
            .map(|i| chroma(knot_ramp(&plain, i as f32 / n as f32)))
            .fold(f64::MAX, f64::min);
        assert!(
            plain_low < RAMP_MIN_CHROMA,
            "CONTROL: without the knots the ramp holds {plain_low:.3}"
        );
        let luma = |c: u32| {
            0.2126 * ((c >> 16) & 0xff) as f32
                + 0.7152 * ((c >> 8) & 0xff) as f32
                + 0.0722 * (c & 0xff) as f32
        };
        // The colour a settled light border quad end carries at ramp point t.
        let on_print = |palette: CardPalette| {
            move |t: f32| ink(print_line(palette.art_at(t, false)), PRINT_ALPHA).color
        };
        let washed = (0..=n)
            .map(|i| {
                let main = InkRole::Leading.ink(CardPalette::Cruise.at(i as f32 / n as f32));
                (
                    gap_to_ramp(ink(main, PRINT_ALPHA).color, on_print(CardPalette::Cruise)),
                    luma(main),
                )
            })
            .fold((0, 0.0), |(g, y), (g1, y1)| (g.max(g1), f32::max(y, y1)));
        assert!(
            washed.0 > 40 && washed.1 <= LIGHT_LINE_MAX_LUMA,
            "CONTROL: main's border ink, opaque, is {} off the print ramp at luma {}",
            washed.0,
            washed.1
        );
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            for kitty_rows in [2u8, 3] {
                let s = CardScene {
                    dark: false,
                    palette,
                    kitty_rows,
                    ..scene()
                };
                let border = parts(&s, Clock::SETTLED).border;
                assert!(!border.is_empty(), "{palette:?} {kitty_rows} rows");
                for q in &border {
                    for c in [q.color, q.color2] {
                        let gap = gap_to_ramp(c, on_print(palette));
                        assert!(
                            gap <= 2,
                            "{palette:?} {kitty_rows} rows: border {c:06x} off its print ramp by {gap}"
                        );
                    }
                }
                for clock in [
                    Clock::SETTLED,
                    Clock {
                        t_ms: Some(900),
                        ride_ms: None,
                    },
                ] {
                    let p = parts(&s, clock);
                    let line = p
                        .border
                        .iter()
                        .flat_map(|q| [q.color, q.color2])
                        .map(luma)
                        .fold(0.0, f32::max);
                    assert!(
                        line <= LIGHT_LINE_MAX_LUMA + 1.0,
                        "{palette:?} {kitty_rows} rows: the border's luma reaches {line}"
                    );
                }
                let rim = parts(&s, Clock::SETTLED)
                    .rim
                    .iter()
                    .map(|q| luma(q.color))
                    .fold(0.0, f32::max);
                if palette == CardPalette::Cruise {
                    assert!(
                        rim > LIGHT_LINE_MAX_LUMA + 1.0,
                        "{kitty_rows} rows: the rim keeps the full gold ({rim})"
                    );
                }
            }
        }
        let gold = PRINT_CRUISE[2];
        let held = print_line(gold);
        assert!(luma(gold) > LIGHT_LINE_MAX_LUMA && luma(held) <= LIGHT_LINE_MAX_LUMA + 1.0);
        assert!(
            luma(held) >= LIGHT_LINE_MAX_LUMA - 1.0,
            "darkened to the bar, no further"
        );
        let hue = |c: u32| chroma_hue(Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8)).1;
        assert!(
            (hue(held) - hue(gold)).abs() < 2.0,
            "the gold keeps its hue"
        );
        assert_eq!(
            print_line(PRINT_CRUISE[4]),
            PRINT_CRUISE[4],
            "blue is under the bar"
        );
    }

    /// The scene's clip (the pane) bounds every quad: a box larger than its
    /// clip loses what lies outside — the right border's column, the left
    /// padding, the line row's lower half — and keeps the rest; a border
    /// segment cut by the clip keeps its colour where it starts and ends on
    /// the ramp's colour where it is cut; a clip that misses the box draws
    /// nothing at all.
    #[test]
    fn a_box_larger_than_its_clip_is_cut_to_it() {
        let whole = scene();
        let cut = clipped();
        let (cx0, cy0, cx1, cy1) = cut.clip;
        let t0 = Instant::now();
        for at in [0, 150, 1300, EPISODE_MS + 1] {
            let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
            a.arm(t0, 7);
            b.arm(t0, 7);
            let (wu, wo, _) = frame(&mut a, t0 + ms(at), &whole, true);
            let (cu, co, fp) = frame(&mut b, t0 + ms(at), &cut, true);
            assert_ne!(fp, 0, "{at} ms: the pane's part is drawn");
            for q in cu.iter().chain(&co) {
                assert_in_band_and_box(q, &cut, &format!("{at} ms"));
            }
            // The kitty is whole inside the clip: the same art either way.
            let kitty = |v: &[GlowQuad]| -> Vec<GlowQuad> {
                v.iter().filter(|q| q.alpha == 255).copied().collect()
            };
            assert!(!kitty(&co).is_empty(), "{at} ms: the kitty fits the clip");
            assert_eq!(kitty(&wo), kitty(&co), "{at} ms");
            // What the whole box drew inside the clip, the cut one drew too.
            let inside = |q: &GlowQuad| {
                i32::from(q.x) >= cx0
                    && i32::from(q.x) + i32::from(q.w) <= cx1
                    && i32::from(q.y) >= cy0
                    && i32::from(q.y) + i32::from(q.h) <= cy1
            };
            for q in wu.iter().filter(|q| inside(q)) {
                assert!(cu.contains(q), "{at} ms: {q:?} lost inside the clip");
            }
            assert!(
                wu.iter().any(|q| !inside(q)),
                "{at} ms: the whole box reaches past the clip"
            );
        }
        // The right border's column lies past the clip: no quad reaches it.
        let g = Geo::new(&whole).expect("a scene");
        let (cu, _, _) = settled(&cut);
        assert!(
            cu.iter()
                .all(|q| i64::from(q.x) + i64::from(q.w) < g.outer.x1 - g.t)
        );
        // The top edge's segment the clip cuts: its left end as drawn whole,
        // its right end between the whole segment's two ends.
        let (wu, _, _) = settled(&whole);
        let top = g.outer.y0;
        let edge = |q: &&GlowQuad| i64::from(q.y) == top && i32::from(q.x) < cx1;
        let cut_seg = cu
            .iter()
            .filter(edge)
            .find(|q| i32::from(q.x) + i32::from(q.w) == cx1)
            .expect("a top segment cut at the clip");
        let whole_seg = wu
            .iter()
            .filter(edge)
            .find(|q| q.x == cut_seg.x && i32::from(q.x) + i32::from(q.w) > cx1)
            .expect("the same segment drawn whole");
        assert_eq!(cut_seg.color, whole_seg.color, "the left end stays");
        for sh in [16, 8, 0] {
            let ch = |c: u32| (c >> sh) & 0xff;
            let (a, b) = (ch(whole_seg.color), ch(whole_seg.color2));
            let at = ch(cut_seg.color2);
            assert!(
                a.min(b) <= at && at <= a.max(b),
                "the cut end {:06x} is on the ramp {:06x} → {:06x}",
                cut_seg.color2,
                whole_seg.color,
                whole_seg.color2
            );
        }
        // A clip that misses the box, and an inverted one, draw nothing.
        for clip in [(0, 0, 5, 5), (cx1, cy0, cx0, cy1), (0, 0, 0, 0)] {
            let s = CardScene { clip, ..whole };
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            assert_eq!(
                frame(&mut card, t0 + ms(100), &s, true),
                (Vec::new(), Vec::new(), 0),
                "{clip:?}"
            );
        }
    }

    /// How many times each device pixel is covered by `quads`.
    fn coverage(quads: &[GlowQuad]) -> std::collections::BTreeMap<(i64, i64), usize> {
        let mut seen = std::collections::BTreeMap::new();
        for q in quads {
            for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
                for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                    *seen.entry((x, y)).or_insert(0) += 1;
                }
            }
        }
        seen
    }

    /// Whether `q` covers device pixel `(x, y)`.
    fn covers(q: &GlowQuad, (x, y): (i64, i64)) -> bool {
        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
        (x0..x0 + i64::from(q.w)).contains(&x) && (y0..y0 + i64::from(q.h)).contains(&y)
    }

    /// Whether `a` and `b` cover a device pixel in common: the meet of their
    /// [`coverage`], read off the two rectangles.
    fn overlap(a: &GlowQuad, b: &GlowQuad) -> bool {
        let meet = |p0: u16, n0: u16, p1: u16, n1: u16| {
            let (lo, hi) = (
                p0.max(p1),
                (u32::from(p0) + u32::from(n0)).min(u32::from(p1) + u32::from(n1)),
            );
            u32::from(lo) < hi
        };
        meet(a.x, a.w, b.x, b.w) && meet(a.y, a.h, b.y, b.h)
    }

    /// The device pixels a rim `rim` wide round `frame` drawn at `at` should
    /// light: those outside the silhouette (the art, its toes' gaps closed)
    /// within `rim` of it (every direction, corners included), inside
    /// `inside` — but none in a toe's gap's columns above or below it, where
    /// the gap opens onto the ground.
    fn rim_ring(
        at: &KittyAt,
        frame: &[&str],
        rim: i64,
        inside: Rect,
    ) -> std::collections::BTreeSet<(i64, i64)> {
        let p = at.p;
        let cell = |x: i64, y: i64| {
            let (dx, dy) = (x - at.rect.x0, y - at.rect.y0);
            (dx >= 0 && dy >= 0).then(|| (dy / p, dx / p))
        };
        let solid = |x: i64, y: i64| cell(x, y).is_some_and(|(r, c)| rim_solid(frame, r, c));
        let notch = |r: i64, c: i64| rim_solid(frame, r, c) && !art_at(frame, r, c);
        let by_a_notch = |x: i64, y: i64| {
            let c = (x - at.rect.x0).div_euclid(p);
            let r = (y - at.rect.y0).div_euclid(p);
            notch(r - 1, c) || notch(r + 1, c)
        };
        let mut ring = std::collections::BTreeSet::new();
        for y in at.rect.y0 - rim..at.rect.y1 + rim {
            for x in at.rect.x0 - rim..at.rect.x1 + rim {
                if solid(x, y) || !inside.contains(x, y) || by_a_notch(x, y) {
                    continue;
                }
                let near = (-rim..=rim).any(|dy| (-rim..=rim).any(|dx| solid(x + dx, y + dy)));
                if near {
                    ring.insert((x, y));
                }
            }
        }
        ring
    }

    /// Whether `quads` light exactly `ring`, each pixel once.
    fn rings_once(
        quads: &[GlowQuad],
        ring: &std::collections::BTreeSet<(i64, i64)>,
    ) -> Result<(), String> {
        let lit = coverage(quads);
        if let Some((px, n)) = lit.iter().find(|(_, n)| **n > 1) {
            return Err(format!("{px:?} lit {n} times"));
        }
        let lit: std::collections::BTreeSet<_> = lit.into_keys().collect();
        if let Some(px) = lit.difference(ring).next() {
            return Err(format!("{px:?} lit off the ring"));
        }
        if let Some(px) = ring.difference(&lit).next() {
            return Err(format!("{px:?} on the ring left dark"));
        }
        Ok(())
    }

    /// THE RIM RINGS THE SILHOUETTE ONCE (§10.6): at both heights, on both
    /// themes, in every frame of the art (open, blink, flick) and at cell
    /// sizes from a 1 px rim to a 3 px one, the rim lights exactly the
    /// pixels outside the kitty within [`rim_px`] of it — its outer corners
    /// closed, its inner corners not doubled, its toes' gaps open
    /// ([`rim_ring`]) — each once, inside the box within the stroke and off
    /// the trail; in the border's spectrum (more than three hues,
    /// each on the palette's ramp), additive on dark and opaque print ink on
    /// light; and none at one-pixel art. NEGATIVE CONTROL: the check catches
    /// a rim with a strip left out (a dark pixel on the ring) and one with a
    /// strip drawn twice (a pixel lit twice).
    #[test]
    fn the_rim_rings_the_silhouette_once() {
        let mut checked = 0;
        let mut on_trail = 0;
        for (cw, ch) in [(9u16, 18u16), (8, 16), (16, 33), (17, 35), (20, 44)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    let s = CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        kitty_col: Some(44),
                        trail: Some((32, 44)),
                        ..scene()
                    };
                    let g = Geo::new(&s).expect("a scene");
                    let at = kitty_rect(&g, &s).expect("a kitty");
                    let rim = rim_px(at.p, g.t);
                    assert!(rim >= 1, "{ch} px: a rim");
                    for (name, t_ms) in [
                        ("open", None),
                        ("blink", Some(BLINK_AT_MS + 10)),
                        ("flick", Some(FLICK_AT_MS + 10)),
                    ] {
                        let clock = Clock {
                            t_ms,
                            ride_ms: t_ms,
                        };
                        let frame = kitty_frame(&at.art, clock);
                        // The first flick falls on the ride.
                        let at = ridden(&g, &s, at, clock);
                        let drawn = parts(&s, clock);
                        let quads = drawn.rim;
                        let mut ring = rim_ring(&at, frame, rim, g.inner().meet(g.clip));
                        let trail = coverage(&drawn.trail);
                        let before = ring.len();
                        ring.retain(|px| !trail.contains_key(px));
                        on_trail += before - ring.len();
                        let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}, {name}");
                        rings_once(&quads, &ring).unwrap_or_else(|e| panic!("{what}: {e}"));
                        for q in &quads {
                            assert_in_band_and_box(q, &s, &what);
                            if dark {
                                assert_eq!((q.alpha, q.alpha2), (0, 0), "{what}: light");
                            } else {
                                assert_eq!((q.alpha, q.alpha2), (255, 255), "{what}: print {q:?}");
                            }
                        }
                        let hues: std::collections::BTreeSet<u32> =
                            quads.iter().map(|q| q.color).collect();
                        assert!(hues.len() > 3, "{what}: a spectrum, {} hues", hues.len());
                        for c in &hues {
                            let gap = gap_to_ramp(*c, |t| {
                                if dark {
                                    light(spectrum(t), f32::from(RIM_COV)).color
                                } else {
                                    ink(CardPalette::Cruise.art_at(t, false), PRINT_ALPHA).color
                                }
                            });
                            assert!(gap <= 2, "{what}: {c:06x} off the spectrum by {gap}");
                        }
                        // NEGATIVE CONTROL: a strip short, a strip twice.
                        let mut short = quads.clone();
                        short.pop();
                        assert!(rings_once(&short, &ring).is_err(), "{what}: CONTROL short");
                        let mut twice = quads.clone();
                        twice.push(quads[0]);
                        assert!(rings_once(&twice, &ring).is_err(), "{what}: CONTROL twice");
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 5 * 2 * 2 * 3);
        assert!(on_trail > 0, "the tail's ring meets the trail somewhere");
        // One-pixel art wears no rim.
        let tiny = CardScene {
            cw: 8,
            ch: 14,
            ..scene()
        };
        assert_eq!(kitty_px(14), 1);
        assert!(
            parts(&tiny, Clock::SETTLED).rim.is_empty(),
            "no rim at one-pixel art"
        );
    }

    /// THE RIM NEVER LIES ON THE TRAIL (review of slice 5b): where the tail
    /// meets the rainbow — swept straight back in the flick, its tip's outer
    /// corner when settled — the rim is cut, so no pixel holds both (on dark
    /// both are light, and light on light doubles to white). Every frame of
    /// a riding episode and of a seated one (armed again), and settled, at
    /// both heights, on both grounds, at three cell sizes. NEGATIVE CONTROL:
    /// the uncut ring meets the trail in the flick at every size and height.
    #[test]
    fn the_rim_never_lies_on_the_trail() {
        for (cw, ch) in [(9u16, 18u16), (16, 33), (20, 44)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    let s = CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        kitty_col: Some(44),
                        trail: Some((32, 44)),
                        ..scene()
                    };
                    let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}");
                    let clocks = (0..EPISODE_MS)
                        .step_by(FRAME_MS as usize)
                        .flat_map(|t| [Clock::riding(t), Clock::seated(t)])
                        .chain([Clock::SETTLED]);
                    for clock in clocks {
                        let p = parts(&s, clock);
                        let both = p
                            .rim
                            .iter()
                            .flat_map(|r| p.trail.iter().map(move |t| (r, t)))
                            .find(|(r, t)| overlap(r, t));
                        assert_eq!(both, None, "{what}: {clock:?}");
                    }
                    // CONTROL: uncut, the flicked tail's ring lies on it.
                    let g = Geo::new(&s).expect("a scene");
                    let seat = kitty_rect(&g, &s).expect("a kitty");
                    let flick = Clock::seated(FLICK_AT_MS + 10);
                    let ring = rim_ring(
                        &seat,
                        kitty_frame(&seat.art, flick),
                        rim_px(seat.p, g.t),
                        g.inner(),
                    );
                    let trail = parts(&s, flick).trail;
                    assert!(
                        ring.iter().any(|&px| trail.iter().any(|q| covers(q, px))),
                        "{what}: CONTROL"
                    );
                }
            }
        }
    }

    /// THE TOES' GAPS OPEN ONTO THE GROUND (review of slice 5a): the rim runs
    /// under each paw and stops at a toe's gap, lining neither its sides nor
    /// its foot, so on a light ground the gap reads as the ground between
    /// paws, not as a hole boxed in by a coloured line. At both heights, on
    /// both grounds, in every frame, at three cell sizes — the tall art's
    /// one-pixel gaps (between its legs, the front legs' two rows deep) and
    /// the two-row art's one, between the front paws under its chin.
    /// NEGATIVE CONTROL: the pixels under the paws beside each gap, where
    /// the paw's foot is on the ground, are lit.
    #[test]
    fn the_toes_gaps_open_onto_the_ground() {
        let (mut gaps, mut paws) = (0, 0);
        for (cw, ch) in [(9u16, 18u16), (16, 33), (20, 44)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    let s = CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        ..scene()
                    };
                    let g = Geo::new(&s).expect("a scene");
                    let seat = kitty_rect(&g, &s).expect("a kitty");
                    let (p, rim) = (seat.p, rim_px(seat.p, g.t));
                    for clock in [
                        Clock::SETTLED,
                        Clock::seated(BLINK_AT_MS + 10),
                        Clock::seated(FLICK_AT_MS + 10),
                    ] {
                        let frame = kitty_frame(&seat.art, clock);
                        let lit = coverage(&parts(&s, clock).rim);
                        let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}");
                        let under = |r: i64, c: i64| {
                            let y = seat.rect.y0 + (r + 1) * p;
                            (seat.rect.x0 + c * p..seat.rect.x0 + (c + 1) * p)
                                .flat_map(move |x| (y..y + rim).map(move |y| (x, y)))
                        };
                        for (r, row) in frame.iter().enumerate() {
                            let r = r as i64;
                            for c in 0..row.len() as i64 {
                                if art_at(frame, r, c) || !rim_solid(frame, r, c) {
                                    continue;
                                }
                                gaps += 1;
                                assert!(
                                    under(r, c).all(|px| !lit.contains_key(&px)),
                                    "{what}: the gap at ({r}, {c}) is lined beneath"
                                );
                                // A leg beside a gap two rows deep goes on
                                // down: only a foot wears the rim under it.
                                for side in [c - 1, c + 1] {
                                    if rim_solid(frame, r + 1, side) {
                                        continue;
                                    }
                                    paws += 1;
                                    assert!(
                                        under(r, side).all(|px| lit.contains_key(&px)),
                                        "{what}: CONTROL, the paw at ({r}, {side})"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            gaps > 0 && paws > 0,
            "CONTROL: {gaps} gaps, {paws} paws beside them"
        );
    }

    /// THE TRAIL IS THE KITTY'S BODY'S HEIGHT (§10.6: the cat leads, the
    /// rainbow trails): at both heights, settled and through an episode
    /// past its wave, the trail's stripes span exactly the art's body band
    /// at the kitty's seat — six stripes top to bottom, each one quad end to
    /// end when settled (split only at the row bands it crosses) — from the
    /// trail's start under the cat to its rear ([`KittyAt::rear`]), so on
    /// every pixel row of the band the rainbow meets the art with no ground
    /// between; well under the two rows' height the trail spanned before
    /// (three stripes a band), and inside the kitty's own height. During the
    /// wave the stripes step a few pixels either way, no further, and the
    /// stretch that reaches under the kitty where the ride has it holds still
    /// inside the band, so no stepped stripe shows past the cat's outline.
    /// CONTROL: an episode frame draws the trail in segments, the settled
    /// frame does not.
    #[test]
    fn the_trail_spans_the_kittys_body() {
        for kitty_rows in [2u8, 3] {
            for (cw, ch) in [(9u16, 18u16), (16, 33), (17, 35)] {
                let s = CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    kitty_col: Some(44),
                    trail: Some((32, 44)),
                    ..scene()
                };
                let g = Geo::new(&s).expect("a scene");
                let at = kitty_rect(&g, &s).expect("a kitty");
                let (b0, b1) = at.body();
                let what = format!("{cw}x{ch}, {kitty_rows} rows");
                let settled = parts(&s, Clock::SETTLED).trail;
                let y0 = settled
                    .iter()
                    .map(|q| i64::from(q.y))
                    .min()
                    .expect("a trail");
                let y1 = settled
                    .iter()
                    .map(|q| i64::from(q.y) + i64::from(q.h))
                    .max()
                    .expect("a trail");
                assert_eq!((y0, y1), (b0, b1), "{what}: the body band");
                assert!(
                    at.rect.y0 <= y0 && y1 <= at.rect.y1,
                    "{what}: inside the cat's height"
                );
                assert!(
                    y1 - y0 < 2 * i64::from(ch) * 3 / 4,
                    "{what}: narrower than two rows"
                );
                let x0 = g.col(32);
                assert!(
                    settled
                        .iter()
                        .all(|q| i64::from(q.x) == x0
                            && i64::from(q.x) + i64::from(q.w) == at.rear()),
                    "{what}: each stripe one quad from the trail's start to the kitty's rear"
                );
                for r in at.art.body.0..at.art.body.0 + at.art.body.1 {
                    let first = at.art.open[r].find(|c| c != '.').expect("a body row") as i64;
                    let x = at.rect.x0 + first * at.p - 1;
                    let mut rows = at.rect.y0 + r as i64 * at.p..at.rect.y0 + (r as i64 + 1) * at.p;
                    assert!(
                        rows.all(|y| settled.iter().any(|q| covers(q, (x, y)))),
                        "{what}: ground between the trail and the art's row {r}"
                    );
                }
                let crossings = (g.top + 1..g.top + 3)
                    .filter(|&r| (y0 + 1..y1).contains(&g.band(r)))
                    .count();
                assert!(
                    settled.len() <= TRAIL_STOPS.len() + crossings,
                    "{what}: {} quads, six stripes and {crossings} band crossings",
                    settled.len()
                );
                let colours: std::collections::BTreeSet<u32> =
                    settled.iter().map(|q| q.color).collect();
                assert_eq!(colours.len(), TRAIL_STOPS.len(), "{what}: six stripes");
                for t in [100, 1500, 3000, WAVE_END_MS + 50] {
                    let ep = parts(&s, Clock::riding(t)).trail;
                    assert!(
                        ep.len() > settled.len(),
                        "{what}: CONTROL, segments at {t} ms"
                    );
                    let amp = (i64::from(ch) / 16).max(1);
                    let under_cat = ridden(&g, &s, at, Clock::riding(t)).rect.x0;
                    for q in &ep {
                        assert!(
                            i64::from(q.y) >= b0 - amp
                                && i64::from(q.y) + i64::from(q.h) <= b1 + amp,
                            "{what}: {q:?} strays from the band at {t} ms"
                        );
                        if i64::from(q.x) + i64::from(q.w) > under_cat {
                            assert!(
                                i64::from(q.y) >= b0 && i64::from(q.y) + i64::from(q.h) <= b1,
                                "{what}: {q:?} steps under the kitty at {t} ms"
                            );
                        }
                    }
                }
            }
        }
    }
}
