// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! "Sparkle words": render-time decorations on whole words the lexicon matches.

use aterm_time::Instant;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use aterm_core::render::{FxArt, PxRect};
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_hash::{FNV64_OFFSET, FxHashMap, KeyedHashMap, fnv_mix_u64, fnv1a_64};
use aterm_lexicon::{Class, FormId, LangSet, Lexicon, Match, ScanOptions, ScanScratch};
use aterm_render::sprites::{FreeSprite, FreeZ};
use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, InkCell, WordDecoration};
use aterm_scene::smoothstep;
use aterm_scene::vector::FIXED_ONE;

use crate::animal_baker::{AnimalBakeKey, AnimalBaker};
use crate::animal_glyphs_gen::{ANIMAL_GLYPHS, AnimalGlyphId, animal_glyph_from_key};
use crate::cat_baker::{BakeKeyV4, CatBaker, CatColorKey, CatTile, EyesFrame, PATCH_STRIP};
use crate::cat_glyphs_gen::{CatGlyphId, GLYPHS, GlyphDef};
#[cfg(test)]
use crate::color_math::relative_luminance;
use crate::color_math::{hsv2rgb, hue_nudge};
use crate::effect_util::lerp_rgb as mix_rgb;
use crate::genome::{
    self, CatAge, Genome, NovaFeatures, NovaMagic, VoteScratch, accessory_variant_v4, cat_age_v4,
    cat_fills_v4, cat_magic, cat_variant_v4, mix, nova_duration_ms, nova_features, nova_magic,
    nova_palette, nova_radius, special_variant_v4,
};
use crate::kitty_registry::{
    KittyLook, KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BOW, TRAIT_CROWN,
};
use crate::nova::{self, NovaEnv};
use crate::spec::{
    BurstKind, BurstSpec, Collection, Colorway, GraphicSpec, InkSpec, SpecTable, WordEffectSpec,
};
use crate::supernova::{self, SuperEnv};
use crate::trick_flash::TrickFlash;
pub use crate::trick_flash::TrickFlashPhase;

const MAX_OCCURRENCES: usize = 128;
const MAX_DECORATIONS: usize = 256;
const PET_DEPART_HOLDS: usize = 2 * crate::kitty_pet::PET_DEPARTURES_MAX;
const MAX_INK_CELLS: usize = 512;
/// Specular sweep ramp-in (ms) — §4.2 `env(t)`.
pub(crate) const INK_RAMP_IN_MS: u64 = 130;
/// Specular sweep ramp-out after `sweep_ms` (ms) — §4.2: after `sweep_ms + 250` the emitted color is exactly the static gradient, constant bytes forever.
pub(crate) const INK_FADE_MS: u64 = 250;
/// §4.3 legibility bound: the word's mid-gradient ink must hold at least this WCAG contrast against the cell background, else the mix is pulled toward the captured base fg until it does.
pub(crate) const MIN_INK_CONTRAST: f32 = 3.5;
const FELINE_GLOW_MS: u64 = 1400;
const FELINE_GLOW_LIFT: f32 = 0.72;
const FELINE_GLOW_AMP: f32 = 0.55;
const PERSIST_CAP: usize = 512;
const GRACE_TTL: Duration = Duration::from_secs(10);
const REKEY_MAX_SEQ_GAP: u64 = 2;
const REKEY_MAX_LINEAR_ROWS: u32 = 2;
const ALIGN_DECISION_CELLS: usize = (PERSIST_CAP + 1) * (MAX_OCCURRENCES + 1);
const ORDINAL_MIX: u64 = 0x9E37_79B9_7F4A_7C15;
const SCAN_MEMO_MIN_GEN: usize = 32;
const SCAN_MEMO_MAX_GEN: usize = 4096;
const MAX_CATS: usize = 8;
const CAT_RISE_MS: u64 = 450;
const CAT_BOUNCE_MS: u64 = 150;
const CAT_MIN_CELL_H: u16 = 14;
const CAT_MIN_CELL_W: u16 = 7;

const MAX_ANIMALS: usize = 8;
const CAT_DESCEND_MS: u64 = 320;
const CAT_ANTICIPATION_MS: u64 = 60;
const CAT_DWELL_CAP_MS: u64 = 3750;
const CAT_DWELL_JITTER_MS: u64 = 300;
const CAT_DWELL_MAGIC_BONUS_MS: u64 = 500;
const RESIZE_SETTLE_MS: u64 = 500;
const DONE_MARKS_CAP: usize = 65_536;
const DONE_MARK_NONE: u32 = u32::MAX;
const RESIDUAL_FADE_MS: u64 = 2000;
const DWELL_SALT: u64 = 0xD3E1_15A1_C0FF_EE00;
const ANTIC_SALT: u64 = 0xA117_1C1F_7A11_57A6;
const IGNITION_WINDOW: Duration = Duration::from_secs(1);
const MAX_RECENT_IGNITIONS: usize = 2;
const MAX_IGNITION_RESERVATIONS: usize = PERSIST_CAP + MAX_RECENT_IGNITIONS;

const GLOW_BURST_MS: u64 = 1400;
const RAINBOW_SPARKLE_MS: u64 = 1000;

/// App-cached sparkle state: the resolved [`DecoConfig`] plus the compiled lexicon, rebuilt only on config reload / toggle, never per frame.
#[derive(Clone)]
pub struct Resolved {
    pub cfg: DecoConfig,
    pub lexicon: std::sync::Arc<Lexicon>,
}

/// `[sparkle_words.feline] style`: the peeking cat, or the ink-only mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FelineStyle {
    /// `style = "cat"` (the default): the authored peeking-cat graphic.
    Cat,
    /// `style = "paw"`: ink only, no paw graphic.
    Paw,
}

/// `[sparkle_words.profanity] style` (§10 / v3 §3.1): rainbow ink (default), the classic nova, or the randomized sparkle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfanityStyle {
    /// `style = "rainbow"` (default): the §3.1 animated rainbow ink, with `supernova_chance`% of episodes escalating to the §3.2 supernova.
    Rainbow,
    /// `style = "nova"`: the §6 classic nova.
    Nova,
    /// `style = "sparkle"`: the randomized sparkle (`supernova_chance` ignored).
    Sparkle,
}

/// A borrowed view of the terminal selection for the tick (§6.4): ignition defers while the word is selected, and an active nova's quads attenuate over selected cells (applied per quad host-side, so both backends stay byte-identical).
#[derive(Clone, Copy)]
pub struct SelView<'a> {
    pub sel: &'a aterm_core::selection::TextSelection,
    pub display_offset: i32,
}

impl SelView<'_> {
    #[inline]
    fn cell_selected(&self, row: u16, col: u16) -> bool {
        self.sel.contains(i32::from(row) - self.display_offset, col)
    }

    #[inline]
    fn span_selected(&self, row: u16, c0: u16, c1: u16) -> bool {
        let mut c = c0;
        while c <= c1 {
            if self.cell_selected(row, c) {
                return true;
            }
            if c == u16::MAX {
                break;
            }
            c += 1;
        }
        false
    }
}

/// Per-frame effect geometry (§2.4 `EffectGeom`): the cell metrics and grid extent the cat emitter sizes against.
#[derive(Clone, Copy, Debug)]
pub struct EffectGeom {
    /// Cell width in px.
    pub cell_w: u16,
    /// Cell height in px.
    pub cell_h: u16,
    /// Viewport rows (quad rows past this are dropped defensively).
    pub rows: u16,
    /// Viewport cols (the horizontal anchor clamp bound).
    pub cols: u16,
}

impl EffectGeom {
    /// Zeroed effect geometry before any frame is prepared.
    pub const EMPTY: Self = Self {
        cell_w: 0,
        cell_h: 0,
        rows: 0,
        cols: 0,
    };
}

impl Default for EffectGeom {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// The host's per-frame view of the cursor companion, for the one-cat-per-caret suppression ([`WordDecorations::tick`]).
#[derive(Clone, Copy, Debug)]
pub struct CompanionOnGlass {
    /// The visible caret cell `(row, col)` in pre-splice grid coords.
    pub cell: Option<(u16, u16)>,
    /// The companion's live drawn sprite `(x0, x1, y0, y1)` in grid pixels, right/bottom exclusive.
    pub body_px: Option<(i32, i32, i32, i32)>,
    /// Does the caret cell's own band belong to the companion as well? True for the caret-escorting flying head, false for the resident pet.
    pub guards_caret: bool,
}

impl CompanionOnGlass {
    /// The flying head with no resolved sprite rect: only the caret band stands in, under-covering the sprite.
    #[inline]
    #[must_use]
    pub const fn at_cell(cell: (u16, u16)) -> Self {
        Self {
            cell: Some(cell),
            body_px: None,
            guards_caret: true,
        }
    }

    /// The flying head with its live drawn rect, from [`WordDecorations::kitty_cursor_footprint`].
    #[inline]
    #[must_use]
    pub const fn at_head(cell: (u16, u16), head_px: (i32, i32, i32, i32)) -> Self {
        Self {
            cell: Some(cell),
            body_px: Some(head_px),
            guards_caret: true,
        }
    }

    /// A resident pet on glass.
    #[inline]
    #[must_use]
    pub const fn at_body(cell: Option<(u16, u16)>, body_px: (i32, i32, i32, i32)) -> Self {
        Self {
            cell,
            body_px: Some(body_px),
            guards_caret: false,
        }
    }

    #[inline]
    #[must_use]
    const fn footprint(self, geom: EffectGeom) -> CompanionFootprint {
        let caret_rect = if let (true, Some((row, col))) = (self.guards_caret, self.cell) {
            let cw = geom.cell_w as i32;
            let ch = geom.cell_h as i32;
            let x0 = (col as i32) * cw;
            let y0 = (row as i32) * ch;
            Some((x0, x0 + 2 * cw, y0, y0 + ch))
        } else {
            None
        };
        CompanionFootprint {
            rects: [self.body_px, caret_rect],
        }
    }
}

/// The pixels the live cursor companion owns this frame — up to two rects; an ambient cat yields if it overlaps either.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompanionFootprint {
    rects: [Option<(i32, i32, i32, i32)>; 2],
}

impl Default for CompanionFootprint {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl CompanionFootprint {
    pub(crate) const EMPTY: Self = Self {
        rects: [None, None],
    };

    /// Is the cat rect `(x, w, top..bottom)` stacked on any companion rect?
    #[inline]
    #[must_use]
    pub(crate) const fn stacked_on(&self, x: i32, w: i32, top: i32, bottom: i32) -> bool {
        let mut i = 0;
        while i < self.rects.len() {
            if let Some(rect) = self.rects[i]
                && cat_is_stacked_on(rect, x, w, top, bottom)
            {
                return true;
            }
            i += 1;
        }
        false
    }
}

/// One frame of the full-body pet, ready to draw.
#[derive(Clone, Copy, Debug)]
pub struct PetCursorFrame {
    pub geom: EffectGeom,
    /// Quantized local foreground/background context (drives the contrast ink): the live body's.
    pub colors: CatColorKey,
    /// `COAT_RAMP` / `EYE_RAMP` indices latched for this appearance ([`crate::kitty_pet::PetBrain::sync_look`]), so a mid-walk companion change cannot re-skin the pet on screen.
    pub coat: u8,
    pub iris: u8,
    pub pet: crate::kitty_pet::PetFrame,
    /// The light page under the pet: the terminal ground where it is no dark theme (`aterm_render::theme_is_dark`), `None` on a dark one.
    pub page: Option<u32>,
}

/// One resolved cursor-companion emission (grouped so the render call does not grow a positional argument list).
#[derive(Clone, Copy, Debug)]
pub struct KittyCursorFrame {
    pub geom: EffectGeom,
    pub cursor: (u16, u16),
    pub look: KittyLook,
    pub colors: CatColorKey,
    pub bob: f32,
    /// Whole rows the seat raises the head ([`KittyCursorLayout::lift`]).
    pub lift: f32,
    pub alpha: u8,
    /// The pose ([`crate::kitty_cursor::CatPose`]): squash/stretch and lean applied to the dest rect, plus the baked eye frame.
    pub pose: crate::kitty_cursor::CatPose,
    /// Sing-along drive 0..=1 (`crate::kitty_sing`): scales note alpha for the wind-down crossfade. 0.0 with an empty `notes` ring does no note work.
    pub sing: f32,
    /// This frame's ♪/♫ sprites (cell-relative to the mouth anchor), capped at [`crate::kitty_sing::MAX_NOTES`].
    pub notes: [Option<crate::kitty_sing::NoteSprite>; crate::kitty_sing::MAX_NOTES],
}

/// One resolved typed-word dog cameo emission ([`crate::dog_cameo`]): breed, coat, envelope alpha and entry-bounce offset — [`KittyCursorFrame`] minus the persistent-companion parts.
#[derive(Clone, Copy, Debug)]
pub struct DogCameoFrame {
    pub geom: EffectGeom,
    /// The cursor cell `(row, col)` the dog visits.
    pub cursor: (u16, u16),
    /// The visit's rolled identity ([`crate::dog_cameo::DogCameo::look`]).
    pub look: crate::dog_cameo::DogLook,
    /// Quantized local terminal palette (shared with the cat bakes).
    pub colors: CatColorKey,
    /// Entry-bounce offset in cell fractions (`<= 0`: up); `0.0` under reduced motion.
    pub bob: f32,
    /// The lifecycle envelope's presence, `0..=255`.
    pub alpha: u8,
    /// The frame's clock: the dog glides between its seats on it ([`WordDecorations::dog_cameo`]).
    pub now: Instant,
}

/// Where the typed dog sits against the caret's row ([`WordDecorations::dog_cameo`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DogSeat {
    /// Centred on the caret's row, behind the caret: where it trails.
    Behind,
    /// Flush over the caret's row.
    Above,
    /// Flush under the caret's row.
    Under,
}

/// The typed dog's place on its visit's last frame: its look (one visit's), its seat, the drawn
/// top-left (px, before the entry bounce) and the frame's clock.
#[derive(Clone, Copy, Debug)]
struct DogPlace {
    look: crate::dog_cameo::DogLook,
    seat: DogSeat,
    at: (f32, f32),
    drawn: Instant,
}

/// The typed dog's glide between frames, s: the flying head's follower ([`WordDecorations::PLACE_TAU`]).
const DOG_PLACE_TAU: f32 = 0.08;

/// Geometry-only inputs for resolving the cursor companion before its local palette is sampled.
#[derive(Clone, Copy, Debug)]
pub struct KittyCursorLayout {
    pub geom: EffectGeom,
    pub cursor: (u16, u16),
    pub look: KittyLook,
    /// The hover bob, in cell heights: drawn as it is, never smoothed.
    pub bob: f32,
    /// Whole rows the companion router's seat raises the head off its rest (the one-row-up seat of `rainbow_kitty::companion::placement`).
    pub lift: f32,
}

/// The cursor companion's prospective pixel footprint, resolved by the same sizing/placement code as emission so palette sampling matches the art.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatFootprint {
    pub x: i32,
    pub y: i32,
    pub w: u16,
    pub h: u16,
}

/// The least air the flying head keeps round the caret's cell, on every side,
/// as a share of its lead ([`KittyCursorPlacement::keep_off_letters`]): half
/// the shipped ¾-cell lead, so a head the follower trails into the caret is
/// pushed off with air between them, not glued to its side, and a head lifted
/// over the caret flies above it instead of perching on its top.
pub const HEAD_CARET_GAP_DEN: i32 = 2;

#[cfg(test)]
thread_local! {
    /// The negative control of [`KittyCursorPlacement::keep_off_letters`]: the
    /// head is drawn where the follower put it.
    pub static HEAD_KEEPS_OFF_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the caret's ring of air (looks10 round 3): only a
    /// head over the caret's cell itself is kept off, as round 2 kept it, so a
    /// head flush beside the caret stays there and a lifted one sits a pixel
    /// over its top.
    pub static HEAD_CARET_RING_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the graze's step (composed r3): a head a bob
    /// lays a few pixels onto a lettered row is lifted clear over the whole
    /// row, as before.
    pub static HEAD_GRAZE_LIFTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the head's flight to a far caret ([`HEAD_FLIGHT`]): the head is
    /// placed at the landing on the jump's frame, as before.
    pub static HEAD_FLIGHT_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the typed dog's seat ([`WordDecorations::dog_cameo`]): it is drawn
    /// centred on the caret's row behind the caret, over whatever lies there, as before.
    pub static DOG_SEAT_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the typed dog's keep-off of the word icons
    /// ([`WordDecorations::dog_cameo`]): its seats are judged by the letters and the caret alone.
    pub static DOG_ICONS_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// How long the flying head takes to fly from where it was last drawn to a caret that went far
/// (a meteor's jump: Enter, a line clear, Home or End; a jump past the follower's two rows): the
/// time its line fold takes to fly it off one edge and in from the other
/// ([`crate::kitty_cursor::CURSOR_FOLD_DURATION`]). Drawn at the landing on the jump's own
/// frame, the head blinked across the screen (49 to 58 cells in one frame at Enter and Ctrl+U).
pub const HEAD_FLIGHT: Duration = crate::kitty_cursor::CURSOR_FOLD_DURATION;

/// The flying head's flight on one frame ([`HEAD_FLIGHT`]): where it took off (its sprite's
/// top-left as last drawn), how far through the flight the frame is (`0..1`), and the arc's
/// height at the middle, px.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HeadFlightSample {
    from: (i32, i32),
    progress: f32,
    arc_px: i32,
}

impl HeadFlightSample {
    /// Where the head is drawn on this frame of its flight to `to` (the landing's top-left):
    /// eased out of the take-off and into the landing, along a low arc.
    #[inline]
    fn at(self, to: (i32, i32)) -> (i32, i32) {
        let q = self.progress.clamp(0.0, 1.0);
        let e = q * q * (3.0 - 2.0 * q);
        let lerp = |a: i32, b: i32| -> i32 {
            ((a as f64) + ((b as f64) - (a as f64)) * f64::from(e))
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };
        let lift = (self.arc_px as f32 * 4.0 * q * (1.0 - q)).round() as i32;
        (
            lerp(self.from.0, to.0),
            lerp(self.from.1, to.1).saturating_sub(lift),
        )
    }
}

/// One cursor-kitty placement resolved once for palette sampling and sprite emission.
#[derive(Clone, Copy, Debug)]
pub struct KittyCursorPlacement {
    pub sample: CatFootprint,
    leg: KittyCursorPlacementLeg,
    facing_left: bool,
    sampled_at: Option<Instant>,
    drawn: Option<PreparedKittyCursorDraw>,
    /// The head's flight to this frame's landing, while it flies ([`HEAD_FLIGHT`]).
    flight: Option<HeadFlightSample>,
}

#[derive(Clone, Copy, Debug)]
struct PreparedKittyCursorDraw {
    /// Where the head rests on this frame: the landing, while it flies.
    rect: CatFootprint,
    settle: Option<(f32, f32)>,
    /// The flight to `rect`, while the head flies ([`Self::shown`]).
    flight: Option<HeadFlightSample>,
}

impl PreparedKittyCursorDraw {
    /// The rectangle drawn on this frame: [`Self::rect`], or the head's place on its flight to it.
    #[inline]
    fn shown(self) -> CatFootprint {
        match self.flight {
            Some(flight) => {
                let (x, y) = flight.at((self.rect.x, self.rect.y));
                CatFootprint {
                    x,
                    y: y.max(0),
                    ..self.rect
                }
            }
            None => self.rect,
        }
    }
}

impl KittyCursorPlacement {
    #[inline]
    const fn settled(sample: CatFootprint) -> Self {
        Self {
            sample,
            leg: KittyCursorPlacementLeg::Settled,
            facing_left: false,
            sampled_at: None,
            drawn: None,
            flight: None,
        }
    }

    /// The actual animated body rectangle prepared for this frame, including its follower, pose,
    /// fold leg and flight.
    #[inline]
    #[must_use]
    pub fn drawn_footprint(self) -> Option<CatFootprint> {
        self.drawn.map(PreparedKittyCursorDraw::shown)
    }

    /// Keep the prepared head off the caret's cell and every lettered cell
    /// (looks10 L10, looks8's companion law): the follower's glide trails the
    /// caret through a burst, and a seat change at the right margin glides
    /// up out of the caret's row, both through the caret and the last
    /// letters. The head keeps a ring of air round the caret's cell,
    /// [`HEAD_CARET_GAP_DEN`] of its lead on every side: a head flush beside
    /// the caret laid its whiskers on it, and one lifted flush over its top
    /// read as perched on it (round 3). The head is pushed off on the side its
    /// centre is on, out of that ring, and failing that lifted clear of what
    /// it would cover; the follower carries on from where it was drawn. A head
    /// nothing clears (a crowded page) keeps the shipped over-ink seat rather
    /// than vanishing (a dip reads as blinking).
    /// `ink` is the seat law's probe ([`crate::rainbow_kitty::companion::cell_ink`]).
    #[inline]
    pub fn keep_off_letters<F>(&mut self, geom: EffectGeom, caret: (u16, u16), ink: F)
    where
        F: Fn(u16, u16) -> Option<bool>,
    {
        self.keep_off(geom, caret, ink, true);
    }

    /// [`Self::keep_off_letters`] for a head fitted to its row
    /// ([`crate::rainbow_kitty::companion::HeadFit`]): it stays on that row, so it is never lifted
    /// clear, and one with no room left past the caret keeps its seat beside it.
    #[inline]
    pub fn keep_fitted_off_letters<F>(&mut self, geom: EffectGeom, caret: (u16, u16), ink: F)
    where
        F: Fn(u16, u16) -> Option<bool>,
    {
        self.keep_off(geom, caret, ink, false);
    }

    #[inline]
    fn keep_off<F>(&mut self, geom: EffectGeom, caret: (u16, u16), ink: F, may_lift: bool)
    where
        F: Fn(u16, u16) -> Option<bool>,
    {
        let Some(draw) = self.drawn.as_mut() else {
            return;
        };
        let (cw, ch) = (i32::from(geom.cell_w), i32::from(geom.cell_h));
        if cw <= 0 || ch <= 0 {
            return;
        }
        let (rows, cols) = (i32::from(geom.rows), i32::from(geom.cols));
        let grid_w = cols.saturating_mul(cw);
        let (w, h) = (i32::from(draw.rect.w), i32::from(draw.rect.h));
        let gap = (WordDecorations::kitty_cursor_lead_px(geom.cell_w) / HEAD_CARET_GAP_DEN).max(1);
        #[cfg(test)]
        let ring = if HEAD_CARET_RING_OFF.with(std::cell::Cell::get) {
            0
        } else {
            gap
        };
        #[cfg(not(test))]
        let ring = gap;
        // The caret's cell and the ring of air round it.
        let caret_box = (
            i32::from(caret.1) * cw,
            (i32::from(caret.1) + 1) * cw,
            i32::from(caret.0) * ch,
            (i32::from(caret.0) + 1) * ch,
        );
        let kept = (
            caret_box.0 - ring,
            caret_box.1 + ring,
            caret_box.2 - ring,
            caret_box.3 + ring,
        );
        // The top of the highest lettered cell the head at (x, y) would
        // cover, or `None` when it covers no letter.
        let inked = |x: i32, y: i32| -> Option<i32> {
            let (r0, r1) = ((y.max(0)) / ch, ((y + h - 1).max(0)) / ch);
            let (c0, c1) = ((x.max(0)) / cw, ((x + w - 1).max(0)) / cw);
            let r_end = r1.min(rows - 1);
            let c_end = c1.min(cols - 1);
            let mut r = r0;
            while r <= r_end {
                let mut c = c0;
                while c <= c_end {
                    if ink(r as u16, c as u16) == Some(true) {
                        return Some(r * ch);
                    }
                    c += 1;
                }
                r += 1;
            }
            None
        };
        // The same, with the caret's ring's top for the caret's ring: `None`
        // when the head covers neither the ring nor a letter.
        let blocked = |x: i32, y: i32| -> Option<i32> {
            let ringed =
                (x < kept.1 && x + w > kept.0 && y < kept.3 && y + h > kept.2).then_some(kept.2);
            match (ringed, inked(x, y)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        };
        let is_clear = |nx: i32, ny: i32| -> bool {
            !(nx < kept.1 && nx + w > kept.0 && ny < kept.3 && ny + h > kept.2)
                && inked(nx, ny).is_none()
        };
        let (x, y) = (draw.rect.x, draw.rect.y);
        let ringed_xy =
            (x < kept.1 && x + w > kept.0 && y < kept.3 && y + h > kept.2).then_some(kept.2);
        let inked_xy = inked(x, y);
        let Some(blocked_xy) = (match (ringed_xy, inked_xy) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }) else {
            return;
        };
        #[cfg(test)]
        if HEAD_KEEPS_OFF_OFF.with(std::cell::Cell::get) {
            return;
        }
        // A head in the ring, off the caret's cell and the letters, steps out
        // of the ring by the least move that clears it, so a bob or a glide
        // that grazes the ring is nudged a few pixels, never thrown across
        // the caret.
        let in_cell =
            x < caret_box.1 && x + w > caret_box.0 && y < caret_box.3 && y + h > caret_box.2;
        let grid_h = rows.saturating_mul(ch);
        let nudged = if in_cell || inked_xy.is_some() {
            None
        } else {
            let candidates = [
                (x >= caret_box.1, (kept.1, y)),
                (x + w <= caret_box.0, (kept.0 - w, y)),
                (y + h <= caret_box.2, (x, kept.2 - h - 1)),
                (y >= caret_box.3, (x, kept.3)),
            ];
            let mut best_dist = i32::MAX;
            let mut best_at = None;
            let mut ci = 0;
            while ci < candidates.len() {
                let (side, (nx, ny)) = candidates[ci];
                if side && nx >= 0 && nx + w <= grid_w && ny >= 0 && ny + h <= grid_h {
                    let dist = (nx - x).abs() + (ny - y).abs();
                    if dist < best_dist && is_clear(nx, ny) {
                        best_dist = dist;
                        best_at = Some((nx, ny));
                    }
                }
                ci += 1;
            }
            best_at
        };
        // A head off the caret's ring that grazes a lettered row by a bob's
        // few pixels (no more than the ring's width) steps back off it by the
        // least vertical move. Lifted clear, it was thrown over the whole
        // lettered row for the frames the bob lay on it and fell back after,
        // a blink (composed r2's Nyan rainbow at fold rest: its head sat
        // beside the caret under a full line at `f1100` and `f1400` and over
        // the line at `f1250`, one pixel of bob into the line's row).
        #[cfg(test)]
        let graze_lifts = HEAD_GRAZE_LIFTS.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let graze_lifts = false;
        let grazed = if in_cell || ringed_xy.is_some() || graze_lifts {
            None
        } else {
            (1..=gap)
                .flat_map(|d| [(x, y + d), (x, y - d)])
                .find(|&(nx, ny)| ny >= 0 && ny + h <= grid_h && is_clear(nx, ny))
        };
        let ahead = x + w / 2 > (caret_box.0 + caret_box.1) / 2;
        let pushed = if ahead {
            let px = x.max(caret_box.1 + gap);
            (px + w <= grid_w).then_some(px)
        } else {
            let px = x.min(caret_box.0 - gap - w);
            (px >= 0).then_some(px)
        };
        let to = match (nudged.or(grazed), pushed) {
            (Some(at), _) => Some(at),
            (None, Some(px)) if is_clear(px, y) => Some((px, y)),
            _ if !may_lift => None,
            _ => {
                // Lift clear of each cell it would cover, a row at a time.
                let mut ly = blocked_xy - h - 1;
                let mut found = None;
                let mut tries = 1;
                while tries <= rows && ly >= 0 {
                    match blocked(x, ly) {
                        None => {
                            found = Some((x, ly));
                            break;
                        }
                        Some(top) => ly = top - h - 1,
                    }
                    tries += 1;
                }
                found
            }
        };
        let Some((nx, ny)) = to else {
            return;
        };
        let (dx, dy) = (nx - x, ny - y);
        draw.rect.x = nx;
        draw.rect.y = ny;
        if let Some((sx, sy)) = draw.settle.as_mut() {
            *sx += dx as f32;
            *sy += dy as f32;
        }
    }

    /// True for either visible half-beat and the off-stage seam between them.
    #[inline]
    #[must_use]
    pub const fn in_motion(self) -> bool {
        !matches!(self.leg, KittyCursorPlacementLeg::Settled)
    }
}

#[derive(Clone, Copy, Debug)]
enum KittyCursorPlacementLeg {
    Settled,
    Exit {
        direction: crate::kitty_cursor::CatFoldDirection,
        eased: f32,
        from: CatFootprint,
        from_sprite: Option<(i32, i32)>,
    },
    Enter {
        direction: crate::kitty_cursor::CatFoldDirection,
        eased: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KittyCursorPlacementKey {
    scope: Option<u64>,
    cell_w: u16,
    cell_h: u16,
    rows: u16,
    cols: u16,
    source: u64,
}

#[derive(Clone, Copy, Debug)]
struct KittyCursorActiveFold {
    started: Instant,
    direction: crate::kitty_cursor::CatFoldDirection,
    from: CatFootprint,
    from_sprite: Option<(i32, i32)>,
}

/// The flying head's flight to a far caret ([`HEAD_FLIGHT`]): when it took off and its sprite's
/// top-left as last drawn there.
#[derive(Clone, Copy, Debug)]
struct KittyHeadFlight {
    started: Instant,
    from: (i32, i32),
}

/// The height of the flying head's arc at the middle of its flight, cells
/// ([`HeadFlightSample`]): a fold's hop over the caret's line.
const HEAD_FLIGHT_ARC: f32 = 0.18;

struct KittyCursorPlacementState {
    key: Option<KittyCursorPlacementKey>,
    last_target: Option<CatFootprint>,
    last_sprite: Option<(i32, i32)>,
    last_emitted_at: Option<Instant>,
    settle: Option<(f32, f32)>,
    seen_fold: Option<(Instant, crate::kitty_cursor::CatFoldDirection)>,
    active: Option<KittyCursorActiveFold>,
    flight: Option<KittyHeadFlight>,
}

impl KittyCursorPlacementState {
    const EMPTY: Self = Self {
        key: None,
        last_target: None,
        last_sprite: None,
        last_emitted_at: None,
        settle: None,
        seen_fold: None,
        active: None,
        flight: None,
    };
}

impl Default for KittyCursorPlacementState {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// The fully-resolved per-frame decoration settings (host config → engine).
#[derive(Clone, Debug)]
pub struct DecoConfig {
    /// Master + per-category gate already folded in: matching runs at all only when at least one category is on.
    pub profanity: bool,
    pub feline: bool,
    /// Canine family → the typed-word dog cameo (the input path).
    pub canine: bool,
    /// Emphasis words (user `extra_words` only; the builtin lexicon has none), ink only.
    pub emphasis: bool,
    /// Animated glyph-ink shimmer over ink-bearing classes (emphasis + profanity + feline).
    pub ink_enabled: bool,
    /// Ink tint vs the captured original fg, `0.0..=1.0` (§4.2 `strength`).
    pub ink_strength: f32,
    /// One specular sweep window, ms; clamped `350..=6000` by the resolver (floor 600 when `ink_loop`, the §6.4 flash margin).
    pub ink_sweep_ms: u32,
    /// Re-sweep while the word stays visible (keeps focused wakes live).
    pub ink_loop: bool,
    /// Forces the static path (no twinkle / no jitter / no pulse).
    pub reduced_motion: bool,
    /// Suppress all decorations while the alternate screen is active (default off).
    pub suppress_in_alt_screen: bool,
    /// Decorate the bare 3-letter `cat` token (opt-in).
    pub allow_bare_cat: bool,
    /// Decorate a lone CJK cat ideograph (opt-in).
    pub cjk_single_char: bool,
    /// Feline graphic mode: authored cat or ink-only `paw`.
    pub feline_style: FelineStyle,
    /// `[sparkle_words.feline] magic` (§10): Fortune (1/512) / Nebula (1/1024) rare cats.
    pub feline_magic: bool,
    /// `[sparkle_words.profanity] style` (§10).
    pub profanity_style: ProfanityStyle,
    /// `[sparkle_words.profanity] magic` (§10): Quasar (1/512) / Singularity (1/1024) rare novas.
    pub profanity_magic: bool,
    /// `[sparkle_words.profanity] supernova_chance` (`0..=100`, 0 disables): the per-appearance supernova escalation chance, consulted only under [`ProfanityStyle::Rainbow`].
    pub supernova_chance: u8,
    /// The custom-word spec table (`[[sparkle_words.custom]]`).
    pub spec_table: SpecTable,
    /// Profanity sparkle palette (`0x00RRGGBB`); empty → hue-rotation.
    pub palette: Vec<u32>,
    /// Sparks emitted per profanity occurrence per frame.
    pub density: u32,
    /// How long an occurrence sparkles after appearing, milliseconds.
    pub anim_ms: u64,
    /// Sub-cell jitter magnitude in pixels.
    pub jitter: i8,
    /// Profanity sparkle opacity `0.0..=1.0`.
    pub intensity: f32,
    /// Sparkle glyph variants to cycle through.
    pub glyphs: Vec<DecoGlyph>,
    /// Folded surfaces to never decorate (`deny` + per-category `ignore_words`).
    pub ignore: HashSet<String>,
}

impl DecoConfig {
    /// Scan options derived from the config (borrows the `ignore` set).
    #[inline]
    pub fn scan_opts(&self) -> ScanOptions<'_> {
        ScanOptions {
            allow_bare_cat: self.allow_bare_cat,
            cjk_single_char: self.cjk_single_char,
            ignore: if self.ignore.is_empty() {
                None
            } else {
                Some(&self.ignore)
            },
        }
    }
}

#[inline]
const fn class_default_spec(class: Class, cfg: &DecoConfig) -> WordEffectSpec {
    match class {
        Class::Profanity => WordEffectSpec {
            graphic: None,
            ink: Some(InkSpec {
                colorway: match cfg.profanity_style {
                    ProfanityStyle::Rainbow => crate::spec::rainbow_ink().colorway,
                    _ => Colorway::TwoTone {
                        c0: 0x00FF_D447,
                        c1: 0x00FF_7CE5,
                    },
                },
                sweep_once: !cfg.ink_loop,
            }),
            burst: Some(match cfg.profanity_style {
                ProfanityStyle::Rainbow => BurstSpec {
                    kind: BurstKind::SuperNova,
                    chance_pct: if cfg.supernova_chance < 100 {
                        cfg.supernova_chance
                    } else {
                        100
                    },
                },
                ProfanityStyle::Nova => BurstSpec {
                    kind: BurstKind::Nova,
                    chance_pct: 100,
                },
                ProfanityStyle::Sparkle => BurstSpec {
                    kind: BurstKind::Sparkle,
                    chance_pct: 100,
                },
            }),
        },
        Class::Feline => WordEffectSpec {
            graphic: Some(GraphicSpec {
                collection: Collection::Cats,
                dwell_ms: (2200, 3598),
            }),
            ink: Some(InkSpec {
                colorway: Colorway::SelfGlow {
                    lift: FELINE_GLOW_LIFT,
                    amp: FELINE_GLOW_AMP,
                    window_ms: FELINE_GLOW_MS as u32,
                },
                sweep_once: true,
            }),
            burst: None,
        },
        Class::Canine => WordEffectSpec {
            graphic: Some(GraphicSpec {
                collection: Collection::Animals,
                dwell_ms: (2200, 3598),
            }),
            ink: Some(InkSpec {
                colorway: Colorway::SelfGlow {
                    lift: FELINE_GLOW_LIFT,
                    amp: FELINE_GLOW_AMP,
                    window_ms: FELINE_GLOW_MS as u32,
                },
                sweep_once: true,
            }),
            burst: None,
        },
        Class::Animal => WordEffectSpec {
            graphic: Some(GraphicSpec {
                collection: Collection::Animals,
                dwell_ms: (2200, 3598),
            }),
            ink: Some(InkSpec {
                colorway: Colorway::SelfGlow {
                    lift: FELINE_GLOW_LIFT,
                    amp: FELINE_GLOW_AMP,
                    window_ms: FELINE_GLOW_MS as u32,
                },
                sweep_once: true,
            }),
            burst: None,
        },
        Class::Emphasis => WordEffectSpec {
            graphic: None,
            ink: Some(InkSpec {
                colorway: Colorway::TwoTone {
                    c0: 0x007C_C8FF,
                    c1: 0x00C8_9AFF,
                },
                sweep_once: !cfg.ink_loop,
            }),
            burst: None,
        },
    }
}

const RAINBOW_HUE_SALT: u64 = 0x5A1A_D0FF_BEAD_5EED;
#[inline]
pub(crate) const fn rainbow_base_hue(gkey: u64) -> f32 {
    (mix(gkey ^ RAINBOW_HUE_SALT) % 360) as f32
}

impl Default for DecoConfig {
    #[inline]
    fn default() -> Self {
        DecoConfig {
            profanity: true,
            feline: true,
            canine: true,
            emphasis: true,
            ink_enabled: true,
            ink_strength: 0.75,
            ink_sweep_ms: 2200,
            ink_loop: false,
            reduced_motion: false,
            suppress_in_alt_screen: false,
            allow_bare_cat: true,
            cjk_single_char: false,
            feline_style: FelineStyle::Cat,
            feline_magic: true,
            profanity_style: ProfanityStyle::Rainbow,
            profanity_magic: true,
            supernova_chance: 30,
            spec_table: SpecTable::default(),
            palette: Vec::new(),
            density: 3,
            anim_ms: 2500,
            jitter: 2,
            intensity: 0.85,
            glyphs: vec![
                DecoGlyph::Star4,
                DecoGlyph::Star5,
                DecoGlyph::Dot,
                DecoGlyph::Plus,
            ],
            ignore: HashSet::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct Episode {
    appeared: Instant,
    last_seen: Instant,
    genome: Genome,
    cat_colors: CatColorKey,
    nova_done: bool,
    nova_start: Option<Instant>,
    logged: bool,
    seed: u64,
    form_id: FormId,
    ctx_fp: u64,
    last_row: u16,
    last_col: u16,
    continuity_tainted: bool,
    seen_seq: u64,
    peek_done: bool,
    peek_started: bool,
    burst_done: bool,
    burst_started: bool,
    sweep_done: bool,
    sweep_started: bool,
    born_done: bool,
    born_settled: bool,
    shown_as: Option<KittyShownAs>,
    phase_start: Option<Instant>,
    peek_pause: Option<Instant>,
    peek_total: u64,
    burst_roll: bool,
    burst_tier: supernova::SuperTier,
    burst_kind: Option<BurstKind>,
    bonk_pending: bool,
    ledger_stamped: bool,
}

impl Episode {
    #[inline]
    const fn fresh(now: Instant, genome: Genome, seed: u64, row: u16, seq: u64) -> Self {
        Episode {
            appeared: now,
            last_seen: now,
            genome,
            cat_colors: CatColorKey {
                accent: 0,
                background: 0,
            },
            nova_done: false,
            nova_start: None,
            logged: false,
            seed,
            form_id: FormId::UNKNOWN,
            ctx_fp: genome.gkey ^ seed,
            last_row: row,
            last_col: 0,
            continuity_tainted: false,
            seen_seq: seq,
            peek_done: false,
            peek_started: false,
            burst_done: false,
            burst_started: false,
            sweep_done: false,
            sweep_started: false,
            born_done: false,
            born_settled: false,
            shown_as: None,
            phase_start: Some(now),
            peek_pause: None,
            peek_total: 0,
            burst_roll: false,
            burst_tier: supernova::SuperTier::Nova,
            burst_kind: None,
            bonk_pending: false,
            ledger_stamped: false,
        }
    }

    #[inline]
    const fn one_shot_started(&self) -> bool {
        !self.born_settled
            && (self.peek_started
                || self.peek_done
                || self.burst_started
                || self.burst_done
                || self.nova_done
                || self.sweep_started
                || self.sweep_done
                || self.born_done)
    }

    #[inline]
    const fn inert(&self) -> bool {
        self.born_done || self.born_settled
    }
}

/// Which edge fired a curse bonk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurseCueKind {
    /// A Profanity episode born with the live caret one past the token and a committed keystroke within reach.
    Typed,
    /// Supernova ignition grant (§3.2): an on-screen curse whose burst roll escalated is detonating.
    Detonated,
}

/// One recorded curse bonk trigger (like the trail's `SoundCue` and [`KittySighting`]).
#[derive(Clone, Copy, Debug)]
pub struct CurseCue {
    pub kind: CurseCueKind,
    /// The word's viewport position; `col` is the lead column — the host maps it to stereo pan exactly like the trail's arrival column.
    pub row: u16,
    pub col: u16,
}

const MAX_CURSE_CUES: usize = 16;

/// One recorded ambient-peek landing, the positioned cue for the resident pet's bat: a [`CurseCue`]-shaped record with the sprite's dest rect.
#[derive(Clone, Copy, Debug)]
pub struct PeekCue {
    /// The word's viewport cell (row, lead column) — the [`CurseCue`] shape.
    pub row: u16,
    pub col: u16,
    /// The landed head's dest rect `(x0, x1, y0, y1)` in grid px, right/bottom exclusive (the head sits rows away from its word).
    pub head_px: (i32, i32, i32, i32),
}

const MAX_PEEK_CUES: usize = 8;

/// How long a committed edit keystroke stays a valid "a human typed here" witness, in ms.
pub(crate) const TYPED_EDIT_WITNESS_MS: u64 = 750;

/// How far from a token the recorded edit caret may sit and still count as an edit of that token, in columns.
pub(crate) const TYPED_EDIT_REACH_COLS: u16 = 2;

#[derive(Clone, Copy, Debug)]
struct TypedEdit {
    at: Instant,
    cell: Option<(u16, u16)>,
    pane: Option<u64>,
    spent: bool,
}

#[derive(Clone, Copy)]
struct PendingBirth {
    occ_idx: u16,
    ctx_fp: u64,
    live_caret_completion: bool,
    at_live_cursor: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PersistAdmission {
    Observed,
    Grace,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct AlignScore {
    stationary: u16,
    matches: u16,
    seed_matches: u16,
    context_matches: u16,
    distance: u64,
}

impl Default for AlignScore {
    #[inline]
    fn default() -> Self {
        Self::ZERO
    }
}

impl AlignScore {
    const ZERO: Self = Self {
        stationary: 0,
        matches: 0,
        seed_matches: 0,
        context_matches: 0,
        distance: 0,
    };

    #[inline]
    const fn with_edge(self, stationary: bool, tier: u8, distance: u32) -> Self {
        Self {
            stationary: self.stationary.saturating_add(stationary as u16),
            matches: self.matches.saturating_add(1),
            seed_matches: self.seed_matches.saturating_add((tier == 0) as u16),
            context_matches: self.context_matches.saturating_add((tier == 1) as u16),
            distance: self.distance.saturating_add(distance as u64),
        }
    }

    #[inline]
    const fn better_than(self, other: Self) -> bool {
        self.stationary > other.stationary
            || (self.stationary == other.stationary
                && (self.matches > other.matches
                    || (self.matches == other.matches
                        && (self.seed_matches > other.seed_matches
                            || (self.seed_matches == other.seed_matches
                                && (self.context_matches > other.context_matches
                                    || (self.context_matches == other.context_matches
                                        && self.distance < other.distance)))))))
    }
}

#[inline]
const fn alignment_edge(
    ep: &Episode,
    candidate: PendingBirth,
    occ: &Occurrence,
    same_width: bool,
    seq: u64,
    cols: u16,
) -> Option<(u8, u32)> {
    // Nonblank partial typing means a feline token was replaced, even when it
    // returns at the same position within the continuity window. Profanity
    // also needs the completed token to end at the live caret: passive
    // redraws may restore a spent episode, a retyped `fuck` must re-arm.
    if ep.continuity_tainted
        && (matches!(occ.class, Class::Feline)
            || (matches!(occ.class, Class::Profanity) && candidate.live_caret_completion))
    {
        return None;
    }
    if matches!(occ.class, Class::Feline)
        && ep.peek_done
        && candidate.at_live_cursor
        && seq.wrapping_sub(ep.seen_seq) >= 2
    {
        return None;
    }
    let recent = seq.wrapping_sub(ep.seen_seq) <= REKEY_MAX_SEQ_GAP;
    let exact_ctx_or_profanity =
        ep.ctx_fp == candidate.ctx_fp || matches!(occ.class, Class::Profanity);
    if !exact_ctx_or_profanity && !(recent && (ep.seed == occ.seed || same_width)) {
        return None;
    }
    let width = (if cols > 1 { cols } else { 1 }) as u32;
    let old_pos = (ep.last_row as u32)
        .saturating_mul(width)
        .saturating_add(ep.last_col as u32);
    let new_pos = (occ.row as u32)
        .saturating_mul(width)
        .saturating_add(occ.start_col as u32);
    let distance = old_pos.abs_diff(new_pos);
    let max_distance = width.saturating_mul(REKEY_MAX_LINEAR_ROWS);

    if recent && ep.seed == occ.seed {
        return if distance <= max_distance {
            Some((0, distance))
        } else {
            None
        };
    }
    if exact_ctx_or_profanity {
        return Some((1, distance));
    }
    if distance <= max_distance {
        Some((2, distance))
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct IgnitionReservation {
    pane: Option<u64>,
    owner: u64,
    start: Instant,
    center: (i32, i32),
}

#[derive(Clone, Copy)]
struct NovaLive {
    idx: u16,
    start: Instant,
    center_px: (i32, i32),
    r_max: f32,
    feats: NovaFeatures,
    magic: Option<NovaMagic>,
}

#[derive(Clone, Copy)]
struct SuperLive {
    idx: u16,
    start: Instant,
    center_px: (i32, i32),
    r_max: f32,
}

#[derive(Clone)]
struct Occurrence {
    row: u16,
    start_col: u16,
    end_col: u16,
    class: Class,
    langs: LangSet,
    form_id: FormId,
    seed: u64,
    ident: u64,
    appeared: Instant,
    genome: Genome,
    ink_base: usize,
    ink_cells: u16,
    ink_bg: [u8; 3],
    cat_colors: CatColorKey,
    cat_text_clear: bool,
    cat_peek_down: bool,
    dec_line: bool,
    inert: bool,
    spec: WordEffectSpec,
    custom: bool,
    species: Option<AnimalGlyphId>,
}

#[derive(Clone, Copy)]
struct CatSurface<'a> {
    cells: &'a [Vec<RenderCell>],
    geom: EffectGeom,
    feline_magic: bool,
}

#[derive(Clone, Copy, Debug)]
struct DoneMarkNode {
    key: u64,
    older: u32,
    newer: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DoneMarkMutation {
    evicted: Option<u64>,
    link_writes: u8,
}

impl DoneMarkMutation {
    const NONE: Self = Self {
        evicted: None,
        link_writes: 0,
    };
}

impl Default for DoneMarkMutation {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

struct DoneMarkLru {
    index: KeyedHashMap<u64, u32>,
    nodes: Vec<DoneMarkNode>,
    free: Vec<u32>,
    oldest: u32,
    newest: u32,
}

impl Default for DoneMarkLru {
    #[inline]
    fn default() -> Self {
        Self {
            index: KeyedHashMap::default(),
            nodes: Vec::new(),
            free: Vec::new(),
            oldest: DONE_MARK_NONE,
            newest: DONE_MARK_NONE,
        }
    }
}

impl DoneMarkLru {
    #[inline]
    fn len(&self) -> usize {
        self.index.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    #[cfg(test)]
    fn contains_key(&self, key: &u64) -> bool {
        self.index.contains_key(key)
    }

    #[inline]
    fn clear(&mut self) {
        self.index.clear();
        self.nodes.clear();
        self.free.clear();
        self.oldest = DONE_MARK_NONE;
        self.newest = DONE_MARK_NONE;
    }

    #[inline]
    const fn detach(&mut self, idx: u32) -> u8 {
        let nodes = self.nodes.as_mut_slice();
        let node = nodes[idx as usize];
        let mut writes = 0u8;
        if node.older == DONE_MARK_NONE {
            self.oldest = node.newer;
        } else {
            nodes[node.older as usize].newer = node.newer;
            writes += 1;
        }
        if node.newer == DONE_MARK_NONE {
            self.newest = node.older;
        } else {
            nodes[node.newer as usize].older = node.older;
            writes += 1;
        }
        nodes[idx as usize].older = DONE_MARK_NONE;
        nodes[idx as usize].newer = DONE_MARK_NONE;
        writes + 2
    }

    #[inline]
    const fn attach_newest(&mut self, idx: u32) -> u8 {
        let nodes = self.nodes.as_mut_slice();
        let mut writes = 2u8;
        nodes[idx as usize].older = self.newest;
        nodes[idx as usize].newer = DONE_MARK_NONE;
        if self.newest == DONE_MARK_NONE {
            self.oldest = idx;
        } else {
            nodes[self.newest as usize].newer = idx;
            writes += 1;
        }
        self.newest = idx;
        writes
    }

    #[inline]
    fn touch(&mut self, key: u64) -> bool {
        let Some(&idx) = self.index.get(&key) else {
            return false;
        };
        if idx != self.newest {
            self.detach(idx);
            self.attach_newest(idx);
        }
        true
    }

    #[inline]
    fn remove(&mut self, key: u64) -> bool {
        let Some(idx) = self.index.remove(&key) else {
            return false;
        };
        self.detach(idx);
        self.free.push(idx);
        true
    }

    #[inline]
    fn insert(&mut self, key: u64) -> DoneMarkMutation {
        self.insert_with_cap(key, DONE_MARKS_CAP)
    }

    #[inline]
    fn insert_with_cap(&mut self, key: u64, cap: usize) -> DoneMarkMutation {
        if let Some(&idx) = self.index.get(&key) {
            let link_writes = if idx == self.newest {
                0
            } else {
                self.detach(idx) + self.attach_newest(idx)
            };
            return DoneMarkMutation {
                evicted: None,
                link_writes,
            };
        }
        if cap == 0 {
            return DoneMarkMutation {
                evicted: Some(key),
                link_writes: 0,
            };
        }

        let mut mutation = DoneMarkMutation::default();
        let idx = if self.len() == cap {
            let idx = self.oldest;
            debug_assert_ne!(idx, DONE_MARK_NONE);
            let old_key = self.nodes[idx as usize].key;
            self.index.remove(&old_key);
            mutation.evicted = Some(old_key);
            mutation.link_writes += self.detach(idx);
            idx
        } else if let Some(idx) = self.free.pop() {
            idx
        } else {
            debug_assert!(self.nodes.len() < u32::MAX as usize);
            let idx = self.nodes.len() as u32;
            self.nodes.push(DoneMarkNode {
                key,
                older: DONE_MARK_NONE,
                newer: DONE_MARK_NONE,
            });
            idx
        };

        self.nodes[idx as usize] = DoneMarkNode {
            key,
            older: DONE_MARK_NONE,
            newer: DONE_MARK_NONE,
        };
        mutation.link_writes += self.attach_newest(idx);
        self.index.insert(key, idx);
        debug_assert!(self.len() <= cap);
        mutation
    }

    #[cfg(test)]
    fn keys_oldest_first(&self) -> Vec<u64> {
        let mut keys = Vec::with_capacity(self.len());
        let mut idx = self.oldest;
        while idx != DONE_MARK_NONE {
            let node = self.nodes[idx as usize];
            keys.push(node.key);
            idx = node.newer;
        }
        keys
    }

    #[cfg(test)]
    fn assert_valid(&self) {
        let keys = self.keys_oldest_first();
        assert_eq!(keys.len(), self.len());
        assert_eq!(
            keys.first().and_then(|k| self.index.get(k)).copied(),
            (self.oldest != DONE_MARK_NONE).then_some(self.oldest)
        );
        assert_eq!(
            keys.last().and_then(|k| self.index.get(k)).copied(),
            (self.newest != DONE_MARK_NONE).then_some(self.newest)
        );
        for (position, key) in keys.iter().enumerate() {
            let idx = self.index[key];
            let node = self.nodes[idx as usize];
            assert_eq!(node.key, *key);
            assert_eq!(
                node.older,
                position
                    .checked_sub(1)
                    .map_or(DONE_MARK_NONE, |p| self.index[&keys[p]])
            );
            assert_eq!(
                node.newer,
                keys.get(position + 1)
                    .map_or(DONE_MARK_NONE, |k| self.index[k])
            );
        }
    }
}

struct KittyCustom {
    source_fp: u64,
    w: u16,
    h: u16,
    rgba: Arc<[u8]>,
    cache_size: (u16, u16),
    cache_rgba: Vec<u8>,
}

struct NotePaintCache {
    host: u64,
    tile: aterm_scene::Tile,
}

impl NotePaintCache {
    #[inline]
    fn tile(
        slot: &mut Option<Self>,
        baker: &mut CatBaker,
        host: u64,
        (w, h): (u16, u16),
        paint: impl FnOnce(&mut aterm_scene::Tile),
    ) -> Option<CatTile> {
        if let Some(tile) = baker.host_peek(host) {
            return Some(tile);
        }
        if !baker.can_bake() {
            return None;
        }
        let cache = match slot {
            Some(cache) if cache.host == host => cache,
            Some(cache) => {
                cache.host = host;
                cache.tile.reset(u32::from(w), u32::from(h));
                paint(&mut cache.tile);
                cache
            }
            None => {
                let mut tile = aterm_scene::Tile::new(u32::from(w), u32::from(h));
                paint(&mut tile);
                slot.insert(Self { host, tile })
            }
        };
        baker.host_tile(host, w, h, cache.tile.pixels())
    }
}

/// Immutable host-authored source for the kitty cursor companion.
#[derive(Clone, Debug)]
pub enum KittySpriteSource {
    BuiltIn,
    Disabled,
    Custom {
        source_fp: u64,
        w: u16,
        h: u16,
        rgba: Arc<[u8]>,
    },
}

#[derive(Default)]
struct ScanMemo {
    hot: KeyedHashMap<String, Vec<Match>>,
    cold: KeyedHashMap<String, Vec<Match>>,
    spare: Vec<(String, Vec<Match>)>,
    gen_cap: usize,
    inputs_fp: u64,
    #[cfg(test)]
    bypass: bool,
    #[cfg(test)]
    hits: u64,
    #[cfg(test)]
    misses: u64,
    #[cfg(test)]
    fresh_buffers: u64,
    #[cfg(test)]
    no_recycle: bool,
}

impl ScanMemo {
    #[inline]
    fn clear(&mut self) {
        let keep = self.pool_target();
        Self::recycle(&mut self.spare, &mut self.hot, keep);
        Self::recycle(&mut self.spare, &mut self.cold, keep);
    }

    #[inline]
    const fn pool_target(&self) -> usize {
        #[cfg(test)]
        if self.no_recycle {
            return 0;
        }
        if self.gen_cap > 1 { self.gen_cap } else { 1 }
    }

    #[inline]
    fn recycle(
        spare: &mut Vec<(String, Vec<Match>)>,
        gone: &mut KeyedHashMap<String, Vec<Match>>,
        keep: usize,
    ) {
        let room = keep.saturating_sub(spare.len());
        spare.extend(gone.drain().take(room));
        debug_assert!(gone.is_empty(), "the drain must empty the generation");
    }

    #[inline]
    fn fit(&mut self, rows: usize, inputs_fp: u64) {
        if self.inputs_fp != inputs_fp {
            self.clear();
            self.inputs_fp = inputs_fp;
        }
        self.gen_cap = rows
            .saturating_mul(2)
            .clamp(SCAN_MEMO_MIN_GEN, SCAN_MEMO_MAX_GEN);
        self.spare.truncate(self.gen_cap);
    }

    #[cfg(test)]
    fn touch(&mut self, text: &str) -> bool {
        self.touch_len(text).is_some()
    }

    #[inline]
    fn touch_len(&mut self, text: &str) -> Option<usize> {
        #[cfg(test)]
        if self.bypass {
            self.misses += 1;
            return None;
        }
        if let Some(m) = self.hot.get(text) {
            #[cfg(test)]
            {
                self.hits += 1;
            }
            return Some(m.len());
        }
        if self.cold.is_empty() {
            #[cfg(test)]
            {
                self.misses += 1;
            }
            return None;
        }
        let Some((key, value)) = self.cold.remove_entry(text) else {
            #[cfg(test)]
            {
                self.misses += 1;
            }
            return None;
        };
        #[cfg(test)]
        {
            self.hits += 1;
        }
        let len = value.len();
        self.install(key, value);
        Some(len)
    }

    #[inline]
    fn matches(&self, text: &str) -> &[Match] {
        let m = self.hot.get(text);
        debug_assert!(m.is_some(), "touch_len() proved residency");
        match m {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    #[inline]
    fn remember(&mut self, text: &str, matches: &[Match]) {
        #[cfg(test)]
        if self.bypass {
            return;
        }
        #[cfg(test)]
        if self.no_recycle {
            self.fresh_buffers += 1;
            self.install(text.to_owned(), matches.to_vec());
            return;
        }
        #[cfg(test)]
        if self.spare.is_empty() {
            self.fresh_buffers += 1;
        }
        let (mut key, mut value) = self.spare.pop().unwrap_or_default();
        key.clear();
        key.push_str(text);
        value.clear();
        value.extend_from_slice(matches);
        self.install(key, value);
    }

    #[inline]
    fn install(&mut self, key: String, value: Vec<Match>) {
        if self.hot.len() >= self.gen_cap.max(1) {
            let keep = self.pool_target();
            Self::recycle(&mut self.spare, &mut self.cold, keep);
            std::mem::swap(&mut self.hot, &mut self.cold);
        }
        self.hot.insert(key, value);
    }
}

#[inline]
fn scan_opts_fp(opts: &ScanOptions<'_>) -> u64 {
    let mut fp = fnv1a_64(&[opts.allow_bare_cat as u8, opts.cjk_single_char as u8]);
    if let Some(ignore) = opts.ignore {
        let mut set = 0u64;
        for word in ignore {
            set ^= fnv1a_64(word.as_bytes());
        }
        fp ^= set;
        fp = fnv_mix_u64(fp, 0xff);
        fp ^= ignore.len() as u64;
    }
    fp
}

#[inline]
fn scan_inputs_fp(lexicon: &Lexicon, opts: &ScanOptions<'_>) -> u64 {
    let fp = fnv_mix_u64(
        scan_opts_fp(opts),
        std::ptr::from_ref(lexicon) as usize as u64,
    );
    fnv_mix_u64(fp, lexicon.spaced_form_count() as u64)
}

#[derive(Default)]
pub struct WordDecorations {
    occ: Vec<Occurrence>,
    last_epoch: u64,
    have_scanned: bool,
    cols: u16,
    frame: u64,
    active_until: Option<Instant>,
    scan_cells: Vec<RenderCell>,
    scan_chars: Vec<char>,
    scan_matches: Vec<Match>,
    scan_memo: ScanMemo,
    scan_scratch: ScanScratch,
    scan_text: String,
    scan_colmap: Vec<u16>,
    scan_row_occupied: Vec<bool>,
    pet_ink_spans: Vec<(u16, u16)>,
    persist: FxHashMap<u64, Episode>,
    seed_ordinals: FxHashMap<u64, u64>,
    votes: VoteScratch,
    ctx_tok: String,
    ctx_folded: String,
    grace_override: Option<Duration>,
    ink_base_fg: Vec<[u8; 3]>,
    ink_cols: Vec<u16>,
    cat_baker: CatBaker,
    cat_baker_ready: bool,
    animal_baker: AnimalBaker,
    pet_baker: crate::pet_baker::PetBaker,
    pet_last_tile: Option<(Arc<FxArt>, crate::pet_glyphs_gen::PetGlyphId, u64, u8, u8)>,
    pet_depart_tiles: [Option<(u32, Arc<FxArt>, u64)>; PET_DEPART_HOLDS],
    dog_baker: crate::dog_baker::DogBaker,
    dog_last_tile: Option<(Arc<FxArt>, u64, crate::dog_cameo::DogLook)>,
    /// Where the typed dog was drawn on its visit's last frame ([`Self::dog_cameo`]).
    dog_place: Option<DogPlace>,
    held_tile_metric: (u16, u16),
    kitty_custom: Option<KittyCustom>,
    kitty_disabled: bool,
    kitty_gen: u64,
    kitty_cursor_placement: KittyCursorPlacementState,
    note_cache: [Option<NotePaintCache>; 2],
    nuke_cache: [Option<NotePaintCache>; 3],
    mote_scratch: Option<aterm_scene::Tile>,
    done_marks: DoneMarkLru,
    frozen_at: Option<Instant>,
    settle_until: Option<Instant>,
    away: bool,
    spend_next_births: bool,
    rescan_seq: u64,
    pending: Vec<PendingBirth>,
    align_old: Vec<(u64, Episode)>,
    companion_claim: Option<u64>,
    caret_word: Option<(u64, (u16, u16))>,
    last_caret: Option<(u16, u16)>,
    align_decisions: Vec<u8>,
    sightings: Vec<KittySighting>,
    curse_cues: Vec<CurseCue>,
    peek_cues: Vec<PeekCue>,
    ignitions: Vec<IgnitionReservation>,
    novas: Vec<NovaLive>,
    supers: Vec<SuperLive>,
    super_until: Option<Instant>,
    burst_hint: Option<Instant>,
    birth_seq: u64,
    combo_births: Vec<Instant>,
    typed_edit: Option<TypedEdit>,
    trick_flash: TrickFlash,
    coupling: Vec<(u16, u8)>,
    dist_scratch: Vec<(u64, u16)>,
    parked: FxHashMap<u64, ParkedPane>,
    bound: Option<u64>,
    scan_session: Option<u64>,
    pane_px: (i32, i32),
    host_brackets_frames: bool,
    companion_body_drawn: bool,
}

#[derive(Default)]
struct ParkedPane {
    occ: Vec<Occurrence>,
    last_epoch: u64,
    have_scanned: bool,
    cols: u16,
    frame: u64,
    active_until: Option<Instant>,
    scan_row_occupied: Vec<bool>,
    pet_ink_spans: Vec<(u16, u16)>,
    scan_memo: ScanMemo,
    ink_base_fg: Vec<[u8; 3]>,
    ink_cols: Vec<u16>,
    persist: FxHashMap<u64, Episode>,
    seed_ordinals: FxHashMap<u64, u64>,
    done_marks: DoneMarkLru,
    frozen_at: Option<Instant>,
    settle_until: Option<Instant>,
    away: bool,
    spend_next_births: bool,
    rescan_seq: u64,
    pending: Vec<PendingBirth>,
    align_old: Vec<(u64, Episode)>,
    companion_claim: Option<u64>,
    caret_word: Option<(u64, (u16, u16))>,
    last_caret: Option<(u16, u16)>,
    burst_end_max: Option<Instant>,
    super_end_max: Option<Instant>,
}

#[inline]
const fn super_window_of(tier: supernova::SuperTier) -> Duration {
    let ms = supernova::total_ms(tier);
    Duration::from_millis(if ms > supernova::SUPER_TOTAL_MS {
        ms
    } else {
        supernova::SUPER_TOTAL_MS
    })
}

#[inline]
fn burst_mutex_end(ep: &Episode) -> Option<Instant> {
    if ep.nova_done {
        return None;
    }
    let start = ep.nova_start?;
    let win = match ep.burst_kind {
        Some(BurstKind::SuperNova) if ep.burst_roll => super_window_of(ep.burst_tier),
        Some(BurstKind::Nova) => {
            // Duration-only decode.
            Duration::from_millis(u64::from(nova_duration_ms(ep.genome.gkey)))
        }
        _ => return None,
    };
    Some(start + win)
}

impl ParkedPane {
    #[inline]
    fn fresh() -> Self {
        Self {
            away: true,
            ..Self::default()
        }
    }

    #[inline]
    fn swap(&mut self, wd: &mut WordDecorations) {
        std::mem::swap(&mut self.occ, &mut wd.occ);
        std::mem::swap(&mut self.last_epoch, &mut wd.last_epoch);
        std::mem::swap(&mut self.have_scanned, &mut wd.have_scanned);
        std::mem::swap(&mut self.cols, &mut wd.cols);
        std::mem::swap(&mut self.frame, &mut wd.frame);
        std::mem::swap(&mut self.active_until, &mut wd.active_until);
        std::mem::swap(&mut self.scan_row_occupied, &mut wd.scan_row_occupied);
        std::mem::swap(&mut self.pet_ink_spans, &mut wd.pet_ink_spans);
        std::mem::swap(&mut self.scan_memo, &mut wd.scan_memo);
        std::mem::swap(&mut self.ink_base_fg, &mut wd.ink_base_fg);
        std::mem::swap(&mut self.ink_cols, &mut wd.ink_cols);
        std::mem::swap(&mut self.persist, &mut wd.persist);
        std::mem::swap(&mut self.seed_ordinals, &mut wd.seed_ordinals);
        std::mem::swap(&mut self.done_marks, &mut wd.done_marks);
        std::mem::swap(&mut self.frozen_at, &mut wd.frozen_at);
        std::mem::swap(&mut self.settle_until, &mut wd.settle_until);
        std::mem::swap(&mut self.away, &mut wd.away);
        std::mem::swap(&mut self.spend_next_births, &mut wd.spend_next_births);
        std::mem::swap(&mut self.rescan_seq, &mut wd.rescan_seq);
        std::mem::swap(&mut self.pending, &mut wd.pending);
        std::mem::swap(&mut self.align_old, &mut wd.align_old);
        std::mem::swap(&mut self.companion_claim, &mut wd.companion_claim);
        std::mem::swap(&mut self.caret_word, &mut wd.caret_word);
        std::mem::swap(&mut self.last_caret, &mut wd.last_caret);
        let (any_end, super_end) = self.burst_summary();
        self.burst_end_max = any_end;
        self.super_end_max = super_end;
    }

    #[inline]
    fn burst_summary(&self) -> (Option<Instant>, Option<Instant>) {
        let (mut any, mut sup) = (None::<Instant>, None::<Instant>);
        for ep in self.persist.values() {
            let Some(end) = burst_mutex_end(ep) else {
                continue;
            };
            arm_until(&mut any, end);
            if matches!(ep.burst_kind, Some(BurstKind::SuperNova)) {
                arm_until(&mut sup, end);
            }
        }
        (any, sup)
    }
}

impl WordDecorations {
    #[inline]
    fn mark_persist_departure(&mut self, ident: u64, ep: &Episode) {
        if ep.one_shot_started() {
            mark_done(&mut self.done_marks, ident, ep);
        }
    }

    #[inline]
    fn rekey_persist_episode(&mut self, old_ident: u64, new_ident: u64, ep: Episode) -> bool {
        if old_ident != new_ident
            && let Some(start) = ep.nova_start
        {
            let mut remapped = 0usize;
            let mut i = 0;
            while i < self.ignitions.len() {
                let reservation = &mut self.ignitions[i];
                if reservation.owner == old_ident && reservation.start == start {
                    reservation.owner = new_ident;
                    remapped += 1;
                }
                i += 1;
            }
            debug_assert!(remapped <= 1, "one limiter slot per episode");
        }
        self.insert_persist_bounded(new_ident, ep, PersistAdmission::Observed)
    }

    #[inline]
    fn oldest_persist(&self) -> Option<(u64, Instant)> {
        let mut oldest: Option<(u64, Instant)> = None;
        for (&key, resident) in &self.persist {
            if match oldest {
                Some((best_key, best_seen)) => (resident.last_seen, key) < (best_seen, best_key),
                None => true,
            } {
                oldest = Some((key, resident.last_seen));
            }
        }
        oldest
    }

    #[inline]
    fn insert_persist_bounded(
        &mut self,
        ident: u64,
        ep: Episode,
        admission: PersistAdmission,
    ) -> bool {
        if admission == PersistAdmission::Grace {
            if self.persist.contains_key(&ident) {
                self.mark_persist_departure(ident, &ep);
                debug_assert!(self.persist.len() <= PERSIST_CAP);
                return false;
            }
        } else if let Some(displaced) = self.persist.remove(&ident) {
            self.mark_persist_departure(ident, &displaced);
        }

        while self.persist.len() > PERSIST_CAP {
            let Some((oldest_key, _)) = self.oldest_persist() else {
                break;
            };
            if let Some(evicted) = self.persist.remove(&oldest_key) {
                self.mark_persist_departure(oldest_key, &evicted);
            }
        }
        if self.persist.len() == PERSIST_CAP {
            let oldest = self.oldest_persist();
            if !matches!(
                oldest,
                Some((oldest_ident, oldest_seen))
                    if (ep.last_seen, ident) > (oldest_seen, oldest_ident)
            ) {
                self.mark_persist_departure(ident, &ep);
                debug_assert!(self.persist.len() <= PERSIST_CAP);
                return false;
            }
            if let Some((oldest_ident, _)) = oldest
                && let Some(evicted) = self.persist.remove(&oldest_ident)
            {
                self.mark_persist_departure(oldest_ident, &evicted);
            }
        }

        self.persist.insert(ident, ep);
        debug_assert!(self.persist.len() <= PERSIST_CAP);
        true
    }

    /// Transient reset (v3 §1.1 reset table: pane-space / split change).
    #[cfg(test)]
    pub fn reset(&mut self) {
        self.with_each_pane(Self::reset_bound);
    }

    #[inline]
    fn reset_bound(&mut self) {
        let marks = &mut self.done_marks;
        for (ident, ep) in &self.persist {
            if ep.one_shot_started() {
                mark_done(marks, *ident, ep);
            }
        }
        self.reset_transient_state();
    }

    #[inline]
    fn park_live_state(&mut self, key: u64) {
        let mut slot = ParkedPane::default();
        slot.swap(self);
        self.parked.insert(key, slot);
    }

    /// Bind `key`'s state into the live fields, recording the pane's grid origin in window pixels (`px_origin`) for the shared flash limiter.
    #[inline]
    pub fn bind_pane(&mut self, key: u64, px_origin: (i32, i32)) {
        self.pane_px = px_origin;
        match self.bound {
            Some(cur) if cur == key => return,
            Some(cur) => self.park_live_state(cur),
            None => {
                let flash = if self.trick_flash.found_in_a_pane() {
                    Some(std::mem::take(&mut self.trick_flash))
                } else {
                    None
                };
                self.reset_bound();
                if let Some(flash) = flash {
                    self.trick_flash = flash;
                }
            }
        }
        let mut slot = match self.parked.remove(&key) {
            Some(s) => s,
            None => ParkedPane::fresh(),
        };
        slot.swap(self);
        self.bound = Some(key);
    }

    /// Declare which session the live fields describe on a host that binds no pane (the unsplit renderer and its capture twin).
    #[inline]
    pub fn set_scan_session(&mut self, session: Option<u64>) {
        let mut live_is_declared = true;
        if let Some(cur) = self.bound
            && Some(cur) != session
        {
            self.park_live_state(cur);
            self.bound = None;
            self.pane_px = (0, 0);
            live_is_declared = false;
        }
        if live_is_declared && self.scan_session == session {
            return;
        }
        if live_is_declared {
            match self.scan_session {
                Some(prev) => self.park_live_state(prev),
                None if self.have_scanned
                    || !matches!(session, Some(s) if self.parked.contains_key(&s)) =>
                {
                    if let Some(s) = session {
                        self.parked.remove(&s);
                    }
                    self.scan_session = session;
                    self.have_scanned = false;
                    return;
                }
                None => {}
            }
        }
        self.scan_session = session;
        if let Some(next) = session {
            let mut slot = match self.parked.remove(&next) {
                Some(s) => s,
                None => ParkedPane::fresh(),
            };
            slot.swap(self);
        }
        self.have_scanned = false;
    }

    #[inline]
    const fn scan_scope(&self) -> Option<u64> {
        match self.bound {
            Some(b) => Some(b),
            None => self.scan_session,
        }
    }

    /// Declare the absolute row of the top visible line (`RenderInput::base_y`, read under the same lock as the cells) for the next rescan in the current scan scope, beside [`Self::set_scan_session`] / [`Self::bind_pane`].
    #[inline]
    pub const fn set_scan_base_y(&mut self, base_y: Option<i64>) {
        let scope = self.scan_scope();
        self.trick_flash.declare_base_y(scope, base_y);
    }

    /// Open a host frame, before any pane's [`tick`](Self::tick).
    #[inline]
    pub const fn begin_host_frame(&mut self) {
        self.host_brackets_frames = true;
        self.cat_baker_ready = false;
        self.companion_body_drawn = false;
    }

    /// Drop parked state for panes this window no longer shows, and unbind the live state if its pane is gone.
    #[inline]
    pub fn retain_panes(&mut self, keep: impl Fn(u64) -> bool) {
        self.trick_flash.retain_panes(&keep);
        self.parked.retain(|k, _| keep(*k));
        let bound_gone = matches!(self.bound, Some(cur) if !keep(cur));
        let session_gone = matches!(self.scan_session, Some(cur) if !keep(cur));
        if bound_gone {
            self.bound = None;
            self.pane_px = (0, 0);
        }
        if session_gone {
            self.scan_session = None;
        }
        if bound_gone || (self.bound.is_none() && session_gone) {
            let flash = std::mem::take(&mut self.trick_flash);
            self.reset_bound();
            self.done_marks.clear();
            self.last_caret = None;
            self.trick_flash = flash;
        }
    }

    #[inline]
    fn with_each_pane(&mut self, mut f: impl FnMut(&mut Self)) {
        if !self.parked.is_empty() {
            let mut parked = std::mem::take(&mut self.parked);
            for slot in parked.values_mut() {
                slot.swap(self);
                f(self);
                slot.swap(self);
            }
            self.parked = parked;
        }
        f(self);
    }

    /// Hard reset (v3 §1.1: master toggle, config reload / lexicon rebuild, web knob setters): clears episodes and `done_marks`, so everything replays.
    #[inline]
    pub fn hard_reset(&mut self) {
        self.with_each_pane(Self::hard_reset_one_pane);
    }

    /// Reset only Sparkle Words state, keeping the cursor companion's placement and sprite caches.
    #[inline]
    pub fn hard_reset_words(&mut self) {
        self.with_each_pane(Self::hard_reset_words_one_pane);
    }

    #[inline]
    fn hard_reset_one_pane(&mut self) {
        self.done_marks.clear();
        self.reset_transient_state();
    }

    #[inline]
    fn hard_reset_words_one_pane(&mut self) {
        self.done_marks.clear();
        self.reset_word_transient_state();
    }

    #[inline]
    fn reset_transient_state(&mut self) {
        self.reset_word_transient_state();
        self.kitty_cursor_placement = KittyCursorPlacementState::default();
        self.cat_baker.clear();
        self.pet_baker.clear();
        self.animal_baker.clear();
        self.pet_last_tile = None;
        self.pet_depart_tiles = [const { None }; PET_DEPART_HOLDS];
        self.dog_baker.clear();
        self.dog_last_tile = None;
        self.dog_place = None;
    }

    #[inline]
    fn drop_holds_if_metric_moved(&mut self, cell_w: u16, cell_h: u16) {
        if cell_w == self.held_tile_metric.0 && cell_h == self.held_tile_metric.1 {
            return;
        }
        self.held_tile_metric = (cell_w, cell_h);
        self.pet_last_tile = None;
        self.pet_depart_tiles = [const { None }; PET_DEPART_HOLDS];
        self.dog_last_tile = None;
        self.dog_place = None;
    }

    #[inline]
    fn begin_baker_frame(&mut self, geom: EffectGeom, _animals: bool) {
        self.drop_holds_if_metric_moved(geom.cell_w, geom.cell_h);
        if self.cat_baker_ready {
            return;
        }
        self.cat_baker.begin_frame(geom.cell_w, geom.cell_h);
        self.animal_baker.begin_frame(geom.cell_w, geom.cell_h);
        self.cat_baker_ready = true;
    }

    #[inline]
    fn reset_word_transient_state(&mut self) {
        self.occ.clear();
        self.have_scanned = false;
        self.last_epoch = 0;
        self.active_until = None;
        self.scan_memo.clear();
        self.scan_row_occupied.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        self.pet_ink_spans.clear();
        self.persist.clear();
        self.seed_ordinals.clear();
        self.pending.clear();
        self.align_old.clear();
        self.companion_claim = None;
        self.caret_word = None;
        self.frozen_at = None;
        self.settle_until = None;
        self.sightings.clear();
        self.curse_cues.clear();
        self.peek_cues.clear();
        self.ignitions.clear();
        self.novas.clear();
        self.coupling.clear();
        self.dist_scratch.clear();
        self.supers.clear();
        self.super_until = None;
        let mut burst_hint: Option<Instant> = None;
        for p in self.parked.values() {
            if let Some(end) = p.burst_end_max {
                burst_hint = Some(match burst_hint {
                    Some(cur) if cur >= end => cur,
                    _ => end,
                });
            }
        }
        self.burst_hint = burst_hint;
        self.combo_births.clear();
        self.trick_flash.clear();
    }

    /// Suspend the engine's clocks (perf_reduced latch, suppressed alt screen).
    #[inline]
    pub const fn freeze(&mut self, now: Instant) {
        if self.frozen_at.is_none() {
            self.frozen_at = Some(now);
        }
    }

    /// Resume after [`freeze`](Self::freeze): shifts every stored timestamp (`appeared`, `last_seen`, `nova_start`, `phase_start`, `peek_pause`, limiter slots, `active_until`, the settle deadline, the burst mutex and its hint) forward by the freeze duration, so animations resume where they paused and occluded episodes keep their remaining grace.
    #[inline]
    pub fn thaw(&mut self, now: Instant) {
        let Some(t0) = self.frozen_at.take() else {
            return;
        };
        let d = now.saturating_duration_since(t0);
        if d.is_zero() {
            return;
        }
        for ep in self.persist.values_mut() {
            ep.appeared += d;
            ep.last_seen += d;
            if let Some(s) = &mut ep.nova_start {
                *s += d;
            }
            if let Some(s) = &mut ep.phase_start {
                *s += d;
            }
            if let Some(s) = &mut ep.peek_pause {
                *s += d;
            }
        }
        let mut oi = 0;
        while oi < self.occ.len() {
            self.occ[oi].appeared += d;
            oi += 1;
        }
        let mut ii = 0;
        while ii < self.ignitions.len() {
            self.ignitions[ii].start += d;
            ii += 1;
        }
        if let Some(a) = &mut self.active_until {
            *a += d;
        }
        if let Some(s) = &mut self.settle_until {
            *s += d;
        }
        if let Some(s) = &mut self.super_until {
            *s += d;
        }
        if let Some(h) = &mut self.burst_hint {
            *h += d;
        }
    }

    #[inline]
    fn combo_level(&mut self, now: Instant, charge: bool) -> u32 {
        let window = Duration::from_millis(supernova::COMBO_WINDOW_MS);
        self.combo_births
            .retain(|t| now.saturating_duration_since(*t) < window);
        if charge {
            if self.combo_births.len() >= supernova::COMBO_CAP {
                self.combo_births.remove(0);
            }
            self.combo_births.push(now);
        }
        self.combo_births.len() as u32
    }

    /// Drain this tick's Kitty Log sightings (§F4.2), keeping capacity.
    #[inline]
    pub fn drain_kitty_sightings(&mut self) -> std::vec::Drain<'_, KittySighting> {
        self.sightings.drain(..)
    }

    /// Drain this tick's curse bonk cues, keeping capacity.
    #[inline]
    pub fn drain_curse_cues(&mut self) -> std::vec::Drain<'_, CurseCue> {
        self.curse_cues.drain(..)
    }

    /// Drain this tick's positioned ambient-peek landings (the pet's bat).
    #[inline]
    pub fn drain_peek_cues(&mut self) -> std::vec::Drain<'_, PeekCue> {
        self.peek_cues.drain(..)
    }

    /// Sync whether the host can present this window (focused and not deco-suspended).
    #[inline]
    pub const fn set_presentable(&mut self, _now: Instant, presentable: bool) {
        // Distinct branch.
        if self.away != presentable {
            return;
        }
        self.away = !presentable;
        if presentable {
            self.spend_next_births = true;
        }
    }

    /// The host's last [`Self::set_presentable`] verdict.
    #[must_use]
    #[inline]
    pub const fn presentable(&self) -> bool {
        !self.away
    }

    /// The flying cursor kitty's dest rect for `layout`, from the emitter's own sizing and placement math.
    #[inline]
    #[must_use]
    pub const fn kitty_cursor_footprint(&self, layout: KittyCursorLayout) -> Option<CatFootprint> {
        let KittyCursorLayout {
            geom,
            cursor: (crow, ccol),
            look,
            bob,
            lift,
        } = layout;
        if self.kitty_disabled || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }
        let ch = geom.cell_h as usize;
        let slot_w = 4 * ch;
        let slot_h = 2 * ch;
        let (w, h) = if let Some(cust) = &self.kitty_custom {
            let nw = if cust.w > 1 { cust.w as usize } else { 1 };
            let nh = if cust.h > 1 { cust.h as usize } else { 1 };
            let raw_max_h = ch * 9 / 5;
            let max_h = if raw_max_h < 1 {
                1
            } else if raw_max_h > slot_h {
                slot_h
            } else {
                raw_max_h
            };
            let s = (slot_w as f32 / nw as f32).min(max_h as f32 / nh as f32);
            let rw = (nw as f32 * s).round() as usize;
            let rh = (nh as f32 * s).round() as usize;
            (
                (if rw < 1 {
                    1
                } else if rw > slot_w {
                    slot_w
                } else {
                    rw
                }) as u16,
                (if rh < 1 {
                    1
                } else if rh > slot_h {
                    slot_h
                } else {
                    rh
                }) as u16,
            )
        } else {
            let look = look.normalized();
            let desired_h = Self::COMPANION_CELL_H * (geom.cell_h as f32) * look.age.scale();
            authored_cat_size(look.variant, desired_h, geom.cell_h)
        };
        let cw = geom.cell_w as i32;
        let ch_i = geom.cell_h as i32;
        let grid_w = (geom.cols as i32).saturating_mul(cw);
        let cursor_right = (ccol as i32).saturating_add(1).saturating_mul(cw);
        let lead = Self::kitty_cursor_lead_px(geom.cell_w);
        let x_cand = cursor_right.saturating_add(lead);
        let x_max = grid_w.saturating_sub(w as i32);
        let x = if x_cand < x_max { x_cand } else { x_max };
        if x < 0 {
            return None;
        }
        let rest_top = (crow as i32) * ch_i + ch_i / 2 - (h as i32) / 2;
        let intrusion = cursor_right - x;
        let rise = if intrusion > 0 {
            let r0 = if intrusion < ch_i / 2 {
                intrusion
            } else {
                ch_i / 2
            };
            let r_max = if rest_top > 0 { rest_top } else { 0 };
            if r0 < r_max { r0 } else { r_max }
        } else {
            0
        };
        // The head is taller than its row, so on row 0 (and the row-1 sky seat) the centred seat
        // starts above the grid, where a pane divider cuts it and the window pad or the tab strip
        // hides it: the seat stops at the grid top, less the hover bob's reach.
        let headroom = (crate::kitty_cursor::BOB_AMP * ch_i as f32).ceil() as i32;
        let seated_raw = rest_top - rise - (lift * ch_i as f32).round() as i32;
        let seated = if seated_raw > headroom {
            seated_raw
        } else {
            headroom
        };
        let y_raw = seated + (bob * ch_i as f32).round() as i32;
        let y = if y_raw > 0 { y_raw } else { 0 };
        Some(CatFootprint { x, y, w, h })
    }

    /// Resolve the cursor kitty's logical target and line-fold placement once for this frame.
    #[inline]
    #[must_use]
    pub fn resolve_kitty_cursor_placement(
        &mut self,
        layout: KittyCursorLayout,
        motion: crate::kitty_cursor::CursorCatPlacementFrame,
    ) -> Option<KittyCursorPlacement> {
        let Some(target) = self.kitty_cursor_footprint(layout) else {
            self.kitty_cursor_placement = KittyCursorPlacementState::default();
            return None;
        };
        let key = KittyCursorPlacementKey {
            scope: self.scan_scope(),
            cell_w: layout.geom.cell_w,
            cell_h: layout.geom.cell_h,
            rows: layout.geom.rows,
            cols: layout.geom.cols,
            source: self.kitty_gen,
        };
        let stale = match self.kitty_cursor_placement.last_emitted_at {
            Some(at) => {
                motion.sampled_at.saturating_duration_since(at)
                    >= crate::kitty_cursor::CURSOR_FOLD_DURATION
            }
            None => true,
        };
        let settled = |flight: Option<HeadFlightSample>| KittyCursorPlacement {
            sample: target,
            leg: KittyCursorPlacementLeg::Settled,
            facing_left: motion.facing_left,
            sampled_at: Some(motion.sampled_at),
            drawn: None,
            flight,
        };
        if self.kitty_cursor_placement.key != Some(key) || stale {
            self.kitty_cursor_placement.key = Some(key);
            self.kitty_cursor_placement.last_target = Some(target);
            self.kitty_cursor_placement.last_sprite = None;
            self.kitty_cursor_placement.settle = None;
            self.kitty_cursor_placement.active = None;
            self.kitty_cursor_placement.flight = None;
            self.kitty_cursor_placement.seen_fold =
                motion.fold.map(|fold| (fold.started, fold.direction));
            return Some(settled(None));
        }

        let Some(fold) = motion.fold else {
            self.kitty_cursor_placement.active = None;
            // A rest more than two rows from the last one (a screen cleared, a prompt drawn
            // far down) is flown to from where the head was drawn: the follower's glide
            // spans two rows, and snapped past them the head blinked across the screen.
            let far = self
                .kitty_cursor_placement
                .last_target
                .is_some_and(|last| (target.y - last.y).abs() > 2 * i32::from(layout.geom.cell_h));
            if far && self.kitty_cursor_placement.flight.is_none() {
                #[cfg(test)]
                let off = HEAD_FLIGHT_OFF.with(std::cell::Cell::get);
                #[cfg(not(test))]
                let off = false;
                if !off {
                    self.launch_kitty_cursor_flight(motion.sampled_at);
                }
            }
            self.kitty_cursor_placement.last_target = Some(target);
            let flight = self.kitty_cursor_flight_at(motion.sampled_at, layout.geom.cell_h);
            return Some(settled(flight));
        };
        // A line fold flies the head off one edge and in at the other: its own flight.
        self.kitty_cursor_placement.flight = None;

        let fold_id = (fold.started, fold.direction);
        if self.kitty_cursor_placement.seen_fold != Some(fold_id) {
            self.kitty_cursor_placement.seen_fold = Some(fold_id);
            self.kitty_cursor_placement.active = Some(KittyCursorActiveFold {
                started: fold.started,
                direction: fold.direction,
                from: match self.kitty_cursor_placement.last_target {
                    Some(last) => last,
                    None => target,
                },
                from_sprite: self.kitty_cursor_placement.last_sprite,
            });
        }
        self.kitty_cursor_placement.last_target = Some(target);

        let active = match self.kitty_cursor_placement.active {
            Some(active)
                if active.started == fold.started && active.direction == fold.direction =>
            {
                active
            }
            _ => {
                return Some(settled(None));
            }
        };
        let progress = if fold.progress.is_finite() {
            fold.progress.clamp(0.0, 1.0)
        } else {
            self.kitty_cursor_placement.active = None;
            return Some(settled(None));
        };
        let leg = if progress < 0.5 {
            KittyCursorPlacementLeg::Exit {
                direction: fold.direction,
                eased: smoothstep(progress * 2.0),
                from: active.from,
                from_sprite: active.from_sprite,
            }
        } else {
            KittyCursorPlacementLeg::Enter {
                direction: fold.direction,
                eased: smoothstep((progress - 0.5) * 2.0),
            }
        };
        Some(KittyCursorPlacement {
            leg,
            ..settled(None)
        })
    }

    /// The caret went far (a meteor's jump, [`HEAD_FLIGHT`]): the flying head flies there from
    /// where it was last drawn, starting at `now`, instead of being drawn at the landing on the
    /// jump's frame. A head not on glass a moment ago (nothing drawn within its fold's time) has
    /// nowhere to fly from, and is placed afresh where its fade brings it in. Whether it flies.
    #[inline]
    pub fn launch_kitty_cursor_flight(&mut self, now: Instant) -> bool {
        #[cfg(test)]
        if HEAD_FLIGHT_OFF.with(std::cell::Cell::get) {
            self.rebase_kitty_cursor_placement();
            return false;
        }
        let state = &self.kitty_cursor_placement;
        let from = match (state.last_sprite, state.last_emitted_at) {
            (Some(from), Some(at))
                if now.saturating_duration_since(at)
                    < crate::kitty_cursor::CURSOR_FOLD_DURATION =>
            {
                from
            }
            _ => {
                self.rebase_kitty_cursor_placement();
                return false;
            }
        };
        let state = &mut self.kitty_cursor_placement;
        state.flight = Some(KittyHeadFlight { started: now, from });
        state.settle = None;
        state.active = None;
        true
    }

    /// The head's flight at `now` ([`Self::launch_kitty_cursor_flight`]), ended once it has
    /// landed.
    #[inline]
    fn kitty_cursor_flight_at(&mut self, now: Instant, cell_h: u16) -> Option<HeadFlightSample> {
        let flight = self.kitty_cursor_placement.flight?;
        let progress =
            now.saturating_duration_since(flight.started).as_secs_f32() / HEAD_FLIGHT.as_secs_f32();
        if progress >= 1.0 {
            self.kitty_cursor_placement.flight = None;
            return None;
        }
        Some(HeadFlightSample {
            from: flight.from,
            progress,
            arc_px: (HEAD_FLIGHT_ARC * f32::from(cell_h)).round() as i32,
        })
    }

    /// Re-anchor the cursor kitty after the host detects a coordinate-space tear between effect sampling and the presented frame.
    #[inline]
    pub const fn rebase_kitty_cursor_placement(&mut self) {
        self.kitty_cursor_placement = KittyCursorPlacementState::EMPTY;
    }

    const COMPANION_CELL_H: f32 = 1.70;

    const KITTY_LEAD_NUM: i32 = 3;
    const KITTY_LEAD_DEN: i32 = 4;

    /// The flying head's lead in px for a `cell_w` cell — the one number the footprint and the companion router's seat law both read.
    #[must_use]
    #[inline]
    pub const fn kitty_cursor_lead_px(cell_w: u16) -> i32 {
        (cell_w as i32).saturating_mul(Self::KITTY_LEAD_NUM) / Self::KITTY_LEAD_DEN
    }

    const PLACE_TAU: f32 = 0.08;

    /// Emit the cat flying in front of the cursor, with the `rainbow kitty` ribbon as its wake.
    #[inline]
    pub fn kitty_cursor(
        &mut self,
        frame: KittyCursorFrame,
        free: &mut Vec<FreeSprite>,
    ) -> Option<u64> {
        if frame.alpha == 0 || frame.geom.cell_w == 0 || frame.geom.cell_h == 0 {
            return None;
        }
        let footprint = self.kitty_cursor_footprint(KittyCursorLayout {
            geom: frame.geom,
            cursor: frame.cursor,
            look: frame.look,
            bob: frame.bob,
            lift: frame.lift,
        })?;
        self.kitty_cursor_at_placement(frame, KittyCursorPlacement::settled(footprint), free)
    }

    /// Resolve the body's exact draw rectangle before the word pass, without advancing its follower.
    #[inline]
    #[must_use]
    pub fn prepare_kitty_cursor_placement(
        &mut self,
        layout: KittyCursorLayout,
        motion: crate::kitty_cursor::CursorCatPlacementFrame,
        pose: crate::kitty_cursor::CatPose,
    ) -> Option<KittyCursorPlacement> {
        let mut placement = self.resolve_kitty_cursor_placement(layout, motion)?;
        placement.drawn =
            Some(self.kitty_cursor_draw_geometry(layout.geom, layout.bob, pose, placement));
        Some(placement)
    }

    #[inline]
    fn kitty_cursor_draw_geometry(
        &self,
        geom: EffectGeom,
        bob: f32,
        pose: crate::kitty_cursor::CatPose,
        placement: KittyCursorPlacement,
    ) -> PreparedKittyCursorDraw {
        let footprint = placement.sample;
        let (nat_w, nat_h) = (footprint.w, footprint.h);
        let mut settle = self.kitty_cursor_placement.settle;
        let grid_w = i32::from(geom.cols).saturating_mul(i32::from(geom.cell_w));
        let dest_w = ((f32::from(nat_w) * pose.scale_x).round() as i32)
            .clamp(1, grid_w.max(1).min(i32::from(u16::MAX))) as u16;
        let dest_h =
            ((f32::from(nat_h) * pose.scale_y).round() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let flip_x = placement.facing_left;
        let lead_px =
            (pose.lead * f32::from(geom.cell_w)).round() as i32 * if flip_x { -1 } else { 1 };
        let cx = footprint.x + i32::from(nat_w) / 2;
        let cy = footprint.y + i32::from(nat_h) / 2;
        let base_x = (footprint.x + lead_px).clamp(0, grid_w - i32::from(nat_w));
        let target_x = (cx - i32::from(dest_w) / 2 + lead_px).clamp(0, grid_w - i32::from(dest_w));
        let target_y = cy - i32::from(dest_h) / 2;
        let scale_dx = target_x - base_x;
        let scale_dy = target_y - footprint.y;
        let clearance = i32::from(dest_w)
            .max(i32::from(nat_w))
            .saturating_add(i32::from(geom.cell_w).saturating_mul(2));
        #[inline]
        const fn lift(cell_h: u16, q: f32) -> i32 {
            (0.18 * (cell_h as f32) * 4.0 * q * (1.0 - q)).round() as i32
        }
        #[inline]
        const fn lerp_px(a: i32, b: i32, q: f32) -> i32 {
            ((a as f64) + ((b as f64) - (a as f64)) * (q as f64))
                .round()
                .clamp(i32::MIN as f64, i32::MAX as f64) as i32
        }
        let (sprite_x, sprite_y) = match placement.leg {
            // The head flies to the landing ([`HeadFlightSample`]): the follower rests there.
            KittyCursorPlacementLeg::Settled if placement.flight.is_some() => {
                let ch = f32::from(geom.cell_h);
                let bob_px = (bob * ch).round() as i32;
                settle = Some((base_x as f32, (footprint.y - bob_px) as f32));
                (target_x, target_y)
            }
            KittyCursorPlacementLeg::Settled => match placement.sampled_at {
                None => (target_x, target_y),
                Some(at) => {
                    let ch = f32::from(geom.cell_h);
                    let bob_px = (bob * ch).round() as i32;
                    let rest = (base_x as f32, (footprint.y - bob_px) as f32);
                    let prev_at = self.kitty_cursor_placement.last_emitted_at;
                    let fresh = match prev_at {
                        Some(prev) => {
                            at.saturating_duration_since(prev)
                                < crate::kitty_cursor::CURSOR_FOLD_DURATION
                        }
                        None => false,
                    };
                    let followed_cand = if let Some(s) = settle {
                        Some(s)
                    } else if fresh && let Some((sx, sy)) = self.kitty_cursor_placement.last_sprite
                    {
                        Some(((sx - scale_dx) as f32, (sy - bob_px - scale_dy) as f32))
                    } else {
                        None
                    };
                    let followed = match followed_cand {
                        Some(p) if fresh && (rest.1 - p.1).abs() <= 2.0 * ch => Some(p),
                        _ => None,
                    };
                    match followed {
                        Some((mut px, mut py)) => {
                            let dt = match prev_at {
                                Some(prev) => {
                                    at.saturating_duration_since(prev).as_secs_f32().min(0.1)
                                }
                                None => 0.0,
                            };
                            if dt > 0.0 && (px != rest.0 || py != rest.1) {
                                const INV_PLACE_TAU: f32 = 1.0 / WordDecorations::PLACE_TAU;
                                let alpha = 1.0 - crate::color_math::exp_f32(-dt * INV_PLACE_TAU);
                                px += (rest.0 - px) * alpha;
                                py += (rest.1 - py) * alpha;
                            }
                            settle = Some((px, py));
                            (
                                px.round() as i32 + scale_dx,
                                py.round() as i32 + bob_px + scale_dy,
                            )
                        }
                        None => {
                            settle = Some(rest);
                            (target_x, target_y)
                        }
                    }
                }
            },
            KittyCursorPlacementLeg::Exit {
                direction,
                eased,
                from,
                from_sprite,
            } => {
                settle = None;
                let from_cx = from.x + i32::from(from.w) / 2;
                let from_x = (from_cx - i32::from(dest_w) / 2 + lead_px)
                    .clamp(0, grid_w - i32::from(dest_w));
                let from_y = from.y + i32::from(from.h) / 2 - i32::from(dest_h) / 2;
                let (start_x, start_y) = match from_sprite {
                    Some(pos) => pos,
                    None => (from_x, from_y),
                };
                let end_x = match direction {
                    crate::kitty_cursor::CatFoldDirection::Forward => {
                        grid_w.saturating_add(clearance)
                    }
                    crate::kitty_cursor::CatFoldDirection::Reverse => {
                        -i32::from(dest_w) - clearance
                    }
                };
                (
                    lerp_px(start_x, end_x, eased),
                    lerp_px(start_y, from_y, eased).saturating_sub(lift(geom.cell_h, eased)),
                )
            }
            KittyCursorPlacementLeg::Enter { direction, eased } => {
                settle = None;
                let start_x = match direction {
                    crate::kitty_cursor::CatFoldDirection::Forward => {
                        -i32::from(dest_w) - clearance
                    }
                    crate::kitty_cursor::CatFoldDirection::Reverse => {
                        grid_w.saturating_add(clearance)
                    }
                };
                (
                    lerp_px(start_x, target_x, eased),
                    target_y.saturating_sub(lift(geom.cell_h, eased)),
                )
            }
        };
        PreparedKittyCursorDraw {
            rect: CatFootprint {
                x: sprite_x,
                // A stretch about the centre and a fold's arc may not lift it above the grid either.
                y: sprite_y.max(0),
                w: dest_w,
                h: dest_h,
            },
            settle,
            flight: placement.flight,
        }
    }

    /// Emit one cursor kitty at the placement from [`Self::resolve_kitty_cursor_placement`].
    #[inline]
    pub fn kitty_cursor_at_placement(
        &mut self,
        frame: KittyCursorFrame,
        placement: KittyCursorPlacement,
        free: &mut Vec<FreeSprite>,
    ) -> Option<u64> {
        let KittyCursorFrame {
            geom,
            cursor,
            look,
            colors,
            bob,
            lift: seat_lift,
            alpha,
            pose,
            sing,
            notes,
        } = frame;
        if alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }
        if !self.claim_companion_body() {
            return None;
        }
        let footprint = placement.sample;
        debug_assert_eq!(
            Some(footprint),
            self.kitty_cursor_footprint(KittyCursorLayout {
                geom,
                cursor,
                look,
                bob,
                lift: seat_lift,
            }),
            "cursor-kitty placement must match the frame it is emitted with"
        );
        self.begin_baker_frame(geom, true);

        let (w, h) = (footprint.w, footprint.h);
        let (art, w, h) = if let Some(cust) = self.kitty_custom.as_mut() {
            let host = crate::kitty_cursor::HOST_ID ^ self.kitty_gen;
            let tile = if let Some(tile) = self.cat_baker.host_peek(host) {
                tile
            } else {
                if !self.cat_baker.can_bake() {
                    return None;
                }
                let src: &[u8] = if (cust.w, cust.h) == (w, h) {
                    &cust.rgba
                } else {
                    if cust.cache_size != (w, h) {
                        let (nw, nh) = (usize::from(cust.w).max(1), usize::from(cust.h).max(1));
                        crate::kitty_cursor::resample_nearest_into(
                            &cust.rgba,
                            nw,
                            nh,
                            usize::from(w),
                            usize::from(h),
                            &mut cust.cache_rgba,
                        );
                        cust.cache_size = (w, h);
                    }
                    &cust.cache_rgba
                };
                self.cat_baker.host_tile(host, w, h, src)?
            };
            (tile.art, w, h)
        } else {
            let look = look.normalized();
            let variant = look.variant;
            let key = BakeKeyV4 {
                variant,
                accessory: look.accessory,
                coat: look.coat,
                iris: look.iris,
                colors,
                w,
                h,
                eyes: pose.eyes,
            };
            let mut tile = self.cat_baker.get_v4(&key);
            if tile.is_none() {
                const FALLBACK_EYES: [EyesFrame; 3] =
                    [EyesFrame::Open, EyesFrame::Happy, EyesFrame::Blink];
                let mut ei = 0;
                while ei < FALLBACK_EYES.len() {
                    let eyes = FALLBACK_EYES[ei];
                    if eyes != key.eyes {
                        tile = self.cat_baker.peek_v4(&BakeKeyV4 { eyes, ..key });
                        if tile.is_some() {
                            break;
                        }
                    }
                    ei += 1;
                }
            }
            let Some(tile) = tile else {
                // Cached tile not yet baked.
                return None;
            };
            (tile.art, w, h)
        };
        let (nat_w, nat_h) = (w, h);
        let flip_x = placement.facing_left;
        let draw = match placement.drawn {
            Some(d) => d,
            None => self.kitty_cursor_draw_geometry(geom, bob, pose, placement),
        };
        let CatFootprint {
            x: sprite_x,
            y: sprite_y,
            w: dest_w,
            h: dest_h,
        } = draw.shown();
        self.kitty_cursor_placement.settle = draw.settle;
        debug_assert_eq!(
            (art.w(), art.h()),
            (nat_w, nat_h),
            "the head tile is its design box"
        );
        let sprite = FreeSprite {
            x: sprite_x,
            y: sprite_y,
            w: dest_w,
            h: dest_h,
            art,
            clip: PxRect::EVERYWHERE,
            tint: 0x00FF_FFFF,
            alpha,
            flip_x,
            z: FreeZ::OverText,
        };

        if let Some(at) = placement.sampled_at {
            self.kitty_cursor_placement.last_sprite = Some((sprite.x, sprite.y));
            self.kitty_cursor_placement.last_emitted_at = Some(at);
        }

        let mut fp = fold_free(FNV64_OFFSET, &sprite);
        if sing > 0.0 {
            let grid_w = i32::from(geom.cols).saturating_mul(i32::from(geom.cell_w));
            let mouth_x = if flip_x {
                sprite.x
            } else {
                sprite.x + i32::from(sprite.w)
            };
            let mouth_y = sprite.y + i32::from(sprite.h) / 2;
            const INV_255: f32 = 1.0 / 255.0;
            let note_dir = if flip_x { -1.0 } else { 1.0 };
            let alpha_scale = f32::from(alpha) * INV_255 * sing;
            let cw_f = f32::from(geom.cell_w);
            let ch_f = f32::from(geom.cell_h);
            let mut ni = 0;
            while ni < notes.len() {
                let Some(note) = &notes[ni] else {
                    ni += 1;
                    continue;
                };
                ni += 1;
                let a = (f32::from(note.alpha) * alpha_scale) as u8;
                if a == 0 {
                    continue;
                }
                let (nw, nh) = crate::kitty_sing::note_nat_size(note.kind, geom.cell_h);
                if i32::from(nw) > grid_w {
                    continue;
                }
                let kind_idx = match note.kind {
                    crate::kitty_sing::NoteKind::Eighth => 0usize,
                    crate::kitty_sing::NoteKind::Beamed => 1,
                };
                let host = crate::kitty_sing::note_host_id(note.kind, nw, nh);
                let Some(tile) = NotePaintCache::tile(
                    &mut self.note_cache[kind_idx],
                    &mut self.cat_baker,
                    host,
                    (nw, nh),
                    |tile| crate::kitty_sing::paint_note(tile, note.kind),
                ) else {
                    continue;
                };
                let nx = mouth_x + (note_dir * note.dx * cw_f).round() as i32 - i32::from(nw) / 2;
                let ny = mouth_y + (note.dy * ch_f).round() as i32 - i32::from(nh) / 2;
                let note_sprite = FreeSprite {
                    x: if placement.in_motion() {
                        nx
                    } else {
                        nx.clamp(0, grid_w - i32::from(nw))
                    },
                    y: ny.max(0),
                    w: nw,
                    h: nh,
                    art: tile.art,
                    clip: PxRect::EVERYWHERE,
                    tint: note.tint,
                    alpha: a,
                    flip_x: false,
                    z: FreeZ::OverText,
                };
                fp = fold_free(fp, &note_sprite);
                free.push(note_sprite);
            }
        }
        free.push(sprite);
        fp = fnv_mix_u64(fp, self.cat_baker.version());
        fp = fnv_mix_u64(fp, u64::from(colors.accent));
        fp = fnv_mix_u64(fp, u64::from(colors.background));
        Some(fp)
    }

    #[inline]
    const fn claim_companion_body(&mut self) -> bool {
        if !self.host_brackets_frames {
            return true;
        }
        if self.companion_body_drawn {
            return false;
        }
        self.companion_body_drawn = true;
        true
    }

    /// Emit the full-body pet ([`crate::kitty_pet`]) as one free sprite, at the brain's fractional column with its feet on `(row + 1)·cell_h` (lifted by the arc; squash and stretch about the feet), plus its motes and the breed handoff's departing bodies, each in its own look and held tile.
    #[inline]
    pub fn pet_cursor(&mut self, frame: PetCursorFrame, free: &mut Vec<FreeSprite>) -> Option<u64> {
        let PetCursorFrame {
            geom,
            colors,
            coat,
            iris,
            pet,
            page,
        } = frame;
        if pet.lane_alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }
        if !self.claim_companion_body() {
            return None;
        }
        self.begin_baker_frame(geom, true);
        self.pet_baker.begin_frame(geom.cell_w, geom.cell_h);

        let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(geom.cell_h)).round();
        let nat_w = (nat_h * crate::kitty_pet::ART_ASPECT).round();
        let nat_h = (nat_h as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let nat_w = (nat_w as i32).clamp(1, i32::from(u16::MAX)) as u16;

        let sprite = if let Some((x0, x1, y0, y1)) =
            pet.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows)
        {
            let key = crate::pet_baker::PetBakeKey {
                pose: pet.pose,
                coat,
                iris,
                colors,
                w: nat_w,
                h: nat_h,
            };
            let host = key.host_id();
            let resolved_tile = match self.pet_baker.tile(&key) {
                Some(rgba) => self.cat_baker.host_tile(host, nat_w, nat_h, rgba),
                None => self.cat_baker.host_peek(host),
            };
            let resolved_art = match resolved_tile {
                Some(t) => {
                    let art = t.art;
                    let already_held = match &self.pet_last_tile {
                        Some((held_art, pose, held_host, held_coat, held_iris)) => {
                            Arc::ptr_eq(held_art, &art)
                                && (*pose, *held_host, *held_coat, *held_iris)
                                    == (key.pose, host, coat, iris)
                        }
                        None => false,
                    };
                    if !already_held {
                        self.pet_last_tile = Some((Arc::clone(&art), key.pose, host, coat, iris));
                    }
                    Some(art)
                }
                None => match &self.pet_last_tile {
                    Some((art, _, _, held_coat, held_iris))
                        if (*held_coat, *held_iris) == (coat, iris) =>
                    {
                        Some(Arc::clone(art))
                    }
                    _ => None,
                },
            };
            resolved_art.map(|art| FreeSprite {
                x: x0,
                y: y0,
                w: (x1 - x0).clamp(1, i32::from(u16::MAX)) as u16,
                h: (y1 - y0).clamp(1, i32::from(u16::MAX)) as u16,
                art,
                clip: PxRect::EVERYWHERE,
                tint: 0x00FF_FFFF,
                alpha: pet.alpha,
                flip_x: pet.facing_left,
                z: if pet.under_ink {
                    FreeZ::UnderText
                } else {
                    FreeZ::OverText
                },
            })
        } else {
            None
        };

        let mut fp = FNV64_OFFSET;
        if let Some(s) = &sprite {
            fp = fold_free(fp, s);
        }
        let mut dep_insert = free.len();
        if pet.alpha > 0 {
            const INV_255: f32 = 1.0 / 255.0;
            let pet_alpha_scale = f32::from(pet.alpha) * INV_255;
            let cell_dims = (geom.cell_w, geom.cell_h);
            let grid_dims = (geom.cols, geom.rows);
            let mut page_lum = None;
            let mut side = 0u16;
            let mut mi = 0;
            while mi < pet.motes.len() {
                let Some(m) = &pet.motes[mi] else {
                    mi += 1;
                    continue;
                };
                mi += 1;
                let alpha = (f32::from(m.alpha) * pet_alpha_scale) as u8;
                if alpha == 0 {
                    continue;
                }
                let Some((x, y, dest)) =
                    crate::kitty_pet::mote_box_px((m.col, m.row), m.scale, cell_dims, grid_dims)
                else {
                    continue;
                };
                if side == 0 {
                    side = crate::kitty_pet::mote_tile_side(geom.cell_h);
                }
                let bucket = Self::pet_mote_rot_bucket(m.rot);
                let host = Self::pet_mote_host_id(m.kind, bucket, side);
                let tile = if let Some(t) = self.cat_baker.host_peek(host) {
                    Some(t)
                } else if self.cat_baker.can_bake() {
                    let side_u32 = u32::from(side);
                    let paint = match &mut self.mote_scratch {
                        Some(tile) => {
                            tile.reset(side_u32, side_u32);
                            tile
                        }
                        slot @ None => slot.insert(aterm_scene::Tile::new(side_u32, side_u32)),
                    };
                    Self::paint_pet_mote(paint, m.kind, bucket, side);
                    self.cat_baker.host_tile(host, side, side, paint.pixels())
                } else {
                    None
                };
                let Some(tile) = tile else {
                    continue;
                };
                let raw_tint = match m.rgb {
                    Some(rgb) => rgb,
                    None => Self::pet_mote_tint(m.kind, colors),
                };
                let tint = if matches!(
                    m.kind,
                    crate::kitty_pet::PetMoteKind::Note | crate::kitty_pet::PetMoteKind::Heart
                ) && let Some(bg) = page
                {
                    let lum = *page_lum.get_or_insert_with(|| {
                        let table = crate::color_math::relative_luminance_table();
                        (table, crate::color_math::relative_luminance_with(table, bg))
                    });
                    Self::pet_mote_on_page(m.kind, raw_tint, Some(lum))
                } else {
                    raw_tint
                };
                let mote_sprite = FreeSprite {
                    x,
                    y,
                    w: dest,
                    h: dest,
                    art: tile.art,
                    clip: PxRect::EVERYWHERE,
                    tint,
                    alpha,
                    flip_x: false,
                    z: FreeZ::OverText,
                };
                fp = fold_free(fp, &mote_sprite);
                free.push(mote_sprite);
            }
        }
        let mut di = 0;
        while di < pet.departures.len() {
            if let Some(d) = pet.departures[di]
                && !d.own_glass
                && let Some(sp) = self.pet_departure_sprite(d, &pet, geom, colors, (nat_w, nat_h))
            {
                fp = fold_free(fp, &sp);
                if dep_insert == free.len() {
                    free.push(sp);
                } else {
                    free.insert(dep_insert, sp);
                }
                dep_insert += 1;
            }
            di += 1;
        }
        if let Some(s) = sprite {
            free.push(s);
        }
        fp = fnv_mix_u64(fp, self.cat_baker.version());
        fp = fnv_mix_u64(fp, self.pet_baker.version());
        fp = fnv_mix_u64(fp, u64::from(colors.accent));
        fp = fnv_mix_u64(fp, u64::from(colors.background));
        Some(fp)
    }

    #[inline]
    fn hold_departure_tile(
        &mut self,
        id: u32,
        (art, host): (&Arc<FxArt>, u64),
        live: &[Option<crate::kitty_pet::PetDepartureSprite>],
    ) {
        #[inline]
        const fn departure_owned(
            held: u32,
            live: &[Option<crate::kitty_pet::PetDepartureSprite>],
        ) -> bool {
            let mut i = 0;
            while i < live.len() {
                if let Some(d) = &live[i]
                    && d.id == held
                {
                    return true;
                }
                i += 1;
            }
            false
        }
        let mut free_idx = None;
        let mut i = 0;
        while i < self.pet_depart_tiles.len() {
            let slot = &mut self.pet_depart_tiles[i];
            match slot.as_ref() {
                Some((held, held_art, held_host)) if *held == id => {
                    if *held_host != host || !Arc::ptr_eq(held_art, art) {
                        *slot = Some((id, Arc::clone(art), host));
                    }
                    return;
                }
                Some((held, ..)) if free_idx.is_none() && !departure_owned(*held, live) => {
                    free_idx = Some(i);
                }
                None if free_idx.is_none() => {
                    free_idx = Some(i);
                }
                _ => {}
            }
            i += 1;
        }
        if let Some(i) = free_idx {
            self.pet_depart_tiles[i] = Some((id, Arc::clone(art), host));
        }
    }

    #[inline]
    fn pet_departure_sprite(
        &mut self,
        d: crate::kitty_pet::PetDepartureSprite,
        pet: &crate::kitty_pet::PetFrame,
        geom: EffectGeom,
        colors: CatColorKey,
        (nat_w, nat_h): (u16, u16),
    ) -> Option<FreeSprite> {
        let alpha = (u16::from(d.alpha) * u16::from(pet.lane_alpha) / 255) as u8;
        if alpha == 0 {
            return None;
        }
        let (x0, x1, y0, y1) = d.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows)?;
        let key = crate::pet_baker::PetBakeKey {
            pose: d.pose,
            coat: d.coat,
            iris: d.iris,
            colors: match d.colors {
                Some(c) => c,
                None => colors,
            },
            w: nat_w,
            h: nat_h,
        };
        let host = key.host_id();
        let resolved_tile = match self.pet_baker.tile(&key) {
            Some(rgba) => self.cat_baker.host_tile(host, nat_w, nat_h, rgba),
            None => self.cat_baker.host_peek(host),
        };
        let art = match resolved_tile {
            Some(t) => {
                let art = t.art;
                self.hold_departure_tile(d.id, (&art, host), &pet.departures);
                art
            }
            None => {
                let mut held_art = None;
                let mut i = 0;
                while i < self.pet_depart_tiles.len() {
                    if let Some((id, art, _)) = &self.pet_depart_tiles[i]
                        && *id == d.id
                    {
                        held_art = Some(Arc::clone(art));
                        break;
                    }
                    i += 1;
                }
                held_art?
            }
        };
        Some(FreeSprite {
            x: x0,
            y: y0,
            w: (x1 - x0).clamp(1, i32::from(u16::MAX)) as u16,
            h: (y1 - y0).clamp(1, i32::from(u16::MAX)) as u16,
            art,
            clip: PxRect::EVERYWHERE,
            tint: 0x00FF_FFFF,
            alpha,
            flip_x: d.facing_left,
            z: if d.under_ink {
                FreeZ::UnderText
            } else {
                FreeZ::OverText
            },
        })
    }

    /// Draw the departure in `slot` on its own glass (`PetDepartureSprite::own_glass`): a split-pane focus switch's fading half, the old body still in the pane the focus left.
    #[inline]
    pub fn pet_own_glass(
        &mut self,
        frame: PetCursorFrame,
        own: EffectGeom,
        origin: (i32, i32),
        slot: usize,
        free: &mut Vec<FreeSprite>,
    ) -> Option<u64> {
        let PetCursorFrame {
            geom, colors, pet, ..
        } = frame;
        if pet.lane_alpha == 0
            || geom.cell_w == 0
            || geom.cell_h == 0
            || own.cols == 0
            || own.rows == 0
        {
            return None;
        }
        let Some(Some(d)) = pet.departures.get(slot).copied() else {
            return None;
        };
        if !d.own_glass {
            return None;
        }
        let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(geom.cell_h)).round();
        let nat_w = (nat_h * crate::kitty_pet::ART_ASPECT).round();
        let nat = (
            (nat_w as i32).clamp(1, i32::from(u16::MAX)) as u16,
            (nat_h as i32).clamp(1, i32::from(u16::MAX)) as u16,
        );
        let pane = (
            origin.0,
            origin.1,
            origin
                .0
                .saturating_add(i32::from(own.cols) * i32::from(own.cell_w)),
            origin
                .1
                .saturating_add(i32::from(own.rows) * i32::from(own.cell_h)),
        );
        let mut sprite = self.pet_departure_sprite(d, &pet, own, colors, nat)?;
        sprite.x = sprite.x.saturating_add(origin.0);
        sprite.y = sprite.y.saturating_add(origin.1);
        if !crate::pane::crop_free_sprite(&mut sprite, pane) {
            return None;
        }
        let d_colors = match d.colors {
            Some(c) => c,
            None => colors,
        };
        let mut fp = FNV64_OFFSET;
        fp = fold_free(fp, &sprite);
        fp = fnv_mix_u64(fp, u64::from(d_colors.accent));
        fp = fnv_mix_u64(fp, u64::from(d_colors.background));
        free.push(sprite);
        fp = fnv_mix_u64(fp, self.cat_baker.version());
        fp = fnv_mix_u64(fp, self.pet_baker.version());
        Some(fp)
    }

    /// The pet's ink probe, for hosts: `(spans, live_row)`, where `spansr` is viewport row `r`'s inked columns as a half-open `first, end)` span (`(0, 0)` = blank) and `live_row` is the newest inked row on glass, the output edge the pet's watch hugs.
    #[inline]
    #[must_use]
    pub const fn pet_ink(&self) -> (&[(u16, u16)], Option<u16>) {
        let spans = self.pet_ink_spans.as_slice();
        let mut live = None;
        let mut r = spans.len();
        while r > 0 {
            r -= 1;
            let (first, end) = spans[r];
            if end > first {
                live = Some(r as u16);
                break;
            }
        }
        (spans, live)
    }

    const PET_MOTE_HOST_SALT: u64 = 0x7A3D_51C9_E60B_2F81;
    const PET_MOTE_ROT_BUCKETS: u32 = 12;
    const PET_MOTE_ACCENTS: [u32; 13] = [
        0x00E8_6F6F,
        0x00E8_AC6F,
        0x00E8_E86F,
        0x00AC_E86F,
        0x006F_E86F,
        0x006F_E8AC,
        0x006F_E8E8,
        0x006F_ACE8,
        0x006F_6FE8,
        0x00AC_6FE8,
        0x00E8_6FE8,
        0x00E8_6FAC,
        0x0096_98A6,
    ];

    #[inline]
    const fn pet_mote_rot_bucket(rot: f32) -> u32 {
        const INV_TAU: f32 = 1.0 / std::f32::consts::TAU;
        let u = rot * INV_TAU;
        let turn = u - u.floor();
        ((turn * Self::PET_MOTE_ROT_BUCKETS as f32) as u32) % Self::PET_MOTE_ROT_BUCKETS
    }

    #[inline]
    const fn pet_mote_kind_tag(kind: crate::kitty_pet::PetMoteKind) -> u64 {
        match kind {
            crate::kitty_pet::PetMoteKind::Dust => 1,
            crate::kitty_pet::PetMoteKind::Zee | crate::kitty_pet::PetMoteKind::ZeePop => 2,
            crate::kitty_pet::PetMoteKind::Note => 3,
            crate::kitty_pet::PetMoteKind::Heart => 4,
        }
    }

    #[inline]
    const fn pet_mote_host_id(kind: crate::kitty_pet::PetMoteKind, bucket: u32, side: u16) -> u64 {
        let mut x = Self::PET_MOTE_HOST_SALT
            ^ (Self::pet_mote_kind_tag(kind) << 40)
            ^ ((bucket as u64) << 20)
            ^ (side as u64);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }

    const PET_MOTE_TINTS: [[u32; 13]; 4] = {
        let mut out = [[0u32; 13]; 4];
        let mut i = 0;
        while i < 13 {
            let accent = Self::PET_MOTE_ACCENTS[i];
            out[0][i] = mix_rgb(0x00B9_B9C0, accent, 0.22);
            out[1][i] = mix_rgb(0x00F0_F0F6, accent, 0.45);
            out[2][i] = mix_rgb(0x00FF_C24D, accent, 0.15);
            out[3][i] = mix_rgb(0x00FF_6B9A, accent, 0.15);
            i += 1;
        }
        out
    };

    #[inline]
    const fn pet_mote_tint(kind: crate::kitty_pet::PetMoteKind, colors: CatColorKey) -> u32 {
        let accent_idx = if colors.accent < 12 {
            colors.accent as usize
        } else {
            12
        };
        let kind_idx = (Self::pet_mote_kind_tag(kind) - 1) as usize;
        Self::PET_MOTE_TINTS[kind_idx][accent_idx]
    }

    #[inline]
    const fn pet_mote_on_page(
        kind: crate::kitty_pet::PetMoteKind,
        tint: u32,
        page_lum: Option<(&crate::color_math::RelativeLuminanceTable, f32)>,
    ) -> u32 {
        match (kind, page_lum) {
            (
                crate::kitty_pet::PetMoteKind::Note | crate::kitty_pet::PetMoteKind::Heart,
                Some((table, y_bg)),
            ) => crate::rainbow_kitty::ribbon::paper_mark_ink_with_bg_lum(table, tint, y_bg),
            _ => tint,
        }
    }

    const PET_MOTE_ROT_SIN_COS: [(f32, f32); Self::PET_MOTE_ROT_BUCKETS as usize] = {
        let mut out = [(0.0f32, 1.0f32); Self::PET_MOTE_ROT_BUCKETS as usize];
        let mut b = 0usize;
        while b < Self::PET_MOTE_ROT_BUCKETS as usize {
            let ang = b as f32 / Self::PET_MOTE_ROT_BUCKETS as f32 * core::f32::consts::TAU;
            out[b] = crate::supernova::sin_cos_f32(ang);
            b += 1;
        }
        out
    };

    #[inline]
    fn paint_pet_mote(
        tile: &mut aterm_scene::Tile,
        kind: crate::kitty_pet::PetMoteKind,
        bucket: u32,
        side: u16,
    ) {
        #[inline]
        const fn rp(u: f32, v: f32, sin: f32, cos: f32) -> (f32, f32) {
            let (du, dv) = (u - 0.5, v - 0.5);
            (0.5 + du * cos - dv * sin, 0.5 + du * sin + dv * cos)
        }
        #[inline]
        fn fill_poly(
            tile: &mut aterm_scene::Tile,
            pts: &[(f32, f32)],
            alpha: f32,
            sin: f32,
            cos: f32,
            fit: aterm_scene::PathTransform,
        ) {
            let mut poly_buf = [aterm_scene::PathCmd::Close; 11];
            debug_assert!(pts.len() < poly_buf.len());
            let mut i = 0;
            while i < pts.len() {
                let (u, v) = pts[i];
                let (x, y) = rp(u, v, sin, cos);
                poly_buf[i] = if i == 0 {
                    aterm_scene::PathCmd::Move(x, y)
                } else {
                    aterm_scene::PathCmd::Line(x, y)
                };
                i += 1;
            }
            poly_buf[pts.len()] = aterm_scene::PathCmd::Close;
            aterm_scene::fill_path(
                tile,
                &[&poly_buf[..=pts.len()]],
                (1.0, 1.0, 1.0),
                alpha,
                fit,
            );
        }
        if side == 0 {
            return;
        }
        let s = f32::from(side);
        let (sin, cos) = Self::PET_MOTE_ROT_SIN_COS[(bucket % Self::PET_MOTE_ROT_BUCKETS) as usize];
        let white = (1.0, 1.0, 1.0);
        let fit = aterm_scene::PathTransform::fit(u32::from(side), u32::from(side));
        match kind {
            crate::kitty_pet::PetMoteKind::Dust => {
                const DUST_DISCS: [(f32, f32, f32, f32); 3] = [
                    (0.42, 0.52, 0.26, 0.50),
                    (0.68, 0.44, 0.16, 0.42),
                    (0.30, 0.34, 0.12, 0.36),
                ];
                let mut i = 0;
                while i < DUST_DISCS.len() {
                    let (u, v, r, a) = DUST_DISCS[i];
                    let (x, y) = rp(u, v, sin, cos);
                    tile.disc(x * s, y * s, (r * s).max(1.0), white, a);
                    i += 1;
                }
            }
            crate::kitty_pet::PetMoteKind::Zee | crate::kitty_pet::PetMoteKind::ZeePop => {
                fill_poly(
                    tile,
                    &[
                        (0.20, 0.16),
                        (0.80, 0.16),
                        (0.80, 0.32),
                        (0.44, 0.66),
                        (0.80, 0.66),
                        (0.80, 0.84),
                        (0.20, 0.84),
                        (0.20, 0.68),
                        (0.56, 0.32),
                        (0.20, 0.32),
                    ],
                    0.88,
                    sin,
                    cos,
                    fit,
                );
            }
            crate::kitty_pet::PetMoteKind::Note => {
                fill_poly(
                    tile,
                    &[(0.46, 0.12), (0.58, 0.12), (0.58, 0.72), (0.46, 0.72)],
                    0.88,
                    sin,
                    cos,
                    fit,
                );
                fill_poly(
                    tile,
                    &[(0.58, 0.12), (0.82, 0.24), (0.74, 0.44), (0.58, 0.28)],
                    0.88,
                    sin,
                    cos,
                    fit,
                );
                let (hx, hy) = rp(0.42, 0.78, sin, cos);
                tile.disc(hx * s, hy * s, (0.15 * s).max(1.2), white, 0.88);
            }
            crate::kitty_pet::PetMoteKind::Heart => {
                fill_poly(
                    tile,
                    &[(0.18, 0.46), (0.82, 0.46), (0.50, 0.86)],
                    0.85,
                    sin,
                    cos,
                    fit,
                );
                const HEART_LOBES: [(f32, f32); 2] = [(0.34, 0.38), (0.66, 0.38)];
                let mut i = 0;
                while i < HEART_LOBES.len() {
                    let (u, v) = HEART_LOBES[i];
                    let (x, y) = rp(u, v, sin, cos);
                    tile.disc(x * s, y * s, (0.19 * s).max(1.2), white, 0.85);
                    i += 1;
                }
            }
        }
    }

    const DOG_TRAIL_NUM: i32 = 3;
    const DOG_TRAIL_DEN: i32 = 4;

    /// Emit the typed-word dog cameo ([`crate::dog_cameo`]) as one free sprite trailing the cursor
    /// cell: behind it on its row, or flush over or under that row, wherever it covers no letter
    /// (`ink`, the seat law's probe, [`crate::rainbow_kitty::companion::cell_ink`]) and keeps off
    /// the caret's cell, as the flying head keeps off them
    /// ([`KittyCursorPlacement::keep_off_letters`]). A seat stays while it is clear, and the dog
    /// glides to a new one. Centred on the caret's row whatever lay there, it sat on the letters
    /// just typed ('agai' of 'again' after 'lazy dog'). With no clear seat beside the caret it
    /// trails behind it under the letters, which stay drawn over it.
    ///
    /// A seat also keeps off the icons the frame's word pass drew (`icons`, grid pixels
    /// `(x0, x1, y0, y1)` of the window, the canvas's own at `origin` in it): the dog slides along
    /// its band past an icon it would lie on, towards the caret. Under the row, the dog its own
    /// word summoned lay on that word's icon, two faces stacked. An icon holds still, so the
    /// slide keeps the caret's pace.
    #[inline]
    pub fn dog_cameo<F>(
        &mut self,
        frame: DogCameoFrame,
        free: &mut Vec<FreeSprite>,
        ink: F,
        icons: &[(i32, i32, i32, i32)],
        origin: (i32, i32),
    ) -> Option<u64>
    where
        F: Fn(u16, u16) -> Option<bool>,
    {
        let DogCameoFrame {
            geom,
            cursor: (crow, ccol),
            look,
            colors,
            bob,
            alpha,
            now,
        } = frame;
        if alpha == 0 || geom.cell_w == 0 || geom.cell_h == 0 {
            return None;
        }

        let ch = i32::from(geom.cell_h);
        let cw = i32::from(geom.cell_w);
        let aspect = crate::dog_baker::DogBaker::aspect(look.breed);
        let max_w = f32::from(geom.cell_h.saturating_mul(4).max(1));
        let max_h = f32::from(geom.cell_h.saturating_mul(2).max(1));
        let desired_h = 1.45 * f32::from(geom.cell_h);
        let fitted_h = desired_h.max(1.0).min(max_h).min(max_w / aspect);
        let h = (fitted_h.round() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let w = ((f32::from(h) * aspect).round() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let grid_w = i32::from(geom.cols).saturating_mul(cw);
        if i32::from(w) > grid_w {
            return None;
        }

        self.begin_baker_frame(geom, false);
        self.dog_baker.begin_frame(geom.cell_w, geom.cell_h);

        let key = crate::dog_baker::DogBakeKey {
            breed: look.breed,
            coat: look.coat,
            colors,
            w,
            h,
        };
        let host = key.host_id();
        let resolved_tile = match self.dog_baker.tile(&key) {
            Some(rgba) => self.cat_baker.host_tile(host, w, h, rgba),
            None => self.cat_baker.host_peek(host),
        };
        let art = match resolved_tile {
            Some(t) => {
                let art = t.art;
                let already_held = match &self.dog_last_tile {
                    Some((held_art, held_host, held_look)) => {
                        Arc::ptr_eq(held_art, &art) && (*held_host, *held_look) == (host, look)
                    }
                    None => false,
                };
                if !already_held {
                    self.dog_last_tile = Some((Arc::clone(&art), host, look));
                }
                art
            }
            None => match &self.dog_last_tile {
                Some((art, _, held)) if *held == look => Arc::clone(art),
                _ => return None,
            },
        };

        let (w_px, h_px) = (i32::from(w), i32::from(h));
        let grid_h = i32::from(geom.rows).saturating_mul(ch);
        let cursor_left = i32::from(ccol).saturating_mul(cw);
        let trail = cw.saturating_mul(Self::DOG_TRAIL_NUM) / Self::DOG_TRAIL_DEN;
        let x = cursor_left
            .saturating_sub(trail)
            .saturating_sub(w_px)
            .max(0);
        let row_top = i32::from(crow) * ch;
        let seat_y = |seat: DogSeat| match seat {
            DogSeat::Behind => row_top + ch / 2 - h_px / 2,
            DogSeat::Above => row_top - h_px - 1,
            DogSeat::Under => row_top + ch + 1,
        };
        let caret_box = (cursor_left, cursor_left + cw, row_top, row_top + ch);
        #[cfg(test)]
        let icons_off = DOG_ICONS_OFF.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let icons_off = false;
        let icons = if icons_off { &[][..] } else { icons };
        // The seat's left edge on the band from `y`: the trailing column, slid right past every
        // word icon the body would lie on there.
        let slid = |y: i32| -> i32 {
            let mut sx = x;
            for _ in 0..=icons.len() {
                let past = icons
                    .iter()
                    .map(|&(x0, x1, y0, y1)| {
                        (x0 - origin.0, x1 - origin.0, y0 - origin.1, y1 - origin.1)
                    })
                    .filter(|&(x0, x1, y0, y1)| {
                        sx < x1 && sx + w_px > x0 && y < y1 && y + h_px > y0
                    })
                    .map(|(_, x1, _, _)| x1)
                    .max();
                match past {
                    Some(x1) => sx = x1,
                    None => break,
                }
            }
            sx
        };
        // Where the seat puts the dog's left edge, when it is clear there: on the grid, off the
        // caret's cell, off every lettered cell and off the word icons.
        let clear = |y: i32| -> Option<i32> {
            if y < 0 || y + h_px > grid_h {
                return None;
            }
            let x = slid(y);
            if x + w_px > grid_w {
                return None;
            }
            if x < caret_box.1
                && x + w_px > caret_box.0
                && y < caret_box.3
                && y + h_px > caret_box.2
            {
                return None;
            }
            let (r0, r1) = (y / ch, (y + h_px - 1) / ch);
            let (c0, c1) = (x / cw, (x + w_px - 1) / cw);
            (r0..=r1)
                .all(|r| (c0..=c1).all(|c| ink(r as u16, c as u16) != Some(true)))
                .then_some(x)
        };
        // The visit's own last frame: a dog drawn a moment ago glides on from there.
        let held = match self.dog_place {
            Some(p)
                if p.look == look
                    && now.saturating_duration_since(p.drawn) < Duration::from_millis(250) =>
            {
                Some(p)
            }
            _ => None,
        };
        #[cfg(test)]
        let seat_off = DOG_SEAT_OFF.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let seat_off = false;
        let seat = held
            .map(|p| p.seat)
            .into_iter()
            .chain([DogSeat::Behind, DogSeat::Above, DogSeat::Under])
            .find_map(|seat| clear(seat_y(seat)).map(|x| (seat, x)));
        let (seat, seat_x, z) = match seat {
            _ if seat_off => (DogSeat::Behind, x, FreeZ::OverText),
            Some((seat, x)) => (seat, x, FreeZ::OverText),
            None => (DogSeat::Behind, x, FreeZ::UnderText),
        };
        let target = (seat_x as f32, seat_y(seat) as f32);
        let at = match held {
            Some(p) => {
                let dt = now
                    .saturating_duration_since(p.drawn)
                    .as_secs_f32()
                    .min(0.1);
                let k = 1.0 - crate::color_math::exp_f32(-dt * (1.0 / DOG_PLACE_TAU));
                (
                    p.at.0 + (target.0 - p.at.0) * k,
                    p.at.1 + (target.1 - p.at.1) * k,
                )
            }
            None => target,
        };
        self.dog_place = Some(DogPlace {
            look,
            seat,
            at,
            drawn: now,
        });
        let y = at.1.round() as i32 + (bob * ch as f32).round() as i32;
        let sprite = FreeSprite {
            x: at.0.round() as i32,
            y,
            w,
            h,
            art,
            clip: PxRect::EVERYWHERE,
            tint: 0x00FF_FFFF,
            alpha,
            flip_x: false,
            z,
        };
        let mut fp = fold_free(FNV64_OFFSET, &sprite);
        free.push(sprite);
        fp = fnv_mix_u64(fp, self.cat_baker.version());
        fp = fnv_mix_u64(fp, self.dog_baker.version());
        fp = fnv_mix_u64(fp, u64::from(colors.accent));
        fp = fnv_mix_u64(fp, u64::from(colors.background));
        Some(fp)
    }

    /// Install one already-resolved immutable source.
    #[inline]
    pub fn set_kitty_sprite_source(&mut self, source: KittySpriteSource) {
        self.kitty_cursor_placement = KittyCursorPlacementState::default();
        match source {
            KittySpriteSource::BuiltIn => {
                self.kitty_custom = None;
                self.kitty_disabled = false;
                self.kitty_gen = 0;
            }
            KittySpriteSource::Disabled => {
                self.kitty_custom = None;
                self.kitty_disabled = true;
                self.kitty_gen = u64::MAX;
            }
            KittySpriteSource::Custom {
                source_fp,
                w,
                h,
                rgba,
            } => {
                let valid = w > 0
                    && h > 0
                    && match usize::from(w).checked_mul(usize::from(h)) {
                        Some(pixels) => pixels.checked_mul(4) == Some(rgba.len()),
                        None => false,
                    };
                if !valid {
                    self.kitty_custom = None;
                    self.kitty_disabled = true;
                    self.kitty_gen = u64::MAX;
                    return;
                }
                let mut cache_rgba = match self.kitty_custom.take() {
                    Some(c) => c.cache_rgba,
                    None => Vec::new(),
                };
                cache_rgba.clear();
                self.kitty_custom = Some(KittyCustom {
                    source_fp,
                    w,
                    h,
                    rgba,
                    cache_size: (0, 0),
                    cache_rgba,
                });
                self.kitty_disabled = false;
                self.kitty_gen = source_fp;
            }
        }
    }

    /// Diagnosable, allocation-free installed source identity for host conformance tests and capture/glass generation checks.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn kitty_sprite_source_fingerprint(&self) -> Option<u64> {
        if self.kitty_disabled {
            None
        } else {
            Some(match &self.kitty_custom {
                Some(custom) => custom.source_fp,
                None => 0,
            })
        }
    }

    /// Borrow the exact immutable RGBA Arc supplied by the host.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn kitty_sprite_rgba(&self) -> Option<&Arc<[u8]>> {
        match &self.kitty_custom {
            Some(custom) => Some(&custom.rgba),
            None => None,
        }
    }

    /// Whether a user-configured kitty sprite currently overrides the built-in homage.
    #[must_use]
    #[inline]
    pub const fn has_custom_kitty_sprite(&self) -> bool {
        self.kitty_custom.is_some()
    }

    /// Record a committed edit keystroke (printable/IME glyph, backspace, line/word kill) in `pane` (`None` from a host that binds no pane).
    #[inline]
    pub fn note_typed_edit(&mut self, now: Instant, pane: Option<u64>) {
        self.typed_edit = Some(TypedEdit {
            at: now,
            cell: self.caret_of_pane(pane),
            pane,
            spent: false,
        });
    }

    /// Record that `word` was just typed to the pet in `pane` (the [`crate::typed_tricks`] listener fired), so the word flashes rainbow once the rescan finds it.
    #[inline]
    pub fn note_trick_typed(
        &mut self,
        now: Instant,
        pane: Option<u64>,
        word: &str,
        back_chars: u16,
    ) {
        let caret = self.caret_of_pane(pane);
        let scope = self.scan_scope();
        self.trick_flash
            .note(now, pane, scope, caret, word, back_chars);
    }

    /// The typed line was not pet talk after all (`sit tight`): a visible flash leaves within a tenth of a second instead of finishing, and one not yet found is dropped.
    #[inline]
    pub fn revoke_trick_flash(&mut self, now: Instant) {
        self.trick_flash.revoke(now);
    }

    /// Each feline occurrence's palette key, and whether any occurrence is a profanity: what a host's scan decided, for the pipeline's tests.
    #[cfg(test)]
    pub(crate) fn scan_verdicts(&self) -> (Vec<CatColorKey>, bool) {
        let mut felines = Vec::new();
        let mut any_profanity = false;
        let mut i = 0;
        while i < self.occ.len() {
            let o = &self.occ[i];
            match o.class {
                Class::Feline => felines.push(o.cat_colors),
                Class::Profanity => any_profanity = true,
                _ => {}
            }
            i += 1;
        }
        (felines, any_profanity)
    }

    /// Where the trick flash stands at `now` (pending, live, revoked or idle), for tests, the lifecycle model's conformance and driven validation.
    #[must_use]
    #[inline]
    pub fn trick_flash_phase(&self, now: Instant) -> TrickFlashPhase {
        self.trick_flash.phase(now)
    }

    #[inline]
    fn caret_of_pane(&self, pane: Option<u64>) -> Option<(u16, u16)> {
        match pane {
            None => self.last_caret,
            Some(key) if self.bound.is_none() || self.bound == Some(key) => self.last_caret,
            Some(key) => match self.parked.get(&key) {
                Some(parked) => parked.last_caret,
                None => None,
            },
        }
    }

    #[inline]
    const fn typed_edit_reached(&self, row: u16, start_col: u16, end_col: u16) -> bool {
        let Some(e) = self.typed_edit else {
            return false;
        };
        let Some((edit_row, edit_col)) = e.cell else {
            return true;
        };
        edit_row == row
            && edit_col >= start_col.saturating_sub(TYPED_EDIT_REACH_COLS)
            && edit_col
                <= end_col
                    .saturating_add(1)
                    .saturating_add(TYPED_EDIT_REACH_COLS)
    }

    /// Whether a live, unspent edit keystroke witnesses this pane's scan at `now` (see [`TYPED_EDIT_WITNESS_MS`]): its pane must match [`Self::scan_scope`], and `None` on either side matches (a host that records keys with no pane has one grid).
    #[must_use]
    #[inline]
    pub fn typed_edit_witness(&self, now: Instant) -> bool {
        let Some(e) = self.typed_edit else {
            return false;
        };
        !e.spent
            && now.saturating_duration_since(e.at) <= Duration::from_millis(TYPED_EDIT_WITNESS_MS)
            && match (e.pane, self.scan_scope()) {
                (Some(typed_in), Some(scanning)) => typed_in == scanning,
                _ => true,
            }
    }

    #[inline]
    const fn spend_typed_edit(&mut self) {
        if let Some(e) = self.typed_edit.as_mut() {
            e.spent = true;
        }
    }

    /// True if the grid changed since the last scan and a rescan is due.
    #[must_use]
    #[inline]
    pub const fn needs_rescan(&self, epoch: u64) -> bool {
        !self.have_scanned || epoch != self.last_epoch
    }

    /// Rescan the visible grid into occurrences.
    #[allow(
        clippy::too_many_arguments,
        reason = "the rescan threads the term, grid geometry (rows/cols), lexicon, config, damage epoch, and clock through one call; a wrapper struct would relocate the list, not simplify it"
    )]
    #[inline]
    pub fn rescan(
        &mut self,
        term: &Terminal,
        rows: usize,
        cols: usize,
        lexicon: &Lexicon,
        cfg: &DecoConfig,
        epoch: u64,
        now: Instant,
    ) {
        // `last_epoch` is untouched, so the rescan re-fires after thaw; any
        // `set_scan_base_y` declaration for this skipped snapshot is still
        // spent so it never serves a later undeclared rescan.
        if self.frozen_at.is_some() {
            self.trick_flash.declare_base_y(None, None);
            return;
        }
        let opts = cfg.scan_opts();
        self.scan_memo.fit(rows, scan_inputs_fp(lexicon, &opts));
        let cursor = if term.grid().display_offset() == 0 {
            let cursor = term.cursor();
            Some((cursor.row, cursor.col))
        } else {
            None
        };
        if let Some(cell) = cursor {
            self.last_caret = Some(cell);
        }
        let mut out = self.rescan_begin(rows);
        let mut ink_full = false;
        let mut row_cells = std::mem::take(&mut self.scan_cells);
        let mut start_row = 0usize;
        'pass: loop {
            let mut r = start_row;
            while r < rows {
                term.render_row_into(r, &mut row_cells);
                let dec_line = if r <= u16::MAX as usize
                    && let Some(row) = term.grid().row(r as u16)
                {
                    row.line_size() != aterm_core::grid::LineSize::SingleWidth
                } else {
                    false
                };
                if !self.scan_row(
                    r,
                    &row_cells,
                    cursor,
                    None,
                    None,
                    None,
                    dec_line,
                    lexicon,
                    &opts,
                    cfg,
                    now,
                    &mut out,
                    &mut ink_full,
                ) {
                    // §3.6 cap saturated top-down: restart once from the
                    // bottom-priority cutoff so the rows nearest the prompt keep
                    // their slots.
                    if start_row == 0 {
                        let cutoff = self.dense_scan_cutoff(
                            rows,
                            lexicon,
                            &opts,
                            &mut row_cells,
                            |rr, buf| {
                                term.render_row_into(rr, buf);
                            },
                        );
                        if cutoff > 0 {
                            self.rescan_restart(&mut out, &mut ink_full);
                            start_row = cutoff;
                            continue 'pass;
                        }
                    }
                    break;
                }
                r += 1;
            }
            break;
        }
        self.locate_trick_flash(rows, cols, cursor, now, cfg, &mut row_cells, |r, buf| {
            term.render_row_into(r, buf);
        });
        self.scan_cells = row_cells;
        self.rescan_end(out, cols, epoch, now, None, None);
    }

    /// [`rescan`](Self::rescan) over an already-extracted frame snapshot: the render path scans the `RenderInput` rows `cell_frame_into` filled under the same Terminal lock, so the per-cell resolve runs once per frame.
    #[allow(
        clippy::too_many_arguments,
        reason = "the rescan threads the snapshot rows, grid geometry (rows/cols), lexicon, config, damage epoch, and clock through one call; a wrapper struct would relocate the list, not simplify it"
    )]
    #[inline]
    pub fn rescan_from_cells(
        &mut self,
        cells: &[Vec<RenderCell>],
        line_sizes: &[aterm_core::grid::LineSize],
        rows: usize,
        cols: usize,
        lexicon: &Lexicon,
        cfg: &DecoConfig,
        epoch: u64,
        now: Instant,
    ) {
        self.rescan_from_cells_inner(
            cells, line_sizes, rows, cols, lexicon, cfg, epoch, now, None, None,
        );
    }

    /// Snapshot rescan with the live pixel geometry and input caret: cat palettes sample the exact prospective sprite footprint across adjacent rows, and language-gated collision forms touching `cursor` stay provisional until a delimiter moves it away.
    #[allow(
        clippy::too_many_arguments,
        reason = "the live renderer adds one cursor coordinate to the geometry-aware snapshot contract"
    )]
    #[inline]
    pub fn rescan_from_cells_with_geom_at_cursor(
        &mut self,
        cells: &[Vec<RenderCell>],
        line_sizes: &[aterm_core::grid::LineSize],
        rows: usize,
        cols: usize,
        lexicon: &Lexicon,
        cfg: &DecoConfig,
        epoch: u64,
        now: Instant,
        geom: EffectGeom,
        default_bg: u32,
        cursor: Option<(u16, u16)>,
    ) {
        self.rescan_from_cells_inner(
            cells,
            line_sizes,
            rows,
            cols,
            lexicon,
            cfg,
            epoch,
            now,
            Some((geom, default_bg)),
            cursor,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "shared implementation keeps the compatibility and geometry-aware snapshot paths byte-identical outside palette sampling"
    )]
    #[inline]
    fn rescan_from_cells_inner(
        &mut self,
        cells: &[Vec<RenderCell>],
        line_sizes: &[aterm_core::grid::LineSize],
        rows: usize,
        cols: usize,
        lexicon: &Lexicon,
        cfg: &DecoConfig,
        epoch: u64,
        now: Instant,
        context: Option<(EffectGeom, u32)>,
        cursor: Option<(u16, u16)>,
    ) {
        // Frozen ⇒ no-op, as in [`rescan`](Self::rescan) (`last_epoch`
        // untouched, so the rescan stays pending); spend any `set_scan_base_y`
        // declaration for this skipped snapshot.
        if self.frozen_at.is_some() {
            self.trick_flash.declare_base_y(None, None);
            return;
        }
        let opts = cfg.scan_opts();
        self.scan_memo.fit(rows, scan_inputs_fp(lexicon, &opts));
        let pass_end = rows.min(cells.len());
        let mut out = self.rescan_begin(pass_end);
        let mut ink_full = false;
        let mut row_cells = std::mem::take(&mut self.scan_cells);
        let mut start_row = 0usize;
        let (ctx_cells, ctx_geom, ctx_bg) = match context {
            Some((geom, default_bg)) => (Some(cells), Some(geom), Some(default_bg)),
            None => (None, None, None),
        };
        'pass: loop {
            let mut r = start_row;
            while r < pass_end {
                let row_slice = &cells[r];
                let dec_line = matches!(
                    line_sizes.get(r),
                    Some(ls) if *ls != aterm_core::grid::LineSize::SingleWidth
                );
                if !self.scan_row(
                    r,
                    row_slice,
                    cursor,
                    ctx_cells,
                    ctx_geom,
                    ctx_bg,
                    dec_line,
                    lexicon,
                    &opts,
                    cfg,
                    now,
                    &mut out,
                    &mut ink_full,
                ) {
                    // §3.6 cap saturated top-down: restart once from the
                    // bottom-priority cutoff (as in the term-walk twin).
                    if start_row == 0 {
                        let cutoff = self.dense_scan_cutoff(
                            rows,
                            lexicon,
                            &opts,
                            &mut row_cells,
                            |rr, buf| {
                                buf.clear();
                                if let Some(row) = cells.get(rr) {
                                    buf.extend_from_slice(row);
                                }
                            },
                        );
                        if cutoff > 0 {
                            self.rescan_restart(&mut out, &mut ink_full);
                            start_row = cutoff;
                            continue 'pass;
                        }
                    }
                    break;
                }
                r += 1;
            }
            break;
        }
        self.locate_trick_flash(rows, cols, cursor, now, cfg, &mut row_cells, |r, buf| {
            buf.clear();
            if let Some(row) = cells.get(r) {
                buf.extend_from_slice(row);
            }
        });
        self.scan_cells = row_cells;
        self.rescan_end(
            out,
            cols,
            epoch,
            now,
            context.map(|(geom, _)| CatSurface {
                cells,
                geom,
                feline_magic: cfg.feline_magic,
            }),
            cursor,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "threads the rescan's geometry, caret, clock, config and row source to the flash; the dense_scan_cutoff shape"
    )]
    #[inline]
    fn locate_trick_flash(
        &mut self,
        rows: usize,
        cols: usize,
        cursor: Option<(u16, u16)>,
        now: Instant,
        cfg: &DecoConfig,
        row_buf: &mut Vec<RenderCell>,
        load_row: impl FnMut(usize, &mut Vec<RenderCell>),
    ) {
        let scope = self.scan_scope();
        self.trick_flash.on_rescan(
            scope,
            now,
            rows,
            cols,
            cursor,
            trick_flash_paintable(cfg),
            row_buf,
            load_row,
        );
    }

    #[inline]
    fn rescan_begin(&mut self, rows: usize) -> Vec<Occurrence> {
        self.seed_ordinals.clear();
        self.rescan_seq = self.rescan_seq.wrapping_add(1);
        self.pending.clear();
        self.scan_row_occupied.clear();
        self.scan_row_occupied.resize(rows, false);
        self.pet_ink_spans.clear();
        self.pet_ink_spans.resize(rows, (0, 0));
        let mut out = std::mem::take(&mut self.occ);
        out.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        out
    }

    #[inline]
    fn dense_scan_cutoff(
        &mut self,
        rows: usize,
        lexicon: &Lexicon,
        opts: &ScanOptions<'_>,
        row_cells: &mut Vec<RenderCell>,
        mut load_row: impl FnMut(usize, &mut Vec<RenderCell>),
    ) -> usize {
        if self.scan_row_occupied.len() < rows {
            self.scan_row_occupied.resize(rows, false);
        }
        if self.pet_ink_spans.len() < rows {
            self.pet_ink_spans.resize(rows, (0, 0));
        }
        let mut budget = MAX_OCCURRENCES;
        let mut cutoff = 0usize;
        let mut r = rows;
        while r > 0 {
            r -= 1;
            load_row(r, row_cells);
            let need_text = cutoff == 0;
            if need_text {
                self.scan_text.clear();
            }
            let mut ink_first = u16::MAX;
            let mut ink_end = 0u16;
            let mut occupied = false;
            let mut c = 0;
            while c < row_cells.len() {
                let cell = &row_cells[c];
                let col = c as u16;
                if cell.wide {
                    if ink_end == col {
                        ink_first = ink_first.min(col);
                        ink_end = col.saturating_add(1);
                    }
                    c += 1;
                    continue;
                }
                if cell.ch != ' '
                    && !if cell.ch.is_ascii() {
                        cell.ch.is_ascii_whitespace()
                    } else {
                        cell.ch.is_whitespace()
                    }
                {
                    if !occupied {
                        occupied = true;
                        if need_text {
                            let mut pc = 0;
                            while pc < c {
                                let prev = &row_cells[pc];
                                if !prev.wide {
                                    self.scan_text.push(prev.ch);
                                }
                                pc += 1;
                            }
                        }
                    }
                    ink_first = ink_first.min(col);
                    ink_end = col.saturating_add(1);
                }
                if need_text && occupied {
                    self.scan_text.push(cell.ch);
                }
                c += 1;
            }
            self.pet_ink_spans[r] = if ink_end > ink_first {
                (ink_first, ink_end)
            } else {
                (0, 0)
            };
            self.scan_row_occupied[r] = occupied;
            if !occupied || cutoff > 0 {
                continue;
            }
            let est = if let Some(len) = self.scan_memo.touch_len(&self.scan_text) {
                len
            } else {
                lexicon.scan_into_with_scratch(
                    &self.scan_text,
                    opts,
                    &mut self.scan_chars,
                    &mut self.scan_matches,
                    &mut self.scan_scratch,
                );
                self.scan_memo.remember(&self.scan_text, &self.scan_matches);
                self.scan_matches.len()
            };
            if est > budget {
                cutoff = r + 1;
            } else {
                budget -= est;
            }
        }
        cutoff.min(rows.saturating_sub(1))
    }

    #[inline]
    fn rescan_restart(&mut self, out: &mut Vec<Occurrence>, ink_full: &mut bool) {
        self.seed_ordinals.clear();
        self.pending.clear();
        out.clear();
        self.ink_base_fg.clear();
        self.ink_cols.clear();
        *ink_full = false;
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the per-row scanner threads the row cells + index, the §5.7 flag, lexicon, config, clock, and the rescan's occurrence/ink-latch accumulators through one call; a wrapper struct would relocate the list, not simplify it"
    )]
    #[inline]
    fn scan_row(
        &mut self,
        r: usize,
        row_cells: &[RenderCell],
        cursor: Option<(u16, u16)>,
        context_cells: Option<&[Vec<RenderCell>]>,
        context_geom: Option<EffectGeom>,
        context_default_bg: Option<u32>,
        dec_line: bool,
        lexicon: &Lexicon,
        opts: &ScanOptions<'_>,
        cfg: &DecoConfig,
        now: Instant,
        out: &mut Vec<Occurrence>,
        ink_full: &mut bool,
    ) -> bool {
        self.scan_text.clear();
        self.scan_colmap.clear();
        let mut ink_first = u16::MAX;
        let mut ink_end = 0u16;
        let mut occupied = false;
        let mut c = 0;
        while c < row_cells.len() {
            let cell = &row_cells[c];
            let col = c as u16;
            if cell.wide {
                if ink_end == col {
                    ink_first = ink_first.min(col);
                    ink_end = col.saturating_add(1);
                }
                c += 1;
                continue;
            }
            if cell.ch != ' '
                && !if cell.ch.is_ascii() {
                    cell.ch.is_ascii_whitespace()
                } else {
                    cell.ch.is_whitespace()
                }
            {
                if !occupied {
                    occupied = true;
                    let mut prev_c = 0;
                    while prev_c < c {
                        let prev = &row_cells[prev_c];
                        if !prev.wide {
                            self.scan_text.push(prev.ch);
                            self.scan_colmap.push(prev_c as u16);
                        }
                        prev_c += 1;
                    }
                }
                ink_first = ink_first.min(col);
                ink_end = col.saturating_add(1);
            }
            if occupied {
                self.scan_text.push(cell.ch);
                self.scan_colmap.push(col);
            }
            c += 1;
        }
        if self.pet_ink_spans.len() <= r {
            self.pet_ink_spans.resize(r + 1, (0, 0));
        }
        self.pet_ink_spans[r] = if ink_end > ink_first {
            (ink_first, ink_end)
        } else {
            (0, 0)
        };
        if self.scan_row_occupied.len() <= r {
            self.scan_row_occupied.resize(r + 1, false);
        }
        self.scan_row_occupied[r] = occupied;
        if !occupied {
            return true;
        }
        let hit_len = self.scan_memo.touch_len(&self.scan_text);
        if hit_len.is_none() {
            lexicon.scan_into_with_scratch(
                &self.scan_text,
                opts,
                &mut self.scan_chars,
                &mut self.scan_matches,
                &mut self.scan_scratch,
            );
            self.scan_memo.remember(&self.scan_text, &self.scan_matches);
        }
        if hit_len == Some(0) || (hit_len.is_none() && self.scan_matches.is_empty()) {
            return true;
        }
        let hit = hit_len.is_some();
        let matches: &[Match] = if hit {
            self.scan_memo.matches(&self.scan_text)
        } else {
            &self.scan_matches
        };
        let mut chars_ready = !hit;
        if chars_ready && !matches.is_empty() {
            debug_assert_eq!(
                self.scan_chars.len(),
                self.scan_colmap.len(),
                "scan_chars must mirror scan_text before the match loop indexes it"
            );
        }
        let seq = self.rescan_seq;
        let ttl = match self.grace_override {
            Some(ttl) => ttl,
            None => GRACE_TTL,
        };
        let mut mi = 0;
        while mi < matches.len() {
            let m = matches[mi];
            mi += 1;
            let start_col = self.scan_colmap.get(m.start).copied().unwrap_or_default();
            let end_lead = match self.scan_colmap.get(m.end.saturating_sub(1)) {
                Some(&c) => c,
                None => start_col,
            };
            let end_col = if matches!(row_cells.get(end_lead as usize + 1), Some(n) if n.wide) {
                end_lead + 1
            } else {
                end_lead
            };
            let at_live_cursor = match cursor {
                Some(cell) => caret_on_span(cell, r as u16, start_col, end_col),
                None => false,
            };
            if m.ambiguous && at_live_cursor {
                continue;
            }
            let live_caret_completion = match cursor {
                Some((cursor_row, cursor_col)) => {
                    cursor_row == r as u16 && cursor_col == end_col.saturating_add(1)
                }
                None => false,
            };
            let ov = cfg.spec_table.override_for(m.form_hash).copied();
            if ov.is_none() {
                match m.class {
                    Class::Profanity if !cfg.profanity => continue,
                    Class::Feline if !cfg.feline => continue,
                    Class::Canine if !cfg.canine => continue,
                    Class::Emphasis if !cfg.emphasis => continue,
                    _ => {}
                }
            }
            let (spec, custom) = match ov {
                Some(s) => (s, true),
                None => (class_default_spec(m.class, cfg), false),
            };
            let seed = seed_of(start_col, m.class, m.form_hash);
            let ordinal = {
                let n = self.seed_ordinals.entry(seed).or_insert(0);
                let o = *n;
                *n += 1;
                o
            };
            let ident = mix(seed ^ ordinal.wrapping_mul(ORDINAL_MIX));
            let mut inert = false;
            let fast = match self.persist.get_mut(&ident) {
                Some(ep)
                    if ep.last_row == r as u16
                        && ep.seed == seed
                        && ep.form_id == m.form_id
                        && !(ep.continuity_tainted
                            && (m.class == Class::Feline
                                || (m.class == Class::Profanity && live_caret_completion)))
                        && !(m.class == Class::Feline
                            && at_live_cursor
                            && ep.peek_done
                            && seq.wrapping_sub(ep.seen_seq) >= 2)
                        && seq.wrapping_sub(ep.seen_seq) <= REKEY_MAX_SEQ_GAP
                        && now.saturating_duration_since(ep.last_seen) <= ttl =>
                {
                    ep.last_seen = now;
                    ep.seen_seq = seq;
                    ep.last_col = start_col;
                    ep.continuity_tainted = false;
                    inert = ep.inert();
                    Some((ep.appeared, ep.genome, ep.cat_colors))
                }
                _ => None,
            };
            let (appeared, genome, cat_colors) = match fast {
                Some(triple) => triple,
                None => {
                    if !chars_ready {
                        self.scan_chars.clear();
                        self.scan_chars.extend(self.scan_text.chars());
                        chars_ready = true;
                        debug_assert_eq!(
                            self.scan_chars.len(),
                            self.scan_colmap.len(),
                            "scan_chars must mirror scan_text before the match loop indexes it"
                        );
                    }
                    let ctx_fp = genome::simhash_ctx(
                        &self.scan_chars,
                        m.start,
                        m.end,
                        m.form_hash,
                        &mut self.votes,
                        &mut self.ctx_tok,
                        &mut self.ctx_folded,
                    );
                    let genome = Genome {
                        gkey: seed ^ ctx_fp,
                        magic: mix(ctx_fp ^ m.form_hash ^ genome::MAGIC_SALT),
                    };
                    self.pending.push(PendingBirth {
                        occ_idx: out.len() as u16,
                        ctx_fp,
                        live_caret_completion,
                        at_live_cursor,
                    });
                    (
                        now,
                        genome,
                        if spec.graphic.is_some() {
                            match (context_cells, context_geom, context_default_bg) {
                                (Some(cells), Some(geom), Some(default_bg)) => {
                                    cat_color_context_footprint(
                                        cells,
                                        geom,
                                        r as u16,
                                        start_col,
                                        end_col,
                                        genome,
                                        cfg.feline_magic,
                                        default_bg,
                                    )
                                }
                                _ => cat_color_context(row_cells, start_col, end_col),
                            }
                        } else {
                            CatColorKey::default()
                        },
                    )
                }
            };
            let ink_bearing = cfg.ink_enabled && spec.ink.is_some();
            let mut ink_base = 0usize;
            let mut ink_cells = 0u16;
            let mut ink_bg = [0u8; 3];
            let mut have_bg = false;
            let capture_ink = ink_bearing && !*ink_full;
            if capture_ink {
                ink_base = self.ink_base_fg.len();
            }
            let mut c = start_col;
            while c <= end_col {
                let cell = &row_cells[c as usize];
                if cell.wide {
                    if c == u16::MAX {
                        break;
                    }
                    c += 1;
                    continue;
                }
                if !have_bg {
                    ink_bg = cell.bg;
                    have_bg = true;
                    if !capture_ink {
                        break;
                    }
                }
                if capture_ink {
                    if self.ink_base_fg.len() == MAX_INK_CELLS {
                        self.ink_base_fg.truncate(ink_base);
                        self.ink_cols.truncate(ink_base);
                        ink_base = 0;
                        ink_cells = 0;
                        *ink_full = true;
                        break;
                    }
                    self.ink_base_fg.push(cell.fg);
                    self.ink_cols.push(c);
                    ink_cells += 1;
                }
                if c == u16::MAX {
                    break;
                }
                c += 1;
            }
            out.push(Occurrence {
                row: r as u16,
                start_col,
                end_col,
                class: m.class,
                langs: m.langs,
                form_id: m.form_id,
                seed,
                ident,
                appeared,
                genome,
                ink_base,
                ink_cells,
                ink_bg,
                cat_colors,
                cat_text_clear: true,
                cat_peek_down: false,
                dec_line,
                inert,
                spec,
                custom,
                species: match m.class {
                    Class::Animal => match m.species {
                        Some(s) => animal_glyph_from_key(lexicon.species_code(s)),
                        None => None,
                    },
                    Class::Canine => animal_glyph_from_key("dog"),
                    _ => None,
                },
            });
            if out.len() >= MAX_OCCURRENCES {
                return false;
            }
        }
        true
    }

    #[inline]
    fn rescan_end(
        &mut self,
        mut out: Vec<Occurrence>,
        cols: usize,
        epoch: u64,
        now: Instant,
        cat_surface: Option<CatSurface<'_>>,
        cursor: Option<(u16, u16)>,
    ) {
        let cols_changed = self.have_scanned && self.cols != cols as u16;
        if cols_changed {
            self.settle_until = Some(now + Duration::from_millis(RESIZE_SETTLE_MS));
        }
        let returning = std::mem::take(&mut self.spend_next_births);
        let born_settled =
            cols_changed || returning || matches!(self.settle_until, Some(t) if now < t);
        if !born_settled {
            self.settle_until = None;
        }

        let ttl = match self.grace_override {
            Some(ttl) => ttl,
            None => GRACE_TTL,
        };
        let marks = &mut self.done_marks;
        self.persist.retain(|ident, ep| {
            if now.saturating_duration_since(ep.last_seen) <= ttl {
                return true;
            }
            if ep.one_shot_started() {
                mark_done(marks, *ident, ep);
            }
            false
        });

        self.align_pending(&mut out, cols as u16, now, born_settled);

        let rescan_seq = self.rescan_seq;
        for ep in self.persist.values_mut() {
            if ep.seen_seq != rescan_seq
                && matches!(
                    self.scan_row_occupied.get(usize::from(ep.last_row)),
                    Some(true)
                )
            {
                ep.continuity_tainted = true;
            }
        }

        if let Some(surface) = cat_surface {
            let mut oi = 0;
            while oi < out.len() {
                let occ = &mut out[oi];
                oi += 1;
                if occ.spec.graphic.is_none() {
                    occ.cat_text_clear = true;
                    occ.cat_peek_down = false;
                    continue;
                }
                let plan = cat_peek_plan(
                    surface.cells,
                    surface.geom,
                    occ.row,
                    occ.start_col,
                    occ.end_col,
                    occ.genome,
                    surface.feline_magic,
                );
                occ.cat_text_clear = plan.is_some();
                occ.cat_peek_down = plan == Some(PeekDir::Down);
            }
        }

        self.occ = out;
        if let Some(cell) = cursor {
            let mut oi = 0;
            while oi < self.occ.len() {
                let occ = &self.occ[oi];
                if caret_on_span(cell, occ.row, occ.start_col, occ.end_col) {
                    self.caret_word = Some((occ.ident, cell));
                    break;
                }
                oi += 1;
            }
            self.last_caret = Some(cell);
        }
        self.last_epoch = epoch;
        self.have_scanned = true;
        self.cols = cols as u16;
    }

    #[inline]
    fn align_pending(
        &mut self,
        out: &mut [Occurrence],
        cols: u16,
        now: Instant,
        born_settled: bool,
    ) {
        if self.pending.is_empty() {
            return;
        }
        let seq = self.rescan_seq;
        let typed_witness = self.typed_edit_witness(now);
        let mut typed_witness_used = false;
        let pending = std::mem::take(&mut self.pending);
        let mut typed_here = [false; MAX_OCCURRENCES];
        if typed_witness {
            let limit = pending.len().min(MAX_OCCURRENCES);
            let mut k = 0;
            while k < limit {
                let occ = &out[usize::from(pending[k].occ_idx)];
                typed_here[k] = self.typed_edit_reached(occ.row, occ.start_col, occ.end_col);
                k += 1;
            }
        }
        let mut done = [false; MAX_OCCURRENCES];
        let mut group = [0usize; MAX_OCCURRENCES];
        let mut unseen = [(FormId::UNKNOWN, 0u64); PERSIST_CAP];
        let mut winner: [Option<usize>; MAX_OCCURRENCES] = [None; MAX_OCCURRENCES];
        let mut matched = [false; PERSIST_CAP];
        let mut prev_scores = [AlignScore::default(); MAX_OCCURRENCES + 1];
        let mut curr_scores = [AlignScore::default(); MAX_OCCURRENCES + 1];
        let mut unseen_len = 0usize;
        for (&key, ep) in &self.persist {
            if ep.seen_seq != seq {
                debug_assert!(unseen_len < PERSIST_CAP);
                unseen[unseen_len] = (ep.form_id, key);
                unseen_len += 1;
            }
        }
        unseen[..unseen_len].sort_unstable_by_key(|&(f, _)| f);
        let mut decisions = std::mem::take(&mut self.align_decisions);
        if unseen_len > 0 && decisions.len() < ALIGN_DECISION_CELLS {
            decisions.resize(ALIGN_DECISION_CELLS, 0);
        }
        let mut i = 0;
        while i < pending.len() {
            if done[i] {
                i += 1;
                continue;
            }
            let form_id = out[usize::from(pending[i].occ_idx)].form_id;
            let mut group_len = 0usize;
            let mut k = i;
            while k < pending.len() {
                if !done[k] && out[usize::from(pending[k].occ_idx)].form_id == form_id {
                    done[k] = true;
                    group[group_len] = k;
                    group_len += 1;
                }
                k += 1;
            }
            let group = &group[..group_len];
            self.align_old.clear();
            if unseen_len > 0 {
                #[inline]
                fn unseen_form_range(unseen: &[(FormId, u64)], form_id: FormId) -> (usize, usize) {
                    let mut l = 0usize;
                    let mut r = unseen.len();
                    while l < r {
                        let mid = (l + r) >> 1;
                        if unseen[mid].0 < form_id {
                            l = mid + 1;
                        } else {
                            r = mid;
                        }
                    }
                    let lo = l;
                    let mut hi = lo;
                    while hi < unseen.len() && unseen[hi].0 == form_id {
                        hi += 1;
                    }
                    (lo, hi)
                }
                let (lo, hi) = unseen_form_range(&unseen[..unseen_len], form_id);
                let mut ui = lo;
                while ui < hi {
                    let k = unseen[ui].1;
                    // Order does not matter; `align_old` is sorted below by
                    // `(last_row, last_col, key)`.
                    if let std::collections::hash_map::Entry::Occupied(entry) =
                        self.persist.entry(k)
                        && entry.get().form_id == form_id
                        && entry.get().seen_seq != seq
                    {
                        self.align_old.push((k, entry.remove()));
                    }
                    ui += 1;
                }
                self.align_old
                    .sort_unstable_by_key(|(k, ep)| (ep.last_row, ep.last_col, *k));
            }

            if !self.align_old.is_empty() {
                winner[..group_len].fill(None);
                prev_scores[..=group_len].fill(AlignScore::default());
                decisions[1..=group_len].fill(2);
                let same_width = self.have_scanned && self.cols == cols;
                let mut oi = 1;
                while oi <= self.align_old.len() {
                    let row_base = oi * (MAX_OCCURRENCES + 1);
                    curr_scores[0] = prev_scores[0];
                    decisions[row_base] = 1;
                    let ep = &self.align_old[oi - 1].1;
                    let mut gj = 1;
                    while gj <= group_len {
                        let mut best = prev_scores[gj];
                        let mut decision = 1u8;
                        if curr_scores[gj - 1].better_than(best) {
                            best = curr_scores[gj - 1];
                            decision = 2;
                        }
                        let k = group[gj - 1];
                        let candidate = pending[k];
                        let occ = &out[usize::from(candidate.occ_idx)];
                        if let Some((tier, distance)) =
                            alignment_edge(ep, candidate, occ, same_width, seq, cols)
                        {
                            let stationary = ep.last_row == occ.row && ep.last_col == occ.start_col;
                            let score = prev_scores[gj - 1].with_edge(stationary, tier, distance);
                            if score.better_than(best) || score == best {
                                best = score;
                                decision = 3;
                            }
                        }
                        curr_scores[gj] = best;
                        decisions[row_base + gj] = decision;
                        gj += 1;
                    }
                    std::mem::swap(&mut prev_scores, &mut curr_scores);
                    oi += 1;
                }
                let mut oi = self.align_old.len();
                let mut gj = group_len;
                while oi > 0 || gj > 0 {
                    match decisions[oi * (MAX_OCCURRENCES + 1) + gj] {
                        1 => oi -= 1,
                        2 => gj -= 1,
                        3 => {
                            winner[gj - 1] = Some(oi - 1);
                            oi -= 1;
                            gj -= 1;
                        }
                        _ => break,
                    }
                }
                matched[..self.align_old.len()].fill(false);
                let mut wi = 0;
                while wi < group_len {
                    if let Some(oi) = winner[wi] {
                        matched[oi] = true;
                    }
                    wi += 1;
                }
            }
            let group_class = out[usize::from(pending[group[0]].occ_idx)].class;
            let mut has_tainted_old = false;
            let mut ai = 0;
            while ai < self.align_old.len() {
                if self.align_old[ai].1.continuity_tainted {
                    has_tainted_old = true;
                    break;
                }
                ai += 1;
            }
            let mut group_has_live_caret_completion = false;
            let mut typed_keys = [0u64; MAX_OCCURRENCES];
            let mut typed_keys_len = 0usize;
            if typed_witness && has_tainted_old {
                let mut gi = 0;
                while gi < group.len() {
                    let k = group[gi];
                    if typed_here[k] {
                        if pending[k].live_caret_completion {
                            group_has_live_caret_completion = true;
                        }
                        if pending[k].at_live_cursor && typed_keys_len < MAX_OCCURRENCES {
                            let occ = &out[usize::from(pending[k].occ_idx)];
                            typed_keys[typed_keys_len] = occ.ident ^ pending[k].ctx_fp;
                            typed_keys_len += 1;
                        }
                    }
                    gi += 1;
                }
            }
            let group_has_at_live_cursor = typed_keys_len > 0;

            let mut j = 0;
            while j < group.len() {
                let k = group[j];
                let occ = &mut out[usize::from(pending[k].occ_idx)];
                if !self.align_old.is_empty()
                    && let Some(oi) = winner[j]
                {
                    matched[oi] = true;
                    let (old_ident, mut ep) = self.align_old[oi];
                    ep.last_seen = now;
                    ep.seen_seq = seq;
                    ep.seed = occ.seed;
                    ep.form_id = occ.form_id;
                    ep.ctx_fp = pending[k].ctx_fp;
                    ep.last_row = occ.row;
                    ep.last_col = occ.start_col;
                    ep.continuity_tainted = false;
                    ep.ledger_stamped = false;
                    occ.appeared = ep.appeared;
                    occ.genome = ep.genome;
                    occ.cat_colors = ep.cat_colors;
                    occ.inert = ep.inert();
                    self.rekey_persist_episode(old_ident, occ.ident, ep);
                } else {
                    let key = occ.ident ^ pending[k].ctx_fp;
                    let typed_reentry = typed_here[k]
                        && ((group_class == Class::Feline && pending[k].at_live_cursor)
                            || (group_class == Class::Profanity
                                && has_tainted_old
                                && pending[k].live_caret_completion));
                    let born_done = if typed_reentry {
                        typed_witness_used = true;
                        self.done_marks.remove(key);
                        false
                    } else {
                        touch_done(&mut self.done_marks, key)
                    };
                    let mut ep = Episode::fresh(now, occ.genome, occ.seed, occ.row, seq);
                    ep.form_id = occ.form_id;
                    ep.ctx_fp = pending[k].ctx_fp;
                    ep.last_col = occ.start_col;
                    ep.cat_colors = occ.cat_colors;
                    ep.burst_kind = occ.spec.burst.map(|b| b.kind);
                    self.birth_seq = self.birth_seq.wrapping_add(1);
                    if !(born_done || born_settled)
                        && let Some(b) = occ.spec.burst
                        && b.chance_pct > 0
                    {
                        let level = if b.kind == BurstKind::SuperNova {
                            let typed = typed_here[k]
                                && group_class == Class::Profanity
                                && pending[k].live_caret_completion;
                            self.combo_level(now, typed)
                        } else {
                            0
                        };
                        let draw =
                            mix(occ.genome.gkey ^ self.birth_seq ^ supernova::SUPERNOVA_SALT);
                        ep.burst_roll =
                            draw % 100 < u64::from(supernova::combo_chance(b.chance_pct, level));
                        ep.burst_tier = supernova::tier_for(draw, level);
                    }
                    if !(born_done || born_settled)
                        && typed_here[k]
                        && group_class == Class::Profanity
                        && pending[k].live_caret_completion
                    {
                        ep.bonk_pending = true;
                    }
                    if born_done || born_settled {
                        ep.born_done = born_done;
                        ep.born_settled = !born_done && born_settled;
                        ep.peek_done = true;
                        ep.burst_done = born_done;
                        ep.sweep_done = true;
                        ep.nova_done = true;
                        occ.inert = true;
                    }
                    self.insert_persist_bounded(occ.ident, ep, PersistAdmission::Observed);
                }
                j += 1;
            }
            let mut oi = 0;
            while oi < self.align_old.len() {
                if matched[oi] {
                    oi += 1;
                    continue;
                }
                let (old_ident, ep) = self.align_old[oi];
                oi += 1;
                if ep.continuity_tainted
                    && ((group_class == Class::Feline && group_has_at_live_cursor)
                        || (group_class == Class::Profanity && group_has_live_caret_completion))
                {
                    let old_key = old_ident ^ ep.ctx_fp;
                    if group_class == Class::Feline {
                        let mut typed = false;
                        let mut ti = 0;
                        while ti < typed_keys_len {
                            if typed_keys[ti] == old_key {
                                typed = true;
                                break;
                            }
                            ti += 1;
                        }
                        if !typed {
                            self.insert_persist_bounded(old_ident, ep, PersistAdmission::Grace);
                            continue;
                        }
                    }
                    self.done_marks.remove(old_key);
                    continue;
                }
                self.insert_persist_bounded(old_ident, ep, PersistAdmission::Grace);
            }
            i += 1;
        }
        self.pending = pending;
        self.align_decisions = decisions;
        self.align_old.clear();
        if typed_witness_used {
            self.spend_typed_edit();
        }
    }

    #[inline]
    const fn ink_sorted_unique(ink: &[InkCell]) -> bool {
        let mut i = 1;
        while i < ink.len() {
            let (a, b) = (ink[i - 1], ink[i]);
            if a.row > b.row || (a.row == b.row && a.col >= b.col) {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Advance the animation one frame and emit into `out` (decorations), `ink` (fg overrides, sorted unique `(row, col)`: row-major emission, the trick flash merged in order), `free` (one [`FreeSprite`] per cat, over its baked art) and `nova` (§6 premultiplied row-band GlowQuads for `nova_add`).
    #[allow(
        clippy::too_many_arguments,
        reason = "the per-frame tick threads the clock, config, geometry, companion view, selection view, focus gate, and four output scratches through one call; a wrapper struct would relocate the list, not simplify it"
    )]
    #[inline]
    pub fn tick(
        &mut self,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        companion: Option<CompanionOnGlass>,
        sel: Option<SelView<'_>>,
        _focused: bool,
        out: &mut Vec<WordDecoration>,
        ink: &mut Vec<InkCell>,
        free: &mut Vec<FreeSprite>,
        nova: &mut Vec<GlowQuad>,
    ) -> u64 {
        let companion_at = match companion {
            Some(c) => c.cell,
            None => None,
        };
        out.clear();
        ink.clear();
        free.clear();
        nova.clear();
        self.sightings.clear();
        self.curse_cues.clear();
        self.peek_cues.clear();
        if !self.host_brackets_frames {
            self.cat_baker_ready = false;
        }
        match (companion, companion_at) {
            (None, _) => self.companion_claim = None,
            (Some(_), None) => {}
            (_, Some(cell)) => {
                let fallback_id = match self.caret_word {
                    Some((id, at)) if at.0 == cell.0 && at.1 <= cell.1 => Some(id),
                    _ => None,
                };
                let mut live = None;
                let mut fallback_live = None;
                let mut oi = 0;
                while oi < self.occ.len() {
                    let occ = &self.occ[oi];
                    if caret_on_span(cell, occ.row, occ.start_col, occ.end_col) {
                        live = Some(occ.ident);
                        break;
                    }
                    if fallback_live.is_none() && fallback_id == Some(occ.ident) {
                        fallback_live = Some(occ.ident);
                    }
                    oi += 1;
                }
                let live = live.or(fallback_live);
                if live.is_some() {
                    self.companion_claim = live;
                }
            }
        }
        if self.frozen_at.is_some() {
            self.novas.clear();
            self.coupling.clear();
            return 0;
        }
        if self.occ.is_empty() {
            let mut fp = FNV64_OFFSET;
            let scope = self.scan_scope();
            if let Some(until) =
                self.trick_flash
                    .emit(scope, now, trick_flash_paintable(cfg), ink, &mut fp)
            {
                self.active_until = Some(until);
                self.novas.clear();
                self.coupling.clear();
                debug_assert!(
                    Self::ink_sorted_unique(ink),
                    "ink must be sorted by (row, col) with unique cells"
                );
                return fp;
            }
            self.active_until = None;
            self.novas.clear();
            self.coupling.clear();
            return 0;
        }
        self.frame = self.frame.wrapping_add(1);
        self.begin_baker_frame(geom, true);
        let prepass = self.episode_prepass(now, cfg, geom);
        let anim = Duration::from_millis(cfg.anim_ms);
        let mut fp = FNV64_OFFSET;
        let mut active_until: Option<Instant> = None;
        let mut nuke_pending: Option<(u64, i32, i32)> = None;
        if let Some(until) = self.super_prepass(now, cfg, geom, sel, prepass.has_super) {
            arm_until(&mut active_until, until);
        }
        if let Some(until) = self.nova_prepass(now, cfg, geom, sel, prepass.has_nova) {
            arm_until(&mut active_until, until);
        }
        if prepass.has_bonk {
            let mut oi = 0;
            while oi < self.occ.len() {
                let occ = &self.occ[oi];
                oi += 1;
                if occ.class != Class::Profanity || occ.inert {
                    continue;
                }
                let Some(ep) = self.persist.get_mut(&occ.ident) else {
                    continue;
                };
                if !ep.bonk_pending {
                    continue;
                }
                if self.curse_cues.len() >= MAX_CURSE_CUES {
                    break;
                }
                ep.bonk_pending = false;
                self.curse_cues.push(CurseCue {
                    kind: CurseCueKind::Typed,
                    row: occ.row,
                    col: occ.start_col,
                });
            }
        }
        let frame = self.frame;
        let claim = self.companion_claim;
        let mut ctx = CatTick {
            baker: &mut self.cat_baker,
            animal_baker: &mut self.animal_baker,
            sightings: &mut self.sightings,
            peek_cues: &mut self.peek_cues,
            companion_px: match companion {
                Some(c) => c.footprint(geom),
                None => CompanionFootprint::default(),
            },
        };
        let mut cats = 0usize;
        let mut animals = 0usize;
        let table = crate::color_math::relative_luminance_table();

        let mut oi = 0;
        while oi < self.occ.len() {
            let occ = &self.occ[oi];
            let cur_oi = oi;
            oi += 1;
            let ep = if !occ.inert && (occ.spec.graphic.is_some() || occ.spec.burst.is_some()) {
                self.persist.get(&occ.ident)
            } else {
                None
            };
            let fx = if occ.ink_cells > 0
                && cfg.ink_enabled
                && matches!(
                    occ.spec.ink,
                    Some(InkSpec {
                        colorway: Colorway::Rainbow { .. } | Colorway::TwoTone { .. },
                        ..
                    })
                )
                && (matches!(occ.spec.burst, Some(b) if b.kind == BurstKind::Nova)
                    || (!self.supers.is_empty()
                        && matches!(occ.spec.burst, Some(b) if b.kind == BurstKind::SuperNova))
                    || !self.coupling.is_empty())
            {
                ink_fx(
                    cur_oi,
                    occ,
                    cfg,
                    now,
                    geom,
                    &self.novas,
                    &self.supers,
                    &self.coupling,
                    ep,
                    table,
                )
            } else {
                INK_FX_NONE
            };
            emit_ink(
                table,
                occ,
                cfg,
                now,
                frame,
                &fx,
                &self.ink_base_fg,
                &self.ink_cols,
                ink,
                &mut fp,
                &mut active_until,
            );
            if occ.class == Class::Profanity
                && matches!(
                    occ.spec.ink,
                    Some(InkSpec {
                        colorway: Colorway::Rainbow { .. },
                        ..
                    })
                )
            {
                emit_rainbow_sparkles(occ, cfg, now, frame, geom, out, &mut fp, &mut active_until);
            }
            if out.len() >= MAX_DECORATIONS {
                continue;
            }
            'graphic: {
                if occ.spec.graphic.is_none() {
                    break 'graphic;
                }
                {
                    // v3 §1.1: born-done / born-settled episodes are inert —
                    // the graphic one-shot is pre-spent, zero quads ever.
                    if occ.inert {
                        break 'graphic;
                    }
                    let peek = match ep {
                        Some(e) => PeekView {
                            start: e.phase_start,
                            peek_done: e.peek_done,
                            arm: e.shown_as,
                            unlogged: !e.logged,
                        },
                        None => PeekView {
                            start: Some(occ.appeared),
                            peek_done: false,
                            arm: None,
                            unlogged: false,
                        },
                    };
                    if peek.peek_done {
                        break 'graphic;
                    }
                    if let Some(c) = companion_at
                        && (claim == Some(occ.ident)
                            || caret_on_span(c, occ.row, occ.start_col, occ.end_col))
                    {
                        break 'graphic;
                    }
                    if matches!(occ.spec.graphic, Some(g) if g.collection == Collection::Animals) {
                        let Some(species) = occ.species else {
                            break 'graphic;
                        };
                        let eligible = peek_eligible(occ, geom);
                        let arm = match peek.arm {
                            Some(a) => a,
                            None => {
                                if !eligible {
                                    KittyShownAs::PawFallbackFloor
                                } else if animals < MAX_ANIMALS {
                                    KittyShownAs::Cat
                                } else {
                                    KittyShownAs::PawFallbackOverflow
                                }
                            }
                        };
                        if arm == KittyShownAs::Cat {
                            animals += 1;
                            if eligible {
                                emit_animal(
                                    &mut ctx,
                                    occ,
                                    species,
                                    cfg,
                                    geom,
                                    now,
                                    frame,
                                    peek,
                                    free,
                                    &mut fp,
                                    &mut active_until,
                                );
                            }
                        }
                        break 'graphic;
                    }
                    let eligible = cat_eligible(occ, cfg, geom);
                    let arm = match peek.arm {
                        Some(a) => a,
                        None => {
                            if cfg.feline_style != FelineStyle::Cat {
                                KittyShownAs::PawStyle
                            } else if !eligible {
                                KittyShownAs::PawFallbackFloor
                            } else if cats < MAX_CATS {
                                KittyShownAs::Cat
                            } else {
                                KittyShownAs::PawFallbackOverflow
                            }
                        }
                    };
                    if arm == KittyShownAs::Cat {
                        cats += 1;
                        if eligible {
                            emit_cat(
                                &mut ctx,
                                occ,
                                cfg,
                                geom,
                                now,
                                frame,
                                peek,
                                free,
                                &mut fp,
                                &mut active_until,
                            );
                        }
                    }
                }
            }
            let Some(burst_spec) = occ.spec.burst else {
                continue;
            };
            match burst_spec.kind {
                BurstKind::Nova => {
                    emit_nova_axis(
                        occ,
                        cur_oi,
                        cfg,
                        geom,
                        now,
                        frame,
                        sel,
                        &self.novas,
                        ep,
                        out,
                        nova,
                        &mut fp,
                        &mut active_until,
                    );
                }
                BurstKind::SuperNova => {
                    emit_super_axis(
                        table,
                        occ,
                        cur_oi,
                        cfg,
                        geom,
                        now,
                        frame,
                        sel,
                        &self.supers,
                        ep,
                        out,
                        nova,
                        &mut fp,
                        &mut active_until,
                        &mut nuke_pending,
                    );
                }
                BurstKind::Glow => {
                    if occ.inert || cfg.reduced_motion {
                        continue;
                    }
                    if matches!(ep, Some(e) if !e.burst_roll) {
                        continue;
                    }
                    let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
                    if t < GLOW_BURST_MS && out.len() < MAX_DECORATIONS {
                        const INV_GLOW_BURST_MS: f32 = 1.0 / GLOW_BURST_MS as f32;
                        const STAR_RGB: u32 = crate::effect_util::twinkle_rgb(true);
                        let until = occ.appeared + Duration::from_millis(GLOW_BURST_MS);
                        arm_until(&mut active_until, until);
                        let e = crate::supernova::sin_cos_f32(
                            core::f32::consts::PI * (t as f32 * INV_GLOW_BURST_MS),
                        )
                        .0
                        .max(0.0);
                        let mid = occ.start_col + (occ.end_col - occ.start_col) / 2;
                        let d = WordDecoration {
                            row: occ.row,
                            col: mid,
                            dx: 0,
                            dy: -((i32::from(geom.cell_h) / 3).min(127) as i8),
                            glyph: DecoGlyph::Star4,
                            blend: DecoBlend::Add,
                            color: STAR_RGB,
                            alpha: scale_u8(cfg.intensity * 0.8 * e),
                        };
                        fp = fold_deco(fp, &d) ^ frame.wrapping_mul(0x9E37_79B1);
                        out.push(d);
                    }
                }
                BurstKind::Sparkle | BurstKind::Starburst => {
                    // v3 §1.1: born-done / born-settled sparkles are inert —
                    // the burst played (or resize-settled); zero decos.
                    if occ.inert {
                        continue;
                    }
                    if matches!(ep, Some(e) if !e.burst_roll) {
                        continue;
                    }
                    let elapsed = now.saturating_duration_since(occ.appeared);
                    let animating = !cfg.reduced_motion && elapsed < anim;
                    if animating {
                        let age_s = elapsed.as_secs_f32();
                        let width = u64::from(occ.end_col.saturating_sub(occ.start_col) + 1).max(1);
                        let until = occ.appeared + anim;
                        arm_until(&mut active_until, until);
                        let density = cfg.density.clamp(1, 12);
                        let frame_rot = self.frame.rotate_left(17);
                        let frame_term = self.frame.wrapping_mul(0x9E37_79B1);
                        let glyphs_len = cfg.glyphs.len().max(1);
                        let mut k = 0;
                        while k < density {
                            if out.len() >= MAX_DECORATIONS {
                                break;
                            }
                            let s = mix(occ.seed
                                ^ u64::from(k).wrapping_mul(0xA24B_AED4_963E_E407)
                                ^ frame_rot);
                            let col = occ.start_col + (s % width) as u16;
                            let glyph = match cfg.glyphs.get((s >> 11) as usize % glyphs_len) {
                                Some(&g) => g,
                                None => DecoGlyph::Star4,
                            };
                            let color = pick_color(cfg, s);
                            let env = twinkle(s, age_s);
                            let alpha = scale_u8(cfg.intensity * env);
                            let d = WordDecoration {
                                row: occ.row,
                                col,
                                dx: jitter(cfg.jitter, s),
                                dy: jitter(cfg.jitter, s >> 7),
                                glyph,
                                blend: DecoBlend::Add,
                                color,
                                alpha,
                            };
                            fp = fold_deco(fp, &d) ^ frame_term;
                            out.push(d);
                            k += 1;
                        }
                    } else {
                        let t = elapsed.as_millis() as u64;
                        let f = if cfg.reduced_motion {
                            if matches!(ep, Some(e) if e.burst_done) {
                                None
                            } else {
                                Some(1.0f32)
                            }
                        } else if t < cfg.anim_ms + RESIDUAL_FADE_MS {
                            const INV_RESIDUAL_FADE_MS: f32 = 1.0 / RESIDUAL_FADE_MS as f32;
                            let until = occ.appeared
                                + Duration::from_millis(cfg.anim_ms + RESIDUAL_FADE_MS);
                            arm_until(&mut active_until, until);
                            Some(1.0 - (t - cfg.anim_ms) as f32 * INV_RESIDUAL_FADE_MS)
                        } else {
                            None
                        };
                        if let Some(f) = f {
                            let d = WordDecoration {
                                row: occ.row,
                                col: occ.start_col,
                                dx: 0,
                                dy: 0,
                                glyph: DecoGlyph::Star4,
                                blend: DecoBlend::Add,
                                color: pick_color(cfg, occ.seed),
                                alpha: scale_u8(cfg.intensity * 0.30 * f),
                            };
                            fp = fold_deco(fp, &d);
                            out.push(d);
                        }
                    }
                }
            }
        }
        if let Some((t, cx, cy)) = nuke_pending {
            let grid_w = i32::from(geom.cols) * i32::from(geom.cell_w);
            let base_y = cy + i32::from(geom.cell_h) / 2;
            const NUKE_PARTS: [(usize, crate::nuke::NukePart, bool); 4] = [
                (0, crate::nuke::NukePart::Skirt, false),
                (1, crate::nuke::NukePart::Stem, false),
                (2, crate::nuke::NukePart::Cap, false),
                (2, crate::nuke::NukePart::Cap, true),
            ];
            let mut ni = 0;
            while ni < NUKE_PARTS.len() {
                let (slot, part, core) = NUKE_PARTS[ni];
                ni += 1;
                let d = if core {
                    crate::nuke::nuke_core_draw(t)
                } else {
                    crate::nuke::nuke_draw(t, part)
                };
                let Some(d) = d else {
                    continue;
                };
                let alpha = (d.alpha * f32::from(u8::MAX)) as u8;
                if alpha == 0 {
                    continue;
                }
                let (nw, nh) = crate::nuke::nuke_nat_size(part, geom.cell_w, geom.cell_h);
                let dest_w = ((f32::from(nw) * d.sx).round() as i32).max(1);
                let dest_h = ((f32::from(nh) * d.sy).round() as i32).max(1);
                if dest_w > grid_w {
                    continue;
                }
                let host = crate::nuke::nuke_host_id(part, nw, nh);
                let Some(tile) = NotePaintCache::tile(
                    &mut self.nuke_cache[slot],
                    &mut self.cat_baker,
                    host,
                    (nw, nh),
                    |tile| crate::nuke::paint_nuke(tile, part),
                ) else {
                    continue;
                };
                let dy = (d.dy_cells * f32::from(geom.cell_h)).round() as i32;
                free.push(FreeSprite {
                    x: (cx - dest_w / 2).clamp(0, (grid_w - dest_w).max(0)),
                    y: base_y - dest_h + dy,
                    w: dest_w as u16,
                    h: dest_h as u16,
                    art: tile.art,
                    clip: PxRect::EVERYWHERE,
                    tint: d.tint,
                    alpha,
                    flip_x: false,
                    z: FreeZ::OverText,
                });
            }
        }
        let mut si = 0;
        while si < self.sightings.len() {
            if let Some(ep) = self.persist.get_mut(&self.sightings[si].ident) {
                ep.logged = true;
            }
            si += 1;
        }
        if !free.is_empty() {
            fp = fnv_mix_u64(fp, self.cat_baker.version());
            fp = fnv_mix_u64(fp, self.animal_baker.version());
        }
        let scope = self.scan_scope();
        if let Some(until) =
            self.trick_flash
                .emit(scope, now, trick_flash_paintable(cfg), ink, &mut fp)
        {
            arm_until(&mut active_until, until);
        }
        debug_assert!(
            Self::ink_sorted_unique(ink),
            "ink must be sorted by (row, col) with unique cells"
        );
        self.active_until = active_until;
        fp
    }

    #[inline]
    fn episode_prepass(
        &mut self,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
    ) -> PrepassFlags {
        let mut flags = PrepassFlags::default();
        let mut cat_slots = 0usize;
        let mut animal_slots = 0usize;
        let mut oi = 0;
        while oi < self.occ.len() {
            let occ = &self.occ[oi];
            oi += 1;
            if occ.inert {
                continue;
            }
            let Some(g) = occ.spec.graphic else {
                continue;
            };
            if let Some(e) = self.persist.get(&occ.ident)
                && e.shown_as == Some(KittyShownAs::Cat)
                && !e.peek_done
                && !e.inert()
            {
                if g.collection == Collection::Animals {
                    animal_slots += 1;
                } else {
                    cat_slots += 1;
                }
            }
        }
        let mut oi = 0;
        while oi < self.occ.len() {
            let occ = &self.occ[oi];
            oi += 1;
            if occ.inert {
                continue;
            }
            let Some(ep) = self.persist.get_mut(&occ.ident) else {
                continue;
            };
            flags.has_bonk |= ep.bonk_pending;
            if ep.inert() {
                continue;
            }
            if !ep.nova_done {
                match occ.spec.burst {
                    Some(BurstSpec {
                        kind: BurstKind::SuperNova,
                        ..
                    }) if ep.burst_roll => flags.has_super = true,
                    Some(BurstSpec {
                        kind: BurstKind::Nova,
                        ..
                    }) if ep.burst_roll || ep.nova_start.is_some() => {
                        flags.has_nova = true;
                    }
                    _ => {}
                }
            }
            let mut appeared_ms: Option<u64> = None;
            if cfg.ink_enabled && occ.ink_cells > 0 && occ.spec.ink.is_some() {
                ep.sweep_started = true;
                if !ep.sweep_done {
                    let t = now.saturating_duration_since(ep.appeared).as_millis() as u64;
                    appeared_ms = Some(t);
                    let (win, once) = match occ.spec.ink {
                        Some(InkSpec {
                            colorway: Colorway::SelfGlow { window_ms, .. },
                            ..
                        }) => (u64::from(window_ms), true),
                        Some(InkSpec {
                            colorway: Colorway::Rainbow { drift_ms, .. },
                            ..
                        }) => (u64::from(drift_ms), true),
                        Some(s) => (u64::from(cfg.ink_sweep_ms) + INK_FADE_MS, s.sweep_once),
                        None => (0, true),
                    };
                    if once && t >= win {
                        ep.sweep_done = true;
                    }
                }
            }
            if let Some(g) = occ.spec.graphic
                && !(ep.peek_done
                    && matches!(
                        ep.shown_as,
                        Some(KittyShownAs::Cat | KittyShownAs::PawStyle)
                    ))
            {
                {
                    let is_animal = g.collection == Collection::Animals;
                    let drawable = if is_animal {
                        peek_eligible(occ, geom)
                    } else {
                        cat_eligible(occ, cfg, geom)
                    };
                    if matches!(
                        ep.shown_as,
                        None | Some(
                            KittyShownAs::PawFallbackFloor | KittyShownAs::PawFallbackOverflow
                        )
                    ) {
                        let prior = ep.shown_as;
                        let arm = if !is_animal && cfg.feline_style != FelineStyle::Cat {
                            KittyShownAs::PawStyle
                        } else if !drawable {
                            KittyShownAs::PawFallbackFloor
                        } else if is_animal {
                            if animal_slots < MAX_ANIMALS {
                                animal_slots += 1;
                                KittyShownAs::Cat
                            } else {
                                KittyShownAs::PawFallbackOverflow
                            }
                        } else if cat_slots < MAX_CATS {
                            cat_slots += 1;
                            KittyShownAs::Cat
                        } else {
                            KittyShownAs::PawFallbackOverflow
                        };
                        if prior.is_some() && arm == KittyShownAs::Cat {
                            ep.phase_start = Some(now);
                            ep.peek_pause = None;
                        }
                        ep.shown_as = Some(arm);
                    }
                    match ep.shown_as {
                        Some(KittyShownAs::Cat) if !ep.peek_done => {
                            // Latch the pause on the first ineligible tick; on
                            // the first eligible tick shift `phase_start` by the
                            // paused span so the cat resumes where it vanished.
                            if drawable {
                                if let Some(paused_at) = ep.peek_pause.take()
                                    && let Some(ps) = ep.phase_start
                                {
                                    ep.phase_start =
                                        Some(ps + now.saturating_duration_since(paused_at));
                                }
                            } else if ep.peek_pause.is_none() {
                                ep.peek_pause = Some(now);
                            }
                            if drawable
                                && !cfg.reduced_motion
                                && let Some(ps) = ep.phase_start
                                && now >= ps
                            {
                                ep.peek_started = true;
                                let magical = !is_animal
                                    && ((cfg.feline_magic && cat_magic(ep.genome.magic).is_some())
                                        || has_accessory(ep.genome.magic));
                                let total =
                                    peek_total_ms(occ.ident, ep.genome.gkey, magical, g.dwell_ms);
                                ep.peek_total = total;
                                if now.saturating_duration_since(ps).as_millis() as u64 >= total {
                                    ep.peek_done = true;
                                }
                            }
                        }
                        Some(KittyShownAs::PawStyle) => {
                            // Style says "no cat art": nothing will draw, so the
                            // one-shot completes at once.
                            if !cfg.reduced_motion
                                && let Some(ps) = ep.phase_start
                                && now >= ps
                            {
                                ep.peek_started = true;
                                ep.peek_done = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            match occ.spec.burst {
                Some(BurstSpec {
                    kind: BurstKind::Sparkle | BurstKind::Starburst,
                    ..
                }) if !cfg.reduced_motion && ep.burst_roll && !ep.burst_done => {
                    ep.burst_started = true;
                    let t = appeared_ms.unwrap_or_else(|| {
                        now.saturating_duration_since(ep.appeared).as_millis() as u64
                    });
                    if t >= cfg.anim_ms + RESIDUAL_FADE_MS {
                        ep.burst_done = true;
                    }
                }
                Some(BurstSpec {
                    kind: BurstKind::Glow,
                    ..
                }) if !cfg.reduced_motion && ep.burst_roll && !ep.burst_done => {
                    ep.burst_started = true;
                    let t = appeared_ms.unwrap_or_else(|| {
                        now.saturating_duration_since(ep.appeared).as_millis() as u64
                    });
                    if t >= GLOW_BURST_MS {
                        ep.burst_done = true;
                    }
                }
                _ => {}
            }
        }
        self.stamp_spent_marks(now, cfg.reduced_motion);
        flags
    }

    #[inline]
    fn stamp_spent_marks(&mut self, now: Instant, reduced_motion: bool) {
        let seq = self.rescan_seq;
        let marks = &mut self.done_marks;
        for (ident, ep) in &mut self.persist {
            // Scrolled-off mid-peek completion: `episode_prepass` only walks
            // visible occurrences, so finish such an episode here against its
            // latched `peek_total`. Paused episodes are exempt (the peek resumes
            // if the word returns with clear rows), and reduced motion never
            // advances flags.
            if !reduced_motion
                && ep.seen_seq != seq
                && ep.shown_as == Some(KittyShownAs::Cat)
                && ep.peek_started
                && !ep.peek_done
                && ep.peek_pause.is_none()
                && let Some(ps) = ep.phase_start
                && now.saturating_duration_since(ps).as_millis() as u64 >= ep.peek_total
            {
                ep.peek_done = true;
            }
            if !ep.ledger_stamped && ep.one_shot_started() {
                mark_done(marks, *ident, ep);
                ep.ledger_stamped = true;
            }
        }
    }

    #[inline]
    fn prune_ignitions(&mut self, now: Instant) {
        if self.ignitions.is_empty() {
            return;
        }
        let persist = &self.persist;
        let scope = self.scan_scope();
        self.ignitions.retain(|reservation| {
            if reservation.start + IGNITION_WINDOW <= now {
                return false;
            }
            if reservation.start <= now {
                return true;
            }
            if reservation.pane != scope {
                return true;
            }
            matches!(
                persist.get(&reservation.owner),
                Some(episode)
                    if !episode.nova_done && episode.nova_start == Some(reservation.start)
            )
        });
        debug_assert!({
            let mut count = 0usize;
            let mut i = 0;
            while i < self.ignitions.len() {
                if self.ignitions[i].pane == scope {
                    count += 1;
                }
                i += 1;
            }
            count <= MAX_IGNITION_RESERVATIONS
        });
    }

    #[inline]
    fn super_prepass(
        &mut self,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        sel: Option<SelView<'_>>,
        has_super: bool,
    ) -> Option<Instant> {
        self.supers.clear();
        self.super_until = None;
        self.prune_ignitions(now);
        if cfg.reduced_motion {
            return None;
        }
        let mut until: Option<Instant> = None;
        let mut busy = false;
        if matches!(self.burst_hint, Some(h) if now < h) {
            for ep in self.persist.values() {
                let Some(end) = burst_mutex_end(ep) else {
                    continue;
                };
                if now >= end {
                    continue;
                }
                busy = true;
                arm_until(&mut until, end);
                if matches!(ep.burst_kind, Some(BurstKind::SuperNova)) {
                    arm_until(&mut self.super_until, end);
                }
            }
        } else {
            debug_assert!(
                !self
                    .persist
                    .values()
                    .any(|ep| matches!(burst_mutex_end(ep), Some(end) if now < end)),
                "a live burst window outlived the burst-mutex hint"
            );
        }
        for p in self.parked.values() {
            debug_assert!(
                p.burst_summary() == (p.burst_end_max, p.super_end_max),
                "a parked shard's burst-mutex summary drifted from its episode map"
            );
            if let Some(e) = p.burst_end_max
                && now < e
            {
                busy = true;
                arm_until(&mut until, e);
            }
            if let Some(e) = p.super_end_max
                && now < e
            {
                arm_until(&mut self.super_until, e);
            }
        }
        if !has_super {
            return until;
        }
        let scope = self.scan_scope();
        let ch = i32::from(geom.cell_h);
        let grid_h = i32::from(geom.rows) * ch;
        let r_max = supernova::r_max_for(ch, grid_h);
        let mut i = 0;
        while i < self.occ.len() {
            let occ = &self.occ[i];
            if !matches!(occ.spec.burst, Some(b) if b.kind == BurstKind::SuperNova) {
                i += 1;
                continue;
            }
            let Some(ep) = self.persist.get_mut(&occ.ident) else {
                i += 1;
                continue;
            };
            if !ep.burst_roll || ep.nova_done || ep.inert() {
                i += 1;
                continue;
            }
            let mut center_px = None;
            if ep.nova_start.is_none() {
                // Deferred while selected (§3.2 safety) or while another
                // supernova holds the mutex (MAX_ACTIVE_SUPERNOVAE = 1).
                if busy
                    || matches!(sel, Some(sv) if sv.span_selected(occ.row, occ.start_col, occ.end_col))
                {
                    i += 1;
                    continue;
                }
                let c = burst_center(occ, geom);
                center_px = Some(c);
                if !grant_burst(
                    &mut self.ignitions,
                    scope,
                    self.pane_px,
                    occ,
                    ep,
                    now,
                    c,
                    r_max,
                    &mut self.burst_hint,
                    &mut self.curse_cues,
                ) {
                    i += 1;
                    continue;
                }
            }
            let Some(start) = ep.nova_start else {
                i += 1;
                continue;
            };
            let win = super_window_of(ep.burst_tier);
            if now >= start + win {
                ep.nova_done = true;
                i += 1;
                continue;
            }
            busy = true;
            let end = start + win;
            arm_until(&mut until, end);
            arm_until(&mut self.super_until, end);
            if now < start {
                i += 1;
                continue;
            }
            if self.supers.len() < supernova::MAX_ACTIVE_SUPERNOVAE {
                self.supers.push(SuperLive {
                    idx: i as u16,
                    start,
                    center_px: center_px.unwrap_or_else(|| burst_center(occ, geom)),
                    r_max,
                });
            }
            i += 1;
        }
        until
    }

    #[inline]
    fn nova_prepass(
        &mut self,
        now: Instant,
        cfg: &DecoConfig,
        geom: EffectGeom,
        sel: Option<SelView<'_>>,
        has_nova: bool,
    ) -> Option<Instant> {
        self.novas.clear();
        self.coupling.clear();
        if cfg.reduced_motion || !has_nova {
            return None;
        }
        let super_busy = matches!(self.super_until, Some(t) if now < t);
        let super_wake = self.super_until.map(|su| su + Duration::from_millis(1));
        let mut until: Option<Instant> = None;
        let scope = self.scan_scope();
        let ch_f = i32::from(geom.cell_h).max(1) as f32;
        let mut i = 0;
        while i < self.occ.len() {
            let occ = &self.occ[i];
            // v3 §6 dispatch: the classic nova is the `BurstKind::Nova` arm
            // (profanity `style = "nova"` and custom specs alike).
            if !matches!(occ.spec.burst, Some(b) if b.kind == BurstKind::Nova) {
                i += 1;
                continue;
            }
            let Some(ep) = self.persist.get_mut(&occ.ident) else {
                i += 1;
                continue;
            };
            if ep.nova_done {
                i += 1;
                continue;
            }
            let mut center_r_max = None;
            if ep.nova_start.is_none() {
                // `chance_pct`: a rolled-off burst never ignites (episodes with
                // a pre-granted `nova_start` are past the roll).
                if !ep.burst_roll {
                    i += 1;
                    continue;
                }
                if matches!(sel, Some(sv) if sv.span_selected(occ.row, occ.start_col, occ.end_col))
                {
                    i += 1;
                    continue;
                }
                if super_busy {
                    if let Some(wake) = super_wake {
                        arm_until(&mut until, wake);
                    }
                    i += 1;
                    continue;
                }
                let c = burst_center(occ, geom);
                let r_max = nova_radius(occ.genome.gkey) * ch_f;
                center_r_max = Some((c, r_max));
                if !grant_burst(
                    &mut self.ignitions,
                    scope,
                    self.pane_px,
                    occ,
                    ep,
                    now,
                    c,
                    r_max,
                    &mut self.burst_hint,
                    &mut self.curse_cues,
                ) {
                    i += 1;
                    continue;
                }
            }
            let Some(start) = ep.nova_start else {
                i += 1;
                continue;
            };
            let d = Duration::from_millis(u64::from(nova_duration_ms(occ.genome.gkey)));
            if now >= start + d {
                ep.nova_done = true;
                i += 1;
                continue;
            }
            let end = start + d;
            arm_until(&mut until, end);
            if now < start {
                i += 1;
                continue;
            }
            if self.novas.len() >= nova::MAX_ACTIVE_NOVAS {
                ep.nova_done = true;
                i += 1;
                continue;
            }
            let (center_px, r_max) = center_r_max
                .unwrap_or_else(|| (burst_center(occ, geom), nova_radius(occ.genome.gkey) * ch_f));
            self.novas.push(NovaLive {
                idx: i as u16,
                start,
                center_px,
                r_max,
                feats: nova_features(occ.genome.gkey),
                magic: if cfg.profanity_magic {
                    nova_magic(occ.genome.magic)
                } else {
                    None
                },
            });
            i += 1;
        }
        let mut ni = 0;
        while ni < self.novas.len() {
            let nv = self.novas[ni];
            self.dist_scratch.clear();
            let mut i = 0;
            while i < self.occ.len() {
                let occ = &self.occ[i];
                if i != usize::from(nv.idx) && occ.ink_cells != 0 {
                    self.dist_scratch
                        .push((span_dist2(occ, nv.center_px, geom), i as u16));
                }
                i += 1;
            }
            let take_n = self.dist_scratch.len().min(nova::MAX_COUPLING_WORDS);
            if self.dist_scratch.len() > nova::MAX_COUPLING_WORDS {
                self.dist_scratch
                    .select_nth_unstable(nova::MAX_COUPLING_WORDS - 1);
            }
            self.dist_scratch[..take_n].sort_unstable();
            let mut di = 0;
            while di < take_n {
                self.coupling.push((self.dist_scratch[di].1, ni as u8));
                di += 1;
            }
            ni += 1;
        }
        until
    }

    /// Whether any occurrence is still inside its animation window (profanity sparkle, a live ink sweep, or a granted, possibly queued, nova window).
    #[inline]
    #[must_use]
    pub fn is_active(&self, now: Instant) -> bool {
        if let Some(d) = self.active_until
            && now < d
        {
            return true;
        }
        for p in self.parked.values() {
            if let Some(d) = p.active_until
                && now < d
            {
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Copy, Default)]
struct PrepassFlags {
    has_super: bool,
    has_nova: bool,
    has_bonk: bool,
}

#[inline]
const fn burst_center(occ: &Occurrence, geom: EffectGeom) -> (i32, i32) {
    let advance = geom.cell_w as i32 * if occ.dec_line { 2 } else { 1 };
    let ch = geom.cell_h as i32;
    let cx = (occ.start_col as i32 + occ.end_col as i32 + 1) * advance / 2;
    let cy = occ.row as i32 * ch + ch / 2;
    (cx, cy)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the prepass's disjoint field borrows, split around its live `&mut persist` entry"
)]
#[inline]
fn grant_burst(
    igns: &mut Vec<IgnitionReservation>,
    scope: Option<u64>,
    pane_px: (i32, i32),
    occ: &Occurrence,
    ep: &mut Episode,
    now: Instant,
    center: (i32, i32),
    r_max: f32,
    burst_hint: &mut Option<Instant>,
    curse_cues: &mut Vec<CurseCue>,
) -> bool {
    let Some(start) =
        grant_pane_ignition(igns, scope, pane_px, occ.ident, now, center, 2.0 * r_max)
    else {
        return false;
    };
    ep.nova_start = Some(start);
    if let Some(end) = burst_mutex_end(ep) {
        arm_until(burst_hint, end);
    }
    ep.burst_started = true;
    ep.burst_done = true;
    if occ.class == Class::Profanity && curse_cues.len() < MAX_CURSE_CUES {
        curse_cues.push(CurseCue {
            kind: CurseCueKind::Detonated,
            row: occ.row,
            col: occ.start_col,
        });
    }
    true
}

#[inline]
fn grant_pane_ignition(
    igns: &mut Vec<IgnitionReservation>,
    pane: Option<u64>,
    pane_px: (i32, i32),
    owner: u64,
    now: Instant,
    center: (i32, i32),
    overlap_dist: f32,
) -> Option<Instant> {
    grant_ignition(
        igns,
        pane,
        owner,
        now,
        (center.0 + pane_px.0, center.1 + pane_px.1),
        overlap_dist,
    )
}

#[inline]
fn grant_ignition(
    igns: &mut Vec<IgnitionReservation>,
    pane: Option<u64>,
    owner: u64,
    now: Instant,
    center: (i32, i32),
    overlap_dist: f32,
) -> Option<Instant> {
    // Memory bound, per scope (each pane or declared session holds its own
    // episodes). The safety bound is the rolling-window count below, over
    // every scope.
    let mut pane_count = 0usize;
    let mut ri = 0;
    while ri < igns.len() {
        if igns[ri].pane == pane {
            pane_count += 1;
            if pane_count >= MAX_IGNITION_RESERVATIONS {
                return None;
            }
        }
        ri += 1;
    }
    let overlap_dist2 = overlap_dist * overlap_dist;
    let check_overlap = overlap_dist > 0.0;
    let mut slot = now;
    loop {
        let mut count = 0usize;
        let mut overlap = false;
        let mut next_free: Option<Instant> = None;
        let mut ri = 0;
        while ri < igns.len() {
            let reservation = &igns[ri];
            let (t, c) = (reservation.start, reservation.center);
            if t > slot || t + IGNITION_WINDOW <= slot {
                ri += 1;
                continue;
            }
            count += 1;
            let (dx, dy) = ((c.0 - center.0) as f32, (c.1 - center.1) as f32);
            if !overlap && check_overlap && dx * dx + dy * dy < overlap_dist2 {
                overlap = true;
            }
            let free = t + IGNITION_WINDOW;
            next_free = Some(match next_free {
                Some(f) => f.min(free),
                None => free,
            });
            ri += 1;
        }
        let cap = if overlap { 1 } else { 2 };
        if count < cap {
            igns.push(IgnitionReservation {
                pane,
                owner,
                start: slot,
                center,
            });
            return Some(slot);
        }
        slot = next_free?;
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, the emit_ink idiom"
)]
#[inline]
fn emit_nova_axis(
    occ: &Occurrence,
    oi: usize,
    cfg: &DecoConfig,
    geom: EffectGeom,
    now: Instant,
    frame: u64,
    sel: Option<SelView<'_>>,
    novas: &[NovaLive],
    ep: Option<&Episode>,
    out: &mut Vec<WordDecoration>,
    nova: &mut Vec<GlowQuad>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    let mut found_nv = None;
    let mut ni = 0;
    while ni < novas.len() {
        let n = novas[ni];
        if usize::from(n.idx) == oi {
            found_nv = Some(n);
            break;
        }
        ni += 1;
    }
    if let Some(nv) = found_nv {
        let t = now.saturating_duration_since(nv.start).as_millis() as u64;
        let until = nv.start + Duration::from_millis(u64::from(nv.feats.duration_ms));
        arm_until(active_until, until);
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        let env = nova_env(occ, &nv, geom, cfg);
        let budget = nova::MAX_NOVA_QUADS_PER.min(nova::MAX_NOVA_QUADS.saturating_sub(nova.len()));
        let n0 = nova.len();
        nova::emit_nova(t, &env, budget, nova);
        if let Some(sv) = sel
            && sv.sel.has_selection()
        {
            let cw = i32::from(geom.cell_w).max(1);
            let mut w = n0;
            let mut r = n0;
            while r < nova.len() {
                let q = nova[r];
                let ccol = ((i32::from(q.x) + i32::from(q.w) / 2) / cw).max(0) as u16;
                if !sv.cell_selected(q.row, ccol) {
                    *fp = fold_glow(*fp, &q);
                    nova[w] = q;
                    w += 1;
                }
                r += 1;
            }
            nova.truncate(w);
        } else {
            let mut qi = n0;
            while qi < nova.len() {
                *fp = fold_glow(*fp, &nova[qi]);
                qi += 1;
            }
        }
        let d0 = out.len();
        nova::emit_debris(t, &env, out, MAX_DECORATIONS);
        if nv.magic == Some(NovaMagic::Singularity) {
            let cap = nova::MAX_RING_ARC_CELLS.min(MAX_DECORATIONS.saturating_sub(out.len()));
            nova::emit_ring_arc(t, &env, out, cap);
        }
        let mut di = d0;
        while di < out.len() {
            *fp = fold_deco(*fp, &out[di]);
            di += 1;
        }
    } else {
        let done = match ep {
            Some(e) => e.nova_done,
            None => true,
        };
        let fade = if cfg.reduced_motion && !occ.inert {
            let rolled_off = matches!(ep, Some(e) if !e.burst_roll);
            let spent = if let Some(e) = ep
                && e.nova_done
                && let Some(s) = e.nova_start
            {
                now >= s + Duration::from_millis(
                    u64::from(nova_duration_ms(occ.genome.gkey)) + RESIDUAL_FADE_MS,
                )
            } else {
                false
            };
            if !rolled_off && !spent {
                Some(1.0f32)
            } else {
                None
            }
        } else if done && !occ.inert {
            if let Some(e) = ep
                && let Some(s) = e.nova_start
            {
                let end = s + Duration::from_millis(u64::from(nova_duration_ms(occ.genome.gkey)));
                let t = now.saturating_duration_since(end).as_millis() as u64;
                if t < RESIDUAL_FADE_MS {
                    const INV_RESIDUAL_FADE_MS: f32 = 1.0 / RESIDUAL_FADE_MS as f32;
                    let f = 1.0 - t as f32 * INV_RESIDUAL_FADE_MS;
                    let until = end + Duration::from_millis(RESIDUAL_FADE_MS);
                    arm_until(active_until, until);
                    Some(f)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(f) = fade {
            let magic = if cfg.profanity_magic {
                nova_magic(occ.genome.magic)
            } else {
                None
            };
            let (_, ember) = nova::ember_pair(nova::palette(nova_palette(occ.genome.gkey)), magic);
            let d = WordDecoration {
                row: occ.row,
                col: occ.start_col,
                dx: 0,
                dy: -((i32::from(geom.cell_h) / 3).min(127) as i8),
                glyph: DecoGlyph::Star4,
                blend: DecoBlend::Add,
                color: ember,
                alpha: scale_u8(cfg.intensity * 0.30 * f),
            };
            *fp = fold_deco(*fp, &d);
            out.push(d);
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, the emit_ink idiom"
)]
#[inline]
fn emit_super_axis(
    table: &crate::color_math::RelativeLuminanceTable,
    occ: &Occurrence,
    oi: usize,
    cfg: &DecoConfig,
    geom: EffectGeom,
    now: Instant,
    frame: u64,
    sel: Option<SelView<'_>>,
    supers: &[SuperLive],
    ep: Option<&Episode>,
    out: &mut Vec<WordDecoration>,
    nova: &mut Vec<GlowQuad>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
    nuke_pending: &mut Option<(u64, i32, i32)>,
) {
    if occ.inert {
        return;
    }
    let mut found_sv = None;
    let mut si = 0;
    while si < supers.len() {
        let s = supers[si];
        if usize::from(s.idx) == oi {
            found_sv = Some(s);
            break;
        }
        si += 1;
    }
    if let Some(sv) = found_sv {
        let t = now.saturating_duration_since(sv.start).as_millis() as u64;
        let tier = match ep {
            Some(e) => e.burst_tier,
            None => supernova::SuperTier::Nova,
        };
        let until = sv.start + Duration::from_millis(supernova::total_ms(tier));
        arm_until(active_until, until);
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        let light = crate::color_math::relative_luminance_rgb(
            table,
            occ.ink_bg[0] as usize,
            occ.ink_bg[1] as usize,
            occ.ink_bg[2] as usize,
        ) > 0.5;
        let advance = i32::from(geom.cell_w) * if occ.dec_line { 2 } else { 1 };
        let env = SuperEnv {
            grid_w: i32::from(geom.cols) * i32::from(geom.cell_w),
            grid_h: i32::from(geom.rows) * i32::from(geom.cell_h),
            cell_w: advance.max(1),
            cell_h: i32::from(geom.cell_h).max(1),
            cx: sv.center_px.0,
            cy: sv.center_px.1,
            r_max: sv.r_max,
            row: occ.row,
            start_col: occ.start_col,
            end_col: occ.end_col,
            cols: geom.cols,
            light,
            intensity: cfg.intensity,
            seed: occ.seed,
            base_hue: rainbow_base_hue(occ.genome.gkey),
        };
        let budget =
            supernova::MAX_SUPER_QUADS_PER.min(nova::MAX_NOVA_QUADS.saturating_sub(nova.len()));
        let n0 = nova.len();
        supernova::emit_super(t, &env, budget, nova);
        if tier == supernova::SuperTier::Nuke && !cfg.reduced_motion {
            *nuke_pending = Some((t, env.cx, env.cy));
        }
        if let Some(sv_sel) = sel
            && sv_sel.sel.has_selection()
        {
            split_super_selection(nova, n0, geom, &sv_sel);
        }
        let mut qi = n0;
        while qi < nova.len() {
            *fp = fold_glow(*fp, &nova[qi]);
            qi += 1;
        }
        let d0 = out.len();
        supernova::emit_super_decos(t, &env, out, MAX_DECORATIONS);
        if let Some(sv_sel) = sel
            && sv_sel.sel.has_selection()
        {
            let mut w = d0;
            let mut r = d0;
            while r < out.len() {
                let dq = out[r];
                if !(matches!(dq.blend, DecoBlend::Over) && sv_sel.cell_selected(dq.row, dq.col)) {
                    *fp = fold_deco(*fp, &dq);
                    out[w] = dq;
                    w += 1;
                }
                r += 1;
            }
            out.truncate(w);
        } else {
            let mut di = d0;
            while di < out.len() {
                *fp = fold_deco(*fp, &out[di]);
                di += 1;
            }
        }
    } else if !cfg.reduced_motion
        && let Some(ep) = ep
        && ep.burst_roll
        && let Some(s) = ep.nova_start
    {
        let end = s + Duration::from_millis(supernova::total_ms(ep.burst_tier));
        if now >= end {
            let t = now.saturating_duration_since(end).as_millis() as u64;
            if t < RESIDUAL_FADE_MS {
                const INV_RESIDUAL_FADE_MS: f32 = 1.0 / RESIDUAL_FADE_MS as f32;
                const STAR_RGB: u32 = crate::effect_util::twinkle_rgb(true);
                let f = 1.0 - t as f32 * INV_RESIDUAL_FADE_MS;
                let until = end + Duration::from_millis(RESIDUAL_FADE_MS);
                arm_until(active_until, until);
                let d = WordDecoration {
                    row: occ.row,
                    col: occ.start_col,
                    dx: 0,
                    dy: -((i32::from(geom.cell_h) / 3).min(127) as i8),
                    glyph: DecoGlyph::Star4,
                    blend: DecoBlend::Add,
                    color: STAR_RGB,
                    alpha: scale_u8(cfg.intensity * 0.30 * f),
                };
                *fp = fold_deco(*fp, &d);
                out.push(d);
            }
        }
    }
}

#[inline]
fn split_super_selection(quads: &mut Vec<GlowQuad>, n0: usize, geom: EffectGeom, sv: &SelView<'_>) {
    let cw = i32::from(geom.cell_w).max(1);
    let cols = i32::from(geom.cols).max(1);
    let mut i = n0;
    while i < quads.len() {
        let q = quads[i];
        let c0 = (i32::from(q.x) / cw).clamp(0, cols - 1);
        let c1 = ((i32::from(q.x) + i32::from(q.w) - 1) / cw).clamp(0, cols - 1);
        let (mut s0, mut s1) = (None, None);
        let mut c = c0;
        while c <= c1 {
            if sv.cell_selected(q.row, c as u16) {
                if s0.is_none() {
                    s0 = Some(c);
                }
                s1 = Some(c);
            }
            c += 1;
        }
        let (Some(s0), Some(s1)) = (s0, s1) else {
            i += 1;
            continue;
        };
        let (hx0, hx1) = (s0 * cw, (s1 + 1) * cw);
        let (qx0, qx1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        debug_assert!(q.is_flat(), "a split nova quad must be flat");
        let mut replaced = false;
        if hx0 > qx0 {
            quads[i] = GlowQuad {
                x: q.x,
                w: (hx0 - qx0).min(i32::from(u16::MAX)) as u16,
                ..q
            };
            replaced = true;
        }
        if hx1 < qx1 {
            let right = GlowQuad {
                x: hx1.max(0) as u16,
                w: (qx1 - hx1).min(i32::from(u16::MAX)) as u16,
                ..q
            };
            if replaced {
                quads.insert(i + 1, right);
                i += 1;
            } else {
                quads[i] = right;
                replaced = true;
            }
        }
        if replaced {
            i += 1;
        } else {
            quads.remove(i);
        }
    }
}

#[inline]
const fn span_dist2(occ: &Occurrence, center: (i32, i32), geom: EffectGeom) -> u64 {
    let advance = geom.cell_w as i32 * if occ.dec_line { 2 } else { 1 };
    let ch = geom.cell_h as i32;
    let x0 = occ.start_col as i32 * advance;
    let x1 = (occ.end_col as i32 + 1) * advance;
    let y = occ.row as i32 * ch + ch / 2;
    let dx0 = x0 - center.0;
    let dx1 = center.0 - x1;
    let dx_max = if dx0 > dx1 { dx0 } else { dx1 };
    let dx = if dx_max > 0 { dx_max } else { 0 };
    let dy = (center.1 - y).abs();
    ((dx as i64) * (dx as i64) + (dy as i64) * (dy as i64)) as u64
}

const NUDGED_NOVA_TABLE: [([u32; 16], [u32; 16]); 8] = {
    let mut out = [([0u32; 16], [0u32; 16]); 8];
    let mut pi = 0;
    while pi < 8 {
        let (core, fringe) = nova::palette(pi as u8);
        let mut n = 0;
        while n < 16 {
            out[pi].0[n] = hue_nudge(core, n as u8);
            out[pi].1[n] = hue_nudge(fringe, n as u8);
            n += 1;
        }
        pi += 1;
    }
    out
};

#[inline]
const fn nudged_nova_pair(palette: u8, gkey: u64) -> (u32, u32) {
    let (n0, n1) = genome::ink_pair_nudges(Class::Profanity, gkey);
    let (cores, fringes) = &NUDGED_NOVA_TABLE[(palette as usize) & 7];
    (cores[(n0 & 0xF) as usize], fringes[(n1 & 0xF) as usize])
}

#[inline]
const fn nova_env(occ: &Occurrence, nv: &NovaLive, geom: EffectGeom, cfg: &DecoConfig) -> NovaEnv {
    let advance = (geom.cell_w as i32) * if occ.dec_line { 2 } else { 1 };
    let (core, fringe) = nudged_nova_pair(nv.feats.palette, occ.genome.gkey);
    let cell_w = if advance > 1 { advance } else { 1 };
    let ch = geom.cell_h as i32;
    let cell_h = if ch > 1 { ch } else { 1 };
    NovaEnv {
        grid_w: (geom.cols as i32) * (geom.cell_w as i32),
        grid_h: (geom.rows as i32) * ch,
        cell_w,
        cell_h,
        cx: nv.center_px.0,
        cy: nv.center_px.1,
        r_max: nv.r_max,
        feats: nv.feats,
        magic: nv.magic,
        core,
        fringe,
        intensity: cfg.intensity,
        seed: occ.seed,
    }
}

struct InkFx {
    pair: Option<(u32, u32)>,
    dim: f32,
    freeze: bool,
    frame_live: bool,
    lift: f32,
}

const INK_FX_NONE: InkFx = InkFx {
    pair: None,
    dim: 1.0,
    freeze: false,
    frame_live: false,
    lift: 0.0,
};

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence resolution over tick-local nova scratch, the emit_ink idiom"
)]
#[inline]
fn ink_fx(
    oi: usize,
    occ: &Occurrence,
    cfg: &DecoConfig,
    now: Instant,
    geom: EffectGeom,
    novas: &[NovaLive],
    supers: &[SuperLive],
    coupling: &[(u16, u8)],
    ep: Option<&Episode>,
    lum_table: &crate::color_math::RelativeLuminanceTable,
) -> InkFx {
    let mut fx = INK_FX_NONE;
    let mut si = 0;
    while si < supers.len() {
        let s = &supers[si];
        if usize::from(s.idx) != oi {
            si += 1;
            continue;
        }
        let t = now.saturating_duration_since(s.start).as_millis() as u64;
        const INV_CHARGE_END_MS: f32 = 1.0 / supernova::CHARGE_END_MS as f32;
        const INV_DETONATION_FADE_MS: f32 =
            1.0 / (supernova::DEBRIS_START_MS - supernova::DETONATION_END_MS) as f32;
        let lift = if t < supernova::CHARGE_END_MS {
            t as f32 * INV_CHARGE_END_MS
        } else if t < supernova::DETONATION_END_MS {
            1.0
        } else if t < supernova::DEBRIS_START_MS {
            1.0 - (t - supernova::DETONATION_END_MS) as f32 * INV_DETONATION_FADE_MS
        } else {
            0.0
        };
        if lift > 0.0 {
            fx.lift = lift;
            fx.frame_live = true;
        }
        break;
    }
    if matches!(occ.spec.burst, Some(b) if b.kind == BurstKind::Nova) {
        let pair = nudged_nova_pair(nova_palette(occ.genome.gkey), occ.genome.gkey);
        let done = match ep {
            Some(e) => e.nova_done,
            None => true,
        };
        if cfg.reduced_motion || done {
            let magic = if cfg.profanity_magic {
                nova_magic(occ.genome.magic)
            } else {
                None
            };
            fx.pair = Some(nova::ember_pair(pair, magic));
        } else {
            fx.pair = Some(pair);
            if let Some(e) = ep
                && let Some(start) = e.nova_start
                && now >= start
            {
                let t = now.saturating_duration_since(start).as_millis() as u64;
                if t < u64::from(nova_duration_ms(occ.genome.gkey)) {
                    fx.freeze = true;
                    fx.dim = nova::dip_envelope(t);
                    fx.frame_live = t < nova::DIP_MS;
                }
            }
        }
    }
    let mut cpi = 0;
    while cpi < coupling.len() {
        let (ci, ni) = coupling[cpi];
        if usize::from(ci) != oi {
            cpi += 1;
            continue;
        }
        let nv = &novas[usize::from(ni)];
        let t = now.saturating_duration_since(nv.start).as_millis() as u64;
        let dist = crate::color_math::sqrt_f32(span_dist2(occ, nv.center_px, geom) as f32);
        let sing = nv.magic == Some(NovaMagic::Singularity);
        if let Some(cross) = nova::crossing_ms(dist, nv.r_max, sing)
            && let Some(env) = nova::pulse_env(t, cross)
        {
            let base = match fx.pair {
                Some(p) => Some(p),
                None => nudged_ink_pair(occ.class, occ.genome.gkey),
            };
            if let Some((c0, c1)) = base {
                let (_, fringe) = nova::palette(nv.feats.palette);
                let amp = nova::PULSE_AMP * env;
                let advance = i32::from(geom.cell_w) * if occ.dec_line { 2 } else { 1 };
                let word_mid =
                    (i32::from(occ.start_col) + i32::from(occ.end_col) + 1) * advance / 2;
                let (a0, a1) = if sing {
                    if nv.center_px.0 >= word_mid {
                        (amp * 0.25, amp)
                    } else {
                        (amp, amp * 0.25)
                    }
                } else {
                    (amp, amp)
                };
                fx.pair = Some((
                    nova::pulse_color_with(lum_table, c0, fringe, a0),
                    nova::pulse_color_with(lum_table, c1, fringe, a1),
                ));
                fx.frame_live = true;
            }
            break;
        }
        cpi += 1;
    }
    fx
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CatGeom {
    w: u16,
    hart: u16,
    chin: u16,
    /// The art's left edge in grid px. `i32`, as the sprite it places is: a grid of 4096
    /// columns of 20 px puts a word past 65,535 px, where a `u16` edge wrapped the cat to the
    /// far side of the frame.
    x: i32,
}

/// §5.2 base art height before age scaling: `Hart = round(1.7·ch)`.
#[must_use]
#[inline]
pub const fn cat_hart(cell_h: u16) -> u16 {
    let h = (1.7 * (cell_h as f32)).round() as u16;
    if h > 1 { h } else { 1 }
}

/// §5.2 chin slice: `floor(0.15·Hart)` — the part spilling into the word row (occluded by the word's ascenders: the cat is visibly behind the word).
#[must_use]
#[inline]
pub const fn cat_chin(hart: u16) -> u16 {
    (0.15 * (hart as f32)).floor() as u16
}

#[inline]
const fn authored_cat_size(variant: CatGlyphId, desired_h: f32, cell_h: u16) -> (u16, u16) {
    let max_w = cell_h.saturating_mul(4).saturating_sub(PATCH_STRIP);
    fit_glyph(&GLYPHS[variant as usize], desired_h, max_w, cell_h)
}

#[inline]
const fn fit_glyph(glyph: &GlyphDef, desired_h: f32, max_w: u16, cell_h: u16) -> (u16, u16) {
    const INV_1000: f32 = 1.0 / 1000.0;
    let aspect = ((glyph.aspect_x1000 as f32) * INV_1000).max(0.001);
    let max_w = (if max_w > 1 { max_w } else { 1 }) as f32;
    let ch2 = cell_h.saturating_mul(2);
    let max_h = (if ch2 > 1 { ch2 } else { 1 }) as f32;
    let fitted_h = desired_h.max(1.0).min(max_h).min(max_w / aspect);
    let h = fitted_h.round().clamp(1.0, max_h) as u16;
    let w = ((h as f32) * aspect).round().clamp(1.0, max_w) as u16;
    (w, h)
}

/// §5.6 rest reveal: full Hart for every identity — bobs and entrances terminate at the same fixed point for everyone.
#[must_use]
#[inline]
pub const fn cat_rest_reveal(hart: u16) -> u16 {
    hart
}

#[inline]
const fn cat_eligible(occ: &Occurrence, cfg: &DecoConfig, geom: EffectGeom) -> bool {
    matches!(cfg.feline_style, FelineStyle::Cat) && peek_eligible(occ, geom)
}

#[inline]
const fn peek_eligible(occ: &Occurrence, geom: EffectGeom) -> bool {
    if geom.cell_h < CAT_MIN_CELL_H
        || geom.cell_w < CAT_MIN_CELL_W
        || occ.dec_line
        || !occ.cat_text_clear
    {
        return false;
    }
    let word_px = (occ.end_col as i32 - occ.start_col as i32 + 1) * geom.cell_w as i32;
    (word_px as f32) >= 0.8 * geom.cell_h as f32
}

#[inline]
const fn cat_geometry(occ: &Occurrence, geom: EffectGeom, variant: CatGlyphId) -> CatGeom {
    cat_geometry_for(occ.start_col, occ.end_col, occ.genome.gkey, geom, variant)
}

#[inline]
const fn cat_geometry_for(
    start_col: u16,
    end_col: u16,
    gkey: u64,
    geom: EffectGeom,
    variant: CatGlyphId,
) -> CatGeom {
    let desired_h = (cat_hart(geom.cell_h) as f32) * cat_age_v4(gkey).scale();
    let size = authored_cat_size(variant, desired_h, geom.cell_h);
    anchored_geom(start_col, end_col, geom, size, &GLYPHS[variant as usize])
}

#[inline]
const fn anchored_geom(
    start_col: u16,
    end_col: u16,
    geom: EffectGeom,
    (w, hart): (u16, u16),
    glyph: &GlyphDef,
) -> CatGeom {
    const INV_FIXED_ONE: f32 = 1.0 / (FIXED_ONE as f32);
    let cw = geom.cell_w as i32;
    let w_i = w as i32;
    let grid_w = (geom.cols as i32) * cw;
    let mid = ((start_col as i32) * cw + ((end_col as i32) + 1) * cw) / 2;
    let center = (glyph.center_x as f32) * INV_FIXED_ONE;
    let max_x = if grid_w > w_i { grid_w - w_i } else { 0 };
    let raw_x = mid - (center * (w as f32)).round() as i32;
    let x = if raw_x < 0 {
        0
    } else if raw_x > max_x {
        max_x
    } else {
        raw_x
    };
    CatGeom {
        w,
        hart,
        chin: cat_chin(hart),
        x,
    }
}

#[inline]
const fn animal_geometry(occ: &Occurrence, geom: EffectGeom, species: AnimalGlyphId) -> CatGeom {
    let glyph = &ANIMAL_GLYPHS[species as usize];
    let max_w = geom.cell_h.saturating_mul(4);
    let size = fit_glyph(glyph, cat_hart(geom.cell_h) as f32, max_w, geom.cell_h);
    anchored_geom(occ.start_col, occ.end_col, geom, size, glyph)
}

#[inline]
const fn ease_out_back_c(p: f32, c: f32) -> f32 {
    let u = p - 1.0;
    1.0 + (c + 1.0) * u * u * u + c * u * u
}

#[inline]
const fn overshoot_c(amp: f32) -> f32 {
    let (mut lo, mut hi) = (0.0f32, 6.0f32);
    let mut i = 0;
    while i < 24 {
        let mid = 0.5 * (lo + hi);
        let over = 4.0 * mid * mid * mid / (27.0 * (mid + 1.0) * (mid + 1.0));
        if over < amp {
            lo = mid;
        } else {
            hi = mid;
        }
        i += 1;
    }
    0.5 * (lo + hi)
}

const PEEK_OVERSHOOT_C: [[f32; 4]; 2] = {
    let mut table = [[0.0f32; 4]; 2];
    let mut k = 0;
    while k < 2 {
        let mult = if k == 1 { 1.3f32 } else { 1.0f32 };
        let mut f = 0;
        while f < 4 {
            let overshoot = 0.06 + f as f32 * 0.04;
            table[k][f] = overshoot_c(overshoot * mult);
            f += 1;
        }
        k += 1;
    }
    table
};

#[inline]
const fn ease_in_out_cubic(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    if p < 0.5 {
        4.0 * p * p * p
    } else {
        let u = -2.0 * p + 2.0;
        1.0 - u * u * u * 0.5
    }
}

#[inline]
const fn dwell_base_ms(gkey: u64, range: (u32, u32)) -> u64 {
    let (lo, hi) = if range.0 < range.1 {
        (range.0 as u64, range.1 as u64)
    } else {
        (range.1 as u64, range.0 as u64)
    };
    lo + (mix(gkey ^ DWELL_SALT) & 3) * ((hi - lo) / 3)
}

#[inline]
const fn peek_dwell_ms(ident: u64, gkey: u64, magical: bool, range: (u32, u32)) -> u64 {
    let mut d = dwell_base_ms(gkey, range).saturating_sub(mix(ident) % CAT_DWELL_JITTER_MS);
    if magical {
        d += CAT_DWELL_MAGIC_BONUS_MS;
    }
    if d < CAT_DWELL_CAP_MS {
        d
    } else {
        CAT_DWELL_CAP_MS
    }
}

#[inline]
const fn anticipation_ms(gkey: u64) -> u64 {
    if mix(gkey ^ ANTIC_SALT) & 1 == 0 {
        CAT_ANTICIPATION_MS
    } else {
        0
    }
}

#[inline]
const fn has_accessory(magic: u64) -> bool {
    accessory_variant_v4(magic).is_some()
}

#[inline]
const fn peek_total_ms(ident: u64, gkey: u64, magical: bool, range: (u32, u32)) -> u64 {
    CAT_RISE_MS
        + peek_dwell_ms(ident, gkey, magical, range)
        + anticipation_ms(gkey)
        + CAT_DESCEND_MS
}

#[inline]
const fn caret_on_span(cell: (u16, u16), row: u16, start_col: u16, end_col: u16) -> bool {
    let (caret_row, caret_col) = cell;
    caret_row == row && caret_col >= start_col && caret_col <= end_col.saturating_add(1)
}

#[inline]
const fn trick_flash_paintable(cfg: &DecoConfig) -> bool {
    cfg.ink_enabled && !cfg.reduced_motion
}

#[inline]
fn arm_until(active_until: &mut Option<Instant>, until: Instant) {
    match active_until {
        Some(d) => {
            if until > *d {
                *d = until;
            }
        }
        None => *active_until = Some(until),
    }
}

#[inline]
fn mark_done(marks: &mut DoneMarkLru, ident: u64, ep: &Episode) {
    let key = ident ^ ep.ctx_fp;
    marks.insert(key);
}

#[inline]
fn touch_done(marks: &mut DoneMarkLru, key: u64) -> bool {
    marks.touch(key)
}

#[derive(Clone, Copy)]
struct CatIdlePose {
    bob_px: i32,
    lead_px: i32,
}

impl CatIdlePose {
    const STILL: Self = Self {
        bob_px: 0,
        lead_px: 0,
    };

    #[inline]
    const fn breathing(td_ms: u64, gkey: u64, cell_h: u16) -> Self {
        const INV_63: f32 = 1.0 / 63.0;
        const INV_255: f32 = 1.0 / 255.0;
        const INV_1000: f32 = 1.0 / 1000.0;
        let amp = (cell_h as f32 * 0.06).clamp(1.0, 2.0);
        let bob_s = 1.5 + (gkey & 0x3f) as f32 * INV_63 * 0.6;
        let sway_s = 2.4 + ((gkey >> 6) & 0x3f) as f32 * INV_63 * 0.8;
        let phase = ((gkey >> 12) & 0xff) as f32 * INV_255;
        let t = td_ms as f32 * INV_1000;
        let (bob, _) = crate::supernova::sin_cos_f32((t / bob_s + phase) * core::f32::consts::TAU);
        let (sway, _) =
            crate::supernova::sin_cos_f32((t / sway_s + phase) * core::f32::consts::TAU);
        Self {
            bob_px: (bob * amp).round() as i32,
            lead_px: (sway * amp * 0.8).round() as i32,
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one cat sprite: its art, its visible rows, the rows its window skips, and the living-cartoon pose"
)]
#[inline]
fn push_cat_free(
    free: &mut Vec<FreeSprite>,
    art: Arc<FxArt>,
    x: i32,
    top: i32,
    bottom: i32,
    skip: i32,
    pose: CatIdlePose,
    fp: &mut u64,
) {
    let h = bottom - top;
    if art.w() == 0 || h <= 0 {
        return;
    }
    // The whole art, lifted by the rows the peek window skips, seen only
    // through the window: the visible rows are the art's `skip..skip + h`.
    let (x, top) = (x + pose.lead_px, top + pose.bob_px);
    let s = FreeSprite {
        x,
        y: top - skip.max(0),
        w: art.w(),
        h: art.h(),
        clip: PxRect::at(x, top, u32::from(art.w()), h as u32),
        art,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
    };
    *fp = fold_free(*fp, &s);
    free.push(s);
}

struct CatTick<'a> {
    baker: &'a mut CatBaker,
    animal_baker: &'a mut AnimalBaker,
    sightings: &'a mut Vec<KittySighting>,
    peek_cues: &'a mut Vec<PeekCue>,
    companion_px: CompanionFootprint,
}

#[inline]
const fn cat_is_stacked_on(a: (i32, i32, i32, i32), x: i32, w: i32, top: i32, bottom: i32) -> bool {
    let (x0, x1, y0, y1) = a;
    let right = x + w;
    let ix1 = if right < x1 { right } else { x1 };
    let ix0 = if x > x0 { x } else { x0 };
    let iy1 = if bottom < y1 { bottom } else { y1 };
    let iy0 = if top > y0 { top } else { y0 };
    let iw = ix1 - ix0;
    let ih = iy1 - iy0;
    if iw <= 0 || ih <= 0 {
        return false;
    }
    let cat_area = (w as i64) * ((bottom - top) as i64);
    if cat_area <= 0 {
        return false;
    }
    (iw as i64) * (ih as i64) * 3 >= cat_area
}

#[derive(Clone, Copy)]
struct PeekView {
    start: Option<Instant>,
    peek_done: bool,
    arm: Option<KittyShownAs>,
    unlogged: bool,
}

#[derive(Clone, Copy)]
struct PeekTiming {
    t: u64,
    dwell_end: u64,
    antic: u64,
    in_rise: bool,
    in_dwell: bool,
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence timing resolution over tick-local accumulators"
)]
#[inline]
fn resolve_peek_timing(
    occ: &Occurrence,
    cfg: &DecoConfig,
    now: Instant,
    frame: u64,
    peek: PeekView,
    magical: bool,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) -> Option<PeekTiming> {
    if peek.peek_done {
        return None;
    }
    let ps = peek.start?;
    let dwell_range = match occ.spec.graphic {
        Some(g) => g.dwell_ms,
        None => (2200, 3598),
    };
    let dwell = peek_dwell_ms(occ.ident, occ.genome.gkey, magical, dwell_range);
    let antic = anticipation_ms(occ.genome.gkey);
    let total = CAT_RISE_MS + dwell + antic + CAT_DESCEND_MS;
    let dwell_end = CAT_RISE_MS + dwell;

    let (t, in_rise, in_dwell) = if cfg.reduced_motion {
        // A completed peek stays done under reduced motion (unfocused windows
        // are demoted to it). The clock latched at birth, so the pose shows
        // from the first presented frame and the flags never advance while
        // reduced. An unlatched clock (defensive) shows nothing.
        (CAT_RISE_MS, false, true)
    } else {
        if now < ps {
            arm_until(active_until, ps + Duration::from_millis(total));
            *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
            return None;
        }
        let t = now.saturating_duration_since(ps).as_millis() as u64;
        if t >= total {
            return None;
        }
        arm_until(active_until, ps + Duration::from_millis(total));
        (t, t < CAT_RISE_MS, t >= CAT_RISE_MS && t < dwell_end)
    };
    Some(PeekTiming {
        t,
        dwell_end,
        antic,
        in_rise,
        in_dwell,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, exactly like emit_ink; a carrier struct would rename the list, not shrink it"
)]
#[inline]
fn emit_cat(
    ctx: &mut CatTick,
    occ: &Occurrence,
    cfg: &DecoConfig,
    geom: EffectGeom,
    now: Instant,
    frame: u64,
    peek: PeekView,
    free: &mut Vec<FreeSprite>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    let magic = if cfg.feline_magic {
        cat_magic(occ.genome.magic)
    } else {
        None
    };
    let magical = magic.is_some() || has_accessory(occ.genome.magic);
    let Some(timing) = resolve_peek_timing(occ, cfg, now, frame, peek, magical, fp, active_until)
    else {
        return;
    };
    let special = if cfg.feline_magic {
        special_variant_v4(occ.genome.magic)
    } else {
        None
    };
    let variant = match special {
        Some(v) => v,
        None => cat_variant_v4(occ.genome.gkey),
    };
    let accessory = if special.is_none() && cfg.feline_magic {
        accessory_variant_v4(occ.genome.magic)
    } else {
        None
    };
    let age = cat_age_v4(occ.genome.gkey);
    let g = cat_geometry(occ, geom, variant);
    let (coat, iris) = cat_fills_v4(occ.genome.gkey);
    let Some((top, bottom)) = emit_peek(
        ctx,
        occ,
        cfg,
        geom,
        now,
        frame,
        timing,
        g,
        age == CatAge::Kitten,
        |ctx| {
            let key = BakeKeyV4 {
                variant,
                accessory,
                coat,
                iris,
                colors: occ.cat_colors,
                w: g.w,
                h: g.hart,
                eyes: EyesFrame::Open,
            };
            ctx.baker.get_v4(&key)
        },
        free,
        fp,
        active_until,
    ) else {
        return;
    };
    if peek.unlogged && ctx.sightings.len() < MAX_OCCURRENCES {
        // The pet's bat cue rides the same edge, with the dest rect that is
        // final only here. Cap-truncation drops the gag, never the log.
        if ctx.peek_cues.len() < MAX_PEEK_CUES {
            ctx.peek_cues.push(PeekCue {
                row: occ.row,
                col: occ.start_col,
                head_px: (g.x, g.x + i32::from(g.w), top, bottom),
            });
        }
        let traits = match accessory {
            Some(CatGlyphId::AccBow) => TRAIT_BOW,
            Some(CatGlyphId::AccCrown) => TRAIT_CROWN,
            _ => 0,
        };
        ctx.sightings.push(KittySighting {
            kitty_type: KittyType::HeadPeek,
            magic: KittyMagic::from_cat(magic),
            shown_as: KittyShownAs::Cat,
            langs: occ.langs,
            traits,
            look: KittyLook {
                variant,
                accessory,
                coat,
                iris,
                age,
            },
            ident: occ.ident,
        });
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, exactly like emit_cat"
)]
#[inline]
fn emit_animal(
    ctx: &mut CatTick,
    occ: &Occurrence,
    species: AnimalGlyphId,
    cfg: &DecoConfig,
    geom: EffectGeom,
    now: Instant,
    frame: u64,
    peek: PeekView,
    free: &mut Vec<FreeSprite>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    let Some(timing) = resolve_peek_timing(occ, cfg, now, frame, peek, false, fp, active_until)
    else {
        return;
    };
    let g = animal_geometry(occ, geom, species);
    emit_peek(
        ctx,
        occ,
        cfg,
        geom,
        now,
        frame,
        timing,
        g,
        false,
        |ctx| {
            let key = AnimalBakeKey {
                species,
                colors: occ.cat_colors,
                w: g.w,
                h: g.hart,
            };
            let host = key.host_id();
            match ctx.animal_baker.tile(&key) {
                Some(rgba) => ctx.baker.host_tile(host, key.w, key.h, rgba),
                None => ctx.baker.host_peek(host),
            }
        },
        free,
        fp,
        active_until,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, exactly like emit_ink; a carrier struct would rename the list, not shrink it"
)]
#[inline]
fn emit_peek<'a>(
    ctx: &mut CatTick<'a>,
    occ: &Occurrence,
    cfg: &DecoConfig,
    geom: EffectGeom,
    now: Instant,
    frame: u64,
    timing: PeekTiming,
    g: CatGeom,
    kitten: bool,
    bake: impl FnOnce(&mut CatTick<'a>) -> Option<CatTile>,
    free: &mut Vec<FreeSprite>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) -> Option<(i32, i32)> {
    let PeekTiming {
        t,
        dwell_end,
        antic,
        in_rise,
        in_dwell,
    } = timing;
    let ch = i32::from(geom.cell_h);
    let rest = i32::from(cat_rest_reveal(g.hart));
    let td = t.saturating_sub(CAT_RISE_MS);
    if !cfg.reduced_motion {
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
    }
    const INV_CAT_RISE_MS: f32 = 1.0 / CAT_RISE_MS as f32;
    const INV_CAT_DESCEND_MS: f32 = 1.0 / CAT_DESCEND_MS as f32;
    const INV_CAT_BOUNCE_MS: f32 = 1.0 / CAT_BOUNCE_MS as f32;
    let p = if in_rise {
        t as f32 * INV_CAT_RISE_MS
    } else {
        1.0
    };
    let mut antic_lift = 0i32;
    let reveal = if cfg.reduced_motion || in_dwell {
        rest
    } else if in_rise {
        let c = PEEK_OVERSHOOT_C[usize::from(kitten)]
            [genome::field(occ.genome.gkey, 15, 2) as usize & 3];
        (ease_out_back_c(p, c) * rest as f32).round() as i32
    } else {
        let ta = t - dwell_end;
        if ta < antic {
            antic_lift = 2;
            rest
        } else {
            let pd = (ta - antic) as f32 * INV_CAT_DESCEND_MS;
            ((1.0 - ease_in_out_cubic(pd)) * rest as f32).round() as i32
        }
    };
    if reveal <= 0 {
        return None;
    }
    let vh = reveal.min(rest);
    let lift = (reveal - rest)
        .max(0)
        .max(antic_lift)
        .min(2 * ch - (i32::from(g.hart) - i32::from(g.chin)));
    let in_bounce = kitten && !cfg.reduced_motion && in_dwell && td < CAT_BOUNCE_MS;
    let bounce = if in_bounce {
        let q = td as f32 * INV_CAT_BOUNCE_MS;
        (2.0 * crate::supernova::sin_cos_f32(std::f32::consts::PI * q).0).round() as i32
    } else {
        0
    };
    let row_top = i32::from(occ.row) * ch;
    let (mut top, mut bottom) = if occ.cat_peek_down {
        let rest_top = row_top + ch - i32::from(g.chin);
        let top = rest_top + lift - bounce;
        (top, top + vh)
    } else {
        let rest_bottom = row_top + i32::from(g.chin);
        let bottom = rest_bottom - lift + bounce;
        (bottom - vh, bottom)
    };
    let grid_h = i32::from(geom.rows) * ch;
    if top < 0 {
        bottom -= top;
        top = 0;
    } else if bottom > grid_h {
        let over = bottom - grid_h;
        top -= over;
        bottom -= over;
    }
    if ctx
        .companion_px
        .stacked_on(g.x, i32::from(g.w), top, bottom)
    {
        return None;
    }
    let Some(tile) = bake(ctx) else {
        arm_until(active_until, now + Duration::from_millis(50));
        *fp = fnv_mix_u64(*fp, frame.wrapping_mul(0x9E37_79B1));
        return None;
    };
    let skip = if occ.cat_peek_down {
        (rest - vh).max(0)
    } else {
        0
    };
    let n_free = free.len();
    let pose = if in_dwell && !in_bounce && !cfg.reduced_motion {
        CatIdlePose::breathing(td, occ.genome.gkey, geom.cell_h)
    } else {
        CatIdlePose::STILL
    };
    debug_assert_eq!(tile.art.w(), g.w, "the peek tile is the design box");
    push_cat_free(free, tile.art, g.x, top, bottom, skip, pose, fp);
    if free.len() > n_free {
        Some((top, bottom))
    } else {
        None
    }
}

const NUDGED_INK_TABLE: [([u32; 16], [u32; 16]); 4] = {
    const PAIRS: [(u32, u32); 4] = [
        (0x007C_C8FF, 0x00C8_9AFF),
        (0x00FF_D447, 0x00FF_7CE5),
        (0x00F7_A8B8, 0x00FF_D9C2),
        (0x00FF_C98A, 0x00B9_E8A3),
    ];
    let mut out = [([0u32; 16], [0u32; 16]); 4];
    let mut ci = 0;
    while ci < 4 {
        let (c0, c1) = PAIRS[ci];
        let mut n = 0;
        while n < 16 {
            out[ci].0[n] = hue_nudge(c0, n as u8);
            out[ci].1[n] = hue_nudge(c1, n as u8);
            n += 1;
        }
        ci += 1;
    }
    out
};

#[inline]
const fn nudged_ink_pair(class: Class, gkey: u64) -> Option<(u32, u32)> {
    let idx = match class {
        Class::Emphasis => 0,
        Class::Profanity => 1,
        Class::Feline => 2,
        Class::Animal => 3,
        Class::Canine => return None,
    };
    let (n0, n1) = genome::ink_pair_nudges(class, gkey);
    let (t0, t1) = &NUDGED_INK_TABLE[idx];
    Some((t0[(n0 & 0xF) as usize], t1[(n1 & 0xF) as usize]))
}

#[inline]
const fn feline_glow_env(t_ms: u64, window_ms: u64, amp: f32) -> f32 {
    if t_ms == 0 || t_ms >= window_ms {
        return 0.0;
    }
    let p = t_ms as f32 / window_ms as f32;
    let (_, cos_p) = crate::supernova::sin_cos_f32(4.0 * core::f32::consts::PI * p);
    let carrier = 0.5 - 0.5 * cos_p;
    amp * carrier * (1.0 - p)
}

const PAPER_GLOW_KEEP: f32 = 0.9;

#[inline]
const fn paper_glow_scale_lum(
    table: &crate::color_math::RelativeLuminanceTable,
    fg: u32,
    lit: u32,
    lum_bg: f32,
    amp: f32,
) -> f32 {
    let lum_fg = crate::color_math::relative_luminance_with(table, fg);
    if lum_bg <= lum_fg || amp <= 0.0 {
        return 1.0;
    }
    let num = lum_bg + 0.05;
    let floor = PAPER_GLOW_KEEP * num / (lum_fg + 0.05);
    #[inline]
    const fn reads_rgb(
        table: &crate::color_math::RelativeLuminanceTable,
        rgb: u32,
        lum_bg: f32,
        num: f32,
        floor: f32,
    ) -> bool {
        let lum = crate::color_math::relative_luminance_with(table, rgb).min(lum_bg);
        num / (lum + 0.05) >= floor
    }
    let mut hi_rgb = mix_rgb(fg, lit, amp);
    if reads_rgb(table, hi_rgb, lum_bg, num, floor) {
        return 1.0;
    }
    let mut lo_rgb = fg & 0x00FF_FFFF;
    let (mut lo, mut hi) = (0.0_f32, amp);
    let mut step = 0;
    while step < 12 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix_rgb(fg, lit, mid);
        if mid_rgb == lo_rgb || (mid_rgb != hi_rgb && reads_rgb(table, mid_rgb, lum_bg, num, floor))
        {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        step += 1;
    }
    lo / amp
}

#[cfg(test)]
const fn paper_glow_scale(fg: u32, lit: u32, bg: u32, amp: f32) -> f32 {
    let table = crate::color_math::relative_luminance_table();
    paper_glow_scale_lum(
        table,
        fg,
        lit,
        crate::color_math::relative_luminance_with(table, bg),
        amp,
    )
}

/// §4.2 specular lobe at gradient position `u` for sweep center `center`:
/// `smoothstep(1 − min(1, |u − center|/0.28))²` — 28% wide, 0 outside.
#[inline]
const fn spec_lobe(u: f32, center: f32) -> f32 {
    const INV_LOBE_HALF_WIDTH: f32 = 1.0 / 0.28;
    let d = (u - center).abs() * INV_LOBE_HALF_WIDTH;
    if d >= 1.0 {
        return 0.0;
    }
    let s = smoothstep(1.0 - d);
    s * s
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators (fp / active_until) and the resident capture buffers; a carrier struct would rename the list, not shrink it"
)]
#[inline]
fn emit_ink(
    table: &crate::color_math::RelativeLuminanceTable,
    occ: &Occurrence,
    cfg: &DecoConfig,
    now: Instant,
    frame: u64,
    fx: &InkFx,
    base_fg: &[[u8; 3]],
    base_cols: &[u16],
    ink: &mut Vec<InkCell>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    if occ.ink_cells == 0 || !cfg.ink_enabled {
        return;
    }
    let Some(ink_spec) = occ.spec.ink else {
        return;
    };
    match ink_spec.colorway {
        Colorway::SelfGlow {
            lift,
            amp,
            window_ms,
        } => {
            if occ.inert {
                return;
            }
            let window = u64::from(window_ms.max(1));
            let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
            if cfg.reduced_motion || t >= window {
                return;
            }
            let until = occ.appeared + Duration::from_millis(window);
            arm_until(active_until, until);
            let glow = feline_glow_env(t, window, amp);
            if glow <= 0.0 || occ.ink_cells == 0 {
                return;
            }
            let frame_term = frame.wrapping_mul(0x9E37_79B1);
            let cols = &base_cols[occ.ink_base..occ.ink_base + occ.ink_cells as usize];
            let fgs = &base_fg[occ.ink_base..occ.ink_base + occ.ink_cells as usize];
            let lum_bg = crate::color_math::relative_luminance_rgb(
                table,
                occ.ink_bg[0] as usize,
                occ.ink_bg[1] as usize,
                occ.ink_bg[2] as usize,
            );
            let mut memo: Option<([u8; 3], [u8; 3])> = None;
            let mut i = 0;
            while i < cols.len() {
                let col = cols[i];
                let fg_rgb = fgs[i];
                let color = match memo {
                    Some((k, v)) if k == fg_rgb => v,
                    _ => {
                        let fg = rgb3_to_u32(fg_rgb);
                        let lit = mix_rgb(fg, 0x00FF_FFFF, lift);
                        let v = u32_to_rgb3(mix_rgb(
                            fg,
                            lit,
                            glow * paper_glow_scale_lum(table, fg, lit, lum_bg, amp),
                        ));
                        memo = Some((fg_rgb, v));
                        v
                    }
                };
                let cell = InkCell {
                    row: occ.row,
                    col,
                    color,
                };
                *fp = fold_ink(*fp, cell);
                *fp ^= frame_term;
                ink.push(cell);
                i += 1;
            }
            return;
        }
        Colorway::Rainbow {
            sat,
            val,
            span_deg,
            drift_ms,
        } => {
            emit_rainbow_ink(
                table,
                occ,
                cfg,
                now,
                frame,
                fx,
                (sat, val, span_deg, drift_ms),
                base_fg,
                base_cols,
                ink,
                fp,
                active_until,
            );
            return;
        }
        Colorway::TwoTone { .. } => {}
    }
    let spec_pair = match ink_spec.colorway {
        Colorway::TwoTone { c0, c1 } if occ.custom => Some((c0, c1)),
        _ => nudged_ink_pair(occ.class, occ.genome.gkey),
    };
    let Some((c0, c1)) = (match fx.pair {
        Some(pair) => Some(pair),
        None => spec_pair,
    }) else {
        return;
    };
    let ink_loop = !ink_spec.sweep_once;
    let sweep = u64::from(cfg.ink_sweep_ms.max(1));
    let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
    let animating =
        !occ.inert && !fx.freeze && !cfg.reduced_motion && (ink_loop || t < sweep + INK_FADE_MS);
    let (center, glare) = if animating {
        const INV_INK_RAMP_IN_MS: f32 = 1.0 / INK_RAMP_IN_MS as f32;
        const INV_INK_FADE_MS: f32 = 1.0 / INK_FADE_MS as f32;
        let phase = if ink_loop { t % sweep } else { t };
        let center = -0.28 + 1.56 * (phase as f32 / sweep as f32);
        let ramp_in = (t as f32 * INV_INK_RAMP_IN_MS).min(1.0);
        let ramp_out = if ink_loop || t < sweep {
            1.0
        } else {
            1.0 - smoothstep((t - sweep) as f32 * INV_INK_FADE_MS)
        };
        (center, 0.65 * ramp_in * ramp_out)
    } else {
        (0.0, 0.0)
    };
    let base_same = c0 == c1;
    let has_glare = glare > 0.0;
    let strength = if cfg.ink_strength <= 0.0 {
        0.0
    } else {
        let mid_base = if base_same { c0 } else { mix_rgb(c0, c1, 0.5) };
        let mid_raw = if has_glare {
            mix_rgb(mid_base, 0x00FF_FFFF, glare * spec_lobe(0.5, center))
        } else {
            mid_base
        };
        legible_strength(
            table,
            cfg.ink_strength,
            rgb3_to_u32(base_fg[occ.ink_base]),
            &[mid_raw],
            crate::color_math::relative_luminance_rgb(
                table,
                occ.ink_bg[0] as usize,
                occ.ink_bg[1] as usize,
                occ.ink_bg[2] as usize,
            ),
        )
    };
    let inv_span = 1.0 / f32::from(occ.end_col.saturating_sub(occ.start_col)).max(1.0);
    let fold_frame = animating || fx.frame_live;
    let frame_term = frame.wrapping_mul(0x9E37_79B1);
    let cols = &base_cols[occ.ink_base..occ.ink_base + occ.ink_cells as usize];
    let fgs = &base_fg[occ.ink_base..occ.ink_base + occ.ink_cells as usize];
    let start_col = occ.start_col;
    let row = occ.row;
    let has_strength = strength > 0.0;
    let uniform_cell_color = !has_strength || (!has_glare && base_same);
    let dim = fx.dim;
    let has_dim = dim < 1.0;
    let mut cached_color: Option<([u8; 3], [u8; 3])> = None;
    let mut i = 0;
    while i < cols.len() {
        let col = cols[i];
        let fg_rgb = fgs[i];
        let cell_color = if uniform_cell_color
            && let Some((prev_fg, prev_out)) = cached_color
            && prev_fg == fg_rgb
        {
            prev_out
        } else {
            let mut color = if has_strength {
                let u = f32::from(col.saturating_sub(start_col)) * inv_span;
                let base = if base_same { c0 } else { mix_rgb(c0, c1, u) };
                let raw = if has_glare {
                    let lobe = glare * spec_lobe(u, center);
                    if lobe > 0.0 {
                        mix_rgb(base, 0x00FF_FFFF, lobe)
                    } else {
                        base
                    }
                } else {
                    base
                };
                mix_rgb(rgb3_to_u32(fg_rgb), raw, strength)
            } else {
                rgb3_to_u32(fg_rgb)
            };
            if has_dim {
                color = dim_rgb(color, dim);
            }
            let out = u32_to_rgb3(color);
            if uniform_cell_color {
                cached_color = Some((fg_rgb, out));
            }
            out
        };
        let cell = InkCell {
            row,
            col,
            color: cell_color,
        };
        *fp = fold_ink(*fp, cell);
        if fold_frame {
            *fp ^= frame_term;
        }
        ink.push(cell);
        i += 1;
    }
    if animating {
        let until = if ink_loop {
            now + Duration::from_millis(sweep)
        } else {
            occ.appeared + Duration::from_millis(sweep + INK_FADE_MS)
        };
        arm_until(active_until, until);
    }
}

#[inline]
const fn legible_strength(
    table: &crate::color_math::RelativeLuminanceTable,
    ink_strength: f32,
    fg_first: u32,
    samples: &[u32],
    lum_bg: f32,
) -> f32 {
    #[inline]
    const fn contrast(
        table: &crate::color_math::RelativeLuminanceTable,
        c: u32,
        lum_bg: f32,
        lum_bg_plus: f32,
    ) -> f32 {
        let la = crate::color_math::relative_luminance_with(table, c);
        if la >= lum_bg {
            (la + 0.05) / lum_bg_plus
        } else {
            lum_bg_plus / (la + 0.05)
        }
    }
    let mut strength = ink_strength.clamp(0.0, 1.0);
    if strength <= 0.0 {
        return 0.0;
    }
    let lum_bg_plus = lum_bg + 0.05;
    let step = strength * 0.125;
    'guard: while strength > 0.0 {
        let mut idx = 0;
        while idx < samples.len() {
            if contrast(
                table,
                mix_rgb(fg_first, samples[idx], strength),
                lum_bg,
                lum_bg_plus,
            ) < MIN_INK_CONTRAST
            {
                strength = (strength - step).max(0.0);
                continue 'guard;
            }
            idx += 1;
        }
        break;
    }
    strength
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure per-occurrence emission over tick-local accumulators, the emit_ink idiom"
)]
#[inline]
fn emit_rainbow_ink(
    table: &crate::color_math::RelativeLuminanceTable,
    occ: &Occurrence,
    cfg: &DecoConfig,
    now: Instant,
    frame: u64,
    fx: &InkFx,
    (sat, val, span_deg, drift_ms): (f32, f32, f32, u32),
    base_fg: &[[u8; 3]],
    base_cols: &[u16],
    ink: &mut Vec<InkCell>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    let lum_bg = crate::color_math::relative_luminance_rgb(
        table,
        occ.ink_bg[0] as usize,
        occ.ink_bg[1] as usize,
        occ.ink_bg[2] as usize,
    );
    let (sat, val) = if lum_bg > 0.5 {
        (1.0, 0.62)
    } else {
        (sat, val)
    };
    let base_hue = rainbow_base_hue(occ.genome.gkey);
    let span_used = span_deg.min(100.0 * f32::from(occ.ink_cells.saturating_sub(1)));
    let drift = u64::from(drift_ms.max(1));
    let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
    let animating = !occ.inert && !cfg.reduced_motion && t < drift;
    let start_hue = base_hue
        + if animating {
            360.0 * (t as f32 / drift as f32)
        } else {
            0.0
        };
    let (guard_hues, strength) = if cfg.ink_strength <= 0.0 {
        ([0; 4], 0.0)
    } else {
        let h0 = hsv2rgb(start_hue, sat, val);
        let (guard_hues, guard_len) = if span_used == 0.0 {
            ([h0; 4], 1)
        } else {
            (
                [
                    h0,
                    hsv2rgb(start_hue + (1.0 / 3.0) * span_used, sat, val),
                    hsv2rgb(start_hue + (2.0 / 3.0) * span_used, sat, val),
                    hsv2rgb(start_hue + span_used, sat, val),
                ],
                4,
            )
        };
        let strength = legible_strength(
            table,
            cfg.ink_strength,
            rgb3_to_u32(base_fg[occ.ink_base]),
            &guard_hues[..guard_len],
            lum_bg,
        );
        (guard_hues, strength)
    };
    let inv_denom = 1.0 / f32::from(occ.ink_cells.saturating_sub(1)).max(1.0);
    let lift_target = if lum_bg > 0.5 {
        0x000A_0A10
    } else {
        0x00FF_FFFF
    };
    let lift_t = 0.85 * fx.lift;
    let fold_frame = animating || fx.frame_live;
    let frame_term = frame.wrapping_mul(0x9E37_79B1);
    let cols = &base_cols[occ.ink_base..occ.ink_base + occ.ink_cells as usize];
    let fgs = &base_fg[occ.ink_base..occ.ink_base + occ.ink_cells as usize];
    let cols_len = cols.len();
    let row = occ.row;
    let has_strength = strength > 0.0;
    let uniform_cell_color = !has_strength || span_used == 0.0;
    let has_lift = fx.lift > 0.0;
    let dim = fx.dim;
    let has_dim = dim < 1.0;
    let mut cached_color: Option<([u8; 3], [u8; 3])> = None;
    let mut i = 0;
    while i < cols_len {
        let col = cols[i];
        let fg_rgb = fgs[i];
        let cell_color = if uniform_cell_color
            && let Some((prev_fg, prev_out)) = cached_color
            && prev_fg == fg_rgb
        {
            prev_out
        } else {
            let mut color = if has_strength {
                let target = if span_used == 0.0 || i == 0 {
                    guard_hues[0]
                } else if cols_len == 4 {
                    guard_hues[i]
                } else if i + 1 == cols_len {
                    guard_hues[3]
                } else {
                    hsv2rgb(start_hue + (i as f32 * inv_denom) * span_used, sat, val)
                };
                mix_rgb(rgb3_to_u32(fg_rgb), target, strength)
            } else {
                rgb3_to_u32(fg_rgb)
            };
            if has_lift {
                color = mix_rgb(color, lift_target, lift_t);
            }
            if has_dim {
                color = dim_rgb(color, dim);
            }
            let out = u32_to_rgb3(color);
            if uniform_cell_color {
                cached_color = Some((fg_rgb, out));
            }
            out
        };
        let cell = InkCell {
            row,
            col,
            color: cell_color,
        };
        *fp = fold_ink(*fp, cell);
        if fold_frame {
            *fp ^= frame_term;
        }
        ink.push(cell);
        i += 1;
    }
    if animating {
        let until = occ.appeared + Duration::from_millis(drift);
        arm_until(active_until, until);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "pure tick-local rainbow decoration emitter"
)]
#[inline]
fn emit_rainbow_sparkles(
    occ: &Occurrence,
    cfg: &DecoConfig,
    now: Instant,
    frame: u64,
    geom: EffectGeom,
    out: &mut Vec<WordDecoration>,
    fp: &mut u64,
    active_until: &mut Option<Instant>,
) {
    if occ.inert || cfg.reduced_motion || out.len() >= MAX_DECORATIONS {
        return;
    }
    let t = now.saturating_duration_since(occ.appeared).as_millis() as u64;
    if t >= RAINBOW_SPARKLE_MS {
        return;
    }
    const INV_RAINBOW_SPARKLE_MS: f32 = 1.0 / RAINBOW_SPARKLE_MS as f32;
    let p = t as f32 * INV_RAINBOW_SPARKLE_MS;
    let envelope = crate::supernova::sin_cos_f32(core::f32::consts::PI * p)
        .0
        .max(0.0);
    let width = occ.end_col.saturating_sub(occ.start_col).max(1);
    let base_hue = rainbow_base_hue(occ.genome.gkey);
    let lift = ((i32::from(geom.cell_h).max(8) / 3).min(127)) as i8;
    let count = 3usize.min(MAX_DECORATIONS.saturating_sub(out.len()));
    let inv_denom = if count >= 3 { 0.5 } else { 1.0 };
    let p_tau = p * 4.0 * core::f32::consts::TAU;
    let seed_phase = (occ.seed & 0xff) as f32 * 0.013;
    let width_f = f32::from(width);
    let env_intensity = cfg.intensity * envelope;
    let start_hue = base_hue + 45.0 * p;
    let mut i = 0;
    while i < count {
        let u = i as f32 * inv_denom;
        let phase = p_tau + i as f32 * 2.094 + seed_phase;
        let (sin_p, cos_p) = crate::supernova::sin_cos_f32(phase);
        let shimmer = 0.62 + 0.38 * sin_p.abs();
        let d = WordDecoration {
            row: occ.row,
            col: occ.start_col + (width_f * u).round() as u16,
            dx: (cos_p * 2.0).round() as i8,
            dy: -lift + i as i8,
            glyph: if i == 1 {
                DecoGlyph::Star5
            } else {
                DecoGlyph::Star4
            },
            blend: DecoBlend::Add,
            color: hsv2rgb(start_hue + 120.0 * i as f32, 0.72, 1.0),
            alpha: scale_u8(env_intensity * shimmer * 0.82),
        };
        *fp = fold_deco(*fp, &d);
        out.push(d);
        i += 1;
    }
    *fp ^= frame.wrapping_mul(0xD1B5_4A32_D192_ED03);
    let until = occ.appeared + Duration::from_millis(RAINBOW_SPARKLE_MS);
    arm_until(active_until, until);
}

/// Fold an ink cell's visible fields into the frame fingerprint (FNV-1a chain, the `fold_deco` sibling).
#[inline]
pub(crate) const fn fold_ink(mut h: u64, c: InkCell) -> u64 {
    h = fnv_mix_u64(h, c.row as u64);
    h = fnv_mix_u64(h, c.col as u64);
    h = fnv_mix_u64(h, c.color[0] as u64);
    h = fnv_mix_u64(h, c.color[1] as u64);
    fnv_mix_u64(h, c.color[2] as u64)
}

#[inline]
const fn dim_rgb(c: u32, f: f32) -> u32 {
    let f = f.clamp(0.0, 1.0);
    #[inline]
    const fn m(c: u32, f: f32, sh: u32) -> u32 {
        ((((c >> sh) & 0xff) as f32) * f).round() as u32
    }
    (m(c, f, 16) << 16) | (m(c, f, 8) << 8) | m(c, f, 0)
}

#[inline]
pub(crate) const fn rgb3_to_u32(c: [u8; 3]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32)
}

#[inline]
pub(crate) const fn avg_rgb(sum: [u32; 3], count: u32) -> [u8; 3] {
    [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ]
}

#[inline]
pub(crate) const fn is_ink_char(ch: char) -> bool {
    ch != ' '
        && ch != '\0'
        && !(if ch.is_ascii() {
            ch.is_ascii_whitespace()
        } else {
            ch.is_whitespace()
        })
}

#[inline]
const fn cat_color_context(cells: &[RenderCell], start: u16, end: u16) -> CatColorKey {
    if cells.is_empty() {
        return CatColorKey::DEFAULT;
    }
    let last = cells.len() - 1;
    let start_u = start as usize;
    let end_u = end as usize;
    let start = if start_u < last { start_u } else { last };
    let end = if end_u < last { end_u } else { last };
    if start > end {
        return CatColorKey::DEFAULT;
    }
    let mut principal = None;
    let mut idx = start;
    while idx <= end {
        if !cells[idx].wide {
            principal = Some(cells[idx]);
            break;
        }
        idx += 1;
    }
    let Some(principal) = principal else {
        return CatColorKey::DEFAULT;
    };

    let mut sum = [
        principal.fg[0] as u32,
        principal.fg[1] as u32,
        principal.fg[2] as u32,
    ];
    let mut count = 1u32;
    let lo = start.saturating_sub(4);
    let hi_cand = end.saturating_add(4);
    let hi = if hi_cand < last { hi_cand } else { last };
    let mut i = lo;
    while i < start {
        let cell = &cells[i];
        if !cell.wide && is_ink_char(cell.ch) {
            sum[0] += cell.fg[0] as u32;
            sum[1] += cell.fg[1] as u32;
            sum[2] += cell.fg[2] as u32;
            count += 1;
        }
        i += 1;
    }
    let mut i = end + 1;
    while i <= hi {
        let cell = &cells[i];
        if !cell.wide && is_ink_char(cell.ch) {
            sum[0] += cell.fg[0] as u32;
            sum[1] += cell.fg[1] as u32;
            sum[2] += cell.fg[2] as u32;
            count += 1;
        }
        i += 1;
    }
    let surrounding = avg_rgb(sum, count);
    CatColorKey::from_rgb(
        rgb3_to_u32(principal.bg),
        rgb3_to_u32(principal.fg),
        rgb3_to_u32(surrounding),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the cold sampler receives one resolved occurrence span plus its immutable frame surface"
)]
#[inline]
const fn cat_color_context_footprint(
    cells: &[Vec<RenderCell>],
    geom: EffectGeom,
    row: u16,
    start: u16,
    end: u16,
    genome: Genome,
    feline_magic: bool,
    default_bg: u32,
) -> CatColorKey {
    const MAX_SAMPLES: u32 = 64;
    if geom.cell_w == 0 || geom.cell_h == 0 || cells.is_empty() {
        return CatColorKey::DEFAULT;
    }
    let row_u = row as usize;
    if row_u >= cells.len() {
        return CatColorKey::DEFAULT;
    }
    let row_line = cells[row_u].as_slice();
    if row_line.is_empty() {
        return CatColorKey::DEFAULT;
    }
    let last_col = row_line.len() - 1;
    let lo_u = start as usize;
    let hi_u = end as usize;
    let lo = if lo_u < last_col { lo_u } else { last_col };
    let hi = if hi_u < last_col { hi_u } else { last_col };
    if lo > hi {
        return CatColorKey::DEFAULT;
    }
    let mut principal = None;
    let mut idx = lo;
    while idx <= hi {
        if !row_line[idx].wide {
            principal = Some(&row_line[idx]);
            break;
        }
        idx += 1;
    }
    let Some(principal) = principal else {
        return CatColorKey::DEFAULT;
    };
    let variant = if feline_magic && let Some(v) = special_variant_v4(genome.magic) {
        v
    } else {
        cat_variant_v4(genome.gkey)
    };
    let g = cat_geometry_for(start, end, genome.gkey, geom, variant);
    let cw = geom.cell_w as i64;
    let ch = geom.cell_h as i64;
    let x0 = g.x as i64;
    let x1_max = (geom.cols as i64) * cw;
    let x1_cand = x0 + (g.w as i64);
    let x1 = if x1_cand < x1_max { x1_cand } else { x1_max };
    let y0_cand = (row as i64) * ch + (g.chin as i64) - (g.hart as i64);
    let y0 = if y0_cand > 0 { y0_cand } else { 0 };
    let y1_max = (geom.rows as i64) * ch;
    let y1_cand = (row as i64) * ch + (g.chin as i64);
    let y1 = if y1_cand < y1_max { y1_cand } else { y1_max };
    if x1 <= x0 || y1 <= y0 {
        return CatColorKey::DEFAULT;
    }
    let c0 = if x0 >= 0 { (x0 / cw) as usize } else { 0 };
    let c1 = if x1 > 0 {
        ((x1 - 1) / cw) as usize
    } else {
        usize::MAX
    };
    let r0 = (y0 / ch) as usize;
    let r1 = ((y1 - 1) / ch) as usize;
    let mut bg_sum = [0u32; 3];
    let mut fg_sum = [0u32; 3];
    let mut sampled = 0u32;
    let mut visible = 0u32;
    let mut min_background_band = 3u8;
    let mut max_background_band = 0u8;
    let table = crate::color_math::relative_luminance_table();
    let fallback_bg = u32_to_rgb3(default_bg);
    let mut fallback_band_opt: Option<u8> = None;
    let mut last_bg: Option<([u8; 3], u8)> = None;
    let mut r = r0;
    'rows: while r <= r1 {
        let line = if r < cells.len() {
            cells[r].as_slice()
        } else {
            &[]
        };
        let in_end = if c1 < line.len() { c1 + 1 } else { line.len() };
        let mut col = c0;
        while col < in_end {
            let cell = &line[col];
            let (background, band) = if let Some((prev_bg, prev_band)) = last_bg
                && cell.bg[0] == prev_bg[0]
                && cell.bg[1] == prev_bg[1]
                && cell.bg[2] == prev_bg[2]
            {
                (cell.bg, prev_band)
            } else {
                let b = CatColorKey::background_band_rgb(table, cell.bg);
                last_bg = Some((cell.bg, b));
                (cell.bg, b)
            };
            if band < min_background_band {
                min_background_band = band;
            }
            if band > max_background_band {
                max_background_band = band;
            }
            bg_sum[0] += background[0] as u32;
            bg_sum[1] += background[1] as u32;
            bg_sum[2] += background[2] as u32;
            sampled += 1;
            if !cell.wide && is_ink_char(cell.ch) {
                fg_sum[0] += cell.fg[0] as u32;
                fg_sum[1] += cell.fg[1] as u32;
                fg_sum[2] += cell.fg[2] as u32;
                visible += 1;
            }
            if sampled == MAX_SAMPLES {
                break 'rows;
            }
            col += 1;
        }
        if col <= c1 {
            let fallback_band = match fallback_band_opt {
                Some(b) => b,
                None => {
                    let b = CatColorKey::background_band_with(table, default_bg);
                    fallback_band_opt = Some(b);
                    b
                }
            };
            let rem_cols = (c1 - col).saturating_add(1);
            let rem_cap = (MAX_SAMPLES - sampled) as usize;
            let n_oob = (if rem_cols < rem_cap {
                rem_cols
            } else {
                rem_cap
            }) as u32;
            if fallback_band < min_background_band {
                min_background_band = fallback_band;
            }
            if fallback_band > max_background_band {
                max_background_band = fallback_band;
            }
            bg_sum[0] += fallback_bg[0] as u32 * n_oob;
            bg_sum[1] += fallback_bg[1] as u32 * n_oob;
            bg_sum[2] += fallback_bg[2] as u32 * n_oob;
            sampled += n_oob;
            if sampled == MAX_SAMPLES {
                break 'rows;
            }
        }
        if r == usize::MAX {
            break;
        }
        r += 1;
    }
    if sampled == 0 {
        return CatColorKey::DEFAULT;
    }
    let background = avg_rgb(bg_sum, sampled);
    let surrounding = if visible == 0 {
        principal.fg
    } else {
        avg_rgb(fg_sum, visible)
    };
    CatColorKey::from_rgb_span_with(
        table,
        rgb3_to_u32(background),
        rgb3_to_u32(principal.fg),
        rgb3_to_u32(surrounding),
        min_background_band,
        max_background_band,
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PeekDir {
    Up,
    Down,
}

const CAT_MAX_BAND_OCCUPANCY: f32 = 0.75;

#[inline]
const fn clear_band_occupancy(
    cells: &[Vec<RenderCell>],
    r0: usize,
    r1: usize,
    c0: usize,
    c1: usize,
) -> Option<f32> {
    const MAX_SAMPLES: usize = 64;
    if r1 < r0 || c1 < c0 {
        return None;
    }
    let width = c1.saturating_sub(c0).saturating_add(1);
    let height = r1.saturating_sub(r0).saturating_add(1);
    let total = width.saturating_mul(height);
    if total == 0 || total > MAX_SAMPLES {
        return None;
    }
    let end_cand = c1.saturating_add(1);
    let mut occupied = 0u32;
    let mut r = r0;
    while r <= r1 {
        if r < cells.len() {
            let line = cells[r].as_slice();
            let end = if end_cand < line.len() {
                end_cand
            } else {
                line.len()
            };
            let mut c = c0;
            while c < end {
                let cell = &line[c];
                if cell.wide {
                    return None;
                }
                let ch = cell.ch;
                if is_ink_char(ch) {
                    if !matches!(ch, '\u{2500}'..='\u{259F}') {
                        return None;
                    }
                    occupied += 1;
                }
                c += 1;
            }
        }
        if r == usize::MAX {
            break;
        }
        r += 1;
    }
    Some(occupied as f32 / total as f32)
}

#[inline]
const fn band_occupancy(
    cells: &[Vec<RenderCell>],
    r0: usize,
    r1: usize,
    c0: usize,
    c1: usize,
    skip_cols: Option<std::ops::RangeInclusive<usize>>,
) -> Option<f32> {
    const MAX_SAMPLES: usize = 64;
    if r1 < r0 || c1 < c0 {
        return None;
    }
    let width = c1.saturating_sub(c0).saturating_add(1);
    let height = r1.saturating_sub(r0).saturating_add(1);
    if width.saturating_mul(height) > MAX_SAMPLES {
        return None;
    }
    let (skip_per_row, skip_span) = match &skip_cols {
        Some(s) => {
            let s0 = if *s.start() > c0 { *s.start() } else { c0 };
            let s1 = if *s.end() < c1 { *s.end() } else { c1 };
            if s1 >= s0 {
                (s1 - s0 + 1, Some((s0, s1.saturating_add(1))))
            } else {
                (0, None)
            }
        }
        None => (0, None),
    };
    let sampled = (width - skip_per_row) * height;
    if sampled == 0 {
        return None;
    }
    let end_cand = c1.saturating_add(1);
    let mut occupied = 0u32;
    let mut r = r0;
    while r <= r1 {
        if r < cells.len() {
            let line = cells[r].as_slice();
            let end = if end_cand < line.len() {
                end_cand
            } else {
                line.len()
            };
            if end > c0 {
                match skip_span {
                    Some((start_col, end_col)) => {
                        let s0 = if start_col > end { end } else { start_col };
                        let s1 = if end_col < s0 {
                            s0
                        } else if end_col > end {
                            end
                        } else {
                            end_col
                        };
                        let mut c = c0;
                        while c < s0 {
                            let cell = &line[c];
                            if cell.wide || is_ink_char(cell.ch) {
                                occupied += 1;
                            }
                            c += 1;
                        }
                        let mut c = s1;
                        while c < end {
                            let cell = &line[c];
                            if cell.wide || is_ink_char(cell.ch) {
                                occupied += 1;
                            }
                            c += 1;
                        }
                    }
                    None => {
                        let mut c = c0;
                        while c < end {
                            let cell = &line[c];
                            if cell.wide || is_ink_char(cell.ch) {
                                occupied += 1;
                            }
                            c += 1;
                        }
                    }
                }
            }
        }
        if r == usize::MAX {
            break;
        }
        r += 1;
    }
    Some(occupied as f32 / sampled as f32)
}

#[inline]
const fn cat_peek_plan(
    cells: &[Vec<RenderCell>],
    geom: EffectGeom,
    row: u16,
    start: u16,
    end: u16,
    genome: Genome,
    feline_magic: bool,
) -> Option<PeekDir> {
    if geom.cell_w == 0 || geom.cell_h == 0 || cells.is_empty() {
        return None;
    }
    let variant = if feline_magic && let Some(v) = special_variant_v4(genome.magic) {
        v
    } else {
        cat_variant_v4(genome.gkey)
    };
    let g = cat_geometry_for(start, end, genome.gkey, geom, variant);
    let cw = geom.cell_w as i64;
    let ch = geom.cell_h as i64;
    let x0 = g.x as i64;
    let x1_max = (geom.cols as i64) * cw;
    let x1_cand = x0 + (g.w as i64);
    let x1 = if x1_cand < x1_max { x1_cand } else { x1_max };
    if x1 <= x0 {
        return None;
    }
    let c0 = if x0 >= 0 { (x0 / cw) as usize } else { 0 };
    let c1 = if x1 > 0 {
        ((x1 - 1) / cw) as usize
    } else {
        usize::MAX
    };
    let body_diff = (g.hart as i64) - (g.chin as i64);
    let body = if body_diff > 0 { body_diff } else { 0 };
    let band_rows = ((body + ch - 1) / ch) as usize;
    if band_rows == 0 {
        return Some(PeekDir::Up);
    }
    let trigger = row as usize;
    let rows = geom.rows as usize;
    let up_band = if trigger >= band_rows {
        clear_band_occupancy(cells, trigger - band_rows, trigger - 1, c0, c1)
    } else {
        None
    };
    if matches!(up_band, Some(u) if u == 0.0) {
        return Some(PeekDir::Up);
    }
    let down_band = if trigger + band_rows < rows {
        clear_band_occupancy(cells, trigger + 1, trigger + band_rows, c0, c1)
    } else {
        None
    };
    if matches!(down_band, Some(d) if d == 0.0) {
        return Some(PeekDir::Down);
    }
    if up_band.is_none() && down_band.is_none() {
        return None;
    }
    let own = band_occupancy(
        cells,
        trigger,
        trigger,
        c0,
        c1,
        Some((start as usize)..=(end as usize)),
    );
    const INV_3: f32 = 1.0 / 3.0;
    #[inline]
    const fn score_of(band: f32, own: Option<f32>) -> f32 {
        match own {
            Some(o) => (band * 2.0 + o) * INV_3,
            None => band,
        }
    }
    let up = match up_band {
        Some(b) => Some(score_of(b, own)),
        None => None,
    };
    let down = match down_band {
        Some(b) => Some(score_of(b, own)),
        None => None,
    };
    match (up, down) {
        (Some(u), Some(d)) => {
            let (score, dir) = if d < u {
                (d, PeekDir::Down)
            } else {
                (u, PeekDir::Up)
            };
            if score <= CAT_MAX_BAND_OCCUPANCY {
                Some(dir)
            } else {
                None
            }
        }
        (Some(u), None) => {
            if u <= CAT_MAX_BAND_OCCUPANCY {
                Some(PeekDir::Up)
            } else {
                None
            }
        }
        (None, Some(d)) => {
            if d <= CAT_MAX_BAND_OCCUPANCY {
                Some(PeekDir::Down)
            } else {
                None
            }
        }
        (None, None) => None,
    }
}

#[inline]
pub(crate) const fn u32_to_rgb3(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

#[cfg(test)]
const fn contrast_ratio(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[inline]
const fn seed_of(start_col: u16, class: Class, form_hash: u64) -> u64 {
    let h = fnv_mix_u64(FNV64_OFFSET, start_col as u64);
    let h = fnv_mix_u64(h, class_tag(class));
    fnv_mix_u64(h, form_hash)
}

#[inline]
const fn class_tag(c: Class) -> u64 {
    match c {
        Class::Profanity => 1,
        Class::Feline => 2,
        Class::Emphasis => 4,
        Class::Canine => 5,
        Class::Animal => 6,
    }
}

#[inline]
const fn glyph_id(g: DecoGlyph) -> u64 {
    match g {
        DecoGlyph::Star4 => 0,
        DecoGlyph::Star5 => 1,
        DecoGlyph::Dot => 2,
        DecoGlyph::Plus => 3,
        DecoGlyph::Paw => 4,
        DecoGlyph::RingArc => 6,
        DecoGlyph::Shade => 7,
    }
}

#[inline]
const fn fold_deco(mut h: u64, d: &WordDecoration) -> u64 {
    h = fnv_mix_u64(h, d.row as u64);
    h = fnv_mix_u64(h, d.col as u64);
    h = fnv_mix_u64(h, (d.dx as u8) as u64);
    h = fnv_mix_u64(h, (d.dy as u8) as u64);
    h = fnv_mix_u64(h, d.color as u64);
    h = fnv_mix_u64(h, d.alpha as u64);
    h = fnv_mix_u64(h, glyph_id(d.glyph));
    fnv_mix_u64(h, matches!(d.blend, DecoBlend::Add) as u64)
}

#[inline]
fn fold_free(mut h: u64, s: &FreeSprite) -> u64 {
    h = fnv_mix_u64(h, (s.x as u32) as u64);
    h = fnv_mix_u64(h, (s.y as u32) as u64);
    h = fnv_mix_u64(h, s.w as u64);
    h = fnv_mix_u64(h, s.h as u64);
    let id = s.art.id();
    h = fnv_mix_u64(h, id as u64);
    h = fnv_mix_u64(h, (id >> 64) as u64);
    h = fnv_mix_u64(h, (s.clip.x0 as u32) as u64);
    h = fnv_mix_u64(h, (s.clip.y0 as u32) as u64);
    h = fnv_mix_u64(h, (s.clip.x1 as u32) as u64);
    h = fnv_mix_u64(h, (s.clip.y1 as u32) as u64);
    h = fnv_mix_u64(h, s.tint as u64);
    h = fnv_mix_u64(h, s.alpha as u64);
    h = fnv_mix_u64(h, s.flip_x as u64);
    fnv_mix_u64(h, s.z as u64)
}

#[inline]
const fn fold_glow(mut h: u64, q: &GlowQuad) -> u64 {
    h = fnv_mix_u64(h, q.row as u64);
    h = fnv_mix_u64(h, q.x as u64);
    h = fnv_mix_u64(h, q.y as u64);
    h = fnv_mix_u64(h, q.w as u64);
    h = fnv_mix_u64(h, q.h as u64);
    fnv_mix_u64(h, q.color as u64)
}

#[inline]
const fn pick_color(cfg: &DecoConfig, s: u64) -> u32 {
    let palette = cfg.palette.as_slice();
    if palette.is_empty() {
        hsv2rgb((s % 360) as f32, 0.85, 1.0)
    } else {
        palette[(s >> 23) as usize % palette.len()]
    }
}

#[inline]
const fn twinkle(s: u64, age_s: f32) -> f32 {
    const BYTE_TO_RAD: f32 = std::f32::consts::TAU / 255.0;
    let phase = (s & 0xff) as f32 * BYTE_TO_RAD;
    crate::effect_util::twinkle_env(age_s, phase)
}

#[inline]
const fn jitter(j: i8, s: u64) -> i8 {
    if j <= 0 {
        return 0;
    }
    let span = 2 * j as i64 + 1;
    (((s % span as u64) as i64) - j as i64) as i8
}

#[inline]
const fn scale_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "word_decorations_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "word_decorations_pet_handoff_emission_tests.rs"]
mod pet_handoff_emission_tests;

#[cfg(test)]
#[path = "word_decorations_pet_stays_inside_the_grid_tests.rs"]
mod pet_stays_inside_the_grid_tests;

#[cfg(test)]
#[path = "word_decorations_dog_cameo_emission_tests.rs"]
mod dog_cameo_emission_tests;

#[cfg(test)]
#[path = "word_decorations_trick_flash_hook_tests.rs"]
mod trick_flash_hook_tests;

#[cfg(test)]
#[path = "nova_host_conformance_tests.rs"]
mod nova_host_conformance;
