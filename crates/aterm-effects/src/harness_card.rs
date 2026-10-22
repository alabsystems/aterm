// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness card (`docs/DESIGN-harness-autopilot-2026-10-15.md` §3): the
//! decoration Claude Code's launch card wears while the aterm harness has the
//! session on Autopilot. Owner, 2026-10-15: "a rainbow border and a similar
//! pixel art with an aterm kitty and fancy rainbows and star power to show off
//! the power of the harness". Its colours are the Autopilot's
//! ([`CardPalette`]): Cruise's spectrum, Land's sunset, and Off's moonbow,
//! white → pearl → silver → steel (owner, 2026-10-19: "I want a white themed
//! for 'off'").
//!
//! This module is the card's pure emitter. The host finds the card
//! (`aterm_phase::launch_card_place`), resolves its geometry into a
//! [`CardScene`] in window pixels, and each frame asks
//! [`HarnessCard::replay`] for the held frame it keeps (or, while the card
//! flows, [`HarnessCard::replay_step`] for the step it keeps), else
//! [`HarnessCard::draw`]s one: two fx contents (`docs/DESIGN-card-engine-2026-10-19.md`
//! §4.8), card-local — the border and the trail under the glyph ink, the
//! kitty, its rim and the stars over it — which it lays at the card's origin
//! as two layers ([`CardFrame::layers`]). Pictures are drawn once per cell
//! height ([`CardArts`]) and a frame is their palettes. It reads no clock
//! (time comes in as `now`), no terminal and no config, so the same inputs
//! give the same bytes.
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
//!   rainbow trails). It runs on under the
//!   art to the haunch's outline ([`KittyArt::rear`]), so the rainbow meets
//!   the cat flush on every row it spans. Settled, a stripe is one ramp end
//!   to end; during the intro it steps and shimmers in segments, but for
//!   the stretch the cat's rim could reach, which holds still, and in the
//!   flow it lies still while the ripple's glints run along its segments.
//! * **Kitty** (into `over`): a blocky cat with pink-lined pointed ears,
//!   an iris beside a dark pupil, a pale muzzle and belly, four legs and
//!   its tail up. Each of its six authored frames is one opaque sprite,
//!   two or three cell rows tall. It blinks and flicks its tail back over
//!   the rainbow; the tall cat keeps a white catch-light over each pupil.
//! * **Rim** (into `over`): a one-pixel outline of the border's spectrum just
//!   outside the kitty's silhouette ([`rim_px`]), so a black or grey coat reads
//!   on a dark ground: additive light on dark, opaque print ink on light, its
//!   hue running round the cat as the border's runs round the box. It is cut
//!   where it would lie on the trail, and runs under each paw but not under
//!   the toes' gaps, which open onto the ground.
//! * **Stars** (into `over`): one to three glints on the rainbow, flaring
//!   as its sheen passes (in the flow, as the ripple's crest does); five to
//!   nine field stars, the ride's sparkles,
//!   and a burst of [`BURST_STARS`] where the kitty lands. All use the same
//!   cached star arts. The glints are small and steady once settled.
//!
//! # An intro, then the flow
//!
//! [`HarnessCard::arm`] starts an INTRO, an episode of [`EPISODE_MS`]: the
//! border's hue drifts (one trip through the palette per [`TRIP_MS`] at
//! first, easing onto the flow's pace) and a white-hot head laps the
//! perimeter twice; on the card's first showing, for the episode's first
//! [`RIDE_MS`], the kitty RIDES in (§10.6) — from the trail's start to its
//! seat, easing out, the trail laid behind it to its back, the field's
//! stars coming up as it passes them and sparkles shed as it goes — and
//! lands with the burst; once the glass has shown it seated it plays every
//! later episode in its seat, the burst at the start ([`Ride`]); the trail
//! steps and shimmers, its glints flaring under the sheen; the kitty blinks
//! and flicks its tail; the stars twinkle.
//!
//! Then THE FLOW, for as long as the card may animate (owner, 2026-10-19:
//! "Make the rainbow at the banner continue to shimmer and flow
//! continuously, which is also aligned with how the claude alien
//! shimmers"): the border's and the rim's hue keep travelling clockwise,
//! one trip per [`FLOW_TRIP_MS`], and CLAUDE'S RIPPLE ([`crate::ripple`]) —
//! the glint wave its mascot shimmers with, a ring born at the wand every
//! [`GLINT_PERIOD_MS`] and running outward at seven cells a second — runs on
//! out from the wand ([`CardScene::wand`]) across the border, the trail,
//! the rim, the stars and the line, a crest toward white on dark and a
//! deeper print on light. Its frames step on the ripple's grid, every
//! [`FLOW_STEP_MS`] — the frame grid of Claude's ripple — so a step's frame
//! is a function of the step alone, replayed between steps
//! ([`HarnessCard::replay_step`]); the host may PHASE-LOCK the grid to
//! Claude's ripple where it sees the wand flare ([`HarnessCard::lock_glint`]:
//! taken whole while no glint shows, else slewed, never more than a step a
//! flare), and free-runs where it does not — the normal case: only Claude's
//! fullscreen renderer with the key on shimmers its mascot and flares its
//! wand. The intro hands over without a jump:
//! its drift eases onto the flow's speed, its head fades as the glints
//! rise over its last eighth.
//!
//! A flow step is a new PALETTE on the same arts (design
//! `docs/DESIGN-card-engine-2026-10-19.md` §4.8, reconciled in its §4.8.1):
//! the trail lies flat in the flow (its step wave is the intro's), so the
//! rim's art stays the picture of its kitty frame, cut round the trail in
//! one hole, and only its strips' entries, the border's and the trail's
//! ramps and the stars' inks move with the hue and the ripple (on the
//! crest a field star's dots and a rainbow glint's arm pick another of the
//! registered star arts, [`CardArts`]). A step is
//! drawn once, into the card's ring of contents, and replayed as the same
//! `Arc`s until the next ([`HarnessCard::replay_step`]); the SETTLED frame,
//! kept and replayed with no clock read, is the held card's.
//!
//! `animate == false` (reduced motion, an unfocused window, the load-shed
//! latch) gives the SETTLED frame at once — constant contents, replayed as
//! the kept `Arc`s ([`HarnessCard::replay`]), and no deadline
//! ([`HarnessCard::deadline`]), so the host idles at zero — and resumes the flow from the settled colours when it
//! may animate again, the glints coming up over a few steps. It does not
//! throw an intro away: one not yet drawn animating is OWED and plays from
//! the first tick that may animate, so a latch that forces Reduced just as
//! the card is first found delays the show rather than cancelling it. A
//! tick with nothing to draw (an empty clip, no rows or columns: a pane
//! mid-layout) holds the intro back the same way. The debt lapses
//! [`OWED_MAX_MS`] after the arm, a margin past the latch's dwells: an
//! intro never plays minutes later on an unrelated redraw, and a latch held
//! longer by sustained load forfeits the show (the card flows on, with no
//! intro). Serious mode ends any intro and the flow and also drops the
//! kitty, the trail and the stars, leaving a static border
//! ([`crate::motion::SeriousEffect::HarnessCard`]).
//!
//! # Laws the host can rely on
//!
//! * **One band per slice.** The engine cuts every item at the cell-row
//!   bands and draws a slice only where its row is repainted (design §4.3),
//!   so the card splits nothing and the law cannot be broken by it.
//! * **Inside the box.** Every pixel lies inside the border's outer rectangle
//!   (its air included) and inside the scene's [`clip`](CardScene::clip) (the
//!   pane); the kitty lies inside the border's stroke as it stands with no
//!   air, wherever its ride has it; the stars, the sparkles and the burst
//!   stay in the trail/kitty region, never over the card's text columns (left
//!   of the trail's start). AIR MOVES THE BORDER AND NOTHING ELSE: the trail,
//!   the kitty, its rim and the stars are laid inside the boxed rows' stroke,
//!   so a card that gains or loses its air (a notice drawn under it, a launch
//!   line over it) keeps every one of them where it was.
//! * **Bounded.** At most [`CARD_ITEMS_MAX`] items a frame across both
//!   contents, whatever the art's detail, and every mark of every frame is
//!   drawn: nothing is cut to the bound.
//! * **Themes.** On a dark theme the border, trail, rim and stars are additive
//!   light (`alpha == 0`); on a light theme every item is source-over
//!   (`alpha > 0`), since additive light is invisible on white. There the
//!   border, trail and rim are opaque card art in print inks
//!   ([`PRINT_CRUISE`], [`PRINT_LAND`], and Off's pearl to graphite,
//!   [`PRINT_OFF`]: white ink would vanish on paper), as a printer lays a rainbow on
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
//!   below it too, a rainbow glint flares once a shimmer pass, and the
//!   ripple glints a point at most twice a cycle (0.65 Hz), the flow's
//!   blink once every two (all const-asserted). The burst only fades, and
//!   the intro's blink comes once per 2.5 s.

use std::sync::Arc;

use aterm_render::{InkCell, premul_rgb};
use aterm_time::Instant;
use std::ops::Range;
use std::time::Duration;

use crate::color_math::relative_luminance;
use crate::cursor_glow::InkRole;
use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE_ADD, STAR_GLINT_COV,
    TWINKLE_FLASH_BOUND_HZ, TWINKLE_FLASH_HZ, mix_rgb, star_arm, star_arm_px, twinkle_env,
    twinkle_peak, twinkle_rgb,
};
use crate::genome::mix;
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::ribbon::{PAPER_MARK_CONTRAST, paper_mark_ink};
use crate::ripple::{
    FLOW_STEP_MS, GLINT_CREST_DARK, GLINT_CREST_LIGHT, GLINT_PERIOD_MS, cell_distance, glint,
};
use crate::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN, spectrum, spectrum_stop};

#[cfg(test)]
mod layer_tests;
mod layers;
#[cfg(test)]
mod oracle;

pub use layers::{CardArts, CardFrame, CardFrames, CardShape, KeptFrame, card_layer_id};

// ─── the clock ──────────────────────────────────────────────────────────────

/// The intro's length (ms): the card plays its episode this long after
/// [`HarnessCard::arm`], then flows ([`HarnessCard::flowing`]).
pub const EPISODE_MS: u64 = 6000;

/// The intro's cadence (ms, ~30 fps): [`HarnessCard::deadline`]'s step and
/// the fingerprint's frame quantum while an intro runs. The flow steps on
/// the ripple's [`FLOW_STEP_MS`].
pub const FRAME_MS: u64 = 33;

/// How long past the last frame drawn (ms) [`HarnessCard::deadline`] keeps
/// asking: for the flow's first frame, `now`, once the intro has run out,
/// and for the flow's next step. A host that draws on its deadline draws
/// the next frame well inside this; one that stopped drawing (the frame
/// scrolled back, the card's version no longer the process's, the pane
/// hidden, the window occluded) but still asks each loop is asked to wake
/// for at most this long, then is let idle — never a spin for as long as
/// the card goes undrawn. A host later than this draws the card's frame on
/// its next wake.
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

/// The deepest a star's pieces stack over one pixel: the two bodies'
/// crossing, the nucleus on it and, on a gold star, a glint where one meets
/// them. The depth of a star's peeled art ([`aterm_render::fx::PieceArt`]),
/// one sprite a layer of it on a light ground, held at every star the card
/// registers and over every arm `1..=32`
/// (`a_stars_pieces_stack_at_most_four_deep`).
pub const STAR_DEPTH_MAX: usize = 4;

/// The most tone rectangles one light star is laid in. Where the trail's
/// ink crosses it, a light star is the paper's white on the print and its
/// ink off it, pixel by pixel (§10.6.10): each trail segment under the star
/// cuts its box into the white over the segment and the ink above and below
/// it, and the box either side of the trail is one rectangle more.
const TONE_RECTS_MAX: usize = 3 * MAX_TRAIL_SEGMENTS as usize + 2;

/// The most items one frame of the card holds across its two contents.
///
/// Under: the border's two edges of at most [`MAX_EDGE_SEGMENTS`] ramps
/// each and its two sides (each ONE stack of one-row ramps, every pixel row
/// its band's colour), and the trail's six stripes in at most
/// [`MAX_TRAIL_SEGMENTS`] segments. Over: the kitty (one sprite), its rim
/// (one art laid in at most [`RIM_COVER_MAX`] rectangles, each cut round
/// the trail's hole into at most four), and every
/// star — the field's [`STARS_MAX`], the burst's [`BURST_STARS`] and the
/// rainbow's [`GLINTS_MAX`] and the ride's [`SHED_SPARKLES`] — one folded sprite on dark, at most
/// [`STAR_DEPTH_MAX`] peeled ones a tone rectangle ([`TONE_RECTS_MAX`]) on
/// light.
///
/// It has NO ART TERM: the kitty is one sprite and the rim one art laid in
/// at most [`RIM_COVER_MAX`] rectangles, however detailed their art
/// (`the_card_is_bounded_without_its_art`), and a box of any height has the
/// same two sides. Nothing is ever cut to it: it is the
/// bound every frame is held to (debug-asserted as it is built), never a
/// budget a frame spends. The quad cap it replaces (680 quads, spent border
/// → trail → kitty → rim → burst → stars → sparkles) grew with the art's
/// detail and took field stars back on a pathological scene.
pub const CARD_ITEMS_MAX: usize = CARD_UNDER_ITEMS_MAX + CARD_OVER_ITEMS_MAX;

/// [`CARD_ITEMS_MAX`]'s under content: the border's edges and sides, the
/// trail's stripes.
const CARD_UNDER_ITEMS_MAX: usize =
    2 * MAX_EDGE_SEGMENTS as usize + 2 + TRAIL_STOPS.len() * MAX_TRAIL_SEGMENTS as usize;

/// [`CARD_ITEMS_MAX`]'s over content: the kitty, its rim (each rectangle of
/// its cover cut round the trail's hole into at most four), the stars.
const CARD_OVER_ITEMS_MAX: usize = 1
    + 4 * RIM_COVER_MAX
    + STAR_DEPTH_MAX * TONE_RECTS_MAX * (STARS_MAX + BURST_STARS + GLINTS_MAX + SHED_SPARKLES);

/// The most rectangles the rim art is laid in. This fixed bound contributes
/// four items per rectangle when the trail cuts its hole.
pub(crate) const RIM_COVER_MAX: usize = 32;

const _: () = assert!(
    CARD_UNDER_ITEMS_MAX == 2 * 48 + 2 + 6 * 16
        && CARD_ITEMS_MAX == CARD_UNDER_ITEMS_MAX + CARD_OVER_ITEMS_MAX
        && CARD_ITEMS_MAX < u16::MAX as usize,
    "the card's item bound is the design's under content and a star term, no art term"
);

/// The border's hue drift at an episode's start: one trip through the palette
/// (`φ` moves by 1) per this many ms, easing to the flow's own pace
/// ([`FLOW_TRIP_MS`]) by the episode's end.
pub const TRIP_MS: u64 = 3000;

/// How far `φ` would drift over the episode at a speed that starts at one
/// trip per [`TRIP_MS`] and eases linearly to zero, `EPISODE / (2·TRIP)`.
const PHI_DRIFT: f32 = EPISODE_MS as f32 / (2.0 * TRIP_MS as f32);

/// The settled border phase. Red sits at the top-left corner and the loop
/// reaches violet halfway round.
const PHI_SETTLED: f32 = 0.0;

/// THE FLOW'S HUE (owner, 2026-10-19: "Make the rainbow at the banner
/// continue to shimmer and flow continuously"): after the intro the border's
/// and the rim's hue keep travelling clockwise — rightward along the top
/// edge, away from the mascot, the way Claude's ripple runs — one trip
/// through the palette per this many ms: four of the ripple's cycles. A
/// time, not a speed along the box, so a border that gains or loses its air
/// (a longer perimeter) moves its hue no faster, and the rim, which shares
/// `φ`, keeps its pace too.
pub const FLOW_TRIP_MS: u64 = 4 * GLINT_PERIOD_MS;

/// The intro's eased part of the drift: the speed starts at one trip per
/// [`TRIP_MS`] and eases linearly onto the flow's one per [`FLOW_TRIP_MS`],
/// so the hand-off to the flow is continuous in place and in speed. The
/// integral of the eased excess, `PHI_DRIFT − EPISODE / (2·FLOW_TRIP)`.
const PHI_INTRO: f32 = PHI_DRIFT - EPISODE_MS as f32 / (2.0 * FLOW_TRIP_MS as f32);

/// How long the flow's glints take to come up where the flow starts from the
/// settled frame (a gate reopened: focus, motion, the load latch, serious
/// mode off): three of the ripple's steps.
const GLINT_FADE_MS: u64 = 3 * FLOW_STEP_MS;

/// How far ahead of the frame that SAW Claude's wand flare the lock puts a
/// ring's birth on the card's grid — a step's start (ms): half the flow's
/// slack ([`FLOW_SLACK_MS`]). Claude's repaint shows the frame its clock is
/// in, so its first flaring repaint shows its cycle's frame 0, and it
/// repaints about once a step after it (127.7 ms on a real window, design
/// §10.6.11); locked so, the card shows its own step 0 on that frame, and
/// each of Claude's later repaints lands this far into one of the card's
/// steps, inside the slack: its present draws the card's step, and the
/// card's own wake ([`HarnessCard::deadline`]) finds it drawn. The card's
/// step changes this far ahead of Claude's on screen; the lock this
/// replaced aimed at Claude's cycle's estimated start (the flare seen half
/// a median repaint, 67 ms, late, and 16 ms more for the frame's
/// composition) and changed it up to 83 ms ahead — past the slack, so each
/// step cost a present of its own.
pub const FLARE_STEP_LEAD_MS: u64 = FLOW_SLACK_MS / 2;

/// THE LOCK'S GAIN: the share of its error one lock takes out where the
/// card's glints show. Claude's first flaring repaint lands up to a repaint
/// (118–142 ms, lab M2: [`crate::ripple::CLAUDE_REPAINT_P90_MS`]) after its
/// cycle's start, so one flare's error is
/// as much that jitter as any drift: taken a quarter at a time it averages
/// out (about 15 ms of wander) instead of being chased flare by flare —
/// the half-step rule this replaced took up to 64 ms whole, and adopted a
/// larger error with a fade, every few flares.
const LOCK_GAIN: i64 = 4;

/// The most one lock moves a grid whose glints show (ms): one step, so the
/// ring at most skips or repeats one of its frames; a larger error is taken
/// out a step a flare.
const LOCK_SLEW_MS: i64 = FLOW_STEP_MS as i64;

/// How long past a flow step's start the flowing card's own wake lands
/// ([`HarnessCard::deadline`]): a present that lands in the step's first
/// [`FLOW_SLACK_MS`] anyway — Claude's own repaint, the vitals band, a
/// keystroke — draws the card's step first, and the card's own wake then
/// finds it drawn. Claude's repaints come about once a step, and the lock
/// puts each [`FLARE_STEP_LEAD_MS`] into a step, inside this slack, so with
/// the key on a flowing card rides Claude's presents (MEASURED on a real
/// window, design §10.6.11); the lock before it put them 83 ms in, past the
/// slack, and nearly every step was a present of its own.
pub const FLOW_SLACK_MS: u64 = 40;

/// The flow's blink: once every two ripple cycles (6.1 s), for two steps.
const FLOW_BLINK_PERIOD_MS: u64 = 2 * GLINT_PERIOD_MS;
const FLOW_BLINK_MS: u64 = 2 * FLOW_STEP_MS;

/// The intro's glints come up over its last eighth (from `u = 0.88`, the
/// head's mirror: the head fades as the rings rise), so the flow takes them
/// over already up.
const GLINT_INTRO_FROM: f32 = 0.88;

const _: () = {
    assert!(
        FLOW_SLACK_MS < FLOW_STEP_MS,
        "the flow's wake lands inside its step"
    );
    assert!(
        FLARE_STEP_LEAD_MS > 0 && FLARE_STEP_LEAD_MS < FLOW_SLACK_MS,
        "Claude's repaints land inside the slack, after the step starts"
    );
    assert!(PHI_INTRO > 0.0, "the intro starts faster than the flow");
    assert!(
        1000.0 / (FLOW_BLINK_PERIOD_MS as f32) <= TWINKLE_FLASH_BOUND_HZ,
        "the flow's blink stays under the photosensitivity bound"
    );
};

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
//
// Off's white card is printed in silver ([`PRINT_OFF`]): pearl at the top,
// where the dark moonbow is white, down to graphite, so the trail holds its
// edge on paper as the moonbow's steel holds it on dark.

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
#[inline]
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

/// Off's six stripes on a light ground, the moonbow in print ([`MOONBOW`]'s
/// paper twin): pearl → silver → platinum → steel → pewter → graphite, lit
/// from above as the dark moonbow runs white → steel. A cool silver (OKLCh
/// hue about 261-267°, chroma 0.015-0.028), never a tinted grey, falling in
/// OKLab L from 0.84 to 0.45 at least 0.071 apart (ΔE) pair by pair. The
/// pearl holds 1.45:1 or more on every built-in light ground (Aterm Day
/// 1.58, Linen 1.49, Solarized Light 1.53, Gruvbox Light 1.45, Latte 1.46):
/// a white ink would vanish on paper as white light vanishes on white. Off
/// leaves the trail's vividness law (`vivid_on_paper` in the tests) and
/// keeps its own, a pearl on paper (`pearl_on_paper`).
const PRINT_OFF: [u32; 6] = [
    0x00C4_CAD4, // pearl
    0x00AA_B1BE, // silver
    0x0093_9BA9, // platinum
    0x007C_8494, // steel
    0x0065_6D7D, // pewter
    0x004F_5666, // graphite
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

/// The kitty at rest, eyes open, tail up: a flying cat facing right, the
/// rainbow arriving at its haunch — a blocky head in the spirit of Claude's
/// block mascot, pointed ears lined pink, a long body with a belly patch,
/// four legs and the tail raised behind. `o` outline, `c` coat, `b` belly
/// and muzzle, `e` eye, `k` pupil, `n` inner ears and nose. Each eye is a
/// block of two pixels a row, the eye's own colour behind and a dark pupil
/// in front ([`PUPIL`]): the iris stops are about as light as a grey coat,
/// and an eye of iris alone goes on one.
const KITTY_OPEN: KittyFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocekccccekco",
    "..occcccccccocekccccekco",
    "..occcccccccoccbbnnbbcco",
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
    "..occcccccccoccbbnnbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// The tail flick: [`KITTY_OPEN`] with the tail swept straight back over
/// the trail.
const KITTY_FLICK: KittyFrame = [
    "..............o......o..",
    ".............ono....ono.",
    "............onnoooooonno",
    "............occcccccccco",
    "...oooooooooocccccccccco",
    "ooocccccccccocekccccekco",
    "ccccccccccccocekccccekco",
    "ooocccccccccoccbbnnbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

/// The tall kitty at rest (§10.6): [`KITTY_OPEN`]'s cat stood three rows
/// tall — the same head twelve pixels wide with its pink-lined ears a row
/// taller, eyes a pixel taller with a white catch-light (`w`) over the
/// pupil, the nose and muzzle where the two-row cat has them; a deeper body
/// with the same belly patch, legs a row longer, and the tail a row longer,
/// up.
const KITTY_TALL_OPEN: KittyTallFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onno....onno",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocewccccewco",
    "..occcccccccocekccccekco",
    "..occcccccccocekccccekco",
    "..occcccccccoccbbnnbbcco",
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
    "..occcccccccoccbbnnbbcco",
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
/// back over the trail at the top of the body, as [`KITTY_FLICK`] sweeps it.
const KITTY_TALL_FLICK: KittyTallFrame = [
    "..............o......o..",
    ".............ono....ono.",
    "............onno....onno",
    "............onnoooooonno",
    "............occcccccccco",
    "............occcccccccco",
    "...oooooooooocccccccccco",
    "ooocccccccccocewccccewco",
    "ccccccccccccocekccccekco",
    "ooocccccccccocekccccekco",
    "..occcccccccoccbbnnbbcco",
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

/// Which frame of its art the kitty wears ([`kitty_frame`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KittyPose {
    /// Eyes open, tail up.
    Open = 0,
    /// The happy blink.
    Blink = 1,
    /// The tail flick.
    Flick = 2,
}

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
    /// The art's frame in `pose`.
    #[inline]
    const fn frame(&self, pose: KittyPose) -> &'static [&'static str] {
        match pose {
            KittyPose::Open => self.open,
            KittyPose::Blink => self.blink,
            KittyPose::Flick => self.flick,
        }
    }

    /// The art's height in pixels.
    #[inline]
    const fn h(&self) -> usize {
        self.open.len()
    }

    /// How many cell rows it is laid over.
    #[inline]
    const fn rows(&self) -> usize {
        self.h() / KITTY_HALF
    }

    /// The column the trail runs to under the art ([`KittyAt::rear`]): the
    /// furthest in of the body band's rows' first pixels in ANY of its
    /// frames, so the rainbow meets the haunch flush on every row it spans
    /// whichever frame shows — the flick's back, its tail swept down, starts
    /// a pixel further in than the open frame's. The art's own opaque pixels
    /// hide the stripes left of it on a row that starts sooner
    /// ([`trail_meets_flush`]).
    #[inline]
    const fn rear(&self) -> usize {
        let frames = [self.open, self.blink, self.flick];
        let mut rear = 0;
        let mut f = 0;
        while f < frames.len() {
            let mut r = self.body.0;
            while r < self.body.0 + self.body.1 {
                let first = first_art(frames[f][r].as_bytes());
                if first > rear {
                    rear = first;
                }
                r += 1;
            }
            f += 1;
        }
        rear
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

/// The first art (not transparent) column of a pixel row, [`KITTY_W`] where
/// it has none.
const fn first_art(row: &[u8]) -> usize {
    let mut c = 0;
    while c < KITTY_W && row[c] == b'.' {
        c += 1;
    }
    c
}

/// Whether, on every row of `body` in `frame`, the row's first pixel is at
/// or left of column `rear` and the pixels from it to `rear` are all art:
/// the trail laid to `rear` under the cat shows left of its silhouette only,
/// never through a hole inside it nor across a strip of ground short of it.
const fn trail_meets_flush(frame: &[&str], body: (usize, usize), rear: usize) -> bool {
    let mut r = body.0;
    while r < body.0 + body.1 {
        let row = frame[r].as_bytes();
        let mut c = first_art(row);
        if c > rear {
            return false;
        }
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

/// Whether `frame`'s rows `rows` hold two eyes, each an iris (`e`) and a
/// pupil (`k`): the columns holding any pixel of an eye there (iris, pupil
/// or the catch-light `w`) fall in exactly two runs, and each run holds both
/// an iris and a pupil. An eye of iris alone goes into a grey coat
/// ([`PUPIL`]), so one drawn without its pupil stops the build, as does a
/// face with one eye, or with the two run together.
const fn two_eyes_of_iris_and_pupil(frame: &[&str], rows: (usize, usize)) -> bool {
    let mut eyes = 0;
    let (mut in_eye, mut iris, mut pupil) = (false, false, false);
    let mut c = 0;
    while c <= KITTY_W {
        let (mut e, mut k, mut any) = (false, false, false);
        let mut r = rows.0;
        while c < KITTY_W && r < rows.1 {
            let b = frame[r].as_bytes()[c];
            e = e || b == b'e';
            k = k || b == b'k';
            any = any || b == b'e' || b == b'k' || b == b'w';
            r += 1;
        }
        if any {
            if !in_eye {
                (in_eye, iris, pupil) = (true, false, false);
            }
            iris = iris || e;
            pupil = pupil || k;
        } else if in_eye {
            if !(iris && pupil) {
                return false;
            }
            eyes += 1;
            in_eye = false;
        }
        c += 1;
    }
    eyes == 2
}

/// THE SHAPE CHECKS, at both heights: every frame [`KITTY_W`] palette
/// characters by its art's height, a whole number of cell rows; the blink is
/// the open frame with its eyes closed (the eye rows alone change, and no
/// pixel of an eye — pupil, catch-light or iris — is left); the flick is the
/// open frame with its tail moved (the columns left of [`KITTY_TAIL_COLS`]
/// alone change); the open cat has two eyes, each an iris and a pupil
/// ([`two_eyes_of_iris_and_pupil`]); the body
/// band lies inside the art and holds the trail's six stripes; and the trail
/// laid to the art's [`rear`](KittyArt::rear) meets the cat flush in every
/// frame, within the tail's columns.
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
                && count_in(art.blink, art.eyes, b'w') == 0,
            "the blink is the open frame with its eyes closed"
        );
        assert!(
            two_eyes_of_iris_and_pupil(art.open, art.eyes),
            "the open cat has two eyes, each an iris and a pupil"
        );
        assert!(
            differ_only_in(art.open, art.flick, (0, h), (0, KITTY_TAIL_COLS)),
            "the flick is the open frame with its tail moved"
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

/// The kitty's nose and inner ears.
const NOSE_PINK: u32 = 0x00FF_8FAB;

/// The kitty's pupils (`k`), on every coat and both grounds: a near-black a
/// shade under `cat_baker::COAT_RAMP`'s blackest coat, so the eye reads on
/// a grey or an orange coat (the grey coat's iris is about as light as its
/// fur, and an eye of iris alone goes on it), and not the very fur of the
/// blackest one, whose eyes the iris beside the pupil (and, three rows
/// tall, the catch-light over it) carry. Not the outline's colour: on a
/// near-black coat over a dark ground the outline turns light grey.
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

// ─── the rainbow's glints ───────────────────────────────────────────────────
//
// Claude Code's mascot holds a wand tipped with a sparkle, `╱✦` (2.1.302 on,
// a session at `--effort workspace`: `crates/aterm-phase/src/fixtures/`
// `claude-2.1.302-launch-workspace-*`). Read off the 2.1.304 bundle: the `✦`
// is one glyph that never changes shape. At rest it is a steady gold (dark
// `#FFCC68`, light `#F07618`), and so it stays on the inline renderer and
// wherever motion is off. Only on the fullscreen renderer, focused, in true
// colour, does it animate, on a 3.072 s cycle of 128 ms ticks: a 256 ms
// flare to near white (bold), 384 ms cooling back to gold, an ember flicker,
// and a 512 ms wind-up to the next flare — while a crest of light runs out
// from the wand's tip across the mascot. (Its entrance's dust, `·` then `~`
// beside its feet, is a separate 60 ms puff as it lands.)
//
// The card's rainbow wears the same sparkle, in the card's own star (the
// crate's one four-point silhouette, `effect_util`): a few GLINTS sit ON
// the trail's stripes, wholly inside its band, clear of the kitty and of
// the field. Settled they are steady and gold, the family's standard star —
// Claude's `✦` at rest — so the card at rest stays constant bytes. During
// an episode they twinkle on the family law, and each FLARES as the trail's
// shimmer runs over it (the crest): it grows to the family's hero grain,
// whitens, and throws its diffraction glints, then cools back as the sheen
// moves on. On the ride each comes up once the kitty has
// passed it. On a light ground a glint on the print is the paper's white
// (the trail's print ink), as every star on the trail is.

/// The most glints one trail wears.
const GLINTS_MAX: usize = 3;

/// One glint per this many cells of the trail's free stretch (left of the
/// kitty), one at least where the band has room: one on a launch card's
/// shortest trail (8 cells), three on its longest (20).
const GLINT_CELLS: i64 = 6;

/// A glint's coverage at the twinkle's peak; settled it holds
/// [`STAR_STEADY_ENV`] of it, as the field does.
const GLINT_COV: f32 = 235.0;

/// The share of the shimmer's peak past which a flaring glint throws its
/// diffraction glints (Claude's flare goes bold).
const GLINT_FLARE_DOTS: f32 = 0.5;

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
pub(crate) const LINE_DARK_LUMA_FLOOR: f32 = 0.30;
/// THE HERO WORD (owner, 2026-10-19: "emphasize aterm in the banner"): the
/// line's `aterm` ([`LineAt::hero`]) spans this much of the palette across
/// its own letters — a whole run, red to violet under Cruise, inside the
/// line's slow gradient.
const LINE_HERO_SPREAD: f32 = 1.0;
/// A dark ground lifts the hero's letters to at least this WCAG luminance: a
/// step above the line's [`LINE_DARK_LUMA_FLOOR`], so the bold word leads
/// it, short of white so its blue and violet keep their hue.
const LINE_HERO_DARK_LUMA_FLOOR: f32 = 0.36;
/// How far a sweep or a glint pulls a hero letter toward white on a dark
/// ground: Claude's own mascot crest ([`GLINT_CREST_DARK`]), against the
/// rest of the line's [`LINE_SWEEP_GAIN`] — the word flares as the shimmer
/// passes. On light the hero is the rest of the line's own letters: the
/// paper law's ink at its place ([`LineInks`], 4.5:1 or more against the
/// pane's ground), deepened toward black by [`LINE_SWEEP_GAIN_LIGHT`], so
/// this gain is the dark ground's alone.
const LINE_HERO_SWEEP_GAIN: f32 = GLINT_CREST_DARK;
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
    #[inline]
    fn default() -> Self {
        Self {
            key: None,
            ink: [0; LINE_INK_STEPS + 1],
        }
    }
}

impl LineInks {
    /// Solve every step for `palette` on `ground`, unless they are kept.
    #[inline]
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
    #[inline]
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
    assert!(
        LINE_HERO_SWEEP_GAIN > LINE_SWEEP_GAIN && LINE_HERO_DARK_LUMA_FLOOR > LINE_DARK_LUMA_FLOOR,
        "the hero word flares harder and stands brighter than the line"
    );
    assert!(
        1000.0 / SHIMMER_MS <= TWINKLE_FLASH_BOUND_HZ,
        "a glint flares under the photosensitivity bound"
    );
};

/// The sunset ramp for [`CardPalette::Land`]: red → orange → gold → rose.
const SUNSET: [u32; 4] = [0x00FF_3B30, 0x00FF_8A1F, 0x00FF_C83D, 0x00FF_6F91];

/// The moonbow ramp for [`CardPalette::Off`], as light on a dark ground: white
/// → pearl → silver → steel — a rainbow by moonlight, which the eye sees
/// white. Cool (OKLCh hue about 259°, chroma at most 0.027) and falling in
/// OKLab L 1.00 / 0.94 / 0.84 / 0.74, each stop at least 0.066 from the next
/// (ΔE). The silver and steel half is what lets the white head and the
/// trail's shimmer, both mixes toward white, read on it, as Claude's mascot
/// mixes its body toward a white crest; every stop is at least 0.41 WCAG
/// relative luminance, so the line's lift ([`LINE_DARK_LUMA_FLOOR`]) never
/// moves it.
const MOONBOW: [u32; 4] = [0x00FF_FFFF, 0x00E4_EAF3, 0x00C3_CCDA, 0x00A2_ADBE];

const WHITE: u32 = 0x00FF_FFFF;

/// Which colours the card wears: the Autopilot it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardPalette {
    /// Cruise: the full spectrum (the rainbow family's one arc).
    #[default]
    Cruise,
    /// Land: a sunset, red → orange → gold → rose.
    Land,
    /// Off: the moonbow — a rainbow by moonlight, which the eye sees white:
    /// white → pearl → silver → steel on dark ([`MOONBOW`]), pearl → graphite
    /// in print ([`PRINT_OFF`]).
    Off,
}

impl CardPalette {
    /// The palette at position `t` (`0..=1`; clamps).
    #[inline]
    fn at(self, t: f32) -> u32 {
        match self {
            Self::Cruise => spectrum(t),
            Self::Land => sunset(t),
            Self::Off => moonbow(t),
        }
    }

    /// The card art's colour at position `t` on a dark or a light ground:
    /// [`at`](Self::at) on dark, its print twin on light
    /// ([`PRINT_CRUISE_RAMP`], so a hue sits where it does on dark;
    /// [`PRINT_LAND`] and [`PRINT_OFF`] evenly, each stripe its own ink).
    #[inline]
    fn art_at(self, t: f32, dark: bool) -> u32 {
        match (self, dark) {
            (_, true) => self.at(t),
            (Self::Cruise, false) => knot_ramp(&PRINT_CRUISE_RAMP, t),
            (Self::Land, false) => even_ramp(&PRINT_LAND, t),
            (Self::Off, false) => even_ramp(&PRINT_OFF, t),
        }
    }

    /// The card art's colour at position `t` (`0..=1`; clamps, and a NaN
    /// reads as `0`) on a dark or a light ground — the border's, the rim's —
    /// for a host's own mark in the card's colours (the composer's rule):
    /// light on dark, opaque print ink on light. Not for letters: on a light
    /// ground the print's gold and pearl are too pale to read as text
    /// ([`Self::letter`] is).
    #[must_use]
    #[inline]
    pub fn art(self, t: f32, dark: bool) -> u32 {
        self.art_at(t, dark)
    }

    /// The palette's legible letter ink at position `t` (`0..=1`; clamps)
    /// by polarity alone: on dark, the colour lifted toward white to at
    /// least [`LINE_DARK_LUMA_FLOOR`] (about 5:1 on a near-black ground, so
    /// blue and violet letters read) — the card's line at rest; on light,
    /// the shared light-ink law for marks over text (`InkRole::OverText`),
    /// for a host that tints toward its own ink (the agents panel's names).
    /// That is not the card's line on light: its letters, the hero's too,
    /// take the paper law's ink against the pane's ground ([`LineInks`],
    /// [`Self::text_at`]), as letters that must read on the pane's own
    /// ground do through [`Self::letter_on`].
    #[must_use]
    #[inline]
    pub fn letter(self, t: f32, dark: bool) -> u32 {
        let base = self.at(t);
        if dark {
            lift_to_luminance(base, LINE_DARK_LUMA_FLOOR)
        } else {
            InkRole::OverText.ink(base)
        }
    }

    /// The card's line letter at position `t` (`0..=1`; clamps) at rest on
    /// the pane's `ground` (`0x00RRGGBB`, dark or light by [`dark_ground`]),
    /// for a host's own letters in the card's colours (the composer rule's
    /// harness switch): on dark [`Self::letter`]'s lift; on light the line's
    /// paper-law ink ([`Self::text_at`]), 4.5:1 or more against `ground`.
    #[must_use]
    #[inline]
    pub fn letter_on(self, t: f32, ground: u32) -> u32 {
        let ground = ground & 0x00FF_FFFF;
        if dark_ground(ground) {
            self.letter(t, true)
        } else {
            self.text_at(t, ground)
        }
    }

    /// Where trail stripe `k` (`0` top … `5` bottom, wrapping) sits on the
    /// palette, as a position for [`Self::art`] and [`Self::letter`]: Cruise's
    /// stripe at its spectrum stop (so `art` there is the stripe's own colour,
    /// on dark and in print), Land's and Off's evenly. The agents panel's
    /// rows take their hues from it, top down (`crate::harness_panel`).
    #[must_use]
    #[inline]
    pub fn stripe_t(self, k: usize) -> f32 {
        let k = k % TRAIL_STOPS.len();
        match self {
            Self::Cruise => stripe_at(k) as f32 / (SPECTRUM_LUT_LEN - 1) as f32,
            Self::Land | Self::Off => k as f32 / (TRAIL_STOPS.len() - 1) as f32,
        }
    }

    /// Trail stripe `k` (0 top … 5 bottom) on a dark or a light ground.
    #[inline]
    fn stripe(self, k: usize, dark: bool) -> u32 {
        let k = k % TRAIL_STOPS.len();
        let even = k as f32 / (TRAIL_STOPS.len() - 1) as f32;
        match (self, dark) {
            (Self::Cruise, true) => spectrum_stop(TRAIL_STOPS[k]),
            (Self::Land, true) => sunset(even),
            (Self::Off, true) => moonbow(even),
            (Self::Cruise, false) => PRINT_CRUISE[k],
            (Self::Land, false) => PRINT_LAND[k],
            (Self::Off, false) => PRINT_OFF[k],
        }
    }

    /// A point mark's colour on a dark ground (a burst star, a sparkle): a
    /// named stop, never a between colour.
    #[inline]
    fn mark(self, k: usize) -> u32 {
        match self {
            Self::Cruise => spectrum_stop(k % 7),
            Self::Land => SUNSET[k % SUNSET.len()],
            Self::Off => MOONBOW[k % MOONBOW.len()],
        }
    }

    /// A point mark's print ink on a light ground's paper: the trail's
    /// stripe inks in turn, held to the border's luma bar ([`print_line`]),
    /// a star's arms being as thin as the border's line.
    #[inline]
    fn star_ink(self, k: usize) -> u32 {
        print_line(self.stripe(k, false))
    }

    /// The line's letter at position `t` (`0..=1`; clamps) on the light
    /// `ground`: the print ramp's ink there as the paper law's point-mark
    /// ink ([`paper_mark_ink`]), text at 4.5:1 or more against `ground`.
    /// Off's print is a silver, not a hue, and the vivid law would saturate
    /// it to a blue: its letter is the same bar reached the same way —
    /// darkened along its own tint only as far as the bar needs
    /// ([`paper_silver_ink`]) — so the white theme reads as graphite and
    /// steel on paper.
    #[inline]
    fn text_at(self, t: f32, ground: u32) -> u32 {
        match self {
            Self::Off => paper_silver_ink(self.art_at(t, false), ground),
            Self::Cruise | Self::Land => paper_mark_ink(self.art_at(t, false), ground),
        }
    }
}

/// A silver's letter ink on the light `ground`: `rgb` darkened toward black
/// (its tint kept) by the least step of a 24-step bisection that reaches
/// WCAG [`PAPER_MARK_CONTRAST`] against `ground`, as the paper law's vivid
/// inks are; a colour already there is unchanged.
#[inline]
fn paper_silver_ink(rgb: u32, ground: u32) -> u32 {
    let y_bg = relative_luminance(ground & 0x00FF_FFFF);
    let holds = |k: f32| {
        let y = relative_luminance(mix_rgb(rgb, 0, 1.0 - k));
        (y.max(y_bg) + 0.05) / (y.min(y_bg) + 0.05) >= PAPER_MARK_CONTRAST
    };
    if holds(1.0) {
        return rgb;
    }
    // On a light page darker is further from the page: black holds.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if holds(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix_rgb(rgb, 0, 1.0 - lo)
}

/// The sunset ramp at `t` (`0..=1`; clamps, and a NaN reads as `0`).
#[inline]
fn sunset(t: f32) -> u32 {
    even_ramp(&SUNSET, t)
}

/// The moonbow ramp at `t` (`0..=1`; clamps, and a NaN reads as `0`).
#[inline]
fn moonbow(t: f32) -> u32 {
    even_ramp(&MOONBOW, t)
}

/// The ramp through evenly spaced `stops` at `t` (`0..=1`; clamps, and a NaN
/// reads as `0`).
#[inline]
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
#[inline]
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
#[inline]
pub(crate) fn print_line(rgb: u32) -> u32 {
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
    /// The stream's row index of grid row 0. The fx layers need none (the
    /// engine finds the bands); the tests' quad oracle tags a quad in grid
    /// row `r` with `row_base + r`.
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
    /// Pass at most the pane's width: its last column index for a box whose
    /// right side shows, the width itself for one whose text fills the
    /// pane's last column (the box open at the pane's edge, no stroke over
    /// that column's glyph).
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
    /// Where Claude's ripple starts, `(grid column, grid row)`: its mascot's
    /// wand (the `✦` the workspace card holds, the mascot's last column on
    /// the banner row) or the mascot's right edge — the flow's glints run
    /// outward from there across the border, the trail and the stars, as
    /// Claude's own run across its mascot ([`crate::ripple`]). `None`: the
    /// box's inner top-left corner.
    pub wand: Option<(u16, u16)>,
}

/// The border's stroke at cell height `ch`: a fourteenth of the cell, at
/// least one pixel.
#[inline]
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
#[inline]
fn kitty_px(ch: u16) -> u16 {
    ch.saturating_sub(stroke_px(ch)) / KITTY_HALF as u16
}

/// How many cells wide the kitty is at cell size `cw × ch`: its
/// [`KITTY_W`] square pixels ([`kitty_px`], the band less the border's
/// stroke), rounded up to whole cells. `0` when the cells are too short for
/// the art (under [`KITTY_HALF`] px plus the stroke), or `cw` is `0`.
#[must_use]
#[inline]
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
    /// What the last tick drew ([`Drew`]): an INTRO frame leaves the flow's
    /// first frame (or, for a card that does not flow, the settled frame)
    /// to be drawn once the intro runs out, which [`Self::deadline`] asks
    /// for; a FLOW frame, the next step.
    drew: Drew,
    /// Where the kitty's ride stands ([`Ride`]): once, on the card's first
    /// showing.
    ride: Ride,
    /// THE FLOW: once armed, the card flows on after its intro for as long
    /// as it may animate ([`Self::flowing`]).
    flows: bool,
    /// The ripple's clock ([`crate::ripple`]): a ring is born at the wand at
    /// `glint0 + k·GLINT_PERIOD_MS`, and the flow's steps fall at `glint0 +
    /// k·FLOW_STEP_MS`. Set by the card's first arm, kept by every later one
    /// (the grid is the card's for life) and moved only by a lock onto
    /// Claude's own ripple ([`Self::lock_glint`]).
    glint0: Option<Instant>,
    /// Where the flow's hue started from: the intro's start where the flow
    /// takes the intro over, so the hue runs on without a jump; the instant
    /// it resumed from the settled frame, so it starts from the settled
    /// colours.
    hue0: Option<Instant>,
    /// When the flow's glints started to come up ([`GLINT_FADE_MS`]): set
    /// where the flow starts from the settled frame; `None` where they are
    /// up.
    fade0: Option<Instant>,
    /// The last frame a tick put on the glass: when, and its flow step and
    /// the grid it was drawn on (`None`: not a flow frame).
    /// [`Self::deadline`]'s next step follows it, and its backstop
    /// ([`SETTLE_GRACE_MS`]) runs from it.
    drawn: Option<(Instant, Option<(u64, u32)>)>,
    /// Bumped by every lock that moves [`Self::glint0`]: a kept flow frame
    /// is a step of one grid ([`KeptFrame`]), and a moved grid draws anew.
    grid_gen: u32,
}

/// What a tick last put on the glass ([`HarnessCard::drew`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Drew {
    /// The settled frame (or nothing: no tick yet, or nothing to draw).
    #[default]
    Settled,
    /// An intro frame.
    Intro,
    /// A flow frame.
    Flow,
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

/// Where in the card's life a frame falls: `t_ms` into its intro (`None`
/// past it: the flow, or the settled frame), how far into the kitty's ride
/// ([`Ride`]; `None` where this intro does not ride, and past it), where on
/// the ripple's clock ([`crate::ripple`]) and in the flow ([`Flow`]).
#[derive(Clone, Copy, Debug)]
struct Clock {
    t_ms: Option<u64>,
    ride_ms: Option<u64>,
    /// The ripple at this frame: ms on its clock (since the card's
    /// `glint0`, on the step grid: Claude's mascot steps every
    /// [`FLOW_STEP_MS`]) and how far up its glints are, `0..=1` (the
    /// intro's rise over its last eighth, times the fade after a resume or a
    /// re-phase, [`GLINT_FADE_MS`]). `None`: no glints (the settled frame).
    glint: Option<(u64, f32)>,
    /// The flow: `None` outside it.
    flow: Option<Flow>,
}

/// A flow frame's place: its step on the ripple's grid (whole
/// [`FLOW_STEP_MS`] since the card's `glint0`) and how far its hue has
/// travelled since the flow's anchor (ms, at the step's start).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Flow {
    step: u64,
    hue_ms: u64,
}

impl Clock {
    const SETTLED: Self = Self {
        t_ms: None,
        ride_ms: None,
        glint: None,
        flow: None,
    };

    /// `t` ms into an intro on a ripple whose clock started with it: the
    /// glints rise over the intro's last eighth.
    #[inline]
    fn intro(t: u64, ride_ms: Option<u64>) -> Self {
        let mut clock = Self {
            t_ms: Some(t),
            ride_ms,
            glint: None,
            flow: None,
        };
        clock.glint = Some((on_grid(t), clock.intro_rise()));
        clock
    }

    /// `t` ms into a card's first episode, which rides from its start.
    #[cfg(test)]
    fn riding(t: u64) -> Self {
        Self::intro(t, Some(t))
    }

    /// `t` ms into an episode that does not ride: the kitty seated
    /// throughout, the burst at the start.
    #[cfg(test)]
    fn seated(t: u64) -> Self {
        Self::intro(t, None)
    }

    /// The flow `ms` after its ripple's clock started, its hue anchored
    /// there and its glints up.
    #[cfg(test)]
    fn flowing(ms: u64) -> Self {
        let step = ms / FLOW_STEP_MS;
        Self {
            t_ms: None,
            ride_ms: None,
            glint: Some((step * FLOW_STEP_MS, 1.0)),
            flow: Some(Flow {
                step,
                hue_ms: step * FLOW_STEP_MS,
            }),
        }
    }

    /// The episode's progress, `0..1`; `1` when settled.
    #[inline]
    fn u(self) -> f32 {
        self.t_ms
            .map_or(1.0, |t| (t as f32 / EPISODE_MS as f32).min(1.0))
    }

    /// The intro's glints: up from `u = GLINT_INTRO_FROM` to its end, the
    /// head's mirror (it fades as they rise), so the flow takes them over
    /// already up.
    #[inline]
    fn intro_rise(self) -> f32 {
        smooth((self.u() - GLINT_INTRO_FROM) / (1.0 - GLINT_INTRO_FROM))
    }

    /// The border's (and the rim's) phase `φ`. In the intro, `PHI_INTRO·(1 −
    /// u)² − t / FLOW_TRIP`: one trip through the palette per [`TRIP_MS`] at
    /// its start, easing onto the flow's one per [`FLOW_TRIP_MS`]; in the
    /// flow, `−hue / FLOW_TRIP`, run on from where the intro left it (or from
    /// the settled phase where the flow resumed from the settled frame);
    /// settled, [`PHI_SETTLED`]. `φ` falls, so a colour travels clockwise:
    /// rightward along the top edge, away from the mascot, as Claude's
    /// ripple runs. `tri`'s period is 2, so only `φ mod 2` is seen — taken
    /// in whole ms, so a flow hours long keeps its precision.
    #[inline]
    fn phi(self) -> f32 {
        let trip = FLOW_TRIP_MS as f32;
        match (self.t_ms, self.flow) {
            (Some(t), _) => {
                let v = 1.0 - self.u();
                PHI_SETTLED + PHI_INTRO * v * v - t as f32 / trip
            }
            (None, Some(f)) => PHI_SETTLED - (f.hue_ms % (2 * FLOW_TRIP_MS)) as f32 / trip,
            (None, None) => PHI_SETTLED,
        }
    }

    /// The line's drift phase offset over the intro, `drift·(1 − u)²`: one
    /// trip of the line's gradient at the start, easing to a stop — the same
    /// way round as the border's; `0` past the intro (the line's hue holds:
    /// it takes the flow's glints, not its hue).
    #[inline]
    fn line_drift(self, drift: f32) -> f32 {
        if self.t_ms.is_none() {
            return 0.0;
        }
        let v = 1.0 - self.u();
        drift * v * v
    }

    /// The head's strength: ramps in over the first 4 % and out over the last
    /// 12 % of the episode; `0` when settled.
    #[inline]
    fn head_env(self) -> f32 {
        if self.t_ms.is_none() {
            return 0.0;
        }
        let u = self.u();
        smooth(u / 0.04) * smooth((1.0 - u) / 0.12)
    }
}

/// `smoothstep` on `0..=1`, clamped.
#[inline]
fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// `ms` floored onto the ripple's step grid ([`FLOW_STEP_MS`]).
#[inline]
const fn on_grid(ms: u64) -> u64 {
    ms / FLOW_STEP_MS * FLOW_STEP_MS
}

/// Whole ms from `from` to `to` (`0` where `to` is not later).
#[inline]
fn ms_between(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.saturating_duration_since(from).as_millis()).unwrap_or(u64::MAX)
}

/// `at` plus `ms`, saturating at `at` where the instant does not exist.
#[inline]
fn later(at: Instant, ms: u64) -> Instant {
    at.checked_add(Duration::from_millis(ms)).unwrap_or(at)
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
    #[inline]
    fn meet(self, o: Self) -> Self {
        Self {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }

    #[inline]
    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    #[inline]
    fn contains(self, x: i64, y: i64) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
    }

    /// `self` moved by `(dx, dy)`.
    #[cfg(test)]
    fn shift(self, dx: i64, dy: i64) -> Self {
        Self {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
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
    /// The stream's row of grid row 0: the quad oracle's row tags.
    #[cfg(test)]
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
    /// The ripple's origin in pixels ([`CardScene::wand`]): the wand cell's
    /// centre, or the inner box's top-left corner.
    wand: (f32, f32),
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
        let t = i64::from(stroke_px(s.ch));
        let wand = match s.wand {
            Some((c, r)) => (
                (ox + i64::from(c) * cw) as f32 + 0.5 * cw as f32,
                (oy + i64::from(r) * ch) as f32 + 0.5 * ch as f32,
            ),
            None => ((core.x0 + t) as f32, (core.y0 + t) as f32),
        };
        Some(Self {
            cw,
            ch,
            ox,
            oy,
            top,
            rows,
            #[cfg(test)]
            row_base: i64::from(s.row_base),
            core,
            outer,
            t,
            clip,
            wand,
        })
    }

    /// The top pixel of grid row `r`'s band.
    #[inline]
    fn band(&self, r: i64) -> i64 {
        self.oy + r * self.ch
    }

    /// The left pixel of grid column `c`.
    #[inline]
    fn col(&self, c: u16) -> i64 {
        self.ox + i64::from(c) * self.cw
    }

    /// The box inside the border's stroke as it stands with no air (inside
    /// [`Self::core`]), where the trail, the kitty, its rim and the stars
    /// are laid: the air moves the border and nothing else.
    #[inline]
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

/// Where the border's and the trail's ramps go: the fx under content, or
/// the tests' quad oracle.
trait Ramps {
    /// Lay `r`, clipped to `clip` and the box, as a horizontal ramp from `l`
    /// at its left edge to `rt` at its right.
    fn ramp(&mut self, clip: Rect, r: Rect, l: Paint, rt: Paint);

    /// Lay one slice of a border side, flat `p` over `r` clipped to `clip`
    /// and the box. A side's slices come one after another down the side,
    /// one a band.
    fn side(&mut self, clip: Rect, r: Rect, p: Paint);
}

/// The ends of the ramp `l` → `rt` over `r` once a clip has cut it to `c`:
/// read off the ramp at `c`'s edges where the clip moved them (a horizontal
/// clip keeps a ramp's colours where it cuts it), else `l` and `rt`.
#[inline]
fn clip_ramp(r: Rect, c: Rect, l: Paint, rt: Paint) -> (Paint, Paint) {
    if (c.x0, c.x1) == (r.x0, r.x1) {
        (l, rt)
    } else {
        let span = (r.x1 - r.x0) as f32;
        let at = |x: i64| ramp(l, rt, (x - r.x0) as f32 / span);
        (at(c.x0), at(c.x1))
    }
}

/// A ramp's value at fraction `f` between its ends (per channel, rounded).
/// The mode is per quad: two additive ends stay additive, two source-over ends
/// stay at least 1.
#[inline]
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
#[inline]
fn light(rgb: u32, cov: f32) -> Paint {
    Paint {
        color: premul_rgb(rgb, cov.clamp(0.0, 255.0) as u8),
        alpha: 0,
    }
}

/// Ink laid source-over at opacity `alpha` (light theme), floored at 1 so the
/// quad stays source-over.
#[inline]
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
    /// The belly and muzzle: the coat lifted toward white.
    belly: u32,
    /// The pupils ([`PUPIL`]).
    pupil: u32,
    /// The iris.
    eye: u32,
    nose: u32,
    /// The tall kitty's catch-light.
    glint: u32,
}

impl KittyInk {
    #[inline]
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
            pupil: PUPIL,
            eye: eye & 0x00FF_FFFF,
            nose: NOSE_PINK,
            glint: WHITE,
        }
    }

    /// The colour of palette character `c`; `None` is transparent.
    #[inline]
    fn of(self, c: u8) -> Option<u32> {
        match c {
            b'o' => Some(self.outline),
            b'c' => Some(self.coat),
            b'b' => Some(self.belly),
            b'k' => Some(self.pupil),
            b'e' => Some(self.eye),
            b'n' => Some(self.nose),
            b'w' => Some(self.glint),
            _ => None,
        }
    }
}

/// Which frame of its art the kitty shows `t_ms` into an episode (the open
/// frame when settled): a blink every [`BLINK_PERIOD_MS`], a tail flick
/// between them. In the flow, a blink once every [`FLOW_BLINK_PERIOD_MS`]
/// (two ripple cycles, half a cycle off the rings' births) for
/// [`FLOW_BLINK_MS`], and no flick: the cat at ease. [`KittyArt::frame`]
/// reads it.
#[inline]
fn kitty_frame(clock: Clock) -> KittyPose {
    let Some(t) = clock.t_ms else {
        return match clock.flow {
            Some(f)
                if (f.step * FLOW_STEP_MS + GLINT_PERIOD_MS / 2) % FLOW_BLINK_PERIOD_MS
                    < FLOW_BLINK_MS =>
            {
                KittyPose::Blink
            }
            _ => KittyPose::Open,
        };
    };
    let in_window = |at: u64, period: u64, len: u64| t >= at && (t - at) % period < len;
    if in_window(BLINK_AT_MS, BLINK_PERIOD_MS, BLINK_MS) {
        KittyPose::Blink
    } else if in_window(FLICK_AT_MS, FLICK_PERIOD_MS, FLICK_MS) {
        KittyPose::Flick
    } else {
        KittyPose::Open
    }
}

/// A deterministic fraction in `0..1` from `(seed, salt)`.
#[inline]
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
#[inline]
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
    #[inline]
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
    #[inline]
    fn rear(&self) -> i64 {
        self.rect.x0 + self.art.rear() as i64 * self.p
    }
}

/// The rim's width at kitty pixel `p` under a border stroke of `t` pixels: the
/// stroke, at most half a kitty pixel — so a rim strip never reaches another
/// across a transparent pixel ([`Emit::rim`]) — and `0`, no rim, for art of
/// one-pixel pixels.
#[inline]
fn rim_px(p: i64, t: i64) -> i64 {
    t.min(p / 2)
}

/// Whether pixel `(r, c)` of `frame` is art (not transparent, and inside it).
#[inline]
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
#[inline]
fn rim_solid(frame: &[&str], r: i64, c: i64) -> bool {
    art_at(frame, r, c) || (art_at(frame, r, c - 1) && art_at(frame, r, c + 1))
}

/// Which art a scene's kitty wears: the tall kitty where the host asked for
/// three rows and the box holds them, else the two-row kitty.
#[inline]
fn kitty_art(g: &Geo, s: &CardScene) -> KittyArt {
    if s.kitty_rows >= 3 && g.rows >= 3 {
        kitty_tall()
    } else {
        KITTY_SHORT
    }
}

/// The tall kitty ([`KITTY_TALL`]), or in a test the art standing in for it
/// ([`stand_in`]).
#[inline]
fn kitty_tall() -> KittyArt {
    #[cfg(test)]
    if let Some(art) = TALL_STAND_IN.with(std::cell::Cell::get) {
        return art;
    }
    KITTY_TALL
}

#[cfg(test)]
thread_local! {
    /// The tall kitty this thread's tests draw in its place ([`stand_in`]).
    static TALL_STAND_IN: std::cell::Cell<Option<KittyArt>> =
        const { std::cell::Cell::new(None) };
}

/// Test seam: draw `art` as the tall kitty on this thread (`None`: the real
/// one again). While one stands in, [`CardArts::for_cell`] builds afresh.
#[cfg(test)]
fn stand_in(art: Option<KittyArt>) {
    TALL_STAND_IN.with(|c| c.set(art));
}

/// Whether a stand-in tall kitty is drawn on this thread ([`stand_in`]).
#[cfg(test)]
fn stand_in_active() -> bool {
    TALL_STAND_IN.with(|c| c.get().is_some())
}

/// Where the kitty's art sits at pixel size `p` with its left edge at `x0`,
/// whether or not it fits: its first [`KITTY_HALF`] pixel rows bottom-aligned
/// in the first row's band, the rest from the second's top down — so no pixel
/// row straddles that boundary, and the tall kitty's head sits where the
/// two-row kitty's does (a card that gains or loses its third row's room
/// stands the cat up or down; its head does not move).
#[inline]
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
#[inline]
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
#[inline]
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
#[inline]
fn ride_at(f: f32) -> f32 {
    RIDE_MS as f32 * (1.0 - (1.0 - f.clamp(0.0, 1.0)).cbrt())
}

/// Where the ride starts and how far it runs, in pixels: from the trail's
/// first pixel inside the box and the clip to the seat's left edge. `None`
/// with no trail, or no way to go.
#[inline]
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
#[inline]
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
#[inline]
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
#[inline]
fn across_trail(b: Rect, (t, amp): (Rect, i64)) -> bool {
    let outer = Rect {
        y0: t.y0 - amp,
        y1: t.y1 + amp,
        ..t
    };
    let inside = b.x0 >= t.x0 && b.x1 <= t.x1 && b.y0 >= t.y0 + amp && b.y1 <= t.y1 - amp;
    !b.meet(outer).is_empty() && !inside
}

/// One glint on the rainbow ([`glint_spots`]): its centre and twinkle phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct GlintSpot {
    x: i64,
    y: i64,
    phase: f32,
}

/// The rainbow's glints for a scene ([`glint_spots`]): up to [`GLINTS_MAX`]
/// spots, and their arm at rest and at a flare's peak (px).
#[derive(Clone, Copy, Debug, Default)]
struct Glints {
    spots: [GlintSpot; GLINTS_MAX],
    n: usize,
    rest: i64,
    flare: i64,
}

impl Glints {
    /// The glints laid.
    #[inline]
    fn spots(&self) -> &[GlintSpot] {
        &self.spots[..self.n]
    }
}

/// A glint's arm at rest and at its flare's peak (px) on a trail band `h`
/// pixels tall whose edges the wave steps by `amp`: the family's standard
/// star at rest and its hero grain flaring (by the ink tax on a light
/// ground), each held to the largest whose reach lies inside the band at
/// every step of the wave, so a glint never pokes off the rainbow; `0`, no
/// glint, where not even a one-pixel arm fits.
#[inline]
fn glint_arms(g: &Geo, dark: bool, h: i64, amp: i64) -> (i64, i64) {
    let tax = if dark { 1.0 } else { STAR_ARM_INK };
    // A reach of `2·(arm + 1) + 1` rows inside `h − 2·amp`.
    let fits = ((h - 2 * amp - 1) / 2 - 1).max(0);
    let arm = |ratio: f32| i64::from(star_arm_px(star_arm(g.ch as f32, ratio * tax))).min(fits);
    (arm(STAR_ARM_STD), arm(STAR_ARM_HERO))
}

/// The glints' spots for this seed and geometry (§10.6.16): one per
/// [`GLINT_CELLS`] of the trail's free stretch, at most [`GLINTS_MAX`], each
/// in its own slice of it, ON a stripe — the four inner ones by turns from
/// a seeded start, so neighbours sit on different stripes — and, at its
/// flare's reach, wholly inside the band in every step of the wave (on a
/// light ground, one tone: the paper's white), clear of the kitty by that
/// reach, inside the star region (never over the card's text) and inside
/// one row band where the band has room on the stripe's side of the line.
/// None where the band is too short to hold a glint. A pure function of its
/// inputs, recomputed each tick.
fn glint_spots(g: &Geo, s: &CardScene, seed: u64) -> Glints {
    let mut out = Glints::default();
    let Some(region) = star_region(g, s) else {
        return out;
    };
    let seat = kitty_rect(g, s);
    let Some((t, amp)) = trail_rect(g, s, seat) else {
        return out;
    };
    let (rest, flare) = glint_arms(g, s.dark, t.y1 - t.y0, amp);
    if rest < 1 {
        return out;
    }
    let m = flare + 1;
    // The free stretch: the trail left of the kitty's art.
    let free = seat.map_or(t.x1, |at| at.rect.x0.min(t.x1));
    let room = Rect {
        x0: t.x0.max(region.x0) + m,
        y0: (t.y0 + amp).max(region.y0) + m,
        x1: free.min(region.x1) - m,
        y1: (t.y1 - amp).min(region.y1) - m,
    };
    if room.is_empty() {
        return out;
    }
    let cells = (free - t.x0) / g.cw.max(1);
    let n = usize::try_from(cells / GLINT_CELLS)
        .unwrap_or(0)
        .clamp(1, GLINTS_MAX);
    let stripes = TRAIL_STOPS.len() as i64;
    let first = (mix(seed ^ 0x474C_494E) % 4) as usize;
    for (i, spot) in out.spots[..n].iter_mut().enumerate() {
        let span = room.x1 - room.x0;
        let at = (i as f32 + 0.2 + 0.6 * unit(seed, 0x474C_0000 | i as u64)) / n as f32;
        let x = room.x0 + (span as f32 * at) as i64;
        // Stripes 1 to 4 by turns, two apart: upper, lower, upper.
        let k = 1 + [0i64, 2, 1, 3][(first + i) % 4];
        let centre = t.y0 + (t.y1 - t.y0) * (2 * k + 1) / (2 * stripes);
        let mut y = centre.clamp(room.y0, room.y1 - 1);
        // Off the line between two rows where the band leaves room on the
        // stripe's side of it: a star laid across it is split in two.
        let row_of = |py: i64| (py - g.oy).div_euclid(g.ch);
        if row_of(y - m) != row_of(y + m) {
            let line = g.band(row_of(y + m));
            let (above, below) = (line - m - 1, line + m);
            let side = if centre < line { above } else { below };
            if (room.y0..room.y1).contains(&side) {
                y = side;
            }
        }
        *spot = GlintSpot {
            x,
            y,
            phase: unit(seed, 0x474C_5048 ^ i as u64) * std::f32::consts::PI,
        };
    }
    Glints {
        n,
        rest,
        flare,
        ..out
    }
}

/// The field's spots for this seed and geometry: up to [`STARS_MAX`], one per
/// slice of the region's width where there is room, clear of the kitty, the
/// rainbow's glints and each other, and on a light ground clear of the trail's edge
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
    let glints = glint_spots(g, s, seed);
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
        let kitty_pad = kitty.map(|r| Rect {
            x0: r.x0 - m,
            y0: r.y0 - m,
            x1: r.x1 + m,
            y1: r.y1 + m,
        });
        let room_h = (room.y1 - room.y0) as f32;
        for k in 0..STAR_TRIES {
            let salt = ((i as u64) << 8) | k;
            let within = if k < STAR_TRIES / 2 { slice } else { room };
            let x = within.x0 + (unit(seed, salt * 2) * (within.x1 - within.x0) as f32) as i64;
            let y = room.y0 + (unit(seed, salt * 2 + 1) * room_h) as i64;
            let on_kitty = kitty_pad.is_some_and(|r| r.contains(x, y));
            let crowded = spots[..n]
                .iter()
                .map(|o| (o.x, o.y))
                .chain(glints.spots().iter().map(|o| (o.x, o.y)))
                .any(|(ox, oy)| (ox - x).abs() < gap && (oy - y).abs() < gap);
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

/// Where a line of the card's ink lies ([`HarnessCard::line_ink_into`]): one
/// grid row's cells `col..col + len`, the hero word among them, and the
/// ripple's origin in the same grid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineAt {
    /// The line's row.
    pub row: u16,
    /// Its first column.
    pub col: u16,
    /// Its length in cells.
    pub len: u16,
    /// THE HERO WORD's cells (owner, 2026-10-19: "emphasize aterm in the
    /// banner"), offsets into the line (`0..0`: none): they wear their own
    /// whole run of the palette ([`LINE_HERO_SPREAD`]) and, on dark, are
    /// lifted a step above the line ([`LINE_HERO_DARK_LUMA_FLOOR`]) and flare
    /// harder as a sweep or a glint passes ([`LINE_HERO_SWEEP_GAIN`]). The
    /// host sets them bold.
    pub hero: Range<u16>,
    /// Where Claude's ripple starts, `(column, row)` in the line's grid
    /// ([`CardScene::wand`]); `None`: the line's first cell.
    pub wand: Option<(u16, u16)>,
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
    /// (focus regained) is never left waiting on an unrelated wake. The first
    /// arm starts the ripple's clock, which every later one keeps; an armed
    /// card flows on past its intro ([`Self::flowing`]).
    #[inline]
    pub fn arm(&mut self, now: Instant, seed: u64) {
        self.episode = Episode::Armed(now);
        self.seed = seed;
        self.still = false;
        self.flows = true;
        self.glint0.get_or_insert(now);
        // The frames drawn before the arm are not this episode's to finish.
        self.drawn = None;
        // A ride run out stays out: the new episode bursts at its start.
        if let Ride::Since(from) = self.ride
            && now.saturating_duration_since(from) >= Duration::from_millis(RIDE_MS)
        {
            self.ride = Ride::Done;
        }
    }

    /// Whether [`Self::deadline`] is `None` at EVERY instant — known without
    /// a clock: the last tick held the card still (`animate == false`, a
    /// serious scene, nothing to draw), or it was never armed and no intro's
    /// clock is armed or playing. A host asks this first and reads its clock
    /// only for a card that is not at rest, so a held card costs its host's
    /// repaint key and wake judge no clock read.
    #[must_use]
    #[inline]
    pub fn at_rest(&self) -> bool {
        self.still || (!self.intro_live() && !self.flows)
    }

    /// Whether the card holds STILL at its settled frame: no intro's clock
    /// is armed or playing, and it does not flow (never armed, or held by
    /// its last tick). Its [`Self::line_ink`] is then the settled gradient
    /// at every instant.
    #[must_use]
    #[inline]
    pub fn is_still(&self) -> bool {
        !self.intro_live() && !(self.flows && !self.still)
    }

    /// Whether an intro is armed or playing: the card's six seconds of show
    /// ([`EPISODE_MS`]), not yet let go. `false` once a tick has let a
    /// played-out intro go, while one is owed, and in the flow.
    #[must_use]
    #[inline]
    pub fn intro_live(&self) -> bool {
        matches!(self.episode, Episode::Armed(_) | Episode::Playing(_))
    }

    /// Whether the card FLOWS at `now`: armed once, its intro over (or
    /// none owed), and its last tick allowed to animate — the next tick that
    /// may animate draws a flow frame. Its frames step on the ripple's grid
    /// ([`Self::flow_step`]), and [`Self::deadline`] asks for each step.
    #[must_use]
    #[inline]
    pub fn flowing(&self, now: Instant) -> bool {
        self.flows
            && !self.still
            && self.glint0.is_some()
            && !matches!(self.episode, Episode::Owed(_))
            && self.running(now).is_none()
    }

    /// The step of the ripple's grid `now` falls in: whole [`FLOW_STEP_MS`]
    /// since its clock started (`None` before the first arm). Two instants
    /// in one step draw the same flow frame.
    #[must_use]
    #[inline]
    pub fn flow_step(&self, now: Instant) -> Option<u64> {
        self.glint0.map(|g0| ms_between(g0, now) / FLOW_STEP_MS)
    }

    /// The ripple's clock's zero: the instant its glints' cycles and the
    /// flow's steps count from ([`Self::flow_step`]); `None` before the
    /// first arm or lock. Another ripple on the same glass that counts from
    /// it — the agents panel's crest, a second card — runs in phase with
    /// this one, its frames on this one's steps.
    #[must_use]
    #[inline]
    pub fn ripple_origin(&self) -> Option<Instant> {
        self.glint0
    }

    /// Start the ripple's clock at `origin` for a card whose clock has not
    /// started (never armed nor locked): a card armed on a glass where
    /// another already flows takes that one's steps, so the window draws
    /// one frame a step for both. A started clock keeps its own — a no-op —
    /// and a later lock moves an adopted one as it would any.
    #[inline]
    pub fn adopt_ripple(&mut self, origin: Instant) {
        if self.glint0.is_none() {
            self.glint0 = Some(origin);
            self.grid_gen = self.grid_gen.wrapping_add(1);
        }
    }

    /// PHASE-LOCK the ripple to Claude's own AS SEEN: its wand's flare
    /// (frame 0 of its cycle, where its ring is born) on the frame composed
    /// at `seen`. The card's ring birth — a step's start — is put
    /// [`FLARE_STEP_LEAD_MS`] before it, so Claude's repaints land inside
    /// the flow's slack, and the error `e` is that instant's distance from
    /// the nearest ring birth on the card's clock, folded into a half cycle
    /// either way. Where no glint of the
    /// card shows (its intro before the glints rise, a settled or held card)
    /// the grid takes `e` whole, unseen; where they show, it SLEWS by a
    /// quarter of `e` ([`LOCK_GAIN`]: Claude's repaint jitter averaged out,
    /// never chased) and at most one step ([`LOCK_SLEW_MS`]) a flare — never
    /// a jump of more than one step, and no fade; an error under four ms
    /// moves nothing. A grid moved bumps [`Self::grid_gen`], so a kept flow frame
    /// of the old one is drawn anew ([`Self::replay_step`]). A card never
    /// armed takes the flare's start as its clock's for its first arm. With
    /// no flare — the inline renderer, the plain mascot, a blurred Claude,
    /// the key turned off — no lock comes, and the grid free-runs where the
    /// last one left it.
    #[inline]
    pub fn lock_glint(&mut self, seen: Instant) {
        if self.glint0.is_none() {
            let at = seen
                .checked_sub(Duration::from_millis(FLARE_STEP_LEAD_MS))
                .unwrap_or(seen);
            self.glint0 = Some(at);
            self.grid_gen = self.grid_gen.wrapping_add(1);
            return;
        }
        self.slew(seen, GLINT_PERIOD_MS);
    }

    /// PHASE-LOCK the flow's STEPS to Claude's repaints AS SEEN: the frame
    /// composed at `seen` showed Claude's mascot repainted (its shimmer, a
    /// frame of its ripple). A step's start is put [`FLARE_STEP_LEAD_MS`]
    /// before it by [`Self::lock_glint`]'s rules — the error folded into
    /// half a step either way, taken whole where no glint shows, else a
    /// quarter of it ([`LOCK_GAIN`]) — so the repaints, which come about
    /// once a step, keep landing inside the flow's slack however Claude's
    /// repaint period sits off the step (127.7 ms on a real window: 7 ms a
    /// cycle, which a lock once a flare follows a quarter at a time, 28 ms
    /// behind). The ring's birth stays on the step [`Self::lock_glint`]
    /// chose. A card never armed has no grid, and nothing moves.
    #[inline]
    pub fn lock_step(&mut self, seen: Instant) {
        if self.glint0.is_some() {
            self.slew(seen, FLOW_STEP_MS);
        }
    }

    /// The locks' one rule: the grid's error against `seen` less
    /// [`FLARE_STEP_LEAD_MS`], folded into half of `period` either way,
    /// taken whole where no glint shows, else slewed a quarter of it and at
    /// most a step.
    #[inline]
    fn slew(&mut self, seen: Instant, period: u64) {
        let Some(g0) = self.glint0 else {
            return;
        };
        let at = seen
            .checked_sub(Duration::from_millis(FLARE_STEP_LEAD_MS))
            .unwrap_or(seen);
        let period = i64::try_from(period).unwrap_or(i64::MAX);
        let ahead = i64::try_from(ms_between(g0, at)).unwrap_or(i64::MAX);
        let behind = i64::try_from(ms_between(at, g0)).unwrap_or(i64::MAX);
        let mut e = (ahead - behind).rem_euclid(period);
        if e > period / 2 {
            e -= period;
        }
        let shift = if self.glints_shown(seen) {
            (e / LOCK_GAIN).clamp(-LOCK_SLEW_MS, LOCK_SLEW_MS)
        } else {
            e
        };
        if shift == 0 {
            return;
        }
        let by = Duration::from_millis(shift.unsigned_abs());
        self.glint0 = Some(if shift > 0 {
            g0.checked_add(by).unwrap_or(g0)
        } else {
            g0.checked_sub(by).unwrap_or(g0)
        });
        self.grid_gen = self.grid_gen.wrapping_add(1);
    }

    /// Whether any glint of the card shows on the glass at `now`, as its
    /// last tick left it: an intro past the glints' rise
    /// ([`GLINT_INTRO_FROM`]), or the flow; never a settled or held card.
    #[inline]
    fn glints_shown(&self, now: Instant) -> bool {
        !self.still
            && match self.running(now) {
                Some(ms) => Clock::intro(ms, None).intro_rise() > 0.0,
                None => self.flowing(now),
            }
    }

    /// Whether a frame with motion verdict `animate` on a scene that is
    /// `serious` or not, and has something to draw or not (`drawable`),
    /// draws the SETTLED frame and leaves the episode where it is, at any
    /// instant: the card has no episode and does not flow, or owes or plays
    /// one that this frame holds back, or flows and this frame holds it back
    /// (it may not animate — the window lost focus, Reduce Motion, the load
    /// latch — or has nothing to draw, and the scene is not serious, which
    /// would end an episode). Such a frame is a function of the scene's
    /// shape and the seed alone (`the_card_is_deterministic`), so its host
    /// may replay the last one ([`Self::replay`]). An owed episode whose
    /// debt lapsed meanwhile ([`OWED_MAX_MS`]) is let go by the next frame
    /// that draws, which starts nothing either way.
    #[inline]
    fn holds(&self, serious: bool, drawable: bool, animate: bool) -> bool {
        let may = animate && drawable;
        match self.episode {
            Episode::Idle => !(self.flows && may && !serious),
            Episode::Owed(_) | Episode::Playing(_) => !serious && !may,
            Episode::Armed(_) => false,
        }
    }

    /// The settled frame `frames` keeps, REPLAYED — a HELD card's: where a
    /// frame on `scene` with motion verdict `animate` would draw the very
    /// settled frame `frames` holds — the card holds still ([`Self::holds`])
    /// and the scene's [`CardShape`] is the kept frame's — the card is left
    /// as that frame would leave it and the kept contents come back at the
    /// scene's origin: the SAME `Arc`s frame after frame, so the renderer
    /// compares two pointers and builds nothing, and a scene that only moved
    /// (an inline scroll under a held card) is the same contents at a new
    /// offset. `None`, the card untouched, where a frame must be drawn
    /// ([`Self::replay_step`], [`Self::draw`]). No clock is read. A kept
    /// FLOW frame is never replayed here: the first held frame after the
    /// flow is drawn, and the later ones replay it.
    ///
    /// `frames` serves ONE card: every way into a state that replays (no
    /// episode, one owed, the flow) is a [`Self::draw`] — an arm leaves them
    /// all — and every draw of a settled or a flow frame keeps it, so what
    /// `frames` holds is this card's frame at its current seed whenever it
    /// is replayed. An intro frame lets it go.
    pub fn replay(
        &mut self,
        scene: &CardScene,
        animate: bool,
        frames: &CardFrames,
    ) -> Option<CardFrame> {
        let kept = frames.kept()?;
        if kept.step.is_some() {
            return None;
        }
        let (shape, origin) = CardShape::of(scene);
        if kept.shape != shape {
            return None;
        }
        let drawable = shape.drawable();
        if !self.holds(scene.serious, drawable, animate) {
            return None;
        }
        // The state change of the frame it replays: `step`'s own, with
        // `advance` leaving the episode where it is.
        self.palette = scene.palette;
        self.still = !(animate && drawable) || scene.serious;
        self.drew = Drew::Settled;
        Some(kept.at(origin))
    }

    /// The flow's replay BETWEEN TWO STEPS: where the card flows, may
    /// animate, and `frames` keeps the flow frame of `now`'s very step on
    /// the ripple's current grid for this very scene's [`CardShape`], the
    /// card is left as the frame would leave it and the kept contents come
    /// back at the scene's origin, the same `Arc`s; `None`, the card
    /// untouched, where a frame must be drawn ([`Self::draw`]). A
    /// keystroke's present, or streamed output's, between two steps costs
    /// the flowing card the clock read that found the step and two pointer
    /// copies.
    pub fn replay_step(
        &mut self,
        scene: &CardScene,
        animate: bool,
        frames: &CardFrames,
        now: Instant,
    ) -> Option<CardFrame> {
        if !animate
            || scene.serious
            || !self.flows
            || self.drew != Drew::Flow
            || self.episode != Episode::Idle
        {
            return None;
        }
        let kept = frames.kept()?;
        let step = self.flow_step(now)?;
        if kept.step != Some((step, self.grid_gen)) {
            return None;
        }
        let (shape, origin) = CardShape::of(scene);
        if kept.shape != shape {
            return None;
        }
        self.palette = scene.palette;
        self.still = false;
        self.drawn = Some((now, Some((step, self.grid_gen))));
        Some(kept.at(origin))
    }

    /// How far into a running episode `now` falls, in ms; `None` when no
    /// episode's clock is running at `now` (none armed, one owed, one played
    /// out).
    #[inline]
    fn running(&self, now: Instant) -> Option<u64> {
        let (Episode::Armed(start) | Episode::Playing(start)) = self.episode else {
            return None;
        };
        let ms = ms_between(start, now);
        (ms < EPISODE_MS).then_some(ms)
    }

    /// The next wake: one [`FRAME_MS`] ahead while an intro animates; while
    /// the card flows, the next step's start on the ripple's grid plus
    /// [`FLOW_SLACK_MS`] — so a present that lands at the step anyway
    /// (the vitals band, a keystroke) draws it first — a fixed instant
    /// however often it is asked; `now` itself when the last tick drew an
    /// intro frame and the intro has run out since (or the grid moved under
    /// the last flow frame), so the flow's first frame is drawn promptly
    /// rather than on the host's next unrelated wake. Every answer but the
    /// intro's stops [`SETTLE_GRACE_MS`] past the last frame a tick drew: a
    /// host that stopped ticking yet asks each loop is let idle after the
    /// grace instead of spinning. `None` otherwise (unarmed, an episode
    /// owed, after a tick with `animate == false`, a serious scene or
    /// nothing to draw), so a held card idles at zero.
    ///
    /// THE HOST'S CONTRACT: ask this only of a card the host TICKED THIS
    /// FRAME. The answer describes the card as its last tick left it, and
    /// only the host knows whether that tick still stands on the glass: a
    /// card the host did not tick — a frame scrolled back, the pane hidden,
    /// the card shown bare (Off, the master switch, `card = false`) or its
    /// version not the process's — registers no deadline, else the card's
    /// frames would be asked for after it left the screen.
    /// [`SETTLE_GRACE_MS`] is the backstop for a host that breaks this, not
    /// the rule.
    #[inline]
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        if self.at_rest() {
            return None;
        }
        if self.running(now).is_some() {
            return now.checked_add(Duration::from_millis(FRAME_MS));
        }
        if matches!(self.episode, Episode::Owed(_)) {
            return None;
        }
        let (at, step) = self.drawn?;
        if now >= later(at, SETTLE_GRACE_MS) {
            return None;
        }
        match (self.drew, step, self.glint0) {
            (Drew::Flow, Some((k, grid)), Some(g0)) if grid == self.grid_gen => g0.checked_add(
                Duration::from_millis((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS),
            ),
            (Drew::Intro | Drew::Flow, _, _) if self.flows => Some(now),
            _ => None,
        }
    }

    /// Draw this frame: the border and the trail into its `under` content
    /// (drawn beneath the glyph ink), the kitty, its rim and the stars into
    /// its `over` content, card-local, laid at the scene's origin
    /// ([`CardFrame`], [`card_layers`]). `animate` is the motion policy's
    /// verdict ([`crate::motion::MotionEffect::HarnessCard`]).
    ///
    /// `false` gives the settled frame and holds the card: an intro not yet
    /// drawn animating is OWED ([`Episode::Owed`]) and plays from the first
    /// frame with `animate == true`, from that frame's `now` (within
    /// [`OWED_MAX_MS`] of the arm); one already playing keeps its clock; the
    /// flow resumes from the settled colours. A scene with nothing to draw
    /// (an empty or inverted clip, no rows, columns or cell size: a pane
    /// mid-layout) is held back the same way, whatever `animate` says, so an
    /// intro is never spent on frames nobody saw; its frame holds nothing.
    /// `scene.serious` gives the settled frame and ends any intro.
    ///
    /// Every frame is built in `frames`' ring, refilled in place, so the
    /// card allocates nothing once the ring is warm; the settled frame and a
    /// flow step are kept there too, which [`Self::replay`] (a held card) and
    /// [`Self::replay_step`] (a flowing one, until its next step) hand back.
    pub fn draw(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        frames: &mut CardFrames,
    ) -> CardFrame {
        let (shape, origin) = CardShape::of(scene);
        let local = shape.scene();
        let geo = Geo::new(local).filter(|_| CardArts::dimensions(local.ch).is_some());
        let clock = self.step(now, scene, animate, geo.is_some());
        let (under, over) = match geo.zip(clock) {
            Some((geo, clock)) => frames.fill(&Emit::new(local, geo, clock, self.seed)),
            None => CardFrames::nothing(),
        };
        let step = clock.and_then(|c| c.flow).map(|f| (f.step, self.grid_gen));
        frames.keep((self.drew != Drew::Intro).then(|| KeptFrame {
            shape,
            step,
            under: Arc::clone(&under),
            over: Arc::clone(&over),
        }));
        CardFrame {
            under,
            over,
            origin,
        }
    }

    /// Step the card for a frame at `now` on `scene` (`drawable`: whether it
    /// has anything to draw): the palette its line wears, the light line's
    /// inks, the motion verdict, the episode ([`Self::advance`]) and the
    /// ride. Returns the frame's clock; `None` when the scene has nothing to
    /// draw, which holds the episode back rather than starting its clock.
    fn step(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        drawable: bool,
    ) -> Option<Clock> {
        self.palette = scene.palette;
        if !scene.dark {
            self.inks.solve(scene.palette, scene.ground & 0x00FF_FFFF);
        }
        let animate = animate && drawable;
        self.still = !animate || scene.serious;
        let clock = self.advance(now, animate, scene.serious);
        self.drew = match (clock.t_ms, clock.flow) {
            (Some(_), _) => Drew::Intro,
            (None, Some(_)) => Drew::Flow,
            (None, None) => Drew::Settled,
        };
        self.drawn = Some((now, clock.flow.map(|f| (f.step, self.grid_gen))));
        if !drawable {
            return None;
        }
        if clock.t_ms.is_none() {
            // The settled frame or the flow's goes on the glass: the kitty
            // is seated there, and stays seated through every episode after
            // ([`Ride`]).
            self.ride = Ride::Done;
        }
        Some(clock)
    }

    /// Step the episode for a frame at `now` and say where in it the frame
    /// falls ([`Episode`]'s transitions): serious mode ends it; a debt
    /// older than [`OWED_MAX_MS`] lapses; a held-back tick owes an episode
    /// not yet drawn and keeps a playing one's clock; a tick that may
    /// animate starts an owed one from `now`, draws a running one, and lets
    /// one that ran out go — into the flow, for a card armed once.
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
                    Ride::Since(from) => Some(ms_between(from, now)),
                    Ride::Done => None,
                };
                self.on_ripple(Clock::intro(ms, ride_ms), now)
            }
            (episode, _) => {
                self.episode = Episode::Idle;
                if !self.flows {
                    return Clock::SETTLED;
                }
                // Where the flow runs on from: the intro it takes over
                // (the hue continues from where it left it, its glints
                // already up), the flow it continues, or — a resume from
                // the settled frame — the settled colours, now.
                let intro = match episode {
                    Episode::Armed(start) | Episode::Playing(start) => Some(start),
                    _ => None,
                };
                match (self.drew, intro) {
                    (Drew::Intro, Some(start)) => self.hue0 = Some(start),
                    (Drew::Flow, _) if self.hue0.is_some() => {}
                    _ => {
                        self.hue0 = Some(now);
                        self.fade0 = Some(now);
                    }
                }
                self.glint0.get_or_insert(now);
                self.flow_at(now)
            }
        }
    }

    /// The intro's `clock` with its glints on the card's own ripple — the
    /// clock its first arm started, not the intro's — at `now`'s step.
    #[inline]
    fn on_ripple(&self, mut clock: Clock, now: Instant) -> Clock {
        if let Some(g0) = self.glint0 {
            let ms = on_grid(ms_between(g0, now));
            clock.glint = Some((ms, clock.intro_rise() * self.fade_at(later(g0, ms))));
        }
        clock
    }

    /// How far up the glints are at `at` after the last fade's start
    /// ([`GLINT_FADE_MS`]); `1` with none.
    #[inline]
    fn fade_at(&self, at: Instant) -> f32 {
        self.fade0.map_or(1.0, |f| {
            smooth(ms_between(f, at) as f32 / GLINT_FADE_MS as f32)
        })
    }

    /// The flow's clock at `now`, as the card stands: `now`'s step on the
    /// ripple's grid, the hue and the glints' fade read at the step's start
    /// (so a step's frame is a function of the step alone).
    #[inline]
    fn flow_at(&self, now: Instant) -> Clock {
        let Some(g0) = self.glint0 else {
            return Clock::SETTLED;
        };
        let step = ms_between(g0, now) / FLOW_STEP_MS;
        let ms = step * FLOW_STEP_MS;
        let step_at = later(g0, ms);
        Clock {
            t_ms: None,
            ride_ms: None,
            glint: Some((ms, self.fade_at(step_at))),
            flow: Some(Flow {
                step,
                hue_ms: self.hue0.map_or(ms, |h| ms_between(h, step_at)),
            }),
        }
    }

    /// The colours of the line under the card ("enhanced by aterm harness
    /// …"), one [`InkCell`] per cell of `row`, columns `col..col + len`,
    /// sorted and unique. During the intro (and `animate`) the palette
    /// drifts and a bright band sweeps the line twice; in the flow the
    /// ripple's glints run along it from the line's first cell; otherwise
    /// it is the settled gradient, the palette end to end, constant bytes.
    /// The line wears the last tick's palette. No hero word ([`LineAt`]).
    ///
    /// `ground` is the pane's background, `0x00RRGGBB`: dark or light by
    /// [`dark_ground`]. Ink stays legible: on dark every letter is lifted to
    /// a relative luminance floor; on light it is the print ramp's ink in
    /// the paper law's point-mark ink ([`paper_mark_ink`], 4.5:1 or more
    /// against `ground`), opaque, and the sweep only deepens it.
    #[inline]
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
        let at = LineAt {
            row,
            col,
            len,
            ..LineAt::default()
        };
        self.line_ink_into(Some(now), &at, ground, animate, &mut out);
        out
    }

    /// [`Self::line_ink`] at `at` — its hero word ([`LineAt::hero`]) in its
    /// own run, its glints from the wand ([`LineAt::wand`]) — appended to
    /// `out`: a host that keeps the line's buffer allocates nothing for the
    /// card's frames once it is grown. `now` is `None` for a card the host
    /// knows is still ([`Self::is_still`], a frame it replayed): a still
    /// card's line is the settled gradient at every instant, and no clock
    /// need be read for it.
    pub fn line_ink_into(
        &self,
        now: Option<Instant>,
        at: &LineAt,
        ground: u32,
        animate: bool,
        out: &mut Vec<InkCell>,
    ) {
        let ground = ground & 0x00FF_FFFF;
        let dark = dark_ground(ground);
        debug_assert!(
            now.is_some() || self.is_still() || !animate,
            "a card that moves needs the clock for its line"
        );
        let clock = match now {
            Some(now) if animate => match self.running(now) {
                Some(ms) => self.on_ripple(Clock::intro(ms, None), now),
                None if self.flowing(now) => self.flow_at(now),
                None => Clock::SETTLED,
            },
            _ => Clock::SETTLED,
        };
        let phi = clock.line_drift(LINE_DRIFT);
        let env = clock.head_env();
        let pos = (clock.u() * LINE_SWEEPS).fract() * 1.4 - 0.2;
        let span = f32::from(at.len.saturating_sub(1).max(1));
        let hero = at.hero.clone();
        let hero_span = f32::from(hero.end.saturating_sub(hero.start).saturating_sub(1).max(1));
        let (wc, wr) = at.wand.unwrap_or((at.col, at.row));
        let glint_at = |c: u16| match clock.glint {
            Some((ms, up)) if up > 0.0 => {
                let d = cell_distance(
                    f32::from(c) - f32::from(wc),
                    f32::from(at.row) - f32::from(wr),
                );
                up * glint(d, ms)
            }
            _ => 0.0,
        };
        let (palette, inks) = (self.palette, &self.inks);
        out.extend(
            (0..at.len)
                .map_while(|i| Some((i, at.col.checked_add(i)?)))
                .map(|(i, c)| {
                    let x = f32::from(i) / span;
                    let place = tri(x + phi);
                    let sweep = if env == 0.0 {
                        0.0
                    } else {
                        let d = (x - pos) / LINE_SWEEP_SIGMA;
                        env * (-d * d).exp()
                    };
                    // The sweep's crest (the intro's) or the ripple's glint
                    // (its last eighth, and the flow), whichever is higher.
                    let crest = sweep.max(glint_at(c));
                    let rgb = if hero.contains(&i) {
                        // The hero: its own whole run of the palette, a
                        // step brighter on dark, flaring harder.
                        let k = f32::from(i - hero.start) / hero_span;
                        let place = tri(k * LINE_HERO_SPREAD + phi);
                        if dark {
                            mix_rgb(
                                lift_to_luminance(palette.at(place), LINE_HERO_DARK_LUMA_FLOOR),
                                WHITE,
                                crest * LINE_HERO_SWEEP_GAIN,
                            )
                        } else {
                            // Text: the paper law's ink at its place, as the
                            // rest of the line; the crest only deepens it.
                            mix_rgb(
                                inks.at(palette, ground, place),
                                0,
                                crest * LINE_SWEEP_GAIN_LIGHT,
                            )
                        }
                    } else if dark {
                        // The palette's letter ink (the badge a host draws
                        // in the card's colours shares it), swept toward
                        // white.
                        mix_rgb(palette.letter(place, true), WHITE, crest * LINE_SWEEP_GAIN)
                    } else {
                        // Toward black: on a light ground darker only reads
                        // further from it.
                        mix_rgb(
                            inks.at(palette, ground, place),
                            0,
                            crest * LINE_SWEEP_GAIN_LIGHT,
                        )
                    };
                    InkCell {
                        row: at.row,
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
#[inline]
pub(crate) fn lift_to_luminance(rgb: u32, floor: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    if crate::color_math::relative_luminance_with(table, rgb) >= floor {
        return rgb;
    }
    let reaches =
        |t: f32| crate::color_math::relative_luminance_with(table, mix_rgb(rgb, WHITE, t)) >= floor;
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
#[inline]
pub fn dark_ground(bg: u32) -> bool {
    relative_luminance(bg) < 0.5
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
    /// The frame's border and rim phase ([`Clock::phi`]), the head's
    /// strength ([`Clock::head_env`]) and place on the arc, and the intro
    /// sheen's place on the trail: each read once a frame, not per pixel.
    phi: f32,
    head_env: f32,
    head_pos: f32,
    shimmer_pos: f32,
    /// The kitty where it is this frame ([`ridden`]) and the frame of its
    /// art it wears.
    kitty: Option<(KittyAt, KittyPose)>,
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
            phi: clock.phi(),
            head_env: clock.head_env(),
            head_pos: (clock.u() * HEAD_LAPS).fract(),
            shimmer_pos: clock
                .t_ms
                .map_or(0.0, |t| (t as f32 / SHIMMER_MS).fract() * 1.4 - 0.2),
            kitty: seat.map(|at| (ridden(&geo, scene, at, clock), kitty_frame(clock))),
            seat,
        }
    }

    /// Whether the kitty rides in this episode: it is drawn, and there is a
    /// trail to ride.
    #[inline]
    fn rides(&self) -> bool {
        self.seat
            .is_some_and(|seat| ride_span(&self.geo, self.scene, &seat).is_some())
    }

    /// The ripple's glint at pixel `(x, y)` this frame (`0..=1`, times how
    /// far up the glints are): Claude's ring train ([`glint`]) at the
    /// point's distance in cells from the wand ([`Geo::wand`],
    /// [`cell_distance`]). `0` with no glints (the settled frame, the
    /// intro's first seven eighths).
    #[inline]
    fn ring_at(&self, x: f32, y: f32) -> f32 {
        let Some((ms, up)) = self.clock.glint.filter(|&(_, up)| up > 0.0) else {
            return 0.0;
        };
        let g = self.geo;
        let d = cell_distance((x - g.wand.0) / g.cw as f32, (y - g.wand.1) / g.ch as f32);
        up * glint(d, ms)
    }

    /// The border's paint at arc fraction `s`, at pixel `(x, y)` (where the
    /// glint is read): the palette at `tri(2s + φ)`, pulled toward white on
    /// dark (toward its own deeper shade on light: a white glint would be a
    /// hole in white) by the intro's head or the ripple's crest, whichever
    /// is higher.
    #[inline]
    fn border_paint(&self, s: f32, (x, y): (f32, f32)) -> Paint {
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + self.phi), self.scene.dark);
        let env = self.head_env;
        let w = if env > 0.0 {
            let d = (s - self.head_pos).rem_euclid(1.0);
            let d = d.min(1.0 - d) / HEAD_SIGMA;
            env * (-d * d).exp()
        } else {
            0.0
        };
        let ring = self.ring_at(x, y);
        if self.scene.dark {
            let w = (w * HEAD_GAIN).max(GLINT_CREST_DARK * ring);
            light(mix_rgb(base, WHITE, w), f32::from(BORDER_COV))
        } else {
            let w = w.max(GLINT_CREST_LIGHT * ring);
            ink(
                mix_rgb(print_line(base), 0, w * LIGHT_HEAD_DEEPEN),
                PRINT_ALPHA,
            )
        }
    }

    /// The border: top edge left → right, right side down, bottom edge right
    /// → left, left side up; arc fraction measured from the top-left corner.
    /// Laid on the OUTER rectangle, its air included: a top edge with air
    /// lies in the band of the row over the box, a bottom edge in the band
    /// of the row under it, and each side gains a slice in each of those
    /// bands, every slice one flat colour ([`Ramps::side`]).
    fn border(&self, out: &mut impl Ramps) {
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
            let mid = (y0 + t / 2) as f32;
            let mut pa = self.border_paint(arc(o.x0) / per, (o.x0 as f32, mid));
            for k in 0..n {
                let xa = o.x0 + w * k / n;
                let xb = o.x0 + w * (k + 1) / n;
                let r = Rect {
                    x0: xa,
                    y0,
                    x1: xb,
                    y1: y0 + t,
                };
                let pb = self.border_paint(arc(xb) / per, (xb as f32, mid));
                out.ramp(all, r, pa, pb);
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
                let p = self.border_paint(
                    arc((ya + yb) / 2) / per,
                    ((x0 + t / 2) as f32, ((ya + yb) / 2) as f32),
                );
                let slice = Rect {
                    x0,
                    y0: ya,
                    x1: x0 + t,
                    y1: yb,
                };
                out.side(all, slice, p);
            }
        }
    }

    /// The step wave's displacement (px, `+` up) for segment `k` of the
    /// trail `lay`: none for a segment that reaches under the kitty (past
    /// [`TrailLay::held`]). The cat holds the ribbon's end still and the wave
    /// runs along the free stretch behind it, so no stepped stripe shows past
    /// the cat's outline where its art leaves the ground open beside the
    /// trail's band (above the tail swept flat in the flick, under the
    /// haunch).
    #[inline]
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

    /// The shimmer at trail fraction `x`, pixel `(px, py)`: the intro's
    /// sweep weight, and the ripple's glint ([`Self::ring_at`]).
    #[inline]
    fn shimmer(&self, x: f32, (px, py): (f32, f32)) -> (f32, f32) {
        let sweep = if self.clock.t_ms.is_none() {
            0.0
        } else {
            let d = (x - self.shimmer_pos) / SHIMMER_SIGMA;
            self.head_env * (-d * d).exp() * SHIMMER_GAIN
        };
        (sweep, self.ring_at(px, py))
    }

    /// One stripe's paint under the shimmer `(w, ring)` ([`Self::shimmer`]):
    /// on dark, light lifted toward white by the sweep `w` or the ripple's
    /// crest, whichever is higher; on a light ground the sweep is a gloss
    /// on the print ink, and the ripple's crest deepens it (as the border's
    /// does), the top and bottom stripes taking [`LIGHT_GLINT_EDGE`] of
    /// both.
    #[inline]
    fn stripe_paint(&self, k: usize, (w, ring): (f32, f32)) -> Paint {
        let base = self.scene.palette.stripe(k, self.scene.dark);
        if self.scene.dark {
            let w = w.max(GLINT_CREST_DARK * ring);
            light(mix_rgb(base, WHITE, w), f32::from(TRAIL_COV))
        } else {
            let edge = k == 0 || k + 1 == TRAIL_STOPS.len();
            let share = if edge { LIGHT_GLINT_EDGE } else { 1.0 };
            let deep = GLINT_CREST_LIGHT * ring * LIGHT_HEAD_DEEPEN * share;
            ink(
                mix_rgb(mix_rgb(base, WHITE, w * share), 0, deep),
                PRINT_ALPHA,
            )
        }
    }

    /// The rainbow trail: six stripes from the text's end to the kitty's
    /// rear, under its art to the haunch ([`KittyAt::rear`]), as tall as the
    /// kitty's body ([`trail_band`]: the cat leads, the rainbow trails).
    /// During the intro it steps and shimmers in segments, and in the flow
    /// the ripple's glints run along them; settled, every segment of a
    /// stripe would wear the same paint, so each stripe is one ramp end to
    /// end: six items of trail, not ninety six.
    ///
    /// The step wave moves the ribbon's outline, not its stripes: a segment
    /// stepped up `d` px grows the top stripe up by `d` and shrinks the bottom
    /// stripe by as much (and the reverse stepped down), so the silhouette
    /// steps while the stripes between keep their places.
    fn trail(&self, out: &mut impl Ramps) {
        let Some(lay) = self.trail_lay() else {
            return;
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
        let mid = (ya + yb) as f32 * 0.5;
        let mut wa = self.shimmer(0.0, (tx0 as f32, mid));
        let mut ends: [Paint; TRAIL_STOPS.len()] =
            std::array::from_fn(|s| self.stripe_paint(s, wa));
        while xa < drawn {
            let xb = (xa + seg).min(drawn);
            let d = self.wave(&lay, k);
            let wb = self.shimmer((xb - tx0) as f32 / len as f32, (xb as f32, mid));
            let same_w = wb == wa;
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
                let pb = if same_w {
                    *end
                } else {
                    self.stripe_paint(s, wb)
                };
                out.ramp(inner, r, *end, pb);
                *end = pb;
            }
            wa = wb;
            xa = xb;
            k += 1;
        }
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
        // Held from a rim's width short of the cat: every segment its rim
        // could touch holds still, so the rim's art is one picture a frame
        // of the kitty and the trail cuts it in one hole ([`Emit::rim_hole`]).
        let held = kitty
            .map(|at| at.rect.x0 - rim_px(at.p, g.t))
            .filter(|&held| drawn > held)
            .unwrap_or(i64::MAX);
        let seg = if self.clock.t_ms.is_some() || self.clock.flow.is_some() {
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
    #[inline]
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

    /// The rim's paint for a strip centred at `(x, y)`: the border's spectrum
    /// at the strip's angle round the kitty's centre `(cx, cy)`, measured
    /// from straight up — `palette(tri(2s + φ))`, the border's own sweep and
    /// drift, so the hue runs round the cat once with no seam and flows and
    /// settles with the border's — and the ripple's crest at the strip, as
    /// on the border (toward white on dark, deeper on light).
    #[inline]
    fn rim_paint(&self, (x, y): (f32, f32), (cx, cy): (f32, f32)) -> Paint {
        let s = ((x - cx).atan2(cy - y) / std::f32::consts::TAU).rem_euclid(1.0);
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + self.phi), self.scene.dark);
        let ring = self.ring_at(x, y);
        if self.scene.dark {
            light(
                mix_rgb(base, WHITE, GLINT_CREST_DARK * ring),
                f32::from(RIM_COV),
            )
        } else {
            ink(
                mix_rgb(base, 0, GLINT_CREST_LIGHT * ring * LIGHT_HEAD_DEEPEN),
                PRINT_ALPHA,
            )
        }
    }

    /// Where the trail cuts the rim this frame: the trail as it is drawn,
    /// `[tx0, drawn) × [ya, yb)`. Every segment the rim could touch holds
    /// still ([`TrailLay::held`]: from a rim's width short of the cat), so
    /// the segments under the rim lie flat in the trail's band and the cut
    /// is one rectangle, whatever the wave does behind the cat: the rim
    /// never lies on the trail (the rainbow is no dark ground to set the
    /// coat off, and light on light doubles to white). `None` where no trail
    /// is drawn.
    #[inline]
    fn rim_hole(&self) -> Option<Rect> {
        let t = self.trail_lay()?;
        Some(Rect {
            x0: t.tx0,
            y0: t.ya,
            x1: t.drawn,
            y1: t.yb,
        })
    }

    /// The trail's ink this frame on a light ground, segment by segment as
    /// [`Self::trail`] lays it ([`Self::trail_cover`]): where a light mark is
    /// the paper's white (each pixel its own tone). None on dark, and where no trail
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
    /// ([`CardPalette::star_ink`]), which each pixel its own tone turns the paper's
    /// white where it lies on the trail.
    #[inline]
    fn mark_ink(&self, k: usize, dark: u32) -> u32 {
        if self.scene.dark {
            dark
        } else {
            self.scene.palette.star_ink(k)
        }
    }

    /// The rainbow's glints: steady at rest; flaring as the intro's sheen
    /// passes, and as the ripple's crest does (the intro's last eighth and
    /// the flow), Claude's `✦` flaring with its ring. Gold at rest (pearl
    /// on the moonbow, whose stars are starlight), white at a flare's peak.
    fn glints(&self, lay: &mut impl FnMut(StarCall)) {
        let Some(region) = star_region(&self.geo, self.scene) else {
            return;
        };
        let glints = glint_spots(&self.geo, self.scene, self.seed);
        let Some(trail) = self.trail_lay().filter(|_| glints.n > 0) else {
            return;
        };
        let (rest, flare) = (glints.rest, glints.flare);
        let u = self.clock.u();
        let settle = smooth((u - (1.0 - STAR_SETTLE_SPAN)) / STAR_SETTLE_SPAN);
        let riding = self
            .seat
            .and_then(|seat| ride_span(&self.geo, self.scene, &seat));
        let t_sec = self.clock.t_ms.map(|t| t as f32 / 1000.0);
        let rest_rgb = match self.scene.palette {
            CardPalette::Off => MOONBOW[1],
            CardPalette::Cruise | CardPalette::Land => twinkle_rgb(true),
        };
        for (i, s) in glints.spots().iter().enumerate() {
            let up = match (self.clock.ride_ms, riding) {
                (Some(t), Some((from, dist))) => {
                    let passed = ride_at((s.x + flare + 1 - from) as f32 / dist as f32);
                    smooth((t as f32 - passed) / STAR_REVEAL_MS)
                }
                _ => 1.0,
            };
            if up <= 0.0 {
                continue;
            }
            let (sweep, ring) = self.shimmer(
                (s.x - trail.tx0) as f32 / trail.len as f32,
                (s.x as f32, s.y as f32),
            );
            let w = (sweep / SHIMMER_GAIN).max(ring).clamp(0.0, 1.0);
            let env = match t_sec {
                Some(t_sec) => {
                    let e = twinkle_env(t_sec, s.phase);
                    (e + (STAR_STEADY_ENV - e) * settle) * up
                }
                None => STAR_STEADY_ENV,
            };
            let env = (env + (1.0 - STAR_STEADY_ENV) * ring).min(1.0);
            let arm = rest + ((flare - rest) as f32 * w).round() as i64;
            lay(StarCall {
                clip: region,
                centre: (s.x, s.y),
                arm,
                cov: GLINT_COV * env,
                gold: w >= GLINT_FLARE_DOTS,
                color: self.mark_ink(i, mix_rgb(rest_rgb, WHITE, w)),
            });
        }
    }

    /// The steady field: twinkling during the intro (easing onto a steady
    /// brightness over its last fifth), fixed once settled; in the flow
    /// steady but for the ripple, each star flaring (and glinting gold) as
    /// a ring passes it, twice a cycle — as the intro's last eighth does.
    fn field(&self, lay: &mut impl FnMut(StarCall)) {
        let Some(region) = star_region(&self.geo, self.scene) else {
            return;
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
        let t_sec = self.clock.t_ms.map(|t| t as f32 / 1000.0);
        let chf = self.geo.ch as f32;
        let (even_rgb, odd_rgb) = match self.scene.palette {
            CardPalette::Cruise => (twinkle_rgb(true), twinkle_rgb(false)),
            CardPalette::Land => (twinkle_rgb(true), mix_rgb(SUNSET[3], WHITE, 0.5)),
            // White stars and pearl ones: starlight by the moonbow.
            CardPalette::Off => (WHITE, MOONBOW[1]),
        };
        for (i, s) in spots[..n].iter().enumerate() {
            let arm = i64::from(star_arm_px(star_arm(chf, s.ratio)));
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
            let ring = self.ring_at(s.x as f32, s.y as f32);
            let (env, gold) = match t_sec {
                Some(t_sec) => {
                    let e = twinkle_env(t_sec, s.phase);
                    let e = (e + (STAR_STEADY_ENV - e) * settle) * up;
                    let e = (e + (1.0 - STAR_STEADY_ENV) * ring).min(1.0);
                    (e, s.gold || (i % 2 == 0 && twinkle_peak(e)) || ring >= 0.5)
                }
                None => (
                    STAR_STEADY_ENV + (1.0 - STAR_STEADY_ENV) * ring,
                    s.gold || ring >= 0.5,
                ),
            };
            let light_rgb = if i % 2 == 0 { even_rgb } else { odd_rgb };
            lay(StarCall {
                clip: region,
                centre: (s.x, s.y),
                arm,
                cov: STAR_COV * env,
                gold,
                color: self.mark_ink(i, light_rgb),
            });
        }
    }

    /// The sparkles the kitty sheds on its ride: the `k`th where its back
    /// was when it had come `(k + 1) / (SHED_SPARKLES + 1)` of the way, at a
    /// height in its body band the seed picks, fading over [`SHED_MS`] as its
    /// arm draws in. Each sits whole in the star region; none outlives the
    /// ride (the last is shed at 0.54 s and out by 0.99 s).
    fn shed(&self, lay: &mut impl FnMut(StarCall)) {
        let (Some(t), Some(seat)) = (self.clock.ride_ms, self.seat) else {
            return;
        };
        if t >= RIDE_MS {
            return;
        }
        let (Some((from, dist)), Some(region)) = (
            ride_span(&self.geo, self.scene, &seat),
            star_region(&self.geo, self.scene),
        ) else {
            return;
        };
        let dark = self.scene.dark;
        // On a light ground the trail's ink at every step of the wave, so far
        // laid: a sparkle shed on it lies wholly there, one white glint on the
        // print (each pixel its own tone), never across the band's edge in two tones.
        let band = if dark {
            None
        } else {
            self.trail_lay().map(|t| {
                Rect {
                    x0: t.tx0,
                    y0: t.ya + t.amp,
                    x1: t.drawn,
                    y1: t.yb - t.amp,
                }
                .meet(region)
            })
        };
        let (y0, y1) = seat.body();
        let span_y = (y1 - y0) as f32;
        let chf = self.geo.ch as f32;
        let tax = if dark { 1.0 } else { STAR_ARM_INK };
        let tf = t as f32;
        let dist_f = dist as f32;
        for k in 0..SHED_SPARKLES {
            let f = (k + 1) as f32 / (SHED_SPARKLES + 1) as f32;
            let age = tf - ride_at(f);
            if !(0.0..SHED_MS as f32).contains(&age) {
                continue;
            }
            let tau = age / SHED_MS as f32;
            let ratio = STAR_ARM_FINE * (1.0 - 0.4 * tau) * tax;
            let arm = i64::from(star_arm_px(star_arm(chf, ratio)));
            let m = arm + 1;
            // A star's reach behind the kitty's back as it was then.
            let x = from + (dist_f * f).round() as i64 - m;
            let y = y0 + (span_y * (0.15 + 0.7 * unit(self.seed, 0x5348_4544 ^ k as u64))) as i64;
            let (lo, hi) = (
                (region.x0 + m, region.y0 + m),
                (region.x1 - m, region.y1 - m),
            );
            if hi.0 <= lo.0 || hi.1 <= lo.1 {
                return;
            }
            let mut centre = (x.clamp(lo.0, hi.0 - 1), y.clamp(lo.1, hi.1 - 1));
            // Wholly on the trail's ink, where the band has room.
            if let Some(b) = band.filter(|b| b.x1 - b.x0 > 2 * m && b.y1 - b.y0 > 2 * m) {
                centre = (
                    centre.0.clamp(b.x0 + m, b.x1 - m - 1),
                    centre.1.clamp(b.y0 + m, b.y1 - m - 1),
                );
            }
            lay(StarCall {
                clip: region,
                centre,
                arm,
                cov: SHED_COV * (1.0 - tau).powf(1.5),
                gold: false,
                color: self.mark_ink(k, self.scene.palette.mark(k)),
            });
        }
    }

    /// The burst: [`BURST_STARS`] stars on a ring thrown out from the kitty
    /// (or the trail's end, with no kitty) over [`BURST_MS`], shrinking and
    /// fading as they fly ([`Self::burst_ring`]). Only during an episode's
    /// first [`BURST_MS`]. On a light ground a star that crosses the trail's
    /// edge, or reaches past its end onto the kitty, is whole in two tones
    /// (each pixel its own tone).
    fn burst_stars(&self, lay: &mut impl FnMut(StarCall)) {
        let Some(ring) = self.burst_ring() else {
            return;
        };
        for (k, &centre) in ring.centres.iter().enumerate() {
            lay(StarCall {
                clip: ring.region,
                centre,
                arm: ring.arm,
                cov: ring.cov,
                gold: true,
                color: self.mark_ink(k, self.scene.palette.mark(k)),
            });
        }
    }

    /// Every star this frame lays, in the order it lays them: the burst's
    /// ring, the rainbow's glints, the steady field, the ride's sparkles.
    fn each_star(&self, lay: &mut impl FnMut(StarCall)) {
        self.burst_stars(lay);
        self.glints(lay);
        self.field(lay);
        self.shed(lay);
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

/// One star a frame lays ([`Emit::each_star`]): the region it is clipped
/// to, its centre, arm and coverage, whether it glints, and its colour —
/// added light on dark, its print ink on light.
#[derive(Clone, Copy, Debug)]
struct StarCall {
    clip: Rect,
    centre: (i64, i64),
    arm: i64,
    cov: f32,
    gold: bool,
    color: u32,
}

/// The rim's strips (§10.6) round `frame` at kitty pixel `p`, `rim` wide,
/// relative to the art's top-left, in the order they are laid: a
/// [`rim_px`]-wide outline just outside the kitty's silhouette
/// ([`rim_solid`]: the art, its toes' one-pixel gaps closed), along every
/// pixel edge with the silhouette on one side and nothing on the other.
/// Horizontal strips run along the art's rows and reach a rim's width past
/// an outer corner to close it; vertical strips run down its columns and
/// stop a rim's width short where a horizontal strip turns an inner corner —
/// so no two strips overlap (a rim is at most half a kitty pixel, so none
/// reaches another across one transparent pixel) and no light is doubled.
/// One picture for each frame of the kitty and pixel size ([`CardArts`]),
/// each strip painted in the border's spectrum at its midpoint
/// ([`Emit::rim_paint`]), so a black or grey coat reads on a dark ground.
fn rim_strips(frame: &[&str], p: i64, rim: i64, mut strip: impl FnMut(Rect)) {
    let (h, w) = (frame.len() as i64, KITTY_W as i64);
    let solid = |r: i64, c: i64| rim_solid(frame, r, c);
    // Horizontal strips at row boundary `b`: over the art (art at row `b`,
    // nothing `solid` at `b - 1`), then under it (art at `b - 1`, nothing at
    // `b`) — over and under art itself, never a toe's gap.
    for b in 0..=h {
        for (art_row, open_row, y0) in [(b, b - 1, b * p - rim), (b - 1, b, b * p)] {
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
                strip(Rect {
                    x0: c0 * p - if outer(c0 - 1) { rim } else { 0 },
                    y0,
                    x1: c * p + if outer(c) { rim } else { 0 },
                    y1: y0 + rim,
                });
            }
        }
    }
    // Vertical strips at column boundary `v`: left of the art (`solid` at
    // column `v`, not at `v - 1`), then right of it.
    for v in 0..=w {
        for (art_col, open_col, x0) in [(v, v - 1, v * p - rim), (v - 1, v, v * p)] {
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
                    y0: top * p + if solid(top - 1, open_col) { rim } else { 0 },
                    x1: x0 + rim,
                    y1: r * p - if solid(r, open_col) { rim } else { 0 },
                };
                if q.y1 > q.y0 {
                    strip(q);
                }
            }
        }
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
    #[inline]
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
    #[inline]
    fn covers(&self) -> &[Rect] {
        &self.covers[..self.n]
    }
}

#[cfg(test)]
mod tests {
    use super::oracle::{QuadSink, Sink, kitty_quads, lay_star, rim_quads};
    use super::*;
    use crate::cursor_glow::Geom;
    use crate::effect_util::{STAR_GLINT, push_twinkle_star, star_core_px, star_waist_px};
    use aterm_render::GlowQuad;

    /// A launch card at a 9 × 18 cell: text to column 31, the trail to the
    /// kitty at column 40, the box 60 columns wide, four boxed rows (the card's
    /// three and the line's), in a pane whose grid starts below a tab strip.
    pub(super) fn scene() -> CardScene {
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
            wand: Some((10, 3)),
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

    /// [`scene`] in a box wider than [`MAX_EDGE_SEGMENTS`] cells round a
    /// trail longer than [`MAX_TRAIL_SEGMENTS`] of its two-cell segments
    /// (columns 8 to 100): longer than the host lays one (`claude_card`'s
    /// `TRAIL_MAX`), but a scene any caller may hand the library, where both
    /// segment caps bind.
    pub(super) fn widest() -> CardScene {
        CardScene {
            cols: 120,
            trail: Some((8, 100)),
            kitty_col: Some(100),
            ..scene()
        }
    }

    /// [`scene`]'s pane, cut short of the box: the right border's column and
    /// the left padding fall outside it, and the line's row is cut through
    /// its middle. The kitty (columns 40 to 46) still fits.
    pub(super) fn clipped() -> CardScene {
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

    /// The scenes every law is checked on: both themes and all three palettes
    /// (Off's moonbow aired on light too), odd cell
    /// sizes, a grid origin inside the padding, no padding, a negative origin
    /// (the box clipped at the stream's left edge), a card with no line row,
    /// a cell height whose kitty would reach into the top border sized from
    /// the whole band (35 px: `35 % 7 == 0` under a 2 px stroke), a box
    /// larger than its pane's clip, and the box's air — over and under it on
    /// both themes, over a three-row box only, and at the pane's first row,
    /// where the air over it is ignored — each at BOTH HEIGHTS (§10.6): the
    /// two-row kitty ([`scenes_two_rows`]), then the same scenes with the
    /// tall kitty.
    pub(super) fn scenes() -> Vec<CardScene> {
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
            palette: CardPalette::Off,
            ..base
        });
        v.push(CardScene {
            palette: CardPalette::Off,
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
            wand: Some((10, 0)),
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
        v.push(aired(CardScene {
            dark: false,
            palette: CardPalette::Off,
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
    pub(super) fn aired(s: CardScene) -> CardScene {
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

    /// The settled frame for `s` with the test seed: an armed card held
    /// (`animate == false`) past its intro.
    fn settled(s: &CardScene) -> (Vec<GlowQuad>, Vec<GlowQuad>, u64) {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0 + ms(EPISODE_MS + 1), s, false)
    }

    /// One frame's marks, each drawn alone into its own stream: what the
    /// quad oracle ([`oracle`]) lays, mark by mark.
    #[derive(Debug, Default)]
    struct Parts {
        border: Vec<GlowQuad>,
        trail: Vec<GlowQuad>,
        kitty: Vec<GlowQuad>,
        rim: Vec<GlowQuad>,
        stars: Vec<GlowQuad>,
        sparkles: Vec<GlowQuad>,
        burst: Vec<GlowQuad>,
        glints: Vec<GlowQuad>,
    }

    /// [`Parts`] of the frame of `s` at `clock`, with the test seed.
    fn parts(s: &CardScene, clock: Clock) -> Parts {
        parts_seeded(s, clock, 7)
    }

    /// Lay `cx`'s star calls as the oracle does into `out`.
    fn lay_calls(cx: &Emit<'_>, out: &mut Vec<GlowQuad>) -> impl FnMut(StarCall) {
        let sink = Sink { geo: cx.geo };
        let trail = cx.trail_ink();
        let dark = cx.scene.dark;
        move |st: StarCall| {
            lay_star(
                out,
                &sink,
                st.clip,
                st.centre,
                st.arm,
                st.cov,
                st.gold,
                st.color,
                trail.covers(),
                dark,
            );
        }
    }

    /// [`Parts`] of the frame of `s` at `clock` for `seed`.
    fn parts_seeded(s: &CardScene, clock: Clock, seed: u64) -> Parts {
        let geo = Geo::new(s).expect("a scene");
        let cx = Emit::new(s, geo, clock, seed);
        let mut out = Parts::default();
        let sink = Sink { geo };
        cx.border(&mut QuadSink {
            sink,
            out: &mut out.border,
        });
        cx.trail(&mut QuadSink {
            sink,
            out: &mut out.trail,
        });
        kitty_quads(&cx, &sink, &mut out.kitty);
        rim_quads(&cx, &sink, &mut out.rim);
        cx.field(&mut lay_calls(&cx, &mut out.stars));
        cx.shed(&mut lay_calls(&cx, &mut out.sparkles));
        cx.burst_stars(&mut lay_calls(&cx, &mut out.burst));
        cx.glints(&mut lay_calls(&cx, &mut out.glints));
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
    /// touching only the eye rows (and shutting every eye: no iris, pupil or
    /// catch-light left), the flick only the tail's columns, the open cat's
    /// two eyes each an iris and a pupil, and the body band's rows, in every
    /// frame, starting at or left of the art's rear and solid from there to
    /// it, where the trail ends under the cat; the tall cat is the two-row
    /// cat stood taller — the same ears' tips, the eyes, pupils, ears' pink
    /// and nose on the same columns, and the same legs a row longer and
    /// paws. The const shape checks hold this at build time; this test reads
    /// them back, so a check weakened to pass reads wrong here. NEGATIVE
    /// CONTROL: the shape checks refuse a blink that moves a pixel off the
    /// eye rows, a flick that moves one right of the tail, an eye of iris
    /// alone, one eye, two eyes run together, a ragged row, a hole in the
    /// haunch the trail would show through, and a rear read off the open
    /// frame alone (the flick's back, its tail swept down, starts a pixel
    /// further in: ground between).
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
            assert!(
                two_eyes_of_iris_and_pupil(art.open, art.eyes),
                "the cat has two eyes, each an iris and a pupil"
            );
            // NEGATIVE CONTROL: a right eye of iris alone, one eye, and the
            // two eyes run together are refused.
            for (what, pupil_row, catch_row) in [
                ("a right eye without its pupil", "ekccccee", "ewccccew"),
                ("one eye", "ekcccccc", "ewcccccc"),
                ("the eyes run together", "ekkkkkek", "ewkkkkew"),
            ] {
                let rows: Vec<String> = art
                    .open
                    .iter()
                    .map(|r| {
                        r.replace("ekccccek", pupil_row)
                            .replace("ewccccew", catch_row)
                    })
                    .collect();
                let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
                assert_ne!(rows, art.open, "CONTROL: {what} is drawn");
                assert!(
                    !two_eyes_of_iris_and_pupil(&rows, art.eyes),
                    "CONTROL: {what} passes the eye check"
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
        // features — the ears' tips, the eyes and pupils, the ears' pink and
        // the nose on the same columns — and the legs and paws are the
        // two-row cat's, a row longer.
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
        let rear = KITTY_TALL.rear();
        assert!(trail_meets_flush(KITTY_TALL.open, KITTY_TALL.body, rear));
        let mut holed: Vec<&str> = KITTY_TALL.open.to_vec();
        holed[KITTY_TALL.body.0 + 1] = "o.occcccccccocewccccewco";
        assert!(!trail_meets_flush(&holed, KITTY_TALL.body, rear));
        // The open frame's own rear is a pixel short of the flick's back.
        let open_rear = |art: KittyArt| {
            art.open[art.body.0..art.body.0 + art.body.1]
                .iter()
                .map(|r| r.find(|c| c != '.').expect("a body row"))
                .max()
                .expect("body rows")
        };
        for art in [KITTY_SHORT, KITTY_TALL] {
            assert!(trail_meets_flush(art.open, art.body, open_rear(art)));
            assert!(
                !trail_meets_flush(art.flick, art.body, open_rear(art)),
                "CONTROL: the flick's back past the open frame's rear"
            );
        }
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

    /// A card armed at `t0` whose intro played on `s` (its first frame and
    /// its last): the next tick that may animate flows.
    fn played(s: &CardScene, t0: Instant) -> HarnessCard {
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, s, true);
        frame(&mut card, t0 + ms(EPISODE_MS - 5), s, true);
        card
    }

    /// Idle to zero once held: a card whose gate closed (the window
    /// unfocused, Reduce Motion, the load latch) after its intro and while
    /// it flowed draws the settled frame on its very next tick and on every
    /// tick after — the same bytes and the same fingerprint — and wants no
    /// frames; an unarmed card, animating or not, is constant and idles.
    #[test]
    fn a_held_card_is_static_and_wants_no_frames() {
        for s in scenes() {
            let t0 = Instant::now();
            let rest = settled(&s);
            assert_ne!(rest.2, 0, "a settled card still draws");
            let mut card = played(&s, t0);
            frame(&mut card, t0 + ms(EPISODE_MS + 500), &s, true);
            assert!(!card.at_rest(), "CONTROL: it flows");
            for k in 1..6 {
                let now = t0 + ms(EPISODE_MS + 500 + k * 977);
                assert_eq!(frame(&mut card, now, &s, false), rest, "held {k}");
                assert!(card.at_rest(), "held {k}: at rest");
                assert_eq!(card.deadline(now), None, "a held card wants no frames");
            }
            let mut unarmed = HarnessCard::default();
            let first = frame(&mut unarmed, t0, &s, true);
            for k in 1..4 {
                let now = t0 + ms(k * 977);
                assert_eq!(frame(&mut unarmed, now, &s, true), first, "unarmed {k}");
                assert_eq!(unarmed.deadline(now), None, "an unarmed card idles");
            }
        }
        assert_eq!(
            HarnessCard::default().deadline(Instant::now()),
            None,
            "an unarmed card idles"
        );
    }

    /// THE FLOW (owner, 2026-10-19: "Make the rainbow at the banner continue
    /// to shimmer and flow continuously"): past its intro a card that may
    /// animate draws a new frame at every step of the ripple's grid — new
    /// quads, a new fingerprint — and the same frame at every instant
    /// inside a step; its deadline is the next step's start plus the slack,
    /// one fixed instant however often it is asked; and its grid is the
    /// ripple's, counted from the first arm. Over two whole ripple cycles,
    /// on every scene.
    #[test]
    fn after_the_intro_the_card_flows_on_the_step_grid() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = played(&s, t0);
            let end = t0 + ms(EPISODE_MS);
            assert_eq!(
                card.deadline(end),
                Some(end),
                "the flow's first frame is owed"
            );
            let first = EPISODE_MS / FLOW_STEP_MS + 1;
            let mut prev: Option<(Vec<GlowQuad>, Vec<GlowQuad>, u64)> = None;
            for k in first..first + 2 * GLINT_PERIOD_MS / FLOW_STEP_MS {
                let at = t0 + ms(k * FLOW_STEP_MS + 3);
                let f = frame(&mut card, at, &s, true);
                assert!(card.flowing(at) && !card.intro_live(), "step {k}: flowing");
                assert_eq!(card.flow_step(at), Some(k), "step {k}: the grid");
                let due = t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
                assert_eq!(card.deadline(at), Some(due), "step {k}: the next step");
                assert_eq!(card.deadline(at + ms(90)), Some(due), "step {k}: fixed");
                let again = frame(&mut card, at + ms(FLOW_STEP_MS - 10), &s, true);
                assert_eq!(again, f, "step {k}: a step's frame is the step's alone");
                if let Some(p) = &prev {
                    assert_ne!(p.2, f.2, "step {k}: the fingerprint held");
                    assert!(p.0 != f.0 || p.1 != f.1, "step {k}: the quads held");
                }
                prev = Some(f);
            }
        }
    }

    /// A GATE CLOSING SETTLES THE FLOW AT ONCE, AND IT RESUMES FROM THE
    /// SETTLED COLOURS: the first held tick is the settled frame, bit for
    /// bit, with no deadline; the first tick after the gate reopens flows
    /// again — the border, the kitty, its rim and the stars exactly the
    /// settled frame's (the hue starts from the settled phase, the glints
    /// from nothing: no jump) — and the glints come up over
    /// [`GLINT_FADE_MS`], the hue travelling on from there. On every scene.
    #[test]
    fn the_flow_settles_when_a_gate_closes_and_resumes_from_the_settled_colours() {
        for s in scenes() {
            let t0 = Instant::now();
            let rest = settled(&s);
            let border = parts(&s, Clock::SETTLED).border.len();
            let mut card = played(&s, t0);
            frame(&mut card, t0 + ms(EPISODE_MS + 200), &s, true);
            let shut = t0 + ms(EPISODE_MS + 300);
            assert_eq!(frame(&mut card, shut, &s, false), rest, "settled at once");
            assert_eq!(card.deadline(shut), None, "held: no wake");
            // Reopened on a step where the kitty does not blink.
            let back = t0 + ms(EPISODE_MS + 5300);
            let (under, over, fp) = frame(&mut card, back, &s, true);
            assert!(card.flowing(back), "it flows again");
            assert_ne!(fp, rest.2, "a flow frame");
            assert_eq!(
                under[..border],
                rest.0[..border],
                "the border resumes settled"
            );
            assert_eq!(
                over, rest.1,
                "the kitty, its rim and the stars resume settled"
            );
            // The glints are up a fade later, and the hue has moved on.
            let up = back + ms(GLINT_FADE_MS + 2 * FLOW_STEP_MS);
            let (later_under, _, _) = frame(&mut card, up, &s, true);
            assert_ne!(
                later_under[..border],
                rest.0[..border],
                "the border flows on"
            );
        }
    }

    /// A SECOND RIPPLE ON THE FIRST'S STEPS (the cross-track review of
    /// 2026-10-19): a card that adopts another's clock before its first arm
    /// flows on that clock's steps — its deadlines are the other's, step for
    /// step — and its arm keeps it. A started clock is not moved by an
    /// adoption. CONTROL: armed alone 50 ms later, its steps fall 50 ms off.
    #[test]
    fn an_adopted_ripple_flows_on_the_first_cards_steps() {
        let s = scene();
        let t0 = Instant::now();
        let mut first = HarnessCard::default();
        first.arm(t0, 7);
        let origin = first.ripple_origin().expect("armed");
        assert_eq!(origin, t0);
        let t1 = t0 + ms(50);
        let mut second = HarnessCard::default();
        second.adopt_ripple(origin);
        second.arm(t1, 9);
        assert_eq!(second.ripple_origin(), Some(origin), "the arm keeps it");
        second.adopt_ripple(t1);
        assert_eq!(
            second.ripple_origin(),
            Some(origin),
            "a started clock stays"
        );
        let mut alone = HarnessCard::default();
        alone.arm(t1, 9);
        let flow = t1 + ms(EPISODE_MS + 1000);
        for card in [&mut first, &mut second, &mut alone] {
            frame(card, t1, &s, true);
            frame(card, flow, &s, true);
            assert!(card.flowing(flow), "CONTROL: it flows");
        }
        let (a, b, c) = (
            first.deadline(flow).expect("a step"),
            second.deadline(flow).expect("a step"),
            alone.deadline(flow).expect("a step"),
        );
        assert_eq!(a, b, "one step for both");
        assert_eq!(first.flow_step(flow), second.flow_step(flow));
        let off = c.max(a).duration_since(c.min(a)).as_millis() % u128::from(FLOW_STEP_MS);
        assert_eq!(
            off.min(u128::from(FLOW_STEP_MS) - off),
            50,
            "CONTROL: 50 ms off"
        );
    }

    /// THE PHASE LOCK (`HarnessCard::lock_glint`), its ring's birth put
    /// [`FLARE_STEP_LEAD_MS`] before the flare was seen. While no glint shows (the
    /// intro before its glints rise) the grid takes the error whole, unseen.
    /// Once they show it slews and never fades, by a quarter of the error
    /// (+40 ms by +10, −30 by −7, and 3040 ms past a birth, which is 32 ms
    /// before the next, by −8) and at most one step: a second off moves it
    /// one step, either way. An error under four ms moves nothing, so a kept
    /// frame stands. A moved grid draws a kept flow frame anew. A card never
    /// armed takes the flare's start for its first arm.
    #[test]
    fn a_lock_is_whole_while_unseen_and_slews_a_step_at_most_once_seen() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        // In the intro, its glints not up: taken whole.
        let seen = t0 + ms(1000);
        assert!(!card.glints_shown(seen), "CONTROL: no glint shows yet");
        card.lock_glint(seen);
        let g0 = t0 + ms(1000 - FLARE_STEP_LEAD_MS);
        assert_eq!(card.glint0, Some(g0), "adopted, unseen");
        frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
        let mut kept = CardFrames::default();
        let at = t0 + ms(EPISODE_MS + 200);
        let _ = card.draw(at, &s, true, &mut kept);
        assert!(card.glints_shown(at), "CONTROL: the flow's glints show");
        assert!(
            card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "CONTROL: kept"
        );
        let flare = |birth_after_g0: u64, off: i64| {
            let birth = later(g0, birth_after_g0);
            let start = if off >= 0 {
                later(birth, off.unsigned_abs())
            } else {
                birth - ms(off.unsigned_abs())
            };
            later(start, FLARE_STEP_LEAD_MS)
        };
        let grid = card.grid_gen;
        // An error under four ms: nothing moves.
        card.lock_glint(flare(3 * GLINT_PERIOD_MS, 3));
        assert_eq!(
            (card.glint0, card.grid_gen),
            (Some(g0), grid),
            "a 3 ms error is noise"
        );
        assert!(
            card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "still kept"
        );
        // 40 ms after the third ring's birth: a quarter of it.
        card.lock_glint(flare(3 * GLINT_PERIOD_MS, 40));
        assert_eq!(card.glint0, Some(g0 + ms(10)), "slewed by +10");
        assert_eq!(card.fade0, None, "the glints stay up");
        assert!(
            !card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "a frame of the old grid is drawn anew"
        );
        // 30 ms before the tenth's (of the moved grid): −7.
        let g1 = g0 + ms(10);
        card.lock_glint(later(
            later(g1, 10 * GLINT_PERIOD_MS) - ms(30),
            FLARE_STEP_LEAD_MS,
        ));
        assert_eq!(card.glint0, Some(g1 - ms(7)), "slewed by −7");
        // 3040 ms after the eleventh's: 32 ms before the twelfth's, −8.
        let g2 = g1 - ms(7);
        card.lock_glint(later(g2, 11 * GLINT_PERIOD_MS + 3040 + FLARE_STEP_LEAD_MS));
        let g3 = g2 - ms(8);
        assert_eq!(card.glint0, Some(g3), "folded, slewed by −8");
        // A second off, either way: one step a flare.
        card.lock_glint(later(g3, 13 * GLINT_PERIOD_MS + 1000 + FLARE_STEP_LEAD_MS));
        let g4 = later(g3, FLOW_STEP_MS);
        assert_eq!(card.glint0, Some(g4), "a step toward it");
        card.lock_glint(later(
            later(g4, 15 * GLINT_PERIOD_MS) - ms(1000),
            FLARE_STEP_LEAD_MS,
        ));
        assert_eq!(card.glint0, Some(g4 - ms(FLOW_STEP_MS)), "a step back");
        assert_eq!(card.fade0, None, "no lock fades the glints");
        let mut fresh = HarnessCard::default();
        let first = t0 + ms(20 * GLINT_PERIOD_MS);
        fresh.lock_glint(first);
        fresh.arm(first + ms(500), 7);
        assert_eq!(
            fresh.glint0,
            Some(first - ms(FLARE_STEP_LEAD_MS)),
            "the first arm keeps the flare's grid"
        );
    }

    /// THE LOCK AVERAGES CLAUDE'S REPAINT JITTER OUT (lab M2: the wand
    /// repaints every 134 ms at the median, p10 118, p90 142, unaligned with
    /// its 3072 ms cycle, so its first flaring repaint lands up to a repaint
    /// after the cycle's start). The simulated Claude runs 500 ms off the
    /// card's first arm. Its first two flares come inside the intro, before
    /// the glints rise, and are taken whole, the card's ring born
    /// [`FLARE_STEP_LEAD_MS`] before each was seen. Then 300 more come, each
    /// seen 0 to [`crate::ripple::CLAUDE_REPAINT_P90_MS`] late, one in eight
    /// late by a 250 ms stall (more than the measured p90 allows). No lock
    /// moves the grid by more than 40 ms (a stall's by more than 64, half a
    /// step), none fades, and after the first ten flares the card's ring
    /// births stay within half a cell of the ring's travel (71 ms at seven
    /// cells a second) of Claude's flares as seen, on average, less the
    /// lead. NEGATIVE
    /// CONTROL: the rule this one replaced takes an error of at most 64 ms
    /// whole and adopts a larger one with a fade. On the same flares it
    /// chases the jitter (moves of more than 40 ms) and re-fades again and
    /// again.
    #[test]
    fn the_lock_averages_claudes_repaint_jitter_out() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let claude0 = t0 + ms(500);
        let late = |k: u64| {
            if k % 8 == 5 {
                250
            } else {
                (k * 53) % (crate::ripple::CLAUDE_REPAINT_P90_MS + 1)
            }
        };
        let seen = |k: u64| later(claude0, k * GLINT_PERIOD_MS + late(k));
        // The card's ring births against Claude's, folded: ms.
        let p = GLINT_PERIOD_MS as i64;
        let fold = |e: i64| {
            let e = e.rem_euclid(p);
            if e > p / 2 { e - p } else { e }
        };
        let offset = |card: &HarnessCard| {
            let g0 = card.glint0.expect("a grid");
            fold(
                i64::try_from(ms_between(claude0, g0)).unwrap()
                    - i64::try_from(ms_between(g0, claude0)).unwrap(),
            )
        };
        let lead = i64::try_from(FLARE_STEP_LEAD_MS).unwrap();
        for k in 0..2 {
            assert!(
                !card.glints_shown(seen(k)),
                "flare {k}: in the intro, unseen"
            );
            card.lock_glint(seen(k));
            assert_eq!(
                offset(&card),
                i64::try_from(late(k)).unwrap() - lead,
                "flare {k}: taken whole"
            );
        }
        frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
        frame(&mut card, t0 + ms(EPISODE_MS + 200), &s, true);
        let (mut worst, mut max_move) = (0i64, 0i64);
        let mean = (2..302)
            .map(|k| i64::try_from(late(k)).unwrap())
            .sum::<i64>()
            / 300;
        for k in 2..302 {
            let before = offset(&card);
            assert!(card.glints_shown(seen(k)), "flare {k}: the glints show");
            card.lock_glint(seen(k));
            let moved = (offset(&card) - before).abs();
            let bound = if late(k) == 250 { 64 } else { 40 };
            assert!(moved <= bound, "flare {k}: moved {moved} ms");
            max_move = max_move.max(moved);
            assert_eq!(card.fade0, None, "flare {k}: no fade");
            if k >= 12 {
                worst = worst.max((offset(&card) - (mean - lead)).abs());
            }
        }
        let half_cell = (500.0 / crate::ripple::GLINT_CELLS_PER_S) as i64;
        assert!(worst <= half_cell, "locked within {worst} ms");
        assert!(max_move > 0, "CONTROL: the lock does move the grid");
        // NEGATIVE CONTROL: the replaced rule on the same flares.
        let (mut off, mut refades, mut chased) = (0i64, 0, 0i64);
        for k in 2..302 {
            let e = fold(i64::try_from(late(k)).unwrap() - off);
            if e.abs() <= 64 {
                chased = chased.max(e.abs());
                off += e;
            } else {
                refades += 1;
                off = i64::try_from(late(k)).unwrap();
            }
        }
        assert!(
            refades > 30 && chased > 40,
            "CONTROL: the old rule re-fades ({refades}) and chases the jitter ({chased} ms)"
        );
    }

    /// THE STEP LOCK (`HarnessCard::lock_step`): a repaint seen puts a
    /// step's start [`FLARE_STEP_LEAD_MS`] before it, its error folded into
    /// half a step either way. Unseen (the intro before its glints rise),
    /// whole: 50 ms past a step's start moves the grid +50 less the lead.
    /// Seen, a quarter: +40 by +10, and 100 ms past a start — 28 before the
    /// next — by −7; an error under four ms moves nothing and keeps the kept
    /// frame. The step lock never moves the grid a whole step, so the ring's
    /// birth stays on the step the flare's lock chose. CONTROL: a card never
    /// armed has no grid and gains none.
    #[test]
    fn a_step_lock_folds_into_half_a_step() {
        let s = scene();
        let mut fresh = HarnessCard::default();
        fresh.lock_step(Instant::now());
        assert_eq!(fresh.glint0, None, "CONTROL: no grid, none made");
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let g0 = card.glint0.expect("armed");
        let seen = later(g0, 5 * FLOW_STEP_MS + 50 + FLARE_STEP_LEAD_MS);
        assert!(!card.glints_shown(seen), "CONTROL: no glint shows yet");
        card.lock_step(seen);
        let g1 = later(g0, 50);
        assert_eq!(card.glint0, Some(g1), "taken whole, unseen");
        frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
        let mut kept = CardFrames::default();
        let at = t0 + ms(EPISODE_MS + 200);
        let _ = card.draw(at, &s, true, &mut kept);
        assert!(card.glints_shown(at), "CONTROL: the flow's glints show");
        let seen_at = |g: Instant, step: u64, off: u64| {
            later(g, step * FLOW_STEP_MS + off + FLARE_STEP_LEAD_MS)
        };
        card.lock_step(seen_at(g1, 60, 3));
        assert_eq!(card.glint0, Some(g1), "a 3 ms error is noise");
        assert!(
            card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "still kept"
        );
        card.lock_step(seen_at(g1, 61, 40));
        let g2 = later(g1, 10);
        assert_eq!(card.glint0, Some(g2), "slewed by +10");
        card.lock_step(seen_at(g2, 62, 100));
        assert_eq!(card.glint0, Some(g2 - ms(7)), "folded, slewed by −7");
    }

    /// CLAUDE'S REPAINTS CARRY THE FLOWING CARD'S STEPS (design §10.6.11):
    /// a simulated Claude repaints its mascot every 127.7 ms (the real
    /// window's MEASURED rate), each repaint 0 to 12 ms late on its beat, on
    /// a 3072 ms cycle 500 ms off the card's first arm, its flare its first
    /// repaint in its cycle's first two frames; a host draws or replays the
    /// card on each repaint's present, locking the ring on the flare's
    /// rising edge and the steps on every other repaint as the GUI does, and
    /// draws on the card's own wake when it comes first. Over 30 s of flow
    /// after 20 s of settling, the card's own wake fires on under a
    /// twentieth of its steps — Claude's presents draw the rest — and on
    /// under a fifth with Claude repainting at 128 to 129 ms. NEGATIVE
    /// CONTROLS: the flare's lock alone follows a 127.7 ms repaint a quarter
    /// at a time, behind it, and the wake fires on more than half the steps;
    /// the lock this replaced — the ring born 83 ms before the flare was
    /// seen — leaves the repaints past the slack, and it fires on nine steps
    /// in ten or more.
    #[test]
    fn claudes_repaints_carry_the_flowing_cards_steps() {
        let s = scene();
        // `period`: Claude's repaint period (µs); `extra`: how much further
        // before the flare the ring is put; `steps_lock`: whether each
        // repaint locks the steps.
        let run = |period: u64, extra: u64, steps_lock: bool| {
            let t0 = Instant::now();
            let us = |n: u64| t0 + Duration::from_micros(n);
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut kept = CardFrames::default();
            let mut draw = |card: &mut HarnessCard, now: Instant| {
                if card.replay_step(&s, true, &kept, now).is_none() {
                    let _ = card.draw(now, &s, true, &mut kept);
                }
            };
            draw(&mut card, t0);
            let claude0 = 500_000u64;
            let repaint = |n: u64| claude0 + n * period + ((n * 37) % 13) * 1000;
            let flaring = |at: u64| (at.saturating_sub(claude0) % 3_072_000) < 2 * 128_000;
            let (mut n, mut lit) = (0u64, false);
            let (mut fires, mut steps) = (0u64, 0u64);
            let settle = 20_000_000u64;
            let end = settle + 30_000_000;
            let mut last_step = None;
            loop {
                let at = repaint(n);
                if at > end {
                    break;
                }
                // The card's own wake, where it comes before the repaint.
                while let Some(wake) = card.deadline(us(at)).filter(|&w| w < us(at)) {
                    let w = u64::try_from(wake.duration_since(t0).as_micros()).unwrap();
                    draw(&mut card, wake);
                    if w >= settle {
                        fires += 1;
                    }
                    if card.deadline(wake) == Some(wake) {
                        break;
                    }
                }
                let flare = flaring(at);
                if flare && !lit {
                    card.lock_glint(us(at) - ms(extra));
                } else if steps_lock {
                    card.lock_step(us(at) - ms(extra));
                }
                lit = flare;
                draw(&mut card, us(at));
                if at >= settle {
                    let step = card.flow_step(us(at));
                    if step != last_step {
                        steps += 1;
                        last_step = step;
                    }
                }
                n += 1;
            }
            (fires, steps)
        };
        let (fires, steps) = run(127_700, 0, true);
        assert!(steps > 200, "CONTROL: 30 s of flow ({steps} steps)");
        assert!(
            fires * 20 < steps,
            "rides Claude's repaints: {fires} wakes in {steps} steps"
        );
        for period in [128_000, 128_600, 129_000] {
            let (fires, steps) = run(period, 0, true);
            assert!(
                fires * 5 < steps,
                "{period} µs: still rides them: {fires} wakes in {steps} steps"
            );
        }
        let (fires, steps) = run(127_700, 0, false);
        assert!(
            fires * 2 > steps,
            "NEGATIVE CONTROL: the flare's lock alone falls behind: {fires} of {steps}"
        );
        let (fires, steps) = run(127_700, 83 - FLARE_STEP_LEAD_MS, false);
        assert!(
            fires * 10 >= steps * 9,
            "NEGATIVE CONTROL: the old lock's wakes: {fires} of {steps} steps"
        );
    }

    /// While an intro runs every frame has a new fingerprint and a deadline
    /// one frame out, and the quads themselves move (for the first five
    /// seconds at every frame; the last second eases into the flow); at its
    /// end the flow's first frame is owed at once, then the step grid.
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
            let end = t0 + ms(EPISODE_MS);
            assert_eq!(
                card.deadline(end),
                Some(end),
                "the flow's first frame is owed"
            );
            frame(&mut card, end, &s, true);
            let step = EPISODE_MS / FLOW_STEP_MS;
            assert_eq!(
                card.deadline(end),
                Some(t0 + ms((step + 1) * FLOW_STEP_MS + FLOW_SLACK_MS)),
                "then the step grid"
            );
        }
    }

    /// Every quad of every frame lies in exactly one row band, tagged with it,
    /// and inside the box: through the intro and two whole ripple cycles of
    /// the flow after it.
    #[test]
    fn every_quad_lies_in_one_band_inside_the_box() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 3);
            let mut at = 0;
            while at <= EPISODE_MS + 2 * GLINT_PERIOD_MS + 100 {
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
    /// settled, riding, seated and flowing (the glints measured from the
    /// wand's cell, the hue's pace a time: neither reads the border's
    /// length), at both heights and on both grounds. At
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
                Clock::flowing(EPISODE_MS + 500),
                Clock::flowing(EPISODE_MS + 1900),
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

    /// On a light theme every quad is source-over, through the intro and two
    /// whole ripple cycles of the flow, and the card is one printed piece:
    /// the border, the trail, the rim and the kitty are opaque, and a mark
    /// that lets the ground through (a star's taper, a twinkle's dip, a
    /// fading burst or sparkle) is exactly one of the print inks or the
    /// paper's white at its opacity — no washed-out light ink anywhere.
    /// CONTROL: the light-ink law's red, laid as a star was before, is none
    /// of them.
    #[test]
    fn a_light_theme_is_source_over_ink_throughout() {
        for s in scenes().into_iter().filter(|s| !s.dark) {
            let inks = mark_inks(s.palette);
            let printed = |c: u32, a: u8| inks.iter().any(|&k| premul_rgb(k, a) == c);
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 5);
            let flow =
                (EPISODE_MS..=EPISODE_MS + 2 * GLINT_PERIOD_MS).step_by(FLOW_STEP_MS as usize);
            for at in (0..EPISODE_MS).step_by(30).chain(flow) {
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
    /// marks (`marks`, laid in `palette`'s inks) over its trail (`trail`): a
    /// white pixel off the trail's ink, or an ink pixel on it. A mark so
    /// faint that the paper's white and one of the palette's inks come to
    /// the same colour at its opacity (Off's silvers at alpha 1) prints the
    /// same either way and breaches nothing. Empty when it holds.
    fn print_breaches(marks: &[GlowQuad], trail: &[GlowQuad], palette: CardPalette) -> Vec<String> {
        let inks: Vec<u32> = (0..TRAIL_STOPS.len())
            .map(|k| palette.star_ink(k))
            .collect();
        let mut out = Vec::new();
        for q in marks {
            let white = q.color == premul_rgb(WHITE, q.alpha);
            if white && inks.iter().any(|&k| premul_rgb(k, q.alpha) == q.color) {
                continue;
            }
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

    /// ONE PRINTED PIECE (§10.6.10): on a light ground, at both heights, every
    /// palette, through the ride, the burst and the twinkle, and settled,
    /// every pixel of a star, a sparkle or the burst that lies on the
    /// trail's ink is the paper's white (a glint cut out of the print) and
    /// every other is print ink: no ink star ever lies on a stripe, where it
    /// would vanish into its own hue, nor white on the paper. Both kinds are
    /// drawn. The longest trail ([`widest`]) too, where the ink is every one
    /// of [`MAX_TRAIL_SEGMENTS`] segments. NEGATIVE CONTROL: an ink star laid at a trail star's centre
    /// with the region alone for its clip (the old placement) prints ink on
    /// the trail.
    #[test]
    fn light_marks_print_on_the_paper_and_glint_on_the_trail() {
        let (mut glints, mut inked) = (0usize, 0usize);
        let longest = CardScene {
            dark: false,
            ..widest()
        };
        for s in scenes()
            .into_iter()
            .filter(|s| !s.dark && s.trail.is_some())
            .chain([longest])
        {
            for t in (0..=EPISODE_MS).step_by(30).map(Some).chain([None]) {
                for ride_ms in [t, None] {
                    let clock = Clock {
                        t_ms: t,
                        ride_ms,
                        glint: None,
                        flow: None,
                    };
                    let p = parts(&s, clock);
                    let marks: Vec<GlowQuad> = p
                        .stars
                        .iter()
                        .chain(&p.sparkles)
                        .chain(&p.burst)
                        .copied()
                        .collect();
                    let bad = print_breaches(&marks, &p.trail, s.palette);
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
        let sink = Sink { geo };
        lay_star(
            &mut old,
            &sink,
            region,
            centre,
            4,
            STAR_COV,
            false,
            s.palette.star_ink(0),
            &[],
            false,
        );
        assert!(
            !print_breaches(&old, &p.trail, s.palette).is_empty(),
            "CONTROL"
        );
    }

    /// THE LIGHT MARKS LIE ONLY ON CELLS THE HOST READ BLANK: on a light
    /// ground, at both heights, every palette, through the ride, the burst
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
                    let p = parts(
                        &s,
                        Clock {
                            t_ms: t,
                            ride_ms,
                            glint: None,
                            flow: None,
                        },
                    );
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

    /// THE KEPT LINE INKS ARE THE LAW: for each palette (Off's silver
    /// included), a table solved on white gives every step's [`CardPalette::text_at`] there, on white and,
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
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
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
    /// every scene's cell turned light, every palette and height, through
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
                        glint: None,
                        flow: None,
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
            glint: None,
            flow: None,
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
        let sink = Sink { geo };
        let arm = i64::from(star_arm_px(star_arm(
            geo.ch as f32,
            STAR_ARM_FINE * STAR_ARM_INK,
        )));
        lay_star(
            &mut two,
            &sink,
            region,
            centre,
            arm,
            SHED_COV,
            false,
            s.palette.star_ink(0),
            trail.covers(),
            false,
        );
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
            let sink = Sink { geo };
            lay_star(
                &mut out,
                &sink,
                region,
                centre,
                arm,
                cov,
                true,
                ink_rgb,
                &[],
                false,
            );
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
    /// scene with a trail, every palette and height, five seeds, on the
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
                            glint: None,
                            flow: None,
                        };
                        let cx = Emit::new(&s, geo, clock, seed);
                        let Some(ring) = cx.burst_ring() else {
                            continue;
                        };
                        frames += 1;
                        let sink = Sink { geo };
                        let mut burst = Vec::new();
                        cx.burst_stars(&mut lay_calls(&cx, &mut burst));
                        let (mut whole, mut before) = (Vec::new(), Vec::new());
                        for (k, &c) in ring.centres.iter().enumerate() {
                            let rgb = s.palette.star_ink(k);
                            for (out, clip) in [
                                (&mut whole, ring.region),
                                (&mut before, one_sided(&cx, ring.region, c)),
                            ] {
                                lay_star(
                                    out,
                                    &sink,
                                    clip,
                                    c,
                                    ring.arm,
                                    ring.cov,
                                    true,
                                    rgb,
                                    &[],
                                    false,
                                );
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
    /// the brightest and the dimmest light built-in, every palette; at the
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
            for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
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
                        glint: None,
                        flow: None,
                    };
                    let phi = clock.line_drift(LINE_DRIFT);
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
    /// settled, every palette (Off's silver letters as well as Cruise's and
    /// Land's hues), every letter at 4.5:1 or more against its
    /// ground — every light built-in, and a light ground dimmer than any of
    /// them, which the law reads. NEGATIVE CONTROL: the letters solved for
    /// white paper, laid on that dim ground, fall under AA.
    #[test]
    fn the_light_line_reads_on_every_light_ground() {
        let dim = 0x00C8_C8C8;
        assert!(!dark_ground(dim));
        let mut grounds: Vec<u32> = light_builtins().into_iter().map(|(_, g)| g).collect();
        grounds.push(dim);
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
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

    /// An episode held back before any of it was drawn — reduced motion, an
    /// unfocused window, the load-shed latch on as the card is found — is
    /// owed, not lost: the held-back ticks draw the settled frame and want
    /// no frames, which delays the show. The first tick that may animate
    /// plays it from that tick — the burst flies, a deadline is wanted —
    /// even when the held-back stretch outlasted the episode; its intro then
    /// runs a whole episode from there, and the flow takes over. The
    /// held-back ticks put the settled frame on the glass, so the kitty
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
            frame(&mut card, late, &s, true);
            assert!(card.intro_live(), "held {held}: the intro still runs");
            let done = go + ms(EPISODE_MS);
            assert_ne!(frame(&mut card, done, &s, true).2, rest_fp, "held {held}");
            assert!(
                !card.intro_live() && card.flowing(done),
                "held {held}: then the flow"
            );
        }
    }

    /// An episode already drawn animating keeps its clock while motion is
    /// withheld (the settled frame, no deadline) and resumes on that clock
    /// when motion comes back inside it; past its end the card flows from
    /// the settled colours the glass last showed. It is not owed again.
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
        let past = t0 + ms(EPISODE_MS + 100);
        let (_, after, fp) = frame(&mut card, past, &s, true);
        assert!(
            !card.intro_live() && card.flowing(past),
            "past its end it flows"
        );
        assert_ne!(fp, rest_fp, "a flow frame");
        assert_eq!(after, rest, "from the settled colours");
        let step = (EPISODE_MS + 100) / FLOW_STEP_MS;
        assert_eq!(
            card.deadline(past),
            Some(t0 + ms((step + 1) * FLOW_STEP_MS + FLOW_SLACK_MS))
        );
    }

    /// The deadline is not a function of `now` alone: once the intro runs
    /// out after a tick that drew one of its frames, the card asks for
    /// `now`, so the flow's first frame is drawn promptly; the tick that
    /// draws it asks for the next step instead.
    #[test]
    fn the_flows_first_frame_is_asked_for_until_it_is_drawn() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let last = t0 + ms(EPISODE_MS - 10);
        frame(&mut card, last, &s, true);
        assert_eq!(card.deadline(last), Some(last + ms(FRAME_MS)));
        let past = t0 + ms(EPISODE_MS + 250);
        assert_eq!(
            card.deadline(past),
            Some(past),
            "the flow's first frame is owed"
        );
        assert_eq!(card.deadline(past), Some(past), "asked until drawn");
        let (_, _, fp) = frame(&mut card, past, &s, true);
        assert_ne!(fp, settled(&s).2, "a flow frame");
        let k = (EPISODE_MS + 250) / FLOW_STEP_MS;
        let next = t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
        assert_eq!(card.deadline(past), Some(next), "drawn: the next step");
        // A held-back tick draws the settled frame itself: nothing is owed.
        let mut held = HarnessCard::default();
        held.arm(t0, 7);
        frame(&mut held, t0, &s, true);
        frame(&mut held, t0 + ms(100), &s, false);
        assert_eq!(held.deadline(past), None);
    }

    /// A host that stops ticking after the last intro frame (the frame
    /// scrolled back, the card's version no longer the process's, the pane
    /// hidden) but asks for the deadline every loop is asked to wake at
    /// once only until [`SETTLE_GRACE_MS`] past the last frame it drew,
    /// then let idle: never a spin for as long as the card goes unticked.
    /// A re-armed card that is never ticked asks for frames while its
    /// episode would run, then nothing — not the flow of the episode before
    /// it.
    #[test]
    fn an_unticked_card_stops_asking_after_the_grace() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let mut at = 0;
        let mut last = 0;
        while at < EPISODE_MS {
            frame(&mut card, t0 + ms(at), &s, true);
            last = at;
            at += FRAME_MS;
        }
        let end = t0 + ms(EPISODE_MS);
        assert_eq!(
            card.deadline(end),
            Some(end),
            "the flow's first frame is owed"
        );
        // The host asks every millisecond for ten seconds and never ticks.
        let asked: Vec<u64> = (0..10_000)
            .filter(|&k| card.deadline(end + ms(k)).is_some())
            .collect();
        assert_eq!(
            asked,
            (0..last + SETTLE_GRACE_MS - EPISODE_MS).collect::<Vec<_>>(),
            "asked only inside the grace"
        );
        assert_eq!(card.deadline(t0 + ms(last + SETTLE_GRACE_MS)), None);
        // The tick that comes late still draws, and flows.
        let late = end + ms(60_000);
        assert_ne!(frame(&mut card, late, &s, true).2, settled(&s).2);
        assert!(card.flowing(late) && card.deadline(late).is_some_and(|d| d > late));

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

    /// The backstop holds for a FLOWING card too: a host that stops ticking
    /// it (an occluded window, whose splice never runs) but asks every loop
    /// is told the same next step, a fixed instant, until
    /// [`SETTLE_GRACE_MS`] past the last frame it drew, then nothing; the
    /// tick that comes late draws the late step and asks for the one after.
    #[test]
    fn an_unticked_flowing_card_stops_asking_after_the_grace() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = played(&s, t0);
        let drawn = t0 + ms(EPISODE_MS + 300);
        frame(&mut card, drawn, &s, true);
        let step = |at: u64| at / FLOW_STEP_MS;
        let next = |k: u64| t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
        let k = step(EPISODE_MS + 300);
        let asked: Vec<u64> = (0..10_000)
            .filter(|&j| card.deadline(drawn + ms(j)).is_some())
            .collect();
        assert_eq!(asked, (0..SETTLE_GRACE_MS).collect::<Vec<_>>(), "the grace");
        assert!(
            (0..SETTLE_GRACE_MS).all(|j| card.deadline(drawn + ms(j)) == Some(next(k))),
            "one fixed instant"
        );
        let late = drawn + ms(60_000);
        frame(&mut card, late, &s, true);
        assert_eq!(card.deadline(late), Some(next(step(EPISODE_MS + 60_300))));
    }

    /// A tick with nothing to draw — an inverted or empty clip, no columns,
    /// no rows, no cell size: a pane mid-layout — draws nothing and holds
    /// the episode back like `animate == false`: an episode not yet drawn is
    /// owed (no deadline meanwhile) and plays whole from the first tick that
    /// draws, even past the episode's length; a playing one keeps its clock,
    /// and the flow takes over at its end.
    #[test]
    fn nothing_to_draw_holds_the_episode_back() {
        let s = scene();
        let (_, rest, _) = settled(&s);
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
            assert_ne!(frame(&mut card, done, &s, true).2, 0, "{what}");
            assert!(card.flowing(done), "{what}: the flow takes over");
        }
    }

    /// An owed episode's debt lapses [`OWED_MAX_MS`] after the arm: a tick
    /// that may animate before then plays it; one at or after it plays no
    /// intro — the card flows, from the settled colours and with no burst —
    /// so the show never plays minutes later on an unrelated redraw. Held
    /// back by `animate == false` or by nothing to draw alike; a fresh arm
    /// owes a fresh episode.
    #[test]
    fn an_owed_episode_lapses_after_its_dwell() {
        let s = scene();
        let (_, rest_over, rest_fp) = settled(&s);
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
                let (_, over, fp) = frame(&mut card, now, &s, true);
                let what = format!("{held_by}: paid at {at} ms");
                assert_ne!(fp, rest_fp, "{what}: it moves either way");
                assert_eq!(card.intro_live(), plays, "{what}");
                let want = if plays {
                    now + ms(FRAME_MS)
                } else {
                    t0 + ms((at / FLOW_STEP_MS + 1) * FLOW_STEP_MS + FLOW_SLACK_MS)
                };
                assert_eq!(card.deadline(now), Some(want), "{what}");
                if !plays {
                    assert_eq!(over, rest_over, "{what}: no burst, the settled colours");
                    // Lapsed for good: a later tick flows on…
                    let after = now + ms(1000);
                    frame(&mut card, after, &s, true);
                    assert!(!card.intro_live() && card.flowing(after), "{what}");
                    // …until an arm owes a fresh episode.
                    card.arm(after, 7);
                    frame(&mut card, after, &s, true);
                    assert!(card.intro_live(), "{what}: a fresh episode");
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
        let over = [p.kitty, p.rim, p.burst, p.glints, p.stars, p.sparkles].concat();
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
                let clock = Clock::intro(t - re, Some(t));
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
                !parts(&s, Clock::intro(RIDE_MS + 10 - re, Some(RIDE_MS + 10)))
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
                        art(at.art.frame(kitty_frame(clock))),
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
    /// seat, and every star is up a [`STAR_REVEAL_MS`] after the ride; the
    /// rainbow's glints come up behind its back as it passes them, on the
    /// trail laid so far. The sparkles it sheds sit behind its back, in the
    /// star region and their bands, and are gone by the ride's end, before
    /// the burst opens; none when settled or with nothing to ride. NEGATIVE
    /// CONTROL: before the kitty starts, stars the field holds left of the
    /// seat, and the glints, are not drawn, and are once it is seated.
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
                    let laid = trail_reach(&p.trail, &s).map_or(i64::MIN, |r| r.1);
                    for q in &p.glints {
                        let x1 = i64::from(q.x) + i64::from(q.w);
                        assert!(
                            x1 <= at.x0,
                            "{what}: {t} ms, a glint ahead of the kitty {q:?}"
                        );
                        assert!(
                            x1 <= laid,
                            "{what}: {t} ms, a glint past the trail laid {q:?}"
                        );
                    }
                    if t >= RIDE_MS {
                        // Out before the burst opens (`CARD_ITEMS_MAX`' count).
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
                assert!(
                    parts(&s, Clock::riding(0)).glints.is_empty(),
                    "{what}: glints"
                );
                assert!(!up.glints.is_empty(), "{what}: glints up after the ride");
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

    /// [`scenes`], and launch cards on the longest trail (20 cells) at the
    /// shipping cell sizes, on both grounds and at both heights: the scenes
    /// the rainbow's glints are checked on.
    fn glint_scenes() -> Vec<CardScene> {
        let mut v = scenes();
        for (cw, ch) in [(9u16, 18u16), (16, 33), (17, 35)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    v.push(CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        trail: Some((32, 52)),
                        kitty_col: Some(52),
                        cols: 63,
                        ..scene()
                    });
                }
            }
        }
        v
    }

    /// The pixel rectangle of quad `q`.
    fn rect_of(q: &GlowQuad) -> Rect {
        Rect {
            x0: i64::from(q.x),
            y0: i64::from(q.y),
            x1: i64::from(q.x) + i64::from(q.w),
            y1: i64::from(q.y) + i64::from(q.h),
        }
    }

    /// THE RAINBOW'S GLINTS LIE ON ITS STRIPES (§10.6.16): on every scene
    /// with a trail, settled and through a whole episode, riding in and
    /// seated, every glint's quad lies inside the trail's band less the
    /// wave's step (so on the rainbow in every frame, the stripes' edges
    /// stepping round it), right of the trail's start (never over the
    /// card's text) and left of the kitty's art; on a light ground each is
    /// the paper's white on the print, in one tone. One glint per
    /// [`GLINT_CELLS`] cells of the trail's free stretch, three at most and
    /// one at least, and the settled frame draws them all. NEGATIVE CONTROL:
    /// a glint laid on the band's edge leaves it.
    #[test]
    fn the_rainbows_glints_lie_on_its_stripes() {
        let mut counts = std::collections::BTreeSet::new();
        let mut laid = 0;
        for s in glint_scenes() {
            let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
            let g = Geo::new(&s).expect("a scene");
            let seat = kitty_rect(&g, &s);
            let glints = glint_spots(&g, &s, 7);
            let Some((t, amp)) = trail_rect(&g, &s, seat) else {
                assert_eq!(glints.n, 0, "{what}: no trail, no glints");
                continue;
            };
            let free = seat.map_or(t.x1, |at| at.rect.x0.min(t.x1));
            if glints.n > 0 {
                let cells = (free - t.x0) / g.cw;
                let want = usize::try_from(cells / GLINT_CELLS).expect("cells");
                assert_eq!(glints.n, want.clamp(1, GLINTS_MAX), "{what}: the count");
                assert!(glints.rest >= 1 && glints.rest <= glints.flare, "{what}");
            }
            counts.insert(glints.n);
            let band = Rect {
                x0: t.x0.max(g.col(s.trail.expect("a trail").0)),
                y0: t.y0 + amp,
                x1: free,
                y1: t.y1 - amp,
            };
            let clocks = std::iter::once(Clock::SETTLED).chain(
                (0..EPISODE_MS)
                    .step_by(FRAME_MS as usize)
                    .flat_map(|t| [Clock::riding(t), Clock::seated(t)]),
            );
            for clock in clocks {
                let p = parts(&s, clock);
                if clock.t_ms.is_none() {
                    assert_eq!(
                        p.glints.is_empty(),
                        glints.n == 0,
                        "{what}: the settled frame's glints"
                    );
                }
                for q in &p.glints {
                    assert_in_band_and_box(q, &s, &what);
                    let r = rect_of(q);
                    assert_eq!(
                        r.meet(band),
                        r,
                        "{what} at {clock:?}: off the rainbow {q:?}"
                    );
                    if !s.dark {
                        let c = q.color;
                        assert!(
                            q.alpha > 0
                                && (c >> 16) & 0xff == c & 0xff
                                && (c >> 8) & 0xff == c & 0xff,
                            "{what} at {clock:?}: not the paper's white {q:?}"
                        );
                    }
                }
                laid += p.glints.len();
            }
        }
        assert!(laid > 0, "glints laid");
        assert!(
            counts.contains(&1) && counts.contains(&GLINTS_MAX),
            "one on a short trail, the most on a long one: {counts:?}"
        );
        // NEGATIVE CONTROL: a glint centred on the band's top edge pokes
        // off the rainbow.
        let s = scene();
        let g = Geo::new(&s).expect("a scene");
        let (t, amp) = trail_rect(&g, &s, kitty_rect(&g, &s)).expect("a trail");
        let mut out = Vec::new();
        let sink = Sink { geo: g };
        let edge = (t.x0 + 2 * g.cw, t.y0 + amp);
        lay_star(
            &mut out,
            &sink,
            g.clip,
            edge,
            glint_spots(&g, &s, 7).rest,
            GLINT_COV,
            false,
            WHITE,
            &[],
            true,
        );
        let band = Rect {
            y0: t.y0 + amp,
            y1: t.y1 - amp,
            ..t
        };
        assert!(
            out.iter().any(|q| rect_of(q).meet(band) != rect_of(q)),
            "CONTROL: a glint on the band's edge leaves it"
        );
    }

    /// A GLINT FLARES AS THE SHEEN PASSES — Claude's `✦` flaring as its
    /// crest runs out from the wand: in a seated episode each glint, at rest
    /// the standard star, grows to its flare's arm and throws its four
    /// diffraction glints when the trail's shimmer reaches it, and is back
    /// at its rest size between passes; it flares on every pass the episode
    /// holds (the shimmer's, under the photosensitivity bound), and the
    /// intro's last frame hands the flow its glints: each the size, and
    /// throwing its glints or not, as the flow's first step lays it, so the
    /// flow follows without a jump (§10.6.16, composed with the flow: the
    /// ripple's crest flares a glint over the intro's last eighth as it does
    /// in the flow). At the shipping cell sizes, on both grounds and at both
    /// heights. NEGATIVE CONTROL: away from the sheen a glint is its settled
    /// size and throws no glints.
    #[test]
    fn a_glint_flares_as_the_sheen_passes() {
        for s in glint_scenes()
            .into_iter()
            .filter(|s| s.trail == Some((32, 52)))
        {
            let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
            let g = Geo::new(&s).expect("a scene");
            let glints = glint_spots(&g, &s, 7);
            assert_eq!(glints.n, GLINTS_MAX, "{what}");
            assert!(glints.flare > glints.rest, "{what}: room to flare");
            let reach = glints.flare + 1;
            // A glint's half-width in a frame, and whether it throws its
            // diffraction glints (one-pixel quads off its arms' line).
            let read = |p: &Parts, spot: &GlintSpot| {
                let mine: Vec<Rect> = p
                    .glints
                    .iter()
                    .map(rect_of)
                    .filter(|r| (r.x0 - spot.x).abs() <= reach && (r.y0 - spot.y).abs() <= reach)
                    .collect();
                let half = mine
                    .iter()
                    .map(|r| (r.x1 - 1 - spot.x).max(spot.x - r.x0))
                    .max();
                let dots = mine.iter().any(|r| {
                    r.x1 - r.x0 == 1 && r.y1 - r.y0 == 1 && r.x0 != spot.x && r.y0 != spot.y
                });
                (half, dots)
            };
            let settled = parts(&s, Clock::SETTLED);
            for spot in glints.spots() {
                assert_eq!(
                    read(&settled, spot),
                    (Some(glints.rest), false),
                    "{what}: at rest"
                );
                let mut flares = 0;
                let mut was_flaring = false;
                let mut rest_between = false;
                for t in (0..EPISODE_MS).step_by(FRAME_MS as usize / 3) {
                    let (half, dots) = read(&parts(&s, Clock::seated(t)), spot);
                    let half = half.expect("a glint");
                    assert!(
                        (glints.rest..=glints.flare).contains(&half),
                        "{what}: {t} ms, arm {half}"
                    );
                    let flaring = half == glints.flare;
                    if flaring && !was_flaring {
                        flares += 1;
                        assert!(dots, "{what}: {t} ms, a flare throws its glints");
                    }
                    if half == glints.rest && flares > 0 {
                        assert!(!dots, "{what}: CONTROL, {t} ms, at rest, no glints");
                        rest_between = true;
                    }
                    was_flaring = flaring;
                }
                // The shimmer sweeps the trail once a SHIMMER_MS, from past
                // its start to past its end, faded in and out at the
                // episode's ends: every pass inside it reaches each glint.
                let passes = (EPISODE_MS as f32 / SHIMMER_MS) as usize;
                assert!(
                    flares >= passes - 1 && flares <= passes + 1,
                    "{what}: {flares} flares for {passes} passes"
                );
                assert!(rest_between, "{what}: CONTROL, back at rest between passes");
            }
            // The intro's last frame and the flow's first lay each glint
            // alike: neither a size nor the dots pop at the handover.
            let last = parts(&s, Clock::seated(EPISODE_MS - 1));
            let first = parts(&s, Clock::flowing(EPISODE_MS));
            for spot in glints.spots() {
                assert_eq!(
                    read(&last, spot),
                    read(&first, spot),
                    "{what}: no jump into the flow"
                );
            }
        }
    }

    /// A FLOWING GLINT FLARES AS THE RIPPLE PASSES — Claude's `✦` flaring
    /// with its ring (§10.6.16, composed with the flow): through one cycle
    /// of the flow, on its step grid, each glint grows past its rest size
    /// and throws its four diffraction glints at some step, where the
    /// ripple's crest is on it, and is the standard star with no glints at
    /// another; never larger than its flare. Under Off, on dark, a glint is
    /// the moonbow's starlight, not Cruise's gold. At the shipping cell
    /// sizes, on both grounds and at both heights. CONTROL: the held card
    /// (the settled frame) lays every glint at rest, with no glints.
    #[test]
    fn a_flowing_glint_flares_as_the_ripple_passes() {
        let chroma = |rgb: u32| {
            let [_, r, g, b] = rgb.to_be_bytes();
            r.max(g).max(b) - r.min(g).min(b)
        };
        for s in glint_scenes()
            .into_iter()
            .filter(|s| s.trail == Some((32, 52)))
        {
            let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
            let g = Geo::new(&s).expect("a scene");
            let glints = glint_spots(&g, &s, 7);
            assert_eq!(glints.n, GLINTS_MAX, "{what}");
            let reach = glints.flare + 1;
            let read = |p: &Parts, spot: &GlintSpot| {
                let mine: Vec<Rect> = p
                    .glints
                    .iter()
                    .map(rect_of)
                    .filter(|r| (r.x0 - spot.x).abs() <= reach && (r.y0 - spot.y).abs() <= reach)
                    .collect();
                let half = mine
                    .iter()
                    .map(|r| (r.x1 - 1 - spot.x).max(spot.x - r.x0))
                    .max()
                    .expect("a glint");
                let dots = mine.iter().any(|r| {
                    r.x1 - r.x0 == 1 && r.y1 - r.y0 == 1 && r.x0 != spot.x && r.y0 != spot.y
                });
                (half, dots)
            };
            let held = parts(&s, Clock::SETTLED);
            let steps: Vec<Parts> = (0..GLINT_PERIOD_MS / FLOW_STEP_MS)
                .map(|k| parts(&s, Clock::flowing(EPISODE_MS + k * FLOW_STEP_MS)))
                .collect();
            for spot in glints.spots() {
                assert_eq!(
                    read(&held, spot),
                    (glints.rest, false),
                    "{what}: CONTROL, the held card's glint at rest"
                );
                let seen: Vec<(i64, bool)> = steps.iter().map(|p| read(p, spot)).collect();
                assert!(
                    seen.iter()
                        .all(|&(half, _)| (glints.rest..=glints.flare).contains(&half)),
                    "{what}: {seen:?}"
                );
                assert!(
                    seen.iter().any(|&(half, dots)| half > glints.rest && dots),
                    "{what}: the crest flares the glint at ({}, {}): {seen:?}",
                    spot.x,
                    spot.y
                );
                assert!(
                    seen.contains(&(glints.rest, false)),
                    "{what}: and it is back at rest off the crest: {seen:?}"
                );
            }
            if s.dark {
                let off = CardScene {
                    palette: CardPalette::Off,
                    ..s
                };
                let starlight = parts(&off, Clock::SETTLED).glints;
                assert!(
                    !starlight.is_empty() && starlight.iter().all(|q| chroma(q.color) < 0x20),
                    "{what}: Off's glints are starlight"
                );
                assert!(
                    held.glints.iter().any(|q| chroma(q.color) >= 0x20),
                    "{what}: CONTROL, Cruise's are gold"
                );
            }
        }
    }

    /// The field keeps clear of the rainbow's glints: no field star sits
    /// within a star's gap of a glint, on either ground, over many seeds,
    /// where a field star on the trail would otherwise often land on one.
    #[test]
    fn the_field_keeps_clear_of_the_glints() {
        let mut near_trail = 0;
        for seed in 0..200u64 {
            for s in glint_scenes()
                .into_iter()
                .filter(|s| s.cw == 9 && s.trail == Some((32, 52)))
            {
                let g = Geo::new(&s).expect("a scene");
                let glints = glint_spots(&g, &s, seed);
                let (field, n) = field_spots(&g, &s, seed, !s.dark);
                let gap = g.cw.max(g.ch / 2);
                for f in &field[..n] {
                    for o in glints.spots() {
                        assert!(
                            (f.x - o.x).abs() >= gap || (f.y - o.y).abs() >= gap,
                            "seed {seed}: a field star {f:?} on a glint {o:?}"
                        );
                    }
                    let (t, _) = trail_rect(&g, &s, kitty_rect(&g, &s)).expect("a trail");
                    near_trail += usize::from(t.contains(f.x, f.y));
                }
            }
        }
        assert!(
            near_trail > 0,
            "CONTROL: field stars on the trail, beside the glints"
        );
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
            let sink = Sink { geo: g };
            lay_star(
                &mut ours,
                &sink,
                g.clip,
                (sx, sy),
                arm,
                cov,
                gold,
                WHITE,
                &[],
                true,
            );
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

    /// Equal inputs give equal bytes — the intro's frames and the flow's, two
    /// ripple cycles on — and the seed moves the stars.
    #[test]
    fn the_card_is_deterministic() {
        for s in scenes() {
            let t0 = Instant::now();
            let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
            a.arm(t0, 42);
            b.arm(t0, 42);
            let flow = [
                EPISODE_MS + 5,
                EPISODE_MS + 900,
                EPISODE_MS + 3100,
                EPISODE_MS + 6200,
            ];
            for at in [0, 33, 700, 2600].into_iter().chain(flow) {
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

    /// A frame's contents as text: every item and palette entry, the arts
    /// by their content ids, and the origin.
    fn frame_text(f: &CardFrame) -> String {
        format!(
            "under {:?} {:?} over {:?} {:?} at {:?}",
            f.under.items(),
            f.under.palette(),
            f.over.items(),
            f.over.palette(),
            f.origin
        )
    }

    /// A CARD AT REST IS REPLAYED, BIT FOR BIT, AND A FLOWING ONE BETWEEN
    /// ITS STEPS ([`HarnessCard::replay`], [`HarnessCard::replay_step`]):
    /// beside a twin that draws every step, a card driven the host's way
    /// (`replay`, else the clock and `replay_step`, else `draw`) hands back
    /// the same contents at the same origin and shows its host the same card
    /// (its deadlines, its line, at rest and still) after every step of a
    /// life — an intro, the flow after it, still frames, a scene that
    /// changes, serious mode, a window that loses motion and an episode owed
    /// until motion comes back, and an owed debt that lapses into the flow.
    /// A held replay takes no clock; a flow step's second frame takes one.
    /// Both hand back the kept `Arc`s themselves: they seal nothing and copy
    /// nothing. CONTROL: every intro frame, every first frame of a new scene
    /// or a new step and every step out of an owed episode is drawn (and
    /// seals its contents), and the replays happen — on every held step
    /// after the first and every flow frame inside its step after the first.
    #[test]
    fn a_card_at_rest_is_replayed_bit_for_bit() {
        /// How a step reaches the glass.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Way {
            /// Drawn: the clock read and the frame computed.
            Draw,
            /// The held card's settled frame, replayed with no clock read.
            Held,
            /// The flow's frame of this step, replayed after one clock read.
            Step,
        }
        use Way::{Draw, Held, Step};
        for s in scenes() {
            // The next Autopilot's palette: another scene.
            let land = CardScene {
                palette: match s.palette {
                    CardPalette::Cruise => CardPalette::Land,
                    CardPalette::Land => CardPalette::Off,
                    CardPalette::Off => CardPalette::Cruise,
                },
                ..s
            };
            let serious = CardScene { serious: true, ..s };
            let t0 = Instant::now();
            let lapse = 3 * EPISODE_MS + OWED_MAX_MS;
            // (ms after t0, scene, animate, arm, how it reaches the glass)
            let steps: Vec<(u64, CardScene, bool, bool, Way)> = vec![
                (0, s, true, true, Draw),
                (33, s, true, false, Draw),
                // Motion lost mid-episode (focus, Reduce Motion, the load
                // latch): the first held frame is drawn, the rest
                // replayed; motion back resumes the clock.
                (100, s, false, false, Draw),
                (140, s, false, false, Held),
                (180, s, false, false, Held),
                (220, s, true, false, Draw),
                (2000, s, true, false, Draw),
                // The flow: step 46 drawn, then replayed inside it; step 47
                // drawn.
                (EPISODE_MS + 1, s, true, false, Draw),
                (EPISODE_MS + 10, s, true, false, Step),
                (EPISODE_MS + 40, s, true, false, Draw),
                // Held: the settled frame drawn once, then replayed.
                (EPISODE_MS + 90, s, false, false, Draw),
                (EPISODE_MS + 92, s, false, false, Held),
                (EPISODE_MS + 95, land, true, false, Draw),
                (EPISODE_MS + 99, land, true, false, Step),
                (EPISODE_MS + 120, serious, true, false, Draw),
                (EPISODE_MS + 130, serious, true, false, Held),
                (EPISODE_MS + 140, s, true, false, Draw),
                // The window loses motion and the card is armed: owed.
                (EPISODE_MS + 200, s, false, true, Draw),
                (EPISODE_MS + 300, s, false, false, Held),
                (EPISODE_MS + 400, s, false, false, Held),
                // Motion back: the owed episode plays from here.
                (EPISODE_MS + 500, s, true, false, Draw),
                (EPISODE_MS + 533, s, true, false, Draw),
                // Armed unfocused again, held past the debt's lapse: the
                // flow, no intro.
                (3 * EPISODE_MS, s, false, true, Draw),
                (lapse + 10, s, false, false, Held),
                (lapse + 20, s, true, false, Draw),
                (lapse + 30, s, true, false, Step),
                (lapse + 200, s, true, false, Draw),
                (lapse + 210, s, true, false, Step),
            ];
            let (mut twin, mut twin_frames) = (HarnessCard::default(), CardFrames::default());
            let (mut host, mut frames) = (HarnessCard::default(), CardFrames::default());
            let mut last: Option<CardFrame> = None;
            for (at, scene, animate, arm, way) in steps {
                let now = t0 + ms(at);
                if arm {
                    twin.arm(now, 9);
                    host.arm(now, 9);
                }
                let want = twin.draw(now, &scene, animate, &mut twin_frames);
                let before = aterm_core::render::fx_counts_here();
                // The host's way (`claude_card`): `replay`, else the clock
                // and `replay_step`.
                let mut clock_reads = 0;
                let replayed = match host.replay(&scene, animate, &frames) {
                    Some(f) => Some((f, Held)),
                    None => {
                        clock_reads += 1;
                        host.replay_step(&scene, animate, &frames, now)
                            .map(|f| (f, Step))
                    }
                };
                let spent = aterm_core::render::fx_counts_here();
                assert_eq!(
                    (spent.0 - before.0, spent.1 - before.1),
                    (0, 0),
                    "{at} ms: a replay builds nothing and copies nothing"
                );
                let (got, how) = match replayed {
                    Some((f, how)) => {
                        let prev = last.as_ref().expect("a frame before a replay");
                        assert!(
                            Arc::ptr_eq(&f.under, &prev.under) && Arc::ptr_eq(&f.over, &prev.over),
                            "{at} ms: the kept Arcs themselves"
                        );
                        (f, how)
                    }
                    None => {
                        let f = host.draw(now, &scene, animate, &mut frames);
                        let built = aterm_core::render::fx_counts_here().0 - spent.0;
                        assert!(
                            built > 0 || f.is_empty(),
                            "{at} ms: CONTROL, a draw seals its contents"
                        );
                        (f, Draw)
                    }
                };
                assert_eq!(how, way, "{at} ms: how it reached the glass");
                assert_eq!(
                    clock_reads,
                    usize::from(way != Held),
                    "{at} ms: clock reads"
                );
                assert_eq!(frame_text(&got), frame_text(&want), "{at} ms: the frame");
                assert_eq!(observed(&host, now), observed(&twin, now), "{at} ms: host");
                last = Some(got);
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
    /// stripe's, in every frame — settled, mid-episode, flowing (two ripple
    /// cycles), both themes, Land's ramp and Off's. The emitter reuses the last segment's right-end paint instead of
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
            let flow = (EPISODE_MS + 1..EPISODE_MS + 2 * GLINT_PERIOD_MS)
                .step_by(FLOW_STEP_MS as usize * 5);
            for at in [0, 40, 450, 1300, 2600, 4100].into_iter().chain(flow) {
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
    /// does not call at rest is one whose intro is armed or playing, or that
    /// flows. CONTROL: the intro's frames and the flow's are not at rest; a
    /// held tick is.
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
            (EPISODE_MS + 3, false),
            (EPISODE_MS + 900, true),
        ] {
            let now = t0 + ms(at);
            let _ = frame(&mut card, now, &s, animate);
            assert_eq!(card.at_rest(), !animate, "{at} ms");
            if card.at_rest() {
                for dt in [0, 1, FRAME_MS, EPISODE_MS, 10 * EPISODE_MS] {
                    assert_eq!(card.deadline(now + ms(dt)), None, "{at} ms + {dt}");
                }
            } else {
                assert!(card.deadline(now).is_some(), "CONTROL: {at} ms moves");
            }
        }
        frame(&mut card, t0 + ms(EPISODE_MS + 1000), &s, false);
        assert!(card.at_rest(), "held");
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
    /// the episode; the ripple's glints along it in the flow after; the
    /// settled gradient (constant bytes) whenever it is held and under
    /// reduced motion; legible on both grounds — Cruise's spectrum, Land's
    /// sunset and Off's moonbow alike (white letters on dark, silver ink on
    /// light), glints included: on dark over the luminance floor, on light
    /// at WCAG AA against the ground, frame by frame.
    #[test]
    fn the_line_sweeps_then_settles() {
        let t0 = Instant::now();
        let (row, col, len) = (7u16, 11u16, 64u16);
        for (palette, ground) in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off]
            .into_iter()
            .flat_map(|p| [(p, NIGHT), (p, DAY)])
        {
            let dark = ground == NIGHT;
            let mut card = HarnessCard::default();
            card.tick(
                t0,
                &CardScene {
                    palette,
                    dark,
                    ground,
                    ..scene()
                },
                false,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            card.arm(t0, 3);
            let rest = card.line_ink(t0 + ms(EPISODE_MS), row, col, len, ground, false);
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
                card.line_ink(t0 + ms(EPISODE_MS + 5000), row, col, len, ground, false),
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
            // The flow: the glints run along the line, step by step, over a
            // whole ripple cycle.
            let flow: Vec<Vec<InkCell>> = (0..GLINT_PERIOD_MS / FLOW_STEP_MS)
                .map(|k| {
                    let at = t0 + ms(EPISODE_MS + 200 + k * FLOW_STEP_MS);
                    card.line_ink(at, row, col, len, ground, true)
                })
                .collect();
            assert!(
                flow.iter().filter(|f| **f != rest).count() > flow.len() / 2,
                "{palette:?}: the flow's glints run along the line"
            );
            assert_ne!(
                rest[0].color,
                rest[rest.len() - 1].color,
                "the settled line is a gradient"
            );
            for c in rest.iter().chain(&mid).chain(flow.iter().flatten()) {
                let rgb = (u32::from(c.color[0]) << 16)
                    | (u32::from(c.color[1]) << 8)
                    | u32::from(c.color[2]);
                if dark {
                    assert!(
                        relative_luminance(rgb) >= LINE_DARK_LUMA_FLOOR - 0.01,
                        "{palette:?}: {rgb:06x} too dark"
                    );
                } else {
                    assert!(
                        contrast(rgb, ground) >= 4.5,
                        "{palette:?}: {rgb:06x} under AA on {ground:06x}: {:.2}:1",
                        contrast(rgb, ground)
                    );
                }
            }
        }
        // The line stops at the last column rather than wrapping.
        let mut card = HarnessCard::default();
        card.arm(t0, 3);
        assert_eq!(card.line_ink(t0, 0, u16::MAX - 1, 5, NIGHT, true).len(), 2);
    }

    /// THE HERO WORD (owner, 2026-10-19: "emphasize aterm in the banner"):
    /// the line's `aterm` ([`LineAt::hero`], cells 12 to 16 of "enhanced by
    /// aterm harness …") wears its own whole run of the card's palette —
    /// under Cruise a red letter and a blue one among its five, under Land
    /// the sunset's, never a blue — lifted a step above the line on dark
    /// and the paper law's ink at its place on light (4.5:1 or more against
    /// the ground, [`LineInks`]), in every palette; settled it
    /// is constant bytes and the same under reduced motion, in the intro it
    /// moves, and in the flow it glints (flaring harder than the line round
    /// it, by its own gains, const-asserted). NEGATIVE CONTROL: the hero
    /// touches only its own cells — every other cell is the hero-less
    /// line's, byte for byte — and it changes its own.
    #[test]
    fn the_lines_hero_word_wears_its_own_rainbow() {
        let t0 = Instant::now();
        let plain = LineAt {
            row: 5,
            col: 13,
            len: 55,
            hero: 0..0,
            wand: Some((10, 3)),
        };
        let at = LineAt {
            hero: 12..17,
            ..plain.clone()
        };
        let rgb = |c: &InkCell| {
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
        };
        let ink = |card: &HarnessCard, now: Option<Instant>, at: &LineAt, ground, animate| {
            let mut out = Vec::new();
            card.line_ink_into(now, at, ground, animate, &mut out);
            out
        };
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for ground in [NIGHT, DAY] {
                let dark = ground == NIGHT;
                let what = format!("{palette:?} dark {dark}");
                let mut card = HarnessCard::default();
                card.tick(
                    t0,
                    &CardScene {
                        palette,
                        dark,
                        ground,
                        ..scene()
                    },
                    false,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                let rest = ink(&card, None, &at, ground, false);
                let bare = ink(&card, None, &plain, ground, false);
                assert_eq!(rest.len(), 55, "{what}");
                for (i, c) in rest.iter().enumerate() {
                    assert_eq!((c.row, c.col), (5, 13 + i as u16), "{what}: contiguous");
                }
                for i in (0..55).filter(|i| !(12..17).contains(i)) {
                    assert_eq!(rest[i], bare[i], "{what}: cell {i} is the line's");
                }
                let hero: Vec<u32> = rest[12..17].iter().map(rgb).collect();
                let changed = (12..17).filter(|&i| rest[i] != bare[i]).count();
                assert!(
                    changed >= 3,
                    "{what}: the hero wears its own ink ({changed})"
                );
                let distinct = hero
                    .iter()
                    .enumerate()
                    .all(|(i, a)| hero[..i].iter().all(|b| b != a));
                assert!(distinct, "{what}: five letters, five colours {hero:06x?}");
                let ch = |c: u32, sh: u32| (c >> sh) & 0xff;
                let red = hero
                    .iter()
                    .any(|&c| ch(c, 16) > ch(c, 8) + 40 && ch(c, 16) > ch(c, 0) + 40);
                let blue = hero
                    .iter()
                    .any(|&c| ch(c, 0) > ch(c, 16) + 30 && ch(c, 0) > ch(c, 8));
                match palette {
                    CardPalette::Cruise => assert!(red && blue, "{what}: red to blue {hero:06x?}"),
                    CardPalette::Land => assert!(!blue, "{what}: a sunset {hero:06x?}"),
                    CardPalette::Off => {}
                }
                for &c in &hero {
                    if dark {
                        assert!(
                            relative_luminance(c) >= LINE_HERO_DARK_LUMA_FLOOR - 0.01,
                            "{what}: {c:06x} under the hero's floor"
                        );
                    } else {
                        assert!(
                            contrast(c, ground) >= 4.5,
                            "{what}: {c:06x} under AA on {ground:06x}"
                        );
                    }
                }
                // Settled: constant, and the same under reduced motion.
                card.arm(t0, 3);
                assert_eq!(
                    ink(&card, Some(t0 + ms(900)), &at, ground, false),
                    rest,
                    "{what}: reduced motion"
                );
                // The intro: it moves, frame after frame.
                let mut seen: Vec<Vec<InkCell>> = Vec::new();
                for t in (0..EPISODE_MS / 2).step_by(FRAME_MS as usize) {
                    let hero = ink(&card, Some(t0 + ms(t)), &at, ground, true)[12..17].to_vec();
                    if !seen.contains(&hero) {
                        seen.push(hero);
                    }
                }
                assert!(
                    seen.len() > 20,
                    "{what}: the intro moves it ({})",
                    seen.len()
                );
                // The flow: it glints.
                let flow = (0..GLINT_PERIOD_MS / FLOW_STEP_MS).any(|k| {
                    let now = t0 + ms(EPISODE_MS + 200 + k * FLOW_STEP_MS);
                    ink(&card, Some(now), &at, ground, true)[12..17] != rest[12..17]
                });
                assert!(flow, "{what}: the flow's glints reach the hero");
            }
        }
    }

    /// THE GLINTS RUN OUTWARD FROM THE WAND: the card lays Claude's ripple
    /// with its origin at the wand cell's centre ([`CardScene::wand`]) and a
    /// row two cells tall, as Claude does: along the wand's row the crest
    /// stands `7 cells/s · T` either side of it as its ring runs out, and
    /// three cells either way of the front it is down; a point two rows
    /// under the wand sees the crest when the ring has run four cells.
    /// CONTROL: with no wand the origin is the inner box's top-left corner.
    #[test]
    fn the_cards_glints_run_outward_from_the_wand() {
        use crate::ripple::GLINT_CELLS_PER_S;
        let s = scene();
        let g = Geo::new(&s).expect("a scene");
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let (wx, wy) = (g.ox as f32 + 10.5 * cw, g.band(3) as f32 + 0.5 * ch);
        let at = |ms: u64| Clock {
            t_ms: None,
            ride_ms: None,
            glint: Some((ms, 1.0)),
            flow: None,
        };
        for k in 3..15u64 {
            let ms = k * FLOW_STEP_MS;
            let cx = Emit::new(&s, g, at(ms), 7);
            let front = GLINT_CELLS_PER_S * ms as f32 / 1000.0;
            assert_eq!(
                cx.ring_at(wx + front * cw, wy),
                1.0,
                "{ms} ms: the front, right"
            );
            assert_eq!(
                cx.ring_at(wx - front * cw, wy),
                1.0,
                "{ms} ms: the front, left"
            );
            for off in [-3.0, 3.0] {
                assert!(
                    cx.ring_at(wx + (front + off) * cw, wy) < 0.5,
                    "{ms} ms: {off} cells off the front"
                );
            }
        }
        let four = (4000.0 / GLINT_CELLS_PER_S).round() as u64;
        let cx = Emit::new(&s, g, at(four), 7);
        assert_eq!(
            cx.ring_at(wx, wy + 2.0 * ch),
            1.0,
            "two rows down: four cells"
        );
        assert!(
            cx.ring_at(wx, wy + 3.0 * ch) < 0.5,
            "three rows down: not yet"
        );
        let bare = CardScene { wand: None, ..s };
        let gb = Geo::new(&bare).expect("a scene");
        let inner = gb.inner();
        let ms = 10 * FLOW_STEP_MS;
        let cx = Emit::new(&bare, gb, at(ms), 7);
        let front = GLINT_CELLS_PER_S * ms as f32 / 1000.0;
        assert_eq!(
            cx.ring_at(inner.x0 as f32 + front * cw, inner.y0 as f32),
            1.0,
            "CONTROL: no wand, the inner box's corner"
        );
    }

    /// THE FLOW STAYS UNDER THE PHOTOSENSITIVITY BOUND: over a minute of
    /// flow on dark (additive light, where a flash reads brightest) under
    /// each palette, a point on the border's top edge, one on the trail and
    /// one on the line flash — a rise in relative luminance of 0.1 or more
    /// from the last trough, after a fall as deep — at most
    /// [`TWINKLE_FLASH_BOUND_HZ`] times a second. CONTROL: each does flash,
    /// the ring passing it, at least every other ripple cycle.
    #[test]
    fn the_flow_stays_under_the_photosensitivity_bound() {
        const SWING: f32 = 0.1;
        const MINUTE_MS: u64 = 60_000;
        let flashes = |series: &[f32]| {
            let (mut n, mut lo, mut hi, mut rising) = (0, f32::MAX, f32::MIN, true);
            for &v in series {
                if rising {
                    lo = lo.min(v);
                    if v - lo >= SWING {
                        n += 1;
                        rising = false;
                        hi = v;
                    }
                } else {
                    hi = hi.max(v);
                    if hi - v >= SWING {
                        rising = true;
                        lo = v;
                    }
                }
            }
            n
        };
        let lum = |premul: u32| relative_luminance(premul);
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            let s = CardScene { palette, ..scene() };
            let g = Geo::new(&s).expect("a scene");
            let covering = |quads: &[GlowQuad], x: i64, y: i64| {
                quads
                    .iter()
                    .find(|q| {
                        let (qx, qy) = (i64::from(q.x), i64::from(q.y));
                        (qx..qx + i64::from(q.w)).contains(&x)
                            && (qy..qy + i64::from(q.h)).contains(&y)
                    })
                    .map(|q| q.color)
            };
            let border = (g.col(26) + 2, g.outer.y0);
            let (_, yb) = trail_band(&g, &s);
            // The trail's bottom stripe and a late letter of the line: the
            // palette's deep end (violet, rose, steel), where a glint
            // lifts the luminance most.
            let trail = (g.col(36) + 2, yb - 2);
            let line = LineAt {
                row: 6,
                col: 13,
                len: 50,
                hero: 12..17,
                wand: Some((10, 3)),
            };
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
            let (mut b, mut t, mut l) = (Vec::new(), Vec::new(), Vec::new());
            let mut ink = Vec::new();
            for at in (EPISODE_MS..EPISODE_MS + MINUTE_MS).step_by(FLOW_STEP_MS as usize) {
                let now = t0 + ms(at);
                let (under, _, _) = frame(&mut card, now, &s, true);
                b.push(lum(
                    covering(&under, border.0, border.1).expect("the border")
                ));
                t.push(lum(covering(&under, trail.0, trail.1).expect("the trail")));
                ink.clear();
                card.line_ink_into(Some(now), &line, NIGHT, true, &mut ink);
                let c = ink[46].color;
                l.push(lum((u32::from(c[0]) << 16)
                    | (u32::from(c[1]) << 8)
                    | u32::from(c[2])));
            }
            for (what, series) in [("border", &b), ("trail", &t), ("line", &l)] {
                let n = flashes(series);
                let hz = n as f32 * 1000.0 / MINUTE_MS as f32;
                assert!(
                    hz <= TWINKLE_FLASH_BOUND_HZ,
                    "{palette:?} {what}: {n} flashes a minute, {hz} Hz"
                );
                assert!(
                    n as u64 >= MINUTE_MS / (2 * GLINT_PERIOD_MS),
                    "{palette:?} {what}: CONTROL, {n} flashes a minute"
                );
            }
        }
    }

    /// The line wears the last tick's palette: Land's differs from Cruise's,
    /// and Off's from both — its letters are [`CardPalette::letter_on`]'s,
    /// on both grounds (on light at the nearest of the kept steps,
    /// [`LineInks::at`]).
    #[test]
    fn the_line_wears_the_cards_palette() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        let cruise = card.line_ink(t0, 0, 0, 30, NIGHT, false);
        let mut under = Vec::new();
        let mut over = Vec::new();
        let mut wear = |card: &mut HarnessCard, palette| {
            card.tick(
                t0,
                &CardScene { palette, ..scene() },
                false,
                &mut under,
                &mut over,
            );
            card.line_ink(t0, 0, 0, 30, NIGHT, false)
        };
        let land = wear(&mut card, CardPalette::Land);
        assert_ne!(land, cruise);
        let off = wear(&mut card, CardPalette::Off);
        assert_ne!(off, cruise, "Off is not Cruise");
        assert_ne!(off, land, "Off is not Land");
        for ground in [NIGHT, DAY] {
            let line = card.line_ink(t0, 0, 0, 30, ground, false);
            let steps = LINE_INK_STEPS as f32;
            for (i, c) in line.iter().enumerate() {
                let t = tri(i as f32 / 29.0);
                let t = if dark_ground(ground) {
                    t
                } else {
                    (t * steps).round() / steps
                };
                let want = CardPalette::Off.letter_on(t, ground);
                assert_eq!(
                    c.color,
                    [(want >> 16) as u8, (want >> 8) as u8, want as u8],
                    "on {ground:06x}: cell {i} is the palette's letter ink"
                );
            }
        }
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

    /// The moonbow ramp runs white → pearl → silver → steel and is total;
    /// Off's art and stripes read it on dark and [`PRINT_OFF`] on light.
    #[test]
    fn the_moonbow_ramp_hits_its_stops() {
        assert_eq!(moonbow(0.0), WHITE);
        assert_eq!(moonbow(0.0), MOONBOW[0]);
        assert_eq!(moonbow(1.0 / 3.0), MOONBOW[1]);
        assert_eq!(moonbow(2.0 / 3.0), MOONBOW[2]);
        assert_eq!(moonbow(1.0), MOONBOW[3]);
        assert_eq!(moonbow(f32::NAN), MOONBOW[0]);
        assert_eq!(moonbow(-3.0), MOONBOW[0]);
        assert_eq!(moonbow(7.0), MOONBOW[3]);
        assert_eq!(CardPalette::Off.art(0.0, true), WHITE);
        assert_eq!(CardPalette::Off.art(1.0, false), PRINT_OFF[5]);
        assert_eq!(CardPalette::Off.stripe(0, true), WHITE);
        assert_eq!(CardPalette::Off.stripe(5, true), MOONBOW[3]);
        assert_eq!(CardPalette::Off.stripe(5, false), PRINT_OFF[5]);
        // Every stop is bright enough that the line's lift leaves it be.
        for c in MOONBOW {
            assert!(relative_luminance(c) >= 0.41, "{c:06x}");
            assert_eq!(lift_to_luminance(c, LINE_DARK_LUMA_FLOOR), c);
        }
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

    /// OFF WEARS THE MOONBOW (owner, 2026-10-19: "a white themed for 'off'"):
    /// every settled border colour is a point on the moonbow at the border's
    /// coverage, every trail stripe its stop at the trail's — white at the
    /// top through pearl and silver to steel at the bottom as light on dark
    /// ([`MOONBOW`]), pearl to graphite in opaque ink on light ([`PRINT_OFF`],
    /// the border's held under its line bar) — on both heights. NEGATIVE
    /// CONTROL: Cruise's and Land's borders are off the moonbow (their hues
    /// are not silver).
    #[test]
    fn off_wears_the_moonbow() {
        for dark in [true, false] {
            for kitty_rows in [2u8, 3] {
                let off = CardScene {
                    palette: CardPalette::Off,
                    dark,
                    kitty_rows,
                    ..scene()
                };
                let border = |t: f32| {
                    if dark {
                        light(moonbow(t), f32::from(BORDER_COV)).color
                    } else {
                        ink(print_line(even_ramp(&PRINT_OFF, t)), PRINT_ALPHA).color
                    }
                };
                let stripe = |rgb: u32| {
                    if dark {
                        light(rgb, f32::from(TRAIL_COV)).color
                    } else {
                        ink(rgb, PRINT_ALPHA).color
                    }
                };
                let (under, _, _) = settled(&off);
                let (rim, trail): (Vec<&GlowQuad>, Vec<&GlowQuad>) =
                    under.iter().partition(|q| on_rim(q, &off));
                assert!(!rim.is_empty() && !trail.is_empty(), "dark {dark}");
                for q in &rim {
                    for c in [q.color, q.color2] {
                        let gap = gap_to_ramp(c, border);
                        assert!(
                            gap <= 2,
                            "dark {dark}, {kitty_rows} rows: border {c:06x} off the moonbow by {gap}"
                        );
                    }
                }
                let stops: Vec<u32> = (0..TRAIL_STOPS.len())
                    .map(|k| {
                        stripe(if dark {
                            moonbow(k as f32 / (TRAIL_STOPS.len() - 1) as f32)
                        } else {
                            PRINT_OFF[k]
                        })
                    })
                    .collect();
                let worn: std::collections::BTreeSet<u32> =
                    trail.iter().flat_map(|q| [q.color, q.color2]).collect();
                assert_eq!(
                    worn,
                    stops.iter().copied().collect(),
                    "dark {dark}, {kitty_rows} rows: the trail is the moonbow's six stops"
                );
                let top = if dark { WHITE } else { PRINT_OFF[0] };
                assert_eq!(stops[0], stripe(top), "white (pearl in print) on top");
                assert_eq!(
                    stops[5],
                    stripe(if dark { MOONBOW[3] } else { PRINT_OFF[5] }),
                    "steel (graphite in print) at the bottom"
                );
                // The negative control: the rainbows leave the moonbow.
                for other in [CardPalette::Cruise, CardPalette::Land] {
                    let s = CardScene {
                        palette: other,
                        ..off
                    };
                    let (under, _, _) = settled(&s);
                    let off_by = under
                        .iter()
                        .filter(|q| on_rim(q, &s))
                        .map(|q| gap_to_ramp(q.color, border))
                        .max()
                        .unwrap_or(0);
                    assert!(
                        off_by > 40,
                        "dark {dark}: {other:?}'s border is not a moonbow ({off_by})"
                    );
                }
            }
        }
    }

    /// Off's light trail is SILVER, not tinted: at most this OKLCh chroma a
    /// stripe.
    const PRINT_OFF_MAX_CHROMA: f64 = 0.04;

    /// Off's light trail law for six stripes, each a quad end's
    /// `(premultiplied colour, alpha)` top to bottom: a PEARL ON PAPER —
    /// every stripe opaque; on white paper at most
    /// [`PRINT_OFF_MAX_CHROMA`] (a silver, not a colour), at least
    /// [`PRINT_MIN_PAPER_CONTRAST`] against the paper (white ink vanishes);
    /// no two stripes within [`PRINT_MIN_STRIPE_DE`]; and OKLab L falling
    /// top to bottom, as the dark moonbow runs white to steel. Every breach,
    /// empty when it holds.
    fn pearl_on_paper(stripes: &[(u32, u8)]) -> Vec<String> {
        use aterm_types::Rgb;
        use aterm_types::scheme_signature::{chroma_hue, oklab, oklab_distance};
        if stripes.len() != TRAIL_STOPS.len() {
            return vec![format!("{} stripes", stripes.len())];
        }
        let mut breaches = Vec::new();
        let paper = |(c, a): (u32, u8)| {
            let ch = |sh: u32| (((c >> sh) & 0xff) + 255 - u32::from(a)).min(255) as u8;
            Rgb::new(ch(16), ch(8), ch(0))
        };
        let on: Vec<Rgb> = stripes.iter().map(|&s| paper(s)).collect();
        for (k, (&(c, a), rgb)) in stripes.iter().zip(&on).enumerate() {
            if a != 255 {
                breaches.push(format!("stripe {k} ({c:06x}) is not opaque: alpha {a}"));
            }
            let px = (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b);
            let (chroma, _) = chroma_hue(*rgb);
            if chroma > PRINT_OFF_MAX_CHROMA {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) is tinted: chroma {chroma:.3}"
                ));
            }
            let contrast = 1.05 / (relative_luminance(px) + 0.05);
            if contrast < PRINT_MIN_PAPER_CONTRAST {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) vanishes on the paper: {contrast:.2}:1"
                ));
            }
        }
        for i in 0..on.len() {
            for j in i + 1..on.len() {
                let d = oklab_distance(on[i], on[j]);
                if d < PRINT_MIN_STRIPE_DE {
                    breaches.push(format!("stripes {i} and {j} are one colour (dE {d:.3})"));
                }
            }
        }
        for k in 1..on.len() {
            let (above, here) = (oklab(on[k - 1]).0, oklab(on[k]).0);
            if here >= above {
                breaches.push(format!(
                    "stripe {k} is out of order: L {here:.3} under {above:.3}"
                ));
            }
        }
        breaches
    }

    /// THE LIGHT OFF TRAIL IS PEARL PRINT: at both heights the settled light
    /// Off trail passes [`pearl_on_paper`] — opaque, silver, on the paper,
    /// distinct, falling in lightness. NEGATIVE CONTROLS, each caught for
    /// its own reason: Cruise's inks are tinted, every one; six opaque
    /// whites vanish and are one colour; the dark moonbow's stripes laid as
    /// print vanish (its white, its first pearl and the one after it); Off's
    /// inks upside down are out of order, every step; and Off's inks fail
    /// the rainbow's vividness law as washed out, which is why Off leaves
    /// that law for this one.
    #[test]
    fn the_light_off_trail_is_pearl_print() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                dark: false,
                palette: CardPalette::Off,
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
                pearl_on_paper(&stripes),
                Vec::<String>::new(),
                "{kitty_rows} rows"
            );
        }
        let lay = |rgb: u32| {
            let p = ink(rgb, PRINT_ALPHA);
            (p.color, p.alpha)
        };
        let count =
            |breaches: &[String], what: &str| breaches.iter().filter(|b| b.contains(what)).count();
        let cruise: Vec<(u32, u8)> = PRINT_CRUISE.iter().map(|&c| lay(c)).collect();
        let cruise = pearl_on_paper(&cruise);
        assert_eq!(count(&cruise, "is tinted"), 6, "{cruise:?}");
        let white = pearl_on_paper(&[lay(WHITE); 6]);
        assert!(breached(&white, "vanishes"), "{white:?}");
        assert!(breached(&white, "one colour"), "{white:?}");
        let moon: Vec<(u32, u8)> = (0..TRAIL_STOPS.len())
            .map(|k| lay(CardPalette::Off.stripe(k, true)))
            .collect();
        let moon = pearl_on_paper(&moon);
        assert_eq!(count(&moon, "vanishes"), 3, "{moon:?}");
        for k in 0..3 {
            assert!(
                breached(&moon, &format!("stripe {k} (")),
                "stripe {k}: {moon:?}"
            );
        }
        let flipped: Vec<(u32, u8)> = PRINT_OFF.iter().rev().map(|&c| lay(c)).collect();
        let flipped = pearl_on_paper(&flipped);
        assert_eq!(count(&flipped, "out of order"), 5, "{flipped:?}");
        let vivid: Vec<(u32, u8)> = PRINT_OFF.iter().map(|&c| lay(c)).collect();
        let vivid = vivid_on_paper(CardPalette::Off, &vivid);
        assert!(breached(&vivid, "washed out"), "{vivid:?}");
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
        let mid = Clock::seated(1500 + 750);
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
            // Off is no rainbow and leaves this law (its own is
            // [`pearl_on_paper`]): checked here only as the negative control
            // that says why — its silver is washed out by this law's measure.
            CardPalette::Off => hues.len() - 1,
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
    /// light border quad end, all three palettes, both heights, is a point on its
    /// palette's print ramp ([`CardPalette::art_at`]) held under the line
    /// bar ([`print_line`]), and every border quad end, settled, with the
    /// head passing or with a glint passing, is under the bar ([`LIGHT_LINE_MAX_LUMA`]) while the rim
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
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
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
                    Clock::seated(900),
                    Clock::flowing(600),
                    Clock::flowing(1400),
                    Clock::flowing(2300),
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
                        let clock = t_ms.map_or(Clock::SETTLED, Clock::riding);
                        let frame = at.art.frame(kitty_frame(clock));
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
                        seat.art.frame(kitty_frame(flick)),
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
    /// both grounds, in every frame, at three cell sizes — the one-pixel
    /// gaps between the four paws, the same at both heights.
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
                        let frame = seat.art.frame(kitty_frame(clock));
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
                                // The paw on each side wears the rim under
                                // its foot. Every gap of the shipped art is
                                // on the paws' last row; the skip is for art
                                // whose gap opened higher up the legs, where
                                // the leg beside it goes on down and wears
                                // no rim under that row.
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
