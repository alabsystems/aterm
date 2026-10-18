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
//!   the boxed rows. Its hue runs along the perimeter, `palette(tri(2s + φ))`
//!   at arc fraction `s` (the doubled `s` spends one whole `tri` period on the
//!   loop, red → violet → red, so the closed perimeter has no seam where it
//!   starts). Top and bottom edges are horizontal gradient segments about a
//!   cell wide; the sides are flat slices, one per row band.
//! * **Rainbow trail** (into `under`): six stripes from the text's end to the
//!   kitty's back, as tall as the kitty's BODY (§10.6: the cat leads, the
//!   rainbow trails), split at the row bands it crosses. Settled, a stripe is
//!   one quad end to end; during an episode it steps and shimmers in
//!   segments.
//! * **Kitty** (into `over`): authored pixel art in the spirit of Claude's block
//!   mascot, [`KITTY_W`] pixels wide with square pixels, each rectangle of one
//!   colour (a run of a pixel row, carried down the rows that repeat it) one
//!   opaque source-over quad. Two heights: [`KITTY_H`] pixels laid two cell
//!   rows tall, and — where the host found the card's third row blank,
//!   [`CardScene::kitty_rows`] — [`KITTY_TALL_H`] pixels laid three rows tall,
//!   the height of Claude's mascot. Three frames each: eyes open, a blink, a
//!   tail flick.
//! * **Rim** (into `over`): a one-pixel outline of the border's spectrum just
//!   outside the kitty's silhouette ([`rim_px`]), so a black or grey coat reads
//!   on a dark ground: additive light on dark, source-over ink on light, its
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
//!   and inside the scene's [`clip`](CardScene::clip) (the pane); the kitty
//!   lies inside the border's stroke, wherever its ride has it; the stars,
//!   the sparkles and the burst stay in the trail/kitty region, never over
//!   the card's text columns (left of the trail's start).
//! * **Budget.** At most [`MAX_CARD_QUADS`] quads per tick across both streams,
//!   spent border → trail → kitty → rim → burst → stars → sparkles (the
//!   burst before the field: a frame over the cap loses a twinkling field
//!   star, never one of the burst's ring); a star is drawn whole or not at
//!   all, to the quad (one the budget cannot finish is taken back).
//! * **Themes.** On a dark theme the border, trail, rim and stars are additive
//!   light (`alpha == 0`); on a light theme every quad is source-over
//!   (`alpha > 0`) through the light-ink law (`cursor_glow`'s `InkRole`), since
//!   additive light is invisible on white. The kitty is opaque art
//!   (`alpha == 255`) on both.
//! * **Photosensitivity.** Every twinkle runs on `effect_util::twinkle_env`,
//!   under the crate's certified 3.2 Hz bound; the trail's step wave cycles
//!   below it too (both const-asserted here). The burst only fades, and the
//!   blink comes once per 2.5 s.

use aterm_render::{GlowQuad, InkCell, premul_rgb};
use aterm_time::Instant;
use std::time::Duration;

use crate::color_math::relative_luminance;
use crate::cursor_glow::InkRole;
use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE_ADD, STAR_GLINT,
    STAR_GLINT_COV, STAR_TAPER_STEPS, TWINKLE_FLASH_BOUND_HZ, TWINKLE_FLASH_HZ, mix_rgb, star_arm,
    star_arm_px, star_body_px, star_core_px, star_taper_cov, star_waist_px, twinkle_env,
    twinkle_peak, twinkle_rgb,
};
use crate::genome::mix;
use crate::rainbow_kitty::meteor::tri;
use crate::spectrum::{spectrum, spectrum_stop};

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
/// The parts: the border ≤ 96 edge segments plus two side slices per boxed
/// row; the trail six stripes settled, ≤ 16 segments of six (each split at
/// the row bands it crosses) during an episode; the kitty's rectangles (about
/// 65, at either height) and its rim (about 45); and up to seventeen stars
/// (the field's nine and the burst's eight: the ride's five sparkles are out
/// before the burst opens).
/// The settled frame — what a card at rest replays into every frame — spends
/// about what main's two-row card did before §10.6 (whose trail was 96
/// segments settled, its kitty 96 runs; three quads more three rows tall on
/// the shortest trail, the rim's strips under the paws), and the busiest
/// frame (the burst opening) stays under the cap with a whole star to spare
/// at the shipping cell sizes, both measured by `the_quad_budget_holds` at
/// both heights. The cap is the backstop for a giant cell or a pathological
/// scene (hundreds of boxed rows), spent in the order border → trail →
/// kitty → rim → burst → stars → sparkles, each star whole or not at all:
/// a frame over it loses a field star, never one of the burst's ring
/// (`the_burst_stays_whole_under_the_cap`).
pub const MAX_CARD_QUADS: usize = 600;

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

/// The light border's opacity at rest (source-over, under the leading role's
/// ceiling).
const BORDER_LIGHT_ALPHA: f32 = 224.0;

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

/// How far the shimmer lifts a stripe toward white (dark) or toward its
/// ceiling opacity (light).
const SHIMMER_GAIN: f32 = 0.45;

/// The dark trail's coverage (additive light).
const TRAIL_COV: u8 = 200;

/// The light trail's opacity (source-over).
const TRAIL_LIGHT_ALPHA: f32 = 212.0;

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
const KITTY_PALETTE: &[u8] = b".ocbenw";

/// The kitty at rest, eyes open, tail up: a flying cat facing right, the
/// rainbow arriving at its tail. `o` outline, `c` coat, `b` belly and muzzle,
/// `e` eye, `n` nose, inner ears and blush.
const KITTY_OPEN: KittyFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccoceecccceeco",
    "..occcccccccoceecccceeco",
    "..occccccccconcbbnnbbcno",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// The blink: [`KITTY_OPEN`] with the eyes shut to a line.
const KITTY_BLINK: KittyFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocccccccccco",
    "..occcccccccocooccccooco",
    "..occccccccconcbbnnbbcno",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// The tail flick: [`KITTY_OPEN`] with the tail swept straight back.
const KITTY_FLICK: KittyFrame = [
    "..............o......o..",
    ".............ono....ono.",
    "............onnoooooonno",
    "............occcccccccco",
    "...oooooooooocccccccccco",
    "ooocccccccccoceecccceeco",
    "ccccccccccccoceecccceeco",
    "oooccccccccconcbbnnbbcno",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// The tall kitty at rest (§10.6): [`KITTY_OPEN`]'s cat stood three rows
/// tall — the same head twelve pixels wide with its pink-lined ears a row
/// taller, eyes a pixel taller with a white catch-light (`w`), the blush,
/// nose and muzzle where the two-row cat has them; a deeper body with the
/// same belly patch, a leg a row longer, and the tail a row longer, up.
const KITTY_TALL_OPEN: KittyTallFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onno....onno",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocewccccewco",
    "..occcccccccoceecccceeco",
    "..occcccccccoceecccceeco",
    "..occccccccconcbbnnbbcno",
    "..occcccccccoccbbbbbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..ocbbbbbbcccccco.......",
    "..occccccccccccco.......",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// The tall blink: [`KITTY_TALL_OPEN`] with the eyes shut to a line across
/// their middle row.
const KITTY_TALL_BLINK: KittyTallFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onno....onno",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocccccccccco",
    "..occcccccccocooccccooco",
    "..occcccccccocccccccccco",
    "..occccccccconcbbnnbbcno",
    "..occcccccccoccbbbbbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..ocbbbbbbcccccco.......",
    "..occccccccccccco.......",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// The tall tail flick: [`KITTY_TALL_OPEN`] with the tail swept straight
/// back at the top of the body, as [`KITTY_FLICK`] sweeps it.
const KITTY_TALL_FLICK: KittyTallFrame = [
    "..............o......o..",
    ".............ono....ono.",
    "............onno....onno",
    "............onnoooooonno",
    "............occcccccccco",
    "............occcccccccco",
    "...oooooooooocccccccccco",
    "ooocccccccccocewccccewco",
    "ccccccccccccoceecccceeco",
    "ooocccccccccoceecccceeco",
    "..occccccccconcbbnnbbcno",
    "..occcccccccoccbbbbbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..ocbbbbbbcccccco.......",
    "..occccccccccccco.......",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// One height of the kitty: its three frames — open, blink, flick — its eye
/// rows (the only rows the blink changes) and its body band, `(first pixel
/// row, pixel rows)` from its back's top outline down: the band the trail's
/// six stripes span (§10.6: the cat leads, the rainbow trails).
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
}

/// The two-row kitty.
const KITTY_SHORT: KittyArt = KittyArt {
    open: &KITTY_OPEN,
    blink: &KITTY_BLINK,
    flick: &KITTY_FLICK,
    eyes: (5, 7),
    body: (4, 7),
};

/// The tall kitty (§10.6).
const KITTY_TALL: KittyArt = KittyArt {
    open: &KITTY_TALL_OPEN,
    blink: &KITTY_TALL_BLINK,
    flick: &KITTY_TALL_FLICK,
    eyes: (7, 10),
    body: (6, 12),
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
/// the open frame with its eyes shut (the eye rows alone change, and no eye
/// pixel is left); the flick is the open frame with its tail moved (the
/// columns left of [`KITTY_TAIL_COLS`] alone change); the open cat has eyes;
/// the body band lies inside the art and holds the trail's six stripes.
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
                && count_in(art.open, art.eyes, b'e') > 0,
            "the blink is the open frame with its eyes shut"
        );
        assert!(
            differ_only_in(art.open, art.flick, (0, h), (0, KITTY_TAIL_COLS)),
            "the flick is the open frame with its tail moved"
        );
        assert!(
            art.body.0 + art.body.1 <= h && art.body.1 >= 6,
            "the body band lies in the art and holds six stripes"
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

/// The dark rim's coverage (additive light), the border's own.
const RIM_COV: u8 = BORDER_COV;

/// The light rim's opacity (source-over), the border's at rest.
const RIM_LIGHT_ALPHA: f32 = BORDER_LIGHT_ALPHA;

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

/// The light theme's star opacity at the peak, before the over-text ceiling.
const STAR_LIGHT_ALPHA: f32 = 220.0;

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
/// How far the sweep pulls a letter toward white (dark) or black (light).
const LINE_SWEEP_GAIN: f32 = 0.55;
/// A dark theme's line ink is lifted to at least this WCAG relative luminance
/// (about 5:1 on a near-black ground), so blue and violet letters read.
const LINE_DARK_LUMA_FLOOR: f32 = 0.30;

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

    /// Trail stripe `k` (0 top … 5 bottom).
    fn stripe(self, k: usize) -> u32 {
        match self {
            Self::Cruise => spectrum_stop(TRAIL_STOPS[k % TRAIL_STOPS.len()]),
            Self::Land => sunset(k as f32 / (TRAIL_STOPS.len() - 1) as f32),
        }
    }

    /// A point mark's colour (a burst star, a light-theme star): a named stop,
    /// never a between colour.
    fn mark(self, k: usize) -> u32 {
        match self {
            Self::Cruise => spectrum_stop(k % 7),
            Self::Land => SUNSET[k % SUNSET.len()],
        }
    }
}

/// The sunset ramp at `t` (`0..=1`; clamps, and a NaN reads as `0`).
fn sunset(t: f32) -> u32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = t * (SUNSET.len() - 1) as f32;
    let i = (x as usize).min(SUNSET.len() - 2);
    mix_rgb(SUNSET[i], SUNSET[i + 1], x - i as f32)
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
    /// read those cells blank.
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
/// top border, its top half bottom-aligned in that row's band: sized from
/// the whole band (`ch / 7`) it would reach into the stroke whenever
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
    /// The border's outer rectangle, unclipped.
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
        let outer = Rect {
            x0: ox - pad,
            y0: oy + top * ch,
            x1: ox + (i64::from(s.cols) + 1) * cw,
            y1: oy + (top + rows) * ch,
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

    /// The box inside the border's stroke.
    fn inner(&self) -> Rect {
        Rect {
            x0: self.outer.x0 + self.t,
            y0: self.outer.y0 + self.t,
            x1: self.outer.x1 - self.t,
            y1: self.outer.y1 - self.t,
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
    belly: u32,
    eye: u32,
    nose: u32,
    /// The tall kitty's catch-light.
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
            belly: mix_rgb(coat, WHITE, 0.55),
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
            b'b' => Some(self.belly),
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
/// Never left of it, where the card's text is.
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
        x1: inner.x1,
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

/// The field's spots for this seed and geometry: up to [`STARS_MAX`], one per
/// slice of the region's width where there is room, clear of the kitty and of
/// each other. A pure function of its inputs, recomputed each tick.
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
    let kitty = kitty_rect(g, s).map(|at| at.rect);
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
            if !on_kitty && !crowded {
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
    reason = "output + sink + clip + centre + arm + coverage + glint + colour + theme: \
              the push_twinkle_star call with its clip and theme"
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
    dark: bool,
) -> bool {
    let (n, left) = (out.len(), sink.left);
    if lay_star(out, sink, clip, centre, arm, cov, gold, color, dark) {
        return true;
    }
    out.truncate(n);
    sink.left = left;
    false
}

/// One four-point star: the crate's one silhouette (`effect_util`'s
/// `push_twinkle_star`: a body through the crossing at the shared waist, the
/// nucleus, tapered points, glints when `gold`), clipped to `clip`. Additive
/// light on dark; on light every piece is source-over at its own coverage,
/// in over-text ink under its ceiling. Returns `false` once the budget is
/// spent, with the star in part ([`push_star`] takes it back).
#[allow(
    clippy::too_many_arguments,
    reason = "output + sink + clip + centre + arm + coverage + glint + colour + theme: \
              the push_twinkle_star call with its clip and theme"
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
    dark: bool,
) -> bool {
    if cov < 1.0 || arm < 1 {
        return true;
    }
    let ink_rgb = InkRole::OverText.ink(color);
    let cap = InkRole::OverText.alpha_cap();
    // One piece of the star at coverage `c` (0..255).
    let paint = |c: f32| {
        if dark {
            light(color, c)
        } else {
            ink(ink_rgb, (c * STAR_LIGHT_ALPHA / 255.0).min(cap))
        }
    };
    let rect = |x: i64, y: i64, w: i64, h: i64| Rect {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    };
    let arm32 = i32::try_from(arm).unwrap_or(i32::MAX);
    let t = i64::from(star_waist_px(arm32));
    let half = (t - 1) / 2;
    let b = i64::from(star_body_px(arm32));
    let full = paint(cov.floor());
    if !sink.flat(out, clip, rect(sx - b, sy - half, 2 * b + 1, t), full) {
        return false;
    }
    if !sink.flat(out, clip, rect(sx - half, sy - b, t, 2 * b + 1), full) {
        return false;
    }
    let c = i64::from(star_core_px(arm32));
    let cc = (c - 1) / 2;
    let core = paint((cov * STAR_CORE_ADD).floor());
    if !sink.flat(out, clip, rect(sx - cc, sy - cc, c, c), core) {
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
            if !sink.flat(out, clip, rect(sx + off, sy - th, len, tt), tip) {
                return false;
            }
            if !sink.flat(out, clip, rect(sx - th, sy + off, tt, len), tip) {
                return false;
            }
        }
    }
    if gold {
        let d = ((arm as f32 * STAR_GLINT).round() as i64).max(1);
        let dim = paint((cov * STAR_GLINT_COV).floor());
        for (gx, gy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            if !sink.flat(out, clip, rect(sx + gx, sy + gy, 1, 1), dim) {
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
    /// Ink stays legible: on dark every letter is lifted to a relative
    /// luminance floor; on light it takes the over-text light ink.
    #[must_use]
    pub fn line_ink(
        &self,
        now: Instant,
        row: u16,
        col: u16,
        len: u16,
        dark: bool,
        animate: bool,
    ) -> Vec<InkCell> {
        let mut out = Vec::with_capacity(usize::from(len));
        self.line_ink_into(Some(now), row, col, len, dark, animate, &mut out);
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
        dark: bool,
        animate: bool,
        out: &mut Vec<InkCell>,
    ) {
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
        out.extend(
            (0..len)
                .map_while(|i| Some((i, col.checked_add(i)?)))
                .map(|(i, c)| {
                    let x = f32::from(i) / span;
                    let base = self.palette.at(tri(x + phi));
                    let d = (x - pos) / LINE_SWEEP_SIGMA;
                    let w = env * (-d * d).exp() * LINE_SWEEP_GAIN;
                    let rgb = if dark {
                        mix_rgb(lift_to_luminance(base, LINE_DARK_LUMA_FLOOR), WHITE, w)
                    } else {
                        mix_rgb(InkRole::OverText.ink(base), 0, w)
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
/// is drawn to (`drawn`, the kitty's back on its ride) of its whole `len`, in
/// segments of `seg` pixels from `tx0`, its stripes spanning `[ya, yb)` and
/// stepped by up to `amp` ([`Emit::wave`]).
#[derive(Clone, Copy, Debug)]
struct TrailLay {
    tx0: i64,
    drawn: i64,
    len: i64,
    seg: i64,
    ya: i64,
    yb: i64,
    amp: i64,
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
        let base = self.scene.palette.at(tri(2.0 * s + phi));
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
            let cap = InkRole::Leading.alpha_cap();
            let rgb = mix_rgb(InkRole::Leading.ink(base), 0, w * LIGHT_HEAD_DEEPEN);
            ink(
                rgb,
                (BORDER_LIGHT_ALPHA + (cap - BORDER_LIGHT_ALPHA) * w).min(cap),
            )
        }
    }

    /// The border: top edge left → right, right side down, bottom edge right
    /// → left, left side up; arc fraction measured from the top-left corner.
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
        // Sides: one flat slice per row band, coloured at its midpoint (the
        // right side runs down, the left side up).
        for left in [false, true] {
            let x0 = if left { o.x0 } else { o.x1 - t };
            let arc = |y: i64| {
                if left {
                    (2 * w + h + (o.y1 - y)) as f32
                } else {
                    (w + (y - o.y0)) as f32
                }
            };
            for r in g.top..g.top + g.rows {
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

    /// The step wave's displacement (px, `+` up) for trail segment `k`.
    fn wave(&self, k: i64, amp: i64) -> i64 {
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

    /// One stripe's paint under shimmer weight `w`.
    fn stripe_paint(&self, k: usize, w: f32) -> Paint {
        let base = self.scene.palette.stripe(k);
        if self.scene.dark {
            light(mix_rgb(base, WHITE, w), f32::from(TRAIL_COV))
        } else {
            let cap = InkRole::Leading.alpha_cap();
            ink(
                InkRole::Leading.ink(base),
                (TRAIL_LIGHT_ALPHA + (cap - TRAIL_LIGHT_ALPHA) * w).min(cap),
            )
        }
    }

    /// The rainbow trail: six stripes from the text's end to the kitty's
    /// back, as tall as the kitty's body ([`trail_band`]: the cat leads, the
    /// rainbow trails), split at the row bands they cross. During an episode
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
        let Some(TrailLay {
            tx0,
            drawn,
            len,
            seg,
            ya,
            yb,
            amp,
        }) = self.trail_lay()
        else {
            return true;
        };
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
            let d = self.wave(k, amp);
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
        let (c0, c1) = self.scene.trail?;
        if g.rows < 2 {
            return None;
        }
        let inner = g.inner();
        let (tx0, tx1) = (g.col(c0).max(inner.x0), g.col(c1).min(inner.x1));
        if tx1 <= tx0 {
            return None;
        }
        let stripes = TRAIL_STOPS.len() as i64;
        let (ya, yb) = trail_band(&g, self.scene);
        let h = yb - ya;
        if h < stripes {
            return None;
        }
        let a = (g.ch / 16).max(1);
        let amp = if h / stripes > a { a } else { 0 };
        let len = tx1 - tx0;
        // On the ride the trail is laid behind the kitty, to its back: its
        // segments fall where the whole trail's do, the last cut there.
        let drawn = match self.kitty {
            Some((at, _)) if ride(self.clock) < 1.0 => tx1.min(at.rect.x0),
            _ => tx1,
        };
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
        })
    }

    /// The pixels segment `k` of the trail `lay` covers: its stripes, the
    /// top one's top and the bottom one's bottom stepped by the wave — one
    /// rectangle, `[xa, xb) × [ya − d, yb − d)`.
    fn trail_cover(&self, lay: &TrailLay, k: i64) -> Rect {
        let d = self.wave(k, lay.amp);
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
    /// that colour) — split at the row bands they cross. About sixty quads
    /// at either height, where a quad a run would be ninety-six two rows
    /// tall and a hundred and forty-four three.
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
        let base = self.scene.palette.at(tri(2.0 * s + phi));
        if self.scene.dark {
            light(base, f32::from(RIM_COV))
        } else {
            ink(
                InkRole::Leading.ink(base),
                RIM_LIGHT_ALPHA.min(InkRole::Leading.alpha_cap()),
            )
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

    /// The steady field: twinkling during an episode (easing onto a steady
    /// brightness over its last fifth), fixed once settled.
    fn stars(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        let Some(region) = star_region(&self.geo, self.scene) else {
            return true;
        };
        let dark = self.scene.dark;
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
            let color = if dark {
                match self.scene.palette {
                    CardPalette::Cruise => twinkle_rgb(i % 2 == 0),
                    CardPalette::Land => {
                        if i % 2 == 0 {
                            twinkle_rgb(true)
                        } else {
                            mix_rgb(SUNSET[3], WHITE, 0.5)
                        }
                    }
                }
            } else {
                self.scene.palette.mark(i)
            };
            if !push_star(
                out,
                sink,
                region,
                (s.x, s.y),
                arm,
                STAR_COV * env,
                gold,
                color,
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
            if !push_star(
                out,
                sink,
                region,
                (x.clamp(lo.0, hi.0 - 1), y.clamp(lo.1, hi.1 - 1)),
                arm,
                SHED_COV * (1.0 - tau).powf(1.5),
                false,
                self.scene.palette.mark(k),
                dark,
            ) {
                return false;
            }
        }
        true
    }

    /// The burst: [`BURST_STARS`] stars on a ring thrown out from the kitty
    /// (or the trail's end, with no kitty) over [`BURST_MS`], shrinking and
    /// fading as they fly. Only during an episode's first [`BURST_MS`].
    fn burst(&self, sink: &mut Sink, out: &mut Vec<GlowQuad>) -> bool {
        // Where the kitty lands, as it lands; at the episode's start where
        // it does not ride.
        let age = match self.clock.ride_ms {
            Some(r) if self.rides() => r.checked_sub(RIDE_MS),
            _ => self.clock.t_ms,
        };
        let Some(t) = age.filter(|&t| t < BURST_MS) else {
            return true;
        };
        let Some(region) = star_region(&self.geo, self.scene) else {
            return true;
        };
        let g = self.geo;
        let ((cx, mid), half_w) = match self.seat {
            Some(at) => {
                let r = at.rect;
                (((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2), (r.x1 - r.x0) / 2)
            }
            None => match self.scene.trail {
                Some((_, end)) => ((g.col(end).min(region.x1), g.band(g.top + 1)), 0),
                None => return true,
            },
        };
        let tau = t as f32 / BURST_MS as f32;
        let fly = 1.0 - (1.0 - tau).powi(3);
        let rx = (half_w as f32 + 2.0 * g.cw as f32).max(3.0 * g.cw as f32);
        let ry = 0.85 * g.ch as f32;
        let reach = 0.35 + 0.65 * fly;
        let cov = BURST_COV * (1.0 - tau).powf(1.2);
        let ratio = STAR_ARM_HERO + (STAR_ARM_FINE - STAR_ARM_HERO) * tau;
        let ratio = ratio * if self.scene.dark { 1.0 } else { STAR_ARM_INK };
        let arm = i64::from(star_arm_px(star_arm(g.ch as f32, ratio)));
        let turn = unit(self.seed, 0x4255_5253) * std::f32::consts::TAU;
        for k in 0..BURST_STARS {
            let theta = turn + std::f32::consts::TAU * k as f32 / BURST_STARS as f32;
            let x = cx + (theta.cos() * rx * reach).round() as i64;
            let y = mid + (theta.sin() * ry * reach).round() as i64;
            let color = self.scene.palette.mark(k);
            if !push_star(
                out,
                sink,
                region,
                (x, y),
                arm,
                cov,
                true,
                color,
                self.scene.dark,
            ) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::Geom;
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
            top: 3,
            rows: 4,
            cols: 60,
            trail: Some((32, 40)),
            kitty_col: Some(40),
            kitty_rows: 2,
            clip: WHOLE_STREAM,
            palette: CardPalette::Cruise,
            dark: true,
            serious: false,
            coat: 0x00E8_A060,
            eye: 0x0040_C080,
        }
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
    /// the whole band (35 px: `35 % 7 == 0` under a 2 px stroke), and a box
    /// larger than its pane's clip — each at BOTH HEIGHTS (§10.6): the
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
        v
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

    /// The box and band law for one quad.
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
        let by0 = i64::from(s.origin_y) + i64::from(s.top) * i64::from(s.ch);
        let by1 = by0 + i64::from(s.rows) * i64::from(s.ch);
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
    /// touching only the eye rows (and shutting every eye), the flick only
    /// the tail's columns; the tall cat is the two-row cat stood taller — the
    /// same head columns, ears, eyes, blush, nose and legs. The const shape
    /// checks hold this at build time; this test reads them back, so a check
    /// weakened to pass reads wrong here. NEGATIVE CONTROL: the shape checks
    /// refuse a blink that moves a pixel off the eye rows, a flick that moves
    /// one right of the tail, and a ragged row.
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
                    assert!(!blink.contains('e'), "the eyes shut (row {r})");
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
        }
        // The same cat: the tall head's columns hold the two-row head's
        // features — the ears' tips, the eyes, the blush and the nose on the
        // same columns — and the legs and paws are the two-row cat's.
        let cols_of = |f: &[&str], ch: char| -> std::collections::BTreeSet<usize> {
            f.iter()
                .flat_map(|r| {
                    r.char_indices()
                        .filter(move |&(_, c)| c == ch)
                        .map(|(i, _)| i)
                })
                .collect()
        };
        for ch in ['e', 'n'] {
            assert_eq!(
                cols_of(KITTY_SHORT.open, ch),
                cols_of(KITTY_TALL.open, ch),
                "{ch:?} on the same columns"
            );
        }
        assert_eq!(KITTY_SHORT.open[0], KITTY_TALL.open[0], "the ears' tips");
        let (short, tall) = (KITTY_SHORT.open, KITTY_TALL.open);
        assert_eq!(
            [short[11], short[12], short[12], short[13]],
            [tall[17], tall[18], tall[19], tall[20]],
            "the legs, a row longer, and the paws"
        );
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
    /// in rectangles carried down the rows, well under a quad a run. Every
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
                kitty.len() * 3 < runs * 2,
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
            let (_, over_l, _) = settled(&light);
            assert_eq!(
                over_l.iter().filter(|q| q.alpha == 255).count(),
                kitty.len(),
                "the kitty is opaque on light too"
            );
            // CONTROL: one quad nudged a pixel right reads back otherwise.
            let mut nudged = kitty.clone();
            nudged[0].x += u16::try_from(at.p).expect("a pixel");
            assert_ne!(drawn_kitty(&nudged, &at, inks), art(at.art.open));
        }
    }

    /// Rec. 709 luma of an un-premultiplied source-over quad end.
    fn luma(premul: u32, a: u8) -> f32 {
        let un = |sh: u32| ((premul >> sh) & 0xff) as f32 * 255.0 / f32::from(a);
        0.2126 * un(16) + 0.7152 * un(8) + 0.0722 * un(0)
    }

    /// On a light theme every quad is source-over, and every mark but the
    /// kitty's art is held under the light-ink ceiling, through an episode.
    #[test]
    fn a_light_theme_is_source_over_ink_throughout() {
        for s in scenes().into_iter().filter(|s| !s.dark) {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 5);
            for at in (0..=EPISODE_MS).step_by(150) {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                for q in under.iter().chain(&over) {
                    assert!(
                        q.alpha > 0 && q.alpha2 > 0,
                        "additive on white at {at} ms: {q:?}"
                    );
                    if q.alpha < 255 {
                        for (c, a) in [(q.color, q.alpha), (q.color2, q.alpha2)] {
                            // The leading ink's bar (LIGHT_INK_BOLD_MAX_LUMA),
                            // plus a level per channel of premultiplied rounding.
                            assert!(
                                luma(c, a) <= 123.0,
                                "too light to read on white at {at} ms: {q:?}"
                            );
                        }
                    }
                }
            }
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
    /// to its back and no further; seated from [`RIDE_MS`] on. At both
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
                let inner = g.inner();
                let trail_end = g.col(s.trail.expect("a trail").1).min(inner.x1);
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
                            trail_end.min(at.rect.x0),
                            "{what}: {t} ms, to its back"
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
    /// boxed rows, a long trail and tall cells, through the burst.
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
        // THE SETTLED FRAME — what a card at rest replays into every frame,
        // typing included — spends no more than the card before §10.6 did,
        // at either height and with the rim, but for one rim strip a toe's
        // gap (the rim runs under each paw, not under the gaps between them:
        // `the_toes_gaps_open_onto_the_ground`): that card drew its trail in
        // segments of six stripes settled and its kitty a quad a run.
        // Measured on a launch card's trail at its longest (20 cells) and
        // shortest (8), at the shipping cell sizes.
        let toes = |frame: &[&str]| {
            (0..frame.len() as i64)
                .flat_map(|r| (0..KITTY_W as i64).map(move |c| (r, c)))
                .filter(|&(r, c)| rim_solid(frame, r, c) && !art_at(frame, r, c))
                .count()
        };
        assert_eq!(toes(&KITTY_OPEN), 3);
        assert_eq!(toes(&KITTY_TALL_OPEN), 3);
        for kitty_rows in [2u8, 3] {
            for (cw, ch, trail) in [(9u16, 18u16, 20u16), (9, 18, 8), (16, 33, 20), (8, 16, 20)] {
                let s = CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    trail: Some((32, 32 + trail)),
                    kitty_col: Some(32 + trail),
                    cols: 32 + trail + 6 + 4 + 1,
                    ..scene()
                };
                let now = parts(&s, Clock::SETTLED);
                let len = i64::from(trail) * i64::from(cw);
                let seg = (TRAIL_SEG_CELLS * i64::from(cw))
                    .max((len + MAX_TRAIL_SEGMENTS - 1) / MAX_TRAIL_SEGMENTS);
                let before = now.border.len()
                    + TRAIL_STOPS.len() * usize::try_from((len + seg - 1) / seg).expect("a count")
                    + runs_of(&KITTY_OPEN)
                    + now.stars.len()
                    + toes(&KITTY_OPEN);
                let (under, over, _) = settled(&s);
                assert!(
                    under.len() + over.len() <= before,
                    "{cw}x{ch}, {kitty_rows} rows, a {trail}-cell trail: {} settled quads, {before} before",
                    under.len() + over.len()
                );
            }
        }
    }

    /// A STAR IS LAID WHOLE OR TAKEN BACK, TO THE QUAD: with the budget
    /// one quad short of a star, [`push_star`] lays nothing, spends nothing
    /// and says the budget is spent; with exactly enough it lays the whole
    /// star and spends it all. NEGATIVE CONTROL: [`lay_star`] alone, one
    /// short, leaves part of a star behind.
    #[test]
    fn a_star_is_laid_whole_or_taken_back() {
        let s = scene();
        let geo = Geo::new(&s).expect("a scene");
        let region = star_region(&geo, &s).expect("a region");
        let centre = ((region.x0 + region.x1) / 2, (region.y0 + region.y1) / 2);
        for (arm, gold, dark) in [(3, false, true), (6, true, true), (5, true, false)] {
            let star = |left: usize, whole: bool| {
                let mut sink = Sink { geo, left };
                let mut out = Vec::new();
                let laid = if whole {
                    push_star(
                        &mut out, &mut sink, region, centre, arm, 200.0, gold, WHITE, dark,
                    )
                } else {
                    lay_star(
                        &mut out, &mut sink, region, centre, arm, 200.0, gold, WHITE, dark,
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
    /// ride's end on a 20-cell trail at a large cell, three rows tall, a
    /// frame can want more than [`MAX_CARD_QUADS`]; it is held to the cap,
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
                card.line_ink(t, 4, 2, 9, true, true)
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
    /// reduced motion; legible on both grounds.
    #[test]
    fn the_line_sweeps_then_settles() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 3);
        let (row, col, len) = (7u16, 11u16, 64u16);
        for dark in [true, false] {
            let rest = card.line_ink(t0 + ms(EPISODE_MS), row, col, len, dark, true);
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
                card.line_ink(t0 + ms(EPISODE_MS + 5000), row, col, len, dark, true),
                "settled is constant"
            );
            assert_eq!(
                rest,
                card.line_ink(t0 + ms(900), row, col, len, dark, false),
                "reduced motion is settled"
            );
            let mid = card.line_ink(t0 + ms(900), row, col, len, dark, true);
            let next = card.line_ink(t0 + ms(900 + FRAME_MS), row, col, len, dark, true);
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
                    let y = 0.2126 * f32::from(c.color[0])
                        + 0.7152 * f32::from(c.color[1])
                        + 0.0722 * f32::from(c.color[2]);
                    assert!(y <= 69.0, "{rgb:06x} too light on white");
                }
            }
        }
        // The line stops at the last column rather than wrapping.
        assert_eq!(card.line_ink(t0, 0, u16::MAX - 1, 5, true, true).len(), 2);
    }

    /// The line wears the last tick's palette.
    #[test]
    fn the_line_wears_the_cards_palette() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        let cruise = card.line_ink(t0, 0, 0, 30, true, false);
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
        assert_ne!(card.line_ink(t0, 0, 0, 30, true, false), cruise);
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
                assert!(
                    !over.iter().any(|q| q.alpha == 255),
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

    /// Land's border and trail wear the sunset ramp: every settled border
    /// colour is a point on it at the border's coverage, every trail stripe
    /// is its stop at the trail's (red at the top through gold to rose at
    /// the bottom), on both themes — and Cruise's are not on it (the
    /// negative control: the spectrum's greens and blues are off the ramp).
    #[test]
    fn land_wears_the_sunset_ramp() {
        for dark in [true, false] {
            let land = CardScene {
                palette: CardPalette::Land,
                dark,
                ..scene()
            };
            // The colour a settled quad end carries for ramp colour `rgb`.
            let cap = InkRole::Leading.alpha_cap();
            let border = |rgb: u32| {
                if dark {
                    light(rgb, f32::from(BORDER_COV)).color
                } else {
                    ink(InkRole::Leading.ink(rgb), BORDER_LIGHT_ALPHA.min(cap)).color
                }
            };
            let stripe = |rgb: u32| {
                if dark {
                    light(rgb, f32::from(TRAIL_COV)).color
                } else {
                    ink(InkRole::Leading.ink(rgb), TRAIL_LIGHT_ALPHA.min(cap)).color
                }
            };
            let (under, _, _) = settled(&land);
            let (rim, trail): (Vec<&GlowQuad>, Vec<&GlowQuad>) =
                under.iter().partition(|q| on_rim(q, &land));
            assert!(!rim.is_empty() && !trail.is_empty(), "dark {dark}");
            for q in &rim {
                for c in [q.color, q.color2] {
                    let gap = gap_to_ramp(c, |t| border(sunset(t)));
                    assert!(
                        gap <= 2,
                        "dark {dark}: border {c:06x} off the sunset by {gap}"
                    );
                }
            }
            let stops: Vec<u32> = (0..TRAIL_STOPS.len())
                .map(|k| stripe(sunset(k as f32 / (TRAIL_STOPS.len() - 1) as f32)))
                .collect();
            let worn: std::collections::BTreeSet<u32> =
                trail.iter().flat_map(|q| [q.color, q.color2]).collect();
            assert_eq!(
                worn,
                stops.iter().copied().collect(),
                "dark {dark}: the trail is the ramp's six stops"
            );
            assert_eq!(stops[0], stripe(SUNSET[0]), "red on top");
            assert_eq!(stops[5], stripe(SUNSET[3]), "rose at the bottom");
            // The negative control: Cruise's border leaves the ramp.
            let cruise = CardScene {
                palette: CardPalette::Cruise,
                ..land
            };
            let (under, _, _) = settled(&cruise);
            let off = under
                .iter()
                .filter(|q| on_rim(q, &cruise))
                .map(|q| gap_to_ramp(q.color, |t| border(sunset(t))))
                .max()
                .unwrap_or(0);
            assert!(
                off > 40,
                "dark {dark}: Cruise's border is not a sunset ({off})"
            );
        }
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
    /// each on the palette's ramp), additive on dark and source-over ink on
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
                                assert!(q.alpha > 0 && q.alpha < 255, "{what}: ink {q:?}");
                            }
                        }
                        let hues: std::collections::BTreeSet<u32> =
                            quads.iter().map(|q| q.color).collect();
                        assert!(hues.len() > 3, "{what}: a spectrum, {} hues", hues.len());
                        if dark {
                            for c in &hues {
                                let gap = gap_to_ramp(*c, |t| {
                                    light(spectrum(t), f32::from(RIM_COV)).color
                                });
                                assert!(gap <= 2, "{what}: {c:06x} off the spectrum by {gap}");
                            }
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
                        let trail = coverage(&p.trail);
                        let both = coverage(&p.rim)
                            .into_keys()
                            .find(|px| trail.contains_key(px));
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
                    let trail = coverage(&parts(&s, flick).trail);
                    assert!(
                        ring.iter().any(|px| trail.contains_key(px)),
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
    /// both grounds, in every frame, at three cell sizes. NEGATIVE CONTROL:
    /// the pixels under the paws beside each gap are lit.
    #[test]
    fn the_toes_gaps_open_onto_the_ground() {
        let mut gaps = 0;
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
                                for side in [c - 1, c + 1] {
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
        assert_eq!(gaps, 3 * 2 * 2 * 3 * 3, "three gaps a frame");
    }

    /// THE TRAIL IS THE KITTY'S BODY'S HEIGHT (§10.6: the cat leads, the
    /// rainbow trails): at both heights, settled and through an episode
    /// past its wave, the trail's stripes span exactly the art's body band
    /// at the kitty's seat — six stripes top to bottom, each one quad end to
    /// end when settled (split only at the row bands it crosses) — from the
    /// trail's start to the kitty's back; well under the two rows' height
    /// the trail spanned before (three stripes a band), and inside the
    /// kitty's own height. During the wave the stripes step a few pixels
    /// either way, no further. CONTROL: an episode frame draws the trail in
    /// segments, the settled frame does not.
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
                            && i64::from(q.x) + i64::from(q.w) == at.rect.x0),
                    "{what}: each stripe one quad from the trail's start to the kitty"
                );
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
                    for q in &ep {
                        assert!(
                            i64::from(q.y) >= b0 - amp
                                && i64::from(q.y) + i64::from(q.h) <= b1 + amp,
                            "{what}: {q:?} strays from the band at {t} ms"
                        );
                    }
                }
            }
        }
    }
}
